//! The plate-solve plan against a real catalog and real files (MCP plan Task 18). Every test names its files with
//! letters, digits, `_` and `-` only. The star catalog is a seam (`Resolvers`): the tests that plan or run with a catalog
//! hand in empty layers, the test of a missing catalog uses the real resolvers over an app-data folder that has none.
//! A run over files that are not images ends fast with every frame failed; the run holds the process-global auto-publish
//! flag at its end, so a test that runs one holds the lock its own tests hold.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;

use super::*;
use crate::api::plate_solve::{prepare_plate_solve_batch, BUSY_ASSISTANT, BUSY_UI};
use crate::mcp::plans::black_hole::tests::{rig, Fx, Rig};
use crate::mcp::plans::{GroupSelection, PlanAnswer};
use crate::models::{File, FileFormat, Frame, FrameAnalysis, FrameVerdict, ImageType};
use crate::services::PlateSolveOrigin;
use crate::{db, events::NullEmitter};

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// ---- fixtures ----

/// A catalog that is installed (no layers: only the plan and the run's bookkeeping are under test here).
fn installed() -> Resolvers {
    Resolvers { layers: |_| Ok(Vec::new()), packs: |_| Vec::new(), dso: |_| None }
}

/// A catalog file with a real file behind it, in `nas/<dir>`; `set` fills the frame.
fn put(rig: &Rig, dir: &str, name: &str, set: impl FnOnce(&mut Frame)) -> Fx {
    let path = rig.base.join("nas").join(dir).join(name);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
    std::fs::write(&path, b"FITS").expect("write file");
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let file_id = db::insert_file(
        &conn,
        &File {
            id: None,
            path: text(&path),
            filename: name.to_string(),
            size: 4,
            modified_at: Utc::now(),
            format: FileFormat::FITS,
            created_at: Utc::now(),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        },
    )
    .expect("file row");
    let mut frame = Frame { file_id, ..Default::default() };
    set(&mut frame);
    let frame_id = db::insert_frame(&conn, &frame).expect("frame row");
    Fx { file_id, frame_id, path, name: name.to_string() }
}

/// A light with no coordinates and no object name.
fn light_without_object(rig: &Rig, dir: &str, name: &str) -> Fx {
    put(rig, dir, name, |f| {
        f.imagetyp = Some(ImageType::Light);
        f.instrume = Some("Cam".to_string());
    })
}

/// A light with no coordinates.
fn light(rig: &Rig, dir: &str, name: &str) -> Fx {
    put(rig, dir, name, |f| {
        f.imagetyp = Some(ImageType::Light);
        f.object = Some("M31".to_string());
        f.instrume = Some("Cam".to_string());
        f.filter = Some("Ha".to_string());
        f.exptime = Some(180.0);
    })
}

fn located_light(rig: &Rig, dir: &str, name: &str) -> Fx {
    put(rig, dir, name, |f| {
        f.imagetyp = Some(ImageType::Light);
        f.ra = Some(10.68);
        f.dec = Some(41.27);
    })
}

/// A minimal stored plate solve for the frame (the app's own "solved").
fn store_solve(rig: &Rig, fx: &Fx) {
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    conn.execute(
        "INSERT INTO plate_solves (frame_id, crpix1, crpix2, crval1, crval2, cd1_1, cd1_2, cd2_1, cd2_2, matched_stars, \
         total_detected, rms_residual_px, rms_residual_arcsec, pixel_scale_arcsec, field_rotation_deg, solve_time_ms, \
         catalog_used, algorithm_used, solved_at) \
         VALUES (?1, 1, 1, 10, 10, 0.001, 0, 0, 0.001, 50, 60, 0.3, 0.5, 1.0, 0, 10, 'c', 'a', '2026-03-01T23:00:00Z')",
        [fx.frame_id],
    )
    .expect("a stored solve");
}

fn dir_of(rig: &Rig, dir: &str) -> PathBuf {
    rig.base.join("nas").join(dir)
}

fn args(rig: &Rig, dir: &str) -> PlanPlateSolveArgs {
    PlanPlateSolveArgs { scope: Scope { folder: Some(text(&dir_of(rig, dir))), ..Default::default() }, only_unsolved: true }
}

/// A Trailed verdict at the current measurement: the input gate refuses the frame.
fn mark_trailed(rig: &Rig, fx: &Fx) {
    let a = FrameAnalysis {
        id: None,
        frame_id: fx.frame_id,
        file_id: fx.file_id,
        stars_detected: 2000,
        median_fwhm: 4.0,
        median_eccentricity: 0.96,
        median_snr: 30.0,
        median_hfr: 2.0,
        frame_snr: 30.0,
        snr_weight: 1.0,
        psf_signal: 100.0,
        background: 100.0,
        noise: 5.0,
        detection_threshold: 5.0,
        width: 4144,
        height: 2822,
        source_channels: 1,
        trail_r_squared: 0.73,
        possibly_trailed: true,
        median_beta: None,
        quality_score: None,
        config_hash: Some("t".into()),
        analyzed_at: "2026-09-05T00:00:00Z".into(),
        measure_version: Some(crate::star_measure::MEASURE_VERSION),
        stars_refused: None,
        frame_verdict: Some(FrameVerdict::Trailed),
        zero_point: None,
        zp_stars: None,
        zp_sigma: None,
        zp_version: None,
        zp_catalog: None,
        zp_refusal: None,
        zp_list_stars: None,
        zp_window_bright: None,
        zp_window_slope: None,
    };
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    db::analysis::upsert_frame_analysis(&conn, &a).expect("analysis row");
}

fn plan_answer(rig: &Rig, a: PlanPlateSolveArgs) -> (McpPlanView, Answer) {
    let answer = prepare(&rig.host, a, installed()).expect("the plan is prepared");
    let id = answer.data["planId"].as_str().expect("a plan id").to_string();
    let view = rig.host.plans.view(Some(&id)).expect("the plan");
    assert_eq!(view.state, PlanState::Waiting, "{}", answer.text);
    (view, answer)
}

fn plan(rig: &Rig, a: PlanPlateSolveArgs) -> McpPlanView {
    plan_answer(rig, a).0
}

fn proposal(view: &McpPlanView) -> PlanSelection {
    PlanSelection {
        groups: view
            .groups
            .iter()
            .map(|g| GroupSelection {
                group_id: g.id.clone(),
                chosen: None,
                included: g.items.iter().filter(|i| i.selected).map(|i| i.id.clone()).collect(),
            })
            .collect(),
    }
}

/// Presses Apply and waits (bounded) until the apply thread has ended the plan.
fn apply(rig: &Rig, id: &str, selection: PlanSelection) -> McpPlanView {
    rig.host.plans.answer(&rig.host, id, PlanAnswer::Apply { selection }).expect("the apply starts");
    let started = Instant::now();
    loop {
        let v = rig.host.plans.view(Some(id)).expect("plan");
        if v.state != PlanState::Applying {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "the plan is stranded: {:?}", v.state);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits (bounded) until the run's snapshot is kept, and returns it.
fn finished(rig: &Rig, run_id: &str) -> SolveOutcome {
    let started = Instant::now();
    loop {
        if let Some(RunView { outcome: Some(o), .. }) = rig.host.solves.get(run_id) {
            return o;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the run kept no snapshot: {:?}", rig.host.solves.get(run_id).map(|r| r.total));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn item<'a>(view: &'a McpPlanView, fx: &Fx) -> &'a PlanItem {
    let id = format!("item:{}", fx.file_id);
    view.groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("{} is not in the plan", fx.name))
}

fn slot_flag(rig: &Rig) -> Option<bool> {
    rig.ctx
        .active_plate_solves
        .lock()
        .unwrap()
        .get(&0)
        .map(|h| h.cancel_flag.load(std::sync::atomic::Ordering::Relaxed))
}

// ---- tests ----

#[test]
fn eligible_subset_is_n_of_m() {
    let rig = rig();
    let a = light(&rig, "subset", "lights-0001.fits");
    let b = light(&rig, "subset", "lights-0002.fits");
    let trailed = light(&rig, "subset", "lights-0003.fits");
    mark_trailed(&rig, &trailed);

    let (view, answer) = plan_answer(&rig, args(&rig, "subset"));
    assert!(view.notes.iter().any(|n| n == "2 of 3 frames can be solved"), "{:?}", view.notes);
    assert!(answer.text.contains("2 of 3 frames can be solved"), "{}", answer.text);
    let locked = item(&view, &trailed);
    assert!(locked.locked.as_deref().is_some_and(|r| r.contains("trails")), "{:?}", locked.locked);
    assert!(!locked.selected);
    for ok in [&a, &b] {
        let i = item(&view, ok);
        assert!(i.selected && i.locked.is_none(), "{} is proposed", ok.name);
        assert_eq!(i.file_name, ok.name);
        assert_eq!(i.folder, text(&dir_of(&rig, "subset")));
    }
    assert!(view.notes.iter().any(|n| n == "star catalog: installed"), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n == "quad index packs: 0 installed"), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n.starts_with("runs ") && n.ends_with(" frames at a time")), "{:?}", view.notes);
    assert_eq!(answer.data["solvable"], 2);
    assert_eq!(answer.data["inScope"], 3);
}

#[test]
fn non_lights_and_located_lights_are_counted_not_listed() {
    let rig = rig();
    let todo = light(&rig, "mixed", "lights-0001.fits");
    let located = located_light(&rig, "mixed", "lights-0002.fits");
    store_solve(&rig, &located);
    let flat = put(&rig, "mixed", "flat-0001.fits", |f| f.imagetyp = Some(ImageType::Flat));
    let view = plan(&rig, args(&rig, "mixed"));
    let ids: Vec<&str> = view.groups.iter().flat_map(|g| g.items.iter()).map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec![format!("item:{}", todo.file_id)], "only the light with no stored solve is listed");
    assert!(view.notes.iter().any(|n| n == "1 of 1 frames can be solved"), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n.contains("1 frame is not a light frame and is left out")), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n.contains("1 light frame already has a plate solve and is left out")), "{:?}", view.notes);
    let _ = (located, flat);

    // onlyUnsolved false lists the located light too.
    let mut all = args(&rig, "mixed");
    all.only_unsolved = false;
    let view = plan(&rig, all);
    let count: usize = view.groups.iter().map(|g| g.items.len()).sum();
    assert_eq!(count, 2);
}

#[test]
fn planning_refused_while_a_ui_batch_runs() {
    let rig = rig();
    light(&rig, "busy", "lights-0001.fits");
    let waiting = plan(&rig, args(&rig, "busy"));

    // A batch started in Athenaeum holds the slot.
    let ui = prepare_plate_solve_batch(&rig.ctx, &[9001, 9002], Vec::new(), Vec::new(), None, PlateSolveOrigin::Ui)
        .expect("the UI batch");
    let err = prepare(&rig.host, args(&rig, "busy"), installed()).err().expect("no plan while a batch runs");
    let said = err.to_string();
    assert_eq!(said, BUSY_UI.replace("{done}", "0").replace("{total}", "2"), "{said}");
    // The plan the user is looking at is untouched, and no plan began.
    assert_eq!(rig.host.plans.view(Some(&waiting.plan_id)).expect("plan").state, PlanState::Waiting);
    assert_eq!(rig.host.plans.view(None).expect("newest").plan_id, waiting.plan_id);
    drop(ui);

    // A run of the assistant's own says so in its own words.
    let mine = prepare_plate_solve_batch(&rig.ctx, &[9001], Vec::new(), Vec::new(), None, PlateSolveOrigin::Assistant)
        .expect("the assistant's batch");
    let said = prepare(&rig.host, args(&rig, "busy"), installed()).err().expect("refused").to_string();
    assert_eq!(said, BUSY_ASSISTANT.replace("{done}", "0").replace("{total}", "1"), "{said}");
    assert_eq!(rig.host.plans.view(Some(&waiting.plan_id)).expect("plan").state, PlanState::Waiting);
    drop(mine);
}

#[test]
fn missing_catalog_locks_every_item() {
    let rig = rig();
    let a = light(&rig, "nocat", "lights-0001.fits");
    let b = light(&rig, "nocat", "lights-0002.fits");
    // The real resolvers over a catalog folder that holds no star catalog.
    let answer = prepare(&rig.host, args(&rig, "nocat"), Resolvers::real()).expect("a plan with every item locked");
    let id = answer.data["planId"].as_str().expect("a plan id").to_string();
    let view = rig.host.plans.view(Some(&id)).expect("plan");
    for fx in [&a, &b] {
        let i = item(&view, fx);
        assert_eq!(i.locked.as_deref(), Some("the star catalog is not installed (Settings \u{2192} Plate Solving)"));
        assert!(!i.selected);
    }
    assert!(view.notes.iter().any(|n| n == "0 of 2 frames can be solved"), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n == "star catalog: not installed"), "{:?}", view.notes);
}

#[test]
fn a_request_with_nothing_to_solve_is_refused_and_keeps_the_waiting_plan() {
    let rig = rig();
    light(&rig, "keep", "lights-0001.fits");
    let solved = located_light(&rig, "keep2", "lights-0001.fits");
    store_solve(&rig, &solved);
    put(&rig, "darks", "dark-0001.fits", |f| f.imagetyp = Some(ImageType::Dark));
    let waiting = plan(&rig, args(&rig, "keep"));

    let located = prepare(&rig.host, args(&rig, "keep2"), installed()).err().expect("refused").to_string();
    assert!(located.contains("already has a plate solve") && located.contains("nothing was planned"), "{located}");
    let darks = prepare(&rig.host, args(&rig, "darks"), installed()).err().expect("refused").to_string();
    assert!(darks.contains("no light frame") && darks.contains("nothing was planned"), "{darks}");
    let none = prepare(
        &rig.host,
        PlanPlateSolveArgs { scope: Scope { object: Some("NoSuchObject".into()), ..Default::default() }, only_unsolved: true },
        installed(),
    )
    .err()
    .expect("refused")
    .to_string();
    assert!(none.contains("nothing was planned"), "{none}");
    assert_eq!(rig.host.plans.view(Some(&waiting.plan_id)).expect("plan").state, PlanState::Waiting);
    assert_eq!(rig.host.plans.view(None).expect("newest").plan_id, waiting.plan_id);
}

#[test]
fn apply_starts_a_run_and_says_started_not_solved() {
    let _lock = crate::api::collab_autopublish::test_lock();
    let rig = rig();
    let a = light(&rig, "start", "lights-0001.fits");
    let b = light(&rig, "start", "lights-0002.fits");
    let view = plan(&rig, args(&rig, "start"));
    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.clone().expect("a report");
    let run_id = report.run_id.clone().expect("the run's id");
    assert!(run_id.starts_with("solve-"), "{run_id}");
    assert_eq!((report.applied.len(), report.failed.len()), (2, 0));

    let outcome = finished(&rig, &run_id);
    crate::api::collab_autopublish::reset_dirty_state_for_test();
    assert_eq!(outcome.solved + outcome.failed, 2);

    let said = crate::mcp::tools::plans::status_answer(&done);
    assert!(
        said.text.contains(&format!("started a plate solve of 2 frames (run {run_id}); ask solve_status for its progress")),
        "{}",
        said.text
    );
    assert!(!said.text.contains("applied"), "a started run is not 'applied': {}", said.text);
    assert_eq!(said.data["report"]["runId"], run_id.as_str());
    let _ = (a, b);
}

#[test]
fn apply_while_a_batch_runs_keeps_the_plan_waiting() {
    let _lock = crate::api::collab_autopublish::test_lock();
    let rig = rig();
    light(&rig, "late", "lights-0001.fits");
    let view = plan(&rig, args(&rig, "late"));
    // The user starts a batch in the window while the plan waits, then presses Apply on the plan.
    let ui = prepare_plate_solve_batch(&rig.ctx, &[9001], Vec::new(), Vec::new(), None, PlateSolveOrigin::Ui)
        .expect("the UI batch");
    let busy = BUSY_UI.replace("{done}", "0").replace("{total}", "1");
    let refused = rig
        .host
        .plans
        .answer(&rig.host, &view.plan_id, PlanAnswer::Apply { selection: proposal(&view) })
        .err()
        .expect("an Apply the slot cannot take is refused");
    match refused {
        ApiError::Conflict(said) => assert_eq!(said, busy),
        other => panic!("expected Conflict, got {other:?}"),
    }
    // The plan did not burn: it still waits, with its selection, and starts once the slot is free.
    let waiting = rig.host.plans.view(Some(&view.plan_id)).expect("plan");
    assert_eq!(waiting.state, PlanState::Waiting);
    assert!(waiting.report.is_none(), "{:?}", waiting.report);
    assert_eq!(proposal(&waiting), proposal(&view));
    drop(ui);
    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.state, PlanState::Applied, "{:?}", done.state);
    let run_id = done.report.expect("a report").run_id.expect("the run started");
    finished(&rig, &run_id);
    crate::api::collab_autopublish::reset_dirty_state_for_test();
}

#[test]
fn a_batch_that_takes_the_slot_after_ready_ends_the_plan_failed() {
    let rig = rig();
    let a = light(&rig, "race", "lights-0001.fits");
    let item_id = format!("item:{}", a.file_id);
    let applier = PlateSolveApplier {
        items: HashMap::from([(
            item_id.clone(),
            ItemInfo { frame_id: a.frame_id, file_name: a.name.clone(), folder: text(&dir_of(&rig, "race")) },
        )]),
        resolve: installed(),
    };
    assert!(applier.ready(&rig.host).is_ok(), "an idle slot is ready");
    // A batch takes the slot between `ready` and the start: the start's own refusal stays the safety net.
    let ui = prepare_plate_solve_batch(&rig.ctx, &[9001], Vec::new(), Vec::new(), None, PlateSolveOrigin::Ui).expect("ui");
    let busy = BUSY_UI.replace("{done}", "0").replace("{total}", "1");
    assert_eq!(applier.ready(&rig.host).err().as_deref(), Some(busy.as_str()));
    let selection = PlanSelection {
        groups: vec![GroupSelection { group_id: "group:1".to_string(), chosen: None, included: vec![item_id.clone()] }],
    };
    let report = applier.apply(&rig.host, "plan-race", &selection, std::slice::from_ref(&item_id));
    assert!(report.run_id.is_none() && report.applied.is_empty(), "{report:?}");
    assert_eq!(report.failed.len(), 1);
    assert_eq!(applier.start_failure(&report).as_deref(), Some(busy.as_str()));
    drop(ui);
}

#[test]
fn apply_fails_the_plan_when_the_catalog_went_missing_after_the_plan() {
    use std::sync::atomic::{AtomicBool, Ordering};
    // The catalog is installed when the plan is made and removed before Apply resolves it again.
    static REMOVED: AtomicBool = AtomicBool::new(false);
    let rig = rig();
    let a = light(&rig, "gone", "lights-0001.fits");
    let resolve = Resolvers {
        layers: |_| if REMOVED.load(Ordering::SeqCst) { Err(ApiError::Invalid("no catalog".into())) } else { Ok(Vec::new()) },
        packs: |_| Vec::new(),
        dso: |_| None,
    };
    let answer = prepare(&rig.host, args(&rig, "gone"), resolve).expect("the plan is prepared");
    let id = answer.data["planId"].as_str().expect("a plan id").to_string();
    let view = rig.host.plans.view(Some(&id)).expect("the plan");
    assert_eq!(view.state, PlanState::Waiting, "{}", answer.text);
    REMOVED.store(true, Ordering::SeqCst);
    let done = apply(&rig, &id, proposal(&view));
    REMOVED.store(false, Ordering::SeqCst);
    assert_eq!(done.state, PlanState::Failed { error: NO_CATALOG.to_string() }, "{:?}", done.state);
    let report = done.report.clone().expect("the report is kept");
    assert!(report.run_id.is_none() && report.applied.is_empty(), "{report:?}");
    assert_eq!((report.failed.len(), report.failed[0].file_name.as_str()), (1, a.name.as_str()));
    let said = crate::mcp::tools::plans::status_answer(&done).text;
    assert!(said.contains(&format!("Nothing was started: {}", NO_CATALOG.trim_end_matches('.'))), "{said}");
    assert!(!said.contains("Some files may have changed") && !said.contains("applied"), "{said}");
}

#[test]
fn status_reports_progress_and_result() {
    let _lock = crate::api::collab_autopublish::test_lock();
    let rig = rig();
    // While it runs: the handle's progress, the failures so far by file name and folder.
    let held = prepare_plate_solve_batch(&rig.ctx, &[9001, 9002, 9003], Vec::new(), Vec::new(), None, PlateSolveOrigin::Assistant)
        .expect("the batch");
    let run_id = held.run_id().to_string();
    rig.host.solves.start(&run_id, "plan-x", 3, std::collections::HashMap::from([(7, ("lights-0002.fits".to_string(), "somewhere".to_string()))]));
    rig.host.solves.note_failed(&run_id, 7, "lights-0002.fits", "no stars found");
    {
        let solves = rig.ctx.active_plate_solves.lock().unwrap();
        let mut p = solves.get(&0).unwrap().progress.lock().unwrap();
        p.done = 2;
        p.current_file = Some("lights-0003.fits".into());
    }
    let running = solve_status(&rig.host, &run_id).expect("status");
    assert!(running.text.contains("2 of 3 frames"), "{}", running.text);
    // The store can still reject a frame or roll the batch back: no "solved so far" until the run ends.
    assert!(!running.text.contains("solved"), "{}", running.text);
    assert!(running.data.get("solved").is_none(), "{}", running.data);
    assert!(running.text.contains("lights-0003.fits"), "{}", running.text);
    assert!(running.text.contains("lights-0002.fits in somewhere: no stars found"), "{}", running.text);
    assert_eq!(running.data["state"], "running");
    assert_eq!(running.data["done"], 2);
    assert_eq!(running.data["total"], 3);
    drop(held);

    // After it: the run's own result, with each failed file and its error.
    let a = light(&rig, "result", "lights-0001.fits");
    let b = light(&rig, "result", "lights-0002.fits");
    let view = plan(&rig, args(&rig, "result"));
    let done = apply(&rig, &view.plan_id, proposal(&view));
    let run_id = done.report.expect("report").run_id.expect("run id");
    let outcome = finished(&rig, &run_id);
    crate::api::collab_autopublish::reset_dirty_state_for_test();
    // The files are not images, so each frame fails fast and the status names it.
    assert_eq!((outcome.solved, outcome.failed), (0, 2), "{outcome:?}");
    let status = solve_status(&rig.host, &run_id).expect("status");
    assert_eq!(status.data["state"], "finished");
    assert_eq!(status.data["solved"], 0);
    assert_eq!(status.data["failed"], 2);
    for fx in [&a, &b] {
        assert!(status.text.contains(&format!("{} in {}", fx.name, text(&dir_of(&rig, "result")))), "{}", status.text);
    }
    let files = status.data["failures"].as_array().expect("failures");
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f["fileName"].is_string() && f["folder"].is_string() && f["error"].is_string()), "{files:?}");
    // A run nobody knows is said in words.
    let unknown = solve_status(&rig.host, "solve-nothing").expect("an answer");
    assert_eq!(unknown.data["state"], "unknown");
}

#[test]
fn the_final_snapshot_counts_from_the_runs_own_result() {
    let rig = rig();
    // A complete event that says 5 solved, then a run that ends with an error (a failed COMMIT rolled them back):
    // nothing was solved, whatever the progress events said.
    rig.host.solves.start("solve-a", "plan-a", 5, Default::default());
    rig.host.solves.note_complete("solve-a", 5, 0, 12);
    rig.host.solves.finish("solve-a", Err(ApiError::Internal("the catalog write failed".to_string())));
    let o = rig.host.solves.get("solve-a").and_then(|r| r.outcome).expect("a snapshot");
    assert_eq!((o.solved, o.failed, o.not_attempted), (0, 5, 0));
    assert_eq!(o.error.as_deref(), Some("the catalog write failed"));

    // A clean run: the counts are the complete event's, and a failure the events did not name is counted apart.
    rig.host.solves.start("solve-b", "plan-b", 4, std::collections::HashMap::from([(1, ("a.fits".to_string(), "f".to_string()))]));
    rig.host.solves.note_failed("solve-b", 1, "a.fits", "no match");
    rig.host.solves.note_complete("solve-b", 2, 2, 30);
    rig.host.solves.finish("solve-b", Ok(()));
    let o = rig.host.solves.get("solve-b").and_then(|r| r.outcome).expect("a snapshot");
    assert_eq!((o.solved, o.failed, o.not_attempted, o.unlisted), (2, 2, 0, 1));
    assert_eq!(o.failures.len(), 1);

    // A cancelled run left frames unattempted.
    rig.host.solves.start("solve-c", "plan-c", 6, Default::default());
    rig.host.solves.note_complete("solve-c", 1, 1, 9);
    rig.host.solves.finish("solve-c", Ok(()));
    let o = rig.host.solves.get("solve-c").and_then(|r| r.outcome).expect("a snapshot");
    assert_eq!((o.solved, o.failed, o.not_attempted), (1, 1, 4));
}

#[test]
fn cancel_solve_stops_only_its_run() {
    let rig = rig();
    // A batch started in Athenaeum holds the slot: the assistant cannot cancel it, whatever id it names.
    let ui = prepare_plate_solve_batch(&rig.ctx, &[9001], Vec::new(), Vec::new(), None, PlateSolveOrigin::Ui).expect("ui");
    let ui_id = ui.run_id().to_string();
    let said = cancel_solve(&rig.host, &ui_id).expect("an answer");
    assert_eq!(said.data["cancelled"], false, "{}", said.text);
    assert_eq!(slot_flag(&rig), Some(false), "the window's run is untouched");
    let said = cancel_solve(&rig.host, "solve-other").expect("an answer");
    assert_eq!(said.data["cancelled"], false);
    assert_eq!(slot_flag(&rig), Some(false));
    drop(ui);

    // The assistant's own run is cancelled; a different id leaves it alone.
    let mine = prepare_plate_solve_batch(&rig.ctx, &[9001], Vec::new(), Vec::new(), None, PlateSolveOrigin::Assistant).expect("mine");
    let my_id = mine.run_id().to_string();
    rig.host.solves.start(&my_id, "plan-m", 1, Default::default());
    let said = cancel_solve(&rig.host, "solve-some-older-run").expect("an answer");
    assert_eq!(said.data["cancelled"], false);
    assert_eq!(slot_flag(&rig), Some(false));
    let said = cancel_solve(&rig.host, &my_id).expect("an answer");
    assert_eq!(said.data["cancelled"], true, "{}", said.text);
    assert_eq!(slot_flag(&rig), Some(true));
    assert!(said.text.contains(&my_id) && said.text.contains("solve_status"), "{}", said.text);
    drop(mine);

    // After the run's end the id answers in words and nothing is cancelled.
    rig.host.solves.finish(&my_id, Ok(()));
    let said = cancel_solve(&rig.host, &my_id).expect("an answer");
    assert_eq!(said.data["cancelled"], false);
    assert!(said.text.contains("already ended"), "{}", said.text);
}

#[test]
fn default_plans_lights_with_header_coordinates_but_no_solve() {
    let rig = rig();
    let mount = located_light(&rig, "mount", "lights-0001.fits");
    let bare = light(&rig, "mount", "lights-0002.fits");
    let (view, answer) = plan_answer(&rig, args(&rig, "mount"));
    for fx in [&mount, &bare] {
        let i = item(&view, fx);
        assert!(i.selected && i.locked.is_none(), "{} is proposed", fx.name);
        assert_eq!(i.evidence.as_deref(), Some("no plate solve is stored for this frame"));
    }
    // The header's position is shown, and is the solver's hint.
    assert_eq!(item(&view, &mount).values["coordinates"], "RA 10.6800, Dec 41.2700");
    assert_eq!(item(&view, &bare).values["coordinates"], "none");
    assert_eq!(answer.data["solvable"], 2);
}

#[test]
fn default_skips_lights_with_a_stored_solve() {
    let rig = rig();
    let todo = located_light(&rig, "solved", "lights-0001.fits");
    let done = located_light(&rig, "solved", "lights-0002.fits");
    store_solve(&rig, &done);
    let view = plan(&rig, args(&rig, "solved"));
    let ids: Vec<&str> = view.groups.iter().flat_map(|g| g.items.iter()).map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec![format!("item:{}", todo.file_id)]);
    assert!(view.notes.iter().any(|n| n.contains("1 light frame already has a plate solve and is left out")), "{:?}", view.notes);
}

#[test]
fn all_solved_refusal_names_plate_solve() {
    let rig = rig();
    for n in 0..3 {
        let fx = light(&rig, "all-solved", &format!("lights-000{n}.fits"));
        store_solve(&rig, &fx);
    }
    let said = prepare(&rig.host, args(&rig, "all-solved"), installed()).err().expect("refused").to_string();
    assert!(
        said.contains("every light frame in the scope (")
            && said.contains(") already has a plate solve (3); pass onlyUnsolved false to solve them again; nothing was planned"),
        "{said}"
    );
    assert!(!said.contains("coordinates"), "{said}");
}

#[test]
fn only_unsolved_false_plans_solved_lights() {
    let rig = rig();
    let todo = light(&rig, "again", "lights-0001.fits");
    let done = light(&rig, "again", "lights-0002.fits");
    store_solve(&rig, &done);
    let mut all = args(&rig, "again");
    all.only_unsolved = false;
    let view = plan(&rig, all);
    for fx in [&todo, &done] {
        let i = item(&view, fx);
        assert!(i.selected && i.locked.is_none(), "{} is proposed", fx.name);
        assert!(i.evidence.is_none(), "{:?}", i.evidence);
    }
    assert!(!view.notes.iter().any(|n| n.contains("plate solve and")), "{:?}", view.notes);
}

#[test]
fn apply_skips_frame_solved_since_plan() {
    let _lock = crate::api::collab_autopublish::test_lock();
    let rig = rig();
    let a = light(&rig, "since", "lights-0001.fits");
    let b = light(&rig, "since", "lights-0002.fits");
    let view = plan(&rig, args(&rig, "since"));
    // A solve lands (the user solved one frame in Athenaeum) while the plan waits.
    store_solve(&rig, &b);
    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.clone().expect("a report");
    assert_eq!((report.applied.len(), report.skipped.len(), report.failed.len()), (1, 1, 0), "{report:?}");
    assert_eq!(report.applied[0].file_name, a.name);
    assert_eq!(report.skipped[0].file_name, b.name);
    assert_eq!(report.skipped[0].reason, "solved since the plan was made");
    let run_id = report.run_id.clone().expect("the run's id");
    let outcome = finished(&rig, &run_id);
    crate::api::collab_autopublish::reset_dirty_state_for_test();
    assert_eq!(outcome.solved + outcome.failed, 1, "only the unsolved frame went to the run");
    let said = crate::mcp::tools::plans::status_answer(&done);
    assert!(said.text.contains("started a plate solve of 1 frame (run"), "{}", said.text);
    assert!(said.text.contains("solved since the plan was made"), "{}", said.text);
}

/// `PLAN_ITEM_CAP + 1` lights in one transaction, each with `set` applied to its frame.
fn bulk_lights(rig: &Rig, dir: &str, set: impl Fn(&mut Frame)) {
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let tx = conn.unchecked_transaction().expect("transaction");
    for n in 0..=PLAN_ITEM_CAP {
        let name = format!("lights-{n:05}.fits");
        let path = rig.base.join("nas").join(dir).join(&name);
        let file_id = db::insert_file(
            &tx,
            &File {
                id: None,
                path: text(&path),
                filename: name,
                size: 4,
                modified_at: Utc::now(),
                format: FileFormat::FITS,
                created_at: Utc::now(),
                content_hash: None,
                archived_in_operation: None,
                archive_zip_path: None,
                archive_path_in_zip: None,
                uuid: None,
                updated_at: None,
            },
        )
        .expect("file row");
        let mut frame = Frame { file_id, imagetyp: Some(ImageType::Light), ..Default::default() };
        set(&mut frame);
        db::insert_frame(&tx, &frame).expect("frame row");
    }
    tx.commit().expect("commit");
}

#[test]
fn an_unscoped_request_over_the_cap_reads_no_frame() {
    let rig = rig();
    bulk_lights(&rig, "many", |_| {});
    super::super::reset_catalog_value_reads();
    let said = prepare(&rig.host, PlanPlateSolveArgs { scope: Scope::default(), only_unsolved: true }, installed())
        .err()
        .expect("refused over the cap")
        .to_string();
    assert!(said.contains("at most 2 000"), "{said}");
    assert_eq!(super::super::catalog_value_reads(), 0, "the cap is counted before any frame is read");
    assert!(rig.host.plans.view(None).is_none(), "no plan began");
}

#[test]
fn unscoped_default_over_cap_refused() {
    // Mount coordinates in every header and no stored solve: all of them are unsolved, so the cap sentence answers
    // (the old test of "coordinates missing" would have planned nothing here).
    let rig = rig();
    bulk_lights(&rig, "many-located", |f| {
        f.ra = Some(10.68);
        f.dec = Some(41.27);
    });
    let said = prepare(&rig.host, PlanPlateSolveArgs { scope: Scope::default(), only_unsolved: true }, installed())
        .err()
        .expect("refused over the cap")
        .to_string();
    assert!(said.contains("2001 items") && said.contains("at most 2 000") && said.contains("narrow the scope"), "{said}");
    assert!(rig.host.plans.view(None).is_none(), "no plan began");
}

#[test]
fn a_cancel_from_the_app_is_named_to_the_assistant() {
    let rig = rig();
    // A run the window cancelled: its handle's flag is set, and the assistant never asked for it.
    rig.host.solves.start("solve-app", "plan-app", 5, Default::default());
    rig.host.solves.note_complete("solve-app", 1, 1, 9);
    let flag = std::sync::atomic::AtomicBool::new(true);
    end_run(&rig.host.solves, "solve-app", &flag, Ok(()));
    let said = solve_status(&rig.host, "solve-app").expect("status");
    assert!(said.text.contains("cancelled in Athenaeum; 3 frames were not attempted"), "{}", said.text);
    assert_eq!(said.data["cancelledInApp"], true);
    assert_eq!(said.data["notAttempted"], 3);

    // The assistant's own cancel is not "in Athenaeum".
    rig.host.solves.start("solve-mine", "plan-mine", 5, Default::default());
    rig.host.solves.note_cancel("solve-mine");
    rig.host.solves.note_complete("solve-mine", 1, 1, 9);
    end_run(&rig.host.solves, "solve-mine", &flag, Ok(()));
    let said = solve_status(&rig.host, "solve-mine").expect("status");
    assert!(!said.text.contains("in Athenaeum"), "{}", said.text);
    assert_eq!(said.data["cancelledInApp"], false);

    // A run nobody cancelled says nothing of it.
    rig.host.solves.start("solve-calm", "plan-calm", 2, Default::default());
    rig.host.solves.note_complete("solve-calm", 2, 0, 9);
    end_run(&rig.host.solves, "solve-calm", &std::sync::atomic::AtomicBool::new(false), Ok(()));
    let said = solve_status(&rig.host, "solve-calm").expect("status");
    assert!(!said.text.contains("cancelled") && said.data["cancelledInApp"] == false, "{}", said.text);
}

#[test]
fn the_plan_is_data_with_caps() {
    let rig = rig();
    // 51 folders of one light each: 50 groups are listed, the rest summed.
    for n in 0..51 {
        light(&rig, &format!("cap-{n:02}"), "lights-0001.fits");
    }
    let view = plan(&rig, PlanPlateSolveArgs { scope: Scope::default(), only_unsolved: true });
    assert_eq!(view.groups.len(), 51);
    let described = describe_plan(&view);
    assert_eq!(described.data["groups"].as_array().expect("groups").len(), 50);
    assert_eq!(described.data["otherGroups"]["groups"], 1);
    assert_eq!(described.data["otherGroups"]["frames"], 1);
    let first = &described.data["groups"][0]["items"][0];
    assert!(first["fileName"].is_string() && first["folder"].is_string() && first["selected"] == true && first["locked"].is_null(), "{first}");
    assert!(described.summary.contains("51 of 51 frames can be solved"), "{}", described.summary);
    let _ = NullEmitter;
}

const NAMES_THE_OBJECT: &str = "A solved frame gets its coordinates; a frame with no object name also gets the name of the catalogued object the solve finds in its field, as a solve started in Athenaeum does.";

#[test]
fn the_solve_plan_says_it_may_name_the_object() {
    let rig = rig();
    light(&rig, "names", "n1.fits");
    let (view, answer) = plan_answer(&rig, args(&rig, "names"));
    // Both halves of the planning answer, and of what plan_status says of the waiting plan.
    assert!(answer.text.contains(NAMES_THE_OBJECT), "{}", answer.text);
    let in_data = |data: &Value| data["notes"].as_array().is_some_and(|n| n.iter().any(|x| x == NAMES_THE_OBJECT));
    assert!(in_data(&answer.data), "{}", answer.data);
    let status = crate::mcp::tools::plans::status_answer(&view);
    assert!(status.text.contains(NAMES_THE_OBJECT), "{}", status.text);
    assert!(in_data(&status.data), "{}", status.data);
    // The tool's own description says it too.
    let tools = crate::mcp::AthenaeumMcp::all_tools().list_all();
    let tool = tools.iter().find(|t| t.name == "plan_plate_solve").expect("the plan_plate_solve tool");
    let described = tool.description.as_deref().unwrap_or_default();
    assert!(described.contains(NAMES_THE_OBJECT), "{described}");
}

#[test]
fn the_final_status_counts_the_frames_that_got_an_object_name() {
    let rig = rig();
    rig.host.solves.start("solve-n", "plan-n", 4, Default::default());
    rig.host.solves.note_complete("solve-n", 4, 0, 10);
    rig.host.solves.note_named("solve-n", 3);
    rig.host.solves.finish("solve-n", Ok(()));
    let status = solve_status(&rig.host, "solve-n").expect("an answer");
    assert!(status.text.contains("3 frames also got an object name"), "{}", status.text);
    assert_eq!(status.data["objectNamed"], 3);
    // A run whose count was not known says nothing of it, and a run that kept no result named nothing.
    rig.host.solves.start("solve-m", "plan-m", 2, Default::default());
    rig.host.solves.note_complete("solve-m", 2, 0, 10);
    rig.host.solves.finish("solve-m", Ok(()));
    let status = solve_status(&rig.host, "solve-m").expect("an answer");
    assert!(!status.text.contains("object name"), "{}", status.text);
    assert!(status.data["objectNamed"].is_null(), "{}", status.data);
}

#[test]
fn the_object_count_reads_the_catalog_by_frame() {
    let rig = rig();
    let bare = light_without_object(&rig, "count", "c1.fits");
    let also_bare = light_without_object(&rig, "count", "c2.fits");
    let named = light(&rig, "count", "c3.fits");
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let ids = [bare.frame_id, also_bare.frame_id, named.frame_id];
    let before = unnamed_among(&conn, &ids).expect("unnamed frames");
    assert_eq!(before, vec![bare.frame_id, also_bare.frame_id]);
    assert!(crate::plate_solve::storage::update_frame_object_if_missing(&conn, bare.frame_id, "RCW 145").expect("update"));
    assert_eq!(named_among(&conn, &before).expect("count"), 1);
}
