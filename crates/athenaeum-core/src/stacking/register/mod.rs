//! Registration v2 (spec §3): star detection on calibrated frames, a
//! quad-seeded correspondence search, RANSAC on the configured linear model,
//! a σ-weighted refit, optional polynomial distortion, per-frame QA and the
//! optional registered-frame writer. Per-frame services only — grouping,
//! reference selection, fan-out and persistence of a whole set belong to
//! the run orchestration. Coordinates are 0-based pixel centres;
//! `PixelMap::forward` maps subject → reference.

pub mod align;
pub mod detect;
pub mod frame;
pub mod local_loop;
pub mod wcs_seed;
pub mod writer;

use serde::{Deserialize, Serialize};

use crate::resample::Interpolation;

/// A linear scale outside `[1 / SCALE_TOLERANCE, SCALE_TOLERANCE]` of the
/// reference fails a frame's registration (spec §3.6, `align::SCALE_RANGE`).
/// M4b (rulings R-M4b-1/7) shares this same tolerance for the plan-time
/// pixel-scale warning (`stacking::plan::build_plan`) — a group the warning
/// flags as "far from the reference" is, by construction, the same
/// situation the per-frame registration gate would refuse a frame over, so
/// both read one constant rather than risking two numbers drifting apart.
pub const SCALE_TOLERANCE: f64 = 1.25;

/// The acceptance window one frame's fitted linear scale must land in
/// (M4b Task 2, ruling R-M4b-2). M1 compared every frame against a fixed
/// `[0.8, 1.25]`, which silently assumed the whole set shares one pixel
/// scale; a set that genuinely mixes scales expects a RATIO, so the window
/// is centred on the frame's own implied ratio to the reference and keeps
/// the same [`SCALE_TOLERANCE`] either side of it.
///
/// The ratio is `frame / reference` when both scales are known, finite and
/// positive — otherwise 1.0, which reproduces `align::SCALE_RANGE` bit for
/// bit, so a set whose frames carry no pixel scale at all behaves exactly
/// as it did before M4b.
pub fn scale_gate_for(frame_scale: Option<f64>, reference_scale: Option<f64>) -> (f64, f64) {
    let ratio = scale_ratio_for(frame_scale, reference_scale);
    (ratio / SCALE_TOLERANCE, ratio * SCALE_TOLERANCE)
}

/// The scale ratio [`scale_gate_for`] centres its window on: `frame /
/// reference` when both scales are known, finite and positive, else 1.0.
///
/// It is public because the WCS-seed trigger (rulings R-T2-1, R-T6-4) has
/// to read the SAME number the gate is built from — deciding "is a scale
/// step expected here?" by comparing the resulting window against
/// `align::SCALE_RANGE` would be exact float equality on a quotient of two
/// measured quantities, and two plate solves of one rig disagree by
/// anything from a part in ten thousand to well over a percent. One rule,
/// two readers, no way for them to disagree; see
/// `wcs_seed::ratio_wants_seed` for the tolerance the trigger applies.
pub fn scale_ratio_for(frame_scale: Option<f64>, reference_scale: Option<f64>) -> f64 {
    match (frame_scale, reference_scale) {
        (Some(frame), Some(reference))
            if frame.is_finite() && reference.is_finite() && frame > 0.0 && reference > 0.0 =>
        {
            frame / reference
        }
        _ => 1.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ModelChoice {
    /// Homography from 30 correspondences, affine from 12, similarity below.
    #[default]
    Auto,
    Similarity,
    Affine,
    Homography,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum DistortionChoice {
    #[default]
    Off,
    Polynomial2,
    Polynomial3,
    Polynomial4,
    /// A regularized thin-plate spline over the refit inliers (M4c,
    /// ruling R-M4c-5): local by construction, so it follows a residual
    /// field a global polynomial of order 2..=4 cannot. `Auto` never
    /// picks it; it is always a deliberate user choice.
    ///
    /// **What it costs** (ruling R-T4-6e). A spline cannot be evaluated
    /// per pixel — 600 logarithms per query — so every stage that
    /// RESAMPLES a frame builds an 8-px displacement grid for it first
    /// (≈ 1 s and ≈ 8.6 MB per direction on a 26 Mpx frame) and hands it
    /// back when that frame is done. A frame is resampled by the
    /// registration writer, by local normalization, by integration and by
    /// drizzle, so a `tps` run pays roughly three to four grid builds per
    /// frame more than a polynomial run — and in exchange never holds
    /// more than a few grids at once, however many frames the set has.
    /// Registration itself pays nothing extra: its residual statistics,
    /// its star re-pairing and the local distortion loop all evaluate the
    /// spline exactly and build no grid at all.
    Tps,
    /// Order 3 for a subject whose geometry differs from the reference's,
    /// when the refit keeps at least `align::AUTO_DISTORTION_MIN_INLIERS`
    /// inliers whose overlap and regularity indices both reach
    /// `align::AUTO_DISTORTION_MIN_OVERLAP` / `align::AUTO_DISTORTION_MIN_REGULARITY`.
    Auto,
}

impl DistortionChoice {
    pub fn order(self) -> Option<u8> {
        match self {
            DistortionChoice::Polynomial2 => Some(2),
            DistortionChoice::Polynomial3 => Some(3),
            DistortionChoice::Polynomial4 => Some(4),
            DistortionChoice::Off | DistortionChoice::Tps | DistortionChoice::Auto => None,
        }
    }
}

/// Which geometry a run's masters are delivered in (spec §3.8, M4b ruling
/// R-M4b-4).
///
/// `CoRegistered` is M1–M4a's behaviour generalized: ONE reference for the
/// whole set, every group resampled into its geometry, so every master of
/// the run shares one pixel grid (and one WCS) whatever pixel scale its own
/// frames were shot at.
///
/// `Native` gives each group its own reference — the group's best-weighted
/// member, two-pass re-picked per group — and therefore its own geometry
/// for local normalization, integration, drizzle, the master's WCS and the
/// rejection bitmaps. No cross-group registration happens at all: a set
/// mixing a bin-1 and a bin-2 train delivers one master per train at its
/// own sampling instead of resampling one into the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum RegistrationGeometry {
    #[default]
    CoRegistered,
    Native,
}

/// Detection cuts. There is no detection sigma: the fast detector's
/// adaptive ladder is threshold-free (it targets `maxStars`), so `minSnr`
/// is the sensitivity dial.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct DetectionConfig {
    pub min_snr: f32,
    pub max_eccentricity: f32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        DetectionConfig {
            min_snr: 10.0,
            max_eccentricity: 0.8,
        }
    }
}

/// Spec §9.2 `registration` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct RegistrationConfig {
    /// M4b (ruling R-M4b-4): co-registered (the default — one reference and
    /// one geometry for the whole set) or native (a reference and a
    /// geometry per group). `#[serde(default)]` rides the struct-level
    /// `default`, so every stored config written before M4b decodes as
    /// co-registered and no `STACKING_CONFIG_VERSION` bump is needed.
    pub geometry: RegistrationGeometry,
    pub model: ModelChoice,
    pub distortion: DistortionChoice,
    /// The thin-plate spline's regularization weight `λ` (M4c, ruling
    /// R-M4c-5) — `0.0` is the interpolating spline, which lands on every
    /// inlier exactly, and a larger value trades node fidelity for a
    /// smoother surface that generalizes between them.
    ///
    /// Unit: px² of the NORMALIZED frame. The kernel is evaluated on
    /// coordinates divided by the node cloud's own diagonal, so `|φ|` is
    /// bounded by 0.184 whatever the sensor, and `λ` is the weight of the
    /// `λ · wᵀw` penalty against displacements measured in pixels. The
    /// useful range therefore depends on the node count (the kernel
    /// block's eigenvalues grow with it): around 0.01 for a few dozen
    /// nodes, roughly an order of magnitude higher at the 600-node cap.
    /// Read only by [`DistortionChoice::Tps`]; ignored by every other
    /// choice.
    pub tps_smoothing: f64,
    /// The local distortion loop (M4c, ruling R-M4c-7): after the first
    /// map, up to `align::LOCAL_DISTORTION_ROUNDS` rounds of re-pairing
    /// every subject star THROUGH the current map at a widening
    /// tolerance, fitting a corrector homography on what is left and
    /// refitting the distortion around it. Needs a distortion model to
    /// refit, so it is a no-op with `distortion: off`.
    pub local_distortion: bool,
    pub interpolation: Interpolation,
    pub clamping_threshold: f32,
    pub max_stars: usize,
    pub ransac_tolerance_px: f64,
    pub ransac_max_iterations: usize,
    pub max_rms_px: f64,
    pub fail_on_max_rms: bool,
    pub detection: DetectionConfig,
    pub write_registered_frames: bool,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        RegistrationConfig {
            geometry: RegistrationGeometry::CoRegistered,
            model: ModelChoice::Auto,
            distortion: DistortionChoice::Off,
            tps_smoothing: 0.0,
            local_distortion: false,
            interpolation: Interpolation::BicubicBSpline,
            clamping_threshold: 0.30,
            max_stars: 2000,
            ransac_tolerance_px: 1.9,
            ransac_max_iterations: 2000,
            max_rms_px: 2.0,
            fail_on_max_rms: false,
            detection: DetectionConfig::default(),
            write_registered_frames: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4b ruling R-M4b-2: the gate is a window around the frame's own
    /// implied scale ratio, and collapses to the M1 fixed window whenever
    /// that ratio is unknown or 1.
    #[test]
    fn the_scale_gate_is_centred_on_the_frames_own_ratio() {
        let fixed = (1.0 / SCALE_TOLERANCE, SCALE_TOLERANCE);
        assert_eq!(scale_gate_for(None, None), fixed);
        assert_eq!(scale_gate_for(Some(0.78), None), fixed);
        assert_eq!(scale_gate_for(None, Some(0.78)), fixed);
        assert_eq!(scale_gate_for(Some(0.78), Some(0.78)), fixed);

        // A software-binned frame against a native-scale reference.
        assert_eq!(scale_gate_for(Some(1.56), Some(0.78)), (1.6, 2.5));
        // … and the other way round.
        let (lo, hi) = scale_gate_for(Some(0.78), Some(1.56));
        assert!((lo - 0.4).abs() < 1e-12 && (hi - 0.625).abs() < 1e-12, "{lo} {hi}");

        // Nonsense scales fall back to the fixed window rather than
        // producing a gate nothing can satisfy.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(scale_gate_for(Some(bad), Some(0.78)), fixed, "{bad}");
            assert_eq!(scale_gate_for(Some(0.78), Some(bad)), fixed, "{bad}");
        }
    }

    /// Ruling R-T2-1: the gate and the WCS-seed trigger read ONE ratio, so
    /// they cannot disagree about whether a scale step is expected.
    #[test]
    fn the_gate_is_built_from_the_shared_ratio() {
        for (frame, reference) in [
            (None, None),
            (Some(0.78), None),
            (None, Some(0.78)),
            (Some(0.78), Some(0.78)),
            (Some(1.56), Some(0.78)),
            (Some(0.78), Some(1.56)),
            (Some(0.7800), Some(0.7803)),
            (Some(f64::NAN), Some(0.78)),
            (Some(0.0), Some(0.78)),
        ] {
            let r = scale_ratio_for(frame, reference);
            assert_eq!(
                scale_gate_for(frame, reference),
                (r / SCALE_TOLERANCE, r * SCALE_TOLERANCE),
                "{frame:?} / {reference:?}"
            );
        }
        assert_eq!(scale_ratio_for(Some(1.56), Some(0.78)), 2.0);
        assert_eq!(scale_ratio_for(None, None), 1.0);
    }

    /// M4b ruling R-M4b-4: the geometry mode is one config field, default
    /// co-registered — a stored document written before M4b decodes to
    /// today's behaviour, and the serde spelling is the spec §9.2 one.
    #[test]
    fn geometry_defaults_to_co_registered_and_spells_itself_in_camel_case() {
        assert_eq!(
            RegistrationGeometry::default(),
            RegistrationGeometry::CoRegistered
        );
        assert_eq!(
            serde_json::to_string(&RegistrationGeometry::CoRegistered).unwrap(),
            "\"coRegistered\""
        );
        assert_eq!(
            serde_json::to_string(&RegistrationGeometry::Native).unwrap(),
            "\"native\""
        );
        let native: RegistrationConfig = serde_json::from_str("{\"geometry\":\"native\"}").unwrap();
        assert_eq!(native.geometry, RegistrationGeometry::Native);
        // … and every other field still carries its own default.
        assert_eq!(native.model, ModelChoice::Auto);
        assert_eq!(native.max_stars, 2000);
    }

    /// M4c rulings R-M4c-5/7: two fields, both `#[serde(default)]`
    /// through the struct-level `default`, so a config document written
    /// before M4c decodes to exactly today's behaviour (interpolating
    /// spline weight, loop off) and no `STACKING_CONFIG_VERSION` bump is
    /// needed. `tps` is a valid `distortion`, and `Auto` never resolves
    /// to it.
    #[test]
    fn tps_smoothing_and_the_local_loop_default_to_off() {
        let d = RegistrationConfig::default();
        assert_eq!(d.tps_smoothing, 0.0);
        assert!(!d.local_distortion);

        let pre_m4c: RegistrationConfig =
            serde_json::from_str("{\"distortion\":\"polynomial3\"}").unwrap();
        assert_eq!(pre_m4c.tps_smoothing, 0.0);
        assert!(!pre_m4c.local_distortion);

        let tps: RegistrationConfig = serde_json::from_str(
            "{\"distortion\":\"tps\",\"tpsSmoothing\":2.5,\"localDistortion\":true}",
        )
        .unwrap();
        assert_eq!(tps.distortion, DistortionChoice::Tps);
        assert_eq!(tps.tps_smoothing, 2.5);
        assert!(tps.local_distortion);

        assert_eq!(
            serde_json::to_string(&DistortionChoice::Tps).unwrap(),
            "\"tps\""
        );
        assert_eq!(DistortionChoice::Tps.order(), None, "a spline has no order");
    }

    #[test]
    fn defaults_match_the_spec() {
        let d = RegistrationConfig::default();
        assert_eq!(d.geometry, RegistrationGeometry::CoRegistered);
        assert_eq!(d.model, ModelChoice::Auto);
        assert_eq!(d.distortion, DistortionChoice::Off);
        assert_eq!(d.tps_smoothing, 0.0);
        assert!(!d.local_distortion);
        assert_eq!(d.interpolation, Interpolation::BicubicBSpline);
        assert_eq!(d.clamping_threshold, 0.30);
        assert_eq!(d.max_stars, 2000);
        assert_eq!(d.ransac_tolerance_px, 1.9);
        assert_eq!(d.ransac_max_iterations, 2000);
        assert_eq!(d.max_rms_px, 2.0);
        assert!(!d.fail_on_max_rms && !d.write_registered_frames);
        assert_eq!(
            (d.detection.min_snr, d.detection.max_eccentricity),
            (10.0, 0.8)
        );
    }

    #[test]
    fn serde_names_and_partial_json() {
        let json = serde_json::to_string(&RegistrationConfig::default()).unwrap();
        for needle in [
            "\"geometry\":\"coRegistered\"",
            "\"model\":\"auto\"",
            "\"distortion\":\"off\"",
            "\"tpsSmoothing\":0.0",
            "\"localDistortion\":false",
            "\"interpolation\":\"bicubicBSpline\"",
            "\"clampingThreshold\":0.3",
            "\"maxStars\":2000",
            "\"ransacTolerancePx\":1.9",
            "\"ransacMaxIterations\":2000",
            "\"maxRmsPx\":2.0",
            "\"failOnMaxRms\":false",
            "\"detection\":{\"minSnr\":10.0,\"maxEccentricity\":0.8}",
            "\"writeRegisteredFrames\":false",
        ] {
            assert!(json.contains(needle), "{needle} missing in {json}");
        }
        assert!(
            !json.contains("sigma"),
            "no detection sigma: the detector is threshold-free"
        );
        let partial: RegistrationConfig = serde_json::from_str("{\"model\":\"homography\",\"distortion\":\"polynomial3\",\"detection\":{\"minSnr\":7.5}}").unwrap();
        assert_eq!(partial.model, ModelChoice::Homography);
        assert_eq!(partial.distortion, DistortionChoice::Polynomial3);
        assert_eq!(partial.detection.min_snr, 7.5);
        assert_eq!(partial.detection.max_eccentricity, 0.8);
        assert_eq!(partial.max_stars, 2000);
        let empty: RegistrationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, RegistrationConfig::default());
        assert_eq!(
            serde_json::to_string(&DistortionChoice::Polynomial4).unwrap(),
            "\"polynomial4\""
        );
        assert_eq!(
            serde_json::to_string(&ModelChoice::Similarity).unwrap(),
            "\"similarity\""
        );
        assert_eq!(DistortionChoice::Polynomial2.order(), Some(2));
        assert_eq!(DistortionChoice::Auto.order(), None);
    }
}
