//! The Analysis mode's budget (Plan A Task 12, spec D-3 / §2.1 `budget`, PA-10, ruling F40; design read R12-3…R12-6):
//! N good, unsaturated stars spread over a 4 × 4 grid of the plane, chosen BEFORE the fits so the quick analysis fits
//! about N stars and not every seed. Pure functions over seeds and fits; [`super::measure_plane`]'s `Mode::Budget` arm
//! is their only caller.
//!
//! - [`select_candidates`] (R12-3): the seeds to fit — a quota of `⌈1.25·N/16⌉` per cell by rank, a short cell's unused
//!   share passed on round-robin.
//! - [`trim`] (R12-4): after the fits, the removal that leaves at most N — unflagged fits first, round-robin by SNR over
//!   the cells.
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

/// The seeds to fit, ascending (= rank order), and the per-cell seed counts (R12-3). The seeds are brightest-first (the
/// detector's order), so the seed index is the rank. The quota is `q = ⌈1.25·N/16⌉` per cell and the target `16·q`;
/// the candidates are the first `min(target, seeds)` of the keys (rank within the seed's own cell, seed index), which is
/// the round-robin with a short cell's share handed on, the last partial round going to the cells whose next seed ranks
/// first. `N = 0` selects nothing (one `warn!`). One sort of at most 24 576 keys.
pub fn select_candidates(
    seeds: &[Seed],
    w: usize,
    h: usize,
    budget: usize,
) -> (Vec<usize>, [u32; 16]) {
    let mut counts = [0u32; CELLS];
    let keys: Vec<(u32, usize)> = seeds
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let c = usize::from(cell_of(s.x, s.y, w, h));
            counts[c] += 1;
            (counts[c] - 1, i)
        })
        .collect();
    if budget == 0 {
        tracing::warn!(count = seeds.len(), "budget: a budget of 0 selects no star");
        return (Vec::new(), counts);
    }
    let quota = (CANDIDATE_FACTOR * budget as f64 / CELLS as f64).ceil() as usize;
    let target = quota.saturating_mul(CELLS).min(seeds.len());
    let mut order = keys;
    order.sort_unstable();
    let mut chosen: Vec<usize> = order.into_iter().take(target).map(|(_, i)| i).collect();
    chosen.sort_unstable();
    (chosen, counts)
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

/// The fits to remove, ascending, so that at most `budget` remain; `cells[k]` = the cell of fit k's SEED (R12-4). Tier 0
/// is `flags.in_statistic()` (neither saturated nor extended), tier 1 the others; a fit's round is its rank within
/// (cell, tier) by SNR descending (NaN last), then fit index; the kept are the first `budget` of (tier, round, SNR
/// descending, fit index). Flagged fits so fill only the slots no unflagged fit wants.
pub fn trim(fits: &[StarFit], cells: &[u8], budget: usize) -> Vec<usize> {
    debug_assert_eq!(fits.len(), cells.len());
    if fits.len() <= budget {
        return Vec::new();
    }
    let tier = |k: usize| usize::from(!fits[k].flags.in_statistic());
    let mut by_group: Vec<usize> = (0..fits.len()).collect();
    by_group.sort_by(|&a, &b| {
        (cells[a], tier(a))
            .cmp(&(cells[b], tier(b)))
            .then_with(|| snr_desc(fits[a].snr, fits[b].snr))
            .then(a.cmp(&b))
    });
    let mut round = vec![0usize; fits.len()];
    let mut run = 0usize;
    for (n, &k) in by_group.iter().enumerate() {
        let new_group = n == 0 || {
            let p = by_group[n - 1];
            (cells[p], tier(p)) != (cells[k], tier(k))
        };
        run = if new_group { 0 } else { run + 1 };
        round[k] = run;
    }
    let mut order: Vec<usize> = (0..fits.len()).collect();
    order.sort_by(|&a, &b| {
        (tier(a), round[a])
            .cmp(&(tier(b), round[b]))
            .then_with(|| snr_desc(fits[a].snr, fits[b].snr))
            .then(a.cmp(&b))
    });
    let mut removed: Vec<usize> = order.split_off(budget);
    removed.sort_unstable();
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
            let (c, n) = select_candidates(&seeds, 400, 400, budget);
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

    /// R12-3: budget 64 → quota 5, target 80.
    #[test]
    fn every_cell_gets_its_quota_by_rank_and_a_short_cell_hands_its_share_on() {
        let full: Vec<Seed> = (0..160).map(|i| in_cell((i % 16) as u8)).collect(); // 10 per cell, interleaved by rank
        let (c, n) = select_candidates(&full, 400, 400, 64);
        assert_eq!(n, [10; 16]);
        assert_eq!(
            c,
            (0..80).collect::<Vec<_>>(),
            "the 5 brightest of every cell"
        );
        let mut short: Vec<Seed> = (0..100).map(|_| in_cell(0)).collect(); // ranks 0..99 in cell 0
        short.extend((0..30).map(|i| in_cell(1 + (i / 2) as u8))); // ranks 100..129: two per cell 1..=15
        let (c, n) = select_candidates(&short, 400, 400, 64);
        assert_eq!(n[0], 100);
        assert!(n[1..].iter().all(|&k| k == 2));
        assert_eq!(c.len(), 80);
        assert!(
            (100..130).all(|i| c.contains(&i)),
            "every short cell's seeds are candidates"
        );
        assert_eq!(
            c[..50],
            (0..50).collect::<Vec<_>>()[..],
            "cell 0 takes the 50 slots left, brightest first"
        );
        assert!(c.windows(2).all(|p| p[0] < p[1]), "ascending = rank order");
    }

    /// R12-3's tie-break: budget 25 → quota 2, target 32. Rounds 1–9 take 30 (nine each in cells 0–2, three in cell 3).
    /// Round 10 has 2 slots for cells 0, 1 and 2, whose tenth seeds rank 32, 31 and 30: cells 2 and 1 take them.
    #[test]
    fn the_last_partial_round_goes_to_the_cells_whose_next_seed_ranks_first() {
        let mut s: Vec<Seed> = (0..27).map(|r| in_cell((r % 3) as u8)).collect();
        s.extend((0..3).map(|_| in_cell(3)));
        s.extend([2u8, 1, 0, 0, 1, 2].map(in_cell));
        let (c, _) = select_candidates(&s, 400, 400, 25);
        assert_eq!(c, (0..32).collect::<Vec<_>>());
        assert_eq!(
            select_candidates(&s, 400, 400, 25).0,
            c,
            "the same input, the same selection"
        );
    }

    #[test]
    fn a_budget_at_or_above_the_seed_count_takes_every_seed_and_zero_takes_none() {
        let s: Vec<Seed> = (0..40).map(|i| in_cell((i * 7 % 16) as u8)).collect();
        assert_eq!(
            select_candidates(&s, 400, 400, 40).0,
            (0..40).collect::<Vec<_>>()
        );
        assert_eq!(select_candidates(&s, 400, 400, 1000).0.len(), 40);
        assert!(select_candidates(&s, 400, 400, 0).0.is_empty());
        assert!(trim(&vec![fit(10.0, StarFlags::default()); 5], &[0; 5], 5).is_empty());
    }

    /// R12-4. Cell 0 holds SNR 50 (saturated), 40, 30, 30, 20; cell 1 holds 25 and NaN; cell 2 holds 5 (extended).
    /// Unflagged first: round 1 is 40 and 25; round 2 is fit 2 (the 30-tie to the lower index) and the NaN; then
    /// 30 and 20; the flagged come last.
    #[test]
    fn the_trim_keeps_unflagged_fits_first_round_robin_by_snr() {
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
        assert_eq!(
            trim(&fits, &cells, 4),
            vec![0, 3, 4, 7],
            "round-robin: cell 1's NaN (round 2) before fit 3 (round 3)"
        );
        assert_eq!(trim(&fits, &cells, 3), vec![0, 3, 4, 6, 7]);
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
