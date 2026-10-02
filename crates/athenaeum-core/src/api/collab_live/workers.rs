//! The live runtime's two off-loop workers (Task 15 fix round 1, C1; owner
//! rule L1, spec §8: the collab lane never waits on hub HTTP or a sweep).
//!
//! - The **feed worker** owns the feed applier and the holder side. It
//!   applies the stream's events one at a time, in stream order (the
//!   applier's cursor and epoch checks rely on that order), and runs the
//!   holder side's timers — the outbox flushes and the hourly digest checks —
//!   between them. Its `Background` retries may wait for minutes; only this
//!   task waits. Effects go back to the loop over a channel, after the
//!   shared holder maps and presence copy already show them.
//! - The **storage task** owns the storage engine: watcher signals, the
//!   periodic ticks and sweeps, the serve oracle's local checks and Sync
//!   now's sweep. Its events go back to the loop the same way.
//!
//! The loop's own arms stay short: executor results, lane grants, pool
//! events, the yield signal, commands and stop are serviced within
//! milliseconds whatever these workers are doing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::api::collab_live::feed::{FeedApplier, FeedEffect, SyncChanges, SyncReport};
use crate::api::collab_live::holdings::{HolderMaps, Holdings};
use crate::api::collab_live::runtime::Shared;
use crate::api::collab_live::storage_task::{HolderView, StorageEngine, StorageEvent};
use crate::api::ApiError;
use crate::collab::hub_client::CollabClient;
use crate::collab::live::wire::{ChangeKind, LiveEvent, ProjectEvent};
use crate::services::ServiceContext;

// ── the feed worker ─────────────────────────────────────────────────────

/// Work for the feed worker, applied in the order it was sent.
pub(crate) enum FeedWork {
    /// The account's hub credentials (again when they changed).
    Creds(Option<(String, String)>),
    Event(LiveEvent),
    /// A claim change was appended for this project: its flush wait starts.
    NoteAppend(String, Instant),
    /// Sync now (spec 2026-10-01 §6.3): a digest check per project, run
    /// right after the next `hello` is applied and its reports sent — the
    /// confirmation never waits behind one hub call per project.
    DigestAfterHello(Vec<String>),
    /// The publishing binding moved to this device: the re-announce check
    /// (final fix A-I1 — a command hands it here and returns at once; only
    /// this worker waits on its `Background` retries).
    Rebind(String),
    /// Test only (final fix A-I3): the worker panics.
    #[cfg(test)]
    Panic,
}

/// What the feed worker hands back to the loop, in order.
pub(crate) enum FeedOut {
    Effects(Vec<FeedEffect>),
    /// A `hello` was applied: the projects refused in the last session are
    /// asked again.
    NewSession,
    /// The hub refused this project (403, spec §4.6).
    Refused {
        project_id: String,
        error: String,
    },
    /// One project confirmed against the hub, or not (spec 2026-10-01
    /// §6.1): the applier's reports after each event, and a not-ok report
    /// after every refusal.
    Synced(SyncReport),
}

pub(crate) struct FeedWorker {
    ctx: Arc<ServiceContext>,
    shared: Arc<Shared>,
    me: String,
    maps: HolderMaps,
    feed: Option<FeedApplier>,
    holdings: Option<Holdings>,
    creds: Option<(String, String)>,
    out: mpsc::UnboundedSender<FeedOut>,
    /// Events sent and not applied yet (the loop's back-pressure on the
    /// stream).
    pending: Arc<AtomicUsize>,
    /// Sync now's digest checks, owed after the next `hello`.
    digest_after_hello: Option<Vec<String>>,
    /// Work drained from the channel while looking for events to fold; it is
    /// served, in order, before the channel is read again (plan P7).
    carry: VecDeque<FeedWork>,
}

/// Plan P7 (wave 6 §11.2): a `project` event that will pull the manifest
/// (frames changed and not inlined).
pub(crate) fn needs_pull(ev: &ProjectEvent) -> bool {
    ev.kinds.contains(&ChangeKind::Frames) && (ev.more || ev.frames.is_none())
}

/// Fold `next` into `into` when it is the same project, contiguous, and
/// changes frames only: the pull `into` forces reads from the stored cursor
/// to the head, so it covers `next`'s rows whether inlined or not.
pub(crate) fn absorb(into: &mut ProjectEvent, next: &ProjectEvent) -> bool {
    let frames_only = !next.kinds.is_empty() && next.kinds.iter().all(|k| *k == ChangeKind::Frames);
    if next.project_id != into.project_id || next.prev != into.version || !frames_only {
        return false;
    }
    into.version = next.version;
    into.more = true;
    into.frames = None;
    true
}

/// Take `first` and, from the front of `queue`, every work item `absorb`
/// accepts; stop at the first one it refuses (left in place, in order).
/// Returns the merged event and how many queued events it absorbed.
pub(crate) fn coalesce(
    mut first: ProjectEvent,
    queue: &mut VecDeque<FeedWork>,
) -> (ProjectEvent, usize) {
    let mut n = 0;
    while let Some(FeedWork::Event(LiveEvent::Project(next))) = queue.front() {
        if !absorb(&mut first, next) {
            break;
        }
        queue.pop_front();
        n += 1;
    }
    (first, n)
}

impl FeedWorker {
    pub(crate) fn new(
        shared: Arc<Shared>,
        me: String,
        maps: HolderMaps,
        out: mpsc::UnboundedSender<FeedOut>,
        pending: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            ctx: Arc::clone(&shared.ctx),
            shared,
            me,
            maps,
            feed: None,
            holdings: None,
            creds: None,
            out,
            pending,
            digest_after_hello: None,
            carry: VecDeque::new(),
        }
    }

    pub(crate) async fn run(mut self, mut work: mpsc::UnboundedReceiver<FeedWork>) {
        loop {
            let due = self.holdings.as_ref().and_then(Holdings::next_deadline);
            // A due timer runs before more work, whatever is queued.
            if due.is_some_and(|d| Instant::now() >= d) {
                self.timers().await;
            }
            // Work drained while folding a burst is served before the channel.
            if let Some(w) = self.carry.pop_front() {
                self.dispatch(w, &mut work).await;
                continue;
            }
            let due = self.holdings.as_ref().and_then(Holdings::next_deadline);
            let sleep = async move {
                match due {
                    Some(d) => tokio::time::sleep_until(d.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                w = work.recv() => match w {
                    None => return,
                    Some(w) => self.dispatch(w, &mut work).await,
                },
                _ = sleep => self.timers().await,
            }
        }
    }

    /// Handle one work item; a pull-needing `project` event first drains
    /// what is already queued and folds the contiguous run behind it into
    /// one pull. `pending` drops once per ORIGINAL event, absorbed ones
    /// included.
    async fn dispatch(&mut self, w: FeedWork, work: &mut mpsc::UnboundedReceiver<FeedWork>) {
        match w {
            FeedWork::Event(LiveEvent::Project(ev)) if needs_pull(&ev) => {
                while let Ok(w) = work.try_recv() {
                    self.carry.push_back(w);
                }
                let (merged, absorbed) = coalesce(ev, &mut self.carry);
                if absorbed > 0 {
                    tracing::debug!(
                        project_id = %merged.project_id,
                        merged = absorbed,
                        version = merged.version,
                        "feed: pull-needing events merged"
                    );
                    self.pending.fetch_sub(absorbed, Ordering::SeqCst);
                }
                self.handle(FeedWork::Event(LiveEvent::Project(merged)))
                    .await;
            }
            w => self.handle(w).await,
        }
    }

    async fn handle(&mut self, w: FeedWork) {
        match w {
            FeedWork::Creds(creds) => self.ensure_feed(creds),
            FeedWork::Event(ev) => {
                self.on_event(ev).await;
                self.pending.fetch_sub(1, Ordering::SeqCst);
            }
            FeedWork::NoteAppend(p, at) => {
                if let Some(h) = self.holdings.as_mut() {
                    h.note_append(&p, at);
                }
            }
            FeedWork::Rebind(project_id) => {
                crate::api::collab_live::feed::run_rebind_check(
                    &self.ctx,
                    &project_id,
                    crate::collab::hub_client::RetryPolicy::Background,
                )
                .await;
            }
            #[cfg(test)]
            FeedWork::Panic => panic!("injected feed worker panic (test hook)"),
            FeedWork::DigestAfterHello(projects) => {
                tracing::debug!(
                    count = projects.len(),
                    "sync now: claim digest checks owed after the next hello"
                );
                self.digest_after_hello = Some(projects);
            }
        }
    }

    /// Sync now's claim digest check per project (its report back-offs
    /// forgotten first, L10).
    async fn digest(&mut self, projects: &[String]) {
        let Some(h) = self.holdings.as_mut() else {
            tracing::debug!("sync now: no holder side loaded yet; the digest checks are skipped");
            return;
        };
        h.clear_backoffs();
        for p in projects {
            if let Err(e) = h.digest_check(p).await {
                tracing::warn!(project_id = %p, error = %e, "sync now: claim digest check failed; retried on the next flush");
            }
        }
    }

    fn send(&self, out: FeedOut) {
        if self.out.send(out).is_err() {
            tracing::debug!("the live runtime is gone; a feed result dropped");
        }
    }

    /// Build the feed applier and the holder side for `creds` (again when
    /// the account's token changed).
    fn ensure_feed(&mut self, creds: Option<(String, String)>) {
        let Some(creds) = creds else {
            return;
        };
        if self.creds.as_ref() == Some(&creds) && self.feed.is_some() {
            return;
        }
        let (hub, token) = creds.clone();
        let clients = CollabClient::new(hub.clone()).and_then(|a| Ok((a, CollabClient::new(hub)?)));
        let (feed_client, holdings_client) = match clients {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "hub client could not be built; the feed waits");
                return;
            }
        };
        match Holdings::load(
            Arc::clone(&self.ctx),
            holdings_client,
            token.clone(),
            self.me.clone(),
        ) {
            Ok(mut h) => {
                h.share_into(&self.maps);
                self.holdings = Some(h);
            }
            Err(e) => {
                tracing::error!(error = %e, "holder maps could not be loaded; the feed waits");
                return;
            }
        }
        self.feed = Some(FeedApplier::new(
            Arc::clone(&self.ctx),
            feed_client,
            token,
            self.shared.emitter(),
        ));
        self.creds = Some(creds);
    }

    async fn on_event(&mut self, ev: LiveEvent) {
        let project = match &ev {
            LiveEvent::Project(p) => Some(p.project_id.clone()),
            LiveEvent::Holders(h) => Some(h.project_id.clone()),
            LiveEvent::Resync(r) => Some(r.project_id.clone()),
            LiveEvent::Account(a) => Some(a.project_id.clone()),
            _ => None,
        };
        let hello = matches!(ev, LiveEvent::Hello(_));
        if hello {
            self.ensure_feed(self.shared.credentials());
            // In order with the refusals of the events before it.
            self.send(FeedOut::NewSession);
        }
        let (Some(feed), Some(holdings)) = (self.feed.as_mut(), self.holdings.as_mut()) else {
            tracing::debug!(
                "feed event before the account was loaded; skipped (the next hello catches up)"
            );
            return;
        };
        let applied = feed.apply(ev, holdings).await;
        // Taken now: `feed` borrows `self.feed` into the arms below, and the
        // reports go out after them (spec 2026-10-01 §6.1).
        let mut reports = feed.take_reports();
        // The loop derives providers from this copy: it is current before
        // the effects reach it.
        self.shared.set_presence(&feed.presence);
        match applied {
            Ok(effects) => {
                let now = Instant::now();
                for e in &effects {
                    // A manifest apply may have queued claim changes: they
                    // flush after the usual delay.
                    if let FeedEffect::NeedSetChanged(p) | FeedEffect::ProjectJoined(p) = e {
                        holdings.note_append(p, now);
                    }
                }
                if !effects.is_empty() {
                    self.send(FeedOut::Effects(effects));
                }
            }
            Err(ApiError::Conflict(m)) if m == "epoch_changed" => {
                tracing::warn!("the hub's epoch changed under the session; reconnecting to reload");
                // Controller ruling R6: no report for this event — the
                // reconnect's `hello` reports every project, and a not-ok
                // here would end a Sync now's wait just before that
                // confirmation.
                if !reports.is_empty() {
                    tracing::debug!(
                        count = reports.len(),
                        "sync reports of an epoch-changed event dropped; the reconnect's hello reports"
                    );
                    reports.clear();
                }
                self.shared.reconnect_now();
            }
            Err(ApiError::Forbidden(e)) => match project {
                Some(project_id) => {
                    // The refusal is a not-ok report for its project too,
                    // and the only one: it folds into the applier's own
                    // report of this event, which keeps what was applied
                    // before the refusal.
                    let refused = refused_report(&project_id, &e);
                    match reports.iter_mut().find(|r| r.project_id == project_id) {
                        Some(r) => r.merge(refused),
                        None => reports.push(refused),
                    }
                    self.send(FeedOut::Refused {
                        project_id,
                        error: e,
                    });
                }
                None => tracing::error!(error = %e, "feed event refused by the hub"),
            },
            Err(e) => {
                tracing::warn!(error = %e, "feed event could not be applied; the next event or the versions vector catches up")
            }
        }
        for r in reports {
            self.send(FeedOut::Synced(r));
        }
        // Sync now's digest checks run after the hello's reports are out
        // (spec 2026-10-01 §6.3).
        if hello {
            if let Some(projects) = self.digest_after_hello.take() {
                self.digest(&projects).await;
            }
        }
    }

    /// The holder side's due work (Task 15 R1: every wake runs the due
    /// flushes AND the hourly check).
    async fn timers(&mut self) {
        let Some(h) = self.holdings.as_mut() else {
            return;
        };
        let now = Instant::now();
        let mut refused = Vec::new();
        for (pid, res) in h.flush_due(now).await {
            if let Err(ApiError::Forbidden(e)) = res {
                refused.push((pid, e));
            }
        }
        h.hourly_digest_checks(now).await;
        for (project_id, error) in refused {
            let report = refused_report(&project_id, &error);
            self.send(FeedOut::Refused { project_id, error });
            self.send(FeedOut::Synced(report));
        }
    }
}

/// The not-ok confirmation report a hub refusal (403) of `project_id` is.
fn refused_report(project_id: &str, error: &str) -> SyncReport {
    SyncReport {
        project_id: project_id.to_string(),
        ok: false,
        error: Some(error.to_string()),
        changes: SyncChanges::default(),
    }
}

// ── the storage task ────────────────────────────────────────────────────

/// Work for the storage task.
pub(crate) enum StorageWork {
    /// The serve oracle refused a serve, or a landing found the store
    /// unavailable: check this frame now (§9.3).
    Check(String, String),
    /// Sync now's stat sweep.
    Sweep,
    /// Test only (final fix A-I3): the task panics.
    #[cfg(test)]
    Panic,
}

/// A batch of storage events, with the watcher's state after it.
pub(crate) struct StorageOut {
    pub events: Vec<StorageEvent>,
    pub degraded: bool,
    pub network: bool,
}

pub(crate) async fn run_storage(
    mut engine: StorageEngine,
    holders: Arc<dyn HolderView>,
    mut work: mpsc::UnboundedReceiver<StorageWork>,
    out: mpsc::UnboundedSender<StorageOut>,
) {
    let mut fs_open = true;
    let mut last = (engine.degraded(), engine.network());
    loop {
        let events = {
            // A due tick runs before more work, whatever is queued.
            if Instant::now() >= engine.next_deadline() {
                engine.tick(Instant::now(), holders.as_ref()).await
            } else {
                let deadline = engine.next_deadline();
                tokio::select! {
                    biased;
                    w = work.recv() => match w {
                        None => return,
                        Some(StorageWork::Check(p, u)) => engine.local_check(&p, &u).await,
                        Some(StorageWork::Sweep) => engine.sweep(holders.as_ref()).await,
                        #[cfg(test)]
                        Some(StorageWork::Panic) => panic!("injected storage task panic (test hook)"),
                    },
                    sig = engine.recv_signal(), if fs_open => {
                        match sig {
                            Some(sig) => engine.on_signal(sig, Instant::now()),
                            None => {
                                tracing::warn!("collaboration folder signals ended; changes are seen by the periodic check only");
                                fs_open = false;
                            }
                        }
                        Vec::new()
                    }
                    _ = tokio::time::sleep_until(deadline.into()) => {
                        engine.tick(Instant::now(), holders.as_ref()).await
                    }
                }
            }
        };
        let now = (engine.degraded(), engine.network());
        if events.is_empty() && now == last {
            continue;
        }
        last = now;
        if out
            .send(StorageOut {
                events,
                degraded: now.0,
                network: now.1,
            })
            .is_err()
        {
            tracing::debug!("the live runtime is gone; the storage task ends");
            return;
        }
    }
}

/// The holder view the storage task rules deletions with: the shared live
/// holder maps and presence copy.
pub(crate) struct SharedHolders {
    pub ctx: Arc<ServiceContext>,
    pub maps: HolderMaps,
    pub shared: Arc<Shared>,
    pub me: String,
}

impl HolderView for SharedHolders {
    fn other_holders(
        &self,
        project_id: &str,
        frame_uuid: &str,
    ) -> crate::collab::live::holders::Redundancy {
        let read = crate::api::db(&self.ctx).and_then(|d| {
            let conn = d.conn();
            Ok((
                crate::db::collab_frames::get(&conn, project_id, frame_uuid)?,
                crate::db::collab::get_project(&conn, project_id)?,
            ))
        });
        match read {
            Ok((Some(row), Some(project))) => {
                let presence = self.shared.presence_copy();
                let maps = match self.maps.read() {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!(project_id, frame_uuid, error = %e, "holder maps lock poisoned; holders counted none");
                        return Default::default();
                    }
                };
                match maps.get(project_id) {
                    Some(map) => crate::api::collab_live::runtime::redundancy_of(
                        map, &presence, &project, &self.me, &row,
                    ),
                    None => Default::default(),
                }
            }
            Ok(_) => Default::default(),
            Err(e) => {
                tracing::warn!(project_id, frame_uuid, error = %e, "holders could not be read; counted none");
                Default::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::FakeHub;
    use crate::collab::live::wire::{HelloEvent, ResyncEvent, ResyncWhat};

    fn drain(rx: &mut mpsc::UnboundedReceiver<FeedOut>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            out.push(match o {
                FeedOut::Effects(_) => "effects".to_string(),
                FeedOut::NewSession => "new_session".to_string(),
                FeedOut::Refused { project_id, .. } => format!("refused {project_id}"),
                FeedOut::Synced(r) => format!(
                    "synced {} ok={} error={}",
                    r.project_id,
                    r.ok,
                    r.error.is_some()
                ),
            });
        }
        out
    }

    /// Spec 2026-10-01 §6.1: the feed worker forwards the applier's
    /// per-project reports — one per project after a `hello` — and a 403 on
    /// an event is forwarded as the refusal AND as exactly one not-ok report
    /// for that project, after the refusal.
    #[tokio::test]
    async fn the_worker_forwards_reports_and_one_not_ok_report_after_a_refusal() {
        let (_tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let ctx = Arc::new(ctx);
        let hub = FakeHub::start().await;
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.add_account("tok", "acc-me", "Me", &me, None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", false)], false);
        crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), "tok");
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        let shared = Arc::new(Shared::new_for_test(Arc::clone(&ctx)));
        shared.set_credentials(Some((hub.uri().to_string(), "tok".to_string())));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut worker = FeedWorker::new(
            shared,
            me,
            HolderMaps::default(),
            tx,
            Arc::new(AtomicUsize::new(0)),
        );

        let hello: HelloEvent = serde_json::from_value(hub.hello_for("tok")).unwrap();
        worker.on_event(LiveEvent::Hello(hello)).await;
        let outs = drain(&mut rx);
        assert!(
            outs.contains(&"synced p1 ok=true error=false".to_string()),
            "{outs:?}"
        );

        hub.remove_member("p1", "acc-me");
        worker
            .on_event(LiveEvent::Resync(ResyncEvent {
                project_id: "p1".into(),
                what: ResyncWhat::Project,
            }))
            .await;
        let outs = drain(&mut rx);
        let refused = outs
            .iter()
            .position(|o| o == "refused p1")
            .unwrap_or_else(|| panic!("the refusal is forwarded: {outs:?}"));
        let synced: Vec<usize> = outs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.starts_with("synced p1"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(synced.len(), 1, "exactly one report for p1: {outs:?}");
        assert_eq!(outs[synced[0]], "synced p1 ok=false error=true", "{outs:?}");
        assert!(
            synced[0] > refused,
            "the report follows the refusal: {outs:?}"
        );
    }

    /// A worker on a fake hub with project `p1`, its shared state, its
    /// output channel and the hub's first `hello` already applied (and
    /// drained).
    async fn worker_after_hello() -> (
        tempfile::TempDir,
        FakeHub,
        Arc<Shared>,
        FeedWorker,
        mpsc::UnboundedReceiver<FeedOut>,
    ) {
        let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let ctx = Arc::new(ctx);
        let hub = FakeHub::start().await;
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.add_account("tok", "acc-me", "Me", &me, None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", false)], false);
        crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), "tok");
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        let shared = Arc::new(Shared::new_for_test(Arc::clone(&ctx)));
        shared.set_credentials(Some((hub.uri().to_string(), "tok".to_string())));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut worker = FeedWorker::new(
            Arc::clone(&shared),
            me,
            HolderMaps::default(),
            tx,
            Arc::new(AtomicUsize::new(0)),
        );
        let hello: HelloEvent = serde_json::from_value(hub.hello_for("tok")).unwrap();
        worker.on_event(LiveEvent::Hello(hello)).await;
        let outs = drain(&mut rx);
        assert!(
            outs.contains(&"synced p1 ok=true error=false".to_string()),
            "{outs:?}"
        );
        (tmp, hub, shared, worker, rx)
    }

    /// Controller ruling R6: a hub epoch change seen under the session (a
    /// holder catch-up answered `410 epoch_changed`) reconnects and reports
    /// nothing — the reconnect's `hello` reports every project, and a not-ok
    /// here would stop the pill just before that confirmation.
    #[tokio::test]
    async fn an_epoch_change_under_the_session_reconnects_without_a_report() {
        let (_tmp, hub, shared, mut worker, mut rx) = worker_after_hello().await;
        let reconnect = shared.reconnect_signal();
        hub.rotate_epoch();
        worker
            .on_event(LiveEvent::Resync(ResyncEvent {
                project_id: "p1".into(),
                what: ResyncWhat::Holders,
            }))
            .await;
        let outs = drain(&mut rx);
        assert!(
            !outs.iter().any(|o| o.starts_with("synced")),
            "no report for an epoch change: {outs:?}"
        );
        assert!(
            reconnect.has_changed().unwrap(),
            "the epoch change reconnects"
        );
    }

    /// Spec 2026-10-01 §6.3: Sync now's claim digest waits for the next
    /// `hello` — nothing reaches the hub before it — and runs once, after
    /// it, with the hello's reports already sent.
    #[tokio::test]
    async fn the_sync_now_digest_runs_once_after_the_next_hello() {
        let (_tmp, hub, _shared, mut worker, mut rx) = worker_after_hello().await;
        let digests = || hub.requests_to("/projects/p1/holders/self");
        let before = digests().await;
        worker
            .handle(FeedWork::DigestAfterHello(vec!["p1".to_string()]))
            .await;
        assert_eq!(digests().await, before, "nothing before the hello");
        assert!(drain(&mut rx).is_empty());

        let hello: HelloEvent = serde_json::from_value(hub.hello_for("tok")).unwrap();
        worker.on_event(LiveEvent::Hello(hello)).await;
        assert_eq!(
            digests().await,
            before + 1,
            "the digest ran after the hello"
        );
        let outs = drain(&mut rx);
        assert!(
            outs.contains(&"synced p1 ok=true error=false".to_string()),
            "{outs:?}"
        );

        let hello: HelloEvent = serde_json::from_value(hub.hello_for("tok")).unwrap();
        worker.on_event(LiveEvent::Hello(hello)).await;
        assert_eq!(digests().await, before + 1, "once, not on every hello");
    }

    fn pev(pid: &str, prev: i64, version: i64, kinds: &[ChangeKind], more: bool) -> ProjectEvent {
        ProjectEvent {
            project_id: pid.into(),
            prev,
            version,
            kinds: kinds.to_vec(),
            frames: None,
            more,
        }
    }

    fn q_event(e: ProjectEvent) -> FeedWork {
        FeedWork::Event(LiveEvent::Project(e))
    }

    #[test]
    fn a_contiguous_frames_only_run_is_absorbed_into_one_pull() {
        let mut first = pev("p", 1, 2, &[ChangeKind::Frames], true);
        assert!(needs_pull(&first));
        assert!(absorb(
            &mut first,
            &pev("p", 2, 3, &[ChangeKind::Frames], true)
        ));
        assert!(
            absorb(&mut first, &pev("p", 3, 4, &[ChangeKind::Frames], false)),
            "an inline one is covered by the pull too"
        );
        assert_eq!(
            (
                first.prev,
                first.version,
                first.more,
                first.frames.is_none()
            ),
            (1, 4, true, true)
        );
    }

    #[test]
    fn absorb_refuses_another_project_a_gap_or_other_kinds() {
        let mut first = pev("p", 1, 2, &[ChangeKind::Frames], true);
        assert!(!absorb(
            &mut first,
            &pev("q", 2, 3, &[ChangeKind::Frames], true)
        ));
        assert!(
            !absorb(&mut first, &pev("p", 5, 6, &[ChangeKind::Frames], true)),
            "a gap is the applier's catch-up"
        );
        assert!(!absorb(
            &mut first,
            &pev("p", 2, 3, &[ChangeKind::Frames, ChangeKind::Members], true)
        ));
        assert_eq!(first.version, 2);
    }

    #[test]
    fn an_inline_event_does_not_start_a_merge() {
        let mut inline = pev("p", 1, 2, &[ChangeKind::Frames], false);
        inline.frames = Some(Vec::new());
        assert!(!needs_pull(&inline));
    }

    #[test]
    fn coalesce_keeps_every_refused_item_in_order() {
        let mut q: VecDeque<FeedWork> = VecDeque::new();
        q.push_back(q_event(pev("p", 2, 3, &[ChangeKind::Frames], true)));
        q.push_back(q_event(pev("q", 7, 8, &[ChangeKind::Frames], true)));
        q.push_back(q_event(pev("p", 3, 4, &[ChangeKind::Frames], true)));
        let (merged, n) = coalesce(pev("p", 1, 2, &[ChangeKind::Frames], true), &mut q);
        assert_eq!(
            (merged.version, n),
            (3, 1),
            "stops at the other project's event"
        );
        assert_eq!(q.len(), 2, "q's event and p's later event stay, in order");
        assert!(matches!(&q[0], FeedWork::Event(LiveEvent::Project(e)) if e.project_id == "q"));
        assert!(matches!(&q[1], FeedWork::Event(LiveEvent::Project(e)) if e.project_id == "p"));
    }

    #[test]
    fn coalesce_stops_at_a_non_project_item_and_resumes_after_it() {
        let mut q: VecDeque<FeedWork> = VecDeque::new();
        q.push_back(q_event(pev("p", 2, 3, &[ChangeKind::Frames], true)));
        q.push_back(FeedWork::NoteAppend("p".into(), Instant::now()));
        q.push_back(q_event(pev("p", 3, 4, &[ChangeKind::Frames], true)));
        let (merged, n) = coalesce(pev("p", 1, 2, &[ChangeKind::Frames], true), &mut q);
        assert_eq!((merged.version, n), (3, 1));
        assert_eq!(q.len(), 2);
        assert!(matches!(&q[0], FeedWork::NoteAppend(p, _) if p == "p"));
        assert!(matches!(&q[1], FeedWork::Event(LiveEvent::Project(e)) if e.version == 4));
    }

    /// Plan P7 / Review Focus 5 through `run`: two projects' pull-needing
    /// events with a non-project item (`NoteAppend`) and a `hello` between
    /// them. The carried re-dispatch path is exercised (a run is cut by
    /// another project's event and resumes after it). Nothing is lost: both
    /// projects' stored versions reach their heads, `pending` returns to
    /// zero (one decrement per ORIGINAL event), and no more manifests are
    /// pulled than project events were sent (unfolded costs one more).
    #[tokio::test]
    async fn run_folds_a_burst_and_pending_returns_to_zero() {
        let (_tmp, hub, _shared, worker, _rx) = worker_after_hello().await;
        let me = crate::api::account::own_device_id(&worker.ctx).unwrap();
        hub.add_account("tok", "acc-me", "Me", &me, None);
        hub.add_project("p2", "m42", &[("acc-me", "send_receive", false)], false);
        crate::api::collab::refresh_projects(&worker.ctx)
            .await
            .unwrap();
        let ctx = Arc::clone(&worker.ctx);
        let stored = |p: &str| {
            crate::db::collab::get_project(&crate::api::db(&ctx).unwrap().conn(), p)
                .unwrap()
                .map(|r| r.hub_version)
                .unwrap_or(0)
        };
        let u1: Vec<String> = (0..4).map(|i| format!("p1-{i}")).collect();
        let u2: Vec<String> = (0..2).map(|i| format!("p2-{i}")).collect();
        let r1: Vec<&str> = u1.iter().map(String::as_str).collect();
        let r2: Vec<&str> = u2.iter().map(String::as_str).collect();
        hub.seed_frames("p1", "acc-me", &r1, "pending");
        hub.seed_frames("p2", "acc-me", &r2, "pending");
        let (b1, b2) = (hub.version("p1"), hub.version("p2"));
        let bump = |p: &str, us: &[String]| {
            for u in us {
                hub.update_frame(p, u, |f| f.filter_canonical = "R".into());
            }
        };
        // First half of the burst, then the hello (built at that head), then
        // the second half: events after the hello are live, not stale.
        bump("p1", &u1[..2]);
        bump("p2", &u2[..1]);
        let hello: HelloEvent = serde_json::from_value(hub.hello_for("tok")).unwrap();
        bump("p1", &u1[2..]);
        bump("p2", &u2[1..]);
        let (h1, h2) = (hub.version("p1"), hub.version("p2"));
        let ev = |p: &str, v: i64| q_event(pev(p, v, v + 1, &[ChangeKind::Frames], true));
        let work = vec![
            ev("p1", b1),
            ev("p1", b1 + 1),
            ev("p2", b2),
            FeedWork::Event(LiveEvent::Hello(hello)),
            FeedWork::NoteAppend("p1".into(), Instant::now()),
            ev("p1", b1 + 2),
            ev("p1", b1 + 3),
            ev("p2", b2 + 1),
        ];
        assert_eq!((b1 + 4, b2 + 2), (h1, h2));
        let events = work
            .iter()
            .filter(|w| matches!(w, FeedWork::Event(LiveEvent::Project(_))))
            .count();
        let counted = events + 1; // the hello is an Event too
        let manifests = || async {
            hub.requests_to("/projects/p1/manifest").await
                + hub.requests_to("/projects/p2/manifest").await
        };
        let before = manifests().await;
        worker.pending.store(counted, Ordering::SeqCst);
        let pending = Arc::clone(&worker.pending);
        let (tx, rx) = mpsc::unbounded_channel();
        for w in work {
            tx.send(w).unwrap();
        }
        drop(tx);
        worker.run(rx).await;
        assert_eq!(
            pending.load(Ordering::SeqCst),
            0,
            "one decrement per original event"
        );
        assert_eq!(stored("p1"), h1, "p1 reaches its head");
        assert_eq!(stored("p2"), h2, "p2 reaches its head");
        let used = manifests().await - before;
        // Folded: a+b, x, c+d, y = 4 pulls, plus the hello's catch-up of
        // the two projects. Unfolded it is 5 + 2 (one more than the events).
        assert!(
            used <= events,
            "{used} manifest pulls for {events} project events"
        );
    }
}
