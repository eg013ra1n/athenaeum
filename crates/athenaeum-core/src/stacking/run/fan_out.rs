//! The per-stage fan-out: the memory admission (`admission`, `AdmissionGate`), the worker pool (`fan_out`,
//! `fan_out_groups`) and the stage-wide progress ticker (`FanOutTicker`).

use super::*;

/// The memory budget one fan-out's admission divides by, resolved ONCE per
/// fan-out from the live `compute.memory_budget_mb` (Task 13c): the setting
/// when positive, else a quarter of the memory this process may use. `bytes`
/// is `None` when auto and the total is unknown (admission 1).
#[derive(Clone, Copy, Debug)]
pub(crate) struct FanBudget {
    pub(crate) bytes: Option<u64>,
    pub(crate) why: &'static str,
}

pub(super) fn fan_budget(rc: &RunContext) -> FanBudget {
    match machine::memory_budget(&rc.ctx) {
        Some((b, why)) => FanBudget { bytes: Some(b), why },
        None => FanBudget { bytes: None, why: "unknown" },
    }
}

/// Memory-budgeted worker count for a fan-out stage (decision 3):
/// `clamp(budget / working_set, 1, max_workers)`. `mem_budget` is
/// `compute.memory_budget_mb` or, at the default, `total_ram_bytes() / 4`;
/// when it is unknown (`None`), the budget is treated as exhausted
/// (admission 1 — the conservative, single-frame-at-a-time fallback) rather
/// than guessed. `max_workers` is the app pool's width — a
/// frame worker that outnumbers the pixel pool only queues behind it (perf
/// tier 1 Task 2: every fan-out's own worker threads now hand their pixel
/// work to `image_pool`, so admitting more frame workers than the pool has
/// threads cannot buy any more parallelism, only more RAM held at once).
pub(super) fn admission(working_set_bytes: u64, max_workers: usize, mem_budget: Option<u64>) -> usize {
    let working_set = working_set_bytes.max(1);
    let n = match mem_budget {
        Some(budget) => budget / working_set,
        None => 1,
    };
    n.clamp(1, max_workers.max(1) as u64) as usize
}

/// `admission` for a fan-out whose working set also carries bytes resident
/// ONCE per group rather than once per worker (perf tier 1 Task 5 fix
/// round 1, Important finding 2) — Calibrate's preloaded master
/// subtrahend/flat planes. `shared_bytes` is subtracted from the memory
/// budget BEFORE the per-worker division (`saturating_sub`, so a shared
/// cost that exceeds the whole budget clamps to admission 1 rather than
/// underflowing), then the remainder is divided by `working_set_bytes`
/// exactly as [`admission`] does. Same `mem_budget` semantics as
/// `admission`: an unknown budget is treated as exhausted
/// (admission 1), never guessed.
pub(super) fn admission_with_shared(
    shared_bytes: u64,
    working_set_bytes: u64,
    max_workers: usize,
    mem_budget: Option<u64>,
) -> usize {
    let working_set = working_set_bytes.max(1);
    let n = match mem_budget {
        Some(budget) => budget.saturating_sub(shared_bytes) / working_set,
        None => 1,
    };
    n.clamp(1, max_workers.max(1) as u64) as usize
}

/// One line per fan-out stating what `admission` decided and from what —
/// the number the audit's Δ estimates are measured against (perf tier 1
/// Task 0). `shared_bytes` (perf tier 1 Task 5 fix round 1) is `0` for
/// every stage but Calibrate, which passes the REAL bytes its group's
/// preloaded master planes hold (see [`admission_with_shared`]).
pub(super) fn log_admission(
    rc: &RunContext,
    stage: Stage,
    group_key: &str,
    working_set_bytes: u64,
    shared_bytes: u64,
    admission: usize,
    budget: FanBudget,
    pass: &str,
) {
    tracing::info!(
        run_id = rc.run_id,
        stage = stage.as_str(),
        pass,
        group_key,
        working_set_bytes,
        shared_bytes,
        admission,
        memory_budget_bytes = machine::opt_i64(budget.bytes),
        budget_why = budget.why,
        pool_threads = rc.ctx.image_pool.current_num_threads(),
        "fan-out admitted"
    );
}

/// End a per-group meter if it is still open (`finish` consumes it; a drop is the aborted path).
pub(super) fn finish_meter(m: &mut Option<crate::stacking::telemetry::StageMeter>) {
    if let Some(m) = m.take() {
        m.finish();
    }
}

/// The live admission of one fan-out (Task 13c, S7): `start` workers are spawned, and a worker re-reads
/// `target` before EVERY pull — while `live > target` it retires (a CAS on `live`) instead of pulling. The
/// target is clamped to `[1, start]` (never above what was started, always one worker). Because the workers
/// pull from one FIFO and results land by item index, a retiring worker changes WHEN items run, never which
/// item lands where: the output is bit-identical to any other admission by construction.
///
/// The production target source re-computes the admission from the live `compute.memory_budget_mb` at most
/// once per second and only ever LOWERS the target (workers that retired do not come back).
pub(crate) struct AdmissionGate {
    start: usize,
    target: AtomicUsize,
    live: AtomicUsize,
    retired: AtomicUsize,
    source: Option<TargetSource>,
}

/// Where a production gate re-reads its target from.
struct TargetSource {
    ctx: Arc<ServiceContext>,
    working_set_bytes: u64,
    shared_bytes: u64,
    max_workers: usize,
    last_check: Mutex<Instant>,
}

/// Re-read the live budget at most this often.
const GATE_REFRESH: Duration = Duration::from_secs(1);

impl AdmissionGate {
    /// A fixed gate: `start` workers, never re-read (tests, and the shape every fan-out had before).
    #[cfg(test)]
    pub(crate) fn fixed(start: usize) -> Self {
        Self::build(start, None)
    }

    /// The production gate: `start` is the admission just computed from `working_set_bytes` (and `shared_bytes`
    /// resident once per group); the live budget lowers the target if memory shrinks mid-fan-out.
    pub(super) fn budgeted(rc: &RunContext, working_set_bytes: u64, shared_bytes: u64, start: usize) -> Self {
        Self::budgeted_on(&rc.ctx, working_set_bytes, shared_bytes, start)
    }

    fn budgeted_on(
        ctx: &Arc<ServiceContext>,
        working_set_bytes: u64,
        shared_bytes: u64,
        start: usize,
    ) -> Self {
        Self::build(
            start,
            Some(TargetSource {
                ctx: ctx.clone(),
                working_set_bytes,
                shared_bytes,
                max_workers: ctx.image_pool.current_num_threads(),
                last_check: Mutex::new(Instant::now()),
            }),
        )
    }

    fn build(start: usize, source: Option<TargetSource>) -> Self {
        let start = start.max(1);
        Self {
            start,
            target: AtomicUsize::new(start),
            live: AtomicUsize::new(0),
            retired: AtomicUsize::new(0),
            source,
        }
    }

    /// Set the target (clamped to `[1, start]`). Tests drive the gate with it.
    #[cfg(test)]
    pub(crate) fn set_target(&self, target: usize) {
        self.target
            .store(target.clamp(1, self.start), Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn target(&self) -> usize {
        self.target.load(Ordering::SeqCst)
    }

    /// Re-compute the target from the live budget (rate-limited; a no-op for a fixed gate). A failed read
    /// keeps the target and logs; the DB lock is held only inside the read, never across item work.
    fn refresh(&self, label: &FanOutLabel) {
        let Some(src) = &self.source else { return };
        let Ok(mut last) = src.last_check.try_lock() else {
            return;
        };
        if last.elapsed() < GATE_REFRESH {
            return;
        }
        *last = Instant::now();
        match machine::try_memory_budget(&src.ctx) {
            Err(e) => tracing::warn!(
                run_id = label.run_id,
                stage = label.stage.as_str(),
                error = %e,
                "fan-out admission: memory budget unreadable; keeping the target"
            ),
            Ok(budget) => {
                let why = budget.map_or("unknown", |b| b.1);
                let want = admission_with_shared(
                    src.shared_bytes,
                    src.working_set_bytes,
                    src.max_workers,
                    budget.map(|b| b.0),
                )
                .clamp(1, self.start);
                let from = self.target.load(Ordering::SeqCst);
                if want < from {
                    self.target.store(want, Ordering::SeqCst);
                    tracing::info!(
                        run_id = label.run_id,
                        stage = label.stage.as_str(),
                        group_key = %label.group_key.as_deref().unwrap_or(""),
                        from,
                        to = want,
                        budget_why = why,
                        "fan-out admission lowered"
                    );
                }
            }
        }
    }

    /// `true` when this worker must retire instead of pulling (it has already left `live`).
    fn retire_if_over_target(&self) -> bool {
        loop {
            let live = self.live.load(Ordering::SeqCst);
            if live <= self.target.load(Ordering::SeqCst).max(1) {
                return false;
            }
            if self
                .live
                .compare_exchange(live, live - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }
}

/// What a fan-out says about itself in `fan-out admission lowered` / `fan-out finished`.
pub(crate) struct FanOutLabel {
    pub(crate) run_id: i64,
    pub(crate) stage: Stage,
    pub(crate) group_key: Option<String>,
    /// Which fan-out of the stage ("measure", "pass1", "pass2", "rewrite", "align", ...).
    pub(crate) pass: &'static str,
}

impl FanOutLabel {
    pub(super) fn new(rc: &RunContext, stage: Stage, group_key: Option<&str>, pass: &'static str) -> Self {
        Self {
            run_id: rc.run_id,
            stage,
            group_key: group_key.map(str::to_string),
            pass,
        }
    }
}

/// Fan `items` out across `min(gate.start, items.len()).max(1)` worker
/// threads pulling from one shared FIFO queue (`std::thread::scope`) —
/// never more workers than there is work, and always at least one so a
/// non-empty `items` makes progress even when the admission itself is 0.
/// `cancel` checked before each item is pulled — never mid-item, since
/// only the item's own function (a caller that needs per-item
/// cancellation captures its OWN `&AtomicBool` into `f`) can decide that.
/// Results land at their item's ORIGINAL index in `items`; an index whose
/// item was never started because `cancel` fired first is `None` —
/// callers check `cancel` once, right after this returns, rather than
/// inspecting every entry for that case. The error type is the caller's
/// (plan 6b-3 D7: Normalize keeps its typed `LnError`). The `gate` may retire
/// workers before their next pull (see [`AdmissionGate`]); one
/// `fan-out finished` line closes the fan-out.
pub(super) fn fan_out<T, R, E, F>(
    items: Vec<T>,
    gate: &AdmissionGate,
    label: FanOutLabel,
    cancel: &AtomicBool,
    f: F,
) -> Vec<Option<Result<R, E>>>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let queue: Mutex<VecDeque<(usize, T)>> = Mutex::new(items.into_iter().enumerate().collect());
    let results: Mutex<Vec<Option<Result<R, E>>>> = Mutex::new((0..n).map(|_| None).collect());
    let workers = gate.start.min(n).max(1);
    gate.live.store(workers, Ordering::SeqCst);
    gate.retired.store(0, Ordering::SeqCst);
    // Timing only: when the last item was pulled and when each worker left, relative to `t0`.
    let t0 = Instant::now();
    let last_pull_ns = AtomicU64::new(0);
    let exit_ns_sum = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let leave = |retired: bool| {
                    exit_ns_sum.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::SeqCst);
                    if !retired {
                        gate.live.fetch_sub(1, Ordering::SeqCst);
                    }
                };
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        leave(false);
                        return;
                    }
                    gate.refresh(&label);
                    if gate.retire_if_over_target() {
                        // A worker leaving an already-drained queue was exiting anyway: not a retirement.
                        if !queue.lock().unwrap().is_empty() {
                            gate.retired.fetch_add(1, Ordering::SeqCst);
                        }
                        leave(true);
                        return;
                    }
                    let next = queue.lock().unwrap().pop_front();
                    let Some((idx, item)) = next else {
                        leave(false);
                        return;
                    };
                    last_pull_ns.fetch_max(t0.elapsed().as_nanos() as u64, Ordering::SeqCst);
                    let out = f(item);
                    results.lock().unwrap()[idx] = Some(out);
                }
            });
        }
    });

    let end_ns = t0.elapsed().as_nanos() as u64;
    let idle_ns = (workers as u64 * end_ns).saturating_sub(exit_ns_sum.load(Ordering::SeqCst));
    tracing::info!(
        run_id = label.run_id,
        stage = label.stage.as_str(),
        group_key = %label.group_key.as_deref().unwrap_or(""),
        pass = label.pass,
        items = n,
        admission = gate.start,
        workers,
        retired = gate.retired.load(Ordering::SeqCst),
        duration_ms = end_ns / 1_000_000,
        tail_ms = end_ns.saturating_sub(last_pull_ns.load(Ordering::SeqCst)) / 1_000_000,
        idle_worker_ms = idle_ns / 1_000_000,
        "fan-out finished"
    );

    results.into_inner().unwrap()
}

/// One group's side of the cross-group queue decision (Task 13c L4a): the per-worker working set and the
/// bytes the group holds resident ONCE (Calibrate's preloaded masters; `0` for a stage without any).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GroupLoad {
    pub(crate) working_set_bytes: u64,
    pub(crate) shared_bytes: u64,
}

/// What [`merged_admission`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MergedAdmission {
    /// The admission of one queue over every group: the LARGEST working set, the SUM of the shared bytes.
    pub(crate) merged: usize,
    /// The smallest / largest admission any group gets from its own queue today.
    pub(crate) per_group_min: usize,
    pub(crate) per_group_max: usize,
    /// `merged >= per_group_max`: the one queue lowers nobody's admission. Otherwise the stage keeps its
    /// per-group queues (the plan's fallback).
    pub(crate) merge: bool,
}

/// The merged-vs-per-group decision for a stage whose groups could share one queue (Task 13c L4a).
pub(crate) fn merged_admission(
    loads: &[GroupLoad],
    max_workers: usize,
    mem_budget: Option<u64>,
) -> MergedAdmission {
    let own: Vec<usize> = loads
        .iter()
        .map(|l| admission_with_shared(l.shared_bytes, l.working_set_bytes, max_workers, mem_budget))
        .collect();
    let ws_max = loads.iter().map(|l| l.working_set_bytes).max().unwrap_or(0);
    let shared_sum: u64 = loads.iter().map(|l| l.shared_bytes).sum();
    let merged = admission_with_shared(shared_sum, ws_max, max_workers, mem_budget);
    let per_group_min = own.iter().copied().min().unwrap_or(1);
    let per_group_max = own.iter().copied().max().unwrap_or(1);
    MergedAdmission {
        merged,
        per_group_min,
        per_group_max,
        merge: merged >= per_group_max,
    }
}

/// [`fan_out`] over the items of SEVERAL groups as ONE queue (Task 13c L4a): `groups[g]` are group `g`'s items
/// and `working_sets[g]` its per-item working set. The queue runs longest first — descending working set, ties
/// by (group, item) order — so the big items start early and the small ones fill the tail; the order is stable
/// and deterministic. `f(g, item)` runs one item. The results come back per group in that group's ORIGINAL item
/// order (`None` = never started because `cancel` fired), exactly as `fan_out` returns them for one group.
pub(super) fn fan_out_groups<T, R, E, F>(
    groups: Vec<Vec<T>>,
    working_sets: &[u64],
    gate: &AdmissionGate,
    label: FanOutLabel,
    cancel: &AtomicBool,
    f: F,
) -> Vec<Vec<Option<Result<R, E>>>>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(usize, T) -> Result<R, E> + Sync,
{
    let lens: Vec<usize> = groups.iter().map(Vec::len).collect();
    let mut entries: Vec<(usize, usize, u64, T)> = Vec::with_capacity(lens.iter().sum());
    for (g, items) in groups.into_iter().enumerate() {
        let ws = working_sets.get(g).copied().unwrap_or(0);
        for (i, item) in items.into_iter().enumerate() {
            entries.push((g, i, ws, item));
        }
    }
    entries.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    let slots: Vec<(usize, usize)> = entries.iter().map(|e| (e.0, e.1)).collect();
    let queue: Vec<(usize, T)> = entries.into_iter().map(|e| (e.0, e.3)).collect();
    let flat = fan_out(queue, gate, label, cancel, |(g, item)| f(g, item));
    let mut out: Vec<Vec<Option<Result<R, E>>>> = lens
        .iter()
        .map(|&n| (0..n).map(|_| None).collect())
        .collect();
    for ((g, i), res) in slots.into_iter().zip(flat) {
        out[g][i] = res;
    }
    out
}

/// Per-frame progress from INSIDE a fan-out (v0.6.3). [`RunContext::progress`]
/// needs `&mut rc`, which the workers of [`fan_out`] cannot hold, so a
/// stage builds one of these before its fan-out and the item closure ticks
/// it as each frame finishes. Measure, Register and Normalize used to
/// report only after a whole group's fan-out had RETURNED — on a real set
/// that is a bar frozen for minutes and then jumping by a group (the
/// owner's report). Same stage-wide `current`/`total` convention Calibrate
/// uses: `done` starts at whatever count the stage had already reached
/// (cached frames, earlier groups), so the bar continues across groups
/// instead of restarting; same `PROGRESS_THROTTLE_MS` throttle, with a tick
/// that lands on `total` always emitted; and the guard is held ACROSS the
/// emit (the `emit_integrate_tick` lesson), so two workers finishing at
/// once cannot deliver their ticks backwards.
pub(crate) struct FanOutTicker {
    emitter: Arc<dyn ProgressEmitter>,
    run_id: i64,
    subject: StackSubject,
    stage: Stage,
    group_key: Option<String>,
    total: usize,
    /// Sub-frame units per frame: `1` everywhere but a two-pass stage ([`Self::with_scale`]). `done` counts units;
    /// the event carries whole frames (`units / scale`) against the same `total`.
    scale: usize,
    done: Arc<AtomicUsize>,
    gate: Arc<Mutex<(Instant, usize)>>,
    /// Fix wave item 3: a byte pair FROZEN at construction — most callers
    /// (Measure/Register/Normalize) pass `(0, 0)`, since their per-frame
    /// ticks have no byte figure of their own to report. Calibrate passes
    /// the group's real `(bytes_done, bytes_total)` as they stood the
    /// instant this group's fan-out started, so its per-frame ticks keep
    /// showing a non-zero byte readout through the fan-out instead of
    /// reporting `0, 0` for the whole thing. A MERGED Calibrate queue's tickers all carry the stage-start
    /// `bytes_done` until the commit loop emits each group's real figure (final review A, M3).
    bytes_done: u64,
    bytes_total: u64,
    /// The event's `message` ([`Self::with_message`]); `None` for every stage but Register's two passes.
    message: Option<&'static str>,
}

impl FanOutTicker {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        rc: &RunContext,
        stage: Stage,
        group_key: Option<String>,
        done_so_far: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
    ) -> Self {
        Self::from_parts(
            rc.emitter.clone(),
            rc.run_id,
            &rc.subject,
            stage,
            group_key,
            done_so_far,
            total,
            bytes_done,
            bytes_total,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        emitter: Arc<dyn ProgressEmitter>,
        run_id: i64,
        subject: &StackSubject,
        stage: Stage,
        group_key: Option<String>,
        done_so_far: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
    ) -> Self {
        Self {
            emitter,
            run_id,
            subject: subject.clone(),
            stage,
            group_key,
            total,
            scale: 1,
            done: Arc::new(AtomicUsize::new(done_so_far)),
            // Armed so the FIRST tick emits: the throttle measures from the
            // last emit, and there has been none.
            gate: Arc::new(Mutex::new((
                Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS),
                0,
            ))),
            bytes_done,
            bytes_total,
            message: None,
        }
    }

    /// Every event this ticker emits carries `message` (Register's "checking the reference" / "aligning").
    pub(super) fn with_message(mut self, message: &'static str) -> Self {
        self.message = Some(message);
        self
    }

    /// Count in `scale` units per frame (Measure's two passes tick half a frame each, `scale = 2`): `done_so_far`
    /// is then in units too, and [`Self::tick_units`] advances by a number of them. The event's `current` stays in
    /// whole frames and `total` in frames, so a stage that lands on `total` still does.
    pub(super) fn with_scale(mut self, scale: usize) -> Self {
        self.scale = scale.max(1);
        self
    }

    /// One more frame finished; emits (throttled) and returns the running
    /// count.
    pub(super) fn tick(&self, frame_id: Option<i64>) -> usize {
        self.tick_units(frame_id, 1)
    }

    /// `units` more sub-frame units finished (see [`Self::with_scale`]); otherwise [`Self::tick`].
    pub(super) fn tick_units(&self, frame_id: Option<i64>, units: usize) -> usize {
        let current = self.done.fetch_add(units, Ordering::SeqCst) + units;
        let now = Instant::now();
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let (last_emit, last_current) = *gate;
        if current < last_current {
            return current;
        }
        let force = current >= self.total * self.scale;
        if !force && now.duration_since(last_emit) < Duration::from_millis(PROGRESS_THROTTLE_MS) {
            return current;
        }
        *gate = (now, current);
        let (set_id, project_id) = wire_subject(&self.subject);
        let percent = if self.total == 0 {
            100.0
        } else {
            100.0 * current as f64 / (self.total * self.scale) as f64
        };
        emit_event(
            self.emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id: self.run_id,
                set_id,
                project_id,
                stage: self.stage,
                group_key: self.group_key.clone(),
                current: current / self.scale,
                total: self.total,
                percent,
                bytes_done: self.bytes_done,
                bytes_total: self.bytes_total,
                frame_id,
                message: self.message.map(str::to_string),
            },
        );
        current
    }

    /// Count into `first`'s running count AND share its throttle / order gate (Task 13c L4a): the tickers of
    /// several groups feeding one queue count into ONE stage-wide number, and emit through one gate, so
    /// `current` never runs backwards however their items interleave and the event rate is not multiplied by
    /// the number of groups.
    pub(super) fn sharing_state(mut self, first: &FanOutTicker) -> Self {
        self.done = first.done.clone();
        self.gate = first.gate.clone();
        self
    }

    /// The count reached so far (cached + ticked).
    pub(super) fn done(&self) -> usize {
        self.done.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[path = "fan_out_tests.rs"]
mod tests;
