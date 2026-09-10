//! Robust location and scale statistics for frame normalization and
//! weighting (math reference §2.4–2.5, spec §5.1): median / MAD / average
//! deviation / biweight midvariance evaluated two-sided about the median,
//! the 1/16 stratified pixel sample every caller shares, the noise-scale
//! factors of the classic SNR weight, and the per-frame `(scale, offset)`
//! pairs the integration engine applies. Pure functions over `f32` samples.

use serde::{Deserialize, Serialize};

/// σ-consistency factor for the median absolute deviation.
pub const MAD_TO_SIGMA: f32 = 1.4826;
/// σ-consistency factor for the average absolute deviation (√(π/2)).
pub const AVGDEV_TO_SIGMA: f32 = 1.2533;
/// σ-consistency factor applied to √BWMV.
pub const BWMV_TO_SIGMA: f32 = 0.991;
/// Samples outside `(CLIP_LO, CLIP_HI)` are excluded from scale estimates:
/// zeros mark missing coverage, the top value marks saturation.
pub const CLIP_LO: f32 = 1.0 / 65535.0;
pub const CLIP_HI: f32 = 1.0 - 1.0 / 65535.0;
/// The tighter clip the noise-scale factors use (§2.5).
pub const NOISE_CLIP_LO: f32 = 2.0 / 65535.0;
pub const NOISE_CLIP_HI: f32 = 1.0 - 2.0 / 65535.0;
/// Stride of the stratified sample: one pixel in `STRIDE²` = 1/16.
pub const SAMPLE_STRIDE: usize = 4;

/// Median with the even-`n` convention "mean of the central two". Reorders
/// `values`; the caller has removed NaNs. NaN for an empty slice.
pub fn median_in_place(values: &mut [f32]) -> f32 {
    let n = values.len();
    if n == 0 {
        return f32::NAN;
    }
    let mid = n / 2;
    let (_, hi, _) = values.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let hi = *hi;
    if n % 2 == 1 {
        hi
    } else {
        let lo = values[..mid]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        0.5 * (lo + hi)
    }
}

pub fn median_of(values: &[f32]) -> f32 {
    let mut v = values.to_vec();
    median_in_place(&mut v)
}

/// Raw median absolute deviation about `m` (no consistency factor).
pub fn mad_about(values: &[f32], m: f32) -> f32 {
    let mut d: Vec<f32> = values.iter().map(|&x| (x - m).abs()).collect();
    median_in_place(&mut d)
}

/// Mean absolute deviation about `m`.
pub fn avg_dev_about(values: &[f32], m: f32) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    let s: f64 = values.iter().map(|&x| (x - m).abs() as f64).sum();
    (s / values.len() as f64) as f32
}

/// σ-consistent biweight midvariance scale about `m` (Wilcox 2012
/// §3.12.1): `u = (x − m)/(9·MAD)`, `BWMV = n·Σ_{|u|<1}(x − m)²(1 − u²)⁴ /
/// [Σ_{|u|<1}(1 − u²)(1 − 5u²)]²`, scale `= √BWMV · 0.991`. `mad` is the
/// raw MAD about `m`. Falls back to the MAD scale when no sample lies in
/// the biweight window or the denominator is not positive.
pub fn bwmv_scale_about(values: &[f32], m: f32, mad: f32) -> f32 {
    let n = values.len();
    if n == 0 {
        return f32::NAN;
    }
    if !(mad > 0.0) {
        return 0.0;
    }
    let w = 9.0 * mad as f64;
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for &x in values {
        let d = (x - m) as f64;
        let u2 = (d / w) * (d / w);
        if u2 < 1.0 {
            let a = 1.0 - u2;
            num += d * d * a.powi(4);
            den += a * (1.0 - 5.0 * u2);
        }
    }
    if den <= 0.0 || num <= 0.0 {
        return mad * MAD_TO_SIGMA;
    }
    let bwmv = n as f64 * num / (den * den);
    (bwmv.sqrt() as f32) * BWMV_TO_SIGMA
}

fn stddev_about_mean(v: &[f32]) -> f64 {
    let n = v.len();
    if n < 2 {
        return 0.0;
    }
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n as f64;
    let ss = v
        .iter()
        .map(|&x| {
            let d = x as f64 - mean;
            d * d
        })
        .sum::<f64>();
    (ss / (n - 1) as f64).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ScaleEstimator {
    #[default]
    Bwmv,
    Mad,
    AvgDev,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationScale {
    pub location: f32,
    pub scale: f32,
}

/// Keep the finite samples strictly inside `(lo, hi)`.
pub fn clip_sample(values: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > lo && *v < hi)
        .collect()
}

fn side_scale(side: &[f32], m: f32, est: ScaleEstimator) -> Option<f32> {
    if side.len() < 2 {
        return None;
    }
    Some(match est {
        ScaleEstimator::Mad => mad_about(side, m) * MAD_TO_SIGMA,
        ScaleEstimator::AvgDev => avg_dev_about(side, m) * AVGDEV_TO_SIGMA,
        ScaleEstimator::Bwmv => bwmv_scale_about(side, m, mad_about(side, m)),
    })
}

/// Location (median) and two-sided scale of an already clipped sample: the
/// estimator runs separately on `x ≤ m` and `x > m` and the two are
/// averaged; an empty side defers to the other. `None` below 4 samples.
pub fn location_scale(values: &[f32], est: ScaleEstimator) -> Option<LocationScale> {
    if values.len() < 4 {
        return None;
    }
    let m = median_of(values);
    let (low, high): (Vec<f32>, Vec<f32>) = values.iter().partition(|&&x| x <= m);
    let scale = match (side_scale(&low, m, est), side_scale(&high, m, est)) {
        (Some(a), Some(b)) => 0.5 * (a + b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return None,
    };
    Some(LocationScale { location: m, scale })
}

/// Visit the 1/16 stratified pixel indices: rows `0, 4, 8, …`; in row `y`
/// the columns start at `(y / 4) % 4` and step by 4, so the four row phases
/// cover all four column phases.
pub fn for_each_stratified(width: usize, height: usize, mut f: impl FnMut(usize)) {
    let mut y = 0;
    while y < height {
        let mut x = (y / SAMPLE_STRIDE) % SAMPLE_STRIDE;
        while x < width {
            f(y * width + x);
            x += SAMPLE_STRIDE;
        }
        y += SAMPLE_STRIDE;
    }
}

/// The finite values at the stratified indices of a plane.
pub fn stratified_sample(data: &[f32], width: usize, height: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(data.len() / (SAMPLE_STRIDE * SAMPLE_STRIDE) + 1);
    for_each_stratified(width, height, |i| {
        let v = data[i];
        if v.is_finite() {
            out.push(v);
        }
    });
    out
}

/// Noise scaling factors (math reference §2.5): `(σ_low, σ_high)` — the
/// standard deviation of each side of the clipped sample after two
/// restrictions to `[c − 4σ, c]` / `[c, c + 4σ]` about the median `c`.
/// `None` below 8 samples or when a side empties.
pub fn noise_scale_factors(values: &[f32]) -> Option<(f32, f32)> {
    let v = clip_sample(values, NOISE_CLIP_LO, NOISE_CLIP_HI);
    if v.len() < 8 {
        return None;
    }
    let c = median_of(&v);
    let mut low: Vec<f32> = v.iter().copied().filter(|&x| x <= c).collect();
    let mut high: Vec<f32> = v.iter().copied().filter(|&x| x >= c).collect();
    for _ in 0..2 {
        let lo = (c as f64 - 4.0 * stddev_about_mean(&low)).max(NOISE_CLIP_LO as f64) as f32;
        low.retain(|&x| x >= lo);
        let hi = (c as f64 + 4.0 * stddev_about_mean(&high)).min(NOISE_CLIP_HI as f64) as f32;
        high.retain(|&x| x <= hi);
    }
    if low.len() < 2 || high.len() < 2 {
        return None;
    }
    Some((
        stddev_about_mean(&low) as f32,
        stddev_about_mean(&high) as f32,
    ))
}

/// Output normalization modes (spec §5.1, math reference §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum OutputNormalization {
    None,
    Additive,
    #[default]
    AdditiveWithScaling,
    Multiplicative,
    MultiplicativeWithScaling,
}

/// Rejection normalization modes (math reference §3.3); `Local` carries
/// per-frame grids instead of a pair (M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum RejectionNormalization {
    None,
    #[default]
    ScaleZeroOffset,
    EqualizeFluxes,
    Local,
}

/// `v′ = v · scale + offset`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizationPair {
    pub scale: f32,
    pub offset: f32,
}

impl NormalizationPair {
    pub const IDENTITY: NormalizationPair = NormalizationPair {
        scale: 1.0,
        offset: 0.0,
    };

    #[inline]
    pub fn apply(&self, v: f32) -> f32 {
        v * self.scale + self.offset
    }
}

fn safe_ratio(num: f32, den: f32) -> f32 {
    if den != 0.0 && num.is_finite() && den.is_finite() {
        num / den
    } else {
        1.0
    }
}

/// The pair that maps a frame with statistics `frame` onto `reference`
/// (`m` = location, `s` = scale, index 0 = reference, i = frame):
/// additive `x + (m₀ − mᵢ)`; additive with scaling `(x − mᵢ)·(s₀/sᵢ) + m₀`;
/// multiplicative `x·m₀/mᵢ`; multiplicative with scaling `(x/mᵢ)·(s₀/sᵢ)·m₀`.
pub fn output_pair(
    reference: LocationScale,
    frame: LocationScale,
    mode: OutputNormalization,
) -> NormalizationPair {
    let needs_scale = matches!(
        mode,
        OutputNormalization::AdditiveWithScaling | OutputNormalization::MultiplicativeWithScaling
    );
    let needs_location = matches!(
        mode,
        OutputNormalization::Multiplicative | OutputNormalization::MultiplicativeWithScaling
    );
    let scale_unusable = !(reference.scale > 0.0) || !reference.scale.is_finite();
    let location_unusable = !(reference.location > 0.0) || !reference.location.is_finite();
    if (needs_scale && scale_unusable) || (needs_location && location_unusable) {
        tracing::warn!(
            location = reference.location,
            scale = reference.scale,
            "reference statistics unusable; identity normalization"
        );
        return NormalizationPair::IDENTITY;
    }
    let (m0, s0, mi, si) = (
        reference.location,
        reference.scale,
        frame.location,
        frame.scale,
    );
    match mode {
        OutputNormalization::None => NormalizationPair::IDENTITY,
        OutputNormalization::Additive => NormalizationPair {
            scale: 1.0,
            offset: m0 - mi,
        },
        OutputNormalization::AdditiveWithScaling => {
            let k = safe_ratio(s0, si);
            NormalizationPair {
                scale: k,
                offset: m0 - mi * k,
            }
        }
        OutputNormalization::Multiplicative => NormalizationPair {
            scale: safe_ratio(m0, mi),
            offset: 0.0,
        },
        OutputNormalization::MultiplicativeWithScaling => NormalizationPair {
            scale: safe_ratio(s0, si) * safe_ratio(m0, mi),
            offset: 0.0,
        },
    }
}

/// The pair applied to the working copy before rejection. For `Local` (M2)
/// this is explicitly `NormalizationPair::IDENTITY`: the real per-pixel
/// local normalization comes from the caller's LN grids
/// (`integration::engine::StackParams::local`, applied when
/// `local_for_rejection` is set), not from this pair — this is the pair a
/// frame with no grid falls back to (ruling R2: a frame with no grid is
/// effectively un-normalized for rejection, not refused).
pub fn rejection_pair(
    reference: LocationScale,
    frame: LocationScale,
    mode: RejectionNormalization,
) -> NormalizationPair {
    match mode {
        RejectionNormalization::None => NormalizationPair::IDENTITY,
        RejectionNormalization::ScaleZeroOffset => {
            output_pair(reference, frame, OutputNormalization::AdditiveWithScaling)
        }
        RejectionNormalization::EqualizeFluxes => {
            output_pair(reference, frame, OutputNormalization::Multiplicative)
        }
        RejectionNormalization::Local => NormalizationPair::IDENTITY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::add_noise;

    fn gaussian(n: usize, sigma: f32, seed: u64) -> Vec<f32> {
        let mut v = vec![0.5f32; n];
        add_noise(&mut v, sigma, seed);
        v
    }
    fn close(a: f32, b: f32, rel: f32) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    #[test]
    fn median_conventions() {
        assert_eq!(median_of(&[5.0, 1.0, 4.0, 2.0, 3.0]), 3.0);
        assert_eq!(median_of(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median_of(&[7.0]), 7.0);
        assert!(median_of(&[]).is_nan());
    }

    #[test]
    fn mad_and_avg_dev_by_hand() {
        let v = [1.0, 2.0, 3.0, 4.0, 100.0];
        let m = median_of(&v);
        assert_eq!(m, 3.0);
        assert_eq!(mad_about(&v, m), 1.0);
        assert!((avg_dev_about(&v, m) - 101.0 / 5.0).abs() < 1e-5);
    }

    #[test]
    fn estimators_are_sigma_consistent_on_gaussian_noise() {
        let v = gaussian(20_000, 0.01, 1);
        let m = median_of(&v);
        let mad = mad_about(&v, m);
        assert!(close(mad * MAD_TO_SIGMA, 0.01, 0.03));
        assert!(close(avg_dev_about(&v, m) * AVGDEV_TO_SIGMA, 0.01, 0.03));
        assert!(close(bwmv_scale_about(&v, m, mad), 0.01, 0.03));
        for est in [
            ScaleEstimator::Bwmv,
            ScaleEstimator::Mad,
            ScaleEstimator::AvgDev,
        ] {
            let ls = location_scale(&v, est).unwrap();
            assert!(
                close(ls.location, 0.5, 0.001),
                "{est:?} location {}",
                ls.location
            );
            assert!(close(ls.scale, 0.01, 0.03), "{est:?} scale {}", ls.scale);
        }
        assert!(location_scale(&[0.1, 0.2, 0.3], ScaleEstimator::Mad).is_none());
    }

    #[test]
    fn robust_scales_ignore_symmetric_outliers() {
        let mut v = gaussian(20_000, 0.01, 2);
        for i in 0..200 {
            v[i * 100] = 0.5 + 0.2; // 1 % at +20σ
            v[i * 100 + 50] = 0.5 - 0.2; // 1 % at −20σ
        }
        let m = median_of(&v);
        let mad = mad_about(&v, m);
        assert!(close(mad * MAD_TO_SIGMA, 0.01, 0.05));
        assert!(close(bwmv_scale_about(&v, m, mad), 0.01, 0.05));
        let sd = stddev_about_mean(&v) as f32;
        assert!(
            sd > 0.025,
            "plain stddev must be blown up by the outliers: {sd}"
        );
        assert!(close(
            location_scale(&v, ScaleEstimator::Bwmv).unwrap().scale,
            0.01,
            0.05
        ));
        assert!(close(
            location_scale(&v, ScaleEstimator::Mad).unwrap().scale,
            0.01,
            0.05
        ));
    }

    #[test]
    fn two_sided_equals_one_sided_on_symmetric_data() {
        let v = gaussian(20_000, 0.02, 3);
        let m = median_of(&v);
        let one = mad_about(&v, m) * MAD_TO_SIGMA;
        let two = location_scale(&v, ScaleEstimator::Mad).unwrap().scale;
        assert!(close(two, one, 0.03));
    }

    #[test]
    fn clip_drops_zeros_saturation_and_nan() {
        let v = [0.0, 0.5, 1.0, f32::NAN, 0.25];
        assert_eq!(clip_sample(&v, CLIP_LO, CLIP_HI), vec![0.5, 0.25]);
    }

    #[test]
    fn stratified_sample_counts() {
        let d = vec![1.0f32; 16 * 16];
        assert_eq!(stratified_sample(&d, 16, 16).len(), 16);
        let d = vec![1.0f32; 17 * 9];
        assert_eq!(stratified_sample(&d, 17, 9).len(), 13);
        let mut d = vec![1.0f32; 8 * 8];
        d[0] = f32::NAN;
        assert_eq!(stratified_sample(&d, 8, 8).len(), 3);
        let mut seen = Vec::new();
        for_each_stratified(8, 8, |i| seen.push(i));
        assert_eq!(seen, vec![0, 4, 33, 37]);
    }

    #[test]
    fn noise_scale_factors_are_symmetric_on_gaussian_noise() {
        let v = gaussian(50_000, 0.01, 4);
        let (lo, hi) = noise_scale_factors(&v).unwrap();
        assert!(lo > 0.0046 && lo < 0.0062, "{lo}");
        assert!(hi > 0.0046 && hi < 0.0062, "{hi}");
        assert!((lo - hi).abs() < 0.0005);
        assert!(noise_scale_factors(&[0.5; 4]).is_none());
    }

    #[test]
    fn normalization_pairs_by_hand() {
        let r = LocationScale {
            location: 0.10,
            scale: 0.02,
        };
        let f = LocationScale {
            location: 0.15,
            scale: 0.04,
        };
        let p = output_pair(r, f, OutputNormalization::AdditiveWithScaling);
        assert!((p.scale - 0.5).abs() < 1e-6 && (p.offset - 0.025).abs() < 1e-6);
        assert!((p.apply(0.15) - 0.10).abs() < 1e-6);
        let p = output_pair(r, f, OutputNormalization::Additive);
        assert_eq!(p.scale, 1.0);
        assert!((p.offset + 0.05).abs() < 1e-6);
        let p = output_pair(r, f, OutputNormalization::Multiplicative);
        assert!((p.scale - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(p.offset, 0.0);
        let p = output_pair(r, f, OutputNormalization::MultiplicativeWithScaling);
        assert!((p.scale - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(p.offset, 0.0);
        assert_eq!(
            output_pair(r, f, OutputNormalization::None),
            NormalizationPair::IDENTITY
        );
        assert_eq!(
            rejection_pair(r, f, RejectionNormalization::ScaleZeroOffset),
            output_pair(r, f, OutputNormalization::AdditiveWithScaling)
        );
        assert_eq!(
            rejection_pair(r, f, RejectionNormalization::EqualizeFluxes),
            output_pair(r, f, OutputNormalization::Multiplicative)
        );
        assert_eq!(
            rejection_pair(r, f, RejectionNormalization::None),
            NormalizationPair::IDENTITY
        );
        assert_eq!(
            rejection_pair(r, f, RejectionNormalization::Local),
            NormalizationPair::IDENTITY
        );
        let zero = LocationScale {
            location: 0.0,
            scale: 0.0,
        };
        assert_eq!(
            output_pair(r, zero, OutputNormalization::AdditiveWithScaling).scale,
            1.0
        );
        assert_eq!(
            output_pair(zero, f, OutputNormalization::AdditiveWithScaling),
            NormalizationPair::IDENTITY
        );
        assert_eq!(
            output_pair(zero, f, OutputNormalization::Multiplicative),
            NormalizationPair::IDENTITY
        );
        assert_eq!(
            output_pair(zero, f, OutputNormalization::Additive).offset,
            -0.15
        );
    }

    #[test]
    fn serde_names_match_the_spec() {
        assert_eq!(
            serde_json::to_string(&ScaleEstimator::AvgDev).unwrap(),
            "\"avgDev\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleEstimator::Bwmv).unwrap(),
            "\"bwmv\""
        );
        assert_eq!(
            serde_json::to_string(&OutputNormalization::AdditiveWithScaling).unwrap(),
            "\"additiveWithScaling\""
        );
        assert_eq!(
            serde_json::to_string(&RejectionNormalization::ScaleZeroOffset).unwrap(),
            "\"scaleZeroOffset\""
        );
        let back: OutputNormalization =
            serde_json::from_str("\"multiplicativeWithScaling\"").unwrap();
        assert_eq!(back, OutputNormalization::MultiplicativeWithScaling);
        assert_eq!(ScaleEstimator::default(), ScaleEstimator::Bwmv);
        assert_eq!(
            OutputNormalization::default(),
            OutputNormalization::AdditiveWithScaling
        );
        assert_eq!(
            RejectionNormalization::default(),
            RejectionNormalization::ScaleZeroOffset
        );
    }
}
