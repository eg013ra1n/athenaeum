//! Per-cell match — **the core architectural fix**.
//!
//! ASTAP, at every (FOV, sky-cell) trial, gnomonically projects the catalog
//! cone onto the *same flat plane as the image* (`equatorial_standard`,
//! `unit_command_line_solving.pas:1693`) and builds catalog quads with the
//! *same distance function* used for the image quads, then matches on a
//! continuous ratio tolerance (`find_fit` :838, :860-862).
//!
//! The legacy Athenaeum path built image quads in flat pixels but the
//! prebuilt catalog quad index in great-circle degrees
//! (`index_builder.rs` `angular_separation_deg`) — two *different* invariants.
//! They agree to first order (why small-FOV frames still solved) but diverge
//! O(θ²) toward the edges of a wide field, pushing the correct quad past the
//! hash-quantization bin so it never matches. Projecting the catalog through
//! [`GnomonicProjection::sky_to_tangent`] and reusing the *same*
//! [`build_quads_multi`] for both sides makes the invariant identical on both
//! sides — the verified root-cause fix for the wide-field / dense failures.

use astroimage::platesolving::{
    build_quads_multi, fit_affine_from_centers, match_quads, AffineTransform, CatalogStar, Quad,
};

use crate::coordinates::angular_distance;

/// Sky positions farther than this from the tangent point are dropped before
/// projection: [`GnomonicProjection::sky_to_tangent`] panics for points on the
/// opposite hemisphere (`cos_c <= 0`). Trial cones are far smaller than this,
/// so the guard is purely defensive — it keeps the contract that a bad cell is
/// *skipped*, never a panic.
const MAX_PROJECTION_SEP_DEG: f64 = 89.0;

/// Arcsec per radian, for converting tangent-plane radians to the arcsec units
/// the affine fit is conditioned in. (Ratios are unit-invariant; the affine's
/// translation/scale are not — arcsec keeps `fit_affine_from_centers`
/// well-scaled.)
const ARCSEC_PER_RAD: f64 = 180.0 * 3600.0 / std::f64::consts::PI;

/// A successful per-cell fit. Carries only what the second-solve / WCS step
/// (Task 6 `refine`) needs: the pixel→tangent-plane affine, the tangent point
/// it was solved about, and how many quad correspondences supported it.
#[derive(Clone, Debug)]
pub struct TrialFit {
    /// Maps image pixel (x, y) → tangent-plane (ξ, η) **arcsec** about
    /// `center` (i.e. the affine fitted from matched quad centers).
    pub transform: AffineTransform,
    /// The tangent point (ra0, dec0) in degrees this trial projected about.
    pub center: (f64, f64),
    /// Number of quad correspondences that survived `match_quads`' median
    /// scale-ratio outlier filter and fed the affine fit.
    pub matched_quads: usize,
}

/// Project catalog stars onto the gnomonic tangent plane centered at
/// `(ra0, dec0)`, in **arcsec**. Stars on the far hemisphere are dropped
/// (never panic — the caller then just gets fewer/zero quads and skips).
pub fn project_catalog(stars: &[CatalogStar], ra0: f64, dec0: f64) -> Vec<(f64, f64)> {
    stars
        .iter()
        .filter(|s| angular_distance(s.ra, s.dec, ra0, dec0) <= MAX_PROJECTION_SEP_DEG)
        .map(|s| {
            let (xi, eta) =
                astroimage::platesolving::GnomonicProjection::sky_to_tangent(s.ra, s.dec, ra0, dec0);
            (xi * ARCSEC_PER_RAD, eta * ARCSEC_PER_RAD)
        })
        .collect()
}

/// ASTAP `minimum_quads = 3 + n/140`, floored at 3
/// (`unit_command_line_solving.pas:2592`), where `n` is the image star count.
pub fn minimum_quads(image_star_count: usize) -> usize {
    (3 + image_star_count / 140).max(3)
}

/// ASTAP `solve_image` search-window oversize factor
/// (`unit_command_line_solving.pas:2457-2465`): for sparse fields the
/// database read window is enlarged so *all* image quads are guaranteed to
/// have a catalog counterpart while stepping the spiral.
///   `< 35` stars → 2.0; `> 140` → 1.0; between → `2·sqrt(35/n)`.
/// Clamped to [1, 2] (ASTAP also clamps to one DB tile; 2 is our practical
/// ceiling — the brightest-N budget below, not area, bounds the pool).
pub fn oversize(n_img: usize) -> f64 {
    let o = if n_img < 35 {
        2.0
    } else if n_img > 140 {
        1.0
    } else {
        2.0 * (35.0 / n_img as f64).sqrt()
    };
    o.clamp(1.0, 2.0)
}

/// ASTAP catalog star budget for one trial: read the **brightest** this many
/// stars in the window so catalog star *density matches the image*
/// (`unit_command_line_solving.pas:2445` `nrstars_required =
/// round(n_img·height/width)`; `:2553` `nrstars_required2 =
/// round(nrstars_required·oversize²)`). This — not a fixed magnitude cut —
/// is what makes the image and catalog quad pools the same family.
/// Floored so a quad can always form.
pub fn catalog_star_budget(n_img: usize, image_w: u32, image_h: u32, oversize: f64) -> usize {
    let w = (image_w.max(1)) as f64;
    let h = (image_h.max(1)) as f64;
    let nrstars_required = (n_img as f64 * h / w).round();
    let n2 = (nrstars_required * oversize * oversize).round();
    (n2 as usize).max(8)
}

/// Run one (FOV, sky-cell) trial.
///
/// * `image_quads` — quads built once from the detected image stars (Task 6).
/// * `cat_stars` — the catalog cone for this cell (from `CatalogQuadSource`).
/// * `center` — `(ra0, dec0)` degrees: both the cone center *and* the gnomonic
///   tangent point, so the catalog is projected onto the image's flat plane.
/// * `group_size` — **must equal** the group size used to build `image_quads`
///   (ASTAP's `find_many_quads` ladder) so both quad sets are structurally the
///   same family.
/// * `quad_tolerance` — continuous ratio tolerance (ASTAP `find_fit`,
///   ≈0.007); `match_quads` accepts a pair when every ratio is within it.
/// * `min_quads` — minimum surviving correspondences to accept (see
///   [`minimum_quads`]).
///
/// Returns `None` (caller advances the spiral) on too-few catalog stars,
/// too-few matches, or a fit that fails `fit_affine_from_centers`' built-in
/// x/y-scale sanity. Never panics.
pub fn run_cell(
    image_quads: &[Quad],
    cat_stars: &[CatalogStar],
    center: (f64, f64),
    group_size: usize,
    quad_tolerance: f64,
    min_quads: usize,
) -> Option<TrialFit> {
    let (ra0, dec0) = center;
    let projected = project_catalog(cat_stars, ra0, dec0);
    if projected.len() < 4 {
        return None;
    }

    // Same function, same group-size ladder as the image side — this identity
    // is the whole fix.
    let catalog_quads = build_quads_multi(&projected, projected.len(), group_size);
    if catalog_quads.is_empty() {
        return None;
    }

    let matches = match_quads(image_quads, &catalog_quads, quad_tolerance);
    if matches.len() < min_quads {
        return None;
    }

    let transform = fit_affine_from_centers(&matches)?;
    Some(TrialFit {
        transform,
        center,
        matched_quads: matches.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn euclid(a: (f64, f64), b: (f64, f64)) -> f64 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
    }

    /// Mirror `pattern_matcher::push_quad_from_indices`' ratio math *exactly*
    /// (that is what the legacy quad index hashed): the 6 pairwise distances
    /// among 4 points, sorted ascending, the smallest 5 divided by the
    /// longest. `d` is the metric — Euclidean for flat planes, great-circle
    /// for raw sky (the legacy catalog side).
    fn quad_ratios<P: Copy>(p: [P; 4], d: impl Fn(P, P) -> f64) -> [f64; 5] {
        let mut dists = [0.0f64; 6];
        let mut k = 0;
        for a in 0..4 {
            for b in (a + 1)..4 {
                dists[k] = d(p[a], p[b]);
                k += 1;
            }
        }
        dists.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let longest = dists[5];
        [
            dists[0] / longest,
            dists[1] / longest,
            dists[2] / longest,
            dists[3] / longest,
            dists[4] / longest,
        ]
    }

    fn max_ratio_dev(x: &[f64; 5], y: &[f64; 5]) -> f64 {
        x.iter()
            .zip(y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
    }

    /// Build a deterministic *wide* synthetic field around a tangent point and
    /// return, for the same star set:
    ///   * `sky`      — true (ra, dec) of each star,
    ///   * `pixels`   — what a real camera (itself a gnomonic projector about
    ///                  the same boresight) records: a pure similarity
    ///                  (rotate + uniform-scale + translate) of the tangent
    ///                  plane,
    ///   * `(ra0, dec0)` — the field/boresight center.
    ///
    /// Because a camera *is* a gnomonic projector, `pixels` is an exact
    /// similarity of `sky_to_tangent(sky)`. So the catalog-projected quads and
    /// the image-pixel quads must be the *same family* — that is the invariant
    /// the rewrite restores.
    fn wide_field() -> (Vec<CatalogStar>, Vec<(f64, f64)>, f64, f64) {
        // ~20° across — the headerless wide-field DSLR regime the plan cites
        // for `_DSC5767`, where the legacy great-circle↔flat-pixel mismatch
        // is largest.
        field(2.9)
    }

    /// Synthetic field whose angular extent scales with `spread` (≈7° at
    /// 1.0, ≈20° at 2.9), used to demonstrate the *FOV growth law* of the
    /// legacy invariant error.
    fn field(spread: f64) -> (Vec<CatalogStar>, Vec<(f64, f64)>, f64, f64) {
        let (ra0, dec0) = (180.0_f64, 40.0_f64);
        let offs_deg: [(f64, f64); 9] = [
            (-2.7, -2.1),
            (2.8, -1.4),
            (-1.2, 2.7),
            (2.3, 2.2),
            (0.3, -2.9),
            (-2.9, 0.5),
            (1.5, 0.2),
            (-0.7, 1.6),
            (1.1, -1.0),
        ];
        let stars: Vec<CatalogStar> = offs_deg
            .iter()
            .enumerate()
            .map(|(i, &(dra, ddec))| CatalogStar {
                // RA offset scaled by cos(dec) so the on-sky spread is ~equal
                // in both axes (a realistic wide field, not RA-squashed).
                ra: ra0 + spread * dra / dec0.to_radians().cos(),
                dec: dec0 + spread * ddec,
                mag: 6.0 + i as f32 * 0.3,
            })
            .collect();

        // Camera: gnomonic about the SAME boresight, 1.5"/px, rotated 17°,
        // origin at image center (2000, 2000).
        let scale = 1.5_f64; // arcsec / px
        let (s, c) = 17.0_f64.to_radians().sin_cos();
        let pixels: Vec<(f64, f64)> = stars
            .iter()
            .map(|st| {
                let (xi, eta) = astroimage::platesolving::GnomonicProjection::sky_to_tangent(
                    st.ra, st.dec, ra0, dec0,
                );
                let (xa, ya) = (xi * ARCSEC_PER_RAD / scale, eta * ARCSEC_PER_RAD / scale);
                (c * xa - s * ya + 2000.0, s * xa + c * ya + 2000.0)
            })
            .collect();

        (stars, pixels, ra0, dec0)
    }

    /// Max |Δ| over the 5 normalized ratios of a fixed wide quad, for the
    /// field at `spread`, under the **new** invariant (image flat-pixel vs
    /// gnomonically-projected catalog) and the **legacy** invariant (image
    /// flat-pixel vs great-circle catalog — what the prebuilt index hashed).
    fn invariant_devs(spread: f64) -> (f64 /*d_new*/, f64 /*d_legacy*/) {
        let (stars, pixels, ra0, dec0) = field(spread);
        let proj = project_catalog(&stars, ra0, dec0);
        assert_eq!(proj.len(), stars.len(), "no star should be culled here");
        let gc = |a: (f64, f64), b: (f64, f64)| angular_distance(a.0, a.1, b.0, b.1);

        let img_r = quad_ratios([pixels[0], pixels[1], pixels[2], pixels[3]], euclid);
        let proj_r = quad_ratios([proj[0], proj[1], proj[2], proj[3]], euclid);
        let gc_r = quad_ratios(
            [
                (stars[0].ra, stars[0].dec),
                (stars[1].ra, stars[1].dec),
                (stars[2].ra, stars[2].dec),
                (stars[3].ra, stars[3].dec),
            ],
            gc,
        );
        (
            max_ratio_dev(&img_r, &proj_r),
            max_ratio_dev(&img_r, &gc_r),
        )
    }

    /// THE decisive test — proves the *mechanism* the research pinned down,
    /// not a contrived threshold:
    ///
    ///  1. The **new** invariant (image flat-pixel ≡ gnomonically-projected
    ///     catalog, both gnomonic about the same point) is *bit-exact* — a
    ///     pure similarity, so the quad is the identical family on both sides.
    ///  2. The **legacy** invariant (great-circle catalog vs flat-pixel image)
    ///     carries a real, systematic error that is *orders of magnitude*
    ///     larger and is eliminated by the fix.
    ///  3. That legacy error grows *super-linearly with FOV* — the O(θ²)
    ///     great-circle↔gnomonic divergence that is the architectural root
    ///     cause of the wide-field / dense (M78, `_DSC5767`) failures.
    #[test]
    fn projected_catalog_invariant_matches_image_legacy_does_not() {
        let (d_new, d_legacy) = invariant_devs(2.9); // ≈20° across (wide)
        let (_, d_legacy_narrow) = invariant_devs(1.0); // ≈7° across (narrow)

        // (1) New invariant is consistent to f64 rounding.
        assert!(
            d_new < 1e-6,
            "new invariant must match image to rounding, got {d_new}"
        );
        // (2) Legacy invariant carries a real wide-field skew, orders of
        // magnitude over the (near-zero) consistent path — i.e. the fix
        // removes it.
        assert!(
            d_legacy > 1e-4,
            "legacy great-circle invariant must carry a real wide-field skew, got {d_legacy}"
        );
        assert!(
            d_legacy > 1e3 * d_new.max(1e-12),
            "legacy error must dwarf the consistent path: legacy={d_legacy} new={d_new}"
        );
        // (3) Mechanism: the legacy error grows super-linearly with FOV
        // (great-circle vs gnomonic diverge O(θ²)). 2.9× the field span ⇒
        // ≫ 2.9× the error; assert > 5× (well clear of linear, robust).
        assert!(
            d_legacy > 5.0 * d_legacy_narrow,
            "legacy error must grow super-linearly with FOV: \
             wide={d_legacy} narrow={d_legacy_narrow} (ratio {})",
            d_legacy / d_legacy_narrow
        );
    }

    /// End-to-end per-cell pipeline: same star set, image quads vs projected
    /// catalog quads → `run_cell` finds the field and returns a sane affine.
    #[test]
    fn run_cell_solves_a_consistent_synthetic_field() {
        let (stars, pixels, ra0, dec0) = wide_field();
        let image_quads = build_quads_multi(&pixels, pixels.len(), 6);
        assert!(!image_quads.is_empty());

        let fit = run_cell(&image_quads, &stars, (ra0, dec0), 6, 0.007, 3)
            .expect("a perfectly-consistent field must solve");

        assert_eq!(fit.center, (ra0, dec0));
        assert!(fit.matched_quads >= 3);

        // The affine maps pixels → tangent-plane arcsec. We built pixels as a
        // 1.5"/px similarity, so each axis scale must come back ≈ 1.5"/px.
        let sx = (fit.transform.a1.powi(2) + fit.transform.b1.powi(2)).sqrt();
        let sy = (fit.transform.a2.powi(2) + fit.transform.b2.powi(2)).sqrt();
        assert!(
            (sx - 1.5).abs() < 0.05 && (sy - 1.5).abs() < 0.05,
            "recovered pixel scale should be ~1.5\"/px, got sx={sx} sy={sy}"
        );
    }

    #[test]
    fn too_few_catalog_stars_returns_none() {
        let (_s, pixels, ra0, dec0) = wide_field();
        let image_quads = build_quads_multi(&pixels, pixels.len(), 6);
        let three = vec![
            CatalogStar { ra: ra0, dec: dec0, mag: 5.0 },
            CatalogStar { ra: ra0 + 0.1, dec: dec0, mag: 5.0 },
            CatalogStar { ra: ra0, dec: dec0 + 0.1, mag: 5.0 },
        ];
        assert!(run_cell(&image_quads, &three, (ra0, dec0), 6, 0.007, 3).is_none());
    }

    #[test]
    fn far_hemisphere_stars_are_dropped_not_panicked() {
        // A star ~150° away must be filtered, not panic sky_to_tangent.
        let stars = vec![
            CatalogStar { ra: 180.0, dec: 40.0, mag: 5.0 },
            CatalogStar { ra: 0.0, dec: -40.0, mag: 5.0 },
        ];
        let proj = project_catalog(&stars, 180.0, 40.0);
        assert_eq!(proj.len(), 1, "the antipodal-ish star must be culled");
    }

    #[test]
    fn oversize_follows_astap_solve_image() {
        assert_eq!(oversize(20), 2.0); // < 35 → 2
        assert_eq!(oversize(34), 2.0);
        assert_eq!(oversize(35), 2.0); // 2·sqrt(35/35)
        assert_eq!(oversize(141), 1.0); // > 140 → 1
        assert_eq!(oversize(300), 1.0);
        // n=140 → 2·sqrt(35/140)=2·0.5=1.0 (boundary, not >140)
        assert!((oversize(140) - 1.0).abs() < 1e-9);
        // n=70 → 2·sqrt(0.5) ≈ 1.4142
        assert!((oversize(70) - 1.414_213_562).abs() < 1e-6);
    }

    #[test]
    fn catalog_budget_matches_image_density() {
        // ASTAP: nrstars_required = round(n·h/w); ·oversize².
        // 300 stars, 4000×2800, oversize 1 → round(300·0.7)=210.
        assert_eq!(catalog_star_budget(300, 4000, 2800, 1.0), 210);
        // oversize 2 → 210·4 = 840.
        assert_eq!(catalog_star_budget(300, 4000, 2800, 2.0), 840);
        // Square frame, oversize 1 → n itself.
        assert_eq!(catalog_star_budget(120, 1000, 1000, 1.0), 120);
        // Tiny field floored so a quad can still form.
        assert_eq!(catalog_star_budget(2, 4000, 4000, 1.0), 8);
    }

    #[test]
    fn minimum_quads_follows_astap_formula() {
        assert_eq!(minimum_quads(0), 3);
        assert_eq!(minimum_quads(139), 3);
        assert_eq!(minimum_quads(140), 4);
        assert_eq!(minimum_quads(1400), 13);
    }
}
