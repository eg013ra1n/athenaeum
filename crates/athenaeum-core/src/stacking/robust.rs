//! Robust Chauvenet Rejection (Maples et al. 2018, ApJS 238, 2; math
//! reference §3.4) for one-dimensional samples, the Winsorization that
//! follows it in the PSF-signal estimator (§1.1), and the error-function
//! family they need. Pure `f64` math, no I/O.

/// erf via Abramowitz & Stegun 7.1.26 (|error| ≤ 1.5e-7).
pub fn erf(x: f64) -> f64 {
    if x < 0.0 {
        -erf(-x)
    } else {
        1.0 - erfc_pos(x)
    }
}

/// Complementary error function, computed without cancellation in the tail.
pub fn erfc(x: f64) -> f64 {
    if x < 0.0 {
        2.0 - erfc_pos(-x)
    } else {
        erfc_pos(x)
    }
}

fn erfc_pos(x: f64) -> f64 {
    const P: f64 = 0.3275911;
    const A: [f64; 5] = [
        0.254829592,
        -0.284496736,
        1.421413741,
        -1.453152027,
        1.061405429,
    ];
    let t = 1.0 / (1.0 + P * x);
    let poly = t * (A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4]))));
    poly * (-x * x).exp()
}

/// Inverse error function (Giles 2010, the single-precision coefficient
/// set evaluated in f64: relative error ≈ 1e-6 on (−1, 1)).
pub fn erfinv(x: f64) -> f64 {
    if x <= -1.0 {
        return f64::NEG_INFINITY;
    }
    if x >= 1.0 {
        return f64::INFINITY;
    }
    let w = -((1.0 - x) * (1.0 + x)).ln();
    let p = if w < 5.0 {
        let w = w - 2.5;
        let mut p = 2.81022636e-08;
        p = 3.43273939e-07 + p * w;
        p = -3.5233877e-06 + p * w;
        p = -4.39150654e-06 + p * w;
        p = 0.00021858087 + p * w;
        p = -0.00125372503 + p * w;
        p = -0.00417768164 + p * w;
        p = 0.246640727 + p * w;
        1.50140941 + p * w
    } else {
        let w = w.sqrt() - 3.0;
        let mut p = -0.000200214257;
        p = 0.000100950558 + p * w;
        p = 0.00134934322 + p * w;
        p = -0.00367342844 + p * w;
        p = 0.00573950773 + p * w;
        p = -0.0076224613 + p * w;
        p = 0.00943887047 + p * w;
        p = 1.00167406 + p * w;
        2.83297682 + p * w
    };
    p * x
}

/// Upper Gaussian tail `Q(z) = ½·erfc(z/√2)`.
pub fn gauss_tail(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

/// Small-sample correction `F(N) = 1 / (1 − 2.9442·N^{−1.073})`, capped at
/// 20 where the denominator is not usefully positive (N ≤ 2).
pub fn small_sample_factor(n: usize) -> f64 {
    let d = 1.0 - 2.9442 * (n as f64).powf(-1.073);
    if d <= 0.05 {
        20.0
    } else {
        1.0 / d
    }
}

/// Linear-interpolation quantile of a sorted slice (position `p·(n − 1)`).
pub(crate) fn quantile_sorted(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    let pos = p * (n - 1) as f64;
    let i = pos.floor() as usize;
    let f = pos - i as f64;
    if i + 1 >= n {
        sorted[n - 1]
    } else {
        sorted[i] + f * (sorted[i + 1] - sorted[i])
    }
}

/// `F(N) · quantile_{0.683}(|x − μ|)` over the sorted deviations.
pub(crate) fn sample_deviation(devs_sorted: &[f64]) -> f64 {
    small_sample_factor(devs_sorted.len()) * quantile_sorted(devs_sorted, 0.683)
}

/// Regress the lowest `trunc(0.683N + 0.317)` sorted deviations against
/// the half-normal quantiles `√2·erfinv((i + 1 − 0.317)/N)` with a line
/// through the origin and return `F(N)·ŷ(1)`; below 8 regression points
/// defers to [`sample_deviation`].
pub(crate) fn line_fit_deviation(devs_sorted: &[f64]) -> f64 {
    let n = devs_sorted.len();
    let m = (0.683 * n as f64 + 0.317).trunc() as usize;
    if m < 8 {
        return sample_deviation(devs_sorted);
    }
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, &y) in devs_sorted.iter().take(m).enumerate() {
        let x = std::f64::consts::SQRT_2 * erfinv((i as f64 + 1.0 - 0.317) / n as f64);
        sxy += x * y;
        sxx += x * x;
    }
    if sxx <= 0.0 {
        return sample_deviation(devs_sorted);
    }
    small_sample_factor(n) * (sxy / sxx)
}

fn median_f64(v: &mut Vec<f64>) -> f64 {
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn stddev(v: &[f64], mu: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    (v.iter().map(|x| (x - mu) * (x - mu)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

#[derive(Debug, Clone, PartialEq)]
pub struct RcrResult {
    /// Centre from the last evaluated phase (the mean once phase 2 ran).
    pub location: f64,
    /// Dispersion from the last evaluated phase.
    pub scale: f64,
    pub kept: Vec<bool>,
    pub rejected: usize,
}

/// Bulk-mode RCR: three phases of decreasing robustness (median + line-fit
/// deviation, median + sample deviation, mean + standard deviation), each
/// iterated: while `n·Q(|extreme − μ|/σ) < limit` reject the single most
/// extreme value (ties go to the high side). `limit = 0.5` is Chauvenet's
/// criterion. Below 3 values nothing is rejected.
pub fn rcr(values: &[f64], limit: f64) -> RcrResult {
    let n = values.len();
    let mut kept = vec![true; n];
    if n < 3 {
        let mut v = values.to_vec();
        return RcrResult {
            location: median_f64(&mut v),
            scale: 0.0,
            kept,
            rejected: 0,
        };
    }
    let (mut location, mut scale) = (f64::NAN, f64::NAN);
    for phase in 0..3 {
        loop {
            let idx: Vec<usize> = (0..n).filter(|&i| kept[i]).collect();
            let m = idx.len();
            if m < 3 {
                break;
            }
            let cur: Vec<f64> = idx.iter().map(|&i| values[i]).collect();
            let (mu, sigma) = if phase < 2 {
                let mut c = cur.clone();
                let med = median_f64(&mut c);
                let mut devs: Vec<f64> = cur.iter().map(|x| (x - med).abs()).collect();
                devs.sort_by(|a, b| a.total_cmp(b));
                let s = if phase == 0 {
                    line_fit_deviation(&devs)
                } else {
                    sample_deviation(&devs)
                };
                (med, s)
            } else {
                let mu = mean(&cur);
                (mu, stddev(&cur, mu))
            };
            location = mu;
            scale = sigma;
            if !(sigma > 0.0) {
                break;
            }
            let (mut imin, mut imax) = (idx[0], idx[0]);
            for &i in &idx {
                if values[i] < values[imin] {
                    imin = i;
                }
                if values[i] > values[imax] {
                    imax = i;
                }
            }
            let d_lo = m as f64 * gauss_tail((mu - values[imin]) / sigma);
            let d_hi = m as f64 * gauss_tail((values[imax] - mu) / sigma);
            if d_lo.min(d_hi) < limit {
                if d_hi <= d_lo {
                    kept[imax] = false;
                } else {
                    kept[imin] = false;
                }
            } else {
                break;
            }
        }
    }
    let rejected = kept.iter().filter(|k| !**k).count();
    RcrResult {
        location,
        scale,
        kept,
        rejected,
    }
}

/// Replace every rejected value by the nearest survivor extreme: below the
/// survivors' minimum → that minimum, above their maximum → that maximum.
/// Order is preserved; nothing is dropped. With no survivors the input is
/// returned unchanged.
pub fn winsorize(values: &[f64], kept: &[bool]) -> Vec<f64> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (v, k) in values.iter().zip(kept) {
        if *k {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
    }
    if !lo.is_finite() {
        return values.to_vec();
    }
    values
        .iter()
        .zip(kept)
        .map(|(&v, &k)| {
            if k {
                v
            } else if v < lo {
                lo
            } else if v > hi {
                hi
            } else {
                v
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gaussian(n: usize, seed: u64) -> Vec<f64> {
        let mut v = vec![0.0f32; n];
        crate::test_support::add_noise(&mut v, 1.0, seed);
        v.iter().map(|&x| x as f64).collect()
    }

    #[test]
    fn erf_family_reference_values() {
        assert!((erf(0.5) - 0.5204998778).abs() < 2e-7);
        assert!((erf(1.0) - 0.8427007929).abs() < 2e-7);
        assert!((erf(2.0) - 0.9953222650).abs() < 2e-7);
        assert!((erfc(3.0) - 2.2090497e-5).abs() < 3e-7);
        assert_eq!(erf(0.0), 0.0);
        assert!((erf(-1.0) + erf(1.0)).abs() < 1e-12);
        assert!((erfc(-1.0) - (2.0 - erfc(1.0))).abs() < 1e-12);
        assert!((gauss_tail(1.959964) - 0.025).abs() < 1e-6);
    }

    #[test]
    fn erfinv_inverts_erf() {
        assert_eq!(erfinv(0.0), 0.0);
        assert!((erfinv(0.5) - 0.4769362762).abs() < 1e-5);
        for &x in &[
            -0.999, -0.99, -0.9, -0.5, -0.1, 0.1, 0.3, 0.7, 0.9, 0.99, 0.999,
        ] {
            assert!((erf(erfinv(x)) - x).abs() < 1e-5, "x = {x}");
        }
        assert_eq!(erfinv(1.0), f64::INFINITY);
        assert_eq!(erfinv(-1.0), f64::NEG_INFINITY);
    }

    #[test]
    fn small_sample_factor_values() {
        assert!((small_sample_factor(100) - 1.0215).abs() < 1e-3);
        assert!((small_sample_factor(1000) - 1.0018).abs() < 5e-4);
        assert!(small_sample_factor(3) > 5.0);
        assert_eq!(small_sample_factor(2), 20.0);
    }

    #[test]
    fn quantile_convention() {
        let s = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(quantile_sorted(&s, 0.0), 1.0);
        assert_eq!(quantile_sorted(&s, 1.0), 4.0);
        assert!((quantile_sorted(&s, 0.5) - 2.5).abs() < 1e-12);
        assert!((quantile_sorted(&s, 1.0 / 3.0) - 2.0).abs() < 1e-12);
        assert!(quantile_sorted(&[], 0.5).is_nan());
    }

    #[test]
    fn deviations_estimate_sigma() {
        let v = gaussian(4000, 11);
        let mut devs: Vec<f64> = v.iter().map(|x| x.abs()).collect();
        devs.sort_by(|a, b| a.total_cmp(b));
        assert!(
            (sample_deviation(&devs) - 1.0).abs() < 0.1,
            "{}",
            sample_deviation(&devs)
        );
        assert!(
            (line_fit_deviation(&devs) - 1.0).abs() < 0.1,
            "{}",
            line_fit_deviation(&devs)
        );
        // fewer than 8 regression points → the sample deviation
        let short: Vec<f64> = devs[..10].to_vec();
        assert_eq!(line_fit_deviation(&short), sample_deviation(&short));
    }

    #[test]
    fn rcr_keeps_a_clean_gaussian_sample() {
        let v = gaussian(500, 12);
        let r = rcr(&v, 0.5);
        assert!(r.rejected <= 25, "{}", r.rejected);
        assert!(r.location.abs() < 0.15, "{}", r.location);
        assert!((r.scale - 1.0).abs() < 0.15, "{}", r.scale);
        assert_eq!(r.kept.len(), 500);
    }

    #[test]
    fn rcr_rejects_gross_outliers_and_keeps_the_bulk() {
        let mut v = gaussian(200, 13);
        v.extend(std::iter::repeat(10.0).take(20));
        v.extend(std::iter::repeat(-8.0).take(5));
        let r = rcr(&v, 0.5);
        for i in 200..225 {
            assert!(!r.kept[i], "outlier {i} survived");
        }
        let bulk_rejected = r.kept[..200].iter().filter(|k| !**k).count();
        assert!(bulk_rejected <= 10, "{bulk_rejected}");
        assert_eq!(r.rejected, 25 + bulk_rejected);
        assert!(r.location.abs() < 0.2, "{}", r.location);
        assert!((r.scale - 1.0).abs() < 0.2, "{}", r.scale);
    }

    #[test]
    fn rcr_leaves_tiny_samples_alone() {
        let r = rcr(&[1.0, 2.0], 0.5);
        assert_eq!(r.rejected, 0);
        assert_eq!(r.kept, vec![true, true]);
        assert_eq!(r.location, 1.5);
        assert_eq!(rcr(&[], 0.5).rejected, 0);
        // identical values: σ = 0, nothing rejected
        let r = rcr(&[3.0; 12], 0.5);
        assert_eq!(r.rejected, 0);
    }

    #[test]
    fn winsorize_replaces_rejects_with_the_nearest_survivor_extreme() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0, 100.0, -50.0];
        let kept = [true, true, true, true, true, false, false];
        assert_eq!(
            winsorize(&v, &kept),
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 5.0, 1.0]
        );
        assert_eq!(winsorize(&v, &[false; 7]), v.to_vec());
    }
}
