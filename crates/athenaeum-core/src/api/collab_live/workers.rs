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
use crate::collab::live::wire::LiveEvent;
use crate::services::ServiceContext;

// ── the feed worker ─────────────────────────────────────────────────────

/// Work for the feed worker, applied in the order it was sent.
pub(crate) enum FeedWork {
    /// The account's hub credentials (again when they changed).
    Creds(Option<(String, String)>),
    Event(LiveEvent),
    /// A claim change was appended for this project: its flush wait starts.
    NoteAppend(String, Instant),
    /// Sync now: a digest check per project.
    DigestAll(Vec<String>),
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
        }
    }

    pub(crate) async fn run(mut self, mut work: mpsc::UnboundedReceiver<FeedWork>) {
        loop {
            let due = self.holdings.as_ref().and_then(Holdings::next_deadline);
            // A due timer runs before more work, whatever is queued.
            if due.is_some_and(|d| Instant::now() >= d) {
                self.timers().await;
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
                    Some(w) => self.handle(w).await,
                },
                _ = sleep => self.timers().await,
            }
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
            FeedWork::DigestAll(projects) => {
                if let Some(h) = self.holdings.as_mut() {
                    h.clear_backoffs();
                    for p in &projects {
                        if let Err(e) = h.digest_check(p).await {
                            tracing::warn!(project_id = %p, error = %e, "sync now: claim digest check failed; retried on the next flush");
                        }
                    }
                }
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
                self.shared.reconnect_now();
            }
            Err(ApiError::Forbidden(e)) => match project {
                Some(project_id) => {
                    // The refusal is a not-ok report for its project too,
                    // and the only one: it replaces the applier's own
                    // report of this event.
                    let refused = refused_report(&project_id, &e);
                    match reports.iter_mut().find(|r| r.project_id == project_id) {
                        Some(r) => *r = refused,
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
}
