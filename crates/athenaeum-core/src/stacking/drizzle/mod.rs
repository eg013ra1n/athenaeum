//! Drizzle stage (M3, spec §7). `geom` is the pure geometry the stage
//! driver calls per source pixel: the reference ↔ output-grid coordinate
//! map, a source pixel's shrunk "drop" corners, forward-mapping those
//! corners subject → reference → output grid through a frame's `PixelMap`,
//! exact convex-quad ∩ unit-pixel clipping (rulings R-M3-1/R-M3-2), and the
//! 16×16 tabulated-kernel micro-drop table for the `circle`/`gaussian`
//! kernels (R-M3-3).
//!
//! [`drizzle_group`] (M3 Task 3) is the stage driver itself: per plane, per
//! included frame, it reads the whole calibrated plane once, and — for
//! every finite non-zero, non-rejected source sample — deposits its
//! (possibly locally-normalized) value onto the scaled output grid through
//! the configured kernel, banded over [`DRIZZLE_BAND_ROWS`] output rows in
//! parallel (ruling R-M3-6). `I / W` where `W > 0` is level-preserving
//! (ruling R-M3-2); `W / max(W)` is the weight map when
//! [`DrizzleInput::write_weight_map`] is on.

pub mod geom;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::geometry::PixelMap;
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::NormalizationPair;
use crate::integration::IntegrationError;
use crate::stacking::ln::grid::LnScratch;
use crate::stacking::ln::LnFrameGrids;
use crate::stacking::measure::{measure_plane, MeasureOptions};
use crate::stacking::rej::RejBitmap;

pub use crate::stacking::config::DrizzleKernel;

/// Output rows processed per parallel band (ruling R-M3-6). The last band
/// of a group is shorter when `out_height` is not a multiple of this.
pub const DRIZZLE_BAND_ROWS: usize = 512;

/// One included frame's plane data, weights and per-frame normalization —
/// everything [`drizzle_group`] needs to deposit it, already resolved by
/// the caller (`stacking::run`, a later task).
pub struct DrizzleFrame<'a> {
    /// The calibrated frame on disk (f32, 1 or 3 planes) — the SAME
    /// geometry as the group's reference (`DrizzleInput::width/height`);
    /// `drizzle_group` refuses a mismatch.
    pub path: &'a Path,
    /// Subject → reference mapping (registration v2's `PixelMap`).
    pub map: &'a PixelMap,
    /// Per-plane normalized weight; ignored (treated as `1.0`) when
    /// [`DrizzleInput::use_weights`] is `false`.
    pub weight: &'a [f64],
    /// Per-plane global OUTPUT normalization pair — the fallback ruling
    /// R-M3-5 uses whenever local normalization is off, unavailable for
    /// this run, or this frame has no LN grids.
    pub output_pair: &'a [NormalizationPair],
    /// Per-channel local-normalization grids in reference geometry, when
    /// LN ran for this frame (`None` otherwise — a frame with no sidecar
    /// always falls back to `output_pair`).
    pub ln: Option<&'a LnFrameGrids>,
    /// This frame's `.rej` rejection bitmap, when
    /// [`DrizzleInput::use_rejection`] is on and one was written for it.
    pub rej: Option<&'a Path>,
}

/// Everything [`drizzle_group`] needs for one group's drizzle pass.
pub struct DrizzleInput<'a> {
    /// Included frames only, in the engine's own order — no min-weight
    /// filtering happens here, the caller already did it.
    pub frames: &'a [DrizzleFrame<'a>],
    /// Reference geometry (the group's un-scaled width/height/channels —
    /// every frame in `frames` must match this exactly).
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    /// Output-grid multiplier (ruling R-M3-10: 1, 2 or 3 — not enforced
    /// here, the plan gate is the range check).
    pub scale: u32,
    pub drop_shrink: f64,
    pub kernel: DrizzleKernel,
    pub use_weights: bool,
    pub use_rejection: bool,
    pub use_local_normalization: bool,
    /// Whether to materialize the normalized `W / max(W)` weight-map plane
    /// in the output (`DrizzleOutput::weight`) — the raw `W` accumulator
    /// itself is always used for `coverage`, this only controls whether the
    /// normalized copy is kept.
    pub write_weight_map: bool,
    pub measure: &'a MeasureOptions,
    /// Injected total system RAM for the memory refusal (ruling R-M3-7,
    /// ruling R-M3-12): `None` probes
    /// [`crate::integration::band_budget::total_ram_bytes`] — the run
    /// always passes `None`; tests inject a small value to exercise the
    /// refusal without needing a huge geometry.
    pub ram_total_bytes: Option<u64>,
}

/// Per-group drizzle statistics — the `stacking_run_groups` / provenance
/// facing summary (a later task writes this into the run's persisted
/// record; `ts_export.rs` registration is that task's, not this one's).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DrizzleStats {
    pub scale: u32,
    pub out_width: usize,
    pub out_height: usize,
    /// Included frames this pass drizzled (whether or not every one
    /// actually contributed — a frame skipped for zero weight still counts,
    /// mirroring `DrizzleInput::frames`'s own length).
    pub frames: usize,
    pub kernel: DrizzleKernel,
    pub drop_shrink: f64,
    pub used_weights: bool,
    pub used_rejection: bool,
    /// Of `frames`, how many actually had their LN grids applied (`0` when
    /// local normalization was off for the run).
    pub ln_frames: usize,
    /// Per plane, measured on the drizzled (output-grid) planes.
    pub fwhm_px: Vec<f64>,
    pub eccentricity: Vec<f64>,
    pub noise: Vec<f64>,
    /// Per plane, the fraction of output pixels with `W > 0`.
    pub coverage: Vec<f64>,
    pub read_ms: u64,
    pub deposit_ms: u64,
    pub bytes_read: u64,
}

/// The drizzled master's pixel data (+ optional weight map) plus its stats.
#[derive(Debug)]
pub struct DrizzleOutput {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    /// `width * height * channels`, plane-major (channel 0's rows, then
    /// channel 1's, …) — the same layout `write_fits_f32` expects.
    pub data: Vec<f32>,
    /// Same layout as `data`, present only when
    /// [`DrizzleInput::write_weight_map`] was set.
    pub weight: Option<Vec<f32>>,
    pub stats: DrizzleStats,
}

/// Progress callback: `(done, total)` over `frames × planes` — ticked once
/// per included frame per plane, whether or not that frame actually
/// contributed (a zero-weight frame is still "done").
pub struct DrizzleProgress<'a> {
    pub on_frame: &'a (dyn Fn(usize, usize) + Sync),
}

/// Drizzle failure modes (never a panic — every fallible step in
/// [`drizzle_group`] returns one of these).
#[derive(Debug)]
pub enum DrizzleError {
    Cancelled,
    /// Ruling R-M3-7: `need` bytes estimated, `have` the refusal threshold
    /// (half of a probed total, or the 4 GiB floor when the total is
    /// unknown) — the caller (a later task) formats the user-facing
    /// message and leaves the group's `master_path` alone.
    Memory {
        need: u64,
        have: u64,
    },
    Io(String),
    BadInput(String),
}

impl std::fmt::Display for DrizzleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrizzleError::Cancelled => write!(f, "drizzle cancelled"),
            DrizzleError::Memory { need, have } => write!(
                f,
                "drizzle needs ≈{need} bytes but only {have} is available; use a smaller scale"
            ),
            DrizzleError::Io(msg) => write!(f, "drizzle io error: {msg}"),
            DrizzleError::BadInput(msg) => write!(f, "drizzle bad input: {msg}"),
        }
    }
}

impl std::error::Error for DrizzleError {}

impl From<IntegrationError> for DrizzleError {
    fn from(e: IntegrationError) -> Self {
        match e {
            IntegrationError::Cancelled => DrizzleError::Cancelled,
            IntegrationError::Io(io) => DrizzleError::Io(io.to_string()),
            IntegrationError::BadInput(m) => DrizzleError::BadInput(m),
            IntegrationError::Decode(m) => DrizzleError::BadInput(m),
        }
    }
}

/// Peak RAM [`drizzle_group`] needs (ruling R-M3-7): the `channels`
/// output-data planes plus the one `I`/`W` accumulator pair alive at a time
/// (`(channels + 2) * out_w * out_h * 4` bytes), one full-resolution source
/// plane in RAM (`width * height * 4`), and — when `ln` is set — two more
/// reference-geometry planes for the per-frame `A`/`B` grids
/// (`2 * width * height * 4`).
pub fn estimate_memory_bytes(
    width: usize,
    height: usize,
    channels: usize,
    scale: u32,
    ln: bool,
) -> u64 {
    let out_w = width as u64 * scale as u64;
    let out_h = height as u64 * scale as u64;
    let mut need = (channels as u64 + 2) * out_w * out_h * 4;
    need += width as u64 * height as u64 * 4;
    if ln {
        need += 2 * width as u64 * height as u64 * 4;
    }
    need
}

/// Everything [`deposit_band`] needs for one frame's deposit pass, bundled
/// so the parallel `for_each` closure captures one small `&FrameDepositCtx`
/// instead of a dozen loose variables.
struct FrameDepositCtx<'a> {
    src: &'a [f32],
    width: usize,
    height: usize,
    map: &'a PixelMap,
    scale: u32,
    drop_shrink: f64,
    kernel: DrizzleKernel,
    kernel_table: Option<&'a geom::KernelTable>,
    rej: Option<&'a RejBitmap>,
    /// `(a_plane, b_plane)`, each `width * height`, reference geometry —
    /// `Some` only when this frame's LN grid is actually driving output
    /// normalization this pass.
    ln: Option<(&'a [f32], &'a [f32])>,
    pair: NormalizationPair,
    w: f32,
    plane: usize,
}

/// Per plane, per included frame, banded forward deposition (rulings
/// R-M3-1..R-M3-7): allocates the `I`/`W` output-geometry accumulators,
/// reads each frame's plane once, resolves its rejection bitmap and/or LN
/// grids, and deposits every source pixel's drop through
/// [`deposit_band`] — banded over [`DRIZZLE_BAND_ROWS`] output rows in
/// parallel on `pool`. Refuses up front (ruling R-M3-7) before any
/// output-geometry allocation happens.
pub fn drizzle_group(
    input: &DrizzleInput<'_>,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &DrizzleProgress<'_>,
) -> Result<DrizzleOutput, DrizzleError> {
    if input.channels == 0 {
        return Err(DrizzleError::BadInput("channels must be > 0".to_string()));
    }
    if input.scale == 0 {
        return Err(DrizzleError::BadInput("scale must be >= 1".to_string()));
    }
    if input.width == 0 || input.height == 0 {
        return Err(DrizzleError::BadInput(format!(
            "reference geometry {}x{} is empty",
            input.width, input.height
        )));
    }

    let group_start = Instant::now();
    let out_w = input.width * input.scale as usize;
    let out_h = input.height * input.scale as usize;
    let plane_out_pixels = out_w * out_h;

    // Ruling R-M3-7: refused BEFORE any output-geometry allocation.
    let need = estimate_memory_bytes(
        input.width,
        input.height,
        input.channels,
        input.scale,
        input.use_local_normalization,
    );
    let total = input
        .ram_total_bytes
        .or_else(crate::integration::band_budget::total_ram_bytes);
    let have = match total {
        Some(t) => t / 2,
        // Unknown total (ruling R-M3-7): refuse above a flat 4 GiB.
        None => 4 * 1024 * 1024 * 1024,
    };
    if need > have {
        return Err(DrizzleError::Memory { need, have });
    }

    // Built once for the whole group — `circle`/`gaussian` only, `None`
    // for `square` (which uses `geom::clip_area` directly).
    let kernel_table = geom::kernel_table(input.kernel, input.drop_shrink);

    let total_units = input.frames.len() * input.channels;
    let mut done_units = 0usize;

    let mut data = vec![0f32; plane_out_pixels * input.channels];
    let mut weight_all = input
        .write_weight_map
        .then(|| vec![0f32; plane_out_pixels * input.channels]);

    let mut fwhm_px = Vec::with_capacity(input.channels);
    let mut eccentricity = Vec::with_capacity(input.channels);
    let mut noise = Vec::with_capacity(input.channels);
    let mut coverage = Vec::with_capacity(input.channels);

    let mut read_ms_total = 0u64;
    let mut deposit_ms_total = 0u64;
    let mut bytes_read_total = 0u64;

    // Reused across every (frame, plane) pair that needs local
    // normalization — one `read_plane`, at most one LN evaluation per
    // frame per plane (perf shape, R-M3-6).
    let plane_pixels = input.width * input.height;
    let mut a_plane_buf = vec![0f32; plane_pixels];
    let mut b_plane_buf = vec![0f32; plane_pixels];

    for c in 0..input.channels {
        let plane_start = Instant::now();
        let mut i_buf = vec![0f32; plane_out_pixels];
        let mut w_buf = vec![0f32; plane_out_pixels];
        let mut plane_bytes_read = 0u64;

        for frame in input.frames {
            // Cancel checked once per frame (perf shape) — the band loop
            // itself never checks it.
            if cancel.load(Ordering::Relaxed) {
                return Err(DrizzleError::Cancelled);
            }

            let w = if input.use_weights {
                frame.weight.get(c).copied().unwrap_or(0.0) as f32
            } else {
                1.0
            };
            if w <= 0.0 {
                done_units += 1;
                (progress.on_frame)(done_units, total_units);
                continue;
            }

            let read_start = Instant::now();
            let reader = PlaneReader::open(frame.path)?;
            if reader.channels() != input.channels
                || reader.width() != input.width
                || reader.height() != input.height
            {
                return Err(DrizzleError::BadInput(format!(
                    "{}: geometry {}x{}x{} != group geometry {}x{}x{}",
                    frame.path.display(),
                    reader.width(),
                    reader.height(),
                    reader.channels(),
                    input.width,
                    input.height,
                    input.channels
                )));
            }
            let src = reader.read_plane(c)?;
            let read_bytes = (plane_pixels * 4) as u64;
            plane_bytes_read += read_bytes;
            bytes_read_total += read_bytes;
            read_ms_total += read_start.elapsed().as_millis() as u64;

            let rej_bitmap = if input.use_rejection {
                match frame.rej {
                    Some(path) => Some(
                        RejBitmap::read(path, input.width, input.height, input.channels)
                            .map_err(|e| DrizzleError::Io(e.to_string()))?,
                    ),
                    None => None,
                }
            } else {
                None
            };

            let has_ln = input.use_local_normalization && frame.ln.is_some();
            if has_ln {
                let grids = frame.ln.expect("has_ln implies Some");
                let grid = grids.channels.get(c).ok_or_else(|| {
                    DrizzleError::BadInput(format!(
                        "{}: local-normalization grids have no channel {c}",
                        frame.path.display()
                    ))
                })?;
                if grid.ref_width != input.width || grid.ref_height != input.height {
                    return Err(DrizzleError::BadInput(format!(
                        "{}: local-normalization grid geometry {}x{} != group geometry {}x{}",
                        frame.path.display(),
                        grid.ref_width,
                        grid.ref_height,
                        input.width,
                        input.height
                    )));
                }
                let mut scratch = LnScratch::for_grid(grid);
                for y in 0..input.height {
                    let start = y * input.width;
                    let end = start + input.width;
                    grid.evaluate_row_into(
                        y,
                        &mut a_plane_buf[start..end],
                        &mut b_plane_buf[start..end],
                        &mut scratch,
                    );
                }
            }

            let pair = frame
                .output_pair
                .get(c)
                .copied()
                .unwrap_or(NormalizationPair::IDENTITY);

            let ctx = FrameDepositCtx {
                src: &src,
                width: input.width,
                height: input.height,
                map: frame.map,
                scale: input.scale,
                drop_shrink: input.drop_shrink,
                kernel: input.kernel,
                kernel_table: kernel_table.as_ref(),
                rej: rej_bitmap.as_ref(),
                ln: has_ln.then(|| (&a_plane_buf[..], &b_plane_buf[..])),
                pair,
                w,
                plane: c,
            };

            let deposit_start = Instant::now();
            pool.install(|| {
                i_buf
                    .par_chunks_mut(DRIZZLE_BAND_ROWS * out_w)
                    .zip(w_buf.par_chunks_mut(DRIZZLE_BAND_ROWS * out_w))
                    .enumerate()
                    .for_each(|(band_idx, (ib, wb))| {
                        deposit_band(ib, wb, band_idx, out_w, &ctx);
                    });
            });
            deposit_ms_total += deposit_start.elapsed().as_millis() as u64;

            done_units += 1;
            (progress.on_frame)(done_units, total_units);
        }

        for idx in 0..plane_out_pixels {
            data[c * plane_out_pixels + idx] = if w_buf[idx] > 0.0 {
                i_buf[idx] / w_buf[idx]
            } else {
                0.0
            };
        }
        let max_w = w_buf.iter().cloned().fold(0f32, f32::max);
        if let Some(weight_all) = weight_all.as_mut() {
            for idx in 0..plane_out_pixels {
                weight_all[c * plane_out_pixels + idx] =
                    if max_w > 0.0 { w_buf[idx] / max_w } else { 0.0 };
            }
        }
        let covered = w_buf.iter().filter(|&&v| v > 0.0).count();
        coverage.push(covered as f64 / plane_out_pixels as f64);

        let plane_data = &data[c * plane_out_pixels..(c + 1) * plane_out_pixels];
        let cm = pool.install(|| measure_plane(plane_data, out_w, out_h, input.measure, None));
        fwhm_px.push(cm.fwhm_px);
        eccentricity.push(cm.eccentricity);
        noise.push(cm.noise);

        debug!(
            plane = c,
            frames = input.frames.len(),
            duration_ms = plane_start.elapsed().as_millis() as u64,
            bytes = plane_bytes_read,
            "drizzle plane deposited"
        );
    }

    let ln_frames = if input.use_local_normalization {
        input.frames.iter().filter(|f| f.ln.is_some()).count()
    } else {
        0
    };

    let stats = DrizzleStats {
        scale: input.scale,
        out_width: out_w,
        out_height: out_h,
        frames: input.frames.len(),
        kernel: input.kernel,
        drop_shrink: input.drop_shrink,
        used_weights: input.use_weights,
        used_rejection: input.use_rejection,
        ln_frames,
        fwhm_px,
        eccentricity,
        noise,
        coverage,
        read_ms: read_ms_total,
        deposit_ms: deposit_ms_total,
        bytes_read: bytes_read_total,
    };

    info!(
        out_width = out_w,
        out_height = out_h,
        drizzle_scale = input.scale,
        frames = input.frames.len(),
        duration_ms = group_start.elapsed().as_millis() as u64,
        "drizzle group finished"
    );

    Ok(DrizzleOutput {
        width: out_w,
        height: out_h,
        channels: input.channels,
        data,
        weight: weight_all,
        stats,
    })
}

/// The bounding box, in SOURCE pixel indices (inclusive, clamped to
/// `[0, width) x [0, height)`), a band of output rows can possibly touch
/// (ruling R-M3-6): the band rect's four corners plus one sample every 32
/// output px along its four edges, mapped output → reference
/// ([`geom::to_reference`]) → subject (`map.inverse`), grown by
/// `drop_shrink / 2 + 1` on every side. A non-finite map (or an empty
/// sample set) falls back to the WHOLE source plane — correctness over
/// speed on that degenerate path.
#[allow(clippy::too_many_arguments)]
fn band_source_window(
    map: &PixelMap,
    out_w: usize,
    y0: usize,
    rows: usize,
    scale: u32,
    width: usize,
    height: usize,
    drop_shrink: f64,
) -> (usize, usize, usize, usize) {
    const STEP: f64 = 32.0;
    let x_lo = -0.5_f64;
    let x_hi = out_w as f64 - 0.5;
    let y_lo = y0 as f64 - 0.5;
    let y_hi = (y0 + rows) as f64 - 0.5;

    let mut sx_lo = f64::INFINITY;
    let mut sx_hi = f64::NEG_INFINITY;
    let mut sy_lo = f64::INFINITY;
    let mut sy_hi = f64::NEG_INFINITY;
    let mut visit = |ox: f64, oy: f64| {
        let rx = geom::to_reference(ox, scale);
        let ry = geom::to_reference(oy, scale);
        let (sx, sy) = map.inverse(rx, ry);
        if sx.is_finite() && sy.is_finite() {
            sx_lo = sx_lo.min(sx);
            sx_hi = sx_hi.max(sx);
            sy_lo = sy_lo.min(sy);
            sy_hi = sy_hi.max(sy);
        }
    };

    let mut x = x_lo;
    while x < x_hi {
        visit(x, y_lo);
        visit(x, y_hi);
        x += STEP;
    }
    visit(x_hi, y_lo);
    visit(x_hi, y_hi);
    let mut y = y_lo;
    while y < y_hi {
        visit(x_lo, y);
        visit(x_hi, y);
        y += STEP;
    }
    visit(x_lo, y_hi);
    visit(x_hi, y_hi);

    if !sx_lo.is_finite() || !sy_lo.is_finite() {
        return (0, 0, width.saturating_sub(1), height.saturating_sub(1));
    }
    let margin = drop_shrink / 2.0 + 1.0;
    let x0 = (sx_lo - margin).floor().max(0.0) as usize;
    let x1 = (sx_hi + margin).ceil().min(width as f64 - 1.0).max(0.0) as usize;
    let y0s = (sy_lo - margin).floor().max(0.0) as usize;
    let y1 = (sy_hi + margin).ceil().min(height as f64 - 1.0).max(0.0) as usize;
    (x0, y0s, x1, y1)
}

/// Deposits one frame's plane into one band of the `I`/`W` accumulators
/// (rulings R-M3-1..R-M3-5): scans only the source-pixel window
/// [`band_source_window`] bounds, and for each finite, non-zero,
/// non-rejected source sample, spreads its normalized value onto the
/// output grid through the configured kernel. No allocation — `ib`/`wb`
/// are this band's own slice of the caller's accumulators.
fn deposit_band(
    ib: &mut [f32],
    wb: &mut [f32],
    band_idx: usize,
    out_w: usize,
    ctx: &FrameDepositCtx<'_>,
) {
    let y0 = band_idx * DRIZZLE_BAND_ROWS;
    let rows = ib.len() / out_w;
    if rows == 0 {
        return;
    }
    let y1 = y0 + rows;

    let (sx0, sy0, sx1, sy1) = band_source_window(
        ctx.map,
        out_w,
        y0,
        rows,
        ctx.scale,
        ctx.width,
        ctx.height,
        ctx.drop_shrink,
    );
    // Ruling R-M3-3's tabulated kernel weights sum to `drop_shrink²` in
    // SOURCE-pixel units (see `geom::kernel_table`'s own doc); scaled by
    // `scale²` here so `circle`/`gaussian` deposit the same mass per drop
    // as `square`'s exact clipping does in OUTPUT-pixel units.
    let scale_sq = ctx.scale as f64 * ctx.scale as f64;

    for y in sy0..=sy1 {
        let row_off = y * ctx.width;
        for x in sx0..=sx1 {
            let d = ctx.src[row_off + x];
            if !d.is_finite() || d == 0.0 {
                continue;
            }
            let (u, v) = ctx.map.forward(x as f64, y as f64);
            if !u.is_finite() || !v.is_finite() {
                continue;
            }

            if let Some(rb) = ctx.rej {
                // Ruling R-M3-4: out-of-range → not rejected.
                let ix = u.round();
                let iy = v.round();
                let rejected = ix >= 0.0
                    && iy >= 0.0
                    && (ix as usize) < ctx.width
                    && (iy as usize) < ctx.height
                    && rb.is_rejected(ctx.plane, ix as usize, iy as usize);
                if rejected {
                    continue;
                }
            }

            let nd = if let Some((a_plane, b_plane)) = ctx.ln {
                // Ruling R-M3-5: clamped (not skipped) to the reference
                // extent.
                let ix = (u.round() as i64).clamp(0, ctx.width as i64 - 1) as usize;
                let iy = (v.round() as i64).clamp(0, ctx.height as i64 - 1) as usize;
                let idx = iy * ctx.width + ix;
                a_plane[idx] * d + b_plane[idx]
            } else {
                ctx.pair.apply(d)
            };

            match ctx.kernel {
                DrizzleKernel::Square => {
                    let corners = geom::drop_corners(x, y, ctx.drop_shrink);
                    if let Some((quad, bbox)) = geom::map_drop(ctx.map, &corners, ctx.scale) {
                        let (bx0, by0, bx1, by1) = bbox;
                        let px0 = bx0.max(0);
                        let px1 = bx1.min(out_w as i64 - 1);
                        let py0 = by0.max(y0 as i64);
                        let py1 = by1.min(y1 as i64 - 1);
                        for py in py0..=py1 {
                            for px in px0..=px1 {
                                let a = geom::clip_area(&quad, px, py);
                                if a > 0.0 {
                                    let aw = (a * ctx.w as f64) as f32;
                                    let idx = (py - y0 as i64) as usize * out_w + px as usize;
                                    ib[idx] += aw * nd;
                                    wb[idx] += aw;
                                }
                            }
                        }
                    }
                }
                DrizzleKernel::Circle | DrizzleKernel::Gaussian => {
                    let table = ctx
                        .kernel_table
                        .expect("circle/gaussian kernels always build a table");
                    for (&(dx, dy), &wt) in table.offsets.iter().zip(table.weights.iter()) {
                        if wt <= 0.0 {
                            continue;
                        }
                        let (u2, v2) = ctx.map.forward(x as f64 + dx, y as f64 + dy);
                        if !u2.is_finite() || !v2.is_finite() {
                            continue;
                        }
                        let ox = geom::to_output(u2, ctx.scale);
                        let oy = geom::to_output(v2, ctx.scale);
                        if !ox.is_finite() || !oy.is_finite() {
                            continue;
                        }
                        let px = ox.round() as i64;
                        let py = oy.round() as i64;
                        if px < 0 || px as usize >= out_w || py < y0 as i64 || py >= y1 as i64 {
                            continue;
                        }
                        let aw = (wt * scale_sq * ctx.w as f64) as f32;
                        let idx = (py - y0 as i64) as usize * out_w + px as usize;
                        ib[idx] += aw * nd;
                        wb[idx] += aw;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::{Linear, LinearKind};
    use crate::integration::source::RejectionBitSink;
    use crate::stacking::ln::grid::LnGrid;
    use crate::stacking::rej::RejBitmapSet;
    use crate::test_support::gaussian_field;

    const W: usize = 64;
    const H: usize = 48;

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
    }

    fn no_progress() -> DrizzleProgress<'static> {
        DrizzleProgress {
            on_frame: &|_, _| {},
        }
    }

    fn identity_map() -> PixelMap {
        PixelMap::linear(Linear::identity()).unwrap()
    }

    fn translation_map(dx: f64, dy: f64) -> PixelMap {
        PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]],
        })
        .unwrap()
    }

    /// A rotation about the frame's own centre `(cx, cy)` — a subject
    /// star at the centre still forward-maps to the centre.
    fn rotation_about_centre(deg: f64, cx: f64, cy: f64) -> PixelMap {
        let r = deg.to_radians();
        let (s, c) = r.sin_cos();
        let tx = cx - (c * cx - s * cy);
        let ty = cy - (s * cx + c * cy);
        PixelMap::linear(Linear {
            kind: LinearKind::Affine,
            m: [[c, -s, tx], [s, c, ty], [0.0, 0.0, 1.0]],
        })
        .unwrap()
    }

    fn write_mono(dir: &std::path::Path, name: &str, w: usize, h: usize, data: &[f32]) -> PathBuf {
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, data, &[]).unwrap();
        p
    }

    fn uniform(w: usize, h: usize, value: f32) -> Vec<f32> {
        vec![value; w * h]
    }

    fn identity_pair() -> [NormalizationPair; 1] {
        [NormalizationPair::IDENTITY]
    }

    fn frame<'a>(
        path: &'a std::path::Path,
        map: &'a PixelMap,
        weight: &'a [f64],
        pair: &'a [NormalizationPair],
    ) -> DrizzleFrame<'a> {
        DrizzleFrame {
            path,
            map,
            weight,
            output_pair: pair,
            ln: None,
            rej: None,
        }
    }

    fn base_input<'a>(
        frames: &'a [DrizzleFrame<'a>],
        measure: &'a MeasureOptions,
    ) -> DrizzleInput<'a> {
        DrizzleInput {
            frames,
            width: W,
            height: H,
            channels: 1,
            scale: 2,
            drop_shrink: 0.9,
            kernel: DrizzleKernel::Square,
            use_weights: true,
            use_rejection: false,
            use_local_normalization: false,
            write_weight_map: true,
            measure,
            ram_total_bytes: None,
        }
    }

    // ── (a) level: a uniform field comes out at its own level, with full
    // coverage, for every kernel and every (scale, drop_shrink) combo. ──

    fn run_level_case(scale: u32, drop_shrink: f64, kernel: DrizzleKernel) {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.25);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &data);
        let p2 = write_mono(dir.path(), "f2.fits", W, H, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [
            frame(&p0, &map, &weight, &pair),
            frame(&p1, &map, &weight, &pair),
            frame(&p2, &map, &weight, &pair),
        ];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.scale = scale;
        input.drop_shrink = drop_shrink;
        input.kernel = kernel;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();

        let out_w = W * scale as usize;
        let out_h = H * scale as usize;
        let margin = 4usize;
        for y in margin..out_h - margin {
            for x in margin..out_w - margin {
                let v = out.data[y * out_w + x];
                assert!(
                    (v - 0.25).abs() < 1e-6,
                    "kernel={kernel:?} scale={scale} drop_shrink={drop_shrink} x={x} y={y} v={v}"
                );
            }
        }
        let weight = out.weight.as_ref().expect("write_weight_map was on");
        let max_w = weight.iter().cloned().fold(0f32, f32::max);
        assert!((max_w - 1.0).abs() < 1e-6, "max_w={max_w}");
        assert!(
            (out.stats.coverage[0] - 1.0).abs() < 1e-9,
            "kernel={kernel:?} scale={scale} drop_shrink={drop_shrink} coverage={}",
            out.stats.coverage[0]
        );
    }

    #[test]
    fn level_preserves_a_uniform_field_square_kernel() {
        run_level_case(2, 0.9, DrizzleKernel::Square);
        run_level_case(1, 0.9, DrizzleKernel::Square);
        run_level_case(3, 0.9, DrizzleKernel::Square);
        run_level_case(2, 0.5, DrizzleKernel::Square);
    }

    #[test]
    fn level_preserves_a_uniform_field_circle_kernel() {
        run_level_case(2, 0.9, DrizzleKernel::Circle);
    }

    #[test]
    fn level_preserves_a_uniform_field_gaussian_kernel() {
        run_level_case(2, 0.9, DrizzleKernel::Gaussian);
    }

    // ── (b) spread: a single source pixel's drop lands on exactly the
    // four output pixels it should, zero samples are skipped. ──

    #[test]
    fn spread_single_pixel_lands_on_four_output_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = vec![0f32; W * H];
        data[10 * W + 10] = 1.0;
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [frame(&p0, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.scale = 2;
        input.drop_shrink = 1.0;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = W * 2;
        for &(x, y) in &[(20usize, 20usize), (21, 20), (20, 21), (21, 21)] {
            let v = out.data[y * out_w + x];
            assert!((v - 1.0).abs() < 1e-6, "x={x} y={y} v={v}");
        }
        for &(x, y) in &[(19usize, 20usize), (22, 20), (20, 19), (20, 22), (19, 19)] {
            let v = out.data[y * out_w + x];
            assert_eq!(v, 0.0, "x={x} y={y} v={v}");
        }
    }

    // ── (c) weights ──

    #[test]
    fn weights_gate_a_frames_contribution() {
        let dir = tempfile::tempdir().unwrap();
        let d0 = uniform(W, H, 0.2);
        let d1 = uniform(W, H, 0.6);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &d0);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &d1);
        let map = identity_map();
        let w0 = [1.0f64];
        let w1 = [0.0f64];
        let pair = identity_pair();
        let frames = [frame(&p0, &map, &w0, &pair), frame(&p1, &map, &w1, &pair)];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.use_weights = true;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = W * input.scale as usize;
        let out_h = H * input.scale as usize;
        let v = out.data[(out_h / 2) * out_w + out_w / 2];
        assert!((v - 0.2).abs() < 1e-6, "use_weights=true v={v}");

        input.use_weights = false;
        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let v = out.data[(out_h / 2) * out_w + out_w / 2];
        assert!((v - 0.4).abs() < 1e-6, "use_weights=false v={v}");
    }

    // ── (d) rejection ──

    #[test]
    fn rejection_drops_a_fully_masked_frame() {
        let dir = tempfile::tempdir().unwrap();
        let d0 = uniform(W, H, 0.2);
        let d1 = uniform(W, H, 0.6);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &d0);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &d1);

        let stems = vec!["f0".to_string(), "f1".to_string()];
        let rej_dir = dir.path().join("rej");
        let set = RejBitmapSet::create(&rej_dir, &stems, W, H, 1).unwrap();
        // Frame 1 ("f1", value 0.6) gets its whole plane rejected; frame
        // 0's slot in the (rows, frames, words) buffer stays zero.
        // `RejBitmap::is_rejected` never reads a column past `width`, so
        // setting the padding bits too (there are none here: `W` is a
        // multiple of 64) is harmless either way.
        let words = W.div_ceil(64);
        let mut band = vec![0u64; H * 2 * words];
        for row in 0..H {
            for wi in 0..words {
                band[(row * 2 + 1) * words + wi] = u64::MAX;
            }
        }
        set.plane_sink(0).record_band(0, H, &band).unwrap();

        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let f0 = DrizzleFrame {
            path: &p0,
            map: &map,
            weight: &weight,
            output_pair: &pair,
            ln: None,
            rej: None,
        };
        let rej1 = set.path(1).to_path_buf();
        let f1 = DrizzleFrame {
            path: &p1,
            map: &map,
            weight: &weight,
            output_pair: &pair,
            ln: None,
            rej: Some(&rej1),
        };
        let frames = [f0, f1];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.use_rejection = true;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = W * input.scale as usize;
        let out_h = H * input.scale as usize;
        let v = out.data[(out_h / 2) * out_w + out_w / 2];
        assert!((v - 0.2).abs() < 1e-6, "use_rejection=true v={v}");

        input.use_rejection = false;
        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let v = out.data[(out_h / 2) * out_w + out_w / 2];
        assert!((v - 0.4).abs() < 1e-6, "use_rejection=false v={v}");
    }

    // ── (e) sharpening: sub-pixel-dithered drizzle with a small drop
    // recovers a sharper (lower FWHM) star than the coarse drop_shrink=1.0
    // "shift-and-add" case, on the same output grid. ──

    fn dithered_fwhm(drop_shrink: f64) -> f64 {
        let dir = tempfile::tempdir().unwrap();
        let sigma = 0.7f64;
        let mut paths = Vec::new();
        let mut maps = Vec::new();
        for j in 0..3usize {
            for i in 0..3usize {
                let cx = 20.0 + i as f64 / 3.0;
                let cy = 20.0 + j as f64 / 3.0;
                let data = gaussian_field(W, H, &[(cx, cy, 0.5)], sigma, 0.01);
                let name = format!("f{i}{j}.fits");
                paths.push(write_mono(dir.path(), &name, W, H, &data));
                maps.push(translation_map(-(i as f64) / 3.0, -(j as f64) / 3.0));
            }
        }
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames: Vec<DrizzleFrame<'_>> = paths
            .iter()
            .zip(maps.iter())
            .map(|(p, m)| frame(p, m, &weight, &pair))
            .collect();
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.scale = 2;
        input.drop_shrink = drop_shrink;
        input.write_weight_map = false;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        out.stats.fwhm_px[0]
    }

    #[test]
    fn sharpening_recovers_sub_pixel_dither() {
        let fwhm_shift_and_add = dithered_fwhm(1.0);
        let fwhm_drizzled = dithered_fwhm(0.6);
        eprintln!(
            "sharpening test: fwhm_drizzled={fwhm_drizzled} fwhm_shift_and_add={fwhm_shift_and_add} \
             fwhm_drizzled/2={} ratio_to_0.95x_threshold={}",
            fwhm_drizzled / 2.0,
            (fwhm_drizzled / 2.0) / (0.95 * fwhm_shift_and_add)
        );
        assert!(
            fwhm_drizzled / 2.0 < 0.95 * fwhm_shift_and_add,
            "fwhm_drizzled/2={} fwhm_shift_and_add={}",
            fwhm_drizzled / 2.0,
            fwhm_shift_and_add
        );
    }

    // ── (f) LN ──

    #[test]
    fn local_normalization_applies_the_frames_grid() {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.2);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = [NormalizationPair {
            scale: 2.0,
            offset: 0.1,
        }];
        let grid = LnGrid::constant(W, H, 1024, 2.0, 0.1);
        let grids = LnFrameGrids {
            channels: vec![grid],
        };

        let f_ln = DrizzleFrame {
            path: &p0,
            map: &map,
            weight: &weight,
            output_pair: &pair,
            ln: Some(&grids),
            rej: None,
        };
        let frames = [f_ln];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.use_local_normalization = true;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = W * input.scale as usize;
        let out_h = H * input.scale as usize;
        let v = out.data[(out_h / 2) * out_w + out_w / 2];
        assert!((v - 0.5).abs() < 1e-6, "LN on: v={v}");
        assert_eq!(out.stats.ln_frames, 1);

        input.use_local_normalization = false;
        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let v = out.data[(out_h / 2) * out_w + out_w / 2];
        assert!(
            (v - 0.5).abs() < 1e-6,
            "LN off, output_pair fallback: v={v}"
        );
        assert_eq!(out.stats.ln_frames, 0);
    }

    // ── (g) rotation conservation ──

    #[test]
    fn rotation_conserves_level_on_interior_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.3);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let cx = (W as f64 - 1.0) / 2.0;
        let cy = (H as f64 - 1.0) / 2.0;
        let map = rotation_about_centre(10.0, cx, cy);
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [frame(&p0, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.scale = 2;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let weight_map = out.weight.as_ref().unwrap();
        let max_w = weight_map.iter().cloned().fold(0f32, f32::max);
        let out_w = W * input.scale as usize;
        let out_h = H * input.scale as usize;
        let mut checked = 0usize;
        for y in 0..out_h {
            for x in 0..out_w {
                let idx = y * out_w + x;
                if weight_map[idx] >= 0.99 * max_w {
                    let v = out.data[idx];
                    assert!((v - 0.3).abs() < 1e-4, "x={x} y={y} v={v}");
                    checked += 1;
                }
            }
        }
        // The brief's own criterion (W >= 0.99*max(W)) doesn't promise a
        // specific count — a 10 deg rotation skews how many pixels land at
        // (near-)full area — this guard only rules out a vacuous pass
        // (zero pixels actually exercised).
        assert!(checked > 500, "checked={checked}");
    }

    // ── (h) cancel ──

    #[test]
    fn cancel_before_the_second_frame_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.2);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [
            frame(&p0, &map, &weight, &pair),
            frame(&p1, &map, &weight, &pair),
        ];
        let measure = MeasureOptions::default();
        let input = base_input(&frames, &measure);

        // The cancel flag is checked once per frame, before that frame's
        // own read/deposit work starts (perf shape) — flipping it from
        // frame 0's own progress tick reliably trips the check at the top
        // of frame 1's turn, without needing a second thread.
        let cancel = AtomicBool::new(false);
        let progress = DrizzleProgress {
            on_frame: &|done, _total| {
                if done == 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        };
        let err = drizzle_group(&input, &pool(), &cancel, &progress).unwrap_err();
        assert!(matches!(err, DrizzleError::Cancelled), "{err:?}");
    }

    // ── (i) memory ──

    #[test]
    fn estimate_memory_bytes_matches_the_r_m3_7_formula() {
        let (width, height, channels, scale) = (6224usize, 4168usize, 3usize, 3u32);
        let out_w = width as u64 * scale as u64;
        let out_h = height as u64 * scale as u64;
        let expected = (channels as u64 + 2) * out_w * out_h * 4
            + width as u64 * height as u64 * 4
            + 2 * width as u64 * height as u64 * 4;
        assert_eq!(
            estimate_memory_bytes(width, height, channels, scale, true),
            expected
        );

        let expected_no_ln =
            (channels as u64 + 2) * out_w * out_h * 4 + width as u64 * height as u64 * 4;
        assert_eq!(
            estimate_memory_bytes(width, height, channels, scale, false),
            expected_no_ln
        );
    }

    #[test]
    fn drizzle_group_refuses_when_the_estimate_exceeds_the_injected_total() {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(8, 8, 0.2);
        let p0 = write_mono(dir.path(), "f0.fits", 8, 8, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [frame(&p0, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let mut input = DrizzleInput {
            frames: &frames,
            width: 8,
            height: 8,
            channels: 1,
            scale: 1,
            drop_shrink: 0.9,
            kernel: DrizzleKernel::Square,
            use_weights: true,
            use_rejection: false,
            use_local_normalization: false,
            write_weight_map: false,
            measure: &measure,
            ram_total_bytes: Some(1024),
        };
        let err =
            drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap_err();
        assert!(matches!(err, DrizzleError::Memory { .. }), "{err:?}");
        input.ram_total_bytes = None;
        // Sanity: without the tiny injected total this tiny geometry must
        // not be refused (guards against a formula that always refuses).
        let ok = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress());
        assert!(ok.is_ok(), "{ok:?}");
    }

    // ── (j) band seam ──

    #[test]
    fn band_seam_leaves_no_row_off_level() {
        let dir = tempfile::tempdir().unwrap();
        let width = 40usize;
        let height = 818usize; // out_h = 818 * 2 = 1636 = 3*512 + 100
        let data = uniform(width, height, 0.4);
        let p0 = write_mono(dir.path(), "f0.fits", width, height, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [frame(&p0, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let input = DrizzleInput {
            frames: &frames,
            width,
            height,
            channels: 1,
            scale: 2,
            drop_shrink: 0.9,
            kernel: DrizzleKernel::Square,
            use_weights: true,
            use_rejection: false,
            use_local_normalization: false,
            write_weight_map: false,
            measure: &measure,
            ram_total_bytes: None,
        };

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = width * 2;
        let out_h = height * 2;
        assert_eq!(out_h, 1636);
        for &seam in &[511usize, 512, 1023, 1024, 1535, 1536] {
            assert!(seam < out_h);
        }
        let margin = 4usize;
        for y in margin..out_h - margin {
            for x in margin..out_w - margin {
                let v = out.data[y * out_w + x];
                assert!((v - 0.4).abs() < 1e-6, "y={y} x={x} v={v}");
            }
        }
    }
}
