//! Spec §15.2, plan 6b-3 Task 9: a collaboration project stacked end to end
//! on the processor's machine, through the fake hub — two publishers, two
//! canonical filters, a coordinator exclusion, a frame the run plate-solves,
//! two members under 25 % overlap, the grid confirmed through the real
//! command, and a second run that solves nothing.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::api::collab_exchange::test_support::{test_ctx, wire_hub};
use crate::api::PathPolicy;
use crate::collab::fake_hub::FakeHub;
use crate::db::stacking::{StackSubject, StackingRunRow};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::stacking::plan::Stage;
use crate::stacking::solve::{FakeSolver, SolveOutcome};
use crate::stacking::test_fixtures::{
    install_stand_in_catalog, render_sky_frame, sky_frame, sky_frame_record, sky_stars,
    ProjectFile, EAST_PAIR_OFFSET_WIDTHS, EAST_PAIR_SPACING_ARCSEC, EAST_PAIR_STAR_SEED,
};

const TARGET: (f64, f64) = (83.8, -5.4);

struct Rec(Mutex<Vec<(String, serde_json::Value)>>);
impl ProgressEmitter for Rec {
    fn emit_json(&self, name: &str, payload: serde_json::Value) {
        self.0.lock().unwrap().push((name.to_string(), payload));
    }
}

fn db_conn(ctx: &ServiceContext) -> r2d2::PooledConnection<crate::db::SqliteConnectionManager> {
    crate::api::db(ctx).unwrap().conn()
}

fn count(ctx: &ServiceContext, table: &str) -> i64 {
    db_conn(ctx)
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// The catalog tables a project run must never write.
fn catalog_rows(ctx: &ServiceContext) -> (i64, i64, i64) {
    (
        count(ctx, "frames"),
        count(ctx, "plate_solves"),
        count(ctx, "registration_results"),
    )
}

/// Waits for the run's terminal row and for its handle to leave.
async fn finished(ctx: &ServiceContext, run_id: i64) -> StackingRunRow {
    // Two runs on the test context's one image-pool thread under full-suite
    // load (review M11): generous, never the expected duration.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let row = crate::db::stacking::get_run(&db_conn(ctx), run_id)
            .unwrap()
            .unwrap();
        let live = ctx.active_stacks.lock().unwrap().contains_key(&run_id);
        if matches!(row.status.as_str(), "done" | "failed" | "cancelled") && !live {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} still {}",
            row.status
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn wcs_of(header: &crate::fits_parser::FitsHeader) -> solvemyastro::wcs::WcsSolution {
    let f = |k: &str| header.get_f64(k).unwrap_or_else(|| panic!("{k} missing"));
    solvemyastro::wcs::WcsSolution {
        crpix: (f("CRPIX1") - 1.0, f("CRPIX2") - 1.0),
        crval: (f("CRVAL1"), f("CRVAL2")),
        cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
        sip_forward: None,
        sip_reverse: None,
    }
}

/// One frame: rendered under `collab/<publisher>/`, published on the hub,
/// and (after the manifest sync) landed.
struct Planned {
    uuid: &'static str,
    account: &'static str,
    publisher: &'static str,
    raw: &'static str,
    canonical: &'static str,
    centre: (f64, f64),
    manifest_wcs: bool,
    scale_arcsec: f64,
    width: usize,
    height: usize,
    osc: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_stacks_end_to_end_through_the_fake_hub() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let hub = FakeHub::start().await;
    let (tmp, ctx) = test_ctx();
    wire_hub(&ctx, &hub.uri(), "tok");
    let b64 = base64::engine::general_purpose::STANDARD;
    hub.add_account("tok", "acc-me", "Me", &b64.encode([7u8; 32]), None);
    hub.add_account("tok-a", "acc-a", "Ann", &b64.encode([8u8; 32]), None);
    hub.add_account("tok-b", "acc-b", "Bob", &b64.encode([9u8; 32]), None);
    hub.add_account("tok-c", "acc-c", "Carol", &b64.encode([10u8; 32]), None);
    hub.add_project(
        "p1",
        "m42",
        &[
            ("acc-me", "send_receive", true),
            ("acc-a", "send_receive", false),
            ("acc-b", "send_receive", false),
            ("acc-c", "send_receive", false),
        ],
        false,
    );
    hub.set_target("p1", "M42", TARGET.0, TARGET.1, 1.0);

    let east = EAST_PAIR_OFFSET_WIDTHS * 384.0 * 1.5 / 3600.0 / TARGET.1.to_radians().cos();
    // The run tests' dithers (±3.6 px at most): the LN field's 0.97 rule
    // must keep every central L member (D8, review I6).
    let near = |i: f64| (TARGET.0 + (i - 1.5) * 0.001, TARGET.1 + (i % 2.0) * 0.0005);
    let frame = |uuid, account, publisher, raw, canonical, centre, manifest_wcs| Planned {
        uuid,
        account,
        publisher,
        raw,
        canonical,
        centre,
        manifest_wcs,
        scale_arcsec: 1.5,
        width: 384,
        height: 288,
        osc: false,
    };
    // Carol shoots at 2.0"/px on a 320 x 240 sensor (a wider and a taller
    // field than Ann's and Bob's): two mono frames join the L group, and
    // three OSC replicas (debayered, RGGB) make a group of their own.
    let coarse = |p: Planned| Planned {
        scale_arcsec: 2.0,
        width: 320,
        height: 240,
        ..p
    };
    let carol = |uuid, raw, canonical, centre, osc| {
        coarse(Planned {
            osc,
            ..frame(uuid, "acc-c", "Carol", raw, canonical, centre, true)
        })
    };
    let planned = vec![
        frame("a0", "acc-a", "Ann", "Luminance", "L", near(0.0), true),
        frame("a1", "acc-a", "Ann", "Luminance", "L", near(1.0), true),
        frame("a2", "acc-a", "Ann", "Luminance", "L", near(2.0), true),
        frame("a3", "acc-a", "Ann", "Luminance", "L", near(3.0), true),
        frame("r0", "acc-a", "Ann", "Red", "R", near(0.5), true),
        frame("r1", "acc-a", "Ann", "Red", "R", near(1.5), true),
        frame("r2", "acc-a", "Ann", "Red", "R", near(2.5), true),
        frame("b0", "acc-b", "Bob", "Lum", "L", near(0.25), true),
        frame("b1", "acc-b", "Bob", "Lum", "L", near(2.75), true),
        frame("b2", "acc-b", "Bob", "Lum", "L", TARGET, false),
        frame(
            "e0",
            "acc-b",
            "Bob",
            "Lum",
            "L",
            (TARGET.0 + east, TARGET.1),
            true,
        ),
        frame(
            "e1",
            "acc-b",
            "Bob",
            "Lum",
            "L",
            (TARGET.0 + east + 0.002, TARGET.1 + 0.001),
            true,
        ),
        carol("c0", "Lum", "L", near(1.0), false),
        carol("c1", "Lum", "L", near(3.0), false),
        carol("o0", "LPro", "LPRO", near(0.5), true),
        carol("o1", "LPro", "LPRO", near(1.5), true),
        carol("o2", "LPro", "LPRO", near(2.5), true),
    ];
    // Task 5b's star field, constant for constant (review I5): the east pair
    // registers over its ≈ 15 % strip there first, a hundred times cheaper.
    let stars = sky_stars(
        (TARGET.0 + east / 2.0, TARGET.1),
        0.2,
        EAST_PAIR_SPACING_ARCSEC,
        EAST_PAIR_STAR_SEED,
    );
    let collab = tmp.path().join("collab");
    let mut b2_record = None;
    let mut rendered = Vec::new();
    for (i, p) in planned.iter().enumerate() {
        let mut f = sky_frame(p.uuid, p.centre);
        f.publisher = p.publisher;
        f.filter = p.raw;
        f.manifest_wcs = p.manifest_wcs;
        f.scale_arcsec = p.scale_arcsec;
        f.width = p.width;
        f.height = p.height;
        if p.osc {
            f.file = ProjectFile::Rgb32;
            f.bayerpat = Some("RGGB");
        }
        f.noise_seed = 500 + i as u64;
        let r = render_sky_frame(&collab, "p1", &f, &stars);
        if p.uuid == "b2" {
            b2_record = Some((r.file_name.clone(), sky_frame_record(&f)));
        }
        let (meta, file_name, blake3, size) = (
            r.meta.clone(),
            r.file_name.clone(),
            r.blake3.clone(),
            r.byte_size,
        );
        let (raw, canonical) = (p.raw.to_string(), p.canonical.to_string());
        let channel = if p.osc { "osc" } else { "mono" };
        hub.seed_frames_with("p1", p.account, &[p.uuid], "published", move |w| {
            w.meta = meta.clone();
            w.file_name = file_name.clone();
            w.blake3 = blake3.clone();
            w.byte_size = size;
            w.xxh3 = "0123456789abcdef".to_string();
            w.filter_raw = raw.clone();
            w.filter_canonical = canonical.clone();
            w.channel = channel.to_string();
            w.exptime_sec = 300.0;
            w.date_obs = Some("2026-01-10T21:00:00".to_string());
        });
        rendered.push((p.uuid, r.path));
    }
    crate::api::collab::refresh_projects(&ctx).await.unwrap();
    crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None)
        .await
        .unwrap();
    {
        let conn = db_conn(&ctx);
        for (uuid, path) in &rendered {
            let stamp =
                crate::api::collab_exchange::size_mtime_from(&std::fs::metadata(path).unwrap());
            crate::db::collab_frames::set_landed(
                &conn,
                "p1",
                uuid,
                &path.to_string_lossy(),
                &stamp,
            )
            .unwrap();
        }
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
    }
    // An empty stand-in catalog, as the plan tests install: with no catalog
    // the plan cannot tell a frame to solve from one that cannot be solved.
    let identity = {
        let conn = db_conn(&ctx);
        let app_data = Path::new(conn.path().unwrap())
            .parent()
            .unwrap()
            .to_path_buf();
        install_stand_in_catalog(&app_data)
    };
    let ctx = Arc::new(ctx);
    let before = catalog_rows(&ctx);

    // The first plan: the grid is not confirmed yet, the proposal is there,
    // and the frame without a manifest WCS will be solved.
    let first =
        crate::api::stacking::get_project_stacking_plan(&ctx, &PathPolicy::AllowAll, "p1", None)
            .unwrap();
    assert!(
        first.blockers.iter().any(|b| b.code == "grid"),
        "{:?}",
        first.blockers
    );
    assert!(
        first
            .warnings
            .iter()
            .any(|w| w.contains("will be plate-solved locally")),
        "{:?}",
        first.warnings
    );
    let proposal = first
        .grid
        .as_ref()
        .and_then(|g| g.proposal)
        .expect("a proposal");
    let grid = crate::api::collab_grid::set_collab_project_grid(&ctx, "p1", proposal, None)
        .await
        .unwrap();
    crate::api::collab::exclude_collab_frame(&ctx, "p1", "a3", "trailed".into())
        .await
        .unwrap();

    let mut cfg = crate::api::stacking::get_project_stacking_config(&ctx, "p1")
        .unwrap()
        .config;
    cfg.normalization.local.enabled = true;
    cfg.reference.two_pass = false;
    crate::api::stacking::set_project_stacking_config(&ctx, "p1", cfg, Vec::new(), None).unwrap();

    let ready =
        crate::api::stacking::get_project_stacking_plan(&ctx, &PathPolicy::AllowAll, "p1", None)
            .unwrap();
    assert!(ready.blockers.is_empty(), "{:?}", ready.blockers);
    assert!(
        ready.project_frames.iter().all(|f| f.frame_uuid != "a3"),
        "the excluded frame leaves the plan"
    );
    assert_eq!(ready.grid.as_ref().and_then(|g| g.current), Some(grid));

    // The run.
    let (b2_name, b2_solve) = b2_record.unwrap();
    let solver = Arc::new(
        FakeSolver::solving_all()
            .with_identity(&identity)
            .with(&b2_name, SolveOutcome::Solved(b2_solve)),
    );
    let events = Arc::new(Rec(Mutex::new(Vec::new())));
    let started = crate::stacking::run::start_stacking_with_solver(
        ctx.clone(),
        events.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
        solver.clone(),
    )
    .unwrap();
    let row = finished(&ctx, started.run_id).await;
    assert_eq!(row.status, "done", "{row:?}");
    assert_eq!(solver.calls(), 1, "the one frame without a WCS is solved");
    let solved: Vec<_> = solver
        .paths()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        solved,
        vec![b2_name.clone()],
        "the solver was asked for b2 only"
    );

    let summary: crate::stacking::provenance::RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary.grid, Some(grid));
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.contains("kept 2 frame(s) on global normalization")),
        "{:?}",
        summary.warnings
    );
    assert_eq!(
        summary
            .warnings
            .iter()
            .filter(|w| w.contains("global normalization"))
            .count(),
        1,
        "one warning, for the L group only: {:?}",
        summary.warnings
    );
    let globals = crate::db::stacking::list_artifacts(
        &db_conn(&ctx),
        &StackSubject::Project("p1".into()),
        Some(crate::stacking::run::LN_GLOBAL_ARTIFACT_KIND),
    )
    .unwrap();
    assert_eq!(globals.len(), 2, "one ln_global artifact per east frame");
    let completes: Vec<serde_json::Value> = events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _)| n == crate::stacking::run::STACKING_COMPLETE_EVENT)
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(completes.len(), 1);
    assert_eq!(completes[0]["projectId"], "p1");
    assert!(completes[0]["setId"].is_null());

    // The masters: grid size, the canonical filter, coverage that counts the
    // footprints. L: 6 central members of Ann and Bob plus Carol's 2 cover the
    // target (8); the strip the east pair shares with them has all 10. R: 3
    // everywhere they overlap. LPRO (OSC, 3 channels): 3.
    let lights = crate::db::stacking::list_master_lights(&db_conn(&ctx), started.run_id).unwrap();
    let masters: Vec<_> = lights.iter().filter(|m| m.kind == "master").collect();
    assert_eq!(masters.len(), 3, "{lights:?}");
    // A project run writes into `<output>/<project title>/` (the title is the
    // hub's slug `m42`, upper-cased, here), under the canonical spelling of
    // the root.
    let out_dir = crate::test_support::canonical_path(&tmp.path().join("out")).join("M42");
    assert_eq!(Path::new(&row.output_dir), out_dir);
    let mut filters = Vec::new();
    for m in &masters {
        assert_eq!((m.width, m.height), (grid.width_px, grid.height_px));
        assert!(Path::new(&m.path).starts_with(&out_dir), "{}", m.path);
        let header = crate::fits_parser::FitsHeader::from_path(Path::new(&m.path)).unwrap();
        let filter = header.get_str("FILTER").unwrap();
        assert_eq!(
            header.get_str("ATH_GRID").as_deref(),
            Some(grid.hash().as_str())
        );
        assert_eq!(header.get_str("ATH_SPRJ").as_deref(), Some("p1"));
        assert_eq!(header.get_str("ATH_RGEO").as_deref(), Some("grid"));
        let channels = crate::integration::plane_reader::PlaneReader::open(Path::new(&m.path))
            .unwrap()
            .channels();
        let coverage = lights
            .iter()
            .find(|c| c.kind == "coverage" && c.group_key == m.group_key)
            .expect("a coverage map per group");
        let reader =
            crate::integration::plane_reader::PlaneReader::open(Path::new(&coverage.path)).unwrap();
        let plane = reader.read_plane(0).unwrap();
        let (x, y) = wcs_of(&header).sky_to_pixel(TARGET.0, TARGET.1);
        let at_target = plane[(y.round() as usize) * reader.width() + x.round() as usize];
        let max = plane.iter().cloned().fold(0f32, f32::max);
        match filter.as_str() {
            "L" => {
                assert_eq!((at_target, max, channels), (8.0, 10.0, 1), "L coverage");
            }
            "R" => {
                assert_eq!((at_target, max, channels), (3.0, 3.0, 1), "R coverage");
            }
            "LPRO" => {
                assert_eq!((at_target, max, channels), (3.0, 3.0, 3), "LPRO coverage");
            }
            other => panic!("a master with FILTER {other:?}: the raw name leaked"),
        }
        filters.push(filter);
    }
    filters.sort();
    assert_eq!(
        filters,
        vec!["L".to_string(), "LPRO".to_string(), "R".to_string()]
    );

    // Run history names uuids and never the excluded frame; the catalog is
    // untouched; nothing written carries ATH_PRJ.
    let detail = crate::api::stacking::get_stacking_run(&ctx, started.run_id).unwrap();
    assert_eq!(
        detail.frames.len(),
        16,
        "17 published minus the excluded one"
    );
    assert!(detail
        .frames
        .iter()
        .all(|f| f.frame_uuid.as_deref() != Some("a3")));
    assert!(detail.frames.iter().all(|f| f.frame_uuid.is_some()));
    assert!(
        detail.frames.iter().all(|f| f.included),
        "every frame registers and stacks: {:?}",
        detail
            .frames
            .iter()
            .filter(|f| !f.included)
            .map(|f| (&f.frame_uuid, &f.exclusion_reason, &f.reg_status))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        catalog_rows(&ctx),
        before,
        "frames, plate_solves, registration_results untouched"
    );
    for root in [tmp.path().join("work"), tmp.path().join("out")] {
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("fits") {
                let header = crate::fits_parser::FitsHeader::from_path(entry.path()).unwrap();
                assert!(
                    header.get_str("ATH_PRJ").is_none(),
                    "{} carries ATH_PRJ",
                    entry.path().display()
                );
            }
        }
    }

    // The plan between the runs agrees with the first run: nothing left to
    // solve (the identity bridge holds) and no stage the run built is stale.
    let between =
        crate::api::stacking::get_project_stacking_plan(&ctx, &PathPolicy::AllowAll, "p1", None)
            .unwrap();
    assert!(
        between
            .warnings
            .iter()
            .all(|w| !w.contains("plate-solved locally")),
        "{:?}",
        between.warnings
    );
    for stage in [Stage::Register, Stage::Normalize] {
        assert!(
            !between.stale_stages.contains(&stage),
            "{stage:?} stale after the run: {:?}",
            between.stale_stages
        );
    }

    // A second run solves nothing and finishes.
    let again = crate::stacking::run::start_stacking_with_solver(
        ctx.clone(),
        Arc::new(crate::events::NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
        solver.clone(),
    )
    .unwrap();
    assert_eq!(finished(&ctx, again.run_id).await.status, "done");
    assert_eq!(solver.calls(), 1, "the cached solve answers the second run");

    // Cleanup keeps the landed replicas byte for byte.
    let landed_before: Vec<Vec<u8>> = rendered
        .iter()
        .map(|(_, p)| std::fs::read(p).unwrap())
        .collect();
    crate::api::stacking::cleanup_project_stacking_work(
        &ctx,
        "p1",
        crate::stacking::paths::CleanupWhat::All,
    )
    .unwrap();
    for ((uuid, path), bytes) in rendered.iter().zip(&landed_before) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{uuid} was touched");
    }
}
