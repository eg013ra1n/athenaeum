//! Tests of the frame rows and the frame-level files; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::stacking::test_fixtures;

#[test]
fn frame_file_stem_ignores_group_membership() {
    let frame = |id: i64, filename: &str| GroupFrame {
        frame_id: id,
        file_id: id,
        filename: filename.to_string(),
        path: format!("/data/{filename}"),
        size: 1,
        modified_at: String::new(),
        exposure_s: Some(60.0),
        date_obs: None,
        width: 8,
        height: 8,
        pixel_scale_arcsec: None,
        scale_source: None,
    };
    // Two frames sharing a source basename: each stem carries its own id, whoever else is in the group.
    assert_eq!(frame_file_stem(&frame(7, "Light_0001.fits")), "Light_0001_f7");
    assert_eq!(frame_file_stem(&frame(9, "Light_0001.fits")), "Light_0001_f9");
    // A lone frame gets the suffix too: the stem does not change when a merge brings a namesake in.
    assert_eq!(frame_file_stem(&frame(3, "Light_0001.fits")), "Light_0001_f3");
    assert_eq!(frame_file_stem(&frame(4, "no_extension")), "no_extension_f4");
}

/// Ruling R4 (a): `replace_frame_level_file` removes an old file only under the root its writer owns, and only
/// when no row of ANY subject still points at it.
#[test]
fn replace_frame_level_file_removes_only_under_the_owned_root_and_only_if_unreferenced() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let _ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let owned = tmp.path().join("work").join("calibrated");
    let elsewhere = tmp.path().join("work").join("landed");
    std::fs::create_dir_all(&owned).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let subject = StackSubject::Project("p1".to_string());
    let row_for = |path: &Path| crate::db::stacking::StackingArtifactRow {
        id: 0,
        subject: subject.clone(),
        frame_id: Some(1),
        group_key: String::new(),
        kind: "calibrated".to_string(),
        path: path.to_str().map(str::to_string),
        config_hash: "h".to_string(),
        size: None,
        modified_at: None,
        payload_json: None,
        created_at: String::new(),
    };
    let new = owned.join("c_new.fits");

    // Under the owned root, unreferenced: removed.
    let gone = owned.join("c_gone.fits");
    std::fs::write(&gone, b"x").unwrap();
    replace_frame_level_file(&conn, &owned, Some(&row_for(&gone)), &new);
    assert!(!gone.exists());

    // Outside the owned root (a landed file in the working folder): kept.
    let landed = elsewhere.join("c_landed.fits");
    std::fs::write(&landed, b"x").unwrap();
    replace_frame_level_file(&conn, &owned, Some(&row_for(&landed)), &new);
    assert!(landed.exists(), "a file outside the owned root was removed");

    // Under the owned root but still named by a row of ANOTHER subject: kept.
    let shared = owned.join("c_shared.fits");
    std::fs::write(&shared, b"x").unwrap();
    crate::db::stacking::insert_test_project(&conn, "p2");
    upsert_artifact(
        &conn,
        &NewArtifact {
            subject: &StackSubject::Project("p2".to_string()),
            frame_id: None,
            group_key: "g",
            kind: "solve",
            path: shared.to_str(),
            config_hash: "h",
            size: None,
            modified_at: None,
            payload_json: None,
        },
    )
    .unwrap();
    replace_frame_level_file(&conn, &owned, Some(&row_for(&shared)), &new);
    assert!(shared.exists(), "a file another subject's row points at was removed");
}

/// Plan 6b-2 C9 (the Register and Output boundaries): a still-included
/// member whose landed file changed is excluded and persisted; a changed
/// reference fails the run.
#[test]
fn exclude_changed_members_excludes_and_fails_on_a_changed_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for u in ["a", "b", "c"] {
        test_fixtures::add_project_frame(
            &conn,
            &landed,
            "p1",
            &test_fixtures::project_frame_spec(u),
        );
    }
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let group = rc.plan_groups[0].clone();
    let entries: Vec<MeasuredFrame> = group
        .frames
        .iter()
        .map(|f| MeasuredFrame {
            frame: f.clone(),
            calibrated: Some(PathBuf::from(&f.path)),
            planes: 1,
            measurement: None,
            weight: None,
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
        })
        .collect();
    rc.measured.insert(group.key.clone(), entries);

    std::fs::write(&group.frames[1].path, vec![0u8; 5000]).unwrap();
    assert!(
        rc.group_ids.contains_key(&group.key),
        "the persist path needs the group's row"
    );
    assert_eq!(exclude_changed_members(&mut rc, &group).unwrap(), 1);
    let changed_id = group.frames[1].frame_id;
    let (included, reason): (i64, Option<String>) = conn
        .query_row(
            "SELECT included, exclusion_reason FROM stacking_run_frames
                 WHERE run_id = ?1 AND COALESCE(frame_id, project_frame) = ?2",
            rusqlite::params![rc.run_id, changed_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(included, 0);
    assert_eq!(
        reason.as_deref(),
        Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
    );
    let entry = &rc.measured[&group.key][1];
    assert!(!entry.included);
    assert_eq!(
        entry.reason.as_deref(),
        Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
    );
    assert_eq!(
        exclude_changed_members(&mut rc, &group).unwrap(),
        0,
        "an excluded frame is not checked again"
    );

    rc.reference_frame_id = Some(group.frames[0].frame_id);
    std::fs::write(&group.frames[0].path, vec![0u8; 6000]).unwrap();
    let err = exclude_changed_members(&mut rc, &group).unwrap_err();
    assert!(
        matches!(err, RunError::Other(ref m) if m.contains("reference frame changed during the run")),
        "{err:?}"
    );
}
