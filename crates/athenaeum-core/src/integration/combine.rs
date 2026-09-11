//! Per-pixel robust integration recipes — PixInsight's two-axis model
//! (Combination × Rejection). Spec:
//! `docs/superpowers/specs/2026-07-06-master-integration-pi-model-design.md`.
//!
//! An [`IntegrationRecipe`] pairs a [`Combination`] (Average or Median of the
//! surviving samples) with a [`Rejection`] algorithm that decides which
//! samples survive first. Rejection runs per pixel stack; the combination
//! then applies to the survivors — every rejection composes with either
//! combination (the reference semantics).
//!
//! The pre-2026-07-06 flat `CombineMethod` enum is retained only as a private,
//! deserialize-only [`LegacyCombineMethod`] so old `recipe_json` blobs still
//! parse (spec §3). Its equivalences: `Mean` = Average+None, `Median` =
//! Median+None, `WinsorizedSigmaClip` = Average+WinsorizedSigma,
//! `PercentileClip` = Average+PercentileClip — pinned bit-for-bit by the tests.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;

/// A stack element the rejection routines can order and read: the plain
/// sample for master builds, a `(value, frame index)` pair for the
/// stacking engine, which needs to know WHICH frames survived.
pub trait Sample: Copy {
    fn value(self) -> f32;
}

impl Sample for f32 {
    #[inline]
    fn value(self) -> f32 {
        self
    }
}

impl Sample for (f32, u16) {
    #[inline]
    fn value(self) -> f32 {
        self.0
    }
}

/// Fixed upper bound on the rejection refit loop (spec §1) so an adversarial
/// pixel stack can never spin unbounded on this hot path.
const MAX_REJECTION_ITERS: usize = 20;

/// A master-integration recipe: reject first, then combine the survivors.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationRecipe {
    pub combination: Combination,
    pub rejection: Rejection,
}

impl IntegrationRecipe {
    /// Average (mean) of the survivors after `rejection`.
    pub const fn average(rejection: Rejection) -> Self {
        Self { combination: Combination::Average, rejection }
    }

    /// Median of the survivors after `rejection`.
    pub const fn median(rejection: Rejection) -> Self {
        Self { combination: Combination::Median, rejection }
    }

    /// Human summary — "Average | Winsorized sigma (3.0/3.0)" style (spec §4).
    /// Printable-ASCII only: this string is written into the ATH_REJ FITS
    /// card, whose values are restricted to 0x20–0x7E.
    pub fn describe(&self) -> String {
        format!("{} | {}", self.combination.label(), self.rejection.label())
    }
}

/// How the surviving samples are collapsed into the output pixel. `Average`
/// is PI's name for our historical `Mean`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Combination {
    Average,
    Median,
}

impl Combination {
    fn label(self) -> &'static str {
        match self {
            Combination::Average => "Average",
            Combination::Median => "Median",
        }
    }
}

/// Which samples get excluded before combination. Same internal-tag serde
/// shape (`tag = "method"`) as the legacy `CombineMethod` it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum Rejection {
    /// Keep every sample.
    None,
    /// PixInsight-style percentile clipping around the median m: reject x when
    /// (m - x)/|m| > low or (x - m)/|m| > high. Deviations normalized by |m|
    /// so thresholds are sign-agnostic.
    PercentileClip { low: f64, high: f64 },
    /// Plain (non-winsorized) sigma clip: iteratively reject samples outside
    /// [m − sigma_low·σ, m + sigma_high·σ] of the current survivor set until
    /// stable. Zero-dispersion sets converge immediately with no rejection.
    SigmaClip { sigma_low: f64, sigma_high: f64 },
    /// Huber-style winsorized sigma clip (unchanged from the legacy master
    /// recipe): a winsorized location/scale estimate, then reject original
    /// samples outside [m − sigma_low·s, m + sigma_high·s].
    WinsorizedSigma { sigma_low: f64, sigma_high: f64 },
    /// Minimum-absolute-deviation ("robust") line fit over (rank, value);
    /// reject samples whose residual falls outside [−sigma_low·d,
    /// +sigma_high·d] where d is [`LINEAR_FIT_SIGMA_SCALE`] times twice the
    /// mean absolute deviation of the residuals from that line; refit and
    /// repeat until stable. The reference's recommended choice for larger
    /// sets with drifting illumination.
    LinearFitClip { sigma_low: f64, sigma_high: f64 },
}

/// Format a rejection parameter for a describe/label string (spec §4). Integer
/// values render with a trailing `.0` (`3.0` → `"3.0"`, matching the spec's
/// `(3.0/3.0)` style) while fractional values keep their natural precision
/// (`0.02` → `"0.02"`) — a flat `{:.1}` would truncate `PercentileClip`'s small
/// thresholds. Must stay byte-identical to `fmtParam` in `CreateMasterDialog.tsx`.
fn fmt_param(x: f64) -> String {
    if x.is_finite() && x == x.trunc() {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

impl Rejection {
    fn label(self) -> String {
        match self {
            Rejection::None => "no rejection".to_string(),
            Rejection::PercentileClip { low, high } => {
                format!("Percentile clip ({}/{})", fmt_param(low), fmt_param(high))
            }
            Rejection::SigmaClip { sigma_low, sigma_high } => {
                format!("Sigma clip ({}/{})", fmt_param(sigma_low), fmt_param(sigma_high))
            }
            Rejection::WinsorizedSigma { sigma_low, sigma_high } => {
                format!("Winsorized sigma ({}/{})", fmt_param(sigma_low), fmt_param(sigma_high))
            }
            Rejection::LinearFitClip { sigma_low, sigma_high } => {
                format!("Linear fit clip ({}/{})", fmt_param(sigma_low), fmt_param(sigma_high))
            }
        }
    }
}

// ── Numeric helpers ─────────────────────────────────────────────────────────

fn mean_f64<T: Sample>(v: &[T]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().map(|s| s.value() as f64).sum::<f64>() / v.len() as f64
}

fn mean<T: Sample>(v: &[T]) -> f32 {
    mean_f64(v) as f32
}

fn median_sorted<T: Sample>(v: &[T]) -> f32 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2].value()
    } else {
        (v[n / 2 - 1].value() + v[n / 2].value()) / 2.0
    }
}

fn stddev<T: Sample>(v: &[T], m: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let var = v
        .iter()
        .map(|s| {
            let d = s.value() as f64 - m;
            d * d
        })
        .sum::<f64>()
        / (v.len() - 1) as f64;
    var.sqrt()
}

fn sort_asc<T: Sample>(v: &mut [T]) {
    // Stable by contract: the weighted path's summation order follows this
    // ordering; sort_unstable_by would permute tied (value, index) pairs and
    // change the f64 summation order.
    v.sort_by(|a, b| a.value().partial_cmp(&b.value()).unwrap_or(std::cmp::Ordering::Equal));
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Combine one pixel column: `values` holds the same pixel from N frames
/// (already normalized/pre-calibrated by the caller). Returns
/// `(value, rejected_count)`.
///
/// `combine_pixel` may reorder `values` in place (sorting / survivor
/// compaction) — callers pass scratch copies.
pub fn combine_pixel(values: &mut [f32], recipe: IntegrationRecipe) -> (f32, usize) {
    let n = values.len();
    if n == 0 {
        return (0.0, 0);
    }

    // Phase 1 — rejection compacts survivors into the prefix `values[..kept]`.
    // `sorted` reports whether that prefix is left in ascending order (so the
    // median path can skip a re-sort).
    let (kept, sorted) = apply_rejection(values, recipe.rejection);

    if kept == 0 {
        // Nothing survived — fall back to the median of the full stack (this
        // reproduces the legacy winsorized all-rejected guard).
        if !sorted {
            sort_asc(values);
        }
        return (median_sorted(values), n);
    }

    // Phase 2 — combine the survivors.
    let survivors = &mut values[..kept];
    let val = match recipe.combination {
        Combination::Average => mean(survivors),
        Combination::Median => {
            if !sorted {
                sort_asc(survivors);
            }
            median_sorted(survivors)
        }
    };
    (val, n - kept)
}

// ── Survivor masks ───────────────────────────────────────────────────────────

/// Words a survivor mask needs for `n` frames.
#[inline]
pub fn mask_words(n: usize) -> usize {
    n.div_ceil(64)
}

#[inline]
pub fn mask_clear(mask: &mut [u64]) {
    mask.iter_mut().for_each(|w| *w = 0);
}

#[inline]
pub fn mask_set(mask: &mut [u64], i: usize) {
    mask[i / 64] |= 1u64 << (i % 64);
}

#[inline]
pub fn mask_get(mask: &[u64], i: usize) -> bool {
    mask[i / 64] & (1u64 << (i % 64)) != 0
}

/// Weighted combination of one pixel column (spec §6.2, math reference §3.2
/// steps 3, 5, 6).
///
/// `work[k] = (rejection-normalized value, frame index)` for every frame
/// with a usable sample (the caller has already dropped missing and
/// range-rejected samples); it is reordered in place. `out_values[i]` is
/// frame `i`'s OUTPUT-normalized value and `weights[i]` its weight, both
/// indexed by frame — only the indices present in `work` are read. The
/// rejection runs on `work`; the result is the weighted mean of the
/// survivors' `out_values` (samples with `out_values == 0` or `weight <= 0`
/// are skipped, math reference §3.6) or their median (weights ignored).
/// Every survivor's bit is set in `mask` (the caller clears it first) and
/// the rejected count is returned. All rejected → the median of every
/// `out_values` present in `work`, no bit set.
///
/// Contracts: `out_values` and `weights` are indexed by the FRAME index that
/// rides in `work` (`n_frames` entries each — an index beyond them panics);
/// `mask` holds `mask_words(n_frames)` words — sized by the frame count, not
/// by `work.len()`, which is the subset with usable samples — and must be
/// cleared by the caller before every call (a stale bit is a phantom
/// survivor nothing detects); `scratch` is reused across calls and never
/// read. A survivor's bit is set whether or not the sample contributed to
/// the average (a zero-valued or zero-weighted survivor is masked as a
/// survivor but skipped by the mean) — rejection maps count rejections, not
/// contributions.
pub fn combine_pixel_weighted(
    work: &mut [(f32, u16)],
    out_values: &[f32],
    weights: &[f32],
    recipe: IntegrationRecipe,
    mask: &mut [u64],
    scratch: &mut Vec<f32>,
) -> (f32, usize) {
    debug_assert_eq!(out_values.len(), weights.len());
    debug_assert!(work.iter().all(|&(_, i)| (i as usize) < out_values.len()));
    debug_assert!(
        mask.iter().all(|&w| w == 0),
        "combine_pixel_weighted: mask must be cleared per pixel"
    );
    let n = work.len();
    if n == 0 {
        return (0.0, 0);
    }
    let (kept, _sorted) = apply_rejection(work, recipe.rejection);
    if kept == 0 {
        scratch.clear();
        scratch.extend(work.iter().map(|&(_, i)| out_values[i as usize]));
        sort_asc(scratch);
        return (median_sorted(scratch), n);
    }
    for &(_, i) in &work[..kept] {
        mask_set(mask, i as usize);
    }
    let value = match recipe.combination {
        Combination::Average => {
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for &(_, i) in &work[..kept] {
                let x = out_values[i as usize];
                let w = weights[i as usize];
                if x != 0.0 && w > 0.0 {
                    num += x as f64 * w as f64;
                    den += w as f64;
                }
            }
            if den > 0.0 {
                (num / den) as f32
            } else {
                // Every survivor was a zero-valued or zero-weighted sample:
                // the plain mean of the survivors, as the unweighted path.
                scratch.clear();
                scratch.extend(work[..kept].iter().map(|&(_, i)| out_values[i as usize]));
                mean(scratch)
            }
        }
        Combination::Median => {
            scratch.clear();
            scratch.extend(work[..kept].iter().map(|&(_, i)| out_values[i as usize]));
            sort_asc(scratch);
            median_sorted(scratch)
        }
    };
    (value, n - kept)
}

/// Runs the chosen rejection algorithm in place, returning
/// `(surviving_count, prefix_is_sorted_ascending)`.
fn apply_rejection<T: Sample>(values: &mut [T], rejection: Rejection) -> (usize, bool) {
    let n = values.len();
    match rejection {
        Rejection::None => (n, false),
        Rejection::PercentileClip { low, high } => reject_percentile(values, low, high),
        Rejection::SigmaClip { sigma_low, sigma_high } => {
            reject_sigma_clip(values, sigma_low, sigma_high)
        }
        Rejection::WinsorizedSigma { sigma_low, sigma_high } => {
            reject_winsorized(values, sigma_low, sigma_high)
        }
        Rejection::LinearFitClip { sigma_low, sigma_high } => {
            reject_linear_fit(values, sigma_low, sigma_high)
        }
    }
}

// ── Rejection algorithms (in place, allocation-free except winsorized) ──────

fn reject_percentile<T: Sample>(values: &mut [T], low: f64, high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    sort_asc(values);
    let m = median_sorted(values) as f64;
    if m.abs() <= f64::EPSILON {
        // Can't normalize deviations by |m| — keep everything.
        return (n, true);
    }
    // Stable compaction: survivors keep ascending order, so the prefix stays
    // sorted (w <= r throughout, so the write never clobbers an unread slot).
    let mut w = 0usize;
    for r in 0..n {
        let xf = values[r].value() as f64;
        let dev = (xf - m) / m.abs();
        let reject = (dev < 0.0 && -dev > low) || (dev > 0.0 && dev > high);
        if !reject {
            values[w] = values[r];
            w += 1;
        }
    }
    (w, true)
}

fn reject_sigma_clip<T: Sample>(values: &mut [T], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    let mut kept = n;
    for _ in 0..MAX_REJECTION_ITERS {
        let slice = &values[..kept];
        let m = mean_f64(slice);
        let s = stddev(slice, m);
        if s <= f64::EPSILON {
            break; // zero dispersion → no (further) rejection
        }
        let lo = m - sigma_low * s;
        let hi = m + sigma_high * s;
        let mut w = 0usize;
        for r in 0..kept {
            let xf = values[r].value() as f64;
            if xf >= lo && xf <= hi {
                values[w] = values[r];
                w += 1;
            }
        }
        if w == kept {
            break; // converged
        }
        if w == 0 {
            // Everything in the current valid prefix was rejected. Do NOT set
            // kept = 0: combine_pixel's all-rejected fallback reads the FULL
            // values[..n], whose tail was overwritten by an earlier iteration's
            // in-place compaction. Break instead, keeping the previous
            // iteration's intact survivor prefix (>= 2, or the initial n) for
            // the combination — never fabricate from corrupted memory.
            break;
        }
        kept = w;
        if kept < 2 {
            break; // stddev undefined below 2 survivors
        }
    }
    (kept, false)
}

fn reject_winsorized<T: Sample>(values: &mut [T], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    sort_asc(values);
    // 1) Winsorized estimate of location/scale (Huber-style iteration): clamp
    //    the working copy at m±1.5σ, recompute, repeat to 0.5% change. This
    //    block is byte-identical to the legacy `WinsorizedSigmaClip` estimator
    //    so Average+WinsorizedSigma reproduces the old master exactly.
    let mut work: Vec<f64> = values.iter().map(|s| s.value() as f64).collect();
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
            * (work.iter().map(|x| (x - new_m) * (x - new_m)).sum::<f64>() / (n - 1) as f64).sqrt();
        let converged = (new_s - s).abs() <= 0.005 * s.abs();
        m = new_m;
        s = new_s;
        if converged {
            break;
        }
    }
    // 2) Keep original samples inside [m − σ_low·s, m + σ_high·s]. Survivors
    //    stay in the sorted order the summation relied on historically.
    let (lo, hi) = (m - sigma_low * s, m + sigma_high * s);
    let mut w = 0usize;
    for r in 0..n {
        let xf = values[r].value() as f64;
        if xf >= lo && xf <= hi {
            values[w] = values[r];
            w += 1;
        }
    }
    (w, true)
}

/// Dispersion-scale knob for [`Rejection::LinearFitClip`]: the rejection band
/// half-width is `sigma_low`/`sigma_high` multiples of
/// `s = LINEAR_FIT_SIGMA_SCALE * 2 * adev` (`adev` = mean absolute deviation
/// of the residuals from the fitted line — spec §6.3, math reference §3.4).
/// Empirical, calibrated by the M4a acceptance run (ruling R-M4a-4): with the
/// least-squares line the M2 acceptance run measured 0.83 %/0.74 % rejected
/// at the Auto 5.0/3.5 thresholds on the LDN 1272 set, against the reference
/// implementation's 2.5–2.8 %. Switching to the minimum-absolute-deviation
/// line (below) closes most of that gap on its own; this constant is the one
/// knob left to tune the remainder to the 2.3–3.3 % acceptance target — this
/// commit leaves it at the neutral `1.0`, so a future re-tune is a one-line
/// diff away, not a re-derivation.
pub const LINEAR_FIT_SIGMA_SCALE: f64 = 1.0;

thread_local! {
    // `reject_linear_fit`'s per-outer-iteration f64 copy of the current
    // survivor values, and `medfit_line`'s own residual scratch (used to
    // take the median of `y − b·x` at each trial slope during
    // bracketing/bisection). Both cleared and reused per call — this
    // rejection runs inside the per-pixel band loop, so a fresh `Vec` per
    // pixel is not acceptable.
    static LINEAR_FIT_VALUE_SCRATCH: RefCell<Vec<f64>> = RefCell::new(Vec::new());
    static MEDFIT_RESIDUAL_SCRATCH: RefCell<Vec<f64>> = RefCell::new(Vec::new());
}

#[inline]
fn robust_sign(x: f64) -> f64 {
    if x >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// Minimum-absolute-deviation line `y = a + b·i` over `values[i]` (math
/// reference §3.4): the classic median/bisection method. Seeds the search
/// from `warm_start_b` when given (the caller's previous iteration's
/// converged slope — fix round 1, ruling R-M4a-16: after the first
/// iteration the bracket collapses to a handful of evaluations instead of
/// walking out from the least-squares slope every time) or the
/// least-squares slope otherwise; the bracket half-width always uses the
/// least-squares slope standard error `σ_b` of the CURRENT survivors,
/// regardless of which slope seeded `b1`. Walks the slope that zeroes the
/// residual-sign functional `f(b) = Σ x_i · sgn(y_i − median(y − b·x) −
/// b·x_i)` by bracketing a sign change and bisecting to it; the intercept is
/// the median of the residuals at the converged slope. A single extreme
/// sample drags a least-squares line toward itself, shrinking its own
/// residual and starving the whole stack's rejection — the median-based
/// line does not move for it.
///
/// Two fast paths (ruling R-M4a-16, finding 3): `f` is an integer-valued
/// step function, so an exact root `f(b) == 0.0` is common (measured 2.8 %
/// of n = 20 stacks) — the widening/bisection would otherwise walk AWAY
/// from it (its `fb * f1 >= 0.0` tie-break shrinks toward the wrong side of
/// an exact zero), returning a worse line than the seed. Both the seed and
/// every bisection evaluation check for this and return immediately when
/// found. The final answer is the last evaluated bisection point — no
/// extra `f` evaluation at the converged midpoint.
///
/// Never panics on degenerate input: fewer than 2 samples, a zero-dispersion
/// (perfect-line) fit, or a sign functional that cannot be bracketed within
/// 32 widenings all fall back to the least-squares line.
pub(crate) fn medfit_line(values: &[f64], warm_start_b: Option<f64>) -> (f64, f64) {
    let n = values.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    if n == 1 {
        return (values[0], 0.0);
    }
    let nf = n as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for (i, &y) in values.iter().enumerate() {
        let x = i as f64;
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    // del = n·Σ(x − x̄)², strictly positive for n >= 2 distinct ranks.
    let del = nf * sxx - sx * sx;
    if del.abs() <= f64::EPSILON {
        return (sy / nf, 0.0); // unreachable for distinct ranks; guard only
    }
    let b_ls = (nf * sxy - sx * sy) / del;
    let a_ls = (sy - b_ls * sx) / nf;
    let mut chisq = 0.0;
    for (i, &y) in values.iter().enumerate() {
        let resid = y - (a_ls + b_ls * i as f64);
        chisq += resid * resid;
    }
    let sigma_b = (chisq / del).sqrt();
    if !(sigma_b > 0.0) {
        // Zero residual dispersion (an exact line): the LS line already IS
        // the robust line, and the bracket below would be degenerate.
        return (a_ls, b_ls);
    }

    MEDFIT_RESIDUAL_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        // O(n) selection instead of an O(n log n) sort per bracket
        // evaluation (ruling R-M4a-16, finding 1) — this is the DEFAULT
        // rejection path for every n >= 20 stack (`RejectionChoice::Auto`),
        // ~26 M calls per plane. A single cold `medfit_line` call (no warm
        // start) evaluates this up to ~14-90 times (widening + bisection);
        // the warm start (b) below only shrinks evaluation count ACROSS
        // `reject_linear_fit`'s outer iterations, not within one call.
        let mut rofunc = |b: f64| -> (f64, f64) {
            scratch.clear();
            scratch.extend(values.iter().enumerate().map(|(i, &y)| y - b * i as f64));
            let m = scratch.len();
            let cmp = |p: &f64, q: &f64| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal);
            let a = if m % 2 == 1 {
                *scratch.select_nth_unstable_by(m / 2, cmp).1
            } else {
                let (lo, hi, _) = scratch.select_nth_unstable_by(m / 2, cmp);
                0.5 * (lo.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + *hi)
            };
            let mut sum = 0.0;
            for (i, &y) in values.iter().enumerate() {
                let resid = y - (a + b * i as f64);
                if resid > 0.0 {
                    sum += i as f64;
                } else if resid < 0.0 {
                    sum -= i as f64;
                }
            }
            (sum, a)
        };

        let mut b1 = warm_start_b.unwrap_or(b_ls);
        let (mut f1, a1) = rofunc(b1);
        if f1 == 0.0 {
            // Exact root at the seed (finding 3): return immediately — any
            // further widening/bisection would only walk away from it.
            return (a1, b1);
        }
        let mut b2 = b1 + 3.0 * sigma_b * robust_sign(f1);
        let (mut f2, _) = rofunc(b2);

        let mut widenings = 0u32;
        while f1 * f2 > 0.0 && widenings < 32 {
            b2 = b1 + 2.0 * (b2 - b1);
            f2 = rofunc(b2).0;
            widenings += 1;
        }
        if f1 * f2 > 0.0 {
            // Could not bracket a sign change of f — degenerate residual
            // landscape; the least-squares line is the least-bad fallback.
            return (a_ls, b_ls);
        }

        let tol = 1e-3 * sigma_b;
        let mut iters = 0u32;
        // Last evaluated bisection point — returned as-is at the end
        // instead of paying for one more `rofunc` at the converged midpoint
        // (finding 1c). Seeded from the seed evaluation so a bracket that
        // exits the loop on its very first `bb == b1 || bb == b2` guard
        // (no floating-point room left) still returns a real evaluated
        // point rather than a fabricated one.
        let mut last_a = a1;
        let mut last_b = b1;
        while (b2 - b1).abs() >= tol && iters < 60 {
            let bb = 0.5 * (b1 + b2);
            if bb == b1 || bb == b2 {
                break; // no floating-point progress left
            }
            let (fb, ab) = rofunc(bb);
            if fb == 0.0 {
                // Exact root found mid-bisection (finding 3): stop here —
                // continuing would shrink the interval away from it.
                return (ab, bb);
            }
            last_a = ab;
            last_b = bb;
            if fb * f1 >= 0.0 {
                b1 = bb;
                f1 = fb;
            } else {
                b2 = bb;
            }
            iters += 1;
        }
        (last_a, last_b)
    })
}

fn reject_linear_fit<T: Sample>(values: &mut [T], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    sort_asc(values);
    let mut kept = n;
    // Warm start (ruling R-M4a-16, finding 1b): after iteration 1 the
    // survivor set has barely moved, so its converged slope is a much
    // better bracket seed for the NEXT iteration than walking out from the
    // least-squares slope again every time — the bracket collapses to a
    // handful of evaluations instead of a fresh widening/bisection search.
    // `None` on the first iteration seeds from the least-squares slope, as
    // before.
    let mut warm_start_b: Option<f64> = None;
    LINEAR_FIT_VALUE_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        for _ in 0..MAX_REJECTION_ITERS {
            let k = kept;
            if k < 2 {
                break;
            }
            // Minimum-absolute-deviation line y = a + b·i over (rank i,
            // value) for i in 0..k — replaces the least-squares line (M4
            // ruling R-M4a-4): a single extreme sample used to drag the LS
            // line toward itself, shrinking its own residual and
            // under-rejecting real stacks (M2 measured 0.83 %/0.74 %
            // rejected against the reference's 2.5–2.8 % at the same
            // 5.0/3.5 thresholds). The median-based line does not move for
            // outliers.
            let kf = k as f64;
            scratch.clear();
            scratch.extend(values[..k].iter().map(|s| s.value() as f64));
            let (a, b) = medfit_line(&scratch, warm_start_b);
            warm_start_b = Some(b);

            let mut abs_sum = 0.0;
            let mut sabs_y = 0.0;
            for (i, &y) in scratch.iter().enumerate() {
                let resid = y - (a + b * i as f64);
                abs_sum += resid.abs();
                sabs_y += y.abs();
            }
            let adev = abs_sum / kf;
            // Dispersion `s = LINEAR_FIT_SIGMA_SCALE · 2·adev`: doubled so the
            // thresholds compare with sigma clipping (spec §6.3, math
            // reference §3.4). The reference's additional slope term
            // `sqrt(1 + b²)` is still omitted — it is inert on [0, 1] input
            // and dimensionally wrong on the ADU-scale stacks the master
            // builder feeds; `LINEAR_FIT_SIGMA_SCALE` is the one knob the
            // M4a acceptance run may turn instead (see its doc comment).
            let s = LINEAR_FIT_SIGMA_SCALE * 2.0 * adev;
            // Scale-relative zero-dispersion guard: on a perfectly (or near-)
            // linear stack the residuals are floating-point noise, not signal —
            // treat that as "no rejection" so a clean ramp is never eaten. Real
            // dispersion (read noise, drift) is orders of magnitude above this.
            let scale = (sabs_y / kf).max(1.0);
            if adev <= 1e-9 * scale {
                break;
            }
            let lo = -sigma_low * s;
            let hi = sigma_high * s;
            let mut w = 0usize;
            for i in 0..k {
                let resid = values[i].value() as f64 - (a + b * i as f64);
                if resid >= lo && resid <= hi {
                    values[w] = values[i];
                    w += 1;
                }
            }
            if w == kept {
                break; // stable
            }
            if w == 0 {
                // See reject_sigma_clip: keep the last valid survivor prefix rather
                // than let combine_pixel fall back over the corrupted-tail full
                // stack. kept holds the previous survivors (>= 2, or the initial n).
                break;
            }
            kept = w;
        }
    });
    (kept, true)
}

// ── Legacy recipe_json compatibility (spec §3) ──────────────────────────────

/// Legacy flat combine enum (pre-2026-07-06). Deserialize-only and private —
/// its sole purpose is to map old `master_provenance.recipe_json` blobs onto
/// the two-axis [`IntegrationRecipe`]. Never serialized, never public.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method")]
enum LegacyCombineMethod {
    Mean,
    Median,
    WinsorizedSigmaClip { sigma_low: f64, sigma_high: f64 },
    PercentileClip { low: f64, high: f64 },
}

impl From<LegacyCombineMethod> for IntegrationRecipe {
    fn from(m: LegacyCombineMethod) -> Self {
        match m {
            LegacyCombineMethod::Mean => IntegrationRecipe::average(Rejection::None),
            LegacyCombineMethod::Median => IntegrationRecipe::median(Rejection::None),
            LegacyCombineMethod::WinsorizedSigmaClip { sigma_low, sigma_high } => {
                IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low, sigma_high })
            }
            LegacyCombineMethod::PercentileClip { low, high } => {
                IntegrationRecipe::average(Rejection::PercentileClip { low, high })
            }
        }
    }
}

/// Parse a `combine` recipe value out of `master_provenance.recipe_json`
/// (spec §3): the new-shape [`IntegrationRecipe`] first, then the legacy
/// `CombineMethod` mapped to its equivalent. `None` if it matches neither.
pub fn parse_recipe_value(value: &serde_json::Value) -> Option<IntegrationRecipe> {
    if let Ok(recipe) = serde_json::from_value::<IntegrationRecipe>(value.clone()) {
        return Some(recipe);
    }
    serde_json::from_value::<LegacyCombineMethod>(value.clone())
        .ok()
        .map(Into::into)
}

/// Render a `master_provenance.recipe_json` blob for display (spec §3 reader):
/// pull its `combine` field, parse via [`parse_recipe_value`] (new-shape then
/// legacy), and describe it. Falls back to the raw JSON string when the blob
/// is neither shape (or has no `combine` field) so nothing is ever lost.
pub fn describe_recipe_json(recipe_json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(recipe_json)
        .ok()
        .as_ref()
        .and_then(|v| v.get("combine"))
        .and_then(parse_recipe_value)
        .map(|r| r.describe())
        .unwrap_or_else(|| recipe_json.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Combination basics ──────────────────────────────────────────────────

    /// `describe()` feeds the ATH_REJ FITS card, whose string values must be
    /// printable ASCII (0x20–0x7E) — a single non-ASCII char (the old ` · `
    /// separator) failed EVERY master build at the final header write.
    #[test]
    fn describe_is_printable_ascii_for_all_variants() {
        let rejections = [
            Rejection::None,
            Rejection::PercentileClip { low: 0.2, high: 0.1 },
            Rejection::SigmaClip { sigma_low: 4.0, sigma_high: 3.0 },
            Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 },
            Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 2.5 },
        ];
        for rej in rejections {
            for recipe in [IntegrationRecipe::average(rej), IntegrationRecipe::median(rej)] {
                let d = recipe.describe();
                assert!(
                    d.bytes().all(|b| (0x20..=0x7E).contains(&b)),
                    "describe() must be printable ASCII (FITS card value): {d:?}"
                );
            }
        }
    }

    #[test]
    fn average_and_median_basics() {
        let (v, r) = combine_pixel(&mut [1.0, 2.0, 3.0, 4.0], IntegrationRecipe::average(Rejection::None));
        assert_eq!((v, r), (2.5, 0));
        let (v, _) = combine_pixel(&mut [5.0, 1.0, 3.0], IntegrationRecipe::median(Rejection::None));
        assert_eq!(v, 3.0);
        let (v, _) = combine_pixel(&mut [4.0, 1.0, 3.0, 2.0], IntegrationRecipe::median(Rejection::None));
        assert_eq!(v, 2.5); // even N: mean of middle two
    }

    #[test]
    fn empty_and_singleton() {
        let (v, _) = combine_pixel(&mut [], IntegrationRecipe::average(Rejection::None));
        assert_eq!(v, 0.0);
        let (v, r) = combine_pixel(
            &mut [42.0],
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert_eq!((v, r), (42.0, 0));
    }

    // ── SigmaClip (spec §5) ─────────────────────────────────────────────────

    #[test]
    fn sigma_clip_rejects_hot_at_3sigma_keeps_at_10sigma() {
        // 20 well-behaved samples ~100 + one hot 5000.
        let base: Vec<f32> = {
            let mut v: Vec<f32> = (0..20).map(|i| 100.0 + (i % 5) as f32).collect();
            v.push(5000.0);
            v
        };

        // 3σ: the hot sample is rejected, result lands on the clean mean.
        let (v, rej) = combine_pixel(
            &mut base.clone(),
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert!(rej >= 1, "3σ must reject the hot sample");
        assert!((v - 102.0).abs() < 3.0, "combined near the clean mean, got {v}");

        // 10σ: the same hot sample stays within the (huge) band → kept.
        let (_, rej10) = combine_pixel(
            &mut base.clone(),
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 10.0, sigma_high: 10.0 }),
        );
        assert_eq!(rej10, 0, "10σ is wide enough to keep the outlier");
    }

    #[test]
    fn sigma_clip_keeps_clean_data() {
        let mut clean: Vec<f32> = (0..30).map(|i| 500.0 + (i % 7) as f32).collect();
        let (_, rej) = combine_pixel(
            &mut clean,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert_eq!(rej, 0, "within-σ data must be kept");
    }

    #[test]
    fn median_of_sigma_clip_survivors() {
        // Sigma clip drops the hot 1000, then the MEDIAN of the survivors is
        // taken — pins that rejection composes with the Median combination.
        let mut vals: Vec<f32> = vec![10.0, 11.0, 12.0, 13.0, 14.0, 1000.0];
        let (v, rej) = combine_pixel(
            &mut vals,
            IntegrationRecipe::median(Rejection::SigmaClip { sigma_low: 2.0, sigma_high: 2.0 }),
        );
        assert!(rej >= 1, "outlier rejected");
        assert_eq!(v, 12.0, "median of survivors {{10..14}} is 12");
    }

    #[test]
    fn sigma_clip_all_rejected_late_iter_uses_survivors_not_corrupted_stack() {
        // Regression (corrupted-prefix fallback). Symmetric bimodal-of-three
        // stack. Iteration 1 (m=5, s≈3.77 → ±1.88 band at 0.5σ) keeps only the
        // {4,4,4,6,6,6} core, compacting it into the prefix and OVERWRITING the
        // tail in place → array becomes [4,4,4,6,6,6, 6,6,6,10,10,10].
        // Iteration 2 over that core (m=5, s≈1.10 → ±0.55 band) rejects
        // EVERYTHING (4<4.45, 6>5.55) → w=0. Before the w==0 guard, kept fell to
        // 0 and combine_pixel's fallback took the median of that CORRUPTED array
        // = 6.0. With the guard we keep the iteration-1 survivors {4,4,4,6,6,6},
        // whose mean is 5.0 — equal to the median of the intact original stack —
        // and 6 samples are reported rejected.
        let mut stack = vec![0.0, 0.0, 0.0, 4.0, 4.0, 4.0, 6.0, 6.0, 6.0, 10.0, 10.0, 10.0];
        let (v, rej) = combine_pixel(
            &mut stack,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 0.5, sigma_high: 0.5 }),
        );
        assert_eq!(v, 5.0, "combine real iteration-1 survivors, never corrupted memory (was 6.0)");
        assert_eq!(rej, 6, "the six {{0,0,0,10,10,10}} extremes stay rejected");
    }

    #[test]
    fn sigma_clip_all_rejected_first_iter_keeps_intact_stack() {
        // All-reject on the FIRST iteration → no prior in-place compaction, so
        // no corruption is possible. Cleanly split stack: m=5, s≈5.22, and even
        // the tight ±0.5σ band [2.39, 7.61] excludes both the 0s and the 10s, so
        // w=0 immediately. The guard keeps the intact full stack; its mean is
        // 5.0 — identical to the pre-fix median-fallback value on this
        // (symmetric) stack, so this previously-correct path is not regressed.
        let mut stack = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0];
        let (v, rej) = combine_pixel(
            &mut stack,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 0.5, sigma_high: 0.5 }),
        );
        assert_eq!(v, 5.0);
        assert_eq!(rej, 0, "nothing corrupted; the full intact stack is kept");
    }

    // ── LinearFitClip (spec §5) ─────────────────────────────────────────────

    #[test]
    fn linear_fit_keeps_clean_ramp() {
        let ramp: Vec<f32> = (0..20).map(|i| 100.0 + 5.0 * i as f32).collect();
        let (v, rej) = combine_pixel(
            &mut ramp.clone(),
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
        );
        assert_eq!(rej, 0, "a clean linear ramp must be kept intact");
        let expect = ramp.iter().map(|&x| x as f64).sum::<f64>() / ramp.len() as f64;
        assert!((v as f64 - expect).abs() < 1e-3);
    }

    #[test]
    fn linear_fit_rejects_spike() {
        // Native [0,1] scale (what a calibrated light stack carries): 20 samples
        // around 0.20, ±0.002 jitter on a 1e-4/rank ramp, one hot frame at +0.30.
        // Dispersion doubled 2026-09-09: measured z 7.294 (old adev) → 3.647
        // (2·adev), still over sigma_high 3.5. The old fixture (100 + 5·i ramp,
        // spike 100_000) had a value/rank slope no real stack can produce; it is
        // retired, not re-thresholded.
        //
        // Medfit line (M4a Task 3, ruling R-M4a-4): measured z ≈ 9.759
        // (a=0.197581, b=0.00037280, adev=0.0152855) on the first-iteration
        // fit — well clear of sigma_high 3.5, more decisively than the LS
        // line's 3.647.
        let mut ramp: Vec<f32> = (0..20)
            .map(|i| 0.20 + 0.0001 * i as f32 + if i % 2 == 0 { 0.002 } else { -0.002 })
            .collect();
        ramp[10] += 0.30;
        let (v, rej) = combine_pixel(
            &mut ramp,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
        );
        assert!(rej >= 1, "spike must be rejected");
        assert!(v < 0.21, "combined value should not be dragged up by the spike, got {v}");
    }

    #[test]
    fn linear_fit_terminates_on_constant_stack() {
        // σ = 0 edge: identical samples → zero residual dispersion → no
        // rejection, and the loop terminates cleanly (guarded division).
        let mut flat = vec![42.0f32; 25];
        let (v, rej) = combine_pixel(
            &mut flat,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
        );
        assert_eq!(rej, 0);
        assert_eq!(v, 42.0);
    }

    #[test]
    fn linear_fit_all_rejected_uses_intact_stack_not_corruption() {
        // Same symmetric reproduction stack as the sigma-clip regression. The
        // least-squares line over this ramp-like stack used to leave every
        // residual outside the tight rejection band, so iteration 1 rejected
        // ALL 12 at once (w=0) BEFORE any in-place compaction — the array is
        // never corrupted here. With the w==0 guard, kept stays at the
        // initial 12 and the survivors (== the intact stack) average to 5.0.
        //
        // Medfit line (M4a Task 3, ruling R-M4a-4): the robust line for this
        // exact symmetric stack is a ≈ 0.0001431, b ≈ 0.9090649 — a fit that
        // (unlike least squares) runs almost exactly THROUGH the two extreme
        // ranks (measured residual ≈ 1.431e-4 at ranks 0 and 11, versus
        // ≈ 0.3636–1.8183 at every other rank), because the L1 line for this
        // perfectly symmetric data hinges on those two points. adev is
        // unchanged by the new fit (≈ 0.81823, s = 2·adev ≈ 1.63645) but the
        // old ±0.15σ band (well above the old ≈0.2807 boundary) now excludes
        // only 10 of 12 samples — the near-zero-residual extremes survive
        // (kept=2, {0.0, 10.0}, which still average to 5.0 by symmetry: the
        // guard is not what makes this one pass any more). To reproduce the
        // FIRST-iteration all-reject case this test exists for, the band
        // must be tighter than the ≈1.431e-4 extreme-rank residual: measured
        // boundary sigma ≈ 8.744e-5; 1e-5 keeps a comfortable margin below
        // it and rejects all 12 on iteration 1, same as before the fit
        // changed. This 1e-5 σ is an artefact of the L1 line hinging on the
        // two extreme ranks of this particular symmetric fixture, not a
        // general property of the routine (fix round 1 review, ruling
        // R-M4a-16).
        let mut stack = vec![0.0, 0.0, 0.0, 4.0, 4.0, 4.0, 6.0, 6.0, 6.0, 10.0, 10.0, 10.0];
        let (v, rej) = combine_pixel(
            &mut stack,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 1e-5, sigma_high: 1e-5 }),
        );
        assert_eq!(v, 5.0, "intact-stack combine, no corruption possible");
        assert_eq!(rej, 0, "guard keeps the full stack when iter 1 rejects everything");
    }

    #[test]
    fn linear_fit_dispersion_is_twice_adev() {
        // A perfect ramp of slope 0.01 per rank plus one spike. With the old
        // dispersion (adev alone) a residual ~4.6x adev is rejected at
        // thresholds 3.0; with s = 2·adev the SAME residual is only ~2.3x the
        // (now doubled) dispersion, so it survives; a bigger spike (~7.3x old
        // adev, ~3.6x new dispersion) is still rejected.
        //
        // Dispersion doubled 2026-09-09: raised from the original brief's
        // spike of +0.009 (case A) / +0.05 more (case B) — measured against
        // the routine's actual sorted-rank fit, that residual never exceeded
        // ~1.4x adev even under the OLD dispersion, so neither case changed
        // behavior. Case A's spike raised +0.009 -> +0.13, case B's
        // additional spike raised +0.05 -> +0.5 (both still relative to the
        // LS-line era, where the measured z was ≈4.605 / ≈2.302 for case A
        // old/new and ≈7.277 / ≈3.638 for case B old/new dispersion).
        //
        // Medfit line (M4a Task 3, ruling R-M4a-4): the robust fit changes
        // both the line AND adev on this fixture (its slope is no longer
        // pinned to the LS value, unlike the near-degenerate stack above) —
        // measured z ≈ 2.679 for case A (a=0.496476, b=0.010947,
        // adev=0.0055106) and z ≈ 8.683 for case B (a=0.496908,
        // b=0.010899, adev=0.0305202). Both land on the SAME side of the
        // 3.0 threshold as the least-squares line did (A survives, B is
        // rejected), so the assertions are unchanged, but the z values that
        // make them true are not — a coincidence of this fixture's fitted
        // slope being tiny either way, not a general property of the fit.
        let mut ramp: Vec<f32> = (0..20).map(|j| 0.5 + 0.01 * j as f32).collect();
        // deviations: ±0.004 alternating, plus the case-A spike below.
        for (j, v) in ramp.iter_mut().enumerate() {
            *v += if j % 2 == 0 { 0.004 } else { -0.004 };
        }
        ramp[10] += 0.13;
        let mut a = ramp.clone();
        let (_, rejected) = combine_pixel(
            &mut a,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert_eq!(rejected, 0, "a ~2.68-dispersion deviation survives at 3.0 under the robust line");
        let mut b = ramp.clone();
        b[10] += 0.5; // ~8.68x the robust-line dispersion — still rejected
        let (_, rejected) = combine_pixel(
            &mut b,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert_eq!(rejected, 1);
    }

    #[test]
    fn linear_fit_rejects_a_cosmic_ray_on_an_adu_scale_dark_column() {
        // A 20-frame dark column in native ADU: pedestal 500, read noise ±5
        // alternating, one +400 ADU cosmic ray. Under a slope-scaled dispersion
        // the sorted-rank slope (≈ 6 ADU/rank) inflated s six-fold and nothing
        // was rejected; with s = 2·adev the ray goes at 5.0/3.5 (measured z
        // ≈ 3.658, over sigma_high 3.5).
        //
        // Medfit line (M4a Task 3, ruling R-M4a-4): measured z ≈ 9.659
        // (a=494.971, b=0.908741, adev=19.9226) on the first-iteration fit —
        // well clear of sigma_high 3.5, more decisively than the LS line's
        // 3.658.
        let mut col: Vec<f32> = (0..20)
            .map(|i| 500.0 + if i % 2 == 0 { 5.0 } else { -5.0 } + 0.3 * i as f32)
            .collect();
        col[7] += 400.0;
        let (v, rej) = combine_pixel(
            &mut col,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
        );
        assert_eq!(rej, 1, "the cosmic ray");
        assert!(v < 510.0, "{v}");
    }

    // ── medfit_line (M4a Task 3, ruling R-M4a-4) ────────────────────────────

    /// Tiny deterministic PRNG for the contaminated-stack fixture below
    /// (mirrors `geometry::ransac::SplitMix64`; no `rand` crate dependency).
    struct SplitMix64(u64);
    impl SplitMix64 {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn next_f64(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// One N(0,1) draw off `rng` via the Box–Muller transform.
    fn next_gaussian(rng: &mut SplitMix64) -> f64 {
        let u1 = rng.next_f64().max(1e-12);
        let u2 = rng.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// `n` samples of N(`mean`, `sigma`), seeded for reproducibility.
    fn fixture_gaussian_stack(n: usize, mean: f64, sigma: f64, seed: u64) -> Vec<f32> {
        let mut rng = SplitMix64(seed);
        (0..n).map(|_| (mean + sigma * next_gaussian(&mut rng)) as f32).collect()
    }

    /// A single, explicitly-placed sample (no jitter) — used to drop known
    /// outlier values into an otherwise-random fixture.
    fn sample_from(value: f32) -> f32 {
        value
    }

    #[test]
    fn medfit_line_ignores_a_single_extreme_outlier_that_drags_least_squares() {
        // a clean ramp 10 + 0.5·i for i in 0..40, plus values[39] = 1000
        let mut v: Vec<f64> = (0..40).map(|i| 10.0 + 0.5 * i as f64).collect();
        v[39] = 1000.0;
        let (a, b) = medfit_line(&v, None);
        assert!((a - 10.0).abs() < 0.05 && (b - 0.5).abs() < 0.01, "medfit ({a}, {b}) must recover the ramp");
        // least squares on the same data does not: its slope is > 1.0 — the point of the test
    }

    #[test]
    fn medfit_line_returns_the_seed_when_the_seed_is_an_exact_root() {
        // A palindrome (values[i] == values[19-i]) makes the least-squares
        // slope exactly 0 by symmetry, and with equally many pairs above and
        // below the median, f(b_ls) == 0 exactly too — `f` is an
        // integer-valued step function, so this is common (fix round 1
        // review, ruling R-M4a-16, finding 3: measured 2.8 % of n = 20
        // stacks). Without the fix, the bisection's `fb * f1 >= 0.0`
        // tie-break at an exact zero shrinks the bracket toward the WRONG
        // side of the root, returning a worse line than the seed.
        let half = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let values: Vec<f64> = half.iter().chain(half.iter().rev()).copied().collect();

        // Verify the premise directly (reproduce medfit_line's own f(b_ls)):
        // the seed IS an exact root on this fixture.
        let n = values.len() as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
        for (i, &y) in values.iter().enumerate() {
            let x = i as f64;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let del = n * sxx - sx * sx;
        let b_ls = (n * sxy - sx * sy) / del;
        let a_ls = (sy - b_ls * sx) / n;
        let mut resid: Vec<f64> =
            values.iter().enumerate().map(|(i, &y)| y - b_ls * i as f64).collect();
        resid.sort_by(|p, q| p.partial_cmp(q).unwrap());
        let m = resid.len();
        let med = if m % 2 == 1 { resid[m / 2] } else { 0.5 * (resid[m / 2 - 1] + resid[m / 2]) };
        let mut f = 0.0;
        for (i, &y) in values.iter().enumerate() {
            let r = y - (med + b_ls * i as f64);
            if r > 0.0 {
                f += i as f64;
            } else if r < 0.0 {
                f -= i as f64;
            }
        }
        assert_eq!(f, 0.0, "premise: f(b_ls) must be an exact root on this fixture");
        assert_eq!(med, a_ls, "sanity: median residual equals the LS intercept on this symmetric fixture");

        let (a, b) = medfit_line(&values, None);
        assert_eq!((a, b), (a_ls, b_ls), "an exact root at the seed must be returned as-is, not walked away from");
    }

    /// Copied verbatim (module-level helpers substituted, generic `T: Sample`
    /// specialized to `f32`) from the pre-Task-3 `reject_linear_fit`
    /// (`git show d6361615:crates/athenaeum-core/src/integration/combine.rs`)
    /// — the least-squares routine this task replaced. Test-only: proves the
    /// discrimination in the test below is real, not assumed (fix round 1
    /// review, ruling R-M4a-16, finding 2).
    fn old_reject_linear_fit_ls(values: &mut [f32], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
        let n = values.len();
        if n < 3 {
            return (n, false);
        }
        sort_asc(values);
        let mut kept = n;
        for _ in 0..MAX_REJECTION_ITERS {
            let k = kept;
            if k < 2 {
                break;
            }
            let kf = k as f64;
            let (mut sx, mut sy, mut sxx, mut sxy, mut sabs_y) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for i in 0..k {
                let x = i as f64;
                let y = values[i] as f64;
                sx += x;
                sy += y;
                sxx += x * x;
                sxy += x * y;
                sabs_y += y.abs();
            }
            let denom = kf * sxx - sx * sx;
            if denom.abs() <= f64::EPSILON {
                break;
            }
            let b = (kf * sxy - sx * sy) / denom;
            let a = (sy - b * sx) / kf;
            let mut abs_sum = 0.0;
            for i in 0..k {
                let resid = values[i] as f64 - (a + b * i as f64);
                abs_sum += resid.abs();
            }
            let adev = abs_sum / kf;
            let s = 2.0 * adev;
            let scale = (sabs_y / kf).max(1.0);
            if adev <= 1e-9 * scale {
                break;
            }
            let lo = -sigma_low * s;
            let hi = sigma_high * s;
            let mut w = 0usize;
            for i in 0..k {
                let resid = values[i] as f64 - (a + b * i as f64);
                if resid >= lo && resid <= hi {
                    values[w] = values[i];
                    w += 1;
                }
            }
            if w == kept {
                break;
            }
            if w == 0 {
                break;
            }
            kept = w;
        }
        (kept, true)
    }

    #[test]
    fn linear_fit_rejection_with_the_robust_line_rejects_the_contaminated_tail_ls_kept() {
        // 200 samples: N(0.1, 0.002) + 6 high outliers at 0.106..0.136 — INSIDE
        // the old least-squares line's blind spot (fix round 1 review, ruling
        // R-M4a-16, finding 2): outliers at 0.13..0.16 sit at 15-28σ, which the
        // old LS routine already rejects on its own, so that fixture did not
        // discriminate between the two routines. The lowered tail drags the LS
        // line toward itself just enough that its own (inflated) dispersion
        // hides it — pinned below as a red-state assertion, not assumed.
        let mut tail = fixture_gaussian_stack(200, 0.1, 0.002, 12345);
        for (k, x) in tail.iter_mut().rev().take(6).enumerate() {
            *x = sample_from(0.106 + 0.006 * k as f32);
        }

        let mut old_copy = tail.clone();
        let (old_kept, _) = old_reject_linear_fit_ls(&mut old_copy, 5.0, 3.5);
        assert_eq!(
            old_kept, 195,
            "red-state pin: the least-squares routine must NOT discriminate this tail"
        );

        // Green: measured kept == 189 for the new medfit-based routine on
        // this exact fixture (versus the LS routine's 195 above) — a real
        // discrimination, not a coincidence of the threshold.
        let mut vals = tail;
        let (kept, _) = reject_linear_fit(&mut vals, 5.0, 3.5);
        assert!(kept <= 194, "all six outliers rejected at 5.0/3.5, kept {kept}");
    }

    /// The single least-squares line fit `medfit_line` replaces, copied from
    /// the pre-Task-3 `reject_linear_fit`'s LS block (`git show
    /// d6361615:crates/athenaeum-core/src/integration/combine.rs`),
    /// generalized from the per-iteration inline computation to a standalone
    /// `&[f64] -> (f64, f64)` function for this cost-ratio benchmark (fix
    /// round 1 review, ruling R-M4a-16, finding 1).
    fn old_least_squares_line(values: &[f64]) -> (f64, f64) {
        let k = values.len();
        let kf = k as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
        for (i, &y) in values.iter().enumerate() {
            let x = i as f64;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let denom = kf * sxx - sx * sx;
        if denom.abs() <= f64::EPSILON {
            return (sy / kf, 0.0);
        }
        let b = (kf * sxy - sx * sy) / denom;
        let a = (sy - b * sx) / kf;
        (a, b)
    }

    /// Cost of one cold (no warm start) `medfit_line` call relative to one
    /// `old_least_squares_line` call, at n = 20 (the `RejectionChoice::Auto`
    /// threshold) and n = 200 (a typical real stack) — ruling R-M4a-16,
    /// finding 1: the review measured 13-44x against the brief's 5-10x bar
    /// before `select_nth_unstable_by` replaced the per-bracket sort. Not
    /// run by default (timing tests are flaky under CI/parallel load) — run
    /// with `-- --ignored --nocapture` and read the printed ratios.
    #[test]
    #[ignore]
    fn medfit_cost_relative_to_least_squares() {
        use std::time::Instant;
        for &n in &[20usize, 200usize] {
            let mut rng = SplitMix64(0x9E37_79B9 ^ n as u64);
            // A real per-pixel stack, in the shape `reject_linear_fit`
            // actually hands `medfit_line`: SORTED ascending (`sort_asc`
            // runs before every call in production), a baseline plus
            // per-frame read noise.
            let mut values: Vec<f64> =
                (0..n).map(|_| 100.0 + 5.0 * next_gaussian(&mut rng)).collect();
            values.sort_by(|p, q| p.partial_cmp(q).unwrap());

            let iters = 20_000u32;
            let t0 = Instant::now();
            for _ in 0..iters {
                std::hint::black_box(old_least_squares_line(std::hint::black_box(&values)));
            }
            let old_elapsed = t0.elapsed();

            let t1 = Instant::now();
            for _ in 0..iters {
                std::hint::black_box(medfit_line(std::hint::black_box(&values), None));
            }
            let new_elapsed = t1.elapsed();

            let old_ns = old_elapsed.as_secs_f64() * 1e9 / iters as f64;
            let new_ns = new_elapsed.as_secs_f64() * 1e9 / iters as f64;
            eprintln!(
                "n={n}: old={old_ns:.1} ns/call new={new_ns:.1} ns/call ratio={:.2}x",
                new_ns / old_ns
            );
        }
    }

    // ── WinsorizedSigma & PercentileClip carried over ───────────────────────

    #[test]
    fn winsorized_rejects_hot_pixel() {
        let mut vals: Vec<f32> = (0..20).map(|i| 100.0 + (i % 5) as f32).collect();
        vals.push(5000.0);
        let (v, rejected) = combine_pixel(
            &mut vals,
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert!(rejected >= 1, "outlier must be rejected");
        assert!((v - 102.0).abs() < 3.0, "combined value near the clean mean, got {v}");
    }

    #[test]
    fn winsorized_sums_original_not_clamped_values() {
        // 12 cluster samples spread over 100.0..100.4 + one at 106.0. With
        // sigma_high = 50 the band reaches ~109 so the ORIGINAL 106.0 is kept;
        // the result must be the mean of the ORIGINAL samples (~100.62), not
        // the clamped work values (~100.20).
        let mut vals: Vec<f32> = (0..12).map(|i| 100.0 + (i % 5) as f32 * 0.1).collect();
        vals.push(106.0);
        let expected = vals.iter().map(|&x| x as f64).sum::<f64>() / vals.len() as f64;
        let (v, rejected) = combine_pixel(
            &mut vals,
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 50.0, sigma_high: 50.0 }),
        );
        assert_eq!(rejected, 0);
        assert!(
            (v as f64 - expected).abs() < 1e-3,
            "must average ORIGINAL samples: got {v}, want {expected}"
        );
    }

    #[test]
    fn percentile_clip_rejects_star_in_sky_flat() {
        let mut vals = vec![10000.0, 10050.0, 9980.0, 10020.0, 10900.0];
        let (v, rejected) = combine_pixel(
            &mut vals,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.2, high: 0.02 }),
        );
        assert_eq!(rejected, 1);
        assert!(v < 10100.0, "{v}");
    }

    // ── Legacy equivalence, bit-for-bit (spec §5) ───────────────────────────
    //
    // The old flat-`CombineMethod` implementations, replicated verbatim as a
    // recorded reference. Average+None must equal old `Mean` and
    // Average+WinsorizedSigma must equal old `WinsorizedSigmaClip` on the same
    // fixture stack, to the bit.

    fn legacy_mean(values: &[f32]) -> f32 {
        (values.iter().map(|&x| x as f64).sum::<f64>() / values.len() as f64) as f32
    }

    fn legacy_winsorized(values: &[f32], sigma_low: f64, sigma_high: f64) -> (f32, usize) {
        let n = values.len();
        if n < 3 {
            return (legacy_mean(values), 0);
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut work: Vec<f64> = sorted.iter().map(|&x| x as f64).collect();
        let mut m = work.iter().sum::<f64>() / n as f64;
        let mut s = stddev(&sorted, m);
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
                * (work.iter().map(|x| (x - new_m) * (x - new_m)).sum::<f64>() / (n - 1) as f64)
                    .sqrt();
            let converged = (new_s - s).abs() <= 0.005 * s.abs();
            m = new_m;
            s = new_s;
            if converged {
                break;
            }
        }
        let (lo, hi) = (m - sigma_low * s, m + sigma_high * s);
        let mut sum = 0.0f64;
        let mut kept = 0usize;
        for &x in sorted.iter() {
            let xf = x as f64;
            if xf >= lo && xf <= hi {
                sum += xf;
                kept += 1;
            }
        }
        if kept == 0 {
            return (median_sorted(&sorted), sorted.len());
        }
        ((sum / kept as f64) as f32, sorted.len() - kept)
    }

    fn fixture_stack() -> Vec<f32> {
        let mut v: Vec<f32> = (0..24)
            .map(|i| 1000.0 + (i as f32 * 0.37).sin() * 5.0 + (i % 3) as f32)
            .collect();
        v.push(9000.0); // outlier so winsorized actually rejects something
        v
    }

    #[test]
    fn legacy_equivalence_average_none_equals_old_mean() {
        let base = fixture_stack();
        let (new_v, rej) = combine_pixel(&mut base.clone(), IntegrationRecipe::average(Rejection::None));
        let old_v = legacy_mean(&base);
        assert_eq!(rej, 0);
        assert_eq!(
            new_v.to_bits(),
            old_v.to_bits(),
            "Average+None must equal old Mean bit-for-bit"
        );
    }

    #[test]
    fn legacy_equivalence_average_winsorized_equals_old_winsorized() {
        let base = fixture_stack();
        let (new_v, new_rej) = combine_pixel(
            &mut base.clone(),
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        let (old_v, old_rej) = legacy_winsorized(&base, 3.0, 3.0);
        assert_eq!(new_rej, old_rej, "rejected count must match the legacy path");
        assert_eq!(
            new_v.to_bits(),
            old_v.to_bits(),
            "Average+WinsorizedSigma must equal old WinsorizedSigmaClip bit-for-bit"
        );
    }

    // ── Legacy recipe_json fallback parse (spec §3, §5) ─────────────────────

    #[test]
    fn legacy_recipe_value_maps_to_equivalent() {
        assert_eq!(
            parse_recipe_value(&serde_json::json!({"method": "mean"})),
            Some(IntegrationRecipe::average(Rejection::None))
        );
        assert_eq!(
            parse_recipe_value(&serde_json::json!({"method": "median"})),
            Some(IntegrationRecipe::median(Rejection::None))
        );
        assert_eq!(
            parse_recipe_value(
                &serde_json::json!({"method": "winsorized_sigma_clip", "sigma_low": 3.0, "sigma_high": 3.0})
            ),
            Some(IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 3.0,
                sigma_high: 3.0
            }))
        );
        assert_eq!(
            parse_recipe_value(
                &serde_json::json!({"method": "percentile_clip", "low": 0.2, "high": 0.02})
            ),
            Some(IntegrationRecipe::average(Rejection::PercentileClip { low: 0.2, high: 0.02 }))
        );
    }

    #[test]
    fn new_recipe_value_round_trips() {
        let recipe = IntegrationRecipe::median(Rejection::SigmaClip { sigma_low: 4.0, sigma_high: 3.0 });
        let v = serde_json::to_value(recipe).unwrap();
        assert_eq!(parse_recipe_value(&v), Some(recipe));
        // Neither-shape → None.
        assert_eq!(parse_recipe_value(&serde_json::json!({"foo": 1})), None);
    }

    #[test]
    fn describe_recipe_json_new_legacy_and_raw() {
        // Legacy blob (combine holds an old CombineMethod).
        let legacy = serde_json::json!({
            "combine": {"method": "winsorized_sigma_clip", "sigma_low": 3.0, "sigma_high": 3.0},
            "syntheticBias": serde_json::Value::Null,
        })
        .to_string();
        assert_eq!(describe_recipe_json(&legacy), "Average | Winsorized sigma (3.0/3.0)");

        // New blob (combine holds an IntegrationRecipe).
        let recipe = IntegrationRecipe::median(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 });
        let new_blob = serde_json::json!({ "combine": recipe }).to_string();
        assert_eq!(describe_recipe_json(&new_blob), "Median | Linear fit clip (5.0/3.5)");

        // Unparseable → raw passthrough (nothing lost).
        assert_eq!(describe_recipe_json("not json at all"), "not json at all");
    }

    // ── Weighted combiner (generic Sample) ──────────────────────────────────

    /// SplitMix64, so the pin needs no dependency.
    fn rng_next(state: &mut u64) -> f64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn random_stack(state: &mut u64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| {
                let g = rng_next(state) + rng_next(state) + rng_next(state) - 1.5;
                let outlier =
                    if rng_next(state) < 0.05 { 6.0 * (rng_next(state) - 0.5) } else { 0.0 };
                // Never an exact zero: the weighted path skips zero-valued
                // samples (missing coverage), the plain mean averages them.
                (0.2 + 0.01 * g as f32 + outlier as f32).max(1e-4)
            })
            .collect()
    }

    #[test]
    fn weighted_combiner_with_unit_weights_is_bit_identical_to_combine_pixel() {
        let recipes = [
            IntegrationRecipe::average(Rejection::None),
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.2, high: 0.1 }),
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 4.0, sigma_high: 3.0 }),
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 }),
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
            IntegrationRecipe::median(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 3.0 }),
            IntegrationRecipe::median(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 }),
        ];
        let mut state = 0x5EED_1234u64;
        let mut scratch = Vec::new();
        for (k, recipe) in recipes.iter().enumerate() {
            for trial in 0..300 {
                let n = 3 + (trial % 30);
                let stack = random_stack(&mut state, n);
                let mut plain = stack.clone();
                let (v_plain, rej_plain) = combine_pixel(&mut plain, *recipe);
                let mut work: Vec<(f32, u16)> =
                    stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
                let weights = vec![1.0f32; n];
                let mut mask = vec![0u64; mask_words(n)];
                let (v_w, rej_w) = combine_pixel_weighted(
                    &mut work,
                    &stack,
                    &weights,
                    *recipe,
                    &mut mask,
                    &mut scratch,
                );
                assert_eq!(
                    v_plain.to_bits(),
                    v_w.to_bits(),
                    "recipe {k} trial {trial}: {v_plain} vs {v_w}"
                );
                assert_eq!(rej_plain, rej_w, "recipe {k} trial {trial}");
                let survivors = (0..n).filter(|&i| mask_get(&mask, i)).count();
                assert_eq!(survivors, n - rej_w, "recipe {k} trial {trial}");
            }
        }
    }

    #[test]
    fn weighted_average_weights_survivors_and_skips_zero_and_unweighted_samples() {
        // frames: 0 → 1.0 (w 3), 1 → 2.0 (w 1), 2 → 0.0 (w 1, missing coverage), 3 → 4.0 (w 0)
        let stack = [1.0f32, 2.0, 0.0, 4.0];
        let mut work: Vec<(f32, u16)> = stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [3.0f32, 1.0, 1.0, 0.0];
        let mut mask = vec![0u64; 1];
        let (v, rej) = combine_pixel_weighted(
            &mut work,
            &stack,
            &weights,
            IntegrationRecipe::average(Rejection::None),
            &mut mask,
            &mut Vec::new(),
        );
        assert_eq!(rej, 0);
        assert!((v - (3.0 * 1.0 + 1.0 * 2.0) / 4.0).abs() < 1e-6, "{v}");
        assert!((0..4).all(|i| mask_get(&mask, i)));
    }

    #[test]
    fn weighted_median_ignores_weights_and_mask_names_the_survivors() {
        let stack = [0.10f32, 0.11, 0.12, 0.13, 0.90];
        let mut work: Vec<(f32, u16)> = stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [1.0f32, 100.0, 1.0, 1.0, 1.0];
        let mut mask = vec![0u64; 1];
        let (v, rej) = combine_pixel_weighted(
            &mut work,
            &stack,
            &weights,
            IntegrationRecipe::median(Rejection::SigmaClip { sigma_low: 1.5, sigma_high: 1.5 }),
            &mut mask,
            &mut Vec::new(),
        );
        assert_eq!(rej, 1, "the 0.90 outlier");
        assert!(!mask_get(&mask, 4) && (0..4).all(|i| mask_get(&mask, i)));
        assert!((v - 0.115).abs() < 1e-6, "median of the four survivors: {v}");
    }

    #[test]
    fn rejection_normalized_values_decide_survival_but_output_values_are_averaged() {
        // Rejection copy says frame 2 is an outlier; its output value is ordinary.
        let rej = [1.0f32, 1.0, 9.0, 1.0, 1.0];
        let out = [0.5f32, 0.5, 0.5, 0.5, 0.5];
        let mut work: Vec<(f32, u16)> = rej.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [1.0f32; 5];
        let mut mask = vec![0u64; 1];
        let (v, rejected) = combine_pixel_weighted(
            &mut work,
            &out,
            &weights,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 2.0, sigma_high: 1.0 }),
            &mut mask,
            &mut Vec::new(),
        );
        assert_eq!(rejected, 1);
        assert!(!mask_get(&mask, 2));
        assert_eq!(v, 0.5);
    }

    #[test]
    fn all_rejected_falls_back_to_the_median_of_the_output_values() {
        // PercentileClip with zero thresholds on an even-length stack: the
        // median falls between two elements, so every sample deviates and
        // the rejection empties the stack (kept == 0).
        let stack = [0.2f32, 0.3, 0.4, 0.5];
        let mut work: Vec<(f32, u16)> = stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [1.0f32; 4];
        let mut mask = vec![0u64; 1];
        let mut plain = stack.to_vec();
        let (v_plain, r_plain) = combine_pixel(
            &mut plain,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.0, high: 0.0 }),
        );
        let (v, r) = combine_pixel_weighted(
            &mut work,
            &stack,
            &weights,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.0, high: 0.0 }),
            &mut mask,
            &mut Vec::new(),
        );
        assert_eq!((v.to_bits(), r), (v_plain.to_bits(), r_plain));
        assert_eq!(r, 4, "every sample rejected");
        assert!(mask.iter().all(|&w| w == 0), "no survivor bit on the all-rejected fallback");
    }

    #[test]
    fn mask_helpers_cover_word_boundaries() {
        let mut m = vec![0u64; mask_words(130)];
        assert_eq!(m.len(), 3);
        for i in [0usize, 63, 64, 127, 128, 129] {
            mask_set(&mut m, i);
        }
        assert!(mask_get(&m, 0) && mask_get(&m, 63) && mask_get(&m, 64) && mask_get(&m, 129));
        assert!(!mask_get(&m, 1) && !mask_get(&m, 65));
        mask_clear(&mut m);
        assert!(m.iter().all(|&w| w == 0));
    }
}
