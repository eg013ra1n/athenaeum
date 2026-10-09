//! Photometry (spec 2026-10-07 §2.1 `photometry`): the star's own sky annulus σ (PA-11 radii; owned here, I-2, and
//! reused by Task 11's `star_snr`) and the per-seed saturation test (F-3, spec §2.1 as revised 2026-10-09, PA-23,
//! ruling R-OSP-5).
//!
//! The saturation test is an established stacking program's method, adopted as is (owner ruling 2026-10-09); its
//! constants are that method's, not measured here. Per plane ([`SaturationPlane`], working units): `bg` = the module's
//! sky median, `σ` = the noise model's σ, `threshold = bg + 5σ`, `norm` = the plane's full scale and
//! `range = min(plane max, norm) − bg`. Per seed, on the seed's own region (F-1):
//! 1. **The plateau test at the seed pixel** ([`plateau_level`]): of pixel0's 8 neighbours, those at or above the
//!    threshold must number at least 3 (mono) or all 8 (a debayered colour plane), with `meanhigh − bg ≥ 0.7·range`
//!    and `pixel0 − minhigh ≤ 0.1·range`. The level is then `min(pixel0, norm) − 0.1·range`, else `norm`.
//! 2. **The verdict fit** ([`saturation_verdict`]): a free-β Moffat on the region's pixels below the level, the
//!    iteration limit ×3 when any pixel is excluded; no more pixels than the fit's 8 parameters refuses the seed
//!    [`RefusalReason::MostlySaturated`].
//! 3. **The flag**: the verdict fit's amplitude above the range. The plateau test only sets the level.
//!
//! The order on the measure path is seeds → field width → verdicts → β sample → fits at β
//! (`fit::fit_stars_diagnosed`); every star is fitted at β with its verdict's exclusion and carries its flag.

use astroimage::analysis::fitting::{fit_moffat_2d, PixelSample};

use super::diagnostics::RefusalReason;
use super::fit::{FitParams, FitScratch};
use crate::integration::stats::{median_in_place, MAD_TO_SIGMA};

/// PA-11: the annulus's inner radius is `max(3·FWHM, 6)` px …
pub const ANNULUS_INNER_PER_FWHM: f64 = 3.0;
pub const ANNULUS_INNER_MIN_PX: f64 = 6.0;
/// … and its width `max(FWHM, 3)` px.
pub const ANNULUS_WIDTH_MIN_PX: f64 = 3.0;
/// [`annulus_sigma`] reads nothing from fewer present pixels than this.
pub const ANNULUS_MIN_PIXELS: usize = 8;

/// The saturation test's detection threshold, `bg + THRESHOLD_SIGMA·σ` (spec §2.1).
pub const THRESHOLD_SIGMA: f64 = 5.0;
/// A plateau's neighbours stand at least this fraction of the range above `bg` on average (`meanhigh`).
pub const PLATEAU_MIN_LEVEL: f64 = 0.7;
/// A plateau's lowest high neighbour lies within this fraction of the range below pixel0; the level sits the same
/// fraction below pixel0.
pub const PLATEAU_RANGE: f64 = 0.1;
/// High neighbours a mono plane's plateau needs (of 8).
pub const PLATEAU_MONO_NEIGHBOURS: usize = 3;
/// High neighbours a colour plane's plateau needs: all of them.
pub const PLATEAU_COLOUR_NEIGHBOURS: usize = 8;
/// The verdict fit's iteration limit is this multiple of `FitParams::max_iter` when any pixel is excluded.
pub const EXCLUDED_ITERATION_FACTOR: usize = 3;
/// The verdict fit's free parameters (B, A, x0, y0, α_x, α_y, θ, β): no more unexcluded pixels than this refuses the
/// seed `MostlySaturated` (n ≤ p).
pub const VERDICT_FIT_PARAMS: usize = 8;

/// `2√(2 ln 2)`: a Gaussian's FWHM per σ (the verdict fit's start σ from `F_start`).
const GAUSSIAN_FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949_3;

/// The star's own sky annulus (PA-11 radii: inner max(3·FWHM, 6), width max(FWHM, 3)): σ = 1.4826·MAD of the
/// finite, unmasked pixels present, and their count. Non-finite, masked and off-image pixels are absent (Review
/// Focus 1 and 5). `None` under 8 present pixels. Owned here (I-2) and reused by `star_snr` (Task 11).
///
/// A pixel belongs to the ring when its centre lies at a distance `d` from `(x, y)` with `inner ≤ d < inner + width`.
pub fn annulus_sigma(
    plane: &[f32],
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    fwhm: f64,
    mask: Option<&[bool]>,
) -> Option<(f64 /* sigma_sky */, usize /* n_sky */)> {
    let mut buf: Vec<f32> = Vec::new();
    if !(x.is_finite() && y.is_finite() && fwhm.is_finite()) || w == 0 || h == 0 {
        return None;
    }
    let inner = (ANNULUS_INNER_PER_FWHM * fwhm).max(ANNULUS_INNER_MIN_PX);
    let outer = inner + fwhm.max(ANNULUS_WIDTH_MIN_PX);
    let (in2, out2) = (inner * inner, outer * outer);
    let x0 = ((x - outer).floor() as i64).max(0);
    let x1 = ((x + outer).ceil() as i64).min(w as i64 - 1);
    let y0 = ((y - outer).floor() as i64).max(0);
    let y1 = ((y + outer).ceil() as i64).min(h as i64 - 1);
    for py in y0..=y1 {
        let dy = py as f64 - y;
        let row = py as usize * w;
        for px in x0..=x1 {
            let dx = px as f64 - x;
            let d2 = dx * dx + dy * dy;
            if d2 < in2 || d2 >= out2 {
                continue;
            }
            let i = row + px as usize;
            let v = plane[i];
            if v.is_finite() && mask.map_or(true, |m| m[i]) {
                buf.push(v);
            }
        }
    }
    let n = buf.len();
    if n < ANNULUS_MIN_PIXELS {
        return None;
    }
    let median = median_in_place(&mut buf);
    for v in buf.iter_mut() {
        *v = (*v - median).abs();
    }
    let mad = median_in_place(&mut buf);
    Some(((MAD_TO_SIGMA * mad) as f64, n))
}

/// The plane-level inputs of the saturation test (spec §2.1, PA-23), once per plane, in working units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaturationPlane {
    /// The sky median (the module's sampled, clipped `SkyLevel::median`, PA-21), in working units.
    pub bg: f64,
    /// `bg + 5·σ`, σ the noise model's.
    pub threshold: f64,
    /// The plane's full scale: `ADU_SCALE` for a calibrated plane, the stored integer type's for raw ADU.
    pub norm: f64,
    /// `min(plane max, norm) − bg`.
    pub range: f64,
    /// A debayered colour plane: its plateau needs all 8 neighbours.
    pub colour: bool,
}

impl SaturationPlane {
    /// `threshold = bg + 5·sigma`, `range = min(plane_max, norm) − bg`; `plane_max` over the plane's finite,
    /// unmasked pixels.
    pub fn new(bg: f64, sigma: f64, plane_max: f64, norm: f64, colour: bool) -> Self {
        SaturationPlane {
            bg,
            threshold: bg + THRESHOLD_SIGMA * sigma,
            norm,
            range: plane_max.min(norm) - bg,
            colour,
        }
    }
}

/// The plateau test at the seed pixel: `region` is the unnormalised region in stamp coordinates (`x − cx`, `y − cy`,
/// plane values, as `fit_one` gathers it), so pixel0 is (0, 0) and `level` compares plane values; pixel0 = the
/// region's pixel at the rounded seed centre, its 8 neighbours from the region (an absent one is not above the
/// threshold); the neighbours ≥ `threshold` must number ≥ 3 (mono) or 8 (colour), with `meanhigh − bg ≥ 0.7·range`
/// and `pixel0 − minhigh ≤ 0.1·range`. `Some(level)` with `level = min(pixel0, norm) − 0.1·range` when it passes,
/// else `None` (the level is then `norm`).
///
/// The seed centre is rounded half up, `⌊s + 0.5⌋`: in stamp coordinates that is always (0, 0) for a seed whose
/// `cx = round(x)` (`fit_one`'s own pixel, rounded half away from zero on a positive image coordinate), including a
/// seed exactly between two pixels. A region without pixel0 has no plateau.
pub(crate) fn plateau_level(
    region: &[PixelSample],
    seed_x: f64,
    seed_y: f64,
    plane: &SaturationPlane,
) -> Option<f64> {
    let (px0, py0) = ((seed_x + 0.5).floor(), (seed_y + 0.5).floor());
    let mut pixel0: Option<f64> = None;
    let (mut n_high, mut sum_high, mut min_high) = (0usize, 0.0f64, f64::INFINITY);
    for s in region {
        let (dx, dy) = (s.x - px0, s.y - py0);
        if dx.abs() > 1.0 || dy.abs() > 1.0 {
            continue;
        }
        if dx == 0.0 && dy == 0.0 {
            pixel0 = Some(s.value);
        } else if s.value >= plane.threshold {
            n_high += 1;
            sum_high += s.value;
            min_high = min_high.min(s.value);
        }
    }
    let pixel0 = pixel0?;
    let needed = if plane.colour {
        PLATEAU_COLOUR_NEIGHBOURS
    } else {
        PLATEAU_MONO_NEIGHBOURS
    };
    if n_high < needed {
        return None;
    }
    let mean_high = sum_high / n_high as f64;
    if mean_high - plane.bg < PLATEAU_MIN_LEVEL * plane.range
        || pixel0 - min_high > PLATEAU_RANGE * plane.range
    {
        return None;
    }
    Some(pixel0.min(plane.norm) - PLATEAU_RANGE * plane.range)
}

/// One seed's saturation verdict (spec §2.1, PA-23). `saturated` is the flag the fit copies; `level` is the exclusion
/// the fit at β applies (Q6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaturationVerdict {
    /// The plateau test passed.
    pub plateau: bool,
    /// Its level, else norm (working units).
    pub level: f64,
    /// The region's pixels ≥ level, left out of the verdict fit.
    pub excluded: u32,
    /// The verdict fit's A (working units).
    pub amplitude: f64,
    /// Its free β (the β sample's fit, C-b).
    pub beta: f64,
    /// Its FWHM, `√(fwhm_x·fwhm_y)` (the payoff reads it, C-d).
    pub fwhm: f64,
    /// `amplitude > range`.
    pub saturated: bool,
    /// `false` = the verdict fit failed (not n ≤ p): `amplitude`, `beta`, `fwhm` NaN, `saturated` false.
    pub fit: bool,
}

/// Spec §2.1 per seed (PA-23): the plateau test, the level, then the free-β verdict fit (`fit_moffat_2d` at this
/// task, `fit_moffat_2d_bounded` with `BETA_BOUNDS` from Task 9) on the region's pixels below the level, started at
/// `b0`, the seed and `f_start` (PA-12), with `3 × params.max_iter` when any pixel is excluded.
/// `Err(MostlySaturated)` when ≤ 8 pixels remain (the fit's 8 parameters).
///
/// `region`, `seed_x`, `seed_y` are in stamp coordinates, as [`plateau_level`] reads them. The fit sees the module's
/// conditioned problem (F-2): the values `(v − b0)/a0` with `a0` = the region's maximum − `b0`, started at B = 0,
/// A = 1, round at `σ = f_start/2.3548`, θ = 0; the amplitude is mapped back to working units. A fit that returns
/// nothing, or a non-finite or non-positive amplitude or width, is `fit = false` (no flag, no refusal; Q4).
#[allow(clippy::too_many_arguments)]
pub(crate) fn saturation_verdict(
    region: &[PixelSample],
    seed_x: f64,
    seed_y: f64,
    b0: f64,
    f_start: f64,
    plane: &SaturationPlane,
    params: &FitParams,
    scratch: &mut FitScratch,
) -> Result<SaturationVerdict, RefusalReason> {
    let plateau = plateau_level(region, seed_x, seed_y, plane);
    let level = plateau.unwrap_or(plane.norm);
    let kept = &mut scratch.verdict;
    kept.clear();
    let mut peak = f64::NEG_INFINITY;
    let mut excluded = 0u32;
    for s in region {
        peak = peak.max(s.value);
        if s.value >= level {
            excluded += 1;
        } else {
            kept.push(PixelSample {
                x: s.x,
                y: s.y,
                value: s.value,
            });
        }
    }
    if kept.len() <= VERDICT_FIT_PARAMS {
        return Err(RefusalReason::MostlySaturated);
    }
    let a0 = (peak - b0).max(1e-9);
    for s in kept.iter_mut() {
        s.value = (s.value - b0) / a0;
    }
    let max_iter = if excluded > 0 {
        EXCLUDED_ITERATION_FACTOR * params.max_iter
    } else {
        params.max_iter
    };
    let sigma = f_start / GAUSSIAN_FWHM_PER_SIGMA;
    let mut verdict = SaturationVerdict {
        plateau: plateau.is_some(),
        level,
        excluded,
        amplitude: f64::NAN,
        beta: f64::NAN,
        fwhm: f64::NAN,
        saturated: false,
        fit: false,
    };
    let Some(m) = fit_moffat_2d(
        kept,
        0.0,
        1.0,
        seed_x,
        seed_y,
        sigma,
        sigma,
        0.0,
        max_iter,
        params.conv_tol,
        params.max_rejects,
    ) else {
        return Ok(verdict);
    };
    let amplitude = m.a * a0;
    let fwhm = (m.fwhm_x() * m.fwhm_y()).sqrt();
    if !(amplitude.is_finite()
        && amplitude > 0.0
        && m.beta.is_finite()
        && fwhm.is_finite()
        && fwhm > 0.0)
    {
        return Ok(verdict);
    }
    verdict.amplitude = amplitude;
    verdict.beta = m.beta;
    verdict.fwhm = fwhm;
    verdict.saturated = amplitude > plane.range;
    verdict.fit = true;
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::add_noise;

    /// A hand-built region, stamp-centred as `fit_one` hands it (pixel0 at (0, 0)): the 3x3 `core` (row-major, centre at index 4, `None` = absent), then `ring_n`
    /// of the 16 pixels of the r = 2 ring at `ring`.
    fn hand_region(core: [Option<f32>; 9], ring: f32, ring_n: usize) -> Vec<PixelSample> {
        let mut px: Vec<PixelSample> = core
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                v.map(|v| PixelSample {
                    x: -1.0 + (i % 3) as f64,
                    y: -1.0 + (i / 3) as f64,
                    value: v as f64,
                })
            })
            .collect();
        let ring_xy = (-2i64..=2)
            .flat_map(|dy| (-2i64..=2).map(move |dx| (dx, dy)))
            .filter(|&(dx, dy)| dx.abs() == 2 || dy.abs() == 2);
        px.extend(ring_xy.take(ring_n).map(|(dx, dy)| PixelSample {
            x: dx as f64,
            y: dy as f64,
            value: ring as f64,
        }));
        px
    }

    #[test]
    fn annulus_sigma_is_the_mad_of_the_present_pixels() {
        // PA-11 at FWHM 2.6: inner max(7.8, 6) = 7.8, width max(2.6, 3) = 3 — the ring 7.8..10.8 (180 pixels about an
        // interior pixel centre, counted). A star 4 px from the border: its ring is cut by the border to 119 in-image
        // pixels (counted), and σ comes from those only; the NaN corner holds the whole in-image ring of a star at (60, 60)
        let (w, h) = (64usize, 64usize);
        let mut data = vec![0.05f32; w * h];
        add_noise(&mut data, 0.004, 3);
        for y in 0..h {
            for x in 0..w {
                if x > 40 && y > 40 {
                    data[y * w + x] = f32::NAN;
                }
            }
        }
        let (sigma, n) = annulus_sigma(&data, w, h, 4.0, 32.0, 2.6, None).unwrap();
        assert!(n > 0 && n < 150, "{n}");
        assert!((sigma / 0.004 - 1.0).abs() < 0.35, "{sigma}");
        assert!(
            annulus_sigma(&data, w, h, 60.0, 60.0, 2.6, None).is_none(),
            "the NaN corner: nothing present"
        );
        let all = vec![false; w * h];
        assert!(
            annulus_sigma(&data, w, h, 32.0, 32.0, 2.6, Some(&all)).is_none(),
            "a fully masked annulus"
        );
    }

    #[test]
    fn the_range_is_the_plane_max_capped_at_full_scale_less_the_median() {
        let p = SaturationPlane::new(0.05, 0.002, 1.24, 1.0, false);
        assert!(
            (p.range - 0.95).abs() < 1e-12 && (p.threshold - 0.06).abs() < 1e-12,
            "{p:?}"
        );
        let p = SaturationPlane::new(0.05, 0.002, 0.6, 1.0, true);
        assert!((p.range - 0.55).abs() < 1e-12 && p.colour, "{p:?}");
    }

    #[test]
    fn the_plateau_test_needs_three_neighbours_on_mono_all_eight_on_colour_and_a_high_flat_top() {
        // range 0.9, threshold 0.06; `c` at the clip, `k` at the sky (under the threshold); the level is 0.95 − 0.09
        let (c, k) = (Some(0.95f32), Some(0.05f32));
        let mono = SaturationPlane::new(0.05, 0.002, 0.95, 1.0, false);
        let colour = SaturationPlane {
            colour: true,
            ..mono
        };
        let level = |core: [Option<f32>; 9], p: &SaturationPlane| {
            plateau_level(&hand_region(core, 0.3, 16), 0.0, 0.0, p)
        };
        assert!(
            level([c, c, c, k, c, k, k, k, k], &mono).is_some(),
            "mono: 3 of 8 above the threshold"
        );
        assert!(
            level([c, c, k, k, c, k, k, k, k], &mono).is_none(),
            "mono: 2 of 8"
        );
        let l = level([c; 9], &colour).expect("colour: 8 of 8");
        assert!((l - 0.86).abs() < 1e-6, "{l}");
        assert!(
            level([c, c, c, c, c, c, c, c, None], &colour).is_none(),
            "colour: 7 of 8, one absent"
        );
        assert!(
            level([c, c, c, c, c, Some(0.84), c, c, c], &mono).is_none(),
            "pixel0 − minhigh 0.11 > 0.1·range"
        );
        let (h, p0) = (Some(0.5f32), Some(0.55f32));
        assert!(
            level([h, h, h, h, p0, h, h, h, h], &mono).is_none(),
            "a flat top at half the range: meanhigh too low"
        );
    }

    #[test]
    fn no_more_unexcluded_pixels_than_parameters_refuses_the_seed_mostly_saturated() {
        // the 3x3 is a plateau at the clip, so the level excludes all nine and the ring's k pixels are left for the
        // 8-parameter fit: 8 → refused (n ≤ p, Q5), 9 → the fit runs
        let plane = SaturationPlane::new(0.05, 0.002, 0.95, 1.0, false);
        let mut s = FitScratch::default();
        let r = saturation_verdict(
            &hand_region([Some(0.95); 9], 0.3, 8),
            0.0,
            0.0,
            0.05,
            2.6,
            &plane,
            &FitParams::default(),
            &mut s,
        );
        assert_eq!(r, Err(RefusalReason::MostlySaturated));
        let r = saturation_verdict(
            &hand_region([Some(0.95); 9], 0.3, 9),
            0.0,
            0.0,
            0.05,
            2.6,
            &plane,
            &FitParams::default(),
            &mut s,
        );
        assert_ne!(
            r,
            Err(RefusalReason::MostlySaturated),
            "nine pixels left: the fit runs"
        );
    }
}
