//! Registration stars (spec §3.1): the detector with Moffat centroid
//! refinement on a luminance plane, then the saturation / eccentricity /
//! SNR cuts and the brightest-`maxStars` truncation.

use std::sync::Arc;

use astroimage::ImageAnalyzer;

use super::DetectionConfig;
use crate::stacking::measure::ADU_SCALE;

/// Absolute saturation level in native `[0, 1]` units: a star whose
/// `peak + background` reaches it has a flat top and no usable centroid.
pub const SATURATION: f32 = 0.95;

/// Calibrated frames are in [0, 1]; a plane whose finite maximum exceeds
/// this was never scaled down (a float32 source keeps ADU) and every star
/// would fail the saturation cut after the `ADU_SCALE` multiply.
pub const NATIVE_UNITS_MAX: f32 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    pub x: f64,
    pub y: f64,
    pub flux: f64,
    /// Fitted centroid σ per axis (px) when the detector's refinement
    /// succeeded; `None` keeps the pass-1 centroid unweighted.
    pub sigma: Option<(f64, f64)>,
}

/// `0.25 R + 0.5 G + 0.25 B` for three planes, the plane itself for one,
/// the plain mean otherwise. Planes are row-major `width × height`.
pub fn luminance(planes: &[&[f32]]) -> Vec<f32> {
    match planes {
        [p] => p.to_vec(),
        [r, g, b] => r
            .iter()
            .zip(g.iter())
            .zip(b.iter())
            .map(|((r, g), b)| 0.25 * r + 0.5 * g + 0.25 * b)
            .collect(),
        [] => Vec::new(),
        _ => {
            let k = 1.0 / planes.len() as f32;
            let mut acc = vec![0.0f32; planes[0].len()];
            for p in planes {
                for (a, v) in acc.iter_mut().zip(p.iter()) {
                    *a += k * v;
                }
            }
            acc
        }
    }
}

/// Detect registration stars on one luminance plane in native units. The
/// detector runs on an ADU-scaled copy (its thresholds are tuned for 16-bit
/// data); results come back in pixel coordinates and native flux.
pub fn detect_stars(
    lum: &[f32],
    w: usize,
    h: usize,
    cfg: &DetectionConfig,
    max_stars: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<Star> {
    if w < 8 || h < 8 || lum.len() < w * h {
        return Vec::new();
    }
    let max = lum
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f32, f32::max);
    if max > NATIVE_UNITS_MAX {
        tracing::warn!(
            max,
            "plane exceeds native units; the saturation cut will drop every star"
        );
    }
    let scaled: Vec<f32> = lum.iter().map(|v| v * ADU_SCALE).collect();
    let mut analyzer = ImageAnalyzer::new()
        .with_max_stars((max_stars + max_stars / 4).max(8))
        .with_centroid_refine(true);
    if let Some(p) = pool {
        analyzer = analyzer.with_thread_pool(Arc::clone(p));
    }
    let result = match analyzer.detect_fast_data(&scaled, w, h, 1) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "star detection failed; no registration stars");
            return Vec::new();
        }
    };
    let saturation = SATURATION * ADU_SCALE;
    let mut stars: Vec<Star> = result
        .stars
        .iter()
        .filter(|s| s.peak + result.background < saturation)
        .filter(|s| s.eccentricity <= cfg.max_eccentricity)
        .filter(|s| s.snr >= cfg.min_snr)
        .map(|s| Star {
            x: s.x as f64,
            y: s.y as f64,
            flux: (s.flux / ADU_SCALE) as f64,
            sigma: (s.sx > 0.0 && s.sy > 0.0).then(|| (s.sx as f64, s.sy as f64)),
        })
        .collect();
    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(max_stars);
    stars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

    fn near(stars: &[Star], x: f64, y: f64, r: f64) -> Option<&Star> {
        stars
            .iter()
            .find(|s| (s.x - x).abs() <= r && (s.y - y).abs() <= r)
    }

    #[test]
    fn luminance_weights_and_passthrough() {
        let r = [1.0f32, 0.0, 0.0];
        let g = [0.0f32, 1.0, 0.0];
        let b = [0.0f32, 0.0, 1.0];
        assert_eq!(luminance(&[&r, &g, &b]), vec![0.25, 0.5, 0.25]);
        assert_eq!(luminance(&[&r]), r.to_vec());
    }

    #[test]
    fn cuts_saturated_elongated_and_faint_stars_and_keeps_the_brightest() {
        let (w, h) = (300, 300);
        // 40 ordinary stars on a grid, plus the three that must be cut.
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for j in 0..5 {
            for i in 0..8 {
                stars.push((
                    30.0 + i as f64 * 34.0,
                    40.0 + j as f64 * 50.0,
                    0.05 + 0.01 * (i + j) as f64,
                ));
            }
        }
        stars.push((150.0, 150.0, 0.95)); // saturated: peak + background = 1.0
        stars.push((60.0, 280.0, 0.003)); // faint: peak ≈ 1.5 σ
        let mut data = gaussian_field(w, h, &stars, 1.8, 0.05);
        let streak = moffat_field(
            w,
            h,
            &[MoffatStar {
                x: 240.0,
                y: 280.0,
                amp: 0.3,
                alpha_x: 8.0,
                alpha_y: 2.0,
                theta: 0.3,
            }],
            4.0,
            0.05,
        );
        for (d, s) in data.iter_mut().zip(streak) {
            *d += s - 0.05;
        }
        add_noise(&mut data, 0.002, 5);
        let cfg = DetectionConfig::default();
        let found = detect_stars(&data, w, h, &cfg, 2000, None);
        assert!(found.len() >= 36, "found {}", found.len());
        assert!(
            near(&found, 30.0, 40.0, 0.3).is_some(),
            "an ordinary star is missing"
        );
        assert!(
            near(&found, 150.0, 150.0, 3.0).is_none(),
            "saturated star kept"
        );
        assert!(near(&found, 60.0, 280.0, 3.0).is_none(), "faint star kept");
        assert!(
            near(&found, 240.0, 280.0, 4.0).is_none(),
            "elongated star kept"
        );
        assert!(
            found.windows(2).all(|p| p[0].flux >= p[1].flux),
            "not sorted by flux"
        );
        assert!(
            found.iter().filter(|s| s.sigma.is_some()).count() >= found.len() / 2,
            "refined σ missing on most stars"
        );
        // A smaller cap changes the detector's threshold and therefore every
        // star's aperture flux by ~1 %, so compare the truncated list by star
        // identity, not by flux.
        let top = detect_stars(&data, w, h, &cfg, 10, None);
        assert_eq!(top.len(), 10);
        for t in &top {
            assert!(
                found[..12]
                    .iter()
                    .any(|f| (f.x - t.x).abs() < 0.5 && (f.y - t.y).abs() < 0.5),
                "top-10 star ({}, {}) is not among the 12 brightest of the full run",
                t.x,
                t.y
            );
        }
    }

    #[test]
    fn empty_and_flat_planes_yield_no_stars() {
        assert!(detect_stars(&[], 0, 0, &DetectionConfig::default(), 100, None).is_empty());
        assert!(detect_stars(
            &vec![0.1f32; 64 * 64],
            64,
            64,
            &DetectionConfig::default(),
            100,
            None
        )
        .is_empty());
    }
}
