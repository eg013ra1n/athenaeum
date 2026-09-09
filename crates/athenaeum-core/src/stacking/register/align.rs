//! Subject → reference alignment (spec §3.2–3.3, §3.6): a quad-matched
//! seed, KD-tree correspondences, RANSAC on the configured linear model,
//! a σ-weighted refit, optional polynomial distortion and the QA gates.
//! Pure geometry over star lists; no I/O.

use std::fmt;

use serde::{Deserialize, Serialize};
use solvemyastro::quad::{build_quads, fit_affine, group_size_for, match_quads};

use super::detect::Star;
use super::{DistortionChoice, ModelChoice, RegistrationConfig};
use crate::geometry::{
    ransac_fit, refit_weighted, Distortion, KdTree2, Linear, LinearKind, Pair, PixelMap,
    RansacConfig, RansacResult, RefitResult,
};

/// Quad-ratio tolerance of the seed matcher (the plate solver's default).
pub const QUAD_TOLERANCE: f64 = 0.007;
/// RANSAC and the QA gates need at least this many inliers.
pub const MIN_INLIERS: usize = 8;
/// A linear scale outside this range fails the frame (spec §3.6).
pub const SCALE_RANGE: (f64, f64) = (0.8, 1.25);
/// `model: auto` — homography from this many correspondences …
pub const AUTO_HOMOGRAPHY_MIN: usize = 30;
/// … affine from this many, similarity below.
pub const AUTO_AFFINE_MIN: usize = 12;
/// `distortion: auto` needs this many refit inliers (spec §3.3).
pub const AUTO_DISTORTION_MIN_INLIERS: usize = 200;
/// `distortion: auto` also requires the inliers' convex hull to cover this
/// fraction of the matched pairs' hull (the RANSAC overlap index — a
/// consistency check on the matching: 0.35 on a subject whose few true
/// pairs sat in a corner of many false ones) …
pub const AUTO_DISTORTION_MIN_OVERLAP: f64 = 0.6;
/// … and the inliers to occupy this fraction of a 4×4 grid over the frame
/// (the RANSAC regularity index): a polynomial fitted on one corner says
/// nothing about the rest of the frame — on the real data the low-coverage
/// OSC subject scores 0.5, the well-covered ones 0.69–1.0.
pub const AUTO_DISTORTION_MIN_REGULARITY: f64 = 0.6;
/// Refit clipping (spec §3.2 step 4).
pub const CLIP_SIGMA: f64 = 3.0;
/// Joint fits: the second round's affine correction is ≈ identity and
/// only re-centres the polynomial around the re-balanced linear model.
pub const DISTORTION_ROUNDS: usize = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AlignError {
    TooFewStars { subject: usize, reference: usize },
    NoSeed { matches: usize },
    TooFewMatches { matches: usize },
    TooFewInliers { inliers: usize },
    Degenerate,
    ScaleOutOfRange { scale: f64 },
    RmsTooHigh { rms_px: f64, max_rms_px: f64 },
}

impl fmt::Display for AlignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlignError::TooFewStars { subject, reference } => write!(
                f,
                "too few stars (subject {subject}, reference {reference})"
            ),
            AlignError::NoSeed { matches } => {
                write!(f, "quad seed failed ({matches} quad matches)")
            }
            AlignError::TooFewMatches { matches } => {
                write!(f, "only {matches} correspondences within tolerance")
            }
            AlignError::TooFewInliers { inliers } => write!(f, "only {inliers} inliers"),
            AlignError::Degenerate => write!(f, "degenerate transform"),
            AlignError::ScaleOutOfRange { scale } => write!(
                f,
                "scale {scale:.4} outside [{}, {}]",
                SCALE_RANGE.0, SCALE_RANGE.1
            ),
            AlignError::RmsTooHigh { rms_px, max_rms_px } => {
                write!(f, "RMS {rms_px:.2} px above {max_rms_px:.2}")
            }
        }
    }
}

impl std::error::Error for AlignError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// Subject → reference (with the stored inverse).
    pub map: PixelMap,
    pub model: LinearKind,
    pub distortion_order: Option<u8>,
    pub seed_matches: usize,
    /// Correspondences in the final pairing (the seed's, or the re-paired set).
    pub pairs: usize,
    /// Growth of the correspondence count from the re-pairing pass through
    /// the refit model (0 when the seed already paired the field, or when
    /// the pass was not taken).
    pub repaired: usize,
    /// Refit inliers.
    pub inliers: usize,
    pub inlier_ratio: f64,
    /// Through the final map, over the refit inliers.
    pub rms_px: f64,
    pub sigma_rms_px: f64,
    pub peak_px: (f64, f64),
    pub scale: f64,
    pub rotation_deg: f64,
    pub translation: (f64, f64),
    pub flipped: bool,
    pub quality_score: f64,
    pub overlap: f64,
    pub regularity: f64,
    pub ransac_iterations: usize,
    pub refit_rounds: usize,
    pub warnings: Vec<String>,
}

pub fn resolve_model(choice: ModelChoice, n: usize) -> LinearKind {
    match choice {
        ModelChoice::Similarity => LinearKind::Similarity,
        ModelChoice::Affine => LinearKind::Affine,
        ModelChoice::Homography => LinearKind::Homography,
        ModelChoice::Auto => {
            if n >= AUTO_HOMOGRAPHY_MIN {
                LinearKind::Homography
            } else if n >= AUTO_AFFINE_MIN {
                LinearKind::Affine
            } else {
                LinearKind::Similarity
            }
        }
    }
}

/// The order `distortion: auto` resolves to: 3 for a cross-geometry subject
/// with enough inliers that are consistent (`overlap`) and spread over the
/// frame (`regularity`); `None` keeps the linear model.
pub fn auto_distortion_order(
    cross_geometry: bool,
    inliers: usize,
    overlap: f64,
    regularity: f64,
) -> Option<u8> {
    (cross_geometry
        && inliers >= AUTO_DISTORTION_MIN_INLIERS
        && overlap >= AUTO_DISTORTION_MIN_OVERLAP
        && regularity >= AUTO_DISTORTION_MIN_REGULARITY)
        .then_some(3)
}

/// `registration_results.model`: the linear kind's serde name, plus
/// `+polynomial<o>` when a distortion was fitted.
pub fn model_name(kind: LinearKind, distortion_order: Option<u8>) -> String {
    let base = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    match distortion_order {
        Some(o) => format!("{base}+polynomial{o}"),
        None => base,
    }
}

/// A polynomial of `order` needs `(order+1)(order+2)` pairs (twice its
/// per-axis term count).
fn min_pairs_for(order: u8) -> usize {
    let o = order as usize;
    (o + 1) * (o + 2)
}

/// Seed subject → reference from quad matching (subject plays "image",
/// reference plays "catalog"; the fitted affine maps image → catalog).
fn seed_affine(sub: &[(f64, f64)], refp: &[(f64, f64)]) -> Result<(Linear, usize), AlignError> {
    let sub_q = build_quads(sub, sub.len(), group_size_for(sub.len()));
    let ref_q = build_quads(refp, refp.len(), group_size_for(refp.len()));
    let matches = match_quads(&sub_q, &ref_q, QUAD_TOLERANCE);
    let a = fit_affine(&matches, &sub_q, &ref_q).ok_or(AlignError::NoSeed {
        matches: matches.len(),
    })?;
    Ok((
        Linear::from_flat(
            LinearKind::Affine,
            [a.a1, a.b1, a.c1, a.a2, a.b2, a.c2, 0.0, 0.0, 1.0],
        ),
        matches.len(),
    ))
}

fn residual_stats(map: &PixelMap, pairs: &[Pair]) -> (f64, f64, (f64, f64)) {
    let mut sum2 = 0.0;
    let mut sum = 0.0;
    let (mut px, mut py) = (0.0f64, 0.0f64);
    for &((sx, sy), (rx, ry)) in pairs {
        let (fx, fy) = map.forward(sx, sy);
        let (dx, dy) = (fx - rx, fy - ry);
        let d = (dx * dx + dy * dy).sqrt();
        sum2 += d * d;
        sum += d;
        px = px.max(dx.abs());
        py = py.max(dy.abs());
    }
    let n = pairs.len().max(1) as f64;
    let rms = (sum2 / n).sqrt();
    let mean = sum / n;
    let var = (sum2 / n - mean * mean).max(0.0);
    (rms, var.sqrt(), (px, py))
}

/// One set of correspondences with the per-pair centroid σ.
struct Pairing {
    pairs: Vec<Pair>,
    sigmas: Vec<(f64, f64)>,
    all_sigmas: bool,
}

/// Correspondences through `model`: every subject star's nearest reference
/// star within `radius`, with the pair's combined centroid σ
/// (`√(σ_s² + σ_r²)` per axis) when both stars carry one.
fn pair_through(
    model: &Linear,
    subject: &[Star],
    reference: &[Star],
    tree: &KdTree2,
    radius: f64,
) -> Pairing {
    let mut p = Pairing {
        pairs: Vec::new(),
        sigmas: Vec::new(),
        all_sigmas: true,
    };
    for s in subject {
        let (px, py) = model.apply(s.x, s.y);
        if let Some((j, _)) = tree.nearest_within(px, py, radius) {
            let r = &reference[j];
            p.pairs.push(((s.x, s.y), (r.x, r.y)));
            match (s.sigma, r.sigma) {
                (Some(a), Some(b)) => p.sigmas.push((
                    (a.0 * a.0 + b.0 * b.0).sqrt(),
                    (a.1 * a.1 + b.1 * b.1).sqrt(),
                )),
                _ => {
                    p.all_sigmas = false;
                    p.sigmas.push((0.0, 0.0));
                }
            }
        }
    }
    p
}

/// Steps 3–4: RANSAC on the model resolved from the pair count, then the
/// σ-weighted refit with `auto` re-resolved from the inlier count (within
/// one call it can only step down; a later call on a larger pairing may
/// resolve higher — `kind`, `ransac` and `refit` are replaced together, so
/// the shipped model is the one the refit was fitted with).
fn ransac_and_refit(
    pairing: &Pairing,
    cfg: &RegistrationConfig,
    reference_geometry: (usize, usize),
) -> Result<(RansacResult, RefitResult, LinearKind), AlignError> {
    let pairs = &pairing.pairs;
    let ransac_kind = resolve_model(cfg.model, pairs.len());
    let mut rc = RansacConfig::new(
        ransac_kind,
        reference_geometry.0 as f64,
        reference_geometry.1 as f64,
    );
    rc.tolerance_px = cfg.ransac_tolerance_px;
    rc.max_iterations = cfg.ransac_max_iterations;
    rc.min_inliers = MIN_INLIERS;
    let ransac = ransac_fit(pairs, &rc).ok_or(AlignError::TooFewInliers { inliers: 0 })?;
    if ransac.inliers.len() < MIN_INLIERS {
        return Err(AlignError::TooFewInliers {
            inliers: ransac.inliers.len(),
        });
    }
    let kind = resolve_model(cfg.model, ransac.inliers.len());
    let sig = pairing.all_sigmas.then_some(pairing.sigmas.as_slice());
    let refit = refit_weighted(pairs, &ransac.inliers, sig, kind, CLIP_SIGMA).ok_or(
        AlignError::TooFewInliers {
            inliers: ransac.inliers.len(),
        },
    )?;
    if refit.inliers.len() < MIN_INLIERS {
        return Err(AlignError::TooFewInliers {
            inliers: refit.inliers.len(),
        });
    }
    Ok((ransac, refit, kind))
}

/// Fit the distortion jointly with the linear part's affine correction
/// (`Distortion::fit_joint`), twice.
fn fit_distortion(
    order: u8,
    linear: Linear,
    pairs: &[Pair],
    weights: Option<&[f64]>,
    center: (f64, f64),
    scale: f64,
) -> Option<(Linear, Distortion)> {
    let (mut lin, mut dist) = Distortion::fit_joint(order, &linear, pairs, weights, center, scale)
        .filter(|(l, d)| l.inverse().is_some() && d.is_well_formed())?;
    for _ in 1..DISTORTION_ROUNDS {
        match Distortion::fit_joint(order, &lin, pairs, weights, center, scale) {
            Some((l, d)) if l.inverse().is_some() && d.is_well_formed() => {
                lin = l;
                dist = d;
            }
            _ => break,
        }
    }
    Some((lin, dist))
}

pub fn align(
    subject: &[Star],
    reference: &[Star],
    reference_geometry: (usize, usize),
    subject_geometry: (usize, usize),
    cfg: &RegistrationConfig,
) -> Result<Alignment, AlignError> {
    if subject.len() < MIN_INLIERS || reference.len() < MIN_INLIERS {
        return Err(AlignError::TooFewStars {
            subject: subject.len(),
            reference: reference.len(),
        });
    }
    let sub_pts: Vec<(f64, f64)> = subject.iter().map(|s| (s.x, s.y)).collect();
    let ref_pts: Vec<(f64, f64)> = reference.iter().map(|s| (s.x, s.y)).collect();

    // 1. Seed.
    let (seed, seed_matches) = seed_affine(&sub_pts, &ref_pts)?;

    // 2. Correspondences through the seed, nearest reference star within 2·tol.
    let tree = KdTree2::build(&ref_pts);
    let radius = 2.0 * cfg.ransac_tolerance_px;
    let mut pairing = pair_through(&seed, subject, reference, &tree, radius);
    if pairing.pairs.len() < MIN_INLIERS {
        return Err(AlignError::TooFewMatches {
            matches: pairing.pairs.len(),
        });
    }

    // 3–4. RANSAC and the σ-weighted refit.
    let (mut ransac, mut refit, mut kind) = ransac_and_refit(&pairing, cfg, reference_geometry)?;
    let mut warnings = Vec::new();

    // 4b. Re-pair through the refit model: the seed is an affine fitted on
    // the matched quads and its accuracy falls off with distance from them
    // (a 1e-3 relative error is 6 px at the far edge, beyond the pairing
    // radius), so a rotated or rescaled subject pairs only near them. One
    // pass through the refit model recovers the rest of the field.
    let mut repaired = 0usize;
    let again = pair_through(&refit.linear, subject, reference, &tree, radius);
    if again.pairs.len() > pairing.pairs.len() {
        match ransac_and_refit(&again, cfg, reference_geometry) {
            Ok((r2, f2, k2)) => {
                repaired = again.pairs.len() - pairing.pairs.len();
                pairing = again;
                ransac = r2;
                refit = f2;
                kind = k2;
            }
            Err(e) => warnings.push(format!(
                "re-pairing through the refit model failed ({e}); seed pairs kept"
            )),
        }
    }
    let pairs = &pairing.pairs;
    let sigmas = &pairing.sigmas;
    let all_sigmas = pairing.all_sigmas;
    let mut linear = refit.linear;
    let refit_scale = refit.linear.scale();
    if !(refit_scale >= SCALE_RANGE.0 && refit_scale <= SCALE_RANGE.1) {
        return Err(AlignError::ScaleOutOfRange { scale: refit_scale });
    }

    // 5. Optional distortion on the refit inliers.
    let inlier_pairs: Vec<Pair> = refit.inliers.iter().map(|&i| pairs[i]).collect();
    let cross_geometry = subject_geometry != reference_geometry;
    let wanted = match cfg.distortion {
        DistortionChoice::Auto => {
            let (overlap, regularity) = (ransac.quality.overlap, ransac.quality.regularity);
            let order =
                auto_distortion_order(cross_geometry, refit.inliers.len(), overlap, regularity);
            if order.is_none()
                && cross_geometry
                && refit.inliers.len() >= AUTO_DISTORTION_MIN_INLIERS
            {
                warnings.push(format!(
                    "auto distortion skipped: overlap {overlap:.3} (min {AUTO_DISTORTION_MIN_OVERLAP}), regularity {regularity:.3} (min {AUTO_DISTORTION_MIN_REGULARITY}); linear model kept"
                ));
            }
            order
        }
        other => other.order(),
    };
    let (map, distortion_order) = match wanted {
        None => (
            PixelMap::linear(linear).ok_or(AlignError::Degenerate)?,
            None,
        ),
        Some(o) if refit.inliers.len() < min_pairs_for(o) => {
            warnings.push(format!(
                "polynomial{o} distortion needs {} inliers, have {}; linear model kept",
                min_pairs_for(o),
                refit.inliers.len()
            ));
            (
                PixelMap::linear(linear).ok_or(AlignError::Degenerate)?,
                None,
            )
        }
        Some(o) => {
            let center = (
                reference_geometry.0 as f64 / 2.0,
                reference_geometry.1 as f64 / 2.0,
            );
            let norm = reference_geometry.0.max(reference_geometry.1) as f64 / 2.0;
            let weights: Option<Vec<f64>> = all_sigmas.then(|| {
                refit
                    .inliers
                    .iter()
                    .map(|&i| {
                        let (sx, sy) = sigmas[i];
                        1.0 / (sx * sx + sy * sy + 1e-4)
                    })
                    .collect()
            });
            match fit_distortion(o, linear, &inlier_pairs, weights.as_deref(), center, norm) {
                Some((lin, d)) => {
                    linear = lin;
                    match PixelMap::with_distortion(lin, d) {
                        Some(m) => (m, Some(o)),
                        None => return Err(AlignError::Degenerate),
                    }
                }
                None => {
                    warnings.push(format!(
                        "polynomial{o} distortion did not fit; linear model kept"
                    ));
                    (
                        PixelMap::linear(linear).ok_or(AlignError::Degenerate)?,
                        None,
                    )
                }
            }
        }
    };
    let scale = linear.scale();

    // 6. QA through the final map.
    let (rms_px, sigma_rms_px, peak_px) = residual_stats(&map, &inlier_pairs);
    if rms_px > cfg.max_rms_px {
        if cfg.fail_on_max_rms {
            return Err(AlignError::RmsTooHigh {
                rms_px,
                max_rms_px: cfg.max_rms_px,
            });
        }
        warnings.push(format!("RMS {rms_px:.2} px above {:.2}", cfg.max_rms_px));
    }
    Ok(Alignment {
        map,
        model: kind,
        distortion_order,
        seed_matches,
        pairs: pairs.len(),
        repaired,
        inliers: refit.inliers.len(),
        inlier_ratio: refit.inliers.len() as f64 / pairs.len() as f64,
        rms_px,
        sigma_rms_px,
        peak_px,
        scale,
        rotation_deg: linear.rotation_deg(),
        translation: linear.translation(),
        flipped: linear.is_flipped(),
        quality_score: ransac.quality.score,
        overlap: ransac.quality.overlap,
        regularity: ransac.quality.regularity,
        ransac_iterations: ransac.iterations,
        refit_rounds: refit.rounds,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;

    fn field(seed: u64, n: usize, w: f64, h: f64) -> Vec<Star> {
        let mut rng = SplitMix64(seed);
        (0..n)
            .map(|_| Star {
                x: 20.0 + rng.next_f64() * (w - 40.0),
                y: 20.0 + rng.next_f64() * (h - 40.0),
                flux: 100.0 + rng.next_f64() * 900.0,
                sigma: Some((0.05, 0.05)),
            })
            .collect()
    }

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

    /// Reference stars = `truth.forward(subject)` + jitter, dropped when they
    /// leave the reference frame; `outliers` extra unmatched stars on each
    /// side; both lists shuffled.
    fn scene(
        subject: &[Star],
        truth: &Linear,
        jitter: f64,
        outliers: usize,
        w: f64,
        h: f64,
        seed: u64,
    ) -> (Vec<Star>, Vec<Star>) {
        let mut rng = SplitMix64(seed);
        let mut reference: Vec<Star> = subject
            .iter()
            .filter_map(|s| {
                let (x, y) = truth.apply(s.x, s.y);
                let (jx, jy) = (
                    (rng.next_f64() - 0.5) * 2.0 * jitter,
                    (rng.next_f64() - 0.5) * 2.0 * jitter,
                );
                (x >= 0.0 && y >= 0.0 && x < w && y < h).then_some(Star {
                    x: x + jx,
                    y: y + jy,
                    flux: s.flux,
                    sigma: s.sigma,
                })
            })
            .collect();
        let mut subject = subject.to_vec();
        reference.extend(field(seed + 1, outliers, w, h));
        subject.extend(field(seed + 2, outliers, w, h));
        for v in [&mut reference, &mut subject] {
            for i in (1..v.len()).rev() {
                let j = rng.below(i + 1);
                v.swap(i, j);
            }
        }
        (subject, reference)
    }

    const W: f64 = 1000.0;
    const H: f64 = 800.0;

    #[test]
    fn recovers_a_similarity_with_outliers_and_jitter() {
        let subject = field(1, 300, W, H);
        let truth = similarity(1.002, 3.0, 12.3, -7.7);
        let (sub, refs) = scene(&subject, &truth, 0.05, 90, W, H, 7);
        let a = align(
            &sub,
            &refs,
            (W as usize, H as usize),
            (W as usize, H as usize),
            &RegistrationConfig::default(),
        )
        .unwrap();
        assert_eq!(
            a.model,
            LinearKind::Homography,
            "auto picks homography above 30 pairs"
        );
        assert!(a.inliers >= 150, "inliers {}", a.inliers);
        assert!(a.rms_px < 0.1, "rms {}", a.rms_px);
        assert!((a.scale - 1.002).abs() < 1e-4, "scale {}", a.scale);
        assert!(
            (a.rotation_deg - 3.0).abs() < 1e-3,
            "rotation {}",
            a.rotation_deg
        );
        assert!(
            (a.translation.0 - 12.3).abs() < 0.05 && (a.translation.1 + 7.7).abs() < 0.05,
            "translation {:?}",
            a.translation
        );
        assert!(!a.flipped && a.distortion_order.is_none());
        assert!(a.inlier_ratio > 0.5 && a.quality_score > 0.0);
        let (fx, fy) = a.map.forward(500.0, 400.0);
        let (tx, ty) = truth.apply(500.0, 400.0);
        assert!((fx - tx).abs() < 0.05 && (fy - ty).abs() < 0.05);
        assert_eq!(model_name(a.model, a.distortion_order), "homography");
    }

    #[test]
    fn a_mirrored_subject_is_flipped_not_failed() {
        let subject = field(2, 250, W, H);
        let truth = Linear::from_flat(
            LinearKind::Affine,
            [-1.0, 0.0, W - 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        );
        let (sub, refs) = scene(&subject, &truth, 0.05, 60, W, H, 8);
        let a = align(
            &sub,
            &refs,
            (W as usize, H as usize),
            (W as usize, H as usize),
            &RegistrationConfig::default(),
        )
        .unwrap();
        assert!(a.flipped);
        assert!(a.rms_px < 0.1 && a.inliers >= 120);
    }

    #[test]
    fn auto_model_steps_down_with_few_stars() {
        let cfg = RegistrationConfig::default();
        let truth = similarity(1.0, 0.5, 3.0, -2.0);
        for (n, kind) in [
            (10usize, LinearKind::Similarity),
            (20, LinearKind::Affine),
            (60, LinearKind::Homography),
        ] {
            let subject = field(3, n, W, H);
            let (sub, refs) = scene(&subject, &truth, 0.02, 0, W, H, 9);
            let a = align(
                &sub,
                &refs,
                (W as usize, H as usize),
                (W as usize, H as usize),
                &cfg,
            )
            .unwrap();
            assert_eq!(a.model, kind, "n = {n}");
            assert!(a.rms_px < 0.1, "n = {n} rms {}", a.rms_px);
        }
        let fixed = RegistrationConfig {
            model: ModelChoice::Similarity,
            ..Default::default()
        };
        let subject = field(3, 60, W, H);
        let (sub, refs) = scene(&subject, &truth, 0.02, 0, W, H, 9);
        assert_eq!(
            align(
                &sub,
                &refs,
                (W as usize, H as usize),
                (W as usize, H as usize),
                &fixed
            )
            .unwrap()
            .model,
            LinearKind::Similarity
        );
    }

    #[test]
    fn failure_modes_are_named() {
        let cfg = RegistrationConfig::default();
        let geo = (W as usize, H as usize);
        let few = field(4, 5, W, H);
        assert!(matches!(
            align(&few, &few, geo, geo, &cfg),
            Err(AlignError::TooFewStars { .. })
        ));
        let subject = field(5, 200, W, H);
        let big = similarity(1.5, 0.0, 0.0, 0.0);
        let (sub, refs) = scene(&subject, &big, 0.02, 0, W, H, 10);
        assert!(matches!(
            align(&sub, &refs, geo, geo, &cfg),
            Err(AlignError::ScaleOutOfRange { .. })
        ));
        let noisy = similarity(1.0, 1.0, 5.0, 5.0);
        let (sub, refs) = scene(&subject, &noisy, 1.5, 0, W, H, 11);
        let strict = RegistrationConfig {
            max_rms_px: 0.5,
            fail_on_max_rms: true,
            ransac_tolerance_px: 4.0,
            ..Default::default()
        };
        assert!(matches!(
            align(&sub, &refs, geo, geo, &strict),
            Err(AlignError::RmsTooHigh { .. })
        ));
        let lenient = RegistrationConfig {
            max_rms_px: 0.5,
            ransac_tolerance_px: 4.0,
            ..Default::default()
        };
        let a = align(&sub, &refs, geo, geo, &lenient).unwrap();
        assert!(
            a.warnings.iter().any(|w| w.contains("RMS")),
            "{:?}",
            a.warnings
        );
        let unrelated = field(6, 200, W, H);
        assert!(align(&subject, &unrelated, geo, geo, &cfg).is_err());
        assert_eq!(
            format!("{}", AlignError::TooFewInliers { inliers: 5 }),
            "only 5 inliers"
        );
    }

    #[test]
    fn polynomial_distortion_absorbs_a_radial_term() {
        let subject = field(12, 400, W, H);
        let truth = similarity(1.0, 1.0, 4.0, -3.0);
        let (cx, cy) = (W / 2.0, H / 2.0);
        // reference = truth(subject) then a barrel term r' = r(1 + k r²).
        let k = 8e-9;
        let reference: Vec<Star> = subject
            .iter()
            .filter_map(|s| {
                let (x, y) = truth.apply(s.x, s.y);
                let (dx, dy) = (x - cx, y - cy);
                let f = 1.0 + k * (dx * dx + dy * dy);
                let (x, y) = (cx + dx * f, cy + dy * f);
                (x >= 0.0 && y >= 0.0 && x < W && y < H).then_some(Star {
                    x,
                    y,
                    flux: s.flux,
                    sigma: s.sigma,
                })
            })
            .collect();
        let geo = (W as usize, H as usize);
        let linear_only = align(
            &subject,
            &reference,
            geo,
            geo,
            &RegistrationConfig::default(),
        )
        .unwrap();
        assert!(
            linear_only.rms_px > 0.12,
            "linear rms {}",
            linear_only.rms_px
        );
        let cfg = RegistrationConfig {
            distortion: DistortionChoice::Polynomial3,
            ..Default::default()
        };
        let a = align(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(a.distortion_order, Some(3));
        assert!(a.rms_px < 0.05, "polynomial rms {}", a.rms_px);
        assert_eq!(
            model_name(a.model, a.distortion_order),
            "homography+polynomial3"
        );
        let auto = RegistrationConfig {
            distortion: DistortionChoice::Auto,
            ..Default::default()
        };
        assert_eq!(
            align(&subject, &reference, geo, geo, &auto)
                .unwrap()
                .distortion_order,
            None,
            "same geometry: auto stays linear"
        );
        assert_eq!(
            align(&subject, &reference, geo, (1010, 800), &auto)
                .unwrap()
                .distortion_order,
            Some(3),
            "cross geometry with ≥ 200 inliers: auto fits order 3"
        );
    }

    #[test]
    fn auto_distortion_needs_cross_geometry_enough_inliers_overlap_and_coverage() {
        assert_eq!(auto_distortion_order(true, 200, 0.6, 0.6), Some(3));
        assert_eq!(
            auto_distortion_order(false, 500, 1.0, 1.0),
            None,
            "same geometry"
        );
        assert_eq!(
            auto_distortion_order(true, 199, 1.0, 1.0),
            None,
            "too few inliers"
        );
        assert_eq!(
            auto_distortion_order(true, 500, 0.59, 1.0),
            None,
            "inconsistent matching"
        );
        assert_eq!(
            auto_distortion_order(true, 500, 1.0, 0.5),
            None,
            "inliers in one corner"
        );
        assert_eq!(
            auto_distortion_order(true, 500, f64::NAN, 1.0),
            None,
            "no quality"
        );
    }

    #[test]
    fn re_pairing_through_the_refit_model_recovers_the_field() {
        // 6000×4000 field, 8° rotation, 0.8 % scale. A model with a 2e-3
        // scale error pairs only the stars near the origin within 3.8 px;
        // the exact model pairs every star scene() kept.
        let (w, h) = (6000.0, 4000.0);
        let subject = field(11, 1500, w, h);
        let truth = similarity(1.008, 8.0, 300.0, -200.0);
        let (subject, reference) = scene(&subject, &truth, 0.05, 0, w, h, 12);
        let ref_pts: Vec<(f64, f64)> = reference.iter().map(|s| (s.x, s.y)).collect();
        let tree = KdTree2::build(&ref_pts);
        let radius = 2.0 * RegistrationConfig::default().ransac_tolerance_px;
        let exact = pair_through(&truth, &subject, &reference, &tree, radius);
        let off = pair_through(
            &similarity(1.008 * 1.002, 8.0, 300.0, -200.0),
            &subject,
            &reference,
            &tree,
            radius,
        );
        assert!(exact.pairs.len() >= 1000, "exact {}", exact.pairs.len());
        assert!(exact.all_sigmas);
        assert!(
            off.pairs.len() < exact.pairs.len() / 3,
            "off {} exact {}",
            off.pairs.len(),
            exact.pairs.len()
        );
        // The mechanism itself, independent of how good the quad seed is: a
        // refit on the off-model's origin-clustered pairs recovers the field.
        let sim = RegistrationConfig {
            model: ModelChoice::Similarity,
            ..Default::default()
        };
        let (_, refit_off, _) = ransac_and_refit(&off, &sim, (w as usize, h as usize)).unwrap();
        let recovered = pair_through(&refit_off.linear, &subject, &reference, &tree, radius);
        assert!(
            recovered.pairs.len() as f64 >= 0.98 * exact.pairs.len() as f64,
            "recovered {} off {} exact {}",
            recovered.pairs.len(),
            off.pairs.len(),
            exact.pairs.len()
        );
        // End to end, whatever the seed's accuracy, the final pairing is complete.
        let a = align(
            &subject,
            &reference,
            (w as usize, h as usize),
            (w as usize, h as usize),
            &RegistrationConfig::default(),
        )
        .unwrap();
        assert!(
            a.pairs as f64 >= 0.98 * exact.pairs.len() as f64,
            "aligned pairs {} exact {} repaired {}",
            a.pairs,
            exact.pairs.len(),
            a.repaired
        );
        assert!(a.rms_px < 0.1, "rms {}", a.rms_px);
    }
}
