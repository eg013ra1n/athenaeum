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

/// How many of the group's own frames the calibration measures: the
/// group's registration/geometry reference frame plus one member per
/// weight SEXTILE of the others (ruling C-15; ruling C-14 originally said
/// "the six best-weighted", which fix round 4's hold-out measured as
/// unrepresentative — see [`select_calibration_frames`]).
///
/// Seven costs ≈ 7 × one detection-arm `normalize_frame` (≈ 6 s on a real
/// 26 Mpx frame) ≈ 45 s per group, against the ≈ 2.5 s per frame the seeds
/// path saves. **Break-even is ≈ 10 members**: below that a group pays more
/// to calibrate than the seeds path saves it, and nothing scales the sample
/// down between the floor and there — see the spec's §2.2 item 8.
pub const SEEDS_CALIBRATION_FRAMES: usize = 7;

/// A channel needs at least this many usable `s_detected / s_seeds`
/// ratios before its median is trusted as `k` (ruling C-14 item 1); below
/// it the channel runs uncalibrated (`k = 1`), never on a two-sample
/// "median" that is really a mean of whatever two frames happened to work.
pub const SEEDS_CALIBRATION_MIN_RATIOS: usize = 3;

/// `k` outside `1 ± this` is refused (ruling C-14 item 1) — **about twice**
/// the largest per-frame bias measured for this path (≈ 2.1 × the +1.449 %
/// of frame 29047, fix round 5's own hold-out; C-14's doc said "four
/// times", which was wrong against +1.401 % and is wronger still against
/// +1.449 % — the VALUE stays, only the claim about it is corrected,
/// C-17). A `k` beyond the band is not a seeds-path bias: it is a
/// calibration frame whose two arms disagreed for some other reason, and
/// applying it would move every frame's `A` — and the master's level — by
/// more than the defect it claims to correct.
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
    /// read back by the plan gate too) or one whose stored value is not a
    /// usable positive finite number.
    ///
    /// This is the ONE place a `k` is turned into a multiplier:
    /// [`super::normalize_frame`] calls it per channel (C-17), so the
    /// short-slice and the non-finite guard live together with the type
    /// that owns them rather than being restated at the apply site.
    pub fn k_for(&self, channel: usize) -> f64 {
        self.k
            .get(channel)
            .copied()
            .filter(|k| k.is_finite() && *k > 0.0)
            .unwrap_or(1.0)
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

/// The calibration sample (ruling C-15, superseding C-14's "the six
/// best-weighted"): the group's registration/geometry reference frame
/// FIRST, then ONE member per weight BIN of the OTHER included members —
/// six bins when the reference is itself a member, so `want = 7` frames in
/// total either way.
///
/// **Why stratified and not the top six** (ruling C-15, from fix round 4's
/// own hold-out measurement): `k` is a median, so what it centres on is the
/// sample's own median bias, and the six best-weighted frames of a real
/// group are not a sample of the group — on the acceptance catalog's mono
/// group they were one contiguous 30-minute window at FWHM 1.99–2.29 px
/// against a group reaching 5.08 px, and their median bias (+0.554 %) sat
/// below the group's own (+0.992 %). Every other frame was then left
/// under-corrected by the difference: a **+0.435 % systematic** on the
/// hold-out, which is what moves a master's LEVEL against the acceptance
/// spec's ± 0.1 % median row. Taking one frame per weight bin puts the
/// median on the group's middle at exactly the same cost — the same seven
/// `normalize_frame` pairs.
///
/// `members` is `(frame_id, weight)` in the group's own member order; the
/// returned indices point back into it, reference first and then bins from
/// best-weighted to worst. A reference frame that is not a member at all
/// (the co-registered case, where the run-wide reference can belong to a
/// different group) simply leaves every slot to the bins. Ties in weight
/// break on the frame id, ascending, and a bin's chosen member is its
/// MEDIAN position (`start + size / 2`) — for an even bin the lower-weight
/// of the two middles, the same "lower median" convention
/// [`calibration_k`] itself uses. All of it is deterministic because the
/// sample is part of the group's LN hash: two runs over the same catalog
/// must choose the same frames in the same order.
///
/// A group with no more OTHER members than there are slots measures all of
/// them, in weight order — the floor case, unchanged from C-14.
pub fn select_calibration_frames(
    reference_frame_id: i64,
    members: &[(i64, f64)],
    want: usize,
) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::with_capacity(want.min(members.len()));
    if let Some(idx) = members.iter().position(|(id, _)| *id == reference_frame_id) {
        chosen.push(idx);
    }
    let slots = want.saturating_sub(chosen.len());
    if slots == 0 {
        return chosen;
    }

    let mut by_weight: Vec<usize> = (0..members.len()).filter(|i| !chosen.contains(i)).collect();
    by_weight.sort_by(|&a, &b| {
        members[b]
            .1
            .partial_cmp(&members[a].1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| members[a].0.cmp(&members[b].0))
    });
    if by_weight.len() <= slots {
        chosen.extend(by_weight);
        return chosen;
    }

    // Contiguous bins over the weight-sorted list, as even as the count
    // allows: `n / slots` each, with the first `n % slots` bins taking one
    // extra. The remainder goes to the LEADING (best-weighted) bins because
    // that end of a real group's distribution is the denser one — the
    // frames there differ least from each other, so an extra member costs
    // the sample the least coverage.
    let n = by_weight.len();
    let base = n / slots;
    let extra = n % slots;
    let mut start = 0usize;
    for bin in 0..slots {
        let size = base + usize::from(bin < extra);
        chosen.push(by_weight[start + size / 2]);
        start += size;
    }
    debug_assert_eq!(start, n, "the bins must partition the weight-sorted list");
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
        // Recorded whether or not its two arms go on to produce a ratio:
        // `frame_ids` is the SAMPLE, and the sample is what the group's
        // `ln_calibration` hash keys on. A frame that failed is still one
        // the measurement asked about.
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

    /// `(frame_id, weight)` pairs for a group of `n` members whose weights
    /// descend from 1.0 in even steps — id `i` carries weight
    /// `(n - i) / n`, so the id order IS the weight order and a test can
    /// read the chosen ids as positions in the ranking.
    fn ranked_members(n: usize) -> Vec<(i64, f64)> {
        (0..n)
            .map(|i| (i as i64, (n - i) as f64 / n as f64))
            .collect()
    }

    #[test]
    fn the_reference_frame_leads_the_sample_and_is_never_picked_twice() {
        // The reference (id 3) is also the best-weighted member — it must
        // appear exactly once, first, and never again among the bins.
        // Others sorted by weight: 2 (0.9), 4 (0.7), 5 (0.6), 1 (0.5).
        // Two slots, two bins of two; each bin takes its LOWER-weight
        // middle: [2, 4] -> 4, [5, 1] -> 1.
        let members = [(1i64, 0.5f64), (2, 0.9), (3, 1.0), (4, 0.7), (5, 0.6)];
        let chosen = select_calibration_frames(3, &members, 3);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![3, 4, 1], "{chosen:?}");
        assert_eq!(
            chosen.iter().filter(|&&i| members[i].0 == 3).count(),
            1,
            "the reference must not also be picked as a bin member"
        );
    }

    #[test]
    fn a_reference_that_is_not_a_member_leaves_every_slot_to_the_bins() {
        // The co-registered case: the run-wide reference belongs to another
        // group, so all `want` slots are bins. Three others, two slots ->
        // bins of 2 and 1: [2 (0.9), 4 (0.7)] -> 4, [1 (0.5)] -> 1.
        let members = [(1i64, 0.5f64), (2, 0.9), (4, 0.7)];
        let chosen = select_calibration_frames(99, &members, 2);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![4, 1]);
    }

    #[test]
    fn a_group_with_no_more_others_than_slots_measures_all_of_them() {
        // The floor, unchanged from C-14: six others and six slots (the
        // reference takes the seventh) -> every one of them, in weight
        // order, with no binning at all.
        let mut members = ranked_members(7);
        let reference = members[0].0;
        let chosen = select_calibration_frames(reference, &members, SEEDS_CALIBRATION_FRAMES);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4, 5, 6]);

        // One more member than slots and the bins engage.
        members = ranked_members(8);
        let chosen = select_calibration_frames(members[0].0, &members, SEEDS_CALIBRATION_FRAMES);
        assert_eq!(chosen.len(), SEEDS_CALIBRATION_FRAMES);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        // Seven others, six bins: sizes 2,1,1,1,1,1 -> [1,2]->2, then 3..7.
        assert_eq!(ids, vec![0, 2, 3, 4, 5, 6, 7]);
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
    fn seven_frames_span_the_weight_range_one_per_sextile() {
        // 20 members, the reference (id 0) the BEST-weighted. Six bins over
        // the 19 others: sizes 4,3,3,3,3,3, each yielding its median
        // position -> ranks 3, 6, 9, 12, 15, 18 of the others.
        let members = ranked_members(20);
        let chosen = select_calibration_frames(0, &members, SEEDS_CALIBRATION_FRAMES);
        assert_eq!(chosen.len(), SEEDS_CALIBRATION_FRAMES);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![0, 3, 6, 9, 12, 15, 18]);
    }

    /// Ruling C-15's own reason, as a pin: the sample must COVER the
    /// group's weight range, which "the six best-weighted" does not. On a
    /// realistic 92-member group (the acceptance catalog's mono group) the
    /// stratified sample's own weight span is nearly the whole group's,
    /// while the top six span a sliver of it.
    #[test]
    fn the_stratified_sample_covers_the_range_the_top_six_did_not() {
        let members = ranked_members(92);
        let group_span = members.first().unwrap().1 - members.last().unwrap().1;

        let chosen = select_calibration_frames(members[0].0, &members, SEEDS_CALIBRATION_FRAMES);
        let weights: Vec<f64> = chosen.iter().map(|&i| members[i].1).collect();
        let span = weights.iter().cloned().fold(f64::MIN, f64::max)
            - weights.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            span > 0.9 * group_span,
            "stratified span {span} must cover the group's {group_span}"
        );

        // What C-14 did: the reference plus the six best-weighted others.
        let top_six: Vec<f64> = members[..SEEDS_CALIBRATION_FRAMES]
            .iter()
            .map(|(_, w)| *w)
            .collect();
        let top_span = top_six.iter().cloned().fold(f64::MIN, f64::max)
            - top_six.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            top_span < 0.1 * group_span,
            "the top-six span {top_span} should be a sliver of {group_span}"
        );
    }

    #[test]
    fn uneven_bins_put_the_remainder_in_the_leading_bins() {
        // 14 others over 6 bins: 14 = 2*6 + 2, so bins 0 and 1 take three
        // members and the rest two. Medians land at ranks 1, 4, 7, 9, 11,
        // 13 of the others (ids 2, 5, 8, 10, 12, 14 with the reference at
        // id 0).
        let members = ranked_members(15);
        let chosen = select_calibration_frames(0, &members, SEEDS_CALIBRATION_FRAMES);
        let ids: Vec<i64> = chosen.iter().map(|&i| members[i].0).collect();
        assert_eq!(ids, vec![0, 2, 5, 8, 10, 12, 14]);

        // Every bin contributes exactly one member and no member twice.
        let mut sorted = chosen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), chosen.len(), "no member may be picked twice");
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
        // C-17: `k_for` is the apply path's own guard, so a stored payload
        // carrying a value no multiplier may use reads as 1.0 rather than
        // poisoning a channel's `A`.
        let broken = SeedsCalibration {
            frame_ids: vec![1],
            k: vec![f64::NAN, -0.5, 0.0, 0.99],
        };
        assert_eq!(broken.k_for(0), 1.0, "NaN");
        assert_eq!(broken.k_for(1), 1.0, "negative");
        assert_eq!(broken.k_for(2), 1.0, "zero");
        assert_eq!(broken.k_for(3), 0.99);
    }

    #[test]
    fn no_channels_at_all_produce_no_factors() {
        assert!(calibration_k(&[]).is_empty());
    }
}
