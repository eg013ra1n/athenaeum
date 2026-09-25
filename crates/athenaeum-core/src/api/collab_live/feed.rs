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
                        .reload_one_project(&h.project_id, &epoch, None, holders)
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
            LiveEvent::Resync(r) => match r.what {
                ResyncWhat::Project => self.catch_up_project(&r.project_id, None, &ALL_KINDS).await,
                ResyncWhat::Holders => {
                    let Some(epoch) = self.epoch.clone() else {
                        tracing::debug!(project_id = %r.project_id, "resync(holders) received before the first hello; skipped");
                        return Ok(vec![]);
                    };
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

        let mut plans: Vec<(String, HelloProject, HelloPlan)> =
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
            plans.push((pid.clone(), hp.clone(), plan));
        }

        if epoch_changed_any {
            let heads: std::collections::BTreeMap<String, i64> = hello
                .projects
                .iter()
                .map(|(pid, hp)| (pid.clone(), hp.version))
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
        for (pid, hp, plan) in plans {
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
            return match self
                .reload_one_project(&ev.project_id, &epoch, Some(ev.version), holders)
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
        let account = self.account_id.clone().unwrap_or_default();
        let database = db(&self.ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, pid)?;
        let tx = conn.unchecked_transaction()?;
        let mut max_mv = project.manifest_cursor;
        let mut counts: std::collections::BTreeMap<
            crate::api::collab_exchange::FramesChangeKind,
            usize,
        > = Default::default();
        for v in rows.iter_mut() {
            v.own = v.publisher_account_id == account;
            let prev = frames_db::get(&tx, pid, &v.frame_uuid)?;
            for kind in crate::api::collab_exchange::classify_frame_change(prev.as_ref(), v) {
                *counts.entry(kind).or_default() += 1;
            }
            frames_db::upsert_from_manifest(&tx, pid, v)?;
            max_mv = max_mv.max(v.manifest_version);
        }
        crate::db::collab::set_sync_state(&tx, pid, Some(version), max_mv, &project.gov_caps_json)?;
        crate::db::collab::set_feed_version(&tx, pid, epoch, version)?;
        tx.commit()?;
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
                .reload_one_project(&pid, &epoch, Some(head_version), holders)
                .await
            {
                Ok(effs) => effects.extend(effs),
                Err(e) => {
                    tracing::error!(project_id = %pid, error = %e, "per-project epoch reload failed (versions tick); retried next tick");
                }
            }
        }

        if epoch_change_needed {
            let heads: std::collections::BTreeMap<String, i64> = v
                .iter()
                .map(|(pid, (version, _))| (pid.clone(), *version))
                .collect();
            effects.extend(self.epoch_change(&epoch, &heads, holders).await?);
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
    /// (a restore): reload every project that actually needs it, reconcile
    /// holdings, and re-announce this device's own frames the hub no longer
    /// lists. `heads` is the per-project version from the triggering hello
    /// or versions vector; a live project missing from it keeps whatever
    /// version its manifest resync lands on.
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
    /// versions tick.
    pub async fn epoch_change(
        &mut self,
        new_epoch: &str,
        heads: &std::collections::BTreeMap<String, i64>,
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
            let head_regressed = heads.get(&pid).is_some_and(|&h| h < stored.version);
            if already_on_new_epoch && !head_regressed {
                // Already reloaded into this exact epoch, no further
                // regression since — nothing to redo (item 4).
                continue;
            }
            match self
                .reload_one_project(&pid, new_epoch, heads.get(&pid).copied(), holders)
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

    /// Reload ONE project fully: manifest resync from 0, a holder-seq reset,
    /// the holder side's own reload, this device's own-frame re-announce,
    /// then — only once every one of those has already succeeded — the
    /// single write that moves this project's cursor into `new_epoch`.
    /// Shared by [`Self::epoch_change`] (every project that needs it, a
    /// genuine hub-wide epoch rotation) and by [`Self::on_project`]/
    /// [`Self::on_versions`]/the `holders` event dispatch in [`Self::apply`]
    /// (fix round 2, item 1) when ONE project's own stored epoch is found
    /// behind the session's — that project must never take a version-number
    /// delta decision against a manifest cursor left over from before its
    /// own restore. `target_version`, when known (the event/tick that
    /// triggered this), is preferred for the cursor; otherwise the
    /// manifest's own freshly-fetched `projectVersion` is used (fix round 2,
    /// item 6 — never a pre-update DB read, which could itself be stale).
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
        holders: &mut dyn HolderSide,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let (seen, project_version) = crate::api::collab_exchange::sync_manifest_full(
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
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::reset_holder_seq(&conn, pid)?;
        }
        let mut effs = holders.reload(pid, new_epoch).await?;
        reannounce_lost_own_frames(&self.ctx, &self.client, &self.token, pid, &seen).await?;
        let cursor_version = target_version.unwrap_or(project_version);
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::set_feed_version(&conn, pid, new_epoch, cursor_version)?;
        }
        effs.push(FeedEffect::NeedSetChanged(pid.to_string()));
        Ok(effs)
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
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
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

    /// Item 2 fix round 2: the holder-seq reset happens before
    /// `holders.reload` runs, and touches ONLY `holder_seq` — `feed_epoch`
    /// is still the OLD value at that moment. `feed_epoch` moves only in the
    /// final write, after `reload` (and the re-announce) already succeeded.
    #[tokio::test]
    async fn epoch_reload_resets_holder_seq_before_reload_without_moving_the_epoch_early() {
        let (_t, ctx, hub, mut f) = rig().await;
        let mut h = RecordingHolders(vec![], Arc::clone(&ctx));
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h)
            .await
            .unwrap();
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
            "holder_seq was already reset before reload ran: {seen}"
        );
        // and afterward, the final write DID move the epoch
        assert_eq!(full_cursor(&ctx, PID).0.as_deref(), Some(e2.as_str()));
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
}
