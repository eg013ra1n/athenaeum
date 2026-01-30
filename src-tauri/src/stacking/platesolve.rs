//! Plate solving integration
//!
//! This module provides integration with external plate solving tools
//! like astrometry.net for determining WCS (World Coordinate System)
//! solutions for images.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use crate::stacking::ffi::WcsSolution;
use std::path::Path;
use std::process::Command;
use serde::{Deserialize, Serialize};

/// Plate solve configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlateSolveConfig {
    /// Camera pixel size in micrometers
    pub pixel_size_um: f64,
    /// Telescope focal length in millimeters
    pub focal_length_mm: f64,
    /// Search radius in degrees (for blind solving)
    pub search_radius_deg: f64,
    /// Initial RA hint (degrees, optional)
    pub initial_ra: Option<f64>,
    /// Initial Dec hint (degrees, optional)
    pub initial_dec: Option<f64>,
    /// Time limit for solving in seconds
    pub timeout_seconds: u32,
    /// Downsample factor (1 = no downsampling)
    pub downsample: u32,
    /// Path to astrometry.net solve-field binary
    pub solver_path: Option<String>,
}

impl Default for PlateSolveConfig {
    fn default() -> Self {
        PlateSolveConfig {
            pixel_size_um: 3.76,
            focal_length_mm: 500.0,
            search_radius_deg: 10.0,
            initial_ra: None,
            initial_dec: None,
            timeout_seconds: 60,
            downsample: 2,
            solver_path: None,
        }
    }
}

impl PlateSolveConfig {
    /// Calculate expected image scale in arcseconds per pixel
    pub fn expected_scale(&self) -> f64 {
        if self.focal_length_mm <= 0.0 {
            return 0.0;
        }
        // Scale = 206.265 * pixel_size_um / focal_length_mm (arcsec/pixel)
        206.265 * self.pixel_size_um / self.focal_length_mm
    }
}

/// Plate solve result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlateSolveResult {
    /// Whether solving succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Solved center RA (degrees)
    pub ra_center: f64,
    /// Solved center Dec (degrees)
    pub dec_center: f64,
    /// Image scale (arcseconds per pixel)
    pub scale_arcsec_per_pixel: f64,
    /// Rotation angle (degrees, east of north)
    pub rotation_deg: f64,
    /// WCS solution (if available)
    pub wcs: Option<WcsSolution>,
}

impl Default for PlateSolveResult {
    fn default() -> Self {
        PlateSolveResult {
            success: false,
            error: None,
            ra_center: 0.0,
            dec_center: 0.0,
            scale_arcsec_per_pixel: 0.0,
            rotation_deg: 0.0,
            wcs: None,
        }
    }
}

impl PlateSolveResult {
    /// Create a failed result with error message
    pub fn failed(error: &str) -> Self {
        PlateSolveResult {
            success: false,
            error: Some(error.to_string()),
            ..Default::default()
        }
    }
}

// =============================================================================
// Plate Solving Functions
// =============================================================================

/// Solve an image using astrometry.net (local installation)
///
/// Requires astrometry.net to be installed locally with index files.
pub async fn solve_with_astrometry_net(
    img_path: &Path,
    config: &PlateSolveConfig,
) -> StackingResult<PlateSolveResult> {
    // Find solve-field binary
    let solver_path = config.solver_path.as_deref()
        .or_else(|| find_solve_field())
        .ok_or_else(|| StackingError::NotInitialized)?;

    // Build command
    let mut cmd = Command::new(solver_path);

    // Basic options
    cmd.arg("--overwrite")
       .arg("--no-plots")
       .arg("--no-verify");

    // Scale hints
    let scale = config.expected_scale();
    if scale > 0.0 {
        let scale_low = scale * 0.8;
        let scale_high = scale * 1.2;
        cmd.arg("--scale-low").arg(format!("{:.3}", scale_low))
           .arg("--scale-high").arg(format!("{:.3}", scale_high))
           .arg("--scale-units").arg("arcsecperpix");
    }

    // Position hints
    if let (Some(ra), Some(dec)) = (config.initial_ra, config.initial_dec) {
        cmd.arg("--ra").arg(format!("{:.6}", ra))
           .arg("--dec").arg(format!("{:.6}", dec))
           .arg("--radius").arg(format!("{:.1}", config.search_radius_deg));
    }

    // Downsample
    if config.downsample > 1 {
        cmd.arg("--downsample").arg(format!("{}", config.downsample));
    }

    // Timeout
    cmd.arg("--cpulimit").arg(format!("{}", config.timeout_seconds));

    // Input file
    cmd.arg(img_path);

    // Execute
    let output = cmd.output().map_err(|e| {
        StackingError::ProcessError(format!("Failed to run solve-field: {}", e))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(PlateSolveResult::failed(&format!("solve-field failed: {}", stderr)));
    }

    // Parse output (look for .wcs file)
    let wcs_path = img_path.with_extension("wcs");
    if wcs_path.exists() {
        // Try to read WCS from the solved file
        let solved_path = img_path.with_extension("new");
        if solved_path.exists() {
            return read_wcs_solution(&solved_path);
        }
    }

    // Parse stdout for solution
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_solve_field_output(&stdout)
}

/// Find solve-field binary in common locations
fn find_solve_field() -> Option<&'static str> {
    let candidates = [
        "/usr/local/bin/solve-field",
        "/usr/bin/solve-field",
        "/opt/homebrew/bin/solve-field",
        "solve-field",
    ];

    for path in &candidates {
        if std::process::Command::new(path)
            .arg("--version")
            .output()
            .is_ok()
        {
            return Some(path);
        }
    }

    None
}

/// Parse solve-field stdout output
fn parse_solve_field_output(output: &str) -> StackingResult<PlateSolveResult> {
    let mut result = PlateSolveResult::default();

    // Look for field center
    for line in output.lines() {
        if line.contains("Field center:") {
            // Format: "Field center: (RA,Dec) = (123.456, 45.678) deg."
            if let Some(coords) = extract_coordinates(line) {
                result.ra_center = coords.0;
                result.dec_center = coords.1;
            }
        }
        if line.contains("Field size:") || line.contains("pixel scale") {
            // Try to extract scale
            if let Some(scale) = extract_scale(line) {
                result.scale_arcsec_per_pixel = scale;
            }
        }
        if line.contains("Field rotation") {
            // Try to extract rotation
            if let Some(rot) = extract_rotation(line) {
                result.rotation_deg = rot;
            }
        }
    }

    // Check if we found a valid solution
    if result.ra_center != 0.0 || result.dec_center != 0.0 {
        result.success = true;
    } else {
        result.error = Some("Could not parse solution from output".to_string());
    }

    Ok(result)
}

/// Extract coordinates from a line like "(RA,Dec) = (123.456, 45.678)"
fn extract_coordinates(line: &str) -> Option<(f64, f64)> {
    // Simple regex-free parsing
    if let Some(start) = line.find("(RA,Dec) = (") {
        let coords_start = start + "(RA,Dec) = (".len();
        if let Some(end) = line[coords_start..].find(')') {
            let coords_str = &line[coords_start..coords_start + end];
            let parts: Vec<&str> = coords_str.split(',').collect();
            if parts.len() == 2 {
                let ra = parts[0].trim().parse::<f64>().ok()?;
                let dec = parts[1].trim().parse::<f64>().ok()?;
                return Some((ra, dec));
            }
        }
    }
    None
}

/// Extract scale from output
fn extract_scale(line: &str) -> Option<f64> {
    // Look for number followed by "arcsec"
    for word in line.split_whitespace() {
        if let Ok(val) = word.parse::<f64>() {
            if val > 0.1 && val < 100.0 {
                return Some(val);
            }
        }
    }
    None
}

/// Extract rotation from output
fn extract_rotation(line: &str) -> Option<f64> {
    for word in line.split_whitespace() {
        if let Ok(val) = word.parse::<f64>() {
            if val.abs() <= 360.0 {
                return Some(val);
            }
        }
    }
    None
}

/// Read WCS solution from a solved FITS file
pub fn read_wcs_solution(path: &Path) -> StackingResult<PlateSolveResult> {
    use fitsio::FitsFile;

    let path_str = path.to_str().ok_or_else(|| {
        StackingError::ReadError {
            path: path.display().to_string(),
            details: "Invalid path".to_string(),
        }
    })?;

    let mut fptr = FitsFile::open(path_str).map_err(|e| {
        StackingError::ReadError {
            path: path_str.to_string(),
            details: e.to_string(),
        }
    })?;

    let hdu = fptr.primary_hdu().map_err(|e| {
        StackingError::ReadError {
            path: path_str.to_string(),
            details: e.to_string(),
        }
    })?;

    // Read WCS keywords
    let crval1: f64 = hdu.read_key(&mut fptr, "CRVAL1").unwrap_or(0.0);
    let crval2: f64 = hdu.read_key(&mut fptr, "CRVAL2").unwrap_or(0.0);
    let crpix1: f64 = hdu.read_key(&mut fptr, "CRPIX1").unwrap_or(0.0);
    let crpix2: f64 = hdu.read_key(&mut fptr, "CRPIX2").unwrap_or(0.0);
    let cd1_1: f64 = hdu.read_key(&mut fptr, "CD1_1").unwrap_or(1.0);
    let cd1_2: f64 = hdu.read_key(&mut fptr, "CD1_2").unwrap_or(0.0);
    let cd2_1: f64 = hdu.read_key(&mut fptr, "CD2_1").unwrap_or(0.0);
    let cd2_2: f64 = hdu.read_key(&mut fptr, "CD2_2").unwrap_or(1.0);
    let naxis1: i64 = hdu.read_key(&mut fptr, "NAXIS1").unwrap_or(0);
    let naxis2: i64 = hdu.read_key(&mut fptr, "NAXIS2").unwrap_or(0);

    let ctype1: String = hdu.read_key(&mut fptr, "CTYPE1").unwrap_or_else(|_| "RA---TAN".to_string());
    let ctype2: String = hdu.read_key(&mut fptr, "CTYPE2").unwrap_or_else(|_| "DEC--TAN".to_string());

    let wcs = WcsSolution {
        crpix1,
        crpix2,
        crval1,
        crval2,
        cd1_1,
        cd1_2,
        cd2_1,
        cd2_2,
        naxis1: naxis1 as i32,
        naxis2: naxis2 as i32,
        ctype1,
        ctype2,
        sip_a: None,
        sip_b: None,
        sip_ap: None,
        sip_bp: None,
    };

    let scale = wcs.pixel_scale();
    let rotation = wcs.rotation();

    Ok(PlateSolveResult {
        success: true,
        error: None,
        ra_center: crval1,
        dec_center: crval2,
        scale_arcsec_per_pixel: scale,
        rotation_deg: rotation,
        wcs: Some(wcs),
    })
}

/// Read WCS from a FitsImage if present
pub fn read_wcs_from_fits(img: &FitsImage) -> Option<WcsSolution> {
    // WCS data would need to be stored in the FitsImage struct
    // For now, return None - WCS is typically read from file directly
    None
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plate_solve_config_default() {
        let config = PlateSolveConfig::default();
        assert!(config.pixel_size_um > 0.0);
        assert!(config.focal_length_mm > 0.0);
    }

    #[test]
    fn test_expected_scale() {
        let config = PlateSolveConfig {
            pixel_size_um: 3.76,
            focal_length_mm: 500.0,
            ..Default::default()
        };

        let scale = config.expected_scale();
        // Expected: 206.265 * 3.76 / 500 ≈ 1.55 arcsec/pixel
        assert!((scale - 1.55).abs() < 0.1);
    }

    #[test]
    fn test_extract_coordinates() {
        let line = "Field center: (RA,Dec) = (123.456, 45.678) deg.";
        let coords = extract_coordinates(line);
        assert!(coords.is_some());
        let (ra, dec) = coords.unwrap();
        assert!((ra - 123.456).abs() < 0.001);
        assert!((dec - 45.678).abs() < 0.001);
    }

    #[test]
    fn test_plate_solve_result_failed() {
        let result = PlateSolveResult::failed("Test error");
        assert!(!result.success);
        assert_eq!(result.error, Some("Test error".to_string()));
    }
}
