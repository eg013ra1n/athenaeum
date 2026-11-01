//! The frame-common star set (Plan D, D1; ruling PD-3): the stars that are unclipped and measurable in (almost) every
//! included, aligned member of one (group, plane), and each member's flux over exactly those stars.
//!
//! Why: a frame's PSF-family weight reads its total starlight (`TFlux`, `TMeanFlux`). That total follows the seeing:
//! clipped stars carry most of it and are extrapolated from their wings, and stars stop clipping as the seeing
//! softens. The flux of the SAME unclipped stars in every frame follows the sky's transparency only, and no refusal of
//! a clipped or blended star can move it.
//!
//! Pure logic, no I/O and no weights. The caller (`run.rs::reweigh_groups`, D1w) reads one member's `.athf` fits at a
//! time and hands the module their [`MemberPlane`]; the flux the module reads is the one Measure owns (`StarFit::signal`,
//! the model's FWTM flux times the plane's `c`), never a second estimate of it.
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
}

/// One of the anchor's candidate stars: group coordinates, native flux units (the stand-in `c` already applied).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub x: f64,
    pub y: f64,
    pub signal: f64,
    pub mean: f64,
    /// The anchor plane's field FWHM, px: the match radius of every member reads it ([`match_member`]).
    pub anchor_fwhm_px: f64,
}

/// A candidate's fit in one member: native flux units, the member's stand-in `c` applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usable {
    pub signal: f64,
    pub mean: f64,
}

/// One member's flux over the common set, native units: `tflux` and `tmean_flux` are the paired ratio to the anchor
/// times the anchor's whole-set sums, `paired` is how many set stars the member has.
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

/// `c_stand_in` as a factor: a non-finite or non-positive value (never produced by `aperture_correction_group`) reads as 1.
fn c_factor(c: f64) -> f64 {
    if c.is_finite() && c > 0.0 {
        c
    } else {
        tracing::warn!(c_stand_in = c, "common set: unusable c stand-in read as 1");
        1.0
    }
}

/// A fit the set may read the flux of: `in_statistic()` (neither clipped nor extended), finite positive `signal` and
/// `area`.
fn flux_fit(f: &StarFit) -> bool {
    f.flags.in_statistic()
        && f.signal.is_finite()
        && f.signal > 0.0
        && f.area.is_finite()
        && f.area > 0.0
}

/// The anchor's candidate stars in group coordinates: `in_statistic()`, finite positive `signal` and `area`, no other
/// anchor fit (of any flag) within [`CORRECTION_ISOLATION_PER_FIELD_FWHM`] x the plane's `fwhm_px`, and the
/// background-limited aperture SNR `signal / (noise * sqrt(area))` at least [`CORRECTION_MIN_SNR`] (the `.athf` stores no
/// SNR). Candidates keep the anchor's fit order.
pub fn candidates(anchor: &MemberPlane<'_>) -> Vec<Candidate> {
    let f = anchor.fwhm_px;
    if !(f.is_finite() && f > 0.0) || !(anchor.noise.is_finite() && anchor.noise > 0.0) {
        return Vec::new();
    }
    let c = c_factor(anchor.c_stand_in);
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
        if !flux_fit(fit) {
            continue;
        }
        let signal = fit.signal / adu;
        if signal / (anchor.noise * fit.area.sqrt()) < CORRECTION_MIN_SNR {
            continue;
        }
        // The fit itself is always within the radius: isolated means it is the only one.
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

/// For each candidate, the member's fit if there is exactly one within the match radius in group coordinates (two
/// within the radius leave the candidate unmatched in this member) and it is `in_statistic()` with a finite positive
/// `signal` and `area`. The radius is `max(POINT_SOURCE_MATCH_PX, POINT_SOURCE_MATCH_PER_FIELD_FWHM x max(F_anchor,
/// F_member))`. Every member fit with a position counts for the uniqueness, clipped or extended ones too.
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

/// The common set and every member's flux over it. `usable[k]` is member k's [`match_member`] result (the anchor
/// included, in member order), `anchor` the anchor's index in that order.
///
/// A candidate joins the set when it is usable in at least [`COMMON_MIN_PRESENCE`] of the members; fewer than
/// [`COMMON_MIN_STARS`] such stars leave no common flux. Member f's flux is the paired ratio to the anchor over the
/// set stars f has (`r_f` for the flux, `q_f` for the mean flux) times the anchor's sums over the whole set, so a
/// frame that lost a third of its stars to cloud still weighs by its transparency, not by the stars it lost. A member
/// with no paired star reads 0. The anchor's own entry is the anchor's sums, exactly.
pub fn common_flux(
    cands: &[Candidate],
    usable: &[Vec<Option<Usable>>],
    anchor: usize,
) -> CommonOutcome {
    if anchor >= usable.len() || usable.iter().any(|u| u.len() != cands.len()) {
        tracing::error!(
            candidates = cands.len(),
            members = usable.len(),
            anchor,
            "common set: the match table does not fit the candidates or the anchor"
        );
        return CommonOutcome::TooFewStars { stars: 0 };
    }
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
        }
    }

    fn identity() -> PixelMap {
        PixelMap::linear(Linear::identity()).expect("identity map")
    }

    /// The whole pipeline over identity-mapped members: candidates of `anchor`, every member's match, the outcome.
    fn run(
        members: &[(Vec<StarFit>, f64, f64)],
        anchor: usize,
    ) -> (Vec<Candidate>, Vec<Vec<Option<Usable>>>, CommonOutcome) {
        let map = identity();
        let planes: Vec<MemberPlane<'_>> = members
            .iter()
            .map(|(fits, fwhm, c)| MemberPlane {
                c_stand_in: *c,
                ..plane(fits, &map, *fwhm)
            })
            .collect();
        let cands = candidates(&planes[anchor]);
        let usable: Vec<_> = planes.iter().map(|p| match_member(&cands, p)).collect();
        let out = common_flux(&cands, &usable, anchor);
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
    fn a_floored_plane_takes_its_group_c_in_the_common_flux() {
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
        assert!(rel(per[2].tflux / per[0].tflux, 1.3) < 1e-12);
        assert!(rel(per[2].tmean_flux / per[0].tmean_flux, 1.3) < 1e-12);
        assert!(rel(per[1].tflux / per[0].tflux, 1.0) < 1e-12);
        // The same plane as the anchor: its stars carry the stand-in, and the others read relative to it.
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
        assert!(rel(per[1].tflux / per[0].tflux, 1.0 / 1.3) < 1e-12);
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
            positions.is_empty() == false,
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
                    fits.iter_mut().for_each(|x| x.signal *= scale);
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
            common_flux(&cands, &usable, 0)
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
}
