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

/// Fill `out` with the sorted-ascending `|values[i] − median|` for every `i`
/// in `order` (already sorted ascending by `values[i]`) and return the
/// median. Because `order` is value-sorted, deviations grow monotonically
/// walking outward from the median position on each side, so a two-pointer
/// merge of the two sides — no re-sort — produces the sorted deviations in
/// `O(order.len())`.
fn sorted_deviations_from_median(values: &[f64], order: &[usize], out: &mut Vec<f64>) -> f64 {
    out.clear();
    let m = order.len();
    let med;
    let (mut l, mut r): (isize, isize);
    if m % 2 == 1 {
        let mid = m / 2;
        med = values[order[mid]];
        out.push(0.0);
        l = mid as isize - 1;
        r = mid as isize + 1;
    } else {
        let (lo, hi) = (m / 2 - 1, m / 2);
        med = 0.5 * (values[order[lo]] + values[order[hi]]);
        let d = (values[order[hi]] - med).abs();
        out.push(d);
        out.push(d);
        l = lo as isize - 1;
        r = hi as isize + 1;
    }
    loop {
        let dl = if l >= 0 {
            Some((med - values[order[l as usize]]).abs())
        } else {
            None
        };
        let dr = if (r as usize) < m {
            Some((values[order[r as usize]] - med).abs())
        } else {
            None
        };
        match (dl, dr) {
            (Some(a), Some(b)) => {
                if a <= b {
                    out.push(a);
                    l -= 1;
                } else {
                    out.push(b);
                    r += 1;
                }
            }
            (Some(a), None) => {
                out.push(a);
                l -= 1;
            }
            (None, Some(b)) => {
                out.push(b);
                r += 1;
            }
            (None, None) => break,
        }
    }
    med
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
    let mut devs: Vec<f64> = Vec::new();
    for phase in 0..3 {
        let mut order: Vec<usize> = (0..n).filter(|&i| kept[i]).collect();
        order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
        loop {
            let m = order.len();
            if m < 3 {
                break;
            }
            let imin = order[0];
            let imax = order[m - 1];
            let (mu, sigma) = if phase < 2 {
                let med = sorted_deviations_from_median(values, &order, &mut devs);
                let s = if phase == 0 {
                    line_fit_deviation(&devs)
                } else {
                    sample_deviation(&devs)
                };
                (med, s)
            } else {
                let mut sum = 0.0f64;
                for &i in &order {
                    sum += values[i];
                }
                let mu = sum / m as f64;
                let mut ss = 0.0f64;
                for &i in &order {
                    let d = values[i] - mu;
                    ss += d * d;
                }
                let sigma = if m > 1 {
                    (ss / (m - 1) as f64).sqrt()
                } else {
                    0.0
                };
                (mu, sigma)
            };
            location = mu;
            scale = sigma;
            if !(sigma > 0.0) {
                break;
            }
            let d_lo = m as f64 * gauss_tail((mu - values[imin]) / sigma);
            let d_hi = m as f64 * gauss_tail((values[imax] - mu) / sigma);
            if d_lo.min(d_hi) < limit {
                if d_hi <= d_lo {
                    kept[imax] = false;
                    order.pop();
                } else {
                    kept[imin] = false;
                    order.remove(0);
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
    debug_assert_eq!(
        values.len(),
        kept.len(),
        "winsorize: values and kept must align"
    );
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
        assert!(erf(0.0).abs() < 1e-8, "{}", erf(0.0));
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
    fn rcr_is_identical_on_a_large_contaminated_sample() {
        let mut v = gaussian(5000, 77);
        v.extend(std::iter::repeat(25.0).take(300));
        v.extend(std::iter::repeat(-20.0).take(200));
        let r = rcr(&v, 0.5);
        for i in 5000..5500 {
            assert!(!r.kept[i], "outlier {i} survived");
        }
        let bulk_rejected = r.kept[..5000].iter().filter(|k| !**k).count();
        assert!(
            bulk_rejected as f64 / 5000.0 <= 0.02,
            "{bulk_rejected} of 5000"
        );
        assert_eq!(r.rejected, 500 + bulk_rejected);
    }

    /// M4c Task 1: `integration::combine` carries a SECOND implementation of
    /// this module's `erfc`, `erfinv` and `rcr` — the integration tree is
    /// ungated and cannot import `stacking`, which is gated behind
    /// `render + solver`. Hold the two copies to the same answers so they
    /// cannot drift apart silently: the error functions bit-for-bit, and the
    /// RCR rejection over 50 random contaminated samples (reached through
    /// the public weighted combiner, whose survivor mask names exactly the
    /// samples the pixel-stack routine kept).
    #[test]
    fn integration_copies_agree_with_this_module() {
        use crate::integration::combine::{
            combine_pixel_weighted, mask_get, mask_words, IntegrationRecipe, Rejection,
        };
        use crate::integration::student_t;

        for i in -500..=500 {
            let x = i as f64 / 100.0;
            assert_eq!(
                erfc(x).to_bits(),
                student_t::erfc(x).to_bits(),
                "erfc({x}): {} vs {}",
                erfc(x),
                student_t::erfc(x)
            );
        }
        for i in -99..=99 {
            let x = i as f64 / 100.0;
            assert_eq!(
                erfinv(x).to_bits(),
                student_t::erfinv(x).to_bits(),
                "erfinv({x}): {} vs {}",
                erfinv(x),
                student_t::erfinv(x)
            );
        }

        let limit = 0.5;
        let mut scratch: Vec<f32> = Vec::new();
        for seed in 0..50u64 {
            // A clean Gaussian plus four distinct planted outliers. The
            // values come from an f32 buffer, so the f64 sample this module
            // takes and the f32 sample the pixel path takes are the same
            // number — any disagreement is the algorithm, not the width.
            let mut values = gaussian(60, 1000 + seed);
            for k in 0..4 {
                values.push(6.0 + 0.5 * k as f64 + 0.01 * seed as f64);
            }
            let n = values.len();
            let mine = rcr(&values, limit);

            let out: Vec<f32> = values.iter().map(|&v| v as f32).collect();
            let mut work: Vec<(f32, u16)> =
                out.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
            let weights = vec![1.0f32; n];
            let mut mask = vec![0u64; mask_words(n)];
            let (_, rejected) = combine_pixel_weighted(
                &mut work,
                &out,
                &weights,
                IntegrationRecipe::average(Rejection::Rcr { limit }),
                &mut mask,
                &mut scratch,
            );
            let theirs: Vec<bool> = (0..n).map(|i| mask_get(&mask, i)).collect();
            assert_eq!(rejected, mine.rejected, "seed {seed}: rejected count");
            assert_eq!(theirs, mine.kept, "seed {seed}: survivor set");
        }
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
