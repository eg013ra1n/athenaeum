//! Subject → reference alignment (spec §3.2–3.3, §3.6): a quad-matched
//! seed (or, from M4b, a seed handed in from the two frames' plate solves
//! — see [`super::wcs_seed`]), KD-tree correspondences, RANSAC on the
//! configured linear model, a σ-weighted refit, optional polynomial
//! distortion and the QA gates. Pure geometry over star lists; no I/O.

use std::fmt;

use serde::{Deserialize, Serialize};
use solvemyastro::quad::{build_quads, fit_affine, group_size_for, match_quads};

use super::detect::Star;
use super::wcs_seed::{WCS_SEED_RADIUS_FACTOR, WCS_SEED_RADIUS_MIN_PX};
use super::{DistortionChoice, ModelChoice, RegistrationConfig, SCALE_TOLERANCE};
use crate::geometry::{
    ransac_fit, refit_weighted, Distortion, KdTree2, Linear, LinearKind, Pair, PixelMap,
    RansacConfig, RansacResult, RefitResult,
};

/// Quad-ratio tolerance of the seed matcher (the plate solver's default).
pub const QUAD_TOLERANCE: f64 = 0.007;
/// RANSAC and the QA gates need at least this many inliers.
pub const MIN_INLIERS: usize = 8;
/// A linear scale outside this range fails the frame (spec §3.6). M4b:
/// derived from the shared [`SCALE_TOLERANCE`] rather than a second literal
/// — bit-identical to the old `(0.8, 1.25)` tuple (`1.0 / 1.25 == 0.8`
/// exactly in `f64`; pinned by
/// `tests::scale_range_matches_the_tolerance_constant`), so every existing
/// test that pins this gate keeps passing unchanged.
pub const SCALE_RANGE: (f64, f64) = (1.0 / SCALE_TOLERANCE, SCALE_TOLERANCE);
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
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum AlignError {
    TooFewStars { subject: usize, reference: usize },
    /// M4b: `after_wcs` marks a quad-seed failure that followed a
    /// plate-solve seed the aligner had already tried and refused — the
    /// frame's stored reason would otherwise lose that fact entirely.
    NoSeed { matches: usize, after_wcs: bool },
    TooFewMatches { matches: usize },
    TooFewInliers { inliers: usize },
    Degenerate,
    /// M4b: `expected` is the scale ratio the gate was centred on (1.0 for
    /// a same-scale set, the frame's own `pixel_scale / reference scale`
    /// otherwise — see [`super::scale_gate_for`]). The applied window is
    /// `expected` ± [`SCALE_TOLERANCE`], which is what [`fmt::Display`]
    /// prints, so the message names the gate the frame was actually judged
    /// against rather than a constant that may not have been used.
    ScaleOutOfRange { scale: f64, expected: f64 },
    RmsTooHigh { rms_px: f64, max_rms_px: f64 },
}

impl fmt::Display for AlignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlignError::TooFewStars { subject, reference } => write!(
                f,
                "too few stars (subject {subject}, reference {reference})"
            ),
            AlignError::NoSeed {
                matches,
                after_wcs: false,
            } => {
                write!(f, "quad seed failed ({matches} quad matches)")
            }
            AlignError::NoSeed {
                matches,
                after_wcs: true,
            } => write!(
                f,
                "quad seed failed ({matches} quad matches) \
                 after a plate-solve seed was tried and refused"
            ),
            AlignError::TooFewMatches { matches } => {
                write!(f, "only {matches} correspondences within tolerance")
            }
            AlignError::TooFewInliers { inliers } => write!(f, "only {inliers} inliers"),
            AlignError::Degenerate => write!(f, "degenerate transform"),
            AlignError::ScaleOutOfRange { scale, expected } => write!(
                f,
                "scale {scale:.2} outside [{:.2}, {:.2}] (expected {expected:.2})",
                expected / SCALE_TOLERANCE,
                expected * SCALE_TOLERANCE
            ),
            AlignError::RmsTooHigh { rms_px, max_rms_px } => {
                write!(f, "RMS {rms_px:.2} px above {max_rms_px:.2}")
            }
        }
    }
}

impl std::error::Error for AlignError {}

/// Where an alignment's initial transform came from (M4b, ruling
/// R-M4b-3). It rides in [`Alignment`] and in [`model_name`] so a stored
/// `registration_results.model` says which of the two produced the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedKind {
    /// The quad matcher over both star lists (the M1–M4a path).
    Quads,
    /// A transform handed in by the caller, built from the two frames'
    /// stored plate solves.
    Wcs,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// Subject → reference (with the stored inverse).
    pub map: PixelMap,
    pub model: LinearKind,
    pub distortion_order: Option<u8>,
    /// Which seed produced the initial transform (M4b).
    pub seed: SeedKind,
    /// Quad matches behind a [`SeedKind::Quads`] seed; 0 for a WCS seed,
    /// which pairs nothing to arrive at its transform.
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
/// `+polynomial<o>` when a distortion was fitted, plus `+wcs` (M4b) when
/// the alignment started from a plate-solve seed rather than the quads.
pub fn model_name(kind: LinearKind, distortion_order: Option<u8>, seed: SeedKind) -> String {
    let base = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut name = match distortion_order {
        Some(o) => format!("{base}+polynomial{o}"),
        None => base,
    };
    if seed == SeedKind::Wcs {
        name.push_str("+wcs");
    }
    name
}

/// The ratio a scale gate is centred on. Every producer builds the window
/// as `(r / SCALE_TOLERANCE, r * SCALE_TOLERANCE)` (see
/// [`super::scale_gate_for`] and [`SCALE_RANGE`]), so the geometric mean
/// recovers `r` exactly — including `1.0` for the fixed M1 window.
fn gate_center(gate: (f64, f64)) -> f64 {
    (gate.0 * gate.1).sqrt()
}

/// A polynomial of `order` needs `(order+1)(order+2)` pairs (twice its
/// per-axis term count).
fn min_pairs_for(order: u8) -> usize {
    let o = order as usize;
    (o + 1) * (o + 2)
}

/// Seed subject → reference from quad matching (subject plays "image",
/// reference plays "catalog"; the fitted affine maps image → catalog).
///
/// `after_wcs` marks a run that only reached the quad matcher because a
/// plate-solve seed was tried first and did not survive (M4b): the frame's
/// stored reason is `AlignError`'s `Display`, so without the flag a
/// failure here would read as though the WCS seed had never been offered.
fn seed_affine(
    sub: &[(f64, f64)],
    refp: &[(f64, f64)],
    after_wcs: bool,
) -> Result<(Linear, usize), AlignError> {
    let sub_q = build_quads(sub, sub.len(), group_size_for(sub.len()));
    let ref_q = build_quads(refp, refp.len(), group_size_for(refp.len()));
    let matches = match_quads(&sub_q, &ref_q, QUAD_TOLERANCE);
    let a = fit_affine(&matches, &sub_q, &ref_q).ok_or(AlignError::NoSeed {
        matches: matches.len(),
        after_wcs,
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

/// Everything one seed has to survive: the correspondences it produces at
/// the pairing radius, then the RANSAC and refit on them. The [`Pairing`]
/// comes back either way, so a caller that wants to try a different seed
/// (M4b's quad fallback) can say how far this one got.
fn pair_and_fit(
    seed: &Linear,
    subject: &[Star],
    reference: &[Star],
    tree: &KdTree2,
    radius: f64,
    cfg: &RegistrationConfig,
    reference_geometry: (usize, usize),
) -> (
    Pairing,
    Result<(RansacResult, RefitResult, LinearKind), AlignError>,
) {
    let pairing = pair_through(seed, subject, reference, tree, radius);
    if pairing.pairs.len() < MIN_INLIERS {
        let matches = pairing.pairs.len();
        return (pairing, Err(AlignError::TooFewMatches { matches }));
    }
    let fitted = ransac_and_refit(&pairing, cfg, reference_geometry);
    (pairing, fitted)
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

/// `hint` (M4b) is an optional subject → reference transform the caller
/// already believes — today the [`super::wcs_seed`] affine built from both
/// frames' plate solves. It replaces the quad seed when it pairs at least
/// [`MIN_INLIERS`] stars within [`WCS_SEED_RADIUS_FACTOR`] × the RANSAC
/// tolerance (floor [`WCS_SEED_RADIUS_MIN_PX`]); otherwise the quad seed
/// runs as before and a warning records that the hint was not confirmed.
///
/// `scale_gate` is the window the refitted linear scale must land in —
/// [`super::scale_gate_for`] centres it on the frame's own expected ratio
/// to the reference, so a genuinely binned frame is judged against 2.0
/// rather than 1.0.
pub fn align(
    subject: &[Star],
    reference: &[Star],
    reference_geometry: (usize, usize),
    subject_geometry: (usize, usize),
    cfg: &RegistrationConfig,
    hint: Option<&Linear>,
    scale_gate: (f64, f64),
) -> Result<Alignment, AlignError> {
    if subject.len() < MIN_INLIERS || reference.len() < MIN_INLIERS {
        return Err(AlignError::TooFewStars {
            subject: subject.len(),
            reference: reference.len(),
        });
    }
    let sub_pts: Vec<(f64, f64)> = subject.iter().map(|s| (s.x, s.y)).collect();
    let ref_pts: Vec<(f64, f64)> = reference.iter().map(|s| (s.x, s.y)).collect();
    let tree = KdTree2::build(&ref_pts);
    let radius = 2.0 * cfg.ransac_tolerance_px;
    let mut warnings = Vec::new();

    // 1. Seed. A hint is taken on trust only once it has paired enough
    // stars on its own — a plate solve for a different night, a stale
    // solve, or two frames that simply do not overlap all look like a
    // perfectly well-formed transform until it is asked to land on stars.
    // An unconfirmed one costs the quad seed's own work and a warning, not
    // the frame.
    let (seed, mut seed_matches, mut seed_kind) = match hint {
        Some(h) => {
            let radius_wcs =
                (WCS_SEED_RADIUS_FACTOR * cfg.ransac_tolerance_px).max(WCS_SEED_RADIUS_MIN_PX);
            let confirm = pair_through(h, subject, reference, &tree, radius_wcs);
            if confirm.pairs.len() >= MIN_INLIERS {
                (*h, 0, SeedKind::Wcs)
            } else {
                warnings.push(format!(
                    "wcs seed rejected ({} pairs); quad seed used",
                    confirm.pairs.len()
                ));
                let (s, m) = seed_affine(&sub_pts, &ref_pts, true)?;
                (s, m, SeedKind::Quads)
            }
        }
        None => {
            let (s, m) = seed_affine(&sub_pts, &ref_pts, false)?;
            (s, m, SeedKind::Quads)
        }
    };

    // 2–4. Correspondences through the seed (nearest reference star within
    // 2·tol), then RANSAC and the σ-weighted refit.
    //
    // A confirmed WCS seed clears a radius several times wider than the
    // pairing one, so a seed accurate to 4–8 px — a solve taken before the
    // rig was touched, a frame re-pointed since — can pass confirmation and
    // still pair almost nothing here. That is not a reason to fail a frame
    // the quad matcher would have carried, so the quad seed gets one turn
    // before the failure is believed.
    let (mut pairing, mut fitted) = pair_and_fit(
        &seed,
        subject,
        reference,
        &tree,
        radius,
        cfg,
        reference_geometry,
    );
    if fitted.is_err() && seed_kind == SeedKind::Wcs {
        warnings.push(format!(
            "wcs seed pairing failed ({} pairs); quad seed used",
            pairing.pairs.len()
        ));
        let (quad, matches) = seed_affine(&sub_pts, &ref_pts, true)?;
        seed_matches = matches;
        seed_kind = SeedKind::Quads;
        let retry = pair_and_fit(
            &quad,
            subject,
            reference,
            &tree,
            radius,
            cfg,
            reference_geometry,
        );
        pairing = retry.0;
        fitted = retry.1;
    }
    let (mut ransac, mut refit, mut kind) = fitted?;

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
    if !(refit_scale >= scale_gate.0 && refit_scale <= scale_gate.1) {
        return Err(AlignError::ScaleOutOfRange {
            scale: refit_scale,
            expected: gate_center(scale_gate),
        });
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
        seed: seed_kind,
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

    /// M4b: `SCALE_RANGE` is now derived from `SCALE_TOLERANCE` rather than
    /// a second literal — pin that the derived tuple still equals the old
    /// hand-written `(0.8, 1.25)` so every test pinning this gate keeps
    /// passing unchanged.
    #[test]
    fn scale_range_matches_the_tolerance_constant() {
        assert_eq!(SCALE_RANGE, (0.8, 1.25));
        assert!((SCALE_RANGE.0 - 1.0 / SCALE_TOLERANCE).abs() < 1e-12);
        assert_eq!(SCALE_RANGE.1, SCALE_TOLERANCE);
    }

    #[test]
    fn align_error_serializes_camel_case_fields() {
        let e = AlignError::RmsTooHigh {
            rms_px: 1.5,
            max_rms_px: 1.0,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"kind":"rmsTooHigh","rmsPx":1.5,"maxRmsPx":1.0}"#);
        let back: AlignError = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }

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

    /// The M1–M4a call shape: no plate-solve hint, the fixed
    /// [`SCALE_RANGE`] gate. Every test written before M4b keeps its
    /// original meaning by going through here.
    fn align_default(
        subject: &[Star],
        reference: &[Star],
        reference_geometry: (usize, usize),
        subject_geometry: (usize, usize),
        cfg: &RegistrationConfig,
    ) -> Result<Alignment, AlignError> {
        align(
            subject,
            reference,
            reference_geometry,
            subject_geometry,
            cfg,
            None,
            SCALE_RANGE,
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
        let a = align_default(
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
        assert_eq!(a.seed, SeedKind::Quads);
        assert_eq!(
            model_name(a.model, a.distortion_order, a.seed),
            "homography"
        );
    }

    #[test]
    fn a_mirrored_subject_is_flipped_not_failed() {
        let subject = field(2, 250, W, H);
        let truth = Linear::from_flat(
            LinearKind::Affine,
            [-1.0, 0.0, W - 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        );
        let (sub, refs) = scene(&subject, &truth, 0.05, 60, W, H, 8);
        let a = align_default(
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
            let a = align_default(
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
            align_default(
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
            align_default(&few, &few, geo, geo, &cfg),
            Err(AlignError::TooFewStars { .. })
        ));
        let subject = field(5, 200, W, H);
        let big = similarity(1.5, 0.0, 0.0, 0.0);
        let (sub, refs) = scene(&subject, &big, 0.02, 0, W, H, 10);
        assert!(matches!(
            align_default(&sub, &refs, geo, geo, &cfg),
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
            align_default(&sub, &refs, geo, geo, &strict),
            Err(AlignError::RmsTooHigh { .. })
        ));
        let lenient = RegistrationConfig {
            max_rms_px: 0.5,
            ransac_tolerance_px: 4.0,
            ..Default::default()
        };
        let a = align_default(&sub, &refs, geo, geo, &lenient).unwrap();
        assert!(
            a.warnings.iter().any(|w| w.contains("RMS")),
            "{:?}",
            a.warnings
        );
        let unrelated = field(6, 200, W, H);
        assert!(align_default(&subject, &unrelated, geo, geo, &cfg).is_err());
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
        let linear_only = align_default(
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
        let a = align_default(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(a.distortion_order, Some(3));
        assert!(a.rms_px < 0.05, "polynomial rms {}", a.rms_px);
        assert_eq!(
            model_name(a.model, a.distortion_order, a.seed),
            "homography+polynomial3"
        );
        let auto = RegistrationConfig {
            distortion: DistortionChoice::Auto,
            ..Default::default()
        };
        assert_eq!(
            align_default(&subject, &reference, geo, geo, &auto)
                .unwrap()
                .distortion_order,
            None,
            "same geometry: auto stays linear"
        );
        assert_eq!(
            align_default(&subject, &reference, geo, (1010, 800), &auto)
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
        // End to end. On this benign scene the quad seed already pairs the
        // field (`repaired` measured 0), so this only guards completeness;
        // the mechanism proof is the check above.
        let a = align_default(
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

    // ── M4b: the per-frame gate and the plate-solve seed ──────────────────

    /// A similarity of `scale` and `rot_deg` taking `src_c` onto `dst_c` —
    /// the relation a software-binned subject has to its reference, with
    /// both frames' centres coincident on the sky.
    fn centred_similarity(
        scale: f64,
        rot_deg: f64,
        src_c: (f64, f64),
        dst_c: (f64, f64),
    ) -> Linear {
        let (s, c) = rot_deg.to_radians().sin_cos();
        let (a, b) = (scale * c, -scale * s);
        let (d, e) = (scale * s, scale * c);
        Linear::from_flat(
            LinearKind::Similarity,
            [
                a,
                b,
                dst_c.0 - a * src_c.0 - b * src_c.1,
                d,
                e,
                dst_c.1 - d * src_c.0 - e * src_c.1,
                0.0,
                0.0,
                1.0,
            ],
        )
    }

    const SUB_W: f64 = 2000.0;
    const SUB_H: f64 = 1500.0;
    const REF_W: f64 = 4000.0;
    const REF_H: f64 = 3000.0;

    /// A 2000x1500 subject and the SAME stars at twice the pixel scale,
    /// rotated 3 deg, in a 4000x3000 reference — a binned frame against a
    /// native-scale reference. The subject stars are inset far enough that
    /// the rotated, doubled field still lands wholly inside the reference,
    /// so `scene` drops none of them.
    fn binned_scene() -> (Vec<Star>, Vec<Star>, Linear) {
        let mut subject = field(31, 300, 1800.0, 1350.0);
        for s in &mut subject {
            s.x += 100.0;
            s.y += 75.0;
        }
        let truth = centred_similarity(
            2.0,
            3.0,
            (SUB_W / 2.0, SUB_H / 2.0),
            (REF_W / 2.0, REF_H / 2.0),
        );
        let (sub, refs) = scene(&subject, &truth, 0.05, 0, REF_W, REF_H, 33);
        assert_eq!(sub.len(), 300, "no subject star may be dropped");
        assert_eq!(refs.len(), 300, "no reference star may be dropped");
        (sub, refs, truth)
    }

    fn binned_geometry() -> ((usize, usize), (usize, usize)) {
        (
            (REF_W as usize, REF_H as usize),
            (SUB_W as usize, SUB_H as usize),
        )
    }

    /// (a) The M1 fixed gate refuses a genuinely binned frame — and it is
    /// the GATE that refuses it, not the seed: the quad matcher is
    /// scale-invariant and finds the field perfectly well.
    #[test]
    fn the_fixed_gate_refuses_a_two_times_binned_frame() {
        let (sub, refs, _) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let err = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            None,
            SCALE_RANGE,
        )
        .expect_err("2x is outside [0.8, 1.25]");
        match err {
            AlignError::ScaleOutOfRange { scale, expected } => {
                assert!((scale - 2.0).abs() < 0.01, "scale {scale}");
                assert_eq!(expected, 1.0);
            }
            other => panic!("expected the gate to refuse it, got {other}"),
        }
        assert_eq!(
            format!(
                "{}",
                AlignError::ScaleOutOfRange {
                    scale: 2.0,
                    expected: 1.0
                }
            ),
            "scale 2.00 outside [0.80, 1.25] (expected 1.00)"
        );
    }

    /// (b) The per-frame gate, centred on the frame's own 2x ratio, lets
    /// the same alignment through on the quad seed alone.
    #[test]
    fn the_per_frame_gate_admits_the_binned_frame() {
        let (sub, refs, _) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));
        assert_eq!(gate, (1.6, 2.5));
        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            None,
            gate,
        )
        .expect("the widened gate admits it");
        assert!((a.scale - 2.0).abs() < 0.01, "scale {}", a.scale);
        assert_eq!(a.seed, SeedKind::Quads);
        assert!(a.inliers >= 200, "inliers {}", a.inliers);
    }

    /// (c) A hint that pairs the field replaces the quad seed outright.
    #[test]
    fn a_confirmed_hint_replaces_the_quad_seed() {
        let (sub, refs, truth) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));
        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            Some(&truth),
            gate,
        )
        .expect("the hint pairs the field");
        assert_eq!(a.seed, SeedKind::Wcs);
        assert_eq!(a.seed_matches, 0, "a WCS seed pairs no quads");
        assert!(
            a.inliers as f64 >= 0.9 * sub.len() as f64,
            "inliers {} of {}",
            a.inliers,
            sub.len()
        );
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        assert_eq!(
            model_name(a.model, a.distortion_order, a.seed),
            "homography+wcs"
        );
    }

    /// (d) A hint that does NOT pair falls back to the quad seed and says
    /// so, rather than failing the frame on a bad guess.
    #[test]
    fn an_unconfirmed_hint_falls_back_to_the_quad_seed() {
        let (sub, refs, truth) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));
        let mut wrong = truth;
        wrong.m[0][2] += 30.0;
        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            Some(&wrong),
            gate,
        )
        .expect("the quad seed carries the frame");
        assert_eq!(a.seed, SeedKind::Quads);
        assert!(
            a.warnings.iter().any(|w| w.contains("wcs seed rejected")),
            "{:?}",
            a.warnings
        );
        assert!((a.scale - 2.0).abs() < 0.01, "scale {}", a.scale);
    }

    /// Fix round 1 (m1): a seed accurate to a few pixels clears the WCS
    /// confirmation radius (8 px) and then pairs nothing at the tighter
    /// pairing radius (2 · 1.9 px). The quad seed gets one turn before the
    /// frame is failed, and the record says what happened.
    #[test]
    fn a_hint_that_confirms_but_cannot_pair_falls_back_to_the_quad_seed() {
        let (sub, refs, truth) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let cfg = RegistrationConfig::default();
        let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));

        let mut stale = truth;
        stale.m[0][2] += 5.0;
        let radius_wcs =
            (WCS_SEED_RADIUS_FACTOR * cfg.ransac_tolerance_px).max(WCS_SEED_RADIUS_MIN_PX);
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let tree = KdTree2::build(&ref_pts);
        // Fixture premise: 5 px confirms at 8 px and pairs at neither 3.8.
        assert!(
            pair_through(&stale, &sub, &refs, &tree, radius_wcs).pairs.len() >= MIN_INLIERS,
            "the stale seed must clear confirmation"
        );
        assert!(
            pair_through(&stale, &sub, &refs, &tree, 2.0 * cfg.ransac_tolerance_px)
                .pairs
                .len()
                < MIN_INLIERS,
            "…and then fail the pairing radius"
        );

        let a = align(&sub, &refs, ref_geo, sub_geo, &cfg, Some(&stale), gate)
            .expect("the quad seed carries the frame");
        assert_eq!(a.seed, SeedKind::Quads);
        assert!(a.seed_matches > 0, "the quad seed's own match count");
        assert!(
            a.warnings
                .iter()
                .any(|w| w.contains("wcs seed pairing failed")),
            "{:?}",
            a.warnings
        );
        assert!((a.scale - 2.0).abs() < 0.01, "scale {}", a.scale);
    }

    /// Fix round 1 (m2): when the WCS seed is discarded AND the quad seed
    /// then fails, the frame's stored reason (an `AlignError`'s `Display`)
    /// must still say a plate-solve seed was offered first.
    #[test]
    fn a_quad_seed_failure_after_a_discarded_hint_says_so() {
        assert_eq!(
            format!(
                "{}",
                AlignError::NoSeed {
                    matches: 3,
                    after_wcs: false
                }
            ),
            "quad seed failed (3 quad matches)"
        );
        assert_eq!(
            format!(
                "{}",
                AlignError::NoSeed {
                    matches: 3,
                    after_wcs: true
                }
            ),
            "quad seed failed (3 quad matches) \
             after a plate-solve seed was tried and refused"
        );

        // End to end: an unrelated reference field, with a hint that
        // cannot confirm — the quad seed has nothing to match either.
        let subject = field(41, 200, SUB_W, SUB_H);
        let unrelated = field(42, 200, REF_W, REF_H);
        let (ref_geo, sub_geo) = binned_geometry();
        let hint = centred_similarity(
            2.0,
            0.0,
            (SUB_W / 2.0, SUB_H / 2.0),
            (REF_W * 4.0, REF_H * 4.0),
        );
        let err = align(
            &subject,
            &unrelated,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            Some(&hint),
            SCALE_RANGE,
        )
        .expect_err("nothing can align these");
        assert!(
            format!("{err}").contains("plate-solve seed"),
            "the reason must name the discarded seed: {err}"
        );
    }

    /// The gate's centre is recoverable from the window itself — the
    /// invariant `AlignError::ScaleOutOfRange`'s message rests on.
    #[test]
    fn a_gates_centre_is_its_geometric_mean() {
        assert_eq!(gate_center(SCALE_RANGE), 1.0);
        for ratio in [0.5, 1.0, 2.0, 3.7] {
            let gate = super::super::scale_gate_for(Some(ratio), Some(1.0));
            assert!((gate_center(gate) - ratio).abs() < 1e-12, "{ratio}");
        }
    }
}
