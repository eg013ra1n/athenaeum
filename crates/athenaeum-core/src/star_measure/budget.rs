//! The Analysis mode's budget (Plan A Task 12, spec D-3 / §2.1 `budget`, PA-10, ruling F40; design read R12-3…R12-6):
//! N good, unsaturated stars spread over a 4 × 4 grid of the plane, chosen BEFORE the fits so the quick analysis fits
//! about N stars and not every seed. Pure functions over seeds and fits; [`super::measure_plane`]'s `Mode::Budget` arm
//! is their only caller.
//!
//! - [`select_candidates`] (R12-3): the seeds to fit — a quota of `⌈1.25·N/16⌉` per cell by rank, a short cell's unused
//!   share passed on round-robin.
//! - [`trim`] (R12-4, Task 13b review I3): after the fits, the removal that leaves at most N — unflagged fits first, the
//!   kept rank-spread over (cell, brightness-class) strata so no class loses its share.
//! - [`report`] (R12-6): the coverage numbers the lab's `budget coverage` row reads.
//!
//! Every cell assignment is the SEED's ([`cell_of`] of the seed's position), so coverage can never exceed 1.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::fit::StarFlags;
use super::fit::{PsfModel, Seed, StarFit};

/// The default budget N (D-3): the number of good unsaturated stars the Analysis mode keeps. Plan C's `maxStars`
/// default reads it.
pub const DEFAULT_BUDGET: usize = 500;

/// The PSF model the Analysis mode runs at by default (R11b-12, R12-1): Moffat 4, which is zero own-β work.
pub const ANALYSIS_PSF_MODEL: PsfModel = PsfModel::Moffat4;

/// The candidates' share above the budget: the fits the module's own refusals (a blend, an edge star) and the
/// duplicate drop will remove still leave about N for the trim.
pub const CANDIDATE_FACTOR: f64 = 1.25;

/// The grid is `CELLS_PER_SIDE`² cells.
const CELLS_PER_SIDE: usize = 4;
const CELLS: usize = CELLS_PER_SIDE * CELLS_PER_SIDE;

/// What the budget did (R12-6), reported on every `Mode::Budget` plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetReport {
    /// The cells holding measurable stars: an accepted fit before the trim, or a not-selected seed with no accepted
    /// candidate (a cell the budget might have covered). 0 on a plane whose verdict (defocused / trailed) emptied it.
    pub cells_with_stars: u8,
    /// The cells holding a kept fit.
    pub cells_covered: u8,
    /// The seeds fitted.
    pub candidates: usize,
    /// The fits kept after the trim.
    pub kept: usize,
    /// The seeds never fitted.
    pub not_selected: usize,
    /// The accepted fits the trim removed.
    pub trimmed: usize,
}

/// The 4 × 4 cell of a position: `4·cy + cx`, `cx = ⌊4x/w⌋`, `cy = ⌊4y/h⌋`, each clamped to [0, 3].
pub fn cell_of(x: f64, y: f64, w: usize, h: usize) -> u8 {
    let side = CELLS_PER_SIDE as f64;
    let axis = |v: f64, n: usize| (side * v / n as f64).floor().clamp(0.0, side - 1.0) as u8;
    CELLS_PER_SIDE as u8 * axis(y, h) + axis(x, w)
}

/// The seeds to fit, ascending, and the per-cell seed counts (R12-3, Task 13b). The owner's rule is that a statistic
/// never reads the N brightest, so inside a cell the candidates are rank-spread over its brightness range (the same
/// even-rank picks as population (a), [`super::population::spread`]) and not its first `q`. The target is `16·q`,
/// `q = ⌈1.25·N/16⌉`, handed to the 16 cells water-fill style (a short cell gives all it has and passes its share on,
/// so every cell with seeds is covered). A cell's pool is its seeds that are not `skip`ped (knot-flagged, pre-fit
/// saturated) and have a finite `brightness` (higher = brighter: the detection's filter SNR, or [`rank_brightness`]
/// without a detection report); a cell with no such seed falls back to all its seeds, and a pool short of the target
/// is topped up in detector order, so the candidates number `min(16·q, seeds)` as before. `N = 0` selects nothing
/// (one `warn!`).
pub fn select_candidates(
    seeds: &[Seed],
    brightness: &[f64],
    skip: &[bool],
    w: usize,
    h: usize,
    budget: usize,
) -> (Vec<usize>, [u32; 16]) {
    let mut counts = [0u32; CELLS];
    let mut all: Vec<Vec<usize>> = vec![Vec::new(); CELLS];
    for (i, s) in seeds.iter().enumerate() {
        let c = usize::from(cell_of(s.x, s.y, w, h));
        counts[c] += 1;
        all[c].push(i);
    }
    if budget == 0 {
        tracing::warn!(count = seeds.len(), "budget: a budget of 0 selects no star");
        return (Vec::new(), counts);
    }
    let quota = (CANDIDATE_FACTOR * budget as f64 / CELLS as f64).ceil() as usize;
    let target = quota.saturating_mul(CELLS).min(seeds.len());
    let key = |i: usize| brightness.get(i).copied().unwrap_or(f64::NAN);
    let by_brightness = |a: &usize, b: &usize| key(*a).total_cmp(&key(*b)).then(a.cmp(b));
    let pools: Vec<Vec<usize>> = all
        .iter()
        .map(|members| {
            let mut pool: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| !skip.get(i).copied().unwrap_or(false) && key(i).is_finite())
                .collect();
            if pool.is_empty() {
                pool = members.clone();
            }
            pool.sort_by(by_brightness);
            pool
        })
        .collect();
    let mut available = [0usize; CELLS];
    for (c, pool) in pools.iter().enumerate() {
        available[c] = pool.len();
    }
    let take = super::population::water_fill(&available, target);
    let mut chosen: Vec<usize> = Vec::with_capacity(target);
    for (c, pool) in pools.iter().enumerate() {
        chosen.extend(super::population::spread(pool, take[c]));
    }
    if chosen.len() < target {
        let mut picked = vec![false; seeds.len()];
        for &i in &chosen {
            picked[i] = true;
        }
        let fill: Vec<usize> = (0..seeds.len())
            .filter(|&i| !picked[i])
            .take(target - chosen.len())
            .collect();
        chosen.extend(fill);
    }
    chosen.sort_unstable();
    (chosen, counts)
}

/// The brightness key of seeds without a detection report: the detector's order is brightest first, so the key falls
/// with the index.
pub fn rank_brightness(n: usize) -> Vec<f64> {
    (0..n).map(|i| (n - i) as f64).collect()
}

/// SNR descending, NaN after every number.
fn snr_desc(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => b.total_cmp(&a),
    }
}

/// The kept fits of one tier: `total` of `members` (fit indices), cell first and brightness-class second, as
/// [`select_candidates`] picks seeds. The total is handed to the 16 cells water-fill style (every cell with fits is
/// covered once `total` reaches the cell count - the R12 coverage contract); inside a cell the fits are cut into
/// [`BRIGHTNESS_STRATA`] equal-count SNR classes of the tier (NaN faintest, the classes are the tier's own quartiles so a
/// class means the same in every cell), the cell's share is handed to its classes water-fill style and a class's picks
/// are evenly rank-spaced ([`super::population::spread`]) - so a trim never empties the faint end. `covered` marks the
/// cells an earlier tier already keeps a fit in: the tier gives its first stars to the others.
fn pick_tier(
    fits: &[StarFit],
    cells: &[u8],
    members: &mut [usize],
    total: usize,
    covered: &[bool; CELLS],
) -> Vec<usize> {
    use super::population::{spread, water_fill, BRIGHTNESS_STRATA};
    members.sort_by(|&a, &b| snr_desc(fits[a].snr, fits[b].snr).reverse().then(a.cmp(&b)));
    let band = members.len();
    let mut strata: Vec<[Vec<usize>; BRIGHTNESS_STRATA]> =
        (0..CELLS).map(|_| Default::default()).collect();
    for (rank, &k) in members.iter().enumerate() {
        let class = (BRIGHTNESS_STRATA * rank / band).min(BRIGHTNESS_STRATA - 1);
        strata[usize::from(cells[k]).min(CELLS - 1)][class].push(k);
    }
    let mut available = [0usize; CELLS];
    for (c, classes) in strata.iter().enumerate() {
        available[c] = classes.iter().map(Vec::len).sum();
    }
    // One star first to every cell the earlier tier left uncovered (the R12 coverage contract holds across tiers), the
    // rest of the total water-fill style.
    let mut per_cell = [0usize; CELLS];
    let mut left = total.min(band);
    for c in 0..CELLS {
        if left > 0 && !covered[c] && available[c] > 0 {
            per_cell[c] = 1;
            left -= 1;
        }
    }
    let mut room = available;
    for c in 0..CELLS {
        room[c] -= per_cell[c];
    }
    for (c, t) in water_fill(&room, left).into_iter().enumerate() {
        per_cell[c] += t;
    }
    let mut kept = Vec::with_capacity(total);
    for (c, classes) in strata.iter().enumerate() {
        let mut in_class = [0usize; BRIGHTNESS_STRATA];
        for (i, m) in classes.iter().enumerate() {
            in_class[i] = m.len();
        }
        let take = water_fill(&in_class, per_cell[c]);
        for (m, t) in classes.iter().zip(take) {
            kept.extend(spread(m, t));
        }
    }
    kept
}

/// The fits to remove, ascending, so that at most `budget` remain; `cells[k]` = the cell of fit k's SEED (R12-4, Task 13b
/// review I3). Tier 0 is `flags.in_statistic()` (neither saturated nor extended), tier 1 the others; the flagged fits
/// only fill the slots no unflagged fit wants. Inside a tier the kept fits are rank-spread over cells, then brightness
/// classes, as population (a)'s pick is ([`pick_tier`]) - the old trim dropped the lowest-SNR fits of every cell
/// and so took about half of the faintest class; this one keeps every class its share.
pub fn trim(fits: &[StarFit], cells: &[u8], budget: usize) -> Vec<usize> {
    debug_assert_eq!(fits.len(), cells.len());
    if fits.len() <= budget {
        return Vec::new();
    }
    let (mut good, mut flagged): (Vec<usize>, Vec<usize>) =
        (0..fits.len()).partition(|&k| fits[k].flags.in_statistic());
    let mut kept = if good.len() >= budget {
        pick_tier(fits, cells, &mut good, budget, &[false; CELLS])
    } else {
        let rest = budget - good.len();
        let mut covered = [false; CELLS];
        for &k in &good {
            covered[usize::from(cells[k]).min(CELLS - 1)] = true;
        }
        let mut kept = good;
        kept.extend(pick_tier(fits, cells, &mut flagged, rest, &covered));
        kept
    };
    kept.sort_unstable();
    let mut removed = Vec::with_capacity(fits.len() - kept.len());
    let mut it = kept.iter().peekable();
    for k in 0..fits.len() {
        if it.peek() == Some(&&k) {
            it.next();
        } else {
            removed.push(k);
        }
    }
    removed
}

/// The report (R12-6) from every seed's cell, the candidates, the accepted fits before the trim and the kept fits (seed
/// indices), and whether a plane verdict (defocused / trailed) emptied the plane: the plane then holds no measurable
/// star by the module's own verdict, so its coverage is 0 / 0 and the scorecard skips it.
pub fn report(
    seed_cells: &[u8],
    candidates: &[usize],
    accepted: &[usize],
    kept: &[usize],
    plane_verdict: bool,
) -> BudgetReport {
    let seeds = seed_cells.len();
    let mut with_stars = [false; CELLS];
    let mut covered = [false; CELLS];
    if !plane_verdict {
        for &i in accepted {
            with_stars[usize::from(seed_cells[i])] = true;
        }
        let mut is_candidate = vec![false; seeds];
        for &i in candidates {
            is_candidate[i] = true;
        }
        let holds_accepted = with_stars;
        for (i, &c) in seed_cells.iter().enumerate() {
            if !is_candidate[i] && !holds_accepted[usize::from(c)] {
                with_stars[usize::from(c)] = true;
            }
        }
        for &i in kept {
            covered[usize::from(seed_cells[i])] = true;
        }
    }
    let count = |a: &[bool; CELLS]| a.iter().filter(|&&b| b).count() as u8;
    BudgetReport {
        cells_with_stars: count(&with_stars),
        cells_covered: count(&covered),
        candidates: candidates.len(),
        kept: kept.len(),
        not_selected: seeds - candidates.len(),
        trimmed: accepted.len().saturating_sub(kept.len()),
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
    /// A seed in the middle of cell `c` of a 400² plane (100-px cells).
    fn in_cell(c: u8) -> Seed {
        seed(50.0 + 100.0 * (c % 4) as f64, 50.0 + 100.0 * (c / 4) as f64)
    }
    fn fit(snr: f64, flags: StarFlags) -> StarFit {
        StarFit {
            x: 0.0,
            y: 0.0,
            background: 0.0,
            amplitude: 1.0,
            fwhm_x: 2.0,
            fwhm_y: 2.0,
            fwtm_x: 4.0,
            fwtm_y: 4.0,
            theta: 0.0,
            beta: 4.0,
            residual: 0.0,
            signal: 1.0,
            area: 1.0,
            peak5: f64::NAN,
            sky_ring: 0.0,
            sky_ring_sigma: 1.0,
            sky_ring_n: 10,
            snr,
            hfr: 1.0,
            flags,
        }
    }

    /// M-1 (Task 12 review): an absurd budget selects every seed; the target `16 * quota` neither panics (debug) nor
    /// wraps to a small number (release).
    #[test]
    fn an_absurd_budget_selects_every_seed_without_overflow() {
        let seeds: Vec<Seed> = (0..40).map(|i| in_cell((i % 16) as u8)).collect();
        for budget in [usize::MAX, usize::MAX / 2, 1 << 62] {
            let (c, n) = pick(&seeds, budget);
            assert_eq!(c, (0..40).collect::<Vec<_>>(), "budget {budget}");
            assert_eq!(n.iter().sum::<u32>(), 40);
        }
    }

    #[test]
    fn a_cell_is_the_4x4_grid_index_of_the_position() {
        assert_eq!(cell_of(0.0, 0.0, 400, 400), 0);
        assert_eq!(cell_of(99.999, 0.0, 400, 400), 0);
        assert_eq!(cell_of(100.0, 0.0, 400, 400), 1);
        assert_eq!(cell_of(399.5, 399.5, 400, 400), 15);
        assert_eq!(cell_of(-0.5, 400.0, 400, 400), 12, "clamped into the grid");
        assert_eq!(cell_of(250.0, 150.0, 400, 300), 10);
    }

    /// [`select_candidates`] on the detector order (no detection report), nothing skipped.
    fn pick(seeds: &[Seed], budget: usize) -> (Vec<usize>, [u32; 16]) {
        select_candidates(seeds, &rank_brightness(seeds.len()), &[], 400, 400, budget)
    }

    /// R12-3 re-pinned by Task 13b (the shared stratified rank, the owner's rule that a statistic never reads the N
    /// brightest): budget 64 -> quota 5, target 80 candidates, spread over the 4 x 4 cells AND the four brightness
    /// classes. Before: the 5 brightest of every cell.
    #[test]
    fn the_candidates_spread_over_the_cells_and_the_brightness_not_the_brightest() {
        let full: Vec<Seed> = (0..160).map(|i| in_cell((i % 16) as u8)).collect(); // 10 per cell, interleaved by rank
        let (c, n) = pick(&full, 64);
        assert_eq!(n, [10; 16]);
        assert_eq!(c.len(), 80);
        assert_ne!(c, (0..80).collect::<Vec<_>>(), "not the brightest 80");
        assert!(c.windows(2).all(|p| p[0] < p[1]), "ascending");
        for class in 0..4 {
            let n_class = c.iter().filter(|&&i| i / 40 == class).count();
            assert!(
                (15..=25).contains(&n_class),
                "class {class} holds {n_class} of 40 seeds"
            );
        }
        for cell in 0..16 {
            let n_cell = c.iter().filter(|&&i| i % 16 == cell).count();
            assert_eq!(n_cell, 5, "cell {cell} gets its quota");
        }
    }

    /// R12-3's short-cell rule survives: a short cell's unused share passes to the others, every seed of a short
    /// cell is a candidate, and the long cell's picks span its brightness range.
    #[test]
    fn a_short_cell_hands_its_share_on() {
        let mut short: Vec<Seed> = (0..100).map(|_| in_cell(0)).collect(); // ranks 0..99 in cell 0
        short.extend((0..30).map(|i| in_cell(1 + (i / 2) as u8))); // ranks 100..129: two per cell 1..=15
        let (c, n) = pick(&short, 64);
        assert_eq!(n[0], 100);
        assert!(n[1..].iter().all(|&k| k == 2));
        assert_eq!(c.len(), 80);
        assert!(
            (100..130).all(|i| c.contains(&i)),
            "every short cell's seeds are candidates"
        );
        let cell0: Vec<usize> = c.iter().copied().filter(|&i| i < 100).collect();
        assert_eq!(cell0.len(), 50, "cell 0 takes the 50 slots left");
        assert!(
            cell0.iter().any(|&i| i >= 50),
            "its picks reach into the faint half, not the 50 brightest"
        );
    }

    /// Skipped (flagged) and key-less seeds enter only when the pool is short, brightest first; the selection is
    /// deterministic.
    #[test]
    fn skipped_seeds_only_top_up_a_short_pool() {
        let seeds: Vec<Seed> = (0..60).map(|i| in_cell((i % 16) as u8)).collect();
        let mut key = rank_brightness(60);
        key[7] = f64::NAN;
        let mut skip = vec![false; 60];
        skip[0] = true;
        // budget 25 -> target 32 of 60: the pool of 58 covers it, so neither the skipped nor the NaN seed is taken
        let (c, _) = select_candidates(&seeds, &key, &skip, 400, 400, 25);
        assert_eq!(c.len(), 32);
        assert!(!c.contains(&0) && !c.contains(&7));
        assert_eq!(select_candidates(&seeds, &key, &skip, 400, 400, 25).0, c);
        // budget 64 -> target 80 > 60: everything, the two left out of the pool last
        let (c, _) = select_candidates(&seeds, &key, &skip, 400, 400, 64);
        assert_eq!(c, (0..60).collect::<Vec<_>>());
    }

    #[test]
    fn a_budget_at_or_above_the_seed_count_takes_every_seed_and_zero_takes_none() {
        let s: Vec<Seed> = (0..40).map(|i| in_cell((i * 7 % 16) as u8)).collect();
        assert_eq!(pick(&s, 40).0, (0..40).collect::<Vec<_>>());
        assert_eq!(pick(&s, 1000).0.len(), 40);
        assert!(pick(&s, 0).0.is_empty());
        assert!(trim(&vec![fit(10.0, StarFlags::default()); 5], &[0; 5], 5).is_empty());
    }

    /// R12-4 (Task 13b review I3). Cell 0 holds SNR 50 (saturated), 40, 30, 30, 20; cell 1 holds 25 and NaN; cell 2
    /// holds 5 (extended). The flagged fits (0 and 7) go before any unflagged one, and a trim that leaves room for all
    /// of them removes nothing.
    #[test]
    fn the_trim_keeps_unflagged_fits_first() {
        let ok = StarFlags::default();
        let fits = [
            fit(50.0, StarFlags::SATURATED),
            fit(40.0, ok),
            fit(30.0, ok),
            fit(30.0, ok),
            fit(20.0, ok),
            fit(25.0, ok),
            fit(f64::NAN, ok),
            fit(5.0, StarFlags::EXTENDED),
        ];
        let cells = [0, 0, 0, 0, 0, 1, 1, 2];
        assert!(trim(&fits, &cells, 8).is_empty());
        assert_eq!(
            trim(&fits, &cells, 6),
            vec![0, 7],
            "both flagged go before any unflagged fit"
        );
        // Five of the six unflagged fits survive the next cut, one flagged-free removal per step, never a flagged keep.
        for budget in 1..6 {
            let removed = trim(&fits, &cells, budget);
            assert_eq!(removed.len(), 8 - budget);
            assert!(
                removed.contains(&0) && removed.contains(&7),
                "{budget}: {removed:?}"
            );
        }
        assert_eq!(trim(&fits, &cells, 0).len(), 8);
    }

    /// Review I3: the trim spreads the kept fits over the brightness classes. 400 unflagged fits in one cell, SNR 1..400,
    /// budget 200: every SNR quartile keeps exactly 50 (the old trim kept the 200 brightest and emptied the two faint
    /// classes), and the faintest class is still present.
    #[test]
    fn the_trim_keeps_the_faintest_class_its_share() {
        let ok = StarFlags::default();
        let fits: Vec<StarFit> = (1..=400).map(|i| fit(f64::from(i), ok)).collect();
        let removed = trim(&fits, &vec![0; 400], 200);
        assert_eq!(removed.len(), 200);
        let mut kept_per_class = [0usize; 4];
        for k in (0..400).filter(|k| !removed.contains(k)) {
            kept_per_class[k / 100] += 1;
        }
        assert_eq!(kept_per_class, [50; 4], "{kept_per_class:?}");
        // Two cells of unequal depth: the water level hands the short cell's unused share on, every class survives.
        let cells: Vec<u8> = (0..400).map(|k| u8::from(k % 8 == 0)).collect();
        let removed = trim(&fits, &cells, 200);
        assert_eq!(removed.len(), 200);
        assert!(
            (0..100).any(|k| !removed.contains(&k)),
            "the faintest class keeps stars"
        );
    }

    /// R12-6. Seeds lie in cells 0, 0, 1, 2 and 3; the candidates are seeds 0, 2 and 3; the accepted fits are seeds 0
    /// and 3; seed 0 is kept.
    #[test]
    fn the_report_counts_the_cells_the_budget_could_have_covered() {
        let cells = [0u8, 0, 1, 2, 3];
        let r = report(&cells, &[0, 2, 3], &[0, 3], &[0], false);
        // cell 0: with stars, covered; cell 1: its only candidate refused, nothing unexamined, not counted; cell 2: accepted then
        // trimmed, with stars but not covered; cell 3: no candidate, seed 4 not selected, with stars
        assert_eq!((r.cells_with_stars, r.cells_covered), (3, 1));
        assert_eq!(
            (r.candidates, r.kept, r.not_selected, r.trimmed),
            (3, 1, 2, 1)
        );
        assert_eq!(
            report(&cells, &[0, 2, 3], &[], &[], true).cells_with_stars,
            0,
            "a plane verdict: no measurable star"
        );
    }
}
