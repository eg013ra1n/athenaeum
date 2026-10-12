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
//! 3. **The flag**: the verdict fit's amplitude above the range, or (plateau-clip clause, owner 2026-10-09) a passed
//!    plateau test with `pixel0 − bg ≥ 0.9·range`, whatever the fitted amplitude and even when the fit failed.
//!
//! 4. **The cross-plane rule** (owner ruling 2026-10-09): on an image of more than one plane, a star flagged on any
//!    plane is flagged on every plane at that position (seeds within 1 px; [`super::cross_plane_flags`]), because the
//!    debayer mixes the channels.
//!
//! The order on the measure path is seeds → field width → verdicts → (every plane's verdicts, then the cross-plane
//! rule) → β sample → fits at β ([`super::prepare_plane`], [`super::finish_plane`]); every star is fitted at β and
//! carries its flag, and a flagged one (own or cross) is fitted without its pixels ≥ its level (Q6 as revised
//! 2026-10-09: an unflagged star is fitted with no exclusion).
//!
//! The star flux (F-5, Task 10, ruling R-OSP-7): a fit's `signal` is [`model_flux_in_fwtm`] (the fitted model's
//! analytic flux inside its own FWTM ellipse) times the plane's [`aperture_correction`] `c`, the median ratio of the
//! [`growth_curve`] flux at `5F` to the model flux over the 64 brightest isolated unsaturated fits — so the flux no
//! longer depends on how much of a profile its FWTM ellipse holds. The growth curve's sky is the plain mean of the far
//! ring `[10F, 12F]` outside every seed's `3F` disc, and a candidate whose fitted amplitude tops the plane's range is
//! screened out (F-3's amplitude clause, on every path; owner ruling R6, 2026-10-11). Round 4: off the measure path
//! the screen's full scale is the caller's ([`super::FitStarsOptions::full_scale`]), and a caller's footprint
//! ([`super::FitStarsOptions::footprint`], LN's `allowed` region) must hold a candidate's whole `12F` disc.

use std::collections::HashMap;

use astroimage::analysis::fitting::{fit_moffat_2d_bounded, PixelSample};

use super::beta::{BETA_BOUNDS, BETA_START};
use super::diagnostics::RefusalReason;
use super::fit::{FitParams, FitScratch, Seed, StarFit};
use crate::integration::stats::{self, median_in_place, MAD_TO_SIGMA};

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
    /// fit's own starts (a caller that ran no verdict takes its frame β from the pixel-integrated re-fit instead,
    /// [`super::beta::frame_beta`] via `integrated_beta_on_region`).
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
#[cfg(test)]
pub(crate) fn plateau_level(
    region: &[PixelSample],
    seed_x: f64,
    seed_y: f64,
    plane: &SaturationPlane,
) -> Option<f64> {
    plateau_test(region, seed_x, seed_y, plane).map(|(level, _)| level)
}

/// The plateau level with pixel0's value beside it: `Some((level, pixel0))` when the test passes. The plateau-clip clause readss
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
    /// Clause 2 (plateau-clip, owner 2026-10-09): the plateau test passed and `pixel0 − bg ≥ 0.9·range` (pixel0 within the
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
/// non-finite or non-positive amplitude or width, is `fit = false` (no refusal; Q4; no amplitude flag, but the plateau-clip clause still applies); a β at a bound is a fit.
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
    // plateau-clip clause: pixel0 within the plateau test's own band (0.1·range) of the clip
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

/// F-5 (Task 10, ruling R-OSP-7): the growth-curve aperture's radius per frame FWHM `F` — a star's growth-curve flux
/// is the sum over the pixels whose centres lie within `5F` of its fitted centre.
pub const GROWTH_RADIUS_PER_FWHM: f64 = 5.0;
/// The growth-curve aperture's local sky (owner ruling R6, 2026-10-11): the plain mean of the present pixels at
/// `10F ≤ d ≤ 12F` from the same centre that lie outside every seed's [`GROWTH_RIM_SEED_MASK_PER_FWHM`] disc. Far
/// enough out that a star's own wing barely reaches it; the mask keeps the neighbours' cores out of it.
pub const GROWTH_RIM_INNER_PER_FWHM: f64 = 10.0;
pub const GROWTH_RIM_OUTER_PER_FWHM: f64 = 12.0;
/// The radius, per frame FWHM, of the disc about every seed of the call (the star's own included, by its seed
/// position) whose pixels the growth-curve rim leaves out (R6).
pub const GROWTH_RIM_SEED_MASK_PER_FWHM: f64 = 3.0;
/// A fit whose centre lies within this many FIELD widths (`FitOutcome::field_width`) of another seed handed to the
/// fitter, accepted or not, is not isolated and does not enter the aperture correction.
pub const CORRECTION_ISOLATION_PER_FIELD_FWHM: f64 = 6.0;
/// The aperture correction is the median over at most this many qualifying fits, the brightest by model flux.
pub const CORRECTION_STARS: usize = 64;
/// Fewer ratios than this: no correction (`c = 1`, `aperture_correction: None`, one `warn!` per plane).
pub const CORRECTION_MIN_RATIOS: usize = 5;

/// The fitted Moffat's flux inside its own FWTM ellipse (F-5): the analytic integral
/// `A·π·αx·αy/(β−1)·(1 − 10^{(1−β)/β})` of the model, no pixel read — the same fraction `1 − 10^{(1−β)/β}` of the
/// model's total at every star of one β, which [`aperture_correction`] then scales to the star's measured light. The
/// α are the fitted FWHMs' (`α = FWHM / (2√(2^{1/β} − 1))`); at β = 1 the limit `ln 10` stands for
/// `(1 − 10^{(1−β)/β})/(β − 1)`.
pub fn model_flux_in_fwtm(f: &StarFit) -> f64 {
    let per_alpha = 2.0 * (2f64.powf(1.0 / f.beta) - 1.0).sqrt();
    let (ax, ay) = (f.fwhm_x / per_alpha, f.fwhm_y / per_alpha);
    let enclosed = if (f.beta - 1.0).abs() < 1e-12 {
        std::f64::consts::LN_10
    } else {
        (1.0 - 10f64.powf((1.0 - f.beta) / f.beta)) / (f.beta - 1.0)
    };
    f.amplitude * std::f64::consts::PI * ax * ay * enclosed
}

/// The plane-wide union of the discs of radius `r` about every seed of one fit call (F-5, owner ruling R6): a pixel is
/// covered when its centre lies within `r` of a seed's own position (`d² ≤ r²`; seed coordinates, not fitted centres).
/// One bit per pixel, built once per call; [`growth_curve`]'s rim leaves the covered pixels out.
pub(crate) struct SeedMask {
    bits: Vec<u64>,
}

impl SeedMask {
    pub(crate) fn new(w: usize, h: usize, seeds: &[Seed], r: f64) -> SeedMask {
        let mut bits = vec![0u64; (w * h).div_ceil(64)];
        if r.is_finite() && r >= 0.0 && w > 0 && h > 0 {
            let (r2, ri) = (r * r, r.ceil() as i64);
            let (xmax, ymax) = (w as i64 - 1, h as i64 - 1);
            for s in seeds.iter().filter(|s| s.x.is_finite() && s.y.is_finite()) {
                let x0 = (s.x.floor() as i64).saturating_sub(ri).max(0);
                let x1 = (s.x.ceil() as i64).saturating_add(ri).min(xmax);
                let y0 = (s.y.floor() as i64).saturating_sub(ri).max(0);
                let y1 = (s.y.ceil() as i64).saturating_add(ri).min(ymax);
                for py in y0..=y1 {
                    let dy2 = (py as f64 - s.y).powi(2);
                    for px in x0..=x1 {
                        if (px as f64 - s.x).powi(2) + dy2 <= r2 {
                            let i = py as usize * w + px as usize;
                            bits[i / 64] |= 1u64 << (i % 64);
                        }
                    }
                }
            }
        }
        SeedMask { bits }
    }

    /// Pixel `i` (row-major) lies within `r` of a seed.
    fn covers(&self, i: usize) -> bool {
        (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }
}

/// A star's growth-curve flux at `5F` (F-5; rim by owner ruling R6, 2026-10-11): `Σ (v − b)` over the pixels whose
/// centres lie within [`GROWTH_RADIUS_PER_FWHM`]` · fwhm` of `(x, y)`, `b` the plain mean of the present (finite,
/// unmasked) pixels at `10F ≤ d ≤ 12F` from it that `seeds` does not cover. Returns `(growth, b)`. `None` — the star
/// does not qualify — when the `12F` disc leaves the plane or the caller's `footprint` (some pixel centre within `12F`
/// with `footprint[i] == false`: LN's region outside its `allowed` mask holds zero fill and unseeded stars, F-5 round
/// 4), a pixel within `5F` is absent (it would bias the sum low by its missing light), or no rim pixel is left.
/// `inside` is scratch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn growth_curve(
    data: &[f32],
    mask: Option<&[bool]>,
    footprint: Option<&[bool]>,
    seeds: &SeedMask,
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    fwhm: f64,
    inside: &mut Vec<f64>,
) -> Option<(f64, f64)> {
    let (r_ap, r_in, r_out) = (
        GROWTH_RADIUS_PER_FWHM * fwhm,
        GROWTH_RIM_INNER_PER_FWHM * fwhm,
        GROWTH_RIM_OUTER_PER_FWHM * fwhm,
    );
    if !(r_out.is_finite() && r_out > 0.0)
        || x - r_out < 0.0
        || y - r_out < 0.0
        || x + r_out > (w as f64 - 1.0)
        || y + r_out > (h as f64 - 1.0)
    {
        return None;
    }
    let (r_ap2, r_in2, r_out2) = (r_ap * r_ap, r_in * r_in, r_out * r_out);
    inside.clear();
    let (mut rim_sum, mut rim_n) = (0.0f64, 0usize);
    let (y0, y1) = ((y - r_out).ceil() as usize, (y + r_out).floor() as usize);
    let (x0, x1) = ((x - r_out).ceil() as usize, (x + r_out).floor() as usize);
    for py in y0..=y1 {
        let dy2 = (py as f64 - y).powi(2);
        for px in x0..=x1 {
            let d2 = (px as f64 - x).powi(2) + dy2;
            let i = py * w + px;
            if d2 <= r_out2 && footprint.is_some_and(|f| !f[i]) {
                return None;
            }
            let present = data[i].is_finite() && mask.map_or(true, |m| m[i]);
            if d2 <= r_ap2 {
                if !present {
                    return None;
                }
                inside.push(data[i] as f64);
            } else if d2 >= r_in2 && d2 <= r_out2 && present && !seeds.covers(i) {
                rim_sum += data[i] as f64;
                rim_n += 1;
            }
        }
    }
    if rim_n == 0 {
        return None;
    }
    let b = rim_sum / rim_n as f64;
    Some((inside.iter().map(|v| v - b).sum(), b))
}

/// The clip screen's range on a path without a saturation verdict (F-5, owner ruling R6 / C6): F-3's
/// `range = min(plane max, norm) − bg` ([`SaturationPlane::new`]) from the fitted plane itself, in the plane's own
/// units — `plane max` over its present (finite, unmasked) pixels, `norm` the caller's `full_scale` in the same units
/// (F-5 round 4: LN's native planes 1.0; `None` = uncapped, `range = plane max − bg`) and `bg` the module's sky
/// median of the plane: the stratified sample ([`stats::stratified_sample`]) clipped to the open interval
/// `(0, plane max)` — `SkyLevel`'s clipped median (PA-21) without its one-count margins, which need the plane's units
/// (the refit paths hand native `[0, 1]` planes (LN) and ADU-scaled ones (the probe) alike). No in-range sample: `bg`
/// is 0, one `warn!`.
pub(crate) fn unverdicted_clip_range(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    full_scale: Option<f64>,
) -> f64 {
    let plane_max = data
        .iter()
        .enumerate()
        .filter(|&(i, v)| v.is_finite() && mask.map_or(true, |m| m[i]))
        .fold(f64::NEG_INFINITY, |m, (_, &v)| m.max(v as f64));
    let sample = stats::stratified_sample(data, w, h);
    let clipped = stats::clip_sample(&sample, 0.0, plane_max as f32);
    let bg = if clipped.is_empty() {
        tracing::warn!(
            samples = sample.len(),
            "aperture correction: no sky sample below the plane max; the clip screen's sky median is 0"
        );
        0.0
    } else {
        stats::median_of(&clipped) as f64
    };
    SaturationPlane::new(
        bg,
        0.0,
        plane_max,
        full_scale.unwrap_or(f64::INFINITY),
        false,
    )
    .range
}

/// The plane's aperture correction (F-5, ruling R-OSP-7; rim and screen by owner ruling R6, 2026-10-11):
/// `c = median(growth / model)` over the [`CORRECTION_STARS`] brightest qualifying fits by [`model_flux_in_fwtm`] (ties
/// by seed index), the growth the [`growth_curve`] flux at `5F` against the far-ring mean outside every seed's `3F`
/// disc ([`SeedMask`], built once here), `F` the plane's frame FWHM ([`super::frame_shape`] over the unsaturated fits,
/// the `FrameAggregates::fwhm_px` statistic). A qualifying fit is accepted (`fits` = the kept fits after the duplicate
/// drop, with their seed indices), not flagged saturated (own or cross-plane), has a finite positive model flux, passes
/// the clip screen (its fitted amplitude not above the range, F-3's amplitude clause: `saturation`'s
/// [`SaturationPlane::range`] on the measure path, else [`unverdicted_clip_range`] under the caller's `full_scale`),
/// has no other seed of `seeds` within [`CORRECTION_ISOLATION_PER_FIELD_FWHM`]` · field_fwhm` of its fitted centre,
/// and a [`growth_curve`] (its `12F` disc inside the plane and the caller's `footprint`, no absent pixel within `5F`,
/// a rim pixel left). Returns `(c, ratios)`: `c` is `None`
/// with no fit at all (`debug!`) or with fewer than [`CORRECTION_MIN_RATIOS`] ratios (one `warn!`: the model flux is
/// used uncorrected, `c = 1`); both per-plane events count the screened candidates (`skipped`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn aperture_correction(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    fits: &[(usize, StarFit)],
    seeds: &[Seed],
    field_fwhm: f64,
    saturation: Option<&SaturationPlane>,
    full_scale: Option<f64>,
    footprint: Option<&[bool]>,
) -> (Option<f64>, usize) {
    if fits.is_empty() {
        tracing::debug!(
            count = seeds.len(),
            "aperture correction: no fits on the plane; none measured"
        );
        return (None, 0);
    }
    let clip_range = saturation.map_or_else(
        || unverdicted_clip_range(data, mask, w, h, full_scale),
        |s| s.range,
    );
    let frame_fwhm =
        super::frame::frame_shape(fits.iter().map(|(_, f)| f).filter(|f| !f.flags.saturated()))
            .map(|(f, _)| f);
    let r_iso = CORRECTION_ISOLATION_PER_FIELD_FWHM * field_fwhm;
    let mut ratios: Vec<f64> = Vec::new();
    let mut screened = 0usize;
    if let Some(f_frame) = frame_fwhm.filter(|f| f.is_finite() && *f > 0.0) {
        if r_iso.is_finite() && r_iso > 0.0 {
            // Every seed by its `r_iso` cell: a seed within `r_iso` of a centre lies in the 3×3 cells about it.
            let cell = |x: f64, y: f64| ((x / r_iso).floor() as i64, (y / r_iso).floor() as i64);
            let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
            for (j, s) in seeds.iter().enumerate() {
                grid.entry(cell(s.x, s.y)).or_default().push(j);
            }
            let isolated = |own: usize, f: &StarFit| -> bool {
                let (cx, cy) = cell(f.x, f.y);
                !(cx - 1..=cx + 1).any(|gx| {
                    (cy - 1..=cy + 1).any(|gy| {
                        grid.get(&(gx, gy)).is_some_and(|at| {
                            at.iter().any(|&j| {
                                j != own && (seeds[j].x - f.x).hypot(seeds[j].y - f.y) <= r_iso
                            })
                        })
                    })
                })
            };
            let mut candidates: Vec<(usize, &StarFit, f64)> = fits
                .iter()
                .filter(|(_, f)| !f.flags.saturated())
                .map(|(i, f)| (*i, f, model_flux_in_fwtm(f)))
                .filter(|&(_, _, m)| m.is_finite() && m > 0.0)
                .collect();
            // The clip screen (R6 / C6): a candidate whose fitted amplitude tops the plane's range is clipped.
            candidates.retain(|&(_, f, _)| {
                let clipped = f.amplitude > clip_range;
                screened += usize::from(clipped);
                !clipped
            });
            candidates.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
            if !candidates.is_empty() {
                let seed_mask = SeedMask::new(w, h, seeds, GROWTH_RIM_SEED_MASK_PER_FWHM * f_frame);
                let mut inside = Vec::new();
                for (seed, f, model) in candidates {
                    if ratios.len() == CORRECTION_STARS {
                        break;
                    }
                    if !isolated(seed, f) {
                        continue;
                    }
                    if let Some((growth, _)) = growth_curve(
                        data,
                        mask,
                        footprint,
                        &seed_mask,
                        w,
                        h,
                        f.x,
                        f.y,
                        f_frame,
                        &mut inside,
                    ) {
                        ratios.push(growth / model);
                    }
                }
            }
        }
    }
    let n = ratios.len();
    if n < CORRECTION_MIN_RATIOS {
        tracing::warn!(
            count = n,
            skipped = screened,
            fwhm_px = frame_fwhm,
            "aperture correction: fewer than 5 isolated unsaturated stars; the star flux is the model's, uncorrected"
        );
        return (None, n);
    }
    let c = super::robust::median_f64(&mut ratios);
    tracing::debug!(
        count = n,
        skipped = screened,
        aperture_correction = c,
        fwhm_px = frame_fwhm,
        "aperture correction measured"
    );
    (Some(c), n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::tests::{grid30, grid30_spaced, median, seeds_of};
    use crate::star_measure::{
        measure_plane, MeasureOptions, PlaneMeasurement, PsfModel, SeedSpec, ADU_SCALE,
    };
    use crate::test_support::add_noise;
    use crate::test_support::{gaussian_field, moffat_field, MoffatStar};

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
        assert!(
            v.by_amplitude,
            "the amplitude clause carries this star: {v:?}"
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

    /// The plateau-clip clause needs no fit: with the verdict fit disabled (`max_iter: 0`, so `fit = false`) a passed
    /// plateau at the clip still flags the star.
    #[test]
    fn a_plateau_at_the_clip_flags_the_star_although_its_verdict_fit_failed() {
        let region = censored_star_region();
        let bg = 0.04_f32 as f64;
        let plane = SaturationPlane::new(bg, 0.002, 1.0, 1.0, false);
        let params = FitParams {
            max_iter: 0,
            ..FitParams::default()
        };
        let mut s = FitScratch::default();
        let v = saturation_verdict(&region, 0.0, 0.0, bg, 2.5, &plane, &params, &mut s)
            .expect("a verdict");
        assert!(
            !v.fit && v.saturated && v.by_plateau_clip && !v.by_amplitude,
            "{v:?}"
        );
    }

    // ---- F-5 (Plan A Task 10): the star flux ----

    /// Plan tests 1 and 2's isolated-star field (F-5 round 4): their premise is an isolated star, and R6's footprint is
    /// `12F`, so [`grid30_spaced`] puts the stars 140 px apart (on round 2's 70-px grid, 12.1 F, the neighbours'
    /// wings fall in the far ring: [`r6_far_ring_takes_in_the_neighbours_wings_on_a_70_px_grid`]) on a plane sized so
    /// every star's `12F` disc of the widest profile (Moffat β 2.5, α 5: measured frame FWHM 5.78 px, 12F = 69.4 px)
    /// lies inside: a 70-px margin + 1 px for the last pixel centre. The range-setting star (see test 1) sits at a grid
    /// cell's centre, 99 px from its four stars. Returns `(stars, w, h, the range-setting star's position)`.
    fn isolated_grid30(alpha: f64) -> (Vec<MoffatStar>, usize, usize, (f64, f64)) {
        const PITCH: f64 = 140.0;
        const MARGIN: f64 = 70.0;
        let (w, h) = (
            (2.0 * MARGIN + 5.0 * PITCH) as usize + 1,
            (2.0 * MARGIN + 4.0 * PITCH) as usize + 1,
        );
        let cell = MARGIN + 0.5 * PITCH;
        (grid30_spaced(alpha, PITCH, MARGIN), w, h, (cell, cell))
    }

    /// Plan test 1's measurement: `stars` and the range-setting star (1.5x the grid amplitude at `range_at`, rendered,
    /// NOT seeded) on a `w × h` Moffat-2.5 plane, measured at the fixed β 2.5 on the stars' own seeds.
    fn moffat_2_5_field_measured(
        stars: &[MoffatStar],
        range_at: (f64, f64),
        w: usize,
        h: usize,
    ) -> PlaneMeasurement {
        let mut field = stars.to_vec();
        field.push(MoffatStar {
            x: range_at.0,
            y: range_at.1,
            amp: 0.45,
            ..stars[0]
        });
        let data = moffat_field(w, h, &field, 2.5, 0.05);
        measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds_of(stars)),
                psf_model: PsfModel::Fixed(2.5),
                ..Default::default()
            },
            None,
        )
    }

    /// RED before F-5 (ruling F28): a Moffat-2.5 star's `signal` is 0.749 of its total at the FWTM (D4); after F-5 it is
    /// the total. On the isolated-star field ([`isolated_grid30`], F-5 round 4).
    #[test]
    fn captured_flux_is_the_total_flux_on_a_moffat_2_5_field() {
        let (stars, w, h, range_at) = isolated_grid30(5.0); // α 5 at β 2.5: total π·A·α²/(β−1) = π·0.3·25/1.5
        let total = std::f64::consts::PI * 0.3 * 25.0 / 1.5;
        // F-3's verdict flags a star whose fitted amplitude tops the plane range, so a field of equal brightest stars
        // has no unsaturated star: one range-setting star (1.5x the grid amplitude, rendered, NOT seeded) sets it.
        let m = moffat_2_5_field_measured(&stars, range_at, w, h);
        assert_eq!(m.stars.len(), 30);
        let med = median(
            m.stars
                .iter()
                .map(|f| f.signal / (total * ADU_SCALE as f64)),
        );
        assert!((med - 1.0).abs() < 0.01, "captured fraction {med}");
        assert!(m.aperture_correction.is_some());
    }

    /// A pin (ruling F28; F-5 round 4): R6's neighbour-wing cost. On round 2's 70-px grid of a 480 × 400 plane (12.1 F
    /// at the measured frame FWHM 5.78 px) the neighbours' β-2.5 wings beyond their `3F` discs fall in the far ring
    /// `[10F, 12F]` and lift its mean, so plan test 1's captured fraction reads 0.98164 (the lab replica's
    /// `growth_mean_rim` reads the same) — on the isolated 140-px grid it is within 1 % of the total (test 1).
    #[test]
    fn r6_far_ring_takes_in_the_neighbours_wings_on_a_70_px_grid() {
        let stars = grid30(5.0);
        let total = std::f64::consts::PI * 0.3 * 25.0 / 1.5;
        let m = moffat_2_5_field_measured(&stars, (25.0, 25.0), 480, 400);
        assert_eq!(m.stars.len(), 30);
        let med = median(
            m.stars
                .iter()
                .map(|f| f.signal / (total * ADU_SCALE as f64)),
        );
        assert!((med - 0.98164).abs() < 1e-5, "captured fraction {med}");
    }

    #[test]
    fn captured_flux_spread_across_profiles_is_within_two_percent() {
        // the same field as Gaussian (σ 2.123 → FWHM 5), Moffat 2.5, 4, 10 at equal total flux; (max − min)/min of the four medians ≤ 2 %
        // F-5 round 4: on the isolated-star field (`isolated_grid30`, 140 px apart).
        let total = 1.0f64;
        let (_, w, h, range_at) = isolated_grid30(1.0);
        let mut meds = Vec::new();
        for (beta, alpha) in [
            (2.5, 5.0 / (2.0 * (2f64.powf(0.4) - 1.0).sqrt())),
            (4.0, 5.0 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt())),
            (10.0, 5.0 / (2.0 * (2f64.powf(0.1) - 1.0).sqrt())),
        ] {
            let amp = total * (beta - 1.0) / (std::f64::consts::PI * alpha * alpha);
            let stars: Vec<MoffatStar> = isolated_grid30(alpha)
                .0
                .into_iter()
                .map(|s| MoffatStar { amp, ..s })
                .collect();
            // the range-setting star (see test 1): 1.5x amplitude at a cell centre, rendered, not seeded
            let mut field = stars.clone();
            field.push(MoffatStar {
                x: range_at.0,
                y: range_at.1,
                amp: 1.5 * amp,
                ..stars[0]
            });
            let data = moffat_field(w, h, &field, beta, 0.05);
            let m = measure_plane(
                &data,
                w,
                h,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds_of(&stars)),
                    ..Default::default()
                },
                None,
            );
            meds.push(median(m.stars.iter().map(|f| f.signal / ADU_SCALE as f64)));
        }
        let sigma = 5.0 / 2.354_820_045;
        let gstars: Vec<(f64, f64, f64)> = isolated_grid30(1.0)
            .0
            .iter()
            .map(|s| {
                (
                    s.x,
                    s.y,
                    total / (2.0 * std::f64::consts::PI * sigma * sigma),
                )
            })
            .collect();
        // the range-setting star (see test 1), rendered, not seeded
        let mut gfield = gstars.clone();
        gfield.push((range_at.0, range_at.1, 1.5 * gstars[0].2));
        let g = gaussian_field(w, h, &gfield, sigma, 0.05);
        let m = measure_plane(
            &g,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(
                    gstars
                        .iter()
                        .map(|&(x, y, p)| Seed {
                            x,
                            y,
                            peak: p,
                            flux: 1.0,
                        })
                        .collect(),
                ),
                ..Default::default()
            },
            None,
        );
        meds.push(median(m.stars.iter().map(|f| f.signal / ADU_SCALE as f64)));
        let (lo, hi) = (
            meds.iter().cloned().fold(f64::INFINITY, f64::min),
            meds.iter().cloned().fold(0.0, f64::max),
        );
        assert!((hi - lo) / lo < 0.02, "{meds:?}");
        assert!(meds.iter().all(|m| (m - total).abs() < 0.01), "{meds:?}");
    }

    #[test]
    fn aperture_correction_without_an_unsaturated_isolated_star_is_one_and_flagged() {
        let stars = vec![
            MoffatStar {
                x: 40.0,
                y: 40.0,
                amp: 0.3,
                alpha_x: 3.0,
                alpha_y: 3.0,
                theta: 0.0,
            },
            MoffatStar {
                x: 52.0,
                y: 40.0,
                amp: 0.3,
                alpha_x: 3.0,
                alpha_y: 3.0,
                theta: 0.0,
            },
            MoffatStar {
                x: 46.0,
                y: 52.0,
                amp: 0.3,
                alpha_x: 3.0,
                alpha_y: 3.0,
                theta: 0.0,
            },
        ];
        let data = moffat_field(100, 100, &stars, 4.0, 0.05);
        let m = measure_plane(
            &data,
            100,
            100,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds_of(&stars)),
                psf_model: PsfModel::Moffat4,
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.aperture_correction, None);
        let f = &m.stars[0];
        let model = f.amplitude
            * std::f64::consts::PI
            * (f.fwhm_x / 2.0 / (2f64.powf(0.25) - 1.0).sqrt())
            * (f.fwhm_y / 2.0 / (2f64.powf(0.25) - 1.0).sqrt())
            / 3.0
            * (1.0 - 10f64.powf(-0.75));
        assert!(
            (f.signal / model - 1.0).abs() < 1e-9,
            "c = 1: signal is the bare model flux"
        );
    }

    /// A pin (ruling F28: true before the growth-curve code reads it): the model flux is the analytic FWTM fraction
    /// `1 − 10^{(1−β)/β}` of the Moffat's total `π·A·αx·αy/(β−1)`, and continuous through β = 1 (its limit `ln 10`).
    #[test]
    fn model_flux_in_fwtm_is_the_analytic_fwtm_fraction_of_the_total() {
        let fit_at = |beta: f64, ax: f64, ay: f64| {
            let k = 2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt();
            let mut f = StarFit {
                x: 0.0,
                y: 0.0,
                background: 0.0,
                amplitude: 2.0,
                fwhm_x: ax * k,
                fwhm_y: ay * k,
                fwtm_x: 0.0,
                fwtm_y: 0.0,
                theta: 0.0,
                beta,
                residual: 0.0,
                signal: 0.0,
                area: 0.0,
                peak5: f64::NAN,
                flags: Default::default(),
            };
            f.fwtm_x = 2.0 * ax * (10f64.powf(1.0 / beta) - 1.0).sqrt();
            f.fwtm_y = 2.0 * ay * (10f64.powf(1.0 / beta) - 1.0).sqrt();
            f
        };
        for beta in [1.5, 2.5, 4.0, 10.0, 100.0] {
            let total = std::f64::consts::PI * 2.0 * 3.0 * 2.0 / (beta - 1.0);
            let got = model_flux_in_fwtm(&fit_at(beta, 3.0, 2.0));
            let want = total * (1.0 - 10f64.powf((1.0 - beta) / beta));
            assert!(
                (got / want - 1.0).abs() < 1e-12,
                "β {beta}: {got} vs {want}"
            );
        }
        let at_one = model_flux_in_fwtm(&fit_at(1.0, 3.0, 2.0));
        let near_one = model_flux_in_fwtm(&fit_at(1.0 + 1e-7, 3.0, 2.0));
        assert!(
            at_one.is_finite() && (at_one / near_one - 1.0).abs() < 1e-5,
            "{at_one} vs {near_one}"
        );
    }

    /// A pin (C0's qualifying rules; rim and edge by owner ruling R6): the growth-curve flux of a lone round Gaussian
    /// star is its total within its `5F` disc, measured against the `[10F, 12F]` rim mean; the `12F` disc leaving the
    /// plane, or one absent pixel within `5F`, refuses it; an absent rim pixel is only left out.
    #[test]
    fn growth_curve_is_the_disc_sum_over_the_rim_and_refuses_an_edge_or_absent_pixel() {
        let (w, h) = (120usize, 120usize);
        let sigma = 2.0;
        let fwhm = sigma * 2.354_820_045;
        let amp = 1000.0;
        let data = gaussian_field(w, h, &[(60.3, 59.6, amp)], sigma, 50.0);
        let total = 2.0 * std::f64::consts::PI * sigma * sigma * amp;
        let none = SeedMask::new(w, h, &[], 0.0);
        let mut scratch = Vec::new();
        let (g, b) = growth_curve(
            &data,
            None,
            None,
            &none,
            w,
            h,
            60.3,
            59.6,
            fwhm,
            &mut scratch,
        )
        .unwrap();
        assert!((g / total - 1.0).abs() < 1e-3, "{g} vs {total}");
        assert_eq!(b, 50.0, "the far ring is flat sky");
        // The 12F disc (56.5 px) leaves a 120-px plane from a centre 30 px off its edge.
        assert_eq!(
            growth_curve(
                &data,
                None,
                None,
                &none,
                w,
                h,
                30.0,
                60.0,
                fwhm,
                &mut scratch
            ),
            None
        );
        let mut holed = data.clone();
        holed[62 * w + 61] = f32::NAN; // 2.5 px from the centre, inside 5F (23.5 px)
        assert_eq!(
            growth_curve(
                &holed,
                None,
                None,
                &none,
                w,
                h,
                60.3,
                59.6,
                fwhm,
                &mut scratch
            ),
            None
        );
        let mut mask = vec![true; w * h];
        mask[62 * w + 61] = false;
        assert_eq!(
            growth_curve(
                &data,
                Some(&mask),
                None,
                &none,
                w,
                h,
                60.3,
                59.6,
                fwhm,
                &mut scratch
            ),
            None
        );
        let mut rim_hole = data.clone();
        rim_hole[59 * w + (60 + 52)] = f32::NAN; // 51.7 px out: in the [10F, 12F] = [47.1, 56.5] rim
        let (g2, _) = growth_curve(
            &rim_hole,
            None,
            None,
            &none,
            w,
            h,
            60.3,
            59.6,
            fwhm,
            &mut scratch,
        )
        .unwrap();
        assert!((g2 - g).abs() < 1e-6 * g, "{g2} vs {g}");
    }

    fn seed_at(x: f64, y: f64) -> Seed {
        Seed {
            x,
            y,
            peak: 1.0,
            flux: 1.0,
        }
    }

    /// R6 (owner ruling 2026-10-11): a neighbour inside the `[10F, 12F]` ring leaves the rim mean `b` where it is when
    /// its seed is handed to the call (its `3F` disc is masked out of the ring) and lifts it when it is not.
    #[test]
    fn a_seeded_neighbour_in_the_rim_ring_is_masked_out_of_the_rim_mean() {
        let (w, h) = (160usize, 160usize);
        let sigma = 2.0;
        // F 4.71 px: the ring [47.1, 56.5] px, the seed discs 3F = 14.1 px (the Gaussians are rendered out to 5σ = 10 px).
        let fwhm = sigma * 2.354_820_045;
        let (x, y) = (80.3, 79.6);
        let (nx, ny) = (117.0, 116.0); // 51.7 px from the star: inside the ring
        let lone = gaussian_field(w, h, &[(x, y, 1000.0)], sigma, 50.0);
        let pair = gaussian_field(w, h, &[(x, y, 1000.0), (nx, ny, 1000.0)], sigma, 50.0);
        let r = GROWTH_RIM_SEED_MASK_PER_FWHM * fwhm;
        let own = SeedMask::new(w, h, &[seed_at(x, y)], r);
        let both = SeedMask::new(w, h, &[seed_at(x, y), seed_at(nx, ny)], r);
        let mut scratch = Vec::new();
        let (_, b_lone) =
            growth_curve(&lone, None, None, &own, w, h, x, y, fwhm, &mut scratch).unwrap();
        let (_, b_masked) =
            growth_curve(&pair, None, None, &both, w, h, x, y, fwhm, &mut scratch).unwrap();
        assert!(
            (b_masked - b_lone).abs() < 1e-9,
            "seeded neighbour: b {b_masked} vs {b_lone}"
        );
        let (_, b_open) =
            growth_curve(&pair, None, None, &own, w, h, x, y, fwhm, &mut scratch).unwrap();
        assert!(
            b_open - b_lone > 1.0,
            "unseeded neighbour: b {b_open} vs {b_lone}"
        );
    }

    /// R6: the growth curve needs the whole `12F` disc inside the plane — a star 52 px from an edge (its `8F` disc,
    /// 37.7 px, inside) is refused at every edge, one 57 px from it is measured.
    #[test]
    fn the_growth_curve_refuses_a_star_whose_12f_disc_leaves_the_plane() {
        let (w, h) = (160usize, 160usize);
        let sigma = 2.0;
        let fwhm = sigma * 2.354_820_045; // 12F = 56.5 px; the far edges are at 159
        let none = SeedMask::new(w, h, &[], 0.0);
        let mut scratch = Vec::new();
        for (x, y, measured) in [
            (52.0, 80.0, false),
            (57.0, 80.0, true),
            (80.0, 52.0, false),
            (80.0, 57.0, true),
            (107.0, 80.0, false),
            (102.0, 80.0, true),
            (80.0, 107.0, false),
            (80.0, 102.0, true),
        ] {
            let data = gaussian_field(w, h, &[(x, y, 1000.0)], sigma, 50.0);
            let g = growth_curve(&data, None, None, &none, w, h, x, y, fwhm, &mut scratch);
            assert_eq!(g.is_some(), measured, "({x}, {y}): {g:?}");
        }
    }

    /// F-5 round 4: the caller's footprint holds the whole `12F` disc like the plane edge — one pixel outside the
    /// footprint 56 px from the centre (inside `12F` = 56.5 px) refuses the star, one 57 px from it does not, and the
    /// measured star's growth and rim are those of the call without a footprint (the footprint is read, never summed).
    #[test]
    fn the_growth_curve_refuses_a_star_whose_12f_disc_leaves_the_footprint() {
        let (w, h) = (160usize, 160usize);
        let sigma = 2.0;
        let fwhm = sigma * 2.354_820_045; // 12F = 56.5 px
        let (x, y) = (80.0, 80.0);
        let data = gaussian_field(w, h, &[(x, y, 1000.0)], sigma, 50.0);
        let none = SeedMask::new(w, h, &[], 0.0);
        let mut scratch = Vec::new();
        let bare = growth_curve(&data, None, None, &none, w, h, x, y, fwhm, &mut scratch).unwrap();
        for (px, measured) in [(136usize, false), (137, true)] {
            let mut footprint = vec![true; w * h];
            footprint[80 * w + px] = false;
            let g = growth_curve(
                &data,
                None,
                Some(&footprint),
                &none,
                w,
                h,
                x,
                y,
                fwhm,
                &mut scratch,
            );
            assert_eq!(g.is_some(), measured, "outside at x = {px}: {g:?}");
            if let Some(g) = g {
                assert_eq!(g, bare, "the footprint changes no sum");
            }
        }
    }
}
