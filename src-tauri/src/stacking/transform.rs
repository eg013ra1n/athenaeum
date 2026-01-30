//! Image transformation
//!
//! This module provides functions for applying geometric transformations
//! to images, including translation, rotation, scaling, and affine transforms.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use crate::stacking::stars::Star;
use crate::stacking::matching::StarMatch;
use rayon::prelude::*;

/// 2x3 Affine transformation matrix
///
/// Transform: [x'] = [a b tx] [x]
///            [y']   [c d ty] [y]
///                            [1]
#[derive(Debug, Clone, Copy)]
pub struct AffineTransform {
    /// Matrix elements (row-major 2x3)
    pub matrix: [[f64; 3]; 2],
}

impl Default for AffineTransform {
    fn default() -> Self {
        // Identity transform
        AffineTransform {
            matrix: [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
        }
    }
}

impl AffineTransform {
    /// Create an identity transform
    pub fn identity() -> Self {
        Self::default()
    }

    /// Create a translation transform
    pub fn translation(tx: f64, ty: f64) -> Self {
        AffineTransform {
            matrix: [
                [1.0, 0.0, tx],
                [0.0, 1.0, ty],
            ],
        }
    }

    /// Create a rotation transform (angle in radians)
    pub fn rotation(angle: f64) -> Self {
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        AffineTransform {
            matrix: [
                [cos_a, -sin_a, 0.0],
                [sin_a, cos_a, 0.0],
            ],
        }
    }

    /// Create a scaling transform
    pub fn scale(sx: f64, sy: f64) -> Self {
        AffineTransform {
            matrix: [
                [sx, 0.0, 0.0],
                [0.0, sy, 0.0],
            ],
        }
    }

    /// Create a similarity transform (rotation + uniform scale + translation)
    pub fn similarity(angle: f64, scale: f64, tx: f64, ty: f64) -> Self {
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        AffineTransform {
            matrix: [
                [scale * cos_a, -scale * sin_a, tx],
                [scale * sin_a, scale * cos_a, ty],
            ],
        }
    }

    /// Apply transform to a point
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.matrix;
        let new_x = m[0][0] * x + m[0][1] * y + m[0][2];
        let new_y = m[1][0] * x + m[1][1] * y + m[1][2];
        (new_x, new_y)
    }

    /// Compute the inverse transform
    pub fn inverse(&self) -> Option<Self> {
        let m = &self.matrix;

        // Determinant of the 2x2 rotation/scale part
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];

        if det.abs() < 1e-10 {
            return None; // Singular matrix
        }

        let inv_det = 1.0 / det;

        // Inverse of the 2x2 part
        let a = m[1][1] * inv_det;
        let b = -m[0][1] * inv_det;
        let c = -m[1][0] * inv_det;
        let d = m[0][0] * inv_det;

        // Inverse translation
        let tx = -(a * m[0][2] + b * m[1][2]);
        let ty = -(c * m[0][2] + d * m[1][2]);

        Some(AffineTransform {
            matrix: [
                [a, b, tx],
                [c, d, ty],
            ],
        })
    }

    /// Compose two transforms: self followed by other
    pub fn compose(&self, other: &AffineTransform) -> Self {
        let a = &self.matrix;
        let b = &other.matrix;

        AffineTransform {
            matrix: [
                [
                    b[0][0] * a[0][0] + b[0][1] * a[1][0],
                    b[0][0] * a[0][1] + b[0][1] * a[1][1],
                    b[0][0] * a[0][2] + b[0][1] * a[1][2] + b[0][2],
                ],
                [
                    b[1][0] * a[0][0] + b[1][1] * a[1][0],
                    b[1][0] * a[0][1] + b[1][1] * a[1][1],
                    b[1][0] * a[0][2] + b[1][1] * a[1][2] + b[1][2],
                ],
            ],
        }
    }

    /// Get rotation angle (radians)
    pub fn rotation_angle(&self) -> f64 {
        self.matrix[1][0].atan2(self.matrix[0][0])
    }

    /// Get scale factor
    pub fn scale_factor(&self) -> f64 {
        let m = &self.matrix;
        let sx = (m[0][0] * m[0][0] + m[1][0] * m[1][0]).sqrt();
        let sy = (m[0][1] * m[0][1] + m[1][1] * m[1][1]).sqrt();
        (sx * sy).sqrt()
    }

    /// Get translation
    pub fn translation_vector(&self) -> (f64, f64) {
        (self.matrix[0][2], self.matrix[1][2])
    }
}

/// Interpolation method for image transformation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    /// Nearest neighbor (fastest)
    Nearest,
    /// Bilinear interpolation
    Bilinear,
    /// Bicubic interpolation (higher quality)
    Bicubic,
    /// Lanczos interpolation (best quality)
    Lanczos,
}

impl Default for Interpolation {
    fn default() -> Self {
        Interpolation::Bilinear
    }
}

// =============================================================================
// Transform Estimation
// =============================================================================

/// Estimate affine transform from matched star pairs
///
/// Uses least-squares fitting with at least 3 point pairs.
pub fn estimate_affine(
    matches: &[StarMatch],
    ref_stars: &[Star],
    target_stars: &[Star],
) -> StackingResult<AffineTransform> {
    if matches.len() < 3 {
        return Err(StackingError::InvalidParameter(
            "Need at least 3 matched pairs for affine transform".to_string()
        ));
    }

    // Build the linear system: Ax = b
    // For each match: [x_t, y_t, 1, 0,   0,   0] [a]   [x_r]
    //                 [0,   0,   0, x_t, y_t, 1] [b] = [y_r]
    //                                           [tx]
    //                                           [c]
    //                                           [d]
    //                                           [ty]

    let n = matches.len();
    let mut ata = [[0.0_f64; 6]; 6];
    let mut atb = [0.0_f64; 6];

    for m in matches {
        let ref_star = &ref_stars[m.ref_idx];
        let target_star = &target_stars[m.target_idx];

        let x = target_star.x;
        let y = target_star.y;
        let xr = ref_star.x;
        let yr = ref_star.y;

        // First row contribution
        let row1 = [x, y, 1.0, 0.0, 0.0, 0.0];
        // Second row contribution
        let row2 = [0.0, 0.0, 0.0, x, y, 1.0];

        // Accumulate A^T * A and A^T * b
        for i in 0..6 {
            for j in 0..6 {
                ata[i][j] += row1[i] * row1[j];
                ata[i][j] += row2[i] * row2[j];
            }
            atb[i] += row1[i] * xr;
            atb[i] += row2[i] * yr;
        }
    }

    // Solve using Gaussian elimination with partial pivoting
    let x = solve_linear_system_6x6(&mut ata, &mut atb)?;

    Ok(AffineTransform {
        matrix: [
            [x[0], x[1], x[2]],
            [x[3], x[4], x[5]],
        ],
    })
}

/// Solve 6x6 linear system using Gaussian elimination
fn solve_linear_system_6x6(
    a: &mut [[f64; 6]; 6],
    b: &mut [f64; 6],
) -> StackingResult<[f64; 6]> {
    const N: usize = 6;

    // Forward elimination with partial pivoting
    for k in 0..N {
        // Find pivot
        let mut max_val = a[k][k].abs();
        let mut max_row = k;
        for i in (k + 1)..N {
            if a[i][k].abs() > max_val {
                max_val = a[i][k].abs();
                max_row = i;
            }
        }

        if max_val < 1e-10 {
            return Err(StackingError::InvalidParameter(
                "Singular matrix in transform estimation".to_string()
            ));
        }

        // Swap rows
        if max_row != k {
            for j in 0..N {
                let tmp = a[k][j];
                a[k][j] = a[max_row][j];
                a[max_row][j] = tmp;
            }
            let tmp = b[k];
            b[k] = b[max_row];
            b[max_row] = tmp;
        }

        // Eliminate column
        for i in (k + 1)..N {
            let factor = a[i][k] / a[k][k];
            for j in k..N {
                a[i][j] -= factor * a[k][j];
            }
            b[i] -= factor * b[k];
        }
    }

    // Back substitution
    let mut x = [0.0_f64; N];
    for i in (0..N).rev() {
        x[i] = b[i];
        for j in (i + 1)..N {
            x[i] -= a[i][j] * x[j];
        }
        x[i] /= a[i][i];
    }

    Ok(x)
}

// =============================================================================
// Image Transformation
// =============================================================================

/// Apply affine transform to an image
pub fn apply_transform(
    img: &FitsImage,
    transform: &AffineTransform,
    interpolation: Interpolation,
    output_size: (u32, u32),
) -> StackingResult<FitsImage> {
    let (out_width, out_height) = output_size;
    let in_width = img.width as usize;
    let in_height = img.height as usize;
    let channels = img.nchannels;

    let mut result = FitsImage::new(out_width, out_height, channels, true)?;

    // We need the inverse transform to map output -> input
    let inv_transform = transform.inverse()
        .ok_or_else(|| StackingError::InvalidParameter(
            "Transform is not invertible".to_string()
        ))?;

    let pixels_per_channel = (out_width * out_height) as usize;

    // Process each channel
    for c in 0..channels {
        let in_channel = img.channel_data(c);
        let out_channel_start = c as usize * pixels_per_channel;

        // Process rows in parallel
        let row_data: Vec<(usize, f32)> = (0..out_height as usize).into_par_iter()
            .flat_map(|y| {
                let mut row_pixels = Vec::with_capacity(out_width as usize);
                for x in 0..out_width as usize {
                    // Map output pixel to input coordinates
                    let (in_x, in_y) = inv_transform.transform_point(x as f64, y as f64);

                    let value = match interpolation {
                        Interpolation::Nearest => {
                            interpolate_nearest(in_channel, in_width, in_height, in_x, in_y)
                        }
                        Interpolation::Bilinear => {
                            interpolate_bilinear(in_channel, in_width, in_height, in_x, in_y)
                        }
                        Interpolation::Bicubic => {
                            interpolate_bicubic(in_channel, in_width, in_height, in_x, in_y)
                        }
                        Interpolation::Lanczos => {
                            interpolate_lanczos(in_channel, in_width, in_height, in_x, in_y)
                        }
                    };

                    row_pixels.push((y * out_width as usize + x, value));
                }
                row_pixels
            })
            .collect();

        for (idx, value) in row_data {
            result.data[out_channel_start + idx] = value;
        }
    }

    // Copy metadata
    result.exposure = img.exposure;
    result.filter = img.filter.clone();
    result.object = img.object.clone();
    result.instrument = img.instrument.clone();
    result.bayer_pattern = img.bayer_pattern.clone();
    result.pixel_size_x = img.pixel_size_x;
    result.pixel_size_y = img.pixel_size_y;
    result.focal_length = img.focal_length;
    result.ccd_temp = img.ccd_temp;
    result.gain = img.gain;
    result.offset = img.offset;
    result.binning_x = img.binning_x;
    result.binning_y = img.binning_y;

    Ok(result)
}

// =============================================================================
// Interpolation Functions
// =============================================================================

/// Nearest neighbor interpolation
fn interpolate_nearest(data: &[f32], width: usize, height: usize, x: f64, y: f64) -> f32 {
    let ix = x.round() as i32;
    let iy = y.round() as i32;

    if ix < 0 || ix >= width as i32 || iy < 0 || iy >= height as i32 {
        return 0.0;
    }

    data[iy as usize * width + ix as usize]
}

/// Bilinear interpolation
fn interpolate_bilinear(data: &[f32], width: usize, height: usize, x: f64, y: f64) -> f32 {
    if x < 0.0 || x >= (width - 1) as f64 || y < 0.0 || y >= (height - 1) as f64 {
        return 0.0;
    }

    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = x0 + 1;
    let y1 = y0 + 1;

    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;

    let p00 = data[y0 * width + x0];
    let p10 = data[y0 * width + x1];
    let p01 = data[y1 * width + x0];
    let p11 = data[y1 * width + x1];

    let top = p00 * (1.0 - fx) + p10 * fx;
    let bottom = p01 * (1.0 - fx) + p11 * fx;

    top * (1.0 - fy) + bottom * fy
}

/// Bicubic interpolation
fn interpolate_bicubic(data: &[f32], width: usize, height: usize, x: f64, y: f64) -> f32 {
    if x < 1.0 || x >= (width - 2) as f64 || y < 1.0 || y >= (height - 2) as f64 {
        return interpolate_bilinear(data, width, height, x, y);
    }

    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;

    let get = |dx: i32, dy: i32| -> f32 {
        let ix = (x0 + dx) as usize;
        let iy = (y0 + dy) as usize;
        data[iy * width + ix]
    };

    // Cubic interpolation kernel
    let cubic = |t: f32| -> [f32; 4] {
        let t2 = t * t;
        let t3 = t2 * t;
        [
            -0.5 * t3 + t2 - 0.5 * t,
            1.5 * t3 - 2.5 * t2 + 1.0,
            -1.5 * t3 + 2.0 * t2 + 0.5 * t,
            0.5 * t3 - 0.5 * t2,
        ]
    };

    let wx = cubic(fx);
    let wy = cubic(fy);

    let mut sum = 0.0_f32;
    for j in 0..4 {
        for i in 0..4 {
            sum += get(i - 1, j - 1) * wx[i as usize] * wy[j as usize];
        }
    }

    sum.max(0.0)
}

/// Lanczos interpolation (3-lobe)
fn interpolate_lanczos(data: &[f32], width: usize, height: usize, x: f64, y: f64) -> f32 {
    if x < 2.0 || x >= (width - 3) as f64 || y < 2.0 || y >= (height - 3) as f64 {
        return interpolate_bilinear(data, width, height, x, y);
    }

    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;

    let get = |dx: i32, dy: i32| -> f32 {
        let ix = (x0 + dx) as usize;
        let iy = (y0 + dy) as usize;
        data[iy * width + ix]
    };

    // Lanczos kernel
    let lanczos = |t: f64| -> f64 {
        if t.abs() < 1e-10 {
            1.0
        } else if t.abs() >= 3.0 {
            0.0
        } else {
            let pi_t = std::f64::consts::PI * t;
            let pi_t_3 = pi_t / 3.0;
            (pi_t.sin() * pi_t_3.sin()) / (pi_t * pi_t_3)
        }
    };

    let mut sum = 0.0_f64;
    let mut weight_sum = 0.0_f64;

    for j in -2..=3 {
        for i in -2..=3 {
            let w = lanczos(fx - i as f64) * lanczos(fy - j as f64);
            sum += get(i, j) as f64 * w;
            weight_sum += w;
        }
    }

    if weight_sum > 1e-10 {
        (sum / weight_sum).max(0.0) as f32
    } else {
        0.0
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_transform() {
        let t = AffineTransform::identity();
        let (x, y) = t.transform_point(10.0, 20.0);
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_translation() {
        let t = AffineTransform::translation(5.0, -3.0);
        let (x, y) = t.transform_point(10.0, 20.0);
        assert!((x - 15.0).abs() < 1e-10);
        assert!((y - 17.0).abs() < 1e-10);
    }

    #[test]
    fn test_rotation() {
        let t = AffineTransform::rotation(std::f64::consts::FRAC_PI_2);
        let (x, y) = t.transform_point(1.0, 0.0);
        assert!(x.abs() < 1e-10);
        assert!((y - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_scale() {
        let t = AffineTransform::scale(2.0, 0.5);
        let (x, y) = t.transform_point(10.0, 20.0);
        assert!((x - 20.0).abs() < 1e-10);
        assert!((y - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_inverse() {
        let t = AffineTransform::similarity(0.1, 1.1, 5.0, -3.0);
        let inv = t.inverse().unwrap();

        let (x1, y1) = t.transform_point(10.0, 20.0);
        let (x2, y2) = inv.transform_point(x1, y1);

        assert!((x2 - 10.0).abs() < 1e-10);
        assert!((y2 - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_compose() {
        let t1 = AffineTransform::translation(10.0, 0.0);
        let t2 = AffineTransform::scale(2.0, 2.0);
        let t = t1.compose(&t2);

        // First translate, then scale: (0,0) -> (10,0) -> (20,0)
        let (x, y) = t.transform_point(0.0, 0.0);
        assert!((x - 20.0).abs() < 1e-10);
        assert!(y.abs() < 1e-10);
    }

    #[test]
    fn test_bilinear_interpolation() {
        // Create a simple 4x4 image with a gradient
        let data: Vec<f32> = (0..16).map(|i| i as f32).collect();

        // Value at center should be average of neighbors
        let val = interpolate_bilinear(&data, 4, 4, 1.5, 1.5);
        // Expected: average of 5,6,9,10 = 7.5
        assert!((val - 7.5).abs() < 1e-5);
    }
}
