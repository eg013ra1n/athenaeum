//! Tests of the integrate and output stages; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::integrate::{LargeScaleRejection, RejectionChoice};
use crate::stacking::rej::RejBitmap;
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, shifted_stars, star_light_spec, wait_for_run,
    wait_for_run_within, LightSpec, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
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
        Some(ArtifactKind::Calibrated),
    )
    .unwrap();
    let mosaics = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some(ArtifactKind::CalibratedMosaic),
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
    // ones (`is_intermediate_kind`).
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
            Some(ArtifactKind::CalibratedMosaic)
        )
        .unwrap()
        .is_empty(),
        "the mosaic rows must go with the calibrated frames"
    );
    assert!(!layout.calibrated_root().exists());
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

/// B1 (M3 final fix wave, I1) run's composite test (b): a second group
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
        ArtifactKind::LnReference,
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
