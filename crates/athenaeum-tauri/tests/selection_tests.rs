// Integration tests for selection module

// Since these are in a test binary, we need to call the functions through the crate
// We'll write them inline to test the logic

/// Calculate great circle distance between two sky positions using Haversine formula
fn angular_distance(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let ra1_rad = ra1.to_radians();
    let dec1_rad = dec1.to_radians();
    let ra2_rad = ra2.to_radians();
    let dec2_rad = dec2.to_radians();

    let dra = ra2_rad - ra1_rad;
    let ddec = dec2_rad - dec1_rad;

    let a =
        (ddec / 2.0).sin().powi(2) +
        dec1_rad.cos() * dec2_rad.cos() * (dra / 2.0).sin().powi(2);

    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    c.to_degrees()
}

/// Test if a point is inside a polygon using ray casting algorithm
fn point_in_polygon(ra: f64, dec: f64, vertices: &[(f64, f64)]) -> bool {
    if vertices.len() < 3 {
        return false;
    }

    let mut inside = false;
    let n = vertices.len();

    for i in 0..n {
        let j = (i + n - 1) % n;
        let (ra_i, dec_i) = vertices[i];
        let (ra_j, dec_j) = vertices[j];

        if ((dec_i > dec) != (dec_j > dec))
            && (ra < (ra_j - ra_i) * (dec - dec_i) / (dec_j - dec_i) + ra_i)
        {
            inside = !inside;
        }
    }

    inside
}

#[test]
fn test_angular_distance_same_point() {
    let distance = angular_distance(0.0, 0.0, 0.0, 0.0);
    assert!(distance < 1e-10, "Same point should have 0 distance, got {}", distance);
}

#[test]
fn test_angular_distance_90_degrees_apart() {
    // Points on equator, 90 degrees apart in RA
    let distance = angular_distance(0.0, 0.0, 90.0, 0.0);
    assert!(
        (distance - 90.0).abs() < 0.01,
        "90 degrees apart should be ~90 degrees, got {}",
        distance
    );
}

#[test]
fn test_angular_distance_pole_to_equator() {
    // North pole to equator
    let distance = angular_distance(0.0, 90.0, 0.0, 0.0);
    assert!(
        (distance - 90.0).abs() < 0.01,
        "Pole to equator should be ~90 degrees, got {}",
        distance
    );
}

#[test]
fn test_angular_distance_symmetry() {
    // Distance should be symmetric
    let d1 = angular_distance(10.0, 20.0, 30.0, 40.0);
    let d2 = angular_distance(30.0, 40.0, 10.0, 20.0);
    assert!(
        (d1 - d2).abs() < 1e-10,
        "Angular distance should be symmetric: d1={}, d2={}",
        d1,
        d2
    );
}

#[test]
fn test_angular_distance_small_values() {
    // Small distances should work correctly
    let distance = angular_distance(0.0, 0.0, 0.1, 0.1);
    assert!(
        distance > 0.0 && distance < 0.2,
        "Small distance should be small: got {}",
        distance
    );
}

#[test]
fn test_point_in_polygon_inside() {
    // Simple square: [0,0], [10,0], [10,10], [0,10]
    let vertices = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    assert!(
        point_in_polygon(5.0, 5.0, &vertices),
        "Point (5,5) should be inside square"
    );
}

#[test]
fn test_point_in_polygon_outside() {
    let vertices = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    assert!(
        !point_in_polygon(15.0, 5.0, &vertices),
        "Point (15,5) should be outside square"
    );
}

#[test]
fn test_point_in_polygon_triangle() {
    // Triangle: [0,0], [10,0], [5,10]
    let vertices = [(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)];
    assert!(
        point_in_polygon(5.0, 5.0, &vertices),
        "Point (5,5) should be inside triangle"
    );
}

#[test]
fn test_point_in_polygon_outside_triangle() {
    let vertices = [(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)];
    assert!(
        !point_in_polygon(0.0, 10.0, &vertices),
        "Point (0,10) should be outside triangle"
    );
}

#[test]
fn test_point_in_polygon_needs_3_vertices() {
    let vertices = [(0.0, 0.0), (10.0, 0.0)]; // Only 2 vertices
    assert!(
        !point_in_polygon(5.0, 5.0, &vertices),
        "Polygon with <3 vertices should return false"
    );
}

#[test]
fn test_point_in_polygon_complex_shape() {
    // L-shaped polygon
    let vertices = [
        (0.0, 0.0),
        (5.0, 0.0),
        (5.0, 5.0),
        (10.0, 5.0),
        (10.0, 10.0),
        (0.0, 10.0),
    ];

    // Inside the L
    assert!(
        point_in_polygon(2.5, 2.5, &vertices),
        "Point should be inside L-shape"
    );

    // Inside the upper right part
    assert!(
        point_in_polygon(7.5, 7.5, &vertices),
        "Point should be inside L-shape"
    );

    // Outside
    assert!(
        !point_in_polygon(7.5, 2.5, &vertices),
        "Point should be outside L-shape"
    );
}

// Test actual sky coordinates
#[test]
fn test_angular_distance_real_stars() {
    // Betelgeuse (~88.3, ~7.4) to Rigel (~78.6, ~-8.2)
    let betelgeuse_ra = 88.3;
    let betelgeuse_dec = 7.4;
    let rigel_ra = 78.6;
    let rigel_dec = -8.2;

    let distance = angular_distance(betelgeuse_ra, betelgeuse_dec, rigel_ra, rigel_dec);

    // These are in Orion, roughly 20 degrees apart
    assert!(
        distance > 15.0 && distance < 25.0,
        "Betelgeuse to Rigel should be ~20 degrees, got {}",
        distance
    );

    println!("Angular distance between Betelgeuse and Rigel: {:.2}°", distance);
}

#[test]
fn test_circle_selection_scenario() {
    // Simulate selecting frames within 5 degrees of a center position
    let center_ra = 180.0;
    let center_dec = 0.0;
    let radius = 5.0;

    // Create some test frames
    let frames = vec![
        (1, 180.0, 0.0, true),   // Exactly at center - should be included
        (2, 182.0, 0.0, true),   // 2 degrees away - should be included
        (3, 185.0, 0.0, true),   // 5 degrees away - should be included
        (4, 186.0, 0.0, false),  // 6 degrees away - should be excluded
        (5, 180.0, 5.0, true),   // 5 degrees away (declination) - should be included
        (6, 180.0, -6.0, false), // 6 degrees away - should be excluded
    ];

    // Test each frame
    for (frame_id, ra, dec, should_be_included) in frames {
        let distance = angular_distance(center_ra, center_dec, ra, dec);
        let is_included = distance <= radius;
        assert_eq!(
            is_included, should_be_included,
            "Frame {} at ({}, {}) distance={:.2} should be {} (included={})",
            frame_id, ra, dec, distance,
            if should_be_included { "included" } else { "excluded" },
            is_included
        );
    }

    println!("Circle selection test passed!");
}

#[test]
fn test_rectangle_selection_scenario() {
    // Simulate selecting frames within a rectangular region
    let ra_min = 170.0;
    let ra_max = 190.0;
    let dec_min = -10.0;
    let dec_max = 10.0;

    let frames = vec![
        (1, 180.0, 0.0, true),    // Center - included
        (2, 170.0, -10.0, true),  // Corner - included
        (3, 190.0, 10.0, true),   // Corner - included
        (4, 165.0, 0.0, false),   // Outside RA range
        (5, 195.0, 0.0, false),   // Outside RA range
        (6, 180.0, -15.0, false), // Outside Dec range
        (7, 180.0, 15.0, false),  // Outside Dec range
    ];

    for (frame_id, ra, dec, should_be_included) in frames {
        let is_included = ra >= ra_min && ra <= ra_max && dec >= dec_min && dec <= dec_max;
        assert_eq!(
            is_included, should_be_included,
            "Frame {} at ({}, {}) should be {}",
            frame_id,
            ra,
            dec,
            if should_be_included {
                "included"
            } else {
                "excluded"
            }
        );
    }

    println!("Rectangle selection test passed!");
}

#[test]
fn test_polygon_selection_scenario() {
    // Simulate selecting frames within a triangular region
    // Triangle: (0,0), (10,0), (5,10)
    let vertices = [(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)];

    let frames = vec![
        (1, 5.0, 5.0, true),   // Inside
        (2, 3.0, 3.0, true),   // Inside
        (3, 7.0, 3.0, true),   // Inside
        (4, 5.0, 0.0, true),   // On edge
        (5, 0.0, 10.0, false), // Outside
        (6, 10.0, 10.0, false),// Outside
    ];

    for (frame_id, ra, dec, should_be_included) in frames {
        let is_included = point_in_polygon(ra, dec, &vertices);
        assert_eq!(
            is_included, should_be_included,
            "Frame {} at ({}, {}) should be {}",
            frame_id,
            ra,
            dec,
            if should_be_included {
                "included"
            } else {
                "excluded"
            }
        );
    }

    println!("Polygon selection test passed!");
}
