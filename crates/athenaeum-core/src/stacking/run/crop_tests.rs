//! Tests of the crop pass; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::test_fixtures::{
    self, seed_star_group, wait_for_run, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

/// Plan 6b-3 D10, Review Focus 1: `cropToCoverage = 0` is today's path —
/// every output is written once, straight into the output folder in its
/// own container (the maps stay FITS), and no scratch folder is made.
#[test]
fn a_zero_crop_keeps_todays_output_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4]);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    let mut cfg = StackingConfig::default();
    cfg.output.coverage_map = true;
    cfg.output.format = crate::stacking::config::OutputFormat::Xisf;
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.write_weight_map = true;
    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .unwrap();
    wait_for_run(&ctx, started.run_id);
    let rows = crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
    assert!(
        rows.len() >= 5,
        "master, coverage, drizzle, weight map, drizzle coverage: {rows:?}"
    );
    for m in &rows {
        assert!(
            crate::test_support::canonical_path(Path::new(&m.path))
                .starts_with(crate::test_support::canonical_path(output.path())),
            "{}",
            m.path
        );
        let expected = if m.kind.ends_with("coverage") {
            "fits"
        } else {
            "xisf"
        };
        assert_eq!(m.format, expected, "{}", m.kind);
    }
    assert!(
        walkdir::WalkDir::new(working.path())
            .into_iter()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().ends_with("-uncropped")),
        "no scratch folder without a crop"
    );
}

/// Spec §5.9, plan 6b-3 D10: a co-registered run with `cropToCoverage =
/// K` crops the master, its coverage map, the drizzled master, its weight
/// map and its coverage to one rectangle — the uncropped run's `coverage
/// ≥ K` box (×2 for the drizzle family) — with the uncropped run's pixels
/// inside it, `CRPIX` moved by exactly the crop origin, rows, group paths
/// and summary on the final files, and the scratch folder gone.
#[test]
fn a_co_registered_run_crops_every_output_to_one_rectangle() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let weighted = |cfg: &mut StackingConfig| {
        drizzle_2x(cfg);
        cfg.drizzle.write_weight_map = true;
    };
    let (ctx, conn, full_rc, grid) = run_sky_project(tmp.path(), weighted);
    let full = crate::db::stacking::list_master_lights(&conn, full_rc.run_id).unwrap();
    let reader = PlaneReader::open(Path::new(&master_row(&full, "coverage").path)).unwrap();
    let rect = crate::stacking::crop::coverage_rect(
        &reader.read_plane(0).unwrap(),
        reader.width(),
        reader.height(),
        4,
    )
    .expect("the four frames overlap");
    assert!(rect.width < 512 && rect.height < 384, "{rect:?}");

    let mut cfg = sky_config(&conn);
    weighted(&mut cfg);
    cfg.output.crop_to_coverage = 4;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    run_pipeline(&mut rc).unwrap();

    let cropped = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    for (kind, s) in [
        ("master", 1),
        ("coverage", 1),
        ("drizzle", 2),
        ("weight_map", 2),
        ("drizzle_coverage", 2),
    ] {
        let (a, b) = (master_row(&full, kind), master_row(&cropped, kind));
        let r = rect.scaled(s);
        assert_eq!(
            (b.width as usize, b.height as usize),
            (r.width, r.height),
            "{kind}"
        );
        assert!(
            Path::new(&b.path).starts_with(tmp.path().join("out")),
            "{kind}: {}",
            b.path
        );
        let (ra, rb) = (
            PlaneReader::open(Path::new(&a.path)).unwrap(),
            PlaneReader::open(Path::new(&b.path)).unwrap(),
        );
        for p in 0..ra.channels() {
            let expected =
                crate::stacking::crop::crop_plane(&ra.read_plane(p).unwrap(), ra.width(), r);
            let got = rb.read_plane(p).unwrap();
            assert!(
                expected
                    .iter()
                    .zip(&got)
                    .all(|(x, y)| x.to_bits() == y.to_bits()),
                "{kind} plane {p}: the crop changes no pixel"
            );
        }
        let ha = crate::fits_parser::FitsHeader::from_path(Path::new(&a.path)).unwrap();
        let hb = crate::fits_parser::FitsHeader::from_path(Path::new(&b.path)).unwrap();
        for (key, origin) in [("CRPIX1", r.x0), ("CRPIX2", r.y0)] {
            let moved = ha.get_f64(key).unwrap() - origin as f64;
            assert!(
                (hb.get_f64(key).unwrap() - moved).abs() < 1e-9,
                "{kind} {key}"
            );
        }
    }
    let groups = crate::db::stacking::list_groups(&conn, rc.run_id).unwrap();
    assert_eq!(
        groups[0].master_path.as_deref(),
        Some(master_row(&cropped, "master").path.as_str())
    );
    assert_eq!(
        groups[0].drizzle_path.as_deref(),
        Some(master_row(&cropped, "drizzle").path.as_str())
    );
    let g = &rc.summary.groups[0];
    assert_eq!(g.master_path, groups[0].master_path);
    assert_eq!(
        g.coverage_path.as_deref(),
        Some(master_row(&cropped, "coverage").path.as_str())
    );
    assert_eq!(
        g.weight_map_path.as_deref(),
        Some(master_row(&cropped, "weight_map").path.as_str())
    );
    assert_eq!(
        g.drizzle_coverage_path.as_deref(),
        Some(master_row(&cropped, "drizzle_coverage").path.as_str())
    );
    assert!(
        !rc.layout.uncropped_dir(rc.run_id).exists(),
        "the scratch folder is removed"
    );
}

/// The sky project with a second group: three `R` frames about 40 px east
/// of the four `Lum` ones (canonical filters `Red` and `Lum`).
fn seed_two_group_project(
    tmp: &Path,
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    crate::collab::grid::CanonicalGrid,
) {
    let db_path = tmp.join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.join("collab"), 4);
    let target: (f64, f64) = (83.8, -5.4);
    let east = 40.0 * 1.5 / 3600.0 / target.1.to_radians().cos();
    let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
    for i in 0..3 {
        let uuid = format!("r{i}");
        let centre = (target.0 + east + i as f64 * 0.001, target.1);
        let mut f = test_fixtures::sky_frame(&uuid, centre);
        f.filter = "R";
        f.noise_seed = 200 + i as u64;
        test_fixtures::add_project_sky_frame(&conn, &tmp.join("collab"), "p1", &f, &stars);
    }
    conn.execute(
        "UPDATE project_frames_local
                SET filter_canonical = CASE WHEN frame_uuid LIKE 'r%' THEN 'Red' ELSE 'Lum' END
              WHERE project_id = 'p1'",
        [],
    )
    .unwrap();
    (ctx, conn, grid)
}

/// Spec §5.9, plan 6b-3 D10: in native mode each group crops to its own
/// `coverage ≥ K` box — two groups shot 40 px apart get two rectangles,
/// each tight (its border rows and columns reach `K`) — and in
/// co-registered mode both get the one, wider, union.
#[test]
fn native_mode_crops_each_group_to_its_own_rectangle() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_two_group_project(tmp.path());
    let run = |geometry: RegistrationGeometry| -> RunContext {
        let mut cfg = sky_config(&conn);
        cfg.registration.geometry = geometry;
        cfg.output.crop_to_coverage = 3;
        crate::db::stacking::set_project_config(
            &conn,
            "p1",
            &serde_json::to_string(&cfg).unwrap(),
            &[],
            None,
        )
        .unwrap();
        let mut rc = project_rc(
            &ctx,
            &conn,
            "p1",
            cfg,
            &tmp.path().join("work"),
            &tmp.path().join("out"),
        );
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
        run_pipeline(&mut rc).unwrap();
        rc
    };
    // (group, CRPIX1, CRPIX2, width, height) of each master.
    let rects = |rc: &RunContext| -> Vec<(String, f64, f64, i64, i64)> {
        crate::db::stacking::list_master_lights(&conn, rc.run_id)
            .unwrap()
            .into_iter()
            .filter(|m| m.kind == "master")
            .map(|m| {
                let h = crate::fits_parser::FitsHeader::from_path(Path::new(&m.path)).unwrap();
                (
                    m.group_key,
                    h.get_f64("CRPIX1").unwrap(),
                    h.get_f64("CRPIX2").unwrap(),
                    m.width,
                    m.height,
                )
            })
            .collect()
    };

    let native = run(RegistrationGeometry::Native);
    let n = rects(&native);
    assert_eq!(n.len(), 2, "{n:?}");
    assert!(
        (n[0].1 - n[1].1).abs() > 20.0,
        "two rectangles about 40 px apart: {n:?}"
    );
    for m in crate::db::stacking::list_master_lights(&conn, native.run_id)
        .unwrap()
        .iter()
        .filter(|m| m.kind == "coverage")
    {
        let r = PlaneReader::open(Path::new(&m.path)).unwrap();
        let (w, h) = (r.width(), r.height());
        let c = r.read_plane(0).unwrap();
        let reaches = |mut it: Box<dyn Iterator<Item = f32> + '_>| it.any(|v| v >= 3.0);
        assert!(
            reaches(Box::new(c[..w].iter().copied())),
            "{}: first row",
            m.path
        );
        assert!(
            reaches(Box::new(c[(h - 1) * w..].iter().copied())),
            "{}: last row",
            m.path
        );
        assert!(
            reaches(Box::new((0..h).map(|y| c[y * w]))),
            "{}: first column",
            m.path
        );
        assert!(
            reaches(Box::new((0..h).map(|y| c[y * w + w - 1]))),
            "{}: last column",
            m.path
        );
    }

    let co = run(RegistrationGeometry::CoRegistered);
    let c = rects(&co);
    assert_eq!(c.len(), 2, "{c:?}");
    assert_eq!(
        (c[0].1, c[0].2, c[0].3, c[0].4),
        (c[1].1, c[1].2, c[1].3, c[1].4),
        "one rectangle for the run"
    );
    assert!(
        c[0].3 > n[0].3.max(n[1].3),
        "the union is wider than either group's own: {c:?} vs {n:?}"
    );
}

/// Plan 6b-3 D10: a crop pass that cannot write never loses a master —
/// the rows and the summary keep naming the scratch files, which stay,
/// and the run says where they are.
#[test]
fn a_failed_crop_keeps_the_masters_in_the_scratch_folder() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let mut cfg = sky_config(&conn);
    cfg.output.crop_to_coverage = 2;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    // A file where the output folder's parent should be: every final
    // write fails at `create_dir_all`.
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"").unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &blocker.join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    run_pipeline(&mut rc).unwrap();

    let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let master = master_row(&rows, "master");
    let scratch = rc.layout.uncropped_dir(rc.run_id);
    assert!(
        Path::new(&master.path).starts_with(&scratch),
        "{}",
        master.path
    );
    assert!(Path::new(&master.path).exists(), "the master survives");
    assert_eq!(master.format, "fits");
    assert_eq!(
        rc.summary.groups[0].master_path.as_deref(),
        Some(master.path.as_str())
    );
    assert!(
        rc.warnings
            .iter()
            .any(|w| w.contains("could not be finalized") && w.contains("-uncropped")),
        "{:?}",
        rc.warnings
    );
}

/// Review I7: a crop whose rectangle is the whole master finalizes
/// through the writer the K = 0 path uses — an XISF master with the same
/// pixels and keywords (all but the run id) — and says nothing about
/// cropping.
#[test]
fn a_full_rectangle_crop_writes_what_the_zero_crop_path_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4]);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    let run_with = |crop: u32| -> (String, RunSummary) {
        let mut cfg = StackingConfig::default();
        cfg.output.format = crate::stacking::config::OutputFormat::Xisf;
        cfg.output.crop_to_coverage = crop;
        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg),
            None,
        )
        .unwrap();
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
        let summary: RunSummary =
            serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
        let rows =
            crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
        (master_row(&rows, "master").path.clone(), summary)
    };
    let (k0, _) = run_with(0);
    let (k1, summary) = run_with(1);
    assert!(k1.ends_with(".xisf") && !k1.contains("-uncropped"), "{k1}");
    let read = |p: &str| astroimage::ImageConverter::read_raw(Path::new(p)).unwrap();
    let ((ma, pa), (mb, pb)) = (read(&k0), read(&k1));
    assert_eq!(
        (ma.width, ma.height, ma.channels),
        (mb.width, mb.height, mb.channels)
    );
    match (pa, pb) {
        (astroimage::PixelData::Float32(a), astroimage::PixelData::Float32(b)) => {
            assert!(
                a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
                "the same pixels"
            );
        }
        _ => panic!("expected Float32 samples"),
    }
    let keys = |p: &str| {
        let mut k = crate::fits_parser::stored_header::parse_stored_header_keys(
            crate::models::FileFormat::XISF,
            &crate::fits_parser::extract_xisf_header(Path::new(p)).unwrap(),
        );
        k.remove("ATH_STKI");
        k
    };
    assert_eq!(keys(&k1), keys(&k0), "the same keywords but the run id");
    assert!(
        summary
            .warnings
            .iter()
            .all(|w| !w.contains("crop") && !w.contains("uncropped")),
        "{:?}",
        summary.warnings
    );
}

/// Plan 6b-3 D10 fix round 1: a crop run cancelled between two groups
/// finalizes the group it already wrote UNCROPPED into the output folder
/// (as a K = 0 run's written master stays), rows and summary point there,
/// the scratch folder is gone and one warning says why.
#[test]
fn a_cancelled_crop_run_finalizes_its_written_groups_uncropped() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_two_group_project(tmp.path());
    let mut cfg = sky_config(&conn);
    cfg.registration.geometry = RegistrationGeometry::Native;
    cfg.output.crop_to_coverage = 3;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    CANCEL_AFTER_NEXT_GROUP.store(true, Ordering::SeqCst);
    let result = run_pipeline(&mut rc);
    CANCEL_AFTER_NEXT_GROUP.store(false, Ordering::SeqCst);
    assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");

    let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let masters: Vec<_> = rows.iter().filter(|m| m.kind == "master").collect();
    assert_eq!(masters.len(), 1, "only the first group wrote: {rows:?}");
    let master = masters[0];
    assert!(Path::new(&master.path).exists(), "{}", master.path);
    assert!(
        Path::new(&master.path).starts_with(tmp.path().join("out")),
        "the master is final, not in scratch: {}",
        master.path
    );
    assert_eq!(master.format, "fits");
    let written = rc
        .summary
        .groups
        .iter()
        .find(|g| g.key == master.group_key)
        .unwrap();
    assert_eq!(written.master_path.as_deref(), Some(master.path.as_str()));
    let groups = crate::db::stacking::list_groups(&conn, rc.run_id).unwrap();
    let row = groups
        .iter()
        .find(|g| g.group_key == master.group_key)
        .unwrap();
    assert_eq!(row.master_path.as_deref(), Some(master.path.as_str()));
    assert_eq!(
        rc.warnings
            .iter()
            .filter(|w| w.contains("did not finish"))
            .count(),
        1,
        "{:?}",
        rc.warnings
    );
    assert!(
        !rc.layout.uncropped_dir(rc.run_id).exists(),
        "the scratch folder is removed"
    );
}

/// Plan 6b-3 D10 fix round 1: a crop that fails is finalized uncropped
/// through the crop writer — the master (full size) is in the output
/// folder, rows and summary point there, the scratch is removed and the
/// run says "written uncropped" exactly once.
#[test]
fn a_failed_crop_is_finalized_uncropped() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let mut cfg = sky_config(&conn);
    cfg.output.crop_to_coverage = 2;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    FORCE_CROP_FAILURE.store(true, Ordering::SeqCst);
    let result = run_pipeline(&mut rc);
    FORCE_CROP_FAILURE.store(false, Ordering::SeqCst);
    result.unwrap();

    let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let master = master_row(&rows, "master");
    assert!(
        Path::new(&master.path).starts_with(tmp.path().join("out")),
        "{}",
        master.path
    );
    assert!(Path::new(&master.path).exists());
    assert_eq!((master.width, master.height), (512, 384), "uncropped");
    assert_eq!(
        rc.summary.groups[0].master_path.as_deref(),
        Some(master.path.as_str())
    );
    assert!(
        !rc.layout.uncropped_dir(rc.run_id).exists(),
        "the scratch folder is removed"
    );
    let said: Vec<_> = rc
        .warnings
        .iter()
        .filter(|w| w.contains("written uncropped"))
        .collect();
    assert_eq!(said.len(), 1, "{:?}", rc.warnings);
    assert!(rc
        .warnings
        .iter()
        .all(|w| !w.contains("could not be finalized")));
}

/// Plan 6b-3 D10 fix round 1: a `K` no pixel reaches (more frames than
/// the group has) is the no-rectangle branch — the master is written
/// uncropped and the run says so once.
#[test]
fn a_crop_no_pixel_reaches_writes_the_master_uncropped() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, conn, rc, _grid) = run_sky_project(tmp.path(), |cfg| {
        cfg.output.crop_to_coverage = 5;
    });
    let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let master = master_row(&rows, "master");
    assert_eq!((master.width, master.height), (512, 384), "uncropped");
    assert!(Path::new(&master.path).exists());
    assert!(
        !Path::new(&master.path).starts_with(rc.layout.uncropped_dir(rc.run_id)),
        "{}",
        master.path
    );
    let said: Vec<_> = rc
        .warnings
        .iter()
        .filter(|w| w.contains("no pixel is covered by 5 frames"))
        .collect();
    assert_eq!(said.len(), 1, "{:?}", rc.warnings);
    assert!(
        !rc.layout.uncropped_dir(rc.run_id).exists(),
        "the scratch folder is removed"
    );
}

/// Final review B2: the crop pass emits an Output-stage tick per group
/// (the first and the closing one are never throttled).
#[test]
fn the_crop_pass_ticks_the_output_stage_per_group() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_two_group_project(tmp.path());
    let mut cfg = sky_config(&conn);
    cfg.registration.geometry = RegistrationGeometry::Native;
    cfg.output.crop_to_coverage = 3;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    let recording = Arc::new(Recording::new());
    rc.emitter = recording.clone();
    run_pipeline(&mut rc).unwrap();
    let ticks: Vec<_> = recording
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["message"] == "cropping")
        .collect();
    assert!(ticks.len() >= 2, "{ticks:?}");
    assert_eq!(ticks[0]["current"], 0);
    assert_eq!(ticks[0]["total"], 2);
    assert!(ticks[0]["groupKey"].is_string(), "{:?}", ticks[0]);
    let last = ticks.last().unwrap();
    assert_eq!(
        (last["current"].as_u64(), last["total"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(last["percent"].as_f64(), Some(100.0));
}

/// Plan 6b-3 D10 fix round 2: a database error while recording one
/// group's crop never deletes the scratch folder its rows may still name;
/// the warning names the folder and the group, and the other group is
/// finalized.
#[test]
fn a_record_failure_keeps_the_crop_scratch_folder() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_two_group_project(tmp.path());
    let mut cfg = sky_config(&conn);
    cfg.registration.geometry = RegistrationGeometry::Native;
    cfg.output.crop_to_coverage = 3;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    FORCE_RECORD_FAILURE.store(true, Ordering::SeqCst);
    let result = run_pipeline(&mut rc);
    FORCE_RECORD_FAILURE.store(false, Ordering::SeqCst);
    assert!(result.is_err(), "a database error fails the run");

    let scratch = rc.layout.uncropped_dir(rc.run_id);
    assert!(scratch.exists(), "the scratch folder survives");
    let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let masters: Vec<_> = rows.iter().filter(|m| m.kind == "master").collect();
    assert_eq!(masters.len(), 2, "{rows:?}");
    let (in_scratch, finalized): (Vec<&&crate::db::stacking::MasterLight>, Vec<_>) = masters
        .iter()
        .partition(|m| Path::new(&m.path).starts_with(&scratch));
    assert_eq!(
        in_scratch.len(),
        1,
        "the failed group's rows still name scratch"
    );
    assert_eq!(finalized.len(), 1, "the other group is finalized");
    assert!(
        Path::new(&in_scratch[0].path).exists(),
        "its master survives"
    );
    assert!(Path::new(&finalized[0].path).starts_with(tmp.path().join("out")));
    let named = in_scratch[0].group_key.as_str();
    assert!(
        rc.warnings
            .iter()
            .any(|w| w.contains(named) && w.contains(&scratch.display().to_string())),
        "{:?}",
        rc.warnings
    );
    // Final review B3: the failed group's just-written final files are
    // gone again, so the output folder holds only the other group's.
    assert!(
        rc.warnings
            .iter()
            .any(|w| w.contains(named) && w.contains("final files were removed")),
        "{:?}",
        rc.warnings
    );
    let out_dir = tmp.path().join("out");
    let mut left: Vec<PathBuf> = Vec::new();
    let mut stack = vec![out_dir.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "fits" || x == "xisf") {
                left.push(p);
            }
        }
    }
    let owned: std::collections::HashSet<PathBuf> = rows
        .iter()
        .filter(|m| Path::new(&m.path).starts_with(&out_dir))
        .map(|m| PathBuf::from(&m.path))
        .collect();
    assert!(!left.is_empty(), "the other group's files exist");
    let orphans: Vec<_> = left.iter().filter(|p| !owned.contains(*p)).collect();
    assert!(
        orphans.is_empty(),
        "every master file in the output folder is named by a row: {orphans:?}"
    );
}

/// Final review: a failure AFTER the group row was updated rolls the whole
/// item back, so no row names a final file the caller then removes.
#[test]
fn a_record_failure_after_the_group_row_rolls_the_item_back() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_two_group_project(tmp.path());
    let mut cfg = sky_config(&conn);
    cfg.registration.geometry = RegistrationGeometry::Native;
    cfg.output.crop_to_coverage = 3;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    FORCE_RECORD_FAILURE_AFTER_GROUP.store(true, Ordering::SeqCst);
    let result = run_pipeline(&mut rc);
    FORCE_RECORD_FAILURE_AFTER_GROUP.store(false, Ordering::SeqCst);
    assert!(result.is_err(), "a database error fails the run");

    let scratch = rc.layout.uncropped_dir(rc.run_id);
    assert!(scratch.exists(), "the scratch folder survives");
    let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let masters: Vec<_> = rows.iter().filter(|m| m.kind == "master").collect();
    let failed = masters
        .iter()
        .find(|m| Path::new(&m.path).starts_with(&scratch))
        .expect("the failed group's master row still names scratch");
    // Every row names a file that exists (none names a removed final file).
    for m in &rows {
        assert!(
            Path::new(&m.path).exists(),
            "row names a missing file: {m:?}"
        );
    }
    // The group rows agree: each group's master_path is a file on disk.
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stacking_run_groups WHERE run_id = ?1 AND master_path IS NOT NULL",
            [rc.run_id],
            |r| r.get(0),
        )
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT group_key, master_path FROM stacking_run_groups WHERE run_id = ?1 AND master_path IS NOT NULL")
        .unwrap();
    let groups: Vec<(String, String)> = stmt
        .query_map([rc.run_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(groups.len() as i64, n);
    for (key, p) in &groups {
        assert!(
            Path::new(p).exists(),
            "group {key} names a missing file {p}"
        );
        if *key == failed.group_key {
            assert!(
                Path::new(p).starts_with(&scratch),
                "the failed group still names scratch: {p}"
            );
        }
    }
}
