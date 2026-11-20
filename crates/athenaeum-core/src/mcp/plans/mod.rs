//! Plans (MCP plan Task 12): what an assistant prepares and only the user applies, in Athenaeum's own window.
//!
//! One store per host, app-wide by handle (`plan-<uuid>`), not per connection. At most one plan is undecided
//! (`preparing` / `waiting`); a new one replaces it, and the replaced plan stays readable by its id. A waiting plan
//! expires after [`PLAN_TTL`] on tokio time. [`PlanStore::answer`] is the only way a plan is applied or cancelled and
//! only the window's command calls it: no MCP tool does. The apply re-checks every file against its state at planning
//! (`apply.rs`) before the plan's [`PlanApplier`] acts on what is still fresh.
//!
//! Every change of a plan's state (and, throttled, its progress while preparing) emits `mcp-plan-updated {"planId": …}` through the host's emitter and nothing else:
//! the window reads the plan again on that event.

mod apply;
pub mod black_hole;
pub mod metadata;
mod model;
#[cfg(all(feature = "render", feature = "solver"))]
pub mod plate_solve;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;

pub use black_hole::{BlackHoleSource, PlanBlackHoleArgs};
pub use metadata::PlanMetadataArgs;
#[cfg(all(feature = "render", feature = "solver"))]
pub use plate_solve::{PlanPlateSolveArgs, SolveRunArgs};
pub use model::{
    catalog_values, disk_stamp, stamp_for_plan, stamp_target, DiskStamp, GroupSelection, ItemSnapshot, McpPlanView,
    PlanAnswer, PlanColumn, PlanContent, PlanGroup, PlanItem, PlanKind, PlanReport, PlanSelection, PlanState,
    ReportLine, NOT_REACHABLE,
};

use super::{McpHost, McpSource};
use crate::api::ApiError;
use crate::events::ProgressEmitter;

/// How long a plan waits for the user.
pub const PLAN_TTL: Duration = Duration::from_secs(3600);
/// The most items one plan may hold.
pub const PLAN_ITEM_CAP: usize = 2_000;
/// A finished plan stays readable this long ...
const KEEP_FINISHED: Duration = Duration::from_secs(24 * 3600);
/// ... and at most this many finished plans are kept.
const KEEP_AT_MOST: usize = 100;
/// Between two progress events of one plan (the last one, `done == total`, is never held back).
const PROGRESS_EVENT_EVERY: Duration = Duration::from_millis(250);
/// The one event a plan emits.
const EVENT: &str = "mcp-plan-updated";

/// What one kind of plan does when the user applies it.
pub trait PlanApplier: Send + Sync {
    /// The kind's own rules on a selection (after the generic check). `Err` is shown to the user.
    fn validate(&self, view: &McpPlanView, sel: &PlanSelection) -> Result<(), String>;
    /// Acts on `fresh`, the included items whose files and catalog values still match the plan (and whose kept copy
    /// does). `sel` is the whole selection. The report holds the applier's own skips and failures; the store adds
    /// the items it skipped itself.
    fn apply(&self, host: &McpHost, plan_id: &str, sel: &PlanSelection, fresh: &[String]) -> PlanReport;
}

pub(super) struct Content {
    snapshots: std::collections::HashMap<String, ItemSnapshot>,
    applier: Box<dyn PlanApplier>,
}

struct Plan {
    id: String,
    kind: PlanKind,
    title: String,
    source: McpSource,
    state: PlanState,
    replaced_by: Option<String>,
    expires_at: Option<String>,
    /// Decided on tokio time: the sleeper and the apply check both read it.
    deadline: Option<Instant>,
    columns: Vec<PlanColumn>,
    groups: Vec<PlanGroup>,
    notes: Vec<String>,
    report: Option<PlanReport>,
    cancel: Arc<AtomicBool>,
    content: Option<Arc<Content>>,
    emitter: Arc<dyn ProgressEmitter>,
    sleeper: Option<tokio::task::AbortHandle>,
    finished_at: Option<Instant>,
    /// When `progress` last told the window.
    last_progress_event: Option<Instant>,
}

impl Plan {
    fn view(&self) -> McpPlanView {
        McpPlanView {
            plan_id: self.id.clone(),
            kind: self.kind,
            title: self.title.clone(),
            source: self.source.clone(),
            state: self.state.clone(),
            replaced_by: self.replaced_by.clone(),
            expires_at: self.expires_at.clone(),
            columns: self.columns.clone(),
            groups: self.groups.clone(),
            notes: self.notes.clone(),
            report: self.report.clone(),
        }
    }

    /// Moves to an end state; the sleeper has nothing left to do.
    fn finish(&mut self, state: PlanState) {
        self.state = state;
        self.finished_at = Some(Instant::now());
        if let Some(s) = self.sleeper.take() {
            s.abort();
        }
    }
}

#[derive(Default)]
struct Inner {
    /// In the order the plans began; the last is the newest.
    plans: Vec<Plan>,
    /// Events to emit once the lock is released.
    pending: Vec<(Arc<dyn ProgressEmitter>, String)>,
}

impl Inner {
    fn notify(&mut self, i: usize) {
        let p = &self.plans[i];
        self.pending.push((Arc::clone(&p.emitter), p.id.clone()));
    }

    /// Expires what is due, drops what has been finished too long.
    fn sweep(&mut self) {
        let now = Instant::now();
        for i in 0..self.plans.len() {
            let p = &mut self.plans[i];
            if p.state == PlanState::Waiting && p.deadline.is_some_and(|d| now >= d) {
                p.finish(PlanState::Expired);
                tracing::info!(plan_id = %p.id, kind = p.kind.as_str(), "mcp plan expired");
                self.notify(i);
            }
        }
        self.plans.retain(|p| p.finished_at.is_none_or(|f| now.duration_since(f) < KEEP_FINISHED));
        let finished = self.plans.iter().filter(|p| p.state.is_finished()).count();
        for _ in KEEP_AT_MOST..finished {
            if let Some(i) = self.plans.iter().position(|p| p.state.is_finished()) {
                self.plans.remove(i);
            }
        }
    }
}

/// The app-wide plans of one host.
#[derive(Clone, Default)]
pub struct PlanStore {
    inner: Arc<Mutex<Inner>>,
}

fn invalid(m: impl Into<String>) -> ApiError {
    ApiError::Invalid(m.into())
}

fn state_words(s: &PlanState) -> &'static str {
    match s {
        PlanState::Preparing { .. } => "still being prepared",
        PlanState::Waiting => "waiting",
        PlanState::Applying => "already being applied",
        PlanState::Applied => "already applied",
        PlanState::Cancelled => "cancelled",
        PlanState::Replaced => "replaced by a newer plan",
        PlanState::Expired => "expired; ask the assistant to prepare it again",
        PlanState::Failed { .. } => "failed",
    }
}

impl PlanStore {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` on the store with due plans expired, then emits what it asked to emit, outside the lock.
    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let (out, pending) = {
            let mut inner = self.lock();
            inner.sweep();
            let out = f(&mut inner);
            (out, std::mem::take(&mut inner.pending))
        };
        for (emitter, id) in pending {
            emitter.emit_json(EVENT, json!({ "planId": id }));
        }
        out
    }

    /// Opens a plan in `preparing`. An undecided plan becomes `replaced` (its cancel flag set, its id still readable);
    /// the window comes to the front. The flag is the new plan's: the planning tool checks it between units of work.
    pub fn begin(&self, host: &McpHost, kind: PlanKind, title: &str) -> (String, Arc<AtomicBool>) {
        let id = format!("plan-{}", uuid::Uuid::new_v4());
        let source = host.source().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "plan began before the catalog was open");
            McpSource {
                build: host.identity().server_name.to_string(),
                version: host.identity().version.to_string(),
                catalog: String::new(),
            }
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let plan = Plan {
            id: id.clone(),
            kind,
            title: title.to_string(),
            source,
            state: PlanState::Preparing { done: 0, total: 0 },
            replaced_by: None,
            expires_at: None,
            deadline: None,
            columns: Vec::new(),
            groups: Vec::new(),
            notes: Vec::new(),
            report: None,
            cancel: Arc::clone(&cancel),
            content: None,
            emitter: Arc::clone(host.emitter()),
            sleeper: None,
            finished_at: None,
            last_progress_event: None,
        };
        self.with(|inner| {
            for i in 0..inner.plans.len() {
                let old = &mut inner.plans[i];
                if old.state.is_undecided() {
                    old.cancel.store(true, Ordering::SeqCst);
                    old.replaced_by = Some(id.clone());
                    old.finish(PlanState::Replaced);
                    tracing::info!(plan_id = %old.id, kind = old.kind.as_str(), "mcp plan replaced");
                    inner.notify(i);
                }
            }
            inner.plans.push(plan);
            let last = inner.plans.len() - 1;
            inner.notify(last);
        });
        tracing::info!(plan_id = %id, kind = kind.as_str(), "mcp plan begun");
        host.bring_to_front();
        (id, cancel)
    }

    /// How far the planning tool is. The window shows it, so the first call, the last (`done == total`) and at most
    /// one in [`PROGRESS_EVENT_EVERY`] between them emit the plan's event; the state itself is always current.
    pub fn progress(&self, id: &str, done: usize, total: usize) {
        self.with(|inner| {
            let Some(i) = inner
                .plans
                .iter()
                .position(|p| p.id == id && matches!(p.state, PlanState::Preparing { .. }))
            else {
                return;
            };
            let p = &mut inner.plans[i];
            p.state = PlanState::Preparing { done, total };
            let now = Instant::now();
            let due = p.last_progress_event.is_none_or(|t| now.duration_since(t) >= PROGRESS_EVENT_EVERY);
            if done == total || due {
                p.last_progress_event = Some(now);
                inner.notify(i);
            }
        });
    }

    /// The plan is ready: it waits for the user for [`PLAN_TTL`]. Refused when it holds more than
    /// [`PLAN_ITEM_CAP`] items (the plan then fails with that sentence) or is no longer being prepared (the user
    /// cancelled it, or a newer plan replaced it).
    pub fn publish(&self, id: &str, c: PlanContent) -> Result<(), String> {
        let items: usize = c.groups.iter().map(|g| g.items.len()).sum();
        let store = self.clone();
        self.with(|inner| {
            let i = inner.plans.iter().position(|p| p.id == id).ok_or_else(|| "this plan is gone".to_string())?;
            let p = &mut inner.plans[i];
            if !matches!(p.state, PlanState::Preparing { .. }) {
                return Err(format!("this plan is no longer being prepared ({})", state_words(&p.state)));
            }
            if items > PLAN_ITEM_CAP {
                let error = format!("this plan would hold {items} items; narrow the scope to at most 2 000");
                tracing::warn!(plan_id = %p.id, count = items, "mcp plan too large");
                p.finish(PlanState::Failed { error: error.clone() });
                inner.notify(i);
                return Err(error);
            }
            // One line for the plan, not one per file: a missing volume locks every file on it.
            let mut unreachable = c
                .groups
                .iter()
                .flat_map(|g| g.items.iter())
                .filter(|i| i.locked.as_deref() == Some(NOT_REACHABLE));
            if let Some(first) = unreachable.next() {
                let count = 1 + unreachable.count();
                let path = std::path::Path::new(&first.folder).join(&first.file_name);
                tracing::warn!(plan_id = %p.id, count, path = %path.display(), "plan items are not reachable");
            }
            let deadline = Instant::now() + PLAN_TTL;
            p.title = c.title;
            p.columns = c.columns;
            p.groups = c.groups;
            p.notes = c.notes;
            p.content = Some(Arc::new(Content { snapshots: c.snapshots, applier: c.applier }));
            p.state = PlanState::Waiting;
            p.deadline = Some(deadline);
            p.expires_at = Some(
                (chrono::Local::now() + chrono::Duration::from_std(PLAN_TTL).unwrap_or_default())
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            );
            match tokio::runtime::Handle::try_current() {
                Ok(rt) => {
                    p.sleeper = Some(
                        rt.spawn(async move {
                            tokio::time::sleep_until(deadline).await;
                            store.with(|_| ());
                        })
                        .abort_handle(),
                    );
                }
                // Without a runtime the deadline is still enforced at every read and at apply.
                Err(_) => tracing::warn!(plan_id = %p.id, "mcp plan has no timer; it expires when next read"),
            }
            tracing::info!(plan_id = %p.id, kind = p.kind.as_str(), count = items, "mcp plan published");
            inner.notify(i);
            Ok(())
        })
    }

    /// The planning tool could not finish; the plan ends `failed` with `error`.
    pub fn fail(&self, id: &str, error: String) {
        self.with(|inner| {
            let Some(i) = inner.plans.iter().position(|p| p.id == id) else { return };
            let p = &mut inner.plans[i];
            if matches!(p.state, PlanState::Preparing { .. }) {
                tracing::error!(plan_id = %p.id, kind = p.kind.as_str(), error = %error, "mcp plan failed");
                p.finish(PlanState::Failed { error });
                inner.notify(i);
            }
        });
    }

    /// A plan by its id, `None` for the newest live one (else the newest). A replaced, cancelled or expired plan is still there.
    pub fn view(&self, id: Option<&str>) -> Option<McpPlanView> {
        self.with(|inner| match id {
            Some(id) => inner.plans.iter().find(|p| p.id == id).map(Plan::view),
            // The page that reloads finds the plan that still needs it: the newest one that is being prepared, waits
            // or is being applied, else the newest plan.
            None => inner
                .plans
                .iter()
                .rev()
                .find(|p| !p.state.is_finished())
                .or_else(|| inner.plans.last())
                .map(Plan::view),
        })
    }

    /// The window's answer. `Cancel` ends a preparing or waiting plan. `Apply` takes a waiting, unexpired plan whose
    /// selection passes the generic check and the applier's own, moves it to `applying` and returns at once; the
    /// re-check and the apply run on a thread of their own and end in `applied` (or `failed`), each change emitted.
    pub fn answer(&self, host: &Arc<McpHost>, id: &str, a: PlanAnswer) -> Result<McpPlanView, ApiError> {
        match a {
            PlanAnswer::Cancel => self.cancel(id),
            PlanAnswer::Apply { selection } => self.start_apply(host, id, selection),
        }
    }

    fn cancel(&self, id: &str) -> Result<McpPlanView, ApiError> {
        self.with(|inner| {
            let i = inner
                .plans
                .iter()
                .position(|p| p.id == id)
                .ok_or_else(|| ApiError::NotFound("this plan is gone; ask the assistant for a new one".to_string()))?;
            let p = &mut inner.plans[i];
            if !p.state.is_undecided() {
                return Err(invalid(format!("this plan is {}; it cannot be cancelled", state_words(&p.state))));
            }
            p.cancel.store(true, Ordering::SeqCst);
            p.finish(PlanState::Cancelled);
            tracing::info!(plan_id = %p.id, kind = p.kind.as_str(), "mcp plan cancelled");
            let view = p.view();
            inner.notify(i);
            Ok(view)
        })
    }

    fn start_apply(&self, host: &Arc<McpHost>, id: &str, selection: PlanSelection) -> Result<McpPlanView, ApiError> {
        // Look first, without holding the lock across the applier's own validation.
        let (view, content) = self.with(|inner| {
            let p = inner
                .plans
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| ApiError::NotFound("this plan is gone; ask the assistant for a new one".to_string()))?;
            if p.state != PlanState::Waiting {
                return Err(invalid(format!("this plan is {}; it cannot be applied", state_words(&p.state))));
            }
            let content = p.content.clone().ok_or_else(|| ApiError::Internal("a waiting plan has no content".into()))?;
            Ok((p.view(), content))
        })?;
        apply::check_selection(&view, &selection).map_err(invalid)?;
        content.applier.validate(&view, &selection).map_err(invalid)?;

        let span = tracing::info_span!("mcp_plan_apply", plan_id = %id, kind = view.kind.as_str());
        // The plan may have been cancelled or replaced meanwhile: decide under the lock.
        let applying = self.with(|inner| {
            let i = inner.plans.iter().position(|p| p.id == id).ok_or_else(|| invalid("this plan is gone"))?;
            let p = &mut inner.plans[i];
            if p.state != PlanState::Waiting {
                return Err(invalid(format!("this plan is {}; it cannot be applied", state_words(&p.state))));
            }
            // What the user chose is what the report and the window show from here on.
            // A group the window left out is not applied: it shows nothing ticked and no kept copy.
            for group in &mut p.groups {
                let answer = selection.groups.iter().find(|g| g.group_id == group.id);
                group.chosen = answer.and_then(|g| g.chosen.clone());
                for item in &mut group.items {
                    item.selected = answer.is_some_and(|g| g.included.contains(&item.id));
                }
            }
            p.state = PlanState::Applying;
            if let Some(s) = p.sleeper.take() {
                s.abort();
            }
            let view = p.view();
            inner.notify(i);
            Ok(view)
        })?;

        let store = self.clone();
        let host = Arc::clone(host);
        let id = id.to_string();
        // A tracing dispatcher is per thread: carry the caller's so the span and its closing line land with it.
        let dispatch = tracing::dispatcher::get_default(tracing::Dispatch::clone);
        let spawned = std::thread::Builder::new().name("mcp-plan-apply".to_string()).spawn({
            let view = applying.clone();
            move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    span.in_scope(|| store.run_apply(host, &id, &view, &selection, content));
                });
            }
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, plan_id = %applying.plan_id, "mcp plan apply thread could not start");
            let error = format!("the apply could not start: {e}");
            self.finish_apply(&applying.plan_id, Err(apply::Failure { error: error.clone(), report: None }));
            return Err(ApiError::Internal(error));
        }
        Ok(applying)
    }

    /// The apply thread's body. Nothing here may strand the plan in `applying`: a panic anywhere in the re-check or
    /// the applier ends it `failed`. The host and the content are let go before the plan is finished, so a caller that
    /// sees the end state also sees the catalog's pool unreferenced by this thread.
    fn run_apply(&self, host: Arc<McpHost>, id: &str, view: &McpPlanView, sel: &PlanSelection, content: Arc<Content>) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply::run(&host, id, view.kind.as_str(), view, sel, &content)
        }))
        .unwrap_or_else(|_| {
            tracing::error!(plan_id = id, "mcp plan apply panicked");
            Err(apply::Failure {
                error: "applying the plan stopped unexpectedly; check the files before trying again".to_string(),
                report: None,
            })
        });
        drop(host);
        drop(content);
        self.finish_apply(id, outcome);
    }

    fn finish_apply(&self, id: &str, outcome: Result<PlanReport, apply::Failure>) {
        self.with(|inner| {
            let Some(i) = inner.plans.iter().position(|p| p.id == id) else { return };
            let p = &mut inner.plans[i];
            match outcome {
                Ok(report) => {
                    p.report = Some(report);
                    p.finish(PlanState::Applied);
                }
                Err(apply::Failure { error, report }) => {
                    tracing::error!(plan_id = %p.id, kind = p.kind.as_str(), error = %error, "mcp plan apply failed");
                    // A plan that failed while it was applying always has a report (an empty one says nothing moved);
                    // only a plan that failed while it was being prepared has none.
                    p.report = report.or_else(|| Some(PlanReport::default()));
                    p.finish(PlanState::Failed { error });
                }
            }
            inner.notify(i);
        });
    }
}

/// What a planning answer tells the assistant about where the plan is.
pub fn plan_opened_text(host: &McpHost) -> String {
    if host.has_viewer() {
        "the plan is open in Athenaeum's window".to_string()
    } else {
        "no Athenaeum page is open; open Athenaeum to decide".to_string()
    }
}
