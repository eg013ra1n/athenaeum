//! The seeds (spec 2026-10-07 §2.1 `seeds`, §3 F-1; Plan A Task 6a): star detection by a two-pass Gaussian matched
//! filter at the field width, its threshold in units of the filtered plane's own measured noise, the selection by the
//! filter's integrated signal, and the field width itself: the azimuthal half-maximum width of the brightest
//! candidates (PA-17), which [`field_width_of`] also gives seeds another detector handed in (I-4).
//!
//! Task 6a changed no fit (ruling F16). From Task 6b the field width also sizes every star's width-start box in the fit
//! (radius `⌈2·F_field⌉`, PA-12): the matched filter's own for its seeds, [`field_width_of`] for seeds handed in.
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

use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;
use tracing::{debug, warn};

use super::domain::{NoiseModel, NoiseSource, BACKGROUND_MODEL_CELL_PX};
use super::fit::Seed;
use crate::integration::stats::{for_each_stratified, mad_about, median_in_place, MAD_TO_SIGMA};

/// `2√(2 ln 2)`: a Gaussian's FWHM per σ.
const GAUSSIAN_FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949_3;

/// The matched filter's threshold in units of the filtered plane's measured noise σ_f (ruling R-OSP-2).
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
/// How many of the first pass's brightest candidates (and of given seeds) the field width reads (PA-17).
const FIELD_WIDTH_STARS: usize = 100;
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
const HALFMAX_PEAK_SEARCH_PX: i64 = 2;
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

/// The field width: the median half-maximum width (FWHM, px) of the brightest candidates or given seeds, clamped to
/// [1.5, 24] px, and how many stars it was read on (`from_stars = 0`: none read; `fwhm_px` is then the first pass's
/// 3 px, or the caller's own value — `FitOutcome.field_width` for a given `FitStarsOptions.field_width`).
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

/// What one pass found: its seeds (unsorted, unmerged) and the counts the debug line reports.
struct PassOutcome {
    seeds: Vec<Seed>,
    sigma_f: f64,
    spikes: usize,
}

/// One pass of `filter` over `grid`: the local maxima of `f` above `threshold·σ_f`, each judged by the hot-pixel rule
/// on its full-resolution pixel (the cell itself, or a super-pixel's brightest present pixel), placed by its centroid
/// on the full plane in a `(2·⌈F⌉+1)²` box, with `flux` the filter's least-squares flux there.
fn run_pass(filter: MatchedFilter, grid: &Grid, det: &Detection) -> Option<PassOutcome> {
    let out = filter.apply(grid);
    let (gw, gh) = (grid.w, grid.h);
    let mut sample: Vec<f32> = Vec::with_capacity(gw * gh / 16 + 1);
    for_each_stratified(gw, gh, |i| {
        if out.f[i].is_finite() {
            sample.push(out.f[i]);
        }
    });
    if sample.len() < 8 {
        sample = out.f.iter().copied().filter(|v| v.is_finite()).collect();
    }
    let m = median_in_place(&mut sample);
    let sigma_f = (MAD_TO_SIGMA * mad_about(&sample, m)) as f64;
    if !(sigma_f.is_finite() && sigma_f > 0.0) {
        warn!(
            width = det.w,
            height = det.h,
            fwhm_px = filter.fwhm_px,
            "matched filter: the filtered plane has no measurable noise; no seeds"
        );
        return None;
    }
    let level = (det.threshold * sigma_f) as f32;
    let maxima: Vec<usize> = (0..gh)
        .into_par_iter()
        .flat_map_iter(|y| {
            let f = &out.f;
            (0..gw).filter_map(move |x| {
                let i = y * gw + x;
                let c = f[i];
                if !(c > level) {
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
        seeds.push(Seed { x, y, peak, flux });
    }
    Some(PassOutcome {
        seeds,
        sigma_f,
        spikes,
    })
}

/// Sorted by `flux` descending (stable), a seed whose centroid lies within [`SAME_STAR_PX`] of a brighter kept one is
/// the same star and dropped.
fn sort_and_merge(mut seeds: Vec<Seed>) -> Vec<Seed> {
    seeds.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    let mut kept = Vec::with_capacity(seeds.len());
    'next: for s in seeds {
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
        kept.push(s);
    }
    kept
}

/// The background the filter subtracts: the mesh model (cell 128 px) over the present pixels, or — should the model
/// be unavailable — the median of the present stratified sample everywhere.
fn background(plane: &[f32], w: usize, h: usize, mask: Option<&[bool]>) -> Vec<f32> {
    let masked: Vec<f32>;
    let data: &[f32] = match mask {
        Some(m) => {
            masked = plane
                .iter()
                .zip(m)
                .map(|(&v, &keep)| if keep { v } else { f32::NAN })
                .collect();
            &masked
        }
        None => plane,
    };
    let model = astroimage::analysis::background::estimate_background_mesh(
        data,
        w,
        h,
        BACKGROUND_MODEL_CELL_PX,
        false,
    )
    .background_map;
    match model {
        Some(m) if m.len() == w * h => m,
        _ => {
            let mut s = crate::integration::stats::stratified_sample(data, w, h);
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
            vec![level; w * h]
        }
    }
}

/// Two passes: a 3-px kernel finds the 100 brightest candidates, their half-maximum widths (`halfmax_width`,
/// PA-17) give the field width (median over ALL of them, saturated ones included — no saturation test runs here,
/// ruling F25 as folded; clamped to [1.5, 24] px); the second pass runs at that width. A field
/// width ≥ 6 px runs the second pass on the 2×2 super-pixel plane (kernel halved; candidate positions ×2,
/// refined on the full plane). Candidates: local maxima of the filtered plane above `threshold·σ_f` (σ_f =
/// 1.4826·MAD of the filtered plane over the stratified sample) with ≥ 2 of 4 cross-neighbours above `background
/// + min(threshold·σ_plane, 0.15·(c − background))` on the UNFILTERED plane, `c` the candidate's own pixel (the
/// hot-pixel rule with its capped level — controller ruling, Task 6a; R-OSP-2). Position: the flux-weighted
/// centroid in a (2·ceil(F)+1)² box on the background-subtracted plane; `peak` = that box's maximum − background;
/// `flux` = the filtered response (the matched filter's integrated signal — the selection key). Sorted by `flux`
/// descending, truncated to `max_stars`. A `NoiseModel` with `source == NoiseSource::None` returns no seeds and one
/// `warn!` (Review Focus 4). Public: Plan C calls it on its own 2×2 super-pixel OSC plane (I-8).
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
) -> (Vec<Seed>, FieldWidth) {
    if plane.len() != w * h || mask.is_some_and(|m| m.len() != w * h) || w < 3 || h < 3 {
        warn!(
            len = plane.len(),
            width = w,
            height = h,
            "matched filter: the plane (or its mask) does not match the geometry; no seeds"
        );
        return (Vec::new(), FieldWidth::unmeasured());
    }
    if noise.source == NoiseSource::None || !(noise.sigma.is_finite() && noise.sigma > 0.0) {
        warn!(
            width = w,
            height = h,
            noise = noise.sigma,
            "matched filter: the plane has no noise estimate, so no threshold; no seeds"
        );
        return (Vec::new(), FieldWidth::unmeasured());
    }
    if !(threshold.is_finite() && threshold > 0.0) {
        warn!(
            threshold,
            "matched filter: the threshold is not a positive number; no seeds"
        );
        return (Vec::new(), FieldWidth::unmeasured());
    }
    let run = || detect_on(plane, w, h, noise.sigma as f32, threshold, max_stars, mask);
    match pool {
        Some(p) => p.install(run),
        None => run(),
    }
}

fn detect_on(
    plane: &[f32],
    w: usize,
    h: usize,
    sigma_plane: f32,
    threshold: f64,
    max_stars: usize,
    mask: Option<&[bool]>,
) -> (Vec<Seed>, FieldWidth) {
    let present_at = |i: usize| plane[i].is_finite() && mask.map_or(true, |m| m[i]);
    let present: Option<Vec<bool>> = if (0..w * h).all(present_at) {
        None
    } else {
        Some((0..w * h).map(present_at).collect())
    };
    let bg = background(plane, w, h, mask);
    let d: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| if present_at(i) { plane[i] - bg[i] } else { 0.0 })
        .collect();
    drop(bg);
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

    // Pass 1: the 3-px kernel; its brightest candidates give the field width.
    let first_filter = MatchedFilter {
        fwhm_px: FIRST_PASS_FWHM_PX,
    };
    let Some(first) = run_pass(first_filter, &full, &det) else {
        return (Vec::new(), FieldWidth::unmeasured());
    };
    let first_seeds = sort_and_merge(first.seeds);
    let n_first = first_seeds.len();
    let field = field_width_of(
        plane,
        w,
        h,
        &first_seeds[..n_first.min(FIELD_WIDTH_STARS)],
        mask,
    );

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
        let Some(mut p) = run_pass(filter, &grid, &det) else {
            return (Vec::new(), field);
        };
        let s = sort_and_merge(std::mem::take(&mut p.seeds));
        (p, s)
    } else if field.from_stars == 0 {
        // No width was read: the field width is the first pass's own, so its candidates stand.
        (
            PassOutcome {
                seeds: Vec::new(),
                sigma_f: first.sigma_f,
                spikes: first.spikes,
            },
            first_seeds,
        )
    } else {
        let Some(mut p) = run_pass(filter, &full, &det) else {
            return (Vec::new(), field);
        };
        let s = sort_and_merge(std::mem::take(&mut p.seeds));
        (p, s)
    };
    let mut seeds = seeds;
    let found = seeds.len();
    seeds.truncate(max_stars);
    debug!(
        width = w,
        height = h,
        threshold,
        sigma = second.sigma_f,
        fwhm_px = field.fwhm_px,
        from_stars = field.from_stars,
        super_pixel,
        spikes = second.spikes,
        stars_detected = seeds.len(),
        count = found,
        "matched filter seeds detected"
    );
    (seeds, field)
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
    let background = median_in_place(&mut rim) as f64;
    let c0 = top as f64 - background;
    if !(c0 > 0.0) {
        return None;
    }
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
            return Some(2.0 * (r_prev + t * (rk - r_prev)));
        }
        r_prev = rk;
        v_prev = vk;
    }
    None
}

/// The field width of GIVEN seeds (I-4, PA-17): the median `halfmax_width` of the ≤ 100 brightest, all of them —
/// no saturation test (ruling F25 as folded) — (box 15 px; a first read ≥ 7 px is re-measured in a `3·F` box,
/// ≤ 48), clamped to [1.5, 24] px. `fit_stars_at` calls it when
/// `FitStarsOptions.field_width` is `None` (LN refits, Peak/Structure seeds, Plan C's mosaic sites); Task 6b's
/// width start reads it; this task's fit does not.
///
/// "Brightest" is by `peak` (a stable sort, so equal peaks keep the given order). A re-measure whose larger box
/// leaves the image keeps the first read. No star read → `from_stars = 0` and the first pass's 3 px.
pub fn field_width_of(
    plane: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    mask: Option<&[bool]>,
) -> FieldWidth {
    let mut order: Vec<usize> = (0..seeds.len()).collect();
    order.sort_by(|&a, &b| seeds[b].peak.total_cmp(&seeds[a].peak));
    let mut widths: Vec<f64> = order
        .iter()
        .take(FIELD_WIDTH_STARS)
        .filter_map(|&i| {
            let s = &seeds[i];
            let first = halfmax_width(plane, w, h, s.x, s.y, HALFMAX_BOX_PX, mask)?;
            if first < HALFMAX_REMEASURE_FROM_PX {
                return Some(first);
            }
            let wide = ((3.0 * first).ceil() as usize).min(HALFMAX_BOX_MAX_PX);
            Some(halfmax_width(plane, w, h, s.x, s.y, wide, mask).unwrap_or(first))
        })
        .collect();
    if widths.is_empty() {
        return FieldWidth::unmeasured();
    }
    widths.sort_by(f64::total_cmp);
    let n = widths.len();
    let median = if n % 2 == 1 {
        widths[n / 2]
    } else {
        0.5 * (widths[n / 2 - 1] + widths[n / 2])
    };
    FieldWidth {
        fwhm_px: median.clamp(FIELD_WIDTH_MIN_PX, FIELD_WIDTH_MAX_PX),
        from_stars: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::{domain, seeds, ADU_SCALE};
    use crate::test_support::fixture_field;

    fn working(data: &[f32]) -> Vec<f32> {
        data.iter().map(|v| v * ADU_SCALE).collect()
    }

    #[test]
    fn matched_filter_finds_every_star_of_the_fixture_and_no_noise_peak() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002); // 150 stars, σ 1.8 px (FWHM 4.24)
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (seeds, fw) =
            seeds::detect(&scaled, w, h, &noise, DEFAULT_THRESHOLD, 24576, None, None);
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
        let (seeds, _) = seeds::detect(
            &data,
            w,
            h,
            &noise_model,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
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
        let (seeds, _) = seeds::detect(&scaled, w, h, &noise, DEFAULT_THRESHOLD, 24576, None, None);
        assert!(
            seeds
                .iter()
                .all(|s| (s.x - 100.0).abs() > 2.0 || (s.y - 100.0).abs() > 2.0),
            "{seeds:?}"
        );
    }
}
