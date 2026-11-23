//! The constants and the pure radius rule of the plate-solve seed (`seed_from_solves`, which reads a stored plate
//! solve, belongs to the caller).

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
/// before the seed is worth building at all (rulings R-T2-1, R-T6-4).
///
/// The ratio is a quotient of two MEASURED pixel scales, and
/// `GroupFrame::pixel_scale_arcsec` prefers the stored plate solve, so the
/// number moves between two solves of the SAME rig. Triggering on
/// `ratio != 1.0` exactly would send every frame of an ordinary same-scale
/// set down the WCS path — precisely the path M1–M4a's pins were measured
/// without — so the trigger needs a tolerance, and the tolerance has to be
/// bigger than that jitter actually is.
///
/// Measured on the owner's catalog (2026-09-11, 1 781 plate-solved lights):
/// within one rig the solved scale scatters around its median with a
/// median deviation of 1e-4–3e-4, but the tails run to 0.8–1.6 % — on the
/// M4a acceptance set alone, 120 of 368 frames sit beyond 1e-3 of their
/// own set's median, and one beyond 1e-2. Nothing anywhere in the catalog
/// reached 5e-2. `0.05` is therefore ≈ 3× above the measured tail and 5×
/// below the [`super::SCALE_TOLERANCE`] step (1.25) that defines a foreign
/// scale in the first place: a same-rig frame never trips it, and a real
/// 5–25 % optical step still gets the seed — which would be harmless even
/// if it did not, since the seed is only ever an accelerator the aligner
/// confirms against stars before trusting.
pub const WCS_SEED_RATIO_EPS: f64 = 0.05;

/// Whether a frame whose implied scale ratio to the reference is `ratio`
/// is worth attempting a plate-solve seed for (rulings R-T2-1, R-T6-4).
/// See [`WCS_SEED_RATIO_EPS`]; a non-finite ratio is never worth it.
pub fn ratio_wants_seed(ratio: f64) -> bool {
    ratio.is_finite() && (ratio - 1.0).abs() > WCS_SEED_RATIO_EPS
}

/// The radius, in reference pixels, at which `align` confirms a seed
/// against the reference's own stars before trusting it:
/// [`WCS_SEED_RADIUS_FACTOR`] × the RANSAC tolerance, never below
/// [`WCS_SEED_RADIUS_MIN_PX`].
pub fn seed_radius_px(ransac_tolerance_px: f64) -> f64 {
    (WCS_SEED_RADIUS_FACTOR * ransac_tolerance_px).max(WCS_SEED_RADIUS_MIN_PX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Pair;

    /// Rulings R-T2-1 and R-T6-4: two solves of the SAME rig disagree —
    /// typically in the fourth digit, but out to 1.6 % in the measured
    /// tail — and none of that may be read as a scale step. A real one
    /// must be. The tolerance is 5 %: ≈ 3× above that tail, 5× below the
    /// 1.25 gate that defines a foreign scale.
    #[test]
    fn only_a_real_scale_step_wants_a_seed() {
        assert_eq!(WCS_SEED_RATIO_EPS, 0.05);
        for same in [
            1.0,
            0.7800 / 0.7803,
            1.0004,
            0.9995,
            1.0011,
            // The measured same-rig tail (2026-09-11, the owner's catalog).
            1.016,
            1.02,
            0.98,
        ] {
            assert!(
                !ratio_wants_seed(same),
                "R-T6-4: {same} is one rig measured twice, not a scale step"
            );
        }
        for stepped in [1.06, 1.27, 0.79, 2.0, 0.5] {
            assert!(
                ratio_wants_seed(stepped),
                "R-T6-4: {stepped} is a real sampling step"
            );
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
