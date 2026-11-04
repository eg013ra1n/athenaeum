//! Robust location and scale statistics for frame normalization and
//! weighting (math reference §2.4–2.5, spec §5.1): the per-frame
//! `(scale, offset)` pairs the integration engine applies. The primitives they
//! read — median / MAD / average deviation / biweight midvariance evaluated
//! two-sided about the median, the 1/16 stratified pixel sample every caller
//! shares, the noise-scale factors of the classic SNR weight — live in
//! rustafits (`astroimage::stats`, moved verbatim with `star_measure`) and are
//! re-exported here, so every `integration::stats::…` path keeps working.

use serde::{Deserialize, Serialize};

pub use astroimage::stats::{
    avg_dev_about, bwmv_scale_about, clip_sample, for_each_stratified, location_scale, mad_about,
    median_in_place, median_of, noise_scale_factors, noise_scale_factors_within, stratified_sample,
    LocationScale, ScaleEstimator, AVGDEV_TO_SIGMA, BWMV_TO_SIGMA, CLIP_HI, CLIP_LO, MAD_TO_SIGMA,
    NOISE_CLIP_HI, NOISE_CLIP_LO, SAMPLE_STRIDE,
};

// The TypeScript face of `ScaleEstimator` (a rustafits type, which cannot derive `ts_rs::TS` here): the same names,
// exported as `ScaleEstimator`; the config fields that hold one point their `#[ts(as = …)]` at it. No `///` doc on
// purpose — ts-rs would copy it into `src/types`, which must stay unchanged.
#[derive(ts_rs::TS)]
#[ts(rename = "ScaleEstimator", rename_all = "camelCase")]
pub enum ScaleEstimatorTs {
    Bwmv,
    Mad,
    AvgDev,
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
///
/// **The default is `Local`** (owner ruling 2026-11-01, OD-1a). `ScaleZeroOffset`
/// scales each frame by its background dispersion, a per-frame scale error of
/// 10-20 % against the frame's star flux; where seeing varies from frame to
/// frame the Linear fit clip then cuts the sharpest frames' star cores (-29 %
/// star peak measured on the LDN1272 project master, hc11 G 0.975 of its
/// inputs' peak). `Local` normalizes by the star flux pair instead, and costs
/// the Normalize stage even when local *output* normalization is off. The
/// stored documents that still carry the old default are rewritten once by
/// `db::schema::migrate_rejection_normalization_local`; the Fast preview pins
/// `ScaleZeroOffset` (no Normalize stage, a preview's promise is speed).
/// Diagnosis: `docs/superpowers/research/2026-11-01-project-master-star-cores.md`;
/// the plan: `docs/superpowers/plans/2026-11-01-rejection-normalization-star-cores.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum RejectionNormalization {
    None,
    ScaleZeroOffset,
    EqualizeFluxes,
    #[default]
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
/// this pair is the FALLBACK only: a frame WITH a grid is overridden in the
/// engine by `local_for_rejection` (the real per-pixel local normalization,
/// `integration::engine::StackParams::local`), but a frame WITHOUT one —
/// every LN fallback trigger the group-level and per-frame LN error paths
/// can hit — integrates with whatever this function returns. Every one of
/// those fallback warnings says "continuing with global normalization", so
/// `Local` resolves to the SAME pair as `ScaleZeroOffset` here (additive
/// with scaling against the reference) — never `IDENTITY`, which would
/// silently drop rejection normalization instead of falling back to the
/// global one the warnings claim. This is the one enum variant whose
/// fallback is baked into the match arm rather than a separate config
/// field: local *output* normalization is a sibling boolean beside
/// `normalization.output`, so its own fallback reads `params.output[i]`
/// correctly, but local *rejection* is a variant of this very enum, so
/// selecting `Local` has no other value to fall back to.
pub fn rejection_pair(
    reference: LocationScale,
    frame: LocationScale,
    mode: RejectionNormalization,
) -> NormalizationPair {
    match mode {
        RejectionNormalization::None => NormalizationPair::IDENTITY,
        RejectionNormalization::ScaleZeroOffset | RejectionNormalization::Local => {
            output_pair(reference, frame, OutputNormalization::AdditiveWithScaling)
        }
        RejectionNormalization::EqualizeFluxes => {
            output_pair(reference, frame, OutputNormalization::Multiplicative)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            rejection_pair(r, f, RejectionNormalization::ScaleZeroOffset)
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
    fn local_rejection_normalization_falls_back_to_global_scale_zero_offset() {
        // C1: a frame with no LN grid must integrate under the same pair
        // ScaleZeroOffset would give it — never IDENTITY, which would
        // contradict every "continuing with global normalization" warning
        // in the LN fallback paths.
        let r = LocationScale {
            location: 0.32,
            scale: 0.05,
        };
        let f = LocationScale {
            location: 0.21,
            scale: 0.09,
        };
        let local = rejection_pair(r, f, RejectionNormalization::Local);
        let global = rejection_pair(r, f, RejectionNormalization::ScaleZeroOffset);
        assert_eq!(local, global);
        assert_ne!(local, NormalizationPair::IDENTITY);
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
        // Owner ruling 2026-11-01 (OD-1a): the shipped default is `local`.
        assert_eq!(
            RejectionNormalization::default(),
            RejectionNormalization::Local
        );
    }
}
