use cdshealpix::nested;

/// HEALpix depth for catalog indexing. Level 6 = 49,152 pixels, ~0.84° each.
pub const HEALPIX_DEPTH: u8 = 6;

/// Total number of HEALpix pixels at our depth.
pub fn n_pixels() -> u64 {
    nested::n_hash(HEALPIX_DEPTH)
}

/// Get the HEALpix pixel ID for a given sky position.
///
/// `ra_deg`, `dec_deg`: sky position in degrees.
/// Returns the nested HEALpix pixel index at HEALPIX_DEPTH.
pub fn sky_to_pixel(ra_deg: f64, dec_deg: f64) -> u64 {
    let lon_rad = ra_deg.to_radians();
    let lat_rad = dec_deg.to_radians();
    nested::hash(HEALPIX_DEPTH, lon_rad, lat_rad)
}

/// Find all HEALpix pixels that overlap with a cone (circle on the sky).
///
/// `ra_deg`, `dec_deg`: cone center in degrees.
/// `radius_deg`: cone radius in degrees.
/// Returns a sorted, deduplicated list of pixel IDs.
pub fn cone_search_pixels(ra_deg: f64, dec_deg: f64, radius_deg: f64) -> Vec<u64> {
    // `cdshealpix::cone_coverage_approx` asserts on its inputs: a non-finite
    // or out-of-domain centre/radius panics. The plate-solve verifier can
    // hand us a degenerate seed WCS (huge/NaN implied pixel scale → absurd
    // cone radius) when solving without a scale hint; treat any such input
    // as "no pixels" so the candidate simply fails verification instead of
    // crashing the whole batch.
    if !ra_deg.is_finite() || !dec_deg.is_finite() || !radius_deg.is_finite() || radius_deg <= 0.0
    {
        return Vec::new();
    }
    // A real catalog cone is at most a few degrees; clamp well inside
    // cdshealpix's valid domain (< π) as a final safety net.
    let radius_deg = radius_deg.min(90.0);

    let lon_rad = ra_deg.to_radians();

    // cdshealpix 0.7.3 has a floating-point edge bug in
    // `largest_center_to_vertex_distance_with_radius` (lib.rs:547/577):
    // for certain cones whose centre latitude ± radius straddles the HEALPix
    // region boundary at ±arcsin(2/3) (~41.81°) it asserts and panics —
    // including on perfectly ordinary few-degree catalog cones (e.g. a real
    // M 31 field at Dec ≈ 41.71°). An unguarded panic here aborts the entire
    // plate-solve batch (every frame, not just this one). It is a knife-edge
    // numeric condition, so nudging the radius by a hair clears it while
    // still covering the field; try a few nudges, then fall back to "no
    // pixels" (candidate fails verification) rather than crash the batch —
    // the same contract as the non-finite/zero-radius guard above.
    let try_cover = |r_deg: f64| -> Option<Vec<u64>> {
        let lat_rad = dec_deg.to_radians();
        let radius_rad = r_deg.to_radians();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            nested::cone_coverage_approx(HEALPIX_DEPTH, lon_rad, lat_rad, radius_rad)
        }))
        .ok()
        .map(|bmoc| bmoc.flat_iter().collect())
    };

    // First the requested radius; then tiny over/under nudges (still covers
    // the field, just shifts off the cdshealpix knife-edge).
    let pixels: Option<Vec<u64>> = try_cover(radius_deg)
        .or_else(|| try_cover(radius_deg * 1.0001))
        .or_else(|| try_cover(radius_deg * 0.9999))
        .or_else(|| try_cover(radius_deg * 1.01))
        .or_else(|| try_cover(radius_deg * 0.99));

    let Some(mut pixels) = pixels else {
        tracing::warn!(
            ra_deg,
            dec_deg,
            radius_deg,
            "cdshealpix panicked at every nudged radius, returning empty cone"
        );
        return Vec::new();
    };
    pixels.sort_unstable();
    pixels.dedup();
    pixels
}

/// Total number of HEALpix pixels at a given depth.
pub fn n_pixels_at_depth(depth: u8) -> u64 {
    nested::n_hash(depth)
}

/// Get the center (RA, Dec) in degrees of a HEALpix pixel.
pub fn pixel_center(depth: u8, pixel: u64) -> (f64, f64) {
    let (lon_rad, lat_rad) = nested::center(depth, pixel);
    (lon_rad.to_degrees(), lat_rad.to_degrees())
}

/// Get all depth-6 sub-pixel IDs that fall within a pixel at a coarser depth.
/// In the nested scheme, pixel P at depth D contains sub-pixels
/// P * 4^(6-D) through (P+1) * 4^(6-D) - 1 at depth 6.
pub fn sub_pixels_at_depth6(pixel: u64, depth: u8) -> Vec<u64> {
    if depth >= HEALPIX_DEPTH {
        return vec![pixel];
    }
    let factor = 4u64.pow((HEALPIX_DEPTH - depth) as u32);
    let start = pixel * factor;
    let end = start + factor;
    (start..end).collect()
}

/// Get all depth-6 sub-pixel IDs for a pixel AND its neighbors at a given depth.
pub fn region_sub_pixels(pixel: u64, depth: u8) -> Vec<u64> {
    let mut coarse_pixels = Vec::with_capacity(10);
    coarse_pixels.push(pixel);
    nested::append_bulk_neighbours(depth, pixel, &mut coarse_pixels);

    let mut depth6_pixels = Vec::new();
    for &cp in &coarse_pixels {
        depth6_pixels.extend(sub_pixels_at_depth6(cp, depth));
    }
    depth6_pixels.sort_unstable();
    depth6_pixels.dedup();
    depth6_pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_pixels_level_6() {
        assert_eq!(n_pixels(), 49152);
    }

    #[test]
    fn sky_to_pixel_deterministic() {
        let p1 = sky_to_pixel(180.0, 45.0);
        let p2 = sky_to_pixel(180.0, 45.0);
        assert_eq!(p1, p2);
        assert!(p1 < n_pixels());
    }

    #[test]
    fn cone_search_returns_pixels() {
        let pixels = cone_search_pixels(180.0, 45.0, 2.0);
        assert!(!pixels.is_empty(), "Cone search should return pixels");
        // 2° radius should cover roughly 10-30 pixels at level 6
        assert!(
            pixels.len() >= 5 && pixels.len() <= 100,
            "Expected 5-100 pixels for 2° cone, got {}",
            pixels.len()
        );
        // All pixels should be valid
        for &p in &pixels {
            assert!(p < n_pixels(), "Pixel {p} out of range");
        }
    }

    #[test]
    fn cone_search_ra_wraparound() {
        // Cone centered near RA=0, should get pixels on both sides
        let pixels = cone_search_pixels(0.5, 0.0, 2.0);
        assert!(!pixels.is_empty());
    }

    #[test]
    fn cone_search_near_pole() {
        let pixels = cone_search_pixels(0.0, 89.0, 2.0);
        assert!(!pixels.is_empty());
    }

    #[test]
    fn cone_search_invalid_radius_returns_empty_no_panic() {
        // Regression: a degenerate plate-solve seed WCS (no scale hint in
        // the blind-scale fallback) can produce a non-finite or absurd cone
        // radius. cdshealpix's cone_coverage_approx asserts and panics on
        // such input — which used to abort the entire plate-solve batch.
        // These must return empty rather than panic.
        assert!(cone_search_pixels(180.0, 45.0, f64::NAN).is_empty());
        assert!(cone_search_pixels(180.0, 45.0, f64::INFINITY).is_empty());
        assert!(cone_search_pixels(180.0, 45.0, -1.0).is_empty());
        assert!(cone_search_pixels(180.0, 45.0, 0.0).is_empty());
        assert!(cone_search_pixels(f64::NAN, 45.0, 2.0).is_empty());
        assert!(cone_search_pixels(180.0, f64::NAN, 2.0).is_empty());
    }

    #[test]
    fn cone_search_is_total_across_sky_and_radius() {
        // Regression: cdshealpix 0.7.3's
        // `largest_center_to_vertex_distance_with_radius` (lib.rs:547/577)
        // asserts and panics for certain cones whose centre latitude ± radius
        // straddles the HEALPix region boundary at ±arcsin(2/3) (~41.81°) —
        // including ordinary few-degree catalog cones. The plate-solve
        // verifier reaches `cone_search` (service.rs) with arbitrary
        // degenerate seed positions/radii from the blind-scale fallback, and
        // an unguarded panic there aborts the *entire* batch (every frame).
        // `cone_search_pixels` must be TOTAL: never panic for any finite
        // input — return a (possibly empty) pixel set instead. This sweeps
        // the boundary band, both hemispheres, the poles, and large radii;
        // any cdshealpix assertion must be caught internally so this test
        // simply completes without unwinding.
        let lat_of_square_cell_deg = (2.0_f64 / 3.0).asin().to_degrees(); // ~41.81
        let mut dec = -89.0_f64;
        while dec <= 89.0 {
            for &r in &[0.05_f64, 0.5, 1.0, 2.5, 4.25, 7.3, 15.0, 45.0, 90.0] {
                for &ra in &[0.327_f64, 90.013, 180.5, 269.91] {
                    // Must return without panicking (guard recovers any
                    // cdshealpix internal assertion).
                    let v = cone_search_pixels(ra, dec, r);
                    for &p in &v {
                        assert!(p < n_pixels(), "pixel {p} out of range");
                    }
                }
            }
            dec += 0.137; // off-grid step to hit irregular float landings
        }
        // Explicitly hammer right at both region boundaries with radii that
        // make the cone straddle them.
        for sign in [1.0_f64, -1.0] {
            let base = sign * lat_of_square_cell_deg;
            for d in [-0.3_f64, -0.05, -0.001, 0.0, 0.001, 0.05, 0.3] {
                for &r in &[0.5_f64, 1.0, 2.0, 4.0, 8.0] {
                    let _ = cone_search_pixels(123.456, base + d, r);
                }
            }
        }
    }

    #[test]
    fn cone_search_huge_radius_clamped_no_panic() {
        // An absurdly large (but finite) radius is clamped instead of
        // panicking inside cdshealpix; still returns a valid pixel set.
        let pixels = cone_search_pixels(180.0, 45.0, 1.0e9);
        assert!(!pixels.is_empty());
        for &p in &pixels {
            assert!(p < n_pixels());
        }
    }

    #[test]
    fn n_pixels_at_various_depths() {
        assert_eq!(n_pixels_at_depth(0), 12);
        assert_eq!(n_pixels_at_depth(1), 48);
        assert_eq!(n_pixels_at_depth(2), 192);
        assert_eq!(n_pixels_at_depth(3), 768);
        assert_eq!(n_pixels_at_depth(4), 3072);
        assert_eq!(n_pixels_at_depth(6), 49152);
    }

    #[test]
    fn pixel_center_valid() {
        let (ra, dec) = pixel_center(3, 0);
        assert!(ra >= 0.0 && ra < 360.0);
        assert!(dec >= -90.0 && dec <= 90.0);
    }

    #[test]
    fn sub_pixels_correct_count() {
        // Depth 3 pixel → depth 6: 4^3 = 64 sub-pixels
        let subs = sub_pixels_at_depth6(0, 3);
        assert_eq!(subs.len(), 64);
        // Depth 6 pixel → itself
        let subs6 = sub_pixels_at_depth6(100, 6);
        assert_eq!(subs6.len(), 1);
        assert_eq!(subs6[0], 100);
    }

    #[test]
    fn region_sub_pixels_includes_neighbors() {
        let subs = region_sub_pixels(0, 3);
        // Center (64 sub-pixels) + up to 8 neighbors (64 each) = 64 to 576
        assert!(subs.len() >= 64, "Should include at least the center region");
        assert!(subs.len() <= 576, "At most 9 regions × 64 = 576");
    }
}
