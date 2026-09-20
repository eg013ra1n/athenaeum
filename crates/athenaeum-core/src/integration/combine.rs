//! Per-pixel robust integration recipes — the reference's two-axis model
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
//! `PercentileClip` = Average+PercentileClip. `Mean` and `PercentileClip`
//! are still pinned bit-for-bit by the tests; `WinsorizedSigmaClip` maps to
//! the same recipe but no longer to the same NUMBERS — ruling R-M4c-3 moved
//! the winsorized fixed point onto the reference loop (see
//! [`reject_winsorized`]).

use super::student_t;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

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
/// is the reference's name for our historical `Mean`.
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
    /// Reference-style percentile clipping around the median m: reject x when
    /// (m - x)/|m| > low or (x - m)/|m| > high. Deviations normalized by |m|
    /// so thresholds are sign-agnostic.
    PercentileClip { low: f64, high: f64 },
    /// Plain (non-winsorized) sigma clip: iteratively reject samples outside
    /// [m − sigma_low·σ, m + sigma_high·σ] of the current survivor set until
    /// stable. Zero-dispersion sets converge immediately with no rejection.
    SigmaClip { sigma_low: f64, sigma_high: f64 },
    /// Huber-style winsorized sigma clip, the reference loop (math reference
    /// §3.4, ruling R-M4c-3): a winsorized location/scale estimate from the
    /// median and the MAD, then reject original samples outside
    /// [m − sigma_low·s, m + sigma_high·s], repeated until stable. Not the
    /// pre-M4c fixed point — see [`reject_winsorized`].
    WinsorizedSigma { sigma_low: f64, sigma_high: f64 },
    /// Minimum-absolute-deviation ("robust") line fit over (rank, value);
    /// reject samples whose residual falls outside [−sigma_low·d,
    /// +sigma_high·d] where d is [`LINEAR_FIT_SIGMA_SCALE`] times twice the
    /// mean absolute deviation of the residuals from that line; refit and
    /// repeat until stable. The reference's recommended choice for larger
    /// sets with drifting illumination.
    LinearFitClip { sigma_low: f64, sigma_high: f64 },
    /// Min/max clipping (math reference §3.4): drop the `low` smallest and
    /// `high` largest samples outright, no statistics involved. The counts
    /// are clamped so at least one sample always survives.
    MinMax { low: usize, high: usize },
    /// Generalized extreme studentized deviate test (Rosner 1983, math
    /// reference §3.4): up to `k = clamp(trunc(outliers_fraction·n), 1, n-2)`
    /// sequential tests of the most extreme studentized residual against the
    /// critical value `lambda_i` for significance `alpha`, each removing that
    /// one sample; `low_relaxation` inflates the scale used below the centre,
    /// so faint samples are rejected less eagerly than bright ones.
    Esd { outliers_fraction: f64, alpha: f64, low_relaxation: f64 },
    /// Robust Chauvenet Rejection (Maples et al. 2018, math reference §3.4):
    /// three phases of decreasing robustness, each rejecting the single most
    /// extreme sample while the expected number of samples at least that
    /// extreme, `n·Q(|x - mu|/sigma)`, stays below `limit` (0.5 is
    /// Chauvenet's criterion).
    Rcr { limit: f64 },
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
            Rejection::MinMax { low, high } => format!("Min/max ({low}/{high})"),
            Rejection::Esd { outliers_fraction, alpha, low_relaxation } => format!(
                "ESD ({}/{}/{})",
                fmt_param(outliers_fraction),
                fmt_param(alpha),
                fmt_param(low_relaxation)
            ),
            Rejection::Rcr { limit } => format!("RCR ({})", fmt_param(limit)),
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

/// Stable by contract: the weighted path's summation order follows this
/// ordering; `sort_unstable_by` would permute tied (value, index) pairs and
/// change the f64 summation order.
///
/// Perf tier A Task 9 (I4, audit §3.3): BUILT and MEASURED as
/// `sort_unstable_by` with an explicit `(value, tie_key)` comparator
/// (`Sample::tie_key`, still on the trait — see its doc) reproducing this
/// same stable order, on the audit's own reasoning that it both avoids the
/// stable sort's ~1.6 KB `driftsort` allocation and is asymptotically an
/// unstable (pdqsort-family) sort. REVERTED: isolated microbenchmarks
/// (`diag_sort_asc_cost` in this module's history) on `(f32, u16)` data at
/// `n` = 20 and 208, 200 000 iterations, comparing `sort_by` (this
/// function) against `sort_unstable_by` with and without the tie_key
/// comparator: at `n` = 208 the unstable sort alone (no tie_key, same
/// comparator cost as this function) was already 1.18× SLOWER than this
/// stable sort despite never allocating, and the tie_key comparator adds
/// another ~1.15× on top (1.36× total) even though ties are rare in that
/// fixture and `Ordering::then_with` short-circuits the tie_key extraction
/// on every non-tied pair. On this machine's toolchain the standard
/// library's stable sort (`driftsort`) is simply faster than
/// `sort_unstable_by`'s pattern-defeating quicksort for a `(f32, u16)`
/// stack at the sizes this rejection loop actually sees — the audit's
/// "unstable never allocates, so it must be faster" premise does not hold
/// here. Kept as the reasoning a future attempt at this item should read
/// before re-trying the same shape.
fn sort_asc<T: Sample>(v: &mut [T]) {
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
///
/// The third return value (perf tier A Task 9, I5) is `true` exactly when
/// `work[..n - rejected]` (the surviving prefix, `rejected` being the
/// second return value) is ascending by `value()` — every rejection
/// algorithm but `None`/`SigmaClip` sorts `work` before compacting, and the
/// stable, order-preserving compaction that follows keeps that order in the
/// surviving prefix (each one's own doc says so where it isn't obvious). A
/// caller that wants a plain positional median of the survivors — not
/// weighted, not `out_values`-based — can read it directly out of that
/// prefix instead of re-sorting `work` a second time. Always `false` when
/// every sample was rejected (the early return below): that branch's own
/// median already comes from a fresh sort of `out_values`, not `work`, so
/// there is nothing for the caller to reuse either way.
pub fn combine_pixel_weighted(
    work: &mut [(f32, u16)],
    out_values: &[f32],
    weights: &[f32],
    recipe: IntegrationRecipe,
    mask: &mut [u64],
    scratch: &mut Vec<f32>,
) -> (f32, usize, bool) {
    debug_assert_eq!(out_values.len(), weights.len());
    debug_assert!(work.iter().all(|&(_, i)| (i as usize) < out_values.len()));
    debug_assert!(
        mask.iter().all(|&w| w == 0),
        "combine_pixel_weighted: mask must be cleared per pixel"
    );
    let n = work.len();
    if n == 0 {
        return (0.0, 0, false);
    }
    let (kept, sorted) = apply_rejection(work, recipe.rejection);
    if kept == 0 {
        scratch.clear();
        scratch.extend(work.iter().map(|&(_, i)| out_values[i as usize]));
        sort_asc(scratch);
        return (median_sorted(scratch), n, false);
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
    (value, n - kept, sorted)
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
        Rejection::MinMax { low, high } => reject_min_max(values, low, high),
        Rejection::Esd { outliers_fraction, alpha, low_relaxation } => {
            reject_esd(values, outliers_fraction, alpha, low_relaxation)
        }
        Rejection::Rcr { limit } => reject_rcr(values, limit),
    }
}

// ── Rejection algorithms (in place, allocation-free) ───────────────────────

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

/// Winsorization point: the working copy is clamped at `mu ± 1.5·sigma`
/// (math reference §3.4).
const WINSORIZE_CLAMP_SIGMA: f64 = 1.5;

/// First-pass cutoff: a sample beyond `mu ± 5·sigma` is replaced by the
/// CENTRE rather than by the neighbouring clamp threshold, so a gross
/// outlier cannot prop the scale up from just outside the band. Later passes
/// clamp plainly (nothing is left out that far).
const WINSORIZE_CUTOFF_SIGMA: f64 = 5.0;

/// Normal-distribution correction for the 1.5-sigma Winsorization point:
/// the standard deviation of a winsorized standard normal is 0.88231, and
/// `1/0.88231 = 1.1334` (math reference §3.4).
const WINSORIZE_SCALE_CORRECTION: f64 = 1.134;

/// `1.4826·MAD` is the σ-consistent MAD scale of a normal sample — the
/// initial scale of the loop (ruling R-M4c-3: a documented deviation from
/// the reference's `1.1926·Sn`, which is O(n²) per pixel stack. The
/// first-pass cutoff above makes the start point nearly irrelevant, and the
/// loop reaches the same fixed point from either one).
const WINSORIZE_MAD_TO_SIGMA: f64 = 1.4826;

/// Relative change in `sigma` below which the Winsorization loop has
/// settled (math reference §3.4), honoured from the second pass on.
const WINSORIZE_CONVERGENCE: f64 = 0.0005;

/// Pass cap for the Winsorization loop (math reference §3.4).
const WINSORIZE_MAX_PASSES: usize = 20;

thread_local! {
    // `reject_winsorized`'s working copy of the current survivor values, in
    // the f32 the samples already are. Cleared and reused per call — this
    // rejection runs inside the per-pixel band loop, so a fresh `Vec` per
    // pixel is not acceptable.
    static WINSORIZE_SCRATCH: RefCell<Vec<f32>> = RefCell::new(Vec::new());
}

/// Median of `v`, which it reorders in place (`select_nth_unstable_by`, so
/// no sortedness contract on the caller and no second buffer). Even lengths
/// average the two middle samples, like [`median_sorted_f64`].
fn median_in_place(v: &mut [f32]) -> f64 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    let (low, mid, _) =
        v.select_nth_unstable_by(n / 2, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let upper = *mid as f64;
    if n % 2 == 1 {
        upper
    } else {
        // Everything left of the pivot is <= it, so the lower middle sample
        // is the largest of that partition.
        let lower = low.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
        0.5 * (lower + upper)
    }
}

/// Winsorized location and scale of a pixel stack — `(mu_w, sigma_w,
/// passes)`, math reference §3.4 via ruling R-M4c-3.
///
/// Start `mu = median`, `sigma = 1.4826·MAD` about it, then loop: clamp the
/// working copy to `[mu − 1.5σ, mu + 1.5σ]` — on the FIRST pass a sample
/// beyond `mu ± 5σ` becomes `mu` instead (an extreme outlier goes to the
/// centre, not to the neighbouring threshold, and stays there for the rest
/// of the loop since the working copy is winsorized cumulatively) — then
/// `mu = mean(v)`, `sigma = 1.134·stddev(v)`; stop once `sigma` moves by
/// less than 0.05 % and at least two passes have run, or at 20 passes.
///
/// **Zero-MAD fallback (ruling R-T2-1), ours and not the reference's.** The
/// MAD is 0 for any stack whose MAJORITY is tied, not just for identical
/// samples — and integer-ADU calibration stacks are exactly that shape, so
/// `15 × 500 ADU + one cosmic ray` would seed `sigma = 0` and switch the
/// whole rejection off on the DEFAULT master recipe. When the MAD is 0 the
/// scale is seeded from the sample standard deviation about the median
/// instead (the retired estimator's contaminated seed, which restores its
/// answer on exactly these stacks); a stack with a non-zero MAD can never
/// reach that branch, so nothing else moves. All-identical samples have no
/// dispersion under either seed and still reject nothing. The reference's
/// `1.1926·Sn` seed degenerates on the same stacks — this fallback is our
/// own, not something read out of §3.4.
///
/// Allocation-free: `scratch` is the caller's reused buffer, used first for
/// the two medians and then as the working copy. The working copy is `f32`
/// where the reference's loop is `f64`; a clamped value's quantization is
/// ~6e-8 relative, three orders of magnitude below the 5e-4 stop rule, so
/// it cannot change how many passes the loop takes.
///
/// Fewer than three samples, and zero dispersion by either seed, return
/// `sigma_w = 0` — which the caller reads as "nothing to reject".
fn winsorized_location_scale<T: Sample>(values: &[T], scratch: &mut Vec<f32>) -> (f64, f64, usize) {
    let n = values.len();
    if n == 0 {
        return (0.0, 0.0, 0);
    }
    if n < 3 {
        // The loop's centre is the median, so the degenerate answer is the
        // median too — a caller that reads mu without looking at sigma must
        // not be handed a different statistic than the loop would give it.
        scratch.clear();
        scratch.extend(values.iter().map(|s| s.value()));
        return (median_in_place(&mut scratch[..]), 0.0, 0);
    }
    scratch.clear();
    scratch.extend(values.iter().map(|s| s.value()));
    let mut mu = median_in_place(&mut scratch[..]);
    for (dst, src) in scratch.iter_mut().zip(values.iter()) {
        *dst = (src.value() as f64 - mu).abs() as f32;
    }
    let mad = median_in_place(&mut scratch[..]);
    let mut sigma = if mad > 0.0 {
        WINSORIZE_MAD_TO_SIGMA * mad
    } else {
        // Majority-tied stack: see the fallback note above.
        stddev(values, mu)
    };
    scratch.clear();
    scratch.extend(values.iter().map(|s| s.value()));

    let mut passes = 0usize;
    while passes < WINSORIZE_MAX_PASSES {
        if !(sigma > 0.0) {
            break; // zero dispersion (or a non-finite scale): nothing to do
        }
        passes += 1;
        let t0 = mu - WINSORIZE_CLAMP_SIGMA * sigma;
        let t1 = mu + WINSORIZE_CLAMP_SIGMA * sigma;
        // The cutoff is a first-pass device: after one pass nothing sits
        // beyond the clamp thresholds any more, let alone beyond 5 sigma.
        let cutoff = passes == 1;
        let (c0, c1) = (mu - WINSORIZE_CUTOFF_SIGMA * sigma, mu + WINSORIZE_CUTOFF_SIGMA * sigma);
        for x in scratch.iter_mut() {
            let v = *x as f64;
            if v < t0 {
                *x = if cutoff && v <= c0 { mu as f32 } else { t0 as f32 };
            } else if v > t1 {
                *x = if cutoff && v >= c1 { mu as f32 } else { t1 as f32 };
            }
        }
        let new_mu = mean_f64(&scratch[..]);
        let new_sigma = WINSORIZE_SCALE_CORRECTION * stddev(&scratch[..], new_mu);
        let settled = passes >= 2 && (new_sigma - sigma).abs() < WINSORIZE_CONVERGENCE * sigma;
        mu = new_mu;
        sigma = new_sigma;
        if settled {
            break;
        }
    }
    (mu, sigma, passes)
}

/// Winsorized sigma clipping (math reference §3.4, ruling R-M4c-3): the
/// winsorized location/scale of the current survivors, a sigma clip of the
/// ORIGINAL samples about `(mu_w, sigma_w)`, repeated until nothing more is
/// rejected.
///
/// Before M4c this routine ran ONE clip about a location/scale seeded from
/// the contaminated mean and standard deviation — byte-identical to the
/// pre-2026-07-06 `WinsorizedSigmaClip` estimator, deliberately, so that
/// masters built by older versions could be reproduced. Ruling R-M4c-3
/// retires that fixed point: the old seed let a gross outlier inflate the
/// very scale it was supposed to be measured against, because plain clamping
/// pinned it at `mu + 1.5·sigma` and left it there, while the reference
/// loop's first-pass cutoff maps it to the centre where it has no leverage.
/// Measured on 100 draws of N(0.1, 0.002) plus 5 samples at +10 sigma
/// (`winsorized_location_scale_lands_on_the_reference_fixed_point`), against
/// each estimator's OWN answer on the clean 100: contamination moves the
/// retired estimator +8.2 % (0.00185773 → 0.00201002) and the reference loop
/// −5.3 % (0.00181578 → 0.00172028).
///
/// **No fixture fingerprint pin moved.** The Winsorized fixtures in this
/// module and in `engine.rs` are tolerance- or survivor-set-based, and on
/// `fixture_stack()` both fixed points reject the same single outlier and
/// average the same 24 survivors — so the bit-for-bit legacy pin below still
/// holds, incidentally (it now asserts the estimators DISAGREE as well, so
/// it cannot quietly become a claim of equivalence). The real move was
/// measured on 21 calibrated LDN 1272 mono frames at 4.0/3.0: rejected
/// fraction 0.380 % → 0.963 %, master median −0.024 %, master MAD +0.50 %,
/// master noise +2.0 %, 6.59 % of the 25.9 M pixels changed. Full numbers in
/// the M4c Task 2 commit body and spec §6.3.
fn reject_winsorized<T: Sample>(values: &mut [T], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    sort_asc(values);
    let mut kept = n;
    WINSORIZE_SCRATCH.with(|cell| {
        let scratch = &mut *cell.borrow_mut();
        for iter in 0..MAX_REJECTION_ITERS {
            let (m, s, _passes) = winsorized_location_scale(&values[..kept], scratch);
            if !(s > 0.0) {
                break; // zero dispersion → no (further) rejection
            }
            let (lo, hi) = (m - sigma_low * s, m + sigma_high * s);
            // Stable compaction: survivors keep the ascending order the
            // summation relies on (w <= r throughout, so the write never
            // clobbers an unread slot).
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
                // Nothing survived. On the FIRST iteration no write has
                // happened yet, so the full stack is intact and
                // `combine_pixel`'s all-rejected fallback can take its median
                // (the historical behaviour of this routine). On a later
                // iteration an earlier compaction has already overwritten the
                // tail — keep that iteration's intact survivor prefix rather
                // than fabricating from clobbered memory, exactly as
                // `reject_sigma_clip` does.
                if iter == 0 {
                    kept = 0;
                }
                break;
            }
            kept = w;
            if kept < 3 {
                break; // the winsorized estimate is not defined below 3
            }
        }
    });
    (kept, true)
}

/// Dispersion-scale knob for [`Rejection::LinearFitClip`]: the rejection band
/// half-width is `sigma_low`/`sigma_high` multiples of
/// `s = LINEAR_FIT_SIGMA_SCALE * 2 * adev` (`adev` = mean absolute deviation
/// of the residuals from the fitted line — spec §6.3, math reference §3.4).
/// Empirical, calibrated by the M4a acceptance run (ruling R-M4a-4): with the
/// least-squares line the M2 acceptance run measured 0.83 %/0.74 % rejected
/// at the Auto 5.0/3.5 thresholds on the LDN 1272 set, against the reference
/// implementation's 2.5–2.8 %. Switching to the minimum-absolute-deviation
/// line (below) closes the whole gap on its own: the M4a acceptance run
/// (2026-09-11, run 13 on the same set) measured 2.985 % (mono) / 2.733 %
/// (OSC) rejected at the Auto 5.0/3.5 with this constant at `1.0` — inside
/// the 2.3–3.3 % target — so `1.0` is the calibrated value, not a
/// placeholder; a future re-tune is still a one-line diff away.
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

thread_local! {
    // Perf tier A Task 0 (audit §3.3, item I11): this thread's own count of
    // `reject_linear_fit`'s outer convergence-loop passes and
    // `medfit_line`'s `rofunc` bracket/bisection evaluations since the last
    // [`take_rejection_counters`] call. Both are read-and-reset once per
    // PIXEL STACK (never per evaluation) by the engine, right after the
    // `combine_pixel`/`combine_pixel_weighted` call that may have driven
    // them — two relaxed atomic adds per pixel, not per evaluation — so a
    // rayon worker's running total never has to be untangled from another
    // worker's on the same thread (there is none: a `Cell` is only ever
    // touched by the thread that owns it) or leaked across plane calls.
    static LINEAR_FIT_ITERS: Cell<u64> = const { Cell::new(0) };
    static MEDFIT_EVALS: Cell<u64> = const { Cell::new(0) };
}

/// Read-and-reset [`LINEAR_FIT_ITERS`]/[`MEDFIT_EVALS`] for the CALLING
/// thread: this thread's own iteration/evaluation counts accumulated since
/// the previous call (or since the thread started, on the first). The
/// caller folds the pair into a shared total once per pixel stack — see the
/// `thread_local!` block's own doc for why the reset happens here rather
/// than being left to the next call to re-derive by subtraction.
pub(crate) fn take_rejection_counters() -> (u64, u64) {
    let iters = LINEAR_FIT_ITERS.with(Cell::take);
    let evals = MEDFIT_EVALS.with(Cell::take);
    (iters, evals)
}

#[inline]
fn robust_sign(x: f64) -> f64 {
    if x >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// Half-width, in least-squares slope standard errors `σ_b`, of the bracket
/// [`medfit_line`] opens around its seed. A COLD call (no warm start) keeps
/// the pre-C4 value and is bit-identical to its pre-C4 self; it is also
/// measured to be near its own optimum, since the robust slope sits
/// `|b_root − b_ls|` p50 = 2.5·σ_b from the least-squares seed — `3·σ_b`
/// brackets barely over half of all cold calls in one probe, and shrinking
/// it would buy one bisection at the price of two evaluations on the rest.
const MEDFIT_COLD_BRACKET_SIGMAS: f64 = 3.0;

/// How many times narrower a WARM [`medfit_line`] bracket starts than the
/// cold one (perf tier C, C4/I8): `3·σ_b / 2³ = 0.375·σ_b`. The previous
/// outer iteration's converged slope is close enough to this one's that the
/// wide bracket only bought bisections — each halving of the starting width
/// is one evaluation saved, since the tolerance it bisects down to is fixed
/// (`1e-3·σ_b`; I9, which would have loosened it, was dropped because the
/// R-M4a-17 dispersion calibration rests on it).
///
/// Measured, not assumed. The slope movement between two outer iterations
/// over 4 000 seeded stacks (n ∈ {8, 20, 50, 100, 200}, 70 % pure noise and
/// the rest carrying hot pixels / a trail / a cold pixel) is `|Δb|`
/// p50 = 0.22·σ_b, p90 = 0.93·σ_b — so a bracket much under `σ_b` pays for
/// itself, and one far under it starts buying widenings back. Swept over
/// the 1 500 stacks of the pins' own families, as a fraction of the
/// evaluations the unchanged `3·σ_b` bracket costs:
///
/// | half-width | evaluations | different roots | worse fit | worst Δadev | mean Δadev |
/// | ---- | ---- | ---- | ---- | ---- | ---- |
/// | 0.75·σ_b | 0.933 | 29 | 71 | 8.5e-2 | +8.3e-5 |
/// | 0.5·σ_b | 0.907 | 77 | 613 | 5.7e-3 | +7.6e-6 |
/// | **0.375·σ_b** | **0.919** | **40** | **141** | **1.5e-3** | **−2.5e-6** |
/// | 0.25·σ_b | 0.902 | 81 | 615 | 3.1e-3 | −6.1e-7 |
/// | 0.1875·σ_b | 0.920 | 50 | 232 | 2.4e-2 | +2.0e-5 |
/// | 0.125·σ_b | 0.910 | 84 | 614 | 3.1e-3 | −2.3e-6 |
///
/// (counts out of 1 000 warm calls; `Δadev` is the relative change in the
/// mean absolute deviation of the returned line — the objective the fit
/// minimizes. No candidate changed a single survivor of the 1 500 stacks,
/// and the rejected fraction over the population was 2.9912 % for every
/// one of them, the unchanged bracket included.)
///
/// A POWER-OF-TWO fraction of the cold bracket specifically. The bisection
/// stops when the bracket is under `tol`, so its FINAL width is
/// `h₀ / 2^⌈log₂(h₀/tol)⌉` — a function of `h₀/tol`, not of `tol` alone. At
/// `3·σ_b/2^k` that final width is the cold bracket's own 0.73·tol and a
/// warm call is no coarser than a cold one; at 0.5, 0.25 or 0.125·σ_b it is
/// 0.98·tol, and the warm line comes back the worse fit on 61 % of calls
/// instead of 14 % — a systematic coarsening rather than a coin flip, even
/// though the amount involved (mean Δadev ~1e-6) is far below anything
/// downstream can see. Widening x2 keeps the alignment, walking the bracket
/// back up through `3·σ_b/4` and `/2` to `3·σ_b` exactly.
///
/// The same sweep is why the width is a multiple of the CURRENT call's
/// `σ_b` rather than a width carried over from the previous call (the shape
/// C4's brief first proposed): `σ_b` collapses between outer iterations as
/// the extreme samples leave the survivor set, so a width in absolute slope
/// units is stale by the time it is used — simulated at 1.00x, i.e. no gain
/// at all.
const MEDFIT_WARM_BRACKET_SHIFT: u32 = 3;

/// Widenings a bracket may take before `medfit_line` gives up and falls back
/// to the least-squares line. The cold path's own pre-C4 bound; a warm call
/// is allowed [`MEDFIT_WARM_BRACKET_SHIFT`] more, which is exactly the
/// number it spends walking its narrower start back up to the cold bracket,
/// so a warm call can never resolve LESS than a cold one would.
const MEDFIT_WIDENINGS: u32 = 32;

/// Minimum-absolute-deviation line `y = a + b·i` over `values[i]` (math
/// reference §3.4): the classic median/bisection method. Seeds the search
/// from `warm_start_b` when given (the caller's previous iteration's
/// converged slope — fix round 1, ruling R-M4a-16: after the first
/// iteration the bracket collapses to a handful of evaluations instead of
/// walking out from the least-squares slope every time) or the
/// least-squares slope otherwise. The bracket half-width always uses the
/// least-squares slope standard error `σ_b` of the CURRENT survivors,
/// regardless of which slope seeded `b1`, but its MULTIPLE of `σ_b` follows
/// the seed (perf tier C, C4/I8): a cold call opens the full
/// [`MEDFIT_COLD_BRACKET_SIGMAS`] bracket, a warm one that shifted right by
/// [`MEDFIT_WARM_BRACKET_SHIFT`] — the previous iteration's converged slope
/// is close enough to this one's that the wide bracket only bought
/// bisections — with that many extra widenings on top of
/// [`MEDFIT_WIDENINGS`], so a warm call can never resolve LESS than a cold
/// one would.
/// Walks the slope that zeroes the
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
        //
        // Perf tier A Task 9 (audit §3.3, I1/I2): both were BUILT and
        // MEASURED — `t_i = b·i` cached once per element into a second
        // scratch buffer and reused at the sign-test site (I1), and the
        // sign sum split across four `i64` accumulators (I2) — and both
        // were reverted. `t_i = b * i as f64` is a register-to-register
        // multiply once `i` (the loop counter) and `b` (already in a
        // register for the whole closure) are both in registers, so
        // caching it in a `Vec` trades a near-free multiply for a store on
        // the write side and a load on the read side; on this machine that
        // is NOT a win. Isolated microbenchmarks
        // (`diag_medfit_new_vs_oracle_cost` in this module's history, run
        // at `n` = 20 and 208 with 50 000 iterations each): I1 alone 1.42–
        // 1.55× the pre-Task-9 cost, I2 alone 1.05–1.09× (both SLOWER, not
        // faster), combined 1.57–1.72×; the `integrate_probe` end-to-end
        // measurement in the Task 9 report shows the same direction on the
        // real 60-frame set. Kept here as the reasoning a future attempt at
        // this item should read before re-trying the same shape.
        let mut rofunc = |b: f64| -> (f64, f64) {
            // Perf tier A Task 0: one evaluation, thread-local, flushed by
            // `take_rejection_counters` — see that function's doc.
            MEDFIT_EVALS.with(|c| c.set(c.get() + 1));
            scratch.clear();
            scratch.extend(values.iter().enumerate().map(|(i, &y)| y - b * i as f64));
            let m = scratch.len();
            let cmp = |p: &f64, q: &f64| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal);
            let a = if m % 2 == 1 {
                *scratch.select_nth_unstable_by(m / 2, cmp).1
            } else {
                // I3 (perf tier A Task 9): the max of the lower partition
                // `select_nth_unstable_by` already produced IS the lower
                // median element — there is no cheaper way to learn it than
                // scanning `lo` (an unsorted partition of unknown internal
                // order; a second `select_nth_unstable_by` call to find its
                // max would cost MORE than this linear fold, not less), so
                // this was already the minimal shape before Task 9 and
                // stays textually unchanged.
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
        // C4/I8: a warm seed opens a bracket `2^MEDFIT_WARM_BRACKET_SHIFT`
        // times narrower than the cold `3·σ_b`, and is allowed that many
        // extra widenings — which walk it right back up to the cold bracket
        // and beyond, so no separate fallback is needed and a warm call can
        // never bracket less than a cold one. The cold arm's expression is
        // unchanged down to its floating-point shape — including the
        // widening's `b1 + 2·(b2 − b1)` rather than an equivalent
        // `b1 + 2h·dir`, which is NOT the same rounding when `|b1| ≫ h` — so
        // a cold call stays bit-identical.
        let warm = warm_start_b.is_some();
        let half_width = if warm {
            MEDFIT_COLD_BRACKET_SIGMAS / (1u32 << MEDFIT_WARM_BRACKET_SHIFT) as f64
        } else {
            MEDFIT_COLD_BRACKET_SIGMAS
        };
        let mut b2 = b1 + half_width * sigma_b * robust_sign(f1);
        let (mut f2, _) = rofunc(b2);

        let mut widenings = 0u32;
        let widening_cap = if warm {
            MEDFIT_WIDENINGS + MEDFIT_WARM_BRACKET_SHIFT
        } else {
            MEDFIT_WIDENINGS
        };
        while f1 * f2 > 0.0 && widenings < widening_cap {
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
            // Perf tier A Task 0: one outer-loop pass, thread-local,
            // flushed by `take_rejection_counters` — see that function's
            // doc.
            LINEAR_FIT_ITERS.with(|c| c.set(c.get() + 1));
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

// ── Min/max, generalized ESD and RCR (M4c Task 1, rulings R-M4c-1/2) ───────
//
// All three are USER choices: `RejectionChoice::resolve`'s Auto ladder never
// selects them (ruling R-M4c-1), so no existing master or stack changes
// because they exist.

/// Median of a value-sorted slice in f64. The f32 `median_sorted` above
/// rounds the even-length average through f32; the ESD centre must not,
/// since its residuals are divided by an f64 standard deviation.
fn median_sorted_f64<T: Sample>(v: &[T]) -> f64 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2].value() as f64
    } else {
        0.5 * (v[n / 2 - 1].value() as f64 + v[n / 2].value() as f64)
    }
}

/// Min/max clipping (math reference §3.4): drop the `low` smallest and
/// `high` largest samples.
///
/// The counts are clamped so at least one sample always survives — a
/// `MinMax { 10, 10 }` on a 20-frame stack keeps one rather than emptying
/// the stack into `combine_pixel`'s all-rejected median fallback, which
/// would silently turn "clip the extremes" into "take the median of
/// everything, extremes included". The low count is honoured first and the
/// high count absorbs the clamp, so the deterministic survivor of a
/// fully-saturated request is the sample just above the requested low cut.
fn reject_min_max<T: Sample>(values: &mut [T], low: usize, high: usize) -> (usize, bool) {
    let n = values.len();
    if n == 0 {
        return (0, false);
    }
    let budget = n - 1; // never reject every sample
    let lo = low.min(budget);
    let hi = high.min(budget - lo);
    if lo == 0 && hi == 0 {
        return (n, false); // nothing to drop, and nothing sorted
    }
    sort_asc(values);
    let kept = n - lo - hi;
    if lo > 0 {
        values.copy_within(lo..n - hi, 0);
    }
    (kept, true)
}

/// Generalized ESD (Rosner 1983; math reference §3.4), `n >= 3`.
///
/// `k = clamp(trunc(f·n), 1, n-2)` sequential tests. Each one estimates the
/// centre as a trimmed mean of the current set (`t_l` dropped low, `t_h`
/// dropped high, the median when that would leave fewer than 3 samples),
/// takes `s_h = stddev(set; mu)` and `s_l = rho·s_h`, forms the studentized
/// residual `(x - mu)/s_h` above the centre and `(mu - x)/s_l` below it, and
/// compares the largest against `lambda_i`. The first `i` whose maximum
/// residual falls below `lambda_i` IS the outlier count, so the loop stops
/// there without removing anything more.
///
/// The residual grows monotonically away from the centre on each side, so
/// the sample removed by a test is always one of the two ENDS of the sorted
/// stack — the surviving set stays the contiguous range `values[lo..hi]`
/// and the whole routine is allocation-free. Ties go to the high side (the
/// same convention as RCR's, §3.4).
///
/// A degenerate configuration (non-positive outlier fraction, an `alpha`
/// outside `(0, 1)`) is not a licence to reject: the stack is returned
/// untouched.
fn reject_esd<T: Sample>(
    values: &mut [T],
    outliers_fraction: f64,
    alpha: f64,
    low_relaxation: f64,
) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    if !(outliers_fraction > 0.0) || !(alpha > 0.0) || alpha >= 1.0 {
        return (n, false);
    }
    let f = outliers_fraction;
    let rho = if low_relaxation > 0.0 { low_relaxation } else { 1.0 };
    let k = ((f * n as f64).trunc() as usize).clamp(1, n - 2);
    sort_asc(values);
    let view: &[T] = values;
    // One memo lookup per pixel stack for the whole lambda vector (ruling
    // R-M4c-2). The closure reads the slice and nothing else — it must never
    // call back into `with_esd_lambdas` (the thread-local is borrowed for
    // its duration).
    let (lo, hi) = student_t::with_esd_lambdas(n, alpha, k, |lambdas| {
        let (mut lo, mut hi) = (0usize, n);
        for i in 0..k {
            let set = &view[lo..hi];
            let m = set.len();
            if m < 3 {
                break;
            }
            let mf = m as f64;
            // Trimming counts for the centre estimate: the relaxation
            // factor trims fewer samples off the low side.
            let t_h = ((f * mf).trunc() as usize).saturating_sub(i).max(1);
            let t_l = (((f / rho) * mf).trunc() as usize).saturating_sub(i).max(1);
            let mu = if t_l + t_h + 2 < m {
                mean_f64(&set[t_l..m - t_h])
            } else {
                median_sorted_f64(set)
            };
            let s_h = stddev(set, mu);
            if !(s_h > 0.0) {
                break; // zero dispersion: nothing is extreme
            }
            let s_l = rho * s_h;
            let studentized = |x: f64| {
                if x >= mu {
                    (x - mu) / s_h
                } else {
                    (mu - x) / s_l
                }
            };
            let r_lo = studentized(set[0].value() as f64);
            let r_hi = studentized(set[m - 1].value() as f64);
            if r_lo.max(r_hi) < lambdas[i] {
                break; // this `i` is the outlier count
            }
            if r_hi >= r_lo {
                hi -= 1;
            } else {
                lo += 1;
            }
        }
        (lo, hi)
    });
    let kept = hi - lo;
    if lo > 0 {
        values.copy_within(lo..hi, 0);
    }
    (kept, true)
}

/// Distinct `N` keys [`RCR_HALF_NORMAL_TABLE`] holds before it is dropped
/// wholesale — the same bound, for the same reason, as the ESD lambda memo.
const RCR_TABLE_MAX_KEYS: usize = 4096;

thread_local! {
    /// `reject_rcr`'s sorted absolute deviations from the current centre,
    /// reused across calls: RCR runs inside the per-pixel band loop, so a
    /// fresh `Vec` per pixel stack is not acceptable.
    static RCR_DEVIATION_SCRATCH: RefCell<Vec<f64>> = RefCell::new(Vec::new());
    /// Half-normal quantile table for the RCR line-fit deviation, keyed by
    /// the deviation count `N`: the `m = trunc(0.683N + 0.317)` abscissae
    /// `sqrt(2)·erfinv((i + 1 - 0.317)/N)` and their sum of squares, both
    /// functions of `N` alone. Same reasoning as the ESD lambda memo
    /// (ruling R-M4c-2): every pixel stack of a plane walks the same
    /// handful of `N` values, and each entry costs `m` `erfinv`
    /// evaluations to build — without the memo the line-fit phase pays
    /// them per iteration per pixel.
    static RCR_HALF_NORMAL_TABLE: RefCell<HashMap<usize, (Vec<f64>, f64)>> =
        RefCell::new(HashMap::new());
}

/// Small-sample correction `F(N) = 1/(1 - 2.9442·N^-1.073)` (math reference
/// §3.4), capped at 20 where the denominator stops being usefully positive
/// (`N <= 2`).
fn rcr_small_sample_factor(n: usize) -> f64 {
    let d = 1.0 - 2.9442 * (n as f64).powf(-1.073);
    if d <= 0.05 {
        20.0
    } else {
        1.0 / d
    }
}

/// Linear-interpolation quantile of a sorted slice (position `p·(n - 1)`).
fn rcr_quantile_sorted(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    let pos = p * (n - 1) as f64;
    let i = pos.floor() as usize;
    let frac = pos - i as f64;
    if i + 1 >= n {
        sorted[n - 1]
    } else {
        sorted[i] + frac * (sorted[i + 1] - sorted[i])
    }
}

/// `SampleDeviation = F(N)·quantile_0.683(|x - mu|)` over the sorted
/// deviations (math reference §3.4).
fn rcr_sample_deviation(devs_sorted: &[f64]) -> f64 {
    rcr_small_sample_factor(devs_sorted.len()) * rcr_quantile_sorted(devs_sorted, 0.683)
}

/// `LineFitDeviation` (math reference §3.4): regress the lowest
/// `m = trunc(0.683N + 0.317)` sorted deviations against the half-normal
/// quantiles `sqrt(2)·erfinv((i + 1 - 0.317)/N)` with a line through the
/// origin and return `F(N)·y(1)`. Below 8 regression points it defers to
/// [`rcr_sample_deviation`], as does a degenerate abscissa table.
fn rcr_line_fit_deviation(devs_sorted: &[f64]) -> f64 {
    let n = devs_sorted.len();
    let m = (0.683 * n as f64 + 0.317).trunc() as usize;
    if m < 8 {
        return rcr_sample_deviation(devs_sorted);
    }
    RCR_HALF_NORMAL_TABLE.with(|cell| {
        let mut table = cell.borrow_mut();
        if table.len() >= RCR_TABLE_MAX_KEYS && !table.contains_key(&n) {
            table.clear();
        }
        let (xs, sxx) = table.entry(n).or_insert_with(|| {
            let xs: Vec<f64> = (0..m)
                .map(|i| {
                    std::f64::consts::SQRT_2
                        * student_t::erfinv((i as f64 + 1.0 - 0.317) / n as f64)
                })
                .collect();
            let sxx = xs.iter().map(|x| x * x).sum::<f64>();
            (xs, sxx)
        });
        if !(*sxx > 0.0) {
            return rcr_sample_deviation(devs_sorted);
        }
        let sxy = xs.iter().zip(devs_sorted).map(|(x, y)| x * y).sum::<f64>();
        rcr_small_sample_factor(n) * (sxy / *sxx)
    })
}

/// Upper Gaussian tail `Q(z) = erfc(z/sqrt(2))/2`.
fn rcr_gauss_tail(z: f64) -> f64 {
    0.5 * student_t::erfc(z / std::f64::consts::SQRT_2)
}

/// Sorted-ascending `|x - median|` of a VALUE-SORTED slice, written into
/// `out`; returns the median. Because the input is value-sorted the
/// deviations grow monotonically walking outward from the median position on
/// each side, so a two-pointer merge produces them already sorted in `O(n)`
/// — no second sort per iteration.
fn rcr_sorted_deviations<T: Sample>(sorted: &[T], out: &mut Vec<f64>) -> f64 {
    out.clear();
    let m = sorted.len();
    if m == 0 {
        return f64::NAN;
    }
    let med;
    let (mut l, mut r): (isize, isize);
    if m % 2 == 1 {
        let mid = m / 2;
        med = sorted[mid].value() as f64;
        out.push(0.0);
        l = mid as isize - 1;
        r = mid as isize + 1;
    } else {
        let (a, b) = (m / 2 - 1, m / 2);
        med = 0.5 * (sorted[a].value() as f64 + sorted[b].value() as f64);
        // The two central deviations are equal by the definition of an
        // even-length median.
        let d = (sorted[b].value() as f64 - med).abs();
        out.push(d);
        out.push(d);
        l = a as isize - 1;
        r = b as isize + 1;
    }
    loop {
        let dl = if l >= 0 {
            Some((med - sorted[l as usize].value() as f64).abs())
        } else {
            None
        };
        let dr = if (r as usize) < m {
            Some((sorted[r as usize].value() as f64 - med).abs())
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

/// Robust Chauvenet Rejection (Maples et al. 2018; math reference §3.4),
/// `n >= 3`.
///
/// Three phases of decreasing robustness — (0) median + line-fit deviation,
/// (1) median + sample deviation, (2) mean + standard deviation — each
/// iterated to convergence: while the smaller of `n·Q((mu - x_min)/sigma)`
/// and `n·Q((x_max - mu)/sigma)` is below `limit`, that one extreme is
/// rejected (ties go to the high side); otherwise the phase ends.
/// `limit = 0.5` is Chauvenet's criterion.
///
/// Same structural property as ESD above: only single extremes leave, so
/// the survivors stay the contiguous sorted range `values[lo..hi]` and the
/// routine allocates nothing per pixel (the deviation buffer and the
/// half-normal abscissae are thread-local and reused).
///
/// This is a second implementation of the same algorithm as
/// `stacking::robust::rcr`, which cannot be reached from here (`stacking` is
/// gated behind `render + solver`, `integration` is not). A cross-check test
/// in `stacking::robust` holds the two to the same answers.
fn reject_rcr<T: Sample>(values: &mut [T], limit: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    sort_asc(values);
    let (lo, hi) = RCR_DEVIATION_SCRATCH.with(|cell| {
        let mut devs = cell.borrow_mut();
        let (mut lo, mut hi) = (0usize, n);
        for phase in 0..3 {
            loop {
                let set = &values[lo..hi];
                let m = set.len();
                if m < 3 {
                    break;
                }
                let (mu, sigma) = if phase < 2 {
                    let med = rcr_sorted_deviations(set, &mut devs);
                    let s = if phase == 0 {
                        rcr_line_fit_deviation(&devs)
                    } else {
                        rcr_sample_deviation(&devs)
                    };
                    (med, s)
                } else {
                    let mu = mean_f64(set);
                    (mu, stddev(set, mu))
                };
                if !(sigma > 0.0) {
                    break; // zero dispersion: nothing is extreme
                }
                let mf = m as f64;
                let d_lo = mf * rcr_gauss_tail((mu - set[0].value() as f64) / sigma);
                let d_hi = mf * rcr_gauss_tail((set[m - 1].value() as f64 - mu) / sigma);
                if d_lo.min(d_hi) >= limit {
                    break; // the phase has converged
                }
                if d_hi <= d_lo {
                    hi -= 1;
                } else {
                    lo += 1;
                }
            }
        }
        (lo, hi)
    });
    let kept = hi - lo;
    if lo > 0 {
        values.copy_within(lo..hi, 0);
    }
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
            Rejection::MinMax { low: 1, high: 1 },
            Rejection::Esd { outliers_fraction: 0.3, alpha: 0.05, low_relaxation: 1.5 },
            Rejection::Rcr { limit: 0.5 },
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

    // ── The cold-bracket reference (tier A Task 9 I1-I5, tier C C4) ───────
    //
    // The two functions below are the `medfit_line`/`reject_linear_fit`
    // bodies as they stood BEFORE both of those cycles, copied verbatim
    // (`medfit_line`'s thread-local scratch swapped for a plain local `Vec`
    // — irrelevant to the numbers, only to where the scratch lives). One
    // copy serves both cycles because tier A Task 9 landed no change inside
    // `medfit_line` at all: I1/I2 were built, measured slower and reverted,
    // I3 was already minimal, so the pre-Task-9 and pre-C4 texts are the same
    // text. They prove, respectively, that I1 (`t_i = b·i` fused) and I2
    // (four integer sign-sum accumulators) moved zero bits, and that C4's
    // warm bracket moves nothing a cold call would have decided.

    /// The pre-Task-9 / pre-C4 `medfit_line`: `rofunc` recomputes
    /// `b * i as f64` at both the residual-build and sign-test sites and
    /// accumulates the sign sum into one running f64 total, and EVERY call —
    /// warm-seeded or not — opens the full `3·σ_b` bracket.
    ///
    /// It ticks `MEDFIT_EVALS` exactly as production does, so a test can
    /// price its evaluations against production's through
    /// [`take_rejection_counters`] by running the two in sequence.
    fn medfit_line_cold_reference(values: &[f64], warm_start_b: Option<f64>) -> (f64, f64) {
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
        let del = nf * sxx - sx * sx;
        if del.abs() <= f64::EPSILON {
            return (sy / nf, 0.0);
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
            return (a_ls, b_ls);
        }

        let mut scratch: Vec<f64> = Vec::new();
        let mut rofunc = |b: f64| -> (f64, f64) {
            MEDFIT_EVALS.with(|c| c.set(c.get() + 1));
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
            return (a_ls, b_ls);
        }

        let tol = 1e-3 * sigma_b;
        let mut iters = 0u32;
        let mut last_a = a1;
        let mut last_b = b1;
        while (b2 - b1).abs() >= tol && iters < 60 {
            let bb = 0.5 * (b1 + b2);
            if bb == b1 || bb == b2 {
                break;
            }
            let (fb, ab) = rofunc(bb);
            if fb == 0.0 {
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
    }

    /// The pre-Task-9 / pre-C4 `reject_linear_fit`, calling
    /// [`medfit_line_cold_reference`] instead of the production
    /// `medfit_line` — every other line (the outer convergence loop, the
    /// dispersion/threshold math) is untouched by both cycles, so this
    /// exists only to carry the reference line fit through the exact same
    /// survivor-compaction loop the production routine runs. C4 changed the
    /// BRACKET inside `medfit_line`, not one line of this loop, so the two
    /// differ in exactly the way the C4 pins measure.
    fn reject_linear_fit_cold_reference<T: Sample>(
        values: &mut [T],
        sigma_low: f64,
        sigma_high: f64,
    ) -> (usize, bool) {
        let n = values.len();
        if n < 3 {
            return (n, false);
        }
        sort_asc(values);
        let mut kept = n;
        let mut warm_start_b: Option<f64> = None;
        let mut scratch: Vec<f64> = Vec::new();
        for _ in 0..MAX_REJECTION_ITERS {
            let k = kept;
            if k < 2 {
                break;
            }
            let kf = k as f64;
            scratch.clear();
            scratch.extend(values[..k].iter().map(|s| s.value() as f64));
            let (a, b) = medfit_line_cold_reference(&scratch, warm_start_b);
            warm_start_b = Some(b);

            let mut abs_sum = 0.0;
            let mut sabs_y = 0.0;
            for (i, &y) in scratch.iter().enumerate() {
                let resid = y - (a + b * i as f64);
                abs_sum += resid.abs();
                sabs_y += y.abs();
            }
            let adev = abs_sum / kf;
            let s = LINEAR_FIT_SIGMA_SCALE * 2.0 * adev;
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
                break;
            }
            if w == 0 {
                break;
            }
            kept = w;
        }
        (kept, true)
    }

    /// One stack of the pin family below: `n` samples of N(100, 5), every
    /// third stack coarsely quantized so ties are frequent.
    fn medfit_pin_stack(seed: u64) -> Vec<f64> {
        let mut rng = SplitMix64(0xD1B5_4A32_D192_ED03 ^ seed);
        let n = 8 + (rng.next_u64() % 200) as usize; // 8..=207
        let quantize = seed % 3 == 0;
        (0..n)
            .map(|_| {
                let raw = 100.0 + 5.0 * next_gaussian(&mut rng);
                if quantize {
                    (raw / 2.0).round() * 2.0 // coarse quantization: frequent ties
                } else {
                    raw
                }
            })
            .collect()
    }

    /// Least-squares slope and its standard error `σ_b`, the scale
    /// `medfit_line`'s bracket and its `tol = 1e-3·σ_b` are both expressed
    /// in — recomputed here so a pin can state a bound in units of the
    /// tolerance rather than of an arbitrary absolute slope.
    fn ls_slope_and_sigma(values: &[f64]) -> (f64, f64) {
        let nf = values.len() as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
        for (i, &y) in values.iter().enumerate() {
            let x = i as f64;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let del = nf * sxx - sx * sx;
        let b_ls = (nf * sxy - sx * sy) / del;
        let a_ls = (sy - b_ls * sx) / nf;
        let mut chisq = 0.0;
        for (i, &y) in values.iter().enumerate() {
            let resid = y - (a_ls + b_ls * i as f64);
            chisq += resid * resid;
        }
        (b_ls, (chisq / del).sqrt())
    }

    /// I1/I2 and C4's cold path in one pin: a COLD `medfit_line` call is
    /// bit-identical to [`medfit_line_cold_reference`] over 1 000 seeded
    /// random stacks, sizes 8..207 (the audit's own n range), ties included.
    ///
    /// C4 narrowed the bracket a WARM call opens and left the cold one
    /// untouched down to its floating-point shape, so this pin covers both:
    /// the `rofunc` internals I1/I2 were about are exercised identically by
    /// either seed, and the cold arm is the one C4 promises has not moved a
    /// bit. The warm arm is
    /// [`medfit_line_warm_bracket_is_as_good_a_fit_as_the_cold_bracket`].
    #[test]
    fn medfit_line_cold_path_matches_the_cold_reference_bit_for_bit() {
        for seed in 0..1000u64 {
            let values = medfit_pin_stack(seed);
            let n = values.len();
            let cold = medfit_line(&values, None);
            let reference = medfit_line_cold_reference(&values, None);
            assert_eq!(
                (cold.0.to_bits(), cold.1.to_bits()),
                (reference.0.to_bits(), reference.1.to_bits()),
                "seed {seed} n {n} (cold): {cold:?} vs reference {reference:?}"
            );
        }
    }

    /// C4/I8: a WARM `medfit_line` call brackets `b_prev ± 0.375·σ_b` instead
    /// of `± 3·σ_b`, so it bisects a different sequence of midpoints and
    /// converges to a different point — usually of the same tolerance
    /// window, sometimes of another one entirely. This pin measures both,
    /// and bounds the only thing that is actually a promise: that the line
    /// it lands on is as good a minimum-absolute-deviation fit as the cold
    /// bracket's.
    ///
    /// The seed is built the way `reject_linear_fit` builds one — the
    /// converged slope of the FULL stack, fed to a fit of the stack minus its
    /// top samples, i.e. exactly what the second outer iteration does.
    ///
    /// Measured over the 1 000 stacks: 960 land within two tolerances of the
    /// cold bracket's answer (`tol = 1e-3·σ_b`) and 40 (4.0 %) converge to a
    /// genuinely DIFFERENT root. That is not a defect and not a rounding
    /// artifact: `f(b) = Σ xᵢ·sgn(…)` is an integer-valued step function
    /// whose zero set is a plateau, so the minimum-absolute-deviation line
    /// is genuinely NOT unique — the cold bracket's pick was never the
    /// canonical one, it is whichever crossing a `3·σ_b`-wide bisection
    /// happened to walk into, while the narrow bracket finds the crossing
    /// nearest the previous iteration's slope. What matters is that neither
    /// is the WORSE line, and it is not: the warm line's mean absolute
    /// deviation — the objective both are minimizing — is above the cold
    /// line's on 141 of 1 000 calls, never by more than 0.151 %, and on
    /// average 2.5e-6 BELOW it. Downstream this changes nothing: see
    /// [`reject_linear_fit_survivors_match_the_cold_bracket_reference`], 0
    /// of 1 500 stacks with a different survivor.
    #[test]
    fn medfit_line_warm_bracket_is_as_good_a_fit_as_the_cold_bracket() {
        let mut within_two_tolerances = 0usize;
        let mut different_roots = 0usize;
        let mut worst_adev_rel = 0.0f64;
        let mut warm_worse = 0usize;
        let mut calls = 0usize;
        for seed in 0..1000u64 {
            let mut values = medfit_pin_stack(seed);
            values.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let n = values.len();
            // Iteration 1: a cold fit of the whole stack (bit-identical
            // either way — the pin above), then drop the top samples the way
            // a rejection pass would and refit warm.
            let (_, b0) = medfit_line(&values, None);
            let survivors = &values[..n - (n / 20).max(1)];
            let (_, sigma_b) = ls_slope_and_sigma(survivors);
            if !(sigma_b > 0.0) {
                continue; // the exact-line guard returns before bracketing
            }
            let tol = 1e-3 * sigma_b;
            let warm = medfit_line(survivors, Some(b0));
            let reference = medfit_line_cold_reference(survivors, Some(b0));
            calls += 1;
            if (warm.1 - reference.1).abs() <= 2.0 * tol {
                within_two_tolerances += 1;
            } else {
                different_roots += 1;
            }
            // The objective both lines minimize, evaluated on the same data.
            let adev = |line: (f64, f64)| -> f64 {
                survivors
                    .iter()
                    .enumerate()
                    .map(|(i, &y)| (y - (line.0 + line.1 * i as f64)).abs())
                    .sum::<f64>()
                    / survivors.len() as f64
            };
            let (warm_adev, reference_adev) = (adev(warm), adev(reference));
            if warm_adev > reference_adev {
                warm_worse += 1;
            }
            worst_adev_rel =
                worst_adev_rel.max((warm_adev - reference_adev).abs() / reference_adev.max(1e-12));
        }
        assert!(
            worst_adev_rel <= 0.01,
            "the warm bracket's line is {:.3} % off the cold bracket's own objective (measured 0.151 % worst)",
            100.0 * worst_adev_rel
        );
        assert!(
            warm_worse * 2 <= calls,
            "the warm bracket returned the worse fit on {warm_worse} of {calls} calls — a systematic coarsening, not the coin flip a dyadic bracket width gives (measured 141)"
        );
        assert!(
            different_roots * 5 <= calls,
            "{different_roots} of {calls} calls landed on a different root — over the 20 % bar (measured 40, 4.0 %)"
        );
        assert!(
            within_two_tolerances + different_roots == calls,
            "bookkeeping"
        );
    }

    /// I1/I2 and C4 on NaN-carrying input: `values` containing a NaN makes `chisq`
    /// (and therefore `sigma_b`) NaN in BOTH the production function and the
    /// oracle, so `!(sigma_b > 0.0)` is true and both return `(a_ls, b_ls)`
    /// WITHOUT ever calling `rofunc` — I1/I2 touch nothing on this path, and
    /// the pin proves it stays that way (a future change to the pre-`rofunc`
    /// guard would be caught here).
    #[test]
    fn medfit_line_matches_the_cold_reference_on_nan_carrying_input() {
        let cases: [&[f64]; 3] = [
            &[1.0, 2.0, f64::NAN, 4.0, 5.0],
            &[f64::NAN, f64::NAN, f64::NAN],
            &[1.0, 2.0, 3.0, 4.0, f64::NAN, 6.0, 7.0, 8.0],
        ];
        for values in cases {
            let a = medfit_line(values, None);
            let b = medfit_line_cold_reference(values, None);
            assert_eq!(
                (a.0.to_bits(), a.1.to_bits()),
                (b.0.to_bits(), b.1.to_bits()),
                "{values:?}: {a:?} vs reference {b:?}"
            );
        }
    }

    /// The pin families for `reject_linear_fit`: the 500 contaminated stacks
    /// tier A Task 9 used (a planted bright tail, n = 20..207) followed by
    /// 1 000 stacks shaped like a real plane instead — sizes the pipeline
    /// actually integrates at, 70 % of them pure noise and the rest carrying
    /// hot pixels, a satellite trail or a cold pixel, which is the mix the
    /// C4 constants were swept against.
    fn reject_pin_stacks() -> Vec<Vec<f32>> {
        let mut out = Vec::with_capacity(1500);
        for seed in 0..500u64 {
            let mut rng = SplitMix64(0xA5A5_1234_5678_9ABC ^ seed);
            let n = 20 + (rng.next_u64() % 188) as usize; // 20..=207
            let mut values: Vec<f32> = (0..n)
                .map(|_| (100.0 + 5.0 * next_gaussian(&mut rng)) as f32)
                .collect();
            // A handful of high outliers, same shape as the discrimination
            // fixture above.
            let n_out = 1 + (rng.next_u64() % 5) as usize;
            for k in 0..n_out.min(n) {
                values[k] = 300.0 + 10.0 * k as f32;
            }
            out.push(values);
        }
        let sizes = [8usize, 20, 50, 100, 200];
        for seed in 0..1000u64 {
            let n = sizes[(seed as usize) % sizes.len()];
            let mut rng = SplitMix64(0x51ED_2701_ABCD_0001 ^ seed);
            let mut values: Vec<f32> = (0..n)
                .map(|_| (100.0 + 5.0 * next_gaussian(&mut rng)) as f32)
                .collect();
            match seed % 10 {
                0 => {
                    // hot pixels / cosmic rays
                    let k = 1 + (rng.next_u64() % 4) as usize;
                    for j in 0..k.min(n) {
                        values[j] = 300.0 + 10.0 * j as f32;
                    }
                }
                1 => {
                    // a satellite trail: several moderate positives
                    let k = 1 + (rng.next_u64() % 6) as usize;
                    for j in 0..k.min(n) {
                        values[j] = 130.0 + 4.0 * j as f32;
                    }
                }
                2 => {
                    values[0] = 20.0; // a cold pixel, and one hot
                    values[n - 1] = 400.0;
                }
                _ => {} // pure noise, as most of a plane is
            }
            out.push(values);
        }
        out
    }

    /// C4/I8 through `reject_linear_fit`, and I1/I2 transitively: the warm
    /// bracket must not move which samples survive.
    ///
    /// It is allowed to — the converged slope is a different point of the
    /// same tolerance window, so a sample sitting exactly on a rejection
    /// threshold could fall the other way — which is why the bar is a rate
    /// (identical survivors on ≥ 99.9 % of stacks, and never a survivor
    /// count more than one apart) rather than bit-for-bit equality. Measured
    /// over the 1 500 stacks of [`reject_pin_stacks`]: 0 stacks differ in
    /// any way, and the rejected fraction over the whole population is
    /// identical to four decimals. The `sorted` flag is a contract of the
    /// routine, not a measurement, so it is still asserted exactly.
    #[test]
    fn reject_linear_fit_survivors_match_the_cold_bracket_reference() {
        let stacks = reject_pin_stacks();
        let total = stacks.len();
        let mut differing = 0usize;
        for (i, values) in stacks.iter().enumerate() {
            let n = values.len();
            let mut a = values.clone();
            let mut b = values.clone();
            let (kept_a, sorted_a) = reject_linear_fit(&mut a, 5.0, 3.5);
            let (kept_b, sorted_b) = reject_linear_fit_cold_reference(&mut b, 5.0, 3.5);
            assert_eq!(sorted_a, sorted_b, "stack {i} n {n}: sorted flag");
            assert!(
                (kept_a as i64 - kept_b as i64).abs() <= 1,
                "stack {i} n {n}: survivor counts {kept_a} vs {kept_b} — more than one sample apart"
            );
            let bits_a: Vec<u32> = a[..kept_a].iter().map(|v| v.to_bits()).collect();
            let bits_b: Vec<u32> = b[..kept_b].iter().map(|v| v.to_bits()).collect();
            if bits_a != bits_b {
                differing += 1;
            }
        }
        assert!(
            differing * 1000 <= total,
            "{differing} of {total} stacks kept different survivors — over the 0.1 % bar (measured 0)"
        );
    }

    /// C4/I8's reason to exist: the narrow warm bracket must actually cost
    /// fewer `medfit_line` evaluations, counted by `MEDFIT_EVALS` (which
    /// [`medfit_line_cold_reference`] ticks the same way production does, so
    /// the two are priced on one scale) over the same 1 500 stacks.
    ///
    /// Measured: 30.15 evaluations per stack against the cold bracket's
    /// 32.81, i.e. 0.919x. The bar is set at 0.95x — loose enough not to
    /// fail on a fixture reshuffle, tight enough that a bracket change which
    /// stopped paying for itself would be caught.
    ///
    /// C4's brief asked for ≤ 0.5x, which is unreachable by ANY change
    /// confined to the warm bracket: the outer loop runs ~2 iterations per
    /// stack, so the first call of each — cold, and untouched by C4 — is
    /// about half of all evaluations, and free warm calls would still only
    /// reach ~0.5x. The rest are bisections of a window that cannot be
    /// widened without loosening `tol`, which I9 dropped on purpose.
    #[test]
    fn reject_linear_fit_warm_bracket_cuts_medfit_evaluations() {
        let stacks = reject_pin_stacks();
        let mut reference_evals = 0u64;
        let mut production_evals = 0u64;
        for values in &stacks {
            let mut a = values.clone();
            let mut b = values.clone();
            let _ = take_rejection_counters();
            let _ = reject_linear_fit_cold_reference(&mut b, 5.0, 3.5);
            let (_, evals) = take_rejection_counters();
            reference_evals += evals;
            let _ = reject_linear_fit(&mut a, 5.0, 3.5);
            let (_, evals) = take_rejection_counters();
            production_evals += evals;
        }
        let ratio = production_evals as f64 / reference_evals as f64;
        assert!(
            ratio <= 0.95,
            "warm bracket costs {ratio:.3}x the cold bracket's evaluations ({production_evals} vs {reference_evals}) — measured 0.905x"
        );
    }

    /// Perf tier C, C4/I7: the rejection loop stops on the FIRST iteration
    /// that rejects nothing. That iteration IS the confirming pass — it runs
    /// a full `medfit_line` and a full filter sweep to learn that the
    /// survivor set has stopped moving — and the loop never runs a second
    /// one. Observed through `LINEAR_FIT_ITERS` (read by
    /// [`take_rejection_counters`]), which ticks once per outer pass.
    ///
    /// C4's brief expected this exit still to be missing ("today it runs one
    /// more to confirm"); it has been in the loop since the linear-fit
    /// clipper first shipped (`e8512317`), as `if w == kept { break; }`, so
    /// I7 is a no-op and this pin is all that is new. Measured while
    /// establishing that: over 1 000 seeded stacks (n ∈ {8, 20, 50, 100,
    /// 200}, contaminated and clean) the final iteration rejected 0 samples
    /// on 1 000 of 1 000, mean 2.36 outer iterations, max 11 — the exit
    /// fires, `MAX_REJECTION_ITERS` never does.
    #[test]
    fn reject_linear_fit_stops_on_the_first_iteration_that_rejects_nothing() {
        // (a) One gross outlier over an otherwise flat stack: pass 1 rejects
        // it, pass 2 sees a zero-dispersion survivor set and stops. Two
        // passes — not three.
        let mut one_outlier: Vec<f32> = vec![100.0; 29];
        one_outlier.push(1000.0);
        let _ = take_rejection_counters();
        let (kept, _) = reject_linear_fit(&mut one_outlier, 5.0, 3.5);
        let (iters, _) = take_rejection_counters();
        assert_eq!(kept, 29, "the single outlier is the only sample rejected");
        assert_eq!(
            iters, 2,
            "pass 1 rejects, pass 2 confirms and exits — a third pass would mean the exit is gone"
        );

        // (b) A stack nothing is rejected from at all: the very first pass is
        // the confirming one, so the loop makes exactly ONE pass.
        let mut ramp: Vec<f32> = (0..20).map(|i| 100.0 + i as f32).collect();
        let n = ramp.len();
        let _ = take_rejection_counters();
        let (kept, _) = reject_linear_fit(&mut ramp, 5.0, 3.5);
        let (iters, _) = take_rejection_counters();
        assert_eq!(kept, n, "a clean ramp loses nothing");
        assert_eq!(iters, 1, "nothing rejected on pass 1 ⇒ no second pass");

        // (c) The same two implications over a seeded family, so the pin is
        // not two hand-picked fixtures: the loop always terminates by the
        // exit (never by `MAX_REJECTION_ITERS`), and a stack that loses
        // nothing costs exactly one pass.
        let mut saw_untouched = false;
        let mut saw_rejecting = false;
        for seed in 0..200u64 {
            let mut rng = SplitMix64(0x0C4E_11A7_0000_0001 ^ seed);
            let n = 20 + (rng.next_u64() % 60) as usize;
            let mut values: Vec<f32> = (0..n)
                .map(|_| (100.0 + 5.0 * next_gaussian(&mut rng)) as f32)
                .collect();
            if seed % 3 == 0 {
                values[0] = 400.0; // a hot pixel on a third of the stacks
            }
            let _ = take_rejection_counters();
            let (kept, _) = reject_linear_fit(&mut values, 5.0, 3.5);
            let (iters, _) = take_rejection_counters();
            assert!(
                iters >= 1 && (iters as usize) < MAX_REJECTION_ITERS,
                "seed {seed} n {n}: {iters} passes — the loop must end on its own exit"
            );
            if kept == n {
                saw_untouched = true;
                assert_eq!(iters, 1, "seed {seed} n {n}: nothing rejected ⇒ one pass");
            } else {
                saw_rejecting = true;
            }
        }
        assert!(
            saw_untouched,
            "the family must contain a stack nothing is rejected from"
        );
        assert!(
            saw_rejecting,
            "the family must contain a stack samples ARE rejected from"
        );
    }

    /// I5: `combine_pixel_weighted`'s third return value is `true` exactly
    /// for the rejection kinds whose survivors are known ascending by value
    /// (every algorithm but `None`/`SigmaClip`, which never sort `work`),
    /// and only once at least one sample survives.
    #[test]
    fn combine_pixel_weighted_reports_sortedness_per_rejection_kind() {
        let make_work = || -> Vec<(f32, u16)> {
            (0..24u16)
                .map(|i| {
                    (
                        100.0 + (i % 7) as f32 + if i == 20 { 500.0 } else { 0.0 },
                        i,
                    )
                })
                .collect()
        };
        let out_values: Vec<f32> = (0..24).map(|i| 100.0 + (i % 7) as f32).collect();
        let weights = vec![1.0f32; 24];
        let cases: [(Rejection, bool); 7] = [
            (Rejection::None, false),
            (
                Rejection::SigmaClip {
                    sigma_low: 3.0,
                    sigma_high: 3.0,
                },
                false,
            ),
            (
                Rejection::PercentileClip {
                    low: 0.1,
                    high: 0.1,
                },
                true,
            ),
            (
                Rejection::WinsorizedSigma {
                    sigma_low: 3.0,
                    sigma_high: 3.0,
                },
                true,
            ),
            (
                Rejection::LinearFitClip {
                    sigma_low: 5.0,
                    sigma_high: 3.5,
                },
                true,
            ),
            (Rejection::MinMax { low: 1, high: 1 }, true),
            (Rejection::Rcr { limit: 0.5 }, true),
        ];
        for (rejection, expect_sorted) in cases {
            let mut work = make_work();
            let mut mask = vec![0u64; mask_words(24)];
            let mut scratch = Vec::new();
            let (_, rejected, sorted) = combine_pixel_weighted(
                &mut work,
                &out_values,
                &weights,
                IntegrationRecipe::average(rejection),
                &mut mask,
                &mut scratch,
            );
            assert_eq!(
                sorted, expect_sorted,
                "{rejection:?}: sorted flag (rejected {rejected})"
            );
            if sorted {
                let kept = work.len() - rejected;
                assert!(
                    work[..kept].windows(2).all(|w| w[0].0 <= w[1].0),
                    "{rejection:?}: work[..kept] must be ascending by value when sorted=true"
                );
            }
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

    // ── Winsorized location/scale (M4c Task 2, ruling R-M4c-3) ──────────────

    /// Ruling R-M4c-3: the Winsorized estimator now follows the reference
    /// loop — `mu = median`, initial `sigma = 1.4826·MAD`, then 1.5-sigma
    /// Winsorization with a first-pass cutoff of 5 (a sample beyond
    /// `mu ± 5·sigma` goes to the CENTRE, not to the neighbouring threshold),
    /// `sigma = 1.134·stddev(v)`, `mu = mean(v)`, stopping at a 0.05 %
    /// change in `sigma` after at least two passes.
    ///
    /// Measured on the fixture below — 100 draws of N(0.1, 0.002), whose own
    /// sample scale is 0.00188190 (the draw runs 5.9 % under the nominal
    /// 0.002, which is ordinary for n = 100: the sampling scatter of a
    /// sample scale is `1/sqrt(2(n−1))` ≈ 7 %), plus 5 samples planted at
    /// +10 nominal sigma:
    ///
    /// | estimator | stack        | mu         | sigma      | passes |
    /// | --------- | ------------ | ---------- | ---------- | ------ |
    /// | reference | contaminated | 0.10017748 | 0.00172028 | 6      |
    /// | reference | clean        | 0.10015293 | 0.00181578 | 4      |
    /// | retired   | contaminated | 0.10030463 | 0.00201002 | (≤ 10) |
    /// | retired   | clean        | 0.10014012 | 0.00185773 | (≤ 10) |
    ///
    /// So the pin is NOT "sigma_w ≈ 0.002": neither estimator's fixed point
    /// is the nominal scale, and on this draw the retired one lands within
    /// 0.5 % of 0.002 purely by cancellation (its contamination bias of
    /// +8.2 % against its own clean value cancels the draw's −5.9 %
    /// deficit). What separates them is how far CONTAMINATION moves each
    /// estimator from its own clean answer: the reference loop −5.3 %, the
    /// retired one +8.2 %, and in opposite directions.
    ///
    /// The reference loop's downward move is by construction, not by error:
    /// the 5 contaminating samples are beyond `mu ± 5·sigma`, so the first
    /// pass maps them to the CENTRE, after which they contribute nothing to
    /// the winsorized variance while still counting in its `n − 1`
    /// denominator. The retired estimator instead started from the
    /// CONTAMINATED mean and standard deviation — 2.4× the true scale here —
    /// and its plain clamping pinned the outliers at `mu + 1.5·sigma` for
    /// good, where they propped up the very scale they were supposed to be
    /// measured against. That is why every Winsorized fingerprint moves in
    /// this task.
    ///
    /// The fixed-point tolerance is 0.1 % rather than exact bits: the
    /// fixture is generated through `ln`/`cos`, whose last ulp is allowed to
    /// differ between platforms. It is still three orders of magnitude
    /// tighter than the 17 % gap to the retired fixed point, so no change of
    /// start point, cutoff or convergence rule can slip through it.
    #[test]
    fn winsorized_location_scale_lands_on_the_reference_fixed_point() {
        const SEED: u64 = 0xD15_0001;
        let clean = fixture_gaussian_stack(100, 0.1, 0.002, SEED);
        let sd_clean = stddev(&clean[..], mean_f64(&clean[..]));
        assert!(
            (sd_clean - 0.001_881_90).abs() < 1e-6,
            "the fixture's own scale is part of the pin: {sd_clean}"
        );

        let mut contaminated = clean.clone();
        for _ in 0..5 {
            contaminated.push(sample_from(0.12)); // +10 nominal sigma
        }
        let mut scratch: Vec<f32> = Vec::new();
        let (mu, sigma, passes) = winsorized_location_scale(&contaminated[..], &mut scratch);
        let (mu_clean, sigma_clean, passes_clean) =
            winsorized_location_scale(&clean[..], &mut scratch);

        assert!(
            (mu - 0.1).abs() < 0.02 * 0.1,
            "mu_w {mu} must land within 2 % of 0.1 (contamination moved it by \
             {} of the clean answer {mu_clean})",
            (mu - mu_clean).abs() / mu_clean
        );
        assert!(
            (sigma - 0.001_720_28).abs() < 1e-3 * 0.001_720_28,
            "the contaminated fixed point moved: sigma_w {sigma}"
        );
        assert!(
            (sigma_clean - 0.001_815_78).abs() < 1e-3 * 0.001_815_78,
            "the clean fixed point moved: sigma_w {sigma_clean}"
        );
        assert!(
            (2..=6).contains(&passes) && (2..=6).contains(&passes_clean),
            "the loop must settle in 2..=6 passes, took {passes} / {passes_clean}"
        );

        // The retired estimator's fixed point on the SAME fixtures, so the
        // move every Winsorized fingerprint made is documented, not
        // discovered. Its (m, s) is order-dependent through the f64
        // summation, so it gets the sorted stacks it always got.
        let mut sorted_contaminated = contaminated.clone();
        sort_asc(&mut sorted_contaminated);
        let mut sorted_clean = clean.clone();
        sort_asc(&mut sorted_clean);
        let (_, old_sigma) = legacy_winsorized_location_scale(&sorted_contaminated);
        let (_, old_sigma_clean) = legacy_winsorized_location_scale(&sorted_clean);
        assert!(
            (old_sigma - 0.002_010_02).abs() < 1e-3 * 0.002_010_02,
            "the retired fixed point is recorded, not asserted into existence: {old_sigma}"
        );
        assert!(
            (sigma - sigma_clean).abs() < (old_sigma - old_sigma_clean).abs(),
            "contamination must move the reference loop LESS than it moved the retired \
             estimator: {} vs {}",
            (sigma - sigma_clean).abs(),
            (old_sigma - old_sigma_clean).abs()
        );
        assert!(
            sigma < sigma_clean && old_sigma > old_sigma_clean,
            "the reference loop errs low under contamination ({sigma} vs {sigma_clean}), the \
             retired estimator errs high ({old_sigma} vs {old_sigma_clean}) — the whole point \
             of the centre-mapping cutoff"
        );

        // All five planted outliers are rejected at 4.0/3.0, and the clean
        // stack keeps (nearly) everything.
        let mut work = planted_stack(100, 0.1, 0.002, SEED, &[0.12; 5]);
        let (kept, sorted_prefix) = reject_winsorized(&mut work, 4.0, 3.0);
        assert!(sorted_prefix, "the survivor prefix is left ascending");
        let kept_ids = survivors(&work, kept);
        for id in 100u16..105 {
            assert!(!kept_ids.contains(&id), "planted outlier {id} survived");
        }
        assert!(kept >= 97, "kept {kept} of 105 — the clean bulk must survive");

        let mut pure = planted_stack(100, 0.1, 0.002, SEED, &[]);
        let (kept_pure, _) = reject_winsorized(&mut pure, 4.0, 3.0);
        assert!(kept_pure >= 98, "a pure Gaussian may lose at most 2: kept {kept_pure}");
    }

    /// Ruling R-T2-1: a stack whose MAJORITY is tied has a zero MAD, which
    /// would seed the loop with `sigma = 0` and turn the whole rejection off
    /// — on the DEFAULT master recipe (`resolve_recipe` → Winsorized 3/3 for
    /// n ≥ 15) and in the Auto ladder's 8–19 band. Integer-ADU calibration
    /// stacks are exactly that shape, so the two cases below are the ones a
    /// bias master with a cosmic ray actually hits. The MAD fallback restores
    /// the retired estimator's answer on them.
    #[test]
    fn winsorized_rejects_the_outlier_of_a_majority_tied_stack() {
        // 15 samples at 500 ADU + one cosmic ray. Without the fallback:
        // 0 rejected and 781.25 combined (the outlier folded straight into
        // the master).
        let mut tied: Vec<f32> = vec![500.0; 15];
        tied.push(sample_from(5000.0));
        let (v, rejected) = combine_pixel(
            &mut tied.clone(),
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 3.0,
                sigma_high: 3.0,
            }),
        );
        assert_eq!(rejected, 1, "the cosmic ray must be rejected");
        assert!((v as f64 - 500.0).abs() < 1e-6, "combined {v}, want 500.0");

        // 9 tied at 500 (a bare majority of 16, so the MAD is still 0), 6
        // samples spread +-1..3 ADU around them, one cosmic ray. Without the
        // fallback: 0 rejected and 1031.25 combined.
        let mut mixed: Vec<f32> = vec![500.0; 9];
        for d in [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0] {
            mixed.push(sample_from(500.0 + d));
        }
        mixed.push(sample_from(9000.0));
        let (v2, rejected2) = combine_pixel(
            &mut mixed.clone(),
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 3.0,
                sigma_high: 3.0,
            }),
        );
        assert_eq!(rejected2, 1, "the cosmic ray must be rejected");
        assert!(
            (v2 as f64 - 500.0).abs() < 1e-6,
            "combined {v2}, want 500.0 (the 15 survivors are symmetric about it)"
        );

        // All-identical samples still reject nothing: there is no dispersion
        // to measure, by either seed.
        let mut flat: Vec<f32> = vec![500.0; 16];
        let (v3, rejected3) = combine_pixel(
            &mut flat,
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 3.0,
                sigma_high: 3.0,
            }),
        );
        assert_eq!(rejected3, 0);
        assert_eq!(v3, 500.0);
    }

    /// The control for the fallback above: a stack with a NON-zero MAD must
    /// be untouched by it. The two values are the ones `combine_pixel`
    /// returned BEFORE the fallback was added (commit 92bf2482) and after —
    /// identical, because the fallback is reachable only when the MAD is
    /// exactly 0.
    #[test]
    fn winsorized_mad_fallback_cannot_touch_a_non_degenerate_stack() {
        let clean = fixture_gaussian_stack(100, 0.1, 0.002, 0xD15_0001);
        let mut contaminated = clean.clone();
        for _ in 0..5 {
            contaminated.push(sample_from(0.12));
        }
        // The premise: this fixture's MAD is nowhere near zero.
        let mut devs: Vec<f32> = contaminated.iter().map(|&x| (x - 0.1).abs()).collect();
        let mad = median_in_place(&mut devs[..]);
        assert!(mad > 1e-4, "the control fixture must have a real MAD: {mad}");

        let (v, rejected) = combine_pixel(
            &mut contaminated.clone(),
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 4.0,
                sigma_high: 3.0,
            }),
        );
        assert_eq!(rejected, 6, "5 planted outliers + 1 clean tail sample");
        // 1e-7 rather than exact bits for the same reason as the fixed-point
        // pin: the fixture is generated through `ln`/`cos`. It is still four
        // orders of magnitude tighter than any change of seed could be.
        assert!(
            (v as f64 - 0.100_086_555).abs() < 1e-7,
            "the non-degenerate answer must not move: {v}"
        );
    }

    /// The all-rejected fallback survives the loop (it used to be reachable
    /// from a single pass). Zero thresholds leave the band `[mu_w, mu_w]`, so
    /// the FIRST iteration rejects everything before any survivor has been
    /// written — the full stack is still intact and `combine_pixel` answers
    /// with its median, the historical behaviour. (The late-iteration branch
    /// of the same guard is unreachable by construction here: once the
    /// median/MAD start is used, a scale small enough to empty a stack whose
    /// own median exists needs a threshold this degenerate, and that empties
    /// it on the first iteration. It mirrors `reject_sigma_clip`'s guard,
    /// which has its own pin.)
    #[test]
    fn winsorized_all_rejected_keeps_the_intact_stack_for_the_median() {
        let base = fixture_gaussian_stack(21, 100.0, 2.0, 0xD15_0002);
        let mut sorted = base.clone();
        sort_asc(&mut sorted);
        let expected = median_sorted(&sorted);
        let (v, rejected) = combine_pixel(
            &mut base.clone(),
            IntegrationRecipe::average(Rejection::WinsorizedSigma {
                sigma_low: 0.0,
                sigma_high: 0.0,
            }),
        );
        assert_eq!(rejected, 21, "the whole stack is rejected");
        assert_eq!(
            v.to_bits(),
            expected.to_bits(),
            "the fallback must be the median of the INTACT stack: {v} vs {expected}"
        );
    }

    // ── Legacy equivalence, bit-for-bit (spec §5) ───────────────────────────
    //
    // The old flat-`CombineMethod` implementations, replicated verbatim as a
    // recorded reference. Average+None must equal old `Mean` on the fixture
    // stack, to the bit.
    //
    // Average+WinsorizedSigma still matches old `WinsorizedSigmaClip` on that
    // same fixture, but INCIDENTALLY, not because the estimators agree: ruling
    // R-M4c-3 moved the winsorized fixed point (see the pin above), and on
    // this fixture both fixed points reject exactly the one planted outlier,
    // so both average the same 24 survivors. The test below asserts the
    // survivors AND asserts that the two estimators disagree, so it can never
    // quietly turn back into a claim of estimator equivalence.

    fn legacy_mean(values: &[f32]) -> f32 {
        (values.iter().map(|&x| x as f64).sum::<f64>() / values.len() as f64) as f32
    }

    /// The retired estimator's location/scale, lifted verbatim out of
    /// `legacy_winsorized` below (same f64 operations in the same order, so
    /// that function's recorded arithmetic is unchanged): the CONTAMINATED
    /// mean and standard deviation as the start point, plain clamping with no
    /// cutoff, a 0.5 % convergence threshold and a 10-pass cap.
    fn legacy_winsorized_location_scale(sorted: &[f32]) -> (f64, f64) {
        let n = sorted.len();
        let mut m = sorted.iter().map(|&x| x as f64).sum::<f64>() / n as f64;
        let mut s = stddev(sorted, m);
        let mut work: Vec<f64> = sorted.iter().map(|&x| x as f64).collect();
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
        (m, s)
    }

    fn legacy_winsorized(values: &[f32], sigma_low: f64, sigma_high: f64) -> (f32, usize) {
        let n = values.len();
        if n < 3 {
            return (legacy_mean(values), 0);
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let (m, s) = legacy_winsorized_location_scale(&sorted);
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
        // ... and it does so only because both fixed points reject the same
        // single outlier here. The estimators themselves are NOT the same any
        // more (ruling R-M4c-3): on this very fixture the retired start point
        // (the contaminated mean and standard deviation) lands somewhere else
        // entirely, which is what moves real master fingerprints.
        let mut sorted = base.clone();
        sort_asc(&mut sorted);
        let mut scratch: Vec<f32> = Vec::new();
        let (new_m, new_s, _) = winsorized_location_scale(&sorted[..], &mut scratch);
        let (old_m, old_s) = legacy_winsorized_location_scale(&sorted);
        // Measured: new (1002.0553, 3.99583) vs retired (1002.2742, 4.30444)
        // — the SCALE is 7.7 % higher on the retired side (the outlier's
        // leverage), the centres agree to 0.02 % (the outlier barely moves a
        // mean of clamped values either way). The scale is what decides
        // survival, so that is what this asserts.
        assert!(
            (new_s - old_s).abs() > 0.01 * old_s.abs(),
            "the two estimators must NOT agree: new ({new_m}, {new_s}) vs retired \
             ({old_m}, {old_s}) — if they do, this test is no longer pinning what it says"
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
            IntegrationRecipe::average(Rejection::MinMax { low: 1, high: 1 }),
            IntegrationRecipe::average(Rejection::Esd {
                outliers_fraction: 0.3,
                alpha: 0.05,
                low_relaxation: 1.5,
            }),
            IntegrationRecipe::average(Rejection::Rcr { limit: 0.5 }),
            IntegrationRecipe::median(Rejection::Rcr { limit: 0.5 }),
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
                let (v_w, rej_w, _) = combine_pixel_weighted(
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
        let (v, rej, _) = combine_pixel_weighted(
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
        let (v, rej, _) = combine_pixel_weighted(
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
        let (v, rejected, _) = combine_pixel_weighted(
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
        let (v, r, _) = combine_pixel_weighted(
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
    // ── Min/max, ESD and RCR (M4c Task 1, rulings R-M4c-1/2) ────────────────

    /// `n_clean` samples of N(`mean`, `sigma`) as `(value, frame index)`
    /// pairs, with `outliers` appended as explicitly placed samples — the
    /// pairs let a pin name WHICH frames a routine must reject, not just how
    /// many, so a routine that rejects the right COUNT of the wrong samples
    /// still fails.
    fn planted_stack(
        n_clean: usize,
        mean: f64,
        sigma: f64,
        seed: u64,
        outliers: &[f32],
    ) -> Vec<(f32, u16)> {
        let mut v = fixture_gaussian_stack(n_clean, mean, sigma, seed);
        v.extend(outliers.iter().map(|&x| sample_from(x)));
        v.into_iter().enumerate().map(|(i, x)| (x, i as u16)).collect()
    }

    /// The frame indices surviving in `work[..kept]`, ascending.
    fn survivors(work: &[(f32, u16)], kept: usize) -> Vec<u16> {
        let mut s: Vec<u16> = work[..kept].iter().map(|&(_, i)| i).collect();
        s.sort_unstable();
        s
    }

    #[test]
    fn min_max_drops_the_named_counts_and_never_the_whole_stack() {
        let mut v: Vec<(f32, u16)> = (0..20).map(|i| (i as f32, i as u16)).collect();
        let (kept, sorted) = reject_min_max(&mut v, 2, 3);
        assert_eq!((kept, sorted), (15, true));
        assert_eq!(survivors(&v, kept), (2u16..17).collect::<Vec<_>>());

        // 10 + 10 on a 20-sample stack would empty it; the clamp keeps one.
        let mut v2: Vec<(f32, u16)> = (0..20).map(|i| (i as f32, i as u16)).collect();
        let (kept2, _) = reject_min_max(&mut v2, 10, 10);
        assert_eq!(kept2, 1, "never every sample");
        assert_eq!(survivors(&v2, kept2), vec![10u16]);

        // 0/0 is a no-op that does not even sort.
        let mut v3: Vec<(f32, u16)> = vec![(3.0, 0), (1.0, 1), (2.0, 2)];
        assert_eq!(reject_min_max(&mut v3, 0, 0), (3, false));
        assert_eq!(v3[0], (3.0, 0), "an untouched stack keeps its order");

        // A request larger than the stack still leaves one sample.
        let mut v4: Vec<(f32, u16)> = vec![(1.0, 0), (2.0, 1), (3.0, 2)];
        assert_eq!(reject_min_max(&mut v4, 9, 9).0, 1);
    }

    #[test]
    fn esd_rejects_the_planted_outliers_and_spares_a_clean_gaussian() {
        // 60 samples of N(1000, 10) plus four planted at +6 sigma.
        let mut work = planted_stack(
            60,
            1000.0,
            10.0,
            0xE5D_0001,
            &[1060.0, 1062.0, 1064.0, 1066.0],
        );
        let (kept, sorted) = reject_esd(&mut work, 0.3, 0.05, 1.5);
        assert!(sorted, "the survivor prefix is left ascending");
        assert_eq!(kept, 60, "exactly the four planted outliers rejected");
        assert_eq!(
            survivors(&work, kept),
            (0u16..60).collect::<Vec<_>>(),
            "the four rejected frames must be the planted ones (60..64)"
        );

        // The same stack with no outliers keeps (nearly) everything.
        let mut clean = planted_stack(60, 1000.0, 10.0, 0xE5D_0002, &[]);
        let (kept_clean, _) = reject_esd(&mut clean, 0.3, 0.05, 1.5);
        assert!(kept_clean >= 58, "kept {kept_clean} of 60");

        // A degenerate configuration rejects nothing rather than everything.
        let mut degenerate = planted_stack(20, 1000.0, 10.0, 0xE5D_0003, &[]);
        assert_eq!(reject_esd(&mut degenerate, 0.0, 0.05, 1.5), (20, false));
        assert_eq!(reject_esd(&mut degenerate, 0.3, 0.0, 1.5), (20, false));
        assert_eq!(reject_esd(&mut degenerate, 0.3, 1.0, 1.5), (20, false));
        // Identical samples have no dispersion: nothing is extreme.
        let mut flat: Vec<(f32, u16)> = (0..12).map(|i| (7.0f32, i as u16)).collect();
        assert_eq!(reject_esd(&mut flat, 0.3, 0.05, 1.5).0, 12);
    }

    /// The low relaxation protects the faint side: with `rho = 1` a planted
    /// LOW outlier is rejected, with a large `rho` it survives.
    #[test]
    fn esd_low_relaxation_protects_the_faint_side() {
        let make = || planted_stack(60, 1000.0, 10.0, 0xE5D_0004, &[940.0]);
        let mut symmetric = make();
        let (kept_sym, _) = reject_esd(&mut symmetric, 0.3, 0.05, 1.0);
        assert!(
            !survivors(&symmetric, kept_sym).contains(&60),
            "rho = 1 must reject the low outlier"
        );
        let mut relaxed = make();
        let (kept_relaxed, _) = reject_esd(&mut relaxed, 0.3, 0.05, 3.0);
        assert!(
            survivors(&relaxed, kept_relaxed).contains(&60),
            "a large rho must spare it"
        );
    }

    #[test]
    fn rcr_rejects_the_planted_outliers_and_spares_a_clean_gaussian() {
        let mut work = planted_stack(
            60,
            1000.0,
            10.0,
            0x8C8_0001,
            &[1060.0, 1062.0, 1064.0, 1066.0],
        );
        let (kept, sorted) = reject_rcr(&mut work, 0.5);
        assert!(sorted, "the survivor prefix is left ascending");
        let kept_ids = survivors(&work, kept);
        for id in 60u16..64 {
            assert!(!kept_ids.contains(&id), "planted outlier {id} survived");
        }
        assert!(kept >= 57, "kept {kept} of 64 — the bulk must survive");

        let mut clean = planted_stack(60, 1000.0, 10.0, 0x8C8_0002, &[]);
        let (kept_clean, _) = reject_rcr(&mut clean, 0.5);
        // This bound has ZERO margin: the measured answer on this fixture
        // is exactly 57 of 60. That is deliberate — RCR is allowed to trim
        // the tails of a clean Gaussian and 3 of 60 is what it trims here —
        // but it means a failure of this line is not noise: the estimator
        // moved, and the next step is to find out how, not to lower the
        // bar.
        assert!(kept_clean >= 57, "kept {kept_clean} of 60");

        // Identical samples have no dispersion: nothing is extreme.
        let mut flat: Vec<(f32, u16)> = (0..12).map(|i| (7.0f32, i as u16)).collect();
        assert_eq!(reject_rcr(&mut flat, 0.5).0, 12);
        // Below three samples the test is not defined.
        assert_eq!(reject_rcr(&mut [(1.0f32, 0u16), (9.0, 1)], 0.5), (2, false));
    }

    /// A stricter `limit` rejects at least as much as a looser one (the
    /// direction of the knob, pinned so a sign error cannot pass).
    #[test]
    fn rcr_limit_is_monotone() {
        let base = planted_stack(80, 500.0, 4.0, 0x8C8_0003, &[]);
        let loose = reject_rcr(&mut base.clone(), 0.1).0;
        let chauvenet = reject_rcr(&mut base.clone(), 0.5).0;
        let strict = reject_rcr(&mut base.clone(), 1.0).0;
        assert!(loose >= chauvenet, "{loose} vs {chauvenet}");
        assert!(chauvenet >= strict, "{chauvenet} vs {strict}");
    }

    #[test]
    fn apply_rejection_dispatches_min_max_esd_and_rcr() {
        // 20 well-behaved samples ~100 + one hot 5000.
        let base: Vec<f32> = {
            let mut v: Vec<f32> = (0..20).map(|i| 100.0 + (i % 5) as f32).collect();
            v.push(5000.0);
            v
        };
        for rej in [
            Rejection::MinMax { low: 1, high: 1 },
            Rejection::Esd { outliers_fraction: 0.3, alpha: 0.05, low_relaxation: 1.5 },
            Rejection::Rcr { limit: 0.5 },
        ] {
            let (v, rejected) = combine_pixel(&mut base.clone(), IntegrationRecipe::average(rej));
            assert!(rejected >= 1, "{rej:?} must reject the hot sample");
            assert!((v - 102.0).abs() < 3.0, "{rej:?} combined near the clean mean, got {v}");
        }
        // MinMax { 0, 0 } routes to the routine but rejects nothing.
        let (v, rejected) = combine_pixel(
            &mut base.clone(),
            IntegrationRecipe::average(Rejection::MinMax { low: 0, high: 0 }),
        );
        assert_eq!(rejected, 0);
        assert_eq!(v.to_bits(), mean(&base).to_bits());
    }

    /// The persisted (master-recipe) JSON is snake_case and append-only:
    /// these three names are added, none of the existing five move.
    #[test]
    fn new_rejection_variants_round_trip_snake_case() {
        let cases = [
            (
                Rejection::MinMax { low: 1, high: 1 },
                serde_json::json!({"method": "min_max", "low": 1, "high": 1}),
            ),
            (
                Rejection::Esd { outliers_fraction: 0.3, alpha: 0.05, low_relaxation: 1.5 },
                serde_json::json!({
                    "method": "esd",
                    "outliers_fraction": 0.3,
                    "alpha": 0.05,
                    "low_relaxation": 1.5
                }),
            ),
            (
                Rejection::Rcr { limit: 0.5 },
                serde_json::json!({"method": "rcr", "limit": 0.5}),
            ),
        ];
        for (rej, wire) in cases {
            assert_eq!(serde_json::to_value(rej).unwrap(), wire, "{rej:?}");
            assert_eq!(serde_json::from_value::<Rejection>(wire.clone()).unwrap(), rej);
            let recipe = IntegrationRecipe::average(rej);
            assert_eq!(parse_recipe_value(&serde_json::to_value(recipe).unwrap()), Some(recipe));
        }
        assert_eq!(
            IntegrationRecipe::average(Rejection::MinMax { low: 2, high: 3 }).describe(),
            "Average | Min/max (2/3)"
        );
        assert_eq!(
            IntegrationRecipe::median(Rejection::Esd {
                outliers_fraction: 0.3,
                alpha: 0.05,
                low_relaxation: 1.5
            })
            .describe(),
            "Median | ESD (0.3/0.05/1.5)"
        );
        assert_eq!(
            IntegrationRecipe::average(Rejection::Rcr { limit: 0.5 }).describe(),
            "Average | RCR (0.5)"
        );
    }
}
