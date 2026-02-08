// Shared utility functions for commands

/// Strip Windows extended-length path prefix that canonicalize() adds.
/// Handles three cases:
///   \\?\C:\...        → C:\...          (local path)
///   \\?\UNC\server\.. → \\server\..     (network UNC path)
///   \\server\share\.. → unchanged       (regular UNC, no prefix)
/// On non-Windows platforms, this is a no-op.
pub fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{}", stripped));
        }
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

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
        (Some(pixel_size), Some(focal_len), Some(sensor_pixels)) if focal_len > 0.0 && sensor_pixels > 0 => {
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

/// Calculate angular distance between two points on the celestial sphere
///
/// Uses the haversine formula to compute the great circle distance.
///
/// # Arguments
/// * `ra1_deg` - Right Ascension of point 1 in degrees
/// * `dec1_deg` - Declination of point 1 in degrees
/// * `ra2_deg` - Right Ascension of point 2 in degrees
/// * `dec2_deg` - Declination of point 2 in degrees
///
/// # Returns
/// Angular distance in degrees
#[allow(dead_code)]
pub fn angular_distance(ra1_deg: f64, dec1_deg: f64, ra2_deg: f64, dec2_deg: f64) -> f64 {
    let ra1 = ra1_deg.to_radians();
    let dec1 = dec1_deg.to_radians();
    let ra2 = ra2_deg.to_radians();
    let dec2 = dec2_deg.to_radians();

    let delta_ra = ra1 - ra2;

    // Haversine formula
    let a = ((dec1 - dec2) / 2.0).sin();
    let b = (delta_ra / 2.0).sin();
    let c = dec1.cos() * dec2.cos();

    let angular_dist = 2.0 * ((a * a + c * b * b).sqrt()).asin();

    angular_dist.to_degrees()
}

/// Format byte count into human-readable format
///
/// # Arguments
/// * `bytes` - Number of bytes
///
/// # Returns
/// Formatted string (e.g., "1.23 GB", "456.78 MB", "789 KB", "123 bytes")
#[allow(dead_code)]
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
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

    #[test]
    fn test_angular_distance() {
        // Test with known values (0.5 degrees apart)
        let dist = angular_distance(0.0, 0.0, 0.5, 0.0);
        assert!((dist - 0.5).abs() < 0.0001);

        // Test with same point
        let dist = angular_distance(123.456, 45.678, 123.456, 45.678);
        assert!(dist.abs() < 0.0001);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
    }
}
