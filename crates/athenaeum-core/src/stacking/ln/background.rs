//! Background model on the `scale/8` mesh (spec §5.2, math §4.2): the
//! large-scale background level of one plane (the LN reference, or a
//! target frame), sampled on the same node grid [`super::grid::LnGrid`]
//! evaluates over — node `(i, j)` at pixel `(i·stride, j·stride)`, `stride
//! = (scale / 8).max(2)`, `(gw, gh)` from [`super::grid::LnGrid::grid_dims`]
//! so this mesh and the B-spline evaluator's mesh can never disagree.
//! Task 5 builds `B = B_ref − s·B_tgt` from a reference grid (`deviation_sigma
//! = 3.0`) and a target grid (`TARGET_DEVIATION_SIGMA = 3.2`, slightly looser
//! since a target frame carries its own noise/registration residual on top
//! of the reference's).
//!
//! **The threshold semantics below are our reading of math §4.2, not a
//! literal transcription — say so, per the task brief.** In particular:
//! `low_clip`/`high_clip_rel` are read as a *global* pre-filter meant to
//! drop genuinely anomalous pixels (saturation, cosmic rays, extended
//! sources) relative to the plane's own bulk distribution — not a literal
//! `value > high_clip_rel · raw_max`. A literal reading breaks on a plane
//! whose own smooth background trend spans a sizeable fraction of its own
//! maximum (no bright stars to set a sane scale) — exactly the case
//! `vertical_gradient_is_tracked_per_cell` below exercises. So the "plane
//! maximum" the high clip is relative to is a *robust* estimate of the
//! bulk background's upper edge (median + a wide, hot-pixel-style
//! deviation margin), not the single brightest raw pixel — a real
//! saturated star or an extended source is still enormously past that
//! margin, but a smooth low-contrast gradient with no bimodal population
//! is not.

use super::grid::LnGrid;
use crate::integration::stats::{mad_about, median_in_place};

/// σ-consistency factor for the median absolute deviation (same constant
/// as [`crate::integration::stats::MAD_TO_SIGMA`], repeated locally so this
/// module reads standalone next to the math it implements).
const MAD_TO_SIGMA: f32 = 1.4826;
/// Deviation multiple (in MAD-sigma) a pixel must exceed its local window
/// median by before the hot-pixel pass replaces it, and the multiple the
/// same window statistic uses to build a robust "bulk maximum" for the
/// global high clip (see the module doc).
const HOT_PIXEL_SIGMA: f32 = 5.0;
/// A cell above this many finite pixels is subsampled 2×2 before the
/// iterative clip (§ algorithm step 3) — cheap without materially changing
/// the robust statistics on a scale/8 mesh's largest cells (128×128 at the
/// default scale).
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
/// nodes [`background_grid`] could not measure directly (see below) —
/// reported per frame as `ln_cells_rejected`; those nodes are still filled
/// in `cells` (from their valid neighbours) unless the WHOLE plane had no
/// valid cell at all, in which case `invalid_cells == gw * gh` and the
/// caller should refuse rather than trust the (all-zero) fallback grid.
#[derive(Debug, Clone)]
pub struct BackgroundGrid {
    pub gw: usize,
    pub gh: usize,
    pub cells: Vec<f32>,
    pub invalid_cells: usize,
}

/// One plane → its large-scale background on the stride grid (node `(i,
/// j)` = the robust level of the stride×stride cell centred on `(i·stride,
/// j·stride)`, clipped to the plane). See the module doc for the clipping
/// thresholds' meaning and algorithm steps.
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

    let cleaned = clean_plane(plane, width, height, p);

    let mut cells = vec![0f32; cell_count];
    let mut invalid = vec![false; cell_count];
    let half = stride / 2;
    let half_hi = stride - half; // symmetric for even stride; keeps total width == stride for odd stride too
    for j in 0..gh {
        let node_y = j * stride;
        let y0 = node_y.saturating_sub(half);
        let y1 = (node_y + half_hi).min(height);
        for i in 0..gw {
            let node_x = i * stride;
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

/// Steps 1–2 of the algorithm: a hot-pixel-corrected, clipped copy of
/// `plane`. Values excluded by the clip are `NaN` in the returned copy;
/// everything else is either the original value or its hot-pixel
/// replacement.
fn clean_plane(plane: &[f32], width: usize, height: usize, p: &BackgroundParams) -> Vec<f32> {
    let high_thresh = robust_high_threshold(plane, p);

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

/// A robust "bulk maximum" for the plane, used both as the hot-pixel
/// candidate cutoff and the global high clip (module doc): the plane's
/// median plus a wide MAD-based margin, scaled by `high_clip_rel`. A
/// genuinely anomalous population (a saturated star, an extended source)
/// sits far past this regardless of its size; a smooth, unimodal
/// background trend — with no separate bright population to set a scale —
/// does not, so it survives untouched.
fn robust_high_threshold(plane: &[f32], p: &BackgroundParams) -> f32 {
    let finite: Vec<f32> = plane.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f32::INFINITY;
    }
    let mut sample = finite.clone();
    let med = median_in_place(&mut sample);
    let mad = mad_about(&finite, med);
    let bulk_max = med + HOT_PIXEL_SIGMA * MAD_TO_SIGMA * mad;
    // A perfectly flat plane (mad == 0, e.g. an all-zero test fixture) must
    // still let its own value through — fall back to the plane's raw max
    // scaled by `high_clip_rel` in that degenerate case.
    if mad > 0.0 {
        p.high_clip_rel * bulk_max.max(med)
    } else {
        let raw_max = finite.iter().copied().fold(f32::MIN, f32::max);
        p.high_clip_rel * raw_max
    }
}

/// The cell's finite pixels, stride-2 subsampled (both axes) when there
/// are more than [`CELL_SUBSAMPLE_THRESHOLD`] of them.
fn gather_cell(
    cleaned: &[f32],
    width: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
) -> Vec<f32> {
    let finite_count = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| cleaned[y * width + x]))
        .filter(|v| v.is_finite())
        .count();

    let step = if finite_count > CELL_SUBSAMPLE_THRESHOLD {
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

fn median_of(values: &[f32]) -> f32 {
    let mut v = values.to_vec();
    median_in_place(&mut v)
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
        let plane: Vec<f32> = (0..w * h).map(|i| 0.05 + (i / w) as f32 * 1e-4).collect(); // +0.0256 top to bottom
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
                plane[y * w + x] = 0.8; // a galaxy core covering cell (0,0)
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
}
