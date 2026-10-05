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

use std::borrow::Cow;
use std::sync::Arc;

use rayon::prelude::*;

use super::grid::LnGrid;
use crate::integration::stats::{mad_about, median_in_place, MAD_TO_SIGMA};

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

/// Per-worker scratch [`background_grid`]'s cell loop hands to
/// [`gather_cell`]/[`robust_cell_level`] via `rayon`'s `map_init` — one
/// instance per worker thread for the WHOLE plane, reused cell to cell
/// instead of each cell allocating its own `Vec`s (perf tier A Task 12,
/// audit's own accounting: ≈ 1,700 cells per plane, `gather_cell` growing a
/// `Vec` from empty for every one of them, up to [`MAX_SIGMA_CLIP_ROUNDS`]
/// rounds each allocating more). `samples` is the cell's working set —
/// filled by `gather_cell` (cleared first), filtered in place by
/// `robust_cell_level`'s `retain`; `dev` is `robust_cell_level`'s own
/// `|x - med|` scratch for the MAD computation, likewise cleared and
/// refilled every round rather than allocated. Neither ever leaks between
/// cells: `gather_cell` always clears `samples` before filling it, so a
/// cell never sees a previous cell's leftover data, and `map_init` hands
/// each rayon worker its own instance — no two cells running concurrently
/// ever share one.
#[derive(Default)]
struct CellScratch {
    samples: Vec<f32>,
    dev: Vec<f32>,
}

/// One plane → its large-scale background on the stride grid (node `(i,
/// j)` = the robust level of the stride×stride cell centred on `(i·stride,
/// j·stride)`, clipped to the plane — the trailing node on either axis is
/// clamped to the plane's last pixel first, see the module doc). See the
/// module doc for the clipping thresholds' meaning and algorithm steps.
///
/// `pool` (perf tier A Task 12) runs `clean_plane`'s two passes and this
/// function's own per-cell loop on the caller's pool when given one — see
/// each site's own doc for why every one of those loops is exact under
/// parallel order: `clean_plane`'s two passes are elementwise (each pixel's
/// output depends only on the read-only input plane and constants, never on
/// another pixel's output), and a cell's statistics are a pure function of
/// that cell's own gathered samples, read by no other cell. `None` does NOT
/// mean serial: a rayon parallel iterator called outside an explicit
/// `ThreadPool::install` still runs on rayon's own lazily-initialized
/// GLOBAL pool (sized by `available_parallelism`, invisible to the
/// caller's `image_pool`/admission budget) — the same convention
/// `psf_signal::fit_all`'s own doc states. Either way the RESULT is
/// bit-identical (the module's own pin proves it) — `pool` only changes
/// whose workers do the work, never what they compute.
///
/// **Reading the plane 4×4-BINNED was tried and MEASURED OUT** (perf tier C
/// item C5, ruling C-29, 2026-09-20 — do not re-propose it). `clip_and_bin`
/// fused `clean_plane`'s two passes into a `LN_BIN = 4` reduction
/// (clip-then-bin: the hot-pixel rescue and the `[low_clip, high_clip]`
/// clip applied per SOURCE pixel exactly as below, then the survivors of
/// each 4×4 block averaged) and had the cell loop gather bins instead of
/// pixels. It is genuinely much faster — `background_ms` 83 → 15 on a 4-wide
/// pool, 73 → 10 on a 10-wide one, per mono frame — and it cost ≈ 104 MB of
/// transient allocation per channel less. It was reverted anyway:
///
/// On the FULL 197-frame acceptance set the `B` grid it produces moves by
/// **2.4e-3 of sky at the MEDIAN node** (mono; OSC green/blue 1.2–1.6e-3),
/// p99 1.4e-2, with single nodes reaching 4× sky through the hard validity
/// gate and `ln_cells_rejected` up by a median of 1 (max 10) per frame.
/// That carries into the masters: mono master MAD **+1.88 %** and OSC blue
/// FWHM **+2.67 %**, against the tier's own 1 % bars, with medians drifting
/// to ±0.07 %. A three-frame probe had reported a median node move of 9e-4
/// — it **understated the full-group move by ≈ 2.5×**, which is the process
/// lesson as much as the numeric one.
///
/// The cause is not the validity gate and cannot be fixed by softening it:
/// the difference between a mean of 16 binned pixels and the per-pixel
/// median of a 128-px cell lives in the MEDIAN of the distribution on a
/// crowded sky (binning smears star flux *below* the per-cell rejection
/// bound instead of letting the clip remove it), not only in the tail. The
/// whole item was worth ≈ 0.4 min of a 14-minute run. Full measurement:
/// `.superpowers/sdd/2026-09-20-stacking-compute-tierC-plan/task-7-report.md`.
pub fn background_grid(
    plane: &[f32],
    width: usize,
    height: usize,
    p: &BackgroundParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
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
    let cleaned = clean_plane(plane, width, height, p, pool);

    let half = stride / 2;
    let half_hi = stride - half; // symmetric for even stride; keeps total width == stride for odd stride too

    // The cell loop, flattened to `0..cell_count` (row-major, `idx = j*gw +
    // i` — the same layout the nested `for j { for i { ... } }` loop it
    // replaces used) so it can run as one indexed parallel iterator: each
    // cell's window bounds (`node_x`/`node_y`/`x0`/`x1`/`y0`/`y1`) are pure
    // functions of `idx`/the constants above, and `gather_cell` +
    // `robust_cell_level` read only `cleaned` (read-only, shared) and the
    // cell's own `CellScratch` (never another cell's) — no cell's
    // computation can observe another's result, so the order they run in
    // cannot change any cell's `(level, ok)`. `collect()` on an
    // `IndexedParallelIterator` preserves index order regardless of which
    // worker finished which cell first, so `results[idx]` is cell `idx`'s
    // own outcome exactly as the serial loop would have produced.
    let body = || {
        (0..cell_count)
            .into_par_iter()
            .map_init(CellScratch::default, |scratch, idx| {
                let j = idx / gw;
                let i = idx % gw;
                // The trailing node overshoots the plane by design
                // (`node_count` guarantees the mesh reaches the last pixel
                // for the spline) — clamp it into the plane before
                // windowing, so its cell is the last stride×stride
                // (clipped) region rather than landing at or past
                // `height`/`width` and gathering nothing.
                let node_y = (j * stride).min(height - 1);
                let y0 = node_y.saturating_sub(half);
                let y1 = (node_y + half_hi).min(height);
                let node_x = (i * stride).min(width - 1);
                let x0 = node_x.saturating_sub(half);
                let x1 = (node_x + half_hi).min(width);

                gather_cell(&cleaned, width, x0, x1, y0, y1, &mut scratch.samples);
                robust_cell_level(&mut scratch.samples, &mut scratch.dev, p)
            })
            .collect::<Vec<(f32, bool)>>()
    };
    let results: Vec<(f32, bool)> = match pool {
        Some(pl) => pl.install(body),
        None => body(),
    };

    let mut cells = Vec::with_capacity(cell_count);
    let mut invalid = Vec::with_capacity(cell_count);
    let mut invalid_cells = 0usize;
    for (level, ok) in results {
        cells.push(level);
        let inv = !ok;
        if inv {
            invalid_cells += 1;
        }
        invalid.push(inv);
    }

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
///
/// Both passes run on `pool` (perf tier A Task 12) when given one, in ONE
/// `install` call. Pass 1 (hot-pixel correction) is exact under any row
/// split: pixel `(x, y)`'s candidacy test and its replacement window both
/// read ONLY `plane` (read-only, shared, never mutated by this function)
/// and constants (`p`, `high_thresh`) — never `cleaned`'s own OUTPUT at any
/// other position — and write only `cleaned[y*width + x]`, so no row's
/// computation can observe or be observed by another row's write; splitting
/// `cleaned` into per-row chunks (`par_chunks_mut(width)`) therefore
/// produces the identical `cleaned` a serial row-by-row pass would, for any
/// row order. Pass 2 (the low/high clip) is a pure elementwise threshold
/// against the two constants `p.low_clip`/`high_thresh` — again no pixel
/// reads another's value — so parallelizing it (`par_iter_mut`) is exact
/// for the same reason.
fn clean_plane(
    plane: &[f32],
    width: usize,
    height: usize,
    p: &BackgroundParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<f32> {
    let high_thresh = high_clip_threshold(p);

    let mut cleaned = plane.to_vec();

    let mut body = || {
        cleaned
            .par_chunks_mut(width)
            .enumerate()
            .for_each(|(y, row)| {
                let base = y * width;
                for (x, o) in row.iter_mut().enumerate() {
                    let v = plane[base + x];
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
                        *o = med;
                    }
                }
            });

        cleaned.par_iter_mut().for_each(|v| {
            if !v.is_finite() || *v < p.low_clip || *v > high_thresh {
                *v = f32::NAN;
            }
        });
    };
    match pool {
        Some(pl) => pl.install(body),
        None => body(),
    }

    cleaned
}

/// The window's finite pixels, stride-2 subsampled (both axes) when the
/// window itself (before filtering to finite) holds more than
/// [`CELL_SUBSAMPLE_THRESHOLD`] pixels — written into `out` (perf tier A
/// Task 12: cleared, then filled, in place) instead of returning a fresh
/// `Vec`. `out` is the caller's per-cell [`CellScratch::samples`] — the
/// push sequence (same row-major stride-stepped order as before) is
/// unchanged, so `out`'s content after this call is byte-identical to what
/// the old `Vec::new()` + push return used to hold, not merely
/// value-equal.
fn gather_cell(
    cleaned: &[f32],
    width: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    out: &mut Vec<f32>,
) {
    out.clear();
    let total = x1.saturating_sub(x0) * y1.saturating_sub(y0);
    let step = if total > CELL_SUBSAMPLE_THRESHOLD {
        2
    } else {
        1
    };

    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let v = cleaned[y * width + x];
            if v.is_finite() {
                out.push(v);
            }
            x += step;
        }
        y += step;
    }
}

/// Algorithm step 3: iterative median±`deviation_sigma`·MAD clipping over
/// a cell's already-clipped finite samples, in place on the caller's
/// per-worker scratch (perf tier A Task 12) instead of `median_of`/
/// `mad_about`'s own fresh-`Vec` calls and a `filter().collect()` per
/// round. `kept` arrives already filled by [`gather_cell`] — this function
/// only ever shrinks it (`retain`) or reorders it (`median_in_place`'s own
/// partial sort), never grows it, and the caller's NEXT cell starts from a
/// fresh `gather_cell` fill (which clears it first), so no cell can
/// observe a previous cell's leftover data. `dev` is scratch for the MAD
/// step's `|x - med|` values, cleared and rebuilt every round.
///
/// Exactness: `median_in_place`/[`mad_about`] compute a SELECTION (the
/// k-th order statistic of a multiset) — deterministic for a given
/// multiset regardless of the array's arrangement when the call is made,
/// because `select_nth_unstable_by` always partitions to place the correct
/// order-statistic VALUE at the pivot position, whatever permutation it
/// starts from. `median_in_place(kept)` therefore returns exactly what
/// `median_of(&kept)` used to (same multiset, same value) while also
/// reordering `kept` in place — harmless, since every later read of `kept`
/// (the `dev` fill, the `retain` predicate, the final `median_in_place`)
/// only cares about VALUES, never position. `dev`'s own median is the same
/// argument one level down: `dev.extend(kept.iter().map(|&x| (x -
/// med).abs()))` builds the identical multiset `mad_about(&kept, med)`
/// would have (same `kept` contents, same `med`), just in whatever order
/// `kept` happens to be in at that moment — irrelevant to the median it
/// computes. `retain` vs the old `filter().collect()`: both keep exactly
/// the elements passing the same predicate and drop the rest, so the two
/// always agree on the same SET of survivors. M1 fix wave correction:
/// that is as far as the equivalence goes — `retain` is a stable filter,
/// but it runs on `kept` AFTER this same round's `median_in_place(kept)`
/// has already reordered it in place (`select_nth_unstable_by` is not a
/// stable sort), so the survivors' RELATIVE ORDER here need not match
/// whatever order a fresh `filter().collect()` over an unscrambled copy
/// would have produced. That permutation difference is harmless for the
/// reason already given above: every later read of `kept` (the `dev`
/// fill, the next round's `retain` predicate, the final
/// `median_in_place`) only cares about VALUES, never position.
///
/// Returns `(level, true)` when the kept set holds after clipping
/// stabilizes (or hits the round cap) with no more than `rejection_limit`
/// of the finite samples thrown out; `(0.0, false)` — invalid — when there
/// is nothing finite to start from, or the iterative clip rejects too much
/// of what there was.
fn robust_cell_level(kept: &mut Vec<f32>, dev: &mut Vec<f32>, p: &BackgroundParams) -> (f32, bool) {
    if kept.is_empty() {
        return (0.0, false);
    }
    let total = kept.len();
    for _ in 0..MAX_SIGMA_CLIP_ROUNDS {
        if kept.is_empty() {
            break;
        }
        let med = median_in_place(kept);
        dev.clear();
        dev.extend(kept.iter().map(|&x| (x - med).abs()));
        let mad = median_in_place(dev);
        if mad <= 0.0 {
            // Every kept sample already agrees with the median at MAD's
            // resolution — a zero deviation bound would otherwise reject
            // everything but exact ties, wrongly invalidating a quantized
            // cell with one dominant value and a modest minority of noise.
            break;
        }
        let bound = p.deviation_sigma * MAD_TO_SIGMA * mad;
        let before = kept.len();
        kept.retain(|&v| (v - med).abs() <= bound);
        if kept.len() == before {
            break;
        }
    }

    let rejected = total.saturating_sub(kept.len());
    let fraction = rejected as f32 / total as f32;
    if kept.is_empty() || fraction > p.rejection_limit {
        return (0.0, false);
    }
    (median_in_place(kept), true)
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

// ===================================================================
// The background model at the scale (spec 2026-10-04 §1.2, rulings
// L-1…L-6). One procedure for the LN reference and every target, each in
// its own noise units: 32-px tiles give robust samples (steps 1–3), a
// penalized tensor cubic B-spline with knot spacing = scale is fitted to
// them with two rejection rounds (steps 4–5) and a sample-count floor
// (step 6), and each reference–target pair is refitted once on the
// samples both kept (step 7, `shared_refit`). There is no cell validity
// gate and no neighbour fill: a tile with too few present pixels is simply
// not a sample, and the membrane penalty continues the surface over it.
// ===================================================================

/// Tile edge in pixels (L-2): non-overlapping, anchored at (0, 0).
pub const TILE_PX: usize = 32;
/// A tile is a sample when at least this many of its pixels are present
/// (a quarter of a full tile).
pub const MIN_TILE_PRESENT: usize = 256;
/// In-tile clip, in MAD-sigma of the tile (L-1).
pub const TILE_CLIP_SIGMA: f32 = 3.0;
/// Sample rejection against the fitted surface, in MAD-sigma of the
/// residuals (L-1).
pub const SAMPLE_CLIP_SIGMA: f64 = 3.0;
/// Rejection rounds; the fit after the last round is final (step 5).
pub const SAMPLE_REJECTION_ROUNDS: usize = 2;
/// A knot cell (scale × scale) is supported at this many accepted samples
/// (L-4). A plane is refused only when no cell is supported.
pub const MIN_SAMPLES_PER_KNOT_CELL: usize = 8;
/// λ = this × the median non-zero diagonal of BᵀB (L-3 as amended by
/// P-20: 1e-2, not 1e-4 — at 1e-4 a coefficient the data reach only through
/// a B-spline tail is too weakly held and amplifies sample noise into gaps,
/// up to 6.8 % of sky per frame on a project member's uncovered pixels).
pub const MEMBRANE_PENALTY_REL: f64 = 1e-2;
const MAD_TO_SIGMA_F64: f64 = 1.4826;

/// The pixel cleaning both models share (unchanged from the cell model):
/// the hot-pixel pass and the `[low_clip, high_clip_rel]` clip, with the
/// 2026-09-10 ruling that 0.85 is 0.85 of full scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CleaningParams {
    pub hot_radius: usize,
    pub low_clip: f32,
    pub high_clip_rel: f32,
}

pub const CLEANING: CleaningParams = CleaningParams {
    hot_radius: 2,
    low_clip: 4.5e-5,
    high_clip_rel: 0.85,
};

/// One tile's statistic (spec §1.2 step 3). `value`/`sigma` are NaN when
/// the tile is not a sample. `max` is the largest hot-pixel-corrected
/// finite pixel taken BEFORE the clip (NaN with none) — the plane maximum
/// the star selection compares with (L-14).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileStat {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub present: u32,
    pub value: f32,
    pub sigma: f32,
    pub max: f32,
}

impl TileStat {
    pub fn is_sample(&self) -> bool {
        self.present as usize >= MIN_TILE_PRESENT
    }

    /// The centre of the tile's pixel extent (not of its present pixels).
    pub fn centre(&self) -> (f64, f64) {
        (
            (self.x0 + self.x1 - 1) as f64 * 0.5,
            (self.y0 + self.y1 - 1) as f64 * 0.5,
        )
    }
}

/// Every tile of one plane, row-major (`tiles[v * tw + u]`).
#[derive(Debug, Clone, PartialEq)]
pub struct TileTable {
    pub width: usize,
    pub height: usize,
    pub tw: usize,
    pub th: usize,
    pub tiles: Vec<TileStat>,
}

#[derive(Default)]
struct TileScratch {
    vals: Vec<f32>,
    dev: Vec<f32>,
    window: Vec<f32>,
}

impl TileTable {
    /// Steps 1–3 over the whole plane. Each tile cleans its own pixels,
    /// reading a `hot_radius` apron from `plane` for the hot-pixel window,
    /// so the result equals the old full-plane pass pixel for pixel
    /// (`tile_statistics_match_the_full_plane_cleaning`) with no full-plane
    /// copy. A tile is a pure function of `plane`, so the parallel collect
    /// (index order) is bit-identical to a serial loop.
    pub fn measure(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> TileTable {
        Self::measure_where(plane, width, height, p, pool, None)
    }

    /// As [`Self::measure`], with every pixel outside `mask` absent —
    /// including inside the hot-pixel window. Equal to `measure` on a copy
    /// of `plane` with NaN outside `mask`.
    pub fn measure_masked(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
        mask: &[bool],
    ) -> TileTable {
        Self::measure_where(plane, width, height, p, pool, Some(mask))
    }

    fn measure_where(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
        mask: Option<&[bool]>,
    ) -> TileTable {
        if width == 0
            || height == 0
            || plane.len() < width.saturating_mul(height)
            || mask.is_some_and(|m| m.len() < width * height)
        {
            return TileTable {
                width,
                height,
                tw: 0,
                th: 0,
                tiles: Vec::new(),
            };
        }
        let plane = &plane[..width * height];
        let (tw, th) = (width.div_ceil(TILE_PX), height.div_ceil(TILE_PX));
        let body = || {
            (0..tw * th)
                .into_par_iter()
                .map_init(TileScratch::default, |s, idx| {
                    let (x0, y0) = ((idx % tw) * TILE_PX, (idx / tw) * TILE_PX);
                    let (x1, y1) = ((x0 + TILE_PX).min(width), (y0 + TILE_PX).min(height));
                    measure_tile(plane, width, height, (x0, y0, x1, y1), p, mask, s)
                })
                .collect::<Vec<TileStat>>()
        };
        let tiles = match pool {
            Some(pl) => pl.install(body),
            None => body(),
        };
        TileTable {
            width,
            height,
            tw,
            th,
            tiles,
        }
    }

    /// A project member's reference tiles (L-5, P-3): a tile whose extent
    /// AND hot-pixel apron lie inside `mask` keeps this table's statistic
    /// bit for bit; a tile with no pixel inside is absent; any other tile is
    /// recomputed on its masked pixels. Identical to
    /// [`Self::measure_masked`] (`restricted_tiles_equal_measuring_the_masked_plane`).
    pub fn restricted(
        &self,
        plane: &[f32],
        mask: &[bool],
        p: &CleaningParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> TileTable {
        let (w, h) = (self.width, self.height);
        if self.tiles.is_empty() || plane.len() < w * h || mask.len() < w * h {
            return TileTable {
                width: w,
                height: h,
                tw: 0,
                th: 0,
                tiles: Vec::new(),
            };
        }
        let r = p.hot_radius;
        let body = || {
            self.tiles
                .par_iter()
                .map_init(TileScratch::default, |s, t| {
                    let (x0, y0, x1, y1) =
                        (t.x0 as usize, t.y0 as usize, t.x1 as usize, t.y1 as usize);
                    let (ax0, ay0) = (x0.saturating_sub(r), y0.saturating_sub(r));
                    let (ax1, ay1) = ((x1 + r).min(w), (y1 + r).min(h));
                    let (mut any_in_tile, mut all_in_apron) = (false, true);
                    for y in ay0..ay1 {
                        for x in ax0..ax1 {
                            let m = mask[y * w + x];
                            all_in_apron &= m;
                            if m && (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                                any_in_tile = true;
                            }
                        }
                    }
                    if all_in_apron {
                        *t
                    } else if !any_in_tile {
                        TileStat {
                            present: 0,
                            value: f32::NAN,
                            sigma: f32::NAN,
                            max: f32::NAN,
                            ..*t
                        }
                    } else {
                        measure_tile(plane, w, h, (x0, y0, x1, y1), p, Some(mask), s)
                    }
                })
                .collect::<Vec<TileStat>>()
        };
        let tiles = match pool {
            Some(pl) => pl.install(body),
            None => body(),
        };
        TileTable {
            width: w,
            height: h,
            tw: self.tw,
            th: self.th,
            tiles,
        }
    }

    /// The plane maximum for relative saturation (L-14): the largest
    /// hot-pixel-corrected finite pixel, NaN when the plane has none.
    pub fn plane_max(&self) -> f32 {
        self.tiles
            .iter()
            .map(|t| t.max)
            .filter(|m| m.is_finite())
            .fold(
                f32::NAN,
                |acc, m| if acc.is_nan() || m > acc { m } else { acc },
            )
    }
}

/// Steps 2–3 for one tile. `mask` (when given) makes every pixel outside it
/// absent, inside the hot-pixel window too.
fn measure_tile(
    plane: &[f32],
    width: usize,
    height: usize,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
    p: &CleaningParams,
    mask: Option<&[bool]>,
    s: &mut TileScratch,
) -> TileStat {
    let at = |k: usize| -> f32 {
        match mask {
            Some(m) if !m[k] => f32::NAN,
            _ => plane[k],
        }
    };
    let high = p.high_clip_rel;
    s.vals.clear();
    let mut max = f32::NEG_INFINITY;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut v = at(y * width + x);
            if !v.is_finite() {
                continue;
            }
            if v > high {
                s.window.clear();
                for wy in y.saturating_sub(p.hot_radius)..=(y + p.hot_radius).min(height - 1) {
                    for wx in x.saturating_sub(p.hot_radius)..=(x + p.hot_radius).min(width - 1) {
                        let wv = at(wy * width + wx);
                        if wv.is_finite() {
                            s.window.push(wv);
                        }
                    }
                }
                let med = median_in_place(&mut s.window);
                let mad = mad_about(&s.window, med);
                if v > med + HOT_PIXEL_SIGMA * MAD_TO_SIGMA * mad {
                    v = med;
                }
            }
            max = max.max(v);
            if v >= p.low_clip && v <= high {
                s.vals.push(v);
            }
        }
    }
    let max = if max == f32::NEG_INFINITY {
        f32::NAN
    } else {
        max
    };
    let present = s.vals.len() as u32;
    let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
    if s.vals.len() < MIN_TILE_PRESENT {
        return TileStat {
            x0,
            y0,
            x1,
            y1,
            present,
            value: f32::NAN,
            sigma: f32::NAN,
            max,
        };
    }
    for _ in 0..MAX_SIGMA_CLIP_ROUNDS {
        let med = median_in_place(&mut s.vals);
        s.dev.clear();
        s.dev.extend(s.vals.iter().map(|&v| (v - med).abs()));
        let mad = median_in_place(&mut s.dev);
        if mad <= 0.0 {
            break;
        }
        let bound = TILE_CLIP_SIGMA * MAD_TO_SIGMA * mad;
        let before = s.vals.len();
        s.vals.retain(|&v| (v - med).abs() <= bound);
        if s.vals.len() == before {
            break;
        }
    }
    let value = median_in_place(&mut s.vals);
    s.dev.clear();
    s.dev.extend(s.vals.iter().map(|&v| (v - value).abs()));
    let sigma = MAD_TO_SIGMA * median_in_place(&mut s.dev);
    TileStat {
        x0,
        y0,
        x1,
        y1,
        present,
        value,
        sigma,
        max,
    }
}

/// A smooth background surface: a tensor-product uniform cubic B-spline on
/// knots `t_k = (k − 3)·knot` along each axis (L-3), `nx × ny` coefficients
/// row-major (`coeffs[j * nx + i]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub knot: f64,
    pub nx: usize,
    pub ny: usize,
    pub coeffs: Vec<f64>,
}

/// The four uniform cubic B-spline weights at local position `t ∈ [0, 1]`.
fn cubic_weights(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        (1.0 - t).powi(3) / 6.0,
        (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0,
        (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0,
        t3 / 6.0,
    ]
}

/// The span (first of the four active basis functions) and local `t` of a
/// coordinate. The last pixel of an extent that is an exact multiple of the
/// knot spacing lands at `t = 1` of the last span.
fn span(v: f64, knot: f64, n: usize) -> (usize, f64) {
    let u = v / knot;
    let m = (u.floor().max(0.0) as usize).min(n - 4);
    (m, u - m as f64)
}

impl Surface {
    /// `(nx, ny)` for a plane at a knot spacing: `ceil((extent − 1)/knot) + 3`.
    fn dims(width: usize, height: usize, knot: usize) -> (usize, usize) {
        (
            (width - 1).div_ceil(knot) + 3,
            (height - 1).div_ceil(knot) + 3,
        )
    }

    pub fn eval(&self, x: f64, y: f64) -> f64 {
        let x = x.clamp(0.0, (self.width - 1) as f64);
        let y = y.clamp(0.0, (self.height - 1) as f64);
        let (mx, tx) = span(x, self.knot, self.nx);
        let (my, ty) = span(y, self.knot, self.ny);
        let (wx, wy) = (cubic_weights(tx), cubic_weights(ty));
        let mut s = 0.0;
        for (b, wyb) in wy.iter().enumerate() {
            let row = (my + b) * self.nx + mx;
            for (a, wxa) in wx.iter().enumerate() {
                s += wyb * wxa * self.coeffs[row + a];
            }
        }
        s
    }

    /// The surface at every LN mesh node (spec §1.2 step 8): node `(i, j)`
    /// at pixel `(i·stride, j·stride)`, clamped into the plane, row-major —
    /// the layout `BackgroundGrid::cells` had and `LnGrid::b` uses.
    pub fn nodes(&self, stride: usize) -> Vec<f32> {
        let (gw, gh) = LnGrid::grid_dims(self.width, self.height, stride);
        let mut out = Vec::with_capacity(gw * gh);
        for j in 0..gh {
            let y = (j * stride).min(self.height - 1) as f64;
            for i in 0..gw {
                let x = (i * stride).min(self.width - 1) as f64;
                out.push(self.eval(x, y) as f32);
            }
        }
        out
    }
}

/// Why a plane has no background model (spec §1.2 step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundRefusal {
    /// No tile has [`MIN_TILE_PRESENT`] present pixels.
    NoSamples,
    /// No knot cell holds [`MIN_SAMPLES_PER_KNOT_CELL`] accepted samples.
    NoSupportedCell,
    /// The normal matrix was not positive definite (never seen; kept so a
    /// numeric failure is an error, not a NaN surface).
    Singular,
}

impl std::fmt::Display for BackgroundRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BackgroundRefusal::NoSamples => "no background sample",
            BackgroundRefusal::NoSupportedCell => "no supported knot cell",
            BackgroundRefusal::Singular => "the background fit is singular",
        })
    }
}

impl std::error::Error for BackgroundRefusal {}

/// One plane's own-threshold model (spec §1.2 steps 4–6). `accepted[k]` is
/// tile `k`'s membership in the final fit; `samples` counts them,
/// `samples_rejected` the samples the rounds left out, `cells` the knot
/// cells and `cells_unsupported` those under the floor.
#[derive(Debug, Clone)]
pub struct BackgroundModel {
    pub table: TileTable,
    pub scale: u32,
    pub surface: Surface,
    pub accepted: Vec<bool>,
    pub samples: usize,
    pub samples_rejected: usize,
    pub cells_unsupported: usize,
    pub cells: usize,
}

impl BackgroundModel {
    pub fn fit(table: TileTable, scale: u32) -> Result<BackgroundModel, BackgroundRefusal> {
        let (width, height) = (table.width, table.height);
        if width < 2 || height < 2 || table.tiles.is_empty() {
            return Err(BackgroundRefusal::NoSamples);
        }
        let knot = scale.max(1) as usize;
        let (nx, ny) = Surface::dims(width, height, knot);
        let values: Vec<f32> = table.tiles.iter().map(|t| t.value).collect();
        let all: Vec<usize> = (0..table.tiles.len())
            .filter(|&k| table.tiles[k].is_sample())
            .collect();
        if all.is_empty() {
            return Err(BackgroundRefusal::NoSamples);
        }
        let solve = |ids: &[usize]| -> Result<Vec<f64>, BackgroundRefusal> {
            fit_coeffs(&table, knot as f64, nx, ny, ids, &[&values])
                .map(|mut c| c.remove(0))
                .ok_or(BackgroundRefusal::Singular)
        };
        let mut accepted_ids = all.clone();
        let mut surface = Surface {
            width,
            height,
            knot: knot as f64,
            nx,
            ny,
            coeffs: solve(&accepted_ids)?,
        };
        let mut resid = vec![0f64; table.tiles.len()];
        for _ in 0..SAMPLE_REJECTION_ROUNDS {
            for &k in &all {
                let (x, y) = table.tiles[k].centre();
                resid[k] = values[k] as f64 - surface.eval(x, y);
            }
            let sigma_e = MAD_TO_SIGMA_F64 * mad_f64(accepted_ids.iter().map(|&k| resid[k]));
            // P-21: tile values are f32, so a residual MAD below a few f32
            // ulps of the level is rounding, not noise — it is σ_e ≤ 0 here.
            let level = median_f64_sorted(
                &mut accepted_ids
                    .iter()
                    .map(|&k| values[k] as f64)
                    .collect::<Vec<_>>(),
            )
            .abs();
            let next: Vec<usize> = if sigma_e > 1e-6 * level {
                all.iter()
                    .copied()
                    .filter(|&k| resid[k].abs() <= SAMPLE_CLIP_SIGMA * sigma_e)
                    .collect()
            } else {
                all.clone()
            };
            // P-9: a round that would keep nothing rejects nothing.
            accepted_ids = if next.is_empty() { all.clone() } else { next };
            surface.coeffs = solve(&accepted_ids)?;
        }
        let (supported, cells) = supported_cells(&table, knot as f64, nx, ny, &accepted_ids);
        if supported == 0 {
            return Err(BackgroundRefusal::NoSupportedCell);
        }
        let mut accepted = vec![false; table.tiles.len()];
        for &k in &accepted_ids {
            accepted[k] = true;
        }
        Ok(BackgroundModel {
            samples: accepted_ids.len(),
            samples_rejected: all.len() - accepted_ids.len(),
            cells_unsupported: cells - supported,
            cells,
            accepted,
            surface,
            scale,
            table,
        })
    }
}

/// `(supported, total)` knot cells for the samples `ids` (L-4).
fn supported_cells(
    table: &TileTable,
    knot: f64,
    nx: usize,
    ny: usize,
    ids: &[usize],
) -> (usize, usize) {
    let (cx, cy) = (nx - 3, ny - 3);
    let mut counts = vec![0usize; cx * cy];
    for &k in ids {
        let (x, y) = table.tiles[k].centre();
        let (mx, _) = span(x, knot, nx);
        let (my, _) = span(y, knot, ny);
        counts[my * cx + mx] += 1;
    }
    (
        counts
            .iter()
            .filter(|&&c| c >= MIN_SAMPLES_PER_KNOT_CELL)
            .count(),
        cx * cy,
    )
}

/// `median |e − median(e)|` in f64 (even counts: the mean of the middle two).
fn mad_f64(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return 0.0;
    }
    let med = median_f64_sorted(&mut v);
    let mut dev: Vec<f64> = v.iter().map(|e| (e - med).abs()).collect();
    median_f64_sorted(&mut dev)
}

fn median_f64_sorted(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Penalized least squares (L-3) on the samples `ids` (ascending tile
/// order), one solution per right-hand side `rhs[r][tile]`. The normal
/// matrix and right-hand sides accumulate in f64 in `ids` order and the
/// solve is a serial dense Cholesky, so the fit is deterministic. `None`
/// only when the matrix is not positive definite.
fn fit_coeffs(
    table: &TileTable,
    knot: f64,
    nx: usize,
    ny: usize,
    ids: &[usize],
    rhs: &[&[f32]],
) -> Option<Vec<Vec<f64>>> {
    let n = nx * ny;
    let mut ata = vec![0f64; n * n];
    let mut atb = vec![vec![0f64; n]; rhs.len()];
    let (mut idx, mut w) = ([0usize; 16], [0f64; 16]);
    for &k in ids {
        let (x, y) = table.tiles[k].centre();
        let (mx, tx) = span(x, knot, nx);
        let (my, ty) = span(y, knot, ny);
        let (wx, wy) = (cubic_weights(tx), cubic_weights(ty));
        for b in 0..4 {
            for a in 0..4 {
                idx[b * 4 + a] = (my + b) * nx + mx + a;
                w[b * 4 + a] = wy[b] * wx[a];
            }
        }
        for p in 0..16 {
            let row = idx[p] * n;
            for q in 0..16 {
                ata[row + idx[q]] += w[p] * w[q];
            }
            for (r, vals) in rhs.iter().enumerate() {
                atb[r][idx[p]] += w[p] * vals[k] as f64;
            }
        }
    }
    let mut diag: Vec<f64> = (0..n)
        .map(|k| ata[k * n + k])
        .filter(|&d| d > 0.0)
        .collect();
    if diag.is_empty() {
        return None;
    }
    let lambda = MEMBRANE_PENALTY_REL * median_f64_sorted(&mut diag);
    let mut couple = |a: usize, b: usize| {
        ata[a * n + a] += lambda;
        ata[b * n + b] += lambda;
        ata[a * n + b] -= lambda;
        ata[b * n + a] -= lambda;
    };
    for j in 0..ny {
        for i in 0..nx {
            let a = j * nx + i;
            if i + 1 < nx {
                couple(a, a + 1);
            }
            if j + 1 < ny {
                couple(a, a + nx);
            }
        }
    }
    cholesky_in_place(&mut ata, n)?;
    Some(
        atb.into_iter()
            .map(|b| cholesky_solve(&ata, n, b))
            .collect(),
    )
}

/// Lower Cholesky factor in place (the upper triangle is left stale).
/// `None` on a non-positive pivot.
fn cholesky_in_place(m: &mut [f64], n: usize) -> Option<()> {
    for j in 0..n {
        let mut d = m[j * n + j];
        for k in 0..j {
            d -= m[j * n + k] * m[j * n + k];
        }
        if !(d > 0.0) {
            return None;
        }
        let d = d.sqrt();
        m[j * n + j] = d;
        for i in j + 1..n {
            let mut s = m[i * n + j];
            for k in 0..j {
                s -= m[i * n + k] * m[j * n + k];
            }
            m[i * n + j] = s / d;
        }
    }
    Some(())
}

fn cholesky_solve(l: &[f64], n: usize, mut b: Vec<f64>) -> Vec<f64> {
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s -= l[k * n + i] * b[k];
        }
        b[i] = s / l[i * n + i];
    }
    b
}

/// The two surfaces `B = B_R − A·B_T` is formed from (spec §1.2 step 7,
/// L-6). `refit` is false when the pair had no supported knot cell in
/// common (or, unreachably, different geometry): the two own-threshold
/// surfaces are then kept and the caller logs one `warn!` per frame.
#[derive(Debug, Clone)]
pub struct PairSurfaces<'a> {
    pub reference: Cow<'a, Surface>,
    pub target: Cow<'a, Surface>,
    pub shared: usize,
    pub refit: bool,
}

/// The symmetric shared-acceptance refit: both surfaces refitted once, with
/// one factorization and two right-hand sides, on the tiles BOTH final fits
/// kept. No rejection round; each side keeps its own sample values.
pub fn shared_refit<'a>(
    reference: &'a BackgroundModel,
    target: &'a BackgroundModel,
) -> PairSurfaces<'a> {
    let own = |shared: usize| PairSurfaces {
        reference: Cow::Borrowed(&reference.surface),
        target: Cow::Borrowed(&target.surface),
        shared,
        refit: false,
    };
    let (rt, tt) = (&reference.table, &target.table);
    if (rt.width, rt.height, rt.tw, rt.th, reference.scale)
        != (tt.width, tt.height, tt.tw, tt.th, target.scale)
    {
        debug_assert!(false, "shared_refit on two geometries");
        return own(0);
    }
    let ids: Vec<usize> = (0..rt.tiles.len())
        .filter(|&k| reference.accepted[k] && target.accepted[k])
        .collect();
    let s = &reference.surface;
    if supported_cells(rt, s.knot, s.nx, s.ny, &ids).0 == 0 {
        return own(ids.len());
    }
    let rv: Vec<f32> = rt.tiles.iter().map(|t| t.value).collect();
    let tv: Vec<f32> = tt.tiles.iter().map(|t| t.value).collect();
    match fit_coeffs(rt, s.knot, s.nx, s.ny, &ids, &[&rv, &tv]) {
        Some(mut c) => {
            let tgt = c.pop().expect("two right-hand sides");
            let rf = c.pop().expect("two right-hand sides");
            PairSurfaces {
                reference: Cow::Owned(Surface {
                    coeffs: rf,
                    ..s.clone()
                }),
                target: Cow::Owned(Surface {
                    coeffs: tgt,
                    ..target.surface.clone()
                }),
                shared: ids.len(),
                refit: true,
            }
        }
        None => own(ids.len()),
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
            None,
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
            None,
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
            None,
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
            None,
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
        let mut dev = Vec::new();
        let (level, ok) = robust_cell_level(&mut samples, &mut dev, &DEFAULT_PARAMS);
        assert!(
            !ok,
            "40% of the cell is a systematic outlier block, above the 30% \
             rejection limit: must be flagged invalid, got level {level}"
        );
    }

    /// Perf tier A Task 12 pin: `robust_cell_level` on caller-owned scratch
    /// (perf tier A) must return the identical `(level, ok)` a fresh `Vec`
    /// per round used to — the exactness argument is that `median_in_place`/
    /// `mad_about` are order-independent SELECTIONS over the same multiset
    /// (see the function's own doc), so reusing/reordering the scratch
    /// buffers changes nothing. A clean 20-sample set with one dominant
    /// outlier cluster exercises more than one clip round (unlike the
    /// over-rejected test above, which stabilizes/fails on round 1).
    #[test]
    fn robust_cell_level_on_scratch_matches_a_fresh_allocation_every_round() {
        let base: Vec<f32> = vec![
            0.200, 0.201, 0.199, 0.202, 0.198, 0.2005, 0.1995, 0.2015, 0.1985, 0.2, 0.2001, 0.1999,
            0.2008, 0.1992, 0.2003, 0.1997, 0.2006, 0.1994, 0.2002, 0.1998,
        ];
        // A reference computed with a brand-new `Vec` per call (the
        // pre-Task-12 shape), reimplemented locally so this pin does not
        // depend on the very code path it is checking.
        fn reference_robust_cell_level(samples: &[f32], p: &BackgroundParams) -> (f32, bool) {
            if samples.is_empty() {
                return (0.0, false);
            }
            let total = samples.len();
            let mut kept = samples.to_vec();
            for _ in 0..MAX_SIGMA_CLIP_ROUNDS {
                if kept.is_empty() {
                    break;
                }
                let m = crate::integration::stats::median_of(&kept);
                let mad = mad_about(&kept, m);
                if mad <= 0.0 {
                    break;
                }
                let bound = p.deviation_sigma * MAD_TO_SIGMA * mad;
                let next: Vec<f32> = kept
                    .iter()
                    .copied()
                    .filter(|&v| (v - m).abs() <= bound)
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
            (crate::integration::stats::median_of(&kept), true)
        }

        for outliers in [0usize, 1, 3, 7] {
            let mut samples = base.clone();
            samples.extend(std::iter::repeat(0.9f32).take(outliers));
            let expected = reference_robust_cell_level(&samples, &DEFAULT_PARAMS);

            let mut scratch = samples.clone();
            let mut dev = Vec::new();
            let got = robust_cell_level(&mut scratch, &mut dev, &DEFAULT_PARAMS);

            assert_eq!(
                got.1, expected.1,
                "outliers={outliers}: validity flag diverged"
            );
            if expected.1 {
                assert_eq!(
                    got.0.to_bits(),
                    expected.0.to_bits(),
                    "outliers={outliers}: level diverged ({} vs {})",
                    got.0,
                    expected.0
                );
            }
        }
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
            None,
        );
        assert_eq!(g.invalid_cells, g.gw * g.gh, "{:?}", g.cells);
        assert!(g.cells.iter().all(|&v| v == 0.0), "{:?}", g.cells);
    }

    fn pool() -> Arc<rayon::ThreadPool> {
        Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(4)
                .build()
                .unwrap(),
        )
    }

    /// Perf tier A Task 12's primary pin: `background_grid` on the shared
    /// `pool` (`clean_plane`'s two passes AND the cell loop now run in
    /// parallel) returns bit-identical `cells`/`invalid_cells` to the
    /// serial (`pool = None`) path — the module's own doc argues why: both
    /// of `clean_plane`'s passes are elementwise (no pixel reads another's
    /// OUTPUT), and a cell's `(level, ok)` is a pure function of that
    /// cell's own gathered samples, read by no other cell — so no cell's
    /// result can depend on which worker computed it or in what order. A
    /// star field (some pixels blown past the high clip, to exercise the
    /// hot-pixel pass) plus a NaN and an Inf pixel (exercising the low/high
    /// clip and `gather_cell`'s finite filter) over a plane large enough
    /// (25×19 cells at stride 32) that the parallel path actually spans
    /// several workers.
    #[test]
    fn background_grid_is_bit_identical_with_and_without_a_pool() {
        let (w, h) = (800usize, 600usize);
        let mut plane: Vec<f32> = (0..w * h)
            .map(|idx| {
                let x = idx % w;
                let y = idx / w;
                0.08 + 0.00002 * (x as f32) + 0.00001 * (y as f32)
            })
            .collect();
        for k in 0..400 {
            let (x, y) = ((k * 53) % w, (k * 97) % h);
            plane[y * w + x] = 0.92; // past the 0.85 high clip: hot-pixel candidates
        }
        plane[5 * w + 5] = f32::NAN;
        plane[9 * w + 9] = f32::INFINITY;

        let p = BackgroundParams {
            scale: 256,
            ..DEFAULT_PARAMS
        };
        let serial = background_grid(&plane, w, h, &p, None);
        let parallel = background_grid(&plane, w, h, &p, Some(&pool()));

        assert_eq!(serial.gw, parallel.gw);
        assert_eq!(serial.gh, parallel.gh);
        assert_eq!(serial.invalid_cells, parallel.invalid_cells);
        assert_eq!(serial.cells.len(), parallel.cells.len());
        for (idx, (&s, &pa)) in serial.cells.iter().zip(parallel.cells.iter()).enumerate() {
            assert_eq!(
                s.to_bits(),
                pa.to_bits(),
                "cell {idx}: serial {s} vs parallel {pa}"
            );
        }
    }

    // ---- Spec 2026-10-04 §1.6 pins for the model at the scale (Task 1) ----

    /// Deterministic Gaussian noise (xorshift64 + Box–Muller), so every pin
    /// is reproducible without a `rand` dependency.
    fn noisy(
        width: usize,
        height: usize,
        sky: impl Fn(f64, f64) -> f64,
        sigma: f64,
        seed: u64,
    ) -> Vec<f32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let mut spare: Option<f64> = None;
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let g = match spare.take() {
                    Some(g) => g,
                    None => {
                        let (u1, u2) = (uniform(), uniform());
                        let r = (-2.0 * u1.ln()).sqrt();
                        let t = std::f64::consts::TAU * u2;
                        spare = Some(r * t.sin());
                        r * t.cos()
                    }
                };
                out.push((sky(x as f64, y as f64) + sigma * g) as f32);
            }
        }
        out
    }

    fn model(plane: &[f32], w: usize, h: usize, scale: u32) -> BackgroundModel {
        BackgroundModel::fit(TileTable::measure(plane, w, h, &CLEANING, None), scale)
            .expect("the fixture has samples")
    }

    fn node_xy(i: usize, j: usize, stride: usize, w: usize, h: usize) -> (f64, f64) {
        (
            (i * stride).min(w - 1) as f64,
            (j * stride).min(h - 1) as f64,
        )
    }

    /// (max |S_ref − S_tgt| with the shared refit, the same without it) over
    /// the nodes within `radius` px of `centre` (P-2).
    fn pair_difference_near(
        rf: &[f32],
        tgt: &[f32],
        w: usize,
        h: usize,
        scale: u32,
        centre: (f64, f64),
        radius: f64,
    ) -> (f64, f64) {
        let (rm, tm) = (model(rf, w, h, scale), model(tgt, w, h, scale));
        let pair = shared_refit(&rm, &tm);
        assert!(pair.refit, "the fixture always has shared samples");
        let stride = (scale / 8) as usize;
        let (gw, gh) = LnGrid::grid_dims(w, h, stride);
        let (rs, ts) = (pair.reference.nodes(stride), pair.target.nodes(stride));
        let (ro, to) = (rm.surface.nodes(stride), tm.surface.nodes(stride));
        let (mut shared, mut own) = (0f64, 0f64);
        for j in 0..gh {
            for i in 0..gw {
                let (x, y) = node_xy(i, j, stride, w, h);
                if (x - centre.0).hypot(y - centre.1) > radius {
                    continue;
                }
                let k = j * gw + i;
                shared = shared.max((rs[k] - ts[k]).abs() as f64);
                own = own.max((ro[k] - to[k]).abs() as f64);
            }
        }
        (shared, own)
    }

    /// P-21: no sample is rejected on rounding-level residuals.
    #[test]
    fn a_constant_plane_gives_constant_nodes() {
        let (w, h) = (1024usize, 768usize);
        let m = model(&vec![0.1f32; w * h], w, h, 256);
        for v in m.surface.nodes(32) {
            assert!(((v as f64) / 0.1 - 1.0).abs() < 1e-7, "{v}");
        }
        assert_eq!(m.samples_rejected, 0);
        assert_eq!(m.cells_unsupported, 0);
    }

    /// P-1 at λ = 1e-2 (P-20): 5e-4 of the amplitude one knot interval
    /// inside, 2e-2 at the edges (the membrane's documented pull toward the
    /// edge level). Prototype: 2.8e-4 inside, 1.34e-2 at the worst corner
    /// node. (At λ = 1e-4 the bars would be 1e-5 and 2e-3.)
    #[test]
    fn a_linear_gradient_is_reproduced_inside_and_flattened_only_at_the_edges() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let amp = 0.02;
        let sky =
            |x: f64, y: f64| 0.1 + amp * (0.6 * x / (w - 1) as f64 + 0.4 * y / (h - 1) as f64);
        let plane: Vec<f32> = (0..w * h)
            .map(|k| sky((k % w) as f64, (k / w) as f64) as f32)
            .collect();
        let m = model(&plane, w, h, scale);
        let stride = 64;
        let (gw, gh) = LnGrid::grid_dims(w, h, stride);
        let nodes = m.surface.nodes(stride);
        let s = scale as f64;
        for j in 0..gh {
            for i in 0..gw {
                let (x, y) = node_xy(i, j, stride, w, h);
                let err = (nodes[j * gw + i] as f64 - sky(x, y)).abs() / amp;
                assert!(err <= 2e-2, "node ({i},{j}): {err:e} of the amplitude");
                let inside = x >= s && x <= (w - 1) as f64 - s && y >= s && y <= (h - 1) as f64 - s;
                if inside {
                    assert!(
                        err <= 5e-4,
                        "inside node ({i},{j}): {err:e} of the amplitude"
                    );
                }
            }
        }
    }

    /// P-2 halo pin. Prototype (3 seeds, λ = 1e-2): ≤ 0.27 % of sky with
    /// the refit.
    #[test]
    fn a_halo_star_moves_reference_and_target_alike_at_three_noise_ratios() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let (cx, cy) = (0.55 * w as f64, 0.45 * h as f64);
        let sky = |x: f64, y: f64| {
            let r2 = (x - cx).powi(2) + (y - cy).powi(2);
            0.1 + 0.6 * (1.0 + r2 / 225.0).powf(-1.5)
        };
        let sigma_t = 0.188 * 0.1;
        for (k, n) in [1.0f64, 5.0, 28.0].into_iter().enumerate() {
            let tgt = noisy(w, h, sky, sigma_t, 11 + k as u64);
            let rf = noisy(w, h, sky, sigma_t / n.sqrt(), 101 + k as u64);
            let (shared, own) = pair_difference_near(&rf, &tgt, w, h, scale, (cx, cy), 600.0);
            assert!(
                shared <= 0.005 * 0.1,
                "n = {n}: {:.3} % of sky with the refit ({:.3} % without)",
                1e3 * shared,
                1e3 * own
            );
        }
    }

    /// P-2 broad-feature pin: FWHM = 1.3 × scale at +30 % of sky. Without
    /// the shared refit (L-6) the low-noise reference rejects more of the
    /// feature than the target does; with it, both surfaces agree.
    /// Prototype (λ = 1e-2): without 0.88 / 0.97 % at n = 5 / 28, with
    /// ≤ 0.20 %.
    #[test]
    fn a_broad_feature_needs_the_shared_refit() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let (cx, cy) = (0.55 * w as f64, 0.45 * h as f64);
        let fwhm = 1.3 * scale as f64;
        let s = fwhm / 2.354_820_045;
        let sky = |x: f64, y: f64| {
            let r2 = (x - cx).powi(2) + (y - cy).powi(2);
            0.1 * (1.0 + 0.30 * (-0.5 * r2 / (s * s)).exp())
        };
        let sigma_t = 0.188 * 0.1;
        for (k, n) in [1.0f64, 5.0, 28.0].into_iter().enumerate() {
            let tgt = noisy(w, h, sky, sigma_t, 21 + k as u64);
            let rf = noisy(w, h, sky, sigma_t / n.sqrt(), 201 + k as u64);
            let (shared, own) = pair_difference_near(&rf, &tgt, w, h, scale, (cx, cy), fwhm);
            assert!(
                shared <= 0.005 * 0.1,
                "n = {n}: {:.3} % of sky with the refit",
                1e3 * shared
            );
            if n >= 5.0 {
                assert!(
                    own > 0.005 * 0.1,
                    "n = {n}: the fixture no longer shows the noise asymmetry the refit removes \
                     ({:.3} % without the refit)",
                    1e3 * own
                );
            }
        }
    }

    /// Review Focus 1 / P-19: the covered part is right and the strip stays
    /// bounded (a blow-up guard, not a fidelity bar). Prototype (λ = 1e-2,
    /// 3 seeds): covered part ≤ 0.09 % of sky; strip ≤ 2.6 % (a 10 %-of-sky
    /// gradient) and ≤ 0.61 % (flat).
    #[test]
    fn an_off_canvas_strip_stays_bounded_and_leaves_the_covered_part_exact() {
        let (w, h, scale) = (2048usize, 1536usize, 512u32);
        let stride = 64;
        for (grad, strip_bar) in [(0.01f64, 0.05 * 0.1), (0.0, 0.02 * 0.1)] {
            let sky = |x: f64, _y: f64| 0.1 + grad * x / (w - 1) as f64;
            let mut plane = noisy(w, h, sky, 0.002, 3);
            for y in 0..h {
                for x in 1400..w {
                    plane[y * w + x] = f32::NAN;
                }
            }
            let m = model(&plane, w, h, scale);
            let (gw, gh) = LnGrid::grid_dims(w, h, stride);
            let nodes = m.surface.nodes(stride);
            let edge_level = sky(1400.0, 0.0);
            for j in 0..gh {
                for i in 0..gw {
                    let (x, y) = node_xy(i, j, stride, w, h);
                    let v = nodes[j * gw + i] as f64;
                    if x >= 1400.0 {
                        assert!(
                            (v - edge_level).abs() <= strip_bar,
                            "grad {grad}: strip node ({i},{j}) {v} vs edge level {edge_level}"
                        );
                    } else if x <= 1400.0 - scale as f64 {
                        assert!(
                            (v - sky(x, y)).abs() <= 0.003 * 0.1,
                            "grad {grad}: covered node ({i},{j}) {v} vs {}",
                            sky(x, y)
                        );
                    }
                }
            }
        }
    }

    /// The pre-2026-10 full-plane cleaning, re-implemented naïvely so this
    /// oracle survives Task 2's deletion of `clean_plane`.
    fn naive_tile_table(plane: &[f32], w: usize, h: usize) -> TileTable {
        let p = CLEANING;
        let mut hot = plane.to_vec();
        for y in 0..h {
            for x in 0..w {
                let v = plane[y * w + x];
                if !(v.is_finite() && v > p.high_clip_rel) {
                    continue;
                }
                let mut win = Vec::new();
                for wy in y.saturating_sub(2)..=(y + 2).min(h - 1) {
                    for wx in x.saturating_sub(2)..=(x + 2).min(w - 1) {
                        let wv = plane[wy * w + wx];
                        if wv.is_finite() {
                            win.push(wv);
                        }
                    }
                }
                let med = median_in_place(&mut win);
                let mad = mad_about(&win, med);
                if v > med + 5.0 * MAD_TO_SIGMA * mad {
                    hot[y * w + x] = med;
                }
            }
        }
        let (tw, th) = (w.div_ceil(TILE_PX), h.div_ceil(TILE_PX));
        let mut tiles = Vec::new();
        for v in 0..th {
            for u in 0..tw {
                let (x0, y0) = (u * TILE_PX, v * TILE_PX);
                let (x1, y1) = ((x0 + TILE_PX).min(w), (y0 + TILE_PX).min(h));
                let mut vals = Vec::new();
                let mut max = f32::NEG_INFINITY;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let c = hot[y * w + x];
                        if !c.is_finite() {
                            continue;
                        }
                        max = max.max(c);
                        if c >= p.low_clip && c <= p.high_clip_rel {
                            vals.push(c);
                        }
                    }
                }
                let max = if max == f32::NEG_INFINITY {
                    f32::NAN
                } else {
                    max
                };
                let present = vals.len() as u32;
                let (value, sigma) = if vals.len() < MIN_TILE_PRESENT {
                    (f32::NAN, f32::NAN)
                } else {
                    for _ in 0..5 {
                        let m = crate::integration::stats::median_of(&vals);
                        let mad = mad_about(&vals, m);
                        if mad <= 0.0 {
                            break;
                        }
                        let next: Vec<f32> = vals
                            .iter()
                            .copied()
                            .filter(|&c| (c - m).abs() <= 3.0 * MAD_TO_SIGMA * mad)
                            .collect();
                        let done = next.len() == vals.len();
                        vals = next;
                        if done {
                            break;
                        }
                    }
                    let m = crate::integration::stats::median_of(&vals);
                    (m, MAD_TO_SIGMA * mad_about(&vals, m))
                };
                tiles.push(TileStat {
                    x0: x0 as u32,
                    y0: y0 as u32,
                    x1: x1 as u32,
                    y1: y1 as u32,
                    present,
                    value,
                    sigma,
                    max,
                });
            }
        }
        TileTable {
            width: w,
            height: h,
            tw,
            th,
            tiles,
        }
    }

    fn assert_tables_bit_identical(a: &TileTable, b: &TileTable) {
        assert_eq!(
            (a.width, a.height, a.tw, a.th),
            (b.width, b.height, b.tw, b.th)
        );
        assert_eq!(a.tiles.len(), b.tiles.len());
        for (k, (s, t)) in a.tiles.iter().zip(&b.tiles).enumerate() {
            assert_eq!(
                (s.x0, s.y0, s.x1, s.y1, s.present),
                (t.x0, t.y0, t.x1, t.y1, t.present),
                "tile {k}"
            );
            assert_eq!(
                s.value.to_bits(),
                t.value.to_bits(),
                "tile {k} value {} vs {}",
                s.value,
                t.value
            );
            assert_eq!(s.sigma.to_bits(), t.sigma.to_bits(), "tile {k} sigma");
            assert_eq!(s.max.to_bits(), t.max.to_bits(), "tile {k} max");
        }
    }

    /// A field with hot pixels (above the 0.85 clip), saturated stars, a NaN
    /// and an Inf, over a partial last tile on both axes (530 × 400).
    fn dirty_field() -> (Vec<f32>, usize, usize) {
        let (w, h) = (530usize, 400usize);
        let mut plane = noisy(w, h, |x, y| 0.08 + 2e-5 * x + 1e-5 * y, 0.003, 7);
        for k in 0..300 {
            let (x, y) = ((k * 53) % w, (k * 97) % h);
            plane[y * w + x] = 0.95;
        }
        for (cx, cy) in [(100usize, 100usize), (400, 300)] {
            for y in cy - 3..=cy + 3 {
                for x in cx - 3..=cx + 3 {
                    plane[y * w + x] = 0.97;
                }
            }
        }
        plane[5 * w + 5] = f32::NAN;
        plane[9 * w + 9] = f32::INFINITY;
        (plane, w, h)
    }

    #[test]
    fn tile_statistics_match_the_full_plane_cleaning() {
        let (plane, w, h) = dirty_field();
        assert_tables_bit_identical(
            &TileTable::measure(&plane, w, h, &CLEANING, None),
            &naive_tile_table(&plane, w, h),
        );
    }

    /// P-3 / L-5: reuse whole-overlap tiles (apron included), recompute the
    /// edge tiles — bit-identical to measuring the masked plane outright.
    #[test]
    fn restricted_tiles_equal_measuring_the_masked_plane() {
        let (mut plane, w, h) = dirty_field();
        let mask: Vec<bool> = (0..w * h)
            .map(|k| {
                let (x, y) = ((k % w) as f64, (k / w) as f64);
                x + 0.7 * y > 150.0 && (x - 260.0).hypot(y - 200.0) < 230.0
            })
            .collect();
        // P-3 discriminator. Tile (u, v) = (4, 1) spans x 128..160, y 32..64 and
        // lies wholly inside the mask, but its hot-pixel apron crosses the mask
        // edge. Bright pixels (0.95) fill the 11 masked-out pixels of the 5 x 5
        // window of (128, 32) plus the in-mask (128, 32) and (129, 32): 13 of 25
        // are bright, so on the unmasked plane the window median is bright and
        // (128, 32) is NOT replaced (it stays above the clip and drops out), while
        // on the masked plane only 2 of its 14 present window pixels are bright and
        // it IS replaced by the window median. A tile-only reuse rule would keep
        // the group value for this tile and fail the assertion below.
        for (x, y) in [
            (126usize, 30usize),
            (127, 30),
            (128, 30),
            (129, 30),
            (126, 31),
            (127, 31),
            (128, 31),
            (126, 32),
            (127, 32),
            (126, 33),
            (126, 34),
            (128, 32),
            (129, 32),
        ] {
            plane[y * w + x] = 0.95;
        }
        let group = TileTable::measure(&plane, w, h, &CLEANING, None);
        let restricted = group.restricted(&plane, &mask, &CLEANING, None);
        let tile = TILE_PX;
        let k = (32 / tile) * group.tw + 128 / tile;
        let (g, r) = (&group.tiles[k], &restricted.tiles[k]);
        assert!(
            (g.present, g.value.to_bits(), g.max.to_bits())
                != (r.present, r.value.to_bits(), r.max.to_bits()),
            "tile (4, 1) must differ between the group and the masked measurement"
        );
        let masked_plane: Vec<f32> = plane
            .iter()
            .zip(&mask)
            .map(|(&v, &m)| if m { v } else { f32::NAN })
            .collect();
        assert_tables_bit_identical(
            &restricted,
            &TileTable::measure(&masked_plane, w, h, &CLEANING, None),
        );
        assert_tables_bit_identical(
            &restricted,
            &TileTable::measure_masked(&plane, w, h, &CLEANING, None, &mask),
        );
    }

    #[test]
    fn the_model_is_bit_identical_with_and_without_a_pool_and_run_to_run() {
        let (plane, w, h) = dirty_field();
        let serial =
            BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, None), 256).unwrap();
        let again =
            BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, None), 256).unwrap();
        let pooled = BackgroundModel::fit(
            TileTable::measure(&plane, w, h, &CLEANING, Some(&pool())),
            256,
        )
        .unwrap();
        for other in [&again, &pooled] {
            assert_tables_bit_identical(&serial.table, &other.table);
            assert_eq!(serial.accepted, other.accepted);
            let bits = |m: &BackgroundModel| {
                m.surface
                    .coeffs
                    .iter()
                    .map(|c| c.to_bits())
                    .collect::<Vec<_>>()
            };
            assert_eq!(bits(&serial), bits(other));
        }
    }

    #[test]
    fn a_plane_with_no_present_pixel_is_refused() {
        let (w, h) = (256usize, 256usize);
        let r = BackgroundModel::fit(
            TileTable::measure(&vec![0.0f32; w * h], w, h, &CLEANING, None),
            256,
        );
        assert_eq!(r.err(), Some(BackgroundRefusal::NoSamples));
    }

    /// L-4: a knot cell needs 8 accepted samples. Seven sample tiles in one
    /// cell of a 1024 × 768 plane at scale 256 refuse; eight fit.
    #[test]
    fn the_sample_floor_refuses_a_plane_with_no_supported_cell() {
        let (w, h) = (1024usize, 768usize);
        for (tiles, ok) in [(7usize, false), (8, true)] {
            let mut plane = vec![f32::NAN; w * h];
            for t in 0..tiles {
                let (x0, y0) = ((t % 4) * TILE_PX, (t / 4) * TILE_PX);
                for y in y0..y0 + TILE_PX {
                    for x in x0..x0 + TILE_PX {
                        plane[y * w + x] = 0.1;
                    }
                }
            }
            let r = BackgroundModel::fit(TileTable::measure(&plane, w, h, &CLEANING, None), 256);
            assert_eq!(r.is_ok(), ok, "{tiles} sample tiles");
            if !ok {
                assert_eq!(r.err(), Some(BackgroundRefusal::NoSupportedCell));
            }
        }
    }

    #[test]
    fn shared_refit_with_no_common_supported_cell_keeps_the_own_surfaces() {
        let (w, h) = (1024usize, 768usize);
        let half = |left: bool| -> Vec<f32> {
            (0..w * h)
                .map(|k| {
                    if ((k % w) < w / 2) == left {
                        0.1
                    } else {
                        f32::NAN
                    }
                })
                .collect()
        };
        let (rm, tm) = (
            model(&half(true), w, h, 256),
            model(&half(false), w, h, 256),
        );
        let pair = shared_refit(&rm, &tm);
        assert!(!pair.refit);
        assert_eq!(pair.shared, 0);
        assert_eq!(*pair.reference, rm.surface);
        assert_eq!(*pair.target, tm.surface);
    }

    /// Step 5: a tile-aligned 3 × 3 block offset by +20 % of sky is
    /// rejected and the surface stays on the sky. Prototype (an unaligned
    /// block, λ = 1e-2): 9–10 rejected, max node error ≤ 0.13 % of sky.
    #[test]
    fn the_rejection_rounds_remove_an_outlier_block() {
        let (w, h) = (1024usize, 768usize);
        let mut plane = noisy(w, h, |_, _| 0.1, 0.002, 5);
        for y in 320..416 {
            for x in 512..608 {
                plane[y * w + x] += 0.02;
            }
        }
        let m = model(&plane, w, h, 256);
        assert!(m.samples_rejected >= 9, "{} rejected", m.samples_rejected);
        for v in m.surface.nodes(32) {
            assert!((v as f64 - 0.1).abs() <= 0.005 * 0.1, "{v}");
        }
    }
}
