//! Relative scale between one target frame and the LN reference (spec
//! §5.2, math §4.3): the RCR location of the matched stars' PSF-flux
//! ratios, `s = RCR_loc(flux_ref,k / flux_tgt,k)`. Task 5 subtracts this
//! into the additive term (`B = B_ref − s·B_tgt`) and later stamps it on
//! the grid as the multiplicative `A`.
//!
//! Detection and PSF fitting run the same detection + fit path on both
//! planes (`detect_seeds`, `DetectionConfig::default()`, `max_stars`,
//! `FitParams::default()`, native units) — except the model: the reference
//! fits with the caller's own `psf` choice, and the target always fits at
//! the reference's OWN resolved β via `psf_signal::fit_stars_with_beta`,
//! never its own independent `Auto` search — which is what makes the
//! ratios comparable (see `relative_scale`'s own doc for why). Both planes
//! are assumed already in the reference geometry (registered), so a
//! shared pixel position means the same sky position. Matching is a
//! nearest-neighbour pass: a [`crate::geometry::kdtree::KdTree2`] built
//! over the reference fits' centroids, queried once per target fit within
//! `match_radius_px`. The spec's "square half-side 4" window and this
//! nearest-within-a-circle query differ only at the corners of that box —
//! close enough that a second shape is not worth the code.
//!
//! **M4c (rulings R-M4c-8/9) added the two pieces M2 recorded as
//! deferred:** a BARYCENTRE second matching pass — when pass 1's pairing
//! covered fewer than [`LN_BARYCENTRE_PASS_THRESHOLD`] of the TARGET's own
//! accepted fits, the same nearest-within query runs again over the
//! DETECTION barycentres each fit came from and the larger of the two
//! pairings wins ([`choose_pairing`]) — and an optional LOCAL SCALE model:
//! with `normalization.local.localScale` on, the surviving ratios'
//! residuals `z_k − s` are fitted with an approximating thin-plate spline
//! ([`fit_local_scale`]) that `ln::normalize_frame` samples on the stride
//! grid as `A(x, y) = s + spline(x, y)`.

use std::collections::HashSet;
use std::f64::consts::{PI, SQRT_2};

use tracing::{debug, warn};

use super::LnError;
use crate::geometry::kdtree::KdTree2;
use crate::geometry::{select_nodes, Pair, ThinPlateSpline, TPS_MAX_NODES, TPS_MIN_NODES};
use crate::stacking::psf_signal::{
    fit_stars, fit_stars_with_beta, FitOutcome, FitParams, PsfModel, Seed, StarFit,
};
use crate::stacking::register::detect::{detect_stars, Star};
use crate::stacking::register::DetectionConfig;

/// Fewer surviving pairs than this and the frame cannot be trusted for
/// local normalization (spec §5.2 ruling: `LnError::TooFewMatches`).
pub const MIN_MATCHES: usize = 20;

/// Ruling R-M4c-9: a pass-1 pairing covering less than this FRACTION of
/// the TARGET plane's own accepted fits triggers the barycentre second
/// pass. `0.8` is math §4.3 step 2's "a second pass using barycentres runs
/// when < 80 % matched"; the denominator is the target's, not the
/// reference's — see [`choose_pairing`] and review finding R-T5-2.
pub const LN_BARYCENTRE_PASS_THRESHOLD: f64 = 0.8;

/// Ruling R-M4c-8: fewer DISTINCT reference stars than this among the
/// pairs surviving RCR and no local scale spline is fitted at all — `A`
/// stays the constant RCR location and the frame is logged as such. A
/// surface fitted on a handful of stars is noise dressed as a flat-field
/// residual. Counted after the reference-index dedupe (review m6), so it
/// counts what the spline is actually fitted on, not how many target fits
/// happened to claim the same few stars.
pub const LN_LOCAL_SCALE_MIN_STARS: usize = 40;

/// Ruling R-M4c-8 / math §4.3 step 5: the local-scale spline's smoothing
/// `λ` is this many RCR dispersions of the ratio sample (`5·σ_z`). A scale
/// field is smooth by physics — it is a flat-field residual — so a `λ`
/// this far above the kernel's own magnitude (`|φ| ≤ 0.184` on normalized
/// coordinates, see [`ThinPlateSpline::fit`]) deliberately leaves little
/// but the spline's affine part on a noisy sample and only lets the radial
/// terms in when the residuals are genuinely tighter than the structure
/// they carry.
pub const LN_LOCAL_SCALE_SMOOTHING_SIGMAS: f64 = 5.0;

#[derive(Debug, Clone, PartialEq)]
pub struct ScaleResult {
    /// RCR location of the matched flux ratios — the global relative scale.
    pub scale: f64,
    /// RCR dispersion of the same sample.
    pub sigma: f64,
    /// Pairs that survived matching (before RCR rejection).
    pub matches: usize,
    /// Of `matches`, how many RCR flagged as outliers.
    pub rejected: usize,
    /// The Moffat β both planes were fitted with — whatever the reference
    /// resolved (`psf::PsfModel::Moffat4`'s fixed 4.0, or `Auto`'s own
    /// per-plane search run once, on the reference only).
    pub beta: f64,
    /// Which matching pass the sample above came from (ruling R-M4c-9):
    /// `1` = the PSF-fit centroids, `2` = the detection barycentres. A tie
    /// keeps pass 1, so `2` means the barycentre pairing was strictly
    /// larger.
    pub pass: u8,
    /// The local scale model (ruling R-M4c-8), `None` unless the caller
    /// asked for one AND at least [`LN_LOCAL_SCALE_MIN_STARS`] DISTINCT
    /// reference stars survived RCR AND the spline could be fitted. The
    /// surface is the
    /// RESIDUAL around [`Self::scale`]: `A(x, y) = scale +
    /// local.displacement(x, y).0` (the y channel is fitted on zeros and
    /// carries nothing — see [`fit_local_scale`]).
    pub local: Option<ThinPlateSpline>,
}

/// Detect star seeds on one plane: registration's own detector for
/// positions (its saturation/eccentricity/SNR cuts are exactly the ones a
/// reliable flux match wants), converted to fit seeds. Fits (not run
/// here) come back brightest-first because detection's own sort order is
/// preserved through `to_seed` and by `fit_stars`/`fit_stars_with_beta`.
/// Shared by the reference's own-model fit and the target's
/// reference-β fit in [`relative_scale`].
fn detect_seeds(data: &[f32], width: usize, height: usize, max_stars: usize) -> Vec<Seed> {
    let cfg = DetectionConfig::default();
    let stars = detect_stars(data, width, height, &cfg, max_stars, None);
    stars.iter().map(to_seed).collect()
}

/// A fit seed from a registration [`Star`]. `Star` carries no peak
/// amplitude (registration only ever needed flux + optional σ), but
/// [`fit_stars`] reads `Seed::peak` in exactly one place — the field-level
/// `initial_sigma()` median used to size the fit stamp — so an amplitude
/// backed out of the flux/σ the detector already refined
/// (`peak = flux / (2π·σx·σy)`, the closed form for a 2-D Gaussian's
/// total) is accurate where it exists. Without a refined σ there is no
/// reliable peak estimate at all: `star.flux` alone (no 2π·σx·σy
/// denominator to shrink it back down) reads as a peak of order the
/// star's own total flux, which — if most of the brightest stars in a
/// plane lack a refined σ — collapses the whole-field `initial_sigma`
/// median toward its 0.7 px floor and starves every wider star of a big
/// enough fit stamp. `peak = 0.0` instead: `initial_sigma` filters
/// `peak > 0.0` and `fit_one` never reads `Seed::peak` at all, so a
/// σ-less seed drops out of that one median instead of poisoning it.
fn to_seed(star: &Star) -> Seed {
    let peak = match star.sigma {
        Some((sx, sy)) if sx > 0.0 && sy > 0.0 => star.flux / (2.0 * PI * sx * sy),
        _ => 0.0,
    };
    Seed {
        x: star.x,
        y: star.y,
        peak,
        flux: star.flux,
    }
}

/// Radius, in pixels, within which an accepted fit is linked back to the
/// detection seed (barycentre) it was fitted from — ruling R-M4c-9's pass
/// 2 needs that link, and [`FitOutcome`] carries no seed index. DERIVED,
/// not chosen: [`crate::stacking::psf_signal::fit_one`] refuses any fit
/// whose centre left its own seed by more than `centroid_tolerance_px` on
/// EITHER axis, so the seed is always within `tolerance·√2` of the fit it
/// produced, and no other seed can be closer than the `dedupe` pass allows
/// (fits within ±1 px of a brighter one are already gone). A fit whose
/// nearest seed still falls outside this radius — which the tolerance
/// makes impossible for the seed list it was fitted from — takes no part
/// in pass 2 rather than being paired with a stranger.
fn seed_link_radius(p: &FitParams) -> f64 {
    p.centroid_tolerance_px * SQRT_2
}

/// Per accepted fit, the detection barycentre it came from — the nearest
/// seed within [`seed_link_radius`], or `None` when there is none (see
/// that function for why that is a degenerate case, not a normal one).
/// Positions are in the same plane coordinates as the fits themselves.
/// `params` must be the SAME [`FitParams`] `fits` was produced with — the
/// link radius is derived from its own `centroid_tolerance_px`, so reading
/// a default here while the fits were made with something else would size
/// the radius against a rule those fits never obeyed (review m4).
fn link_barycentres(
    fits: &[StarFit],
    seeds: &[Seed],
    params: &FitParams,
) -> Vec<Option<(f64, f64)>> {
    let radius = seed_link_radius(params);
    let points: Vec<(f64, f64)> = seeds.iter().map(|s| (s.x, s.y)).collect();
    let tree = KdTree2::build(&points);
    fits.iter()
        .map(|f| {
            tree.nearest_within(f.x, f.y, radius)
                .map(|(i, _)| (seeds[i].x, seeds[i].y))
        })
        .collect()
}

/// A [`KdTree2`] over the PRESENT positions of `positions`, plus the map
/// from tree point index back to the slot (fit index) it came from. With
/// every slot present the map is the identity and the tree is exactly what
/// `KdTree2::build` over the fits' own centroids has always produced — the
/// property that keeps pass 1 bit-identical to M2's single-pass code.
fn tree_over(positions: &[Option<(f64, f64)>]) -> (KdTree2, Vec<usize>) {
    let mut points = Vec::with_capacity(positions.len());
    let mut of_point = Vec::with_capacity(positions.len());
    for (i, p) in positions.iter().enumerate() {
        if let Some(&(x, y)) = p.as_ref() {
            points.push((x, y));
            of_point.push(i);
        }
    }
    (KdTree2::build(&points), of_point)
}

/// One positional matching pass: each target slot claims its single
/// nearest reference point within `radius`, one way, in target-slot order.
/// Returns `(reference fit index, target fit index)` pairs — a reference
/// fit can be claimed by more than one target fit in a crowded field, and
/// RCR downstream absorbs the resulting duplicate ratios (that is M2's own
/// documented behaviour, unchanged; the local-scale spline dedupes on the
/// reference index itself, see [`fit_local_scale`]).
fn pair_positions(
    ref_tree: &KdTree2,
    ref_fit_of_point: &[usize],
    tgt_positions: &[Option<(f64, f64)>],
    radius: f64,
) -> Vec<(usize, usize)> {
    let mut pairs = Vec::with_capacity(tgt_positions.len());
    for (tgt_idx, p) in tgt_positions.iter().enumerate() {
        let Some(&(x, y)) = p.as_ref() else {
            continue;
        };
        if let Some((point, _dist)) = ref_tree.nearest_within(x, y, radius) {
            pairs.push((ref_fit_of_point[point], tgt_idx));
        }
    }
    pairs
}

/// Ruling R-M4c-9 in one place. Pass 1 matches the two planes' PSF-FIT
/// centroids; if its pairing covered fewer than
/// [`LN_BARYCENTRE_PASS_THRESHOLD`] of the TARGET's own accepted fits
/// (`tgt_fit_positions.len()`), a second pass matches the DETECTION
/// BARYCENTRES with the same radius, and the LARGER of the two pairings
/// wins.
///
/// **The denominator is the TARGET's fit count, not the reference's**
/// (review finding R-T5-2). The LN reference is an integration of the
/// group's best `referenceFrames` frames and is therefore deeper than any
/// single target: measured against ITS fit count, "matched under 80 %" is
/// the ordinary case and pass 2 would run on nearly every real frame — for
/// nothing, since the only pairs it can add are those whose two fits
/// drifted across the match radius, and a fit is bounded to
/// [`seed_link_radius`] of its own seed. The shortfall this pass exists to
/// repair is fits that WALKED, which is a property of the target, so the
/// target is what the threshold is measured against.
///
/// A tie keeps pass 1 — so a frame pass 1 already handled cannot have its
/// numbers changed by this rule, whatever the second pass finds. A target
/// with no accepted fits has no denominator and nothing to pair either
/// way: pass 1 (empty) stands.
///
/// `tgt_barycentres` is a CLOSURE, not a slice: linking a plane's fits
/// back to their seeds costs a tree over every one of them
/// ([`link_barycentres`]), and on the overwhelming majority of frames pass
/// 1 is enough — so that work happens only when the threshold actually
/// sends us to pass 2. The REFERENCE side is prepared eagerly instead
/// (once per group, amortized over its whole fan-out — see
/// [`PreparedReferenceChannel`]).
#[allow(clippy::too_many_arguments)]
fn choose_pairing(
    ref_tree: &KdTree2,
    ref_fit_of_point: &[usize],
    tgt_fit_positions: &[Option<(f64, f64)>],
    ref_barycentre_tree: &KdTree2,
    ref_barycentre_of_point: &[usize],
    tgt_barycentres: impl FnOnce() -> Vec<Option<(f64, f64)>>,
    radius: f64,
) -> (Vec<(usize, usize)>, u8) {
    let pass1 = pair_positions(ref_tree, ref_fit_of_point, tgt_fit_positions, radius);
    // The target's own accepted fits — one slot per fit, so the slice's
    // own length IS the count (R-T5-2, above).
    let tgt_fits = tgt_fit_positions.len();
    if tgt_fits == 0 || pass1.len() as f64 >= LN_BARYCENTRE_PASS_THRESHOLD * tgt_fits as f64 {
        return (pass1, 1);
    }
    let pass2 = pair_positions(
        ref_barycentre_tree,
        ref_barycentre_of_point,
        &tgt_barycentres(),
        radius,
    );
    if pass2.len() > pass1.len() {
        (pass2, 2)
    } else {
        (pass1, 1)
    }
}

/// The flux-ratio sample one pairing produces: `z_k =
/// signal_ref,k / signal_tgt,k` (math §4.3 step 3), the REFERENCE fit's
/// own centroid for each sample (where the local-scale spline is
/// evaluated) and that fit's index (what the spline dedupes on). All three
/// are index-aligned and in the pairing's own order, so the sample handed
/// to RCR is exactly what M2's inline loop built.
struct RatioSample {
    ratios: Vec<f64>,
    positions: Vec<(f64, f64)>,
    ref_idx: Vec<usize>,
}

fn ratio_sample(
    prepared: &PreparedReferenceChannel,
    tgt_outcome: &FitOutcome,
    pairs: &[(usize, usize)],
) -> RatioSample {
    let mut out = RatioSample {
        ratios: Vec::with_capacity(pairs.len()),
        positions: Vec::with_capacity(pairs.len()),
        ref_idx: Vec::with_capacity(pairs.len()),
    };
    for &(r, t) in pairs {
        let rf = &prepared.outcome.fits[r];
        let (flux_ref, flux_tgt) = (rf.signal, tgt_outcome.fits[t].signal);
        if flux_ref > 0.0 && flux_tgt > 0.0 {
            out.ratios.push(flux_ref / flux_tgt);
            out.positions.push((rf.x, rf.y));
            out.ref_idx.push(r);
        }
    }
    out
}

/// The local scale model of ruling R-M4c-8: an approximating thin-plate
/// spline through the RESIDUALS `z_k − scale` of the pairs RCR kept, at
/// their REFERENCE positions, with smoothing
/// `LN_LOCAL_SCALE_SMOOTHING_SIGMAS · σ_z`.
///
/// The spline is a two-channel object ([`ThinPlateSpline`] fits an x and a
/// y displacement over one node set) and only the x channel means anything
/// here: `dy` is all zeros and `displacement(..).1` is never read. Nodes
/// are grid-stratified ([`select_nodes`], cap [`TPS_MAX_NODES`]) so a
/// crowded corner cannot buy the whole budget, and deduped on the
/// REFERENCE fit index first: the one-way match lets two target fits claim
/// one reference star, and two coincident nodes make the bordered system
/// singular (`fit` would return `None` for the whole frame).
///
/// `None` — `A` stays the constant `scale` — when fewer than
/// [`LN_LOCAL_SCALE_MIN_STARS`] DISTINCT reference stars survived RCR
/// (counted after that dedupe, review m6), when the node cap's own floor
/// ([`TPS_MIN_NODES`]) is not met, or when the system is singular anyway.
/// Every one of those says so at `warn`.
fn fit_local_scale(
    sample: &RatioSample,
    kept: &[bool],
    scale: f64,
    sigma: f64,
    width: usize,
    height: usize,
) -> Option<ThinPlateSpline> {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut nodes: Vec<(f64, f64)> = Vec::new();
    let mut residuals: Vec<f64> = Vec::new();
    for (k, &keep) in kept.iter().enumerate().take(sample.ratios.len()) {
        if !keep {
            continue;
        }
        if !seen.insert(sample.ref_idx[k]) {
            continue;
        }
        nodes.push(sample.positions[k]);
        residuals.push(sample.ratios[k] - scale);
    }

    // The floor is applied AFTER the dedupe (review m6): a crowded field
    // where 40 surviving pairs collapse onto 5 distinct reference stars
    // carries five stars' worth of information, not forty, and a surface
    // fitted on it would be guarded by nothing but [`TPS_MIN_NODES`].
    // What the floor counts is what the spline is actually fitted on.
    if nodes.len() < LN_LOCAL_SCALE_MIN_STARS {
        warn!(
            count = nodes.len(),
            "local scale: too few distinct matched stars survived RCR; A stays the global scale"
        );
        return None;
    }

    // `select_nodes` stratifies over each pair's REFERENCE coordinate (its
    // second element) — which is the only coordinate an LN pair has, both
    // planes already living in the reference geometry — and orders by σ
    // within a cell. There is no per-star σ here, so `None`: the order
    // inside a cell is the node order, which is the target fits' own
    // amplitude order (brightest first, `psf_signal::dedupe`).
    let pairs: Vec<Pair> = nodes.iter().map(|&p| (p, p)).collect();
    let idx = select_nodes(&pairs, None, (width as f64, height as f64), TPS_MAX_NODES);
    if idx.len() < TPS_MIN_NODES {
        warn!(
            ln_local_nodes = idx.len(),
            "local scale: too few distinct nodes for a spline; A stays the global scale"
        );
        return None;
    }
    let chosen_nodes: Vec<(f64, f64)> = idx.iter().map(|&i| nodes[i]).collect();
    let chosen_dz: Vec<f64> = idx.iter().map(|&i| residuals[i]).collect();
    let zeros = vec![0.0f64; chosen_nodes.len()];

    let lambda = LN_LOCAL_SCALE_SMOOTHING_SIGMAS * sigma;
    // A σ that is not a usable number (an RCR sample so degenerate its
    // dispersion came back NaN, or a negative one, which cannot happen but
    // would poison the solve) falls back to the interpolating spline
    // rather than refusing a local scale outright.
    let lambda = if lambda.is_finite() && lambda >= 0.0 {
        lambda
    } else {
        0.0
    };

    let spline = ThinPlateSpline::fit(&chosen_nodes, &chosen_dz, &zeros, lambda);
    if spline.is_none() {
        warn!(
            ln_local_nodes = chosen_nodes.len(),
            "local scale: the spline could not be fitted; A stays the global scale"
        );
    }
    spline
}

/// The reference side of [`relative_scale`] (detection + PSF fit + the
/// built match tree), computed ONCE PER GROUP instead of once per frame
/// (final fix wave, I2): the LN reference plane is immutable for a group's
/// whole fan-out, but `relative_scale` used to re-detect and re-fit it on
/// EVERY call — `LnReferenceForDetection` (`ln/mod.rs`) already hoists the
/// group-level sanitized copy for this exact reason (fix round 1, item 6);
/// this hoists the far more expensive detect+fit+tree half that was left
/// behind. `outcome.fits[i].signal`/`outcome.beta` are what
/// [`relative_scale_against`] reads; `tree` is built from the same fits'
/// centroids, exactly as [`relative_scale`]'s own body used to build it
/// inline.
pub struct PreparedReferenceChannel {
    outcome: FitOutcome,
    /// Tree over every accepted fit's PSF centroid; point index IS the fit
    /// index (`fit_of_point` is the identity — kept explicit so pass 1 and
    /// pass 2 share one [`pair_positions`]).
    tree: KdTree2,
    fit_of_point: Vec<usize>,
    /// Ruling R-M4c-9's pass-2 side of the same reference: a tree over the
    /// DETECTION barycentres the accepted fits came from, with the map back
    /// to fit indices. Built here rather than lazily per frame for exactly
    /// the reason the fit tree is (I2): the reference is immutable for a
    /// group's whole fan-out.
    barycentre_tree: KdTree2,
    barycentre_of_point: Vec<usize>,
}

impl PreparedReferenceChannel {
    /// `reference` is one channel's row-major `width × height` plane
    /// (already in the reference geometry); `psf`/`max_stars` are the
    /// SAME values a direct [`relative_scale`] call on this reference would
    /// use.
    pub fn build(
        reference: &[f32],
        width: usize,
        height: usize,
        psf: PsfModel,
        max_stars: usize,
    ) -> PreparedReferenceChannel {
        let fit_params = FitParams::default();
        let ref_seeds = detect_seeds(reference, width, height, max_stars);
        let outcome = fit_stars(reference, width, height, &ref_seeds, psf, &fit_params);
        let fit_positions: Vec<Option<(f64, f64)>> =
            outcome.fits.iter().map(|f| Some((f.x, f.y))).collect();
        let (tree, fit_of_point) = tree_over(&fit_positions);
        let barycentres = link_barycentres(&outcome.fits, &ref_seeds, &fit_params);
        let (barycentre_tree, barycentre_of_point) = tree_over(&barycentres);
        PreparedReferenceChannel {
            outcome,
            tree,
            fit_of_point,
            barycentre_tree,
            barycentre_of_point,
        }
    }
}

/// Global relative scale `s = RCR_loc(z_k)`, `z_k = flux_ref,k / flux_tgt,k`
/// over stars matched within `match_radius_px` of each other (math §4.3),
/// against an already-[`PreparedReferenceChannel::build`]t reference — the
/// per-frame half of what [`relative_scale`] used to do in one call.
/// `z_k` is built from [`StarFit::signal`] (background-subtracted flux
/// inside the fitted FWTM ellipse), never [`StarFit::mean_flux`]: for a
/// fixed β the FWTM ellipse encloses a fixed fraction of the profile
/// regardless of FWHM, so `signal` is width-independent — `mean_flux`
/// divides by the ellipse's own area (`π·(k/2)²·fwtm_x·fwtm_y`, which
/// scales as FWTM²) and would leak the two planes' seeing difference
/// straight into the scale.
///
/// The TARGET always fits at the reference's OWN resolved β
/// ([`psf_signal::fit_stars_with_beta`], never a second, independent
/// `Auto` search) — because β changes the FWTM-enclosed flux fraction
/// (math §1.4), fitting the two planes at two different β values would
/// bias the ratio systematically even though `signal` itself is
/// width-independent for a FIXED β (the original review's finding).
/// `target` is a row-major `width × height` plane already in the reference
/// geometry (registered); `max_stars` should be the same `measurement`
/// config value the frame's own measurement pass used. Fewer than
/// [`MIN_MATCHES`] surviving pairs is [`LnError::TooFewMatches`] — the
/// caller excludes the frame from the LN pass rather than trust a scale
/// from a handful of stars.
///
/// `local_scale` is `normalization.local.localScale`: with it on, the
/// returned [`ScaleResult::local`] carries the local scale spline of
/// ruling R-M4c-8 (when enough pairs survived — see [`fit_local_scale`]);
/// with it off that field is `None` and every number this function returns
/// is what M2/M3/M4a produced, unchanged.
#[allow(clippy::too_many_arguments)]
pub fn relative_scale_against(
    prepared: &PreparedReferenceChannel,
    target: &[f32],
    width: usize,
    height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
) -> Result<ScaleResult, LnError> {
    let fit_params = FitParams::default();
    let tgt_seeds = detect_seeds(target, width, height, max_stars);
    let tgt_outcome = fit_stars_with_beta(
        target,
        width,
        height,
        &tgt_seeds,
        prepared.outcome.beta,
        &fit_params,
    );

    // Pass 1 on the PSF-fit centroids; pass 2 (ruling R-M4c-9) on the
    // DETECTION barycentres, only when pass 1 covered too little of the
    // TARGET's own fits (review R-T5-2) — `choose_pairing` owns the whole
    // rule, including the tie that keeps pass 1.
    let tgt_fit_positions: Vec<Option<(f64, f64)>> =
        tgt_outcome.fits.iter().map(|f| Some((f.x, f.y))).collect();
    let (pairs, pass) = choose_pairing(
        &prepared.tree,
        &prepared.fit_of_point,
        &tgt_fit_positions,
        &prepared.barycentre_tree,
        &prepared.barycentre_of_point,
        || link_barycentres(&tgt_outcome.fits, &tgt_seeds, &fit_params),
        match_radius_px,
    );

    let sample = ratio_sample(prepared, &tgt_outcome, &pairs);
    if sample.ratios.len() < MIN_MATCHES {
        return Err(LnError::TooFewMatches {
            matches: sample.ratios.len(),
        });
    }

    let r = crate::stacking::robust::rcr(&sample.ratios, rcr_limit);
    let local = if local_scale {
        fit_local_scale(&sample, &r.kept, r.location, r.scale, width, height)
    } else {
        None
    };
    debug!(
        ln_scale = r.location,
        sigma = r.scale,
        ln_matches = sample.ratios.len(),
        rejected = r.rejected,
        ln_pass = pass,
        ln_local_nodes = local.as_ref().map_or(0, |s| s.nodes.len()),
        "ln relative scale"
    );
    Ok(ScaleResult {
        scale: r.location,
        sigma: r.scale,
        matches: sample.ratios.len(),
        rejected: r.rejected,
        beta: prepared.outcome.beta,
        pass,
        local,
    })
}

/// Thin wrapper: [`PreparedReferenceChannel::build`] +
/// [`relative_scale_against`] in one call — used by the probe and by every
/// existing test that has no group-level `PreparedReferenceChannel` handy.
/// `normalize_frame` (the real per-frame pipeline, `ln/mod.rs`) calls
/// [`relative_scale_against`] directly against the group's ONE prepared
/// reference channel instead, so it never re-detects or re-fits the
/// reference plane per frame (final fix wave, I2).
#[allow(clippy::too_many_arguments)]
pub fn relative_scale(
    reference: &[f32],
    target: &[f32],
    width: usize,
    height: usize,
    psf: PsfModel,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
) -> Result<ScaleResult, LnError> {
    let prepared = PreparedReferenceChannel::build(reference, width, height, psf, max_stars);
    relative_scale_against(
        &prepared,
        target,
        width,
        height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;
    use crate::stacking::test_fixtures::synthetic_star_field;
    use crate::test_support::{add_noise, moffat_field, MoffatStar};

    const WIDTH: usize = 512;
    const HEIGHT: usize = 384;
    const FWHM: f64 = 4.2; // sigma ~1.8 px, the same field family measure.rs's tests use
    const NOISE: f32 = 0.002;

    /// 10x6 = 60 stars on a jittered grid, amplitudes spread over
    /// `[0.1, 0.3)`, well clear of `NOISE` and `DetectionConfig::default()`'s
    /// `min_snr = 10.0`.
    fn star_grid(seed: u64) -> Vec<(f64, f64, f64)> {
        let mut rng = SplitMix64(seed);
        let mut stars = Vec::with_capacity(60);
        for j in 0..6 {
            for i in 0..10 {
                let x = 40.0 + i as f64 * 48.0 + (rng.next_f64() - 0.5) * 12.0;
                let y = 40.0 + j as f64 * 60.0 + (rng.next_f64() - 0.5) * 12.0;
                let amp = 0.1 + 0.2 * rng.next_f64();
                stars.push((x, y, amp));
            }
        }
        stars
    }

    fn scale_stars(stars: &[(f64, f64, f64)], k: f64) -> Vec<(f64, f64, f64)> {
        stars.iter().map(|&(x, y, a)| (x, y, a * k)).collect()
    }

    #[test]
    fn matched_flux_ratio_recovers_a_uniform_scale() {
        let stars = star_grid(1);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 11);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 21);

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a clean uniformly-scaled field must match");

        assert!((r.scale - 1.25).abs() < 0.01, "scale {}", r.scale);
        assert!(
            r.matches as f64 >= 0.9 * stars.len() as f64,
            "matched {} of {}",
            r.matches,
            stars.len()
        );
        // A uniformly-scaled field is clean by construction, but RCR's own
        // false-positive rate at this sample size can still flag a handful
        // of legitimate points (`rcr_keeps_a_clean_gaussian_sample` in
        // `robust.rs` allows up to 5% on a much larger sample) — the bar
        // here is "no systematic rejection", not zero.
        assert!(r.rejected <= 5, "rejected {} of {}", r.rejected, r.matches);
    }

    /// I2 (final fix wave): `relative_scale` is now a thin
    /// build-then-`relative_scale_against` wrapper — this pins that the
    /// split produces IDENTICAL results to a direct `relative_scale_against`
    /// call against a `PreparedReferenceChannel` built the same way.
    #[test]
    fn relative_scale_equals_relative_scale_against_a_prepared_reference() {
        let stars = star_grid(1);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 11);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 21);

        let via_wrapper = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a clean uniformly-scaled field must match");

        let prepared =
            PreparedReferenceChannel::build(&reference, WIDTH, HEIGHT, PsfModel::Moffat4, 200);
        let via_prepared =
            relative_scale_against(&prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false)
                .expect("the same prepared reference must match the same target");

        assert_eq!(via_wrapper, via_prepared);
    }

    #[test]
    fn rcr_rejects_gross_outlier_stars_and_keeps_the_scale() {
        let stars = star_grid(2);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 12);
        // Same uniform 0.8x scale as the clean case, but the last 10% (6 of
        // 60) get an extra x3 on top — a target flux far enough from the
        // bulk ratio (1.25 vs ~0.417) that RCR (limit 0.3) must drop them.
        let outliers = (stars.len() * 9) / 10;
        let target_stars: Vec<(f64, f64, f64)> = stars
            .iter()
            .enumerate()
            .map(|(idx, &(x, y, a))| {
                let k = if idx >= outliers { 0.8 * 3.0 } else { 0.8 };
                (x, y, a * k)
            })
            .collect();
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 22);

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a majority-clean field must still match");

        assert!((r.scale - 1.25).abs() < 0.02, "scale {}", r.scale);
        let expected_outliers = stars.len() - outliers;
        assert!(
            r.rejected + 1 >= expected_outliers,
            "rejected {} of {} planted outliers",
            r.rejected,
            expected_outliers
        );
    }

    #[test]
    fn a_starless_target_is_too_few_matches() {
        let stars = star_grid(3);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 13);
        // No stars at all on the target side — flat background plus noise.
        let target = synthetic_star_field(WIDTH, HEIGHT, &[], FWHM, NOISE, 23);

        let err = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect_err("a starless target cannot produce 20 matched pairs");

        match err {
            LnError::TooFewMatches { matches } => {
                assert!(matches < MIN_MATCHES, "matches {matches}");
            }
            other => panic!("expected TooFewMatches, got {other:?}"),
        }
    }

    /// `FWHM = 2·α·√(2^{1/β} − 1)` for a Moffat profile (the analogue of
    /// [`crate::stacking::psf_signal::fwtm_from_alpha`] at half- rather
    /// than tenth-maximum — FWHM is the half-maximum width, FWTM the
    /// tenth-maximum one), inverted for `α`.
    fn moffat_alpha_for_fwhm(fwhm: f64, beta: f64) -> f64 {
        fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt())
    }

    /// A uniform-flux Moffat (β = 4) star field: every star carries the
    /// SAME total flux `flux` (`π·A·αx·αy/(β−1)`, `test_support::
    /// moffat_field`'s own doc) — "keep the fluxes equal per star" (fix
    /// round 1, item 1) — at `fwhm` px, so the only thing that varies
    /// between a reference and target built from two calls with different
    /// `fwhm` is the seeing, not a randomized per-star flux.
    fn seeing_field(
        positions: &[(f64, f64)],
        flux: f64,
        fwhm: f64,
        beta: f64,
        w: usize,
        h: usize,
        noise_seed: u64,
    ) -> Vec<f32> {
        let alpha = moffat_alpha_for_fwhm(fwhm, beta);
        let amp = flux * (beta - 1.0) / (PI * alpha * alpha);
        let stars: Vec<MoffatStar> = positions
            .iter()
            .map(|&(x, y)| MoffatStar {
                x,
                y,
                amp,
                alpha_x: alpha,
                alpha_y: alpha,
                theta: 0.0,
            })
            .collect();
        let mut data = moffat_field(w, h, &stars, beta, 0.08);
        add_noise(&mut data, NOISE, noise_seed);
        data
    }

    /// The reference (FWHM 2.5) / target (FWHM 3.0, flux × 0.8) pair fix
    /// round 1's items 1 and 3 both test against.
    fn seeing_difference_pair() -> (Vec<f32>, Vec<f32>) {
        let positions: Vec<(f64, f64)> = star_grid(4).iter().map(|&(x, y, _)| (x, y)).collect();
        let beta = 4.0;
        // Uniform total flux, chosen so the reference's peak amplitude
        // (~0.2 native units) is comfortably above `NOISE` and below the
        // detector's saturation cut, and the target's (dimmer AND wider,
        // so its peak amplitude drops further still) stays well above it.
        let flux = 1.8;
        let reference = seeing_field(&positions, flux, 2.5, beta, WIDTH, HEIGHT, 41);
        let target = seeing_field(&positions, flux * 0.8, 3.0, beta, WIDTH, HEIGHT, 42);
        (reference, target)
    }

    #[test]
    fn seeing_difference_does_not_bias_the_scale() {
        let (reference, target) = seeing_difference_pair();
        let positions_len = star_grid(4).len();

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a clean field differing only in seeing must still match");

        assert!((r.scale - 1.25).abs() < 0.02, "scale {}", r.scale);
        assert!(
            r.matches as f64 >= 0.9 * positions_len as f64,
            "matched {} of {}",
            r.matches,
            positions_len
        );
    }

    #[test]
    fn target_is_fitted_with_the_reference_beta() {
        let (reference, target) = seeing_difference_pair();

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Auto,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("Auto must resolve on this clean, purely-Moffat4 field");

        // Independently resolve what `Auto` picks for the REFERENCE alone,
        // the exact same way `relative_scale` does internally — the
        // target must have been fitted at this same β, not its own.
        let ref_seeds = detect_seeds(&reference, WIDTH, HEIGHT, 200);
        let ref_out = fit_stars(
            &reference,
            WIDTH,
            HEIGHT,
            &ref_seeds,
            PsfModel::Auto,
            &FitParams::default(),
        );

        assert_eq!(r.beta, ref_out.beta);
        assert!(
            crate::stacking::psf_signal::AUTO_BETAS.contains(&r.beta),
            "beta {} is not one of AUTO_BETAS",
            r.beta
        );
    }

    #[test]
    fn explicit_moffat4_pins_beta_four() {
        let (reference, target) = seeing_difference_pair();

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a clean field differing only in seeing must still match");

        assert_eq!(r.beta, 4.0);
    }

    // ---- M4c ruling R-M4c-9: the barycentre second matching pass -------

    fn present(v: &[(f64, f64)]) -> Vec<Option<(f64, f64)>> {
        v.iter().map(|&p| Some(p)).collect()
    }

    fn grid_positions(seed: u64) -> Vec<(f64, f64)> {
        star_grid(seed).iter().map(|&(x, y, _)| (x, y)).collect()
    }

    /// `choose_pairing` on a target whose PSF-FIT centroids all walked
    /// away from the reference's while the DETECTION barycentres stayed
    /// where they were: pass 1 pairs nothing, so the rule runs pass 2 on
    /// the barycentres and its (strictly larger) pairing wins.
    ///
    /// **The displacement is 5 px, not the brief's 2.5 px** — deliberately
    /// larger than `match_radius_px`, which is what it takes to empty pass
    /// 1 at all (a 2.5 px walk is comfortably INSIDE the 4 px window and
    /// pass 1 keeps every pair: that is the next test). It is also larger
    /// than the fitter's own `centroid_tolerance_px` allows a real fit to
    /// walk from its own seed, which is exactly why this pins the RULE on
    /// synthesized position lists rather than on two rendered planes: no
    /// achievable pair of real planes can empty the window this way.
    #[test]
    fn the_barycentre_pass_recovers_a_pairing_the_fit_positions_lost() {
        let grid = grid_positions(7);
        let ref_fits = present(&grid);
        let ref_bary = present(&grid);
        let (ref_tree, ref_map) = tree_over(&ref_fits);
        let (bary_tree, bary_map) = tree_over(&ref_bary);

        let walked: Vec<(f64, f64)> = grid.iter().map(|&(x, y)| (x + 5.0, y)).collect();
        let tgt_fits = present(&walked);
        let tgt_bary = present(&grid);

        let (pairs, pass) = choose_pairing(
            &ref_tree,
            &ref_map,
            &tgt_fits,
            &bary_tree,
            &bary_map,
            || tgt_bary,
            4.0,
        );

        let pass1 = pair_positions(&ref_tree, &ref_map, &tgt_fits, 4.0);
        // The denominator is the TARGET's own fit count (R-T5-2), which
        // here happens to equal the reference's.
        assert!(
            (pass1.len() as f64) < LN_BARYCENTRE_PASS_THRESHOLD * tgt_fits.len() as f64,
            "pass 1 matched {} of the target's {} fits — the second pass would not even run",
            pass1.len(),
            tgt_fits.len()
        );
        assert_eq!(pass, 2, "the barycentre pairing must win");
        assert!(
            pairs.len() as f64 >= 0.9 * grid.len() as f64,
            "pass 2 matched {} of {}",
            pairs.len(),
            grid.len()
        );
    }

    /// The negative control at the brief's own 2.5 px: a walk that small
    /// never leaves the 4 px window, pass 1 covers everything, and the
    /// barycentre pass does not run at all (`pass == 1`).
    #[test]
    fn a_small_fit_walk_keeps_pass_one() {
        let grid = grid_positions(8);
        let ref_fits = present(&grid);
        let (ref_tree, ref_map) = tree_over(&ref_fits);
        // An EMPTY barycentre side: if pass 2 ran at all it could only
        // shrink the pairing, so this also pins that it does not run.
        let (bary_tree, bary_map) = tree_over(&[]);

        let walked: Vec<(f64, f64)> = grid.iter().map(|&(x, y)| (x + 2.5, y)).collect();
        let tgt_fits = present(&walked);

        let (pairs, pass) = choose_pairing(
            &ref_tree,
            &ref_map,
            &tgt_fits,
            &bary_tree,
            &bary_map,
            || panic!("the barycentre pass must not even be prepared here"),
            4.0,
        );

        assert_eq!(pass, 1);
        assert_eq!(pairs.len(), grid.len());
    }

    /// A tie keeps pass 1 (ruling R-M4c-9's own wording is "the larger of
    /// the two pairings"): nothing about a frame pass 1 already handled may
    /// change because the second pass found the same number of pairs.
    ///
    /// Half of the TARGET's own fits sit where no reference star is, so
    /// pass 1 covers 50 % of them — under the threshold on the R-T5-2
    /// denominator — and the barycentre pass runs; it is handed the same
    /// positions, so it ties, and the tie keeps pass 1.
    #[test]
    fn a_tie_between_the_two_pairings_keeps_pass_one() {
        let grid = grid_positions(9);
        let half = grid.len() / 2;
        let mut tgt_positions: Vec<(f64, f64)> = grid[..half].to_vec();
        // The other half, parked far from every reference star (the
        // frame's own stars are on a 48 px pitch starting at x = 40).
        tgt_positions.extend(grid[half..].iter().map(|&(_, y)| (-1000.0, y)));

        let ref_fits = present(&grid);
        let (ref_tree, ref_map) = tree_over(&ref_fits);
        let (bary_tree, bary_map) = tree_over(&ref_fits);
        let tgt = present(&tgt_positions);

        let (pairs, pass) = choose_pairing(
            &ref_tree,
            &ref_map,
            &tgt,
            &bary_tree,
            &bary_map,
            || tgt.clone(),
            4.0,
        );

        assert!(
            (half as f64) < LN_BARYCENTRE_PASS_THRESHOLD * tgt.len() as f64,
            "the scene must put pass 1 under the threshold: {half} of {}",
            tgt.len()
        );
        // … and pass 2 finds exactly as many pairs, which is not larger.
        assert_eq!(pass, 1);
        assert_eq!(pairs.len(), half);
    }

    /// R-T5-2's whole point: a target that matched every one of ITS OWN
    /// fits stays on pass 1 even though the (deeper) LN reference has many
    /// more fits than that — which is the ordinary case on real data, and
    /// what the reference-side denominator got wrong.
    #[test]
    fn a_deeper_reference_does_not_trigger_the_barycentre_pass() {
        let deep = grid_positions(10);
        // The target sees only the brightest quarter of the reference's
        // stars — 15 of 60 — but every one of them pairs.
        let shallow: Vec<(f64, f64)> = deep[..deep.len() / 4].to_vec();
        let ref_fits = present(&deep);
        let (ref_tree, ref_map) = tree_over(&ref_fits);
        let (bary_tree, bary_map) = tree_over(&ref_fits);
        let tgt = present(&shallow);

        let (pairs, pass) = choose_pairing(
            &ref_tree,
            &ref_map,
            &tgt,
            &bary_tree,
            &bary_map,
            || panic!("a fully-matched target must not reach the barycentre pass"),
            4.0,
        );

        assert!(
            (pairs.len() as f64) < LN_BARYCENTRE_PASS_THRESHOLD * deep.len() as f64,
            "the scene must be one the OLD reference-side denominator would have tripped: \
             {} pairs vs the reference's {} fits",
            pairs.len(),
            deep.len()
        );
        assert_eq!(pass, 1);
        assert_eq!(pairs.len(), shallow.len());
    }

    /// A [`StarFit`] at `(x, y)` — only the position and a positive
    /// `signal` matter to the pairing machinery under test.
    fn fit_at(x: f64, y: f64) -> StarFit {
        StarFit {
            x,
            y,
            background: 0.0,
            amplitude: 1.0,
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            fwtm_x: 6.0,
            fwtm_y: 6.0,
            theta: 0.0,
            beta: 4.0,
            residual: 0.01,
            signal: 100.0,
            area: 28.0,
        }
    }

    fn seed_at(x: f64, y: f64) -> Seed {
        Seed {
            x,
            y,
            peak: 1.0,
            flux: 100.0,
        }
    }

    /// Review m8: a fit that walked from its seed still finds it (the link
    /// radius is `centroid_tolerance_px · √2`), a fit that is farther than
    /// the fitter could ever have put it links to nothing, and the
    /// returned positions are the SEEDS' — not the fits'.
    #[test]
    fn link_barycentres_finds_the_seed_a_walked_fit_came_from() {
        let params = FitParams::default();
        let seeds = vec![seed_at(100.0, 100.0), seed_at(300.0, 220.0)];
        let fits = vec![
            // Walked by the most the tolerance allows on both axes
            // (1.5, 1.5) — distance 2.12, exactly the link radius.
            fit_at(101.5, 101.5),
            // Twice that: no fitter could have produced this from either
            // seed, so it must not be linked to a stranger.
            fit_at(304.0, 224.0),
        ];

        let linked = link_barycentres(&fits, &seeds, &params);

        assert_eq!(linked.len(), 2);
        assert_eq!(
            linked[0],
            Some((100.0, 100.0)),
            "the walked fit must link to its own seed's barycentre"
        );
        assert_eq!(
            linked[1], None,
            "a fit beyond the link radius must link to nothing, not to the nearest stranger"
        );
    }

    /// Review m8: the `of_point` map is NOT the identity once some slots
    /// are absent, and `pair_positions` must report the FIT index, not the
    /// tree's point index. With the first two reference fits unlinked, a
    /// target landing on reference fit 3 must come back as `3`.
    #[test]
    fn a_pairing_through_a_shifted_map_reports_fit_indices() {
        let ref_positions = vec![
            None,
            None,
            Some((50.0, 50.0)),
            Some((150.0, 60.0)),
            Some((260.0, 70.0)),
        ];
        let (tree, of_point) = tree_over(&ref_positions);
        assert_eq!(
            of_point,
            vec![2, 3, 4],
            "the map must skip the absent slots"
        );

        let tgt = vec![Some((150.5, 60.5)), Some((259.0, 70.0))];
        let pairs = pair_positions(&tree, &of_point, &tgt, 4.0);

        assert_eq!(
            pairs,
            vec![(3, 0), (4, 1)],
            "pairs must carry reference FIT indices, not tree point indices"
        );
    }

    /// Review m6: the 40-star floor counts DISTINCT reference stars. 60
    /// surviving pairs that all claim the same 5 reference fits carry five
    /// stars' worth of information and must not produce a spline.
    #[test]
    fn the_local_scale_floor_counts_distinct_reference_stars() {
        let crowded = RatioSample {
            ratios: (0..60).map(|k| 0.8 + (k % 7) as f64 * 0.001).collect(),
            positions: (0..60)
                .map(|k| {
                    let s = k % 5;
                    (40.0 + s as f64 * 90.0, 40.0 + s as f64 * 60.0)
                })
                .collect(),
            ref_idx: (0..60).map(|k| k % 5).collect(),
        };
        let kept = vec![true; 60];

        assert!(
            fit_local_scale(&crowded, &kept, 0.8, 0.01, WIDTH, HEIGHT).is_none(),
            "60 pairs over 5 distinct stars must not fit a surface"
        );

        // The control: the same 60 pairs, one distinct reference star
        // each, spread over the frame — that one DOES fit.
        let spread = RatioSample {
            ratios: crowded.ratios.clone(),
            positions: (0..60)
                .map(|k| (30.0 + (k % 10) as f64 * 45.0, 30.0 + (k / 10) as f64 * 55.0))
                .collect(),
            ref_idx: (0..60).collect(),
        };
        assert!(
            fit_local_scale(&spread, &kept, 0.8, 0.01, WIDTH, HEIGHT).is_some(),
            "60 distinct stars over the frame must fit a surface"
        );
    }

    #[test]
    fn a_clean_field_stays_on_pass_one_and_carries_no_local_model() {
        let stars = star_grid(1);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 11);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 21);

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a clean uniformly-scaled field must match");

        assert_eq!(r.pass, 1);
        assert!(r.local.is_none(), "localScale was off");
    }

    // ---- M4c ruling R-M4c-8: the local scale spline --------------------

    /// The brief's flat-field-like gradient: every target star's amplitude
    /// is scaled by `k(x) = 1.2 + 0.1·(x/w − 0.5)`, so the RATIO the scale
    /// measures — `z = flux_ref / flux_tgt` — follows `1/k(x)`, from
    /// `1/1.15 ≈ 0.870` at the left edge to `1/1.25 = 0.800` at the right.
    fn gradient_k(x: f64) -> f64 {
        1.2 + 0.1 * (x / WIDTH as f64 - 0.5)
    }

    fn gradient_pair(seed: u64) -> (Vec<f32>, Vec<f32>) {
        let stars = star_grid(seed);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 51);
        let target_stars: Vec<(f64, f64, f64)> = stars
            .iter()
            .map(|&(x, y, a)| (x, y, a * gradient_k(x)))
            .collect();
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 52);
        (reference, target)
    }

    #[test]
    fn the_local_spline_follows_a_scale_gradient() {
        let (reference, target) = gradient_pair(11);

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            true,
        )
        .expect("a clean field with a smooth scale gradient must match");

        assert!(
            r.matches >= LN_LOCAL_SCALE_MIN_STARS,
            "matched {} — below the local-scale floor, the spline would be skipped",
            r.matches
        );
        let spline = r
            .local
            .as_ref()
            .expect("localScale was on and enough pairs survived");

        let y = (HEIGHT / 2) as f64;
        // Left / centre / right of the frame, the columns the brief names.
        for x in [0.0, (WIDTH / 2) as f64, (WIDTH - 1) as f64] {
            let a = r.scale + spline.displacement(x, y).0;
            let want = 1.0 / gradient_k(x);
            assert!(
                (a - want).abs() < 0.01,
                "A({x}) = {a}, expected {want} (the 1/k gradient) within 0.01"
            );
        }
    }

    /// With `localScale` off the SAME field yields the identical global
    /// numbers and no spline: the constant RCR location is all `A` gets.
    /// This is the "off is byte-identical" contract in one assertion.
    #[test]
    fn without_local_scale_the_gradient_field_keeps_the_constant_scale() {
        let (reference, target) = gradient_pair(11);

        let off = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            false,
        )
        .expect("a clean field with a smooth scale gradient must match");
        let on = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            true,
        )
        .expect("a clean field with a smooth scale gradient must match");

        assert!(off.local.is_none());
        assert!(on.local.is_some());
        // Everything the global term is made of is untouched by the flag.
        assert_eq!(off.scale, on.scale);
        assert_eq!(off.sigma, on.sigma);
        assert_eq!(off.matches, on.matches);
        assert_eq!(off.rejected, on.rejected);
        assert_eq!(off.pass, on.pass);
    }

    /// Peak-to-peak, in units of `σ_z`, that a PURE-NOISE ratio sample's
    /// spurious local-scale surface may reach — see
    /// [`a_pure_noise_sample_produces_only_a_small_spurious_surface`].
    const LOCAL_SCALE_NOISE_PTP_SIGMAS: f64 = 3.0;

    /// **R-T5-1 control pin.** The gradient test above is noise-free
    /// (`NOISE = 0.002`), so it cannot see what the review found by
    /// replicating [`ThinPlateSpline::fit`] numerically: because
    /// `λ = LN_LOCAL_SCALE_SMOOTHING_SIGMAS · σ_z` scales WITH the
    /// dispersion and the solve is linear, a pure-noise ratio sample — no
    /// true structure at all — still yields a smooth SPURIOUS surface of
    /// peak-to-peak ≈ 1–2·σ_z. The ±25 % safety band never sees it (it is
    /// two orders of magnitude below), and the math reference's
    /// surface-simplification step (§4.3 step 5: tolerance 3·σ_z, reject
    /// fraction 0.1), which is what would suppress it, was dropped by
    /// ruling R-M4c-8 and is NOT being implemented blind — Task 7's
    /// acceptance variant E measures the effect on real data first.
    ///
    /// So this is a NUMBER TO BEAT, not a guard: a uniformly scaled target
    /// (constant true scale, nothing for a surface to find) at a realistic
    /// ratio dispersion, asserting the sampled `A` grid's peak-to-peak
    /// stays within [`LOCAL_SCALE_NOISE_PTP_SIGMAS`]·σ_z. **Measured
    /// 2026-09-12 over 10 seeds at σ_z ∈ [0.031, 0.038]: ptp/σ_z ∈
    /// [0.92, 2.18]**, so the pin sits at 3.0 — ≈ 1.4× the observed
    /// maximum. For scale, the gradient test's own REAL structure runs at
    /// ptp/σ_z ≈ 5, so this bound still separates signal from the
    /// artefact. A λ change, or the simplification step arriving, should
    /// push these numbers DOWN and this constant with them.
    #[test]
    fn a_pure_noise_sample_produces_only_a_small_spurious_surface() {
        // `NOISE` (0.002) gives σ_z ≈ 0.014; this level is what puts the
        // ratio dispersion at the ≈ 0.03 the review asked for.
        const LOUD: f32 = 0.006;
        const STRIDE: usize = 128;

        for seed in 0..3u64 {
            let stars = star_grid(70 + seed);
            let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, LOUD, 300 + seed);
            // A UNIFORM scale: the true surface is flat everywhere, so
            // whatever the spline finds is the sample's own noise.
            let target_stars = scale_stars(&stars, 0.8);
            let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, LOUD, 400 + seed);

            let r = relative_scale(
                &reference,
                &target,
                WIDTH,
                HEIGHT,
                PsfModel::Moffat4,
                200,
                4.0,
                0.3,
                true,
            )
            .expect("a uniformly-scaled field must match even at this noise level");
            assert!(
                r.matches >= LN_LOCAL_SCALE_MIN_STARS,
                "seed {seed}: matched {} — the scene must clear the local-scale floor",
                r.matches
            );
            let spline = r
                .local
                .as_ref()
                .expect("enough distinct stars survived, so a surface was fitted");

            let (gw, gh) = super::super::LnGrid::grid_dims(WIDTH, HEIGHT, STRIDE);
            let (a, used) =
                super::super::a_grid(Some(spline), r.scale, gw, gh, STRIDE, WIDTH, HEIGHT);
            assert!(
                used,
                "seed {seed}: the spurious surface sits far inside the safety band, so what \
                 this pin measures is the SAMPLED grid, not the constant fallback"
            );

            let lo = a.iter().cloned().fold(f32::INFINITY, f32::min) as f64;
            let hi = a.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as f64;
            let ratio = (hi - lo) / r.sigma;
            assert!(
                ratio <= LOCAL_SCALE_NOISE_PTP_SIGMAS,
                "seed {seed}: a pure-noise sample produced a surface of peak-to-peak {:.5} = \
                 {ratio:.3}·σ_z (σ_z {:.5}), over the {LOCAL_SCALE_NOISE_PTP_SIGMAS}·σ_z pin. \
                 λ = 5·σ_z scales with the dispersion, so the spline reproduces the sample's \
                 own noise as a smooth surface (review R-T5-1); 0.92–2.18·σ_z when pinned.",
                hi - lo,
                r.sigma
            );
        }
    }

    #[test]
    fn too_few_matched_stars_leave_the_scale_global() {
        // 24 stars: above `MIN_MATCHES` (20), below
        // `LN_LOCAL_SCALE_MIN_STARS` (40).
        let stars: Vec<(f64, f64, f64)> = star_grid(12).into_iter().take(24).collect();
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 61);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 62);

        let r = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
            true,
        )
        .expect("24 stars still clear MIN_MATCHES");

        assert!(
            r.matches < LN_LOCAL_SCALE_MIN_STARS,
            "matched {} — the scene was meant to stay under the floor",
            r.matches
        );
        assert!(
            r.local.is_none(),
            "a sample under the floor must not produce a spline"
        );
    }
}
