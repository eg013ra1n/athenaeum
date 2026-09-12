//! Student's t quantile, the regularized incomplete beta it is inverted
//! from, and the error-function family the rejection routines in
//! [`super::combine`] need — all dependency-free (ruling R-M4c-2).
//!
//! Why a second copy of `erfc`/`erfinv` exists: `stacking::robust` already
//! carries the same formulas, but `stacking` is gated behind
//! `render + solver` while `integration` is not, so the integration engine
//! cannot import them. This module is the ungated copy; a test in
//! `stacking::robust` cross-checks the two against each other so they can
//! never drift apart silently.
//!
//! The generalized-ESD critical value `λ_i` (math reference §3.4) is the one
//! consumer of the t quantile. Because each `λ` costs a bisection over the
//! incomplete beta, and a 26 Mpx plane hands the rejection routine tens of
//! millions of pixel stacks that all share the same frame count, the `λ`
//! vector is memoised per `(n, alpha)` in a thread-local (R-M4c-2) —
//! [`with_esd_lambdas`] is the only way the hot path reads it.

use std::cell::RefCell;
use std::collections::HashMap;

// ── Error-function family (ungated copy of `stacking::robust`'s) ────────────

/// Complementary error function, computed without cancellation in the tail
/// (Abramowitz & Stegun 7.1.26, |error| ≤ 1.5e-7).
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

/// Inverse error function (Giles 2010, the single-precision coefficient set
/// evaluated in f64: relative error ≈ 1e-6 on (−1, 1)).
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

// ── Regularized incomplete beta ────────────────────────────────────────────

/// `ln Γ(x)` for `x > 0` (Lanczos, Numerical Recipes 6.1 `gammln`; relative
/// error ≈ 1e-10). Only ever called with the half-integer shape parameters
/// the t distribution needs.
fn ln_gamma(x: f64) -> f64 {
    const COF: [f64; 6] = [
        76.180_091_729_471_46,
        -86.505_320_329_416_77,
        24.014_098_240_830_91,
        -1.231_739_572_450_155,
        0.120_865_097_386_617_9e-2,
        -0.539_523_938_495_3e-5,
    ];
    let mut tmp = x + 5.5;
    tmp -= (x + 0.5) * tmp.ln();
    let mut y = x;
    let mut ser = 1.000_000_000_190_015;
    for c in COF {
        y += 1.0;
        ser += c / y;
    }
    -tmp + (2.506_628_274_631_000_5 * ser / x).ln()
}

/// Lentz's modified continued fraction for the incomplete beta (Numerical
/// Recipes 6.4 `betacf`). Converges fast for `x < (a+1)/(a+b+2)`; the caller
/// applies the symmetry swap outside that range.
fn beta_continued_fraction(a: f64, b: f64, x: f64) -> f64 {
    const MAX_ITERS: usize = 300;
    const EPS: f64 = 3e-14;
    const TINY: f64 = 1e-300;

    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_ITERS {
        let mf = m as f64;
        let m2 = 2.0 * mf;
        // Even step.
        let mut aa = mf * (b - mf) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        // Odd step.
        aa = -(a + mf) * (qab + mf) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() <= EPS {
            break;
        }
    }
    h
}

/// Regularized incomplete beta `I_x(a, b)` (Numerical Recipes 6.4 `betai`):
/// the continued fraction above, with the `I_x(a, b) = 1 − I_{1−x}(b, a)`
/// symmetry swap outside the fraction's fast-converging range. `a`, `b` must
/// be positive; `x` outside `[0, 1]` clamps to the corresponding limit.
pub fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if !(x > 0.0) {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    if !(a > 0.0) || !(b > 0.0) {
        return f64::NAN;
    }
    let ln_beta = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b);
    let bt = (ln_beta + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * beta_continued_fraction(a, b, x) / a
    } else {
        1.0 - bt * beta_continued_fraction(b, a, 1.0 - x) / b
    }
}

// ── Student's t quantile ───────────────────────────────────────────────────

/// Largest `t` the quantile search will report — past it the upper tail of
/// every `nu ≥ 1` t distribution is far below any `alpha` a rejection
/// routine is configured with.
const T_QUANTILE_MAX: f64 = 1e3;

/// Upper-tail Student's t quantile: the `t` with `P(T > t) = p` for `nu`
/// degrees of freedom, `p ∈ (0, 0.5]`.
///
/// `P(T > t) = ½·I_{nu/(nu+t²)}(nu/2, 1/2)`, which is monotonically
/// decreasing in `t`, so the quantile is a plain bisection over
/// `[0, T_QUANTILE_MAX]` (ruling R-M4c-2). Degenerate input answers at the
/// limits rather than panicking: `p ≥ 0.5` → `0.0` (the median),
/// `p ≤ 0` → `+∞`, `nu ≤ 0` → NaN.
pub fn t_quantile(p: f64, nu: f64) -> f64 {
    if !(p > 0.0) {
        return f64::INFINITY;
    }
    if p >= 0.5 {
        return 0.0;
    }
    if !(nu > 0.0) {
        return f64::NAN;
    }
    let tail = |t: f64| 0.5 * regularized_incomplete_beta(nu / (nu + t * t), 0.5 * nu, 0.5);
    let (mut lo, mut hi) = (0.0f64, T_QUANTILE_MAX);
    if tail(hi) > p {
        // `p` is smaller than the tail at the far end of the bracket: the
        // true quantile is beyond it. Report the bracket end.
        //
        // Silent by design, and the analytic bound says why it is safe.
        // The only caller is [`esd_critical`], whose
        // `λ = t·(m−1) / sqrt((m−2+t²)·m)` (with `m = n−i ≥ 3`, its own
        // guard) is monotone in `t` and SATURATING: as `t → ∞` it tends to
        // `(m−1)/sqrt(m)`, largest in relative terms at the smallest
        // stack, `m = 3` → `2/sqrt(3) ≈ 1.154700`. The cap IS reachable
        // there — `n = 3, alpha = 0.001` asks for the `nu = 1` (Cauchy)
        // quantile at `p = alpha/6`, whose true `t ≈ 1910` — and at
        // `t = T_QUANTILE_MAX = 1e3` λ already reads 1.154699, i.e. within
        // 6e-7 (the error falls as `1/(2t²)`), far under 1e-4 and far
        // under anything a rejection decision can see. A caller that
        // wanted `t` itself rather than a saturating function of it would
        // need a real error here, not this.
        return hi;
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if mid == lo || mid == hi {
            break; // no floating-point room left
        }
        if tail(mid) > p {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo <= 1e-12 * (1.0 + hi) {
            break;
        }
    }
    0.5 * (lo + hi)
}

// ── Generalized-ESD critical values ────────────────────────────────────────

/// The two-tailed Rosner critical value `λ_i` for a stack of `n` samples
/// with `i` already removed, at significance `alpha` (math reference §3.4):
/// `p = α/(2(n−i))`, `t = t_{p, n−i−2}`,
/// `λ_i = t·(n−i−1)/sqrt((n−i−2+t²)(n−i))`.
///
/// Returns `+∞` when the test is not defined (`n − i < 3`, so the residual's
/// degrees of freedom would be non-positive, or an `alpha` outside `(0, 1)`)
/// — an infinite critical value means "nothing here is an outlier", which is
/// the safe answer for a rejection routine.
pub(crate) fn esd_critical(n: usize, i: usize, alpha: f64) -> f64 {
    if i + 3 > n || !(alpha > 0.0) || alpha >= 1.0 {
        return f64::INFINITY;
    }
    let nn = (n - i) as f64;
    let nu = nn - 2.0;
    let t = t_quantile(alpha / (2.0 * nn), nu);
    if !t.is_finite() {
        return f64::INFINITY;
    }
    t * (nn - 1.0) / ((nu + t * t) * nn).sqrt()
}

/// Distinct `(n, alpha)` keys the memo holds before it is dropped wholesale.
/// A band loop asks for one frame count (plus the handful of smaller ones
/// produced by missing/range-rejected samples), so the live key set is tiny;
/// the cap only stops an adversarial caller from growing the map without
/// bound.
const ESD_MEMO_MAX_KEYS: usize = 4096;

thread_local! {
    /// `(n, alpha·1e6 rounded)` → the `λ_0..λ_{k−1}` vector, grown on demand
    /// (ruling R-M4c-2). Every pixel stack of a plane shares one key, so the
    /// incomplete beta is evaluated at most `k` times per distinct `n`.
    static ESD_LAMBDA_MEMO: RefCell<HashMap<(usize, u32), Vec<f64>>> =
        RefCell::new(HashMap::new());
}

/// Call `f` with the first `k` ESD critical values for a stack of `n`
/// samples at significance `alpha`, computing and memoising any not cached
/// yet.
///
/// Contract: `f` must not itself call back into this function — the
/// thread-local memo is borrowed for the duration (a re-entrant call would
/// panic on the `RefCell`). The only caller is `combine::reject_esd`, whose
/// body reads the slice and nothing else.
pub(crate) fn with_esd_lambdas<R>(
    n: usize,
    alpha: f64,
    k: usize,
    f: impl FnOnce(&[f64]) -> R,
) -> R {
    let key = (n, (alpha.clamp(0.0, 1.0) * 1e6).round() as u32);
    ESD_LAMBDA_MEMO.with(|cell| {
        let mut memo = cell.borrow_mut();
        if memo.len() >= ESD_MEMO_MAX_KEYS && !memo.contains_key(&key) {
            memo.clear();
        }
        let lambdas = memo.entry(key).or_default();
        while lambdas.len() < k {
            let i = lambdas.len();
            lambdas.push(esd_critical(n, i, alpha));
        }
        f(&lambdas[..k])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny deterministic PRNG (mirrors `geometry::ransac::SplitMix64`; no
    /// `rand` crate dependency).
    struct SplitMix64(u64);
    impl SplitMix64 {
        fn next_f64(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn incomplete_beta_uniform_case() {
        // I_x(1, 1) = x. The continued fraction lands within a few ulps of
        // it rather than exactly on it (measured `I_0.5(1,1) =
        // 0.500_000_000_000_000_22`), so this is a tolerance, not an
        // equality.
        assert!(
            (regularized_incomplete_beta(0.5, 1.0, 1.0) - 0.5).abs() < 1e-12,
            "{}",
            regularized_incomplete_beta(0.5, 1.0, 1.0)
        );
        for &x in &[0.1, 0.25, 0.75, 0.9] {
            assert!(
                (regularized_incomplete_beta(x, 1.0, 1.0) - x).abs() < 1e-12,
                "I_{x}(1,1) = {}",
                regularized_incomplete_beta(x, 1.0, 1.0)
            );
        }
        assert_eq!(regularized_incomplete_beta(0.0, 2.0, 3.0), 0.0);
        assert_eq!(regularized_incomplete_beta(1.0, 2.0, 3.0), 1.0);
    }

    #[test]
    fn incomplete_beta_symmetry_identity() {
        // I_x(a, b) + I_{1−x}(b, a) = 1 for every (x, a, b).
        let mut rng = SplitMix64(0xBEEF_1234);
        for _ in 0..20 {
            let x = 0.02 + 0.96 * rng.next_f64();
            let a = 0.2 + 20.0 * rng.next_f64();
            let b = 0.2 + 20.0 * rng.next_f64();
            let s =
                regularized_incomplete_beta(x, a, b) + regularized_incomplete_beta(1.0 - x, b, a);
            assert!((s - 1.0).abs() < 1e-12, "x={x} a={a} b={b} sum={s}");
        }
    }

    #[test]
    fn t_quantile_matches_published_table_values() {
        assert!(
            (t_quantile(0.025, 10.0) - 2.2281).abs() < 1e-4,
            "{}",
            t_quantile(0.025, 10.0)
        );
        assert!(
            (t_quantile(0.005, 30.0) - 2.7500).abs() < 1e-3,
            "{}",
            t_quantile(0.005, 30.0)
        );
        assert!(
            (t_quantile(0.025, 1e6) - 1.9600).abs() < 1e-3,
            "{}",
            t_quantile(0.025, 1e6)
        );
        // Degenerate input answers at the limits.
        assert_eq!(t_quantile(0.5, 10.0), 0.0);
        assert_eq!(t_quantile(0.0, 10.0), f64::INFINITY);
        assert!(t_quantile(0.025, 0.0).is_nan());
        // Monotone in p and in nu.
        assert!(t_quantile(0.01, 10.0) > t_quantile(0.05, 10.0));
        assert!(t_quantile(0.025, 5.0) > t_quantile(0.025, 50.0));
    }

    #[test]
    fn esd_critical_matches_rosners_table() {
        // Rosner's published λ_1 for n = 50 at α = 0.05 is 3.13.
        let l = esd_critical(50, 0, 0.05);
        assert!((l - 3.128).abs() < 1e-2, "{l}");
        // λ shrinks as samples are removed (measured: 3.128247 at i = 0,
        // 3.120128 at i = 1) — a smaller remaining set has a smaller
        // critical value, so a later outlier needs a smaller studentized
        // residual to be declared one.
        let l1 = esd_critical(50, 1, 0.05);
        assert!(l1 < l, "{l1} vs {l}");
        // Undefined tests report +∞ (nothing is an outlier).
        assert_eq!(esd_critical(3, 1, 0.05), f64::INFINITY);
        assert_eq!(esd_critical(50, 0, 0.0), f64::INFINITY);
        assert_eq!(esd_critical(50, 0, 1.0), f64::INFINITY);
    }

    #[test]
    fn esd_lambda_memo_returns_the_same_values_as_direct_computation() {
        let direct: Vec<f64> = (0..6).map(|i| esd_critical(20, i, 0.05)).collect();
        let memoed = with_esd_lambdas(20, 0.05, 6, |l| l.to_vec());
        assert_eq!(memoed, direct);
        // A second, longer request extends the same cached vector.
        let longer = with_esd_lambdas(20, 0.05, 9, |l| l.to_vec());
        assert_eq!(&longer[..6], &direct[..]);
        assert_eq!(longer.len(), 9);
        // A different alpha is a different key.
        let other = with_esd_lambdas(20, 0.01, 6, |l| l.to_vec());
        assert!(other[0] > direct[0], "{:?} vs {:?}", other[0], direct[0]);
    }

    #[test]
    fn erfc_reference_values() {
        assert!((erfc(0.0) - 1.0).abs() < 2e-7);
        assert!((erfc(1.0) - 0.157_299_207).abs() < 2e-7);
        assert!((erfc(3.0) - 2.209_049_7e-5).abs() < 3e-7);
        assert!((erfc(-1.0) - (2.0 - erfc(1.0))).abs() < 1e-12);
    }

    #[test]
    fn erfinv_inverts_erf() {
        assert_eq!(erfinv(0.0), 0.0);
        assert!((erfinv(0.5) - 0.476_936_276).abs() < 1e-5);
        for &x in &[-0.9, -0.5, 0.1, 0.3, 0.683, 0.9] {
            let y = erfinv(x);
            assert!(((1.0 - erfc(y)) - x).abs() < 1e-5, "x = {x}");
        }
    }
}
