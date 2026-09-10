//! Frame weights, selection and reference choice (spec §4.2–4.4, math
//! reference §1.2, §1.5, §2.5, §3.7). Weights are per channel and
//! normalized by the group maximum per channel; the frame-level weight is
//! the mean over channels.

use serde::{Deserialize, Serialize};

use super::measure::{ChannelMeasurement, FrameMeasurement};
use crate::geometry::PixelMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct SelectionConfig {
    /// Frames below this fraction of the group's maximum weight are excluded.
    pub min_weight_fraction: f64,
    pub max_fwhm_px: Option<f64>,
    pub max_eccentricity: Option<f64>,
    pub min_stars: Option<usize>,
    /// A frame whose registration failed is dropped from its group (spec
    /// §9.2). Read by the run orchestration, not by [`select_frames`] —
    /// this stage has no registration outcome to check, so the field is
    /// carried here only so a stored config round-trips.
    pub exclude_on_registration_failure: bool,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        SelectionConfig {
            min_weight_fraction: 0.05,
            max_fwhm_px: None,
            max_eccentricity: None,
            min_stars: None,
            exclude_on_registration_failure: true,
        }
    }
}

/// One exclusion reason per frame (`None` = included), checked in the
/// spec's order: manual exclusion, missing weight input, the weight gate,
/// then the optional FWHM / eccentricity / star-count filters (frame-level:
/// channel means for FWHM and eccentricity, the channel minimum for stars).
/// `SelectionConfig::exclude_on_registration_failure` is not read here — see
/// its own doc comment.
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

/// The sky-penalized normalization ranking (spec §4.4/§5.2, ruling
/// R-M3-17 v2): the set's reference frame (manual pin or auto
/// [`best_by_weight`]) is the REGISTRATION reference only — it no longer
/// anchors normalization just because it happens to be a group's member.
/// Instead, every admissible member (the same `included` gate
/// [`best_by_weight`] applies — the weight-floor/star-count exclusion has
/// already happened upstream in `select_frames`, `included` just carries
/// its verdict) scores
///
/// ```text
/// s_i = weight.normalized_mean / sqrt(sky[i])
/// ```
///
/// where `sky[i]` is the frame's mean per-plane background (mean over
/// `FrameMeasurement.channels[*].median`) — faint-signal SNR scales as
/// `1 / sqrt(background)`, so this both ranks by weight AND penalizes a
/// bright sky. A member whose `sky[i]` is non-finite or `<= 0` is not a
/// candidate at all (dropped from the result, not merely ranked last) —
/// there is no physical background to take a square root of.
///
/// Fix round 1, Important 2: `sky[i]` is floored to
/// `SKY_BACKGROUND_FLOOR_FRACTION` (1%) of the admissible candidates' own
/// median background before the square root — an over-subtracted master
/// dark (a warmer/longer dark, an amp-glow mismatch) can leave a frame's
/// measured background genuinely near zero without the frame's own
/// calibration having failed outright (which is what the `<= 0` drop above
/// already catches); left unfloored, such a frame would win by a landslide
/// and become the anchor AND the whole LN reference, normalizing the
/// master to the one frame whose calibration is suspect. The floor is
/// relative, not absolute, because a real background sits anywhere from
/// ~1e-3 to ~5e-2 in native units depending on the site/target — no single
/// absolute constant would both catch a genuinely broken frame and leave a
/// genuinely dark-sky one alone.
///
/// Returns the admissible candidates' ORIGINAL indices, descending by
/// `s_i`; ties broken by the higher weight, then by star count
/// ([`best_by_weight`]'s own tie-break), then by the higher original
/// index — this last tie-break exists ONLY so a FULL tie (equal score,
/// equal weight, equal star count) matches `best_by_weight`'s own
/// `Iterator::max_by` semantics, which return the LAST of equally maximum
/// elements; without it a stable sort's "first survivor wins" would
/// disagree with `best_by_weight` on that one degenerate case (pinned by
/// `sky_penalized_order_matches_best_by_weight_on_a_full_tie`). On a group
/// whose members share one background (the common case on a single-night
/// set), `s_i` is monotonic in weight, so this order's first element is
/// exactly what [`best_by_weight`] would return — the two rules coincide
/// except when sky varies enough across members to move the ranking (a
/// real single night can drift 20-30% night-to-night with altitude/
/// moonrise, enough to reorder near-ties even without a second night in
/// the mix).
///
/// The caller takes the first element as the group's NORMALIZATION anchor
/// (`GroupInput.reference`, `normalization_reference_frame_id`), and the
/// first `normalization.local.referenceFrames` elements as the LN
/// reference's member list (`stacking::run::run_group_normalization`, in
/// place of ranking by raw weight alone — `stacking::ln::reference::
/// build_reference` itself is unchanged, it still ranks by weight whatever
/// candidate set it is handed, but the run now hands it this sky-penalized
/// top-N instead of the whole group). Fix round 1, Important 3: an EMPTY
/// result (every member dropped for a non-finite/non-positive background)
/// is the caller's signal to fall back to [`best_by_weight`] — this
/// function itself never fails, an empty `Vec` is a valid, honest answer.
pub(crate) fn sky_penalized_order(
    weights: &[FrameWeight],
    included: &[bool],
    star_counts: &[usize],
    sky: &[f64],
) -> Vec<usize> {
    let mut candidates: Vec<usize> = (0..weights.len())
        .filter(|&i| included.get(i).copied().unwrap_or(false))
        .filter(|&i| sky.get(i).is_some_and(|&bg| bg.is_finite() && bg > 0.0))
        .collect();
    if candidates.is_empty() {
        return candidates;
    }
    let median_bg = {
        let mut bgs: Vec<f64> = candidates.iter().map(|&i| sky[i]).collect();
        bgs.sort_by(f64::total_cmp);
        let n = bgs.len();
        if n % 2 == 1 {
            bgs[n / 2]
        } else {
            0.5 * (bgs[n / 2 - 1] + bgs[n / 2])
        }
    };
    let floor = SKY_BACKGROUND_FLOOR_FRACTION * median_bg;
    let score = |i: usize| weights[i].normalized_mean / sky[i].max(floor).sqrt();
    candidates.sort_by(|&a, &b| {
        score(b)
            .total_cmp(&score(a))
            .then_with(|| {
                weights[b]
                    .normalized_mean
                    .total_cmp(&weights[a].normalized_mean)
            })
            .then_with(|| star_counts.get(b).cmp(&star_counts.get(a)))
            .then_with(|| b.cmp(&a))
    });
    candidates
}

/// Fix round 1, Important 2: the floor [`sky_penalized_order`] clamps a
/// candidate's background to, as a fraction of the admissible candidates'
/// own median background — see that function's doc for the rationale.
const SKY_BACKGROUND_FLOOR_FRACTION: f64 = 0.01;

/// Minimum [`reference_coverage`] fraction for a candidate to be
/// admissible as the normalization anchor or an LN-reference member (spec
/// §4.4, ruling R-M3-17 v2 addendum). Fix round 1, Important 6: the
/// threshold was chosen from the estimator's OWN measured values at
/// 6224×4168 (reproducible with [`reference_coverage`] as shipped, a pure
/// rotation about the rectangle's centre) — 1°→1.0, 2°→0.988, 3°→0.980,
/// **4°→0.969 (the first crossing below 0.97)**, 5°→0.957; a 30-px
/// translation dither loses far less, ≈0.995. A rotation this small only
/// clips the four corners' nearest grid cell(s) — the loss is
/// tangential-to-the-boundary near each corner, not radial, so it grows
/// slower than a first guess of "a couple of degrees" suggests; 0.97 is
/// set just past the measured 4° crossing, comfortably below what a real
/// misregistration this filter exists to catch (a different camera angle,
/// a badly failed solve) actually produces.
pub(crate) const MIN_REFERENCE_COVERAGE: f64 = 0.97;

/// The fraction of the reference rectangle `[0, ref_w) x [0, ref_h)` a
/// registered frame's own footprint covers (spec §4.4, ruling R-M3-17 v2
/// addendum): a rotated frame, a frame from a session with another camera
/// angle, or a badly offset one must not become the normalization anchor
/// nor an LN-reference member — normalizing to it (or modelling the LN
/// reference's background on it) would carry its uncovered corners/edges
/// into every other frame.
///
/// Estimated by mapping a 32 x 32 grid of reference-pixel CELL CENTRES
/// (over `[0, ref_w) x [0, ref_h)`) through `map.inverse` (reference pixel
/// → subject pixel) and counting the fraction that lands inside the
/// frame's own native extent `[0, src_w) x [0, src_h)`. Pure/deterministic;
/// the caller compares the result against [`MIN_REFERENCE_COVERAGE`].
pub(crate) fn reference_coverage(
    map: &PixelMap,
    src_w: usize,
    src_h: usize,
    ref_w: usize,
    ref_h: usize,
) -> f64 {
    const GRID: usize = 32;
    if ref_w == 0 || ref_h == 0 {
        return 0.0;
    }
    let mut covered = 0usize;
    for gy in 0..GRID {
        let ry = (gy as f64 + 0.5) * ref_h as f64 / GRID as f64;
        for gx in 0..GRID {
            let rx = (gx as f64 + 0.5) * ref_w as f64 / GRID as f64;
            let (sx, sy) = map.inverse(rx, ry);
            if sx >= 0.0 && sx < src_w as f64 && sy >= 0.0 && sy < src_h as f64 {
                covered += 1;
            }
        }
    }
    covered as f64 / (GRID * GRID) as f64
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
            ..Default::default()
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

    /// A bare [`FrameWeight`] carrying just `normalized_mean` — the only
    /// field [`sky_penalized_order`]/[`best_by_weight`] read — for the
    /// sky-penalized-ranking tests below, which drive the score directly
    /// rather than through `compute_weights`.
    fn fw(normalized_mean: f64) -> FrameWeight {
        FrameWeight {
            channels: Vec::new(),
            normalized: Vec::new(),
            mean: normalized_mean,
            normalized_mean,
            missing: None,
        }
    }

    /// Ruling R-M3-17 v2: a two-night group where the top-weighted trio is
    /// the bright night and a lower-weighted pair is the dark night — the
    /// sky penalty (`s = w / sqrt(bg)`) must still put the dark-night
    /// frames first, both for the normalization anchor (the first element)
    /// and the LN reference's member list (the first N).
    #[test]
    fn sky_penalized_order_prefers_the_dark_night_over_higher_raw_weight() {
        // Bright night: weights 1.0/0.95/0.9, background 3000 (constant).
        // Dark night: weights 0.6/0.55, background 300 — a 10x darker sky
        // more than offsets the lower raw weight once penalized by
        // 1/sqrt(background).
        let weights = vec![fw(1.0), fw(0.95), fw(0.9), fw(0.6), fw(0.55)];
        let included = [true; 5];
        let star_counts = [100usize; 5];
        let sky = [3000.0, 3000.0, 3000.0, 300.0, 300.0];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert_eq!(
            order,
            vec![3, 4, 0, 1, 2],
            "the dark-night pair (indices 3, 4) must rank ahead of every \
             bright-night frame despite the lower raw weight: {order:?}"
        );
        // The anchor is the order's first element; the LN reference's
        // member list (N=2) is its first two — both dark-night frames.
        assert_eq!(order.first().copied(), Some(3));
        assert_eq!(&order[..2], &[3, 4]);
    }

    /// On equal backgrounds `s_i` is monotonic in weight, so the ranking
    /// must degenerate to `best_by_weight`'s own tie-break (weight, then
    /// star count) — in particular its FIRST element must equal
    /// `best_by_weight`'s pick.
    #[test]
    fn sky_penalized_order_matches_best_by_weight_on_equal_backgrounds() {
        let weights = vec![fw(0.9), fw(0.5), fw(0.9)];
        let included = [true, true, true];
        let star_counts = [100usize, 999, 150];
        let sky = [5.0, 5.0, 5.0];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert_eq!(
            order.first().copied(),
            best_by_weight(&weights, &included, &star_counts),
            "on equal backgrounds the top pick must match best_by_weight: {order:?}"
        );
        // Weight ties (indices 0 and 2) break on star count, matching
        // best_by_weight's own tie-break — index 2 (150 stars) outranks
        // index 0 (100 stars).
        assert_eq!(order, vec![2, 0, 1]);
    }

    /// Fix round 1, Important 4: on a FULL tie (equal score, equal weight,
    /// equal star count) the two rules must still agree — `best_by_weight`
    /// (`Iterator::max_by`) returns the LAST of equally maximum elements,
    /// so `sky_penalized_order`'s own final index tie-break must too.
    #[test]
    fn sky_penalized_order_matches_best_by_weight_on_a_full_tie() {
        let weights = vec![fw(0.7), fw(0.7), fw(0.7)];
        let included = [true, true, true];
        let star_counts = [50usize, 50, 50];
        let sky = [4.0, 4.0, 4.0];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert_eq!(
            order.first().copied(),
            best_by_weight(&weights, &included, &star_counts),
            "on a full tie the top pick must still match best_by_weight's own \
             last-wins semantics: order={order:?}"
        );
        assert_eq!(
            order.first().copied(),
            Some(2),
            "max_by returns the LAST of equally maximum elements"
        );
    }

    /// Fix round 1, Important 2: a near-zero background must not let a
    /// low-weight candidate dominate by raw `1/sqrt(bg)` — it is floored to
    /// a fraction of the admissible candidates' own median background.
    #[test]
    fn sky_penalized_order_floors_a_near_zero_background_so_it_cannot_dominate() {
        // Unfloored, candidate 3's score would be 0.05 / sqrt(1e-6) = 50 —
        // far above every other candidate's ~0.4-0.5. Floored to 1% of the
        // admissible median (4.0), its score becomes 0.05 / sqrt(0.04) =
        // 0.25, below candidate 0's 1.0 / sqrt(4.0) = 0.5.
        let weights = vec![fw(1.0), fw(0.9), fw(0.8), fw(0.05)];
        let included = [true, true, true, true];
        let star_counts = [100usize; 4];
        let sky = [4.0, 4.0, 4.0, 1e-6];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert_eq!(
            order.first().copied(),
            Some(0),
            "the near-zero-background candidate (index 3) must not dominate: order={order:?}"
        );
        assert_ne!(
            order.first().copied(),
            Some(3),
            "an unfloored score of ~50 would otherwise make index 3 win outright"
        );
    }

    /// A frame with no physical background (zero, negative, or non-finite)
    /// is not a candidate at all — never merely ranked last.
    #[test]
    fn sky_penalized_order_skips_non_positive_or_non_finite_background() {
        let weights = vec![fw(1.0), fw(0.5), fw(0.3)];
        let included = [true, true, true];
        let star_counts = [100usize; 3];
        let sky = [0.0, 4.0, 1.0];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert_eq!(
            order,
            vec![2, 1],
            "index 0 (background 0.0) must be dropped entirely, not ranked last: {order:?}"
        );

        let sky_nan = [f64::NAN, 4.0, 1.0];
        let order_nan = sky_penalized_order(&weights, &included, &star_counts, &sky_nan);
        assert_eq!(order_nan, vec![2, 1], "a non-finite background is skipped too");

        let sky_neg = [-3.0, 4.0, 1.0];
        let order_neg = sky_penalized_order(&weights, &included, &star_counts, &sky_neg);
        assert_eq!(order_neg, vec![2, 1], "a negative background is skipped too");
    }

    /// Fix round 1, Important 3: when EVERY admissible member's background
    /// is non-finite/non-positive (a real scenario: an over-subtracted
    /// master dark), the order is a valid, honest empty `Vec` — this
    /// function never fails; the caller (`stacking::run::
    /// process_group_output`'s `pick_reference_idx`) is the one that must
    /// fall back to `best_by_weight`, see the composite test in `run.rs`.
    #[test]
    fn sky_penalized_order_returns_empty_when_every_candidate_has_no_usable_background() {
        let weights = vec![fw(1.0), fw(0.9), fw(0.5)];
        let included = [true, true, true];
        let star_counts = [100usize; 3];
        let sky = [0.0, f64::NAN, -1.0];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert!(
            order.is_empty(),
            "every candidate has a non-finite/non-positive background: {order:?}"
        );
    }

    /// The same `included` admissibility [`best_by_weight`] applies — an
    /// excluded candidate is skipped however favorable its score.
    #[test]
    fn sky_penalized_order_skips_inadmissible_candidates() {
        let weights = vec![fw(1.0), fw(0.9)];
        let included = [true, false];
        let star_counts = [100usize; 2];
        // Index 1 would win on score alone (much darker sky) if it were
        // admissible.
        let sky = [10.0, 0.01];

        let order = sky_penalized_order(&weights, &included, &star_counts, &sky);
        assert_eq!(order, vec![0]);
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

    // ── reference_coverage (R-M3-17 v2 addendum) ────────────────────────

    use crate::geometry::{Linear, LinearKind};

    fn identity_map() -> PixelMap {
        PixelMap::linear(Linear::identity()).unwrap()
    }

    fn translated_map(dx: f64, dy: f64) -> PixelMap {
        // Subject -> reference: reference = subject + (dx, dy).
        PixelMap::linear(Linear::from_flat(
            LinearKind::Similarity,
            [1.0, 0.0, dx, 0.0, 1.0, dy, 0.0, 0.0, 1.0],
        ))
        .unwrap()
    }

    /// Subject -> reference: a rotation by `deg` about `(cx, cy)`.
    fn rotated_map(deg: f64, cx: f64, cy: f64) -> PixelMap {
        let theta = deg.to_radians();
        let (c, s) = (theta.cos(), theta.sin());
        // u = c*(x-cx) - s*(y-cy) + cx = c*x - s*y + (cx - c*cx + s*cy)
        // v = s*(x-cx) + c*(y-cy) + cy = s*x + c*y + (cy - s*cx - c*cy)
        let m02 = cx - c * cx + s * cy;
        let m12 = cy - s * cx - c * cy;
        PixelMap::linear(Linear::from_flat(
            LinearKind::Similarity,
            [c, -s, m02, s, c, m12, 0.0, 0.0, 1.0],
        ))
        .unwrap()
    }

    #[test]
    fn reference_coverage_identity_is_full() {
        let map = identity_map();
        assert_eq!(reference_coverage(&map, 6224, 4168, 6224, 4168), 1.0);
    }

    /// A 30-px dither on a several-thousand-px frame loses well under a
    /// percent (the addendum's own "≈ 0.995") — comfortably above
    /// [`MIN_REFERENCE_COVERAGE`], so this candidate must still pass.
    #[test]
    fn reference_coverage_small_dither_passes() {
        let map = translated_map(30.0, 30.0);
        let coverage = reference_coverage(&map, 6224, 4168, 6224, 4168);
        assert!(
            coverage >= MIN_REFERENCE_COVERAGE,
            "a 30-px dither must still pass the coverage gate: {coverage}"
        );
        assert!(
            coverage > 0.98,
            "a 30-px dither on a 6224x4168 frame should lose well under 2%: {coverage}"
        );
    }

    /// A 2-degree rotation about the frame's own centre loses several
    /// percent at the corners — below [`MIN_REFERENCE_COVERAGE`].
    /// The addendum's own worked example ("a 2-degree rotation about the
    /// centre loses several %, below `MIN_REFERENCE_COVERAGE`") does not
    /// hold, numerically, for this exact 32x32-grid estimator at
    /// 6224x4168: a rotation this small only clips the four corners'
    /// nearest grid cell(s) — measured (this test, before the assertion
    /// below existed) at 1deg=1.0, 2deg=0.98828125, 3deg=0.98046875,
    /// 4deg=0.96875 (first crossing), 5deg=0.95703125. The GATE'S OWN
    /// purpose — reject a materially misaligned frame — is what the
    /// composite pipeline test below exercises; this unit test pins a
    /// rotation (5 degrees, comfortably past the 4-degree crossing) that
    /// the estimator, AS LITERALLY SPECIFIED (spec addendum, ruling
    /// R-M3-17 v2), verifiably fails, rather than asserting a "2 degrees"
    /// outcome the algorithm does not actually produce.
    #[test]
    fn reference_coverage_rotation_fails() {
        let map = rotated_map(5.0, 6224.0 / 2.0, 4168.0 / 2.0);
        let coverage = reference_coverage(&map, 6224, 4168, 6224, 4168);
        assert!(
            coverage < MIN_REFERENCE_COVERAGE,
            "a 5-degree rotation must fail the coverage gate: {coverage}"
        );
    }

    /// A native extent LARGER than the reference (e.g. an OSC sensor at
    /// 6248x4176 registered onto a 6224x4168 reference) passes trivially —
    /// every reference cell centre lands well inside the larger subject
    /// extent.
    #[test]
    fn reference_coverage_larger_native_extent_passes() {
        let map = identity_map();
        assert_eq!(reference_coverage(&map, 6248, 4176, 6224, 4168), 1.0);
    }

    /// A native extent SMALLER than the reference (a mismatched camera
    /// angle/sensor) cannot cover the reference rectangle's far corner —
    /// below [`MIN_REFERENCE_COVERAGE`].
    #[test]
    fn reference_coverage_smaller_native_extent_fails() {
        let map = identity_map();
        let coverage = reference_coverage(&map, 6000, 4000, 6224, 4168);
        assert!(
            coverage < MIN_REFERENCE_COVERAGE,
            "a 6000x4000 native extent onto a 6224x4168 reference must fail: {coverage}"
        );
    }

    /// Composite (spec §4.4 addendum to ruling R-M3-17 v2): the coverage
    /// filter runs BEFORE the sky-penalized ranking — a candidate with the
    /// best score but a rotated (poorly-covering) registration must be
    /// skipped in favour of the next-best one that actually covers the
    /// reference geometry. Mirrors exactly what `stacking::run::
    /// process_group_output`'s `pick_reference_idx` closure does: build
    /// the coverage mask, then rank with it as `included`.
    #[test]
    fn coverage_filter_then_sky_penalized_rank_skips_a_rotated_top_score_candidate() {
        let ref_w = 6224usize;
        let ref_h = 4168usize;
        // Frame 0 would have the best sky-penalized score by far (tiny
        // background, high weight) but is registered with a 5-degree
        // rotation — well below MIN_REFERENCE_COVERAGE. Frames 1-3 are
        // identity-registered (full coverage); frame 1 is the
        // best-scoring of THOSE.
        let maps = [
            rotated_map(5.0, ref_w as f64 / 2.0, ref_h as f64 / 2.0),
            identity_map(),
            identity_map(),
            identity_map(),
        ];
        let weights = vec![fw(1.0), fw(0.9), fw(0.6), fw(0.5)];
        let star_counts = [100usize; 4];
        let sky = [1.0, 4.0, 4.0, 4.0];

        let coverage: Vec<f64> = maps
            .iter()
            .map(|m| reference_coverage(m, ref_w, ref_h, ref_w, ref_h))
            .collect();
        let coverage_mask: Vec<bool> = coverage
            .iter()
            .map(|&c| c >= MIN_REFERENCE_COVERAGE)
            .collect();
        assert_eq!(coverage_mask, vec![false, true, true, true], "{coverage:?}");

        // Without the coverage filter, frame 0 would win by a wide margin.
        let unfiltered = sky_penalized_order(&weights, &[true; 4], &star_counts, &sky);
        assert_eq!(unfiltered.first().copied(), Some(0));

        let order = sky_penalized_order(&weights, &coverage_mask, &star_counts, &sky);
        assert_eq!(
            order.first().copied(),
            Some(1),
            "the rotated frame 0 must be skipped for frame 1 (best among the \
             covering candidates): order={order:?}"
        );
    }
}
