//! Relative scale between one target frame and the LN reference (spec
//! §5.2, math §4.3): the RCR location of the matched stars' PSF-flux
//! ratios, `s = RCR_loc(flux_ref,k / flux_tgt,k)`. Task 5 subtracts this
//! into the additive term (`B = B_ref − s·B_tgt`) and later stamps it on
//! the grid as the multiplicative `A`.
//!
//! Detection and PSF fitting run the same detection + fit path on both
//! planes (`detect_and_fit`, `DetectionConfig::default()`, `max_stars`,
//! one concrete PSF model, `FitParams::default()`, native units), which is
//! what makes the ratios comparable — ruling R2. Both planes are assumed
//! already in the reference geometry (registered), so a shared pixel
//! position means the same sky position. Matching is a single
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
use crate::stacking::psf_signal::{fit_stars, FitOutcome, FitParams, PsfModel, Seed};
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
}

/// Detect + PSF-fit one plane at a concrete `model`: registration's own
/// detector for star positions (its saturation/eccentricity/SNR cuts are
/// exactly the ones a reliable flux match wants), converted to fit seeds
/// and handed to the measurement fitter. Fits come back brightest-first
/// (detection's own sort order, preserved by the fitter). `model` is
/// always a concrete choice by the time this is called — see
/// [`resolve_concrete_model`] — never re-resolved per plane.
fn detect_and_fit(
    data: &[f32],
    width: usize,
    height: usize,
    model: PsfModel,
    max_stars: usize,
) -> FitOutcome {
    let cfg = DetectionConfig::default();
    let stars = detect_stars(data, width, height, &cfg, max_stars, None);
    let seeds: Vec<Seed> = stars.iter().map(to_seed).collect();
    fit_stars(data, width, height, &seeds, model, &FitParams::default())
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

/// One concrete `PsfModel` for both planes (ruling item 3): `Auto`'s
/// adaptive per-plane β search picks whichever β best fits THAT plane's
/// own residuals, so a seeing or SNR difference between reference and
/// target can land the two planes on different β — and β changes the
/// FWTM-enclosed flux fraction (math §1.4), biasing the ratio
/// systematically even though [`StarFit::signal`] itself is otherwise
/// width-independent for a fixed β. `Moffat4` is already concrete
/// (nothing to resolve). For `Auto`, `resolved_beta` is whatever the
/// REFERENCE plane's own `fit_stars` call picked
/// ([`FitOutcome::beta`]) — reused here as `Moffat4` when it lands on
/// exactly 4.0 (`fit_stars`'s own default/fallback β, and what a typical
/// calibrated-light PSF resolves to), the only OTHER concrete value
/// `PsfModel` can express. `PsfModel` has no variant for an arbitrary β
/// (2.5/6/10), so on any other resolved value there is no way to pin the
/// target to the same one through this public API — it falls back to its
/// own independent `Auto` search, same as before this fix. A real fix for
/// that case needs a `PsfModel` variant that carries a raw β, which is
/// `psf_signal.rs`'s call, not this file's.
fn resolve_concrete_model(psf: PsfModel, resolved_beta: f64) -> PsfModel {
    match psf {
        PsfModel::Moffat4 => PsfModel::Moffat4,
        PsfModel::Auto => {
            if resolved_beta == 4.0 {
                PsfModel::Moffat4
            } else {
                PsfModel::Auto
            }
        }
    }
}

/// Global relative scale `s = RCR_loc(z_k)`, `z_k = flux_ref,k / flux_tgt,k`
/// over stars matched within `match_radius_px` of each other (math §4.3).
/// `z_k` is built from [`StarFit::signal`] (background-subtracted flux
/// inside the fitted FWTM ellipse), never [`StarFit::mean_flux`]: for a
/// fixed β the FWTM ellipse encloses a fixed fraction of the profile
/// regardless of FWHM, so `signal` is width-independent — `mean_flux`
/// divides by the ellipse's own area (`π·(k/2)²·fwtm_x·fwtm_y`, which
/// scales as FWTM²) and would leak the two planes' seeing difference
/// straight into the scale. `reference`/`target` are row-major
/// `width × height` planes already in the reference geometry (registered);
/// `psf`/`max_stars` should be the same `measurement` config values the
/// frame's own measurement pass used for `max_stars`, though the fit model
/// itself is resolved once from the reference and reused on the target —
/// see [`resolve_concrete_model`]. Fewer than [`MIN_MATCHES`] surviving
/// pairs is [`LnError::TooFewMatches`] — the caller excludes the frame
/// from the LN pass rather than trust a scale from a handful of stars.
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
    let ref_outcome = detect_and_fit(reference, width, height, psf, max_stars);
    let concrete = resolve_concrete_model(psf, ref_outcome.beta);
    let tgt_outcome = detect_and_fit(target, width, height, concrete, max_stars);

    let ref_points: Vec<(f64, f64)> = ref_outcome.fits.iter().map(|f| (f.x, f.y)).collect();
    let tree = KdTree2::build(&ref_points);

    let mut ratios: Vec<f64> = Vec::with_capacity(tgt_outcome.fits.len());
    for tf in &tgt_outcome.fits {
        // One-way match, target → reference: each target fit claims its
        // single nearest reference fit within `match_radius_px`, so a
        // reference star can be claimed by more than one target fit in a
        // crowded field — RCR (below) absorbs the resulting duplicate or
        // skewed ratios. The barycentre one-to-one pass (spec §4.3) is
        // deferred to M4 (ruling R2), same as the module doc above.
        let Some((i, _dist)) = tree.nearest_within(tf.x, tf.y, match_radius_px) else {
            continue;
        };
        let (flux_ref, flux_tgt) = (ref_outcome.fits[i].signal, tf.signal);
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
    })
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
    /// [`crate::stacking::psf_signal::fwtm_from_alpha`] at tenth- rather
    /// than half-maximum), inverted for `α`.
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
    fn auto_resolves_one_beta_from_the_reference_for_both_planes() {
        let (reference, target) = seeing_difference_pair();

        let via_auto = relative_scale(
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
        let via_explicit = relative_scale(
            &reference,
            &target,
            WIDTH,
            HEIGHT,
            PsfModel::Moffat4,
            200,
            4.0,
            0.3,
        )
        .expect("the explicit-model call must match too");

        // The field is generated at β = 4, so `fit_stars`'s own Auto
        // search resolves the reference to exactly β = 4 (same as
        // `psf_signal::tests::auto_model_prefers_the_generating_beta`) —
        // `resolve_concrete_model` then reuses `Moffat4` for the target,
        // making the two calls do IDENTICAL fitting work.
        assert!(
            (via_auto.scale - via_explicit.scale).abs() < 1e-6,
            "{} vs {}",
            via_auto.scale,
            via_explicit.scale
        );
        assert!((via_auto.sigma - via_explicit.sigma).abs() < 1e-6);
        assert_eq!(via_auto.matches, via_explicit.matches);
        assert_eq!(via_auto.rejected, via_explicit.rejected);
    }
}
