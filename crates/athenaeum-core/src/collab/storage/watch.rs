//! The fast path of change detection (spec §9.2): notify events aggregated
//! for 10 s; a removal concluded only after a 60 s settle, so a move or a
//! rename is one event. The stat sweep (`sweep.rs`) is the authority and the
//! serve check (`collab::serve`) the correctness gate; this only makes the
//! common case fast.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const AGGREGATE: Duration = Duration::from_secs(10);
pub const SETTLE: Duration = Duration::from_secs(60);
pub const CANARY_REL: &str = ".athenaeum/canary";
pub const CANARY_EVERY: Duration = Duration::from_secs(300);
pub const CANARY_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsSignal {
    /// A path under the root changed. May be a directory, not only a file —
    /// notify reports directory creates/renames/removes too.
    Touched(PathBuf),
    /// The root itself was touched (renamed, recreated, permissions changed).
    Root,
    Canary,
    /// The watcher reports it may have missed events (inotify's
    /// `IN_Q_OVERFLOW`, FSEvents' `kFSEventStreamEventFlagMustScanSubDirs`,
    /// surfaced by `notify` as `Event::need_rescan()`). Nothing here can be
    /// trusted incremental; the caller must fall back to a full stat sweep
    /// (fix round 1, Important ruling).
    Rescan,
    WatchError(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Drained {
    /// Paths concluded changed this drain. May be directories, not only
    /// files.
    pub changed: Vec<PathBuf>,
    /// Paths concluded removed (settled) this drain. May be directories.
    pub removed: Vec<PathBuf>,
    pub root_touched: bool,
}

#[derive(Debug)]
pub struct Aggregator {
    touched: BTreeSet<PathBuf>,
    window_started: Option<Instant>,
    root_touched: bool,
    pending_removals: BTreeMap<PathBuf, Instant>,
    aggregate: Duration,
    settle: Duration,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::with_timings(AGGREGATE, SETTLE)
    }
}

impl Aggregator {
    pub fn with_timings(aggregate: Duration, settle: Duration) -> Self {
        Self {
            touched: BTreeSet::new(),
            window_started: None,
            root_touched: false,
            pending_removals: BTreeMap::new(),
            aggregate,
            settle,
        }
    }

    pub fn observe(&mut self, sig: &FsSignal, now: Instant) {
        match sig {
            FsSignal::Touched(p) => {
                self.touched.insert(p.clone());
                self.window_started.get_or_insert(now);
            }
            // A rescan is folded into `root_touched` the same as the root
            // itself changing: neither can be resolved incrementally, and
            // Task 9's storage task reads `root_touched` as its cue to run
            // an out-of-band stat sweep rather than trust the aggregator.
            FsSignal::Root | FsSignal::Rescan => {
                self.root_touched = true;
                self.window_started.get_or_insert(now);
            }
            FsSignal::Canary | FsSignal::WatchError(_) => {}
        }
    }

    pub fn drain(&mut self, now: Instant, exists: impl Fn(&Path) -> bool) -> Drained {
        let mut out = Drained::default();
        if self
            .window_started
            .is_some_and(|t| now >= t + self.aggregate)
        {
            self.window_started = None;
            out.root_touched = std::mem::take(&mut self.root_touched);
            for p in std::mem::take(&mut self.touched) {
                if exists(&p) {
                    self.pending_removals.remove(&p);
                    out.changed.push(p);
                } else {
                    self.pending_removals.entry(p).or_insert(now);
                }
            }
        }
        let settled: Vec<PathBuf> = self
            .pending_removals
            .iter()
            .filter(|(_, since)| now >= **since + self.settle)
            .map(|(p, _)| p.clone())
            .collect();
        for p in settled {
            self.pending_removals.remove(&p);
            if exists(&p) {
                out.changed.push(p);
            } else {
                out.removed.push(p);
            }
        }
        out.changed.sort();
        out.changed.dedup();
        out.removed.sort();
        out
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let w = self.window_started.map(|t| t + self.aggregate);
        let r = self
            .pending_removals
            .values()
            .map(|t| *t + self.settle)
            .min();
        match (w, r) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn pending_removal(&self, path: &Path) -> bool {
        self.pending_removals.contains_key(path)
    }
}

/// Everything under `.athenaeum` is ours, and most of it is ignored: the
/// blob store and the write probe are internal bookkeeping, not content a
/// device holds. Two exceptions produce a signal like any other path: the
/// canary (its whole job is to be observed) and the storage marker
/// (`store-id`) — a marker change (a device replace, a take-over) must not
/// be swallowed silently (fix round 1, folded ruling).
pub fn is_ignored(root: &Path, path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "athtmp") {
        return true;
    }
    match path.strip_prefix(root) {
        Ok(rel) => {
            rel.starts_with(".athenaeum")
                && rel != Path::new(CANARY_REL)
                && rel != Path::new(super::marker::MARKER_REL)
        }
        Err(_) => false,
    }
}

#[derive(Debug, Default)]
pub struct Canary {
    written_at: Option<Instant>,
    seen: bool,
}

impl Canary {
    pub fn due(&self, now: Instant) -> bool {
        self.written_at.is_none_or(|t| now >= t + CANARY_EVERY)
    }
    pub fn wrote(&mut self, now: Instant) {
        self.written_at = Some(now);
        self.seen = false;
    }
    pub fn observed(&mut self) {
        self.seen = true;
    }
    /// True once a write went unobserved for [`CANARY_DEADLINE`].
    pub fn dead(&mut self, now: Instant) -> bool {
        self.written_at
            .is_some_and(|t| !self.seen && now >= t + CANARY_DEADLINE)
    }
}

pub fn write_canary(root: &Path) -> std::io::Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default();
    std::fs::write(root.join(CANARY_REL), stamp)
}

/// True for `EventKind::Access` events other than a completed write. Opens
/// and reads are noise for change detection (every provider serve would
/// otherwise generate a signal); a `Close(Write)` is the one access event
/// that means a save actually finished, so it is treated like any other
/// touch (fix round 1, folded ruling).
fn is_uninteresting_access(kind: &notify::EventKind) -> bool {
    matches!(
        kind,
        notify::EventKind::Access(a)
            if !matches!(a, notify::event::AccessKind::Close(notify::event::AccessMode::Write))
    )
}

/// Turn one native event into zero or more [`FsSignal`]s. `root` and
/// `canary` must already be in the SAME (canonicalized/normalized) spelling
/// [`spawn_watcher`] passed to the underlying watcher — see its doc comment.
/// A free function (not inlined in the sink) so a synthetic `notify::Event`
/// can exercise the rescan and access-filtering rules without a real
/// watcher (fix round 1).
fn signals_for_event(root: &Path, canary: &Path, event: &notify::Event) -> Vec<FsSignal> {
    if event.need_rescan() {
        // inotify's `IN_Q_OVERFLOW` and FSEvents' "must scan subdirs" both
        // surface here as `EventKind::Other` + `Flag::Rescan` with EMPTY
        // paths — silently doing nothing with them would be exactly the
        // never-swallow violation the rest of this module exists to avoid
        // (fix round 1, Important ruling).
        tracing::warn!(
            kind = ?event.kind,
            "collaboration folder watcher may have missed events; forcing a rescan"
        );
        return vec![FsSignal::Rescan];
    }
    if is_uninteresting_access(&event.kind) {
        return Vec::new();
    }
    event
        .paths
        .iter()
        .filter_map(|path| {
            if path == root {
                Some(FsSignal::Root)
            } else if path == canary {
                Some(FsSignal::Canary)
            } else if is_ignored(root, path) {
                None
            } else {
                Some(FsSignal::Touched(path.clone()))
            }
        })
        .collect()
}

/// A recursive watcher on the root. Events cross an UNBOUNDED channel: a
/// bounded one can block the platform's fs-event thread (P23). `None` when
/// no watcher can be established — the caller then runs the degraded sweep.
///
/// The root is canonicalized (and, on Windows, de-verbatim'd via
/// `normalize_path`) before it is ever compared against an event path or
/// passed to the underlying watcher. FSEvents (macOS) — and, in general, any
/// backend — reports paths in their canonical spelling regardless of what
/// was passed to `watch()`; a symlinked root, or the ordinary macOS `/var`
/// vs `/private/var` split, would otherwise make `path == root` and
/// `path == canary` never match: the canary is wrongly declared dead and
/// `.athenaeum/blobs` writes go through unfiltered (fix round 1, Important
/// ruling).
pub fn spawn_watcher(
    root: &Path,
    tx: tokio::sync::mpsc::UnboundedSender<FsSignal>,
) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let root_owned = match root.canonicalize() {
        Ok(c) => crate::api::scan_roots::normalize_path(&c),
        Err(e) => {
            tracing::warn!(path = %root.display(), error = %e, "collaboration folder watcher: root could not be canonicalized; periodic check only");
            return None;
        }
    };
    let canary = root_owned.join(CANARY_REL);
    let watch_root = root_owned.clone();
    let sink = move |res: notify::Result<notify::Event>| match res {
        Ok(event) => {
            for sig in signals_for_event(&root_owned, &canary, &event) {
                if tx.send(sig).is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "collaboration folder watcher error");
            let _ = tx.send(FsSignal::WatchError(e.to_string()));
        }
    };
    let mut watcher = match notify::recommended_watcher(sink) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(path = %watch_root.display(), error = %e, "collaboration folder watcher unavailable; periodic check only");
            return None;
        }
    };
    if let Err(e) = watcher.watch(&watch_root, notify::RecursiveMode::Recursive) {
        tracing::warn!(path = %watch_root.display(), error = %e, "collaboration folder watch failed; periodic check only");
        return None;
    }
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn changes_wait_for_the_aggregation_window() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        a.observe(&FsSignal::Touched(p("/c/m31/a/x.fits")), t0);
        assert_eq!(
            a.drain(t0 + Duration::from_secs(5), |_| true),
            Drained::default()
        );
        let d = a.drain(t0 + AGGREGATE, |_| true);
        assert_eq!(d.changed, vec![p("/c/m31/a/x.fits")]);
        assert!(d.removed.is_empty());
    }

    #[test]
    fn a_removal_is_concluded_only_after_the_settle_window() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        a.observe(&FsSignal::Touched(p("/c/x.fits")), t0);
        let gone = |_: &Path| false;
        assert!(a.drain(t0 + AGGREGATE, gone).removed.is_empty());
        assert!(a.pending_removal(Path::new("/c/x.fits")));
        assert!(a
            .drain(t0 + AGGREGATE + Duration::from_secs(30), gone)
            .removed
            .is_empty());
        let d = a.drain(t0 + AGGREGATE + SETTLE, gone);
        assert_eq!(d.removed, vec![p("/c/x.fits")]);
    }

    #[test]
    fn a_move_is_one_event_not_a_deletion() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        let present: HashSet<PathBuf> = [p("/c/new/x.fits")].into();
        let exists = |q: &Path| present.contains(q);
        a.observe(&FsSignal::Touched(p("/c/old/x.fits")), t0);
        a.observe(&FsSignal::Touched(p("/c/new/x.fits")), t0);
        let d = a.drain(t0 + AGGREGATE, exists);
        assert_eq!(d.changed, vec![p("/c/new/x.fits")]);
        // the old path settles as removed; the storage task re-adopts the new
        // path by hash BEFORE it rules on the removal (Task 9)
        let d = a.drain(t0 + AGGREGATE + SETTLE, exists);
        assert_eq!(d.removed, vec![p("/c/old/x.fits")]);
    }

    #[test]
    fn a_file_back_before_the_settle_cancels_the_removal() {
        let t0 = Instant::now();
        let mut a = Aggregator::default();
        a.observe(&FsSignal::Touched(p("/c/x.fits")), t0);
        a.drain(t0 + AGGREGATE, |_| false);
        a.observe(
            &FsSignal::Touched(p("/c/x.fits")),
            t0 + Duration::from_secs(20),
        );
        let d = a.drain(t0 + Duration::from_secs(30), |_| true);
        assert_eq!(d.changed, vec![p("/c/x.fits")]);
        assert!(a.drain(t0 + SETTLE * 2, |_| true).removed.is_empty());
    }

    #[test]
    fn our_own_temp_files_and_the_store_are_ignored_but_the_canary_is_not() {
        let root = Path::new("/c");
        assert!(is_ignored(root, Path::new("/c/m31/a/x.fits.athtmp")));
        assert!(is_ignored(
            root,
            Path::new("/c/.athenaeum/blobs/data/ab.data")
        ));
        assert!(
            is_ignored(root, Path::new("/c/.athenaeum/.write-probe")),
            "the write probe stays ignored"
        );
        assert!(!is_ignored(root, Path::new("/c/.athenaeum/canary")));
        assert!(
            !is_ignored(root, Path::new("/c/.athenaeum/store-id")),
            "a marker change (device replace, take-over) must produce a signal"
        );
        assert!(!is_ignored(root, Path::new("/c/m31/a/x.fits")));
    }

    #[test]
    fn a_rescan_flag_forces_a_rescan_signal_despite_empty_paths() {
        // Fix round 1 (Important ruling): inotify's `IN_Q_OVERFLOW` and
        // FSEvents' "must scan subdirs" both surface as `EventKind::Other` +
        // `Flag::Rescan` with NO paths at all — nothing to iterate, so the
        // old per-path loop silently produced no signal whatsoever.
        let mut attrs = notify::event::EventAttributes::new();
        attrs.set_flag(notify::event::Flag::Rescan);
        let event = notify::Event {
            kind: notify::EventKind::Other,
            paths: vec![],
            attrs,
        };
        let root = Path::new("/c");
        let canary = root.join(CANARY_REL);
        assert_eq!(
            signals_for_event(root, &canary, &event),
            vec![FsSignal::Rescan]
        );
    }

    #[test]
    fn a_plain_access_event_is_skipped_but_a_completed_write_close_is_not() {
        let root = Path::new("/c");
        let canary = root.join(CANARY_REL);
        let touched = root.join("m31/a/x.fits");

        let opened = notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Open(
                notify::event::AccessMode::Any,
            )),
            paths: vec![touched.clone()],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(signals_for_event(root, &canary, &opened).is_empty());

        let read = notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Read),
            paths: vec![touched.clone()],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(signals_for_event(root, &canary, &read).is_empty());

        let closed_after_write = notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Close(
                notify::event::AccessMode::Write,
            )),
            paths: vec![touched.clone()],
            attrs: notify::event::EventAttributes::new(),
        };
        assert_eq!(
            signals_for_event(root, &canary, &closed_after_write),
            vec![FsSignal::Touched(touched)]
        );
    }

    #[test]
    fn the_canary_declares_a_silent_watcher_dead() {
        let t0 = Instant::now();
        let mut c = Canary::default();
        assert!(c.due(t0));
        c.wrote(t0);
        assert!(!c.dead(t0 + Duration::from_secs(10)));
        assert!(c.dead(t0 + CANARY_DEADLINE));
        c.wrote(t0 + CANARY_EVERY);
        c.observed();
        assert!(!c.dead(t0 + CANARY_EVERY + CANARY_DEADLINE));
    }

    #[tokio::test]
    async fn a_real_watcher_reports_a_new_file() {
        let (_tmp, root) = crate::test_support::canonical_tempdir();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Some(_w) = spawn_watcher(&root, tx) else {
            eprintln!("no filesystem watcher on this platform; skipping");
            return;
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(root.join("x.fits"), b"data").unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let sig = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("an event within 10 s")
                .unwrap();
            if sig == FsSignal::Touched(root.join("x.fits")) {
                break;
            }
        }
    }

    #[tokio::test]
    async fn a_non_canonical_root_still_detects_the_canary() {
        // Fix round 1 (Important ruling): FSEvents (macOS) reports paths in
        // their CANONICAL spelling regardless of what was passed to
        // `watch()`. A macOS temp dir is exactly the `/var` vs
        // `/private/var` split this guards against — `tmp.path()` here is
        // deliberately the raw, non-canonical spelling, not
        // `canonical_tempdir()`: before the fix, the canary write's
        // canonical event path never equalled the non-canonical `canary`
        // this function built, and the live watcher was wrongly declared
        // dead.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".athenaeum")).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Some(_w) = spawn_watcher(root, tx) else {
            eprintln!("no filesystem watcher on this platform; skipping");
            return;
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        write_canary(root).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let sig = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("an event within 10 s")
                .unwrap();
            if sig == FsSignal::Canary {
                break;
            }
        }
    }

    #[tokio::test]
    async fn a_real_watcher_filters_the_blob_store_and_reports_the_canary() {
        let (_tmp, root) = crate::test_support::canonical_tempdir();
        std::fs::create_dir_all(root.join(".athenaeum/blobs")).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Some(_w) = spawn_watcher(&root, tx) else {
            eprintln!("no filesystem watcher on this platform; skipping");
            return;
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        write_canary(&root).unwrap();
        std::fs::write(root.join(".athenaeum/blobs/x.data"), b"blob").unwrap();
        std::fs::write(root.join("marker.fits"), b"marker").unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut saw_canary = false;
        loop {
            let sig = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("an event within 10 s")
                .unwrap();
            match &sig {
                FsSignal::Canary => saw_canary = true,
                FsSignal::Touched(p) => {
                    assert!(
                        !p.starts_with(root.join(".athenaeum")),
                        "the blob store must be filtered: {p:?}"
                    );
                    if *p == root.join("marker.fits") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_canary, "the canary write must be observed");
    }
}
