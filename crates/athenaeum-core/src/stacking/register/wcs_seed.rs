//! A subject → reference alignment seed built from the two frames' stored
//! plate solves (M4b, ruling R-M4b-3), the way a coordinate-based aligner
//! works: sample a grid of subject pixels, project each one to the sky
//! through the subject's own WCS, project it back into the reference's
//! pixel grid through the reference's WCS, and least-squares an affine
//! through the resulting correspondences.
//!
//! Two frames at different pixel scales have no common quad geometry to
//! seed from cheaply, and the quad seed's tolerance is a ratio tolerance —
//! it degrades exactly where a 2x scale step puts it. When both frames are
//! solved this seed lands the subject on the reference to within a pixel
//! before a single star has been paired, and `align` then only has to
//! confirm it.
//!
//! Coordinates are 0-based pixel centres on both sides (`PlateSolveRecord`
//! and `WcsSolution` agree on this) — there is no ±1 anywhere in here.

use crate::geometry::{fit_affine, Linear, Pair};
use crate::plate_solve::PlateSolveRecord;

/// The seed samples a `WCS_SEED_GRID` x `WCS_SEED_GRID` grid of subject
/// pixels. 25 points over-determine the 6 affine parameters comfortably
/// and spread the projection's own curvature evenly across the frame.
pub const WCS_SEED_GRID: usize = 5;
/// The grid is inset this fraction of the frame from each edge, so the fit
/// is never anchored on the extreme corners where a distortion polynomial
/// is least constrained.
pub const WCS_SEED_INSET: f64 = 0.05;
/// A seed whose implied scale falls outside this range is not a plausible
/// frame-to-frame relation at all (a mis-stored CD matrix, a solve for a
/// different instrument) and is dropped in favour of the quad seed. It is
/// deliberately far wider than the per-frame acceptance gate — this only
/// rejects nonsense, the gate judges the fitted alignment.
pub const WCS_SEED_SCALE_RANGE: (f64, f64) = (0.05, 20.0);
/// The pairing radius `align` confirms a WCS seed through, as a multiple
/// of the RANSAC tolerance …
pub const WCS_SEED_RADIUS_FACTOR: f64 = 4.0;
/// … with this floor in pixels (a tight RANSAC tolerance must not make the
/// confirmation stricter than the seed's own accuracy).
pub const WCS_SEED_RADIUS_MIN_PX: f64 = 8.0;
/// How far a frame's implied scale ratio to the reference must sit from 1
/// before the seed is worth building at all (ruling R-T2-1).
///
/// The ratio is a quotient of two MEASURED pixel scales — and
/// `GroupFrame::pixel_scale_arcsec` prefers the stored plate solve, so two
/// independent solves of one rig land a few parts in ten thousand apart.
/// Triggering on `ratio != 1.0` exactly would therefore send every frame
/// of an ordinary same-scale set down the WCS path, which is precisely the
/// path the M1–M4a pins were measured without. `1e-3` sits two orders
/// below [`super::SCALE_TOLERANCE`]'s 0.25 and two orders above that
/// solve-to-solve jitter, so it separates "the same rig, measured twice"
/// from "genuinely a different sampling" with room on both sides.
pub const WCS_SEED_RATIO_EPS: f64 = 1e-3;

/// Whether a frame whose implied scale ratio to the reference is `ratio`
/// is worth attempting a plate-solve seed for (ruling R-T2-1). See
/// [`WCS_SEED_RATIO_EPS`]; a non-finite ratio is never worth it.
pub fn ratio_wants_seed(ratio: f64) -> bool {
    ratio.is_finite() && (ratio - 1.0).abs() > WCS_SEED_RATIO_EPS
}

/// Subject → reference as an affine, from both frames' stored plate
/// solves. `subject_geometry` is the subject frame's own `(width, height)`
/// in pixels — the grid is laid over it, not over the reference.
///
/// `None` when either record does not convert to a usable solution, when
/// the subject geometry is degenerate, when a projected point is not
/// finite (a singular CD matrix, a point behind the tangent plane), when
/// the fit is degenerate, or when the implied scale is outside
/// [`WCS_SEED_SCALE_RANGE`]. The seed knows nothing about whether the two
/// fields actually overlap — a subject pointing elsewhere yields a
/// perfectly well-formed affine that maps it far off the reference, and
/// the caller's pairing step is what discovers that.
pub fn seed_from_solves(
    subject: &PlateSolveRecord,
    reference: &PlateSolveRecord,
    subject_geometry: (usize, usize),
) -> Option<Linear> {
    let subject_wcs = subject.to_solution()?;
    let reference_wcs = reference.to_solution()?;
    let (w, h) = (subject_geometry.0 as f64, subject_geometry.1 as f64);
    if !(w > 1.0 && h > 1.0) {
        return None;
    }

    let span = (WCS_SEED_GRID - 1) as f64;
    let mut pairs: Vec<Pair> = Vec::with_capacity(WCS_SEED_GRID * WCS_SEED_GRID);
    for i in 0..WCS_SEED_GRID {
        for j in 0..WCS_SEED_GRID {
            let fx = WCS_SEED_INSET + (1.0 - 2.0 * WCS_SEED_INSET) * (i as f64 / span);
            let fy = WCS_SEED_INSET + (1.0 - 2.0 * WCS_SEED_INSET) * (j as f64 / span);
            let (sx, sy) = (fx * w, fy * h);
            let (ra, dec) = subject_wcs.pixel_to_sky(sx, sy);
            if !ra.is_finite() || !dec.is_finite() {
                return None;
            }
            let (rx, ry) = reference_wcs.sky_to_pixel(ra, dec);
            if !rx.is_finite() || !ry.is_finite() {
                return None;
            }
            pairs.push(((sx, sy), (rx, ry)));
        }
    }

    let seed = fit_affine(&pairs, None)?;
    let scale = seed.scale();
    if !(scale.is_finite() && scale >= WCS_SEED_SCALE_RANGE.0 && scale <= WCS_SEED_SCALE_RANGE.1) {
        return None;
    }
    Some(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CD matrix for `scale` arcsec/px rotated `rot_deg`, in the usual
    /// sky handedness (RA increasing to the left).
    fn cd(scale_arcsec: f64, rot_deg: f64) -> [[f64; 2]; 2] {
        let s = scale_arcsec / 3600.0;
        let (sn, cs) = rot_deg.to_radians().sin_cos();
        [[-s * cs, s * sn], [s * sn, s * cs]]
    }

    fn solved(
        frame_id: i64,
        crpix: (f64, f64),
        crval: (f64, f64),
        scale_arcsec: f64,
        rot_deg: f64,
    ) -> PlateSolveRecord {
        let m = cd(scale_arcsec, rot_deg);
        PlateSolveRecord {
            id: None,
            frame_id,
            crpix1: crpix.0,
            crpix2: crpix.1,
            crval1: crval.0,
            crval2: crval.1,
            cd1_1: m[0][0],
            cd1_2: m[0][1],
            cd2_1: m[1][0],
            cd2_2: m[1][1],
            sip_order: None,
            sip_a_coeffs: None,
            sip_b_coeffs: None,
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 60,
            total_detected: 200,
            rms_residual_px: 0.3,
            rms_residual_arcsec: 0.2,
            pixel_scale_arcsec: scale_arcsec,
            field_rotation_deg: rot_deg,
            solve_time_ms: 5,
            catalog_used: "test".to_string(),
            algorithm_used: "test".to_string(),
            solved_at: "2025-01-01T00:00:00Z".to_string(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    /// The 6224x4168 reference at 0.78"/px, unrotated.
    fn reference() -> PlateSolveRecord {
        solved(1, (3112.0, 2084.0), (300.0, 60.0), 0.78, 0.0)
    }

    /// The same sky in a 3112x2084 frame at 1.56"/px, rotated 3 deg.
    fn subject() -> PlateSolveRecord {
        solved(2, (1556.0, 1042.0), (300.0, 60.0), 1.56, 3.0)
    }

    #[test]
    fn seeds_a_two_times_scale_step_from_the_two_solves() {
        let seed = seed_from_solves(&subject(), &reference(), (3112, 2084))
            .expect("two solved frames seed");

        // The subject's own reference pixel is its centre and both solves
        // name the same sky there, so it must land on the reference's.
        let (x, y) = seed.apply(1556.0, 1042.0);
        assert!(
            (x - 3112.0).abs() < 0.01 && (y - 2084.0).abs() < 0.01,
            "centre maps to ({x}, {y})"
        );
        assert!((seed.scale() - 2.0).abs() < 1e-3, "scale {}", seed.scale());
        assert!(
            (seed.rotation_deg() - 3.0).abs() < 0.01,
            "rotation {}",
            seed.rotation_deg()
        );
    }

    #[test]
    fn a_subject_pointing_elsewhere_still_seeds() {
        // 5 deg of RA away at dec 60: thousands of pixels clear of the
        // 6224x4168 reference. The seed does not know about overlap — the
        // pairing step downstream is what refuses this.
        let away = solved(3, (1556.0, 1042.0), (305.0, 60.0), 1.56, 3.0);
        let seed =
            seed_from_solves(&away, &reference(), (3112, 2084)).expect("a well-formed affine");
        let (x, y) = seed.apply(1556.0, 1042.0);
        assert!(
            (x - 3112.0).hypot(y - 2084.0) > 5_000.0,
            "the centre should land far off the reference: ({x}, {y})"
        );
        // Still a sane similarity-like step, just pointed elsewhere.
        assert!((seed.scale() - 2.0).abs() < 0.1, "scale {}", seed.scale());
    }

    #[test]
    fn an_unconvertible_record_or_a_degenerate_geometry_seeds_nothing() {
        let mut broken = subject();
        broken.sip_order = Some(2);
        broken.sip_a_coeffs = Some("not json".to_string());
        broken.sip_b_coeffs = Some("[[0.0]]".to_string());
        assert!(seed_from_solves(&broken, &reference(), (3112, 2084)).is_none());
        assert!(seed_from_solves(&subject(), &broken, (3112, 2084)).is_none());
        assert!(seed_from_solves(&subject(), &reference(), (0, 0)).is_none());

        // A singular CD matrix projects nowhere.
        let mut singular = reference();
        singular.cd1_1 = 0.0;
        singular.cd1_2 = 0.0;
        singular.cd2_1 = 0.0;
        singular.cd2_2 = 0.0;
        assert!(seed_from_solves(&subject(), &singular, (3112, 2084)).is_none());
    }

    #[test]
    fn an_implausible_scale_step_is_refused() {
        // 0.78"/px against 0.0195"/px is a 40x step — nonsense rather than
        // a frame pair, and outside WCS_SEED_SCALE_RANGE.
        let tiny = solved(4, (1556.0, 1042.0), (300.0, 60.0), 0.0195, 0.0);
        assert!(seed_from_solves(&reference(), &tiny, (6224, 4168)).is_none());
        assert!(WCS_SEED_SCALE_RANGE.0 > 0.0 && WCS_SEED_SCALE_RANGE.1 > WCS_SEED_SCALE_RANGE.0);
    }

    /// Ruling R-T2-1: two solves of the SAME rig differ in the fourth or
    /// fifth digit and must not be read as a scale step; a real one must.
    #[test]
    fn only_a_real_scale_step_wants_a_seed() {
        assert_eq!(WCS_SEED_RATIO_EPS, 1e-3);
        for same in [1.0, 0.7800 / 0.7803, 0.7803 / 0.7800, 1.0004, 0.9995] {
            assert!(!ratio_wants_seed(same), "{same} is the same sampling");
        }
        for stepped in [2.0, 0.5, 1.27, 0.79, 1.0011] {
            assert!(ratio_wants_seed(stepped), "{stepped} is a real step");
        }
        assert!(!ratio_wants_seed(f64::NAN));
        assert!(!ratio_wants_seed(f64::INFINITY));
    }

    #[test]
    fn the_grid_is_inset_and_square() {
        assert_eq!(WCS_SEED_GRID, 5);
        assert_eq!(WCS_SEED_INSET, 0.05);
        assert_eq!(WCS_SEED_RADIUS_FACTOR, 4.0);
        assert_eq!(WCS_SEED_RADIUS_MIN_PX, 8.0);
        // Sanity on the helper the tests themselves lean on: a pair is
        // (subject, reference), the order `fit_affine` fits.
        let p: Pair = ((1.0, 2.0), (3.0, 4.0));
        assert_eq!((p.0, p.1), ((1.0, 2.0), (3.0, 4.0)));
    }
}
