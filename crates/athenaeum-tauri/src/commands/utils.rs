// Shared utility functions for commands

/// Calculate field of view (FOV) in degrees given camera and telescope parameters
///
/// # Arguments
/// * `pixel_size_um` - Pixel size in micrometers
/// * `focal_length_mm` - Telescope focal length in millimeters
/// * `naxis` - Number of pixels along sensor dimension
/// * `binning` - Binning factor (1 = no binning, 2 = 2x2 binning, etc.)
///
/// # Returns
/// FOV in degrees, or None if parameters are invalid
pub fn calculate_fov(
    pixel_size_um: Option<f64>,
    focal_length_mm: Option<f64>,
    naxis: Option<i32>,
    _binning: Option<i32>,
) -> Option<f64> {
    match (pixel_size_um, focal_length_mm, naxis) {
        (Some(pixel_size), Some(focal_len), Some(sensor_pixels))
            if focal_len > 0.0 && sensor_pixels > 0 =>
        {
            // Convert pixel size from micrometers to millimeters
            let pixel_size_mm = pixel_size / 1000.0;

            // Calculate sensor dimension in mm
            // FITS XPIXSZ reports effective pixel size after binning, so no need to multiply by bin
            let sensor_mm = pixel_size_mm * sensor_pixels as f64;

            // FOV formula: FOV = 2 * arctan(sensor_mm / (2 * focal_length_mm)) * (180 / π)
            let fov_radians = 2.0 * (sensor_mm / (2.0 * focal_len)).atan();
            let fov_degrees = fov_radians.to_degrees();

            Some(fov_degrees)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_fov() {
        // Test with typical values (ASI294MC Pro with 200mm focal length)
        let fov = calculate_fov(Some(4.63), Some(200.0), Some(4144), Some(1));
        assert!(fov.is_some());
        let fov_val = fov.unwrap();
        assert!(fov_val > 5.0 && fov_val < 6.0); // Should be ~5.49 degrees
    }

    #[test]
    fn test_calculate_fov_none() {
        assert_eq!(calculate_fov(None, Some(200.0), Some(4144), Some(1)), None);
        assert_eq!(calculate_fov(Some(4.63), None, Some(4144), Some(1)), None);
        assert_eq!(calculate_fov(Some(4.63), Some(200.0), None, Some(1)), None);
    }
}
