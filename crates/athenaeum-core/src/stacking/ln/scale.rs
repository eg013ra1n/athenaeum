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

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, SQRT_2};
use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, warn};

use super::LnError;
use crate::geometry::kdtree::KdTree2;
use crate::geometry::{
    select_nodes, Pair, PixelMap, ThinPlateSpline, TPS_MAX_NODES, TPS_MIN_NODES,
};
use crate::stacking::psf_signal::{
    fit_stars, fit_stars_with_beta, FitOutcome, FitParams, PsfModel, Seed, StarFit,
};
use crate::stacking::register::detect::{detect_stars, Star, SATURATION};
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

/// Perf tier A Task 0 (audit §3.1): wall time (ms) inside each phase of one
/// [`relative_scale_against`] call — `detect_ms` ([`detect_seeds`] on the
/// TARGET), `fit_ms` ([`psf_signal::fit_stars_with_beta`] on the target),
/// `match_ms` (pairing — [`choose_pairing`]/[`ratio_sample`] — plus RCR and,
/// when asked for, [`fit_local_scale`], all together). `refine_ms` is
/// reserved for the centroid-refine LM step Task 3 splits out of
/// `detect_seeds`'s own detector call; it stays `0` until then. The four
/// never exceed the caller's own wall-clock measurement of the whole call —
/// they cover a subset of it, with the gap being the bookkeeping between
/// the timed sections (allocations, the fit-position `Vec` build, the
/// `MIN_MATCHES` check).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScaleTimings {
    pub detect_ms: u64,
    pub refine_ms: u64,
    pub fit_ms: u64,
    pub match_ms: u64,
}

#[derive(Debug, Clone)]
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
    /// Perf tier A Task 0: where this call's own wall time went — see
    /// [`ScaleTimings`]'s own doc.
    pub timings: ScaleTimings,
}

/// Hand-written (perf tier A Task 0): deliberately excludes `timings` —
/// wall-clock telemetry, not result data. Two calls that agree on every
/// other field (same stars, same matches, same scale) still measure
/// different real wall times run to run, so a derived `PartialEq` including
/// `timings` would make
/// `relative_scale_equals_relative_scale_against_a_prepared_reference`
/// (and any future caller comparing two otherwise-identical results) flaky.
impl PartialEq for ScaleResult {
    fn eq(&self, other: &Self) -> bool {
        self.scale == other.scale
            && self.sigma == other.sigma
            && self.matches == other.matches
            && self.rejected == other.rejected
            && self.beta == other.beta
            && self.pass == other.pass
            && self.local == other.local
    }
}

/// Detect star seeds on one plane: registration's own detector for
/// positions (its saturation/eccentricity/SNR cuts are exactly the ones a
/// reliable flux match wants), converted to fit seeds. Fits (not run
/// here) come back brightest-first because detection's own sort order is
/// preserved through `to_seed` and by `fit_stars`/`fit_stars_with_beta`.
/// Shared by the reference's own-model fit and the target's
/// reference-β fit in [`relative_scale`].
fn detect_seeds(
    data: &[f32],
    width: usize,
    height: usize,
    max_stars: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<Seed> {
    let cfg = DetectionConfig::default();
    let stars = detect_stars(data, width, height, &cfg, max_stars, pool);
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
///
/// `ref_signal`/`tgt_fwhm` (Tier C Task 2 diagnostics round, ruling C-11)
/// are ADDITIVE — populated alongside `ratios`/`positions`/`ref_idx` from
/// data `ratio_sample` already reads, never changing what those three
/// contain. They exist only so [`ScaleMatchDiag`] can report a per-pair
/// brightness/FWHM without a second pass over `pairs`.
struct RatioSample {
    ratios: Vec<f64>,
    positions: Vec<(f64, f64)>,
    ref_idx: Vec<usize>,
    ref_signal: Vec<f64>,
    tgt_fwhm: Vec<f64>,
}

/// `ref_fits` is the REFERENCE outcome's own fit list — Tier C ruling C-10:
/// the caller picks WHICH beta's reference fits to read (the default's, for
/// the detection fallback; the target's own, for [`relative_scale_from_fits`]),
/// so this function itself stays beta-agnostic — it only ever indexes `[r]`.
fn ratio_sample(
    ref_fits: &[StarFit],
    tgt_outcome: &FitOutcome,
    pairs: &[(usize, usize)],
) -> RatioSample {
    let mut out = RatioSample {
        ratios: Vec::with_capacity(pairs.len()),
        positions: Vec::with_capacity(pairs.len()),
        ref_idx: Vec::with_capacity(pairs.len()),
        ref_signal: Vec::with_capacity(pairs.len()),
        tgt_fwhm: Vec::with_capacity(pairs.len()),
    };
    for &(r, t) in pairs {
        let rf = &ref_fits[r];
        let (flux_ref, flux_tgt) = (rf.signal, tgt_outcome.fits[t].signal);
        if flux_ref > 0.0 && flux_tgt > 0.0 {
            out.ratios.push(flux_ref / flux_tgt);
            out.positions.push((rf.x, rf.y));
            out.ref_idx.push(r);
            out.ref_signal.push(rf.signal);
            out.tgt_fwhm.push(tgt_outcome.fits[t].fwhm());
        }
    }
    out
}

/// Diagnostic-only match record (Tier C Task 2 diagnostics round, ruling
/// C-11) — never read by `normalize_frame` or any other production caller.
/// One entry per pair `ratio_sample` accepted (`flux_ref > 0 && flux_tgt >
/// 0`), in the pairing's own order — exactly [`RatioSample`]'s own rows,
/// plus the matching [`crate::stacking::robust::RcrResult::kept`] flag.
#[derive(Debug, Clone, Copy)]
pub struct ScaleMatchDiag {
    /// The reference star's own fitted centroid, in reference-plane pixel
    /// coordinates — the join key an external caller uses to line up two
    /// different calls' matched sets (position, not index: two calls may
    /// resolve their reference fits via independent detect+fit passes, so
    /// index equality is not guaranteed even when the physical star is the
    /// same one).
    pub ref_x: f64,
    pub ref_y: f64,
    /// The reference fit's own `signal` (background-subtracted flux) — a
    /// stand-in for the star's brightness, for flux-decile binning.
    pub ref_signal: f64,
    /// `z_k = signal_ref / signal_target` — the exact ratio RCR sees.
    pub ratio: f64,
    /// Whether RCR kept this pair (index-aligned with `ratios` inside the
    /// ordinary call — the same `RcrResult.kept[k]`).
    pub kept: bool,
    /// The TARGET fit's own FWHM (`sqrt(fwhm_x * fwhm_y)`).
    pub target_fwhm: f64,
}

fn diag_from_sample(sample: &RatioSample, kept: &[bool]) -> Vec<ScaleMatchDiag> {
    (0..sample.ratios.len())
        .map(|k| ScaleMatchDiag {
            ref_x: sample.positions[k].0,
            ref_y: sample.positions[k].1,
            ref_signal: sample.ref_signal[k],
            ratio: sample.ratios[k],
            kept: kept.get(k).copied().unwrap_or(false),
            target_fwhm: sample.tgt_fwhm[k],
        })
        .collect()
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

/// One β's worth of prepared reference data — the fit outcome plus the
/// match tree over its own centroids (`fit_of_point` is the identity, kept
/// explicit so pass 1 and pass 2 share one [`pair_positions`]).
struct PreparedBeta {
    outcome: FitOutcome,
    tree: KdTree2,
    fit_of_point: Vec<usize>,
}

impl PreparedBeta {
    fn from_outcome(outcome: FitOutcome) -> PreparedBeta {
        let fit_positions: Vec<Option<(f64, f64)>> =
            outcome.fits.iter().map(|f| Some((f.x, f.y))).collect();
        let (tree, fit_of_point) = tree_over(&fit_positions);
        PreparedBeta {
            outcome,
            tree,
            fit_of_point,
        }
    }
}

/// The reference side of [`relative_scale`] (detection + PSF fit + the
/// built match tree), computed ONCE PER GROUP instead of once per frame
/// (final fix wave, I2): the LN reference plane is immutable for a group's
/// whole fan-out, but `relative_scale` used to re-detect and re-fit it on
/// EVERY call — `LnReferenceForDetection` (`ln/mod.rs`) already hoists the
/// group-level sanitized copy for this exact reason (fix round 1, item 6);
/// this hoists the far more expensive detect+fit+tree half that was left
/// behind.
///
/// **Tier C Task 2 fix round 1, ruling C-10** (superseded by fix round 2,
/// ruling C-12 — struct KEPT, its per-β use case retired): comparing a
/// target's own Moffat fit against a reference fitted at a DIFFERENT β
/// biases the flux ratio (two different profile shapes enclose different
/// fractions of the same star's light) — measured on a real catalog frame
/// at ≈ 3.4 % when the target's own β sat two `AUTO_BETAS` steps from the
/// group's. C-10's fix was to never compare across β at all: this struct
/// can hold the reference's fit ONE detection produces, refitted at EVERY
/// β a caller names via `extra_betas` (`betas`, keyed by `f64::to_bits()`
/// — every value in play is a small closed set of literals, `AUTO_BETAS`
/// or a caller's `Fixed`/`Moffat4` constant, never the result of
/// arithmetic, so bit equality is exact equality here), plus the DEFAULT β
/// (`normalization.local.psfModel`'s own resolution, ruling C-1) every
/// caller uses. **Ruling C-12 found the per-β comparison itself was never
/// the dominant bias** (the diagnostics round traced the real ~5 %
/// residual to a Moffat fit's `signal` not being warp-invariant) and
/// retired the flux-comparison design that needed per-β reference fits at
/// all — [`relative_scale_from_seeds`] always reads
/// [`Self::default_prepared`], regardless of what β the SEED source's own
/// fits carried. The multi-β machinery stays (a caller can still ask for
/// extra betas and look one up via `prepared_for`), but no current
/// production caller passes a non-empty `extra_betas` any more.
pub struct PreparedReferenceChannel {
    /// The β [`relative_scale_against`] (detection on the warped frame)
    /// always uses — `normalization.local.psfModel`'s resolution (ruling
    /// C-1: the group β when `Auto`).
    default_beta: f64,
    betas: HashMap<u64, PreparedBeta>,
    /// Ruling R-M4c-9's pass-2 side of the same reference: a tree over the
    /// DETECTION barycentres the DEFAULT-β accepted fits came from, with
    /// the map back to fit indices. There is only one — the barycentre
    /// pass is the detection fallback's own mechanism, which only ever
    /// compares at the default β (see [`relative_scale_against`]).
    barycentre_tree: KdTree2,
    barycentre_of_point: Vec<usize>,
}

impl PreparedReferenceChannel {
    /// `reference` is one channel's row-major `width × height` plane
    /// (already in the reference geometry); `default_psf`/`max_stars` are
    /// the SAME values a direct [`relative_scale`] call on this reference
    /// would use — `default_psf` resolves the DEFAULT β (ruling C-1).
    /// `extra_betas` (ruling C-10) are the group's OTHER member betas to
    /// ALSO prepare — a subset of `AUTO_BETAS`, so at most 4 total; a value
    /// already equal to the resolved default is not refitted twice. `pool`
    /// (perf tier 1 Task 2 fix round 1, item 3): the reference-side
    /// detect+fit this hoists once per group is real parallel work —
    /// routed to `image_pool` when the caller has one
    /// (`LnReferenceForDetection::build`, itself called from `run.rs` with
    /// `rc.ctx.image_pool` in scope), `None` from the probe/tests.
    pub fn build(
        reference: &[f32],
        width: usize,
        height: usize,
        default_psf: PsfModel,
        extra_betas: &[f64],
        max_stars: usize,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> PreparedReferenceChannel {
        let fit_params = FitParams::default();
        let ref_seeds = detect_seeds(reference, width, height, max_stars, pool);
        let default_outcome = fit_stars(
            reference,
            width,
            height,
            &ref_seeds,
            default_psf,
            &fit_params,
            pool,
        );
        let default_beta = default_outcome.beta;

        let barycentres = link_barycentres(&default_outcome.fits, &ref_seeds, &fit_params);
        let (barycentre_tree, barycentre_of_point) = tree_over(&barycentres);

        let mut betas: HashMap<u64, PreparedBeta> = HashMap::new();
        betas.insert(
            default_beta.to_bits(),
            PreparedBeta::from_outcome(default_outcome),
        );
        for &beta in extra_betas {
            if betas.contains_key(&beta.to_bits()) {
                continue;
            }
            let outcome = fit_stars_with_beta(
                reference,
                width,
                height,
                &ref_seeds,
                beta,
                &fit_params,
                pool,
            );
            betas.insert(beta.to_bits(), PreparedBeta::from_outcome(outcome));
        }

        PreparedReferenceChannel {
            default_beta,
            betas,
            barycentre_tree,
            barycentre_of_point,
        }
    }

    /// The DEFAULT β's prepared data — always present (`build` inserts it
    /// unconditionally).
    fn default_prepared(&self) -> &PreparedBeta {
        self.betas
            .get(&self.default_beta.to_bits())
            .expect("PreparedReferenceChannel::build always inserts the default beta")
    }

    /// This channel's default β — `normalization.local.psfModel`'s own
    /// resolution (ruling C-1).
    pub fn default_beta(&self) -> f64 {
        self.default_beta
    }

    /// The prepared data for `beta`, when it was one of `build`'s
    /// `extra_betas` (or equalled the default) — `None` for a β this
    /// reference was never fitted at.
    ///
    /// No current production caller: ruling C-12 retired the per-target-β
    /// lookup [`relative_scale_from_seeds`]'s predecessor used this for —
    /// the seeds design always reads [`Self::default_prepared`] instead.
    /// Kept (with a direct test) for `PreparedReferenceChannel`'s own
    /// completeness — the ruling kept `extra_betas`/`betas` themselves for
    /// the same reason.
    #[allow(dead_code)]
    fn prepared_for(&self, beta: f64) -> Option<&PreparedBeta> {
        self.betas.get(&beta.to_bits())
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
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<ScaleResult, LnError> {
    relative_scale_against_core(
        prepared,
        target,
        width,
        height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
        pool,
    )
    .map(|(r, _diag)| r)
}

/// Diagnostic-only (Tier C Task 2 diagnostics round, ruling C-11): exactly
/// [`relative_scale_against`]'s own computation — this and it share ONE
/// body ([`relative_scale_against_core`]) — but also returning the
/// per-match [`ScaleMatchDiag`] records, so an external caller (the C-11
/// probe extension) can compare which pairs two different calls kept
/// without re-implementing the detect/fit/pairing/RCR chain. Never called
/// by `normalize_frame` or any other production path.
#[allow(clippy::too_many_arguments)]
pub fn relative_scale_against_with_diag(
    prepared: &PreparedReferenceChannel,
    target: &[f32],
    width: usize,
    height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<(ScaleResult, Vec<ScaleMatchDiag>), LnError> {
    relative_scale_against_core(
        prepared,
        target,
        width,
        height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
        pool,
    )
}

#[allow(clippy::too_many_arguments)]
fn relative_scale_against_core(
    prepared: &PreparedReferenceChannel,
    target: &[f32],
    width: usize,
    height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<(ScaleResult, Vec<ScaleMatchDiag>), LnError> {
    let fit_params = FitParams::default();
    let default = prepared.default_prepared();

    // Perf tier A Task 0 (audit §3.1): wall time per phase, so the LN
    // stage's own `scale_ms` (ln/mod.rs) splits into where it actually
    // goes instead of staying one opaque number.
    let t = Instant::now();
    let tgt_seeds = detect_seeds(target, width, height, max_stars, pool);
    let detect_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    let tgt_outcome = fit_stars_with_beta(
        target,
        width,
        height,
        &tgt_seeds,
        default.outcome.beta,
        &fit_params,
        pool,
    );
    let fit_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    // Pass 1 on the PSF-fit centroids; pass 2 (ruling R-M4c-9) on the
    // DETECTION barycentres, only when pass 1 covered too little of the
    // TARGET's own fits (review R-T5-2) — `choose_pairing` owns the whole
    // rule, including the tie that keeps pass 1.
    let tgt_fit_positions: Vec<Option<(f64, f64)>> =
        tgt_outcome.fits.iter().map(|f| Some((f.x, f.y))).collect();
    let (pairs, pass) = choose_pairing(
        &default.tree,
        &default.fit_of_point,
        &tgt_fit_positions,
        &prepared.barycentre_tree,
        &prepared.barycentre_of_point,
        || link_barycentres(&tgt_outcome.fits, &tgt_seeds, &fit_params),
        match_radius_px,
    );

    let sample = ratio_sample(&default.outcome.fits, &tgt_outcome, &pairs);
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
    let match_ms = t.elapsed().as_millis() as u64;

    debug!(
        ln_scale = r.location,
        sigma = r.scale,
        ln_matches = sample.ratios.len(),
        rejected = r.rejected,
        ln_pass = pass,
        ln_local_nodes = local.as_ref().map_or(0, |s| s.nodes.len()),
        scale_source = "detected",
        "ln relative scale"
    );
    let diag = diag_from_sample(&sample, &r.kept);
    Ok((
        ScaleResult {
            scale: r.location,
            sigma: r.scale,
            matches: sample.ratios.len(),
            rejected: r.rejected,
            beta: default.outcome.beta,
            pass,
            local,
            timings: ScaleTimings {
                detect_ms,
                refine_ms: 0,
                fit_ms,
                match_ms,
            },
        },
        diag,
    ))
}

/// Maps one Measure [`StarFit`]'s CENTROID (position only) through `map`
/// into the reference geometry — ruling C-12 (Tier C Task 2 fix round 2)
/// drops the earlier `|det J|` flux correction entirely along with the
/// native-flux comparison it existed for (see
/// [`relative_scale_from_seeds`]'s own doc): Measure's fits are SEEDS now,
/// not a source of flux to correct and compare. `None` when the mapped
/// centroid, rounded to the nearest pixel (the same rounding
/// [`psf_signal::fit_one`] applies to a seed internally), falls outside
/// `[0, ref_width) × [0, ref_height)`.
fn map_seed_position(
    fit: &StarFit,
    map: &PixelMap,
    ref_width: usize,
    ref_height: usize,
) -> Option<(f64, f64)> {
    let (mx, my) = map.forward_exact(fit.x, fit.y);
    let (px, py) = (mx.round(), my.round());
    if !(px >= 0.0 && py >= 0.0 && (px as usize) < ref_width && (py as usize) < ref_height) {
        return None;
    }
    Some((mx, my))
}

/// Ruling C-12's saturation guard: the ONE piece of
/// [`register::detect::passes_register_cuts`]'s job this path still needs,
/// applied to the WARPED plane's own pixel value at a mapped seed position
/// (native `[0, 1]` units — [`SATURATION`]) instead of a Measure fit's
/// NATIVE `background + amplitude`. Measure's own acceptance already
/// covers eccentricity/SNR on the native frame — what it cannot know is
/// whether the WARPED pixel at the mapped position is unusable (a
/// registration shift walking a seed onto a brighter neighbour, or
/// resampling landing on a hot/bad pixel), which would corrupt the fresh
/// PSF fit about to run there. `px`/`py` are assumed already in-bounds
/// (every caller gets them from [`map_seed_position`] first).
fn warped_pixel_is_saturated(target_plane: &[f32], ref_width: usize, px: f64, py: f64) -> bool {
    let value = target_plane[(py as usize) * ref_width + (px as usize)];
    !(value.is_finite() && (value as f64) < SATURATION as f64)
}

/// Ruling C-2: [`relative_scale_from_seeds`] has no detection barycentre to
/// fall back on (the seeds are Measure's own mapped positions, not a fresh
/// detection's), so pass 2 is a SECOND [`pair_positions`] query on the SAME
/// fitted positions against the SAME reference tree, at `2 × radius` — the
/// wider-radius pass literally REPLACES the barycentre pass of
/// [`choose_pairing`]; [`LN_BARYCENTRE_PASS_THRESHOLD`] keeps its name and
/// value, and a tie keeps pass 1, exactly as [`choose_pairing`] itself.
fn choose_pairing_widened(
    ref_tree: &KdTree2,
    ref_fit_of_point: &[usize],
    tgt_positions: &[Option<(f64, f64)>],
    radius: f64,
) -> (Vec<(usize, usize)>, u8) {
    let pass1 = pair_positions(ref_tree, ref_fit_of_point, tgt_positions, radius);
    let tgt_fits = tgt_positions.len();
    if tgt_fits == 0 || pass1.len() as f64 >= LN_BARYCENTRE_PASS_THRESHOLD * tgt_fits as f64 {
        return (pass1, 1);
    }
    let pass2 = pair_positions(ref_tree, ref_fit_of_point, tgt_positions, 2.0 * radius);
    if pass2.len() > pass1.len() {
        (pass2, 2)
    } else {
        (pass1, 1)
    }
}

/// LN's flux-ratio scale computed from Measure's OWN persisted fits, used
/// as SEED POSITIONS ONLY (Tier C Task 2 fix round 2, ruling C-12).
///
/// **Why not compare Measure's fitted flux directly (the original Task 2
/// design)**: the diagnostics round (ruling C-11) found the ~5 % residual
/// that survived ruling C-10's per-β fix traces to the Moffat fit's own
/// `signal` NOT being warp-invariant — a fit on the NATIVE calibrated frame
/// and a fit of the IDENTICAL star on the WARPED (bicubic-interpolated)
/// frame integrate measurably different flux, the gap growing with how
/// undersampled the star's native PSF is relative to the resampling
/// kernel (0.5 % on well-sampled frames, up to 19-20 % on the sharpest
/// real frames measured). An aperture SUM over the same pixels — no fit
/// model — conserves flux through the warp to ≤ 0.6 % even on those same
/// frames, so the divergence is in the FIT, not the pixels. Today's
/// [`relative_scale_against`] never hits this: both sides it compares are
/// fits taken on a plane of the SAME kind (the reference is itself an
/// integration of already-warped frames; the target is warped too), which
/// is why it stays the correct, unbiased baseline this function is
/// checked against.
///
/// **What this function does instead**: it keeps Measure's cheap per-plane
/// detection (skipping [`detect_seeds`]'s own expensive full-frame search)
/// but discards every FITTED VALUE, using only the accepted stars'
/// POSITIONS as seeds for a fresh [`fit_stars_with_beta`] call on the
/// WARPED TARGET PLANE — exactly [`relative_scale_against`]'s own fit
/// call, fed a pre-selected seed list instead of [`detect_seeds`]'s
/// full-frame one. The expensive full-frame detection disappears; the PSF
/// fit itself (now over a much smaller, pre-matched list) and the
/// pairing/RCR/local-scale tail are the SAME code [`relative_scale_against`]
/// runs.
///
/// `fits` is Measure's accepted [`StarFit`]s for ONE plane of the frame —
/// their POSITIONS only matter; their own β/flux never reach the result.
/// `map` is the frame's registration (subject → reference); `target_plane`
/// is the WARPED target plane (already in the reference geometry — the
/// plane the caller is about to fit on) that both the saturation guard and
/// the PSF fit itself read; `ref_width`/`ref_height` are its dimensions.
///
/// Per fit: (1) [`map_seed_position`] — map the centroid through
/// [`PixelMap::forward_exact`] (ruling R-T4-3), dropping one whose mapped,
/// rounded position falls outside `[0, ref_width) × [0, ref_height)`; (2)
/// [`warped_pixel_is_saturated`] — a saturation guard on the WARPED
/// plane's own pixel value at that position (native `[0, 1]` units) —
/// this is the one piece of the OLD design's
/// [`register::detect::passes_register_cuts`] this path still needs
/// (Measure's own acceptance already covers eccentricity/SNR on the
/// NATIVE frame, but says nothing about the WARPED pixel the fresh fit is
/// about to sample); (3) PRE-SELECT — keep only seeds with a reference
/// star within `match_radius_px` of the mapped, UNFITTED position
/// (`prepared`'s own match tree at the reference's DEFAULT β, cheap, no
/// fit yet). **Fix round 3, ruling C-13, LEVER 1 measured this step
/// directly** ([`SeedFilterBreakdown`]): it is the dominant nominal loss
/// on real frames (24-44% of Measure's own in-coverage, unsaturated
/// seeds) — but ALSO measured that relaxing it (widening to `2 ×
/// match_radius_px`) or removing it (fitting every in-coverage,
/// unsaturated seed and letting [`choose_pairing_widened`] decide) leaves
/// `matched` and `scale` UNCHANGED while costing real time — the lost
/// stars have no reference counterpart within either radius at all, so
/// this filter is kept at `match_radius_px`, the cheapest of the three
/// variants measured.
///
/// Survivors become [`Seed`]s (`peak`/`flux` copied from the Measure fit's
/// own `amplitude`/`signal` — used only by [`fit_stars_with_beta`]'s
/// field-level `initial_sigma` heuristic to size the fit stamp, a RATIO
/// any unit difference between Measure's ADU-scaled fits and this native
/// plane cancels out of), sorted brightest-first (the convention
/// [`detect_seeds`]'s own `to_seed` documents), capped at `max_stars`, then
/// fitted with [`fit_stars_with_beta`] at the reference's DEFAULT β —
/// never a seed's own original Measure β, which no longer matters: this is
/// a FRESH fit on the warped plane, at whatever β makes it comparable to
/// the reference's own fit, exactly like [`relative_scale_against`].
/// `Err(TooFewMatches)` when fewer than [`MIN_MATCHES`] pairs survive —
/// the caller falls back to full detection.
///
/// Pairing is [`choose_pairing_widened`] (ruling C-2 — there is still no
/// detection barycentre to fall back on: the seeds are Measure's own
/// positions, not a fresh detection's). [`ratio_sample`] /
/// [`crate::stacking::robust::rcr`] / [`fit_local_scale`] run UNCHANGED.
#[allow(clippy::too_many_arguments)]
pub fn relative_scale_from_seeds(
    prepared: &PreparedReferenceChannel,
    fits: &[StarFit],
    map: &PixelMap,
    target_plane: &[f32],
    ref_width: usize,
    ref_height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<ScaleResult, LnError> {
    relative_scale_from_seeds_core(
        prepared,
        fits,
        map,
        target_plane,
        ref_width,
        ref_height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
        pool,
    )
    .map(|(r, _diag, _breakdown)| r)
}

/// Fix round 3, ruling C-13, LEVER 1's own instrument: a per-STAGE count of
/// [`relative_scale_from_seeds`]'s filter chain, so a caller can see WHICH
/// step is responsible for the gap between Measure's own fit count and the
/// number that ends up matched — never called by `normalize_frame` or any
/// other production path. `available` is `fits.len()`; each later field
/// counts survivors of one more step, in the SAME order the function itself
/// applies them: `in_coverage` (the mapped, rounded position falls inside
/// the reference canvas), `past_saturation` (the warped plane's own pixel
/// value at that position clears [`SATURATION`]), `past_preselect` (a
/// reference star sits within `match_radius_px` of the mapped, UNFITTED
/// position, BEFORE the `max_stars` cap — ruling C-13, LEVER 1 measured
/// this as the dominant nominal loss, 24-44% of real frames' in-coverage,
/// unsaturated seeds, but ALSO measured that relaxing or removing it does
/// not recover those stars — `matched` and `scale` came back unchanged
/// whether this filter ran at `match_radius_px`, at `2 ×` it, or not at
/// all, so the filter is kept as the cheapest of the three; the lost stars
/// have no reference counterpart within either radius, full stop),
/// `fit_accepted` (`fit_stars_with_beta` produced a `StarFit` for that
/// seed — a seed can still be dropped here: the admission check in
/// `psf_signal::fit_one`, a fit that failed to converge, or `dedupe`
/// collapsing two seeds that walked onto the same star), `matched` (the
/// pairing + `ratio_sample` step — [`ScaleResult::matches`] is exactly
/// this number).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedFilterBreakdown {
    pub available: usize,
    pub in_coverage: usize,
    pub past_saturation: usize,
    pub past_preselect: usize,
    pub fit_accepted: usize,
    pub matched: usize,
}

/// Diagnostic-only (ruling C-13, LEVER 1): exactly
/// [`relative_scale_from_seeds`]'s own computation, plus the
/// [`SeedFilterBreakdown`] the `_core` body already tallies for free.
#[allow(clippy::too_many_arguments)]
pub fn relative_scale_from_seeds_with_breakdown(
    prepared: &PreparedReferenceChannel,
    fits: &[StarFit],
    map: &PixelMap,
    target_plane: &[f32],
    ref_width: usize,
    ref_height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<(ScaleResult, SeedFilterBreakdown), LnError> {
    relative_scale_from_seeds_core(
        prepared,
        fits,
        map,
        target_plane,
        ref_width,
        ref_height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
        pool,
    )
    .map(|(r, _diag, breakdown)| (r, breakdown))
}

/// Diagnostic-only (mirrors [`relative_scale_against_with_diag`]'s own
/// contract): exactly [`relative_scale_from_seeds`]'s own computation —
/// this and it share ONE body ([`relative_scale_from_seeds_core`]) — but
/// also returning the per-match [`ScaleMatchDiag`] records. Never called
/// by `normalize_frame` or any other production path.
#[allow(clippy::too_many_arguments)]
pub fn relative_scale_from_seeds_with_diag(
    prepared: &PreparedReferenceChannel,
    fits: &[StarFit],
    map: &PixelMap,
    target_plane: &[f32],
    ref_width: usize,
    ref_height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<(ScaleResult, Vec<ScaleMatchDiag>), LnError> {
    relative_scale_from_seeds_core(
        prepared,
        fits,
        map,
        target_plane,
        ref_width,
        ref_height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
        pool,
    )
    .map(|(r, diag, _breakdown)| (r, diag))
}

#[allow(clippy::too_many_arguments)]
fn relative_scale_from_seeds_core(
    prepared: &PreparedReferenceChannel,
    fits: &[StarFit],
    map: &PixelMap,
    target_plane: &[f32],
    ref_width: usize,
    ref_height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
    local_scale: bool,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<(ScaleResult, Vec<ScaleMatchDiag>, SeedFilterBreakdown), LnError> {
    let default = prepared.default_prepared();
    let fit_params = FitParams::default();
    let mut breakdown = SeedFilterBreakdown {
        available: fits.len(),
        ..Default::default()
    };

    // Perf tier A Task 0 convention: this pre-select loop is this path's
    // own analog of `detect_seeds` (it is what decides WHAT gets fitted,
    // far cheaper than a full-frame detection) — timed into `detect_ms`
    // for the same log field the detection path reports.
    //
    // Fix round 3, ruling C-13, LEVER 1: `SeedFilterBreakdown` (below) DOES
    // show this pre-select step (a reference star within `match_radius_px`
    // of the mapped, UNFITTED seed) as the dominant nominal loss — 24-44%
    // of Measure's own in-coverage, unsaturated fits on real catalog
    // frames, dwarfing coverage (<1%) and saturation (0% measured)
    // combined. But relaxing it does NOT recover those stars: measured on
    // the real catalog, BOTH removing this filter entirely (fit every
    // in-coverage, unsaturated seed) AND widening it to `2 × match_radius_
    // px` left `matched` and the RCR-averaged `scale` UNCHANGED to three
    // significant figures on every frame tried, while costing real time —
    // fitting 16502 candidates instead of 9198 on frame 29053 moved
    // `scale_ms` from ≈353 ms to 588 ms for zero benefit. The lost stars
    // are not lost to an overly tight RADIUS; their mapped positions
    // genuinely have no reference counterpart within either radius (a
    // registration/centroid gap the fit's own `centroid_tolerance_px`
    // cannot bridge, or a Measure detection the deeper, integrated
    // reference never resolved as a stable star in the first place) — so
    // this filter is kept AS IS, at `match_radius_px`, being the cheapest
    // of the three measured variants and no less accurate than either.
    let t = Instant::now();
    let mut seeds: Vec<Seed> = Vec::with_capacity(fits.len());
    for f in fits {
        let Some((mx, my)) = map_seed_position(f, map, ref_width, ref_height) else {
            continue;
        };
        breakdown.in_coverage += 1;
        if warped_pixel_is_saturated(target_plane, ref_width, mx.round(), my.round()) {
            continue;
        }
        breakdown.past_saturation += 1;
        if default
            .tree
            .nearest_within(mx, my, match_radius_px)
            .is_none()
        {
            continue;
        }
        breakdown.past_preselect += 1;
        seeds.push(Seed {
            x: mx,
            y: my,
            peak: f.amplitude,
            flux: f.signal,
        });
    }
    seeds.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    seeds.truncate(max_stars);
    let detect_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    let tgt_outcome = fit_stars_with_beta(
        target_plane,
        ref_width,
        ref_height,
        &seeds,
        default.outcome.beta,
        &fit_params,
        pool,
    );
    breakdown.fit_accepted = tgt_outcome.fits.len();
    let fit_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    let tgt_fit_positions: Vec<Option<(f64, f64)>> =
        tgt_outcome.fits.iter().map(|f| Some((f.x, f.y))).collect();
    let (pairs, pass) = choose_pairing_widened(
        &default.tree,
        &default.fit_of_point,
        &tgt_fit_positions,
        match_radius_px,
    );

    let sample = ratio_sample(&default.outcome.fits, &tgt_outcome, &pairs);
    breakdown.matched = sample.ratios.len();
    if sample.ratios.len() < MIN_MATCHES {
        return Err(LnError::TooFewMatches {
            matches: sample.ratios.len(),
        });
    }

    let r = crate::stacking::robust::rcr(&sample.ratios, rcr_limit);
    let local = if local_scale {
        fit_local_scale(&sample, &r.kept, r.location, r.scale, ref_width, ref_height)
    } else {
        None
    };
    let match_ms = t.elapsed().as_millis() as u64;

    debug!(
        ln_scale = r.location,
        sigma = r.scale,
        ln_matches = sample.ratios.len(),
        rejected = r.rejected,
        ln_pass = pass,
        ln_local_nodes = local.as_ref().map_or(0, |s| s.nodes.len()),
        scale_source = "seeds",
        "ln relative scale"
    );
    let diag = diag_from_sample(&sample, &r.kept);
    Ok((
        ScaleResult {
            scale: r.location,
            sigma: r.scale,
            matches: sample.ratios.len(),
            rejected: r.rejected,
            beta: default.outcome.beta,
            pass,
            local,
            timings: ScaleTimings {
                detect_ms,
                refine_ms: 0,
                fit_ms,
                match_ms,
            },
        },
        diag,
        breakdown,
    ))
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
    let prepared =
        PreparedReferenceChannel::build(reference, width, height, psf, &[], max_stars, None);
    relative_scale_against(
        &prepared,
        target,
        width,
        height,
        max_stars,
        match_radius_px,
        rcr_limit,
        local_scale,
        None,
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

        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );
        let via_prepared = relative_scale_against(
            &prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("the same prepared reference must match the same target");

        assert_eq!(via_wrapper, via_prepared);
    }

    /// Perf tier A Task 0 (audit §3.1): `detect_ms + refine_ms + fit_ms +
    /// match_ms` must never exceed the caller's OWN wall-clock measurement
    /// of the whole `relative_scale_against` call — `floor(a) + floor(b) <=
    /// floor(a + b)` for any nonnegative reals, extended to four terms, so
    /// this holds by construction as long as every timed section falls
    /// inside the outer measurement window (which it does: the outer timer
    /// starts before the first inner one and stops after the last). No
    /// tolerance needed. `refine_ms` stays `0` until Task 3 splits the
    /// centroid-refine LM step out of `detect_seeds`.
    #[test]
    fn scale_timings_never_exceed_the_callers_own_wall_measurement() {
        let stars = star_grid(4);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 14);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 24);

        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );

        let t = std::time::Instant::now();
        let r = relative_scale_against(
            &prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("a clean uniformly-scaled field must match");
        let wall_ms = t.elapsed().as_millis() as u64;

        let sum =
            r.timings.detect_ms + r.timings.refine_ms + r.timings.fit_ms + r.timings.match_ms;
        assert!(
            sum <= wall_ms,
            "timings sum {sum} exceeds the caller's own wall time {wall_ms} \
             (detect={} refine={} fit={} match={})",
            r.timings.detect_ms,
            r.timings.refine_ms,
            r.timings.fit_ms,
            r.timings.match_ms
        );
        assert_eq!(
            r.timings.refine_ms, 0,
            "refine_ms stays 0 until Task 3 splits the centroid-refine LM step out of detection"
        );
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
        let ref_seeds = detect_seeds(&reference, WIDTH, HEIGHT, 200, None);
        let ref_out = fit_stars(
            &reference,
            WIDTH,
            HEIGHT,
            &ref_seeds,
            PsfModel::Auto,
            &FitParams::default(),
            None,
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
            // Unused by `fit_local_scale` — diagnostic-only fields (ruling
            // C-11), placeholders here.
            ref_signal: vec![0.0; 60],
            tgt_fwhm: vec![0.0; 60],
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
            ref_signal: vec![0.0; 60],
            tgt_fwhm: vec![0.0; 60],
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

    /// **R-T5-1 control pin.** The gradient test above runs at the
    /// module's quiet `NOISE = 0.002`, which puts the ratio dispersion at
    /// σ_z ≈ 0.014 — a real but small sample noise, well under the
    /// ≈ 0.03 the review asked for (that is what this test's own `LOUD =
    /// 0.006` reaches) — and its true surface dominates whatever the
    /// sample noise contributes, so it cannot see what the review found by
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
    /// maximum. The test itself loops over the FIRST 3 of those seeds
    /// (the range is what the measurement covered, not what runs on every
    /// `cargo test`; each seed is a full detect-fit-RCR-spline pass).
    /// For scale, the gradient test's own REAL structure runs at
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

    // ---- Tier C Task 2: LN from Measure's fits ---------------------------

    use crate::geometry::{Linear, LinearKind};
    use crate::stacking::measure::ADU_SCALE;

    /// "Measure-like" fits for `target`: ADU-scaled `StarFit`s produced the
    /// same way `measure::measure_plane_with_seeds` would (detect on the
    /// NATIVE plane for positions, fit on an ADU-scaled copy for the actual
    /// amplitude/background/signal values) — a stand-in for a real `.athf`
    /// artifact this test module has no fixture run to produce one from.
    fn measure_like_fits(target: &[f32], w: usize, h: usize, beta: f64) -> Vec<StarFit> {
        let seeds = detect_seeds(target, w, h, 200, None);
        let scaled: Vec<f32> = target.iter().map(|&v| v * ADU_SCALE).collect();
        fit_stars_with_beta(&scaled, w, h, &seeds, beta, &FitParams::default(), None).fits
    }

    fn identity_map() -> PixelMap {
        PixelMap::linear(Linear::identity()).expect("identity must invert")
    }

    fn similarity_map(scale: f64) -> PixelMap {
        let linear = Linear {
            kind: LinearKind::Similarity,
            m: [[scale, 0.0, 0.0], [0.0, scale, 0.0], [0.0, 0.0, 1.0]],
        };
        PixelMap::linear(linear).expect("a non-degenerate similarity must invert")
    }

    /// Ruling C-12's own "positions-mapping" pin, replacing the retired
    /// Jacobian-correction one (`map_fit_corrects_signal_by_the_local_
    /// jacobian_determinant`, gone with `map_fit` itself): a seed's
    /// centroid maps through an arbitrary linear map exactly like any
    /// other pixel coordinate — there is no flux to correct any more — and
    /// one whose mapped, rounded position falls outside the reference
    /// canvas is dropped.
    #[test]
    fn map_seed_position_maps_through_a_similarity_and_drops_out_of_bounds() {
        let fit = fit_at(200.0, 150.0);

        let identity = identity_map();
        let (mx, my) =
            map_seed_position(&fit, &identity, WIDTH, HEIGHT).expect("must map inside bounds");
        assert_eq!(mx, fit.x);
        assert_eq!(my, fit.y);

        let sim = similarity_map(1.2);
        let (mx, my) =
            map_seed_position(&fit, &sim, WIDTH, HEIGHT).expect("must map inside bounds");
        assert!((mx - fit.x * 1.2).abs() < 1e-9);
        assert!((my - fit.y * 1.2).abs() < 1e-9);

        let far = fit_at(490.0, 370.0);
        assert!(
            map_seed_position(&far, &sim, WIDTH, HEIGHT).is_none(),
            "490*1.2=588 > WIDTH must be dropped"
        );
    }

    /// The saturation guard ruling C-12 keeps from the retired
    /// `passes_register_cuts` call: it reads the WARPED plane's own pixel
    /// value at the mapped position (native `[0, 1]` units,
    /// `SATURATION = 0.95`), not anything carried on the source `StarFit`
    /// — a non-finite pixel (a warped frame's off-canvas strip) is treated
    /// as unusable too.
    #[test]
    fn warped_pixel_is_saturated_reads_the_target_planes_own_value() {
        let mut plane = vec![0.1f32; WIDTH * HEIGHT];
        plane[150 * WIDTH + 200] = 0.99; // above SATURATION (0.95)
        plane[151 * WIDTH + 200] = f32::NAN;
        assert!(warped_pixel_is_saturated(&plane, WIDTH, 200.0, 150.0));
        assert!(
            warped_pixel_is_saturated(&plane, WIDTH, 200.0, 151.0),
            "a non-finite pixel must also be treated as unusable"
        );
        assert!(!warped_pixel_is_saturated(&plane, WIDTH, 199.0, 150.0));
    }

    /// Ruling C-2: `choose_pairing_widened` fires its second, wider pass
    /// only when pass 1 covers less than [`LN_BARYCENTRE_PASS_THRESHOLD`] of
    /// the mapped fits, and the larger pairing wins — the same contract
    /// [`choose_pairing`] has for its own (barycentre) second pass, minus
    /// the barycentre tree this path has none of.
    #[test]
    fn choose_pairing_widened_runs_pass_two_when_pass_one_covers_too_little() {
        let grid = grid_positions(51);
        let ref_fits = present(&grid);
        let (ref_tree, ref_map) = tree_over(&ref_fits);

        // 5 px is outside the 4 px window (emptying pass 1) but well inside
        // the widened 8 px one — same displacement the barycentre-pass test
        // above uses, for the same reason.
        let walked: Vec<(f64, f64)> = grid.iter().map(|&(x, y)| (x + 5.0, y)).collect();
        let tgt = present(&walked);

        let pass1 = pair_positions(&ref_tree, &ref_map, &tgt, 4.0);
        assert!(
            (pass1.len() as f64) < LN_BARYCENTRE_PASS_THRESHOLD * tgt.len() as f64,
            "pass 1 matched {} of {} — the widened pass would not even run",
            pass1.len(),
            tgt.len()
        );

        let (pairs, pass) = choose_pairing_widened(&ref_tree, &ref_map, &tgt, 4.0);
        assert_eq!(pass, 2, "the widened pairing must win");
        assert!(
            pairs.len() as f64 >= 0.9 * grid.len() as f64,
            "pass 2 matched {} of {}",
            pairs.len(),
            grid.len()
        );
    }

    #[test]
    fn choose_pairing_widened_keeps_pass_one_when_coverage_is_already_high() {
        let grid = grid_positions(52);
        let ref_fits = present(&grid);
        let (ref_tree, ref_map) = tree_over(&ref_fits);
        let walked: Vec<(f64, f64)> = grid.iter().map(|&(x, y)| (x + 2.5, y)).collect();
        let tgt = present(&walked);

        let (pairs, pass) = choose_pairing_widened(&ref_tree, &ref_map, &tgt, 4.0);
        assert_eq!(pass, 1);
        assert_eq!(pairs.len(), grid.len());
    }

    #[test]
    fn choose_pairing_widened_ties_keep_pass_one() {
        let grid = grid_positions(53);
        let half = grid.len() / 2;
        let mut tgt_positions: Vec<(f64, f64)> = grid[..half].to_vec();
        tgt_positions.extend(grid[half..].iter().map(|&(_, y)| (-1000.0, y)));
        let ref_fits = present(&grid);
        let (ref_tree, ref_map) = tree_over(&ref_fits);
        let tgt = present(&tgt_positions);

        let (pairs, pass) = choose_pairing_widened(&ref_tree, &ref_map, &tgt, 4.0);
        assert!(
            (half as f64) < LN_BARYCENTRE_PASS_THRESHOLD * tgt.len() as f64,
            "the scene must put pass 1 under the threshold"
        );
        assert_eq!(pass, 1, "a tie (same positions widened) must keep pass 1");
        assert_eq!(pairs.len(), half);
    }

    /// DELTA pin (design §8's own acceptance metric): on a clean uniformly-
    /// scaled field, `relative_scale_from_seeds` fed Measure-like fits
    /// (used as seed positions only) must agree with the detection oracle
    /// (`relative_scale_against`, run on the identical two rendered planes)
    /// within 0.5%, matching at least 80% as many stars.
    #[test]
    fn relative_scale_from_seeds_matches_the_detection_oracle_within_half_a_percent() {
        let stars = star_grid(31);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 71);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 81);

        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );

        let oracle = relative_scale_against(
            &prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("the detection oracle must match a clean uniformly-scaled field");

        let fits = measure_like_fits(&target, WIDTH, HEIGHT, prepared.default_beta());
        let identity = identity_map();
        let from_seeds = relative_scale_from_seeds(
            &prepared, &fits, &identity, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("seeds from Measure-like fits on the same field must also match");

        let rel = (from_seeds.scale - oracle.scale).abs() / oracle.scale.abs();
        assert!(
            rel < 0.005,
            "scale {} vs oracle {} ({:.4}% off)",
            from_seeds.scale,
            oracle.scale,
            rel * 100.0
        );
        assert!(
            from_seeds.matches as f64 >= 0.8 * oracle.matches as f64,
            "matched {} vs oracle's {}",
            from_seeds.matches,
            oracle.matches
        );
    }

    #[test]
    fn relative_scale_from_seeds_reports_scale_source_seeds() {
        // A dedicated tracing capture would need this module's own
        // subscriber (register/detect.rs's own tests already show the
        // pattern); simplest to just confirm the call succeeds and returns
        // a sane result — `scale_source` on the emitted event is a
        // constant literal ("seeds") checked by inspection, not re-derived
        // at runtime by anything this function returns.
        let stars = star_grid(32);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 72);
        let target_stars = scale_stars(&stars, 1.1);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 82);
        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );
        let fits = measure_like_fits(&target, WIDTH, HEIGHT, prepared.default_beta());
        let identity = identity_map();
        let r = relative_scale_from_seeds(
            &prepared, &fits, &identity, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("a clean field must match");
        assert!((r.scale - 1.0 / 1.1).abs() < 0.02, "scale {}", r.scale);
    }

    /// **Ruling C-12 (Tier C Task 2 fix round 2)**, the "per-β test becomes
    /// a 'seeds at one β' test" — this test replaces
    /// `a_target_beta_far_from_the_references_default_is_no_longer_biased`
    /// (ruling C-10's own pin). The diagnostics round (ruling C-11) found
    /// C-10's per-β reference fix never closed the real ~5% residual — the
    /// actual cause was a Moffat fit's `signal` not being warp-invariant —
    /// so `relative_scale_from_seeds` now throws the source fits' own β
    /// away entirely and always re-fits at the reference's DEFAULT β on
    /// the warped plane. Two source fit lists that differ ONLY in their
    /// own β (10.0 vs 4.0 — the exact mismatch C-10 needed a per-β
    /// reference to fix) must therefore produce numerically IDENTICAL
    /// results: a seed carries position only, so the source fit's β is
    /// invisible to this function. `PreparedReferenceChannel::build` is
    /// still exercised at a non-default β here — ruling C-12's own "keep
    /// C-10's structure" — even though no production caller passes a
    /// non-empty `extra_betas` any more; `prepared_for` has no caller
    /// outside this test.
    #[test]
    fn seeds_at_one_beta_are_immune_to_the_sources_own_beta() {
        let stars = star_grid(34);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 91);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 92);

        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Fixed(10.0),
            &[4.0],
            200,
            None,
        );
        assert!(
            prepared.prepared_for(4.0).is_some(),
            "extra_betas must still be reachable"
        );
        assert!(prepared.prepared_for(999.0).is_none());

        let identity = identity_map();
        let seeds_at_10 = measure_like_fits(&target, WIDTH, HEIGHT, 10.0);
        let seeds_at_4 = measure_like_fits(&target, WIDTH, HEIGHT, 4.0);

        let from_10 = relative_scale_from_seeds(
            &prepared,
            &seeds_at_10,
            &identity,
            &target,
            WIDTH,
            HEIGHT,
            200,
            4.0,
            0.3,
            false,
            None,
        )
        .expect("matched-beta seeds must succeed");
        let from_4 = relative_scale_from_seeds(
            &prepared,
            &seeds_at_4,
            &identity,
            &target,
            WIDTH,
            HEIGHT,
            200,
            4.0,
            0.3,
            false,
            None,
        )
        .expect("a source beta far from the default must no longer matter");

        // Not `< 1e-6`: `measure_like_fits` at β 10 vs β 4 converge to
        // slightly different CENTROIDS for the same pixel data (a
        // different assumed profile shape shifts the least-squares
        // optimum by a tiny amount), so the two seed lists this feeds
        // `relative_scale_from_seeds` are not bit-identical positions —
        // measured at ≈ 8.8e-5 relative on this fixture. `1e-3` is three
        // orders of magnitude above that measurement and four below the
        // several-percent bias ruling C-10 needed a per-β reference to
        // fix, so it is still a real "the source beta no longer matters"
        // pin, not a loosened one.
        let rel = (from_4.scale - from_10.scale).abs() / from_10.scale.abs();
        assert!(
            rel < 1e-3,
            "the source fits' own beta must be invisible to the seeds path: {:.9}",
            rel
        );
        assert_eq!(
            from_10.beta, 10.0,
            "the fit always runs at the reference's own default beta"
        );
        assert_eq!(from_4.beta, 10.0);
    }

    #[test]
    fn relative_scale_from_seeds_is_too_few_matches_on_an_empty_list() {
        let stars = star_grid(33);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 73);
        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );
        let identity = identity_map();
        let target = vec![0.0f32; WIDTH * HEIGHT];
        let err = relative_scale_from_seeds(
            &prepared,
            &[],
            &identity,
            &target,
            WIDTH,
            HEIGHT,
            200,
            4.0,
            0.3,
            false,
            None,
        )
        .expect_err("no fits at all cannot produce 20 matched pairs");
        assert!(matches!(err, LnError::TooFewMatches { matches: 0 }));
    }

    /// The `_with_diag` variants must be bit-for-bit the SAME computation
    /// as the plain ones (they share one `_core` body) — this pins that
    /// refactor, not any new behavior. Also checks the diag vec's own
    /// internal consistency: its length is the pre-RCR match count, and
    /// the number of `kept` entries is `matches - rejected`.
    #[test]
    fn with_diag_variants_match_the_plain_ones_and_the_diag_vec_is_self_consistent() {
        let stars = star_grid(35);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 93);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 94);
        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );

        let plain = relative_scale_against(
            &prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("a clean uniformly-scaled field must match");
        let (diag_result, diag) = relative_scale_against_with_diag(
            &prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("the diag variant must succeed identically");
        assert_eq!(plain, diag_result, "with_diag must not change the result");
        assert_eq!(diag.len(), plain.matches);
        assert_eq!(
            diag.iter().filter(|d| d.kept).count(),
            plain.matches - plain.rejected
        );

        let fits = measure_like_fits(&target, WIDTH, HEIGHT, prepared.default_beta());
        let identity = identity_map();
        let plain_seeds = relative_scale_from_seeds(
            &prepared, &fits, &identity, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("seeds must match");
        let (diag_seeds_result, diag_seeds) = relative_scale_from_seeds_with_diag(
            &prepared, &fits, &identity, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("the diag variant must succeed identically");
        assert_eq!(plain_seeds, diag_seeds_result);
        assert_eq!(diag_seeds.len(), plain_seeds.matches);
        assert_eq!(
            diag_seeds.iter().filter(|d| d.kept).count(),
            plain_seeds.matches - plain_seeds.rejected
        );
    }

    /// Fix round 3, ruling C-13, LEVER 1: [`SeedFilterBreakdown`]'s counts
    /// must be monotonically non-increasing through the filter chain, its
    /// `available` must equal the input length, and its `matched` must
    /// equal the plain call's own `matches` — on a clean field every seed
    /// clears every gate, so every stage should read the SAME count.
    #[test]
    fn seed_filter_breakdown_counts_the_whole_chain_and_matches_the_plain_result() {
        let stars = star_grid(36);
        let reference = synthetic_star_field(WIDTH, HEIGHT, &stars, FWHM, NOISE, 95);
        let target_stars = scale_stars(&stars, 0.8);
        let target = synthetic_star_field(WIDTH, HEIGHT, &target_stars, FWHM, NOISE, 96);
        let prepared = PreparedReferenceChannel::build(
            &reference,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            &[],
            200,
            None,
        );
        let fits = measure_like_fits(&target, WIDTH, HEIGHT, prepared.default_beta());
        let identity = identity_map();
        let (plain, breakdown) = relative_scale_from_seeds_with_breakdown(
            &prepared, &fits, &identity, &target, WIDTH, HEIGHT, 200, 4.0, 0.3, false, None,
        )
        .expect("a clean uniformly-scaled field must match");

        assert_eq!(breakdown.available, fits.len());
        assert!(breakdown.in_coverage <= breakdown.available);
        assert!(breakdown.past_saturation <= breakdown.in_coverage);
        assert!(breakdown.past_preselect <= breakdown.past_saturation);
        // `fit_accepted` is NOT bounded by `past_preselect` alone in
        // general (the `max_stars` truncation sits between them), but on
        // this clean field with 60 stars and `max_stars = 200` the
        // truncation never bites, so the ordinary chain inequality holds.
        assert!(breakdown.fit_accepted <= breakdown.past_preselect);
        assert!(breakdown.matched <= breakdown.fit_accepted);
        assert_eq!(
            breakdown.matched, plain.matches,
            "the breakdown's own final count must equal the plain result's matches"
        );
        // On an identity map with no saturation and no crowding, every
        // Measure-like fit should clear every gate on this clean field.
        assert_eq!(breakdown.available, breakdown.in_coverage);
        assert_eq!(breakdown.in_coverage, breakdown.past_saturation);
    }
}
