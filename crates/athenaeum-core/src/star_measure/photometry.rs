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
//! longer depends on how much of a profile its FWTM ellipse holds. The growth curve's sky is the star's stored B_i,
//! and a candidate whose fitted amplitude tops the plane's range is screened out (F-3's amplitude clause, on every
//! path; owner ruling R6, 2026-10-11). Round 4: off the measure path the screen's full scale is the caller's
//! ([`super::FitStarsOptions::full_scale`]), and a caller's footprint ([`super::FitStarsOptions::footprint`], LN's
//! `allowed` region) must hold a candidate's whole `12F` disc.
//!
//! One aperture-scale sky and noise per star (Task 10b, F-10, rulings B1–B4): every accepted fit carries B_i, the plain
//! mean of its present pixels at `10F ≤ d ≤ 12F` outside every seed's `3F` disc (R6's ring, [`ring_sky`], measured
//! once per fit after the duplicate drop, [`ring_skies`]), σ_i, the RMS of `v − B_i` over the same pixels, and n_i.
//! Every aperture consumer reads them and none re-estimates them: `c` now, Task 11's SNR and HFR later. The fit's own
//! local `b` stays free (B1): it is the model's scale, B_i the aperture's; `b − B_i` is the stored near/far diagnostic
//! ([`super::StarFit::sky_offset`]).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use astroimage::analysis::fitting::{fit_moffat_2d_bounded, PixelSample};
use rayon::prelude::*;

use super::beta::{BETA_BOUNDS, BETA_START};
use super::diagnostics::RefusalReason;
use super::fit::{FitParams, FitScratch, Seed, StarFit};
use crate::integration::stats;
#[cfg(test)]
use crate::integration::stats::{median_in_place, MAD_TO_SIGMA};

/// PA-11: the annulus's inner radius is `max(3·FWHM, 6)` px …
#[cfg(test)]
pub const ANNULUS_INNER_PER_FWHM: f64 = 3.0;
#[cfg(test)]
pub const ANNULUS_INNER_MIN_PX: f64 = 6.0;
/// … and its width `max(FWHM, 3)` px.
#[cfg(test)]
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
/// Focus 1 and 5). `None` under 8 present pixels. Retired for the SNR by Task 10b's ruling B4 (no seed mask; its p90
/// read 1.09–1.32 of the truth): kept as a `#[cfg(test)]` function (a test is its only reader); Task 11 reads σ_i ([`ring_sky`]).
///
/// A pixel belongs to the ring when its centre lies at a distance `d` from `(x, y)` with `inner ≤ d < inner + width`.
#[cfg(test)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    /// The maximum of the plane's finite, unmasked pixels (uncapped by `norm`).
    pub plane_max: f64,
    /// How many present pixels equal [`SaturationPlane::plane_max`]: two or more is an exact clip.
    pub max_count: usize,
    /// The plane's near-clip floor (Task 10c, R10c-10), working units: derived once per plane after the verdict pass
    /// ([`derive_clip_floor`]); `None` until then and whenever the plane shows no clipped evidence, which leaves the
    /// near-clip clause with nothing to flag.
    pub clip_floor: Option<f64>,
    /// How many pieces of evidence set [`SaturationPlane::clip_floor`]: the tie (one) plus the certified cores.
    pub clip_evidence: usize,
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
        plane_max: f64::INFINITY,
        max_count: 0,
        clip_floor: None,
        clip_evidence: 0,
    };

    /// `threshold = bg + 5·sigma`, `range = min(plane_max, norm) − bg`; `plane_max` over the plane's finite,
    /// unmasked pixels. One pixel at the maximum, no floor ([`SaturationPlane::with_max_count`] says otherwise).
    pub fn new(bg: f64, sigma: f64, plane_max: f64, norm: f64, colour: bool) -> Self {
        SaturationPlane {
            bg,
            threshold: bg + THRESHOLD_SIGMA * sigma,
            norm,
            range: plane_max.min(norm) - bg,
            colour,
            plane_max,
            max_count: 1,
            clip_floor: None,
            clip_evidence: 0,
        }
    }

    /// The same plane with `n` present pixels at its maximum.
    pub fn with_max_count(mut self, n: usize) -> Self {
        self.max_count = n;
        self
    }

    /// The same plane with its near-clip floor set.
    pub fn with_clip_floor(mut self, floor: Option<f64>) -> Self {
        self.clip_floor = floor;
        self
    }

    /// The knot exemption's band test (Task 10c, R10c-1), read BEFORE any verdict exists: the maximum of the finite
    /// values `top3x3` stands within the plateau band of the clip, `max - bg >= (1 - PLATEAU_RANGE)·range`. An empty
    /// or all-non-finite set is not near the clip. On [`SaturationPlane::UNTESTED`] nothing is. The verdict's
    /// near-clip clause (class B) reads [`SaturationPlane::clip_floor`] instead; every floor is at or above this band
    /// (the plane max, or a pixel0 that passed clause 2's own band), so class B is a subset of what this test passes.
    pub(crate) fn near_clip(&self, top3x3: impl IntoIterator<Item = f64>) -> bool {
        let top = top3x3
            .into_iter()
            .filter(|v| v.is_finite())
            .fold(f64::NEG_INFINITY, f64::max);
        top - self.bg >= (1.0 - PLATEAU_RANGE) * self.range
    }
}

/// The plane's near-clip floor (Task 10c, R10c-10; owner: "adjust the threshold in the ideal case"): the lowest value
/// at which this plane's own clipped evidence shows a clip. Evidence is (1) the plane max, when two or more present
/// pixels equal it (an exact clip), and (2) `pixel0` of every verdict flagged by clause 1 (the fitted amplitude beyond
/// the range) AND clause 2 (a plateau with pixel0 within the band of the clip): a certified clipped core. The floor is
/// the minimum of the evidence, no margin (a 5σ margin changed no lab or real number); no evidence, no floor.
///
/// Why per plane: an exact clip collapses the floor onto the clip value, a flat-divided clip (calibrated frames) spans
/// the spread of its clipped values and the floor is its lowest, and a clip-free plane shows neither a tie nor a
/// certified core, so nothing is near a clip. Either clause alone certifies unclipped stars (clause 2 a big star with
/// a flat top, clause 1 the brightest star of a clip-free plane); together they certified none on the 11 clip-free
/// lab planes. `pixel0`, not the 3×3 maximum: on a debayered plane a core's 3×3 maximum is debayer overshoot.
///
/// Every floor is at least `bg + 0.9·range` (the tied maximum is the plane max; clause 2 holds pixel0 above the
/// band), so class B never flags a star the knot exemption's band test ([`SaturationPlane::near_clip`]) did not pass.
///
/// Limits: (a) a clipped plane whose clipped stars are all one-pixel clips has no tie and no certified core, so no
/// class B; (b) the floor never goes below the plateau band, so a clip lying below `bg + 0.9·range` (a mono plane
/// whose clip is 0.86 to 0.90 of the range) is not seen; (c) the floor is the frame-wide lowest clipped value, so an
/// unclipped star between it and a vignetted corner's clip is flagged; (d) one certified star can set the floor;
/// (e) clauses 1 and 2 keep their own clip-free false positives; (f) a `MostlySaturated` refusal (the heaviest clipped
/// core) has no verdict, so its pixel0 is never evidence: on a flat-divided clip whose lowest-clipped stars are all
/// refused, the floor sits too high (the miss side, never a false flag); (g) on an integer plane a chance tie at a
/// clip-free maximum sets a floor at the plane max, and class B then adds only the star(s) holding that pixel, which
/// clause 1 flags anyway (e).
pub(crate) fn derive_clip_floor<'a>(
    plane: &SaturationPlane,
    verdicts: impl IntoIterator<Item = &'a SaturationVerdict>,
) -> Option<(f64, usize)> {
    if !plane.range.is_finite() {
        return None;
    }
    let mut floor = (plane.max_count >= 2).then_some(plane.plane_max);
    let mut evidence = usize::from(floor.is_some());
    for v in verdicts {
        if v.by_amplitude && v.by_plateau_clip && v.pixel0.is_finite() {
            floor = Some(floor.map_or(v.pixel0, |f| f.min(v.pixel0)));
            evidence += 1;
        }
    }
    floor.map(|f| (f, evidence))
}

/// Re-reads clause 3 (class B) on a verdict that was made before the plane's floor existed: the plateau test did not
/// pass and the 3×3 maximum stands at or above [`SaturationPlane::clip_floor`]. Level and fit are untouched; `saturated`
/// is the star's own flag again (a cross flag is applied later).
pub(crate) fn mark_near_clip(plane: &SaturationPlane, v: &mut SaturationVerdict) {
    v.by_near_clip = !v.plateau && plane.clip_floor.is_some_and(|f| v.top3x3 >= f);
    v.saturated = v.by_amplitude || v.by_plateau_clip || v.by_near_clip;
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    /// The star's own flag or a cross flag: `by_amplitude || by_plateau_clip || by_near_clip || cross`.
    pub saturated: bool,
    /// Clause 1: the verdict fit succeeded and `amplitude > range`.
    pub by_amplitude: bool,
    /// Clause 2 (plateau-clip, owner 2026-10-09): the plateau test passed and `pixel0 − bg ≥ 0.9·range` (pixel0 within the
    /// test's own 0.1·range band of the clip), whatever the fitted amplitude. It needs no fit: a verdict whose fit
    /// failed (`fit = false`) with a passed plateau at the clip is flagged.
    pub by_plateau_clip: bool,
    /// Clause 3 (near-clip, Task 10c): the plateau test did NOT pass and the 3x3 maximum about the seed pixel stands
    /// within the plateau band of the clip ([`SaturationPlane::near_clip`]). An undersampled star clipped on about one
    /// pixel has no plateau (its neighbours are not high), so clause 2 cannot see it; its clipped pixel stays in the
    /// fit (level and fit unchanged) and the point model's beta rises instead of the amplitude passing the range.
    /// Like clause 2 it needs no fit: it holds when the verdict fit failed.
    pub by_near_clip: bool,
    /// The region's pixel at the rounded seed centre (working units), NaN when the region lacks it.
    pub pixel0: f64,
    /// The maximum of the region's finite pixels within the 3x3 about the seed pixel (working units); negative
    /// infinity when there are none. Clause 3 reads it against the plane's floor.
    pub top3x3: f64,
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
    /// The verdict fit passed the fit's own centre and region tests (Task 10b ruling A4; `CentreMoved`,
    /// `CentreAtRim`, `RegionTooSmall`, [`super::fit`]'s `placement`, on the seed's region): only a centred verdict
    /// joins the frame β's sample. `false` when the fit failed (`fit = false`).
    pub centred: bool,
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
/// A successful fit is `centred` when it passes the fit's centre and region tests on the region of radius `r` about
/// the stamp origin (Task 10b ruling A4).
#[allow(clippy::too_many_arguments)]
pub(crate) fn saturation_verdict(
    region: &[PixelSample],
    seed_x: f64,
    seed_y: f64,
    b0: f64,
    f_start: f64,
    r: f64,
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
    // near-clip clause (class B): no plateau and the 3x3 maximum at or above the plane's floor. The floor is derived
    // from the plane's verdicts after this pass ([`derive_clip_floor`]), so a plane without one yet flags nothing and
    // [`mark_near_clip`] re-reads this clause; the 3x3 maximum and pixel0 are kept for it.
    let (px0, py0) = ((seed_x + 0.5).floor(), (seed_y + 0.5).floor());
    let top3x3 = region
        .iter()
        .filter(|s| (s.x - px0).abs() <= 1.0 && (s.y - py0).abs() <= 1.0)
        .map(|s| s.value)
        .filter(|v| v.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    let pixel0 = region
        .iter()
        .find(|s| s.x == px0 && s.y == py0)
        .map_or(f64::NAN, |s| s.value);
    let by_near_clip = plateau.is_none() && plane.clip_floor.is_some_and(|f| top3x3 >= f);
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
        saturated: by_plateau_clip || by_near_clip,
        by_amplitude: false,
        by_plateau_clip,
        by_near_clip,
        pixel0,
        top3x3,
        fit: false,
        cross: false,
        iterations: 0,
        centred: false,
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
    verdict.saturated = verdict.by_amplitude || by_plateau_clip || by_near_clip;
    verdict.fit = true;
    verdict.centred = super::fit::placement(
        (m.x0, m.y0),
        (m.alpha_x, m.alpha_y),
        m.beta,
        (seed_x, seed_y),
        (0.0, 0.0, r),
        m.theta,
        params,
    )
    .is_ok();
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
/// The growth-curve aperture's local sky (owner ruling R6, 2026-10-11; from Task 10b every accepted star's B_i,
/// ruling B2, [`ring_sky`]): the plain mean of the present pixels at `10F ≤ d ≤ 12F` from the same centre that lie
/// outside every seed's [`GROWTH_RIM_SEED_MASK_PER_FWHM`] disc. Far enough out that a star's own wing barely reaches
/// it; the mask keeps the neighbours' cores out of it.
pub const GROWTH_RIM_INNER_PER_FWHM: f64 = 10.0;
pub const GROWTH_RIM_OUTER_PER_FWHM: f64 = 12.0;
/// The radius, per frame FWHM, of the disc about every seed of the call (the star's own included, by its seed
/// position) whose pixels the ring sky B_i leaves out (R6).
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
/// One bit per pixel, built once per call; [`ring_sky`] leaves the covered pixels out.
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

/// One star's aperture-scale sky and noise (Task 10b, F-10, rulings B2 and B4), in the plane's units: what
/// [`ring_sky`] reads over R6's ring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingSky {
    /// B_i: the plain mean of the ring's present pixels.
    pub mean: f64,
    /// σ_i: the RMS of `v − B_i` over the same pixels.
    pub sigma: f64,
    /// n_i: the present ring pixels read.
    pub n: u32,
}

/// A star's aperture-scale sky B_i and noise σ_i (Task 10b, F-10, rulings B2 and B4): R6's ring — the pixels whose
/// centres lie at `10F ≤ d ≤ 12F` from `(x, y)` ([`GROWTH_RIM_INNER_PER_FWHM`], [`GROWTH_RIM_OUTER_PER_FWHM`]) that
/// are present (finite, unmasked, and inside the caller's `footprint`), on the plane, and outside every seed's disc
/// (`seeds`, [`GROWTH_RIM_SEED_MASK_PER_FWHM`]` · F`, built once per call). `mean` is their plain mean, summed in
/// row-major order (the sum the growth curve's rim read before Task 10b, bit for bit, wherever the `12F` disc lies on
/// the plane and the footprint); `sigma` is the RMS of `v − mean` over the same pixels; a star near an edge reads its
/// partial ring. `None` under [`ANNULUS_MIN_PIXELS`] present pixels or for a non-finite centre or width.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ring_sky(
    data: &[f32],
    mask: Option<&[bool]>,
    footprint: Option<&[bool]>,
    seeds: &SeedMask,
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    fwhm: f64,
) -> Option<RingSky> {
    let (r_in, r_out) = (
        GROWTH_RIM_INNER_PER_FWHM * fwhm,
        GROWTH_RIM_OUTER_PER_FWHM * fwhm,
    );
    if !(x.is_finite() && y.is_finite() && r_out.is_finite() && r_out > 0.0) || w == 0 || h == 0 {
        return None;
    }
    let (r_in2, r_out2) = (r_in * r_in, r_out * r_out);
    let (y0, y1) = (
        (y - r_out).ceil().max(0.0),
        (y + r_out).floor().min((h - 1) as f64),
    );
    let (x0, x1) = (
        (x - r_out).ceil().max(0.0),
        (x + r_out).floor().min((w - 1) as f64),
    );
    if y1 < y0 || x1 < x0 {
        return None;
    }
    let (y0, y1, x0, x1) = (y0 as i64, y1 as i64, x0 as i64, x1 as i64);
    // The plain sum is the mean's (row-major, as the growth curve's rim summed it); the variance reads sums shifted by
    // the first ring value, so no large sky level cancels in them.
    let (mut sum, mut n) = (0.0f64, 0usize);
    let (mut shift, mut s1, mut s2) = (f64::NAN, 0.0f64, 0.0f64);
    for py in y0..=y1 {
        let dy2 = (py as f64 - y).powi(2);
        if dy2 > r_out2 {
            continue;
        }
        // The pixels surely inside the inner circle (one pixel of margin) are never read: every pixel left is tested
        // by the same `d²` as before, so the ring's set and order do not change.
        let half = if dy2 < r_in2 {
            (r_in2 - dy2).sqrt() - 1.0
        } else {
            0.0
        };
        let (skip_lo, skip_hi) = if half > 0.0 {
            ((x - half).floor() as i64 + 1, (x + half).ceil() as i64 - 1)
        } else {
            (i64::MAX, i64::MIN)
        };
        let row = py as usize * w;
        let mut px = x0;
        while px <= x1 {
            if px >= skip_lo && px <= skip_hi {
                px = skip_hi + 1;
                continue;
            }
            let d2 = (px as f64 - x).powi(2) + dy2;
            let i = row + px as usize;
            px += 1;
            if d2 < r_in2 || d2 > r_out2 || seeds.covers(i) {
                continue;
            }
            let v = data[i];
            if !(v.is_finite() && mask.map_or(true, |m| m[i]) && footprint.map_or(true, |f| f[i])) {
                continue;
            }
            let v = v as f64;
            sum += v;
            n += 1;
            if shift.is_nan() {
                shift = v;
            }
            let d = v - shift;
            s1 += d;
            s2 += d * d;
        }
    }
    if n < ANNULUS_MIN_PIXELS {
        return None;
    }
    let nf = n as f64;
    let mean = sum / nf;
    let var = (s2 / nf - (s1 / nf).powi(2)).max(0.0);
    Some(RingSky {
        mean,
        sigma: var.sqrt(),
        n: u32::try_from(n).unwrap_or(u32::MAX),
    })
}

/// Every kept fit's B_i, σ_i and n_i (Task 10b, F-10, ruling B2: once per accepted fit, after the duplicate drop), at
/// the ring width `F` returned: the caller's `given` (LN's pair-common arm, [`super::FitStarsOptions::ring_fwhm`];
/// one that is not a positive number is refused with a `warn!`), else the plane's frame FWHM over the kept fits
/// neither saturated nor extended ([`super::frame::frame_shape`], the `FrameAggregates::fwhm_px` statistic). The
/// seeds' `3F` discs are built once ([`SeedMask`]). A fit without a ring (fewer than 8 present pixels, or no `F`: no
/// fit in the statistic) keeps NaN, NaN, 0 and is counted in the returned `missing`. `pool` as the fit's.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ring_skies(
    data: &[f32],
    mask: Option<&[bool]>,
    footprint: Option<&[bool]>,
    w: usize,
    h: usize,
    seeds: &[Seed],
    kept: &mut [(usize, StarFit)],
    given: Option<f64>,
    pool: Option<&std::sync::Arc<rayon::ThreadPool>>,
) -> (Option<f64> /* F */, usize /* missing */) {
    for (_, f) in kept.iter_mut() {
        f.sky_ring = f64::NAN;
        f.sky_ring_sigma = f64::NAN;
        f.sky_ring_n = 0;
    }
    if kept.is_empty() {
        return (None, 0);
    }
    let own = || {
        super::frame::frame_shape(
            kept.iter()
                .map(|(_, f)| f)
                .filter(|f| f.flags.in_statistic()),
        )
        .map(|(f, _)| f)
    };
    let fwhm = match given {
        Some(f) if f.is_finite() && f > 0.0 => Some(f),
        Some(bad) => {
            tracing::warn!(
                fwhm_px = bad,
                count = kept.len(),
                "ring sky: the given ring width is not a positive number; the plane's own frame FWHM is used"
            );
            own()
        }
        None => own(),
    }
    .filter(|f| f.is_finite() && *f > 0.0);
    let Some(f_ring) = fwhm else {
        tracing::debug!(
            count = kept.len(),
            "ring sky: no frame FWHM on the plane; no star carries a ring sky"
        );
        return (None, kept.len());
    };
    let seed_mask = SeedMask::new(w, h, seeds, GROWTH_RIM_SEED_MASK_PER_FWHM * f_ring);
    let mut body = || {
        kept.par_iter_mut().for_each(|(_, f)| {
            if let Some(r) = ring_sky(data, mask, footprint, &seed_mask, w, h, f.x, f.y, f_ring) {
                f.sky_ring = r.mean;
                f.sky_ring_sigma = r.sigma;
                f.sky_ring_n = r.n;
            }
        })
    };
    match pool {
        Some(p) => p.install(body),
        None => body(),
    }
    let missing = kept.iter().filter(|(_, f)| f.sky_ring_n == 0).count();
    tracing::debug!(
        count = kept.len(),
        missing,
        fwhm_px = f_ring,
        "ring sky measured"
    );
    (Some(f_ring), missing)
}

/// A star's growth-curve flux at `5F` (F-5; its sky the star's stored B_i from Task 10b, ruling B2): `Σ (v − b)` over
/// the pixels whose centres lie within [`GROWTH_RADIUS_PER_FWHM`]` · fwhm` of `(x, y)`, summed in row-major order, `b`
/// the caller's B_i ([`ring_sky`]). `None` — the star does not qualify (`c`'s own rule, unchanged) — when `b` is
/// missing (not finite), the `12F` disc leaves the plane or the caller's `footprint` (some pixel centre within `12F`
/// with `footprint[i] == false`: LN's region outside its `allowed` mask holds zero fill and unseeded stars, F-5 round
/// 4), or a pixel within `5F` is absent (it would bias the sum low by its missing light). `inside` is scratch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn growth_curve(
    data: &[f32],
    mask: Option<&[bool]>,
    footprint: Option<&[bool]>,
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    fwhm: f64,
    b: f64,
    inside: &mut Vec<f64>,
) -> Option<f64> {
    let (r_ap, r_out) = (
        GROWTH_RADIUS_PER_FWHM * fwhm,
        GROWTH_RIM_OUTER_PER_FWHM * fwhm,
    );
    if !b.is_finite()
        || !(r_out.is_finite() && r_out > 0.0)
        || x - r_out < 0.0
        || y - r_out < 0.0
        || x + r_out > (w as f64 - 1.0)
        || y + r_out > (h as f64 - 1.0)
    {
        return None;
    }
    let (r_ap2, r_out2) = (r_ap * r_ap, r_out * r_out);
    inside.clear();
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
            if d2 <= r_ap2 {
                if !(data[i].is_finite() && mask.map_or(true, |m| m[i])) {
                    return None;
                }
                inside.push(data[i] as f64);
            }
        }
    }
    Some(inside.iter().map(|v| v - b).sum())
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

/// The plane's aperture correction (F-5, ruling R-OSP-7; rim and screen by owner ruling R6, 2026-10-11; the rim read
/// once per star from Task 10b, ruling B2): `c = median(growth / model)` over the [`CORRECTION_STARS`] brightest
/// qualifying fits by [`model_flux_in_fwtm`] (ties by seed index), the growth the [`growth_curve`] flux at `5F` against
/// the fit's stored B_i ([`StarFit::sky_ring`], [`ring_skies`]), `F` = `ring_fwhm`, the ring width the B_i were read at
/// (the plane's frame FWHM over the fits neither saturated nor extended, or the caller's
/// [`super::FitStarsOptions::ring_fwhm`]). A qualifying fit is accepted (`fits` = the kept fits after the duplicate
/// drop, with their seed indices), not flagged saturated (own or cross-plane) nor extended (Task 10b, P3), has a
/// finite positive model flux, passes the clip screen (its fitted amplitude not above the range, F-3's amplitude
/// clause: `saturation`'s [`SaturationPlane::range`] on the measure path, else [`unverdicted_clip_range`] under the
/// caller's `full_scale`), has no other seed of `seeds` within [`CORRECTION_ISOLATION_PER_FIELD_FWHM`]` · field_fwhm`
/// of its fitted centre, and a [`growth_curve`] (its B_i present, its `12F` disc inside the plane and the caller's
/// `footprint`, no absent pixel within `5F`). Returns `(c, ratios)`: `c` is `None` with no fit at all (`debug!`) or
/// with fewer than [`CORRECTION_MIN_RATIOS`] ratios (one `warn!`: the model flux is used uncorrected, `c = 1`); both
/// per-plane events count the screened candidates (`skipped`). `own[k]` (Task 12, the budget fits a subset of `seeds`) is
/// the index into `seeds` of fit seed `k`, the one the isolation test skips; `None` = the identity.
#[allow(clippy::too_many_arguments)]
pub(crate) fn aperture_correction(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    fits: &[(usize, StarFit)],
    seeds: &[Seed],
    own: Option<&[usize]>,
    field_fwhm: f64,
    ring_fwhm: Option<f64>,
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
    let r_iso = CORRECTION_ISOLATION_PER_FIELD_FWHM * field_fwhm;
    let mut ratios: Vec<f64> = Vec::new();
    let mut screened = 0usize;
    if let Some(f_ring) = ring_fwhm.filter(|f| f.is_finite() && *f > 0.0) {
        if r_iso.is_finite() && r_iso > 0.0 {
            // Every seed by its `r_iso` cell: a seed within `r_iso` of a centre lies in the 3×3 cells about it.
            let cell = |x: f64, y: f64| ((x / r_iso).floor() as i64, (y / r_iso).floor() as i64);
            let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
            for (j, s) in seeds.iter().enumerate() {
                grid.entry(cell(s.x, s.y)).or_default().push(j);
            }
            // `own[k]` = the index into `seeds` of fit k's seed (the budget fits a subset, Task 12); `None` = the identity.
            let isolated = |fit_seed: usize, f: &StarFit| -> bool {
                let own = own.map_or(fit_seed, |o| o[fit_seed]);
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
            // Neither saturated (F-3) nor extended (F-10, P3): a knot is not a star whose growth curve `c` reads.
            let mut candidates: Vec<(usize, &StarFit, f64)> = fits
                .iter()
                .filter(|(_, f)| f.flags.in_statistic())
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
            let mut inside = Vec::new();
            for (seed, f, model) in candidates {
                if ratios.len() == CORRECTION_STARS {
                    break;
                }
                if !isolated(seed, f) {
                    continue;
                }
                if let Some(growth) = growth_curve(
                    data,
                    mask,
                    footprint,
                    w,
                    h,
                    f.x,
                    f.y,
                    f_ring,
                    f.sky_ring,
                    &mut inside,
                ) {
                    ratios.push(growth / model);
                }
            }
        }
    }
    let n = ratios.len();
    if n < CORRECTION_MIN_RATIOS {
        tracing::warn!(
            count = n,
            skipped = screened,
            fwhm_px = ring_fwhm,
            "aperture correction: fewer than 5 isolated unsaturated stars; the star flux is the model's, uncorrected"
        );
        return (None, n);
    }
    let c = super::robust::median_f64(&mut ratios);
    tracing::debug!(
        count = n,
        skipped = screened,
        aperture_correction = c,
        fwhm_px = ring_fwhm,
        "aperture correction measured"
    );
    (Some(c), n)
}

/// The aperture SNR's radius per star FWHM (PA-11's aperture, unchanged): `r_ap = max(1.5 * fwhm, 3)` px about the
/// fitted centre, the pixels whose centres lie within it.
pub const SNR_APERTURE_PER_FWHM: f64 = 1.5;
/// … and its floor, px.
pub const SNR_APERTURE_MIN_PX: f64 = 3.0;

const _: () = assert!(
    super::seeds::NOISE_CORR_SIDE == 5,
    "ApertureNoise::rho is the noise read's 5 x 5"
);

/// What the aperture SNR needs to know about the plane's pixel noise beyond the star's own ring (Task 11, ruling
/// R11-1).
///
/// `rho` is the plane's noise correlation `rho(L) = C(L) / C(0)` (`rho[dy + 2][dx + 2]`), read ONCE per plane by the
/// matched filter's own noise read (Task 10b A1c; this module never reads or fits it): the variance of a sum over the
/// aperture's pixels is `sigma^2 * sum_{i,j} rho(i - j)` and not `N * sigma^2` once neighbouring pixels share noise
/// (a debayered, resampled or integrated plane). `None` = the pixels are white (a sensor plane the caller stated
/// white: `kappa` is 1, derived, nothing read) or the read failed (then `kappa` is 1 too and the frame line says so,
/// [`super::SnrNoise`]).
///
/// `shot_spread` is `s`: a flux-preserving interpolation (the debayer) spreads each CFA site's shot noise over
/// `1 / share` pixels, so an aperture's shot variance is `s` times the white `S / g` (R 4, G 2, B 4 on an RGGB
/// plane's `share` 0.25 / 0.5 / 0.25; 1 on every plane whose pixels are sites). Both are facts of the plane's
/// provenance, which the caller states.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ApertureNoise {
    pub rho: Option<[[f64; 5]; 5]>,
    pub shot_spread: f64,
}

impl ApertureNoise {
    /// White pixels, one site per pixel: `kappa = 1`, `s = 1`.
    pub const WHITE: ApertureNoise = ApertureNoise {
        rho: None,
        shot_spread: 1.0,
    };
}

impl Default for ApertureNoise {
    fn default() -> Self {
        ApertureNoise::WHITE
    }
}

/// The terms of one star's SNR variance (Task 11, ruling R11-1):
/// `sigma_S^2 = shot_spread * S / g + kappa * n_ap * sigma_sky^2 * (1 + n_ap / n_sky)`, the first term only with a gain
/// (`shot_term`). `s` is the aperture sum `S = sum (v - B_i)`, `n_ap` its present pixels, `(sigma_sky, n_sky)` the
/// star's own stored `(sky_ring_sigma, sky_ring_n)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnrTerms {
    pub s: f64,
    pub n_ap: usize,
    pub sigma_sky: f64,
    pub n_sky: u32,
    pub kappa: f64,
    pub shot_spread: f64,
    pub shot_term: bool,
}

/// `kappa(A) = sum_{i,j in A} rho(i - j) / N_ap` over the aperture's present pixels `pix` (integer pixel coordinates):
/// the variance of the aperture sum of correlated noise in units of its white value `N_ap * sigma^2`. 1 for white
/// pixels (`rho` = 1 at lag 0 and 0 elsewhere); only pairs within the read's reach (2 px) contribute, and a pixel
/// absent from the aperture (masked, non-finite, off the plane) contributes none.
fn aperture_kappa(pix: &[(i64, i64)], rho: &[[f64; 5]; 5]) -> f64 {
    if pix.is_empty() {
        return 1.0;
    }
    let (x0, x1) = pix
        .iter()
        .fold((i64::MAX, i64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (y0, y1) = pix
        .iter()
        .fold((i64::MAX, i64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let gw = (x1 - x0 + 1) as usize;
    let mut grid = vec![false; gw * (y1 - y0 + 1) as usize];
    for &(px, py) in pix {
        grid[(py - y0) as usize * gw + (px - x0) as usize] = true;
    }
    let mut sum = 0.0f64;
    for &(px, py) in pix {
        for dy in -2i64..=2 {
            let qy = py + dy;
            if qy < y0 || qy > y1 {
                continue;
            }
            for dx in -2i64..=2 {
                let qx = px + dx;
                if qx >= x0 && qx <= x1 && grid[(qy - y0) as usize * gw + (qx - x0) as usize] {
                    sum += rho[(dy + 2) as usize][(dx + 2) as usize];
                }
            }
        }
    }
    sum / pix.len() as f64
}

/// A star's aperture SNR (F-6, Task 11, rulings R11-1 and R11-5): `S / sigma_S` over the pixels whose centres lie
/// within `max(1.5 * fwhm, 3)` px of the fitted centre that are present (finite and unmasked), `S = sum (v - B_i)`.
///
/// The sky and its noise are the star's own stored ring (`sky_ring`, `sky_ring_sigma`, `sky_ring_n`; Task 10b): this
/// function estimates neither. `sigma_S^2 = shot_spread * max(S, 0) / g + kappa(A) * N_ap * sigma_i^2 * (1 + N_ap /
/// n_i)`: the star's shot noise (`g` in e- per ADU of the plane's working units; without `gain`, or one that is not a
/// positive number, the term is omitted and [`SnrTerms::shot_term`] says so) and the sky's, the second factor being the
/// uncertainty of B_i itself. `kappa(A)` carries the correlation of the noise inside the aperture
/// ([`ApertureNoise::rho`]; 1 on a white plane); `shot_spread` the interpolation's spreading of the shot noise.
///
/// Recorded limits. (R11-2) the noise read under-reads the correlation on a debayered plane's R and B by 12 to 21 %.
/// (R11-3) on structured sky (nebula) the real flux error is 3.6 to 6 times this noise value: the annulus method's own
/// limit, the structure term was measured out of F-6. (R11-6) a gain from a header that is wrong moves the median
/// star's SNR 4 to 10 %, bright stars up to 64 %.
///
/// NaN (counted by the caller as `snr_missing`) where the star has no ring (`sky_ring` or its sigma not finite, no
/// ring pixel), no present aperture pixel, or a variance that is not positive. A saturated or extended star reads
/// like any other (R11-5): its `S` is the clipped pixels' sum.
pub fn star_snr(
    data: &[f32],
    w: usize,
    h: usize,
    f: &StarFit,
    gain: Option<f64>,
    mask: Option<&[bool]>,
    noise: &ApertureNoise,
) -> (f64, SnrTerms) {
    let gain = gain.filter(|g| g.is_finite() && *g > 0.0);
    let mut terms = SnrTerms {
        s: f64::NAN,
        n_ap: 0,
        sigma_sky: f.sky_ring_sigma,
        n_sky: f.sky_ring_n,
        kappa: 1.0,
        shot_spread: noise.shot_spread,
        shot_term: gain.is_some(),
    };
    let r = (SNR_APERTURE_PER_FWHM * f.fwhm()).max(SNR_APERTURE_MIN_PX);
    if !(f.sky_ring.is_finite()
        && f.sky_ring_sigma.is_finite()
        && f.sky_ring_n > 0
        && f.x.is_finite()
        && f.y.is_finite()
        && r.is_finite()
        && w > 0
        && h > 0)
    {
        return (f64::NAN, terms);
    }
    let r2 = r * r;
    let (y0, y1) = (
        (f.y - r).ceil().max(0.0),
        (f.y + r).floor().min((h - 1) as f64),
    );
    let (x0, x1) = (
        (f.x - r).ceil().max(0.0),
        (f.x + r).floor().min((w - 1) as f64),
    );
    let mut pix: Vec<(i64, i64)> = Vec::new();
    let mut s = 0.0f64;
    if y1 >= y0 && x1 >= x0 {
        for py in y0 as usize..=y1 as usize {
            let dy2 = (py as f64 - f.y).powi(2);
            for px in x0 as usize..=x1 as usize {
                if (px as f64 - f.x).powi(2) + dy2 > r2 {
                    continue;
                }
                let i = py * w + px;
                if data[i].is_finite() && mask.map_or(true, |m| m[i]) {
                    s += data[i] as f64 - f.sky_ring;
                    pix.push((px as i64, py as i64));
                }
            }
        }
    }
    terms.s = s;
    terms.n_ap = pix.len();
    if pix.is_empty() {
        return (f64::NAN, terms);
    }
    if let Some(rho) = &noise.rho {
        terms.kappa = aperture_kappa(&pix, rho);
    }
    let n = pix.len() as f64;
    let sky =
        terms.kappa * n * f.sky_ring_sigma * f.sky_ring_sigma * (1.0 + n / f64::from(f.sky_ring_n));
    let shot = gain.map_or(0.0, |g| noise.shot_spread * s.max(0.0) / g);
    let var = shot + sky;
    if var > 0.0 && var.is_finite() {
        (s / var.sqrt(), terms)
    } else {
        (f64::NAN, terms)
    }
}

/// A star's half-flux radius (F-6, Task 11, ruling R11-5): the distance about the fitted centre inside which the
/// background-subtracted cumulative sum reaches half of the star's `signal` (F-5's corrected flux). The present pixels
/// whose centres lie within `5 * ring_fwhm` ([`GROWTH_RADIUS_PER_FWHM`], the growth curve's aperture) are taken in order
/// of distance (ties in row-major order), summed as `v - B_i` (the star's stored ring sky), and the radius is
/// interpolated linearly between the two bracketing pixels' distances by the share of the last pixel's value it takes.
///
/// NaN where it is not defined: no ring sky, a `signal` that is not a positive number, no ring width, or the `5F`
/// circle holding less than half of the signal. By construction the value is at most `5 * ring_fwhm`. A saturated
/// star's pixels are clipped while its `signal` is the unclipped model's, so its HFR reads wide; an extended one's
/// reads what its pixels hold (R11-5: reported on every accepted fit, the frame median leaves both out).
pub fn hfr(
    data: &[f32],
    w: usize,
    h: usize,
    f: &StarFit,
    ring_fwhm: f64,
    mask: Option<&[bool]>,
) -> f64 {
    let r = GROWTH_RADIUS_PER_FWHM * ring_fwhm;
    let target = 0.5 * f.signal;
    if !(f.sky_ring.is_finite()
        && f.x.is_finite()
        && f.y.is_finite()
        && r.is_finite()
        && r > 0.0
        && target.is_finite()
        && target > 0.0
        && w > 0
        && h > 0)
    {
        return f64::NAN;
    }
    let r2 = r * r;
    let (y0, y1) = (
        (f.y - r).ceil().max(0.0),
        (f.y + r).floor().min((h - 1) as f64),
    );
    let (x0, x1) = (
        (f.x - r).ceil().max(0.0),
        (f.x + r).floor().min((w - 1) as f64),
    );
    if y1 < y0 || x1 < x0 {
        return f64::NAN;
    }
    let mut px: Vec<(f64, f64)> = Vec::new();
    for py in y0 as usize..=y1 as usize {
        let dy2 = (py as f64 - f.y).powi(2);
        for pxl in x0 as usize..=x1 as usize {
            let d2 = (pxl as f64 - f.x).powi(2) + dy2;
            let i = py * w + pxl;
            if d2 <= r2 && data[i].is_finite() && mask.map_or(true, |m| m[i]) {
                px.push((d2, data[i] as f64 - f.sky_ring));
            }
        }
    }
    // A stable sort: pixels at one distance keep their row-major order.
    px.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cum = 0.0f64;
    for (k, &(d2, v)) in px.iter().enumerate() {
        let before = cum;
        cum += v;
        if cum >= target {
            if k == 0 {
                return d2.sqrt();
            }
            let frac = if v != 0.0 { (target - before) / v } else { 0.0 };
            let d_prev = px[k - 1].0.sqrt();
            return d_prev + frac * (d2.sqrt() - d_prev);
        }
    }
    f64::NAN
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
            f64::INFINITY,
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
            f64::INFINITY,
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
        let v = saturation_verdict(
            &region,
            0.0,
            0.0,
            bg,
            2.5,
            f64::INFINITY,
            &plane,
            &params,
            &mut s,
        )
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
        let v = saturation_verdict(
            &region,
            0.0,
            0.0,
            bg,
            2.5,
            f64::INFINITY,
            &plane,
            &params,
            &mut s,
        )
        .expect("a verdict");
        assert!(
            !v.fit && v.saturated && v.by_plateau_clip && !v.by_amplitude,
            "{v:?}"
        );
    }

    /// A pixel-integrated Moffat star (β 2.5, FWHM `fwhm`, amplitude `amp`, centre `(cx, cy)` in stamp coordinates) on
    /// a 41 x 41 stamp at sky 0.04, clipped at 1.0 (Task 10c's fixtures).
    fn clipped_star_region(amp: f64, fwhm: f64, cx: f64, cy: f64) -> Vec<PixelSample> {
        clipped_star_region_on(0.04, amp, fwhm, cx, cy)
    }

    /// [`clipped_star_region`] on a local background `sky` (the plane's own stays 0.04).
    fn clipped_star_region_on(sky: f64, amp: f64, fwhm: f64, cx: f64, cy: f64) -> Vec<PixelSample> {
        let beta = 2.5;
        let alpha = fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt());
        let offsets: Vec<f64> = (0..15).map(|i| (i as f64 + 0.5) / 15.0 - 0.5).collect();
        let mut region = Vec::new();
        for y in -20..=20 {
            for x in -20..=20 {
                let (x, y) = (x as f64, y as f64);
                let mut sum = 0.0;
                for &oy in &offsets {
                    for &ox in &offsets {
                        let r2 = (x + ox - cx).powi(2) + (y + oy - cy).powi(2);
                        sum += (1.0 + r2 / (alpha * alpha)).powf(-beta);
                    }
                }
                region.push(PixelSample {
                    x,
                    y,
                    value: (sky + amp * sum / 225.0).min(1.0),
                });
            }
        }
        region
    }

    /// The plane of the near-clip fixtures: bg 0.04, clip 1.0 (range 0.96), an exact clip (three pixels at the maximum),
    /// so its floor is the clip value itself.
    fn exact_clip_plane() -> SaturationPlane {
        let mut p = SaturationPlane::new(0.04, 0.002, 1.0, 1.0, false).with_max_count(3);
        let (floor, _) = derive_clip_floor(&p, []).expect("a tie is evidence");
        p.clip_floor = Some(floor);
        p
    }

    fn near_clip_verdict(region: &[PixelSample]) -> SaturationVerdict {
        near_clip_verdict_on(region, &exact_clip_plane())
    }

    fn near_clip_verdict_on(region: &[PixelSample], plane: &SaturationPlane) -> SaturationVerdict {
        let mut s = FitScratch::default();
        saturation_verdict(
            region,
            0.0,
            0.0,
            0.04,
            1.8,
            f64::INFINITY,
            plane,
            &FitParams::default(),
            &mut s,
        )
        .expect("a verdict")
    }

    /// Task 10c (R10c-1): an undersampled star (FWHM 1.8 px) clipped on about one pixel has no plateau (its neighbours
    /// sit well under 0.7·range), so clauses 1 and 2 may both miss; clause 3 flags it.
    #[test]
    fn an_undersampled_star_clipped_on_one_pixel_is_flagged_near_clip() {
        let region = clipped_star_region(1.6, 1.8, 0.1, 0.05);
        let v = near_clip_verdict(&region);
        assert!(!v.plateau && v.by_near_clip && v.saturated, "{v:?}");
    }

    /// The test reads the 3x3 maximum, not pixel0: the seed one pixel off the clipped pixel (pixel0 about 0.6·range).
    #[test]
    fn near_clip_reads_the_3x3_maximum() {
        let region = clipped_star_region(1.6, 1.8, 1.2, 0.0);
        let p0 = region
            .iter()
            .find(|s| s.x == 0.0 && s.y == 0.0)
            .expect("pixel0")
            .value;
        assert!(
            (p0 - 0.04) / 0.96 < 0.7 && (p0 - 0.04) / 0.96 > 0.45,
            "pixel0 {p0}"
        );
        let v = near_clip_verdict(&region);
        assert!(!v.plateau && v.by_near_clip && v.saturated, "{v:?}");
    }

    /// A star whose top stands at 0.85·range (below the plane's floor, the exact clip) stays unflagged by the near-clip
    /// clause.
    #[test]
    fn a_star_at_085_of_the_range_is_not_near_clip() {
        let mut region = clipped_star_region(0.9, 2.5, 0.0, 0.0);
        let top = region.iter().map(|s| s.value).fold(0.0, f64::max);
        let k = (0.04 + 0.85 * 0.96 - 0.04) / (top - 0.04);
        for s in region.iter_mut() {
            s.value = 0.04 + (s.value - 0.04) * k;
        }
        // the plane max (range setter) is a brighter star elsewhere: range 0.96
        let v = near_clip_verdict(&region);
        assert!(!v.by_near_clip && !v.by_plateau_clip, "{v:?}");
    }

    // ---- Task 10c (R10c-10): the near-clip floor derived per plane ----

    /// A hand-made verdict for the floor's evidence rules: only the fields the floor and clause 3 read are meaningful.
    fn hand_verdict(
        by_amplitude: bool,
        by_plateau_clip: bool,
        plateau: bool,
        pixel0: f64,
        top3x3: f64,
    ) -> SaturationVerdict {
        SaturationVerdict {
            plateau,
            level: 1.0,
            excluded: 0,
            amplitude: 1.0,
            beta: 3.0,
            fwhm: 3.0,
            saturated: by_amplitude || by_plateau_clip,
            by_amplitude,
            by_plateau_clip,
            by_near_clip: false,
            pixel0,
            top3x3,
            fit: true,
            iterations: 5,
            cross: false,
            centred: true,
        }
    }

    /// Plane of the floor tests: bg 0.04, clip 1.0, range 0.96, in range units `v = bg + u·0.96`.
    fn floor_plane() -> SaturationPlane {
        SaturationPlane::new(0.04, 0.002, 1.0, 1.0, false)
    }

    fn at(u: f64) -> f64 {
        0.04 + u * 0.96
    }

    /// With the floor derived, re-read clause 3 on a copy of `v`.
    fn class_b(plane: &SaturationPlane, mut v: SaturationVerdict) -> bool {
        mark_near_clip(plane, &mut v);
        v.by_near_clip
    }

    /// An exact clip (two or more pixels at the plane max): the floor is the plane max, so a no-plateau star at
    /// 0.95·range (inside the old fixed 0.9 band) is NOT class B, and one at the clip is.
    #[test]
    fn an_exact_clip_puts_the_floor_on_the_plane_max() {
        let mut plane = floor_plane().with_max_count(4);
        let (f, n) = derive_clip_floor(&plane, []).expect("a tie");
        assert_eq!((f, n), (1.0, 1));
        plane.clip_floor = Some(f);
        assert!(!class_b(
            &plane,
            hand_verdict(false, false, false, at(0.7), at(0.95))
        ));
        assert!(class_b(
            &plane,
            hand_verdict(false, false, false, at(0.7), 1.0)
        ));
    }

    /// A clip-free plane (a unique maximum; the brightest star at 0.9-0.99 of it flagged by one clause only) shows no
    /// evidence: no floor, no class B whatever the star.
    #[test]
    fn a_clip_free_plane_has_no_floor_and_no_class_b() {
        let plane = floor_plane();
        let verdicts = [
            hand_verdict(true, false, false, at(0.9), at(0.99)),
            hand_verdict(false, true, true, at(0.93), at(0.95)),
            hand_verdict(false, false, false, at(0.7), at(0.97)),
        ];
        assert_eq!(derive_clip_floor(&plane, &verdicts), None);
        for v in verdicts {
            assert!(!class_b(&plane, v), "{v:?}");
        }
    }

    /// A flat-divided clip: certified cores (clauses 1 AND 2) spread over 0.97-1.03·range, so the floor is the lowest
    /// pixel0; a one-pixel clip at 0.98 is class B, an unclipped no-plateau star at 0.93 is not.
    #[test]
    fn a_flat_divided_clip_spans_its_certified_cores() {
        let mut plane = floor_plane();
        plane.range = 1.04 * 0.96;
        let cores = [
            hand_verdict(true, true, true, at(1.03), at(1.03)),
            hand_verdict(true, true, true, at(0.97), at(0.98)),
            hand_verdict(true, true, true, at(1.0), at(1.01)),
        ];
        let (f, n) = derive_clip_floor(&plane, &cores).expect("three cores");
        assert_eq!(n, 3);
        assert!((f - at(0.97)).abs() < 1e-12, "{f}");
        plane.clip_floor = Some(f);
        assert!(class_b(
            &plane,
            hand_verdict(false, false, false, at(0.5), at(0.98))
        ));
        assert!(!class_b(
            &plane,
            hand_verdict(false, false, false, at(0.5), at(0.93))
        ));
    }

    /// An unclipped wide star (F 7.5) passes the plateau test with a fitted amplitude under the range: clause 2 alone,
    /// not clause 1, so it certifies nothing and the plane has no floor.
    #[test]
    fn an_unclipped_wide_plateau_star_sets_no_floor() {
        let plane = floor_plane();
        let region = clipped_star_region(0.93 * 0.96, 7.5, 0.0, 0.0);
        let v = near_clip_verdict_on(&region, &plane);
        assert!(v.plateau && v.by_plateau_clip && !v.by_amplitude, "{v:?}");
        assert_eq!(derive_clip_floor(&plane, [&v]), None);
    }

    /// A certified core whose 3x3 maximum overshoots pixel0 (debayer overshoot): the floor is pixel0, not the maximum.
    #[test]
    fn the_floor_reads_pixel0_not_the_3x3_maximum() {
        let plane = floor_plane();
        let core = hand_verdict(true, true, true, at(0.96), at(1.07));
        let (f, _) = derive_clip_floor(&plane, [&core]).expect("a core");
        assert!((f - at(0.96)).abs() < 1e-12, "{f}");
    }

    /// The pins: a floor is never below `bg + 0.9·range` (class B is inside the knot exemption's band), the UNTESTED
    /// plane has none, and a plane without evidence flags no class B.
    #[test]
    fn floor_pins() {
        let plane = floor_plane().with_max_count(2);
        let certified = hand_verdict(true, true, true, at(0.9), at(0.9));
        let (f, _) = derive_clip_floor(&plane, [&certified]).expect("evidence");
        assert!(f >= plane.bg + 0.9 * plane.range - 1e-12, "{f}");
        let (f, _) = derive_clip_floor(&plane, []).expect("tie");
        assert!(f >= plane.bg + 0.9 * plane.range - 1e-12, "{f}");
        assert_eq!(
            derive_clip_floor(&SaturationPlane::UNTESTED.with_max_count(5), [&certified]),
            None
        );
        let empty = floor_plane();
        assert_eq!(empty.clip_floor, None);
        assert!(!class_b(
            &empty,
            hand_verdict(false, false, false, at(0.9), 1.0)
        ));
    }

    /// I-1: clause 3 on its own. A sharp star on a RAISED local background, clipped on one or two pixels: the fitted
    /// amplitude (above the local sky) stays under the range and no plateau forms, so clauses 1 and 2 both miss and
    /// only the near-clip clause flags it. The lab has 27 such stars on neb3_4096 (the raised-background family) and
    /// 0 on any flat-sky family: the scan over flat sky below finds what it finds.
    #[test]
    fn a_clipped_star_on_a_raised_background_is_flagged_by_clause_3_alone() {
        let plane = exact_clip_plane();
        let mut found = Vec::new();
        for sky in [0.25, 0.35, 0.45] {
            for amp in [0.6, 0.7, 0.8] {
                for (cx, cy) in [(0.0, 0.0), (0.2, 0.1), (0.35, 0.3)] {
                    let region = clipped_star_region_on(sky, amp, 1.8, cx, cy);
                    let v = near_clip_verdict_on(&region, &plane);
                    if v.by_near_clip && !v.by_amplitude && !v.by_plateau_clip {
                        found.push((sky, amp, cx, cy));
                    }
                }
            }
        }
        assert!(
            !found.is_empty(),
            "no raised-background star isolates clause 3"
        );
    }

    /// The review's flat-sky scan (amplitude 1.0-1.3 × sub-pixel offsets, sky 0.04, FWHM 1.8): records how many stars
    /// only clause 3 flags there. The lab's count on its flat-sky families is 0 (27 on neb3_4096).
    #[test]
    fn the_flat_sky_scan_of_clause_3_alone() {
        let plane = exact_clip_plane();
        let mut only3 = 0;
        for amp in [1.0, 1.1, 1.2, 1.3] {
            for (cx, cy) in [(0.0, 0.0), (0.1, 0.05), (0.25, 0.2), (0.4, 0.4), (0.5, 0.5)] {
                let v = near_clip_verdict_on(&clipped_star_region(amp, 1.8, cx, cy), &plane);
                if v.by_near_clip && !v.by_amplitude && !v.by_plateau_clip {
                    only3 += 1;
                }
            }
        }
        assert_eq!(only3, 0, "flat sky: clause 3 never flags alone");
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
                sky_ring: f64::NAN,
                sky_ring_sigma: f64::NAN,
                sky_ring_n: 0,
                snr: f64::NAN,
                hfr: f64::NAN,
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

    /// A pin (C0's qualifying rules; rim and edge by owner ruling R6; the rim read by `ring_sky` and handed in as B_i
    /// from Task 10b, ruling B2): the growth-curve flux of a lone round Gaussian star is its total within its `5F` disc,
    /// measured against the `[10F, 12F]` ring mean; the `12F` disc leaving the plane, one absent pixel within `5F` or a
    /// missing B_i refuses it; an absent ring pixel is only left out of B_i.
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
        let b = ring_sky(&data, None, None, &none, w, h, 60.3, 59.6, fwhm)
            .unwrap()
            .mean;
        assert_eq!(b, 50.0, "the far ring is flat sky");
        let g = growth_curve(&data, None, None, w, h, 60.3, 59.6, fwhm, b, &mut scratch).unwrap();
        assert!((g / total - 1.0).abs() < 1e-3, "{g} vs {total}");
        // The 12F disc (56.5 px) leaves a 120-px plane from a centre 30 px off its edge.
        assert_eq!(
            growth_curve(&data, None, None, w, h, 30.0, 60.0, fwhm, b, &mut scratch),
            None
        );
        // A missing B_i (Task 10b): no qualifier.
        assert_eq!(
            growth_curve(
                &data,
                None,
                None,
                w,
                h,
                60.3,
                59.6,
                fwhm,
                f64::NAN,
                &mut scratch
            ),
            None
        );
        let mut holed = data.clone();
        holed[62 * w + 61] = f32::NAN; // 2.5 px from the centre, inside 5F (23.5 px)
        assert_eq!(
            growth_curve(&holed, None, None, w, h, 60.3, 59.6, fwhm, b, &mut scratch),
            None
        );
        let mut mask = vec![true; w * h];
        mask[62 * w + 61] = false;
        assert_eq!(
            growth_curve(
                &data,
                Some(&mask),
                None,
                w,
                h,
                60.3,
                59.6,
                fwhm,
                b,
                &mut scratch
            ),
            None
        );
        let mut rim_hole = data.clone();
        rim_hole[59 * w + (60 + 52)] = f32::NAN; // 51.7 px out: in the [10F, 12F] = [47.1, 56.5] rim
        let b2 = ring_sky(&rim_hole, None, None, &none, w, h, 60.3, 59.6, fwhm)
            .unwrap()
            .mean;
        let g2 = growth_curve(
            &rim_hole,
            None,
            None,
            w,
            h,
            60.3,
            59.6,
            fwhm,
            b2,
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

    /// R6 (owner ruling 2026-10-11): a neighbour inside the `[10F, 12F]` ring leaves the ring mean B_i where it is when
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
        let b_lone = ring_sky(&lone, None, None, &own, w, h, x, y, fwhm)
            .unwrap()
            .mean;
        let b_masked = ring_sky(&pair, None, None, &both, w, h, x, y, fwhm)
            .unwrap()
            .mean;
        assert!(
            (b_masked - b_lone).abs() < 1e-9,
            "seeded neighbour: b {b_masked} vs {b_lone}"
        );
        let b_open = ring_sky(&pair, None, None, &own, w, h, x, y, fwhm)
            .unwrap()
            .mean;
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
            let g = growth_curve(&data, None, None, w, h, x, y, fwhm, 50.0, &mut scratch);
            assert_eq!(g.is_some(), measured, "({x}, {y}): {g:?}");
        }
    }

    /// F-5 round 4: the caller's footprint holds the whole `12F` disc like the plane edge — one pixel outside the
    /// footprint 56 px from the centre (inside `12F` = 56.5 px) refuses the star, one 57 px from it does not, and the
    /// measured star's growth is that of the call without a footprint (the footprint is read, never summed).
    #[test]
    fn the_growth_curve_refuses_a_star_whose_12f_disc_leaves_the_footprint() {
        let (w, h) = (160usize, 160usize);
        let sigma = 2.0;
        let fwhm = sigma * 2.354_820_045; // 12F = 56.5 px
        let (x, y) = (80.0, 80.0);
        let data = gaussian_field(w, h, &[(x, y, 1000.0)], sigma, 50.0);
        let mut scratch = Vec::new();
        let bare = growth_curve(&data, None, None, w, h, x, y, fwhm, 50.0, &mut scratch).unwrap();
        for (px, measured) in [(136usize, false), (137, true)] {
            let mut footprint = vec![true; w * h];
            footprint[80 * w + px] = false;
            let g = growth_curve(
                &data,
                None,
                Some(&footprint),
                w,
                h,
                x,
                y,
                fwhm,
                50.0,
                &mut scratch,
            );
            assert_eq!(g.is_some(), measured, "outside at x = {px}: {g:?}");
            if let Some(g) = g {
                assert_eq!(g, bare, "the footprint changes no sum");
            }
        }
    }

    /// Task 10b (ruling B2): `ring_sky` reads exactly the pixels a plain scan of the plane finds in the ring — centres at
    /// `10F ≤ d ≤ 12F`, present (finite, unmasked, inside the footprint) and outside every seed's `3F` disc — summed in
    /// row-major order, so B_i is that scan's mean bit for bit (the inner circle it never visits is not in the ring);
    /// σ_i is their RMS about B_i; a star near a corner reads its partial ring; under 8 pixels there is none.
    #[test]
    fn ring_sky_is_the_plain_scan_of_the_ring_pixels() {
        use crate::test_support::add_noise;
        let (w, h) = (150usize, 130usize);
        let mut data = gaussian_field(w, h, &[(70.4, 61.7, 900.0), (101.0, 95.0, 700.0)], 1.6, 0.0);
        for v in data.iter_mut() {
            *v += 3000.0;
        }
        add_noise(&mut data, 12.0, 41);
        data[17 * w + 33] = f32::NAN;
        let mut mask = vec![true; w * h];
        mask[60 * w + 120] = false;
        let mut footprint = vec![true; w * h];
        footprint[100 * w + 30] = false;
        let seeds = [seed_at(70.4, 61.7), seed_at(101.0, 95.0)];
        for &(x, y, fwhm) in &[
            (70.4, 61.7, 4.1),
            (66.25, 58.5, 3.3),
            (8.6, 120.2, 2.7),
            (140.0, 4.0, 1.9),
        ] {
            let sm = SeedMask::new(w, h, &seeds, GROWTH_RIM_SEED_MASK_PER_FWHM * fwhm);
            let r_in2 = (GROWTH_RIM_INNER_PER_FWHM * fwhm).powi(2);
            let r_out2 = (GROWTH_RIM_OUTER_PER_FWHM * fwhm).powi(2);
            let mut picked: Vec<f64> = Vec::new();
            for py in 0..h {
                let dy2 = (py as f64 - y).powi(2);
                for px in 0..w {
                    let d2 = (px as f64 - x).powi(2) + dy2;
                    let i = py * w + px;
                    if d2 >= r_in2
                        && d2 <= r_out2
                        && !sm.covers(i)
                        && data[i].is_finite()
                        && mask[i]
                        && footprint[i]
                    {
                        picked.push(data[i] as f64);
                    }
                }
            }
            let got =
                ring_sky(&data, Some(&mask), Some(&footprint), &sm, w, h, x, y, fwhm).unwrap();
            assert_eq!(got.n as usize, picked.len(), "({x}, {y}) F {fwhm}");
            let mean = picked.iter().sum::<f64>() / picked.len() as f64;
            assert_eq!(
                got.mean, mean,
                "({x}, {y}) F {fwhm}: the scan's mean, bit for bit"
            );
            let rms = (picked.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                / picked.len() as f64)
                .sqrt();
            assert!(
                (got.sigma / rms - 1.0).abs() < 1e-9,
                "σ_i {} vs {rms}",
                got.sigma
            );
        }
        let none = SeedMask::new(w, h, &[], 0.0);
        assert_eq!(
            ring_sky(&data, None, None, &none, w, h, 70.0, 60.0, 0.1),
            None,
            "4 ring pixels: under 8"
        );
    }
}

/// F-6 (Task 11): the aperture SNR and the half-flux radius, on synthetic planes whose truth is analytic.
#[cfg(test)]
mod snr_hfr_tests {
    use super::*;
    use crate::star_measure::tests::median;
    use crate::star_measure::{
        fit_stars_at, measure_plane, FitStarsOptions, MeasureOptions, PixelNoise, SeedSpec,
        SnrNoise, ADU_SCALE,
    };
    use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

    /// The FWHM of a β = 4 Moffat of alpha 2.99 (the ring width the tests hand the fit, so the ring's pixel count is
    /// the same on every draw).
    fn f_true() -> f64 {
        2.0 * 2.99 * (2f64.powf(0.25) - 1.0).sqrt()
    }

    fn star(amp: f64) -> (MoffatStar, Seed) {
        let s = MoffatStar {
            x: 40.3,
            y: 39.7,
            amp,
            alpha_x: 2.99,
            alpha_y: 2.99,
            theta: 0.0,
        };
        let seed = Seed {
            x: s.x,
            y: s.y,
            peak: amp * ADU_SCALE as f64,
            flux: 1.0,
        };
        (s, seed)
    }

    fn scaled(v: &[f32]) -> Vec<f32> {
        v.iter().map(|x| x * ADU_SCALE).collect()
    }

    #[test]
    fn snr_matches_the_analytic_value_on_a_white_noise_star() {
        // sky 0.05, g 50000 e- per calibrated unit, read noise 0.0005: sigma_px = sqrt(0.05 / g + 0.0005^2) = 0.001118.
        // The analytic SNR on the star's own ring (R6: 10F..12F, 929 pixel centres at F = 2.6012, a count made by hand;
        // PA-11's annulus would give 168.03) and aperture (r_ap = 1.5 F = 3.902, 48 pixel centres): 178.97.
        let g = 50000.0;
        let (s, seed) = star(0.2);
        let clean = moffat_field(80, 80, &[s], 4.0, 0.05);
        let total = std::f64::consts::PI * 0.2 * 2.99 * 2.99 / 3.0;
        let sigma_px = (0.05 / g + 0.0005f64.powi(2)).sqrt();
        // the ring's pixel centres at 10F..12F about the exact centre, counted by hand
        let n_ring = (0..80usize)
            .flat_map(|y| (0..80usize).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let d = (x as f64 - s.x).hypot(y as f64 - s.y);
                d >= 10.0 * f_true() && d <= 12.0 * f_true()
            })
            .count();
        assert_eq!(n_ring, 929);
        let (n_ap, n_ring) = (48.0, n_ring as f64);
        let r_ap = 1.5 * f_true();
        let s_ap = total * (1.0 - (1.0 + (r_ap / 2.99).powi(2)).powi(-3));
        let analytic =
            s_ap / (s_ap / g + n_ap * sigma_px * sigma_px * (1.0 + n_ap / n_ring)).sqrt();
        assert!(
            (analytic - 178.97).abs() < 0.05,
            "the hand value {analytic}"
        );
        let opts = FitStarsOptions {
            gain_e_per_working_adu: Some(g / ADU_SCALE as f64),
            ring_fwhm: Some(f_true()),
            ..Default::default()
        };
        let mut snrs = Vec::new();
        for k in 0..20 {
            let mut noisy = clean.clone();
            add_noise(&mut noisy, sigma_px as f32, k);
            let data = scaled(&noisy);
            let out = fit_stars_at(&data, 80, 80, &[seed], 4.0, &opts, None);
            assert_eq!(out.fits.len(), 1, "draw {k}");
            let f = out.fits[0];
            assert!(
                f.sky_ring_n.abs_diff(929) <= 4,
                "the ring's pixel count (929 about the exact centre, hand count), draw {k}: {}",
                f.sky_ring_n
            );
            let (snr, terms) = star_snr(
                &data,
                80,
                80,
                &f,
                opts.gain_e_per_working_adu,
                None,
                &ApertureNoise::WHITE,
            );
            assert_eq!(snr, f.snr, "the fit carries the same value");
            // 48 pixel centres at the exact centre and width; the fit's own centre and FWHM move one by a pixel
            assert!(terms.n_ap.abs_diff(48) <= 1, "draw {k}: {}", terms.n_ap);
            assert!(terms.shot_term && terms.kappa == 1.0 && terms.shot_spread == 1.0);
            snrs.push(f.snr);
        }
        let med = median(snrs.iter().cloned());
        assert!(
            (med / analytic - 1.0).abs() < 0.02,
            "snr {med} vs analytic {analytic}"
        );
    }

    #[test]
    fn snr_without_gain_omits_the_shot_term_and_says_so() {
        let (s, seed) = star(0.2);
        let mut clean = moffat_field(80, 80, &[s], 4.0, 0.05);
        add_noise(&mut clean, 0.002, 5);
        let data = scaled(&clean);
        let with = fit_stars_at(
            &data,
            80,
            80,
            &[seed],
            4.0,
            &FitStarsOptions {
                gain_e_per_working_adu: Some(1.0),
                ..Default::default()
            },
            None,
        );
        let without = fit_stars_at(
            &data,
            80,
            80,
            &[seed],
            4.0,
            &FitStarsOptions::default(),
            None,
        );
        assert!(with.fits[0].snr.is_finite() && without.fits[0].snr.is_finite());
        assert!(without.fits[0].snr > with.fits[0].snr);
        let (_, terms) = star_snr(
            &data,
            80,
            80,
            &without.fits[0],
            None,
            None,
            &ApertureNoise::WHITE,
        );
        assert!(!terms.shot_term);
        let (_, terms) = star_snr(
            &data,
            80,
            80,
            &with.fits[0],
            Some(1.0),
            None,
            &ApertureNoise::WHITE,
        );
        assert!(terms.shot_term);
        // a gain that is not a positive number is no gain
        let (snr, terms) = star_snr(
            &data,
            80,
            80,
            &with.fits[0],
            Some(f64::NAN),
            None,
            &ApertureNoise::WHITE,
        );
        assert!(!terms.shot_term && snr == without.fits[0].snr);
    }

    #[test]
    fn edge_star_snr_reads_the_stars_own_ring() {
        // a star 10 px from the border (a closer one is refused `outsideImage` by its fit region) with a NaN wedge beside
        // it: its ring holds what the plane gives it - the border and the wedge cut it - and the SNR reads exactly those
        // pixels' sigma and count
        let s = MoffatStar {
            x: 10.0,
            y: 40.0,
            amp: 0.3,
            alpha_x: 2.99,
            alpha_y: 2.99,
            theta: 0.0,
        };
        let mut data = moffat_field(80, 80, &[s], 4.0, 0.05);
        add_noise(&mut data, 0.002, 3);
        for y in 50..80 {
            for x in 0..20 {
                data[y * 80 + x] = f32::NAN;
            }
        }
        let data = scaled(&data);
        let seed = Seed {
            x: 10.0,
            y: 40.0,
            peak: 0.3 * ADU_SCALE as f64,
            flux: 1.0,
        };
        let opts = FitStarsOptions {
            ring_fwhm: Some(f_true()),
            ..Default::default()
        };
        let out = fit_stars_at(&data, 80, 80, &[seed], 4.0, &opts, None);
        assert_eq!(out.fits.len(), 1, "the edge star fits: {:?}", out.refused);
        let f = out.fits[0];
        let (snr, terms) = star_snr(&data, 80, 80, &f, None, None, &ApertureNoise::WHITE);
        assert!(snr.is_finite() && snr > 0.0, "{snr}");
        assert_eq!(terms.sigma_sky, f.sky_ring_sigma);
        assert_eq!(terms.n_sky, f.sky_ring_n);
        assert!(
            f64::from(terms.n_sky) < 0.6 * 929.0,
            "the border and the wedge cut the ring: {}",
            terms.n_sky
        );
        assert!(terms.n_sky >= ANNULUS_MIN_PIXELS as u32);
    }

    #[test]
    fn snr_and_hfr_are_nan_and_counted_when_the_ring_is_missing() {
        // only the star's own 10F..12F ring (0.5 px wider on each side) is absent: the star itself fits, and the SNR
        // and the HFR alone have no sky to read. The open run reads both, so the test cannot pass on zero fits.
        let (s, seed) = star(0.2);
        // an unseeded brighter star sets the plane's range: a lone star would be flagged saturated by F-3's amplitude
        // clause (its fitted amplitude tops the plane max less the sky), leave the frame statistic empty and the plane
        // without a ring width (R11-5)
        let range = MoffatStar {
            x: 100.0,
            y: 40.0,
            amp: 0.45,
            ..s
        };
        let mut data = moffat_field(120, 80, &[s, range], 4.0, 0.05);
        add_noise(&mut data, 0.002, 9);
        let opts = |mask: Option<Vec<bool>>| MeasureOptions {
            seeds: SeedSpec::Given(vec![seed]),
            mask,
            pixel_noise: PixelNoise::White,
            ..Default::default()
        };
        let open = measure_plane(&data, 120, 80, &opts(None), None);
        assert_eq!(open.stars.len(), 1);
        assert!(open.stars[0].snr.is_finite(), "snr {}", open.stars[0].snr);
        assert!(open.stars[0].hfr.is_finite(), "hfr {}", open.stars[0].hfr);
        assert_eq!(open.snr_missing, 0);
        assert!(open.frame.snr_median.is_some() && open.frame.hfr_median.is_some());
        let big_f = open.frame.fwhm_px.expect("the frame FWHM");
        let f = open.stars[0];
        let (lo, hi) = (10.0 * big_f - 0.5, 12.0 * big_f + 0.5);
        let mask: Vec<bool> = (0..120 * 80)
            .map(|i| {
                let d = ((i % 120) as f64 - f.x).hypot((i / 120) as f64 - f.y);
                d < lo || d > hi
            })
            .collect();
        let m = measure_plane(&data, 120, 80, &opts(Some(mask)), None);
        assert_eq!(m.stars.len(), 1, "the star itself is unmasked and fits");
        assert!(m.stars[0].snr.is_nan() && m.stars[0].hfr.is_nan());
        assert_eq!(m.snr_missing, 1);
        assert!(m.frame.snr_median.is_none() && m.frame.hfr_median.is_none());
        assert_eq!(m.snr_noise, SnrNoise::White);
    }

    /// Six isolated Gaussian stars (sigma 2, phase (0.3, 0.7)) fitted at beta 100: `c` is measured on six stars and
    /// `signal` is the total flux, so the HFR is the Gaussian's half-flux radius up to the pixel grid.
    fn gaussian_stars() -> (Vec<f32>, crate::star_measure::FitOutcome) {
        let sigma = 2.0;
        let stars: Vec<(f64, f64, f64)> = (0..6)
            .map(|i| {
                (
                    60.3 + (i % 3) as f64 * 60.0,
                    59.7 + (i / 3) as f64 * 60.0,
                    0.3,
                )
            })
            .collect();
        // an unseeded brighter star sets the plane's range (the clip screen), outside every seeded star's 12F disc
        let mut rendered = stars.clone();
        rendered.push((120.0, 290.0, 0.45));
        let g = gaussian_field(240, 330, &rendered, sigma, 0.0);
        let data = scaled(&g);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|&(x, y, p)| Seed {
                x,
                y,
                peak: p,
                flux: 1.0,
            })
            .collect();
        let out = fit_stars_at(
            &data,
            240,
            330,
            &seeds,
            100.0,
            &FitStarsOptions::default(),
            None,
        );
        (data, out)
    }

    #[test]
    fn hfr_of_a_gaussian_is_the_half_flux_radius() {
        // six isolated stars, so the aperture correction is measured (>= 5 qualify, Task 10) and `signal` is the total
        // flux; one star would leave c = 1 and signal = 0.898 * total, the HFR 9 % low (computed). The phase (0.3, 0.7)
        // matters: this interpolated discrete HFR moves with the sub-pixel phase (sigma 2: up to +-8 % over a 0.1-px
        // phase grid, computed); at this phase it reads +0.2 % with c measured.
        let (_, out) = gaussian_stars();
        assert!(
            out.aperture_correction.is_some(),
            "c is measured on six isolated stars"
        );
        let want = 1.177_41 * 2.0; // the Gaussian's half-flux radius sqrt(2 ln 2) sigma
        assert_eq!(out.fits.len(), 6);
        let ring = out.ring_fwhm.expect("a ring width");
        for f in &out.fits {
            assert!((f.hfr / want - 1.0).abs() < 0.03, "hfr {} vs {want}", f.hfr);
            assert!(f.hfr <= 5.0 * ring, "HFR is at most 5F by construction");
        }
    }

    #[test]
    fn hfr_is_nan_when_the_5f_circle_holds_less_than_half_the_signal() {
        let (data, out) = gaussian_stars();
        let ring = out.ring_fwhm.expect("a ring width");
        let mut f = out.fits[0];
        assert!(hfr(&data, 240, 330, &f, ring, None).is_finite());
        f.signal *= 2.5; // half of 2.5 x the flux is more than the circle holds
        assert!(hfr(&data, 240, 330, &f, ring, None).is_nan());
        f.signal = out.fits[0].signal;
        f.sky_ring = f64::NAN; // no ring sky
        assert!(hfr(&data, 240, 330, &f, ring, None).is_nan());
        assert!(
            hfr(&data, 240, 330, &out.fits[0], f64::NAN, None).is_nan(),
            "no ring width"
        );
    }

    /// 512 x 512 white noise smoothed by the separable [1, 2, 1] / 4 (a correlated plane: lag-1 rho 2/3 per axis, lag-2
    /// 1/6) and one star, measured with the seed given and the noise stated mixed.
    fn smoothed_plane() -> (Vec<f32>, Seed) {
        let n = 512usize;
        let mut noise = vec![0.0f32; n * n];
        add_noise(&mut noise, 0.004, 21);
        let at = |x: i64, y: i64| {
            noise[y.clamp(0, n as i64 - 1) as usize * n + x.clamp(0, n as i64 - 1) as usize]
        };
        let k = [0.25f32, 0.5, 0.25];
        let mut rows = vec![0.0f32; n * n];
        for y in 0..n as i64 {
            for x in 0..n as i64 {
                rows[y as usize * n + x as usize] =
                    (0..3).map(|j| k[j] * at(x + j as i64 - 1, y)).sum();
            }
        }
        let at2 = |x: i64, y: i64| {
            rows[y.clamp(0, n as i64 - 1) as usize * n + x.clamp(0, n as i64 - 1) as usize]
        };
        let mut sm = vec![0.0f32; n * n];
        for y in 0..n as i64 {
            for x in 0..n as i64 {
                sm[y as usize * n + x as usize] =
                    (0..3).map(|j| k[j] * at2(x, y + j as i64 - 1)).sum();
            }
        }
        let s = MoffatStar {
            x: 256.3,
            y: 255.7,
            amp: 0.3,
            alpha_x: 2.99,
            alpha_y: 2.99,
            theta: 0.0,
        };
        // an unseeded brighter star sets the plane's range (R11-5: a lone star flags itself saturated)
        let range = MoffatStar {
            x: 100.0,
            y: 100.0,
            amp: 0.6,
            ..s
        };
        let sky = moffat_field(n, n, &[s, range], 4.0, 0.05);
        // the native (unit-domain) plane: `measure_plane` scales it by `ADU_SCALE` itself
        let data: Vec<f32> = sky.iter().zip(&sm).map(|(a, b)| a + b).collect();
        (
            data,
            Seed {
                x: s.x,
                y: s.y,
                peak: 0.3 * ADU_SCALE as f64,
                flux: 1.0,
            },
        )
    }

    #[test]
    fn snr_on_a_mixed_plane_reads_the_aperture_correlation() {
        let (native, seed) = smoothed_plane();
        let data = scaled(&native); // the working-unit plane the fits and the aperture read
        let n = 512usize;
        let pm = measure_plane(
            &native,
            n,
            n,
            &MeasureOptions {
                seeds: SeedSpec::Given(vec![seed]),
                pixel_noise: PixelNoise::Mixed,
                ..Default::default()
            },
            None,
        );
        assert_eq!(
            pm.snr_noise,
            SnrNoise::Read,
            "the correlation is read once for a given-seed mixed plane"
        );
        assert_eq!(pm.stars.len(), 1);
        let f = pm.stars[0];
        // the same plane through a white aperture: the ratio is sqrt(kappa) of the star's own aperture
        let (white, wt) = star_snr(&data, n, n, &f, None, None, &ApertureNoise::WHITE);
        assert_eq!(wt.kappa, 1.0);
        assert_eq!(wt.shot_spread, 1.0);
        let (mixed, mt) = (f.snr, {
            let rho = crate::star_measure::seeds::read_noise_correlation(
                &data,
                n,
                n,
                None,
                &crate::star_measure::domain::background_mesh(&data, n, n, None, true),
            )
                .expect("the read")
                .rho();
            star_snr(
                &data,
                n,
                n,
                &f,
                None,
                None,
                &ApertureNoise {
                    rho: Some(rho),
                    shot_spread: 1.0,
                },
            )
            .1
        });
        assert!(
            (white / mixed - mt.kappa.sqrt()).abs() < 1e-9 * mt.kappa.sqrt(),
            "{white} / {mixed} vs {}",
            mt.kappa
        );
        // kappa analytic over the star's aperture pixels: rho(dx, dy) = r(dx) r(dy), r = 1, 2/3, 1/6
        let r = |d: i64| match d.abs() {
            0 => 1.0,
            1 => 2.0 / 3.0,
            2 => 1.0 / 6.0,
            _ => 0.0,
        };
        let r_ap = (1.5 * f.fwhm()).max(3.0);
        let mut pix = Vec::new();
        for y in 0..n as i64 {
            for x in 0..n as i64 {
                if ((x as f64 - f.x).powi(2) + (y as f64 - f.y).powi(2)) <= r_ap * r_ap
                    && (x - 256).abs() < 8
                    && (y - 256).abs() < 8
                {
                    pix.push((x, y));
                }
            }
        }
        let mut analytic = 0.0;
        for &(x, y) in &pix {
            for &(x2, y2) in &pix {
                analytic += r(x - x2) * r(y - y2);
            }
        }
        analytic /= pix.len() as f64;
        assert_eq!(pix.len(), mt.n_ap);
        assert!(analytic > 4.5 && analytic < 7.2, "{analytic}");
        assert!(
            (mt.kappa / analytic - 1.0).abs() < 0.03,
            "kappa read {} vs analytic {analytic}",
            mt.kappa
        );
    }
}
