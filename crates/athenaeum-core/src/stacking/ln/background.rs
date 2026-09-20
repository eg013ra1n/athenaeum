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
//!
//! **Perf tier C item C5 (ruling C-5): the cell statistics are taken on a
//! [`LN_BIN`]×`LN_BIN` reduction of the plane, not the plane itself.** The
//! mesh is unchanged — node `(i, j)` still sits at `(i·stride, j·stride)`
//! in FULL-resolution pixels, which is what [`super::grid::LnGrid`], its
//! B-spline evaluator and the integration engine's row evaluator index, and
//! what the `.athln` sidecar's layout is — only what is measured INSIDE
//! each cell moves: [`clip_and_bin`] applies the pre-C5 hot-pixel and
//! clipping rules per SOURCE pixel and averages the survivors of each 4×4
//! block, and the cell loop gathers those blocks (`stride / LN_BIN` of them
//! per cell edge at the default scale: 32, not 128). The model lives on a
//! 128-px stride, so a 32-px-stride input carries it with room to spare.
//! The node values move at the 1e-3 relative level and the `.athln`
//! sidecars move with them — that is what [`LN_BACKGROUND_VERSION`] is for,
//! and what this module's own delta pins against the verbatim pre-C5
//! oracle measure.

use std::sync::Arc;

use rayon::prelude::*;

use super::grid::LnGrid;
use crate::integration::stats::{mad_about, median_in_place, MAD_TO_SIGMA};

/// Deviation multiple (in MAD-sigma) a pixel must exceed its local window
/// median by before the hot-pixel pass replaces it.
const HOT_PIXEL_SIGMA: f32 = 5.0;
/// A cell whose window holds more than this many SAMPLES is subsampled 2×2
/// before the iterative clip (§ algorithm step 3) — cheap without
/// materially changing the robust statistics on a scale/8 mesh's largest
/// cells. Since perf tier C item C5 a "sample" is a [`LN_BIN`]×`LN_BIN`
/// BIN, not a pixel, so the threshold bites `LN_BIN²` later in plane
/// terms: the default scale's 128×128-px cell is 32×32 = 1 024 bins and no
/// longer subsamples at all (it used to, at 16 384 pixels), while the
/// largest scale the config offers (4096 → stride 512) still does at
/// 128×128 = 16 384 bins.
const CELL_SUBSAMPLE_THRESHOLD: usize = 4096;
/// Iterative per-cell median±sigma clipping stops after this many rounds
/// even if the kept set has not yet stabilized.
const MAX_SIGMA_CLIP_ROUNDS: usize = 5;

/// Perf tier C item C5 (ruling C-5): the plane is reduced
/// `LN_BIN`×`LN_BIN` (mean of the surviving pixels of each block, see
/// [`clip_and_bin`]) before any cell statistic is taken. The background is
/// modelled on a `scale/8` = 128-px node mesh, so a 32-px-stride input
/// carries it with room to spare, and the cell loop then reads 16× fewer
/// samples. The MESH itself is untouched — see [`background_grid`]'s own
/// windowing comment.
pub const LN_BIN: usize = 4;

/// How many of a bin's `LN_BIN²` source pixels must survive
/// [`clip_and_bin`]'s hot-pixel/clip rule for the bin to carry a value at
/// all; below it the bin is `NaN` and [`gather_cell`] drops it. Half the
/// block: a bin built from a handful of survivors is a star's edge, not a
/// background sample.
const LN_BIN_MIN_FINITE: usize = 8;

/// The background model's own version (perf tier C item C5), folded into
/// the `ln` and `ln_reference` artifact hashes — and NOTHING else — by
/// [`crate::stacking::config::normalization_subtree`]. Neither the
/// measurement nor the registration hash moves with it, and it is not part
/// of the whole-config run fingerprint: the node values this module
/// produces are stored in `.athln` sidecars and nowhere upstream of them.
/// `2` is the [`LN_BIN`]-binned model; `1` was the full-resolution one
/// M2–perf-tier-A shipped, so the FIRST run after this re-normalizes every
/// set once, on purpose.
pub const LN_BACKGROUND_VERSION: u32 = 2;

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
/// Since perf tier C item C5 the cell's samples are the
/// [`LN_BIN`]×`LN_BIN` bins [`clip_and_bin`] produced, not the plane's own
/// pixels — see the module doc and [`clip_and_bin`]'s.
///
/// `pool` (perf tier A Task 12) runs [`clip_and_bin`]'s reduction and this
/// function's own per-cell loop on the caller's pool when given one — see
/// each site's own doc for why every one of those loops is exact under
/// parallel order: each output bin is a pure function of its own source
/// pixels and the read-only input plane (never of another bin's output),
/// and a cell's statistics are a pure function of that cell's own gathered
/// samples, read by no other cell. `None` does NOT
/// mean serial: a rayon parallel iterator called outside an explicit
/// `ThreadPool::install` still runs on rayon's own lazily-initialized
/// GLOBAL pool (sized by `available_parallelism`, invisible to the
/// caller's `image_pool`/admission budget) — the same convention
/// `psf_signal::fit_all`'s own doc states. Either way the RESULT is
/// bit-identical (the module's own pin proves it) — `pool` only changes
/// whose workers do the work, never what they compute.
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

    // `width < LN_BIN || height < LN_BIN` joins the pre-existing degenerate
    // inputs (perf tier C item C5): a plane thinner than ONE bin on either
    // axis reduces to an empty scratch, so no cell could be measured from
    // it — report that the way the other degenerate cases already are,
    // all-invalid, which every caller already treats as "refuse" (see
    // `BackgroundGrid`'s own doc and `ln/mod.rs`'s `invalid_cells == gw *
    // gh` check). Unreachable from the pipeline: an LN plane is a
    // reference-geometry frame, thousands of pixels on a side.
    if width < LN_BIN
        || height < LN_BIN
        || plane.len() < width.saturating_mul(height)
        || cell_count == 0
    {
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
    let (binned, bw, bh) = clip_and_bin(plane, width, height, p, pool);
    debug_assert_eq!(binned.len(), bw * bh);

    let half = stride / 2;
    let half_hi = stride - half; // symmetric for even stride; keeps total width == stride for odd stride too

    // The cell loop, flattened to `0..cell_count` (row-major, `idx = j*gw +
    // i` — the same layout the nested `for j { for i { ... } }` loop it
    // replaces used) so it can run as one indexed parallel iterator: each
    // cell's window bounds (`node_x`/`node_y`/`x0`/`x1`/`y0`/`y1`) are pure
    // functions of `idx`/the constants above, and `gather_cell` +
    // `robust_cell_level` read only `binned` (read-only, shared) and the
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

                // Perf tier C item C5: the MESH is unchanged — node `(i,
                // j)` still sits at `(i·stride, j·stride)` in
                // FULL-resolution pixels, which is what `LnGrid`,
                // `grid.rs`'s B-spline evaluator and the integration
                // engine's row evaluator all index, and what the `.athln`
                // sidecar's layout is. Only the STATISTICS inside the cell
                // move: the same full-resolution window, mapped onto the
                // binned scratch by taking the bins whose CENTRES fall
                // inside it — i.e. rounding each edge to the nearest bin
                // boundary (`+ LN_BIN/2` then floor), not flooring the low
                // edge and ceiling the high one. For a window whose edges
                // are multiples of `LN_BIN` the two agree exactly, and
                // every scale the pipeline offers is such a case (`stride`
                // and `half` are both multiples of `LN_BIN` for
                // `scale ∈ [256, 4096]`); they differ only where `x0`/`x1`
                // are NOT — the clamped trailing node and the plane's own
                // right/bottom edge — and there the floor/ceil pair
                // silently widens the window by up to `LN_BIN - 1` px on
                // EACH side, which shifts the cell's own centre. Measured
                // on the `vertical_gradient` fixture: floor/ceil moved the
                // trailing cell 2.0e-3 off the unbinned oracle (0.07455 vs
                // 0.0747, a window centred 2 rows low), rounding 6.7e-4.
                let bx0 = ((x0 + LN_BIN / 2) / LN_BIN).min(bw);
                let bx1 = ((x1 + LN_BIN / 2) / LN_BIN).min(bw);
                let by0 = ((y0 + LN_BIN / 2) / LN_BIN).min(bh);
                let by1 = ((y1 + LN_BIN / 2) / LN_BIN).min(bh);

                gather_cell(&binned, bw, bx0, bx1, by0, by1, &mut scratch.samples);
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

/// Steps 1–2 of the algorithm FUSED into perf tier C item C5's reduction
/// (ruling C-5): one pass over the plane that yields the
/// `LN_BIN`×`LN_BIN`-binned scratch the cell loop reads — with no
/// full-resolution intermediate copy at all, where the pre-C5 `clean_plane`
/// allocated and wrote a whole second plane (≈ 104 MB on a 26 Mpx frame)
/// and then walked it twice more.
///
/// **The per-SOURCE-pixel rule is the pre-C5 one, unchanged and in the same
/// order** — spec §6's "clip-then-bin": (1) a finite pixel past the high
/// clip is a hot-pixel candidate, and its `(2·hot_radius+1)²` window of the
/// ORIGINAL plane (never of any partially-written output — the old pass
/// read `plane` too, which is exactly what made it exact under any row
/// split) gives `med`/`mad`; the pixel becomes `med` when it exceeds `med +
/// HOT_PIXEL_SIGMA · MAD_TO_SIGMA · mad`, and otherwise keeps its own value
/// (a saturated star CORE, whose neighbourhood is bright too, is not a hot
/// pixel). (2) The resulting value is kept only when it is finite and
/// inside `[low_clip, high_clip]`. So a star core and anything else past
/// the high clip is ABSENT from its bin rather than averaged into it — the
/// other order (bin first, clip the bin) would let one saturated pixel drag
/// a whole 4×4 block 50 % above the sky and hand the cell's iterative clip
/// a contaminated sample to reject, inflating the rejected fraction that
/// decides whether the cell is valid at all.
///
/// The surviving values of each block are then averaged (`f64`
/// accumulation in a fixed row-major order — deterministic, and a mean of
/// ≤ 16 `f32`s in `[4.5e-5, 0.85]` loses nothing to it); a block with fewer
/// than [`LN_BIN_MIN_FINITE`] survivors is `NaN`, which [`gather_cell`]
/// drops exactly as it drops a clipped pixel today.
///
/// **Edge remainder**: `bw = width / LN_BIN`, `bh = height / LN_BIN` — the
/// trailing `width % LN_BIN` columns and `height % LN_BIN` rows (at most 3
/// of each) are DROPPED, not averaged into a short bin. A partial bin would
/// need a survivor rule of its own and would carry up to 4× a full bin's
/// variance, in exchange for a strip that is 0.07 % of a 4096-px axis, at
/// the very edge of a surface sampled every 128 px; the edge cell still
/// gathers ~1 000 whole bins either way.
///
/// **Deviation thresholds are NOT re-derived.** `HOT_PIXEL_SIGMA` still
/// judges a FULL-resolution pixel against a full-resolution window here, so
/// nothing about it moved. `deviation_sigma`/[`TARGET_DEVIATION_SIGMA`], in
/// [`robust_cell_level`], now clip binned values whose noise is ≈ `LN_BIN`×
/// lower — but the bound they scale is that same sample's OWN MAD, so the
/// clip is self-consistent and scale-free by construction; widening them by
/// `LN_BIN` to preserve the pre-C5 ABSOLUTE bound was measured against the
/// unbinned oracle and is the worse of the two (task 7's report has both
/// numbers), because it re-admits the star wings the per-cell clip exists
/// to remove.
///
/// Parallel over OUTPUT bin rows (`par_chunks_mut(bw)`): every output bin
/// is a pure function of its own `LN_BIN` source rows plus the read-only
/// `plane`, so no bin can observe another's result and the reduction is
/// exact under any row order — the same argument the two passes it replaces
/// carried, and the module's own with/without-a-pool pin still proves it.
/// The hot-pixel window is one per WORKER (`for_each_init`) instead of a
/// fresh `Vec` per candidate.
fn clip_and_bin(
    plane: &[f32],
    width: usize,
    height: usize,
    p: &BackgroundParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (Vec<f32>, usize, usize) {
    let high_thresh = high_clip_threshold(p);
    let bw = width / LN_BIN;
    let bh = height / LN_BIN;
    let mut binned = vec![f32::NAN; bw * bh];

    let mut body = || {
        binned.par_chunks_mut(bw).enumerate().for_each_init(
            Vec::<f32>::new,
            |window, (by, row)| {
                for (bx, out) in row.iter_mut().enumerate() {
                    let mut sum = 0f64;
                    let mut count = 0usize;
                    for dy in 0..LN_BIN {
                        let y = by * LN_BIN + dy;
                        let base = y * width;
                        for dx in 0..LN_BIN {
                            let x = bx * LN_BIN + dx;
                            let v = plane[base + x];
                            let mut w = v;
                            if v.is_finite() && v > high_thresh {
                                let x0 = x.saturating_sub(p.hot_radius);
                                let x1 = (x + p.hot_radius).min(width - 1);
                                let y0 = y.saturating_sub(p.hot_radius);
                                let y1 = (y + p.hot_radius).min(height - 1);
                                window.clear();
                                for wy in y0..=y1 {
                                    for wx in x0..=x1 {
                                        let wv = plane[wy * width + wx];
                                        if wv.is_finite() {
                                            window.push(wv);
                                        }
                                    }
                                }
                                if !window.is_empty() {
                                    let med = median_in_place(window);
                                    let mad = mad_about(window, med);
                                    if v > med + HOT_PIXEL_SIGMA * MAD_TO_SIGMA * mad {
                                        w = med;
                                    }
                                }
                            }
                            if w.is_finite() && w >= p.low_clip && w <= high_thresh {
                                sum += w as f64;
                                count += 1;
                            }
                        }
                    }
                    *out = if count >= LN_BIN_MIN_FINITE {
                        (sum / count as f64) as f32
                    } else {
                        f32::NAN
                    };
                }
            },
        );
    };
    match pool {
        Some(pl) => pl.install(body),
        None => body(),
    }

    (binned, bw, bh)
}

/// The window's finite samples, stride-2 subsampled (both axes) when the
/// window itself (before filtering to finite) holds more than
/// [`CELL_SUBSAMPLE_THRESHOLD`] of them — since perf tier C item C5 the
/// caller passes the BINNED scratch and its own width, so a "sample" here
/// is a `LN_BIN`×`LN_BIN` bin (`NaN` when [`clip_and_bin`] found too few
/// survivors in it, dropped by the same finite filter that used to drop a
/// clipped pixel); the pre-C5 `background_grid_unbinned_reference` passes
/// the full-resolution cleaned plane and gets the pixel reading. Written
/// into `out` (perf tier A
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
/// Since perf tier C item C5 the "samples" a production call sees are
/// [`LN_BIN`]×`LN_BIN` bins rather than pixels, so `rejection_limit` is a
/// fraction of a ~16× smaller population — but it is a FRACTION, and the
/// deviation bound it works with is scaled by that same population's own
/// MAD, so neither number is re-derived; see [`clip_and_bin`]'s doc and
/// `the_cell_deviation_sigmas_are_not_re_derived_for_the_binned_plane`.
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

/// The pre-C5 [`background_grid`], kept VERBATIM (perf tier C item C5,
/// ruling C-5) as the oracle the binned production path is measured
/// against — see
/// `the_binned_background_tracks_the_unbinned_oracle_within_a_thousandth`.
/// It reads the full-resolution plane through `clean_plane`'s two passes
/// and gathers every cell's samples per PIXEL; the shipped
/// [`background_grid`] gathers them per `LN_BIN`×`LN_BIN` BIN instead, so
/// the two are deliberately NOT expected to agree bit for bit — the pin
/// records the DELTA (spec §8: "its own pin against the pre-change code
/// recording the DELTA, not identity"). Do not "clean this up" to call the
/// shipped code: the moment it does, it stops being an independent oracle.
#[cfg(test)]
mod unbinned_reference {
    use super::*;

    pub(super) fn background_grid_unbinned_reference(
        plane: &[f32],
        width: usize,
        height: usize,
        p: &BackgroundParams,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> BackgroundGrid {
        let stride = (p.scale / 8).max(2) as usize;
        let (gw, gh) = LnGrid::grid_dims(width, height, stride);
        let cell_count = gw * gh;

        if width == 0
            || height == 0
            || plane.len() < width.saturating_mul(height)
            || cell_count == 0
        {
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
                        let mut window: Vec<f32> =
                            Vec::with_capacity((x1 - x0 + 1) * (y1 - y0 + 1));
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

    // ---- perf tier C item C5 (ruling C-5): the binned background model ----

    /// One fixture for the C5 delta pins: a plane, its geometry and the
    /// [`BackgroundParams`] to model it with. The first six are this
    /// module's own M2 LN fixtures verbatim (the same planes
    /// `flat_plane_with_stars_recovers_the_flat_level`,
    /// `vertical_gradient_is_tracked_per_cell`,
    /// `a_cell_that_is_mostly_star_is_invalid_and_filled_from_neighbours`,
    /// `trailing_node_overshoot_gets_a_real_window`,
    /// `a_plane_entirely_below_the_low_clip_is_fully_invalid` and
    /// `background_grid_is_bit_identical_with_and_without_a_pool` build);
    /// the last two are a NOISY field at the production scale (1024 →
    /// stride 128), which none of the M2 fixtures exercise — a flat or
    /// perfectly linear plane cannot tell a median of pixels from a median
    /// of 4×4 means, and the whole question C5 asks is what the reduction
    /// costs on data that actually has a noise floor.
    struct C5Fixture {
        name: &'static str,
        width: usize,
        height: usize,
        plane: Vec<f32>,
        params: BackgroundParams,
    }

    /// A deterministic pseudo-noisy sky: a smooth 2-D background gradient
    /// around `base`, per-pixel noise of about `sigma` (the sum of 4
    /// uniforms — near enough to Gaussian for a background estimator, and
    /// reproducible from the seed), and `stars` Moffat-ish sources of
    /// varying brightness, some of them past the 0.85 high clip so the
    /// hot-pixel pass and the clip both have work to do.
    fn noisy_sky(
        width: usize,
        height: usize,
        base: f32,
        sigma: f32,
        stars: usize,
        seed: u64,
    ) -> Vec<f32> {
        noisy_sky_with(width, height, base, sigma, stars, seed, false)
    }

    /// `uniform_peaks` swaps the faint-dominated power law for a UNIFORM
    /// peak draw over `[0.02, 0.62]` with one star in eleven saturated —
    /// a field carrying a mean star peak of 3× the sky, which no real one
    /// does. Only `the_binned_backgrounds_star_flux_bias_grows_with_crowding`
    /// uses it, to characterise where the reduction's limit is.
    #[allow(clippy::too_many_arguments)]
    fn noisy_sky_with(
        width: usize,
        height: usize,
        base: f32,
        sigma: f32,
        stars: usize,
        seed: u64,
        uniform_peaks: bool,
    ) -> Vec<f32> {
        let mut rng = crate::geometry::ransac::SplitMix64(seed);
        let mut plane = vec![0f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let gx = x as f32 / width as f32;
                let gy = y as f32 / height as f32;
                let bg = base * (1.0 + 0.25 * gx + 0.15 * gy - 0.1 * gx * gy);
                let mut n = 0f64;
                for _ in 0..4 {
                    n += rng.next_f64() - 0.5;
                }
                plane[y * width + x] = bg + sigma * (n as f32);
            }
        }
        for k in 0..stars {
            let cx = rng.below(width) as f32;
            let cy = rng.below(height) as f32;
            // Faint-dominated, the way a real field is: a power law over
            // the peak (most stars a few percent of sky, a handful bright,
            // one in forty saturated past the high clip). A UNIFORM peak
            // distribution — the first cut of this fixture — puts a mean
            // star peak of 3× the sky into the field, which is a flux
            // level no sky frame carries; the crowding characterisation
            // test below keeps that case, honestly labelled.
            let peak = if uniform_peaks {
                if k % 11 == 0 {
                    1.1
                } else {
                    0.02 + 0.6 * rng.next_f64() as f32
                }
            } else if k % 40 == 0 {
                1.1
            } else {
                (0.004 / (rng.next_f64().max(1e-3)).powf(0.55) as f32).min(0.5)
            };
            let r = if uniform_peaks {
                1.5 + 2.5 * rng.next_f64() as f32
            } else {
                1.2 + 1.3 * rng.next_f64() as f32
            };
            let x0 = (cx as isize - 8).max(0) as usize;
            let x1 = ((cx as isize + 8) as usize).min(width - 1);
            let y0 = (cy as isize - 8).max(0) as usize;
            let y1 = ((cy as isize + 8) as usize).min(height - 1);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let v = peak / (1.0 + (dx * dx + dy * dy) / (r * r)).powf(2.5);
                    plane[y * width + x] += v;
                }
            }
        }
        plane
    }

    fn c5_m2_fixtures() -> Vec<C5Fixture> {
        let scale256 = BackgroundParams {
            scale: 256,
            ..DEFAULT_PARAMS
        };

        let mut flat = vec![0.10f32; 512 * 384];
        for k in 0..200 {
            let (x, y) = ((k * 37) % 512, (k * 91) % 384);
            flat[y * 512 + x] = 0.9;
        }

        let mut gradient: Vec<f32> = (0..256 * 256)
            .map(|i| 0.05 + (i / 256) as f32 * 1e-4)
            .collect();
        gradient[128 * 256 + 128] = 0.9;

        let mut star_cell = vec![0.10f32; 256 * 256];
        for y in 0..40 {
            for x in 0..40 {
                star_cell[y * 256 + x] = 0.95;
            }
        }

        let mut trailing = vec![0.10f32; 1026 * 514];
        trailing[10 * 1026 + 10] = 0.9;

        let (pw, ph) = (800usize, 600usize);
        let mut pooled: Vec<f32> = (0..pw * ph)
            .map(|idx| {
                let x = idx % pw;
                let y = idx / pw;
                0.08 + 0.00002 * (x as f32) + 0.00001 * (y as f32)
            })
            .collect();
        for k in 0..400 {
            let (x, y) = ((k * 53) % pw, (k * 97) % ph);
            pooled[y * pw + x] = 0.92;
        }
        pooled[5 * pw + 5] = f32::NAN;
        pooled[9 * pw + 9] = f32::INFINITY;

        vec![
            C5Fixture {
                name: "flat_plane_with_stars",
                width: 512,
                height: 384,
                plane: flat,
                params: BackgroundParams {
                    scale: 256,
                    hot_radius: 2,
                    low_clip: 4.5e-5,
                    high_clip_rel: 0.85,
                    deviation_sigma: 3.0,
                    rejection_limit: 0.3,
                },
            },
            C5Fixture {
                name: "vertical_gradient",
                width: 256,
                height: 256,
                plane: gradient,
                params: scale256,
            },
            C5Fixture {
                name: "mostly_star_cell",
                width: 256,
                height: 256,
                plane: star_cell,
                params: scale256,
            },
            C5Fixture {
                name: "trailing_node_overshoot",
                width: 1026,
                height: 514,
                plane: trailing,
                params: scale256,
            },
            C5Fixture {
                name: "gradient_star_field_with_a_nan_and_an_inf",
                width: pw,
                height: ph,
                plane: pooled,
                params: scale256,
            },
            C5Fixture {
                name: "entirely_below_the_low_clip",
                width: 128,
                height: 128,
                plane: vec![0.0f32; 128 * 128],
                params: scale256,
            },
        ]
    }

    /// The two fixtures the M2 set does not contain: a NOISY sky at the
    /// production scale (1024 → stride 128), at the reference plane's
    /// `deviation_sigma` (3.0) and at a target plane's
    /// [`TARGET_DEVIATION_SIGMA`] (3.2). A flat or perfectly linear plane
    /// cannot tell a median of pixels from a median of 4×4 means; this is
    /// the only fixture where the question C5 asks has an answer.
    fn c5_noisy_fixtures() -> Vec<C5Fixture> {
        vec![
            C5Fixture {
                name: "noisy_sky_reference_sigma",
                width: 1024,
                height: 768,
                plane: noisy_sky(1024, 768, 0.10, 0.002, 900, 0xC5_0001),
                params: DEFAULT_PARAMS,
            },
            C5Fixture {
                name: "noisy_sky_target_sigma",
                width: 1024,
                height: 768,
                plane: noisy_sky(1024, 768, 0.14, 0.003, 900, 0xC5_0002),
                params: BackgroundParams {
                    deviation_sigma: TARGET_DEVIATION_SIGMA,
                    ..DEFAULT_PARAMS
                },
            },
        ]
    }

    /// The largest `|binned − oracle| / max(|oracle|, floor)` over a grid,
    /// with `floor` keeping a cell the oracle put at (or near) zero — the
    /// all-invalid fallback — from turning a rounding difference into an
    /// unbounded "relative" number.
    fn max_relative_deviation(got: &BackgroundGrid, oracle: &BackgroundGrid) -> (f64, usize) {
        const FLOOR: f64 = 1e-3; // the low clip is 4.5e-5; a real sky cell sits at 0.02..0.5
        let mut worst = 0f64;
        let mut worst_at = 0usize;
        for (idx, (&g, &o)) in got.cells.iter().zip(oracle.cells.iter()).enumerate() {
            let d = ((g as f64) - (o as f64)).abs() / (o as f64).abs().max(FLOOR);
            if d > worst {
                worst = d;
                worst_at = idx;
            }
        }
        (worst, worst_at)
    }

    /// `(max |relative|, mean signed relative, mean |relative|)`.
    fn relative_deviation_stats(got: &BackgroundGrid, oracle: &BackgroundGrid) -> (f64, f64, f64) {
        const FLOOR: f64 = 1e-3;
        let n = got.cells.len().max(1) as f64;
        let (mut worst, mut signed, mut absolute) = (0f64, 0f64, 0f64);
        for (&g, &o) in got.cells.iter().zip(oracle.cells.iter()) {
            let rel = ((g as f64) - (o as f64)) / (o as f64).abs().max(FLOOR);
            worst = worst.max(rel.abs());
            signed += rel;
            absolute += rel.abs();
        }
        (worst, signed / n, absolute / n)
    }

    fn assert_same_mesh_and_validity(got: &BackgroundGrid, oracle: &BackgroundGrid, name: &str) {
        assert_eq!(
            (got.gw, got.gh),
            (oracle.gw, oracle.gh),
            "{name}: the MESH is a cross-module contract (LnGrid/grid.rs/the integration \
             engine's row evaluator all index it, and it IS the .athln layout) — C5 must \
             not move it"
        );
        assert_eq!(
            got.invalid_cells, oracle.invalid_cells,
            "{name}: the binned reduction must flag the same number of cells invalid"
        );
    }

    /// Perf tier C item C5 (ruling C-5), the DELTA pin: the shipped
    /// [`background_grid`] models the background from a `LN_BIN`×`LN_BIN`
    /// reduction of the plane instead of the plane itself, so its node
    /// values are NOT bit-identical to the pre-C5 code's — they must stay
    /// within 1e-3 RELATIVE of it, with the same mesh and the same
    /// per-cell validity flags, on every M2 LN fixture. The oracle is
    /// [`unbinned_reference::background_grid_unbinned_reference`], a
    /// verbatim copy of the pre-C5 function.
    ///
    /// Measured at the time of writing (task 7's report has the table):
    /// `flat_plane_with_stars` 0, `vertical_gradient` 6.695e-4,
    /// `mostly_star_cell` 0, `trailing_node_overshoot` 0,
    /// `gradient_star_field_with_a_nan_and_an_inf` 1.474e-4,
    /// `entirely_below_the_low_clip` 0. The four zeros are not a sign that
    /// nothing happens — those planes are flat or saturated inside every
    /// cell, where a mean of 16 and a median of 16·N agree exactly — which
    /// is precisely why this pin also runs the noisy sky next door.
    ///
    /// The test asserts that at least one fixture MOVED: a tolerance a
    /// no-op satisfies proves nothing about the code it claims to cover.
    #[test]
    fn the_binned_background_tracks_the_unbinned_oracle_within_a_thousandth() {
        const TOLERANCE: f64 = 1e-3;
        let mut any_moved = false;
        for f in c5_m2_fixtures() {
            let oracle = unbinned_reference::background_grid_unbinned_reference(
                &f.plane, f.width, f.height, &f.params, None,
            );
            let got = background_grid(&f.plane, f.width, f.height, &f.params, None);
            assert_same_mesh_and_validity(&got, &oracle, f.name);

            let (worst, at) = max_relative_deviation(&got, &oracle);
            if worst > 0.0 {
                any_moved = true;
            }
            assert!(
                worst <= TOLERANCE,
                "{}: max relative deviation {worst:.3e} at cell {at} (binned {} vs oracle {}) \
                 exceeds {TOLERANCE:.0e}",
                f.name,
                got.cells[at],
                oracle.cells[at]
            );
        }
        assert!(
            any_moved,
            "no fixture's node values moved at all — `background_grid` is still the unbinned \
             path and this pin is vacuous"
        );
    }

    /// The same DELTA pin on a NOISY sky, with its own bar and its own
    /// reasoning (perf tier C item C5). On a plane that has a noise floor
    /// the two paths are two SAMPLE estimators over partly different pixel
    /// subsets, not two ways of computing one number: at the production
    /// stride the oracle subsamples its 128×128-px cell 2× ([`
    /// CELL_SUBSAMPLE_THRESHOLD`], 16 384 > 4 096) and medians 4 096
    /// pixels, while the binned path medians 1 024 bins built from ALL
    /// 16 384. Their difference therefore has a floor of roughly
    /// `0.017 · σ_noise / sky` per cell (1 σ) — 3.4e-4 at these fixtures'
    /// 2 % noise — with no defect anywhere; a grid of ~50 cells reaches
    /// ≈ 3 σ of that. Measured with ZERO stars in the field, i.e. pure
    /// estimator difference: 7.342e-4. With the fixtures' 900 faint stars:
    /// 7.808e-4 (reference σ) and 1.009e-3 (target σ).
    ///
    /// The bar is 2e-3 ≈ 6 σ of that floor — a real regression bar (a
    /// doubling of the star-flux bias, or any systematic shift, trips it)
    /// rather than a rubber stamp, and deliberately NOT the M2 fixtures'
    /// 1e-3, which measures a different thing: those planes carry no noise
    /// at all.
    #[test]
    fn the_binned_background_on_a_noisy_sky_stays_inside_the_estimator_difference() {
        const TOLERANCE: f64 = 2e-3;
        for f in c5_noisy_fixtures() {
            let oracle = unbinned_reference::background_grid_unbinned_reference(
                &f.plane, f.width, f.height, &f.params, None,
            );
            let got = background_grid(&f.plane, f.width, f.height, &f.params, None);
            assert_same_mesh_and_validity(&got, &oracle, f.name);

            let (worst, at) = max_relative_deviation(&got, &oracle);
            assert!(
                worst > 0.0,
                "{}: a noisy sky must not come out bit-identical — the binned path is not live",
                f.name
            );
            assert!(
                worst <= TOLERANCE,
                "{}: max relative deviation {worst:.3e} at cell {at} (binned {} vs oracle {}) \
                 exceeds {TOLERANCE:.0e}",
                f.name,
                got.cells[at],
                oracle.cells[at]
            );
        }
    }

    /// The threshold decision of perf tier C item C5, pinned with the
    /// measurement that made it. [`robust_cell_level`]'s
    /// `deviation_sigma`/[`TARGET_DEVIATION_SIGMA`] now clip BINNED values,
    /// whose noise is ≈ [`LN_BIN`]× lower than the pixels' — so either they
    /// stay as they are (the clip is relative to the sample's own MAD, so
    /// it is scale-free and needs no re-derivation) or they are widened by
    /// `LN_BIN` to preserve the pre-C5 ABSOLUTE bound. Measured against the
    /// unbinned oracle on the noisy skies, as-is is the closer of the two
    /// on all three statistics:
    ///
    /// | fixture | as-is max / bias | ×`LN_BIN` max / bias |
    /// | ------- | ---------------- | -------------------- |
    /// | reference σ | 7.808e-4 / +1.847e-4 | 9.479e-4 / +2.417e-4 |
    /// | target σ | 1.009e-3 / +1.797e-4 | 1.115e-3 / +2.167e-4 |
    ///
    /// The mechanism is visible in the sign: widening the bound re-admits
    /// the star wings the per-cell clip exists to remove, so every cell
    /// reads HIGH. So the sigmas stay at 3.0/3.2 — this test is what says
    /// so, and what a future "shouldn't these scale with the binning?"
    /// has to argue against.
    #[test]
    fn the_cell_deviation_sigmas_are_not_re_derived_for_the_binned_plane() {
        for f in c5_noisy_fixtures() {
            let oracle = unbinned_reference::background_grid_unbinned_reference(
                &f.plane, f.width, f.height, &f.params, None,
            );
            let as_is = background_grid(&f.plane, f.width, f.height, &f.params, None);
            let widened = background_grid(
                &f.plane,
                f.width,
                f.height,
                &BackgroundParams {
                    deviation_sigma: f.params.deviation_sigma * LN_BIN as f32,
                    ..f.params
                },
                None,
            );
            let (a_max, a_bias, a_abs) = relative_deviation_stats(&as_is, &oracle);
            let (w_max, w_bias, w_abs) = relative_deviation_stats(&widened, &oracle);
            assert!(
                a_max < w_max && a_bias.abs() < w_bias.abs() && a_abs < w_abs,
                "{}: widening the deviation sigma by LN_BIN was expected to be the WORSE \
                 approximation of the unbinned oracle on all three statistics — \
                 as-is (max {a_max:.3e}, bias {a_bias:+.3e}, mean|d| {a_abs:.3e}) vs \
                 widened (max {w_max:.3e}, bias {w_bias:+.3e}, mean|d| {w_abs:.3e})",
                f.name
            );
        }
    }

    /// The measured LIMIT of the C5 reduction, recorded as a
    /// characterisation pin rather than a quality bar (the project's own
    /// ecc-audit pattern): **the binned model's deviation from the
    /// unbinned one grows with how much star FLUX the field carries**,
    /// because binning smears a star's wings *below* the per-cell
    /// rejection bound instead of letting the clip remove them outright,
    /// while the unbinned path judges every pixel on its own.
    ///
    /// Measured (max relative deviation from the oracle, 1024×768 at the
    /// production scale): a faint-dominated field of 2 000 stars reads
    /// 1.077e-3, while a flux-saturated one — uniform peaks in
    /// `[0.02, 0.62]`, one in eleven saturated, a mean star peak of 3× the
    /// sky, which no real frame carries — reads 1.595e-3 at 900 stars and
    /// 3.333e-2 at 2 000, where it also puts `invalid_cells` at 36 against
    /// the oracle's 8.
    ///
    /// This test asserts the ORDERING, not those numbers: the
    /// flux-saturated field must deviate MORE than the faint one at the
    /// same star count, by a clear margin. If a future change removes the
    /// sensitivity, this fails and someone re-measures rather than
    /// discovering it on a real crowded field.
    #[test]
    fn the_binned_backgrounds_star_flux_bias_grows_with_crowding() {
        let (w, h) = (1024usize, 768usize);
        let measure = |uniform: bool, stars: usize| {
            let plane = noisy_sky_with(w, h, 0.10, 0.002, stars, 0xC5_0001, uniform);
            let oracle = unbinned_reference::background_grid_unbinned_reference(
                &plane,
                w,
                h,
                &DEFAULT_PARAMS,
                None,
            );
            let got = background_grid(&plane, w, h, &DEFAULT_PARAMS, None);
            (
                max_relative_deviation(&got, &oracle).0,
                got.invalid_cells,
                oracle.invalid_cells,
            )
        };

        let (faint, faint_inv, faint_oracle_inv) = measure(false, 900);
        let (loaded, loaded_inv, loaded_oracle_inv) = measure(true, 900);
        assert!(
            loaded > 1.5 * faint,
            "a flux-saturated field must deviate clearly more than a faint-dominated one at              the same star count: {loaded:.3e} (invalid {loaded_inv} vs oracle              {loaded_oracle_inv}) is not >1.5x {faint:.3e} (invalid {faint_inv} vs oracle              {faint_oracle_inv})"
        );
        // And the faint field — the realistic one — stays inside the noisy
        // fixtures' own bar even at more than twice their star count.
        let (dense_faint, _, _) = measure(false, 2000);
        assert!(
            dense_faint <= 2e-3,
            "a faint-dominated field at 2000 stars must stay inside the noisy fixtures' 2e-3:              {dense_faint:.3e}"
        );
    }

    /// Perf tier C item C5's edge-remainder ruling: `width % LN_BIN`
    /// columns and `height % LN_BIN` rows are DROPPED rather than averaged
    /// into a short bin (see [`clip_and_bin`]'s doc). A plane too thin to
    /// hold ONE whole bin on either axis therefore has nothing to measure,
    /// and joins the module's other degenerate inputs as all-invalid —
    /// which every caller already treats as "refuse" — instead of silently
    /// returning a zero surface that looks measured.
    #[test]
    fn a_plane_thinner_than_one_bin_is_fully_invalid() {
        for (w, h) in [(3usize, 64usize), (64, 3), (2, 2)] {
            let plane = vec![0.10f32; w * h];
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
            assert_eq!(
                g.invalid_cells,
                g.gw * g.gh,
                "{w}x{h}: a plane thinner than LN_BIN={LN_BIN} must report every cell invalid"
            );
            assert!(g.cells.iter().all(|&v| v == 0.0), "{w}x{h}: {:?}", g.cells);
        }
        // And one pixel MORE than a whole bin on both axes is measured
        // normally — the guard is the thin-plane case, not a size floor.
        let plane = vec![0.10f32; 5 * 5];
        let g = background_grid(
            &plane,
            5,
            5,
            &BackgroundParams {
                scale: 256,
                ..DEFAULT_PARAMS
            },
            None,
        );
        assert_eq!(g.invalid_cells, 0, "{:?}", g.cells);
        assert!(
            g.cells.iter().all(|&c| (c - 0.10).abs() < 1e-6),
            "{:?}",
            g.cells
        );
    }
}
