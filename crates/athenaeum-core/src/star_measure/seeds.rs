//! The seeds (spec 2026-10-07 §2.1 `seeds`, §3 F-1; Plan A Task 6a): star detection by a two-pass Gaussian matched
//! filter at the field width, its threshold in units of σ_f, the propagated detection noise (A1), the selection by the
//! filter's integrated signal, and the field width itself: the mode of the candidates' azimuthal half-maximum widths
//! (PA-17; Task 10b A2), which [`field_width_of`] also gives seeds another detector handed in (I-4).
//!
//! Task 6a changed no fit (ruling F16). From Task 6b the field width also sizes every star's width-start box in the fit
//! (radius `⌈2·F_field⌉`, PA-12): the matched filter's own for its seeds, [`field_width_of`] for seeds handed in.
//!
//! Task 10b phase A1 (F-10): the threshold's σ_f is PROPAGATED — the plane's one mesh's noise map × the noise's
//! filtered-to-pixel gain ([`detection_noise`]) — not measured on the filtered plane, and two knot tests at the field
//! width ([`knot_verdict`]) drop structure seeds and flag extended ones ([`DetectionReport`]). Phase A2b: their bounds
//! sit at the field's star-width envelope ([`WidthEnvelope`]), the field width plus 3.09 × the stars' width spread.
//!
//! **The filter.** It runs on the plane minus its mesh background (the 128-px model the noise residual uses). Every
//! absent pixel — non-finite, or `false` in the caller's mask — counts zero and leaves the normalisation: per position
//! `num = Σ k·d` and `den = Σ k²` over the present pixels, `k` the unnormalised Gaussian at the field width, each a
//! separable convolution. `f = num/√den` is the matched filter's signal in units of the pixel σ, with the same noise
//! wherever data is present (a registered frame's NaN border neither seeds nor dims its neighbours), and
//! `flux = 2πσ_F²·num/den` is the least-squares flux of a Gaussian star of the field width at that position: the
//! selection key.
//!
//! **The super-pixel pass.** At a field width ≥ 6 px the second pass runs on 2×2 blocks: a block carries the sum of its
//! present pixels and their count, so `num = Σ k·sum` and `den = Σ k²·count` are the same least-squares terms on a
//! coarser grid (the kernel halved in cells) at a quarter of the cost.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;
use tracing::{debug, warn};

use super::beta::BETA_BOUNDS;
use super::domain::{self, BackgroundMesh, NoiseModel, NoiseSource};
use super::fit::{Seed, WIDTH_START_MIN_PEAK_SNR};
use super::photometry::SaturationPlane;
use super::robust::median_f64;
use super::PixelNoise;
use crate::integration::stats::{for_each_stratified, mad_about, median_in_place, MAD_TO_SIGMA};

/// `2√(2 ln 2)`: a Gaussian's FWHM per σ.
const GAUSSIAN_FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949_3;

/// The matched filter's threshold in units of the detection noise σ_f (ruling R-OSP-2): the filtered plane's measured
/// σ through Task 10, the propagated σ_f(x) from Task 10b (ruling A1; K unchanged, the two agree on a structure-free
/// field — D2: 1.00–1.04).
///
/// Chosen by PA-2 (Plan A Task 6a) before any fitted bar was read. The rule: the smallest K of {3.0, 3.5, 4.0, 4.5, 5.0,
/// 6.0} at which every `twins/*` family (46, run whole) and `artefacts` read ≤ 0.5 false detections per Mpx of cause
/// `nothing` on the `seeds` path; the `background/*` families are reported only. The table (athenaeum-lab
/// `star-lab/results/plan-a/t06a-seeds/threshold-choice.txt`; star_probe 7e3289d7 built from athenaeum 71cac35c):
///
/// | K | deciding cells over 0.5 | worst deciding cell, `nothing` / Mpx |
/// | ---- | ---- | ---- |
/// | 3.0 | 45 of 47 | `twins/sharp/gauss/f1.2` 1001.8 |
/// | 3.5 | 41 | `twins/sharp/m10/f1.2` 196.8 |
/// | 4.0 | 40 | `twins/sharp/m10/f1.2` 32.2 (`artefacts` 10.0) |
/// | 4.5 | 29 | `twins/sharp/blur1/f1.2` 3.42 (`artefacts` 1.19) |
/// | 5.0 | 1 | `twins/sharp/blur2.5/f1.2` 0.56 |
/// | 6.0 | 0 | every deciding cell 0.00 |
///
/// The narrow fields decide it: at a field width clamped to 1.5 px the filtered noise has the most maxima per area.
pub const DEFAULT_THRESHOLD: f64 = 6.0;

/// The first pass's kernel FWHM, px, and the field width when no candidate's half-maximum width reads.
const FIRST_PASS_FWHM_PX: f64 = 3.0;
/// The field width's clamp, px (PA-17).
const FIELD_WIDTH_MIN_PX: f64 = 1.5;
const FIELD_WIDTH_MAX_PX: f64 = 24.0;
/// From this field width on the second pass runs on the 2×2 super-pixel plane.
const SUPER_PIXEL_FROM_PX: f64 = 6.0;
/// The half-maximum width's first box (half-width, px); a first read of at least `HALFMAX_REMEASURE_FROM_PX` is read
/// again in a box of half-width `3·F`, at most `HALFMAX_BOX_MAX_PX` (PA-17).
const HALFMAX_BOX_PX: usize = 15;
const HALFMAX_REMEASURE_FROM_PX: f64 = 7.0;
const HALFMAX_BOX_MAX_PX: usize = 48;
/// The rings' centre is the brightest present pixel within this many px of the seed (Chebyshev), so a brighter
/// neighbour inside the box is never measured in the seed's place.
pub(super) const HALFMAX_PEAK_SEARCH_PX: i64 = 2;
/// The kernel's reach, in σ.
const KERNEL_REACH_SIGMAS: f64 = 3.0;
/// Two candidates whose centroids land within this distance (px) are one star (a tied or masked-centre maximum).
const SAME_STAR_PX: f64 = 1.0;
/// The hot-pixel rule's cap, ρ (controller ruling, Task 6a): a cross-neighbour of a real star holds at least this
/// fraction of the star's peak pixel. 0.15 is geometry, fitted to no bar: the cross-neighbour/peak ratio of a
/// pixel-integrated Gaussian of FWHM 1.0 px centred on a pixel is 0.157, and no star the fit accepts is narrower (Task
/// 6b refuses below 0.8 px as `narrowerThanPixel`).
const HOT_PIXEL_NEIGHBOUR_RATIO: f32 = 0.15;

/// A Gaussian matched filter at the field width, in a separable convolution over the finite, unmasked pixels
/// (a non-finite or masked pixel contributes nothing and normalises nothing — the border is not a star).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchedFilter {
    pub fwhm_px: f64,
}

/// The field width: the half-sample mode of the half-maximum widths (FWHM, px) of every readable candidate or given
/// seed ([`field_width_of`]; PA-17 as revised by Task 10b A2), clamped to [1.5, 24] px, and how many readable reads it
/// was taken over (`from_stars = 0`: none; `fwhm_px` is then the first pass's 3 px, or the caller's own value —
/// `FitOutcome.field_width` for a given `FitStarsOptions.field_width`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldWidth {
    pub fwhm_px: f64,
    pub from_stars: usize,
}

impl FieldWidth {
    /// The width no star measured: the first pass's kernel FWHM.
    fn unmeasured() -> FieldWidth {
        FieldWidth {
            fwhm_px: FIRST_PASS_FWHM_PX,
            from_stars: 0,
        }
    }
}

/// The grid a pass filters: per cell, the sum of its present background-subtracted pixels and, unless every cell
/// holds exactly one present pixel (`None`), their count; `step` full-resolution pixels per cell side.
struct Grid<'a> {
    sum: &'a [f32],
    count: Option<&'a [f32]>,
    w: usize,
    h: usize,
    step: usize,
}

/// `den = Σ k²` over the present pixels: separable (`x[col]·y[row]`) when every pixel is present, a plane otherwise.
enum Den {
    Separable { x: Vec<f32>, y: Vec<f32> },
    Plane(Vec<f32>),
}

impl Den {
    fn at(&self, i: usize, w: usize) -> f32 {
        match self {
            Den::Separable { x, y } => x[i % w] * y[i / w],
            Den::Plane(p) => p[i],
        }
    }
}

/// One pass's output: `f = num/√den` per cell (NaN where the cell holds no present pixel) and `den`.
struct Filtered {
    f: Vec<f32>,
    den: Den,
}

impl MatchedFilter {
    /// The kernel's σ, px.
    pub fn sigma_px(&self) -> f64 {
        self.fwhm_px / GAUSSIAN_FWHM_PER_SIGMA
    }

    /// The least-squares flux of a Gaussian star of this width per `num/den` (`2πσ²`).
    fn flux_per_ratio(&self) -> f64 {
        let s = self.sigma_px();
        2.0 * std::f64::consts::PI * s * s
    }

    /// The 1-D taps `exp(−(i·step)²/(2σ²))`, `i = 0..=R`, `R = max(1, ⌈3σ/step⌉)`, on a grid of `step` px per cell.
    fn taps(&self, step: usize) -> Vec<f32> {
        let s = self.sigma_px();
        let reach = ((KERNEL_REACH_SIGMAS * s / step as f64).ceil() as usize).max(1);
        (0..=reach)
            .map(|i| {
                let r = (i * step) as f64;
                (-r * r / (2.0 * s * s)).exp() as f32
            })
            .collect()
    }

    fn apply(&self, grid: &Grid) -> Filtered {
        let (w, h) = (grid.w, grid.h);
        let taps = self.taps(grid.step);
        let num = convolve_cols(&convolve_rows(grid.sum, w, &taps), w, h, &taps);
        let sq: Vec<f32> = taps.iter().map(|t| t * t).collect();
        let den = match grid.count {
            None => Den::Separable {
                x: edge_sums(w, &sq),
                y: edge_sums(h, &sq),
            },
            Some(c) => Den::Plane(convolve_cols(&convolve_rows(c, w, &sq), w, h, &sq)),
        };
        let f = num
            .par_iter()
            .enumerate()
            .map(|(i, &n)| {
                let present = grid.count.map_or(true, |c| c[i] > 0.0);
                let d = den.at(i, w);
                if present && d > 0.0 {
                    n / d.sqrt()
                } else {
                    f32::NAN
                }
            })
            .collect();
        Filtered { f, den }
    }
}

/// `Σ_j taps[|j|]·src[x + j]` along each row, the image's edge treated as absent.
fn convolve_rows(src: &[f32], w: usize, taps: &[f32]) -> Vec<f32> {
    let reach = taps.len() - 1;
    let mut out = vec![0.0f32; src.len()];
    out.par_chunks_mut(w)
        .zip(src.par_chunks(w))
        .for_each(|(o, row)| {
            for x in 0..w {
                let mut acc = taps[0] * row[x];
                for (j, &k) in taps.iter().enumerate().take(reach + 1).skip(1) {
                    if x >= j {
                        acc += k * row[x - j];
                    }
                    if x + j < w {
                        acc += k * row[x + j];
                    }
                }
                o[x] = acc;
            }
        });
    out
}

/// `Σ_j taps[|j|]·src[y + j]` along each column, the image's edge treated as absent.
fn convolve_cols(src: &[f32], w: usize, h: usize, taps: &[f32]) -> Vec<f32> {
    let reach = taps.len() - 1;
    let mut out = vec![0.0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        o.copy_from_slice(&src[y * w..(y + 1) * w]);
        o.iter_mut().for_each(|v| *v *= taps[0]);
        for (j, &k) in taps.iter().enumerate().take(reach + 1).skip(1) {
            if y >= j {
                let up = &src[(y - j) * w..(y - j + 1) * w];
                o.iter_mut().zip(up).for_each(|(v, u)| *v += k * u);
            }
            if y + j < h {
                let dn = &src[(y + j) * w..(y + j + 1) * w];
                o.iter_mut().zip(dn).for_each(|(v, d)| *v += k * d);
            }
        }
    });
    out
}

/// `Σ_{j: 0 ≤ i + j < n} sq[|j|]` for every `i < n`: one axis of `den` when every pixel is present.
fn edge_sums(n: usize, sq: &[f32]) -> Vec<f32> {
    let reach = sq.len() - 1;
    (0..n)
        .map(|i| {
            let mut s = sq[0];
            for (j, &k) in sq.iter().enumerate().take(reach + 1).skip(1) {
                if i >= j {
                    s += k;
                }
                if i + j < n {
                    s += k;
                }
            }
            s
        })
        .collect()
}

/// The full-resolution plane the candidates are judged and placed on: background-subtracted, absent pixels 0.
struct Detection<'a> {
    d: &'a [f32],
    /// `None` = every pixel present.
    present: Option<&'a [bool]>,
    w: usize,
    h: usize,
    /// The plane's noise σ (working units): the hot-pixel rule's unit.
    sigma_plane: f32,
    threshold: f64,
}

impl Detection<'_> {
    fn is_present(&self, i: usize) -> bool {
        self.present.map_or(true, |p| p[i])
    }

    /// The hot-pixel rule on the UNFILTERED plane (controller ruling, Task 6a): a candidate stands as a star when at
    /// least 2 of its 4 cross-neighbours lie above `background + min(threshold·σ_plane, ρ·(c − background))`, `c`
    /// the candidate's own pixel and ρ = [`HOT_PIXEL_NEIGHBOUR_RATIO`] (`d` is background-subtracted). When fewer
    /// than 2 cross-neighbours are present (a checkerboard mask), the 4 diagonal neighbours stand in.
    ///
    /// A bright candidate (`c ≥ threshold·σ_plane/ρ`, 40 σ at the default 6) sees the plan's rule unchanged: its
    /// neighbours must stand `threshold·σ_plane` clear, which a lone hot pixel's never do. The cap is the finding of
    /// Task 6a: a matched-filter detection at `threshold·σ_f` puts only ≈ 0.16–0.27·threshold σ on each unfiltered
    /// pixel, so the uncapped level asked a faint star for 3–6 times the threshold and became the threshold itself
    /// (R-OSP-2); a fainter candidate's neighbours need only hold the fraction of its peak every real star's do.
    fn has_neighbours(&self, x: usize, y: usize) -> bool {
        const CROSS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
        const DIAGONAL: [(i64, i64); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
        let c = self.d[y * self.w + x];
        let level =
            ((self.threshold * self.sigma_plane as f64) as f32).min(HOT_PIXEL_NEIGHBOUR_RATIO * c);
        let at = |(dx, dy): (i64, i64)| -> Option<f32> {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if nx < 0 || ny < 0 || nx >= self.w as i64 || ny >= self.h as i64 {
                return None;
            }
            let j = ny as usize * self.w + nx as usize;
            self.is_present(j).then(|| self.d[j])
        };
        let present_cross = CROSS.iter().filter(|&&o| at(o).is_some()).count();
        let set = if present_cross >= 2 { CROSS } else { DIAGONAL };
        set.iter()
            .filter(|&&o| at(o).is_some_and(|v| v > level))
            .count()
            >= 2
    }

    /// The flux-weighted centroid (weights `max(d, 0)`) in the `(2b+1)²` box about `(x, y)` and the box's maximum of
    /// `d` (the peak above the background); `None` when the box holds no present pixel.
    fn centroid(&self, x: usize, y: usize, b: usize) -> Option<(f64, f64, f64)> {
        let (x0, x1) = (x.saturating_sub(b), (x + b).min(self.w - 1));
        let (y0, y1) = (y.saturating_sub(b), (y + b).min(self.h - 1));
        let (mut sw, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
        let mut peak = f64::NEG_INFINITY;
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                let i = yy * self.w + xx;
                if !self.is_present(i) {
                    continue;
                }
                let v = self.d[i] as f64;
                peak = peak.max(v);
                let wv = v.max(0.0);
                sw += wv;
                sx += wv * xx as f64;
                sy += wv * yy as f64;
            }
        }
        if peak == f64::NEG_INFINITY {
            return None;
        }
        Some(if sw > 0.0 {
            (sx / sw, sy / sw, peak)
        } else {
            (x as f64, y as f64, peak)
        })
    }
}

/// What one pass found: its seeds (unsorted, unmerged) and the numbers the debug line and the
/// [`DetectionReport`] carry.
struct PassOutcome {
    /// Each with its filtered SNR: the maximum's `f` over the σ_f the pass thresholded it at.
    seeds: Vec<(Seed, f64)>,
    /// 1.4826·MAD of the filtered plane over the stratified sample: the threshold's σ through Task 10, reported.
    sigma_f_measured: f64,
    /// The σ_f the pass thresholded at: the median of the propagated map over the stratified sample, or
    /// `sigma_f_measured` under [`PassNoise::Measured`].
    sigma_f: f64,
    /// The noise's gain at the pass's kernel and grid; NaN under [`PassNoise::Measured`].
    gain: f64,
    spikes: usize,
}

/// The noise one pass thresholds at (Task 10b ruling A1): PROPAGATED, σ_f(x) = the mesh's noise map at the cell's
/// full-resolution pixel × the noise's gain at this pass's kernel and grid; or — no noise map, or no gain measured —
/// the filtered plane's MEASURED σ_f, the rule through Task 10 (one `warn!` per plane says so).
/// `median` is the map's median over the stratified sample of the present pixels ([`map_median`]), read once per
/// plane: every pass reports `median × gain` as its σ_f.
#[derive(Clone, Copy)]
enum PassNoise<'a> {
    Propagated {
        map: &'a [f32],
        median: f64,
        gain: f64,
    },
    Measured,
}

impl<'a> PassNoise<'a> {
    fn of(
        map: Option<(&'a [f32], f64)>,
        gain: GainSource,
        fwhm: f64,
        step: usize,
    ) -> PassNoise<'a> {
        match (map, gain.at(fwhm, step)) {
            (Some((map, median)), Some(gain)) if gain.is_finite() => {
                PassNoise::Propagated { map, median, gain }
            }
            _ => PassNoise::Measured,
        }
    }
}

/// Where a plane's noise gain comes from (Task 10b A1b/A1c, rulings C-7, C-10): derived for a plane the caller states
/// white ([`PixelNoise::White`]), read from the plane for a mixed or unknown one.
#[derive(Clone, Copy)]
enum GainSource {
    /// Pixels never mixed: shot, read and dark noise are per pixel and calibration is per-pixel arithmetic, so every
    /// lag-≠0 covariance is 0 and the gain is 1 at any kernel and grid. Nothing is read from the plane.
    Unmixed,
    /// Pixels spatially mixed (a debayered colour plane): the noise's own correlation, read on the plane.
    Read(NoiseCorrelation),
    /// Mixed, but the correlation could not be read.
    None,
}

impl GainSource {
    /// The plane's correlation (read unless the caller stated the noise white) as the gain's source.
    fn of(pixel_noise: PixelNoise, read: impl FnOnce() -> Option<NoiseCorrelation>) -> GainSource {
        if pixel_noise == PixelNoise::White {
            GainSource::Unmixed
        } else {
            read().map_or(GainSource::None, GainSource::Read)
        }
    }

    /// The gain at a kernel of FWHM `fwhm` px on a grid of `step`; `None` when none is available.
    fn at(&self, fwhm: f64, step: usize) -> Option<f64> {
        match self {
            GainSource::Unmixed => Some(1.0),
            GainSource::Read(c) => Some(c.gain(fwhm, step)),
            GainSource::None => None,
        }
    }
}

/// 1.4826·MAD of a filtered plane's finite values over the stratified sample of its grid (all of them when the sample
/// holds fewer than 8).
fn measured_sigma(f: &[f32], gw: usize, gh: usize) -> f64 {
    let mut sample: Vec<f32> = Vec::with_capacity(gw * gh / 16 + 1);
    for_each_stratified(gw, gh, |i| {
        if f[i].is_finite() {
            sample.push(f[i]);
        }
    });
    if sample.len() < 8 {
        sample = f.iter().copied().filter(|v| v.is_finite()).collect();
    }
    let m = median_in_place(&mut sample);
    (MAD_TO_SIGMA * mad_about(&sample, m)) as f64
}

/// The median of `map` over the stratified sample of the present pixels (all pixels when `present` is `None`).
fn map_median(map: &[f32], present: Option<&[bool]>, w: usize, h: usize) -> f64 {
    let mut sample: Vec<f32> = Vec::with_capacity(w * h / 16 + 1);
    for_each_stratified(w, h, |i| {
        if map[i].is_finite() && present.map_or(true, |p| p[i]) {
            sample.push(map[i]);
        }
    });
    if sample.is_empty() {
        f64::NAN
    } else {
        median_in_place(&mut sample) as f64
    }
}

/// One pass of `filter` over `grid`: the local maxima of `f` above `threshold·σ_f(x)` (`noise`, ruling A1), each
/// judged by the hot-pixel rule on its full-resolution pixel (the cell itself, or a super-pixel's brightest present
/// pixel), placed by its centroid on the full plane in a `(2·⌈F⌉+1)²` box, with `flux` the filter's least-squares
/// flux there. A filtered plane with no measurable spread seeds nothing.
fn run_pass(
    filter: MatchedFilter,
    grid: &Grid,
    det: &Detection,
    noise: PassNoise,
) -> Option<PassOutcome> {
    let out = filter.apply(grid);
    let (gw, gh) = (grid.w, grid.h);
    let sigma_f_measured = measured_sigma(&out.f, gw, gh);
    if !(sigma_f_measured.is_finite() && sigma_f_measured > 0.0) {
        warn!(
            width = det.w,
            height = det.h,
            fwhm_px = filter.fwhm_px,
            "matched filter: the filtered plane has no measurable noise; no seeds"
        );
        return None;
    }
    let measured_level = (det.threshold * sigma_f_measured) as f32;
    let (sigma_f, gain) = match noise {
        PassNoise::Propagated { median, gain, .. } => (median * gain, gain),
        PassNoise::Measured => (sigma_f_measured, f64::NAN),
    };
    // σ_f at a cell: at its full-resolution pixel (its top-left one on the super-pixel grid); the level is K·σ_f.
    let sigma_at = |x: usize, y: usize| -> Option<f64> {
        match noise {
            PassNoise::Propagated { map, gain, .. } => {
                let j = (y * grid.step).min(det.h - 1) * det.w + (x * grid.step).min(det.w - 1);
                let s = map[j] as f64 * gain;
                (s.is_finite() && s > 0.0).then_some(s)
            }
            PassNoise::Measured => None,
        }
    };
    let level_at = |x: usize, y: usize| -> f32 {
        match sigma_at(x, y) {
            Some(s) => (det.threshold * s) as f32,
            None => measured_level,
        }
    };
    let level_at = &level_at;
    let maxima: Vec<usize> = (0..gh)
        .into_par_iter()
        .flat_map_iter(|y| {
            let f = &out.f;
            (0..gw).filter_map(move |x| {
                let i = y * gw + x;
                let c = f[i];
                if !(c > level_at(x, y)) {
                    return None;
                }
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if (dx == 0 && dy == 0)
                            || nx < 0
                            || ny < 0
                            || nx >= gw as i64
                            || ny >= gh as i64
                        {
                            continue;
                        }
                        let j = ny as usize * gw + nx as usize;
                        let o = f[j];
                        // A tie goes to the lower index, so a flat top yields one maximum.
                        if o > c || (o == c && j < i) {
                            return None;
                        }
                    }
                }
                Some(i)
            })
        })
        .collect();
    let half_box = filter.fwhm_px.ceil() as usize;
    let ratio = filter.flux_per_ratio();
    let mut spikes = 0usize;
    let mut seeds = Vec::with_capacity(maxima.len());
    for i in maxima {
        let (cx, cy) = (i % gw, i / gw);
        let (px, py) = if grid.step == 1 {
            (cx, cy)
        } else {
            // The super-pixel's brightest present full-resolution pixel.
            let mut best: Option<(usize, usize, f32)> = None;
            for yy in cy * grid.step..((cy + 1) * grid.step).min(det.h) {
                for xx in cx * grid.step..((cx + 1) * grid.step).min(det.w) {
                    let j = yy * det.w + xx;
                    if det.is_present(j) && best.map_or(true, |(_, _, v)| det.d[j] > v) {
                        best = Some((xx, yy, det.d[j]));
                    }
                }
            }
            match best {
                Some((xx, yy, _)) => (xx, yy),
                None => continue,
            }
        };
        if !det.has_neighbours(px, py) {
            spikes += 1;
            continue;
        }
        let Some((x, y, peak)) = det.centroid(px, py, half_box) else {
            continue;
        };
        if !(peak > 0.0) {
            continue;
        }
        let den = out.den.at(i, gw) as f64;
        let flux = ratio * out.f[i] as f64 / den.sqrt();
        let snr = out.f[i] as f64 / sigma_at(cx, cy).unwrap_or(sigma_f_measured);
        seeds.push((Seed { x, y, peak, flux }, snr));
    }
    Some(PassOutcome {
        seeds,
        sigma_f_measured,
        sigma_f,
        gain,
        spikes,
    })
}

/// Sorted by `flux` descending (stable), a seed whose centroid lies within [`SAME_STAR_PX`] of a brighter kept one is
/// the same star and dropped. Each seed carries its filtered SNR (the pass's `f / σ_f` at its maximum) along.
fn sort_and_merge(mut seeds: Vec<(Seed, f64)>) -> Vec<(Seed, f64)> {
    seeds.sort_by(|a, b| b.0.flux.total_cmp(&a.0.flux));
    let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    let mut kept = Vec::with_capacity(seeds.len());
    'next: for item in seeds {
        let s = &item.0;
        let (gx, gy) = (s.x.floor() as i64, s.y.floor() as i64);
        for dy in -1..=1 {
            for dx in -1..=1 {
                if let Some(list) = grid.get(&(gx + dx, gy + dy)) {
                    if list
                        .iter()
                        .any(|&(x, y)| (x - s.x).hypot(y - s.y) <= SAME_STAR_PX)
                    {
                        continue 'next;
                    }
                }
            }
        }
        grid.entry((gx, gy)).or_default().push((s.x, s.y));
        kept.push(item);
    }
    kept
}

/// The background the filter subtracts: the plane's one mesh model (cell 128 px, [`domain::background_mesh`]) over the
/// present pixels, or — should the model be unavailable — the median of the present stratified sample everywhere.
fn background<'m>(
    plane: &[f32],
    w: usize,
    h: usize,
    mask: Option<&[bool]>,
    model: Option<&'m [f32]>,
) -> Cow<'m, [f32]> {
    if let Some(m) = model {
        return Cow::Borrowed(m);
    }
    let mut s: Vec<f32> = Vec::with_capacity(w * h / 16 + 1);
    for_each_stratified(w, h, |i| {
        let v = plane[i];
        if v.is_finite() && mask.map_or(true, |m| m[i]) {
            s.push(v);
        }
    });
    let level = if s.is_empty() {
        0.0
    } else {
        median_in_place(&mut s)
    };
    warn!(
        width = w,
        height = h,
        "matched filter: the mesh background model is unavailable; a flat background is subtracted"
    );
    Cow::Owned(vec![level; w * h])
}

/// Two passes: a 3-px kernel finds the candidates, and the half-sample mode of the half-maximum widths
/// ([`halfmax_width`], PA-17) of every readable one gives the field width (Task 10b A2: no count, no ranking and no
/// saturation test, ruling F25; clamped to [1.5, 24] px; [`field_width_of`]); the second pass runs at that width. A
/// field width ≥ 6 px runs the second pass on the 2×2 super-pixel plane (kernel halved; candidate positions ×2,
/// refined on the full plane). Candidates: local maxima of the filtered plane above `threshold·σ_f(x)` — σ_f
/// PROPAGATED (Task 10b ruling A1): the mesh's per-cell clipped σ (its noise map) × the noise's filtered-to-pixel
/// gain at the pass's kernel and grid ([`detection_noise`]), so smooth structure the mesh cannot follow no longer
/// raises the threshold; the filtered plane's measured σ_f (1.4826·MAD over the stratified sample, the rule through
/// Task 10) when no gain is measured — with ≥ 2 of 4 cross-neighbours above `background
/// + min(threshold·σ_plane, 0.15·(c − background))` on the UNFILTERED plane, `c` the candidate's own pixel (the
/// hot-pixel rule with its capped level — controller ruling, Task 6a; R-OSP-2). Position: the flux-weighted
/// centroid in a (2·ceil(F)+1)² box on the background-subtracted plane; `peak` = that box's maximum − background;
/// `flux` = the filtered response (the matched filter's integrated signal — the selection key). Sorted by `flux`
/// descending; the knot tests (ruling A3, [`knot_verdict`]: kernels at the field width, bounds at its star-width
/// envelope, [`WidthEnvelope`], A2b) then drop the structure seeds and flag the extended ones ([`DetectionReport`]);
/// truncated to `max_stars`. A `NoiseModel` with `source ==
/// NoiseSource::None` returns no seeds and one `warn!` (Review Focus 4). Public: Plan C calls it on its own 2×2
/// super-pixel OSC plane (I-8). Standalone it has no saturation plane, so no seed is exempt from the knot tests as
/// near the clip; [`super::measure_plane`] hands its own.
///
/// `pixel_noise` is the caller's statement of the noise's spatial correlation (ruling C-10): [`PixelNoise::White`]
/// (sensor pixels: raw or calibrated mono, an undebayered CFA plane, Plan C's super-pixel plane) makes the
/// propagated noise's gain 1 by derivation, nothing read from the plane; [`PixelNoise::Mixed`] (debayered,
/// resampled, integrated) and [`PixelNoise::Unknown`] read the gain from the plane's own noise correlation. Every
/// caller states its truth ([`super::MeasureOptions::pixel_noise`]).
///
/// The plane is in the module's working units (ADU-scaled); `mask[i] == false` marks pixel `i` absent like a
/// non-finite one. Two candidates whose centroids land within 1 px are one star (the brighter is kept). With a
/// checkerboard mask the cross-neighbours of a present pixel are all absent, so the diagonal ones stand in. The cap
/// (`0.15` = [`HOT_PIXEL_NEIGHBOUR_RATIO`]) leaves a bright candidate (`c − background ≥ threshold·σ_plane/0.15`)
/// under the uncapped `background + threshold·σ_plane`.
#[allow(clippy::too_many_arguments)]
pub fn detect(
    plane: &[f32],
    w: usize,
    h: usize,
    noise: &NoiseModel,
    threshold: f64,
    max_stars: usize,
    mask: Option<&[bool]>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    pixel_noise: PixelNoise,
) -> (Vec<Seed>, FieldWidth, DetectionReport) {
    detect_with(
        plane,
        w,
        h,
        noise,
        threshold,
        max_stars,
        mask,
        pool,
        pixel_noise,
        None,
        None,
    )
}

/// [`detect`] on the plane's one background mesh when the caller already computed it ([`super::measure_plane`],
/// ruling Q4: [`domain::background_mesh`] over the same present pixels, with its noise map; moved in, so its
/// background map is freed once the plane is background-subtracted) and with the plane's saturation inputs (the knot
/// tests' near-clip exemption); `None` = the mesh computed here, no exemption.
#[allow(clippy::too_many_arguments)]
pub(crate) fn detect_with(
    plane: &[f32],
    w: usize,
    h: usize,
    noise: &NoiseModel,
    threshold: f64,
    max_stars: usize,
    mask: Option<&[bool]>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    pixel_noise: PixelNoise,
    mesh: Option<BackgroundMesh>,
    saturation: Option<&SaturationPlane>,
) -> (Vec<Seed>, FieldWidth, DetectionReport) {
    if plane.len() != w * h || mask.is_some_and(|m| m.len() != w * h) || w < 3 || h < 3 {
        warn!(
            len = plane.len(),
            width = w,
            height = h,
            "matched filter: the plane (or its mask) does not match the geometry; no seeds"
        );
        return (
            Vec::new(),
            FieldWidth::unmeasured(),
            DetectionReport::none(),
        );
    }
    if noise.source == NoiseSource::None || !(noise.sigma.is_finite() && noise.sigma > 0.0) {
        warn!(
            width = w,
            height = h,
            noise = noise.sigma,
            "matched filter: the plane has no noise estimate, so no threshold; no seeds"
        );
        return (
            Vec::new(),
            FieldWidth::unmeasured(),
            DetectionReport::none(),
        );
    }
    if !(threshold.is_finite() && threshold > 0.0) {
        warn!(
            threshold,
            "matched filter: the threshold is not a positive number; no seeds"
        );
        return (
            Vec::new(),
            FieldWidth::unmeasured(),
            DetectionReport::none(),
        );
    }
    let run = || {
        let mesh = mesh.unwrap_or_else(|| domain::background_mesh(plane, w, h, mask, true));
        detect_on(
            plane,
            w,
            h,
            noise.sigma as f32,
            threshold,
            max_stars,
            mask,
            pixel_noise,
            mesh,
            saturation,
        )
    };
    match pool {
        Some(p) => p.install(run),
        None => run(),
    }
}

#[allow(clippy::too_many_arguments)]
fn detect_on(
    plane: &[f32],
    w: usize,
    h: usize,
    sigma_plane: f32,
    threshold: f64,
    max_stars: usize,
    mask: Option<&[bool]>,
    pixel_noise: PixelNoise,
    mesh: BackgroundMesh,
    saturation: Option<&SaturationPlane>,
) -> (Vec<Seed>, FieldWidth, DetectionReport) {
    let present_at = |i: usize| plane[i].is_finite() && mask.map_or(true, |m| m[i]);
    let present: Option<Vec<bool>> = if (0..w * h).all(present_at) {
        None
    } else {
        Some((0..w * h).map(present_at).collect())
    };
    let BackgroundMesh {
        background: model,
        noise: noise_map,
    } = mesh;
    let bg = background(plane, w, h, mask, model.as_deref());
    let d: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| if present_at(i) { plane[i] - bg[i] } else { 0.0 })
        .collect();
    drop(bg);
    drop(model);
    let count: Option<Vec<f32>> = present
        .as_ref()
        .map(|p| p.iter().map(|&k| if k { 1.0 } else { 0.0 }).collect());
    let det = Detection {
        d: &d,
        present: present.as_deref(),
        w,
        h,
        sigma_plane,
        threshold,
    };
    let full = Grid {
        sum: &d,
        count: count.as_deref(),
        w,
        h,
        step: 1,
    };

    // The detection noise (ruling A1): σ_f(x) = the mesh's noise map × the noise's gain at each pass's kernel and
    // grid, the gain from the noise's own correlation, read once per plane.
    let corr = GainSource::of(pixel_noise, || {
        noise_map
            .as_deref()
            .and_then(|m| NoiseCorrelation::measure(&d, present.as_deref(), m, w, h))
    });
    if noise_map.is_none() || matches!(corr, GainSource::None) {
        warn!(
            width = w,
            height = h,
            "matched filter: no noise map or no measurable noise correlation; the threshold reads the filtered plane's measured noise"
        );
    }
    // The map's median is read once: every pass reports it × its own gain (the same sample, so the same number).
    let map = match (noise_map.as_deref(), corr) {
        (Some(m), GainSource::Unmixed | GainSource::Read(_)) => {
            Some((m, map_median(m, present.as_deref(), w, h)))
        }
        _ => None,
    };
    let pass_noise = |fwhm: f64, step: usize| PassNoise::of(map, corr, fwhm, step);
    let none = |field: FieldWidth| (Vec::new(), field, DetectionReport::none());

    // Pass 1: the 3-px kernel; its readable candidates give the field width.
    let first_filter = MatchedFilter {
        fwhm_px: FIRST_PASS_FWHM_PX,
    };
    let Some(first) = run_pass(first_filter, &full, &det, pass_noise(FIRST_PASS_FWHM_PX, 1)) else {
        return none(FieldWidth::unmeasured());
    };
    let first_seeds = sort_and_merge(first.seeds);
    let at: Vec<(f64, f64)> = first_seeds.iter().map(|(s, _)| (s.x, s.y)).collect();
    let reads = readable_widths(plane, w, h, &at, mask);
    drop(at);
    let mut widths: Vec<f64> = reads.iter().map(|&(width, _)| width).collect();
    let field = field_width_of_reads(&mut widths);
    drop(widths);
    // The knot bounds' star-width envelope over the same readable reads (Task 10b A2b, ruling A2-2: E4).
    let envelope = if field.from_stars > 0 {
        WidthEnvelope::of(&reads, field.fwhm_px)
    } else {
        WidthEnvelope::at(field.fwhm_px)
    };
    drop(reads);

    // Pass 2: at the field width, on the 2×2 super-pixel plane from 6 px on.
    let filter = MatchedFilter {
        fwhm_px: field.fwhm_px,
    };
    let super_pixel = field.fwhm_px >= SUPER_PIXEL_FROM_PX;
    let (second, seeds) = if super_pixel {
        let (sw, sh) = (w.div_ceil(2), h.div_ceil(2));
        let mut sums = vec![0.0f32; sw * sh];
        let mut counts = vec![0.0f32; sw * sh];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if det.is_present(i) {
                    let c = (y / 2) * sw + x / 2;
                    sums[c] += d[i];
                    counts[c] += 1.0;
                }
            }
        }
        let grid = Grid {
            sum: &sums,
            count: Some(&counts),
            w: sw,
            h: sh,
            step: 2,
        };
        let Some(mut p) = run_pass(filter, &grid, &det, pass_noise(field.fwhm_px, 2)) else {
            return none(field);
        };
        let s = sort_and_merge(std::mem::take(&mut p.seeds));
        (p, s)
    } else if field.from_stars == 0 {
        // No width was read: the field width is the first pass's own, so its candidates stand.
        (
            PassOutcome {
                seeds: Vec::new(),
                sigma_f_measured: first.sigma_f_measured,
                sigma_f: first.sigma_f,
                gain: first.gain,
                spikes: first.spikes,
            },
            first_seeds,
        )
    } else {
        let Some(mut p) = run_pass(filter, &full, &det, pass_noise(field.fwhm_px, 1)) else {
            return none(field);
        };
        let s = sort_and_merge(std::mem::take(&mut p.seeds));
        (p, s)
    };

    // The knot tests (ruling A3) at the field width, on the full-resolution plane, z in units of σ_f there, their
    // bounds at the envelope (A2b): a structure seed is dropped and counted, an extended one kept and flagged.
    let knot_sigma = match pass_noise(field.fwhm_px, 1) {
        PassNoise::Propagated { map, gain, .. } => KnotSigma::Propagated { map, gain },
        PassNoise::Measured => KnotSigma::Measured(second.sigma_f_measured),
    };
    let bounds = knot_bounds(field.fwhm_px, envelope.fwhm_px);
    let verdicts: Vec<KnotVerdict> = seeds
        .par_iter()
        .map(|(s, _)| {
            let sigma = knot_sigma.at(s, w, h, second.sigma_f_measured);
            let m = knot_metrics(&d, present.as_deref(), w, h, s.x, s.y, field.fwhm_px, sigma);
            knot_verdict(
                &m,
                bounds,
                threshold,
                near_clip(plane, mask, w, h, s, saturation),
            )
        })
        .collect();
    let mut kept = Vec::with_capacity(seeds.len());
    let mut extended = Vec::with_capacity(seeds.len());
    let mut filter_snr = Vec::with_capacity(seeds.len());
    let mut structure_dropped = 0usize;
    for ((s, snr), v) in seeds.into_iter().zip(verdicts) {
        match v {
            KnotVerdict::Structure => structure_dropped += 1,
            v => {
                kept.push(s);
                extended.push(v == KnotVerdict::Extended);
                filter_snr.push(snr);
            }
        }
    }
    let found = kept.len();
    kept.truncate(max_stars);
    extended.truncate(max_stars);
    filter_snr.truncate(max_stars);
    let report = DetectionReport {
        sigma_f: second.sigma_f,
        sigma_f_measured: second.sigma_f_measured,
        noise_gain: second.gain,
        structure_dropped,
        extended,
        filter_snr,
        envelope,
    };
    debug!(
        width = w,
        height = h,
        threshold,
        sigma_f = report.sigma_f,
        sigma_f_measured = report.sigma_f_measured,
        noise_gain = report.noise_gain,
        structure_dropped,
        extended = report.extended.iter().filter(|&&e| e).count(),
        fwhm_px = field.fwhm_px,
        from_stars = field.from_stars,
        super_pixel,
        spikes = second.spikes,
        stars_detected = kept.len(),
        count = found,
        "matched filter seeds detected"
    );
    (kept, field, report)
}

/// σ_f at a seed for the knot tests' z: the propagated map × gain at the field width (full resolution), or the
/// second pass's measured σ_f. The measured fallback is for the warned path only (no noise map, or no correlation to
/// propagate through): when the second pass ran on the 2×2 super-pixel grid that σ_f is the step-2 filtered plane's while
/// the knot metrics run at full resolution, so the fallback's z is approximate there.
#[derive(Clone, Copy)]
enum KnotSigma<'a> {
    Propagated { map: &'a [f32], gain: f64 },
    Measured(f64),
}

impl KnotSigma<'_> {
    fn at(&self, s: &Seed, w: usize, h: usize, fallback: f64) -> f64 {
        match *self {
            KnotSigma::Propagated { map, gain } => {
                let x = (s.x.round().max(0.0) as usize).min(w - 1);
                let y = (s.y.round().max(0.0) as usize).min(h - 1);
                let v = map[y * w + x] as f64 * gain;
                if v.is_finite() && v > 0.0 {
                    v
                } else {
                    fallback
                }
            }
            KnotSigma::Measured(v) => v,
        }
    }
}

/// The knot exemption (ruling A3; Task 8's owned range, never a new maximum): the brightest present pixel of the 3×3
/// about the seed stands within the plateau band of the clip ([`SaturationPlane::near_clip`], the same function the
/// saturation verdict's near-clip clause calls). No saturation plane (a standalone [`detect`]): no seed is near the clip.
fn near_clip(
    plane: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    s: &Seed,
    saturation: Option<&SaturationPlane>,
) -> bool {
    let Some(sat) = saturation else {
        return false;
    };
    let (cx, cy) = (s.x.round() as i64, s.y.round() as i64);
    let mut vals = [f64::NAN; 9];
    let mut k = 0;
    for yy in cy - 1..=cy + 1 {
        for xx in cx - 1..=cx + 1 {
            if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                continue;
            }
            let i = yy as usize * w + xx as usize;
            if mask.map_or(true, |m| m[i]) {
                vals[k] = plane[i] as f64;
                k += 1;
            }
        }
    }
    sat.near_clip(vals)
}

/// What the module's matched filter measured on one plane besides its seeds (Task 10b, F-10): the detection noise of
/// its second pass and the knot tests' outcome. Reported (the probe's frame line), not persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectionReport {
    /// The σ_f the second pass thresholded at (working units): the median of the propagated σ_f(x) over the
    /// stratified sample, or the measured one when no gain was measured.
    pub sigma_f: f64,
    /// 1.4826·MAD of the second pass's filtered plane over the stratified sample: the threshold's σ through Task 10.
    pub sigma_f_measured: f64,
    /// The noise's filtered-to-pixel σ ratio at the second pass's kernel and grid: exactly 1 on a plane whose pixels
    /// were never spatially mixed (ruling C-7), read from the plane on a mixed one; NaN when none was measured.
    pub noise_gain: f64,
    /// The seeds the knot test dropped as structure (ruling A3).
    pub structure_dropped: usize,
    /// One per returned seed, in seed order: the knot test flagged it extended (ruling A3). An extended seed is fitted
    /// but leaves the β sample, `c`'s qualifiers and the frame statistic (ruling P3).
    pub extended: Vec<bool>,
    /// One per returned seed, in seed order: its filtered SNR, the matched filter's response at its maximum over the
    /// σ_f that pass thresholded it at (propagated, or measured when no gain was measured) - the probe's `filterSnr`
    /// (controller addendum to phase A1, the lab's W.1 row).
    pub filter_snr: Vec<f64>,
    /// The field's star-width envelope the knot bounds sit at (Task 10b A2b, rulings A2-1 and A2-2): F_env and its
    /// spread `s`, px — the probe's `fieldWidthEnvelope` and `fieldWidthSpread`.
    pub envelope: WidthEnvelope,
}

impl DetectionReport {
    /// No detection ran (a refused plane): no σ_f, no seeds.
    pub fn none() -> DetectionReport {
        DetectionReport {
            sigma_f: f64::NAN,
            sigma_f_measured: f64::NAN,
            noise_gain: f64::NAN,
            structure_dropped: 0,
            extended: Vec::new(),
            filter_snr: Vec::new(),
            envelope: WidthEnvelope {
                spread_px: f64::NAN,
                fwhm_px: f64::NAN,
            },
        }
    }
}

/// The detection noise of one plane at one kernel (Task 10b ruling A1), [`detection_noise`]'s answer.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectionNoise {
    /// σ_f(x), working units: the mesh's per-cell clipped σ (its noise map) × `gain`; the measured σ_f everywhere
    /// when no gain was measured.
    pub sigma: Vec<f32>,
    /// The noise's filtered-to-pixel σ ratio at the kernel: exactly 1 when the caller stated the noise white (rulings C-7, C-10), else
    /// read once per plane on the high-pass residual; NaN when not measured.
    pub gain: f64,
    /// 1.4826·MAD of the filtered plane over the stratified sample: σ_f as measured through Task 10.
    pub measured: f64,
}

impl DetectionNoise {
    /// The median of [`Self::sigma`] over the plane (its finite values); NaN when there are none.
    pub fn median(&self) -> f64 {
        let mut s: Vec<f32> = self
            .sigma
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        if s.is_empty() {
            f64::NAN
        } else {
            median_in_place(&mut s) as f64
        }
    }
}

/// The plane's detection noise at a kernel of FWHM `fwhm` px on the full-resolution grid (Task 10b ruling A1): the
/// plane's one mesh ([`domain::background_mesh`] with its noise map) over the present pixels, the noise's correlation
/// read on the high-pass residual of the background-subtracted plane ([`NoiseCorrelation`]), `gain` at the kernel,
/// `sigma = noise map × gain` (`pixel_noise` as in [`detect`]), and `measured` = today's σ_f of the filtered plane. [`detect`] uses the same pieces per
/// pass; this is their one-kernel view (tests, the probe).
pub fn detection_noise(
    plane: &[f32],
    w: usize,
    h: usize,
    mask: Option<&[bool]>,
    fwhm: f64,
    pixel_noise: PixelNoise,
) -> DetectionNoise {
    let none = DetectionNoise {
        sigma: Vec::new(),
        gain: f64::NAN,
        measured: f64::NAN,
    };
    if plane.len() != w * h || mask.is_some_and(|m| m.len() != w * h) || w < 3 || h < 3 {
        warn!(
            len = plane.len(),
            width = w,
            height = h,
            "detection noise: the plane (or its mask) does not match the geometry"
        );
        return none;
    }
    let present_at = |i: usize| plane[i].is_finite() && mask.map_or(true, |m| m[i]);
    let present: Option<Vec<bool>> = if (0..w * h).all(present_at) {
        None
    } else {
        Some((0..w * h).map(present_at).collect())
    };
    let mesh = domain::background_mesh(plane, w, h, mask, true);
    let bg = background(plane, w, h, mask, mesh.background.as_deref());
    let d: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| if present_at(i) { plane[i] - bg[i] } else { 0.0 })
        .collect();
    drop(bg);
    let count: Option<Vec<f32>> = present
        .as_ref()
        .map(|p| p.iter().map(|&k| if k { 1.0 } else { 0.0 }).collect());
    let grid = Grid {
        sum: &d,
        count: count.as_deref(),
        w,
        h,
        step: 1,
    };
    let measured = measured_sigma(&MatchedFilter { fwhm_px: fwhm }.apply(&grid).f, w, h);
    let corr = GainSource::of(pixel_noise, || {
        mesh.noise
            .as_deref()
            .and_then(|m| NoiseCorrelation::measure(&d, present.as_deref(), m, w, h))
    });
    let gain = corr.at(fwhm, 1).unwrap_or(f64::NAN);
    let sigma = match mesh.noise {
        Some(map) if gain.is_finite() => map.iter().map(|&v| (v as f64 * gain) as f32).collect(),
        _ => vec![measured as f32; w * h],
    };
    DetectionNoise {
        sigma,
        gain,
        measured,
    }
}

/// The high-pass box of the noise read (half-width, px): `r = d − (the mean of the other 24 pixels of the 5×5 box)`
/// keeps a few per cent of anything smooth over a few px (a gradient, a nebula, a star's far wing), so what the read
/// sees is the noise.
const NOISE_HP_HALF: usize = 2;
/// The reach of the noise's own autocovariance (px, Chebyshev): `C(L) = 0` beyond it (calibration, debayering and
/// resampling correlate pixels within two of each other).
const NOISE_CORR_REACH: usize = 2;
/// The residual's autocovariance reaches `NOISE_CORR_REACH + 2·NOISE_HP_HALF`: every lag up to it is read.
const NOISE_LAG_REACH: usize = NOISE_CORR_REACH + 2 * NOISE_HP_HALF;
/// A residual farther than this many robust σ from the residual median is a source, not noise …
const NOISE_CLIP_SIGMAS: f32 = 5.0;
/// … and every residual within this many px (Chebyshev) of one leaves the read with it: a source reaches one high-pass
/// box beyond its own pixel on either side.
const NOISE_CLIP_DILATE_PX: usize = 2 * NOISE_HP_HALF;
/// The fewest quiet anchors the read needs; with fewer the gain is not measured.
const NOISE_MIN_ANCHORS: usize = 4096;
/// About this many anchors are read: every row on a small plane, a stride of rows on a large one.
const NOISE_ANCHOR_TARGET: usize = 1 << 20;

const NOISE_CORR_SIDE: usize = 2 * NOISE_CORR_REACH + 1;

/// The noise's autocovariance `C(L)` at the lags `|L|∞ ≤ NOISE_CORR_REACH` (`C(−L) = C(L)`; working units²), read once
/// per plane (Task 10b ruling A1): the filtered-to-pixel σ ratio of the noise at any kernel and grid — the gain σ_f
/// propagates the mesh's noise map with — follows from it ([`NoiseCorrelation::gain`]).
///
/// The read: the high-pass residual `r` ([`NOISE_HP_HALF`]) over the pixels whose whole box is present; its sources
/// (beyond [`NOISE_CLIP_SIGMAS`] robust σ) and their neighbourhood ([`NOISE_CLIP_DILATE_PX`]) left out; anchors in the
/// quieter half of the plane by the mesh's noise map (D2: the darkest boxes); the mean product `r(i)·r(i+L)` (about
/// the residual median) at every lag `|L|∞ ≤ NOISE_LAG_REACH` of the half plane, both ends quiet; and `C` the
/// least-squares solution of `E[r(i)·r(i+L)] = Σ_v H₂(v)·C(L + v)`, `H₂` the high-pass box's autocorrelation. A smooth
/// structure never reaches it (the high-pass took it out); white noise reads `C(L ≠ 0) = 0`, a gain of 1.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NoiseCorrelation {
    /// `c[dy + R][dx + R]`, `R` = [`NOISE_CORR_REACH`].
    c: [[f64; NOISE_CORR_SIDE]; NOISE_CORR_SIDE],
}

impl NoiseCorrelation {
    /// `None` when the plane is too small, its residual has no spread, fewer than [`NOISE_MIN_ANCHORS`] anchors are
    /// quiet, or the solve fails (`C(0)` not a positive number).
    fn measure(
        d: &[f32],
        present: Option<&[bool]>,
        noise_map: &[f32],
        w: usize,
        h: usize,
    ) -> Option<NoiseCorrelation> {
        let (b, lag) = (NOISE_HP_HALF, NOISE_LAG_REACH as i64);
        if w < 2 * b + NOISE_LAG_REACH + 1 || h < 2 * b + NOISE_LAG_REACH + 1 {
            return None;
        }
        let r = high_pass(d, present, w, h);
        let mut sample: Vec<f32> = Vec::with_capacity(w * h / 16 + 1);
        for_each_stratified(w, h, |i| {
            if r[i].is_finite() {
                sample.push(r[i]);
            }
        });
        if sample.len() < 8 {
            return None;
        }
        let med = median_in_place(&mut sample);
        let sigma = MAD_TO_SIGMA * mad_about(&sample, med);
        if !(sigma.is_finite() && sigma > 0.0) {
            return None;
        }
        let quiet = quiet_residuals(&r, med, sigma, w, h);
        let mut levels: Vec<f32> = Vec::with_capacity(w * h / 16 + 1);
        for_each_stratified(w, h, |i| {
            if quiet[i] && noise_map[i].is_finite() {
                levels.push(noise_map[i]);
            }
        });
        if levels.is_empty() {
            return None;
        }
        let darker = median_in_place(&mut levels);
        let lags: Vec<(i64, i64)> = (0..=lag)
            .flat_map(|dy| (-lag..=lag).map(move |dx| (dx, dy)))
            .filter(|&(dx, dy)| dy > 0 || dx >= 0)
            .collect();
        let stride = (w * h / NOISE_ANCHOR_TARGET).max(1);
        let rows: Vec<usize> = (0..h - NOISE_LAG_REACH).step_by(stride).collect();
        // Per row in parallel, then summed in row order: a parallel `reduce` combines the rows in whatever tree the
        // work-stealing split built, so its floating-point sum (and the gain) could differ in the last bits between
        // two runs on the same plane.
        let per_row: Vec<(Vec<f64>, Vec<u64>)> = rows
            .par_iter()
            .map(|&y| {
                let mut s = vec![0.0f64; lags.len()];
                let mut n = vec![0u64; lags.len()];
                for x in 0..w {
                    let i = y * w + x;
                    if !quiet[i] || !(noise_map[i] <= darker) {
                        continue;
                    }
                    let ri = (r[i] - med) as f64;
                    for (k, &(dx, dy)) in lags.iter().enumerate() {
                        let xx = x as i64 + dx;
                        if xx < 0 || xx >= w as i64 {
                            continue;
                        }
                        let j = (y + dy as usize) * w + xx as usize;
                        if quiet[j] {
                            s[k] += ri * (r[j] - med) as f64;
                            n[k] += 1;
                        }
                    }
                }
                (s, n)
            })
            .collect();
        let mut sums = vec![0.0f64; lags.len()];
        let mut counts = vec![0u64; lags.len()];
        for (s, n) in &per_row {
            sums.iter_mut().zip(s).for_each(|(a, b)| *a += b);
            counts.iter_mut().zip(n).for_each(|(a, b)| *a += b);
        }
        if (counts[0] as usize) < NOISE_MIN_ANCHORS {
            return None;
        }
        Self::solve(&lags, &sums, &counts)
    }

    /// The least-squares `C` from the residual's autocovariance at `lags` (see [`NoiseCorrelation`]).
    fn solve(lags: &[(i64, i64)], sums: &[f64], counts: &[u64]) -> Option<NoiseCorrelation> {
        let rr = NOISE_CORR_REACH as i64;
        let unknowns: Vec<(i64, i64)> = (0..=rr)
            .flat_map(|dy| (-rr..=rr).map(move |dx| (dx, dy)))
            .filter(|&(dx, dy)| dy > 0 || dx >= 0)
            .collect();
        let index = |dx: i64, dy: i64| -> Option<usize> {
            let (dx, dy) = if dy < 0 || (dy == 0 && dx < 0) {
                (-dx, -dy)
            } else {
                (dx, dy)
            };
            unknowns.iter().position(|&u| u == (dx, dy))
        };
        let h2 = high_pass_autocorrelation();
        let hb = 2 * NOISE_HP_HALF as i64;
        let n = unknowns.len();
        let mut ata = vec![vec![0.0f64; n]; n];
        let mut aty = vec![0.0f64; n];
        for (k, &(lx, ly)) in lags.iter().enumerate() {
            if counts[k] == 0 {
                continue;
            }
            let y = sums[k] / counts[k] as f64;
            let mut row = vec![0.0f64; n];
            for vy in -hb..=hb {
                for vx in -hb..=hb {
                    let (qx, qy) = (lx + vx, ly + vy);
                    if qx.abs() > rr || qy.abs() > rr {
                        continue;
                    }
                    if let Some(u) = index(qx, qy) {
                        row[u] += h2[(vy + hb) as usize][(vx + hb) as usize];
                    }
                }
            }
            for a in 0..n {
                aty[a] += row[a] * y;
                for c in 0..n {
                    ata[a][c] += row[a] * row[c];
                }
            }
        }
        let x = solve_linear(ata, aty)?;
        let mut c = [[0.0f64; NOISE_CORR_SIDE]; NOISE_CORR_SIDE];
        for dy in -rr..=rr {
            for dx in -rr..=rr {
                c[(dy + rr) as usize][(dx + rr) as usize] = x[index(dx, dy)?];
            }
        }
        let c0 = c[NOISE_CORR_REACH][NOISE_CORR_REACH];
        (c0.is_finite() && c0 > 0.0 && c.iter().flatten().all(|v| v.is_finite()))
            .then_some(NoiseCorrelation { c })
    }

    /// The noise's filtered-to-pixel σ ratio for [`MatchedFilter`] `fwhm` on a grid of `step` px per cell (`f =
    /// Σk·sum/√(Σk²·count)`, every cell full): `G² = Σ_L C(L)·A(L) / (A(0)·C(0))`, `A` the autocorrelation of the
    /// kernel's pixel weights (each cell's tap on its `step × step` pixels). 1 on white noise; NaN when not a
    /// positive number.
    fn gain(&self, fwhm: f64, step: usize) -> f64 {
        let taps = MatchedFilter { fwhm_px: fwhm }.taps(step);
        let reach = taps.len() - 1;
        let weights: Vec<f64> = (0..2 * reach + 1)
            .flat_map(|c| std::iter::repeat(taps[c.abs_diff(reach)] as f64).take(step.max(1)))
            .collect();
        let auto = |l: usize| -> f64 {
            weights
                .iter()
                .zip(weights.iter().skip(l))
                .map(|(a, b)| a * b)
                .sum()
        };
        let a: Vec<f64> = (0..=NOISE_CORR_REACH).map(auto).collect();
        let rr = NOISE_CORR_REACH as i64;
        let mut num = 0.0f64;
        for dy in -rr..=rr {
            for dx in -rr..=rr {
                num += self.c[(dy + rr) as usize][(dx + rr) as usize]
                    * a[dx.unsigned_abs() as usize]
                    * a[dy.unsigned_abs() as usize];
            }
        }
        let g2 = num / (a[0] * a[0] * self.c[NOISE_CORR_REACH][NOISE_CORR_REACH]);
        if g2.is_finite() && g2 > 0.0 {
            g2.sqrt()
        } else {
            f64::NAN
        }
    }
}

/// Output rows per block of [`high_pass`]: each block holds its own row sums (its rows ± the box's half-width).
const HIGH_PASS_BLOCK_ROWS: usize = 64;

/// The high-pass residual of the noise read: `r(i) = (25·d(i) − Σ_box d)/24` over the 5×5 box about `i`, NaN where
/// any pixel of the box is absent or outside the plane.
fn high_pass(d: &[f32], present: Option<&[bool]>, w: usize, h: usize) -> Vec<f32> {
    let b = NOISE_HP_HALF;
    let side = 2 * b + 1;
    let at = |i: usize| -> (f32, u8) {
        if present.map_or(true, |p| p[i]) {
            (d[i], 1)
        } else {
            (0.0, 0)
        }
    };
    // Row sums over the box's width, then column sums of those: the box sum and its present count. The row sums are
    // held per block of output rows (its rows and the box's reach about them), never for the whole plane.
    let row_sums = |y: usize, out: &mut [(f32, u8)]| {
        for x in b..w - b {
            let (mut s, mut n) = (0.0f32, 0u8);
            for xx in x - b..=x + b {
                let (v, k) = at(y * w + xx);
                s += v;
                n += k;
            }
            out[x] = (s, n);
        }
    };
    let full = (side * side) as u8;
    let mut r = vec![f32::NAN; w * h];
    r.par_chunks_mut(w * HIGH_PASS_BLOCK_ROWS)
        .enumerate()
        .for_each(|(k, block)| {
            let y0 = k * HIGH_PASS_BLOCK_ROWS;
            let y1 = y0 + block.len() / w;
            let (lo, hi) = (y0.saturating_sub(b), (y1 + b).min(h));
            let mut rows = vec![(0.0f32, 0u8); (hi - lo) * w];
            for (yy, out) in (lo..hi).zip(rows.chunks_mut(w)) {
                row_sums(yy, out);
            }
            for (y, out) in (y0..y1).zip(block.chunks_mut(w)) {
                if y < b || y + b >= h {
                    continue;
                }
                for x in b..w - b {
                    let (mut s, mut n) = (0.0f32, 0u8);
                    for yy in y - b..=y + b {
                        let (v, k) = rows[(yy - lo) * w + x];
                        s += v;
                        n += k;
                    }
                    if n == full {
                        out[x] = (full as f32 * d[y * w + x] - s) / (full - 1) as f32;
                    }
                }
            }
        });
    r
}

/// The residuals the noise read keeps: finite, and no source (a residual beyond [`NOISE_CLIP_SIGMAS`]·`sigma` of
/// `med`) within [`NOISE_CLIP_DILATE_PX`] px (Chebyshev).
fn quiet_residuals(r: &[f32], med: f32, sigma: f32, w: usize, h: usize) -> Vec<bool> {
    let k = NOISE_CLIP_DILATE_PX;
    // Per block of output rows: each needed row's sources and their reach along the row (a running count), then the
    // reach down the columns (a running count over the rows) — no whole-plane source or reach map is kept.
    let mut quiet = vec![false; w * h];
    quiet
        .par_chunks_mut(w * QUIET_BLOCK_ROWS)
        .enumerate()
        .for_each(|(b, block)| {
            let y0 = b * QUIET_BLOCK_ROWS;
            let y1 = y0 + block.len() / w;
            let (lo, hi) = (y0.saturating_sub(k), (y1 + k).min(h));
            // `below[(yy − lo + 1)·w + x]` = rows lo..=yy whose reach covers column x.
            let mut below = vec![0u32; (hi - lo + 1) * w];
            let mut prefix = vec![0u32; w + 1];
            for yy in lo..hi {
                let row = &r[yy * w..(yy + 1) * w];
                for (x, &v) in row.iter().enumerate() {
                    let source = v.is_finite() && (v - med).abs() > NOISE_CLIP_SIGMAS * sigma;
                    prefix[x + 1] = prefix[x] + u32::from(source);
                }
                let (prev, cur) = below.split_at_mut((yy - lo + 1) * w);
                let prev = &prev[(yy - lo) * w..];
                for x in 0..w {
                    let reach = prefix[(x + k).min(w - 1) + 1] - prefix[x.saturating_sub(k)];
                    cur[x] = prev[x] + u32::from(reach > 0);
                }
            }
            for (y, out) in (y0..y1).zip(block.chunks_mut(w)) {
                let (a, z) = (y.saturating_sub(k), (y + k).min(h - 1));
                for x in 0..w {
                    let near = below[(z - lo + 1) * w + x] - below[(a - lo) * w + x] > 0;
                    out[x] = r[y * w + x].is_finite() && !near;
                }
            }
        });
    quiet
}

/// Output rows per block of [`quiet_residuals`]: each block counts its own rows' sources (its rows ± the dilation).
const QUIET_BLOCK_ROWS: usize = 64;

/// The autocorrelation `H₂(v) = Σ_u hp(u)·hp(u + v)` of the high-pass box `hp` (1 at the centre, −1/24 on the other
/// 24 pixels), `|v|∞ ≤ 2·NOISE_HP_HALF`, indexed `[vy + 2B][vx + 2B]`.
fn high_pass_autocorrelation() -> Vec<Vec<f64>> {
    let b = NOISE_HP_HALF as i64;
    let side = 2 * b + 1;
    let hp = |x: i64, y: i64| -> f64 {
        if x.abs() > b || y.abs() > b {
            0.0
        } else if x == 0 && y == 0 {
            1.0
        } else {
            -1.0 / ((side * side - 1) as f64)
        }
    };
    let hb = 2 * b;
    (-hb..=hb)
        .map(|vy| {
            (-hb..=hb)
                .map(|vx| {
                    let mut s = 0.0;
                    for uy in -b..=b {
                        for ux in -b..=b {
                            s += hp(ux, uy) * hp(ux + vx, uy + vy);
                        }
                    }
                    s
                })
                .collect()
        })
        .collect()
}

/// Gaussian elimination with partial pivoting; `None` when the system is singular.
fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    let scale = a.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
    if !(scale > 0.0) {
        return None;
    }
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if !(a[pivot][col].abs() > 1e-12 * scale) {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            for k in col..n {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0f64; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    Some(x)
}

/// The knot tests' one-sided level (Task 10b ruling A3, D2): z = 3.09, a 0.1 % false-alarm rate per seed.
pub const KNOT_Z: f64 = 3.09;

/// The read-error law of one half-maximum read (Task 10b A2b, ruling A2-2): a read at peak SNR ν ([`HalfmaxRead`]'s
/// `snr`) misreads its star's width by the relative error e(ν) = √((κ/ν)² + f₀²) (1σ), κ = `READ_ERROR_KAPPA` = 0.899
/// and f₀ = `READ_ERROR_FLOOR` = 0.0208 — fitted on 12 constant-width lab families (residual 0.0036). κ agrees with the
/// peak's and the ring's noise propagated through the profile's slope at the half-maximum crossing (κ ≈ 1.0).
const READ_ERROR_KAPPA: f64 = 0.899;
/// f₀ of the read-error law (see [`READ_ERROR_KAPPA`]): the relative error no SNR removes.
const READ_ERROR_FLOOR: f64 = 0.0208;

/// e(ν), the relative error of a half-maximum read at peak SNR `nu` (see [`READ_ERROR_KAPPA`]).
fn read_error(nu: f64) -> f64 {
    (READ_ERROR_KAPPA / nu).hypot(READ_ERROR_FLOOR)
}

/// The field's star-width envelope (Task 10b A2b; rulings A2-1, A2-2): the FWHM the knot bounds' β 1.5 star is drawn
/// at, F_env = F_field + [`KNOT_Z`]·s. F_field is the MODE of the stars' widths (A2), so half the real stars are broader
/// than it, and a bound at F_field flags them extended; the bound sits at the widths' upper envelope at the knot tests'
/// own level instead. `s` is the stars' width spread: 1.4826 × the median of F_field − w over the readable reads w (the
/// field width's own pool, [`field_width_of`]) with `floor ≤ w ≤ F_field` — the lower half about F_field, mirrored:
///
/// - the LOWER half, because every contaminant of a half-maximum read the mode leaves out is WIDER than the PSF (a
///   clipped core, a blend, a knot, a sky bump: A2's derivation), so the upper half carries them and the lower does not;
/// - the floor `F_field·(1 − KNOT_Z·e(10))` = 0.715·F_field, because a star at least F_field wide reads, at the knot
///   tests' 3.09 σ, no narrower than `F_field·(1 − 3.09·e(ν))`, and e is largest at the readability gate ν = 10
///   (`WIDTH_START_MIN_PEAK_SNR`, Task 6b R1; [`read_error`]). A read below it is no star of the field's width but a
///   narrower event — a single-pixel event reads ≈ 1.3 px at SNR 10–18, up to 29 % of a real frame's readable reads —
///   that would otherwise fill the lower half and set its median.
///
/// The measured lower-half spread carries each read's own half-maximum error, which is not the stars' spread: the knot
/// tests' 3.09·σ_r already adds every seed's noise, so the envelope is the TRUE width spread — the measured spread with
/// the mean of (F_field·e(ν_i))² over the reads that set it removed in quadrature, `s² = max(0, s_measured² − mean)`
/// (Task 10b A2b, ruling A2-3: E4n). On stars of one width the spread reads 0 (F_env = F_field), not the read noise.
///
/// No readable read in `[floor, F_field]` (none at all, or a field width clamped below every read): `s = 0` and the
/// bounds sit at the field width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidthEnvelope {
    /// `s_true`, px: the stars' width spread about the field width, the reads' own error removed.
    pub spread_px: f64,
    /// F_env = F_field + [`KNOT_Z`]·`spread_px`, px.
    pub fwhm_px: f64,
}

impl WidthEnvelope {
    /// No spread: the envelope is the field width `field` itself.
    fn at(field: f64) -> WidthEnvelope {
        WidthEnvelope {
            spread_px: 0.0,
            fwhm_px: field,
        }
    }

    /// The envelope of the readable reads `reads` (width px, peak SNR; any order) about the field width `field` (see
    /// [`WidthEnvelope`]); one `warn!` when no read lies between the floor and the field width.
    fn of(reads: &[(f64, f64)], field: f64) -> WidthEnvelope {
        let floor = field * (1.0 - KNOT_Z * read_error(WIDTH_START_MIN_PEAK_SNR));
        let mut below: Vec<f64> = Vec::new();
        let mut noise_var = 0.0f64;
        for &(width, snr) in reads {
            if width >= floor && width <= field {
                below.push(field - width);
                noise_var += (field * read_error(snr)).powi(2);
            }
        }
        if below.is_empty() {
            warn!(
                fwhm_px = field,
                count = reads.len(),
                "field width envelope: no readable read between the floor and the field width; the knot bounds sit at the field width"
            );
            return WidthEnvelope::at(field);
        }
        let noise_var = noise_var / below.len() as f64;
        let measured = f64::from(MAD_TO_SIGMA) * median_f64(&mut below);
        let spread = (measured * measured - noise_var).max(0.0).sqrt();
        WidthEnvelope {
            spread_px: spread,
            fwhm_px: field + KNOT_Z * spread,
        }
    }
}

/// One seed's knot metrics (Task 10b ruling A3, D2 §3): Gaussian kernels at `fwhm` (F) and `2·fwhm` (2F) centred on the
/// seed's sub-pixel position. `snr`, `snr_2f`: the plain responses `Σk·d/√Σk²` (each kernel truncated at 3σ of its
/// own), over `sigma`; `rho` their kernels' correlation. `z`, `z_2f`: the zero-sum responses over the 2F box (each
/// kernel with the box mean removed), over `sigma`; `rho_z` their correlation. Absent pixels (`present[i] == false`, or
/// outside the plane) leave every sum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnotMetrics {
    pub snr: f64,
    pub snr_2f: f64,
    pub z: f64,
    pub z_2f: f64,
    pub rho: f64,
    pub rho_z: f64,
}

/// The bounds of the two ratios: [`knot_metrics`] at the field width of a pixel-centred, point-sampled Moffat β =
/// `BETA_BOUNDS.0` star — the broadest profile the fit models — of FWHM the field's star-width envelope F_env
/// ([`WidthEnvelope`], Task 10b A2b) (`plain` = `snr_2f/snr`, `zero_sum` = `z_2f/z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnotBounds {
    pub plain: f64,
    pub zero_sum: f64,
}

/// A seed's knot verdict (Task 10b ruling A3): a point source, an extended one (kept, flagged) or structure (dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnotVerdict {
    Star,
    Extended,
    Structure,
}

/// The knot metrics of the seed at `(x, y)` on the background-subtracted plane `d` (see [`KnotMetrics`]).
#[allow(clippy::too_many_arguments)]
pub fn knot_metrics(
    d: &[f32],
    present: Option<&[bool]>,
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    fwhm: f64,
    sigma: f64,
) -> KnotMetrics {
    let s1 = fwhm / GAUSSIAN_FWHM_PER_SIGMA;
    let s2 = 2.0 * s1;
    let r1 = (KERNEL_REACH_SIGMAS * s1).ceil() as i64;
    let r2 = (KERNEL_REACH_SIGMAS * s2).ceil() as i64;
    let (px, py) = (x.round() as i64, y.round() as i64);
    // The separable kernels' factors along each axis, centred on the sub-pixel position.
    let axis = |c: i64, at: f64, s: f64| -> Vec<f64> {
        (-r2..=r2)
            .map(|k| {
                let e = (c + k) as f64 - at;
                (-e * e / (2.0 * s * s)).exp()
            })
            .collect()
    };
    let (g1x, g1y, g2x, g2y) = (
        axis(px, x, s1),
        axis(py, y, s1),
        axis(px, x, s2),
        axis(py, y, s2),
    );
    let (mut n1, mut q1, mut n2, mut q2, mut c12) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let (mut sg1, mut sg2, mut sv, mut sg1v, mut sg2v) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let (mut sg1g1, mut sg2g2, mut sg1g2, mut nn) = (0f64, 0f64, 0f64, 0f64);
    for dy in -r2..=r2 {
        let yy = py + dy;
        if yy < 0 || yy >= h as i64 {
            continue;
        }
        for dx in -r2..=r2 {
            let xx = px + dx;
            if xx < 0 || xx >= w as i64 {
                continue;
            }
            let i = yy as usize * w + xx as usize;
            if !present.map_or(true, |p| p[i]) {
                continue;
            }
            let v = d[i] as f64;
            let (kx, ky) = ((dx + r2) as usize, (dy + r2) as usize);
            let g1 = g1x[kx] * g1y[ky];
            let g2 = g2x[kx] * g2y[ky];
            if dx.abs() <= r1 && dy.abs() <= r1 {
                n1 += g1 * v;
                q1 += g1 * g1;
                c12 += g1 * g2;
            }
            n2 += g2 * v;
            q2 += g2 * g2;
            sg1 += g1;
            sg2 += g2;
            sv += v;
            sg1v += g1 * v;
            sg2v += g2 * v;
            sg1g1 += g1 * g1;
            sg2g2 += g2 * g2;
            sg1g2 += g1 * g2;
            nn += 1.0;
        }
    }
    let dz1 = sg1g1 - sg1 * sg1 / nn;
    let dz2 = sg2g2 - sg2 * sg2 / nn;
    let cz = sg1g2 - sg1 * sg2 / nn;
    KnotMetrics {
        snr: n1 / q1.sqrt() / sigma,
        snr_2f: n2 / q2.sqrt() / sigma,
        z: (sg1v - sg1 * sv / nn) / dz1.sqrt() / sigma,
        z_2f: (sg2v - sg2 * sv / nn) / dz2.sqrt() / sigma,
        rho: c12 / (q1 * q2).sqrt(),
        rho_z: cz / (dz1 * dz2).sqrt(),
    }
}

/// The knot bounds of the kernels at `fwhm` (see [`KnotBounds`]): the metrics, through the kernels at `fwhm`, of a
/// noiseless Moffat β = `BETA_BOUNDS.0` star of FWHM `star_fwhm` — the field's star-width envelope (Task 10b A2b;
/// `star_fwhm = fwhm` is A3's bound at the field width) — point-sampled and centred on a pixel of a stamp wide enough
/// to hold the 2F box.
pub fn knot_bounds(fwhm: f64, star_fwhm: f64) -> KnotBounds {
    let beta = BETA_BOUNDS.0;
    let s = fwhm / GAUSSIAN_FWHM_PER_SIGMA;
    let alpha = star_fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt());
    let half = ((2.0 * KERNEL_REACH_SIGMAS * s).ceil() as usize + 2).max(50);
    let n = 2 * half + 1;
    let stamp: Vec<f32> = (0..n * n)
        .map(|i| {
            let (dx, dy) = ((i % n) as f64 - half as f64, (i / n) as f64 - half as f64);
            (1.0 + (dx * dx + dy * dy) / (alpha * alpha)).powf(-beta) as f32
        })
        .collect();
    let m = knot_metrics(&stamp, None, n, n, half as f64, half as f64, fwhm, 1.0);
    KnotBounds {
        plain: m.snr_2f / m.snr,
        zero_sum: m.z_2f / m.z,
    }
}

/// The knot verdict (Task 10b ruling A3; D2 §3, thresholds derived from the PSF and the noise, never tuned): a
/// `near_clip` seed is a star (its clipped core is the saturation flag's, Task 10c); else STRUCTURE when its plain
/// ratio `snr_2f/snr` exceeds `bounds.plain` by [`KNOT_Z`] of the ratio's noise `√(1 + p² − 2pρ)/snr` AND its zero-sum
/// response `z` is below `threshold` (no compact source at F); else EXTENDED when `z > 0` and its zero-sum ratio
/// `z_2f/z` exceeds `bounds.zero_sum` by [`KNOT_Z`] of `√(1 + q² − 2qρ_z)/z`; else a star. A metric that is not a
/// number decides nothing.
pub fn knot_verdict(
    m: &KnotMetrics,
    bounds: KnotBounds,
    threshold: f64,
    near_clip: bool,
) -> KnotVerdict {
    if near_clip {
        return KnotVerdict::Star;
    }
    let p = bounds.plain;
    if m.snr > 0.0 {
        let ratio = m.snr_2f / m.snr;
        let noise = (1.0 + p * p - 2.0 * p * m.rho).max(0.0).sqrt() / m.snr;
        if ratio > p + KNOT_Z * noise && m.z < threshold {
            return KnotVerdict::Structure;
        }
    }
    let q = bounds.zero_sum;
    if m.z > 0.0 {
        let ratio = m.z_2f / m.z;
        let noise = (1.0 + q * q - 2.0 * q * m.rho_z).max(0.0).sqrt() / m.z;
        if ratio > q + KNOT_Z * noise {
            return KnotVerdict::Extended;
        }
    }
    KnotVerdict::Star
}

/// PA-17: the azimuthal half-maximum width of one star — rings of 0.5 px about the brightest pixel in a box of
/// half-width `box_r`, background = the box rim's median, FWHM = 2 × the radius where the ring-mean profile
/// crosses half the brightest pixel (linear interpolation between the bracketing rings). `None` when the profile
/// never crosses inside the box or the box leaves the image/mask.
///
/// The brightest pixel is the brightest present pixel within 2 px of `(x, y)`, and the box is centred on it. Absent
/// pixels (non-finite, or `false` in `mask`) are left out of every ring and of the rim; a ring that holds no present
/// pixel is skipped (rings of 0.5 px about a pixel centre are empty between the distances the grid holds), and a
/// ring's radius is the mean distance of its pixels. The box "leaves the image/mask" when it crosses the image edge
/// or its rim holds no present pixel. Only the pixels within `box_r` of the centre form rings.
pub fn halfmax_width(
    plane: &[f32],
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    box_r: usize,
    mask: Option<&[bool]>,
) -> Option<f64> {
    halfmax_read(plane, w, h, x, y, box_r, mask).map(|r| r.width)
}

/// One half-maximum read (Task 10b A2): [`halfmax_width`]'s width and the read's own SNR — the brightest pixel's height
/// above the box rim's median over the rim's robust σ (1.4826·MAD); infinite on a rim with no spread, so any peak above
/// it reads (the width start's readability convention, Task 6b R1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HalfmaxRead {
    pub(crate) width: f64,
    pub(crate) snr: f64,
}

/// [`halfmax_width`] with its read's SNR ([`HalfmaxRead`]).
fn halfmax_read(
    plane: &[f32],
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    box_r: usize,
    mask: Option<&[bool]>,
) -> Option<HalfmaxRead> {
    if box_r == 0 || !x.is_finite() || !y.is_finite() || plane.len() != w * h {
        return None;
    }
    let present = |i: usize| plane[i].is_finite() && mask.map_or(true, |m| m[i]);
    let (sx, sy) = (x.round() as i64, y.round() as i64);
    let mut best: Option<(i64, i64, f32)> = None;
    for yy in sy - HALFMAX_PEAK_SEARCH_PX..=sy + HALFMAX_PEAK_SEARCH_PX {
        for xx in sx - HALFMAX_PEAK_SEARCH_PX..=sx + HALFMAX_PEAK_SEARCH_PX {
            if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                continue;
            }
            let i = yy as usize * w + xx as usize;
            if present(i) && best.map_or(true, |(_, _, v)| plane[i] > v) {
                best = Some((xx, yy, plane[i]));
            }
        }
    }
    let (px, py, top) = best?;
    let r = box_r as i64;
    if px - r < 0 || py - r < 0 || px + r >= w as i64 || py + r >= h as i64 {
        return None;
    }
    let at = |dx: i64, dy: i64| ((py + dy) as usize) * w + (px + dx) as usize;
    let mut rim: Vec<f32> = Vec::with_capacity(8 * box_r);
    for d in -r..=r {
        for (dx, dy) in [(d, -r), (d, r)] {
            if present(at(dx, dy)) {
                rim.push(plane[at(dx, dy)]);
            }
        }
        if d > -r && d < r {
            for (dx, dy) in [(-r, d), (r, d)] {
                if present(at(dx, dy)) {
                    rim.push(plane[at(dx, dy)]);
                }
            }
        }
    }
    if rim.is_empty() {
        return None;
    }
    let level = median_in_place(&mut rim);
    let background = level as f64;
    let c0 = top as f64 - background;
    if !(c0 > 0.0) {
        return None;
    }
    let sigma_rim = (MAD_TO_SIGMA * mad_about(&rim, level)) as f64;
    let snr = if sigma_rim > 0.0 {
        c0 / sigma_rim
    } else {
        f64::INFINITY
    };
    // Rings k = ⌊d/0.5⌋ for 0 < d ≤ box_r: (Σ value, Σ distance, count).
    let mut rings = vec![(0.0f64, 0.0f64, 0u32); 2 * box_r + 1];
    for dy in -r..=r {
        for dx in -r..=r {
            if dx == 0 && dy == 0 {
                continue;
            }
            let dist = (dx as f64).hypot(dy as f64);
            if dist > box_r as f64 {
                continue;
            }
            let i = at(dx, dy);
            if !present(i) {
                continue;
            }
            let ring = &mut rings[(dist / 0.5) as usize];
            ring.0 += plane[i] as f64;
            ring.1 += dist;
            ring.2 += 1;
        }
    }
    let half = 0.5 * c0;
    let (mut r_prev, mut v_prev) = (0.0f64, c0);
    for &(sum, dist, n) in rings.iter().skip(1) {
        if n == 0 {
            continue;
        }
        let (rk, vk) = (dist / n as f64, sum / n as f64 - background);
        if vk <= half {
            let t = (v_prev - half) / (v_prev - vk);
            return Some(HalfmaxRead {
                width: 2.0 * (r_prev + t * (rk - r_prev)),
                snr,
            });
        }
        r_prev = rk;
        v_prev = vk;
    }
    None
}

/// One star's own half-maximum width (PA-17): [`halfmax_width`] in a box of half-width 15 px, read again in a box of
/// `⌈3·F⌉` (≤ 48) when the first read is ≥ 7 px (a re-read whose box leaves the image keeps the first). `None` when no
/// read crosses half the maximum. The field width's per-star read, and the fit's width start where the star is
/// readable (Task 6b round 1, ruling R1).
pub(crate) fn own_halfmax_width(
    plane: &[f32],
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    mask: Option<&[bool]>,
) -> Option<f64> {
    own_halfmax_read(plane, w, h, x, y, mask).map(|r| r.width)
}

/// [`own_halfmax_width`] with the SNR of the read it keeps (the re-read's when the wider box read, else the first's).
fn own_halfmax_read(
    plane: &[f32],
    w: usize,
    h: usize,
    x: f64,
    y: f64,
    mask: Option<&[bool]>,
) -> Option<HalfmaxRead> {
    let first = halfmax_read(plane, w, h, x, y, HALFMAX_BOX_PX, mask)?;
    if first.width < HALFMAX_REMEASURE_FROM_PX {
        return Some(first);
    }
    let wide = ((3.0 * first.width).ceil() as usize).min(HALFMAX_BOX_MAX_PX);
    Some(halfmax_read(plane, w, h, x, y, wide, mask).unwrap_or(first))
}

/// The field width of a pool of positions (Task 10b A2, ruling A5; PA-17 revised): every position's own half-maximum
/// read ([`own_halfmax_read`]) that is READABLE — its SNR at least the same level as the width start's readability gate
/// (`WIDTH_START_MIN_PEAK_SNR`, 10, Task 6b R1), read in this half-maximum read's own box and rim, not the fit's ⌈2F⌉ box — and, of those widths, the half-sample mode ([`half_sample_mode`]),
/// clamped to [1.5, 24] px; `from_stars` counts the readable reads. No count, no ranking and no saturation test (F25
/// stands): every contaminant of a half-maximum read is WIDER than the PSF — a clipped core is read at half the clip,
/// outside the PSF's half maximum; a blend, a knot or a sky bump is a broader profile — while the stars share one
/// width, so the PSF is the reads' mode wherever the brightest candidates sit. No readable read → the first pass's 3 px with
/// `from_stars = 0`. Reads run in parallel and are sorted before the mode, so the width does not depend on the order
/// of `at`.
fn field_width_from(
    plane: &[f32],
    w: usize,
    h: usize,
    at: &[(f64, f64)],
    mask: Option<&[bool]>,
) -> FieldWidth {
    let mut widths: Vec<f64> = readable_widths(plane, w, h, at, mask)
        .into_iter()
        .map(|(width, _)| width)
        .collect();
    field_width_of_reads(&mut widths)
}

/// The (width, peak SNR) of the readable half-maximum reads of the positions `at` ([`field_width_from`]'s pool; the
/// knot bounds' envelope reads the same pool, [`WidthEnvelope`], and removes each read's error from the spread).
fn readable_widths(
    plane: &[f32],
    w: usize,
    h: usize,
    at: &[(f64, f64)],
    mask: Option<&[bool]>,
) -> Vec<(f64, f64)> {
    at.par_iter()
        .filter_map(|&(x, y)| own_halfmax_read(plane, w, h, x, y, mask))
        .filter(|r| r.snr >= WIDTH_START_MIN_PEAK_SNR)
        .map(|r| (r.width, r.snr))
        .collect()
}

/// The field width of the readable reads `widths` ([`field_width_from`]): their half-sample mode, clamped; sorts
/// `widths`.
fn field_width_of_reads(widths: &mut [f64]) -> FieldWidth {
    if widths.is_empty() {
        return FieldWidth::unmeasured();
    }
    let n = widths.len();
    FieldWidth {
        fwhm_px: half_sample_mode(widths).clamp(FIELD_WIDTH_MIN_PX, FIELD_WIDTH_MAX_PX),
        from_stars: n,
    }
}

/// The half-sample mode (Bickel's): sorted, the half of the values with the smallest range is kept, again and again,
/// until at most three remain; of three, the mean of the closer pair (the middle one when both gaps are equal), of two
/// their mean. Parameter-free; `v` is sorted in place and must not be empty.
fn half_sample_mode(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let mut s: &[f64] = v;
    while s.len() > 3 {
        let half = s.len().div_ceil(2);
        let mut best = 0usize;
        for i in 1..=s.len() - half {
            if s[i + half - 1] - s[i] < s[best + half - 1] - s[best] {
                best = i;
            }
        }
        s = &s[best..best + half];
    }
    match *s {
        [a, b, c] => {
            let (lo, hi) = (b - a, c - b);
            if lo < hi {
                0.5 * (a + b)
            } else if hi < lo {
                0.5 * (b + c)
            } else {
                b
            }
        }
        [a, b] => 0.5 * (a + b),
        [a] => a,
        _ => f64::NAN,
    }
}

/// The field width of GIVEN seeds (I-4, PA-17 as revised by Task 10b A2): the same rule as the matched filter's own
/// width ([`field_width_from`]) — the half-sample mode of the half-maximum reads (box 15 px; a first read ≥ 7 px is
/// re-measured in a `3·F` box, ≤ 48, and one whose larger box leaves the image keeps the first) of EVERY seed whose
/// read is readable, clamped to [1.5, 24] px; no ranking (the seeds' order and `peak` do not matter) and no saturation
/// test (ruling F25). `fit_stars_at` calls it when `FitStarsOptions.field_width` is `None` (LN refits, Peak/Structure
/// seeds, Plan C's mosaic sites); the fit's width start reads the field width it returns (Task 6b). No readable read →
/// `from_stars = 0` and the first pass's 3 px.
pub fn field_width_of(
    plane: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    mask: Option<&[bool]>,
) -> FieldWidth {
    let at: Vec<(f64, f64)> = seeds.iter().map(|s| (s.x, s.y)).collect();
    field_width_from(plane, w, h, &at, mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::{domain, seeds, ADU_SCALE};
    use crate::test_support::fixture_field;
    use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

    fn working(data: &[f32]) -> Vec<f32> {
        data.iter().map(|v| v * ADU_SCALE).collect()
    }

    #[test]
    fn matched_filter_finds_every_star_of_the_fixture_and_no_noise_peak() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002); // 150 stars, σ 1.8 px (FWHM 4.24)
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (seeds, fw, _) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        assert_eq!(seeds.len(), 150);
        assert!((fw.fwhm_px - 1.8 * 2.3548).abs() < 0.3, "{fw:?}");
        assert!(seeds.windows(2).all(|p| p[0].flux >= p[1].flux));
        assert!(seeds.iter().all(|s| s.peak > 0.0));
    }

    /// Controller ruling (Task 6a, finding 1): a broad faint star is a star. Twenty Gaussian stars at FWHM 8 px and
    /// Fisher SNR 20, each centred on a pixel, on a plane of known noise: every one passes the hot-pixel neighbour test
    /// at `DEFAULT_THRESHOLD`. Its neighbours hold ≈ 3.2 σ_plane each, below `threshold·σ_plane`, so the brief's
    /// literal level failed most of them.
    #[test]
    fn a_broad_star_at_fisher_snr_20_passes_the_neighbour_test() {
        use crate::test_support::{add_noise, gaussian_field};
        let (w, h, sky, noise) = (400usize, 320usize, 1000.0f32, 50.0f32);
        let sigma = 8.0 / GAUSSIAN_FWHM_PER_SIGMA;
        // Fisher SNR of a point-sampled Gaussian of amplitude A on white noise: A·√(Σg²)/σ.
        let g2: f64 = (-40i64..=40)
            .flat_map(|y| (-40i64..=40).map(move |x| (x * x + y * y) as f64))
            .map(|r2| (-r2 / (sigma * sigma)).exp())
            .sum();
        let amp = 20.0 * noise as f64 / g2.sqrt();
        let centres: Vec<(usize, usize)> = (0..20)
            .map(|i| (50 + (i % 5) * 75, 50 + (i / 5) * 75))
            .collect();
        let stars: Vec<(f64, f64, f64)> = centres
            .iter()
            .map(|&(x, y)| (x as f64, y as f64, amp))
            .collect();
        let mut data = gaussian_field(w, h, &stars, sigma, sky);
        add_noise(&mut data, noise, 17);
        let d: Vec<f32> = data.iter().map(|v| v - sky).collect();
        let det = Detection {
            d: &d,
            present: None,
            w,
            h,
            sigma_plane: noise,
            threshold: DEFAULT_THRESHOLD,
        };
        let passed = centres
            .iter()
            .filter(|&&(x, y)| det.has_neighbours(x, y))
            .count();
        assert_eq!(
            passed, 20,
            "a FWHM-8 star at Fisher SNR 20 is a star, not a hot pixel"
        );
        let (noise_model, _) = domain::noise_model(&data, w, h, None);
        let (seeds, _, _) = seeds::detect(
            &data,
            w,
            h,
            &noise_model,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        let found = centres
            .iter()
            .filter(|&&(x, y)| {
                seeds
                    .iter()
                    .any(|s| (s.x - x as f64).hypot(s.y - y as f64) < 2.0)
            })
            .count();
        assert_eq!(found, 20, "{seeds:?}");
    }

    #[test]
    fn hot_pixel_is_not_seeded() {
        let (mut data, w, h) = fixture_field(3, 0.0, 0.002); // the noise field without stars
        data[100 * w + 100] = 0.9;
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (seeds, _, _) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        assert!(
            seeds
                .iter()
                .all(|s| (s.x - 100.0).abs() > 2.0 || (s.y - 100.0).abs() > 2.0),
            "{seeds:?}"
        );
    }

    /// A noiseless round star of FWHM `fwhm` and peak 1 at (x, y) on an n × n zero plane: Moffat of `beta`, or Gaussian.
    fn round_star(n: usize, x: f64, y: f64, fwhm: f64, beta: Option<f64>) -> Vec<f32> {
        match beta {
            Some(b) => {
                let a = fwhm / (2.0 * (2f64.powf(1.0 / b) - 1.0).sqrt());
                moffat_field(
                    n,
                    n,
                    &[MoffatStar {
                        x,
                        y,
                        amp: 1.0,
                        alpha_x: a,
                        alpha_y: a,
                        theta: 0.0,
                    }],
                    b,
                    0.0,
                )
            }
            None => gaussian_field(n, n, &[(x, y, 1.0)], fwhm / GAUSSIAN_FWHM_PER_SIGMA, 0.0),
        }
    }

    /// Controller arithmetic (a replica of D2's metric): Moffat β 1.5 at the field width reads plain / zero-sum
    /// 0.9113 / 0.8854 at F 1.2, 0.9716 / 0.9351 at F 2.6, 0.9725 / 0.9328 at F 5, 0.9726 / 0.9326 at F 8.
    #[test]
    fn knot_bounds_are_a_moffat_one_and_a_half_star_at_the_field_width() {
        for (fwhm, plain, zero_sum) in [
            (1.2, 0.9113, 0.8854),
            (2.6, 0.9716, 0.9351),
            (5.0, 0.9725, 0.9328),
            (8.0, 0.9726, 0.9326),
        ] {
            let b = knot_bounds(fwhm, fwhm);
            assert!(
                (b.plain - plain).abs() < 0.002 && (b.zero_sum - zero_sum).abs() < 0.002,
                "F {fwhm}: {b:?}"
            );
        }
    }

    /// Real profiles at the field width are stars at every SNR (replica: Moffat 2.5 reads 0.9025 / 0.8921, the widest).
    #[test]
    fn star_profiles_are_neither_structure_nor_extended() {
        for fwhm in [2.6, 5.0] {
            let bounds = knot_bounds(fwhm, fwhm);
            for beta in [Some(2.5), Some(4.0), None] {
                let d = round_star(121, 60.3, 59.8, fwhm, beta);
                let unit = knot_metrics(&d, None, 121, 121, 60.3, 59.8, fwhm, 1.0);
                for snr in [8.0, 2000.0] {
                    let m = knot_metrics(&d, None, 121, 121, 60.3, 59.8, fwhm, unit.snr / snr);
                    assert_eq!(
                        knot_verdict(&m, bounds, DEFAULT_THRESHOLD, false),
                        KnotVerdict::Star,
                        "F {fwhm} β {beta:?} snr {snr}: {m:?}"
                    );
                }
            }
        }
    }

    /// Replica: a Gaussian blob of FWHM 4F at SNR 8 reads plain 1.70, z(F) 3.99 < K (structure); of FWHM 3F at SNR 40,
    /// z(F) 26.6 ≥ K and zero-sum 1.48 (extended); a near-clip seed is exempt.
    #[test]
    fn a_faint_broad_blob_is_structure_and_a_bright_one_extended() {
        let f = 5.0;
        let bounds = knot_bounds(f, f);
        let at_snr = |k: f64, snr: f64| {
            let d = round_star(161, 80.3, 79.8, k * f, None);
            let unit = knot_metrics(&d, None, 161, 161, 80.3, 79.8, f, 1.0);
            knot_metrics(&d, None, 161, 161, 80.3, 79.8, f, unit.snr / snr)
        };
        let faint = at_snr(4.0, 8.0);
        assert!(faint.z < DEFAULT_THRESHOLD, "{faint:?}");
        assert_eq!(
            knot_verdict(&faint, bounds, DEFAULT_THRESHOLD, false),
            KnotVerdict::Structure
        );
        assert_eq!(
            knot_verdict(&faint, bounds, DEFAULT_THRESHOLD, true),
            KnotVerdict::Star
        );
        assert_eq!(
            knot_verdict(&at_snr(3.0, 40.0), bounds, DEFAULT_THRESHOLD, false),
            KnotVerdict::Extended
        );
    }

    /// The propagated σ_f is the filtered noise wherever the plane holds no structure (D2: 1.00–1.04). Controller
    /// arithmetic: a 3×3 binomial smoothing gives the 3-px kernel a gain Σ(k∗b)²/(Σk²·Σb²) = 2.323² (simulated 2.337).
    #[test]
    fn propagated_noise_is_the_filtered_noise_on_white_and_correlated_noise() {
        let (w, h, sigma) = (512usize, 512usize, 0.002f32);
        let mut white = vec![0.05f32; w * h];
        add_noise(&mut white, sigma, 11);
        let tap = [1.0f32, 2.0, 1.0];
        let smooth: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as i64, (i / w) as i64);
                let (mut s, mut k) = (0.0f32, 0.0f32);
                for dy in -1..=1i64 {
                    for dx in -1..=1i64 {
                        let (xx, yy) = (x + dx, y + dy);
                        if xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64 {
                            let t = tap[(dx + 1) as usize] * tap[(dy + 1) as usize];
                            s += t * white[yy as usize * w + xx as usize];
                            k += t;
                        }
                    }
                }
                s / k
            })
            .collect();
        for (plane, gain) in [(&white, 1.0), (&smooth, 2.33)] {
            let n = seeds::detection_noise(&working(plane), w, h, None, 3.0, PixelNoise::Mixed);
            assert!(
                (n.median() / n.measured - 1.0).abs() < 0.04,
                "propagated {} vs measured {}",
                n.median(),
                n.measured
            );
            assert!(
                (n.gain / gain - 1.0).abs() < 0.10,
                "gain {} vs {gain}",
                n.gain
            );
        }
    }

    /// Smooth structure the 128-px mesh cannot follow (2σ·sin·sin, period 40 px) reaches the measured σ_f (the filter
    /// integrates it) but neither the propagated σ_f nor the gain. Controller arithmetic (numpy, 3-px kernel): measured
    /// 4.73 σ, the mesh's per-cell clipped σ 1.45 σ.
    #[test]
    fn smooth_structure_reaches_the_measured_noise_but_not_the_propagated_one() {
        let (w, h, sigma) = (512usize, 512usize, 0.002f32);
        let tau = std::f64::consts::TAU;
        let mut data: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                0.05 + (2.0 * sigma as f64 * (tau * x / 40.0).sin() * (tau * y / 40.0).sin()) as f32
            })
            .collect();
        add_noise(&mut data, sigma, 5);
        let n = seeds::detection_noise(&working(&data), w, h, None, 3.0, PixelNoise::Mixed);
        let px = sigma as f64 * ADU_SCALE as f64;
        assert!(n.measured > 3.0 * px, "measured {} vs σ {px}", n.measured);
        assert!(
            n.median() > 1.2 * px && n.median() < 1.7 * px,
            "propagated {} vs σ {px}",
            n.median()
        );
        assert!(
            (0.95..1.10).contains(&n.gain),
            "the gain is the noise's, not the sky's: {}",
            n.gain
        );
    }

    /// Controller addendum to A1 (a pin): every returned seed carries its filtered SNR — its maximum's response over the
    /// σ_f it was thresholded at — so each one stands above the threshold, and the report holds one per seed.
    #[test]
    fn every_seed_carries_its_filtered_snr_above_the_threshold() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (seeds, _, rep) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        assert_eq!(rep.filter_snr.len(), seeds.len());
        assert!(rep.noise_gain.is_finite(), "{}", rep.noise_gain);
        assert!(
            rep.filter_snr
                .iter()
                .all(|&v| v > DEFAULT_THRESHOLD * (1.0 - 1e-6)),
            "{:?}",
            rep.filter_snr
        );
    }

    /// The 150-star clean fixture: every star still seeded, nothing dropped or extended, and the propagated σ_f is the
    /// fixture's TRUE noise (0.002 · `ADU_SCALE`) within 4 %. The measured σ_f is reported only: on this fixture it
    /// carries the 150 bright stars' filtered wings (≈ 10 % of the plane; 1.12 × the truth already before A1), so
    /// "propagated = measured" holds on sparse fields only (ruling A1-1).
    #[test]
    fn propagated_detection_noise_is_the_true_noise_on_a_clean_field() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (seeds, _, rep) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        let truth = f64::from(0.002 * ADU_SCALE);
        println!(
            "sigma_f {:.3} (truth {truth:.3}, ratio {:.4}); sigma_f_measured {:.3} (ratio {:.4}); gain {:.4}",
            rep.sigma_f,
            rep.sigma_f / truth,
            rep.sigma_f_measured,
            rep.sigma_f_measured / truth,
            rep.noise_gain
        );
        assert_eq!(seeds.len(), 150);
        assert!((rep.sigma_f / truth - 1.0).abs() < 0.04, "{rep:?}");
        assert_eq!(rep.structure_dropped, 0);
        assert!(rep.extended.iter().all(|&e| !e), "{rep:?}");
    }

    /// Task 10b A2 fixture: 300 pixel-integrated Moffat-4 stars of FWHM 2.4 on a 20 × 15 grid (48 px apart, sub-pixel
    /// phases varied); every third one (100) so bright that the plane's ceiling (1.0) clips its core, the other 200 at
    /// amplitudes 0.04–0.34 (peak SNR 20–170 at σ 0.002). The matched filter's 100 brightest candidates are exactly the
    /// clipped stars. Returns the plane, its size, the stars `(x, y, amplitude)` and which of them are clipped.
    fn clipped_brightest_field() -> (Vec<f32>, usize, usize, Vec<(f64, f64, f64)>, Vec<bool>) {
        use crate::test_support::{integrated_field, IntegratedProfile};
        let (w, h) = (1000usize, 760usize);
        let mut stars = Vec::new();
        let mut clipped = Vec::new();
        for k in 0..300usize {
            let (i, j) = (k % 20, k / 20);
            let x = 30.0 + i as f64 * 48.0 + 0.13 * (k % 7) as f64;
            let y = 30.0 + j as f64 * 48.0 + 0.17 * (k % 5) as f64;
            let bright = k % 3 == 0;
            let amp = if bright {
                4.0 + 0.02 * k as f64
            } else {
                0.04 + 0.001 * k as f64
            };
            stars.push((x, y, amp));
            clipped.push(bright);
        }
        let profile = IntegratedProfile::Moffat {
            fwhm: 2.4,
            beta: 4.0,
        };
        let mut data = integrated_field(w, h, &stars, profile, 5, 0.05);
        add_noise(&mut data, 0.002, 21);
        for v in data.iter_mut() {
            *v = v.min(1.0);
        }
        (data, w, h, stars, clipped)
    }

    /// Task 10b A2 fixture: 200 pixel-integrated Moffat-4 stars of FWHM 3 (amplitudes 0.05–0.25, peak SNR 25–125 at
    /// σ 0.002) and 60 compact sky bumps — round Gaussians of FWHM 10 and amplitude 0.6, far below the 128-px mesh's
    /// scale — on a 20 × 13 grid, a bump at three places in every 13. The 3-px filter ranks every bump above every
    /// star, so the matched filter's 100 brightest candidates are the 60 bumps and the 40 brightest stars. Returns the
    /// plane, its size and the sources `(x, y, amplitude)` with which of them are bumps.
    fn bumpy_sky_field() -> (Vec<f32>, usize, usize, Vec<(f64, f64, f64)>, Vec<bool>) {
        use crate::test_support::{integrated_field, IntegratedProfile};
        let (w, h) = (1000usize, 660usize);
        let (mut stars, mut bumps, mut sources, mut is_bump) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for k in 0..260usize {
            let (i, j) = (k % 20, k / 20);
            let x = 30.0 + i as f64 * 48.0 + 0.11 * (k % 7) as f64;
            let y = 30.0 + j as f64 * 48.0 + 0.19 * (k % 5) as f64;
            let bump = matches!(k % 13, 3 | 7 | 11);
            if bump {
                bumps.push((x, y, 0.6));
                sources.push((x, y, 0.6));
            } else {
                let amp = 0.05 + 0.001 * (k % 200) as f64;
                stars.push((x, y, amp));
                sources.push((x, y, amp));
            }
            is_bump.push(bump);
        }
        let mut data = integrated_field(
            w,
            h,
            &stars,
            IntegratedProfile::Moffat {
                fwhm: 3.0,
                beta: 4.0,
            },
            5,
            0.05,
        );
        let sky = integrated_field(
            w,
            h,
            &bumps,
            IntegratedProfile::Gaussian { fwhm: 10.0 },
            1,
            0.0,
        );
        for (v, s) in data.iter_mut().zip(&sky) {
            *v += s;
        }
        add_noise(&mut data, 0.002, 23);
        (data, w, h, sources, is_bump)
    }

    /// The stars' own width: the median of the half-maximum reads ([`own_halfmax_width`], PA-17's estimator) of the
    /// sources that are not `excluded`, at their true positions — what the field width should read once the clipped
    /// cores or the sky bumps are out of the way.
    fn stars_own_width(
        data: &[f32],
        w: usize,
        h: usize,
        sources: &[(f64, f64, f64)],
        excluded: &[bool],
    ) -> f64 {
        let mut reads: Vec<f64> = sources
            .iter()
            .zip(excluded)
            .filter(|(_, &e)| !e)
            .filter_map(|(&(x, y, _), _)| own_halfmax_width(data, w, h, x, y, None))
            .collect();
        assert!(reads.len() >= 150, "{} reads", reads.len());
        crate::star_measure::robust::median_f64(&mut reads)
    }

    /// Task 10b A2 truth test (a) (ruling A5, F25 revisited): when the brightest hundred candidates are clipped stars,
    /// the field width still reads the unclipped stars' width (within PA-17's 10 %). Through Task 10 the field width
    /// was the median over the 100 brightest candidates — here every one of them a clipped core, read at half the clip.
    #[test]
    fn field_width_reads_the_stars_width_when_the_brightest_hundred_are_saturated() {
        let (data, w, h, stars, clipped) = clipped_brightest_field();
        let scaled = working(&data);
        let own = stars_own_width(&scaled, w, h, &stars, &clipped);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (_, field, _) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        println!("field {field:?}; the unclipped stars' own width {own:.3} (true FWHM 2.4)");
        assert!(
            (field.fwhm_px / own - 1.0).abs() < 0.10,
            "{field:?} vs {own}"
        );
    }

    /// Task 10b A2 truth test (b): compact sky bumps among the brightest candidates do not set the field width — it
    /// reads the stars' width (within 10 %). Through Task 10 the 60 bumps were the majority of the 100 read.
    #[test]
    fn field_width_reads_the_stars_width_on_structured_sky() {
        let (data, w, h, sources, is_bump) = bumpy_sky_field();
        let scaled = working(&data);
        let own = stars_own_width(&scaled, w, h, &sources, &is_bump);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (_, field, _) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        println!("field {field:?}; the stars' own width {own:.3} (true FWHM 3)");
        assert!(
            (field.fwhm_px / own - 1.0).abs() < 0.10,
            "{field:?} vs {own}"
        );
    }

    /// Task 10b A2 truth test (c) (I-4): `field_width_of` GIVEN seeds follows the same rule as the matched filter's own
    /// width — on both fixtures, with the clipped stars and the bumps leading the given order (by peak), it reads the
    /// stars' width within 10 %, and the detector's own width within 5 % (the same rule on nearly the same sources).
    #[test]
    fn field_width_of_given_seeds_follows_the_same_rule() {
        for (data, w, h, sources, excluded) in [clipped_brightest_field(), bumpy_sky_field()] {
            let scaled = working(&data);
            let own = stars_own_width(&scaled, w, h, &sources, &excluded);
            let given: Vec<Seed> = sources
                .iter()
                .map(|&(x, y, amp)| Seed {
                    x,
                    y,
                    peak: amp,
                    flux: amp,
                })
                .collect();
            let fw = seeds::field_width_of(&scaled, w, h, &given, None);
            let (noise, _) = domain::noise_model(&scaled, w, h, None);
            let (_, detected, _) = seeds::detect(
                &scaled,
                w,
                h,
                &noise,
                DEFAULT_THRESHOLD,
                24576,
                None,
                None,
                PixelNoise::White,
            );
            println!("given {fw:?}; detected {detected:?}; the stars' own width {own:.3}");
            assert!((fw.fwhm_px / own - 1.0).abs() < 0.10, "{fw:?} vs {own}");
            assert!(
                (fw.fwhm_px / detected.fwhm_px - 1.0).abs() < 0.05,
                "{fw:?} vs {detected:?}"
            );
        }
    }

    /// Task 10b A2b truth test 1 (ruling A2-2; written in full before the code, RED at 1bc29ef8, where the knot bounds
    /// sit at the field width itself): the field's star-width envelope ignores single-pixel events.
    /// [`crate::test_support::width_spread_field`] (600 Moffat β 2.5 stars, FWHM N(3.0, 0.24) px, peak SNR 20–2000)
    /// with and without 250 single-pixel events at SNR 10–18. A lone hot pixel never reaches the field width's reads
    /// (the detector's hot-pixel rule asks ≥ 2 of 4 cross-neighbours at 15 % of its peak), so each event carries two
    /// opposite cross-neighbours at 30 % of its height: the ≈ 1.3 px read of a real frame's single-pixel events.
    /// σ_true = the spread (standard deviation) of the noiseless reads. Bars: |F_env(with events) − F_env(without)| ≤
    /// 0.02·F, and F_env/F within ±0.05 of 1 + 3.09·σ_true/F.
    #[test]
    fn field_width_envelope_ignores_single_pixel_events() {
        use crate::test_support::width_spread_field;
        let (w, h) = (2048usize, 2048usize);
        let (noiseless, stars, sigma) = width_spread_field();
        let truth: Vec<f64> = stars
            .iter()
            .filter_map(|s| own_halfmax_read(&noiseless, w, h, s.x, s.y, None))
            .map(|r| r.width)
            .collect();
        assert_eq!(truth.len(), stars.len(), "every noiseless star reads");
        let mean = truth.iter().sum::<f64>() / truth.len() as f64;
        let var = truth.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (truth.len() - 1) as f64;
        let sigma_true = var.sqrt();
        let mut plain = noiseless.clone();
        add_noise(&mut plain, sigma as f32, 102);
        let mut events = plain.clone();
        let mut rng = crate::geometry::ransac::SplitMix64(103);
        let mut at: Vec<(usize, usize)> = Vec::new();
        while at.len() < 250 {
            let x = 40 + (rng.next_f64() * (w - 80) as f64) as usize;
            let y = 40 + (rng.next_f64() * (h - 80) as f64) as usize;
            let far = |(ex, ey): (f64, f64)| (ex - x as f64).hypot(ey - y as f64) >= 25.0;
            if stars.iter().all(|s| far((s.x, s.y)))
                && at.iter().all(|&(ex, ey)| far((ex as f64, ey as f64)))
            {
                let v = (sigma * (10.0 + 8.0 * rng.next_f64())) as f32;
                events[y * w + x] += v;
                events[y * w + x - 1] += 0.3 * v;
                events[y * w + x + 1] += 0.3 * v;
                at.push((x, y));
            }
        }
        let run = |data: &[f32]| {
            let scaled = working(data);
            let (noise, _) = domain::noise_model(&scaled, w, h, None);
            let (_, fw, report) = seeds::detect(
                &scaled,
                w,
                h,
                &noise,
                DEFAULT_THRESHOLD,
                24576,
                None,
                None,
                PixelNoise::White,
            );
            (fw, report.envelope)
        };
        let ((f0, e0), (f1, e1)) = (run(&plain), run(&events));
        // Premise: the events reach the field width's readable reads, each narrower than the envelope's floor.
        let floor = f0.fwhm_px * (1.0 - KNOT_Z * read_error(WIDTH_START_MIN_PEAK_SNR));
        let scaled = working(&events);
        let event_reads: Vec<f64> = at
            .iter()
            .filter_map(|&(x, y)| own_halfmax_read(&scaled, w, h, x as f64, y as f64, None))
            .filter(|r| r.snr >= WIDTH_START_MIN_PEAK_SNR)
            .map(|r| r.width)
            .collect();
        assert!(
            f1.from_stars >= f0.from_stars + 125 && event_reads.iter().all(|&r| r < floor),
            "premise: readable reads {} → {} with the events; event reads {:?} vs floor {floor}",
            f0.from_stars,
            f1.from_stars,
            &event_reads[..event_reads.len().min(10)]
        );
        let f = f0.fwhm_px;
        assert!(
            (e1.fwhm_px - e0.fwhm_px).abs() <= 0.02 * f,
            "F_env with events {e1:?} vs without {e0:?} (F {f}, {f1:?})"
        );
        for (fw, e) in [(f0, e0), (f1, e1)] {
            let want = 1.0 + KNOT_Z * sigma_true / fw.fwhm_px;
            assert!(
                (e.fwhm_px / fw.fwhm_px - want).abs() <= 0.05,
                "F_env/F {} vs 1 + 3.09·σ_true/F {want} (σ_true {sigma_true}, {e:?}, {fw:?})",
                e.fwhm_px / fw.fwhm_px
            );
        }
    }

    /// A pixel-scale nebula texture: bilinear lattice noise at spacings 2, 4, … 64 px, each octave's amplitude
    /// ∝ its spacing (a k⁻³ sky, structure function ∝ |L|), `amp` the 64-px octave's amplitude.
    fn nebula_texture(w: usize, h: usize, amp: f32, p: f32, seed: u64) -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        for (o, s) in [2usize, 4, 8, 16, 32, 64].into_iter().enumerate() {
            let (lw, lh) = (w / s + 2, h / s + 2);
            let mut lat = vec![0.0f32; lw * lh];
            add_noise(&mut lat, 1.0, seed + o as u64);
            let a = amp * (s as f32 / 64.0).powf(p);
            for y in 0..h {
                for x in 0..w {
                    let (fx, fy) = ((x % s) as f32 / s as f32, (y % s) as f32 / s as f32);
                    let (ix, iy) = (x / s, y / s);
                    let v = |dx: usize, dy: usize| lat[(iy + dy) * lw + ix + dx];
                    let top = v(0, 0) * (1.0 - fx) + v(1, 0) * fx;
                    let bot = v(0, 1) * (1.0 - fx) + v(1, 1) * fx;
                    out[y * w + x] += a * (top * (1.0 - fy) + bot * fy);
                }
            }
        }
        out
    }

    /// C-7 fixture: white noise plus a pixel-scale nebula texture (the read's gain on it is 1.57 at HEAD), 30 stars at
    /// the filtered response `z`·σ_f(gain 1). Returns (plane in working units, star positions, σ_f at gain 1).
    fn textured_white_noise_field(z: f64) -> (Vec<f32>, usize, usize, Vec<(f64, f64, f64)>) {
        let (w, h, sigma) = (768usize, 512usize, 0.002f32);
        let mut plane = nebula_texture(w, h, 0.008, 0.5, 21);
        for v in plane.iter_mut() {
            *v += 0.05;
        }
        add_noise(&mut plane, sigma, 5);
        let scaled0 = working(&plane);
        let n = seeds::detection_noise(&scaled0, w, h, None, 3.0, PixelNoise::Mixed);
        let sigma_f1 = n.median() / n.gain;
        let at: Vec<(f64, f64, f64)> = (0..30)
            .map(|k| {
                (
                    60.0 + 110.0 * (k % 6) as f64 + 7.0 * (k / 6) as f64,
                    60.0 + 85.0 * (k / 6) as f64 + 3.0 * (k % 6) as f64,
                    1.0,
                )
            })
            .collect();
        let unit = gaussian_field(w, h, &at[..1], 1.8, 0.0);
        let grid = Grid {
            sum: &unit,
            count: None,
            w,
            h,
            step: 1,
        };
        let peak = MatchedFilter {
            fwhm_px: 1.8 * 2.3548,
        }
        .apply(&grid)
        .f
        .iter()
        .cloned()
        .fold(0.0f32, f32::max) as f64;
        let amp = z * sigma_f1 / peak / ADU_SCALE as f64;
        let stars = gaussian_field(
            w,
            h,
            &at.iter().map(|&(x, y, _)| (x, y, amp)).collect::<Vec<_>>(),
            1.8,
            0.0,
        );
        let data: Vec<f32> = plane.iter().zip(&stars).map(|(a, b)| a + b).collect();
        (working(&data), w, h, at)
    }

    fn stars_found(found: &[Seed], at: &[(f64, f64, f64)]) -> usize {
        at.iter()
            .filter(|&&(x, y, _)| found.iter().any(|s| (s.x - x).hypot(s.y - y) < 2.0))
            .count()
    }

    /// Ruling C-7 (a): on a plane whose pixels were never mixed the noise gain is 1 by derivation, whatever texture the
    /// sky has; the stars at 7·σ_f(gain 1) are found (12 of 30: the knot tests drop the rest as structure on this heavy
    /// texture; 1 of 30 at HEAD, where the read returned 1.57 and the stars fell to z = 4.5).
    #[test]
    fn an_unmixed_plane_has_gain_one_and_finds_a_star_at_seven_sigma() {
        let (scaled, w, h, at) = textured_white_noise_field(7.0);
        let n = seeds::detection_noise(&scaled, w, h, None, 3.0, PixelNoise::White);
        assert_eq!(n.gain, 1.0);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (found, _, rep) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        assert_eq!(rep.noise_gain, 1.0);
        let hit = stars_found(&found, &at);
        assert!(
            hit >= 10,
            "found {hit} of 30 ({} seeds, {} dropped)",
            found.len(),
            rep.structure_dropped
        );
    }

    /// Ruling C-7 (b): the same texture on a plane flagged mixed keeps the read (the premise: it reads above 1.3),
    /// and the star at 7·σ_f(gain 1) is lost to it as before.
    #[test]
    fn a_mixed_plane_keeps_the_read() {
        let (scaled, w, h, at) = textured_white_noise_field(7.0);
        let n = seeds::detection_noise(&scaled, w, h, None, 3.0, PixelNoise::Mixed);
        assert!(
            n.gain.is_finite() && n.gain >= 1.3,
            "premise: gain {}",
            n.gain
        );
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (found, _, rep) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::Mixed,
        );
        assert!(
            rep.noise_gain >= 1.3 && rep.noise_gain.is_finite(),
            "{}",
            rep.noise_gain
        );
        assert!(
            stars_found(&found, &at) <= 3,
            "found {}",
            stars_found(&found, &at)
        );
    }

    /// A white-noise plane upsampled 2× bilinearly (the product's resamplers mix pixels the same way): flat sky, so
    /// every seed on it is false. 192² source → 384².
    fn upsampled_white_noise() -> (Vec<f32>, usize, usize, Vec<f32>, usize, usize) {
        let (sw, sh) = (192usize, 192usize);
        let mut src = vec![0.1f32; sw * sh];
        add_noise(&mut src, 0.002, 41);
        let (w, h) = (sw * 2, sh * 2);
        let at = |x: usize, y: usize| src[y.min(sh - 1) * sw + x.min(sw - 1)];
        let mut up = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / 2.0, y as f32 / 2.0);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                up[y * w + x] = at(x0, y0) * (1.0 - tx) * (1.0 - ty)
                    + at(x0 + 1, y0) * tx * (1.0 - ty)
                    + at(x0, y0 + 1) * (1.0 - tx) * ty
                    + at(x0 + 1, y0 + 1) * tx * ty;
            }
        }
        (working(&src), sw, sh, working(&up), w, h)
    }

    fn false_seeds(plane: &[f32], w: usize, h: usize, pn: PixelNoise) -> (usize, f64) {
        let (noise, _) = domain::noise_model(plane, w, h, None);
        let (found, _, rep) = seeds::detect(
            plane,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            pn,
        );
        (found.len(), rep.noise_gain)
    }

    /// Ruling C-10 (a): a resampled white-noise plane has correlated noise. With the caller saying `Unknown` the gain
    /// is read (above 1.4) and the false-seed count stays at the white plane's level; stated `White` the read is
    /// skipped (the contract: the caller's word) and the flood appears.
    #[test]
    fn a_resampled_plane_stated_unknown_reads_the_gain_and_stated_white_floods() {
        let (src, sw, sh, up, w, h) = upsampled_white_noise();
        let (white_plane_seeds, g0) = false_seeds(&src, sw, sh, PixelNoise::White);
        assert_eq!(g0, 1.0);
        let (read_seeds, g) = false_seeds(&up, w, h, PixelNoise::Unknown);
        assert!(g >= 1.4 && g.is_finite(), "read gain {g}");
        let (mixed_seeds, gm) = false_seeds(&up, w, h, PixelNoise::Mixed);
        assert_eq!((mixed_seeds, gm.to_bits()), (read_seeds, g.to_bits()));
        let (flood, gw) = false_seeds(&up, w, h, PixelNoise::White);
        assert_eq!(gw, 1.0);
        eprintln!("a1c premise: white src {white_plane_seeds}, unknown {read_seeds}, white-stated {flood}");
        assert!(
            read_seeds <= white_plane_seeds + 2,
            "{read_seeds} vs {white_plane_seeds}"
        );
        assert!(flood >= read_seeds + 20, "flood {flood} vs {read_seeds}");
    }

    /// Ruling C-10 (b): the default states nothing, and nothing is the read.
    #[test]
    fn the_default_pixel_noise_is_unknown_and_reads_the_gain() {
        assert_eq!(PixelNoise::default(), PixelNoise::Unknown);
        assert_eq!(
            crate::star_measure::MeasureOptions::default().pixel_noise,
            PixelNoise::Unknown
        );
        let (_, _, _, up, w, h) = upsampled_white_noise();
        let m = crate::star_measure::measure_plane(
            &up.iter().map(|v| v / ADU_SCALE).collect::<Vec<_>>(),
            w,
            h,
            &crate::star_measure::MeasureOptions {
                seeds: crate::star_measure::SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                ..Default::default()
            },
            None,
        );
        let g = m.detection.as_ref().expect("detection report").noise_gain;
        assert!(g >= 1.4 && g.is_finite(), "default reads the gain: {g}");
    }

    /// Ruling C-10 (c): `White` together with a debayered colour plane is a contradiction: the gain is read.
    #[test]
    fn white_stated_on_a_colour_plane_reads_the_gain() {
        assert_eq!(PixelNoise::White.with_colour(true), PixelNoise::Mixed);
        assert_eq!(PixelNoise::Unknown.with_colour(true), PixelNoise::Mixed);
        assert_eq!(PixelNoise::White.with_colour(false), PixelNoise::White);
        let (_, _, _, up, w, h) = upsampled_white_noise();
        let m = crate::star_measure::measure_plane(
            &up.iter().map(|v| v / ADU_SCALE).collect::<Vec<_>>(),
            w,
            h,
            &crate::star_measure::MeasureOptions {
                seeds: crate::star_measure::SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                colour: true,
                pixel_noise: PixelNoise::White,
                ..Default::default()
            },
            None,
        );
        let g = m.detection.as_ref().expect("detection report").noise_gain;
        assert!(g >= 1.4 && g.is_finite(), "contradiction reads: {g}");
    }

    /// Task 10c pin (R10c-10): the knot exemption's `near_clip` is the fixed 0.9·range band, read before any verdict;
    /// the verdict's near-clip clause reads the plane's derived floor, which is never below that band. With the floor
    /// AT the band the two agree on a clipped star, a star at 0.95·range, one at 0.85·range and a faint star; with a
    /// floor above it (the exact clip, 1.0) class B is a subset of the exemption's set.
    #[test]
    fn the_verdict_near_clip_equals_the_seed_near_clip() {
        use crate::star_measure::fit::{FitParams, FitScratch};
        use crate::star_measure::photometry::saturation_verdict;
        use astroimage::analysis::fitting::PixelSample;
        let (w, h) = (21usize, 21usize);
        let bg = 0.04;
        let plane = SaturationPlane::new(bg, 0.002, 1.0, 1.0, false);
        let at_band = plane.with_clip_floor(Some(bg + 0.9 * plane.range));
        let at_clip = plane.with_clip_floor(Some(1.0));
        for top in [1.0f64, 0.95, 0.85, 0.3] {
            let mut data = vec![bg as f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let r2 = ((x as f64 - 10.0).powi(2) + (y as f64 - 10.0).powi(2)) / 2.0;
                    data[y * w + x] = (bg + (top - bg) * (1.0 + r2).powf(-2.5)) as f32;
                }
            }
            let seed = Seed {
                x: 10.0,
                y: 10.0,
                peak: top,
                flux: top,
            };
            let region: Vec<PixelSample> = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| PixelSample {
                    x: x as f64 - 10.0,
                    y: y as f64 - 10.0,
                    value: data[y * w + x] as f64,
                })
                .collect();
            let verdict = |p: &SaturationPlane| {
                saturation_verdict(
                    &region,
                    0.0,
                    0.0,
                    bg,
                    1.8,
                    f64::INFINITY,
                    p,
                    &FitParams::default(),
                    &mut FitScratch::default(),
                )
                .expect("a verdict")
            };
            let v = verdict(&at_band);
            let seeds_says = near_clip(&data, None, w, h, &seed, Some(&plane));
            assert_eq!(v.by_near_clip, seeds_says && !v.plateau, "top {top}: {v:?}");
            let v = verdict(&at_clip);
            assert!(
                !v.by_near_clip || seeds_says,
                "top {top}: class B outside the exemption"
            );
            assert_eq!(seeds_says, top - bg >= 0.9 * (1.0 - bg), "top {top}");
        }
    }
}
