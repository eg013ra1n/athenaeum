//! Tests of the reference stage; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::registration::db::get_registration_for_frame_set;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, star_light_spec, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

#[test]
fn auto_reference_is_the_best_frame_of_the_largest_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    // f0 is the cleanest frame (noticeably, but not by a factor big
    // enough to push the others below the 5% weight floor) — it must
    // win the auto reference pick.
    let noise = [3.0f32, 5.0, 5.0, 5.0];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let cfg = StackingConfig::default();
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

    assert_eq!(rc.reference_frame_id, Some(light_ids[0]));
    assert!(rc.reference_calibrated.is_some());

    let row = crate::db::stacking::get_run(&fixture.conn, run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.reference_frame_id, Some(light_ids[0]));
    assert_eq!(row.reference_mode, "auto");
}

#[test]
fn manual_reference_excluded_by_selection_is_forced_included() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);

    // A 5th, pure-noise frame — selection excludes it by weight. It is
    // then chosen as the MANUAL reference: the run must keep it
    // included regardless (fix round 1, item 1).
    let noise_date = date_obs_at(4);
    let noise_spec = star_light_spec("noise", &noise_date);
    let (noise_id, _path) =
        test_fixtures::add_light_with_field(&fixture, &noise_spec, &[], 600.0, 30.0, 999);

    let mut all_ids = light_ids.clone();
    all_ids.push(noise_id);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &all_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, noise_id)
        .unwrap();

    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
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

    run_stages_for_test(&mut rc, Stage::Register).unwrap();

    assert_eq!(rc.reference_frame_id, Some(noise_id));

    let rows = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    let noise_row = rows
        .iter()
        .find(|r| r.frame_id == noise_id)
        .expect("the manual reference has a registration_results row");
    assert!(noise_row.is_reference);
    assert_eq!(noise_row.status, "reference");

    let frame_rows = crate::db::stacking::list_frame_rows(&fixture.conn, rc.run_id).unwrap();
    let noise_frame_row = frame_rows
        .iter()
        .find(|r| r.frame_id == noise_id)
        .expect("the manual reference has a stacking_run_frames row");
    assert!(noise_frame_row.included, "{noise_frame_row:?}");
    assert_eq!(noise_frame_row.reg_status.as_deref(), Some("reference"));

    assert_eq!(
        rc.warnings
            .iter()
            .filter(|w| w.contains("kept despite selection"))
            .count(),
        1,
        "{:?}",
        rc.warnings
    );
}

#[test]
fn manual_reference_in_the_exclusion_list_fails_the_run() {
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
    crate::registration::db::set_frame_set_reference(
        &fixture.conn,
        fixture.set_id,
        light_ids[0],
    )
    .unwrap();

    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
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
    // The reference frame is ALSO in the run's own manual exclusion
    // list — the plan gate cannot see this (it only checks the raw
    // file is on disk), so the run itself must refuse loudly.
    rc.excluded = vec![light_ids[0]];

    let err = run_stages_for_test(&mut rc, Stage::Register)
        .expect_err("a manually-excluded reference frame must fail the run");
    match err {
        RunError::Other(msg) => {
            assert!(
                msg.contains("the manual reference frame is excluded"),
                "{msg}"
            );
        }
        other => panic!("expected RunError::Other, got {other:?}"),
    }
}

/// Spec §5.4, plan 6b-2 C15: a project's auto reference is the best frame
/// WITH a WCS, from the largest group that holds one. It is never taken
/// from a larger, better-weighted group without one. In native geometry,
/// a group with no candidate takes the run reference.
#[test]
fn a_projects_auto_reference_is_the_best_frame_with_a_wcs() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 4, 3);
    let weights: HashMap<&str, f64> = [
        ("c_r1.fits", 0.9),
        ("c_r2.fits", 0.9),
        ("c_r3.fits", 0.9),
        ("c_r4.fits", 0.9),
        ("c_g1.fits", 0.4),
        ("c_g2.fits", 0.7),
        ("c_g3.fits", 0.6),
    ]
    .into_iter()
    .collect();
    let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));
    let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &work, &out);
    measure_all(&mut rc, &weights);
    stage_reference(&mut rc).unwrap();
    let g2 = handle_of(&rc, "c_g2.fits");
    assert_eq!(rc.reference_frame_id, Some(g2));
    assert_eq!(rc.summary.reference.filename.as_deref(), Some("c_g2.fits"));
    let rule = rc.wcs_rule.as_ref().unwrap();
    assert_eq!(rule.candidates.len(), 3);
    assert!(!rule.candidates.contains(&handle_of(&rc, "c_r1.fits")));

    let mut native = StackingConfig::default();
    native.registration.geometry = RegistrationGeometry::Native;
    let mut rc = project_rc(&ctx, &conn, "p1", native, &work, &out);
    measure_all(&mut rc, &weights);
    stage_reference(&mut rc).unwrap();
    assert_eq!(rc.group_geometry["mono__G__300s"].reference_frame_id, g2);
    assert_eq!(
        rc.group_geometry["mono__R__300s"].reference_frame_id, g2,
        "a group with no candidate takes the run reference"
    );
}

/// Spec §5.4, plan 6b-2 C15: a frame the run solved locally qualifies
/// only when no included frame has a manifest WCS.
#[test]
fn a_locally_solved_frame_qualifies_only_without_a_manifest_wcs() {
    use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));

    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab1"), "p1", 3, 0);
    let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &work, &out);
    let r2 = handle_of(&rc, "c_r2.fits");
    write_outcome(
        &conn,
        "p1",
        r2,
        &blake3_of(&conn, "p1", "r2"),
        "h",
        SolveOutcome::Solved(FakeSolver::fixture_record()),
    )
    .unwrap();
    let weights: HashMap<&str, f64> =
        [("c_r1.fits", 0.9), ("c_r2.fits", 0.5), ("c_r3.fits", 0.8)]
            .into_iter()
            .collect();
    measure_all(&mut rc, &weights);
    stage_reference(&mut rc).unwrap();
    assert_eq!(
        rc.reference_frame_id,
        Some(r2),
        "the one frame with a WCS, not the best-weighted"
    );

    crate::db::stacking::insert_test_project(&conn, "p2");
    seed_r_and_g(&conn, &tmp.path().join("collab2"), "p2", 3, 3);
    let mut rc = project_rc(&ctx, &conn, "p2", StackingConfig::default(), &work, &out);
    let r2 = handle_of(&rc, "c_r2.fits");
    write_outcome(
        &conn,
        "p2",
        r2,
        &blake3_of(&conn, "p2", "r2"),
        "h",
        SolveOutcome::Solved(FakeSolver::fixture_record()),
    )
    .unwrap();
    measure_all(&mut rc, &[("c_r2.fits", 0.99)].into_iter().collect());
    stage_reference(&mut rc).unwrap();
    let rule = rc.wcs_rule.as_ref().unwrap();
    assert!(
        !rule.candidates.contains(&r2) && rule.with_wcs.contains(&r2),
        "{rule:?}"
    );
    let g: HashSet<i64> = ["c_g1.fits", "c_g2.fits", "c_g3.fits"]
        .iter()
        .map(|n| handle_of(&rc, n))
        .collect();
    assert!(g.contains(&rc.reference_frame_id.unwrap()));
}

/// Spec §5.4: with no frame carrying a WCS the reference stage fails, in
/// the spec's words.
#[test]
fn with_no_frame_with_a_wcs_the_reference_stage_fails() {
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
    measure_all(&mut rc, &HashMap::new());
    let err = stage_reference(&mut rc).unwrap_err();
    assert!(
        matches!(err, RunError::Other(ref m) if m == NO_PROJECT_REFERENCE),
        "{err:?}"
    );
    assert_eq!(
        NO_PROJECT_REFERENCE,
        "no frame of this project could be plate-solved; the grid needs one"
    );
}

/// Plan 6b-2 C15: a manual pin without a WCS fails the stage; a pin with
/// one is the reference.
#[test]
fn a_manual_pin_without_a_wcs_fails_the_reference_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 3);
    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));

    let mut rc = project_rc(&ctx, &conn, "p1", cfg.clone(), &work, &out);
    crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("r1")).unwrap();
    measure_all(&mut rc, &HashMap::new());
    let err = stage_reference(&mut rc).unwrap_err();
    assert!(
        matches!(err, RunError::Other(ref m) if m == MANUAL_REFERENCE_WITHOUT_WCS),
        "{err:?}"
    );

    let mut rc = project_rc(&ctx, &conn, "p1", cfg, &work, &out);
    crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("g2")).unwrap();
    measure_all(&mut rc, &HashMap::new());
    stage_reference(&mut rc).unwrap();
    assert_eq!(rc.reference_frame_id, Some(handle_of(&rc, "c_g2.fits")));
}

/// Review Focus 1, plan 6b-2 C15: a frame-set run has no WCS rule, so its
/// reference pick, its native groups and its two-pass re-pick take
/// today's paths unchanged.
#[test]
fn a_frame_set_run_has_no_wcs_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, ids, working, output) = seed_star_group(
        &db_path,
        SET_NAME,
        &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)],
        &[5.0f32; 3],
    );
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
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
    let rc = test_context(
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
    assert!(wcs_tiers(&rc).unwrap().is_none());
    assert!(rc.wcs_rule.is_none());

    // The Reference stage itself leaves no rule behind, and every group
    // may switch (the two-pass predicate is today's answer).
    let mut rc = rc;
    run_stages_for_test(&mut rc, Stage::Reference).unwrap();
    assert!(rc.wcs_rule.is_none());
    let reference = rc.reference_frame_id.unwrap();
    for g in rc.plan_groups.clone() {
        assert!(group_can_switch(&rc, &g.key, reference));
    }
}

/// Review round 1, item 5: a group that will be integrated (>= 3 included)
/// supplies the reference, not a skipped group's better-weighted candidate.
#[test]
fn an_integrated_groups_candidate_wins_over_a_skipped_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for (filter, n) in [("R", 3usize), ("G", 2usize)] {
        for i in 1..=n {
            let uuid = format!("{}{i}", filter.to_lowercase());
            let mut spec = test_fixtures::project_frame_spec(&uuid);
            spec.filter = filter;
            spec.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
            test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
        }
    }
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let weights: HashMap<&str, f64> = [("c_g1.fits", 0.99), ("c_g2.fits", 0.98)]
        .into_iter()
        .collect();
    measure_all(&mut rc, &weights);
    stage_reference(&mut rc).unwrap();
    let reference = rc.reference_frame_id.unwrap();
    let r: HashSet<i64> = ["c_r1.fits", "c_r2.fits", "c_r3.fits"]
        .iter()
        .map(|n| handle_of(&rc, n))
        .collect();
    assert!(r.contains(&reference), "the integrated R group supplies it");
}

/// Review round 1, item 7 (I6): an excluded pin without a WCS fails with
/// the exclusion message, not the WCS one.
#[test]
fn an_excluded_manual_pin_reports_the_exclusion_before_the_wcs() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 3);
    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("r1")).unwrap();
    measure_all(&mut rc, &HashMap::new());
    let r1 = handle_of(&rc, "c_r1.fits");
    for entry in rc.measured.values_mut().flatten() {
        if entry.frame.frame_id == r1 {
            entry.calibrated = None;
            entry.included = false;
            entry.reason = Some("calibration failed".to_string());
        }
    }
    let err = stage_reference(&mut rc).unwrap_err();
    assert!(
        matches!(err, RunError::Other(ref m) if m == "the manual reference frame is excluded: calibration failed"),
        "{err:?}"
    );
}

/// Review round 1, item 7: a manual pin with only a LOCAL WCS qualifies
/// even while manifest frames exist (`with_wcs` is manifest plus local;
/// only the Auto candidates prefer the manifest). And a pin that stage 3
/// excluded still counts as included for the tier decision.
#[test]
fn a_manual_pin_with_a_local_wcs_qualifies_and_an_excluded_pin_counts_as_included() {
    use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 3);
    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));

    let mut rc = project_rc(&ctx, &conn, "p1", cfg.clone(), &work, &out);
    let r1 = handle_of(&rc, "c_r1.fits");
    write_outcome(
        &conn,
        "p1",
        r1,
        &blake3_of(&conn, "p1", "r1"),
        "h",
        SolveOutcome::Solved(FakeSolver::fixture_record()),
    )
    .unwrap();
    crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("r1")).unwrap();
    measure_all(&mut rc, &HashMap::new());
    stage_reference(&mut rc).unwrap();
    assert_eq!(rc.reference_frame_id, Some(r1));

    // Only the pinned manifest frame is a manifest frame that is not
    // included: it still makes the manifest tier the candidate tier.
    let mut rc = project_rc(&ctx, &conn, "p1", cfg, &work, &out);
    crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("g2")).unwrap();
    measure_all(&mut rc, &HashMap::new());
    let g2 = handle_of(&rc, "c_g2.fits");
    for g in ["c_g1.fits", "c_g3.fits"] {
        let id = handle_of(&rc, g);
        for entry in rc.measured.values_mut().flatten() {
            if entry.frame.frame_id == id {
                entry.included = false;
            }
        }
    }
    for entry in rc.measured.values_mut().flatten() {
        if entry.frame.frame_id == g2 {
            entry.included = false;
        }
    }
    stage_reference(&mut rc).unwrap();
    let rule = rc.wcs_rule.as_ref().unwrap();
    assert!(rule.candidates.contains(&g2), "{rule:?}");
    assert!(
        !rule.candidates.contains(&r1),
        "the manifest tier wins: {rule:?}"
    );
}

/// Plan 6b-3 D14 (fix round 1): the plan and the run's reference rule
/// agree on a WCS the reference cannot be built on. One frame's manifest
/// WCS carries a half-stored reverse SIP pair (`from_record` refuses it):
/// the plan has no `reference` blocker, and the run's candidates are the
/// three other frames, never that one.
#[test]
fn an_unusable_manifest_wcs_is_never_a_reference_candidate() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let sip = serde_json::json!({
        "order": 2,
        "a": [[0.0, 0.0, 1e-6], [0.0, 2e-6], [3e-6]],
        "b": [[0.0, 0.0, 1e-6], [0.0, 2e-6], [3e-6]],
        "ap": [[0.0]],
    });
    let changed = conn
        .execute(
            "UPDATE project_frames_local \
                 SET manifest_json = json_set(manifest_json, '$.meta.wcs.sip', json(?1)) \
                 WHERE project_id = 'p1' AND frame_uuid = 'l0'",
            [sip.to_string()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let cfg = sky_config(&conn);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg.clone(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let uuid_of = |rc: &RunContext, id: i64| match &rc.origins[&id] {
        FrameOrigin::Project { frame_uuid, .. } => frame_uuid.clone(),
        _ => unreachable!(),
    };
    let bad_id = *rc
        .origins
        .keys()
        .find(|id| uuid_of(&rc, **id) == "l0")
        .unwrap();
    let good_ids: HashSet<i64> = rc
        .origins
        .keys()
        .copied()
        .filter(|id| *id != bad_id)
        .collect();
    assert_eq!(good_ids.len(), 3);

    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        Some(cfg),
    )
    .unwrap();
    assert!(
        plan.blockers.iter().all(|b| b.code != "reference"),
        "{:?}",
        plan.blockers
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("a WCS that cannot be used")),
        "{:?}",
        plan.warnings
    );

    stage_calibrate(&mut rc).unwrap();
    stage_measure(&mut rc).unwrap();
    let rule = wcs_tiers(&rc).unwrap().expect("a project has a rule");
    assert_eq!(rule.candidates, good_ids);
    assert_eq!(rule.with_wcs, good_ids);
}
