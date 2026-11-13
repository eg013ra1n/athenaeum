//! Tests of the two-pass reference pick; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::registration::db::get_registration_for_frame_set;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{self, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH};

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
            key,
            ArtifactKind::TwoPass,
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

/// One member recalibrated (its source file's pixels changed, so its calibration hash and with it its
/// registration hash against the candidate move): the key moves, the row is stale, the dry pass runs again and
/// the row is rewritten under the new key.
#[test]
fn two_pass_cache_misses_when_a_member_changes() {
    let rig = TwoPassRig::new();
    let rc1 = rig.run(Arc::new(NullEmitter));
    let key1 = rig.decision_row(&rc1).unwrap().config_hash;

    rewrite_light_pixels(&rig.fixture.conn, rig.light_ids[2]);
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
