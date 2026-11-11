//! The rig the run's tests share (`seed_ready`, `Recording`, `Pass1Rig`, the seeded groups and projects) and the
//! tests of a run as a whole: start and cancel, the thread's one `stacking-complete`, a full run and its rerun. A
//! stage's own tests are in `<stage>_tests.rs`, a child module of the stage's file.

use super::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, shifted_stars, star_light_spec, wait_for_run, LightSpec,
    BASE_STARS, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

/// A frame nothing has measured yet, for the summary-row tests of Plan B B0.
pub(super) fn bare_measured_frame(frame_id: i64) -> MeasuredFrame {
    use crate::stacking::groups::GroupFrame;
    MeasuredFrame {
        frame: GroupFrame {
            frame_id,
            file_id: frame_id,
            filename: "f.fits".into(),
            path: "/f.fits".into(),
            size: 1,
            modified_at: "2026-01-01".into(),
            exposure_s: Some(60.0),
            date_obs: None,
            width: 64,
            height: 48,
            pixel_scale_arcsec: None,
            scale_source: None,
        },
        calibrated: None,
        planes: 1,
        measurement: None,
        weight: None,
        included: true,
        reason: None,
        registration: None,
        cached_calibrated: false,
        cached_metrics: false,
        ln_scale: None,
        cached_ln: false,
        ln_growth_factor: None,
        ln_growth_pairs: None,
        ln_aperture_correction: None,
        measure_floored: false,
    }
}

pub(super) fn light_spec<'a>(stem: &'a str, date_obs: &'a str) -> LightSpec<'a> {
    LightSpec {
        stem,
        instrume: "cam",
        filter: None,
        binning: 1,
        width: 64,
        height: 48,
        exptime: 60.0,
        date_obs,
        bayerpat: None,
        write_file: true,
    }
}

pub(super) const THREE_TIMES: [&str; 3] = [
    "2025-01-01T00:00:00",
    "2025-01-01T00:05:00",
    "2025-01-01T00:10:00",
];
pub(super) const SET_NAME: &str = "LDN 1272";

pub(super) struct Recording(pub(super) std::sync::Mutex<Vec<(String, serde_json::Value)>>);

impl Recording {
    pub(super) fn new() -> Self {
        Recording(std::sync::Mutex::new(Vec::new()))
    }
    pub(super) fn events(&self, name: &str) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

impl ProgressEmitter for Recording {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        self.0
            .lock()
            .unwrap()
            .push((event_name.to_string(), payload));
    }
}

/// Seed a ready-to-run fixture (3 lights + linked master dark/flat +
/// `stacking.working_dir`/`stacking.output_dir`) onto `ctx`'s OWN
/// on-disk catalog (`db_path` — the SAME path `ctx`'s pooled `Database`
/// was opened on), by opening a second, plain `Connection` to that file
/// and driving it through the SAME `test_fixtures` helpers `plan.rs`/
/// `groups.rs`'s tests use. A `ServiceContext`'s pooled `Database` can
/// only see rows committed to an on-disk file — never a private
/// in-memory `Connection` — so this is the one way to reuse the proven
/// fixture builder for a `start_stacking`/`RunContext`-level test.
pub(super) fn seed_ready(
    db_path: &Path,
    set_name: &str,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, t) in THREE_TIMES.iter().enumerate() {
        let (id, _path) = test_fixtures::add_light(&fixture, &light_spec(&format!("f{i}"), t));
        light_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_WORKING_DIR,
        working.path().to_str().unwrap(),
    )
    .unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_OUTPUT_DIR,
        output.path().to_str().unwrap(),
    )
    .unwrap();

    (fixture, light_ids, working, output)
}

// ── start_stacking / cancel_stacking ────────────────────────────────

#[test]
fn start_refuses_blocked_plans_and_double_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    // No lights at all yet: the plan is blocked ("frames" — fewer than
    // 3 included) — start refuses with Invalid.
    let empty_conn = rusqlite::Connection::open(&db_path).unwrap();
    let empty_fixture = test_fixtures::frame_set_with_conn(empty_conn, "Empty");
    let err = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(empty_fixture.set_id),
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    drop(empty_fixture);

    // A ready fixture (masters + folders), on the SAME catalog. Fix
    // round 1, item 6: hold the queue's one slot so the first run
    // parks in `acquire` — start twice → the second is a Conflict
    // deterministically, with no race against the first run's own
    // completion (same technique as `cancel_before_admission_finishes_cancelled`).
    let (fixture, light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);
    let _ = &light_ids;

    let hold_flag = Arc::new(AtomicBool::new(false));
    let (hold_permit, _hold_job) = ctx
        .compute_queue
        .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
        .unwrap();

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("first start should succeed");

    let second = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(second, ApiError::Conflict(_)), "{second:?}");

    cancel_stacking(&ctx, started.run_id).unwrap();
    drop(hold_permit);

    wait_for_run(&ctx, started.run_id);
}

/// Plan 5b final fix wave, review finding B2: a failure AFTER the
/// `active_stacks` handle is registered but BEFORE the run thread
/// actually spawns must never wedge the frame set. Before
/// `StartStackingGuard` existed, such a failure (here: the per-group
/// `insert_group` loop, via the brief's own suggested fault — a
/// `stacking_run_groups` write failing) returned via `?` straight past
/// all cleanup: the handle stayed in `active_stacks` forever (a
/// process-memory map `heal_interrupted_runs` cannot see past) and the
/// `stacking_runs` row stayed `"planning"` forever, so every later
/// `start_stacking` for the same set answered `Conflict` until restart.
#[test]
fn start_stacking_never_wedges_a_set_when_insert_group_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);

    // Fault injection: the per-group insert loop writes to
    // `stacking_run_groups`; `insert_run` (the row that must NOT be
    // left stuck) and the handle registration both land against
    // `stacking_runs`, an untouched table, so they still succeed.
    fixture
        .conn
        .execute("DROP TABLE stacking_run_groups", [])
        .unwrap();

    let err = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect_err("insert_group failing must surface as an error, not wedge the set");
    assert!(matches!(err, ApiError::Internal(_)), "{err:?}");

    assert!(
        ctx.active_stacks.lock().unwrap().is_empty(),
        "the guard must remove the handle on this early-return path"
    );

    let rows = crate::db::stacking::list_runs(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].status, "failed", "must never stay stuck planning");
    assert!(rows[0].finished_at.is_some());
    assert!(rows[0].error.is_some());

    // Undo the fault and confirm the set is no longer wedged: a second
    // `start_stacking` is admitted, not `Conflict`.
    crate::db::init_db(&fixture.conn).unwrap();
    let second = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("a second start on the same set must be admitted after the failed setup");

    cancel_stacking(&ctx, second.run_id).unwrap();
    wait_for_run(&ctx, second.run_id);
}

#[test]
fn cancel_before_admission_finishes_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);

    // Occupy the queue's one slot with a dummy Analysis permit from this
    // test thread, so the stacking run below is admitted only once it
    // is dropped.
    let hold_flag = Arc::new(AtomicBool::new(false));
    let (hold_permit, _hold_job) = ctx
        .compute_queue
        .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
        .unwrap();

    let recorder = Arc::new(Recording::new());
    let started = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("start should succeed even while the queue is held");

    // Still queued behind the held permit — must not be 'running' yet.
    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_ne!(row.status, "running", "must not be running while queued");

    cancel_stacking(&ctx, started.run_id).unwrap();
    drop(hold_permit);

    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "cancelled");
    assert!(row.finished_at.is_some());

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
    assert_eq!(completes[0]["success"].as_bool(), Some(false));
    assert_eq!(completes[0]["setId"], serde_json::json!(fixture.set_id));
    assert!(
        completes[0].get("projectId") == Some(&serde_json::Value::Null),
        "{:?}",
        completes[0]
    );

    // Never even reached "running" — the queue wait handles it before
    // `run_pipeline` gets that far.
    let running_events: Vec<_> = recorder
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["stage"] == "calibrate")
        .collect();
    assert!(
        running_events.is_empty(),
        "a queue-cancelled run must never reach stage 1: {running_events:?}"
    );
}

#[test]
fn thread_emits_complete_exactly_once_even_on_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);

    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    let run_id = insert_run(
        &fixture.conn,
        &NewRun {
            subject: &StackSubject::FrameSet(fixture.set_id),
            config_json: "{}",
            config_hash: "test-hash",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.path().to_str().unwrap(),
            output_dir: output.path().to_str().unwrap(),
        },
    )
    .unwrap();

    let recorder = Arc::new(Recording::new());
    let mut rc = test_context(
        ctx.clone(),
        recorder.clone(),
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output.path().to_path_buf(),
        HashMap::new(),
    );
    rc.fail_after_stage = Some(Stage::Calibrate);
    let cancel = rc.cancel.clone();

    {
        let mut active = ctx.active_stacks.lock().unwrap();
        active.insert(
            run_id,
            StackHandle {
                cancel_flag: cancel,
                subject: StackSubject::FrameSet(fixture.set_id),
            },
        );
    }

    let handle = std::thread::Builder::new()
        .name(format!("stacking-run-{run_id}"))
        .spawn(move || {
            run_thread(rc);
        })
        .unwrap();
    handle
        .join()
        .expect("the injected panic must be caught inside run_thread, never propagate");

    wait_for_run(&ctx, run_id);

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["success"].as_bool(), Some(false));
    let error = completes[0]["error"].as_str().expect("error text present");
    assert!(error.contains("panicked"), "{error}");

    assert!(
        !ctx.active_stacks.lock().unwrap().contains_key(&run_id),
        "handle removed"
    );

    let row = crate::db::stacking::get_run(&fixture.conn, run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.error.as_deref().unwrap_or("").contains("panicked"));
}

/// The sum of `items` over the `fan-out finished` lines of `stage` and `pass` in a
/// `crate::stacking::telemetry::capture_logs` text; 0 when there is none (a stage whose work was all cached
/// opens no fan-out).
pub(super) fn fan_out_items(logs: &str, stage: &str, pass: &str) -> usize {
    logs.lines()
        .filter(|l| {
            l.contains("fan-out finished")
                && l.contains(&format!("stage=\"{stage}\""))
                && l.contains(&format!("pass=\"{pass}\""))
        })
        .map(|l| {
            let rest = l.split("items=").nth(1).expect("items field");
            rest.split_whitespace().next().unwrap().parse::<usize>().unwrap()
        })
        .sum()
}

/// A real `stacking_runs` row plus one `stacking_run_groups` row per
/// `plan_groups` entry, all FK-linked to that same run — everything
/// [`test_context`]'s `group_ids` and stages 3/5's own DB writes need
/// that [`seed_ready`]'s calibrate-only fixture never had to provide.
pub(super) fn seed_run_and_groups(
    conn: &rusqlite::Connection,
    frames_set_id: i64,
    plan_groups: &[IntegrationGroup],
    working: &Path,
    output: &Path,
) -> (i64, HashMap<String, i64>) {
    let run_id = insert_run(
        conn,
        &NewRun {
            subject: &StackSubject::FrameSet(frames_set_id),
            config_json: "{}",
            config_hash: "test-hash",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.to_str().unwrap(),
            output_dir: output.to_str().unwrap(),
        },
    )
    .unwrap();

    let mut group_ids = HashMap::new();
    for g in plan_groups {
        let (anchor_width, anchor_height) = group_anchor_geometry(g);
        let group_id = insert_group(
            conn,
            &NewGroup {
                run_id,
                group_key: &g.key,
                instrume: g.instrume.as_deref(),
                color_mode: color_mode_wire(g.color_mode),
                filter: g.filter.as_deref(),
                binning: Some(g.binning),
                width: anchor_width,
                height: anchor_height,
                exposure: g.exposure_s,
                exposure_class: None,
                frame_count: g.frames.len() as i64,
                included_count: g.frames.len() as i64,
            },
        )
        .unwrap();
        group_ids.insert(g.key.clone(), group_id);
    }
    (run_id, group_ids)
}

/// Run stages 1 through `through` (inclusive) on `rc` — the harness the
/// tests below drive instead of calling each stage function by hand.
pub(super) fn run_stages_for_test(rc: &mut RunContext, through: Stage) -> Result<(), RunError> {
    stage_calibrate(rc)?;
    if through == Stage::Calibrate {
        return Ok(());
    }
    stage_measure(rc)?;
    if through == Stage::Measure {
        return Ok(());
    }
    stage_reference(rc)?;
    if through == Stage::Reference {
        return Ok(());
    }
    stage_register(rc)?;
    Ok(())
}

/// Plan A Task 11b: the frame set's stored `metrics` rows, by frame id: (key, measurement, created at).
pub(super) fn metrics_by_frame(
    conn: &rusqlite::Connection,
    set_id: i64,
) -> HashMap<i64, (String, FrameMeasurement, String)> {
    crate::db::stacking::list_artifacts(conn, &StackSubject::FrameSet(set_id), Some("metrics"))
        .unwrap()
        .into_iter()
        .map(|a| {
            let m: FrameMeasurement =
                serde_json::from_str(a.payload_json.as_deref().unwrap()).unwrap();
            (a.frame_id.unwrap(), (a.config_hash, m, a.created_at))
        })
        .collect()
}

/// Plan A Task 11c: the `measure_frame_pass1` calls (test log) whose path lies under `root` — every pass-1 read of
/// one test's own working tree, immune to sibling tests (`FITS_WRITE_FAILURE_LOG`'s reasoning).
pub(super) fn pass1_logged_under(root: &Path) -> Vec<PathBuf> {
    crate::stacking::measure::PASS1_LOG
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(root))
        .cloned()
        .collect()
}

/// Plan A Task 11c: the `stacking_artifacts` rows of one kind, by frame id.
pub(super) fn rows_of_kind(
    conn: &rusqlite::Connection,
    set_id: i64,
    kind: &str,
) -> HashMap<i64, crate::db::stacking::StackingArtifactRow> {
    crate::db::stacking::list_artifacts(conn, &StackSubject::FrameSet(set_id), Some(kind))
        .unwrap()
        .into_iter()
        .map(|a| (a.frame_id.unwrap(), a))
        .collect()
}

/// Plan A Task 11c: four mono frames of `seed_star_group` measured one Measure stage at a time on the test pool's
/// one thread (admission 1), with what the Step 1 tests count.
pub(super) struct Pass1Rig {
    _tmp: tempfile::TempDir,
    ctx: Arc<ServiceContext>,
    pub(super) fixture: test_fixtures::Fixture,
    pub(super) working: tempfile::TempDir,
    output: tempfile::TempDir,
    pub(super) cfg: StackingConfig,
    pub(super) layout: WorkingLayout,
    pub(super) plan_groups: Vec<IntegrationGroup>,
    group_key: String,
    pub(super) subject: StackSubject,
    /// The run's frame origins when the set is attested (`test_context` assumes a plain catalog origin).
    origins: Option<FrameOrigins>,
}

/// What one Measure stage did.
pub(super) struct Pass1Out {
    pub(super) beta: Option<f64>,
    /// The `measure_frame_pass1` calls this run made.
    pub(super) logged: Vec<PathBuf>,
    pub(super) excluded: usize,
}

impl Pass1Rig {
    pub(super) fn new() -> Pass1Rig {
        Pass1Rig::build(1, false)
    }

    /// The rig on an `image_pool` of `threads` workers; `osc` seeds RGGB mosaics, which stage 1 debayers into
    /// three-plane `_d` files.
    pub(super) fn build(threads: usize, osc: bool) -> Pass1Rig {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let mut raw = ServiceContext::new_for_tests(db_path.clone());
        if threads > 1 {
            raw.image_pool = Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap(),
            );
        }
        let ctx = Arc::new(raw);
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let (fixture, light_ids, working, output) = if osc {
            seed_osc_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4])
        } else {
            seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4])
        };
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();
        let subject = StackSubject::FrameSet(fixture.set_id);
        Pass1Rig {
            _tmp: tmp,
            ctx,
            fixture,
            working,
            output,
            cfg,
            layout,
            plan_groups,
            group_key,
            subject,
            origins: None,
        }
    }

    /// An attested set (externally calibrated, ruling R12 (a)): the four lights are three-plane float FITS used IN
    /// PLACE, so each `calibrated` row points at the user's own file, outside `calibrated/` and `prepared/`.
    pub(super) fn attested() -> Pass1Rig {
        let mut rig = Pass1Rig::new();
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        make_lights_three_plane(&rig.fixture, &shifts, &[0, 1, 2, 3]);
        rig.fixture
            .conn
            .execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [rig.fixture.set_id],
            )
            .unwrap();
        let (groups, origins) = frame_source(&rig.fixture.conn, &rig.subject, &rig.cfg).unwrap();
        rig.group_key = groups[0].key.clone();
        rig.plan_groups = groups;
        rig.origins = Some(origins);
        rig
    }

    pub(super) fn run(&self, label: &str, rerun_from: Option<Stage>) -> Pass1Out {
        self.try_run(rerun_from, None)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"))
    }

    /// One Measure stage; `cancel_after = Some(n)` sets the run's cancel flag once the n-th first pass under this
    /// rig's working tree has finished (a test-only hook in `measure_frame_pass1`, no timing).
    pub(super) fn try_run(
        &self,
        rerun_from: Option<Stage>,
        cancel_after: Option<usize>,
    ) -> Result<Pass1Out, RunError> {
        let log_root = self.log_root();
        let before = pass1_logged_under(&log_root).len();
        let (run_id, group_ids) = seed_run_and_groups(
            &self.fixture.conn,
            self.fixture.set_id,
            &self.plan_groups,
            self.working.path(),
            self.output.path(),
        );
        let mut rc = test_context(
            self.ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            self.fixture.set_id,
            SET_NAME,
            self.cfg.clone(),
            self.plan_groups.clone(),
            self.layout.clone(),
            self.output.path().to_path_buf(),
            group_ids,
        );
        rc.rerun_from = rerun_from;
        if let Some(origins) = &self.origins {
            rc.origins = origins.clone();
        }
        if let Some(n) = cancel_after {
            crate::stacking::measure::PASS1_CANCEL_AFTER
                .lock()
                .unwrap()
                .push((self.working.path().to_path_buf(), n));
        }
        let result = run_stages_for_test(&mut rc, Stage::Measure);
        crate::stacking::measure::PASS1_CANCEL_AFTER
            .lock()
            .unwrap()
            .retain(|(dir, _)| dir != self.working.path());
        result?;
        Ok(Pass1Out {
            beta: crate::db::stacking::list_groups(&self.fixture.conn, run_id).unwrap()[0].beta,
            logged: pass1_logged_under(&log_root)[before..].to_vec(),
            excluded: rc.runtime_exclusions.len(),
        })
    }

    /// The folder whose first passes this rig counts: the working folder, or (an attested set, whose lights are
    /// measured in place) the folder of the fixture's own lights.
    fn log_root(&self) -> PathBuf {
        if self.origins.is_some() {
            self.fixture.dir.path().to_path_buf()
        } else {
            self.working.path().to_path_buf()
        }
    }

    pub(super) fn rows(&self, kind: &str) -> HashMap<i64, crate::db::stacking::StackingArtifactRow> {
        rows_of_kind(&self.fixture.conn, self.fixture.set_id, kind)
    }

    pub(super) fn calibrated_path(&self, frame_id: i64) -> PathBuf {
        let row = crate::db::stacking::find_artifact(
            &self.fixture.conn,
            &self.subject,
            "",
            "calibrated",
            Some(frame_id),
        )
        .unwrap()
        .unwrap();
        PathBuf::from(row.path.unwrap())
    }

    /// The root of the (not yet existing) pass-1 spill directory, named without the code under test.
    pub(super) fn measured_root(&self) -> PathBuf {
        self.layout.calibrated_root().parent().unwrap().join("measured")
    }

    pub(super) fn delete_rows(&self, kinds: &[&str]) {
        crate::db::stacking::delete_artifacts(&self.fixture.conn, &self.subject, kinds)
            .unwrap();
    }

    /// The group β moves: the stored own β of the given members goes below every other's (the
    /// `measure_reruns_pass_two_…` edit).
    pub(super) fn lower_own_betas(&self, lowered: &[(i64, f64)]) {
        let rows = metrics_by_frame(&self.fixture.conn, self.fixture.set_id);
        for &(frame_id, own) in lowered {
            let (hash, mut m, _) = rows[&frame_id].clone();
            m.channels[0].beta_own = Some(own);
            let payload = serde_json::to_string(&m).unwrap();
            upsert_artifact(
                &self.fixture.conn,
                &NewArtifact {
                    subject: &self.subject,
                    frame_id: Some(frame_id),
                    group_key: "",
                    kind: "metrics",
                    path: None,
                    config_hash: &hash,
                    size: None,
                    modified_at: None,
                    payload_json: Some(&payload),
                },
            )
            .unwrap();
        }
    }

    pub(super) fn sorted_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.rows("metrics").keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}

/// The widest `admission` any `fan-out admitted` line of `logs` reports: more than 1 means the stage really ran
/// its frames in parallel (an unknown RAM total admits one worker whatever the pool).
pub(super) fn widest_admission(logs: &str) -> usize {
    logs.lines()
        .filter(|l| l.contains("fan-out admitted"))
        .filter_map(|l| l.split(" admission=").nth(1)?.split_whitespace().next()?.parse().ok())
        .max()
        .unwrap_or(0)
}

/// Every plane of one FITS file, as bit patterns.
pub(super) fn file_plane_bits(path: &Path) -> Vec<Vec<u32>> {
    let reader = PlaneReader::open(path).unwrap();
    (0..reader.channels())
        .map(|p| reader.read_plane(p).unwrap().iter().map(|v| v.to_bits()).collect())
        .collect()
}

// ── two-pass registration reference (M4a Task 4, ruling R-M4a-5) ──────

/// [`BASE_STARS`] rotated `deg` about the fixture canvas's centre and
/// then shifted by `(dx, dy)` — the fixture shape the two-pass pick
/// exists for: one frame whose own pointing deviates from the rest of
/// its group. [`shifted_stars`] can only translate.
fn rotated_shifted_stars(deg: f64, dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
    let cx = STAR_FIELD_WIDTH as f64 / 2.0;
    let cy = STAR_FIELD_HEIGHT as f64 / 2.0;
    let (s, c) = deg.to_radians().sin_cos();
    BASE_STARS
        .iter()
        .map(|&(x, y, a)| {
            let (px, py) = (x - cx, y - cy);
            (cx + px * c - py * s + dx, cy + px * s + py * c + dy, a)
        })
        .collect()
}

/// As [`seed_star_group`], but with an explicit star field per frame —
/// that helper can only shift ONE field, and these tests need frame A
/// ROTATED against the other five.
pub(super) fn seed_star_group_with_fields(
    db_path: &Path,
    set_name: &str,
    fields: &[(Vec<(f64, f64, f64)>, f32)],
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, (stars, sigma)) in fields.iter().enumerate() {
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = star_light_spec(&stem, &date_obs);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            stars,
            600.0,
            *sigma,
            100 + i as u64,
        );
        light_ids.push(id);
    }

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_WORKING_DIR,
        working.path().to_str().unwrap(),
    )
    .unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_OUTPUT_DIR,
        output.path().to_str().unwrap(),
    )
    .unwrap();

    (fixture, light_ids, working, output)
}

/// Six frames whose pointing agrees except for frame A (`f0`), which is
/// rotated 0.8 deg AND offset (6, -5) px from the other five — and is
/// also the cleanest frame (noise 3.0 against 5.0), so stage 4's
/// best-by-weight pick lands on exactly the frame whose geometry the
/// whole set would then inherit.
///
/// The offset carries most of the deviation ON PURPOSE: these fixtures
/// are 192x144, so 0.8 deg of rotation alone displaces a corner by only
/// `0.8° · hypot(192, 144)/2 = 1.7 px` — below [`TWO_PASS_MIN_GAIN_PX`]
/// by construction, and a switch on that would be exactly the sub-pixel
/// churn the floor exists to prevent. On the real 6224x4168 frames the
/// rotation term alone is 33 px.
pub(super) fn two_pass_fields() -> Vec<(Vec<(f64, f64, f64)>, f32)> {
    vec![
        (rotated_shifted_stars(0.8, 6.0, -5.0), 3.0f32),
        (rotated_shifted_stars(0.0, 0.0, 0.0), 5.0f32),
        (rotated_shifted_stars(0.0, 1.0, 0.0), 5.0f32),
        (rotated_shifted_stars(0.0, 0.0, 1.0), 5.0f32),
        (rotated_shifted_stars(0.0, 1.0, 1.0), 5.0f32),
        (rotated_shifted_stars(0.0, 2.0, 1.0), 5.0f32),
    ]
}

/// Builds a `RunContext` over [`two_pass_fields`] and runs stages 1-4,
/// asserting that stage 4 did pick frame A — the premise every test
/// below rests on. The caller then drives stage 5 itself.
pub(super) fn two_pass_context(
    tmp: &tempfile::TempDir,
    cfg: StackingConfig,
) -> (
    Arc<ServiceContext>,
    test_fixtures::Fixture,
    Vec<i64>,
    RunContext,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) =
        seed_star_group_with_fields(&db_path, SET_NAME, &two_pass_fields());
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output_dir,
        group_ids,
    );
    run_stages_for_test(&mut rc, Stage::Reference).unwrap();
    (ctx, fixture, light_ids, rc, working, output)
}

// ── M4b Task 2: mixed pixel scales inside one group ──────────────────

/// The coarse half of the mixed-scale fixture: [`BASE_STARS`] as they
/// are, in a [`STAR_FIELD_WIDTH`]x[`STAR_FIELD_HEIGHT`] frame …
pub(super) const COARSE_W: usize = STAR_FIELD_WIDTH;
pub(super) const COARSE_H: usize = STAR_FIELD_HEIGHT;
/// … and the fine half: the SAME sky sampled twice as finely — every
/// star at twice the pixel coordinate, in a frame twice the size.
pub(super) const FINE_W: usize = STAR_FIELD_WIDTH * 2;
pub(super) const FINE_H: usize = STAR_FIELD_HEIGHT * 2;
pub(super) const FINE_SCALE_ARCSEC: f64 = 0.78;
pub(super) const COARSE_SCALE_ARCSEC: f64 = 1.56;
pub(super) const MIXED_RA_DEG: f64 = 300.0;
pub(super) const MIXED_DEC_DEG: f64 = 60.0;

/// [`BASE_STARS`] scaled about the pixel origin and dithered — the
/// same sky at `factor` times the sampling.
pub(super) fn scaled_stars(factor: f64, dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
    BASE_STARS
        .iter()
        .map(|&(x, y, a)| (x * factor + dx, y * factor + dy, a))
        .collect()
}

pub(super) fn mixed_light_spec<'a>(
    stem: &'a str,
    date_obs: &'a str,
    width: usize,
    height: usize,
) -> LightSpec<'a> {
    LightSpec {
        stem,
        instrume: "cam",
        filter: None,
        binning: 1,
        width,
        height,
        exptime: 60.0,
        date_obs,
        bayerpat: None,
        write_file: true,
    }
}

// ── M4b Task 3: native geometry mode ─────────────────────────────────

/// Two groups in ONE set, at two pixel scales and two native
/// geometries: four fine frames ([`FINE_W`]x[`FINE_H`],
/// [`FINE_SCALE_ARCSEC`]) under one filter and three coarse ones
/// ([`COARSE_W`]x[`COARSE_H`], [`COARSE_SCALE_ARCSEC`]) under another.
/// The group key is colour mode + filter + binning + exposure, so the
/// filter alone splits them — geometry and camera never do (M2 Task
/// 10) — and the fine group is the larger, so `Auto` resolves the
/// RUN-wide reference into it.
///
/// Frame 0 of each group is the cleanest of its own group (noise 3.0
/// against 5.0), so each group's own best-by-weight pick is
/// deterministic. Every frame is plate-solved: that is what lets the
/// coarse group register onto the FINE reference at all in
/// co-registered mode (a ×2 scale step needs the WCS seed, M4b Task
/// 2) — the very comparison these tests rest on.
///
/// A THIRD group of two frames (filter `SII`, coarse geometry) rides
/// along: it is below the 3-frame viability floor, so stage 5 never
/// registers it and stage 9 skips it — the case fix round 1's m3 is
/// about (`SummaryGroup::reference_frame_id` must stay `None` for a
/// group that never registered onto anything).
///
/// Returns `(fixture, fine_ids, coarse_ids, tiny_ids, working, output)`.
pub(super) fn seed_two_geometry_groups(
    db_path: &Path,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    Vec<i64>,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    let mut fine_ids = Vec::new();
    let mut coarse_ids = Vec::new();
    for (i, (dx, dy)) in [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)]
        .iter()
        .enumerate()
    {
        let date_obs = date_obs_at(i);
        let stem = format!("fine{i}");
        let mut spec = mixed_light_spec(&stem, &date_obs, FINE_W, FINE_H);
        spec.filter = Some("Ha");
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(2.0, *dx, *dy),
            600.0,
            if i == 0 { 3.0 } else { 5.0 },
            300 + i as u64,
        );
        fine_ids.push(id);
    }
    for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)].iter().enumerate() {
        let date_obs = date_obs_at(i + 4);
        let stem = format!("coarse{i}");
        let mut spec = mixed_light_spec(&stem, &date_obs, COARSE_W, COARSE_H);
        spec.filter = Some("OIII");
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(1.0, *dx, *dy),
            600.0,
            if i == 0 { 3.0 } else { 5.0 },
            310 + i as u64,
        );
        coarse_ids.push(id);
    }

    let mut tiny_ids = Vec::new();
    for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0)].iter().enumerate() {
        let date_obs = date_obs_at(i + 7);
        let stem = format!("tiny{i}");
        let mut spec = mixed_light_spec(&stem, &date_obs, COARSE_W, COARSE_H);
        spec.filter = Some("SII");
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(1.0, *dx, *dy),
            600.0,
            5.0,
            320 + i as u64,
        );
        tiny_ids.push(id);
    }

    test_fixtures::add_master_dark_and_flat(&fixture, &fine_ids, FINE_W, FINE_H);
    test_fixtures::add_master_dark_and_flat(&fixture, &coarse_ids, COARSE_W, COARSE_H);
    test_fixtures::add_master_dark_and_flat(&fixture, &tiny_ids, COARSE_W, COARSE_H);

    for &id in &fine_ids {
        test_fixtures::seed_plate_solve_wcs(
            &fixture.conn,
            id,
            FINE_W,
            FINE_H,
            MIXED_RA_DEG,
            MIXED_DEC_DEG,
            FINE_SCALE_ARCSEC,
        );
    }
    for &id in coarse_ids.iter().chain(tiny_ids.iter()) {
        test_fixtures::seed_plate_solve_wcs(
            &fixture.conn,
            id,
            COARSE_W,
            COARSE_H,
            MIXED_RA_DEG,
            MIXED_DEC_DEG,
            COARSE_SCALE_ARCSEC,
        );
    }

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_WORKING_DIR,
        working.path().to_str().unwrap(),
    )
    .unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_OUTPUT_DIR,
        output.path().to_str().unwrap(),
    )
    .unwrap();

    (fixture, fine_ids, coarse_ids, tiny_ids, working, output)
}

/// One group's summary, by key.
pub(super) fn summary_group<'a>(rc: &'a RunContext, key: &str) -> &'a SummaryGroup {
    rc.summary
        .groups
        .iter()
        .find(|g| g.key == key)
        .unwrap_or_else(|| panic!("no summary for group {key}"))
}

// ── stages 6/7/9 (Task 8): normalize, integrate, output ────────────────

/// Every canonical stage in pipeline order, used to check the SET of
/// stages a full run's progress events cover, in the order they first
/// appear.
const CANONICAL_STAGE_ORDER: [&str; 7] = [
    "calibrate",
    "measure",
    "reference",
    "register",
    "normalize",
    "integrate",
    "output",
];

/// The distinct `stage` values of `events`, in first-appearance order —
/// used to check a run's progress events cover the canonical stage
/// sequence, and that `percent` is monotonic WITHIN each `(stage,
/// groupKey)` pair's own consecutive run of events (fix round 1, item 7:
/// NOT keyed on `stage` alone — `Normalize`/`Integrate`/`Output` each
/// reset to 0% per group, so a multi-group run legitimately sees percent
/// "go backwards" moving from one group's 100% to the next group's 0%
/// within the same stage; a per-frame stage like `register` carries a
/// `groupKey` too but its `current`/`total` are cumulative across every
/// group, so keying on the pair is still correct there — it just never
/// changes group_key mid-climb).
fn stage_sequence_and_monotonic(events: &[serde_json::Value]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut last_percent: HashMap<(String, String), f64> = HashMap::new();
    for e in events {
        let stage = e["stage"].as_str().unwrap_or("").to_string();
        if !order.contains(&stage) {
            order.push(stage.clone());
        }
        let group_key = e["groupKey"].as_str().unwrap_or("").to_string();
        let percent = e["percent"].as_f64().unwrap_or(0.0);
        let key = (stage.clone(), group_key.clone());
        if let Some(&prev) = last_percent.get(&key) {
            assert!(
                percent + 1e-9 >= prev,
                "percent went backwards for stage {stage} group {group_key}: {prev} -> {percent}"
            );
        }
        last_percent.insert(key, percent);
    }
    order
}

/// Rewrites `compute.memory_budget_mb` the first time a progress event of `stage` names `group_key`: a settings
/// change landing between two groups' fan-outs (the first event of a group's own fan-out comes after that
/// group's admission line).
pub(super) struct BudgetSwitch {
    pub(super) ctx: Arc<ServiceContext>,
    pub(super) stage: Stage,
    pub(super) group_key: String,
    pub(super) to_mb: u64,
    pub(super) done: AtomicBool,
}

impl ProgressEmitter for BudgetSwitch {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if event_name != STACKING_PROGRESS_EVENT
            || payload["stage"] != self.stage.as_str()
            || payload["groupKey"] != self.group_key.as_str()
            || self.done.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let conn = self.ctx.db.get().unwrap().conn();
        self.ctx
            .settings
            .persist_setting(
                &conn,
                crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB,
                &self.to_mb.to_string(),
            )
            .unwrap();
    }
}

/// The `memory_budget_bytes` of every `fan-out admitted` line of `stage` naming `group_key`, in log order.
pub(super) fn admitted_budgets(text: &str, stage: Stage, group_key: &str) -> Vec<u64> {
    text.lines()
        .filter(|l| {
            l.contains("fan-out admitted")
                && l.contains(&format!("stage=\"{}\"", stage.as_str()))
                && l.contains(group_key)
        })
        .map(|l| {
            let rest = l.split("memory_budget_bytes=").nth(1).expect("budget field");
            rest.split_whitespace().next().unwrap().parse().unwrap()
        })
        .collect()
}

#[test]
fn full_run_writes_a_master_with_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // The default config on purpose: rejection normalization is `local`
    // since OD-1a, so this run takes the Normalize stage's seeds
    // calibration and per-frame fan-out, and `stage_sequence_and_monotonic`
    // below covers the default path's progress (pcores review r1, I3).
    let recorder = Arc::new(Recording::new());
    let started = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("start should succeed");

    wait_for_run(&ctx, started.run_id);

    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    assert_eq!(group.status, "done", "{group:?}");
    let master_path = group.master_path.clone().expect("master path recorded");
    assert!(
        Path::new(&master_path).exists(),
        "master file missing: {master_path}"
    );

    let stats: GroupStats =
        serde_json::from_str(group.stats_json.as_deref().expect("stats_json set")).unwrap();
    assert_eq!(stats.frames, 4, "{stats:?}");

    // M4d Task 3 (ruling R-M4d-4), fix round 1 m2: a run WITHOUT drizzle
    // catalogs exactly one output — the master. The drizzle + weight-map
    // case has its own test; this is the other half of the rule, that
    // stage 9 records what it WROTE and nothing it did not.
    let master_lights =
        crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
    let kinds: Vec<&str> = master_lights.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, vec!["master"], "{master_lights:?}");
    assert_eq!(master_lights[0].path, master_path);
    assert_eq!(master_lights[0].frames, group.included_count);

    let header = FitsHeader::from_path(Path::new(&master_path)).unwrap();
    assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Light"));
    assert_eq!(header.get_i32("NCOMBINE"), Some(4));
    assert_eq!(
        header.get_str("ATH_STKI").as_deref(),
        Some(started.run_id.to_string().as_str())
    );
    assert_eq!(
        header.get_str("ATH_STKG").as_deref(),
        Some(group.group_key.as_str())
    );
    // Fix round 1, item 3: `ATH_STKF` names the SOURCE frame (the
    // catalog `files.filename` stem, "f<i>" for `seed_star_group`'s
    // fixture lights) — never the calibrated artifact's own on-disk
    // name (which would need `c_`/`_d` stripping the source name never
    // does).
    //
    // Ruling R-M3-17 v2: `ATH_STKF` names the group's NORMALIZATION
    // anchor (`members[reference_idx]`, sky-penalized), which since
    // this ruling is no longer guaranteed to be `run_row.
    // reference_frame_id` (the REGISTRATION reference, auto-picked by
    // weight over the whole set). This assertion only still holds
    // because `seed_star_group`'s fixture gives every frame the SAME
    // configured background (600.0) — `s_i = w_i / sqrt(bg_i)` is then
    // monotonic in weight, so the sky-penalized anchor coincides with
    // the auto/best-by-weight registration reference by construction,
    // not because the two are the same thing in general (see the
    // dedicated `normalization_anchor_prefers_the_dark_night_over_the_
    // pinned_bright_reference` test below for a fixture where they
    // deliberately differ).
    let reference_frame_id = run_row
        .reference_frame_id
        .expect("reference frame recorded");
    let reference_index = light_ids
        .iter()
        .position(|&id| id == reference_frame_id)
        .expect("reference frame is one of the fixture's own lights");
    let expected_stem = format!("f{reference_index}");
    assert_eq!(
        header.get_str("ATH_STKF").as_deref(),
        Some(expected_stem.as_str()),
        "ATH_STKF must be the fixture light's own stem"
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let run_json_path = layout.run_json(started.run_id);
    let run_json_text = std::fs::read_to_string(&run_json_path).unwrap();
    let _summary: RunSummary =
        serde_json::from_str(&run_json_text).expect("runs/run-<id>.json parses");
    let summary_json = run_row.summary_json.clone().expect("summary_json stored");
    assert_eq!(
        run_json_text, summary_json,
        "runs/run-<id>.json and the DB's summary_json must be the SAME document"
    );

    let events = recorder.events(STACKING_PROGRESS_EVENT);
    let order = stage_sequence_and_monotonic(&events);
    assert_eq!(order, CANONICAL_STAGE_ORDER, "{order:?}");

    // I3 (pcores review r1): the Normalize row of the default path is ONE
    // count - the seeds calibration's frames, then the per-frame fan-out
    // continuing from there, to a last tick on the same total. The
    // calibration message must be among the events, or the monotonic
    // check above says nothing about it.
    let normalize: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["stage"].as_str() == Some("normalize"))
        .collect();
    assert!(
        normalize.iter().any(|e| e["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("calibrating the seeds path"))),
        "the default run takes the seeds calibration: {normalize:?}"
    );
    let climbing_totals: std::collections::BTreeSet<u64> = normalize
        .iter()
        .filter(|e| e["percent"].as_f64().unwrap_or(0.0) > 0.0)
        .map(|e| e["total"].as_u64().unwrap())
        .collect();
    assert_eq!(
        climbing_totals.len(),
        1,
        "one count from the first tick to the last: {normalize:?}"
    );
    let last = normalize.last().unwrap();
    assert_eq!(last["current"], last["total"], "{last:?}");

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["success"].as_bool(), Some(true));
    let masters = completes[0]["masters"].as_array().unwrap();
    assert_eq!(masters.len(), 1, "{masters:?}");
    assert_eq!(
        masters[0]["groupKey"].as_str(),
        Some(group.group_key.as_str())
    );
}

#[test]
fn rerun_reuses_everything_and_only_integrates() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let started1 = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("first run should succeed");
    wait_for_run(&ctx, started1.run_id);
    let row1 = crate::db::stacking::get_run(&fixture.conn, started1.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row1.status, "done", "{row1:?}");
    let groups1 = crate::db::stacking::list_groups(&fixture.conn, started1.run_id).unwrap();
    let master_path1 = groups1[0].master_path.clone().expect("first master path");

    let started2 = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("second run should succeed");
    wait_for_run(&ctx, started2.run_id);
    let row2 = crate::db::stacking::get_run(&fixture.conn, started2.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row2.status, "done", "{row2:?}");

    let summary2: RunSummary =
        serde_json::from_str(row2.summary_json.as_deref().unwrap()).unwrap();
    let mut any_frame_checked = false;
    for group in &summary2.groups {
        for f in &group.frames {
            if f.included {
                any_frame_checked = true;
                assert!(f.cached_calibrated, "{f:?}");
                assert!(f.cached_metrics, "{f:?}");
                assert!(f.cached_registration, "{f:?}");
            }
        }
    }
    assert!(
        any_frame_checked,
        "no included frame in the second run's summary"
    );

    let groups2 = crate::db::stacking::list_groups(&fixture.conn, started2.run_id).unwrap();
    let master_path2 = groups2[0].master_path.clone().expect("second master path");
    assert_ne!(
        master_path1, master_path2,
        "a rerun must never overwrite the first master"
    );
    assert!(
        master_path2.ends_with("_2.fits"),
        "expected a collision-suffixed name: {master_path2}"
    );
    assert!(Path::new(&master_path1).exists());
    assert!(Path::new(&master_path2).exists());
}

#[test]
fn cancel_mid_run_keeps_artifacts_writes_no_master() {
    /// Cancels every active stacking run for `frames_set_id` the moment
    /// the FIRST `measure`-stage progress event fires — no need to know
    /// the run id ahead of time (which `start_stacking` has not
    /// returned yet at the point the run thread's own first events can
    /// already be firing): a stacking run's cancel flag lives on its
    /// `StackHandle` in `ctx.active_stacks`, keyed by `frames_set_id`
    /// there too.
    struct CancelOnFirstMeasure {
        ctx: Arc<ServiceContext>,
        frames_set_id: i64,
        fired: std::sync::atomic::AtomicBool,
        recording: Recording,
    }
    impl ProgressEmitter for CancelOnFirstMeasure {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.recording.emit_json(event_name, payload.clone());
            if event_name == STACKING_PROGRESS_EVENT
                && payload["stage"] == "measure"
                && !self.fired.swap(true, Ordering::SeqCst)
            {
                let active = self.ctx.active_stacks.lock().unwrap();
                for h in active.values() {
                    if h.subject == StackSubject::FrameSet(self.frames_set_id) {
                        h.cancel_flag.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let emitter = Arc::new(CancelOnFirstMeasure {
        ctx: ctx.clone(),
        frames_set_id: fixture.set_id,
        fired: std::sync::atomic::AtomicBool::new(false),
        recording: Recording::new(),
    });

    let started = start_stacking(
        ctx.clone(),
        emitter.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("start should succeed");

    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "cancelled", "{row:?}");

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert!(
        crate::api::sync::dir_size_bytes(&layout.calibrated_root()) > 0,
        "calibrated artifacts from stage 1 must survive a cancel"
    );

    let output_entries: Vec<_> = std::fs::read_dir(output.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        output_entries.is_empty(),
        "a cancelled run must write no master: {output_entries:?}"
    );

    let completes = emitter.recording.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
    assert_eq!(completes[0]["success"].as_bool(), Some(false));
    assert_eq!(completes[0]["masters"].as_array().map(|v| v.len()), Some(0));
}

#[test]
fn delete_intermediates_cleans_the_working_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;
    // Perf tier A Task 6a: the registered artifact is now written
    // unconditionally (`registration.write_registered_frames` no
    // longer gates it — see the field's own doc), so `registered_root()`
    // always has content before cleanup runs; the assertion below is no
    // longer at risk of passing trivially over an empty directory.

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert_eq!(
        crate::api::sync::dir_size_bytes(&layout.calibrated_root()),
        0,
        "calibrated frames must be gone after DeleteIntermediates"
    );
    assert_eq!(
        crate::api::sync::dir_size_bytes(&layout.registered_root()),
        0,
        "registered frames must be gone after DeleteIntermediates"
    );

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let master_path = groups[0].master_path.clone().expect("master path");
    assert!(
        Path::new(&master_path).exists(),
        "cleanup must never touch the output folder"
    );

    // v0.6.3: the cleanup keeps the `metrics` rows (file-less, keyed on
    // the calibration CONFIG hash — `paths::INTERMEDIATE_ARTIFACT_KINDS`),
    // so a second run re-calibrates and re-registers but does NOT
    // re-measure. Pinned end to end on the second run's own summary,
    // not on the kind list: a regression in `measurement_hash_for`'s
    // inputs (a calibrated-file identity sneaking into the hash) would
    // make every kept row a miss while the kind list still passed.
    let mut cfg2 = StackingConfig::default();
    cfg2.output.cleanup = CleanupPolicy::DeleteIntermediates;
    let second = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg2),
        None,
    )
    .expect("second start should succeed");
    wait_for_run(&ctx, second.run_id);
    let row2 = crate::db::stacking::get_run(&fixture.conn, second.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row2.status, "done", "{row2:?}");
    let summary2: RunSummary =
        serde_json::from_str(row2.summary_json.as_deref().unwrap()).unwrap();
    let frames2: Vec<&crate::stacking::provenance::SummaryFrame> = summary2
        .groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .collect();
    assert!(!frames2.is_empty());
    for f in &frames2 {
        assert!(
            !f.cached_calibrated,
            "the cleanup removed every calibrated frame, so run 2 must regenerate: {f:?}"
        );
        assert!(
            f.cached_metrics,
            "the kept metrics row must be a cache hit for the regenerated frame: {f:?}"
        );
    }
    let _ = output;
}

// ── M4d Task 1: the CFA mosaic artifact and Bayer drizzle ────────────

/// [`seed_star_group`] with a declared `RGGB` mosaic on every frame, so
/// `group_frames` puts them in ONE **OSC** group and stage 1 debayers
/// them. The pixels are the same star field the mono fixtures use — a
/// mosaic the debayer reads as one (every site carries the field's own
/// value), which is all the Bayer deposit needs: the per-colour routing
/// itself is pinned on a real per-colour fixture in
/// `drizzle::tests::bayer_drizzle_*`.
pub(super) fn seed_osc_star_group(
    db_path: &Path,
    set_name: &str,
    shifts: &[(f64, f64)],
    noise_sigmas: &[f32],
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    assert_eq!(shifts.len(), noise_sigmas.len());
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, (&(dx, dy), &sigma)) in shifts.iter().zip(noise_sigmas.iter()).enumerate() {
        let stars = shifted_stars(dx, dy);
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = test_fixtures::LightSpec {
            bayerpat: Some("RGGB"),
            ..star_light_spec(&stem, &date_obs)
        };
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            600.0,
            sigma,
            100 + i as u64,
        );
        light_ids.push(id);
    }

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_WORKING_DIR,
        working.path().to_str().unwrap(),
    )
    .unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_OUTPUT_DIR,
        output.path().to_str().unwrap(),
    )
    .unwrap();

    (fixture, light_ids, working, output)
}

// ── R-M3-17 v2: sky-penalized normalization anchor ──────────────────

/// Seeds 6 star-field lights for the sky-penalized-anchor tests
/// (ruling R-M3-17 v2): `f0`-`f2` are a bright night (exptime
/// 100/95/90s -> normalized weight 1.0/0.95/0.9 under
/// `WeightMode::Exposure`, background 3000 raw ADU), `f3`-`f5` a dark
/// night (exptime 60/55/50s -> weight 0.6/0.55/0.5, background 300 raw
/// ADU — a 10x darker sky; three of them, not two, so `referenceFrames
/// = 3` can pick a member list that is ENTIRELY dark-night — `build_
/// reference`/`build_ln_reference` refuse fewer than 3 candidates
/// outright, so a 2-frame dark night could never populate a whole LN
/// reference on its own). Weight is exact and controllable via
/// `WeightMode::Exposure` (`weight == exptime`, the same trick
/// `drizzle_over_a_min_weight_drop_matches_the_kept_frames_exact_
/// weighted_mean` uses) rather than fought for through noise/PSF
/// fitting. Zero relative shift for every frame (identity
/// registration, full reference coverage) — these tests are about
/// weight/background interaction, not registration robustness, which
/// `drizzle_runs_for_a_group_whose_members_differ_in_native_geometry_
/// from_the_reference` and the register-module tests already cover.
/// Does NOT link a master dark/flat — callers call
/// `test_fixtures::add_master_dark_and_flat` themselves.
pub(super) fn seed_sky_penalty_group(
    db_path: &Path,
    set_name: &str,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let exptimes = [100.0f64, 95.0, 90.0, 60.0, 55.0, 50.0];
    let backgrounds = [3000.0f32, 3000.0, 3000.0, 300.0, 300.0, 300.0];
    let noise = [3.0f32; 6];

    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for i in 0..6 {
        let stem = format!("f{i}");
        let date_obs = date_obs_at(i);
        let spec = LightSpec {
            stem: &stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: STAR_FIELD_WIDTH,
            height: STAR_FIELD_HEIGHT,
            exptime: exptimes[i],
            date_obs: &date_obs,
            bayerpat: None,
            write_file: true,
        };
        let stars = shifted_stars(0.0, 0.0);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            backgrounds[i],
            noise[i],
            100 + i as u64,
        );
        light_ids.push(id);
    }

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_WORKING_DIR,
        working.path().to_str().unwrap(),
    )
    .unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_OUTPUT_DIR,
        output.path().to_str().unwrap(),
    )
    .unwrap();

    (fixture, light_ids, working, output)
}

/// Review Focus 5 (heal half).
#[test]
fn the_heal_marks_an_interrupted_project_run_failed_and_leaves_frame_sets_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    crate::db::stacking::insert_test_project(&fixture.conn, "p1");

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;
    let config_json = serde_json::to_string(&cfg).unwrap();
    let new_run = |subject: &StackSubject| {
        let id = insert_run(
            &fixture.conn,
            &NewRun {
                subject,
                config_json: &config_json,
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: output.path().to_str().unwrap(),
            },
        )
        .unwrap();
        crate::db::stacking::set_run_status(&fixture.conn, id, "running").unwrap();
        id
    };
    let set_subject = StackSubject::FrameSet(fixture.set_id);
    let project_subject = StackSubject::Project("p1".into());
    let set_run = new_run(&set_subject);
    let project_run = new_run(&project_subject);
    ctx.active_stacks.lock().unwrap().insert(
        set_run,
        StackHandle {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            subject: set_subject.clone(),
        },
    );

    let project_rej = working
        .path()
        .join(crate::stacking::paths::PROJECTS_DIR)
        .join("p1")
        .join("rej")
        .join(format!("run-{project_run}"));
    std::fs::create_dir_all(&project_rej).unwrap();
    std::fs::write(project_rej.join("leftover.rej"), b"stale").unwrap();

    let healed = heal_interrupted_runs(&ctx, &fixture.conn).unwrap();
    assert_eq!(healed, 1, "only the project run is stuck");

    let project_row = crate::db::stacking::get_run(&fixture.conn, project_run)
        .unwrap()
        .unwrap();
    assert_eq!(project_row.status, "failed");
    assert_eq!(
        project_row.error.as_deref(),
        Some("interrupted by a restart")
    );
    assert!(
        !project_rej.exists(),
        "the project run's rej dir is swept through subject_layout"
    );
    let set_row = crate::db::stacking::get_run(&fixture.conn, set_run)
        .unwrap()
        .unwrap();
    assert_eq!(
        set_row.status, "running",
        "a live frame-set run is untouched"
    );
}

/// Review Focus 5 (work usage and cleanup half): a project run row and
/// project artifacts/folder do not disturb a frame set's usage or cleanup.
#[test]
fn work_usage_and_cleanup_of_a_frame_set_ignore_project_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    crate::db::stacking::insert_test_project(&fixture.conn, "p1");

    let mut cfg = StackingConfig::default();
    cfg.paths.working_dir = Some(working.path().to_string_lossy().into_owned());
    crate::api::stacking::set_stacking_config(&ctx, fixture.set_id, cfg, Vec::new()).unwrap();

    let set_group = WorkingLayout::new(working.path(), &set_slug(SET_NAME)).calibrated_dir();
    std::fs::create_dir_all(&set_group).unwrap();
    std::fs::write(set_group.join("x.bin"), vec![0u8; 1024]).unwrap();
    let project_group = working
        .path()
        .join(crate::stacking::paths::PROJECTS_DIR)
        .join("p1")
        .join("calibrated")
        .join("g");
    std::fs::create_dir_all(&project_group).unwrap();
    std::fs::write(project_group.join("y.bin"), vec![0u8; 512]).unwrap();

    let project_subject = StackSubject::Project("p1".into());
    let project_run = insert_run(
        &fixture.conn,
        &NewRun {
            subject: &project_subject,
            config_json: "{}",
            config_hash: "h",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.path().to_str().unwrap(),
            output_dir: output.path().to_str().unwrap(),
        },
    )
    .unwrap();
    crate::db::stacking::set_run_status(&fixture.conn, project_run, "running").unwrap();
    // A live handle: the project run is genuinely active, so the heal
    // sweep inside the two commands leaves it alone.
    ctx.active_stacks.lock().unwrap().insert(
        project_run,
        StackHandle {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            subject: project_subject.clone(),
        },
    );
    crate::db::stacking::upsert_artifact(
        &fixture.conn,
        &crate::db::stacking::NewArtifact {
            subject: &project_subject,
            frame_id: None,
            group_key: "g",
            kind: "calibrated",
            path: None,
            config_hash: "h",
            size: None,
            modified_at: None,
            payload_json: None,
        },
    )
    .unwrap();

    let usage = crate::api::stacking::get_stacking_work_usage(&ctx, fixture.set_id).unwrap();
    assert_eq!(
        usage.calibrated_bytes, 1024,
        "only the frame set's own folder"
    );

    crate::api::stacking::cleanup_stacking_work(&ctx, fixture.set_id, CleanupWhat::All)
        .unwrap();
    assert!(!set_group.join("x.bin").exists());
    assert!(
        project_group.join("y.bin").exists(),
        "the project folder is untouched"
    );
    assert_eq!(
        crate::db::stacking::list_artifacts(&fixture.conn, &project_subject, None)
            .unwrap()
            .len(),
        1,
        "the project's artifact row is untouched"
    );
    let run_row = crate::db::stacking::get_run(&fixture.conn, project_run)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "running");
}

pub(super) fn project_conn(db_path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn
}

/// A project run's context over the project's own frame source (plan
/// 6b-2): a real run row and group rows, so stages can persist.
pub(super) fn project_rc(
    ctx: &Arc<ServiceContext>,
    conn: &rusqlite::Connection,
    project_id: &str,
    cfg: StackingConfig,
    working: &Path,
    output: &Path,
) -> RunContext {
    let source =
        crate::stacking::project_source::project_frame_source(conn, project_id, &cfg).unwrap();
    let subject = StackSubject::Project(project_id.to_string());
    let output = crate::stacking::paths::subject_output_dir(
        output,
        &subject,
        &crate::stacking::plan::subject_label(conn, &subject).unwrap(),
    );
    let run_id = insert_run(
        conn,
        &NewRun {
            subject: &subject,
            config_json: &serde_json::to_string(&cfg).unwrap(),
            config_hash: &config_hash(&cfg),
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.to_str().unwrap(),
            output_dir: output.to_str().unwrap(),
        },
    )
    .unwrap();
    let mut group_ids = HashMap::new();
    for g in &source.groups {
        let id = insert_group(
            conn,
            &NewGroup {
                run_id,
                group_key: &g.key,
                instrume: g.instrume.as_deref(),
                color_mode: color_mode_wire(g.color_mode),
                filter: g.filter.as_deref(),
                binning: Some(g.binning),
                width: None,
                height: None,
                exposure: g.exposure_s,
                exposure_class: Some(&g.exposure_class),
                frame_count: g.frames.len() as i64,
                included_count: g.frames.len() as i64,
            },
        )
        .unwrap();
        group_ids.insert(g.key.clone(), id);
    }
    let layout = subject_layout(conn, working, &subject).unwrap();
    project_test_context(
        ctx.clone(),
        Arc::new(NullEmitter),
        run_id,
        project_id,
        cfg,
        source.groups,
        source.origins,
        layout,
        output.to_path_buf(),
        group_ids,
    )
}

/// `calibrated` rows pointing at each project frame's landed file — what
/// Prepare writes for a float replica used in place (Task 9b) — so
/// Measure can run without Prepare.
pub(super) fn seed_in_place_calibrated(conn: &rusqlite::Connection, rc: &mut RunContext) {
    for g in rc.plan_groups.clone() {
        for f in &g.frames {
            let hash = rc
                .memo
                .calibration_hash_checked(conn, &rc.config, f)
                .unwrap();
            let (size, modified_at) = file_identity(Path::new(&f.path)).unwrap();
            upsert_artifact(
                conn,
                &NewArtifact {
                    subject: &rc.subject,
                    frame_id: Some(f.frame_id),
                    group_key: artifact_group_key("calibrated", &g.key),
                    kind: "calibrated",
                    path: Some(&f.path),
                    config_hash: &hash,
                    size: Some(size),
                    modified_at: Some(&modified_at),
                    payload_json: None,
                },
            )
            .unwrap();
        }
    }
}

/// Rewrite every light of a seeded star group as a three-plane float
/// FITS in the unit domain (an externally debayered light: no BAYERPAT
/// in the catalog, three planes in the file) and refresh the catalog's
/// file identity.
pub(super) fn make_lights_three_plane(
    fixture: &test_fixtures::Fixture,
    shifts: &[(f64, f64)],
    only: &[usize],
) {
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        if !only.contains(&i) {
            continue;
        }
        let path: String = fixture
            .conn
            .query_row(
                "SELECT fi.path FROM files fi JOIN frames f ON f.file_id = fi.id
                     WHERE fi.filename = ?1",
                [format!("f{i}.fits")],
                |r| r.get(0),
            )
            .unwrap();
        let stars: Vec<(f64, f64, f64)> = shifted_stars(dx, dy)
            .into_iter()
            .map(|(x, y, a)| (x, y, a / 12000.0))
            .collect();
        let plane = test_fixtures::synthetic_star_field(
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
            &stars,
            3.0,
            0.002,
            100 + i as u64,
        );
        let rgb: Vec<f32> = [1.0f32, 0.8, 0.6]
            .iter()
            .flat_map(|k| plane.iter().map(move |v| v * k))
            .collect();
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
            Card::new("EXPTIME", CardValue::Real(60.0)).unwrap(),
        ];
        crate::fits_writer::writer::write_fits_f32(
            Path::new(&path),
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
            3,
            &rgb,
            &cards,
        )
        .unwrap();
        let (size, modified_at) = file_identity(Path::new(&path)).unwrap();
        fixture
            .conn
            .execute(
                "UPDATE files SET size = ?1, modified_at = ?2 WHERE path = ?3",
                rusqlite::params![size, modified_at, path],
            )
            .unwrap();
    }
}

pub(super) fn handle_of(rc: &RunContext, file_name: &str) -> i64 {
    rc.plan_groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .find(|f| f.filename == file_name)
        .unwrap()
        .frame_id
}

/// R frames `r1..=r{n_r}` with no WCS and G frames `g1..=g{n_g}` with a
/// manifest WCS, all landed under `landed`.
pub(super) fn seed_r_and_g(
    conn: &rusqlite::Connection,
    landed: &Path,
    project_id: &str,
    n_r: usize,
    n_g: usize,
) {
    for i in 1..=n_r {
        let uuid = format!("r{i}");
        let mut spec = test_fixtures::project_frame_spec(&uuid);
        spec.filter = "R";
        test_fixtures::add_project_frame(conn, landed, project_id, &spec);
    }
    for i in 1..=n_g {
        let uuid = format!("g{i}");
        let mut spec = test_fixtures::project_frame_spec(&uuid);
        spec.filter = "G";
        spec.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        test_fixtures::add_project_frame(conn, landed, project_id, &spec);
    }
}

pub(super) fn blake3_of(conn: &rusqlite::Connection, project_id: &str, uuid: &str) -> String {
    conn.query_row(
        "SELECT blake3 FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
        [project_id, uuid],
        |r| r.get(0),
    )
    .unwrap()
}

/// Every frame of `rc` measured and included, its weight from `weights`
/// by file name (0.5 by default), its calibrated file its own landed path:
/// what stages 4 and 5 read, without running Measure.
pub(super) fn measure_all(rc: &mut RunContext, weights: &HashMap<&str, f64>) {
    for g in rc.plan_groups.clone() {
        let entries: Vec<MeasuredFrame> = g
            .frames
            .iter()
            .map(|f| {
                let w = weights.get(f.filename.as_str()).copied().unwrap_or(0.5);
                MeasuredFrame {
                    frame: f.clone(),
                    calibrated: Some(PathBuf::from(&f.path)),
                    planes: 1,
                    measurement: None,
                    weight: Some(FrameWeight {
                        channels: vec![w],
                        normalized: vec![w],
                        mean: w,
                        normalized_mean: w,
                        missing: None,
                    }),
                    included: true,
                    reason: None,
                    registration: None,
                    cached_calibrated: true,
                    cached_metrics: false,
                    ln_scale: None,
                    cached_ln: false,
                    ln_growth_factor: None,
                    ln_growth_pairs: None,
                    ln_aperture_correction: None,
                    measure_floored: false,
                }
            })
            .collect();
        rc.measured.insert(g.key.clone(), entries);
    }
}

/// The project fixture of the 6b-3 run tests: `n` mono L frames dithered
/// around the fixture target (83.8, −5.4), every one with a manifest
/// WCS, and the grid they land on (1.5″/px, 512 × 384, north up).
pub(super) fn seed_sky_project(
    conn: &rusqlite::Connection,
    landed: &Path,
    n: usize,
) -> crate::collab::grid::CanonicalGrid {
    seed_sky_project_exposures(conn, landed, &vec![300.0; n])
}

/// [`seed_sky_project`] with one frame per entry of `exposures` (seconds).
fn seed_sky_project_exposures(
    conn: &rusqlite::Connection,
    landed: &Path,
    exposures: &[f64],
) -> crate::collab::grid::CanonicalGrid {
    crate::db::stacking::insert_test_project(conn, "p1");
    let target = (83.8, -5.4);
    let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
    let uuids: Vec<String> = (0..exposures.len()).map(|i| format!("l{i}")).collect();
    for (i, uuid) in uuids.iter().enumerate() {
        // Dithers of ±3.6 / ±1.2 px (RA) and 1.2 px (Dec): the LN field's
        // frame-unit lattice (D8, review I6) resolves a dither, and the
        // 0.97 rule fails a member that misses over 3 % of the field —
        // as it fails a frame set's member against the reference.
        let dither = ((i as f64 - 1.5) * 0.001, (i % 2) as f64 * 0.0005);
        let mut f = test_fixtures::sky_frame(uuid, (target.0 + dither.0, target.1 + dither.1));
        f.noise_seed = 100 + i as u64;
        f.exptime_sec = exposures[i];
        test_fixtures::add_project_sky_frame(conn, landed, "p1", &f, &stars);
    }
    crate::collab::grid::CanonicalGrid {
        scale_arcsec: 1.5,
        width_px: 512,
        height_px: 384,
        crval1: target.0,
        crval2: target.1,
        rotation_deg: 0.0,
    }
}

/// The project config of the 6b-3 run tests, stored as the project's
/// document so a plan resolves the same groups and hashes: the seed,
/// with the two-pass re-pick off (its dry pass always detects afresh).
pub(super) fn sky_config(conn: &rusqlite::Connection) -> StackingConfig {
    let mut cfg = crate::stacking::config::project_seed_config(StackingConfig::default());
    cfg.reference.two_pass = false;
    crate::db::stacking::set_project_config(
        conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    cfg
}

/// Background-subtracted centroid of the brightest blob within 3 px of
/// `(x, y)` (7 × 7 window).
pub(super) fn centroid_near(plane: &[f32], w: usize, x: f64, y: f64) -> (f64, f64) {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    let mut vals = Vec::new();
    for dy in -3..=3 {
        for dx in -3..=3 {
            vals.push((
                (cx + dx) as f64,
                (cy + dy) as f64,
                plane[((cy + dy) as usize) * w + (cx + dx) as usize] as f64,
            ));
        }
    }
    let mut sorted: Vec<f64> = vals.iter().map(|v| v.2).collect();
    sorted.sort_by(f64::total_cmp);
    let floor = sorted[sorted.len() / 2];
    let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
    for (px, py, v) in vals {
        let wgt = (v - floor).max(0.0);
        sx += px * wgt;
        sy += py * wgt;
        sw += wgt;
    }
    (sx / sw, sy / sw)
}

/// Runs the whole pipeline on the 4-frame sky project, on its grid, with
/// `tweak` applied to the project's stored config. The files' raw
/// `FILTER` is `L`; the canonical filter is `Lum`.
pub(super) fn run_sky_project(
    tmp: &Path,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    run_sky_project_filter(tmp, "Lum", tweak)
}

pub(super) fn run_sky_project_filter(
    tmp: &Path,
    canonical: &str,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    run_sky_project_exposures(tmp, canonical, &[300.0; 4], tweak)
}

/// [`run_sky_project_filter`] over one frame per entry of `exposures`.
pub(super) fn run_sky_project_exposures(
    tmp: &Path,
    canonical: &str,
    exposures: &[f64],
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    run_sky_project_excluding(tmp, canonical, exposures, &[], tweak)
}

/// [`run_sky_project_exposures`] with the frames at the indices in
/// `excluded` on the project's manual exclusion list: stored as the plan
/// reads them, and on the run as the plan hands them over (`rc.excluded`).
pub(super) fn run_sky_project_excluding(
    tmp: &Path,
    canonical: &str,
    exposures: &[f64],
    excluded: &[usize],
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    let db_path = tmp.join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project_exposures(&conn, &tmp.join("collab"), exposures);
    let excluded_uuids: Vec<String> = excluded.iter().map(|i| format!("l{i}")).collect();
    conn.execute(
        "UPDATE project_frames_local SET filter_canonical = ?1 WHERE project_id = 'p1'",
        [canonical],
    )
    .unwrap();
    let mut cfg = sky_config(&conn);
    tweak(&mut cfg);
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &excluded_uuids,
        None,
    )
    .unwrap();
    let handles: Vec<i64> =
        crate::stacking::project_source::project_frame_source(&conn, "p1", &cfg)
            .unwrap()
            .frames
            .iter()
            .filter(|f| excluded_uuids.contains(&f.frame_uuid))
            .filter_map(|f| f.handle)
            .collect();
    assert_eq!(
        handles.len(),
        excluded_uuids.len(),
        "every excluded frame has a handle"
    );
    let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.join("work"), &tmp.join("out"));
    rc.excluded = handles;
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    run_pipeline(&mut rc).unwrap();
    (ctx, conn, rc, grid)
}

/// Seeds the sky project with its folders and confirmed grid configured,
/// without starting anything.
fn seed_startable_sky_project(
    tmp: &Path,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    crate::collab::grid::CanonicalGrid,
) {
    let db_path = tmp.join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.join("collab"), 4);
    for (key, dir) in [
        (
            crate::settings::keys::STACKING_WORKING_DIR,
            tmp.join("work"),
        ),
        (crate::settings::keys::STACKING_OUTPUT_DIR, tmp.join("out")),
    ] {
        crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
    }
    crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
    let mut cfg = sky_config(&conn);
    tweak(&mut cfg);
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    (ctx, conn, grid)
}

/// Starts the seeded project through the production entry point.
fn start_seeded_project(ctx: &Arc<ServiceContext>) -> Result<StartedStacking, ApiError> {
    start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
    )
}

/// [`seed_startable_sky_project`], started, and waited for.
fn start_sky_project(
    tmp: &Path,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    StartedStacking,
    crate::collab::grid::CanonicalGrid,
) {
    let (ctx, conn, grid) = seed_startable_sky_project(tmp, tweak);
    let started = start_seeded_project(&ctx).expect("a project run starts");
    wait_for_run(&ctx, started.run_id);
    (ctx, conn, started, grid)
}

/// Spec §2, plan 6b-3 D13: `start_stacking` starts a project. The run
/// queues as "Stacking · <title>", stacks onto the confirmed grid,
/// records it in its summary, and writes into `<output>/<title>/`. Its
/// run row names that same folder.
#[test]
fn a_project_run_starts_on_its_confirmed_grid() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_startable_sky_project(tmp.path(), |_| {});

    let hold_flag = Arc::new(AtomicBool::new(false));
    let (hold, _hold_job) = ctx
        .compute_queue
        .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
        .unwrap();
    let started = start_seeded_project(&ctx).expect("a project run starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ctx
        .compute_queue
        .snapshot()
        .iter()
        .any(|e| e.label == "Stacking · p1")
    {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            ctx.compute_queue.snapshot()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(hold);
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    // The run validates its folders in Save mode, which canonicalizes.
    let out = crate::test_support::canonical_path(&tmp.path().join("out")).join("p1");
    assert_eq!(
        Path::new(&row.output_dir),
        out,
        "the run row names the folder the run writes into"
    );
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary.grid, Some(grid));
    assert_eq!(summary.project_id.as_deref(), Some("p1"));
    let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
    let master = master_row(&masters, "master");
    assert!(Path::new(&master.path).starts_with(&out), "{}", master.path);
    assert_eq!((master.width, master.height), (512, 384));
}

/// Plan 6b-3 D13: a project whose grid is not confirmed is refused at the
/// plan's `grid` blocker, before anything is written: no run row and no
/// cancel handle are left.
#[test]
fn a_project_without_a_confirmed_grid_is_refused_and_writes_nothing() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, _grid) = seed_startable_sky_project(tmp.path(), |_| {});
    crate::db::collab::set_canonical_grid(&conn, "p1", None).unwrap();
    let err = start_seeded_project(&ctx).expect_err("an unconfirmed grid refuses the start");
    assert!(
        matches!(&err, ApiError::Invalid(m) if m.contains("grid")),
        "{err:?}"
    );
    let runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM stacking_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 0, "no stacking_runs row is written");
    assert!(ctx.active_stacks.lock().unwrap().is_empty());
}

/// Plan 6b-3 D13 (review): a run started through the production entry
/// point carries the plan's confirmed grid in `RunContext.grid`, so its
/// master is on the grid (`ATH_GRID`, `ATH_SPRJ`, the canonical filter),
/// and the plan it started from agrees with it on Register and
/// Normalize: neither is stale after the run (the run folds the same
/// field-aware LN upstream the plan does).
#[test]
fn a_started_project_run_is_grid_consistent_with_its_plan() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, started, grid) =
        start_sky_project(tmp.path(), |cfg| cfg.normalization.local.enabled = true);
    let row = crate::db::stacking::get_run(&conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
    let master = master_row(&masters, "master");
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
    assert_eq!(
        header.get_str("ATH_GRID").as_deref(),
        Some(grid.hash().as_str())
    );
    assert_eq!(header.get_str("ATH_SPRJ").as_deref(), Some("p1"));
    assert_eq!(header.get_str("ATH_RGEO").as_deref(), Some("grid"));
    let canonical: String = conn
        .query_row(
            "SELECT DISTINCT filter_canonical FROM project_frames_local WHERE project_id = 'p1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        header.get_str("FILTER").as_deref(),
        Some(canonical.as_str())
    );
    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        None,
    )
    .unwrap();
    for stage in [Stage::Register, Stage::Normalize] {
        assert!(
            !plan.stale_stages.contains(&stage),
            "{stage:?} stale after the run: {:?}",
            plan.stale_stages
        );
    }
}

/// Plan 6b-3 Task 9 ruling: the composed registered hash when the
/// reference WCS comes from a LOCAL SOLVE. No frame carries a manifest
/// WCS; a fake solver answers each with its true record. After the run
/// the plan agrees on Register and Normalize, the master states the grid,
/// and the reference is one of the solved frames.
#[test]
fn a_project_run_on_locally_solved_wcs_is_grid_consistent_with_its_plan() {
    use crate::stacking::solve::{FakeSolver, SolveOutcome};
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let target = (83.8, -5.4);
    let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
    // A stand-in catalog, so the plan reads an identity, and the fake
    // solver reports it: the run's cached solves are then the plan's.
    let identity = test_fixtures::install_stand_in_catalog(tmp.path());
    let mut solver = FakeSolver::solving_all().with_identity(&identity);
    for i in 0..3usize {
        let uuid = format!("l{i}");
        let dither = ((i as f64 - 1.5) * 0.001, (i % 2) as f64 * 0.0005);
        let mut f = test_fixtures::sky_frame(&uuid, (target.0 + dither.0, target.1 + dither.1));
        f.noise_seed = 100 + i as u64;
        f.manifest_wcs = false;
        let landed = tmp.path().join("collab");
        let path = test_fixtures::add_project_sky_frame(&conn, &landed, "p1", &f, &stars);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let record = test_fixtures::sky_frame_record(&f);
        solver = solver.with(&name, SolveOutcome::Solved(record));
    }
    let grid = crate::collab::grid::CanonicalGrid {
        scale_arcsec: 1.5,
        width_px: 512,
        height_px: 384,
        crval1: target.0,
        crval2: target.1,
        rotation_deg: 0.0,
    };
    for (key, dir) in [
        (
            crate::settings::keys::STACKING_WORKING_DIR,
            tmp.path().join("work"),
        ),
        (
            crate::settings::keys::STACKING_OUTPUT_DIR,
            tmp.path().join("out"),
        ),
    ] {
        crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
    }
    crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
    let mut cfg = sky_config(&conn);
    cfg.normalization.local.enabled = true;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();

    let solver = Arc::new(solver);
    let started = start_stacking_with_solver(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
        solver.clone(),
    )
    .expect("a project run starts");
    wait_for_run(&ctx, started.run_id);
    let row = crate::db::stacking::get_run(&conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    assert_eq!(solver.calls(), 3, "every frame is solved locally");

    let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
    let master = master_row(&masters, "master");
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
    assert_eq!(
        header.get_str("ATH_GRID").as_deref(),
        Some(grid.hash().as_str())
    );
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let reference = summary.reference.filename.expect("a reference frame");
    let with_manifest_wcs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM project_frames_local \
                 WHERE project_id = 'p1' AND manifest_json LIKE '%\"wcs\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(with_manifest_wcs, 0, "no frame carries a manifest WCS");
    let uuid = reference
        .strip_prefix("c_")
        .and_then(|n| n.strip_suffix(".fits"))
        .unwrap_or_else(|| panic!("unexpected reference name {reference}"));
    let handle = summary.reference.frame_id.expect("a reference frame id");
    let outcome = crate::stacking::solve::stored_outcome(
        &conn,
        "p1",
        handle,
        &blake3_of(&conn, "p1", uuid),
    )
    .unwrap();
    assert!(
        matches!(outcome, Some(SolveOutcome::Solved(_))),
        "the reference has a cached Solved outcome: {outcome:?}"
    );

    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        None,
    )
    .unwrap();
    for stage in [Stage::Register, Stage::Normalize] {
        assert!(
            !plan.stale_stages.contains(&stage),
            "{stage:?} stale after the run: {:?}",
            plan.stale_stages
        );
    }
    assert!(
        plan.warnings
            .iter()
            .all(|w| !w.contains("plate-solved locally")),
        "the run's cached solves are the plan's: {:?}",
        plan.warnings
    );
}

pub(super) fn drizzle_2x(cfg: &mut StackingConfig) {
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
}

pub(super) fn master_row<'a>(
    rows: &'a [crate::db::stacking::MasterLight],
    kind: &str,
) -> &'a crate::db::stacking::MasterLight {
    rows.iter()
        .find(|m| m.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} row in {rows:?}"))
}
