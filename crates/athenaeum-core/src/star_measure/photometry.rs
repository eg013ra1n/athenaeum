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
//!    [`RefusalReason::MostlySaturated`]. From Task 9 (F-4, ruling R-OSP-6) it is rustafits' bounded fit
//!    (`fit_moffat_2d_bounded`: ln β in [`BETA_BOUNDS`], start β [`BETA_START`], the corrected θ column), a point
//!    model; the unflagged bright seeds are the frame β's sample, each re-fitted on the same pixels from the same
//!    starts with the pixel-integrated model ([`integrated_beta`], the owner's E5; [`super::beta`]).
//! 3. **The flag**: the verdict fit's amplitude above the range. The plateau test only sets the level.
//!
//! 4. **The cross-plane rule** (owner ruling 2026-10-09): on an image of more than one plane, a star flagged on any
//!    plane is flagged on every plane at that position (seeds within 1 px; [`super::cross_plane_flags`]), because the
//!    debayer mixes the channels.
//!
//! The order on the measure path is seeds → field width → verdicts → (every plane's verdicts, then the cross-plane
//! rule) → β sample → fits at β ([`super::prepare_plane`], [`super::finish_plane`]); every star is fitted at β and
//! carries its flag, and a flagged one (own or cross) is fitted without its pixels ≥ its level (Q6 as revised
//! 2026-10-09: an unflagged star is fitted with no exclusion).

use astroimage::analysis::fitting::{fit_moffat_2d_bounded, PixelSample};

use super::beta::{BETA_BOUNDS, BETA_START};
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
    /// No saturation test (F-4, Q7): every threshold, level and range infinite, so no seed has a plateau, no pixel is
    /// excluded and nothing is flagged. [`saturation_verdict`] on it is the bare bounded free-β fit from the verdict
    /// fit's own starts — the frame β's own sample for a caller that ran no verdict ([`super::beta::frame_beta`]).
    pub const UNTESTED: SaturationPlane = SaturationPlane {
        bg: 0.0,
        threshold: f64::INFINITY,
        norm: f64::INFINITY,
        range: f64::INFINITY,
        colour: false,
    };

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
    plateau_test(region, seed_x, seed_y, plane).map(|(level, _)| level)
}

/// [`plateau_level`] with pixel0's value beside the level: `Some((level, pixel0))` when the test passes. R2 reads
/// pixel0 against the clip band.
pub(crate) fn plateau_test(
    region: &[PixelSample],
    seed_x: f64,
    seed_y: f64,
    plane: &SaturationPlane,
) -> Option<(f64, f64)> {
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
    Some((pixel0.min(plane.norm) - PLATEAU_RANGE * plane.range, pixel0))
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
    /// Its free β, inside [`BETA_BOUNDS`]: the point model's, reported (the probe's verdict line). The frame β's
    /// sample does not read it: a sample star is re-fitted pixel-integrated ([`integrated_beta`], E5), because the
    /// point model reads an undersampled star's β high.
    pub beta: f64,
    /// Its FWHM, `√(fwhm_x·fwhm_y)` (the payoff reads it, C-d).
    pub fwhm: f64,
    /// The star's own flag or a cross flag: `by_amplitude || by_plateau_clip || cross`.
    pub saturated: bool,
    /// Clause 1: the verdict fit succeeded and `amplitude > range`.
    pub by_amplitude: bool,
    /// Clause 2 (R2, owner 2026-10-09): the plateau test passed and `pixel0 − bg ≥ 0.9·range` (pixel0 within the
    /// test's own 0.1·range band of the clip), whatever the fitted amplitude. It needs no fit: a verdict whose fit
    /// failed (`fit = false`) with a passed plateau at the clip is flagged.
    pub by_plateau_clip: bool,
    /// `false` = the verdict fit failed (not n ≤ p): `amplitude`, `beta`, `fwhm` NaN, `saturated` false unless a cross
    /// flag. A β at a bound is a successful fit.
    pub fit: bool,
    /// The verdict fit's LM iterations (F-4: [`super::FitDiagnostics::iterations`]); 0 when the fitter returned
    /// nothing.
    pub iterations: u16,
    /// The flag came from another plane (the cross-plane rule, owner ruling 2026-10-09, ruling R-OSP-5): on an image of
    /// more than one plane a star flagged on any plane is flagged on every plane at that position
    /// ([`super::cross_plane_flags`]); `saturated` is then true and the rest of the verdict stays this plane's own.
    /// Always `false` on a single plane.
    pub cross: bool,
}

/// Spec §2.1 per seed (PA-23): the plateau test, the level, then the free-β verdict fit (`fit_moffat_2d_bounded` in
/// [`BETA_BOUNDS`] from β [`BETA_START`], Task 9; `fit_moffat_2d` through Task 8) on the region's pixels below the
/// level, started at `b0`, the seed and `f_start` (PA-12), with `3 × params.max_iter` when any pixel is excluded.
/// `Err(MostlySaturated)` when ≤ 8 pixels remain (the fit's 8 parameters).
///
/// `region`, `seed_x`, `seed_y` are in stamp coordinates, as [`plateau_level`] reads them. The fit sees the module's
/// conditioned problem (F-2): the values `(v − b0)/a0` with `a0` = the region's maximum − `b0`, started at B = 0,
/// A = 1, round at `σ = f_start/2.3548`, θ = 0; the amplitude is mapped back to working units. A fit that returns
/// nothing (α < 0.3, A ≤ 0, a non-finite parameter, or a solve that never left its start — fix round A3), or a
/// non-finite or non-positive amplitude or width, is `fit = false` (no flag, no refusal; Q4); a β at a bound is a fit.
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
    let test = plateau_test(region, seed_x, seed_y, plane);
    let plateau = test.map(|(level, _)| level);
    let level = plateau.unwrap_or(plane.norm);
    // R2: pixel0 within the plateau test's own band (0.1·range) of the clip
    let by_plateau_clip =
        test.is_some_and(|(_, pixel0)| pixel0 - plane.bg >= (1.0 - PLATEAU_RANGE) * plane.range);
    let kept = &mut scratch.verdict;
    let Some((a0, excluded)) = verdict_pixels(region, level, b0, kept) else {
        return Err(RefusalReason::MostlySaturated);
    };
    let max_iter = verdict_max_iter(excluded, params);
    let sigma = f_start / GAUSSIAN_FWHM_PER_SIGMA;
    let mut verdict = SaturationVerdict {
        plateau: plateau.is_some(),
        level,
        excluded,
        amplitude: f64::NAN,
        beta: f64::NAN,
        fwhm: f64::NAN,
        saturated: by_plateau_clip,
        by_amplitude: false,
        by_plateau_clip,
        fit: false,
        cross: false,
        iterations: 0,
    };
    let Some(m) = fit_moffat_2d_bounded(
        kept,
        0.0,
        1.0,
        seed_x,
        seed_y,
        sigma,
        sigma,
        0.0,
        BETA_START,
        BETA_BOUNDS,
        max_iter,
        params.conv_tol,
        params.max_rejects,
        1,
    ) else {
        return Ok(verdict);
    };
    verdict.iterations = u16::try_from(m.iters).unwrap_or(u16::MAX);
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
    verdict.by_amplitude = amplitude > plane.range;
    verdict.saturated = verdict.by_amplitude || by_plateau_clip;
    verdict.fit = true;
    Ok(verdict)
}

/// The verdict fit's pixels (spec §2.1): the region's pixels below `level`, conditioned `(v − b0)/a0` into `kept`
/// (cleared first), with `a0` = the region's maximum − `b0` (at least 1e-9) — the maximum over every region pixel, the
/// excluded ones included. `Some((a0, excluded))`, or `None` when no more pixels remain than the fit's 8 parameters.
fn verdict_pixels(
    region: &[PixelSample],
    level: f64,
    b0: f64,
    kept: &mut Vec<PixelSample>,
) -> Option<(f64, u32)> {
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
        return None;
    }
    let a0 = (peak - b0).max(1e-9);
    for s in kept.iter_mut() {
        s.value = (s.value - b0) / a0;
    }
    Some((a0, excluded))
}

/// The verdict fit's iteration limit: `params.max_iter`, ×[`EXCLUDED_ITERATION_FACTOR`] when any pixel is excluded.
fn verdict_max_iter(excluded: u32, params: &FitParams) -> usize {
    if excluded > 0 {
        EXCLUDED_ITERATION_FACTOR * params.max_iter
    } else {
        params.max_iter
    }
}

/// One sample star's β for the frame β (F-4 as revised by the owner's ruling E5, 2026-10-09; ruling R-OSP-6): the
/// verdict fit's own problem — the region's pixels below `level` (the verdict's level), conditioned as
/// [`saturation_verdict`] conditions them, started at B = 0, A = 1, the seed, round at `σ = f_start/2.3548`, θ = 0
/// and β [`BETA_START`], in [`BETA_BOUNDS`], with the verdict's iteration budget — fitted with the PIXEL-INTEGRATED
/// bounded free-β Moffat at `k = `[`super::beta::sample_subsample`]`(f_start)` sub-samples per pixel side. The point
/// model reads an undersampled star's β high (its pixels integrate a flatter core); the integrated one does not.
/// `None` when no more pixels remain than the fit's 8 parameters, or when the fit fails the verdict fit's own
/// acceptance (no fit — a solve that never left its start among them, fix round A3; a non-finite or non-positive
/// amplitude or width; a non-finite β): the caller counts the star out of the sample. A β at a bound is a fit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn integrated_beta(
    region: &[PixelSample],
    seed_x: f64,
    seed_y: f64,
    b0: f64,
    f_start: f64,
    level: f64,
    params: &FitParams,
    scratch: &mut FitScratch,
) -> Option<f64> {
    let kept = &mut scratch.verdict;
    let (_, excluded) = verdict_pixels(region, level, b0, kept)?;
    let sigma = f_start / GAUSSIAN_FWHM_PER_SIGMA;
    let m = fit_moffat_2d_bounded(
        kept,
        0.0,
        1.0,
        seed_x,
        seed_y,
        sigma,
        sigma,
        0.0,
        BETA_START,
        BETA_BOUNDS,
        verdict_max_iter(excluded, params),
        params.conv_tol,
        params.max_rejects,
        super::beta::sample_subsample(f_start),
    )?;
    let fwhm = (m.fwhm_x() * m.fwhm_y()).sqrt();
    (m.a.is_finite() && m.a > 0.0 && m.beta.is_finite() && fwhm.is_finite() && fwhm > 0.0)
        .then_some(m.beta)
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

    /// A saturated star as a plane holds it (fix round A3): a round Moffat β 2.5, FWHM 2.2 px, 50 above a 0.04 sky,
    /// centred at (0.3, −0.2) in a 41 × 41 region about pixel0, each pixel the light it collects (15 × 15 sub-samples),
    /// clipped at the plane's full scale 1.0 and stored as `f32`. Its plateau's level (0.904) censors the top (50 pixels).
    fn censored_star_region() -> Vec<PixelSample> {
        let (amp, beta, fwhm) = (50.0, 2.5, 2.2);
        let alpha = fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt());
        let offsets: Vec<f64> = (0..15).map(|i| (i as f64 + 0.5) / 15.0 - 0.5).collect();
        let mut region = Vec::new();
        for y in -20..=20 {
            for x in -20..=20 {
                let (x, y) = (x as f64, y as f64);
                let mut sum = 0.0;
                for &oy in &offsets {
                    for &ox in &offsets {
                        let r2 = (x + ox - 0.3).powi(2) + (y + oy + 0.2).powi(2);
                        sum += (1.0 + r2 / (alpha * alpha)).powf(-beta);
                    }
                }
                let v = (0.04 + amp * sum / 225.0).min(1.0) as f32;
                region.push(PixelSample {
                    x,
                    y,
                    value: v as f64,
                });
            }
        }
        region
    }

    /// Fix round A3 (Task 9): a censored star's bounded fits leave their start. The star's top is censored (its plateau
    /// level excludes 50 pixels) and the fits start under it (B = 0, A = 1 — the clipped maximum, 50× under the truth —
    /// round at F 2.5 px, β 3), where every early step overshoots the fitter's centroid guard. Before, five such steps
    /// ended both fits on their start: the verdict read the start amplitude (the clipped maximum, not above the range,
    /// so the saturated star went unflagged into the β sample) and the sample fit read the start β, exactly 3. Now the
    /// verdict flags the star and the integrated sample fit, on the verdict's own pixels, reads β 2.5 within the
    /// module's 1 %.
    #[test]
    fn a_censored_star_is_flagged_and_its_integrated_fit_recovers_its_beta() {
        let region = censored_star_region();
        let bg = 0.04_f32 as f64;
        let plane = SaturationPlane::new(bg, 0.002, 1.0, 1.0, false);
        let params = FitParams::default();
        let mut s = FitScratch::default();
        let v = saturation_verdict(&region, 0.0, 0.0, bg, 2.5, &plane, &params, &mut s)
            .expect("a verdict");
        println!("verdict: {v:?}");
        assert!(v.plateau && v.excluded == 50, "{v:?}");
        assert!(
            v.fit && v.saturated && v.amplitude > 40.0,
            "the verdict never left its start: {v:?}"
        );
        let beta = integrated_beta(&region, 0.0, 0.0, bg, 2.5, v.level, &params, &mut s)
            .expect("a sample fit");
        println!("integrated sample beta {beta}");
        assert!(
            beta != 3.0_f64.ln().exp(),
            "the sample fit never left its start beta"
        );
        assert!((beta / 2.5 - 1.0).abs() < 0.01, "beta {beta}");
    }
}
