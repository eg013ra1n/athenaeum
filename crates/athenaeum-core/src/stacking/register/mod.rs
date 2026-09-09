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
pub mod writer;

use serde::{Deserialize, Serialize};

use crate::resample::Interpolation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ModelChoice {
    /// Homography from 30 correspondences, affine from 12, similarity below.
    #[default]
    Auto,
    Similarity,
    Affine,
    Homography,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
