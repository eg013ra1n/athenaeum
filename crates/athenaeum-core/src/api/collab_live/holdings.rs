//! The holder side of the live exchange (collab v3 wave 3, Task 6; spec §6,
//! I4/I5, plan P7–P9): every live project's holder map, loaded from disk and
//! kept current by snapshot, delta resume (`GET …/holders?since=`) and
//! `holders` events; this device's claim reports — the outbox flush, the
//! `full: true` report and the digest check that repairs a drift.
//!
//! [`Holdings`] implements the feed applier's [`HolderSide`]. Per its
//! contract it persists the holder cursor ONLY through
//! [`crate::db::collab::set_holder_seq_only`] — `feed_epoch` is the applier's
//! alone.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use rusqlite::Connection;

use crate::account::AccountClientError;
use crate::api::collab_exchange::client_err;
use crate::api::collab_live::feed::{FeedEffect, HolderSide};
use crate::api::{db, ApiError};
use crate::collab::hub_client::{
    is_retryable, with_retry, CollabClient, RetryPolicy, INTERACTIVE_ATTEMPTS,
};
use crate::collab::live::backoff::Backoff;
use crate::collab::live::cursor::{step, HolderPlan, Step};
use crate::collab::live::digest::ClaimDigest;
use crate::collab::live::holders::ProjectHolders;
use crate::collab::live::outbox::{self, FlushClock, DIGEST_CHECK_EVERY};
use crate::collab::live::wire::{
    HelloProject, HolderDeltaWire, HoldersEvent, HoldersReportReplyWire, HoldersReportWire,
    HoldersSnapshotWire,
};
use crate::collab::snapshot::SnapshotMember;
use crate::db::collab::CollabProjectRow;
use crate::db::collab_live as live_db;
use crate::services::ServiceContext;

/// The `last_error` a claim the hub refused leaves on its frame row (P9).
const REFUSED_ERROR: &str = live_db::REFUSED_CLAIM_ERROR;

/// The standard base64 node ids of every member device in the project's
/// signed membership snapshot — the "is a member" leg of I5. A snapshot that
/// does not parse is logged and yields the empty set: fail closed, no
/// providers.
pub fn member_devices(project: &CollabProjectRow) -> HashSet<String> {
    match serde_json::from_str::<Vec<SnapshotMember>>(&project.members_json) {
        Ok(members) => members.into_iter().flat_map(|m| m.nodes).collect(),
        Err(error) => {
            tracing::warn!(project_id = %project.project_id, %error, "members_json does not parse; no member devices, so no providers");
            HashSet::new()
        }
    }
}

/// What one flush did: report entries sent, the hub's digest verdict, and
/// how many claims it refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushOutcome {
    pub sent: usize,
    pub digest_match: bool,
    pub refused: usize,
}

/// A failed report: the hub refused or was unreachable (the outbox keeps
/// its rows), or the local store failed.
enum SendErr {
    Hub(AccountClientError),
    Local(ApiError),
}

impl From<ApiError> for SendErr {
    fn from(e: ApiError) -> Self {
        SendErr::Local(e)
    }
}

impl From<anyhow::Error> for SendErr {
    fn from(e: anyhow::Error) -> Self {
        SendErr::Local(e.into())
    }
}

impl From<rusqlite::Error> for SendErr {
    fn from(e: rusqlite::Error) -> Self {
        SendErr::Local(e.into())
    }
}

fn write_maps(
    maps: &HolderMaps,
) -> std::sync::RwLockWriteGuard<'_, HashMap<String, ProjectHolders>> {
    maps.write().unwrap_or_else(|p| p.into_inner())
}

/// One project's retry state after a failed report.
#[derive(Default)]
struct Retry {
    backoff: Backoff,
    retry_at: Option<Instant>,
    /// A `warn!` was logged for the current outage (once per outage).
    outage_logged: bool,
}

/// Every live project's in-memory holder map, shared between the holder
/// side (the feed worker writes it) and the runtime loop and the storage
/// task (they read it) — Task 15 fix round 1, C1. The lock is never held
/// across an await: writers apply a delta or swap a map in between their
/// hub calls, readers derive a provider list or a redundancy and let go.
pub type HolderMaps = Arc<RwLock<HashMap<String, ProjectHolders>>>;

/// Every live project's holder map, flush clock and report retry state.
pub struct Holdings {
    ctx: Arc<ServiceContext>,
    client: CollabClient,
    token: String,
    me: String,
    maps: HolderMaps,
    clocks: HashMap<String, FlushClock>,
    backoffs: HashMap<String, Retry>,
    /// Projects whose `full: true` report is owed (a digest mismatch after
    /// `hello` or a reload whose report failed) — retried by [`Holdings::flush_due`].
    needs_full: HashSet<String>,
    last_digest_check: Instant,
}

impl Holdings {
    /// Load every live project's persisted holder map. A project with unsent
    /// outbox rows gets a flush clock started now, so a restart flushes what
    /// was left.
    pub fn load(
        ctx: Arc<ServiceContext>,
        client: CollabClient,
        token: String,
        me: String,
    ) -> Result<Self, ApiError> {
        let now = Instant::now();
        let mut maps = HashMap::new();
        let mut clocks = HashMap::new();
        {
            let database = db(&ctx)?;
            let conn = database.conn();
            for p in crate::db::collab::list_projects(&conn)? {
                let (devices, claims) = live_db::load_holders(&conn, &p.project_id)?;
                maps.insert(
                    p.project_id.clone(),
                    ProjectHolders::from_rows(&devices, &claims),
                );
                if live_db::outbox_len(&conn, &p.project_id)? > 0 {
                    let mut c = FlushClock::new();
                    c.on_append(now);
                    clocks.insert(p.project_id, c);
                }
            }
        }
        tracing::debug!(count = maps.len(), "holder maps loaded");
        Ok(Holdings {
            ctx,
            client,
            token,
            me,
            maps: Arc::new(RwLock::new(maps)),
            clocks,
            backoffs: HashMap::new(),
            needs_full: HashSet::new(),
            last_digest_check: now,
        })
    }

    /// This device's own node id (standard base64), as holder maps name it.
    pub fn me(&self) -> &str {
        &self.me
    }

    /// A copy of one project's in-memory holder map.
    pub fn map(&self, project_id: &str) -> Option<ProjectHolders> {
        self.read_maps().get(project_id).cloned()
    }

    /// The shared maps: the runtime and the storage task read them while
    /// this holder side keeps them current.
    pub fn maps_handle(&self) -> HolderMaps {
        Arc::clone(&self.maps)
    }

    /// Move this holder side onto `target` (its contents replaced by this
    /// side's maps), so readers holding `target` see every later change.
    pub fn share_into(&mut self, target: &HolderMaps) {
        let loaded = std::mem::take(&mut *write_maps(&self.maps));
        *write_maps(target) = loaded;
        self.maps = Arc::clone(target);
    }

    fn read_maps(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, ProjectHolders>> {
        self.maps.read().unwrap_or_else(|p| p.into_inner())
    }

    /// Apply `f` to the in-memory map, loaded from its persisted rows when
    /// this session has not seen the project yet. The lock is held only for
    /// `f` (never across an await).
    fn with_map_mut(
        &mut self,
        project_id: &str,
        f: impl FnOnce(&mut ProjectHolders),
    ) -> Result<(), ApiError> {
        let loaded = if self.read_maps().contains_key(project_id) {
            None
        } else {
            let (devices, claims) = {
                let database = db(&self.ctx)?;
                let conn = database.conn();
                live_db::load_holders(&conn, project_id)?
            };
            Some(ProjectHolders::from_rows(&devices, &claims))
        };
        let mut maps = write_maps(&self.maps);
        if let Some(map) = loaded {
            maps.entry(project_id.to_string()).or_insert(map);
        }
        f(maps.get_mut(project_id).expect("inserted above"));
        Ok(())
    }

    /// The stored holder cursor, `None` when the project is not a live cache
    /// row (never joined, or lost).
    fn stored_holder_seq(&self, project_id: &str) -> Result<Option<i64>, ApiError> {
        let database = db(&self.ctx)?;
        let conn = database.conn();
        Ok(crate::db::collab::get_live_project(&conn, project_id)?.map(|p| p.holder_seq))
    }

    /// Load the project's whole holder map (`GET …/holders/snapshot`) and
    /// persist it in one transaction with every frame's `frameSeq` and the
    /// snapshot's holder cursor.
    pub async fn snapshot(
        &mut self,
        project_id: &str,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let snap = with_retry("holders snapshot", RetryPolicy::Background, || {
            self.client.holders_snapshot(&self.token, project_id)
        })
        .await
        .map_err(|e| {
            tracing::warn!(project_id, error = %e, "holder snapshot failed");
            client_err(e)
        })?;
        if snap.epoch != epoch {
            tracing::warn!(project_id, epoch = %snap.epoch, session_epoch = epoch, "holder snapshot is from another epoch");
            return Err(ApiError::Conflict("epoch_changed".into()));
        }
        let map = ProjectHolders::from_snapshot(&snap);
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            persist_snapshot(&conn, project_id, &map, &snap)?;
        }
        write_maps(&self.maps).insert(project_id.to_string(), map);
        tracing::info!(
            project_id,
            holder_seq = snap.holder_seq,
            count = snap.devices.len(),
            "holder snapshot loaded"
        );
        Ok(vec![FeedEffect::ProvidersChanged(project_id.to_string())])
    }

    /// Resume from the stored holder cursor over `GET …/holders?since=`,
    /// page by page; the cursor then becomes the FIRST page's `holderSeq`
    /// (hub contract — a later page may repeat newer changes, harmlessly).
    /// No local map (`holder_seq < 0`) or a `410` below the floor / ahead of
    /// the hub → the snapshot; `410 epoch_changed` → `Conflict("epoch_changed")`
    /// for the session to turn into an epoch change.
    pub async fn delta_resume(
        &mut self,
        project_id: &str,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let Some(stored) = self.stored_holder_seq(project_id)? else {
            tracing::debug!(
                project_id,
                "holder catch-up for a project that is not cached; skipped"
            );
            return Ok(vec![]);
        };
        if stored < 0 {
            return self.snapshot(project_id, epoch).await;
        }
        let mut since = stored;
        let mut after: Option<String> = None;
        let mut head: Option<i64> = None;
        let mut applied = false;
        loop {
            let res = with_retry("holders delta", RetryPolicy::Background, || {
                self.client.holders_since(
                    &self.token,
                    project_id,
                    since,
                    after.as_deref(),
                    Some(epoch),
                )
            })
            .await;
            let page = match res {
                Ok(p) => p,
                Err(AccountClientError::Gone(e)) if e == "epoch_changed" => {
                    tracing::warn!(
                        project_id,
                        epoch,
                        "holder delta refused: the hub epoch changed"
                    );
                    return Err(ApiError::Conflict("epoch_changed".into()));
                }
                Err(AccountClientError::Gone(e)) => {
                    tracing::info!(project_id, reason = %e, holder_seq = since, "holder delta refused; reloading the snapshot");
                    return self.snapshot(project_id, epoch).await;
                }
                Err(e) => {
                    tracing::warn!(project_id, error = %e, "holder delta failed");
                    return Err(client_err(e));
                }
            };
            head.get_or_insert(page.holder_seq);
            if !page.deltas.is_empty() {
                {
                    let database = db(&self.ctx)?;
                    let conn = database.conn();
                    apply_deltas(&conn, project_id, &page.deltas, None)?;
                }
                self.with_map_mut(project_id, |map| {
                    for d in &page.deltas {
                        map.apply_delta(d);
                    }
                })?;
                applied = true;
            }
            if !page.has_more {
                break;
            }
            match page.next {
                Some(n) => {
                    since = n.since;
                    after = Some(n.after);
                }
                None => {
                    // Never move the cursor past rows this catch-up did not
                    // read: it stays where it was, and the next catch-up
                    // re-reads from there (re-applying a page is harmless).
                    tracing::error!(
                        project_id,
                        holder_seq = stored,
                        "holder delta page says hasMore without a next cursor; cursor left unchanged"
                    );
                    return Err(ApiError::Internal(
                        "hub holder delta page has more rows but no cursor".into(),
                    ));
                }
            }
        }
        let head = head.unwrap_or(stored);
        {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            crate::db::collab::set_holder_seq_only(&conn, project_id, head)?;
        }
        tracing::debug!(project_id, holder_seq = head, "holder delta applied");
        Ok(if applied {
            vec![FeedEffect::ProvidersChanged(project_id.to_string())]
        } else {
            vec![]
        })
    }

    /// Send the project's pending outbox as one coalesced delta report. The
    /// hub call is NOT retried here — the session re-schedules with the
    /// project's back-off ([`Self::flush_due`]); the outbox keeps the rows
    /// until the hub has taken them. A `digestMatch: false` answer sends one
    /// `full: true` report right after.
    pub async fn flush(&mut self, project_id: &str) -> Result<FlushOutcome, ApiError> {
        let res = flush_once(&self.ctx, &self.client, &self.token, project_id).await;
        match res {
            Ok(done) => {
                self.report_ok(project_id, done.next_flush_ms);
                if !done.outcome.digest_match {
                    tracing::info!(
                        project_id,
                        "claim digest mismatch after a flush; sending the full claim set"
                    );
                    self.full_report(project_id).await?;
                }
                Ok(done.outcome)
            }
            Err(e) => Err(self.report_failed(project_id, "holdings flush", e)),
        }
    }

    /// Send the device's whole claim set as `full: true` under a NEW journal
    /// sequence (a reused one would be ignored for every frame already
    /// stamped with it, T6 ruling), and ack every outbox row it subsumes.
    ///
    /// A claim that entered the local set while the report was in flight
    /// WITHOUT an outbox row — an announce's or a version's implicit claim
    /// (P9, hub contract "Implicit claims") — may have been written by the
    /// hub before this report reached it, stamped below its `reportSeq`: the
    /// hub then removed it as absent from the "whole set as of R", and the
    /// digests still matched (both sides describe the set the report read).
    /// Such a claim owes another full report, sent at once by
    /// [`Self::flush_due`] — never left for the hourly digest check.
    pub async fn full_report(&mut self, project_id: &str) -> Result<(), ApiError> {
        match report_full(&self.ctx, &self.client, &self.token, project_id).await {
            Ok((next_flush_ms, unreported)) => {
                if unreported.is_empty() {
                    self.needs_full.remove(project_id);
                } else {
                    tracing::info!(
                        project_id,
                        count = unreported.len(),
                        "claims added while the full report was in flight; sending the full claim set again"
                    );
                    self.needs_full.insert(project_id.to_string());
                }
                self.report_ok(project_id, Some(next_flush_ms));
                Ok(())
            }
            Err(e) => {
                self.needs_full.insert(project_id.to_string());
                Err(self.report_failed(project_id, "full holder report", e))
            }
        }
    }

    /// An empty report — the hub compares its digest of this device's claims
    /// with ours. A mismatch sends the full claim set and answers `false`. A
    /// project with unsent outbox rows flushes instead (its digest would
    /// differ only by what the flush is about to send).
    pub async fn digest_check(&mut self, project_id: &str) -> Result<bool, ApiError> {
        let (claims, report_seq, pending) = {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            (
                live_db::my_claims(&conn, project_id)?,
                live_db::current_report_seq(&conn)?,
                live_db::outbox_len(&conn, project_id)?,
            )
        };
        if pending > 0 {
            return Ok(self.flush(project_id).await?.digest_match);
        }
        let report = outbox::digest_check(&claims, report_seq)?;
        let reply = match self
            .client
            .report_holders(&self.token, project_id, &report)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(self.report_failed(project_id, "claim digest check", SendErr::Hub(e)))
            }
        };
        self.report_ok(project_id, Some(reply.next_flush_ms));
        if reply.digest_match {
            tracing::debug!(
                project_id,
                count = report.count,
                digest_match = true,
                "claim digest checked"
            );
            return Ok(true);
        }
        tracing::info!(
            project_id,
            count = report.count,
            "claim digest mismatch; sending the full claim set"
        );
        self.full_report(project_id).await?;
        Ok(false)
    }

    /// Once [`DIGEST_CHECK_EVERY`] has passed: a digest check per live
    /// project, off the critical path. Failures are logged and retried next
    /// hour.
    pub async fn hourly_digest_checks(&mut self, now: Instant) {
        if now.duration_since(self.last_digest_check) < DIGEST_CHECK_EVERY {
            return;
        }
        self.last_digest_check = now;
        let pids: Vec<String> = match db(&self.ctx).and_then(|d| {
            Ok(crate::db::collab::list_projects(&d.conn())?
                .into_iter()
                .map(|p| p.project_id)
                .collect())
        }) {
            Ok(p) => p,
            Err(error) => {
                tracing::warn!(%error, "hourly claim digest checks: listing projects failed; retried next hour");
                return;
            }
        };
        for pid in pids {
            if let Err(error) = self.digest_check(&pid).await {
                tracing::warn!(project_id = %pid, %error, "hourly claim digest check failed; retried next hour");
            }
        }
    }

    /// Flush every project whose outbox is due (its flush delay has passed,
    /// or it reached the entry threshold) and is not backing off after a
    /// failure, plus every owed full report. Pending rows no clock knew of
    /// (appended without [`Self::note_append`]) start their wait now.
    pub async fn flush_due(
        &mut self,
        now: Instant,
    ) -> Vec<(String, Result<FlushOutcome, ApiError>)> {
        let pending: Vec<(String, usize)> = match db(&self.ctx).and_then(|d| {
            let conn = d.conn();
            let mut out = Vec::new();
            for pid in live_db::outbox_projects(&conn)? {
                let n = live_db::outbox_len(&conn, &pid)?;
                out.push((pid, n));
            }
            Ok(out)
        }) {
            Ok(p) => p,
            Err(error) => {
                // Task 15 R1: every due clock and owed report waits at least
                // the back-off floor — left in the past, `next_deadline`
                // would wake the session loop again at once, forever.
                let floor = now + crate::collab::live::backoff::BACKOFF_FLOOR;
                let waiting: Vec<String> = self
                    .clocks
                    .iter()
                    .filter(|(_, c)| c.deadline().is_some())
                    .map(|(pid, _)| pid.clone())
                    .chain(self.needs_full.iter().cloned())
                    .collect();
                for pid in waiting {
                    let r = self.backoffs.entry(pid).or_default();
                    r.retry_at = Some(r.retry_at.map_or(floor, |t| t.max(floor)));
                }
                tracing::warn!(%error, retry_in_ms = crate::collab::live::backoff::BACKOFF_FLOOR.as_millis() as u64, "reading the claim outbox failed; flush retried after the back-off floor");
                return vec![];
            }
        };
        // A clock whose project has nothing pending any more (another path
        // emptied the outbox: a publish's own flush, a lost project's
        // cleanup) stops waiting — or `next_deadline` would stay in the past
        // and a loop sleeping on it would spin.
        for (pid, clock) in self.clocks.iter_mut() {
            if !pending.iter().any(|(p, _)| p == pid) {
                clock.clear();
            }
        }
        let mut out = Vec::new();
        for (pid, n) in pending {
            let clock = self.clocks.entry(pid.clone()).or_default();
            if clock.deadline().is_none() {
                clock.on_append(now);
            }
            if !clock.due(now, n) || self.backing_off(&pid, now) {
                continue;
            }
            let res = self.flush(&pid).await;
            out.push((pid, res));
        }
        let owed: Vec<String> = self.needs_full.iter().cloned().collect();
        for pid in owed {
            if self.backing_off(&pid, now) || out.iter().any(|(p, _)| p == &pid) {
                continue;
            }
            let res = self.full_report(&pid).await.map(|()| FlushOutcome {
                sent: 0,
                digest_match: true,
                refused: 0,
            });
            out.push((pid, res));
        }
        out
    }

    /// Sync now (L10): every project's report back-off is forgotten, so the
    /// next flush is due at once instead of after the back-off.
    pub fn clear_backoffs(&mut self) {
        self.backoffs.clear();
    }

    /// The executor calls this after every state transition that wrote an
    /// outbox row: the project's flush wait starts at its first unsent entry.
    pub fn note_append(&mut self, project_id: &str, now: Instant) {
        self.clocks
            .entry(project_id.to_string())
            .or_default()
            .on_append(now);
    }

    /// The earliest instant anything here is due: a flush (never before the
    /// project's back-off allows), an owed full report, or the hourly digest
    /// check.
    pub fn next_deadline(&self) -> Option<Instant> {
        let retry_at = |pid: &str| self.backoffs.get(pid).and_then(|r| r.retry_at);
        let flushes = self
            .clocks
            .iter()
            .filter_map(|(pid, c)| c.deadline().map(|d| retry_at(pid).map_or(d, |r| r.max(d))));
        let owed = self
            .needs_full
            .iter()
            .map(|pid| retry_at(pid).unwrap_or_else(Instant::now));
        flushes
            .chain(owed)
            .chain(std::iter::once(self.last_digest_check + DIGEST_CHECK_EVERY))
            .min()
    }

    fn backing_off(&self, project_id: &str, now: Instant) -> bool {
        self.backoffs
            .get(project_id)
            .and_then(|r| r.retry_at)
            .is_some_and(|t| now < t)
    }

    /// A report went through: reset the project's back-off (logging the
    /// recovery once) and restart its flush wait — at the hub's new delay
    /// when the answer carried one (`None`: nothing was sent, the stored
    /// delay stays). Rows appended while the report was in flight are still
    /// pending: they start a new wait now, never left without a deadline.
    fn report_ok(&mut self, project_id: &str, next_flush_ms: Option<u64>) {
        if let Some(r) = self.backoffs.remove(project_id) {
            if r.outage_logged {
                tracing::info!(project_id, "holder reports recovered");
            }
        }
        let still_pending = match db(&self.ctx)
            .and_then(|d| Ok(live_db::outbox_len(&d.conn(), project_id)?))
        {
            Ok(n) => n > 0,
            Err(error) => {
                tracing::warn!(project_id, %error, "reading the claim outbox after a report failed; keeping a flush deadline");
                true
            }
        };
        let clock = self.clocks.entry(project_id.to_string()).or_default();
        match next_flush_ms {
            Some(ms) => clock.flushed(ms),
            None => clock.clear(),
        }
        if still_pending {
            clock.on_append(Instant::now());
        }
    }

    /// A report failed — the hub, or the local store: back off (full
    /// jitter) either way so a persistent failure is not retried every tick,
    /// log once per outage (`error!` for a local failure, `warn!` for the
    /// hub), and return the error for the caller.
    fn report_failed(&mut self, project_id: &str, what: &str, e: SendErr) -> ApiError {
        let r = self.backoffs.entry(project_id.to_string()).or_default();
        let delay = r.backoff.next_delay();
        r.retry_at = Some(Instant::now() + delay);
        let retry_in_ms = delay.as_millis() as u64;
        match e {
            SendErr::Local(e) => {
                if r.outage_logged {
                    tracing::debug!(project_id, command = what, error = %e, retry_in_ms, "holder report failed locally again");
                } else {
                    r.outage_logged = true;
                    tracing::error!(project_id, command = what, error = %e, retry_in_ms, "holder report failed locally");
                }
                e
            }
            SendErr::Hub(e) => {
                if r.outage_logged {
                    tracing::debug!(project_id, command = what, error = %e, retry_in_ms, "holder report failed again; the outbox keeps its entries");
                } else {
                    r.outage_logged = true;
                    tracing::warn!(project_id, command = what, error = %e, retry_in_ms, "holder report failed; the outbox keeps its entries");
                }
                client_err(e)
            }
        }
    }

    /// Compare the hub's `(claimCount, claimDigest)` from `hello` with the
    /// local claim set; a mismatch owes a full report, sent now (and retried
    /// by [`Self::flush_due`] if it fails).
    async fn reconcile_after_hello(
        &mut self,
        project_id: &str,
        hello: &HelloProject,
    ) -> Result<(), ApiError> {
        let claims = {
            let database = db(&self.ctx)?;
            let conn = database.conn();
            live_db::my_claims(&conn, project_id)?
        };
        let local = ClaimDigest::of_claims(claims.iter().map(|(u, v)| (u.as_str(), *v)))?;
        if local.count == hello.claim_count && local.hex() == hello.claim_digest {
            return Ok(());
        }
        tracing::info!(
            project_id,
            count = local.count,
            "claim digest mismatch after hello; sending the full claim set"
        );
        if let Err(error) = self.full_report(project_id).await {
            tracing::warn!(project_id, %error, "full claim report after hello failed; retried on the next flush");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl HolderSide for Holdings {
    async fn on_hello_project(
        &mut self,
        project_id: &str,
        hello: &HelloProject,
        plan: HolderPlan,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let effects = match plan {
            HolderPlan::Snapshot => self.snapshot(project_id, epoch).await?,
            HolderPlan::Delta => self.delta_resume(project_id, epoch).await?,
            HolderPlan::InSync => vec![],
        };
        self.reconcile_after_hello(project_id, hello).await?;
        Ok(effects)
    }

    /// Prev-checked (I3): apply a contiguous event in one transaction with
    /// the new cursor, ignore an old one, catch a gap up over REST.
    async fn on_holders_event(
        &mut self,
        ev: &HoldersEvent,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        let Some(stored) = self.stored_holder_seq(&ev.project_id)? else {
            tracing::debug!(project_id = %ev.project_id, "holders event for a project that is not cached; skipped");
            return Ok(vec![]);
        };
        match step(stored, ev.prev, ev.seq) {
            Step::Ignore => Ok(vec![]),
            Step::CatchUp => {
                tracing::debug!(project_id = %ev.project_id, holder_seq = stored, prev = ev.prev, "holders event gap; catching up");
                self.delta_resume(&ev.project_id, epoch).await
            }
            Step::Apply => {
                {
                    let database = db(&self.ctx)?;
                    let conn = database.conn();
                    apply_deltas(&conn, &ev.project_id, &ev.deltas, Some(ev.seq))?;
                }
                self.with_map_mut(&ev.project_id, |map| {
                    for d in &ev.deltas {
                        map.apply_delta(d);
                    }
                })?;
                Ok(if ev.deltas.is_empty() {
                    vec![]
                } else {
                    vec![FeedEffect::ProvidersChanged(ev.project_id.clone())]
                })
            }
        }
    }

    async fn catch_up(
        &mut self,
        project_id: &str,
        epoch: &str,
    ) -> Result<Vec<FeedEffect>, ApiError> {
        self.delta_resume(project_id, epoch).await
    }

    /// Snapshot, then the full claim set (an epoch change or an
    /// `account: joined`). A failed full report does not fail the reload —
    /// it is owed and retried by [`Holdings::flush_due`].
    async fn reload(&mut self, project_id: &str, epoch: &str) -> Result<Vec<FeedEffect>, ApiError> {
        let effects = self.snapshot(project_id, epoch).await?;
        if let Err(error) = self.full_report(project_id).await {
            tracing::warn!(project_id, %error, "full claim report after a holder reload failed; retried on the next flush");
        }
        Ok(effects)
    }

    fn forget(&mut self, project_id: &str) {
        write_maps(&self.maps).remove(project_id);
        self.clocks.remove(project_id);
        self.backoffs.remove(project_id);
        self.needs_full.remove(project_id);
    }
}

/// Persist a snapshot: the map, every frame's hub ordinal, and the cursor —
/// one transaction.
fn persist_snapshot(
    conn: &Connection,
    project_id: &str,
    map: &ProjectHolders,
    snap: &HoldersSnapshotWire,
) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    live_db::replace_holders(&tx, project_id, &map.device_rows(), &map.claim_rows())?;
    for f in &snap.frames {
        crate::db::collab_frames::set_frame_seq(&tx, project_id, &f.uuid, f.seq)?;
    }
    crate::db::collab::set_holder_seq_only(&tx, project_id, snap.holder_seq)?;
    tx.commit()?;
    Ok(())
}

/// Apply one page's (or event's) device deltas — and, for an event, its new
/// cursor — in one transaction.
fn apply_deltas(
    conn: &Connection,
    project_id: &str,
    deltas: &[HolderDeltaWire],
    new_seq: Option<i64>,
) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for d in deltas {
        live_db::apply_holder_delta(&tx, project_id, &d.device, &d.add, &d.rm)?;
    }
    if let Some(seq) = new_seq {
        crate::db::collab::set_holder_seq_only(&tx, project_id, seq)?;
    }
    tx.commit()?;
    Ok(())
}

/// A successful delta flush: what it did, and the hub's next delay.
struct Flushed {
    outcome: FlushOutcome,
    /// The hub's `nextFlushMs`; `None` when nothing was sent.
    next_flush_ms: Option<u64>,
}

/// Read the outbox and the claim set in ONE read transaction, send them as a
/// coalesced delta report (no retry), and apply the answer: ack the rows the
/// report carried (rows appended meanwhile have higher sequences and stay),
/// drop refused claims (P9).
async fn flush_once(
    ctx: &ServiceContext,
    client: &CollabClient,
    token: &str,
    project_id: &str,
) -> Result<Flushed, SendErr> {
    let (rows, claims) = {
        let database = db(ctx)?;
        let conn = database.conn();
        let tx = conn.unchecked_transaction()?;
        let rows = live_db::outbox(&tx, project_id)?;
        let claims = live_db::my_claims(&tx, project_id)?;
        tx.commit()?;
        (rows, claims)
    };
    if rows.is_empty() {
        return Ok(Flushed {
            outcome: FlushOutcome {
                sent: 0,
                digest_match: true,
                refused: 0,
            },
            next_flush_ms: None,
        });
    }
    let report = outbox::delta_report(&rows, &claims)?;
    let max_seq = rows.iter().map(|r| r.seq).max().unwrap_or(0);
    let reply = client
        .report_holders(token, project_id, &report)
        .await
        .map_err(SendErr::Hub)?;
    apply_reply(ctx, project_id, &report, &reply, max_seq)?;
    let sent = report.add.len() + report.remove.len();
    tracing::debug!(
        project_id,
        count = sent,
        report_seq = report.report_seq,
        holder_seq = reply.holder_seq,
        digest_match = reply.digest_match,
        refused = reply.refused.len(),
        "holdings flushed"
    );
    Ok(Flushed {
        outcome: FlushOutcome {
            sent,
            digest_match: reply.digest_match,
            refused: reply.refused.len(),
        },
        next_flush_ms: Some(reply.next_flush_ms),
    })
}

/// Send the whole claim set as `full: true` under a NEW journal sequence R
/// (taken in the same transaction that reads the set, so no outbox row below
/// R is missing from it), and ack every outbox row up to R. Returns the hub's
/// next flush delay and the claims the report may have cost
/// ([`unreported_claims`]).
async fn report_full(
    ctx: &ServiceContext,
    client: &CollabClient,
    token: &str,
    project_id: &str,
) -> Result<(u64, Vec<String>), SendErr> {
    let (report_seq, claims) = {
        let database = db(ctx)?;
        let conn = database.conn();
        // IMMEDIATE: `next_report_seq` reads before it writes; a deferred
        // read-to-write upgrade under another writer fails at once with
        // SQLITE_BUSY, never waiting the busy timeout.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let r = live_db::next_report_seq(&tx)?;
        let claims = live_db::my_claims(&tx, project_id)?;
        tx.commit()?;
        (r, claims)
    };
    let report = outbox::full_report(&claims, report_seq)?;
    let reply = client
        .report_holders(token, project_id, &report)
        .await
        .map_err(SendErr::Hub)?;
    apply_reply(ctx, project_id, &report, &reply, report_seq)?;
    if reply.digest_match {
        tracing::info!(
            project_id,
            count = report.count,
            report_seq,
            holder_seq = reply.holder_seq,
            refused = reply.refused.len(),
            "full claim set reported"
        );
    } else {
        tracing::warn!(
            project_id,
            count = report.count,
            report_seq,
            holder_seq = reply.holder_seq,
            "claim digest still differs after a full report; the next check retries"
        );
    }
    let unreported = unreported_claims(ctx, project_id, &claims)?;
    Ok((reply.next_flush_ms, unreported))
}

/// The frames whose local claim a `full: true` report that sent `reported`
/// did not carry, and that no outbox row still names (after the report's
/// ack, every row left is newer than it and its own flush reports it): a
/// claim added without an outbox row while the report was in flight — an
/// implicit claim. The hub may have written that claim before the report
/// arrived and removed it again as absent from the full set.
fn unreported_claims(
    ctx: &ServiceContext,
    project_id: &str,
    reported: &[(String, i32)],
) -> Result<Vec<String>, SendErr> {
    let sent: HashMap<&str, i32> = reported.iter().map(|(u, v)| (u.as_str(), *v)).collect();
    let database = db(ctx)?;
    let conn = database.conn();
    let tx = conn.unchecked_transaction()?;
    let now = live_db::my_claims(&tx, project_id)?;
    let pending: HashSet<String> = live_db::outbox(&tx, project_id)?
        .into_iter()
        .map(|r| r.frame_uuid)
        .collect();
    tx.commit()?;
    Ok(now
        .into_iter()
        .filter(|(u, v)| sent.get(u.as_str()) != Some(v) && !pending.contains(u))
        .map(|(u, _)| u)
        .collect())
}

/// Apply a report's answer in one transaction: ack the outbox up to
/// `ack_up_to`, and drop each refused claim (P9) — only while the claim set
/// still holds exactly what the report sent AND no newer outbox row names the
/// frame: a frame re-added while the report was in flight keeps its newer
/// claim (the next flush reports it). A dropped claim marks its row.
fn apply_reply(
    ctx: &ServiceContext,
    project_id: &str,
    report: &HoldersReportWire,
    reply: &HoldersReportReplyWire,
    ack_up_to: i64,
) -> Result<(), SendErr> {
    let sent: HashMap<&str, i32> = report
        .add
        .iter()
        .map(|a| (a.uuid.as_str(), a.content_version))
        .collect();
    let mut dropped = Vec::new();
    let mut kept = Vec::new();
    {
        let database = db(ctx)?;
        let conn = database.conn();
        let tx = conn.unchecked_transaction()?;
        live_db::ack_outbox(&tx, project_id, ack_up_to)?;
        for u in &reply.refused {
            let Some(cv) = sent.get(u.as_str()) else {
                // The hub refuses only what a report added; a refusal naming
                // anything else leaves the claim set alone.
                tracing::warn!(project_id, frame_uuid = %u, "hub refused a claim this report did not add; ignored");
                continue;
            };
            if live_db::drop_refused_claim(&tx, project_id, u, *cv, ack_up_to)? {
                crate::db::collab_frames::set_error(&tx, project_id, u, Some(REFUSED_ERROR))?;
                dropped.push(u);
            } else {
                kept.push(u);
            }
        }
        tx.commit()?;
    }
    for u in dropped {
        tracing::warn!(project_id, frame_uuid = %u, "claim refused by the hub");
    }
    for u in kept {
        tracing::info!(project_id, frame_uuid = %u, "claim refused by the hub, but re-added since the report; the next flush reports it");
    }
    Ok(())
}

/// Flush one project's outbox before a version call (hub rule: "flush any
/// pending outbox entry for such a frame before calling version"), retried
/// like an interactive request (at most [`INTERACTIVE_ATTEMPTS`]). A digest
/// mismatch in the answer sends the full claim set. The publish path uses it;
/// its failure is the caller's to log and fold — the hub keeps the highest
/// `reportSeq` per frame, so a late flush is harmless.
pub(crate) async fn flush_project_now(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<(), ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::warn!(project_id, "holdings flush skipped: signed out");
        return Err(ApiError::SignedOut("Sign in to report holdings.".into()));
    };
    let client = CollabClient::new(&hub_url).map_err(|e| {
        tracing::error!(project_id, error = %e, "holdings flush: hub client failed");
        client_err(e)
    })?;
    let mut backoff = Backoff::new();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match flush_once(ctx, &client, &token, project_id).await {
            Ok(done) => {
                if !done.outcome.digest_match {
                    tracing::info!(
                        project_id,
                        "claim digest mismatch after a flush; sending the full claim set"
                    );
                    let (c, t) = (&client, token.as_str());
                    let full = with_retry(
                        "full holder report",
                        RetryPolicy::Interactive,
                        || async move {
                            match report_full(ctx, c, t, project_id).await {
                                Ok(v) => Ok(Ok(v)),
                                Err(SendErr::Hub(e)) => Err(e),
                                Err(SendErr::Local(e)) => Ok(Err(e)),
                            }
                        },
                    )
                    .await;
                    match full {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::error!(project_id, error = %e, "full holder report failed locally");
                            return Err(e);
                        }
                        Err(e) => {
                            tracing::warn!(project_id, error = %e, "full holder report failed");
                            return Err(client_err(e));
                        }
                    }
                }
                return Ok(());
            }
            Err(SendErr::Hub(e)) if is_retryable(&e) && attempt < INTERACTIVE_ATTEMPTS => {
                let delay = backoff.next_delay();
                tracing::warn!(project_id, attempt, error = %e, retry_in_ms = delay.as_millis() as u64, "holdings flush failed; retrying");
                tokio::time::sleep(delay).await;
            }
            Err(SendErr::Hub(e)) => {
                tracing::warn!(project_id, attempt, error = %e, "holdings flush failed; giving up");
                return Err(client_err(e));
            }
            Err(SendErr::Local(e)) => {
                tracing::error!(project_id, error = %e, "holdings flush failed locally");
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::FakeHub;
    use crate::db::collab_live as live_db;

    async fn rig() -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub, Holdings) {
        let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let ctx = Arc::new(ctx);
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_account("tok-o", "acc-o", "Other", "BBB=", None);
        hub.add_project(
            "p1",
            "m31",
            &[("acc-me", "send_receive", false), ("acc-o", "send", false)],
            false,
        );
        hub.seed_frames("p1", "acc-o", &["u1", "u2"], "published");
        crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), "tok");
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None)
            .await
            .unwrap();
        let h = Holdings::load(
            Arc::clone(&ctx),
            CollabClient::new(hub.uri()).unwrap(),
            "tok".into(),
            "AAA=".into(),
        )
        .unwrap();
        (tmp, ctx, hub, h)
    }

    fn hold(ctx: &ServiceContext, uuid: &str) {
        let conn = crate::api::db(ctx).unwrap().conn();
        crate::db::collab_frames::set_local_state(
            &conn,
            "p1",
            uuid,
            crate::db::collab_frames::LocalState::Held,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn snapshot_persists_and_reloads_without_a_second_snapshot() {
        let (_t, ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        assert_eq!(h.map("p1").unwrap().claimants(1, 1), vec!["BBB="]);
        let h2 = Holdings::load(
            Arc::clone(&ctx),
            CollabClient::new(hub.uri()).unwrap(),
            "tok".into(),
            "AAA=".into(),
        )
        .unwrap();
        assert_eq!(h2.map("p1"), h.map("p1"));
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(
            crate::db::collab::get_project(&conn, "p1")
                .unwrap()
                .unwrap()
                .holder_seq
                >= 1
        );
        // the snapshot also filled frame_seq on the manifest rows
        assert_eq!(
            crate::db::collab_frames::get(&conn, "p1", "u2")
                .unwrap()
                .unwrap()
                .frame_seq,
            Some(2)
        );
    }

    /// The HolderSide contract: a snapshot moves the holder cursor only,
    /// never the feed epoch.
    #[tokio::test]
    async fn a_snapshot_never_writes_the_feed_epoch() {
        let (_t, ctx, _hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_feed_version(&conn, "p1", "old-epoch", 0).unwrap();
        }
        h.snapshot("p1", "epoch-1").await.unwrap();
        let conn = crate::api::db(&ctx).unwrap().conn();
        let p = crate::db::collab::get_project(&conn, "p1")
            .unwrap()
            .unwrap();
        assert_eq!(p.feed_epoch.as_deref(), Some("old-epoch"));
        assert!(p.holder_seq >= 1);
    }

    #[tokio::test]
    async fn a_state_change_flushes_with_a_matching_digest_and_acks_the_outbox() {
        let (_t, ctx, hub, mut h) = rig().await;
        hold(&ctx, "u1");
        h.note_append("p1", Instant::now());
        let out = h.flush("p1").await.unwrap();
        assert_eq!(
            out,
            FlushOutcome {
                sent: 1,
                digest_match: true,
                refused: 0
            }
        );
        assert_eq!(
            hub.holders_of("p1", "u1"),
            vec!["AAA=".to_string(), "BBB=".to_string()]
        );
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(live_db::outbox_len(&conn, "p1").unwrap(), 0);
    }

    #[tokio::test]
    async fn a_digest_mismatch_sends_one_full_report_that_repairs_the_hub() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            // a claim the hub never heard about (the local outbox was lost)
            live_db::add_implicit_claim(&conn, "p1", "u2", 1).unwrap();
        }
        assert!(!h.digest_check("p1").await.unwrap()); // mismatch → full report inside
        assert!(hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
        assert!(h.digest_check("p1").await.unwrap());
    }

    /// T6 ruling: a full report takes a NEW report_seq — reusing the last
    /// reported one would be ignored for the very frame whose claim drifted,
    /// and the mismatch would loop.
    #[tokio::test]
    async fn a_full_report_repairs_a_drift_on_the_last_reported_frame() {
        let (_t, ctx, hub, mut h) = rig().await;
        hold(&ctx, "u1");
        h.flush("p1").await.unwrap();
        assert!(hub.holders_of("p1", "u1").contains(&"AAA=".to_string()));
        // the local claim vanishes without an outbox row (a lost write)
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::drop_claims(&conn, "p1", &["u1".to_string()]).unwrap();
        }
        assert!(!h.digest_check("p1").await.unwrap());
        assert!(!hub.holders_of("p1", "u1").contains(&"AAA=".to_string()));
        assert!(h.digest_check("p1").await.unwrap());
    }

    #[tokio::test]
    async fn refused_claims_leave_the_local_claim_set() {
        let (_t, ctx, _hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::record_claim_change(
                &conn,
                "p1",
                "ghost",
                live_db::ClaimOp::Add { content_version: 1 },
            )
            .unwrap();
        }
        let out = h.flush("p1").await.unwrap();
        assert_eq!(out.refused, 1);
        assert!(out.digest_match);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(live_db::my_claims(&conn, "p1").unwrap().is_empty());
        assert_eq!(live_db::outbox_len(&conn, "p1").unwrap(), 0);
    }

    #[tokio::test]
    async fn a_refused_claim_marks_its_frame_row() {
        let (_t, ctx, hub, mut h) = rig().await;
        // a frame the hub no longer has (a restore lost it) is refused
        hold(&ctx, "u2");
        hub.forget_frames("p1", &["u2"]);
        let out = h.flush("p1").await.unwrap();
        assert_eq!(out.refused, 1);
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, "p1", "u2")
            .unwrap()
            .unwrap();
        assert_eq!(row.last_error.as_deref(), Some(REFUSED_ERROR));
        assert!(live_db::my_claims(&conn, "p1").unwrap().is_empty());
    }

    #[tokio::test]
    async fn delta_resume_and_410_falls_back_to_the_snapshot() {
        let (_t, ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        hub.seed_frames("p1", "acc-o", &["u3"], "published");
        let effects = h.delta_resume("p1", "epoch-1").await.unwrap();
        assert_eq!(effects, vec![FeedEffect::ProvidersChanged("p1".into())]);
        assert_eq!(h.map("p1").unwrap().claimants(3, 1), vec!["BBB="]);
        // a cursor ahead of the hub → 410 → snapshot
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_holder_seq(&conn, "p1", "epoch-1", 999).unwrap();
        }
        h.delta_resume("p1", "epoch-1").await.unwrap();
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(
            crate::db::collab::get_project(&conn, "p1")
                .unwrap()
                .unwrap()
                .holder_seq
                < 999
        );
    }

    /// Multi-page catch-up: every row lands and the cursor is the FIRST
    /// page's `holderSeq`.
    #[tokio::test]
    async fn a_paged_delta_resume_applies_every_page() {
        let (_t, ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        hub.seed_frames("p1", "acc-o", &["u3", "u4", "u5", "u6", "u7"], "published");
        hub.set_page_size(2);
        h.delta_resume("p1", "epoch-1").await.unwrap();
        for seq in 3..=7 {
            assert_eq!(
                h.map("p1").unwrap().claimants(seq, 1),
                vec!["BBB="],
                "frameSeq {seq}"
            );
        }
        let conn = crate::api::db(&ctx).unwrap().conn();
        let (_, claims) = live_db::load_holders(&conn, "p1").unwrap();
        assert_eq!(claims.len(), 7, "persisted too");
        assert_eq!(
            crate::db::collab::get_project(&conn, "p1")
                .unwrap()
                .unwrap()
                .holder_seq,
            hub.holder_seq("p1")
        );
    }

    #[tokio::test]
    async fn an_epoch_mismatch_in_delta_resume_is_an_epoch_change() {
        let (_t, _ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        hub.rotate_epoch();
        let e = h.delta_resume("p1", "epoch-1").await.unwrap_err();
        assert!(
            matches!(e, ApiError::Conflict(ref m) if m == "epoch_changed"),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn holders_events_apply_contiguously_ignore_old_and_catch_up_gaps() {
        let (_t, ctx, hub, mut h) = rig().await;
        h.snapshot("p1", "epoch-1").await.unwrap();
        let seq = |ctx: &ServiceContext| {
            let conn = crate::api::db(ctx).unwrap().conn();
            crate::db::collab::get_project(&conn, "p1")
                .unwrap()
                .unwrap()
                .holder_seq
        };
        let ev = |prev: i64, seq: i64, add: Vec<(i32, i32)>| HoldersEvent {
            project_id: "p1".into(),
            prev,
            seq,
            deltas: vec![HolderDeltaWire {
                device: "CCC=".into(),
                add,
                rm: vec![],
            }],
        };
        // a gap → REST catch-up from the stored cursor: the hub's own change
        // (u9's publisher claim) lands, the cursor is the hub's head
        hub.seed_frames("p1", "acc-o", &["u9"], "published");
        let head = hub.holder_seq("p1");
        h.on_holders_event(&ev(head + 5, head + 6, vec![]), "epoch-1")
            .await
            .unwrap();
        assert_eq!(h.map("p1").unwrap().claimants(3, 1), vec!["BBB="]);
        assert_eq!(seq(&ctx), head);
        // contiguous → applied in one transaction with the new cursor
        let eff = h
            .on_holders_event(&ev(head, head + 1, vec![(1, 1)]), "epoch-1")
            .await
            .unwrap();
        assert_eq!(eff, vec![FeedEffect::ProvidersChanged("p1".into())]);
        assert_eq!(seq(&ctx), head + 1);
        assert!(h.map("p1").unwrap().claimants(1, 1).contains(&"CCC="));
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let (_, claims) = live_db::load_holders(&conn, "p1").unwrap();
            assert!(claims.contains(&("CCC=".to_string(), 1, 1)));
        }
        // old → ignored
        assert!(h
            .on_holders_event(&ev(head - 1, head, vec![(2, 1)]), "epoch-1")
            .await
            .unwrap()
            .is_empty());
        assert!(!h.map("p1").unwrap().claimants(2, 1).contains(&"CCC="));
        assert_eq!(seq(&ctx), head + 1);
    }

    #[tokio::test]
    async fn a_hub_outage_keeps_the_outbox_for_later() {
        let (_t, ctx, hub, mut h) = rig().await;
        hold(&ctx, "u1");
        hub.set_failing("/holders/self", true);
        assert!(h.flush("p1").await.is_err());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert_eq!(live_db::outbox_len(&conn, "p1").unwrap(), 1);
        }
        hub.set_failing("/holders/self", false);
        assert_eq!(h.flush("p1").await.unwrap().sent, 1);
    }

    #[tokio::test]
    async fn flush_due_waits_for_the_flush_delay_and_picks_up_unannounced_rows() {
        let (_t, ctx, hub, mut h) = rig().await;
        let t0 = Instant::now();
        hold(&ctx, "u1"); // no note_append: flush_due finds the row itself
        assert!(h.flush_due(t0).await.is_empty(), "the wait starts now");
        assert!(h
            .next_deadline()
            .is_some_and(|d| d <= t0 + outbox::DEFAULT_FLUSH));
        let done = h.flush_due(t0 + outbox::DEFAULT_FLUSH).await;
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].1.as_ref().unwrap().sent, 1);
        assert!(hub.holders_of("p1", "u1").contains(&"AAA=".to_string()));
        assert!(h.flush_due(t0 + outbox::DEFAULT_FLUSH * 3).await.is_empty());
    }

    #[tokio::test]
    async fn hello_reconciles_a_digest_mismatch_with_a_full_report() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::add_implicit_claim(&conn, "p1", "u1", 1).unwrap();
        }
        let hello: crate::collab::live::wire::HelloEvent =
            serde_json::from_value(hub.hello_for("tok")).unwrap();
        let hp = hello.projects["p1"].clone();
        h.on_hello_project("p1", &hp, HolderPlan::Snapshot, "epoch-1")
            .await
            .unwrap();
        assert!(hub.holders_of("p1", "u1").contains(&"AAA=".to_string()));
        // in sync now: a second hello sends nothing
        let before = hub.holder_seq("p1");
        let hello: crate::collab::live::wire::HelloEvent =
            serde_json::from_value(hub.hello_for("tok")).unwrap();
        h.on_hello_project("p1", &hello.projects["p1"], HolderPlan::InSync, "epoch-1")
            .await
            .unwrap();
        assert_eq!(hub.holder_seq("p1"), before);
    }

    /// An implicit claim (an announce's, a version's) that lands while a
    /// full report is in flight: the hub wrote it before the report arrived,
    /// stamped below the report's sequence, so the "whole set as of R"
    /// removed it — and both digests still matched, since both describe the
    /// set the report read. The claim is owed at once (another full report),
    /// never left for the hourly digest check: until then no other member
    /// can fetch the frame from its only holder.
    #[tokio::test]
    async fn an_implicit_claim_added_during_a_full_report_is_reported_again_at_once() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::add_implicit_claim(&conn, "p1", "u1", 1).unwrap();
        }
        let ctx2 = Arc::clone(&ctx);
        hub.before_next("/holders/self", move |st| {
            // The announce of u2 completes between the report's read and the
            // hub's handling of it: the hub's implicit claim, then the app's.
            let p = st.projects.get_mut("p1").unwrap();
            assert!(p.write_hub_claim("AAA=", "u2", 1));
            p.holder_seq += 1;
            let conn = crate::api::db(&ctx2).unwrap().conn();
            live_db::add_implicit_claim(&conn, "p1", "u2", 1).unwrap();
        });
        h.full_report("p1").await.unwrap();
        let me = "AAA=".to_string();
        assert!(hub.holders_of("p1", "u1").contains(&me));
        assert!(
            !hub.holders_of("p1", "u2").contains(&me),
            "the full report removed the implicit claim it did not carry"
        );
        assert!(
            h.next_deadline().unwrap() <= Instant::now(),
            "another full report is due at once"
        );
        let done = h.flush_due(Instant::now()).await;
        assert_eq!(done.len(), 1, "the owed full report ran");
        assert!(done[0].1.is_ok());
        assert!(hub.holders_of("p1", "u1").contains(&me));
        assert!(
            hub.holders_of("p1", "u2").contains(&me),
            "the implicit claim is back on the hub"
        );
        assert!(
            h.flush_due(Instant::now()).await.is_empty(),
            "in sync: nothing more is owed"
        );
    }

    #[tokio::test]
    async fn reload_snapshots_and_reports_the_full_claim_set() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::add_implicit_claim(&conn, "p1", "u2", 1).unwrap();
        }
        let eff = h.reload("p1", "epoch-1").await.unwrap();
        assert_eq!(eff, vec![FeedEffect::ProvidersChanged("p1".into())]);
        assert!(hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
        h.forget("p1");
        assert!(h.map("p1").is_none());
    }

    #[tokio::test]
    async fn flush_project_now_sends_the_pending_outbox() {
        let (_t, ctx, hub, _h) = rig().await;
        hold(&ctx, "u2");
        flush_project_now(&ctx, "p1").await.unwrap();
        assert!(hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(live_db::outbox_len(&conn, "p1").unwrap(), 0);
    }

    /// Fix round 1, item 1: a clock whose outbox another path emptied (a
    /// publish's own flush) stops waiting at the next `flush_due` — the
    /// only deadline left is the hourly digest check, never a past instant.
    #[tokio::test]
    async fn a_clock_emptied_by_another_path_leaves_no_past_deadline() {
        let (_t, ctx, _hub, mut h) = rig().await;
        let t0 = Instant::now();
        hold(&ctx, "u1");
        h.note_append("p1", t0);
        flush_project_now(&ctx, "p1").await.unwrap();
        let later = t0 + outbox::DEFAULT_FLUSH * 5;
        assert!(h.flush_due(later).await.is_empty());
        assert_eq!(
            h.next_deadline(),
            Some(h.last_digest_check + DIGEST_CHECK_EVERY),
            "only the hourly check is left"
        );
    }

    /// Task 15 R1: an outbox that cannot be read pushes every due deadline
    /// past the back-off floor — never a deadline in the past a loop would
    /// spin on.
    #[tokio::test]
    async fn an_unreadable_outbox_pushes_the_deadlines_past_the_floor() {
        let (_t, ctx, _hub, mut h) = rig().await;
        let t0 = Instant::now();
        hold(&ctx, "u1");
        h.note_append("p1", t0);
        let late = t0 + outbox::DEFAULT_FLUSH * 5;
        crate::api::db(&ctx)
            .unwrap()
            .conn()
            .execute("DROP TABLE collab_outbox", [])
            .unwrap();
        assert!(h.flush_due(late).await.is_empty());
        let d = h.next_deadline().unwrap();
        assert!(
            d >= late + crate::collab::live::backoff::BACKOFF_FLOOR,
            "the due flush waits the floor: {:?} after the tick",
            d.checked_duration_since(late)
        );
    }

    /// Fix round 1, item 1: rows appended while a report is in flight keep a
    /// flush deadline after the report's ack.
    #[tokio::test]
    async fn rows_appended_during_a_report_keep_a_deadline() {
        let (_t, ctx, hub, mut h) = rig().await;
        hold(&ctx, "u1");
        let ctx2 = Arc::clone(&ctx);
        hub.before_next("/holders/self", move |_| hold(&ctx2, "u2"));
        assert_eq!(h.flush("p1").await.unwrap().sent, 1);
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert_eq!(
                live_db::outbox_len(&conn, "p1").unwrap(),
                1,
                "u2 is still pending"
            );
        }
        let d = h.next_deadline().unwrap();
        assert!(
            d < h.last_digest_check + DIGEST_CHECK_EVERY,
            "the pending row has its own flush deadline"
        );
        let done = h.flush_due(d).await;
        assert_eq!(done.len(), 1);
        assert!(hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
    }

    /// Fix round 1, item 3: a frame re-added while the refusing report was
    /// in flight keeps its newer claim — at another version, or at the same
    /// version with a newer outbox row.
    #[tokio::test]
    async fn a_claim_re_added_during_the_refused_report_survives() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            for g in ["ghost1", "ghost2", "ghost3"] {
                live_db::record_claim_change(
                    &conn,
                    "p1",
                    g,
                    live_db::ClaimOp::Add { content_version: 1 },
                )
                .unwrap();
            }
        }
        let ctx2 = Arc::clone(&ctx);
        hub.before_next("/holders/self", move |_| {
            let conn = crate::api::db(&ctx2).unwrap().conn();
            live_db::record_claim_change(
                &conn,
                "p1",
                "ghost1",
                live_db::ClaimOp::Add { content_version: 2 },
            )
            .unwrap();
            live_db::record_claim_change(
                &conn,
                "p1",
                "ghost2",
                live_db::ClaimOp::Add { content_version: 1 },
            )
            .unwrap();
        });
        let out = h.flush("p1").await.unwrap();
        assert_eq!(out.refused, 3);
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(
            live_db::my_claims(&conn, "p1").unwrap(),
            vec![("ghost1".to_string(), 2), ("ghost2".to_string(), 1)],
            "ghost3 (untouched since the report) is dropped; the re-added ones survive"
        );
    }

    /// Fix round 1, item 4: a page claiming more rows but carrying no cursor
    /// is an error, and the stored cursor does not move past unread rows.
    #[tokio::test]
    async fn a_has_more_page_without_next_leaves_the_cursor_alone() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let (_t, ctx, _hub, _h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_holder_seq_only(&conn, "p1", 3).unwrap();
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p1/holders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "epoch": "epoch-1", "holderSeq": 9, "floor": 0,
                "deltas": [{"device": "BBB=", "add": [[5, 1]], "rm": []}],
                "hasMore": true, "next": null
            })))
            .mount(&server)
            .await;
        let mut h = Holdings::load(
            Arc::clone(&ctx),
            CollabClient::new(server.uri()).unwrap(),
            "tok".into(),
            "AAA=".into(),
        )
        .unwrap();
        assert!(h.delta_resume("p1", "epoch-1").await.is_err());
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert_eq!(
            crate::db::collab::get_project(&conn, "p1")
                .unwrap()
                .unwrap()
                .holder_seq,
            3
        );
    }

    /// Fix round 1, item 5: a lost project is not loaded (its holder map,
    /// clock and hourly check are gone with it) — `list_projects` filters
    /// `lost_at`.
    #[tokio::test]
    async fn a_lost_project_is_not_loaded() {
        let (_t, ctx, hub, _h) = rig().await;
        hold(&ctx, "u1");
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::mark_lost(&conn, "p1").unwrap();
        }
        let mut h = Holdings::load(
            Arc::clone(&ctx),
            CollabClient::new(hub.uri()).unwrap(),
            "tok".into(),
            "AAA=".into(),
        )
        .unwrap();
        assert!(h.map("p1").is_none());
        assert_eq!(
            h.next_deadline(),
            Some(h.last_digest_check + DIGEST_CHECK_EVERY),
            "no flush clock for the lost project's rows"
        );
        let before = hub.holder_writes();
        h.hourly_digest_checks(Instant::now() + DIGEST_CHECK_EVERY)
            .await;
        assert_eq!(hub.holder_writes(), before);
        assert!(!hub.holders_of("p1", "u1").contains(&"AAA=".to_string()));
    }

    /// Fix round 1, item 9: a full report owed after a failed reload report
    /// is retried by `flush_due` once the hub is back.
    #[tokio::test]
    async fn an_owed_full_report_is_retried_by_flush_due() {
        let (_t, ctx, hub, mut h) = rig().await;
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            live_db::add_implicit_claim(&conn, "p1", "u2", 1).unwrap();
        }
        hub.set_failing("/holders/self", true);
        h.reload("p1", "epoch-1").await.unwrap(); // snapshot ok, full report owed
        assert!(!hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
        assert!(h
            .next_deadline()
            .is_some_and(|d| d < h.last_digest_check + DIGEST_CHECK_EVERY));
        hub.set_failing("/holders/self", false);
        let done = h
            .flush_due(Instant::now() + std::time::Duration::from_secs(120))
            .await;
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(done[0].1.is_ok());
        assert!(hub.holders_of("p1", "u2").contains(&"AAA=".to_string()));
        assert!(h
            .flush_due(Instant::now() + std::time::Duration::from_secs(240))
            .await
            .is_empty());
    }

    #[test]
    fn member_devices_reads_the_snapshot_nodes_and_fails_closed() {
        let mut p = crate::db::collab::CollabProjectRow {
            project_id: "p1".into(),
            members_json: serde_json::json!([
                {"accountId":"a","displayName":"A","dataRole":"send_receive","coordinator":false,"nodes":["AAA=","CCC="]},
                {"accountId":"b","displayName":"B","dataRole":"send","coordinator":false,"nodes":["BBB="]}
            ])
            .to_string(),
            ..test_project_row()
        };
        let got = member_devices(&p);
        assert_eq!(got.len(), 3);
        assert!(got.contains("BBB="));
        p.members_json = "not json".into();
        assert!(member_devices(&p).is_empty());
    }

    fn test_project_row() -> crate::db::collab::CollabProjectRow {
        crate::db::collab::CollabProjectRow {
            project_id: String::new(),
            slug: String::new(),
            title: String::new(),
            data_role: String::new(),
            is_coordinator: false,
            require_approval: false,
            pending_frames: 0,
            project_status: String::new(),
            target_name: String::new(),
            target_ra_deg: 0.0,
            target_dec_deg: 0.0,
            target_radius_deg: 0.0,
            membership_version: 0,
            snapshot_payload_b64: String::new(),
            snapshot_signature_b64: String::new(),
            members_json: "[]".into(),
            thresholds_version: None,
            thresholds_rules_json: None,
            gov_caps_json: "[]".into(),
            auto_replicate: false,
            synced_caps_json: "[]".into(),
            hub_version: 0,
            manifest_cursor: 0,
            dictionary_version: None,
            dictionary_json: None,
            policy_json: String::new(),
            replication_paused: false,
            auto_publish: false,
            fetched_at: String::new(),
            feed_epoch: None,
            holder_seq: -1,
        }
    }
}
