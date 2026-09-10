//! Relative scale between one target frame and the LN reference (spec
//! §5.2, math §4.3): the RCR location of the matched stars' PSF-flux
//! ratios, `s = RCR_loc(flux_ref,k / flux_tgt,k)`. Task 5 subtracts this
//! into the additive term (`B = B_ref − s·B_tgt`) and later stamps it on
//! the grid as the multiplicative `A`.
//!
//! Detection and PSF fitting reuse the registration detector
//! ([`crate::stacking::register::detect::detect_stars`], its default
//! [`crate::stacking::register::DetectionConfig`]) and the measurement
//! fitter ([`crate::stacking::psf_signal::fit_stars`]) on each plane
//! independently — both planes are assumed already in the reference
//! geometry (registered), so a shared pixel position means the same sky
//! position. Matching is a single nearest-neighbour pass: a
//! [`crate::geometry::kdtree::KdTree2`] built over the reference fits'
//! centroids, queried once per target fit within `match_radius_px`. The
//! spec's "square half-side 4" window and this nearest-within-a-circle
//! query differ only at the corners of that box — close enough that a
//! second shape is not worth the code. **The barycentre second-pass match
//! (spec §4.3) is deferred to M4** (controller ruling R2): this is the
//! first pass only, so a frame whose stars moved enough between passes
//! that fewer than 80 % still fall within `match_radius_px` of their
//! reference counterpart will under-match — recorded, not fixed, here.

use std::f64::consts::PI;

use super::LnError;
use crate::geometry::kdtree::KdTree2;
use crate::stacking::psf_signal::{fit_stars, FitParams, PsfModel, Seed, StarFit};
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

/// Detect + PSF-fit one plane: registration's own detector for star
/// positions (its saturation/eccentricity/SNR cuts are exactly the ones a
/// reliable flux match wants), converted to fit seeds and handed to the
/// measurement fitter. Fits come back brightest-first (detection's own
/// sort order, preserved by the fitter).
fn detect_and_fit(
    data: &[f32],
    width: usize,
    height: usize,
    psf: PsfModel,
    max_stars: usize,
) -> Vec<StarFit> {
    let cfg = DetectionConfig::default();
    let stars = detect_stars(data, width, height, &cfg, max_stars, None);
    let seeds: Vec<Seed> = stars.iter().map(to_seed).collect();
    fit_stars(data, width, height, &seeds, psf, &FitParams::default()).fits
}

/// A fit seed from a registration [`Star`]. `Star` carries no peak
/// amplitude (registration only ever needed flux + optional σ), but
/// [`fit_stars`] reads `Seed::peak` in exactly one place — the field-level
/// `initial_sigma()` median used to size the fit stamp — so an amplitude
/// backed out of the flux/σ the detector already refined
/// (`peak = flux / (2π·σx·σy)`, the closed form for a 2-D Gaussian's
/// total) is accurate where it exists; without a refined σ the flux itself
/// is a safe order-of-magnitude stand-in for that one median.
fn to_seed(star: &Star) -> Seed {
    let peak = match star.sigma {
        Some((sx, sy)) if sx > 0.0 && sy > 0.0 => star.flux / (2.0 * PI * sx * sy),
        _ => star.flux,
    };
    Seed {
        x: star.x,
        y: star.y,
        peak: peak.max(f64::MIN_POSITIVE),
        flux: star.flux,
    }
}

/// Global relative scale `s = RCR_loc(z_k)`, `z_k = flux_ref,k / flux_tgt,k`
/// over stars matched within `match_radius_px` of each other (math §4.3).
/// `reference`/`target` are row-major `width × height` planes already in
/// the reference geometry (registered); `psf`/`max_stars` should be the
/// same `measurement` settings the frame's own measurement pass used, so
/// local normalization sees the same stars. Fewer than [`MIN_MATCHES`]
/// surviving pairs is [`LnError::TooFewMatches`] — the caller excludes the
/// frame from the LN pass rather than trust a scale from a handful of
/// stars.
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
    let ref_fits = detect_and_fit(reference, width, height, psf, max_stars);
    let tgt_fits = detect_and_fit(target, width, height, psf, max_stars);

    let ref_points: Vec<(f64, f64)> = ref_fits.iter().map(|f| (f.x, f.y)).collect();
    let tree = KdTree2::build(&ref_points);

    let mut ratios: Vec<f64> = Vec::with_capacity(tgt_fits.len());
    for tf in &tgt_fits {
        let Some((i, _dist)) = tree.nearest_within(tf.x, tf.y, match_radius_px) else {
            continue;
        };
        let (flux_ref, flux_tgt) = (ref_fits[i].mean_flux(), tf.mean_flux());
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
}
