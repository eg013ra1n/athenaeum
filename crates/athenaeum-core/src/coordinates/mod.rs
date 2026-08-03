// Coordinate parsing and conversion utilities for astronomical coordinates

use anyhow::{anyhow, Result};

/// Parse Right Ascension from sexagesimal string to decimal degrees
/// Accepts formats like "HH:MM:SS.sss" or "HH MM SS.sss"
/// RA range: 0-24 hours -> 0-360 degrees
pub fn parse_ra_sexagesimal(ra_str: &str) -> Result<f64> {
    let parts: Vec<&str> = ra_str
        .trim()
        .split(|c: char| c == ':' || c == ' ')
        .filter(|s| !s.is_empty())
        .collect();

    if parts.len() < 2 || parts.len() > 3 {
        return Err(anyhow!("Invalid RA format: {}", ra_str));
    }

    let hours: f64 = parts[0]
        .parse()
        .map_err(|_| anyhow!("Invalid hours in RA: {}", parts[0]))?;
    let minutes: f64 = parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid minutes in RA: {}", parts[1]))?;
    let seconds: f64 = if parts.len() == 3 {
        parts[2]
            .parse()
            .map_err(|_| anyhow!("Invalid seconds in RA: {}", parts[2]))?
    } else {
        0.0
    };

    // Validate ranges
    if hours < 0.0 || hours >= 24.0 {
        return Err(anyhow!("RA hours out of range [0, 24): {}", hours));
    }
    if minutes < 0.0 || minutes >= 60.0 {
        return Err(anyhow!("RA minutes out of range [0, 60): {}", minutes));
    }
    if seconds < 0.0 || seconds >= 60.0 {
        return Err(anyhow!("RA seconds out of range [0, 60): {}", seconds));
    }

    // Convert to degrees (15 degrees per hour)
    let degrees = (hours + minutes / 60.0 + seconds / 3600.0) * 15.0;
    Ok(degrees)
}

/// Parse Declination from sexagesimal string to decimal degrees
/// Accepts formats like "±DD:MM:SS.sss" or "±DD MM SS.sss"
/// Dec range: -90 to +90 degrees
pub fn parse_dec_sexagesimal(dec_str: &str) -> Result<f64> {
    let trimmed = dec_str.trim();

    // Determine sign
    let (sign, rest) = if trimmed.starts_with('-') {
        (-1.0, &trimmed[1..])
    } else if trimmed.starts_with('+') {
        (1.0, &trimmed[1..])
    } else {
        (1.0, trimmed)
    };

    let parts: Vec<&str> = rest
        .split(|c: char| c == ':' || c == ' ')
        .filter(|s| !s.is_empty())
        .collect();

    if parts.len() < 2 || parts.len() > 3 {
        return Err(anyhow!("Invalid Dec format: {}", dec_str));
    }

    let degrees: f64 = parts[0]
        .parse()
        .map_err(|_| anyhow!("Invalid degrees in Dec: {}", parts[0]))?;
    let minutes: f64 = parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid arcminutes in Dec: {}", parts[1]))?;
    let seconds: f64 = if parts.len() == 3 {
        parts[2]
            .parse()
            .map_err(|_| anyhow!("Invalid arcseconds in Dec: {}", parts[2]))?
    } else {
        0.0
    };

    // Validate ranges
    if degrees < 0.0 || degrees > 90.0 {
        return Err(anyhow!("Dec degrees out of range [0, 90]: {}", degrees));
    }
    if minutes < 0.0 || minutes >= 60.0 {
        return Err(anyhow!("Dec arcminutes out of range [0, 60): {}", minutes));
    }
    if seconds < 0.0 || seconds >= 60.0 {
        return Err(anyhow!("Dec arcseconds out of range [0, 60): {}", seconds));
    }

    // Convert to decimal degrees
    let decimal = degrees + minutes / 60.0 + seconds / 3600.0;
    Ok(sign * decimal)
}

/// Format decimal degrees RA to sexagesimal string (HH:MM:SS.s)
pub fn format_ra_sexagesimal(degrees: f64) -> String {
    let hours = degrees / 15.0;
    let h = hours.floor();
    let minutes = (hours - h) * 60.0;
    let m = minutes.floor();
    let seconds = (minutes - m) * 60.0;

    format!("{:02.0}:{:02.0}:{:04.1}", h, m, seconds)
}

/// Format decimal degrees Dec to sexagesimal string (±DD:MM:SS.s)
pub fn format_dec_sexagesimal(degrees: f64) -> String {
    let sign = if degrees < 0.0 { "-" } else { "+" };
    let abs_degrees = degrees.abs();
    let d = abs_degrees.floor();
    let minutes = (abs_degrees - d) * 60.0;
    let m = minutes.floor();
    let seconds = (minutes - m) * 60.0;

    format!("{}{:02.0}:{:02.0}:{:04.1}", sign, d, m, seconds)
}

/// Calculate great-circle angular distance between two points using spherical law of cosines
/// More numerically stable than haversine for most astronomical use cases
/// Input: RA and Dec in decimal degrees
/// Output: Angular separation in degrees
pub fn angular_distance(ra1_deg: f64, dec1_deg: f64, ra2_deg: f64, dec2_deg: f64) -> f64 {
    // Convert to radians
    let ra1 = ra1_deg.to_radians();
    let dec1 = dec1_deg.to_radians();
    let ra2 = ra2_deg.to_radians();
    let dec2 = dec2_deg.to_radians();

    // Spherical law of cosines
    let cos_angle = dec1.sin() * dec2.sin() + dec1.cos() * dec2.cos() * (ra2 - ra1).cos();

    // Clamp to [-1, 1] to handle floating point errors
    let cos_angle = cos_angle.clamp(-1.0, 1.0);

    // Return in degrees
    cos_angle.acos().to_degrees()
}

/// Convert 3D Cartesian unit vector to RA/Dec (degrees)
pub fn cartesian_to_radec(x: f64, y: f64, z: f64) -> (f64, f64) {
    let dec_rad = z.asin();
    let ra_rad = y.atan2(x);

    let ra_deg = ra_rad.to_degrees();
    let dec_deg = dec_rad.to_degrees();

    // Normalize RA to [0, 360)
    let ra_normalized = if ra_deg < 0.0 { ra_deg + 360.0 } else { ra_deg };

    (ra_normalized, dec_deg)
}

/// Convert RA/Dec (degrees) to 3D Cartesian unit vector
pub fn radec_to_cartesian(ra_deg: f64, dec_deg: f64) -> (f64, f64, f64) {
    let ra_rad = ra_deg.to_radians();
    let dec_rad = dec_deg.to_radians();

    let x = dec_rad.cos() * ra_rad.cos();
    let y = dec_rad.cos() * ra_rad.sin();
    let z = dec_rad.sin();

    (x, y, z)
}

/// Calculate spherical mean of multiple RA/Dec positions
/// This is more accurate than arithmetic mean for spherical coordinates
/// Input: Vector of (RA, Dec) pairs in decimal degrees
/// Output: Mean (RA, Dec) in decimal degrees
pub fn spherical_mean(coords: &[(f64, f64)]) -> Result<(f64, f64)> {
    if coords.is_empty() {
        return Err(anyhow!("Cannot compute mean of empty coordinate list"));
    }

    // Convert all to Cartesian unit vectors and sum
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_z = 0.0;

    for &(ra, dec) in coords {
        let (x, y, z) = radec_to_cartesian(ra, dec);
        sum_x += x;
        sum_y += y;
        sum_z += z;
    }

    // Normalize the sum vector
    let magnitude = (sum_x.powi(2) + sum_y.powi(2) + sum_z.powi(2)).sqrt();

    if magnitude < 1e-10 {
        return Err(anyhow!(
            "Coordinates are too dispersed to compute a meaningful mean"
        ));
    }

    let mean_x = sum_x / magnitude;
    let mean_y = sum_y / magnitude;
    let mean_z = sum_z / magnitude;

    // Convert back to RA/Dec
    Ok(cartesian_to_radec(mean_x, mean_y, mean_z))
}

/// Normalize RA to [0, 360) range
pub fn normalize_ra(ra_deg: f64) -> f64 {
    let mut normalized = ra_deg % 360.0;
    if normalized < 0.0 {
        normalized += 360.0;
    }
    normalized
}

/// Normalize Dec to [-90, 90] range
pub fn normalize_dec(dec_deg: f64) -> f64 {
    dec_deg.clamp(-90.0, 90.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ra_sexagesimal() {
        // Test basic format
        let ra = parse_ra_sexagesimal("12:30:45.5").unwrap();
        assert!((ra - 187.689583).abs() < 0.0001);

        // Test without seconds
        let ra = parse_ra_sexagesimal("12:30").unwrap();
        assert!((ra - 187.5).abs() < 0.0001);

        // Test with spaces
        let ra = parse_ra_sexagesimal("12 30 45.5").unwrap();
        assert!((ra - 187.689583).abs() < 0.0001);
    }

    #[test]
    fn test_parse_dec_sexagesimal() {
        // Test positive
        let dec = parse_dec_sexagesimal("+45:30:15.5").unwrap();
        assert!((dec - 45.504306).abs() < 0.0001);

        // Test negative
        let dec = parse_dec_sexagesimal("-45:30:15.5").unwrap();
        assert!((dec + 45.504306).abs() < 0.0001);

        // Test without sign
        let dec = parse_dec_sexagesimal("45:30:15.5").unwrap();
        assert!((dec - 45.504306).abs() < 0.0001);
    }

    #[test]
    fn test_format_roundtrip() {
        let ra_deg = 187.689583;
        let formatted = format_ra_sexagesimal(ra_deg);
        let parsed = parse_ra_sexagesimal(&formatted).unwrap();
        assert!((parsed - ra_deg).abs() < 0.01); // Within 0.01 degrees
    }

    #[test]
    fn test_angular_distance() {
        // Same point
        let dist = angular_distance(0.0, 0.0, 0.0, 0.0);
        assert!(dist.abs() < 1e-10);

        // 90 degrees apart
        let dist = angular_distance(0.0, 0.0, 90.0, 0.0);
        assert!((dist - 90.0).abs() < 0.001);

        // Pole to equator
        let dist = angular_distance(0.0, 90.0, 0.0, 0.0);
        assert!((dist - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_spherical_mean() {
        // Test mean of two points
        let coords = vec![(0.0, 0.0), (10.0, 0.0)];
        let (ra, dec) = spherical_mean(&coords).unwrap();
        assert!((ra - 5.0).abs() < 0.1);
        assert!(dec.abs() < 0.1);

        // Test mean around north pole
        let coords = vec![(0.0, 89.0), (90.0, 89.0), (180.0, 89.0), (270.0, 89.0)];
        let (_, dec) = spherical_mean(&coords).unwrap();
        assert!(dec > 88.0); // Should be close to pole
    }

    #[test]
    fn test_cartesian_roundtrip() {
        let ra = 123.456;
        let dec = 45.678;
        let (x, y, z) = radec_to_cartesian(ra, dec);
        let (ra2, dec2) = cartesian_to_radec(x, y, z);
        assert!((ra - ra2).abs() < 0.0001);
        assert!((dec - dec2).abs() < 0.0001);
    }

    #[test]
    fn test_normalize_ra() {
        assert!((normalize_ra(370.0) - 10.0).abs() < 1e-10);
        assert!((normalize_ra(-10.0) - 350.0).abs() < 1e-10);
        assert!((normalize_ra(180.0) - 180.0).abs() < 1e-10);
    }

    #[test]
    fn test_normalize_dec() {
        assert_eq!(normalize_dec(95.0), 90.0);
        assert_eq!(normalize_dec(-95.0), -90.0);
        assert_eq!(normalize_dec(45.0), 45.0);
    }
}
