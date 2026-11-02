//! The frame-common star set (Plan D, D1; ruling PD-3): the stars that are unclipped and measurable in (almost) every
//! included, aligned member of one (group, plane), and each member's flux over exactly those stars.
//!
//! Why: a frame's PSF-family weight reads its total starlight (`TFlux`, `TMeanFlux`). That total follows the seeing:
//! clipped stars carry most of it and are extrapolated from their wings, and stars stop clipping as the seeing
//! softens. The flux of the SAME unclipped stars in every frame follows the sky's transparency only. No refusal of a
//! clipped star can move it. A blended star's refusal (Plan D D3) leaves the `.athf`, so it would also leave the anchor's
//! isolation test; Measure marks the kept fits within that test's radius of it (`StarFlags::BLEND_NEIGHBOUR`, PD-36) and
//! [`candidates`] reads the mark as "not isolated", so the set is the one the unrefused fits gave.
//!
//! Pure logic, no I/O and no weights. The caller (`run.rs::reweigh_groups`, D1w) reads one member's `.athf` fits at a
//! time and hands the module their [`MemberPlane`]. On a THREE-plane image (PD-32: [`APERTURE_SUM_PLANES`], the gate of the
//! sibling rescue) the flux the paired ratios read is each fit's aperture sum `S`
//! (`StarFit::ap_sum`, Plan D D2b, PD-26; its aperture Plan D D2c, PD-30): the sum of `v - B_i` over the pixels within
//! 3 x the frame's ring FWHM of the star, one radius for the frame and none from the fit, read in Measure's HFR walk and
//! persisted, never a second estimate. It is the pixels' sum, so it carries neither the model's bias nor `c` (the model
//! flux times `c` ran +6-7 %/px of FWHM against the grey truth on the beta-floor R plane), and its aperture does not follow
//! the frame's profile (S at 1.5 x the fit's FWHM did, +5-13 %/px). The anchor's whole-set sums, which the ratios scale,
//! and everything else Measure reads stay the model flux times `c` (`StarFit::signal`): the weight's absolute scale is
//! the one the master's PSF SNR is read on. A fit is eligible when it is not clipped and not extended, an extended one
//! only where a point-like seed of another plane sits at it (`StarFlags::in_common_set`, PD-30). Any other plane count
//! reads what the module read before D2b (971606c8): the model flux times `c` (`signal`, `signal / area`), `in_statistic()` as
//! the flag rule, no condition on `S` ([`common_flux_model`]); `S` fixes the beta-floor plane of a colour image and was
//! measured there only (PD-26, PD-30).
//!
//! Steps, in order:
//! 1. [`pick_anchor`]: one member per group, picked without any weight (PD-1).
//! 2. [`candidates`]: the anchor's isolated, unflagged, bright-enough fits, mapped into group coordinates.
//! 3. [`match_member`]: per member, which candidate has a unique, usable fit (through that member's own map).
//! 4. [`common_flux`]: the set (usable in at least [`COMMON_MIN_PRESENCE`] of the members) and every member's paired
//!    flux ratio to the anchor over the stars it has.

use crate::geometry::{KdTree2, PixelMap};
use crate::stacking::measure::ADU_SCALE;
use crate::star_measure::photometry::{
    CORRECTION_ISOLATION_PER_FIELD_FWHM, CORRECTION_MIN_SNR, POINT_SOURCE_MATCH_PER_FIELD_FWHM,
    POINT_SOURCE_MATCH_PX,
};
use crate::star_measure::StarFit;

/// Fewer common stars than this: the (group, plane) has no common flux (PD-3, PD-6).
pub const COMMON_MIN_STARS: usize = 20;
/// A star joins the set when it is usable in at least this share of the included, aligned members (PD-3).
pub const COMMON_MIN_PRESENCE: f64 = 0.9;
/// A set under this share of the anchor's candidates is logged (PD-39): candidates enter group coordinates through the
/// anchor's one registration map ([`candidates`]), so a map that is off by more than the match radius over part of the
/// frame removes those stars from every member at once. runB mono L read 0.54 on a wrong anchor map and 0.96 on the
/// repaired one.
pub const COMMON_WARN_SHARE: f64 = 0.8;

/// Whether a set of `stars` out of `candidates` is under [`COMMON_WARN_SHARE`] of them (PD-39). A caller with no
/// candidate has no set to judge.
pub fn set_is_small_against_candidates(stars: usize, candidates: usize) -> bool {
    candidates > 0 && (stars as f64) < COMMON_WARN_SHARE * candidates as f64
}

/// One member's plane, as the module reads it. Nothing is owned: the caller keeps one member's fits resident at a time.
#[derive(Debug, Clone, Copy)]
pub struct MemberPlane<'a> {
    /// The plane's accepted fits, as `.athf` stores them (`signal` in the module's ADU units, `area` in px²).
    pub fits: &'a [StarFit],
    /// The member's registration map, frame pixel to group pixel (`PixelMap::forward_exact`; no grid is built).
    pub map: &'a PixelMap,
    /// The plane's field FWHM, px (`ChannelMeasurement::fwhm_px`).
    pub fwhm_px: f64,
    /// The plane's noise σ_N in native units (`ChannelMeasurement::noise`).
    pub noise: f64,
    /// `aperture_correction_group` of a plane at floor (i), else 1: the fits' `signal` of such a plane carries no `c`,
    /// and `take_group_c` scales the plane's totals by this (PD-3).
    pub c_stand_in: f64,
    /// The image's plane count (`run.rs::reweigh_group`'s `nch`; the master's and the best sub's `GroupInput::channels`): three
    /// planes weigh by the aperture sum, any other count by the model flux times `c` (PD-32). Every member of one call
    /// chain ([`candidates`], [`match_member`], [`common_flux`]) carries the same count.
    pub planes: usize,
}

/// One of the anchor's candidate stars: group coordinates, native flux units. `signal` and `mean` are the model flux
/// (and its mean over the FWTM area) with the stand-in `c` applied: the scale of the anchor's whole-set sums. `ap_sum` and
/// `ap_mean` are what the paired ratios read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub x: f64,
    pub y: f64,
    pub signal: f64,
    pub mean: f64,
    /// The fit's aperture sum `S` and `S / area`, native units, no `c` (Plan D D2b, PD-26); finite on every candidate of a
    /// three-plane image, NaN where the fit has none on any other (PD-32: nothing reads it there).
    pub ap_sum: f64,
    pub ap_mean: f64,
    /// The anchor plane's field FWHM, px: the match radius of every member reads it ([`match_member`]).
    pub anchor_fwhm_px: f64,
}

/// A candidate's fit in one member: native flux units, the member's stand-in `c` applied to `signal` and `mean` (the
/// model flux, which D1b's paired gain and PSF SNRs read) and not to the aperture sum, which the weight reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usable {
    pub signal: f64,
    pub mean: f64,
    /// The fit's aperture sum `S` and `S / area`, native units, no `c` (Plan D D2b, PD-26); NaN where `S` is not finite
    /// and positive: the fit is then not usable for the weight ([`common_flux`]), though its model flux still is.
    pub ap_sum: f64,
    pub ap_mean: f64,
}

/// One member's flux over the common set, native units: `tflux` and `tmean_flux` are the paired ratio to the anchor (of
/// the aperture sums `S`, resp. `S / area`) times the anchor's whole-set sums (of the model flux times `c`), `paired` is
/// how many set stars the member has with a usable `S`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommonFlux {
    pub tflux: f64,
    pub tmean_flux: f64,
    pub paired: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommonOutcome {
    /// `positions` are the set's stars in group coordinates; `per_member` is in the order of `usable`.
    Measured {
        positions: Vec<(f64, f64)>,
        per_member: Vec<CommonFlux>,
    },
    /// Fewer than [`COMMON_MIN_STARS`] stars are common: no common flux for this (group, plane).
    TooFewStars { stars: usize },
    /// The set holds [`COMMON_MIN_STARS`] stars or more but the anchor's aperture sums `S` over it, or its `S / area` over it,
    /// add to zero or less (Plan D D2c, PD-30: an `S` at or under the sky is noise and is summed, not dropped): there is no
    /// flux for the members' ratios to scale, so no common flux for this (group, plane), like [`CommonOutcome::TooFewStars`].
    NoAnchorFlux { stars: usize },
    /// The match table does not fit the candidates or the anchor: a wiring bug of the caller, never a property of the
    /// sky. A caller must not read it as "under 20 stars" (PD-6's fallback reason).
    MalformedTable,
}

/// The group's anchor: among the members whose summed unsaturated-star count is at least the group median, the one
/// with the lowest mean FWHM (ties: the lower `frame_id`). Returns its index, `None` when no member qualifies (no
/// member, a length mismatch, or no finite positive FWHM among the members at or above the median).
pub fn pick_anchor(mean_fwhm: &[f64], unsaturated: &[usize], frame_ids: &[i64]) -> Option<usize> {
    let n = mean_fwhm.len();
    if unsaturated.len() != n || frame_ids.len() != n {
        tracing::error!(
            members = n,
            unsaturated = unsaturated.len(),
            frame_ids = frame_ids.len(),
            "common set: anchor inputs differ in length"
        );
        return None;
    }
    if n == 0 {
        return None;
    }
    let mut sorted = unsaturated.to_vec();
    sorted.sort_unstable();
    let median = if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    };
    (0..n)
        .filter(|&i| {
            unsaturated[i] as f64 >= median && mean_fwhm[i].is_finite() && mean_fwhm[i] > 0.0
        })
        .min_by(|&a, &b| {
            mean_fwhm[a]
                .total_cmp(&mean_fwhm[b])
                .then(frame_ids[a].cmp(&frame_ids[b]))
        })
}

/// The plane count at which the weight's flux term is the aperture sum `S` (Plan D D2c, PD-32): the same "exactly three planes"
/// as `star_measure::point_like_sibling_seeds`' gate. `S` fixes the beta-floor plane of a colour image (PD-26, measured there
/// only) and the sibling rescue (PD-30) exists there only; any other count reads the model flux times `c`, as the module did
/// before D2b (971606c8).
pub const APERTURE_SUM_PLANES: usize = 3;

/// Whether an image of `planes` planes weighs by the aperture sum (PD-32); otherwise by the model flux times `c`.
fn weighs_by_aperture_sum(planes: usize) -> bool {
    planes == APERTURE_SUM_PLANES
}

/// `c_stand_in` as a factor: a non-finite or non-positive value (never produced by `aperture_correction_group`) reads as 1.
fn c_factor(c: f64) -> f64 {
    if c.is_finite() && c > 0.0 {
        c
    } else {
        tracing::warn!(c_stand_in = c, "common set: unusable c stand-in read as 1");
        1.0
    }
}

/// A fit the set may read the flux of, finite positive `signal` and `area` and, by the flags: on a three-plane image
/// `in_common_set()` (not clipped, and not extended unless PD-9's sibling test rescued it: `SIBLING_POINT`), on any other
/// `in_statistic()` (neither clipped nor extended), the rule the module had before D2c (971606c8, PD-32).
fn flux_fit(f: &StarFit, planes: usize) -> bool {
    let flagged_in = if weighs_by_aperture_sum(planes) {
        f.flags.in_common_set()
    } else {
        f.flags.in_statistic()
    };
    flagged_in && f.signal.is_finite() && f.signal > 0.0 && f.area.is_finite() && f.area > 0.0
}

/// A fit's aperture sum `S` in native units, `None` unless it is finite (Plan D D2b, PD-30): a fit with no ring sky, or a
/// disc that leaves the plane or holds an absent pixel, has none and is not usable for the weight. A sum at or under the sky
/// is noise, not a missing value: it is usable and enters the sums as it is.
fn aperture_sum(f: &StarFit) -> Option<f64> {
    let s = f.ap_sum / ADU_SCALE as f64;
    s.is_finite().then_some(s)
}

/// A usable aperture sum as the ratios read it (the `Candidate` / `Usable` fields).
fn usable_ap(s: f64) -> bool {
    s.is_finite()
}

/// The anchor's candidate stars in group coordinates: a [`flux_fit`] (on a three-plane image `in_common_set()`: not clipped,
/// not extended unless rescued by a point-like sibling seed; on any other count `in_statistic()`), finite positive `signal`
/// and `area`, no other anchor fit (of any flag) within [`CORRECTION_ISOLATION_PER_FIELD_FWHM`] x the plane's `fwhm_px`, no
/// [`crate::star_measure::StarFlags::BLEND_NEIGHBOUR`] mark (on every plane count: a fit the blend refusal removed from the
/// list lay within that same radius, and Measure set the mark from the same width, PD-36), and the
/// background-limited aperture SNR `signal / (noise * sqrt(area))` at least [`CORRECTION_MIN_SNR`] (the `.athf` stores no SNR;
/// the screen stays on the model flux, as before D2b), and, on a three-plane image only, since its ratios divide by it, a
/// finite aperture sum ([`aperture_sum`]: one at or under the sky is a candidate too). On any other count the candidate carries
/// the fit's `S` when it has one and NaN when it has none, and nothing reads it ([`common_flux`]). Candidates keep the anchor's
/// fit order.
pub fn candidates(anchor: &MemberPlane<'_>) -> Vec<Candidate> {
    let f = anchor.fwhm_px;
    if !(f.is_finite() && f > 0.0) || !(anchor.noise.is_finite() && anchor.noise > 0.0) {
        return Vec::new();
    }
    let c = c_factor(anchor.c_stand_in);
    let by_aperture = weighs_by_aperture_sum(anchor.planes);
    // Isolation reads every fit with a position, flagged or not: a clipped neighbour contaminates a star's wings.
    let mut at: Vec<usize> = Vec::with_capacity(anchor.fits.len());
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(anchor.fits.len());
    for (i, fit) in anchor.fits.iter().enumerate() {
        if fit.x.is_finite() && fit.y.is_finite() {
            at.push(i);
            pts.push((fit.x, fit.y));
        }
    }
    let tree = KdTree2::build(&pts);
    let isolation = CORRECTION_ISOLATION_PER_FIELD_FWHM * f;
    let adu = ADU_SCALE as f64;
    let mut out = Vec::new();
    for (k, &i) in at.iter().enumerate() {
        let fit = &anchor.fits[i];
        if !flux_fit(fit, anchor.planes) {
            continue;
        }
        let signal = fit.signal / adu;
        if signal / (anchor.noise * fit.area.sqrt()) < CORRECTION_MIN_SNR {
            continue;
        }
        let ap = aperture_sum(fit);
        if by_aperture && ap.is_none() {
            continue;
        }
        // The fit itself is always within the radius: isolated means it is the only one. A fit the blend refusal removed
        // from the list (so from this tree) counts as another one: Measure marked its neighbours (Plan D D3, PD-36).
        if tree.within(pts[k].0, pts[k].1, isolation).len() != 1 || fit.flags.blend_neighbour() {
            continue;
        }
        let (x, y) = anchor.map.forward_exact(fit.x, fit.y);
        if !(x.is_finite() && y.is_finite()) {
            continue;
        }
        out.push(Candidate {
            x,
            y,
            signal: signal * c,
            mean: signal / fit.area * c,
            ap_sum: ap.unwrap_or(f64::NAN),
            ap_mean: ap.map_or(f64::NAN, |s| s / fit.area),
            anchor_fwhm_px: f,
        });
    }
    out
}

/// For each candidate, the member's fit if there is exactly one within the match radius in group coordinates (two
/// within the radius leave the candidate unmatched in this member) and it is a [`flux_fit`]: on a three-plane image
/// `in_common_set()` (not saturated, and not extended unless `SIBLING_POINT` rescued it; `in_statistic()` is the narrower rule
/// the frame statistic reads), on any other count `in_statistic()`, with a finite positive `signal` and `area`. The radius is
/// `max(POINT_SOURCE_MATCH_PX, POINT_SOURCE_MATCH_PER_FIELD_FWHM x max(F_anchor, F_member))`. Every member fit with a position
/// counts for the uniqueness, clipped or extended ones too.
///
/// The fit's aperture sum rides along (`ap_sum`, `ap_mean`; NaN where it has none, [`Usable`]): D1b's paired gain reads
/// the model flux of the same match, and the weight ([`common_flux`]) leaves a fit without a sum out of its own pair.
pub fn match_member(cands: &[Candidate], member: &MemberPlane<'_>) -> Vec<Option<Usable>> {
    let mut out = vec![None; cands.len()];
    if cands.is_empty() {
        return out;
    }
    let c = c_factor(member.c_stand_in);
    let adu = ADU_SCALE as f64;
    let mut at: Vec<usize> = Vec::with_capacity(member.fits.len());
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(member.fits.len());
    for (i, fit) in member.fits.iter().enumerate() {
        if !(fit.x.is_finite() && fit.y.is_finite()) {
            continue;
        }
        let (gx, gy) = member.map.forward_exact(fit.x, fit.y);
        if gx.is_finite() && gy.is_finite() {
            at.push(i);
            pts.push((gx, gy));
        }
    }
    let tree = KdTree2::build(&pts);
    for (slot, cand) in out.iter_mut().zip(cands) {
        let radius = POINT_SOURCE_MATCH_PX
            .max(POINT_SOURCE_MATCH_PER_FIELD_FWHM * cand.anchor_fwhm_px.max(member.fwhm_px));
        let hits = tree.within(cand.x, cand.y, radius);
        if hits.len() != 1 {
            continue;
        }
        let fit = &member.fits[at[hits[0].0]];
        if flux_fit(fit, member.planes) {
            let signal = fit.signal / adu;
            let ap = aperture_sum(fit);
            *slot = Some(Usable {
                signal: signal * c,
                mean: signal / fit.area * c,
                ap_sum: ap.unwrap_or(f64::NAN),
                ap_mean: ap.map_or(f64::NAN, |s| s / fit.area),
            });
        }
    }
    out
}

/// The common set and every member's flux over it. `usable[k]` is member k's [`match_member`] result (the anchor
/// included, in member order), `anchor` the anchor's index in that order.
///
/// A candidate joins the set when it is usable in at least [`COMMON_MIN_PRESENCE`] of the members, usable meaning a
/// matched fit with a finite aperture sum ([`Usable::ap_sum`]); fewer than [`COMMON_MIN_STARS`] such stars leave no
/// common flux ([`CommonOutcome::TooFewStars`]), and so does a set whose anchor sums `S` or `S / area` add to zero or
/// less ([`CommonOutcome::NoAnchorFlux`]). Member f's flux is the paired ratio to the anchor over the set stars f has (`r_f` for
/// the flux, `q_f` for the mean flux), taken on the aperture sums `S` and `S / area` (Plan D D2b, PD-26), times the anchor's
/// sums over the whole set (the model flux times `c`: the scale the master's PSF SNR is read on), so a frame that lost a
/// third of its stars to cloud still weighs by its transparency, not by the stars it lost. A fit of f without a finite `S`
/// is not usable for f: it leaves f's own pair, both sides of f's ratio, and counts against the star's presence; an `S` at
/// or under the sky is noise (PD-30) and is summed as it is. A member with no paired star, or whose paired sums are not
/// positive (a ratio of two negative sums must not read as a positive weight), reads 0. The anchor's own entry is the
/// anchor's sums, exactly.
///
/// A table that does not fit (`usable` rows not as long as `cands`, or `anchor` past the table) is a wiring bug of the
/// caller: an `error!`, a `debug_assert!`, and [`CommonOutcome::MalformedTable`] in a release build.
///
/// All of the above is a three-plane image (`planes`, PD-32). Any other count reads the model flux times `c` as the module
/// did at 971606c8 ([`common_flux_model`]): present means a matched fit, the ratios are of `signal` and `signal / area`, a
/// member whose paired sums are not positive reads 0 with no star paired, and the anchor's sums are never tested
/// ([`CommonOutcome::NoAnchorFlux`] is a three-plane outcome).
pub fn common_flux(
    cands: &[Candidate],
    usable: &[Vec<Option<Usable>>],
    anchor: usize,
    planes: usize,
) -> CommonOutcome {
    if anchor >= usable.len() || usable.iter().any(|u| u.len() != cands.len()) {
        tracing::error!(
            candidates = cands.len(),
            members = usable.len(),
            anchor,
            "common set: the match table does not fit the candidates or the anchor"
        );
        debug_assert!(
            false,
            "common set: the match table does not fit the candidates or the anchor"
        );
        return CommonOutcome::MalformedTable;
    }
    if !weighs_by_aperture_sum(planes) {
        return common_flux_model(cands, usable, anchor);
    }
    let members = usable.len() as f64;
    let set: Vec<usize> = (0..cands.len())
        .filter(|&j| {
            if !usable_ap(cands[j].ap_sum) {
                return false;
            }
            let present = usable
                .iter()
                .filter(|u| u[j].is_some_and(|f| usable_ap(f.ap_sum)))
                .count();
            present as f64 / members >= COMMON_MIN_PRESENCE
        })
        .collect();
    if set.len() < COMMON_MIN_STARS {
        return CommonOutcome::TooFewStars { stars: set.len() };
    }
    // The ratios scale the anchor's sums over the set, S and S / area; a set the anchor sees at or under the sky has none
    // (PD-30). Both must be positive: a positive S with a non-positive S / area would read every fully paired member as
    // (0, 0) below and cut all of them but the anchor (review m3 of D2c).
    let anchor_ap: f64 = set.iter().map(|&j| cands[j].ap_sum).sum();
    let anchor_ap_mean: f64 = set.iter().map(|&j| cands[j].ap_mean).sum();
    if !(anchor_ap > 0.0 && anchor_ap_mean > 0.0) {
        return CommonOutcome::NoAnchorFlux { stars: set.len() };
    }
    let anchor_signal: f64 = set.iter().map(|&j| cands[j].signal).sum();
    let anchor_mean: f64 = set.iter().map(|&j| cands[j].mean).sum();
    let per_member = usable
        .iter()
        .enumerate()
        .map(|(k, u)| {
            if k == anchor {
                return CommonFlux {
                    tflux: anchor_signal,
                    tmean_flux: anchor_mean,
                    paired: set.len(),
                };
            }
            let (mut sig, mut sig_a, mut mean, mut mean_a, mut paired) =
                (0.0, 0.0, 0.0, 0.0, 0usize);
            for &j in &set {
                if let Some(f) = u[j].filter(|f| usable_ap(f.ap_sum)) {
                    sig += f.ap_sum;
                    sig_a += cands[j].ap_sum;
                    mean += f.ap_mean;
                    mean_a += cands[j].ap_mean;
                    paired += 1;
                }
            }
            if paired == 0 {
                return CommonFlux {
                    tflux: 0.0,
                    tmean_flux: 0.0,
                    paired: 0,
                };
            }
            // Pairs, but no positive flux to read a ratio of (the paired sums are noise): zero, not a negative flux.
            if !(sig_a > 0.0 && mean_a > 0.0 && sig > 0.0 && mean > 0.0) {
                return CommonFlux {
                    tflux: 0.0,
                    tmean_flux: 0.0,
                    paired,
                };
            }
            CommonFlux {
                tflux: sig / sig_a * anchor_signal,
                tmean_flux: mean / mean_a * anchor_mean,
                paired,
            }
        })
        .collect();
    CommonOutcome::Measured {
        positions: set.iter().map(|&j| (cands[j].x, cands[j].y)).collect(),
        per_member,
    }
}

/// [`common_flux`] for an image that is not three planes (PD-32): `common_set.rs` at 971606c8 verbatim, the table already
/// checked. A candidate joins the set when a matched fit is there in at least [`COMMON_MIN_PRESENCE`] of the members; member f's
/// flux is the paired ratio to the anchor of the model flux times `c` (`signal`, and `signal / area` for the mean) times the
/// anchor's sums over the whole set; a member with no paired star, or whose paired anchor sums are not positive, reads 0 with
/// `paired` 0. No condition on `S`: a fit without one is a star like any other here.
fn common_flux_model(
    cands: &[Candidate],
    usable: &[Vec<Option<Usable>>],
    anchor: usize,
) -> CommonOutcome {
    let members = usable.len() as f64;
    let set: Vec<usize> = (0..cands.len())
        .filter(|&j| {
            let present = usable.iter().filter(|u| u[j].is_some()).count();
            present as f64 / members >= COMMON_MIN_PRESENCE
        })
        .collect();
    if set.len() < COMMON_MIN_STARS {
        return CommonOutcome::TooFewStars { stars: set.len() };
    }
    let anchor_signal: f64 = set.iter().map(|&j| cands[j].signal).sum();
    let anchor_mean: f64 = set.iter().map(|&j| cands[j].mean).sum();
    let per_member = usable
        .iter()
        .enumerate()
        .map(|(k, u)| {
            if k == anchor {
                return CommonFlux {
                    tflux: anchor_signal,
                    tmean_flux: anchor_mean,
                    paired: set.len(),
                };
            }
            let (mut sig, mut sig_a, mut mean, mut mean_a, mut paired) =
                (0.0, 0.0, 0.0, 0.0, 0usize);
            for &j in &set {
                if let Some(f) = u[j] {
                    sig += f.signal;
                    sig_a += cands[j].signal;
                    mean += f.mean;
                    mean_a += cands[j].mean;
                    paired += 1;
                }
            }
            if paired == 0 || !(sig_a > 0.0 && mean_a > 0.0) {
                return CommonFlux {
                    tflux: 0.0,
                    tmean_flux: 0.0,
                    paired: 0,
                };
            }
            CommonFlux {
                tflux: sig / sig_a * anchor_signal,
                tmean_flux: mean / mean_a * anchor_mean,
                paired,
            }
        })
        .collect();
    CommonOutcome::Measured {
        positions: set.iter().map(|&j| (cands[j].x, cands[j].y)).collect(),
        per_member,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{DistortionModel, Linear, LinearKind, ThinPlateSpline};
    use crate::star_measure::StarFlags;

    const W: f64 = 2000.0;
    const H: f64 = 1500.0;
    const NOISE: f64 = 1e-3;
    /// `area = AREA_PER_FWHM2 · FWHM²`.
    const AREA_PER_FWHM2: f64 = 7.0;
    const ADU: f64 = ADU_SCALE as f64;
    /// The plane count of the colour images the aperture-sum weight is for (PD-32); every fixture is one unless it says so.
    const OSC_PLANES: usize = 3;

    struct SplitMix(u64);

    impl SplitMix {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// Uniform in `[0, 1)`.
        fn unit(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// One star of the synthetic sky: group coordinates and native flux.
    #[derive(Clone, Copy)]
    struct Star {
        x: f64,
        y: f64,
        flux: f64,
    }

    /// `n` stars on a jittered 20 × 15 lattice (cells of 100 px, jitter ±25 px: at least 50 px apart, far outside 6 F
    /// for every FWHM here), fluxes log-uniform over ×1000.
    fn sky(n: usize, seed: u64) -> Vec<Star> {
        let mut rng = SplitMix(seed);
        (0..n)
            .map(|i| {
                let (cx, cy) = ((i % 20) as f64, (i / 20) as f64);
                Star {
                    x: (cx + 0.5) * 100.0 + (rng.unit() - 0.5) * 50.0,
                    y: (cy + 0.5) * 100.0 + (rng.unit() - 0.5) * 50.0,
                    flux: 10f64.powf(3.0 * rng.unit()),
                }
            })
            .collect()
    }

    /// An exposure of the sky: transparency, field FWHM, the peak level above which a star clips, the position jitter
    /// (px) of the fitter and the seed of that jitter.
    #[derive(Clone, Copy)]
    struct Shot {
        t: f64,
        fwhm: f64,
        clip: f64,
        jitter: f64,
        seed: u64,
    }

    impl Shot {
        fn new(t: f64, fwhm: f64) -> Shot {
            Shot {
                t,
                fwhm,
                clip: f64::INFINITY,
                jitter: 0.0,
                seed: 1,
            }
        }
        fn area(&self) -> f64 {
            AREA_PER_FWHM2 * self.fwhm * self.fwhm
        }
    }

    fn fit_at(
        x: f64,
        y: f64,
        signal_native: f64,
        area: f64,
        fwhm: f64,
        flags: StarFlags,
    ) -> StarFit {
        StarFit {
            x,
            y,
            background: 0.0,
            amplitude: 1.0,
            fwhm_x: fwhm,
            fwhm_y: fwhm,
            fwtm_x: fwhm * 1.8,
            fwtm_y: fwhm * 1.8,
            theta: 0.0,
            beta: 3.0,
            residual: 0.02,
            signal: signal_native * ADU,
            area,
            peak5: f64::NAN,
            sky_ring: f64::NAN,
            sky_ring_sigma: f64::NAN,
            sky_ring_n: 0,
            // A fixture without an opinion on S: the aperture sum is the model's signal (c = 1), so the weight reads what
            // it read before D2b. The S tests move it on its own.
            ap_sum: signal_native * ADU,
            snr: f64::NAN,
            hfr: f64::NAN,
            flags,
        }
    }

    /// The fits one exposure of `sky` yields, in the member's own pixels (the identity map). A star whose peak
    /// (`signal / area`) exceeds `clip` is flagged saturated and its signal is extrapolated high (x 1.5), the way a
    /// clipped star's wings overstate it.
    fn fits_of(sky: &[Star], s: &Shot) -> Vec<StarFit> {
        let mut rng = SplitMix(s.seed);
        sky.iter()
            .map(|st| {
                let signal = s.t * st.flux;
                let sat = signal / s.area() > s.clip;
                let (dx, dy) = (
                    (rng.unit() - 0.5) * 2.0 * s.jitter,
                    (rng.unit() - 0.5) * 2.0 * s.jitter,
                );
                let (flags, signal) = if sat {
                    (StarFlags::SATURATED, signal * 1.5)
                } else {
                    (StarFlags::default(), signal)
                };
                fit_at(st.x + dx, st.y + dy, signal, s.area(), s.fwhm, flags)
            })
            .collect()
    }

    fn plane<'a>(fits: &'a [StarFit], map: &'a PixelMap, fwhm: f64) -> MemberPlane<'a> {
        MemberPlane {
            fits,
            map,
            fwhm_px: fwhm,
            noise: NOISE,
            c_stand_in: 1.0,
            planes: OSC_PLANES,
        }
    }

    fn identity() -> PixelMap {
        PixelMap::linear(Linear::identity()).expect("identity map")
    }

    /// The whole pipeline over identity-mapped members of a 3-plane image: candidates of `anchor`, every member's match, the
    /// outcome.
    fn run(
        members: &[(Vec<StarFit>, f64, f64)],
        anchor: usize,
    ) -> (Vec<Candidate>, Vec<Vec<Option<Usable>>>, CommonOutcome) {
        run_planes(members, anchor, OSC_PLANES)
    }

    /// [`run`] on an image of `image_planes` planes.
    fn run_planes(
        members: &[(Vec<StarFit>, f64, f64)],
        anchor: usize,
        image_planes: usize,
    ) -> (Vec<Candidate>, Vec<Vec<Option<Usable>>>, CommonOutcome) {
        let map = identity();
        let planes: Vec<MemberPlane<'_>> = members
            .iter()
            .map(|(fits, fwhm, c)| MemberPlane {
                c_stand_in: *c,
                planes: image_planes,
                ..plane(fits, &map, *fwhm)
            })
            .collect();
        let cands = candidates(&planes[anchor]);
        let usable: Vec<_> = planes.iter().map(|p| match_member(&cands, p)).collect();
        let out = common_flux(&cands, &usable, anchor, image_planes);
        (cands, usable, out)
    }

    fn measured(out: &CommonOutcome) -> (&Vec<(f64, f64)>, &Vec<CommonFlux>) {
        match out {
            CommonOutcome::Measured {
                positions,
                per_member,
            } => (positions, per_member),
            other => panic!("expected a measured set, got {other:?}"),
        }
    }

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs().max(f64::MIN_POSITIVE)
    }

    #[test]
    fn a_common_set_flux_follows_transparency_not_width() {
        let world = sky(300, 11);
        let t = [1.0, 0.95, 0.9, 1.0, 0.85, 1.0];
        let fwhm = [2.0, 2.5, 3.0, 3.5, 4.0, 4.5];
        let members: Vec<_> = t
            .iter()
            .zip(&fwhm)
            .map(|(&t, &f)| {
                (
                    fits_of(
                        &world,
                        &Shot {
                            clip: 15.0,
                            ..Shot::new(t, f)
                        },
                    ),
                    f,
                    1.0,
                )
            })
            .collect();
        let clipped = |k: usize| members[k].0.iter().filter(|f| f.flags.saturated()).count();
        assert!(
            clipped(0) > 20,
            "the sharp member must clip a real share of the sky: {}",
            clipped(0)
        );
        assert_eq!(clipped(5), 0, "the widest member clips nothing");
        let (cands, _usable, out) = run(&members, 0);
        let (positions, per) = measured(&out);
        assert_eq!(
            cands.len(),
            300 - clipped(0),
            "the anchor's candidates are its unclipped stars"
        );
        assert_eq!(
            positions.len(),
            cands.len(),
            "every anchor star is unclipped in every wider member"
        );
        let a = per[0];
        for k in 0..members.len() {
            let ratio = per[k].tflux / a.tflux;
            assert!(
                rel(ratio, t[k]) < 1e-12,
                "member {k}: flux ratio {ratio} vs transparency {}",
                t[k]
            );
            let mean_ratio = per[k].tmean_flux / a.tmean_flux;
            let expect =
                t[k] * AREA_PER_FWHM2 * fwhm[0].powi(2) / (AREA_PER_FWHM2 * fwhm[k].powi(2));
            assert!(
                rel(mean_ratio, expect) < 1e-12,
                "member {k}: mean-flux ratio {mean_ratio} vs {expect}"
            );
            assert_eq!(per[k].paired, cands.len());
        }
        // The old total follows the width: the sum over all fits (clipped ones extrapolated) is not the transparency.
        let naive = |k: usize| members[k].0.iter().map(|f| f.signal).sum::<f64>();
        assert!(
            rel(naive(5) / naive(0), t[5]) > 0.01,
            "the plain total must be width-dependent in this scenario, or the test proves nothing"
        );
    }

    #[test]
    fn clipped_and_non_member_fits_never_move_a_frames_common_flux() {
        let world = sky(300, 12);
        let widths = [2.0, 2.3, 2.6, 2.9, 3.2, 3.5, 3.8, 4.1, 4.4, 4.7];
        let mut members: Vec<_> = widths
            .iter()
            .enumerate()
            .map(|(k, &f)| {
                let shot = Shot {
                    clip: 8.0,
                    jitter: 0.3,
                    seed: 40 + k as u64,
                    ..Shot::new(1.0 - 0.02 * k as f64, f)
                };
                (fits_of(&world, &shot), f, 1.0)
            })
            .collect();
        // Member 3 also has two set stars clipped (9 of 10 members still have them: they stay in the set), and it
        // clips real stars of its own (the flux > 610 ones), which the anchor clips too.
        let faint: Vec<usize> = (0..world.len())
            .filter(|&j| world[j].flux < 100.0)
            .take(2)
            .collect();
        assert_eq!(faint.len(), 2);
        members[3].0[faint[0]].flags = StarFlags::SATURATED;
        members[3].0[faint[1]].flags = StarFlags::SATURATED;
        assert!(members[3].0.iter().filter(|f| f.flags.saturated()).count() > 10);
        let (_cands, _usable, before) = run(&members, 0);
        let (positions, per_before) = measured(&before);
        assert!(positions.len() >= 100);
        let has = |j: usize| {
            positions
                .iter()
                .any(|&(x, y)| (x - world[j].x).abs() < 2.0 && (y - world[j].y).abs() < 2.0)
        };
        assert!(
            has(faint[0]) && has(faint[1]),
            "the two clipped stars of member 3 are still in the set"
        );
        // Member 3 loses its clipped fits and every fit that is not a star of the set.
        let in_set = |f: &StarFit| {
            positions
                .iter()
                .any(|&(px, py)| (px - f.x).abs() < 2.0 && (py - f.y).abs() < 2.0)
        };
        let mut trimmed = members.clone();
        trimmed[3].0.retain(|f| !f.flags.saturated() && in_set(f));
        assert!(
            trimmed[3].0.len() < members[3].0.len(),
            "the trim must remove something"
        );
        let (_c, _u, after) = run(&trimmed, 0);
        let (positions_after, per_after) = measured(&after);
        assert_eq!(positions, positions_after);
        assert_eq!(per_before[3].tflux.to_bits(), per_after[3].tflux.to_bits());
        assert_eq!(
            per_before[3].tmean_flux.to_bits(),
            per_after[3].tmean_flux.to_bits()
        );
        assert_eq!(per_before[3].paired, per_after[3].paired);
    }

    #[test]
    fn a_star_unusable_in_more_than_a_tenth_of_the_members_leaves_the_set() {
        let world = sky(300, 13);
        let mut members: Vec<_> = (0..10)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        let sat = |m: &mut (Vec<StarFit>, f64, f64), j: usize| m.0[j].flags = StarFlags::SATURATED;
        // Star 5: clipped in two of ten members (80 % present: out). Star 6: clipped in one (90 %: in).
        sat(&mut members[2], 5);
        sat(&mut members[7], 5);
        sat(&mut members[4], 6);
        let (cands, _usable, out) = run(&members, 0);
        assert_eq!(cands.len(), 300);
        let (positions, per) = measured(&out);
        let has = |j: usize| {
            positions
                .iter()
                .any(|&(x, y)| x == world[j].x && y == world[j].y)
        };
        assert!(!has(5), "star 5 is usable in 8 of 10 members: out");
        assert!(has(6), "star 6 is usable in 9 of 10 members: in");
        assert_eq!(positions.len(), 299);
        assert_eq!(per[0].paired, 299);
        assert_eq!(
            per[4].paired, 298,
            "the member with star 6 clipped pairs without it"
        );
        assert_eq!(
            per[2].paired, 299,
            "star 5 is not in the set: member 2 pairs every set star"
        );
        // Identical exposures: the paired ratio is exactly 1, so member 4 keeps the whole set's flux.
        assert_eq!(per[4].tflux.to_bits(), per[0].tflux.to_bits());
    }

    #[test]
    fn the_anchor_is_the_sharpest_member_with_at_least_the_median_star_count() {
        // The sharpest member (0) has the fewest unsaturated stars: out. Median 500; the next sharpest at or above it.
        let fwhm = [2.0, 2.5, 3.0, 3.5, 4.0];
        let unsat = [100, 500, 520, 480, 510];
        let ids = [10, 11, 12, 13, 14];
        assert_eq!(pick_anchor(&fwhm, &unsat, &ids), Some(1));
        // Ties in FWHM: the lower frame_id, wherever it sits in the order.
        assert_eq!(
            pick_anchor(&[2.0, 2.0, 2.0], &[300, 300, 300], &[9, 4, 7]),
            Some(1)
        );
        // An even count: the median is the mean of the two middle values (150), so only the second qualifies.
        assert_eq!(pick_anchor(&[2.0, 3.0], &[100, 200], &[1, 2]), Some(1));
        // A member with no usable FWHM never wins, however many stars it has.
        assert_eq!(
            pick_anchor(&[f64::NAN, 3.0, 0.0], &[900, 500, 900], &[1, 2, 3]),
            None
        );
        assert_eq!(
            pick_anchor(&[f64::NAN, 3.0, 4.0], &[400, 500, 500], &[1, 2, 3]),
            Some(1)
        );
        // Nothing to pick from, or inputs that disagree in length.
        assert_eq!(pick_anchor(&[], &[], &[]), None);
        assert_eq!(pick_anchor(&[2.0, 3.0], &[100], &[1, 2]), None);
    }

    #[test]
    fn a_floored_planes_stand_in_c_scales_only_the_anchors_sums() {
        // Moved by Plan D D2b (PD-26). Before: a floored plane's stand-in c multiplied its model flux, so its ratio to the
        // anchor read x 1.3 (and 1 / 1.3 with the anchor floored). The ratios now read S, which no c multiplies: a
        // floored member weighs by its pixels, and the stand-in survives only in the anchor's whole-set sums, the scale the
        // weight is read on (take_group_c's "tflux carries it once").
        let world = sky(100, 14);
        let members: Vec<_> = (0..4)
            .map(|k| {
                (
                    fits_of(&world, &Shot::new(1.0, 3.0)),
                    3.0,
                    if k == 2 { 1.3 } else { 1.0 },
                )
            })
            .collect();
        let (_c, _u, out) = run(&members, 0);
        let (_p, per) = measured(&out);
        for k in 1..4 {
            assert!(rel(per[k].tflux / per[0].tflux, 1.0) < 1e-12, "member {k}");
            assert!(
                rel(per[k].tmean_flux / per[0].tmean_flux, 1.0) < 1e-12,
                "member {k}"
            );
        }
        // The anchor itself is the floored plane: its sums carry the stand-in once, the others read relative to it.
        let members: Vec<_> = (0..4)
            .map(|k| {
                (
                    fits_of(&world, &Shot::new(1.0, 3.0)),
                    3.0,
                    if k == 0 { 1.3 } else { 1.0 },
                )
            })
            .collect();
        let (_c, _u, out) = run(&members, 0);
        let (_p, per) = measured(&out);
        let model: f64 = members[0].0.iter().map(|f| f.signal / ADU).sum();
        assert!(rel(per[0].tflux, 1.3 * model) < 1e-12);
        assert!(rel(per[1].tflux / per[0].tflux, 1.0) < 1e-12);
        assert!(rel(per[1].tflux, 1.3 * model) < 1e-12);
    }

    #[test]
    fn an_ambiguous_match_is_not_paired() {
        let world = sky(40, 15);
        let anchor = fits_of(&world, &Shot::new(1.0, 3.0));
        let mut member = fits_of(&world, &Shot::new(1.0, 3.0));
        let f = member[7];
        // Star 7 gains a second fit 0.8 px away (both within the 1.5 px radius): ambiguous.
        member.push(StarFit { x: f.x + 0.8, ..f });
        // Star 8's only fit moves to 1.2 px (inside the radius: paired); star 9's to 2.0 px (outside: unmatched).
        member[8].x += 1.2;
        member[9].x += 2.0;
        let map = identity();
        let cands = candidates(&plane(&anchor, &map, 3.0));
        assert_eq!(cands.len(), 40);
        let got = match_member(&cands, &plane(&member, &map, 3.0));
        assert!(got[7].is_none(), "two fits in the radius: unmatched");
        assert!(got[8].is_some(), "one fit 1.2 px away: paired");
        assert!(
            got[9].is_none(),
            "the only fit is outside the radius: unmatched"
        );
        assert_eq!(got.iter().filter(|g| g.is_some()).count(), 38);
    }

    #[test]
    fn the_match_radius_grows_with_the_field_fwhm() {
        let world = sky(40, 16);
        let anchor = fits_of(&world, &Shot::new(1.0, 3.0));
        let map = identity();
        let cands = candidates(&plane(&anchor, &map, 3.0));
        // A fit 2.4 px off: outside max(1.5, 0.5 x 3) = 1.5, inside max(1.5, 0.5 x 6) = 3 (member FWHM 6).
        let mut member = fits_of(&world, &Shot::new(1.0, 6.0));
        member[3].x += 2.4;
        assert!(match_member(&cands, &plane(&member, &map, 3.0))[3].is_none());
        assert!(match_member(&cands, &plane(&member, &map, 6.0))[3].is_some());
    }

    #[test]
    fn under_twenty_common_stars_there_is_no_common_flux() {
        for (n, stars) in [(19usize, Some(19usize)), (20, None)] {
            let world = sky(n, 17);
            let members: Vec<_> = (0..5)
                .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
                .collect();
            let (_c, _u, out) = run(&members, 0);
            match stars {
                Some(s) => assert_eq!(out, CommonOutcome::TooFewStars { stars: s }),
                None => assert_eq!(measured(&out).0.len(), 20, "20 stars is enough"),
            }
        }
    }

    #[test]
    fn a_member_without_a_matched_star_has_zero_common_flux() {
        let world = sky(60, 18);
        let mut members: Vec<_> = (0..10)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        members[6].0.clear();
        let (_c, _u, out) = run(&members, 0);
        let (positions, per) = measured(&out);
        assert!(
            !positions.is_empty(),
            "nine of ten members still carry the set"
        );
        assert_eq!(
            per[6],
            CommonFlux {
                tflux: 0.0,
                tmean_flux: 0.0,
                paired: 0
            }
        );
        assert!(per[5].tflux > 0.0);
    }

    #[test]
    fn a_cloudy_frame_weighs_by_its_transparency_on_the_stars_it_kept() {
        let world = sky(300, 19);
        let mut members: Vec<_> = (0..10)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        // Member 7: half the transparency, and every third fit lost (cloud over a third of the field).
        members[7].0 = fits_of(&world, &Shot::new(0.5, 3.0))
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 3 != 0)
            .map(|(_, f)| f)
            .collect();
        let (cands, _u, out) = run(&members, 0);
        let (positions, per) = measured(&out);
        // 9 of 10 members have each of the lost stars: the set keeps all 300.
        assert_eq!(positions.len(), 300);
        let whole: f64 = cands.iter().map(|c| c.signal).sum();
        assert!(
            rel(per[7].tflux, 0.5 * whole) < 1e-12,
            "r_f = 0.5 over the stars it kept: {}",
            per[7].tflux / whole
        );
        assert!(
            (per[7].paired as i64 - 200).abs() <= 1,
            "paired {}",
            per[7].paired
        );
        assert!(per[7].tflux > 0.0);
    }

    #[test]
    fn the_match_goes_through_a_flipped_registration_map() {
        let world = sky(200, 20);
        let anchor_fits = fits_of(&world, &Shot::new(1.0, 3.0));
        let ident = identity();
        let cands = candidates(&plane(&anchor_fits, &ident, 3.0));
        assert_eq!(cands.len(), 200);
        // The member was shot rotated by 180 degrees about the field centre and shifted by (17.3, -8.6): a star at
        // group (x, y) sits at (2 cx - x + 17.3, 2 cy - y - 8.6) in its pixels. Its map takes them back.
        let (sx, sy) = (17.3, -8.6);
        let lin = Linear {
            kind: LinearKind::Affine,
            m: [[-1.0, 0.0, W + sx], [0.0, -1.0, H + sy], [0.0, 0.0, 1.0]],
        };
        let flip = PixelMap::linear(lin).expect("rotation map");
        let member_fits: Vec<StarFit> = anchor_fits
            .iter()
            .map(|f| StarFit {
                x: W - f.x + sx,
                y: H - f.y + sy,
                ..*f
            })
            .collect();
        // Through the identity the rotated fits match nothing; through their own map every candidate pairs.
        let blind = match_member(&cands, &plane(&member_fits, &ident, 3.0));
        assert!(blind.iter().filter(|u| u.is_some()).count() < 5);
        let got = match_member(&cands, &plane(&member_fits, &flip, 3.0));
        assert!(
            got.iter().all(|u| u.is_some()),
            "every candidate is paired through the flipped map"
        );

        // A map with a thin-plate-spline layer (a smooth warp of about 1.5 px): the member's pixels are the exact
        // inverse of the map at each group position, so the same pairs come back.
        let warped = {
            let mut rng = SplitMix(99);
            let nodes: Vec<(f64, f64)> =
                (0..60).map(|_| (rng.unit() * W, rng.unit() * H)).collect();
            let field = |x: f64, y: f64| {
                (
                    1.5 * (x / 500.0).sin() * (y / 400.0).cos(),
                    1.5 * (x / 500.0).cos() * (y / 400.0).sin(),
                )
            };
            let (fx, fy): (Vec<f64>, Vec<f64>) = nodes.iter().map(|&(x, y)| field(x, y)).unzip();
            let (ix, iy): (Vec<f64>, Vec<f64>) = (
                fx.iter().map(|v| -v).collect(),
                fy.iter().map(|v| -v).collect(),
            );
            let forward = ThinPlateSpline::fit(&nodes, &fx, &fy, 0.0).expect("forward spline");
            let inverse = ThinPlateSpline::fit(&nodes, &ix, &iy, 0.0).expect("inverse spline");
            let b = forward.node_bounds();
            let (mx, my) = ((b[2] - b[0]) * 0.05, (b[3] - b[1]) * 0.05);
            let model = DistortionModel::tps(
                forward,
                inverse,
                [b[0] - mx, b[1] - my, b[2] + mx, b[3] + my],
            );
            PixelMap::with_distortion_model(lin, model).expect("tps map")
        };
        let warped_fits: Vec<StarFit> = anchor_fits
            .iter()
            .map(|f| {
                let (x, y) = warped.inverse_exact(f.x, f.y);
                StarFit { x, y, ..*f }
            })
            .collect();
        let got = match_member(&cands, &plane(&warped_fits, &warped, 3.0));
        assert!(
            got.iter().all(|u| u.is_some()),
            "every candidate is paired through the spline map"
        );
    }

    #[test]
    fn candidates_are_isolated_unflagged_and_above_the_snr_floor() {
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let signal_at = |snr: f64| snr * NOISE * area.sqrt();
        let bright = signal_at(500.0);
        let fit = |x: f64, y: f64, s: f64, flags: StarFlags| fit_at(x, y, s, area, f, flags);
        let fits = vec![
            fit(100.0, 100.0, bright, StarFlags::default()), // 0: ordinary: in
            fit(300.0, 100.0, bright, StarFlags::EXTENDED),  // 1: extended: out
            fit(500.0, 100.0, bright, StarFlags::default()), // 2: neighbour at 5.9 F: out
            fit(500.0 + 5.9 * f, 100.0, bright, StarFlags::default()), // 3: its neighbour: out
            fit(700.0, 100.0, bright, StarFlags::default()), // 4: neighbour at 6.1 F: in
            fit(700.0 + 6.1 * f, 100.0, bright, StarFlags::default()), // 5: its neighbour: in
            fit(900.0, 100.0, signal_at(19.9), StarFlags::default()), // 6: SNR 19.9: out
            fit(1100.0, 100.0, signal_at(20.1), StarFlags::default()), // 7: SNR 20.1: in
            fit(1300.0, 100.0, bright, StarFlags::default()), // 8: a clipped neighbour at 10 px: out
            fit(1310.0, 100.0, bright, StarFlags::SATURATED), // 9: clipped: out
            fit(1500.0, 100.0, f64::NAN, StarFlags::default()), // 10: no signal: out
            fit(1700.0, 100.0, bright, StarFlags::default()), // 11: ordinary: in
        ];
        let map = identity();
        let cands = candidates(&MemberPlane {
            c_stand_in: 1.2,
            ..plane(&fits, &map, f)
        });
        let xs: Vec<f64> = cands.iter().map(|c| c.x).collect();
        assert_eq!(xs, vec![100.0, 700.0, 700.0 + 6.1 * f, 1100.0, 1700.0]);
        // The stand-in c is in the candidate's flux; the SNR proxy reads the fit without it.
        assert!(rel(cands[0].signal, bright * 1.2) < 1e-12);
        assert!(rel(cands[0].mean, bright / area * 1.2) < 1e-12);
        assert_eq!(cands[0].anchor_fwhm_px, f);
        // A plane with no noise or no FWHM has no isolation radius or SNR to judge by: no candidates, never a panic.
        assert!(candidates(&MemberPlane {
            noise: 0.0,
            ..plane(&fits, &map, f)
        })
        .is_empty());
        assert!(candidates(&MemberPlane {
            fwhm_px: f64::NAN,
            ..plane(&fits, &map, f)
        })
        .is_empty());
    }

    /// Plan D D3 (PD-36): a candidate-worthy anchor fit that carries `BLEND_NEIGHBOUR` is not a candidate, on every plane
    /// count (the bit is read by the one test in the isolation loop, not by `flux_fit`), and the same fit without it is.
    #[test]
    fn a_blend_neighbour_is_not_a_candidate_on_any_plane_count() {
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let bright = 500.0 * NOISE * area.sqrt();
        let map = identity();
        let fits_with = |bit: StarFlags| {
            vec![
                fit_at(100.0, 100.0, bright, area, f, StarFlags::default()),
                fit_at(300.0, 100.0, bright, area, f, bit), // the one under test, alone in 6 F
                fit_at(500.0, 100.0, bright, area, f, StarFlags::default()),
            ]
        };
        for planes in [1usize, 2, 3, 4] {
            let at_planes = |fits: &[StarFit]| {
                candidates(&MemberPlane {
                    planes,
                    ..plane(fits, &map, f)
                })
                .iter()
                .map(|c| c.x)
                .collect::<Vec<f64>>()
            };
            assert_eq!(
                at_planes(&fits_with(StarFlags::default())),
                vec![100.0, 300.0, 500.0],
                "{planes} planes"
            );
            assert_eq!(
                at_planes(&fits_with(StarFlags::BLEND_NEIGHBOUR)),
                vec![100.0, 500.0],
                "{planes} planes, marked"
            );
            // The sibling-point rescue does not undo the mark: marked, extended and rescued is out on a three-plane image too.
            let both = fits_with(
                StarFlags::BLEND_NEIGHBOUR | StarFlags::EXTENDED | StarFlags::SIBLING_POINT,
            );
            assert_eq!(
                at_planes(&both),
                vec![100.0, 500.0],
                "{planes} planes, marked and extended"
            );
        }
    }

    /// Plan D D3 (PD-36), the equivalence: the candidates of a D3 fit list (the Blend-refused fit absent, its neighbours
    /// marked by Measure's `mark_blend_neighbours` at 6 x the plane's `fwhm_px`) are the candidates of the same list with the
    /// refused fit put back and no bit (d2d's isolation, where it was a kept fit). Each refused fit has a kept partner in its
    /// contour (the Blend's definition), so it is no candidate in d2d either. Neighbours at 5.9 F of a refused fit drop out
    /// in both; those at 6.1 F stay in both. On one, two, three and four planes.
    #[test]
    fn candidates_of_a_d3_list_are_those_of_the_list_with_the_refused_fits_put_back() {
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let bright = 500.0 * NOISE * area.sqrt();
        let ok = StarFlags::default();
        let (mut d2d, mut d3): (Vec<StarFit>, Vec<StarFit>) = (Vec::new(), Vec::new());
        let mut refused: Vec<(f64, f64)> = Vec::new();
        for i in 0..4 {
            let (x, y) = (100.0 + 400.0 * i as f64, 100.0);
            let r = fit_at(x, y, bright, area, f, ok);
            d2d.push(r);
            refused.push((x, y));
            for kept in [
                fit_at(x + 4.0 * f, y, bright, area, f, ok), // its partner: in the contour
                fit_at(x - 5.9 * f, y, bright, area, f, ok), // just inside the isolation radius
                fit_at(x, y + 6.1 * f, bright, area, f, ok), // just outside
            ] {
                d2d.push(kept);
                d3.push(kept);
            }
        }
        // Three stars nothing refused is near: candidates in both.
        for j in 0..3 {
            let g = fit_at(100.0 + 300.0 * j as f64, 800.0, bright, area, f, ok);
            d2d.push(g);
            d3.push(g);
        }
        crate::star_measure::fit::mark_blend_neighbours(
            &mut d3,
            &refused,
            CORRECTION_ISOLATION_PER_FIELD_FWHM * f,
        );
        assert_eq!(
            d3.iter().filter(|g| g.flags.blend_neighbour()).count(),
            8,
            "the partner and the 5.9 F star of each"
        );
        let map = identity();
        for planes in [1usize, 2, 3, 4] {
            let run = |fits: &[StarFit]| {
                candidates(&MemberPlane {
                    planes,
                    ..plane(fits, &map, f)
                })
            };
            let (want, got) = (run(&d2d), run(&d3));
            assert_eq!(
                want.len(),
                4 + 3,
                "{planes} planes: the 6.1 F stars and the free three"
            );
            assert_eq!(got, want, "{planes} planes");
        }
    }

    #[test]
    fn common_weights_are_scale_invariant() {
        let world = sky(150, 21);
        let t = [1.0, 0.9, 0.8, 0.95, 0.7, 1.0, 0.85, 0.9, 1.0, 0.75];
        let build = |scale: f64| -> Vec<(Vec<StarFit>, f64, f64)> {
            t.iter()
                .enumerate()
                .map(|(k, &t)| {
                    let f = 2.0 + 0.3 * k as f64;
                    let mut fits = fits_of(
                        &world,
                        &Shot {
                            clip: 20.0,
                            ..Shot::new(t, f)
                        },
                    );
                    fits.iter_mut().for_each(|x| {
                        x.signal *= scale;
                        x.ap_sum *= scale;
                    });
                    (fits, f, 1.0)
                })
                .collect()
        };
        let run_at = |scale: f64| {
            let members = build(scale);
            let map = identity();
            let planes: Vec<MemberPlane<'_>> = members
                .iter()
                .map(|(fits, f, c)| MemberPlane {
                    noise: NOISE * scale,
                    c_stand_in: *c,
                    ..plane(fits, &map, *f)
                })
                .collect();
            let cands = candidates(&planes[0]);
            let usable: Vec<_> = planes.iter().map(|p| match_member(&cands, p)).collect();
            common_flux(&cands, &usable, 0, OSC_PLANES)
        };
        let (a, b) = (run_at(1.0), run_at(65535.0));
        let (pa, fa) = measured(&a);
        let (pb, fb) = measured(&b);
        assert_eq!(pa, pb, "the same stars");
        assert!(pa.len() >= 50);
        for k in 0..t.len() {
            let (ra, rb) = (fa[k].tflux / fa[0].tflux, fb[k].tflux / fb[0].tflux);
            let (qa, qb) = (
                fa[k].tmean_flux / fa[0].tmean_flux,
                fb[k].tmean_flux / fb[0].tmean_flux,
            );
            assert!(rel(rb, ra) < 1e-12, "member {k}: r_f {ra} vs {rb}");
            assert!(rel(qb, qa) < 1e-12, "member {k}: q_f {qa} vs {qb}");
            assert_eq!(fa[k].paired, fb[k].paired);
        }
    }

    #[test]
    fn the_presence_rule_counts_the_set_not_the_candidates() {
        // 25 candidates; stars 0..6 are clipped in two of the ten members (8 of 10 present: out), so the SET is 19 while
        // the candidates are 25. A check on the candidates would let this through.
        let world = sky(25, 22);
        let build = |clipped_stars: usize| {
            let mut members: Vec<_> = (0..10)
                .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
                .collect();
            for j in 0..clipped_stars {
                members[2].0[j].flags = StarFlags::SATURATED;
                members[7].0[j].flags = StarFlags::SATURATED;
            }
            members
        };
        let (cands, _usable, out) = run(&build(6), 0);
        assert_eq!(cands.len(), 25);
        assert_eq!(out, CommonOutcome::TooFewStars { stars: 19 });
        // Five clipped in the two members: the set is 20, the boundary, and measured.
        let (cands, _usable, out) = run(&build(5), 0);
        assert_eq!(cands.len(), 25);
        assert_eq!(measured(&out).0.len(), 20);
    }

    #[test]
    fn a_clipped_second_fit_still_makes_a_match_ambiguous() {
        // Uniqueness reads every member fit with a position, whatever its flags: a clipped neighbour inside the radius
        // contaminates the candidate's wings, so the candidate is unmatched in that member.
        let world = sky(40, 23);
        let anchor = fits_of(&world, &Shot::new(1.0, 3.0));
        let mut member = fits_of(&world, &Shot::new(1.0, 3.0));
        let f = member[11];
        member.push(StarFit {
            x: f.x + 0.8,
            flags: StarFlags::SATURATED,
            ..f
        });
        let map = identity();
        let cands = candidates(&plane(&anchor, &map, 3.0));
        assert_eq!(cands.len(), 40);
        let got = match_member(&cands, &plane(&member, &map, 3.0));
        assert!(
            got[11].is_none(),
            "a saturated second fit 0.8 px away: unmatched"
        );
        assert_eq!(got.iter().filter(|g| g.is_some()).count(), 39);
    }

    #[test]
    fn the_match_radius_reads_the_anchors_fwhm_too() {
        // Anchor F 6, member F 3: the radius is max(1.5, 0.5 x max(6, 3)) = 3 px, so a fit 2.4 px off pairs. The radius
        // reads the WIDER of the two fields, whichever side it is on.
        let world = sky(40, 24);
        let anchor = fits_of(&world, &Shot::new(1.0, 6.0));
        let map = identity();
        let cands = candidates(&plane(&anchor, &map, 6.0));
        assert_eq!(cands.len(), 40);
        assert!(cands.iter().all(|c| c.anchor_fwhm_px == 6.0));
        let mut member = fits_of(&world, &Shot::new(1.0, 3.0));
        member[3].x += 2.4;
        let got = match_member(&cands, &plane(&member, &map, 3.0));
        assert!(got[3].is_some(), "2.4 px off, anchor F 6: paired");
        // The same fit against an anchor of F 3 and a member of F 3 is outside max(1.5, 1.5).
        let narrow = candidates(&plane(&fits_of(&world, &Shot::new(1.0, 3.0)), &map, 3.0));
        assert!(match_member(&narrow, &plane(&member, &map, 3.0))[3].is_none());
    }

    /// Every fit's aperture sum `S` scaled by `k`: the model flux (`signal`), `area` and `c` are untouched.
    fn scale_s(fits: &mut [StarFit], k: f64) {
        fits.iter_mut().for_each(|f| f.ap_sum *= k);
    }

    #[test]
    fn a_member_at_half_transparency_weighs_by_its_aperture_sums() {
        // Plan D D2b (PD-26): the paired ratios read S. Member 3's every S is halved while its model flux, its area and its
        // stand-in c are the others': the weight halves, the mean-flux twin too (same area), and nobody else moves.
        let world = sky(120, 31);
        let mut members: Vec<_> = (0..6)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        scale_s(&mut members[3].0, 0.5);
        let (_c, _u, out) = run(&members, 0);
        let (positions, per) = measured(&out);
        assert_eq!(positions.len(), 120);
        assert!(
            rel(per[3].tflux / per[0].tflux, 0.5) < 1e-12,
            "{}",
            per[3].tflux / per[0].tflux
        );
        assert!(rel(per[3].tmean_flux / per[0].tmean_flux, 0.5) < 1e-12);
        for k in [1, 2, 4, 5] {
            assert_eq!(per[k].tflux.to_bits(), per[0].tflux.to_bits(), "member {k}");
            assert_eq!(
                per[k].tmean_flux.to_bits(),
                per[0].tmean_flux.to_bits(),
                "member {k}"
            );
        }
    }

    #[test]
    fn a_members_own_c_does_not_move_its_weight() {
        // The weight no longer reads c: member 2's model flux doubles (its c alone doubled: S is the pixels' sum, not the
        // model's), and member 4 is a floored plane whose stand-in c is 2. Both keep the anchor's flux.
        let world = sky(120, 32);
        let mut members: Vec<_> = (0..6)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        members[2].0.iter_mut().for_each(|f| f.signal *= 2.0);
        members[4].2 = 2.0;
        let (_c, _u, out) = run(&members, 0);
        let (_p, per) = measured(&out);
        for k in [1, 2, 3, 4, 5] {
            assert!(
                rel(per[k].tflux / per[0].tflux, 1.0) < 1e-12,
                "member {k}: {}",
                per[k].tflux / per[0].tflux
            );
            assert!(
                rel(per[k].tmean_flux / per[0].tmean_flux, 1.0) < 1e-12,
                "member {k}"
            );
        }
    }

    #[test]
    fn the_anchors_sums_stay_the_models_flux_times_c() {
        // S is 0.8 of the model flux here; the anchor's whole-set sums, the scale `psf_snr` is read on against the master's
        // (D1b), stay the candidates' model flux times the stand-in c. Member 2 at half S reads half of THAT.
        let world = sky(100, 34);
        let mut members: Vec<_> = (0..4)
            .map(|k| {
                (
                    fits_of(&world, &Shot::new(1.0, 3.0)),
                    3.0,
                    if k == 0 { 1.25 } else { 1.0 },
                )
            })
            .collect();
        for m in &mut members {
            scale_s(&mut m.0, 0.8);
        }
        scale_s(&mut members[2].0, 0.5);
        let (cands, _u, out) = run(&members, 0);
        let (_p, per) = measured(&out);
        let sum_signal: f64 = cands.iter().map(|c| c.signal).sum();
        let sum_mean: f64 = cands.iter().map(|c| c.mean).sum();
        assert_eq!(per[0].tflux.to_bits(), sum_signal.to_bits());
        assert_eq!(per[0].tmean_flux.to_bits(), sum_mean.to_bits());
        assert!(rel(per[2].tflux, 0.5 * sum_signal) < 1e-12);
        assert!(rel(per[2].tmean_flux, 0.5 * sum_mean) < 1e-12);
        assert!(rel(per[1].tflux, sum_signal) < 1e-12);
    }

    #[test]
    fn a_fit_without_an_aperture_sum_leaves_only_its_own_pair_and_a_negative_one_stays() {
        // Member 4's fit of star 7 has no ring (S NaN): unusable for that member, it leaves its own pair, both sides of that
        // member's ratio (identical exposures: the ratio is exactly 1 only when the anchor's star 7 leaves member 4's
        // denominator too). Member 6's fit of star 9 has S = -0.1 (PD-30: a sum at or under the sky is noise and is summed,
        // not dropped): it stays paired and enters the sums as it is. 9 of 10 present: both stars stay in the set.
        let world = sky(100, 33);
        let mut members: Vec<_> = (0..10)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        members[4].0[7].ap_sum = f64::NAN;
        members[6].0[9].ap_sum = -0.1 * ADU;
        let (_cands, usable, out) = run(&members, 0);
        // D1b reads the model flux: the fits are still there for it, with no aperture sum on them.
        let lone = usable[4][7].expect("the model flux is usable");
        assert!(lone.signal > 0.0 && lone.ap_sum.is_nan() && lone.ap_mean.is_nan());
        let kept = usable[6][9].expect("usable");
        assert!(rel(kept.ap_sum, -0.1) < 1e-12, "{}", kept.ap_sum);
        let (positions, per) = measured(&out);
        assert_eq!(positions.len(), 100);
        assert_eq!(per[4].paired, 99);
        assert_eq!(per[6].paired, 100, "S = -0.1 stays paired");
        for k in [0, 1, 2, 3, 5, 7, 8, 9] {
            assert_eq!(per[k].paired, 100, "member {k}");
        }
        assert!(
            rel(per[4].tflux / per[0].tflux, 1.0) < 1e-12,
            "member 4: {}",
            per[4].tflux / per[0].tflux
        );
        assert!(rel(per[4].tmean_flux / per[0].tmean_flux, 1.0) < 1e-12);
        // Member 6 reads the sum with -0.1 in star 9's place: (T - f9 - 0.1) / T.
        let total: f64 = world.iter().map(|s| s.flux).sum();
        let want = (total - world[9].flux - 0.1) / total;
        assert!(
            rel(per[6].tflux / per[0].tflux, want) < 1e-12,
            "{} vs {want}",
            per[6].tflux / per[0].tflux
        );
    }

    #[test]
    fn an_unusable_aperture_sum_counts_against_the_presence_rule() {
        // As `a_star_unusable_in_more_than_a_tenth_of_the_members_leaves_the_set`, with the aperture sum as the missing piece:
        // a NaN S is missing; an S of 0 or under (noise, PD-30) is present.
        let world = sky(300, 35);
        let mut members: Vec<_> = (0..10)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        members[2].0[5].ap_sum = f64::NAN;
        members[7].0[5].ap_sum = f64::NAN;
        members[4].0[6].ap_sum = f64::NAN;
        members[3].0[8].ap_sum = 0.0;
        members[8].0[8].ap_sum = -0.5 * ADU;
        members[1].0[8].ap_sum = -0.5 * ADU;
        let (cands, _usable, out) = run(&members, 0);
        assert_eq!(cands.len(), 300);
        let (positions, per) = measured(&out);
        let has = |j: usize| {
            positions
                .iter()
                .any(|&(x, y)| x == world[j].x && y == world[j].y)
        };
        assert!(!has(5), "star 5 has a usable S in 8 of 10 members: out");
        assert!(has(6), "star 6 has a usable S in 9 of 10 members: in");
        assert!(
            has(8),
            "star 8 has a finite S in all 10 members, three of them at or under zero: in"
        );
        assert_eq!(positions.len(), 299);
        assert_eq!(per[4].paired, 298);
        assert_eq!(per[2].paired, 299);
        assert_eq!(per[3].paired, 299);
        assert_eq!(per[8].paired, 299);
    }

    #[test]
    fn an_anchor_fit_needs_a_finite_aperture_sum_to_be_a_candidate() {
        // The ratio's denominator is the anchor's S: a fit with none cannot start a pair; one at or under the sky (PD-30:
        // noise, summed) can. The SNR screen is still the `signal`-based one.
        let world = sky(40, 36);
        let mut anchor = fits_of(&world, &Shot::new(1.0, 3.0));
        anchor[3].ap_sum = f64::NAN;
        anchor[8].ap_sum = -2.0 * ADU;
        let map = identity();
        let cands = candidates(&plane(&anchor, &map, 3.0));
        assert_eq!(cands.len(), 39);
        assert!(cands.iter().all(|c| c.x != world[3].x));
        let c8 = cands
            .iter()
            .find(|c| c.x == world[8].x)
            .expect("S = -2 is a candidate");
        assert!(rel(c8.ap_sum, -2.0) < 1e-12 && c8.ap_mean < 0.0);
        assert!(cands
            .iter()
            .all(|c| c.ap_sum.is_finite() && c.ap_mean.is_finite()));
        // The same fit as a member (D1b's reader): the model flux is usable, the aperture sum is not.
        let full = candidates(&plane(&fits_of(&world, &Shot::new(1.0, 3.0)), &map, 3.0));
        let got = match_member(&full, &plane(&anchor, &map, 3.0));
        assert!(got.iter().all(|g| g.is_some()));
        assert!(got[3].unwrap().ap_sum.is_nan());
        assert!(rel(got[8].unwrap().ap_sum, -2.0) < 1e-12);
    }

    #[test]
    fn a_rescued_extended_anchor_fit_is_a_candidate_and_a_plain_extended_one_is_not() {
        // Plan D D2c (PD-30): `flux_fit` reads `!SATURATED && (!EXTENDED || SIBLING_POINT)`. Of three extended anchor fits, the
        // one carrying SIBLING_POINT is a candidate; the plain extended one and the saturated one with it are not.
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let bright = 500.0 * NOISE * area.sqrt();
        let fit = |x: f64, flags: StarFlags| fit_at(x, 100.0, bright, area, f, flags);
        let fits = vec![
            fit(100.0, StarFlags::default()),
            fit(300.0, StarFlags::EXTENDED),
            fit(500.0, StarFlags::EXTENDED | StarFlags::SIBLING_POINT),
            fit(
                700.0,
                StarFlags::SATURATED | StarFlags::EXTENDED | StarFlags::SIBLING_POINT,
            ),
        ];
        let map = identity();
        let xs: Vec<f64> = candidates(&plane(&fits, &map, f))
            .iter()
            .map(|c| c.x)
            .collect();
        assert_eq!(xs, vec![100.0, 500.0]);
        // The member side reads the same predicate: the rescued fit pairs, the plain extended one does not.
        let member = |flags: StarFlags| {
            let anchor = [fit(500.0, StarFlags::default())];
            let cand = candidates(&plane(&anchor, &map, f));
            let theirs = [fit(500.0, flags)];
            match_member(&cand, &plane(&theirs, &map, f))[0]
        };
        assert!(member(StarFlags::default()).is_some());
        assert!(
            member(StarFlags::EXTENDED).is_none(),
            "a plain extended fit does not pair"
        );
        assert!(member(StarFlags::EXTENDED | StarFlags::SIBLING_POINT).is_some());
        assert!(
            member(StarFlags::SATURATED | StarFlags::SIBLING_POINT | StarFlags::EXTENDED).is_none()
        );
        // And the statistic is not widened.
        assert!(!(StarFlags::EXTENDED | StarFlags::SIBLING_POINT).in_statistic());
    }

    #[test]
    fn a_rescued_star_joins_the_set_where_the_members_flag_it_too() {
        // Star 0 is extended with SIBLING_POINT in every member (a knot flag on a star the other planes see as a point): it
        // is a candidate, present everywhere, and in the set. Star 1 is plain extended in the anchor only: not a candidate.
        let world = sky(60, 41);
        let mut members: Vec<_> = (0..4)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        for m in &mut members {
            m.0[0].flags = StarFlags::EXTENDED | StarFlags::SIBLING_POINT;
        }
        members[0].0[1].flags = StarFlags::EXTENDED;
        let (cands, _u, out) = run(&members, 0);
        assert_eq!(cands.len(), 59);
        let (positions, _per) = measured(&out);
        assert_eq!(positions.len(), 59);
        let has = |j: usize| {
            positions
                .iter()
                .any(|&(x, y)| x == world[j].x && y == world[j].y)
        };
        assert!(has(0) && !has(1));
    }

    #[test]
    fn a_set_whose_anchor_sum_is_not_positive_has_no_common_flux() {
        // PD-30: the anchor's aperture sums over the set add to zero or less (every S is an S at or under the sky): the
        // members' ratios have nothing to scale, so the plane falls back (PD-6) with the set's size named.
        let world = sky(60, 42);
        let mut members: Vec<_> = (0..4)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        for m in &mut members {
            m.0.iter_mut().for_each(|f| f.ap_sum = -f.ap_sum);
        }
        let (cands, _u, out) = run(&members, 0);
        assert_eq!(cands.len(), 60, "a negative S is a candidate");
        assert_eq!(out, CommonOutcome::NoAnchorFlux { stars: 60 });
    }

    #[test]
    fn a_set_whose_anchor_mean_sum_is_not_positive_has_no_common_flux_either() {
        // Plan D D2c r1 (review m3): the ratios scale the anchor's sums S AND S / area. A set whose sums S add to a positive
        // number but whose S / area add to zero or less (a big positive S on a wide aperture against a handful of negative
        // ones on narrow ones) would read every fully paired member as (0, 0) and cut all of them but the anchor: the plane
        // falls back instead, with the set's size named. The other way round, S <= 0 with S / area > 0, falls back too.
        let world = sky(60, 43);
        let anchor_with = |first: (f64, f64), rest: f64| {
            let mut members: Vec<_> = (0..4)
                .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
                .collect();
            for m in &mut members {
                let area = m.0[1].area;
                for (i, f) in m.0.iter_mut().enumerate() {
                    if i == 0 {
                        // `first.0` is the area as a multiple of the others', `first.1` the sum S.
                        f.area = area * first.0;
                        f.ap_sum = first.1 * ADU;
                    } else {
                        f.ap_sum = rest * ADU;
                    }
                }
            }
            run(&members, 0)
        };
        // S: 100 - 59 = +41; S / area: 100 / (4 A) - 59 / A < 0.
        let (cands, _u, out) = anchor_with((4.0, 100.0), -1.0);
        assert_eq!(cands.len(), 60, "every star is a candidate");
        assert_eq!(out, CommonOutcome::NoAnchorFlux { stars: 60 });
        // S: 40 - 59 = -19; S / area: 40 / (A / 4) - 59 / A > 0.
        let (cands, _u, out) = anchor_with((0.25, 40.0), -1.0);
        assert_eq!(cands.len(), 60, "every star is a candidate");
        assert_eq!(out, CommonOutcome::NoAnchorFlux { stars: 60 });
        // And both sums positive is a measured set.
        let (cands, _u, out) = anchor_with((1.0, 1.0), 1.0);
        assert_eq!(cands.len(), 60);
        assert!(matches!(out, CommonOutcome::Measured { .. }), "{out:?}");
    }

    #[test]
    fn a_member_whose_paired_sums_are_not_positive_reads_no_flux() {
        // A member (here, member 2) whose every S is under the sky: the ratio is negative, and psf_signal_weight multiplies
        // two such fluxes into a positive number. Its common flux is zero, never negative.
        let world = sky(60, 43);
        let mut members: Vec<_> = (0..4)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        members[2].0.iter_mut().for_each(|f| f.ap_sum = -f.ap_sum);
        let (_c, _u, out) = run(&members, 0);
        let (_p, per) = measured(&out);
        assert_eq!((per[2].tflux, per[2].tmean_flux), (0.0, 0.0));
        assert!(per[1].tflux > 0.0);
    }

    #[test]
    fn the_aperture_sum_and_mean_are_native_units_without_c() {
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let signal = 500.0 * NOISE * area.sqrt();
        let mut fit = fit_at(100.0, 100.0, signal, area, f, StarFlags::default());
        fit.ap_sum = 0.8 * fit.signal;
        let fits = vec![fit];
        let map = identity();
        let with_c = |c: f64| MemberPlane {
            c_stand_in: c,
            ..plane(&fits, &map, f)
        };
        let cands = candidates(&with_c(1.2));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].ap_sum.to_bits(), (fits[0].ap_sum / ADU).to_bits());
        assert_eq!(
            cands[0].ap_mean.to_bits(),
            (fits[0].ap_sum / ADU / area).to_bits()
        );
        // c is on the model flux alone.
        assert!(rel(cands[0].signal, signal * 1.2) < 1e-12);
        let got = match_member(&cands, &with_c(1.2))[0].expect("paired");
        assert_eq!(got.ap_sum.to_bits(), cands[0].ap_sum.to_bits());
        assert_eq!(got.ap_mean.to_bits(), cands[0].ap_mean.to_bits());
        assert!(rel(got.signal, signal * 1.2) < 1e-12);
    }

    #[test]
    fn a_malformed_match_table_is_its_own_outcome() {
        let world = sky(60, 25);
        let members: Vec<_> = (0..4)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        let (cands, usable, _) = run(&members, 0);
        // A member row of the wrong length, and an anchor index past the table.
        let mut short = usable.clone();
        short[2].pop();
        let cases = [(short, 0usize), (usable.clone(), usable.len())];
        for (table, anchor) in cases {
            let got = std::panic::catch_unwind(|| common_flux(&cands, &table, anchor, OSC_PLANES));
            if cfg!(debug_assertions) {
                // A debug build stops at the wiring bug.
                assert!(got.is_err(), "debug_assert on a malformed table");
            } else {
                assert_eq!(
                    got.expect("no panic in release"),
                    CommonOutcome::MalformedTable
                );
            }
        }
    }

    /// `common_set.rs` as it read at 971606c8 (the d2 head), the flux term the weight read before the aperture sum: the model
    /// flux times `c`, `signal` and `signal / area`, with `in_statistic()` as the flux fit's flag rule and no condition on `S`.
    /// A private copy, only so the tests below can say that a group that is not three planes reads exactly this (PD-32); it
    /// is not a second implementation to keep, and nothing else reads it.
    mod d2 {
        use super::*;

        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Candidate {
            pub x: f64,
            pub y: f64,
            pub signal: f64,
            pub mean: f64,
            pub anchor_fwhm_px: f64,
        }

        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Usable {
            pub signal: f64,
            pub mean: f64,
        }

        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Flux {
            pub tflux: f64,
            pub tmean_flux: f64,
            pub paired: usize,
        }

        #[derive(Debug, Clone, PartialEq)]
        pub enum Outcome {
            Measured {
                positions: Vec<(f64, f64)>,
                per_member: Vec<Flux>,
            },
            TooFewStars {
                stars: usize,
            },
        }

        fn flux_fit(f: &StarFit) -> bool {
            f.flags.in_statistic()
                && f.signal.is_finite()
                && f.signal > 0.0
                && f.area.is_finite()
                && f.area > 0.0
        }

        pub fn candidates(anchor: &MemberPlane<'_>) -> Vec<Candidate> {
            let f = anchor.fwhm_px;
            if !(f.is_finite() && f > 0.0) || !(anchor.noise.is_finite() && anchor.noise > 0.0) {
                return Vec::new();
            }
            let c = c_factor(anchor.c_stand_in);
            let mut at: Vec<usize> = Vec::with_capacity(anchor.fits.len());
            let mut pts: Vec<(f64, f64)> = Vec::with_capacity(anchor.fits.len());
            for (i, fit) in anchor.fits.iter().enumerate() {
                if fit.x.is_finite() && fit.y.is_finite() {
                    at.push(i);
                    pts.push((fit.x, fit.y));
                }
            }
            let tree = KdTree2::build(&pts);
            let isolation = CORRECTION_ISOLATION_PER_FIELD_FWHM * f;
            let adu = ADU_SCALE as f64;
            let mut out = Vec::new();
            for (k, &i) in at.iter().enumerate() {
                let fit = &anchor.fits[i];
                if !flux_fit(fit) {
                    continue;
                }
                let signal = fit.signal / adu;
                if signal / (anchor.noise * fit.area.sqrt()) < CORRECTION_MIN_SNR {
                    continue;
                }
                if tree.within(pts[k].0, pts[k].1, isolation).len() != 1 {
                    continue;
                }
                let (x, y) = anchor.map.forward_exact(fit.x, fit.y);
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                out.push(Candidate {
                    x,
                    y,
                    signal: signal * c,
                    mean: signal / fit.area * c,
                    anchor_fwhm_px: f,
                });
            }
            out
        }

        pub fn match_member(cands: &[Candidate], member: &MemberPlane<'_>) -> Vec<Option<Usable>> {
            let mut out = vec![None; cands.len()];
            if cands.is_empty() {
                return out;
            }
            let c = c_factor(member.c_stand_in);
            let adu = ADU_SCALE as f64;
            let mut at: Vec<usize> = Vec::with_capacity(member.fits.len());
            let mut pts: Vec<(f64, f64)> = Vec::with_capacity(member.fits.len());
            for (i, fit) in member.fits.iter().enumerate() {
                if !(fit.x.is_finite() && fit.y.is_finite()) {
                    continue;
                }
                let (gx, gy) = member.map.forward_exact(fit.x, fit.y);
                if gx.is_finite() && gy.is_finite() {
                    at.push(i);
                    pts.push((gx, gy));
                }
            }
            let tree = KdTree2::build(&pts);
            for (slot, cand) in out.iter_mut().zip(cands) {
                let radius = POINT_SOURCE_MATCH_PX.max(
                    POINT_SOURCE_MATCH_PER_FIELD_FWHM * cand.anchor_fwhm_px.max(member.fwhm_px),
                );
                let hits = tree.within(cand.x, cand.y, radius);
                if hits.len() != 1 {
                    continue;
                }
                let fit = &member.fits[at[hits[0].0]];
                if flux_fit(fit) {
                    let signal = fit.signal / adu;
                    *slot = Some(Usable {
                        signal: signal * c,
                        mean: signal / fit.area * c,
                    });
                }
            }
            out
        }

        pub fn common_flux(
            cands: &[Candidate],
            usable: &[Vec<Option<Usable>>],
            anchor: usize,
        ) -> Outcome {
            let members = usable.len() as f64;
            let set: Vec<usize> = (0..cands.len())
                .filter(|&j| {
                    let present = usable.iter().filter(|u| u[j].is_some()).count();
                    present as f64 / members >= COMMON_MIN_PRESENCE
                })
                .collect();
            if set.len() < COMMON_MIN_STARS {
                return Outcome::TooFewStars { stars: set.len() };
            }
            let anchor_signal: f64 = set.iter().map(|&j| cands[j].signal).sum();
            let anchor_mean: f64 = set.iter().map(|&j| cands[j].mean).sum();
            let per_member = usable
                .iter()
                .enumerate()
                .map(|(k, u)| {
                    if k == anchor {
                        return Flux {
                            tflux: anchor_signal,
                            tmean_flux: anchor_mean,
                            paired: set.len(),
                        };
                    }
                    let (mut sig, mut sig_a, mut mean, mut mean_a, mut paired) =
                        (0.0, 0.0, 0.0, 0.0, 0usize);
                    for &j in &set {
                        if let Some(f) = u[j] {
                            sig += f.signal;
                            sig_a += cands[j].signal;
                            mean += f.mean;
                            mean_a += cands[j].mean;
                            paired += 1;
                        }
                    }
                    if paired == 0 || !(sig_a > 0.0 && mean_a > 0.0) {
                        return Flux {
                            tflux: 0.0,
                            tmean_flux: 0.0,
                            paired: 0,
                        };
                    }
                    Flux {
                        tflux: sig / sig_a * anchor_signal,
                        tmean_flux: mean / mean_a * anchor_mean,
                        paired,
                    }
                })
                .collect();
            Outcome::Measured {
                positions: set.iter().map(|&j| (cands[j].x, cands[j].y)).collect(),
                per_member,
            }
        }
    }

    /// A group whose aperture sums are NOT the model flux, with every case where the two flux terms' rules part: each fit's
    /// `S` is the model's flux times a factor in 0.6..1.4, one anchor fit and one member fit have no `S`, one member fit a
    /// negative one, an extended fit carries `SIBLING_POINT` (in the anchor and in one member), a member fit has no signal,
    /// stars clip by seeing, and members stand on their own `c` (two of them at a stand-in).
    fn s_unlike_the_model_group(seed: u64) -> Vec<(Vec<StarFit>, f64, f64)> {
        let world = sky(150, seed);
        let t = [1.0, 0.9, 0.8, 0.95, 0.7, 1.0, 0.85, 0.9];
        let mut members: Vec<_> = t
            .iter()
            .enumerate()
            .map(|(k, &t)| {
                let f = 2.0 + 0.3 * k as f64;
                let shot = Shot {
                    clip: 20.0,
                    jitter: 0.3,
                    seed: seed + k as u64,
                    ..Shot::new(t, f)
                };
                let mut rng = SplitMix(seed * 977 + k as u64);
                let mut fits = fits_of(&world, &shot);
                fits.iter_mut()
                    .for_each(|x| x.ap_sum *= 0.6 + 0.8 * rng.unit());
                let c = match k {
                    3 => 1.15,
                    5 => 0.9,
                    _ => 1.0,
                };
                (fits, f, c)
            })
            .collect();
        members[0].0[11].ap_sum = f64::NAN;
        members[2].0[7].ap_sum = f64::NAN;
        members[4].0[9].ap_sum = -0.1 * ADU;
        members[0].0[13].flags = StarFlags::EXTENDED | StarFlags::SIBLING_POINT;
        members[1].0[13].flags = StarFlags::EXTENDED | StarFlags::SIBLING_POINT;
        members[6].0[15].signal = f64::NAN;
        members
    }

    /// The d2 pipeline over the same members, identity maps.
    fn run_d2(
        members: &[(Vec<StarFit>, f64, f64)],
        anchor: usize,
    ) -> (
        Vec<d2::Candidate>,
        Vec<Vec<Option<d2::Usable>>>,
        d2::Outcome,
    ) {
        let map = identity();
        let planes: Vec<MemberPlane<'_>> = members
            .iter()
            .map(|(fits, fwhm, c)| MemberPlane {
                c_stand_in: *c,
                ..plane(fits, &map, *fwhm)
            })
            .collect();
        let cands = d2::candidates(&planes[anchor]);
        let usable: Vec<_> = planes.iter().map(|p| d2::match_member(&cands, p)).collect();
        let out = d2::common_flux(&cands, &usable, anchor);
        (cands, usable, out)
    }

    #[test]
    fn the_weight_reads_the_aperture_sum_on_three_planes_only() {
        assert!(weighs_by_aperture_sum(3));
        for planes in [0, 1, 2, 4, 5] {
            assert!(!weighs_by_aperture_sum(planes), "{planes} planes");
        }
    }

    #[test]
    fn a_group_that_is_not_three_planes_reads_what_d2_read_bit_for_bit() {
        // PD-32: a mono (and a two-plane) group's candidates, matches, set and weights are the model flux times c, as at
        // 971606c8, whatever its fits' S say.
        for seed in [51, 52, 53] {
            let members = s_unlike_the_model_group(seed);
            let (old_cands, old_usable, old_out) = run_d2(&members, 0);
            for planes in [1, 2, 4] {
                let (cands, usable, out) = run_planes(&members, 0, planes);
                assert_eq!(cands.len(), old_cands.len(), "seed {seed}, {planes} planes");
                assert!(cands.len() > 100);
                for (c, o) in cands.iter().zip(&old_cands) {
                    let bits = |c: &Candidate| {
                        [c.x, c.y, c.signal, c.mean, c.anchor_fwhm_px].map(f64::to_bits)
                    };
                    let old = [o.x, o.y, o.signal, o.mean, o.anchor_fwhm_px].map(f64::to_bits);
                    assert_eq!(bits(c), old, "seed {seed}, {planes} planes: a candidate");
                }
                for (k, (row, old_row)) in usable.iter().zip(&old_usable).enumerate() {
                    assert_eq!(row.len(), old_row.len());
                    for (j, (u, o)) in row.iter().zip(old_row).enumerate() {
                        let pair =
                            |a: Option<(f64, f64)>| a.map(|(s, m)| (s.to_bits(), m.to_bits()));
                        assert_eq!(
                            pair(u.map(|u| (u.signal, u.mean))),
                            pair(o.map(|o| (o.signal, o.mean))),
                            "seed {seed}, {planes} planes: member {k}, candidate {j}"
                        );
                    }
                }
                let (positions, per) = measured(&out);
                let d2::Outcome::Measured {
                    positions: old_positions,
                    per_member: old_per,
                } = &old_out
                else {
                    panic!("d2 measured a set: {old_out:?}");
                };
                assert_eq!(positions.len(), old_positions.len());
                assert!(positions.len() >= 100);
                for (p, o) in positions.iter().zip(old_positions) {
                    assert_eq!(
                        (p.0.to_bits(), p.1.to_bits()),
                        (o.0.to_bits(), o.1.to_bits())
                    );
                }
                assert_eq!(per.len(), old_per.len());
                for (k, (p, o)) in per.iter().zip(old_per).enumerate() {
                    assert_eq!(
                        (p.tflux.to_bits(), p.tmean_flux.to_bits(), p.paired),
                        (o.tflux.to_bits(), o.tmean_flux.to_bits(), o.paired),
                        "seed {seed}, {planes} planes: member {k}"
                    );
                }
            }
            // The same group on three planes is NOT that: the fixture's S is not the model flux, so a member's weight moves
            // (the test above would hold for a pipeline that never read S at all).
            let (_c, _u, three) = run_planes(&members, 0, 3);
            let (_p, per3) = measured(&three);
            let d2::Outcome::Measured { per_member, .. } = &old_out else {
                unreachable!()
            };
            assert!(
                per3.iter()
                    .zip(per_member)
                    .skip(1)
                    .any(|(a, b)| a.tflux.to_bits() != b.tflux.to_bits()),
                "seed {seed}: three planes read S"
            );
        }
    }

    #[test]
    fn a_mono_group_keeps_the_fits_the_aperture_sum_would_drop() {
        // d2's usable rule is a finite positive `signal` and `area`: a fit with no S (no ring sky) is a candidate and a
        // match on one plane, not on three (PD-32; the three-plane side is `an_anchor_fit_needs_a_finite_aperture_sum...`).
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let bright = 500.0 * NOISE * area.sqrt();
        let mut fit = fit_at(100.0, 100.0, bright, area, f, StarFlags::default());
        fit.ap_sum = f64::NAN;
        let fits = vec![fit];
        let map = identity();
        for planes in [1, 2] {
            let on = MemberPlane {
                planes,
                ..plane(&fits, &map, f)
            };
            let cands = candidates(&on);
            assert_eq!(cands.len(), 1, "{planes} planes");
            let got = match_member(&cands, &on)[0].expect("paired");
            assert!(rel(got.signal, bright) < 1e-12, "{planes} planes");
        }
        assert!(candidates(&plane(&fits, &map, f)).is_empty());
    }

    #[test]
    fn a_mono_group_whose_every_s_is_under_the_sky_still_has_its_model_flux_set() {
        // A three-plane group whose anchor sums S to zero or less has no flux for the ratios to scale (NoAnchorFlux); a mono
        // one reads the model flux, which is positive by construction, and measures a set (PD-32).
        let world = sky(60, 54);
        let mut members: Vec<_> = (0..4)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        for m in &mut members {
            m.0.iter_mut().for_each(|f| f.ap_sum = -1.0);
        }
        let (_c, _u, three) = run_planes(&members, 0, 3);
        assert!(
            matches!(three, CommonOutcome::NoAnchorFlux { stars: 60 }),
            "{three:?}"
        );
        let (_c, _u, one) = run_planes(&members, 0, 1);
        let (positions, per) = measured(&one);
        assert_eq!(positions.len(), 60);
        for p in per {
            assert_eq!(p.tflux.to_bits(), per[0].tflux.to_bits());
            assert!(p.tflux > 0.0 && p.paired == 60);
        }
    }

    #[test]
    fn a_rescued_extended_fit_is_a_flux_fit_on_three_planes_only() {
        // `SIBLING_POINT` is set only where another plane's seed can rescue a fit (three planes by construction); a group of
        // any other count keeps `in_statistic()` as d2 did, the flag or not, on both the anchor's and the member's side.
        let f = 3.0;
        let area = AREA_PER_FWHM2 * f * f;
        let bright = 500.0 * NOISE * area.sqrt();
        let rescued = StarFlags::EXTENDED | StarFlags::SIBLING_POINT;
        let anchor = [fit_at(500.0, 100.0, bright, area, f, rescued)];
        let map = identity();
        let on = |planes: usize| MemberPlane {
            planes,
            ..plane(&anchor, &map, f)
        };
        assert_eq!(candidates(&on(3)).len(), 1);
        for planes in [1, 2] {
            assert!(candidates(&on(planes)).is_empty(), "{planes} planes");
        }
        let plain = [fit_at(500.0, 100.0, bright, area, f, StarFlags::default())];
        let cand = candidates(&plane(&plain, &map, f));
        let theirs = [fit_at(500.0, 100.0, bright, area, f, rescued)];
        let member = |planes: usize| {
            match_member(
                &cand,
                &MemberPlane {
                    planes,
                    ..plane(&theirs, &map, f)
                },
            )[0]
        };
        assert!(member(3).is_some());
        assert!(member(1).is_none() && member(2).is_none());
    }

    #[test]
    fn a_malformed_table_is_malformed_on_any_plane_count() {
        // The wiring check runs before the flux term is chosen.
        let world = sky(60, 26);
        let members: Vec<_> = (0..3)
            .map(|_| (fits_of(&world, &Shot::new(1.0, 3.0)), 3.0, 1.0))
            .collect();
        for planes in [1, 3] {
            let (cands, usable, _) = run_planes(&members, 0, planes);
            let mut short = usable.clone();
            short[1].pop();
            let got = std::panic::catch_unwind(|| common_flux(&cands, &short, 0, planes));
            if cfg!(debug_assertions) {
                assert!(
                    got.is_err(),
                    "{planes} planes: debug_assert on a malformed table"
                );
            } else {
                assert_eq!(
                    got.expect("no panic in release"),
                    CommonOutcome::MalformedTable
                );
            }
        }
    }
}
