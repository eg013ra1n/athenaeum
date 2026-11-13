//! Tests of the normalize stage; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, date_obs_at, seed_star_group, shifted_stars, star_light_spec, wait_for_run, LightSpec,
    STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

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
    fn artifacts(&self, kind: ArtifactKind) -> Vec<crate::db::stacking::StackingArtifactRow> {
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

        let ln_artifacts = rig.artifacts(ArtifactKind::Ln);
        assert_eq!(ln_artifacts.len(), 4, "{ln_artifacts:?}");
        let ln_ref_artifacts = rig.artifacts(ArtifactKind::LnReference);
        assert_eq!(ln_ref_artifacts.len(), 1, "{ln_ref_artifacts:?}");
    }

    // the_ln_payload_records_the_background_sample_counts
    // Spec 2026-10-04 §10: every `ln` row records the background model's
    // sample counters, and the retired `cells_rejected` is gone.
    {
        let ln_rows = rig.artifacts(ArtifactKind::Ln);
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
    let ln_rows = rig.artifacts(ArtifactKind::Ln);
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

    let ln_artifacts = rig.artifacts(ArtifactKind::Ln);
    assert_eq!(ln_artifacts.len(), 4, "{ln_artifacts:?}");
    let ln_ref_artifacts = rig.artifacts(ArtifactKind::LnReference);
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

/// The regroup fixture as a rig (frame-level cache spec §7; file identity T4c): six Moffat lights (beta 1.2) of two
/// exposure classes (60 s and 90 s, three each), a master dark and flat, a Manual reference `light_ids[0]`, the
/// working folder `<tmp>/w1` and the output `<tmp>/out` under one tempdir (a test may rename the working folder).
/// A run samples and banks its own inputs: the rig never calls `bank_samples`.
pub(super) struct RegroupRig {
    pub(super) tmp: tempfile::TempDir,
    pub(super) ctx: Arc<ServiceContext>,
    pub(super) fixture: test_fixtures::Fixture,
    pub(super) light_ids: Vec<i64>,
    pub(super) working: PathBuf,
    pub(super) output: PathBuf,
    pub(super) layout: WorkingLayout,
}

impl RegroupRig {
    /// `bayerpat` `Some("RGGB")` seeds a debayered OSC set (as set 270: its registration reads no Measure
    /// output, so neither its own group nor its reference's group enters the registration hash and a merge cannot
    /// move it); `None` seeds mono lights.
    pub(super) fn new(bayerpat: Option<&'static str>) -> Self {
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
                bayerpat,
                ..star_light_spec(&stem, &date_obs)
            };
            // A Moffat field whose true beta (1.2) lies below `BETA_BOUNDS`' floor: every fit pegs at the floor,
            // so every frame, every exposure class and the merged group read the same group beta (as set 270's
            // 1.5). Without it the lower median of the fitted betas moves with the membership and pass 2 re-runs.
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
        let working = tmp.path().join("w1");
        let output = tmp.path().join("out");
        std::fs::create_dir_all(&working).unwrap();
        std::fs::create_dir_all(&output).unwrap();
        for (key, dir) in [
            (crate::settings::keys::STACKING_WORKING_DIR, &working),
            (crate::settings::keys::STACKING_OUTPUT_DIR, &output),
        ] {
            crate::db::set_setting(&fixture.conn, key, dir.to_str().unwrap()).unwrap();
        }
        let layout = WorkingLayout::new(&working, &set_slug(SET_NAME));
        RegroupRig { tmp, ctx, fixture, light_ids, working, output, layout }
    }

    /// Make `working` the working folder of the next run: the setting and the layout (the folder is created when it
    /// does not exist; a test that moved or copied the old one has made it already).
    pub(super) fn point_at(&mut self, working: &Path) {
        std::fs::create_dir_all(working).unwrap();
        crate::db::set_setting(
            &self.fixture.conn,
            crate::settings::keys::STACKING_WORKING_DIR,
            working.to_str().unwrap(),
        )
        .unwrap();
        self.working = working.to_path_buf();
        self.layout = WorkingLayout::new(working, &set_slug(SET_NAME));
    }

    /// The config of the regroup tests: Manual reference, local normalization on.
    pub(super) fn cfg(&self, mode: crate::stacking::config::ExposureMode, tolerance: f64) -> StackingConfig {
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
    }

    /// The plan gate's answer under `cfg` (no file read, no sample read).
    pub(super) fn plan(&self, cfg: &StackingConfig) -> crate::stacking::plan::StackingPlan {
        crate::stacking::plan::build_plan(
            &self.fixture.conn,
            &self.ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(self.fixture.set_id),
            Some(cfg.clone()),
        )
        .expect("the plan should build")
    }

    /// The stages the plan gate reads stale under `cfg` (no file read, no sample read).
    pub(super) fn stale(&self, cfg: &StackingConfig) -> Vec<Stage> {
        self.plan(cfg).stale_stages
    }

    /// One run through Output under `cfg`; returns the groups, the run context and the captured logs.
    pub(super) fn run(&self, cfg: &StackingConfig) -> (Vec<IntegrationGroup>, RunContext, String) {
        let fixture = &self.fixture;
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let (run_id, group_ids) = seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            &self.working,
            &self.output,
        );
        let mut rc = test_context(
            self.ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups.clone(),
            self.layout.clone(),
            self.output.clone(),
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
    }

    /// Every cataloged file (the lights and the masters), as the catalog spells them.
    pub(super) fn catalog_files(&self) -> Vec<PathBuf> {
        let mut stmt = self.fixture.conn.prepare("SELECT path FROM files ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| PathBuf::from(p.unwrap()))
            .collect()
    }

    /// Every file under the working folder (the calibrated, measured, registered and normalized files).
    pub(super) fn working_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.working, &mut out);
        out
    }
}

/// Move the modification time of every file in `paths` by `f` (the catalog rows are left as the scanner wrote them:
/// a copy onto a filesystem of coarser time, not a rescan).
pub(super) fn set_times(paths: &[PathBuf], f: impl Fn(std::time::SystemTime) -> std::time::SystemTime) {
    for path in paths {
        let before = std::fs::metadata(path).unwrap().modified().unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(f(before)).unwrap();
    }
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
    let rig = RegroupRig::new(Some("RGGB"));
    let fixture = &rig.fixture;
    let light_ids = &rig.light_ids;
    let cfg_for = |mode: ExposureMode, tolerance: f64| rig.cfg(mode, tolerance);
    let stale_stages = |cfg: &StackingConfig| rig.stale(cfg);
    let run = |cfg: &StackingConfig| rig.run(cfg);

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
        let mut rows: Vec<_> = rows_of_kind(&fixture.conn, fixture.set_id, ArtifactKind::Registered)
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
    for kind in [ArtifactKind::Calibrated, ArtifactKind::Measured, ArtifactKind::Registered] {
        let rows = rows_of_kind(&fixture.conn, fixture.set_id, kind);
        assert_eq!(rows.len(), 6, "{kind}: {rows:?}");
        assert!(rows.values().all(|r| r.group_key.is_empty()), "{kind}: {rows:?}");
    }
}

/// File-identity FI-6: a mono merge or split keeps every registration while the reference stays the same frame and the
/// groups' betas stay (the fixture pegs every fit at the floor). The Manual reference sits in the 60 s class; under
/// `stops` the 90 s frames join its group, and before the reference's measurement was folded for every one-plane frame
/// that flipped their `ref_measure:` term and re-registered them. The gate reads Register fresh before each run and
/// the run fans out over 0 items in every frame-level stage: the two sides fold the same term.
#[test]
fn mono_regroup_keeps_every_registration_when_the_reference_stays() {
    use crate::stacking::config::ExposureMode;
    let rig = RegroupRig::new(None);
    let cfg_seconds = rig.cfg(ExposureMode::Seconds, 2.0);
    let (groups1, rc1, logs1) = rig.run(&cfg_seconds);
    assert_eq!(groups1.len(), 2, "one group per exposure class: {groups1:?}");
    assert_eq!(rc1.reference_frame_id, Some(rig.light_ids[0]), "the pinned reference");
    assert_eq!(fan_out_items(&logs1, "register", "align"), 6, "{logs1}");
    let registered_rows = || {
        let mut rows: Vec<_> =
            rows_of_kind(&rig.fixture.conn, rig.fixture.set_id, ArtifactKind::Registered)
                .into_values()
                .map(|r| (r.frame_id, r.id, r.created_at, r.config_hash, r.path))
                .collect();
        rows.sort();
        rows
    };
    let registered1 = registered_rows();
    assert_eq!(registered1.len(), 6);

    for (what, cfg, n_groups) in [
        ("stops", rig.cfg(ExposureMode::Stops, 2.0), 1usize),
        ("seconds again", cfg_seconds.clone(), 2usize),
    ] {
        let stale = rig.stale(&cfg);
        assert!(
            !stale.contains(&Stage::Register),
            "the gate reads Register stale before the regroup to {what}: {stale:?}"
        );
        let (groups, rc, logs) = rig.run(&cfg);
        assert_eq!(groups.len(), n_groups, "{what}: {groups:?}");
        assert_eq!(rc.reference_frame_id, Some(rig.light_ids[0]), "{what}: the reference moved");
        assert_nothing_recomputed(&logs, what);
        assert_eq!(registered_rows(), registered1, "{what}: a registered row moved");
    }
}

/// `0` over each of the four frame-level fan-outs, asserted on one run's logs.
fn assert_nothing_recomputed(logs: &str, what: &str) {
    for (stage, pass) in [
        ("calibrate", "calibrate"),
        ("measure", "pass1"),
        ("measure", "pass2"),
        ("register", "align"),
    ] {
        assert_eq!(
            fan_out_items(logs, stage, pass),
            0,
            "{what} redid {stage}/{pass}: {logs}"
        );
    }
}

/// Every file under `root`: path, bytes and modification time, sorted by path (a folder's walk before and after).
fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let time = std::fs::metadata(&path).unwrap().modified().unwrap();
                out.push((path.clone(), std::fs::read(&path).unwrap(), time));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `(kind, path)` of every catalog row that names a file, by path.
fn artifact_files(rig: &RegroupRig) -> Vec<(String, PathBuf)> {
    let mut stmt = rig
        .fixture
        .conn
        .prepare("SELECT kind, path FROM stacking_artifacts WHERE path IS NOT NULL ORDER BY path")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, PathBuf::from(r.get::<_, String>(1)?))))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// The five frame-level fan-outs of one run, each over 0 items: the four of `assert_nothing_recomputed` and LN.
fn assert_nothing_recomputed_with_ln(logs: &str, what: &str) {
    assert_nothing_recomputed(logs, what);
    assert_eq!(
        fan_out_items(logs, "normalize", "normalize"),
        0,
        "{what} redid the local normalization: {logs}"
    );
}

fn whole_seconds(t: std::time::SystemTime) -> std::time::SystemTime {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(d.as_secs())
}

/// File-identity §4.10 (FI-9), the regression that fails before the change: a working folder renamed to another path
/// (its files kept, their times rounded to whole seconds as on a coarser disk) is reused where it now is. Run 2 under
/// the new folder recomputes nothing at calibrate, pass 1, pass 2, align or LN, every file row names the new folder
/// and the old one is never recreated.
#[test]
fn a_moved_working_folder_reuses_every_frame_level_stage() {
    use crate::stacking::config::ExposureMode;
    let mut rig = RegroupRig::new(Some("RGGB"));
    let cfg = rig.cfg(ExposureMode::Seconds, 2.0);
    let w1 = rig.working.clone();
    let (_, _, logs1) = rig.run(&cfg);
    assert_eq!(fan_out_items(&logs1, "calibrate", "calibrate"), 6, "{logs1}");
    assert!(fan_out_items(&logs1, "normalize", "normalize") > 0, "run 1 normalizes: {logs1}");
    let rows1 = artifact_files(&rig);
    assert!(rows1.len() > 6 * 3, "calibrated, measured, registered and LN rows: {rows1:?}");
    assert!(rows1.iter().all(|(_, p)| p.starts_with(&w1)), "run 1 wrote under W1: {rows1:?}");

    let w2 = rig.tmp.path().join("w2");
    std::fs::rename(&w1, &w2).unwrap();
    rig.point_at(&w2);
    set_times(&rig.working_files(), whole_seconds);

    // The gate reads through the move: every stage a run reuses reads cached, and it writes nothing (the rows still
    // name W1 after the plan).
    let stale = rig.stale(&cfg);
    for stage in [Stage::Calibrate, Stage::Measure, Stage::Register, Stage::Normalize] {
        assert!(!stale.contains(&stage), "the plan reads {stage:?} stale after the move: {stale:?}");
    }
    assert_eq!(artifact_files(&rig), rows1, "the plan wrote no row");
    assert!(
        rig.plan(&cfg).warnings.iter().all(|w| !w.contains("cached files were not found")),
        "a moved folder is nothing to warn about"
    );

    let (_, _, logs2) = rig.run(&cfg);
    assert_nothing_recomputed_with_ln(&logs2, "a moved working folder");
    let rows2 = artifact_files(&rig);
    assert_eq!(rows2.len(), rows1.len(), "no row was added or lost: {rows2:?}");
    assert!(rows2.iter().all(|(_, p)| p.starts_with(&w2)), "every file row names W2: {rows2:?}");
    assert!(!w1.exists(), "the old folder was not recreated");

    // The run rebased the rows: the gate has no candidate left and reads the same, with nothing to warn about.
    let subject = StackSubject::FrameSet(rig.fixture.set_id);
    let view = crate::stacking::rebase::RebaseView::for_gate(&rig.fixture.conn, &subject, &rig.layout.root).unwrap();
    assert_eq!((view.moved(), view.left_gone()), (0, 0), "the rebased rows leave the view empty");
    assert!(crate::stacking::rebase::candidates(&rig.fixture.conn, &subject, &rig.layout.root)
        .unwrap()
        .is_empty());
    let stale = rig.stale(&cfg);
    assert!(
        ![Stage::Calibrate, Stage::Measure, Stage::Register, Stage::Normalize].iter().any(|s| stale.contains(s)),
        "{stale:?}"
    );
}

/// A COPY of the working folder (times kept) is used at its new place and the original is left exactly as it was;
/// once the original is removed the next run still reuses the copy.
#[test]
fn a_copied_working_folder_is_used_and_the_original_is_untouched() {
    use crate::stacking::config::ExposureMode;
    let mut rig = RegroupRig::new(Some("RGGB"));
    let cfg = rig.cfg(ExposureMode::Seconds, 2.0);
    let w1 = rig.working.clone();
    let (_, _, logs1) = rig.run(&cfg);
    assert_eq!(fan_out_items(&logs1, "calibrate", "calibrate"), 6, "{logs1}");

    let w2 = rig.tmp.path().join("w2");
    copy_tree_keeping_times(&w1, &w2);
    let original = tree_snapshot(&w1);
    assert!(!original.is_empty());
    rig.point_at(&w2);

    let (_, _, logs2) = rig.run(&cfg);
    assert_nothing_recomputed_with_ln(&logs2, "a copied working folder");
    let rows = artifact_files(&rig);
    assert!(rows.iter().all(|(_, p)| p.starts_with(&w2)), "every file row names W2: {rows:?}");
    assert_eq!(tree_snapshot(&w1), original, "the original folder is untouched");

    std::fs::remove_dir_all(&w1).unwrap();
    let (_, _, logs3) = rig.run(&cfg);
    assert_nothing_recomputed_with_ln(&logs3, "the copy after the original was removed");
    assert!(!w1.exists(), "the removed folder was not recreated");
}

/// The defect the 2 s rule guards against: an older, diverged copy is never read. Run A fills W1; W2 is its copy;
/// run B under W2 recalibrates with another setting (same sizes, other pixels); W1's times go an hour back; W2 is
/// removed; run C at B's config under W1 finds W1's files of the right size but another time. Calibrate covers all 6
/// frames, the run warns once with the number of rows it left (the plan with the number its gate reads), and the planes are B's, not A's.
#[test]
fn a_diverged_copy_is_recomputed_not_read() {
    use crate::stacking::config::ExposureMode;
    let mut rig = RegroupRig::new(Some("RGGB"));
    let cfg_a = rig.cfg(ExposureMode::Seconds, 2.0);
    let mut cfg_b = cfg_a.clone();
    cfg_b.calibration.flat_norm = !cfg_b.calibration.flat_norm;
    let w1 = rig.working.clone();
    let (_, _, logs_a) = rig.run(&cfg_a);
    assert_eq!(fan_out_items(&logs_a, "calibrate", "calibrate"), 6, "{logs_a}");
    let calibrated = |rig: &RegroupRig| -> Vec<(i64, u64, Vec<Vec<u32>>)> {
        let mut rows: Vec<_> = rows_of_kind(&rig.fixture.conn, rig.fixture.set_id, ArtifactKind::Calibrated)
            .into_values()
            .map(|r| {
                let path = PathBuf::from(r.path.unwrap());
                (r.frame_id.unwrap(), std::fs::metadata(&path).unwrap().len(), file_plane_bits(&path))
            })
            .collect();
        rows.sort();
        rows
    };
    let planes_a = calibrated(&rig);
    assert_eq!(planes_a.len(), 6);

    let w2 = rig.tmp.path().join("w2");
    copy_tree_keeping_times(&w1, &w2);
    rig.point_at(&w2);
    let (_, _, logs_b) = rig.run(&cfg_b);
    assert_eq!(fan_out_items(&logs_b, "calibrate", "calibrate"), 6, "B recalibrates: {logs_b}");
    let planes_b = calibrated(&rig);
    assert_eq!(planes_b.len(), 6);
    for (a, b) in planes_a.iter().zip(&planes_b) {
        assert_eq!(a.1, b.1, "premise: the calibrated sizes are equal");
        assert_ne!(a.2, b.2, "premise: the two settings give other pixels");
    }

    // W1 now holds run A's files, an hour older than anything a row of B's recorded.
    set_times(&tree_files(&w1), |t| t - std::time::Duration::from_secs(3600));
    std::fs::remove_dir_all(&w2).unwrap();
    rig.point_at(&w1);
    let left: i64 = {
        let under_w2 = format!("{}{}", w2.display(), std::path::MAIN_SEPARATOR);
        rig.fixture
            .conn
            .query_row(
                "SELECT COUNT(*) FROM stacking_artifacts WHERE substr(path, 1, ?1) = ?2
                   AND size IS NOT NULL AND modified_at IS NOT NULL",
                rusqlite::params![under_w2.len() as i64, under_w2],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(left > 6, "premise: B's rows name W2: {left}");

    // The gate says the same before the run: Calibrate is owed and the warning counts the rows it left. The gate
    // spells the current folder as a run records it, the rig's layout as configured.
    let plan_c = rig.plan(&cfg_b);
    assert!(plan_c.stale_stages.contains(&Stage::Calibrate), "{:?}", plan_c.stale_stages);
    let gate_root = WorkingLayout::new(
        &crate::stacking::paths::run_spelled_working(&w1).unwrap_or_else(|| w1.clone()),
        &set_slug(SET_NAME),
    )
    .root;
    // The plan counts the left rows its gate reads (not the measured rows or the LN files, which it never judges), the
    // run the rows its stages read: the plan's count is a lower bound of the run's, and both name the folder.
    let plan_warning = plan_c
        .warnings
        .iter()
        .find(|w| w.contains("cached files were not found"))
        .unwrap_or_else(|| panic!("the plan says it left rows: {:?}", plan_c.warnings));
    let plan_left: usize = plan_warning.split_whitespace().next().unwrap().parse().unwrap();
    assert!((6..=left as usize).contains(&plan_left), "{plan_warning} of {left} rows");
    assert_eq!(
        *plan_warning,
        crate::stacking::rebase::left_warning(plan_left, &gate_root),
        "the plan names the folder as a run records it"
    );

    let (_, rc_c, logs_c) = rig.run(&cfg_b);
    assert_eq!(fan_out_items(&logs_c, "calibrate", "calibrate"), 6, "{logs_c}");
    let warning = crate::stacking::rebase::left_warning(left as usize, &rig.layout.root);
    assert!(
        rc_c.warnings.contains(&warning),
        "the run says how many rows it left ({warning}): {:?}",
        rc_c.warnings
    );
    assert_eq!(calibrated(&rig), planes_b, "run C wrote B's planes, it did not read A's");
}

/// The "not found" warning goes away after one run: it counts only the rows a stage reads. Run 1 fills W1 with local
/// normalization on; the folder is switched to an empty W2 and W1 is removed, and local normalization is turned off.
/// The plan and run 2 warn about the files they read (the calibrated, measured, registered and plane files) and not
/// about the LN files no stage reads now; run 2 recomputes; the next plan and run 3 carry no such warning, though the
/// LN rows still name the removed W1.
#[test]
fn the_not_found_warning_is_gone_after_one_run() {
    use crate::stacking::config::ExposureMode;
    let mut rig = RegroupRig::new(Some("RGGB"));
    let cfg_on = rig.cfg(ExposureMode::Seconds, 2.0);
    let mut cfg = cfg_on.clone();
    cfg.normalization.local.enabled = false;
    cfg.normalization.rejection = crate::integration::stats::RejectionNormalization::None;
    let w1 = rig.working.clone();
    let (_, _, logs1) = rig.run(&cfg_on);
    assert_eq!(fan_out_items(&logs1, "calibrate", "calibrate"), 6, "{logs1}");
    assert!(fan_out_items(&logs1, "normalize", "normalize") > 0, "run 1 normalizes: {logs1}");

    let w2 = rig.tmp.path().join("w2");
    rig.point_at(&w2);
    std::fs::remove_dir_all(&w1).unwrap();
    let under_w1 = format!("{}{}", w1.display(), std::path::MAIN_SEPARATOR);
    let count_under_w1 = |ln: bool| -> usize {
        rig.fixture
            .conn
            .query_row(
                "SELECT COUNT(*) FROM stacking_artifacts WHERE substr(path, 1, ?1) = ?2
                   AND size IS NOT NULL AND modified_at IS NOT NULL
                   AND (kind IN ('ln', 'ln_reference')) = ?3",
                rusqlite::params![under_w1.len() as i64, under_w1, ln],
                |r| r.get::<_, i64>(0),
            )
            .unwrap() as usize
    };
    let (read_rows, ln_rows) = (count_under_w1(false), count_under_w1(true));
    assert!(read_rows >= 6 && ln_rows > 0, "premise: {read_rows} files a stage reads, {ln_rows} LN files");

    let not_found = |warnings: &[String]| -> Option<String> {
        warnings.iter().find(|w| w.contains("cached files were not found")).cloned()
    };
    let leading_count = |w: &str| -> usize { w.split_whitespace().next().unwrap().parse().unwrap() };

    let plan = rig.plan(&cfg);
    let plan_warning = not_found(&plan.warnings).unwrap_or_else(|| panic!("the plan warns: {:?}", plan.warnings));
    let n = leading_count(&plan_warning);
    assert!((6..=read_rows).contains(&n), "the plan counts the rows its stages read, not the LN files: {plan_warning}");

    let (_, rc2, logs2) = rig.run(&cfg);
    assert_eq!(fan_out_items(&logs2, "calibrate", "calibrate"), 6, "run 2 recomputes: {logs2}");
    let run_warning =
        not_found(&rc2.warnings).unwrap_or_else(|| panic!("run 2 warns: {:?}", rc2.warnings));
    assert_eq!(run_warning, crate::stacking::rebase::left_warning(read_rows, &rig.layout.root));

    // The LN rows still name the removed W1, and nothing reads them: no warning from now on.
    assert_eq!(count_under_w1(true), ln_rows, "premise: the LN rows were left");
    let plan3 = rig.plan(&cfg);
    assert_eq!(not_found(&plan3.warnings), None, "{:?}", plan3.warnings);
    let (_, rc3, logs3) = rig.run(&cfg);
    assert_eq!(not_found(&rc3.warnings), None, "{:?}", rc3.warnings);
    assert_nothing_recomputed(&logs3, "run 3");
}

/// Every file under `root`.
fn tree_files(root: &Path) -> Vec<PathBuf> {
    tree_snapshot(root).into_iter().map(|(p, _, _)| p).collect()
}

/// The file-identity cycle's first regression: a copy of the working tree, the lights and the masters on a filesystem
/// of coarser time (whole seconds, then 100 ns steps) changes no identity. Nothing a frame owns is recomputed, and the
/// gate reads Calibrate, Measure and Register fresh before each run. The run samples and banks its own inputs (no
/// helper does it): before run 1 no light or master row has a `content_hash`, after it the gate reads the stages fresh
/// from the banked samples. The catalog rows keep the times the scanner wrote; a run whose row no longer matches
/// the file samples it again and finds the same sample.
#[test]
fn coarser_times_reuse_every_frame_level_stage() {
    use crate::stacking::config::ExposureMode;
    let rig = RegroupRig::new(Some("RGGB"));
    let unbanked: i64 = rig
        .fixture
        .conn
        .query_row("SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(unbanked, 0, "premise: no light or master row carries a sample before run 1");
    let cfg = rig.cfg(ExposureMode::Seconds, 2.0);
    let (_, _, logs1) = rig.run(&cfg);
    assert_eq!(fan_out_items(&logs1, "calibrate", "calibrate"), 6, "{logs1}");
    assert_eq!(fan_out_items(&logs1, "measure", "pass1"), 6, "{logs1}");
    assert_eq!(fan_out_items(&logs1, "register", "align"), 6, "{logs1}");
    let banked: Vec<String> = {
        // The raw dark and flat subs the masters stand for are no input of a run: they stay unsampled.
        let mut stmt = rig
            .fixture
            .conn
            .prepare(
                "SELECT f.path FROM files f JOIN frames fr ON fr.file_id = f.id
                  WHERE f.content_hash IS NULL AND (fr.is_master = 1 OR fr.imagetyp = 'Light')",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|p| p.unwrap()).collect()
    };
    let with_sample: i64 = rig
        .fixture
        .conn
        .query_row("SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(with_sample, 6 + 2, "the six lights and the two masters carry a sample");
    assert_eq!(banked, Vec::<String>::new(), "run 1 banked every light and master");
    let stale = rig.stale(&cfg);
    for stage in [Stage::Calibrate, Stage::Measure, Stage::Register] {
        assert!(!stale.contains(&stage), "the gate reads {stage:?} stale after run 1: {stale:?}");
    }

    let mut files = rig.catalog_files();
    files.extend(rig.working_files());
    assert!(
        rig.working.starts_with(rig.tmp.path()) && rig.output.starts_with(rig.tmp.path()),
        "the working folder and the output sit under the rig's one tempdir"
    );
    assert!(files.len() > 6 * 2, "the lights, the masters and the working files: {}", files.len());

    let whole_seconds = |t: std::time::SystemTime| {
        let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(d.as_secs())
    };
    let hundred_ns = |t: std::time::SystemTime| {
        let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
        std::time::UNIX_EPOCH
            + std::time::Duration::new(d.as_secs(), (d.subsec_nanos() / 100) * 100 + 100)
    };
    for (what, truncate) in [
        ("whole seconds", &whole_seconds as &dyn Fn(std::time::SystemTime) -> std::time::SystemTime),
        ("100 ns steps", &hundred_ns),
    ] {
        set_times(&files, truncate);
        let stale = rig.stale(&cfg);
        for stage in [Stage::Calibrate, Stage::Measure, Stage::Register] {
            assert!(
                !stale.contains(&stage),
                "the gate reads {stage:?} stale at {what}: {stale:?}"
            );
        }
        let (_, _, logs) = rig.run(&cfg);
        assert_nothing_recomputed(&logs, what);
    }
}

/// The lights' part of the coarser-time case. A copy of the lights and masters onto coarser times, then a scan
/// re-parse of the copies: every row takes the file's new time and loses its sample (`content_hash = NULL`), as the
/// scanner writes it. The gate reads no file, so it reads Calibrate stale until a run has sampled again (FI-7, the
/// residual stays visible). The run samples every light and master again, finds the same samples, recomputes
/// nothing at calibrate, pass 1, pass 2 or align, and banks the samples over the rows once more; the gate is fresh
/// after it.
#[test]
fn a_reparse_of_copied_lights_reuses_every_frame_level_stage() {
    use crate::stacking::config::ExposureMode;
    let rig = RegroupRig::new(Some("RGGB"));
    let cfg = rig.cfg(ExposureMode::Seconds, 2.0);
    let (_, _, logs1) = rig.run(&cfg);
    assert_eq!(fan_out_items(&logs1, "calibrate", "calibrate"), 6, "{logs1}");
    let with_sample = |rig: &RegroupRig| -> i64 {
        rig.fixture
            .conn
            .query_row("SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(with_sample(&rig), 6 + 2, "run 1 banked the six lights and the two masters");

    let mut files = rig.catalog_files();
    files.extend(rig.working_files());
    let whole_seconds = |t: std::time::SystemTime| {
        let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(d.as_secs() + 1)
    };
    set_times(&files, whole_seconds);
    // The re-parse: each catalog row takes its file's new time and drops its sample.
    for path in rig.catalog_files() {
        let live = std::fs::metadata(&path).unwrap().modified().unwrap();
        let time = chrono::DateTime::<chrono::Utc>::from(live).to_rfc3339();
        let n = rig
            .fixture
            .conn
            .execute(
                "UPDATE files SET modified_at = ?1, content_hash = NULL WHERE path = ?2",
                rusqlite::params![time, path.to_str().unwrap()],
            )
            .unwrap();
        assert_eq!(n, 1, "the row of {}", path.display());
    }
    assert_eq!(with_sample(&rig), 0, "the re-parse left no sample");

    let stale = rig.stale(&cfg);
    assert!(
        stale.contains(&Stage::Calibrate),
        "FI-7: the gate reads no file, so it reads Calibrate stale before the run samples again: {stale:?}"
    );
    let (_, _, logs2) = rig.run(&cfg);
    assert_nothing_recomputed(&logs2, "a re-parse of copied lights");
    assert_eq!(with_sample(&rig), 6 + 2, "the run banked the six lights and the two masters again");
    let stale = rig.stale(&cfg);
    for stage in [Stage::Calibrate, Stage::Measure, Stage::Register] {
        assert!(!stale.contains(&stage), "the gate reads {stage:?} stale after the run: {stale:?}");
    }
}

/// The residual the sample accepts, pinned so it stays visible: one byte changed at the middle of a light (the size
/// the same, the time moved by 5 s, no rescan) is read by the gate as fresh (the gate reads no file), and the run that
/// samples the file again finds the new bytes and recomputes exactly that frame: one calibrate, one first pass.
#[test]
fn an_in_window_edit_is_a_miss() {
    use crate::stacking::config::ExposureMode;
    let rig = RegroupRig::new(Some("RGGB"));
    let cfg = rig.cfg(ExposureMode::Seconds, 2.0);
    rig.run(&cfg);

    let victim: String = rig
        .fixture
        .conn
        .query_row(
            "SELECT path FROM files WHERE id = (SELECT file_id FROM frames WHERE id = ?1)",
            [rig.light_ids[4]],
            |r| r.get(0),
        )
        .unwrap();
    let before = std::fs::metadata(&victim).unwrap();
    {
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&victim).unwrap();
        let middle = before.len() / 2;
        let mut byte = [0u8; 1];
        file.seek(SeekFrom::Start(middle)).unwrap();
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0x01;
        file.seek(SeekFrom::Start(middle)).unwrap();
        file.write_all(&byte).unwrap();
        file.set_modified(before.modified().unwrap() + std::time::Duration::from_secs(5))
            .unwrap();
    }
    let after = std::fs::metadata(&victim).unwrap();
    assert_eq!(after.len(), before.len(), "a same-size edit");
    assert_ne!(after.modified().unwrap(), before.modified().unwrap());

    let stale = rig.stale(&cfg);
    for stage in [Stage::Calibrate, Stage::Measure, Stage::Register] {
        assert!(
            !stale.contains(&stage),
            "the gate reads no file: {stage:?} reads fresh before the run samples the edit: {stale:?}"
        );
    }
    let (_, _, logs) = rig.run(&cfg);
    assert_eq!(fan_out_items(&logs, "calibrate", "calibrate"), 1, "{logs}");
    assert_eq!(fan_out_items(&logs, "measure", "pass1"), 1, "{logs}");
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
    let ref_rows = rig.artifacts(ArtifactKind::LnReference);
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
/// forces a REAL registration change the established way (change a
/// source file's pixels, same technique
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

    // Change light_ids[0]'s pixels (one byte; the catalog row stays as scanned) — forces recalibration (and,
    // downstream, re-registration) of that ONE frame on the next run.
    rewrite_light_pixels(&rig.fixture.conn, rig.light_ids[0]);

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

    let ln_artifacts = rig.artifacts(ArtifactKind::Ln);
    assert!(ln_artifacts.is_empty(), "{ln_artifacts:?}");
    let ln_ref_artifacts = rig.artifacts(ArtifactKind::LnReference);
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
        ArtifactKind::LnReference,
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
                ArtifactKind::Ln,
                Some(f.frame_id)
            )
            .unwrap()
            .is_some(),
            "{} has its sidecar",
            f.filename
        );
    }
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
            ArtifactKind::Ln,
            Some(e.frame.frame_id),
        )
        .unwrap();
        let marker = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group,
            ArtifactKind::LnGlobal,
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
            rusqlite::params![ArtifactKind::LnGlobal.as_db_str(), b0],
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
            ArtifactKind::LnGlobal,
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
            ArtifactKind::LnGlobal,
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

/// The write order at the LN sidecars (file-identity spec §4.2): a member's `ln` row is deleted before its sidecar
/// is rewritten, so a run cancelled at the fan-out leaves no row of the old hash. Run 1 normalizes locally; run 2
/// moves the star selection (the LN hash with it) and is cancelled right before the normalize fan-out. It proves
/// the delete order only, not wrong pixels: the hook cancels before any sidecar is written.
#[test]
fn a_cancelled_normalization_leaves_no_row_of_the_old_hash() {
    let rig = LnRig::new();
    let cfg_1 = ln_stored_config();
    let plan_groups = rig.plan_groups(&cfg_1);
    assert_eq!(plan_groups.len(), 1, "{plan_groups:?}");
    let mut rc_1 = rig.context(cfg_1.clone(), plan_groups);
    run_register_and_output(&mut rc_1);
    let rows_1 = rig.artifacts(ArtifactKind::Ln);
    assert_eq!(rows_1.len(), 4, "premise: run 1 wrote a row per frame: {rows_1:?}");
    let hashes_1: std::collections::HashSet<String> =
        rows_1.iter().map(|r| r.config_hash.clone()).collect();

    let mut cfg_2 = cfg_1.clone();
    cfg_2.normalization.local.star_selection.min_peak_snr += 1.0;
    let plan_groups = rig.plan_groups(&cfg_2);
    let mut rc_2 = rig.context(cfg_2, plan_groups);
    let dir = rig.working.path().to_path_buf();
    LN_CANCEL_BEFORE_FAN_OUT.lock().unwrap().push(dir.clone());
    let result = run_stages_for_test(&mut rc_2, Stage::Register).and_then(|()| stage_output(&mut rc_2));
    LN_CANCEL_BEFORE_FAN_OUT.lock().unwrap().retain(|d| d != &dir);
    assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");

    let rows_2 = rig.artifacts(ArtifactKind::Ln);
    assert!(
        rows_2.iter().all(|r| !hashes_1.contains(&r.config_hash)),
        "an ln row of run 1's hash survived the cancelled run: {rows_2:?}"
    );
}
