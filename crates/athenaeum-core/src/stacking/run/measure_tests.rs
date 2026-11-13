//! Tests of the measure stage; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, shifted_stars, star_light_spec, wait_for_run, LightSpec,
    STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

#[test]
fn measure_reuses_metrics_artifacts() {
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

    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");

    let build = |run_id: i64, group_ids: HashMap<String, i64>| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output_dir.clone(),
            group_ids,
        )
    };

    let (run_id1, group_ids1) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc1 = build(run_id1, group_ids1);
    run_stages_for_test(&mut rc1, Stage::Measure).unwrap();

    let metrics1 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Metrics),
    )
    .unwrap();
    assert_eq!(metrics1.len(), 4, "{metrics1:?}");
    let before: HashMap<i64, String> = metrics1
        .iter()
        .map(|a| (a.frame_id.unwrap(), a.created_at.clone()))
        .collect();

    let (run_id2, group_ids2) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc2 = build(run_id2, group_ids2);
    run_stages_for_test(&mut rc2, Stage::Measure).unwrap();

    let metrics2 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Metrics),
    )
    .unwrap();
    assert_eq!(metrics2.len(), 4, "{metrics2:?}");
    let after: HashMap<i64, String> = metrics2
        .iter()
        .map(|a| (a.frame_id.unwrap(), a.created_at.clone()))
        .collect();
    assert_eq!(
        before, after,
        "a fresh metrics artifact must not be recreated"
    );
}

/// Plan B B4 floor (i) (R-PB4-5), the run wiring: a plane stored at the fit-call floor (a fit, no `c`) takes its
/// group channel's median kept `c` in `measure_group_finish`, on the cached path, BEFORE `compute_weights` -
/// `tflux` scales by it, `psf_signal_weight` by its square, and the frame's weight reads the scaled value. The
/// stored `metrics` artifact keeps the module's values. (The pure rule is pinned by `substitute_group_c`'s tests.)
#[test]
fn measure_folds_a_floored_member_to_the_group_channel_median_c_before_the_weights() {
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
    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let group_key = plan_groups[0].key.clone();
    let subject = StackSubject::FrameSet(fixture.set_id);
    let run = || {
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
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output_dir.clone(),
            group_ids,
        );
        run_stages_for_test(&mut rc, Stage::Measure).unwrap();
        rc
    };
    let rc1 = run();
    drop(rc1);
    let rows = metrics_by_frame(&fixture.conn, fixture.set_id);
    let mut ids: Vec<i64> = rows.keys().copied().collect();
    ids.sort_unstable();
    let floored_id = ids[0];
    // This fixture's Gaussian stars measure no `c`, so the stored rows are set by hand: three peers carry a `c`
    // (the cached path reads the payload as is), the first member is at floor (i): a fit ran, no `c`.
    let peer_c = [1.10, 1.30, 1.20];
    let median = 1.20;
    let store = |frame_id: i64, c: Option<f64>| {
        let (hash, mut m, _) = rows[&frame_id].clone();
        assert!(m.channels[0].stars_fitted > 0);
        m.channels[0].aperture_correction = c;
        let payload = serde_json::to_string(&m).unwrap();
        upsert_artifact(
            &fixture.conn,
            &NewArtifact {
                subject: &subject,
                frame_id: Some(frame_id),
                group_key: "",
                kind: ArtifactKind::Metrics,
                path: None,
                config_hash: &hash,
                size: None,
                modified_at: None,
                payload_json: Some(&payload),
            },
        )
        .unwrap();
        m
    };
    let stored = store(floored_id, None);
    for (id, c) in ids[1..].iter().zip(peer_c) {
        store(*id, Some(c));
    }

    let rc2 = run();
    let entries = &rc2.measured[&group_key];
    let floored = entries
        .iter()
        .find(|e| e.frame.frame_id == floored_id)
        .and_then(|e| e.measurement.as_ref())
        .expect("the floored member is measured");
    let ch = &floored.channels[0];
    assert!(entries.iter().all(|e| e.cached_metrics), "the cached path");
    assert_eq!(ch.aperture_correction, None);
    assert_eq!(ch.aperture_correction_group, Some(median));
    assert!((ch.tflux - stored.channels[0].tflux * median).abs() <= 1e-9 * ch.tflux.abs());
    assert!(
        (ch.psf_signal_weight - stored.channels[0].psf_signal_weight * median * median).abs()
            <= 1e-9 * ch.psf_signal_weight.abs()
    );
    for e in entries.iter().filter(|e| e.frame.frame_id != floored_id) {
        let c = &e.measurement.as_ref().unwrap().channels[0];
        assert_eq!(
            c.aperture_correction_group, None,
            "a measured plane is not substituted"
        );
    }
    // `compute_weights` read the stand-in: the weight is the scaled value, not the stored one.
    let w = entries
        .iter()
        .find(|e| e.frame.frame_id == floored_id)
        .and_then(|e| e.weight.as_ref())
        .expect("a weight");
    assert!(
        (w.channels[0] - ch.psf_signal_weight).abs() <= 1e-9 * ch.psf_signal_weight.abs(),
        "{:?}",
        w.channels
    );
    // The artifact keeps the module's values.
    let after = metrics_by_frame(&fixture.conn, fixture.set_id);
    assert_eq!(
        after[&floored_id].1.channels[0].aperture_correction_group,
        None
    );
    assert_eq!(
        after[&floored_id].1.channels[0].tflux,
        stored.channels[0].tflux
    );
}

/// Plan A Task 11b (owner decision R11b-12): the cache follows the group β. An unchanged group β is a cache hit
/// (no row rewritten); a group β that moves — here two members' stored own βs set below every other's, so the
/// lower median of four moves (one member's cannot: this fixture's Gaussian stars read one own β on every frame)
/// — makes the plan gate report Measure stale and re-runs the second pass for EVERY member: each row is re-keyed
/// on the new group β, every channel fitted at it, and the own βs are the artifact's (Plan A Task 11c: pass 1
/// was spilled to `measured/` once, so the move runs no pass 1 at all and the artifact rows are untouched).
#[test]
fn measure_reruns_pass_two_for_the_group_when_the_group_beta_moves() {
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
    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let subject = StackSubject::FrameSet(fixture.set_id);
    let run = |label: &str| {
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
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output_dir.clone(),
            group_ids,
        );
        run_stages_for_test(&mut rc, Stage::Measure)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        crate::db::stacking::list_groups(&fixture.conn, run_id).unwrap()[0].beta
    };
    let measure_stale = || {
        build_plan(
            &fixture.conn,
            &ctx.settings,
            &PathPolicy::AllowAll,
            &subject,
            Some(cfg.clone()),
        )
        .unwrap()
        .stale_stages
        .contains(&Stage::Measure)
    };
    let calibrated = |frame_id: i64| {
        crate::db::stacking::find_artifact(
            &fixture.conn,
            &subject,
            "",
            ArtifactKind::Calibrated,
            Some(frame_id),
        )
        .unwrap()
        .unwrap()
        .config_hash
    };

    let beta1 = run("run 1").expect("a group beta");
    let logged = || pass1_logged_under(working.path()).len();
    let measured1 = rows_of_kind(&fixture.conn, fixture.set_id, ArtifactKind::Measured);
    assert_eq!(logged(), 4, "run 1 ran pass 1 for every member, once");
    assert_eq!(measured1.len(), 4, "run 1 stored every member's first pass");
    for (frame_id, row) in &measured1 {
        assert_eq!(
            row.config_hash,
            pass1_hash_for(&cfg, &calibrated(*frame_id)),
            "frame {frame_id}: keyed on the calibration hash and the measurement subtree, not the group beta"
        );
    }
    let rows1 = metrics_by_frame(&fixture.conn, fixture.set_id);
    assert_eq!(rows1.len(), 4);
    for (frame_id, (hash, _, _)) in &rows1 {
        assert_eq!(
            *hash,
            measurement_hash_for(&cfg, &calibrated(*frame_id), Some(beta1))
        );
    }
    assert!(!measure_stale(), "an unchanged group is cached");

    // Unchanged group beta: a cache hit, nothing rewritten.
    assert_eq!(run("run 2"), Some(beta1));
    let rows2 = metrics_by_frame(&fixture.conn, fixture.set_id);
    for (frame_id, (hash, _, created)) in &rows2 {
        assert_eq!(hash, &rows1[frame_id].0);
        assert_eq!(
            created, &rows1[frame_id].2,
            "frame {frame_id}: a cache hit rewrites nothing"
        );
    }

    assert_eq!(logged(), 4, "a cache hit runs no pass 1");

    // Two members' stored own betas below every other's: the lower median of four moves to the larger of them.
    let mut ids: Vec<i64> = rows1.keys().copied().collect();
    ids.sort_unstable();
    let lowered = [(ids[0], 1.6), (ids[1], 1.7)];
    assert!(rows1
        .values()
        .all(|(_, m, _)| m.channels[0].beta_own.unwrap() > 1.7));
    for (frame_id, own) in lowered {
        let (hash, mut m, _) = rows1[&frame_id].clone();
        m.channels[0].beta_own = Some(own);
        let payload = serde_json::to_string(&m).unwrap();
        upsert_artifact(
            &fixture.conn,
            &NewArtifact {
                subject: &subject,
                frame_id: Some(frame_id),
                group_key: "",
                kind: ArtifactKind::Metrics,
                path: None,
                config_hash: &hash,
                size: None,
                modified_at: None,
                payload_json: Some(&payload),
            },
        )
        .unwrap();
    }
    assert!(
        measure_stale(),
        "the gate resolves the moved group beta and reads every member stale"
    );

    assert_eq!(run("run 3"), Some(1.7));
    assert_eq!(
        logged(),
        4,
        "the group-beta move re-ran pass 2 only: no pass 1 on a valid artifact"
    );
    for (frame_id, row) in rows_of_kind(&fixture.conn, fixture.set_id, ArtifactKind::Measured) {
        assert_eq!(
            row.created_at, measured1[&frame_id].created_at,
            "frame {frame_id}: the artifact is written once and reused"
        );
    }
    let rows3 = metrics_by_frame(&fixture.conn, fixture.set_id);
    for (frame_id, (hash, m, _)) in &rows3 {
        assert_eq!(
            *hash,
            measurement_hash_for(&cfg, &calibrated(*frame_id), Some(1.7)),
            "frame {frame_id}"
        );
        assert_ne!(*hash, rows1[frame_id].0, "frame {frame_id}: re-keyed");
        assert_eq!(
            m.channels[0].beta, 1.7,
            "frame {frame_id}: fitted at the new group beta"
        );
        assert_eq!(
            m.channels[0].beta_own, rows1[frame_id].1.channels[0].beta_own,
            "frame {frame_id}: the own beta is the artifact's (the pixels' first read)"
        );
    }
}

/// The `(group key, created_at, config hash, path)` of every `metrics`, `measured` and `fits.0` row, by (kind,
/// frame): what a regroup must leave untouched.
fn measure_rows_snapshot(
    rig: &Pass1Rig,
) -> Vec<(String, i64, String, String, String, Option<String>)> {
    let mut out = Vec::new();
    for kind in [ArtifactKind::Metrics, ArtifactKind::Measured, ArtifactKind::Fits(0)] {
        for (frame_id, row) in rig.rows(kind) {
            out.push((
                kind.to_string(),
                frame_id,
                row.group_key.clone(),
                row.created_at.clone(),
                row.config_hash.clone(),
                row.path.clone(),
            ));
        }
    }
    out.sort();
    out
}

/// Frame-level cache spec §5.1 (Task 4): a regroup (here "one group per exposure" -> "no split") finds the
/// `metrics`, `measured` and `fits.0` rows again: neither pass runs, the group beta is the same, and no row or
/// file is rewritten.
#[test]
fn measure_rows_are_reused_across_a_regroup() {
    let mut rig = Pass1Rig::new();
    rig.cfg.grouping.exposure_mode = crate::stacking::config::ExposureMode::Seconds;
    rig.plan_groups =
        group_frames(&rig.fixture.conn, rig.fixture.set_id, &rig.cfg.grouping).unwrap();
    let (first, logs1) =
        crate::stacking::telemetry::capture_logs(|| rig.run("run 1", None));
    assert_eq!(fan_out_items(&logs1, "measure", "pass1"), 4, "{logs1}");
    assert_eq!(fan_out_items(&logs1, "measure", "pass2"), 4, "{logs1}");
    let rows1 = measure_rows_snapshot(&rig);
    assert_eq!(rows1.len(), 12, "metrics, measured and fits.0 for four frames");
    assert!(rows1.iter().all(|r| r.2.is_empty()), "frame-level rows sit under the empty key: {rows1:?}");

    let old_key = rig.plan_groups[0].key.clone();
    rig.cfg.grouping.exposure_mode = crate::stacking::config::ExposureMode::NoSplit;
    rig.plan_groups =
        group_frames(&rig.fixture.conn, rig.fixture.set_id, &rig.cfg.grouping).unwrap();
    assert_ne!(rig.plan_groups[0].key, old_key, "the two groupings must name the group differently");
    let (second, logs2) =
        crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    assert_eq!(fan_out_items(&logs2, "measure", "pass1"), 0, "a regroup re-ran pass 1: {logs2}");
    assert_eq!(fan_out_items(&logs2, "measure", "pass2"), 0, "a regroup re-ran pass 2: {logs2}");
    assert!(second.logged.is_empty(), "{:?}", second.logged);
    assert_eq!(second.beta, first.beta, "the same members, the same group beta");
    assert_eq!(measure_rows_snapshot(&rig), rows1, "a regroup rewrote or added rows");
}

/// Frame-level cache spec §5.1, review focus 1 (Task 4): two exposure classes with different PSF shapes (one
/// Moffat at beta 2.5, one Gaussian) have different group betas; merged into one group the group beta is a third
/// value. Pass 1 is reused (it is keyed on the calibration hash and the measurement subtree, not on the group
/// beta) and pass 2 re-runs for every member.
#[test]
fn merged_group_with_a_new_beta_reruns_pass_two_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    // Three frames per class: a group needs three included frames to be measured at all.
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (0.0, 0.0), (3.0, 0.0), (0.0, 3.0)];
    let mut light_ids = Vec::new();
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let moffat_class = i < 3;
        let spec = LightSpec {
            exptime: if moffat_class { 60.0 } else { 120.0 },
            ..star_light_spec(&stem, &date_obs)
        };
        let stars = shifted_stars(dx, dy);
        let (id, _) = if moffat_class {
            test_fixtures::add_light_with_moffat_field(
                &fixture,
                &spec,
                &stars,
                3.4,
                2.5,
                600.0,
                5.0,
                100 + i as u64,
            )
        } else {
            test_fixtures::add_light_with_field_fwhm(
                &fixture,
                &spec,
                &stars,
                3.4,
                600.0,
                5.0,
                100 + i as u64,
            )
        };
        light_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, working.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    // One Measure stage under `mode`; returns every group's beta and the captured logs.
    let measure = |mode: crate::stacking::config::ExposureMode| {
        let mut cfg = StackingConfig::default();
        cfg.grouping.exposure_mode = mode;
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
            plan_groups.clone(),
            layout.clone(),
            output.path().to_path_buf(),
            group_ids,
        );
        let (res, logs) = crate::stacking::telemetry::capture_logs(|| {
            run_stages_for_test(&mut rc, Stage::Measure)
        });
        res.unwrap();
        let betas: Vec<f64> = plan_groups
            .iter()
            .map(|g| rc.measured_groups[&g.key].beta.expect("a measured group has a beta"))
            .collect();
        (plan_groups.len(), betas, logs)
    };

    let (n_split, betas_split, logs1) = measure(crate::stacking::config::ExposureMode::Seconds);
    assert_eq!(n_split, 2, "one group per exposure class");
    assert_eq!(fan_out_items(&logs1, "measure", "pass1"), 6, "{logs1}");
    assert_eq!(fan_out_items(&logs1, "measure", "pass2"), 6, "{logs1}");

    let (n_merged, betas_merged, logs2) =
        measure(crate::stacking::config::ExposureMode::NoSplit);
    assert_eq!(n_merged, 1);
    assert!(
        betas_split.iter().any(|b| *b != betas_merged[0]),
        "premise: the merged group's beta differs from a source group's: {betas_split:?} -> {betas_merged:?}"
    );
    assert!(
        betas_split.iter().all(|b| *b != betas_merged[0]),
        "premise: ... and from both of them: {betas_split:?} -> {betas_merged:?}"
    );
    assert_eq!(
        fan_out_items(&logs2, "measure", "pass1"),
        0,
        "the first passes are the frames', not the group's: {logs2}"
    );
    assert_eq!(
        fan_out_items(&logs2, "measure", "pass2"),
        6,
        "every member is refitted at the new group beta: {logs2}"
    );
}

/// Frame-level cache spec §5.7 (Task 4): a `measured` row whose path is in an old `measured/<group_key>/` folder
/// under another stem is the artifact, read as it stands: no pass 1, no spurious refusal for the file the layout
/// would name now, nothing written to the layout path.
#[test]
fn the_spill_is_read_from_the_row_path() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let old_dir = rig.measured_root().join("old_group_key");
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut moved = Vec::new();
    for (frame_id, row) in rig.rows(ArtifactKind::Measured) {
        let old = old_dir.join(format!("legacy_{frame_id}.athm"));
        std::fs::rename(row.path.as_ref().unwrap(), &old).unwrap();
        rig.fixture
            .conn
            .execute(
                "UPDATE stacking_artifacts SET path = ?1 WHERE id = ?2",
                rusqlite::params![old.to_str().unwrap(), row.id],
            )
            .unwrap();
        moved.push(old);
    }
    assert_eq!(moved.len(), 4);
    // Pass 2 is lost; the first passes are only on disk under their old names.
    rig.delete_rows(&[ArtifactKind::Metrics, ArtifactKind::Fits(0)]);

    let (second, logs) = crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    assert_eq!(fan_out_items(&logs, "measure", "pass1"), 0, "pass 1 re-ran: {logs}");
    assert_eq!(fan_out_items(&logs, "measure", "pass2"), 4, "{logs}");
    assert!(second.logged.is_empty(), "pass 1 re-ran for {:?}", second.logged);
    assert!(
        !logs.contains("first-pass artifact"),
        "no refusal or stale line for a spill read from its row: {logs}"
    );
    let rows = rig.rows(ArtifactKind::Measured);
    assert_eq!(rows.len(), 4);
    for row in rows.values() {
        let path = PathBuf::from(row.path.as_ref().unwrap());
        assert!(moved.contains(&path), "the row keeps its path: {path:?}");
        assert!(path.is_file());
    }
    let in_layout_root: Vec<_> = std::fs::read_dir(rig.measured_root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    assert!(in_layout_root.is_empty(), "nothing was written to the layout path: {in_layout_root:?}");
}

/// Moves each frame's `kind` file into `<root>/old_group_key/legacy_<id><suffix>` and points its row there: what the
/// migration (spec §5.7) leaves. Returns the new path by frame id.
fn move_rows_to_legacy_folder(
    rig: &Pass1Rig,
    kind: ArtifactKind,
    root: &Path,
    suffix: &str,
) -> HashMap<i64, PathBuf> {
    let old_dir = root.join("old_group_key");
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut moved = HashMap::new();
    for (frame_id, row) in rig.rows(kind) {
        let old = old_dir.join(format!("legacy_{frame_id}{suffix}"));
        std::fs::rename(row.path.as_ref().unwrap(), &old).unwrap();
        rig.fixture
            .conn
            .execute(
                "UPDATE stacking_artifacts SET path = ?1 WHERE id = ?2",
                rusqlite::params![old.to_str().unwrap(), row.id],
            )
            .unwrap();
        moved.insert(frame_id, old);
    }
    moved
}

/// The layout's pass-1 path for one frame of the rig's group.
fn layout_spill_of(rig: &Pass1Rig, frame_id: i64) -> PathBuf {
    let frame = rig.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.frame_id == frame_id)
        .expect("a frame of the group");
    rig.layout.measured_path(&frame_file_stem(frame))
}

/// Ruling R6 M1 (Task 7): a REFUSED first-pass artifact is rewritten at the layout path, not the old row path, so
/// `replace_frame_level_file` retires the legacy `measured/<old_key>/` file. Three refusal arms: a head refused
/// with the member's own beta on its `metrics` row (validated when pass 2 needs it), a head refused on the
/// controller at prepare time (no `metrics` row), and a body refused by the worker's whole-file checksum; the
/// fourth member's legacy file is valid and stays where it is.
#[test]
fn a_refused_spill_is_rewritten_at_the_layout_path() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let ids = rig.sorted_ids();
    assert_eq!(ids.len(), 4);
    let moved = move_rows_to_legacy_folder(&rig, ArtifactKind::Measured, &rig.measured_root(), ".athm");
    let flip = |path: &Path, at: usize| {
        let mut bytes = std::fs::read(path).unwrap();
        let at = if at == usize::MAX { bytes.len() / 2 } else { at };
        bytes[at] ^= 0x5a;
        std::fs::write(path, &bytes).unwrap();
    };
    let head = crate::stacking::pass1_artifact::PREFIX_LEN;
    flip(&moved[&ids[0]], head); // pending arm: metrics row present, head refused
    flip(&moved[&ids[1]], usize::MAX); // worker arm: head fine, whole-file checksum refused
    flip(&moved[&ids[3]], head); // prepare arm: metrics row gone, head refused
    rig.fixture
        .conn
        .execute(
            "DELETE FROM stacking_artifacts WHERE kind = 'metrics' AND frame_id = ?1",
            [ids[3]],
        )
        .unwrap();
    // Pass 2 is owed for all four: their `fits.0` rows are gone.
    rig.delete_rows(&[ArtifactKind::Fits(0)]);

    let second = rig.run("run 2", None);
    let mut rerun = second.logged.clone();
    rerun.sort();
    let mut expected: Vec<PathBuf> = [ids[0], ids[1], ids[3]]
        .iter()
        .map(|&id| rig.calibrated_path(id))
        .collect();
    expected.sort();
    assert_eq!(rerun, expected, "exactly the three refused artifacts re-ran pass 1");
    assert_eq!(second.excluded, 0);

    let rows = rig.rows(ArtifactKind::Measured);
    assert_eq!(rows.len(), 4);
    for &id in &[ids[0], ids[1], ids[3]] {
        let path = PathBuf::from(rows[&id].path.clone().unwrap());
        assert_eq!(path, layout_spill_of(&rig, id), "frame {id}: rewritten at the layout path");
        assert!(path.is_file(), "frame {id}");
        assert!(!moved[&id].exists(), "frame {id}: the legacy file is retired");
    }
    let kept = PathBuf::from(rows[&ids[2]].path.clone().unwrap());
    assert_eq!(kept, moved[&ids[2]], "a valid legacy artifact keeps its row path");
    assert!(kept.is_file());
}

/// Review F4 (file-identity, Minor 1): a member whose own beta is on its `metrics` row keeps its `measured` row until
/// pass 2 needs it; its artifact is then refused and the worker writes the file. The other set's row over that file
/// (a same-named set shares the working root) goes before the write, like every other frame-level writer's.
#[test]
fn a_refused_pending_spill_retires_the_other_sets_row() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let ids = rig.sorted_ids();
    let other = same_named_set_with_copies_of_rows(&rig.fixture.conn, rig.fixture.set_id, ArtifactKind::Measured);
    assert_eq!(rows_of_kind(&rig.fixture.conn, other, ArtifactKind::Measured).len(), 4, "premise: the copies stand");
    let moved = move_rows_to_legacy_folder(&rig, ArtifactKind::Measured, &rig.measured_root(), ".athm");
    let mut bytes = std::fs::read(&moved[&ids[0]]).unwrap();
    bytes[crate::stacking::pass1_artifact::PREFIX_LEN] ^= 0x5a;
    std::fs::write(&moved[&ids[0]], &bytes).unwrap();
    // Pass 2 is owed for all four, and the first member's `metrics` row stays: its artifact is validated only now.
    rig.delete_rows(&[ArtifactKind::Fits(0)]);

    let second = rig.run("run 2", None);
    assert_eq!(second.logged, vec![rig.calibrated_path(ids[0])], "only the refused artifact re-ran pass 1");
    let left = rows_of_kind(&rig.fixture.conn, other, ArtifactKind::Measured);
    assert!(!left.contains_key(&ids[0]), "the other set keeps a row over the file the worker rewrote: {left:?}");
}

/// Ruling R6 M2 (Task 7): stale `fits.0` and `measured` rows pointing into `fits/<old_key>/` and
/// `measured/<old_key>/`, `rerun_from = Measure`: the old files are gone and the new ones sit at
/// `<stem>_f<id>` in the flat folders.
#[test]
fn a_forced_measure_replaces_files_in_old_group_folders() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let old_measured = move_rows_to_legacy_folder(&rig, ArtifactKind::Measured, &rig.measured_root(), ".athm");
    let old_fits = move_rows_to_legacy_folder(&rig, ArtifactKind::Fits(0), &rig.layout.fits_root(), ".p0.athf");
    assert_eq!((old_measured.len(), old_fits.len()), (4, 4));

    rig.run("run 2", Some(Stage::Measure));
    for group_frame in &rig.plan_groups[0].frames {
        let id = group_frame.frame_id;
        let stem = frame_file_stem(group_frame);
        assert!(!old_measured[&id].exists(), "frame {id}: the old pass-1 file is gone");
        assert!(!old_fits[&id].exists(), "frame {id}: the old fits file is gone");
        let measured = rig.rows(ArtifactKind::Measured)[&id].path.clone().unwrap();
        assert_eq!(PathBuf::from(&measured), rig.layout.measured_path(&stem));
        assert!(Path::new(&measured).is_file());
        let fits = rig.rows(ArtifactKind::Fits(0))[&id].path.clone().unwrap();
        assert_eq!(PathBuf::from(&fits), rig.layout.fits_path(&stem, 0));
        assert!(Path::new(&fits).is_file());
    }
}

/// Plan A Task 11c, bar 3: a first pass is alive only while its worker holds it, so the peak over a fresh run
/// and over a group-β-move run is the admitted workers (one here), never the group (four).
#[test]
fn measure_pass1_residency_is_the_admitted_workers() {
    let rig = Pass1Rig::new();
    let calibrated_dir = rig.layout.calibrated_dir();
    rig.run("run 1", None);
    assert_eq!(
        crate::stacking::measure::pass1_peak(&calibrated_dir),
        1,
        "a fresh run holds one frame's first pass at a time"
    );
    let ids = rig.sorted_ids();
    assert_eq!(ids.len(), 4);
    rig.lower_own_betas(&[(ids[0], 1.6), (ids[1], 1.7)]);
    assert_eq!(rig.run("run 2", None).beta, Some(1.7), "the group beta moved");
    assert_eq!(
        crate::stacking::measure::pass1_peak(&calibrated_dir),
        1,
        "a group-beta move holds one frame's first pass at a time"
    );
}

/// Plan A Task 11c: a run cancelled after pass 1 (the `metrics` and `fits.0` rows gone, the first pass on disk)
/// resumes from the artifact: no pass-1 work, the same group β to the bit, the four rows back.
#[test]
fn measure_pass1_from_the_artifact_after_a_lost_pass_two() {
    let rig = Pass1Rig::new();
    let first = rig.run("run 1", None);
    assert_eq!(first.logged.len(), 4);
    rig.delete_rows(&[ArtifactKind::Metrics, ArtifactKind::Fits(0)]);
    assert!(rig.rows(ArtifactKind::Metrics).is_empty());
    let second = rig.run("run 2", None);
    assert!(
        second.logged.is_empty(),
        "pass 1 re-ran for {:?}",
        second.logged
    );
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
    assert_eq!(
        second.beta.map(f64::to_bits),
        first.beta.map(f64::to_bits),
        "the group beta from the artifacts' own betas is the one the pixels gave"
    );
}

/// Plan A Task 11c (R11c-4), amended by the frame-level cache (spec §5.3) and the file identity (FI-2): a calibrated
/// file whose mtime moved since the artifact was written does not cost the first pass: validity follows the
/// calibration hash (an own file is judged by key + size), not the mtime. Nothing is excluded.
#[test]
fn measure_pass1_artifact_survives_a_touched_source() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let ids = rig.sorted_ids();
    let touched = rig.calibrated_path(ids[2]);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&touched)
        .unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
        .unwrap();
    drop(file);
    rig.delete_rows(&[ArtifactKind::Fits(0)]);
    let second = rig.run("run 2", None);
    assert!(second.logged.is_empty(), "pass 1 re-ran for {:?}", second.logged);
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
}

/// The write order of a first pass (file-identity §4.2): the `measured` row of a member left owed is deleted BEFORE
/// pass 1 replaces its file. Run B (another first-pass key) is cancelled after its first first pass, so the files
/// of the frames it never reached are untouched, but a frame it did reach was rewritten: no row of run A's key is
/// left anywhere, every row is B's or absent.
#[test]
fn a_cancelled_first_pass_leaves_no_row_of_the_old_key() {
    let mut rig = Pass1Rig::new();
    rig.run("run A", None);
    let rows_a = rig.rows(ArtifactKind::Measured);
    assert_eq!(rows_a.len(), 4, "premise: run A wrote every first pass");
    let keys_a: Vec<String> = rows_a.values().map(|r| r.config_hash.clone()).collect();

    rig.cfg.measurement.max_stars += 1;
    let res = rig.try_run(None, Some(1));
    assert!(matches!(res, Err(RunError::Cancelled)), "{:?}", res.map(|o| o.beta));
    let rows_b = rig.rows(ArtifactKind::Measured);
    for (frame_id, row) in &rows_b {
        assert!(
            !keys_a.contains(&row.config_hash),
            "frame {frame_id}: a row of run A's key is left over a file run B may have rewritten"
        );
    }
    assert!(
        rows_b.len() < rows_a.len(),
        "premise: the frames run B never reached lost their row: {} rows",
        rows_b.len()
    );
}

/// The write order of the `.athf` (file-identity §4.2, ruling C-30 as amended): a failed write leaves no row for
/// that plane (the delete came first), where it used to leave the old run's row in place. Run 2 is at another
/// measurement hash; one frame's `.athf` path is a directory, so its write fails and only that frame loses its row.
#[test]
fn a_failed_fits_write_leaves_no_row() {
    let mut rig = Pass1Rig::new();
    rig.run("run 1", None);
    let rows = rig.rows(ArtifactKind::Fits(0));
    assert_eq!(rows.len(), 4, "premise: run 1 wrote every .athf");
    let victim_id = *rows.keys().min().unwrap();
    let victim_path = PathBuf::from(rows[&victim_id].path.clone().unwrap());
    std::fs::remove_file(&victim_path).unwrap();
    std::fs::create_dir(&victim_path).unwrap();

    let failures_before = FITS_WRITE_FAILURE_LOG.lock().unwrap().len();
    rig.cfg.measurement.max_stars += 1;
    rig.run("run 2", None);
    let failures: Vec<PathBuf> = FITS_WRITE_FAILURE_LOG.lock().unwrap()[failures_before..]
        .iter()
        .filter(|p| p.starts_with(victim_path.parent().unwrap()))
        .cloned()
        .collect();
    assert_eq!(failures, vec![victim_path.clone()], "the write of the planted collision failed");

    let rows_after = rig.rows(ArtifactKind::Fits(0));
    assert!(!rows_after.contains_key(&victim_id), "the failed write left a row: {rows_after:?}");
    assert_eq!(rows_after.len(), 3, "the other frames wrote theirs: {rows_after:?}");
    for (frame_id, row) in &rows_after {
        assert_ne!(
            row.config_hash, rows[frame_id].config_hash,
            "premise: frame {frame_id} was re-measured at the new hash"
        );
    }
}

/// A same-size pixel edit by someone else of an in-place calibrated file (an attested light) at the middle of the file,
/// a new time (+5 s), `calibrated`'s file returned. Returns the edited path.
fn edit_in_place_light_same_size(rig: &Pass1Rig, id: i64) -> PathBuf {
    let edited = rig.calibrated_path(id);
    assert!(
        !edited.starts_with(rig.layout.calibrated_root()) && !edited.starts_with(rig.layout.prepared_root()),
        "{edited:?} is used in place"
    );
    let before = std::fs::metadata(&edited).unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        // One pixel inside the data unit, same size: 1.0f32 big endian.
        let mut file = std::fs::OpenOptions::new().write(true).open(&edited).unwrap();
        file.seek(SeekFrom::Start(before.len() / 2)).unwrap();
        file.write_all(&1.0f32.to_be_bytes()).unwrap();
        file.set_modified(before.modified().unwrap() + std::time::Duration::from_secs(5))
            .unwrap();
    }
    let after = std::fs::metadata(&edited).unwrap();
    assert_eq!(after.len(), before.len(), "a same-size edit");
    assert_ne!(after.modified().unwrap(), before.modified().unwrap());
    edited
}

/// Ruling R12 (a) as the file identity reads it: a file Athenaeum never rewrites (an attested light used in place) is
/// judged by the key, and the key holds the light's sample. A same-size pixel edit by someone else (a new mtime, the
/// catalog row refreshed and its `content_hash` NULLed as a rescan's re-parse does) makes THAT frame's first pass
/// stale; it re-runs, the others do not, nothing is excluded. Without the `content_hash = NULL` the banked sample
/// would be trusted: the row's size and time match the file.
#[test]
fn measure_pass1_artifact_of_an_edited_in_place_source_reruns_pass_one() {
    let rig = Pass1Rig::attested();
    let first = rig.run("run 1", None);
    assert_eq!(first.logged.len(), 4);
    let ids = rig.sorted_ids();
    assert_eq!(ids.len(), 4);
    let edited = edit_in_place_light_same_size(&rig, ids[2]);
    let (size, modified_at) = file_identity(&edited).unwrap();
    rig.fixture
        .conn
        .execute(
            "UPDATE files SET size = ?1, modified_at = ?2, content_hash = NULL WHERE path = ?3",
            rusqlite::params![size, modified_at, edited.to_str().unwrap()],
        )
        .unwrap();
    rig.delete_rows(&[ArtifactKind::Metrics, ArtifactKind::Fits(0)]);
    let second = rig.run("run 2", None);
    assert_eq!(second.logged, vec![edited], "only the edited file re-ran pass 1");
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
}

/// The same edit with the catalog row untouched (no rescan): the row's time no longer matches the file's, so the run
/// samples the file again, finds the edited bytes, and only that frame's first pass re-runs.
#[test]
fn measure_pass1_artifact_of_an_edited_in_place_source_without_a_rescan_reruns_pass_one() {
    let rig = Pass1Rig::attested();
    let first = rig.run("run 1", None);
    assert_eq!(first.logged.len(), 4);
    let ids = rig.sorted_ids();
    let edited = edit_in_place_light_same_size(&rig, ids[2]);
    rig.delete_rows(&[ArtifactKind::Metrics, ArtifactKind::Fits(0)]);
    let second = rig.run("run 2", None);
    assert_eq!(second.logged, vec![edited], "only the edited file re-ran pass 1");
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
}

/// Plan A Task 11c (R11c-4), amended (spec §5.3): a calibrated file of another SIZE than the one pass 1 read, whose
/// row says so too (the calibrate stage reuses it), makes THAT frame's first pass stale; it re-runs, the others do
/// not, nothing is excluded.
#[test]
fn measure_pass1_artifact_of_a_source_of_another_size_reruns_pass_one() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let ids = rig.sorted_ids();
    let resized = rig.calibrated_path(ids[2]);
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&resized).unwrap();
        file.write_all(&[0u8; 2880]).unwrap();
    }
    let row = crate::db::stacking::find_artifact(
        &rig.fixture.conn,
        &rig.subject,
        "",
        ArtifactKind::Calibrated,
        Some(ids[2]),
    )
    .unwrap()
    .unwrap();
    let (size, modified_at) = file_identity(&resized).unwrap();
    upsert_artifact(
        &rig.fixture.conn,
        &NewArtifact {
            subject: &rig.subject,
            frame_id: Some(ids[2]),
            group_key: "",
            kind: ArtifactKind::Calibrated,
            path: row.path.as_deref(),
            config_hash: &row.config_hash,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: row.payload_json.as_deref(),
        },
    )
    .unwrap();
    rig.delete_rows(&[ArtifactKind::Fits(0)]);
    let second = rig.run("run 2", None);
    assert_eq!(second.logged, vec![resized], "only the resized file re-ran pass 1");
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
}

/// Frame-level cache spec §5.3 (Task 8): "Delete intermediates" removes `calibrated/` and keeps `measured/`. The
/// next run recalibrates every frame (a new file, a new mtime, the same bytes) and, because it needs pass 2 again
/// (here: the `metrics` and `fits.0` rows of the measurement are gone), reads the stored first pass: the calibration
/// hash in the artifact's key is its validity, not the calibrated file's mtime. No `rerun_from`: that would force
/// Measure fresh.
#[test]
fn pass1_survives_a_byte_identical_recalibration() {
    let rig = Pass1Rig::new();
    let (first, logs1) = crate::stacking::telemetry::capture_logs(|| rig.run("run 1", None));
    assert_eq!(fan_out_items(&logs1, "measure", "pass1"), 4, "{logs1}");
    let calibrated_before: Vec<PathBuf> = rig
        .sorted_ids()
        .into_iter()
        .map(|id| rig.calibrated_path(id))
        .collect();
    let measured_before: Vec<(i64, String, std::time::SystemTime)> = rig
        .rows(ArtifactKind::Measured)
        .into_iter()
        .map(|(id, row)| {
            let path = row.path.unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            (id, path, modified)
        })
        .collect();
    assert_eq!(measured_before.len(), 4);

    cleanup_work(
        &rig.fixture.conn,
        &rig.subject,
        &rig.layout,
        crate::stacking::paths::CleanupWhat::Intermediates,
    )
    .unwrap();
    assert!(calibrated_before.iter().all(|p| !p.exists()), "calibrated/ went");
    assert!(
        measured_before.iter().all(|(_, p, _)| Path::new(p).is_file()),
        "measured/ stayed"
    );
    rig.delete_rows(&[ArtifactKind::Metrics, ArtifactKind::Fits(0)]);

    let (second, logs2) = crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    assert!(
        calibrated_before.iter().all(|p| p.is_file()),
        "run 2 recalibrated every frame"
    );
    assert_eq!(fan_out_items(&logs2, "measure", "pass1"), 0, "pass 1 re-ran: {logs2}");
    assert_eq!(fan_out_items(&logs2, "measure", "pass2"), 4, "{logs2}");
    assert!(second.logged.is_empty(), "{:?}", second.logged);
    assert_eq!(second.excluded, 0);
    assert_eq!(
        second.beta.map(f64::to_bits),
        first.beta.map(f64::to_bits),
        "the group beta from the stored first passes is the one the pixels gave"
    );
    for (id, path, modified) in &measured_before {
        assert_eq!(
            std::fs::metadata(path).unwrap().modified().unwrap(),
            *modified,
            "frame {id}: the first-pass file was rewritten"
        );
    }
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
}

/// Ruling R10 M3: a first pass re-run inside a pass-2 worker (its stored artifact fails the whole-file checksum
/// after the head check passed) is written there; when that member's pass 2 then fails (here the calibrated file
/// is touched right after the first pass, so the read guard refuses the second), the controller still records the
/// artifact's row, as phase 1 does. Without it the rewritten file carries a new mtime the old row does not know,
/// `plan::is_fresh` refuses it and the frame re-runs pass 1 on every run while its pass 2 keeps failing.
#[test]
fn measure_pass1_rewritten_inside_a_failing_pass_two_records_its_row() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    for row in rig.rows(ArtifactKind::Measured).into_values() {
        let artifact = PathBuf::from(row.path.unwrap());
        let mut bytes = std::fs::read(&artifact).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x5a;
        // The row still describes the file (same size, same mtime), so the controller's head check passes.
        let modified = std::fs::metadata(&artifact).unwrap().modified().unwrap();
        std::fs::write(&artifact, &bytes).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&artifact)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }
    rig.delete_rows(&[ArtifactKind::Metrics, ArtifactKind::Fits(0)]);

    crate::stacking::measure::PASS1_TOUCH_AFTER
        .lock()
        .unwrap()
        .push((rig.working.path().to_path_buf(), 1));
    let (second, logs2) = crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    crate::stacking::measure::PASS1_TOUCH_AFTER
        .lock()
        .unwrap()
        .retain(|(dir, _)| dir != rig.working.path());
    assert_eq!(second.logged.len(), 4, "every artifact was refused after its head passed");
    assert_eq!(second.excluded, 1, "the touched frame's pass 2 was refused: {logs2}");
    let rows = rig.rows(ArtifactKind::Measured);
    assert_eq!(rows.len(), 4);
    for (id, row) in &rows {
        assert!(
            crate::stacking::plan::file_matches_identity(
                Path::new(row.path.as_deref().unwrap()),
                row.size
            ),
            "frame {id}: the rewritten artifact's row describes another file"
        );
    }
    let third = rig.run("run 3", None);
    assert!(
        third.logged.is_empty(),
        "pass 1 re-ran for {:?}",
        third.logged
    );
}

/// Plan A Task 11c (R11c-2): an artifact that fails its checksum is recomputed (a `warn!`), never decoded and
/// never an exclusion.
#[test]
fn measure_pass1_corrupt_artifact_is_recomputed() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let ids = rig.sorted_ids();
    let row = rig
        .rows(ArtifactKind::Measured)
        .remove(&ids[1])
        .expect("run 1 wrote a measured row for every member");
    let artifact = PathBuf::from(row.path.unwrap());
    let mut bytes = std::fs::read(&artifact).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x5a;
    std::fs::write(&artifact, &bytes).unwrap();
    rig.delete_rows(&[ArtifactKind::Fits(0)]);
    let second = rig.run("run 2", None);
    assert_eq!(
        second.logged,
        vec![rig.calibrated_path(ids[1])],
        "only the corrupt artifact's frame re-ran pass 1"
    );
    assert_eq!(second.excluded, 0);
}

/// Plan A Task 11c review I2 (R11c-5 (1), bar 5): a run cancelled during phase 1, after at least one frame's pass 1
/// finished and its artifact was written, leaves that frame's `measured` row; the next run reuses it (no pass 1
/// for that frame) and the others run pass 1.
#[test]
fn measure_pass1_cancelled_run_keeps_the_rows_of_the_files_it_wrote() {
    let rig = Pass1Rig::new();
    let cancelled = rig.try_run(None, Some(2));
    assert!(
        matches!(cancelled, Err(RunError::Cancelled)),
        "the run honoured the cancel"
    );
    let written: Vec<PathBuf> = rig
        .rows(ArtifactKind::Measured)
        .values()
        .map(|row| PathBuf::from(row.path.clone().unwrap()))
        .collect();
    assert!(
        !written.is_empty() && written.iter().all(|p| p.is_file()),
        "the cancelled run left rows for files that exist"
    );
    let kept = rig.rows(ArtifactKind::Measured).len();
    assert!(kept < 4, "the cancel landed mid-phase-1 ({kept} of 4 written)");
    let second = rig.run("run 2", None);
    assert_eq!(
        second.logged.len(),
        4 - kept,
        "the next run runs pass 1 only for the frames the cancelled run did not finish"
    );
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
}

/// Plan A Task 11c review M1 (R11c-5 (3)): a member whose own beta is on its `metrics` row is a cache hit and its
/// artifact is never opened — a corrupt one raises no refusal `warn!` on a run that needs no pass 2.
#[test]
fn measure_pass1_cache_hit_member_does_not_validate_its_artifact() {
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let ids = rig.sorted_ids();
    let artifact = PathBuf::from(rig.rows(ArtifactKind::Measured)[&ids[1]].path.clone().unwrap());
    let mut bytes = std::fs::read(&artifact).unwrap();
    // A HEAD byte: a head read (step 0's validation) refuses it, so the test fails if a cache hit validates.
    bytes[crate::stacking::pass1_artifact::PREFIX_LEN] ^= 0x5a;
    std::fs::write(&artifact, &bytes).unwrap();
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::WARN)
        .finish();
    let second = tracing::subscriber::with_default(subscriber, || rig.run("run 2", None));
    assert!(second.logged.is_empty(), "a cache hit runs no pass 1");
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        !text.contains("first-pass artifact"),
        "no refusal logged for a cache-hit member: {text}"
    );
}

/// Plan A Task 11c (R11c-5): a spill that cannot be written (here a file where its directory belongs) is a
/// `warn!`, no `measured` row, the records dropped and pass 1 re-run in that member's pass-2 worker — twice, never
/// held, so residency stays the admitted workers' even on a full disk.
#[test]
fn measure_pass1_write_failure_recomputes_in_pass_two() {
    let rig = Pass1Rig::new();
    // The spill folder is flat now (frame-level cache): a file stands where `measured/` belongs.
    std::fs::create_dir_all(rig.measured_root().parent().unwrap()).unwrap();
    std::fs::write(rig.measured_root(), b"not a directory").unwrap();
    let out = rig.run("run 1", None);
    assert!(rig.rows(ArtifactKind::Measured).is_empty(), "no row for a file that is not there");
    assert_eq!(rig.rows(ArtifactKind::Metrics).len(), 4);
    assert_eq!(out.excluded, 0);
    assert_eq!(out.logged.len(), 8, "pass 1 ran twice per member");
    assert_eq!(
        crate::stacking::measure::pass1_peak(&rig.layout.calibrated_dir()),
        1
    );
}

/// Plan A Task 11c (R11c-5): `rerun_from <= Measure` skips the artifact as it skips the `metrics` row: pass 1
/// runs for every member and the artifact is overwritten in place (rows present, one per member).
#[test]
fn measure_pass1_rerun_from_measure_recomputes_it() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let second = rig.run("run 2", Some(Stage::Measure));
    assert_eq!(second.logged.len(), 4, "a forced Measure re-runs every first pass");
    assert_eq!(rig.rows(ArtifactKind::Measured).len(), 4);
}

/// Plan A Task 11c (upgrade path): a member with a `metrics` row but no artifact (a catalog from before the
/// spill) runs pass 1 inside pass 2 ONCE and leaves the artifact; the next group-beta move reads it.
#[test]
fn measure_pass1_upgrade_runs_it_inside_pass_two_once() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    rig.delete_rows(&[ArtifactKind::Measured]);
    let _ = std::fs::remove_dir_all(rig.measured_root());
    let ids = rig.sorted_ids();
    rig.lower_own_betas(&[(ids[0], 1.6), (ids[1], 1.7)]);
    let second = rig.run("run 2", None);
    assert_eq!(second.beta, Some(1.7));
    assert_eq!(second.logged.len(), 4, "the upgrade ripple: pass 1 inside pass 2, once");
    assert_eq!(rig.rows(ArtifactKind::Measured).len(), 4);
    rig.lower_own_betas(&[(ids[2], 1.2), (ids[3], 1.3)]);
    let third = rig.run("run 3", None);
    assert_eq!(third.beta, Some(1.3), "the group beta moved again");
    assert!(
        third.logged.is_empty(),
        "pass 1 re-ran for {:?}",
        third.logged
    );
}

/// Plan A Task 11c, bar 1 (a pin, ruling F28: green at HEAD of the spill and after): every `.athf` (bytes), every
/// `metrics` payload minus its wall-clock `durationMs`, the group β (f64 bits) and every `metrics` / `fits.0` key
/// of a run equal the in-memory two-pass measurement's (11b's: `measure_frame_pass1` then `measure_frame_pass2` at
/// the run's group β) — fresh, and again from the artifact after the `fits.0` rows are gone.
#[test]
fn measure_pass1_spill_is_byte_identical_fresh_and_from_the_artifact() {
    let rig = Pass1Rig::new();
    let opts = rig
        .cfg
        .measurement
        .measure_options(rig.cfg.normalization.scale_estimator);
    let strip = |mut v: serde_json::Value| {
        v.as_object_mut().unwrap().remove("durationMs");
        v
    };
    // Per frame: (metrics key, fits.0 key, payload minus its timing, the .athf bytes).
    let snapshot = || {
        let metrics = rig.rows(ArtifactKind::Metrics);
        let fits = rig.rows(ArtifactKind::Fits(0));
        assert_eq!((metrics.len(), fits.len()), (4, 4));
        metrics
            .into_iter()
            .map(|(frame_id, row)| {
                let fits_row = &fits[&frame_id];
                let payload =
                    serde_json::from_str(row.payload_json.as_deref().unwrap()).unwrap();
                (
                    frame_id,
                    (
                        row.config_hash.clone(),
                        fits_row.config_hash.clone(),
                        strip(payload),
                        std::fs::read(fits_row.path.as_deref().unwrap()).unwrap(),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };

    let fresh = rig.run("fresh", None);
    let beta = fresh.beta.expect("a group beta");
    let from_fresh = snapshot();
    for (frame_id, (_, _, payload, athf)) in &from_fresh {
        let path = rig.calibrated_path(*frame_id);
        let cancel = AtomicBool::new(false);
        let pass1 = measure_frame_pass1(&path, &opts, None, &cancel).unwrap();
        let betas = vec![beta; pass1.beta_own().len()];
        let (m, fits) =
            measure_frame_pass2(&path, pass1, &betas, &opts, None, &cancel).unwrap();
        assert_eq!(
            payload,
            &strip(serde_json::to_value(&m).unwrap()),
            "frame {frame_id}: payload"
        );
        let reference = rig.working.path().join(format!("reference-{frame_id}.athf"));
        fits_artifact::write_fits(&reference, &fits[0]).unwrap();
        assert_eq!(
            athf,
            &std::fs::read(&reference).unwrap(),
            "frame {frame_id}: the .athf bytes"
        );
    }

    rig.delete_rows(&[ArtifactKind::Fits(0)]);
    let again = rig.run("from the artifact", None);
    assert!(again.logged.is_empty(), "pass 1 re-ran for {:?}", again.logged);
    assert_eq!(
        again.beta.map(f64::to_bits),
        Some(beta.to_bits()),
        "the group beta, to the bit"
    );
    assert_eq!(
        snapshot(),
        from_fresh,
        "keys, payloads and .athf bytes from the artifact are the fresh run's"
    );
}

/// Plan A Task 11b: `Moffat4` keeps the single pass, byte-identical to before the task — every row keyed on the
/// measurement hash without a β, its payload the single-pass measurement exactly (no `betaOwn` key), every fit at 4.
#[test]
fn moffat4_measure_is_one_pass_and_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0)];
    let noise = [5.0f32; 3];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    let mut cfg = StackingConfig::default();
    cfg.measurement.psf_model = psf_signal::PsfModel::Moffat4;
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
        cfg.clone(),
        plan_groups.clone(),
        WorkingLayout::new(working.path(), &set_slug(SET_NAME)),
        output.path().to_path_buf(),
        group_ids,
    );
    run_stages_for_test(&mut rc, Stage::Measure).unwrap();
    let subject = StackSubject::FrameSet(fixture.set_id);
    let opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);
    let rows =
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some(ArtifactKind::Metrics)).unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        let frame_id = row.frame_id.unwrap();
        let calibrated = crate::db::stacking::find_artifact(
            &fixture.conn,
            &subject,
            "",
            ArtifactKind::Calibrated,
            Some(frame_id),
        )
        .unwrap()
        .unwrap();
        let before = crate::stacking::config::stage_hash(
            &crate::stacking::config::measurement_subtree(&cfg),
            &[calibrated.config_hash.as_str()],
            &[],
        );
        assert_eq!(row.config_hash, before, "frame {frame_id}: the pre-11b key");
        let payload = row.payload_json.unwrap();
        assert!(!payload.contains("betaOwn"), "frame {frame_id}: {payload}");
        let (single, fits) = measure_frame_with_fits(
            Path::new(calibrated.path.as_deref().unwrap()),
            &opts,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        let strip = |mut v: serde_json::Value| {
            v.as_object_mut().unwrap().remove("durationMs");
            v
        };
        assert_eq!(
            strip(serde_json::from_str(&payload).unwrap()),
            strip(serde_json::to_value(&single).unwrap()),
            "frame {frame_id}: the single pass, exactly"
        );
        assert!(fits[0].iter().all(|f| f.beta == 4.0));
    }
    assert_eq!(
        crate::db::stacking::list_groups(&fixture.conn, run_id).unwrap()[0].beta,
        Some(4.0)
    );
    // Plan A Task 11c: the single pass spills nothing.
    assert!(
        rows_of_kind(&fixture.conn, fixture.set_id, ArtifactKind::Measured).is_empty(),
        "no measured row under Moffat4"
    );
    assert!(
        !WorkingLayout::new(working.path(), &set_slug(SET_NAME))
            .measured_root()
            .exists(),
        "no measured/ under Moffat4"
    );
}

/// Plan A Task 11b: a group of one is fitted at its own β — its row's `beta` is its `betaOwn`, which is the group
/// row's β (the run itself stops after Measure: one frame is no stackable group).
#[test]
fn a_group_of_one_is_fitted_at_its_own_beta() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &[(0.0, 0.0)], &[5.0]);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
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
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        WorkingLayout::new(working.path(), &set_slug(SET_NAME)),
        output.path().to_path_buf(),
        group_ids,
    );
    assert!(
        run_stages_for_test(&mut rc, Stage::Measure).is_err(),
        "one frame is no stackable group"
    );
    let rows = metrics_by_frame(&fixture.conn, fixture.set_id);
    assert_eq!(rows.len(), 1);
    let (_, m, _) = rows.values().next().unwrap();
    let own = m.channels[0].beta_own.expect("the own beta");
    assert_eq!(m.channels[0].beta, own);
    assert_eq!(
        crate::db::stacking::list_groups(&fixture.conn, run_id).unwrap()[0].beta,
        Some(own)
    );
}

/// Perf tier C Task 1: stage 3 writes one `fits.0` artifact per frame
/// (this fixture's mono field, one plane) alongside `metrics`, each
/// readable back with `read_fits` and matching that frame's own
/// `stars_fitted`; the group row's `beta` is `psf_signal::group_beta`
/// of the members' own `Auto` picks; a second run reads both as
/// cached; deleting one frame's `.athf` file (not its DB row) makes
/// the plan gate report Measure stale for that frame, even though its
/// `metrics` row is untouched.
///
/// Plan A Task 11b (owner decision R11b-12): the members' own picks are
/// now `betaOwn`, and every member's `.athf` β == its `metrics` β ==
/// `stacking_run_groups.beta` == the β stage 6 fits its reference at
/// (`RunContext::measured_groups`, mapped `Auto` → `Fixed`).
#[test]
fn measure_writes_fits_artifacts_and_the_group_beta() {
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

    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");

    let build = |run_id: i64, group_ids: HashMap<String, i64>| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output_dir.clone(),
            group_ids,
        )
    };

    let (run_id1, group_ids1) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc1 = build(run_id1, group_ids1);
    run_stages_for_test(&mut rc1, Stage::Measure).unwrap();

    let metrics = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Metrics),
    )
    .unwrap();
    assert_eq!(metrics.len(), 4, "{metrics:?}");
    let fits_rows = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Fits(0)),
    )
    .unwrap();
    assert_eq!(
        fits_rows.len(),
        4,
        "one fits.0 row per frame (mono, one plane): {fits_rows:?}"
    );

    let mut betas: Vec<f64> = Vec::new();
    let mut fitted_at: Vec<(f64, i64)> = Vec::new();
    for metrics_row in &metrics {
        let frame_id = metrics_row.frame_id.expect("metrics row has a frame_id");
        let m: FrameMeasurement =
            serde_json::from_str(metrics_row.payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(m.channels.len(), 1, "mono fixture: one channel");
        betas.push(
            m.channels[0]
                .beta_own
                .expect("the two-pass Measure records the own beta"),
        );
        fitted_at.push((m.channels[0].beta, frame_id));

        let fits_row = fits_rows
            .iter()
            .find(|a| a.frame_id == Some(frame_id))
            .unwrap_or_else(|| panic!("no fits.0 row for frame {frame_id}"));
        assert_eq!(fits_row.config_hash, metrics_row.config_hash);
        let fits =
            fits_artifact::read_fits(Path::new(fits_row.path.as_deref().unwrap())).unwrap();
        assert_eq!(
            fits.len(),
            m.channels[0].stars_fitted,
            "read_fits must return exactly the frame's stars_fitted count"
        );
        // `.athf` stores every number as an `f32`.
        assert!(
            fits.iter()
                .all(|f| f.beta as f32 == m.channels[0].beta as f32),
            "every .athf fit is at the frame's fitted beta"
        );
    }

    let expected_beta = psf_signal::group_beta(&betas);
    for (beta, frame_id) in &fitted_at {
        assert_eq!(
            *beta, expected_beta,
            "frame {frame_id} is fitted at the group beta"
        );
    }
    assert_eq!(
        rc1.measured_groups
            .get(&plan_groups[0].key)
            .and_then(|g| g.beta),
        Some(expected_beta),
        "stage 6 fits its reference at the same group beta"
    );
    let groups_after = crate::db::stacking::list_groups(&fixture.conn, run_id1).unwrap();
    assert_eq!(groups_after.len(), 1);
    assert_eq!(
        groups_after[0].beta,
        Some(expected_beta),
        "the group row's beta must be group_beta() of the members' own Auto picks"
    );

    // A second run: everything reads as cached, `fits_cached` included.
    let plan = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
    )
    .unwrap();
    assert!(
        !plan.stale_stages.contains(&Stage::Measure),
        "{:?}",
        plan.stale_stages
    );
    assert_eq!(plan.groups[0].fits_cached, plan.groups[0].frame_count);

    let (run_id2, group_ids2) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc2 = build(run_id2, group_ids2);
    run_stages_for_test(&mut rc2, Stage::Measure).unwrap();
    let fits_rows2 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Fits(0)),
    )
    .unwrap();
    assert_eq!(
        fits_rows2.iter().map(|a| &a.created_at).collect::<Vec<_>>(),
        fits_rows.iter().map(|a| &a.created_at).collect::<Vec<_>>(),
        "a fresh fits artifact must not be recreated"
    );

    // Delete one frame's `.athf` FILE (not its DB row): `metrics` for
    // that frame is untouched, but Measure must still read as stale for
    // it — the pair, not either row alone, decides freshness.
    let victim = fits_rows[0].path.as_deref().unwrap();
    std::fs::remove_file(victim).unwrap();
    let plan_after_delete = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
    )
    .unwrap();
    assert!(
        plan_after_delete.stale_stages.contains(&Stage::Measure),
        "a fits file removed by hand must make Measure stale even though metrics survives: {:?}",
        plan_after_delete.stale_stages
    );
    assert_eq!(
        plan_after_delete.groups[0].fits_cached, 3,
        "only the untouched three frames still read fits-fresh"
    );
    assert_eq!(
        plan_after_delete.groups[0].metrics_cached, 4,
        "metrics itself is untouched by the fits deletion"
    );
}

/// Tier C Task 1 fix round 1: a PLAIN run (`rerun_from: None`) must heal
/// a missing `fits` file on its own — before this fix `stage_measure`'s
/// own reuse decision checked only the `metrics` hash, so it kept
/// reusing the stale metrics forever and only "Re-run from Measure"
/// would notice. Two full `start_stacking` runs on the real pipeline
/// (`CleanupPolicy::KeepAll` so nothing sweeps `fits`/`metrics` between
/// them): the first measures every frame; one frame's `.athf` file is
/// then deleted (its DB row and its `metrics` row are untouched); a
/// plain second run re-measures ONLY that frame (its `metrics` row is
/// rewritten — `created_at` moves — while the other three are byte-for-
/// byte the same row) and its `fits` files come back, and `build_plan`
/// reads Measure as fresh again afterwards.
#[test]
fn measure_heals_a_missing_fits_sidecar_on_a_plain_rerun() {
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started1 = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
        None,
    )
    .expect("first run should start");
    wait_for_run(&ctx, started1.run_id);
    let row1 = crate::db::stacking::get_run(&fixture.conn, started1.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row1.status, "done", "{row1:?}");

    let metrics_before = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Metrics),
    )
    .unwrap();
    assert_eq!(metrics_before.len(), 4, "{metrics_before:?}");
    let fits_before = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Fits(0)),
    )
    .unwrap();
    assert_eq!(fits_before.len(), 4, "{fits_before:?}");
    let created_before: HashMap<i64, String> = metrics_before
        .iter()
        .map(|a| (a.frame_id.unwrap(), a.created_at.clone()))
        .collect();

    // Delete one frame's persisted `.athf` FILE — its DB rows (metrics
    // AND the fits row itself) are untouched.
    let victim_id = fits_before[0].frame_id.unwrap();
    let victim_path = fits_before[0].path.clone().unwrap();
    std::fs::remove_file(&victim_path).unwrap();

    let plan_before = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
    )
    .expect("the plan should build");
    assert!(
        plan_before.stale_stages.contains(&Stage::Measure),
        "{:?}",
        plan_before.stale_stages
    );
    assert_eq!(
        plan_before.groups[0].fits_cached, 3,
        "only the three untouched frames read fits-fresh: {:?}",
        plan_before.groups[0]
    );
    assert_eq!(
        plan_before.groups[0].metrics_cached, 4,
        "metrics itself is untouched by the fits deletion: {:?}",
        plan_before.groups[0]
    );

    // A PLAIN second run — no `rerun_from` — must heal it on its own.
    let started2 = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
        None,
    )
    .expect("second run should start");
    wait_for_run(&ctx, started2.run_id);
    let row2 = crate::db::stacking::get_run(&fixture.conn, started2.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row2.status, "done", "{row2:?}");

    let metrics_after = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Metrics),
    )
    .unwrap();
    assert_eq!(metrics_after.len(), 4, "{metrics_after:?}");
    for a in &metrics_after {
        let fid = a.frame_id.unwrap();
        let before = created_before.get(&fid).unwrap();
        if fid == victim_id {
            assert_ne!(
                &a.created_at, before,
                "the victim frame's metrics row must have been rewritten"
            );
        } else {
            assert_eq!(
                &a.created_at, before,
                "an untouched frame's metrics row must not be recreated"
            );
        }
    }

    let fits_after = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Fits(0)),
    )
    .unwrap();
    assert_eq!(fits_after.len(), 4, "{fits_after:?}");
    assert!(
        Path::new(&victim_path).exists(),
        "the deleted fits file must have been rewritten"
    );

    let plan_after = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
    )
    .expect("the plan should build");
    assert!(
        !plan_after.stale_stages.contains(&Stage::Measure),
        "a plain run must heal the missing fits file on its own: {:?}",
        plan_after.stale_stages
    );
    assert_eq!(
        plan_after.groups[0].fits_cached, 4,
        "{:?}",
        plan_after.groups[0]
    );
    assert_eq!(
        plan_after.groups[0].metrics_cached, 4,
        "{:?}",
        plan_after.groups[0]
    );
}

/// Final fix wave (ruling C-30, the whole-branch review's I1): a `.athf`
/// write that CANNOT succeed degrades — the run completes, the frame
/// keeps its `metrics` row, and Register falls back to detection for it
/// (`star_source = "detected"`), exactly the way a missing or stale
/// artifact already behaved. Before the fix the same failure mapped to
/// `RunError::Other` and killed stage 3, throwing away the whole run
/// over a cache file every consumer already knows how to live without.
///
/// The unwritable path is a DIRECTORY planted at one frame's own
/// `<stem>.p0.athf` (a path collision `write_fits`'s tmp+rename cannot
/// resolve: `create_dir_all(parent)` and the tmp write both succeed,
/// the final rename onto a directory does not) — one frame, so the
/// degrade fires exactly ONCE and its three siblings prove the run is
/// otherwise untouched. Blocking the whole `fits/` directory
/// instead would fail all four writes and could not tell a per-plane
/// `continue` apart from a stage that gave up.
#[test]
fn a_fits_artifact_write_failure_degrades_to_detection() {
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started1 = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
        None,
    )
    .expect("first run should start");
    wait_for_run(&ctx, started1.run_id);
    assert_eq!(
        crate::db::stacking::get_run(&fixture.conn, started1.run_id)
            .unwrap()
            .unwrap()
            .status,
        "done"
    );

    let fits_before = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Fits(0)),
    )
    .unwrap();
    assert_eq!(fits_before.len(), 4, "{fits_before:?}");

    // Plant the collision: the file goes, a directory takes its name.
    let victim_id = fits_before[0].frame_id.unwrap();
    let victim_path = PathBuf::from(fits_before[0].path.clone().unwrap());
    std::fs::remove_file(&victim_path).unwrap();
    std::fs::create_dir(&victim_path).unwrap();

    let failures_before = FITS_WRITE_FAILURE_LOG.lock().unwrap().len();

    let started2 = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
        None,
    )
    .expect("second run should start");
    wait_for_run(&ctx, started2.run_id);
    let row2 = crate::db::stacking::get_run(&fixture.conn, started2.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        row2.status, "done",
        "an unwritable fits artifact must not fail the run: {row2:?}"
    );

    // Exactly one degrade, and it is this frame's plane. Filtered to
    // this test's own tree for the reason `FITS_WRITE_FAILURE_LOG`'s
    // own doc gives — sibling tests measure frames concurrently under
    // `cargo test`'s default parallelism. The prefix is the victim's
    // own `fits/` directory as the DB spells it, NOT
    // `working.path()`: on macOS the tempdir hands out `/var/folders/…`
    // while everything the run writes carries the canonical
    // `/private/var/folders/…`, so a `working.path()` prefix matches
    // nothing at all.
    let group_fits_dir = victim_path.parent().unwrap().to_path_buf();
    let failures: Vec<PathBuf> = {
        let log = FITS_WRITE_FAILURE_LOG.lock().unwrap();
        log[failures_before..]
            .iter()
            .filter(|p| p.starts_with(&group_fits_dir))
            .cloned()
            .collect()
    };
    assert_eq!(
        failures,
        vec![victim_path.clone()],
        "exactly one write degraded, and it is the planted collision"
    );

    // The group still produced its master — the run did the work.
    let groups = crate::db::stacking::list_groups(&fixture.conn, started2.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].status, "done", "{:?}", groups[0]);
    assert!(groups[0].master_path.is_some(), "{:?}", groups[0]);

    // The `metrics` row is KEPT — only the plane's `fits` row is
    // skipped, and only for the frame whose write failed.
    let metrics_after = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Metrics),
    )
    .unwrap();
    assert_eq!(metrics_after.len(), 4, "{metrics_after:?}");

    // No row was written for a file that is not on disk, and none is
    // left: the write order deletes a plane's row BEFORE it writes
    // (file-identity §4.2), so the victim has no `fits.0` row at all, which
    // is exactly the `resolve_register_fits` gate — a frame with no fresh
    // `fits.0` registers through `detect_frame_stars`'s detection arm,
    // `star_source = "detected"`. Its three siblings stay `"fits"`.
    let fits_after = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::Fits(0)),
    )
    .unwrap();
    assert_eq!(
        fits_after.len(),
        3,
        "the failed write leaves no row for the victim: {fits_after:?}"
    );
    assert!(
        fits_after.iter().all(|row| row.frame_id != Some(victim_id)),
        "{fits_after:?}"
    );
    let plan_after = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
    )
    .expect("the plan should build");
    assert_eq!(
        plan_after.groups[0].fits_cached, 3,
        "only the three untouched frames read fits-fresh: {:?}",
        plan_after.groups[0]
    );
    assert_eq!(
        plan_after.groups[0].metrics_cached, 4,
        "{:?}",
        plan_after.groups[0]
    );

    // The degraded frame is still a full member of the run: measured,
    // included, and registered (from detection).
    let registrations =
        crate::registration::db::get_registration_for_frame_set(&fixture.conn, fixture.set_id)
            .unwrap();
    assert!(
        registrations.iter().any(|r| r.frame_id == victim_id),
        "the degraded frame must still be registered: {registrations:?}"
    );
}

#[test]
fn selection_excludes_manual_and_low_weight() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);

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
    rc.excluded = vec![light_ids[1]];

    run_stages_for_test(&mut rc, Stage::Measure).unwrap();

    let group_key = rc.plan_groups[0].key.clone();
    let entries = rc.measured.get(&group_key).expect("group measured");
    let entry_of = |id: i64| entries.iter().find(|e| e.frame.frame_id == id).unwrap();

    assert!(
        !entry_of(light_ids[1]).included,
        "manually excluded frame must not be included"
    );
    assert_eq!(
        entry_of(light_ids[1]).reason.as_deref(),
        Some("excluded manually")
    );

    assert!(
        !entry_of(noise_id).included,
        "pure-noise frame must be excluded by weight"
    );
    let noise_reason = entry_of(noise_id).reason.clone().unwrap_or_default();
    assert!(noise_reason.contains("weight"), "{noise_reason}");

    for &id in &[light_ids[0], light_ids[2], light_ids[3]] {
        assert!(entry_of(id).included, "frame {id} should remain included");
    }
}

/// Task 11b review I1: pass 1 of the group measure ticks. The bar moves from the first frame: the first event
/// after the stage-start event is a pass-1 half-tick (1 of 8 units on 4 frames, 12.5 %), the percents never go backwards, and the stage
/// still lands on `total`.
#[test]
fn measure_pass_one_ticks_and_the_stage_lands_on_total() {
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
    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let recorder = Arc::new(Recording::new());
    let mut rc = test_context(
        ctx.clone(),
        recorder.clone() as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    run_stages_for_test(&mut rc, Stage::Measure).unwrap();

    let measure: Vec<serde_json::Value> = recorder
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["stage"] == "measure")
        .collect();
    assert!(measure.len() >= 2, "{measure:?}");
    let percents: Vec<f64> = measure
        .iter()
        .map(|e| e["percent"].as_f64().unwrap())
        .collect();
    assert!(
        percents
            .iter()
            .find(|&&p| p > 0.0)
            .is_some_and(|p| (p - 12.5).abs() < 1e-9),
        "the first tick after the stage-start event is a pass-1 half-tick: {percents:?}"
    );
    assert!(
        percents.windows(2).all(|w| w[0] <= w[1]),
        "progress never goes backwards: {percents:?}"
    );
    let last = measure.last().unwrap();
    assert_eq!(last["current"], last["total"], "{last}");
    assert_eq!(last["current"], 4, "{last}");
}

/// Task 13c L4 review I2, Measure: a later group's pass-1 queue reads the budget when it starts, not at the
/// stage's start (per-group queues).
#[test]
fn measure_reads_the_budget_before_each_groups_fan_out() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut ctx_raw = ServiceContext::new_for_tests(db_path.clone());
    ctx_raw.image_pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap());
    let ctx = Arc::new(ctx_raw);
    let (fixture, _fine, _coarse, _tiny, working, output) = seed_two_geometry_groups(&db_path);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let (run_id, group_ids) =
        seed_run_and_groups(&fixture.conn, fixture.set_id, &plan_groups, working.path(), output.path());
    let w = tempfile::tempdir().unwrap();
    let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout.clone(),
        output.path().to_path_buf(),
        group_ids.clone(),
    );
    stage_calibrate(&mut rc).unwrap();
    {
        let conn = ctx.db.get().unwrap().conn();
        ctx.settings
            .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "2048")
            .unwrap();
    }
    let switch = Arc::new(BudgetSwitch {
        ctx: ctx.clone(),
        stage: Stage::Measure,
        group_key: plan_groups[0].key.clone(),
        to_mb: 1024,
        done: AtomicBool::new(false),
    });
    rc.emitter = switch.clone() as Arc<dyn ProgressEmitter>;
    let (res, text) =
        crate::stacking::telemetry::capture_logs(|| stage_measure_with(&mut rc, Some(false)));
    res.unwrap();
    assert!(switch.done.load(Ordering::SeqCst), "the budget was switched mid-stage");
    let first = admitted_budgets(&text, Stage::Measure, &plan_groups[0].key);
    assert_eq!(first.first(), Some(&(2048u64 << 20)), "the first group's pass 1 ran on the old budget: {text}");
    for g in &plan_groups[1..] {
        let later = admitted_budgets(&text, Stage::Measure, &g.key);
        assert!(!later.is_empty(), "no admission line for {}: {text}", g.key);
        assert!(
            later.iter().all(|&b| b == 1024u64 << 20),
            "{} did not see the new budget: {later:?}",
            g.key
        );
    }
}

/// Task 13c L4c: Measure's per-worker working set carries no shared bytes, so equal-sized groups (an OSC and a
/// mono group of one sensor size, four mono groups of the runner) merge: the one queue lowers nobody's admission.
#[test]
fn measure_pass1_loads_of_equal_sized_groups_merge() {
    const GIB: u64 = 1 << 30;
    let load = |w: u64, h: u64| GroupLoad {
        working_set_bytes: measure_working_set_bytes(w, h),
        shared_bytes: 0,
    };
    // Dev Mac set 204: OSC + mono, one sensor size, 4 GiB budget, pool 10.
    let d = merged_admission(&[load(6248, 4176), load(6248, 4176)], 10, Some(4 * GIB));
    assert!(d.merge, "{d:?}");
    // Runner: four mono groups, 8 GiB budget (RAM / 4 of 32 GiB), pool 16.
    let hc11 = load(9576, 6388);
    let d = merged_admission(&[hc11; 4], 16, Some(8 * GIB));
    assert!(d.merge, "{d:?}");
    // A group with a larger frame lowers nobody's admission only when its own admission is already the max.
    let d = merged_admission(&[load(6248, 4176), load(9576, 6388)], 16, Some(8 * GIB));
    assert!(!d.merge, "{d:?}");
}

/// Task 13c L4c: stage 3 over several groups with ONE pass-1 queue gives the artifacts, metrics, group betas,
/// measured order, warnings and exclusions the per-group queues give.
#[test]
fn measure_one_pass1_queue_across_groups_matches_per_group_queues() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut ctx_raw = ServiceContext::new_for_tests(db_path.clone());
    ctx_raw.image_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap(),
    );
    let ctx = Arc::new(ctx_raw);
    let (fixture, _fine, _coarse, _tiny, working, output) = seed_two_geometry_groups(&db_path);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 3, "{plan_groups:?}");
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let w = tempfile::tempdir().unwrap();
    let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
    let rec = Arc::new(Recording::new());
    let mut rc = test_context(
        ctx.clone(),
        rec.clone() as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout.clone(),
        output.path().to_path_buf(),
        group_ids.clone(),
    );
    stage_calibrate(&mut rc).unwrap();
    let (warn0, excl0) = (rc.warnings.len(), rc.runtime_exclusions.len());
    // Both shapes re-measure from scratch in place over the same calibrated files.
    rc.rerun_from = Some(Stage::Measure);
    type Rows = Vec<(String, Option<i64>, String, String, Option<i64>)>;
    type Snapshot = (
        String,
        Vec<(String, Vec<u8>)>,
        Vec<String>,
        Vec<(i64, String)>,
        Vec<String>,
        Rows,
        Vec<(u64, u64)>,
    );
    // `mode`: 0 = per-group queues, 1 = one pass-1 queue, 2 = the pre-L4 baseline (the stage run once per
    // group, in group order, on this run context; its outcome is not compared — a group alone may be too small).
    let mut run = |mode: u8| -> Snapshot {
        rc.measured.clear();
        rc.measured_groups.clear();
        rc.warnings.truncate(warn0);
        rc.runtime_exclusions.truncate(excl0);
        rec.0.lock().unwrap().clear();
        // Each shape writes its own rows; the calibrated ones stay (they are the input).
        fixture
            .conn
            .execute(
                "DELETE FROM stacking_artifacts WHERE kind NOT IN ('calibrated', 'calibrated_mosaic')",
                [],
            )
            .unwrap();
        let outcome = if mode == 2 {
            for g in &plan_groups {
                rc.plan_groups = vec![g.clone()];
                let _ = stage_measure_with(&mut rc, Some(false));
            }
            rc.plan_groups = plan_groups.clone();
            String::new()
        } else {
            format!("{:?}", stage_measure_with(&mut rc, Some(mode == 1)).map_err(|e| format!("{e:?}")))
        };
        let mut rows: Rows = Vec::new();
        for kind in [ArtifactKind::Metrics, ArtifactKind::Measured] {
            for r in crate::db::stacking::list_artifacts(&fixture.conn, &rc.subject, Some(kind)).unwrap() {
                // A first-pass artifact's bytes carry its wall-clock timings: its size differs run to run.
                let size = if kind == ArtifactKind::Measured { None } else { r.size };
                rows.push((r.group_key, r.frame_id, r.kind, r.config_hash, size));
            }
        }
        let progress: Vec<(u64, u64)> = rec
            .events(STACKING_PROGRESS_EVENT)
            .iter()
            .filter(|e| e["stage"] == Stage::Measure.as_str())
            .map(|e| (e["current"].as_u64().unwrap(), e["total"].as_u64().unwrap()))
            .collect();
        let mut files = Vec::new();
        // Frame-level folders: flat, no group sub-folder (spec §5.1).
        for dir in [layout.measured_dir(), layout.fits_dir()] {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut names: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
            names.sort();
            for n in names {
                files.push((
                    n.file_name().unwrap().to_string_lossy().to_string(),
                    std::fs::read(&n).unwrap(),
                ));
            }
        }
        let mut order = Vec::new();
        for g in &plan_groups {
            let beta = rc.measured_groups.get(&g.key).map(|m| m.beta);
            order.push(format!("{} beta={beta:?}", g.key));
            for e in rc.measured.get(&g.key).into_iter().flatten() {
                order.push(format!(
                    "{} {} {:?} {:?} {}",
                    e.frame.frame_id,
                    e.included,
                    e.reason,
                    e.cached_metrics,
                    // The wall-clock `durationMs` is the one field that differs run to run.
                    {
                        let mut v = serde_json::to_value(&e.measurement).unwrap();
                        if let Some(o) = v.as_object_mut() {
                            o.remove("durationMs");
                        }
                        v.to_string()
                    }
                ));
            }
        }
        (
            outcome,
            files,
            rc.warnings.clone(),
            rc.runtime_exclusions.clone(),
            order,
            rows,
            progress,
        )
    };
    let per_group = run(0);
    let merged = run(1);
    let baseline = run(2);
    assert!(
        per_group.1.iter().any(|(n, _)| n.ends_with(".athm")),
        "the fixture writes first-pass artifacts: {:?}",
        per_group.1.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert_eq!(per_group.0, merged.0, "outcome");
    assert_eq!(per_group.2, merged.2, "warnings in the same order");
    assert_eq!(per_group.3, merged.3, "exclusions");
    assert_eq!(per_group.4, merged.4, "betas, measured order and metrics");
    // Artifact rows (metrics, first-pass `measured`) by (group, frame, kind, hash, size), ORDER BY id.
    assert!(!per_group.5.is_empty(), "the fixture writes measure rows");
    assert_eq!(per_group.5, merged.5, "measure rows");
    // The pre-L4 order: one stage run per group.
    assert_eq!(baseline.2, merged.2, "warnings against the pre-L4 interleaving");
    assert_eq!(baseline.3, merged.3, "exclusions against the pre-L4 interleaving");
    assert_eq!(baseline.4, merged.4, "measured order and metrics against the pre-L4 order");
    assert_eq!(baseline.5, merged.5, "measure rows against the pre-L4 order");
    // Review I1: the stage-wide progress never steps back and lands on its total, in both shapes.
    for (name, shot) in [("per-group", &per_group), ("merged", &merged)] {
        assert!(!shot.6.is_empty(), "{name}: no progress events");
        assert!(
            shot.6.windows(2).all(|w| w[0].0 <= w[1].0),
            "{name}: measure progress went backwards: {:?}",
            shot.6
        );
        let (last, total) = *shot.6.last().unwrap();
        assert_eq!(last, total, "{name}: the last event lands on total: {:?}", shot.6);
    }
    assert_eq!(per_group.1.len(), merged.1.len());
    for (a, b) in per_group.1.iter().zip(&merged.1) {
        assert_eq!(a.0, b.0);
        if a.0.ends_with(".athm") {
            // The body carries the first pass's wall-clock timings (`read_ms`, `elapsed`): two runs never
            // agree on those bytes, so compare what describes the pixels (stamp, geometry, own beta); the
            // pass-2 output derived from the body is compared byte for byte below (`.athf`) and above (metrics).
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join("a"), &a.1).unwrap();
            std::fs::write(d.path().join("b"), &b.1).unwrap();
            let ha = crate::stacking::pass1_artifact::read_pass1_head(&d.path().join("a")).unwrap();
            let hb = crate::stacking::pass1_artifact::read_pass1_head(&d.path().join("b")).unwrap();
            assert_eq!(ha, hb, "{} head differs between the two queue shapes", a.0);
        } else {
            assert!(a.1 == b.1, "{} differs between the two queue shapes", a.0);
        }
    }
}

// Perf tier 1 Task 3: Measure's and Normalize's working-set formulas
// moved from an all-planes guess to a measured one-channel-resident
// factor (`MEASURE_PLANES_RESIDENT`/`LN_PLANES_RESIDENT`) — both stages
// read and process one channel at a time, so the frame's own plane
// count (3 for OSC) must not multiply into the admitted worker count.
#[test]
fn measure_working_set_is_one_channel_deep() {
    // A 3-plane 26 Mpx frame: one channel resident at a time, so the
    // planes count does not multiply in.
    let one_channel = measure_working_set_bytes(6248, 4176);
    assert_eq!(one_channel, MEASURE_PLANES_RESIDENT * 6248 * 4176 * 4);
    // 16 GB, 10-worker pool: at least 3 OSC frames in flight.
    let n = (16u64 << 30) / 4 / one_channel;
    assert!(n >= 3, "admission would be {n}");
}

/// Plan 6b-2 C9 (the Measure boundary): a landed file the exchange
/// rewrote after the frame source read its row is excluded "changed
/// during the run"; its neighbours are not.
#[test]
fn measure_excludes_a_project_frame_whose_landed_file_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    let paths: Vec<PathBuf> = ["a", "b", "c", "d"]
        .iter()
        .map(|u| {
            test_fixtures::add_project_frame(
                &conn,
                &landed,
                "p1",
                &test_fixtures::project_frame_spec(u),
            )
        })
        .collect();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    seed_in_place_calibrated(&conn, &mut rc);
    std::fs::write(&paths[1], vec![0u8; 5000]).unwrap();

    // The synthetic frames may or may not measure well enough for a
    // viable group; the exclusion is recorded either way.
    match stage_measure(&mut rc) {
        Ok(()) => {}
        Err(RunError::Other(ref m)) if m.contains("no group has 3 included frames") => {}
        Err(e) => panic!("stage_measure failed unexpectedly: {e:?}"),
    }
    let b = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.filename == "c_b.fits")
        .unwrap()
        .frame_id;
    let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    assert_eq!(
        reasons.get(&b).map(String::as_str),
        Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
    );
    for f in rc.plan_groups[0].frames.iter().filter(|f| f.frame_id != b) {
        assert_ne!(
            reasons.get(&f.frame_id).map(String::as_str),
            Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
        );
    }
}
