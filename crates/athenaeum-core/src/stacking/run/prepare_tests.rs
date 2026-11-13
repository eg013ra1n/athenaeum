//! Tests of the prepare stage; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, seed_star_group, wait_for_run, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

/// Ruling R12 (b): what Prepare writes under `prepared/` (a debayered mosaic, a 16-bit FITS and an XISF converted
/// into the unit domain) is reproduced to the bit by a second Prepare of the same frames in a new run context,
/// on a four-thread pool. Length-only on the first pass's stamp holds for `prepared/` only if this does.
#[test]
fn re_preparing_reproduces_the_prepared_planes_bit_for_bit() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut raw = ServiceContext::new_for_tests(db_path.clone());
    raw.image_pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap());
    let ctx = Arc::new(raw);
    assert!(ctx.image_pool.current_num_threads() > 1);
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for (u, file, bayer) in [
        ("m1", test_fixtures::ProjectFile::Mosaic32, Some("RGGB")),
        ("m2", test_fixtures::ProjectFile::Mosaic32, Some("RGGB")),
        ("m3", test_fixtures::ProjectFile::Mosaic32, Some("RGGB")),
        ("i1", test_fixtures::ProjectFile::Mono16, None),
        ("x1", test_fixtures::ProjectFile::Xisf32, None),
    ] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = file;
        spec.bayerpat = bayer;
        test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
    }
    let working = tmp.path().join("work");
    let out = tmp.path().join("out");
    let prepared_bits = |rc: &RunContext| -> Vec<(i64, PathBuf, Vec<Vec<u32>>)> {
        let mut v: Vec<(i64, PathBuf, Vec<Vec<u32>>)> = rc
            .plan_groups
            .iter()
            .flat_map(|g| g.frames.iter())
            .map(|f| {
                let row = crate::db::stacking::find_artifact(
                    &conn,
                    &rc.subject,
                    "",
                    ArtifactKind::Calibrated,
                    Some(f.frame_id),
                )
                .unwrap()
                .unwrap();
                let path = PathBuf::from(row.path.unwrap());
                let bits = file_plane_bits(&path);
                (f.frame_id, path, bits)
            })
            .collect();
        v.sort_by_key(|(id, _, _)| *id);
        v
    };

    let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &working, &out);
    stage_calibrate(&mut rc).unwrap();
    assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
    let before = prepared_bits(&rc);
    assert_eq!(before.len(), 5);
    assert!(
        before.iter().all(|(_, p, _)| p.starts_with(rc.layout.prepared_root())),
        "every frame was written under prepared/: {before:?}"
    );
    assert_eq!(before.iter().filter(|(_, _, b)| b.len() == 3).count(), 3, "the mosaics were debayered");
    for (_, path, _) in &before {
        std::fs::remove_file(path).unwrap();
    }
    crate::db::stacking::delete_artifacts(&conn, &rc.subject, &[ArtifactKind::Calibrated]).unwrap();

    let mut rc2 = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &working, &out);
    let (res, logs) = crate::stacking::telemetry::capture_logs(|| stage_calibrate(&mut rc2));
    res.unwrap();
    if total_ram_bytes().is_some() {
        assert!(widest_admission(&logs) > 1, "the second Prepare ran in parallel: {logs}");
    }
    assert!(rc2.runtime_exclusions.is_empty(), "{:?}", rc2.runtime_exclusions);
    let after = prepared_bits(&rc2);
    for (_, path, _) in &after {
        assert!(path.is_file(), "re-prepared {}", path.display());
    }
    assert_eq!(before, after, "a second Prepare changed a pixel (or a path)");
}

/// Plan 6b-2 C12 (Review Focus 3): a unit-domain float replica, mono or
/// three-plane, is used IN PLACE. Its `calibrated` artifact is the landed
/// file, whose bytes and mtime never change. A second pass reuses it, and
/// `CleanupWhat::All` leaves it on disk.
#[test]
fn prepare_uses_a_float_replica_in_place_and_never_touches_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    let mut paths = Vec::new();
    for u in ["m1", "m2", "m3"] {
        paths.push(test_fixtures::add_project_frame(
            &conn,
            &landed,
            "p1",
            &test_fixtures::project_frame_spec(u),
        ));
    }
    for u in ["o1", "o2", "o3"] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = test_fixtures::ProjectFile::Rgb32;
        spec.bayerpat = Some("RGGB");
        paths.push(test_fixtures::add_project_frame(
            &conn, &landed, "p1", &spec,
        ));
    }
    let before: Vec<(Vec<u8>, std::time::SystemTime)> = paths
        .iter()
        .map(|p| {
            (
                std::fs::read(p).unwrap(),
                std::fs::metadata(p).unwrap().modified().unwrap(),
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
    let mut keys: Vec<&str> = rc.plan_groups.iter().map(|g| g.key.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["mono__L__300s", "osc__L__300s"]);

    stage_calibrate(&mut rc).unwrap();
    assert!(
        rc.runtime_exclusions.is_empty(),
        "{:?}",
        rc.runtime_exclusions
    );
    let artifacts =
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some(ArtifactKind::Calibrated)).unwrap();
    assert_eq!(artifacts.len(), 6);
    for a in &artifacts {
        let path = PathBuf::from(a.path.as_deref().unwrap());
        assert!(paths.contains(&path), "used in place: {path:?}");
    }
    assert!(!rc.layout.prepared_root().exists(), "nothing was written");
    assert_eq!(rc.cached_calibrated.len(), 6, "every frame reported");
    assert!(rc.cached_calibrated.values().all(|cached| !cached));

    stage_calibrate(&mut rc).unwrap();
    assert_eq!(rc.cached_calibrated.len(), 6);
    assert!(
        rc.cached_calibrated.values().all(|cached| *cached),
        "the second pass reuses every frame"
    );

    crate::stacking::paths::cleanup_work(
        &conn,
        &rc.subject,
        &rc.layout,
        crate::stacking::paths::CleanupWhat::All,
    )
    .unwrap();
    for (p, (bytes, mtime)) in paths.iter().zip(&before) {
        assert_eq!(&std::fs::read(p).unwrap(), bytes, "{p:?} was rewritten");
        assert_eq!(
            &std::fs::metadata(p).unwrap().modified().unwrap(),
            mtime,
            "{p:?} was touched"
        );
    }
}

/// Plan 6b-2 C12, C13: a CFA mosaic replica is debayered into `prepared/`
/// when `debayerOsc` is on:
/// - the generator's own VNG;
/// - the mosaic cards are gone and `ATH_PREP = 'debayered'`;
/// - no project or calibration stamp;
/// - the landed mosaic itself is kept for Bayer drizzle.
/// With `debayerOsc` off it is used in place, as a `cfa` group.
#[test]
fn prepare_debayers_a_mosaic_and_keeps_the_landed_mosaic_for_bayer_drizzle() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for u in ["o1", "o2", "o3"] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = test_fixtures::ProjectFile::Mosaic32;
        spec.bayerpat = Some("RGGB");
        test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
    }
    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.bayer = true;
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    assert_eq!(rc.plan_groups[0].key, "osc__L__300s");
    stage_calibrate(&mut rc).unwrap();
    assert!(
        rc.runtime_exclusions.is_empty(),
        "{:?}",
        rc.runtime_exclusions
    );
    let key = "";
    for f in rc.plan_groups[0].frames.clone() {
        let row = |kind: ArtifactKind| {
            crate::db::stacking::find_artifact(&conn, &rc.subject, key, kind, Some(f.frame_id))
                .unwrap()
                .and_then(|a| a.path)
                .unwrap()
        };
        let stem = Path::new(&f.filename)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let calibrated = PathBuf::from(row(ArtifactKind::Calibrated));
        assert_eq!(
            calibrated,
            rc.layout
                .prepared_dir()
                .join(format!("p_{stem}_f{}_d.fits", f.frame_id))
        );
        assert_eq!(row(ArtifactKind::CalibratedMosaic), f.path, "the landed mosaic itself");
        let h = FitsHeader::from_path(&calibrated).unwrap();
        assert_eq!(h.get_i32("NAXIS3"), Some(3));
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("debayered"));
        assert_eq!(h.get_str("ATH_CDBM").as_deref(), Some("VNG"));
        for absent in ["BAYERPAT", "CALSTAT", "ATH_CSRC", "ATH_PRJ"] {
            assert_eq!(h.get_str(absent), None, "{absent}");
        }
    }

    let mut off = StackingConfig::default();
    off.calibration.debayer_osc = false;
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        off,
        &tmp.path().join("work2"),
        &tmp.path().join("out"),
    );
    assert_eq!(rc.plan_groups[0].key, "cfa__L__300s");
    stage_calibrate(&mut rc).unwrap();
    for f in rc.plan_groups[0].frames.clone() {
        let a = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            "",
            ArtifactKind::Calibrated,
            Some(f.frame_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            a.path.as_deref(),
            Some(f.path.as_str()),
            "a mosaic kept as a mosaic is used in place"
        );
    }
}

/// Ruling R4 (a): an attested frame used in place has a `calibrated` row pointing at the user's own landed file.
/// Converting the frame later (the debayer turned on) writes a new prepared file and replaces the row, but the
/// landed file is never ours to remove, even when the working folder overlaps the scan root (here the landed
/// files sit INSIDE the set's working folder, which is only a warning in the settings).
#[test]
fn converting_an_in_place_frame_keeps_its_landed_file_inside_the_working_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let working = tmp.path().join("work");
    let subject = StackSubject::Project("p1".to_string());
    let landed = subject_layout(&conn, &working, &subject).unwrap().root.join("landed");
    for u in ["o1", "o2"] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = test_fixtures::ProjectFile::Mosaic32;
        spec.bayerpat = Some("RGGB");
        test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
    }
    let out = tmp.path().join("out");

    // Debayer off: the mosaics are used in place, the rows point at the landed files.
    let mut off = StackingConfig::default();
    off.calibration.debayer_osc = false;
    let mut rc = project_rc(&ctx, &conn, "p1", off, &working, &out);
    stage_calibrate(&mut rc).unwrap();
    let frames = rc.plan_groups[0].frames.clone();
    assert_eq!(frames.len(), 2);
    for f in &frames {
        let row = crate::db::stacking::find_artifact(&conn, &rc.subject, "", ArtifactKind::Calibrated, Some(f.frame_id))
            .unwrap()
            .unwrap();
        assert_eq!(row.path.as_deref(), Some(f.path.as_str()), "used in place");
        assert!(Path::new(&f.path).starts_with(&rc.layout.root), "the landed file is inside the working folder");
    }

    // Debayer on, same working folder: the rows move to `prepared/`, the landed files stay.
    let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &working, &out);
    stage_calibrate(&mut rc).unwrap();
    assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
    for f in &frames {
        let row = crate::db::stacking::find_artifact(&conn, &rc.subject, "", ArtifactKind::Calibrated, Some(f.frame_id))
            .unwrap()
            .unwrap();
        let now = PathBuf::from(row.path.unwrap());
        assert!(now.starts_with(rc.layout.prepared_dir()), "converted under prepared/: {now:?}");
        assert!(Path::new(&f.path).exists(), "the user's landed file {} was removed", f.path);
    }
}

/// File-identity §4.10, "never touched": a row that names the user's own file (here a landed file INSIDE the working
/// folder) is not a working file of its folder, so a copy of the folder never takes it over. The trigger maps a row
/// only when the first component below the old root is its kind's own subtree (`calibrated`, `prepared`, ...);
/// `landed` is none, so the in-place rows keep naming the first folder's landed files even though a copy of them
/// sits at the same place under the new folder with the same size and time.
#[test]
fn an_in_place_row_inside_the_working_root_is_never_rebased() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let working = tmp.path().join("work");
    let subject = StackSubject::Project("p1".to_string());
    let landed = subject_layout(&conn, &working, &subject).unwrap().root.join("landed");
    for u in ["o1", "o2"] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = test_fixtures::ProjectFile::Mosaic32;
        spec.bayerpat = Some("RGGB");
        test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
    }
    let out = tmp.path().join("out");
    let mut off = StackingConfig::default();
    off.calibration.debayer_osc = false;
    let mut rc = project_rc(&ctx, &conn, "p1", off.clone(), &working, &out);
    stage_calibrate(&mut rc).unwrap();
    let frames = rc.plan_groups[0].frames.clone();
    assert_eq!(frames.len(), 2);
    let row_path = |rc: &RunContext, frame_id: i64| -> String {
        crate::db::stacking::find_artifact(&conn, &rc.subject, "", ArtifactKind::Calibrated, Some(frame_id))
            .unwrap()
            .unwrap()
            .path
            .unwrap()
    };
    for f in &frames {
        assert_eq!(row_path(&rc, f.frame_id), f.path, "used in place");
        assert!(Path::new(&f.path).starts_with(&rc.layout.root.join("landed")), "{}", f.path);
    }

    // The whole working folder, landed files included, copied with its times; the next run points at the copy.
    let working2 = tmp.path().join("work2");
    copy_tree_keeping_times(&working, &working2);
    let mut rc2 = project_rc(&ctx, &conn, "p1", off, &working2, &out);
    assert_ne!(rc2.layout.root, rc.layout.root);
    for f in &frames {
        let copy = rc2.layout.root.join(Path::new(&f.path).strip_prefix(&rc.layout.root).unwrap());
        assert!(copy.is_file(), "the copy of the landed file sits at the same place under W2: {copy:?}");
    }
    // The rows are no candidates at all: the in-place rule of Prepare would re-point a rebased row at its light on
    // this very run, so the end state alone could not tell a rebase from none.
    let found = crate::stacking::rebase::candidates(&conn, &rc2.subject, &rc2.layout.root).unwrap();
    assert!(found.is_empty(), "an in-place row is no candidate: {found:?}");
    let (res, logs) = crate::stacking::telemetry::capture_logs(|| stage_calibrate(&mut rc2));
    res.unwrap();
    assert!(!logs.contains("working folder rows rebased"), "nothing was rebased: {logs}");
    for f in &frames {
        assert_eq!(row_path(&rc2, f.frame_id), f.path, "the in-place row still names W1's landed file");
    }
}

/// Plan 6b-2 C12: an XISF replica and a 16-bit FITS are converted into
/// float32 FITS under `prepared/`, in the unit domain. The 16-bit file
/// becomes exactly its physical values ÷ 65535. A float FITS beside them
/// stays in place.
#[test]
fn prepare_converts_xisf_and_integer_files_into_the_unit_domain() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    let mut landed_of = HashMap::new();
    for (u, file) in [
        ("m1", test_fixtures::ProjectFile::Mono32),
        ("x1", test_fixtures::ProjectFile::Xisf32),
        ("i1", test_fixtures::ProjectFile::Mono16),
    ] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = file;
        landed_of.insert(
            u,
            test_fixtures::add_project_frame(&conn, &landed, "p1", &spec),
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
    stage_calibrate(&mut rc).unwrap();
    assert!(
        rc.runtime_exclusions.is_empty(),
        "{:?}",
        rc.runtime_exclusions
    );
    let handle_of = |uuid: &str| -> i64 {
        rc.plan_groups[0]
            .frames
            .iter()
            .find(|f| f.path == landed_of[uuid].to_str().unwrap())
            .unwrap()
            .frame_id
    };
    let calibrated_of = |uuid: &str| -> PathBuf {
        PathBuf::from(
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                "",
                ArtifactKind::Calibrated,
                Some(handle_of(uuid)),
            )
            .unwrap()
            .and_then(|a| a.path)
            .unwrap(),
        )
    };
    assert_eq!(calibrated_of("m1"), landed_of["m1"]);
    for uuid in ["x1", "i1"] {
        let p = calibrated_of(uuid);
        assert_eq!(
            p,
            rc.layout
                .prepared_dir()
                .join(format!("p_c_{uuid}_f{}.fits", handle_of(uuid)))
        );
        let h = FitsHeader::from_path(&p).unwrap();
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));
        assert_eq!((h.get_str("CALSTAT"), h.get_str("ATH_PRJ")), (None, None));
        let plane = PlaneReader::open(&p).unwrap().read_plane(0).unwrap();
        assert!(
            plane.iter().all(|v| (0.0..=1.0).contains(v)),
            "{uuid}: unit domain"
        );
    }
    let physical = PlaneReader::open(&landed_of["i1"])
        .unwrap()
        .read_plane(0)
        .unwrap();
    let converted = PlaneReader::open(&calibrated_of("i1"))
        .unwrap()
        .read_plane(0)
        .unwrap();
    for (p, c) in physical.iter().zip(&converted) {
        assert_eq!(c.to_bits(), ((*p as f64 / 65535.0) as f32).to_bits());
    }
}

/// Plan 6b-2 C12: a file no reader here can decode is excluded
/// "unsupported file: …"; its neighbours are prepared.
#[test]
fn prepare_excludes_an_unsupported_file() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for u in ["m1", "m2", "m3"] {
        test_fixtures::add_project_frame(
            &conn,
            &landed,
            "p1",
            &test_fixtures::project_frame_spec(u),
        );
    }
    let mut tile = test_fixtures::project_frame_spec("t1");
    tile.file = test_fixtures::ProjectFile::EmptyPrimary;
    test_fixtures::add_project_frame(&conn, &landed, "p1", &tile);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    stage_calibrate(&mut rc).unwrap();
    let t1 = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.filename == "c_t1.fits")
        .unwrap()
        .frame_id;
    let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(
        reasons[&t1],
        "unsupported file: not a 1- or 3-plane 8/16/32-bit or float primary image"
    );
}

/// Review I12 (plan 6b-2 C13): the group's colour comes from the manifest,
/// Prepare's debayer from the file's own header. A file whose post-Prepare
/// plane count contradicts its group is excluded, never fed to Measure.
/// Here the manifest has no `bayerpat` (a `mono` group) while the file
/// declares a mosaic, which Prepare would debayer to three planes.
#[test]
fn prepare_excludes_a_file_whose_planes_contradict_its_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for u in ["m1", "m2", "m3"] {
        test_fixtures::add_project_frame(
            &conn,
            &landed,
            "p1",
            &test_fixtures::project_frame_spec(u),
        );
    }
    // `Mosaic32` writes BAYERPAT RGGB into the file; `bayerpat: None`
    // keeps it out of the manifest.
    let mut odd = test_fixtures::project_frame_spec("x1");
    odd.file = test_fixtures::ProjectFile::Mosaic32;
    test_fixtures::add_project_frame(&conn, &landed, "p1", &odd);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    assert_eq!(rc.plan_groups.len(), 1, "every frame is in the mono group");
    assert_eq!(rc.plan_groups[0].key, "mono__L__300s");
    stage_calibrate(&mut rc).unwrap();
    let x1 = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.filename == "c_x1.fits")
        .unwrap()
        .frame_id;
    let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(
        reasons[&x1],
        "plane count 3 disagrees with the group's colour mode"
    );
    assert!(
        !rc.layout.prepared_root().exists(),
        "nothing was written for it"
    );
}

/// Plan 6b-2 C9 (the Prepare boundary): a landed file the exchange
/// rewrote after the frame source read its row is excluded "changed
/// during the run" before Prepare reads it.
#[test]
fn prepare_excludes_a_project_frame_whose_landed_file_changed() {
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
    std::fs::write(&paths[1], vec![0u8; 5000]).unwrap();
    stage_calibrate(&mut rc).unwrap();
    let b = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.filename == "c_b.fits")
        .unwrap()
        .frame_id;
    let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(reasons[&b], crate::stacking::origin::CHANGED_DURING_THE_RUN);
    assert!(crate::db::stacking::find_artifact(
        &conn,
        &rc.subject,
        "",
        ArtifactKind::Calibrated,
        Some(b)
    )
    .unwrap()
    .is_none());
}

/// Spec §15.1 "Seam" (Review Focus 2): Prepare never reads the catalog
/// for a project frame. `files`/`frames` rows whose ids equal the
/// handles, pointing at nothing, are never followed, and the stage-1
/// hash is the Prepare hash over the manifest's content identity.
#[test]
fn prepare_never_reads_the_catalog_for_a_project() {
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
    let handles: Vec<i64> = rc.origins.keys().copied().collect();
    for (i, h) in handles.iter().enumerate() {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'decoy.fits', 7, '2020-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![h, format!("/decoy/{i}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, filter, exptime, naxis1, naxis2, bayerpat)
                 VALUES (?1, ?1, 'Light', 'Z', 999.0, 7, 7, 'RGGB')",
            [h],
        )
        .unwrap();
    }
    stage_calibrate(&mut rc).unwrap();
    assert!(
        rc.runtime_exclusions.is_empty(),
        "{:?}",
        rc.runtime_exclusions
    );
    for f in rc.plan_groups[0].frames.clone() {
        let a = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            "",
            ArtifactKind::Calibrated,
            Some(f.frame_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(a.path.as_deref(), Some(f.path.as_str()));
        let (blake3, size): (String, i64) = conn
            .query_row(
                "SELECT blake3, byte_size FROM project_frames_local WHERE project_id = 'p1' AND landed_path = ?1",
                [&f.path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            a.config_hash,
            crate::stacking::plan::prepare_hash_for_project(&rc.config, &blake3, size)
        );
    }
    let decoys: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM frames WHERE filter = 'Z' AND exptime = 999.0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decoys, 3, "the decoy rows are untouched");
}

/// Spec §13 (R3), plan 6b-2 C11: an attested set (externally calibrated
/// lights, no masters at all) runs end to end:
/// - its plan asks for no master;
/// - stage 1 prepares, converting the 16-bit lights into the unit domain
///   under `prepared/`, and reports in the stage-1 slot;
/// - nothing is calibrated;
/// - the run builds no master (review I11: read from the run's summary,
///   not from a catalog count that is zero either way).
#[test]
fn an_attested_set_stacks_without_masters() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    // Review I11: a buildable raw linked dark, so "no master" is a real
    // claim. The unattested plan owns one build; the attested one none.
    test_fixtures::add_raw_linked_dark(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    let library_dir = tmp.path().join("library");
    std::fs::create_dir_all(&library_dir).unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::CALIBRATION_LIBRARY_DIR,
        &library_dir.to_string_lossy(),
    )
    .unwrap();
    let plan_of = || {
        crate::stacking::plan::build_plan(
            &fixture.conn,
            &crate::settings::SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(fixture.set_id),
            None,
        )
        .unwrap()
    };
    assert_eq!(
        plan_of().masters_to_build.len(),
        1,
        "the unattested twin owes a master"
    );
    fixture
        .conn
        .execute(
            "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
            [fixture.set_id],
        )
        .unwrap();
    let plan = plan_of();
    assert!(
        plan.masters_to_build.is_empty(),
        "{:?}",
        plan.masters_to_build
    );
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
    .expect("an attested set needs no masters to start");
    wait_for_run(&ctx, started.run_id);
    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");

    let subject = StackSubject::FrameSet(fixture.set_id);
    let layout =
        crate::stacking::paths::subject_layout(&fixture.conn, working.path(), &subject)
            .unwrap();
    let calibrated =
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some(ArtifactKind::Calibrated))
            .unwrap();
    assert_eq!(calibrated.len(), light_ids.len());
    for a in &calibrated {
        let p = PathBuf::from(a.path.as_deref().unwrap());
        // The run canonicalizes its working folder (macOS `/var` is
        // `/private/var`), so compare resolved paths.
        assert!(
            crate::test_support::canonical_path(&p)
                .starts_with(crate::test_support::canonical_path(&layout.prepared_root())),
            "{p:?}"
        );
        let h = FitsHeader::from_path(&p).unwrap();
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));
        assert_eq!(h.get_str("CALSTAT"), None);
        // An attested light's output stem carries its frame id.
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            name.ends_with(&format!("_f{}.fits", a.frame_id.unwrap())),
            "{name}"
        );
    }
    assert!(!layout.calibrated_root().exists(), "nothing was calibrated");
    let summary: RunSummary =
        serde_json::from_str(run_row.summary_json.as_deref().expect("summary_json set"))
            .unwrap();
    assert!(
        summary.masters_built.is_empty(),
        "{:?}",
        summary.masters_built
    );
    assert!(
        summary.stages.iter().any(|t| t.stage == Stage::Calibrate),
        "Prepare reports in the stage-1 slot: {:?}",
        summary.stages
    );
}

/// Spec §13 (R3), plan 6b-2 C13 (fix round 1): an attested set of
/// externally debayered three-plane lights, with no BAYERPAT in the
/// catalog, groups as `osc__`, plans the same groups the run uses, and
/// stacks end to end with the files used in place. Its unattested twin
/// keeps the old `mono__` key: the frame-set rule did not move.
#[test]
fn an_attested_three_plane_set_groups_as_osc_and_stacks() {
    attested_three_plane_case(true);
}

/// Review round 2: with the debayer OFF the three-plane attested lights
/// stay `osc` (not `cfa`), Prepare expects three planes for an `osc__`
/// group and excludes nothing.
#[test]
fn an_attested_three_plane_set_stays_osc_with_the_debayer_off() {
    attested_three_plane_case(false);
}

fn attested_three_plane_case(debayer_osc: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &[0.0f32; 4]);
    make_lights_three_plane(&fixture, &shifts, &[0, 1, 2, 3]);
    let subject = StackSubject::FrameSet(fixture.set_id);
    let mut cfg = StackingConfig::default();
    cfg.calibration.debayer_osc = debayer_osc;

    // The unattested twin: catalog rule only, `mono__`.
    let (plain, _) = frame_source(&fixture.conn, &subject, &cfg).unwrap();
    assert_eq!(plain.len(), 1);
    assert!(plain[0].key.starts_with("mono__"), "{}", plain[0].key);

    fixture
        .conn
        .execute(
            "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
            [fixture.set_id],
        )
        .unwrap();
    let (groups, _) = frame_source(&fixture.conn, &subject, &cfg).unwrap();
    assert_eq!(groups.len(), 1);
    assert!(groups[0].key.starts_with("osc__"), "{}", groups[0].key);
    assert_eq!(groups[0].color_mode, ColorMode::Osc);
    let plan = crate::stacking::plan::build_plan(
        &fixture.conn,
        &crate::settings::SettingsManager::new(),
        &PathPolicy::AllowAll,
        &subject,
        Some(cfg.clone()),
    )
    .unwrap();
    let plan_keys: Vec<&str> = plan.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(
        plan_keys,
        vec![groups[0].key.as_str()],
        "the plan matches the run"
    );

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        subject.clone(),
        Some(cfg.clone()),
        None,
    )
    .expect("an attested set starts");
    wait_for_run(&ctx, started.run_id);
    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");
    let calibrated =
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some(ArtifactKind::Calibrated))
            .unwrap();
    assert_eq!(
        calibrated.len(),
        light_ids.len(),
        "Prepare excluded nothing"
    );
    for a in &calibrated {
        let p: String = fixture
            .conn
            .query_row(
                "SELECT fi.path FROM files fi WHERE fi.id = (SELECT file_id FROM frames WHERE id = ?1)",
                [a.frame_id.unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            a.path.as_deref(),
            Some(p.as_str()),
            "a three-plane light is used in place"
        );
    }
}

/// Zip task 8: an attested set whose externally debayered lights sit inside a zip stacks
/// like the disk set. Their colour comes from the entry headers (`osc__`, no extraction
/// at grouping), Prepare extracts each through the zip cache, and a float light that
/// would be used in place is copied into `prepared/` instead — the `calibrated`
/// artifact never names a cache file the cache may evict.
#[test]
fn an_attested_zip_resident_set_stacks_with_its_artifacts_off_the_cache() {
    use crate::zip_store::test_support::{zip_files, Shape};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &[0.0f32; 4]);
    make_lights_three_plane(&fixture, &shifts, &[0, 1, 2, 3]);
    fixture
        .conn
        .execute(
            "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
            [fixture.set_id],
        )
        .unwrap();
    let file_ids: Vec<i64> = light_ids
        .iter()
        .map(|id| {
            fixture
                .conn
                .query_row("SELECT file_id FROM frames WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        })
        .collect();
    let zip = tmp.path().join("lights.zip");
    let zipped = zip_files(&fixture.conn, &file_ids, &zip, Shape::Foreign);
    assert!(zipped.iter().all(|p| !Path::new(p).exists()));

    let subject = StackSubject::FrameSet(fixture.set_id);
    let cfg = StackingConfig::default();
    let (groups, _) = frame_source(&fixture.conn, &subject, &cfg).unwrap();
    assert_eq!(groups.len(), 1);
    assert!(
        groups[0].key.starts_with("osc__"),
        "the shape came from the zip headers: {}",
        groups[0].key
    );

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        subject.clone(),
        Some(cfg.clone()),
        None,
    )
    .expect("an attested zip-resident set starts");
    wait_for_run(&ctx, started.run_id);
    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");

    let layout =
        crate::stacking::paths::subject_layout(&fixture.conn, working.path(), &subject)
            .unwrap();
    let calibrated =
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some(ArtifactKind::Calibrated))
            .unwrap();
    assert_eq!(calibrated.len(), light_ids.len(), "Prepare excluded nothing");
    for a in &calibrated {
        let p = PathBuf::from(a.path.as_deref().unwrap());
        assert!(p.exists(), "{p:?}");
        assert!(
            crate::test_support::canonical_path(&p)
                .starts_with(crate::test_support::canonical_path(&layout.prepared_root())),
            "a zip-resident float light is copied into prepared/, not used in place: {p:?}"
        );
        assert!(!zipped.contains(&p.to_string_lossy().into_owned()));
    }
    let resident: i64 = fixture
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(resident, light_ids.len() as i64, "the rows stay in the zip");

    // A re-run with every stage-1 artifact fresh extracts NOTHING: freshness is decided from the
    // entry headers before any entry is read out of the zip.
    assert!(
        ctx.zip_cache.extraction_count() >= light_ids.len() as u64,
        "the first run extracted every light"
    );
    ctx.zip_cache.clear_unpinned().unwrap();
    assert_eq!(ctx.zip_cache.status().unwrap().entries, 0, "the cache is empty before the re-run");
    let extractions_before = ctx.zip_cache.extraction_count();
    let again = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        subject.clone(),
        Some(cfg),
        None,
    )
    .expect("the re-run starts");
    wait_for_run(&ctx, again.run_id);
    let rerun_row = crate::db::stacking::get_run(&fixture.conn, again.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(rerun_row.status, "done", "{rerun_row:?}");
    assert_eq!(
        ctx.zip_cache.extraction_count(),
        extractions_before,
        "a re-run with fresh stage-1 artifacts read nothing out of the zip"
    );
    assert_eq!(ctx.zip_cache.status().unwrap().entries, 0, "and the cache stays empty");
}

/// Review I12 on the attested path: a light whose catalog says colour
/// (BAYERPAT) but whose file is a plain one-plane image contradicts its
/// `osc__` group and is excluded, never fed to Measure.
#[test]
fn an_attested_light_contradicting_its_group_is_excluded() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let (fixture, _ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &[0.0f32; 4]);
    make_lights_three_plane(&fixture, &shifts, &[0, 1, 2]);
    fixture
        .conn
        .execute(
            "UPDATE frames SET bayerpat = 'RGGB'
                 WHERE file_id = (SELECT id FROM files WHERE filename = 'f3.fits')",
            [],
        )
        .unwrap();
    fixture
        .conn
        .execute(
            "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
            [fixture.set_id],
        )
        .unwrap();
    let subject = StackSubject::FrameSet(fixture.set_id);
    let cfg = StackingConfig::default();
    let (groups, origins) = frame_source(&fixture.conn, &subject, &cfg).unwrap();
    assert_eq!(
        groups.len(),
        1,
        "{:?}",
        groups.iter().map(|g| &g.key).collect::<Vec<_>>()
    );
    assert!(groups[0].key.starts_with("osc__"));
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &groups,
        working.path(),
        output.path(),
    );
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter),
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        groups,
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    rc.origins = origins;
    rc.memo = HashMemo::for_run(rc.origins.clone());
    stage_calibrate(&mut rc).unwrap();
    let odd = rc.plan_groups[0]
        .frames
        .iter()
        .find(|f| f.filename == "f3.fits")
        .unwrap()
        .frame_id;
    let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(
        reasons[&odd],
        "plane count 1 disagrees with the group's colour mode"
    );
}

/// Plan 6b-2 C11: one stage-1 path per run. A run whose frames mix
/// attested and calibrated origins is refused before any file is read.
#[test]
fn a_run_mixing_prepared_and_calibrated_frames_is_refused() {
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
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter),
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    let first = rc.plan_groups[0].frames[0].clone();
    let mut origins: HashMap<i64, FrameOrigin> = (*rc.origins).clone();
    origins.insert(
        first.frame_id,
        FrameOrigin::Catalog {
            file_id: first.file_id,
            attested: true,
        },
    );
    rc.origins = Arc::new(origins);
    rc.memo = HashMemo::for_run(rc.origins.clone());
    let err = stage_calibrate(&mut rc).unwrap_err();
    assert!(
        matches!(err, RunError::Other(ref m) if m.contains("cannot mix prepared and calibrated frames")),
        "{err:?}"
    );
    assert!(!rc.layout.calibrated_root().exists() && !rc.layout.prepared_root().exists());
}

/// Plan 6b-2 final review I1: a three-plane replica without a manifest
/// `bayerpat` (an externally debayered contributor light) groups as
/// `osc`, is used in place by Prepare and is excluded nowhere.
#[test]
fn prepare_uses_a_three_plane_replica_without_a_manifest_bayerpat_in_an_osc_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    let mut paths = Vec::new();
    for u in ["e1", "e2", "e3"] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = test_fixtures::ProjectFile::Rgb32;
        spec.bayerpat = None;
        paths.push(test_fixtures::add_project_frame(
            &conn, &landed, "p1", &spec,
        ));
    }
    for debayer_osc in [true, false] {
        let mut cfg = StackingConfig::default();
        cfg.calibration.debayer_osc = debayer_osc;
        let work = tmp.path().join(format!("work-{debayer_osc}"));
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &work, &tmp.path().join("out"));
        let keys: Vec<&str> = rc.plan_groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, vec!["osc__L__300s"], "debayer_osc={debayer_osc}");
        stage_calibrate(&mut rc).unwrap();
        assert!(
            rc.runtime_exclusions.is_empty(),
            "{:?}",
            rc.runtime_exclusions
        );
        let artifacts =
            crate::db::stacking::list_artifacts(&conn, &rc.subject, Some(ArtifactKind::Calibrated))
                .unwrap();
        assert_eq!(artifacts.len(), 3);
        for a in &artifacts {
            let path = PathBuf::from(a.path.as_deref().unwrap());
            assert!(paths.contains(&path), "used in place: {path:?}");
        }
    }
}

/// The file-identity write order (§4.2) at Prepare: a row is gone before the file it names is replaced. Run B
/// (`debayer_osc` flipped, which moves the prepare hash) is cut short at its second frame; the later jobs already
/// rewrote their `prepared/` files, so without the delete they would keep run A's row over them. The files of a mono
/// light come out byte-equal whatever the flip, so this proves the delete order, not wrong pixels.
#[test]
fn a_cancelled_prepare_leaves_no_row_over_its_files() {
    let rig = Pass1Rig::attested_16bit();
    let frames: Vec<i64> = rig.plan_groups[0].frames.iter().map(|f| f.frame_id).collect();
    assert_eq!(frames.len(), 4);
    let cfg_a = StackingConfig::default();
    rig.try_stage_one(&cfg_a).unwrap();
    let rows_a = rig.rows(ArtifactKind::Calibrated);
    assert_eq!(rows_a.len(), 4, "premise: run A prepared every frame");
    for (frame_id, row) in &rows_a {
        let path = Path::new(row.path.as_deref().unwrap());
        assert!(path.starts_with(rig.layout.prepared_root()), "frame {frame_id}: {path:?}");
    }

    let mut cfg_b = cfg_a.clone();
    cfg_b.calibration.debayer_osc = !cfg_b.calibration.debayer_osc;
    let res = rig.try_stage_one_with_job_cancelled(&cfg_b, rig.layout.prepared_root(), frames[1]);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");

    let rows_b = rig.rows(ArtifactKind::Calibrated);
    let first_b = rows_b.get(&frames[0]).expect("frame 1 was committed before the cancelled job");
    assert_ne!(first_b.config_hash, rows_a[&frames[0]].config_hash, "frame 1 carries run B's hash");
    for frame_id in &frames[1..] {
        let old = &rows_a[frame_id];
        assert!(Path::new(old.path.as_deref().unwrap()).is_file(), "frame {frame_id}'s file is still there");
        assert!(
            !rows_b.contains_key(frame_id),
            "frame {frame_id} keeps a row over a file it may have been rewritten: {:?}",
            rows_b.get(frame_id)
        );
    }

    let (res, logs) = crate::stacking::telemetry::capture_logs(|| rig.try_stage_one(&cfg_a));
    res.unwrap();
    assert_eq!(fan_out_items(&logs, "calibrate", "prepare"), 4, "{logs}");
    for (frame_id, row) in rig.rows(ArtifactKind::Calibrated) {
        assert_eq!(row.config_hash, rows_a[&frame_id].config_hash, "frame {frame_id} is back at A's hash");
    }
}

/// The in-place rule (file-identity §4.1): a `calibrated` row that names the user's own light is fresh only while it
/// names THIS frame's current input. After a relocate the row names the light's old path, where another file of the
/// same size and the recorded time now sits: the gate counts that frame stale, and the run re-points the row without
/// writing a byte under `prepared/` and without reading the pixels of pass 1 again.
#[test]
fn an_in_place_row_at_another_path_than_its_light_is_stale() {
    let mut rig = Pass1Rig::attested();
    let output = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, rig.working.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&rig.fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }
    let cached = |rig: &Pass1Rig| {
        crate::stacking::plan::build_plan(
            &rig.fixture.conn,
            &rig.ctx.settings,
            &crate::api::PathPolicy::AllowAll,
            &rig.subject,
            Some(rig.cfg.clone()),
        )
        .expect("the plan should build")
        .groups[0]
            .calibrated_cached
    };

    let first = rig.run("run 1", None);
    assert_eq!(first.logged.len(), 4, "premise: run 1 measured every light");
    let frame = rig.plan_groups[0].frames[0].clone();
    let old_path = PathBuf::from(&frame.path);
    let row = rig.rows(ArtifactKind::Calibrated)[&frame.frame_id].clone();
    assert_eq!(Path::new(row.path.as_deref().unwrap()), old_path, "premise: the row names the light in place");
    let n = cached(&rig);
    assert_eq!(n, 4, "premise: the gate reads every in-place row as cached");

    // The light moves: a copy that keeps its time, the catalog row follows (as a relocate does), and another file of
    // the same size carrying the recorded time is left at the old path.
    let mtime = std::fs::metadata(&old_path).unwrap().modified().unwrap();
    let new_path = old_path.with_file_name("moved_f0.fits");
    std::fs::copy(&old_path, &new_path).unwrap();
    let stamp = |path: &Path| {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(mtime).unwrap();
    };
    stamp(&new_path);
    let mut bytes = std::fs::read(&old_path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    std::fs::write(&old_path, &bytes).unwrap();
    stamp(&old_path);
    assert_eq!(std::fs::metadata(&old_path).unwrap().len(), row.size.unwrap() as u64, "premise: the decoy has the recorded size");
    assert_eq!(std::fs::metadata(&old_path).unwrap().modified().unwrap(), mtime, "premise: and the recorded time");
    rig.fixture
        .conn
        .execute(
            "UPDATE files SET path = ?1, filename = ?2 WHERE path = ?3",
            rusqlite::params![new_path.to_str().unwrap(), "moved_f0.fits", old_path.to_str().unwrap()],
        )
        .unwrap();
    assert_eq!(cached(&rig), 3, "the row at the old path is not this frame's input");

    // The run takes `GroupFrame.path` from the plan groups: recompute them.
    let (groups, origins) =
        frame_source(&rig.fixture.conn, &rig.subject, &rig.cfg).unwrap();
    assert_eq!(groups[0].frames.iter().find(|f| f.frame_id == frame.frame_id).unwrap().path, new_path.to_string_lossy());
    rig.plan_groups = groups;
    rig.set_origins(origins);
    let second = rig.run("run 2", None);

    let prepared = rig.layout.prepared_root();
    let written = std::fs::read_dir(&prepared).map_or(0, |entries| entries.count());
    assert_eq!(written, 0, "an in-place frame writes nothing under prepared/");
    let row_after = rig.rows(ArtifactKind::Calibrated)[&frame.frame_id].clone();
    assert_eq!(Path::new(row_after.path.as_deref().unwrap()), new_path, "the row is re-pointed at the current input");
    assert!(second.logged.is_empty(), "pass 1 re-ran for {:?}", second.logged);
    assert_eq!(cached(&rig), 4);
}

/// FI-11's exemption: a row that names the frame's INPUT in place (a file Athenaeum did not write) keeps key + size and
/// no time check. The light's identity is in the key (size + content sample); a copy of it with another time (here
/// an hour later, the same bytes) is the same light, so the gate still reads it cached and stage 1 reuses every row.
#[test]
fn an_in_place_row_is_reused_when_its_input_has_another_time() {
    let rig = Pass1Rig::attested();
    let output = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, rig.working.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&rig.fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }
    let cached = |rig: &Pass1Rig| {
        crate::stacking::plan::build_plan(
            &rig.fixture.conn,
            &rig.ctx.settings,
            &crate::api::PathPolicy::AllowAll,
            &rig.subject,
            Some(rig.cfg.clone()),
        )
        .expect("the plan should build")
        .groups[0]
            .calibrated_cached
    };
    let first = rig.run("run 1", None);
    assert_eq!(first.logged.len(), 4, "premise: run 1 measured every light");
    let rows = rig.rows(ArtifactKind::Calibrated);
    let lights: Vec<PathBuf> = rig.plan_groups[0].frames.iter().map(|f| PathBuf::from(&f.path)).collect();
    for (frame, light) in rig.plan_groups[0].frames.iter().zip(&lights) {
        assert_eq!(
            Path::new(rows[&frame.frame_id].path.as_deref().unwrap()),
            light,
            "premise: the row names the light in place"
        );
    }
    assert_eq!(cached(&rig), 4, "premise: the gate reads every in-place row as cached");

    let before: Vec<Vec<u8>> = lights.iter().map(|p| std::fs::read(p).unwrap()).collect();
    for light in &lights {
        let moved = std::fs::metadata(light).unwrap().modified().unwrap() + std::time::Duration::from_secs(3600);
        std::fs::OpenOptions::new().write(true).open(light).unwrap().set_modified(moved).unwrap();
    }
    let after: Vec<Vec<u8>> = lights.iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert_eq!(before, after, "premise: only the time moved");

    assert_eq!(cached(&rig), 4, "the gate does not compare an input's time");
    let (res, logs) = crate::stacking::telemetry::capture_logs(|| rig.try_stage_one(&rig.cfg));
    res.unwrap();
    assert_eq!(fan_out_items(&logs, "calibrate", "prepare"), 0, "stage 1 reused every in-place row: {logs}");
}

/// Review F2 (file-identity §4.1): a Prepare subject's own files written in a previous working folder stay fresh in
/// the gate, as they do in the run (a `Write` action has no path expectation). The run recorded W1; the setting then
/// moves to W2; the gate counts every frame cached (the rows sit under a folder the subject's runs recorded) and the
/// next run prepares none.
#[test]
fn a_previous_working_folders_prepared_rows_stay_fresh_in_the_gate() {
    let rig = Pass1Rig::attested_16bit();
    let cfg = rig.cfg.clone();
    rig.try_stage_one(&cfg).unwrap();
    let rows = rig.rows(ArtifactKind::Calibrated);
    assert_eq!(rows.len(), 4, "premise: the run prepared every frame");
    for (frame_id, row) in &rows {
        let path = Path::new(row.path.as_deref().unwrap());
        assert!(path.starts_with(rig.layout.prepared_root()), "frame {frame_id}: {path:?}");
    }

    let output = tempfile::tempdir().unwrap();
    let moved = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, moved.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&rig.fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }
    let plan = crate::stacking::plan::build_plan(
        &rig.fixture.conn,
        &rig.ctx.settings,
        &crate::api::PathPolicy::AllowAll,
        &rig.subject,
        Some(cfg.clone()),
    )
    .expect("the plan should build");
    assert_eq!(plan.groups[0].calibrated_cached, 4, "the gate reads the previous folder's rows as cached");

    let (res, logs) = crate::stacking::telemetry::capture_logs(|| rig.try_stage_one(&cfg));
    res.unwrap();
    assert_eq!(fan_out_items(&logs, "calibrate", "prepare"), 0, "{logs}");
}

/// Review F1 (file-identity §4.2), at Prepare: the other same-named set's rows over the `prepared/` files this run
/// rewrites (`debayer_osc` flipped, which moves the prepare hash) are retired with the writer's own.
#[test]
fn a_same_named_sets_prepare_retires_the_other_sets_rows() {
    let rig = Pass1Rig::attested_16bit();
    let cfg_a = rig.cfg.clone();
    rig.try_stage_one(&cfg_a).unwrap();
    let other = same_named_set_with_copies_of_the_calibrated_rows(&rig);
    assert_eq!(rows_of_kind(&rig.fixture.conn, other, ArtifactKind::Calibrated).len(), 4, "premise: the copies stand");

    let mut cfg_b = cfg_a.clone();
    cfg_b.calibration.debayer_osc = !cfg_b.calibration.debayer_osc;
    rig.try_stage_one(&cfg_b).unwrap();

    let other_rows = rows_of_kind(&rig.fixture.conn, other, ArtifactKind::Calibrated);
    assert!(other_rows.is_empty(), "the other set keeps rows over rewritten files: {other_rows:?}");
}
