//! Deterministic RANSAC over the linear models (spec §3.2) with the four
//! model-quality indexes of the math reference §5.2 — inlier ratio,
//! overlap (convex-hull area covered by the inliers), regularity (grid
//! occupancy of the inliers) and RMS — and the σ-weighted iterative
//! refit that follows a successful sample.

use super::linear::{fit_linear, residuals, rms, Linear, LinearKind, Pair};

/// SplitMix64 — tiny, deterministic, good enough to draw samples.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    #[inline]
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

#[derive(Clone, Debug)]
pub struct RansacConfig {
    pub kind: LinearKind,
    pub tolerance_px: f64,
    pub max_iterations: usize,
    pub seed: u64,
    pub min_inliers: usize,
    pub frame_width: f64,
    pub frame_height: f64,
}

impl RansacConfig {
    pub fn new(kind: LinearKind, frame_width: f64, frame_height: f64) -> RansacConfig {
        RansacConfig {
            kind,
            tolerance_px: 1.9,
            max_iterations: 2000,
            seed: 0x5EED_5EED,
            min_inliers: 8,
            frame_width,
            frame_height,
        }
    }

    fn sample_size(&self) -> usize {
        match self.kind {
            LinearKind::Similarity => 2,
            LinearKind::Affine => 3,
            LinearKind::Homography => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    pub inlier_ratio: f64,
    pub overlap: f64,
    pub regularity: f64,
    pub score: f64,
}

#[derive(Clone, Debug)]
pub struct RansacResult {
    pub linear: Linear,
    pub inliers: Vec<usize>,
    pub rms_px: f64,
    pub quality: Quality,
    pub iterations: usize,
}

/// Convex hull area by Andrew's monotone chain (shoelace), 0 for < 3 points.
fn hull_area(mut pts: Vec<(f64, f64)>) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup();
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut lower: Vec<(f64, f64)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(f64, f64)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    let n = lower.len();
    let mut area = 0.0;
    for i in 0..n {
        let (x1, y1) = lower[i];
        let (x2, y2) = lower[(i + 1) % n];
        area += x1 * y2 - x2 * y1;
    }
    area.abs() / 2.0
}

/// Fraction of a 4×4 grid over the frame that holds at least one inlier.
fn regularity(pts: &[(f64, f64)], w: f64, h: f64) -> f64 {
    const G: usize = 4;
    let mut cells = [false; G * G];
    for &(x, y) in pts {
        let cx = ((x / w) * G as f64).floor().clamp(0.0, (G - 1) as f64) as usize;
        let cy = ((y / h) * G as f64).floor().clamp(0.0, (G - 1) as f64) as usize;
        cells[cy * G + cx] = true;
    }
    cells.iter().filter(|c| **c).count() as f64 / (G * G) as f64
}

fn quality(pairs: &[Pair], inliers: &[usize], rms_px: f64, cfg: &RansacConfig) -> Quality {
    let inlier_ratio = inliers.len() as f64 / pairs.len().max(1) as f64;
    let all_ref: Vec<(f64, f64)> = pairs.iter().map(|p| p.1).collect();
    let in_ref: Vec<(f64, f64)> = inliers.iter().map(|&i| pairs[i].1).collect();
    let all_area = hull_area(all_ref);
    let overlap = if all_area > 0.0 {
        (hull_area(in_ref.clone()) / all_area).min(1.0)
    } else {
        0.0
    };
    let regularity = regularity(&in_ref, cfg.frame_width, cfg.frame_height);
    let score = inlier_ratio * overlap * regularity / (1.0 + rms_px);
    Quality {
        inlier_ratio,
        overlap,
        regularity,
        score,
    }
}

/// A sample is degenerate when two subject points nearly coincide or (for
/// 3+ points) three of them are nearly collinear.
fn degenerate(sample: &[Pair]) -> bool {
    for i in 0..sample.len() {
        for j in (i + 1)..sample.len() {
            let (a, b) = (sample[i].0, sample[j].0);
            if (a.0 - b.0).abs() < 1.0 && (a.1 - b.1).abs() < 1.0 {
                return true;
            }
        }
    }
    if sample.len() >= 3 {
        for i in 0..sample.len() {
            for j in (i + 1)..sample.len() {
                for k in (j + 1)..sample.len() {
                    let (a, b, c) = (sample[i].0, sample[j].0, sample[k].0);
                    let area2 = ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs();
                    if area2 < 4.0 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn inliers_of(t: &Linear, pairs: &[Pair], tol: f64) -> Vec<usize> {
    residuals(t, pairs)
        .iter()
        .enumerate()
        .filter(|(_, r)| **r <= tol)
        .map(|(i, _)| i)
        .collect()
}

/// RANSAC: minimal samples → `fit_linear` → inlier count under
/// `tolerance_px`; the adaptive iteration bound
/// `N = log(1 − 0.9999) / log(1 − w^k)` shrinks as the best inlier ratio
/// `w` grows; early exit above 98 % inliers. The winner is refit on all
/// its inliers and scored.
pub fn ransac_fit(pairs: &[Pair], cfg: &RansacConfig) -> Option<RansacResult> {
    let k = cfg.sample_size();
    let n = pairs.len();
    if n < k.max(cfg.min_inliers) {
        return None;
    }
    let mut rng = SplitMix64(cfg.seed);
    let mut best: Option<(Vec<usize>, Linear)> = None;
    let mut needed = cfg.max_iterations;
    let mut iterations = 0usize;
    let mut sample: Vec<Pair> = Vec::with_capacity(k);
    let mut picks: Vec<usize> = Vec::with_capacity(k);
    while iterations < needed.min(cfg.max_iterations) {
        iterations += 1;
        picks.clear();
        while picks.len() < k {
            let p = rng.below(n);
            if !picks.contains(&p) {
                picks.push(p);
            }
        }
        sample.clear();
        sample.extend(picks.iter().map(|&i| pairs[i]));
        if degenerate(&sample) {
            continue;
        }
        let Some(t) = fit_linear(cfg.kind, &sample, None) else {
            continue;
        };
        let inl = inliers_of(&t, pairs, cfg.tolerance_px);
        let better = best
            .as_ref()
            .map(|(b, _)| inl.len() > b.len())
            .unwrap_or(true);
        if better {
            let w = inl.len() as f64 / n as f64;
            if w > 0.98 {
                best = Some((inl, t));
                break;
            }
            if w > 0.0 {
                let denom = (1.0 - w.powi(k as i32)).ln();
                if denom < 0.0 {
                    needed = ((1.0f64 - 0.9999).ln() / denom).ceil().max(1.0) as usize;
                }
            }
            best = Some((inl, t));
        }
    }
    let (inl, _) = best?;
    if inl.len() < cfg.min_inliers {
        return None;
    }
    // Refit on every inlier, then re-select inliers under the refit model
    // once so the reported set is consistent with the reported model.
    let subset: Vec<Pair> = inl.iter().map(|&i| pairs[i]).collect();
    let linear = fit_linear(cfg.kind, &subset, None)?;
    let inliers = inliers_of(&linear, pairs, cfg.tolerance_px);
    if inliers.len() < cfg.min_inliers {
        return None;
    }
    let subset: Vec<Pair> = inliers.iter().map(|&i| pairs[i]).collect();
    let rms_px = rms(&residuals(&linear, &subset));
    let quality = quality(pairs, &inliers, rms_px, cfg);
    Some(RansacResult {
        linear,
        inliers,
        rms_px,
        quality,
        iterations,
    })
}

#[derive(Clone, Debug)]
pub struct RefitResult {
    pub linear: Linear,
    pub inliers: Vec<usize>,
    pub rms_px: f64,
    /// Standard deviation of the per-pair residuals.
    pub sigma_rms_px: f64,
    /// Largest |Δx|, |Δy| over the final inliers.
    pub peak_px: (f64, f64),
    pub rounds: usize,
}

/// σ-weighted least squares on `inliers` with iterative `clip_sigma`
/// clipping (≤ 5 rounds; stops when the inlier set's Jaccard similarity
/// with the previous round exceeds 0.97). `sigmas` are per-pair centroid
/// uncertainties `(σx, σy)`; weight `1 / (σx² + σy² + ε)`, uniform when
/// absent.
pub fn refit_weighted(
    pairs: &[Pair],
    inliers: &[usize],
    sigmas: Option<&[(f64, f64)]>,
    kind: LinearKind,
    clip_sigma: f64,
) -> Option<RefitResult> {
    const EPS: f64 = 1e-4;
    let mut current: Vec<usize> = inliers.to_vec();
    current.sort_unstable();
    current.dedup();
    let mut rounds = 0usize;
    let mut linear: Option<Linear> = None;
    // The inlier set produced by the PREVIOUS round, compared against this
    // round's own output to detect convergence. `None` on the first round:
    // there is no previous round yet, so round 1 can never "converge" — the
    // caller's raw `inliers` may still carry the very outliers clipping is
    // meant to remove, and comparing against that raw input let a single
    // pass that happened to prune them look identical to a converged fit,
    // reporting the model the polluted round 1 subset produced instead of
    // refitting on the cleaned set.
    let mut prev_output: Option<Vec<usize>> = None;
    loop {
        rounds += 1;
        let subset: Vec<Pair> = current.iter().map(|&i| pairs[i]).collect();
        let weights: Option<Vec<f64>> = sigmas.map(|s| {
            current
                .iter()
                .map(|&i| 1.0 / (s[i].0.powi(2) + s[i].1.powi(2) + EPS))
                .collect()
        });
        let t = fit_linear(kind, &subset, weights.as_deref())?;
        let res = residuals(&t, &subset);
        let r = rms(&res);
        let next: Vec<usize> = current
            .iter()
            .zip(res.iter())
            .filter(|(_, &e)| e <= clip_sigma * r.max(1e-9))
            .map(|(&i, _)| i)
            .collect();
        linear = Some(t);
        if next.len() < 3 {
            break;
        }
        let converged = prev_output.as_ref().is_some_and(|prev| {
            let inter = next
                .iter()
                .filter(|i| prev.binary_search(i).is_ok())
                .count();
            let union = prev.len() + next.len() - inter;
            let jaccard = if union == 0 {
                1.0
            } else {
                inter as f64 / union as f64
            };
            jaccard > 0.97
        });
        prev_output = Some(next.clone());
        current = next;
        if converged || rounds >= 5 {
            break;
        }
    }
    let linear = linear?;
    let subset: Vec<Pair> = current.iter().map(|&i| pairs[i]).collect();
    let res = residuals(&linear, &subset);
    let rms_px = rms(&res);
    let mean = res.iter().sum::<f64>() / res.len().max(1) as f64;
    let sigma_rms_px =
        (res.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / res.len().max(1) as f64).sqrt();
    let mut peak = (0.0f64, 0.0f64);
    for ((x, y), (u, v)) in &subset {
        let (px, py) = linear.apply(*x, *y);
        peak.0 = peak.0.max((px - u).abs());
        peak.1 = peak.1.max((py - v).abs());
    }
    Some(RefitResult {
        linear,
        inliers: current,
        rms_px,
        sigma_rms_px,
        peak_px: peak,
        rounds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind, Pair};

    fn rng_points(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut r = SplitMix64(seed);
        (0..n)
            .map(|_| (r.next_f64() * 6000.0, r.next_f64() * 4000.0))
            .collect()
    }

    fn truth() -> Linear {
        Linear {
            kind: LinearKind::Homography,
            m: [
                [0.992, 0.0387, -23.54],
                [-0.0383, 0.9922, 136.37],
                [8.8e-8, -2.5e-8, 1.0],
            ],
        }
    }

    /// 300 true correspondences with 0.1 px noise plus 130 outliers (30 %).
    fn contaminated(seed: u64) -> (Vec<Pair>, usize) {
        let t = truth();
        let mut r = SplitMix64(seed);
        let mut pairs: Vec<Pair> = rng_points(300, seed)
            .into_iter()
            .map(|(x, y)| {
                let (u, v) = t.apply(x, y);
                (
                    (x, y),
                    (
                        u + (r.next_f64() - 0.5) * 0.2,
                        v + (r.next_f64() - 0.5) * 0.2,
                    ),
                )
            })
            .collect();
        let n_true = pairs.len();
        for (x, y) in rng_points(130, seed + 1) {
            pairs.push(((x, y), (r.next_f64() * 6000.0, r.next_f64() * 4000.0)));
        }
        (pairs, n_true)
    }

    #[test]
    fn splitmix_is_deterministic_and_bounded() {
        let mut a = SplitMix64(42);
        let mut b = SplitMix64(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
            let f = a.next_f64();
            assert!((0.0..1.0).contains(&f));
            assert!(a.below(7) < 7);
            b.next_f64();
            b.below(7);
        }
    }

    #[test]
    fn ransac_recovers_the_inlier_set_under_thirty_percent_outliers() {
        let (pairs, n_true) = contaminated(5);
        let cfg = RansacConfig::new(LinearKind::Homography, 6000.0, 4000.0);
        let res = ransac_fit(&pairs, &cfg).expect("a model");
        // Every recovered inlier is a true pair, and nearly all true pairs are recovered.
        assert!(
            res.inliers.iter().all(|&i| i < n_true),
            "an outlier was accepted"
        );
        assert!(
            res.inliers.len() >= (n_true as f64 * 0.95) as usize,
            "only {} inliers",
            res.inliers.len()
        );
        assert!(res.rms_px < 0.15, "rms {}", res.rms_px);
        assert!(res.quality.inlier_ratio > 0.6 && res.quality.inlier_ratio < 0.75);
        assert!(res.quality.overlap > 0.9);
        assert!(res.quality.regularity > 0.9);
        assert!(res.iterations <= cfg.max_iterations);
    }

    #[test]
    fn ransac_is_deterministic_for_a_seed() {
        let (pairs, _) = contaminated(9);
        let cfg = RansacConfig::new(LinearKind::Affine, 6000.0, 4000.0);
        let a = ransac_fit(&pairs, &cfg).unwrap();
        let b = ransac_fit(&pairs, &cfg).unwrap();
        assert_eq!(a.inliers, b.inliers);
        assert_eq!(a.linear, b.linear);
        assert_eq!(a.iterations, b.iterations);
    }

    #[test]
    fn ransac_fails_honestly_on_noise() {
        let mut r = SplitMix64(3);
        let pairs: Vec<Pair> = (0..60)
            .map(|_| {
                (
                    (r.next_f64() * 6000.0, r.next_f64() * 4000.0),
                    (r.next_f64() * 6000.0, r.next_f64() * 4000.0),
                )
            })
            .collect();
        let cfg = RansacConfig::new(LinearKind::Similarity, 6000.0, 4000.0);
        assert!(ransac_fit(&pairs, &cfg).is_none());
    }

    #[test]
    fn similarity_from_two_pairs_and_affine_from_three_are_exact_samples() {
        let t = Linear {
            kind: LinearKind::Affine,
            m: [[1.01, 0.02, 5.0], [-0.03, 0.99, -7.0], [0.0, 0.0, 1.0]],
        };
        let pairs: Vec<Pair> = rng_points(40, 11)
            .into_iter()
            .map(|(x, y)| ((x, y), t.apply(x, y)))
            .collect();
        let cfg = RansacConfig::new(LinearKind::Affine, 6000.0, 4000.0);
        let res = ransac_fit(&pairs, &cfg).unwrap();
        assert_eq!(res.inliers.len(), 40);
        assert!(res.rms_px < 1e-6);
    }

    #[test]
    fn weighted_refit_clips_residual_outliers_and_reports_peaks() {
        let (pairs, n_true) = contaminated(21);
        let cfg = RansacConfig::new(LinearKind::Homography, 6000.0, 4000.0);
        let res = ransac_fit(&pairs, &cfg).unwrap();
        // Hand the refit a slightly polluted inlier list: add 5 outliers.
        let mut inl = res.inliers.clone();
        inl.extend((n_true..n_true + 5).collect::<Vec<_>>());
        let sig: Vec<(f64, f64)> = pairs.iter().map(|_| (0.05, 0.05)).collect();
        let r = refit_weighted(&pairs, &inl, Some(&sig), LinearKind::Homography, 3.0).unwrap();
        assert!(
            r.inliers.iter().all(|&i| i < n_true),
            "clip must drop the 5 outliers"
        );
        assert!(r.rms_px < 0.15);
        assert!(r.sigma_rms_px < r.rms_px);
        assert!(
            r.peak_px.0 < 0.5 && r.peak_px.1 < 0.5,
            "peaks {:?}",
            r.peak_px
        );
        assert!(r.rounds >= 1 && r.rounds <= 5);
    }
}
