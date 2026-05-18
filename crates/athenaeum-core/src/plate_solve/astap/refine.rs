//! Second solve → WCS.
//!
//! ASTAP, once a trial has matched, re-projects the catalog at the solved
//! centre and refits over the full inlier set (`standard_equatorial`
//! re-projection, `unit_command_line_solving.pas:2603-2656`). We reuse the
//! legacy verify/refit machinery verbatim for this — it is already exactly a
//! gnomonic re-projection + iterative full-field similarity refit
//! (`service::count_inliers` / `service::translation_refit`) — so the
//! second solve is identical to the proven legacy path. The only new code is
//! turning the trial's quad-centre affine into the seed WCS.

use astroimage::platesolving::{AffineTransform, ImageStar};

use crate::catalog::CatalogEngine;
use crate::plate_solve::astap::trial::TrialFit;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::service::{
    adaptive_tol_px, count_inliers, scale_is_plausible, similarity_to_wcs, translation_refit,
    Similarity, SolveResult,
};

/// Arcsec → radian. The trial affine works in tangent-plane arcsec
/// (`trial::ARCSEC_PER_RAD`); the legacy [`Similarity`] convention is radians.
const RAD_PER_ARCSEC: f64 = std::f64::consts::PI / (180.0 * 3600.0);

/// Catalog magnitude depth for the verification cone (mirrors the legacy
/// per-candidate verify, `service.rs`: `cone_search(.., 12.0, ..)`).
const VERIFY_MAG_LIMIT: f32 = 12.0;

/// Convert the trial's quad-centre affine (image px → tangent-plane **arcsec**
/// about `fit.center`) into the legacy [`Similarity`] (image px → tangent
/// **radians**, origin at `image_center`).
///
/// Trial affine: `xi_as = a1·x + b1·y + c1`, `eta_as = a2·x + b2·y + c2`.
/// Similarity:    `xi_rad = a·(x-cx) + b·(y-cy) + tx`. Matching coefficients
/// (with `k = RAD_PER_ARCSEC`) gives `a = a1·k`, … and the translation folded
/// to the image-centre origin: `tx = k·(a1·cx + b1·cy + c1)`.
pub(crate) fn seed_similarity_from_affine(
    t: &AffineTransform,
    image_center: (f64, f64),
) -> Similarity {
    let k = RAD_PER_ARCSEC;
    let (cx, cy) = image_center;
    Similarity {
        a: t.a1 * k,
        b: t.b1 * k,
        c: t.a2 * k,
        d: t.b2 * k,
        tx: k * (t.a1 * cx + t.b1 * cy + t.c1),
        ty: k * (t.a2 * cx + t.b2 * cy + t.c2),
    }
}

/// Verify + second-solve a trial fit. Returns `(result, inliers,
/// expected_catalog_stars_in_fov)` for the caller's density gate, or `None`
/// if the seed is non-physical, the cone search fails, or nothing verifies.
///
/// This is the legacy per-candidate verify path (`service.rs` seed-WCS →
/// `count_inliers` → `translation_refit` → re-count tight), driven by the
/// gnomonically-consistent trial seed instead of a hash-index candidate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn refine_trial(
    fit: &TrialFit,
    image_stars: &[ImageStar],
    image_size: (u32, u32),
    image_center: (f64, f64),
    catalog: &CatalogEngine,
    config: &PlateSolveConfig,
    obs_epoch: f64,
    hint_scale_arcsec: Option<f64>,
    filename: &str,
) -> Option<(SolveResult, usize, usize)> {
    let seed_sim = seed_similarity_from_affine(&fit.transform, image_center);
    let seed_wcs = similarity_to_wcs(&seed_sim, image_center, fit.center);

    // Physical-plausibility only (0.1–30"/px); the scale prior is enforced by
    // the FOV ladder, not here.
    if !scale_is_plausible(&seed_wcs, None, 0.0) {
        return None;
    }

    let (seed_ra, seed_dec) = seed_wcs.pixel_to_sky(image_center.0, image_center.1);
    let seed_scale = seed_wcs.pixel_scale_arcsec();
    let image_fov_deg =
        (image_size.0 as f64).max(image_size.1 as f64) * seed_scale / 3600.0;
    let cone_radius = image_fov_deg * 0.7;
    // Reject ≥90° too: a catalog star at exactly 90° separation gives
    // cos_c == 0 in `sky_to_tangent`, which would trip its `cos_c > 0`
    // assert during `count_inliers` and panic the batch worker. Physically
    // unreachable (needs a ~128° image) but the guard must be provably
    // panic-safe, not merely unreachable.
    if !cone_radius.is_finite() || cone_radius <= 0.0 || cone_radius >= 90.0 {
        return None;
    }

    let (verify_stars, cat_name) = catalog
        .cone_search(seed_ra, seed_dec, cone_radius, VERIFY_MAG_LIMIT, obs_epoch)
        .ok()?;
    if verify_stars.len() < 4 {
        return None;
    }

    let base_tol = config.base_verification_tolerance_arcsec;
    let tol_px = adaptive_tol_px(seed_scale, base_tol);
    let seed_gate_tol_px = (tol_px * 2.0).min(20.0);

    let (seed_inliers, seed_rs, _) =
        count_inliers(&seed_wcs, &verify_stars, image_stars, image_size, tol_px);
    let (gate_inliers, _, gate_pairs) = count_inliers(
        &seed_wcs,
        &verify_stars,
        image_stars,
        image_size,
        seed_gate_tol_px,
    );

    // Second solve: full-field iterative similarity refit (= ASTAP's
    // re-projection at the solved centre). Identical to the legacy refit.
    let (final_wcs, final_inliers, final_rs) = if gate_inliers >= 6 {
        match translation_refit(
            &seed_wcs,
            &gate_pairs,
            &verify_stars,
            image_stars,
            image_size,
            tol_px,
            (seed_ra, seed_dec),
            image_center,
            hint_scale_arcsec,
            0.10,
        ) {
            Some((refit_wcs, _, _)) => {
                let (rt_inliers, rt_rs, _) = count_inliers(
                    &refit_wcs,
                    &verify_stars,
                    image_stars,
                    image_size,
                    tol_px,
                );
                if rt_inliers > seed_inliers {
                    (refit_wcs, rt_inliers, rt_rs)
                } else {
                    (seed_wcs, seed_inliers, seed_rs)
                }
            }
            None => (seed_wcs, seed_inliers, seed_rs),
        }
    } else {
        (seed_wcs, seed_inliers, seed_rs)
    };

    if final_inliers == 0 {
        return None;
    }

    let fscale = final_wcs.pixel_scale_arcsec();
    let rms_px = (final_rs / final_inliers as f64).sqrt();
    let ratio = if verify_stars.is_empty() {
        0.0
    } else {
        final_inliers as f64 / verify_stars.len() as f64
    };
    let (fra, fdec) = final_wcs.pixel_to_sky(image_center.0, image_center.1);
    eprintln!(
        "plate_solve [{}]: astap trial verified — RA={:.3} Dec={:.3} \
         scale={:.3}\"/px inliers={}/{}",
        filename,
        fra,
        fdec,
        fscale,
        final_inliers,
        verify_stars.len()
    );

    let result = SolveResult {
        wcs: final_wcs.clone(),
        matched_stars: final_inliers,
        total_detected: image_stars.len(),
        rms_residual_px: rms_px,
        rms_residual_arcsec: rms_px * fscale,
        pixel_scale_arcsec: fscale,
        field_rotation_deg: final_wcs.field_rotation_deg(),
        solve_time_ms: 0,
        catalog_used: cat_name.to_string(),
        algorithm_used: "astap_port".to_string(),
        derived_focallen_mm: None,
        focallen_corrected: false,
        expected_catalog_stars_in_fov: verify_stars.len(),
        inlier_ratio: ratio,
    };
    Some((result, final_inliers, verify_stars.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seed conversion must round-trip: a WCS built from the seed
    /// Similarity must reproduce the affine's pixel→sky mapping. Build a
    /// known affine (1.5"/px, 17° rotation about a tangent point), convert,
    /// and check the recovered pixel scale + that image-centre maps to the
    /// tangent point.
    #[test]
    fn seed_similarity_recovers_scale_and_centre() {
        let ra0 = 180.0_f64;
        let dec0 = 40.0_f64;
        let image_center = (2000.0, 2000.0);
        let scale = 1.5_f64; // arcsec / px
        let (s, c) = 17.0_f64.to_radians().sin_cos();
        // Affine: tangent-arcsec = R(17°) * scale * (px - centre).
        // xi_as = scale*c*(x-cx) - scale*s*(y-cy)
        // eta_as = scale*s*(x-cx) + scale*c*(y-cy)
        let (cx, cy) = image_center;
        let a1 = scale * c;
        let b1 = -scale * s;
        let a2 = scale * s;
        let b2 = scale * c;
        let t = AffineTransform {
            a1,
            b1,
            c1: -(a1 * cx + b1 * cy),
            a2,
            b2,
            c2: -(a2 * cx + b2 * cy),
        };

        let sim = seed_similarity_from_affine(&t, image_center);
        let wcs = similarity_to_wcs(&sim, image_center, (ra0, dec0));

        // Image centre → tangent point.
        let (mra, mdec) = wcs.pixel_to_sky(cx, cy);
        assert!(
            (mra - ra0).abs() < 1e-6 && (mdec - dec0).abs() < 1e-6,
            "image centre must map to the tangent point, got ({mra},{mdec})"
        );
        // Recovered pixel scale ≈ 1.5"/px.
        assert!(
            (wcs.pixel_scale_arcsec() - scale).abs() < 1e-6,
            "scale {} != {scale}",
            wcs.pixel_scale_arcsec()
        );
    }
}
