//! The feed applier (collab v3 wave 3, Task 5, I2/I3, spec §4.3–§4.4): drives
//! one project cache's worth of local state from the hub's `GET /me/events`
//! stream. Owns the per-project `(epoch, version)`/`(epoch, holderSeq)`
//! cursors ([`crate::collab::live::cursor`]), the prev-checked apply-vs-
//! catch-up decision, REST catch-up and resync, the 60 s versions vector, and
//! epoch change (reload every snapshot, re-announce own frames the hub no
//! longer lists under their existing uuids). The holder side of the feed
//! ([`HolderSide`]) is a trait so Task 6 can implement it for `Holdings`
//! without this module depending on that one.

use std::collections::HashSet;
use std::sync::Arc;

use crate::api::{db, ApiError};
use crate::collab::hub_client::{CollabClient, FrameInWire};
use crate::collab::live::cursor::{
    plan_hello, plan_versions, step, FeedCursor, HelloPlan, HolderPlan, Step, VersionsPlan,
};
use crate::collab::live::presence::PresenceBook;
use crate::collab::live::wire::{
    AccountEvent, AccountKind, ChangeKind, HelloEvent, HelloProject, HoldersEvent, LiveEvent,
    ProjectEvent, ResyncWhat, VersionsEvent,
};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;

/// Every kind a `project` event can carry, in the wire's canonical order. A
/// gap (`Step::CatchUp`) never knows which kinds it missed, so every catch-up
/// caused by a gap passes this instead of the event's own (necessarily
/// incomplete) `kinds` list.
const ALL_KINDS: [ChangeKind; 5] = [
    ChangeKind::Frames,
    ChangeKind::Meta,
    ChangeKind::Members,
    ChangeKind::Thresholds,
    ChangeKind::Dictionary,
];

/// One observable consequence of applying a feed event or a catch-up, for a
/// caller (the local session, a later task) to turn into a notification or a
/// UI refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEffect {
    NeedSetChanged(String),
    ProvidersChanged(String),
    MembersChanged(String),
    ProjectJoined(String),
    ProjectGone(String),
    EpochChanged,
}

/// The holder side of the feed (Task 6 implements it for `Holdings`). Kept
/// as a trait so this module never depends on the holder map's internals.
///
/// **Contract (fix round 3, item 5):** an implementation persists its own
/// `holder_seq` ONLY through [`crate::db::collab::set_holder_seq_only`],
/// never through [`crate::db::collab::set_holder_seq`]. The latter also
/// stamps `feed_epoch`, and every caller of these trait methods
/// (`FeedApplier::reload_one_project` in particular) relies on `feed_epoch`
/// moving in exactly one place — its own final write, after `reload` (and
/// the re-announce that follows it) have both already succeeded (I1). A
/// `reload`/`on_hello_project`/`on_holders_event`/`catch_up` implementation
/// that stamped `feed_epoch` itself would move the epoch before its own work
/// — the very bug this rule exists to keep out.
#[async_trait::async_trait]
pub trait HolderSide: Send {
    async fn on_hello_project(
        &mut self,
        project_id: &str,
        hello: &HelloProject,
        plan: HolderPlan,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError>;
    async fn on_holders_event(
        &mut self,
        ev: &HoldersEvent,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError>;
    async fn catch_up(
        &mut self,
        project_id: &str,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError>;
    /// Snapshot + full report (an epoch change, or an `account: joined`).
    async fn reload(&mut self, project_id: &str, epoch: &str) -> Result<Vec<FeedEffect>, ApiError>;
    fn forget(&mut self, project_id: &str);
}

/// Drives one live session's worth of feed events into the local cache.
pub struct FeedApplier {
    ctx: Arc<ServiceContext>,
    client: CollabClient,
    token: String,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    pub presence: PresenceBook,
    /// This device's account id, learned from `hello.accountId`.
    pub account_id: Option<String>,
    /// The hub epoch this session last confirmed, learned from `hello.epoch`
    /// (and updated by [`FeedApplier::epoch_change`]).
    pub epoch: Option<String>,
}

impl FeedApplier {
    pub fn new(
        ctx: Arc<ServiceContext>,
        client: CollabClient,
        token: String,
        emitter: Option<Arc<dyn ProgressEmitter>>,
    ) -> Self {
        FeedApplier {
            ctx,
            client,
            token,
            emitter,
            presence: PresenceBook::default(),
            account_id: None,
            epoch: None,
        }
    }

    fn stored_cursor(&self, project_id: &str) -> Result<Option<FeedCursor>, ApiError> {
        let database = db(&self.ctx)?;
        let conn = database.conn();
        Ok(
            crate::db::collab::get_live_project(&conn, project_id)?.map(|p| FeedCursor {
                epoch: p.feed_epoch,
                version: p.hub_version,
                holder_seq: p.holder_seq,
            }),
        )
    }

    /// True when `stored`'s own confirmed epoch does NOT match the session's
    /// (fix round 2, item 1). A version-number-only decision (`step`,
    /// `plan_versions`) against such a project's cursor would compare
    /// against a `manifest_cursor` left over from BEFORE its restore —
    /// exactly the silent-skip C1 was fixed against, just reopened one level
    /// up. `stored.epoch == None` (never live-fed) does not count: that
    /// project's own first catch-up is already a full fetch (`since = 0`
    /// from a fresh row), so the ordinary path already behaves like a
    /// reload.
    fn project_needs_epoch_reload(&self, stored: &FeedCursor) -> bool {
        self.epoch.as_deref().is_some_and(|session_epoch| {
            stored.epoch.is_some() && stored.epoch.as_deref() != Some(session_epoch)
        })
    }

    /// Apply one decoded event of the hub's `/me/events` stream.
    pub async fn apply(
        &mut self,
        ev: LiveEvent,
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        match ev {
            LiveEvent::Hello(h) => self.on_hello(h, holders).await,
            LiveEvent::Project(p) => self.on_project(p, holders).await,
            LiveEvent::Holders(h) => {
                // M7 fix round: never stamp an empty epoch. In real operation
                // `hello` always precedes every other event; this guard is
                // defensive (and load-bearing for a test that feeds events
                // out of order).
                let Some(epoch) = self.epoch.clone() else {
                    tracing::debug!(project_id = %h.project_id, "holders event received before the first hello; skipped");
                    return Ok(vec![]);
                };
                let stored = self.stored_cursor(&h.project_id)?.unwrap_or(FeedCursor {
                    epoch: None,
                    version: 0,
                    holder_seq: -1,
                });
                // Item 1 fix round 2: this project is stuck on a different
                // epoch than the session's confirmed one (its own reload
                // failed at some point) — a holders delta against its stale
                // holder map is meaningless; reload it fully instead.
                if self.project_needs_epoch_reload(&stored) {
                    tracing::warn!(project_id = %h.project_id, stored_epoch = ?stored.epoch, session_epoch = %epoch, "holders event for a project stuck on a different epoch; reloading it fully instead");
                    return match self
                        .reload_one_project(&h.project_id, &epoch, None, &ALL_KINDS, holders)
                        .await
                    {
                        Ok(effs) => Ok(effs),
                        Err(e) => {
                            tracing::error!(project_id = %h.project_id, error = %e, "per-project epoch reload failed (holders event); retried next event");
                            Ok(vec![])
                        }
                    };
                }
                holders.on_holders_event(&h, &epoch).await
            }
            LiveEvent::Presence(p) => {
                let changed = self.presence.apply_event(&p);
                Ok(if changed.is_empty() {
                    vec![]
                } else {
                    vec![FeedEffect::ProvidersChanged(p.project_id)]
                })
            }
            LiveEvent::Account(a) => self.on_account(a, holders).await,
            // Item 1 fix round 3: `resync` names ONE side of ONE project to
            // catch up over REST — exactly the shape `on_project`/`on_versions`
            // already guard. Without this, a project stuck on a stale epoch
            // would delta-fetch from its pre-restore `manifest_cursor` (or run
            // a holders catch-up against its stale map) and `set_feed_version`
            // would stamp the CURRENT (new) epoch anyway — losing that
            // project's reload for good, not just delaying it.
            LiveEvent::Resync(r) => match r.what {
                ResyncWhat::Project => {
                    let Some(epoch) = self.epoch.clone() else {
                        tracing::debug!(project_id = %r.project_id, "resync(project) received before the first hello; skipped");
                        return Ok(vec![]);
                    };
                    let stored = self.stored_cursor(&r.project_id)?.unwrap_or(FeedCursor {
                        epoch: None,
                        version: 0,
                        holder_seq: -1,
                    });
                    if self.project_needs_epoch_reload(&stored) {
                        tracing::warn!(project_id = %r.project_id, stored_epoch = ?stored.epoch, session_epoch = %epoch, "resync(project) for a project stuck on a different epoch; reloading it fully instead of a delta");
                        return match self
                            .reload_one_project(&r.project_id, &epoch, None, &ALL_KINDS, holders)
                            .await
                        {
                            Ok(effs) => Ok(effs),
                            Err(e) => {
                                tracing::error!(project_id = %r.project_id, error = %e, "per-project epoch reload failed (resync); retried next event");
                                Ok(vec![])
                            }
                        };
                    }
                    self.catch_up_project(&r.project_id, None, &ALL_KINDS).await
                }
                ResyncWhat::Holders => {
                    let Some(epoch) = self.epoch.clone() else {
                        tracing::debug!(project_id = %r.project_id, "resync(holders) received before the first hello; skipped");
                        return Ok(vec![]);
                    };
                    let stored = self.stored_cursor(&r.project_id)?.unwrap_or(FeedCursor {
                        epoch: None,
                        version: 0,
                        holder_seq: -1,
                    });
                    if self.project_needs_epoch_reload(&stored) {
                        tracing::warn!(project_id = %r.project_id, stored_epoch = ?stored.epoch, session_epoch = %epoch, "resync(holders) for a project stuck on a different epoch; reloading it fully instead of a stale-map catch-up");
                        return match self
                            .reload_one_project(&r.project_id, &epoch, None, &ALL_KINDS, holders)
                            .await
                        {
                            Ok(effs) => Ok(effs),
                            Err(e) => {
                                tracing::error!(project_id = %r.project_id, error = %e, "per-project epoch reload failed (resync); retried next event");
                                Ok(vec![])
                            }
                        };
                    }
                    holders.catch_up(&r.project_id, &epoch).await
                }
                ResyncWhat::Unknown => {
                    tracing::warn!(project_id = %r.project_id, "resync event names a side this build doesn't know; ignored");
                    Ok(vec![])
                }
            },
            LiveEvent::Versions(v) => self.on_versions(v, holders).await,
            LiveEvent::Unknown(name) => {
                tracing::debug!(event = %name, "unknown live event kind; ignored (forward compatibility)");
                Ok(vec![])
            }
        }
    }

    /// The stream's first event: this device's session, the account's epoch,
    /// and every one of the account's projects now.
    async fn on_hello(
        &mut self,
        hello: HelloEvent,
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let mut effects = Vec::new();
        self.account_id = Some(hello.account_id.clone());

        let max_report_seq = hello
            .projects
            .values()
            .map(|p| p.report_seq)
            .max()
            .unwrap_or(0);
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab_live::meta_set(
                &conn,
                crate::db::collab_live::META_ACCOUNT_ID,
                &hello.account_id,
            )?;
            if max_report_seq > 0 {
                crate::db::collab_live::raise_report_seq(&conn, max_report_seq)?;
            }
        }

        // A cached live project absent from `hello.projects`: the account
        // left this project while disconnected. Fix round 2, item 5: isolate
        // per project like every other loop here — one failing `on_account`
        // must not abort the rest of this hello.
        let cached_live: Vec<String> = {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::list_projects(&conn)?
                .into_iter()
                .map(|p| p.project_id)
                .collect()
        };
        for pid in &cached_live {
            if !hello.projects.contains_key(pid) {
                match self
                    .on_account(
                        AccountEvent {
                            kind: AccountKind::Left,
                            project_id: pid.clone(),
                        },
                        holders,
                    )
                    .await
                {
                    Ok(effs) => effects.extend(effs),
                    Err(e) => {
                        tracing::error!(project_id = %pid, error = %e, "processing this project's departure failed; retried on the next hello");
                    }
                }
            }
        }

        // A project in `hello.projects` without a cache row: a fresh join
        // this device never saw the `account: joined` event for (e.g. it
        // happened before this device's first-ever connect). Refresh once so
        // the per-project plan below has a cursor to compare against, and
        // report it joined — the brief's "treat as joined" (fix round: this
        // must actually emit `ProjectJoined`, not just silently fall into the
        // ordinary per-project plan). I2 fix round: a refresh failure here is
        // isolated per project — it never aborts the rest of this hello, and
        // the project (still uncached) is simply retried on the next one.
        for (pid, _) in &hello.projects {
            if self.stored_cursor(pid)?.is_none() {
                let only: HashSet<String> = [pid.clone()].into_iter().collect();
                match crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only)).await {
                    Ok(_) => effects.push(FeedEffect::ProjectJoined(pid.clone())),
                    Err(e) => {
                        tracing::error!(project_id = %pid, error = %e, "could not refresh a newly seen project; retried on the next hello");
                    }
                }
            }
        }

        let mut plans: Vec<(String, HelloProject, HelloPlan, FeedCursor)> =
            Vec::with_capacity(hello.projects.len());
        let mut epoch_changed_any = false;
        for (pid, hp) in &hello.projects {
            self.presence.apply_hello(pid, &hp.presence);
            let stored = self.stored_cursor(pid)?.unwrap_or(FeedCursor {
                epoch: None,
                version: 0,
                holder_seq: -1,
            });
            let plan = plan_hello(&stored, &hello.epoch, hp.version, hp.holder_seq);
            epoch_changed_any |= plan.epoch_changed;
            plans.push((pid.clone(), hp.clone(), plan, stored));
        }

        if epoch_changed_any {
            // Item 2 fix round 3: `epoch_change`'s own regression check needs
            // both axes (a holder-seq-only regression is an epoch change too,
            // per `plan_hello`/`plan_versions`), so both are carried through.
            let heads: std::collections::BTreeMap<String, (i64, i64)> = hello
                .projects
                .iter()
                .map(|(pid, hp)| (pid.clone(), (hp.version, hp.holder_seq)))
                .collect();
            // `epoch_change` itself sets `self.epoch` once every reload it
            // could complete has run (I1 fix round) — nothing left to do here
            // but return what it collected. Fix round 2, item 4: it reloads
            // only the projects that actually need it (stuck on the old
            // epoch, or freshly regressed), never every live project every
            // single hello.
            effects.extend(self.epoch_change(&hello.epoch, &heads, holders).await?);
            return Ok(effects);
        }

        // Every project keeps the SAME epoch from here: set it before the
        // per-project catch-ups below, which stamp `set_feed_version` with
        // `self.epoch`.
        self.epoch = Some(hello.epoch.clone());

        // I2 fix round: one project's catch-up (or holder hello-sync) failure
        // must not abort every other project's — and must not repeat forever
        // on every future hello either. Isolate per project, log, continue;
        // a project whose catch-up failed simply keeps its old cursor, so
        // the next `project`/`versions` event (or hello) retries it.
        for (pid, hp, plan, stored) in plans {
            if plan.catch_up_project {
                match self
                    .catch_up_project(&pid, Some(hp.version), &ALL_KINDS)
                    .await
                {
                    Ok(effs) => effects.extend(effs),
                    Err(e) => {
                        tracing::error!(project_id = %pid, error = %e, "hello catch-up failed for this project; retried on the next hello");
                    }
                }
            } else if stored.epoch.is_none() {
                // Fix round 4, finding 3 (controller ruling): a quiet row
                // never live-fed (`feed_epoch` NULL) that is already in sync
                // with the head runs no catch-up, and the holder side only
                // ever persists its seq — nothing would stamp its epoch, so
                // a later hub epoch change with no regression would go
                // undetected for it. Stamp the hello epoch with its current
                // version; no fetch.
                let stamped = {
                    let database = db(&self.ctx)?;
                    let conn = database.conn();
                    crate::db::collab::set_feed_version(&conn, &pid, &hello.epoch, stored.version)
                };
                if let Err(e) = stamped {
                    tracing::error!(project_id = %pid, error = %e, "stamping the hello epoch on an in-sync project failed; retried on the next hello");
                }
            }
            match holders
                .on_hello_project(&pid, &hp, plan.holders, &hello.epoch)
                .await
            {
                Ok(effs) => effects.extend(effs),
                Err(e) => {
                    tracing::error!(project_id = %pid, error = %e, "holder hello-sync failed for this project");
                }
            }
        }

        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab_live::meta_set(
                &conn,
                crate::db::collab_live::META_EPOCH,
                &hello.epoch,
            )?;
        }
        Ok(effects)
    }

    /// A `project` event: prev-checked apply, ignore, or REST catch-up.
    async fn on_project(
        &mut self,
        ev: ProjectEvent,
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let Some(epoch) = self.epoch.clone() else {
            tracing::debug!(project_id = %ev.project_id, "project event received before the first hello; skipped");
            return Ok(vec![]);
        };
        let stored = self.stored_cursor(&ev.project_id)?.unwrap_or(FeedCursor {
            epoch: None,
            version: 0,
            holder_seq: -1,
        });
        // Item 1 fix round 2: this project's own stored epoch is behind the
        // session's — its `manifest_cursor` may be a pre-restore number a
        // plain version comparison (`step`, below) cannot tell apart from a
        // legitimate contiguous head. Route it to a full reload instead of
        // ever taking `Step::Apply`/`Step::CatchUp`'s delta path.
        if self.project_needs_epoch_reload(&stored) {
            tracing::warn!(project_id = %ev.project_id, stored_epoch = ?stored.epoch, session_epoch = %epoch, "project event for a project stuck on a different epoch; reloading it fully instead of a delta");
            // Item 3 fix round 3: pass the event's own `kinds` through — a
            // Members/Thresholds/Dictionary/Meta change this event carries
            // must still be refreshed and reported (`MembersChanged` etc.),
            // not silently consumed by the cursor moving past it.
            return match self
                .reload_one_project(&ev.project_id, &epoch, Some(ev.version), &ev.kinds, holders)
                .await
            {
                Ok(effs) => Ok(effs),
                Err(e) => {
                    tracing::error!(project_id = %ev.project_id, error = %e, "per-project epoch reload failed (project event); retried next event");
                    Ok(vec![])
                }
            };
        }
        match step(stored.version, ev.prev, ev.version) {
            Step::Ignore => Ok(vec![]),
            // Every kind of a gap is unknown, so a gap catches up on all of
            // them (T5 ruling).
            Step::CatchUp => {
                self.catch_up_project(&ev.project_id, Some(ev.version), &ALL_KINDS)
                    .await
            }
            Step::Apply => self.apply_contiguous(ev).await,
        }
    }

    async fn apply_contiguous(&mut self, ev: ProjectEvent) -> Result<Vec<FeedEffect>, ApiError> {
        let pid = ev.project_id.clone();
        // M7 fix round: never stamp an empty epoch (defensive — `hello`
        // always precedes a `project` event in real operation).
        let Some(epoch) = self.epoch.clone() else {
            tracing::debug!(project_id = %pid, "project event received before the first hello; skipped");
            return Ok(vec![]);
        };
        let mut effects = Vec::new();
        let small_docs = ev.kinds.iter().any(|k| {
            matches!(
                k,
                ChangeKind::Meta
                    | ChangeKind::Members
                    | ChangeKind::Thresholds
                    | ChangeKind::Dictionary
            )
        });
        if small_docs {
            let only: HashSet<String> = [pid.clone()].into_iter().collect();
            let report =
                crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only)).await?;
            for moved in &report.gate_moved {
                crate::api::collab::on_thresholds_or_dictionary_moved(&self.ctx, moved);
            }
            if ev.kinds.contains(&ChangeKind::Members) {
                effects.push(FeedEffect::MembersChanged(pid.clone()));
            }
        }
        if ev.kinds.contains(&ChangeKind::Grid) {
            tracing::debug!(project_id = %pid, version = ev.version, "grid change seen; no consumer in this wave");
        }

        // I4 fix round: a caps change — possibly just surfaced by the
        // small-document refresh above, e.g. a `members` event that changed
        // MY governance caps — needs the full since=0 fetch + prune the caps
        // rule requires. `apply_inline`'s lightweight write would mark the
        // caps synced without ever doing that fetch, silently losing rows a
        // caps-narrowing must prune (or never pulling rows a caps-widening
        // newly reveals).
        let caps_changed = {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            let project = crate::api::collab_exchange::live_project(&conn, &pid)?;
            project.gov_caps_json != project.synced_caps_json
        };
        if caps_changed {
            crate::api::collab_exchange::sync_manifest(
                &self.ctx,
                &pid,
                self.emitter.as_deref(),
                Some(ev.version),
            )
            .await?;
            effects.push(FeedEffect::NeedSetChanged(pid.clone()));
        } else {
            match (ev.kinds.contains(&ChangeKind::Frames), ev.frames) {
                (true, Some(rows)) if !ev.more => {
                    self.apply_inline(&pid, ev.version, &epoch, rows)?;
                    effects.push(FeedEffect::NeedSetChanged(pid.clone()));
                }
                (true, _) => {
                    crate::api::collab_exchange::sync_manifest(
                        &self.ctx,
                        &pid,
                        self.emitter.as_deref(),
                        Some(ev.version),
                    )
                    .await?;
                    effects.push(FeedEffect::NeedSetChanged(pid.clone()));
                }
                (false, _) => {}
            }
        }
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::set_feed_version(&conn, &pid, &epoch, ev.version)?;
        }
        tracing::debug!(project_id = %pid, prev = ev.prev, version = ev.version, "project event applied");
        Ok(effects)
    }

    /// Apply a `project` event's inlined frame rows straight from the
    /// stream, with no manifest read at all. Only reached when this
    /// project's caps are already in sync (see [`Self::apply_contiguous`]'s
    /// `caps_changed` gate, I4 fix round).
    fn apply_inline(
        &self,
        pid: &str,
        version: i64,
        epoch: &str,
        mut rows: Vec<crate::collab::hub_client::FrameViewWire>,
    ) -> Result<(), ApiError> {
        use crate::db::collab_frames as frames_db;
        let database = db(&self.ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, pid)?;
        let own = crate::api::collab_exchange::own_devices(&self.ctx, &conn, &project)?;
        // IMMEDIATE: this transaction reads before it writes, and a
        // read-to-write upgrade under another writer fails at once with
        // SQLITE_BUSY (or BUSY_SNAPSHOT) — the busy timeout never applies —
        // and the event would be dropped (the last event of a burst then
        // waits for the 60 s versions vector). Taking the write lock up front
        // waits the busy timeout instead.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let mut routes = frames_db::EngineRoutes::default();
        let mut max_mv = project.manifest_cursor;
        let mut counts: std::collections::BTreeMap<
            crate::api::collab_exchange::FramesChangeKind,
            usize,
        > = Default::default();
        for v in rows.iter_mut() {
            let prev = frames_db::get(&tx, pid, &v.frame_uuid)?;
            // A6: own per DEVICE, never per account.
            crate::api::collab_exchange::derive_device_own(v, &own, prev.as_ref());
            for kind in crate::api::collab_exchange::classify_frame_change(prev.as_ref(), v) {
                *counts.entry(kind).or_default() += 1;
            }
            frames_db::upsert_from_manifest_deferred(&tx, pid, v, &mut routes)?;
            max_mv = max_mv.max(v.manifest_version);
        }
        crate::db::collab::set_sync_state(&tx, pid, Some(version), max_mv, &project.gov_caps_json)?;
        crate::db::collab::set_feed_version(&tx, pid, epoch, version)?;
        tx.commit()?;
        routes.route();
        for (kind, count) in counts {
            let change = crate::api::collab_exchange::CollabFramesChange {
                project_id: pid.to_string(),
                kind,
                count,
            };
            tracing::info!(
                project_id = pid,
                count,
                kind = kind.as_str(),
                "manifest delta applied"
            );
            if let Some(em) = self.emitter.as_deref() {
                crate::events::emit_event(
                    em,
                    crate::api::collab_exchange::COLLAB_FRAMES_CHANGED_EVENT,
                    &change,
                );
            }
        }
        Ok(())
    }

    /// Catch a project up over REST: the manifest delta FIRST (I3 fix
    /// round), then the small documents named in `kinds`
    /// (membership/thresholds/dictionary/meta). `version` is the head that
    /// triggered this catch-up, logged only — the stored cursor becomes the
    /// manifest response's OWN `projectVersion`, never an event head the
    /// fetch might not have actually reached (T5 ruling).
    ///
    /// Reading the manifest first matters: the old order (small documents,
    /// then manifest) let a caps/members/thresholds/dictionary change commit
    /// BETWEEN the two reads. That change's version would already be inside
    /// the cursor the manifest read then produced, yet this catch-up would
    /// never have refreshed it — the next hello/versions/project event would
    /// see the cursor as fully caught up and never ask again. Reading the
    /// manifest first means whatever cursor this catch-up lands on, the
    /// small-document refresh that follows picks up AT LEAST that state.
    ///
    /// Fix round 2, item 3: reading the manifest first only moves the race
    /// window — it does not close it. If the small-document refresh ITSELF
    /// is what reveals a governance-caps change (this device's caps just
    /// narrowed or widened), the manifest fetch above already ran under the
    /// OLD caps, and its `projectVersion` would otherwise become the stored
    /// cursor with no fetch ever having happened under the NEW caps. So
    /// after the small-document refresh, this re-checks `gov_caps_json` vs
    /// `synced_caps_json` and — only on a mismatch — runs one more full
    /// (`since = 0`) fetch + prune before the cursor is allowed to move.
    pub async fn catch_up_project(
        &mut self,
        project_id: &str,
        version: Option<i64>,
        kinds: &[ChangeKind],
    ) -> Result<Vec<FeedEffect>, ApiError> {
        // M7 fix round: never stamp an empty epoch.
        let Some(epoch) = self.epoch.clone() else {
            tracing::debug!(
                project_id,
                "catch-up requested before the first hello; skipped"
            );
            return Ok(vec![]);
        };
        let mut effects = Vec::new();
        let small_docs = kinds.iter().any(|k| {
            matches!(
                k,
                ChangeKind::Meta
                    | ChangeKind::Members
                    | ChangeKind::Thresholds
                    | ChangeKind::Dictionary
            )
        });

        // M3 fix round: a manifest-sync failure is logged here, at the
        // applier boundary, before it propagates — every caller of this
        // function used to fail silently past this point.
        let (_changes, _seen, mut project_version) =
            crate::api::collab_exchange::sync_manifest_inner(
                &self.ctx,
                project_id,
                self.emitter.as_deref(),
                None,
                false,
            )
            .await
            .map_err(|e| {
                tracing::warn!(project_id, requested_version = ?version, error = %e, "catch-up manifest sync failed; cursor stays put for a retry");
                e
            })?;

        if small_docs {
            let only: HashSet<String> = [project_id.to_string()].into_iter().collect();
            // Fix round 2, item 7: a failure here means the small-document
            // state (and, if item 3's re-check below never even runs, any
            // caps change it would have revealed) never landed — the cursor
            // must stay put for a retry, same as the manifest failure above,
            // not "still advance from the manifest" as the previous wording
            // claimed (the `?` on this line returns before the cursor write
            // further down ever runs).
            let report = crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only))
                .await
                .map_err(|e| {
                    tracing::warn!(project_id, error = %e, "catch-up small-document refresh failed; cursor stays put for a retry");
                    e
                })?;
            for moved in &report.gate_moved {
                crate::api::collab::on_thresholds_or_dictionary_moved(&self.ctx, moved);
            }
            if kinds.contains(&ChangeKind::Members) {
                effects.push(FeedEffect::MembersChanged(project_id.to_string()));
            }

            // Item 3 fix round 2: the manifest fetch above ran under the
            // caps this device had BEFORE the refresh just above. If that
            // refresh changed them, this project needs the caps rule's full
            // fetch + prune before the cursor is allowed to advance — a
            // project that then goes quiet would otherwise never refetch or
            // prune again.
            let caps_changed_now = {
                let database = db(&self.ctx)?;
                let conn = database.conn();
                let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
                project.gov_caps_json != project.synced_caps_json
            };
            if caps_changed_now {
                let (_c2, _s2, pv2) = crate::api::collab_exchange::sync_manifest_inner(
                    &self.ctx,
                    project_id,
                    self.emitter.as_deref(),
                    None,
                    true,
                )
                .await
                .map_err(|e| {
                    tracing::warn!(project_id, error = %e, "catch-up caps-change full resync failed; cursor stays put for a retry");
                    e
                })?;
                project_version = pv2;
            }
        }

        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::set_feed_version(&conn, project_id, &epoch, project_version)?;
        }
        tracing::debug!(
            project_id,
            requested_version = ?version,
            applied_version = project_version,
            "project caught up over rest"
        );
        effects.push(FeedEffect::NeedSetChanged(project_id.to_string()));
        Ok(effects)
    }

    /// The 60 s `versions` self-heal vector: per project, a head above the
    /// cursor is a catch-up, a head below is an epoch change.
    async fn on_versions(
        &mut self,
        v: VersionsEvent,
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        // M7 fix round: a versions-vector epoch change (or any processing at
        // all) received before the first hello waits for hello — never
        // stamp an empty epoch string.
        let Some(epoch) = self.epoch.clone() else {
            tracing::debug!("versions vector received before the first hello; waiting for hello");
            return Ok(vec![]);
        };
        let mut effects = Vec::new();
        let mut epoch_change_needed = false;
        let mut catchups: Vec<(String, i64, bool, bool)> = Vec::new();
        let mut stale_epoch_projects: Vec<(String, i64)> = Vec::new();
        for (pid, (head_version, head_holder_seq)) in &v {
            let Some(stored) = self.stored_cursor(pid)? else {
                continue;
            };
            // Item 1/4 fix round 2: a project already stuck on a different
            // epoch than the session's confirmed one must never take a
            // version-number-only delta decision (that would silently
            // reopen C1 against ITS stale, pre-restore manifest cursor), and
            // it must never be allowed to trigger a GLOBAL epoch change for
            // every other, perfectly healthy, project either — it's routed
            // to its own per-project reload alone.
            if self.project_needs_epoch_reload(&stored) {
                stale_epoch_projects.push((pid.clone(), *head_version));
                continue;
            }
            match plan_versions(&stored, *head_version, *head_holder_seq) {
                VersionsPlan::InSync => {}
                VersionsPlan::CatchUp { project, holders } => {
                    catchups.push((pid.clone(), *head_version, project, holders));
                }
                VersionsPlan::EpochChange => epoch_change_needed = true,
            }
        }

        // Item 5 fix round 2: isolate each stale-epoch project's own reload
        // — one permanently failing project must never block a healthy
        // sibling's per-tick reload, nor repeat across every OTHER project
        // (see the `stale_epoch_projects` split above).
        for (pid, head_version) in stale_epoch_projects {
            match self
                .reload_one_project(&pid, &epoch, Some(head_version), &ALL_KINDS, holders)
                .await
            {
                Ok(effs) => effects.extend(effs),
                Err(e) => {
                    tracing::error!(project_id = %pid, error = %e, "per-project epoch reload failed (versions tick); retried next tick");
                }
            }
        }

        if epoch_change_needed {
            // Item 2 fix round 3: `v` already carries `(version, holderSeq)`
            // per project — exactly what `epoch_change`'s own regression
            // check needs on both axes.
            effects.extend(self.epoch_change(&epoch, &v, holders).await?);
            return Ok(effects);
        }
        // Item 5 fix round 2: isolate each catch-up — one project's failure
        // must not skip every other project's still in this same tick.
        for (pid, head_version, needs_project, needs_holders) in catchups {
            if needs_project {
                match self
                    .catch_up_project(&pid, Some(head_version), &ALL_KINDS)
                    .await
                {
                    Ok(effs) => effects.extend(effs),
                    Err(e) => {
                        tracing::error!(project_id = %pid, error = %e, "versions catch-up failed for this project; retried next tick");
                    }
                }
            }
            if needs_holders {
                match holders.catch_up(&pid, &epoch).await {
                    Ok(effs) => effects.extend(effs),
                    Err(e) => {
                        tracing::error!(project_id = %pid, error = %e, "versions holder catch-up failed for this project; retried next tick");
                    }
                }
            }
        }
        Ok(effects)
    }

    /// The caller's own account membership on a project changed.
    async fn on_account(
        &mut self,
        ev: AccountEvent,
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        match ev.kind {
            AccountKind::Joined => {
                // M7 fix round: never stamp an empty epoch.
                let Some(epoch) = self.epoch.clone() else {
                    tracing::debug!(project_id = %ev.project_id, "account-joined received before the first hello; skipped");
                    return Ok(vec![]);
                };
                // Fix round 4, finding 2: a LIVE cache row stuck on a
                // different epoch than the session's must move into it only
                // through a full reload — the refresh + delta catch-up below
                // would fetch from its pre-restore `manifest_cursor` and
                // stamp the new epoch anyway, losing the reload for good.
                // `reload_one_project` already runs the small-document
                // refresh and `holders.reload` itself.
                if let Some(stored) = self.stored_cursor(&ev.project_id)? {
                    if self.project_needs_epoch_reload(&stored) {
                        tracing::warn!(project_id = %ev.project_id, stored_epoch = ?stored.epoch, session_epoch = %epoch, "account-joined for a project stuck on a different epoch; reloading it fully instead of a delta");
                        let mut effects = self
                            .reload_one_project(&ev.project_id, &epoch, None, &ALL_KINDS, holders)
                            .await
                            .map_err(|e| {
                                tracing::error!(project_id = %ev.project_id, error = %e, "per-project epoch reload failed (account joined); its cursor stays on the old epoch for a retry");
                                e
                            })?;
                        effects.push(FeedEffect::ProjectJoined(ev.project_id));
                        return Ok(effects);
                    }
                }
                let only: HashSet<String> = [ev.project_id.clone()].into_iter().collect();
                let report =
                    crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only)).await?;
                for moved in &report.gate_moved {
                    crate::api::collab::on_thresholds_or_dictionary_moved(&self.ctx, moved);
                }
                let mut effects = self
                    .catch_up_project(&ev.project_id, None, &ALL_KINDS)
                    .await?;
                effects.extend(holders.reload(&ev.project_id, &epoch).await?);
                effects.push(FeedEffect::ProjectJoined(ev.project_id));
                Ok(effects)
            }
            AccountKind::Left => {
                let only: HashSet<String> = [ev.project_id.clone()].into_iter().collect();
                crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only)).await?;
                {
                    let database = db(&self.ctx)?;
                    let conn = database.conn();
                    crate::db::collab_live::clear_project_live_state(&conn, &ev.project_id)?;
                }
                self.presence.forget_project(&ev.project_id);
                holders.forget(&ev.project_id);
                Ok(vec![FeedEffect::ProjectGone(ev.project_id)])
            }
            AccountKind::Unknown => {
                tracing::warn!(project_id = %ev.project_id, "account event of a kind this build doesn't know; ignored");
                Ok(vec![])
            }
        }
    }

    /// `hello.epoch != stored epoch`, or a hub head below the stored cursor
    /// on EITHER axis (a restore): reload every project that actually needs
    /// it, reconcile holdings, and re-announce this device's own frames the
    /// hub no longer lists. `heads` is the per-project `(version, holderSeq)`
    /// from the triggering hello or versions vector; a live project missing
    /// from it keeps whatever version its manifest resync lands on.
    ///
    /// I1/I2 fix round: each project's reload ([`Self::reload_one_project`])
    /// is isolated and, crucially, its cursor moves into `new_epoch` ONLY
    /// after every step of its own reload has already succeeded. A project
    /// whose reload fails partway keeps its OLD `feed_epoch` stamped, so the
    /// next hello or versions check sees it as still on the stale epoch and
    /// retries the WHOLE reload for it (never a lightweight delta catch-up
    /// against a manifest cursor from before the restore — see
    /// [`Self::project_needs_epoch_reload`], fix round 2 item 1). Other
    /// projects' successful reloads are unaffected, and this function itself
    /// always returns `Ok` — a caller (`on_hello`/`on_versions`) never has
    /// the whole hello or versions pass aborted by one broken project.
    ///
    /// Fix round 2, item 4: a project already on `new_epoch` with no head
    /// regression is skipped entirely — a permanently failing sibling must
    /// never force every healthy project through a redundant since=0
    /// fetch + prune + snapshot + re-announce on every single hello or
    /// versions tick. Fix round 3, item 2: "no head regression" is checked on
    /// BOTH axes — `plan_hello`/`plan_versions` (`collab/live/cursor.rs`)
    /// already declare an epoch change on a holder-seq regression alone (the
    /// version can be perfectly fine), and this skip must agree, or such a
    /// project would be skipped here forever while `EpochChanged` keeps
    /// firing with nothing ever resetting it.
    pub async fn epoch_change(
        &mut self,
        new_epoch: &str,
        heads: &std::collections::BTreeMap<String, (i64, i64)>,
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        tracing::warn!(
            epoch = new_epoch,
            "hub epoch changed; reloading affected projects"
        );
        let live: Vec<(String, FeedCursor)> = {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::list_projects(&conn)?
                .into_iter()
                .map(|p| {
                    (
                        p.project_id,
                        FeedCursor {
                            epoch: p.feed_epoch,
                            version: p.hub_version,
                            holder_seq: p.holder_seq,
                        },
                    )
                })
                .collect()
        };
        let mut effects = vec![FeedEffect::EpochChanged];
        for (pid, stored) in live {
            let already_on_new_epoch = stored.epoch.as_deref() == Some(new_epoch);
            let head = heads.get(&pid).copied();
            let version_regressed = head.is_some_and(|(v, _)| v < stored.version);
            // Same rule as `plan_versions`/`plan_hello`: no local holder map
            // yet (`< 0`) never counts as a regression.
            let holder_regressed =
                head.is_some_and(|(_, h)| stored.holder_seq >= 0 && h < stored.holder_seq);
            if already_on_new_epoch && !version_regressed && !holder_regressed {
                // Already reloaded into this exact epoch, no further
                // regression on either axis since — nothing to redo (item 4).
                continue;
            }
            match self
                .reload_one_project(&pid, new_epoch, head.map(|(v, _)| v), &ALL_KINDS, holders)
                .await
            {
                Ok(effs) => effects.extend(effs),
                Err(e) => {
                    tracing::error!(
                        project_id = %pid,
                        epoch = new_epoch,
                        error = %e,
                        "epoch reload failed for this project; its cursor stays on the old epoch so the next pass retries it"
                    );
                }
            }
        }
        // Written after every project has been attempted (I1 fix round): a
        // partial failure above never prevented this from being reached, so
        // the session's own confirmed epoch always tracks the hub's.
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab_live::meta_set(&conn, crate::db::collab_live::META_EPOCH, new_epoch)?;
        }
        self.epoch = Some(new_epoch.to_string());
        Ok(effects)
    }

    /// Reload ONE project fully: manifest resync from 0, the small documents
    /// (membership/thresholds/dictionary/meta, with their own caps re-check —
    /// fix round 3, item 3), a holder-seq reset, the holder side's own
    /// reload, this device's own-frame re-announce, then — only once every
    /// one of those has already succeeded — the single write that moves this
    /// project's cursor into `new_epoch`. Shared by [`Self::epoch_change`]
    /// (every project that needs it, a genuine hub-wide epoch rotation) and
    /// by [`Self::on_project`]/[`Self::on_versions`]/the `holders` and
    /// `resync` dispatch in [`Self::apply`] (fix round 2 item 1, fix round 3
    /// item 1) when ONE project's own stored epoch is found behind the
    /// session's — that project must never take a version-number delta
    /// decision against a manifest cursor left over from before its own
    /// restore. `target_version`, when known (the event/tick that triggered
    /// this), is preferred for the cursor; otherwise the manifest's own
    /// freshly-fetched `projectVersion` is used (fix round 2, item 6 — never
    /// a pre-update DB read, which could itself be stale). `kinds` is the
    /// triggering event's own kinds when known (`on_project`), else
    /// [`ALL_KINDS`] (a hub-wide epoch change, or a tick/event with no kind
    /// information of its own) — used only to decide whether to report
    /// `MembersChanged`; the small-document refresh itself always runs.
    ///
    /// Fix round 3, item 3: this reload used to stamp the cursor right after
    /// the manifest fetch, with NO small-document refresh at all — a
    /// Members/Thresholds/Dictionary/Meta change carried by the very event
    /// that routed a stuck project here (or by an ordinary hub-wide epoch
    /// change) was silently consumed: the cursor moved past it, and it was
    /// never applied. This now runs the identical refresh-then-caps-recheck
    /// sequence [`Self::catch_up_project`] runs (item 3, fix round 2) and
    /// reports the same effects (`MembersChanged`, `on_thresholds_or_dictionary_moved`).
    ///
    /// Fix round 2, item 2: the holder-seq reset happens FIRST, through
    /// [`crate::db::collab::reset_holder_seq`], which touches ONLY
    /// `holder_seq` — never `feed_epoch`. `feed_epoch` moves in exactly one
    /// place: the final `set_feed_version` write below, after
    /// `holders.reload` and the re-announce have both already succeeded.
    async fn reload_one_project(
        &mut self,
        pid: &str,
        new_epoch: &str,
        target_version: Option<i64>,
        kinds: &[ChangeKind],
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let (mut seen, mut project_version) = crate::api::collab_exchange::sync_manifest_full(
            &self.ctx,
            pid,
            self.emitter.as_deref(),
            None,
        )
        .await
        .map_err(|e| {
            tracing::warn!(project_id = pid, epoch = new_epoch, error = %e, "epoch-reload manifest sync failed");
            e
        })?;

        let mut effects = Vec::new();
        {
            let only: HashSet<String> = [pid.to_string()].into_iter().collect();
            let report = crate::api::collab::refresh_projects_reporting(&self.ctx, Some(&only))
                .await
                .map_err(|e| {
                    tracing::warn!(project_id = pid, epoch = new_epoch, error = %e, "epoch-reload small-document refresh failed");
                    e
                })?;
            for moved in &report.gate_moved {
                crate::api::collab::on_thresholds_or_dictionary_moved(&self.ctx, moved);
            }
            if kinds.contains(&ChangeKind::Members) {
                effects.push(FeedEffect::MembersChanged(pid.to_string()));
            }
            let caps_changed_now = {
                let database = db(&self.ctx)?;
                let conn = database.conn();
                let project = crate::api::collab_exchange::live_project(&conn, pid)?;
                project.gov_caps_json != project.synced_caps_json
            };
            if caps_changed_now {
                let (seen2, pv2) = crate::api::collab_exchange::sync_manifest_full(
                    &self.ctx,
                    pid,
                    self.emitter.as_deref(),
                    None,
                )
                .await
                .map_err(|e| {
                    tracing::warn!(project_id = pid, epoch = new_epoch, error = %e, "epoch-reload caps-change full resync failed");
                    e
                })?;
                seen = seen2;
                project_version = pv2;
            }
        }

        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::reset_holder_seq(&conn, pid)?;
        }
        effects.extend(holders.reload(pid, new_epoch).await?);
        reannounce_lost_own_frames(&self.ctx, &self.client, &self.token, pid, &seen).await?;
        let cursor_version = target_version.unwrap_or(project_version);
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::set_feed_version(&conn, pid, new_epoch, cursor_version)?;
        }
        effects.push(FeedEffect::NeedSetChanged(pid.to_string()));
        Ok(effects)
    }
}

/// This device's own frames the hub no longer lists (`seen` — every uuid the
/// epoch-reload manifest fetch returned) are re-announced under their
/// existing uuids (hub § "Epoch change"; plan P25). Thresholds are
/// prospective: the current gate version stamps every re-announced frame,
/// same as any other announce.
///
/// I5 fix round (controller ruling, overrides the brief snippet): only a
/// frame this device actually HOLDS (`LocalState::OwnHeld`) is eligible.
/// `origin == Own` alone included `own_missing` rows — the hub knows about
/// them, but this device has no bytes for them — and re-announcing one would
/// make the hub write an implicit claim for content this device cannot
/// serve. A 409 "already announced" names specific uuids (R8a's pattern);
/// this drops just those and retries the rest, rather than abandoning the
/// whole batch over one frame that turned out not to be lost after all.
pub(crate) async fn reannounce_lost_own_frames(
    ctx: &ServiceContext,
    client: &CollabClient,
    token: &str,
    project_id: &str,
    seen: &HashSet<String>,
) -> Result<usize, ApiError> {
    use crate::db::collab_frames::{self as frames_db, LocalState};
    let (lost, gate): (Vec<FrameInWire>, i32) = {
        let database = db(ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
        let own = crate::api::collab_exchange::own_devices(ctx, &conn, &project)?;
        let lost: Vec<FrameInWire> = frames_db::list_for_project(&conn, project_id)?
            .into_iter()
            .filter(|r| r.local_state == LocalState::OwnHeld && !seen.contains(&r.frame_uuid))
            .filter_map(|r| {
                crate::api::collab_exchange::parse_manifest_wire(
                    project_id,
                    &r.frame_uuid,
                    &r.manifest_json,
                    "reannounce",
                )
            })
            // A6: only frames THIS device published (or a device it
            // replaced); another device's frames are that device's to
            // re-announce.
            .filter(|v| {
                let mine = v
                    .publisher_device_id
                    .as_deref()
                    .is_none_or(|d| own.is_mine(d, &v.publisher_account_id));
                if !mine {
                    tracing::debug!(project_id, frame_uuid = %v.frame_uuid, "re-announce: frame published by another device; skipped");
                }
                mine
            })
            .map(|v| FrameInWire {
                frame_uuid: v.frame_uuid,
                file_name: v.file_name,
                blake3: v.blake3,
                byte_size: v.byte_size,
                xxh3: v.xxh3,
                filter_raw: v.filter_raw,
                filter_canonical: v.filter_canonical,
                channel: v.channel,
                exptime_sec: v.exptime_sec,
                date_obs: v.date_obs,
                gate_version: 0,
                meta: v.meta,
            })
            .collect();
        (lost, project.thresholds_version.unwrap_or(0))
    };
    let mut announced = 0usize;
    for mut batch in crate::api::collab::announce_batches(lost) {
        for f in batch.iter_mut() {
            // Thresholds are prospective (v3 §10, plan P25): stamp the
            // current gate version, same as a first-time announce.
            f.gate_version = gate;
        }
        // Final fix D-18: in flight until the implicit claims are recorded.
        let _claim_calls = crate::api::collab_live::holdings::ClaimCalls::begin(
            ctx,
            project_id,
            batch.iter().map(|f| f.frame_uuid.clone()),
        );
        while !batch.is_empty() {
            match crate::collab::hub_client::with_retry(
                "reannounce",
                crate::collab::hub_client::RetryPolicy::Background,
                || client.announce_frames(token, project_id, &batch),
            )
            .await
            {
                Ok(resp) => {
                    let database = db(ctx)?;
                    let conn = database.conn();
                    for f in &batch {
                        crate::db::collab_live::add_implicit_claim(
                            &conn,
                            project_id,
                            &f.frame_uuid,
                            1,
                        )?;
                    }
                    announced += resp.announced;
                    break;
                }
                Err(e)
                    if e.hub_text()
                        .is_some_and(|m| m.contains("already announced")) =>
                {
                    let already = already_announced_indices(&e, &batch);
                    if already.is_empty() {
                        tracing::warn!(project_id, error = %e, "already-announced refusal named no uuid in this batch; giving up on it");
                        break;
                    }
                    tracing::info!(
                        project_id,
                        count = already.len(),
                        "own frames already back on the hub; retrying the rest of the batch"
                    );
                    for &i in already.iter().rev() {
                        batch.remove(i);
                    }
                }
                Err(crate::account::AccountClientError::PublishingDevice {
                    device_id,
                    device_name,
                }) => {
                    // A6: another device of this account publishes into the
                    // project now; only it may announce. Recorded (the
                    // publish path stops on it too) and NOT an error — an
                    // error would keep the epoch reload failing and retrying.
                    tracing::warn!(
                        project_id,
                        count = batch.len(),
                        bound_device_id = %device_id,
                        outcome = "publishing_device",
                        "re-announce refused: another device of the account publishes into this project"
                    );
                    let database = db(ctx)?;
                    let conn = database.conn();
                    crate::db::collab::set_publishing_device(
                        &conn,
                        project_id,
                        Some(&crate::db::collab::PublishingDevice {
                            device_id,
                            name: device_name,
                        }),
                    )?;
                    // Fix round 2: a move of the binding to this device
                    // runs this check again (`after_binding_moved_here`).
                    crate::db::collab_live::set_reannounce_refused(&conn, project_id, true)?;
                    return Ok(announced);
                }
                Err(e) => {
                    tracing::error!(project_id, count = batch.len(), error = %e, "re-announce of own frames failed");
                    return Err(crate::api::collab_exchange::client_err(e));
                }
            }
        }
    }
    if announced > 0 {
        tracing::warn!(
            project_id,
            count = announced,
            "own frames re-announced after an epoch change"
        );
    }
    Ok(announced)
}

/// A6 fix round 1: the binding moved TO this device (a switch, or a
/// `/me/projects` refresh that shows it here). While another device was
/// bound, an epoch-change re-announce of this device's frames was refused
/// (recorded, not retried); run the same check now — one full manifest
/// fetch, then every own frame the hub no longer lists is re-announced.
/// Signed out → nothing to do.
pub(crate) async fn reannounce_after_rebind(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<usize, ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok(0);
    };
    let client = CollabClient::new(&hub_url).map_err(crate::api::collab_exchange::client_err)?;
    let (seen, _) =
        crate::api::collab_exchange::sync_manifest_full(ctx, project_id, None, None).await?;
    let n = reannounce_lost_own_frames(ctx, &client, &token, project_id, &seen).await?;
    tracing::info!(
        project_id,
        count = n,
        "own frames checked after the binding moved here"
    );
    Ok(n)
}

/// Fix round 2: whether a move of the binding TO this device must run
/// [`reannounce_after_rebind`] — only when the previous binding named
/// ANOTHER device (`None → me`, the first refresh after the upgrade, never
/// does) or an epoch re-announce was refused meanwhile (the mark).
pub(crate) fn rebind_needs_reannounce(
    ctx: &ServiceContext,
    project_id: &str,
    previous: Option<&str>,
    me: &str,
) -> bool {
    if previous.is_some_and(|p| p != me) {
        return true;
    }
    match db(ctx).and_then(|d| {
        crate::db::collab_live::reannounce_refused(&d.conn(), project_id).map_err(ApiError::from)
    }) {
        Ok(refused) => refused,
        Err(e) => {
            tracing::warn!(project_id, error = %e, "reading the refused re-announce mark failed; checking anyway");
            true
        }
    }
}

/// [`reannounce_after_rebind`] when [`rebind_needs_reannounce`] says so.
/// The refused mark is cleared before the check and set again by a refusal
/// inside it or by a failure. Never fails the caller (a switch or a
/// refresh): a failure is logged.
pub(crate) async fn after_binding_moved_here(
    ctx: &ServiceContext,
    project_id: &str,
    previous: Option<&str>,
    me: &str,
) {
    if !rebind_needs_reannounce(ctx, project_id, previous, me) {
        tracing::debug!(
            project_id,
            "the binding moved here from no device; no re-announce check"
        );
        return;
    }
    // Fix round 3 (m1): the mark is cleared BEFORE the check. A refusal
    // inside it (the binding moved away again meanwhile) sets it again
    // itself; a failure sets it again here — it never outlives or loses a
    // refusal.
    let mark = |refused: bool| {
        let written = db(ctx).and_then(|d| {
            crate::db::collab_live::set_reannounce_refused(&d.conn(), project_id, refused)
                .map_err(ApiError::from)
        });
        if let Err(e) = written {
            tracing::warn!(project_id, refused, error = %e, "writing the refused re-announce mark failed");
        }
    };
    mark(false);
    if let Err(e) = reannounce_after_rebind(ctx, project_id).await {
        tracing::warn!(project_id, error = %e, "re-announce check after the binding moved here failed");
        mark(true);
    }
}

/// Indices of `batch` a 409 "already announced" refusal names — R8a's
/// pattern (`api::collab::already_announced_in`), adapted to a re-announce
/// batch's [`FrameInWire`] shape.
fn already_announced_indices(
    e: &crate::account::AccountClientError,
    batch: &[FrameInWire],
) -> Vec<usize> {
    let Some(m) = e.hub_text() else {
        return Vec::new();
    };
    batch
        .iter()
        .enumerate()
        .filter(|(_, f)| m.contains(&format!("frame {} already announced", f.frame_uuid)))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::FakeHub;
    use crate::collab::live::wire::{
        AccountEvent, AccountKind, ChangeKind, HoldersEvent, LiveEvent, PresenceEvent,
        ProjectEvent, VersionsEvent,
    };
    use crate::db::collab_frames::LocalState;

    struct NoHolders(Vec<String>);
    #[async_trait::async_trait]
    impl HolderSide for NoHolders {
        async fn on_hello_project(
            &mut self,
            pid: &str,
            _: &HelloProject,
            plan: HolderPlan,
            _: &str,
        ) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("hello {pid} {plan:?}"));
            Ok(vec![])
        }
        async fn on_holders_event(
            &mut self,
            ev: &HoldersEvent,
            _: &str,
        ) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("holders {}", ev.seq));
            Ok(vec![])
        }
        async fn catch_up(&mut self, pid: &str, _: &str) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("catch_up {pid}"));
            Ok(vec![])
        }
        async fn reload(&mut self, pid: &str, _: &str) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("reload {pid}"));
            Ok(vec![])
        }
        fn forget(&mut self, pid: &str) {
            self.0.push(format!("forget {pid}"));
        }
    }

    /// Records what the DB looked like at the moment `reload` was called —
    /// item 2's contract: `holder_seq` already reset to -1, `feed_epoch`
    /// still the OLD value (the final write hasn't happened yet).
    struct RecordingHolders(Vec<String>, Arc<ServiceContext>);
    #[async_trait::async_trait]
    impl HolderSide for RecordingHolders {
        async fn on_hello_project(
            &mut self,
            pid: &str,
            _: &HelloProject,
            plan: HolderPlan,
            _: &str,
        ) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("hello {pid} {plan:?}"));
            Ok(vec![])
        }
        async fn on_holders_event(
            &mut self,
            ev: &HoldersEvent,
            _: &str,
        ) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("holders {}", ev.seq));
            Ok(vec![])
        }
        async fn catch_up(&mut self, pid: &str, _: &str) -> Result<Vec<FeedEffect>, ApiError> {
            self.0.push(format!("catch_up {pid}"));
            Ok(vec![])
        }
        async fn reload(
            &mut self,
            pid: &str,
            epoch_arg: &str,
        ) -> Result<Vec<FeedEffect>, ApiError> {
            let (stored_epoch, _v, holder_seq) = full_cursor(&self.1, pid);
            self.0.push(format!(
                "reload {pid} epoch_arg={epoch_arg} stored_epoch={stored_epoch:?} holder_seq={holder_seq}"
            ));
            // Item 4/5 fix round 3: the HolderSide contract — persist the
            // seq ONLY through the seq-only setter, never `set_holder_seq`
            // (which would also stamp `feed_epoch`). This write must SURVIVE
            // whatever `reload_one_project` does after `reload` returns.
            let conn = crate::api::db(&self.1).unwrap().conn();
            crate::db::collab::set_holder_seq_only(&conn, pid, 42).unwrap();
            Ok(vec![])
        }
        fn forget(&mut self, pid: &str) {
            self.0.push(format!("forget {pid}"));
        }
    }

    const PID: &str = "p1";

    async fn rig() -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub, FeedApplier) {
        let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let ctx = Arc::new(ctx);
        let hub = FakeHub::start().await;
        // A6: `own` is derived per device, so the hub must know this
        // device's real key for "acc-me"'s frames to be own here.
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.add_account("tok", "acc-me", "Me", &me, None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project(
            PID,
            "m31",
            &[("acc-me", "send_receive", false), ("acc-o", "send", false)],
            false,
        );
        crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), "tok");
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        let client = CollabClient::new(hub.uri()).unwrap();
        let f = FeedApplier::new(Arc::clone(&ctx), client, "tok".into(), None);
        (tmp, ctx, hub, f)
    }

    async fn manifest_requests(hub: &FakeHub) -> usize {
        hub.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().ends_with("/manifest"))
            .count()
    }

    async fn manifest_requests_for(hub: &FakeHub, project_id: &str) -> usize {
        let suffix = format!("/projects/{project_id}/manifest");
        hub.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().ends_with(&suffix))
            .count()
    }

    /// The `since` query parameter of every manifest request for
    /// `project_id`, in the order the hub received them. A full reload's
    /// first page is `since = 0`; an incremental delta starts at the stored
    /// `manifest_cursor`.
    async fn manifest_sinces_for(hub: &FakeHub, project_id: &str) -> Vec<i64> {
        let suffix = format!("/projects/{project_id}/manifest");
        hub.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().ends_with(&suffix))
            .map(|r| {
                r.url
                    .query_pairs()
                    .find(|(k, _)| k == "since")
                    .and_then(|(_, v)| v.parse().ok())
                    .expect("every manifest request carries a numeric since")
            })
            .collect()
    }

    fn manifest_cursor(ctx: &ServiceContext, project_id: &str) -> i64 {
        let conn = crate::api::db(ctx).unwrap().conn();
        crate::db::collab::get_project(&conn, project_id)
            .unwrap()
            .unwrap()
            .manifest_cursor
    }

    fn cursor(ctx: &ServiceContext) -> (Option<String>, i64) {
        let conn = crate::api::db(ctx).unwrap().conn();
        let p = crate::db::collab::get_project(&conn, PID).unwrap().unwrap();
        (p.feed_epoch, p.hub_version)
    }

    fn project_cursor(ctx: &ServiceContext, project_id: &str) -> (Option<String>, i64) {
        let conn = crate::api::db(ctx).unwrap().conn();
        let p = crate::db::collab::get_project(&conn, project_id)
            .unwrap()
            .unwrap();
        (p.feed_epoch, p.hub_version)
    }

    fn full_cursor(ctx: &ServiceContext, project_id: &str) -> (Option<String>, i64, i64) {
        let conn = crate::api::db(ctx).unwrap().conn();
        let p = crate::db::collab::get_project(&conn, project_id)
            .unwrap()
            .unwrap();
        (p.feed_epoch, p.hub_version, p.holder_seq)
    }

    /// `membership_version` is written only by the small-document refresh
    /// (`refresh_projects_reporting`/`upsert_project`) — `sync_manifest_full`
    /// never touches it. A change here is direct evidence that a reload
    /// actually ran the small-document refresh, not just the frames fetch.
    fn membership_version(ctx: &ServiceContext, project_id: &str) -> i64 {
        let conn = crate::api::db(ctx).unwrap().conn();
        crate::db::collab::get_project(&conn, project_id)
            .unwrap()
            .unwrap()
            .membership_version
    }

    fn set_own_held(ctx: &ServiceContext, project_id: &str, frame_uuid: &str) {
        let conn = crate::api::db(ctx).unwrap().conn();
        crate::db::collab_frames::set_local_state(
            &conn,
            project_id,
            frame_uuid,
            LocalState::OwnHeld,
        )
        .unwrap();
    }

    fn hello(hub: &FakeHub, epoch: &str) -> crate::collab::live::wire::HelloEvent {
        let mut v = hub.hello_for("tok");
        v["epoch"] = serde_json::Value::String(epoch.to_string());
        serde_json::from_value(v).unwrap()
    }

    fn project_event_from_hub(hub: &FakeHub, project_id: &str) -> ProjectEvent {
        let v = hub
            .last_event("project", project_id)
            .expect("fake hub published no project event for this project");
        serde_json::from_value(v).unwrap()
    }

    #[tokio::test]
    async fn inline_frames_apply_without_a_manifest_read() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let before = manifest_requests(&hub).await;
        hub.seed_frames(PID, "acc-o", &["u1"], "published");
        let ev = project_event_from_hub(&hub, PID);
        let effects = f
            .apply(LiveEvent::Project(ev.clone()), &mut h)
            .await
            .unwrap();
        assert_eq!(manifest_requests(&hub).await, before);
        assert!(effects.contains(&FeedEffect::NeedSetChanged(PID.into())));
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, PID, "u1")
            .unwrap()
            .unwrap();
        assert_eq!(row.frame_seq, Some(1));
        drop(conn);
        assert_eq!(cursor(&ctx).1, ev.version);
    }

    #[tokio::test]
    async fn a_gap_catches_up_over_rest_and_an_old_event_is_ignored() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        hub.seed_frames(PID, "acc-o", &["u1"], "published");
        hub.seed_frames(PID, "acc-o", &["u2"], "published");
        let last = project_event_from_hub(&hub, PID); // prev = cursor + 1 -> a gap
        let before = manifest_requests(&hub).await;
        f.apply(LiveEvent::Project(last.clone()), &mut h)
            .await
            .unwrap();
        assert_eq!(manifest_requests(&hub).await, before + 1);
        assert_eq!(cursor(&ctx).1, last.version);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab_frames::get(&conn, PID, "u1")
            .unwrap()
            .is_some());
        drop(conn);
        // replaying it is a no-op
        f.apply(LiveEvent::Project(last), &mut h).await.unwrap();
        assert_eq!(manifest_requests(&hub).await, before + 1);
    }

    /// Another connection takes the catalog's write lock and holds it for
    /// `hold` — as the storage task's scope pass or a landing does
    /// (`BEGIN IMMEDIATE`, milliseconds in the app). Returns once it is held.
    fn hold_write_lock(
        ctx: &ServiceContext,
        hold: std::time::Duration,
    ) -> std::thread::JoinHandle<()> {
        let path = crate::api::db(ctx).unwrap().path().to_path_buf();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(&path).unwrap();
            crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            held_tx.send(()).unwrap();
            std::thread::sleep(hold);
            conn.execute_batch("COMMIT").unwrap();
        });
        held_rx.recv().unwrap();
        writer
    }

    /// The load flake of `a_publish_is_fetched_without_any_poll` (2026-09-27):
    /// a manifest write that began as a read transaction could not take the
    /// write lock another writer held — SQLite answers `SQLITE_BUSY` at once
    /// (no busy wait for a read-to-write upgrade) — so the event was dropped,
    /// and the LAST event of a burst waited for the 60 s versions vector.
    /// The write lock is taken up front: the apply waits the busy timeout.
    #[tokio::test]
    async fn an_inline_apply_waits_for_another_writer_instead_of_dropping_the_event() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        hub.seed_frames(PID, "acc-o", &["u1"], "published");
        let ev = project_event_from_hub(&hub, PID);
        let writer = hold_write_lock(&ctx, std::time::Duration::from_millis(300));
        let effects = f
            .apply(LiveEvent::Project(ev.clone()), &mut h)
            .await
            .expect("the apply waits for the other writer");
        writer.join().unwrap();
        assert!(effects.contains(&FeedEffect::NeedSetChanged(PID.into())));
        assert_eq!(cursor(&ctx).1, ev.version);
    }

    /// As above for the REST catch-up's manifest page write.
    #[tokio::test]
    async fn a_rest_catch_up_waits_for_another_writer_instead_of_dropping_the_event() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        hub.seed_frames(PID, "acc-o", &["u1"], "published");
        hub.seed_frames(PID, "acc-o", &["u2"], "published");
        let last = project_event_from_hub(&hub, PID); // a gap: caught up over REST
        let writer = hold_write_lock(&ctx, std::time::Duration::from_millis(300));
        f.apply(LiveEvent::Project(last.clone()), &mut h)
            .await
            .expect("the catch-up waits for the other writer");
        writer.join().unwrap();
        assert_eq!(cursor(&ctx).1, last.version);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab_frames::get(&conn, PID, "u2")
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn members_kind_refreshes_the_snapshot_and_reports_it() {
        let (_t, _ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        hub.add_member(PID, "acc-x", "send_receive", false);
        let ev = project_event_from_hub(&hub, PID);
        assert!(ev.kinds.contains(&ChangeKind::Members));
        let effects = f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert!(effects.contains(&FeedEffect::MembersChanged(PID.into())));
    }

    /// Task 15 (replaces the wave-2 poll's `threshold_move_calls_the_hook`
    /// and `dictionary_move_is_refetched_and_calls_the_hook`): a thresholds
    /// or dictionary move reaching the feed fires the gate hook (the
    /// auto-publish trigger) once per move.
    #[tokio::test]
    async fn a_thresholds_or_dictionary_move_fires_the_gate_hook() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let _ = crate::api::collab::take_gate_moves_seen();
        hub.set_thresholds_version(PID, 3);
        let ev = project_event_from_hub(&hub, PID);
        assert!(ev.kinds.contains(&ChangeKind::Thresholds));
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert_eq!(
            crate::api::collab::take_gate_moves_seen(),
            vec![PID.to_string()]
        );
        hub.set_dictionary(PID, 2, crate::collab::fake_hub::default_dictionary());
        let ev = project_event_from_hub(&hub, PID);
        assert!(ev.kinds.contains(&ChangeKind::Dictionary));
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert_eq!(
            crate::api::collab::take_gate_moves_seen(),
            vec![PID.to_string()]
        );
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab::get_project(&conn, PID)
                .unwrap()
                .unwrap()
                .dictionary_version,
            Some(2),
            "the dictionary is refetched"
        );
    }

    #[tokio::test]
    async fn account_left_marks_the_project_lost_and_forgets_its_live_state() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        hub.remove_member(PID, "acc-me");
        let effects = f
            .apply(
                LiveEvent::Account(AccountEvent {
                    kind: AccountKind::Left,
                    project_id: PID.into(),
                }),
                &mut h,
            )
            .await
            .unwrap();
        assert!(effects.contains(&FeedEffect::ProjectGone(PID.into())));
        assert!(h.0.contains(&"forget p1".to_string()));
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab::lost_at(&conn, PID).unwrap().is_some());
    }

    /// A6 fix round 1: an epoch change while ANOTHER device of this account
    /// is bound — the re-announce is refused (recorded, the epoch still
    /// moves, nothing retried); when the binding moves back to this device
    /// ("Publish from this device"), the lost own frame is re-announced.
    #[tokio::test]
    async fn a_lost_own_frame_is_reannounced_once_the_binding_moves_here() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.seed_frames(PID, "acc-me", &["own1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        f.catch_up_project(PID, None, &[ChangeKind::Frames])
            .await
            .unwrap();
        set_own_held(&ctx, PID, "own1");
        // Another device of the account is bound (in service).
        hub.add_account("tok-2", "acc-me", "Me", "T1RIRVI=", None);
        hub.add_device("acc-me", "T1RIRVI=", "dev-2", "Laptop", None);
        hub.set_publishing_device(PID, "acc-me", "T1RIRVI=");
        hub.forget_frames(PID, &["own1"]);
        let e2 = hub.rotate_epoch();
        let effects = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        assert!(hub.frame(PID, "own1").is_none(), "refused while not bound");
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert_eq!(
                crate::db::collab::publishing_device(&conn, PID)
                    .unwrap()
                    .map(|d| d.device_id),
                Some("T1RIRVI=".to_string()),
                "the refusal is recorded"
            );
        }
        let card = crate::api::collab::set_collab_publishing_device(&ctx, PID)
            .await
            .unwrap();
        assert!(card.publishing_here);
        let back = hub
            .frame(PID, "own1")
            .expect("re-announced after the switch");
        assert_eq!(back.publisher_device_id.as_deref(), Some(me.as_str()));
    }

    /// A6 fix round 2 (M1): the first refresh after the upgrade that shows
    /// the binding here (nothing cached → this device) runs NO full
    /// manifest sync — nothing was refused, nothing can be lost.
    #[tokio::test]
    async fn a_binding_moving_here_from_no_device_runs_no_full_sync() {
        let (_t, ctx, hub, _f) = rig().await;
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.set_publishing_device(PID, "acc-me", &me);
        let before = manifest_requests(&hub).await;
        let card = crate::api::collab::refresh_projects(&ctx)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.project_id == PID)
            .unwrap();
        assert!(card.publishing_here);
        assert_eq!(
            manifest_requests(&hub).await,
            before,
            "no re-announce check"
        );
    }

    /// A6 fix round 2 (M1): a re-announce refused while another device was
    /// bound is marked; the other device is revoked (the binding reads
    /// unbound), then this device is bound — a move from NO device, which
    /// still runs the check because of the mark, and clears it.
    #[tokio::test]
    async fn a_refused_reannounce_is_checked_again_when_the_binding_comes_here_from_none() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.seed_frames(PID, "acc-me", &["own1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        f.catch_up_project(PID, None, &[ChangeKind::Frames])
            .await
            .unwrap();
        set_own_held(&ctx, PID, "own1");
        hub.add_account("tok-2", "acc-me", "Me", "T1RIRVI=", None);
        hub.set_publishing_device(PID, "acc-me", "T1RIRVI=");
        hub.forget_frames(PID, &["own1"]);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(hub.frame(PID, "own1").is_none(), "refused while not bound");
        let marked = || {
            crate::db::collab_live::reannounce_refused(&crate::api::db(&ctx).unwrap().conn(), PID)
                .unwrap()
        };
        assert!(marked(), "the refusal is marked");
        // The other device is revoked: unbound (another device → none).
        hub.revoke_device("T1RIRVI=", true);
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        assert!(hub.frame(PID, "own1").is_none(), "not a move here");
        // This device is bound (none → this device): the mark runs the check.
        hub.set_publishing_device(PID, "acc-me", &me);
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        assert_eq!(
            hub.frame(PID, "own1")
                .expect("re-announced")
                .publisher_device_id
                .as_deref(),
            Some(me.as_str())
        );
        assert!(!marked(), "cleared after the check");
    }

    /// A6 fix round 3 (m1): the mark survives a refused check — the check
    /// runs (the mark) while another device is bound again, is refused, and
    /// the mark stays; a later none → this device still runs it.
    #[tokio::test]
    async fn the_refused_mark_survives_a_refused_check() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        let me = crate::api::account::own_device_id(&ctx).unwrap();
        hub.seed_frames(PID, "acc-me", &["own1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        f.catch_up_project(PID, None, &[ChangeKind::Frames])
            .await
            .unwrap();
        set_own_held(&ctx, PID, "own1");
        hub.add_account("tok-2", "acc-me", "Me", "T1RIRVI=", None);
        hub.set_publishing_device(PID, "acc-me", "T1RIRVI=");
        hub.forget_frames(PID, &["own1"]);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        let marked = || {
            crate::db::collab_live::reannounce_refused(&crate::api::db(&ctx).unwrap().conn(), PID)
                .unwrap()
        };
        assert!(marked());
        // The binding "came here" but moved away again before the check ran.
        after_binding_moved_here(&ctx, PID, None, &me).await;
        assert!(hub.frame(PID, "own1").is_none(), "refused again");
        assert!(marked(), "the mark survives a refused check");
        // Later: the other device is revoked, then this device is bound.
        hub.revoke_device("T1RIRVI=", true);
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        hub.set_publishing_device(PID, "acc-me", &me);
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        assert!(hub.frame(PID, "own1").is_some(), "the check ran");
        assert!(!marked());
    }

    #[tokio::test]
    async fn an_epoch_change_refetches_everything_and_reannounces_lost_own_frames() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        // an own frame the hub knows, actually held on this device's disk
        hub.seed_frames(PID, "acc-me", &["own1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        f.catch_up_project(PID, None, &[ChangeKind::Frames])
            .await
            .unwrap();
        set_own_held(&ctx, PID, "own1");
        // the hub is restored without it
        hub.forget_frames(PID, &["own1"]);
        let e2 = hub.rotate_epoch();
        let effects = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        assert!(h.0.contains(&"reload p1".to_string()));
        assert!(
            hub.frame(PID, "own1").is_some(),
            "re-announced under the same uuid"
        );
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab_live::meta_get(&conn, crate::db::collab_live::META_EPOCH)
                .unwrap()
                .as_deref(),
            Some(e2.as_str())
        );
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
    }

    /// I1 fix round: a failure partway through one project's epoch reload
    /// must not be mistaken for done. Its cursor stays on the OLD epoch, so
    /// the very next hello (even carrying the SAME new epoch again) retries
    /// the whole reload, never a lightweight delta catch-up against a
    /// manifest cursor from before the restore.
    #[tokio::test]
    async fn an_epoch_change_failure_is_retried_on_the_next_hello() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.seed_frames(PID, "acc-me", &["own1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        set_own_held(&ctx, PID, "own1");
        hub.forget_frames(PID, &["own1"]);
        let e2 = hub.rotate_epoch();

        hub.set_failing("/manifest", true);
        let effects = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(
            effects.contains(&FeedEffect::EpochChanged),
            "the epoch change itself is still reported"
        );
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some("e1"),
            "the failed reload never moved the cursor into the new epoch"
        );
        assert!(
            hub.frame(PID, "own1").is_none(),
            "not re-announced yet — the manifest fetch failed first"
        );

        hub.set_failing("/manifest", false);
        let effects2 = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(
            effects2.contains(&FeedEffect::EpochChanged),
            "retried on the next hello, not silently skipped"
        );
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
        assert!(
            hub.frame(PID, "own1").is_some(),
            "re-announced once the retry succeeded"
        );
    }

    /// I2 fix round: a project whose epoch reload fails PERMANENTLY must
    /// never block a healthy sibling project's reload, and every hello must
    /// still return normally (the feed keeps going live) instead of failing
    /// outright forever.
    #[tokio::test]
    async fn epoch_change_isolates_a_permanently_failing_project() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.add_project("p2", "m42", &[("acc-me", "send_receive", false)], false);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        assert_eq!(project_cursor(&ctx, "p2").0.as_deref(), Some("e1"));

        hub.set_failing("/projects/p2/manifest", true);
        let e2 = hub.rotate_epoch();
        let effects = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some(e2.as_str()),
            "the healthy project (p1) still reached the new epoch"
        );
        assert_eq!(
            project_cursor(&ctx, "p2").0.as_deref(),
            Some("e1"),
            "the permanently failing project (p2) stays on the old epoch, retried later"
        );
    }

    /// Item 4 fix round 2: once a project has actually reached the new
    /// epoch, a REPEAT epoch-change trigger (because a sibling is still
    /// stuck) must not redo its since=0 fetch + prune + snapshot +
    /// re-announce all over again.
    #[tokio::test]
    async fn epoch_change_skips_a_project_already_on_the_new_epoch() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.add_project("p2", "m42", &[("acc-me", "send_receive", false)], false);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();

        hub.set_failing("/projects/p2/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some(e2.as_str()),
            "p1 reached e2"
        );
        assert_eq!(project_cursor(&ctx, "p2").0.as_deref(), Some("e1"));

        let before_p1 = manifest_requests_for(&hub, PID).await;
        let effects = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(
            effects.contains(&FeedEffect::EpochChanged),
            "still reported — p2 needs it"
        );
        assert_eq!(
            manifest_requests_for(&hub, PID).await,
            before_p1,
            "p1 (already on the new epoch) is not redundantly reloaded"
        );
        assert_eq!(
            project_cursor(&ctx, "p2").0.as_deref(),
            Some("e1"),
            "p2 is retried again — still failing"
        );
    }

    /// Item 1 fix round 2: after ITS OWN epoch reload has already failed
    /// (stuck on the old epoch while the session moved on), a project event
    /// naming a version the stale cursor could easily look "contiguous"
    /// against must still be routed to a full reload, never a delta that
    /// would reopen C1 against a pre-restore manifest cursor.
    #[tokio::test]
    async fn a_project_event_for_an_epoch_stuck_project_reloads_fully() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let (_, stale_version) = cursor(&ctx);

        hub.set_failing("/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some("e1"),
            "p1's own reload failed and never moved"
        );
        hub.set_failing("/manifest", false);

        // Now a normal `project` event arrives, with a head at or above the
        // stale version — exactly the shape a plain `step()` comparison
        // could mistake for a legitimate contiguous (or gap) delta.
        hub.seed_frames(PID, "acc-o", &["u1"], "published");
        let ev = project_event_from_hub(&hub, PID);
        assert!(ev.version >= stale_version);
        let before = manifest_requests_for(&hub, PID).await;
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert!(
            manifest_requests_for(&hub, PID).await > before,
            "routed to a full reload (a manifest fetch happened), never a bare delta"
        );
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some(e2.as_str()),
            "the project event's own reload brought it up to the session's real epoch"
        );
    }

    /// Item 2 fix round 2, tightened in fix round 3 item 4: the holder-seq
    /// reset happens before `holders.reload` runs, and touches ONLY
    /// `holder_seq` — `feed_epoch` is still the OLD value at that moment.
    /// `feed_epoch` moves only in the final write, after `reload` (and the
    /// re-announce) already succeeded — and that final write must never
    /// clobber a seq the holder side itself already persisted during
    /// `reload` (item 5's seq-only-setter contract).
    ///
    /// This test is seeded with a REAL (non-default) `holder_seq` first, so
    /// the `holder_seq=-1` assertion below actually proves the reset ran:
    /// the schema default is already -1, so without seeding, that assertion
    /// would pass even with `FeedApplier::reload_one_project`'s
    /// `reset_holder_seq` call deleted outright. Verified by hand: removing
    /// that call makes this test fail with `holder_seq=7` in the recorded
    /// string instead of `-1` (see the fix round 3 report).
    #[tokio::test]
    async fn epoch_reload_resets_holder_seq_before_reload_without_moving_the_epoch_early() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = RecordingHolders(vec![], Arc::clone(&ctx));
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_holder_seq(&conn, PID, "e1", 7).unwrap();
        }
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        let seen =
            h.0.iter()
                .find(|s| s.starts_with("reload p1"))
                .cloned()
                .expect("holders.reload was called for p1");
        assert!(
            seen.contains("stored_epoch=Some(\"e1\")"),
            "feed_epoch had NOT moved yet when reload ran: {seen}"
        );
        assert!(
            seen.contains("holder_seq=-1"),
            "holder_seq was reset from 7 to -1 before reload ran: {seen}"
        );
        // The final state: the epoch moved, AND the holder side's own seq
        // write (42, made from inside `reload`) survived — nothing after
        // `reload` returns touches `holder_seq` again.
        let (final_epoch, _, final_holder_seq) = full_cursor(&ctx, PID);
        assert_eq!(final_epoch.as_deref(), Some(e2.as_str()));
        assert_eq!(
            final_holder_seq, 42,
            "the holder side's own seq write survives — no clobber"
        );
    }

    #[tokio::test]
    async fn versions_vector_catches_up_or_changes_epoch() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let (_, v) = cursor(&ctx);
        let mut vv = VersionsEvent::new();
        vv.insert(PID.into(), (v + 3, 0));
        hub.seed_frames(PID, "acc-o", &["u5"], "published");
        f.apply(LiveEvent::Versions(vv), &mut h).await.unwrap();
        assert_eq!(
            cursor(&ctx).1,
            hub.version(PID),
            "the cursor lands on the hub's real version, never the event's manufactured head"
        );
    }

    /// M2 fix round: a `versions` head BELOW the stored cursor is an epoch
    /// change (hub § "Cursor rules"), not merely ignored — even with no
    /// epoch string change to report (a versions vector never carries one).
    #[tokio::test]
    async fn versions_vector_head_below_cursor_is_an_epoch_change() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.seed_frames(PID, "acc-o", &["u1"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let (_, v) = cursor(&ctx);
        assert!(v > 0);
        let mut vv = VersionsEvent::new();
        vv.insert(PID.into(), (v - 1, 0));
        let effects = f.apply(LiveEvent::Versions(vv), &mut h).await.unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        assert!(h.0.contains(&"reload p1".to_string()));
    }

    /// Item 1/4 fix round 2: a versions tick for a project already stuck on
    /// a different epoch must reload just that project, never fall through
    /// to `plan_versions` (which only compares raw numbers) and never widen
    /// into a global epoch change that would redo a healthy sibling too.
    #[tokio::test]
    async fn versions_tick_reloads_an_epoch_stuck_project_without_touching_a_healthy_sibling() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.add_project("p2", "m42", &[("acc-me", "send_receive", false)], false);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();

        hub.set_failing("/projects/p2/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
        assert_eq!(project_cursor(&ctx, "p2").0.as_deref(), Some("e1"));
        hub.set_failing("/projects/p2/manifest", false);

        let before_p1 = manifest_requests_for(&hub, PID).await;
        let (_, p1_version) = cursor(&ctx);
        let (_, p2_version) = project_cursor(&ctx, "p2");
        let mut vv = VersionsEvent::new();
        vv.insert(PID.into(), (p1_version, 0));
        vv.insert("p2".into(), (p2_version, 0));
        let effects = f.apply(LiveEvent::Versions(vv), &mut h).await.unwrap();
        assert!(
            !effects.contains(&FeedEffect::EpochChanged),
            "no GLOBAL epoch change — only p2's own per-project reload"
        );
        assert_eq!(
            manifest_requests_for(&hub, PID).await,
            before_p1,
            "the healthy sibling p1 is untouched"
        );
        assert_eq!(
            project_cursor(&ctx, "p2").0.as_deref(),
            Some(e2.as_str()),
            "p2 reloaded and reached the session's real epoch"
        );
    }

    /// A `presence` event with no candidacy flip is a no-op effect (the
    /// applier's own dispatch, not [`PresenceBook`]'s own test coverage).
    #[tokio::test]
    async fn a_presence_event_with_no_candidacy_change_produces_no_effect() {
        let (_t, _ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let effects = f
            .apply(
                LiveEvent::Presence(PresenceEvent {
                    project_id: PID.into(),
                    replace: false,
                    changes: vec![],
                }),
                &mut h,
            )
            .await
            .unwrap();
        assert!(effects.is_empty());
    }

    /// I4 fix round (item 7's missing test): a `members` event that narrows
    /// this device's OWN governance caps must prune a now-invisible pending
    /// row in the SAME apply — `apply_contiguous`'s caps-changed fallback,
    /// not merely record the new caps and leave stale rows cached.
    #[tokio::test]
    async fn a_members_event_that_narrows_my_caps_prunes_in_the_same_apply() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.set_caps(PID, "acc-me", &["data.moderate"]);
        hub.seed_frames(PID, "acc-o", &["pending1"], "pending");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab_frames::get(&conn, PID, "pending1")
            .unwrap()
            .is_some());
        drop(conn);

        hub.set_caps(PID, "acc-me", &[]);
        let ev = project_event_from_hub(&hub, PID);
        assert!(ev.kinds.contains(&ChangeKind::Members));
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();

        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(
            crate::db::collab_frames::get(&conn, PID, "pending1")
                .unwrap()
                .is_none(),
            "the pending row is pruned in the same pass the caps narrowed"
        );
    }

    /// Item 3 fix round 2: `catch_up_project`'s own small-document refresh
    /// can be what FIRST reveals a caps change (not known beforehand); the
    /// caps rule's full fetch + prune must still run, in the same call,
    /// before the cursor advances.
    #[tokio::test]
    async fn catch_up_project_prunes_when_its_own_small_document_refresh_reveals_a_caps_change() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.set_caps(PID, "acc-me", &["data.moderate"]);
        hub.seed_frames(PID, "acc-o", &["pending1"], "pending");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(crate::db::collab_frames::get(&conn, PID, "pending1")
            .unwrap()
            .is_some());
        drop(conn);

        // Narrow caps AND publish another frame, forcing a GAP (Step::CatchUp)
        // rather than one contiguous Apply — exercising catch_up_project's
        // own path (ALL_KINDS), not apply_contiguous's.
        hub.set_caps(PID, "acc-me", &[]);
        hub.seed_frames(PID, "acc-o", &["another"], "published");
        let ev = project_event_from_hub(&hub, PID); // prev = cursor + 2 -> a gap
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();

        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(
            crate::db::collab_frames::get(&conn, PID, "pending1")
                .unwrap()
                .is_none(),
            "pruned once the small-document refresh revealed the caps change"
        );
        assert!(crate::db::collab_frames::get(&conn, PID, "another")
            .unwrap()
            .is_some());
    }

    /// I5 fix round: an `own_missing` row (the hub knows it, this device has
    /// no bytes for it) must never be re-announced — only `OwnHeld` rows.
    #[tokio::test]
    async fn reannounce_only_lifts_locally_held_own_frames() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.seed_frames(
            PID,
            "acc-me",
            &["own_missing_one", "own_held_one"],
            "published",
        );
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        set_own_held(&ctx, PID, "own_held_one");
        hub.forget_frames(PID, &["own_missing_one", "own_held_one"]);

        let client = CollabClient::new(hub.uri()).unwrap();
        let announced = reannounce_lost_own_frames(&ctx, &client, "tok", PID, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(announced, 1);
        assert!(
            hub.frame(PID, "own_held_one").is_some(),
            "the held frame is re-announced"
        );
        assert!(
            hub.frame(PID, "own_missing_one").is_none(),
            "an own_missing row is never re-announced — this device cannot serve it"
        );
    }

    /// I5 fix round: a 409 "already announced" for one uuid in the batch
    /// must not abandon the rest — the truly-lost frame still lands.
    #[tokio::test]
    async fn reannounce_drops_an_already_announced_uuid_and_retries_the_rest() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.seed_frames(PID, "acc-me", &["still_there", "truly_lost"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        set_own_held(&ctx, PID, "still_there");
        set_own_held(&ctx, PID, "truly_lost");
        // Only "truly_lost" is actually forgotten by the hub; "still_there"
        // stays — as if this device's belief that BOTH are lost were stale.
        hub.forget_frames(PID, &["truly_lost"]);

        let client = CollabClient::new(hub.uri()).unwrap();
        let announced = reannounce_lost_own_frames(&ctx, &client, "tok", PID, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(
            announced, 1,
            "the truly-lost frame is re-announced despite sharing a batch with an already-announced one"
        );
        assert!(hub.frame(PID, "truly_lost").is_some());
    }

    /// Item 1 fix round 3, made discriminating in fix round 4 (finding 1):
    /// `resync(project)` for a project stuck on a different epoch must reload
    /// it fully, never a delta against its pre-restore `manifest_cursor` —
    /// otherwise `catch_up_project` would stamp the CURRENT epoch anyway,
    /// losing that project's reload for good. The epoch stamp alone cannot
    /// tell the two paths apart (`epoch_change` already moved the session
    /// epoch, so the unguarded delta stamps it too); what does is the SHAPE
    /// of the work: the stale cursor is seeded non-zero, and only a full
    /// reload asks the hub for `since = 0` and reloads the holder side.
    /// Verified by hand: with the `ResyncWhat::Project` guard removed this
    /// test fails on the `since = 0` assertion.
    #[tokio::test]
    async fn resync_project_for_an_epoch_stuck_project_reloads_fully() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.seed_frames(PID, "acc-o", &["u0"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        assert!(
            manifest_cursor(&ctx, PID) > 0,
            "a non-zero stale cursor, so a delta and a full fetch differ on the wire"
        );

        hub.set_failing("/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some("e1"),
            "stuck on the old epoch"
        );
        hub.set_failing("/manifest", false);

        let before = manifest_sinces_for(&hub, PID).await.len();
        let calls_before = h.0.len();
        f.apply(
            LiveEvent::Resync(crate::collab::live::wire::ResyncEvent {
                project_id: PID.into(),
                what: ResyncWhat::Project,
            }),
            &mut h,
        )
        .await
        .unwrap();
        let sinces = manifest_sinces_for(&hub, PID).await;
        assert_eq!(
            sinces.get(before),
            Some(&0),
            "routed to a full reload (since = 0), never a delta from the stale cursor: {sinces:?}"
        );
        assert!(
            h.0[calls_before..].contains(&"reload p1".to_string()),
            "the holder side was reloaded too: {:?}",
            &h.0[calls_before..]
        );
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some(e2.as_str()),
            "resync(project) brought it up to the session's real epoch"
        );
    }

    /// Item 1 fix round 3: `resync(holders)` for a project stuck on a
    /// different epoch must reload it fully, never a `holders.catch_up`
    /// against its stale holder map.
    #[tokio::test]
    async fn resync_holders_for_an_epoch_stuck_project_reloads_fully_instead_of_a_stale_catch_up() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();

        hub.set_failing("/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(cursor(&ctx).0.as_deref(), Some("e1"));
        hub.set_failing("/manifest", false);

        f.apply(
            LiveEvent::Resync(crate::collab::live::wire::ResyncEvent {
                project_id: PID.into(),
                what: ResyncWhat::Holders,
            }),
            &mut h,
        )
        .await
        .unwrap();
        assert!(
            h.0.contains(&"reload p1".to_string()),
            "routed to a full reload"
        );
        assert!(
            !h.0.iter().any(|s| s == "catch_up p1"),
            "never a stale-map catch-up"
        );
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
    }

    /// Item 6 fix round 3: a `holders` delta event naming a project stuck on
    /// a different epoch must reload it fully, never apply the delta against
    /// its stale holder map.
    #[tokio::test]
    async fn a_holders_event_for_an_epoch_stuck_project_reloads_fully() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();

        hub.set_failing("/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(cursor(&ctx).0.as_deref(), Some("e1"));
        hub.set_failing("/manifest", false);

        let holders_ev = HoldersEvent {
            project_id: PID.into(),
            prev: 0,
            seq: 1,
            deltas: vec![],
        };
        f.apply(LiveEvent::Holders(holders_ev), &mut h)
            .await
            .unwrap();
        assert!(
            h.0.contains(&"reload p1".to_string()),
            "routed to a full reload"
        );
        assert!(
            !h.0.iter().any(|s| s.starts_with("holders ")),
            "never applied against the stale map"
        );
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
    }

    /// Item 2 fix round 3: `plan_hello`/`plan_versions` declare an epoch
    /// change on a holder-seq regression ALONE — the version and even the
    /// epoch string can be unchanged. `epoch_change`'s own skip (fix round 2
    /// item 4) must agree, or such a project would be skipped forever while
    /// `EpochChanged` keeps firing with nothing ever resetting it.
    #[tokio::test]
    async fn epoch_change_reloads_on_a_holder_seq_regression_even_when_the_epoch_and_version_are_unchanged(
    ) {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        // A real (non-negative) holder_seq, as if a snapshot had already
        // loaded once.
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_holder_seq(&conn, PID, "e1", 5).unwrap();
        }
        let (_, v) = cursor(&ctx);
        let mut vv = VersionsEvent::new();
        vv.insert(PID.into(), (v, 2)); // version unchanged, holder_seq regressed 5 -> 2
        let effects = f.apply(LiveEvent::Versions(vv), &mut h).await.unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        let (_, _, holder_seq_after) = full_cursor(&ctx, PID);
        assert_eq!(
            holder_seq_after, -1,
            "actually reloaded (reset by reload_one_project), not silently skipped"
        );
    }

    /// Item 3 fix round 3: a stuck project's own routing event can carry a
    /// Members/Thresholds/Dictionary/Meta change — `reload_one_project` must
    /// still refresh the small documents and report the same effects the
    /// normal (`catch_up_project`) path would, not silently consume the
    /// change while moving the cursor past it.
    #[tokio::test]
    async fn a_stuck_projects_event_carrying_a_members_change_refreshes_it_in_the_same_reload() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();

        hub.set_failing("/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some("e1"),
            "stuck on the old epoch"
        );
        hub.set_failing("/manifest", false);

        let before_membership = membership_version(&ctx, PID);
        hub.add_member(PID, "acc-x", "send_receive", false);
        let ev = project_event_from_hub(&hub, PID);
        assert!(ev.kinds.contains(&ChangeKind::Members));
        let effects = f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert!(
            effects.contains(&FeedEffect::MembersChanged(PID.into())),
            "the members effect is reported even though this project was routed to a full reload"
        );
        assert!(
            membership_version(&ctx, PID) > before_membership,
            "the small-document refresh actually ran inside reload_one_project"
        );
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some(e2.as_str()),
            "also reached the session's real epoch"
        );
    }

    /// Fix round 4, finding 2: an `account: joined` event for a project that
    /// already has a LIVE cache row stuck on a different epoch must reload
    /// it fully (`reload_one_project`), never refresh + delta-catch-up from
    /// its pre-restore `manifest_cursor` (which would stamp the new epoch
    /// and lose the reload for good). The unguarded path also calls
    /// `holders.reload`, so the discriminator is the manifest's `since`.
    #[tokio::test]
    async fn account_joined_for_an_epoch_stuck_live_project_reloads_fully() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        hub.seed_frames(PID, "acc-o", &["u0"], "published");
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        assert!(manifest_cursor(&ctx, PID) > 0);

        hub.set_failing("/manifest", true);
        let e2 = hub.rotate_epoch();
        f.apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert_eq!(
            cursor(&ctx).0.as_deref(),
            Some("e1"),
            "stuck on the old epoch"
        );
        hub.set_failing("/manifest", false);

        let before = manifest_sinces_for(&hub, PID).await.len();
        let effects = f
            .apply(
                LiveEvent::Account(AccountEvent {
                    kind: AccountKind::Joined,
                    project_id: PID.into(),
                }),
                &mut h,
            )
            .await
            .unwrap();
        let sinces = manifest_sinces_for(&hub, PID).await;
        assert_eq!(
            sinces.get(before),
            Some(&0),
            "a full reload (since = 0), never a delta from the stale cursor: {sinces:?}"
        );
        assert!(effects.contains(&FeedEffect::ProjectJoined(PID.into())));
        assert!(h.0.contains(&"reload p1".to_string()));
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
    }

    /// Fix round 4, finding 3 (controller ruling): a cached, quiet project
    /// whose `feed_epoch` is still NULL and whose `hub_version` already
    /// equals the hello head never runs a catch-up, and a holder side may
    /// only persist its seq (never the epoch). Without a stamp here its
    /// `feed_epoch` would stay NULL forever, and a later hub epoch change
    /// with no version/holder-seq regression would never be detected for it.
    #[tokio::test]
    async fn hello_stamps_a_null_epoch_in_sync_row_so_a_later_epoch_change_reloads_it() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = NoHolders(vec![]);
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let row = crate::db::collab::get_project(&conn, PID).unwrap().unwrap();
            crate::db::collab::set_sync_state(
                &conn,
                PID,
                Some(hub.version(PID)),
                row.manifest_cursor,
                &row.gov_caps_json,
            )
            .unwrap();
        }
        let (epoch0, v0) = cursor(&ctx);
        assert_eq!(epoch0, None, "never live-fed");
        assert_eq!(v0, hub.version(PID), "already in sync with the head");

        let before = manifest_requests_for(&hub, PID).await;
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
        assert_eq!(
            manifest_requests_for(&hub, PID).await,
            before,
            "an in-sync row is stamped without any fetch"
        );
        assert_eq!(
            cursor(&ctx),
            (Some("e1".to_string()), v0),
            "the hello epoch is stamped, the version kept"
        );

        let e2 = hub.rotate_epoch();
        let effects = f
            .apply(LiveEvent::Hello(hello(&hub, &e2)), &mut h)
            .await
            .unwrap();
        assert!(effects.contains(&FeedEffect::EpochChanged));
        assert!(
            h.0.contains(&"reload p1".to_string()),
            "the epoch change was detected for this quiet row and reloaded it"
        );
        assert_eq!(cursor(&ctx).0.as_deref(), Some(e2.as_str()));
    }
}
