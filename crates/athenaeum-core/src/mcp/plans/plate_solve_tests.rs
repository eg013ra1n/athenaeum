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
    let flat = put(&rig, "mixed", "flat-0001.fits", |f| f.imagetyp = Some(ImageType::Flat));
    let view = plan(&rig, args(&rig, "mixed"));
    let ids: Vec<&str> = view.groups.iter().flat_map(|g| g.items.iter()).map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec![format!("item:{}", todo.file_id)], "only the light that misses coordinates is listed");
    assert!(view.notes.iter().any(|n| n == "1 of 1 frames can be solved"), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n.contains("1 frame is not a light frame and is left out")), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n.contains("1 light frame already has coordinates and is left out")), "{:?}", view.notes);
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
    located_light(&rig, "keep2", "lights-0001.fits");
    put(&rig, "darks", "dark-0001.fits", |f| f.imagetyp = Some(ImageType::Dark));
    let waiting = plan(&rig, args(&rig, "keep"));

    let located = prepare(&rig.host, args(&rig, "keep2"), installed()).err().expect("refused").to_string();
    assert!(located.contains("already has coordinates") && located.contains("nothing was planned"), "{located}");
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
fn apply_refused_when_a_ui_batch_started_meanwhile() {
    let rig = rig();
    light(&rig, "late", "lights-0001.fits");
    let view = plan(&rig, args(&rig, "late"));
    // The user starts a batch in the window while the plan waits.
    let ui = prepare_plate_solve_batch(&rig.ctx, &[9001], Vec::new(), Vec::new(), None, PlateSolveOrigin::Ui)
        .expect("the UI batch");
    let done = apply(&rig, &view.plan_id, proposal(&view));
    let report = done.report.clone().expect("a report");
    assert!(report.run_id.is_none(), "nothing started");
    assert!(report.applied.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].reason, BUSY_UI.replace("{done}", "0").replace("{total}", "1"));
    let said = crate::mcp::tools::plans::status_answer(&done).text;
    assert!(said.contains("did not start a plate solve") && !said.contains("started a plate solve of"), "{said}");
    assert!(rig.host.solves.get("solve-none").is_none());
    drop(ui);
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
        p.solved = 1;
        p.current_file = Some("lights-0003.fits".into());
    }
    let running = solve_status(&rig.host, &run_id).expect("status");
    assert!(running.text.contains("2 of 3 frames"), "{}", running.text);
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
