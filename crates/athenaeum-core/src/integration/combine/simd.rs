//! Vector kernels of [`super::medfit_line`]'s `rofunc` (fast-measurement S1):
//! the element-wise residual build `y − b·i` (with the window counts of 13c
//! 4d), the residual median's selection, and the residual-sign sum — results
//! bit-identical to the scalar code, which stays as the oracle and the
//! fallback ([`Kernel::Scalar`]).
//!
//! Only what cannot change rounding is vectorized:
//!
//! - **Element-wise arithmetic.** Each lane evaluates the SAME f64 expression
//!   as the scalar code, operation by operation (`y − b·i` is one multiply and
//!   one subtract, `y − (a + b·i)` one multiply, one add, one subtract; never a
//!   fused multiply-add), so every residual has the scalar residual's bits.
//!   The rank `i` is an exact f64 integer stepped by an exact add.
//! - **Comparisons and integer counts.** `r < lo` / `r <= hi` / `r > 0` /
//!   `r < 0` are the ordered IEEE comparisons in both forms (false on NaN).
//! - **The sign sum.** The scalar code adds `±i` into an f64 that starts at
//!   `+0.0`. Every partial sum is an integer of magnitude at most
//!   `m·(m − 1)/2`, so below 2^53 each add is exact and the total does not
//!   depend on the order; an exact cancellation in round-to-nearest gives
//!   `+0.0`, and no operand is ever `−0.0`, so the sum is never `−0.0`. The
//!   vector form sums the same `±i` as integers and converts once — the same
//!   value, the same bits. Stacks above [`SIGN_SUM_EXACT_MAX`] (none in
//!   practice) keep the scalar loop.
//! - **The selection** ([`select_order_stats`]): a branch-free three-way
//!   partition (less / equal / greater against a pivot, compressed stores) on
//!   a NaN-free set. Every floating-point reduction keeps its scalar order;
//!   the only one left is the even-`m` lower median's max, which is exact.
//!
//! Why the selection returns the full selection's bits. On NaN-free input
//! the scalar comparator is a total preorder whose only non-trivial
//! equivalence is `−0.0 == +0.0`, so the `k`-th order statistic is one value,
//! and a nonzero value has one bit pattern (`±inf` included): ANY correct
//! selection returns it. A zero order statistic is the exception — which zero
//! a selection lands on depends on the algorithm — so the caller hands every
//! zero result back to the scalar selection, exactly as 13c 4d's window
//! does.
//!
//! The kernel is resolved once per process ([`active`]): NEON on aarch64
//! (baseline), AVX2 on x86_64 (runtime-detected), else scalar; the environment
//! variable [`FORCE_SCALAR_ENV`] forces the scalar path (proofs, timing).

use std::sync::atomic::{AtomicU8, Ordering};

/// Setting this environment variable (to anything but `0` or empty) forces
/// [`Kernel::Scalar`] for the process — the before arm of a timing or an
/// identity proof on one binary.
pub(crate) const FORCE_SCALAR_ENV: &str = "ATHENAEUM_SCALAR_SELECTION";

/// Above this stack size the integer sign sum is not used (the scalar f64 sum
/// stays exact far beyond it; the guard only keeps the proof's premise, every
/// partial sum below 2^53, local and obvious).
pub(crate) const SIGN_SUM_EXACT_MAX: usize = 1 << 24;

/// Proof that this CPU runs the vector kernels: constructed only by
/// [`VectorToken::detect`], so a [`Kernel::Vector`] can never reach an
/// AVX2 instruction on a CPU without AVX2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VectorToken(());

impl VectorToken {
    pub(crate) fn detect() -> Option<Self> {
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        {
            Some(VectorToken(()))
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                Some(VectorToken(()))
            } else {
                None
            }
        }
        #[cfg(not(any(
            all(target_arch = "aarch64", target_feature = "neon"),
            target_arch = "x86_64"
        )))]
        {
            None
        }
    }
}

/// Which `rofunc` code [`super::medfit_line`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kernel {
    /// The scalar code (13c 4d): the oracle and the fallback.
    Scalar,
    /// NEON (aarch64) or AVX2 (x86_64) element-wise kernels and the
    /// branch-free selection.
    Vector(VectorToken),
}

impl Kernel {
    fn name(self) -> &'static str {
        match self {
            Kernel::Scalar => "scalar",
            Kernel::Vector(_) if cfg!(target_arch = "x86_64") => "avx2",
            Kernel::Vector(_) => "neon",
        }
    }

    /// Every kernel this CPU can run, scalar first (the tests run each).
    #[cfg(test)]
    pub(crate) fn available() -> Vec<Kernel> {
        let mut v = vec![Kernel::Scalar];
        v.extend(VectorToken::detect().map(Kernel::Vector));
        v
    }
}

/// 0 = unresolved, 1 = scalar, 2 = vector.
static RESOLVED: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<Option<Kernel>> = const { std::cell::Cell::new(None) };
}

/// Test-only: run the calling thread's `medfit_line` calls on `kernel`
/// (`None` = the resolved one).
#[cfg(test)]
pub(crate) fn force(kernel: Option<Kernel>) {
    FORCED.with(|c| c.set(kernel));
}

/// The process's kernel (one relaxed load after the first call).
#[inline]
pub(crate) fn active() -> Kernel {
    #[cfg(test)]
    if let Some(k) = FORCED.with(std::cell::Cell::get) {
        return k;
    }
    match RESOLVED.load(Ordering::Relaxed) {
        1 => Kernel::Scalar,
        // `resolve` stored 2 only after `detect()` returned a token on this
        // process's CPU, so the proof is still valid; no re-detection.
        2 => Kernel::Vector(VectorToken(())),
        _ => resolve(),
    }
}

#[cold]
fn resolve() -> Kernel {
    let forced = std::env::var_os(FORCE_SCALAR_ENV).is_some_and(|v| !v.is_empty() && v != "0");
    let kernel = match (forced, VectorToken::detect()) {
        (false, Some(t)) => Kernel::Vector(t),
        _ => Kernel::Scalar,
    };
    let code = if kernel == Kernel::Scalar { 1 } else { 2 };
    if RESOLVED.swap(code, Ordering::Relaxed) != code {
        tracing::info!(
            kind = kernel.name(),
            forced,
            "integration selection kernel resolved"
        );
    }
    kernel
}

// ── Element-wise kernels ────────────────────────────────────────────────────

/// `out = [y_i − b·i]`, the scalar residual expression per element.
pub(crate) fn residuals(t: VectorToken, values: &[f64], b: f64, out: &mut Vec<f64>) {
    let m = values.len();
    out.clear();
    out.reserve(m);
    // SAFETY: `out` holds at least `m` elements of capacity; `arch::residuals`
    // writes exactly `out[0..m]` and reads `values[0..m]`; `t` proves the CPU
    // runs the vector instructions.
    unsafe {
        arch::residuals(t, values.as_ptr(), m, b, out.as_mut_ptr());
        out.set_len(m);
    }
}

/// [`residuals`] plus the window counts of 13c 4d: `(below, le)` = how many
/// residuals are `< lo` and `<= hi`.
pub(crate) fn residuals_counted(
    t: VectorToken,
    values: &[f64],
    b: f64,
    lo: f64,
    hi: f64,
    out: &mut Vec<f64>,
) -> (usize, usize) {
    let m = values.len();
    out.clear();
    out.reserve(m);
    // SAFETY: as in `residuals`.
    unsafe {
        let counts = arch::residuals_counted(t, values.as_ptr(), m, b, lo, hi, out.as_mut_ptr());
        out.set_len(m);
        counts
    }
}

/// `Σ_i (+i if y_i − (a + b·i) > 0, −i if < 0)` as the scalar f64 loop
/// computes it, bit for bit (module doc: exact integer sums).
pub(crate) fn sign_sum(t: VectorToken, values: &[f64], a: f64, b: f64) -> f64 {
    let m = values.len();
    if m > SIGN_SUM_EXACT_MAX {
        return sign_sum_scalar(values, a, b);
    }
    // SAFETY: reads `values[0..m]`; `t` proves the CPU runs the vector instructions.
    unsafe { arch::sign_sum(t, values.as_ptr(), m, a, b) as f64 }
}

/// The scalar code's sign sum (13c 4d), verbatim.
pub(crate) fn sign_sum_scalar(values: &[f64], a: f64, b: f64) -> f64 {
    let mut sum = 0.0;
    for (i, &y) in values.iter().enumerate() {
        let resid = y - (a + b * i as f64);
        if resid > 0.0 {
            sum += i as f64;
        } else if resid < 0.0 {
            sum -= i as f64;
        }
    }
    sum
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod arch {
    use super::VectorToken;
    use std::arch::aarch64::*;

    const LANES0: [f64; 2] = [0.0, 1.0];
    const LANES1: [f64; 2] = [2.0, 3.0];
    const RANKS0: [u64; 2] = [0, 1];
    const RANKS1: [u64; 2] = [2, 3];

    #[inline]
    pub(super) unsafe fn residuals(
        _t: VectorToken,
        y: *const f64,
        m: usize,
        b: f64,
        out: *mut f64,
    ) {
        let vb = vdupq_n_f64(b);
        let four = vdupq_n_f64(4.0);
        let mut x0 = vld1q_f64(LANES0.as_ptr());
        let mut x1 = vld1q_f64(LANES1.as_ptr());
        let mut i = 0usize;
        while i + 4 <= m {
            let r0 = vsubq_f64(vld1q_f64(y.add(i)), vmulq_f64(vb, x0));
            let r1 = vsubq_f64(vld1q_f64(y.add(i + 2)), vmulq_f64(vb, x1));
            vst1q_f64(out.add(i), r0);
            vst1q_f64(out.add(i + 2), r1);
            x0 = vaddq_f64(x0, four);
            x1 = vaddq_f64(x1, four);
            i += 4;
        }
        while i < m {
            *out.add(i) = *y.add(i) - b * i as f64;
            i += 1;
        }
    }

    #[inline]
    pub(super) unsafe fn residuals_counted(
        _t: VectorToken,
        y: *const f64,
        m: usize,
        b: f64,
        lo: f64,
        hi: f64,
        out: *mut f64,
    ) -> (usize, usize) {
        let vb = vdupq_n_f64(b);
        let vlo = vdupq_n_f64(lo);
        let vhi = vdupq_n_f64(hi);
        let four = vdupq_n_f64(4.0);
        let mut x0 = vld1q_f64(LANES0.as_ptr());
        let mut x1 = vld1q_f64(LANES1.as_ptr());
        // An all-ones lane is −1: subtracting the mask adds one per hit.
        let (mut below0, mut below1) = (vdupq_n_u64(0), vdupq_n_u64(0));
        let (mut le0, mut le1) = (vdupq_n_u64(0), vdupq_n_u64(0));
        let mut i = 0usize;
        while i + 4 <= m {
            let r0 = vsubq_f64(vld1q_f64(y.add(i)), vmulq_f64(vb, x0));
            let r1 = vsubq_f64(vld1q_f64(y.add(i + 2)), vmulq_f64(vb, x1));
            vst1q_f64(out.add(i), r0);
            vst1q_f64(out.add(i + 2), r1);
            below0 = vsubq_u64(below0, vcltq_f64(r0, vlo));
            below1 = vsubq_u64(below1, vcltq_f64(r1, vlo));
            le0 = vsubq_u64(le0, vcleq_f64(r0, vhi));
            le1 = vsubq_u64(le1, vcleq_f64(r1, vhi));
            x0 = vaddq_f64(x0, four);
            x1 = vaddq_f64(x1, four);
            i += 4;
        }
        let mut below = vaddvq_u64(vaddq_u64(below0, below1)) as usize;
        let mut le = vaddvq_u64(vaddq_u64(le0, le1)) as usize;
        while i < m {
            let r = *y.add(i) - b * i as f64;
            *out.add(i) = r;
            below += (r < lo) as usize;
            le += (r <= hi) as usize;
            i += 1;
        }
        (below, le)
    }

    #[inline]
    pub(super) unsafe fn sign_sum(_t: VectorToken, y: *const f64, m: usize, a: f64, b: f64) -> i64 {
        let va = vdupq_n_f64(a);
        let vb = vdupq_n_f64(b);
        let four = vdupq_n_f64(4.0);
        let four_u = vdupq_n_u64(4);
        let mut x0 = vld1q_f64(LANES0.as_ptr());
        let mut x1 = vld1q_f64(LANES1.as_ptr());
        let mut k0 = vld1q_u64(RANKS0.as_ptr());
        let mut k1 = vld1q_u64(RANKS1.as_ptr());
        let (mut pos0, mut pos1) = (vdupq_n_u64(0), vdupq_n_u64(0));
        let (mut neg0, mut neg1) = (vdupq_n_u64(0), vdupq_n_u64(0));
        let mut i = 0usize;
        while i + 4 <= m {
            let r0 = vsubq_f64(vld1q_f64(y.add(i)), vaddq_f64(va, vmulq_f64(vb, x0)));
            let r1 = vsubq_f64(vld1q_f64(y.add(i + 2)), vaddq_f64(va, vmulq_f64(vb, x1)));
            pos0 = vaddq_u64(pos0, vandq_u64(vcgtzq_f64(r0), k0));
            pos1 = vaddq_u64(pos1, vandq_u64(vcgtzq_f64(r1), k1));
            neg0 = vaddq_u64(neg0, vandq_u64(vcltzq_f64(r0), k0));
            neg1 = vaddq_u64(neg1, vandq_u64(vcltzq_f64(r1), k1));
            x0 = vaddq_f64(x0, four);
            x1 = vaddq_f64(x1, four);
            k0 = vaddq_u64(k0, four_u);
            k1 = vaddq_u64(k1, four_u);
            i += 4;
        }
        let mut sum =
            vaddvq_u64(vaddq_u64(pos0, pos1)) as i64 - vaddvq_u64(vaddq_u64(neg0, neg1)) as i64;
        while i < m {
            let resid = *y.add(i) - (a + b * i as f64);
            if resid > 0.0 {
                sum += i as i64;
            } else if resid < 0.0 {
                sum -= i as i64;
            }
            i += 1;
        }
        sum
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::VectorToken;
    use std::arch::x86_64::*;

    #[inline]
    pub(super) unsafe fn residuals(
        _t: VectorToken,
        y: *const f64,
        m: usize,
        b: f64,
        out: *mut f64,
    ) {
        residuals_avx2(y, m, b, out)
    }

    #[inline]
    pub(super) unsafe fn residuals_counted(
        _t: VectorToken,
        y: *const f64,
        m: usize,
        b: f64,
        lo: f64,
        hi: f64,
        out: *mut f64,
    ) -> (usize, usize) {
        residuals_counted_avx2(y, m, b, lo, hi, out)
    }

    #[inline]
    pub(super) unsafe fn sign_sum(_t: VectorToken, y: *const f64, m: usize, a: f64, b: f64) -> i64 {
        sign_sum_avx2(y, m, a, b)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn hsum_epi64(v: __m256i) -> i64 {
        let mut lanes = [0i64; 4];
        _mm256_storeu_si256(lanes.as_mut_ptr() as *mut __m256i, v);
        lanes[0]
            .wrapping_add(lanes[1])
            .wrapping_add(lanes[2])
            .wrapping_add(lanes[3])
    }

    #[target_feature(enable = "avx2")]
    unsafe fn residuals_avx2(y: *const f64, m: usize, b: f64, out: *mut f64) {
        let vb = _mm256_set1_pd(b);
        let four = _mm256_set1_pd(4.0);
        let mut x = _mm256_setr_pd(0.0, 1.0, 2.0, 3.0);
        let mut i = 0usize;
        while i + 4 <= m {
            let r = _mm256_sub_pd(_mm256_loadu_pd(y.add(i)), _mm256_mul_pd(vb, x));
            _mm256_storeu_pd(out.add(i), r);
            x = _mm256_add_pd(x, four);
            i += 4;
        }
        while i < m {
            *out.add(i) = *y.add(i) - b * i as f64;
            i += 1;
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn residuals_counted_avx2(
        y: *const f64,
        m: usize,
        b: f64,
        lo: f64,
        hi: f64,
        out: *mut f64,
    ) -> (usize, usize) {
        let vb = _mm256_set1_pd(b);
        let vlo = _mm256_set1_pd(lo);
        let vhi = _mm256_set1_pd(hi);
        let four = _mm256_set1_pd(4.0);
        let mut x = _mm256_setr_pd(0.0, 1.0, 2.0, 3.0);
        // An all-ones lane is −1: subtracting the mask adds one per hit.
        let mut below = _mm256_setzero_si256();
        let mut le = _mm256_setzero_si256();
        let mut i = 0usize;
        while i + 4 <= m {
            let r = _mm256_sub_pd(_mm256_loadu_pd(y.add(i)), _mm256_mul_pd(vb, x));
            _mm256_storeu_pd(out.add(i), r);
            below = _mm256_sub_epi64(
                below,
                _mm256_castpd_si256(_mm256_cmp_pd::<_CMP_LT_OQ>(r, vlo)),
            );
            le = _mm256_sub_epi64(le, _mm256_castpd_si256(_mm256_cmp_pd::<_CMP_LE_OQ>(r, vhi)));
            x = _mm256_add_pd(x, four);
            i += 4;
        }
        let mut below = hsum_epi64(below) as usize;
        let mut le = hsum_epi64(le) as usize;
        while i < m {
            let r = *y.add(i) - b * i as f64;
            *out.add(i) = r;
            below += (r < lo) as usize;
            le += (r <= hi) as usize;
            i += 1;
        }
        (below, le)
    }

    #[target_feature(enable = "avx2")]
    unsafe fn sign_sum_avx2(y: *const f64, m: usize, a: f64, b: f64) -> i64 {
        let va = _mm256_set1_pd(a);
        let vb = _mm256_set1_pd(b);
        let zero = _mm256_setzero_pd();
        let four = _mm256_set1_pd(4.0);
        let four_i = _mm256_set1_epi64x(4);
        let mut x = _mm256_setr_pd(0.0, 1.0, 2.0, 3.0);
        let mut k = _mm256_setr_epi64x(0, 1, 2, 3);
        let mut pos = _mm256_setzero_si256();
        let mut neg = _mm256_setzero_si256();
        let mut i = 0usize;
        while i + 4 <= m {
            let r = _mm256_sub_pd(
                _mm256_loadu_pd(y.add(i)),
                _mm256_add_pd(va, _mm256_mul_pd(vb, x)),
            );
            let gt = _mm256_castpd_si256(_mm256_cmp_pd::<_CMP_GT_OQ>(r, zero));
            let lt = _mm256_castpd_si256(_mm256_cmp_pd::<_CMP_LT_OQ>(r, zero));
            pos = _mm256_add_epi64(pos, _mm256_and_si256(gt, k));
            neg = _mm256_add_epi64(neg, _mm256_and_si256(lt, k));
            x = _mm256_add_pd(x, four);
            k = _mm256_add_epi64(k, four_i);
            i += 4;
        }
        let mut sum = hsum_epi64(pos) - hsum_epi64(neg);
        while i < m {
            let resid = *y.add(i) - (a + b * i as f64);
            if resid > 0.0 {
                sum += i as i64;
            } else if resid < 0.0 {
                sum -= i as i64;
            }
            i += 1;
        }
        sum
    }
}

/// No vector unit: a [`VectorToken`] cannot exist, so these are never
/// reached; they keep the module compiling with the same expressions.
#[cfg(not(any(
    all(target_arch = "aarch64", target_feature = "neon"),
    target_arch = "x86_64"
)))]
mod arch {
    use super::VectorToken;

    pub(super) unsafe fn residuals(
        _t: VectorToken,
        y: *const f64,
        m: usize,
        b: f64,
        out: *mut f64,
    ) {
        for i in 0..m {
            *out.add(i) = *y.add(i) - b * i as f64;
        }
    }

    pub(super) unsafe fn residuals_counted(
        _t: VectorToken,
        y: *const f64,
        m: usize,
        b: f64,
        lo: f64,
        hi: f64,
        out: *mut f64,
    ) -> (usize, usize) {
        let (mut below, mut le) = (0, 0);
        for i in 0..m {
            let r = *y.add(i) - b * i as f64;
            *out.add(i) = r;
            below += (r < lo) as usize;
            le += (r <= hi) as usize;
        }
        (below, le)
    }

    pub(super) unsafe fn sign_sum(_t: VectorToken, y: *const f64, m: usize, a: f64, b: f64) -> i64 {
        let mut sum = 0i64;
        for i in 0..m {
            let resid = *y.add(i) - (a + b * i as f64);
            if resid > 0.0 {
                sum += i as i64;
            } else if resid < 0.0 {
                sum -= i as i64;
            }
        }
        sum
    }
}

// ── Selection ───────────────────────────────────────────────────────────────

/// [`select_order_stats`]' ping-pong buffers (grown, never shrunk).
#[derive(Default)]
pub(crate) struct SelectScratch {
    a: Vec<f64>,
    b: Vec<f64>,
}

/// Three-way partition of `src` around `p` into `dst` (same length): the
/// elements `< p` at the front in source order, the elements `> p` at the
/// back in reverse source order; the elements equal to `p` are counted by
/// the caller (`len − less − greater`), not stored. Branch-free compressed
/// stores: every element is written at both open ends and each end advances
/// by its comparison bit. Returns `(less, greater)`.
#[inline(always)]
fn partition3(src: &[f64], p: f64, dst: &mut [f64]) -> (usize, usize) {
    let len = src.len();
    assert_eq!(dst.len(), len);
    if len == 0 {
        return (0, 0);
    }
    let top = len - 1;
    let d = dst.as_mut_ptr();
    let (mut nl, mut ng) = (0usize, 0usize);
    for &x in src {
        // SAFETY: before element `j` is stored, `nl + ng <= j <= top`, so
        // `nl <= top` and `top − ng >= 0`: both stores are inside `dst`, and
        // neither overwrites a kept element (each lands on its end's next
        // free slot; the two coincide only on the last element, which both
        // then hold).
        unsafe {
            *d.add(nl) = x;
            *d.add(top - ng) = x;
        }
        nl += (x < p) as usize;
        ng += (x > p) as usize;
    }
    (nl, ng)
}

/// A pivot drawn from `v`'s own elements (so it lands in the equal class and
/// every round shrinks the set): the median of the first, middle and last.
/// (Measured: picking the min / median / max of the three by the target
/// rank instead costs 1.03–1.10× per `medfit_line` call.)
#[inline(always)]
fn pivot(v: &[f64]) -> f64 {
    let n = v.len();
    if n < 3 {
        return v[0];
    }
    let (a, b, c) = (v[0], v[n / 2], v[n - 1]);
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if c < lo {
        lo
    } else if c > hi {
        hi
    } else {
        c
    }
}

/// The `k`-th order statistic (0-based, ascending) of a NaN-free `src` and,
/// when `need_lower`, the `(k − 1)`-th — `(lower, kth)`, `lower == kth` when
/// not asked for. `src` is not modified. Quickselect on [`partition3`]:
/// elements equal to the pivot leave the set at once, so ties cost nothing.
/// A returned zero may carry either sign (module doc): the caller treats a
/// zero as "not decided here".
///
/// The `(k − 1)`-th: in the round that finds the `k`-th inside the pivot's
/// equal class, it is the pivot again when the class holds rank `k − 1` too,
/// else the max of that round's less part, else (empty less part) the max of
/// everything already discarded below — the last pivot the search moved
/// right of, which is that discarded set's max.
///
/// Worst case: a median-of-3 quickselect is O(n²) on adversarial order, so
/// the rounds are capped at `2·⌈log₂ n⌉ + 8` and the call returns `None` —
/// the caller then runs the scalar selection, which yields the same order
/// statistic (one bit pattern per nonzero value on NaN-free input).
pub(crate) fn select_order_stats(
    src: &[f64],
    k: usize,
    need_lower: bool,
    s: &mut SelectScratch,
) -> Option<(f64, f64)> {
    let rounds = 2 * (usize::BITS - src.len().leading_zeros()) as usize + 8;
    select_order_stats_capped(src, k, need_lower, s, rounds)
}

/// [`select_order_stats`] with an explicit round cap (`None` past it).
pub(crate) fn select_order_stats_capped(
    src: &[f64],
    k: usize,
    need_lower: bool,
    s: &mut SelectScratch,
    max_rounds: usize,
) -> Option<(f64, f64)> {
    #[derive(Clone, Copy)]
    enum Loc {
        Src,
        A,
        B,
    }
    let n = src.len();
    assert!(
        k < n && (!need_lower || k >= 1),
        "select_order_stats: rank {k} of {n}"
    );
    if s.a.len() < n {
        s.a.resize(n, 0.0);
    }
    if s.b.len() < n {
        s.b.resize(n, 0.0);
    }
    let (mut loc, mut start, mut len, mut k) = (Loc::Src, 0usize, n, k);
    let mut left_max = f64::NEG_INFINITY;
    let mut rounds = 0usize;
    loop {
        if rounds >= max_rounds {
            return None;
        }
        rounds += 1;
        let (from, to, next): (&[f64], &mut [f64], Loc) = match loc {
            Loc::Src => (&src[start..start + len], &mut s.a[..len], Loc::A),
            Loc::A => (&s.a[start..start + len], &mut s.b[..len], Loc::B),
            Loc::B => (&s.b[start..start + len], &mut s.a[..len], Loc::A),
        };
        let p = pivot(from);
        let (nl, ng) = partition3(from, p, to);
        let ne = len - nl - ng;
        if k < nl {
            (loc, start, len) = (next, 0, nl);
        } else if k < nl + ne {
            let lower = if !need_lower || k > nl {
                p
            } else if nl > 0 {
                to[..nl].iter().copied().fold(f64::NEG_INFINITY, f64::max)
            } else {
                left_max
            };
            return Some((lower, p));
        } else {
            left_max = p;
            k -= nl + ne;
            (loc, start, len) = (next, len - ng, ng);
        }
    }
}
