//! Background model on the `scale/8` mesh (spec §5.2, math §4.2): the
//! large-scale background level of one plane (the LN reference, or a
//! target frame), sampled on the same node grid [`super::grid::LnGrid`]
//! evaluates over — node `(i, j)` at pixel `(i·stride, j·stride)`, `stride
//! = (scale / 8).max(2)`, `(gw, gh)` from [`super::grid::LnGrid::grid_dims`]
//! so this mesh and the B-spline evaluator's mesh can never disagree. The
//! trailing node on each axis (`node_count` overshoots the plane by design,
//! so the spline reaches the last pixel) is clamped into the plane before
//! its window is built — its cell is the last stride×stride (half-open,
//! clipped) region of the plane, not an empty, past-the-edge one. Task 5
//! builds `B = B_ref − s·B_tgt` from a reference grid (`deviation_sigma =
//! 3.0`) and a target grid (`TARGET_DEVIATION_SIGMA = 3.2`, slightly looser
//! since a target frame carries its own noise/registration residual on top
//! of the reference's).
//!
//! `low_clip`/`high_clip_rel` are the clipping thresholds of math §4.2 in
//! the pipeline's normalized units (float32 in [0, 1], ADU / 65535): a
//! pixel below `low_clip` or above `high_clip_rel` of FULL SCALE (1.0) is
//! excluded before any per-cell statistic sees it. Controller ruling
//! (2026-09-10): "0.85 relative to the maximum" is read as 0.85 of the
//! representable maximum — near-saturation for a 16-bit camera — not of
//! the plane's own maximum. The plane-maximum reading clips an entire
//! flat, star-less plane (its maximum IS the background), and a robust
//! median+MAD ceiling was tried and rejected because on a field with
//! bright nebulosity it clips the nebula out of the background model; the
//! full-scale reading touches neither. The per-cell deviation clipping
//! below is what removes stars from the model.
//!
//! `invalid_cells` is a per-frame count for the caller (Task 5) to log as
//! `ln_cells_rejected` — this module does not log it itself.

use super::grid::LnGrid;
use crate::integration::stats::{mad_about, median_in_place, median_of, MAD_TO_SIGMA};

/// Deviation multiple (in MAD-sigma) a pixel must exceed its local window
/// median by before the hot-pixel pass replaces it.
const HOT_PIXEL_SIGMA: f32 = 5.0;
/// A cell whose window holds more than this many pixels is subsampled 2×2
/// before the iterative clip (§ algorithm step 3) — cheap without
/// materially changing the robust statistics on a scale/8 mesh's largest
/// cells (128×128 at the default scale).
const CELL_SUBSAMPLE_THRESHOLD: usize = 4096;
/// Iterative per-cell median±sigma clipping stops after this many rounds
/// even if the kept set has not yet stabilized.
const MAX_SIGMA_CLIP_ROUNDS: usize = 5;

/// Tunables for [`background_grid`] (math §4.2). `scale` is the LN scale
/// (1024 in M2 → stride 128); `deviation_sigma` is 3.0 for the reference
/// plane, [`TARGET_DEVIATION_SIGMA`] (3.2) for a target plane.
#[derive(Debug, Clone, Copy)]
pub struct BackgroundParams {
    pub scale: u32,
    pub hot_radius: usize,
    pub low_clip: f32,
    pub high_clip_rel: f32,
    pub deviation_sigma: f32,
    pub rejection_limit: f32,
}

/// Reference-plane defaults (math §4.2).
pub const DEFAULT_PARAMS: BackgroundParams = BackgroundParams {
    scale: 1024,
    hot_radius: 2,
    low_clip: 4.5e-5,
    high_clip_rel: 0.85,
    deviation_sigma: 3.0,
    rejection_limit: 0.3,
};

/// `deviation_sigma` Task 5 uses for a *target* frame's background model —
/// slightly looser than the reference's 3.0 (`DEFAULT_PARAMS`) since a
/// target carries its own noise/registration residual on top of the
/// reference's.
pub const TARGET_DEVIATION_SIGMA: f32 = 3.2;

/// One plane's background, sampled on the `scale/8` node mesh. `cells` is
/// `gw × gh`, row-major (`cells[j * gw + i]` = node `(i, j)`'s level) — the
/// same layout [`LnGrid::a`]/[`LnGrid::b`] use, so a `BackgroundGrid` can
/// feed the B-spline evaluator directly. `invalid_cells` is the number of
/// nodes [`background_grid`] could not measure directly (see below) — the
/// per-frame caller (Task 5) logs it as `ln_cells_rejected`, not this
/// module. Those nodes are still filled in `cells` (from their valid
/// neighbours) unless the WHOLE plane had no valid cell at all, in which
/// case `invalid_cells == gw * gh` and the caller should refuse rather
/// than trust the (all-zero) fallback grid.
#[derive(Debug, Clone)]
pub struct BackgroundGrid {
    pub gw: usize,
    pub gh: usize,
    pub cells: Vec<f32>,
    pub invalid_cells: usize,
}

/// One plane → its large-scale background on the stride grid (node `(i,
/// j)` = the robust level of the stride×stride cell centred on `(i·stride,
/// j·stride)`, clipped to the plane — the trailing node on either axis is
/// clamped to the plane's last pixel first, see the module doc). See the
/// module doc for the clipping thresholds' meaning and algorithm steps.
pub fn background_grid(
    plane: &[f32],
    width: usize,
    height: usize,
    p: &BackgroundParams,
) -> BackgroundGrid {
    let stride = (p.scale / 8).max(2) as usize;
    let (gw, gh) = LnGrid::grid_dims(width, height, stride);
    let cell_count = gw * gh;

    if width == 0 || height == 0 || plane.len() < width.saturating_mul(height) || cell_count == 0 {
        return BackgroundGrid {
            gw,
            gh,
            cells: vec![0.0; cell_count],
            invalid_cells: cell_count,
        };
    }

    // Every pass below reads only `plane[..width * height]` — a longer
    // input's tail is ignored everywhere, not just here.
    let plane = &plane[..width * height];
    let cleaned = clean_plane(plane, width, height, p);

    let mut cells = vec![0f32; cell_count];
    let mut invalid = vec![false; cell_count];
    let half = stride / 2;
    let half_hi = stride - half; // symmetric for even stride; keeps total width == stride for odd stride too
    for j in 0..gh {
        // The trailing node overshoots the plane by design (`node_count`
        // guarantees the mesh reaches the last pixel for the spline) —
        // clamp it into the plane before windowing, so its cell is the
        // last stride×stride (clipped) region rather than landing at or
        // past `height`/`width` and gathering nothing.
        let node_y = (j * stride).min(height - 1);
        let y0 = node_y.saturating_sub(half);
        let y1 = (node_y + half_hi).min(height);
        for i in 0..gw {
            let node_x = (i * stride).min(width - 1);
            let x0 = node_x.saturating_sub(half);
            let x1 = (node_x + half_hi).min(width);

            let samples = gather_cell(&cleaned, width, x0, x1, y0, y1);
            let (level, ok) = robust_cell_level(samples, p);
            let idx = j * gw + i;
            cells[idx] = level;
            invalid[idx] = !ok;
        }
    }

    let invalid_cells = invalid.iter().filter(|&&v| v).count();
    fill_invalid_cells(&mut cells, &invalid, gw, gh);

    BackgroundGrid {
        gw,
        gh,
        cells,
        invalid_cells,
    }
}

/// The high clip of math §4.2 as ruled in the module doc: `high_clip_rel`
/// of full scale (1.0 in the pipeline's normalized units) — an absolute
/// near-saturation threshold, independent of the plane's content (M7,
/// final fix wave: no longer takes a `plane` parameter — it never read one;
/// that was a leftover from the plane-maximum reading the controller's
/// ruling replaced with this full-scale constant). Used both as the
/// hot-pixel candidate cutoff and the global high clip below.
fn high_clip_threshold(p: &BackgroundParams) -> f32 {
    p.high_clip_rel
}

/// Steps 1–2 of the algorithm: a hot-pixel-corrected, clipped copy of
/// `plane`. Values excluded by the clip are `NaN` in the returned copy;
/// everything else is either the original value or its hot-pixel
/// replacement. The only transient full-plane allocation beyond the
/// returned copy is the small per-candidate hot-pixel window (at most
/// `(2·hot_radius+1)²` samples), built only for pixels past the high clip.
fn clean_plane(plane: &[f32], width: usize, height: usize, p: &BackgroundParams) -> Vec<f32> {
    let high_thresh = high_clip_threshold(p);

    let mut cleaned = plane.to_vec();
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let v = plane[idx];
            if !(v.is_finite() && v > high_thresh) {
                continue;
            }
            let x0 = x.saturating_sub(p.hot_radius);
            let x1 = (x + p.hot_radius).min(width - 1);
            let y0 = y.saturating_sub(p.hot_radius);
            let y1 = (y + p.hot_radius).min(height - 1);
            let mut window: Vec<f32> = Vec::with_capacity((x1 - x0 + 1) * (y1 - y0 + 1));
            for wy in y0..=y1 {
                for wx in x0..=x1 {
                    let wv = plane[wy * width + wx];
                    if wv.is_finite() {
                        window.push(wv);
                    }
                }
            }
            if window.is_empty() {
                continue;
            }
            let med = median_in_place(&mut window);
            let mad = mad_about(&window, med);
            let thresh = med + HOT_PIXEL_SIGMA * MAD_TO_SIGMA * mad;
            if v > thresh {
                cleaned[idx] = med;
            }
        }
    }

    for v in cleaned.iter_mut() {
        if !v.is_finite() || *v < p.low_clip || *v > high_thresh {
            *v = f32::NAN;
        }
    }

    cleaned
}

/// The window's finite pixels, stride-2 subsampled (both axes) when the
/// window itself (before filtering to finite) holds more than
/// [`CELL_SUBSAMPLE_THRESHOLD`] pixels.
fn gather_cell(
    cleaned: &[f32],
    width: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
) -> Vec<f32> {
    let total = x1.saturating_sub(x0) * y1.saturating_sub(y0);
    let step = if total > CELL_SUBSAMPLE_THRESHOLD {
        2
    } else {
        1
    };

    let mut samples = Vec::new();
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let v = cleaned[y * width + x];
            if v.is_finite() {
                samples.push(v);
            }
            x += step;
        }
        y += step;
    }
    samples
}

/// Algorithm step 3: iterative median±`deviation_sigma`·MAD clipping over
/// a cell's already-clipped finite samples. Returns `(level, true)` when
/// the kept set holds after clipping stabilizes (or hits the round cap)
/// with no more than `rejection_limit` of the finite samples thrown out;
/// `(0.0, false)` — invalid — when there is nothing finite to start from,
/// or the iterative clip rejects too much of what there was.
fn robust_cell_level(samples: Vec<f32>, p: &BackgroundParams) -> (f32, bool) {
    if samples.is_empty() {
        return (0.0, false);
    }
    let total = samples.len();
    let mut kept = samples;
    for _ in 0..MAX_SIGMA_CLIP_ROUNDS {
        if kept.is_empty() {
            break;
        }
        let med = median_of(&kept);
        let mad = mad_about(&kept, med);
        if mad <= 0.0 {
            // Every kept sample already agrees with the median at MAD's
            // resolution — a zero deviation bound would otherwise reject
            // everything but exact ties, wrongly invalidating a quantized
            // cell with one dominant value and a modest minority of noise.
            break;
        }
        let bound = p.deviation_sigma * MAD_TO_SIGMA * mad;
        let next: Vec<f32> = kept
            .iter()
            .copied()
            .filter(|&v| (v - med).abs() <= bound)
            .collect();
        if next.len() == kept.len() {
            kept = next;
            break;
        }
        kept = next;
    }

    let rejected = total.saturating_sub(kept.len());
    let fraction = rejected as f32 / total as f32;
    if kept.is_empty() || fraction > p.rejection_limit {
        return (0.0, false);
    }
    (median_of(&kept), true)
}

/// Algorithm step 4: repeatedly fill invalid cells from the mean of their
/// valid 8-neighbours (a cell filled in one round counts as valid for the
/// next), until none is left or a full pass makes no further progress —
/// the latter only when no cell anywhere was ever valid, in which case
/// every cell stays at its zero-initialized level and the caller sees
/// `invalid_cells == gw * gh`.
fn fill_invalid_cells(cells: &mut [f32], invalid: &[bool], gw: usize, gh: usize) {
    let mut still_invalid = invalid.to_vec();
    loop {
        let pending: Vec<usize> = still_invalid
            .iter()
            .enumerate()
            .filter(|&(_, &inv)| inv)
            .map(|(idx, _)| idx)
            .collect();
        if pending.is_empty() {
            break;
        }

        let mut updates: Vec<(usize, f32)> = Vec::new();
        for idx in pending {
            let j = idx / gw;
            let i = idx % gw;
            let mut sum = 0f64;
            let mut count = 0usize;
            for dj in -1isize..=1 {
                for di in -1isize..=1 {
                    if di == 0 && dj == 0 {
                        continue;
                    }
                    let nj = j as isize + dj;
                    let ni = i as isize + di;
                    if nj < 0 || ni < 0 || nj as usize >= gh || ni as usize >= gw {
                        continue;
                    }
                    let nidx = nj as usize * gw + ni as usize;
                    if !still_invalid[nidx] {
                        sum += cells[nidx] as f64;
                        count += 1;
                    }
                }
            }
            if count > 0 {
                updates.push((idx, (sum / count as f64) as f32));
            }
        }
        if updates.is_empty() {
            // Nothing anywhere had a valid neighbour to fill from — every
            // still-pending cell stays invalid (only reachable when no
            // cell in the whole grid was ever valid).
            break;
        }
        for (idx, v) in updates {
            cells[idx] = v;
            still_invalid[idx] = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_plane_with_stars_recovers_the_flat_level() {
        let (w, h) = (512, 384);
        let mut plane = vec![0.10f32; w * h];
        for k in 0..200 {
            let (x, y) = ((k * 37) % w, (k * 91) % h);
            plane[y * w + x] = 0.9; // bright points
        }
        let g = background_grid(
            &plane,
            w,
            h,
            &BackgroundParams {
                scale: 256,
                hot_radius: 2,
                low_clip: 4.5e-5,
                high_clip_rel: 0.85,
                deviation_sigma: 3.0,
                rejection_limit: 0.3,
            },
        );
        assert_eq!((g.gw, g.gh), (17, 13)); // stride 32
        assert!(
            g.cells.iter().all(|c| (c - 0.10).abs() < 1e-4),
            "{:?}",
            &g.cells[..8]
        );
        assert_eq!(g.invalid_cells, 0);
    }

    #[test]
    fn vertical_gradient_is_tracked_per_cell() {
        let (w, h) = (256, 256);
        let mut plane: Vec<f32> = (0..w * h).map(|i| 0.05 + (i / w) as f32 * 1e-4).collect(); // +0.0256 top to bottom
        plane[128 * w + 128] = 0.9; // one star: the high clip (0.85 of full scale) removes it, the gradient survives
        let g = background_grid(
            &plane,
            w,
            h,
            &BackgroundParams {
                scale: 256,
                ..DEFAULT_PARAMS
            },
        );
        let top = g.cells[0];
        let bottom = g.cells[(g.gh - 1) * g.gw];
        assert!(
            bottom - top > 0.020 && bottom - top < 0.030,
            "{top} {bottom}"
        );
    }

    #[test]
    fn a_cell_that_is_mostly_star_is_invalid_and_filled_from_neighbours() {
        let (w, h) = (256, 256);
        let mut plane = vec![0.10f32; w * h];
        for y in 0..40 {
            for x in 0..40 {
                plane[y * w + x] = 0.95; // a saturated star core covering cell (0,0): above the 0.85 full-scale high clip
            }
        }
        let g = background_grid(
            &plane,
            w,
            h,
            &BackgroundParams {
                scale: 256,
                ..DEFAULT_PARAMS
            },
        );
        assert_eq!(g.invalid_cells, 1);
        assert!((g.cells[0] - 0.10).abs() < 1e-3);
    }

    /// The trailing node's raw pixel address (`(gw-1)·stride`/`(gh-1)·stride`)
    /// overshoots the plane whenever `(extent - 1) mod stride` falls in
    /// `1..=half-1` — half of all extents. At scale 256 (stride 32,
    /// half 16), a width/height of 1026/514 hits it on both axes
    /// (`1025 % 32 == 1`, `513 % 32 == 1`), matching the scale-1024/stride-128
    /// case (`4097 % 128 == 1`, `2049 % 128 == 1`) the review flagged —
    /// cheaper to run here. Before the clamp fix, the last row/column of
    /// cells came out invalid (an inverted, empty window) and got
    /// neighbour-filled instead of measured.
    ///
    /// One bright pixel is planted so the hot-pixel pass has something to
    /// correct on this plane too (same as
    /// `flat_plane_with_stars_recovers_the_flat_level`); with the high clip
    /// at 0.85 of full scale a flat plane at 0.10 is never clipped, so the
    /// pixel is not needed to "set the scale" — it exercises the pass.
    #[test]
    fn trailing_node_overshoot_gets_a_real_window() {
        let (w, h) = (1026usize, 514usize);
        let mut plane = vec![0.10f32; w * h];
        plane[10 * w + 10] = 0.9; // a hot pixel, corrected back to the flat level by the pass
        let g = background_grid(
            &plane,
            w,
            h,
            &BackgroundParams {
                scale: 256,
                ..DEFAULT_PARAMS
            },
        );
        assert_eq!(g.invalid_cells, 0, "cells: {:?}", g.cells);
        assert!(g.cells.iter().all(|c| (c - 0.10).abs() < 1e-4));
    }

    /// M8 (final fix wave): `robust_cell_level`'s `fraction > p.rejection_limit`
    /// branch — distinct from the empty-samples path every other invalid-cell
    /// test in this module reaches. A tight 6-sample cluster (nonzero MAD, so
    /// the deviation bound is meaningful, not the `mad <= 0.0` early break) plus
    /// a 4-sample block far enough outside that bound: the iterative clip
    /// removes exactly the 4 (40%, above the 30% `rejection_limit`) and
    /// stabilizes there — `kept` is the 6-sample majority, never empty.
    #[test]
    fn robust_cell_level_over_rejected_by_the_iterative_clip_is_invalid() {
        let mut samples = vec![0.098f32, 0.099, 0.100, 0.101, 0.102, 0.103];
        samples.extend([0.50f32; 4]);
        let (level, ok) = robust_cell_level(samples, &DEFAULT_PARAMS);
        assert!(
            !ok,
            "40% of the cell is a systematic outlier block, above the 30% \
             rejection limit: must be flagged invalid, got level {level}"
        );
    }

    /// M8 (final fix wave): `fill_invalid_cells`'s terminal branch — "no cell
    /// anywhere was ever valid" (`invalid_cells == gw * gh`). Every pixel
    /// below `low_clip` clips to NaN in `clean_plane`, so every cell's
    /// `gather_cell` comes back empty and every `robust_cell_level` call is
    /// the `samples.is_empty()` case — this is the group-level LN fallback
    /// trigger 2 in `run.rs` (C1's "reference background has no measurable
    /// cell" path).
    #[test]
    fn a_plane_entirely_below_the_low_clip_is_fully_invalid() {
        let (w, h) = (128, 128);
        let plane = vec![0.0f32; w * h]; // < low_clip (4.5e-5) everywhere
        let g = background_grid(
            &plane,
            w,
            h,
            &BackgroundParams {
                scale: 256,
                ..DEFAULT_PARAMS
            },
        );
        assert_eq!(g.invalid_cells, g.gw * g.gh, "{:?}", g.cells);
        assert!(g.cells.iter().all(|&v| v == 0.0), "{:?}", g.cells);
    }
}
