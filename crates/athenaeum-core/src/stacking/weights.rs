//! Frame weights, selection and reference choice (spec §4.2–4.4, math
//! reference §1.2, §1.5, §2.5, §3.7). Weights are per channel and
//! normalized by the group maximum per channel; the frame-level weight is
//! the mean over channels.

use serde::{Deserialize, Serialize};

use super::measure::{ChannelMeasurement, FrameMeasurement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum WeightMode {
    #[default]
    PsfSignalWeight,
    PsfSnr,
    /// `(noiseScale / σ_N)²` with `noiseScale` the mean of the two noise-scale factors.
    Noise,
    /// The classic formula with the user's `FormulaWeights`.
    Formula,
    Exposure,
    Keyword,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FormulaWeights {
    pub fwhm: f64,
    pub eccentricity: f64,
    pub snr: f64,
    pub stars: f64,
    pub pedestal: f64,
}

impl Default for FormulaWeights {
    fn default() -> Self {
        FormulaWeights {
            fwhm: 15.0,
            eccentricity: 15.0,
            snr: 20.0,
            stars: 0.0,
            pedestal: 50.0,
        }
    }
}

pub struct WeightInput<'a> {
    pub measurement: &'a FrameMeasurement,
    /// `EXPTIME`, for `WeightMode::Exposure`.
    pub exposure_s: Option<f64>,
    /// The configured keyword's value, for `WeightMode::Keyword`.
    pub keyword_value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameWeight {
    /// Raw weight per channel.
    pub channels: Vec<f64>,
    /// `channels / max over the group's non-excluded frames`, per channel.
    pub normalized: Vec<f64>,
    pub mean: f64,
    pub normalized_mean: f64,
    /// Why this frame has no usable weight (a missing exposure or keyword).
    pub missing: Option<String>,
}

#[derive(Clone, Copy)]
struct Range {
    lo: f64,
    hi: f64,
}

impl Range {
    fn empty() -> Self {
        Range {
            lo: f64::INFINITY,
            hi: f64::NEG_INFINITY,
        }
    }
    fn add(&mut self, v: f64) {
        self.lo = self.lo.min(v);
        self.hi = self.hi.max(v);
    }
    /// `(v − lo)/(hi − lo)`; a degenerate range scores full credit.
    fn credit_up(&self, v: f64) -> f64 {
        if self.hi > self.lo {
            ((v - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
    /// `1 − (v − lo)/(hi − lo)`; a degenerate range scores full credit.
    fn credit_down(&self, v: f64) -> f64 {
        if self.hi > self.lo {
            (1.0 - (v - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

#[derive(Clone, Copy)]
struct FormulaRanges {
    fwhm: Range,
    ecc: Range,
    snr: Range,
    stars: Range,
}

fn formula_ranges(inputs: &[WeightInput], excluded: &[bool], nch: usize) -> Vec<FormulaRanges> {
    let mut out = vec![
        FormulaRanges {
            fwhm: Range::empty(),
            ecc: Range::empty(),
            snr: Range::empty(),
            stars: Range::empty()
        };
        nch
    ];
    for (i, inp) in inputs.iter().enumerate() {
        if excluded.get(i).copied().unwrap_or(false) {
            continue;
        }
        for (c, ch) in inp.measurement.channels.iter().enumerate().take(nch) {
            out[c].fwhm.add(ch.fwhm_px);
            out[c].ecc.add(ch.eccentricity);
            out[c].snr.add(ch.snr_weight);
            out[c].stars.add(ch.stars_fitted as f64);
        }
    }
    out
}

/// `W = A·(1 − ΔFWHM) + B·(1 − ΔEcc) + C·ΔSNRW + D·ΔStars + P` with each Δ
/// normalized to the group range (math reference §1.5).
fn formula_weight(ch: &ChannelMeasurement, f: &FormulaWeights, r: &FormulaRanges) -> f64 {
    f.fwhm * r.fwhm.credit_down(ch.fwhm_px)
        + f.eccentricity * r.ecc.credit_down(ch.eccentricity)
        + f.snr * r.snr.credit_up(ch.snr_weight)
        + f.stars * r.stars.credit_up(ch.stars_fitted as f64)
        + f.pedestal
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// Weights for one group. `excluded[i]` marks frames that must not set the
/// per-channel maximum (manual exclusions); their weights are still
/// computed. Non-finite or non-positive raw weights become 0.
pub fn compute_weights(
    inputs: &[WeightInput],
    mode: WeightMode,
    formula: &FormulaWeights,
    excluded: &[bool],
) -> Vec<FrameWeight> {
    let nch = inputs
        .iter()
        .map(|i| i.measurement.channels.len())
        .max()
        .unwrap_or(0);
    if nch > 0 && inputs.iter().any(|i| i.measurement.channels.len() != nch) {
        tracing::warn!(
            channels = nch,
            "frames in one group have different channel counts; missing channels weigh zero"
        );
    }
    let ranges = (mode == WeightMode::Formula).then(|| formula_ranges(inputs, excluded, nch));
    let mut out: Vec<FrameWeight> = inputs
        .iter()
        .map(|inp| {
            let mut missing = None;
            let channels: Vec<f64> = (0..nch)
                .map(|c| {
                    let ch = match inp.measurement.channels.get(c) {
                        Some(ch) => ch,
                        None => return 0.0,
                    };
                    let w = match mode {
                        WeightMode::PsfSignalWeight => ch.psf_signal_weight,
                        WeightMode::PsfSnr => ch.psf_snr,
                        WeightMode::Noise => {
                            let ns = 0.5 * (ch.noise_scale_low + ch.noise_scale_high);
                            if ch.noise > 0.0 {
                                (ns / ch.noise).powi(2)
                            } else {
                                0.0
                            }
                        }
                        WeightMode::Formula => {
                            formula_weight(ch, formula, &ranges.as_ref().unwrap()[c])
                        }
                        WeightMode::Exposure => match inp.exposure_s {
                            Some(e) if e > 0.0 => e,
                            _ => {
                                missing = Some("no exposure time".to_string());
                                0.0
                            }
                        },
                        WeightMode::Keyword => match inp.keyword_value {
                            Some(k) if k.is_finite() => k,
                            _ => {
                                missing = Some("no weight keyword value".to_string());
                                0.0
                            }
                        },
                        WeightMode::None => 1.0,
                    };
                    if w.is_finite() && w > 0.0 {
                        w
                    } else {
                        0.0
                    }
                })
                .collect();
            FrameWeight {
                mean: mean(&channels),
                channels,
                normalized: Vec::new(),
                normalized_mean: 0.0,
                missing,
            }
        })
        .collect();
    for c in 0..nch {
        let max = out
            .iter()
            .enumerate()
            .filter(|(i, w)| !excluded.get(*i).copied().unwrap_or(false) && w.missing.is_none())
            .map(|(_, w)| w.channels[c])
            .fold(0.0f64, f64::max);
        for w in &mut out {
            w.normalized
                .push(if max > 0.0 { w.channels[c] / max } else { 0.0 });
        }
    }
    for w in &mut out {
        w.normalized_mean = mean(&w.normalized);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SelectionConfig {
    /// Frames below this fraction of the group's maximum weight are excluded.
    pub min_weight_fraction: f64,
    pub max_fwhm_px: Option<f64>,
    pub max_eccentricity: Option<f64>,
    pub min_stars: Option<usize>,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        SelectionConfig {
            min_weight_fraction: 0.05,
            max_fwhm_px: None,
            max_eccentricity: None,
            min_stars: None,
        }
    }
}

/// One exclusion reason per frame (`None` = included), checked in the
/// spec's order: manual exclusion, missing weight input, the weight gate,
/// then the optional FWHM / eccentricity / star-count filters (frame-level:
/// channel means for FWHM and eccentricity, the channel minimum for stars).
pub fn select_frames(
    inputs: &[WeightInput],
    weights: &[FrameWeight],
    manual_excluded: &[bool],
    cfg: &SelectionConfig,
) -> Vec<Option<String>> {
    inputs
        .iter()
        .enumerate()
        .map(|(i, inp)| {
            if manual_excluded.get(i).copied().unwrap_or(false) {
                return Some("excluded manually".to_string());
            }
            let Some(w) = weights.get(i) else {
                return Some("no weight computed".to_string());
            };
            if let Some(m) = &w.missing {
                return Some(m.clone());
            }
            if w.normalized_mean < cfg.min_weight_fraction {
                return Some(format!(
                    "weight {:.3} below {:.2} of the group maximum",
                    w.normalized_mean, cfg.min_weight_fraction
                ));
            }
            let m = inp.measurement;
            if let Some(max) = cfg.max_fwhm_px {
                let f = m.mean_fwhm_px();
                if f > max {
                    return Some(format!("FWHM {f:.2} px above {max:.2}"));
                }
            }
            if let Some(max) = cfg.max_eccentricity {
                let e = m.mean_eccentricity();
                if e > max {
                    return Some(format!("eccentricity {e:.2} above {max:.2}"));
                }
            }
            if let Some(min) = cfg.min_stars {
                let s = m.min_stars();
                if s < min {
                    return Some(format!("{s} stars below {min}"));
                }
            }
            None
        })
        .collect()
}

/// The included frame with the highest normalized mean weight, ties broken
/// by star count (spec §4.4 — the caller restricts this to one group).
pub fn best_by_weight(
    weights: &[FrameWeight],
    included: &[bool],
    star_counts: &[usize],
) -> Option<usize> {
    (0..weights.len())
        .filter(|&i| included.get(i).copied().unwrap_or(false))
        .max_by(|&a, &b| {
            weights[a]
                .normalized_mean
                .total_cmp(&weights[b].normalized_mean)
                .then(star_counts.get(a).cmp(&star_counts.get(b)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::measure::NoiseSource;

    fn meas(psfsw: &[f64], fwhm: f64, ecc: f64, stars: usize, snrw: f64) -> FrameMeasurement {
        FrameMeasurement {
            width: 10,
            height: 10,
            duration_ms: 0,
            channels: psfsw
                .iter()
                .map(|&w| ChannelMeasurement {
                    stars_detected: stars,
                    stars_fitted: stars,
                    beta: 4.0,
                    fwhm_px: fwhm,
                    eccentricity: ecc,
                    tflux: 1.0,
                    tmean_flux: 1.0,
                    mean_flux_rejected: 0,
                    m_star: 1.0,
                    n_star: 1.0,
                    noise: 0.01,
                    noise_source: NoiseSource::Mrs,
                    median: 0.1,
                    mad: 0.01,
                    median_mean_dev: 0.012,
                    snr_weight: snrw,
                    noise_scale_low: 0.006,
                    noise_scale_high: 0.006,
                    location: 0.1,
                    scale: 0.01,
                    psf_signal_weight: w,
                    psf_snr: w * 2.0,
                })
                .collect(),
        }
    }
    fn input(m: &FrameMeasurement) -> WeightInput<'_> {
        WeightInput {
            measurement: m,
            exposure_s: Some(120.0),
            keyword_value: None,
        }
    }

    #[test]
    fn psf_signal_weights_normalize_per_channel_and_average() {
        let ms = [
            meas(&[2.0, 1.0], 3.0, 0.1, 100, 5.0),
            meas(&[1.0, 4.0], 3.0, 0.1, 100, 5.0),
        ];
        let inputs: Vec<WeightInput> = ms.iter().map(input).collect();
        let w = compute_weights(
            &inputs,
            WeightMode::PsfSignalWeight,
            &FormulaWeights::default(),
            &[false, false],
        );
        assert_eq!(w[0].channels, vec![2.0, 1.0]);
        assert_eq!(w[0].normalized, vec![1.0, 0.25]);
        assert_eq!(w[1].normalized, vec![0.5, 1.0]);
        assert!((w[0].mean - 1.5).abs() < 1e-12);
        assert!((w[0].normalized_mean - 0.625).abs() < 1e-12);
        assert!(w.iter().all(|x| x.missing.is_none()));
    }

    #[test]
    fn excluded_frames_do_not_set_the_maximum() {
        let ms = [
            meas(&[10.0], 3.0, 0.1, 100, 5.0),
            meas(&[1.0], 3.0, 0.1, 100, 5.0),
        ];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let w = compute_weights(
            &inputs,
            WeightMode::PsfSignalWeight,
            &FormulaWeights::default(),
            &[true, false],
        );
        assert_eq!(w[0].normalized, vec![10.0]);
        assert_eq!(w[1].normalized, vec![1.0]);
    }

    #[test]
    fn other_modes() {
        let m = meas(&[2.0], 3.0, 0.1, 100, 5.0);
        let inputs = [
            WeightInput {
                measurement: &m,
                exposure_s: Some(300.0),
                keyword_value: Some(0.7),
            },
            WeightInput {
                measurement: &m,
                exposure_s: None,
                keyword_value: None,
            },
        ];
        let f = FormulaWeights::default();
        let none = [false, false];
        let w = compute_weights(&inputs, WeightMode::PsfSnr, &f, &none);
        assert_eq!(w[0].channels, vec![4.0]);
        let w = compute_weights(&inputs, WeightMode::Noise, &f, &none);
        assert!((w[0].channels[0] - (0.006f64 / 0.01).powi(2)).abs() < 1e-12);
        let w = compute_weights(&inputs, WeightMode::Exposure, &f, &none);
        assert_eq!(w[0].channels, vec![300.0]);
        assert_eq!(w[1].channels, vec![0.0]);
        assert_eq!(w[1].missing.as_deref(), Some("no exposure time"));
        assert_eq!(w[0].normalized, vec![1.0]);
        let w = compute_weights(&inputs, WeightMode::Keyword, &f, &none);
        assert_eq!(w[0].channels, vec![0.7]);
        assert_eq!(w[1].missing.as_deref(), Some("no weight keyword value"));
        let w = compute_weights(&inputs, WeightMode::None, &f, &none);
        assert_eq!(w[0].normalized, vec![1.0]);
        assert_eq!(w[1].normalized, vec![1.0]);
    }

    #[test]
    fn formula_mode_ranks_by_the_four_terms() {
        let ms = [
            meas(&[1.0], 2.0, 0.2, 300, 9.0),
            meas(&[1.0], 4.0, 0.2, 100, 1.0),
            meas(&[1.0], 3.0, 0.2, 200, 5.0),
        ];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let f = FormulaWeights {
            fwhm: 15.0,
            eccentricity: 15.0,
            snr: 20.0,
            stars: 10.0,
            pedestal: 50.0,
        };
        let w = compute_weights(&inputs, WeightMode::Formula, &f, &[false; 3]);
        assert!(
            (w[0].channels[0] - 110.0).abs() < 1e-9,
            "{}",
            w[0].channels[0]
        ); // 15 + 15 + 20 + 10 + 50
        assert!(
            (w[1].channels[0] - 65.0).abs() < 1e-9,
            "{}",
            w[1].channels[0]
        ); // 0 + 15 + 0 + 0 + 50
        assert!(
            (w[2].channels[0] - 87.5).abs() < 1e-9,
            "{}",
            w[2].channels[0]
        ); // 7.5 + 15 + 10 + 5 + 50
        assert_eq!(w[0].normalized, vec![1.0]);
    }

    #[test]
    fn selection_reasons_in_order() {
        let ms = [
            meas(&[1.0], 3.0, 0.1, 100, 5.0),
            meas(&[0.02], 3.0, 0.1, 100, 5.0),
            meas(&[0.9], 5.0, 0.1, 100, 5.0),
            meas(&[0.9], 3.0, 0.7, 100, 5.0),
            meas(&[0.9], 3.0, 0.1, 10, 5.0),
            meas(&[0.9], 3.0, 0.1, 100, 5.0),
        ];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let manual = [false, false, false, false, false, true];
        let w = compute_weights(
            &inputs,
            WeightMode::PsfSignalWeight,
            &FormulaWeights::default(),
            &manual,
        );
        let cfg = SelectionConfig {
            min_weight_fraction: 0.05,
            max_fwhm_px: Some(4.0),
            max_eccentricity: Some(0.5),
            min_stars: Some(50),
        };
        let r = select_frames(&inputs, &w, &manual, &cfg);
        assert_eq!(r[0], None);
        assert_eq!(
            r[1].as_deref(),
            Some("weight 0.020 below 0.05 of the group maximum")
        );
        assert_eq!(r[2].as_deref(), Some("FWHM 5.00 px above 4.00"));
        assert_eq!(r[3].as_deref(), Some("eccentricity 0.70 above 0.50"));
        assert_eq!(r[4].as_deref(), Some("10 stars below 50"));
        assert_eq!(r[5].as_deref(), Some("excluded manually"));
        let included: Vec<bool> = r.iter().map(|x| x.is_none()).collect();
        assert_eq!(
            best_by_weight(&w, &included, &[100, 100, 100, 100, 10, 100]),
            Some(0)
        );
        assert_eq!(best_by_weight(&w, &[false; 6], &[100; 6]), None);
        let r = select_frames(&inputs, &w, &manual, &SelectionConfig::default());
        assert_eq!(r[2], None);
        assert_eq!(r[4], None);
    }

    #[test]
    fn ties_in_weight_break_on_star_count() {
        let ms = [
            meas(&[1.0], 3.0, 0.1, 100, 5.0),
            meas(&[1.0], 3.0, 0.1, 150, 5.0),
        ];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let w = compute_weights(
            &inputs,
            WeightMode::PsfSignalWeight,
            &FormulaWeights::default(),
            &[false, false],
        );
        assert_eq!(best_by_weight(&w, &[true, true], &[100, 150]), Some(1));
    }

    #[test]
    fn missing_inputs_are_named_before_the_weight_gate() {
        let m = meas(&[1.0], 3.0, 0.1, 100, 5.0);
        let inputs = [
            WeightInput {
                measurement: &m,
                exposure_s: Some(60.0),
                keyword_value: None,
            },
            WeightInput {
                measurement: &m,
                exposure_s: None,
                keyword_value: None,
            },
        ];
        let w = compute_weights(
            &inputs,
            WeightMode::Exposure,
            &FormulaWeights::default(),
            &[false, false],
        );
        let r = select_frames(&inputs, &w, &[false, false], &SelectionConfig::default());
        assert_eq!(r[0], None);
        assert_eq!(r[1].as_deref(), Some("no exposure time"));
        assert_eq!(
            select_frames(
                &inputs,
                &w[..1],
                &[false, false],
                &SelectionConfig::default()
            )[1]
            .as_deref(),
            Some("no weight computed")
        );
    }

    #[test]
    fn serde_names_match_the_spec() {
        assert_eq!(
            serde_json::to_string(&WeightMode::PsfSignalWeight).unwrap(),
            "\"psfSignalWeight\""
        );
        assert_eq!(
            serde_json::to_string(&WeightMode::PsfSnr).unwrap(),
            "\"psfSnr\""
        );
        assert_eq!(
            serde_json::to_string(&WeightMode::None).unwrap(),
            "\"none\""
        );
        assert_eq!(WeightMode::default(), WeightMode::PsfSignalWeight);
        assert_eq!(
            serde_json::from_str::<FormulaWeights>("{}").unwrap(),
            FormulaWeights::default()
        );
        assert_eq!(
            serde_json::from_str::<SelectionConfig>("{\"maxFwhmPx\":3.5}").unwrap(),
            SelectionConfig {
                max_fwhm_px: Some(3.5),
                ..Default::default()
            }
        );
        let f = serde_json::to_string(&FormulaWeights::default()).unwrap();
        assert!(
            f.contains("\"fwhm\":15.0")
                && f.contains("\"pedestal\":50.0")
                && f.contains("\"stars\":0.0")
        );
        let s = serde_json::to_string(&SelectionConfig::default()).unwrap();
        assert!(s.contains("\"minWeightFraction\":0.05") && s.contains("\"maxFwhmPx\":null"));
        let back: WeightMode = serde_json::from_str("\"keyword\"").unwrap();
        assert_eq!(back, WeightMode::Keyword);
    }
}
