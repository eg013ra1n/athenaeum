//! The two star populations (Plan A Task 13b; probe / lab only, the stacking Measure runs `Mode::All`, rulings R13b-2 and R13b-3). One plane's
//! detection feeds two consumers that need different stars, so one list is never cut for both:
//!
//! - **(a) the frame-statistic population** - the frame FWHM and eccentricity, the F-6 medians, the ring width `F` and
//!   the aperture correction `c`: [`STATISTIC_STARS`] good, isolated, unsaturated stars spread over the plane's 4 x 4
//!   grid and across brightness (never the N brightest). Chosen BEFORE the verdict pass, from the detection alone.
//! - **(b) the star list** Register and local normalization read (`.athf`): the first `list` seeds in detector order
//!   (brightest first by matched-filter flux).
//!
//! The pass fits `fitted = (a) U (b)` and nothing else; every other seed keeps its detection record. Pure functions
//! over seeds and the detection report: [`select`] is deterministic (no random draw anywhere).
//!
//! Eligible for (a): not flagged extended by the knot test, no OTHER detected seed within
//! [`ISOLATION_PER_FIELD_FWHM`] field widths (every seed counts, faint ones too, as the ring sky and `c` count them),
//! not flagged by the pre-fit saturation clauses ([`pre_saturated`]), and a filter SNR of at least
//! [`STATISTIC_SNR_FLOOR`] and below the eligible pool's 95th percentile ([`STATISTIC_TOP_QUANTILE`]). The band is cut
//! into [`BRIGHTNESS_STRATA`] equal-count brightness quartiles by filter SNR; with the 16 cells that makes 64 strata,
//! each with a quota of `ceil(N / 64)` handed on water-fill style when a stratum is short, and the picks of a stratum
//! are evenly spaced by rank.

use std::collections::HashMap;

use astroimage::analysis::fitting::PixelSample;
use serde::{Deserialize, Serialize};

use super::budget::cell_of;
use super::fit::Seed;
use super::photometry::{plateau_test, SaturationPlane};

/// N for the stacking Measure stage: the frame-statistic stars per plane (sd of the frame FWHM 0.15-0.24 %, of the
/// eccentricity 0.003-0.004 on four set 204 mono frames, below the frame-to-frame difference).
pub const STATISTIC_STARS: usize = 1000;

/// The list size (b) the stacking Measure stage would fit by default for the plane kind - a mono plane 2 x the default
/// `registration.max_stars` (2 000), a colour plane 2 000. Route A (owner decision 2026-10-17) has [`super::Mode::All`]
/// fit every seed and select (a) alone, so only tests and the probes read it.
pub fn default_list(colour: bool) -> usize {
    if colour {
        2000
    } else {
        4000
    }
}
/// A star joins (a) from this filter SNR (the matched filter's response over its propagated noise).
pub const STATISTIC_SNR_FLOOR: f64 = 20.0;
/// ... and below this quantile of the eligible pool's filter SNR (the margin above the pre-fit saturation clauses).
pub const STATISTIC_TOP_QUANTILE: f64 = 0.95;
/// Equal-count brightness classes per cell.
pub const BRIGHTNESS_STRATA: usize = 4;
/// A seed with another detected seed within this many field widths is not isolated.
pub const ISOLATION_PER_FIELD_FWHM: f64 = 1.5;

/// The plane's 4 x 4 cells.
const CELLS: usize = 16;
/// Cells x brightness classes.
pub const STRATA: usize = CELLS * BRIGHTNESS_STRATA;
/// The stratum of a fit that is not in (a).
pub const NO_STRATUM: u8 = u8::MAX;

/// Who is fitted and who is in the frame statistic (stored in the first-pass record, `PlanePrep`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Population {
    /// The seeds the pass fits, ascending seed index: (a) U (b).
    pub fitted: Vec<usize>,
    /// Population (a), ascending seed index.
    pub statistic: Vec<usize>,
    /// `stratum[k]` = `cell * 4 + brightness class` of `statistic[k]`, in `0..64`.
    pub stratum: Vec<u8>,
    /// The size of list (b): the first `list` seeds (`min(M, seeds)`).
    pub list: usize,
}

/// A fit call's view of a [`Population`], one entry per FIT-CALL seed (the position in `Population::fitted`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PopulationView<'a> {
    pub(crate) statistic: &'a [bool],
}

impl Population {
    /// `(in (a), stratum or NO_STRATUM)` for every fitted seed, in `fitted` order.
    pub(super) fn fit_flags(&self) -> (Vec<bool>, Vec<u8>) {
        let mut in_a = vec![false; self.fitted.len()];
        let mut stratum = vec![NO_STRATUM; self.fitted.len()];
        let mut j = 0;
        for (k, &seed) in self.fitted.iter().enumerate() {
            while j < self.statistic.len() && self.statistic[j] < seed {
                j += 1;
            }
            if j < self.statistic.len() && self.statistic[j] == seed {
                in_a[k] = true;
                stratum[k] = self.stratum[j];
            }
        }
        (in_a, stratum)
    }
}

/// What the population did on one plane, reported on the frame line (never compared or stored).
#[derive(Debug, Clone, PartialEq)]
pub struct PopulationReport {
    /// |(a)| selected.
    pub statistic: usize,
    /// |(b)|.
    pub list: usize,
    /// |(a) U (b)|: the seeds the pass fitted.
    pub fitted: usize,
    /// (a)'s stars accepted by the fit (the frame statistic's population before the saturation and knot flags).
    pub statistic_accepted: usize,
    /// The share of (a)'s accepted fits with FWHM above 1.3 field widths and eccentricity above 0.8 (merged pairs that
    /// the seed isolation could not see; re-opened above 2 %).
    pub merged_share: f64,
    /// The standard error of the median `c` is taken as, when `c` was measured on the 64 brightest fits of (a).
    pub aperture_se: Option<f64>,
}

/// Whether the seeds' pre-fit saturation clauses flag them (before any verdict exists): the band test of the 3 x 3
/// about the seed pixel, which is the superset of the verdict's plateau-clip and near-clip clauses (`near_clip`,
/// [`SaturationPlane::near_clip`]), or a passed plateau test on that 3 x 3. The amplitude clause needs a fit and stays
/// the fit's. `sat` carries no near-clip floor yet; the band is its lower bound.
///
/// R13b-12: the clip evidence available before any fit is a tie (`sat.max_count >= 2`, `derive_clip_floor`'s first
/// evidence) or a seed whose 3 x 3 passes the plateau test on the full-scale-anchored plane (`range = norm - bg`; a
/// clipped core, flat to the test's own tolerance). With evidence both clauses read the plane's own range as the
/// verdict does; without it both read the full scale `norm - bg`, and a clip-free plane's bright stars stay eligible.
pub(super) fn pre_saturated(
    plane: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seeds: &[Seed],
    sat: &SaturationPlane,
) -> Vec<bool> {
    let anchored = SaturationPlane {
        range: sat.norm - sat.bg,
        ..*sat
    };
    let mut stamp: Vec<PixelSample> = Vec::with_capacity(9);
    let evidence = sat.max_count >= 2
        || seeds.iter().any(|s| {
            gather_stamp(plane, mask, w, h, s, &mut stamp);
            plateau_test(&stamp, s.x, s.y, &anchored).is_some()
        });
    let sat = if evidence { sat } else { &anchored };
    seeds
        .iter()
        .map(|s| {
            if super::seeds::near_clip(plane, mask, w, h, s, Some(sat)) {
                return true;
            }
            gather_stamp(plane, mask, w, h, s, &mut stamp);
            plateau_test(&stamp, s.x, s.y, sat).is_some()
        })
        .collect()
}

/// How many seeds carry a full-scale plateau core (the pre-fit clip evidence's second clause), for the debug line.
pub(super) fn plateau_core_count(
    plane: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seeds: &[Seed],
    sat: &SaturationPlane,
) -> usize {
    let anchored = SaturationPlane {
        range: sat.norm - sat.bg,
        ..*sat
    };
    let mut stamp: Vec<PixelSample> = Vec::with_capacity(9);
    seeds
        .iter()
        .filter(|s| {
            gather_stamp(plane, mask, w, h, s, &mut stamp);
            plateau_test(&stamp, s.x, s.y, &anchored).is_some()
        })
        .count()
}

/// The finite unmasked pixels of the 3 x 3 about a seed's pixel.
fn gather_stamp(
    plane: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    s: &Seed,
    stamp: &mut Vec<PixelSample>,
) {
    stamp.clear();
    let (cx, cy) = (s.x.round() as i64, s.y.round() as i64);
    for yy in cy - 1..=cy + 1 {
        for xx in cx - 1..=cx + 1 {
            if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                continue;
            }
            let i = yy as usize * w + xx as usize;
            let v = plane[i];
            if v.is_finite() && mask.map_or(true, |m| m[i]) {
                stamp.push(PixelSample {
                    x: xx as f64,
                    y: yy as f64,
                    value: v as f64,
                });
            }
        }
    }
}

/// Which seeds have another seed of `seeds` within `r` of them (inclusive).
fn crowded(seeds: &[Seed], r: f64) -> Vec<bool> {
    if !(r.is_finite() && r > 0.0) {
        return vec![false; seeds.len()];
    }
    let cell = |x: f64, y: f64| ((x / r).floor() as i64, (y / r).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (j, s) in seeds.iter().enumerate() {
        grid.entry(cell(s.x, s.y)).or_default().push(j);
    }
    seeds
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (cx, cy) = cell(s.x, s.y);
            (cx - 1..=cx + 1).any(|gx| {
                (cy - 1..=cy + 1).any(|gy| {
                    grid.get(&(gx, gy)).is_some_and(|at| {
                        at.iter()
                            .any(|&j| j != i && (seeds[j].x - s.x).hypot(seeds[j].y - s.y) <= r)
                    })
                })
            })
        })
        .collect()
}

/// How many stars each stratum gives: a water level `L` (the largest one with `sum(min(available, L)) <= total`, about
/// the quota `ceil(total / 64)` when every stratum is deep), a short stratum giving all it has and handing its unused
/// share to the rest; the remainder below one level goes one star each to the strata with room, in stratum order.
pub(super) fn water_fill<const N: usize>(available: &[usize; N], total: usize) -> [usize; N] {
    let total = total.min(available.iter().sum());
    let used = |level: usize| available.iter().map(|&n| n.min(level)).sum::<usize>();
    let (mut lo, mut hi) = (0usize, total);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if used(mid) <= total {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut take = [0usize; N];
    for s in 0..N {
        take[s] = available[s].min(lo);
    }
    let mut remaining = total - used(lo);
    for s in 0..N {
        if remaining == 0 {
            break;
        }
        if take[s] < available[s] {
            take[s] += 1;
            remaining -= 1;
        }
    }
    take
}

/// The `t` evenly rank-spaced picks of `members` (ascending brightness): the middle of each of `t` equal ranges.
pub(super) fn spread<'a>(members: &'a [usize], t: usize) -> impl Iterator<Item = usize> + 'a {
    let n = members.len();
    (0..t).map(move |j| members[(2 * j + 1) * n / (2 * t)])
}

/// Population (a): `(seed indices ascending, their strata)`.
#[allow(clippy::too_many_arguments)]
fn statistic_stars(
    seeds: &[Seed],
    filter_snr: &[f64],
    extended: &[bool],
    pre_saturated: &[bool],
    w: usize,
    h: usize,
    field_fwhm: f64,
    statistic: usize,
) -> (Vec<usize>, Vec<u8>) {
    if statistic == 0 || seeds.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let crowded = crowded(seeds, ISOLATION_PER_FIELD_FWHM * field_fwhm);
    let flag = |v: &[bool], i: usize| v.get(i).copied().unwrap_or(false);
    let snr = |i: usize| filter_snr.get(i).copied().unwrap_or(f64::NAN);
    let mut pool: Vec<usize> = (0..seeds.len())
        .filter(|&i| {
            !flag(extended, i)
                && !flag(pre_saturated, i)
                && !crowded[i]
                && snr(i).is_finite()
                && snr(i) >= STATISTIC_SNR_FLOOR
        })
        .collect();
    pool.sort_by(|&a, &b| snr(a).total_cmp(&snr(b)).then(a.cmp(&b)));
    // The lowest 95 % of the eligible pool by filter SNR; the quartiles are equal-count ranks of that band.
    let keep = (STATISTIC_TOP_QUANTILE * pool.len() as f64).floor() as usize;
    pool.truncate(keep);
    stratified_pick(&pool, seeds, w, h, statistic)
}

/// Population (a)'s stratified rank (Plan A Task 13b; the Analysis budget shares its even-rank pick, [`spread`], and
/// its water-fill): `pool` is
/// seed indices ascending by brightness. It is cut into [`BRIGHTNESS_STRATA`] equal-count brightness classes, each
/// seed lands in `cell * 4 + class` of the plane's 4 x 4 grid, `total` stars are handed to the 64 strata water-fill
/// style, and a stratum's picks are evenly spaced by rank (deterministic; never the brightest). Returns the chosen
/// seed indices ascending and their strata.
pub(super) fn stratified_pick(
    pool: &[usize],
    seeds: &[Seed],
    w: usize,
    h: usize,
    total: usize,
) -> (Vec<usize>, Vec<u8>) {
    let band = pool.len();
    let mut strata: Vec<Vec<usize>> = vec![Vec::new(); STRATA];
    for (rank, &i) in pool.iter().enumerate() {
        let class = (BRIGHTNESS_STRATA * rank / band).min(BRIGHTNESS_STRATA - 1);
        let cell = usize::from(cell_of(seeds[i].x, seeds[i].y, w, h));
        strata[cell * BRIGHTNESS_STRATA + class].push(i);
    }
    let mut available = [0usize; STRATA];
    for (s, members) in strata.iter().enumerate() {
        available[s] = members.len();
    }
    let take = water_fill(&available, total);
    let mut chosen: Vec<(usize, u8)> = Vec::new();
    for (s, members) in strata.iter().enumerate() {
        for i in spread(members, take[s]) {
            chosen.push((i, s as u8));
        }
    }
    chosen.sort_unstable();
    chosen.into_iter().unzip()
}

/// The two populations of one plane (module docs). `seeds` are in detector order (brightest first); `filter_snr` and
/// `extended` are the detection report's per-seed vectors (a missing entry reads as not eligible / not extended);
/// `pre_saturated` is [`pre_saturated`]'s; `field_fwhm` is the detection's field width; `statistic` is N and `list` is M.
#[allow(clippy::too_many_arguments)]
pub fn select(
    seeds: &[Seed],
    filter_snr: &[f64],
    extended: &[bool],
    pre_saturated: &[bool],
    w: usize,
    h: usize,
    field_fwhm: f64,
    statistic: usize,
    list: usize,
) -> Population {
    let list = list.min(seeds.len());
    let (stat, stratum) = statistic_stars(
        seeds,
        filter_snr,
        extended,
        pre_saturated,
        w,
        h,
        field_fwhm,
        statistic,
    );
    let mut fit = vec![false; seeds.len()];
    fit[..list].iter_mut().for_each(|f| *f = true);
    for &i in &stat {
        fit[i] = true;
    }
    Population {
        fitted: (0..seeds.len()).filter(|&i| fit[i]).collect(),
        statistic: stat,
        stratum,
        list,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(x: f64, y: f64) -> Seed {
        Seed {
            x,
            y,
            peak: 1.0,
            flux: 1.0,
        }
    }

    /// `n` seeds on a lattice `pitch` px apart over a 400 x 400 plane (rows of 20), filter SNR rising with the index.
    fn lattice(n: usize, pitch: f64) -> (Vec<Seed>, Vec<f64>) {
        let seeds: Vec<Seed> = (0..n)
            .map(|i| {
                seed(
                    10.0 + pitch * (i % 20) as f64,
                    10.0 + pitch * (i / 20) as f64,
                )
            })
            .collect();
        let snr: Vec<f64> = (0..n).map(|i| 20.0 + i as f64).collect();
        (seeds, snr)
    }

    /// [`lattice`] with the filter SNR a permutation of the same values, so every cell holds every brightness class.
    fn scrambled(n: usize, pitch: f64) -> (Vec<Seed>, Vec<f64>) {
        let (seeds, _) = lattice(n, pitch);
        let snr: Vec<f64> = (0..n).map(|i| 20.0 + ((i * 37) % n) as f64).collect();
        (seeds, snr)
    }

    /// A 64 x 64 plane at `bg` with a peaked 3 x 3 star (`top`) at (20, 20) and a dimmer one (`dim`) at (44, 44).
    fn star_plane(bg: f32, top: f32, dim: f32) -> Vec<f32> {
        let mut p = vec![bg; 64 * 64];
        for (cx, cy, v) in [(20usize, 20usize, top), (44, 44, dim)] {
            for y in cy - 1..=cy + 1 {
                for x in cx - 1..=cx + 1 {
                    p[y * 64 + x] = if (x, y) == (cx, cy) {
                        v
                    } else {
                        bg + 0.4 * (v - bg)
                    };
                }
            }
        }
        p
    }

    /// The rule before R13b-10: both clauses on the plane's own `range`, whatever the evidence.
    fn pre_saturated_old(plane: &[f32], seeds: &[Seed], sat: &SaturationPlane) -> Vec<bool> {
        seeds
            .iter()
            .map(|s| {
                if super::super::seeds::near_clip(plane, None, 64, 64, s, Some(sat)) {
                    return true;
                }
                let (cx, cy) = (s.x.round() as i64, s.y.round() as i64);
                let mut stamp = Vec::new();
                for yy in cy - 1..=cy + 1 {
                    for xx in cx - 1..=cx + 1 {
                        stamp.push(PixelSample {
                            x: xx as f64,
                            y: yy as f64,
                            value: plane[yy as usize * 64 + xx as usize] as f64,
                        });
                    }
                }
                plateau_test(&stamp, s.x, s.y, sat).is_some()
            })
            .collect()
    }

    #[test]
    fn a_clip_free_plane_keeps_its_bright_unsaturated_stars_eligible() {
        let plane = star_plane(0.1, 0.8, 0.5);
        let seeds = [seed(20.0, 20.0), seed(44.0, 44.0)];
        // One pixel at the max: no tie, as on a real clip-free plane.
        let sat = SaturationPlane::new(0.1, 0.002, 0.8, 1.0, false).with_max_count(1);
        let old = pre_saturated_old(&plane, &seeds, &sat);
        assert_eq!(
            old,
            vec![true, false],
            "the old rule screened the brightest star"
        );
        assert_eq!(
            pre_saturated(&plane, None, 64, 64, &seeds, &sat),
            vec![false, false]
        );
    }

    #[test]
    fn a_plane_with_a_tie_at_the_max_screens_exactly_as_before() {
        let plane = star_plane(0.1, 0.8, 0.5);
        let seeds = [seed(20.0, 20.0), seed(44.0, 44.0)];
        let sat = SaturationPlane::new(0.1, 0.002, 0.8, 1.0, false).with_max_count(2);
        assert_eq!(
            pre_saturated(&plane, None, 64, 64, &seeds, &sat),
            pre_saturated_old(&plane, &seeds, &sat)
        );
        assert_eq!(
            pre_saturated(&plane, None, 64, 64, &seeds, &sat),
            vec![true, false]
        );
    }

    #[test]
    fn one_full_scale_plateau_core_without_a_tie_screens_exactly_as_before() {
        let mut plane = star_plane(0.1, 0.8, 0.5);
        for y in 31..=33 {
            for x in 31..=33 {
                plane[y * 64 + x] = 0.9;
            }
        }
        let seeds = [seed(20.0, 20.0), seed(44.0, 44.0), seed(32.0, 32.0)];
        let sat = SaturationPlane::new(0.1, 0.002, 0.8, 1.0, false).with_max_count(1);
        assert_eq!(plateau_core_count(&plane, None, 64, 64, &seeds, &sat), 1);
        let got = pre_saturated(&plane, None, 64, 64, &seeds, &sat);
        assert_eq!(got, pre_saturated_old(&plane, &seeds, &sat));
        assert_eq!(got, vec![true, false, true]);
    }

    fn pick(seeds: &[Seed], snr: &[f64], n: usize, m: usize) -> Population {
        let none = vec![false; seeds.len()];
        select(seeds, snr, &none, &none, 400, 400, 4.0, n, m)
    }

    #[test]
    fn list_b_is_the_first_m_seeds_in_detector_order() {
        let (s, snr) = lattice(400, 19.0);
        let p = pick(&s, &snr, 0, 120);
        assert!(p.statistic.is_empty());
        assert_eq!(p.fitted, (0..120).collect::<Vec<_>>());
        assert_eq!(p.list, 120);
        assert_eq!(
            pick(&s, &snr, 0, 10_000).fitted.len(),
            400,
            "M above the count"
        );
    }

    #[test]
    fn a_seed_with_a_neighbour_within_one_and_a_half_field_widths_is_not_isolated() {
        // field 4 px: 6 px. Seeds 0 and 1 are 5.9 apart, 2 and 3 are 6.0 apart (inclusive: crowded); 4.. stand alone.
        let mut s = vec![
            seed(50.0, 50.0),
            seed(55.9, 50.0),
            seed(200.0, 200.0),
            seed(206.0, 200.0),
        ];
        s.extend((0..40).map(|i| {
            seed(
                10.0 + 30.0 * (i % 12) as f64,
                300.0 + 20.0 * (i / 12) as f64,
            )
        }));
        let flags = crowded(&s, 6.0);
        assert_eq!(flags[..4], [true; 4]);
        assert!(flags[4..].iter().all(|&c| !c));
        let snr = vec![50.0; s.len()];
        let p = pick(&s, &snr, 1000, 0);
        assert!(!p.statistic.is_empty());
        assert!(
            p.statistic.iter().all(|&i| i >= 4),
            "the crowded seeds are never in (a): {:?}",
            p.statistic
        );
    }

    #[test]
    fn the_band_is_the_floor_up_to_the_ninety_fifth_percentile_and_flags_leave() {
        let (s, mut snr) = lattice(200, 19.0);
        snr[0] = 5.0; // below the floor
        let mut ext = vec![false; 200];
        ext[1] = true;
        let mut sat = vec![false; 200];
        sat[2] = true;
        let p = select(&s, &snr, &ext, &sat, 400, 400, 4.0, 1000, 0);
        // 200 seeds: 0 (floor), 1 (extended), 2 (saturated) leave -> 197 eligible; the highest 5 % of them go
        // (keep floor(0.95 * 197) = 187 of the lowest).
        assert_eq!(p.statistic.len(), 187);
        assert!(
            !p.statistic.contains(&0) && !p.statistic.contains(&1) && !p.statistic.contains(&2)
        );
        assert!(
            !p.statistic.contains(&199),
            "the brightest are above the percentile"
        );
        assert!(snr[*p.statistic.last().unwrap()] >= STATISTIC_SNR_FLOOR);
    }

    #[test]
    fn strata_get_the_quota_by_water_filling_and_the_picks_are_evenly_ranked() {
        // 4 cells' worth of stars (the top-left 2 x 2 cells of a 400 plane hold the lattice below), plenty each: N = 64
        // over the strata that exist hands every stratum the same share.
        let (s, snr) = scrambled(400, 9.0); // 20 x 20 lattice, 9 px: x, y in 10..181, so cells 0, 1 (x/100), rows 0, 1
        let p = pick(&s, &snr, 64, 0);
        assert_eq!(p.statistic.len(), 64);
        let mut per = [0usize; STRATA];
        for &st in &p.stratum {
            per[usize::from(st)] += 1;
        }
        let used: Vec<usize> = per.iter().copied().filter(|&n| n > 0).collect();
        assert_eq!(used.len(), 16, "4 cells x 4 classes");
        assert!(used.iter().all(|&n| n == 4), "{used:?}");
        // The picks inside one stratum are spread by rank: the SNR of the picks climbs through the stratum.
        let st0 = p.stratum[0];
        let mut snrs: Vec<f64> = p
            .statistic
            .iter()
            .zip(&p.stratum)
            .filter(|&(_, &st)| st == st0)
            .map(|(&i, _)| snr[i])
            .collect();
        snrs.sort_by(f64::total_cmp);
        assert_eq!(snrs.len(), 4);
        assert!(
            snrs.windows(2).all(|w| w[1] > w[0]),
            "distinct picks {snrs:?}"
        );
    }

    #[test]
    fn water_fill_hands_a_short_strata_share_on_and_the_remainder_in_order() {
        let mut avail = [0usize; STRATA];
        avail[0] = 1;
        avail[1] = 100;
        avail[2] = 100;
        avail[3] = 100;
        // 10 over four strata: the short one gives its 1, the rest share 9 as 3, 3, 3.
        let t = water_fill(&avail, 10);
        assert_eq!(t[..4], [1, 3, 3, 3]);
        // 11: remainder goes to the first stratum with room, in stratum order.
        let t = water_fill(&avail, 11);
        assert_eq!(t[..4].iter().sum::<usize>(), 11);
        assert_eq!(t[..4], [1, 4, 3, 3]);
        // More than exists: everything.
        let t = water_fill(&avail, 10_000);
        assert_eq!(t[..4], [1, 100, 100, 100]);
        // Quota ceil(1000 / 64) = 16 on 64 deep strata: 1000 = 40 x 16 + 24 x 15.
        let deep = [500usize; STRATA];
        let t = water_fill(&deep, 1000);
        assert_eq!(t.iter().sum::<usize>(), 1000);
        assert!(t.iter().all(|&n| n == 15 || n == 16));
        assert_eq!(t.iter().filter(|&&n| n == 16).count(), 40);
    }

    #[test]
    fn the_selection_is_deterministic_and_fitted_is_a_u_b() {
        let (s, snr) = lattice(400, 19.0);
        let a = pick(&s, &snr, 100, 50);
        assert_eq!(a, pick(&s, &snr, 100, 50));
        assert_eq!(a.list, 50);
        let mut want: Vec<usize> = (0..50).chain(a.statistic.iter().copied()).collect();
        want.sort_unstable();
        want.dedup();
        assert_eq!(a.fitted, want);
        let (in_a, stratum) = a.fit_flags();
        assert_eq!(in_a.iter().filter(|&&f| f).count(), a.statistic.len());
        for (k, &i) in a.fitted.iter().enumerate() {
            assert_eq!(in_a[k], a.statistic.binary_search(&i).is_ok());
            assert_eq!(stratum[k] == NO_STRATUM, !in_a[k]);
        }
    }

    #[test]
    fn an_empty_or_tiny_plane_selects_what_it_has() {
        let none: Vec<bool> = Vec::new();
        let p = select(&[], &[], &none, &none, 100, 100, 4.0, 1000, 4000);
        assert_eq!((p.fitted.len(), p.statistic.len(), p.list), (0, 0, 0));
        // Three stars: M covers them all; one eligible star cannot clear the 95 % cut (keep = floor(0.95) = 0).
        let s = vec![seed(10.0, 10.0), seed(50.0, 50.0), seed(90.0, 90.0)];
        let p = select(&s, &[30.0; 3], &none, &none, 100, 100, 4.0, 1000, 4000);
        assert_eq!(p.fitted, vec![0, 1, 2]);
        assert_eq!(p.statistic.len(), 2, "keep floor(0.95 * 3) = 2");
    }
}
