use super::*;
use crate::events::NullEmitter;
use crate::registration::db::get_registration_for_frame_set;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::integrate::{LargeScaleRejection, RejectionChoice};
use crate::stacking::rej::RejBitmap;
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, shifted_stars, star_light_spec, wait_for_run,
    wait_for_run_within, LightSpec, BASE_STARS, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

/// Route E: `SummaryFrame.field_width_at_floor` is true when any channel's field width sat at the floor, false
/// otherwise and for a frame without a measurement; a summary written before the field reads it as false.
#[test]
fn the_summary_frame_carries_the_floor_flag() {
    use crate::stacking::groups::GroupFrame;
    use crate::stacking::measure::{ChannelMeasurement, FrameMeasurement};
    let channel = |flag: bool| ChannelMeasurement {
        field_width_at_floor: flag,
        ..Default::default()
    };
    let entry = |measurement: Option<FrameMeasurement>| MeasuredFrame {
        frame: GroupFrame {
            frame_id: 1,
            file_id: 1,
            filename: "f.fits".into(),
            path: "/f.fits".into(),
            size: 1,
            modified_at: "2026-01-01".into(),
            exposure_s: Some(60.0),
            date_obs: None,
            width: 64,
            height: 48,
            pixel_scale_arcsec: None,
            scale_source: None,
        },
        calibrated: None,
        planes: 1,
        measurement,
        weight: None,
        included: true,
        reason: None,
        registration: None,
        cached_calibrated: false,
        cached_metrics: false,
        ln_scale: None,
        cached_ln: false,
        ln_growth_factor: None,
        ln_growth_pairs: None,
        ln_aperture_correction: None,
        measure_floored: false,
    };
    let frame = |flags: &[bool]| FrameMeasurement {
        width: 64,
        height: 48,
        channels: flags.iter().map(|&f| channel(f)).collect(),
        duration_ms: 0,
    };
    assert!(
        summary_frame_for(&entry(Some(frame(&[false, true, false]))), None)
            .field_width_at_floor
    );
    assert!(
        !summary_frame_for(&entry(Some(frame(&[false, false]))), None).field_width_at_floor
    );
    assert!(!summary_frame_for(&entry(None), None).field_width_at_floor);
    let mut json = serde_json::to_value(summary_frame_for(&entry(None), None)).unwrap();
    json.as_object_mut().unwrap().remove("fieldWidthAtFloor");
    let back: SummaryFrame = serde_json::from_value(json).unwrap();
    assert!(!back.field_width_at_floor);
}

/// A frame nothing has measured yet, for the summary-row tests of Plan B B0.
pub(super) fn bare_measured_frame(frame_id: i64) -> MeasuredFrame {
    use crate::stacking::groups::GroupFrame;
    MeasuredFrame {
        frame: GroupFrame {
            frame_id,
            file_id: frame_id,
            filename: "f.fits".into(),
            path: "/f.fits".into(),
            size: 1,
            modified_at: "2026-01-01".into(),
            exposure_s: Some(60.0),
            date_obs: None,
            width: 64,
            height: 48,
            pixel_scale_arcsec: None,
            scale_source: None,
        },
        calibrated: None,
        planes: 1,
        measurement: None,
        weight: None,
        included: true,
        reason: None,
        registration: None,
        cached_calibrated: false,
        cached_metrics: false,
        ln_scale: None,
        cached_ln: false,
        ln_growth_factor: None,
        ln_growth_pairs: None,
        ln_aperture_correction: None,
        measure_floored: false,
    }
}

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

/// Plan B B0 (PB-2): the run summary keeps LN's growth factor, its pair count and the target side's `c`. A frame LN
/// did not compute (or an LN cache hit) reads `null` for all three; a summary written before B0, which has no such
/// keys, decodes with `None`; values survive the round trip.
#[test]
fn summary_frame_ln_fields_default_none_and_round_trip() {
    let bare = summary_frame_for(&bare_measured_frame(1), None);
    assert_eq!(
        (
            bare.ln_growth_factor,
            bare.ln_growth_pairs,
            bare.ln_aperture_correction
        ),
        (None, None, None)
    );
    let mut json = serde_json::to_value(&bare).unwrap();
    for key in ["lnGrowthFactor", "lnGrowthPairs", "lnApertureCorrection"] {
        let removed = json.as_object_mut().unwrap().remove(key);
        assert_eq!(
            removed,
            Some(serde_json::Value::Null),
            "{key} is null in the payload, never skipped"
        );
    }
    let old: SummaryFrame = serde_json::from_value(json).unwrap();
    assert_eq!(
        (
            old.ln_growth_factor,
            old.ln_growth_pairs,
            old.ln_aperture_correction
        ),
        (None, None, None)
    );

    let mut entry = bare_measured_frame(2);
    entry.ln_growth_factor = Some(1.02);
    entry.ln_growth_pairs = Some(17);
    entry.ln_aperture_correction = Some(1.11);
    let json = serde_json::to_value(summary_frame_for(&entry, None)).unwrap();
    assert_eq!(json["lnGrowthFactor"], serde_json::json!(1.02));
    assert_eq!(json["lnGrowthPairs"], serde_json::json!(17));
    assert_eq!(json["lnApertureCorrection"], serde_json::json!(1.11));
    let back: SummaryFrame = serde_json::from_value(json).unwrap();
    assert_eq!(
        (
            back.ln_growth_factor,
            back.ln_growth_pairs,
            back.ln_aperture_correction
        ),
        (Some(1.02), Some(17), Some(1.11))
    );
}

/// Plan B B0 (PB-2, F1's floor (ii)): a member whose growth curve had no pair on both frames reports
/// `growth_pairs: Some(0)` with the kept ratio (`growth_factor: Some(1.0)`); both land on its row as they are, not
/// as an absent value. A frame LN did not normalize, and an LN cache hit, leave all three `None`.
#[test]
fn ln_summary_keeps_growth_pairs_and_factor_for_a_zero_pair_member() {
    let outcome = ln::LnFrameOutcome {
        scale: 0.98,
        growth_pairs: Some(0),
        growth_factor: Some(1.0),
        target_aperture_correction: Some(1.12),
        ..Default::default()
    };
    let mut entry = bare_measured_frame(3);
    apply_ln_summary(
        &mut entry,
        Some(outcome.scale),
        false,
        outcome.growth_factor,
        outcome.growth_pairs,
        outcome.target_aperture_correction,
    );
    let row = summary_frame_for(&entry, None);
    assert_eq!(row.ln_scale, Some(0.98));
    assert_eq!(row.ln_growth_pairs, Some(0));
    assert_eq!(row.ln_growth_factor, Some(1.0));
    assert_eq!(row.ln_aperture_correction, Some(1.12));

    // An LN cache hit carries only the scale; a frame LN did not normalize carries nothing.
    let mut cached = bare_measured_frame(4);
    apply_ln_summary(&mut cached, Some(0.97), true, None, None, None);
    let row = summary_frame_for(&cached, None);
    assert_eq!((row.ln_scale, row.cached_ln), (Some(0.97), true));
    assert_eq!(
        (
            row.ln_growth_factor,
            row.ln_growth_pairs,
            row.ln_aperture_correction
        ),
        (None, None, None)
    );
}

fn light_spec<'a>(stem: &'a str, date_obs: &'a str) -> LightSpec<'a> {
    LightSpec {
        stem,
        instrume: "cam",
        filter: None,
        binning: 1,
        width: 64,
        height: 48,
        exptime: 60.0,
        date_obs,
        bayerpat: None,
        write_file: true,
    }
}

const THREE_TIMES: [&str; 3] = [
    "2025-01-01T00:00:00",
    "2025-01-01T00:05:00",
    "2025-01-01T00:10:00",
];
pub(super) const SET_NAME: &str = "LDN 1272";

struct Recording(std::sync::Mutex<Vec<(String, serde_json::Value)>>);

impl Recording {
    fn new() -> Self {
        Recording(std::sync::Mutex::new(Vec::new()))
    }
    fn events(&self, name: &str) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

impl ProgressEmitter for Recording {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        self.0
            .lock()
            .unwrap()
            .push((event_name.to_string(), payload));
    }
}

/// Seed a ready-to-run fixture (3 lights + linked master dark/flat +
/// `stacking.working_dir`/`stacking.output_dir`) onto `ctx`'s OWN
/// on-disk catalog (`db_path` — the SAME path `ctx`'s pooled `Database`
/// was opened on), by opening a second, plain `Connection` to that file
/// and driving it through the SAME `test_fixtures` helpers `plan.rs`/
/// `groups.rs`'s tests use. A `ServiceContext`'s pooled `Database` can
/// only see rows committed to an on-disk file — never a private
/// in-memory `Connection` — so this is the one way to reuse the proven
/// fixture builder for a `start_stacking`/`RunContext`-level test.
fn seed_ready(
    db_path: &Path,
    set_name: &str,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, t) in THREE_TIMES.iter().enumerate() {
        let (id, _path) = test_fixtures::add_light(&fixture, &light_spec(&format!("f{i}"), t));
        light_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);

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

    (fixture, light_ids, working, output)
}

/// Runs stage 1 on a fresh fixture whose three lights link the master
/// flat, and the master dark only when `keep_dark`; returns the context
/// (for a second run) and the group key.
fn pedestal_stage_one(
    ctx: &Arc<ServiceContext>,
    db_path: &Path,
    keep_dark: bool,
    flat_norm: bool,
) -> (RunContext, String, tempfile::TempDir, tempfile::TempDir) {
    let fixture_conn = rusqlite::Connection::open(db_path).unwrap();
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let mut light_ids = Vec::new();
    for (i, t) in THREE_TIMES.iter().enumerate() {
        let (id, _) = test_fixtures::add_light(&fixture, &light_spec(&format!("f{i}"), t));
        light_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);
    if !keep_dark {
        fixture
            .conn
            .execute(
                "DELETE FROM calibration_set_to_frames \
                     WHERE source_type = 'frame' AND calibration_type = 'Dark'",
                [],
            )
            .unwrap();
    }
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let mut cfg = StackingConfig::default();
    cfg.calibration.flat_norm = flat_norm;
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let group_key = plan_groups[0].key.clone();
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output.path().to_path_buf(),
        HashMap::new(),
    );
    stage_calibrate(&mut rc).unwrap();
    (rc, group_key, working, output)
}

/// Spec 2026-10-04 §5.1-§5.3: a group whose lights link the master flat
/// only gets ONE run warning with the flat's corner ratio, again when
/// stage 1 is fully cached; a group with its dark links gets none.
#[test]
fn a_flat_only_group_gets_one_run_warning_with_the_corner_ratio() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (mut rc, key, _w, _o) = pedestal_stage_one(&ctx, &db_path, false, true);

    // The fixture flat is a constant 0.5 plane stamped ATH_FNRM = 0.5
    // and flat normalization is on: corners = centre -> ratio 1.
    let prefix =
        format!("group {key}: 3 lights were calibrated with a flat but no dark or bias");
    let hits: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", rc.warnings);
    assert!(
        hits[0].contains(
            "The flat's darkest corner reads 100 % of its centre, so the offset pedestal is 1.0\u{d7} larger there."
        ),
        "{}",
        hits[0]
    );

    // Second run: every frame's stage-1 artifact is fresh (all Reused).
    rc.warnings.clear();
    stage_calibrate(&mut rc).unwrap();
    assert!(
        rc.cached_calibrated.values().all(|c| *c),
        "stage 1 not cached"
    );
    let again = rc
        .warnings
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .count();
    assert_eq!(again, 1, "{:?}", rc.warnings);

    // Dark links kept: no such warning.
    let tmp2 = tempfile::tempdir().unwrap();
    let db2 = tmp2.path().join("catalog.db");
    let ctx2 = Arc::new(ServiceContext::new_for_tests(db2.clone()));
    let (rc2, _k2, _w2, _o2) = pedestal_stage_one(&ctx2, &db2, true, true);
    assert!(
        !rc2.warnings
            .iter()
            .any(|w| w.contains("flat but no dark or bias")),
        "{:?}",
        rc2.warnings
    );
}

/// The corner ratio is corner over centre, so switching flat
/// normalization off must not change the sentence.
#[test]
fn the_corner_ratio_does_not_depend_on_flat_normalization() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (rc, key, _w, _o) = pedestal_stage_one(&ctx, &db_path, false, false);
    let prefix =
        format!("group {key}: 3 lights were calibrated with a flat but no dark or bias");
    let hits: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", rc.warnings);
    assert!(
        hits[0].contains(
            "reads 100 % of its centre, so the offset pedestal is 1.0\u{d7} larger there."
        ),
        "{}",
        hits[0]
    );
}

// ── start_stacking / cancel_stacking ────────────────────────────────

#[test]
fn start_refuses_blocked_plans_and_double_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    // No lights at all yet: the plan is blocked ("frames" — fewer than
    // 3 included) — start refuses with Invalid.
    let empty_conn = rusqlite::Connection::open(&db_path).unwrap();
    let empty_fixture = test_fixtures::frame_set_with_conn(empty_conn, "Empty");
    let err = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(empty_fixture.set_id),
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    drop(empty_fixture);

    // A ready fixture (masters + folders), on the SAME catalog. Fix
    // round 1, item 6: hold the queue's one slot so the first run
    // parks in `acquire` — start twice → the second is a Conflict
    // deterministically, with no race against the first run's own
    // completion (same technique as `cancel_before_admission_finishes_cancelled`).
    let (fixture, light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);
    let _ = &light_ids;

    let hold_flag = Arc::new(AtomicBool::new(false));
    let (hold_permit, _hold_job) = ctx
        .compute_queue
        .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
        .unwrap();

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("first start should succeed");

    let second = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(second, ApiError::Conflict(_)), "{second:?}");

    cancel_stacking(&ctx, started.run_id).unwrap();
    drop(hold_permit);

    wait_for_run(&ctx, started.run_id);
}

/// Plan 5b final fix wave, review finding B2: a failure AFTER the
/// `active_stacks` handle is registered but BEFORE the run thread
/// actually spawns must never wedge the frame set. Before
/// `StartStackingGuard` existed, such a failure (here: the per-group
/// `insert_group` loop, via the brief's own suggested fault — a
/// `stacking_run_groups` write failing) returned via `?` straight past
/// all cleanup: the handle stayed in `active_stacks` forever (a
/// process-memory map `heal_interrupted_runs` cannot see past) and the
/// `stacking_runs` row stayed `"planning"` forever, so every later
/// `start_stacking` for the same set answered `Conflict` until restart.
#[test]
fn start_stacking_never_wedges_a_set_when_insert_group_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);

    // Fault injection: the per-group insert loop writes to
    // `stacking_run_groups`; `insert_run` (the row that must NOT be
    // left stuck) and the handle registration both land against
    // `stacking_runs`, an untouched table, so they still succeed.
    fixture
        .conn
        .execute("DROP TABLE stacking_run_groups", [])
        .unwrap();

    let err = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect_err("insert_group failing must surface as an error, not wedge the set");
    assert!(matches!(err, ApiError::Internal(_)), "{err:?}");

    assert!(
        ctx.active_stacks.lock().unwrap().is_empty(),
        "the guard must remove the handle on this early-return path"
    );

    let rows = crate::db::stacking::list_runs(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].status, "failed", "must never stay stuck planning");
    assert!(rows[0].finished_at.is_some());
    assert!(rows[0].error.is_some());

    // Undo the fault and confirm the set is no longer wedged: a second
    // `start_stacking` is admitted, not `Conflict`.
    crate::db::init_db(&fixture.conn).unwrap();
    let second = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("a second start on the same set must be admitted after the failed setup");

    cancel_stacking(&ctx, second.run_id).unwrap();
    wait_for_run(&ctx, second.run_id);
}

#[test]
fn admission_reads_the_budget_none_default_and_setting() {
    let gib = 1u64 << 30;
    // Unknown budget: one worker, exactly as an unknown RAM total was.
    assert_eq!(admission(gib, 16, None), 1);
    // The default budget is a quarter of RAM: the number is today's.
    if let Some(total) = total_ram_bytes() {
        let today = (((total / 4) / gib).clamp(1, 16)) as usize;
        assert_eq!(admission(gib, 16, Some(total / 4)), today);
        assert_eq!(
            admission_with_shared(gib / 2, gib, 16, Some(total / 4)),
            (((total / 4).saturating_sub(gib / 2) / gib).clamp(1, 16)) as usize
        );
    }
    // A setting drives it, clamped to [1, max_workers].
    assert_eq!(admission(gib, 16, Some(5 * gib)), 5);
    assert_eq!(admission(gib, 4, Some(64 * gib)), 4);
    assert_eq!(admission(gib, 16, Some(gib / 4)), 1);
    assert_eq!(admission_with_shared(10 * gib, gib, 16, Some(2 * gib)), 1);
    assert_eq!(admission_with_shared(gib, gib, 16, Some(5 * gib)), 4);
}

#[test]
fn fan_budget_follows_the_live_setting() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(dir.path().join("t.db"));
    let auto = machine::memory_budget(&ctx);
    assert_eq!(auto.map(|(_, w)| w).unwrap_or("auto"), "auto");
    let conn = ctx.db.get().unwrap().conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "2048")
        .unwrap();
    drop(conn);
    assert_eq!(machine::memory_budget(&ctx), Some((2048 << 20, "setting")));
}

#[test]
fn cancel_before_admission_finishes_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);

    // Occupy the queue's one slot with a dummy Analysis permit from this
    // test thread, so the stacking run below is admitted only once it
    // is dropped.
    let hold_flag = Arc::new(AtomicBool::new(false));
    let (hold_permit, _hold_job) = ctx
        .compute_queue
        .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
        .unwrap();

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
    .expect("start should succeed even while the queue is held");

    // Still queued behind the held permit — must not be 'running' yet.
    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_ne!(row.status, "running", "must not be running while queued");

    cancel_stacking(&ctx, started.run_id).unwrap();
    drop(hold_permit);

    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "cancelled");
    assert!(row.finished_at.is_some());

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
    assert_eq!(completes[0]["success"].as_bool(), Some(false));
    assert_eq!(completes[0]["setId"], serde_json::json!(fixture.set_id));
    assert!(
        completes[0].get("projectId") == Some(&serde_json::Value::Null),
        "{:?}",
        completes[0]
    );

    // Never even reached "running" — the queue wait handles it before
    // `run_pipeline` gets that far.
    let running_events: Vec<_> = recorder
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["stage"] == "calibrate")
        .collect();
    assert!(
        running_events.is_empty(),
        "a queue-cancelled run must never reach stage 1: {running_events:?}"
    );
}

#[test]
fn thread_emits_complete_exactly_once_even_on_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);

    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    let run_id = insert_run(
        &fixture.conn,
        &NewRun {
            subject: &StackSubject::FrameSet(fixture.set_id),
            config_json: "{}",
            config_hash: "test-hash",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.path().to_str().unwrap(),
            output_dir: output.path().to_str().unwrap(),
        },
    )
    .unwrap();

    let recorder = Arc::new(Recording::new());
    let mut rc = test_context(
        ctx.clone(),
        recorder.clone(),
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output.path().to_path_buf(),
        HashMap::new(),
    );
    rc.fail_after_stage = Some(Stage::Calibrate);
    let cancel = rc.cancel.clone();

    {
        let mut active = ctx.active_stacks.lock().unwrap();
        active.insert(
            run_id,
            StackHandle {
                cancel_flag: cancel,
                subject: StackSubject::FrameSet(fixture.set_id),
            },
        );
    }

    let handle = std::thread::Builder::new()
        .name(format!("stacking-run-{run_id}"))
        .spawn(move || {
            run_thread(rc);
        })
        .unwrap();
    handle
        .join()
        .expect("the injected panic must be caught inside run_thread, never propagate");

    wait_for_run(&ctx, run_id);

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["success"].as_bool(), Some(false));
    let error = completes[0]["error"].as_str().expect("error text present");
    assert!(error.contains("panicked"), "{error}");

    assert!(
        !ctx.active_stacks.lock().unwrap().contains_key(&run_id),
        "handle removed"
    );

    let row = crate::db::stacking::get_run(&fixture.conn, run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.error.as_deref().unwrap_or("").contains("panicked"));
}

// ── Task 13c L1: Calibrate working set + parallel master decode ─────

/// The measured per-frame working set, per colour mode: mono 3 planes,
/// a debayered OSC group 5 (was a flat 8). Admission on the hc11 and
/// set 204 geometries against a 16 GiB-machine (4 GiB) and a 128 GiB
/// -machine (32 GiB) budget.
#[test]
fn calibrate_working_set_is_per_colour_mode() {
    let (w, h) = (9576u64, 6388u64);
    let plane = w * h * 4;
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Mono, false), 3 * plane);
    // A mono group never holds a debayer output, whatever the flag says.
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Mono, true), 3 * plane);
    // An OSC group kept as mosaics (debayer off) is mono-sized too.
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Osc, false), 3 * plane);
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Osc, true), 5 * plane);

    let gib = 1u64 << 30;
    // hc11: 978 743 808 shared bytes, 4 GiB budget -> admission 4; 128 GiB machine -> the pool.
    let hc11_ws = calibrate_working_set_bytes(w, h, ColorMode::Mono, false);
    assert_eq!(admission_with_shared(978_743_808, hc11_ws, 16, Some(4 * gib)), 4);
    assert_eq!(admission_with_shared(978_743_808, hc11_ws, 10, Some(32 * gib)), 10);
    // set 204 OSC (6248x4176, 199 MB shared): 7 on 4 GiB; mono capped by the pool at 10.
    let (w2, h2) = (6248u64, 4176u64);
    let osc = calibrate_working_set_bytes(w2, h2, ColorMode::Osc, true);
    let mono = calibrate_working_set_bytes(w2, h2, ColorMode::Mono, false);
    assert_eq!(admission_with_shared(199 << 20, osc, 16, Some(4 * gib)), 7);
    assert_eq!(admission_with_shared(199 << 20, mono, 10, Some(4 * gib)), 10);
}

/// The preload by fan-out equals the serial decode bit for bit, and the
/// hot-pixel pre-build measures each distinct dark once, pushing its
/// warnings in job order.
#[test]
fn parallel_master_preload_matches_the_serial_decode() {
    use crate::fits_writer::write_fits_f32;
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (16usize, 12usize);
    let write = |name: &str, base: f32| -> PathBuf {
        let data: Vec<f32> = (0..w * h).map(|i| base + (i % 7) as f32 * 0.25).collect();
        let p = dir.path().join(name);
        write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
        p
    };
    let dark_a = write("dark_a.fits", 10.0);
    let dark_b = write("dark_b.fits", 20.0);
    let flat = write("flat.fits", 1.0);
    let opts: CalibratedLightOptions = serde_json::from_str("{}").unwrap();
    assert!(opts.hot_pixel_correction);
    let job = |id: i64, dark: &Path| CalibrateJob {
        frame_id: id,
        old_calibrated: None,
        old_mosaic: None,
        hash: String::new(),
        spec: GenerationSpec {
            inputs: crate::calibration_library::light_cal::LightCalInputs {
                light_path: dir.path().join(format!("light{id}.fits")),
                dark_path: Some(dark.to_path_buf()),
                bias_path: None,
                flat_path: Some(flat.clone()),
                flat_norm: true,
                flat_norm_mode: Default::default(),
                cfa_geometry: None,
                params: Default::default(),
                scale_divisor: 1.0,
                output_path: dir.path().join(format!("out{id}.fits")),
                cards: vec![],
                scratch_dir: dir.path().to_path_buf(),
            },
            cards: vec![],
            cfa_geometry: None,
            dark_path: Some(dark.to_path_buf()),
            debayer: false,
            format: Default::default(),
        },
        out: dir.path().join(format!("o{id}.fits")),
        mosaic_out: None,
        opts: opts.clone(),
        _pins: Vec::new(),
    };
    // Two lights share dark A, one uses dark B.
    let jobs = vec![job(1, &dark_a), job(2, &dark_a), job(3, &dark_b)];

    let db = tempfile::tempdir().unwrap();
    let mut ctx = ServiceContext::new_for_tests(db.path().join("t.db"));
    ctx.image_pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap());
    let working = tempfile::tempdir().unwrap();
    let mut rc = test_context(
        Arc::new(ctx),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        1,
        "L1",
        StackingConfig::default(),
        Vec::new(),
        WorkingLayout::new(working.path(), &set_slug("L1")),
        working.path().to_path_buf(),
        HashMap::new(),
    );
    let scratch = dir.path();

    let preloaded =
        build_preloaded_masters(&rc, &jobs, scratch, "g", (w as u64, h as u64), 0).unwrap();
    assert_eq!(preloaded.planes.len(), 3);
    let never = AtomicBool::new(false);
    for p in [&dark_a, &dark_b, &flat] {
        let serial = preload_master_plane(p, scratch, &never).unwrap();
        let got = &preloaded.planes[p];
        assert_eq!((got.width, got.height), (serial.width, serial.height));
        assert_eq!(
            got.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            serial.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "{}",
            p.display()
        );
    }

    prebuild_hot_maps(&mut rc, &jobs, scratch, "g", (w as u64, h as u64), 0);
    assert_eq!(rc.hot_maps.len(), 2, "one map per distinct dark");
    assert!(rc.hot_maps.contains_key(&dark_a) && rc.hot_maps.contains_key(&dark_b));
    let refused: Vec<&String> = rc.warnings.iter().filter(|m| m.contains("Hot-pixel")).collect();
    for (i, d) in [&dark_a, &dark_b].iter().enumerate() {
        if let HotPixelMapOutcome::Refused(_) = &*rc.hot_maps[*d] {
            assert!(refused.iter().any(|m| m.contains(&d.display().to_string())), "{i}");
        }
    }
    // Warnings follow job order: dark A's (if any) before dark B's.
    let pos = |d: &PathBuf| refused.iter().position(|m| m.contains(&d.display().to_string()));
    if let (Some(a), Some(b)) = (pos(&dark_a), pos(&dark_b)) {
        assert!(a < b);
    }
    // A second call is a no-op (cache hit): nothing new is pushed.
    let n = rc.warnings.len();
    prebuild_hot_maps(&mut rc, &jobs, scratch, "g", (w as u64, h as u64), 0);
    assert_eq!(rc.warnings.len(), n);
}

// ── stage_calibrate ──────────────────────────────────────────────────

/// Perf tier 1 Task 4: the fan-out must produce the same calibrated
/// PIXELS, the same header CARDS and the same warning ORDER as the
/// sequential stage did. `seed_ready`'s master dark is a constant-value
/// plane (MAD = 0, see `hot_pixel_map_from_dark`'s "zero mad" branch),
/// so `prebuild_hot_maps` refuses its hot-pixel map and pushes exactly
/// one "Hot-pixel correction skipped for …" warning — the one thing a
/// reordering bug in this task would show up as.
///
/// NOT a raw-byte comparison, despite the brief's own framing (a
/// discovery made while writing this pin, unrelated to the fan-out):
/// `light_resolve::source_cards_for_file` rebuilds a light's header
/// cards from `parse_stored_header_keys`'s `HashMap<String, String>`
/// (`fits_parser/stored_header.rs`), so the card ORDER in a calibrated
/// output already varies from one generation to the next — proven by
/// running this fixture's admission-1 path twice and diffing the raw
/// bytes, which disagree starting at the SAME byte offset (560, right
/// after `SIMPLE`/`BITPIX`/`NAXIS*`) whether or not a fan-out is
/// involved. That is a pre-existing property of the header-rebuild path
/// (harmless: nothing downstream keys freshness on byte-identical
/// headers), not a regression this task introduces, and fixing it is
/// out of this task's scope. So this pin compares what "the same
/// calibrated bytes" actually needs to mean here: the header's CARD SET
/// (sorted, so ordering cannot fail it) and the PIXEL DATA (which the
/// header-order shuffle cannot touch — same card COUNT on both sides
/// keeps the header block count, and therefore the data offset,
/// identical) read back bit-for-bit via `PlaneReader`.
///
/// Since Task 13c D1 `source_cards_for_file` returns its cards keyword-sorted,
/// so the order is now deterministic; the sorted comparison below stays.
///
/// Controller ruling (2026-09-19): no env var or static forces
/// admission — two SEPARATE `ServiceContext`s give the two runs two
/// different `image_pool` widths instead. `ServiceContext::new_for_tests`'s
/// stock 1-thread pool clamps `admission` to 1 (sequential, one job at a
/// time) whatever the working-set math says; a second context built
/// here with a 4-thread pool clamps `admission` up to 4 instead —
/// `admission`'s RAM-budget term is astronomically larger than this
/// fixture's `CALIBRATE_PLANES_RESIDENT_MONO * 64 * 48 * 4` (~37 KB) working
/// set on any machine with a few GB of RAM, so the pool WIDTH is the
/// clamp that actually binds. Two separate working folders + run ids;
/// the parallel run also sets `rerun_from = Some(Stage::Calibrate)` (the
/// SAME escape hatch `calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones`'s
/// own rc4 uses below) because both runs share ONE on-disk catalog —
/// without it, the parallel run's freshness check would find the
/// sequential run's OWN `stacking_artifacts` rows fresh (identical
/// config hash) and reuse them instead of regenerating, which would
/// make this pin compare a file against itself.
#[test]
fn calibrate_fan_out_matches_the_sequential_stage_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");

    // Fix round 1: `admission_with_shared` (like `admission`) treats an
    // unknown RAM total as an exhausted budget — admission 1 always,
    // whatever `shared_bytes`/`working_set_bytes` say. On a machine (or
    // sandbox) with no RAM probe the parallel run below could never
    // actually admit more than one worker, so this pin would compare
    // sequential against sequential for a reason that has nothing to do
    // with the fan-out under test. Skip it rather than fail it.
    if total_ram_bytes().is_none() {
        return;
    }

    // Sequential reference: the stock 1-thread `image_pool` clamps
    // `admission` to 1.
    let ctx_seq = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let working_seq = tempfile::tempdir().unwrap();
    let output_seq = tempfile::tempdir().unwrap();
    let layout_seq = WorkingLayout::new(working_seq.path(), &set_slug(SET_NAME));
    let mut rc_seq = test_context(
        ctx_seq,
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout_seq.clone(),
        output_seq.path().to_path_buf(),
        HashMap::new(),
    );
    stage_calibrate(&mut rc_seq).unwrap();

    let seq_dir = layout_seq.calibrated_dir();
    let mut seq_files: Vec<PathBuf> = std::fs::read_dir(&seq_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    seq_files.sort();
    assert_eq!(seq_files.len(), 3, "{seq_files:?}");
    let seq_warnings = rc_seq.warnings.clone();

    // Parallel: a second `ServiceContext` whose `image_pool` is 4 wide —
    // `admission` clamps to the pool width, not to 1. A fresh working
    // folder and `rerun_from` so this run always regenerates through
    // the fan-out rather than reusing the sequential run's own rows.
    let mut ctx2 = ServiceContext::new_for_tests(db_path.clone());
    ctx2.image_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap(),
    );
    let ctx_par = Arc::new(ctx2);
    // Task 4 fix round: assert this fixture's working set still admits
    // more than one worker on the 4-thread pool before running, so a
    // future admission-math change cannot silently degrade this pin to
    // comparing sequential against sequential. Fix round 1 (Important
    // finding 2): `admission_with_shared` with `shared_bytes` for the
    // fixture's own two preloaded planes (its dark and flat — see the
    // doc comment above), `calibrate_working_set_bytes` for the
    // per-worker term.
    assert!(
        admission_with_shared(
            2 * 64 * 48 * 4,
            calibrate_working_set_bytes(64, 48, ColorMode::Mono, false),
            4,
            total_ram_bytes().map(|t| t / 4),
        ) > 1,
        "fixture working set must admit more than one worker for this pin to test the fan-out"
    );
    let working_par = tempfile::tempdir().unwrap();
    let output_par = tempfile::tempdir().unwrap();
    let layout_par = WorkingLayout::new(working_par.path(), &set_slug(SET_NAME));
    let mut rc_par = test_context(
        ctx_par,
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        2,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout_par.clone(),
        output_par.path().to_path_buf(),
        HashMap::new(),
    );
    rc_par.rerun_from = Some(Stage::Calibrate);
    stage_calibrate(&mut rc_par).unwrap();

    let par_dir = layout_par.calibrated_dir();
    let mut par_files: Vec<PathBuf> = std::fs::read_dir(&par_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    par_files.sort();
    assert_eq!(par_files.len(), 3, "{par_files:?}");

    // Sorted header card lines: order-independent (see the doc comment
    // above), still catches a genuinely different card set.
    fn header_lines_sorted(path: &Path) -> Vec<String> {
        let mut lines: Vec<String> = FitsHeader::from_path(path)
            .unwrap()
            .to_header_text()
            .lines()
            .map(|l| l.to_string())
            .collect();
        lines.sort();
        lines
    }
    // Every plane's pixel data, read back through the SAME reader the
    // pipeline itself uses — the header shuffle cannot touch this: equal
    // card COUNTS (both sides carry the same cards) keep the header's
    // 2880-byte block count, and therefore the data offset, identical.
    fn planes(path: &Path) -> Vec<Vec<f32>> {
        let reader = PlaneReader::open(path).unwrap();
        (0..reader.channels())
            .map(|p| reader.read_plane(p).unwrap())
            .collect()
    }

    for (i, (sp, pp)) in seq_files.iter().zip(par_files.iter()).enumerate() {
        assert_eq!(
            header_lines_sorted(sp),
            header_lines_sorted(pp),
            "file {i}: header card SET must match the sequential stage (order is not \
                 pinned — see the doc comment above)"
        );
        assert_eq!(
            planes(sp),
            planes(pp),
            "file {i}: calibrated pixel data must match the sequential stage bit for bit"
        );
    }
    assert_eq!(
        seq_warnings, rc_par.warnings,
        "warning order (the one hot-pixel refusal) must match the sequential stage"
    );
    assert_eq!(
        seq_warnings.len(),
        1,
        "the fixture's dark has MAD = 0: exactly one refusal warning is expected: {seq_warnings:?}"
    );
}

/// Zip plan Task 9: the non-attested Calibrate stage reads lights AND masters out of zips. The same
/// frame set calibrated once from disk and once with every light and both masters inside a zip gives
/// byte-identical `calibrated` artifacts, no frame is excluded, and the stage-1 hash (which only names
/// the masters) resolved for every frame.
#[test]
fn calibrate_stage_reads_zip_resident_lights_and_masters_like_disk() {
    use crate::zip_store::test_support::{zip_files, Shape};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    let cfg = StackingConfig::default();
    let calibrate = |working_dir: &Path| {
        let layout = WorkingLayout::new(working_dir, &set_slug(SET_NAME));
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            1,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups,
            layout.clone(),
            output.path().to_path_buf(),
            HashMap::new(),
        );
        stage_calibrate(&mut rc).unwrap();
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        let dir = layout.calibrated_dir();
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
            .collect();
        files.sort();
        files
    };
    let from_disk = calibrate(working.path());
    assert_eq!(from_disk.len(), 3);

    // Fresh stage-1 state, then every light and both masters go into one zip.
    fixture.conn.execute("DELETE FROM stacking_artifacts", []).unwrap();
    let mut file_ids: Vec<i64> = light_ids
        .iter()
        .map(|id| fixture.conn.query_row("SELECT file_id FROM frames WHERE id = ?1", [id], |r| r.get(0)).unwrap())
        .collect();
    let mut stmt = fixture
        .conn
        .prepare(
            "SELECT DISTINCT fr.file_id FROM calibration_set cs
                 JOIN calibration_set_frames csf ON csf.set_id = cs.id
                 JOIN frames fr ON fr.id = csf.frame_id WHERE cs.is_master_library = 1",
        )
        .unwrap();
    let masters: Vec<i64> = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    drop(stmt);
    assert_eq!(masters.len(), 2, "a master dark and a master flat");
    file_ids.extend(masters);
    let zipped = zip_files(&fixture.conn, &file_ids, &tmp.path().join("night.zip"), Shape::Foreign);
    assert!(zipped.iter().all(|p| !Path::new(p).exists()));

    let before = ctx.zip_cache.extraction_count();
    let working2 = tempfile::tempdir().unwrap();
    let from_zip = calibrate(working2.path());
    // The pixels are the contract. The headers differ in exactly one place by design: the provenance
    // cards name the master by its catalog path, which for a zip master is `<zip>/<entry>`.
    let data_of = |bytes: &[u8]| -> Vec<u8> {
        let end = bytes
            .chunks(80)
            .position(|card| card.starts_with(b"END     "))
            .expect("an END card");
        let header_len = ((end + 1) * 80).div_ceil(2880) * 2880;
        bytes[header_len..].to_vec()
    };
    assert_eq!(from_zip.len(), from_disk.len());
    for ((name_z, bytes_z), (name_d, bytes_d)) in from_zip.iter().zip(&from_disk) {
        assert_eq!(name_z, name_d);
        assert!(
            data_of(bytes_z) == data_of(bytes_d),
            "{name_z}: a zip-resident night calibrates to the same pixels"
        );
    }
    assert!(ctx.zip_cache.extraction_count() - before >= 5, "three lights and two masters were read out of the zip");
}


#[test]
fn calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);

    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();

    let build = |run_id: i64, groups: Vec<IntegrationGroup>| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            groups,
            layout.clone(),
            output_dir.clone(),
            HashMap::new(),
        )
    };

    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");

    // First pass: 3 calibrated files, 3 artifact rows.
    let mut rc1 = build(1, plan_groups.clone());
    stage_calibrate(&mut rc1).unwrap();

    let calibrated_dir = layout.calibrated_dir();
    let files_on_disk = std::fs::read_dir(&calibrated_dir).unwrap().count();
    assert_eq!(files_on_disk, 3);

    let artifacts_after_1 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    assert_eq!(artifacts_after_1.len(), 3);

    let snapshot = |artifacts: &[crate::db::stacking::StackingArtifactRow]| -> HashMap<i64, (std::time::SystemTime, String)> {
        artifacts
            .iter()
            .map(|a| {
                let path = a.path.clone().unwrap();
                let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
                (a.frame_id.unwrap(), (mtime, a.config_hash.clone()))
            })
            .collect()
    };
    let before = snapshot(&artifacts_after_1);

    // Second pass: 0 files rewritten (mtimes unchanged), rows unchanged.
    let mut rc2 = build(2, plan_groups.clone());
    stage_calibrate(&mut rc2).unwrap();
    let artifacts_after_2 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    assert_eq!(artifacts_after_2.len(), 3);
    let after_reuse = snapshot(&artifacts_after_2);
    assert_eq!(
        before, after_reuse,
        "a fresh artifact must not be rewritten"
    );

    // Touch one source file's size in `files` — only that frame regenerates.
    fixture
        .conn
        .execute(
            "UPDATE files SET size = size + 1000 WHERE id = \
                 (SELECT file_id FROM frames WHERE id = ?1)",
            rusqlite::params![light_ids[0]],
        )
        .unwrap();
    let plan_groups_touched =
        group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let mut rc3 = build(3, plan_groups_touched);
    stage_calibrate(&mut rc3).unwrap();
    let artifacts_after_3 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    let after_touch = snapshot(&artifacts_after_3);
    assert_ne!(
        after_touch[&light_ids[0]], before[&light_ids[0]],
        "the touched frame must regenerate"
    );
    assert_eq!(
        after_touch[&light_ids[1]], before[&light_ids[1]],
        "an untouched frame must not regenerate"
    );
    assert_eq!(
        after_touch[&light_ids[2]], before[&light_ids[2]],
        "an untouched frame must not regenerate"
    );

    // rerun_from Calibrate: every artifact is stale, all three regenerate.
    let plan_groups_rerun = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let mut rc4 = build(4, plan_groups_rerun);
    rc4.rerun_from = Some(Stage::Calibrate);
    stage_calibrate(&mut rc4).unwrap();
    let artifacts_after_4 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    let after_rerun = snapshot(&artifacts_after_4);
    for &id in &light_ids {
        assert_ne!(
            after_rerun[&id], after_touch[&id],
            "frame {id} must regenerate under rerun_from Calibrate"
        );
    }
}

/// The sum of `items` over the `fan-out finished` lines of `stage` and `pass` in a
/// `crate::stacking::telemetry::capture_logs` text; 0 when there is none (a stage whose work was all cached
/// opens no fan-out).
pub(super) fn fan_out_items(logs: &str, stage: &str, pass: &str) -> usize {
    logs.lines()
        .filter(|l| {
            l.contains("fan-out finished")
                && l.contains(&format!("stage=\"{stage}\""))
                && l.contains(&format!("pass=\"{pass}\""))
        })
        .map(|l| {
            let rest = l.split("items=").nth(1).expect("items field");
            rest.split_whitespace().next().unwrap().parse::<usize>().unwrap()
        })
        .sum()
}

/// Frame-level cache spec §5.1: a regroup (here "one group per exposure" -> "no split") finds the calibrated
/// rows again; nothing is recalibrated and no file is rewritten.
#[test]
fn calibrated_artifact_is_reused_across_a_regroup() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    let mut cfg_seconds = StackingConfig::default();
    cfg_seconds.grouping.exposure_mode = crate::stacking::config::ExposureMode::Seconds;
    let mut cfg_all = StackingConfig::default();
    cfg_all.grouping.exposure_mode = crate::stacking::config::ExposureMode::NoSplit;
    let groups_of = |cfg: &StackingConfig| {
        group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap()
    };
    let (groups_seconds, groups_all) = (groups_of(&cfg_seconds), groups_of(&cfg_all));
    assert_ne!(
        groups_seconds[0].key, groups_all[0].key,
        "the two groupings must name the group differently"
    );
    let build = |run_id: i64, cfg: &StackingConfig, groups: Vec<IntegrationGroup>| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            groups,
            layout.clone(),
            output.path().to_path_buf(),
            HashMap::new(),
        )
    };
    let rows = || {
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated"),
        )
        .unwrap()
    };

    let mut rc1 = build(1, &cfg_seconds, groups_seconds);
    let (res, text1) = crate::stacking::telemetry::capture_logs(|| stage_calibrate(&mut rc1));
    res.unwrap();
    assert_eq!(fan_out_items(&text1, "calibrate", "calibrate"), 3, "{text1}");
    let first = rows();
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|r| r.group_key.is_empty()), "{first:?}");
    let mtimes = |rows: &[crate::db::stacking::StackingArtifactRow]| -> Vec<std::time::SystemTime> {
        rows.iter()
            .map(|r| std::fs::metadata(r.path.as_ref().unwrap()).unwrap().modified().unwrap())
            .collect()
    };
    let first_mtimes = mtimes(&first);

    let mut rc2 = build(2, &cfg_all, groups_all);
    let (res, text2) = crate::stacking::telemetry::capture_logs(|| stage_calibrate(&mut rc2));
    res.unwrap();
    assert_eq!(
        fan_out_items(&text2, "calibrate", "calibrate"),
        0,
        "a regroup recalibrated: {text2}"
    );
    let second = rows();
    assert_eq!(second.len(), 3, "a regroup added rows");
    for (a, b) in first.iter().zip(&second) {
        assert_eq!((a.id, &a.created_at, &a.path), (b.id, &b.created_at, &b.path));
        assert_eq!(b.group_key, "");
    }
    assert_eq!(mtimes(&second), first_mtimes, "a regroup rewrote a calibrated file");
}

/// Frame-level cache spec §5.1/§5.7 (review focus 3): a frame re-written under a new stem or folder takes its
/// old file with it, unless another row still points to it.
#[test]
fn rewriting_a_frame_removes_its_old_file_once_unreferenced() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let cfg = StackingConfig::default();
    let subject = StackSubject::FrameSet(fixture.set_id);
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let build = |run_id: i64| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output.path().to_path_buf(),
            HashMap::new(),
        )
    };
    let mut rc1 = build(1);
    stage_calibrate(&mut rc1).unwrap();

    // Rows as an older version left them: the file in `calibrated/<group_key>/` under the old stem.
    let old_dir = layout.calibrated_dir().join(&plan_groups[0].key);
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut old_paths = Vec::new();
    for &id in &light_ids[..2] {
        let row = crate::db::stacking::find_artifact(&fixture.conn, &subject, "", "calibrated", Some(id))
            .unwrap()
            .unwrap();
        let old = old_dir.join(format!("c_old_{id}.fits"));
        std::fs::rename(row.path.as_ref().unwrap(), &old).unwrap();
        fixture
            .conn
            .execute(
                "UPDATE stacking_artifacts SET path = ?1 WHERE id = ?2",
                rusqlite::params![old.to_str().unwrap(), row.id],
            )
            .unwrap();
        old_paths.push(old);
    }
    // Another row (of another kind) still points to the second frame's old file.
    upsert_artifact(
        &fixture.conn,
        &NewArtifact {
            subject: &subject,
            frame_id: Some(light_ids[1]),
            group_key: "",
            kind: "solve",
            path: old_paths[1].to_str(),
            config_hash: "h",
            size: None,
            modified_at: None,
            payload_json: None,
        },
    )
    .unwrap();

    let mut rc2 = build(2);
    rc2.rerun_from = Some(Stage::Calibrate);
    stage_calibrate(&mut rc2).unwrap();

    for (n, &id) in light_ids[..2].iter().enumerate() {
        let row = crate::db::stacking::find_artifact(&fixture.conn, &subject, "", "calibrated", Some(id))
            .unwrap()
            .unwrap();
        let new = PathBuf::from(row.path.unwrap());
        assert_eq!(new.parent().unwrap(), layout.calibrated_dir(), "{new:?}");
        let name = new.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.ends_with(&format!("_f{id}.fits")), "{name}");
        assert!(new.exists());
        assert_eq!(
            old_paths[n].exists(),
            n == 1,
            "frame {id}: the old file {} be gone",
            if n == 0 { "must" } else { "must not" }
        );
    }
}

/// Frame-level cache spec §5.1 (review focus 4): a collab project frame (`StackSubject::Project`) takes the
/// same frame-level key and the frame-only stem.
#[test]
fn a_project_frame_uses_the_frame_level_key() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for (u, file) in [
        ("m1", test_fixtures::ProjectFile::Mono32),
        ("x1", test_fixtures::ProjectFile::Xisf32),
    ] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = file;
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
    stage_calibrate(&mut rc).unwrap();
    assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
    let rows = crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("calibrated")).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(row.group_key, "", "{row:?}");
        assert!(matches!(row.subject, StackSubject::Project(_)), "{row:?}");
        let handle = row.frame_id.expect("a project frame handle");
        let found = crate::db::stacking::find_artifact(&conn, &rc.subject, "", "calibrated", Some(handle))
            .unwrap();
        assert_eq!(found.map(|r| r.id), Some(row.id));
    }
    // The converted file is named by the frame alone: `p_<stem>_f<handle>.fits`, in the flat folder.
    let converted = rows
        .iter()
        .map(|r| PathBuf::from(r.path.as_ref().unwrap()))
        .find(|p| p.starts_with(rc.layout.prepared_dir()))
        .expect("the XISF frame is converted under prepared/");
    let handle = rows
        .iter()
        .find(|r| PathBuf::from(r.path.as_ref().unwrap()) == converted)
        .unwrap()
        .frame_id
        .unwrap();
    assert_eq!(converted.parent().unwrap(), rc.layout.prepared_dir());
    assert!(
        converted
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(&format!("_f{handle}.fits")),
        "{converted:?}"
    );
}

/// Fix round 1, Critical: two frames of one group sharing a source
/// basename (a capture counter restarted on another night) must not
/// collide on one calibrated output. Since frame-level cache spec §5.1
/// EVERY frame's stem carries `_f<frame_id>`, the unique-named frame
/// included, so a merge never renames a file.
#[test]
fn calibrate_outputs_disambiguate_same_basename_within_a_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    let fixture_conn = rusqlite::Connection::open(&db_path).unwrap();
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    // Two lights sharing the SAME source basename ("dup.fits") — a
    // capture counter that restarted on a different night, so the two
    // physical files live in DIFFERENT directories (`files.path` is
    // UNIQUE, so they can't share a path even though they share a
    // name). `add_light` gives the first one a real file; the second
    // is a manual copy into a sibling directory with the same
    // filename, inserted the same way `add_light`'s own `insert_light_row`
    // would (this test's file scope is `run.rs` only — the fixture
    // builder itself is untouched).
    let (dup_a, path_a) =
        test_fixtures::add_light(&fixture, &light_spec("dup", THREE_TIMES[0]));

    let night2_dir = fixture.dir.path().join("night2");
    std::fs::create_dir_all(&night2_dir).unwrap();
    let path_b = night2_dir.join("dup.fits");
    std::fs::copy(&path_a, &path_b).unwrap();
    let meta_b = std::fs::metadata(&path_b).unwrap();
    let modified_b =
        chrono::DateTime::<chrono::Utc>::from(meta_b.modified().unwrap()).to_rfc3339();
    fixture
        .conn
        .execute(
            "INSERT INTO files (path, filename, size, modified_at, format) \
                 VALUES (?1, 'dup.fits', ?2, ?3, 'FITS')",
            rusqlite::params![path_b.to_string_lossy(), meta_b.len() as i64, modified_b],
        )
        .unwrap();
    let file_id_b = fixture.conn.last_insert_rowid();
    fixture
        .conn
        .execute(
            "INSERT INTO frames (file_id, instrume, filter, xbinning, naxis1, naxis2, \
                 exptime, date_obs, imagetyp) VALUES (?1, 'cam', NULL, 1, 64, 48, 60.0, ?2, 'Light')",
            rusqlite::params![file_id_b, THREE_TIMES[1]],
        )
        .unwrap();
    let dup_b = fixture.conn.last_insert_rowid();
    fixture
        .conn
        .execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![fixture.session_id, dup_b],
        )
        .unwrap();

    let (uniq, _) = test_fixtures::add_light(&fixture, &light_spec("uniq", THREE_TIMES[2]));
    let light_ids = vec![dup_a, dup_b, uniq];
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();

    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();

    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");

    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout.clone(),
        output_dir,
        HashMap::new(),
    );
    stage_calibrate(&mut rc).unwrap();

    let artifacts = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    assert_eq!(artifacts.len(), 3, "{artifacts:?}");

    let path_of = |frame_id: i64| -> String {
        artifacts
            .iter()
            .find(|a| a.frame_id == Some(frame_id))
            .unwrap_or_else(|| panic!("no artifact row for frame {frame_id}"))
            .path
            .clone()
            .unwrap()
    };
    let path_a = path_of(dup_a);
    let path_b = path_of(dup_b);
    let path_uniq = path_of(uniq);

    assert_ne!(
        path_a, path_b,
        "colliding frames must get two DISTINCT calibrated files"
    );
    assert!(
        path_a.contains(&format!("_f{dup_a}")),
        "colliding frame {dup_a} must carry the disambiguating suffix: {path_a}"
    );
    assert!(
        path_b.contains(&format!("_f{dup_b}")),
        "colliding frame {dup_b} must carry the disambiguating suffix: {path_b}"
    );
    // Spec §5.1: the stem is a function of the frame alone, so a frame with a unique name carries its id too
    // (a merge that brings a namesake in must not rename it).
    assert!(
        path_uniq.contains(&format!("_f{uniq}")),
        "a frame with a unique source name carries its own id as well: {path_uniq}"
    );

    let calibrated_dir = layout.calibrated_dir();
    let files_on_disk = std::fs::read_dir(&calibrated_dir).unwrap().count();
    assert_eq!(
        files_on_disk, 3,
        "three distinct files on disk — neither collision overwrote the other"
    );
}

// ── stages 3-5 (Task 7): measure & select, reference, register ─────────

/// A real `stacking_runs` row plus one `stacking_run_groups` row per
/// `plan_groups` entry, all FK-linked to that same run — everything
/// [`test_context`]'s `group_ids` and stages 3/5's own DB writes need
/// that [`seed_ready`]'s calibrate-only fixture never had to provide.
pub(super) fn seed_run_and_groups(
    conn: &rusqlite::Connection,
    frames_set_id: i64,
    plan_groups: &[IntegrationGroup],
    working: &Path,
    output: &Path,
) -> (i64, HashMap<String, i64>) {
    let run_id = insert_run(
        conn,
        &NewRun {
            subject: &StackSubject::FrameSet(frames_set_id),
            config_json: "{}",
            config_hash: "test-hash",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.to_str().unwrap(),
            output_dir: output.to_str().unwrap(),
        },
    )
    .unwrap();

    let mut group_ids = HashMap::new();
    for g in plan_groups {
        let (anchor_width, anchor_height) = group_anchor_geometry(g);
        let group_id = insert_group(
            conn,
            &NewGroup {
                run_id,
                group_key: &g.key,
                instrume: g.instrume.as_deref(),
                color_mode: color_mode_wire(g.color_mode),
                filter: g.filter.as_deref(),
                binning: Some(g.binning),
                width: anchor_width,
                height: anchor_height,
                exposure: g.exposure_s,
                exposure_class: None,
                frame_count: g.frames.len() as i64,
                included_count: g.frames.len() as i64,
            },
        )
        .unwrap();
        group_ids.insert(g.key.clone(), group_id);
    }
    (run_id, group_ids)
}

/// Run stages 1 through `through` (inclusive) on `rc` — the harness the
/// tests below drive instead of calling each stage function by hand.
pub(super) fn run_stages_for_test(rc: &mut RunContext, through: Stage) -> Result<(), RunError> {
    stage_calibrate(rc)?;
    if through == Stage::Calibrate {
        return Ok(());
    }
    stage_measure(rc)?;
    if through == Stage::Measure {
        return Ok(());
    }
    stage_reference(rc)?;
    if through == Stage::Reference {
        return Ok(());
    }
    stage_register(rc)?;
    Ok(())
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
        rows_of_kind(&self.fixture.conn, self.fixture.set_id, "registered")
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
    // Another row, of another group key, still points at one of the old files.
    let kept_frame = rig.light_ids[3];
    upsert_artifact(
        &rig.fixture.conn,
        &NewArtifact {
            subject: &StackSubject::FrameSet(rig.fixture.set_id),
            frame_id: Some(kept_frame),
            group_key: "stray",
            kind: "registered",
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
            "registered",
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

    // Run B: another reference, cancelled right after its Register fan-out.
    crate::registration::db::set_frame_set_reference(&rig.fixture.conn, rig.fixture.set_id, rig.light_ids[1])
        .unwrap();
    let (_rc_b, res) = rig.run_with(&cfg, true);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    let rows_b = rig.registered_snapshot();
    assert_eq!(rows_b.len(), rows_a.len());
    for (a, b) in rows_a.iter().zip(&rows_b) {
        assert_eq!((&a.1, &a.4, &a.5), (&b.1, &b.4, &b.5), "the cancelled run recorded a row");
        assert_ne!(a.6, b.6, "premise: run B overwrote frame {}'s file", a.0);
        assert_eq!(
            std::fs::metadata(a.5.as_ref().unwrap()).unwrap().len() as i64,
            rig.registered_rows()[&a.0].size.unwrap(),
            "premise: the sizes still agree, so only the modification time tells the files apart"
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

/// Ruling R4 (c) (Task 5): the `.rej` stem is the frame's own stem. Two frames of one merged group whose
/// registered rows point at `registered/<keyA>/r_Light_0001.fits` and `registered/<keyB>/r_Light_0001.fits` (one
/// file name from two old group folders) get two distinct `.rej` files: the stems of their stored paths are equal
/// (the premise, asserted), the frames' own stems carry their ids, and a set created with those stems writes one
/// `.rej` per frame.
#[test]
fn rejection_map_stems_do_not_collide_across_old_group_folders() {
    let frame = |id: i64| GroupFrame {
        frame_id: id,
        file_id: id,
        filename: "Light_0001.fits".to_string(),
        path: format!("/data/n{id}/Light_0001.fits"),
        size: 1,
        modified_at: String::new(),
        exposure_s: Some(60.0),
        date_obs: None,
        width: 8,
        height: 8,
        pixel_scale_arcsec: None,
        scale_source: None,
    };
    let group = IntegrationGroup {
        key: "merged".into(),
        instrume: None,
        color_mode: ColorMode::Mono,
        filter: None,
        binning: 1,
        cameras: Vec::new(),
        exposure_s: None,
        exposure_class: "all".into(),
        frames: vec![frame(7), frame(9)],
        total_exposure_s: 0.0,
        pixel_scale_arcsec: None,
    };
    let stored = [
        Path::new("/work/registered/keyA/r_Light_0001.fits"),
        Path::new("/work/registered/keyB/r_Light_0001.fits"),
    ];
    assert_eq!(stored[0].file_stem(), stored[1].file_stem(), "premise: the stored paths share a stem");

    let stems = rejection_map_stems(&group, &[7, 9]);
    assert_eq!(stems, vec!["Light_0001_f7".to_string(), "Light_0001_f9".to_string()]);
    let dir = tempfile::tempdir().unwrap();
    let _set = crate::stacking::rej::RejBitmapSet::create(dir.path(), &stems, 8, 8, 1).unwrap();
    let mut files: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rej"))
        .collect();
    files.sort();
    assert_eq!(files, vec!["Light_0001_f7.rej".to_string(), "Light_0001_f9.rej".to_string()]);
    // A member the group does not hold keeps a distinct stem from its position and id.
    assert_eq!(rejection_map_stems(&group, &[7, 11]), vec!["Light_0001_f7".to_string(), "frame_1_f11".to_string()]);
}

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
        Some("metrics"),
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
        Some("metrics"),
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

/// Plan A Task 11b: the frame set's stored `metrics` rows, by frame id: (key, measurement, created at).
fn metrics_by_frame(
    conn: &rusqlite::Connection,
    set_id: i64,
) -> HashMap<i64, (String, FrameMeasurement, String)> {
    crate::db::stacking::list_artifacts(conn, &StackSubject::FrameSet(set_id), Some("metrics"))
        .unwrap()
        .into_iter()
        .map(|a| {
            let m: FrameMeasurement =
                serde_json::from_str(a.payload_json.as_deref().unwrap()).unwrap();
            (a.frame_id.unwrap(), (a.config_hash, m, a.created_at))
        })
        .collect()
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
                kind: "metrics",
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
            "calibrated",
            Some(frame_id),
        )
        .unwrap()
        .unwrap()
        .config_hash
    };

    let beta1 = run("run 1").expect("a group beta");
    let logged = || pass1_logged_under(working.path()).len();
    let measured1 = rows_of_kind(&fixture.conn, fixture.set_id, "measured");
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
                kind: "metrics",
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
    for (frame_id, row) in rows_of_kind(&fixture.conn, fixture.set_id, "measured") {
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

/// Plan A Task 11c: the `measure_frame_pass1` calls (test log) whose path lies under `root` — every pass-1 read of
/// one test's own working tree, immune to sibling tests (`FITS_WRITE_FAILURE_LOG`'s reasoning).
fn pass1_logged_under(root: &Path) -> Vec<PathBuf> {
    crate::stacking::measure::PASS1_LOG
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(root))
        .cloned()
        .collect()
}

/// Plan A Task 11c: the `stacking_artifacts` rows of one kind, by frame id.
fn rows_of_kind(
    conn: &rusqlite::Connection,
    set_id: i64,
    kind: &str,
) -> HashMap<i64, crate::db::stacking::StackingArtifactRow> {
    crate::db::stacking::list_artifacts(conn, &StackSubject::FrameSet(set_id), Some(kind))
        .unwrap()
        .into_iter()
        .map(|a| (a.frame_id.unwrap(), a))
        .collect()
}

/// Plan A Task 11c: four mono frames of `seed_star_group` measured one Measure stage at a time on the test pool's
/// one thread (admission 1), with what the Step 1 tests count.
struct Pass1Rig {
    _tmp: tempfile::TempDir,
    ctx: Arc<ServiceContext>,
    fixture: test_fixtures::Fixture,
    working: tempfile::TempDir,
    output: tempfile::TempDir,
    cfg: StackingConfig,
    layout: WorkingLayout,
    plan_groups: Vec<IntegrationGroup>,
    group_key: String,
    subject: StackSubject,
    /// The run's frame origins when the set is attested (`test_context` assumes a plain catalog origin).
    origins: Option<FrameOrigins>,
}

/// What one Measure stage did.
struct Pass1Out {
    beta: Option<f64>,
    /// The `measure_frame_pass1` calls this run made.
    logged: Vec<PathBuf>,
    excluded: usize,
}

impl Pass1Rig {
    fn new() -> Pass1Rig {
        Pass1Rig::build(1, false)
    }

    /// The rig on an `image_pool` of `threads` workers; `osc` seeds RGGB mosaics, which stage 1 debayers into
    /// three-plane `_d` files.
    fn build(threads: usize, osc: bool) -> Pass1Rig {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let mut raw = ServiceContext::new_for_tests(db_path.clone());
        if threads > 1 {
            raw.image_pool = Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap(),
            );
        }
        let ctx = Arc::new(raw);
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let (fixture, light_ids, working, output) = if osc {
            seed_osc_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4])
        } else {
            seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4])
        };
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        let cfg = StackingConfig::default();
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let group_key = plan_groups[0].key.clone();
        let subject = StackSubject::FrameSet(fixture.set_id);
        Pass1Rig {
            _tmp: tmp,
            ctx,
            fixture,
            working,
            output,
            cfg,
            layout,
            plan_groups,
            group_key,
            subject,
            origins: None,
        }
    }

    /// An attested set (externally calibrated, ruling R12 (a)): the four lights are three-plane float FITS used IN
    /// PLACE, so each `calibrated` row points at the user's own file, outside `calibrated/` and `prepared/`.
    fn attested() -> Pass1Rig {
        let mut rig = Pass1Rig::new();
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        make_lights_three_plane(&rig.fixture, &shifts, &[0, 1, 2, 3]);
        rig.fixture
            .conn
            .execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [rig.fixture.set_id],
            )
            .unwrap();
        let (groups, origins) = frame_source(&rig.fixture.conn, &rig.subject, &rig.cfg).unwrap();
        rig.group_key = groups[0].key.clone();
        rig.plan_groups = groups;
        rig.origins = Some(origins);
        rig
    }

    fn run(&self, label: &str, rerun_from: Option<Stage>) -> Pass1Out {
        self.try_run(rerun_from, None)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"))
    }

    /// One Measure stage; `cancel_after = Some(n)` sets the run's cancel flag once the n-th first pass under this
    /// rig's working tree has finished (a test-only hook in `measure_frame_pass1`, no timing).
    fn try_run(
        &self,
        rerun_from: Option<Stage>,
        cancel_after: Option<usize>,
    ) -> Result<Pass1Out, RunError> {
        let log_root = self.log_root();
        let before = pass1_logged_under(&log_root).len();
        let (run_id, group_ids) = seed_run_and_groups(
            &self.fixture.conn,
            self.fixture.set_id,
            &self.plan_groups,
            self.working.path(),
            self.output.path(),
        );
        let mut rc = test_context(
            self.ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            self.fixture.set_id,
            SET_NAME,
            self.cfg.clone(),
            self.plan_groups.clone(),
            self.layout.clone(),
            self.output.path().to_path_buf(),
            group_ids,
        );
        rc.rerun_from = rerun_from;
        if let Some(origins) = &self.origins {
            rc.origins = origins.clone();
        }
        if let Some(n) = cancel_after {
            crate::stacking::measure::PASS1_CANCEL_AFTER
                .lock()
                .unwrap()
                .push((self.working.path().to_path_buf(), n));
        }
        let result = run_stages_for_test(&mut rc, Stage::Measure);
        crate::stacking::measure::PASS1_CANCEL_AFTER
            .lock()
            .unwrap()
            .retain(|(dir, _)| dir != self.working.path());
        result?;
        Ok(Pass1Out {
            beta: crate::db::stacking::list_groups(&self.fixture.conn, run_id).unwrap()[0].beta,
            logged: pass1_logged_under(&log_root)[before..].to_vec(),
            excluded: rc.runtime_exclusions.len(),
        })
    }

    /// The folder whose first passes this rig counts: the working folder, or (an attested set, whose lights are
    /// measured in place) the folder of the fixture's own lights.
    fn log_root(&self) -> PathBuf {
        if self.origins.is_some() {
            self.fixture.dir.path().to_path_buf()
        } else {
            self.working.path().to_path_buf()
        }
    }

    fn rows(&self, kind: &str) -> HashMap<i64, crate::db::stacking::StackingArtifactRow> {
        rows_of_kind(&self.fixture.conn, self.fixture.set_id, kind)
    }

    fn calibrated_path(&self, frame_id: i64) -> PathBuf {
        let row = crate::db::stacking::find_artifact(
            &self.fixture.conn,
            &self.subject,
            "",
            "calibrated",
            Some(frame_id),
        )
        .unwrap()
        .unwrap();
        PathBuf::from(row.path.unwrap())
    }

    /// The root of the (not yet existing) pass-1 spill directory, named without the code under test.
    fn measured_root(&self) -> PathBuf {
        self.layout.calibrated_root().parent().unwrap().join("measured")
    }

    fn delete_rows(&self, kinds: &[&str]) {
        crate::db::stacking::delete_artifacts(&self.fixture.conn, &self.subject, kinds)
            .unwrap();
    }

    /// The group β moves: the stored own β of the given members goes below every other's (the
    /// `measure_reruns_pass_two_…` edit).
    fn lower_own_betas(&self, lowered: &[(i64, f64)]) {
        let rows = metrics_by_frame(&self.fixture.conn, self.fixture.set_id);
        for &(frame_id, own) in lowered {
            let (hash, mut m, _) = rows[&frame_id].clone();
            m.channels[0].beta_own = Some(own);
            let payload = serde_json::to_string(&m).unwrap();
            upsert_artifact(
                &self.fixture.conn,
                &NewArtifact {
                    subject: &self.subject,
                    frame_id: Some(frame_id),
                    group_key: "",
                    kind: "metrics",
                    path: None,
                    config_hash: &hash,
                    size: None,
                    modified_at: None,
                    payload_json: Some(&payload),
                },
            )
            .unwrap();
        }
    }

    fn sorted_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.rows("metrics").keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}

/// The `(group key, created_at, config hash, path)` of every `metrics`, `measured` and `fits.0` row, by (kind,
/// frame): what a regroup must leave untouched.
fn measure_rows_snapshot(
    rig: &Pass1Rig,
) -> Vec<(String, i64, String, String, String, Option<String>)> {
    let mut out = Vec::new();
    for kind in ["metrics", "measured", "fits.0"] {
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
    for (frame_id, row) in rig.rows("measured") {
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
    rig.delete_rows(&["metrics", "fits.0"]);

    let (second, logs) = crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    assert_eq!(fan_out_items(&logs, "measure", "pass1"), 0, "pass 1 re-ran: {logs}");
    assert_eq!(fan_out_items(&logs, "measure", "pass2"), 4, "{logs}");
    assert!(second.logged.is_empty(), "pass 1 re-ran for {:?}", second.logged);
    assert!(
        !logs.contains("first-pass artifact"),
        "no refusal or stale line for a spill read from its row: {logs}"
    );
    let rows = rig.rows("measured");
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
    kind: &str,
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
    let moved = move_rows_to_legacy_folder(&rig, "measured", &rig.measured_root(), ".athm");
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
    rig.delete_rows(&["fits.0"]);

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

    let rows = rig.rows("measured");
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

/// Ruling R6 M2 (Task 7): stale `fits.0` and `measured` rows pointing into `fits/<old_key>/` and
/// `measured/<old_key>/`, `rerun_from = Measure`: the old files are gone and the new ones sit at
/// `<stem>_f<id>` in the flat folders.
#[test]
fn a_forced_measure_replaces_files_in_old_group_folders() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    let old_measured = move_rows_to_legacy_folder(&rig, "measured", &rig.measured_root(), ".athm");
    let old_fits = move_rows_to_legacy_folder(&rig, "fits.0", &rig.layout.fits_root(), ".p0.athf");
    assert_eq!((old_measured.len(), old_fits.len()), (4, 4));

    rig.run("run 2", Some(Stage::Measure));
    for group_frame in &rig.plan_groups[0].frames {
        let id = group_frame.frame_id;
        let stem = frame_file_stem(group_frame);
        assert!(!old_measured[&id].exists(), "frame {id}: the old pass-1 file is gone");
        assert!(!old_fits[&id].exists(), "frame {id}: the old fits file is gone");
        let measured = rig.rows("measured")[&id].path.clone().unwrap();
        assert_eq!(PathBuf::from(&measured), rig.layout.measured_path(&stem));
        assert!(Path::new(&measured).is_file());
        let fits = rig.rows("fits.0")[&id].path.clone().unwrap();
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
    rig.delete_rows(&["metrics", "fits.0"]);
    assert!(rig.rows("metrics").is_empty());
    let second = rig.run("run 2", None);
    assert!(
        second.logged.is_empty(),
        "pass 1 re-ran for {:?}",
        second.logged
    );
    assert_eq!(rig.rows("metrics").len(), 4);
    assert_eq!(
        second.beta.map(f64::to_bits),
        first.beta.map(f64::to_bits),
        "the group beta from the artifacts' own betas is the one the pixels gave"
    );
}

/// Plan A Task 11c (R11c-4), amended by the frame-level cache (spec §5.3): a calibrated file whose mtime moved
/// since the artifact was written is recalibrated by the calibrate stage (its row records the old mtime), and the
/// first pass survives: validity follows the calibration hash, not the mtime. Nothing is excluded.
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
    rig.delete_rows(&["fits.0"]);
    let second = rig.run("run 2", None);
    assert!(second.logged.is_empty(), "pass 1 re-ran for {:?}", second.logged);
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows("metrics").len(), 4);
}

/// Ruling R12 (a): a file Athenaeum never rewrites (an attested light used in place) keeps its modification time in
/// the first pass's stamp. A same-size pixel edit by someone else (a new mtime, the catalog row refreshed as a
/// rescan does) makes THAT frame's first pass stale; it re-runs, the others do not, nothing is excluded. Without
/// the mtime the edit would be silently reused (the length is the same).
#[test]
fn measure_pass1_artifact_of_an_edited_in_place_source_reruns_pass_one() {
    let rig = Pass1Rig::attested();
    let first = rig.run("run 1", None);
    assert_eq!(first.logged.len(), 4);
    let ids = rig.sorted_ids();
    assert_eq!(ids.len(), 4);
    for id in &ids {
        let p = rig.calibrated_path(*id);
        assert!(
            !p.starts_with(rig.layout.calibrated_root()) && !p.starts_with(rig.layout.prepared_root()),
            "{p:?} is used in place"
        );
    }
    let edited = rig.calibrated_path(ids[2]);
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
    let (size, modified_at) = file_identity(&edited).unwrap();
    rig.fixture
        .conn
        .execute(
            "UPDATE files SET size = ?1, modified_at = ?2 WHERE path = ?3",
            rusqlite::params![size, modified_at, edited.to_str().unwrap()],
        )
        .unwrap();
    rig.delete_rows(&["metrics", "fits.0"]);
    let second = rig.run("run 2", None);
    assert_eq!(second.logged, vec![edited], "only the edited file re-ran pass 1");
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows("metrics").len(), 4);
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
        "calibrated",
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
            kind: "calibrated",
            path: row.path.as_deref(),
            config_hash: &row.config_hash,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: row.payload_json.as_deref(),
        },
    )
    .unwrap();
    rig.delete_rows(&["fits.0"]);
    let second = rig.run("run 2", None);
    assert_eq!(second.logged, vec![resized], "only the resized file re-ran pass 1");
    assert_eq!(second.excluded, 0);
    assert_eq!(rig.rows("metrics").len(), 4);
}

/// The widest `admission` any `fan-out admitted` line of `logs` reports: more than 1 means the stage really ran
/// its frames in parallel (an unknown RAM total admits one worker whatever the pool).
fn widest_admission(logs: &str) -> usize {
    logs.lines()
        .filter(|l| l.contains("fan-out admitted"))
        .filter_map(|l| l.split(" admission=").nth(1)?.split_whitespace().next()?.parse().ok())
        .max()
        .unwrap_or(0)
}

/// Every plane of one FITS file, as bit patterns.
fn file_plane_bits(path: &Path) -> Vec<Vec<u32>> {
    let reader = PlaneReader::open(path).unwrap();
    (0..reader.channels())
        .map(|p| reader.read_plane(p).unwrap().iter().map(|v| v.to_bits()).collect())
        .collect()
}

/// Every plane of every calibrated frame of the rig, as bit patterns, by frame id.
fn calibrated_plane_bits(rig: &Pass1Rig) -> Vec<(i64, Vec<Vec<u32>>)> {
    rig.sorted_ids()
        .into_iter()
        .map(|id| (id, file_plane_bits(&rig.calibrated_path(id))))
        .collect()
}

/// The precondition of keeping a first pass across a recalibration, on a rig: a frame calibrated, its file and
/// row deleted and calibrated again in a NEW run context has the same planes to the bit. `planes` is the plane
/// count every calibrated file must have (1 mono, 3 debayered).
fn assert_recalibration_is_bit_identical(rig: &Pass1Rig, planes: usize, parallel: bool) {
    rig.run("run 1", None);
    let before = calibrated_plane_bits(rig);
    assert_eq!(before.len(), 4);
    assert!(before.iter().all(|(_, p)| p.len() == planes), "{planes} planes each");
    let calibrated_files: Vec<PathBuf> =
        before.iter().map(|(id, _)| rig.calibrated_path(*id)).collect();
    assert!(
        calibrated_files.iter().all(|f| f.starts_with(rig.layout.calibrated_root())),
        "written under calibrated/: {calibrated_files:?}"
    );
    rig.delete_rows(&["calibrated"]);
    for file in &calibrated_files {
        std::fs::remove_file(file).unwrap();
    }
    let (_, logs) = crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    if parallel && total_ram_bytes().is_some() {
        assert!(widest_admission(&logs) > 1, "run 2 recalibrated in parallel: {logs}");
    }
    for file in &calibrated_files {
        assert!(file.is_file(), "run 2 recalibrated {}", file.display());
    }
    let after = calibrated_plane_bits(rig);
    assert_eq!(before, after, "a recalibration changed a pixel");
}

/// Frame-level cache spec §5.3 / §8 Q1 (Task 8, step 1): the precondition of keeping a first pass across a
/// recalibration. A frame calibrated, its file and row deleted and calibrated again in a NEW run context has the
/// same planes to the bit. Without this pin the mtime stamp must stay. Ruling R12 (b): the mono case on the stock
/// one-thread pool and on a four-thread one, and the debayered OSC case (three-plane `_d` files) on four threads.
#[test]
fn recalibration_reproduces_the_planes_bit_for_bit() {
    assert_recalibration_is_bit_identical(&Pass1Rig::new(), 1, false);
    assert_recalibration_is_bit_identical(&Pass1Rig::build(4, false), 1, true);
    assert_recalibration_is_bit_identical(&Pass1Rig::build(4, true), 3, true);
}

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
                    "calibrated",
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
    crate::db::stacking::delete_artifacts(&conn, &rc.subject, &["calibrated"]).unwrap();

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
        .rows("measured")
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
    rig.delete_rows(&["metrics", "fits.0"]);

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
    assert_eq!(rig.rows("metrics").len(), 4);
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
    for row in rig.rows("measured").into_values() {
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
    rig.delete_rows(&["metrics", "fits.0"]);

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
    let rows = rig.rows("measured");
    assert_eq!(rows.len(), 4);
    for (id, row) in &rows {
        assert!(
            crate::stacking::plan::file_matches_identity(
                Path::new(row.path.as_deref().unwrap()),
                row.size,
                row.modified_at.as_deref()
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
        .rows("measured")
        .remove(&ids[1])
        .expect("run 1 wrote a measured row for every member");
    let artifact = PathBuf::from(row.path.unwrap());
    let mut bytes = std::fs::read(&artifact).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x5a;
    std::fs::write(&artifact, &bytes).unwrap();
    rig.delete_rows(&["fits.0"]);
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
        .rows("measured")
        .values()
        .map(|row| PathBuf::from(row.path.clone().unwrap()))
        .collect();
    assert!(
        !written.is_empty() && written.iter().all(|p| p.is_file()),
        "the cancelled run left rows for files that exist"
    );
    let kept = rig.rows("measured").len();
    assert!(kept < 4, "the cancel landed mid-phase-1 ({kept} of 4 written)");
    let second = rig.run("run 2", None);
    assert_eq!(
        second.logged.len(),
        4 - kept,
        "the next run runs pass 1 only for the frames the cancelled run did not finish"
    );
    assert_eq!(rig.rows("metrics").len(), 4);
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
    let artifact = PathBuf::from(rig.rows("measured")[&ids[1]].path.clone().unwrap());
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
    assert!(rig.rows("measured").is_empty(), "no row for a file that is not there");
    assert_eq!(rig.rows("metrics").len(), 4);
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
    assert_eq!(rig.rows("measured").len(), 4);
}

/// Plan A Task 11c (upgrade path): a member with a `metrics` row but no artifact (a catalog from before the
/// spill) runs pass 1 inside pass 2 ONCE and leaves the artifact; the next group-beta move reads it.
#[test]
fn measure_pass1_upgrade_runs_it_inside_pass_two_once() {
    let rig = Pass1Rig::new();
    rig.run("run 1", None);
    rig.delete_rows(&["measured"]);
    let _ = std::fs::remove_dir_all(rig.measured_root());
    let ids = rig.sorted_ids();
    rig.lower_own_betas(&[(ids[0], 1.6), (ids[1], 1.7)]);
    let second = rig.run("run 2", None);
    assert_eq!(second.beta, Some(1.7));
    assert_eq!(second.logged.len(), 4, "the upgrade ripple: pass 1 inside pass 2, once");
    assert_eq!(rig.rows("measured").len(), 4);
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
        let metrics = rig.rows("metrics");
        let fits = rig.rows("fits.0");
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

    rig.delete_rows(&["fits.0"]);
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
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some("metrics")).unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        let frame_id = row.frame_id.unwrap();
        let calibrated = crate::db::stacking::find_artifact(
            &fixture.conn,
            &subject,
            "",
            "calibrated",
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
        rows_of_kind(&fixture.conn, fixture.set_id, "measured").is_empty(),
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
        Some("metrics"),
    )
    .unwrap();
    assert_eq!(metrics.len(), 4, "{metrics:?}");
    let fits_rows = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("fits.0"),
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
        Some("fits.0"),
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
        Some("metrics"),
    )
    .unwrap();
    assert_eq!(metrics_before.len(), 4, "{metrics_before:?}");
    let fits_before = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("fits.0"),
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
        Some("metrics"),
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
        Some("fits.0"),
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
        Some("fits.0"),
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
        Some("metrics"),
    )
    .unwrap();
    assert_eq!(metrics_after.len(), 4, "{metrics_after:?}");

    // No row was written for a file that is not on disk: the victim's
    // row is the ONE left over from run 1, and it reads STALE (the
    // recorded size does not match what is at that path now), which is
    // exactly the `resolve_register_fits` gate — a frame with no fresh
    // `fits.0` registers through `detect_frame_stars`'s detection arm,
    // `star_source = "detected"`. Its three siblings stay `"fits"`.
    let fits_after = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("fits.0"),
    )
    .unwrap();
    assert_eq!(
        fits_after.len(),
        4,
        "the skipped write leaves run 1's row in place, it does not add one: {fits_after:?}"
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

// ── two-pass registration reference (M4a Task 4, ruling R-M4a-5) ──────

/// [`BASE_STARS`] rotated `deg` about the fixture canvas's centre and
/// then shifted by `(dx, dy)` — the fixture shape the two-pass pick
/// exists for: one frame whose own pointing deviates from the rest of
/// its group. [`shifted_stars`] can only translate.
fn rotated_shifted_stars(deg: f64, dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
    let cx = STAR_FIELD_WIDTH as f64 / 2.0;
    let cy = STAR_FIELD_HEIGHT as f64 / 2.0;
    let (s, c) = deg.to_radians().sin_cos();
    BASE_STARS
        .iter()
        .map(|&(x, y, a)| {
            let (px, py) = (x - cx, y - cy);
            (cx + px * c - py * s + dx, cy + px * s + py * c + dy, a)
        })
        .collect()
}

/// As [`seed_star_group`], but with an explicit star field per frame —
/// that helper can only shift ONE field, and these tests need frame A
/// ROTATED against the other five.
fn seed_star_group_with_fields(
    db_path: &Path,
    set_name: &str,
    fields: &[(Vec<(f64, f64, f64)>, f32)],
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, (stars, sigma)) in fields.iter().enumerate() {
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = star_light_spec(&stem, &date_obs);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            stars,
            600.0,
            *sigma,
            100 + i as u64,
        );
        light_ids.push(id);
    }

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

    (fixture, light_ids, working, output)
}

/// Six frames whose pointing agrees except for frame A (`f0`), which is
/// rotated 0.8 deg AND offset (6, -5) px from the other five — and is
/// also the cleanest frame (noise 3.0 against 5.0), so stage 4's
/// best-by-weight pick lands on exactly the frame whose geometry the
/// whole set would then inherit.
///
/// The offset carries most of the deviation ON PURPOSE: these fixtures
/// are 192x144, so 0.8 deg of rotation alone displaces a corner by only
/// `0.8° · hypot(192, 144)/2 = 1.7 px` — below [`TWO_PASS_MIN_GAIN_PX`]
/// by construction, and a switch on that would be exactly the sub-pixel
/// churn the floor exists to prevent. On the real 6224x4168 frames the
/// rotation term alone is 33 px.
fn two_pass_fields() -> Vec<(Vec<(f64, f64, f64)>, f32)> {
    vec![
        (rotated_shifted_stars(0.8, 6.0, -5.0), 3.0f32),
        (rotated_shifted_stars(0.0, 0.0, 0.0), 5.0f32),
        (rotated_shifted_stars(0.0, 1.0, 0.0), 5.0f32),
        (rotated_shifted_stars(0.0, 0.0, 1.0), 5.0f32),
        (rotated_shifted_stars(0.0, 1.0, 1.0), 5.0f32),
        (rotated_shifted_stars(0.0, 2.0, 1.0), 5.0f32),
    ]
}

/// Builds a `RunContext` over [`two_pass_fields`] and runs stages 1-4,
/// asserting that stage 4 did pick frame A — the premise every test
/// below rests on. The caller then drives stage 5 itself.
fn two_pass_context(
    tmp: &tempfile::TempDir,
    cfg: StackingConfig,
) -> (
    Arc<ServiceContext>,
    test_fixtures::Fixture,
    Vec<i64>,
    RunContext,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) =
        seed_star_group_with_fields(&db_path, SET_NAME, &two_pass_fields());
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

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
    (ctx, fixture, light_ids, rc, working, output)
}

/// Ruling R-M4a-5: stage 4 picks frame A on weight, stage 5's dry pass
/// measures how far A's own pointing sits from the group's median, and
/// the reference moves to one of the other five. The run row, the
/// summary block, the run warning and every persisted
/// `registration_results` row all name the FINAL reference.
#[test]
fn two_pass_moves_the_reference_off_a_deviating_top_weight_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, fixture, light_ids, mut rc, working, _output) =
        two_pass_context(&tmp, StackingConfig::default());
    assert!(rc.config.reference.two_pass, "the default must be on");
    assert_eq!(
        rc.reference_frame_id,
        Some(light_ids[0]),
        "fixture premise: stage 4 picks the deviating frame A on weight"
    );

    // Perf tier 1 Task 9: pinned structurally rather than through a
    // captured "frame stars detected"/"frame registered" log line —
    // `fan_out`'s workers run on their own `std::thread::scope`
    // threads, which start with no thread-local `tracing` dispatcher
    // of their own, so a `set_default`-scoped capture layer (the
    // pattern `sync::engine_tests`/`sharing::iroh::tests` use
    // elsewhere in this crate) would silently observe nothing from
    // inside a fan-out; only a `set_global_default` would reach it,
    // and that can be installed at most once per test binary — not
    // usable per-test.
    //
    // `DETECT_FRAME_STARS_LOG` logs every real call's path
    // process-wide, so under `cargo test`'s default parallelism other
    // `stacking::run`/`stacking::register` tests' own calls land in
    // the same log while this one runs (confirmed empirically: a bare
    // `AtomicUsize` delta read 7 instead of 6 under
    // `--test-threads=8`, matching single-threaded exactly at 6) —
    // filtering the new entries down to paths under THIS test's own
    // `working` tempdir (every fixture writes its calibrated frames
    // under its own unique dir) isolates the count correctly
    // regardless of how many sibling tests are registering frames of
    // their own at the same moment.
    let detect_log_before = crate::stacking::register::frame::DETECT_FRAME_STARS_LOG
        .lock()
        .unwrap()
        .len();

    stage_register(&mut rc).unwrap();

    let detect_calls: Vec<PathBuf> = {
        let log = crate::stacking::register::frame::DETECT_FRAME_STARS_LOG
            .lock()
            .unwrap();
        log[detect_log_before..]
            .iter()
            .filter(|p| p.starts_with(working.path()))
            .cloned()
            .collect()
    };
    // The fixture's own two-pass switch (asserted below) means every
    // one of the group's 6 included frames is detected EXACTLY once
    // across the whole stage: the dry pass detects the 5 non-original-
    // reference candidates (including the eventual `chosen` one, whose
    // detections the persisting pass then reuses for the switched
    // reference's own `ReferenceStars` — see `two_pass_refine`), and
    // the persisting pass detects exactly the 1 remaining frame (the
    // ORIGINAL reference, now a plain subject) that the dry pass never
    // touched. Before this task, `two_pass_refine`'s switch resolution
    // always re-read+re-detected the winning candidate too, and the
    // persisting pass always re-registered the whole group regardless
    // of the dry pass — 5 (dry) + 5 (persisting, the group minus
    // `chosen`) = 10 through `detect_frame_stars`'s own predecessor
    // logic (what this counter observes), or 12 counting the stage's
    // two separate `reference_stars` reads (the initial reference and
    // the switch's own re-detection of `chosen`, neither of which
    // this counter — scoped to `detect_frame_stars` — sees).
    assert_eq!(
        detect_calls.len(),
        light_ids.len(),
        "every included frame is read and detected exactly once across the two passes: {detect_calls:?}"
    );
    // Fix round 1, Minor 5: a bare count could hide a frame detected
    // twice offsetting one never detected at all — assert the paths
    // are DISTINCT so "each frame exactly once" is what is actually
    // pinned, not just "six detections happened somewhere".
    assert_eq!(
        detect_calls.iter().collect::<HashSet<_>>().len(),
        light_ids.len(),
        "each frame must be detected on its own distinct path, no repeats: {detect_calls:?}"
    );
    assert!(
        rc.dry_pass_stars.is_empty(),
        "the dry-pass cache must be drained/cleared by the end of the stage: {:?}",
        rc.dry_pass_stars.keys().collect::<Vec<_>>()
    );

    let chosen = rc.reference_frame_id.expect("a reference after stage 5");
    assert_ne!(chosen, light_ids[0], "the deviating frame must not stay");
    assert!(
        light_ids[1..].contains(&chosen),
        "the new reference must be one of the five agreeing frames: {chosen}"
    );

    let row = crate::db::stacking::get_run(&fixture.conn, rc.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.reference_frame_id, Some(chosen));
    assert_eq!(row.reference_mode, "auto");

    assert_eq!(rc.summary.reference.frame_id, Some(chosen));
    assert_eq!(rc.summary.reference.switched_from, Some(light_ids[0]));
    // The exact wording of ruling R-M4a-5, including the corner
    // displacements — the string literal is line-continued in the
    // source, so this also pins that it renders as one flat sentence.
    let switch_warning = rc
        .warnings
        .iter()
        .find(|w| w.starts_with("reference switched by the two-pass pick:"))
        .unwrap_or_else(|| panic!("no switch warning in {:?}", rc.warnings));
    assert!(
        switch_warning.contains(&format!(
            ": {} → {chosen} (corner displacement ",
            light_ids[0]
        )),
        "{switch_warning}"
    );
    assert!(switch_warning.ends_with(" px)"), "{switch_warning}");
    assert!(
        switch_warning.matches(" → ").count() == 2,
        "{switch_warning}"
    );

    // Pass 1 persisted nothing: every row is pass 2's, against the
    // final reference, with exactly one identity row.
    let rows = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    assert_eq!(rows.len(), light_ids.len(), "{rows:?}");
    assert!(
        rows.iter().all(|r| r.reference_frame_id == chosen),
        "{rows:?}"
    );
    assert_eq!(rows.iter().filter(|r| r.is_reference).count(), 1);
    assert!(rows.iter().any(|r| r.frame_id == chosen && r.is_reference));

    // One Register timing covering both passes, and the Reference
    // timing stage 4 pushed — no extra stage rows.
    assert_eq!(
        rc.timings
            .iter()
            .filter(|t| t.stage == Stage::Register)
            .count(),
        1,
        "{:?}",
        rc.timings
    );
}

/// Fix round 1, item 3 — the discriminating pin for the dry pass.
/// `upsert_registration` is `INSERT OR REPLACE`, so a run-level "pass 1
/// persisted nothing" assertion cannot tell a dry pass that wrote no
/// row from one whose rows pass 2 simply overwrote. This drives
/// [`register_group_pass`] DIRECTLY, both ways, over the same context:
/// with `persist = false` the `registration_results` table stays EMPTY
/// and no entry carries a registration outcome; the very next call with
/// `persist = true` produces both.
#[test]
fn a_dry_register_pass_writes_no_rows_and_no_outcomes() {
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, fixture, light_ids, mut rc, _working, _output) =
        two_pass_context(&tmp, StackingConfig::default());

    // Everything `stage_register` resolves before its own group loop.
    let cfg = rc.config.clone();
    let group = rc.plan_groups[0].clone();
    let reference_frame_id = rc.reference_frame_id.unwrap();
    let reference_calibrated = rc.reference_calibrated.clone().unwrap();
    let ref_stars = reference_stars(
        &reference_calibrated,
        &cfg.registration,
        Some(&rc.ctx.image_pool),
        None,
    )
    .unwrap();
    // (M4b Task 3 fix round 1, m7: nothing sets `rc.reference_width`/
    // `height` here any more — `register_group_pass` reads the group's
    // own `GroupGeometry`, which stage 4 already resolved.)
    let reference_group_frame =
        find_frame_in_groups(&rc.plan_groups, reference_frame_id).unwrap();
    let reference_hash = {
        let conn = db(&rc.ctx).unwrap().conn();
        rc.memo
            .calibration_hash_checked(&conn, &cfg, &reference_group_frame)
            .unwrap()
    };
    // No stored rows exist yet, so the cache map is empty either way —
    // the point here is what the pass WRITES, not what it reuses.
    let by_frame: HashMap<i64, RegistrationRecord> = HashMap::new();
    assert!(
        get_registration_for_frame_set(&fixture.conn, fixture.set_id)
            .unwrap()
            .is_empty(),
        "premise: nothing registered before this test runs"
    );

    let mut progress = (0usize, light_ids.len() * 2);
    let dry = register_group_pass(
        &mut rc,
        &group,
        &ref_stars,
        reference_frame_id,
        &reference_hash,
        &by_frame,
        false,
        false,
        &mut progress,
        &mut SolveCache::new(),
    )
    .unwrap();

    assert_eq!(
        dry.len(),
        light_ids.len(),
        "the dry pass reports every frame it registered: {dry:?}"
    );
    assert!(
        dry.iter().all(|r| r.outcome.is_ok()),
        "the fixture aligns every frame: {dry:?}"
    );
    assert!(
        get_registration_for_frame_set(&fixture.conn, fixture.set_id)
            .unwrap()
            .is_empty(),
        "a dry pass must write no registration_results row"
    );
    assert!(
        rc.measured[&group.key]
            .iter()
            .all(|e| e.registration.is_none()),
        "a dry pass must store no per-frame registration outcome"
    );
    assert_eq!(
        progress.0,
        light_ids.len(),
        "the dry pass still ticks the shared Register progress"
    );

    // The same call with `persist = true`, on the SAME context: now the
    // rows and the per-frame outcomes appear.
    let wet = register_group_pass(
        &mut rc,
        &group,
        &ref_stars,
        reference_frame_id,
        &reference_hash,
        &by_frame,
        false,
        true,
        &mut progress,
        &mut SolveCache::new(),
    )
    .unwrap();

    assert!(
        wet.is_empty(),
        "a persisting pass records its outcomes where the run reads them, \
             and returns none of its own: {wet:?}"
    );
    let rows = get_registration_for_frame_set(&fixture.conn, fixture.set_id).unwrap();
    assert_eq!(rows.len(), light_ids.len(), "{rows:?}");
    assert_eq!(rows.iter().filter(|r| r.is_reference).count(), 1);
    assert!(rc.measured[&group.key]
        .iter()
        .all(|e| e.registration.is_some()));
    assert_eq!(progress.0, light_ids.len() * 2);
}
/// The same fixture with the toggle off: nothing re-picks, frame A
/// stays the reference, no warning is added and no switch is recorded.
#[test]
fn two_pass_off_keeps_the_stage_four_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = StackingConfig::default();
    cfg.reference.two_pass = false;
    let (_ctx, fixture, light_ids, mut rc, _working, _output) = two_pass_context(&tmp, cfg);
    assert_eq!(rc.reference_frame_id, Some(light_ids[0]));

    stage_register(&mut rc).unwrap();

    assert_eq!(rc.reference_frame_id, Some(light_ids[0]));
    assert_eq!(rc.summary.reference.switched_from, None);
    assert!(
        !rc.warnings.iter().any(|w| w.contains("two-pass")),
        "{:?}",
        rc.warnings
    );
    let row = crate::db::stacking::get_run(&fixture.conn, rc.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.reference_frame_id, Some(light_ids[0]));
}

/// "Manual references never move" (ruling R-M4a-5): the same deviating
/// frame A, pinned by hand, with `twoPass` left ON — the dry pass does
/// not run at all.
#[test]
fn two_pass_never_moves_a_manual_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) =
        seed_star_group_with_fields(&db_path, SET_NAME, &two_pass_fields());
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
    assert!(cfg.reference.two_pass, "left on deliberately");

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

    run_stages_for_test(&mut rc, Stage::Register).unwrap();

    assert_eq!(rc.reference_frame_id, Some(light_ids[0]));
    assert_eq!(rc.summary.reference.switched_from, None);
    assert!(
        !rc.warnings.iter().any(|w| w.contains("two-pass")),
        "{:?}",
        rc.warnings
    );
    let row = crate::db::stacking::get_run(&fixture.conn, rc.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.reference_frame_id, Some(light_ids[0]));
    assert_eq!(row.reference_mode, "manual");
}

// ── two-pass decision cache (frame-level cache spec §5.6, Task 10) ────

/// Six frames of [`two_pass_fields`] on an Auto reference with `twoPass` on, run through Register as often as a
/// test asks: a second run finds the first one's rows, as a user's second run does.
struct TwoPassRig {
    _tmp: tempfile::TempDir,
    ctx: Arc<ServiceContext>,
    fixture: test_fixtures::Fixture,
    light_ids: Vec<i64>,
    working: tempfile::TempDir,
    output: tempfile::TempDir,
    layout: WorkingLayout,
}

impl TwoPassRig {
    fn new() -> TwoPassRig {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, light_ids, working, output) =
            seed_star_group_with_fields(&db_path, SET_NAME, &two_pass_fields());
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        TwoPassRig { _tmp: tmp, ctx, fixture, light_ids, working, output, layout }
    }

    /// One run through Register, `emitter` receiving its progress.
    fn run(&self, emitter: Arc<dyn ProgressEmitter>) -> RunContext {
        self.run_cfg(StackingConfig::default(), emitter)
    }

    /// One run through Register under `cfg`.
    fn run_cfg(&self, cfg: StackingConfig, emitter: Arc<dyn ProgressEmitter>) -> RunContext {
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
            cfg,
            plan_groups,
            self.layout.clone(),
            self.output.path().to_path_buf(),
            group_ids,
        );
        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        rc
    }

    /// Every member's stored registration transform (`registration_results`), by frame id.
    fn transforms(&self) -> HashMap<i64, Option<String>> {
        get_registration_for_frame_set(&self.fixture.conn, self.fixture.set_id)
            .unwrap()
            .into_iter()
            .map(|r| (r.frame_id, r.transform_json))
            .collect()
    }

    /// The group's `two_pass` row, if any.
    fn decision_row(&self, rc: &RunContext) -> Option<crate::db::stacking::StackingArtifactRow> {
        let key = &rc.plan_groups[0].key;
        crate::db::stacking::find_artifact(
            &self.fixture.conn,
            &StackSubject::FrameSet(self.fixture.set_id),
            artifact_group_key(TWO_PASS_ARTIFACT_KIND, key),
            TWO_PASS_ARTIFACT_KIND,
            None,
        )
        .unwrap()
    }
}

/// Spec §5.6: the dry pass runs once. Run 1 registers the six members twice (the dry pass, then the real one) and
/// records `{"switchTo": <frame>}` on the group; run 2 finds the row fresh, skips the dry pass (no fan-out
/// opens: the real pass is all cache hits too), and ends on the same final reference and `switched_from`.
#[test]
fn two_pass_decision_is_cached_on_the_second_run() {
    let rig = TwoPassRig::new();
    let (rc1, logs1) =
        crate::stacking::telemetry::capture_logs(|| rig.run(Arc::new(NullEmitter)));
    let chosen = rc1.reference_frame_id.unwrap();
    assert_ne!(chosen, rig.light_ids[0], "premise: the dry pass switched the reference");
    assert_eq!(
        fan_out_items(&logs1, "register", "align"),
        2 * rig.light_ids.len(),
        "run 1: the dry pass and the real pass each register every member: {logs1}"
    );
    let row = rig.decision_row(&rc1).expect("run 1 recorded its decision");
    assert_eq!(row.group_key, rc1.plan_groups[0].key, "group-level");
    assert!(row.path.is_none() && row.frame_id.is_none());
    let payload: serde_json::Value = serde_json::from_str(row.payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(payload, serde_json::json!({ "switchTo": chosen }));

    // Ruling R14 (e): the stars a hit builds for the switched reference are the ones the dry pass built. One
    // member that is neither the old nor the new reference loses its registration row, so run 2 registers it
    // again against the hit's reference stars: its transform must be run 1's bit for bit.
    let transforms1 = rig.transforms();
    let probe = *rig
        .light_ids
        .iter()
        .find(|&&id| id != chosen && id != rig.light_ids[0])
        .unwrap();
    assert!(transforms1[&probe].is_some(), "premise: run 1 stored the probe's transform");
    assert_eq!(
        rig.fixture
            .conn
            .execute(
                "DELETE FROM registration_results WHERE frames_set_id = ?1 AND frame_id = ?2",
                rusqlite::params![rig.fixture.set_id, probe],
            )
            .unwrap(),
        1
    );

    let (rc2, logs2) =
        crate::stacking::telemetry::capture_logs(|| rig.run(Arc::new(NullEmitter)));
    assert_eq!(
        fan_out_items(&logs2, "register", "align"),
        1,
        "run 2 registers the probe only: no dry pass, every other member reused: {logs2}"
    );
    let transforms2 = rig.transforms();
    assert_eq!(
        transforms2[&probe], transforms1[&probe],
        "the probe's transform moved: the stars built on a hit differ from the dry pass's"
    );
    assert!(logs2.contains("two-pass decision reused"), "{logs2}");
    assert_eq!(rc2.reference_frame_id, Some(chosen));
    assert_eq!(rc2.summary.reference.frame_id, Some(chosen));
    assert_eq!(rc2.summary.reference.switched_from, Some(rig.light_ids[0]));
    assert!(
        rc2.warnings.iter().any(|w| w.starts_with("reference switched by the two-pass pick:")),
        "{:?}",
        rc2.warnings
    );
    let run_row = crate::db::stacking::get_run(&rig.fixture.conn, rc2.run_id).unwrap().unwrap();
    assert_eq!(run_row.reference_frame_id, Some(chosen));
    assert_eq!(
        rig.decision_row(&rc2).unwrap().config_hash,
        row.config_hash,
        "the same inputs, the same key"
    );
}

/// One member recalibrated (its source file's catalog size moved, so its calibration hash and with it its
/// registration hash against the candidate move): the key moves, the row is stale, the dry pass runs again and
/// the row is rewritten under the new key.
#[test]
fn two_pass_cache_misses_when_a_member_changes() {
    let rig = TwoPassRig::new();
    let rc1 = rig.run(Arc::new(NullEmitter));
    let key1 = rig.decision_row(&rc1).unwrap().config_hash;

    rig.fixture
        .conn
        .execute(
            "UPDATE files SET size = size + 1000 WHERE id = \
                 (SELECT file_id FROM frames WHERE id = ?1)",
            rusqlite::params![rig.light_ids[2]],
        )
        .unwrap();
    let (rc2, logs2) =
        crate::stacking::telemetry::capture_logs(|| rig.run(Arc::new(NullEmitter)));
    assert!(!logs2.contains("two-pass decision reused"), "{logs2}");
    assert!(
        fan_out_items(&logs2, "register", "align") >= rig.light_ids.len(),
        "the dry pass did not run again: {logs2}"
    );
    let key2 = rig.decision_row(&rc2).unwrap().config_hash;
    assert_ne!(key1, key2, "the key ignored a changed member");
}

/// A `selection.minWeightFraction` that floors three of [`two_pass_fields`]' six members at Measure time (their
/// weights are about 0.296, 0.296, 0.304 against 0.39, 0.40 and 1.0), with a margin of 0.036 either side.
const FLOOR_FLOORING_THREE_MEMBERS: f64 = 0.34;

/// Ruling R14 (a): a `selection.minWeightFraction` edit changes no hash the members carry, but it moves which
/// members the Measure-time floor takes out of the pick (PSF modes defer the gate). The floored set is a term of
/// the key, so the edit is a miss.
#[test]
fn two_pass_cache_misses_when_a_floor_edit_floors_a_member() {
    let rig = TwoPassRig::new();
    let rc1 = rig.run(Arc::new(NullEmitter));
    let key1 = rig.decision_row(&rc1).unwrap().config_hash;
    let group = rc1.plan_groups[0].key.clone();
    assert!(
        !rc1.measured[&group].iter().any(|e| e.measure_floored),
        "premise: the default floor takes nobody out"
    );

    let mut cfg = StackingConfig::default();
    cfg.selection.min_weight_fraction = FLOOR_FLOORING_THREE_MEMBERS;
    let (rc2, logs2) = crate::stacking::telemetry::capture_logs(|| rig.run_cfg(cfg, Arc::new(NullEmitter)));
    assert!(
        rc2.measured[&group].iter().any(|e| e.measure_floored),
        "premise: the edit floors a member"
    );
    assert!(!logs2.contains("two-pass decision reused"), "the floored pick reused an unfloored decision: {logs2}");
    let key2 = rig.decision_row(&rc2).unwrap().config_hash;
    assert_ne!(key1, key2, "the key ignored the floored member");
}

fn ranking_entry(id: i64, weight: Option<f64>, floored: bool) -> MeasuredFrame {
    use crate::stacking::groups::GroupFrame;
    MeasuredFrame {
        frame: GroupFrame {
            frame_id: id,
            file_id: id,
            filename: format!("f{id}.fits"),
            path: format!("/f{id}.fits"),
            size: 1,
            modified_at: "2026-01-01".into(),
            exposure_s: Some(60.0),
            date_obs: None,
            width: 64,
            height: 48,
            pixel_scale_arcsec: None,
            scale_source: None,
        },
        calibrated: Some(PathBuf::from(format!("/c_f{id}.fits"))),
        planes: 1,
        measurement: None,
        weight: weight.map(|w| FrameWeight {
            channels: vec![w],
            normalized: vec![w],
            mean: w,
            normalized_mean: w,
            missing: None,
        }),
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
        measure_floored: floored,
    }
}

/// Ruling R14 (a), (c): the key follows the floored members, the WHOLE weight ranking (a swap below the top N
/// moves it), the members whose weight is not finite and the pick version.
#[test]
fn two_pass_key_follows_the_floor_the_whole_ranking_and_the_pick_version() {
    let cfg = StackingConfig::default();
    let hashes: Vec<(i64, String)> = (1..=12).map(|id| (id, format!("h{id}"))).collect();
    let entries = |weights: &[f64], floored: &[i64]| -> Vec<MeasuredFrame> {
        weights
            .iter()
            .enumerate()
            .map(|(i, w)| ranking_entry(i as i64 + 1, Some(*w), floored.contains(&(i as i64 + 1))))
            .collect()
    };
    let key_of = |entries: &[MeasuredFrame]| {
        two_pass_key(&cfg, 1, "calib", &two_pass_ranking(entries, 1, None), None, &hashes)
    };
    let weights: Vec<f64> = (0..12).map(|i| 1.0 - 0.05 * i as f64).collect();
    let base = entries(&weights, &[]);
    assert!(crate::stacking::weights::TWO_PASS_CANDIDATES < 11, "premise: ranks 11 and 12 are below the top N");
    let base_key = key_of(&base);
    assert_eq!(base_key, key_of(&entries(&weights, &[])), "premise: the key is deterministic");

    // A floor edit that floors a member.
    assert_ne!(base_key, key_of(&entries(&weights, &[5])), "a floored member is a key term");
    // A floored member below the top N leaves the full ranking (a truncated one would not notice).
    assert_ne!(base_key, key_of(&entries(&weights, &[12])), "a floored member below the top N is a key term");
    // A floored member a project's WCS rule keeps out of the ranking anyway still leaves the dry pass's median,
    // so the floored set is a term of its own.
    let allowed: HashSet<i64> = (1..=12).filter(|id| *id != 5).collect();
    let allowed_key = |floored: &[i64]| {
        two_pass_key(
            &cfg,
            1,
            "calib",
            &two_pass_ranking(&entries(&weights, floored), 1, Some(&allowed)),
            None,
            &hashes,
        )
    };
    assert_ne!(allowed_key(&[]), allowed_key(&[5]), "the floored set must be a key term of its own");
    // The reference itself stays a candidate whatever its flag, so flagging it is not a change.
    assert_eq!(base_key, key_of(&entries(&weights, &[1])), "the reference is never floored out");
    // A swap of ranks 11 and 12: outside the top N, inside the ranking.
    let mut swapped = weights.clone();
    swapped.swap(10, 11);
    assert_ne!(base_key, key_of(&entries(&swapped, &[])), "the ranking below the top N is a key term");
    // A member whose weight is not finite leaves the ranking and is named apart.
    let mut nan = weights.clone();
    nan[6] = f64::NAN;
    let nan_entries = entries(&nan, &[]);
    let ranking = two_pass_ranking(&nan_entries, 1, None);
    assert_eq!(ranking.non_finite, vec![7]);
    assert!(!ranking.ranked.contains(&7));
    assert_ne!(base_key, key_of(&nan_entries));
    // A member with no weight at all ranks last, finite.
    let mut unweighed = entries(&weights, &[]);
    unweighed[2].weight = None;
    assert_eq!(*two_pass_ranking(&unweighed, 1, None).ranked.last().unwrap(), 3);
    // The pick version is a term.
    let ranking = two_pass_ranking(&base, 1, None);
    let with_version = |v: u32| two_pass_key_with_version(&cfg, v, 1, "calib", &ranking, None, &hashes);
    assert_eq!(base_key, with_version(TWO_PASS_PICK_VERSION));
    assert_ne!(base_key, with_version(TWO_PASS_PICK_VERSION + 1), "the key ignored the pick version");
}

/// The dry pass names itself in the Register progress ("checking the reference"), the real pass is "aligning",
/// and the stage total counts the dry pass only while it runs: 12 on run 1, 6 once the decision is cached.
#[test]
fn dry_pass_progress_names_its_pass() {
    let register_events = |rec: &Recording| -> Vec<serde_json::Value> {
        rec.events(STACKING_PROGRESS_EVENT)
            .into_iter()
            .filter(|e| e["stage"] == Stage::Register.as_str())
            .collect()
    };
    let rig = TwoPassRig::new();
    let members = rig.light_ids.len() as u64;

    let rec1 = Arc::new(Recording::new());
    rig.run(rec1.clone() as Arc<dyn ProgressEmitter>);
    let events1 = register_events(&rec1);
    assert!(
        events1.iter().all(|e| e["total"] == 2 * members),
        "run 1: the total is the dry pass plus the real one: {events1:?}"
    );
    let messages1: Vec<Option<&str>> = events1.iter().map(|e| e["message"].as_str()).collect();
    let first_aligning = messages1
        .iter()
        .position(|m| *m == Some("aligning"))
        .expect("a real-pass tick carries \"aligning\"");
    assert!(
        messages1[..first_aligning].contains(&Some("checking the reference")),
        "a dry-pass tick carries \"checking the reference\": {messages1:?}"
    );
    assert!(
        !messages1[first_aligning..].contains(&Some("checking the reference")),
        "the dry pass is over before the real pass starts: {messages1:?}"
    );

    let rec2 = Arc::new(Recording::new());
    rig.run(rec2.clone() as Arc<dyn ProgressEmitter>);
    let events2 = register_events(&rec2);
    assert!(
        events2.iter().all(|e| e["total"] == members),
        "run 2: a cached decision removes the dry pass from the total: {events2:?}"
    );
    assert!(
        events2.iter().all(|e| e["message"] != "checking the reference"),
        "{events2:?}"
    );
    let last = events2.last().expect("Register ticked");
    assert_eq!((last["current"].as_u64(), last["message"].as_str()), (Some(members), Some("aligning")));
}

// ── M4b Task 2: mixed pixel scales inside one group ──────────────────

/// The coarse half of the mixed-scale fixture: [`BASE_STARS`] as they
/// are, in a [`STAR_FIELD_WIDTH`]x[`STAR_FIELD_HEIGHT`] frame …
const COARSE_W: usize = STAR_FIELD_WIDTH;
const COARSE_H: usize = STAR_FIELD_HEIGHT;
/// … and the fine half: the SAME sky sampled twice as finely — every
/// star at twice the pixel coordinate, in a frame twice the size.
const FINE_W: usize = STAR_FIELD_WIDTH * 2;
const FINE_H: usize = STAR_FIELD_HEIGHT * 2;
const FINE_SCALE_ARCSEC: f64 = 0.78;
const COARSE_SCALE_ARCSEC: f64 = 1.56;
const MIXED_RA_DEG: f64 = 300.0;
const MIXED_DEC_DEG: f64 = 60.0;

/// [`BASE_STARS`] scaled about the pixel origin and dithered — the
/// same sky at `factor` times the sampling.
fn scaled_stars(factor: f64, dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
    BASE_STARS
        .iter()
        .map(|&(x, y, a)| (x * factor + dx, y * factor + dy, a))
        .collect()
}

fn mixed_light_spec<'a>(
    stem: &'a str,
    date_obs: &'a str,
    width: usize,
    height: usize,
) -> LightSpec<'a> {
    LightSpec {
        stem,
        instrume: "cam",
        filter: None,
        binning: 1,
        width,
        height,
        exptime: 60.0,
        date_obs,
        bayerpat: None,
        write_file: true,
    }
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

// ── M4b Task 3: native geometry mode ─────────────────────────────────

/// Two groups in ONE set, at two pixel scales and two native
/// geometries: four fine frames ([`FINE_W`]x[`FINE_H`],
/// [`FINE_SCALE_ARCSEC`]) under one filter and three coarse ones
/// ([`COARSE_W`]x[`COARSE_H`], [`COARSE_SCALE_ARCSEC`]) under another.
/// The group key is colour mode + filter + binning + exposure, so the
/// filter alone splits them — geometry and camera never do (M2 Task
/// 10) — and the fine group is the larger, so `Auto` resolves the
/// RUN-wide reference into it.
///
/// Frame 0 of each group is the cleanest of its own group (noise 3.0
/// against 5.0), so each group's own best-by-weight pick is
/// deterministic. Every frame is plate-solved: that is what lets the
/// coarse group register onto the FINE reference at all in
/// co-registered mode (a ×2 scale step needs the WCS seed, M4b Task
/// 2) — the very comparison these tests rest on.
///
/// A THIRD group of two frames (filter `SII`, coarse geometry) rides
/// along: it is below the 3-frame viability floor, so stage 5 never
/// registers it and stage 9 skips it — the case fix round 1's m3 is
/// about (`SummaryGroup::reference_frame_id` must stay `None` for a
/// group that never registered onto anything).
///
/// Returns `(fixture, fine_ids, coarse_ids, tiny_ids, working, output)`.
fn seed_two_geometry_groups(
    db_path: &Path,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    Vec<i64>,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    let mut fine_ids = Vec::new();
    let mut coarse_ids = Vec::new();
    for (i, (dx, dy)) in [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)]
        .iter()
        .enumerate()
    {
        let date_obs = date_obs_at(i);
        let stem = format!("fine{i}");
        let mut spec = mixed_light_spec(&stem, &date_obs, FINE_W, FINE_H);
        spec.filter = Some("Ha");
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(2.0, *dx, *dy),
            600.0,
            if i == 0 { 3.0 } else { 5.0 },
            300 + i as u64,
        );
        fine_ids.push(id);
    }
    for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)].iter().enumerate() {
        let date_obs = date_obs_at(i + 4);
        let stem = format!("coarse{i}");
        let mut spec = mixed_light_spec(&stem, &date_obs, COARSE_W, COARSE_H);
        spec.filter = Some("OIII");
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(1.0, *dx, *dy),
            600.0,
            if i == 0 { 3.0 } else { 5.0 },
            310 + i as u64,
        );
        coarse_ids.push(id);
    }

    let mut tiny_ids = Vec::new();
    for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0)].iter().enumerate() {
        let date_obs = date_obs_at(i + 7);
        let stem = format!("tiny{i}");
        let mut spec = mixed_light_spec(&stem, &date_obs, COARSE_W, COARSE_H);
        spec.filter = Some("SII");
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &scaled_stars(1.0, *dx, *dy),
            600.0,
            5.0,
            320 + i as u64,
        );
        tiny_ids.push(id);
    }

    test_fixtures::add_master_dark_and_flat(&fixture, &fine_ids, FINE_W, FINE_H);
    test_fixtures::add_master_dark_and_flat(&fixture, &coarse_ids, COARSE_W, COARSE_H);
    test_fixtures::add_master_dark_and_flat(&fixture, &tiny_ids, COARSE_W, COARSE_H);

    for &id in &fine_ids {
        test_fixtures::seed_plate_solve_wcs(
            &fixture.conn,
            id,
            FINE_W,
            FINE_H,
            MIXED_RA_DEG,
            MIXED_DEC_DEG,
            FINE_SCALE_ARCSEC,
        );
    }
    for &id in coarse_ids.iter().chain(tiny_ids.iter()) {
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

    (fixture, fine_ids, coarse_ids, tiny_ids, working, output)
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

/// One group's summary, by key.
fn summary_group<'a>(rc: &'a RunContext, key: &str) -> &'a SummaryGroup {
    rc.summary
        .groups
        .iter()
        .find(|g| g.key == key)
        .unwrap_or_else(|| panic!("no summary for group {key}"))
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
                kind: &fits_artifact::artifact_kind(0),
                path: Some(path.to_str().unwrap()),
                config_hash: &expected_hash,
                size: Some(size),
                modified_at: None,
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
            kind: &fits_artifact::artifact_kind(0),
            path: Some(path.to_str().unwrap()),
            config_hash: &expected_hash,
            size: Some(size),
            modified_at: None,
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

// ── stages 6/7/9 (Task 8): normalize, integrate, output ────────────────

/// Every canonical stage in pipeline order, used to check the SET of
/// stages a full run's progress events cover, in the order they first
/// appear.
const CANONICAL_STAGE_ORDER: [&str; 7] = [
    "calibrate",
    "measure",
    "reference",
    "register",
    "normalize",
    "integrate",
    "output",
];

/// The distinct `stage` values of `events`, in first-appearance order —
/// used to check a run's progress events cover the canonical stage
/// sequence, and that `percent` is monotonic WITHIN each `(stage,
/// groupKey)` pair's own consecutive run of events (fix round 1, item 7:
/// NOT keyed on `stage` alone — `Normalize`/`Integrate`/`Output` each
/// reset to 0% per group, so a multi-group run legitimately sees percent
/// "go backwards" moving from one group's 100% to the next group's 0%
/// within the same stage; a per-frame stage like `register` carries a
/// `groupKey` too but its `current`/`total` are cumulative across every
/// group, so keying on the pair is still correct there — it just never
/// changes group_key mid-climb).
fn stage_sequence_and_monotonic(events: &[serde_json::Value]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut last_percent: HashMap<(String, String), f64> = HashMap::new();
    for e in events {
        let stage = e["stage"].as_str().unwrap_or("").to_string();
        if !order.contains(&stage) {
            order.push(stage.clone());
        }
        let group_key = e["groupKey"].as_str().unwrap_or("").to_string();
        let percent = e["percent"].as_f64().unwrap_or(0.0);
        let key = (stage.clone(), group_key.clone());
        if let Some(&prev) = last_percent.get(&key) {
            assert!(
                percent + 1e-9 >= prev,
                "percent went backwards for stage {stage} group {group_key}: {prev} -> {percent}"
            );
        }
        last_percent.insert(key, percent);
    }
    order
}

#[test]
fn combine_rows_tracker_follows_bands_and_never_double_counts() {
    // Three bands of 512/512/256 rows in a 1280-row plane.
    let mut t = CombineRowsTracker::default();
    assert_eq!(t.rows(), 0);
    t.on_combine(100, 512); // no band announced yet: ignored
    assert_eq!(t.rows(), 0);
    t.on_band(1);
    t.on_combine(100, 512);
    assert_eq!(t.rows(), 100);
    t.on_combine(512, 512); // band 1 done → committed
    assert_eq!(t.rows(), 512);
    t.on_combine(512, 512); // the engine repeats the final tick
    assert_eq!(t.rows(), 512, "a repeated final tick must not double-count");
    t.on_band(2); // band 2 read: percent holds at band 1's rows
    assert_eq!(t.rows(), 512);
    t.on_combine(10, 512);
    assert_eq!(t.rows(), 522);
    t.on_combine(512, 512);
    t.on_band(3);
    t.on_combine(256, 256);
    assert_eq!(t.rows(), 1280);
    assert!((t.frac(1280) - 1.0).abs() < 1e-12);
    t.on_band(0); // pass-2 forecast tick names no band
    assert_eq!(t.rows(), 1280);
    t.on_plane();
    assert_eq!(t.rows(), 0, "a new plane starts from zero");
    assert_eq!(t.frac(0), 0.0, "a zero-row plane reads 0, never NaN");
    // A band whose ticks never reach `h` contributes its last `done`
    // until the next band is announced, then that partial is dropped
    // rather than committed as a whole band.
    t.on_band(1);
    t.on_combine(300, 512);
    assert_eq!(t.rows(), 300);
    t.on_band(2);
    assert_eq!(t.rows(), 0, "an unfinished band is never committed");
}

#[test]
fn emit_integrate_ticks_never_race_percent_backwards() {
    // Fix round 1, item 1 + fix round 2: drives the REAL
    // `emit_integrate_tick` (not a reimplementation) from two genuine
    // `std::thread::scope` threads, both hitting the SAME
    // `IntegrateTickState` mutex, under FULLY DETERMINISTIC turn-taking
    // — kept alongside `emit_integrate_ticks_never_go_backwards_under_free_racing`
    // below because it proves something that one cannot: with no
    // legitimate reason for ANY tick to be stale (every value is
    // offered to the function in strict, correct order), the max-latch
    // must never drop a genuinely-valid tick — `events.len() == TOTAL`
    // below is exactly that "zero false positives" check. The free-race
    // test proves the opposite direction (stale ticks under genuine
    // racing are dropped, never delivered backwards) but, precisely
    // because it allows drops, cannot also prove the latch isn't
    // OVER-eager.
    //
    // Fix round 1's own history, preserved: a version of the free-race
    // test with no coordination beyond the mutex was flaky against the
    // fix-round-1-only code (~1 in 10 runs) — a value read outside the
    // lock can sit unsubmitted for an unbounded time regardless of lock
    // discipline inside this function, so a later, higher tick could
    // reach the recorder before an earlier, lower one. That is now
    // fully addressed by fix round 2's max-latch (the stale tick is
    // dropped, not delivered out of order) — see the free-race test
    // below, which fix round 1 could not have passed but fix round 2
    // does, reliably.
    //
    // This test's own determinism comes from a shared `turn` counter
    // that hands each value to the thread whose parity matches it,
    // advancing only AFTER that thread's own `emit_integrate_tick` call
    // has fully returned (a `compare_exchange`-before-call variant was
    // tried and failed the same way the free-race version does, for the
    // same reason — releasing the next turn before this call's own
    // critical section is done lets the two threads' calls run
    // concurrently again).
    struct ThreadPercentRecorder {
        events: Mutex<Vec<f64>>,
    }
    impl ProgressEmitter for ThreadPercentRecorder {
        fn emit_json(&self, _event_name: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push(payload["percent"].as_f64().unwrap_or(f64::NAN));
        }
    }

    let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
    let recorder = ThreadPercentRecorder {
        events: Mutex::new(Vec::new()),
    };
    let turn = std::sync::atomic::AtomicU64::new(0);
    const TOTAL: u64 = 400;
    const CHANNELS: usize = 1;

    let worker = |thread_id: u64| {
        loop {
            let v = turn.load(Ordering::SeqCst);
            if v >= TOTAL {
                return;
            }
            if v % 2 != thread_id {
                std::hint::spin_loop();
                continue;
            }
            emit_integrate_tick(
                &tick_state,
                &recorder,
                1,
                &StackSubject::FrameSet(1),
                "g",
                CHANNELS,
                0,
                v as f64 / TOTAL as f64,
                v,
                TOTAL,
                true, // force: every value must reach the recorder, or
                // a throttle-skipped tick could hide a real
                // ordering violation.
                None,
            );
            turn.store(v + 1, Ordering::SeqCst);
        }
    };

    std::thread::scope(|scope| {
        scope.spawn(|| worker(0));
        scope.spawn(|| worker(1));
    });

    let events = recorder.events.into_inner().unwrap();
    assert_eq!(
        events.len(),
        TOTAL as usize,
        "the latch must never drop a genuinely valid, in-order tick: {events:?}"
    );
    let mut last = -1.0f64;
    for &percent in &events {
        assert!(
            percent + 1e-9 >= last,
            "percent went backwards: {last} -> {percent}"
        );
        last = percent;
    }
}

/// Plan 6b-2 C1: a frame-set tick still carries its set id (and a null
/// project id); a project tick carries the project id and a null set id.
#[test]
fn a_tick_carries_the_subject_pair() {
    for (subject, set_id, project_id) in [
        (
            StackSubject::FrameSet(7),
            serde_json::json!(7),
            serde_json::Value::Null,
        ),
        (
            StackSubject::Project("p1".into()),
            serde_json::Value::Null,
            serde_json::json!("p1"),
        ),
    ] {
        let rec = Arc::new(Recording::new());
        let ticker = FanOutTicker::from_parts(
            rec.clone(),
            1,
            &subject,
            Stage::Measure,
            None,
            0,
            1,
            0,
            0,
        );
        ticker.tick(Some(3));
        let last = rec
            .events(STACKING_PROGRESS_EVENT)
            .pop()
            .expect("the first tick is always emitted");
        // `.get`, so a missing key cannot pass for a null one.
        assert_eq!(last.get("setId"), Some(&set_id), "{last}");
        assert_eq!(last.get("projectId"), Some(&project_id), "{last}");
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

fn gate_label(pass: &'static str) -> FanOutLabel {
    FanOutLabel {
        run_id: 77,
        stage: Stage::Measure,
        group_key: Some("g-test".to_string()),
        pass,
    }
}

/// Task 13c L4a: the cross-group queue runs longest first (descending working set, ties by group then item
/// order) and hands every group its results back in its OWN item order.
#[test]
fn fan_out_groups_runs_longest_first_and_splits_results_by_group() {
    let order = Mutex::new(Vec::<(usize, u32)>::new());
    let cancel = AtomicBool::new(false);
    // Group 0 small (ws 10), group 1 big (ws 30), group 2 small again (ws 10): one worker, so the pull
    // order is the queue order.
    let out = fan_out_groups(
        vec![vec![1u32, 2], vec![10, 20, 30], vec![100]],
        &[10, 30, 10],
        &AdmissionGate::fixed(1),
        gate_label("t"),
        &cancel,
        |g, v| {
            order.lock().unwrap().push((g, v));
            Ok::<u32, String>(v * 2)
        },
    );
    assert_eq!(
        *order.lock().unwrap(),
        vec![(1, 10), (1, 20), (1, 30), (0, 1), (0, 2), (2, 100)]
    );
    let flat: Vec<Vec<Option<Result<u32, String>>>> = vec![
        vec![Some(Ok(2)), Some(Ok(4))],
        vec![Some(Ok(20)), Some(Ok(40)), Some(Ok(60))],
        vec![Some(Ok(200))],
    ];
    assert_eq!(out, flat);
    // Admission N gives the same split.
    let wide = fan_out_groups(
        vec![vec![1u32, 2], vec![10, 20, 30], vec![100]],
        &[10, 30, 10],
        &AdmissionGate::fixed(8),
        gate_label("t"),
        &cancel,
        |_, v| Ok::<u32, String>(v * 2),
    );
    assert_eq!(wide, flat);
    // An empty group stays an empty slot.
    let with_empty = fan_out_groups(
        vec![vec![], vec![5u32]],
        &[1, 1],
        &AdmissionGate::fixed(2),
        gate_label("t"),
        &cancel,
        |_, v| Ok::<u32, String>(v),
    );
    assert_eq!(with_empty, vec![vec![], vec![Some(Ok(5))]]);
}

/// Task 13c L4a: one queue is used only when it lowers no group's admission. Uniform groups merge; a
/// group with a much bigger working set (or a big shared sum) makes the merged admission fall below a
/// small group's own, so the stage keeps its per-group queues.
#[test]
fn merged_admission_falls_back_when_it_would_lower_a_groups_admission() {
    const GIB: u64 = 1 << 30;
    let plane = |w: u64, h: u64| w * h * 4;
    // Uniform and tiny against a roomy budget: everything clamps to the pool width, so merge.
    let small = GroupLoad {
        working_set_bytes: 3 * plane(64, 48),
        shared_bytes: 2 * plane(64, 48),
    };
    let d = merged_admission(&[small, small], 4, Some(8 * GIB));
    assert_eq!((d.merged, d.per_group_min, d.per_group_max, d.merge), (4, 4, 4, true));
    // A mixed pair: mono 9576x6388 next to a tiny group on a 1 GiB budget. The big group's working set
    // sets the merged ws, so the tiny group would lose workers: fall back.
    let big = GroupLoad {
        working_set_bytes: 3 * plane(9576, 6388),
        shared_bytes: 2 * plane(9576, 6388),
    };
    let d = merged_admission(&[big, small], 16, Some(8 * GIB));
    assert!(d.merged < d.per_group_max, "{d:?}");
    assert!(!d.merge, "{d:?}");
    // Set 204 on the dev Mac (OSC 6248x4176 debayered + a mono group, 4 GiB, pool 10) falls back.
    let osc = GroupLoad {
        working_set_bytes: 5 * plane(6248, 4176),
        shared_bytes: 2 * plane(6248, 4176),
    };
    let mono = GroupLoad {
        working_set_bytes: 3 * plane(6248, 4176),
        shared_bytes: 2 * plane(6248, 4176),
    };
    let d = merged_admission(&[osc, mono], 10, Some(4 * GIB));
    assert!(!d.merge, "{d:?}");
    // An unknown budget is admission 1 everywhere: nothing to lose, merge.
    let d = merged_admission(&[big, small], 16, None);
    assert_eq!((d.merged, d.per_group_max, d.merge), (1, 1, true));
}

/// Task 13c L4a: stage 1 over several groups through ONE queue gives the files, warnings, exclusions and
/// cache flags the per-group queues give (same bytes, same commit order).
#[test]
fn calibrate_one_queue_across_groups_matches_per_group_queues() {
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
    type Rows = Vec<(String, Option<i64>, String, Option<i64>)>;
    type Snapshot = (
        Vec<(String, Vec<u8>)>,
        Vec<String>,
        Vec<(i64, String)>,
        Vec<(i64, bool)>,
        Rows,
        Vec<(u64, u64)>,
    );
    // `mode`: 0 = per-group queues, 1 = one queue over every group, 2 = the pre-L4 baseline — the stage run
    // once per group, in group order, on one run context.
    let run = |mode: u8| -> Snapshot {
        let w = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
        // The shared catalog keeps the other shape's rows otherwise: compare rows written by THIS shape.
        fixture
            .conn
            .execute("DELETE FROM stacking_artifacts", [])
            .unwrap();
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
        // Both runs share one catalog: regenerate rather than reuse the other run's rows.
        rc.rerun_from = Some(Stage::Calibrate);
        if mode == 2 {
            for g in &plan_groups {
                rc.plan_groups = vec![g.clone()];
                stage_calibrate_with(&mut rc, Some(false)).unwrap();
            }
            rc.plan_groups = plan_groups.clone();
        } else {
            stage_calibrate_with(&mut rc, Some(mode == 1)).unwrap();
        }
        let rows: Rows = crate::db::stacking::list_artifacts(&fixture.conn, &rc.subject, Some("calibrated"))
            .unwrap()
            .into_iter()
            .map(|r| (r.group_key, r.frame_id, r.config_hash, r.size))
            .collect();
        let progress: Vec<(u64, u64)> = rec
            .events(STACKING_PROGRESS_EVENT)
            .iter()
            .filter(|e| e["stage"] == Stage::Calibrate.as_str())
            .map(|e| (e["current"].as_u64().unwrap(), e["total"].as_u64().unwrap()))
            .collect();
        let mut files = Vec::new();
        for g in &plan_groups {
            let dir = layout.calibrated_dir();
            let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            names.sort();
            for n in names {
                files.push((
                    format!("{}/{}", g.key, n.file_name().unwrap().to_string_lossy()),
                    std::fs::read(&n).unwrap(),
                ));
            }
        }
        let mut cached: Vec<(i64, bool)> = rc.cached_calibrated.iter().map(|(k, v)| (*k, *v)).collect();
        cached.sort();
        (
            files,
            rc.warnings.clone(),
            rc.runtime_exclusions.clone(),
            cached,
            rows,
            progress,
        )
    };
    let per_group = run(0);
    let merged = run(1);
    let baseline = run(2);
    assert!(!per_group.0.is_empty(), "the fixture calibrates something");
    assert_eq!(per_group.0.len(), merged.0.len());
    assert_eq!(per_group.1, merged.1, "warnings in the same order");
    assert_eq!(per_group.2, merged.2, "exclusions");
    assert_eq!(per_group.3, merged.3, "cache flags");
    // The artifact rows, in commit order (ORDER BY id): group, then frame, same hash and size.
    assert!(!per_group.4.is_empty(), "the fixture writes calibrated rows");
    assert_eq!(per_group.4, merged.4, "calibrated rows and their commit order");
    // The pre-L4 order — one stage run per group — for the lists and the rows.
    assert_eq!(baseline.4, merged.4, "rows against the pre-L4 group-by-group order");
    assert_eq!(baseline.1, merged.1, "warnings against the pre-L4 interleaving");
    assert_eq!(baseline.2, merged.2, "exclusions against the pre-L4 interleaving");
    assert_eq!(baseline.0.len(), merged.0.len());
    // Review I1: the stage-wide progress never steps back and lands on its total, in both shapes.
    for (name, shot) in [("per-group", &per_group), ("merged", &merged)] {
        assert!(!shot.5.is_empty(), "{name}: no progress events");
        assert!(
            shot.5.windows(2).all(|w| w[0].0 <= w[1].0),
            "{name}: calibrate progress went backwards: {:?}",
            shot.5
        );
        let (last, total) = *shot.5.last().unwrap();
        assert_eq!(last, total, "{name}: the last event lands on total: {:?}", shot.5);
    }
    for (a, b) in per_group.0.iter().zip(&merged.0) {
        assert_eq!(a.0, b.0);
        assert!(a.1 == b.1, "{} differs between the two queue shapes", a.0);
    }
}

/// Cancels the run on the first Calibrate tick that carries a frame (a worker finishing inside the queue).
struct CancelOnCalibrateTick {
    inner: Recording,
    cancel: Arc<AtomicBool>,
}

impl ProgressEmitter for CancelOnCalibrateTick {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if event_name == STACKING_PROGRESS_EVENT
            && payload["stage"] == Stage::Calibrate.as_str()
            && payload["current"].as_u64().unwrap_or(0) > 0
        {
            self.cancel.store(true, Ordering::SeqCst);
        }
        self.inner.emit_json(event_name, payload);
    }
}

/// Task 13c final review A, M1: a cancel during the merged Calibrate queue still replays every stashed phase-1
/// list (group 0's warnings and exclusions reach `rc.warnings` / `rc.runtime_exclusions`).
#[test]
fn calibrate_cancel_in_merged_queue_replays_phase1_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut ctx_raw = ServiceContext::new_for_tests(db_path.clone());
    ctx_raw.image_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let ctx = Arc::new(ctx_raw);
    let (fixture, _fine, _coarse, _tiny, working, output) = seed_two_geometry_groups(&db_path);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let w = tempfile::tempdir().unwrap();
    let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
    let cancel = Arc::new(AtomicBool::new(false));
    let emitter = Arc::new(CancelOnCalibrateTick {
        inner: Recording::new(),
        cancel: cancel.clone(),
    });
    let mut rc = test_context(
        ctx.clone(),
        emitter as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    rc.cancel = cancel;
    rc.rerun_from = Some(Stage::Calibrate);
    // The fixture's per-group hot-pixel warnings are stashed per group before the merged queue runs.
    let r = stage_calibrate_with(&mut rc, Some(true));
    assert!(matches!(r, Err(RunError::Cancelled)), "{r:?}");
    let w2 = tempfile::tempdir().unwrap();
    let mut uncancelled = test_context(
        ctx.clone(),
        Arc::new(Recording::new()) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        WorkingLayout::new(w2.path(), &set_slug(SET_NAME)),
        output.path().to_path_buf(),
        seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        )
        .1,
    );
    uncancelled.rerun_from = Some(Stage::Calibrate);
    stage_calibrate_with(&mut uncancelled, Some(true)).unwrap();
    // Every warning the full merged run raised is present after the cancel (phase-1 ones replayed, commit-time
    // ones are a subset of the full run's).
    for w in &rc.warnings {
        assert!(uncancelled.warnings.contains(w), "{w}");
    }
    assert!(
        !rc.warnings.is_empty(),
        "the stashed lists were dropped by the cancel (the fixture raises a hot-pixel warning per group)"
    );
    assert_eq!(rc.warnings, uncancelled.warnings, "every stashed warning, in group order");
}

/// Rewrites `compute.memory_budget_mb` the first time a progress event of `stage` names `group_key`: a settings
/// change landing between two groups' fan-outs (the first event of a group's own fan-out comes after that
/// group's admission line).
struct BudgetSwitch {
    ctx: Arc<ServiceContext>,
    stage: Stage,
    group_key: String,
    to_mb: u64,
    done: AtomicBool,
}

impl ProgressEmitter for BudgetSwitch {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if event_name != STACKING_PROGRESS_EVENT
            || payload["stage"] != self.stage.as_str()
            || payload["groupKey"] != self.group_key.as_str()
            || self.done.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let conn = self.ctx.db.get().unwrap().conn();
        self.ctx
            .settings
            .persist_setting(
                &conn,
                crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB,
                &self.to_mb.to_string(),
            )
            .unwrap();
    }
}

/// The `memory_budget_bytes` of every `fan-out admitted` line of `stage` naming `group_key`, in log order.
fn admitted_budgets(text: &str, stage: Stage, group_key: &str) -> Vec<u64> {
    text.lines()
        .filter(|l| {
            l.contains("fan-out admitted")
                && l.contains(&format!("stage=\"{}\"", stage.as_str()))
                && l.contains(group_key)
        })
        .map(|l| {
            let rest = l.split("memory_budget_bytes=").nth(1).expect("budget field");
            rest.split_whitespace().next().unwrap().parse().unwrap()
        })
        .collect()
}

/// Task 13c L4 review I2: the memory budget is read right before EACH fan-out, so a settings change between
/// two groups' fan-outs is seen by the second group (Calibrate, per-group queues).
#[test]
fn calibrate_reads_the_budget_before_each_groups_fan_out() {
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
    let set_budget = |mb: u64| {
        let conn = ctx.db.get().unwrap().conn();
        ctx.settings
            .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, &mb.to_string())
            .unwrap();
    };
    set_budget(2048);
    let w = tempfile::tempdir().unwrap();
    let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
    let switch = Arc::new(BudgetSwitch {
        ctx: ctx.clone(),
        stage: Stage::Calibrate,
        group_key: plan_groups[0].key.clone(),
        to_mb: 1024,
        done: AtomicBool::new(false),
    });
    let mut rc = test_context(
        ctx.clone(),
        switch.clone() as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    rc.rerun_from = Some(Stage::Calibrate);
    let (res, text) =
        crate::stacking::telemetry::capture_logs(|| stage_calibrate_with(&mut rc, Some(false)));
    res.unwrap();
    assert!(switch.done.load(Ordering::SeqCst), "the budget was switched mid-stage");
    let first = admitted_budgets(&text, Stage::Calibrate, &plan_groups[0].key);
    assert_eq!(first, vec![2048u64 << 20], "the first group ran on the old budget: {text}");
    for g in &plan_groups[1..] {
        let later = admitted_budgets(&text, Stage::Calibrate, &g.key);
        assert!(!later.is_empty(), "no admission line for {}: {text}", g.key);
        assert!(
            later.iter().all(|&b| b == 1024u64 << 20),
            "{} did not see the new budget: {later:?}",
            g.key
        );
    }
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
        for kind in ["metrics", "measured"] {
            for r in crate::db::stacking::list_artifacts(&fixture.conn, &rc.subject, Some(kind)).unwrap() {
                // A first-pass artifact's bytes carry its wall-clock timings: its size differs run to run.
                let size = if kind == "measured" { None } else { r.size };
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

/// Task 13c S7: a target dropped mid-run retires workers before their next pull; the results are those
/// of admission 1 and of admission N, never more than `start` workers ran, and the run finished.
#[test]
fn fan_out_gate_dropped_mid_run_is_identical_and_bounded() {
    const N: usize = 64;
    let work = |i: usize| -> Result<u64, String> {
        std::thread::sleep(Duration::from_millis(2));
        Ok((i as u64).wrapping_mul(2_654_435_761).rotate_left(7) ^ 0xA5)
    };
    let run = |gate: &AdmissionGate, drop_to: Option<(usize, usize)>| {
        let done = AtomicUsize::new(0);
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let cancel = AtomicBool::new(false);
        let out = fan_out(
            (0..N).collect::<Vec<usize>>(),
            gate,
            gate_label("t"),
            &cancel,
            |i| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                let r = work(i);
                running.fetch_sub(1, Ordering::SeqCst);
                if let Some((after, to)) = drop_to {
                    if done.fetch_add(1, Ordering::SeqCst) + 1 == after {
                        gate.set_target(to);
                    }
                }
                r
            },
        );
        (out, peak.load(Ordering::SeqCst))
    };
    let one = run(&AdmissionGate::fixed(1), None);
    let many = run(&AdmissionGate::fixed(N), None);
    let gate = AdmissionGate::fixed(8);
    let (dropped, peak) = run(&gate, Some((10, 2)));
    assert_eq!(dropped.len(), N);
    assert!(dropped.iter().all(|r| r.is_some()), "every item ran");
    assert_eq!(dropped, one.0, "identical to admission 1");
    assert_eq!(dropped, many.0, "identical to admission N");
    assert!(peak <= 8, "never more than start workers: {peak}");
    assert_eq!(gate.target(), 2);
    assert!(gate.retired.load(Ordering::SeqCst) >= 1, "workers retired");
    // The target is clamped to [1, start].
    gate.set_target(0);
    assert_eq!(gate.target(), 1);
    gate.set_target(99);
    assert_eq!(gate.target(), 8);
    // Dropped all the way to 1 mid-run still finishes with one worker.
    let g1 = AdmissionGate::fixed(6);
    let (to_one, _) = run(&g1, Some((3, 1)));
    assert_eq!(to_one, one.0);
}

/// The `fan-out finished` line carries every fixed field; an `admission lowered` source is covered by
/// the live-budget test below.
#[test]
fn fan_out_finished_line_has_its_fields() {
    let cancel = AtomicBool::new(false);
    let gate = AdmissionGate::fixed(3);
    let (_, text) = crate::stacking::telemetry::capture_logs(|| {
        fan_out(
            vec![1u64, 2, 3, 4, 5, 6],
            &gate,
            gate_label("pass1"),
            &cancel,
            |i| {
                std::thread::sleep(Duration::from_millis(5));
                Ok::<u64, String>(i)
            },
        )
    });
    let line = text
        .lines()
        .find(|l| l.contains("fan-out finished"))
        .unwrap_or_else(|| panic!("no fan-out finished line: {text}"));
    for f in [
        "run_id=77",
        "stage=\"measure\"",
        "group_key=g-test",
        "pass=\"pass1\"",
        "items=6",
        "admission=3",
        "workers=3",
        "retired=0",
        "duration_ms=",
        "tail_ms=",
        "idle_worker_ms=",
    ] {
        assert!(line.contains(f), "{f} missing in {line}");
    }
    assert_eq!(text.matches("fan-out finished").count(), 1);
}

/// The production gate lowers its target from the live memory budget (one `warn` for an unreadable read is
/// not asserted here; this pins the lowering and its log line).
#[test]
fn fan_out_gate_lowers_from_the_live_memory_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = Arc::new(ServiceContext::new_for_tests(tmp.path().join("t.db")));
    let conn = ctx.db.get().unwrap().conn();
    // 1 MiB budget against a 64 MiB working set: admission 1.
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "1")
        .unwrap();
    drop(conn);
    let gate = AdmissionGate::budgeted_on(&ctx, 64 << 20, 0, 4);
    // The rate limit measures from construction: arm it.
    if let Some(src) = &gate.source {
        *src.last_check.lock().unwrap() = Instant::now() - Duration::from_secs(5);
    }
    let mut label = gate_label("t");
    label.run_id = 78;
    label.group_key = Some("g-live".to_string());
    let ((), text) = crate::stacking::telemetry::capture_logs(|| gate.refresh(&label));
    assert_eq!(gate.target.load(Ordering::SeqCst), 1);
    let line = text
        .lines()
        .find(|l| l.contains("fan-out admission lowered"))
        .unwrap_or_else(|| panic!("no lowered line: {text}"));
    for f in [
        "run_id=78",
        "group_key=g-live",
        "from=4",
        "to=1",
        "budget_why=\"setting",
    ] {
        assert!(line.contains(f), "{f} missing in {line}");
    }
    // Rate-limited: an immediate second refresh does nothing, and a raised budget never raises the target.
    gate.refresh(&label);
    assert_eq!(gate.target.load(Ordering::SeqCst), 1);
    // A raised budget never raises the target: persist a roomy value, let the rate limit pass, refresh.
    let conn = ctx.db.get().unwrap().conn();
    ctx.settings
        .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, "4096")
        .unwrap();
    drop(conn);
    if let Some(src) = &gate.source {
        *src.last_check.lock().unwrap() = Instant::now() - Duration::from_secs(5);
    }
    let ((), text2) = crate::stacking::telemetry::capture_logs(|| gate.refresh(&label));
    assert_eq!(gate.target.load(Ordering::SeqCst), 1, "{text2}");
    assert!(!text2.contains("fan-out admission lowered"), "{text2}");
}

/// v0.6.3: the fan-out stages' per-frame ticker. Two workers race 250
/// ticks through one ticker: the recorded percents never go backwards
/// (the gate is held across the emit), the first tick is emitted (the
/// gate is armed at construction), the count continues from
/// `done_so_far` instead of restarting, and the tick that lands on
/// `total` is always emitted whatever the throttle says.
#[test]
fn fan_out_ticker_is_monotonic_continues_and_always_lands_on_total() {
    struct Rec {
        events: Mutex<Vec<(usize, f64)>>,
    }
    impl ProgressEmitter for Rec {
        fn emit_json(&self, _event_name: &str, payload: serde_json::Value) {
            let current = payload["current"].as_u64().unwrap() as usize;
            let percent = payload["percent"].as_f64().unwrap();
            self.events.lock().unwrap().push((current, percent));
        }
    }
    const DONE_SO_FAR: usize = 50;
    const TICKS: usize = 250;
    const TOTAL: usize = DONE_SO_FAR + TICKS;

    let rec = Arc::new(Rec {
        events: Mutex::new(Vec::new()),
    });
    let ticker = FanOutTicker::from_parts(
        rec.clone(),
        1,
        &StackSubject::FrameSet(1),
        Stage::Measure,
        Some("g".to_string()),
        DONE_SO_FAR,
        TOTAL,
        0,
        0,
    );
    let counter = AtomicUsize::new(0);
    let worker = || loop {
        if counter.fetch_add(1, Ordering::SeqCst) >= TICKS {
            break;
        }
        ticker.tick(Some(7));
    };
    std::thread::scope(|scope| {
        scope.spawn(worker);
        scope.spawn(worker);
    });

    assert_eq!(ticker.done(), TOTAL);
    let events = rec.events.lock().unwrap().clone();
    assert!(!events.is_empty(), "no ticks recorded at all");
    // Two workers: the one whose `fetch_add` returned 52 can take the gate
    // before the one holding 51, which then drops its stale tick — so the
    // first emitted tick is 51 or 52, never a restart from 0.
    assert!(
        (DONE_SO_FAR + 1..=DONE_SO_FAR + 2).contains(&events[0].0),
        "the first tick continues from done_so_far and is emitted: {events:?}"
    );
    let mut last = -1.0f64;
    for &(_, percent) in &events {
        assert!(
            percent + 1e-9 >= last,
            "percent went backwards: {last} -> {percent}"
        );
        last = percent;
    }
    let final_event = events.last().unwrap();
    assert_eq!(
        final_event.0, TOTAL,
        "the tick on total is always emitted: {events:?}"
    );
    assert!((final_event.1 - 100.0).abs() < 1e-9, "{events:?}");
}

#[test]
fn emit_integrate_ticks_never_go_backwards_under_free_racing() {
    // Fix round 2, brief's own test ask: TWO THREADS RACE FREELY — no
    // external turn-taking, genuine concurrent calls — each pulling its
    // next `(plane, frac)` from a shared, monotonically increasing
    // counter immediately before calling the REAL `emit_integrate_tick`.
    // This is the exact scenario that was flaky before fix round 2 (see
    // `emit_integrate_ticks_never_race_percent_backwards`'s own doc
    // comment) — a value fetched early by one thread could sit
    // unsubmitted while the other thread ran far ahead, and once
    // finally submitted, land AFTER a much higher percent already on
    // the wire. The max-latch fixes this not by reordering (impossible
    // without a queue) but by DROPPING the stale tick outright: the
    // recorded sequence can be a strict SUBSET of everything fetched,
    // but whatever subset does get through must never go backwards.
    // Run 20 times (fresh state each iteration) per the brief, each
    // iteration racing >= 200 values across the two threads.
    struct ThreadPercentRecorder {
        events: Mutex<Vec<f64>>,
    }
    impl ProgressEmitter for ThreadPercentRecorder {
        fn emit_json(&self, _event_name: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push(payload["percent"].as_f64().unwrap_or(f64::NAN));
        }
    }

    const ITERATIONS: usize = 20;
    const TOTAL: u64 = 250;
    const CHANNELS: usize = 1;

    for iteration in 0..ITERATIONS {
        let tick_state: Mutex<IntegrateTickState> = Mutex::new(IntegrateTickState::new());
        let recorder = ThreadPercentRecorder {
            events: Mutex::new(Vec::new()),
        };
        let counter = std::sync::atomic::AtomicU64::new(0);

        let worker = || loop {
            let v = counter.fetch_add(1, Ordering::SeqCst);
            if v >= TOTAL {
                break;
            }
            emit_integrate_tick(
                &tick_state,
                &recorder,
                1,
                &StackSubject::FrameSet(1),
                "g",
                CHANNELS,
                0,
                v as f64 / TOTAL as f64,
                v,
                TOTAL,
                true, // force: bypass the throttle so every fetched
                // value reaches the latch — the latch, not the
                // throttle, is what this test exercises.
                None,
            );
        };

        std::thread::scope(|scope| {
            scope.spawn(worker);
            scope.spawn(worker);
        });

        let events = recorder.events.into_inner().unwrap();
        assert!(
            !events.is_empty(),
            "iteration {iteration}: no ticks recorded at all"
        );
        let mut last = -1.0f64;
        for &percent in &events {
            assert!(
                percent + 1e-9 >= last,
                "iteration {iteration}: percent went backwards: {last} -> {percent}"
            );
            last = percent;
        }
    }
}

#[test]
fn full_run_writes_a_master_with_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // The default config on purpose: rejection normalization is `local`
    // since OD-1a, so this run takes the Normalize stage's seeds
    // calibration and per-frame fan-out, and `stage_sequence_and_monotonic`
    // below covers the default path's progress (pcores review r1, I3).
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
    .expect("start should succeed");

    wait_for_run(&ctx, started.run_id);

    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    assert_eq!(group.status, "done", "{group:?}");
    let master_path = group.master_path.clone().expect("master path recorded");
    assert!(
        Path::new(&master_path).exists(),
        "master file missing: {master_path}"
    );

    let stats: GroupStats =
        serde_json::from_str(group.stats_json.as_deref().expect("stats_json set")).unwrap();
    assert_eq!(stats.frames, 4, "{stats:?}");

    // M4d Task 3 (ruling R-M4d-4), fix round 1 m2: a run WITHOUT drizzle
    // catalogs exactly one output — the master. The drizzle + weight-map
    // case has its own test; this is the other half of the rule, that
    // stage 9 records what it WROTE and nothing it did not.
    let master_lights =
        crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
    let kinds: Vec<&str> = master_lights.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, vec!["master"], "{master_lights:?}");
    assert_eq!(master_lights[0].path, master_path);
    assert_eq!(master_lights[0].frames, group.included_count);

    let header = FitsHeader::from_path(Path::new(&master_path)).unwrap();
    assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Light"));
    assert_eq!(header.get_i32("NCOMBINE"), Some(4));
    assert_eq!(
        header.get_str("ATH_STKI").as_deref(),
        Some(started.run_id.to_string().as_str())
    );
    assert_eq!(
        header.get_str("ATH_STKG").as_deref(),
        Some(group.group_key.as_str())
    );
    // Fix round 1, item 3: `ATH_STKF` names the SOURCE frame (the
    // catalog `files.filename` stem, "f<i>" for `seed_star_group`'s
    // fixture lights) — never the calibrated artifact's own on-disk
    // name (which would need `c_`/`_d` stripping the source name never
    // does).
    //
    // Ruling R-M3-17 v2: `ATH_STKF` names the group's NORMALIZATION
    // anchor (`members[reference_idx]`, sky-penalized), which since
    // this ruling is no longer guaranteed to be `run_row.
    // reference_frame_id` (the REGISTRATION reference, auto-picked by
    // weight over the whole set). This assertion only still holds
    // because `seed_star_group`'s fixture gives every frame the SAME
    // configured background (600.0) — `s_i = w_i / sqrt(bg_i)` is then
    // monotonic in weight, so the sky-penalized anchor coincides with
    // the auto/best-by-weight registration reference by construction,
    // not because the two are the same thing in general (see the
    // dedicated `normalization_anchor_prefers_the_dark_night_over_the_
    // pinned_bright_reference` test below for a fixture where they
    // deliberately differ).
    let reference_frame_id = run_row
        .reference_frame_id
        .expect("reference frame recorded");
    let reference_index = light_ids
        .iter()
        .position(|&id| id == reference_frame_id)
        .expect("reference frame is one of the fixture's own lights");
    let expected_stem = format!("f{reference_index}");
    assert_eq!(
        header.get_str("ATH_STKF").as_deref(),
        Some(expected_stem.as_str()),
        "ATH_STKF must be the fixture light's own stem"
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let run_json_path = layout.run_json(started.run_id);
    let run_json_text = std::fs::read_to_string(&run_json_path).unwrap();
    let _summary: RunSummary =
        serde_json::from_str(&run_json_text).expect("runs/run-<id>.json parses");
    let summary_json = run_row.summary_json.clone().expect("summary_json stored");
    assert_eq!(
        run_json_text, summary_json,
        "runs/run-<id>.json and the DB's summary_json must be the SAME document"
    );

    let events = recorder.events(STACKING_PROGRESS_EVENT);
    let order = stage_sequence_and_monotonic(&events);
    assert_eq!(order, CANONICAL_STAGE_ORDER, "{order:?}");

    // I3 (pcores review r1): the Normalize row of the default path is ONE
    // count - the seeds calibration's frames, then the per-frame fan-out
    // continuing from there, to a last tick on the same total. The
    // calibration message must be among the events, or the monotonic
    // check above says nothing about it.
    let normalize: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["stage"].as_str() == Some("normalize"))
        .collect();
    assert!(
        normalize.iter().any(|e| e["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("calibrating the seeds path"))),
        "the default run takes the seeds calibration: {normalize:?}"
    );
    let climbing_totals: std::collections::BTreeSet<u64> = normalize
        .iter()
        .filter(|e| e["percent"].as_f64().unwrap_or(0.0) > 0.0)
        .map(|e| e["total"].as_u64().unwrap())
        .collect();
    assert_eq!(
        climbing_totals.len(),
        1,
        "one count from the first tick to the last: {normalize:?}"
    );
    let last = normalize.last().unwrap();
    assert_eq!(last["current"], last["total"], "{last:?}");

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["success"].as_bool(), Some(true));
    let masters = completes[0]["masters"].as_array().unwrap();
    assert_eq!(masters.len(), 1, "{masters:?}");
    assert_eq!(
        masters[0]["groupKey"].as_str(),
        Some(group.group_key.as_str())
    );
}

// ── Stage 0.5: the run builds/rebuilds its own masters ────────────────

/// Owner requirement 2026-09-09 ("the pipeline should build the
/// calibration masters itself when they are missing"): the Task 6/7
/// fixture's TWO master files deleted from disk — provenance rows and
/// their real raw source sub-frames intact, via `add_master_dark_and_flat`'s
/// real `register_master` registration (Task 8) — `start_stacking`
/// rebuilds both inside stage 0.5, BEFORE calibrate, and the run still
/// finishes `done`.
#[test]
fn deleted_masters_are_rebuilt_by_stage_masters() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    let (dark_set, flat_set) = test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let master_path = |set_id: i64| -> String {
        fixture
            .conn
            .query_row(
                "SELECT fi.path FROM calibration_set_frames csf
                     JOIN frames fr ON fr.id = csf.frame_id
                     JOIN files fi ON fi.id = fr.file_id
                     WHERE csf.set_id = ?1",
                [set_id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let dark_path = master_path(dark_set);
    let flat_path = master_path(flat_set);
    // Before: both master files exist (the fixture just wrote them).
    assert!(
        Path::new(&dark_path).exists(),
        "fixture sanity: dark master written"
    );
    assert!(
        Path::new(&flat_path).exists(),
        "fixture sanity: flat master written"
    );
    std::fs::remove_file(&dark_path).unwrap();
    std::fs::remove_file(&flat_path).unwrap();
    assert!(!Path::new(&dark_path).exists());
    assert!(!Path::new(&flat_path).exists());

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
    .expect("start should succeed");

    wait_for_run(&ctx, started.run_id);

    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");

    // After: both masters rebuilt AT THEIR ORIGINAL LIBRARY PATHS.
    assert!(
        Path::new(&dark_path).exists(),
        "dark master must be rebuilt at its original path"
    );
    assert!(
        Path::new(&flat_path).exists(),
        "flat master must be rebuilt at its original path"
    );

    // Progress: stage `masters` reports 2/2 before `calibrate` ever ticks.
    let events = recorder.events(STACKING_PROGRESS_EVENT);
    let masters_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["stage"].as_str() == Some("masters"))
        .map(|(i, _)| i)
        .collect();
    assert!(!masters_indices.is_empty(), "no masters progress events");
    let last_masters = &events[*masters_indices.last().unwrap()];
    assert_eq!(
        last_masters["current"].as_u64(),
        Some(2),
        "{last_masters:?}"
    );
    assert_eq!(last_masters["total"].as_u64(), Some(2), "{last_masters:?}");
    let first_calibrate_index = events
        .iter()
        .position(|e| e["stage"].as_str() == Some("calibrate"))
        .expect("a calibrate progress event exists");
    assert!(
        *masters_indices.last().unwrap() < first_calibrate_index,
        "masters progress must finish before calibrate starts: masters at {masters_indices:?}, calibrate first at {first_calibrate_index}"
    );

    // The summary lists two masters_built, one per rebuilt master.
    let summary: RunSummary = serde_json::from_str(
        run_row
            .summary_json
            .as_deref()
            .expect("summary_json stored"),
    )
    .unwrap();
    assert_eq!(
        summary.masters_built.len(),
        2,
        "{:?}",
        summary.masters_built
    );
    let rebuilt_master_set_ids: std::collections::HashSet<i64> = summary
        .masters_built
        .iter()
        .map(|m| m.master_set_id)
        .collect();
    assert_eq!(
        rebuilt_master_set_ids,
        std::collections::HashSet::from([dark_set, flat_set]),
        "{:?}",
        summary.masters_built
    );
    for m in &summary.masters_built {
        assert_eq!(m.kind, MasterWork::Rebuild, "{m:?}");
        assert!(Path::new(&m.path).exists(), "{m:?}");
    }
}

#[test]
fn rerun_reuses_everything_and_only_integrates() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let started1 = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("first run should succeed");
    wait_for_run(&ctx, started1.run_id);
    let row1 = crate::db::stacking::get_run(&fixture.conn, started1.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row1.status, "done", "{row1:?}");
    let groups1 = crate::db::stacking::list_groups(&fixture.conn, started1.run_id).unwrap();
    let master_path1 = groups1[0].master_path.clone().expect("first master path");

    let started2 = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("second run should succeed");
    wait_for_run(&ctx, started2.run_id);
    let row2 = crate::db::stacking::get_run(&fixture.conn, started2.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row2.status, "done", "{row2:?}");

    let summary2: RunSummary =
        serde_json::from_str(row2.summary_json.as_deref().unwrap()).unwrap();
    let mut any_frame_checked = false;
    for group in &summary2.groups {
        for f in &group.frames {
            if f.included {
                any_frame_checked = true;
                assert!(f.cached_calibrated, "{f:?}");
                assert!(f.cached_metrics, "{f:?}");
                assert!(f.cached_registration, "{f:?}");
            }
        }
    }
    assert!(
        any_frame_checked,
        "no included frame in the second run's summary"
    );

    let groups2 = crate::db::stacking::list_groups(&fixture.conn, started2.run_id).unwrap();
    let master_path2 = groups2[0].master_path.clone().expect("second master path");
    assert_ne!(
        master_path1, master_path2,
        "a rerun must never overwrite the first master"
    );
    assert!(
        master_path2.ends_with("_2.fits"),
        "expected a collision-suffixed name: {master_path2}"
    );
    assert!(Path::new(&master_path1).exists());
    assert!(Path::new(&master_path2).exists());
}

#[test]
fn cancel_mid_run_keeps_artifacts_writes_no_master() {
    /// Cancels every active stacking run for `frames_set_id` the moment
    /// the FIRST `measure`-stage progress event fires — no need to know
    /// the run id ahead of time (which `start_stacking` has not
    /// returned yet at the point the run thread's own first events can
    /// already be firing): a stacking run's cancel flag lives on its
    /// `StackHandle` in `ctx.active_stacks`, keyed by `frames_set_id`
    /// there too.
    struct CancelOnFirstMeasure {
        ctx: Arc<ServiceContext>,
        frames_set_id: i64,
        fired: std::sync::atomic::AtomicBool,
        recording: Recording,
    }
    impl ProgressEmitter for CancelOnFirstMeasure {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.recording.emit_json(event_name, payload.clone());
            if event_name == STACKING_PROGRESS_EVENT
                && payload["stage"] == "measure"
                && !self.fired.swap(true, Ordering::SeqCst)
            {
                let active = self.ctx.active_stacks.lock().unwrap();
                for h in active.values() {
                    if h.subject == StackSubject::FrameSet(self.frames_set_id) {
                        h.cancel_flag.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }

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

    let emitter = Arc::new(CancelOnFirstMeasure {
        ctx: ctx.clone(),
        frames_set_id: fixture.set_id,
        fired: std::sync::atomic::AtomicBool::new(false),
        recording: Recording::new(),
    });

    let started = start_stacking(
        ctx.clone(),
        emitter.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("start should succeed");

    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "cancelled", "{row:?}");

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert!(
        crate::api::sync::dir_size_bytes(&layout.calibrated_root()) > 0,
        "calibrated artifacts from stage 1 must survive a cancel"
    );

    let output_entries: Vec<_> = std::fs::read_dir(output.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        output_entries.is_empty(),
        "a cancelled run must write no master: {output_entries:?}"
    );

    let completes = emitter.recording.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
    assert_eq!(completes[0]["success"].as_bool(), Some(false));
    assert_eq!(completes[0]["masters"].as_array().map(|v| v.len()), Some(0));
}

#[test]
fn delete_intermediates_cleans_the_working_folder() {
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

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;
    // Perf tier A Task 6a: the registered artifact is now written
    // unconditionally (`registration.write_registered_frames` no
    // longer gates it — see the field's own doc), so `registered_root()`
    // always has content before cleanup runs; the assertion below is no
    // longer at risk of passing trivially over an empty directory.

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert_eq!(
        crate::api::sync::dir_size_bytes(&layout.calibrated_root()),
        0,
        "calibrated frames must be gone after DeleteIntermediates"
    );
    assert_eq!(
        crate::api::sync::dir_size_bytes(&layout.registered_root()),
        0,
        "registered frames must be gone after DeleteIntermediates"
    );

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let master_path = groups[0].master_path.clone().expect("master path");
    assert!(
        Path::new(&master_path).exists(),
        "cleanup must never touch the output folder"
    );

    // v0.6.3: the cleanup keeps the `metrics` rows (file-less, keyed on
    // the calibration CONFIG hash — `paths::INTERMEDIATE_ARTIFACT_KINDS`),
    // so a second run re-calibrates and re-registers but does NOT
    // re-measure. Pinned end to end on the second run's own summary,
    // not on the kind list: a regression in `measurement_hash_for`'s
    // inputs (a calibrated-file identity sneaking into the hash) would
    // make every kept row a miss while the kind list still passed.
    let mut cfg2 = StackingConfig::default();
    cfg2.output.cleanup = CleanupPolicy::DeleteIntermediates;
    let second = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg2),
        None,
    )
    .expect("second start should succeed");
    wait_for_run(&ctx, second.run_id);
    let row2 = crate::db::stacking::get_run(&fixture.conn, second.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row2.status, "done", "{row2:?}");
    let summary2: RunSummary =
        serde_json::from_str(row2.summary_json.as_deref().unwrap()).unwrap();
    let frames2: Vec<&crate::stacking::provenance::SummaryFrame> = summary2
        .groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .collect();
    assert!(!frames2.is_empty());
    for f in &frames2 {
        assert!(
            !f.cached_calibrated,
            "the cleanup removed every calibrated frame, so run 2 must regenerate: {f:?}"
        );
        assert!(
            f.cached_metrics,
            "the kept metrics row must be a cache hit for the regenerated frame: {f:?}"
        );
    }
    let _ = output;
}

#[test]
fn maps_written_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.integration.write_rejection_maps = true;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let group = &groups[0];
    let low = group.rejection_low_path.clone().expect("_rejlow path set");
    let high = group
        .rejection_high_path
        .clone()
        .expect("_rejhigh path set");
    assert!(low.ends_with("_rejlow.fits"), "{low}");
    assert!(high.ends_with("_rejhigh.fits"), "{high}");
    assert!(Path::new(&low).exists(), "{low}");
    assert!(Path::new(&high).exists(), "{high}");
}

// ── Stage 6 (Task 5): local normalization ───────────────────────────

/// 2026-10-06 (L-24): on this 192x144 field the corner stars' [6F, 8F]
/// annuli are under half present, so F1 keeps 17 pairs (< MIN_MATCHES 20)
/// and every member would be excluded; the run tests built on it pin
/// `PsfSignal` because they test estimator-agnostic run plumbing. The
/// default F1 path is covered by
/// `the_default_f1_path_runs_end_to_end_and_the_plan_reads_it_cached`.
///
/// Dense enough (24 stars, comfortably over `ln::scale::MIN_MATCHES`'s 20)
/// for `relative_scale` to succeed — [`BASE_STARS`]' own 10-star field is
/// too sparse for local normalization's own tests, which need a match
/// count, not just a detection count. Same canvas
/// ([`STAR_FIELD_WIDTH`]x[`STAR_FIELD_HEIGHT`]) and spacing philosophy as
/// [`BASE_STARS`] (comfortably wider than the detector's own blob radius
/// at this sigma/amplitude); jittered off a 6x4 grid (`SplitMix64`, same
/// technique `ln::scale`'s own tests use) rather than a perfectly regular
/// one — a plain grid's repeated distances/angles are exactly what makes
/// registration's quad-based star matching ambiguous ("quad seed
/// failed" with thousands of candidate quads, observed empirically on
/// the first, unjittered version of this fixture).
fn ln_base_stars() -> Vec<(f64, f64, f64)> {
    let mut rng = crate::geometry::ransac::SplitMix64(7);
    let mut stars = Vec::with_capacity(24);
    for row in 0..4 {
        for col in 0..6 {
            let x = 16.0 + col as f64 * 32.0 + (rng.next_f64() - 0.5) * 10.0;
            let y = 18.0 + row as f64 * 36.0 + (rng.next_f64() - 0.5) * 10.0;
            let amp = if (row + col) % 2 == 0 { 9000.0 } else { 6500.0 };
            stars.push((x, y, amp));
        }
    }
    stars
}

fn ln_shifted_stars(dx: f64, dy: f64) -> Vec<(f64, f64, f64)> {
    ln_base_stars()
        .iter()
        .map(|&(x, y, a)| (x + dx, y + dy, a))
        .collect()
}

/// Same shape as [`seed_star_group`], but [`ln_base_stars`]' denser field
/// instead of [`BASE_STARS`] — Task 5's local-normalization tests need
/// `relative_scale` to actually succeed (≥ `ln::scale::MIN_MATCHES`
/// matched star pairs), not just a viable group.
fn seed_ln_star_group(
    db_path: &Path,
    set_name: &str,
    shifts: &[(f64, f64)],
    noise_sigmas: &[f32],
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    assert_eq!(shifts.len(), noise_sigmas.len());
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, (&(dx, dy), &sigma)) in shifts.iter().zip(noise_sigmas.iter()).enumerate() {
        let stars = ln_shifted_stars(dx, dy);
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = star_light_spec(&stem, &date_obs);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            600.0,
            sigma,
            100 + i as u64,
        );
        light_ids.push(id);
    }

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

    (fixture, light_ids, working, output)
}

/// The four jittered-grid shifts the local-normalization tests stack.
const LN_SHIFTS: [(f64, f64); 4] = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];

/// The config of the local-normalization runs: LN on over the dense LN star field. P-13: the subject is the LN
/// run, not the star selection (synthetic stars, none saturated), so the selection is OFF. 2026-10-06: pinned to
/// PsfSignal, see `ln_base_stars` (L-24).
fn ln_run_config() -> StackingConfig {
    let mut cfg = StackingConfig::default();
    cfg.normalization.local.enabled = true;
    cfg.normalization.local.flux_estimator =
        crate::stacking::integrate::LnFluxEstimator::PsfSignal;
    cfg.normalization.local.star_selection = crate::stacking::integrate::LnStarSelection::OFF;
    cfg
}

/// [`ln_run_config`] for a test that reads the plan against the stored config: OFF is not storable (it
/// serializes as null), so it is replaced by the finite selection that cuts nothing (P-13, synthetic stars).
fn ln_stored_config() -> StackingConfig {
    let mut cfg = ln_run_config();
    cfg.normalization.local.star_selection = crate::stacking::integrate::LnStarSelection {
        min_peak_snr: 5.0,
        max_stars: 5000,
        saturation: 1.0,
    };
    cfg
}

/// Stages 1 to 5, then Normalize and Output.
fn run_register_and_output(rc: &mut RunContext) {
    run_stages_for_test(rc, Stage::Register).unwrap();
    stage_output(rc).unwrap();
}

/// Bit-for-bit pixel equality of two masters (headers legitimately differ: NORMALIZATION card, run id,
/// timestamps); `what` names the first master in a failure.
fn assert_masters_equal_bit_for_bit(master_a: &str, master_b: &str, what: &str) {
    let reader_a = PlaneReader::open(Path::new(master_a)).unwrap();
    let reader_b = PlaneReader::open(Path::new(master_b)).unwrap();
    assert_eq!(reader_a.channels(), reader_b.channels());
    for p in 0..reader_a.channels() {
        let plane_a = reader_a.read_plane(p).unwrap();
        let plane_b = reader_b.read_plane(p).unwrap();
        assert_eq!(plane_a.len(), plane_b.len());
        for (i, (a, b)) in plane_a.iter().zip(plane_b.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "plane {p} pixel {i}: {what} {a} vs global-normalization {b}"
            );
        }
    }
}

type SeedGroup = fn(
    &Path,
    &str,
    &[(f64, f64)],
    &[f32],
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
);

/// One star-field frame set with its master dark and flat, the working and output folders, the layout and the
/// context every run of the local-normalization tests starts from.
///
/// Fields drop in the order written. The catalog's folder (`_tmp`) goes last, after the context and the
/// fixture's connection have closed `catalog.db`: a folder with an open database in it cannot be removed on
/// Windows, and a `TempDir` that fails to remove itself leaves the folder behind without a word.
struct LnRig {
    layout: WorkingLayout,
    output_dir: PathBuf,
    output: tempfile::TempDir,
    working: tempfile::TempDir,
    light_ids: Vec<i64>,
    fixture: test_fixtures::Fixture,
    ctx: Arc<ServiceContext>,
    _tmp: tempfile::TempDir,
}

impl LnRig {
    /// [`seed_ln_star_group`] on [`LN_SHIFTS`], noise 5.0.
    fn new() -> LnRig {
        LnRig::seeded(seed_ln_star_group, &LN_SHIFTS, &[5.0f32; 4])
    }

    /// A group seeded by `seed` (one of [`seed_ln_star_group`], `seed_star_group`) on `shifts` and `noise`.
    fn seeded(seed: SeedGroup, shifts: &[(f64, f64)], noise: &[f32]) -> LnRig {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, light_ids, working, output) = seed(&db_path, SET_NAME, shifts, noise);
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &light_ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        LnRig::from_parts(tmp, ctx, fixture, light_ids, working, output)
    }

    /// A rig over a fixture the test built itself.
    fn from_parts(
        tmp: tempfile::TempDir,
        ctx: Arc<ServiceContext>,
        fixture: test_fixtures::Fixture,
        light_ids: Vec<i64>,
        working: tempfile::TempDir,
        output: tempfile::TempDir,
    ) -> LnRig {
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let output_dir = output.path().to_path_buf();
        LnRig {
            layout,
            output_dir,
            output,
            working,
            light_ids,
            fixture,
            ctx,
            _tmp: tmp,
        }
    }

    fn plan_groups(&self, cfg: &StackingConfig) -> Vec<IntegrationGroup> {
        group_frames(&self.fixture.conn, self.fixture.set_id, &cfg.grouping).unwrap()
    }

    /// A fresh run row and its context under `cfg`, over `plan_groups`.
    fn context(&self, cfg: StackingConfig, plan_groups: Vec<IntegrationGroup>) -> RunContext {
        let (run_id, group_ids) = seed_run_and_groups(
            &self.fixture.conn,
            self.fixture.set_id,
            &plan_groups,
            self.working.path(),
            self.output.path(),
        );
        test_context(
            self.ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            self.fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            self.layout.clone(),
            self.output_dir.clone(),
            group_ids,
        )
    }

    /// The plan under `cfg`.
    fn plan(&self, cfg: &StackingConfig) -> crate::stacking::plan::StackingPlan {
        build_plan(
            &self.fixture.conn,
            &self.ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(self.fixture.set_id),
            Some(cfg.clone()),
        )
        .expect("the plan should build")
    }

    /// The set's `stacking_artifacts` rows of one kind.
    fn artifacts(&self, kind: &str) -> Vec<crate::db::stacking::StackingArtifactRow> {
        crate::db::stacking::list_artifacts(
            &self.fixture.conn,
            &StackSubject::FrameSet(self.fixture.set_id),
            Some(kind),
        )
        .unwrap()
    }

    /// Frame `i`'s `.athln` sidecar (its stem is `f<i>_f<frame id>`, `frame_file_stem`).
    fn sidecar(&self, group_key: &str, i: usize) -> PathBuf {
        self.layout
            .ln_sidecar_path(group_key, &format!("f{i}_f{}", self.light_ids[i]))
    }

    /// A plain FILE where `run_group_normalization` expects to `create_dir_all` the group's `ln/` directory: the
    /// mkdir fails outright, before any per-frame work, landing in the `Err(RunError::Other(..))` arm
    /// ("continuing with global normalization").
    fn block_ln_dir(&self, group_key: &str) {
        let blocked_ln_dir = self.layout.ln_dir(group_key);
        std::fs::create_dir_all(blocked_ln_dir.parent().unwrap()).unwrap();
        std::fs::write(&blocked_ln_dir, b"not a directory").unwrap();
    }

    /// The comparison run of the rejection-fallback tests: `rejection = scaleZeroOffset`, LN off entirely, in
    /// separate working and output folders (returned first, so that a caller's `let (_dirs, rc) = …` drops
    /// the run context before the folders it wrote into).
    fn run_global_comparison(&self) -> ((tempfile::TempDir, tempfile::TempDir), RunContext) {
        let mut cfg_global = StackingConfig::default();
        cfg_global.normalization.rejection = RejectionNormalization::ScaleZeroOffset;
        cfg_global.normalization.local.enabled = false;

        let working_b = tempfile::tempdir().unwrap();
        let output_b = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &self.fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working_b.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &self.fixture.conn,
            crate::settings::keys::STACKING_OUTPUT_DIR,
            output_b.path().to_str().unwrap(),
        )
        .unwrap();
        let layout_b = WorkingLayout::new(working_b.path(), &set_slug(SET_NAME));
        let plan_groups_b = self.plan_groups(&cfg_global);
        let (run_id_b, group_ids_b) = seed_run_and_groups(
            &self.fixture.conn,
            self.fixture.set_id,
            &plan_groups_b,
            working_b.path(),
            output_b.path(),
        );
        let mut rc_b = test_context(
            self.ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id_b,
            self.fixture.set_id,
            SET_NAME,
            cfg_global,
            plan_groups_b,
            layout_b,
            output_b.path().to_path_buf(),
            group_ids_b,
        );
        run_register_and_output(&mut rc_b);
        ((working_b, output_b), rc_b)
    }
}

/// One local-normalization run on the four-frame LN star fixture (`ln_run_config`, Register then Output), and the
/// four facts four tests used to read off it, one run each: its artifacts, its payload counters, the grids reaching
/// integration and the Normalize stage timing. Each block below carries the old test's name and doc comment.
#[test]
fn a_local_normalization_run_writes_its_sidecars_payloads_grids_and_stage_timing() {
    let rig = LnRig::new();
    let cfg = ln_run_config();
    let plan_groups = rig.plan_groups(&cfg);
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let group_key = plan_groups[0].key.clone();
    let mut rc = rig.context(cfg, plan_groups);
    run_register_and_output(&mut rc);

    // local_normalization_writes_one_sidecar_per_included_frame_and_a_reference
    {
        let reference_path = rig.layout.ln_reference_path(&group_key);
        assert!(reference_path.exists(), "LN reference must be written");

        let group_summary = summary_group(&rc, &group_key);
        assert_eq!(
            group_summary.ln_reference_path.as_deref(),
            Some(reference_path.to_string_lossy().as_ref())
        );

        let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 4, "{:?}", group_summary.frames);

        for i in 0..4 {
            let sidecar = rig.sidecar(&group_key, i);
            assert!(sidecar.exists(), "sidecar for f{i} must exist: {sidecar:?}");
        }

        for f in included {
            assert!(f.ln_scale.is_some(), "{f:?}");
            assert!(!f.cached_ln, "first run must not be cached: {f:?}");
        }

        let ln_artifacts = rig.artifacts("ln");
        assert_eq!(ln_artifacts.len(), 4, "{ln_artifacts:?}");
        let ln_ref_artifacts = rig.artifacts("ln_reference");
        assert_eq!(ln_ref_artifacts.len(), 1, "{ln_ref_artifacts:?}");
    }

    // the_ln_payload_records_the_background_sample_counts
    // Spec 2026-10-04 §10: every `ln` row records the background model's
    // sample counters, and the retired `cells_rejected` is gone.
    {
        let ln_rows = rig.artifacts("ln");
        assert_eq!(ln_rows.len(), 4, "{ln_rows:?}");
        for row in &ln_rows {
            let json = row
                .payload_json
                .as_deref()
                .expect("every ln row carries a payload");
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            assert!(value.get("cells_rejected").is_none(), "{json}");
            let payload: LnArtifactPayload =
                serde_json::from_str(json).expect("the ln payload decodes");
            assert!(payload.samples > 0, "{json}");
            assert!(payload.samples_shared > 0, "{json}");
            assert!(payload.samples_shared <= payload.samples, "{json}");
        }
    }

    // local_normalization_grids_reach_integration_and_ln_frames_matches_included
    // M2 Task 7: `GroupInput.ln` — read back from the sidecars stage 6 just
    // wrote/confirmed fresh — must actually reach `integrate_group`, not
    // just exist as an unused field. `GroupStats.ln_frames` (Task 6) is the
    // signal: it is computed from `input.ln` alone (`integrate.rs`), so if
    // the wiring in `process_group_output` were broken (still passing
    // `None`, or misaligned with `frames`' own order), it would read `0`
    // even though every included frame has a real sidecar.
    {
        let group_summary = summary_group(&rc, &group_key);
        let stats = group_summary
            .stats
            .as_ref()
            .expect("group wrote a master and its stats");
        assert_eq!(stats.included, 4, "{stats:?}");
        assert_eq!(
            stats.ln_frames, stats.included,
            "every included frame must have carried its LN grid into integration: {stats:?}"
        );
    }

    // local_normalization_pushes_a_stage_timing_before_integrate
    // Final fix wave, A1 (ruling R5): with local normalization on,
    // `RunSummary.stages` (here, `rc.timings` — `stage_output` pushes into
    // it directly, same source the run's final summary copies) must carry
    // a `Normalize` entry with a real, non-zero duration, positioned
    // BEFORE `Integrate` (stage order) — the acceptance run measured LN as
    // the single most expensive stage, so it cannot stay invisible.
    {
        let normalize_idx = rc
            .timings
            .iter()
            .position(|t| t.stage == Stage::Normalize)
            .expect("a Normalize StageTiming must be pushed when LN is active");
        let integrate_idx = rc
            .timings
            .iter()
            .position(|t| t.stage == Stage::Integrate)
            .expect("Integrate StageTiming always pushed");
        assert!(
            normalize_idx < integrate_idx,
            "Normalize must precede Integrate in stage order: {:?}",
            rc.timings
        );
        assert!(
            rc.timings[normalize_idx].duration_ms > 0,
            "LN did real work (a reference build + a 4-frame fan-out); its timing must not be zero: {:?}",
            rc.timings[normalize_idx]
        );
    }
}

/// Perf tier C Task 2 (spec §2.2.3): a frame's `fits.<plane>` artifact
/// deleted strictly BETWEEN stage 3 (Measure, which just wrote it) and
/// stage 6 (Normalize, which reads it back) must not fail the run — it
/// falls back to full detection on the warped plane
/// (`scale::relative_scale_against`), the same honest-LN-master
/// contract Task 6a's own registered-artifact fallback (Pin 3, above)
/// established for a missing `registered` artifact.
///
/// Fix round 4 (ruling C-14 item 3, the review's I1): the pin asserts
/// the VICTIM FRAME'S OWN recorded scale source — `LnArtifactPayload::
/// ln_scale_source` on its `ln` row — and that its SIBLINGS, whose
/// artifacts were left alone, record `"seeds"`. The global
/// `ln::fallback_counters` reading this used to rest on could not tell
/// the victim's fallback from any other frame's, and the seeds
/// calibration's own forced-detection arm bumped it too, so
/// `fallbacks >= 1` was true no matter what the victim did. The counter
/// is still checked (it must have fired at least once), but it is no
/// longer the thing the pin rests on.
#[test]
fn local_normalization_falls_back_to_detection_when_a_fits_artifact_is_missing() {
    let rig = LnRig::new();
    let cfg = ln_run_config();
    let plan_groups = rig.plan_groups(&cfg);
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let group_key = plan_groups[0].key.clone();
    // Extracted before `plan_groups` is moved into `rig.context` below.
    let target_frame = plan_groups[0]
        .frames
        .iter()
        .find(|f| f.frame_id == rig.light_ids[0])
        .expect("f0 must be a group member")
        .clone();
    let stem = frame_file_stem(&target_frame);

    let mut rc = rig.context(cfg, plan_groups);

    run_stages_for_test(&mut rc, Stage::Register).unwrap();

    let fits_path = rig.layout.fits_path(&stem, 0);
    assert!(
        fits_path.exists(),
        "f0's fits artifact must exist right after Measure: {fits_path:?}"
    );
    std::fs::remove_file(&fits_path).unwrap();

    let _guard = crate::stacking::ln::fallback_counters::exclusive();
    crate::stacking::ln::fallback_counters::reset();
    stage_output(&mut rc).unwrap();
    let fallbacks = crate::stacking::ln::fallback_counters::FALLBACKS
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        fallbacks >= 1,
        "deleting f0's fits artifact must trigger at least one detection fallback"
    );

    // The pin itself: PER FRAME, from each one's own `ln` artifact
    // payload. f0 (the victim) must record `"detected"`; every sibling,
    // whose `fits` artifacts were never touched, must record `"seeds"`.
    let ln_rows = rig.artifacts("ln");
    assert_eq!(ln_rows.len(), 4, "{ln_rows:?}");
    let mut seen_victim = false;
    for row in &ln_rows {
        let payload: LnArtifactPayload = serde_json::from_str(
            row.payload_json
                .as_deref()
                .expect("every ln row carries a payload"),
        )
        .expect("the ln payload decodes");
        let frame_id = row.frame_id.expect("an ln row is per frame");
        if frame_id == rig.light_ids[0] {
            seen_victim = true;
            assert_eq!(
                payload.ln_scale_source, "detected",
                "the frame whose fits artifact was deleted must record the detection fallback"
            );
        } else {
            assert_eq!(
                payload.ln_scale_source, "seeds",
                "frame {frame_id} kept its fits artifact and must record the seeds path"
            );
        }
    }
    assert!(seen_victim, "f0 must have an ln artifact of its own");

    let group_summary = summary_group(&rc, &group_key);
    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(
        included.len(),
        4,
        "the run must still complete with every frame included: {:?}",
        group_summary.frames
    );
    for f in &included {
        assert!(f.ln_scale.is_some(), "{f:?}");
    }
}

#[test]
fn local_normalization_sidecars_are_cached_on_the_second_run() {
    let rig = LnRig::new();
    let cfg = ln_stored_config();
    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();

    let mut rc1 = rig.context(cfg.clone(), plan_groups.clone());
    run_register_and_output(&mut rc1);

    let mut rc2 = rig.context(cfg.clone(), plan_groups);
    run_stages_for_test(&mut rc2, Stage::Register).unwrap();

    let start = Instant::now();
    stage_output(&mut rc2).unwrap();
    let elapsed = start.elapsed();
    // The wall-clock ceiling is a developer-machine probe: a shared CI
    // runner under the full-workspace load has taken 1.03 s for this
    // fully-cached stage (v0.6.3's release commit, Linux job), which is
    // the runner, not a cache miss — the `cached_ln` flags and the
    // artifact counts below are what pin the caching itself, on every
    // machine. Same convention as the announce→fetching ceiling in
    // `tests/sync_e2e.rs`'s `bidirectional_simultaneous_transfers_both_complete`.
    if std::env::var_os("CI").is_some() {
        eprintln!(
            "skipping the wall-clock ceiling on CI: second run's stage_output (LN fully \
                 cached) took {elapsed:?}"
        );
    } else {
        assert!(
            elapsed < Duration::from_secs(1),
            "second run's stage_output (LN fully cached) took {elapsed:?}"
        );
    }

    let group_summary = summary_group(&rc2, &group_key);
    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
    for f in included {
        assert!(f.cached_ln, "{f:?}");
    }

    let ln_artifacts = rig.artifacts("ln");
    assert_eq!(ln_artifacts.len(), 4, "{ln_artifacts:?}");
    let ln_ref_artifacts = rig.artifacts("ln_reference");
    assert_eq!(ln_ref_artifacts.len(), 1, "{ln_ref_artifacts:?}");

    // M1: the round trip below is only meaningful while there IS a
    // beta to disagree about — a group whose column stayed NULL would
    // make both sides fold nothing and pass vacuously.
    let run2_groups = crate::db::stacking::list_groups(&rig.fixture.conn, rc2.run_id).unwrap();
    assert!(
        run2_groups.iter().all(|g| g.beta.is_some()),
        "stage 3 must have recorded a group beta: {run2_groups:?}"
    );

    // M1 (final fix wave, ruling C-30): the PLAN GATE must agree with
    // the run about these same sidecars. It recomputes
    // `normalization_hash_for` from stored data alone, and since M1
    // that hash folds in the group beta — which the gate can only read
    // back from `stacking_run_groups.beta` of the last run, while the
    // run passes what stage 3 resolved in memory. If the two ever
    // disagreed (a NULL column, a different rounding, a fold on one
    // side only), Normalize would read permanently stale here even
    // though the run just cache-hit every frame. That round trip, not
    // a second copy of the assertion above, is what this pins.
    let plan_after = rig.plan(&cfg);
    assert!(
        !plan_after.stale_stages.contains(&Stage::Normalize),
        "the gate must resolve the same group beta the run did: {:?}",
        plan_after.stale_stages
    );
    assert_eq!(
        plan_after.groups[0].ln_cached, 4,
        "{:?}",
        plan_after.groups[0]
    );
}

/// Frame-level cache spec §7 (Task 12): one debayered-OSC fixture of two exposure classes (60 s and 90 s, three
/// frames each) stacked under `seconds` (two groups), `stops` (one group), and `seconds` with a 200 s tolerance (one group),
/// co-registered geometry, local normalization on, a Manual reference. After the first run nothing keyed on a
/// frame is recomputed: calibrate, pass 1, pass 2 and registration fan out over 0 items, no registered row or
/// file moves, the plan gate reads Calibrate, Measure and Register fresh before each regroup, and the
/// group-level stages (LN, rejection, the master) run for the new group.
///
/// The reference is pinned on purpose. Under Auto the reference is picked again from the new grouping's weights
/// and a different pick re-registers every frame by `ref:<id>` (observed on this fixture: frame 4 under
/// `seconds`, frame 6 under `stops`), as under Native geometry; with `reference.twoPass` the dry pass is a
/// group-level decision keyed on the membership, so a regroup is a miss and registers the new group once more
/// without persisting (README, "Frame-level cache").
#[test]
fn regroup_reuses_every_frame_level_stage() {
    use crate::stacking::config::ExposureMode;
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0), (2.0, 2.0), (1.0, 0.0)];
    let mut light_ids = Vec::new();
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = LightSpec {
            exptime: if i < 3 { 60.0 } else { 90.0 },
            // A debayered OSC set, as set 270: its registration reads no Measure output, so neither its own
            // group nor its reference's group enters the registration hash and a merge cannot move it.
            bayerpat: Some("RGGB"),
            ..star_light_spec(&stem, &date_obs)
        };
        // A Moffat field whose true beta (1.2) lies below `BETA_BOUNDS`' floor: every fit pegs at the floor, so
        // every frame, every exposure class and the merged group read the same group beta (as set 270's 1.5).
        // Without it the lower median of the fitted betas moves with the membership and pass 2 re-runs.
        let (id, _) = test_fixtures::add_light_with_moffat_field(
            &fixture,
            &spec,
            &ln_shifted_stars(dx, dy),
            3.4,
            1.2,
            600.0,
            5.0,
            100 + i as u64,
        );
        light_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, light_ids[0]).unwrap();
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, working.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    let cfg_for = |mode: ExposureMode, tolerance: f64| {
        let mut cfg = StackingConfig::default();
        cfg.grouping.exposure_mode = mode;
        cfg.grouping.exposure_tolerance_sec = tolerance;
        cfg.reference.mode = ReferenceMode::Manual;
        cfg.normalization.local.enabled = true;
        // As `local_normalization_sidecars_are_cached_on_the_second_run`: the finite selection that cuts
        // nothing, pinned to PsfSignal (L-24).
        cfg.normalization.local.flux_estimator =
            crate::stacking::integrate::LnFluxEstimator::PsfSignal;
        cfg.normalization.local.star_selection = crate::stacking::integrate::LnStarSelection {
            min_peak_snr: 5.0,
            max_stars: 5000,
            saturation: 1.0,
        };
        cfg
    };
    let stale_stages = |cfg: &StackingConfig| {
        crate::stacking::plan::build_plan(
            &fixture.conn,
            &ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(fixture.set_id),
            Some(cfg.clone()),
        )
        .expect("the plan should build")
        .stale_stages
    };
    // One run through Output under `cfg`; returns the run and the captured logs.
    let run = |cfg: &StackingConfig| {
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
            layout.clone(),
            output.path().to_path_buf(),
            group_ids,
        );
        let (res, logs) = crate::stacking::telemetry::capture_logs(|| {
            run_stages_for_test(&mut rc, Stage::Register)?;
            stage_output(&mut rc)
        });
        res.unwrap();
        // `start_stacking` records the reference on the run row; the plan gate reads it from there.
        fixture
            .conn
            .execute(
                "UPDATE stacking_runs SET reference_frame_id = ?1 WHERE id = ?2",
                rusqlite::params![rc.reference_frame_id, rc.run_id],
            )
            .unwrap();
        (plan_groups, rc, logs)
    };

    let cfg1 = cfg_for(ExposureMode::Seconds, 2.0);
    let (groups1, rc1, logs1) = run(&cfg1);
    assert_eq!(groups1.len(), 2, "one group per exposure class: {groups1:?}");
    assert_eq!(fan_out_items(&logs1, "calibrate", "calibrate"), 6, "{logs1}");
    assert_eq!(fan_out_items(&logs1, "measure", "pass1"), 6, "{logs1}");
    assert_eq!(fan_out_items(&logs1, "measure", "pass2"), 6, "{logs1}");
    let reference1 = rc1.reference_frame_id;
    assert_eq!(reference1, Some(light_ids[0]), "the pinned reference");
    assert_eq!(fan_out_items(&logs1, "register", "align"), 6, "{logs1}");
    let registered_rows = || {
        let mut rows: Vec<_> = rows_of_kind(&fixture.conn, fixture.set_id, "registered")
            .into_values()
            .map(|r| (r.frame_id, r.id, r.created_at, r.config_hash, r.path))
            .collect();
        rows.sort();
        rows
    };
    let registered1 = registered_rows();
    assert_eq!(registered1.len(), 6);

    let regroups = [
        ("stops", cfg_for(ExposureMode::Stops, 2.0), 1usize),
        ("a 200 s tolerance", cfg_for(ExposureMode::Seconds, 200.0), 1usize),
    ];
    let mut seen_keys = vec![groups1.iter().map(|g| g.key.clone()).collect::<Vec<_>>()];
    for (what, cfg, n_groups) in regroups {
        let stale = stale_stages(&cfg);
        for stage in [Stage::Calibrate, Stage::Measure, Stage::Register] {
            assert!(
                !stale.contains(&stage),
                "the gate reads {stage:?} stale before the regroup to {what}: {stale:?}"
            );
        }
        let (groups, rc, logs) = run(&cfg);
        assert_eq!(groups.len(), n_groups, "{what}: {groups:?}");
        let keys: Vec<String> = groups.iter().map(|g| g.key.clone()).collect();
        assert!(!seen_keys.contains(&keys), "{what} must name its group differently: {keys:?}");
        seen_keys.push(keys);
        assert_eq!(rc.reference_frame_id, reference1, "{what}: the reference moved");
        for (stage, pass) in [
            ("calibrate", "calibrate"),
            ("measure", "pass1"),
            ("measure", "pass2"),
            ("register", "align"),
            ("register", "rewrite"),
        ] {
            assert_eq!(
                fan_out_items(&logs, stage, pass),
                0,
                "a regroup to {what} redid {stage}/{pass}: {logs}"
            );
        }
        assert_eq!(registered_rows(), registered1, "{what}: a registered row moved");

        // The group-level stages ran for the new group: LN scales exist and the master is written.
        let summary = &rc.summary.groups[0];
        let included: Vec<_> = summary.frames.iter().filter(|f| f.included).collect();
        assert_eq!(included.len(), 6, "{what}: {:?}", summary.frames);
        for f in &included {
            assert!(f.ln_scale.is_some(), "{what}: {f:?}");
            assert!(!f.cached_ln, "{what}: LN of a new group is computed, not reused: {f:?}");
        }
        let master = summary.master_path.clone().expect("a master path");
        assert!(Path::new(&master).exists(), "{what}: the master is on disk");
    }

    // Every frame-level row still sits under the empty key and names one file.
    for kind in ["calibrated", "measured", "registered"] {
        let rows = rows_of_kind(&fixture.conn, fixture.set_id, kind);
        assert_eq!(rows.len(), 6, "{kind}: {rows:?}");
        assert!(rows.values().all(|r| r.group_key.is_empty()), "{kind}: {rows:?}");
    }
}

/// Ruling R6 I1 (the registration half of Task 4's review focus 1): a regroup that merges two mono exposure
/// classes with different PSF shapes gives the merged group a new beta, so its members' pass 2 and registration
/// (the mono registration reads Measure's `fits.0`, which carries the beta) are redone. A debayered-OSC group in the
/// same set, registered onto an OSC reference of its own group, reads no Measure output: its frames are reused
/// whatever happens to the mono betas. Align items = the mono members only; the OSC frames come back `cached`.
#[test]
fn a_merged_beta_re_registers_the_mono_members_and_not_the_debayered_osc_frames() {
    use crate::stacking::config::ExposureMode;
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let mut mono_ids = Vec::new();
    let mut osc_ids = Vec::new();
    // Mono: three Moffat (beta 2.5) frames at 60 s and three Gaussian ones at 120 s, as in
    // `merged_group_with_a_new_beta_reruns_pass_two_only`.
    for i in 0..6 {
        let (dx, dy) = shifts[i % 3];
        let date_obs = date_obs_at(i);
        let stem = format!("m{i}");
        let moffat_class = i < 3;
        let spec = LightSpec {
            filter: Some("L"),
            exptime: if moffat_class { 60.0 } else { 120.0 },
            ..star_light_spec(&stem, &date_obs)
        };
        let stars = shifted_stars(dx, dy);
        let (id, _) = if moffat_class {
            test_fixtures::add_light_with_moffat_field(&fixture, &spec, &stars, 3.4, 2.5, 600.0, 5.0, 100 + i as u64)
        } else {
            test_fixtures::add_light_with_field_fwhm(&fixture, &spec, &stars, 3.4, 600.0, 5.0, 100 + i as u64)
        };
        mono_ids.push(id);
    }
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        let date_obs = date_obs_at(i + 6);
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
    test_fixtures::add_master_dark_and_flat(&fixture, &all_ids, STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT);
    // The reference is an OSC frame: no mono group's beta can move the OSC frames' registration through it.
    crate::registration::db::set_frame_set_reference(&fixture.conn, fixture.set_id, osc_ids[0]).unwrap();
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    for (key, dir) in [
        (crate::settings::keys::STACKING_WORKING_DIR, working.path()),
        (crate::settings::keys::STACKING_OUTPUT_DIR, output.path()),
    ] {
        crate::db::set_setting(&fixture.conn, key, dir.to_str().unwrap()).unwrap();
    }
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    let run = |mode: ExposureMode| {
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
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
        let (res, logs) =
            crate::stacking::telemetry::capture_logs(|| run_stages_for_test(&mut rc, Stage::Register));
        res.unwrap();
        (plan_groups, rc, logs)
    };
    // Whether each member's registration this pass was reused, for the frames in `ids`.
    let cached_of = |rc: &RunContext, ids: &[i64]| -> Vec<bool> {
        let mut out = Vec::new();
        for entries in rc.measured.values() {
            for e in entries.iter().filter(|e| ids.contains(&e.frame.frame_id)) {
                match &e.registration {
                    Some(RegisteredFrameOutcome::Aligned { cached, .. }) => out.push(*cached),
                    _ => panic!("frame {} not aligned", e.frame.frame_id),
                }
            }
        }
        out
    };
    let mono_beta = |rc: &RunContext, groups: &[IntegrationGroup]| -> Vec<f64> {
        groups
            .iter()
            .filter(|g| g.frames.iter().any(|f| mono_ids.contains(&f.frame_id)))
            .map(|g| rc.measured_groups[&g.key].beta.expect("a measured mono group has a beta"))
            .collect()
    };

    let (groups1, rc1, logs1) = run(ExposureMode::Seconds);
    assert_eq!(groups1.len(), 3, "two mono classes and the OSC group: {groups1:?}");
    assert_eq!(rc1.reference_frame_id, Some(osc_ids[0]));
    assert_eq!(fan_out_items(&logs1, "register", "align"), 10, "a first run aligns every frame: {logs1}");
    assert_eq!(cached_of(&rc1, &all_ids), vec![false; 10]);
    let betas1 = mono_beta(&rc1, &groups1);
    assert_eq!(betas1.len(), 2);

    let (groups2, rc2, logs2) = run(ExposureMode::NoSplit);
    assert_eq!(groups2.len(), 2, "one merged mono group and the OSC group: {groups2:?}");
    let betas2 = mono_beta(&rc2, &groups2);
    assert_eq!(betas2.len(), 1);
    assert!(
        betas1.iter().all(|b| (b - betas2[0]).abs() > 1e-6),
        "premise: the merged beta {betas2:?} differs from both source betas {betas1:?}"
    );
    assert_eq!(
        fan_out_items(&logs2, "register", "align"),
        mono_ids.len(),
        "the merged mono members re-register and nothing else does: {logs2}"
    );
    assert_eq!(cached_of(&rc2, &mono_ids), vec![false; 6], "the mono members re-register");
    assert_eq!(cached_of(&rc2, &osc_ids), vec![true; 4], "the debayered OSC frames are reused");
}

/// T5 (spec 2026-10-04 §3 step 1, L-23): the run writes the
/// reference-frames saturation map beside the LN reference and reads it
/// back on a cache hit; a cached reference whose map is missing is
/// rebuilt (map included) with a `warn!`, and its members' `.athln`
/// sidecars stay cached — the rebuilt reference is the same image.
#[test]
fn a_cached_ln_reference_without_its_saturation_map_is_rebuilt() {
    let rig = LnRig::new();
    let cfg = ln_stored_config();
    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();
    let reference_path = rig.layout.ln_reference_path(&group_key);
    let map_path = rig.layout.ln_reference_saturation_path(&group_key);
    let run_card = |path: &Path| {
        crate::fits_parser::FitsHeader::from_path(path)
            .unwrap()
            .get_str("ATH_STKI")
            .unwrap_or_default()
    };
    let run = |stage_logs: bool| -> (i64, String, RunContext) {
        let mut rc = rig.context(cfg.clone(), plan_groups.clone());
        run_stages_for_test(&mut rc, Stage::Register).unwrap();
        let logs = if stage_logs {
            crate::stacking::solve::tests::captured_logs(|| stage_output(&mut rc).unwrap())
        } else {
            stage_output(&mut rc).unwrap();
            String::new()
        };
        (rc.run_id, logs, rc)
    };

    // Run 1 builds the reference and its map, with the same cards.
    let (run1, _, _) = run(false);
    assert!(
        map_path.exists(),
        "the saturation map is written beside the reference"
    );
    assert_eq!(run_card(&reference_path), run1.to_string());
    assert_eq!(run_card(&map_path), run1.to_string());

    // Run 2: a cache hit reads the map; nothing is rebuilt.
    let (_, logs2, _) = run(true);
    assert!(!logs2.contains("no usable saturation map"), "{logs2}");
    assert_eq!(run_card(&reference_path), run1.to_string(), "cache hit");

    // Run 3: the map is gone, so the cached reference is rebuilt.
    std::fs::remove_file(&map_path).unwrap();
    let (run3, logs3, rc3) = run(true);
    assert!(
        logs3.contains("the cached LN reference has no usable saturation map; rebuilding"),
        "{logs3}"
    );
    assert!(
        logs3.contains("ln reference saturation map built"),
        "{logs3}"
    );
    assert!(map_path.exists(), "the map is rebuilt");
    assert_eq!(
        run_card(&reference_path),
        run3.to_string(),
        "the reference is rebuilt"
    );
    assert_eq!(run_card(&map_path), run3.to_string());
    let group_summary = summary_group(&rc3, &group_key);
    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
    for f in included {
        assert!(f.cached_ln, "the members' sidecars stay cached: {f:?}");
    }
}

/// Spec §2.3 + Review Focus 3: the plan reports the scale it will use,
/// marked estimated before any run; the run resolves the same value; the
/// next plan reads it exactly from the reference payload and Normalize
/// is cached.
#[test]
fn the_plan_reads_normalize_cached_after_a_run_with_a_derived_scale() {
    let rig = LnRig::new();
    let mut cfg = ln_stored_config();
    cfg.normalization.local.scale_mode = Some(crate::stacking::integrate::LnScaleMode::Auto);
    let expected_scale = ln::resolve_scale(
        &cfg.normalization.local,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let plan_groups = rig.plan_groups(&cfg);
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let group_key = plan_groups[0].key.clone();

    // 1. The plan before any run: the scale of the anchor geometry, an
    //    estimate.
    let plan_before = rig.plan(&cfg);
    for g in &plan_before.groups {
        assert_eq!(g.ln_scale_px, Some(expected_scale), "{g:?}");
        assert!(g.ln_scale_estimated, "{g:?}");
    }

    // 2. The run.
    let mut rc = rig.context(cfg.clone(), plan_groups);
    run_register_and_output(&mut rc);

    // 3. The summary and the stored reference payload carry the scale
    //    and the geometry.
    let group_summary = summary_group(&rc, &group_key);
    assert_eq!(group_summary.ln_scale_px, Some(expected_scale));
    assert_eq!(
        group_summary.ln_flux_estimator,
        Some(crate::stacking::integrate::LnFluxEstimator::PsfSignal)
    );
    assert_eq!(group_summary.ln_aperture, None);
    let ref_rows = rig.artifacts("ln_reference");
    assert_eq!(ref_rows.len(), 1, "{ref_rows:?}");
    let payload: crate::stacking::plan::LnReferencePayload =
        serde_json::from_str(ref_rows[0].payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        (payload.width, payload.height, payload.scale_px),
        (STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT, expected_scale)
    );

    // 4. The next plan reads the scale exactly and Normalize is cached.
    let plan_after = rig.plan(&cfg);
    let g = &plan_after.groups[0];
    assert!(!g.ln_scale_estimated, "{g:?}");
    assert_eq!(g.ln_scale_px, Some(expected_scale), "{g:?}");
    assert_eq!(
        (g.ln_width, g.ln_height),
        (
            Some(STAR_FIELD_WIDTH as u32),
            Some(STAR_FIELD_HEIGHT as u32)
        )
    );
    assert!(
        !plan_after.stale_stages.contains(&Stage::Normalize),
        "{:?}",
        plan_after.stale_stages
    );
    assert_eq!(g.ln_cached, 4, "{g:?}");
}

/// 2026-10-06 (L-24): the DEFAULT path - no estimator pin. A 384x288 field
/// on which every star keeps its whole [6F, 8F] annulus present, so F1 at
/// the growth curve forms a sample on every member: a master is written,
/// the summary names the estimator and the aperture, every included
/// frame has its sidecar, and the plan reads Normalize as cached.
#[test]
fn the_default_f1_path_runs_end_to_end_and_the_plan_reads_it_cached() {
    const W: usize = 384;
    const H: usize = 288;
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    // 7 x 5 jittered grid, every star >= 40 px (8F ~ 30 + shift + margin)
    // from every edge.
    let mut rng = crate::geometry::ransac::SplitMix64(11);
    let mut base = Vec::new();
    for row in 0..5 {
        for col in 0..7 {
            let x = 44.0 + col as f64 * 49.0 + (rng.next_f64() - 0.5) * 12.0;
            let y = 44.0 + row as f64 * 50.0 + (rng.next_f64() - 0.5) * 12.0;
            let amp = if (row + col) % 2 == 0 { 9000.0 } else { 6500.0 };
            base.push((x, y, amp));
        }
    }
    let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
    let mut light_ids = Vec::new();
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        let stars: Vec<_> = base.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = LightSpec {
            width: W,
            height: H,
            ..star_light_spec(&stem, &date_obs)
        };
        let (id, _) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            600.0,
            5.0,
            100 + i as u64,
        );
        light_ids.push(id);
    }
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
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, W, H);
    let rig = LnRig::from_parts(tmp, ctx, fixture, light_ids, working, output);

    // No estimator or aperture line: the defaults are the subject. OFF is
    // not storable (it serializes as null), so the finite no-cut selection.
    let mut cfg = StackingConfig::default();
    cfg.normalization.local.enabled = true;
    cfg.normalization.local.star_selection = crate::stacking::integrate::LnStarSelection {
        min_peak_snr: 5.0,
        max_stars: 5000,
        saturation: 1.0,
    };
    cfg.normalization.local.scale_mode = Some(crate::stacking::integrate::LnScaleMode::Auto);

    let plan_groups = rig.plan_groups(&cfg);
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let group_key = plan_groups[0].key.clone();
    let mut rc = rig.context(cfg.clone(), plan_groups);
    run_register_and_output(&mut rc);

    let group_summary = summary_group(&rc, &group_key);
    assert_eq!(
        group_summary.ln_flux_estimator,
        Some(crate::stacking::integrate::LnFluxEstimator::CommonAperture)
    );
    assert_eq!(
        group_summary.ln_aperture,
        Some(crate::stacking::integrate::LnAperture::GrowthCurve)
    );
    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
    for f in &included {
        assert!(f.ln_scale.is_some(), "{f:?}");
    }
    for i in 0..4 {
        let sidecar = rig.sidecar(&group_key, i);
        assert!(sidecar.exists(), "sidecar for f{i} must exist: {sidecar:?}");
    }

    let plan_after = rig.plan(&cfg);
    let g = &plan_after.groups[0];
    assert!(!g.ln_scale_estimated, "{g:?}");
    assert!(
        !plan_after.stale_stages.contains(&Stage::Normalize),
        "{:?}",
        plan_after.stale_stages
    );
    assert_eq!(g.ln_cached, 4, "{g:?}");
}

/// Final review I-1: on the 192x144 field the corner stars' annuli are
/// under half present, so the default F1 keeps 17 pairs (< 20). Each
/// member is excluded with a reason naming the common aperture (not
/// "N matched stars"), and the group carries ONE run warning counting
/// them.
#[test]
fn a_common_aperture_shortfall_names_its_cause_and_warns_once_per_group() {
    let rig = LnRig::new();

    // The estimator and the aperture stay at their defaults: the subject.
    let mut cfg = StackingConfig::default();
    cfg.normalization.local.enabled = true;
    cfg.normalization.local.star_selection = crate::stacking::integrate::LnStarSelection::OFF;

    let plan_groups = rig.plan_groups(&cfg);
    let mut rc = rig.context(cfg, plan_groups);
    run_stages_for_test(&mut rc, Stage::Register).unwrap();
    let _ = stage_output(&mut rc);

    let aperture: Vec<&(i64, String)> = rc
        .runtime_exclusions
        .iter()
        .filter(|(_, why)| why.contains("common aperture"))
        .collect();
    assert!(!aperture.is_empty(), "{:?}", rc.runtime_exclusions);
    for (_, why) in &aperture {
        assert!(why.chars().count() < 200, "{why}");
        assert!(
            why.contains("(< 20)") && why.contains("matched stars fit"),
            "{why}"
        );
        assert!(!why.ends_with("matched stars (< 20)"), "{why}");
    }
    let hits: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.contains("common aperture could not measure enough stars"))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", rc.warnings);
    assert!(
        hits[0].contains(&format!("{} frame(s)", aperture.len())),
        "{hits:?} vs {} exclusions",
        aperture.len()
    );
}

/// Review Focus 3: a run whose scale differs from the plan's estimate
/// warns once.
#[test]
fn a_run_warns_once_when_its_ln_scale_differs_from_the_plan() {
    let rig = LnRig::new();
    let cfg = ln_run_config();
    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();

    let mut rc = rig.context(cfg, plan_groups);
    // A value the fixture geometry cannot resolve to.
    rc.planned_ln_scales.insert(group_key, 8192);

    run_register_and_output(&mut rc);

    let hits: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.contains("the plan estimated 8192 px"))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", rc.warnings);
}

/// Fix round 1, item 2: the per-frame `ln` hash must fold in the
/// group's own `reference_hash`, not just the reference MEMBER ID list —
/// otherwise re-registering (or recalibrating) ONE reference member
/// rebuilds the reference (a new `B_ref`, a new scale anchor) while
/// every OTHER member's sidecar still reads as fresh against a
/// reference that no longer matches it. A direct SQL flip of
/// `registration_results.config_hash` would just get silently
/// re-derived (and reverted) by the second run's own stage 5, so this
/// forces a REAL registration change the established way (touch a
/// source file's `size` in the catalog, same technique
/// `calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones`
/// uses): recalibration cascades into a genuinely different calibration
/// hash for that one frame, which cascades into a genuinely different
/// registration hash for it. `referenceFrames` defaults to 20 (clamped
/// to this fixture's 4 members), so ALL FOUR are reference members —
/// touching any one of them is guaranteed to touch the reference.
///
/// Carry-over (d), Task 5's re-review: the touched member
/// (`light_ids[0]`) must NOT also be the run's own REGISTRATION
/// reference — `registration_hash_for` folds the registration
/// reference's own identity into EVERY frame's registration hash
/// (`stage_reference`/`stage_register`), so touching the registration
/// reference itself would already invalidate every OTHER frame's plain
/// registration hash regardless of this task's item-2 fix, confounding
/// what the test is meant to isolate. Pinned via a MANUAL registration
/// reference at `light_ids[1]` (any frame other than the touched one)
/// so `light_ids[0]`'s own registration change cannot cascade into the
/// other three frames' registration hashes on its own — only the LN
/// reference's own hash (item 2's fix) can.
#[test]
fn local_normalization_sidecars_all_rebuild_when_a_reference_members_registration_changes() {
    let rig = LnRig::new();
    crate::registration::db::set_frame_set_reference(
        &rig.fixture.conn,
        rig.fixture.set_id,
        rig.light_ids[1],
    )
    .unwrap();

    let mut cfg = ln_run_config();
    cfg.reference.mode = ReferenceMode::Manual;

    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();

    let mut rc1 = rig.context(cfg.clone(), plan_groups);
    run_register_and_output(&mut rc1);

    // Touch light_ids[0]'s catalog `size` — forces recalibration (and,
    // downstream, re-registration) of that ONE frame on the next run,
    // without changing the actual pixels on disk at all.
    rig.fixture
        .conn
        .execute(
            "UPDATE files SET size = size + 1000 WHERE id = \
                 (SELECT file_id FROM frames WHERE id = ?1)",
            rusqlite::params![rig.light_ids[0]],
        )
        .unwrap();

    let plan_groups2 = rig.plan_groups(&cfg);
    let mut rc2 = rig.context(cfg, plan_groups2);
    run_register_and_output(&mut rc2);

    let group_summary = summary_group(&rc2, &group_key);
    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
    for f in included {
        assert!(
            !f.cached_ln,
            "frame {} must rebuild its sidecar once a reference member's own \
                 registration changed: {f:?}",
            f.frame_id
        );
    }
}

/// Fix round 1, item 4: deterministic exclusion coverage (no starless
/// frame needed) — pre-creating a DIRECTORY at one frame's own sidecar
/// path makes [`crate::stacking::ln::LnFrameGrids::write`]'s tmp-file +
/// atomic-rename fail outright (`rename` onto an existing directory is
/// never valid), so `normalize_frame` returns an error for exactly that
/// frame with no need to engineer a starless field. This exercises the
/// FULL exclusion path: `members.retain`/`pick_reference_idx`
/// re-derivation in `process_group_output`, the group still integrating
/// and writing its master from the surviving 3, `SummaryFrame.included
/// == false` with the reason, and (item 1) the SAME reason reaching the
/// frame's `stacking_run_frames` row.
#[test]
fn local_normalization_excludes_a_frame_whose_sidecar_write_fails() {
    let rig = LnRig::new();
    let cfg = ln_run_config();
    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();

    // f1's stem is `f1_f<frame id>` (`frame_file_stem`).
    let blocked_sidecar = rig.sidecar(&group_key, 1);
    std::fs::create_dir_all(&blocked_sidecar).unwrap();

    let mut rc = rig.context(cfg, plan_groups);

    run_register_and_output(&mut rc);

    let blocked_frame_id = rig.light_ids[1];
    let group_summary = summary_group(&rc, &group_key);
    let blocked_summary = group_summary
        .frames
        .iter()
        .find(|f| f.frame_id == blocked_frame_id)
        .expect("f1's own summary entry exists");
    assert!(!blocked_summary.included, "{blocked_summary:?}");
    assert!(
        blocked_summary
            .exclusion_reason
            .as_deref()
            .unwrap_or_default()
            .contains("writing .athln sidecar"),
        "{blocked_summary:?}"
    );

    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(included.len(), 3, "{:?}", group_summary.frames);
    assert!(group_summary.master_path.is_some(), "{group_summary:?}");
    assert!(Path::new(group_summary.master_path.as_ref().unwrap()).exists());

    // Item 1: the SAME exclusion reached the frame's own DB row, not
    // just the in-memory summary.
    let frame_rows = crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc.run_id).unwrap();
    let blocked_row = frame_rows
        .iter()
        .find(|r| r.frame_id == blocked_frame_id)
        .expect("f1's own stacking_run_frames row exists");
    assert!(!blocked_row.included, "{blocked_row:?}");
    assert!(
        blocked_row
            .exclusion_reason
            .as_deref()
            .unwrap_or_default()
            .contains("writing .athln sidecar"),
        "{blocked_row:?}"
    );
}

#[test]
fn local_normalization_off_leaves_no_ln_files() {
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let rig = LnRig::seeded(seed_star_group, &shifts, &[5.0f32; 4]);

    // `StackingConfig::default()`'s `normalization.local.enabled` is
    // `false`; the disabled path needs the rejection normalization global
    // too (the default is `local` since OD-1a, which runs LN for rejection).
    let mut cfg = StackingConfig::default();
    cfg.normalization.rejection = RejectionNormalization::ScaleZeroOffset;
    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();

    let mut rc = rig.context(cfg, plan_groups);

    run_register_and_output(&mut rc);

    let ln_dir = rig.layout.ln_dir(&group_key);
    assert!(
        !ln_dir.exists(),
        "no ln/ files must be written when local normalization is off"
    );

    let group_summary = summary_group(&rc, &group_key);
    assert!(group_summary.ln_reference_path.is_none());
    for f in &group_summary.frames {
        assert!(f.ln_scale.is_none(), "{f:?}");
        assert!(!f.cached_ln, "{f:?}");
    }

    let ln_artifacts = rig.artifacts("ln");
    assert!(ln_artifacts.is_empty(), "{ln_artifacts:?}");
    let ln_ref_artifacts = rig.artifacts("ln_reference");
    assert!(ln_ref_artifacts.is_empty(), "{ln_ref_artifacts:?}");
}

/// M2 fix round 1, item 3: a `ln` artifact row a cache hit trusts
/// (hash/size/mtime all still match) whose actual FILE content can no
/// longer be read — corruption, a race, anything short of the size
/// changing — must be RE-NORMALIZED in the same run, not excluded
/// forever (the OLD behaviour this test used to pin: excluding left the
/// stale artifact row and the corrupt file in place, so every LATER run
/// would cache-hit the same bytes and re-exclude the same frame,
/// permanently). One payload byte is flipped WITHOUT changing the
/// file's length, so `is_fresh`'s live `std::fs::metadata` stat still
/// agrees with the artifact row's stored size and the run treats this
/// as a cache hit — `run_group_normalization`'s own read-back (moved
/// earlier by item 3, no longer deferred to `process_group_output`) is
/// the first point this second run actually looks at the bytes, finds
/// them corrupt, and falls through to `needs_normalize` instead of
/// excluding. (Contrast
/// `local_normalization_excludes_a_frame_whose_sidecar_write_fails`,
/// which exercises a genuine WRITE failure via a directory at the
/// path — that path is unaffected by this fix, since a `needs_normalize`
/// frame that then FAILS to normalize is still excluded/warned exactly
/// as before.)
#[test]
fn local_normalization_re_normalizes_a_frame_whose_cached_sidecar_is_unreadable() {
    let rig = LnRig::new();
    let cfg = ln_run_config();
    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();

    let mut rc1 = rig.context(cfg.clone(), plan_groups.clone());
    run_register_and_output(&mut rc1);

    // Corrupt f1's sidecar payload in place, same total length: the
    // artifact row's stored size/hash are untouched, so the second
    // run's `is_fresh` check still calls this a cache hit.
    let touched_frame_id = rig.light_ids[1];
    let sidecar = rig.sidecar(&group_key, 1);
    let mut bytes = std::fs::read(&sidecar).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&sidecar, &bytes).unwrap();

    let mut rc2 = rig.context(cfg, plan_groups);
    run_register_and_output(&mut rc2);

    let group_summary = summary_group(&rc2, &group_key);
    let touched_summary = group_summary
        .frames
        .iter()
        .find(|f| f.frame_id == touched_frame_id)
        .expect("f1's own summary entry exists");
    assert!(
        touched_summary.included,
        "a corrupt cached sidecar must be re-normalized, not excluded: {touched_summary:?}"
    );
    assert!(
        !touched_summary.cached_ln,
        "f1 was re-normalized this run, not reused: {touched_summary:?}"
    );
    assert!(touched_summary.ln_scale.is_some(), "{touched_summary:?}");
    // The other three frames' own artifacts were never touched — still
    // genuine cache hits.
    for f in group_summary
        .frames
        .iter()
        .filter(|f| f.frame_id != touched_frame_id)
    {
        assert!(f.cached_ln, "{f:?}");
    }

    let included: Vec<_> = group_summary.frames.iter().filter(|f| f.included).collect();
    assert_eq!(included.len(), 4, "{:?}", group_summary.frames);
    assert!(group_summary.master_path.is_some(), "{group_summary:?}");
    assert!(Path::new(group_summary.master_path.as_ref().unwrap()).exists());
    let stats = group_summary.stats.as_ref().unwrap();
    assert_eq!(stats.included, 4, "{stats:?}");
    assert_eq!(
        stats.ln_frames, 4,
        "every member — the re-normalized one included — has a grid: {stats:?}"
    );

    // The re-normalized sidecar file itself is fixed (a whole, readable
    // .athln, not the corrupt bytes) — the group's own artifact caching
    // for the NEXT run depends on this actually landing on disk.
    let repaired = crate::stacking::ln::LnFrameGrids::read(&sidecar);
    assert!(repaired.is_ok(), "{repaired:?}");

    let frame_rows = crate::db::stacking::list_frame_rows(&rig.fixture.conn, rc2.run_id).unwrap();
    let touched_row = frame_rows
        .iter()
        .find(|r| r.frame_id == touched_frame_id)
        .expect("f1's own stacking_run_frames row exists");
    assert!(touched_row.included, "{touched_row:?}");
    assert!(touched_row.exclusion_reason.is_none(), "{touched_row:?}");
}

/// Same run shape, LN off (the M1 shape): no `Normalize` entry at all —
/// not a zero-duration one.
#[test]
fn local_normalization_off_pushes_no_stage_timing() {
    let rig = LnRig::new();

    let mut cfg = StackingConfig::default();
    assert!(!cfg.normalization.local.enabled);
    // The M1 shape is global rejection too: the default is `local` since
    // OD-1a, which runs the Normalize stage for rejection.
    cfg.normalization.rejection = RejectionNormalization::ScaleZeroOffset;

    let plan_groups = rig.plan_groups(&cfg);
    let mut rc = rig.context(cfg, plan_groups);

    run_register_and_output(&mut rc);

    assert!(
        !rc.timings.iter().any(|t| t.stage == Stage::Normalize),
        "LN disabled must push no Normalize entry at all: {:?}",
        rc.timings
    );
}

/// B1 (Critical C1), test (ii): the group-level LN fallback (ruling
/// R3-shaped — here forced via trigger 1, an unwritable `ln/<group>`
/// path so `run_group_normalization`'s own `create_dir_all` fails
/// outright before any frame is attempted) must integrate with the SAME
/// rejection pair `ScaleZeroOffset` would give it — never an
/// un-normalized identity pair. Proven by bit-for-bit pixel equality
/// against the identical fixture integrated with
/// `rejection = scaleZeroOffset` and LN off entirely (the fallback
/// leaves `GroupInput.ln` `None`, so `local_for_output`'s value cannot
/// affect the written pixels either — see `integrate_planes`'s own doc).
#[test]
fn local_rejection_normalization_group_fallback_matches_global_scale_zero_offset() {
    let rig = LnRig::new();

    let plan_groups_probe = rig.plan_groups(&StackingConfig::default());
    let group_key = plan_groups_probe[0].key.clone();

    // ── Run A: rejection = local, group-level fallback forced ──
    let mut cfg_fallback = StackingConfig::default();
    cfg_fallback.normalization.rejection = RejectionNormalization::Local;
    cfg_fallback.normalization.local.enabled = true;

    // Trigger 1: a plain FILE where `run_group_normalization` expects to
    // `create_dir_all` the group's `ln/` directory — the mkdir fails
    // outright, before any per-frame work, landing in the
    // `Err(RunError::Other(..))` arm ("continuing with global
    // normalization").
    rig.block_ln_dir(&group_key);

    let plan_groups_a = rig.plan_groups(&cfg_fallback);
    let mut rc_a = rig.context(cfg_fallback, plan_groups_a);
    run_register_and_output(&mut rc_a);

    let group_summary_a = summary_group(&rc_a, &group_key);
    assert!(
        group_summary_a.ln_reference_path.is_none(),
        "the mkdir failure must fall back to global normalization, not run LN: {group_summary_a:?}"
    );
    assert!(
        rc_a.warnings
            .iter()
            .any(|w| w.contains("local normalization failed")),
        "the fallback must warn: {:?}",
        rc_a.warnings
    );
    let master_a = group_summary_a
        .master_path
        .clone()
        .expect("group A wrote a master despite the LN fallback");

    // ── Run B: rejection = scaleZeroOffset, LN off, separate output dir ──
    let (_dirs_b, rc_b) = rig.run_global_comparison();

    let group_summary_b = summary_group(&rc_b, &group_key);
    assert!(group_summary_b.ln_reference_path.is_none());
    let master_b = group_summary_b
        .master_path
        .clone()
        .expect("group B wrote a master");

    // ── Compare pixel data bit-for-bit (headers legitimately differ:
    // NORMALIZATION card, run id, timestamps). ──
    assert_masters_equal_bit_for_bit(&master_a, &master_b, "fallback");
}

/// B1, test (iii) + A3: the rejection-only composite mode
/// (`rejection = local`, `local.enabled = false`) with one frame whose
/// own LN normalization fails (a directory pre-created at its sidecar
/// path, same deterministic trick as
/// `local_normalization_excludes_a_frame_whose_sidecar_write_fails`) —
/// the frame stays INCLUDED (ruling R2: LN driving rejection only never
/// excludes), `ln_frames < included`, the group still integrates and
/// writes a master, and a `warn!`/`rc.warnings` entry is recorded. The
/// SURVIVING grid-less frame's own rejection normalization must have
/// used the global pair — proven the same way as the group-level test
/// above, by bit-for-bit pixel equality against the identical fixture
/// integrated with `rejection = scaleZeroOffset` (no LN at all).
#[test]
fn local_normalization_rejection_only_frame_failure_keeps_global_normalization() {
    let rig = LnRig::new();

    // `ln_run_config` pins the estimator and the selection (P-13, L-24); the subject here is the one grid-less
    // frame among normalized ones, so LN runs for rejection only.
    let mut cfg_rejection_only = ln_run_config();
    cfg_rejection_only.normalization.rejection = RejectionNormalization::Local;
    cfg_rejection_only.normalization.local.enabled = false;

    let plan_groups_a = rig.plan_groups(&cfg_rejection_only);
    let group_key = plan_groups_a[0].key.clone();

    // f1's stem is `f1_f<frame id>` — same deterministic write-failure trick
    // as the OUTPUT-normalization exclusion test above.
    let blocked_sidecar = rig.sidecar(&group_key, 1);
    std::fs::create_dir_all(&blocked_sidecar).unwrap();

    let mut rc_a = rig.context(cfg_rejection_only, plan_groups_a);
    run_register_and_output(&mut rc_a);

    let blocked_frame_id = rig.light_ids[1];
    let group_summary_a = summary_group(&rc_a, &group_key);
    let stats_a = group_summary_a
        .stats
        .as_ref()
        .expect("group wrote a master and its stats");
    assert_eq!(stats_a.included, 4, "{stats_a:?}");
    assert_eq!(
        stats_a.ln_frames,
        stats_a.included - 1,
        "exactly the blocked frame's grid is missing: {stats_a:?}"
    );
    assert!(
        stats_a.ln_frames < stats_a.included,
        "the blocked frame's own grid must be missing: {stats_a:?}"
    );
    let blocked_summary = group_summary_a
        .frames
        .iter()
        .find(|f| f.frame_id == blocked_frame_id)
        .expect("f1's own summary entry exists");
    assert!(
        blocked_summary.included,
        "rejection-only LN never excludes a frame it could not normalize: {blocked_summary:?}"
    );
    // LN runs for rejection only here: the warning names the rejection
    // normalization the user got, not a "local normalization" they never
    // turned on (OD-1a).
    assert!(
        rc_a.warnings.iter().any(|w| w.starts_with(&format!(
            "frame {blocked_frame_id}: local rejection normalization could not be computed"
        ))),
        "the per-frame failure must warn: {:?}",
        rc_a.warnings
    );
    assert!(
        !rc_a
            .warnings
            .iter()
            .any(|w| w.contains("local normalization failed")),
        "{:?}",
        rc_a.warnings
    );
    let master_a = group_summary_a
        .master_path
        .clone()
        .expect("group A wrote a master with the blocked frame kept");

    // ── Comparison run: rejection = scaleZeroOffset, LN off entirely ──
    let (_dirs_b, rc_b) = rig.run_global_comparison();

    let group_summary_b = summary_group(&rc_b, &group_key);
    let master_b = group_summary_b
        .master_path
        .clone()
        .expect("comparison group wrote a master");

    assert_masters_equal_bit_for_bit(&master_a, &master_b, "rejection-only fallback");
}

/// The rejection-only twin of the group-level fallback test above: LN
/// runs for rejection alone (`local.enabled = false`, the shipped
/// default), its `ln/<group>` directory cannot be created, and the run
/// warning says what the user chose - local REJECTION normalization -
/// and what they got instead, not "local normalization failed".
#[test]
fn local_normalization_rejection_only_group_failure_names_the_rejection_normalization() {
    let rig = LnRig::new();
    let mut cfg = StackingConfig::default();
    cfg.normalization.rejection = RejectionNormalization::Local;
    cfg.normalization.local.enabled = false;

    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();
    // A plain FILE where the group's `ln/` directory must be created.
    rig.block_ln_dir(&group_key);
    let mut rc = rig.context(cfg, plan_groups);
    run_register_and_output(&mut rc);

    assert!(
        rc.warnings.iter().any(|w| w.starts_with(&format!(
            "group {group_key}: local rejection normalization could not be computed ("
        )) && w.ends_with("star cores where seeing varies")),
        "{:?}",
        rc.warnings
    );
    assert!(
        !rc.warnings
            .iter()
            .any(|w| w.contains("local normalization failed")),
        "the user never turned local normalization on: {:?}",
        rc.warnings
    );
    let summary = summary_group(&rc, &group_key);
    assert!(summary.master_path.is_some(), "the group still stacks");
}

/// `referenceFrames < 3` with LN for rejection only: no LN reference, the
/// group rejects on the global pair, and the warning says so in the
/// rejection normalization's own words.
#[test]
fn local_normalization_rejection_only_reference_frames_below_three_names_the_rejection_normalization(
) {
    let rig = LnRig::new();
    let mut cfg = StackingConfig::default();
    cfg.normalization.rejection = RejectionNormalization::Local;
    cfg.normalization.local.enabled = false;
    cfg.normalization.local.reference_frames = 2;

    let plan_groups = rig.plan_groups(&cfg);
    let group_key = plan_groups[0].key.clone();
    let mut rc = rig.context(cfg, plan_groups);
    run_register_and_output(&mut rc);

    assert!(
        rc.warnings.iter().any(|w| w.starts_with(&format!(
            "group {group_key}: local rejection normalization could not be computed — referenceFrames (2) < 3;"
        ))),
        "{:?}",
        rc.warnings
    );
    assert!(
        !rc.warnings
            .iter()
            .any(|w| w.contains("local normalization off")),
        "{:?}",
        rc.warnings
    );
}

/// One full run on the LN star fixture with drizzle on and `rejection =
/// local` (the default): returns the group's `GroupStats` and the
/// drizzle's own `DrizzleStats`. `local_output` is `normalization.local.
/// enabled` - LN as the OUTPUT normalization, not only for rejection.
fn drizzle_run_with_ln(
    local_output: bool,
) -> (GroupStats, crate::stacking::drizzle::DrizzleStats) {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (1.0, 1.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_ln_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    assert_eq!(
        cfg.normalization.rejection,
        RejectionNormalization::Local,
        "the shipped default"
    );
    cfg.normalization.local.enabled = local_output;
    // The subject is what the drizzle does with the grids, not the star
    // selection (synthetic stars, none saturated; see `ln_base_stars`).
    cfg.normalization.local.flux_estimator =
        crate::stacking::integrate::LnFluxEstimator::PsfSignal;
    cfg.normalization.local.star_selection = crate::stacking::integrate::LnStarSelection::OFF;
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    assert!(cfg.drizzle.use_local_normalization, "the drizzle default");
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary.groups.len(), 1, "{:?}", summary.groups);
    let group = &summary.groups[0];
    let stats = group.stats.clone().expect("group stats recorded");
    let drizzle = group.drizzle.clone().expect("drizzle stats recorded");
    (stats, drizzle)
}

/// The drizzle follows the plain master's OUTPUT normalization (pcores
/// review r1, I1): with LN computed for rejection alone the grids exist
/// (`GroupStats.ln_frames`) and rejected against them, but the drizzled
/// master is globally normalized like the plain one - none of its frames
/// carries a grid into the drizzle (`DrizzleStats.ln_frames == 0`).
#[test]
fn drizzle_does_not_use_ln_grids_computed_for_rejection_only() {
    let (stats, drizzle) = drizzle_run_with_ln(false);
    assert_eq!(stats.included, 4, "{stats:?}");
    assert_eq!(
        stats.ln_frames, stats.included,
        "LN ran for rejection, every frame has a grid: {stats:?}"
    );
    assert_eq!(drizzle.frames, stats.included, "{drizzle:?}");
    assert_eq!(
        drizzle.ln_frames, 0,
        "LN output is off: the drizzle must stay on the global pair: {drizzle:?}"
    );
}

/// The other half: with LN as the OUTPUT normalization the drizzle still
/// applies the grids (the toggle's only effect).
#[test]
fn drizzle_uses_ln_grids_when_ln_drives_the_output() {
    let (stats, drizzle) = drizzle_run_with_ln(true);
    assert_eq!(stats.included, 4, "{stats:?}");
    assert_eq!(
        drizzle.ln_frames, drizzle.frames,
        "every drizzled frame carries its grid: {drizzle:?}"
    );
    assert!(drizzle.ln_frames > 0, "{drizzle:?}");
}

/// Every LN fallback sentence, in both modes: with local normalization
/// ON (output) the wording is the one the user has always read; with LN
/// for rejection only it names the rejection normalization, says the
/// global pair was used, and never claims "local normalization" failed or
/// is off. The background-model refusal has no cheap end-to-end fixture,
/// so its sentence is pinned here.
#[test]
fn ln_fallback_warnings_name_what_the_user_chose() {
    let cases = [
        LnFallback::GroupFailed { msg: "boom" },
        LnFallback::FrameFailed {
            frame_id: 7,
            msg: "boom",
        },
        LnFallback::ReferenceFramesBelowThree {
            reference_frames: 2,
        },
        LnFallback::BackgroundRefused {
            reason: "no background sample",
        },
        LnFallback::LittleOverlap {
            count: 2,
            percent: 25.0,
        },
    ];
    let expected_on = [
        "group g: local normalization failed: boom",
        "frame 7: local normalization failed: boom",
        "group g: local normalization off — referenceFrames (2) < 3; using global normalization",
        "group g: local normalization off — the LN reference's background model is refused (no background sample); using global normalization",
        "group g: local normalization kept 2 frame(s) on global normalization — under 25 % overlap with the LN reference",
    ];
    let expected_rejection_only = [
        "group g: local rejection normalization could not be computed (boom); rejection used the global normalization, which can clip star cores where seeing varies",
        "frame 7: local rejection normalization could not be computed (boom); this frame's rejection used the global normalization",
        "group g: local rejection normalization could not be computed — referenceFrames (2) < 3; rejection used the global normalization, which can clip star cores where seeing varies",
        "group g: local rejection normalization could not be computed — the reference frame's background model is refused (no background sample); rejection used the global normalization, which can clip star cores where seeing varies",
        "group g: 2 frame(s) used the global normalization for rejection — under 25 % overlap with the reference frame",
    ];
    for (i, what) in cases.iter().enumerate() {
        assert_eq!(
            ln_fallback_warning("g", false, what),
            expected_on[i],
            "on {i}"
        );
        let only = ln_fallback_warning("g", true, what);
        assert_eq!(only, expected_rejection_only[i], "rejection only {i}");
        assert!(
            !only.contains("local normalization"),
            "rejection-only wording must not name a feature the user did not turn on: {only}"
        );
    }
}

// ── M4c Task 3: large-scale rejection wired into the run ────────────

/// The trail fixture's geometry, chosen so the band sits in a
/// star-free strip of the 192x144 field (the stars span rows 24..102)
/// and so the whole affected span plus the growth disc stays inside the
/// frame.
const TRAIL_CORE: std::ops::Range<usize> = 115..120;
const TRAIL_SHOULDER_LOW: std::ops::Range<usize> = 113..115;
const TRAIL_SHOULDER_HIGH: std::ops::Range<usize> = 120..122;
/// Raw ADU. Calibration subtracts the fixture's constant master dark
/// (100) and divides by the flat's own normalization (1.0), so the
/// calibrated background is 300 ADU and every excess below survives
/// unchanged.
const TRAIL_BACKGROUND: f32 = 400.0;
const TRAIL_NOISE: f32 = 30.0;
/// 450/300 = 1.5x the background: past the percentile clip's `high =
/// 1.0` (so the per-pixel pass rejects it outright), and 5 sigma short
/// of the star detector's own `background + 20 sigma` level (so it is
/// not mistaken for a source by stage 3).
const TRAIL_CORE_EXCESS: f32 = 450.0;
/// 240/300 = 0.8x — INSIDE the percentile clip's threshold, i.e. the
/// residual a real trail's faint edges leave in the master, and 2 sigma
/// clear of it so noise does not reject it by accident.
const TRAIL_SHOULDER_EXCESS: f32 = 240.0;

/// Six frames of the same star field (no shift: the trail is an
/// artifact of ONE frame's own pixel grid, and identity registration
/// keeps its rows where the test put them), one of which — `f2` —
/// carries a flat-topped trail: a 5-px core with a 2-px shoulder on
/// each side.
fn seed_trail_group(
    db_path: &Path,
    set_name: &str,
    trail_idx: usize,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);
    let bands = vec![
        (TRAIL_CORE.start, TRAIL_CORE.end, TRAIL_CORE_EXCESS),
        (
            TRAIL_SHOULDER_LOW.start,
            TRAIL_SHOULDER_LOW.end,
            TRAIL_SHOULDER_EXCESS,
        ),
        (
            TRAIL_SHOULDER_HIGH.start,
            TRAIL_SHOULDER_HIGH.end,
            TRAIL_SHOULDER_EXCESS,
        ),
    ];

    let mut light_ids = Vec::new();
    for i in 0..6usize {
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = star_light_spec(&stem, &date_obs);
        let (id, _path) = test_fixtures::add_light_with_field_and_bands(
            &fixture,
            &spec,
            &shifted_stars(0.0, 0.0),
            TRAIL_BACKGROUND,
            TRAIL_NOISE,
            400 + i as u64,
            if i == trail_idx { &bands } else { &[] },
        );
        light_ids.push(id);
    }

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

    (fixture, light_ids, working, output)
}

/// M4c Task 3 (ruling R-M4c-4) through the whole run: one frame of six
/// carries a flat-topped trail whose CORE the per-pixel rejection
/// catches and whose 2-px SHOULDER it cannot see (the shoulder deviates
/// 0.8x the background, the percentile clip's high threshold is 1.0x).
///
/// With `integration.largeScale` off, the master keeps 1/6 of that
/// shoulder — measurably high. With it on, the run produces the
/// rejection bitmaps even though drizzle is off (the widened sink
/// condition), the core's own rejected rows survive the filter, the
/// disc dilation covers the shoulders, and the second pass forces them
/// out: the master's shoulder rows come out at the other five frames'
/// level. The `.rejl` siblings ride the same per-run removal as the
/// `.rej` files.
#[test]
fn large_scale_rejection_removes_a_trails_shoulder_from_the_master() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let trail_idx = 2usize;
    let (fixture, light_ids, working, _output) =
        seed_trail_group(&db_path, SET_NAME, trail_idx);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let base_config = || {
        let mut cfg = StackingConfig::default();
        // The Auto rule at n = 6 is percentile 0.2/0.1, which would
        // reject the shoulder too — this pin needs a per-pixel test the
        // shoulder genuinely survives, which is the situation a real
        // trail's faint edges create at any threshold.
        cfg.integration.rejection = RejectionChoice::PercentileClip {
            low: 0.2,
            high: 1.0,
        };
        cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;
        cfg
    };

    let run_off = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(base_config()),
        None,
    )
    .expect("the baseline run should start");
    wait_for_run(&ctx, run_off.run_id);
    let row_off = crate::db::stacking::get_run(&fixture.conn, run_off.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row_off.status, "done", "{row_off:?}");
    let groups_off = crate::db::stacking::list_groups(&fixture.conn, run_off.run_id).unwrap();
    assert_eq!(groups_off.len(), 1, "{groups_off:?}");
    let master_off = groups_off[0]
        .master_path
        .clone()
        .expect("the baseline run wrote a master");
    let stats_off: GroupStats =
        serde_json::from_str(groups_off[0].stats_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        stats_off.large_scale_rejected_fraction, None,
        "large-scale rejection is off for the baseline run"
    );

    let mut cfg_on = base_config();
    cfg_on.integration.large_scale = LargeScaleRejection {
        enabled: true,
        protected_layers: 2,
        growth: 2,
    };
    let recorder = Arc::new(Recording::new());
    let run_on = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg_on),
        None,
    )
    .expect("the large-scale run should start");
    wait_for_run(&ctx, run_on.run_id);
    let row_on = crate::db::stacking::get_run(&fixture.conn, run_on.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row_on.status, "done", "{row_on:?}");
    let groups_on = crate::db::stacking::list_groups(&fixture.conn, run_on.run_id).unwrap();
    assert_eq!(groups_on.len(), 1, "{groups_on:?}");
    let master_on = groups_on[0]
        .master_path
        .clone()
        .expect("the large-scale run wrote a master");
    assert_ne!(master_on, master_off, "two runs, two master files");
    let stats_on: GroupStats =
        serde_json::from_str(groups_on[0].stats_json.as_deref().unwrap()).unwrap();
    let forced = stats_on
        .large_scale_rejected_fraction
        .expect("the second pass ran");
    // 9 affected rows of one frame in six, over the group's own
    // geometry — the filter keeps the 5-px core and grows it by 2. A
    // relative tolerance rather than an exact count: the trail frame's
    // own registration transform is identity only to within what the
    // star fit resolves, and a sub-pixel warp leaves the outermost
    // column without coverage — a sample with no coverage is not a
    // rejection (the engine's own rule), so it never enters the
    // first-pass bitmap the filter reads. Measured shortfall: 8 px of
    // 1728.
    let expected_forced = 9.0 * STAR_FIELD_WIDTH as f64
        / (6.0 * STAR_FIELD_WIDTH as f64 * STAR_FIELD_HEIGHT as f64);
    assert!(
        (forced - expected_forced).abs() / expected_forced < 0.01,
        "forced fraction {forced} != {expected_forced} (within 1 %)"
    );

    // The Integrate stage reported TWICE the planes (one pass each) and
    // still reached 100 %.
    let integrate_events: Vec<_> = recorder
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["stage"] == "integrate")
        .collect();
    assert!(!integrate_events.is_empty(), "no integrate progress events");
    let last = integrate_events.last().unwrap();
    assert_eq!(
        last["total"].as_u64(),
        Some(2),
        "one mono plane x two passes: {integrate_events:?}"
    );
    assert_eq!(last["current"].as_u64(), Some(2), "{integrate_events:?}");

    // The per-run temporaries — `.rej` AND the `.rejl` siblings — are
    // gone with the rest of the run's `rej/run-<id>` tree.
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert!(
        !layout.rej_run_dir(run_on.run_id).exists(),
        "rej/run-<id> (with its .rejl siblings) must be removed after the run"
    );

    let reader_off = PlaneReader::open(Path::new(&master_off)).unwrap();
    let reader_on = PlaneReader::open(Path::new(&master_on)).unwrap();
    assert_eq!(reader_off.channels(), 1);
    assert_eq!(reader_on.width(), reader_off.width());
    let plane_off = reader_off.read_plane(0).unwrap();
    let plane_on = reader_on.read_plane(0).unwrap();
    let w = reader_off.width();
    let row_mean = |data: &[f32], y: usize| -> f64 {
        data[y * w..(y + 1) * w]
            .iter()
            .map(|&v| v as f64)
            .sum::<f64>()
            / w as f64
    };
    // A star-free control row from the same strip as the trail.
    let control_off = row_mean(&plane_off, 108);
    let control_on = row_mean(&plane_on, 108);
    assert!(
        ((control_on - control_off) / control_off).abs() < 0.01,
        "the control row must not move: {control_on} vs {control_off}"
    );
    // The headline comparison is over ALL FOUR shoulder rows together
    // (768 px): a single row's mean carries ≈ 0.3 % of master noise
    // (30 ADU per frame over five surviving frames, 192 px), so a
    // 1 % per-row bound would sit only ~3 sigma clear of it. Measured
    // on this fixture: without 11.70 %, with 0.40 % over the four rows
    // (row 113 alone: without 9.98 %, with 0.54 %).
    let shoulder_rows: Vec<usize> = TRAIL_SHOULDER_LOW.chain(TRAIL_SHOULDER_HIGH).collect();
    let band_mean = |data: &[f32], control: f64| -> f64 {
        let sum: f64 = shoulder_rows.iter().map(|&y| row_mean(data, y)).sum();
        (sum / shoulder_rows.len() as f64 - control) / control
    };
    let without_band = band_mean(&plane_off, control_off);
    let with_band = band_mean(&plane_on, control_on);
    assert!(
        without_band > 0.05,
        "without large-scale rejection the shoulder must be >= 5 % high, got {:.2} %",
        without_band * 100.0
    );
    assert!(
        with_band.abs() < 0.01,
        "with large-scale rejection the shoulder must be within 1 % of the other frames' \
             level, got {:.2} %",
        with_band * 100.0
    );
    for y in shoulder_rows.iter().copied() {
        let without = (row_mean(&plane_off, y) - control_off) / control_off;
        let with = (row_mean(&plane_on, y) - control_on) / control_on;
        assert!(
            without > 0.05,
            "row {y}: without large-scale rejection the shoulder must be >= 5 % high, \
                 got {:.2} %",
            without * 100.0
        );
        assert!(
            with.abs() < 0.02,
            "row {y}: with large-scale rejection the shoulder must land at the other \
                 frames' level, got {:.2} %",
            with * 100.0
        );
    }
    // The core rows are rejected per-pixel either way.
    for y in TRAIL_CORE {
        let without = (row_mean(&plane_off, y) - control_off) / control_off;
        let with = (row_mean(&plane_on, y) - control_on) / control_on;
        assert!(
            without.abs() < 0.02 && with.abs() < 0.02,
            "row {y}: the core is rejected either way: {without} vs {with}"
        );
    }
}

/// Final fix wave: a large-scale run with drizzle OFF writes NO
/// `pass2/` directory and warns about nothing.
///
/// The second pass's own bitmap set exists for one reader — the Drizzle
/// stage, which must see the bits the returned master was built with
/// (ruling R-T3-1). With drizzle off nothing opens it, so writing it is
/// ≈ 3.3 MB per frame per plane at 26 Mpx (≈ 600 MB on a 208-frame
/// group) of pure waste; and `second_pass_rej_ok` coming back false
/// used to push a run warning saying "drizzle skipped … the second
/// pass's rejection bitmaps are unavailable" about a drizzle that was
/// never going to run.
///
/// `KeepAll` so the run's own `rej/` tree survives for this test to
/// look inside it: the first pass's `.rej` files and their `.rejl`
/// siblings must BOTH be there (the filter needs them), and `pass2/`
/// must not exist at all.
#[test]
fn large_scale_with_drizzle_off_writes_no_second_pass_set_and_no_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, _output) = seed_trail_group(&db_path, SET_NAME, 2);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.integration.rejection = RejectionChoice::PercentileClip {
        low: 0.2,
        high: 1.0,
    };
    cfg.integration.large_scale = LargeScaleRejection {
        enabled: true,
        protected_layers: 2,
        growth: 2,
    };
    // Drizzle off — the whole point of the pin.
    assert!(!cfg.drizzle.enabled, "the default must be drizzle off");
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("the run should start");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    assert!(group.master_path.is_some(), "the master must be written");
    assert!(
        group.drizzle_path.is_none(),
        "drizzle never ran: {:?}",
        group.drizzle_path
    );
    let stats: GroupStats = serde_json::from_str(group.stats_json.as_deref().unwrap()).unwrap();
    assert!(
        stats.large_scale_rejected_fraction.is_some(),
        "the second pass must have run: {stats:?}"
    );

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert!(
        !summary
            .warnings
            .iter()
            .any(|w| w.contains("drizzle") || w.contains("rejection bitmaps")),
        "no drizzle or bitmap warning belongs on a drizzle-off run: {:?}",
        summary.warnings
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let rej_dir = layout.rej_dir(started.run_id, &group.group_key);
    let names: Vec<String> = std::fs::read_dir(&rej_dir)
        .unwrap_or_else(|e| panic!("reading {rej_dir:?}: {e}"))
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with(".rej")),
        "the first pass's bitmaps must be there: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with(".rejl")),
        "the filter's processed siblings must be there: {names:?}"
    );
    assert!(
        !rej_dir.join(crate::stacking::rej::SECOND_PASS_DIR).exists(),
        "no pass2/ directory with drizzle off: {names:?}"
    );
}

/// Fix round 1 (ruling R-T3-1): WHICH bitmaps the Drizzle stage is
/// handed. With the large-scale second pass's own set present, its
/// files — the master's real rejected set; without it, the first
/// pass's, exactly as M3 did.
#[test]
fn drizzle_reads_the_second_passs_bitmaps_when_the_large_scale_pass_wrote_them() {
    let dir = tempfile::tempdir().unwrap();
    let stems = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let set = crate::stacking::rej::RejBitmapSet::create(dir.path(), &stems, 32, 8, 1).unwrap();

    let with_second = drizzle_rejection_paths(Some(&set), 3, true);
    assert_eq!(with_second.len(), 3);
    for (k, stem) in stems.iter().enumerate() {
        let path = with_second[k].as_ref().expect("a path per frame");
        assert_eq!(path, &set.second_pass_path(k));
        assert_eq!(
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str()),
            Some(crate::stacking::rej::SECOND_PASS_DIR),
            "the second pass's own directory, not the first pass's"
        );
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some(format!("{stem}.rej").as_str()),
            "same stem as the first pass's file"
        );
    }

    // M3's path, untouched: no second pass (or no usable set from it).
    let without = drizzle_rejection_paths(Some(&set), 3, false);
    for k in 0..3 {
        assert_eq!(without[k].as_deref(), Some(set.path(k)));
    }
    // And no bitmaps at all when the run never made a set.
    assert_eq!(
        drizzle_rejection_paths(None, 3, true),
        vec![None, None, None]
    );
}

/// Fix round 1 (ruling R-T3-1) end to end: drizzle and large-scale
/// rejection ON TOGETHER — the acceptance's own variant D. The run must
/// succeed, the second pass's bitmap set must be on disk (one file per
/// included frame, in `pass2/`, the same size as the first pass's), and
/// drizzle must have consumed it without being skipped: it opens every
/// path it is handed, so a drizzled master plus no skip warning is the
/// behavioural proof that those files are what it read.
#[test]
fn drizzle_and_large_scale_together_use_the_second_passs_bitmaps() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, _output) = seed_trail_group(&db_path, SET_NAME, 2);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.integration.rejection = RejectionChoice::PercentileClip {
        low: 0.2,
        high: 1.0,
    };
    cfg.integration.large_scale = LargeScaleRejection {
        enabled: true,
        protected_layers: 2,
        growth: 2,
    };
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.use_rejection = true;
    // KeepAll so the per-run bitmaps survive for this test to read.
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("the run should start");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    let drizzle_path = group
        .drizzle_path
        .clone()
        .expect("drizzle ran with the second pass's bitmaps");
    assert!(Path::new(&drizzle_path).exists(), "{drizzle_path}");

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert!(
        !summary
            .warnings
            .iter()
            .any(|w| w.contains("drizzle skipped")),
        "drizzle must not have been skipped: {:?}",
        summary.warnings
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let rej_dir = layout.rej_dir(started.run_id, &group.group_key);
    let pass2 = rej_dir.join(crate::stacking::rej::SECOND_PASS_DIR);
    let mut pass2_files: Vec<String> = std::fs::read_dir(&pass2)
        .unwrap_or_else(|e| panic!("reading {pass2:?}: {e}"))
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    pass2_files.sort();
    assert_eq!(
        pass2_files.len(),
        group.included_count as usize,
        "one second-pass bitmap per included frame: {pass2_files:?}"
    );
    for name in &pass2_files {
        let first = rej_dir.join(name);
        assert!(first.exists(), "the first pass's sibling must exist too");
        assert_eq!(
            std::fs::metadata(pass2.join(name)).unwrap().len(),
            std::fs::metadata(&first).unwrap().len(),
            "both passes' sets have the same geometry, so the same file size"
        );
    }
}

// ── M4d Task 3: every written output is cataloged ───────────────────

/// Ruling R-M4d-4: stage 9 records one `master_lights` row per output it
/// actually WROTE — the master, and with drizzle + `writeWeightMap` on,
/// the drizzled master and that drizzle's weight map. Every geometry
/// column is checked against the FILE the writer produced (not against
/// the config that asked for it), `frames` is the group's own included
/// count, and `total_exposure_s` is the plain sum of those frames'
/// exposures — 60 s each in this fixture.
#[test]
fn a_run_catalogs_every_written_master_light() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.use_rejection = true;
    cfg.drizzle.write_weight_map = true;
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    assert_eq!(group.status, "done", "{group:?}");

    let rows = crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["master", "drizzle", "weight_map"],
        "one row per written output, in write order: {rows:?}"
    );

    let master = rows.iter().find(|r| r.kind == "master").unwrap();
    assert_eq!(master.subject, StackSubject::FrameSet(fixture.set_id));
    assert_eq!(master.run_id, started.run_id);
    assert_eq!(master.group_key, group.group_key);
    assert_eq!(
        Some(master.path.clone()),
        group.master_path,
        "the row must name the same file the group row does"
    );
    assert_eq!(master.format, "fits");
    assert_eq!(master.frames, group.included_count);
    assert_eq!(
        master.total_exposure_s,
        Some(60.0 * group.included_count as f64),
        "{master:?}"
    );
    assert!(!master.created_at.is_empty());
    let master_reader = PlaneReader::open(Path::new(&master.path)).unwrap();
    assert_eq!(master.width, master_reader.width() as i64);
    assert_eq!(master.height, master_reader.height() as i64);
    assert_eq!(master.channels, master_reader.channels() as i64);

    let drizzle = rows.iter().find(|r| r.kind == "drizzle").unwrap();
    assert_eq!(Some(drizzle.path.clone()), group.drizzle_path);
    assert_eq!(drizzle.frames, master.frames);
    assert_eq!(drizzle.total_exposure_s, master.total_exposure_s);
    let drizzle_reader = PlaneReader::open(Path::new(&drizzle.path)).unwrap();
    assert_eq!(drizzle.width, drizzle_reader.width() as i64);
    assert_eq!(drizzle.height, drizzle_reader.height() as i64);
    assert_eq!(drizzle.channels, drizzle_reader.channels() as i64);
    assert_eq!(drizzle.width, 2 * master.width, "scale 2 on the long axis");
    assert_eq!(drizzle.height, 2 * master.height);

    let weight = rows.iter().find(|r| r.kind == "weight_map").unwrap();
    assert_ne!(weight.path, drizzle.path);
    let weight_reader = PlaneReader::open(Path::new(&weight.path)).unwrap();
    assert_eq!(weight.width, weight_reader.width() as i64);
    assert_eq!(weight.height, weight_reader.height() as i64);
    assert_eq!(
        (weight.width, weight.height),
        (drizzle.width, drizzle.height),
        "a weight map shares its drizzle's geometry"
    );

    // The point lookup `render_master_light_preview` resolves a path with.
    let found = crate::db::stacking::find_master_light(
        &fixture.conn,
        started.run_id,
        &group.group_key,
        "master",
    )
    .unwrap()
    .expect("the master row is findable by (run, group, kind)");
    assert_eq!(found.id, master.id);
}

// ── M3 Task 5: the Drizzle stage wired into the run ─────────────────

/// Brief test (a): drizzle 2x on, `useRejection` on, `cleanup =
/// deleteIntermediates` — the run succeeds, the group row's
/// `drizzle_path` names an existing file of `out_w = 2*W`,
/// `out_h = 2*H`, `summary.groups[0].drizzle.scale == 2`, `stages`
/// contains `drizzle` between `integrate` and `output`, `rej/run-<id>`
/// does NOT exist after the run, `stacking-complete`'s
/// `masters[0].drizzlePath` is `Some`.
#[test]
fn drizzle_writes_a_scaled_master_and_removes_rej_after_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.use_rejection = true;
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;

    let recorder = Arc::new(Recording::new());
    let started = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    let master_path = group.master_path.clone().expect("master path recorded");
    let drizzle_path = group.drizzle_path.clone().expect("drizzle path recorded");
    assert!(
        Path::new(&drizzle_path).exists(),
        "drizzled master missing: {drizzle_path}"
    );

    let master_reader = PlaneReader::open(Path::new(&master_path)).unwrap();
    let drizzle_reader = PlaneReader::open(Path::new(&drizzle_path)).unwrap();
    assert_eq!(drizzle_reader.width(), 2 * master_reader.width());
    assert_eq!(drizzle_reader.height(), 2 * master_reader.height());

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let group_summary = &summary.groups[0];
    assert_eq!(
        group_summary
            .drizzle
            .as_ref()
            .expect("drizzle stats recorded")
            .scale,
        2
    );

    // Fix round 1, Important I1: real per-frame progress — the Drizzle
    // stage's own `stacking-progress` events must reach a genuine
    // `current == total` completion, not just the forced starting
    // `0/total` tick (a no-op `on_frame` would leave the LAST recorded
    // `current` at 0, exactly what a live progress bar user sees as
    // "stuck at 0%").
    let drizzle_events: Vec<_> = recorder
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["stage"] == "drizzle")
        .collect();
    assert!(
        !drizzle_events.is_empty(),
        "expected at least one drizzle-stage progress event"
    );
    let expected_total = (group.included_count as usize) * master_reader.channels();
    let last = drizzle_events.last().expect("checked non-empty above");
    assert_eq!(
        last["total"].as_u64(),
        Some(expected_total as u64),
        "{drizzle_events:?}"
    );
    assert_eq!(
        last["current"].as_u64(),
        Some(expected_total as u64),
        "the last drizzle progress event must reach current == total: {drizzle_events:?}"
    );

    let integrate_idx = summary
        .stages
        .iter()
        .position(|t| t.stage == Stage::Integrate)
        .expect("Integrate stage timing");
    let drizzle_idx = summary
        .stages
        .iter()
        .position(|t| t.stage == Stage::Drizzle)
        .expect("Drizzle stage timing");
    let output_idx = summary
        .stages
        .iter()
        .position(|t| t.stage == Stage::Output)
        .expect("Output stage timing");
    assert!(
        integrate_idx < drizzle_idx && drizzle_idx < output_idx,
        "expected integrate < drizzle < output, got {:?}",
        summary.stages
    );

    // B2 (M3 final fix wave, I2): Drizzle and Output must be DISJOINT
    // spans, not the Output timer including the whole drizzle duration
    // — checked against the run's own wall time rather than requiring
    // `output.duration_ms < drizzle.duration_ms` (not a real relation
    // on this fixture; only that the two never sum past the run's
    // actual wall-clock span).
    let drizzle_ms = summary.stages[drizzle_idx].duration_ms;
    let output_ms = summary.stages[output_idx].duration_ms;
    let run_started_at = chrono::DateTime::parse_from_rfc3339(&summary.started_at)
        .expect("started_at parses as RFC3339");
    let run_finished_at = chrono::DateTime::parse_from_rfc3339(
        summary
            .finished_at
            .as_deref()
            .expect("a completed run has finished_at"),
    )
    .expect("finished_at parses as RFC3339");
    let wall_ms = (run_finished_at - run_started_at).num_milliseconds().max(0) as u64;
    assert!(
        output_ms + drizzle_ms <= wall_ms,
        "output_ms={output_ms} drizzle_ms={drizzle_ms} wall_ms={wall_ms} — Output must not \
             double-count drizzle's own duration"
    );

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert!(
        !layout.rej_run_dir(started.run_id).exists(),
        "rej/run-<id> must be removed after the run"
    );

    let completes = recorder.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    let masters = completes[0]["masters"].as_array().unwrap();
    assert_eq!(masters.len(), 1, "{masters:?}");
    assert_eq!(
        masters[0]["drizzlePath"].as_str(),
        Some(drizzle_path.as_str())
    );
}

// ── M4d Task 1: the CFA mosaic artifact and Bayer drizzle ────────────

/// [`seed_star_group`] with a declared `RGGB` mosaic on every frame, so
/// `group_frames` puts them in ONE **OSC** group and stage 1 debayers
/// them. The pixels are the same star field the mono fixtures use — a
/// mosaic the debayer reads as one (every site carries the field's own
/// value), which is all the Bayer deposit needs: the per-colour routing
/// itself is pinned on a real per-colour fixture in
/// `drizzle::tests::bayer_drizzle_*`.
fn seed_osc_star_group(
    db_path: &Path,
    set_name: &str,
    shifts: &[(f64, f64)],
    noise_sigmas: &[f32],
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    assert_eq!(shifts.len(), noise_sigmas.len());
    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for (i, (&(dx, dy), &sigma)) in shifts.iter().zip(noise_sigmas.iter()).enumerate() {
        let stars = shifted_stars(dx, dy);
        let date_obs = date_obs_at(i);
        let stem = format!("f{i}");
        let spec = test_fixtures::LightSpec {
            bayerpat: Some("RGGB"),
            ..star_light_spec(&stem, &date_obs)
        };
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            600.0,
            sigma,
            100 + i as u64,
        );
        light_ids.push(id);
    }

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

    (fixture, light_ids, working, output)
}

/// Step 3's run test (rulings R-M4d-1/2), end to end on an OSC group:
/// stage 1 keeps one `calibrated_mosaic` artifact per frame — a
/// single-plane file of the frame's own geometry, beside the debayered
/// `_d` sibling — the Drizzle stage deposits from those mosaics (the
/// drizzled R plane's level matches the master's own R plane, i.e. the
/// mask is level-preserving in a real run), and
/// `CleanupWhat::Intermediates` — what `cleanup = deleteIntermediates`
/// maps to — removes the rows with the calibrated frames.
#[test]
fn bayer_drizzle_keeps_a_mosaic_per_frame_and_deposits_from_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [
        (0.0, 0.0),
        (3.0, 0.0),
        (0.0, 3.0),
        (2.0, 2.0),
        (-2.0, 1.0),
        (1.0, -2.0),
    ];
    let noise = [5.0f32; 6];
    let (fixture, light_ids, working, _output) =
        seed_osc_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.bayer = true;
    // KeepAll so this test can inspect the mosaics, then clean up itself.
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(Recording::new()),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    assert!(
        group.group_key.contains("osc"),
        "the fixture must make an OSC group: {}",
        group.group_key
    );

    // One mosaic artifact per calibrated frame, each a single-plane file
    // of the frame's own geometry beside its `_d` sibling.
    let calibrated = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    let mosaics = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated_mosaic"),
    )
    .unwrap();
    assert_eq!(
        mosaics.len(),
        calibrated.len(),
        "one mosaic per calibrated frame: {mosaics:?}"
    );
    assert_eq!(mosaics.len(), light_ids.len(), "{mosaics:?}");
    for m in &mosaics {
        let path = m.path.clone().expect("a mosaic artifact names its file");
        let reader = PlaneReader::open(Path::new(&path)).unwrap();
        assert_eq!(reader.channels(), 1, "the mosaic is one plane: {path}");
        assert_eq!(
            (reader.width(), reader.height()),
            (STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT)
        );
        let name = Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap();
        assert!(
            name.starts_with("c_") && !name.ends_with("_d.fits"),
            "the mosaic takes the mono spelling: {name}"
        );
        let debayered = Path::new(&path).with_file_name(name.replace(".fits", "_d.fits"));
        assert!(
            debayered.exists(),
            "the debayered sibling must be there too: {}",
            debayered.display()
        );
    }

    // The deposit read those mosaics, and `I / W` is still
    // level-preserving through the colour mask: the drizzled R plane's
    // median matches the master's own R plane median.
    let master_path = group.master_path.clone().expect("master written");
    let drizzle_path = group.drizzle_path.clone().expect("drizzle ran");
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert!(
        !summary.warnings.iter().any(|w| w.contains("Bayer drizzle")),
        "every frame should have had a mosaic: {:?}",
        summary.warnings
    );
    let master = PlaneReader::open(Path::new(&master_path)).unwrap();
    assert_eq!(master.channels(), 3, "an OSC master has three planes");
    let drizzled = PlaneReader::open(Path::new(&drizzle_path)).unwrap();
    assert_eq!(drizzled.channels(), 3);
    assert_eq!(drizzled.width(), 2 * master.width());

    // The signature of the colour mask, and the proof the deposit really
    // came from the mosaics: R and B carry a QUARTER of a mosaic's sites
    // each against G's half, so their output coverage is strictly lower.
    // A deposit from the debayered planes covers all three identically.
    let stats = summary.groups[0]
        .drizzle
        .as_ref()
        .expect("drizzle stats recorded");
    assert_eq!(stats.coverage.len(), 3, "{:?}", stats.coverage);
    assert!(
        stats.coverage[0] < stats.coverage[1] && stats.coverage[2] < stats.coverage[1],
        "R and B must cover less of the grid than G: {:?}",
        stats.coverage
    );

    let median_of = |mut v: Vec<f32>| -> f64 {
        v.retain(|x| x.is_finite() && *x > 0.0);
        assert!(!v.is_empty(), "nothing deposited");
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2] as f64
    };
    for plane in 0..3usize {
        let m = median_of(master.read_plane(plane).unwrap());
        let d = median_of(drizzled.read_plane(plane).unwrap());
        assert!(
            (d - m).abs() <= 0.01 * m.abs(),
            "plane {plane}: drizzled median {d} is not within 1% of the master's {m}"
        );
    }

    // `deleteIntermediates` takes the mosaic rows with the calibrated
    // ones (`INTERMEDIATE_ARTIFACT_KINDS`).
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    cleanup_work(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        &layout,
        CleanupWhat::Intermediates,
    )
    .unwrap();
    assert!(
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated_mosaic")
        )
        .unwrap()
        .is_empty(),
        "the mosaic rows must go with the calibrated frames"
    );
    assert!(!layout.calibrated_root().exists());
}

/// Step 3's cache rule (ruling R-M4d-1): a frame calibrated by an
/// earlier run that wanted no mosaic is NOT reused as-is once
/// `drizzle.bayer` goes on — the run regenerates the pair in one
/// generation. And it costs nothing the other way round: the calibrated
/// artifact's own hash is unchanged by the toggle, so a set does not
/// recalibrate when the mosaic is already there.
#[test]
fn turning_bayer_on_regenerates_the_mosaic_of_an_already_calibrated_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_osc_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let run_with_bayer = |bayer: bool| {
        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 1;
        cfg.drizzle.bayer = bayer;
        cfg.output.cleanup = CleanupPolicy::KeepAll;
        let started = start_stacking(
            ctx.clone(),
            Arc::new(Recording::new()),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
    };
    let hashes = || -> Vec<(Option<i64>, String)> {
        let mut rows = crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated"),
        )
        .unwrap()
        .into_iter()
        .map(|a| (a.frame_id, a.config_hash))
        .collect::<Vec<_>>();
        rows.sort();
        rows
    };

    run_with_bayer(false);
    let before = hashes();
    assert_eq!(before.len(), light_ids.len(), "{before:?}");
    assert!(
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated_mosaic")
        )
        .unwrap()
        .is_empty(),
        "no mosaic is kept while bayer is off"
    );

    run_with_bayer(true);
    let mosaics = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated_mosaic"),
    )
    .unwrap();
    assert_eq!(
        mosaics.len(),
        light_ids.len(),
        "every already-calibrated frame regenerates its mosaic: {mosaics:?}"
    );
    for m in &mosaics {
        assert!(Path::new(&m.path.clone().unwrap()).exists());
    }
    assert_eq!(
        hashes(),
        before,
        "drizzle.bayer must not change a calibrated frame's own hash"
    );
}

/// Fix round 1 (I1): `bayer` on with the OSC debayer OFF must not want a
/// mosaic no generation can produce. Before the fix, `want_mosaic` was
/// true, the generator wrote nothing, the artifact never appeared and the
/// group recalibrated from scratch on EVERY run, with only a `debug!`.
/// Now the run says so once, keeps no mosaic, and the second run reuses
/// every calibrated frame.
#[test]
fn bayer_with_the_debayer_off_says_so_once_and_stays_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_osc_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let cfg = || {
        let mut c = StackingConfig::default();
        c.drizzle.enabled = true;
        c.drizzle.scale = 1;
        c.drizzle.bayer = true;
        c.calibration.debayer_osc = false;
        c.output.cleanup = CleanupPolicy::KeepAll;
        c
    };
    let run_once = || -> RunSummary {
        let started = start_stacking(
            ctx.clone(),
            Arc::new(Recording::new()),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg()),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap()
    };

    let first = run_once();
    let expected = "Bayer drizzle is ignored: the OSC debayer is off for this run, so no \
                        calibrated CFA mosaic is kept and drizzle deposits each frame's \
                        calibrated planes as they are.";
    assert!(
        first.warnings.iter().any(|w| w == expected),
        "the contradiction must be reported once: {:?}",
        first.warnings
    );
    assert_eq!(
        first
            .warnings
            .iter()
            .filter(|w| w.starts_with("Bayer drizzle is ignored"))
            .count(),
        1,
        "once per run, not once per group or frame: {:?}",
        first.warnings
    );
    assert!(
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated_mosaic")
        )
        .unwrap()
        .is_empty(),
        "no mosaic can exist while the debayer is off"
    );

    let second = run_once();
    let frames: Vec<&SummaryFrame> =
        second.groups.iter().flat_map(|g| g.frames.iter()).collect();
    assert_eq!(frames.len(), light_ids.len(), "{frames:?}");
    assert!(
        frames.iter().all(|f| f.cached_calibrated),
        "the second run must reuse every calibrated frame: {:?}",
        frames
            .iter()
            .map(|f| (f.frame_id, f.cached_calibrated))
            .collect::<Vec<_>>()
    );
}

/// Fix round 1 (I2): the plan gate and the run must agree about what the
/// first `bayer` run will do. A fully calibrated set used to report
/// Calibrate fresh and `calibratedCached = N` while the run was about to
/// regenerate all N pairs — and only for the OSC group, which is what
/// makes the mono half of this set the control.
#[test]
fn the_plan_reports_the_mosaics_a_bayer_run_still_owes() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    // One OSC group and one mono group in the same set (the bucket key's
    // own colour axis splits them), four frames each.
    let fixture_conn = rusqlite::Connection::open(&db_path).unwrap();
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let mut osc_ids = Vec::new();
    let mut mono_ids = Vec::new();
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        for (osc, ids) in [(true, &mut osc_ids), (false, &mut mono_ids)] {
            let date_obs = date_obs_at(i);
            let stem = format!("{}{i}", if osc { "osc" } else { "mono" });
            let spec = test_fixtures::LightSpec {
                bayerpat: if osc { Some("RGGB") } else { None },
                ..star_light_spec(&stem, &date_obs)
            };
            let (id, _path) = test_fixtures::add_light_with_field(
                &fixture,
                &spec,
                &shifted_stars(dx, dy),
                600.0,
                5.0,
                100 + i as u64,
            );
            ids.push(id);
        }
    }
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
    let all_ids: Vec<i64> = osc_ids.iter().chain(mono_ids.iter()).copied().collect();
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &all_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let cfg = |bayer: bool| {
        let mut c = StackingConfig::default();
        c.drizzle.enabled = true;
        c.drizzle.scale = 1;
        c.drizzle.bayer = bayer;
        c.output.cleanup = CleanupPolicy::KeepAll;
        c
    };
    let run_with = |bayer: bool| {
        let started = start_stacking(
            ctx.clone(),
            Arc::new(Recording::new()),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg(bayer)),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
    };
    let plan_with = |bayer: bool| {
        build_plan(
            &fixture.conn,
            &ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(fixture.set_id),
            Some(cfg(bayer)),
        )
        .expect("the plan should build")
    };
    let group_of = |plan: &crate::stacking::plan::StackingPlan, osc: bool| {
        plan.groups
            .iter()
            .find(|g| g.color_mode == if osc { ColorMode::Osc } else { ColorMode::Mono })
            .expect("both groups planned")
            .clone()
    };

    // Calibrated by a run that wanted no mosaic.
    run_with(false);
    let plan_off = plan_with(false);
    assert!(
        !plan_off.stale_stages.contains(&Stage::Calibrate),
        "a non-bayer plan over a calibrated set is fresh: {:?}",
        plan_off.stale_stages
    );
    assert_eq!(group_of(&plan_off, true).calibrated_cached, 4);
    assert_eq!(group_of(&plan_off, false).calibrated_cached, 4);

    // The same set, planned with Bayer drizzle on: the OSC group owes
    // four mosaics, the mono group owes nothing.
    let plan_on = plan_with(true);
    assert!(
        plan_on.stale_stages.contains(&Stage::Calibrate),
        "the OSC group's missing mosaics make Calibrate stale: {:?}",
        plan_on.stale_stages
    );
    assert_eq!(
        group_of(&plan_on, true).calibrated_cached,
        0,
        "every OSC frame is regenerated as a pair"
    );
    assert_eq!(
        group_of(&plan_on, false).calibrated_cached,
        4,
        "the mono group ignores drizzle.bayer entirely"
    );

    // And after the bayer run, fresh again.
    run_with(true);
    let plan_after = plan_with(true);
    assert!(
        !plan_after.stale_stages.contains(&Stage::Calibrate),
        "{:?}",
        plan_after.stale_stages
    );
    assert_eq!(group_of(&plan_after, true).calibrated_cached, 4);
    assert_eq!(group_of(&plan_after, false).calibrated_cached, 4);
}

/// Ruling R-T4-6d: a whole `tps` run — registration, LN, integration
/// and drizzle — must never hold more than a handful of displacement
/// grids at once, however many frames it has.
///
/// This is the acceptance-run failure in miniature. Run 27 (208 mono
/// frames at 6224×4168, `distortion: "tps"`, LN on, drizzle 2×)
/// registered, normalized and integrated in 11 minutes and then sat at
/// 0 % CPU for 2.7 hours with 11.3 of 12 GB of swap in use: every
/// frame's INVERSE grid stayed alive from the registration writer
/// onward (the clones share one cache, so nothing freed it), and
/// drizzle then built a FORWARD grid per frame on top.
///
/// `PEAK_ALIVE` is the number that matters — grids resident at the
/// same moment, not grids built over the run's life. Rebuilding is
/// cheap and expected; 2 × frame_count resident is not.
#[test]
fn a_tps_run_never_holds_more_than_a_few_displacement_grids_at_once() {
    use crate::geometry::pixel_map::grid_counters;

    // The counters are process-global; this holds the harness's other
    // grid-building tests off for the measurement window.
    let _quiet = grid_counters::exclusive();

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    // 20 frames — the ruling's own figure, and enough that "one grid
    // per frame" and "a handful" cannot be confused. The field has to
    // be DENSE: the spline arm needs `TPS_MIN_INLIERS` (32) inliers
    // before it fits at all, and LN needs 20 matched stars, so the
    // 10-star `BASE_STARS` field every other run test uses would leave
    // this one measuring a pipeline that built no grid.
    // 56 stars on an 8x7 lattice, amplitudes in [`BASE_STARS`]' own ADU
    // range, spaced 23x19 px — far enough apart on this 192x144 canvas
    // that their supra-threshold regions do not merge (the hazard
    // `STAR_FIELD_WIDTH`'s own doc records). Each position carries a
    // deterministic ±5 px jitter: a PERFECTLY regular lattice is
    // degenerate for the quad matcher (measured: 160–4834 ambiguous
    // quad matches per frame and not one usable affine), which is its
    // own small lesson about synthetic star fields.
    let dense = |dx: f64, dy: f64| -> Vec<(f64, f64, f64)> {
        let mut stars = Vec::new();
        for row in 0..7 {
            for col in 0..8 {
                let k = (row * 8 + col) as f64;
                let jx = (k * 2.399963).sin() * 4.0;
                let jy = (k * 1.618034).cos() * 4.0;
                let x = 13.0 + col as f64 * 23.0 + jx + dx;
                let y = 14.0 + row as f64 * 19.0 + jy + dy;
                let amp = 6000.0 + ((row * 8 + col) % 5) as f64 * 875.0;
                stars.push((x, y, amp));
            }
        }
        stars
    };
    let fields: Vec<(Vec<(f64, f64, f64)>, f32)> = (0..20)
        .map(|i| (dense((i % 5) as f64 * 1.5, (i / 5) as f64 * 1.5), 5.0f32))
        .collect();
    let (fixture, light_ids, _working, _output) =
        seed_star_group_with_fields(&db_path, SET_NAME, &fields);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
    cfg.registration.local_distortion = true;
    // Perf tier A Task 6a: the registered artifact is written
    // unconditionally now, so `write_registered_frames` no longer needs
    // setting here for the writer stage's own grid build/release below
    // to run.
    //
    // LN stays OFF here: its PSF-flux scale needs 20 matched stars per
    // frame and this 192x144 canvas cannot carry that many resolvable
    // ones (measured: 3–6 matched, every frame excluded). Nothing is
    // lost for THIS property — LN warps through `RegisteredSource`,
    // whose `Drop` is the same release path integration exercises
    // below, and `local_normalization_sidecars_are_cached_on_the_second_run`
    // covers LN's own pipeline.
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;

    grid_counters::reset();
    let recorder = Arc::new(Recording::new());
    let started = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run_within(&ctx, started.run_id, Duration::from_secs(180));

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let (builds, _alive, peak) = grid_counters::snapshot();
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let tps_frames = summary.groups[0]
        .frames
        .iter()
        .filter(|f| f.reg_model.as_deref().is_some_and(|m| m.contains("+tps")))
        .count();
    assert!(
        tps_frames >= 8,
        "the spline arm must actually have fitted: {tps_frames} frames carry +tps"
    );
    // The run really did use the spline arm — otherwise the rest of
    // this test would pass on a pipeline that never built a grid.
    assert!(
        builds > 0,
        "a tps run must have built displacement grids at all"
    );
    // There is deliberately NO `alive == 0` assertion (fix round 3,
    // finding 1): the thread's `RunHandleGuard` now genuinely drops
    // last, which means `rc` — every map, every grid — is already gone
    // by the time `wait_for_run` observes the de-registration. Such an
    // assertion could not fail however many releases were removed, so
    // it would have measured teardown rather than the releases. What
    // the releases are pinned by is `peak` below and the three
    // per-stage tests (`writer`, `registered_source`, `drizzle`).
    // Where the peak comes from — traced build by build (v0.6.3 CI
    // fix, `GRID_TRACE` instrumentation on `grid_counters`): drizzle
    // still processes one frame at a time on the run thread (build,
    // warp, release, next frame). Before perf tier A Task 6a,
    // `integrate_planes` opened ONE `RegisteredSource` per group
    // (ruling R-T4-7) and every band read every included frame through
    // its inverse grid, so the group's spline frames' inverse grids
    // were all resident for the whole integration — one per frame, by
    // design (rebuilding 600-node splines per band would cost far more
    // than the ~3 MB a grid holds) — measured then: 33 grids built over
    // the run and a peak of exactly `tps_frames` (11), on a 10-worker
    // Mac AND on a 4-worker CI runner alike. Task 6a made Integrate (and
    // Normalize) read the materialized registered artifact instead —
    // when it is fresh (this test's ordinary case: LN is off and
    // nothing deletes the artifact) neither stage touches a
    // `PixelMap`'s grid at all, so the writer's own builds are what
    // remain: re-measured post-Task-6a at 22 builds (33 minus the 11
    // integration used to add) and a peak of 1.
    //
    // Fix round 2 (ruling R-TA-6 M8): fix round 1 moved the write from
    // a sequential post-fan-out loop into the register fan-out's own
    // worker closures, so it is NO LONGER true by construction that
    // "the writer holds one grid at a time" — up to `admission_n`
    // workers can each be mid-warp, each holding its own grid, at
    // once. Re-measured after that change (this run): still 22 builds,
    // peak 1 — a property of THIS fixture, not a guarantee the design
    // makes. On a 192x144 canvas with only 56 stars, one worker's
    // detection + RANSAC + distortion fit dominates its own per-frame
    // wall time far more than its brief warp-then-release window, so
    // two workers rarely land inside their OWN grid-holding moment at
    // the same instant even though nothing serializes them; a larger
    // real frame, where the warp itself is a much bigger share of a
    // worker's time, would be expected to show a peak closer to
    // `admission_n`. The actual bound the design guarantees is `peak
    // <= admission_n <= tps_frames` (never more resident grids than
    // in-flight register workers, and never more workers than there
    // are frames to register) — this assertion checks the frame-count
    // half of that, which is what R-T4-6 cares about (the 2x shape:
    // inverse grids surviving into drizzle's forward grids, what
    // thrashed acceptance run 27), not the sequential-writer claim
    // this comment used to make.
    //
    // The assertion this replaces read `peak <= threads + 4` — the
    // hypothesis that a per-frame release bounds residency by the POOL.
    // It held on the 10-worker dev Mac only because 11 <= 14, and
    // failed on every 4-worker GitHub runner (11 > 8) from v0.6.1 to
    // v0.6.2 — a wrong model of the integration source, not a leak.
    let threads = rayon::current_num_threads().max(1);
    eprintln!(
        "R-T4-6d measured: builds {builds} peak_alive {peak} threads {threads} \
             tps_frames {tps_frames}"
    );
    assert!(
        peak <= tps_frames,
        "peak resident grids {peak} over {tps_frames} spline frames (builds \
             {builds}, {threads} workers) — a stage is holding a second grid per \
             frame, or one stage's grids survived into the next"
    );
}

/// B10 (M3 final fix wave, M7): the persisted `RunSummary.config` must
/// record the CLAMPED `dropShrink`, not the pre-clamp value
/// `start_stacking` captured before `stage_output`'s own clamp ran —
/// otherwise the summary (and `runs/run-<id>.json`) disagree with the
/// value the run actually used and the drizzled master's own
/// `ATH_DRZP` card.
#[test]
fn drizzle_summary_config_records_the_clamped_drop_shrink() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.drop_shrink = 0.2; // out of [0.5, 1.0] — clamps to 0.5

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    assert!(
        row.summary_json
            .as_deref()
            .unwrap_or_default()
            .contains("drizzle drop shrink 0.2 out of [0.5, 1.0]; clamped to 0.5"),
        "{:?}",
        row.summary_json
    );

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        summary.config.drizzle.drop_shrink, 0.5,
        "the persisted config must record the CLAMPED value, not 0.2"
    );

    let group = &summary.groups[0];
    let stats = group
        .drizzle
        .as_ref()
        .expect("drizzle stats recorded for the group");
    assert_eq!(
        stats.drop_shrink, 0.5,
        "the group's own drizzle stats already agreed with the clamp"
    );
}

/// Brief test (b): same as above with `cleanup = keepAll` —
/// `rej/run-<id>/<group>/` holds `included` `.rej` files of the right
/// size (validated by reading each one back via `RejBitmap::read` at
/// the master's own geometry — a short/foreign file would refuse).
#[test]
fn drizzle_keep_all_leaves_rejection_bitmaps_of_the_right_size() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.use_rejection = true;
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let group = &groups[0];
    let included_count = group.included_count as usize;

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let rej_dir = layout.rej_dir(started.run_id, &group.group_key);
    assert!(
        rej_dir.exists(),
        "rej dir must survive keepAll: {rej_dir:?}"
    );

    let entries: Vec<_> = std::fs::read_dir(&rej_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), included_count, "{entries:?}");

    let master_path = group.master_path.clone().expect("master path");
    let master_reader = PlaneReader::open(Path::new(&master_path)).unwrap();
    for e in &entries {
        RejBitmap::read(
            &e.path(),
            master_reader.width(),
            master_reader.height(),
            master_reader.channels(),
        )
        .unwrap_or_else(|err| {
            panic!(
                "{:?} is not a valid .rej at the master's geometry: {err}",
                e.path()
            )
        });
    }
}

/// Brief test (c): drizzle on with `writeWeightMap` — `weight_map_path`
/// exists on disk.
#[test]
fn drizzle_write_weight_map_produces_a_weight_map_file() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
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
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let weight_map_path = summary.groups[0]
        .weight_map_path
        .clone()
        .expect("weight map path recorded");
    assert!(
        Path::new(&weight_map_path).exists(),
        "weight map missing: {weight_map_path}"
    );
}

/// Brief test (d): drizzle off adds no `drizzle` timing and no `rej/`;
/// the master's bytes are BIT-IDENTICAL to a run of the SAME fixture
/// with drizzle 2x on (the drizzle stage must never touch the master —
/// the M1/M2 pins stay green with drizzle wired in).
#[test]
fn drizzle_off_leaves_the_master_byte_identical_to_drizzle_on() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // Run 1: drizzle off (the M1/M2 default).
    let started_off = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("first run should succeed");
    wait_for_run(&ctx, started_off.run_id);
    let row_off = crate::db::stacking::get_run(&fixture.conn, started_off.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row_off.status, "done", "{row_off:?}");
    let summary_off: RunSummary =
        serde_json::from_str(row_off.summary_json.as_deref().unwrap()).unwrap();
    assert!(
        !summary_off.stages.iter().any(|t| t.stage == Stage::Drizzle),
        "{:?}",
        summary_off.stages
    );
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert!(
        !layout.rej_run_dir(started_off.run_id).exists(),
        "no rej/ when drizzle is off"
    );
    let groups_off =
        crate::db::stacking::list_groups(&fixture.conn, started_off.run_id).unwrap();
    let master_off = groups_off[0].master_path.clone().expect("first master");

    // Run 2: SAME fixture, drizzle 2x on — the master must not change.
    let mut cfg_on = StackingConfig::default();
    cfg_on.drizzle.enabled = true;
    cfg_on.drizzle.scale = 2;

    let started_on = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg_on),
        None,
    )
    .expect("second run should succeed");
    wait_for_run(&ctx, started_on.run_id);
    let row_on = crate::db::stacking::get_run(&fixture.conn, started_on.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row_on.status, "done", "{row_on:?}");
    let groups_on = crate::db::stacking::list_groups(&fixture.conn, started_on.run_id).unwrap();
    let master_on = groups_on[0].master_path.clone().expect("second master");
    assert_ne!(
        master_off, master_on,
        "a rerun must never overwrite the first master"
    );

    let reader_off = PlaneReader::open(Path::new(&master_off)).unwrap();
    let reader_on = PlaneReader::open(Path::new(&master_on)).unwrap();
    assert_eq!(reader_off.channels(), reader_on.channels());
    for p in 0..reader_off.channels() {
        let plane_off = reader_off.read_plane(p).unwrap();
        let plane_on = reader_on.read_plane(p).unwrap();
        assert_eq!(plane_off.len(), plane_on.len());
        for (i, (a, b)) in plane_off.iter().zip(plane_on.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "plane {p} pixel {i}: drizzle off {a} vs drizzle on {b} \u{2014} the drizzle stage must never touch the master"
            );
        }
    }
}

/// Spec §5.7, plan 6b-3 D9: the coverage map is a side output — a frame
/// set's master is bit-identical with it on and off — written as
/// `<stem>_coverage.fits` (one plane, `BUNIT = 'frames'`) whose counts are
/// the shift geometry, with its own `master_lights` row and summary path.
#[test]
fn a_frame_sets_master_is_bit_identical_with_the_coverage_map_on_and_off() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );
    let run_with = |cfg: Option<StackingConfig>| -> (i64, String) {
        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            cfg,
            None,
        )
        .expect("the run starts");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
        let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
        (
            started.run_id,
            groups[0].master_path.clone().expect("a master"),
        )
    };
    let (off_run, master_off) = run_with(None);
    let mut cfg_on = StackingConfig::default();
    cfg_on.output.coverage_map = true;
    let (on_run, master_on) = run_with(Some(cfg_on));

    let (reader_off, reader_on) = (
        PlaneReader::open(Path::new(&master_off)).unwrap(),
        PlaneReader::open(Path::new(&master_on)).unwrap(),
    );
    for p in 0..reader_off.channels() {
        let (a, b) = (
            reader_off.read_plane(p).unwrap(),
            reader_on.read_plane(p).unwrap(),
        );
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "plane {p} pixel {i}");
        }
    }
    assert!(
        crate::db::stacking::list_master_lights(&fixture.conn, off_run)
            .unwrap()
            .iter()
            .all(|m| m.kind != "coverage"),
        "no map when it is off"
    );
    let lights = crate::db::stacking::list_master_lights(&fixture.conn, on_run).unwrap();
    let row = lights
        .iter()
        .find(|m| m.kind == "coverage")
        .expect("a coverage row");
    assert_eq!((row.channels, row.format.as_str()), (1, "fits"));
    let path = PathBuf::from(&row.path);
    assert!(
        path.to_str().unwrap().ends_with("_coverage.fits"),
        "{}",
        row.path
    );
    let header = crate::fits_parser::FitsHeader::from_path(&path).unwrap();
    assert_eq!(header.get_str("BUNIT").as_deref(), Some("frames"));
    let reader = PlaneReader::open(&path).unwrap();
    assert_eq!(reader.channels(), 1);
    let (w, h) = (reader.width(), reader.height());
    let cov = reader.read_plane(0).unwrap();
    assert_eq!(
        cov[(h / 2) * w + w / 2],
        4.0,
        "every frame covers the centre"
    );
    assert!(cov[0] < 4.0, "the shifted frames leave the corner");
    let run_row = crate::db::stacking::get_run(&fixture.conn, on_run)
        .unwrap()
        .unwrap();
    let summary: RunSummary =
        serde_json::from_str(run_row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        summary.groups[0].coverage_path.as_deref(),
        Some(row.path.as_str())
    );
}

/// Spec §5.8, plan 6b-3 D9: with the coverage map on, drizzle writes its
/// own coverage beside its weight map (kind `drizzle_coverage`, one plane
/// per channel, the drizzle's size), and the drizzled master is
/// bit-identical to the run without it.
#[test]
fn drizzle_writes_its_own_coverage_beside_the_weight_map() {
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
    let run_with = |coverage_map: bool| -> i64 {
        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        cfg.output.coverage_map = coverage_map;
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
        started.run_id
    };
    let (off, on) = (run_with(false), run_with(true));
    let lights =
        |run: i64| crate::db::stacking::list_master_lights(&fixture.conn, run).unwrap();
    let drizzled = |run: i64| {
        lights(run)
            .into_iter()
            .find(|m| m.kind == "drizzle")
            .unwrap()
    };
    let plane = |path: &str| {
        PlaneReader::open(Path::new(path))
            .unwrap()
            .read_plane(0)
            .unwrap()
    };
    let (a, b) = (plane(&drizzled(off).path), plane(&drizzled(on).path));
    assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
    assert!(lights(off).iter().all(|m| m.kind != "drizzle_coverage"));
    let row = lights(on)
        .into_iter()
        .find(|m| m.kind == "drizzle_coverage")
        .expect("a drizzle coverage row");
    let d = drizzled(on);
    assert_eq!(
        (row.width, row.height, row.channels),
        (d.width, d.height, d.channels)
    );
    assert!(row.path.ends_with("_coverage.fits"), "{}", row.path);
    let cov = plane(&row.path);
    assert!(
        cov.iter().any(|&c| c == 4.0),
        "the interior is under every frame"
    );
    // The two adjacent summary paths cannot be swapped unseen.
    let run_row = crate::db::stacking::get_run(&fixture.conn, on)
        .unwrap()
        .unwrap();
    let summary: RunSummary =
        serde_json::from_str(run_row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        summary.groups[0].drizzle_coverage_path.as_deref(),
        Some(row.path.as_str())
    );
    let master_cov = lights(on)
        .into_iter()
        .find(|m| m.kind == "coverage")
        .expect("the master coverage row");
    assert_eq!(
        summary.groups[0].coverage_path.as_deref(),
        Some(master_cov.path.as_str())
    );
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&row.path)).unwrap();
    assert_eq!(
        header.get_i32("ATH_DRZ"),
        Some(2),
        "told apart from a master coverage"
    );
}

/// Plan 6b-3 ruling (b), fix round 1: a grid drizzle measures its
/// statistics on the master's covered rectangle times the drizzle factor
/// — `summary.groups[0].drizzle` is bit-equal to `measure_plane` on that
/// crop of the written drizzled master and differs from the whole
/// plane's. The defensive arm (no rectangle handed over) fails the
/// drizzle visibly and keeps the master: one warning, no drizzle rows.
#[test]
fn a_grid_drizzles_statistics_ride_the_masters_covered_rectangle() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    for defensive in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let wide = crate::collab::grid::CanonicalGrid {
            width_px: 768,
            height_px: 576,
            ..grid
        };
        let mut cfg = sky_config(&conn);
        cfg.output.coverage_map = true;
        drizzle_2x(&mut cfg);
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
            cfg.clone(),
            &tmp.path().join("work"),
            &tmp.path().join("out"),
        );
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(wide)));
        FORCE_NO_DRIZZLE_STATS_RECT.store(defensive, Ordering::SeqCst);
        let ran = run_pipeline(&mut rc);
        FORCE_NO_DRIZZLE_STATS_RECT.store(false, Ordering::SeqCst);
        ran.unwrap();

        let group = &rc.summary.groups[0];
        assert!(group.master_path.is_some(), "the master is kept");
        if defensive {
            assert!(group.drizzle_path.is_none() && group.drizzle.is_none());
            let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
            assert!(
                rows.iter()
                    .all(|m| m.kind != "drizzle" && m.kind != "drizzle_coverage"),
                "no drizzle rows"
            );
            let warned = rc
                .warnings
                .iter()
                .filter(|w| w.contains("drizzle failed"))
                .count();
            assert_eq!(warned, 1, "{:?}", rc.warnings);
            continue;
        }
        let dstats = group.drizzle.as_ref().expect("the drizzle's stats");
        let master =
            PlaneReader::open(Path::new(group.master_path.as_deref().unwrap())).unwrap();
        let (w, h) = (master.width(), master.height());
        let count = PlaneReader::open(Path::new(group.coverage_path.as_deref().unwrap()))
            .unwrap()
            .read_plane(0)
            .unwrap();
        // Final review A1: the rectangle is the one at least half the
        // group's frames cover.
        let depth = group.included_count.div_ceil(2) as u32;
        let rect = crate::stacking::crop::largest_covered_rect(&count, w, h, depth)
            .expect("a rect")
            .scaled(2);
        let drz = PlaneReader::open(Path::new(group.drizzle_path.as_deref().unwrap())).unwrap();
        let (dw, dh) = (drz.width(), drz.height());
        assert_eq!((dw, dh), (w * 2, h * 2));
        let plane = drz.read_plane(0).unwrap();
        let opts = cfg
            .measurement
            .measure_options(cfg.normalization.scale_estimator);
        let sub = crate::stacking::crop::crop_plane(&plane, dw, rect);
        let on_rect =
            crate::stacking::measure::measure_plane(&sub, rect.width, rect.height, &opts, None);
        assert_eq!(dstats.noise[0].to_bits(), on_rect.noise.to_bits());
        assert_eq!(dstats.fwhm_px[0].to_bits(), on_rect.fwhm_px.to_bits());
        let whole = crate::stacking::measure::measure_plane(&plane, dw, dh, &opts, None);
        assert!(
            whole.noise.to_bits() != on_rect.noise.to_bits()
                || whole.fwhm_px.to_bits() != on_rect.fwhm_px.to_bits(),
            "the fixture tells the crop from the whole plane"
        );
    }
}

/// Plan 6b-3 D4 refinement (review I1): a grid master's statistics are
/// measured on its largest covered rectangle. On a grid three times the
/// frames' area most of the plane is the uncovered 0.0, so the test reads
/// the written master and its coverage map back and pins the stored
/// noise and PSF SNR to `measure_plane` on exactly that crop; a
/// whole-plane measurement of the same master must differ, which proves
/// the fixture can tell the two apart.
#[test]
fn a_grid_masters_statistics_ignore_uncovered_pixels() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
    let wide = crate::collab::grid::CanonicalGrid {
        width_px: 768,
        height_px: 576,
        ..grid
    };
    let mut cfg = sky_config(&conn);
    cfg.output.coverage_map = true;
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
        cfg.clone(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(wide)));
    run_pipeline(&mut rc).unwrap();

    let group = &rc.summary.groups[0];
    let stats = group.stats.as_ref().expect("the group's stats");
    let master = PlaneReader::open(Path::new(group.master_path.as_deref().unwrap())).unwrap();
    let coverage =
        PlaneReader::open(Path::new(group.coverage_path.as_deref().unwrap())).unwrap();
    let (w, h) = (master.width(), master.height());
    assert_eq!((w, h), (768, 576));
    let plane = master.read_plane(0).unwrap();
    let count = coverage.read_plane(0).unwrap();
    // Final review A1: the rectangle at least half the group's frames cover.
    let depth = group.included_count.div_ceil(2) as u32;
    let rect =
        crate::stacking::crop::largest_covered_rect(&count, w, h, depth).expect("a rect");
    assert!(
        rect.width * 3 < w * 2 && rect.height * 3 < h * 2,
        "{rect:?} is a part of the grid"
    );
    let opts = cfg
        .measurement
        .measure_options(cfg.normalization.scale_estimator);
    let sub = crate::stacking::crop::crop_plane(&plane, w, rect);
    let on_rect =
        crate::stacking::measure::measure_plane(&sub, rect.width, rect.height, &opts, None);
    assert_eq!(stats.master_noise[0].to_bits(), on_rect.noise.to_bits());
    // Plan D D1b: the stored PSF SNR is read over the group's frame-common stars (the set `reweigh_groups`
    // recorded) matched in that crop, at the crop's own origin - not over every star of the crop, as it was.
    let set = rc.common_stars.get(&group.key).and_then(|s| s[0].as_deref());
    let set = set.expect("the reweigh recorded the group's frame-common set");
    let measured = crate::stacking::measure::measure_planes_diagnosed(
        1,
        rect.width,
        rect.height,
        &opts,
        None,
        crate::stacking::measure::SeedOverride::Fast,
        false,
        crate::star_measure::PixelNoise::Mixed,
        None,
        |_| Ok::<_, std::convert::Infallible>(std::borrow::Cow::Borrowed(sub.as_slice())),
    )
    .unwrap();
    let (ch, fits) = (&measured[0].measurement.channel, &measured[0].measurement.fits);
    assert_eq!(ch.noise.to_bits(), on_rect.noise.to_bits());
    let origin = (rect.x0, rect.y0);
    let inside =
        crate::stacking::integrate::positions_in_crop(set, origin, (rect.width, rect.height));
    let (on_set, paired) = crate::stacking::integrate::common_psf_snr(
        fits,
        &inside,
        origin,
        ch.noise,
        ch.fwhm_px,
        1,
    )
    .expect("the set pairs in the crop");
    assert!(paired >= 20, "{paired}");
    assert_eq!(stats.master_psf_snr[0].to_bits(), on_set.to_bits());
    assert_ne!(
        on_set.to_bits(),
        on_rect.psf_snr.to_bits(),
        "the set is a part of the crop's stars"
    );
    let whole = crate::stacking::measure::measure_plane(&plane, w, h, &opts, None);
    assert!(
        whole.noise.to_bits() != on_rect.noise.to_bits()
            || whole.psf_snr.to_bits() != on_rect.psf_snr.to_bits(),
        "the fixture tells the covered rectangle from the whole plane"
    );
}

/// Brief test (e): a cancel raised during the drizzle stage (via a
/// `stacking-progress` listener flipping the run's cancel flag on the
/// first `stage == "drizzle"` event, same mechanism
/// `cancel_mid_run_keeps_artifacts_writes_no_master` uses for
/// `"measure"`) — the run finishes `cancelled`, `rej/` is still removed
/// (cleanup != keepAll), the master is present, and `drizzle_path`
/// stays `NULL` (ruling R-M3-7: a drizzle failure/cancel never touches
/// an already-written master).
#[test]
fn cancel_during_drizzle_keeps_the_master_removes_rej_leaves_drizzle_path_null() {
    struct CancelOnDrizzle {
        ctx: Arc<ServiceContext>,
        frames_set_id: i64,
        fired: std::sync::atomic::AtomicBool,
        recording: Recording,
    }
    impl ProgressEmitter for CancelOnDrizzle {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.recording.emit_json(event_name, payload.clone());
            if event_name == STACKING_PROGRESS_EVENT
                && payload["stage"] == "drizzle"
                && !self.fired.swap(true, Ordering::SeqCst)
            {
                let active = self.ctx.active_stacks.lock().unwrap();
                for h in active.values() {
                    if h.subject == StackSubject::FrameSet(self.frames_set_id) {
                        h.cancel_flag.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;

    let emitter = Arc::new(CancelOnDrizzle {
        ctx: ctx.clone(),
        frames_set_id: fixture.set_id,
        fired: std::sync::atomic::AtomicBool::new(false),
        recording: Recording::new(),
    });

    let started = start_stacking(
        ctx.clone(),
        emitter.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "cancelled", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let group = &groups[0];
    let master_path = group
        .master_path
        .clone()
        .expect("master must survive the cancel");
    assert!(Path::new(&master_path).exists(), "{master_path}");
    assert!(group.drizzle_path.is_none(), "{group:?}");

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    assert!(
        !layout.rej_run_dir(started.run_id).exists(),
        "rej/run-<id> must be removed even on cancel"
    );

    let completes = emitter.recording.events(STACKING_COMPLETE_EVENT);
    assert_eq!(completes.len(), 1, "{completes:?}");
    assert_eq!(completes[0]["cancelled"].as_bool(), Some(true));
}

/// B1 (M3 final fix wave, I1) run.rs composite test (b): a second group
/// whose members are natively LARGER than the run's reference — the
/// acceptance set's own shape (the OSC group's 6248x4176 native frames
/// registered onto a 6224x4168 reference) since the 2026-09-10 owner
/// decision made grouping camera-agnostic. Before the fix, `drizzle_
/// group`'s per-frame geometry check refused every frame in this group
/// outright; both groups must now drizzle, each onto the run's ONE
/// shared reference-sized output grid.
#[test]
fn drizzle_runs_for_a_group_whose_members_differ_in_native_geometry_from_the_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    // Group 1 — the reference's own group, at the usual fixture
    // geometry (STAR_FIELD_WIDTH x STAR_FIELD_HEIGHT).
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, group1_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &group1_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // Group 2 — a DIFFERENT filter (its own group key: grouping is
    // camera/geometry-agnostic, filter still splits), a LARGER native
    // canvas carrying the SAME star field (so registration against
    // group 1's reference finds the same real inlier matches it
    // already does within group 1). Its own master dark/flat pair is
    // built at ITS OWN geometry — calibration requires a light and its
    // masters to share geometry, independent of B1.
    // Wider AND taller than the reference (STAR_FIELD_WIDTH x
    // STAR_FIELD_HEIGHT = 192x144) — big enough margin around
    // `BASE_STARS`' own extent (max x=150, max y=102) that every star
    // stays comfortably inside both canvases.
    let (odd_w, odd_h) = (220usize, 170usize);
    let mut group2_ids = Vec::new();
    for (i, (&(dx, dy), &sigma)) in shifts.iter().zip(noise.iter()).enumerate() {
        let stars = shifted_stars(dx, dy);
        let date_obs = date_obs_at(i);
        let stem = format!("g{i}");
        let spec = LightSpec {
            filter: Some("Ha"),
            width: odd_w,
            height: odd_h,
            ..star_light_spec(&stem, &date_obs)
        };
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            600.0,
            sigma,
            200 + i as u64,
        );
        group2_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(&fixture, &group2_ids, odd_w, odd_h);

    // Pin the reference to group 1's own unshifted frame — deterministic,
    // avoids depending on cross-group weight-based auto-selection.
    crate::registration::db::set_frame_set_reference(
        &fixture.conn,
        fixture.set_id,
        group1_ids[0],
    )
    .unwrap();

    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;

    let recorder = Arc::new(Recording::new());
    let started = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 2, "{groups:?}");

    let expected_out_w = STAR_FIELD_WIDTH * 2;
    let expected_out_h = STAR_FIELD_HEIGHT * 2;
    for group in &groups {
        let drizzle_path = group.drizzle_path.clone().unwrap_or_else(|| {
            panic!(
                "group {}: drizzle_path must be Some — {group:?}",
                group.group_key
            )
        });
        assert!(
            Path::new(&drizzle_path).exists(),
            "group {}: {drizzle_path}",
            group.group_key
        );
        let reader = PlaneReader::open(Path::new(&drizzle_path)).unwrap();
        assert_eq!(
            reader.width(),
            expected_out_w,
            "group {}: outWidth == 2*refW",
            group.group_key
        );
        assert_eq!(
            reader.height(),
            expected_out_h,
            "group {}: outHeight == 2*refH",
            group.group_key
        );
    }
}

/// B3 (M3 final fix wave, I3): a `.rej` write failure mid-integration —
/// the WHOLE `rej/run-<id>/` tree (already created by `RejBitmapSet::
/// create` before `integrate_group` starts) is removed out from under
/// the engine on the FIRST `stage == "integrate"` `stacking-progress`
/// event, via a listener (same "intercept the real progress channel"
/// mechanism `cancel_during_drizzle_keeps_the_master_removes_rej_
/// leaves_drizzle_path_null` uses one stage later) — before any band's
/// rejection bits are written. Ruling: the group's otherwise-good
/// integration must not fail because of it: the master is written,
/// `drizzle_path` stays `NULL`, and the run's warnings name the
/// failure.
#[test]
fn a_rej_write_failure_mid_integration_skips_drizzle_keeps_the_master() {
    struct DeleteRejOnFirstIntegrateTick {
        ctx: Arc<ServiceContext>,
        frames_set_id: i64,
        working: std::path::PathBuf,
        set_slug: String,
        fired: AtomicBool,
        recording: Recording,
    }
    impl ProgressEmitter for DeleteRejOnFirstIntegrateTick {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.recording.emit_json(event_name, payload.clone());
            if event_name == STACKING_PROGRESS_EVENT
                && payload["stage"] == "integrate"
                && !self.fired.swap(true, Ordering::SeqCst)
            {
                let run_id = {
                    let active = self.ctx.active_stacks.lock().unwrap();
                    active
                        .iter()
                        .find(|(_, h)| h.subject == StackSubject::FrameSet(self.frames_set_id))
                        .map(|(id, _)| *id)
                        .expect("this run must be active")
                };
                let layout = WorkingLayout::new(&self.working, &self.set_slug);
                let dir = layout.rej_run_dir(run_id);
                std::fs::remove_dir_all(&dir)
                    .expect("remove rej/run-<id> for the fault injection");
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.use_rejection = true;
    // Deterministic, non-statistical rejection (rather than relying on
    // per-frame noise to occasionally disagree enough to trip Auto's
    // algorithmic rejection): `BASE_STARS`' brightest peaks are
    // 6000-14000 raw ADU, calibrated (dark-subtracted, flat-divided,
    // then divided by the 16-bit `scale_divisor` = 65535 — the
    // calibrated-lights export engine's own scaling, reused verbatim by
    // the stacking Calibrate stage) down to roughly 0.09-0.21, while the
    // ~600 ADU background lands around 0.008-0.009. `range_high = 0.05`
    // sits between the two, so every frame's star-core pixels are
    // RANGE-rejected identically — guaranteeing this run's `.rej`
    // bitmaps are non-empty regardless of noise. Range rejection feeds
    // the same `RejectionBitSink` bits algorithmic rejection does
    // (`engine.rs`'s band loop sets `bits_row` for both).
    cfg.integration.range_high = Some(0.05);

    let emitter = Arc::new(DeleteRejOnFirstIntegrateTick {
        ctx: ctx.clone(),
        frames_set_id: fixture.set_id,
        working: working.path().to_path_buf(),
        set_slug: set_slug(SET_NAME),
        fired: AtomicBool::new(false),
        recording: Recording::new(),
    });

    let started = start_stacking(
        ctx.clone(),
        emitter.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    let master_path = group
        .master_path
        .clone()
        .expect("master must survive a rej write failure");
    assert!(Path::new(&master_path).exists(), "{master_path}");
    assert!(group.drizzle_path.is_none(), "{group:?}");

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.contains("rejection bitmaps could not be written")),
        "{:?}",
        summary.warnings
    );

    // B7 (M3 final fix wave, M4): drizzle was SKIPPED for this group
    // (never attempted — the rej set failed before `drizzle_group` was
    // ever called), so there is no `0/total` starting tick to complete
    // — the terminal tick must be `0/0`, carrying the skip reason as
    // `message`, so the Drizzle row does not stay stuck on whatever the
    // previous stage left it at.
    let drizzle_events: Vec<_> = emitter
        .recording
        .events(STACKING_PROGRESS_EVENT)
        .into_iter()
        .filter(|e| e["stage"] == "drizzle")
        .collect();
    assert!(
        drizzle_events.iter().any(|e| {
            e["current"].as_u64() == Some(0)
                && e["total"].as_u64() == Some(0)
                && e["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("rejection bitmaps could not be written"))
        }),
        "{drizzle_events:?}"
    );
}

/// Fix round 2, Important I3 (re-review): the round-1 version of this
/// test dropped the fixture's LAST frame (index 3 of 4), which leaves
/// `output.included == [0, 1, 2]` — `k == idx` for every survivor, so a
/// hypothetical bug that swapped `k` (position in `output.included`)
/// for `idx` (position in `stack_frames`/`members`) anywhere in the
/// drizzle fan-out would have been COMPLETELY INVISIBLE (`stack_frames[k]`
/// and `stack_frames[idx]` are the same array access for every kept
/// frame). Reworked: 5 frames, DROP THE SECOND (index 1 of 5) — engine
/// order for the 4 survivors is `output.included == [0, 2, 3, 4]`, so
/// `k=1→idx=2`, `k=2→idx=3`, `k=3→idx=4` for every survivor past the
/// first (only `k=0→idx=0` still coincides, unavoidably, since the
/// FIRST kept frame is always its own position).
///
/// Each kept frame gets a DISTINCT, exactly-known weight
/// (`measurement.weight_mode = Exposure`, so `weight == exptime`
/// directly — no noise/PSF-derived formula to fight for a precise
/// target) and a DISTINCT background level; `normalization.output =
/// None` makes every frame's output pair the identity (no scale/offset
/// warps the deposited value); `integration.rejection = None` disables
/// algorithm rejection outright, so nothing in the flat, noise-free
/// (well, near-zero-noise) background region this test samples is ever
/// flagged and skipped. Under those three settings, ruling R-M3-2's
/// `I = Σ a·w·N(d)`, `W = Σ a·w`, output `I/W` reduces EXACTLY to the
/// plain weighted mean `Σ w_i·v_i / Σ w_i` of the kept frames' own
/// calibrated levels at any interior, star-free output pixel — the `a`
/// (area) factor is identical across every frame at a given output
/// pixel (all frames share the SAME zero-shift geometry) and cancels.
/// The test measures each kept frame's own calibrated level directly
/// (reading the SAME file the drizzle stage itself reads, at the SAME
/// star-free coordinate) rather than assuming a calibration formula, so
/// the predicted level is independent of the calibration math entirely.
///
/// See the task report for exactly which survivor pairs a `k`-instead-
/// of-`idx` bug would swap here, and by how much the predicted level
/// would move.
#[test]
fn drizzle_over_a_min_weight_drop_matches_the_kept_frames_exact_weighted_mean() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    // Index 1 (of 0..5) is the one that will be dropped by the
    // min-weight gate — a middle member, not the tail, so every
    // survivor past the first has k != idx (see the doc comment
    // above). Zero relative shift for every frame: this test cares
    // about weight/level alignment, not registration robustness (which
    // every OTHER drizzle test already exercises with real shifts), and
    // a flat background region's value is unaffected by interpolation
    // under an identity map either way.
    let exptimes = [60.0f64, 0.001, 30.0, 15.0, 7.5];
    let backgrounds = [600.0f32, 9999.0, 900.0, 1200.0, 1500.0];
    let noise = [2.0f32, 2.0, 2.0, 2.0, 2.0];
    let mut light_ids = Vec::new();
    for i in 0..5 {
        let stem = format!("f{i}");
        let date_obs = date_obs_at(i);
        let spec = LightSpec {
            stem: &stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: STAR_FIELD_WIDTH,
            height: STAR_FIELD_HEIGHT,
            exptime: exptimes[i],
            date_obs: &date_obs,
            bayerpat: None,
            write_file: true,
        };
        let stars = shifted_stars(0.0, 0.0);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            backgrounds[i],
            noise[i],
            100 + i as u64,
        );
        light_ids.push(id);
    }

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

    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    // Grouping clusters by exposure too (spec §2) — its anchor-relative
    // rule (`seconds_classes`, `groups.rs`) compares every sorted
    // exposure against the CLUSTER'S FIRST value, so the 0.001s..60s
    // spread here needs a tolerance past the full span, or the 5
    // frames would fragment into several undersized groups before this
    // test's own weight gate ever runs.
    cfg.grouping.exposure_tolerance_sec = 100.0;
    // Exact, controllable weights: weight == exptime, nothing derived
    // from noise/PSF fits (which fix round 1 found hard to hit a
    // precise target with).
    cfg.measurement.weight_mode = WeightMode::Exposure;
    // Stage 3 must not exclude frame 1 on weight — it has to survive
    // selection and reach `integrate_group`'s OWN gate.
    cfg.selection.min_weight_fraction = 0.0;
    // Identity output pairs for every frame: the deposited value is
    // each frame's own calibrated level, unscaled/unshifted.
    cfg.normalization.output = crate::integration::stats::OutputNormalization::None;
    // No algorithm rejection: nothing in the flat background region
    // this test samples is ever flagged, whatever a real sigma-clip
    // might make of four frames with deliberately different raw
    // background levels.
    cfg.integration.rejection = crate::stacking::integrate::RejectionChoice::None;
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
    cfg.drizzle.use_rejection = true;
    cfg.drizzle.use_weights = true;
    cfg.output.cleanup = CleanupPolicy::KeepAll;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary.groups.len(), 1, "{:?}", summary.groups);
    let group_summary = &summary.groups[0];
    let stats = group_summary.stats.as_ref().expect("group stats recorded");
    assert_eq!(
        stats.frames, 5,
        "all 5 frames must reach integration: {stats:?}"
    );
    assert_eq!(
        stats.dropped_below_min_weight, 1,
        "expected exactly one frame dropped below the weight floor: {stats:?}"
    );
    assert_eq!(stats.included, 4, "{stats:?}");

    let drizzle_stats = group_summary
        .drizzle
        .as_ref()
        .expect("drizzle stats recorded");
    assert_eq!(
        drizzle_stats.frames, stats.included,
        "drizzle.frames must equal the group's own included count: {drizzle_stats:?}"
    );

    // The 4 KEPT frames' own calibrated levels, measured directly from
    // the SAME files the drizzle stage reads (`SummaryFrame.calibrated_path`),
    // at a coordinate far from every star (`BASE_STARS` spans roughly
    // x in [30, 150], y in [24, 102] on this 192x144 canvas — (165..185,
    // 110..130) is comfortably outside any star's PSF wing at sigma
    // 1.6px) and averaged over a 20x20 patch there to wash out the
    // (small, near-zero) per-pixel noise.
    let region_mean =
        |plane: &[f32], width: usize, x0: usize, y0: usize, w: usize, h: usize| -> f64 {
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    sum += plane[y * width + x] as f64;
                    count += 1;
                }
            }
            sum / count as f64
        };

    let kept_indices = [0usize, 2, 3, 4];
    let kept_weights = [1.0f64, 0.5, 0.25, 0.125]; // exptime / max(exptime) = 60/60, 30/60, 15/60, 7.5/60
    let mut kept_levels = Vec::with_capacity(4);
    let mut kept_stems = Vec::with_capacity(4);
    for &i in &kept_indices {
        let frame_id = light_ids[i];
        let sf = group_summary
            .frames
            .iter()
            .find(|f| f.frame_id == frame_id)
            .unwrap_or_else(|| panic!("frame {i} (id {frame_id}) not in summary"));
        let calibrated_path = sf
            .calibrated_path
            .as_ref()
            .unwrap_or_else(|| panic!("frame {i} has no calibrated_path: {sf:?}"));
        let reader = PlaneReader::open(Path::new(calibrated_path)).unwrap();
        let plane = reader.read_plane(0).unwrap();
        let level = region_mean(&plane, reader.width(), 165, 110, 20, 20);
        kept_levels.push(level);
        // The `.rej` stem is the frame's own `frame_file_stem` (ruling R4 (c)); the calibrated file is
        // `c_<that stem>.fits` (mono: no `_d` marker).
        let calibrated_stem = Path::new(calibrated_path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        kept_stems.push(
            calibrated_stem
                .strip_prefix("c_")
                .unwrap_or_else(|| panic!("calibrated stem {calibrated_stem} has no c_ prefix"))
                .to_string(),
        );
    }

    let sum_w: f64 = kept_weights.iter().sum();
    let sum_wv: f64 = kept_weights
        .iter()
        .zip(kept_levels.iter())
        .map(|(&w, &v)| w * v)
        .sum();
    let expected_level = sum_wv / sum_w;

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let group = &groups[0];
    let drizzle_path = group.drizzle_path.clone().expect("drizzle path recorded");
    let drizzle_reader = PlaneReader::open(Path::new(&drizzle_path)).unwrap();
    let drizzle_plane = drizzle_reader.read_plane(0).unwrap();
    // Scale-2 output: the same (165..185, 110..130) source patch maps
    // to (330..370, 220..260) in output pixels.
    let drizzled_level = region_mean(&drizzle_plane, drizzle_reader.width(), 330, 220, 40, 40);

    assert!(
        (drizzled_level - expected_level).abs() < 1e-4,
        "drizzled interior level does not match the kept frames' exact weighted mean: \
             expected={expected_level} (levels={kept_levels:?}, weights={kept_weights:?}), \
             drizzled={drizzled_level}, diff={}",
        (drizzled_level - expected_level).abs()
    );

    // The .rej files: exactly the 4 KEPT frames' own frame stems,
    // no more, no less — in particular never the dropped frame's.
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let rej_dir = layout.rej_dir(started.run_id, &group.group_key);
    let mut actual_stems: Vec<String> = std::fs::read_dir(&rej_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| {
            Path::new(&e.file_name())
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    actual_stems.sort();
    let mut expected_stems = kept_stems.clone();
    expected_stems.sort();
    assert_eq!(
        actual_stems, expected_stems,
        "rej/ must hold exactly the kept frames' bitmaps, named by their own frame stems"
    );
}

// ── R-M3-17 v2: sky-penalized normalization anchor ──────────────────

/// Seeds 6 star-field lights for the sky-penalized-anchor tests
/// (ruling R-M3-17 v2): `f0`-`f2` are a bright night (exptime
/// 100/95/90s -> normalized weight 1.0/0.95/0.9 under
/// `WeightMode::Exposure`, background 3000 raw ADU), `f3`-`f5` a dark
/// night (exptime 60/55/50s -> weight 0.6/0.55/0.5, background 300 raw
/// ADU — a 10x darker sky; three of them, not two, so `referenceFrames
/// = 3` can pick a member list that is ENTIRELY dark-night — `build_
/// reference`/`build_ln_reference` refuse fewer than 3 candidates
/// outright, so a 2-frame dark night could never populate a whole LN
/// reference on its own). Weight is exact and controllable via
/// `WeightMode::Exposure` (`weight == exptime`, the same trick
/// `drizzle_over_a_min_weight_drop_matches_the_kept_frames_exact_
/// weighted_mean` uses) rather than fought for through noise/PSF
/// fitting. Zero relative shift for every frame (identity
/// registration, full reference coverage) — these tests are about
/// weight/background interaction, not registration robustness, which
/// `drizzle_runs_for_a_group_whose_members_differ_in_native_geometry_
/// from_the_reference` and the register-module tests already cover.
/// Does NOT link a master dark/flat — callers call
/// `test_fixtures::add_master_dark_and_flat` themselves.
fn seed_sky_penalty_group(
    db_path: &Path,
    set_name: &str,
) -> (
    test_fixtures::Fixture,
    Vec<i64>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let exptimes = [100.0f64, 95.0, 90.0, 60.0, 55.0, 50.0];
    let backgrounds = [3000.0f32, 3000.0, 3000.0, 300.0, 300.0, 300.0];
    let noise = [3.0f32; 6];

    let fixture_conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, set_name);

    let mut light_ids = Vec::new();
    for i in 0..6 {
        let stem = format!("f{i}");
        let date_obs = date_obs_at(i);
        let spec = LightSpec {
            stem: &stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: STAR_FIELD_WIDTH,
            height: STAR_FIELD_HEIGHT,
            exptime: exptimes[i],
            date_obs: &date_obs,
            bayerpat: None,
            write_file: true,
        };
        let stars = shifted_stars(0.0, 0.0);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            backgrounds[i],
            noise[i],
            100 + i as u64,
        );
        light_ids.push(id);
    }

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

    (fixture, light_ids, working, output)
}

/// Ruling R-M3-17 v2 (spec §4.4): the set's reference — here manually
/// pinned to `f0`, the brightest-WEIGHTED frame — anchors REGISTRATION
/// only. The group's NORMALIZATION anchor is chosen sky-penalized
/// instead (`s_i = weight.normalized_mean / sqrt(background)`), so the
/// dark-night frame `f3` (weight 0.6, background 300) outranks the
/// bright-night trio (`f0`-`f2`, weight 1.0/0.95/0.9, background 3000):
/// `s3 ≈ 0.0346` beats `s0 ≈ 0.0183` despite the lower raw weight. The
/// master's background at a star-free region must track `f3`'s own
/// calibrated level, not `f0`'s (pinned) or the bright night's — this
/// is the LDN 1272 bug this ruling fixes, reproduced on a synthetic
/// fixture.
#[test]
fn normalization_anchor_prefers_the_dark_night_over_the_pinned_bright_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    let (fixture, light_ids, working, output) = seed_sky_penalty_group(&db_path, SET_NAME);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // Manual pin: the registration reference is f0 — the brightest-
    // weighted frame, and the OPPOSITE of the normalization anchor this
    // test expects. A regression back to Ruling 7 (pinned reference ==
    // normalization anchor) would fail the normalization_reference_
    // frame_id assertion below.
    crate::registration::db::set_frame_set_reference(
        &fixture.conn,
        fixture.set_id,
        light_ids[0],
    )
    .unwrap();

    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    // Sorted exptimes are 50/55/60/90/95/100 — the cluster anchors on
    // the FIRST (50), so the tolerance must cover the 50s gap up to
    // 100.
    cfg.grouping.exposure_tolerance_sec = 55.0;
    cfg.measurement.weight_mode = WeightMode::Exposure;
    // No algorithm rejection: nothing in the flat background region
    // this test samples should ever be flagged, whatever a real
    // sigma-clip might make of frames with deliberately different raw
    // background levels.
    cfg.integration.rejection = crate::stacking::integrate::RejectionChoice::None;

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
    assert_eq!(
        rc.reference_frame_id,
        Some(light_ids[0]),
        "the registration reference must stay the manual pin"
    );
    stage_output(&mut rc).unwrap();

    let group_summary = &rc.summary.groups[0];
    assert_eq!(
        group_summary.normalization_reference_frame_id,
        Some(light_ids[3]),
        "the dark-night frame (lowest background, still well-weighted) must anchor \
             normalization, not the pinned bright reference: {group_summary:?}"
    );

    // Measured directly from the SAME files the output stage itself
    // reads, at a coordinate far from every star (see the drizzle
    // min-weight-drop test above for the same (165, 110, 20, 20)
    // star-free patch on this fixture's canvas).
    let region_mean =
        |plane: &[f32], width: usize, x0: usize, y0: usize, w: usize, h: usize| -> f64 {
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    sum += plane[y * width + x] as f64;
                    count += 1;
                }
            }
            sum / count as f64
        };
    let level_of = |frame_id: i64| -> f64 {
        let path = group_summary
            .frames
            .iter()
            .find(|f| f.frame_id == frame_id)
            .and_then(|f| f.calibrated_path.as_ref())
            .unwrap_or_else(|| panic!("frame {frame_id} has no calibrated_path"));
        let reader = PlaneReader::open(Path::new(path)).unwrap();
        let plane = reader.read_plane(0).unwrap();
        region_mean(&plane, reader.width(), 165, 110, 20, 20)
    };

    let dark_level = level_of(light_ids[3]);
    let bright_level = level_of(light_ids[0]);
    assert!(
        bright_level > dark_level * 2.0,
        "fixture sanity: the bright frame's own calibrated background must be well \
             above the dark frame's: bright={bright_level} dark={dark_level}"
    );

    let master_path = group_summary
        .master_path
        .as_ref()
        .expect("group master written");
    let master_reader = PlaneReader::open(Path::new(master_path)).unwrap();
    let master_plane = master_reader.read_plane(0).unwrap();
    let master_level = region_mean(&master_plane, master_reader.width(), 165, 110, 20, 20);

    assert!(
        (master_level - dark_level).abs() < 0.2 * (bright_level - dark_level).abs(),
        "the master's background must track the dark-night anchor, not the bright/pinned \
             frame: master={master_level} dark={dark_level} bright={bright_level}"
    );
}

/// Ruling R-M3-17 v2: the LN reference's member list is the top `N` by
/// the SAME sky-penalized score the normalization anchor uses, not raw
/// weight — with `referenceFrames = 3` on this fixture, the three
/// dark-night frames (`f3`/`f4`/`f5`) must be chosen over the
/// higher-raw-weight bright-night frames.
#[test]
fn ln_reference_members_are_sky_penalized_not_weight_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    let (fixture, light_ids, working, output) = seed_sky_penalty_group(&db_path, SET_NAME);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.grouping.exposure_tolerance_sec = 55.0;
    cfg.measurement.weight_mode = WeightMode::Exposure;
    // `rejection = Local` (not `local.enabled`): this fixture's
    // `BASE_STARS` field is only 10 stars, under the 20-matched-star
    // floor `normalize_frame` needs to measure a per-frame relative
    // scale — with `local.enabled` (LN driving OUTPUT normalization) a
    // frame that fails that floor is EXCLUDED from the group, and all
    // 6 would be, emptying it. LN driving REJECTION normalization only
    // still builds the reference and ranks its members the same
    // sky-penalized way (`run_group_normalization` runs whenever
    // either is set), but a frame whose scale can't be measured keeps
    // global normalization with a warning instead of being dropped —
    // exactly what this test needs to reach a written master.
    cfg.normalization.rejection = RejectionNormalization::Local;
    // 3, not 2: `build_reference`/`build_ln_reference` refuse fewer
    // than 3 candidates outright, and the fixture's dark night is
    // exactly 3 frames deep for this reason.
    cfg.normalization.local.reference_frames = 3;

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
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
    stage_output(&mut rc).unwrap();

    let artifact = crate::db::stacking::find_artifact(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        &group_key,
        "ln_reference",
        None,
    )
    .unwrap()
    .expect("ln_reference artifact row written");
    let payload: crate::stacking::plan::LnReferencePayload =
        serde_json::from_str(artifact.payload_json.as_deref().unwrap()).unwrap();

    assert_eq!(
        payload.reference_member_ids,
        vec![light_ids[3], light_ids[4], light_ids[5]],
        "the LN reference's members must be the three dark-night frames (the higher \
             sky-penalized score), not the higher-raw-weight bright-night ones: {payload:?}"
    );
}

/// Fix round 1, Important 3: when EVERY member's measured background
/// is non-positive (an over-subtracted master dark is one real cause —
/// a warmer/longer dark, an amp-glow mismatch), `sky_penalized_order`
/// returns an empty order for the whole group; `pick_reference_idx`
/// must fall back to `best_by_weight` rather than fail the group
/// outright — restoring the pre-R-M3-17 guarantee that a group with
/// `>= 3` members always gets a written master. The background is
/// forced bad directly on the already-measured entries (WEIGHT is
/// untouched, still a real, positive `PsfSignalWeight` from the real
/// star field) rather than fought for through calibration math — this
/// isolates the ONE condition under test ("weight fine, background
/// unusable") from whatever combination of calibration parameters a
/// real over-subtracted dark would also perturb (fewer/dimmer star
/// detections, a different noise estimate, …), the same "pre-create a
/// failure deterministically" technique
/// `local_normalization_excludes_a_frame_whose_sidecar_write_fails`
/// uses for its own targeted exclusion.
#[test]
fn an_all_negative_background_group_falls_back_to_best_by_weight_instead_of_failing() {
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
    let group_key = plan_groups[0].key.clone();

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
    for entry in rc.measured.get_mut(&group_key).unwrap() {
        if let Some(m) = entry.measurement.as_mut() {
            for ch in &mut m.channels {
                ch.median = -1.0;
            }
        }
    }
    stage_output(&mut rc).unwrap();

    let group_summary = &rc.summary.groups[0];
    assert!(
        group_summary.master_path.is_some(),
        "the group must still write a master despite every background being \
             unusable: {group_summary:?}"
    );
    assert!(
        group_summary.normalization_reference_frame_id.is_some(),
        "an anchor must still be chosen: {group_summary:?}"
    );
    assert!(
        rc.warnings
            .iter()
            .any(|w| w.contains("no admissible member has a usable measured background")),
        "the fallback must be visible in the run's own warnings: {:?}",
        rc.warnings
    );
}

/// Fix round 1, Important 5: the addendum's rotated-frame skip, as a
/// COMPOSITE test through `pick_reference_idx` itself (the unit-level
/// `weights::coverage_filter_then_sky_penalized_rank_skips_a_rotated_
/// top_score_candidate` re-implements the wiring rather than exercising
/// it) — this catches a transposed `src_w`/`ref_w` argument, an
/// inverted `>= 3` fallback, or the wrong axis pair, none of which the
/// unit test can. A member with a NATIVE EXTENT smaller than the
/// group's own reference geometry stands in for "a rotated frame" —
/// grouping is camera/geometry-agnostic (spec §2), so this member
/// shares its group with 4 normal-sized ones — both a rotation and a
/// size mismatch fail `reference_coverage` the same way, and a size
/// mismatch is far cheaper to engineer reliably through the real
/// detector/registration pipeline than an actual rotated star field.
/// The mismatched member is given by far the highest weight
/// (`WeightMode::Exposure`) — the addendum's "top-score member" — and
/// must still be skipped as both the normalization anchor and the LN
/// reference's sole member, with NO coverage-dropped warning (4 normal
/// members comfortably clear the `>= 3` fallback threshold — "the
/// warning-free path").
#[test]
fn coverage_gate_skips_a_top_score_member_with_a_mismatched_native_extent() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    let fixture_conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    // 4 normal-sized members (the reference's own geometry), distinct
    // exptimes so the "next-best" anchor after the mismatched member is
    // skipped is unambiguous.
    let normal_exptimes = [45.0f64, 40.0, 35.0, 30.0];
    let mut normal_ids = Vec::new();
    for (i, &exptime) in normal_exptimes.iter().enumerate() {
        let stem = format!("n{i}");
        let date_obs = date_obs_at(i);
        let spec = LightSpec {
            exptime,
            ..star_light_spec(&stem, &date_obs)
        };
        let stars = shifted_stars(0.0, 0.0);
        let (id, _path) = test_fixtures::add_light_with_field(
            &fixture,
            &spec,
            &stars,
            1000.0,
            3.0,
            100 + i as u64,
        );
        normal_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &normal_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    // The mismatched member: a canvas well under the reference's own
    // (192x144) but still large enough to contain BASE_STARS' own
    // extent (max x=150, y=102) with a safe margin — by far the
    // highest weight.
    const SMALL_W: usize = 180;
    const SMALL_H: usize = 130;
    let small_date_obs = date_obs_at(4);
    let small_spec = LightSpec {
        width: SMALL_W,
        height: SMALL_H,
        exptime: 100.0,
        ..star_light_spec("small", &small_date_obs)
    };
    let small_stars = shifted_stars(0.0, 0.0);
    let (small_id, _path) = test_fixtures::add_light_with_field(
        &fixture,
        &small_spec,
        &small_stars,
        1000.0,
        3.0,
        999,
    );
    test_fixtures::add_master_dark_and_flat(&fixture, &[small_id], SMALL_W, SMALL_H);

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

    // Manual pin: the registration reference is n1 — a DIFFERENT
    // normal member than the one the normalization anchor should end
    // up being (n0, the highest-weighted survivor once the mismatched
    // member is excluded) — reinforcing that registration and
    // normalization stay separate concepts (ruling R-M3-17).
    crate::registration::db::set_frame_set_reference(
        &fixture.conn,
        fixture.set_id,
        normal_ids[1],
    )
    .unwrap();

    let mut cfg = StackingConfig::default();
    cfg.reference.mode = ReferenceMode::Manual;
    // Sorted exptimes are 30/35/40/45/100 — the cluster anchors on the
    // FIRST (30), so the tolerance must cover the 70s gap up to 100.
    cfg.grouping.exposure_tolerance_sec = 80.0;
    cfg.measurement.weight_mode = WeightMode::Exposure;
    // `rejection = Local` (not `local.enabled`), same reasoning as
    // `ln_reference_members_are_sky_penalized_not_weight_only`: this
    // fixture's star field is under the 20-matched-star floor
    // `normalize_frame` needs, so `local.enabled` would exclude every
    // member from the group instead of just skipping the mismatched
    // one from the LN reference's member list. `referenceFrames = 3`,
    // not 1: `build_reference`/`build_ln_reference` refuse fewer than
    // 3 candidates outright.
    cfg.normalization.rejection = RejectionNormalization::Local;
    cfg.normalization.local.reference_frames = 3;

    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();
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
    assert_eq!(
        rc.reference_frame_id,
        Some(normal_ids[1]),
        "the registration reference must stay the manual pin"
    );
    stage_output(&mut rc).unwrap();

    let group_summary = &rc.summary.groups[0];
    assert_eq!(
        group_summary.normalization_reference_frame_id,
        Some(normal_ids[0]),
        "the mismatched-geometry top-score member ('small') must be skipped for \
             the next-best COVERING member: {group_summary:?}"
    );

    // The warning-free path: 4 normal members comfortably clear the
    // >= 3 fallback threshold, so the real coverage filter stays
    // active and no "filter dropped" warning fires.
    assert!(
        !rc.warnings
            .iter()
            .any(|w| w.contains("reference-coverage filter dropped")),
        "4 covering members must never trip the < 3 fallback: {:?}",
        rc.warnings
    );

    // The LN reference (referenceFrames = 1) must also skip the
    // mismatched member — with `referenceFrames = 3` and the mismatched
    // member excluded by coverage, the members are the 3 BEST-SCORING
    // of the 4 covering normals (n0/n1/n2, exptime 45/40/35), never
    // `small` despite its own top raw score.
    let artifact = crate::db::stacking::find_artifact(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        &group_key,
        "ln_reference",
        None,
    )
    .unwrap()
    .expect("ln_reference artifact row written");
    let payload: crate::stacking::plan::LnReferencePayload =
        serde_json::from_str(artifact.payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        payload.reference_member_ids,
        vec![normal_ids[0], normal_ids[1], normal_ids[2]],
        "the LN reference's members must never include the mismatched-geometry \
             one despite its own top raw score: {payload:?}"
    );
}

// ── M4d Task 2: XISF output (ruling R-M4d-3) ────────────────────────

/// `output.format = xisf`: the master, the drizzled master and its
/// weight map all land as `.xisf`, all read back through the reader this
/// project ships at the run's own geometry (`PlaneReader` is FITS-only,
/// so these assertions go through `astroimage::ImageConverter::read_raw`
/// instead), and the rejection maps stay FITS.
#[test]
fn an_xisf_run_writes_xisf_outputs_and_fits_rejection_maps() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.format = crate::stacking::config::OutputFormat::Xisf;
    cfg.integration.write_rejection_maps = true;
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
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");

    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let group = &groups[0];
    let master_path = group.master_path.clone().expect("master path recorded");
    assert!(master_path.ends_with(".xisf"), "{master_path}");
    let drizzle_path = group.drizzle_path.clone().expect("drizzle path recorded");
    assert!(drizzle_path.ends_with(".xisf"), "{drizzle_path}");

    // The rejection maps are diagnostics and stay FITS either way.
    let low = group.rejection_low_path.clone().expect("_rejlow path set");
    assert!(low.ends_with("_rejlow.fits"), "{low}");
    assert!(Path::new(&low).exists(), "{low}");

    let (master_meta, _) =
        astroimage::ImageConverter::read_raw(Path::new(&master_path)).unwrap();
    assert!(
        master_meta.width > 0 && master_meta.height > 0,
        "{} x {}",
        master_meta.width,
        master_meta.height
    );
    let (drizzle_meta, _) =
        astroimage::ImageConverter::read_raw(Path::new(&drizzle_path)).unwrap();
    assert_eq!(drizzle_meta.width, 2 * master_meta.width);
    assert_eq!(drizzle_meta.height, 2 * master_meta.height);
    assert_eq!(drizzle_meta.channels, master_meta.channels);

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let weight_map_path = summary.groups[0]
        .weight_map_path
        .clone()
        .expect("weight map path recorded");
    assert!(weight_map_path.ends_with(".xisf"), "{weight_map_path}");
    let (weight_meta, _) =
        astroimage::ImageConverter::read_raw(Path::new(&weight_map_path)).unwrap();
    assert_eq!(
        (weight_meta.width, weight_meta.height, weight_meta.channels),
        (
            drizzle_meta.width,
            drizzle_meta.height,
            drizzle_meta.channels
        ),
        "the weight map shares the drizzled master's grid"
    );

    // Fix round 1, ruling R-T2-1: the XISF header always STATES the
    // stored array's row order rather than leaving a reader with the
    // FITS convention's silent default. The effective order is the
    // SOURCE frames' own `ROWORDER`, copied through calibration
    // (`calibration_library::light_headers`' `COPY_THROUGH_KEYWORDS`),
    // registration (`register::writer::REGISTERED_COPY_THROUGH`) and
    // the master's own card build — so this fixture's `TOP-DOWN` lights
    // give a `TOP-DOWN` master and nothing to warn about. Only a set
    // whose frames carry no card at all lands on the `'BOTTOM-UP'`
    // default (the two sibling tests below cover both of those).
    let keys = crate::fits_parser::stored_header::parse_stored_header_keys(
        crate::models::FileFormat::XISF,
        &crate::fits_parser::extract_xisf_header(Path::new(&master_path)).unwrap(),
    );
    assert_eq!(
        keys.get("ROWORDER").map(String::as_str),
        Some(crate::orientation::ROW_ORDER_TOP_DOWN),
        "{keys:?}"
    );
    assert!(
        !summary
            .warnings
            .iter()
            .any(|w| w.contains("bottom-up row order")),
        "a TOP-DOWN set must not warn: {:?}",
        summary.warnings
    );
}

/// Point what the CALIBRATE stage reads at a different row order: the
/// stored `fits_header` blob (`light_resolve::source_cards_for_file`
/// parses it) and the `frames.roworder` column (its catalog fallback).
/// `None` removes both — the state of a source frame that never carried
/// the card. The raw file's own bytes are deliberately left alone: the
/// pipeline never re-reads a light's header off disk, so rewriting the
/// file would change nothing and prove nothing.
fn set_lights_row_order(conn: &rusqlite::Connection, frame_ids: &[i64], order: Option<&str>) {
    let card_line = order.map(|order| {
        let records = crate::fits_writer::card::format_card(
            &crate::fits_writer::Card::new(
                "ROWORDER",
                crate::fits_writer::CardValue::Str(order.to_string()),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(records.len(), 1, "one 80-byte record");
        String::from_utf8(records[0].to_vec()).unwrap()
    });
    for &frame_id in frame_ids {
        let (file_id, header): (i64, String) = conn
            .query_row(
                "SELECT f.file_id, h.header
                     FROM frames f JOIN fits_header h ON h.file_id = f.file_id
                     WHERE f.id = ?1",
                rusqlite::params![frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("frame {frame_id} has no stored header: {e}"));
        let mut lines: Vec<String> = header
            .lines()
            .filter(|l| !l.starts_with("ROWORDER"))
            .map(|l| l.to_string())
            .collect();
        if let Some(line) = card_line.clone() {
            match lines.iter().position(|l| l.starts_with("END")) {
                Some(i) => lines.insert(i, line),
                None => lines.push(line),
            }
        }
        conn.execute(
            "UPDATE fits_header SET header = ?1 WHERE file_id = ?2",
            rusqlite::params![lines.join("\n"), file_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE frames SET roworder = ?1 WHERE id = ?2",
            rusqlite::params![order, frame_id],
        )
        .unwrap();
    }
}

/// Ruling R-T2-1: a bottom-up set's XISF master keeps the frames' row
/// order — the pixels are not flipped, the header states `BOTTOM-UP`
/// explicitly, and the run warns exactly ONCE, naming the group.
#[test]
fn a_bottom_up_xisf_run_warns_once_about_the_row_order() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    set_lights_row_order(
        &fixture.conn,
        &light_ids,
        Some(crate::orientation::ROW_ORDER_BOTTOM_UP),
    );
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.format = crate::stacking::config::OutputFormat::Xisf;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let group = &groups[0];
    let master_path = group.master_path.clone().expect("master path recorded");
    assert!(master_path.ends_with(".xisf"), "{master_path}");

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let row_order: Vec<&String> = summary
        .warnings
        .iter()
        .filter(|w| w.contains("bottom-up row order"))
        .collect();
    assert_eq!(
        row_order.len(),
        1,
        "exactly one row-order warning: {:?}",
        summary.warnings
    );
    assert!(
        row_order[0].contains(&group.group_key),
        "the warning names its group: {}",
        row_order[0]
    );

    let keys = crate::fits_parser::stored_header::parse_stored_header_keys(
        crate::models::FileFormat::XISF,
        &crate::fits_parser::extract_xisf_header(Path::new(&master_path)).unwrap(),
    );
    assert_eq!(
        keys.get("ROWORDER").map(String::as_str),
        Some(crate::orientation::ROW_ORDER_BOTTOM_UP),
        "the XISF header states the effective order: {keys:?}"
    );
}

/// Ruling R-T2-1, the third case: source frames carrying NO `ROWORDER`
/// at all. The astronomical convention reads that as bottom-up
/// (`orientation::row_order_is_bottom_up`'s `None` arm), so the XISF
/// header says `'BOTTOM-UP'` — a statement the source never made, which
/// is exactly why it comes with the warning.
#[test]
fn a_source_without_a_row_order_card_is_declared_bottom_up_and_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    set_lights_row_order(&fixture.conn, &light_ids, None);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let mut cfg = StackingConfig::default();
    cfg.output.format = crate::stacking::config::OutputFormat::Xisf;

    let started = start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        Some(cfg),
        None,
    )
    .expect("start should succeed");
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
    let master_path = groups[0].master_path.clone().expect("master path recorded");

    let keys = crate::fits_parser::stored_header::parse_stored_header_keys(
        crate::models::FileFormat::XISF,
        &crate::fits_parser::extract_xisf_header(Path::new(&master_path)).unwrap(),
    );
    assert_eq!(
        keys.get("ROWORDER").map(String::as_str),
        Some(crate::orientation::ROW_ORDER_BOTTOM_UP),
        "{keys:?}"
    );

    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        summary
            .warnings
            .iter()
            .filter(|w| w.contains("bottom-up row order"))
            .count(),
        1,
        "{:?}",
        summary.warnings
    );
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

/// Review Focus 5 (heal half).
#[test]
fn the_heal_marks_an_interrupted_project_run_failed_and_leaves_frame_sets_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    crate::db::stacking::insert_test_project(&fixture.conn, "p1");

    let mut cfg = StackingConfig::default();
    cfg.output.cleanup = CleanupPolicy::DeleteIntermediates;
    let config_json = serde_json::to_string(&cfg).unwrap();
    let new_run = |subject: &StackSubject| {
        let id = insert_run(
            &fixture.conn,
            &NewRun {
                subject,
                config_json: &config_json,
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: output.path().to_str().unwrap(),
            },
        )
        .unwrap();
        crate::db::stacking::set_run_status(&fixture.conn, id, "running").unwrap();
        id
    };
    let set_subject = StackSubject::FrameSet(fixture.set_id);
    let project_subject = StackSubject::Project("p1".into());
    let set_run = new_run(&set_subject);
    let project_run = new_run(&project_subject);
    ctx.active_stacks.lock().unwrap().insert(
        set_run,
        StackHandle {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            subject: set_subject.clone(),
        },
    );

    let project_rej = working
        .path()
        .join(crate::stacking::paths::PROJECTS_DIR)
        .join("p1")
        .join("rej")
        .join(format!("run-{project_run}"));
    std::fs::create_dir_all(&project_rej).unwrap();
    std::fs::write(project_rej.join("leftover.rej"), b"stale").unwrap();

    let healed = heal_interrupted_runs(&ctx, &fixture.conn).unwrap();
    assert_eq!(healed, 1, "only the project run is stuck");

    let project_row = crate::db::stacking::get_run(&fixture.conn, project_run)
        .unwrap()
        .unwrap();
    assert_eq!(project_row.status, "failed");
    assert_eq!(
        project_row.error.as_deref(),
        Some("interrupted by a restart")
    );
    assert!(
        !project_rej.exists(),
        "the project run's rej dir is swept through subject_layout"
    );
    let set_row = crate::db::stacking::get_run(&fixture.conn, set_run)
        .unwrap()
        .unwrap();
    assert_eq!(
        set_row.status, "running",
        "a live frame-set run is untouched"
    );
}

/// Review Focus 5 (work usage and cleanup half): a project run row and
/// project artifacts/folder do not disturb a frame set's usage or cleanup.
#[test]
fn work_usage_and_cleanup_of_a_frame_set_ignore_project_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    crate::db::stacking::insert_test_project(&fixture.conn, "p1");

    let mut cfg = StackingConfig::default();
    cfg.paths.working_dir = Some(working.path().to_string_lossy().into_owned());
    crate::api::stacking::set_stacking_config(&ctx, fixture.set_id, cfg, Vec::new()).unwrap();

    let set_group = WorkingLayout::new(working.path(), &set_slug(SET_NAME)).calibrated_dir();
    std::fs::create_dir_all(&set_group).unwrap();
    std::fs::write(set_group.join("x.bin"), vec![0u8; 1024]).unwrap();
    let project_group = working
        .path()
        .join(crate::stacking::paths::PROJECTS_DIR)
        .join("p1")
        .join("calibrated")
        .join("g");
    std::fs::create_dir_all(&project_group).unwrap();
    std::fs::write(project_group.join("y.bin"), vec![0u8; 512]).unwrap();

    let project_subject = StackSubject::Project("p1".into());
    let project_run = insert_run(
        &fixture.conn,
        &NewRun {
            subject: &project_subject,
            config_json: "{}",
            config_hash: "h",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.path().to_str().unwrap(),
            output_dir: output.path().to_str().unwrap(),
        },
    )
    .unwrap();
    crate::db::stacking::set_run_status(&fixture.conn, project_run, "running").unwrap();
    // A live handle: the project run is genuinely active, so the heal
    // sweep inside the two commands leaves it alone.
    ctx.active_stacks.lock().unwrap().insert(
        project_run,
        StackHandle {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            subject: project_subject.clone(),
        },
    );
    crate::db::stacking::upsert_artifact(
        &fixture.conn,
        &crate::db::stacking::NewArtifact {
            subject: &project_subject,
            frame_id: None,
            group_key: "g",
            kind: "calibrated",
            path: None,
            config_hash: "h",
            size: None,
            modified_at: None,
            payload_json: None,
        },
    )
    .unwrap();

    let usage = crate::api::stacking::get_stacking_work_usage(&ctx, fixture.set_id).unwrap();
    assert_eq!(
        usage.calibrated_bytes, 1024,
        "only the frame set's own folder"
    );

    crate::api::stacking::cleanup_stacking_work(&ctx, fixture.set_id, CleanupWhat::All)
        .unwrap();
    assert!(!set_group.join("x.bin").exists());
    assert!(
        project_group.join("y.bin").exists(),
        "the project folder is untouched"
    );
    assert_eq!(
        crate::db::stacking::list_artifacts(&fixture.conn, &project_subject, None)
            .unwrap()
            .len(),
        1,
        "the project's artifact row is untouched"
    );
    let run_row = crate::db::stacking::get_run(&fixture.conn, project_run)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "running");
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

pub(super) fn project_conn(db_path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn
}

/// A project run's context over the project's own frame source (plan
/// 6b-2): a real run row and group rows, so stages can persist.
pub(super) fn project_rc(
    ctx: &Arc<ServiceContext>,
    conn: &rusqlite::Connection,
    project_id: &str,
    cfg: StackingConfig,
    working: &Path,
    output: &Path,
) -> RunContext {
    let source =
        crate::stacking::project_source::project_frame_source(conn, project_id, &cfg).unwrap();
    let subject = StackSubject::Project(project_id.to_string());
    let output = crate::stacking::paths::subject_output_dir(
        output,
        &subject,
        &crate::stacking::plan::subject_label(conn, &subject).unwrap(),
    );
    let run_id = insert_run(
        conn,
        &NewRun {
            subject: &subject,
            config_json: &serde_json::to_string(&cfg).unwrap(),
            config_hash: &config_hash(&cfg),
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: working.to_str().unwrap(),
            output_dir: output.to_str().unwrap(),
        },
    )
    .unwrap();
    let mut group_ids = HashMap::new();
    for g in &source.groups {
        let id = insert_group(
            conn,
            &NewGroup {
                run_id,
                group_key: &g.key,
                instrume: g.instrume.as_deref(),
                color_mode: color_mode_wire(g.color_mode),
                filter: g.filter.as_deref(),
                binning: Some(g.binning),
                width: None,
                height: None,
                exposure: g.exposure_s,
                exposure_class: Some(&g.exposure_class),
                frame_count: g.frames.len() as i64,
                included_count: g.frames.len() as i64,
            },
        )
        .unwrap();
        group_ids.insert(g.key.clone(), id);
    }
    let layout = subject_layout(conn, working, &subject).unwrap();
    project_test_context(
        ctx.clone(),
        Arc::new(NullEmitter),
        run_id,
        project_id,
        cfg,
        source.groups,
        source.origins,
        layout,
        output.to_path_buf(),
        group_ids,
    )
}

/// `calibrated` rows pointing at each project frame's landed file — what
/// Prepare writes for a float replica used in place (Task 9b) — so
/// Measure can run without Prepare.
fn seed_in_place_calibrated(conn: &rusqlite::Connection, rc: &mut RunContext) {
    for g in rc.plan_groups.clone() {
        for f in &g.frames {
            let hash = rc
                .memo
                .calibration_hash_checked(conn, &rc.config, f)
                .unwrap();
            let (size, modified_at) = file_identity(Path::new(&f.path)).unwrap();
            upsert_artifact(
                conn,
                &NewArtifact {
                    subject: &rc.subject,
                    frame_id: Some(f.frame_id),
                    group_key: artifact_group_key("calibrated", &g.key),
                    kind: "calibrated",
                    path: Some(&f.path),
                    config_hash: &hash,
                    size: Some(size),
                    modified_at: Some(&modified_at),
                    payload_json: None,
                },
            )
            .unwrap();
        }
    }
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
        crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("calibrated")).unwrap();
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
        let row = |kind: &str| {
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
        let calibrated = PathBuf::from(row("calibrated"));
        assert_eq!(
            calibrated,
            rc.layout
                .prepared_dir()
                .join(format!("p_{stem}_f{}_d.fits", f.frame_id))
        );
        assert_eq!(row("calibrated_mosaic"), f.path, "the landed mosaic itself");
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
            "calibrated",
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
        let row = crate::db::stacking::find_artifact(&conn, &rc.subject, "", "calibrated", Some(f.frame_id))
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
        let row = crate::db::stacking::find_artifact(&conn, &rc.subject, "", "calibrated", Some(f.frame_id))
            .unwrap()
            .unwrap();
        let now = PathBuf::from(row.path.unwrap());
        assert!(now.starts_with(rc.layout.prepared_dir()), "converted under prepared/: {now:?}");
        assert!(Path::new(&f.path).exists(), "the user's landed file {} was removed", f.path);
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
    let key = "mono__L__300s";
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
                "calibrated",
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
        "calibrated",
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
            "calibrated",
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
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some("calibrated"))
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

/// Rewrite every light of a seeded star group as a three-plane float
/// FITS in the unit domain (an externally debayered light: no BAYERPAT
/// in the catalog, three planes in the file) and refresh the catalog's
/// file identity.
fn make_lights_three_plane(
    fixture: &test_fixtures::Fixture,
    shifts: &[(f64, f64)],
    only: &[usize],
) {
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        if !only.contains(&i) {
            continue;
        }
        let path: String = fixture
            .conn
            .query_row(
                "SELECT fi.path FROM files fi JOIN frames f ON f.file_id = fi.id
                     WHERE fi.filename = ?1",
                [format!("f{i}.fits")],
                |r| r.get(0),
            )
            .unwrap();
        let stars: Vec<(f64, f64, f64)> = shifted_stars(dx, dy)
            .into_iter()
            .map(|(x, y, a)| (x, y, a / 12000.0))
            .collect();
        let plane = test_fixtures::synthetic_star_field(
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
            &stars,
            3.0,
            0.002,
            100 + i as u64,
        );
        let rgb: Vec<f32> = [1.0f32, 0.8, 0.6]
            .iter()
            .flat_map(|k| plane.iter().map(move |v| v * k))
            .collect();
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
            Card::new("EXPTIME", CardValue::Real(60.0)).unwrap(),
        ];
        crate::fits_writer::writer::write_fits_f32(
            Path::new(&path),
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
            3,
            &rgb,
            &cards,
        )
        .unwrap();
        let (size, modified_at) = file_identity(Path::new(&path)).unwrap();
        fixture
            .conn
            .execute(
                "UPDATE files SET size = ?1, modified_at = ?2 WHERE path = ?3",
                rusqlite::params![size, modified_at, path],
            )
            .unwrap();
    }
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
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some("calibrated"))
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
        crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some("calibrated"))
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
    rc.memo = HashMemo::new(rc.origins.clone());
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
    rc.memo = HashMemo::new(rc.origins.clone());
    let err = stage_calibrate(&mut rc).unwrap_err();
    assert!(
        matches!(err, RunError::Other(ref m) if m.contains("cannot mix prepared and calibrated frames")),
        "{err:?}"
    );
    assert!(!rc.layout.calibrated_root().exists() && !rc.layout.prepared_root().exists());
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

fn handle_of(rc: &RunContext, file_name: &str) -> i64 {
    rc.plan_groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .find(|f| f.filename == file_name)
        .unwrap()
        .frame_id
}

/// R frames `r1..=r{n_r}` with no WCS and G frames `g1..=g{n_g}` with a
/// manifest WCS, all landed under `landed`.
fn seed_r_and_g(
    conn: &rusqlite::Connection,
    landed: &Path,
    project_id: &str,
    n_r: usize,
    n_g: usize,
) {
    for i in 1..=n_r {
        let uuid = format!("r{i}");
        let mut spec = test_fixtures::project_frame_spec(&uuid);
        spec.filter = "R";
        test_fixtures::add_project_frame(conn, landed, project_id, &spec);
    }
    for i in 1..=n_g {
        let uuid = format!("g{i}");
        let mut spec = test_fixtures::project_frame_spec(&uuid);
        spec.filter = "G";
        spec.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        test_fixtures::add_project_frame(conn, landed, project_id, &spec);
    }
}

fn blake3_of(conn: &rusqlite::Connection, project_id: &str, uuid: &str) -> String {
    conn.query_row(
        "SELECT blake3 FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
        [project_id, uuid],
        |r| r.get(0),
    )
    .unwrap()
}

/// Every frame of `rc` measured and included, its weight from `weights`
/// by file name (0.5 by default), its calibrated file its own landed path:
/// what stages 4 and 5 read, without running Measure.
fn measure_all(rc: &mut RunContext, weights: &HashMap<&str, f64>) {
    for g in rc.plan_groups.clone() {
        let entries: Vec<MeasuredFrame> = g
            .frames
            .iter()
            .map(|f| {
                let w = weights.get(f.filename.as_str()).copied().unwrap_or(0.5);
                MeasuredFrame {
                    frame: f.clone(),
                    calibrated: Some(PathBuf::from(&f.path)),
                    planes: 1,
                    measurement: None,
                    weight: Some(FrameWeight {
                        channels: vec![w],
                        normalized: vec![w],
                        mean: w,
                        normalized_mean: w,
                        missing: None,
                    }),
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
                }
            })
            .collect();
        rc.measured.insert(g.key.clone(), entries);
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
            crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("calibrated"))
                .unwrap();
        assert_eq!(artifacts.len(), 3);
        for a in &artifacts {
            let path = PathBuf::from(a.path.as_deref().unwrap());
            assert!(paths.contains(&path), "used in place: {path:?}");
        }
    }
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

/// Review round 1, B1: a native project group with no WCS candidate
/// borrows the run reference and has nothing to switch to, so it gets no
/// dry pass; a group holding another candidate may switch.
#[test]
fn a_native_project_group_without_a_candidate_cannot_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 4, 3);
    let mut native = StackingConfig::default();
    native.registration.geometry = RegistrationGeometry::Native;
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        native,
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    measure_all(&mut rc, &HashMap::new());
    stage_reference(&mut rc).unwrap();
    let reference = rc.reference_frame_id.unwrap();
    let r_ref = rc.group_geometry["mono__R__300s"].reference_frame_id;
    assert_eq!(r_ref, reference, "the R group borrows the run reference");
    assert!(!group_can_switch(&rc, "mono__R__300s", r_ref));
    assert!(group_can_switch(&rc, "mono__G__300s", reference));
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

/// The project fixture of the 6b-3 run tests: `n` mono L frames dithered
/// around the fixture target (83.8, −5.4), every one with a manifest
/// WCS, and the grid they land on (1.5″/px, 512 × 384, north up).
fn seed_sky_project(
    conn: &rusqlite::Connection,
    landed: &Path,
    n: usize,
) -> crate::collab::grid::CanonicalGrid {
    seed_sky_project_exposures(conn, landed, &vec![300.0; n])
}

/// [`seed_sky_project`] with one frame per entry of `exposures` (seconds).
fn seed_sky_project_exposures(
    conn: &rusqlite::Connection,
    landed: &Path,
    exposures: &[f64],
) -> crate::collab::grid::CanonicalGrid {
    crate::db::stacking::insert_test_project(conn, "p1");
    let target = (83.8, -5.4);
    let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
    let uuids: Vec<String> = (0..exposures.len()).map(|i| format!("l{i}")).collect();
    for (i, uuid) in uuids.iter().enumerate() {
        // Dithers of ±3.6 / ±1.2 px (RA) and 1.2 px (Dec): the LN field's
        // frame-unit lattice (D8, review I6) resolves a dither, and the
        // 0.97 rule fails a member that misses over 3 % of the field —
        // as it fails a frame set's member against the reference.
        let dither = ((i as f64 - 1.5) * 0.001, (i % 2) as f64 * 0.0005);
        let mut f = test_fixtures::sky_frame(uuid, (target.0 + dither.0, target.1 + dither.1));
        f.noise_seed = 100 + i as u64;
        f.exptime_sec = exposures[i];
        test_fixtures::add_project_sky_frame(conn, landed, "p1", &f, &stars);
    }
    crate::collab::grid::CanonicalGrid {
        scale_arcsec: 1.5,
        width_px: 512,
        height_px: 384,
        crval1: target.0,
        crval2: target.1,
        rotation_deg: 0.0,
    }
}

/// The project config of the 6b-3 run tests, stored as the project's
/// document so a plan resolves the same groups and hashes: the seed,
/// with the two-pass re-pick off (its dry pass always detects afresh).
fn sky_config(conn: &rusqlite::Connection) -> StackingConfig {
    let mut cfg = crate::stacking::config::project_seed_config(StackingConfig::default());
    cfg.reference.two_pass = false;
    crate::db::stacking::set_project_config(
        conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    cfg
}

fn run_to_register(rc: &mut RunContext) {
    stage_calibrate(rc).unwrap();
    stage_measure(rc).unwrap();
    stage_reference(rc).unwrap();
    stage_register(rc).unwrap();
}

/// Background-subtracted centroid of the brightest blob within 3 px of
/// `(x, y)` (7 × 7 window).
fn centroid_near(plane: &[f32], w: usize, x: f64, y: f64) -> (f64, f64) {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    let mut vals = Vec::new();
    for dy in -3..=3 {
        for dx in -3..=3 {
            vals.push((
                (cx + dx) as f64,
                (cy + dy) as f64,
                plane[((cy + dy) as usize) * w + (cx + dx) as usize] as f64,
            ));
        }
    }
    let mut sorted: Vec<f64> = vals.iter().map(|v| v.2).collect();
    sorted.sort_by(f64::total_cmp);
    let floor = sorted[sorted.len() / 2];
    let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
    for (px, py, v) in vals {
        let wgt = (v - floor).max(0.0);
        sx += px * wgt;
        sy += py * wgt;
        sw += wgt;
    }
    (sx / sw, sy / sw)
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
            crate::stacking::paths::artifact_group_key("registered", &group),
            "registered",
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
                crate::stacking::paths::artifact_group_key("registered", &group),
                "registered",
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
            crate::stacking::paths::artifact_group_key("registered", &group),
            "registered",
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

/// Ruling R14 (b): the landed-file identity check runs BEFORE the two-pass lookup. A cached decision names a
/// member whose landed file has changed since (the catalog of a project has no rescan, so the member's hashes did
/// not move): the lookup must see the group without it, miss, and run the dry pass over the rest. Without the
/// reorder the hit adopts the changed member as the reference and the persisting pass then fails the run
/// ("the reference frame changed during the run").
#[test]
fn a_cached_decision_naming_a_changed_member_is_not_adopted() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 5);
    let cfg = crate::stacking::config::project_seed_config(StackingConfig::default());
    assert!(cfg.reference.two_pass, "premise: the project seed keeps the two-pass re-pick on");
    crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
        .unwrap();
    let mut rc = project_rc(&ctx, &conn, "p1", cfg.clone(), &tmp.path().join("work"), &tmp.path().join("out"));
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    stage_calibrate(&mut rc).unwrap();
    stage_measure(&mut rc).unwrap();
    stage_reference(&mut rc).unwrap();

    // The decision an earlier run cached, under the key this run computes now: "switch to <victim>".
    let group = rc.plan_groups[0].clone();
    let reference = rc.reference_frame_id.unwrap();
    let reference_hash = {
        let frame = find_frame_in_groups(&rc.plan_groups, reference).unwrap();
        let conn = db(&rc.ctx).unwrap().conn();
        rc.memo.calibration_hash_checked(&conn, &cfg, &frame).unwrap()
    };
    let victim = group.frames.iter().find(|f| f.frame_id != reference).unwrap().clone();
    let lookup = two_pass_lookup(&mut rc, &group, reference, &reference_hash, false).unwrap();
    store_two_pass_decision(&rc, &group, &lookup, Some(victim.frame_id));
    assert_eq!(
        two_pass_lookup(&mut rc, &group, reference, &reference_hash, false).unwrap().cached,
        Some(Some(victim.frame_id)),
        "premise: the stored decision is a hit before the file changes"
    );

    // The victim's landed file changes, still a readable FITS.
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&victim.path).unwrap();
        file.write_all(&[0u8; 64]).unwrap();
    }

    let ((), logs) = crate::stacking::telemetry::capture_logs(|| stage_register(&mut rc).unwrap());
    assert!(!logs.contains("two-pass decision reused"), "a decision naming a changed member was reused: {logs}");
    assert_ne!(rc.reference_frame_id, Some(victim.frame_id), "the changed member became the reference");
    let entry = rc.measured[&group.key].iter().find(|e| e.frame.frame_id == victim.frame_id).unwrap();
    assert!(!entry.included, "the changed member stayed in the run");
    assert_eq!(entry.reason.as_deref(), Some(crate::stacking::origin::CHANGED_DURING_THE_RUN));
    assert_eq!(
        rc.measured[&group.key].iter().filter(|e| e.included).count(),
        4,
        "the other four members are registered"
    );
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

/// Runs the whole pipeline on the 4-frame sky project, on its grid, with
/// `tweak` applied to the project's stored config. The files' raw
/// `FILTER` is `L`; the canonical filter is `Lum`.
fn run_sky_project(
    tmp: &Path,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    run_sky_project_filter(tmp, "Lum", tweak)
}

fn run_sky_project_filter(
    tmp: &Path,
    canonical: &str,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    run_sky_project_exposures(tmp, canonical, &[300.0; 4], tweak)
}

/// [`run_sky_project_filter`] over one frame per entry of `exposures`.
fn run_sky_project_exposures(
    tmp: &Path,
    canonical: &str,
    exposures: &[f64],
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    run_sky_project_excluding(tmp, canonical, exposures, &[], tweak)
}

/// [`run_sky_project_exposures`] with the frames at the indices in
/// `excluded` on the project's manual exclusion list: stored as the plan
/// reads them, and on the run as the plan hands them over (`rc.excluded`).
fn run_sky_project_excluding(
    tmp: &Path,
    canonical: &str,
    exposures: &[f64],
    excluded: &[usize],
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    RunContext,
    crate::collab::grid::CanonicalGrid,
) {
    let db_path = tmp.join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project_exposures(&conn, &tmp.join("collab"), exposures);
    let excluded_uuids: Vec<String> = excluded.iter().map(|i| format!("l{i}")).collect();
    conn.execute(
        "UPDATE project_frames_local SET filter_canonical = ?1 WHERE project_id = 'p1'",
        [canonical],
    )
    .unwrap();
    let mut cfg = sky_config(&conn);
    tweak(&mut cfg);
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &excluded_uuids,
        None,
    )
    .unwrap();
    let handles: Vec<i64> =
        crate::stacking::project_source::project_frame_source(&conn, "p1", &cfg)
            .unwrap()
            .frames
            .iter()
            .filter(|f| excluded_uuids.contains(&f.frame_uuid))
            .filter_map(|f| f.handle)
            .collect();
    assert_eq!(
        handles.len(),
        excluded_uuids.len(),
        "every excluded frame has a handle"
    );
    let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.join("work"), &tmp.join("out"));
    rc.excluded = handles;
    rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
    run_pipeline(&mut rc).unwrap();
    (ctx, conn, rc, grid)
}

/// Spec §5.6 seam 1, plan 6b-3 D5: a project's LN reference lies on the
/// grid with the pixels no member covers NaN (never data), and every
/// member is normalized on its overlap and cached.
#[test]
fn a_project_ln_reference_leaves_uncovered_pixels_nan() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, conn, rc, _grid) =
        run_sky_project(tmp.path(), |cfg| cfg.normalization.local.enabled = true);
    let group = rc.plan_groups[0].key.clone();
    let reference = PlaneReader::open(&rc.layout.ln_reference_path(&group)).unwrap();
    assert_eq!((reference.width(), reference.height()), (512, 384));
    let plane = reference.read_plane(0).unwrap();
    assert!(plane[0].is_nan(), "a corner no member covers is NaN");
    assert!(plane[192 * 512 + 256].is_finite(), "the centre is data");
    for f in &rc.plan_groups[0].frames {
        assert!(
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group,
                "ln",
                Some(f.frame_id)
            )
            .unwrap()
            .is_some(),
            "{} has its sidecar",
            f.filename
        );
    }
}

/// Seeds the sky project with its folders and confirmed grid configured,
/// without starting anything.
fn seed_startable_sky_project(
    tmp: &Path,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    crate::collab::grid::CanonicalGrid,
) {
    let db_path = tmp.join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_sky_project(&conn, &tmp.join("collab"), 4);
    for (key, dir) in [
        (
            crate::settings::keys::STACKING_WORKING_DIR,
            tmp.join("work"),
        ),
        (crate::settings::keys::STACKING_OUTPUT_DIR, tmp.join("out")),
    ] {
        crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
    }
    crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
    let mut cfg = sky_config(&conn);
    tweak(&mut cfg);
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    (ctx, conn, grid)
}

/// Starts the seeded project through the production entry point.
fn start_seeded_project(ctx: &Arc<ServiceContext>) -> Result<StartedStacking, ApiError> {
    start_stacking(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
    )
}

/// [`seed_startable_sky_project`], started, and waited for.
fn start_sky_project(
    tmp: &Path,
    tweak: impl FnOnce(&mut StackingConfig),
) -> (
    Arc<ServiceContext>,
    rusqlite::Connection,
    StartedStacking,
    crate::collab::grid::CanonicalGrid,
) {
    let (ctx, conn, grid) = seed_startable_sky_project(tmp, tweak);
    let started = start_seeded_project(&ctx).expect("a project run starts");
    wait_for_run(&ctx, started.run_id);
    (ctx, conn, started, grid)
}

/// Spec §2, plan 6b-3 D13: `start_stacking` starts a project. The run
/// queues as "Stacking · <title>", stacks onto the confirmed grid,
/// records it in its summary, and writes into `<output>/<title>/`. Its
/// run row names that same folder.
#[test]
fn a_project_run_starts_on_its_confirmed_grid() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, grid) = seed_startable_sky_project(tmp.path(), |_| {});

    let hold_flag = Arc::new(AtomicBool::new(false));
    let (hold, _hold_job) = ctx
        .compute_queue
        .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
        .unwrap();
    let started = start_seeded_project(&ctx).expect("a project run starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ctx
        .compute_queue
        .snapshot()
        .iter()
        .any(|e| e.label == "Stacking · p1")
    {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            ctx.compute_queue.snapshot()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(hold);
    wait_for_run(&ctx, started.run_id);

    let row = crate::db::stacking::get_run(&conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    // The run validates its folders in Save mode, which canonicalizes.
    let out = crate::test_support::canonical_path(&tmp.path().join("out")).join("p1");
    assert_eq!(
        Path::new(&row.output_dir),
        out,
        "the run row names the folder the run writes into"
    );
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary.grid, Some(grid));
    assert_eq!(summary.project_id.as_deref(), Some("p1"));
    let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
    let master = master_row(&masters, "master");
    assert!(Path::new(&master.path).starts_with(&out), "{}", master.path);
    assert_eq!((master.width, master.height), (512, 384));
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

/// Plan 6b-3 D13: a project whose grid is not confirmed is refused at the
/// plan's `grid` blocker, before anything is written: no run row and no
/// cancel handle are left.
#[test]
fn a_project_without_a_confirmed_grid_is_refused_and_writes_nothing() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, _grid) = seed_startable_sky_project(tmp.path(), |_| {});
    crate::db::collab::set_canonical_grid(&conn, "p1", None).unwrap();
    let err = start_seeded_project(&ctx).expect_err("an unconfirmed grid refuses the start");
    assert!(
        matches!(&err, ApiError::Invalid(m) if m.contains("grid")),
        "{err:?}"
    );
    let runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM stacking_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 0, "no stacking_runs row is written");
    assert!(ctx.active_stacks.lock().unwrap().is_empty());
}

/// Plan 6b-3 D13 (review): a run started through the production entry
/// point carries the plan's confirmed grid in `RunContext.grid`, so its
/// master is on the grid (`ATH_GRID`, `ATH_SPRJ`, the canonical filter),
/// and the plan it started from agrees with it on Register and
/// Normalize: neither is stale after the run (the run folds the same
/// field-aware LN upstream the plan does).
#[test]
fn a_started_project_run_is_grid_consistent_with_its_plan() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, started, grid) =
        start_sky_project(tmp.path(), |cfg| cfg.normalization.local.enabled = true);
    let row = crate::db::stacking::get_run(&conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
    let master = master_row(&masters, "master");
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
    assert_eq!(
        header.get_str("ATH_GRID").as_deref(),
        Some(grid.hash().as_str())
    );
    assert_eq!(header.get_str("ATH_SPRJ").as_deref(), Some("p1"));
    assert_eq!(header.get_str("ATH_RGEO").as_deref(), Some("grid"));
    let canonical: String = conn
        .query_row(
            "SELECT DISTINCT filter_canonical FROM project_frames_local WHERE project_id = 'p1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        header.get_str("FILTER").as_deref(),
        Some(canonical.as_str())
    );
    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        None,
    )
    .unwrap();
    for stage in [Stage::Register, Stage::Normalize] {
        assert!(
            !plan.stale_stages.contains(&stage),
            "{stage:?} stale after the run: {:?}",
            plan.stale_stages
        );
    }
}

/// Plan 6b-3 Task 9 ruling: the composed registered hash when the
/// reference WCS comes from a LOCAL SOLVE. No frame carries a manifest
/// WCS; a fake solver answers each with its true record. After the run
/// the plan agrees on Register and Normalize, the master states the grid,
/// and the reference is one of the solved frames.
#[test]
fn a_project_run_on_locally_solved_wcs_is_grid_consistent_with_its_plan() {
    use crate::stacking::solve::{FakeSolver, SolveOutcome};
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let target = (83.8, -5.4);
    let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
    // A stand-in catalog, so the plan reads an identity, and the fake
    // solver reports it: the run's cached solves are then the plan's.
    let identity = test_fixtures::install_stand_in_catalog(tmp.path());
    let mut solver = FakeSolver::solving_all().with_identity(&identity);
    for i in 0..3usize {
        let uuid = format!("l{i}");
        let dither = ((i as f64 - 1.5) * 0.001, (i % 2) as f64 * 0.0005);
        let mut f = test_fixtures::sky_frame(&uuid, (target.0 + dither.0, target.1 + dither.1));
        f.noise_seed = 100 + i as u64;
        f.manifest_wcs = false;
        let landed = tmp.path().join("collab");
        let path = test_fixtures::add_project_sky_frame(&conn, &landed, "p1", &f, &stars);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let record = test_fixtures::sky_frame_record(&f);
        solver = solver.with(&name, SolveOutcome::Solved(record));
    }
    let grid = crate::collab::grid::CanonicalGrid {
        scale_arcsec: 1.5,
        width_px: 512,
        height_px: 384,
        crval1: target.0,
        crval2: target.1,
        rotation_deg: 0.0,
    };
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
    let mut cfg = sky_config(&conn);
    cfg.normalization.local.enabled = true;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();

    let solver = Arc::new(solver);
    let started = start_stacking_with_solver(
        ctx.clone(),
        Arc::new(NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
        solver.clone(),
    )
    .expect("a project run starts");
    wait_for_run(&ctx, started.run_id);
    let row = crate::db::stacking::get_run(&conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "done", "{row:?}");
    assert_eq!(solver.calls(), 3, "every frame is solved locally");

    let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
    let master = master_row(&masters, "master");
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
    assert_eq!(
        header.get_str("ATH_GRID").as_deref(),
        Some(grid.hash().as_str())
    );
    let summary: RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    let reference = summary.reference.filename.expect("a reference frame");
    let with_manifest_wcs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM project_frames_local \
                 WHERE project_id = 'p1' AND manifest_json LIKE '%\"wcs\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(with_manifest_wcs, 0, "no frame carries a manifest WCS");
    let uuid = reference
        .strip_prefix("c_")
        .and_then(|n| n.strip_suffix(".fits"))
        .unwrap_or_else(|| panic!("unexpected reference name {reference}"));
    let handle = summary.reference.frame_id.expect("a reference frame id");
    let outcome = crate::stacking::solve::stored_outcome(
        &conn,
        "p1",
        handle,
        &blake3_of(&conn, "p1", uuid),
    )
    .unwrap();
    assert!(
        matches!(outcome, Some(SolveOutcome::Solved(_))),
        "the reference has a cached Solved outcome: {outcome:?}"
    );

    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        None,
    )
    .unwrap();
    for stage in [Stage::Register, Stage::Normalize] {
        assert!(
            !plan.stale_stages.contains(&stage),
            "{stage:?} stale after the run: {:?}",
            plan.stale_stages
        );
    }
    assert!(
        plan.warnings
            .iter()
            .all(|w| !w.contains("plate-solved locally")),
        "the run's cached solves are the plan's: {:?}",
        plan.warnings
    );
}

/// Plan/run agreement (plan 6b-3 D6): after a project run with LN on, the
/// plan gate recomputes the masked LN hashes (registration upstream +
/// `lnmask:1`) and does not list Normalize as stale.
#[test]
fn a_project_plan_agrees_with_its_ln_run() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, conn, _rc, grid) =
        run_sky_project(tmp.path(), |cfg| cfg.normalization.local.enabled = true);
    crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
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
    let subject = StackSubject::Project("p1".into());
    let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &subject, None).unwrap();
    assert!(
        !plan.stale_stages.contains(&Stage::Normalize),
        "{:?}",
        plan.stale_stages
    );
}

fn drizzle_2x(cfg: &mut StackingConfig) {
    cfg.drizzle.enabled = true;
    cfg.drizzle.scale = 2;
}

fn written_wcs(header: &crate::fits_parser::FitsHeader) -> solvemyastro::wcs::WcsSolution {
    let f = |k: &str| header.get_f64(k).unwrap_or_else(|| panic!("{k} missing"));
    solvemyastro::wcs::WcsSolution {
        crpix: (f("CRPIX1") - 1.0, f("CRPIX2") - 1.0),
        crval: (f("CRVAL1"), f("CRVAL2")),
        cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
        sip_forward: None,
        sip_reverse: None,
    }
}

fn nearest_sky_star() -> (f64, f64, f64) {
    test_fixtures::sky_stars((83.8, -5.4), 0.12, 50.0, 7)
        .into_iter()
        .min_by(|a, b| {
            (a.0 - 83.8)
                .hypot(a.1 + 5.4)
                .total_cmp(&(b.0 - 83.8).hypot(b.1 + 5.4))
        })
        .unwrap()
}

/// Spec §5.8, §5.9, plan 6b-3 D3, D11: a project master lands in
/// `<output>/<title>/` at the grid's size with the grid's TAN, and its
/// stars sit where that WCS puts their sky positions — the drizzled
/// master too, at twice the size. Both state the canonical filter, the
/// grid's row order, the project and the grid, and claim no plate solve.
#[test]
fn a_project_run_writes_grid_masters_with_the_grid_wcs() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, conn, rc, grid) = run_sky_project(tmp.path(), drizzle_2x);
    let masters = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let master = masters
        .iter()
        .find(|m| m.kind == "master")
        .expect("a master");
    let drizzled = masters
        .iter()
        .find(|m| m.kind == "drizzle")
        .expect("a drizzled master");
    assert!(
        Path::new(&master.path).starts_with(tmp.path().join("out").join("p1")),
        "{}",
        master.path
    );
    assert_eq!((master.width, master.height), (512, 384));
    assert_eq!((drizzled.width, drizzled.height), (1024, 768));
    let star = nearest_sky_star();
    for (path, tol) in [(&master.path, 0.5), (&drizzled.path, 1.0)] {
        let header = crate::fits_parser::FitsHeader::from_path(Path::new(path)).unwrap();
        let wcs = written_wcs(&header);
        let reader = PlaneReader::open(Path::new(path)).unwrap();
        let plane = reader.read_plane(0).unwrap();
        let (x, y) = wcs.sky_to_pixel(star.0, star.1);
        let (mx, my) = centroid_near(&plane, reader.width(), x, y);
        assert!(
            (mx - x).hypot(my - y) < tol,
            "{path}: star at ({mx}, {my}), the WCS says ({x}, {y})"
        );
        assert_eq!(
            header.get_str("FILTER").as_deref(),
            Some("Lum"),
            "the canonical filter"
        );
        assert_eq!(header.get_str("ROWORDER").as_deref(), Some("BOTTOM-UP"));
        assert_eq!(header.get_str("ATH_RGEO").as_deref(), Some("grid"));
        assert_eq!(header.get_str("ATH_SPRJ").as_deref(), Some("p1"));
        assert_eq!(
            header.get_str("ATH_GRID").as_deref(),
            Some(grid.hash().as_str())
        );
        assert!(
            header.get_str("PLTSOLVD").is_none(),
            "a grid TAN is not a plate solve"
        );
    }
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
    assert!((header.get_f64("CRVAL1").unwrap() - 83.8).abs() < 1e-9);
    assert!(
        (header.get_f64("CRPIX1").unwrap() - 256.5).abs() < 1e-9,
        "1-based centre of 512 px"
    );
    assert!((header.get_f64("CRPIX2").unwrap() - 192.5).abs() < 1e-9);
    let dh = crate::fits_parser::FitsHeader::from_path(Path::new(&drizzled.path)).unwrap();
    assert!(
        (dh.get_f64("CRPIX1").unwrap() - 512.5).abs() < 1e-9,
        "{:?}",
        dh.get_f64("CRPIX1")
    );
    assert!(
        (dh.get_f64("CRPIX2").unwrap() - 384.5).abs() < 1e-9,
        "{:?}",
        dh.get_f64("CRPIX2")
    );
    assert!(
        !rc.warnings
            .iter()
            .any(|w| w.contains("no plate solve on the reference frame")),
        "a grid master always has a WCS: {:?}",
        rc.warnings
    );
}

/// The file name of the one master a sky-project run wrote.
fn written_master_name(conn: &rusqlite::Connection, run_id: i64) -> String {
    let masters = crate::db::stacking::list_master_lights(conn, run_id).unwrap();
    let master = masters
        .iter()
        .find(|m| m.kind == "master")
        .expect("a master");
    Path::new(&master.path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Plan 6c D9: a `stops` group of 120 s and 180 s frames is one group, and
/// its master names the range of the frames it integrated (the group's own
/// label would read `120s`).
#[test]
fn a_stops_group_of_mixed_exposures_names_its_master_with_the_range() {
    use crate::stacking::config::ExposureMode;
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, conn, rc, _grid) =
        run_sky_project_exposures(tmp.path(), "Lum", &[120.0, 180.0, 120.0, 180.0], |cfg| {
            cfg.grouping.exposure_mode = ExposureMode::Stops
        });
    assert_eq!(rc.plan_groups.len(), 1, "one group over both exposures");
    assert_eq!(rc.plan_groups[0].exposure_s, Some(120.0));
    assert_eq!(
        written_master_name(&conn, rc.run_id),
        "p1_Lum_mono_120-180s_4x.fits"
    );
}

/// Plan 6c D9, r2: the name is over the frames the master integrated, not
/// over the class. A `stops` class of 60 s x 2 and 120 s x 3 is `60s` by
/// its shortest member; with both 60 s frames excluded the master holds
/// three 120 s frames and says `120s`, and with one excluded it holds a
/// 60 s and three 120 s frames and says the range.
#[test]
fn stops_master_name_follows_the_included_frames() {
    use crate::stacking::config::ExposureMode;
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let exposures = [60.0, 60.0, 120.0, 120.0, 120.0];
    for (excluded, expected) in [
        (&[0usize, 1][..], "p1_Lum_mono_120s_3x.fits"),
        (&[0usize][..], "p1_Lum_mono_60-120s_4x.fits"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (_ctx, conn, rc, _grid) =
            run_sky_project_excluding(tmp.path(), "Lum", &exposures, excluded, |cfg| {
                cfg.grouping.exposure_mode = ExposureMode::Stops
            });
        assert_eq!(
            rc.plan_groups.len(),
            1,
            "{excluded:?}: one group over both exposures"
        );
        assert_eq!(
            rc.plan_groups[0].exposure_s,
            Some(60.0),
            "{excluded:?}: the class's label is its shortest member, excluded or not"
        );
        assert_eq!(
            written_master_name(&conn, rc.run_id),
            expected,
            "{excluded:?}"
        );
    }
}

/// Plan 6c D9: a `seconds` group keeps the cluster's own label, whatever
/// its frames' exposures within the tolerance, so today's names hold; a
/// `stops` group whose frames share one exposure keeps it too.
#[test]
fn a_seconds_group_and_a_uniform_stops_group_keep_the_single_exposure_name() {
    use crate::stacking::config::ExposureMode;
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    for (mode, exposures) in [
        (ExposureMode::Seconds, [120.0, 120.0, 121.0, 121.0]),
        (ExposureMode::Stops, [120.0; 4]),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (_ctx, conn, rc, _grid) =
            run_sky_project_exposures(tmp.path(), "Lum", &exposures, |cfg| {
                cfg.grouping.exposure_mode = mode
            });
        assert_eq!(rc.plan_groups.len(), 1, "{mode:?}");
        assert_eq!(
            written_master_name(&conn, rc.run_id),
            "p1_Lum_mono_120s_4x.fits",
            "{mode:?}"
        );
    }
}

/// Spec §5.1, §5.9 (review I3): no file a project run writes carries
/// `ATH_PRJ`, and every one carries the scanner's skip marker
/// (`ATH_STK` or `ATH_REG`).
#[test]
fn no_file_a_project_run_writes_carries_ath_prj() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let _run = run_sky_project(tmp.path(), |cfg| {
        drizzle_2x(cfg);
        // The weight map is off by default; the test's count names it.
        cfg.drizzle.write_weight_map = true;
    });
    let mut fits: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(tmp.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let p = entry.path();
        // The seeded replicas and the catalog are the test's inputs.
        if p.starts_with(tmp.path().join("collab"))
            || p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("catalog.db"))
        {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        assert!(
            ["fits", "athf", "athm", "rej", "athln", "json", "png", "jpg"].contains(&ext),
            "an unexpected output {}",
            p.display()
        );
        if ext != "fits" {
            continue;
        }
        let header = crate::fits_parser::FitsHeader::from_path(p).unwrap();
        assert!(
            header.get_str("ATH_PRJ").is_none(),
            "{} carries ATH_PRJ",
            p.display()
        );
        assert!(
            header.get_str("ATH_STK").is_some() || header.get_str("ATH_REG").is_some(),
            "{} would be cataloged",
            p.display()
        );
        fits.push(p.to_path_buf());
    }
    // 4 registered frames, the master, its coverage map (the project seed
    // turns `coverageMap` on), the drizzled master, its weight map, and
    // drizzle's own coverage (plan 6b-3 D9: the 9th file), plus the two
    // files of the LN reference (`reference.fits` and
    // `reference-saturation.fits`): rejection normalization defaults to
    // `local` since OD-1a, so a default project run builds one.
    assert_eq!(fits.len(), 11, "{fits:?}");
    let out = tmp.path().join("out").join("p1");
    for name in [
        "p1_Lum_mono_300s_4x.fits",
        "p1_Lum_mono_300s_4x_coverage.fits",
        "p1_Lum_mono_300s_4x_drizzle2x.fits",
        "p1_Lum_mono_300s_4x_drizzle2x_weight.fits",
        "p1_Lum_mono_300s_4x_drizzle2x_coverage.fits",
    ] {
        assert!(fits.contains(&out.join(name)), "{name} missing in {fits:?}");
    }
    // Plan 6b-3 ruling (a): the drizzle weight map and drizzle coverage
    // agree with the drizzled master on the project and the grid.
    let card = |name: &str, k: &str| {
        crate::fits_parser::FitsHeader::from_path(&out.join(name))
            .unwrap()
            .get_str(k)
    };
    for k in ["ATH_SPRJ", "ATH_GRID"] {
        let want = card("p1_Lum_mono_300s_4x_drizzle2x.fits", k);
        assert!(want.is_some(), "the drizzled master carries {k}");
        for name in [
            "p1_Lum_mono_300s_4x_drizzle2x_weight.fits",
            "p1_Lum_mono_300s_4x_drizzle2x_coverage.fits",
        ] {
            assert_eq!(card(name, k), want, "{name} {k}");
        }
    }
}

/// Review M4: a group whose canonical filter is empty writes no `FILTER`
/// card (the reference's raw name never leaks) and the run warns once.
#[test]
fn an_empty_canonical_filter_writes_no_filter_card_and_warns_once() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let (_ctx, conn, rc, _grid) = run_sky_project_filter(tmp.path(), "", |_| {});
    let masters = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
    let master = masters
        .iter()
        .find(|m| m.kind == "master")
        .expect("a master");
    let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
    assert!(header.get_str("FILTER").is_none(), "no FILTER card");
    assert_eq!(header.get_str("ATH_RGEO").as_deref(), Some("grid"));
    let warned = rc
        .warnings
        .iter()
        .filter(|w| w.contains("canonical filter is empty"))
        .count();
    assert_eq!(warned, 1, "{:?}", rc.warnings);
}

/// The fallback fixture: one `L` group of four frames on one field and
/// two frames `EAST_PAIR_OFFSET_WIDTHS` (0.85) of a frame width east —
/// their overlap with the other four is about 15 % of their own
/// footprint — on a grid wide enough for both, with LN on. The star
/// field is the e2e's own (review I5: the same spacing and seed, so the
/// strip registration is tried here first, far cheaper than in Task 9).
fn seed_two_field_project(
    conn: &rusqlite::Connection,
    landed: &Path,
) -> crate::collab::grid::CanonicalGrid {
    crate::db::stacking::insert_test_project(conn, "p1");
    let target: (f64, f64) = (83.8, -5.4);
    let east = test_fixtures::EAST_PAIR_OFFSET_WIDTHS * 384.0 * 1.5
        / 3600.0
        / target.1.to_radians().cos();
    let stars = test_fixtures::sky_stars(
        (target.0 + east / 2.0, target.1),
        0.2,
        test_fixtures::EAST_PAIR_SPACING_ARCSEC,
        test_fixtures::EAST_PAIR_STAR_SEED,
    );
    let mut uuids: Vec<(String, (f64, f64))> = (0..4)
        // seed_sky_project's dithers, for the same reason (D8, review I6).
        .map(|i| {
            (
                format!("a{i}"),
                (
                    target.0 + (i as f64 - 1.5) * 0.001,
                    target.1 + (i % 2) as f64 * 0.0005,
                ),
            )
        })
        .collect();
    uuids.extend((0..2).map(|i| {
        (
            format!("b{i}"),
            (target.0 + east + i as f64 * 0.002, target.1),
        )
    }));
    for (i, (uuid, centre)) in uuids.iter().enumerate() {
        let mut f = test_fixtures::sky_frame(uuid, *centre);
        f.noise_seed = 300 + i as u64;
        test_fixtures::add_project_sky_frame(conn, landed, "p1", &f, &stars);
    }
    crate::collab::grid::CanonicalGrid {
        scale_arcsec: 1.5,
        width_px: 768,
        height_px: 320,
        crval1: target.0 + east / 2.0,
        crval2: target.1,
        rotation_deg: 0.0,
    }
}

/// Spec §5.6, plan 6b-3 D7, D8: a member whose overlap with the LN
/// reference is under 25 % keeps global normalization — no sidecar, a
/// marker, still included — with one warning naming the count; a second
/// run counts it as cached, and so does the plan. The east pair
/// registers over its ≈ 15 % strip (review I5: a failure names itself
/// here, never as a missing warning in Task 9). The plan's Normalize view
/// moves with the LN field before any run does (review I6: the twin).
#[test]
fn a_member_under_a_quarter_overlap_keeps_global_normalization() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_two_field_project(&conn, &tmp.path().join("collab"));
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
    let mut cfg = sky_config(&conn);
    cfg.normalization.local.enabled = true;
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &[],
        None,
    )
    .unwrap();
    let run = |cfg: StackingConfig| {
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
    let rc = run(cfg.clone());
    let group = rc.plan_groups[0].key.clone();
    let entries = &rc.measured[&group];
    for e in entries
        .iter()
        .filter(|e| e.frame.filename.starts_with("c_b"))
    {
        let Some(RegisteredFrameOutcome::Aligned { record, .. }) = &e.registration else {
            panic!(
                "{} did not register over its ≈ 15 % strip: {:?}",
                e.frame.filename, e.reason
            );
        };
        assert!(
            record.matched_stars >= 4,
            "{} registered on {} matched stars",
            e.frame.filename,
            record.matched_stars
        );
    }
    for e in entries {
        assert!(
            e.included,
            "{} stays in the stack: {:?}",
            e.frame.filename, e.reason
        );
        let ln = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group,
            "ln",
            Some(e.frame.frame_id),
        )
        .unwrap();
        let marker = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group,
            LN_GLOBAL_ARTIFACT_KIND,
            Some(e.frame.frame_id),
        )
        .unwrap();
        if e.frame.filename.starts_with("c_b") {
            assert!(
                ln.is_none() && marker.is_some(),
                "{} falls back",
                e.frame.filename
            );
        } else {
            assert!(
                ln.is_some() && marker.is_none(),
                "{} is normalized locally",
                e.frame.filename
            );
        }
    }
    assert_eq!(
        rc.warnings
            .iter()
            .filter(|w| w.contains("2 frame(s)") && w.contains("global normalization"))
            .count(),
        1,
        "{:?}",
        rc.warnings
    );

    let again = run(cfg.clone());
    for e in again.measured[&group]
        .iter()
        .filter(|e| e.frame.filename.starts_with("c_b"))
    {
        assert!(
            e.cached_ln,
            "{}: the marker is a cache hit",
            e.frame.filename
        );
    }
    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        None,
    )
    .unwrap();
    assert!(
        !plan.stale_stages.contains(&Stage::Normalize),
        "{:?}",
        plan.stale_stages
    );

    // Review fix 5: a lost marker is re-derived — the member is
    // normalized again (falls back again) and the marker is rewritten.
    let b0 = again.measured[&group]
        .iter()
        .find(|e| e.frame.filename.starts_with("c_b"))
        .unwrap()
        .frame
        .frame_id;
    let deleted = conn
        .execute(
            "DELETE FROM stacking_artifacts WHERE kind = ?1 AND project_frame = ?2",
            rusqlite::params![LN_GLOBAL_ARTIFACT_KIND, b0],
        )
        .unwrap();
    assert_eq!(deleted, 1, "the marker row of {b0} existed");
    let third = run(cfg.clone());
    let e = third.measured[&group]
        .iter()
        .find(|e| e.frame.frame_id == b0)
        .unwrap();
    assert!(
        !e.cached_ln,
        "{}: no marker, so no cache hit",
        e.frame.filename
    );
    assert!(
        crate::db::stacking::find_artifact(
            &conn,
            &third.subject,
            &group,
            LN_GLOBAL_ARTIFACT_KIND,
            Some(b0)
        )
        .unwrap()
        .is_some(),
        "the marker is rewritten"
    );

    // The twin: excluding a central member moves the field (five frames
    // in scope, three needed), so every LN hash moves — and the plan says
    // so now, not after the next run has re-derived the member list.
    crate::db::stacking::set_project_config(
        &conn,
        "p1",
        &serde_json::to_string(&cfg).unwrap(),
        &["a1".to_string()],
        None,
    )
    .unwrap();
    let plan = build_plan(
        &conn,
        &ctx.settings,
        &PathPolicy::AllowAll,
        &StackSubject::Project("p1".into()),
        None,
    )
    .unwrap();
    assert!(
        plan.stale_stages.contains(&Stage::Normalize),
        "{:?}",
        plan.stale_stages
    );
}

/// The rejection-only twin of the test above: the same two-field project
/// with local normalization OFF for the output and `rejection = local`
/// (the shipped default). The two east frames fall back to the global
/// normalization for rejection, and the one run warning says so without
/// naming a "local normalization" the user never turned on.
#[test]
fn a_member_under_a_quarter_overlap_rejection_only_warns_in_rejection_words() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    let grid = seed_two_field_project(&conn, &tmp.path().join("collab"));
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
    let mut cfg = sky_config(&conn);
    cfg.normalization.rejection = RejectionNormalization::Local;
    cfg.normalization.local.enabled = false;
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

    let group = rc.plan_groups[0].key.clone();
    for e in rc.measured[&group]
        .iter()
        .filter(|e| e.frame.filename.starts_with("c_b"))
    {
        let marker = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group,
            LN_GLOBAL_ARTIFACT_KIND,
            Some(e.frame.frame_id),
        )
        .unwrap();
        assert!(marker.is_some(), "{} falls back", e.frame.filename);
    }
    let overlap: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.contains("2 frame(s)") && w.contains("global normalization"))
        .collect();
    assert_eq!(overlap.len(), 1, "{:?}", rc.warnings);
    assert!(
        overlap[0].starts_with(&format!(
            "group {group}: 2 frame(s) used the global normalization for rejection"
        )),
        "{overlap:?}"
    );
    assert!(
        !rc.warnings
            .iter()
            .any(|w| w.contains("local normalization kept")),
        "{:?}",
        rc.warnings
    );
}

fn master_row<'a>(
    rows: &'a [crate::db::stacking::MasterLight],
    kind: &str,
) -> &'a crate::db::stacking::MasterLight {
    rows.iter()
        .find(|m| m.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} row in {rows:?}"))
}

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
