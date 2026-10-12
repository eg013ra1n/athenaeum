//! The star selection for the LN scale (spec 2026-10-04 §3, L-11, L-12,
//! L-14): relative saturation, peak SNR, the brightest by the reference's
//! flux, capped on PAIRS, and the bright-quarter fallback. It runs after
//! pairing and before the flux ratio, on both scale paths, both
//! estimators, both seeds-calibration arms and the local-scale input — all
//! through `scale.rs`'s two `_core` bodies.

use super::aperture::FiniteMask;
use super::background::TileNoise;
use super::scale::MIN_MATCHES;
use super::LnError;
use crate::stacking::integrate::LnStarSelection;
use crate::stacking::psf_signal::StarFit;

/// The reference-frames saturation mask (spec §3 step 1, L-23): a bit per
/// pixel of the LN reference, set where at least one frame the reference
/// integrates exceeds `saturation` × its own plane maximum
/// (`ln::reference::reference_saturation_mask` thresholds the map). The same
/// one-bit storage as [`FiniteMask`] (7.6 MB at 61 Mpx).
#[derive(Debug, Clone, PartialEq)]
pub struct SaturationMask(FiniteMask);

impl SaturationMask {
    /// Bit `k` (row-major) set where `f(k)`.
    pub fn from_fn(width: usize, height: usize, f: impl Fn(usize) -> bool) -> SaturationMask {
        SaturationMask(FiniteMask::from_fn(width, height, f))
    }

    /// No pixel set: the test is inert.
    pub fn empty(width: usize, height: usize) -> SaturationMask {
        Self::from_fn(width, height, |_| false)
    }

    pub fn width(&self) -> usize {
        self.0.width()
    }

    pub fn height(&self) -> usize {
        self.0.height()
    }

    /// `false` outside the plane.
    pub fn get(&self, x: isize, y: isize) -> bool {
        self.0.get(x, y)
    }
}

/// One side of a pair: the plane the scale reads (sanitized for detection),
/// and its background model's noise and maximum.
#[derive(Clone, Copy)]
pub struct SelectionSide<'a> {
    pub plane: &'a [f32],
    pub width: usize,
    pub height: usize,
    pub noise: &'a TileNoise,
    /// L-23: the reference-frames saturation mask of the LN reference, on
    /// the reference side only. `None` on every target side and wherever
    /// no map exists (a test, a probe without `--ref-saturation`).
    pub frames_saturation: Option<&'a SaturationMask>,
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
    /// L-23: pairs that passed both planes' own relative-saturation test
    /// and failed only the reference-frames mask (`ln_ref_frames_saturated`).
    pub ref_frames_saturated: usize,
}

/// The pixels whose centres lie within `max(2, FWHM)` px of the fit's
/// centroid, inside a `width × height` plane, row by row: the one disc both
/// relative-saturation tests read (L-14 on the side's plane, L-23 on the
/// reference-frames mask).
fn disc(f: &StarFit, width: usize, height: usize) -> impl Iterator<Item = (usize, usize)> {
    let r = f.fwhm().max(2.0);
    let (fx, fy) = (f.x, f.y);
    let (x0, x1) = (
        (fx - r).floor().max(0.0) as usize,
        ((fx + r).ceil().max(0.0) as usize).min(width - 1),
    );
    let (y0, y1) = (
        (fy - r).floor().max(0.0) as usize,
        ((fy + r).ceil().max(0.0) as usize).min(height - 1),
    );
    (y0..=y1)
        .flat_map(move |y| (x0..=x1).map(move |x| (x, y)))
        .filter(move |&(x, y)| {
            let (dx, dy) = (x as f64 - fx, y as f64 - fy);
            dx * dx + dy * dy <= r * r
        })
}

/// The largest pixel within `max(2, FWHM)` px of the fit's centroid
/// (pixel centres, inside the plane).
fn local_peak(side: &SelectionSide<'_>, f: &StarFit) -> f64 {
    let mut best = f64::NEG_INFINITY;
    for (x, y) in disc(f, side.width, side.height) {
        let v = side.plane[y * side.width + x];
        if v.is_finite() {
            best = best.max(v as f64);
        }
    }
    best
}

/// L-14: relative saturation against this side's hot-pixel-corrected max.
fn plane_saturated(side: &SelectionSide<'_>, f: &StarFit, sel: &LnStarSelection) -> bool {
    let max = side.noise.plane_max() as f64;
    max.is_finite() && local_peak(side, f) > sel.saturation * max
}

/// L-23: the relative test failed in at least one frame the LN reference
/// integrates — a set bit of the side's mask inside the same disc. Inert
/// without a mask and under a non-finite threshold (`LnStarSelection::OFF`).
fn frames_saturated(side: &SelectionSide<'_>, f: &StarFit, sel: &LnStarSelection) -> bool {
    match side.frames_saturation {
        Some(mask) if sel.saturation.is_finite() => {
            disc(f, side.width, side.height).any(|(x, y)| mask.get(x as isize, y as isize))
        }
        _ => false,
    }
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
    // Step 1: a side is saturated when its own plane's test fails; the
    // reference side also when (L-23) its reference-frames mask does. The
    // count is the pairs only that mask dropped. A target never carries a
    // mask, so none can be counted here.
    debug_assert!(
        target.frames_saturation.is_none(),
        "a target side carries no reference-frames saturation mask"
    );
    let mut ref_frames_saturated = 0usize;
    let mut unsaturated: Vec<(usize, usize)> = pairs
        .iter()
        .copied()
        .filter(|&(r, t)| {
            let (rf, tf) = (&ref_fits[r], &tgt_fits[t]);
            if plane_saturated(reference, rf, sel) || plane_saturated(target, tf, sel) {
                return false;
            }
            if frames_saturated(reference, rf, sel) {
                ref_frames_saturated += 1;
                return false;
            }
            true
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
            ref_frames_saturated,
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
        ref_frames_saturated,
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
            sky_ring: f64::NAN,
            sky_ring_sigma: f64::NAN,
            sky_ring_n: 0,
            snr: f64::NAN,
            hfr: f64::NAN,
            flags: crate::star_measure::StarFlags::default(),
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
            frames_saturation: None,
        }
    }

    /// A 5 × 5 core of `v` centred on the fit (a real core, not a hot pixel:
    /// the plane maximum keeps it).
    fn paint_core(plane: &mut [f32], f: &StarFit, v: f32) {
        let (cx, cy) = (f.x.round() as usize, f.y.round() as usize);
        for y in cy - 2..=cy + 2 {
            for x in cx - 2..=cx + 2 {
                plane[y * W + x] = v;
            }
        }
    }

    /// T1 (L-23): a two-frame reference. Star 0 is clipped at the saturation
    /// level (1.0, its frame's plane maximum) in frame A only and reads 0.3
    /// in frame B, so the integrated plane's peak (0.65) stays under 0.75 ×
    /// the integrated maximum while frame A's ratio is 1.0. Star 1 is clipped
    /// in both frames and on the integrated plane, so the plane's own test
    /// drops it and it is never counted.
    fn two_frame_reference(fits: &[StarFit]) -> (Vec<f32>, SaturationMask) {
        let (mut a, mut b) = (flat(), flat());
        paint_core(&mut a, &fits[0], 1.0);
        paint_core(&mut b, &fits[0], 0.3);
        paint_core(&mut a, &fits[1], 1.0);
        paint_core(&mut b, &fits[1], 1.0);
        let integrated: Vec<f32> = a.iter().zip(&b).map(|(x, y)| 0.5 * (x + y)).collect();
        let mut map = vec![f32::NAN; W * H];
        let ma = crate::stacking::ln::reference::accumulate_reference_saturation(
            &mut map, &a, W, H, None,
        );
        let mb = crate::stacking::ln::reference::accumulate_reference_saturation(
            &mut map, &b, W, H, None,
        );
        assert_eq!((ma, mb), (1.0, 1.0), "each frame's own plane maximum");
        let (cx, cy) = (fits[0].x.round() as usize, fits[0].y.round() as usize);
        assert_eq!(map[cy * W + cx], 1.0, "frame A's ratio at star 0");
        let mask = crate::stacking::ln::reference::reference_saturation_mask(&map, W, H, 0.75);
        (integrated, mask)
    }

    #[test]
    fn a_star_saturated_in_one_integrated_frame_is_left_out_and_counted() {
        let (rf, tf) = (clean(27), clean(27));
        let (integrated, mask) = two_frame_reference(&rf);
        let ref_max = crate::stacking::ln::TileTable::measure(
            &integrated,
            W,
            H,
            &crate::stacking::ln::CLEANING,
            None,
        )
        .plane_max();
        assert_eq!(ref_max, 1.0);
        let (cx, cy) = (rf[0].x.round() as usize, rf[0].y.round() as usize);
        assert!(
            (integrated[cy * W + cx] as f64) < 0.75 * ref_max as f64,
            "the integrated plane's own test keeps star 0"
        );
        let ref_noise = TileNoise::uniform(0.001, ref_max);
        let tgt_noise = TileNoise::uniform(0.001, 1.0);
        let target = flat();
        let without = rank_candidates(
            &diagonal(27),
            &rf,
            &tf,
            &side(&integrated, &ref_noise),
            &side(&target, &tgt_noise),
            &LnStarSelection::default(),
        )
        .unwrap();
        assert!(without.ranked.contains(&(0, 0)), "selected without the map");
        assert!(!without.ranked.contains(&(1, 1)));
        assert_eq!(without.ranked.len(), 26);
        assert_eq!(without.ref_frames_saturated, 0);

        let with = rank_candidates(
            &diagonal(27),
            &rf,
            &tf,
            &SelectionSide {
                frames_saturation: Some(&mask),
                ..side(&integrated, &ref_noise)
            },
            &side(&target, &tgt_noise),
            &LnStarSelection::default(),
        )
        .unwrap();
        assert!(!with.ranked.contains(&(0, 0)), "dropped with the map");
        assert!(!with.ranked.contains(&(1, 1)));
        assert_eq!(with.ranked.len(), 25);
        assert_eq!(
            with.ref_frames_saturated, 1,
            "only star 0 failed the map alone"
        );
        assert_eq!(with.rule, SelectionRule::Snr);
    }

    /// T3: `LnStarSelection::OFF` means off — even a mask with every bit set
    /// drops nothing and counts nothing.
    #[test]
    fn off_ignores_the_reference_frames_mask() {
        let noise = TileNoise::uniform(0.001, 1.0);
        let (rf, tf) = (clean(27), clean(27));
        let plane = flat();
        let full = SaturationMask::from_fn(W, H, |_| true);
        let c = rank_candidates(
            &diagonal(27),
            &rf,
            &tf,
            &SelectionSide {
                frames_saturation: Some(&full),
                ..side(&plane, &noise)
            },
            &side(&plane, &noise),
            &LnStarSelection::OFF,
        )
        .unwrap();
        assert_eq!(c.ranked.len(), 27);
        assert_eq!(c.ref_frames_saturated, 0);
        // The same mask under the default selection drops every pair.
        let r = rank_candidates(
            &diagonal(27),
            &rf,
            &tf,
            &SelectionSide {
                frames_saturation: Some(&full),
                ..side(&plane, &noise)
            },
            &side(&plane, &noise),
            &LnStarSelection::default(),
        );
        assert!(
            matches!(r, Err(LnError::TooFewMatches { matches: 0 })),
            "{r:?}"
        );
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
