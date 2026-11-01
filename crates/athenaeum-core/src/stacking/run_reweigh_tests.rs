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
use crate::stacking::weights::FormulaWeights;

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

/// One sub of a harness: the filter it is shot through (a filter is a group), its look, where the field sits on the
/// canvas (a dither, px) and the sensor noise (ADU).
#[derive(Clone, Copy)]
struct Placed {
    filter: Option<&'static str>,
    sub: Sub,
    shift: (f64, f64),
    noise: f32,
}

impl Placed {
    fn new(sub: Sub) -> Placed {
        Placed {
            filter: None,
            sub,
            shift: (0.0, 0.0),
            noise: 5.0,
        }
    }

    fn through(self, filter: &'static str) -> Placed {
        Placed {
            filter: Some(filter),
            ..self
        }
    }

    fn shifted(self, dx: f64, dy: f64) -> Placed {
        Placed {
            shift: (dx, dy),
            ..self
        }
    }

    fn noisy(self, noise: f32) -> Placed {
        Placed { noise, ..self }
    }
}

fn render(field: &[Star], sub: Sub, index: usize, shift: (f64, f64)) -> Vec<(f64, f64, f64)> {
    field
        .iter()
        .filter(|s| match s.present {
            In::All => true,
            In::Only(i) => i == index,
            In::Except(i) => i != index,
        })
        .map(|s| {
            (
                s.x + shift.0,
                s.y + shift.1,
                s.amp * sub.t * (REF_FWHM / sub.fwhm).powi(2),
            )
        })
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
    let placed: Vec<Placed> = subs.iter().map(|&s| Placed::new(s)).collect();
    let h = harness_placed(cfg, field, &placed, bayerpat);
    assert_eq!(h.rc.plan_groups.len(), 1, "{:?}", h.rc.plan_groups);
    h
}

/// A harness over subs that differ in filter (one group per filter), dither and noise.
fn harness_placed(
    cfg: StackingConfig,
    field: &[Star],
    subs: &[Placed],
    bayerpat: Option<&str>,
) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = rusqlite::Connection::open(&db_path).expect("open fixture connection");
    let fixture = test_fixtures::frame_set_with_conn(conn, SET_NAME);
    let mut ids = Vec::new();
    for (i, p) in subs.iter().enumerate() {
        let stem = format!("f{i}");
        let date_obs = date_obs_at(i);
        let spec = LightSpec {
            stem: &stem,
            instrume: "cam",
            filter: p.filter,
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
            &render(field, p.sub, i, p.shift),
            p.sub.fwhm,
            600.0,
            p.noise,
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
    // One master dark and flat per filter.
    let mut filters: Vec<Option<&'static str>> = Vec::new();
    for p in subs {
        if !filters.contains(&p.filter) {
            filters.push(p.filter);
        }
    }
    for f in filters {
        let of_filter: Vec<i64> = subs
            .iter()
            .zip(&ids)
            .filter(|(p, _)| p.filter == f)
            .map(|(_, &id)| id)
            .collect();
        test_fixtures::add_master_dark_and_flat(&fixture, &of_filter, W, H);
    }
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
        self.rc
            .measured
            .values()
            .flatten()
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
    // Each mode with whether its sixth sub is under the 5 % floor at Measure time. The sixth sub has ten times the noise: its
    // SNR is the group's lowest, so a formula that reads the SNR alone gives it no credit at all (weight 0); the Noise
    // weight is a ratio of two noise estimates, which the noise level does not move, and under Exposure the six are equal.
    let snr_only = FormulaWeights {
        fwhm: 0.0,
        eccentricity: 0.0,
        snr: 20.0,
        stars: 0.0,
        pedestal: 0.0,
    };
    for (mode, floored_at_measure) in [
        (WeightMode::Noise, false),
        (WeightMode::Exposure, false),
        (WeightMode::Formula, true),
    ] {
        let mut cfg = StackingConfig::default();
        cfg.measurement.weight_mode = mode;
        cfg.measurement.formula = snr_only;
        // The sharp subs of a plain field (the soft sub's bright extras would skew the weights alone), and the noisy one.
        let mut subs: Vec<Placed> = MAIN_SUBS[..5].iter().map(|&s| Placed::new(s)).collect();
        subs.push(Placed::new(Sub { fwhm: 2.4, t: 1.0 }).noisy(50.0));
        let mut h = harness_placed(cfg, &field(32, 600.0), &subs, None);
        h.to_measure();
        // Stage 3 as it always was: the floor runs at Measure, with the fraction text, and a frame under it is never
        // registered nor flagged.
        let noisy = h.entry(h.ids[5]);
        if floored_at_measure {
            assert!(!noisy.included, "{mode:?}: {:?}", noisy.weight);
            assert!(
                noisy.reason.clone().unwrap().contains("below 0.05 of the group maximum"),
                "{mode:?}: {:?}",
                noisy.reason
            );
        } else {
            assert!(noisy.included, "{mode:?}: {:?}", noisy.reason);
        }
        for &id in &h.ids {
            assert!(!h.entry(id).measure_floored, "{mode:?}: only the deferring modes flag");
        }
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
            assert!(!h.entry(id).measure_floored, "{mode:?} sub {i}");
        }
        if floored_at_measure {
            let noisy = h.entry(h.ids[5]);
            assert!(noisy.registration.is_none(), "{mode:?}: a frame out at Measure is not registered");
            assert!(!noisy.included);
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

/// [`MAIN_SUBS`] with sub 3 brighter (t = 1.1), so that the floor test's cut separates two subs that differ by more than
/// the noise of their renders: sub 1 (2.4 px, t = 1.0) and sub 3 are no longer the same picture.
const FLOOR_SUBS: [Sub; 6] = [
    Sub { fwhm: 2.2, t: 1.25 },
    Sub { fwhm: 2.4, t: 1.0 },
    Sub { fwhm: 2.5, t: 1.0 },
    Sub { fwhm: 2.4, t: 1.1 },
    Sub { fwhm: 2.6, t: 1.0 },
    Sub { fwhm: 4.8, t: 1.0 },
];

/// PD-5 and PD-17: the floor `min_weight_fraction` runs on the common-set weight after Register, on frames that
/// registered, with the stage-3 text. An Auto registration reference (here the soft sub, the Measure-time pick) is
/// excluded like any other frame: it is the grid's origin, not pixels, and nothing after Register needs it in the stack.
#[test]
fn the_floor_drops_registered_frames_and_an_auto_reference_under_it() {
    let mut cfg = StackingConfig::default();
    cfg.selection.min_weight_fraction = 0.6;
    let mut h = harness(cfg, &main_field(), &FLOOR_SUBS);
    h.to_measure();
    assert!(h.ids.iter().all(|&id| h.entry(id).included), "nothing is floored at Measure");

    let log = h.to_register();
    assert_eq!(h.rc.reference_frame_id, Some(h.ids[5]), "the reference does not move");
    let w = h.weights();
    // After the reweigh: 1.0, 0.53, 0.50, ~0.85, 0.46, 0.14 (the soft sub, the reference). The cut at 0.6 has a clear
    // gap on both sides.
    let (kept, dropped) = (0.6 + 0.1, 0.6 - 0.05);
    for i in [0, 3] {
        assert!(h.entry(h.ids[i]).included, "sub {i}: {w:?}");
        assert!(w[i].unwrap() > kept, "sub {i}: {w:?}");
    }
    for i in [1, 2, 4, 5] {
        let e = h.entry(h.ids[i]);
        assert!(!e.included, "sub {i}: {w:?}");
        assert!(w[i].unwrap() < dropped, "sub {i}: {w:?}");
        let reason = e.reason.clone().unwrap();
        assert!(reason.contains("below 0.60 of the group maximum"), "{reason}");
        assert!(matches!(
            e.registration,
            Some(RegisteredFrameOutcome::Aligned { .. })
        ));
        assert!(e.weight.is_some(), "a floored frame keeps the weight it was floored on");
    }
    assert!(!log.contains("kept despite the weight floor"), "{log}");
    assert_eq!(
        log.matches("frame excluded").count(),
        4,
        "four frames floored, the reference among them: {log}"
    );
    assert!(h.rc.warnings.iter().all(|m| !m.contains("kept despite")), "{:?}", h.rc.warnings);
    let rows = h.rows();
    assert!(!rows[&h.ids[2]].included);
    assert!(!rows[&h.ids[5]].included);
    assert!(
        rows[&h.ids[5]]
            .exclusion_reason
            .as_deref()
            .unwrap()
            .contains("below 0.60 of the group maximum")
    );
}

/// PD-17: a MANUAL pin is stage 4's own rule (a pinned frame is forced in despite the selection), so the floor does not
/// drop it; it is kept with a warn and a run warning. The other frames are floored as in the Auto twin.
#[test]
fn the_floor_keeps_a_manual_reference_under_it() {
    let mut cfg = StackingConfig::default();
    cfg.selection.min_weight_fraction = 0.6;
    cfg.reference.mode = ReferenceMode::Manual;
    let mut h = harness(cfg, &main_field(), &FLOOR_SUBS);
    crate::registration::db::set_frame_set_reference(&h.fixture.conn, h.fixture.set_id, h.ids[5]).unwrap();
    h.to_measure();
    let log = h.to_register();
    assert_eq!(h.rc.reference_frame_id, Some(h.ids[5]));
    let w = h.weights();
    for i in [0, 3] {
        assert!(h.entry(h.ids[i]).included, "sub {i}: {w:?}");
    }
    for i in [1, 2, 4] {
        assert!(!h.entry(h.ids[i]).included, "sub {i}: {w:?}");
    }
    let pin = h.entry(h.ids[5]);
    assert!(pin.included, "{:?}", pin.reason);
    assert!(pin.weight.as_ref().unwrap().normalized_mean < 0.6, "{w:?}");
    assert!(log.contains("weights: manual reference kept despite the weight floor"), "{log}");
    assert_eq!(log.matches("frame excluded").count(), 3, "{log}");
    assert!(h
        .rc
        .warnings
        .iter()
        .any(|m| m.contains("manual reference frame") && m.contains("kept despite")));
    assert!(h.rows()[&h.ids[5]].included);
}

/// PD-17, downstream: with the Auto reference excluded by the floor, the group still stacks - the output members are the
/// included aligned frames, and the normalization anchor is its own pick - and the group's master says which frame was the
/// grid's origin.
#[test]
fn a_group_stacks_without_its_excluded_auto_reference() {
    let mut cfg = StackingConfig::default();
    cfg.selection.min_weight_fraction = 0.2;
    let mut h = harness(cfg, &main_field(), &FLOOR_SUBS);
    h.to_measure();
    h.to_register();
    let reference = h.ids[5];
    assert_eq!(h.rc.reference_frame_id, Some(reference));
    assert!(!h.entry(reference).included, "its weight 0.14 is under the 0.20 floor");
    assert!(h.ids[..5].iter().all(|&id| h.entry(id).included));

    stage_output(&mut h.rc).unwrap();
    let key = h.key();
    let group = h.rc.summary.groups.iter().find(|g| g.key == key).expect("the group's summary");
    let master = group.master_path.clone().expect("a master was written");
    assert!(Path::new(&master).exists(), "{master}");
    assert_eq!(group.reference_frame_id, Some(reference));
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

// ── PD-18: the reference pick sees the Measure-time floor ───────────────────────────────────────────────────────────

/// A sub at a tenth of the flux of the sound ones: about 1 % of the group's PSF weight at Measure time, under the 5 % floor
/// that the PSF modes defer.
const FAINT: Sub = Sub { fwhm: 2.4, t: 0.1 };

/// `weight_gate` on the frame's current weight: the premise of a "floored at Measure time" fixture, read before Register
/// (and so before the reweigh overwrites the weight).
fn fails_the_floor(h: &Harness, id: i64) -> bool {
    weight_gate(h.entry(id).weight.as_ref().unwrap(), &h.cfg.selection).is_some()
}

fn pick_entry(id: i64, weight: f64, floored: bool) -> MeasuredFrame {
    let mut e = super::tests::bare_measured_frame(id);
    e.weight = Some(FrameWeight {
        channels: vec![weight],
        normalized: vec![weight],
        mean: weight,
        normalized_mean: weight,
        missing: None,
    });
    e.measure_floored = floored;
    e
}

/// The frame pick and the two-pass candidate list skip a frame under the Measure-time floor, and nothing else changes
/// without the flag.
#[test]
fn a_frame_under_the_measure_time_floor_is_in_no_reference_pick() {
    // A project whose only WCS candidates are under the floor holds no candidate; unflagged, the better one wins.
    let entries = |floored: bool| {
        vec![
            pick_entry(1, 1.0, false),
            pick_entry(2, 0.04, floored),
            pick_entry(3, 0.03, floored),
        ]
    };
    let rule = WcsRule {
        candidates: [2, 3].into_iter().collect(),
        with_wcs: [2, 3].into_iter().collect(),
    };
    assert_eq!(best_candidate_idx(&entries(false), Some(&rule)), Some(1));
    assert_eq!(best_candidate_idx(&entries(true), Some(&rule)), None);
    assert_eq!(best_candidate_idx(&entries(true), None), Some(0));
    assert!(entries(false)[1].in_reference_pick());
    assert!(!entries(true)[1].in_reference_pick());

    // The two-pass candidates: neither the median nor the top-N sees the flagged frames, but the reference stays.
    let pass = |idx: usize, frame_id: i64, dx: f64| PassResult {
        idx,
        frame_id,
        outcome: Ok((0.0, (dx, 0.0))),
    };
    let passes = [pass(0, 10, 0.0), pass(1, 11, 1.0), pass(2, 12, 12.0), pass(3, 13, 12.0)];
    let idxs = |c: Vec<TwoPassCandidate>| c.iter().map(|c| c.idx).collect::<Vec<_>>();
    let measured = vec![
        pick_entry(10, 1.0, false),
        pick_entry(11, 0.9, false),
        pick_entry(12, 0.03, true),
        pick_entry(13, 0.02, true),
    ];
    assert_eq!(idxs(two_pass_candidates(Some(&measured), &passes, 10)), vec![0, 1]);
    let unflagged: Vec<_> = measured.iter().map(|e| pick_entry(e.frame.frame_id, 0.5, false)).collect();
    assert_eq!(idxs(two_pass_candidates(Some(&unflagged), &passes, 10)), vec![0, 1, 2, 3]);
    let flagged_reference = vec![
        pick_entry(10, 0.03, true),
        pick_entry(11, 0.9, false),
        pick_entry(12, 0.03, true),
        pick_entry(13, 0.02, true),
    ];
    assert_eq!(
        idxs(two_pass_candidates(Some(&flagged_reference), &passes, 10)),
        vec![0, 1],
        "the current reference is the identity every deviation is read against"
    );
}

/// PD-18, door (a): group A is the larger only by its three faint subs. They are registered and reweighed, but the Auto
/// pick counts A's three sound subs against B's five, so the reference stays in B, as it did when stage 3 floored them.
#[test]
fn the_reference_group_is_chosen_without_the_frames_under_the_measure_time_floor() {
    let sound = Placed::new(Sub { fwhm: 2.4, t: 1.0 });
    let subs = [
        // Group A: three sound subs and three faint ones (six included at Measure time).
        sound.through("A"),
        sound.through("A"),
        sound.through("A"),
        Placed::new(FAINT).through("A"),
        Placed::new(FAINT).through("A"),
        Placed::new(FAINT).through("A"),
        // Group B: five sound subs, the sharpest first.
        Placed::new(Sub { fwhm: 2.2, t: 1.25 }).through("B"),
        sound.through("B"),
        sound.through("B"),
        sound.through("B"),
        sound.through("B"),
    ];
    let mut h = harness_placed(StackingConfig::default(), &field(32, 1500.0), &subs, None);
    assert_eq!(h.rc.plan_groups.len(), 2);
    h.to_measure();
    let (a, b) = (h.ids[..6].to_vec(), h.ids[6..].to_vec());
    for &id in &a[3..] {
        assert!(fails_the_floor(&h, id), "premise: {id} is under the floor at Measure time");
        assert!(h.entry(id).included, "stage 3 defers the floor");
        assert!(h.entry(id).measure_floored);
    }
    for &id in a[..3].iter().chain(&b) {
        assert!(!fails_the_floor(&h, id), "premise: {id} clears the floor");
        assert!(!h.entry(id).measure_floored);
    }
    // What the Measure-time pick picks in B: its best weight.
    let best_in_b = *b
        .iter()
        .max_by(|&&x, &&y| {
            let w = |id: i64| h.entry(id).weight.as_ref().unwrap().normalized_mean;
            w(x).total_cmp(&w(y))
        })
        .unwrap();

    h.to_register();
    assert_eq!(
        h.rc.reference_frame_id,
        Some(best_in_b),
        "the reference is B's best, not A's (A has 6 included, B 5)"
    );
    assert_eq!(h.rc.summary.reference.switched_from, None);
    // The faint subs are registered and reweighed all the same (PD-5).
    for &id in &a[3..] {
        let e = h.entry(id);
        assert!(matches!(e.registration, Some(RegisteredFrameOutcome::Aligned { .. })), "{id}");
        assert!(e.weight.is_some(), "{id}");
        assert!(e.measurement.as_ref().unwrap().channels[0].common_tflux.is_some(), "{id}");
    }
}

/// PD-18, doors (b) and (c): in a reference group under ten frames, five faint subs framed 12 px away from the three sound
/// ones would take the two-pass median to their framing and enter the top-ten cut, and one of them would become the
/// reference. Seen by the pick they are not: the reference is the Measure-time best and nothing switched.
#[test]
fn frames_under_the_measure_time_floor_do_not_move_the_two_pass_reference() {
    let mut subs = vec![
        Placed::new(Sub { fwhm: 2.2, t: 1.25 }),
        Placed::new(Sub { fwhm: 2.4, t: 1.0 }).shifted(1.0, 0.0),
        Placed::new(Sub { fwhm: 2.5, t: 1.0 }).shifted(0.0, 1.0),
    ];
    subs.extend((0..5).map(|_| Placed::new(FAINT).shifted(12.0, 0.0)));
    let mut cfg = StackingConfig::default();
    assert!(cfg.reference.two_pass, "the default must be on");
    cfg.reference.mode = ReferenceMode::Auto;
    let mut h = harness_placed(cfg, &field(32, 1500.0), &subs, None);
    h.to_measure();
    for &id in &h.ids[3..] {
        assert!(fails_the_floor(&h, id), "premise: {id} is under the floor at Measure time");
    }
    let best = argmax(&h.weights());
    assert_eq!(best, 0, "premise: the sharp, transparent sub is the Measure-time best: {:?}", h.weights());

    h.to_register();
    assert_eq!(h.rc.reference_frame_id, Some(h.ids[0]));
    assert_eq!(h.rc.summary.reference.switched_from, None, "{:?}", h.rc.warnings);
    assert!(h.rc.warnings.iter().all(|w| !w.contains("two-pass")), "{:?}", h.rc.warnings);
    for &id in &h.ids[3..] {
        let e = h.entry(id);
        assert!(matches!(e.registration, Some(RegisteredFrameOutcome::Aligned { .. })), "{id}");
        assert!(e.measure_floored, "{id}");
    }
}

/// m5: a cancel lands between the members of one group's reweigh, not only between groups.
#[test]
fn a_cancel_lands_between_the_members_of_one_groups_reweigh() {
    let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS);
    h.to_measure();
    h.to_register();
    let key = h.key();
    let cfg = h.cfg.clone();
    let mut members = Vec::new();
    for (idx, e) in h.rc.measured[&key].iter().enumerate() {
        let frame = e.frame.clone();
        let calib_hash = {
            let conn = db(&h.rc.ctx).unwrap().conn();
            h.rc.memo.calibration_hash_checked(&conn, &cfg, &frame).unwrap()
        };
        members.push(ReweighMember {
            idx,
            frame_id: frame.frame_id,
            hash: group_measurement_hash(&h.rc, &cfg, &key, &calib_hash),
        });
    }
    let entries = h.rc.measured[&key].as_slice();
    assert!(plane_outcome(&h.rc, &key, entries, &members, 0, 0).is_ok());
    h.rc.cancel.store(true, Ordering::SeqCst);
    let err = plane_outcome(&h.rc, &key, entries, &members, 0, 0).err().expect("a cancelled run");
    assert!(matches!(err, RunError::Cancelled), "{err:?}");
}

/// D1b: `stage_output` hands `integrate_group` the set `reweigh_groups` recorded for the group, so the group's SNR gain
/// is measured on it (version 2); the same group with its record dropped reads no gain, with the warn that names why.
#[test]
fn the_output_stage_measures_the_gain_on_the_recorded_common_set() {
    let mut gains = Vec::new();
    for keep_set in [true, false] {
        let mut h = harness(StackingConfig::default(), &main_field(), &MAIN_SUBS[..5]);
        h.to_measure();
        h.to_register();
        let key = h.key();
        let set = h.rc.common_stars.get(&key).expect("the reweigh recorded the group's set");
        assert_eq!(set.len(), 1);
        assert!(set[0].as_ref().is_some_and(|s| s.len() >= 20), "{set:?}");
        if !keep_set {
            h.rc.common_stars.clear();
        }
        let (res, log) = capture_logs(|| stage_output(&mut h.rc));
        res.unwrap();
        let group = h.rc.summary.groups.iter().find(|g| g.key == key).expect("the group's summary");
        let stats = group.stats.as_ref().expect("the group's stats");
        assert_eq!(stats.snr_gain_version, 2);
        if !keep_set {
            assert!(log.contains("snr_gain not measured") && log.contains("no positions"), "{log}");
        }
        gains.push(stats.snr_gain.clone());
    }
    assert!(gains[0].len() == 1 && gains[0][0] > 1.0 && gains[0][0].is_finite(), "{gains:?}");
    assert_eq!(gains[1], vec![0.0]);
}
