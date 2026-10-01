//! Spec 2026-10-01 §5 — one place every publish-family run goes through:
//! progress (`collab-publish-progress`, throttled), exactly one
//! `collab-publish-finished`, the persisted last run, the snapshot a page
//! opened mid-run reads, and the cancel flag the compute queue also sets.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::api::collab::PublishResult;
use crate::api::{db, ApiError};
use crate::db::collab::PublishMode;
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;

pub const COLLAB_PUBLISH_PROGRESS_EVENT: &str = "collab-publish-progress";
pub const COLLAB_PUBLISH_FINISHED_EVENT: &str = "collab-publish-finished";
const THROTTLE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum PublishRunKind {
    Calibrate,
    Publish,
    Republish,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum PublishTrigger {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum PublishStage {
    Queued,
    Calibrating,
    Seeding,
    Announcing,
    Versions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum PublishOutcome {
    Done,
    Cancelled,
    Refused,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabPublishProgress {
    pub project_id: String,
    pub publish_run_id: String,
    pub kind: PublishRunKind,
    pub trigger: PublishTrigger,
    pub mode: Option<PublishMode>,
    pub stage: PublishStage,
    pub current: u32,
    pub total: u32,
    pub current_file: Option<String>,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabPublishFinished {
    pub project_id: String,
    pub publish_run_id: String,
    pub kind: PublishRunKind,
    pub trigger: PublishTrigger,
    pub outcome: PublishOutcome,
    pub calibrated: u32,
    pub announced: u32,
    pub updated: u32,
    pub stale: u32,
    pub held_back: u32,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabPublishRunView {
    pub running: Option<CollabPublishProgress>,
    pub last: Option<CollabPublishFinished>,
}

struct RunState {
    progress: Mutex<CollabPublishProgress>,
    cancel: Arc<AtomicBool>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    last_emit: Mutex<Option<Instant>>,
    /// Set by the first exit (`finish` or the guard's drop); everything after
    /// it is a no-op, so exactly one `collab-publish-finished` ever goes out.
    finished: AtomicBool,
    /// The catalog the last run is persisted into (a clone of the context's
    /// handle, so the guard can persist without a `ServiceContext`).
    db: crate::db::Database,
}

impl RunState {
    /// Persist, unregister (only this run's own entry), then emit — in that
    /// order, so a listener re-reading the run on the event sees it gone.
    fn conclude(&self, key: &str, finished: &CollabPublishFinished) {
        let project_id = finished.project_id.as_str();
        match serde_json::to_string(finished) {
            Ok(json) => {
                if let Err(e) =
                    crate::db::collab::set_last_publish_run(&self.db.conn(), project_id, &json)
                {
                    tracing::error!(project_id, error = %e, "last publish run not stored");
                }
            }
            Err(e) => {
                tracing::error!(project_id, error = %e, "last publish run not serialized")
            }
        }
        unregister(key, self);
        if let Some(em) = self.emitter.as_ref() {
            crate::events::emit_event(em.as_ref(), COLLAB_PUBLISH_FINISHED_EVENT, finished);
        }
    }
}

/// Remove `key` from the registry, but only when it still holds `state` (a
/// newer run of the same project must never be unregistered by an older one).
fn unregister(key: &str, state: &RunState) {
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    if reg
        .get(key)
        .is_some_and(|s| std::ptr::eq(Arc::as_ptr(s), state))
    {
        reg.remove(key);
    }
}

fn registry() -> &'static Mutex<HashMap<String, Arc<RunState>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<RunState>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

#[cfg(test)]
fn cancel_seams() -> &'static Mutex<HashMap<String, usize>> {
    static S: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Test seam: every later run of this project cancels itself once a tick
/// reports `current >= n` (deterministic "cancel after N frames").
#[cfg(test)]
pub(crate) fn cancel_after_frames_for_test(ctx: &ServiceContext, project_id: &str, n: usize) {
    let k = key(ctx, project_id).unwrap();
    cancel_seams().lock().unwrap().insert(k, n);
}

fn key(ctx: &ServiceContext, project_id: &str) -> Result<String, ApiError> {
    Ok(format!("{}|{project_id}", db(ctx)?.path().display()))
}

/// The handle every stage of a run reports through (cheap to clone into the
/// generation thread).
#[derive(Clone)]
pub(crate) struct RunHandle {
    key: String,
    state: Arc<RunState>,
}

/// Unregisters the run when dropped (a panic never leaves a ghost run).
pub(crate) struct RunGuard {
    key: String,
    state: Arc<RunState>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.state.finished.swap(true, Ordering::SeqCst) {
            unregister(&self.key, &self.state);
            return;
        }
        // Never finished: the future was dropped (a cancelled web handler) or
        // a panic unwound. The run still ends with exactly one event.
        let p = self
            .state
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // A detached generation thread must stop writing too.
        self.state.cancel.store(true, Ordering::SeqCst);
        tracing::warn!(project_id = %p.project_id, publish_run_id = %p.publish_run_id, "publish run interrupted");
        let finished = finished_of(
            &p,
            PublishOutcome::Failed,
            Some("run interrupted".into()),
            None,
        );
        crate::api::collab_prepare::drop_withheld_prepared_db(&self.state.db, &p.project_id);
        self.state.conclude(&self.key, &finished);
    }
}

fn finished_of(
    p: &CollabPublishProgress,
    outcome: PublishOutcome,
    error: Option<String>,
    r: Option<&PublishResult>,
) -> CollabPublishFinished {
    CollabPublishFinished {
        project_id: p.project_id.clone(),
        publish_run_id: p.publish_run_id.clone(),
        kind: p.kind,
        trigger: p.trigger,
        outcome,
        calibrated: r.map_or(0, |r| r.calibrated as u32),
        announced: r.map_or(0, |r| r.announced as u32),
        updated: r.map_or(0, |r| r.updated as u32),
        stale: r.map_or(0, |r| r.stale as u32),
        held_back: r.map_or(0, |r| r.held_back.len() as u32),
        error,
        started_at: p.started_at.clone(),
        finished_at: chrono::Utc::now().to_rfc3339(),
    }
}

impl RunHandle {
    pub(crate) fn begin(
        ctx: &ServiceContext,
        project_id: &str,
        kind: PublishRunKind,
        trigger: PublishTrigger,
        mode: Option<PublishMode>,
        emitter: Option<Arc<dyn ProgressEmitter>>,
    ) -> Result<(Self, RunGuard), ApiError> {
        let key = key(ctx, project_id)?;
        let progress = CollabPublishProgress {
            project_id: project_id.to_string(),
            publish_run_id: uuid::Uuid::new_v4().to_string(),
            kind,
            trigger,
            mode,
            stage: PublishStage::Queued,
            current: 0,
            total: 0,
            current_file: None,
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        let state = Arc::new(RunState {
            progress: Mutex::new(progress),
            cancel: Arc::new(AtomicBool::new(false)),
            emitter,
            last_emit: Mutex::new(None),
            finished: AtomicBool::new(false),
            db: db(ctx)?.clone(),
        });
        registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.clone(), Arc::clone(&state));
        let run = Self {
            key: key.clone(),
            state,
        };
        tracing::info!(project_id, publish_run_id = %run.id(), kind = ?kind, trigger = ?trigger, "publish run started");
        run.emit_progress(true);
        let guard = RunGuard {
            key,
            state: Arc::clone(&run.state),
        };
        Ok((run, guard))
    }

    pub(crate) fn id(&self) -> String {
        self.state
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .publish_run_id
            .clone()
    }

    pub(crate) fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.state.cancel)
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.state.cancel.load(Ordering::SeqCst)
    }

    pub(crate) fn stage(&self, stage: PublishStage, total: usize) {
        if self.state.finished.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut p = self
                .state
                .progress
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            p.stage = stage;
            p.total = total as u32;
            p.current = 0;
            p.current_file = None;
            tracing::info!(project_id = %p.project_id, publish_run_id = %p.publish_run_id, stage = ?stage, total, "publish run stage");
        }
        self.emit_progress(true);
    }

    pub(crate) fn tick(&self, current: usize, file: Option<&str>) {
        if self.state.finished.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut p = self
                .state
                .progress
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            p.current = current as u32;
            p.current_file = file.map(str::to_string);
        }
        #[cfg(test)]
        if cancel_seams()
            .lock()
            .unwrap()
            .get(&self.key)
            .is_some_and(|n| current >= *n)
        {
            self.state.cancel.store(true, Ordering::SeqCst);
        }
        self.emit_progress(false);
    }

    fn emit_progress(&self, force: bool) {
        if self.state.finished.load(Ordering::SeqCst) {
            return;
        }
        let Some(em) = self.state.emitter.as_ref() else {
            return;
        };
        let mut last = self
            .state
            .last_emit
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        if !force && last.is_some_and(|t| now.duration_since(t) < THROTTLE) {
            return;
        }
        *last = Some(now);
        let snapshot = self
            .state
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        crate::events::emit_event(em.as_ref(), COLLAB_PUBLISH_PROGRESS_EVENT, &snapshot);
    }

    /// The single exit: emit `collab-publish-finished`, persist it, drop withheld
    /// prepared frames left by a mid-run withhold (plan W3), unregister.
    pub(crate) fn finish(self, ctx: &ServiceContext, res: &Result<PublishResult, ApiError>) {
        if self.state.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let p = self
            .state
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let (outcome, error) = outcome_of(res, self.cancelled());
        let finished = finished_of(&p, outcome, error, res.as_ref().ok());
        crate::api::collab_prepare::drop_withheld_prepared(ctx, &p.project_id);
        tracing::info!(
            project_id = %p.project_id, publish_run_id = %p.publish_run_id, outcome = ?outcome,
            count = finished.announced + finished.calibrated + finished.updated, "publish run finished"
        );
        self.state.conclude(&self.key, &finished);
    }
}

fn outcome_of(
    res: &Result<PublishResult, ApiError>,
    cancelled: bool,
) -> (PublishOutcome, Option<String>) {
    if cancelled {
        return (PublishOutcome::Cancelled, None);
    }
    match res {
        Ok(_) => (PublishOutcome::Done, None),
        Err(ApiError::Conflict(m))
            if crate::api::collab_autopublish::is_publishing_device_refusal(m)
                || m == crate::account::client::COLLAB_API_OUTDATED_MSG =>
        {
            (PublishOutcome::Refused, Some(m.clone()))
        }
        Err(e) => (PublishOutcome::Failed, Some(e.to_string())),
    }
}

pub(crate) fn is_active(ctx: &ServiceContext, project_id: &str) -> bool {
    key(ctx, project_id)
        .map(|k| {
            registry()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains_key(&k)
        })
        .unwrap_or(false)
}

pub fn get_collab_publish_run(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<CollabPublishRunView, ApiError> {
    let running = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key(ctx, project_id)?)
        .map(|s| s.progress.lock().unwrap_or_else(|p| p.into_inner()).clone());
    let last = {
        let d = db(ctx)?;
        let json = crate::db::collab::last_publish_run(&d.conn(), project_id)
            .map_err(crate::api::collab::internal)?;
        match json
            .as_deref()
            .map(serde_json::from_str::<CollabPublishFinished>)
        {
            Some(Ok(f)) => Some(f),
            Some(Err(e)) => {
                tracing::warn!(project_id, error = %e, "stored last publish run unreadable; ignored");
                None
            }
            None => None,
        }
    };
    Ok(CollabPublishRunView { running, last })
}

pub fn cancel_collab_publish(ctx: &ServiceContext, project_id: &str) -> Result<(), ApiError> {
    match registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key(ctx, project_id)?)
    {
        Some(s) => {
            s.cancel.store(true, Ordering::SeqCst);
            tracing::info!(project_id, publish_run_id = %s.progress.lock().unwrap_or_else(|p| p.into_inner()).publish_run_id, "publish run cancel requested");
        }
        None => tracing::debug!(project_id, "publish cancel: no run"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support::Recorder;

    fn ctx_with_project() -> (tempfile::TempDir, crate::services::ServiceContext) {
        let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        // `db/collab.rs`'s test `sample_row` (≈754), made `pub(crate)` for this.
        crate::db::collab::upsert_project(&conn, &crate::db::collab::tests::sample_row("p1"))
            .unwrap();
        drop(conn);
        (tmp, ctx)
    }

    #[test]
    fn progress_is_throttled_but_stages_go_out_at_once() {
        let (_t, ctx) = ctx_with_project();
        let rec = std::sync::Arc::new(Recorder::default());
        let (run, _guard) = RunHandle::begin(
            &ctx,
            "p1",
            PublishRunKind::Calibrate,
            PublishTrigger::Manual,
            None,
            Some(rec.clone()),
        )
        .unwrap();
        run.stage(PublishStage::Calibrating, 10);
        for i in 1..=10 {
            run.tick(i, Some("c_x.fits"));
        }
        let p = rec.payloads(COLLAB_PUBLISH_PROGRESS_EVENT);
        assert_eq!(p[0]["stage"], "queued");
        assert_eq!(p[1]["stage"], "calibrating");
        assert!(
            p.len() <= 3,
            "ten ticks inside 300 ms collapse: {}",
            p.len()
        );
        let view = get_collab_publish_run(&ctx, "p1").unwrap();
        assert_eq!(
            view.running.unwrap().current,
            10,
            "the snapshot is always current"
        );
    }

    #[test]
    fn finish_emits_once_persists_and_unregisters() {
        let (_t, ctx) = ctx_with_project();
        let rec = std::sync::Arc::new(Recorder::default());
        let (run, guard) = RunHandle::begin(
            &ctx,
            "p1",
            PublishRunKind::Publish,
            PublishTrigger::Auto,
            None,
            Some(rec.clone()),
        )
        .unwrap();
        let res: Result<crate::api::collab::PublishResult, ApiError> =
            Ok(crate::api::collab::PublishResult {
                announced: 3,
                ..Default::default()
            });
        run.finish(&ctx, &res);
        drop(guard);
        let f = rec.payloads(COLLAB_PUBLISH_FINISHED_EVENT);
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0]["outcome"].as_str(), f[0]["announced"].as_u64()),
            (Some("done"), Some(3))
        );
        let view = get_collab_publish_run(&ctx, "p1").unwrap();
        assert!(view.running.is_none());
        assert_eq!(
            view.last.unwrap().announced,
            3,
            "read back from last_publish_run"
        );
        assert!(!is_active(&ctx, "p1"));
    }

    #[test]
    fn cancel_sets_the_flag_and_a_cancelled_run_reports_cancelled() {
        let (_t, ctx) = ctx_with_project();
        let rec = std::sync::Arc::new(Recorder::default());
        let (run, _g) = RunHandle::begin(
            &ctx,
            "p1",
            PublishRunKind::Calibrate,
            PublishTrigger::Manual,
            None,
            Some(rec.clone()),
        )
        .unwrap();
        cancel_collab_publish(&ctx, "p1").unwrap();
        assert!(run.cancelled() && run.cancel_flag().load(std::sync::atomic::Ordering::SeqCst));
        run.finish(&ctx, &Ok(Default::default()));
        assert_eq!(
            rec.payloads(COLLAB_PUBLISH_FINISHED_EVENT)[0]["outcome"],
            "cancelled"
        );
        cancel_collab_publish(&ctx, "p1").unwrap(); // no run: a no-op
    }

    #[test]
    fn a_refusal_is_refused_and_other_errors_are_failed() {
        let (_t, ctx) = ctx_with_project();
        for (err, want) in [
            (
                ApiError::Conflict(format!(
                    "{}:Observatory",
                    crate::account::client::COLLAB_PUBLISHING_DEVICE
                )),
                "refused",
            ),
            (ApiError::Internal("disk full".into()), "failed"),
        ] {
            let rec = std::sync::Arc::new(Recorder::default());
            let (run, _g) = RunHandle::begin(
                &ctx,
                "p1",
                PublishRunKind::Publish,
                PublishTrigger::Manual,
                None,
                Some(rec.clone()),
            )
            .unwrap();
            run.finish(&ctx, &Err(err));
            assert_eq!(
                rec.payloads(COLLAB_PUBLISH_FINISHED_EVENT)[0]["outcome"],
                want
            );
        }
    }

    #[test]
    fn a_guard_dropped_without_finish_emits_one_failed_and_persists_it() {
        let (_t, ctx) = ctx_with_project();
        let rec = std::sync::Arc::new(Recorder::default());
        let (run, guard) = RunHandle::begin(
            &ctx,
            "p1",
            PublishRunKind::Publish,
            PublishTrigger::Manual,
            None,
            Some(rec.clone()),
        )
        .unwrap();
        let cancel = run.cancel_flag();
        drop(run);
        drop(guard);
        assert!(
            cancel.load(Ordering::SeqCst),
            "an interrupted run is cancelled"
        );
        let f = rec.payloads(COLLAB_PUBLISH_FINISHED_EVENT);
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0]["outcome"].as_str(), f[0]["error"].as_str()),
            (Some("failed"), Some("run interrupted"))
        );
        let view = get_collab_publish_run(&ctx, "p1").unwrap();
        assert!(view.running.is_none());
        assert_eq!(view.last.unwrap().outcome, PublishOutcome::Failed);
    }

    #[test]
    fn finish_twice_emits_once_and_a_tick_after_finish_emits_nothing() {
        let (_t, ctx) = ctx_with_project();
        let rec = std::sync::Arc::new(Recorder::default());
        let (run, guard) = RunHandle::begin(
            &ctx,
            "p1",
            PublishRunKind::Publish,
            PublishTrigger::Manual,
            None,
            Some(rec.clone()),
        )
        .unwrap();
        let again = run.clone();
        run.finish(&ctx, &Ok(Default::default()));
        let progress_before = rec.payloads(COLLAB_PUBLISH_PROGRESS_EVENT).len();
        again.stage(PublishStage::Announcing, 5);
        again.tick(3, Some("x"));
        again.finish(&ctx, &Err(ApiError::Internal("late".into())));
        drop(guard);
        assert_eq!(rec.payloads(COLLAB_PUBLISH_FINISHED_EVENT).len(), 1);
        assert_eq!(
            rec.payloads(COLLAB_PUBLISH_PROGRESS_EVENT).len(),
            progress_before
        );
        assert_eq!(
            get_collab_publish_run(&ctx, "p1")
                .unwrap()
                .last
                .unwrap()
                .outcome,
            PublishOutcome::Done
        );
    }

    #[test]
    fn the_run_is_unregistered_before_the_finished_event() {
        struct Probe {
            key: String,
            seen: Mutex<Option<bool>>,
        }
        impl ProgressEmitter for Probe {
            fn emit_json(&self, event: &str, _payload: serde_json::Value) {
                if event == COLLAB_PUBLISH_FINISHED_EVENT {
                    let registered = registry().lock().unwrap().contains_key(&self.key);
                    *self.seen.lock().unwrap() = Some(registered);
                }
            }
        }
        let (_t, ctx) = ctx_with_project();
        let probe = Arc::new(Probe {
            key: key(&ctx, "p1").unwrap(),
            seen: Mutex::new(None),
        });
        let (run, _g) = RunHandle::begin(
            &ctx,
            "p1",
            PublishRunKind::Publish,
            PublishTrigger::Manual,
            None,
            Some(probe.clone()),
        )
        .unwrap();
        run.finish(&ctx, &Ok(Default::default()));
        assert_eq!(
            *probe.seen.lock().unwrap(),
            Some(false),
            "gone when the event fires"
        );
    }
}
