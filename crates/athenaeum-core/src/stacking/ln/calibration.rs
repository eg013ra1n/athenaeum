//! The per-group seeds calibration (perf tier C Task 2, ruling C-14).
//!
//! [`super::scale::relative_scale_from_seeds`] reads a few tenths of a
//! percent HIGHER than the detection path it replaces — same-signed on
//! every real frame measured (fix round 2's 13-frame table: median
//! +0.666 %, max +1.401 %). A same-signed bias moves a master's LEVEL
//! (spec §8's median ± 0.1 % row), so it has to be removed. Fix round 3
//! (ruling C-13, LEVER 1) measured the obvious structural explanations —
//! the saturation guard, the pre-select radius — and found none of them:
//! widening or removing the pre-select left `matched` and `scale`
//! unchanged while costing real time. What is left is a multiplicative
//! per-group correction, measured directly by running BOTH paths on a
//! sample of the group's own frames.
//!
//! This module owns that measurement's two pure halves — which frames to
//! measure ([`select_calibration_frames`]) and how their per-channel
//! ratios become `k` ([`calibration_k`]) — plus the loop that drives
//! [`super::normalize_frame`] twice per calibration frame
//! ([`measure_seeds_calibration`]). `stacking::run` owns everything around
//! it: reading each frame's persisted fits, the progress events, caching
//! the result as the group's `ln_calibration` artifact and folding it into
//! every member's own LN hash.
//!
//! **Ruling C-14 vs fix round 3**: `k` is now per CHANNEL (an OSC group's
//! three planes can carry different residuals — the review's I4), measured
//! over SEVEN frames rather than three (I5 — a 3-sample median of a bias
//! that spans +0.1…+1.4 % across a group leaves a ±0.3 % common-mode
//! sampling error, which lands straight on the master's median), and
//! refused outright when it leaves [`SEEDS_CALIBRATION_BAND`] (the review's
//! C1 — an unguarded median of a handful of in-sample ratios lets ONE
//! degenerate calibration frame move every frame's `A`).

use std::cmp::Ordering;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::resample::Interpolation;
use crate::stacking::integrate::{LocalNormalizationConfig, StackFrame};
use crate::stacking::measure::MeasureOptions;
use crate::stacking::psf_signal::StarFit;

use super::background::BackgroundGrid;
use super::reference::LnReference;
use super::{normalize_frame, LnReferenceForDetection, LnScaleSeeds};

/// How many of the group's own frames the calibration measures (ruling
/// C-14 item 1): the group's registration/geometry reference frame plus
/// the six best-weighted OTHER included members. Seven costs ≈ 7 × one
/// detection-arm `normalize_frame` (≈ 6 s on a real 26 Mpx frame) ≈ 45 s
/// per group, against the ≈ 2.5 s per frame the seeds path saves over a
/// group of 90–160 frames — see the spec's §2.2 item 8 for the honest
/// arithmetic.
pub const SEEDS_CALIBRATION_FRAMES: usize = 7;

/// A channel needs at least this many usable `s_detected / s_seeds`
/// ratios before its median is trusted as `k` (ruling C-14 item 1); below
/// it the channel runs uncalibrated (`k = 1`), never on a two-sample
/// "median" that is really a mean of whatever two frames happened to work.
pub const SEEDS_CALIBRATION_MIN_RATIOS: usize = 3;

/// `k` outside `1 ± this` is refused (ruling C-14 item 1) — four times the
/// largest per-frame bias ever measured for this path (+1.401 % on frame
/// 29035 of the acceptance catalog, fix round 2's own table). A `k` beyond
/// it is not a seeds-path bias: it is a calibration frame whose two arms
/// disagreed for some other reason, and applying it would move every
/// frame's `A` — and the master's level — by more than the defect it
/// claims to correct.
pub const SEEDS_CALIBRATION_BAND: f64 = 0.03;

/// One group's measured seeds calibration — what
/// [`measure_seeds_calibration`] produced, what `stacking::run` stores as
/// the group's `ln_calibration` artifact payload and folds into every
/// member's own LN config hash (ruling C-14 item 2: a changed `k` must
/// invalidate the group's sidecars, or a group ends up with a mix of
/// frames normalized at different `k` — the review's I3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedsCalibration {
    /// The calibration frames' catalog ids, in the order
    /// [`select_calibration_frames`] chose them (the reference first).
    /// Part of the hash: a different sample is a different measurement
    /// even when the `k` it produced happens to round the same way.
    pub frame_ids: Vec<i64>,
    /// One factor per channel, `1.0` for a channel that could not be
    /// calibrated (too few usable ratios, or a median outside
    /// [`SEEDS_CALIBRATION_BAND`]).
    pub k: Vec<f64>,
}

impl SeedsCalibration {
    /// Channel `channel`'s factor, `1.0` for a channel this calibration
    /// says nothing about (a group whose channel count changed under a
    /// stored calibration — never in one run, but a stored payload is
    /// read back by the plan gate too).
    pub fn k_for(&self, channel: usize) -> f64 {
        self.k.get(channel).copied().unwrap_or(1.0)
    }

    /// Every factor exactly `1.0` — a calibration that measured cleanly
    /// and found nothing to correct is indistinguishable, numerically,
    /// from no calibration at all; the caller still records it, because
    /// "measured, and it was 1" is a different cache state from "never
    /// measured".
    pub fn is_neutral(&self) -> bool {
        self.k.iter().all(|k| *k == 1.0)
    }
}

/// Why one channel's `k` came out the way it did — the return of
/// [`calibration_k`], so the caller can warn ONCE for the whole group
/// about short samples and once per channel about a refused median,
/// without this pure function owning the group-level logging.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelOutcome {
    /// The median of `samples` usable ratios, inside the band.
    Calibrated,
    /// Fewer than [`SEEDS_CALIBRATION_MIN_RATIOS`] usable ratios.
    TooFewRatios,
    /// The median landed outside [`SEEDS_CALIBRATION_BAND`]; `measured` is
    /// what it was before being refused.
    OutOfBand { measured: f64 },
}

/// One channel's calibration verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelCalibration {
    /// The factor to apply — `1.0` for anything but [`ChannelOutcome::Calibrated`].
    pub k: f64,
    /// How many usable ratios the median was taken over.
    pub samples: usize,
    pub outcome: ChannelOutcome,
}

/// The calibration sample (ruling C-14 item 1): the group's
/// registration/geometry reference frame FIRST, then the best-weighted
/// OTHER members by descending weight, up to `want` frames in total.
///
/// `members` is `(frame_id, weight)` in the group's own member order; the
/// returned indices point back into it. A reference frame that is not a
/// member at all (never happens — the reference IS a member — but this
/// must not panic or silently produce a short sample) simply leaves the
/// whole sample to the weight ranking. Ties in weight break on the
/// frame id, ascending: the sample is part of the group's LN hash, so two
/// runs over the same catalog must choose the same frames in the same
/// order.
pub fn select_calibration_frames(
    reference_frame_id: i64,
    members: &[(i64, f64)],
    want: usize,
) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::with_capacity(want.min(members.len()));
    if let Some(idx) = members.iter().position(|(id, _)| *id == reference_frame_id) {
        chosen.push(idx);
    }
    let mut by_weight: Vec<usize> = (0..members.len()).filter(|i| !chosen.contains(i)).collect();
    by_weight.sort_by(|&a, &b| {
        members[b]
            .1
            .partial_cmp(&members[a].1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| members[a].0.cmp(&members[b].0))
    });
    chosen.extend(
        by_weight
            .into_iter()
            .take(want.saturating_sub(chosen.len())),
    );
    chosen
}

/// `ratios[channel]` is that channel's own `s_detected / s_seeds` sample —
/// one entry per calibration frame whose SEEDS arm genuinely took the
/// seeds path on that channel (a channel that fell back to detection in
/// the seeds arm has nothing to compare and contributes no ratio).
///
/// Per channel: drop the non-finite and the non-positive, require at least
/// [`SEEDS_CALIBRATION_MIN_RATIOS`], take the MEDIAN — for an odd count the
/// natural middle, for an even one the LOWER of the two middles (the
/// deliberate choice: an interpolated mid-point is not one of the measured
/// ratios, and with the band guard below a real sample value is the more
/// conservative estimate) — and refuse it outright when it leaves
/// `1 ± `[`SEEDS_CALIBRATION_BAND`]. Channels are INDEPENDENT: an OSC
/// group's blue plane being refused says nothing about its red.
pub fn calibration_k(ratios: &[Vec<f64>]) -> Vec<ChannelCalibration> {
    ratios
        .iter()
        .map(|samples| {
            let mut usable: Vec<f64> = samples
                .iter()
                .copied()
                .filter(|r| r.is_finite() && *r > 0.0)
                .collect();
            if usable.len() < SEEDS_CALIBRATION_MIN_RATIOS {
                return ChannelCalibration {
                    k: 1.0,
                    samples: usable.len(),
                    outcome: ChannelOutcome::TooFewRatios,
                };
            }
            usable.sort_by(f64::total_cmp);
            let n = usable.len();
            let median = if n % 2 == 1 {
                usable[n / 2]
            } else {
                usable[n / 2 - 1]
            };
            if (median - 1.0).abs() > SEEDS_CALIBRATION_BAND {
                return ChannelCalibration {
                    k: 1.0,
                    samples: n,
                    outcome: ChannelOutcome::OutOfBand { measured: median },
                };
            }
            ChannelCalibration {
                k: median,
                samples: n,
                outcome: ChannelOutcome::Calibrated,
            }
        })
        .collect()
}

/// What [`measure_seeds_calibration`] found, before the caller turns it
/// into a [`SeedsCalibration`] and logs it.
pub struct CalibrationMeasurement {
    pub frame_ids: Vec<i64>,
    pub channels: Vec<ChannelCalibration>,
}

impl CalibrationMeasurement {
    pub fn calibration(&self) -> SeedsCalibration {
        SeedsCalibration {
            frame_ids: self.frame_ids.clone(),
            k: self.channels.iter().map(|c| c.k).collect(),
        }
    }
}

/// Runs [`normalize_frame`] TWICE on each of `indices`' frames — once on
/// the seeds path (`load_fits` supplies that frame's persisted per-plane
/// [`StarFit`]s) and once with [`LnScaleSeeds::ForcedDetection`], which is
/// today's full-detection path run ON PURPOSE: silent, and NOT counted as
/// a fallback (ruling C-14 item 3 — the review's I1/I2: the calibration's
/// own detection runs used to fire the genuine-fallback `warn!` three
/// times per group and to bump the test-only fallback counter, which made
/// the run pin that counts fallbacks vacuous).
///
/// Both arms write to a THROWAWAY sidecar inside `scratch_dir`, which this
/// function creates and removes again (the review's M4 — fix round 3 wrote
/// them straight into the group's live `ln/` directory, one rename away
/// from a real per-frame sidecar). The caller chooses the location; a
/// directory on the SAME volume as the real sidecars is the sane choice
/// (the system temp dir can be a different, much smaller filesystem), and
/// a crash leaves at most one empty-ish directory the group's own cleanup
/// sweeps with everything else.
///
/// `frames` is the group's own [`StackFrame`] slice and `indices` points
/// into it; `load_fits(i)` yields frame `i`'s per-plane fits (empty lists
/// are fine — that frame's seeds arm then falls back and contributes no
/// ratio). `on_progress(done, total, frame_id)` is called before each
/// calibration frame so the caller can emit a stage event.
///
/// Returns `None` — silently, per the review's M3 — when the run is
/// cancelled mid-measurement, and also when `scratch_dir` cannot be created
/// (a `warn!` then, since that one is a real, unexpected failure); the
/// caller records NO calibration in either case and the group's frames run
/// exactly as they would have before this existed. Every other outcome is
/// `Some`, including one where no channel could be calibrated at all —
/// "measured, and `k` is 1" is a real answer, and the caller stores it so
/// the next run does not pay for the measurement again.
#[allow(clippy::too_many_arguments)]
pub fn measure_seeds_calibration(
    reference: &LnReference,
    reference_for_detection: &LnReferenceForDetection<'_>,
    ref_backgrounds: &[BackgroundGrid],
    frames: &[StackFrame],
    indices: &[usize],
    load_fits: &dyn Fn(usize) -> Vec<Vec<StarFit>>,
    cfg: &LocalNormalizationConfig,
    measure: &MeasureOptions,
    interpolation: Interpolation,
    clamping: f32,
    scratch_dir: &Path,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
    on_progress: &dyn Fn(usize, usize, i64),
) -> Option<CalibrationMeasurement> {
    let channels = reference.planes.len();
    if let Err(e) = std::fs::create_dir_all(scratch_dir) {
        warn!(
            path = %scratch_dir.display(),
            error = %e,
            "ln seeds calibration: no scratch directory for the throwaway sidecars; the seeds path runs uncalibrated for this group"
        );
        return None;
    }
    // Removed on EVERY exit below (including the cancel returns), so a
    // cancelled measurement leaves nothing behind either.
    let _scratch = ScratchDir(scratch_dir);

    let mut ratios: Vec<Vec<f64>> = vec![Vec::with_capacity(indices.len()); channels];
    let mut frame_ids: Vec<i64> = Vec::with_capacity(indices.len());
    let total = indices.len();
    for (done, &i) in indices.iter().enumerate() {
        if cancel.load(AtomicOrdering::Relaxed) {
            return None;
        }
        let Some(frame) = frames.get(i) else { continue };
        frame_ids.push(frame.frame_id);
        on_progress(done, total, frame.frame_id);

        let fits = load_fits(i);
        let seeds_outcome = run_arm(
            reference,
            reference_for_detection,
            ref_backgrounds,
            frame,
            LnScaleSeeds::Measured(&fits),
            cfg,
            measure,
            interpolation,
            clamping,
            &scratch_dir.join(format!("seeds-{}.athln", frame.frame_id)),
            pool,
            cancel,
        );
        drop(fits);
        if cancel.load(AtomicOrdering::Relaxed) {
            return None;
        }
        let detected_outcome = run_arm(
            reference,
            reference_for_detection,
            ref_backgrounds,
            frame,
            LnScaleSeeds::ForcedDetection,
            cfg,
            measure,
            interpolation,
            clamping,
            &scratch_dir.join(format!("detect-{}.athln", frame.frame_id)),
            pool,
            cancel,
        );

        match (seeds_outcome, detected_outcome) {
            (Some(seeds), Some(detected)) => {
                for (channel, bucket) in ratios.iter_mut().enumerate() {
                    // A channel that fell back to detection in the SEEDS
                    // arm compared detection against detection — a ratio
                    // of 1 by construction, and one that would drag the
                    // median towards "no correction" for reasons that have
                    // nothing to do with the seeds path.
                    if !seeds
                        .channel_from_seeds
                        .get(channel)
                        .copied()
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let (Some(&s_seeds), Some(&s_detected)) = (
                        seeds.channel_scales.get(channel),
                        detected.channel_scales.get(channel),
                    ) else {
                        continue;
                    };
                    if s_seeds.is_finite() && s_seeds != 0.0 && s_detected.is_finite() {
                        bucket.push(s_detected / s_seeds);
                    }
                }
            }
            (seeds, detected) => {
                tracing::debug!(
                    frame_id = frame.frame_id,
                    seeds_ok = seeds.is_some(),
                    detected_ok = detected.is_some(),
                    "ln seeds calibration: skipping a calibration frame"
                );
            }
        }
    }

    Some(CalibrationMeasurement {
        frame_ids,
        channels: calibration_k(&ratios),
    })
}

/// Removes the scratch directory when the measurement returns, whichever
/// way it returns (a cancel is an early `return None`, so a plain
/// `remove_dir_all` at the end of the body would be skipped on exactly the
/// path the review's M3 is about).
struct ScratchDir<'a>(&'a Path);

impl Drop for ScratchDir<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0);
    }
}

/// One arm of one calibration frame: `normalize_frame` with NO calibration
/// applied (the measurement is of the RAW gap between the two paths) into a
/// throwaway sidecar, which is removed as soon as the outcome is in hand.
/// A failure is not fatal and not loud here — the caller's own skip
/// `debug!` names the frame and which arm failed.
#[allow(clippy::too_many_arguments)]
fn run_arm(
    reference: &LnReference,
    reference_for_detection: &LnReferenceForDetection<'_>,
    ref_backgrounds: &[BackgroundGrid],
    frame: &StackFrame,
    seeds: LnScaleSeeds<'_>,
    cfg: &LocalNormalizationConfig,
    measure: &MeasureOptions,
    interpolation: Interpolation,
    clamping: f32,
    sidecar: &Path,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Option<super::LnFrameOutcome> {
    let outcome = normalize_frame(
        reference,
        reference_for_detection,
        ref_backgrounds,
        frame,
        seeds,
        cfg,
        measure,
        interpolation,
        clamping,
        sidecar,
        None,
        pool,
        cancel,
    );
    let _ = std::fs::remove_file(sidecar);
    outcome.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_frame_leads_the_sample_and_is_never_picked_twice() {
        // The reference (id 3) is also the best-weighted member — it must
        // appear exactly once, first, and the remaining slots must go to
        // the next-best OTHER members.
        let members = [(1i64, 0.5f64), (2, 0.9), (3, 1.0), (4, 0.7), (5, 0.6)];
        let chosen = select_calibration_frames(3, &members, 3);
        assert_eq!(chosen, vec![2, 1, 3], "{chosen:?}");
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![3, 2, 4]);
    }

    #[test]
    fn a_reference_that_is_not_a_member_leaves_the_whole_sample_to_the_weights() {
        let members = [(1i64, 0.5f64), (2, 0.9), (4, 0.7)];
        let chosen = select_calibration_frames(99, &members, 2);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![2, 4]);
    }

    #[test]
    fn the_sample_is_capped_by_the_group_size_and_ties_break_on_the_frame_id() {
        let members = [(7i64, 0.5f64), (3, 0.5), (5, 0.5)];
        let chosen = select_calibration_frames(-1, &members, SEEDS_CALIBRATION_FRAMES);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![3, 5, 7], "equal weights must order by frame id");
        assert_eq!(
            select_calibration_frames(-1, &[], SEEDS_CALIBRATION_FRAMES),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn seven_frames_are_asked_for_and_the_reference_costs_one_of_the_seven() {
        let members: Vec<(i64, f64)> = (0..20).map(|i| (i as i64, i as f64 / 20.0)).collect();
        let chosen = select_calibration_frames(0, &members, SEEDS_CALIBRATION_FRAMES);
        assert_eq!(chosen.len(), SEEDS_CALIBRATION_FRAMES);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        // The reference (id 0, the WORST-weighted) first, then the six best.
        assert_eq!(ids, vec![0, 19, 18, 17, 16, 15, 14]);
    }

    #[test]
    fn an_odd_sample_takes_the_natural_median_and_an_even_one_the_lower_middle() {
        let odd = calibration_k(&[vec![0.990, 1.002, 0.996]]);
        assert_eq!(odd[0].outcome, ChannelOutcome::Calibrated);
        assert_eq!(odd[0].k, 0.996);
        assert_eq!(odd[0].samples, 3);

        // Sorted: 0.990, 0.996, 1.002, 1.004 — the lower middle is 0.996.
        let even = calibration_k(&[vec![1.002, 0.990, 1.004, 0.996]]);
        assert_eq!(even[0].k, 0.996, "even n must take the LOWER middle");
        assert_eq!(even[0].samples, 4);

        let seven = calibration_k(&[vec![0.991, 0.993, 0.994, 0.995, 0.997, 0.999, 1.001]]);
        assert_eq!(seven[0].k, 0.995);
        assert_eq!(seven[0].samples, 7);
    }

    #[test]
    fn fewer_than_three_usable_ratios_leave_the_channel_at_one() {
        for sample in [vec![], vec![0.99], vec![0.99, 1.01]] {
            let out = calibration_k(&[sample.clone()]);
            assert_eq!(out[0].k, 1.0, "{sample:?}");
            assert_eq!(out[0].outcome, ChannelOutcome::TooFewRatios, "{sample:?}");
        }
        // Non-finite and non-positive ratios are not usable, so a
        // three-entry sample can still be too short.
        let out = calibration_k(&[vec![0.99, f64::NAN, -1.0]]);
        assert_eq!(out[0].outcome, ChannelOutcome::TooFewRatios);
        assert_eq!(out[0].samples, 1);
    }

    #[test]
    fn a_median_outside_the_band_is_refused_rather_than_clamped() {
        let low = calibration_k(&[vec![0.90, 0.95, 0.96]]);
        assert_eq!(low[0].k, 1.0);
        assert_eq!(
            low[0].outcome,
            ChannelOutcome::OutOfBand { measured: 0.95 },
            "refused, and the measured value reported for the warn"
        );

        let high = calibration_k(&[vec![1.04, 1.05, 1.20]]);
        assert_eq!(high[0].k, 1.0);
        assert!(matches!(high[0].outcome, ChannelOutcome::OutOfBand { .. }));

        // The refusal is a strict `>` on `|median - 1|`, so a value just
        // inside the band is calibrated and one just outside is not. (The
        // literal edge, `1.0 + SEEDS_CALIBRATION_BAND`, is NOT asserted
        // either way: `(1.03f64 - 1.0).abs()` is `0.030000000000000027`,
        // so the exact boundary falls on whichever side f64 rounding puts
        // it — a property of the arithmetic, not a decision worth pinning.)
        let inside = calibration_k(&[vec![1.0 + SEEDS_CALIBRATION_BAND * 0.99; 3]]);
        assert_eq!(inside[0].outcome, ChannelOutcome::Calibrated);
        let outside = calibration_k(&[vec![1.0 + SEEDS_CALIBRATION_BAND * 1.01; 3]]);
        assert!(matches!(
            outside[0].outcome,
            ChannelOutcome::OutOfBand { .. }
        ));
        assert_eq!(outside[0].k, 1.0);
    }

    #[test]
    fn channels_are_calibrated_independently_of_one_another() {
        let out = calibration_k(&[
            vec![0.991, 0.993, 0.995], // fine
            vec![0.80, 0.81, 0.82],    // out of band
            vec![1.002, 1.004],        // too few
        ]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].k, 0.993);
        assert_eq!(out[0].outcome, ChannelOutcome::Calibrated);
        assert_eq!(out[1].k, 1.0);
        assert!(matches!(out[1].outcome, ChannelOutcome::OutOfBand { .. }));
        assert_eq!(out[2].k, 1.0);
        assert_eq!(out[2].outcome, ChannelOutcome::TooFewRatios);

        let calibration = CalibrationMeasurement {
            frame_ids: vec![11, 12, 13],
            channels: out,
        }
        .calibration();
        assert_eq!(calibration.k, vec![0.993, 1.0, 1.0]);
        assert_eq!(calibration.k_for(0), 0.993);
        assert_eq!(calibration.k_for(7), 1.0, "an unknown channel is 1.0");
        assert!(!calibration.is_neutral());
        assert!(SeedsCalibration {
            frame_ids: vec![1],
            k: vec![1.0, 1.0],
        }
        .is_neutral());
    }

    #[test]
    fn no_channels_at_all_produce_no_factors() {
        assert!(calibration_k(&[]).is_empty());
    }
}
