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
#[cfg(test)]
pub(crate) fn quantile_sorted(sorted: &[f64], p: f64) -> f64 {
    quantile_sorted_n(sorted, sorted.len(), p)
}

/// [`quantile_sorted`] over a sorted sample of `n` values of which only the first `sorted.len()` (at least
/// [`quantile_prefix_len`]) are held: the same operations on the same values.
fn quantile_sorted_n(sorted: &[f64], n: usize, p: f64) -> f64 {
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

/// How many leading sorted values [`quantile_sorted_n`] reads at `p` over `n` values.
fn quantile_prefix_len(n: usize, p: f64) -> usize {
    if n == 0 {
        return 0;
    }
    let i = (p * (n - 1) as f64).floor() as usize;
    if i + 1 >= n {
        n
    } else {
        i + 2
    }
}

/// `F(N) · quantile_{0.683}(|x − μ|)` over the sorted deviations.
#[cfg(test)]
pub(crate) fn sample_deviation(devs_sorted: &[f64]) -> f64 {
    sample_deviation_n(devs_sorted, devs_sorted.len())
}

fn sample_deviation_n(devs_prefix: &[f64], n: usize) -> f64 {
    small_sample_factor(n) * quantile_sorted_n(devs_prefix, n, 0.683)
}

/// Regress the lowest `trunc(0.683N + 0.317)` sorted deviations against
/// the half-normal quantiles `√2·erfinv((i + 1 − 0.317)/N)` with a line
/// through the origin and return `F(N)·ŷ(1)`; below 8 regression points
/// defers to [`sample_deviation`].
#[cfg(test)]
pub(crate) fn line_fit_deviation(devs_sorted: &[f64]) -> f64 {
    line_fit_deviation_n(devs_sorted, devs_sorted.len())
}

/// How many leading sorted deviations [`line_fit_deviation_n`] reads over `n` values.
fn line_fit_prefix_len(n: usize) -> usize {
    let m = (0.683 * n as f64 + 0.317).trunc() as usize;
    let q = quantile_prefix_len(n, 0.683);
    if m < 8 {
        q
    } else {
        m.max(q)
    }
}

/// [`line_fit_deviation`] over a sorted sample of `n` deviations of which the leading `devs_prefix` (at least
/// [`line_fit_prefix_len`]) are held: the same operations on the same values, in the same order.
fn line_fit_deviation_n(devs_prefix: &[f64], n: usize) -> f64 {
    let m = (0.683 * n as f64 + 0.317).trunc() as usize;
    if m < 8 {
        return sample_deviation_n(devs_prefix, n);
    }
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, &y) in devs_prefix.iter().take(m).enumerate() {
        let x = std::f64::consts::SQRT_2 * erfinv((i as f64 + 1.0 - 0.317) / n as f64);
        sxy += x * y;
        sxx += x * x;
    }
    if sxx <= 0.0 {
        return sample_deviation_n(devs_prefix, n);
    }
    small_sample_factor(n) * (sxy / sxx)
}

/// The median of `v` (the mean of the two middle values for an even count; NaN when empty); sorts `v`. Shared with
/// F-5's aperture correction (`photometry::aperture_correction`, Task 10).
pub(super) fn median_f64(v: &mut Vec<f64>) -> f64 {
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

/// Fill `out` with the first `need` of the sorted-ascending `|sv[i] − median|` over `sv` (the kept values, already
/// sorted ascending) and return the median. Because `sv` is sorted, deviations grow monotonically walking outward from
/// the median position on each side, so a two-pointer merge of the two sides — no re-sort — produces the sorted
/// deviations in order; the merge stops once `need` are out (the line fit and the quantile read only a prefix), which
/// leaves every value it did produce what the full merge produces.
fn sorted_deviations_prefix(sv: &[f64], need: usize, out: &mut Vec<f64>) -> f64 {
    out.clear();
    let m = sv.len();
    let med;
    let (mut l, mut r): (isize, isize);
    if m % 2 == 1 {
        let mid = m / 2;
        med = sv[mid];
        out.push(0.0);
        l = mid as isize - 1;
        r = mid as isize + 1;
    } else {
        let (lo, hi) = (m / 2 - 1, m / 2);
        med = 0.5 * (sv[lo] + sv[hi]);
        let d = (sv[hi] - med).abs();
        out.push(d);
        out.push(d);
        l = lo as isize - 1;
        r = hi as isize + 1;
    }
    while out.len() < need {
        let dl = if l >= 0 {
            Some((med - sv[l as usize]).abs())
        } else {
            None
        };
        let dr = if (r as usize) < m {
            Some((sv[r as usize] - med).abs())
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

/// Below this many deviations [`sorted_deviations_prefix_chunked`] defers to the plain merge.
const CHUNKED_MERGE_MIN: usize = 256;
/// Independent merges [`sorted_deviations_prefix_chunked`] runs side by side.
const MERGE_LANES: usize = 8;

/// [`sorted_deviations_prefix`] by a chunked merge — the same values in the same order, bit for bit (Task 13b Step
/// 4a). The plain merge's data-dependent branch mispredicts on most steps near the median; here the two sides'
/// deviations (`left[p] = |med − sv[l₀ − p]|`, `right[q] = |sv[r₀ + q] − med|`, both ascending) are written first,
/// padded with `+∞`, and the output is cut into [`MERGE_LANES`] chunks whose starting `(p, q)` a binary search finds
/// ([`merge_split`]); the chunks then merge branch-free and interleaved, each step the plain merge's own rule (`left`
/// when `left[p] ≤ right[q]`). Every chunk follows the full merge's trajectory, so `out` is the full merge's prefix.
/// The `+∞` padding stands in for an exhausted side: against a finite value it loses, and where it ties a real
/// `+∞` (an overflowed deviation) the value written is `+∞` either way and every later value is `+∞`, so the values
/// are the plain merge's. Windows with a non-finite end value or median, and short prefixes, take the plain merge.
fn sorted_deviations_prefix_chunked(
    sv: &[f64],
    need: usize,
    out: &mut Vec<f64>,
    left: &mut Vec<f64>,
    right: &mut Vec<f64>,
) -> f64 {
    let m = sv.len();
    if need < CHUNKED_MERGE_MIN
        || m < CHUNKED_MERGE_MIN
        || !(sv[0].is_finite() && sv[m - 1].is_finite())
    {
        return sorted_deviations_prefix(sv, need, out);
    }
    out.clear();
    let (med, l0, r0) = if m % 2 == 1 {
        let mid = m / 2;
        out.push(0.0);
        (sv[mid], mid - 1, mid + 1)
    } else {
        let (lo, hi) = (m / 2 - 1, m / 2);
        let med = 0.5 * (sv[lo] + sv[hi]);
        let d = (sv[hi] - med).abs();
        out.push(d);
        out.push(d);
        (med, lo - 1, hi + 1)
    };
    if !med.is_finite() {
        return sorted_deviations_prefix(sv, need, out);
    }
    let base = out.len();
    let n = need.min(m) - base;
    let (na, nb) = ((l0 + 1).min(n), (m - r0).min(n));
    left.clear();
    left.extend(sv[l0 + 1 - na..=l0].iter().rev().map(|&v| (med - v).abs()));
    left.resize(n + 1, f64::INFINITY);
    right.clear();
    right.extend(sv[r0..r0 + nb].iter().map(|&v| (v - med).abs()));
    right.resize(n + 1, f64::INFINITY);
    out.resize(base + n, 0.0);
    let dst = &mut out[base..];
    let (mut p, mut q, mut s, mut e) = (
        [0usize; MERGE_LANES],
        [0usize; MERGE_LANES],
        [0usize; MERGE_LANES],
        [0usize; MERGE_LANES],
    );
    for c in 0..MERGE_LANES {
        s[c] = c * n / MERGE_LANES;
        e[c] = (c + 1) * n / MERGE_LANES;
        p[c] = merge_split(left, right, s[c]);
        q[c] = s[c] - p[c];
    }
    // Every chunk holds at least ⌊n / lanes⌋ outputs.
    for _ in 0..n / MERGE_LANES {
        for c in 0..MERGE_LANES {
            let (a, b) = (left[p[c]], right[q[c]]);
            let take = a <= b;
            dst[s[c]] = if take { a } else { b };
            p[c] += take as usize;
            q[c] += (!take) as usize;
            s[c] += 1;
        }
    }
    for c in 0..MERGE_LANES {
        while s[c] < e[c] {
            let (a, b) = (left[p[c]], right[q[c]]);
            let take = a <= b;
            dst[s[c]] = if take { a } else { b };
            p[c] += take as usize;
            q[c] += (!take) as usize;
            s[c] += 1;
        }
    }
    med
}

/// How many `a` values the merge of the ascending `a` and `b` (taking `a[p]` when `a[p] ≤ b[q]`) has written after
/// `s` outputs: the least `p ≤ s` with `a[p] > b[s − p − 1]`, which is monotone in `p` (`a` before `b` on ties).
fn merge_split(a: &[f64], b: &[f64], s: usize) -> usize {
    let (mut lo, mut hi) = (0usize, s);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if a[mid] <= b[s - mid - 1] {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

// ---------------------------------------------------------------------------------------------------------------------
// Phase-0 rejections by verified decisions (Task 13b Step 4a).
//
// Phase 0's line fit evaluates `X_i = √2·erfinv((i + 1 − 0.317)/m)` for every regression point `i < k = trunc(0.683m +
// 0.317)` on every iteration, and `m` changes with every rejection, so nothing repeats; that `erfinv` (a `ln` plus a
// polynomial) is almost the whole cost of `rcr`. A rejection does not need the exact `σ`, only its DECISION: whether
// `min(d_lo, d_hi) < limit` and whether `d_hi ≤ d_lo`. [`verified_line_fit_rejection`] computes a cheap `σ̃` from a
// table of `X` with linear interpolation, a RIGOROUS interval `[σ_L, σ_U]` that holds the `σ` the exact code would
// compute (its own floating-point rounding included), and bounds on `d` over every `z` that interval allows; it takes
// a rejection only when both decisions are the same for every `σ` in the interval, and otherwise hands the iteration
// to the exact code. An iteration that rejects nothing, and therefore every `location`/`scale` this function returns,
// is always computed exactly. The proof follows; `u = 2⁻⁵³`, `γ_k = k·u/(1 − k·u)`.
//
// Platform assumption (the same libm the exact code already depends on): `f64::ln` and `f64::exp` are within relative
// 1e-12 / 1e-11 of the true value for normal results and within 1e-300 absolute below the normal range. Every libm
// Rust targets is within 1 ulp (2.2e-16), four orders of magnitude inside.
//
// (1) The table. `T_j = fl(√2·erfinv(j·h))`, `h = 2⁻¹⁶`, `j = 0..=45056` (`p ≤ 0.6875`; phase 0 reads
//     `p = (i + 0.683)/m ≤ 0.683` because `i ≤ k − 1 ≤ 0.683m − 0.683`). On that range `w = −ln(1 − p²) ≤ 0.64 < 5`,
//     so `erfinv` is the single branch `f(p) = √2·p·Q(w)` with `Q` the degree-8 polynomial in `w − 2.5`
//     (`|w − 2.5| ≤ 2.5`). With `Q₁ = Σ k|c_k|2.5^(k−1) ≤ 0.309`, `Q₂ = Σ k(k−1)|c_k|2.5^(k−2) ≤ 0.052`,
//     `w' = 2p/(1 − p²)`, `w'' = 2(1 + p²)/(1 − p²)²` (both increasing in `p`):
//     `|f''| = √2·|2Q'w' + p(Q''w'² + Q'w'')| ≤ √2·(2Q₁w' + p(Q₂w'² + Q₁w'')) ≤ 5.8` at `p = 0.6875`
//     (the test `half_normal_table_bound_constants_hold` recomputes it from the coefficients); `M₂ = 6` is used.
//     Linear interpolation on a cell of width `h` errs by at most `h²·M₂/8 = 1.7462e-10`.
// (2) Floating point around the table. `erfinv`'s own evaluation (`(1−p)(1+p)` 3 roundings, `ln`, `w − 2.5`, 16 Horner
//     roundings on terms ≤ 2.17, two products) is within relative `ε_f ≤ 44u + 0.3·1e-12 ≤ 3.1e-13` of `f`; it hits
//     both the nodes and the exact code's `X_i` (`2ε_f·max f ≤ 6.2e-13`, `max f ≤ 1.01`). The interpolation `T_j +
//     frac·(T_{j+1} − T_j)` adds ≤ 2u·1.01; the position `q = fl(fl(i + 1 − 0.317)·fl(2¹⁶/m))` differs from the exact
//     code's `p·2¹⁶` by ≤ 3.01u relative, i.e. ≤ `max f'·3.01u·0.6875 ≤ 4·2.3e-16`. All of (2) is < 1e-12, so
//     `|X̃_i − X_i| ≤ e = 1.7462e-10 + 1e-12 ≤ HALF_NORMAL_ERR`. The table is checked strictly increasing at build
//     (so `0 ≤ X̃_i`); a table that fails it is not used.
// (3) The σ interval. With `y_i ≥ 0` the ascending deviations (identical in both paths), `A = fl Σ X̃_i y_i`,
//     `B = fl Σ X̃_i²` (any order: every term is non-negative, so each sum is within relative `γ_k` of its exact
//     value), `s_x = fl(k·T_top) ≥ ΣX̃_i/(1 + 3u)` (every `X̃_i` is at most the top node of its cell to within 2u, and
//     that node at most the one above the last cell) and `s_y = fl(k·y_{k−1}) ≥ Σy_i/(1 + u)`, the exact code's sums
//     satisfy
//     `sxy ∈ A·[(1 − γ − ε₁')(1 − γ), (1 + 2γ + ε₁')(1 + γ)]`, `ε₁' = e·Σy_i/A` (from `|Σ(X_i − X̃_i)y_i| ≤ e·Σy_i`),
//     and `sxx ∈ B·[(1 − γ − ε₂')(1 − γ), (1 + 2γ + ε₂')(1 + γ)]`, `ε₂' = e·(2ΣX̃_i + k·e)/B` (from
//     `|ΣX_i² − ΣX̃_i²| ≤ e·Σ(X_i + X̃_i)`); the computed `ε₁ = e·s_y/A`, `ε₂ = e·(2s_x + k·e)/B` are within
//     `(1 + 2γ)(1 + 4u)` of upper bounds on them. The exact `σ = fl(F(m)·fl(sxy/sxx))` and our
//     `σ̃ = fl(F(m)·fl(A/B))` then differ by a relative `≤ 5γ + ε₁' + ε₂' + 6u + O(ρ²)`; `ρ = 1.01·(ε₁ + ε₂) + 8γ +
//     8u` covers it (with `ρ ≤ 1e-3`, `γ ≤ 1e-6` the quadratic terms are below the 1 % slack), and
//     `σ_L = fl(σ̃·fl(1 − ρ))`, `σ_U = fl(σ̃·fl(1 + ρ))` keep `σ ∈ [σ_L, σ_U]` with their own rounding.
//     `σ_L > 0` also settles the exact code's `sxx > 0` and `σ > 0` branches.
// (4) The decision. `a = μ − v_min ≥ 0` and `b = v_max − μ ≥ 0` are the exact code's own operands (μ is the median, the
//     same float in both paths); correctly rounded division is monotone, so the exact `z = fl(a/σ)` lies in
//     `[fl(a/σ_U), fl(a/σ_L)]`. `d(z) = fl(m·gauss_tail(z))` is the exact code's float function; `G(z)` is the same
//     formula in exact arithmetic, `m/2·P(t)·e^(−x²)`, `x = z/√2`, `t = 1/(1 + 0.3275911x)`, `P(t) = t·Σ A_k t^k`.
//     `G` is strictly decreasing for `z ≥ 0`: `t` decreases, `P'(t) = Σ (k+1)A_k t^k > 0.23` on `[0, 1]` (test
//     `gauss_tail_bound_constants_hold`, grid plus Lipschitz) so `P(t(x))` decreases and stays positive, and `e^(−x²)`
//     decreases. For every float `z ≥ 0`, `|d(z) − G(z)| ≤ ε_g·G(z) + α`, `ε_g = 1e-10`, `α = 1e-280`: while
//     `x² ≤ 700` every intermediate is normal and the relative error is `≤ 3.01u·700` (the rounded exponent) `+ 1e-11`
//     (exp) `+ ~600u` (`t`, the Horner of condition `≤ 4.48/0.23`, the products) `< 1.1e-11`; beyond, both `d` and
//     `G` are below `m·2e-300 ≤ α`. Hence over the interval `d ∈ [(d(z_U) − α)(1 − 4ε_g) − α, (d(z_L) + α)(1 + 4ε_g) +
//     α]` (`−α` as the floor when `d(z_U) ≤ α`), the margins absorbing the bounds' own rounding. A rejection is taken
//     when `min(upper_lo, upper_hi) < limit` and the side is certain: `a` and `b` bit-equal (then `d_lo` and `d_hi`
//     are the same float and the exact code takes the high side), `upper_hi ≤ lower_lo` (high), or
//     `lower_hi > upper_lo` (low). Anything else, any non-finite operand, and `m ≤ 3` (so the iteration after a fast
//     rejection always evaluates) go to the exact code.
// ---------------------------------------------------------------------------------------------------------------------

/// Cells per unit `p` of the half-normal quantile table (`h = 2⁻¹⁶`; a power of two keeps `p·2¹⁶` exact).
const HALF_NORMAL_CELLS_PER_UNIT: f64 = 65_536.0;
/// Cells of the table: it covers `p ∈ [0, 0.6875]`.
const HALF_NORMAL_CELLS: usize = 45_056;
/// The interpolation error bound on the half-normal quantile at `p ≤ 0.6875`: `h²·M₂/8 + 1e-12` (proof above).
const HALF_NORMAL_ERR: f64 = 1.76e-10;
/// Relative error bound of `m·gauss_tail(z)` against its exact-arithmetic formula (proof above, step 4).
const TAIL_REL: f64 = 1e-10;
/// Absolute error bound of `m·gauss_tail(z)` below the normal range (proof above, step 4).
const TAIL_ABS: f64 = 1e-280;

/// `T_j = √2·erfinv(j/2¹⁶)` for `j = 0..=HALF_NORMAL_CELLS`, built once, or `None` when the build check fails (it is
/// then never used and every phase-0 iteration runs exactly).
fn half_normal_table() -> Option<&'static [f64]> {
    static TABLE: std::sync::OnceLock<Option<Vec<f64>>> = std::sync::OnceLock::new();
    TABLE
        .get_or_init(|| {
            let t: Vec<f64> = (0..=HALF_NORMAL_CELLS)
                .map(|j| std::f64::consts::SQRT_2 * erfinv(j as f64 / HALF_NORMAL_CELLS_PER_UNIT))
                .collect();
            let ok = t[0] == 0.0 && t.iter().all(|x| x.is_finite()) && t.windows(2).all(|w| w[1] > w[0]);
            if ok {
                Some(t)
            } else {
                tracing::warn!(
                    count = t.len(),
                    "rcr: the half-normal table is not strictly increasing; every rejection is decided exactly"
                );
                None
            }
        })
        .as_deref()
}

/// `X̃_i`: the table read at `q = fl(fl(i + 1 − 0.317)·scale)`, `scale = fl(2¹⁶/m)`, by linear interpolation; the
/// caller keeps `q` inside the table.
#[inline]
fn half_normal_interp(table: &[f64], i: usize, scale: f64) -> f64 {
    let q = (i as f64 + 1.0 - 0.317) * scale;
    let j = q as usize;
    let t0 = table[j];
    t0 + (q - j as f64) * (table[j + 1] - t0)
}

/// The interval `[σ_L, σ_U]` that holds the line-fit `σ` [`line_fit_deviation_n`] computes over the same `devs` and
/// `m`, from the table (proof above, steps 1–3); `None` when the line fit does not apply (`k < 8`) or the interval is
/// not usable.
fn line_fit_sigma_bounds(devs: &[f64], m: usize, table: &[f64]) -> Option<(f64, f64)> {
    let k = (0.683 * m as f64 + 0.317).trunc() as usize;
    if k < 8 || devs.len() < k {
        return None;
    }
    let scale = HALF_NORMAL_CELLS_PER_UNIT / m as f64;
    // `q` grows with `i`, so the last one bounds every index.
    let q_last = ((k - 1) as f64 + 1.0 - 0.317) * scale;
    if !(q_last < (table.len() - 1) as f64) {
        return None;
    }
    // Two accumulator pairs, even and odd `i` (the bound holds for any summation order).
    let ys = &devs[..k];
    let (mut sxy0, mut sxx0, mut sxy1, mut sxx1) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut i = 0;
    while i + 1 < k {
        let (x0, x1) = (
            half_normal_interp(table, i, scale),
            half_normal_interp(table, i + 1, scale),
        );
        sxy0 += x0 * ys[i];
        sxx0 += x0 * x0;
        sxy1 += x1 * ys[i + 1];
        sxx1 += x1 * x1;
        i += 2;
    }
    if i < k {
        let x = half_normal_interp(table, i, scale);
        sxy0 += x * ys[i];
        sxx0 += x * x;
    }
    let (sxy, sxx) = (sxy0 + sxy1, sxx0 + sxx1);
    let kf = k as f64;
    // Σ X̃_i ≤ k·T[top] (every X̃_i is at most its cell's upper node, to within 2u) and Σ y_i ≤ k·y_{k−1} (the
    // deviations ascend); the 1 % slack in `rho` absorbs the few-u slop.
    let sx = kf * table[q_last as usize + 1];
    let sy = kf * ys[k - 1];
    if !(sxy > 0.0 && sxx > 0.0 && sxy.is_finite() && sxx.is_finite() && sy.is_finite()) {
        return None;
    }
    let u = 0.5 * f64::EPSILON;
    let gamma = kf * u / (1.0 - kf * u);
    if !(gamma <= 1e-6) {
        return None;
    }
    let eps1 = HALF_NORMAL_ERR * sy / sxy;
    let eps2 = HALF_NORMAL_ERR * (2.0 * sx + kf * HALF_NORMAL_ERR) / sxx;
    let rho = 1.01 * (eps1 + eps2) + 8.0 * gamma + 8.0 * u;
    if !(rho <= 1e-3) {
        return None;
    }
    let sigma = small_sample_factor(m) * (sxy / sxx);
    let (lo, hi) = (sigma * (1.0 - rho), sigma * (1.0 + rho));
    if lo > 0.0 && hi.is_finite() {
        Some((lo, hi))
    } else {
        None
    }
}

/// Bounds on the exact code's `m·gauss_tail(z)` over every float `z` in `[z_small, z_large]` (proof above, step 4).
fn tail_bounds(m: f64, z_small: f64, z_large: f64) -> (f64, f64) {
    let upper = (m * gauss_tail(z_small) + TAIL_ABS) * (1.0 + 4.0 * TAIL_REL) + TAIL_ABS;
    let g = m * gauss_tail(z_large);
    let lower = if g > TAIL_ABS {
        (g - TAIL_ABS) * (1.0 - 4.0 * TAIL_REL) - TAIL_ABS
    } else {
        -TAIL_ABS
    };
    (lower, upper)
}

/// Phase 0's rejection decided without the exact `σ` when the decision is the same for every `σ` the bound allows:
/// `Some(true)` rejects the maximum, `Some(false)` the minimum, `None` hands the iteration to the exact code (proof
/// above).
fn verified_line_fit_rejection(
    devs: &[f64],
    m: usize,
    mu: f64,
    vmin: f64,
    vmax: f64,
    limit: f64,
    table: &[f64],
) -> Option<bool> {
    if m <= 3 || !(limit.is_finite() && mu.is_finite() && vmin.is_finite() && vmax.is_finite()) {
        return None;
    }
    let (s_lo, s_hi) = line_fit_sigma_bounds(devs, m, table)?;
    let (a, b) = (mu - vmin, vmax - mu);
    let mf = m as f64;
    let (lo_lower, lo_upper) = tail_bounds(mf, a / s_hi, a / s_lo);
    let (hi_lower, hi_upper) = tail_bounds(mf, b / s_hi, b / s_lo);
    if !(lo_upper.min(hi_upper) < limit) {
        return None;
    }
    if a.to_bits() == b.to_bits() || hi_upper <= lo_lower {
        Some(true)
    } else if hi_lower > lo_upper {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
thread_local! {
    /// Phase-0 line-fit REJECTIONS decided by the bound / by the exact code, on this thread (tests only).
    static VERIFIED_DECISIONS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
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
///
/// The kept values stay sorted in one window `lo..hi` of two parallel arrays (indices, values) across all three
/// phases — a rejection is the extreme at either end, the survivors keep their order (a stable sort of the survivors
/// is the earlier order with the rejected ones removed), so no phase re-sorts — and each iteration reads contiguous
/// values and only the sorted deviations its estimator uses. Every statistic is the same sum in the same order.
///
/// Phase 0's rejections are decided by [`verified_line_fit_rejection`] whenever its rigorous bound settles them (no
/// `erfinv` on those iterations); every other iteration, and every one that rejects nothing, runs the exact code, so
/// the result is bit-for-bit the exact one (proof above the helper).
pub fn rcr(values: &[f64], limit: f64) -> RcrResult {
    let _timer = super::substeps::RcrTimer::start();
    let table = half_normal_table();
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
    let (mut left, mut right): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let sv: Vec<f64> = order.iter().map(|&i| values[i]).collect();
    let (mut lo, mut hi) = (0usize, n);
    for phase in 0..3 {
        loop {
            let m = hi - lo;
            if m < 3 {
                break;
            }
            let win = &sv[lo..hi];
            let (imin, imax) = (order[lo], order[hi - 1]);
            let (vmin, vmax) = (win[0], win[m - 1]);
            let (mu, sigma) = if phase < 2 {
                let need = if phase == 0 {
                    line_fit_prefix_len(m)
                } else {
                    quantile_prefix_len(m, 0.683)
                };
                let med =
                    sorted_deviations_prefix_chunked(win, need, &mut devs, &mut left, &mut right);
                if phase == 0 {
                    if let Some(table) = table {
                        let verified =
                            verified_line_fit_rejection(&devs, m, med, vmin, vmax, limit, table);
                        #[cfg(test)]
                        if verified.is_some() {
                            VERIFIED_DECISIONS.with(|c| c.set((c.get().0 + 1, c.get().1)));
                        }
                        match verified {
                            Some(true) => {
                                kept[imax] = false;
                                hi -= 1;
                                continue;
                            }
                            Some(false) => {
                                kept[imin] = false;
                                lo += 1;
                                continue;
                            }
                            None => {}
                        }
                    }
                }
                let s = if phase == 0 {
                    line_fit_deviation_n(&devs, m)
                } else {
                    sample_deviation_n(&devs, m)
                };
                (med, s)
            } else {
                let mut sum = 0.0f64;
                for &v in win {
                    sum += v;
                }
                let mu = sum / m as f64;
                let mut ss = 0.0f64;
                for &v in win {
                    let d = v - mu;
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
            let d_lo = m as f64 * gauss_tail((mu - vmin) / sigma);
            let d_hi = m as f64 * gauss_tail((vmax - mu) / sigma);
            if d_lo.min(d_hi) < limit {
                #[cfg(test)]
                if phase == 0 && (0.683 * m as f64 + 0.317).trunc() as usize >= 8 {
                    VERIFIED_DECISIONS.with(|c| c.set((c.get().0, c.get().1 + 1)));
                }
                if d_hi <= d_lo {
                    kept[imax] = false;
                    hi -= 1;
                } else {
                    kept[imin] = false;
                    lo += 1;
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

    /// The pre-Task-13b-1a implementation, verbatim, as the oracle of the bit-equality tests.
    mod reference {
        /// REFERENCE (the pre-Task-13b-1a body, kept for the bit-equality property test). Fill `out` with the sorted-ascending `|values[i] − median|` for every `i`
        /// in `order` (already sorted ascending by `values[i]`) and return the
        /// median. Because `order` is value-sorted, deviations grow monotonically
        /// walking outward from the median position on each side, so a two-pointer
        /// merge of the two sides — no re-sort — produces the sorted deviations in
        /// `O(order.len())`.
        pub(super) fn sorted_deviations_from_median_ref(
            values: &[f64],
            order: &[usize],
            out: &mut Vec<f64>,
        ) -> f64 {
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

        /// Bulk-mode RCR: three phases of decreasing robustness (median + line-fit
        /// deviation, median + sample deviation, mean + standard deviation), each
        /// iterated: while `n·Q(|extreme − μ|/σ) < limit` reject the single most
        /// extreme value (ties go to the high side). `limit = 0.5` is Chauvenet's
        /// criterion. Below 3 values nothing is rejected.
        pub(super) fn rcr_reference(
            values: &[f64],
            limit: f64,
        ) -> crate::star_measure::robust::RcrResult {
            let n = values.len();
            let mut kept = vec![true; n];
            if n < 3 {
                let mut v = values.to_vec();
                return crate::star_measure::robust::RcrResult {
                    location: crate::star_measure::robust::median_f64(&mut v),
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
                        let med = sorted_deviations_from_median_ref(values, &order, &mut devs);
                        let s = if phase == 0 {
                            crate::star_measure::robust::line_fit_deviation(&devs)
                        } else {
                            crate::star_measure::robust::sample_deviation(&devs)
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
                    let d_lo = m as f64
                        * crate::star_measure::robust::gauss_tail((mu - values[imin]) / sigma);
                    let d_hi = m as f64
                        * crate::star_measure::robust::gauss_tail((values[imax] - mu) / sigma);
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
            crate::star_measure::robust::RcrResult {
                location,
                scale,
                kept,
                rejected,
            }
        }

        /// The Task-13b-1a implementation (one sorted window, every iteration exact), verbatim but for the timer,
        /// as the oracle of the Step 4a verified-decision tests.
        pub(super) fn rcr_windowed(
            values: &[f64],
            limit: f64,
        ) -> crate::star_measure::robust::RcrResult {
            use crate::star_measure::robust::{
                gauss_tail, line_fit_deviation_n, line_fit_prefix_len, median_f64,
                quantile_prefix_len, sample_deviation_n, sorted_deviations_prefix, RcrResult,
            };
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
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
            let sv: Vec<f64> = order.iter().map(|&i| values[i]).collect();
            let (mut lo, mut hi) = (0usize, n);
            for phase in 0..3 {
                loop {
                    let m = hi - lo;
                    if m < 3 {
                        break;
                    }
                    let win = &sv[lo..hi];
                    let (imin, imax) = (order[lo], order[hi - 1]);
                    let (vmin, vmax) = (win[0], win[m - 1]);
                    let (mu, sigma) = if phase < 2 {
                        let need = if phase == 0 {
                            line_fit_prefix_len(m)
                        } else {
                            quantile_prefix_len(m, 0.683)
                        };
                        let med = sorted_deviations_prefix(win, need, &mut devs);
                        let s = if phase == 0 {
                            line_fit_deviation_n(&devs, m)
                        } else {
                            sample_deviation_n(&devs, m)
                        };
                        (med, s)
                    } else {
                        let mut sum = 0.0f64;
                        for &v in win {
                            sum += v;
                        }
                        let mu = sum / m as f64;
                        let mut ss = 0.0f64;
                        for &v in win {
                            let d = v - mu;
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
                    let d_lo = m as f64 * gauss_tail((mu - vmin) / sigma);
                    let d_hi = m as f64 * gauss_tail((vmax - mu) / sigma);
                    if d_lo.min(d_hi) < limit {
                        if d_hi <= d_lo {
                            kept[imax] = false;
                            hi -= 1;
                        } else {
                            kept[imin] = false;
                            lo += 1;
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
    }

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
            // A clean Gaussian plus four distinct planted outliers.
            //
            // The two paths do NOT see the same numbers: `values` is
            // `Vec<f64>` and neither the Gaussian draws nor
            // `6.0 + 0.5·k + 0.01·seed` is f32-exact, so the pixel path's
            // `v as f32` rounds every sample. What makes the comparison
            // sound is that RCR's SURVIVOR SET is robust to that rounding
            // on these samples — the outliers sit whole units clear of the
            // clean population's spread and the survivors are nowhere near
            // the rejection boundary, so a ULP of f32 rounding cannot move
            // a sample across it. A disagreement here is therefore the
            // algorithm drifting apart, not the width; a fixture whose
            // samples crowded the boundary would need matched widths
            // instead.
            let mut values = gaussian(60, 1000 + seed);
            for k in 0..4 {
                values.push(6.0 + 0.5 * k as f64 + 0.01 * seed as f64);
            }
            let n = values.len();
            let mine = rcr(&values, limit);

            let out: Vec<f32> = values.iter().map(|&v| v as f32).collect();
            let mut work: Vec<(f32, u16)> = out
                .iter()
                .enumerate()
                .map(|(i, &v)| (v, i as u16))
                .collect();
            let weights = vec![1.0f32; n];
            let mut mask = vec![0u64; mask_words(n)];
            let (_, rejected, _) = combine_pixel_weighted(
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

    fn assert_same_result(a: &RcrResult, b: &RcrResult, what: &str) {
        assert_eq!(a.kept, b.kept, "{what}: kept");
        assert_eq!(a.rejected, b.rejected, "{what}: rejected");
        assert_eq!(
            a.location.to_bits(),
            b.location.to_bits(),
            "{what}: location"
        );
        assert_eq!(a.scale.to_bits(), b.scale.to_bits(), "{what}: scale");
    }

    /// The new `rcr` against BOTH oracles: the pre-1a implementation and 1a's windowed exact one (Step 4a's oracle).
    fn assert_rcr_bit_equal(v: &[f64], limit: f64, what: &str) {
        let a = rcr(v, limit);
        assert_same_result(
            &a,
            &reference::rcr_windowed(v, limit),
            &format!("{what} (windowed)"),
        );
        assert_same_result(
            &a,
            &reference::rcr_reference(v, limit),
            &format!("{what} (pre-1a)"),
        );
    }

    /// Phase-0 line-fit rejections decided by the bound / by the exact code on this thread since the last call.
    fn take_verified_decisions() -> (u64, u64) {
        VERIFIED_DECISIONS.with(|c| c.replace((0, 0)))
    }

    /// Task 13b 1a: the windowed RCR is bit-equal to the pre-change implementation (kept above as the oracle) on
    /// random samples of every shape the estimator meets: Gaussian, contaminated, heavy-tailed, tie-laden, tiny.
    #[test]
    fn rcr_is_bit_equal_to_the_reference_on_random_vectors() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        for n in (0..40).chain([57, 100, 257, 1000, 4096]) {
            for kind in 0..5 {
                let v: Vec<f64> = (0..n)
                    .map(|i| {
                        let u = next().max(1e-12);
                        match kind {
                            0 => u,
                            1 => -u.ln(),
                            2 => (1.0 / u).powf(1.5),
                            3 => (u * 8.0).floor() * 0.25,
                            _ => {
                                if i % 17 == 0 {
                                    50.0 * u
                                } else {
                                    u
                                }
                            }
                        }
                    })
                    .collect();
                for limit in [0.5, 0.1, 2.0] {
                    assert_rcr_bit_equal(&v, limit, &format!("n={n} kind={kind} limit={limit}"));
                }
            }
        }
    }

    /// Task 13b 1a: on real fit vectors (`ATH_RCR_REAL`: a text file, one finite value per line, blank lines
    /// separating vectors; the star signals of set 204 planes) — skipped when unset.
    #[test]
    fn rcr_is_bit_equal_to_the_reference_on_real_fit_vectors() {
        let Ok(path) = std::env::var("ATH_RCR_REAL") else {
            return;
        };
        let text = std::fs::read_to_string(&path).expect("ATH_RCR_REAL readable");
        let mut count = 0;
        for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
            let v: Vec<f64> = block
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            assert_rcr_bit_equal(&v, 0.5, &format!("real vector {count} (n={})", v.len()));
            count += 1;
        }
        assert!(count > 0, "no vector in {path}");
        let (fast, exact) = take_verified_decisions();
        eprintln!(
            "ATH_RCR_REAL: {count} vectors bit-equal; phase-0 line-fit rejections: {fast} by the bound, {exact} exact"
        );
    }

    /// Step 4a: the chunked merge writes the plain merge's values bit for bit — odd and even windows, prefixes from
    /// the threshold to the whole window, heavy ties, one-sided data (one side exhausts early), and deviations that
    /// overflow to `+∞` (the padding ties a real `+∞`).
    #[test]
    fn chunked_merge_matches_the_plain_merge() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut uni = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let (mut plain, mut fast, mut left, mut right) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut cases = 0;
        for m in (250..=420).chain([513, 1000, 1001, 4096, 4097, 21_516]) {
            for kind in 0..6 {
                let mut sv: Vec<f64> = (0..m)
                    .map(|i| match kind {
                        0 => uni(),
                        1 => (uni() * 6.0).floor(),
                        2 => -uni().ln() * if i % 9 == 0 { 1e3 } else { 1.0 },
                        3 => (uni() - 0.5) * 1.7e308,
                        4 => {
                            if i < m / 3 {
                                1.0
                            } else {
                                1.0 + uni()
                            }
                        }
                        _ => {
                            if i % 2 == 0 {
                                -1.6e308
                            } else {
                                1.6e308 * uni()
                            }
                        }
                    })
                    .collect();
                sv.sort_by(|a, b| a.total_cmp(b));
                let needs = [
                    CHUNKED_MERGE_MIN,
                    m * 683 / 1000,
                    m * 683 / 1000 + 2,
                    m - 1,
                    m,
                ];
                for &need in needs.iter().filter(|&&n| n >= 1 && n <= m) {
                    let a = sorted_deviations_prefix(&sv, need, &mut plain);
                    let b = sorted_deviations_prefix_chunked(
                        &sv, need, &mut fast, &mut left, &mut right,
                    );
                    assert_eq!(
                        a.to_bits(),
                        b.to_bits(),
                        "m {m} kind {kind} need {need}: median"
                    );
                    assert_eq!(
                        plain.len(),
                        fast.len(),
                        "m {m} kind {kind} need {need}: length"
                    );
                    for (i, (x, y)) in plain.iter().zip(&fast).enumerate() {
                        assert_eq!(
                            x.to_bits(),
                            y.to_bits(),
                            "m {m} kind {kind} need {need}: value {i}"
                        );
                    }
                    cases += 1;
                }
            }
        }
        assert!(cases > 4_000, "{cases}");
    }

    /// Step 4a, bound step (1): the analytic `|f''|` bound behind [`HALF_NORMAL_ERR`], recomputed from `erfinv`'s own
    /// coefficients (the local copy is tied to `erfinv` bit-for-bit over the table range).
    #[test]
    fn half_normal_table_bound_constants_hold() {
        const C: [f64; 9] = [
            1.50140941,
            0.246640727,
            -0.00417768164,
            -0.00125372503,
            0.00021858087,
            -4.39150654e-06,
            -3.5233877e-06,
            3.43273939e-07,
            2.81022636e-08,
        ];
        let local = |x: f64| {
            let w = -((1.0 - x) * (1.0 + x)).ln() - 2.5;
            let mut p = C[8];
            for k in (0..8).rev() {
                p = C[k] + p * w;
            }
            p * x
        };
        let p_max = HALF_NORMAL_CELLS as f64 / HALF_NORMAL_CELLS_PER_UNIT;
        assert_eq!(p_max, 0.6875);
        for j in 0..=50_000 {
            let x = p_max * j as f64 / 50_000.0;
            assert_eq!(local(x).to_bits(), erfinv(x).to_bits(), "x = {x}");
        }
        let s = 1.0 - p_max * p_max;
        assert!(
            -s.ln() < 5.0,
            "the table range stays on erfinv's first branch"
        );
        let q1: f64 = (1..9)
            .map(|k| k as f64 * C[k].abs() * 2.5f64.powi(k as i32 - 1))
            .sum();
        let q2: f64 = (2..9)
            .map(|k| (k * (k - 1)) as f64 * C[k].abs() * 2.5f64.powi(k as i32 - 2))
            .sum();
        let (w1, w2) = (2.0 * p_max / s, 2.0 * (1.0 + p_max * p_max) / (s * s));
        let m2 = std::f64::consts::SQRT_2 * (2.0 * q1 * w1 + p_max * (q2 * w1 * w1 + q1 * w2));
        assert!(
            q1 <= 0.309 && q2 <= 0.052 && m2 <= 5.8,
            "q1 {q1} q2 {q2} m2 {m2}"
        );
        let h = 1.0 / HALF_NORMAL_CELLS_PER_UNIT;
        assert!(h * h * 6.0 / 8.0 + 1e-12 <= HALF_NORMAL_ERR);
    }

    /// Step 4a, bound step (4): `P'(t) > 0.23` and `Σ A_k t^k ≥ 0.23` on `[0, 1]` (grid plus a Lipschitz bound), from
    /// a local copy of `erfc`'s constants tied to `erfc` bit-for-bit.
    #[test]
    fn gauss_tail_bound_constants_hold() {
        const P: f64 = 0.3275911;
        const A: [f64; 5] = [
            0.254829592,
            -0.284496736,
            1.421413741,
            -1.453152027,
            1.061405429,
        ];
        let local = |x: f64| {
            let t = 1.0 / (1.0 + P * x);
            let poly = t * (A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4]))));
            poly * (-x * x).exp()
        };
        for i in 0..=40_000 {
            let x = i as f64 * 1e-3;
            assert_eq!(local(x).to_bits(), erfc(x).to_bits(), "x = {x}");
        }
        let r =
            |t: f64| A[0] + t * (2.0 * A[1] + t * (3.0 * A[2] + t * (4.0 * A[3] + t * 5.0 * A[4])));
        let q = |t: f64| A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4])));
        let lr = 2.0 * A[1].abs() + 6.0 * A[2].abs() + 12.0 * A[3].abs() + 20.0 * A[4].abs();
        let lq = A[1].abs() + 2.0 * A[2].abs() + 3.0 * A[3].abs() + 4.0 * A[4].abs();
        let n = 100_000;
        let step = 1.0 / n as f64;
        let (mut rmin, mut qmin) = (f64::INFINITY, f64::INFINITY);
        for i in 0..=n {
            let t = i as f64 * step;
            rmin = rmin.min(r(t));
            qmin = qmin.min(q(t));
        }
        assert!(rmin - lr * step > 0.23, "P' min {rmin}");
        assert!(qmin - lq * step >= 0.23, "q min {qmin}");
        let qhat: f64 = A.iter().map(|a| a.abs()).sum();
        assert!(qhat <= 4.48, "{qhat}");
    }

    /// Step 4a, bound steps (1)–(2): `|X̃_i − X_i| ≤ HALF_NORMAL_ERR` for EVERY regression point the line fit reads at
    /// every `m` up to 6 000 and at large `m` the stars reach, and on a dense sweep of `p` over the whole table.
    #[test]
    fn half_normal_table_error_is_within_the_bound() {
        let table = half_normal_table().expect("the table builds");
        let mut worst = 0.0f64;
        let mut check = |m: usize| {
            let k = (0.683 * m as f64 + 0.317).trunc() as usize;
            let scale = HALF_NORMAL_CELLS_PER_UNIT / m as f64;
            for i in 0..k {
                let exact = std::f64::consts::SQRT_2 * erfinv((i as f64 + 1.0 - 0.317) / m as f64);
                let err = (half_normal_interp(table, i, scale) - exact).abs();
                assert!(err <= HALF_NORMAL_ERR, "m {m} i {i}: {err}");
                worst = worst.max(err);
            }
        };
        for m in 12..=6_000 {
            check(m);
        }
        for m in [
            9_999, 20_014, 21_750, 23_845, 24_576, 50_000, 131_071, 1_000_003,
        ] {
            check(m);
        }
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..2_000_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let p = (state >> 11) as f64 / (1u64 << 53) as f64 * 0.6874;
            let q = p * HALF_NORMAL_CELLS_PER_UNIT;
            let j = q as usize;
            let x = table[j] + (q - j as f64) * (table[j + 1] - table[j]);
            let err = (x - std::f64::consts::SQRT_2 * erfinv(p)).abs();
            assert!(err <= HALF_NORMAL_ERR, "p {p}: {err}");
            worst = worst.max(err);
        }
        eprintln!(
            "half-normal table: worst |X~ - X| {worst:e} against the bound {HALF_NORMAL_ERR:e}"
        );
    }

    /// The same over every `m` up to 40 000 (`cargo test --release -- --ignored`): every `(i, m)` pair phase 0 reads.
    #[test]
    #[ignore]
    fn half_normal_table_error_is_within_the_bound_up_to_40000() {
        let table = half_normal_table().expect("the table builds");
        let mut worst = 0.0f64;
        for m in 12..=40_000usize {
            let k = (0.683 * m as f64 + 0.317).trunc() as usize;
            let scale = HALF_NORMAL_CELLS_PER_UNIT / m as f64;
            for i in 0..k {
                let exact = std::f64::consts::SQRT_2 * erfinv((i as f64 + 1.0 - 0.317) / m as f64);
                let err = (half_normal_interp(table, i, scale) - exact).abs();
                assert!(err <= HALF_NORMAL_ERR, "m {m} i {i}: {err}");
                worst = worst.max(err);
            }
        }
        eprintln!("half-normal table up to m = 40000: worst {worst:e}");
    }

    /// Step 4a: the verified-decision `rcr` is bit-equal to both oracles on 12 000 random vectors of every shape the
    /// estimator meets — Gaussian, contaminated, heavy-tailed (Pareto, Cauchy), star-flux-like (log-normal plus a power
    /// tail), quantized with many ties, mirrored (so the two extremes tie exactly), all-equal, tiny n, and n at the
    /// line-fit threshold (k = 8 at m = 12) — and the bound decides most phase-0 rejections.
    #[test]
    fn rcr_with_verified_decisions_is_bit_equal_on_random_vectors() {
        let mut state = 0xD1B5_4A32_D192_ED03u64;
        let mut uni = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        take_verified_decisions();
        let mut vectors = 0;
        let mut per_kind = [(0u64, 0u64); 10];
        for idx in 0..12_000usize {
            let n = match idx % 6 {
                0 => idx % 8,
                1 => 8 + idx % 9,
                2 => 17 + idx % 84,
                3 => 100 + (idx * 7) % 900,
                4 => 30 + (idx * 13) % 400,
                _ => {
                    if idx % 60 == 5 {
                        1_000 + (idx * 31) % 3_000
                    } else {
                        12 + idx % 40
                    }
                }
            };
            let kind = (idx / 6) % 10;
            let gauss = |uni: &mut dyn FnMut() -> f64| {
                (-2.0 * uni().ln()).sqrt() * (2.0 * std::f64::consts::PI * uni()).cos()
            };
            let mut v: Vec<f64> = Vec::with_capacity(n);
            for i in 0..n {
                let x = match kind {
                    0 => gauss(&mut uni),
                    1 => {
                        if uni() < 0.1 {
                            (5.0 + 45.0 * uni()) * if uni() < 0.7 { 1.0 } else { -1.0 }
                        } else {
                            gauss(&mut uni)
                        }
                    }
                    2 => uni().powf(-1.0 / (0.5 + (idx % 3) as f64 * 0.5)),
                    3 => (std::f64::consts::PI * (uni() - 0.5)).tan(),
                    4 => (2.0 * gauss(&mut uni)).exp() + 0.05 * uni().powf(-1.5),
                    5 => (gauss(&mut uni) * 2.0).round() * 0.25,
                    6 => {
                        let a = (uni() * 40.0).floor();
                        if i % 2 == 0 {
                            a
                        } else {
                            -a
                        }
                    }
                    7 => {
                        if uni() < 0.97 {
                            3.5
                        } else {
                            3.5 + gauss(&mut uni)
                        }
                    }
                    8 => 1e-3 * (-uni().ln()) + 1e6,
                    _ => {
                        if i % 2 == 0 {
                            gauss(&mut uni) * 1e-6 + 7.0
                        } else {
                            gauss(&mut uni) * 1e3
                        }
                    }
                };
                v.push(x);
            }
            if idx % 97 == 0 {
                v = vec![2.25; n];
            }
            let limit = match idx % 11 {
                0 => 0.1,
                1 => 2.0,
                _ => 0.5,
            };
            assert_rcr_bit_equal(
                &v,
                limit,
                &format!("idx {idx} n {n} kind {kind} limit {limit}"),
            );
            let (f, e) = take_verified_decisions();
            per_kind[kind].0 += f;
            per_kind[kind].1 += e;
            vectors += 1;
        }
        let (fast, exact) = per_kind
            .iter()
            .fold((0, 0), |(f, e), &(a, b)| (f + a, e + b));
        eprintln!(
            "{vectors} random vectors bit-equal; phase-0 line-fit rejections: {fast} by the bound, {exact} exact; \
             per kind {per_kind:?}"
        );
        assert!(vectors >= 10_000);
        // Two-sided extreme tails (both extremes past z ≈ 37, where both `d` sit below the normal range: the bimodal
        // kind 9, contaminated kind 1, Cauchy kind 3) leave the side to the exact code by design.
        assert!(
            fast > 5 * exact,
            "the bound decides too little: {fast} vs {exact}"
        );
    }
}
