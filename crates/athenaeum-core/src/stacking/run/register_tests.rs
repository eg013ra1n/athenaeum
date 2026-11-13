//! Tests of the register stage; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::registration::db::get_registration_for_frame_set;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, shifted_stars, star_light_spec, wait_for_run, LightSpec,
    STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

/// Robust quad seed RQ-6: one `warn!` per frame under half its group's median inliers, carrying the median and
/// the seed's model; a group with every frame at or over half logs nothing.
#[test]
fn a_low_inlier_frame_is_warned_once_with_its_group_median() {
    let counts = [(1, 1_600), (2, 1_750), (3, 1_900), (4, 87), (5, 1_650), (6, 1_700)];
    let rows: Vec<(i64, usize, String)> = counts
        .into_iter()
        .map(|(id, inliers)| {
            let model = if id == 4 { "homography+wcs" } else { "homography" };
            (id, inliers, model.to_string())
        })
        .collect();
    let ((), text) =
        crate::stacking::telemetry::capture_logs(|| warn_low_inlier_frames(41, "g-low", &rows));
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("registration kept under half its group's median inliers"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    for field in [
        "WARN",
        "run_id=41",
        "group_key=g-low",
        "frame_id=4",
        "inliers=87",
        "median_inliers=1675",
        "model=homography+wcs",
    ] {
        assert!(lines[0].contains(field), "{field} missing in {}", lines[0]);
    }

    let fine: Vec<(i64, usize, String)> = rows
        .iter()
        .map(|(id, _, m)| (*id, 1_700, m.clone()))
        .collect();
    let ((), quiet) =
        crate::stacking::telemetry::capture_logs(|| warn_low_inlier_frames(41, "g-low", &fine));
    assert!(!quiet.contains("registration kept under half"), "{quiet}");
}

/// The rows the diagnostic reads (RQ-6): aligned, non-reference frames, cached rows included; a failed frame, a
/// frame with no outcome and the reference are not part of the median.
#[test]
fn the_low_inlier_rows_are_the_aligned_non_reference_frames() {
    let aligned = |id: i64,
                   inliers: i64,
                   is_reference: bool,
                   cached: bool,
                   model: Option<&str>| {
        let mut e = bare_measured_frame(id);
        e.registration = Some(RegisteredFrameOutcome::Aligned {
            map: PixelMap::linear(Linear::identity()).expect("identity map"),
            record: RegistrationRecord {
                frame_id: id,
                is_reference,
                matched_stars: inliers,
                model: model.map(str::to_string),
                ..Default::default()
            },
            cached,
            registered_hash: String::new(),
        });
        e
    };
    let mut failed = bare_measured_frame(5);
    failed.registration = Some(RegisteredFrameOutcome::Failed("registration failed".into()));
    let entries = vec![
        aligned(1, 0, true, false, None),
        aligned(2, 1_700, false, true, Some("affine")),
        aligned(3, 87, false, false, Some("homography+wcs")),
        failed,
        bare_measured_frame(6),
        aligned(7, 1_650, false, false, None),
    ];
    assert_eq!(
        low_inlier_rows(&entries),
        vec![
            (2, 1_700, "affine".to_string()),
            (3, 87, "homography+wcs".to_string()),
            (7, 1_650, String::new()),
        ]
    );
}

/// Robust quad seed RQ-6, review r1 I1: "after the persisting registration pass, cached rows included". A group
/// whose every included frame is a cache hit registers nothing and returns early, yet its low-inlier frame is
/// still named: the second stage over unchanged rows warns for the frame whose stored row kept 20 inliers.
#[test]
fn a_fully_cached_group_still_warns_for_its_low_inlier_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = StackingConfig::default();
    cfg.reference.two_pass = false;
    let (_ctx, fixture, _light_ids, mut rc, _working, _output) = two_pass_context(&tmp, cfg);

    stage_register(&mut rc).unwrap();

    // The stored rows stay fresh (same hash, same reference): only the inlier counts move.
    let mut rows = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    rows.retain(|r| !r.is_reference);
    assert!(rows.len() >= 3, "premise: a median needs three frames: {rows:?}");
    let low_frame = rows[0].frame_id;
    for (i, row) in rows.iter_mut().enumerate() {
        row.matched_stars = if i == 0 { 20 } else { 1_000 };
        crate::registration::db::upsert_registration(&fixture.conn, row).unwrap();
    }

    let ((), text) = crate::stacking::telemetry::capture_logs(|| stage_register(&mut rc).unwrap());

    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("registration kept under half its group's median inliers"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    for field in [
        "WARN".to_string(),
        format!("run_id={}", rc.run_id),
        format!("frame_id={low_frame}"),
        "inliers=20".to_string(),
        "median_inliers=1000".to_string(),
    ] {
        assert!(lines[0].contains(&field), "{field} missing in {}", lines[0]);
    }
    // The premise: the second stage found every frame in the cache, so it registered nothing and returned early.
    assert!(
        rc.measured.values().flatten().filter(|e| e.included).all(|e| matches!(
            &e.registration,
            Some(RegisteredFrameOutcome::Aligned { cached: true, .. })
        )),
        "every included frame must be a cache hit"
    );
}

#[test]
fn registered_file_name_does_not_misdetect_a_source_name_ending_in_d() {
    // Fix round 1 addendum: a mono source light named "vega_d" must not
    // be mistaken for a debayered output just because its own stem
    // happens to end in "_d" — `debayered` is now an explicit fact the
    // caller passes (`MeasuredFrame::planes == 3`), never inferred from
    // the calibrated file's own name.
    assert_eq!(registered_file_name("vega_d", false), "r_vega_d.fits");
    assert_eq!(registered_file_name("vega_d", true), "r_vega_d_d.fits");
}

/// Perf tier A Task 6a, Pin 3: a registered artifact deleted strictly
/// BETWEEN stage 5 (Register, which just wrote it) and stage 7
/// (Integrate, which reads it) must not fail the run — every affected
/// frame falls back to the on-the-fly warp (counted via
/// `RegisteredSource`'s test-only `fallback_counters`, since asserting
/// the paired `warn!` line would need a tracing subscriber this test
/// binary cannot own exclusively, see `open_materialized`'s own doc),
/// and the resulting master is bit-for-bit identical to a baseline
/// run's own — the master is unchanged either way, by construction (Pin
/// 1 already proved the writer and the on-the-fly warp agree pixel for
/// pixel; this pin proves the RUN actually reaches that fallback path
/// and does not fail or diverge when it does).
#[test]
fn a_registered_frame_deleted_mid_run_falls_back_and_the_master_is_unchanged() {
    use crate::integration::registered_source::fallback_counters;

    // Fix round 2, ruling R-TA-6 M7: the counter is process-global and
    // the harness runs tests in parallel — without this guard another
    // test's own fault injection could land between this test's
    // `reset()` and `snapshot()` and inflate its count.
    let _fallback_guard = fallback_counters::exclusive();

    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];

    // `delete_registered`: when true, every included frame's `registered/`
    // artifact is removed right after stage 5 (Register) finishes and
    // before `stage_output` (stages 6-9) runs — the exact "between
    // stage 5 and 6" window this pin is about.
    let run_once = |delete_registered: bool| -> (usize, Vec<Vec<f32>>, usize) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
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
            plan_groups.clone(),
            layout,
            output.path().to_path_buf(),
            group_ids,
        );
        run_stages_for_test(&mut rc, Stage::Register).unwrap();

        if delete_registered {
            let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
            let dir = layout.registered_dir();
            assert!(
                dir.exists() && std::fs::read_dir(&dir).unwrap().next().is_some(),
                "stage 5 must have written at least one registered file at {}",
                dir.display()
            );
            std::fs::remove_dir_all(&dir)
                .expect("remove registered/ for the fault injection");
        }

        fallback_counters::reset();
        stage_output(&mut rc).unwrap();
        let fallbacks = fallback_counters::snapshot();

        let master = rc
            .summary
            .groups
            .iter()
            .find(|g| g.key == plan_groups[0].key)
            .and_then(|g| g.master_path.clone())
            .expect("master path");
        // Read the master's pixel data back NOW, while `working`/`output`
        // (this closure's own `TempDir`s) are still alive — both are
        // dropped (deleting everything under them, master included) the
        // moment this closure returns, so a caller comparing by PATH
        // after the fact would be reading a file that no longer exists.
        let reader = PlaneReader::open(Path::new(&master)).unwrap();
        let channels = reader.channels();
        let planes: Vec<Vec<f32>> = (0..channels)
            .map(|p| reader.read_plane(p).unwrap())
            .collect();
        (channels, planes, fallbacks)
    };

    let (channels_baseline, planes_baseline, _fallbacks_baseline) = run_once(false);
    let (channels_faulted, planes_faulted, fallbacks_faulted) = run_once(true);

    assert!(
        fallbacks_faulted > 0,
        "deleting every registered artifact must make Integrate fall back \
             at least once (mono group, one plane, four included frames)"
    );

    assert_eq!(channels_baseline, channels_faulted);
    for p in 0..channels_baseline {
        let plane_a = &planes_baseline[p];
        let plane_b = &planes_faulted[p];
        assert_eq!(plane_a.len(), plane_b.len());
        for (i, (a, b)) in plane_a.iter().zip(plane_b.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "plane {p} pixel {i}: baseline {a} vs registered-file-deleted-mid-run {b}"
            );
        }
    }
}

/// A failed registered-frame write is visible: ONE run warning per group
/// (count + first error), never one per frame, and a run whose writes
/// succeed emits none. The failure is injected with a read-only
/// `registered/<group>` directory.
#[cfg(unix)]
#[test]
fn an_unwritable_registered_dir_pushes_one_warning_per_group() {
    use std::os::unix::fs::PermissionsExt;

    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let run_once = |unwritable: bool| -> Vec<String> {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
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
        let reg_dirs: Vec<PathBuf> = vec![layout.registered_dir()];
        if unwritable {
            for d in &reg_dirs {
                std::fs::create_dir_all(d).unwrap();
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o555)).unwrap();
            }
        }
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups.clone(),
            layout,
            output.path().to_path_buf(),
            group_ids,
        );
        let res = run_stages_for_test(&mut rc, Stage::Register);
        // Let the tempdir clean up.
        for d in &reg_dirs {
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
        }
        res.unwrap();
        rc.warnings
            .iter()
            .filter(|w| w.contains("registered frames could not be written"))
            .cloned()
            .collect()
    };

    assert!(run_once(false).is_empty(), "a clean write emits no warning");
    let warnings = run_once(true);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("for 4 frames of")
            && warnings[0].contains("; they are warped on the fly"),
        "{}",
        warnings[0]
    );
}

/// Fix round 2, ruling R-TA-6 I2: a `registered` artifact deleted AFTER
/// a finished run (the same `registered/<group>` directory Pin 3 above
/// faults mid-run) must make the NEXT plan call Register stale, not
/// falsely report it cached. Before this fix `compute_register_stale`
/// only checked the `registration_results` row (the alignment itself),
/// never the on-disk artifact Task 6a's writer keeps beside it — so an
/// existing catalog whose registered files went missing (a cleanup, or
/// simply an upgrade to a build that writes them for the first time)
/// reported Register as cached while a run would actually warp and
/// write every included frame.
#[test]
fn register_stale_reacts_to_a_deleted_registered_artifact() {
    use crate::stacking::plan::build_plan;

    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);
    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let plan_before = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg.clone()),
    )
    .expect("the plan should build");
    assert!(
        !plan_before.stale_stages.contains(&Stage::Register),
        "a finished run's own registered artifacts must read fresh: {:?}",
        plan_before.stale_stages
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let dir = layout.registered_dir();
    assert!(
        dir.exists() && std::fs::read_dir(&dir).unwrap().next().is_some(),
        "the run must have written at least one registered file at {}",
        dir.display()
    );
    std::fs::remove_dir_all(&dir).expect("remove registered/ for the fault injection");

    let plan_after = build_plan(
        &fixture.conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
    )
    .expect("the plan should build");
    assert!(
        plan_after.stale_stages.contains(&Stage::Register),
        "a registered artifact deleted after the run must make Register stale, not \
             falsely cached: {:?}",
        plan_after.stale_stages
    );
}

/// Frame-level cache spec §5.1 (Task 5): four mono frames of `seed_star_group` registered onto a manual
/// reference, with what the registered-artifact tests count.
struct RegisterRig {
    _tmp: tempfile::TempDir,
    ctx: Arc<ServiceContext>,
    fixture: test_fixtures::Fixture,
    light_ids: Vec<i64>,
    working: tempfile::TempDir,
    output: tempfile::TempDir,
    layout: WorkingLayout,
}

/// Cancels the run on the last Register tick: every frame's aligned file is written by then, and the controller's
/// `check_cancel` after the fan-out returns before it records any of them.
struct CancelWhenRegisterDone {
    cancel: Arc<AtomicBool>,
}

impl ProgressEmitter for CancelWhenRegisterDone {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        let current = payload["current"].as_u64().unwrap_or(0);
        if event_name == STACKING_PROGRESS_EVENT
            && payload["stage"] == Stage::Register.as_str()
            && current > 0
            && current == payload["total"].as_u64().unwrap_or(u64::MAX)
        {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
}

impl RegisterRig {
    fn new() -> RegisterRig {
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
        crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, light_ids[0])
            .unwrap();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        RegisterRig { _tmp: tmp, ctx, fixture, light_ids, working, output, layout }
    }

    /// The config of every run: a manual reference, so a regroup cannot move it.
    fn cfg(&self, mode: crate::stacking::config::ExposureMode) -> StackingConfig {
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        cfg.grouping.exposure_mode = mode;
        cfg
    }

    /// One run through Register under `cfg`'s grouping.
    fn run(&self, cfg: &StackingConfig) -> RunContext {
        let (rc, res) = self.run_with(cfg, false);
        res.unwrap();
        rc
    }

    /// One run through Register; `cancel_after_register` cancels it right after the Register fan-out wrote its files.
    fn run_with(&self, cfg: &StackingConfig, cancel_after_register: bool) -> (RunContext, Result<(), RunError>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter: Arc<dyn ProgressEmitter> = if cancel_after_register {
            Arc::new(CancelWhenRegisterDone { cancel: cancel.clone() })
        } else {
            Arc::new(NullEmitter)
        };
        let plan_groups = group_frames(&self.fixture.conn, self.fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
        let (run_id, group_ids) = seed_run_and_groups(
            &self.fixture.conn,
            self.fixture.set_id,
            &plan_groups,
            self.working.path(),
            self.output.path(),
        );
        let mut rc = test_context(
            self.ctx.clone(),
            emitter,
            run_id,
            self.fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups,
            self.layout.clone(),
            self.output.path().to_path_buf(),
            group_ids,
        );
        rc.cancel = cancel;
        let res = run_stages_for_test(&mut rc, Stage::Register);
        (rc, res)
    }

    fn registered_rows(&self) -> HashMap<i64, crate::db::stacking::StackingArtifactRow> {
        rows_of_kind(&self.fixture.conn, self.fixture.set_id, ArtifactKind::Registered)
    }

    /// Every registered row's `(frame, row id, group key, created_at, hash, path, file mtime)`: what a regroup must
    /// leave untouched.
    fn registered_snapshot(
        &self,
    ) -> Vec<(i64, i64, String, String, String, Option<String>, Option<std::time::SystemTime>)> {
        let mut out: Vec<_> = self
            .registered_rows()
            .into_iter()
            .map(|(frame_id, r)| {
                let mtime = r
                    .path
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .and_then(|m| m.modified().ok());
                (frame_id, r.id, r.group_key, r.created_at, r.config_hash, r.path, mtime)
            })
            .collect();
        out.sort_by_key(|r| r.0);
        out
    }

    /// The files under `registered/`, recursively.
    fn registered_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.layout.registered_root(), &mut out);
        out.sort();
        out
    }

    fn register_stale(&self, cfg: &StackingConfig) -> bool {
        crate::stacking::plan::build_plan(
            &self.fixture.conn,
            &self.ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(self.fixture.set_id),
            Some(cfg.clone()),
        )
        .expect("the plan should build")
        .stale_stages
        .contains(&Stage::Register)
    }
}

/// Frame-level cache spec §5.1 (Task 5): a regroup (here "one group per exposure" -> "no split") finds the
/// registered rows again, with the same reference: no frame is aligned or rewritten, and neither the rows nor
/// the files move; the gate reads Register fresh under the new grouping too.
#[test]
fn registered_frames_are_reused_across_a_regroup() {
    use crate::stacking::config::ExposureMode;
    let rig = RegisterRig::new();
    let cfg_seconds = rig.cfg(ExposureMode::Seconds);
    let cfg_all = rig.cfg(ExposureMode::NoSplit);
    let key_of = |cfg: &StackingConfig| {
        group_frames(&rig.fixture.conn, rig.fixture.set_id, &cfg.grouping).unwrap()[0].key.clone()
    };
    assert_ne!(key_of(&cfg_seconds), key_of(&cfg_all), "the two groupings must name the group differently");

    let (rc1, logs1) = crate::stacking::telemetry::capture_logs(|| rig.run(&cfg_seconds));
    assert_eq!(fan_out_items(&logs1, "register", "align"), 4, "{logs1}");
    let rows1 = rig.registered_snapshot();
    assert_eq!(rows1.len(), 4);
    assert!(rows1.iter().all(|r| r.2.is_empty()), "frame-level rows sit under the empty key: {rows1:?}");
    let files1 = rig.registered_files();
    assert_eq!(files1.len(), 4, "{files1:?}");
    assert_eq!(files1.iter().filter(|p| p.parent() != Some(rig.layout.registered_root().as_path())).count(), 0);

    let (rc2, logs2) = crate::stacking::telemetry::capture_logs(|| rig.run(&cfg_all));
    assert_eq!(rc2.reference_frame_id, rc1.reference_frame_id);
    assert_eq!(fan_out_items(&logs2, "register", "align"), 0, "a regroup re-aligned: {logs2}");
    assert_eq!(fan_out_items(&logs2, "register", "rewrite"), 0, "a regroup rewrote: {logs2}");
    assert_eq!(rig.registered_snapshot(), rows1, "a regroup rewrote or added rows");
    assert_eq!(rig.registered_files(), files1, "a regroup added or moved a file");
    assert!(!rig.register_stale(&cfg_all), "the gate reads the reused rows fresh");
}

/// Frame-level cache spec §5.7, review focus 2 (Task 5): a `registered` row whose path is in an old
/// `registered/<group_key>/` folder under another file name is the artifact, read as it stands. Register finds it
/// fresh and writes nothing, and Integrate opens the row's path (no fallback to the on-the-fly warp) and stacks
/// the same master as a run that never migrated.
#[test]
fn a_migrated_row_is_read_from_its_stored_path() {
    use crate::integration::registered_source::fallback_counters;
    use crate::stacking::config::ExposureMode;
    let _fallback_guard = fallback_counters::exclusive();

    let master_of = |rc: &RunContext| -> Vec<Vec<f32>> {
        let master = rc
            .summary
            .groups
            .iter()
            .find_map(|g| g.master_path.clone())
            .expect("master path");
        let reader = PlaneReader::open(Path::new(&master)).unwrap();
        (0..reader.channels()).map(|p| reader.read_plane(p).unwrap()).collect()
    };

    let baseline = {
        let rig = RegisterRig::new();
        let mut rc = rig.run(&rig.cfg(ExposureMode::Seconds));
        stage_output(&mut rc).unwrap();
        master_of(&rc)
    };

    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    rig.run(&cfg);
    let old_dir = rig.layout.registered_root().join("old_group_key");
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut moved = Vec::new();
    for (frame_id, row) in rig.registered_rows() {
        let old = old_dir.join(format!("legacy_{frame_id}.fits"));
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
    let rows_before = rig.registered_snapshot();

    let (mut rc, logs) = crate::stacking::telemetry::capture_logs(|| rig.run(&cfg));
    assert_eq!(fan_out_items(&logs, "register", "align"), 0, "{logs}");
    assert_eq!(fan_out_items(&logs, "register", "rewrite"), 0, "a migrated row was rewritten: {logs}");
    assert_eq!(rig.registered_snapshot(), rows_before, "a migrated row moved");
    assert_eq!(rig.registered_files(), {
        let mut m = moved.clone();
        m.sort();
        m
    }, "nothing was written to the layout path");

    fallback_counters::reset();
    stage_output(&mut rc).unwrap();
    assert_eq!(fallback_counters::snapshot(), 0, "Integrate did not open the stored path");
    let planes = master_of(&rc);
    assert_eq!(planes.len(), baseline.len());
    for (p, (a, b)) in baseline.iter().zip(&planes).enumerate() {
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "plane {p}: the migrated registered files stack another master"
        );
    }

    // The stored path is the file opened, not just a file that happens to stack alike: change one pixel of a moved
    // file (same size), record the file's new identity as a writer would, and the master moves.
    let victim = &moved[1];
    let mut bytes = std::fs::read(victim).unwrap();
    // A big-endian f32 near the middle of the data section, aligned to 4 bytes (the header is whole 2880-byte
    // blocks): flip the top mantissa bit, a finite change of a quarter or more of the value.
    let at = (bytes.len() / 2) & !3;
    bytes[at + 1] ^= 0x40;
    std::fs::write(victim, &bytes).unwrap();
    let meta = std::fs::metadata(victim).unwrap();
    assert_eq!(
        rig.fixture
            .conn
            .execute(
                "UPDATE stacking_artifacts SET size = ?1, modified_at = ?2 WHERE path = ?3",
                rusqlite::params![
                    meta.len() as i64,
                    crate::stacking::plan::render_modified_at(meta.modified().unwrap()),
                    victim.to_str().unwrap()
                ],
            )
            .unwrap(),
        1
    );
    let (mut rc, logs) = crate::stacking::telemetry::capture_logs(|| rig.run(&cfg));
    assert_eq!(fan_out_items(&logs, "register", "rewrite"), 0, "the edited row was rewritten: {logs}");
    fallback_counters::reset();
    stage_output(&mut rc).unwrap();
    assert_eq!(fallback_counters::snapshot(), 0);
    let edited = master_of(&rc);
    assert!(
        baseline.iter().zip(&edited).any(|(a, b)| a.iter().zip(b).any(|(x, y)| x.to_bits() != y.to_bits())),
        "the master did not move with the stored file's pixels: Integrate did not open the row's path"
    );
}

/// Ruling R7 (c) M2 (Task 5): re-registering a frame whose row points into an old `registered/<old_key>/` folder
/// writes the new file at the layout path, repoints the row and removes the old file - unless another row still
/// points at it.
#[test]
fn re_registering_a_migrated_row_removes_the_old_file_unless_another_row_points_at_it() {
    use crate::stacking::config::ExposureMode;
    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    rig.run(&cfg);
    let old_dir = rig.layout.registered_root().join("old_group_key");
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut legacy: HashMap<i64, PathBuf> = HashMap::new();
    for (frame_id, row) in rig.registered_rows() {
        let old = old_dir.join(format!("legacy_{frame_id}.fits"));
        std::fs::rename(row.path.as_ref().unwrap(), &old).unwrap();
        rig.fixture
            .conn
            .execute(
                "UPDATE stacking_artifacts SET path = ?1 WHERE id = ?2",
                rusqlite::params![old.to_str().unwrap(), row.id],
            )
            .unwrap();
        legacy.insert(frame_id, old);
    }
    assert_eq!(legacy.len(), 4);
    // Another row, of another group key, still points at one of the old files. No writer leaves a `registered` row
    // under a group key since the frame-level keys; the test door stores it as given.
    let kept_frame = rig.light_ids[3];
    crate::db::stacking::upsert_artifact_under_key(
        &rig.fixture.conn,
        &NewArtifact {
            subject: &StackSubject::FrameSet(rig.fixture.set_id),
            frame_id: Some(kept_frame),
            group_key: "stray",
            kind: ArtifactKind::Registered,
            path: legacy[&kept_frame].to_str(),
            config_hash: "h",
            size: None,
            modified_at: None,
            payload_json: None,
        },
    )
    .unwrap();

    // Another reference: every frame is registered again.
    crate::registration::db::set_frame_set_reference(&rig.fixture.conn, rig.fixture.set_id, rig.light_ids[1])
        .unwrap();
    let (_rc, logs) = crate::stacking::telemetry::capture_logs(|| rig.run(&cfg));
    assert_eq!(fan_out_items(&logs, "register", "align"), 4, "{logs}");

    for &frame_id in &rig.light_ids {
        let row = crate::db::stacking::find_artifact(
            &rig.fixture.conn,
            &StackSubject::FrameSet(rig.fixture.set_id),
            "",
            ArtifactKind::Registered,
            Some(frame_id),
        )
        .unwrap()
        .expect("the frame's registered row");
        let new = Path::new(row.path.as_ref().unwrap());
        assert_eq!(new.parent(), Some(rig.layout.registered_dir().as_path()), "frame {frame_id}: {new:?}");
        assert!(new.is_file());
    }
    for (frame_id, old) in &legacy {
        assert_eq!(
            old.exists(),
            *frame_id == kept_frame,
            "frame {frame_id}: {} should {} its old file",
            if *frame_id == kept_frame { "keep" } else { "lose" },
            old.display()
        );
    }
}

/// Ruling R7 (b) (Task 5): a cached file counts as fresh only while it is the file its row recorded. Run B
/// (another manual reference) re-aligns every frame, so its fan-out overwrites the files under their layout names
/// with its own pixels; cancelled right there, it records nothing, so the rows still hold run A's hash, size and
/// modification time. Run A's config again: the hashes match and the sizes are equal, but the files are not the
/// ones the rows recorded. The gate reads Register stale, and the re-run rewrites them: the master is run A's,
/// bit for bit.
#[test]
fn a_cancelled_rewrite_does_not_leave_another_runs_pixels_fresh() {
    use crate::stacking::config::ExposureMode;

    let master_of = |rc: &RunContext| -> Vec<Vec<f32>> {
        let master = rc
            .summary
            .groups
            .iter()
            .find_map(|g| g.master_path.clone())
            .expect("master path");
        let reader = PlaneReader::open(Path::new(&master)).unwrap();
        (0..reader.channels()).map(|p| reader.read_plane(p).unwrap()).collect()
    };

    let baseline = {
        let rig = RegisterRig::new();
        let mut rc = rig.run(&rig.cfg(ExposureMode::Seconds));
        stage_output(&mut rc).unwrap();
        master_of(&rc)
    };

    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    let rc_a = rig.run(&cfg);
    assert!(
        !crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc_a.run_id).unwrap().is_empty(),
        "premise: the gate reads run A's frame rows, so it is not vacuous"
    );
    assert!(!rig.register_stale(&cfg), "run A's own rows read fresh");
    let rows_a = rig.registered_snapshot();
    assert_eq!(rows_a.len(), 4);
    let sizes_a: HashMap<i64, i64> = rig
        .registered_rows()
        .into_iter()
        .map(|(frame_id, row)| (frame_id, row.size.unwrap()))
        .collect();

    // Run B: another reference, cancelled right after its Register fan-out.
    crate::registration::db::set_frame_set_reference(&rig.fixture.conn, rig.fixture.set_id, rig.light_ids[1])
        .unwrap();
    let (_rc_b, res) = rig.run_with(&cfg, true);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    // The write order (file-identity spec §4.2): every row was deleted before its file was replaced, and the
    // cancelled run recorded none of its own, so no row is left over run B's files.
    assert!(
        rig.registered_rows().is_empty(),
        "a row was left over the files the cancelled run wrote: {:?}",
        rig.registered_rows()
    );
    for a in &rows_a {
        let path = a.5.as_ref().unwrap();
        let meta = std::fs::metadata(path).unwrap_or_else(|e| panic!("run B's file {path} is gone: {e}"));
        assert_ne!(Some(meta.modified().unwrap()), a.6, "premise: run B overwrote frame {}'s file", a.0);
        assert_eq!(
            meta.len() as i64,
            sizes_a[&a.0],
            "premise: the sizes still agree with run A's rows, so the gate assertion below cannot pass on a size change"
        );
    }

    // Run A's config again.
    crate::registration::db::set_frame_set_reference(&rig.fixture.conn, rig.fixture.set_id, rig.light_ids[0])
        .unwrap();
    // Run B, cancelled inside Register, wrote no frame rows; the gate (ruling R9) reads run A's, and what is under
    // test is that it does not trust the files B overwrote.
    assert!(rig.register_stale(&cfg), "the gate trusted files another run overwrote");
    let (mut rc, logs) = crate::stacking::telemetry::capture_logs(|| rig.run(&cfg));
    assert_eq!(
        fan_out_items(&logs, "register", "rewrite"),
        4,
        "the re-run did not rewrite the overwritten files: {logs}"
    );
    assert!(!rig.register_stale(&cfg), "the rewritten files read fresh");
    stage_output(&mut rc).unwrap();
    let planes = master_of(&rc);
    assert_eq!(planes.len(), baseline.len());
    for (p, (a, b)) in baseline.iter().zip(&planes).enumerate() {
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "plane {p}: the master is not run A's"
        );
    }
}

/// The write order (file-identity spec §4.2) deletes a `registered` row only where a file is written: the two-pass
/// dry pass persists nothing, so it deletes nothing. After a run, the dry pass against ANOTHER reference (every
/// frame a miss, and every frame queued for registration) leaves every row and file as it was.
#[test]
fn a_dry_pass_leaves_the_registered_rows_alone() {
    use crate::stacking::config::ExposureMode;

    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    let mut rc = rig.run(&cfg);
    let before = rig.registered_snapshot();
    assert_eq!(before.len(), 4, "premise: run A recorded a row per frame");

    let group = rc.plan_groups[0].clone();
    let other = rig.light_ids[1];
    let other_calibrated = rc.measured[&group.key]
        .iter()
        .find(|e| e.frame.frame_id == other)
        .and_then(|e| e.calibrated.clone())
        .expect("the other reference's calibrated file");
    let ref_stars = reference_stars(&other_calibrated, &cfg.registration, Some(&rc.ctx.image_pool), None).unwrap();
    let other_frame = find_frame_in_groups(&rc.plan_groups, other).unwrap();
    let reference_hash = {
        let conn = db(&rc.ctx).unwrap().conn();
        rc.memo.calibration_hash_checked(&conn, &cfg, &other_frame).unwrap()
    };
    let by_frame: HashMap<i64, RegistrationRecord> = HashMap::new();
    let mut progress = (0usize, 8usize);
    let dry = register_group_pass(
        &mut rc,
        &group,
        &ref_stars,
        other,
        &reference_hash,
        &by_frame,
        false,
        false,
        &mut progress,
        &mut SolveCache::new(),
    )
    .unwrap();
    assert_eq!(dry.len(), 4, "premise: the dry pass registered every frame: {dry:?}");
    assert_eq!(rig.registered_snapshot(), before, "the dry pass touched the registered rows or files");
}

/// Ruling R9: a run cancelled inside Register is the newest run and has no frame rows (they are written when the
/// stage is done). The gate used to judge those no rows and read Register fresh; it now judges the latest run that
/// has rows. Run A, then run B on another manual reference cancelled inside Register, then the gate under B's
/// config: run A's rows name another reference, so Register is stale.
#[test]
fn a_run_cancelled_inside_register_reads_register_stale() {
    use crate::stacking::config::ExposureMode;

    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    let rc_a = rig.run(&cfg);
    assert!(!rig.register_stale(&cfg), "run A's own rows read fresh");

    crate::registration::db::set_frame_set_reference(&rig.fixture.conn, rig.fixture.set_id, rig.light_ids[1])
        .unwrap();
    let (rc_b, res) = rig.run_with(&cfg, true);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    assert!(
        crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc_b.run_id).unwrap().is_empty(),
        "premise: the cancelled run wrote no frame rows"
    );
    assert!(
        !crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc_a.run_id).unwrap().is_empty(),
        "premise: run A did"
    );
    assert!(rig.register_stale(&cfg), "the gate read the cancelled run's empty frame list as fresh");
}

/// Ruling R14 (e), the fresh side of R9: run A registers under a config, then run B on the SAME config is
/// cancelled inside Register and writes no frame rows. The gate looks back past B to A's rows, which are fresh,
/// and reads Register fresh.
#[test]
fn a_run_cancelled_inside_register_leaves_an_older_fresh_run_fresh() {
    use crate::stacking::config::ExposureMode;

    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    rig.run(&cfg);
    assert!(!rig.register_stale(&cfg), "run A's own rows read fresh");

    let (rc_b, res) = rig.run_with(&cfg, true);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    assert!(
        crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc_b.run_id).unwrap().is_empty(),
        "premise: the cancelled run wrote no frame rows"
    );
    assert!(
        !rig.register_stale(&cfg),
        "the gate stopped at the cancelled run instead of looking back to run A"
    );
}

/// Ruling R14 (d): a run that ended inside Register can hold frame rows that are all excluded (a changed landed
/// file takes members out before the stage's rows are complete). Those rows say nothing about what was
/// registered, so the gate skips such a run like an empty one and judges the run before it. Run A, then run B on
/// another manual reference cancelled inside Register with one excluded row: the gate under B's config reads run
/// A's rows, which name the other reference, and Register is stale (the excluded-only rows alone would judge no
/// frame and read fresh).
#[test]
fn a_run_with_only_excluded_rows_is_skipped_by_the_register_gate() {
    use crate::stacking::config::ExposureMode;

    let rig = RegisterRig::new();
    let cfg = rig.cfg(ExposureMode::Seconds);
    rig.run(&cfg);

    crate::registration::db::set_frame_set_reference(&rig.fixture.conn, rig.fixture.set_id, rig.light_ids[1])
        .unwrap();
    let (rc_b, res) = rig.run_with(&cfg, true);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    let group_id = *rc_b.group_ids.values().next().unwrap();
    crate::db::stacking::upsert_frame_row(
        &rig.fixture.conn,
        &StackSubject::FrameSet(rig.fixture.set_id),
        &crate::db::stacking::NewFrameRow {
            run_id: rc_b.run_id,
            group_id,
            frame_id: rig.light_ids[2],
            included: false,
            exclusion_reason: Some("changed during the run"),
            weight: None,
            weight_channels_json: None,
            metrics_json: None,
            reg_status: None,
            reg_model: None,
            reg_rms_px: None,
            reg_inliers: None,
            reg_inlier_ratio: None,
            reg_flipped: None,
            rejected_fraction: None,
        },
    )
    .unwrap();
    assert!(
        crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc_b.run_id)
            .unwrap()
            .iter()
            .all(|r| !r.included),
        "premise: the newest run has a row, and it is not included"
    );
    assert!(rig.register_stale(&cfg), "the gate judged a run with no included row");
}

/// Six lights that land in ONE group — same filter, binning and
/// exposure — but at two pixel scales: three shot at
/// [`FINE_SCALE_ARCSEC`] in a [`FINE_W`]x[`FINE_H`] frame and three at
/// [`COARSE_SCALE_ARCSEC`] in a [`COARSE_W`]x[`COARSE_H`] one, of the
/// same field. That is the M4b case: one target, two optical trains,
/// nothing about camera or native geometry in the group key.
///
/// Each frame is dithered a pixel or two so the six are not literally
/// the same image; the dither stays well inside the WCS seed's own
/// confirmation radius, which is deliberate — the fixture's plate
/// solves all name one `crval`, so the seed knows the scale step but
/// not the dither, exactly as a real solve pair does to within its
/// residual. Returns `(fixture, fine_ids, coarse_ids, working,
/// output)`; masters for BOTH geometries are already linked.
fn seed_mixed_scale_group(
    db_path: &Path,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    let mut fine_ids = Vec::new();
    let mut coarse_ids = Vec::new();
    for (i, (dx, dy)) in [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0)].iter().enumerate() {
        let date_obs = date_obs_at(i);
        let stem = format!("fine{i}");
        let spec = mixed_light_spec(&stem, &date_obs, FINE_W, FINE_H);
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(2.0, *dx, *dy),
            600.0,
            5.0,
            200 + i as u64,
        );
        fine_ids.push(id);
    }
    for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)].iter().enumerate() {
        let date_obs = date_obs_at(i + 3);
        let stem = format!("coarse{i}");
        let spec = mixed_light_spec(&stem, &date_obs, COARSE_W, COARSE_H);
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(1.0, *dx, *dy),
            600.0,
            5.0,
            210 + i as u64,
        );
        coarse_ids.push(id);
    }

    test_fixtures::add_master_dark_and_flat(&fixture, &fine_ids, FINE_W, FINE_H);
    test_fixtures::add_master_dark_and_flat(&fixture, &coarse_ids, COARSE_W, COARSE_H);
    crate::registration::db::set_frame_set_reference(
        &fixture.conn,
        fixture.set_id,
        fine_ids[0],
    )
    .unwrap();

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

    (fixture, fine_ids, coarse_ids, working, output)
}

/// Drives stages 1-5 over [`seed_mixed_scale_group`] with the fine
/// frame `fine_ids[0]` pinned as the manual reference, and hands back
/// the context plus every `registration_results` row by frame id.
fn run_mixed_scale_register(
    fixture: &test_fixtures::Fixture,
    ctx: Arc<ServiceContext>,
    working: &tempfile::TempDir,
    output: &tempfile::TempDir,
) -> (RunContext, HashMap<i64, RegistrationRecord>) {
    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(
        plan_groups.len(),
        1,
        "premise: two pixel scales, one group: {plan_groups:?}"
    );
    assert_eq!(plan_groups[0].frames.len(), 6);
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc = test_context(
        ctx,
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

    let rows = get_registration_for_frame_set(&fixture.conn, fixture.set_id)
        .unwrap()
        .into_iter()
        .map(|r| (r.frame_id, r))
        .collect();
    (rc, rows)
}

/// What three independent solves of ONE rig look like: the same
/// sampling, disagreeing in the fourth digit (rulings R-T2-1,
/// R-T6-4 — the measured median deviation; that ruling's tolerance
/// covers the 1.6 % tail too). The first is [`FINE_SCALE_ARCSEC`]
/// itself, since `fine_ids[0]` is the pinned reference every ratio is
/// taken against.
const FINE_SOLVED_SCALES: [f64; 3] = [FINE_SCALE_ARCSEC, 0.7803, 0.7801];

/// M4b Task 2 (rulings R-M4b-2/3): frames whose own pixel scale is
/// twice the reference's are judged against a gate centred on 2.0 and
/// register, and the plate-solve seed is what carried them there.
///
/// Rulings R-T2-1/R-T6-4/R-T6-9 ride on the same run: the three fine
/// frames carry solve-to-solve scale jitter (0.7800 / 0.7803 / 0.7801
/// — ratios within 4e-4 of 1), which must NOT be read as a scale step.
/// Since R-T6-9 they DO get a seed built (they are solved, and so is
/// the reference) — it simply does not lead, and the quads carry them,
/// so no `+wcs` lands on their rows and no seed warning anywhere.
#[test]
fn mixed_scale_frames_register_through_the_per_frame_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, fine_ids, coarse_ids, working, output) = seed_mixed_scale_group(&db_path);

    for (&id, &scale) in fine_ids.iter().zip(FINE_SOLVED_SCALES.iter()) {
        test_fixtures::seed_plate_solve_wcs(
            &fixture.conn,
            id,
            FINE_W,
            FINE_H,
            MIXED_RA_DEG,
            MIXED_DEC_DEG,
            scale,
        );
    }
    for &id in &coarse_ids {
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

    let (rc, rows) = run_mixed_scale_register(&fixture, ctx, &working, &output);

    let group_key = rc.plan_groups[0].key.clone();
    for entry in &rc.measured[&group_key] {
        assert!(
            entry.included,
            "frame {} excluded: {:?}",
            entry.frame.frame_id, entry.reason
        );
    }

    for &id in &coarse_ids {
        let row = rows.get(&id).unwrap_or_else(|| panic!("no row for {id}"));
        assert_eq!(row.status, "aligned", "{id}: {row:?}");
        let scale = row.scale.expect("an aligned row carries its scale");
        assert!((scale - 2.0).abs() < 0.02, "{id}: scale {scale}");
        assert!(
            row.model.as_deref().unwrap_or_default().ends_with("+wcs"),
            "{id}: the plate-solve seed should have carried it: {:?}",
            row.model
        );
    }
    // Ruling R-T2-1: solve-to-solve jitter is not a scale step.
    // Fixture premise first — these really are different numbers, and
    // they really are within the tolerance.
    for (i, &scale) in FINE_SOLVED_SCALES.iter().enumerate().skip(1) {
        let ratio = scale / FINE_SOLVED_SCALES[0];
        assert_ne!(scale, FINE_SOLVED_SCALES[0], "frame {i} must differ");
        assert!(
            !ratio_wants_seed(ratio),
            "frame {i}: ratio {ratio} must read as the same sampling"
        );
    }
    for &id in &fine_ids {
        let row = rows.get(&id).unwrap_or_else(|| panic!("no row for {id}"));
        let scale = row.scale.expect("an aligned row carries its scale");
        assert!((scale - 1.0).abs() < 0.02, "{id}: scale {scale}");
        // R-T6-4 with R-T6-9's plumbing in place: a seed WAS built for
        // these frames (both they and the reference are solved), and
        // the jitter only decided that it would not LEAD. The quads
        // carried them, so the row must not name the seed …
        assert!(
            !row.model.as_deref().unwrap_or_default().contains("wcs"),
            "{id}: {:?}",
            row.model
        );
    }
    // … and the seed never had to step in, so no frame reported the
    // quad matcher failing or a hint being refused.
    assert!(
        !rc.warnings.iter().any(|w| w.contains("wcs seed")),
        "{:?}",
        rc.warnings
    );
    assert!(
        !rc.warnings
            .iter()
            .any(|w| w.contains("plate-solve seed used")),
        "{:?}",
        rc.warnings
    );
    assert_eq!(rows[&fine_ids[0]].status, "reference");
}

/// The same six frames with NO pixel scale recorded anywhere: the gate
/// collapses to M1's fixed window and the binned three are refused by
/// name — the regression this task exists to lift.
#[test]
fn mixed_scale_frames_without_a_known_scale_hit_the_fixed_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, fine_ids, coarse_ids, working, output) = seed_mixed_scale_group(&db_path);

    let (rc, rows) = run_mixed_scale_register(&fixture, ctx, &working, &output);

    let group_key = rc.plan_groups[0].key.clone();
    let expected = "registration failed: scale 2.00 outside [0.80, 1.25] (expected 1.00)";
    for &id in &coarse_ids {
        let entry = rc.measured[&group_key]
            .iter()
            .find(|e| e.frame.frame_id == id)
            .expect("every frame has an entry");
        assert!(!entry.included, "{id} should have been excluded");
        assert_eq!(entry.reason.as_deref(), Some(expected), "{id}");
        assert_eq!(rows[&id].status, "failed");
    }
    for &id in &fine_ids {
        let entry = rc.measured[&group_key]
            .iter()
            .find(|e| e.frame.frame_id == id)
            .expect("every frame has an entry");
        assert!(entry.included, "{id}: {:?}", entry.reason);
    }
}

/// Drives stages 1-9 over [`seed_two_geometry_groups`] with `cfg`, and
/// hands back the finished context.
fn run_two_geometry_groups(
    fixture: &test_fixtures::Fixture,
    ctx: Arc<ServiceContext>,
    cfg: StackingConfig,
    working: &tempfile::TempDir,
    output: &tempfile::TempDir,
) -> RunContext {
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(
        plan_groups.len(),
        3,
        "premise: one group per filter: {plan_groups:?}"
    );
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc = test_context(
        ctx,
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
    stage_output(&mut rc).unwrap();
    rc
}

/// The group key of the group holding `frame_id`.
fn group_key_of(rc: &RunContext, frame_id: i64) -> String {
    rc.plan_groups
        .iter()
        .find(|g| g.frames.iter().any(|f| f.frame_id == frame_id))
        .map(|g| g.key.clone())
        .unwrap_or_else(|| panic!("frame {frame_id} is in no group"))
}

/// A written master's own `NAXIS1`/`NAXIS2` and `ATH_RGEO`.
fn master_geometry(group: &SummaryGroup) -> (i64, i64, String) {
    let path = group
        .master_path
        .clone()
        .unwrap_or_else(|| panic!("group {} wrote no master", group.key));
    let header = FitsHeader::from_path(Path::new(&path)).unwrap();
    (
        header.get_i32("NAXIS1").expect("NAXIS1") as i64,
        header.get_i32("NAXIS2").expect("NAXIS2") as i64,
        header.get_str("ATH_RGEO").expect("ATH_RGEO"),
    )
}

/// M4b rulings R-M4b-4/5/8, (a): in `native` mode each group registers
/// onto its OWN best-weighted member and its master is delivered in
/// that member's geometry — the two groups here are 2x apart in
/// sampling, so a co-registered run would have to resample one into
/// the other. The run-level reference row stays the LARGEST group's
/// (ruling R-M4b-4), and every master says `ATH_RGEO = 'native'`.
#[test]
fn native_geometry_gives_each_group_its_own_reference_and_geometry() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, fine_ids, coarse_ids, tiny_ids, working, output) =
        seed_two_geometry_groups(&db_path);

    let mut cfg = StackingConfig::default();
    cfg.registration.geometry = RegistrationGeometry::Native;
    // The two-pass re-pick has its own test below; this one pins the
    // best-by-weight pick itself, so it must not move.
    cfg.reference.two_pass = false;

    let rc = run_two_geometry_groups(&fixture, ctx, cfg, &working, &output);

    let fine_key = group_key_of(&rc, fine_ids[0]);
    let coarse_key = group_key_of(&rc, coarse_ids[0]);
    assert_ne!(fine_key, coarse_key);

    // Each group's own reference is its own cleanest member …
    let fine = summary_group(&rc, &fine_key);
    let coarse = summary_group(&rc, &coarse_key);
    assert_eq!(fine.reference_frame_id, Some(fine_ids[0]), "{fine:?}");
    assert_eq!(coarse.reference_frame_id, Some(coarse_ids[0]), "{coarse:?}");

    // … and each master is delivered in that member's own geometry.
    let (fw, fh, fgeo) = master_geometry(fine);
    assert_eq!((fw, fh), (FINE_W as i64, FINE_H as i64));
    assert_eq!(fgeo, "native");
    let (cw, ch, cgeo) = master_geometry(coarse);
    assert_eq!((cw, ch), (COARSE_W as i64, COARSE_H as i64));
    assert_eq!(cgeo, "native");

    // Fix round 1 (m3): the two-frame `SII` group never reached stage
    // 5, so it never registered onto anything — its summary must not
    // claim a reference (the Results card would render
    // `· reference #N` for a master that was never written).
    let tiny = summary_group(&rc, &group_key_of(&rc, tiny_ids[0]));
    assert_eq!(
        tiny.reference_frame_id, None,
        "a group the register stage skipped has no reference: {tiny:?}"
    );
    assert_eq!(tiny.master_path, None, "{tiny:?}");

    // The run-level reference is still the largest group's — what the
    // plan gate and the results header show (ruling R-M4b-4).
    assert_eq!(rc.reference_frame_id, Some(fine_ids[0]));
    let row = crate::db::stacking::get_run(&fixture.conn, rc.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.reference_frame_id, Some(fine_ids[0]));

    // Every persisted registration row names its own group's
    // reference, and the coarse frames were never warped onto the fine
    // one.
    let rows: HashMap<i64, RegistrationRecord> =
        get_registration_for_frame_set(&fixture.conn, fixture.set_id)
            .unwrap()
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect();
    for &id in &fine_ids {
        assert_eq!(rows[&id].reference_frame_id, fine_ids[0], "{id}");
    }
    for &id in &coarse_ids {
        assert_eq!(rows[&id].reference_frame_id, coarse_ids[0], "{id}");
        let scale = rows[&id].scale.expect("an aligned row carries its scale");
        assert!(
            (scale - 1.0).abs() < 0.02,
            "{id}: a native-mode frame registers at its own sampling: {scale}"
        );
    }
}

/// (b) The SAME set in the default `coRegistered` mode: one reference
/// for the whole run, both masters in its geometry (the coarse group
/// resampled ×2 into it), and every group's summary reference equal to
/// the run's. `ATH_RGEO = 'coRegistered'`.
#[test]
fn co_registered_geometry_keeps_one_reference_for_every_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, fine_ids, coarse_ids, _tiny_ids, working, output) =
        seed_two_geometry_groups(&db_path);

    let mut cfg = StackingConfig::default();
    assert_eq!(
        cfg.registration.geometry,
        RegistrationGeometry::CoRegistered,
        "the default is co-registered"
    );
    cfg.reference.two_pass = false;

    let rc = run_two_geometry_groups(&fixture, ctx, cfg, &working, &output);

    let run_reference = rc.reference_frame_id.expect("a reference");
    assert_eq!(run_reference, fine_ids[0], "the largest group's best frame");

    let fine_key = group_key_of(&rc, fine_ids[0]);
    let coarse_key = group_key_of(&rc, coarse_ids[0]);
    for key in [&fine_key, &coarse_key] {
        let group = summary_group(&rc, key);
        assert_eq!(
            group.reference_frame_id,
            Some(run_reference),
            "co-registered: every group names the run's reference: {group:?}"
        );
        let (w, h, geo) = master_geometry(group);
        assert_eq!(
            (w, h),
            (FINE_W as i64, FINE_H as i64),
            "co-registered: every master is in the run reference's geometry ({key})"
        );
        assert_eq!(geo, "coRegistered");
    }

    // The coarse frames really were resampled ×2 onto the fine grid.
    let rows: HashMap<i64, RegistrationRecord> =
        get_registration_for_frame_set(&fixture.conn, fixture.set_id)
            .unwrap()
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect();
    for &id in &coarse_ids {
        assert_eq!(rows[&id].reference_frame_id, run_reference, "{id}");
        let scale = rows[&id].scale.expect("an aligned row carries its scale");
        assert!((scale - 2.0).abs() < 0.02, "{id}: scale {scale}");
    }
}

/// Controller ruling R-T3-2 (fix round 1, m6): a Manual pin in native
/// mode pins ITS OWN group's reference and nothing else — the other
/// groups still auto-pick their own best-weighted member — while the
/// RUN-level `stacking_runs.reference_frame_id` stays the pin, whatever
/// size its group is, because that is the choice the results header and
/// the plan gate both key on. The pin never moves, two-pass on.
#[test]
fn native_manual_reference_pins_only_its_own_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, fine_ids, coarse_ids, _tiny_ids, working, output) =
        seed_two_geometry_groups(&db_path);

    // Deliberately the SMALLER group, and deliberately NOT that group's
    // own best-by-weight member (`coarse_ids[0]` is the cleanest): both
    // halves of the ruling are only visible when the pin disagrees with
    // what Auto would have chosen.
    let pin = coarse_ids[1];
    crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, pin)
        .unwrap();

    let mut cfg = StackingConfig::default();
    cfg.registration.geometry = RegistrationGeometry::Native;
    cfg.reference.mode = ReferenceMode::Manual;
    assert!(cfg.reference.two_pass, "the default must be on");

    let rc = run_two_geometry_groups(&fixture, ctx, cfg, &working, &output);

    // (i) the pinned group keeps the pin, and two-pass did not move it.
    let coarse = summary_group(&rc, &group_key_of(&rc, pin));
    assert_eq!(
        coarse.reference_frame_id,
        Some(pin),
        "a manual pin is its own group's reference: {coarse:?}"
    );
    assert_eq!(
        rc.summary.reference.switched_from, None,
        "{:?}",
        rc.warnings
    );
    assert!(
        !rc.warnings.iter().any(|w| w.contains("two-pass")),
        "{:?}",
        rc.warnings
    );

    // (ii) the other group auto-picks its OWN member — never the pin.
    let fine = summary_group(&rc, &group_key_of(&rc, fine_ids[0]));
    let fine_reference = fine.reference_frame_id.expect("a group reference");
    assert_ne!(fine_reference, pin, "the pin applies to its own group only");
    assert!(
        fine_ids.contains(&fine_reference),
        "the fine group's reference must be one of its own frames: {fine_reference}"
    );

    // (iii) the RUN-level reference is the pin, in the context, the
    // summary and the `stacking_runs` row alike.
    assert_eq!(rc.reference_frame_id, Some(pin));
    assert_eq!(rc.summary.reference.frame_id, Some(pin));
    let row = crate::db::stacking::get_run(&fixture.conn, rc.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.reference_frame_id, Some(pin));
    assert_eq!(row.reference_mode, "manual");

    // … and each master is still delivered in its own group's geometry.
    assert_eq!(
        master_geometry(fine),
        (FINE_W as i64, FINE_H as i64, "native".to_string())
    );
    assert_eq!(
        master_geometry(coarse),
        (COARSE_W as i64, COARSE_H as i64, "native".to_string())
    );
}

/// (c) Native mode with the two-pass re-pick ON (the default): the
/// pick runs over each group's OWN members, so whatever it chooses is
/// still a member of that same group — it can never wander into the
/// other group's frames, which have a different geometry entirely.
#[test]
fn native_two_pass_picks_within_each_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, fine_ids, coarse_ids, _tiny_ids, working, output) =
        seed_two_geometry_groups(&db_path);

    let mut cfg = StackingConfig::default();
    cfg.registration.geometry = RegistrationGeometry::Native;
    assert!(cfg.reference.two_pass, "the default must be on");

    let rc = run_two_geometry_groups(&fixture, ctx, cfg, &working, &output);

    let fine_key = group_key_of(&rc, fine_ids[0]);
    let coarse_key = group_key_of(&rc, coarse_ids[0]);

    let fine = summary_group(&rc, &fine_key);
    let fine_reference = fine.reference_frame_id.expect("a group reference");
    assert!(
        fine_ids.contains(&fine_reference),
        "the fine group's reference must be one of its own frames: {fine_reference}"
    );
    let coarse = summary_group(&rc, &coarse_key);
    let coarse_reference = coarse.reference_frame_id.expect("a group reference");
    assert!(
        coarse_ids.contains(&coarse_reference),
        "the coarse group's reference must be one of its own frames: {coarse_reference}"
    );

    // … and each master still comes out in its own group's geometry.
    assert_eq!(
        master_geometry(fine),
        (FINE_W as i64, FINE_H as i64, "native".to_string())
    );
    assert_eq!(
        master_geometry(coarse),
        (COARSE_W as i64, COARSE_H as i64, "native".to_string())
    );
}

/// Tier C Task 3 fix round 2 (ruling C-18, item 5, follow-up): the
/// floor `resolve_register_fits` applies before handing a fit-derived
/// star list downstream — below `align::MIN_INLIERS` it must return
/// `None` (proven here to actually reach a real, non-empty detection
/// when that `None` is handed to `detect_frame_stars`, on a real
/// star-containing field — not just "the resolver declined", which
/// alone would not rule out a broken caller silently treating the
/// refusal as "no stars anywhere"); at exactly `MIN_INLIERS` it must be
/// admitted whole. No DB access beyond a real `frames_set` row (the FK
/// `stacking_artifacts.frames_set_id` requires one) and one real
/// calibrated frame — `resolve_register_fits` itself needs no group,
/// run or plan machinery.
#[test]
fn resolve_register_fits_floors_below_min_inliers_and_admits_at_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, "floor-fixture");

    // A real `frames` row (`stacking_artifacts.frame_id` FKs to it) —
    // `add_light_with_field` writes a RAW 16-bit light, which is the
    // wrong shape for `detect_frame_stars` (it expects a CALIBRATED
    // float32 frame in native `[0, 1]` units, the same convention
    // `register::frame::tests::pair()` uses); the catalog row is all
    // that's needed from it, so its file is overwritten below with a
    // genuinely calibrated-shaped star field at the SAME path.
    let date_obs = date_obs_at(0);
    let spec = star_light_spec("f0", &date_obs);
    let (frame_id, calibrated_path) = test_fixtures::add_light_with_field(
        &fixture,
        &spec,
        &shifted_stars(0.0, 0.0),
        600.0,
        5.0,
        101,
    );
    // BASE_STARS (ten stars, "well above the required >= 8" per its own
    // doc comment), re-amplituded into native `[0, 1]` units — proof
    // that the below-floor branch below reaches an ACTUAL detection on
    // a real field, not a stub.
    let native_stars: Vec<(f64, f64, f64)> = shifted_stars(0.0, 0.0)
        .into_iter()
        .map(|(x, y, _)| (x, y, 0.3))
        .collect();
    let mut plane = crate::test_support::gaussian_field(
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
        &native_stars,
        1.6,
        0.08,
    );
    crate::test_support::add_noise(&mut plane, 0.002, 101);
    crate::fits_writer::write_fits_f32(
        &calibrated_path,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
        1,
        &plane,
        &[],
    )
    .unwrap();

    let cfg = StackingConfig::default();
    let rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        "floor-fixture",
        cfg.clone(),
        // The group `resolve_register_fits` looks up: a mono one, so its frames read `fits.0` (ruling R3 (b)).
        vec![IntegrationGroup {
            key: "mono__floor_test".into(),
            instrume: None,
            color_mode: ColorMode::Mono,
            filter: None,
            binning: 1,
            cameras: Vec::new(),
            exposure_s: None,
            exposure_class: "unknown".into(),
            frames: Vec::new(),
            total_exposure_s: 0.0,
            pixel_scale_arcsec: None,
        }],
        WorkingLayout::new(fixture.dir.path(), "floor-fixture"),
        fixture.dir.path().to_path_buf(),
        HashMap::new(),
    );

    let group_key = "mono__floor_test";
    let calibrated_hash = "floor-test-calibrated-hash";
    // No Measure ran on this context: the group β is not resolved (Task 11b), as `resolve_register_fits` reads it.
    let expected_hash = measurement_hash_for(&cfg, calibrated_hash, None);

    // A fixed-value fit that survives `passes_register_cuts` on its
    // own — the same shape `detect::tests::good_fit()` pins — repeated
    // at distinct positions.
    let good_fit_at = |i: usize| psf_signal::StarFit {
        x: 10.0 + i as f64 * 4.0,
        y: 10.0 + i as f64 * 3.0,
        background: 100.0,
        amplitude: 5000.0,
        fwhm_x: 3.0,
        fwhm_y: 3.0,
        fwtm_x: 6.0,
        fwtm_y: 6.0,
        theta: 0.0,
        beta: 4.0,
        residual: 0.02,
        signal: 40000.0,
        area: 28.0,
        peak5: f64::NAN,
        sky_ring: f64::NAN,
        sky_ring_sigma: f64::NAN,
        sky_ring_n: 0,
        ap_sum: f64::NAN,
        snr: f64::NAN,
        hfr: f64::NAN,
        flags: crate::star_measure::StarFlags::default(),
    };

    let seed_fits = |fits: &[psf_signal::StarFit]| {
        let path = fixture
            .dir
            .path()
            .join(format!("floor-{}.athf", fits.len()));
        fits_artifact::write_fits(&path, fits).unwrap();
        let size = std::fs::metadata(&path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &fixture.conn,
            &crate::db::stacking::NewArtifact {
                subject: &StackSubject::FrameSet(fixture.set_id),
                frame_id: Some(frame_id),
                group_key: "",
                kind: ArtifactKind::Fits(0),
                path: Some(path.to_str().unwrap()),
                config_hash: &expected_hash,
                size: Some(size),
                modified_at: Some(&crate::stacking::test_fixtures::modified_at_of(&path)),
                payload_json: None,
            },
        )
        .unwrap();
    };

    // Below the floor: MIN_INLIERS - 1 fits, every one individually
    // cut-surviving (only their COUNT is short).
    let below: Vec<psf_signal::StarFit> =
        (0..align::MIN_INLIERS - 1).map(good_fit_at).collect();
    seed_fits(&below);
    let resolved = resolve_register_fits(&rc, group_key, frame_id, 1, calibrated_hash);
    assert!(
        resolved.is_none(),
        "a {}-star list below MIN_INLIERS ({}) must be refused",
        below.len(),
        align::MIN_INLIERS
    );

    // The caller's own fallback: `None` handed to `detect_frame_stars`
    // must reach a REAL detection on the star-containing field, not
    // silently return an empty list.
    let detected = detect_frame_stars(
        &calibrated_path,
        &cfg.registration,
        None,
        resolved.as_deref(),
    )
    .unwrap();
    assert!(
        !detected.stars.is_empty(),
        "the detection fallback must find the field's real stars, not return empty"
    );
    assert!(
        detected.stars.len() >= align::MIN_INLIERS,
        "the real field carries well above MIN_INLIERS stars: {}",
        detected.stars.len()
    );

    // At exactly the floor: MIN_INLIERS fits must be admitted whole
    // (the fits path, not detection).
    let at_floor: Vec<psf_signal::StarFit> = (0..align::MIN_INLIERS).map(good_fit_at).collect();
    seed_fits(&at_floor);
    let resolved_at_floor = resolve_register_fits(&rc, group_key, frame_id, 1, calibrated_hash);
    assert_eq!(
        resolved_at_floor.as_ref().map(Vec::len),
        Some(align::MIN_INLIERS),
        "exactly MIN_INLIERS fits must be admitted whole: {resolved_at_floor:?}"
    );
}

/// Ruling R3 (b), the negative: a frame that calibrates to ONE plane inside a debayered OSC group (an unparseable
/// BAYERPAT) never reaches Measure's `fits.0`, even when a fresh at-floor row is there: registration folds that
/// artifact only for a group whose frames read it (`register_reads_measure_fits`), so reading it would be a read
/// the hash does not cover. The same row, asked for through a mono group, is admitted (the control).
#[test]
fn resolve_register_fits_is_none_for_a_group_that_does_not_read_fits() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, "floor-fixture");
    let date_obs = date_obs_at(0);
    let (frame_id, _path) = test_fixtures::add_light(
        &fixture,
        &LightSpec {
            write_file: false,
            ..star_light_spec("f0", &date_obs)
        },
    );

    let cfg = StackingConfig::default();
    assert!(cfg.calibration.debayer_osc, "premise: the OSC group is debayered");
    let group = |key: &str, color_mode| IntegrationGroup {
        key: key.into(),
        instrume: None,
        color_mode,
        filter: None,
        binning: 1,
        cameras: Vec::new(),
        exposure_s: None,
        exposure_class: "unknown".into(),
        frames: Vec::new(),
        total_exposure_s: 0.0,
        pixel_scale_arcsec: None,
    };
    let rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        "floor-fixture",
        cfg.clone(),
        vec![
            group("osc__floor_test", ColorMode::Osc),
            group("mono__floor_test", ColorMode::Mono),
        ],
        WorkingLayout::new(fixture.dir.path(), "floor-fixture"),
        fixture.dir.path().to_path_buf(),
        HashMap::new(),
    );
    let calibrated_hash = "floor-test-calibrated-hash";
    let expected_hash = measurement_hash_for(&cfg, calibrated_hash, None);
    let good_fit_at = |i: usize| psf_signal::StarFit {
        x: 10.0 + i as f64 * 4.0,
        y: 10.0 + i as f64 * 3.0,
        background: 100.0,
        amplitude: 5000.0,
        fwhm_x: 3.0,
        fwhm_y: 3.0,
        fwtm_x: 6.0,
        fwtm_y: 6.0,
        theta: 0.0,
        beta: 4.0,
        residual: 0.02,
        signal: 40000.0,
        area: 28.0,
        peak5: f64::NAN,
        sky_ring: f64::NAN,
        sky_ring_sigma: f64::NAN,
        sky_ring_n: 0,
        ap_sum: f64::NAN,
        snr: f64::NAN,
        hfr: f64::NAN,
        flags: crate::star_measure::StarFlags::default(),
    };
    let at_floor: Vec<psf_signal::StarFit> = (0..align::MIN_INLIERS).map(good_fit_at).collect();
    let path = fixture.dir.path().join("floor-at-floor.athf");
    fits_artifact::write_fits(&path, &at_floor).unwrap();
    let size = std::fs::metadata(&path).unwrap().len() as i64;
    crate::db::stacking::upsert_artifact(
        &fixture.conn,
        &crate::db::stacking::NewArtifact {
            subject: &StackSubject::FrameSet(fixture.set_id),
            frame_id: Some(frame_id),
            group_key: "",
            kind: ArtifactKind::Fits(0),
            path: Some(path.to_str().unwrap()),
            config_hash: &expected_hash,
            size: Some(size),
            modified_at: Some(&crate::stacking::test_fixtures::modified_at_of(&path)),
            payload_json: None,
        },
    )
    .unwrap();

    assert_eq!(
        resolve_register_fits(&rc, "mono__floor_test", frame_id, 1, calibrated_hash)
            .as_ref()
            .map(Vec::len),
        Some(align::MIN_INLIERS),
        "the control: a mono group reads the fresh at-floor row"
    );
    assert!(
        resolve_register_fits(&rc, "osc__floor_test", frame_id, 1, calibrated_hash).is_none(),
        "a debayered OSC group's frame detects instead of reading a row its hash does not cover"
    );
}

#[test]
fn registration_rows_are_written_and_reused() {
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

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();

    let run_pass = |cfg: StackingConfig| -> RunContext {
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
            layout.clone(),
            output_dir.clone(),
            group_ids,
        );
        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        rc
    };

    let rc1 = run_pass(StackingConfig::default());
    let rows1 = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    assert_eq!(rows1.len(), 4, "{rows1:?}");
    assert_eq!(rows1.iter().filter(|r| r.is_reference).count(), 1);
    let reference_id = rc1.reference_frame_id.unwrap();
    assert!(rows1
        .iter()
        .any(|r| r.frame_id == reference_id && r.is_reference));

    // `stacking_run_frames` rows: one per frame of the run's group, all
    // four included and aligned (one of them the reference).
    let frame_rows = crate::db::stacking::list_frame_rows(&fixture.conn, rc1.run_id).unwrap();
    assert_eq!(frame_rows.len(), 4, "{frame_rows:?}");
    assert!(frame_rows.iter().all(|r| r.included));
    assert!(frame_rows.iter().all(|r| r.metrics_json.is_some()));
    assert!(frame_rows
        .iter()
        .all(|r| matches!(r.reg_status.as_deref(), Some("aligned" | "reference"))));
    assert_eq!(
        frame_rows
            .iter()
            .filter(|r| r.reg_status.as_deref() == Some("reference"))
            .count(),
        1
    );
    let before_at: HashMap<i64, String> = rows1
        .iter()
        .map(|r| (r.frame_id, r.registered_at.clone()))
        .collect();
    let before_hash: HashMap<i64, Option<String>> = rows1
        .iter()
        .map(|r| (r.frame_id, r.config_hash.clone()))
        .collect();

    // Second pass, same config: every row is reused verbatim.
    let rc2 = run_pass(StackingConfig::default());
    assert_eq!(rc2.reference_frame_id, rc1.reference_frame_id);
    let rows2 = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    let after_at: HashMap<i64, String> = rows2
        .iter()
        .map(|r| (r.frame_id, r.registered_at.clone()))
        .collect();
    assert_eq!(
        before_at, after_at,
        "a fresh registration row must not be rewritten"
    );

    // Every frame's OWN registration outcome on the second pass is a
    // reuse (`cached: true`) — not merely a DB row that happens to
    // look the same, but a run that provably never called
    // `register_frame`/`identity_registration` again.
    for entries in rc2.measured.values() {
        for entry in entries {
            match &entry.registration {
                Some(RegisteredFrameOutcome::Aligned { cached, .. }) => {
                    assert!(
                        *cached,
                        "frame {} was re-registered on the second pass",
                        entry.frame.frame_id
                    );
                }
                Some(RegisteredFrameOutcome::Failed(msg)) => {
                    panic!("frame {} failed registration: {msg}", entry.frame.frame_id);
                }
                None => {
                    panic!("frame {} has no registration outcome", entry.frame.frame_id);
                }
            }
        }
    }

    // Third pass, `max_stars` changed: the registration config hash
    // changes for every frame, so every row must re-register (asserted
    // via the hash, not `registered_at` — a fast, all-in-memory test
    // pass can legitimately land in the same millisecond twice).
    let mut cfg3 = StackingConfig::default();
    cfg3.registration.max_stars = 50;
    let _rc3 = run_pass(cfg3);
    let rows3 = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    let after_hash: HashMap<i64, Option<String>> = rows3
        .iter()
        .map(|r| (r.frame_id, r.config_hash.clone()))
        .collect();
    for (id, hash) in &before_hash {
        assert_ne!(
            &after_hash[id], hash,
            "frame {id} must re-register after cfg.registration.max_stars changes"
        );
    }
}

/// Ruling R3 (c), the run: a mixed mono + debayered-OSC set registered onto a mono reference. A repeat pass
/// reuses every row; moving `measurement.maxStars` re-registers the OSC frames too (they read the reference's
/// stars out of Measure's `fits.0`, which the new config rewrote), where before R3 they stayed cached against
/// star lists the reference no longer matched.
#[test]
fn an_osc_frame_re_registers_when_the_mono_references_measurement_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let mut mono_ids = Vec::new();
    let mut osc_ids = Vec::new();
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        let date_obs = date_obs_at(i);
        let stem = format!("m{i}");
        let spec = LightSpec {
            filter: Some("L"),
            ..star_light_spec(&stem, &date_obs)
        };
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &shifted_stars(dx, dy),
            600.0,
            5.0,
            100 + i as u64,
        );
        mono_ids.push(id);
    }
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        let date_obs = date_obs_at(i + 4);
        let stem = format!("o{i}");
        let spec = LightSpec {
            bayerpat: Some("RGGB"),
            ..star_light_spec(&stem, &date_obs)
        };
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &shifted_stars(dx, dy),
            600.0,
            5.0,
            120 + i as u64,
        );
        osc_ids.push(id);
    }
    let all_ids: Vec<i64> = mono_ids.iter().chain(osc_ids.iter()).copied().collect();
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &all_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, mono_ids[0])
        .unwrap();
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, working.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let run_pass = |cfg: StackingConfig| -> RunContext {
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 2, "one mono group, one OSC group");
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
            layout.clone(),
            output.path().to_path_buf(),
            group_ids,
        );
        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        rc
    };
    // Whether each OSC frame's registration this pass was reused.
    let osc_cached = |rc: &RunContext| -> Vec<bool> {
        let mut out = Vec::new();
        for entries in rc.measured.values() {
            for e in entries.iter().filter(|e| osc_ids.contains(&e.frame.frame_id)) {
                match &e.registration {
                    Some(RegisteredFrameOutcome::Aligned { cached, .. }) => out.push(*cached),
                    _ => panic!("osc frame {} not aligned", e.frame.frame_id),
                }
            }
        }
        out
    };

    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    let rc1 = run_pass(cfg.clone());
    assert_eq!(rc1.reference_frame_id, Some(mono_ids[0]));
    assert_eq!(osc_cached(&rc1), vec![false; 4], "a first pass registers");

    let rc2 = run_pass(cfg.clone());
    assert_eq!(osc_cached(&rc2), vec![true; 4], "the same config reuses");

    // R5 M2, the round trip on this fixture: after the repeat pass the gate reads Register not stale under `cfg`,
    // and stale under `moved` before the run that moves, because the OSC rows' registration hash folds the
    // mono reference's measurement hash (ruling R3 (a)). It pins that the gate and the run agree on this one
    // reference move, not the gate's staleness in general.
    let register_stale = |cfg: &StackingConfig| -> bool {
        crate::stacking::plan::build_plan(
            &fixture.conn,
            &ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(fixture.set_id),
            Some(cfg.clone()),
        )
        .expect("the plan should build")
        .stale_stages
        .contains(&Stage::Register)
    };
    let mut moved = cfg.clone();
    moved.measurement.max_stars = cfg.measurement.max_stars - 1000;
    assert!(!register_stale(&cfg), "the run's own rows read fresh at the gate");
    assert!(
        register_stale(&moved),
        "the gate reads Register stale under the moved reference measurement before it runs"
    );
    let rc3 = run_pass(moved.clone());
    assert_eq!(
        osc_cached(&rc3),
        vec![false; 4],
        "the reference's measurement moved: the OSC frames re-register"
    );
}

/// Ruling R-T3-3: `SummaryGroup::reference_frame_id` is `Some` ONLY
/// when the group's master was WRITTEN — never for a group that got as
/// far as registration and then lost its master anyway.
///
/// The fixture is the narrow path fix round 1's "did stage 5 touch this
/// group" test could not see: two real star frames plus two starless
/// ones, so the group IS viable at stage 5 (4 included ≥ 3), stage 5
/// registers all four and the two starless ones fail RANSAC, leaving 2
/// — below the member floor. The group is skipped at Output with no
/// master, and must therefore report no reference, even though its
/// frames carry real registration outcomes (both `aligned` and
/// `failed`, asserted below as the premise).
#[test]
fn a_group_that_loses_its_master_after_registering_reports_no_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0)];
    let noise = [4.0f32; 2];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);

    // Two flat-field frames: constant background + noise, no stars —
    // the registration detector finds nothing on them, so RANSAC
    // refuses both with too few inliers.
    let mut all_ids = light_ids.clone();
    for i in 0..2 {
        let flat_date = date_obs_at(2 + i);
        let stem = format!("flat{i}");
        let flat_spec = star_light_spec(&stem, &flat_date);
        let (flat_id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &flat_spec,
            &[],
            600.0,
            4.0,
            777 + i as u64,
        );
        all_ids.push(flat_id);
    }
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &all_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // `WeightMode::None` keeps stage 3's weight filter from dropping
    // the starless frames before registration ever sees them (the
    // sibling test's own reasoning); `best_by_weight`'s star-count
    // tie-break still puts a real star frame in the reference seat.
    let mut cfg = StackingConfig::default();
    cfg.measurement.weight_mode = WeightMode::None;
    cfg.registration.geometry = RegistrationGeometry::Native;

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let group_key = plan_groups[0].key.clone();
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let mut rc = test_context(
        ctx,
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output.path().to_path_buf(),
        group_ids,
    );

    run_stages_for_test(&mut rc, Stage::Register).unwrap();
    // No group produced a master, so the Output stage itself fails —
    // but it pushes every group's summary before it says so.
    let result = stage_output(&mut rc);
    assert!(result.is_err(), "the run has no master to hand back");

    let group = summary_group(&rc, &group_key);
    assert_eq!(group.master_path, None, "{group:?}");
    assert_eq!(
        group.reference_frame_id, None,
        "a group with no master reports no reference: {group:?}"
    );
    // Premise: this group really did register — the two star frames
    // aligned and the two starless ones failed, so "stage 5 never
    // touched it" is NOT why the reference is absent.
    assert!(
        group
            .frames
            .iter()
            .any(|f| f.reg_status.as_deref() == Some("aligned")),
        "{:?}",
        group.frames
    );
    assert!(
        group
            .frames
            .iter()
            .any(|f| f.reg_status.as_deref() == Some("failed")),
        "{:?}",
        group.frames
    );

    // The skip is visible: one run warning that says why, the same reason on
    // the group row's `error`, and the group named by the completion event's
    // source list (`groups_without_master` reads the summary's groups).
    // Register excluded the two starless frames, so the group reaches the Output boundary with 2 included.
    let reason = "fewer than 3 included frames";
    let expected = format!("group {group_key} skipped: {reason}");
    let lines: Vec<&String> = rc.warnings.iter().filter(|w| **w == expected).collect();
    assert_eq!(lines.len(), 1, "{:?}", rc.warnings);
    let rows = crate::db::stacking::list_groups(&fixture.conn, rc.run_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "skipped");
    assert_eq!(rows[0].error.as_deref(), Some(reason));
    let without_master: Vec<&str> = rc
        .summary
        .groups
        .iter()
        .filter(|g| g.master_path.is_none())
        .map(|g| g.key.as_str())
        .collect();
    assert_eq!(without_master, vec![group_key.as_str()]);
}

#[test]
fn registration_failure_excludes_by_default_and_fails_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0)];
    let noise = [4.0f32; 3];
    let (fixture, light_ids, working, output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);

    // A flat-field frame: constant background + noise, no stars — the
    // registration detector finds nothing on it, so RANSAC refuses it
    // with too few inliers.
    let flat_date = date_obs_at(3);
    let flat_spec = star_light_spec("flat", &flat_date);
    let (flat_id, _path) =
        test_fixtures::add_light_with_field(&fixture, &flat_spec, &[], 600.0, 4.0, 777);

    let mut all_ids = light_ids.clone();
    all_ids.push(flat_id);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &all_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();

    // `WeightMode::None` gives every frame the SAME weight, so the
    // starless flat frame is not excluded by stage 3's own weight
    // filter before ever reaching registration — isolating THIS test's
    // signal to the registration-failure path alone.
    let mut base_cfg = StackingConfig::default();
    base_cfg.measurement.weight_mode = WeightMode::None;

    let run_pass = |cfg: StackingConfig| -> (RunContext, Result<(), RunError>) {
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
            layout.clone(),
            output_dir.clone(),
            group_ids,
        );
        let result = run_stages_for_test(&mut rc, Stage::Register);
        (rc, result)
    };

    // Pass A: default `exclude_on_registration_failure = true` — the
    // flat frame is excluded, the run otherwise succeeds.
    let (rc_a, result_a) = run_pass(base_cfg.clone());
    result_a.expect("run must succeed when the failing frame is simply excluded");
    let group_key = rc_a.plan_groups[0].key.clone();
    let entries = rc_a.measured.get(&group_key).unwrap();
    let flat_entry = entries
        .iter()
        .find(|e| e.frame.frame_id == flat_id)
        .unwrap();
    assert!(!flat_entry.included, "flat-field frame must be excluded");
    let reason = flat_entry.reason.clone().unwrap_or_default();
    assert!(reason.contains("registration failed"), "{reason}");
    for &id in &light_ids {
        assert!(
            entries
                .iter()
                .find(|e| e.frame.frame_id == id)
                .unwrap()
                .included,
            "frame {id} should remain included"
        );
    }

    // Pass B: `exclude_on_registration_failure = false` — the same
    // failure now fails the whole run.
    let mut cfg_b = base_cfg;
    cfg_b.selection.exclude_on_registration_failure = false;
    let (_rc_b, result_b) = run_pass(cfg_b);
    let err = result_b.expect_err("run must fail when registration failures are not excluded");
    match err {
        RunError::Other(msg) => assert!(msg.contains("registration failed"), "{msg}"),
        other => panic!("expected RunError::Other, got {other:?}"),
    }
}

#[test]
fn solve_of_reads_through_the_origin_and_treats_a_missing_origin_as_no_solve() {
    let f = test_fixtures::frame_set("LDN 1272");
    let (frame_id, _) = test_fixtures::add_light(
        &f,
        &LightSpec {
            stem: "a",
            instrume: "cam",
            filter: Some("L"),
            binning: 1,
            width: 64,
            height: 48,
            exptime: 60.0,
            date_obs: "2025-01-01T20:00:00",
            bayerpat: None,
            write_file: false,
        },
    );
    test_fixtures::seed_plate_solve_scale(&f.conn, frame_id, 1.5);

    let with_origin: FrameOrigins = Arc::new(
        [(
            frame_id,
            crate::stacking::origin::FrameOrigin::Catalog {
                file_id: 0,
                attested: false,
            },
        )]
        .into_iter()
        .collect(),
    );
    let mut solves = SolveCache::new();
    let found = solve_of(&mut solves, &f.conn, &with_origin, frame_id).unwrap();
    assert!((found.pixel_scale_arcsec - 1.5).abs() < 1e-9);

    let none: FrameOrigins = Arc::new(HashMap::new());
    let mut solves = SolveCache::new();
    assert!(solve_of(&mut solves, &f.conn, &none, frame_id).is_none());
}

/// Plan 6b-2 final review M1: the identity check precedes the
/// reference's star read, so a reference whose landed file changed
/// between Measure and Register fails the run before that read, in both
/// geometry modes.
#[test]
fn register_fails_on_a_changed_reference_before_reading_its_stars() {
    for native in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 4, 3);
        let mut cfg = StackingConfig::default();
        if native {
            cfg.registration.geometry = RegistrationGeometry::Native;
        }
        let mut rc = project_rc(
            &ctx,
            &conn,
            "p1",
            cfg,
            &tmp.path().join("work"),
            &tmp.path().join("out"),
        );
        measure_all(&mut rc, &HashMap::new());
        stage_reference(&mut rc).unwrap();
        let reference = rc.reference_frame_id.unwrap();
        let path = match origin_of(&rc.origins, reference).unwrap() {
            FrameOrigin::Project { landed_path, .. } => landed_path.clone(),
            other => panic!("not a project origin: {other:?}"),
        };
        // Make the reference's calibrated file unreadable as stars, so a
        // read before the check would fail with a different message.
        std::fs::write(&path, vec![0u8; 7000]).unwrap();
        let err = stage_register(&mut rc).unwrap_err();
        assert!(
            matches!(err, RunError::Other(ref m) if m.contains("the reference frame changed during the run")),
            "native={native}: {err:?}"
        );
    }
}

/// Ruling R15 (M1): the native Register total counts a group's dry pass only if, AFTER the identity check, the
/// group can still switch. A project group whose only other WCS candidate changed on disk drops it at the
/// Register boundary, so `two_pass_refine` runs no pass and the total is the members that register, once.
#[test]
fn native_register_total_counts_no_dry_pass_once_the_only_candidate_is_excluded() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    // One group of four: only g1 and g2 carry a manifest WCS, so g2 is g1's only switchable candidate.
    for i in 1..=4 {
        let uuid = format!("g{i}");
        let mut spec = test_fixtures::project_frame_spec(&uuid);
        spec.filter = "G";
        if i <= 2 {
            spec.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        }
        test_fixtures::add_project_frame(&conn, &tmp.path().join("collab"), "p1", &spec);
    }
    let weights: HashMap<&str, f64> = [("c_g1.fits", 0.9), ("c_g2.fits", 0.5)].into_iter().collect();
    let mut cfg = StackingConfig::default();
    cfg.registration.geometry = RegistrationGeometry::Native;
    assert!(cfg.reference.two_pass && cfg.reference.mode == ReferenceMode::Auto);
    let mut rc = project_rc(&ctx, &conn, "p1", cfg.clone(), &tmp.path().join("work"), &tmp.path().join("out"));
    measure_all(&mut rc, &weights);
    stage_reference(&mut rc).unwrap();
    let key = rc.plan_groups[0].key.clone();
    let reference = rc.geometry_of(&key).reference_frame_id;
    let candidate = rc.measured[&key]
        .iter()
        .find(|e| e.frame.filename == "c_g2.fits")
        .map(|e| e.frame.frame_id)
        .unwrap();
    assert_ne!(reference, candidate, "premise: g2 is not the reference");
    assert!(group_can_switch(&rc, &key, reference), "premise: g2 is a switchable candidate");

    // Untouched: the group runs a dry pass, so its four members count twice.
    assert_eq!(native_register_total(&mut rc, &cfg, None, false).unwrap(), 8);

    // g2's landed file changes: the check excludes it, the group keeps three members and nothing to switch to.
    let path = match origin_of(&rc.origins, candidate).unwrap() {
        FrameOrigin::Project { landed_path, .. } => landed_path.clone(),
        other => panic!("not a project origin: {other:?}"),
    };
    std::fs::write(&path, vec![0u8; 5000]).unwrap();
    assert_eq!(
        native_register_total(&mut rc, &cfg, None, false).unwrap(),
        3,
        "the total counted a dry pass `two_pass_refine` will not run"
    );
    assert_eq!(included_count_of(&rc, &key), 3);
    assert!(!group_can_switch(&rc, &key, reference));
}

fn run_to_register(rc: &mut RunContext) {
    stage_calibrate(rc).unwrap();
    stage_measure(rc).unwrap();
    stage_reference(rc).unwrap();
    stage_register(rc).unwrap();
}

/// Spec §5.5, plan 6b-3 D1, D2, D3: on a project's grid every registered
/// frame has the grid's size, NaN outside its footprint, its stars where
/// the grid's TAN puts their sky positions, `ATH_GRID` and the grid's own
/// row order. The star map is cached as a `registration` artifact, never
/// in `registration_results`.
#[test]
fn a_project_frame_registers_onto_the_grid() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let cfg = sky_config(&conn);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    let run = Arc::new(crate::stacking::grid_map::GridRun::new(grid));
    rc.grid = Some(run.clone());
    run_to_register(&mut rc);

    let group = rc.plan_groups[0].key.clone();
    let entries = &rc.measured[&group];
    assert_eq!(
        entries.iter().filter(|e| e.included).count(),
        4,
        "{:?}",
        entries
            .iter()
            .map(|e| (&e.frame.filename, &e.reason))
            .collect::<Vec<_>>()
    );
    for e in entries {
        let Some(RegisteredFrameOutcome::Aligned {
            map,
            registered_hash,
            ..
        }) = &e.registration
        else {
            panic!(
                "frame {} did not register: {:?}",
                e.frame.filename, e.reason
            );
        };
        assert!(
            map.is_flipped(),
            "a det CD > 0 frame lands mirrored on the grid"
        );
        let row = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group,
            ArtifactKind::Registered,
            Some(e.frame.frame_id),
        )
        .unwrap()
        .expect("a registered artifact");
        assert_eq!(&row.config_hash, registered_hash);
        let path = PathBuf::from(row.path.unwrap());
        let reader = crate::integration::plane_reader::PlaneReader::open(&path).unwrap();
        assert_eq!((reader.width(), reader.height()), (512, 384));
        let plane = reader.read_plane(0).unwrap();
        assert!(
            plane[0].is_nan() && plane[512 * 384 - 1].is_nan(),
            "NaN outside the footprint"
        );
        let header = crate::fits_parser::FitsHeader::from_path(&path).unwrap();
        assert_eq!(
            header.get_str("ATH_GRID").as_deref(),
            Some(run.hash.as_str())
        );
        assert_eq!(header.get_str("ROWORDER").as_deref(), Some("BOTTOM-UP"));
        assert!(header.get_str("ATH_PRJ").is_none());
        // The sky star nearest the target lands where the grid puts it.
        let stars = test_fixtures::sky_stars((83.8, -5.4), 0.12, 50.0, 7);
        let star = stars
            .iter()
            .min_by(|a, b| {
                let da = (a.0 - 83.8).hypot(a.1 + 5.4);
                let db = (b.0 - 83.8).hypot(b.1 + 5.4);
                da.total_cmp(&db)
            })
            .unwrap();
        let (gx, gy) = run.tan.sky_to_pixel(star.0, star.1);
        let (mx, my) = centroid_near(&plane, 512, gx, gy);
        assert!(
            (mx - gx).hypot(my - gy) < 0.5,
            "{}: star at ({mx}, {my}), grid says ({gx}, {gy})",
            e.frame.filename
        );
        // A star 100 px or more from the grid centre pins rotation and
        // scale, not just translation and parity.
        let far = stars
            .iter()
            .map(|s| (s, run.tan.sky_to_pixel(s.0, s.1)))
            .filter(|(_, (x, y))| {
                (x - 256.0).hypot(y - 192.0) >= 100.0
                    && (20.0..492.0).contains(x)
                    && (20.0..364.0).contains(y)
            })
            .min_by(|a, b| {
                let da = (a.1 .0 - 256.0).hypot(a.1 .1 - 192.0);
                let db = (b.1 .0 - 256.0).hypot(b.1 .1 - 192.0);
                da.total_cmp(&db)
            })
            .expect("a sky star at least 100 px from the grid centre");
        let (fx, fy) = far.1;
        let (mx, my) = centroid_near(&plane, 512, fx, fy);
        assert!(
            (mx - fx).hypot(my - fy) < 0.5,
            "{}: far star at ({mx}, {my}), grid says ({fx}, {fy})",
            e.frame.filename
        );
    }
    let stars_cached = crate::stacking::plan::stored_registrations(&conn, &rc.subject).unwrap();
    assert_eq!(stars_cached.len(), 4);
    let catalog_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM registration_results", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        catalog_rows, 0,
        "a project never writes registration_results"
    );
}

/// Spec §5.5, plan 6b-3 D2: moving the grid re-composes the cached star
/// maps and rewrites every registered frame — no star detection runs.
#[test]
fn a_grid_change_recomposes_without_star_detection() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let cfg = sky_config(&conn);
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    run_to_register(&mut rc);
    let group = rc.plan_groups[0].key.clone();
    let before: HashMap<i64, String> = rc.measured[&group]
        .iter()
        .map(|e| {
            let row = crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group,
                ArtifactKind::Registered,
                Some(e.frame.frame_id),
            )
            .unwrap()
            .unwrap();
            (e.frame.frame_id, row.config_hash)
        })
        .collect();

    let moved = crate::collab::grid::CanonicalGrid {
        crval2: grid.crval2 + 0.005,
        ..grid
    };
    let moved_run = Arc::new(crate::stacking::grid_map::GridRun::new(moved));
    rc.grid = Some(Arc::clone(&moved_run));
    let detected = |dir: &Path| {
        crate::stacking::register::frame::DETECT_FRAME_STARS_LOG
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(dir))
            .count()
    };
    let detected_before = detected(tmp.path());
    stage_register(&mut rc).unwrap();
    assert_eq!(
        detected_before,
        detected(tmp.path()),
        "a grid move runs no star detection"
    );
    for e in &rc.measured[&group] {
        let Some(RegisteredFrameOutcome::Aligned { cached, .. }) = &e.registration else {
            panic!("{} lost its registration", e.frame.filename);
        };
        assert!(*cached, "the star map is reused");
        let row = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group,
            ArtifactKind::Registered,
            Some(e.frame.frame_id),
        )
        .unwrap()
        .unwrap();
        assert_ne!(
            row.config_hash, before[&e.frame.frame_id],
            "the registered frame is rewritten"
        );
        let header =
            crate::fits_parser::FitsHeader::from_path(&PathBuf::from(row.path.unwrap()))
                .unwrap();
        assert_eq!(
            header.get_str("ATH_GRID").as_deref(),
            Some(moved_run.hash.as_str()),
            "the frame was rewritten for the moved grid"
        );
    }
}

/// Plan 6b-3 D2: the plan and the run agree — after a run the plan reports
/// Register cached; a moved grid makes it stale.
#[test]
fn a_project_plan_reports_register_cached_then_stale_on_a_grid_change() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let cfg = sky_config(&conn);
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
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        cfg,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    run_to_register(&mut rc);

    let subject = StackSubject::Project("p1".into());
    let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &subject, None).unwrap();
    assert!(
        !plan.stale_stages.contains(&Stage::Register),
        "{:?}",
        plan.stale_stages
    );

    let moved = crate::collab::grid::CanonicalGrid {
        crval1: grid.crval1 + 0.01,
        ..grid
    };
    crate::db::collab::set_canonical_grid(&conn, "p1", Some(&moved.canonical_json())).unwrap();
    let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &subject, None).unwrap();
    assert!(
        plan.stale_stages.contains(&Stage::Register),
        "{:?}",
        plan.stale_stages
    );
}
