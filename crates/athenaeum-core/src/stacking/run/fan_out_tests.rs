//! Tests of the fan-out; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;

#[test]
fn admission_reads_the_budget_none_default_and_setting() {
    let gib = 1u64 << 30;
    // Unknown budget: one worker, exactly as an unknown RAM total was.
    assert_eq!(admission(gib, 16, None), 1);
    // The default budget is a quarter of RAM: the number is today's.
    if let Some(total) = total_ram_bytes() {
        let today = (((total / 4) / gib).clamp(1, 16)) as usize;
        assert_eq!(admission(gib, 16, Some(total / 4)), today);
        assert_eq!(
            admission_with_shared(gib / 2, gib, 16, Some(total / 4)),
            (((total / 4).saturating_sub(gib / 2) / gib).clamp(1, 16)) as usize
        );
    }
    // A setting drives it, clamped to [1, max_workers].
    assert_eq!(admission(gib, 16, Some(5 * gib)), 5);
    assert_eq!(admission(gib, 4, Some(64 * gib)), 4);
    assert_eq!(admission(gib, 16, Some(gib / 4)), 1);
    assert_eq!(admission_with_shared(10 * gib, gib, 16, Some(2 * gib)), 1);
    assert_eq!(admission_with_shared(gib, gib, 16, Some(5 * gib)), 4);
}

#[test]
fn fan_budget_follows_the_live_setting() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(dir.path().join("t.db"));
    let auto = machine::memory_budget(&ctx);
    assert_eq!(auto.map(|(_, w)| w).unwrap_or("auto"), "auto");
    let conn = ctx.db.get().unwrap().conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "2048")
        .unwrap();
    drop(conn);
    assert_eq!(machine::memory_budget(&ctx), Some((2048 << 20, "setting")));
}

/// Plan 6b-2 C1: a frame-set tick still carries its set id (and a null
/// project id); a project tick carries the project id and a null set id.
#[test]
fn a_tick_carries_the_subject_pair() {
    for (subject, set_id, project_id) in [
        (
            StackSubject::FrameSet(7),
            serde_json::json!(7),
            serde_json::Value::Null,
        ),
        (
            StackSubject::Project("p1".into()),
            serde_json::Value::Null,
            serde_json::json!("p1"),
        ),
    ] {
        let rec = Arc::new(Recording::new());
        let ticker = FanOutTicker::from_parts(
            rec.clone(),
            1,
            &subject,
            Stage::Measure,
            None,
            0,
            1,
            0,
            0,
        );
        ticker.tick(Some(3));
        let last = rec
            .events(STACKING_PROGRESS_EVENT)
            .pop()
            .expect("the first tick is always emitted");
        // `.get`, so a missing key cannot pass for a null one.
        assert_eq!(last.get("setId"), Some(&set_id), "{last}");
        assert_eq!(last.get("projectId"), Some(&project_id), "{last}");
    }
}

fn gate_label(pass: &'static str) -> FanOutLabel {
    FanOutLabel {
        run_id: 77,
        stage: Stage::Measure,
        group_key: Some("g-test".to_string()),
        pass,
    }
}

/// Task 13c L4a: the cross-group queue runs longest first (descending working set, ties by group then item
/// order) and hands every group its results back in its OWN item order.
#[test]
fn fan_out_groups_runs_longest_first_and_splits_results_by_group() {
    let order = Mutex::new(Vec::<(usize, u32)>::new());
    let cancel = AtomicBool::new(false);
    // Group 0 small (ws 10), group 1 big (ws 30), group 2 small again (ws 10): one worker, so the pull
    // order is the queue order.
    let out = fan_out_groups(
        vec![vec![1u32, 2], vec![10, 20, 30], vec![100]],
        &[10, 30, 10],
        &AdmissionGate::fixed(1),
        gate_label("t"),
        &cancel,
        |g, v| {
            order.lock().unwrap().push((g, v));
            Ok::<u32, String>(v * 2)
        },
    );
    assert_eq!(
        *order.lock().unwrap(),
        vec![(1, 10), (1, 20), (1, 30), (0, 1), (0, 2), (2, 100)]
    );
    let flat: Vec<Vec<Option<Result<u32, String>>>> = vec![
        vec![Some(Ok(2)), Some(Ok(4))],
        vec![Some(Ok(20)), Some(Ok(40)), Some(Ok(60))],
        vec![Some(Ok(200))],
    ];
    assert_eq!(out, flat);
    // Admission N gives the same split.
    let wide = fan_out_groups(
        vec![vec![1u32, 2], vec![10, 20, 30], vec![100]],
        &[10, 30, 10],
        &AdmissionGate::fixed(8),
        gate_label("t"),
        &cancel,
        |_, v| Ok::<u32, String>(v * 2),
    );
    assert_eq!(wide, flat);
    // An empty group stays an empty slot.
    let with_empty = fan_out_groups(
        vec![vec![], vec![5u32]],
        &[1, 1],
        &AdmissionGate::fixed(2),
        gate_label("t"),
        &cancel,
        |_, v| Ok::<u32, String>(v),
    );
    assert_eq!(with_empty, vec![vec![], vec![Some(Ok(5))]]);
}

/// Task 13c L4a: one queue is used only when it lowers no group's admission. Uniform groups merge; a
/// group with a much bigger working set (or a big shared sum) makes the merged admission fall below a
/// small group's own, so the stage keeps its per-group queues.
#[test]
fn merged_admission_falls_back_when_it_would_lower_a_groups_admission() {
    const GIB: u64 = 1 << 30;
    let plane = |w: u64, h: u64| w * h * 4;
    // Uniform and tiny against a roomy budget: everything clamps to the pool width, so merge.
    let small = GroupLoad {
        working_set_bytes: 3 * plane(64, 48),
        shared_bytes: 2 * plane(64, 48),
    };
    let d = merged_admission(&[small, small], 4, Some(8 * GIB));
    assert_eq!((d.merged, d.per_group_min, d.per_group_max, d.merge), (4, 4, 4, true));
    // A mixed pair: mono 9576x6388 next to a tiny group on a 1 GiB budget. The big group's working set
    // sets the merged ws, so the tiny group would lose workers: fall back.
    let big = GroupLoad {
        working_set_bytes: 3 * plane(9576, 6388),
        shared_bytes: 2 * plane(9576, 6388),
    };
    let d = merged_admission(&[big, small], 16, Some(8 * GIB));
    assert!(d.merged < d.per_group_max, "{d:?}");
    assert!(!d.merge, "{d:?}");
    // Set 204 on the dev Mac (OSC 6248x4176 debayered + a mono group, 4 GiB, pool 10) falls back.
    let osc = GroupLoad {
        working_set_bytes: 5 * plane(6248, 4176),
        shared_bytes: 2 * plane(6248, 4176),
    };
    let mono = GroupLoad {
        working_set_bytes: 3 * plane(6248, 4176),
        shared_bytes: 2 * plane(6248, 4176),
    };
    let d = merged_admission(&[osc, mono], 10, Some(4 * GIB));
    assert!(!d.merge, "{d:?}");
    // An unknown budget is admission 1 everywhere: nothing to lose, merge.
    let d = merged_admission(&[big, small], 16, None);
    assert_eq!((d.merged, d.per_group_max, d.merge), (1, 1, true));
}

/// Task 13c S7: a target dropped mid-run retires workers before their next pull; the results are those
/// of admission 1 and of admission N, never more than `start` workers ran, and the run finished.
#[test]
fn fan_out_gate_dropped_mid_run_is_identical_and_bounded() {
    const N: usize = 64;
    let work = |i: usize| -> Result<u64, String> {
        std::thread::sleep(Duration::from_millis(2));
        Ok((i as u64).wrapping_mul(2_654_435_761).rotate_left(7) ^ 0xA5)
    };
    let run = |gate: &AdmissionGate, drop_to: Option<(usize, usize)>| {
        let done = AtomicUsize::new(0);
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let cancel = AtomicBool::new(false);
        let out = fan_out(
            (0..N).collect::<Vec<usize>>(),
            gate,
            gate_label("t"),
            &cancel,
            |i| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                let r = work(i);
                running.fetch_sub(1, Ordering::SeqCst);
                if let Some((after, to)) = drop_to {
                    if done.fetch_add(1, Ordering::SeqCst) + 1 == after {
                        gate.set_target(to);
                    }
                }
                r
            },
        );
        (out, peak.load(Ordering::SeqCst))
    };
    let one = run(&AdmissionGate::fixed(1), None);
    let many = run(&AdmissionGate::fixed(N), None);
    let gate = AdmissionGate::fixed(8);
    let (dropped, peak) = run(&gate, Some((10, 2)));
    assert_eq!(dropped.len(), N);
    assert!(dropped.iter().all(|r| r.is_some()), "every item ran");
    assert_eq!(dropped, one.0, "identical to admission 1");
    assert_eq!(dropped, many.0, "identical to admission N");
    assert!(peak <= 8, "never more than start workers: {peak}");
    assert_eq!(gate.target(), 2);
    assert!(gate.retired.load(Ordering::SeqCst) >= 1, "workers retired");
    // The target is clamped to [1, start].
    gate.set_target(0);
    assert_eq!(gate.target(), 1);
    gate.set_target(99);
    assert_eq!(gate.target(), 8);
    // Dropped all the way to 1 mid-run still finishes with one worker.
    let g1 = AdmissionGate::fixed(6);
    let (to_one, _) = run(&g1, Some((3, 1)));
    assert_eq!(to_one, one.0);
}

/// The `fan-out finished` line carries every fixed field; an `admission lowered` source is covered by
/// the live-budget test below.
#[test]
fn fan_out_finished_line_has_its_fields() {
    let cancel = AtomicBool::new(false);
    let gate = AdmissionGate::fixed(3);
    let (_, text) = crate::stacking::telemetry::capture_logs(|| {
        fan_out(
            vec![1u64, 2, 3, 4, 5, 6],
            &gate,
            gate_label("pass1"),
            &cancel,
            |i| {
                std::thread::sleep(Duration::from_millis(5));
                Ok::<u64, String>(i)
            },
        )
    });
    let line = text
        .lines()
        .find(|l| l.contains("fan-out finished"))
        .unwrap_or_else(|| panic!("no fan-out finished line: {text}"));
    for f in [
        "run_id=77",
        "stage=\"measure\"",
        "group_key=g-test",
        "pass=\"pass1\"",
        "items=6",
        "admission=3",
        "workers=3",
        "retired=0",
        "duration_ms=",
        "tail_ms=",
        "idle_worker_ms=",
    ] {
        assert!(line.contains(f), "{f} missing in {line}");
    }
    assert_eq!(text.matches("fan-out finished").count(), 1);
}

/// The production gate lowers its target from the live memory budget (one `warn` for an unreadable read is
/// not asserted here; this pins the lowering and its log line).
#[test]
fn fan_out_gate_lowers_from_the_live_memory_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = Arc::new(ServiceContext::new_for_tests(tmp.path().join("t.db")));
    let conn = ctx.db.get().unwrap().conn();
    // 1 MiB budget against a 64 MiB working set: admission 1.
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "1")
        .unwrap();
    drop(conn);
    let gate = AdmissionGate::budgeted_on(&ctx, 64 << 20, 0, 4);
    // The rate limit measures from construction: arm it.
    if let Some(src) = &gate.source {
        *src.last_check.lock().unwrap() = Instant::now() - Duration::from_secs(5);
    }
    let mut label = gate_label("t");
    label.run_id = 78;
    label.group_key = Some("g-live".to_string());
    let ((), text) = crate::stacking::telemetry::capture_logs(|| gate.refresh(&label));
    assert_eq!(gate.target.load(Ordering::SeqCst), 1);
    let line = text
        .lines()
        .find(|l| l.contains("fan-out admission lowered"))
        .unwrap_or_else(|| panic!("no lowered line: {text}"));
    for f in [
        "run_id=78",
        "group_key=g-live",
        "from=4",
        "to=1",
        "budget_why=\"setting",
    ] {
        assert!(line.contains(f), "{f} missing in {line}");
    }
    // Rate-limited: an immediate second refresh does nothing, and a raised budget never raises the target.
    gate.refresh(&label);
    assert_eq!(gate.target.load(Ordering::SeqCst), 1);
    // A raised budget never raises the target: persist a roomy value, let the rate limit pass, refresh.
    let conn = ctx.db.get().unwrap().conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "4096")
        .unwrap();
    drop(conn);
    if let Some(src) = &gate.source {
        *src.last_check.lock().unwrap() = Instant::now() - Duration::from_secs(5);
    }
    let ((), text2) = crate::stacking::telemetry::capture_logs(|| gate.refresh(&label));
    assert_eq!(gate.target.load(Ordering::SeqCst), 1, "{text2}");
    assert!(!text2.contains("fan-out admission lowered"), "{text2}");
}

/// v0.6.3: the fan-out stages' per-frame ticker. Two workers race 250
/// ticks through one ticker: the recorded percents never go backwards
/// (the gate is held across the emit), the first tick is emitted (the
/// gate is armed at construction), the count continues from
/// `done_so_far` instead of restarting, and the tick that lands on
/// `total` is always emitted whatever the throttle says.
#[test]
fn fan_out_ticker_is_monotonic_continues_and_always_lands_on_total() {
    struct Rec {
        events: Mutex<Vec<(usize, f64)>>,
    }
    impl ProgressEmitter for Rec {
        fn emit_json(&self, _event_name: &str, payload: serde_json::Value) {
            let current = payload["current"].as_u64().unwrap() as usize;
            let percent = payload["percent"].as_f64().unwrap();
            self.events.lock().unwrap().push((current, percent));
        }
    }
    const DONE_SO_FAR: usize = 50;
    const TICKS: usize = 250;
    const TOTAL: usize = DONE_SO_FAR + TICKS;

    let rec = Arc::new(Rec {
        events: Mutex::new(Vec::new()),
    });
    let ticker = FanOutTicker::from_parts(
        rec.clone(),
        1,
        &StackSubject::FrameSet(1),
        Stage::Measure,
        Some("g".to_string()),
        DONE_SO_FAR,
        TOTAL,
        0,
        0,
    );
    let counter = AtomicUsize::new(0);
    let worker = || loop {
        if counter.fetch_add(1, Ordering::SeqCst) >= TICKS {
            break;
        }
        ticker.tick(Some(7));
    };
    std::thread::scope(|scope| {
        scope.spawn(worker);
        scope.spawn(worker);
    });

    assert_eq!(ticker.done(), TOTAL);
    let events = rec.events.lock().unwrap().clone();
    assert!(!events.is_empty(), "no ticks recorded at all");
    // Two workers: the one whose `fetch_add` returned 52 can take the gate
    // before the one holding 51, which then drops its stale tick — so the
    // first emitted tick is 51 or 52, never a restart from 0.
    assert!(
        (DONE_SO_FAR + 1..=DONE_SO_FAR + 2).contains(&events[0].0),
        "the first tick continues from done_so_far and is emitted: {events:?}"
    );
    let mut last = -1.0f64;
    for &(_, percent) in &events {
        assert!(
            percent + 1e-9 >= last,
            "percent went backwards: {last} -> {percent}"
        );
        last = percent;
    }
    let final_event = events.last().unwrap();
    assert_eq!(
        final_event.0, TOTAL,
        "the tick on total is always emitted: {events:?}"
    );
    assert!((final_event.1 - 100.0).abs() < 1e-9, "{events:?}");
}
