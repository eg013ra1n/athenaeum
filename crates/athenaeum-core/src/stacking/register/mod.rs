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
/// It is public because the WCS-seed trigger (ruling R-T2-1) has to read
/// the SAME number the gate is built from — deciding "is a scale step
/// expected here?" by comparing the resulting window against
/// `align::SCALE_RANGE` would be exact float equality on a quotient of two
/// measured quantities, and two plate solves of one rig differ in the
/// fourth digit. One rule, two readers, no way for them to disagree; see
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
            DistortionChoice::Off | DistortionChoice::Auto => None,
        }
    }
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
    pub model: ModelChoice,
    pub distortion: DistortionChoice,
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
            model: ModelChoice::Auto,
            distortion: DistortionChoice::Off,
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

    #[test]
    fn defaults_match_the_spec() {
        let d = RegistrationConfig::default();
        assert_eq!(d.model, ModelChoice::Auto);
        assert_eq!(d.distortion, DistortionChoice::Off);
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
            "\"model\":\"auto\"",
            "\"distortion\":\"off\"",
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
