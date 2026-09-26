//! The storage engine (spec §9.1–§9.4, L4–L6, plan P10/P11/P13/P24, Task 9):
//! it drives every replica and own frame's LOCAL state from what the disk
//! says, through the pure state machine (`collab::storage::states`) and the
//! L4 deletion rules (`collab::storage::deletions`).
//!
//! - **Availability first.** Every tick and every sweep re-reads the storage
//!   marker ([`StoreGuard::check_now`]). A store that is not serving changes
//!   no frame — unmounted is not deleted.
//! - **Fast path.** Watcher signals ([`FsSignal`]) are aggregated for 10 s;
//!   a removal is concluded only after the 60 s settle, so a move is one
//!   event. Changed paths are processed FIRST (a moved file is re-adopted by
//!   hash before its old path is ruled a deletion), then removals.
//! - **Authority.** The stat sweep ([`StorageEngine::sweep`]) rechecks every
//!   landed path (own frames outside the root included, A1): hourly ±25 %
//!   while the watcher is healthy, every 5 minutes while it is not or the
//!   store is on a network volume.
//! - **Deletions** (L4): each settled replica deletion is ruled over a
//!   rolling 5-minute window — re-fetched, or (> 10 in the window, or the
//!   same frame deleted again within 24 h) ONE non-blocking, reversible
//!   choice. A frame no other device holds raises "lost everywhere" at once.
//! - **Changed files** (L5): a replica whose bytes changed stops serving at
//!   once (unseeded before the state write) and is quarantined — never
//!   overwritten or deleted without the user.
//!
//! Every state write goes through `db::collab_frames::set_local_state` (claim
//! + outbox in the same transaction, C24), under the project's disk lock,
//! one transaction per frame. The engine owns no task: the caller (Task 15's
//! session loop) `select!`s on [`StorageEngine::recv_signal`] and
//! [`StorageEngine::next_deadline`] and calls [`StorageEngine::tick`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{db, ApiError};
use crate::collab::live::holders::Redundancy;
use crate::collab::storage::deletions::{self, DeletionRuling};
use crate::collab::storage::marker::{StoreGuard, StoreState};
use crate::collab::storage::states::{transition, StateEvent};
use crate::collab::storage::sweep::{self, Stamp, StatVerdict};
use crate::collab::storage::watch::{self, Aggregator, Canary, FsSignal};
use crate::db::collab_frames::{self as frames_db, FrameOrigin, LocalFrameRow, LocalState};
use crate::db::collab_live::{self as live_db, QuarantineRow};
use crate::geometry::ransac::SplitMix64;
use crate::services::ServiceContext;
use crate::sharing::iroh::node::SharedIrohNode;

/// How often a tick re-reads the storage marker when nothing touched the
/// root (plan Task 9: "every 5 s").
pub const STORE_CHECK_EVERY: Duration = Duration::from_secs(5);
/// Consecutive watcher errors, with no good signal between them, before the
/// watcher counts as dead (a single transient error never flips it — Task 8
/// review carry).
pub const WATCH_ERRORS_DEGRADE: u32 = 3;
/// The machine-readable prefix of the `Conflict` [`resolve_changed_file`]
/// answers when the system trash refuses the changed file.
pub const TRASH_UNAVAILABLE: &str = "trash_unavailable";

/// Slack past one collab GC interval before a parked frame (a moved file
/// whose re-seed hit a dead store entry) is retried.
pub const PARKED_RETRY_MARGIN: Duration = Duration::from_secs(60);
/// When a parked frame is retried: one collab GC interval (the dead entry
/// is collected once its seed tags are gone, P31) plus
/// [`PARKED_RETRY_MARGIN`].
pub const PARKED_RETRY_AFTER: Duration = Duration::from_secs(
    crate::sharing::iroh::GC_INTERVAL.as_secs() + PARKED_RETRY_MARGIN.as_secs(),
);

/// Who else holds a frame's CURRENT version, from the holder map (Task 6).
pub trait HolderView: Send + Sync {
    fn other_holders(&self, project_id: &str, frame_uuid: &str) -> Redundancy;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageEvent {
    StateChanged {
        project_id: String,
        frame_uuid: String,
        from: LocalState,
        to: LocalState,
    },
    /// ONE non-blocking, reversible choice (L4): "Re-fetch" or "Stop keeping".
    DeletionChoice {
        count: usize,
        project_ids: Vec<String>,
    },
    /// No other holder of the current version anywhere (L4) — "restore it
    /// from the Trash".
    FrameLost {
        project_id: String,
        frame_uuid: String,
        file_name: String,
    },
    /// A replica's bytes changed; it stopped serving (L5).
    Quarantined {
        project_id: String,
        frame_uuid: String,
        file_name: String,
    },
    Availability(StoreState),
    /// `true`: changes are seen by the periodic check only.
    WatcherDegraded(bool),
}

#[derive(Debug, Clone, Copy)]
pub struct StorageTimings {
    pub aggregate: Duration,
    pub settle: Duration,
    pub sweep_healthy: Duration,
    pub sweep_degraded: Duration,
}

impl Default for StorageTimings {
    fn default() -> Self {
        Self {
            aggregate: watch::AGGREGATE,
            settle: watch::SETTLE,
            sweep_healthy: sweep::SWEEP_HEALTHY,
            sweep_degraded: sweep::SWEEP_DEGRADED,
        }
    }
}

pub struct StorageEngine {
    ctx: Arc<ServiceContext>,
    node: Arc<SharedIrohNode>,
    guard: Arc<StoreGuard>,
    /// The stored root spelling (what every `landed_path` uses).
    root: PathBuf,
    /// The canonical spelling watcher events arrive in, when it differs.
    canon_root: Option<PathBuf>,
    agg: Aggregator,
    canary: Canary,
    /// The last canary write went unobserved past its deadline; cleared by
    /// the next observed canary.
    canary_dead: bool,
    watcher: Option<notify::RecommendedWatcher>,
    fs_rx: tokio::sync::mpsc::UnboundedReceiver<FsSignal>,
    _sink: watch::SinkRegistration,
    degraded: bool,
    network: bool,
    watch_errors: u32,
    state: StoreState,
    recheck_store: bool,
    next_check: Instant,
    next_sweep: Instant,
    /// Parked frames are retried at this deadline (fix round 1).
    next_parked_retry: Option<Instant>,
    /// Until this instant the watcher may have missed moves (a rescan, a
    /// touched root, a watcher error): removals are ruled only after a walk
    /// for the moved files (fix round 1).
    watch_gap_until: Option<Instant>,
    rng: SplitMix64,
    timings: StorageTimings,
    /// `(instant, wall-clock ms)` at start: the wall clock the L4 history is
    /// kept in follows the (injectable) `Instant` the ticks run on.
    clock_base: (Instant, i64),
}

impl StorageEngine {
    pub fn start(
        ctx: Arc<ServiceContext>,
        node: Arc<SharedIrohNode>,
        guard: Arc<StoreGuard>,
    ) -> Self {
        Self::start_with(ctx, node, guard, StorageTimings::default())
    }

    pub fn start_with(
        ctx: Arc<ServiceContext>,
        node: Arc<SharedIrohNode>,
        guard: Arc<StoreGuard>,
        timings: StorageTimings,
    ) -> Self {
        let root = guard.root().to_path_buf();
        let state = guard.check_now();
        let (tx, fs_rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher = watch::spawn_watcher(&root, tx.clone());
        let canon_root = root
            .canonicalize()
            .ok()
            .map(|c| crate::api::scan_roots::normalize_path(&c))
            .filter(|c| *c != root);
        let mut roots = vec![root.clone()];
        roots.extend(canon_root.clone());
        let sink = watch::register_engine_sink(roots, tx);
        let network = sweep::is_network_volume(&root);
        let degraded = watcher.is_none() || network;
        let now = Instant::now();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5eed);
        tracing::info!(
            path = %root.display(),
            state = ?state,
            outcome = if degraded { "periodic_check_only" } else { "watching" },
            "collaboration storage engine started"
        );
        Self {
            ctx,
            node,
            guard,
            root,
            canon_root,
            agg: Aggregator::with_timings(timings.aggregate, timings.settle),
            canary: Canary::default(),
            canary_dead: false,
            watcher,
            fs_rx,
            _sink: sink,
            degraded,
            network,
            watch_errors: 0,
            state,
            recheck_store: false,
            next_check: now + STORE_CHECK_EVERY,
            // A sweep at start confirms the files before anything new is
            // reported (§9.1).
            next_sweep: now,
            next_parked_retry: None,
            watch_gap_until: None,
            rng: SplitMix64(seed),
            timings,
            clock_base: (now, chrono::Utc::now().timestamp_millis()),
        }
    }

    pub fn next_deadline(&self) -> Instant {
        let mut d = self.next_check.min(self.next_sweep);
        if let Some(a) = self.agg.next_deadline() {
            d = d.min(a);
        }
        if let Some(r) = self.next_parked_retry {
            d = d.min(r);
        }
        d
    }

    /// The next watcher signal (`select!`-friendly). `None` once every
    /// sender is gone.
    pub async fn recv_signal(&mut self) -> Option<FsSignal> {
        self.fs_rx.recv().await
    }

    pub fn on_signal(&mut self, sig: FsSignal, now: Instant) {
        match sig {
            FsSignal::Canary => {
                self.canary.observed();
                self.canary_dead = false;
                self.watch_errors = 0;
            }
            FsSignal::Root => {
                self.recheck_store = true;
                self.watch_errors = 0;
                self.mark_watch_gap(now);
                self.agg.observe(&FsSignal::Root, now);
            }
            FsSignal::Rescan => {
                self.recheck_store = true;
                self.watch_errors = 0;
                self.mark_watch_gap(now);
                self.agg.observe(&FsSignal::Rescan, now);
            }
            FsSignal::Touched(p) => {
                self.watch_errors = 0;
                let p = self.normalize(&p);
                if p == self.root.join(crate::collab::storage::marker::MARKER_REL) {
                    // A marker change (device replace, take-over, swapped
                    // disk) is a store question, not a frame's.
                    self.recheck_store = true;
                    return;
                }
                self.agg.observe(&FsSignal::Touched(p), now);
            }
            FsSignal::WatchError(e) => {
                self.watch_errors = self.watch_errors.saturating_add(1);
                self.mark_watch_gap(now);
                tracing::debug!(
                    error = %e,
                    count = self.watch_errors,
                    "collaboration folder watcher error counted"
                );
            }
        }
    }

    pub async fn tick(&mut self, now: Instant, holders: &dyn HolderView) -> Vec<StorageEvent> {
        let mut ev = Vec::new();
        self.pump(now);
        // 1. availability — not serving: drain nothing, change no frame.
        if self.recheck_store || now >= self.next_check {
            self.refresh_store(now, &mut ev).await;
        }
        if !self.state.serving() {
            return ev;
        }
        // 2. the canary
        self.canary_step(now).await;
        self.update_degraded(now, &mut ev);
        // 3. aggregated changes: changed FIRST (re-adoption of a moved file
        //    before its old path is ruled a deletion), then removals.
        let drained = self.agg.drain(now, |p| p.exists());
        if drained.root_touched {
            self.mark_watch_gap(now);
        }
        for path in self.expand_changed(&drained.changed) {
            self.pump(now);
            self.on_changed(&path, now, &mut ev).await;
        }
        if !drained.removed.is_empty() {
            let mut rows: Vec<LocalFrameRow> = Vec::new();
            for removed in &drained.removed {
                match self.rows_under(removed) {
                    Ok(found) => rows.extend(found),
                    Err(e) => {
                        tracing::error!(path = %removed.display(), error = %e, "rows under a removed path could not be read")
                    }
                }
            }
            rows.sort_by(|a, b| {
                (&a.project_id, &a.frame_uuid).cmp(&(&b.project_id, &b.frame_uuid))
            });
            rows.dedup_by(|a, b| a.project_id == b.project_id && a.frame_uuid == b.frame_uuid);
            // Moves the watcher did not see (degraded, or a rescan / touched
            // root / watcher error) are re-adopted BEFORE the removal is
            // ruled a deletion (fix round 1).
            if self.degraded || self.watch_gap_until.is_some_and(|t| now <= t) {
                self.adopt_unseen_moves(&rows, now, &mut ev).await;
            }
            self.file_gone(rows, now, holders, &mut ev).await;
        }
        // 3b. parked frames, one collab GC interval after they were parked
        if self.next_parked_retry.is_some_and(|t| now >= t) {
            self.retry_parked(now, &mut ev).await;
        }
        // 4. the sweep (a touched root or a rescan forces one)
        if drained.root_touched || now >= self.next_sweep {
            let swept = self.sweep_at(now, holders).await;
            for e in swept {
                if !ev.contains(&e) {
                    ev.push(e);
                }
            }
        }
        ev
    }

    pub async fn sweep(&mut self, holders: &dyn HolderView) -> Vec<StorageEvent> {
        self.sweep_at(Instant::now(), holders).await
    }

    /// The immediate local check a refused serve triggers (§9.3): the same
    /// bytes re-record the stamp, different bytes quarantine, a missing file
    /// enters the settle.
    pub async fn local_check(&mut self, project_id: &str, frame_uuid: &str) -> Vec<StorageEvent> {
        let now = Instant::now();
        let mut ev = Vec::new();
        self.refresh_store(now, &mut ev).await;
        if !self.state.serving() {
            return ev;
        }
        let row = match db(&self.ctx)
            .and_then(|d| Ok(frames_db::get(&d.conn(), project_id, frame_uuid)?))
        {
            Ok(Some(row)) => row,
            Ok(None) => return ev,
            Err(e) => {
                tracing::error!(project_id, frame_uuid, error = %e, "local check: frame could not be read");
                return ev;
            }
        };
        if checked_state(row.local_state) {
            self.recheck(&row, now, &mut ev).await;
        }
        ev
    }

    pub fn degraded(&self) -> bool {
        self.degraded
    }

    pub fn network(&self) -> bool {
        self.network
    }

    // ── internals ───────────────────────────────────────────────────────

    fn mark_watch_gap(&mut self, now: Instant) {
        let until = now + self.timings.aggregate + self.timings.settle * 2;
        self.watch_gap_until = Some(self.watch_gap_until.map_or(until, |t| t.max(until)));
    }

    /// Keep the signal channel drained into the aggregator before (and
    /// between) slow work, so a burst never piles up behind a hash.
    fn pump(&mut self, now: Instant) {
        while let Ok(sig) = self.fs_rx.try_recv() {
            self.on_signal(sig, now);
        }
    }

    fn normalize(&self, p: &Path) -> PathBuf {
        match &self.canon_root {
            Some(c) => match p.strip_prefix(c) {
                Ok(rel) => self.root.join(rel),
                Err(_) => p.to_path_buf(),
            },
            None => p.to_path_buf(),
        }
    }

    fn wall_ms(&self, now: Instant) -> i64 {
        let (base_i, base_ms) = self.clock_base;
        base_ms + now.saturating_duration_since(base_i).as_millis() as i64
    }

    async fn check_store(&self) -> StoreState {
        let guard = Arc::clone(&self.guard);
        match tokio::task::spawn_blocking(move || guard.check_now()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "storage marker check task failed");
                self.state.clone()
            }
        }
    }

    async fn refresh_store(&mut self, now: Instant, ev: &mut Vec<StorageEvent>) {
        let state = self.check_store().await;
        self.recheck_store = false;
        self.next_check = now + STORE_CHECK_EVERY;
        if state != self.state {
            let was_serving = self.state.serving();
            if !was_serving && state.serving() {
                // Coming back: a sweep confirms the files before anything
                // new is reported (§9.1).
                self.next_sweep = now;
            }
            self.state = state.clone();
            ev.push(StorageEvent::Availability(state));
        }
    }

    async fn canary_step(&mut self, now: Instant) {
        // Only a writable, marked store gets a canary: a write refused by a
        // read-only or unmarked root is not a dead watcher (Task 8 carry).
        if self.watcher.is_none() || self.state != StoreState::Available {
            return;
        }
        if self.canary.due(now) {
            let root = self.root.clone();
            match tokio::task::spawn_blocking(move || watch::write_canary(&root)).await {
                Ok(Ok(())) => self.canary.wrote(now),
                Ok(Err(e)) => {
                    tracing::debug!(path = %self.root.display(), error = %e, "canary write failed; not counted against the watcher")
                }
                Err(e) => tracing::warn!(error = %e, "canary write task failed"),
            }
        }
        if !self.canary_dead && self.canary.dead(now) {
            self.canary_dead = true;
            tracing::warn!(path = %self.root.display(), "collaboration folder watcher missed the canary; changes are seen by periodic check only");
        }
    }

    fn update_degraded(&mut self, now: Instant, ev: &mut Vec<StorageEvent>) {
        let watch_failed = self.watch_errors >= WATCH_ERRORS_DEGRADE;
        let degraded = self.watcher.is_none() || self.network || self.canary_dead || watch_failed;
        if degraded != self.degraded {
            self.degraded = degraded;
            if degraded {
                tracing::warn!(
                    path = %self.root.display(),
                    count = self.watch_errors,
                    "collaboration folder changes are seen by periodic check only"
                );
                self.next_sweep = self.next_sweep.min(now + self.timings.sweep_degraded);
            } else {
                tracing::info!(path = %self.root.display(), "collaboration folder watcher healthy again");
            }
            ev.push(StorageEvent::WatcherDegraded(degraded));
        }
    }

    fn next_sweep_delay(&mut self) -> Duration {
        if self.degraded {
            return self.timings.sweep_degraded;
        }
        let d = sweep::next_sweep_delay(false, &mut self.rng);
        // scale the jittered hour to the configured healthy cadence
        self.timings
            .sweep_healthy
            .mul_f64(d.as_secs_f64() / sweep::SWEEP_HEALTHY.as_secs_f64())
    }

    /// A changed directory is walked: every frame file beneath it is a
    /// changed path of its own (Task 8 carry).
    fn expand_changed(&self, changed: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for p in changed {
            if p.is_dir() {
                for entry in walkdir::WalkDir::new(p)
                    .into_iter()
                    .filter_entry(|e| e.file_name() != ".athenaeum")
                {
                    match entry {
                        Ok(e) if e.file_type().is_file() => {
                            if !watch::is_ignored(&self.root, e.path()) {
                                out.push(e.path().to_path_buf());
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(path = %p.display(), error = %e, "walk of a changed folder failed")
                        }
                    }
                }
            } else {
                out.push(p.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn rows_under(&self, removed: &Path) -> Result<Vec<LocalFrameRow>, ApiError> {
        let db = db(&self.ctx)?;
        Ok(frames_db::rows_under(
            &db.conn(),
            &removed.to_string_lossy(),
        )?)
    }

    async fn on_changed(&mut self, path: &Path, now: Instant, ev: &mut Vec<StorageEvent>) {
        let path_str = path.to_string_lossy().to_string();
        let row = match db(&self.ctx)
            .and_then(|d| Ok(frames_db::find_by_landed_path(&d.conn(), &path_str)?))
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(path = %path_str, error = %e, "changed path could not be looked up");
                return;
            }
        };
        match row {
            Some(row) if checked_state(row.local_state) => self.recheck(&row, now, ev).await,
            Some(row)
                if matches!(
                    row.local_state,
                    LocalState::Missing
                        | LocalState::Wanted
                        | LocalState::AwaitingChoice
                        | LocalState::NotKept
                        | LocalState::OwnMissing
                ) =>
            {
                if row.local_state == LocalState::Wanted
                    && row.origin == FrameOrigin::Replica
                    && row.size_mtime_seen.is_some()
                    && !row.awaiting_gc
                {
                    // A re-included frame whose file was verified at this
                    // version (fix round 1): re-adopt, or quarantine an edit.
                    self.check_reincluded(&row, path, ev).await;
                } else {
                    // The file came back (FileBack / PutBack): stat + hash
                    // decide.
                    self.readopt(path, now, ev).await;
                }
            }
            // `idle`: an excluded frame is never served; nothing to do.
            Some(_) => {}
            None => {
                if !crate::scanner::is_frame_file(path) || watch::is_ignored(&self.root, path) {
                    return;
                }
                // An unknown file: a moved or put-back frame (§9.4 "Unknown
                // files"), else "Other files" (R18).
                if !self.readopt(path, now, ev).await {
                    let size_mtime = std::fs::metadata(path).ok().map(|m| Stamp::of(&m).encode());
                    let listed = db(&self.ctx).and_then(|d| {
                        Ok(frames_db::record_foreign_file(
                            &d.conn(),
                            &path_str,
                            None,
                            size_mtime.as_deref(),
                        )?)
                    });
                    match listed {
                        Ok(()) => {
                            tracing::info!(path = %path_str, "file is not part of any project; listed as foreign")
                        }
                        Err(e) => {
                            tracing::error!(path = %path_str, error = %e, "foreign file could not be listed")
                        }
                    }
                }
            }
        }
    }

    /// Re-adoption by hash; `true` when the file matched any cached frame.
    async fn readopt(&mut self, path: &Path, now: Instant, ev: &mut Vec<StorageEvent>) -> bool {
        match crate::api::collab_live::replace::adopt_by_hash_detailed(
            &self.ctx, &self.node, &self.root, path,
        )
        .await
        {
            Ok(out) => {
                if !out.parked.is_empty() {
                    self.schedule_parked_retry(now);
                }
                for a in out.adopted {
                    if a.from != a.to {
                        ev.push(StorageEvent::StateChanged {
                            project_id: a.project_id,
                            frame_uuid: a.frame_uuid,
                            from: a.from,
                            to: a.to,
                        });
                    }
                }
                if out.matched {
                    if let Err(e) = db(&self.ctx).and_then(|d| {
                        Ok(frames_db::forget_foreign_file(
                            &d.conn(),
                            &path.to_string_lossy(),
                        )?)
                    }) {
                        tracing::warn!(path = %path.display(), error = %e, "foreign-file entry could not be dropped");
                    }
                }
                out.matched
            }
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "re-adoption by hash failed");
                // unknown ⇒ not listed: a failed lookup must not misfile it
                true
            }
        }
    }

    /// `stat_verdict` against the recorded stamp; the same bytes re-record
    /// it, different bytes quarantine (replica) / mark changed (own), a
    /// missing file enters the settle.
    async fn recheck(&mut self, row: &LocalFrameRow, now: Instant, ev: &mut Vec<StorageEvent>) {
        let Some(path) = row.landed_path.clone() else {
            return;
        };
        let recorded = row.size_mtime_seen.as_deref().and_then(Stamp::parse);
        let p = PathBuf::from(&path);
        let verdict =
            match tokio::task::spawn_blocking(move || sweep::stat_verdict(&p, recorded)).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(path, error = %e, "stat task failed");
                    return;
                }
            };
        let changed_state = matches!(
            row.local_state,
            LocalState::Quarantined | LocalState::OwnChanged
        );
        let current = match verdict {
            StatVerdict::Unreadable(e) => {
                tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path, error = %e, "landed file unreadable; left as is");
                return;
            }
            StatVerdict::Missing => {
                if !changed_state {
                    // the settle decides (a move shows up as one event)
                    self.agg
                        .observe(&FsSignal::Touched(PathBuf::from(&path)), now);
                }
                return;
            }
            StatVerdict::Same if !changed_state => return,
            StatVerdict::Same => match recorded {
                Some(s) => s,
                None => return,
            },
            StatVerdict::Drifted(s) => s,
        };
        // R21: a file already rejected at this very stamp is not rehashed.
        let rejected = match db(&self.ctx).and_then(|d| {
            Ok(frames_db::rejected_size_mtime(
                &d.conn(),
                &row.project_id,
                &row.frame_uuid,
            )?)
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "rejected stamp could not be read");
                return;
            }
        };
        if rejected.as_deref() == Some(current.encode().as_str()) {
            return;
        }
        let same_bytes = if current.size as i64 != row.byte_size {
            false
        } else {
            match crate::api::collab_exchange::xxh3_on_blocking(Path::new(&path)).await {
                Ok(h) => h == row.xxh3,
                Err(e) => {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path, error = %format!("{e:#}"), "landed file could not be hashed; left as is");
                    return;
                }
            }
        };
        match (changed_state, same_bytes) {
            (false, true) => self.stamp_drift(row, &current).await,
            (false, false) => self.content_changed(row, &path, &current, false, ev).await,
            (true, true) => self.bytes_back(row, &path, &current, ev).await,
            (true, false) => {
                // still changed, at a new stamp: remember it (R21)
                if let Err(e) = self.with_frame_tx(row, |tx| {
                    frames_db::set_rejected_size_mtime(tx, &row.project_id, &row.frame_uuid, &current.encode())?;
                    tx.execute(
                        "UPDATE collab_quarantine SET observed_size_mtime = ?3 WHERE project_id = ?1 AND frame_uuid = ?2",
                        rusqlite::params![row.project_id, row.frame_uuid, current.encode()],
                    )?;
                    Ok(Some(()))
                })
                .await
                {
                    tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "changed-file stamp could not be recorded");
                }
            }
        }
    }

    async fn stamp_drift(&mut self, row: &LocalFrameRow, current: &Stamp) {
        let res = self
            .with_frame_tx(row, |tx| {
                frames_db::set_size_mtime_seen(
                    tx,
                    &row.project_id,
                    &row.frame_uuid,
                    &current.encode(),
                )?;
                Ok(Some(()))
            })
            .await;
        match res {
            Ok(_) => {
                tracing::debug!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, "stamp drift with the same bytes; stamp re-recorded")
            }
            Err(e) => {
                tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "stamp could not be re-recorded")
            }
        }
    }

    /// `unpark`: the row was parked over a dead store entry — `awaiting_gc`
    /// is cleared in the same transaction, so a later "Re-fetch original"
    /// fetches it (Task 11).
    async fn content_changed(
        &mut self,
        row: &LocalFrameRow,
        path: &str,
        current: &Stamp,
        unpark: bool,
        ev: &mut Vec<StorageEvent>,
    ) {
        let Some(to) = transition(row.origin, row.local_state, StateEvent::ContentChanged) else {
            return;
        };
        // Stop serving FIRST (L5: the moment a change is confirmed).
        if let Err(e) = self
            .node
            .unseed_project_frame(&row.project_id, &row.frame_uuid)
            .await
        {
            tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "changed frame could not be unseeded");
        }
        let replica = row.origin == FrameOrigin::Replica;
        let res = self
            .with_frame_tx(row, |tx| {
                if unpark {
                    tx.execute(
                        "UPDATE project_frames_local SET awaiting_gc = 0 WHERE project_id = ?1 AND frame_uuid = ?2",
                        rusqlite::params![row.project_id, row.frame_uuid],
                    )?;
                }
                let w = frames_db::set_local_state(tx, &row.project_id, &row.frame_uuid, to)?;
                if replica {
                    live_db::quarantine(
                        tx,
                        &QuarantineRow {
                            project_id: row.project_id.clone(),
                            frame_uuid: row.frame_uuid.clone(),
                            path: path.to_string(),
                            detected_at: String::new(),
                            quarantined_version: row.content_version,
                            observed_size_mtime: Some(current.encode()),
                        },
                    )?;
                }
                frames_db::set_rejected_size_mtime(
                    tx,
                    &row.project_id,
                    &row.frame_uuid,
                    &current.encode(),
                )?;
                Ok(w.map(|w| (w.from, w.to)))
            })
            .await;
        match res {
            Ok(Some((from, to))) => {
                if replica {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path, "replica changed on disk; quarantined");
                } else {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path, "own frame changed on disk; not served");
                }
                ev.push(StorageEvent::StateChanged {
                    project_id: row.project_id.clone(),
                    frame_uuid: row.frame_uuid.clone(),
                    from,
                    to,
                });
                if replica {
                    ev.push(StorageEvent::Quarantined {
                        project_id: row.project_id.clone(),
                        frame_uuid: row.frame_uuid.clone(),
                        file_name: row.file_name.clone(),
                    });
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "changed frame could not be quarantined")
            }
        }
    }

    /// A quarantined / own-changed frame whose bytes hash to the current
    /// version again (Task 9 ruling): seeded again, served again.
    async fn bytes_back(
        &mut self,
        row: &LocalFrameRow,
        path: &str,
        current: &Stamp,
        ev: &mut Vec<StorageEvent>,
    ) {
        let Some(to) = transition(row.origin, row.local_state, StateEvent::StampDrift) else {
            return;
        };
        match self
            .node
            .seed_project_frame(
                &row.project_id,
                &row.frame_uuid,
                row.content_version,
                Path::new(path),
            )
            .await
        {
            Ok(h) if h.to_string() == row.blake3 => {}
            Ok(h) => {
                tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, blake3 = %h, "restored file hashes to other content; left changed");
                if let Err(e) = self
                    .node
                    .unseed_project_frame(&row.project_id, &row.frame_uuid)
                    .await
                {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "unseed after a mismatch failed");
                }
                return;
            }
            Err(e) => {
                tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "restored file could not be seeded; left changed");
                return;
            }
        }
        let res = self
            .with_frame_tx(row, |tx| {
                frames_db::set_size_mtime_seen(tx, &row.project_id, &row.frame_uuid, &current.encode())?;
                tx.execute(
                    "UPDATE project_frames_local SET rejected_size_mtime = NULL WHERE project_id = ?1 AND frame_uuid = ?2",
                    rusqlite::params![row.project_id, row.frame_uuid],
                )?;
                live_db::unquarantine(tx, &row.project_id, &row.frame_uuid)?;
                let w = frames_db::set_local_state(tx, &row.project_id, &row.frame_uuid, to)?;
                Ok(w.map(|w| (w.from, w.to)))
            })
            .await;
        match res {
            Ok(Some((from, to))) => {
                tracing::info!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path, "changed file is back to the published bytes; served again");
                ev.push(StorageEvent::StateChanged {
                    project_id: row.project_id.clone(),
                    frame_uuid: row.frame_uuid.clone(),
                    from,
                    to,
                });
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "restored frame could not be recorded")
            }
        }
    }

    /// Run `f` in ONE transaction under the project's disk lock, only while
    /// the row still is what the caller decided on (same state, same landed
    /// path) — a concurrent landing or user action wins. `Ok(None)` when the
    /// row moved on.
    async fn with_frame_tx<T>(
        &self,
        row: &LocalFrameRow,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<Option<T>>,
    ) -> Result<Option<T>, ApiError> {
        with_frame_tx(&self.ctx, row, f).await
    }

    async fn file_gone(
        &mut self,
        rows: Vec<LocalFrameRow>,
        now: Instant,
        holders: &dyn HolderView,
        ev: &mut Vec<StorageEvent>,
    ) {
        // Re-read every row: a re-adoption earlier in this tick (a moved
        // file, the unseen-moves walk) may have moved it on (fix round 1).
        let rows: Vec<LocalFrameRow> = rows
            .into_iter()
            .filter_map(|r| {
                match db(&self.ctx)
                    .and_then(|d| Ok(frames_db::get(&d.conn(), &r.project_id, &r.frame_uuid)?))
                {
                    Ok(fresh) => fresh,
                    Err(e) => {
                        tracing::error!(project_id = %r.project_id, frame_uuid = %r.frame_uuid, error = %e, "gone frame could not be re-read");
                        None
                    }
                }
            })
            .filter(|r| {
                r.landed_path
                    .as_deref()
                    .is_some_and(|p| !Path::new(p).exists())
            })
            .filter(|r| matches!(r.local_state, LocalState::Held | LocalState::OwnHeld))
            .collect();
        if rows.is_empty() {
            return;
        }
        let now_ms = self.wall_ms(now);
        let day_ago = now_ms - deletions::SECOND_DELETION.as_millis() as i64;
        let history = match db(&self.ctx)
            .and_then(|d| Ok(live_db::deletions_since(&d.conn(), day_ago)?))
        {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "deletion history could not be read; deletions left unruled");
                return;
            }
        };
        let batch: Vec<(String, String)> = rows
            .iter()
            .filter(|r| r.origin == FrameOrigin::Replica)
            .map(|r| (r.project_id.clone(), r.frame_uuid.clone()))
            .collect();
        let ruled = deletions::rule_batch(&history, &batch, now_ms);
        let mut awaiting = 0usize;
        let mut choice_projects: Vec<String> = Vec::new();
        let mut applied: Vec<&LocalFrameRow> = Vec::new();

        for r in &rows {
            let key = (r.project_id.clone(), r.frame_uuid.clone());
            let ruling = ruled
                .rulings
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, d)| *d);
            let res = self
                .with_frame_tx(r, |tx| {
                    let Some(gone) = transition(r.origin, r.local_state, StateEvent::FileGone)
                    else {
                        return Ok(None);
                    };
                    frames_db::set_local_state(tx, &r.project_id, &r.frame_uuid, gone)?;
                    // Unseeded below: the stamp no longer records seeded
                    // bytes (it is the "known seeded" record re-inclusion
                    // trusts — fix round 1).
                    tx.execute(
                        "UPDATE project_frames_local SET size_mtime_seen = NULL
                         WHERE project_id = ?1 AND frame_uuid = ?2",
                        rusqlite::params![r.project_id, r.frame_uuid],
                    )?;
                    let mut to = gone;
                    if let Some(ruling) = ruling {
                        live_db::record_deletion(tx, &r.project_id, &r.frame_uuid, now_ms)?;
                        if let Some(next) = transition(r.origin, gone, StateEvent::Ruled(ruling)) {
                            frames_db::set_local_state(tx, &r.project_id, &r.frame_uuid, next)?;
                            to = next;
                        }
                    }
                    Ok(Some(to))
                })
                .await;
            match res {
                Ok(Some(to)) => {
                    tracing::info!(
                        project_id = %r.project_id,
                        frame_uuid = %r.frame_uuid,
                        from_state = r.local_state.as_db_str(),
                        to_state = to.as_db_str(),
                        "landed file gone"
                    );
                    applied.push(r);
                    // The dead entry stops being pinned, so the collab GC can
                    // drop it (P31) — after the state write, so a frame a
                    // concurrent landing revived is never unseeded.
                    if let Err(e) = self
                        .node
                        .unseed_project_frame(&r.project_id, &r.frame_uuid)
                        .await
                    {
                        tracing::warn!(project_id = %r.project_id, frame_uuid = %r.frame_uuid, error = %e, "gone frame could not be unseeded");
                    }
                    if to == LocalState::AwaitingChoice {
                        awaiting += 1;
                        choice_projects.push(r.project_id.clone());
                    }
                    ev.push(StorageEvent::StateChanged {
                        project_id: r.project_id.clone(),
                        frame_uuid: r.frame_uuid.clone(),
                        from: r.local_state,
                        to,
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(project_id = %r.project_id, frame_uuid = %r.frame_uuid, error = %e, "gone frame could not be recorded")
                }
            }
        }

        // Mass: every frame deleted earlier in the window joins the choice.
        if ruled.mass {
            for (pid, uuid) in &ruled.pull_into_choice {
                let row = match db(&self.ctx)
                    .and_then(|d| Ok(frames_db::get(&d.conn(), pid, uuid)?))
                {
                    Ok(Some(row)) => row,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::error!(project_id = %pid, frame_uuid = %uuid, error = %e, "earlier deletion could not be read");
                        continue;
                    }
                };
                let Some(to) = transition(
                    row.origin,
                    row.local_state,
                    StateEvent::Ruled(DeletionRuling::AwaitChoice),
                ) else {
                    continue;
                };
                let res = self
                    .with_frame_tx(&row, |tx| {
                        let w = frames_db::set_local_state(tx, pid, uuid, to)?;
                        Ok(w.map(|w| (w.from, w.to)))
                    })
                    .await;
                match res {
                    Ok(Some((from, to))) => {
                        awaiting += 1;
                        choice_projects.push(pid.clone());
                        ev.push(StorageEvent::StateChanged {
                            project_id: pid.clone(),
                            frame_uuid: uuid.clone(),
                            from,
                            to,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::error!(project_id = %pid, frame_uuid = %uuid, error = %e, "earlier deletion could not join the choice")
                    }
                }
            }
        }
        if awaiting > 0 {
            choice_projects.sort();
            choice_projects.dedup();
            let count = if ruled.mass {
                ruled.window_count
            } else {
                awaiting
            };
            tracing::warn!(
                count,
                window_count = ruled.window_count,
                "replica deletions await one choice"
            );
            ev.push(StorageEvent::DeletionChoice {
                count,
                project_ids: choice_projects,
            });
        }

        // Lost everywhere: nobody else holds the current version (L4) — only
        // for frames whose state write applied (fix round 1).
        for r in applied {
            if deletions::lost_everywhere(holders.other_holders(&r.project_id, &r.frame_uuid).total)
            {
                tracing::error!(project_id = %r.project_id, frame_uuid = %r.frame_uuid, path = r.landed_path.as_deref().unwrap_or_default(), "frame lost everywhere");
                ev.push(StorageEvent::FrameLost {
                    project_id: r.project_id.clone(),
                    frame_uuid: r.frame_uuid.clone(),
                    file_name: r.file_name.clone(),
                });
            }
        }

        if let Err(e) =
            db(&self.ctx).and_then(|d| Ok(live_db::prune_deletions(&d.conn(), day_ago)?))
        {
            tracing::warn!(error = %e, "old deletion records could not be pruned");
        }
    }

    async fn sweep_at(&mut self, now: Instant, _holders: &dyn HolderView) -> Vec<StorageEvent> {
        let mut ev = Vec::new();
        // Re-read the marker on every sweep (Task 8 carry).
        self.refresh_store(now, &mut ev).await;
        if !self.state.serving() {
            if ev.is_empty() {
                ev.push(StorageEvent::Availability(self.state.clone()));
            }
            return ev;
        }
        let delay = self.next_sweep_delay();
        self.next_sweep = now + delay;
        let rows =
            match db(&self.ctx).and_then(|d| Ok(frames_db::rows_with_landed_path(&d.conn())?)) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "stat sweep: landed frames could not be read");
                    return ev;
                }
            };
        let mut checked = 0usize;
        for row in rows {
            self.pump(now);
            if checked_state(row.local_state) {
                checked += 1;
                self.recheck(&row, now, &mut ev).await;
            }
        }
        // Parked frames are retried by every sweep too (fix round 1).
        self.retry_parked(now, &mut ev).await;
        tracing::debug!(count = checked, "stat sweep finished");
        ev
    }

    fn schedule_parked_retry(&mut self, now: Instant) {
        let at = now + PARKED_RETRY_AFTER;
        self.next_parked_retry = Some(self.next_parked_retry.map_or(at, |t| t.min(at)));
    }

    /// Retry every parked frame (a moved file whose re-seed hit a dead store
    /// entry). Per frame:
    /// - its file is gone or no longer the frame's size → released to a
    ///   plain fetch (`awaiting_gc` cleared);
    /// - the store entry is gone / partial / readable → re-seeded from the
    ///   recorded path (`held`); when that does not land, released;
    /// - the entry is still dead → still the frame's bytes: every sibling
    ///   frame sharing the hash (C10) is parked too, because its tag keeps
    ///   the dead entry alive and it cannot serve through it either; not the
    ///   frame's bytes any more → released.
    ///
    /// Frames still parked get the next retry one GC interval later.
    async fn retry_parked(&mut self, now: Instant, ev: &mut Vec<StorageEvent>) {
        self.next_parked_retry = None;
        let rows = match db(&self.ctx).and_then(|d| Ok(frames_db::parked_rows(&d.conn())?)) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "parked frames could not be read");
                self.schedule_parked_retry(now);
                return;
            }
        };
        let mut still = 0usize;
        for row in rows {
            self.pump(now);
            if self.retry_one_parked(&row, ev).await {
                still += 1;
            }
        }
        if still > 0 {
            tracing::debug!(count = still, "frames still parked over a dead store entry");
            self.schedule_parked_retry(now);
        }
    }

    /// `true` when the frame stays parked.
    async fn retry_one_parked(&mut self, row: &LocalFrameRow, ev: &mut Vec<StorageEvent>) -> bool {
        use crate::api::collab_live::replace::{land_candidate, Landing};
        use crate::sharing::iroh::node::BlobHealth;
        let path = PathBuf::from(row.landed_path.as_deref().unwrap_or_default());
        let size_ok = std::fs::metadata(&path)
            .map(|m| m.is_file() && m.len() as i64 == row.byte_size)
            .unwrap_or(false);
        if !size_ok {
            self.release_or_quarantine_parked(row, &path, "file gone or changed", None, ev)
                .await;
            return false;
        }
        let health = match row.blake3.parse::<iroh_blobs::Hash>() {
            Ok(h) => self.node.collab_blob_health(h).await.ok(),
            Err(e) => {
                tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "parked frame blake3 does not parse");
                None
            }
        };
        match health {
            None => true,
            Some(BlobHealth::Dead) => {
                if !self.file_matches(row, &path).await {
                    self.release_or_quarantine_parked(
                        row,
                        &path,
                        "file no longer the frame's bytes",
                        Some(false),
                        ev,
                    )
                    .await;
                    return false;
                }
                self.park_siblings(row, ev).await;
                true
            }
            Some(h) => match land_candidate(&self.ctx, &self.node, row, &path).await {
                Ok(Landing::Landed { from, to }) => {
                    tracing::info!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path = %path.display(), "parked frame re-seeded");
                    if from != to {
                        ev.push(StorageEvent::StateChanged {
                            project_id: row.project_id.clone(),
                            frame_uuid: row.frame_uuid.clone(),
                            from,
                            to,
                        });
                    }
                    false
                }
                Ok(Landing::Parked) => true,
                Ok(Landing::Refused) => {
                    if h == BlobHealth::Readable && self.file_matches(row, &path).await {
                        return true;
                    }
                    self.release_or_quarantine_parked(row, &path, "re-seed refused", None, ev)
                        .await;
                    false
                }
                Err(e) => {
                    tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "parked frame re-seed failed");
                    true
                }
            },
        }
    }

    /// Full-file xxh3 of `path` (off the runtime) equals the frame's.
    async fn file_matches(&self, row: &LocalFrameRow, path: &Path) -> bool {
        match crate::api::collab_exchange::xxh3_on_blocking(path).await {
            Ok(h) => h == row.xxh3,
            Err(e) => {
                tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path = %path.display(), error = %format!("{e:#}"), "parked frame could not be hashed");
                false
            }
        }
    }

    /// End a replica's parking whose re-seed did not happen. Task 11 (ledger
    /// ruling): a released frame must never be landed over a file that no
    /// longer holds its bytes. The parked row is a verified copy of THIS
    /// version (its stamp was recorded when it was parked), so a file still
    /// at its path with other bytes is an edit — spec §9.4 "Held ──content
    /// changed──▶ Quarantined (stops serving at once; file untouched)", I9
    /// "different bytes quarantine", L5: it is quarantined (Changed files),
    /// never released to a fetch that would replace it. A file that is gone,
    /// or still holds the frame's bytes (the landing then just records it,
    /// C1), is released. An own frame is never fetched: released as before.
    /// `holds`: whether the file holds the frame's bytes, when the caller
    /// already knows.
    async fn release_or_quarantine_parked(
        &mut self,
        row: &LocalFrameRow,
        path: &Path,
        reason: &str,
        holds: Option<bool>,
        ev: &mut Vec<StorageEvent>,
    ) {
        if row.origin == FrameOrigin::Replica {
            match std::fs::metadata(path) {
                Ok(m) if m.is_file() => {
                    let holds = match holds {
                        Some(h) => h,
                        None => {
                            m.len() as i64 == row.byte_size && self.file_matches(row, path).await
                        }
                    };
                    if !holds {
                        self.content_changed(
                            row,
                            &path.to_string_lossy(),
                            &Stamp::of(&m),
                            true,
                            ev,
                        )
                        .await;
                        return;
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path = %path.display(), error = %e, "parked frame's file could not be read; left parked");
                    return;
                }
            }
        }
        self.release_parked(row, reason).await;
    }

    /// Clear `awaiting_gc` (and the stamp): the frame is fetched normally.
    async fn release_parked(&self, row: &LocalFrameRow, reason: &str) {
        let res = self
            .with_frame_tx(row, |tx| {
                tx.execute(
                    "UPDATE project_frames_local SET awaiting_gc = 0, size_mtime_seen = NULL
                     WHERE project_id = ?1 AND frame_uuid = ?2",
                    rusqlite::params![row.project_id, row.frame_uuid],
                )?;
                Ok(Some(()))
            })
            .await;
        match res {
            Ok(_) => {
                tracing::info!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, reason, "parked frame released to a fetch")
            }
            Err(e) => {
                tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "parked frame could not be released")
            }
        }
    }

    /// C10: the dead entry is shared — every held sibling with the same
    /// hash reads through it too, and its tag keeps the entry from being
    /// collected. Park each at its own path.
    async fn park_siblings(&mut self, row: &LocalFrameRow, ev: &mut Vec<StorageEvent>) {
        let siblings = match db(&self.ctx)
            .and_then(|d| Ok(frames_db::rows_with_blake3(&d.conn(), &row.blake3)?))
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "siblings of a dead store entry could not be read");
                return;
            }
        };
        for sib in siblings {
            if (sib.project_id == row.project_id && sib.frame_uuid == row.frame_uuid)
                || !sib.local_state.servable()
            {
                continue;
            }
            let Some(path) = sib.landed_path.clone().map(PathBuf::from) else {
                continue;
            };
            tracing::warn!(
                project_id = %sib.project_id,
                frame_uuid = %sib.frame_uuid,
                path = %path.display(),
                "frame shares a dead store entry; parked until the entry is collected"
            );
            match crate::api::collab_live::replace::park_row(&self.ctx, &self.node, &sib, &path)
                .await
            {
                Ok(()) => {
                    let to = if sib.origin == FrameOrigin::Own {
                        LocalState::OwnMissing
                    } else {
                        LocalState::Wanted
                    };
                    ev.push(StorageEvent::StateChanged {
                        project_id: sib.project_id.clone(),
                        frame_uuid: sib.frame_uuid.clone(),
                        from: sib.local_state,
                        to,
                    });
                }
                Err(e) => {
                    tracing::error!(project_id = %sib.project_id, frame_uuid = %sib.frame_uuid, error = %e, "sibling of a dead store entry could not be parked")
                }
            }
        }
    }

    /// A re-included frame (still `wanted`, its stamp recorded at this
    /// version) whose landed file the manifest applier handed over: the same
    /// bytes are re-seeded and verified (`held`); an edit made while it was
    /// idle is quarantined (I9, L5) — never fetched over.
    async fn check_reincluded(
        &mut self,
        row: &LocalFrameRow,
        path: &Path,
        ev: &mut Vec<StorageEvent>,
    ) {
        use crate::api::collab_live::replace::{land_candidate, Landing};
        let recorded = row.size_mtime_seen.as_deref().and_then(Stamp::parse);
        let p = path.to_path_buf();
        let verdict =
            match tokio::task::spawn_blocking(move || sweep::stat_verdict(&p, recorded)).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "stat task failed");
                    return;
                }
            };
        let current = match verdict {
            StatVerdict::Missing | StatVerdict::Unreadable(_) => return,
            StatVerdict::Same => None,
            StatVerdict::Drifted(s) => Some(s),
        };
        let same_bytes = match current {
            None => true,
            Some(s) if s.size as i64 != row.byte_size => false,
            Some(_) => self.file_matches(row, path).await,
        };
        if same_bytes {
            match land_candidate(&self.ctx, &self.node, row, path).await {
                Ok(Landing::Landed { from, to }) => {
                    if from != to {
                        ev.push(StorageEvent::StateChanged {
                            project_id: row.project_id.clone(),
                            frame_uuid: row.frame_uuid.clone(),
                            from,
                            to,
                        });
                    }
                }
                Ok(_) => {
                    tracing::warn!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, path = %path.display(), "re-included frame could not be re-seeded; stays wanted")
                }
                Err(e) => {
                    tracing::error!(project_id = %row.project_id, frame_uuid = %row.frame_uuid, error = %e, "re-included frame could not be re-adopted")
                }
            }
            return;
        }
        let Some(current) = current else { return };
        self.content_changed(row, &path.to_string_lossy(), &current, false, ev)
            .await;
    }

    /// Moves the watcher did not see: before removals are ruled, walk the
    /// root for frame files no row references whose size matches a removed
    /// frame, and re-adopt them by hash (fix round 1).
    async fn adopt_unseen_moves(
        &mut self,
        removed: &[LocalFrameRow],
        now: Instant,
        ev: &mut Vec<StorageEvent>,
    ) {
        let sizes: std::collections::HashSet<i64> = removed
            .iter()
            .filter(|r| matches!(r.local_state, LocalState::Held | LocalState::OwnHeld))
            .filter(|r| {
                r.landed_path
                    .as_deref()
                    .is_some_and(|p| !Path::new(p).exists())
            })
            .map(|r| r.byte_size)
            .collect();
        if sizes.is_empty() {
            return;
        }
        let root = self.root.clone();
        let walked = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for entry in walkdir::WalkDir::new(&root)
                .into_iter()
                .filter_entry(|e| e.file_name() != ".athenaeum")
            {
                match entry {
                    Ok(e) if e.file_type().is_file() => {
                        let path = e.path();
                        if !crate::scanner::is_frame_file(path) || watch::is_ignored(&root, path) {
                            continue;
                        }
                        if e.metadata()
                            .is_ok_and(|m| sizes.contains(&(m.len() as i64)))
                        {
                            out.push(path.to_path_buf());
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(path = %root.display(), error = %e, "walk for unseen moves failed on one entry")
                    }
                }
            }
            out
        })
        .await;
        let candidates = match walked {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "walk for unseen moves failed");
                return;
            }
        };
        let mut adopted = 0usize;
        for path in candidates {
            self.pump(now);
            let known = db(&self.ctx).and_then(|d| {
                Ok(frames_db::find_by_landed_path(
                    &d.conn(),
                    &path.to_string_lossy(),
                )?)
            });
            match known {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "unseen-move candidate could not be looked up");
                    continue;
                }
            }
            if self.readopt(&path, now, ev).await {
                adopted += 1;
            }
        }
        if adopted > 0 {
            tracing::info!(
                count = adopted,
                "moves the watcher did not see re-adopted before ruling deletions"
            );
        }
    }
}

/// States whose landed file the sweep and the fast path recheck.
fn checked_state(s: LocalState) -> bool {
    matches!(
        s,
        LocalState::Held | LocalState::OwnHeld | LocalState::Quarantined | LocalState::OwnChanged
    )
}

async fn with_frame_tx<T>(
    ctx: &ServiceContext,
    row: &LocalFrameRow,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<Option<T>>,
) -> Result<Option<T>, ApiError> {
    let lock = crate::api::collab_exchange::project_disk_lock(ctx, &row.project_id)?;
    let _guard = lock.lock().await;
    frame_tx_locked(ctx, row, f)
}

/// [`with_frame_tx`] for a caller that already holds the project's disk
/// lock. `BEGIN IMMEDIATE`; the row must still match on state, landed path,
/// content version and stamp. Also the write path of the device-replace
/// adoption (`replace::land_candidate`, `replace::park_row`, Task 11).
pub(crate) fn frame_tx_locked<T>(
    ctx: &ServiceContext,
    row: &LocalFrameRow,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<Option<T>>,
) -> Result<Option<T>, ApiError> {
    let db = db(ctx)?;
    let mut conn = db.conn();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(cur) = frames_db::get(&tx, &row.project_id, &row.frame_uuid)? else {
        return Ok(None);
    };
    if cur.local_state != row.local_state
        || cur.landed_path != row.landed_path
        || cur.content_version != row.content_version
        || cur.size_mtime_seen != row.size_mtime_seen
    {
        tracing::debug!(
            project_id = %row.project_id,
            frame_uuid = %row.frame_uuid,
            from_state = row.local_state.as_db_str(),
            to_state = cur.local_state.as_db_str(),
            "frame moved on meanwhile; left to the newer write"
        );
        return Ok(None);
    }
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

// ── user actions ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionAction {
    Refetch,
    StopKeeping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedAction {
    RefetchOriginal,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedOutcome {
    /// `true`: the changed file went to the system trash; `false`: it was
    /// deleted after the user confirmed.
    pub trashed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCopyRow {
    pub frame_uuid: String,
    pub file_name: String,
    pub holders_online: usize,
    pub holders_total: usize,
    pub at_risk: bool,
}

/// The system trash, behind a seam so a test can stand in for it.
pub trait Trash: Send + Sync {
    fn delete(&self, path: &Path) -> Result<(), String>;
}

/// The OS trash through the `trash` crate.
pub struct SystemTrash;

impl Trash for SystemTrash {
    fn delete(&self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

fn require_project(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> Result<crate::db::collab::CollabProjectRow, ApiError> {
    crate::db::collab::get_project(conn, project_id)?.ok_or_else(|| {
        tracing::warn!(project_id, "unknown collaboration project");
        ApiError::NotFound(format!("unknown project {project_id}"))
    })
}

/// Apply `ev` to every row of `project_id` in `from` (optionally only the
/// named frames) in one transaction. A row in any other state is skipped.
fn apply_user_event(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuids: Option<&[String]>,
    from: LocalState,
    ev: StateEvent,
) -> Result<usize, ApiError> {
    let db = db(ctx)?;
    let mut conn = db.conn();
    require_project(&conn, project_id)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let rows = frames_db::list_by_state(&tx, project_id, &[from])?;
    let mut n = 0;
    for row in rows {
        if frame_uuids.is_some_and(|u| !u.contains(&row.frame_uuid)) {
            continue;
        }
        let Some(to) = transition(row.origin, row.local_state, ev) else {
            continue;
        };
        frames_db::set_local_state(&tx, project_id, &row.frame_uuid, to)?;
        // the wave-2 need set still reads `locally_declined`
        tx.execute(
            "UPDATE project_frames_local SET locally_declined = ?3 WHERE project_id = ?1 AND frame_uuid = ?2",
            rusqlite::params![project_id, row.frame_uuid, to == LocalState::NotKept],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// Answer the deletion choice (L4) for every awaiting frame of a project,
/// or only the named ones. Reversible: "Stop keeping" → the "Not kept" list
/// ([`keep_again`]). Returns the frames moved.
pub fn resolve_deletions(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuids: Option<&[String]>,
    action: DeletionAction,
) -> Result<usize, ApiError> {
    let (ev, outcome) = match action {
        DeletionAction::Refetch => (StateEvent::Refetch, "refetch"),
        DeletionAction::StopKeeping => (StateEvent::StopKeeping, "stop_keeping"),
    };
    let n = apply_user_event(ctx, project_id, frame_uuids, LocalState::AwaitingChoice, ev).map_err(|e| {
        tracing::error!(project_id, outcome, error = %e, "deletion choice could not be applied");
        e
    })?;
    tracing::info!(project_id, count = n, outcome, "deletion choice resolved");
    Ok(n)
}

/// "Keep again" (L6), per frame or for all.
pub fn keep_again(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuids: Option<&[String]>,
) -> Result<usize, ApiError> {
    let n = apply_user_event(
        ctx,
        project_id,
        frame_uuids,
        LocalState::NotKept,
        StateEvent::KeepAgain,
    )
    .map_err(|e| {
        tracing::error!(project_id, error = %e, "keep again could not be applied");
        e
    })?;
    tracing::info!(project_id, count = n, "frames kept again");
    Ok(n)
}

/// Resolve a changed (quarantined) replica (L5).
///
/// - `RefetchOriginal`: the edited file goes to the system trash; when the
///   trash refuses, the call answers `Conflict("trash_unavailable: …")`
///   unless `confirmed_delete`, in which case the file is deleted. The frame
///   is then wanted again.
/// - `Delete`: requires `confirmed_delete`; the file is deleted and the frame
///   is not kept (reversible through [`keep_again`]).
pub async fn resolve_changed_file(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    project_id: &str,
    frame_uuid: &str,
    action: ChangedAction,
    confirmed_delete: bool,
) -> Result<ChangedOutcome, ApiError> {
    resolve_changed_file_with(
        ctx,
        node,
        project_id,
        frame_uuid,
        action,
        confirmed_delete,
        Arc::new(SystemTrash),
    )
    .await
}

/// [`resolve_changed_file`] with an injected [`Trash`].
pub async fn resolve_changed_file_with(
    ctx: &ServiceContext,
    node: &SharedIrohNode,
    project_id: &str,
    frame_uuid: &str,
    action: ChangedAction,
    confirmed_delete: bool,
    trash: Arc<dyn Trash>,
) -> Result<ChangedOutcome, ApiError> {
    // The state is validated under the project's disk lock BEFORE anything
    // is trashed or deleted, and the lock is held through the state write
    // (fix round 1).
    let lock = crate::api::collab_exchange::project_disk_lock(ctx, project_id)?;
    let _guard = lock.lock().await;
    let (row, path) = {
        let db = db(ctx)?;
        let conn = db.conn();
        require_project(&conn, project_id)?;
        let Some(row) = frames_db::get(&conn, project_id, frame_uuid)? else {
            tracing::warn!(project_id, frame_uuid, "changed file: unknown frame");
            return Err(ApiError::NotFound(format!("unknown frame {frame_uuid}")));
        };
        if row.local_state != LocalState::Quarantined {
            tracing::warn!(
                project_id,
                frame_uuid,
                state = row.local_state.as_db_str(),
                "changed file: the frame is not quarantined"
            );
            return Err(ApiError::Conflict(format!(
                "frame {frame_uuid} is not a changed file"
            )));
        }
        let path = live_db::list_quarantine(&conn, project_id)?
            .into_iter()
            .find(|q| q.frame_uuid == frame_uuid)
            .map(|q| q.path)
            .or_else(|| row.landed_path.clone());
        (row, path.map(PathBuf::from))
    };
    let ev = match action {
        ChangedAction::Delete => StateEvent::DeleteChanged,
        ChangedAction::RefetchOriginal => StateEvent::Refetch,
    };
    let Some(to) = transition(row.origin, row.local_state, ev) else {
        return Err(ApiError::Conflict(format!(
            "frame {frame_uuid} is not a changed file"
        )));
    };
    let trashed = match action {
        ChangedAction::Delete => {
            if !confirmed_delete {
                tracing::warn!(
                    project_id,
                    frame_uuid,
                    "changed file delete refused: not confirmed"
                );
                return Err(ApiError::Invalid(
                    "confirm the deletion of the changed file first".to_string(),
                ));
            }
            if let Some(p) = &path {
                remove_changed_file(p)?;
            }
            false
        }
        ChangedAction::RefetchOriginal => {
            let trashed = match &path {
                Some(p) if p.exists() => {
                    let (t, p2) = (Arc::clone(&trash), p.clone());
                    let res = tokio::task::spawn_blocking(move || t.delete(&p2))
                        .await
                        .map_err(|e| ApiError::Internal(format!("trash task join: {e}")))?;
                    match res {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::warn!(project_id, frame_uuid, path = %p.display(), error = %e, "system trash refused the changed file");
                            if !confirmed_delete {
                                return Err(ApiError::Conflict(format!(
                                    "{TRASH_UNAVAILABLE}: the system trash is not available — confirm to delete the changed file"
                                )));
                            }
                            remove_changed_file(p)?;
                            false
                        }
                    }
                }
                _ => false,
            };
            trashed
        }
    };
    if let Err(e) = node.unseed_project_frame(project_id, frame_uuid).await {
        tracing::debug!(project_id, frame_uuid, error = %e, "changed file: unseed skipped");
    }
    let applied = frame_tx_locked(ctx, &row, |tx| {
        live_db::unquarantine(tx, project_id, frame_uuid)?;
        tx.execute(
            "UPDATE project_frames_local SET rejected_size_mtime = NULL, size_mtime_seen = NULL, locally_declined = ?3
             WHERE project_id = ?1 AND frame_uuid = ?2",
            rusqlite::params![project_id, frame_uuid, to == LocalState::NotKept],
        )?;
        frames_db::set_local_state(tx, project_id, frame_uuid, to)?;
        Ok(Some(()))
    })
    .map_err(|e| {
        tracing::error!(project_id, frame_uuid, error = %e, "changed file resolution could not be recorded");
        e
    })?;
    if applied.is_none() {
        tracing::warn!(
            project_id,
            frame_uuid,
            "changed file moved on while it was resolved"
        );
        return Err(ApiError::Conflict(format!(
            "frame {frame_uuid} changed while it was resolved"
        )));
    }
    tracing::info!(
        project_id,
        frame_uuid,
        to_state = to.as_db_str(),
        outcome = if trashed { "trashed" } else { "deleted" },
        "changed file resolved"
    );
    Ok(ChangedOutcome { trashed })
}

fn remove_changed_file(p: &Path) -> Result<(), ApiError> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            tracing::error!(path = %p.display(), error = %e, "changed file could not be deleted");
            Err(ApiError::Internal(format!("delete {}: {e}", p.display())))
        }
    }
}

/// The last-copy warning (L4, I7) for the frames a "Stop keeping" would
/// drop: fewer than 2 other holders of the current version, offline
/// included. Unknown frames are left out.
pub fn last_copy_report(
    ctx: &ServiceContext,
    holders: &dyn HolderView,
    project_id: &str,
    frame_uuids: &[String],
) -> Result<Vec<LastCopyRow>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    require_project(&conn, project_id)?;
    let mut out = Vec::new();
    for uuid in frame_uuids {
        let Some(row) = frames_db::get(&conn, project_id, uuid)? else {
            continue;
        };
        let r = holders.other_holders(project_id, uuid);
        out.push(LastCopyRow {
            frame_uuid: row.frame_uuid,
            file_name: row.file_name,
            holders_online: r.online,
            holders_total: r.total,
            at_risk: deletions::last_copy_warning(r.total),
        });
    }
    Ok(out)
}

/// Re-derive every replica row's inclusion after a policy / role / toggle
/// change (P10): published ∧ accepted ∧ role allowed ∧ policy matching →
/// `idle` → `held`/`wanted` (stat + hash decide); otherwise `wanted`/`held`/
/// `awaiting_choice`/`missing` → `idle` (file kept, not served, not
/// fetched). Auto-replicate off changes no state. One transaction; returns
/// the rows moved.
pub fn apply_policy(ctx: &ServiceContext, project_id: &str) -> Result<usize, ApiError> {
    let db = db(ctx)?;
    let mut conn = db.conn();
    let project = require_project(&conn, project_id)?;
    let allowed = crate::api::collab_exchange::role_allows_replication(
        &project.data_role,
        project.is_coordinator,
    );
    let policy = crate::api::collab_exchange::read_policy(&project);
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut moved = 0usize;
    let mut routes: Vec<PathBuf> = Vec::new();
    for row in frames_db::list_for_project(&tx, project_id)? {
        if row.origin != FrameOrigin::Replica {
            continue;
        }
        let include = row.state == "published"
            && row.accepted
            && allowed
            && crate::api::collab_exchange::policy_matches(&row, &policy);
        let ev = if include {
            StateEvent::Reincluded
        } else {
            StateEvent::Excluded
        };
        if transition(row.origin, row.local_state, ev).is_none() {
            continue;
        }
        // No hashing inside this write transaction (fix round 1): a file
        // that needs a hash is handed to the storage engine after commit.
        let to = if ev == StateEvent::Reincluded {
            let (to, route) = frames_db::reinclude(&tx, &row)?;
            routes.extend(route);
            to
        } else {
            frames_db::exclude_to_idle(&tx, &row)?
        };
        if to != row.local_state {
            moved += 1;
        }
    }
    tx.commit()?;
    for path in routes {
        if !watch::route_touched(&path) {
            tracing::debug!(project_id, path = %path.display(), "no storage engine watches this file; the re-included frame stays wanted");
        }
    }
    tracing::info!(
        project_id,
        count = moved,
        "replication scope applied to local frame states"
    );
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;
    use crate::db::collab_frames::{self as frames_db, LocalState};

    struct Holders(usize);
    impl HolderView for Holders {
        fn other_holders(&self, _: &str, _: &str) -> Redundancy {
            Redundancy {
                online: self.0,
                total: self.0,
            }
        }
    }

    fn state(ctx: &ServiceContext, pid: &str, uuid: &str) -> LocalState {
        let conn = crate::api::db(ctx).unwrap().conn();
        frames_db::get(&conn, pid, uuid)
            .unwrap()
            .unwrap()
            .local_state
    }

    fn row(ctx: &ServiceContext, pid: &str, uuid: &str) -> LocalFrameRow {
        let conn = crate::api::db(ctx).unwrap().conn();
        frames_db::get(&conn, pid, uuid).unwrap().unwrap()
    }

    const AGG: Duration = crate::collab::storage::watch::AGGREGATE;
    const SETTLE: Duration = crate::collab::storage::watch::SETTLE;

    #[tokio::test]
    async fn a_touched_file_with_the_same_bytes_keeps_serving() {
        let rig = ts::landed_rig(1).await; // one held replica, landed and seeded
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::set_mtime(&path, 60);
        let mut eng = rig.engine();
        let ev = eng.local_check(&pid, &uuid).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held);
        assert!(ev.is_empty(), "{ev:?}");
        let recorded = Stamp::parse(
            row(&rig.ctx, &pid, &uuid)
                .size_mtime_seen
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert!(
            recorded.matches(&Stamp::of(&std::fs::metadata(&path).unwrap())),
            "the stamp is re-recorded"
        );
    }

    #[tokio::test]
    async fn an_edited_replica_is_quarantined_at_once_and_stops_serving() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::overwrite_same_size(&path);
        let mut eng = rig.engine();
        let ev = eng.local_check(&pid, &uuid).await;
        assert!(
            matches!(
                ev.as_slice(),
                [
                    StorageEvent::StateChanged {
                        to: LocalState::Quarantined,
                        ..
                    },
                    StorageEvent::Quarantined { .. }
                ]
            ),
            "{ev:?}"
        );
        assert!(path.exists(), "the edited file is never touched");
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab_live::list_quarantine(&conn, &pid)
                .unwrap()
                .len(),
            1
        );
        assert!(!frames_db::get(&conn, &pid, &uuid).unwrap().unwrap().on_disk);
        drop(conn);
        assert!(
            rig.node
                .project_frame_tags(&pid, &uuid)
                .await
                .unwrap()
                .is_empty(),
            "unseeded: it stopped serving"
        );
        // unchanged since the rejection: not rehashed, no second event
        assert!(eng.local_check(&pid, &uuid).await.is_empty());
    }

    #[tokio::test]
    async fn a_quarantined_file_whose_bytes_come_back_serves_again() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let original = std::fs::read(&path).unwrap();
        ts::overwrite_same_size(&path);
        let mut eng = rig.engine();
        eng.local_check(&pid, &uuid).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Quarantined);
        std::fs::write(&path, &original).unwrap();
        ts::set_mtime(&path, 120);
        let ev = eng.local_check(&pid, &uuid).await;
        assert!(
            matches!(
                ev.as_slice(),
                [StorageEvent::StateChanged {
                    from: LocalState::Quarantined,
                    to: LocalState::Held,
                    ..
                }]
            ),
            "{ev:?}"
        );
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert!(crate::db::collab_live::list_quarantine(&conn, &pid)
            .unwrap()
            .is_empty());
        drop(conn);
        assert_eq!(
            rig.node
                .project_frame_tags(&pid, &uuid)
                .await
                .unwrap()
                .len(),
            1,
            "seeded again"
        );
    }

    /// L5: nothing lands over a quarantined file, a new version included —
    /// the file stays untouched and the frame waits for the user.
    #[tokio::test]
    async fn a_new_version_waits_for_a_quarantined_file() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::overwrite_same_size(&path);
        let edited = std::fs::read(&path).unwrap();
        let mut eng = rig.engine();
        eng.local_check(&pid, &uuid).await;
        rig.hub.update_frame(&pid, &uuid, |f| {
            f.content_version = 2;
            f.blake3 = "e".repeat(64);
        });
        let v = rig.hub.frame(&pid, &uuid).unwrap();
        frames_db::upsert_from_manifest(&crate::api::db(&rig.ctx).unwrap().conn(), &pid, &v)
            .unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Quarantined);
        assert!(eng.sweep(&Holders(2)).await.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), edited, "never overwritten");
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        let q = crate::db::collab_live::list_quarantine(&conn, &pid).unwrap();
        assert_eq!(
            q[0].quarantined_version, 1,
            "listed as 'a new version is waiting'"
        );
    }

    #[tokio::test]
    async fn a_single_deletion_settles_then_refetches() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        std::fs::remove_file(&path).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        assert!(eng.tick(t0 + AGG, &Holders(2)).await.is_empty());
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held); // not concluded yet
        let ev = eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
        assert!(
            !ev.iter().any(|e| matches!(
                e,
                StorageEvent::FrameLost { .. } | StorageEvent::DeletionChoice { .. }
            )),
            "{ev:?}"
        );
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        let ops: Vec<_> = crate::db::collab_live::outbox(&conn, &pid)
            .unwrap()
            .into_iter()
            .map(|r| r.op)
            .collect();
        assert_eq!(
            ops.last(),
            Some(&crate::db::collab_live::ClaimOp::Remove),
            "the claim leaves through the outbox"
        );
        drop(conn);
        assert!(
            rig.node
                .project_frame_tags(&pid, &uuid)
                .await
                .unwrap()
                .is_empty(),
            "unseeded (P31)"
        );
    }

    #[tokio::test]
    async fn fifteen_deletions_raise_one_choice_and_a_last_copy_raises_lost_everywhere() {
        let rig = ts::landed_rig(15).await;
        let mut eng = rig.engine();
        let t0 = Instant::now();
        for (_, _, path) in &rig.frames {
            std::fs::remove_file(path).unwrap();
            eng.on_signal(FsSignal::Touched(path.clone()), t0);
        }
        eng.tick(t0 + AGG, &Holders(0)).await;
        let ev = eng.tick(t0 + AGG + SETTLE, &Holders(0)).await;
        let choices: Vec<_> = ev
            .iter()
            .filter(|e| matches!(e, StorageEvent::DeletionChoice { .. }))
            .collect();
        assert_eq!(choices.len(), 1);
        assert!(
            matches!(choices[0], StorageEvent::DeletionChoice { count: 15, .. }),
            "{choices:?}"
        );
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, StorageEvent::FrameLost { .. }))
                .count(),
            15
        );
        for (pid, uuid, _) in &rig.frames {
            assert_eq!(state(&rig.ctx, pid, uuid), LocalState::AwaitingChoice);
        }
        // the choice is reversible and non-blocking
        let (pid, _, _) = &rig.frames[0];
        let report =
            last_copy_report(&rig.ctx, &Holders(1), pid, &[rig.frames[0].1.clone()]).unwrap();
        assert!(report[0].at_risk && report[0].holders_total == 1);
        assert_eq!(
            resolve_deletions(&rig.ctx, pid, None, DeletionAction::StopKeeping).unwrap(),
            15
        );
        assert_eq!(keep_again(&rig.ctx, pid, None).unwrap(), 15);
        assert_eq!(state(&rig.ctx, pid, &rig.frames[3].1), LocalState::Wanted);
        assert!(!row(&rig.ctx, pid, &rig.frames[3].1).locally_declined);
    }

    /// L4: a frame deleted a second time within 24 h joins the choice even
    /// alone, so the app never fights a user who deletes one file at a time.
    #[tokio::test]
    async fn a_second_deletion_within_a_day_raises_the_choice() {
        let rig = ts::landed_rig(2).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let (_, uuid2, path2) = rig.frames[1].clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            let an_hour_ago = chrono::Utc::now().timestamp_millis() - 3_600_000;
            crate::db::collab_live::record_deletion(&conn, &pid, &uuid, an_hour_ago).unwrap();
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&path2).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        eng.on_signal(FsSignal::Touched(path2.clone()), t0);
        eng.tick(t0 + AGG, &Holders(3)).await;
        let ev = eng.tick(t0 + AGG + SETTLE, &Holders(3)).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::AwaitingChoice);
        assert_eq!(state(&rig.ctx, &pid, &uuid2), LocalState::Wanted);
        assert!(
            ev.contains(&StorageEvent::DeletionChoice {
                count: 1,
                project_ids: vec![pid.clone()]
            }),
            "{ev:?}"
        );
        assert_eq!(
            resolve_deletions(
                &rig.ctx,
                &pid,
                Some(&[uuid.clone()]),
                DeletionAction::Refetch
            )
            .unwrap(),
            1
        );
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
    }

    /// Task 8 carry: a removed folder settles as one path; every landing
    /// beneath it is gone with it.
    #[tokio::test]
    async fn a_removed_folder_takes_every_landing_beneath_it() {
        let rig = ts::landed_rig(3).await;
        let dir = rig.frames[0].2.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&dir).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(dir.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        for (pid, uuid, _) in &rig.frames {
            assert_eq!(state(&rig.ctx, pid, uuid), LocalState::Wanted);
        }
    }

    #[tokio::test]
    async fn a_moved_file_is_readopted_by_hash_without_a_deletion() {
        // > 16 KiB (imported by reference); the NEW path sorts first, so the
        // store's path union reads the live file at once — held, not parked.
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let moved = path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("a-renamed.fits");
        std::fs::rename(&path, &moved).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        eng.on_signal(FsSignal::Touched(moved.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        let r = row(&rig.ctx, &pid, &uuid);
        assert_eq!(r.local_state, LocalState::Held);
        assert_eq!(
            r.landed_path.as_deref(),
            Some(moved.to_string_lossy().as_ref())
        );
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert!(
            crate::db::collab_live::deletions_since(&conn, 0)
                .unwrap()
                .is_empty(),
            "no deletion recorded"
        );
        assert_eq!(
            crate::db::collab_live::outbox_len(&conn, &pid).unwrap(),
            1,
            "only the landing's add: a move changes no servability"
        );
        drop(conn);
        let hash: iroh_blobs::Hash = r.blake3.parse().unwrap();
        assert_eq!(
            rig.node.collab_blob_health(hash).await.unwrap(),
            crate::sharing::iroh::node::BlobHealth::Readable,
            "served from the new path"
        );
    }

    /// Poll until the collab store dropped `blake3`'s entry (a test GC gate
    /// is open), within 20 s.
    async fn wait_collected(node: &SharedIrohNode, blake3: &str) {
        let hash: iroh_blobs::Hash = blake3.parse().unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while node.collab_blob_health(hash).await.unwrap()
            != crate::sharing::iroh::node::BlobHealth::Missing
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "GC never dropped the entry"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn gc_gate() -> Arc<std::sync::atomic::AtomicBool> {
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::sharing::iroh::node::test_gc::arm(Some(Arc::clone(&gate)));
        gate
    }

    /// Task 8 carry + fix round 1: a changed folder is walked. Renamed so the
    /// OLD folder sorts first (`other` → `renamed-folder`), every > 16 KiB
    /// frame hits the dead entry and is parked; once the collab GC dropped
    /// the entries, the parked retry — one GC interval later — re-seeds
    /// every frame from its new path.
    #[tokio::test]
    async fn a_renamed_folder_is_walked_parked_and_readopted_after_the_gc() {
        use std::sync::atomic::Ordering;
        let gate = gc_gate();
        let rig = ts::landed_rig(2).await;
        crate::sharing::iroh::node::test_gc::arm(None);
        let dir = rig.frames[0].2.parent().unwrap().to_path_buf();
        let renamed = dir.parent().unwrap().join("renamed-folder");
        std::fs::rename(&dir, &renamed).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(dir.clone()), t0);
        eng.on_signal(FsSignal::Touched(renamed.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        for (pid, uuid, _) in &rig.frames {
            let r = row(&rig.ctx, pid, uuid);
            assert!(
                r.local_state == LocalState::Wanted && r.awaiting_gc,
                "parked: {r:?}"
            );
        }
        let retry_at = eng.next_parked_retry.expect("a parked retry is scheduled");
        assert!(eng.next_deadline() <= retry_at);
        assert!(retry_at >= t0 + AGG + PARKED_RETRY_AFTER);
        gate.store(true, Ordering::SeqCst);
        for (pid, uuid, _) in &rig.frames {
            wait_collected(&rig.node, &row(&rig.ctx, pid, uuid).blake3).await;
        }
        gate.store(false, Ordering::SeqCst);
        let ev = eng.tick(retry_at, &Holders(2)).await;
        assert!(
            ev.iter()
                .filter(|e| matches!(
                    e,
                    StorageEvent::StateChanged {
                        to: LocalState::Held,
                        ..
                    }
                ))
                .count()
                == 2,
            "{ev:?}"
        );
        assert!(eng.next_parked_retry.is_none());
        for (pid, uuid, path) in &rig.frames {
            let r = row(&rig.ctx, pid, uuid);
            assert_eq!(r.local_state, LocalState::Held);
            assert!(!r.awaiting_gc);
            let want = renamed.join(path.file_name().unwrap());
            assert_eq!(
                r.landed_path.as_deref(),
                Some(want.to_string_lossy().as_ref())
            );
        }
    }

    /// Wave-3 replacement for the retired wave-2
    /// `identical_content_second_frame_is_linked_not_fetched` (T11/T12/T18
    /// ruling, P24): a wanted frame whose exact bytes appear in the root is
    /// linked from that file — no transfer — and seeded.
    #[tokio::test]
    async fn an_identical_file_in_the_root_is_linked_not_fetched() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let bytes = std::fs::read(&path).unwrap();
        // a second frame, same content, wanted
        rig.hub.seed_frames(&pid, "acc-o", &["twin"], "published");
        let v0 = rig.hub.frame(&pid, &uuid).unwrap();
        rig.hub.update_frame(&pid, "twin", |f| {
            f.blake3 = v0.blake3.clone();
            f.xxh3 = v0.xxh3.clone();
            f.byte_size = v0.byte_size;
        });
        let v = rig.hub.frame(&pid, "twin").unwrap();
        frames_db::upsert_from_manifest(&crate::api::db(&rig.ctx).unwrap().conn(), &pid, &v)
            .unwrap();
        assert_eq!(state(&rig.ctx, &pid, "twin"), LocalState::Wanted);
        let copy = path.parent().unwrap().join("twin-copy.fits");
        std::fs::write(&copy, &bytes).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(copy.clone()), t0);
        let ev = eng.tick(t0 + AGG, &Holders(2)).await;
        assert!(
            ev.contains(&StorageEvent::StateChanged {
                project_id: pid.clone(),
                frame_uuid: "twin".into(),
                from: LocalState::Wanted,
                to: LocalState::Held
            }),
            "{ev:?}"
        );
        let r = row(&rig.ctx, &pid, "twin");
        assert_eq!(
            r.landed_path.as_deref(),
            Some(copy.to_string_lossy().as_ref())
        );
        assert_eq!(
            rig.node
                .project_frame_tags(&pid, "twin")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            state(&rig.ctx, &pid, &uuid),
            LocalState::Held,
            "the first frame is untouched"
        );
    }

    /// A moved file above the store's inline threshold whose OLD path sorts
    /// first: iroh-blobs keeps reading the dead path (external-path union)
    /// and the collab store never copy-repairs (P20), so the frame is
    /// recorded at its new path and parked — not served, not fetched — until
    /// the collab GC drops the dead entry; then the sweep re-adopts it.
    #[tokio::test]
    async fn a_moved_file_over_a_dead_store_entry_is_parked_then_readopted() {
        let rig = ts::landed_rig(0).await;
        let big: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        let old = rig.root.join("m31").join("a").join("big.fits");
        ts::land_frame(&rig.ctx, &rig.hub, &rig.node, "big", &old, &big).await;
        let moved = rig.root.join("m31").join("z-big.fits");
        std::fs::rename(&old, &moved).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(old.clone()), t0);
        eng.on_signal(FsSignal::Touched(moved.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        let r = row(&rig.ctx, ts::PID, "big");
        assert_eq!(
            r.landed_path.as_deref(),
            Some(moved.to_string_lossy().as_ref()),
            "recorded at the new path"
        );
        assert_eq!(r.local_state, LocalState::Wanted);
        assert!(
            r.awaiting_gc,
            "parked until the collab GC drops the dead entry"
        );
        assert!(rig
            .node
            .project_frame_tags(ts::PID, "big")
            .await
            .unwrap()
            .is_empty());
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        // the entry is still dead: a sweep retries and keeps it parked
        eng.sweep(&Holders(2)).await;
        let r = row(&rig.ctx, ts::PID, "big");
        assert!(r.local_state == LocalState::Wanted && r.awaiting_gc);
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert!(
            crate::db::collab_live::deletions_since(&conn, 0)
                .unwrap()
                .is_empty(),
            "never ruled a deletion"
        );
        let ops: Vec<_> = crate::db::collab_live::outbox(&conn, ts::PID)
            .unwrap()
            .into_iter()
            .map(|o| o.op)
            .collect();
        assert_eq!(
            ops,
            vec![
                crate::db::collab_live::ClaimOp::Add { content_version: 1 },
                crate::db::collab_live::ClaimOp::Remove
            ],
            "the landing's add, then one rm: a parked frame is not claimed"
        );
        drop(conn);

        // a new version while parked is fetched, never left parked
        rig.hub.update_frame(ts::PID, "big", |f| {
            f.content_version = 2;
            f.blake3 = "f".repeat(64);
        });
        let v = rig.hub.frame(ts::PID, "big").unwrap();
        frames_db::upsert_from_manifest(&crate::api::db(&rig.ctx).unwrap().conn(), ts::PID, &v)
            .unwrap();
        let r = row(&rig.ctx, ts::PID, "big");
        assert!(
            r.local_state == LocalState::Wanted && !r.awaiting_gc,
            "{r:?}"
        );
        let need =
            crate::api::collab_exchange::frame_need(&[r], &Default::default(), true, true, false);
        assert_eq!(need.len(), 1, "the new version is in the need set");
    }

    /// Task 11 (ledger ruling): a parked frame whose file was then edited is
    /// never released to a fetch that would land over the edit — spec §9.4
    /// "Held ──content changed──▶ Quarantined", I9, L5: it is quarantined,
    /// the file untouched, `awaiting_gc` cleared (so a later "Re-fetch
    /// original" fetches it).
    #[tokio::test]
    async fn a_parked_frame_whose_file_was_edited_is_quarantined_not_released() {
        let rig = ts::landed_rig(0).await;
        let big: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        let old = rig.root.join("m31").join("a").join("big.fits");
        ts::land_frame(&rig.ctx, &rig.hub, &rig.node, "big", &old, &big).await;
        let moved = rig.root.join("m31").join("z-big.fits");
        std::fs::rename(&old, &moved).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(old.clone()), t0);
        eng.on_signal(FsSignal::Touched(moved.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        let r = row(&rig.ctx, ts::PID, "big");
        assert!(
            r.local_state == LocalState::Wanted && r.awaiting_gc,
            "parked: {r:?}"
        );

        ts::overwrite_same_size(&moved);
        let edited = std::fs::read(&moved).unwrap();
        eng.sweep(&Holders(2)).await;
        let r = row(&rig.ctx, ts::PID, "big");
        assert_eq!(r.local_state, LocalState::Quarantined, "{r:?}");
        assert!(!r.awaiting_gc, "no longer parked");
        assert_eq!(
            std::fs::read(&moved).unwrap(),
            edited,
            "the edit is untouched"
        );
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab_live::list_quarantine(&conn, ts::PID)
                .unwrap()
                .len(),
            1,
            "listed under Changed files"
        );
        // Not `wanted`: the landing fence (`set_landed_if` + `fresh_row`,
        // P13) refuses any landing over it — see the landing tests.
    }

    /// C10 + fix round 1: a dead entry kept alive by an identical sibling's
    /// tag. The sibling reads through the same dead entry, so the retry
    /// parks it too; once the GC dropped the entry, both are re-seeded, each
    /// from its own path.
    #[tokio::test]
    async fn a_dead_entry_shared_with_an_identical_frame_parks_both_until_the_gc() {
        use std::sync::atomic::Ordering;
        let gate = gc_gate();
        let rig = ts::landed_rig(0).await;
        crate::sharing::iroh::node::test_gc::arm(None);
        let big: Vec<u8> = (0..40 * 1024u32).map(|i| (i % 241) as u8).collect();
        let a = rig.root.join("m31").join("a").join("x.fits");
        let b = rig.root.join("m31").join("b").join("y.fits");
        ts::land_frame(&rig.ctx, &rig.hub, &rig.node, "fa", &a, &big).await;
        ts::land_frame(&rig.ctx, &rig.hub, &rig.node, "fb", &b, &big).await;
        let moved = rig.root.join("m31").join("z.fits");
        std::fs::rename(&a, &moved).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(a.clone()), t0);
        eng.on_signal(FsSignal::Touched(moved.clone()), t0);
        // the start sweep's parked retry runs in the same tick as the move:
        // fa parks, its entry is dead and pinned by fb's tag, so fb parks too
        let ev = eng.tick(t0 + AGG, &Holders(2)).await;
        assert!(row(&rig.ctx, ts::PID, "fa").awaiting_gc, "fa parked");
        let fb = row(&rig.ctx, ts::PID, "fb");
        assert!(
            fb.local_state == LocalState::Wanted && fb.awaiting_gc,
            "the sibling is parked too: {ev:?}"
        );
        assert_eq!(
            fb.landed_path.as_deref(),
            Some(b.to_string_lossy().as_ref())
        );
        gate.store(true, Ordering::SeqCst);
        wait_collected(&rig.node, &fb.blake3).await;
        gate.store(false, Ordering::SeqCst);
        let retry_at = eng.next_parked_retry.expect("still parked, retried again");
        eng.tick(retry_at, &Holders(2)).await;
        let fa = row(&rig.ctx, ts::PID, "fa");
        let fb = row(&rig.ctx, ts::PID, "fb");
        assert_eq!(
            (fa.local_state, fb.local_state),
            (LocalState::Held, LocalState::Held)
        );
        assert_eq!(
            fa.landed_path.as_deref(),
            Some(moved.to_string_lossy().as_ref())
        );
        assert_eq!(
            fb.landed_path.as_deref(),
            Some(b.to_string_lossy().as_ref())
        );
        let hash: iroh_blobs::Hash = fa.blake3.parse().unwrap();
        assert_eq!(
            rig.node.collab_blob_health(hash).await.unwrap(),
            crate::sharing::iroh::node::BlobHealth::Readable
        );
    }

    /// Fix round 1: a move the watcher did not see (degraded) is re-adopted
    /// by the walk that runs before removals are ruled — no deletion choice,
    /// no "lost everywhere", no re-fetch.
    #[tokio::test]
    async fn a_folder_renamed_unseen_in_degraded_mode_is_readopted_not_deleted() {
        let rig = ts::landed_rig(12).await;
        let dir = rig.frames[0].2.parent().unwrap().to_path_buf();
        // the new name sorts first: re-seeded at once (the parked path has
        // its own tests)
        let renamed = dir.parent().unwrap().join("0-renamed");
        std::fs::rename(&dir, &renamed).unwrap();
        let mut eng = rig.engine();
        let (_quiet_tx, quiet_rx) = tokio::sync::mpsc::unbounded_channel();
        eng.fs_rx = quiet_rx; // the watcher sees nothing
        eng.network = true; // → degraded
        let t0 = Instant::now();
        let mut ev = eng.tick(t0 + AGG, &Holders(0)).await; // start sweep: all missing
        assert!(eng.degraded());
        ev.extend(eng.tick(t0 + AGG * 2, &Holders(0)).await);
        ev.extend(eng.tick(t0 + AGG * 2 + SETTLE, &Holders(0)).await);
        assert!(
            !ev.iter().any(|e| matches!(
                e,
                StorageEvent::DeletionChoice { .. } | StorageEvent::FrameLost { .. }
            )),
            "{ev:?}"
        );
        for (pid, uuid, path) in &rig.frames {
            let r = row(&rig.ctx, pid, uuid);
            assert_eq!(r.local_state, LocalState::Held, "{uuid}");
            let want = renamed.join(path.file_name().unwrap());
            assert_eq!(
                r.landed_path.as_deref(),
                Some(want.to_string_lossy().as_ref())
            );
        }
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert!(crate::db::collab_live::deletions_since(&conn, 0)
            .unwrap()
            .is_empty());
    }

    /// Fix round 1 (a): a re-included frame that was NOT seeded when it was
    /// excluded (it went missing, was re-fetch-ruled, then excluded; the
    /// file came back from the trash) is never held on a stat alone — it
    /// stays wanted, claims nothing, and the engine re-adopts it by hash.
    #[tokio::test]
    async fn a_reincluded_unseeded_frame_is_readopted_not_held_on_a_stat() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let bytes = std::fs::read(&path).unwrap();
        let stamp_before = row(&rig.ctx, &pid, &uuid).size_mtime_seen;
        std::fs::remove_file(&path).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
        let narrow = r#"{"filters":["Ha"]}"#;
        crate::db::collab::set_policy(&crate::api::db(&rig.ctx).unwrap().conn(), &pid, narrow)
            .unwrap();
        apply_policy(&rig.ctx, &pid).unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Idle);
        // restored from the trash, same bytes, same mtime as recorded
        std::fs::write(&path, &bytes).unwrap();
        if let Some(st) = stamp_before.as_deref().and_then(Stamp::parse) {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(st.mtime as u64))
                .unwrap();
        }
        let adds_before =
            crate::db::collab_live::my_claims(&crate::api::db(&rig.ctx).unwrap().conn(), &pid)
                .unwrap()
                .len();
        crate::db::collab::set_policy(&crate::api::db(&rig.ctx).unwrap().conn(), &pid, "{}")
            .unwrap();
        apply_policy(&rig.ctx, &pid).unwrap();
        assert_eq!(
            state(&rig.ctx, &pid, &uuid),
            LocalState::Wanted,
            "never held on a stat alone"
        );
        assert_eq!(
            crate::db::collab_live::my_claims(&crate::api::db(&rig.ctx).unwrap().conn(), &pid)
                .unwrap()
                .len(),
            adds_before,
            "claims nothing it cannot serve"
        );
        // the routed file: re-adopted by hash, seeded, held
        let t1 = t0 + AGG + SETTLE + Duration::from_secs(1);
        eng.tick(t1, &Holders(2)).await;
        eng.tick(t1 + AGG, &Holders(2)).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held);
        assert_eq!(
            rig.node
                .project_frame_tags(&pid, &uuid)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Fix round 1 (b), I9/L5: a held frame excluded (seeded, stamp kept),
    /// edited while idle, then re-included: not held, not re-fetched over —
    /// quarantined by the engine.
    #[tokio::test]
    async fn a_reincluded_frame_edited_while_idle_is_quarantined() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        let mut eng = rig.engine();
        crate::db::collab::set_policy(
            &crate::api::db(&rig.ctx).unwrap().conn(),
            &pid,
            r#"{"filters":["Ha"]}"#,
        )
        .unwrap();
        apply_policy(&rig.ctx, &pid).unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Idle);
        ts::overwrite_same_size(&path);
        let edited = std::fs::read(&path).unwrap();
        crate::db::collab::set_policy(&crate::api::db(&rig.ctx).unwrap().conn(), &pid, "{}")
            .unwrap();
        apply_policy(&rig.ctx, &pid).unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
        let t0 = Instant::now();
        let mut ev = eng.tick(t0, &Holders(2)).await;
        ev.extend(eng.tick(t0 + AGG, &Holders(2)).await);
        assert_eq!(
            state(&rig.ctx, &pid, &uuid),
            LocalState::Quarantined,
            "{ev:?}"
        );
        assert!(
            ev.iter()
                .any(|e| matches!(e, StorageEvent::Quarantined { .. })),
            "{ev:?}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            edited,
            "the edit is untouched"
        );
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab_live::list_quarantine(&conn, &pid)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn an_unknown_file_is_listed_as_foreign_and_never_touched() {
        let rig = ts::landed_rig(1).await;
        let stray = rig.root.join("m31").join("stray.fits");
        std::fs::write(&stray, b"not a project frame").unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(stray.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert!(
            frames_db::foreign_file_size_mtime(&conn, &stray.to_string_lossy())
                .unwrap()
                .is_some()
        );
        assert!(stray.exists());
    }

    #[tokio::test]
    async fn an_unavailable_store_changes_no_frame() {
        let rig = ts::landed_rig(3).await;
        std::fs::remove_file(rig.root.join(crate::collab::storage::marker::MARKER_REL)).unwrap();
        for (_, _, p) in &rig.frames {
            std::fs::remove_file(p).unwrap();
        }
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Root, t0);
        let ev = eng.sweep(&Holders(2)).await;
        assert!(!ev.is_empty(), "the unavailability is reported");
        assert!(
            ev.iter()
                .all(|e| matches!(e, StorageEvent::Availability(_))),
            "{ev:?}"
        );
        for (_, _, p) in &rig.frames {
            eng.on_signal(FsSignal::Touched(p.clone()), t0);
        }
        eng.tick(t0 + AGG, &Holders(2)).await;
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        for (pid, uuid, _) in &rig.frames {
            assert_eq!(
                state(&rig.ctx, pid, uuid),
                LocalState::Held,
                "unmounted is not deleted"
            );
        }
    }

    struct MoveToTrash(PathBuf);
    impl Trash for MoveToTrash {
        fn delete(&self, path: &Path) -> Result<(), String> {
            std::fs::create_dir_all(&self.0).map_err(|e| e.to_string())?;
            std::fs::rename(path, self.0.join(path.file_name().unwrap())).map_err(|e| e.to_string())
        }
    }
    struct NoTrash;
    impl Trash for NoTrash {
        fn delete(&self, _: &Path) -> Result<(), String> {
            Err("no trash on this volume".into())
        }
    }

    /// Wave-3 replacement for the retired wave-2
    /// `an_edited_replica_is_kept_beside_the_refetched_frame` (T11/T12/T18
    /// ruling): "Re-fetch original" keeps the edit — in the trash — and the
    /// frame is wanted again; a missing trash asks, then deletes on
    /// confirmation.
    #[tokio::test]
    async fn refetch_original_sends_the_edit_to_the_trash_or_asks() {
        let rig = ts::landed_rig(2).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::overwrite_same_size(&path);
        let edited = std::fs::read(&path).unwrap();
        let mut eng = rig.engine();
        eng.local_check(&pid, &uuid).await;
        let trash_dir = rig.root.parent().unwrap().join("Trash");
        let out = resolve_changed_file_with(
            &rig.ctx,
            &rig.node,
            &pid,
            &uuid,
            ChangedAction::RefetchOriginal,
            false,
            Arc::new(MoveToTrash(trash_dir.clone())),
        )
        .await
        .unwrap();
        assert!(out.trashed && !path.exists());
        assert_eq!(
            std::fs::read(trash_dir.join(path.file_name().unwrap())).unwrap(),
            edited,
            "the edit is kept"
        );
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
        assert!(crate::db::collab_live::list_quarantine(
            &crate::api::db(&rig.ctx).unwrap().conn(),
            &pid
        )
        .unwrap()
        .is_empty());

        // no trash: the call asks first, then deletes on confirmation
        let (_, uuid2, path2) = rig.frames[1].clone();
        ts::overwrite_same_size(&path2);
        eng.local_check(&pid, &uuid2).await;
        let err = resolve_changed_file_with(
            &rig.ctx,
            &rig.node,
            &pid,
            &uuid2,
            ChangedAction::RefetchOriginal,
            false,
            Arc::new(NoTrash),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, ApiError::Conflict(m) if m.starts_with(TRASH_UNAVAILABLE)),
            "{err}"
        );
        assert!(
            path2.exists() && state(&rig.ctx, &pid, &uuid2) == LocalState::Quarantined,
            "nothing deleted silently"
        );
        let out = resolve_changed_file_with(
            &rig.ctx,
            &rig.node,
            &pid,
            &uuid2,
            ChangedAction::RefetchOriginal,
            true,
            Arc::new(NoTrash),
        )
        .await
        .unwrap();
        assert!(!out.trashed && !path2.exists());
        assert_eq!(state(&rig.ctx, &pid, &uuid2), LocalState::Wanted);
    }

    #[tokio::test]
    async fn refetch_original_trashes_or_asks_and_delete_needs_confirmation() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        ts::overwrite_same_size(&path);
        let mut eng = rig.engine();
        eng.local_check(&pid, &uuid).await;
        let err = resolve_changed_file(
            &rig.ctx,
            &rig.node,
            &pid,
            &uuid,
            ChangedAction::Delete,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("confirm"), "{err}");
        assert!(path.exists());
        let out = resolve_changed_file(
            &rig.ctx,
            &rig.node,
            &pid,
            &uuid,
            ChangedAction::Delete,
            true,
        )
        .await
        .unwrap();
        assert!(!out.trashed && !path.exists());
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::NotKept);
        // reversible (L6)
        assert_eq!(
            keep_again(&rig.ctx, &pid, Some(&[uuid.clone()])).unwrap(),
            1
        );
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Wanted);
    }

    #[tokio::test]
    async fn an_own_frame_gone_is_own_missing_and_never_ruled() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, path) = rig.frames[0].clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute("UPDATE project_frames_local SET origin = 'own', local_state = 'own_held' WHERE frame_uuid = ?1", [&uuid]).unwrap();
        }
        std::fs::remove_file(&path).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        eng.on_signal(FsSignal::Touched(path.clone()), t0);
        eng.tick(t0 + AGG, &Holders(2)).await;
        eng.tick(t0 + AGG + SETTLE, &Holders(2)).await;
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::OwnMissing);
        let conn = crate::api::db(&rig.ctx).unwrap().conn();
        assert!(crate::db::collab_live::deletions_since(&conn, 0)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn one_watch_error_is_transient_and_a_run_of_them_degrades() {
        let rig = ts::landed_rig(1).await;
        let mut eng = rig.engine();
        if eng.degraded() {
            eprintln!("no filesystem watcher on this platform; skipping");
            return;
        }
        // Only the injected signals below: real watcher events (the canary
        // write's own) would reset the error run nondeterministically.
        let (_quiet_tx, quiet_rx) = tokio::sync::mpsc::unbounded_channel();
        eng.fs_rx = quiet_rx;
        let t0 = Instant::now();
        eng.on_signal(FsSignal::WatchError("blip".into()), t0);
        let ev = eng.tick(t0, &Holders(2)).await;
        assert!(!ev.contains(&StorageEvent::WatcherDegraded(true)) && !eng.degraded());
        for _ in 0..WATCH_ERRORS_DEGRADE {
            eng.on_signal(FsSignal::WatchError("down".into()), t0);
        }
        let ev = eng.tick(t0 + Duration::from_secs(1), &Holders(2)).await;
        assert!(
            ev.contains(&StorageEvent::WatcherDegraded(true)) && eng.degraded(),
            "{ev:?}"
        );
        eng.on_signal(FsSignal::Canary, t0 + Duration::from_secs(2));
        let ev = eng.tick(t0 + Duration::from_secs(2), &Holders(2)).await;
        assert!(ev.contains(&StorageEvent::WatcherDegraded(false)), "{ev:?}");
    }

    /// Task 8 carry: a canary write a read-only root refuses is not a dead
    /// watcher.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_root_never_declares_the_watcher_dead() {
        use std::os::unix::fs::PermissionsExt;
        let rig = ts::landed_rig(1).await;
        let athenaeum = rig.root.join(".athenaeum");
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut eng = rig.engine();
        let t0 = Instant::now();
        let mut all = eng.tick(t0, &Holders(2)).await;
        all.extend(eng.tick(t0 + watch::CANARY_DEADLINE * 2, &Holders(2)).await);
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !all.contains(&StorageEvent::WatcherDegraded(true)),
            "{all:?}"
        );
        assert_eq!(
            state(&rig.ctx, &rig.frames[0].0, &rig.frames[0].1),
            LocalState::Held,
            "read-only still serves"
        );
    }

    #[tokio::test]
    async fn setting_the_policy_or_the_toggle_applies_the_scope() {
        let rig = ts::landed_rig(1).await;
        let (pid, uuid, _) = rig.frames[0].clone();
        let narrow = crate::api::collab_exchange::ReplicationPolicy {
            filters: vec!["Ha".into()],
            ..Default::default()
        };
        crate::api::collab_exchange::set_collab_policy(&rig.ctx, &pid, narrow)
            .await
            .unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Idle);
        crate::api::collab_exchange::set_collab_policy(&rig.ctx, &pid, Default::default())
            .await
            .unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held);
        // auto-replicate off changes no state (P10)
        crate::api::collab_exchange::set_project_auto_replicate(&rig.ctx, &pid, false).unwrap();
        assert_eq!(state(&rig.ctx, &pid, &uuid), LocalState::Held);
    }

    #[tokio::test]
    async fn apply_policy_idles_what_it_drops_and_reincludes_by_stat() {
        let rig = ts::landed_rig(2).await;
        let (pid, held_uuid, _) = rig.frames[0].clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET filter_canonical = 'Ha' WHERE frame_uuid = ?1",
                [&rig.frames[1].1],
            )
            .unwrap();
            crate::db::collab::set_policy(&conn, &pid, r#"{"filters":["Ha"]}"#).unwrap();
        }
        assert_eq!(apply_policy(&rig.ctx, &pid).unwrap(), 1);
        assert_eq!(state(&rig.ctx, &pid, &held_uuid), LocalState::Idle);
        assert_eq!(state(&rig.ctx, &pid, &rig.frames[1].1), LocalState::Held);
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            crate::db::collab::set_policy(&conn, &pid, r#"{}"#).unwrap();
        }
        assert_eq!(apply_policy(&rig.ctx, &pid).unwrap(), 1);
        assert_eq!(
            state(&rig.ctx, &pid, &held_uuid),
            LocalState::Held,
            "the file never moved: held again"
        );
    }
}
