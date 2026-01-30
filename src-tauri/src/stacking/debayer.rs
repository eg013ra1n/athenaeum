//! Debayering (Demosaicing)
//!
//! This module provides pure Rust implementations for converting Bayer-pattern
//! CFA (Color Filter Array) images to RGB color images.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use crate::stacking::ffi::SensorPattern;
use rayon::prelude::*;

/// Bayer pattern types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BayerPattern {
    /// Red-Green-Green-Blue (most common)
    RGGB,
    /// Blue-Green-Green-Red
    BGGR,
    /// Green-Blue-Red-Green
    GBRG,
    /// Green-Red-Blue-Green
    GRBG,
}

impl From<SensorPattern> for Option<BayerPattern> {
    fn from(p: SensorPattern) -> Self {
        match p {
            SensorPattern::BayerRggb => Some(BayerPattern::RGGB),
            SensorPattern::BayerBggr => Some(BayerPattern::BGGR),
            SensorPattern::BayerGbrg => Some(BayerPattern::GBRG),
            SensorPattern::BayerGrbg => Some(BayerPattern::GRBG),
            _ => None,
        }
    }
}

impl From<BayerPattern> for SensorPattern {
    fn from(p: BayerPattern) -> Self {
        match p {
            BayerPattern::RGGB => SensorPattern::BayerRggb,
            BayerPattern::BGGR => SensorPattern::BayerBggr,
            BayerPattern::GBRG => SensorPattern::BayerGbrg,
            BayerPattern::GRBG => SensorPattern::BayerGrbg,
        }
    }
}

/// Debayer interpolation method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebayerMethod {
    /// Bilinear interpolation (fast, lower quality)
    Bilinear,
    /// Variable Number of Gradients (good quality, moderate speed)
    VNG,
    /// Adaptive Homogeneity-Directed (high quality, slower)
    AHD,
}

impl Default for DebayerMethod {
    fn default() -> Self {
        DebayerMethod::VNG
    }
}

// =============================================================================
// Pattern Detection
// =============================================================================

/// Detect Bayer pattern from FITS header
pub fn detect_bayer_pattern(img: &FitsImage) -> Option<BayerPattern> {
    img.bayer_pattern.as_ref().and_then(|s| {
        match s.to_uppercase().as_str() {
            "RGGB" => Some(BayerPattern::RGGB),
            "BGGR" => Some(BayerPattern::BGGR),
            "GBRG" => Some(BayerPattern::GBRG),
            "GRBG" => Some(BayerPattern::GRBG),
            _ => None,
        }
    })
}

/// Check if an image is a Bayer-pattern CFA image
pub fn is_bayer(img: &FitsImage) -> bool {
    img.is_cfa() && detect_bayer_pattern(img).is_some()
}

// =============================================================================
// Debayering Algorithms
// =============================================================================

/// Debayer using bilinear interpolation
///
/// Fast but produces color artifacts at edges.
pub fn debayer_bilinear(img: &FitsImage, pattern: BayerPattern) -> StackingResult<FitsImage> {
    if img.nchannels != 1 {
        return Err(StackingError::InvalidParameter(
            "Debayering requires a single-channel CFA image".to_string()
        ));
    }

    let width = img.width as usize;
    let height = img.height as usize;

    // Create RGB output image
    let mut result = FitsImage::new(img.width, img.height, 3, true)?;

    // Copy metadata
    result.exposure = img.exposure;
    result.filter = img.filter.clone();
    result.object = img.object.clone();
    result.instrument = img.instrument.clone();
    result.pixel_size_x = img.pixel_size_x;
    result.pixel_size_y = img.pixel_size_y;
    result.focal_length = img.focal_length;
    result.ccd_temp = img.ccd_temp;
    result.gain = img.gain;
    result.offset = img.offset;
    result.binning_x = img.binning_x;
    result.binning_y = img.binning_y;
    // Don't copy bayer_pattern - output is RGB

    let input = &img.data;
    let pixels_per_channel = width * height;

    // Determine color positions based on pattern
    // For a 2x2 Bayer block starting at (0,0):
    // Position indices: (0,0), (1,0), (0,1), (1,1)
    let (r_pos, g_pos1, g_pos2, b_pos) = match pattern {
        BayerPattern::RGGB => ((0, 0), (1, 0), (0, 1), (1, 1)),
        BayerPattern::BGGR => ((1, 1), (0, 1), (1, 0), (0, 0)),
        BayerPattern::GBRG => ((1, 0), (0, 0), (1, 1), (0, 1)),
        BayerPattern::GRBG => ((0, 1), (0, 0), (1, 1), (1, 0)),
    };

    // Helper to safely get pixel
    let get_pixel = |x: i32, y: i32| -> f32 {
        if x >= 0 && x < width as i32 && y >= 0 && y < height as i32 {
            input[y as usize * width + x as usize]
        } else {
            0.0
        }
    };

    // Process each pixel
    // We'll process rows in parallel
    let row_data: Vec<(usize, [f32; 3])> = (0..height).into_par_iter()
        .flat_map(|y| {
            let mut row_pixels = Vec::with_capacity(width);
            for x in 0..width {
                let xi = x as i32;
                let yi = y as i32;

                // Determine what color this pixel is in the Bayer pattern
                let is_red_col = (x % 2) == r_pos.0;
                let is_red_row = (y % 2) == r_pos.1;
                let is_blue_col = (x % 2) == b_pos.0;
                let is_blue_row = (y % 2) == b_pos.1;

                let (r, g, b) = if is_red_col && is_red_row {
                    // Red pixel: red is known, interpolate green and blue
                    let red = input[y * width + x];
                    let green = (get_pixel(xi - 1, yi) + get_pixel(xi + 1, yi) +
                                 get_pixel(xi, yi - 1) + get_pixel(xi, yi + 1)) / 4.0;
                    let blue = (get_pixel(xi - 1, yi - 1) + get_pixel(xi + 1, yi - 1) +
                                get_pixel(xi - 1, yi + 1) + get_pixel(xi + 1, yi + 1)) / 4.0;
                    (red, green, blue)
                } else if is_blue_col && is_blue_row {
                    // Blue pixel: blue is known, interpolate green and red
                    let blue = input[y * width + x];
                    let green = (get_pixel(xi - 1, yi) + get_pixel(xi + 1, yi) +
                                 get_pixel(xi, yi - 1) + get_pixel(xi, yi + 1)) / 4.0;
                    let red = (get_pixel(xi - 1, yi - 1) + get_pixel(xi + 1, yi - 1) +
                               get_pixel(xi - 1, yi + 1) + get_pixel(xi + 1, yi + 1)) / 4.0;
                    (red, green, blue)
                } else {
                    // Green pixel: green is known, interpolate red and blue
                    let green = input[y * width + x];

                    // Determine if red is horizontal or vertical neighbor
                    let red_horizontal = (is_red_col != is_red_row) == (x % 2 == 0);

                    let (red, blue) = if red_horizontal {
                        // Red neighbors are horizontal
                        let red = (get_pixel(xi - 1, yi) + get_pixel(xi + 1, yi)) / 2.0;
                        let blue = (get_pixel(xi, yi - 1) + get_pixel(xi, yi + 1)) / 2.0;
                        (red, blue)
                    } else {
                        // Red neighbors are vertical
                        let red = (get_pixel(xi, yi - 1) + get_pixel(xi, yi + 1)) / 2.0;
                        let blue = (get_pixel(xi - 1, yi) + get_pixel(xi + 1, yi)) / 2.0;
                        (red, blue)
                    };

                    (red, green, blue)
                };

                row_pixels.push((y * width + x, [r, g, b]));
            }
            row_pixels
        })
        .collect();

    // Write results to output channels
    for (idx, [r, g, b]) in row_data {
        result.data[idx] = r;                          // R channel
        result.data[pixels_per_channel + idx] = g;     // G channel
        result.data[2 * pixels_per_channel + idx] = b; // B channel
    }

    Ok(result)
}

/// Debayer using Variable Number of Gradients (VNG)
///
/// Higher quality than bilinear, reduces color fringing.
pub fn debayer_vng(img: &FitsImage, pattern: BayerPattern) -> StackingResult<FitsImage> {
    if img.nchannels != 1 {
        return Err(StackingError::InvalidParameter(
            "Debayering requires a single-channel CFA image".to_string()
        ));
    }

    let width = img.width as usize;
    let height = img.height as usize;

    // Create RGB output image
    let mut result = FitsImage::new(img.width, img.height, 3, true)?;

    // Copy metadata
    result.exposure = img.exposure;
    result.filter = img.filter.clone();
    result.object = img.object.clone();
    result.instrument = img.instrument.clone();
    result.pixel_size_x = img.pixel_size_x;
    result.pixel_size_y = img.pixel_size_y;
    result.focal_length = img.focal_length;
    result.ccd_temp = img.ccd_temp;
    result.gain = img.gain;
    result.offset = img.offset;
    result.binning_x = img.binning_x;
    result.binning_y = img.binning_y;

    let input = &img.data;
    let pixels_per_channel = width * height;

    // VNG uses 8 gradient directions
    // This is a simplified VNG implementation

    // Helper to safely get pixel
    let get_pixel = |x: i32, y: i32| -> f32 {
        if x >= 0 && x < width as i32 && y >= 0 && y < height as i32 {
            input[y as usize * width + x as usize]
        } else {
            // Mirror edge pixels
            let cx = x.clamp(0, width as i32 - 1) as usize;
            let cy = y.clamp(0, height as i32 - 1) as usize;
            input[cy * width + cx]
        }
    };

    // Determine color positions based on pattern
    let (r_pos, _, _, b_pos) = match pattern {
        BayerPattern::RGGB => ((0, 0), (1, 0), (0, 1), (1, 1)),
        BayerPattern::BGGR => ((1, 1), (0, 1), (1, 0), (0, 0)),
        BayerPattern::GBRG => ((1, 0), (0, 0), (1, 1), (0, 1)),
        BayerPattern::GRBG => ((0, 1), (0, 0), (1, 1), (1, 0)),
    };

    // Process each pixel using VNG-style gradient analysis
    let row_data: Vec<(usize, [f32; 3])> = (0..height).into_par_iter()
        .flat_map(|y| {
            let mut row_pixels = Vec::with_capacity(width);
            for x in 0..width {
                let xi = x as i32;
                let yi = y as i32;

                let is_red_col = (x % 2) == r_pos.0;
                let is_red_row = (y % 2) == r_pos.1;
                let is_blue_col = (x % 2) == b_pos.0;
                let is_blue_row = (y % 2) == b_pos.1;

                let pixel = input[y * width + x];

                // Compute gradients in 8 directions
                let grad_n = (get_pixel(xi, yi - 2) - pixel).abs() +
                             (get_pixel(xi, yi - 1) - get_pixel(xi, yi + 1)).abs();
                let grad_s = (get_pixel(xi, yi + 2) - pixel).abs() +
                             (get_pixel(xi, yi + 1) - get_pixel(xi, yi - 1)).abs();
                let grad_e = (get_pixel(xi + 2, yi) - pixel).abs() +
                             (get_pixel(xi + 1, yi) - get_pixel(xi - 1, yi)).abs();
                let grad_w = (get_pixel(xi - 2, yi) - pixel).abs() +
                             (get_pixel(xi - 1, yi) - get_pixel(xi + 1, yi)).abs();

                // Find minimum gradient
                let min_grad = grad_n.min(grad_s).min(grad_e).min(grad_w);
                let threshold = min_grad * 1.5;

                // Count valid directions and accumulate
                let mut sum = 0.0;
                let mut count = 0.0;

                if grad_n <= threshold { sum += get_pixel(xi, yi - 1); count += 1.0; }
                if grad_s <= threshold { sum += get_pixel(xi, yi + 1); count += 1.0; }
                if grad_e <= threshold { sum += get_pixel(xi + 1, yi); count += 1.0; }
                if grad_w <= threshold { sum += get_pixel(xi - 1, yi); count += 1.0; }

                let edge_interp = if count > 0.0 { sum / count } else { pixel };

                let (r, g, b) = if is_red_col && is_red_row {
                    // Red pixel
                    let green = edge_interp;
                    let blue = (get_pixel(xi - 1, yi - 1) + get_pixel(xi + 1, yi - 1) +
                                get_pixel(xi - 1, yi + 1) + get_pixel(xi + 1, yi + 1)) / 4.0;
                    (pixel, green, blue)
                } else if is_blue_col && is_blue_row {
                    // Blue pixel
                    let green = edge_interp;
                    let red = (get_pixel(xi - 1, yi - 1) + get_pixel(xi + 1, yi - 1) +
                               get_pixel(xi - 1, yi + 1) + get_pixel(xi + 1, yi + 1)) / 4.0;
                    (red, green, pixel)
                } else {
                    // Green pixel
                    let red_horizontal = is_red_col != is_red_row;
                    let (red, blue) = if red_horizontal {
                        ((get_pixel(xi - 1, yi) + get_pixel(xi + 1, yi)) / 2.0,
                         (get_pixel(xi, yi - 1) + get_pixel(xi, yi + 1)) / 2.0)
                    } else {
                        ((get_pixel(xi, yi - 1) + get_pixel(xi, yi + 1)) / 2.0,
                         (get_pixel(xi - 1, yi) + get_pixel(xi + 1, yi)) / 2.0)
                    };
                    (red, pixel, blue)
                };

                row_pixels.push((y * width + x, [r, g, b]));
            }
            row_pixels
        })
        .collect();

    // Write results
    for (idx, [r, g, b]) in row_data {
        result.data[idx] = r;
        result.data[pixels_per_channel + idx] = g;
        result.data[2 * pixels_per_channel + idx] = b;
    }

    Ok(result)
}

// =============================================================================
// High-Level API
// =============================================================================

/// Debayer a CFA image to RGB
///
/// # Arguments
///
/// * `img` - Input CFA image
/// * `method` - Interpolation method
/// * `pattern` - Bayer pattern (auto-detected if None)
pub fn debayer(
    img: &FitsImage,
    method: DebayerMethod,
    pattern: Option<BayerPattern>,
) -> StackingResult<FitsImage> {
    // Auto-detect pattern if not specified
    let pattern = pattern.or_else(|| detect_bayer_pattern(img))
        .ok_or_else(|| StackingError::InvalidParameter(
            "Could not detect Bayer pattern - please specify manually".to_string()
        ))?;

    match method {
        DebayerMethod::Bilinear => debayer_bilinear(img, pattern),
        DebayerMethod::VNG => debayer_vng(img, pattern),
        DebayerMethod::AHD => {
            // AHD is complex - fallback to VNG for now
            // TODO: Implement full AHD algorithm
            debayer_vng(img, pattern)
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_bayer_image() -> FitsImage {
        // Create a simple 4x4 Bayer pattern image
        let mut img = FitsImage::new(4, 4, 1, true).unwrap();
        img.bayer_pattern = Some("RGGB".to_string());

        // Set up a simple test pattern
        // R G R G
        // G B G B
        // R G R G
        // G B G B
        let pattern = [
            0.8, 0.5, 0.8, 0.5,  // Row 0: R G R G
            0.5, 0.2, 0.5, 0.2,  // Row 1: G B G B
            0.8, 0.5, 0.8, 0.5,  // Row 2
            0.5, 0.2, 0.5, 0.2,  // Row 3
        ];
        img.data.copy_from_slice(&pattern);

        img
    }

    #[test]
    fn test_detect_pattern() {
        let img = create_bayer_image();
        assert_eq!(detect_bayer_pattern(&img), Some(BayerPattern::RGGB));
    }

    #[test]
    fn test_is_bayer() {
        let img = create_bayer_image();
        assert!(is_bayer(&img));

        let rgb_img = FitsImage::new(4, 4, 3, true).unwrap();
        assert!(!is_bayer(&rgb_img));
    }

    #[test]
    fn test_debayer_bilinear() {
        let img = create_bayer_image();
        let result = debayer_bilinear(&img, BayerPattern::RGGB).unwrap();

        assert_eq!(result.nchannels, 3);
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
        assert!(result.bayer_pattern.is_none());
    }

    #[test]
    fn test_debayer_vng() {
        let img = create_bayer_image();
        let result = debayer_vng(&img, BayerPattern::RGGB).unwrap();

        assert_eq!(result.nchannels, 3);
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn test_debayer_auto() {
        let img = create_bayer_image();
        let result = debayer(&img, DebayerMethod::Bilinear, None).unwrap();

        assert_eq!(result.nchannels, 3);
    }

    #[test]
    fn test_debayer_rgb_error() {
        let img = FitsImage::new(4, 4, 3, true).unwrap();
        assert!(debayer_bilinear(&img, BayerPattern::RGGB).is_err());
    }
}
