//! The local distortion correction loop (M4c, ruling R-M4c-7): a
//! refinement pass over an already-fitted map, for the case the first
//! pairing could not cover the field.
//!
//! Per round: re-pair every subject star THROUGH the current map (linear
//! part and distortion together, not just the linear model) at a widening
//! tolerance; RANSAC a corrector homography on what the map still gets
//! wrong — predicted reference position → actual reference position; stop
//! once that corrector is the identity to within
//! [`LOCAL_DISTORTION_STOP`]; otherwise compose it into the linear part
//! and refit the distortion around the result.
//!
//! Extracted from `align.rs` in fix round 1 (minor 7): the loop is a
//! self-contained refinement with its own state, and `align` was 385
//! lines with it inline.

use tracing::debug;

use super::align::{
    build_map, pair_projected, residual_stats, DistortionFit, MapFit, Residuals, MIN_INLIERS,
};
use super::detect::Star;
use super::RegistrationConfig;
use crate::geometry::{ransac_fit, KdTree2, Linear, LinearKind, Pair, PixelMap, RansacConfig};

/// Rounds of the local distortion loop (ruling R-M4c-7).
pub const LOCAL_DISTORTION_ROUNDS: usize = 3;

/// The loop stops once the corrector homography is this close to the
/// identity in Frobenius norm (ruling R-M4c-7).
///
/// The norm mixes units, and honestly so: the translation entries are
/// pixels, the linear block is dimensionless, and the projective row is
/// per-pixel. So the threshold is NOT "a sub-milli-pixel correction" —
/// a pure scale of 1.0005 has norm ≈ 7·10⁻⁴ and sits UNDER it while
/// displacing a 6248 px frame's edge by 1.56 px. What it does mean is
/// "the corrector is numerically indistinguishable from the identity in
/// every entry", which is the state a converged round reaches: with the
/// linear part already least-squares-fitted over these pairs, the
/// residual a homography could still absorb is at the solver's floor,
/// orders of magnitude below 10⁻³, not just inside it. A corrector
/// carrying a real trend — the partial-pairing case the loop exists for
/// — lands far above the threshold in the translation entries alone.
pub const LOCAL_DISTORTION_STOP: f64 = 1e-3;

/// What the loop did, for the caller to fold back into its `Alignment`.
pub(super) struct LoopOutcome {
    /// Rounds whose corrector was actually FITTED — the pairing produced
    /// at least [`MIN_INLIERS`] correspondences and the corrector's
    /// RANSAC succeeded (ruling R-T4-2). A round that converged counts;
    /// a round the pairing or the RANSAC ended before a corrector existed
    /// does not.
    pub rounds: usize,
    /// The best map, its linear part and its distortion — the incumbent
    /// when no round was kept.
    pub fit: MapFit,
    /// Inlier pairs the reported residuals are measured over: the kept
    /// round's own set, or the incumbent's.
    pub inlier_pairs: Vec<Pair>,
    /// Correspondences in the pairing those inliers came out of — a kept
    /// round re-paired, so this moves with it and the reported
    /// `inlier_ratio` stays coherent.
    pub pairs_len: usize,
    pub warnings: Vec<String>,
}

/// `‖H − I‖_F` — see [`LOCAL_DISTORTION_STOP`] for what the number means.
pub fn corrector_norm(h: &Linear) -> f64 {
    let mut sum = 0.0;
    for i in 0..3 {
        for j in 0..3 {
            let identity = if i == j { 1.0 } else { 0.0 };
            let d = h.m[i][j] - identity;
            sum += d * d;
        }
    }
    sum.sqrt()
}

/// `a · b` for the 3×3 homogeneous matrices. The kind LABEL is `b`'s: a
/// corrector composed into a similarity may well make it projective, and
/// `Distortion::fit_joint` already sets that precedent.
pub fn compose(a: &Linear, b: &Linear) -> Linear {
    let mut m = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a.m[i][0] * b.m[0][j] + a.m[i][1] * b.m[1][j] + a.m[i][2] * b.m[2][j];
        }
    }
    Linear { kind: b.kind, m }
}

/// Runs the loop. `plan` is the distortion the frame asked for — a round
/// that cannot deliver exactly that is refused rather than shipped as a
/// silent downgrade.
///
/// **The accept guard** (ruling R-T4-5): a round is kept only when its
/// candidate map is no worse than the incumbent ON ONE COMMON PAIR SET —
/// the incumbent's own inliers. Comparing each model on its own inlier
/// set would let a round whose corrector RANSAC happened to keep an
/// easier subset look better while being worse on the population the
/// incumbent was judged on, which is exactly the "never worse" claim the
/// loop must not fake.
#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    plan: DistortionFit,
    incumbent: MapFit,
    inlier_pairs: Vec<Pair>,
    pairs_len: usize,
    subject: &[Star],
    reference: &[Star],
    tree: &KdTree2,
    reference_geometry: (usize, usize),
    cfg: &RegistrationConfig,
) -> LoopOutcome {
    let mut out = LoopOutcome {
        rounds: 0,
        fit: incumbent,
        inlier_pairs,
        pairs_len,
        warnings: Vec::new(),
    };
    if !cfg.local_distortion || !out.fit.fit.is_some() {
        return out;
    }

    for round in 0..LOCAL_DISTORTION_ROUNDS {
        let tolerance = cfg.ransac_tolerance_px * (1.0 + round as f64);
        let again = pair_projected(
            &|x, y| out.fit.map.forward_exact(x, y),
            subject,
            reference,
            tree,
            tolerance,
        );
        if again.pairs.len() < MIN_INLIERS {
            break;
        }
        let residual_pairs: Vec<Pair> = again
            .pairs
            .iter()
            .map(|&((sx, sy), r)| (out.fit.map.forward_exact(sx, sy), r))
            .collect();
        let mut rc = RansacConfig::new(
            LinearKind::Homography,
            reference_geometry.0 as f64,
            reference_geometry.1 as f64,
        );
        rc.tolerance_px = cfg.ransac_tolerance_px;
        rc.max_iterations = cfg.ransac_max_iterations;
        rc.min_inliers = MIN_INLIERS;
        let Some(corrector) = ransac_fit(&residual_pairs, &rc) else {
            break;
        };
        let norm = corrector_norm(&corrector.linear);
        out.rounds = round + 1;
        debug!(
            round = out.rounds,
            inliers = corrector.inliers.len(),
            rms_px = corrector.rms_px,
            corrector_norm = norm,
            "local distortion round"
        );
        if norm < LOCAL_DISTORTION_STOP {
            break;
        }
        let composed = compose(&corrector.linear, &out.fit.linear);
        if composed.inverse().is_none() {
            out.warnings.push(
                "local distortion round produced a singular linear model; previous map kept"
                    .to_string(),
            );
            break;
        }
        let kept: Vec<Pair> = corrector.inliers.iter().map(|&i| again.pairs[i]).collect();
        let kept_sigmas: Option<Vec<(f64, f64)>> = again
            .all_sigmas
            .then(|| corrector.inliers.iter().map(|&i| again.sigmas[i]).collect());
        let mut candidate = match build_map(
            plan,
            composed,
            &kept,
            kept_sigmas.as_deref(),
            reference_geometry,
            cfg.tps_smoothing,
        ) {
            Ok(c) => c,
            Err(e) => {
                out.warnings.push(format!(
                    "local distortion round {} could not rebuild the map ({e}); previous map kept",
                    out.rounds
                ));
                break;
            }
        };
        if candidate.fit != plan {
            // The round's own inlier set was too small for the requested
            // distortion, or its fit did not converge — `build_map`
            // degraded to the linear model and said why. Shipping that
            // instead of the map we already have would be a silent
            // downgrade, so the round is refused and its reason recorded.
            out.warnings.extend(candidate.notes);
            break;
        }
        // One common set, both models (ruling R-T4-5).
        let common = &out.inlier_pairs;
        let incumbent_rms = common_rms(&out.fit.map, common);
        let candidate_rms = common_rms(&candidate.map, common);
        if candidate_rms > incumbent_rms {
            break;
        }
        // A KEPT round's notes are not always empty (minor 8): the spline
        // arm records a failed hold-out fit while still shipping its
        // model, so the map being adopted can carry a caveat the frame
        // must hear about.
        out.warnings.extend(candidate.notes.drain(..));
        out.fit = candidate;
        out.pairs_len = again.pairs.len();
        out.inlier_pairs = kept;
    }
    out
}

/// The RMS of `pairs` through `map` — the single number the accept guard
/// compares, always over the same pair set for both models.
fn common_rms(map: &PixelMap, pairs: &[Pair]) -> f64 {
    let (rms, _, _): Residuals = residual_stats(map, pairs);
    rms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::LinearKind;

    fn similarity(scale: f64, rot_deg: f64, tx: f64, ty: f64) -> Linear {
        let (s, c) = rot_deg.to_radians().sin_cos();
        Linear::from_flat(
            LinearKind::Similarity,
            [
                scale * c,
                -scale * s,
                tx,
                scale * s,
                scale * c,
                ty,
                0.0,
                0.0,
                1.0,
            ],
        )
    }

    /// Minor 6: the threshold's units are mixed, and the doc must not
    /// pretend otherwise — a pure scale that shifts a big frame's edge by
    /// more than a pixel still sits under it.
    #[test]
    fn the_corrector_norm_is_zero_only_at_the_identity() {
        assert_eq!(corrector_norm(&Linear::identity()), 0.0);

        let shifted = Linear::from_flat(
            LinearKind::Affine,
            [1.0, 0.0, 0.0005, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        );
        assert!(corrector_norm(&shifted) < LOCAL_DISTORTION_STOP);
        let shifted = Linear::from_flat(
            LinearKind::Affine,
            [1.0, 0.0, 0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        );
        assert!(corrector_norm(&shifted) > LOCAL_DISTORTION_STOP);

        // The honest caveat, pinned: a 1.0005 pure scale is UNDER the
        // threshold and still moves a 6248 px frame's edge 1.56 px.
        let scaled = similarity(1.0005, 0.0, 0.0, 0.0);
        let norm = corrector_norm(&scaled);
        assert!(
            norm < LOCAL_DISTORTION_STOP,
            "a 1.0005 scale should sit under the threshold, norm {norm}"
        );
        let edge = 0.0005 * 6248.0 / 2.0;
        assert!(
            edge > 1.5,
            "…while displacing the frame edge by {edge} px — the doc says so"
        );
    }

    /// Composition is `a · b`, so applying the composite equals applying
    /// `b` and then `a`.
    #[test]
    fn composition_applies_b_then_a() {
        let a = similarity(1.1, 3.0, 7.0, -2.0);
        let b = similarity(0.9, -5.0, -3.0, 11.0);
        let c = compose(&a, &b);
        let (bx, by) = b.apply(123.0, 456.0);
        let (ex, ey) = a.apply(bx, by);
        let (cx, cy) = c.apply(123.0, 456.0);
        assert!((cx - ex).abs() < 1e-9 && (cy - ey).abs() < 1e-9);
        assert_eq!(compose(&Linear::identity(), &b).m, b.m);
        assert_eq!(
            compose(&a, &b).kind,
            b.kind,
            "the kind label is the model's, not the corrector's"
        );
    }
}
