//! Subject → reference alignment (spec §3.2–3.3, §3.6): a quad-matched
//! seed (or, from M4b, a seed handed in from the two frames' plate solves
//! — see [`super::wcs_seed`]), KD-tree correspondences, RANSAC on the
//! configured linear model, a σ-weighted refit, optional polynomial
//! distortion and the QA gates. Pure geometry over star lists; no I/O.

use std::fmt;

use serde::{Deserialize, Serialize};
use solvemyastro::quad::{build_quads, fit_affine, group_size_for, match_quads, Affine, Quad};
use super::detect::Star;
use super::wcs_seed::seed_radius_px;
use super::local_loop;
use super::{DistortionChoice, ModelChoice, RegistrationConfig, SCALE_TOLERANCE};
// One import path for everything `geometry` re-exports at its root (fix
// round 1, minor 10); `DOMAIN_MARGIN`, `degenerate` and `SplitMix64` are
// the items that live only in their own modules.
use crate::geometry::polynomial::DOMAIN_MARGIN;
use crate::geometry::ransac::{degenerate, SplitMix64};
use crate::geometry::{
    ransac_fit, refit_weighted, select_nodes, Distortion, DistortionModel, KdTree2, Linear,
    LinearKind, Pair, PixelMap, RansacConfig, RansacResult, RefitResult, ThinPlateSpline,
    TPS_MAX_NODES, TPS_MIN_NODES,
};

/// The version of what [`align`] returns for an unchanged star list, folded
/// into the registration stage hash (`alignVersion`,
/// `stacking::config::registration_subtree`; robust quad seed RQ-5) because
/// no config field moves when the seed or the acceptance gate does. Policy:
/// one bump per merged plan that changes what `align` returns for the same
/// star lists, whatever the number of changes in it — the robust quad seed
/// and the coverage gate share version 1. A bump re-registers every set once
/// and re-normalizes and re-integrates what reads it.
pub const ALIGN_VERSION: u32 = 1;
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
/// A thin-plate spline needs this many refit inliers before it is fitted
/// at all (M4c Task 4, the plan's `4 · MIN_INLIERS`). Below it the spline
/// would interpolate a handful of stars and say nothing whatsoever about
/// the rest of the frame, so the linear model is kept instead — the same
/// stance [`min_pairs_for`] takes for the polynomial arm, at the scale a
/// LOCAL model needs.
pub const TPS_MIN_INLIERS: usize = 4 * MIN_INLIERS;

/// What a frame's distortion layer actually turned out to be — the third
/// state `Option<u8>` could not carry once M4c added a model with no
/// order (ruling R-M4c-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistortionFit {
    #[default]
    None,
    Polynomial(u8),
    Tps,
}

impl DistortionFit {
    /// The polynomial order, when that is what was fitted.
    pub fn order(self) -> Option<u8> {
        match self {
            DistortionFit::Polynomial(o) => Some(o),
            DistortionFit::None | DistortionFit::Tps => None,
        }
    }

    /// True for anything but [`DistortionFit::None`].
    pub fn is_some(self) -> bool {
        self != DistortionFit::None
    }
}

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
    /// RQ-10: the map's inliers cover `coverage` of the 4 x 4 grid over the
    /// overlap, which holds `cells` cells with an eligible star (see
    /// [`coverage`]), under [`MIN_COVERAGE`].
    LowCoverage { coverage: f64, cells: usize },
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
            AlignError::LowCoverage { coverage, .. } => write!(
                f,
                "registration inliers cover {coverage:.2} of the overlap (min {MIN_COVERAGE:.2})"
            ),
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

/// Which seed [`align`] tries FIRST when the caller hands it a hint (M4b,
/// ruling R-T6-9). Either way the other one is the fallback, so a hint is
/// never the only thing standing between a frame and its alignment — and
/// neither is the quad matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeedPolicy {
    /// The M1 order, byte-identical for every frame the quads carry: the
    /// quad matcher leads and the hint is only reached when it fails.
    #[default]
    QuadFirst,
    /// The hint leads. For a frame whose own pixel scale says a real step
    /// from the reference is expected — the quad matcher's tolerance is a
    /// RATIO tolerance and degrades exactly where a scale step puts it.
    WcsFirst,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// Subject → reference (with the stored inverse).
    pub map: PixelMap,
    pub model: LinearKind,
    /// Which distortion layer the frame ended up with (M4c): none, a
    /// polynomial of that order, or a thin-plate spline.
    pub distortion: DistortionFit,
    /// Which seed produced the initial transform (M4b).
    pub seed: SeedKind,
    /// Quad matches behind a [`SeedKind::Quads`] seed; 0 for a WCS seed,
    /// which pairs nothing to arrive at its transform.
    pub seed_matches: usize,
    /// Correspondences in the final pairing (the seed's, the step-4b
    /// re-paired set, or — after a KEPT local distortion round (M4c) —
    /// that round's own widened pairing, since that is the population
    /// [`Alignment::inliers`] and [`Alignment::inlier_ratio`] came out
    /// of).
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
    /// How much of the overlap the refit inliers cover (RQ-10, [`coverage`]):
    /// at least [`MIN_COVERAGE`] for any frame that registered.
    pub coverage: f64,
    pub ransac_iterations: usize,
    pub refit_rounds: usize,
    /// Rounds of the local distortion loop whose corrector was actually
    /// FITTED (M4c, ruling R-M4c-7; semantics settled by R-T4-1): the
    /// round's re-pairing produced at least [`MIN_INLIERS`]
    /// correspondences AND its corrector's RANSAC succeeded. A round that
    /// then CONVERGED counts — it did the work and found nothing left to
    /// fix; a round the pairing or the RANSAC ended before a corrector
    /// existed does not. Bounded by
    /// [`super::local_loop::LOCAL_DISTORTION_ROUNDS`]; 0 when
    /// `registration.localDistortion` is off, which is the default, and 0
    /// when there is no distortion layer for the loop to refit.
    ///
    /// Kept separate from [`Alignment::refit_rounds`], which has always
    /// meant the σ-clip rounds inside one
    /// [`crate::geometry::refit_weighted`] call (≤ 5) — one number cannot
    /// honestly be both.
    pub local_rounds: usize,
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
/// `+polynomial<o>` or `+tps` when a distortion was fitted, plus `+wcs`
/// (M4b) when the alignment started from a plate-solve seed rather than
/// the quads.
///
/// `+wcs` stays LAST whatever the distortion is — `homography+tps+wcs`,
/// never `homography+wcs+tps`. The frames table splits the trailing
/// `+wcs` off to render its own chip (`FramesTable.tsx::splitRegModel`),
/// so the order is a UI contract, not a cosmetic choice.
pub fn model_name(kind: LinearKind, distortion: DistortionFit, seed: SeedKind) -> String {
    let base = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut name = match distortion {
        DistortionFit::None => base,
        DistortionFit::Polynomial(o) => format!("{base}+polynomial{o}"),
        DistortionFit::Tps => format!("{base}+tps"),
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

/// A quad match whose quad centres lie within this many pixels of a fit's
/// image counts as that fit's consensus (RQ-1). Absolute, not scaled by the
/// field: the true matches of a sharp frame sit within 0.3 px of the truth.
pub const SEED_TOL_PX: f64 = 5.0;
/// The consensus loop re-admits matches within this multiple of the kept
/// set's median residual, never below [`SEED_TOL_PX`] (RQ-1 (c)).
pub const SEED_LO_MEDIAN_FACTOR: f64 = 5.0;
/// At most this many least-squares fits behind a consensus seed.
pub const SEED_LO_MAX_FITS: usize = 5;
/// The seed's RANSAC draws at most this many samples (the registration
/// RANSAC's own bound).
pub const SEED_RANSAC_MAX_ITERATIONS: usize = 2000;
/// The seed's `SplitMix64` start (the registration RANSAC's own).
pub const SEED_RANSAC_SEED: u64 = 0x5EED_5EED;

/// Which route produced a [`QuadSeed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRoute {
    /// Today's single `fit_affine` over every match: fewer than
    /// [`MIN_INLIERS`] matches, or RANSAC found no consensus of that size.
    Single,
    /// The plain fit was clean (every residual within [`SEED_TOL_PX`]) and
    /// is returned bit for bit.
    Clean,
    /// RANSAC over the matches, then least squares over the consensus until
    /// it repeats.
    Consensus,
}

/// The initial subject → reference estimate from the quad matches, with
/// what was kept to arrive at it.
#[derive(Debug, Clone)]
pub struct QuadSeed {
    pub affine: Affine,
    /// Per match, in `matches` order: true = in the returned fit.
    pub kept: Vec<bool>,
    pub route: SeedRoute,
    /// RANSAC samples drawn (0 off the [`SeedRoute::Consensus`] route).
    pub hypotheses: usize,
    /// `fit_affine` calls over at least [`MIN_INLIERS`] matches behind the
    /// returned fit.
    pub fits: usize,
}

impl QuadSeed {
    /// Matches left out of the returned fit.
    pub fn dropped(&self) -> usize {
        self.kept.iter().filter(|k| !**k).count()
    }
}

/// Distance, per match, between `a`'s image of the image-quad centre and the
/// catalog-quad centre.
fn quad_residuals(
    a: &Affine,
    matches: &[(usize, usize)],
    image_quads: &[Quad],
    catalog_quads: &[Quad],
) -> Vec<f64> {
    matches
        .iter()
        .map(|&(i, c)| {
            let (ix, iy) = image_quads[i].center;
            let (cx, cy) = catalog_quads[c].center;
            let (fx, fy) = a.apply(ix, iy);
            (fx - cx).hypot(fy - cy)
        })
        .collect()
}

/// The numpy median (the mean of the two middle values for an even count).
fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

/// `fit_affine` over the matches flagged in `keep`, in their original order.
fn fit_kept(
    keep: &[bool],
    matches: &[(usize, usize)],
    image_quads: &[Quad],
    catalog_quads: &[Quad],
) -> Option<Affine> {
    let subset: Vec<(usize, usize)> = matches
        .iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(m, _)| *m)
        .collect();
    fit_affine(&subset, image_quads, catalog_quads)
}

/// The quad seed that survives a share of false matches (RQ-1, RQ-2).
///
/// The plain fit over every match is today's call; a false quad match (a
/// star pattern that recurs elsewhere in the reference) drags it 40-50 px
/// off the field. The residual of a match under a fit is the distance
/// between the fit's image of its image-quad centre and its catalog-quad
/// centre.
///
/// 1. Fewer than [`MIN_INLIERS`] matches, or a plain fit whose every
///    residual is within [`SEED_TOL_PX`]: the plain fit, bit for bit
///    ([`SeedRoute::Single`], [`SeedRoute::Clean`]).
/// 2. Otherwise a deterministic RANSAC over the matches: 3-match samples
///    from a `SplitMix64` at [`SEED_RANSAC_SEED`], degenerate samples
///    skipped, a hypothesis is `fit_affine` over its three matches (its
///    xy-scale gate included), its consensus is every match within
///    [`SEED_TOL_PX`], a strictly larger consensus replaces the best. The
///    bound `N = ln(1 - 0.9999) / ln(1 - w^3)` shrinks with the best share
///    `w`, with an early exit above `w = 0.98` and at most
///    [`SEED_RANSAC_MAX_ITERATIONS`] samples.
/// 3. Least squares over the best consensus, then again over every match
///    within `max(SEED_TOL_PX, SEED_LO_MEDIAN_FACTOR x the kept set's
///    median)` of the fit, until that set repeats, at most
///    [`SEED_LO_MAX_FITS`] fits.
///
/// Why RANSAC and not a clipped fit: a clip rests on the median residual,
/// and false matches inflate it; past a small false share the clip removes
/// nothing and keeps the dragged fit. On the research's 1 054 match sets the
/// kept set equals the clipped estimator's 1 054 times out of 1 054; on
/// frame 162's 486 true matches under 30-85 % random false matches the seed
/// lands 0.0 px off the true one (clipped: 15.8-3 321 px). A RANSAC that
/// finds fewer than [`MIN_INLIERS`] consensus matches gives the plain fit
/// back, so no outcome is worse than today's; a plain fit that is `None`
/// with 8 matches or more still gets its RANSAC (it recovers 190 of 200 such
/// seeds of a 75 % random false set).
///
/// The bit-identity condition: a set whose plain fit is clean keeps it bit
/// for bit; a set whose consensus converges to every match gets
/// `fit_affine(all)` in the original order, the same bits. True matches lost
/// to a strongly non-uniform distortion (10 px or more at the corners
/// against the affine) give a narrower seed, not a wrong one.
///
/// `None` only when the plain fit is `None` and no consensus replaces it.
pub fn robust_quad_seed(
    matches: &[(usize, usize)],
    image_quads: &[Quad],
    catalog_quads: &[Quad],
) -> Option<QuadSeed> {
    let n = matches.len();
    let plain = fit_affine(matches, image_quads, catalog_quads);
    // Today's outcome, `None` included.
    let single = |plain: Option<Affine>| {
        plain.map(|affine| QuadSeed {
            affine,
            kept: vec![true; n],
            route: SeedRoute::Single,
            hypotheses: 0,
            fits: usize::from(n >= MIN_INLIERS),
        })
    };
    if n < MIN_INLIERS {
        return single(plain);
    }
    if let Some(affine) = &plain {
        let clean = quad_residuals(affine, matches, image_quads, catalog_quads)
            .iter()
            .all(|r| *r <= SEED_TOL_PX);
        if clean {
            return Some(QuadSeed {
                affine: affine.clone(),
                kept: vec![true; n],
                route: SeedRoute::Clean,
                hypotheses: 0,
                fits: 1,
            });
        }
    }

    // RANSAC over the quad-centre pairs; the draw is `ransac_fit`'s.
    let centres: Vec<Pair> = matches
        .iter()
        .map(|&(i, c)| (image_quads[i].center, catalog_quads[c].center))
        .collect();
    let mut rng = SplitMix64(SEED_RANSAC_SEED);
    let mut best: Vec<bool> = Vec::new();
    let mut best_n = 0usize;
    let mut needed = SEED_RANSAC_MAX_ITERATIONS;
    let mut hypotheses = 0usize;
    while hypotheses < needed.min(SEED_RANSAC_MAX_ITERATIONS) {
        hypotheses += 1;
        let mut picks = [0usize; 3];
        let mut drawn = 0;
        while drawn < 3 {
            let p = rng.below(n);
            if !picks[..drawn].contains(&p) {
                picks[drawn] = p;
                drawn += 1;
            }
        }
        let sample = [centres[picks[0]], centres[picks[1]], centres[picks[2]]];
        if degenerate(&sample) {
            continue;
        }
        let three = [matches[picks[0]], matches[picks[1]], matches[picks[2]]];
        let Some(hypothesis) = fit_affine(&three, image_quads, catalog_quads) else {
            continue;
        };
        // A non-finite residual compares false: out of the consensus.
        let consensus: Vec<bool> = quad_residuals(&hypothesis, matches, image_quads, catalog_quads)
            .iter()
            .map(|r| *r <= SEED_TOL_PX)
            .collect();
        let count = consensus.iter().filter(|c| **c).count();
        if count > best_n {
            best = consensus;
            best_n = count;
            let w = count as f64 / n as f64;
            if w > 0.98 {
                break;
            }
            let denom = (1.0 - w.powi(3)).ln();
            if denom < 0.0 {
                needed = ((1.0f64 - 0.9999).ln() / denom).ceil().max(1.0) as usize;
            }
        }
    }
    if best_n < MIN_INLIERS {
        return single(plain);
    }

    // Least squares over the consensus, re-admitting until the set repeats.
    let mut keep = best;
    let Some(mut fit) = fit_kept(&keep, matches, image_quads, catalog_quads) else {
        return single(plain);
    };
    let mut fits = 1;
    loop {
        let r = quad_residuals(&fit, matches, image_quads, catalog_quads);
        let kept_r: Vec<f64> = r
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(r, _)| *r)
            .collect();
        let tau = SEED_TOL_PX.max(SEED_LO_MEDIAN_FACTOR * median(&kept_r));
        let next: Vec<bool> = r.iter().map(|r| *r <= tau).collect();
        if next == keep
            || fits == SEED_LO_MAX_FITS
            || next.iter().filter(|k| **k).count() < MIN_INLIERS
        {
            break;
        }
        let Some(refit) = fit_kept(&next, matches, image_quads, catalog_quads) else {
            break;
        };
        fit = refit;
        keep = next;
        fits += 1;
    }
    Some(QuadSeed {
        affine: fit,
        kept: keep,
        route: SeedRoute::Consensus,
        hypotheses,
        fits,
    })
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
    let a = robust_quad_seed(&matches, &sub_q, &ref_q)
        .ok_or(AlignError::NoSeed {
            matches: matches.len(),
            after_wcs,
        })?
        .affine;
    Ok((
        Linear::from_flat(
            LinearKind::Affine,
            [a.a1, a.b1, a.c1, a.a2, a.b2, a.c2, 0.0, 0.0, 1.0],
        ),
        matches.len(),
    ))
}

/// The coverage index's grid over the overlap (RQ-10 (b)): 4 x 4 cells.
pub const COVERAGE_GRID: usize = 4;
/// The grid over the reference that decides which subject stars can be
/// judged at all (RQ-10 (a)): 16 x 16 cells.
pub const REFERENCE_OCCUPANCY_GRID: usize = 16;
/// A map whose inliers cover less of the overlap than this is refused
/// (RQ-10): 12 of 16 cells on a dense overlap.
pub const MIN_COVERAGE: f64 = 0.75;

/// How much of the overlap a map's inliers cover (RQ-10 (a)-(c)), in subject
/// pixels. Returns `(coverage, cells)`: the share of the 4 x 4 grid's cells
/// holding an eligible star that also hold an inlier, and the number of
/// cells holding an eligible star.
///
/// - A subject star is *eligible* when `map` lands it inside the reference
///   frame (no clamping into edge cells) in a 16 x 16 reference cell that
///   holds a reference star. A reference that is empty in part of the frame
///   (clouds, a dense nebula, a vignetted corner) therefore never refuses a
///   subject for stars that have nowhere to land.
/// - The 4 x 4 grid is laid over the bounding box of the eligible stars, so
///   it always spans the overlap: the whole frame for a full overlap, a
///   corner for a small one. A zero-width side of the box is one column or
///   row.
/// - `inlier_subject_pts` are the subject sides of the refit's inlier pairs
///   (the pairing carries coordinates, not star indices). One outside the
///   box, or in a cell without an eligible star, counts for nothing.
///
/// `(0.0, 0)` when no star is eligible. `reference_geometry` is the
/// reference frame's `(width, height)`.
pub fn coverage(
    subject: &[(f64, f64)],
    reference: &[(f64, f64)],
    inlier_subject_pts: &[(f64, f64)],
    map: &Linear,
    reference_geometry: (usize, usize),
) -> (f64, usize) {
    let (w, h) = (reference_geometry.0 as f64, reference_geometry.1 as f64);
    if !(w > 0.0 && h > 0.0) {
        return (0.0, 0);
    }
    let occupancy = REFERENCE_OCCUPANCY_GRID;
    let cell_of = |x: f64, y: f64| -> usize {
        let last = (occupancy - 1) as f64;
        let cx = ((x / w) * occupancy as f64).floor().clamp(0.0, last) as usize;
        let cy = ((y / h) * occupancy as f64).floor().clamp(0.0, last) as usize;
        cy * occupancy + cx
    };
    // Reference centroids are in the frame by construction; the clamp only
    // keeps one that rounds onto the far edge in the last cell.
    let mut occupied = vec![false; occupancy * occupancy];
    for &(x, y) in reference {
        if x.is_finite() && y.is_finite() {
            occupied[cell_of(x, y)] = true;
        }
    }
    let eligible: Vec<(f64, f64)> = subject
        .iter()
        .copied()
        .filter(|&(x, y)| {
            let (u, v) = map.apply(x, y);
            u.is_finite()
                && v.is_finite()
                && (0.0..w).contains(&u)
                && (0.0..h).contains(&v)
                && occupied[cell_of(u, v)]
        })
        .collect();
    if eligible.is_empty() {
        return (0.0, 0);
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &(x, y) in &eligible {
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    let g = COVERAGE_GRID;
    let axis = |v: f64, lo: f64, hi: f64| -> usize {
        if hi > lo {
            (((v - lo) / (hi - lo)) * g as f64)
                .floor()
                .clamp(0.0, (g - 1) as f64) as usize
        } else {
            0
        }
    };
    let cell = |x: f64, y: f64| axis(y, y0, y1) * g + axis(x, x0, x1);
    let mut holds_eligible = vec![false; g * g];
    for &(x, y) in &eligible {
        holds_eligible[cell(x, y)] = true;
    }
    let mut holds_inlier = vec![false; g * g];
    for &(x, y) in inlier_subject_pts {
        if (x0..=x1).contains(&x) && (y0..=y1).contains(&y) {
            holds_inlier[cell(x, y)] = true;
        }
    }
    let cells = holds_eligible.iter().filter(|c| **c).count();
    let covered = holds_eligible
        .iter()
        .zip(&holds_inlier)
        .filter(|(e, i)| **e && **i)
        .count();
    (covered as f64 / cells as f64, cells)
}

/// RMS, residual σ and peak |Δx|/|Δy| of `pairs` through `map`.
///
/// Evaluated through the EXACT distortion (ruling R-T4-3a): this is a few
/// hundred stars, so the grid path would build a half-million-sample
/// displacement grid to answer them — three orders of magnitude more
/// spline evaluations than doing it directly, once per registered frame,
/// and for a direction the pixel work may never ask about.
pub(super) fn residual_stats(map: &PixelMap, pairs: &[Pair]) -> Residuals {
    let mut sum2 = 0.0;
    let mut sum = 0.0;
    let (mut px, mut py) = (0.0f64, 0.0f64);
    for &((sx, sy), (rx, ry)) in pairs {
        let (fx, fy) = map.forward_exact(sx, sy);
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
pub(super) struct Pairing {
    pub pairs: Vec<Pair>,
    pub sigmas: Vec<(f64, f64)>,
    pub all_sigmas: bool,
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
    pair_projected(&|x, y| model.apply(x, y), subject, reference, tree, radius)
}

/// [`pair_through`] with the projection supplied: the local distortion
/// loop (ruling R-M4c-7) re-pairs through the whole current MAP, linear
/// part and distortion together, not just a linear model.
pub(super) fn pair_projected(
    project: &dyn Fn(f64, f64) -> (f64, f64),
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
        let (px, py) = project(s.x, s.y);
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

/// What [`align`] has left to try when the leading seed does not survive
/// the pairing and the fit (M4b). Never more than one turn each way: a
/// seed that has already failed is not retried, and the seed that took
/// over has no further fallback of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    None,
    ToQuads,
    ToWcs,
}

/// The short reason a lead attempt failed, for the R-T6-9 fallback's
/// warning — each error's own wording minus the prefix the warning already
/// supplies. The lead is a quad-seeded attempt, or a plate-solve one whose
/// map the coverage gate refused (`LowCoverage`).
fn quad_failure_reason(e: &AlignError) -> String {
    match e {
        AlignError::NoSeed { matches, .. } => format!("{matches} quad matches"),
        AlignError::TooFewMatches { matches } => format!("{matches} correspondences"),
        AlignError::TooFewInliers { inliers } => format!("{inliers} inliers"),
        AlignError::LowCoverage { coverage, .. } => {
            format!("map covers {coverage:.2} of the overlap")
        }
        other => other.to_string(),
    }
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

/// What a seed that survived [`fit_from_seed`] leaves: the RANSAC and refit
/// of the final pairing, the linear kind, how many pairs the step-4b
/// re-pairing added, and the coverage the gate measured.
type SeedFit = (RansacResult, RefitResult, LinearKind, usize, f64);

/// Steps 2-4b for one seed, and the coverage gate last (RQ-10): the pairing
/// and fit of [`pair_and_fit`], one re-pairing through the refit model (the
/// seed is an affine fitted on the matched quads and its accuracy falls off
/// with distance from them, so a rotated or rescaled subject pairs only
/// near them; one pass through the refit model recovers the rest of the
/// field), then [`coverage`] of the refit's inliers over the overlap. A map
/// whose inliers cover less of it than [`MIN_COVERAGE`] is refused with
/// [`AlignError::LowCoverage`], whichever seed produced it.
///
/// The [`Pairing`] comes back either way, so a caller that falls back to the
/// other seed (M4b) can say how far this one got. The warnings are this
/// attempt's own (the 4b re-pairing's); a caller that discards the attempt
/// discards them.
#[allow(clippy::too_many_arguments)]
fn fit_from_seed(
    seed: &Linear,
    subject: &[Star],
    reference: &[Star],
    sub_pts: &[(f64, f64)],
    ref_pts: &[(f64, f64)],
    tree: &KdTree2,
    radius: f64,
    cfg: &RegistrationConfig,
    reference_geometry: (usize, usize),
) -> (Pairing, Result<SeedFit, AlignError>, Vec<String>) {
    let mut warnings = Vec::new();
    let (mut pairing, fitted) = pair_and_fit(
        seed,
        subject,
        reference,
        tree,
        radius,
        cfg,
        reference_geometry,
    );
    let (mut ransac, mut refit, mut kind) = match fitted {
        Ok(f) => f,
        Err(e) => return (pairing, Err(e), warnings),
    };

    // 4b. Re-pair through the refit model.
    let mut repaired = 0usize;
    let again = pair_through(&refit.linear, subject, reference, tree, radius);
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

    // The gate: the refit's inliers (the subject side of their pairs)
    // against the overlap the refit's own map defines.
    let inlier_pts: Vec<(f64, f64)> = refit.inliers.iter().map(|&i| pairing.pairs[i].0).collect();
    let (covered, cells) = coverage(
        sub_pts,
        ref_pts,
        &inlier_pts,
        &refit.linear,
        reference_geometry,
    );
    if covered < MIN_COVERAGE {
        return (
            pairing,
            Err(AlignError::LowCoverage {
                coverage: covered,
                cells,
            }),
            warnings,
        );
    }
    (
        pairing,
        Ok((ransac, refit, kind, repaired, covered)),
        warnings,
    )
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

/// Fit the thin-plate-spline layer on `pairs` around `linear` (M4c,
/// ruling R-M4c-5).
///
/// Nodes are chosen grid-stratified over the reference frame
/// ([`select_nodes`], cap [`TPS_MAX_NODES`], best σ per cell first) so a
/// crowded corner cannot buy the whole budget. Each direction is then
/// fitted at ITS OWN evaluation points, exactly as the polynomial arm
/// does: the forward spline lives at `L(sub)` (which is where
/// `PixelMap::forward` asks for it) and carries `ref − L(sub)`; the
/// inverse lives at `ref` and carries `L(sub) − ref`. Both sets of
/// positions are in reference space, and they differ only by the
/// residual the spline is there to absorb.
///
/// Unlike the polynomial arm this does NOT rebalance the linear part:
/// the spline's own affine block absorbs any residual affine term, so
/// there is nothing to fold back out.
///
/// The returned [`TpsOutcome`] carries the honest QA triple demanded by
/// ruling R-T4-4 — see the body.
fn fit_tps(
    linear: &Linear,
    pairs: &[Pair],
    sigmas: Option<&[(f64, f64)]>,
    frame: (f64, f64),
    smoothing: f64,
) -> Option<TpsOutcome> {
    let (idx, coincident) =
        dedupe_nodes(linear, pairs, select_nodes(pairs, sigmas, frame, TPS_MAX_NODES));
    if idx.len() < TPS_MIN_NODES {
        return None;
    }
    let model = fit_tps_on(linear, pairs, &idx, smoothing)?;

    // Honest QA (ruling R-T4-4). An interpolating spline lands every one
    // of its own nodes by construction, so measuring the reported RMS
    // there says nothing about the model and leaves `maxRmsPx` unable to
    // fail a TPS frame at all. Measure on inliers the SHIPPED spline's
    // fit did not see.
    let mut notes = Vec::new();
    // The node cap (or a cell-stratified pick that could not use every
    // pair) leaves inliers out: they are the natural hold-out, and the
    // model measured on them is the one being shipped. Coincident pairs
    // are NOT hold-out candidates — see `dedupe_nodes`.
    let node_set: std::collections::HashSet<usize> = idx.iter().copied().collect();
    let held: Vec<usize> = (0..pairs.len())
        .filter(|i| !node_set.contains(i) && !coincident.contains(i))
        .collect();
    let qa = if !held.is_empty() {
        let held_pairs: Vec<Pair> = held.iter().map(|&i| pairs[i]).collect();
        PixelMap::with_distortion_model(*linear, model.clone())
            .map(|m| residual_stats(&m, &held_pairs))
    } else {
        // Every inlier IS a node: hold `TPS_HOLDOUT_STRIDE`-th of them
        // back, fit a spline on the rest, and report ITS error on the
        // held-out ones. The node order `select_nodes` returns is
        // cell-major round-robin, so a fixed stride over it is spread
        // across the frame rather than clustered.
        let holdout: Vec<usize> = idx.iter().copied().step_by(TPS_HOLDOUT_STRIDE).collect();
        let fitted: Vec<usize> = idx
            .iter()
            .enumerate()
            .filter(|(k, _)| k % TPS_HOLDOUT_STRIDE != 0)
            .map(|(_, &i)| i)
            .collect();
        let held_pairs: Vec<Pair> = holdout.iter().map(|&i| pairs[i]).collect();
        fit_tps_on(linear, pairs, &fitted, smoothing)
            .and_then(|m| PixelMap::with_distortion_model(*linear, m))
            .map(|m| residual_stats(&m, &held_pairs))
    };
    if qa.is_none() {
        notes.push(
            "tps hold-out fit failed; the reported RMS is measured at the spline's own nodes"
                .to_string(),
        );
    }
    Some(TpsOutcome { model, qa, notes })
}

/// Two nodes closer together than this — in EITHER direction's node
/// positions, which are `L(sub)` and `ref` — are one node as far as the
/// spline is concerned, and only the first is kept.
///
/// This is not a nicety. `pair_through` gives every subject star its own
/// nearest reference star, independently, so TWO subject stars can pair
/// to ONE reference star; RANSAC and the refit have no reason to drop
/// either. Both survive into the node list, the inverse spline's node set
/// then carries that reference position twice, and Bookstein's system is
/// EXACTLY singular — the whole spline fails and the frame silently falls
/// back to its linear model. Measured on the synthetic 450-star field: it
/// happens on roughly one seed in three.
///
/// 0.05 px is safe in both directions: no detector resolves two stars
/// that close as two objects, so a pair this near another is the same
/// star, never a distinct one being thrown away. It also keeps the solve
/// away from the ill-conditioned regime a near-duplicate produces.
pub const TPS_MIN_NODE_SEPARATION_PX: f64 = 0.05;

/// Splits `idx` into the nodes to fit on and the ones dropped for
/// coinciding with an earlier node (see
/// [`TPS_MIN_NODE_SEPARATION_PX`]), preserving `idx`'s order — whatever
/// priority [`select_nodes`] established survives, so of two coincident
/// pairs the one that comes FIRST in that order wins: the better-σ pair
/// of a cell when the node cap made `select_nodes` stratify, and simply
/// the lower pair index when it did not (under the cap `select_nodes`
/// returns `0..n` untouched, and there is no per-cell ranking to
/// inherit).
///
/// The dropped list matters beyond the fit: a dropped pair sits at (as
/// good as) the same position as a node, so it is NOT a hold-out
/// candidate for the QA measurement — measuring there would measure the
/// correspondence's own ambiguity (two subject stars, one reference star)
/// and blame it on the model.
fn dedupe_nodes(
    linear: &Linear,
    pairs: &[Pair],
    idx: Vec<usize>,
) -> (Vec<usize>, std::collections::HashSet<usize>) {
    let eps2 = TPS_MIN_NODE_SEPARATION_PX * TPS_MIN_NODE_SEPARATION_PX;
    let mut kept: Vec<usize> = Vec::with_capacity(idx.len());
    let mut dropped = std::collections::HashSet::new();
    let mut positions: Vec<((f64, f64), (f64, f64))> = Vec::with_capacity(idx.len());
    for i in idx {
        let ((sx, sy), r) = pairs[i];
        let f = linear.apply(sx, sy);
        let clash = positions.iter().any(|&(pf, pr)| {
            let (dfx, dfy) = (f.0 - pf.0, f.1 - pf.1);
            let (drx, dry) = (r.0 - pr.0, r.1 - pr.1);
            dfx * dfx + dfy * dfy < eps2 || drx * drx + dry * dry < eps2
        });
        if clash {
            dropped.insert(i);
        } else {
            kept.push(i);
            positions.push((f, r));
        }
    }
    (kept, dropped)
}

/// One spline pair over the node subset `idx` of `pairs`, around
/// `linear`. Both [`fit_tps`]'s shipped model and its hold-out model come
/// through here, so the two cannot drift apart.
///
/// `idx` is assumed already de-duplicated by [`dedupe_nodes`]; a
/// coincident pair in it makes the solve singular and the fit `None`.
fn fit_tps_on(
    linear: &Linear,
    pairs: &[Pair],
    idx: &[usize],
    smoothing: f64,
) -> Option<DistortionModel> {
    if idx.len() < TPS_MIN_NODES {
        return None;
    }
    let mut fwd_nodes = Vec::with_capacity(idx.len());
    let mut fwd_dx = Vec::with_capacity(idx.len());
    let mut fwd_dy = Vec::with_capacity(idx.len());
    let mut inv_nodes = Vec::with_capacity(idx.len());
    let mut inv_dx = Vec::with_capacity(idx.len());
    let mut inv_dy = Vec::with_capacity(idx.len());
    for &i in idx {
        let ((sx, sy), (rx, ry)) = pairs[i];
        let (px, py) = linear.apply(sx, sy);
        fwd_nodes.push((px, py));
        fwd_dx.push(rx - px);
        fwd_dy.push(ry - py);
        inv_nodes.push((rx, ry));
        inv_dx.push(px - rx);
        inv_dy.push(py - ry);
    }
    let forward = ThinPlateSpline::fit(&fwd_nodes, &fwd_dx, &fwd_dy, smoothing)?;
    let inverse = ThinPlateSpline::fit(&inv_nodes, &inv_dx, &inv_dy, smoothing)?;
    let domain = tps_domain(&forward, &inverse);
    let model = DistortionModel::tps(forward, inverse, domain);
    model.is_well_formed().then_some(model)
}

/// The union of both splines' node bounding boxes, each side inflated by
/// [`DOMAIN_MARGIN`] of its extent — the box evaluation clamps into and
/// the displacement grid covers. Same guard, same margin, as the
/// polynomial arm's `fitted_domain`: a pixel far outside the
/// star-covered region gets the nearest fitted edge's displacement
/// instead of an extrapolation nobody measured.
fn tps_domain(forward: &ThinPlateSpline, inverse: &ThinPlateSpline) -> [f64; 4] {
    let (f, i) = (forward.node_bounds(), inverse.node_bounds());
    let b = [
        f[0].min(i[0]),
        f[1].min(i[1]),
        f[2].max(i[2]),
        f[3].max(i[3]),
    ];
    let (mx, my) = ((b[2] - b[0]) * DOMAIN_MARGIN, (b[3] - b[1]) * DOMAIN_MARGIN);
    [b[0] - mx, b[1] - my, b[2] + mx, b[3] + my]
}

/// σ-derived least-squares weights for the distortion fits: `1 / (σx² +
/// σy² + ε)`, or `None` when any pair lacks a centroid σ.
fn distortion_weights(sigmas: Option<&[(f64, f64)]>) -> Option<Vec<f64>> {
    sigmas.map(|s| {
        s.iter()
            .map(|(sx, sy)| 1.0 / (sx * sx + sy * sy + 1e-4))
            .collect()
    })
}

/// The residual triple [`residual_stats`] reports: `(rms, σ, (peak_x,
/// peak_y))`, all in pixels.
pub(super) type Residuals = (f64, f64, (f64, f64));

/// What [`fit_tps`] produces: the shipped spline pair, the honest QA
/// triple (ruling R-T4-4) and any notes.
struct TpsOutcome {
    model: DistortionModel,
    /// Residuals on inliers the shipped spline's fit did NOT see. `None`
    /// only when the hold-out fit itself failed, which the notes say.
    qa: Option<Residuals>,
    notes: Vec<String>,
}

/// Every inlier a node? Then one in [`TPS_HOLDOUT_STRIDE`] of them is
/// held back from a second fit, and THAT fit's error on them is what the
/// frame reports (ruling R-T4-4). 5 = the ruling's 80/20 split.
pub const TPS_HOLDOUT_STRIDE: usize = 5;

/// One distortion fit's result, as step 5 and the local distortion loop
/// both need it.
pub(super) struct MapFit {
    pub map: PixelMap,
    /// The linear part — the joint polynomial fit rebalances it, the
    /// spline arm leaves it alone.
    pub linear: Linear,
    /// What was actually fitted, which is NOT always what was asked for:
    /// too few inliers or a fit that would not converge degrade to the
    /// linear model, with the reason in `notes`.
    pub fit: DistortionFit,
    /// Residuals measured on pairs the fit did not see — `Some` only for
    /// the spline arm, which interpolates its own nodes and would
    /// otherwise report a residual of zero (ruling R-T4-4). `None` means
    /// "measure through the map itself", which is honest for every model
    /// that does not interpolate.
    pub qa: Option<Residuals>,
    pub notes: Vec<String>,
}

/// Step 5 as a function, so the local distortion loop can re-run it
/// around a corrected linear model. A distortion the inlier count or the
/// solver refuses is a NOTE plus the linear model, never a failure.
pub(super) fn build_map(
    plan: DistortionFit,
    linear: Linear,
    inlier_pairs: &[Pair],
    sigmas: Option<&[(f64, f64)]>,
    reference_geometry: (usize, usize),
    tps_smoothing: f64,
) -> Result<MapFit, AlignError> {
    let mut notes = Vec::new();
    let linear_only = |notes: Vec<String>| -> Result<MapFit, AlignError> {
        Ok(MapFit {
            map: PixelMap::linear(linear).ok_or(AlignError::Degenerate)?,
            linear,
            fit: DistortionFit::None,
            qa: None,
            notes,
        })
    };
    match plan {
        DistortionFit::None => linear_only(notes),
        DistortionFit::Polynomial(o) if inlier_pairs.len() < min_pairs_for(o) => {
            notes.push(format!(
                "polynomial{o} distortion needs {} inliers, have {}; linear model kept",
                min_pairs_for(o),
                inlier_pairs.len()
            ));
            linear_only(notes)
        }
        DistortionFit::Polynomial(o) => {
            let center = (
                reference_geometry.0 as f64 / 2.0,
                reference_geometry.1 as f64 / 2.0,
            );
            let norm = reference_geometry.0.max(reference_geometry.1) as f64 / 2.0;
            let weights = distortion_weights(sigmas);
            match fit_distortion(o, linear, inlier_pairs, weights.as_deref(), center, norm) {
                Some((lin, d)) => match PixelMap::with_distortion(lin, d) {
                    Some(map) => Ok(MapFit {
                        map,
                        linear: lin,
                        fit: DistortionFit::Polynomial(o),
                        qa: None,
                        notes,
                    }),
                    None => Err(AlignError::Degenerate),
                },
                None => {
                    notes.push(format!(
                        "polynomial{o} distortion did not fit; linear model kept"
                    ));
                    linear_only(notes)
                }
            }
        }
        DistortionFit::Tps if inlier_pairs.len() < TPS_MIN_INLIERS => {
            notes.push(format!(
                "tps distortion needs {TPS_MIN_INLIERS} inliers, have {}; linear model kept",
                inlier_pairs.len()
            ));
            linear_only(notes)
        }
        DistortionFit::Tps => {
            let frame = (reference_geometry.0 as f64, reference_geometry.1 as f64);
            match fit_tps(&linear, inlier_pairs, sigmas, frame, tps_smoothing) {
                Some(outcome) => match PixelMap::with_distortion_model(linear, outcome.model) {
                    Some(map) => {
                        notes.extend(outcome.notes);
                        Ok(MapFit {
                            map,
                            linear,
                            fit: DistortionFit::Tps,
                            qa: outcome.qa,
                            notes,
                        })
                    }
                    None => Err(AlignError::Degenerate),
                },
                None => {
                    notes.push("tps distortion did not fit; linear model kept".to_string());
                    linear_only(notes)
                }
            }
        }
    }
}

/// `hint` (M4b) is an optional subject → reference transform the caller
/// already believes — today the [`super::wcs_seed`] affine built from both
/// frames' plate solves. It is only ever used once it has paired at least
/// [`MIN_INLIERS`] stars within [`seed_radius_px`] of the reference's own,
/// and `policy` says whether it leads or covers for the quad matcher
/// (ruling R-T6-9). Whichever leads, the other gets ONE turn if it fails,
/// and a warning records the switch. A frame with no hint takes exactly
/// the M1 path, quad seed and error messages alike.
///
/// A seed also fails when its map is not carried by the overlap (RQ-10):
/// the last check of [`fit_from_seed`] is [`coverage`] against
/// [`MIN_COVERAGE`], and a refusal takes the same one turn each way. A
/// frame refused both ways fails with [`AlignError::LowCoverage`]; one that
/// passes keeps the map, warnings and error texts it had before the gate.
///
/// `scale_gate` is the window the refitted linear scale must land in —
/// [`super::scale_gate_for`] centres it on the frame's own expected ratio
/// to the reference, so a genuinely binned frame is judged against 2.0
/// rather than 1.0.
#[allow(clippy::too_many_arguments)]
pub fn align(
    subject: &[Star],
    reference: &[Star],
    reference_geometry: (usize, usize),
    subject_geometry: (usize, usize),
    cfg: &RegistrationConfig,
    hint: Option<&Linear>,
    policy: SeedPolicy,
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
    // `confirm_hint` is that check at the wide confirmation radius: `Ok`
    // with the hint, or `Err` with however few stars it managed (0 when
    // the caller handed in no hint at all).
    let radius_wcs = seed_radius_px(cfg.ransac_tolerance_px);
    let confirm_hint = || -> Result<Linear, usize> {
        match hint {
            None => Err(0),
            Some(h) => {
                let n = pair_through(h, subject, reference, &tree, radius_wcs)
                    .pairs
                    .len();
                if n >= MIN_INLIERS {
                    Ok(*h)
                } else {
                    Err(n)
                }
            }
        }
    };

    // Which seed leads, and what remains to fall back on (ruling R-T6-9).
    // `WcsFirst` is the cross-scale case and behaves as it has since Task
    // 2. `QuadFirst` is every same-scale frame: the M1 path runs first,
    // untouched — and the hint, when the caller has one, catches what it
    // drops. On real data (set 195, run 16) the quad matcher lost 14 of 30
    // H-alpha frames to "0 inliers" against an O-filter reference of the
    // SAME rig, every one of them carrying 456-465 matched stars in its own
    // stored solve; a plate-solve seed carries exactly those frames.
    let wcs_leads = policy == SeedPolicy::WcsFirst && hint.is_some();
    let (seed, mut seed_matches, mut seed_kind, fallback) = if wcs_leads {
        match confirm_hint() {
            // Quads remain as the fallback (a confirmed seed can still fail
            // to pair at the tighter radius — a solve taken before the rig
            // was touched, a frame re-pointed since).
            Ok(h) => (h, 0, SeedKind::Wcs, Fallback::ToQuads),
            Err(n) => {
                warnings.push(format!("wcs seed rejected ({n} pairs); quad seed used"));
                let (s, m) = seed_affine(&sub_pts, &ref_pts, true)?;
                (s, m, SeedKind::Quads, Fallback::None)
            }
        }
    } else {
        match seed_affine(&sub_pts, &ref_pts, false) {
            Ok((s, m)) => {
                let fallback = if hint.is_some() {
                    Fallback::ToWcs
                } else {
                    Fallback::None
                };
                (s, m, SeedKind::Quads, fallback)
            }
            // The quad matcher found nothing to match. With a confirmed
            // hint in hand that is not the end of the frame; without one
            // the M1 verdict stands, word for word.
            Err(quad_err) => match confirm_hint() {
                Ok(h) => {
                    warnings.push(format!(
                        "quad seed failed ({}); plate-solve seed used",
                        quad_failure_reason(&quad_err)
                    ));
                    (h, 0, SeedKind::Wcs, Fallback::None)
                }
                Err(_) => return Err(quad_err),
            },
        }
    };

    // 2–4b. Correspondences through the seed (nearest reference star within
    // 2·tol), then RANSAC, the σ-weighted refit, the re-pairing and the
    // coverage gate — with one turn for whichever seed did not lead. Either
    // way the error a frame that fails BOTH ways reports is the quad path's,
    // which is the one every M1-era message named. A discarded attempt's
    // warnings go with it.
    let (mut pairing, mut fitted, mut fit_warnings) = fit_from_seed(
        &seed,
        subject,
        reference,
        &sub_pts,
        &ref_pts,
        &tree,
        radius,
        cfg,
        reference_geometry,
    );
    if let Err(lead_err) = &fitted {
        match fallback {
            Fallback::None => {}
            Fallback::ToQuads => {
                // A refused map says how much of the overlap it covered; any
                // other failure, how many stars it paired.
                let reason = match lead_err {
                    AlignError::LowCoverage { .. } => quad_failure_reason(lead_err),
                    _ => format!("{} pairs", pairing.pairs.len()),
                };
                warnings.push(format!(
                    "wcs seed pairing failed ({reason}); quad seed used"
                ));
                // The warning above travels with a returned `Ok`; when the
                // quad turn also fails the verdict is the quad path's and the
                // refusal's numbers would be gone. Log them here, before the
                // `?` below can drop them (never swallow errors).
                match lead_err {
                    AlignError::LowCoverage { coverage, cells } => tracing::warn!(
                        seed = "wcs",
                        coverage = *coverage,
                        cells = *cells,
                        pairs = pairing.pairs.len(),
                        reason = %reason,
                        "plate-solve seed refused; quad seed tried"
                    ),
                    _ => tracing::warn!(
                        seed = "wcs",
                        pairs = pairing.pairs.len(),
                        reason = %reason,
                        "plate-solve seed refused; quad seed tried"
                    ),
                }
                let (quad, matches) = seed_affine(&sub_pts, &ref_pts, true)?;
                seed_matches = matches;
                seed_kind = SeedKind::Quads;
                let retry = fit_from_seed(
                    &quad,
                    subject,
                    reference,
                    &sub_pts,
                    &ref_pts,
                    &tree,
                    radius,
                    cfg,
                    reference_geometry,
                );
                pairing = retry.0;
                fitted = retry.1;
                fit_warnings = retry.2;
            }
            Fallback::ToWcs => {
                // Ruling R-T6-9. The quad seed paired or fitted too little,
                // or its map covers too little of the overlap; the hint gets
                // the same pairing/RANSAC/refit/gate treatment. A retry that
                // also fails leaves the quad path's verdict standing rather
                // than replacing it with the seed's.
                let quad_reason = quad_failure_reason(lead_err);
                if let Ok(h) = confirm_hint() {
                    let retry = fit_from_seed(
                        &h,
                        subject,
                        reference,
                        &sub_pts,
                        &ref_pts,
                        &tree,
                        radius,
                        cfg,
                        reference_geometry,
                    );
                    if retry.1.is_ok() {
                        warnings.push(format!(
                            "quad seed failed ({quad_reason}); plate-solve seed used"
                        ));
                        seed_matches = 0;
                        seed_kind = SeedKind::Wcs;
                        pairing = retry.0;
                        fitted = retry.1;
                        fit_warnings = retry.2;
                    } else if let Err(retry_err) = &retry.1 {
                        // The quad verdict stands and the retry's own error
                        // is dropped with `retry`; say what it was.
                        tracing::warn!(
                            error = %retry_err,
                            "plate-solve seed retry failed; quad verdict stands"
                        );
                    }
                }
            }
        }
    }
    let (ransac, refit, kind, repaired, coverage) = fitted?;
    warnings.extend(fit_warnings);
    let pairs = &pairing.pairs;
    let sigmas = &pairing.sigmas;
    let all_sigmas = pairing.all_sigmas;
    let linear = refit.linear;
    let refit_scale = refit.linear.scale();
    if !(refit_scale >= scale_gate.0 && refit_scale <= scale_gate.1) {
        return Err(AlignError::ScaleOutOfRange {
            scale: refit_scale,
            expected: gate_center(scale_gate),
        });
    }

    // 5. Optional distortion on the refit inliers.
    let inlier_pairs: Vec<Pair> = refit.inliers.iter().map(|&i| pairs[i]).collect();
    let inlier_sigmas: Option<Vec<(f64, f64)>> =
        all_sigmas.then(|| refit.inliers.iter().map(|&i| sigmas[i]).collect());
    // The pairing the reported inlier RATIO is taken against. The local
    // distortion loop re-pairs, so a kept round moves this too.
    let pairs_len = pairs.len();
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
            // Ruling R-M4c-6: `auto` stays polynomial by inlier count —
            // the spline is never chosen for the user.
            order.map_or(DistortionFit::None, DistortionFit::Polynomial)
        }
        DistortionChoice::Tps => DistortionFit::Tps,
        other => other
            .order()
            .map_or(DistortionFit::None, DistortionFit::Polynomial),
    };
    let fitted = build_map(
        wanted,
        linear,
        &inlier_pairs,
        inlier_sigmas.as_deref(),
        reference_geometry,
        cfg.tps_smoothing,
    )?;
    warnings.extend(fitted.notes.clone());

    // 5b. The local distortion loop (ruling R-M4c-7), off by default and
    // a no-op without a distortion layer to refit — see
    // [`super::local_loop`] for the round and its accept guard.
    let outcome = local_loop::run(
        wanted,
        fitted,
        inlier_pairs,
        pairs_len,
        subject,
        reference,
        &tree,
        reference_geometry,
        cfg,
    );
    let local_rounds = outcome.rounds;
    let inlier_pairs = outcome.inlier_pairs;
    let pairs_len = outcome.pairs_len;
    let MapFit {
        map,
        linear,
        fit: distortion,
        qa,
        notes: _,
    } = outcome.fit;
    warnings.extend(outcome.warnings);
    let scale = linear.scale();

    // 6. QA through the final map — or, when the fitted model INTERPOLATES
    // its own inliers, over pairs its fit did not see (ruling R-T4-4). An
    // interpolating spline's residual at its own nodes is a solver
    // artefact, and reporting it would leave `maxRmsPx` unable to fail a
    // TPS frame at all. Every other model is measured where it always was.
    let (rms_px, sigma_rms_px, peak_px) = match qa {
        Some(triple) => triple,
        None => residual_stats(&map, &inlier_pairs),
    };
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
        distortion,
        seed: seed_kind,
        seed_matches,
        pairs: pairs_len,
        repaired,
        inliers: inlier_pairs.len(),
        inlier_ratio: inlier_pairs.len() as f64 / pairs_len.max(1) as f64,
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
        coverage,
        ransac_iterations: ransac.iterations,
        refit_rounds: refit.rounds,
        local_rounds,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;
    use crate::stacking::register::local_loop::{compose, LOCAL_DISTORTION_ROUNDS};

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
            SeedPolicy::QuadFirst,
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
        assert!(!a.flipped && !a.distortion.is_some());
        assert!(a.inlier_ratio > 0.5 && a.quality_score > 0.0);
        let (fx, fy) = a.map.forward(500.0, 400.0);
        let (tx, ty) = truth.apply(500.0, 400.0);
        assert!((fx - tx).abs() < 0.05 && (fy - ty).abs() < 0.05);
        assert_eq!(a.seed, SeedKind::Quads);
        assert_eq!(
            model_name(a.model, a.distortion, a.seed),
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
        assert_eq!(a.distortion, DistortionFit::Polynomial(3));
        assert!(a.rms_px < 0.05, "polynomial rms {}", a.rms_px);
        assert_eq!(
            model_name(a.model, a.distortion, a.seed),
            "homography+polynomial3"
        );
        let auto = RegistrationConfig {
            distortion: DistortionChoice::Auto,
            ..Default::default()
        };
        assert_eq!(
            align_default(&subject, &reference, geo, geo, &auto)
                .unwrap()
                .distortion,
            DistortionFit::None,
            "same geometry: auto stays linear"
        );
        assert_eq!(
            align_default(&subject, &reference, geo, (1010, 800), &auto)
                .unwrap()
                .distortion,
            DistortionFit::Polynomial(3),
            "cross geometry with ≥ 200 inliers: auto fits order 3"
        );
    }

    /// A smooth distortion field that is NOT a polynomial of order 2..=4:
    /// a 1.5 px checkerboard `1.5 · sin(x/120) · cos(y/100)` completing
    /// about 1.6 periods per axis across this 1000×800 frame, which a
    /// cubic surface has no terms for.
    ///
    /// The plan's brief said `sin(x/300) · cos(y/250)`. Measured, that
    /// completes only HALF a period per axis here and a cubic fits it to
    /// 0.055 px RMS — the pin would have passed the cubic and proved
    /// nothing. The wavelengths are shortened until the pin
    /// discriminates, which is the point of the pin; the amplitude is
    /// the brief's 1.5 px, so the pairing radius and the quad seed see
    /// exactly the field the brief intended.
    fn wobble_scene(seed: u64, n: usize) -> (Vec<Star>, Vec<Star>, (usize, usize)) {
        let subject = field(seed, n, W, H);
        let truth = similarity(1.0, 0.7, 3.0, -2.0);
        let reference: Vec<Star> = subject
            .iter()
            .filter_map(|s| {
                let (x, y) = truth.apply(s.x, s.y);
                let dx = 1.5 * (x / 120.0).sin() * (y / 100.0).cos();
                let dy = 1.5 * (x / 120.0).cos() * (y / 100.0).sin();
                let (x, y) = (x + dx, y + dy);
                (x >= 0.0 && y >= 0.0 && x < W && y < H).then_some(Star {
                    x,
                    y,
                    flux: s.flux,
                    sigma: s.sigma,
                })
            })
            .collect();
        (subject, reference, (W as usize, H as usize))
    }

    /// M4c Step 3(a), the pin that TPS earns its keep: on a field a cubic
    /// cannot represent, the spline lands the inliers where the
    /// polynomial leaves them a third of a pixel off.
    ///
    /// Measured at `λ = 0`: at the inliers, cubic 0.929 px vs spline
    /// 0.0013 px; off the inliers, over the whole star-covered field,
    /// cubic 1.193 px vs spline 0.108 px. The two spline numbers differ by
    /// two orders of magnitude for a reason worth stating — with `λ = 0`
    /// and every inlier a node the spline INTERPOLATES, so its residual
    /// AT the inliers is a solver artefact, not a model error. The
    /// off-inlier figure is the honest accuracy statement, and it beats
    /// the polynomial by 11× on its own.
    ///
    /// **λ is pinned explicitly here**, not taken from the default, which
    /// ruling R-T7-1 moved to 0.5. That default is calibrated at the
    /// 600-node cap on real 26 Mpx frames, whose residual field is smooth
    /// at frame scale; this 1000×800 synthetic wobbles with a period of
    /// ≈ 100 px over 450 nodes, so 0.5 over-smooths it — hold-out 0.717 px
    /// against the interpolating spline's 0.052, off-inlier 0.838 against
    /// 0.108. The second block below keeps the shipped default honest at
    /// the one thing that still has to hold on such a field: even
    /// over-smoothed, the spline beats the cubic off the inliers (0.838 vs
    /// 1.193 px, 1.4× rather than 11×).
    #[test]
    fn the_spline_follows_a_field_the_polynomial_cannot() {
        let (subject, reference, geo) = wobble_scene(31, 450);
        let poly = RegistrationConfig {
            distortion: DistortionChoice::Polynomial3,
            ..Default::default()
        };
        let p = align_default(&subject, &reference, geo, geo, &poly).unwrap();
        assert_eq!(p.distortion, DistortionFit::Polynomial(3));
        assert!(
            p.rms_px > 0.4,
            "a cubic should NOT be able to follow this field: rms {}",
            p.rms_px
        );

        let tps = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            // The interpolating spline — see this test's own doc for why λ
            // is spelled out rather than taken from the default.
            tps_smoothing: 0.0,
            ..Default::default()
        };
        let t = align_default(&subject, &reference, geo, geo, &tps).unwrap();
        assert_eq!(t.distortion, DistortionFit::Tps);
        assert!(t.rms_px <= 0.15, "spline rms {}", t.rms_px);
        assert_eq!(
            model_name(t.model, t.distortion, t.seed),
            "homography+tps",
            "the model label names the spline"
        );
        // `+wcs` (M4b) stays the LAST suffix — `FramesTable.tsx` strips it
        // off the end to render its own chip.
        assert_eq!(
            model_name(t.model, t.distortion, SeedKind::Wcs),
            "homography+tps+wcs"
        );

        // Ruling R-T7-1's shipped λ over-smooths this synthetic field, so
        // it gets the weaker of the two claims: it must still beat the
        // cubic off the inliers. Measured 0.838 px against the cubic's
        // 1.193 (the interpolating spline's own 0.108 is asserted below).
        let shipped = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            ..Default::default()
        };
        assert_eq!(
            shipped.tps_smoothing, 0.5,
            "this block exists to exercise the SHIPPED λ"
        );
        let ts = align_default(&subject, &reference, geo, geo, &shipped).unwrap();
        assert_eq!(ts.distortion, DistortionFit::Tps);
        assert!(
            off_truth_rms(&ts.map) < off_truth_rms(&p.map),
            "off-inlier at the shipped λ: spline {} px vs cubic {} px",
            off_truth_rms(&ts.map),
            off_truth_rms(&p.map)
        );

        // Off the inliers, over the whole star-covered field: the spline
        // still beats the cubic, which is the claim that matters for the
        // pixels a resampler actually asks about.
        let (poly_off, tps_off) = (off_truth_rms(&p.map), off_truth_rms(&t.map));
        assert!(
            tps_off < poly_off,
            "off-inlier: spline {tps_off} px vs cubic {poly_off} px"
        );
    }

    /// Ruling R-M4c-7 and its fix-round guard R-T4-5: the loop is bounded
    /// by [`LOCAL_DISTORTION_ROUNDS`], it KEEPS a round when the round
    /// earns it, and it never ships a map that generalizes worse.
    ///
    /// What actually happens on this scene, measured: the first map is
    /// fitted on 415 of the 450 pairs — the σ-clip refit drops 35 of them,
    /// which on a 1.5 px non-polynomial field are the legitimate stars in
    /// the deepest lobes. Re-pairing THROUGH that map recovers all 450,
    /// the corrector on the 35 it was never fitted on is far from the
    /// identity, and the round is kept: 415 → 450 inliers, two rounds run.
    /// That is exactly the partial-first-pairing case the loop exists for.
    ///
    /// The pin compares the two maps OFF their own nodes, against the
    /// truth field. It deliberately does NOT compare the two reported
    /// `rms_px` values: since ruling R-T4-4 those are hold-out estimates
    /// over each model's OWN node set (415 nodes vs 450, 83 held out vs
    /// 90), so the two numbers answer two different questions and neither
    /// ordering between them would mean anything.
    #[test]
    fn the_local_distortion_loop_is_bounded_and_keeps_a_round_it_earns() {
        let (subject, reference, geo) = wobble_scene(32, 450);
        let base_cfg = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            ..Default::default()
        };
        let base = align_default(&subject, &reference, geo, geo, &base_cfg).unwrap();
        assert_eq!(base.local_rounds, 0, "the loop is off by default");

        let loop_cfg = RegistrationConfig {
            local_distortion: true,
            ..base_cfg.clone()
        };
        let looped = align_default(&subject, &reference, geo, geo, &loop_cfg).unwrap();
        assert!(
            looped.local_rounds <= LOCAL_DISTORTION_ROUNDS,
            "{} rounds",
            looped.local_rounds
        );
        assert!(
            looped.local_rounds > 0,
            "a distortion model on and the loop enabled: it must have run"
        );
        assert_eq!(looped.distortion, DistortionFit::Tps);
        // A round was actually KEPT (ruling R-T4-5's pin): the map is not
        // the one the loop started from, and it is fitted on MORE
        // correspondences.
        assert!(
            looped.inliers > base.inliers,
            "a kept round must have grown the inlier set: {} vs {}",
            looped.inliers,
            base.inliers
        );
        assert_ne!(
            looped.map, base.map,
            "a kept round must have changed the map"
        );

        // Never worse, measured where it means something: both maps
        // against the truth field, at the same 400 points, none of which
        // is a node of either.
        let (base_off, loop_off) = (off_truth_rms(&base.map), off_truth_rms(&looped.map));
        assert!(
            loop_off <= base_off,
            "the loop must not generalize worse: {loop_off} px vs {base_off} px"
        );

        // The polynomial arm goes through the same loop.
        let poly_loop = RegistrationConfig {
            distortion: DistortionChoice::Polynomial3,
            local_distortion: true,
            ..Default::default()
        };
        let poly_base = RegistrationConfig {
            local_distortion: false,
            ..poly_loop.clone()
        };
        let b = align_default(&subject, &reference, geo, geo, &poly_base).unwrap();
        let l = align_default(&subject, &reference, geo, geo, &poly_loop).unwrap();
        assert!(l.local_rounds <= LOCAL_DISTORTION_ROUNDS && l.local_rounds > 0);
        // The polynomial arm reports the map's OWN residuals either way
        // (it does not interpolate), so these two numbers ARE comparable.
        assert!(
            l.rms_px <= b.rms_px + 1e-9,
            "polynomial loop rms {} vs base {}",
            l.rms_px,
            b.rms_px
        );
        assert!(off_truth_rms(&l.map) <= off_truth_rms(&b.map) + 1e-9);

        // With no distortion model there is nothing to refit, so the loop
        // does not run at all.
        let off = RegistrationConfig {
            distortion: DistortionChoice::Off,
            local_distortion: true,
            ..Default::default()
        };
        assert_eq!(
            align_default(&subject, &reference, geo, geo, &off)
                .unwrap()
                .local_rounds,
            0
        );
    }

    /// RMS of a map against [`wobble_scene`]'s own truth at 400 fixed
    /// points, none of them a star — the one comparison that is common to
    /// any two maps of that scene, whatever each was fitted on.
    fn off_truth_rms(map: &PixelMap) -> f64 {
        let truth = similarity(1.0, 0.7, 3.0, -2.0);
        let mut rng = SplitMix64(7777);
        let mut sum = 0.0;
        for _ in 0..400 {
            let (x, y) = (
                60.0 + rng.next_f64() * (W - 120.0),
                60.0 + rng.next_f64() * (H - 120.0),
            );
            let (tx, ty) = truth.apply(x, y);
            let (tx, ty) = (
                tx + 1.5 * (tx / 120.0).sin() * (ty / 100.0).cos(),
                ty + 1.5 * (tx / 120.0).cos() * (ty / 100.0).sin(),
            );
            let (fx, fy) = map.forward_exact(x, y);
            sum += (fx - tx).powi(2) + (fy - ty).powi(2);
        }
        (sum / 400.0).sqrt()
    }

    /// Ruling R-T4-4: the reported RMS of a TPS frame is measured on
    /// inliers the shipped spline's fit did NOT see, so it is a real
    /// number and `maxRmsPx` / `failOnMaxRms` can actually refuse a TPS
    /// frame.
    ///
    /// Before this ruling the spline interpolated every inlier and
    /// reported ~1e-3 px whatever the field looked like, which made the
    /// QA gate structurally unable to fire on the one distortion model
    /// most able to overfit.
    #[test]
    fn a_tps_frame_reports_a_real_rms_and_the_gate_can_refuse_it() {
        let (subject, reference, geo) = wobble_scene(36, 450);
        let cfg = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            ..Default::default()
        };
        let a = align_default(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(a.distortion, DistortionFit::Tps, "{:?}", a.warnings);
        // Hold-out, not interpolation: a real, non-zero number.
        assert!(
            a.rms_px > 1e-3,
            "an interpolating spline's reported rms must be a hold-out \
             measurement, got {}",
            a.rms_px
        );
        // It is NOT comparable to the polynomial arm's number (that one is
        // in-sample, this one is a hold-out) and this test does not
        // pretend otherwise — the model comparison that means something
        // lives in `the_spline_follows_a_field_the_polynomial_cannot`,
        // against the truth field. What is pinned here is that the number
        // is real and that the gate can act on it.

        // The soft gate warns at the reported number …
        let soft = RegistrationConfig {
            max_rms_px: a.rms_px / 2.0,
            ..cfg.clone()
        };
        let warned = align_default(&subject, &reference, geo, geo, &soft).unwrap();
        assert!(
            warned.warnings.iter().any(|w| w.starts_with("RMS ")),
            "{:?}",
            warned.warnings
        );
        // … and the hard gate refuses the frame outright.
        let hard = RegistrationConfig {
            max_rms_px: a.rms_px / 2.0,
            fail_on_max_rms: true,
            ..cfg.clone()
        };
        match align_default(&subject, &reference, geo, geo, &hard) {
            Err(AlignError::RmsTooHigh { rms_px, max_rms_px }) => {
                assert!(rms_px > max_rms_px, "{rms_px} vs {max_rms_px}");
            }
            other => panic!("expected the RMS gate to refuse the frame, got {other:?}"),
        }
        // A generous limit still passes, so the gate is not simply broken.
        let ok = RegistrationConfig {
            max_rms_px: a.rms_px * 4.0,
            fail_on_max_rms: true,
            ..cfg
        };
        assert!(align_default(&subject, &reference, geo, geo, &ok).is_ok());
    }

    /// Found in fix round 1, and a real defect rather than a test
    /// artefact: two subject stars can pair to ONE reference star
    /// (`pair_through` gives each subject star its own nearest reference
    /// star, independently), RANSAC and the refit keep both, and the
    /// inverse spline's node set then carries that reference position
    /// twice — Bookstein's system is exactly singular and the whole
    /// spline fails. Before [`dedupe_nodes`] it happened on roughly one
    /// synthetic seed in three (seeds 36 and 37 below), and the frame
    /// silently fell back to its linear model with a "did not fit" note.
    #[test]
    fn coincident_nodes_do_not_kill_the_spline() {
        // A scene with a deliberate collision: two subject stars sitting
        // on top of each other pair to the same reference star.
        let (mut subject, reference, geo) = wobble_scene(38, 200);
        let twin = subject[7];
        subject.push(Star {
            x: twin.x + 0.01,
            y: twin.y - 0.01,
            flux: twin.flux * 0.9,
            sigma: twin.sigma,
        });
        let cfg = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            ..Default::default()
        };
        let a = align_default(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(
            a.distortion,
            DistortionFit::Tps,
            "a coincident pair must not sink the fit: {:?}",
            a.warnings
        );

        // The rule itself: an exact duplicate in either direction is
        // dropped, a pair 1 px away is kept.
        let linear = Linear::identity();
        let pairs: Vec<Pair> = vec![
            ((0.0, 0.0), (0.0, 0.0)),
            ((100.0, 0.0), (100.0, 0.0)),
            // same subject position → same forward node
            ((100.0, 0.0), (400.0, 400.0)),
            // same reference position → same inverse node
            ((0.0, 200.0), (0.0, 0.0)),
            // 1 px away from the first: distinct, kept
            ((1.0, 0.0), (1.0, 0.0)),
        ];
        let (kept, dropped) = dedupe_nodes(&linear, &pairs, (0..pairs.len()).collect());
        assert_eq!(kept, vec![0, 1, 4], "{kept:?}");
        assert_eq!(dropped.len(), 2);
        assert!(dropped.contains(&2) && dropped.contains(&3));
        assert!(TPS_MIN_NODE_SEPARATION_PX < 0.1, "safely below a centroid");
    }

    /// Ruling R-T4-4's node-cap branch: with more inliers than
    /// [`TPS_MAX_NODES`], the inliers that did NOT become nodes are the
    /// hold-out set and the SHIPPED model is what gets measured on them —
    /// no second fit needed.
    #[test]
    fn over_the_node_cap_the_non_node_inliers_are_the_holdout() {
        let (subject, reference, geo) = wobble_scene(37, TPS_MAX_NODES + 400);
        let cfg = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            max_stars: 4000,
            ..Default::default()
        };
        let a = align_default(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(a.distortion, DistortionFit::Tps);
        assert!(
            a.inliers > TPS_MAX_NODES,
            "the cap must actually bite: {} inliers",
            a.inliers
        );
        assert!(
            a.rms_px > 1e-3,
            "the non-node inliers are a real hold-out: rms {}",
            a.rms_px
        );
    }

    /// M4c Step 3(c): a local model fitted on a handful of stars says
    /// nothing about the rest of the frame, so below
    /// [`TPS_MIN_INLIERS`] the linear model is kept — with the reason
    /// said out loud, never silently.
    #[test]
    fn a_spline_needs_enough_inliers_or_the_linear_model_is_kept() {
        // 20 stars is above MIN_INLIERS (8) and below TPS_MIN_INLIERS (32).
        let subject = field(33, 20, W, H);
        let truth = similarity(1.0, 0.4, 5.0, -4.0);
        let (subject, reference) = scene(&subject, &truth, 0.02, 0, W, H, 34);
        let geo = (W as usize, H as usize);
        let cfg = RegistrationConfig {
            distortion: DistortionChoice::Tps,
            ..Default::default()
        };
        let a = align_default(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(a.distortion, DistortionFit::None);
        assert!(a.inliers < TPS_MIN_INLIERS, "{} inliers", a.inliers);
        assert!(
            a.warnings.iter().any(|w| w.starts_with("tps distortion needs")
                && w.ends_with("; linear model kept")),
            "{:?}",
            a.warnings
        );
        let name = model_name(a.model, a.distortion, a.seed);
        assert!(!name.contains('+'), "no distortion suffix, got {name}");
        assert_eq!(TPS_MIN_INLIERS, 4 * MIN_INLIERS);
    }

    /// The compose-and-refit half of a local distortion round, driven
    /// directly (ruling R-M4c-7): a corrector homography composed into
    /// the linear part, the distortion refitted around the result, and
    /// the map that comes back better than the one the corrector was
    /// measured against.
    ///
    /// The scene is the loop's own worst case made explicit — a linear
    /// model that is WRONG by a 4 px shear across the frame, which is
    /// what a first map fitted on a partial pairing looks like.
    #[test]
    fn build_map_refits_the_distortion_around_a_corrected_linear_model() {
        let (subject, reference, geo) = wobble_scene(35, 450);
        // Pair the truth up by nearest neighbour so the test owns a
        // clean correspondence list without going through `align`.
        let ref_pts: Vec<(f64, f64)> = reference.iter().map(|s| (s.x, s.y)).collect();
        let tree = KdTree2::build(&ref_pts);
        let truth = similarity(1.0, 0.7, 3.0, -2.0);
        let pairs: Vec<Pair> = subject
            .iter()
            .filter_map(|s| {
                let (px, py) = truth.apply(s.x, s.y);
                tree.nearest_within(px, py, 3.0)
                    .map(|(j, _)| ((s.x, s.y), ref_pts[j]))
            })
            .collect();
        assert!(pairs.len() > 400, "{} pairs", pairs.len());

        // A deliberately wrong linear model: the truth with a shear that
        // reaches 4 px at the frame's edge.
        let mut wrong = truth;
        wrong.m[0][1] += 0.005;
        let first = build_map(DistortionFit::Tps, wrong, &pairs, None, geo, 0.0).unwrap();
        assert_eq!(first.fit, DistortionFit::Tps);
        assert!(first.notes.is_empty(), "{:?}", first.notes);
        // Ruling R-T4-4: every inlier is a node here, so the hold-out
        // fit produced the reported residuals and they are NOT zero.
        let qa = first.qa.expect("a hold-out QA triple");
        assert!(qa.0 > 1e-6, "hold-out rms {} must be a real number", qa.0);
        let before = first.map;

        // The corrector the loop would fit on what that map still gets
        // wrong, composed back in — and the distortion refitted.
        let residual: Vec<Pair> = pairs
            .iter()
            .map(|&((sx, sy), r)| (before.forward_exact(sx, sy), r))
            .collect();
        let mut rc = RansacConfig::new(LinearKind::Homography, geo.0 as f64, geo.1 as f64);
        rc.tolerance_px = 1.9;
        rc.min_inliers = MIN_INLIERS;
        let corrector = ransac_fit(&residual, &rc).expect("a corrector fits");
        let composed = compose(&corrector.linear, &wrong);
        let second = build_map(DistortionFit::Tps, composed, &pairs, None, geo, 0.0).unwrap();
        assert_eq!(second.fit, DistortionFit::Tps);
        let after = second.map;

        // Both maps interpolate their own nodes, so the comparison that
        // means anything is OFF them: how well the map lands a subject
        // pixel that was never a node.
        let off = |m: &PixelMap| -> f64 {
            let mut rng = SplitMix64(8888);
            let mut sum = 0.0;
            for _ in 0..300 {
                let (x, y) = (
                    80.0 + rng.next_f64() * (W - 160.0),
                    80.0 + rng.next_f64() * (H - 160.0),
                );
                let (tx, ty) = truth.apply(x, y);
                let (tx, ty) = (
                    tx + 1.5 * (tx / 120.0).sin() * (ty / 100.0).cos(),
                    ty + 1.5 * (tx / 120.0).cos() * (ty / 100.0).sin(),
                );
                let (fx, fy) = m.forward_exact(x, y);
                sum += (fx - tx).powi(2) + (fy - ty).powi(2);
            }
            (sum / 300.0).sqrt()
        };
        let (b, a) = (off(&before), off(&after));
        // Measured: 0.0047574 px before, 0.0047574 px after — identical
        // to seven digits, and that is the finding, not a weak pin. An
        // INTERPOLATING spline over every pair absorbs whatever linear
        // error it is handed, shear included, so correcting the linear
        // part underneath it cannot move the map. It is also the
        // structural reason the end-to-end loop's corrector comes out at
        // the identity on the TPS arm (see
        // `the_local_distortion_loop_is_bounded_and_never_worse`). The
        // slack is a float floor, six orders below anything registration
        // measures.
        assert!(
            a <= b + 1e-6,
            "the corrected map must not be worse off its nodes: {a} vs {b}"
        );

        // The other two plans go through the same function, and the
        // linear-only plan never refuses. Neither of them carries a
        // hold-out QA triple: they do not interpolate, so the map's own
        // residuals ARE the honest number (ruling R-T4-4).
        let poly =
            build_map(DistortionFit::Polynomial(3), composed, &pairs, None, geo, 0.0).unwrap();
        assert_eq!(poly.fit, DistortionFit::Polynomial(3));
        assert_ne!(
            poly.linear.m, composed.m,
            "the joint polynomial fit rebalances the linear part"
        );
        assert!(poly.map.distortion.is_some() && poly.qa.is_none());
        let plain = build_map(DistortionFit::None, composed, &pairs, None, geo, 0.0).unwrap();
        assert_eq!(plain.fit, DistortionFit::None);
        assert!(plain.map.distortion.is_none() && plain.notes.is_empty() && plain.qa.is_none());
        assert_eq!(plain.linear.m, composed.m);
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
            SeedPolicy::QuadFirst,
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
            SeedPolicy::QuadFirst,
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
            SeedPolicy::WcsFirst,
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
            model_name(a.model, a.distortion, a.seed),
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
            SeedPolicy::WcsFirst,
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
            seed_radius_px(cfg.ransac_tolerance_px);
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

        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &cfg,
            Some(&stale),
            SeedPolicy::WcsFirst,
            gate,
        )
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
            SeedPolicy::WcsFirst,
            SCALE_RANGE,
        )
        .expect_err("nothing can align these");
        assert!(
            format!("{err}").contains("plate-solve seed"),
            "the reason must name the discarded seed: {err}"
        );
    }

    // ── R-T6-9: the plate-solve seed as the quad matcher's fallback ──────

    const DISJOINT_W: f64 = 2000.0;
    const DISJOINT_H: f64 = 1500.0;

    /// A subject and a reference that DO correspond but share almost none
    /// of their stars — the shape of a narrowband frame against a
    /// differently-filtered reference of the same rig (set 195, run 16:
    /// 14 of 30 H-alpha frames lost to "0 inliers" against an O-filter
    /// reference). The ten stars the two have in common sit far apart,
    /// each surrounded by neighbours the other frame does not have, so no
    /// LOCAL group of four is common to both and the quad matcher has
    /// nothing to seed from; an exact transform pairs all ten at once.
    fn disjoint_population_scene() -> (Vec<Star>, Vec<Star>, Linear) {
        let sites: Vec<(f64, f64)> = (0..5)
            .flat_map(|i| (0..2).map(move |j| (i, j)))
            .map(|(i, j)| (300.0 + i as f64 * 340.0, 400.0 + j as f64 * 600.0))
            .collect();
        disjoint_scene_at(&sites)
    }

    /// [`disjoint_population_scene`] with the shared stars at `sites`.
    fn disjoint_scene_at(sites: &[(f64, f64)]) -> (Vec<Star>, Vec<Star>, Linear) {
        let truth = similarity(1.0, 2.0, 9.0, -6.0);
        let mut rng = SplitMix64(71);
        let shared: Vec<Star> = sites
            .iter()
            .map(|&(x, y)| Star {
                x,
                y,
                flux: 500.0 + rng.next_f64() * 500.0,
                sigma: Some((0.05, 0.05)),
            })
            .collect();

        let mut subject = shared.clone();
        subject.extend(field(72, 250, DISJOINT_W, DISJOINT_H));
        let mut reference: Vec<Star> = shared
            .iter()
            .map(|s| {
                let (x, y) = truth.apply(s.x, s.y);
                Star {
                    x,
                    y,
                    flux: s.flux,
                    sigma: s.sigma,
                }
            })
            .collect();
        reference.extend(field(73, 250, DISJOINT_W, DISJOINT_H));

        for v in [&mut subject, &mut reference] {
            for i in (1..v.len()).rev() {
                let j = rng.below(i + 1);
                v.swap(i, j);
            }
        }
        (subject, reference, truth)
    }

    fn disjoint_geometry() -> ((usize, usize), (usize, usize)) {
        let g = (DISJOINT_W as usize, DISJOINT_H as usize);
        (g, g)
    }

    /// (a) Ruling R-T6-9: under `QuadFirst` — every same-scale frame — a
    /// quad seed that cannot carry the frame is not the end of it. The
    /// plate-solve seed gets one turn and says so.
    ///
    /// RG2: the shared stars here are sixteen, one in each cell of the grid
    /// over the overlap. With ten of them in half of the cells the coverage
    /// gate refuses the plate-solve map (see
    /// `a_plate_solve_map_resting_on_ten_shared_stars_is_refused`).
    #[test]
    fn the_plate_solve_seed_is_the_quad_seeds_fallback() {
        let sites: Vec<(f64, f64)> = (0..4)
            .flat_map(|i| (0..4).map(move |j| (i, j)))
            .map(|(i, j)| (265.0 + i as f64 * 490.0, 203.0 + j as f64 * 365.0))
            .collect();
        let (sub, refs, truth) = disjoint_scene_at(&sites);
        let (ref_geo, sub_geo) = disjoint_geometry();
        let cfg = RegistrationConfig::default();

        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &cfg,
            Some(&truth),
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        )
        .expect("the plate-solve seed carries the frame");
        assert_eq!(a.seed, SeedKind::Wcs);
        assert_eq!(a.seed_matches, 0, "a WCS seed pairs no quads");
        assert!(
            a.warnings
                .iter()
                .any(|w| w.contains("plate-solve seed used")),
            "{:?}",
            a.warnings
        );
        // The linear kind is whatever the sixteen inliers resolve to; what this
        // pins is the suffix — the row must name the seed that shipped it.
        assert!(
            model_name(a.model, a.distortion, a.seed).ends_with("+wcs"),
            "{}",
            model_name(a.model, a.distortion, a.seed)
        );
        assert!(a.rms_px < 0.1, "rms {}", a.rms_px);
    }

    /// (b) The same fixture with no hint: the fallback needs a solve, so
    /// the frame fails exactly as it did before M4b.
    #[test]
    fn without_a_hint_the_same_frame_fails_as_it_always_did() {
        let (sub, refs, _) = disjoint_population_scene();
        let (ref_geo, sub_geo) = disjoint_geometry();
        let err = align_default(&sub, &refs, ref_geo, sub_geo, &RegistrationConfig::default())
            .expect_err("the quad matcher has nothing to work with");
        assert!(
            matches!(
                err,
                AlignError::NoSeed {
                    after_wcs: false,
                    ..
                } | AlignError::TooFewMatches { .. }
                    | AlignError::TooFewInliers { .. }
            ),
            "an M1-era failure, not a seed one: {err}"
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

    // ---------------------------------------------------------------
    // The robust quad seed (RQ-1, RQ-2, PD-39) and the coverage index
    // (RQ-10). The quad-level tests hand `robust_quad_seed` synthetic
    // `Quad`s with only their centres filled in: the seed reads nothing
    // else of a quad.
    // ---------------------------------------------------------------

    /// runB's two cameras (research (c)): a 6224 x 4168 subject frame onto a
    /// 6248 x 4176 reference.
    const SEED_SUBJECT: (f64, f64) = (6224.0, 4168.0);
    const SEED_REFERENCE: (f64, f64) = (6248.0, 4176.0);

    /// Scale 0.99227, turned 180 degrees, the subject frame centred on the
    /// reference frame.
    fn seed_truth() -> Linear {
        let (ax, ay) =
            similarity(0.99227, 180.0, 0.0, 0.0).apply(SEED_SUBJECT.0 / 2.0, SEED_SUBJECT.1 / 2.0);
        similarity(
            0.99227,
            180.0,
            SEED_REFERENCE.0 / 2.0 - ax,
            SEED_REFERENCE.1 / 2.0 - ay,
        )
    }

    /// One match as its two quad centres: (image, catalog).
    type CentrePair = ((f64, f64), (f64, f64));

    fn centre_quad(center: (f64, f64)) -> Quad {
        Quad {
            star_idx: [0, 1, 2, 3],
            ratios: [0.0; 5],
            longest: 1.0,
            center,
        }
    }

    /// The two quad lists and `matches[i] = (i, i)` of a list of centre pairs.
    fn quad_level(pairs: &[CentrePair]) -> (Vec<Quad>, Vec<Quad>, Vec<(usize, usize)>) {
        (
            pairs.iter().map(|p| centre_quad(p.0)).collect(),
            pairs.iter().map(|p| centre_quad(p.1)).collect(),
            (0..pairs.len()).map(|i| (i, i)).collect(),
        )
    }

    /// The image centre `p` with its catalog centre: the truth's image of
    /// `p`, moved by `shift`, jittered by up to 0.10 px per axis.
    fn jittered_match(
        truth: &Linear,
        p: (f64, f64),
        shift: (f64, f64),
        rng: &mut SplitMix64,
    ) -> CentrePair {
        let (x, y) = truth.apply(p.0, p.1);
        let jx = (rng.next_f64() - 0.5) * 0.2;
        let jy = (rng.next_f64() - 0.5) * 0.2;
        (p, (x + shift.0 + jx, y + shift.1 + jy))
    }

    /// `n` true matches: image centres uniform over the subject frame.
    fn true_matches(seed: u64, n: usize) -> Vec<CentrePair> {
        let truth = seed_truth();
        let mut rng = SplitMix64(seed);
        (0..n)
            .map(|_| {
                let p = (
                    rng.next_f64() * SEED_SUBJECT.0,
                    rng.next_f64() * SEED_SUBJECT.1,
                );
                jittered_match(&truth, p, (0.0, 0.0), &mut rng)
            })
            .collect()
    }

    /// Fisher-Yates over `(pair, is_true)`, then split.
    fn shuffled_with_flags(
        mut all: Vec<(CentrePair, bool)>,
        rng: &mut SplitMix64,
    ) -> (Vec<CentrePair>, Vec<bool>) {
        for i in (1..all.len()).rev() {
            let j = rng.below(i + 1);
            all.swap(i, j);
        }
        all.into_iter().unzip()
    }

    /// Largest distance, over a 33 x 23 grid on the subject frame, between
    /// a seed's image of a point and the truth's.
    fn grid_error(a: &Affine, truth: &Linear) -> f64 {
        let mut worst = 0.0f64;
        for j in 0..23 {
            for i in 0..33 {
                let x = SEED_SUBJECT.0 * i as f64 / 32.0;
                let y = SEED_SUBJECT.1 * j as f64 / 22.0;
                let (fx, fy) = a.apply(x, y);
                let (tx, ty) = truth.apply(x, y);
                worst = worst.max((fx - tx).hypot(fy - ty));
            }
        }
        worst
    }

    fn bits(a: &Affine) -> [u64; 6] {
        [a.a1, a.b1, a.c1, a.a2, a.b2, a.c2].map(f64::to_bits)
    }

    /// The numpy median of a copy of `values`.
    fn median_of(values: &[f64]) -> f64 {
        let mut v = values.to_vec();
        v.sort_by(|a, b| a.total_cmp(b));
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    }

    #[test]
    fn a_clean_quad_match_set_keeps_the_single_fit_bit_for_bit() {
        let mut pairs = true_matches(1, 486);
        pairs[100].1 .0 += 4.9;
        let (image, catalog, matches) = quad_level(&pairs);
        let seed = robust_quad_seed(&matches, &image, &catalog).expect("a clean set seeds");
        assert_eq!(seed.route, SeedRoute::Clean);
        assert_eq!(seed.dropped(), 0);
        assert_eq!(seed.hypotheses, 0);
        let plain = fit_affine(&matches, &image, &catalog).unwrap();
        assert_eq!(bits(&seed.affine), bits(&plain));
    }

    /// A smooth radial term makes the plain fit's largest residual exceed the
    /// 5 px tolerance, so the set takes the RANSAC route, yet no match is
    /// dropped: the consensus fit's median residual equals the plain
    /// median's, so the consensus loop's threshold (5 x the median) re-admits
    /// the corners, and the seed is `fit_affine(all)` bit for bit. Review
    /// r2 measured radial terms of 15 / 20 / 30 px at the corners: Consensus,
    /// dropped 0 on 15 / 15 scenes. Here the term is `k * rho^2` along the
    /// radius (`rho` 1 at the corners) and the affine absorbs 70 % of it, so
    /// the guard (plain worst residual above 5 px and within 5 x the
    /// median) sizes k at 20 / 30 / 40 (k = 15 leaves the plain fit clean).
    /// This is the only test of the converge-to-every-match branch: no
    /// research set took it (0 of 1 005).
    #[test]
    fn a_distorted_clean_set_converges_to_every_match_bit_for_bit() {
        let centre = (SEED_REFERENCE.0 / 2.0, SEED_REFERENCE.1 / 2.0);
        let corner = centre.0.hypot(centre.1);
        for corner_px in [20.0, 30.0, 40.0] {
            for s in 0..5u64 {
                let pairs: Vec<CentrePair> = true_matches(s, 486)
                    .into_iter()
                    .map(|(p, (x, y))| {
                        let (dx, dy) = (x - centre.0, y - centre.1);
                        let r = dx.hypot(dy);
                        let k = corner_px * (r / corner).powi(2) / r;
                        (p, (x + k * dx, y + k * dy))
                    })
                    .collect();
                let (image, catalog, matches) = quad_level(&pairs);
                let plain = fit_affine(&matches, &image, &catalog).unwrap();
                let r = quad_residuals(&plain, &matches, &image, &catalog);
                let worst = r.iter().cloned().fold(0.0, f64::max);
                assert!(
                    worst > SEED_TOL_PX && worst <= SEED_LO_MEDIAN_FACTOR * median_of(&r),
                    "guard: corner {corner_px} px seed {s}: plain worst {worst}, median {}",
                    median_of(&r)
                );
                let seed = robust_quad_seed(&matches, &image, &catalog).unwrap();
                assert_eq!(
                    seed.route,
                    SeedRoute::Consensus,
                    "corner {corner_px} seed {s}"
                );
                assert!(
                    seed.fits >= 2,
                    "corner {corner_px} seed {s}: the loop ran {} fit(s), so it re-admitted nothing",
                    seed.fits
                );
                assert_eq!(seed.dropped(), 0, "corner {corner_px} seed {s}");
                assert_eq!(
                    bits(&seed.affine),
                    bits(&plain),
                    "corner {corner_px} seed {s}"
                );
            }
        }
    }

    #[test]
    fn two_far_false_quad_matches_are_dropped_and_the_seed_is_the_true_fit() {
        let truth = seed_truth();
        let mut rng = SplitMix64(77);
        let mut pairs = true_matches(2, 486);
        pairs.push(jittered_match(
            &truth,
            (600.0, 600.0),
            (-2405.0, 0.0),
            &mut rng,
        ));
        pairs.push(jittered_match(
            &truth,
            (900.0, 3500.0),
            (-4298.0, 0.0),
            &mut rng,
        ));
        let (image, catalog, matches) = quad_level(&pairs);

        let pulled = fit_affine(&matches, &image, &catalog).expect("the plain fit exists");
        assert!(
            grid_error(&pulled, &truth) >= 10.0,
            "guard: the plain fit over all 488 is {} px off",
            grid_error(&pulled, &truth)
        );

        let seed = robust_quad_seed(&matches, &image, &catalog).unwrap();
        assert_eq!(seed.route, SeedRoute::Consensus);
        assert_eq!(seed.dropped(), 2);
        assert!(seed.kept[..486].iter().all(|k| *k));
        assert!(!seed.kept[486] && !seed.kept[487]);
        let true_fit = fit_affine(&matches[..486], &image, &catalog).unwrap();
        assert_eq!(bits(&seed.affine), bits(&true_fit));
        assert!(grid_error(&seed.affine, &truth) <= 0.5);

        // Only the 2 405 px match.
        let (image, catalog, matches) = quad_level(&pairs[..487]);
        let seed = robust_quad_seed(&matches, &image, &catalog).unwrap();
        assert_eq!(seed.route, SeedRoute::Consensus);
        assert_eq!(seed.dropped(), 1);
        assert!(!seed.kept[486]);
        let true_fit = fit_affine(&matches[..486], &image, &catalog).unwrap();
        assert_eq!(bits(&seed.affine), bits(&true_fit));
        assert!(grid_error(&seed.affine, &truth) <= 0.5);
    }

    #[test]
    fn a_small_match_set_keeps_the_single_fit() {
        let truth = seed_truth();
        let mut rng = SplitMix64(5);
        let mut pairs = true_matches(3, 6);
        // The false match sits at the centroid of the true ones: it has no
        // leverage on the slopes, so the fit stays inside the xy-scale gate
        // and the plain fit exists to compare with.
        let centroid = (
            pairs.iter().map(|p| p.0 .0).sum::<f64>() / 6.0,
            pairs.iter().map(|p| p.0 .1).sum::<f64>() / 6.0,
        );
        pairs.push(jittered_match(&truth, centroid, (-2405.0, 0.0), &mut rng));
        let (image, catalog, matches) = quad_level(&pairs);
        let plain = fit_affine(&matches, &image, &catalog).expect("guard: the plain fit exists");
        let seed = robust_quad_seed(&matches, &image, &catalog).unwrap();
        assert_eq!(seed.route, SeedRoute::Single);
        assert_eq!(seed.dropped(), 0);
        assert_eq!(seed.hypotheses, 0);
        assert_eq!(bits(&seed.affine), bits(&plain));
    }

    #[test]
    fn a_random_false_majority_is_recovered() {
        let truth = seed_truth();
        let mut plain_none = 0;
        for share in [0.3, 0.5, 0.7, 0.85] {
            let false_n = (486.0 * share / (1.0 - share)) as usize;
            for s in 0..10u64 {
                let mut rng = SplitMix64(1000 + s);
                let mut all: Vec<(CentrePair, bool)> = true_matches(s, 486)
                    .into_iter()
                    .map(|p| (p, true))
                    .collect();
                for _ in 0..false_n {
                    let image = (
                        rng.next_f64() * SEED_SUBJECT.0,
                        rng.next_f64() * SEED_SUBJECT.1,
                    );
                    let catalog = (
                        rng.next_f64() * SEED_REFERENCE.0,
                        rng.next_f64() * SEED_REFERENCE.1,
                    );
                    all.push(((image, catalog), false));
                }
                let (pairs, is_true) = shuffled_with_flags(all, &mut rng);
                let (image, catalog, matches) = quad_level(&pairs);
                if fit_affine(&matches, &image, &catalog).is_none() {
                    plain_none += 1;
                }
                let seed = robust_quad_seed(&matches, &image, &catalog)
                    .unwrap_or_else(|| panic!("share {share} seed {s}: no seed"));
                assert_eq!(seed.route, SeedRoute::Consensus, "share {share} seed {s}");
                assert!(
                    grid_error(&seed.affine, &truth) <= 0.5,
                    "share {share} seed {s}: {} px off",
                    grid_error(&seed.affine, &truth)
                );
                for (i, t) in is_true.iter().enumerate() {
                    assert!(
                        !*t || seed.kept[i],
                        "share {share} seed {s}: true match {i} lost"
                    );
                }
            }
        }
        assert!(
            plain_none > 0,
            "guard: the scenes must include sets whose plain fit is None (RQ-2)"
        );
    }

    #[test]
    fn a_consistent_false_minority_is_recovered() {
        let truth = seed_truth();
        for share in [0.1, 0.3] {
            let false_n = (486.0 * share / (1.0 - share)) as usize;
            for s in 0..5u64 {
                let mut rng = SplitMix64(2000 + s);
                let mut all: Vec<(CentrePair, bool)> = true_matches(s, 486)
                    .into_iter()
                    .map(|p| (p, true))
                    .collect();
                for _ in 0..false_n {
                    let p = (
                        rng.next_f64() * SEED_SUBJECT.0,
                        rng.next_f64() * SEED_SUBJECT.1,
                    );
                    all.push((jittered_match(&truth, p, (-2405.0, 0.0), &mut rng), false));
                }
                let (pairs, _) = shuffled_with_flags(all, &mut rng);
                let (image, catalog, matches) = quad_level(&pairs);
                let seed = robust_quad_seed(&matches, &image, &catalog)
                    .unwrap_or_else(|| panic!("share {share} seed {s}: no seed"));
                assert!(
                    grid_error(&seed.affine, &truth) <= 0.5,
                    "share {share} seed {s}: {} px off",
                    grid_error(&seed.affine, &truth)
                );
            }
        }
    }

    #[test]
    fn the_robust_seed_is_deterministic() {
        let mut rng = SplitMix64(4242);
        let mut all: Vec<(CentrePair, bool)> = true_matches(9, 486)
            .into_iter()
            .map(|p| (p, true))
            .collect();
        for _ in 0..486 {
            let image = (
                rng.next_f64() * SEED_SUBJECT.0,
                rng.next_f64() * SEED_SUBJECT.1,
            );
            let catalog = (
                rng.next_f64() * SEED_REFERENCE.0,
                rng.next_f64() * SEED_REFERENCE.1,
            );
            all.push(((image, catalog), false));
        }
        let (pairs, _) = shuffled_with_flags(all, &mut rng);
        let (image, catalog, matches) = quad_level(&pairs);
        let a = robust_quad_seed(&matches, &image, &catalog).unwrap();
        let b = robust_quad_seed(&matches, &image, &catalog).unwrap();
        assert_eq!(a.kept, b.kept);
        assert_eq!(a.hypotheses, b.hypotheses);
        assert_eq!(a.fits, b.fits);
        assert_eq!(bits(&a.affine), bits(&b.affine));
    }

    /// RQ-10 (a)-(c) on hand-built lists, reference 1600 x 1600 (16 x 16
    /// occupancy cells of 100 px) and the identity map unless said.
    #[test]
    fn coverage_counts_only_stars_that_land_where_the_reference_has_stars() {
        let geo = (1600usize, 1600usize);
        let lattice: Vec<(f64, f64)> = (0..16)
            .flat_map(|j| (0..16).map(move |i| (50.0 + 100.0 * i as f64, 50.0 + 100.0 * j as f64)))
            .collect();
        let identity = similarity(1.0, 0.0, 0.0, 0.0);

        // No eligible star: no cells.
        assert_eq!(coverage(&[], &lattice, &[], &identity, geo), (0.0, 0));

        // A full overlap, every star an inlier: 16 of 16 cells.
        assert_eq!(
            coverage(&lattice, &lattice, &lattice, &identity, geo),
            (1.0, 16)
        );

        // Inliers in 2 of 16 cells (the corner cells): 0.125.
        let two = [lattice[0], lattice[255]];
        assert_eq!(
            coverage(&lattice, &lattice, &two, &identity, geo),
            (0.125, 16)
        );

        // A cell with an eligible star and no inlier counts against: no
        // inlier in the grid's first cell (x, y < 425).
        let all_but_one_cell: Vec<(f64, f64)> = lattice
            .iter()
            .copied()
            .filter(|&(x, y)| !(x < 425.0 && y < 425.0))
            .collect();
        assert_eq!(
            coverage(&lattice, &lattice, &all_but_one_cell, &identity, geo),
            (15.0 / 16.0, 16)
        );

        // A corner overlap: the map sends only the subject's far quadrant
        // into the reference (64 stars); the 4 x 4 grid spans THOSE stars,
        // so it still has 16 cells. Inliers outside the box are ignored.
        let shift = similarity(1.0, 0.0, -800.0, -800.0);
        assert_eq!(
            coverage(&lattice, &lattice, &lattice, &shift, geo),
            (1.0, 16)
        );

        // An inlier outside the eligible box counts for nothing: the lattice
        // stars with x < 800 or y < 800 are outside the box of the 64 stars
        // the shift images into the frame.
        let outside: Vec<(f64, f64)> = lattice
            .iter()
            .copied()
            .filter(|&(x, y)| x < 800.0 || y < 800.0)
            .collect();
        assert_eq!(
            coverage(&lattice, &lattice, &outside, &shift, geo),
            (0.0, 16)
        );

        // A zero-width box is one column: five stars on x = 850.
        let column: Vec<(f64, f64)> = (0..5).map(|k| (850.0, 150.0 + 200.0 * k as f64)).collect();
        assert_eq!(
            coverage(&column, &lattice, &column, &identity, geo),
            (1.0, 4)
        );
        assert_eq!(
            coverage(&column, &lattice, &column[..2], &identity, geo),
            (0.5, 4)
        );

        // Not eligible: a star imaged outside the reference frame, and a
        // star imaged into a reference cell with no reference star. The
        // reference holds three corner stars and one at (50, 850).
        let reference = [(50.0, 50.0), (1550.0, 50.0), (50.0, 1550.0), (50.0, 850.0)];
        let corners = [(50.0, 50.0), (1550.0, 50.0), (50.0, 1550.0)];
        let mut subject = corners.to_vec();
        subject.push((-300.0, 850.0)); // outside the frame (clamped, its cell holds a star)
        subject.push((1550.0, 1550.0)); // inside, but its cell holds no reference star
        assert_eq!(
            coverage(&subject, &reference, &corners, &identity, geo),
            (1.0, 3)
        );
        // With a reference star in that cell the fourth corner is eligible
        // and has no inlier among `corners`.
        let mut reference4 = reference.to_vec();
        reference4.push((1550.0, 1550.0));
        assert_eq!(
            coverage(&subject, &reference4, &corners, &identity, geo),
            (0.75, 4)
        );
    }

    /// The `recovers_a_similarity_with_outliers_and_jitter` scene with no
    /// outliers: its matches are all true, so `seed_affine` is the plain fit.
    #[test]
    fn seed_affine_is_bit_identical_to_the_plain_fit_on_a_clean_scene() {
        let subject = field(1, 300, W, H);
        let truth = similarity(1.002, 3.0, 12.3, -7.7);
        let (sub, refs) = scene(&subject, &truth, 0.05, 0, W, H, 7);
        let sub_pts: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let sub_q = build_quads(&sub_pts, sub_pts.len(), group_size_for(sub_pts.len()));
        let ref_q = build_quads(&ref_pts, ref_pts.len(), group_size_for(ref_pts.len()));
        let matches = match_quads(&sub_q, &ref_q, QUAD_TOLERANCE);
        let robust = robust_quad_seed(&matches, &sub_q, &ref_q).expect("the scene seeds");
        assert_eq!(
            robust.dropped(),
            0,
            "guard: the scene carries no false quad match"
        );
        let plain = fit_affine(&matches, &sub_q, &ref_q).expect("the plain fit exists");

        let (seed, n) = seed_affine(&sub_pts, &ref_pts, false).unwrap();
        assert_eq!(n, matches.len());
        let m = seed.m;
        let got = Affine {
            a1: m[0][0],
            b1: m[0][1],
            c1: m[0][2],
            a2: m[1][0],
            b2: m[1][1],
            c2: m[1][2],
        };
        assert_eq!(bits(&got), bits(&plain));
    }

    // ---- The decoy scene: two asterisms whose translated copies pull a plain quad fit ----

    /// Stars in the subject field (start N; tuned until the true quad
    /// matches land in the measured 400-600 window).
    const DECOY_STARS: usize = 2200;
    const DECOY_SITES: [(f64, f64); 2] = [(600.0, 600.0), (900.0, 3500.0)];
    const DECOY_SHIFTS: [f64; 2] = [-2405.0, -4298.0];
    /// Four stars inside 20 px, a different shape per site.
    const DECOY_OFFSETS: [[(f64, f64); 4]; 2] = [
        [(-9.0, -6.0), (7.5, -8.0), (9.0, 5.0), (-4.0, 9.0)],
        [(-8.0, 7.0), (6.0, 9.5), (9.0, -3.0), (-2.0, -9.0)],
    ];

    /// N subject stars on 6224 x 4168; the reference is their images
    /// (+-0.05 px) with a random 18 % removed and as many reference-only
    /// stars added (overlap 0.82); both lists shuffled. Two subject
    /// asterisms (4 stars inside 20 px) have their true images in the
    /// reference and translated copies of those images at (-2 405, 0) and
    /// (-4 298, 0) px; no other star within 80 px of any of the six
    /// asterisms in either list.
    fn decoy_scene(seed: u64) -> (Vec<Star>, Vec<Star>, Linear) {
        let truth = seed_truth();
        let mut rng = SplitMix64(seed ^ 0xDEC0);
        let near = |a: (f64, f64), b: (f64, f64), r: f64| (a.0 - b.0).hypot(a.1 - b.1) < r;
        let true_sites: Vec<(f64, f64)> = DECOY_SITES
            .iter()
            .map(|&(x, y)| truth.apply(x, y))
            .collect();
        let copy_sites: Vec<(f64, f64)> = true_sites
            .iter()
            .zip(DECOY_SHIFTS)
            .map(|(&(x, y), d)| (x + d, y))
            .collect();
        let reference_sites: Vec<(f64, f64)> =
            true_sites.iter().chain(&copy_sites).copied().collect();

        // The subject field, carved around the asterisms.
        let mut subject: Vec<Star> = field(seed, DECOY_STARS, SEED_SUBJECT.0, SEED_SUBJECT.1)
            .into_iter()
            .filter(|s| DECOY_SITES.iter().all(|&c| !near((s.x, s.y), c, 82.0)))
            .collect();
        let jitter = |rng: &mut SplitMix64| (rng.next_f64() - 0.5) * 0.1;
        let mut reference = Vec::new();
        let mut removed = 0usize;
        for s in &subject {
            let (x, y) = truth.apply(s.x, s.y);
            let (jx, jy) = (jitter(&mut rng), jitter(&mut rng));
            if rng.next_f64() < 0.18 {
                removed += 1;
            } else if reference_sites.iter().all(|&c| !near((x, y), c, 80.0)) {
                reference.push(Star {
                    x: x + jx,
                    y: y + jy,
                    flux: s.flux,
                    sigma: s.sigma,
                });
            }
        }
        // As many reference-only stars as were removed.
        reference.extend(
            field(seed + 1, removed, SEED_REFERENCE.0, SEED_REFERENCE.1)
                .into_iter()
                .filter(|s| reference_sites.iter().all(|&c| !near((s.x, s.y), c, 80.0))),
        );
        // The asterisms: subject stars, their true images and the copies.
        for (n, offsets) in DECOY_OFFSETS.iter().enumerate() {
            let (site, shift) = (DECOY_SITES[n], DECOY_SHIFTS[n]);
            for (k, &(ox, oy)) in offsets.iter().enumerate() {
                let star = Star {
                    x: site.0 + ox,
                    y: site.1 + oy,
                    flux: 500.0,
                    sigma: Some((0.05, 0.05)),
                };
                let (x, y) = truth.apply(star.x, star.y);
                for dx in [0.0, shift] {
                    reference.push(Star {
                        x: x + dx + jitter(&mut rng),
                        y: y + jitter(&mut rng),
                        flux: 500.0 - k as f64,
                        sigma: star.sigma,
                    });
                }
                subject.push(star);
            }
        }
        for v in [&mut subject, &mut reference] {
            for i in (1..v.len()).rev() {
                let j = rng.below(i + 1);
                v.swap(i, j);
            }
        }
        (subject, reference, truth)
    }

    /// The guards of the decoy scene, asserted before any test reads it: the
    /// true quad matches, the far false ones, and a plain fit that the false
    /// ones pull off the field.
    fn assert_decoy_guards(subject: &[Star], reference: &[Star], truth: &Linear) {
        let sub_pts: Vec<(f64, f64)> = subject.iter().map(|s| (s.x, s.y)).collect();
        let ref_pts: Vec<(f64, f64)> = reference.iter().map(|s| (s.x, s.y)).collect();
        let sub_q = build_quads(&sub_pts, sub_pts.len(), group_size_for(sub_pts.len()));
        let ref_q = build_quads(&ref_pts, ref_pts.len(), group_size_for(ref_pts.len()));
        let matches = match_quads(&sub_q, &ref_q, QUAD_TOLERANCE);
        let off_truth: Vec<f64> = matches
            .iter()
            .map(|&(i, c)| {
                let (x, y) = truth.apply(sub_q[i].center.0, sub_q[i].center.1);
                (x - ref_q[c].center.0).hypot(y - ref_q[c].center.1)
            })
            .collect();
        let true_n = off_truth.iter().filter(|d| **d <= 3.0).count();
        let far_n = off_truth.iter().filter(|d| **d > 2000.0).count();
        assert!(
            (400..=600).contains(&true_n),
            "guard: {true_n} true quad matches (tune DECOY_STARS)"
        );
        assert!(far_n >= 2, "guard: {far_n} matches beyond 2 000 px");
        let plain = fit_affine(&matches, &sub_q, &ref_q).expect("guard: the plain fit exists");
        assert!(
            grid_error(&plain, truth) >= 10.0,
            "guard: the plain fit is {} px off",
            grid_error(&plain, truth)
        );
    }

    fn decoy_geometry() -> ((usize, usize), (usize, usize)) {
        (
            (SEED_REFERENCE.0 as usize, SEED_REFERENCE.1 as usize),
            (SEED_SUBJECT.0 as usize, SEED_SUBJECT.1 as usize),
        )
    }

    /// Largest distance, over the 33 x 23 grid on the subject frame, between
    /// an alignment's map and the truth's.
    fn map_error(a: &Alignment, truth: &Linear) -> f64 {
        let mut worst = 0.0f64;
        for j in 0..23 {
            for i in 0..33 {
                let x = SEED_SUBJECT.0 * i as f64 / 32.0;
                let y = SEED_SUBJECT.1 * j as f64 / 22.0;
                let (fx, fy) = a.map.forward(x, y);
                let (tx, ty) = truth.apply(x, y);
                worst = worst.max((fx - tx).hypot(fy - ty));
            }
        }
        worst
    }

    #[test]
    fn two_decoy_asterisms_no_longer_pull_the_seed_off_the_field() {
        let (sub, refs, truth) = decoy_scene(1);
        assert_decoy_guards(&sub, &refs, &truth);
        let (ref_geo, sub_geo) = decoy_geometry();
        let a = align_default(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
        )
        .expect("the field aligns");
        assert_eq!(a.seed, SeedKind::Quads);
        assert!(
            map_error(&a, &truth) <= 0.1,
            "map {} px off",
            map_error(&a, &truth)
        );
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let tree = KdTree2::build(&ref_pts);
        let at_truth = pair_through(&truth, &sub, &refs, &tree, 3.8).pairs.len();
        assert!(
            a.inliers as f64 >= 0.95 * at_truth as f64,
            "inliers {} of {at_truth} at the truth",
            a.inliers
        );
        assert!(
            a.repaired * 100 <= a.pairs,
            "the seed pairs the field: repaired {} of {}",
            a.repaired,
            a.pairs
        );
    }

    /// Research: frame 162 registered right on 4 of 200 single-star
    /// deletions with the plain seed, 200 of 200 with the robust one.
    #[test]
    fn deleting_one_registration_star_never_flips_the_map() {
        let (sub, refs, truth) = decoy_scene(1);
        assert_decoy_guards(&sub, &refs, &truth);
        let (ref_geo, sub_geo) = decoy_geometry();
        let cfg = RegistrationConfig::default();
        let mut run = 0;
        for k in (0..sub.len()).step_by(25) {
            // Never an asterism star.
            if DECOY_SITES
                .iter()
                .any(|&c| (sub[k].x - c.0).hypot(sub[k].y - c.1) < 40.0)
            {
                continue;
            }
            let mut fewer = sub.clone();
            fewer.remove(k);
            let a = align_default(&fewer, &refs, ref_geo, sub_geo, &cfg)
                .unwrap_or_else(|e| panic!("deleting star {k}: {e}"));
            assert_eq!(a.seed, SeedKind::Quads, "deleting star {k}");
            assert!(
                map_error(&a, &truth) <= 0.2,
                "deleting star {k}: map {} px off",
                map_error(&a, &truth)
            );
            assert!(
                a.repaired * 100 <= a.pairs,
                "deleting star {k}: repaired {} of {}",
                a.repaired,
                a.pairs
            );
            run += 1;
        }
        assert!(run >= 20, "only {run} deletions ran");
    }

    // ---------------------------------------------------------------
    // The coverage gate (RQ-10, RG2).
    // ---------------------------------------------------------------

    /// Largest distance between an alignment's map and the truth's over a
    /// 33 x 23 grid on `frame`, counting only the points whose truth image
    /// satisfies `keep`. Panics when too few points qualify, so a region
    /// that came out empty cannot pass for exact.
    fn map_error_in(
        a: &Alignment,
        truth: &Linear,
        frame: (f64, f64),
        keep: &dyn Fn(f64, f64) -> bool,
    ) -> f64 {
        let mut worst = 0.0f64;
        let mut counted = 0usize;
        for j in 0..23 {
            for i in 0..33 {
                let x = frame.0 * i as f64 / 32.0;
                let y = frame.1 * j as f64 / 22.0;
                let (tx, ty) = truth.apply(x, y);
                if !keep(tx, ty) {
                    continue;
                }
                let (fx, fy) = a.map.forward(x, y);
                worst = worst.max((fx - tx).hypot(fy - ty));
                counted += 1;
            }
        }
        assert!(counted >= 20, "only {counted} grid points in the region");
        worst
    }

    /// `fit_from_seed` at the default configuration, the way `align` calls it.
    fn gate_run(
        sub: &[Star],
        refs: &[Star],
        seed: &Linear,
        geo: (usize, usize),
    ) -> (Pairing, Result<SeedFit, AlignError>, Vec<String>) {
        let cfg = RegistrationConfig::default();
        let sub_pts: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let tree = KdTree2::build(&ref_pts);
        fit_from_seed(
            seed,
            sub,
            refs,
            &sub_pts,
            &ref_pts,
            &tree,
            2.0 * cfg.ransac_tolerance_px,
            &cfg,
            geo,
        )
    }

    /// `truth` after a scale error `(ex, ey)` about the subject-frame point
    /// `c`: right where `ex * |x - c.0|` and `ey * |y - c.1|` stay inside the
    /// pairing radius, wrong everywhere else.
    fn band_seed(truth: &Linear, c: (f64, f64), e: (f64, f64)) -> Linear {
        let error = Linear::from_flat(
            LinearKind::Affine,
            [1.0 + e.0, 0.0, -e.0 * c.0, 0.0, 1.0 + e.1, -e.1 * c.1, 0.0, 0.0, 1.0],
        );
        compose(truth, &error)
    }

    /// The decoy scene's band seed: a scale error about the subject frame's
    /// first corner, so the 3.8 px pairing holds only near it. At 1e-2 it
    /// pairs 17 of the 1 793 stars the truth pairs, those within about
    /// 380 px of the corner, and the fit on them covers 0.19 of the overlap
    /// after the 4b re-pairing (refused from 5e-3 to 1.2e-2 but for 7e-3,
    /// which recovers). The plan's first value, 2e-3, pairs 237 stars over
    /// a quarter disc of about 1 900 px: the fit on them extrapolates to
    /// 0.01 px, the 4b re-pairing recovers the whole field and the gate has
    /// nothing to refuse.
    const BAND_EPS: (f64, f64) = (1e-2, 1e-2);

    #[test]
    fn a_band_supported_map_is_refused_for_low_coverage() {
        let (sub, refs, truth) = decoy_scene(1);
        assert_decoy_guards(&sub, &refs, &truth);
        let (ref_geo, _) = decoy_geometry();
        let seed = band_seed(&truth, (0.0, 0.0), BAND_EPS);
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let tree = KdTree2::build(&ref_pts);
        let at_seed = pair_through(&seed, &sub, &refs, &tree, 3.8).pairs.len();
        let at_truth = pair_through(&truth, &sub, &refs, &tree, 3.8).pairs.len();
        assert!(
            at_seed >= MIN_INLIERS && at_seed * 5 <= at_truth,
            "guard: the seed pairs {at_seed} of the {at_truth} stars the truth pairs"
        );
        match gate_run(&sub, &refs, &seed, ref_geo).1 {
            Err(AlignError::LowCoverage { coverage, cells }) => {
                assert!(coverage < MIN_COVERAGE, "coverage {coverage}");
                assert!(cells > 0);
            }
            Err(other) => panic!("refused for another reason: {other}"),
            Ok((_, refit, _, _, covered)) => panic!(
                "accepted: coverage {covered:.2}, {} inliers",
                refit.inliers.len()
            ),
        }
    }

    /// A reference with a cloud over its right half: the subject stars that
    /// land there have nowhere to pair, and the gate does not count them.
    #[test]
    fn a_reference_with_a_starless_half_refuses_no_subject() {
        let (sub, mut refs, truth) = decoy_scene(1);
        let half = SEED_REFERENCE.0 / 2.0;
        let starred: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        refs.retain(|s| s.x < half);
        let into_cloud = sub
            .iter()
            .filter(|s| truth.apply(s.x, s.y).0 >= half)
            .count();
        assert!(
            into_cloud * 10 >= sub.len() * 4,
            "guard: {into_cloud} of {} subject stars land under the cloud",
            sub.len()
        );
        let (ref_geo, sub_geo) = decoy_geometry();
        // The mechanism: the same inliers (every star imaged into the left
        // half) cover half the overlap when the whole reference is starred
        // and all of it when only the left half is.
        let sub_pts: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
        let left_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let imaged_left: Vec<(f64, f64)> = sub_pts
            .iter()
            .copied()
            .filter(|&(x, y)| truth.apply(x, y).0 < half)
            .collect();
        let (starred_cover, _) = coverage(&sub_pts, &starred, &imaged_left, &truth, ref_geo);
        let (cloud_cover, _) = coverage(&sub_pts, &left_pts, &imaged_left, &truth, ref_geo);
        assert!(
            starred_cover < MIN_COVERAGE && cloud_cover >= MIN_COVERAGE,
            "guard: {starred_cover} with every cell starred, {cloud_cover} under the cloud"
        );
        let a = align_default(&sub, &refs, ref_geo, sub_geo, &RegistrationConfig::default())
            .expect("the left half aligns");
        assert!(a.coverage >= MIN_COVERAGE, "coverage {}", a.coverage);
        let left = map_error_in(&a, &truth, SEED_SUBJECT, &|x, _| x < half);
        assert!(left <= 0.1, "{left} px off truth over the left half");
    }

    #[test]
    fn a_low_coverage_wcs_map_falls_back_to_the_quad_seed() {
        let (sub, refs, truth) = decoy_scene(1);
        assert_decoy_guards(&sub, &refs, &truth);
        let (ref_geo, sub_geo) = decoy_geometry();
        let band = band_seed(&truth, (0.0, 0.0), BAND_EPS);
        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            Some(&band),
            SeedPolicy::WcsFirst,
            SCALE_RANGE,
        )
        .expect("the quad seed carries the frame");
        assert_eq!(a.seed, SeedKind::Quads);
        assert!(a.seed_matches > 0, "the quad seed's own match count");
        assert!(a.coverage >= MIN_COVERAGE, "coverage {}", a.coverage);
        assert!(
            map_error(&a, &truth) <= 0.1,
            "map {} px off",
            map_error(&a, &truth)
        );
        assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
        let covered: f64 = a.warnings[0]
            .strip_prefix("wcs seed pairing failed (map covers ")
            .and_then(|w| w.strip_suffix(" of the overlap); quad seed used"))
            .unwrap_or_else(|| panic!("not the switch warning's shape: {:?}", a.warnings))
            .parse()
            .expect("the coverage prints as a number");
        assert!(covered < MIN_COVERAGE, "{covered}");
    }

    // ---------------------------------------------------------------
    // Task RL (R4-33 (3)): the plate-solve seed's refusal is logged, not
    // dropped with the quad verdict. The logs are read through the crate's
    // scoped capture (`stacking::telemetry::capture_logs`).
    // ---------------------------------------------------------------

    const SEED_REFUSED: &str = "plate-solve seed refused; quad seed tried";
    const SEED_RETRY_FAILED: &str = "plate-solve seed retry failed; quad verdict stands";

    /// Run `f` under the log capture and return its result with the lines
    /// that carry `phrase`.
    fn lines_logged<R>(phrase: &str, f: impl FnOnce() -> R) -> (R, Vec<String>) {
        let (out, logs) = crate::stacking::telemetry::capture_logs(|| {
            // Callsites first hit by a test with no subscriber cache "never".
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        let lines = logs
            .lines()
            .filter(|l| l.contains(phrase))
            .map(str::to_owned)
            .collect();
        (out, lines)
    }

    /// The value after `key=` on one formatted log line, up to the next
    /// field (`seed="wcs"`, `coverage=0.19`, `reason=map covers 0.19 of the
    /// overlap` all read back whole).
    fn log_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let at = line.find(&format!("{key}="))? + key.len() + 1;
        let rest = &line[at..];
        let end = [" seed=", " coverage=", " cells=", " pairs=", " reason=", " error="]
            .iter()
            .filter_map(|k| rest.find(k))
            .min()
            .unwrap_or(rest.len());
        Some(rest[..end].trim())
    }

    /// A refusal the quad seed then carries: the line gives what the
    /// returned warning only gives to two decimals, and `cells` and `pairs`
    /// beside it.
    #[test]
    fn a_refused_plate_solve_seed_logs_its_coverage_cells_and_pairs() {
        let (sub, refs, truth) = decoy_scene(1);
        assert_decoy_guards(&sub, &refs, &truth);
        let (ref_geo, sub_geo) = decoy_geometry();
        let band = band_seed(&truth, (0.0, 0.0), BAND_EPS);
        let (a, lines) = lines_logged(SEED_REFUSED, || {
            align(
                &sub,
                &refs,
                ref_geo,
                sub_geo,
                &RegistrationConfig::default(),
                Some(&band),
                SeedPolicy::WcsFirst,
                SCALE_RANGE,
            )
        });
        let a = a.expect("the quad seed carries the frame");
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(line.contains("WARN"), "{line}");
        assert_eq!(log_field(line, "seed"), Some("\"wcs\""), "{line}");
        let coverage: f64 = log_field(line, "coverage")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no coverage on {line}"));
        assert!(coverage < MIN_COVERAGE, "{line}");
        let cells: usize = log_field(line, "cells")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no cells on {line}"));
        assert!((1..=16).contains(&cells), "{line}");
        let pairs: usize = log_field(line, "pairs")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no pairs on {line}"));
        assert!(pairs >= MIN_INLIERS, "{line}");
        // The warning the frame returns names the same refusal, rounded.
        assert_eq!(
            log_field(line, "reason"),
            Some(format!("map covers {coverage:.2} of the overlap").as_str()),
            "{line}"
        );
        assert_eq!(
            a.warnings[0],
            format!("wcs seed pairing failed (map covers {coverage:.2} of the overlap); quad seed used")
        );
    }

    /// The case the line exists for (set 195's refused frames): the quad
    /// turn fails too, `NoSeed` is all the frame reports, and the refusal's
    /// coverage and cells would be nowhere. Here: ten shared stars hold 8 of
    /// the overlap's 16 cells, the plate-solve map is refused, and the quad
    /// matcher has nothing to match.
    #[test]
    fn a_refusal_followed_by_a_quad_failure_still_logs_the_refusal() {
        let (sub, refs, truth) = disjoint_population_scene();
        let (ref_geo, sub_geo) = disjoint_geometry();
        let (err, lines) = lines_logged(SEED_REFUSED, || {
            align(
                &sub,
                &refs,
                ref_geo,
                sub_geo,
                &RegistrationConfig::default(),
                Some(&truth),
                SeedPolicy::WcsFirst,
                SCALE_RANGE,
            )
        });
        let err = err.expect_err("the quad matcher has nothing to match either");
        assert!(
            matches!(err, AlignError::NoSeed { after_wcs: true, .. }),
            "the frame's verdict is unchanged: {err}"
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        let coverage: f64 = log_field(line, "coverage")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no coverage on {line}"));
        assert!(coverage < MIN_COVERAGE, "{line}");
        assert_eq!(log_field(line, "cells"), Some("16"), "{line}");
        // The ten shared stars, and whatever field star the pairing radius
        // adds to them.
        let pairs: usize = log_field(line, "pairs")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no pairs on {line}"));
        assert!((10..=14).contains(&pairs), "{line}");
    }

    /// A refusal that is not for coverage says how many stars the seed
    /// paired, and carries no coverage or cells.
    #[test]
    fn a_plate_solve_seed_that_pairs_too_little_logs_its_pairs() {
        let (sub, refs, truth) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));
        let mut stale = truth;
        stale.m[0][2] += 5.0;
        let (a, lines) = lines_logged(SEED_REFUSED, || {
            align(
                &sub,
                &refs,
                ref_geo,
                sub_geo,
                &RegistrationConfig::default(),
                Some(&stale),
                SeedPolicy::WcsFirst,
                gate,
            )
        });
        a.expect("the quad seed carries the frame");
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        let pairs: usize = log_field(line, "pairs")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no pairs on {line}"));
        assert!(pairs < MIN_INLIERS, "{line}");
        assert_eq!(
            log_field(line, "reason"),
            Some(format!("{pairs} pairs").as_str()),
            "{line}"
        );
        assert!(
            !line.contains("coverage=") && !line.contains("cells="),
            "{line}"
        );
    }

    /// A frame the plate-solve seed carries logs no refusal.
    #[test]
    fn a_carried_frame_logs_no_refusal() {
        let (sub, refs, truth) = binned_scene();
        let (ref_geo, sub_geo) = binned_geometry();
        let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));
        let ((), lines) = lines_logged(SEED_REFUSED, || {
            align(
                &sub,
                &refs,
                ref_geo,
                sub_geo,
                &RegistrationConfig::default(),
                Some(&truth),
                SeedPolicy::WcsFirst,
                gate,
            )
            .expect("the confirmed hint carries the frame");
        });
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// The quad seed leads and is refused; the hint is confirmed but its
    /// map is refused too. The quad path's verdict stands, as it always
    /// did, and the retry's own error is on the log.
    #[test]
    fn a_failed_plate_solve_retry_is_logged_and_the_quad_verdict_stands() {
        let (sub, refs, truth) = compact_decoy_scene(1, COMPACT_STARS);
        let (ref_geo, sub_geo) = decoy_geometry();
        let band = band_seed(&truth, (0.0, 0.0), BAND_EPS);
        let cfg = RegistrationConfig::default();
        let without = align_default(&sub, &refs, ref_geo, sub_geo, &cfg)
            .expect_err("the compact decoy is refused without a hint");
        let (with, lines) = lines_logged(SEED_RETRY_FAILED, || {
            align(
                &sub,
                &refs,
                ref_geo,
                sub_geo,
                &cfg,
                Some(&band),
                SeedPolicy::QuadFirst,
                SCALE_RANGE,
            )
        });
        let with = with.expect_err("neither seed carries the frame");
        assert_eq!(format!("{with}"), format!("{without}"), "the quad verdict stands");
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(line.contains("WARN"), "{line}");
        assert!(
            log_field(line, "error").is_some_and(|e| !e.is_empty()),
            "{line}"
        );
    }

    /// Stars of a dense cluster in one subject cell of the 4 x 4 grid.
    const COMPACT_STARS: usize = 3000;
    const COMPACT_BOX: ((f64, f64), (f64, f64)) = ((1700.0, 200.0), (2900.0, 1000.0));

    /// The decoy scene plus a dense cluster of subject stars whose images
    /// the reference holds twice: at their true place with the scene's usual
    /// 18 % loss, and as complete copies at (-2 405, 0) px. Its copies'
    /// quads match the cluster's quads whole, so the false quad matches at
    /// that one offset outnumber the true ones.
    fn compact_decoy_scene(seed: u64, stars: usize) -> (Vec<Star>, Vec<Star>, Linear) {
        let (mut subject, mut reference, truth) = decoy_scene(seed);
        let mut rng = SplitMix64(seed ^ 0xC1A5);
        let (lo, hi) = COMPACT_BOX;
        let jitter = |rng: &mut SplitMix64| (rng.next_f64() - 0.5) * 0.1;
        for _ in 0..stars {
            let star = Star {
                x: lo.0 + rng.next_f64() * (hi.0 - lo.0),
                y: lo.1 + rng.next_f64() * (hi.1 - lo.1),
                flux: 200.0 + rng.next_f64() * 600.0,
                sigma: Some((0.05, 0.05)),
            };
            let (x, y) = truth.apply(star.x, star.y);
            if rng.next_f64() >= 0.18 {
                reference.push(Star {
                    x: x + jitter(&mut rng),
                    y: y + jitter(&mut rng),
                    ..star.clone()
                });
            }
            reference.push(Star {
                x: x - 2405.0 + jitter(&mut rng),
                y: y + jitter(&mut rng),
                ..star.clone()
            });
            subject.push(star);
        }
        for v in [&mut subject, &mut reference] {
            for i in (1..v.len()).rev() {
                let j = rng.below(i + 1);
                v.swap(i, j);
            }
        }
        (subject, reference, truth)
    }

    /// Review r2 I3. A dense cluster of subject stars inside one of the 16
    /// cells, present in the reference at one false offset, makes the false
    /// quad matches the larger consistent group: any estimator takes it, and
    /// the map it fits pairs the cluster and nothing else. The gate sees
    /// that the inliers cover a sliver of the overlap and refuses; the frame
    /// has no other seed and fails, never `Ok` on a wrong map.
    ///
    /// The other half of RQ-10, by design: a copy spread over the WHOLE
    /// field has inliers in every cell, and no estimator and no gate can tell
    /// it from the field.
    #[test]
    fn a_compact_decoy_majority_is_refused() {
        let (sub, refs, truth) = compact_decoy_scene(1, COMPACT_STARS);
        let (ref_geo, sub_geo) = decoy_geometry();
        // Guards: the cluster sits in at most 2 of the 16 subject cells, and
        // its false matches outnumber the true ones.
        let cells: std::collections::BTreeSet<(usize, usize)> = sub
            .iter()
            .filter(|s| {
                let (lo, hi) = COMPACT_BOX;
                (lo.0..=hi.0).contains(&s.x) && (lo.1..=hi.1).contains(&s.y)
            })
            .map(|s| {
                (
                    (s.x / (SEED_SUBJECT.0 / 4.0)) as usize,
                    (s.y / (SEED_SUBJECT.1 / 4.0)) as usize,
                )
            })
            .collect();
        assert!(cells.len() <= 2, "guard: the cluster spans {cells:?}");
        let sub_pts: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let sub_q = build_quads(&sub_pts, sub_pts.len(), group_size_for(sub_pts.len()));
        let ref_q = build_quads(&ref_pts, ref_pts.len(), group_size_for(ref_pts.len()));
        let matches = match_quads(&sub_q, &ref_q, QUAD_TOLERANCE);
        let (mut true_n, mut false_n) = (0usize, 0usize);
        for &(i, c) in &matches {
            let (x, y) = truth.apply(sub_q[i].center.0, sub_q[i].center.1);
            let (dx, dy) = (ref_q[c].center.0 - x, ref_q[c].center.1 - y);
            if dx.hypot(dy) <= 3.0 {
                true_n += 1;
            } else if (dx + 2405.0).hypot(dy) <= 3.0 {
                false_n += 1;
            }
        }
        assert!(
            false_n > true_n && true_n >= 300,
            "guard: {false_n} false quad matches against {true_n} true"
        );

        match align_default(&sub, &refs, ref_geo, sub_geo, &RegistrationConfig::default()) {
            Err(AlignError::LowCoverage { coverage, .. }) => {
                assert!(coverage < MIN_COVERAGE, "coverage {coverage}")
            }
            Err(AlignError::TooFewInliers { .. }) => {}
            Err(other) => panic!("refused for another reason: {other}"),
            Ok(a) => assert!(
                map_error(&a, &truth) <= 1.0,
                "accepted a map {} px off truth (coverage {:.2}, {} inliers)",
                map_error(&a, &truth),
                a.coverage,
                a.inliers
            ),
        }
    }

    /// A mosaic step: the reference shows the subject's right 40 % at its
    /// own left 40 %. The cells of the grid over the overlap, not over the
    /// frame, are what the inliers have to cover.
    #[test]
    fn a_partial_overlap_passes_the_gate() {
        let subject = field(51, 700, W, H);
        let truth = similarity(1.0, 0.3, -600.0, 4.0);
        let (sub, refs) = scene(&subject, &truth, 0.05, 100, W, H, 52);
        let in_overlap = refs.len() - 100;
        assert!(
            (200..=300).contains(&in_overlap),
            "guard: {in_overlap} subject stars image into the reference"
        );
        let g = (W as usize, H as usize);
        let a = align_default(&sub, &refs, g, g, &RegistrationConfig::default())
            .expect("the overlap aligns");
        assert!(a.coverage >= MIN_COVERAGE, "coverage {}", a.coverage);
        let err = map_error_in(&a, &truth, (W, H), &|x, y| {
            (0.0..W).contains(&x) && (0.0..H).contains(&y)
        });
        assert!(err <= 0.1, "{err} px off truth over the overlap");
    }

    /// The reference's footprint of a subject at x0.207: a few reference
    /// cells of the 16 x 16 grid, every one of them starred.
    #[test]
    fn a_cross_scale_subject_passes_the_gate() {
        let subject = field(61, 600, SUB_W, SUB_H);
        let truth = centred_similarity(
            0.207,
            4.0,
            (SUB_W / 2.0, SUB_H / 2.0),
            (REF_W / 2.0, REF_H / 2.0),
        );
        let (sub, mut refs) = scene(&subject, &truth, 0.05, 0, REF_W, REF_H, 62);
        assert_eq!((sub.len(), refs.len()), (600, 600), "guard: no star is lost");
        refs.extend(field(63, 150, REF_W, REF_H));
        let (ref_geo, sub_geo) = binned_geometry();
        let gate = super::super::scale_gate_for(Some(0.207), Some(1.0));
        let a = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            Some(&truth),
            SeedPolicy::WcsFirst,
            gate,
        )
        .expect("the confirmed hint carries the frame");
        assert_eq!(a.seed, SeedKind::Wcs);
        assert!(a.coverage >= MIN_COVERAGE, "coverage {}", a.coverage);
        assert!(a.inliers >= 540, "inliers {}", a.inliers);
    }

    /// Review r2 M4. A subject at x4.8 whose frame is about five times the
    /// reference's: the reference sees one subject cell of the 16. A floor on
    /// the subject cells would refuse it; the grid over the overlap gives it
    /// its own 16.
    #[test]
    fn a_wide_subject_on_a_narrow_reference_passes_the_gate() {
        let (ref_w, ref_h) = (2400.0, 1800.0);
        let subject = field(71, 2400, SUB_W, SUB_H);
        let truth = centred_similarity(
            4.8,
            2.0,
            (SUB_W / 2.0, SUB_H / 2.0),
            (ref_w / 2.0, ref_h / 2.0),
        );
        let (sub, refs) = scene(&subject, &truth, 0.05, 0, ref_w, ref_h, 72);
        let sub_pts: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let geo = (ref_w as usize, ref_h as usize);
        let (all_in, cells) = coverage(&sub_pts, &ref_pts, &sub_pts, &truth, geo);
        assert!(
            (100..=250).contains(&refs.len()) && cells == 16 && all_in == 1.0,
            "guard: {} stars in the overlap, {cells} cells",
            refs.len()
        );
        let gate = super::super::scale_gate_for(Some(4.8), Some(1.0));
        let a = align(
            &sub,
            &refs,
            geo,
            (SUB_W as usize, SUB_H as usize),
            &RegistrationConfig::default(),
            Some(&truth),
            SeedPolicy::WcsFirst,
            gate,
        )
        .expect("the confirmed hint carries the frame");
        assert_eq!(a.seed, SeedKind::Wcs);
        assert!(a.coverage >= MIN_COVERAGE, "coverage {}", a.coverage);
        assert!(a.inliers as f64 >= 0.9 * refs.len() as f64, "inliers {}", a.inliers);
    }

    /// Stars in 5 of the 16 cells only, every one paired: the index counts
    /// the cells that can hold an inlier, so it is 1, not 5 / 16.
    #[test]
    fn a_sparse_field_passes_the_gate() {
        let sites = [
            (100.0, 100.0),
            (900.0, 100.0),
            (100.0, 700.0),
            (900.0, 700.0),
            (400.0, 300.0),
        ];
        let mut rng = SplitMix64(81);
        let subject: Vec<Star> = sites
            .iter()
            .flat_map(|&(cx, cy)| (0..8).map(move |k| (cx, cy, k)))
            .map(|(cx, cy, _)| Star {
                x: cx + (rng.next_f64() - 0.5) * 50.0,
                y: cy + (rng.next_f64() - 0.5) * 50.0,
                flux: 100.0 + rng.next_f64() * 900.0,
                sigma: Some((0.05, 0.05)),
            })
            .collect();
        let truth = similarity(1.0, 1.0, 8.0, -5.0);
        let (sub, refs) = scene(&subject, &truth, 0.05, 0, W, H, 82);
        assert_eq!((sub.len(), refs.len()), (40, 40), "guard: no star is lost");
        let sub_pts: Vec<(f64, f64)> = sub.iter().map(|s| (s.x, s.y)).collect();
        let ref_pts: Vec<(f64, f64)> = refs.iter().map(|s| (s.x, s.y)).collect();
        let g = (W as usize, H as usize);
        assert_eq!(
            coverage(&sub_pts, &ref_pts, &sub_pts, &truth, g).1,
            5,
            "guard: the stars hold 5 cells"
        );
        let a = align_default(&sub, &refs, g, g, &RegistrationConfig::default())
            .expect("the clusters align");
        assert_eq!(a.inliers, 40);
        assert_eq!(a.coverage, 1.0);
    }

    /// The `align` scenes that ended `Ok` before the gate, re-run: the same
    /// map bits, pairing, seed and warnings as at the R1 head (recorded then),
    /// and a coverage over the minimum. The disjoint-population scene is not
    /// here: the gate refuses it by design (see
    /// `a_plate_solve_map_resting_on_ten_shared_stars_is_refused`).
    #[test]
    fn every_existing_scene_keeps_its_map_bit_for_bit() {
        struct Pinned {
            name: &'static str,
            hash: u64,
            inliers: usize,
            pairs: usize,
            repaired: usize,
            seed: SeedKind,
            seed_matches: usize,
            warnings: &'static [&'static str],
        }
        let binned = |name, seed, seed_matches, warnings| Pinned {
            name,
            hash: 0x0679c24c918f668f,
            inliers: 300,
            pairs: 300,
            repaired: 0,
            seed,
            seed_matches,
            warnings,
        };
        let pinned = [
            Pinned {
                name: "jitter",
                hash: 0x5d1fb80bd7d6be12,
                inliers: 297,
                pairs: 299,
                repaired: 0,
                seed: SeedKind::Quads,
                seed_matches: 44,
                warnings: &[],
            },
            Pinned {
                name: "mirrored",
                hash: 0x0c501c163664e11a,
                inliers: 250,
                pairs: 250,
                repaired: 0,
                seed: SeedKind::Quads,
                seed_matches: 61,
                warnings: &[],
            },
            Pinned {
                name: "decoy",
                hash: 0x146c7bc43d667978,
                inliers: 1793,
                pairs: 1793,
                repaired: 0,
                seed: SeedKind::Quads,
                seed_matches: 439,
                warnings: &[],
            },
            binned("binned_quads", SeedKind::Quads, 216, &[]),
            binned("binned_hint", SeedKind::Wcs, 0, &[]),
            binned(
                "binned_unconfirmed",
                SeedKind::Quads,
                216,
                &["wcs seed rejected (1 pairs); quad seed used"],
            ),
            binned(
                "binned_stale",
                SeedKind::Quads,
                216,
                &["wcs seed pairing failed (0 pairs); quad seed used"],
            ),
        ];

        let cfg = RegistrationConfig::default();
        let mut got: Vec<(&str, Alignment, (f64, f64))> = Vec::new();
        {
            let subject = field(1, 300, W, H);
            let truth = similarity(1.002, 3.0, 12.3, -7.7);
            let (sub, refs) = scene(&subject, &truth, 0.05, 90, W, H, 7);
            let g = (W as usize, H as usize);
            let a = align_default(&sub, &refs, g, g, &cfg).unwrap();
            got.push(("jitter", a, (W, H)));
        }
        {
            let subject = field(2, 250, W, H);
            let truth = Linear::from_flat(
                LinearKind::Affine,
                [-1.0, 0.0, W - 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            );
            let (sub, refs) = scene(&subject, &truth, 0.05, 60, W, H, 8);
            let g = (W as usize, H as usize);
            let a = align_default(&sub, &refs, g, g, &cfg).unwrap();
            got.push(("mirrored", a, (W, H)));
        }
        {
            let (sub, refs, _) = decoy_scene(1);
            let (rg, sg) = decoy_geometry();
            let a = align_default(&sub, &refs, rg, sg, &cfg).unwrap();
            got.push(("decoy", a, SEED_SUBJECT));
        }
        {
            let (sub, refs, truth) = binned_scene();
            let (rg, sg) = binned_geometry();
            let gate = super::super::scale_gate_for(Some(1.56), Some(0.78));
            let frame = (SUB_W, SUB_H);
            let mut wrong = truth;
            wrong.m[0][2] += 30.0;
            let mut stale = truth;
            stale.m[0][2] += 5.0;
            for (name, hint, policy) in [
                ("binned_quads", None, SeedPolicy::QuadFirst),
                ("binned_hint", Some(&truth), SeedPolicy::WcsFirst),
                ("binned_unconfirmed", Some(&wrong), SeedPolicy::WcsFirst),
                ("binned_stale", Some(&stale), SeedPolicy::WcsFirst),
            ] {
                let a = align(&sub, &refs, rg, sg, &cfg, hint, policy, gate).unwrap();
                got.push((name, a, frame));
            }
        }

        assert_eq!(got.len(), pinned.len());
        for (p, (name, a, frame)) in pinned.iter().zip(&got) {
            assert_eq!(p.name, *name);
            assert_eq!(
                (a.inliers, a.pairs, a.repaired, a.seed, a.seed_matches),
                (p.inliers, p.pairs, p.repaired, p.seed, p.seed_matches),
                "{name}"
            );
            assert_eq!(a.warnings, p.warnings, "{name}");
            assert_eq!(map_fingerprint(a, *frame), p.hash, "{name}: the map moved");
            assert!(a.coverage >= MIN_COVERAGE, "{name}: coverage {}", a.coverage);
        }
    }

    /// FNV-1a over the bits of the alignment's image of a 17 x 13 grid on
    /// the subject frame: equal only when the map is the same to the bit.
    fn map_fingerprint(a: &Alignment, frame: (f64, f64)) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for j in 0..13 {
            for i in 0..17 {
                let (x, y) = (frame.0 * i as f64 / 16.0, frame.1 * j as f64 / 12.0);
                let (fx, fy) = a.map.forward(x, y);
                for v in [fx, fy] {
                    for byte in v.to_bits().to_le_bytes() {
                        h ^= byte as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        h
    }

    /// The ten shared stars of the disjoint-population scene hold 8 of the
    /// 16 cells of the overlap: a plate-solve map that rests on them covers
    /// half of it, and the gate refuses it (RQ-10) where the plate-solve seed
    /// used to carry the frame. `the_plate_solve_seed_is_the_quad_seeds_fallback`
    /// runs the same scene with the shared stars spread over every cell.
    #[test]
    fn a_plate_solve_map_resting_on_ten_shared_stars_is_refused() {
        let (sub, refs, truth) = disjoint_population_scene();
        let (ref_geo, sub_geo) = disjoint_geometry();
        let err = align(
            &sub,
            &refs,
            ref_geo,
            sub_geo,
            &RegistrationConfig::default(),
            Some(&truth),
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        )
        .expect_err("ten stars in half of the cells do not cover the overlap");
        match err {
            AlignError::LowCoverage { coverage, cells } => {
                assert!(coverage < MIN_COVERAGE, "coverage {coverage}");
                assert_eq!(cells, 16);
            }
            other => panic!("refused for another reason: {other}"),
        }
    }

    /// The tie rule of the seed's RANSAC: two disjoint consistent groups of
    /// equal size (243 true matches, 243 at (-2 405, 0) px) reach the same
    /// consensus, and the strict `>` keeps the first hypothesis that got
    /// there. Replays `SplitMix64(SEED_RANSAC_SEED)`'s draws to name that
    /// group, and guards that on some scene the last group to reach the
    /// maximum is the other one (a `>=` would have flipped it).
    #[test]
    fn the_first_of_two_equal_consistent_groups_wins_the_tie() {
        let truth = seed_truth();
        let mut flipped_by_ge = 0;
        for s in 0..8u64 {
            let mut rng = SplitMix64(3000 + s);
            let mut all: Vec<(CentrePair, bool)> = true_matches(s, 243)
                .into_iter()
                .map(|p| (p, true))
                .collect();
            for _ in 0..243 {
                let p = (
                    rng.next_f64() * SEED_SUBJECT.0,
                    rng.next_f64() * SEED_SUBJECT.1,
                );
                all.push((jittered_match(&truth, p, (-2405.0, 0.0), &mut rng), false));
            }
            let (pairs, is_true) = shuffled_with_flags(all, &mut rng);
            let (image, catalog, matches) = quad_level(&pairs);
            let seed = robust_quad_seed(&matches, &image, &catalog).unwrap();
            assert_eq!(seed.route, SeedRoute::Consensus, "scene {s}");

            // Replay the draws; record the group and consensus of every
            // pure sample that gave a hypothesis.
            let n = matches.len();
            let centres: Vec<Pair> = pairs.iter().map(|p| (p.0, p.1)).collect();
            let mut draws = SplitMix64(SEED_RANSAC_SEED);
            let mut hypotheses: Vec<(bool, usize)> = Vec::new();
            for _ in 0..seed.hypotheses {
                let mut picks = [0usize; 3];
                let mut drawn = 0;
                while drawn < 3 {
                    let p = draws.below(n);
                    if !picks[..drawn].contains(&p) {
                        picks[drawn] = p;
                        drawn += 1;
                    }
                }
                let sample = [centres[picks[0]], centres[picks[1]], centres[picks[2]]];
                if degenerate(&sample) {
                    continue;
                }
                let three = [matches[picks[0]], matches[picks[1]], matches[picks[2]]];
                let Some(h) = fit_affine(&three, &image, &catalog) else {
                    continue;
                };
                let group = is_true[picks[0]];
                if is_true[picks[1]] != group || is_true[picks[2]] != group {
                    continue;
                }
                let count = quad_residuals(&h, &matches, &image, &catalog)
                    .iter()
                    .filter(|r| **r <= SEED_TOL_PX)
                    .count();
                hypotheses.push((group, count));
            }
            let top = hypotheses.iter().map(|h| h.1).max().unwrap();
            let at_top: Vec<bool> = hypotheses
                .iter()
                .filter(|h| h.1 == top)
                .map(|h| h.0)
                .collect();
            let first = at_top[0];
            for (i, t) in is_true.iter().enumerate() {
                assert_eq!(
                    seed.kept[i],
                    *t == first,
                    "scene {s}: match {i} (the first group to reach {top} is {})",
                    if first { "the true one" } else { "the false one" }
                );
            }
            // A `>=` keeps the LAST hypothesis to reach the maximum, so the
            // scene discriminates only when that one is the other group.
            if *at_top.last().unwrap() != first {
                flipped_by_ge += 1;
            }
        }
        assert!(
            flipped_by_ge >= 1,
            "guard: no scene ended with the other group last at the top consensus"
        );
    }
}
