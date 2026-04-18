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
    let lon_rad = ra_deg.to_radians();
    let lat_rad = dec_deg.to_radians();
    let radius_rad = radius_deg.to_radians();

    let bmoc = nested::cone_coverage_approx(HEALPIX_DEPTH, lon_rad, lat_rad, radius_rad);

    let mut pixels: Vec<u64> = bmoc.flat_iter().collect();
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
