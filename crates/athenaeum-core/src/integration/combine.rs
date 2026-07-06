//! Per-pixel robust integration recipes — PixInsight's two-axis model
//! (Combination × Rejection). Spec:
//! `docs/superpowers/specs/2026-07-06-master-integration-pi-model-design.md`.
//!
//! An [`IntegrationRecipe`] pairs a [`Combination`] (Average or Median of the
//! surviving samples) with a [`Rejection`] algorithm that decides which
//! samples survive first. Rejection runs per pixel stack; the combination
//! then applies to the survivors — every rejection composes with either
//! combination (PI semantics).
//!
//! The pre-2026-07-06 flat `CombineMethod` enum is retained only as a private,
//! deserialize-only [`LegacyCombineMethod`] so old `recipe_json` blobs still
//! parse (spec §3). Its equivalences: `Mean` = Average+None, `Median` =
//! Median+None, `WinsorizedSigmaClip` = Average+WinsorizedSigma,
//! `PercentileClip` = Average+PercentileClip — pinned bit-for-bit by the tests.

use serde::{Deserialize, Serialize};

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

    /// Human summary — "Average · Winsorized sigma (3.0/3.0)" style (spec §4).
    pub fn describe(&self) -> String {
        format!("{} · {}", self.combination.label(), self.rejection.label())
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
    /// Least-squares line fit over (rank, value); reject samples whose
    /// residual falls outside [−sigma_low·d, +sigma_high·d] where d is the
    /// mean absolute deviation of the residuals; refit and repeat until
    /// stable. PI-recommended for larger sets with drifting illumination.
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

fn mean_f64(v: &[f32]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64
}

fn mean(v: &[f32]) -> f32 {
    mean_f64(v) as f32
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

fn sort_asc(v: &mut [f32]) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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

/// Runs the chosen rejection algorithm in place, returning
/// `(surviving_count, prefix_is_sorted_ascending)`.
fn apply_rejection(values: &mut [f32], rejection: Rejection) -> (usize, bool) {
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

fn reject_percentile(values: &mut [f32], low: f64, high: f64) -> (usize, bool) {
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
        let xf = values[r] as f64;
        let dev = (xf - m) / m.abs();
        let reject = (dev < 0.0 && -dev > low) || (dev > 0.0 && dev > high);
        if !reject {
            values[w] = values[r];
            w += 1;
        }
    }
    (w, true)
}

fn reject_sigma_clip(values: &mut [f32], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
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
            let xf = values[r] as f64;
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

fn reject_winsorized(values: &mut [f32], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    sort_asc(values);
    // 1) Winsorized estimate of location/scale (Huber-style iteration): clamp
    //    the working copy at m±1.5σ, recompute, repeat to 0.5% change. This
    //    block is byte-identical to the legacy `WinsorizedSigmaClip` estimator
    //    so Average+WinsorizedSigma reproduces the old master exactly.
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
        let xf = values[r] as f64;
        if xf >= lo && xf <= hi {
            values[w] = values[r];
            w += 1;
        }
    }
    (w, true)
}

fn reject_linear_fit(values: &mut [f32], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
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
        // Least-squares line y = a + b·i over (rank i, value) for i in 0..k.
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
            break; // degenerate — can't fit a line
        }
        let b = (kf * sxy - sx * sy) / denom;
        let a = (sy - b * sx) / kf;
        // Residual dispersion = mean absolute deviation of residuals.
        let mut abs_sum = 0.0;
        for i in 0..k {
            let resid = values[i] as f64 - (a + b * i as f64);
            abs_sum += resid.abs();
        }
        let d = abs_sum / kf;
        // Scale-relative zero-dispersion guard: on a perfectly (or near-)
        // linear stack the residuals are floating-point noise, not signal —
        // treat that as "no rejection" so a clean ramp is never eaten. Real
        // dispersion (read noise, drift) is orders of magnitude above this.
        let scale = (sabs_y / kf).max(1.0);
        if d <= 1e-9 * scale {
            break;
        }
        let lo = -sigma_low * d;
        let hi = sigma_high * d;
        let mut w = 0usize;
        for i in 0..k {
            let resid = values[i] as f64 - (a + b * i as f64);
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
        let mut ramp: Vec<f32> = (0..20).map(|i| 100.0 + 5.0 * i as f32).collect();
        ramp[10] = 100_000.0; // one gross spike in the middle of the ramp
        let (v, rej) = combine_pixel(
            &mut ramp,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
        );
        assert!(rej >= 1, "spike must be rejected");
        assert!(v < 1000.0, "combined value should not be dragged up by the spike, got {v}");
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
        // least-squares line over this ramp-like stack leaves every residual
        // outside the tight ±0.5σ band, so iteration 1 rejects ALL 12 at once
        // (w=0) BEFORE any in-place compaction — the array is never corrupted
        // here. With the w==0 guard, kept stays at the initial 12 and the
        // survivors (== the intact stack) average to 5.0. (Pre-fix, kept fell to
        // 0 and the fallback median of the still-intact stack was also 5.0 —
        // value unchanged, now sourced from a real combine over survivors.)
        let mut stack = vec![0.0, 0.0, 0.0, 4.0, 4.0, 4.0, 6.0, 6.0, 6.0, 10.0, 10.0, 10.0];
        let (v, rej) = combine_pixel(
            &mut stack,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 0.5, sigma_high: 0.5 }),
        );
        assert_eq!(v, 5.0, "intact-stack combine, no corruption possible");
        assert_eq!(rej, 0, "guard keeps the full stack when iter 1 rejects everything");
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
        assert_eq!(describe_recipe_json(&legacy), "Average · Winsorized sigma (3.0/3.0)");

        // New blob (combine holds an IntegrationRecipe).
        let recipe = IntegrationRecipe::median(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 });
        let new_blob = serde_json::json!({ "combine": recipe }).to_string();
        assert_eq!(describe_recipe_json(&new_blob), "Median · Linear fit clip (5.0/3.5)");

        // Unparseable → raw passthrough (nothing lost).
        assert_eq!(describe_recipe_json("not json at all"), "not json at all");
    }
}
