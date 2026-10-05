//! The star selection for the LN scale (spec 2026-10-04 §3, L-11, L-12,
//! L-14): relative saturation, peak SNR, the brightest by the reference's
//! flux, capped on PAIRS, and the bright-quarter fallback. It runs after
//! pairing and before the flux ratio, on both scale paths, both
//! estimators, both seeds-calibration arms and the local-scale input — all
//! through `scale.rs`'s two `_core` bodies.

use super::background::TileNoise;
use super::scale::MIN_MATCHES;
use super::LnError;
use crate::stacking::integrate::LnStarSelection;
use crate::stacking::psf_signal::StarFit;

/// One side of a pair: the plane the scale reads (sanitized for detection),
/// and its background model's noise and maximum.
#[derive(Clone, Copy)]
pub struct SelectionSide<'a> {
    pub plane: &'a [f32],
    pub width: usize,
    pub height: usize,
    pub noise: &'a TileNoise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRule {
    Snr,
    BrightQuarter,
}

impl SelectionRule {
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionRule::Snr => "snr",
            SelectionRule::BrightQuarter => "brightQuarter",
        }
    }
}

/// The pairs an estimator may use, brightest first, and how many of the
/// valid ones it keeps. `fwhm_set` is how many of the first `ranked`
/// entries define the common FWHM (all SNR passers, or the fallback's
/// quota) — spec §4.1's "pairs that passed steps 1–2 (or the fallback set)".
#[derive(Debug, Clone, PartialEq)]
pub struct Candidates {
    pub ranked: Vec<(usize, usize)>,
    pub quota: usize,
    pub rule: SelectionRule,
    pub fwhm_set: usize,
}

/// The largest pixel within `max(2, FWHM)` px of the fit's centroid
/// (pixel centres, inside the plane).
fn local_peak(side: &SelectionSide<'_>, f: &StarFit) -> f64 {
    let r = f.fwhm().max(2.0);
    let (x0, x1) = (
        (f.x - r).floor().max(0.0) as usize,
        ((f.x + r).ceil().max(0.0) as usize).min(side.width - 1),
    );
    let (y0, y1) = (
        (f.y - r).floor().max(0.0) as usize,
        ((f.y + r).ceil().max(0.0) as usize).min(side.height - 1),
    );
    let mut best = f64::NEG_INFINITY;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f64 - f.x, y as f64 - f.y);
            if dx * dx + dy * dy <= r * r {
                let v = side.plane[y * side.width + x];
                if v.is_finite() {
                    best = best.max(v as f64);
                }
            }
        }
    }
    best
}

/// L-14: relative saturation against this side's hot-pixel-corrected max.
fn saturated(side: &SelectionSide<'_>, f: &StarFit, sel: &LnStarSelection) -> bool {
    let max = side.noise.plane_max() as f64;
    max.is_finite() && local_peak(side, f) > sel.saturation * max
}

/// L-12 with P-18: no noise information (σ ≤ 0 or non-finite) is no cut.
fn snr_passes(side: &SelectionSide<'_>, f: &StarFit, sel: &LnStarSelection) -> bool {
    if sel.min_peak_snr == f64::NEG_INFINITY {
        return true; // OFF means off, even for a fit with no peak
    }
    let signal = f.peak5 - f.background;
    let sigma = side.noise.sigma_local(f.x, f.y);
    if !(sigma.is_finite() && sigma > 0.0) {
        return signal > 0.0;
    }
    signal / sigma >= sel.min_peak_snr
}

pub fn rank_candidates(
    pairs: &[(usize, usize)],
    ref_fits: &[StarFit],
    tgt_fits: &[StarFit],
    reference: &SelectionSide<'_>,
    target: &SelectionSide<'_>,
    sel: &LnStarSelection,
) -> Result<Candidates, LnError> {
    let rank = |v: &mut Vec<(usize, usize)>| {
        v.sort_by(|&(ra, ta), &(rb, tb)| {
            ref_fits[rb]
                .signal
                .total_cmp(&ref_fits[ra].signal)
                .then(ra.cmp(&rb))
                .then(ta.cmp(&tb))
        });
    };
    let mut unsaturated: Vec<(usize, usize)> = pairs
        .iter()
        .copied()
        .filter(|&(r, t)| {
            !saturated(reference, &ref_fits[r], sel) && !saturated(target, &tgt_fits[t], sel)
        })
        .collect();
    let mut snr: Vec<(usize, usize)> = unsaturated
        .iter()
        .copied()
        .filter(|&(r, t)| {
            snr_passes(reference, &ref_fits[r], sel) && snr_passes(target, &tgt_fits[t], sel)
        })
        .collect();
    let cap = sel.max_stars as usize;
    if snr.len() >= MIN_MATCHES {
        rank(&mut snr);
        let fwhm_set = snr.len();
        return Ok(Candidates {
            ranked: snr,
            quota: cap,
            rule: SelectionRule::Snr,
            fwhm_set,
        });
    }
    let n = unsaturated.len();
    if n < MIN_MATCHES {
        return Err(LnError::TooFewMatches { matches: n });
    }
    rank(&mut unsaturated);
    let quota = n.div_ceil(4).max(MIN_MATCHES).min(cap);
    Ok(Candidates {
        ranked: unsaturated,
        quota,
        rule: SelectionRule::BrightQuarter,
        fwhm_set: quota,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 256;
    const H: usize = 256;

    fn fit(x: f64, y: f64, fwhm: f64, background: f64, signal: f64, peak5: f64) -> StarFit {
        StarFit {
            x,
            y,
            background,
            amplitude: peak5 - background,
            fwhm_x: fwhm,
            fwhm_y: fwhm,
            fwtm_x: fwhm * 1.8,
            fwtm_y: fwhm * 1.8,
            theta: 0.0,
            beta: 4.0,
            residual: 0.02,
            signal,
            area: 28.0,
            peak5,
        }
    }

    /// Fit `i` of a 10-wide lattice, 22 px apart: windows never overlap.
    fn at(i: usize) -> (f64, f64) {
        (12.0 + (i % 10) as f64 * 22.0, 12.0 + (i / 10) as f64 * 22.0)
    }

    /// `n` clean fits: bright (SNR 200 at σ = 0.001), signal distinct and
    /// increasing with `i` unless the caller overrides it.
    fn clean(n: usize) -> Vec<StarFit> {
        (0..n)
            .map(|i| {
                let (x, y) = at(i);
                fit(x, y, 3.0, 0.1, 100.0 + i as f64, 0.3)
            })
            .collect()
    }

    fn flat() -> Vec<f32> {
        vec![0.1f32; W * H]
    }

    fn paint(plane: &mut [f32], f: &StarFit, v: f32) {
        plane[f.y.round() as usize * W + f.x.round() as usize] = v;
    }

    fn side<'a>(plane: &'a [f32], noise: &'a TileNoise) -> SelectionSide<'a> {
        SelectionSide {
            plane,
            width: W,
            height: H,
            noise,
        }
    }

    fn diagonal(n: usize) -> Vec<(usize, usize)> {
        (0..n).map(|i| (i, i)).collect()
    }

    // Planes are flat 0.1 except where a test paints a star's pixels.
    #[test]
    fn a_saturated_star_is_left_out_on_either_side() {
        let (rf, tf) = (clean(27), clean(27));
        let (mut rp, mut tp) = (flat(), flat());
        paint(&mut rp, &rf[0], 0.9); // 0.9 > 0.75 × 1.0 on the reference
        paint(&mut tp, &tf[1], 0.9); // and on the target
        let noise = TileNoise::uniform(0.001, 1.0);
        let c = rank_candidates(
            &diagonal(27),
            &rf,
            &tf,
            &side(&rp, &noise),
            &side(&tp, &noise),
            &LnStarSelection::default(),
        )
        .unwrap();
        assert_eq!(c.ranked.len(), 25);
        assert!(!c.ranked.contains(&(0, 0)));
        assert!(!c.ranked.contains(&(1, 1)));
        assert_eq!(c.rule, SelectionRule::Snr);
    }

    #[test]
    fn peak_snr_is_judged_on_both_sides_with_the_local_noise() {
        let noise = TileNoise::uniform(0.001, 1.0);
        let plane = flat();
        let run = |target_excess: f64| {
            let rf = clean(26);
            let mut tf = clean(26);
            tf[25].peak5 = 0.1 + target_excess;
            rank_candidates(
                &diagonal(26),
                &rf,
                &tf,
                &side(&plane, &noise),
                &side(&plane, &noise),
                &LnStarSelection::default(),
            )
            .unwrap()
        };
        let out = run(0.03); // SNR 30 < 40
        assert_eq!(out.rule, SelectionRule::Snr);
        assert!(!out.ranked.contains(&(25, 25)));
        assert_eq!(out.ranked.len(), 25);
        let inn = run(0.05); // SNR 50
        assert_eq!(inn.rule, SelectionRule::Snr);
        assert!(inn.ranked.contains(&(25, 25)));
        assert_eq!(inn.ranked.len(), 26);
        // The reference side is judged too.
        let mut rf = clean(26);
        rf[3].peak5 = 0.13;
        let c = rank_candidates(
            &diagonal(26),
            &rf,
            &clean(26),
            &side(&plane, &noise),
            &side(&plane, &noise),
            &LnStarSelection::default(),
        )
        .unwrap();
        assert!(!c.ranked.contains(&(3, 3)));
    }

    #[test]
    fn pairs_are_ranked_by_the_reference_signal_and_capped_at_max_stars() {
        // Distinct signals in shuffled order (7 is coprime with 30).
        let mut rf = clean(30);
        for (i, f) in rf.iter_mut().enumerate() {
            f.signal = 1000.0 + ((i * 7) % 30) as f64;
        }
        let tf = clean(30);
        let noise = TileNoise::uniform(0.001, 1.0);
        let plane = flat();
        let sel = LnStarSelection {
            max_stars: 25,
            ..LnStarSelection::default()
        };
        let c = rank_candidates(
            &diagonal(30),
            &rf,
            &tf,
            &side(&plane, &noise),
            &side(&plane, &noise),
            &sel,
        )
        .unwrap();
        let mut expected = diagonal(30);
        expected.sort_by(|a, b| rf[b.0].signal.total_cmp(&rf[a.0].signal));
        assert_eq!(c.ranked, expected);
        assert_eq!(c.quota, 25);
        assert_eq!(c.fwhm_set, 30);
        assert_eq!(c.rule, SelectionRule::Snr);

        // Two pairs sharing one reference fit tie on signal: target order.
        let mut pairs = diagonal(30);
        pairs.insert(0, (5, 29));
        let tf2 = clean(30);
        let c2 = rank_candidates(
            &pairs,
            &rf,
            &tf2,
            &side(&plane, &noise),
            &side(&plane, &noise),
            &sel,
        )
        .unwrap();
        let p5: Vec<(usize, usize)> = c2.ranked.iter().copied().filter(|p| p.0 == 5).collect();
        assert_eq!(p5, vec![(5, 5), (5, 29)]);
    }

    #[test]
    fn fewer_than_twenty_snr_pairs_fall_back_to_the_bright_quarter() {
        let noise = TileNoise::uniform(0.001, 1.0);
        let plane = flat();
        // 100 pairs pass saturation, only the first 10 pass SNR.
        let rf = clean(100);
        let mut tf = clean(100);
        for f in tf.iter_mut().skip(10) {
            f.peak5 = 0.101; // SNR 1
        }
        let c = rank_candidates(
            &diagonal(100),
            &rf,
            &tf,
            &side(&plane, &noise),
            &side(&plane, &noise),
            &LnStarSelection::default(),
        )
        .unwrap();
        assert_eq!(c.rule, SelectionRule::BrightQuarter);
        assert_eq!(c.ranked.len(), 100);
        assert_eq!(c.quota, 25);
        assert_eq!(c.fwhm_set, 25);
        let sig: Vec<f64> = c.ranked.iter().map(|&(r, _)| rf[r].signal).collect();
        assert!(sig.windows(2).all(|w| w[0] >= w[1]));

        // 60 pass saturation and none pass SNR: quarter 15 lifts to 20.
        let rf = clean(60);
        let mut tf = clean(60);
        for f in tf.iter_mut() {
            f.peak5 = 0.101;
        }
        let c = rank_candidates(
            &diagonal(60),
            &rf,
            &tf,
            &side(&plane, &noise),
            &side(&plane, &noise),
            &LnStarSelection::default(),
        )
        .unwrap();
        assert_eq!(c.rule, SelectionRule::BrightQuarter);
        assert_eq!(c.quota, 20);
        assert_eq!(c.fwhm_set, 20);
    }

    #[test]
    fn fewer_than_twenty_after_saturation_is_too_few_matches() {
        let noise = TileNoise::uniform(0.001, 1.0);
        let plane = flat();
        let f = clean(19);
        let r = rank_candidates(
            &diagonal(19),
            &f,
            &f,
            &side(&plane, &noise),
            &side(&plane, &noise),
            &LnStarSelection::default(),
        );
        assert!(
            matches!(r, Err(LnError::TooFewMatches { matches: 19 })),
            "{r:?}"
        );
    }

    #[test]
    fn off_keeps_every_pair_in_rank_order() {
        let noise = TileNoise::uniform(0.001, 1.0);
        let (rf, mut tf) = (clean(40), clean(40));
        tf[7].peak5 = f64::NAN; // no peak at all: OFF still keeps it
        tf[8].peak5 = 0.1; // a faint one
        let (mut rp, tp) = (flat(), flat());
        paint(&mut rp, &rf[9], 1.0); // a saturated one
        let c = rank_candidates(
            &diagonal(40),
            &rf,
            &tf,
            &side(&rp, &noise),
            &side(&tp, &noise),
            &LnStarSelection::OFF,
        )
        .unwrap();
        assert_eq!(c.ranked.len(), 40);
        assert_eq!(c.rule, SelectionRule::Snr);
        assert_eq!(c.quota, u32::MAX as usize);
        assert_eq!(c.fwhm_set, 40);
        assert_eq!(c.ranked[0], (39, 39)); // the brightest reference signal
    }
}
