//! Image arithmetic operations
//!
//! This module provides pixel-level operations for image processing.
//! All operations work with FitsImage structures using f32 pixel data.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::fits::FitsImage;
use rayon::prelude::*;

/// Add two images: result = a + b
///
/// Images must have the same dimensions.
pub fn image_add(a: &FitsImage, b: &FitsImage) -> StackingResult<FitsImage> {
    check_dimensions(a, b)?;

    let mut result = a.clone();
    result.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            *a_val += b_val;
        });

    Ok(result)
}

/// Subtract two images: result = a - b
///
/// Images must have the same dimensions.
pub fn image_subtract(a: &FitsImage, b: &FitsImage) -> StackingResult<FitsImage> {
    check_dimensions(a, b)?;

    let mut result = a.clone();
    result.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            *a_val -= b_val;
        });

    Ok(result)
}

/// Multiply two images: result = a * b
///
/// Images must have the same dimensions.
pub fn image_multiply(a: &FitsImage, b: &FitsImage) -> StackingResult<FitsImage> {
    check_dimensions(a, b)?;

    let mut result = a.clone();
    result.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            *a_val *= b_val;
        });

    Ok(result)
}

/// Divide two images: result = a / b * norm
///
/// Division by zero is handled by clamping to zero.
/// Images must have the same dimensions.
///
/// # Arguments
/// * `a` - Numerator image
/// * `b` - Denominator image
/// * `norm` - Normalization factor (typically the mean of b)
pub fn image_divide(a: &FitsImage, b: &FitsImage, norm: f64) -> StackingResult<FitsImage> {
    check_dimensions(a, b)?;

    let norm_f32 = norm as f32;
    let mut result = a.clone();

    result.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            if *b_val > 1e-10 {
                *a_val = (*a_val / *b_val) * norm_f32;
            } else {
                *a_val = 0.0;
            }
        });

    Ok(result)
}

/// Scale an image by a constant factor: img = img * factor
///
/// Modifies the image in place.
pub fn image_scale(img: &mut FitsImage, factor: f64) {
    let factor_f32 = factor as f32;
    img.data.par_iter_mut().for_each(|v| {
        *v *= factor_f32;
    });
}

/// Add a constant offset to an image: img = img + offset
///
/// Modifies the image in place.
pub fn image_add_scalar(img: &mut FitsImage, offset: f64) {
    let offset_f32 = offset as f32;
    img.data.par_iter_mut().for_each(|v| {
        *v += offset_f32;
    });
}

/// Clip pixel values to a range
///
/// Modifies the image in place.
pub fn image_clip(img: &mut FitsImage, min: f32, max: f32) {
    img.data.par_iter_mut().for_each(|v| {
        *v = v.clamp(min, max);
    });
}

/// Normalize image to 0-1 range based on current min/max
pub fn image_normalize(img: &mut FitsImage) {
    let (min_val, max_val) = img.data.par_iter()
        .fold(|| (f32::MAX, f32::MIN), |(min, max), &v| {
            (min.min(v), max.max(v))
        })
        .reduce(|| (f32::MAX, f32::MIN), |(min1, max1), (min2, max2)| {
            (min1.min(min2), max1.max(max2))
        });

    let range = max_val - min_val;
    if range > 1e-10 {
        img.data.par_iter_mut().for_each(|v| {
            *v = (*v - min_val) / range;
        });
    }
}

/// Invert an image: img = 1.0 - img (assumes 0-1 range)
pub fn image_invert(img: &mut FitsImage) {
    img.data.par_iter_mut().for_each(|v| {
        *v = 1.0 - *v;
    });
}

/// Apply gamma correction: img = img^gamma
pub fn image_gamma(img: &mut FitsImage, gamma: f64) {
    let gamma_f32 = gamma as f32;
    img.data.par_iter_mut().for_each(|v| {
        if *v > 0.0 {
            *v = v.powf(gamma_f32);
        }
    });
}

/// Check that two images have the same dimensions
fn check_dimensions(a: &FitsImage, b: &FitsImage) -> StackingResult<()> {
    if a.width != b.width || a.height != b.height || a.nchannels != b.nchannels {
        return Err(StackingError::DimensionMismatch {
            expected: format!("{}x{}x{}", a.width, a.height, a.nchannels),
            actual: format!("{}x{}x{}", b.width, b.height, b.nchannels),
        });
    }
    Ok(())
}

/// Add image b to image a in place: a += b
pub fn image_add_inplace(a: &mut FitsImage, b: &FitsImage) -> StackingResult<()> {
    check_dimensions(a, b)?;

    a.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            *a_val += b_val;
        });

    Ok(())
}

/// Subtract image b from image a in place: a -= b
pub fn image_subtract_inplace(a: &mut FitsImage, b: &FitsImage) -> StackingResult<()> {
    check_dimensions(a, b)?;

    a.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            *a_val -= b_val;
        });

    Ok(())
}

/// Divide image a by image b in place with normalization: a = (a / b) * norm
pub fn image_divide_inplace(a: &mut FitsImage, b: &FitsImage, norm: f64) -> StackingResult<()> {
    check_dimensions(a, b)?;

    let norm_f32 = norm as f32;
    a.data.par_iter_mut()
        .zip(b.data.par_iter())
        .for_each(|(a_val, b_val)| {
            if *b_val > 1e-10 {
                *a_val = (*a_val / *b_val) * norm_f32;
            } else {
                *a_val = 0.0;
            }
        });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_add() {
        let mut a = FitsImage::new(10, 10, 1, true).unwrap();
        let mut b = FitsImage::new(10, 10, 1, true).unwrap();

        a.data.iter_mut().for_each(|v| *v = 0.5);
        b.data.iter_mut().for_each(|v| *v = 0.3);

        let result = image_add(&a, &b).unwrap();
        assert!((result.data[0] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_image_subtract() {
        let mut a = FitsImage::new(10, 10, 1, true).unwrap();
        let mut b = FitsImage::new(10, 10, 1, true).unwrap();

        a.data.iter_mut().for_each(|v| *v = 0.5);
        b.data.iter_mut().for_each(|v| *v = 0.3);

        let result = image_subtract(&a, &b).unwrap();
        assert!((result.data[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn test_image_scale() {
        let mut img = FitsImage::new(10, 10, 1, true).unwrap();
        img.data.iter_mut().for_each(|v| *v = 0.5);

        image_scale(&mut img, 2.0);
        assert!((img.data[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_image_clip() {
        let mut img = FitsImage::new(10, 10, 1, true).unwrap();
        img.data[0] = -0.5;
        img.data[1] = 0.5;
        img.data[2] = 1.5;

        image_clip(&mut img, 0.0, 1.0);
        assert_eq!(img.data[0], 0.0);
        assert_eq!(img.data[1], 0.5);
        assert_eq!(img.data[2], 1.0);
    }

    #[test]
    fn test_dimension_mismatch() {
        let a = FitsImage::new(10, 10, 1, true).unwrap();
        let b = FitsImage::new(20, 20, 1, true).unwrap();

        assert!(image_add(&a, &b).is_err());
    }
}
