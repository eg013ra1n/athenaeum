/// Astronomical algorithms for spatial selection queries

/// Calculate great circle distance between two sky positions using Haversine formula
///
/// # Arguments
/// * `ra1` - Right Ascension of point 1 in degrees (0-360 or -180-180)
/// * `dec1` - Declination of point 1 in degrees (-90 to +90)
/// * `ra2` - Right Ascension of point 2 in degrees (0-360 or -180-180)
/// * `dec2` - Declination of point 2 in degrees (-90 to +90)
///
/// # Returns
/// Angular distance in degrees
pub fn angular_distance(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    // Convert to radians
    let ra1_rad = ra1.to_radians();
    let dec1_rad = dec1.to_radians();
    let ra2_rad = ra2.to_radians();
    let dec2_rad = dec2.to_radians();

    // Haversine formula
    let dra = ra2_rad - ra1_rad;
    let ddec = dec2_rad - dec1_rad;

    let a = (ddec / 2.0).sin().powi(2)
        + dec1_rad.cos() * dec2_rad.cos() * (dra / 2.0).sin().powi(2);

    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    // Convert back to degrees
    c.to_degrees()
}

/// Test if a point is inside a polygon using ray casting algorithm
///
/// # Arguments
/// * `ra` - Right Ascension of test point in degrees
/// * `dec` - Declination of test point in degrees
/// * `vertices` - Polygon vertices as slice of (RA, Dec) tuples in degrees
///
/// # Returns
/// true if point is inside polygon, false otherwise
///
/// Note: This uses a simple ray casting algorithm suitable for small polygons
/// on the sky. For very large polygons crossing RA boundaries (0/360),
/// consider additional normalization.
pub fn point_in_polygon(ra: f64, dec: f64, vertices: &[(f64, f64)]) -> bool {
    if vertices.len() < 3 {
        return false;
    }

    let mut inside = false;
    let n = vertices.len();

    for i in 0..n {
        let j = (i + n - 1) % n;
        let (ra_i, dec_i) = vertices[i];
        let (ra_j, dec_j) = vertices[j];

        // Ray casting: check if ray from point crosses polygon edge
        if ((dec_i > dec) != (dec_j > dec))
            && (ra < (ra_j - ra_i) * (dec - dec_i) / (dec_j - dec_i) + ra_i)
        {
            inside = !inside;
        }
    }

    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_angular_distance_same_point() {
        let distance = angular_distance(0.0, 0.0, 0.0, 0.0);
        assert!((distance - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_angular_distance_90_degrees() {
        // Points on equator, 90 degrees apart
        let distance = angular_distance(0.0, 0.0, 90.0, 0.0);
        assert!((distance - 90.0).abs() < 0.01);
    }

    #[test]
    fn test_angular_distance_pole_to_equator() {
        // North pole to equator
        let distance = angular_distance(0.0, 90.0, 0.0, 0.0);
        assert!((distance - 90.0).abs() < 0.01);
    }

    #[test]
    fn test_point_in_polygon_inside() {
        // Simple square: [0,0], [10,0], [10,10], [0,10]
        let vertices = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(point_in_polygon(5.0, 5.0, &vertices));
    }

    #[test]
    fn test_point_in_polygon_outside() {
        let vertices = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(!point_in_polygon(15.0, 5.0, &vertices));
    }

    #[test]
    fn test_point_in_polygon_edge() {
        let vertices = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        // Point on edge - ray casting may or may not include this
        // Just test that function runs without panic
        let _ = point_in_polygon(5.0, 0.0, &vertices);
    }

    #[test]
    fn test_point_in_polygon_triangle() {
        // Triangle: [0,0], [10,0], [5,10]
        let vertices = [(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)];
        assert!(point_in_polygon(5.0, 5.0, &vertices));
        assert!(!point_in_polygon(0.0, 10.0, &vertices));
    }
}
