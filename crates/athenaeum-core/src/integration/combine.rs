//! Per-pixel robust combination. Algorithms per spec §9 / research findings
//! 5 & 7: winsorized sigma clip (PixInsight master recipe) and percentile
//! clip (sky flats / small sets), plus plain mean/median.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum CombineMethod {
    Mean,
    Median,
    /// Iterative winsorized sigma clipping, then mean of survivors.
    WinsorizedSigmaClip { sigma_low: f64, sigma_high: f64 },
    /// PixInsight-style percentile clipping around the median m:
    /// reject x when (m - x)/|m| > low or (x - m)/|m| > high.
    /// Deviations are normalized by |m| (not m), so thresholds are
    /// sign-agnostic and behave the same for negative medians.
    PercentileClip { low: f64, high: f64 },
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    (v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64) as f32
}

fn median_sorted(v: &[f32]) -> f32 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn stddev(v: &[f32], m: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let var = v
        .iter()
        .map(|&x| {
            let d = x as f64 - m;
            d * d
        })
        .sum::<f64>()
        / (v.len() - 1) as f64;
    var.sqrt()
}

/// Combine one pixel column: `values` holds the same pixel from N frames
/// (already normalized/pre-calibrated by the caller). Returns (value, rejected_count).
///
/// `combine_pixel` may reorder `values` in place (sorting) — callers pass scratch copies.
pub fn combine_pixel(values: &mut [f32], method: CombineMethod) -> (f32, usize) {
    match method {
        CombineMethod::Mean => (mean(values), 0),
        CombineMethod::Median => {
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (median_sorted(values), 0)
        }
        CombineMethod::WinsorizedSigmaClip {
            sigma_low,
            sigma_high,
        } => {
            let n = values.len();
            if n < 3 {
                return (mean(values), 0);
            }
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // 1) Winsorized estimate of location/scale (Huber-style iteration):
            //    clamp the working copy at m±1.5σ, recompute, repeat to 0.5% change.
            let mut work: Vec<f64> = values.iter().map(|&x| x as f64).collect();
            let mut m = work.iter().sum::<f64>() / n as f64;
            let mut s = stddev(values, m);
            for _ in 0..10 {
                if s <= f64::EPSILON {
                    break;
                }
                let (lo, hi) = (m - 1.5 * s, m + 1.5 * s);
                for x in work.iter_mut() {
                    *x = x.clamp(lo, hi);
                }
                let new_m = work.iter().sum::<f64>() / n as f64;
                let new_s = 1.134
                    * (work
                        .iter()
                        .map(|x| (x - new_m) * (x - new_m))
                        .sum::<f64>()
                        / (n - 1) as f64)
                        .sqrt();
                let converged = (new_s - s).abs() <= 0.005 * s.abs();
                m = new_m;
                s = new_s;
                if converged {
                    break;
                }
            }
            // 2) Reject original samples outside [m - σ_low·s, m + σ_high·s], mean the rest.
            let (lo, hi) = (m - sigma_low * s, m + sigma_high * s);
            let mut sum = 0.0f64;
            let mut kept = 0usize;
            for &x in values.iter() {
                let xf = x as f64;
                if xf >= lo && xf <= hi {
                    sum += xf;
                    kept += 1;
                }
            }
            if kept == 0 {
                return (median_sorted(values), values.len());
            }
            ((sum / kept as f64) as f32, values.len() - kept)
        }
        CombineMethod::PercentileClip { low, high } => {
            let n = values.len();
            if n < 3 {
                return (mean(values), 0);
            }
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = median_sorted(values) as f64;
            if m.abs() <= f64::EPSILON {
                return (m as f32, 0);
            }
            let mut sum = 0.0f64;
            let mut kept = 0usize;
            for &x in values.iter() {
                let xf = x as f64;
                let dev = (xf - m) / m.abs();
                let reject = (dev < 0.0 && -dev > low) || (dev > 0.0 && dev > high);
                if !reject {
                    sum += xf;
                    kept += 1;
                }
            }
            if kept == 0 {
                return (m as f32, n);
            }
            ((sum / kept as f64) as f32, n - kept)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_and_median_basics() {
        let (v, r) = combine_pixel(&mut [1.0, 2.0, 3.0, 4.0], CombineMethod::Mean);
        assert_eq!((v, r), (2.5, 0));
        let (v, _) = combine_pixel(&mut [5.0, 1.0, 3.0], CombineMethod::Median);
        assert_eq!(v, 3.0);
        let (v, _) = combine_pixel(&mut [4.0, 1.0, 3.0, 2.0], CombineMethod::Median);
        assert_eq!(v, 2.5); // even N: mean of middle two
    }

    #[test]
    fn winsorized_rejects_hot_pixel() {
        // 20 well-behaved samples around 100 + one cosmic-ray 5000.
        let mut vals: Vec<f32> = (0..20).map(|i| 100.0 + (i % 5) as f32).collect();
        vals.push(5000.0);
        let (v, rejected) = combine_pixel(
            &mut vals,
            CombineMethod::WinsorizedSigmaClip {
                sigma_low: 3.0,
                sigma_high: 3.0,
            },
        );
        assert!(rejected >= 1, "outlier must be rejected");
        assert!((v - 102.0).abs() < 3.0, "combined value near the clean mean, got {v}");
    }

    #[test]
    fn winsorized_keeps_clean_data_unclipped() {
        let mut vals: Vec<f32> = (0..30).map(|i| 500.0 + (i % 7) as f32).collect();
        let clean_mean = vals.iter().sum::<f32>() / vals.len() as f32;
        let (v, rejected) = combine_pixel(
            &mut vals,
            CombineMethod::WinsorizedSigmaClip {
                sigma_low: 3.0,
                sigma_high: 3.0,
            },
        );
        assert_eq!(rejected, 0);
        assert!((v - clean_mean).abs() < 0.5);
    }

    #[test]
    fn winsorized_sums_original_not_clamped_values() {
        // 12 cluster samples spread over 100.0..100.4 + one at 106.0. The
        // winsorized iteration clamps 106 in the WORK copy (down to ~100.5),
        // but the final estimate keeps sigma_final > 0 (~0.19) thanks to the
        // cluster spread, so with sigma_high = 50 the rejection band reaches
        // ~109 and the ORIGINAL 106.0 is kept. A buggy implementation that
        // means the CLAMPED work values would return ~100.20 (< true mean);
        // the correct one returns the mean of the ORIGINAL samples (~100.62).
        //
        // NOTE: a zero-spread cluster (12 x exactly 100.0) does NOT work here:
        // the winsorized sigma collapses toward 0 each iteration, the 50-sigma
        // band shrinks to ~ +/-0.2, and the original 106 gets rejected. The
        // spread keeps sigma_final positive so `rejected == 0` holds.
        let mut vals: Vec<f32> = (0..12).map(|i| 100.0 + (i % 5) as f32 * 0.1).collect();
        vals.push(106.0);
        let expected = vals.iter().map(|&x| x as f64).sum::<f64>() / vals.len() as f64;
        let (v, rejected) = combine_pixel(
            &mut vals,
            CombineMethod::WinsorizedSigmaClip {
                sigma_low: 50.0,
                sigma_high: 50.0,
            },
        );
        assert_eq!(rejected, 0);
        assert!(
            (v as f64 - expected).abs() < 1e-3,
            "must average ORIGINAL samples: got {v}, want {expected}"
        );
    }

    #[test]
    fn percentile_clip_rejects_star_in_sky_flat() {
        // median ~ 10000; star pixel 10900 is +9% > high limit 2%.
        let mut vals = vec![10000.0, 10050.0, 9980.0, 10020.0, 10900.0];
        let (v, rejected) = combine_pixel(&mut vals, CombineMethod::PercentileClip { low: 0.2, high: 0.02 });
        assert_eq!(rejected, 1);
        assert!(v < 10100.0, "{v}");
    }

    #[test]
    fn degenerate_inputs() {
        let (v, r) = combine_pixel(
            &mut [42.0],
            CombineMethod::WinsorizedSigmaClip {
                sigma_low: 3.0,
                sigma_high: 3.0,
            },
        );
        assert_eq!((v, r), (42.0, 0));
        let (v, _) = combine_pixel(&mut [], CombineMethod::Mean);
        assert!(v == 0.0);
    }
}
