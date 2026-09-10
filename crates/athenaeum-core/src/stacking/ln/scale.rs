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
//! shared pixel position means the same sky position. Matching is a single
//! nearest-neighbour pass: a [`crate::geometry::kdtree::KdTree2`] built
//! over the reference fits' centroids, queried once per target fit within
//! `match_radius_px`. The spec's "square half-side 4" window and this
//! nearest-within-a-circle query differ only at the corners of that box —
//! close enough that a second shape is not worth the code. **The
//! barycentre second-pass match (spec §4.3) is deferred to M4** (ruling
//! R2): this is the first pass only, so a frame whose stars moved enough
//! between passes that fewer than 80 % still fall within `match_radius_px`
//! of their reference counterpart will under-match — recorded, not fixed,
//! here.

use std::f64::consts::PI;

use tracing::debug;

use super::LnError;
use crate::geometry::kdtree::KdTree2;
use crate::stacking::psf_signal::{
    fit_stars, fit_stars_with_beta, FitOutcome, FitParams, PsfModel, Seed,
};
use crate::stacking::register::detect::{detect_stars, Star};
use crate::stacking::register::DetectionConfig;

/// Fewer surviving pairs than this and the frame cannot be trusted for
/// local normalization (spec §5.2 ruling: `LnError::TooFewMatches`).
pub const MIN_MATCHES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq)]
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
    tree: KdTree2,
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
        let ref_seeds = detect_seeds(reference, width, height, max_stars);
        let outcome = fit_stars(
            reference,
            width,
            height,
            &ref_seeds,
            psf,
            &FitParams::default(),
        );
        let ref_points: Vec<(f64, f64)> = outcome.fits.iter().map(|f| (f.x, f.y)).collect();
        let tree = KdTree2::build(&ref_points);
        PreparedReferenceChannel { outcome, tree }
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
pub fn relative_scale_against(
    prepared: &PreparedReferenceChannel,
    target: &[f32],
    width: usize,
    height: usize,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
) -> Result<ScaleResult, LnError> {
    let tgt_seeds = detect_seeds(target, width, height, max_stars);
    let tgt_outcome = fit_stars_with_beta(
        target,
        width,
        height,
        &tgt_seeds,
        prepared.outcome.beta,
        &FitParams::default(),
    );

    let mut ratios: Vec<f64> = Vec::with_capacity(tgt_outcome.fits.len());
    for tf in &tgt_outcome.fits {
        // One-way match, target → reference: each target fit claims its
        // single nearest reference fit within `match_radius_px`, so a
        // reference star can be claimed by more than one target fit in a
        // crowded field — RCR (below) absorbs the resulting duplicate or
        // skewed ratios. The barycentre one-to-one pass (spec §4.3) is
        // deferred to M4 (ruling R2), same as the module doc above.
        let Some((i, _dist)) = prepared.tree.nearest_within(tf.x, tf.y, match_radius_px) else {
            continue;
        };
        let (flux_ref, flux_tgt) = (prepared.outcome.fits[i].signal, tf.signal);
        if flux_ref > 0.0 && flux_tgt > 0.0 {
            ratios.push(flux_ref / flux_tgt);
        }
    }

    if ratios.len() < MIN_MATCHES {
        return Err(LnError::TooFewMatches {
            matches: ratios.len(),
        });
    }

    let r = crate::stacking::robust::rcr(&ratios, rcr_limit);
    debug!(
        ln_scale = r.location,
        sigma = r.scale,
        ln_matches = ratios.len(),
        rejected = r.rejected,
        "ln relative scale"
    );
    Ok(ScaleResult {
        scale: r.location,
        sigma: r.scale,
        matches: ratios.len(),
        rejected: r.rejected,
        beta: prepared.outcome.beta,
    })
}

/// Thin wrapper: [`PreparedReferenceChannel::build`] +
/// [`relative_scale_against`] in one call — used by the probe and by every
/// existing test that has no group-level `PreparedReferenceChannel` handy.
/// `normalize_frame` (the real per-frame pipeline, `ln/mod.rs`) calls
/// [`relative_scale_against`] directly against the group's ONE prepared
/// reference channel instead, so it never re-detects or re-fits the
/// reference plane per frame (final fix wave, I2).
pub fn relative_scale(
    reference: &[f32],
    target: &[f32],
    width: usize,
    height: usize,
    psf: PsfModel,
    max_stars: usize,
    match_radius_px: f64,
    rcr_limit: f64,
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
        )
        .expect("a clean uniformly-scaled field must match");

        let prepared =
            PreparedReferenceChannel::build(&reference, WIDTH, HEIGHT, PsfModel::Moffat4, 200);
        let via_prepared = relative_scale_against(&prepared, &target, WIDTH, HEIGHT, 200, 4.0, 0.3)
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
        )
        .expect("a clean field differing only in seeing must still match");

        assert_eq!(r.beta, 4.0);
    }
}
