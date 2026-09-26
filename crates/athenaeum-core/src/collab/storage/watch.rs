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
    Touched(PathBuf),
    Root,
    Canary,
    WatchError(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Drained {
    pub changed: Vec<PathBuf>,
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
            FsSignal::Root => {
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

pub fn is_ignored(root: &Path, path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "athtmp") {
        return true;
    }
    match path.strip_prefix(root) {
        Ok(rel) => rel.starts_with(".athenaeum") && rel != Path::new(CANARY_REL),
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

/// A recursive watcher on the root. Events cross an UNBOUNDED channel: a
/// bounded one can block the platform's fs-event thread (P23). `None` when
/// no watcher can be established — the caller then runs the degraded sweep.
pub fn spawn_watcher(
    root: &Path,
    tx: tokio::sync::mpsc::UnboundedSender<FsSignal>,
) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let root_owned = root.to_path_buf();
    let canary = root.join(CANARY_REL);
    let sink = move |res: notify::Result<notify::Event>| match res {
        Ok(event) => {
            for path in event.paths {
                let sig = if path == root_owned {
                    FsSignal::Root
                } else if path == canary {
                    FsSignal::Canary
                } else if is_ignored(&root_owned, &path) {
                    continue;
                } else {
                    FsSignal::Touched(path)
                };
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
            tracing::warn!(path = %root.display(), error = %e, "collaboration folder watcher unavailable; periodic check only");
            return None;
        }
    };
    if let Err(e) = watcher.watch(root, notify::RecursiveMode::Recursive) {
        tracing::warn!(path = %root.display(), error = %e, "collaboration folder watch failed; periodic check only");
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
        assert!(!is_ignored(root, Path::new("/c/.athenaeum/canary")));
        assert!(!is_ignored(root, Path::new("/c/m31/a/x.fits")));
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
}
