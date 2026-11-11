//! Tests of the solve sub-step; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{self, seed_star_group};

fn use_solver(rc: &mut RunContext, fake: &Arc<crate::stacking::solve::FakeSolver>) {
    let solver: Arc<dyn crate::stacking::solve::FrameSolver> = fake.clone();
    rc.solver = solver;
}

/// Spec §5.2, §15.1 (Review Focus 2), plan 6b-2 C14: the Solve sub-step
/// solves each project frame without a manifest WCS once, through the
/// run's solver and on the file stage 1 handed on, and caches the
/// outcome as a `solve` artifact. It never writes `plate_solves` or
/// `frames`, not even a `frames` row whose id equals a handle. A second
/// pass solves nothing, and `stored_solve` then serves the local WCS.
#[test]
fn the_solve_sub_step_never_writes_plate_solves_or_frames() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let handles: Vec<i64> = rc.origins.keys().copied().collect();
    for (i, h) in handles.iter().enumerate() {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'decoy.fits', 7, '2020-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![h, format!("/decoy/{i}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, filter, exptime, naxis1, naxis2)
                 VALUES (?1, ?1, 'Light', 'Z', 999.0, 7, 7)",
            [h],
        )
        .unwrap();
        test_fixtures::seed_plate_solve_scale(&conn, *h, 9.9);
    }
    // `frame_id` is UNIQUE and `insert_plate_solve` is INSERT OR REPLACE,
    // so a handle-keyed write over a decoy keeps the count: the snapshot
    // also carries every plate_solves row's content.
    let snapshot = |c: &rusqlite::Connection| -> (i64, i64, String, String) {
        c.query_row(
            "SELECT (SELECT COUNT(*) FROM plate_solves), (SELECT COUNT(*) FROM frames),
                        (SELECT group_concat(filter || ':' || exptime || ':' || IFNULL(ra, ''), ',') FROM frames),
                        IFNULL((SELECT group_concat(id || ':' || frame_id || ':' || pixel_scale_arcsec, ',')
                                FROM (SELECT * FROM plate_solves ORDER BY id)), '')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    let before = snapshot(&conn);
    let fake = Arc::new(crate::stacking::solve::FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);

    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 3);
    assert_eq!(
        snapshot(&conn),
        before,
        "plate_solves and frames are never written"
    );
    let solves =
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve")).unwrap();
    assert_eq!(solves.len(), 3);
    assert!(solves
        .iter()
        .all(|a| a.group_key.is_empty() && a.path.is_none()));

    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 3, "a cached outcome is never solved again");
    let h = handles[0];
    let record = crate::stacking::origin::stored_solve(&conn, &rc.origins[&h], h)
        .unwrap()
        .unwrap();
    assert_eq!(
        (record.crval1, record.crval2, record.frame_id),
        (83.8, -5.4, 0)
    );
}

/// Plan 6b-2 C14: a failed or refused solve is cached like a solved one,
/// so it is not retried every run, and the frame stays in the run
/// without a WCS.
#[test]
fn a_failed_or_refused_solve_is_cached_and_the_frame_stays() {
    use crate::stacking::solve::{FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(
        FakeSolver::solving_all()
            .with("c_r1.fits", SolveOutcome::Failed("no match".into()))
            .with(
                "c_r2.fits",
                SolveOutcome::Refused(
                    "Rejected: low confidence (6 inliers, ratio 0.01, 1.50\"/px)".into(),
                ),
            ),
    );
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    solve_project_frames(&mut rc).unwrap();
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 3, "every outcome is cached");
    assert!(
        rc.runtime_exclusions.is_empty(),
        "{:?}",
        rc.runtime_exclusions
    );
    for (name, solved) in [
        ("c_r1.fits", false),
        ("c_r2.fits", false),
        ("c_r3.fits", true),
    ] {
        let h = handle_of(&rc, name);
        assert_eq!(
            crate::stacking::origin::stored_solve(&conn, &rc.origins[&h], h)
                .unwrap()
                .is_some(),
            solved,
            "{name}"
        );
    }
}

/// Plan 6b-2 final review M4: a frame whose manifest has no pixel scale
/// is solved with no scale hint and no scale check, so the run says so,
/// once per frame, and still solves it.
#[test]
fn a_frame_without_a_manifest_scale_is_warned_about_before_solving() {
    use crate::stacking::solve::FakeSolver;
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for (uuid, scale) in [("r1", None), ("r2", Some(1.5)), ("r3", Some(1.5))] {
        let mut spec = test_fixtures::project_frame_spec(uuid);
        spec.pixel_scale_arcsec = scale;
        test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
    }
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    let logs = crate::stacking::solve::tests::captured_logs(|| {
        solve_project_frames(&mut rc).unwrap();
    });
    assert_eq!(fake.calls(), 3, "the frame is still solved");
    assert_eq!(
        logs.matches("manifest has no pixel scale").count(),
        1,
        "{logs}"
    );
    let r1 = handle_of(&rc, "c_r1.fits");
    assert!(logs.contains(&format!("frame_id={r1}")), "{logs}");
}

/// Plan 6b-2 final review M7: a project row that vanished is a warning,
/// never a silent blind solve; the frames are still solved, with no hint.
#[test]
fn a_missing_project_row_is_logged_and_nothing_is_cached() {
    use crate::stacking::solve::FakeSolver;
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    // No cascade: the run and its artifacts stay, only the row goes.
    conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    conn.execute("DELETE FROM collab_projects WHERE project_id = 'p1'", [])
        .unwrap();
    // The solve itself runs (without a hint), but its cache write then
    // fails on the artifact's foreign key to the project, so the run
    // reports the failure and nothing is cached.
    let mut result = None;
    let logs = crate::stacking::solve::tests::captured_logs(|| {
        result = Some(solve_project_frames(&mut rc));
    });
    assert!(
        logs.contains("project row missing; solving without a position hint"),
        "{logs}"
    );
    assert!(
        matches!(result, Some(Err(RunError::Other(ref m))) if m.contains("FOREIGN KEY")),
        "{result:?}"
    );
    assert!(
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve"))
            .unwrap()
            .is_empty()
    );
}

/// Review round 2: an `Unavailable` outcome is never cached, adds a run
/// warning and lets the run go on with the other frames.
#[test]
fn an_unavailable_solve_is_not_cached_and_the_run_continues() {
    use crate::stacking::solve::{FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::solving_all().with(
        "c_r1.fits",
        SolveOutcome::Unavailable("copy unwritable".into()),
    ));
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    solve_project_frames(&mut rc).unwrap();
    let r1 = handle_of(&rc, "c_r1.fits");
    assert!(
        crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(r1))
            .unwrap()
            .is_none()
    );
    for n in ["c_r2.fits", "c_r3.fits"] {
        let h = handle_of(&rc, n);
        assert!(
            crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(h))
                .unwrap()
                .is_some()
        );
    }
    assert!(
        rc.warnings.iter().any(|w| w.contains("copy unwritable")),
        "{:?}",
        rc.warnings
    );
    assert!(rc.runtime_exclusions.is_empty());
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(
        fake.calls(),
        4,
        "r1 is attempted again next time, the others are cached"
    );
}

/// Review round 2: a scratch folder that cannot be created is one warning
/// and no attempt and no cached outcome.
#[test]
fn a_scratch_folder_that_cannot_be_created_skips_solving_uncached() {
    use crate::stacking::solve::FakeSolver;
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    std::fs::create_dir_all(&rc.layout.root).unwrap();
    std::fs::write(rc.layout.tmp_root(), b"a file").unwrap();
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 0);
    assert_eq!(rc.warnings.len(), 1, "{:?}", rc.warnings);
    for n in ["c_r1.fits", "c_r2.fits", "c_r3.fits"] {
        let h = handle_of(&rc, n);
        assert!(
            crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(h))
                .unwrap()
                .is_none()
        );
    }
}

/// Review I7: a failure or refusal cached by ANOTHER app version is tried
/// once more (a release may carry a solver that rescues the frame); a solve
/// cached by another version is kept; this version's new outcomes then hold.
#[test]
fn a_failure_cached_by_another_app_version_is_solved_again() {
    use crate::stacking::solve::{
        solve_hash, write_artifact, FakeSolver, SolveArtifact, SolveOutcome,
    };
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    for (uuid, outcome) in [
        ("r1", SolveOutcome::Failed("no match".into())),
        ("r2", SolveOutcome::Refused("refused".into())),
        ("r3", SolveOutcome::Solved(FakeSolver::fixture_record())),
    ] {
        let blake3 = blake3_of(&conn, "p1", uuid);
        write_artifact(
            &conn,
            "p1",
            handle_of(&rc, &format!("c_{uuid}.fits")),
            &solve_hash(&blake3, "fake:1"),
            &SolveArtifact {
                blake3,
                app_version: "0.0.0-older".into(),
                outcome,
            },
        )
        .unwrap();
    }
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(
        fake.calls(),
        2,
        "the two failures are tried again; the solve is kept"
    );
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 2, "now cached by this version");
}

/// Plan 6b-2 C9 (the Solve boundary): a landed file that changed since
/// the frame source read it is excluded "changed during the run" and is
/// never handed to the solver.
#[test]
fn the_solve_sub_step_excludes_a_frame_whose_landed_file_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(crate::stacking::solve::FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    let r2 = handle_of(&rc, "c_r2.fits");
    let r2_path = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.frame_id == r2)
        .unwrap()
        .path
        .clone();
    std::fs::write(&r2_path, vec![0u8; 5000]).unwrap();
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 2);
    let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    assert_eq!(
        reasons.get(&r2).map(String::as_str),
        Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
    );
    assert!(
        crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(r2))
            .unwrap()
            .is_none()
    );
}

/// Plan 6b-2 C14: with no solver (no catalog installed) nothing is
/// solved or cached, and the run says why.
#[test]
fn without_a_solver_nothing_is_solved_and_the_run_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(crate::stacking::solve::FakeSolver::unavailable(
        "no star catalog",
    ));
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 0);
    assert!(
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        rc.warnings,
        vec!["3 frame(s) without a WCS in the manifest could not be plate-solved: no star catalog".to_string()]
    );
}

/// Review Focus 1: a frame-set run has nothing to solve. The sub-step
/// never asks the solver anything and the run gains no warning.
#[test]
fn the_solve_sub_step_is_a_no_op_for_a_frame_set() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _ids, working, output) = seed_star_group(
        &db_path,
        SET_NAME,
        &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)],
        &[5.0f32; 3],
    );
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter),
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        WorkingLayout::new(working.path(), &set_slug(SET_NAME)),
        output.path().to_path_buf(),
        group_ids,
    );
    let fake = Arc::new(crate::stacking::solve::FakeSolver::unavailable(
        "never asked",
    ));
    use_solver(&mut rc, &fake);
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 0);
    assert!(rc.warnings.is_empty(), "{:?}", rc.warnings);
}

/// Plan 6b-2 C14: a cached outcome whose hash differs (another solver
/// config or catalog), even one written by this app version, is a miss: the
/// frame is solved again and the row carries the current hash.
#[test]
fn a_cached_outcome_under_another_hash_is_solved_again() {
    use crate::stacking::solve::{solve_hash, write_outcome, FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 1, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    let h = handle_of(&rc, "c_r1.fits");
    let blake3 = blake3_of(&conn, "p1", "r1");
    write_outcome(
        &conn,
        "p1",
        h,
        &blake3,
        "stale",
        SolveOutcome::Solved(FakeSolver::fixture_record()),
    )
    .unwrap();
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(fake.calls(), 1);
    let row = crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(h))
        .unwrap()
        .unwrap();
    assert_eq!(row.config_hash, solve_hash(&blake3, "fake:1"));
}

/// A cancel raised during a solve ends the sub-step Cancelled and caches
/// nothing, not even the failure the solver returned.
#[test]
fn a_cancel_during_a_solve_caches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(crate::stacking::solve::FakeSolver::cancelling());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    let err = solve_project_frames(&mut rc).unwrap_err();
    assert!(matches!(err, RunError::Cancelled), "{err:?}");
    assert_eq!(fake.calls(), 1);
    assert!(
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve"))
            .unwrap()
            .is_empty()
    );
}

/// The solver gets the file stage 1 handed on (the `calibrated`
/// artifact's path), never the landed replica.
#[test]
fn the_solver_receives_the_prepared_file_not_the_replica() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 2, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(crate::stacking::solve::FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    let h = handle_of(&rc, "c_r1.fits");
    let landed = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.frame_id == h)
        .unwrap()
        .path
        .clone();
    let prepared = tmp.path().join("work").join("prepared").join("c_r1.fits");
    std::fs::create_dir_all(prepared.parent().unwrap()).unwrap();
    std::fs::copy(&landed, &prepared).unwrap();
    let updated = conn
        .execute(
            "UPDATE stacking_artifacts SET path = ?1 WHERE kind = 'calibrated' AND project_frame = ?2",
            rusqlite::params![prepared.to_string_lossy(), h],
        )
        .unwrap();
    assert_eq!(updated, 1);
    solve_project_frames(&mut rc).unwrap();
    let paths = fake.paths();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&prepared), "{paths:?}");
    assert!(!paths.contains(&PathBuf::from(&landed)), "{paths:?}");
}

/// With no solver, a frame whose solved outcome is cached is still served
/// and is not counted in the warning.
#[test]
fn the_unavailable_warning_counts_only_frames_without_a_solved_outcome() {
    use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let fake = Arc::new(FakeSolver::unavailable("no star catalog"));
    use_solver(&mut rc, &fake);
    seed_in_place_calibrated(&conn, &mut rc);
    write_outcome(
        &conn,
        "p1",
        handle_of(&rc, "c_r1.fits"),
        &blake3_of(&conn, "p1", "r1"),
        "h",
        SolveOutcome::Solved(FakeSolver::fixture_record()),
    )
    .unwrap();
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(
        rc.warnings,
        vec!["2 frame(s) without a WCS in the manifest could not be plate-solved: no star catalog"
            .to_string()]
    );
}

/// A catalog that does not open says nothing about a frame: the run warns
/// (counting only unsolved frames), asks the solver nothing and caches
/// nothing. A run whose frames are all cached never asks `ready`.
#[test]
fn a_catalog_that_does_not_open_is_never_cached_and_a_cached_run_never_opens_it() {
    use crate::stacking::solve::{solve_hash, write_outcome, FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let broken = Arc::new(FakeSolver::not_ready("tier_500: cannot open"));
    use_solver(&mut rc, &broken);
    seed_in_place_calibrated(&conn, &mut rc);
    solve_project_frames(&mut rc).unwrap();
    assert_eq!(broken.ready_calls(), 1);
    assert_eq!(broken.calls(), 0);
    assert!(
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        rc.warnings,
        vec!["3 frame(s) without a WCS in the manifest could not be plate-solved: tier_500: cannot open".to_string()]
    );

    // Every frame cached under the working solver's identity: no ready().
    let fake = Arc::new(FakeSolver::solving_all());
    use_solver(&mut rc, &fake);
    for uuid in ["r1", "r2", "r3"] {
        let blake3 = blake3_of(&conn, "p1", uuid);
        write_outcome(
            &conn,
            "p1",
            handle_of(&rc, &format!("c_{uuid}.fits")),
            &blake3,
            &solve_hash(&blake3, "fake:1"),
            SolveOutcome::Failed("no match".into()),
        )
        .unwrap();
    }
    solve_project_frames(&mut rc).unwrap();
    assert_eq!((fake.ready_calls(), fake.calls()), (0, 0));
}
