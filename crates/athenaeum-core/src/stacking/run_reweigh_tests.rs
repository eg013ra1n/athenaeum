//! Plan D D1w: the reweigh in the run (`reweigh_groups`, the floor deferral). A child module of `run` (its private items
//! are in reach). The fixtures render 16-bit star fields of a chosen width and transparency on a 384 x 288 canvas.
//!
//! Why the fixtures look the way they do: the Measure-time weight reads EVERY accepted fit of a frame, and the stars a
//! frame fits differ from sub to sub (a clipped star, a lost one, a refused one). The Gaussian stars rendered here are
//! extrapolated almost exactly even when they clip, so a clipped star would not move a Measure-time weight; the
//! "stars only one sub has" stand in for the ones real subs clip, lose or refuse. The common set must ignore them.

use super::tests::{date_obs_at, run_stages_for_test, seed_run_and_groups, SET_NAME};
use super::*;
use crate::db::stacking::StackingRunFrameRow;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::telemetry::capture_logs;
use crate::stacking::test_fixtures::{self, LightSpec};

const W: usize = 384;
const H: usize = 288;
/// The FWHM the star amplitudes below are quoted at, px.
const REF_FWHM: f64 = 2.4;

/// Which subs have a star.
#[derive(Clone, Copy, PartialEq)]
enum In {
    All,
    Only(usize),
    Except(usize),
}

/// One star of the fixture field: position, its peak amplitude (ADU above the background) at [`REF_FWHM`], and the subs
/// that have it.
#[derive(Clone, Copy)]
struct Star {
    x: f64,
    y: f64,
    amp: f64,
    present: In,
}

/// `n` stars with peaks log-uniform over x6 from `amp0`, on a jittered lattice of `pitch` px cells starting at `offset`
/// (`cols` to a row). The default lattice (48 px, 8 columns) keeps the stars well past the detector's blob radius and
/// the common set's 6 F isolation.
fn lattice(n: usize, amp0: f64, seed: u64, pitch: f64, offset: f64, cols: usize) -> Vec<Star> {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    (0..n)
        .map(|k| {
            let (i, j) = (k % cols, k / cols);
            Star {
                x: offset + pitch * i as f64 + (next() - 0.5) * 16.0,
                y: offset + pitch * j as f64 + (next() - 0.5) * 16.0,
                amp: amp0 * 6f64.powf(next()),
                present: In::All,
            }
        })
        .collect()
}

/// The shared field: at most 32 stars (rows 0 to 3 of the default lattice).
fn field(n: usize, amp0: f64) -> Vec<Star> {
    assert!(n <= 32);
    lattice(n, amp0, 0x9E37_79B9_7F4A_7C15, 48.0, 24.0, 8)
}

/// The fixture's last lattice cell holds one star far past the 16-bit ceiling in every sub: it clips everywhere (so it
/// is never in a common set) and it sets every plane's clip range, which keeps the bright stars below it from reading
/// as "the brightest star of a clip-free plane" in a soft sub.
fn with_clipped_star(mut f: Vec<Star>) -> Vec<Star> {
    f.push(Star {
        x: 24.0 + 48.0 * 7.0,
        y: 24.0 + 48.0 * 5.0,
        amp: 3.0e5,
        present: In::All,
    });
    f
}

/// `count` bright stars (peak `amp` at [`REF_FWHM`]) that only sub `only` has, in rows 4 and 5 of the lattice.
fn with_stars_only_in(mut f: Vec<Star>, count: usize, amp: f64, only: usize) -> Vec<Star> {
    for k in 0..count {
        f.push(Star {
            x: 24.0 + 48.0 * (k % 8) as f64,
            y: 24.0 + 48.0 * (4 + k / 8) as f64,
            amp,
            present: In::Only(only),
        });
    }
    f
}

/// One sub: its FWHM (px) and its transparency (a flux factor).
#[derive(Clone, Copy)]
struct Sub {
    fwhm: f64,
    t: f64,
}

fn render(field: &[Star], sub: Sub, index: usize) -> Vec<(f64, f64, f64)> {
    field
        .iter()
        .filter(|s| match s.present {
            In::All => true,
            In::Only(i) => i == index,
            In::Except(i) => i != index,
        })
        .map(|s| (s.x, s.y, s.amp * sub.t * (REF_FWHM / sub.fwhm).powi(2)))
        .collect()
}

/// The main fixture: 32 stars every sub has, one clipped star every sub has, and ten bright stars only the soft sub
/// (the last) has. At Measure time the soft sub tops the weights (its ten extra stars quadruple its `TFlux`); on the
/// common set the sharpest sub does.
fn main_field() -> Vec<Star> {
    with_stars_only_in(with_clipped_star(field(32, 600.0)), 10, 200_000.0, 5)
}

const MAIN_SUBS: [Sub; 6] = [
    Sub { fwhm: 2.2, t: 1.25 },
    Sub { fwhm: 2.4, t: 1.0 },
    Sub { fwhm: 2.5, t: 1.0 },
    Sub { fwhm: 2.4, t: 1.0 },
    Sub { fwhm: 2.6, t: 1.0 },
    Sub { fwhm: 4.8, t: 1.0 },
];

struct Harness {
    _tmp: tempfile::TempDir,
    ctx: Arc<ServiceContext>,
    cfg: StackingConfig,
    fixture: test_fixtures::Fixture,
    ids: Vec<i64>,
    working: tempfile::TempDir,
    output: tempfile::TempDir,
    rc: RunContext,
}

fn harness(cfg: StackingConfig, field: &[Star], subs: &[Sub]) -> Harness {
    harness_with(cfg, field, subs, None)
}

/// [`harness`] with a declared Bayer pattern on every sub: an OSC group, three planes after stage 1's debayer.
fn harness_with(
    cfg: StackingConfig,
    field: &[Star],
    subs: &[Sub],
    bayerpat: Option<&str>,
) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(conn, SET_NAME);
    let mut ids = Vec::new();
    for (i, sub) in subs.iter().enumerate() {
        let stem = format!("f{i}");
        let date_obs = date_obs_at(i);
        let spec = LightSpec {
            stem: &stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: W,
            height: H,
            exptime: 60.0,
            date_obs: &date_obs,
            bayerpat,
            write_file: true,
        };
        let (id, _) = test_fixtures::add_light_with_field_fwhm(
            &fixture,
            &spec,
            &render(field, *sub, i),
            sub.fwhm,
            600.0,
            5.0,
            100 + i as u64,
        );
        ids.push(id);
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
    test_fixtures::add_master_dark_and_flat(&fixture, &ids, W, H);
    let rc = new_context(&ctx, &cfg, &fixture, &working, &output);
    Harness {
        _tmp: tmp,
        ctx,
        cfg,
        fixture,
        ids,
        working,
        output,
        rc,
    }
}

/// A fresh run row and context on the fixture's catalog and working folder.
fn new_context(
    ctx: &Arc<ServiceContext>,
    cfg: &StackingConfig,
    fixture: &test_fixtures::Fixture,
    working: &tempfile::TempDir,
    output: &tempfile::TempDir,
) -> RunContext {
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
    test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups,
        layout,
        output.path().to_path_buf(),
        group_ids,
    )
}

/// The reference stage and the register stage, as `run_stages_for_test` runs them after Measure.
fn finish_register(rc: &mut RunContext) {
    stage_reference(rc).unwrap();
    stage_register(rc).unwrap();
}

impl Harness {
    fn key(&self) -> String {
        self.rc.plan_groups[0].key.clone()
    }

    fn entry(&self, id: i64) -> &MeasuredFrame {
        self.rc.measured[&self.key()]
            .iter()
            .find(|e| e.frame.frame_id == id)
            .unwrap()
    }

    fn to_measure(&mut self) {
        run_stages_for_test(&mut self.rc, Stage::Measure).unwrap();
    }

    /// Reference and Register after Measure; the log of those two stages.
    fn to_register(&mut self) -> String {
        capture_logs(|| finish_register(&mut self.rc)).1
    }

    /// `normalized_mean` per sub, in sub order; `None` for a frame without a weight.
    fn weights(&self) -> Vec<Option<f64>> {
        self.ids
            .iter()
            .map(|&id| self.entry(id).weight.as_ref().map(|w| w.normalized_mean))
            .collect()
    }

    fn channel(&self, i: usize) -> &crate::stacking::measure::ChannelMeasurement {
        &self.entry(self.ids[i]).measurement.as_ref().unwrap().channels[0]
    }

    fn rows(&self) -> HashMap<i64, StackingRunFrameRow> {
        crate::db::stacking::list_frame_rows(&self.fixture.conn, self.rc.run_id)
            .unwrap()
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect()
    }

    /// The `metrics` artifacts' payloads by frame: what Measure persisted.
    fn metrics_payloads(&self) -> HashMap<i64, String> {
        crate::db::stacking::list_artifacts(
            &self.fixture.conn,
            &StackSubject::FrameSet(self.fixture.set_id),
            Some("metrics"),
        )
        .unwrap()
        .into_iter()
        .map(|a| (a.frame_id.unwrap(), a.payload_json.unwrap()))
        .collect()
    }
}

fn argmax(w: &[Option<f64>]) -> usize {
    w.iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| (i, v)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
        .expect("a weight")
}

/// Plane 0 of a frame row's `metrics_json`.
fn row_plane0(row: &StackingRunFrameRow) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(row.metrics_json.as_deref().unwrap()).unwrap();
    v["channels"][0].clone()
}

#[test]
fn psf_weights_are_read_on_the_frame_common_set_after_register() {
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    let before = h.weights();
    let payloads_before = h.metrics_payloads();
    assert_eq!(payloads_before.len(), 6);
    // Measure time: the soft sub fits ten stars the others do not, and its weight tops the group.
    assert_eq!(argmax(&before), 5, "{before:?}");

    let log = h.to_register();
    let after = h.weights();
    assert_eq!(argmax(&after), 0, "the sharpest, most transparent sub tops: {after:?}");
    let top = after[0].unwrap();
    assert!(
        after[5].unwrap() < 0.5 * top,
        "the soft sub falls once its extra stars are out of the weight: {after:?}"
    );
    assert!(h.ids.iter().all(|&id| h.entry(id).included));

    // The persisted `metrics` artifacts are the Measure-time payloads, byte for byte.
    assert_eq!(h.metrics_payloads(), payloads_before);

    // The frame rows say where the weight came from.
    let rows = h.rows();
    let set = h.rc.common_stars[&h.key()][0].as_ref().expect("a set").len();
    assert_eq!(set, 32, "the 32 stars every sub has, not the clipped one nor the soft sub's ten");
    for &id in &h.ids {
        let p = row_plane0(&rows[&id]);
        assert!(p["commonTflux"].as_f64().unwrap() > 0.0, "{p}");
        assert!(p["commonTmeanFlux"].as_f64().unwrap() > 0.0, "{p}");
        assert_eq!(p["commonStars"].as_u64(), Some(set as u64), "{p}");
    }
    // Weight wiring: PSFSW and PSFSNR of the common totals, in the units Measure stored the noise in.
    for i in 0..6 {
        let c = h.channel(i);
        let (tf, tm) = (c.common_tflux.unwrap(), c.common_tmean_flux.unwrap());
        assert_eq!(
            c.psf_signal_weight,
            psf_signal::psf_signal_weight(tf, tm, c.noise, c.m_star)
        );
        assert_eq!(c.psf_snr, psf_signal::psf_snr(tf, c.noise));
    }
    // The common flux follows the transparency, not the width: the sharpest sub (the anchor) is 1.25 times as
    // transparent as the four sharp ones, and the soft sub's ten extra stars are nowhere in its flux.
    let anchor = h.channel(0).common_tflux.unwrap();
    for i in 1..6 {
        let r = h.channel(i).common_tflux.unwrap() / anchor;
        assert!((r - 0.8).abs() < 0.03, "sub {i}: flux ratio {r}");
    }

    // The logs: one set per plane, one debug line per reweighed frame.
    assert!(log.contains("weights: frame-common star set"), "{log}");
    assert!(log.contains(&format!("anchor_frame_id={}", h.ids[0])), "{log}");
    assert!(log.contains("common_stars=32"), "{log}");
    assert!(log.contains("count=6"), "{log}");
    assert_eq!(log.matches("frame reweighed").count(), 6, "{log}");
    assert!(log.contains("paired_stars=32"), "{log}");
    assert!(
        !log.contains("no frame-common star set"),
        "no fallback expected: {log}"
    );
    assert!(
        h.rc.warnings.iter().all(|w| !w.contains("frame-common")),
        "{:?}",
        h.rc.warnings
    );
}

#[test]
fn the_weight_floor_runs_on_the_common_set_weight_under_psf_snr() {
    let mut cfg = StackingConfig::default();
    cfg.measurement.weight_mode = WeightMode::PsfSnr;
    let mut h = harness(cfg, &main_field(), &MAIN_SUBS);
    h.to_measure();
    let before = h.weights();
    // The sharpest sub's Measure-time PSF SNR is under 5 % of the soft sub's (the ten extra stars): the floor used to
    // drop it before Register.
    assert!(before[0].unwrap() < 0.05, "{before:?}");
    assert!(h.entry(h.ids[0]).included, "stage 3 defers the 5 % floor");
    assert_eq!(h.entry(h.ids[0]).reason, None);

    h.to_register();
    let e = h.entry(h.ids[0]);
    assert!(e.included, "{:?}", e.reason);
    assert!(matches!(
        e.registration,
        Some(RegisteredFrameOutcome::Aligned { .. })
    ));
    let after = h.weights();
    assert_eq!(argmax(&after), 0, "{after:?}");
    assert_eq!(after[0], Some(1.0));
    assert!(h.ids.iter().all(|&id| h.entry(id).included));
}

#[test]
fn a_group_without_a_common_set_keeps_its_measure_time_weights_and_warns() {
    let subs = [
        Sub { fwhm: 2.2, t: 1.0 },
        Sub { fwhm: 2.4, t: 1.0 },
        Sub { fwhm: 2.5, t: 1.0 },
        Sub { fwhm: 2.4, t: 1.0 },
        Sub { fwhm: 2.6, t: 1.0 },
        Sub { fwhm: 2.4, t: 1.0 },
        // A seventh sub at a tenth of the flux: 1 % of the weight.
        Sub { fwhm: 2.4, t: 0.1 },
    ];
    let mut h = harness(StackingConfig::default(), &field(10, 4000.0), &subs);
    h.to_measure();
    let before = h.weights();
    assert!(before[6].unwrap() < 0.02, "{before:?}");

    let log = h.to_register();
    let after = h.weights();
    assert_eq!(&after[..6], &before[..6], "the Measure-time weights, bit for bit");
    assert!(log.contains("weights: no frame-common star set"), "{log}");
    assert!(log.contains("plane=0"), "{log}");
    // The set is the field's ten stars less the ones the faint seventh sub lost: under twenty either way.
    let counted: usize = log
        .split("common_stars=")
        .nth(1)
        .and_then(|t| t.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .expect("a common_stars field on the warn");
    assert!((1..=10).contains(&counted), "{log}");
    assert!(log.contains("fallback=\"measure_time\""), "{log}");
    assert!(log.contains("reason=\"under 20 common stars\""), "{log}");
    assert!(log.contains(&format!("group_key={}", h.key())), "{log}");
    let run_warnings: Vec<_> = h
        .rc
        .warnings
        .iter()
        .filter(|w| w.contains("frame-common star set"))
        .collect();
    assert_eq!(run_warnings.len(), 1, "{:?}", h.rc.warnings);
    // The deferred floor still runs on the Measure-time weights, with the fraction text (PD-6).
    let e = h.entry(h.ids[6]);
    assert!(!e.included);
    let reason = e.reason.clone().unwrap();
    assert!(reason.contains("below 0.05 of the group maximum"), "{reason}");
    assert!(h.ids[..6].iter().all(|&id| h.entry(id).included));
    // No sample was reweighed: the rows carry no common-set fields and the run holds no set.
    for (_, row) in h.rows() {
        assert!(row_plane0(&row).get("commonTflux").is_none());
    }
    assert!(h.rc.common_stars.is_empty());
}

#[test]
fn a_member_without_a_fresh_fits_artifact_sends_the_plane_back_to_its_measure_time_weights() {
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    let before = h.weights();
    // The `.athf` of one sub is deleted between Measure and Register.
    let key = h.key();
    let row = crate::db::stacking::find_artifact(
        &h.fixture.conn,
        &StackSubject::FrameSet(h.fixture.set_id),
        &key,
        "fits.0",
        Some(h.ids[2]),
    )
    .unwrap()
    .expect("the fits artifact");
    std::fs::remove_file(row.path.unwrap()).unwrap();

    let log = h.to_register();
    assert!(log.contains("weights: no frame-common star set"), "{log}");
    assert!(log.contains("reason=\"member without a fresh .athf\""), "{log}");
    assert!(log.contains(&format!("frame_id={}", h.ids[2])), "{log}");
    assert_eq!(h.weights(), before, "the plane keeps its Measure-time weights");
    assert_eq!(
        h.rc.warnings
            .iter()
            .filter(|w| w.contains("frame-common star set"))
            .count(),
        1
    );
    assert!(h.rc.common_stars.is_empty());
}

#[test]
fn a_group_without_an_anchor_falls_back_with_its_own_reason() {
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    h.to_register();
    let after_first = h.weights();
    // Every member's mean FWHM reads zero (no unsaturated star): `pick_anchor` finds nobody.
    let key = h.key();
    for e in h.rc.measured.get_mut(&key).unwrap() {
        for c in &mut e.measurement.as_mut().unwrap().channels {
            c.fwhm_px = 0.0;
        }
    }
    let warnings_before = h.rc.warnings.len();
    let (res, log) = capture_logs(|| reweigh_groups(&mut h.rc));
    res.unwrap();
    assert!(log.contains("weights: no frame-common star set"), "{log}");
    assert!(log.contains("reason=\"no anchor\""), "{log}");
    assert!(!log.contains("under 20 common stars"), "{log}");
    assert_eq!(h.rc.warnings.len(), warnings_before + 1, "{:?}", h.rc.warnings);
    // The plane keeps the inputs it had.
    assert_eq!(h.weights(), after_first);
}

#[test]
fn non_psf_weight_modes_are_untouched_by_the_reweigh() {
    for mode in [WeightMode::Noise, WeightMode::Exposure] {
        let mut cfg = StackingConfig::default();
        cfg.measurement.weight_mode = mode;
        // The sharp subs of a plain field: the soft sub's bright extras would skew the Noise weights alone.
        let mut h = harness(cfg, &field(32, 600.0), &MAIN_SUBS[..5]);
        h.to_measure();
        let weights: Vec<_> = h
            .ids
            .iter()
            .map(|&id| h.entry(id).weight.clone())
            .collect();
        let included: Vec<_> = h.ids.iter().map(|&id| h.entry(id).included).collect();
        let payloads = h.metrics_payloads();

        let log = h.to_register();
        for (i, &id) in h.ids.iter().enumerate() {
            assert_eq!(h.entry(id).weight, weights[i], "{mode:?} sub {i}");
            assert_eq!(h.entry(id).included, included[i], "{mode:?} sub {i}");
        }
        assert_eq!(h.metrics_payloads(), payloads, "{mode:?}");
        assert!(h.rc.common_stars.is_empty(), "{mode:?}");
        assert!(!log.contains("frame-common star set"), "{mode:?}: {log}");
        let rows = h.rows();
        for &id in &h.ids {
            assert!(row_plane0(&rows[&id]).get("commonTflux").is_none(), "{mode:?}");
            assert_eq!(rows[&id].weight, h.entry(id).weight.as_ref().map(|w| w.normalized_mean));
        }
    }
}

#[test]
fn the_reweigh_never_readmits_a_frame_excluded_before_register() {
    // Sub 5 is soft (FWHM 4.8: out at Measure by the FWHM filter). Sub 6 sits in another field altogether: it
    // measures well and cannot be registered (out at Register). Neither may come back, and neither carries a weight
    // on the frames table beside the common-set weights of the other five (PD-6).
    let mut f = field(32, 600.0);
    for s in &mut f {
        s.present = In::Except(6);
    }
    for mut s in lattice(28, 600.0, 0xD1B5_4A32_D192_ED03, 41.0, 17.0, 9) {
        s.present = In::Only(6);
        f.push(s);
    }
    let subs = [
        Sub { fwhm: 2.2, t: 1.25 },
        Sub { fwhm: 2.4, t: 1.0 },
        Sub { fwhm: 2.5, t: 1.0 },
        Sub { fwhm: 2.4, t: 1.0 },
        Sub { fwhm: 2.6, t: 1.0 },
        Sub { fwhm: 4.8, t: 1.0 },
        Sub { fwhm: 2.4, t: 0.6 },
    ];
    let mut cfg = StackingConfig::default();
    cfg.selection.max_fwhm_px = Some(3.0);
    let mut h = harness(cfg, &f, &subs);
    h.to_measure();
    assert!(!h.entry(h.ids[5]).included);
    assert!(h.entry(h.ids[6]).included, "{:?}", h.entry(h.ids[6]).reason);

    h.to_register();
    let soft = h.entry(h.ids[5]);
    assert!(!soft.included);
    assert!(soft.reason.clone().unwrap().contains("FWHM"), "{:?}", soft.reason);
    assert!(soft.registration.is_none());
    assert!(soft.weight.is_none(), "not reweighed: no weight on the common-set scale");
    let lost = h.entry(h.ids[6]);
    assert!(!lost.included);
    assert!(matches!(
        lost.registration,
        Some(RegisteredFrameOutcome::Failed(_))
    ));
    assert!(lost.weight.is_none());

    let w = h.weights();
    assert_eq!(argmax(&w), 0, "{w:?}");
    assert_eq!(w[0], Some(1.0));
    assert!(w[1..5].iter().all(|v| v.is_some()));
    let rows = h.rows();
    for i in [5, 6] {
        let row = &rows[&h.ids[i]];
        assert!(!row.included);
        assert_eq!(row.weight, None);
        assert_eq!(row.weight_channels_json, None);
        assert!(row.metrics_json.is_some(), "the measurement is still on the row");
    }
    assert!(rows[&h.ids[0]].weight.is_some());
}

#[test]
fn the_reweigh_leaves_the_registration_reference_and_rows_unchanged() {
    // PD-11: the registration reference stays the Measure-time pick (the soft sub), though the common-set weight puts
    // another sub on top, and the stored registration rows do not move with the weights.
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    h.to_register();
    assert_eq!(h.rc.reference_frame_id, Some(h.ids[5]));
    assert_eq!(argmax(&h.weights()), 0);
    // The summary's reference weight is the new one.
    let new = h.entry(h.ids[5]).weight.as_ref().unwrap().normalized_mean;
    assert_eq!(h.rc.summary.reference.weight, Some(new));
    assert_eq!(h.rc.summary.reference.frame_id, Some(h.ids[5]));
    assert_eq!(h.rc.summary.reference.switched_from, None);

    let first: Vec<_> = h
        .ids
        .iter()
        .map(|&id| match &h.entry(id).registration {
            Some(RegisteredFrameOutcome::Aligned { registered_hash, .. }) => registered_hash.clone(),
            _ => panic!("frame {id} is not aligned"),
        })
        .collect();

    // A second run on the same catalog reuses every registration row verbatim.
    let mut rc2 = new_context(&h.ctx, &h.cfg, &h.fixture, &h.working, &h.output);
    run_stages_for_test(&mut rc2, Stage::Register).unwrap();
    assert_eq!(rc2.reference_frame_id, Some(h.ids[5]));
    for (i, &id) in h.ids.iter().enumerate() {
        let e = rc2.measured[&h.key()]
            .iter()
            .find(|e| e.frame.frame_id == id)
            .unwrap();
        match &e.registration {
            Some(RegisteredFrameOutcome::Aligned {
                registered_hash,
                cached,
                ..
            }) => {
                assert!(*cached, "sub {i} registered again");
                assert_eq!(registered_hash, &first[i], "sub {i}");
            }
            _ => panic!("frame {id} is not aligned in the second run"),
        }
    }
}

#[test]
fn a_rerun_from_normalize_reweighs_to_the_same_weights() {
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    h.to_register();

    let mut rc2 = new_context(&h.ctx, &h.cfg, &h.fixture, &h.working, &h.output);
    rc2.rerun_from = Some(Stage::Normalize);
    run_stages_for_test(&mut rc2, Stage::Register).unwrap();

    let key = h.key();
    for (i, &id) in h.ids.iter().enumerate() {
        let a = h.entry(id);
        let b = rc2.measured[&key]
            .iter()
            .find(|e| e.frame.frame_id == id)
            .unwrap();
        assert!(b.cached_metrics, "sub {i}: Measure reused its metrics");
        assert!(
            matches!(&b.registration, Some(RegisteredFrameOutcome::Aligned { cached: true, .. })),
            "sub {i}: Register reused its row"
        );
        assert_eq!(a.weight, b.weight, "sub {i}: the same weight, bit for bit");
        let (ca, cb) = (
            &a.measurement.as_ref().unwrap().channels[0],
            &b.measurement.as_ref().unwrap().channels[0],
        );
        assert!(cb.common_tflux.is_some(), "sub {i}: reweighed from the cached fits and map");
        assert_eq!(ca.common_tflux, cb.common_tflux, "sub {i}");
        assert_eq!(ca.common_tmean_flux, cb.common_tmean_flux, "sub {i}");
        assert_eq!(ca.common_stars, cb.common_stars, "sub {i}");
        assert_eq!(ca.psf_signal_weight, cb.psf_signal_weight, "sub {i}");
    }
    assert_eq!(h.rc.common_stars, rc2.common_stars);
}

/// A frame row and a `metrics` payload written before Plan D decode: no `commonTflux` key, a `NULL` weight.
#[test]
fn a_frame_row_written_before_plan_d_decodes() {
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS[..3]);
    h.to_measure();
    // The payload Measure persisted is the pre-Plan D shape (the fields are absent, not null).
    let payload = h.metrics_payloads().remove(&h.ids[0]).unwrap();
    assert!(!payload.contains("commonTflux"), "{payload}");
    let old: FrameMeasurement = serde_json::from_str(&payload).unwrap();
    let c = &old.channels[0];
    assert_eq!((c.common_tflux, c.common_tmean_flux, c.common_stars), (None, None, None));
    // A row of an old run: that payload, and no weight (a frame the old run never weighed).
    let conn = &h.fixture.conn;
    let group_id = h.rc.group_ids[&h.key()];
    crate::db::stacking::upsert_frame_row(
        conn,
        &h.rc.subject,
        &crate::db::stacking::NewFrameRow {
            run_id: h.rc.run_id,
            group_id,
            frame_id: h.ids[0],
            included: false,
            exclusion_reason: Some("registration failed"),
            weight: None,
            weight_channels_json: None,
            metrics_json: Some(&payload),
            reg_status: Some("failed"),
            reg_model: None,
            reg_rms_px: None,
            reg_inliers: None,
            reg_inlier_ratio: None,
            reg_flipped: None,
            rejected_fraction: None,
        },
    )
    .unwrap();
    let rows = h.rows();
    let row = &rows[&h.ids[0]];
    assert_eq!(row.weight, None);
    assert_eq!(row.weight_channels_json, None);
    let back: FrameMeasurement = serde_json::from_str(row.metrics_json.as_deref().unwrap()).unwrap();
    assert_eq!(back.channels[0].common_tflux, None);
}

/// M4 of the D1 review: the match radius reads frame-pixel FWHMs in group pixels, so a map whose scale is not 1 is
/// logged (debug) per member, for D1m to see on a mono-to-OSC group.
#[test]
fn a_map_scale_off_one_is_logged_per_member() {
    use crate::geometry::{Linear, LinearKind, PixelMap};
    let identity = PixelMap::linear(Linear::identity()).unwrap();
    assert!((map_scale(&identity, W, H) - 1.0).abs() < 1e-12);
    let lin = |s: f64| Linear {
        kind: LinearKind::Similarity,
        m: [[s, 0.0, 3.0], [0.0, s, -5.0], [0.0, 0.0, 1.0]],
    };
    let scaled = PixelMap::linear(lin(1.2)).unwrap();
    assert!((map_scale(&scaled, W, H) - 1.2).abs() < 1e-9);

    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    let log = h.to_register();
    assert!(!log.contains("map scale"), "identity maps log nothing: {log}");
    let key = h.key();
    let idx = h.rc.measured[&key]
        .iter()
        .position(|e| e.frame.frame_id == h.ids[1])
        .unwrap();
    if let Some(RegisteredFrameOutcome::Aligned { map, .. }) =
        &mut h.rc.measured.get_mut(&key).unwrap()[idx].registration
    {
        *map = scaled;
    } else {
        panic!("sub 1 is aligned");
    }
    let (res, log) = capture_logs(|| reweigh_groups(&mut h.rc));
    res.unwrap();
    let lines: Vec<_> = log.lines().filter(|l| l.contains("map scale")).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains(&format!("frame_id={}", h.ids[1])), "{}", lines[0]);
    assert!(lines[0].contains("map_scale=1.2"), "{}", lines[0]);
}

/// PD-5: the floor `min_weight_fraction` runs on the common-set weight after Register, on frames that registered, with
/// the stage-3 text. The registration reference (here the soft sub, the Measure-time pick) is not dropped by a floor
/// that did not exist when it was picked: a pinned frame, or the grid's origin, stays in the stack.
#[test]
fn the_floor_drops_registered_frames_but_keeps_the_registration_reference() {
    let mut cfg = StackingConfig::default();
    cfg.selection.min_weight_fraction = 0.6;
    let mut h = harness(cfg, &main_field(), &MAIN_SUBS);
    h.to_measure();
    assert!(h.ids.iter().all(|&id| h.entry(id).included), "nothing is floored at Measure");

    let log = h.to_register();
    assert_eq!(h.rc.reference_frame_id, Some(h.ids[5]));
    let w = h.weights();
    // After the reweigh: 1.0, 0.53, 0.50, 0.70, 0.46, 0.14 (the soft sub, the reference).
    for i in [0, 3] {
        assert!(h.entry(h.ids[i]).included, "sub {i}: {w:?}");
    }
    for i in [1, 2, 4] {
        let e = h.entry(h.ids[i]);
        assert!(!e.included, "sub {i}: {w:?}");
        let reason = e.reason.clone().unwrap();
        assert!(reason.contains("below 0.60 of the group maximum"), "{reason}");
        assert!(matches!(
            e.registration,
            Some(RegisteredFrameOutcome::Aligned { .. })
        ));
        assert!(e.weight.is_some(), "a floored frame keeps the weight it was floored on");
    }
    let reference = h.entry(h.ids[5]);
    assert!(reference.included, "{:?}", reference.reason);
    assert!(reference.weight.as_ref().unwrap().normalized_mean < 0.6);
    assert!(log.contains("weights: reference kept despite the weight floor"), "{log}");
    assert_eq!(
        log.matches("frame excluded").count(),
        3,
        "three frames floored, the reference not: {log}"
    );
    assert!(h
        .rc
        .warnings
        .iter()
        .any(|m| m.contains("reference frame") && m.contains("kept despite")));
    let rows = h.rows();
    assert!(!rows[&h.ids[2]].included);
    assert!(rows[&h.ids[5]].included);
}

/// D1 review: the wiring bug (`MalformedTable`) is never worded as the sky's "under 20 common stars".
#[test]
fn every_fallback_has_its_own_reason() {
    let reasons = [
        Fallback::TooFewStars(19).reason(),
        Fallback::MissingFits(7).reason(),
        Fallback::NoAnchor.reason(),
        Fallback::MalformedTable.reason(),
    ];
    for (i, a) in reasons.iter().enumerate() {
        for b in &reasons[i + 1..] {
            assert_ne!(a, b);
        }
    }
    assert_eq!(Fallback::TooFewStars(19).common_stars(), 19);
    assert_eq!(Fallback::NoAnchor.common_stars(), 0);
    assert_eq!(Fallback::MalformedTable.common_stars(), 0);
}

/// PD-6: the fallback is per (group, plane). On an OSC group one plane's missing `.athf` sends that plane back to its
/// Measure-time weight inputs; the other two are reweighed, and the frame weight is the mean of the three.
#[test]
fn a_plane_that_falls_back_leaves_its_siblings_reweighed() {
    let mut h = harness_with(
        StackingConfig::default(),
        &field(32, 600.0),
        &MAIN_SUBS[..5],
        Some("RGGB"),
    );
    h.to_measure();
    let key = h.key();
    let measure_time: Vec<Vec<f64>> = h
        .ids
        .iter()
        .map(|&id| h.entry(id).weight.as_ref().unwrap().channels.clone())
        .collect();
    assert_eq!(measure_time[0].len(), 3, "an OSC group has three planes");
    let row = crate::db::stacking::find_artifact(
        &h.fixture.conn,
        &StackSubject::FrameSet(h.fixture.set_id),
        &key,
        "fits.1",
        Some(h.ids[3]),
    )
    .unwrap()
    .expect("the plane 1 fits artifact");
    std::fs::remove_file(row.path.unwrap()).unwrap();

    let log = h.to_register();
    assert!(log.contains("plane=1"), "{log}");
    assert!(log.contains("reason=\"member without a fresh .athf\""), "{log}");
    assert_eq!(log.matches("weights: no frame-common star set").count(), 1, "{log}");
    assert_eq!(log.matches("weights: frame-common star set").count(), 2, "{log}");
    let sets = &h.rc.common_stars[&key];
    assert!(sets[0].is_some() && sets[1].is_none() && sets[2].is_some());
    for (i, &id) in h.ids.iter().enumerate() {
        let m = h.entry(id).measurement.as_ref().unwrap();
        assert!(m.channels[0].common_tflux.is_some(), "sub {i} plane 0");
        assert!(m.channels[1].common_tflux.is_none(), "sub {i} plane 1 fell back");
        assert!(m.channels[2].common_tflux.is_some(), "sub {i} plane 2");
        let w = h.entry(id).weight.as_ref().unwrap();
        // Plane 1 is normalized on its own maximum; its raw weight is the Measure-time one, bit for bit.
        assert_eq!(w.channels[1], measure_time[i][1], "sub {i} plane 1 raw weight");
        let mean = w.normalized.iter().sum::<f64>() / 3.0;
        assert!((w.normalized_mean - mean).abs() < 1e-12, "sub {i}");
        assert!(w.channels[0] != measure_time[i][0], "sub {i} plane 0 was reweighed");
    }
}
