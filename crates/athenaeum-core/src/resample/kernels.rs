//! Separable 1-D interpolation kernels and the two deringing clamps.
//!
//! Weights are evaluated per axis at a fractional offset `frac ∈ [0, 1)`
//! measured from `floor(coord)`; the caller applies them to the taps starting
//! at `floor(coord) + first_tap_offset()`. `Nearest` is the exception: its
//! single tap sits at `floor(coord + 0.5)`, not at `floor(coord)` — `taps_for`
//! is the entry point every caller must use for tap placement. Every kernel's
//! weights are normalized to sum to one (a constant field resamples to
//! itself). Formulas: math reference §5.4.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Interpolation {
    Nearest,
    Bilinear,
    /// Keys cubic convolution, a = -0.5 — interpolating, mild ringing.
    BicubicSpline,
    /// Cubic B-spline — smoothing (1/6, 4/6, 1/6 at integer positions).
    BicubicBSpline,
    Lanczos3,
    Lanczos4,
    /// Mitchell–Netravali cubic filter, B = C = 1/3.
    MitchellNetravali,
}

/// One axis' taps for a sample position: `first` is the source index of
/// tap 0, `w[..n]` the weights.
#[derive(Clone, Copy, Debug)]
pub struct Taps {
    pub first: isize,
    pub n: usize,
    pub w: [f32; 8],
}

impl Interpolation {
    /// Half-width of the kernel support in source pixels.
    pub fn radius(self) -> usize {
        match self {
            Interpolation::Nearest => 0,
            Interpolation::Bilinear => 1,
            Interpolation::BicubicSpline
            | Interpolation::BicubicBSpline
            | Interpolation::MitchellNetravali => 2,
            Interpolation::Lanczos3 => 3,
            Interpolation::Lanczos4 => 4,
        }
    }

    /// Number of taps per axis.
    pub fn taps(self) -> usize {
        match self {
            Interpolation::Nearest => 1,
            Interpolation::Bilinear => 2,
            Interpolation::BicubicSpline
            | Interpolation::BicubicBSpline
            | Interpolation::MitchellNetravali => 4,
            Interpolation::Lanczos3 => 6,
            Interpolation::Lanczos4 => 8,
        }
    }

    /// Offset of tap 0 from `floor(coord)`.
    pub fn first_tap_offset(self) -> isize {
        match self {
            Interpolation::Nearest => 0,
            Interpolation::Bilinear => 0,
            Interpolation::BicubicSpline
            | Interpolation::BicubicBSpline
            | Interpolation::MitchellNetravali => -1,
            Interpolation::Lanczos3 => -2,
            Interpolation::Lanczos4 => -3,
        }
    }

    /// True when the kernel reproduces the samples at integer positions.
    pub fn is_interpolating(self) -> bool {
        !matches!(
            self,
            Interpolation::BicubicBSpline | Interpolation::MitchellNetravali
        )
    }

    /// Fills `out[..n]` with the weights for fractional offset `frac ∈ [0, 1)`
    /// and returns `n`. Weights sum to one. For `Nearest`, the returned
    /// weight belongs to the rounded tap that `taps_for` places, not to
    /// `floor(coord)`.
    pub fn weights(self, frac: f32, out: &mut [f32; 8]) -> usize {
        let n = self.taps();
        match self {
            Interpolation::Nearest => {
                out[0] = 1.0;
            }
            Interpolation::Bilinear => {
                out[0] = 1.0 - frac;
                out[1] = frac;
            }
            Interpolation::BicubicSpline => {
                for (i, o) in out[..4].iter_mut().enumerate() {
                    let d = (frac - (i as f32 - 1.0)).abs();
                    *o = keys(d, -0.5);
                }
            }
            Interpolation::BicubicBSpline => {
                for (i, o) in out[..4].iter_mut().enumerate() {
                    let d = (frac - (i as f32 - 1.0)).abs();
                    *o = cubic_bspline(d);
                }
            }
            Interpolation::MitchellNetravali => {
                for (i, o) in out[..4].iter_mut().enumerate() {
                    let d = (frac - (i as f32 - 1.0)).abs();
                    *o = mitchell(d, 1.0 / 3.0, 1.0 / 3.0);
                }
            }
            Interpolation::Lanczos3 | Interpolation::Lanczos4 => {
                let a = self.radius() as f32;
                let first = self.first_tap_offset() as f32;
                for (i, o) in out[..n].iter_mut().enumerate() {
                    let d = frac - (first + i as f32);
                    *o = lanczos(d, a);
                }
            }
        }
        let sum: f32 = out[..n].iter().sum();
        if sum.abs() > 1e-12 && (sum - 1.0).abs() > 1e-7 {
            for o in out[..n].iter_mut() {
                *o /= sum;
            }
        }
        n
    }
}

/// Keys cubic convolution kernel, parameter `a` (−0.5 = Catmull-Rom family).
fn keys(d: f32, a: f32) -> f32 {
    if d <= 1.0 {
        (a + 2.0) * d * d * d - (a + 3.0) * d * d + 1.0
    } else if d < 2.0 {
        a * d * d * d - 5.0 * a * d * d + 8.0 * a * d - 4.0 * a
    } else {
        0.0
    }
}

/// Cubic B-spline basis (smoothing).
fn cubic_bspline(d: f32) -> f32 {
    if d <= 1.0 {
        2.0 / 3.0 - d * d + d * d * d / 2.0
    } else if d < 2.0 {
        let t = 2.0 - d;
        t * t * t / 6.0
    } else {
        0.0
    }
}

/// Mitchell–Netravali family with parameters B, C.
fn mitchell(d: f32, b: f32, c: f32) -> f32 {
    let d2 = d * d;
    let d3 = d2 * d;
    let v = if d < 1.0 {
        (12.0 - 9.0 * b - 6.0 * c) * d3 + (-18.0 + 12.0 * b + 6.0 * c) * d2 + (6.0 - 2.0 * b)
    } else if d < 2.0 {
        (-b - 6.0 * c) * d3
            + (6.0 * b + 30.0 * c) * d2
            + (-12.0 * b - 48.0 * c) * d
            + (8.0 * b + 24.0 * c)
    } else {
        0.0
    };
    v / 6.0
}

/// Lanczos window: sinc(d)·sinc(d/a) for |d| < a.
fn lanczos(d: f32, a: f32) -> f32 {
    if d.abs() >= a {
        return 0.0;
    }
    if d.abs() < 1e-6 {
        return 1.0;
    }
    let x = std::f32::consts::PI * d;
    let sx = x.sin() / x;
    let xa = x / a;
    let sxa = xa.sin() / xa;
    sx * sxa
}

/// Taps for one axis at `coord` (0-based, integer = pixel centre).
pub fn taps_for(interp: Interpolation, coord: f32) -> Taps {
    let base = coord.floor();
    let frac = coord - base;
    let mut w = [0f32; 8];
    let n = match interp {
        // Nearest rounds instead of flooring, so it carries its own base.
        Interpolation::Nearest => {
            w[0] = 1.0;
            return Taps {
                first: (coord + 0.5).floor() as isize,
                n: 1,
                w,
            };
        }
        other => other.weights(frac, &mut w),
    };
    Taps {
        first: base as isize + interp.first_tap_offset(),
        n,
        w,
    }
}

/// Keys-kernel deringing along one axis (math reference §5.4): `w`/`p` are
/// the four taps in order (outer, inner, inner, outer). When the outer taps'
/// negative contribution reaches `threshold` × the inner taps' contribution
/// the cubic is replaced by the inner-tap linear estimate.
pub fn clamp_keys_1d(w: &[f32; 4], p: &[f32; 4], threshold: f32) -> f32 {
    let f12 = w[1] * p[1] + w[2] * p[2];
    let f03 = w[0] * p[0] + w[3] * p[3];
    if -f03 >= f12 * threshold {
        let inner = w[1] + w[2];
        if inner.abs() > 1e-12 {
            return f12 / inner;
        }
    }
    f12 + f03
}

/// Lanczos deringing (math reference §5.4): `pos_sum`/`pos_w` are the
/// positive-weight contributions and their weight sum; `neg_sum`/`neg_w` the
/// magnitudes of the negative ones. `r = neg/pos`; at `r ≥ 1` only the
/// positive part survives, above `threshold` the negative part is attenuated
/// by `1 − ((r − threshold)/(1 − threshold))²`.
pub fn combine_lanczos_clamped(
    pos_sum: f32,
    pos_w: f32,
    neg_sum: f32,
    neg_w: f32,
    threshold: f32,
) -> f32 {
    if pos_sum <= 0.0 || pos_w <= 0.0 {
        let denom = pos_w - neg_w;
        return if denom.abs() > 1e-12 {
            (pos_sum - neg_sum) / denom
        } else {
            0.0
        };
    }
    let r = neg_sum / pos_sum;
    if r >= 1.0 {
        return pos_sum / pos_w;
    }
    if r > threshold {
        let k = 1.0 - ((r - threshold) / (1.0 - threshold)).powi(2);
        let denom = pos_w - neg_w * k;
        return (pos_sum - neg_sum * k) / denom;
    }
    (pos_sum - neg_sum) / (pos_w - neg_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Interpolation; 7] = [
        Interpolation::Nearest,
        Interpolation::Bilinear,
        Interpolation::BicubicSpline,
        Interpolation::BicubicBSpline,
        Interpolation::Lanczos3,
        Interpolation::Lanczos4,
        Interpolation::MitchellNetravali,
    ];

    #[test]
    fn every_kernel_partitions_unity() {
        for k in ALL {
            for frac in [0.0f32, 0.25, 0.5, 0.9, 0.999] {
                let mut w = [0f32; 8];
                let n = k.weights(frac, &mut w);
                assert_eq!(n, k.taps(), "{k:?} tap count");
                let sum: f32 = w[..n].iter().sum();
                assert!((sum - 1.0).abs() < 1e-5, "{k:?} at {frac}: sum {sum}");
            }
        }
    }

    #[test]
    fn interpolating_kernels_reproduce_samples_at_integer_positions() {
        for k in [
            Interpolation::Nearest,
            Interpolation::Bilinear,
            Interpolation::BicubicSpline,
            Interpolation::Lanczos3,
            Interpolation::Lanczos4,
        ] {
            let mut w = [0f32; 8];
            let n = k.weights(0.0, &mut w);
            // The tap that sits on the sample itself carries all the weight.
            let centre = (-k.first_tap_offset()) as usize;
            assert!(
                (w[centre] - 1.0).abs() < 1e-6,
                "{k:?} centre weight {}",
                w[centre]
            );
            for (i, wi) in w[..n].iter().enumerate() {
                if i != centre {
                    assert!(wi.abs() < 1e-6, "{k:?} tap {i} = {wi}");
                }
            }
            assert!(k.is_interpolating());
        }
    }

    #[test]
    fn bspline_at_integer_is_the_one_four_one_smoother() {
        let mut w = [0f32; 8];
        let n = Interpolation::BicubicBSpline.weights(0.0, &mut w);
        assert_eq!(n, 4);
        assert!((w[0] - 1.0 / 6.0).abs() < 1e-6);
        assert!((w[1] - 4.0 / 6.0).abs() < 1e-6);
        assert!((w[2] - 1.0 / 6.0).abs() < 1e-6);
        assert!(w[3].abs() < 1e-6);
        assert!(!Interpolation::BicubicBSpline.is_interpolating());
    }

    #[test]
    fn bilinear_weights_are_exact() {
        let mut w = [0f32; 8];
        let n = Interpolation::Bilinear.weights(0.25, &mut w);
        assert_eq!(n, 2);
        assert!((w[0] - 0.75).abs() < 1e-6 && (w[1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn weights_are_mirror_symmetric() {
        for k in ALL {
            let mut a = [0f32; 8];
            let mut b = [0f32; 8];
            let n = k.weights(0.3, &mut a);
            k.weights(0.7, &mut b);
            // w(t) reversed equals w(1 - t) for every symmetric kernel.
            for i in 0..n {
                assert!((a[i] - b[n - 1 - i]).abs() < 1e-5, "{k:?} tap {i}");
            }
        }
    }

    #[test]
    fn taps_for_places_the_base_index() {
        let t = taps_for(Interpolation::BicubicSpline, 10.25);
        assert_eq!(t.first, 9); // floor(10.25) - 1
        assert_eq!(t.n, 4);
        let t = taps_for(Interpolation::Lanczos3, 10.25);
        assert_eq!(t.first, 8); // floor - 2
        assert_eq!(t.n, 6);
        let t = taps_for(Interpolation::Nearest, 10.75);
        assert_eq!(t.first, 11);
        assert_eq!(t.n, 1);
    }

    #[test]
    fn keys_clamp_removes_overshoot_on_a_step_edge() {
        // Samples around a hard edge 0,0 | 1,1 at fractional position 0.5.
        let mut w = [0f32; 8];
        Interpolation::BicubicSpline.weights(0.5, &mut w);
        let w4 = [w[0], w[1], w[2], w[3]];
        let p = [0.0f32, 0.0, 1.0, 1.0];
        let raw: f32 = w4.iter().zip(p.iter()).map(|(a, b)| a * b).sum();
        assert!((raw - 0.5).abs() < 1e-6); // symmetric case has no overshoot
                                           // Asymmetric edge: 0,0,0,1 at frac 0.75 rings below zero without a clamp.
        Interpolation::BicubicSpline.weights(0.75, &mut w);
        let w4 = [w[0], w[1], w[2], w[3]];
        let p = [0.0f32, 0.0, 0.0, 1.0];
        let raw: f32 = w4.iter().zip(p.iter()).map(|(a, b)| a * b).sum();
        assert!(raw < 0.0, "expected negative ringing, got {raw}");
        let clamped = clamp_keys_1d(&w4, &p, 0.3);
        assert!(clamped >= 0.0 && clamped <= 1.0, "clamped {clamped}");
    }

    #[test]
    fn lanczos_clamp_attenuates_negative_lobes() {
        // pos 1.2 with weight 1.1, neg 0.5 with weight 0.1: r = 0.4167 > 0.3.
        let v = combine_lanczos_clamped(1.2, 1.1, 0.5, 0.1, 0.3);
        let unclamped = (1.2 - 0.5) / (1.1 - 0.1);
        assert!(
            v > unclamped,
            "attenuation must raise the value: {v} vs {unclamped}"
        );
        // r >= 1 collapses to the positive part only.
        let v = combine_lanczos_clamped(1.0, 1.0, 1.5, 0.2, 0.3);
        assert!((v - 1.0).abs() < 1e-6);
        // r <= threshold is untouched.
        let v = combine_lanczos_clamped(1.0, 1.0, 0.1, 0.05, 0.3);
        assert!((v - (0.9 / 0.95)).abs() < 1e-6);
    }
}
