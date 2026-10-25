//! Trail R²: the Rayleigh statistic of the measured stars' canonical angles.
//!
//! Stored for wire / sync compatibility (PC-6); nothing reads it since C7 removed the plate-solve gate pair (PC-12).
//! The legacy flag rule and its suppressions are not ported.

/// Fewer measured angles than this read 0.0.
const MIN_ANGLES: usize = 20;
/// A field FWHM below this (px) reads 0.0 (pixel-grid quantization).
const MIN_FIELD_FWHM: f64 = 2.0;

/// Rayleigh R² of the doubled angles, `|Σ e^{2iθ}|² / n²`.
///
/// Returns 0.0 with fewer than 20 angles or a field FWHM under 2 px.
pub fn trail_r_squared(thetas: &[f64], field_fwhm: f64) -> f64 {
    let n = thetas.len();
    if n < MIN_ANGLES || !(field_fwhm >= MIN_FIELD_FWHM) {
        return 0.0;
    }
    let (c, s) = thetas.iter().fold((0.0f64, 0.0f64), |(c, s), &t| {
        let a = 2.0 * t;
        (c + a.cos(), s + a.sin())
    });
    (c * c + s * s) / (n as f64 * n as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_aligned_angles_read_near_one() {
        let t: Vec<f64> = (0..20)
            .map(|i| 0.3 + 0.02 * if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        assert!(trail_r_squared(&t, 3.0) > 0.95);
    }

    #[test]
    fn nineteen_angles_read_zero() {
        assert_eq!(trail_r_squared(&[0.3; 19], 3.0), 0.0);
    }

    #[test]
    fn a_field_fwhm_below_two_reads_zero() {
        assert_eq!(trail_r_squared(&[0.3; 30], 1.9), 0.0);
    }

    #[test]
    fn uniform_angles_read_near_zero() {
        let t: Vec<f64> = (0..200)
            .map(|i| -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / 200.0)
            .collect();
        assert!(trail_r_squared(&t, 3.0) < 0.01);
    }

    #[test]
    fn mirrored_angles_read_the_same() {
        let t: Vec<f64> = (0..40).map(|i| 0.1 + 0.037 * i as f64).collect();
        let m: Vec<f64> = t.iter().map(|x| -x).collect();
        assert!((trail_r_squared(&t, 3.0) - trail_r_squared(&m, 3.0)).abs() < 1e-12);
    }
}
