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
    /// The calibrated frame on disk (f32, 1 or 3 planes) — its OWN native
    /// geometry, read from the file itself (B1, M3 final fix wave): since
    /// the 2026-09-10 owner decision grouping is camera-agnostic, a group's
    /// members may differ in native geometry from the run's reference (and
    /// from each other) — `drizzle_group` only requires `channels` to
    /// match `DrizzleInput::channels`; `map` (subject → reference) carries
    /// every frame correctly onto the shared output grid regardless of its
    /// own width/height.
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
    /// Reference geometry (the group's un-scaled width/height/channels):
    /// the output grid is this, scaled by `scale`; the `.rej` rejection
    /// bitmaps and the LN grids are in this geometry too. B1 (M3 final fix
    /// wave): a frame in `frames` no longer needs to match this — only its
    /// own `channels` must match `channels` below; its own native geometry
    /// is read from the file (see [`DrizzleFrame::path`]'s doc).
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

/// Peak RAM [`drizzle_group`] needs — ruling R-M3-7, AMENDED by ruling
/// R-M3-16 (was R-M3-15 through fix round 1; renumbered B5, M3 final fix
/// wave, which folds in two more allocations the review found still
/// unaccounted: the single-pass star detector's own working copy inside
/// `measure_plane`'s `detect_fast_data` call — a `lum` buffer the same size
/// as `measure_plane`'s own scaled copy, always made (mono `channels == 1`
/// call) regardless of seed source — and one frame's `RejBitmap` held in RAM
/// while `deposit_band` reads it (`channels * height * ceil(width / 64) * 8`
/// bytes; at most one is ever live at a time — a fresh frame's bitmap
/// replaces the last). Fix round 1's own amendment (originally R-M3-15)
/// after review found the ORIGINAL formula under-counted real peak by up to
/// ~75%, omitting two allocations the driver actually makes: the
/// weight-map planes (real peak when `write_weight_map` is on — the shipped
/// default) and `measure_plane`'s own ADU-scaled copy of the plane it is
/// currently measuring, alive at the same time as `data`/`weight_all`/the
/// last `i_buf`/`w_buf`. The full accounting: the `channels` output-data
/// planes plus the one `I`/`W` accumulator pair alive at a time
/// (`(channels + 2) * out_w * out_h * 4` bytes); the `channels` weight-map
/// planes when `write_weight_map` is set (`channels * out_w * out_h * 4`);
/// `measure_plane`'s scaled copy (`out_w * out_h * 4`); the detector's own
/// `lum` copy of that same plane (`out_w * out_h * 4`); one full-resolution
/// source plane in RAM (`width * height * 4`); when `ln` is set, two more
/// reference-geometry planes for the per-frame `A`/`B` grids
/// (`2 * width * height * 4`); and, when `use_rejection` is set, one
/// reference-geometry `RejBitmap`
/// (`channels * height * ceil(width / 64) * 8`).
pub fn estimate_memory_bytes(
    width: usize,
    height: usize,
    channels: usize,
    scale: u32,
    ln: bool,
    write_weight_map: bool,
    use_rejection: bool,
) -> u64 {
    let out_w = width as u64 * scale as u64;
    let out_h = height as u64 * scale as u64;
    let mut need = (channels as u64 + 2) * out_w * out_h * 4;
    if write_weight_map {
        need += channels as u64 * out_w * out_h * 4;
    }
    need += out_w * out_h * 4; // measure_plane's own ADU-scaled copy
    need += out_w * out_h * 4; // B5: detect_fast_data's own `lum` working copy
    need += width as u64 * height as u64 * 4;
    if ln {
        need += 2 * width as u64 * height as u64 * 4;
    }
    if use_rejection {
        // B5: one frame's `RejBitmap` (reference geometry) held in RAM —
        // `crate::stacking::rej`'s own on-disk body layout, mirrored here:
        // `channels * height * words` u64 words, `words = ceil(width / 64)`.
        let words = (width as u64).div_ceil(64);
        need += channels as u64 * height as u64 * words * 8;
    }
    need
}

/// Everything [`deposit_band`] needs for one frame's deposit pass, bundled
/// so the parallel `for_each` closure captures one small `&FrameDepositCtx`
/// instead of a dozen loose variables.
struct FrameDepositCtx<'a> {
    src: &'a [f32],
    /// This FRAME's own geometry — B1 (M3 final fix wave): indexes `src`
    /// and bounds [`band_source_window`]'s clamp. May differ from
    /// `ref_width`/`ref_height` below (a group's members are not required
    /// to match the run's reference geometry any more).
    src_width: usize,
    src_height: usize,
    /// The run's REFERENCE geometry — bounds the `.rej` bitmap lookup and
    /// indexes the LN grid, both of which are always reference-geometry
    /// sized regardless of this frame's own `src_width`/`src_height`.
    ref_width: usize,
    ref_height: usize,
    map: &'a PixelMap,
    scale: u32,
    drop_shrink: f64,
    kernel: DrizzleKernel,
    kernel_table: Option<&'a geom::KernelTable>,
    rej: Option<&'a RejBitmap>,
    /// `(a_plane, b_plane)`, each `ref_width * ref_height`, reference
    /// geometry — `Some` only when this frame's LN grid is actually driving
    /// output normalization this pass.
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
    // Important 3 (fix round 1): a short `weight`/`output_pair` slice used
    // to fall back silently (`.get(c).unwrap_or(..)`) to "this frame
    // contributes nothing" / "this frame is normalized by the identity" —
    // a caller bug that changed the master's pixels without a word.
    // Validated up front, alongside the other geometry checks, before any
    // allocation — every per-frame access below indexes directly, safe by
    // construction. `weight` only matters when `use_weights` is on (the
    // `else` branch below never reads it); `output_pair` is always the
    // fallback normalization source (LN can be off globally, or absent for
    // any individual frame), so it is always required.
    for (idx, f) in input.frames.iter().enumerate() {
        if input.use_weights && f.weight.len() != input.channels {
            return Err(DrizzleError::BadInput(format!(
                "frame {idx} ({}): weight has {} channel(s), need {}",
                f.path.display(),
                f.weight.len(),
                input.channels
            )));
        }
        if f.output_pair.len() != input.channels {
            return Err(DrizzleError::BadInput(format!(
                "frame {idx} ({}): output_pair has {} channel(s), need {}",
                f.path.display(),
                f.output_pair.len(),
                input.channels
            )));
        }
    }

    let group_start = Instant::now();
    let out_w = input.width * input.scale as usize;
    let out_h = input.height * input.scale as usize;
    let plane_out_pixels = out_w * out_h;

    // Ruling R-M3-7 (amended by R-M3-16, was R-M3-15 through fix round 1):
    // refused BEFORE any output-geometry allocation.
    let need = estimate_memory_bytes(
        input.width,
        input.height,
        input.channels,
        input.scale,
        input.use_local_normalization,
        input.write_weight_map,
        input.use_rejection,
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

    // Minor 12 (fix round 1): accumulate `Duration`s, not per-frame
    // `as_millis() as u64` truncations — summing already-truncated
    // millisecond counts loses up to 1ms per frame, which is not noise at
    // a few hundred frames.
    let mut read_duration_total = std::time::Duration::ZERO;
    let mut deposit_duration_total = std::time::Duration::ZERO;
    let mut bytes_read_total = 0u64;

    // Reused across every (frame, plane) pair that needs local
    // normalization — one `read_plane`, at most one LN evaluation per
    // frame per plane (perf shape, R-M3-6). Minor 4 (fix round 1): sized
    // to `plane_pixels` only when the run can actually use them — an
    // unconditional allocation here cost 208 MB on a full frame the R-M3-7
    // estimate (correctly) never counts for the LN-off case.
    let plane_pixels = input.width * input.height;
    let mut a_plane_buf = if input.use_local_normalization {
        vec![0f32; plane_pixels]
    } else {
        Vec::new()
    };
    let mut b_plane_buf = if input.use_local_normalization {
        vec![0f32; plane_pixels]
    } else {
        Vec::new()
    };

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

            // Safe direct index: validated up front (Important 3, above)
            // that `frame.weight.len() == input.channels` whenever
            // `use_weights` is on.
            let w = if input.use_weights {
                frame.weight[c] as f32
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
            // B1 (M3 final fix wave, I1): only `channels` has to match the
            // group — a frame's own width/height is read straight off the
            // file and carried through as its SOURCE geometry; `map`
            // (subject → reference) is what places it correctly on the
            // shared output grid regardless of how it compares to the
            // reference's own size.
            if reader.channels() != input.channels {
                return Err(DrizzleError::BadInput(format!(
                    "{}: {} channel(s) != group channels {}",
                    frame.path.display(),
                    reader.channels(),
                    input.channels
                )));
            }
            let src_width = reader.width();
            let src_height = reader.height();
            if src_width == 0 || src_height == 0 {
                return Err(DrizzleError::BadInput(format!(
                    "{}: source geometry {src_width}x{src_height} is empty",
                    frame.path.display()
                )));
            }
            let src = reader.read_plane(c)?;
            let read_bytes = (src_width * src_height * 4) as u64;
            plane_bytes_read += read_bytes;
            bytes_read_total += read_bytes;
            read_duration_total += read_start.elapsed();

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

            // Safe direct index: validated up front (Important 3, above)
            // that `frame.output_pair.len() == input.channels`.
            let pair = frame.output_pair[c];

            let ctx = FrameDepositCtx {
                src: &src,
                src_width,
                src_height,
                ref_width: input.width,
                ref_height: input.height,
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
            deposit_duration_total += deposit_start.elapsed();

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

        // Minor 14 (fix round 1): `duration_ms` covers this plane's whole
        // pass — every frame's deposit AND the `measure_plane` call just
        // above, not deposition alone (the event name says "deposited",
        // not "measured", but the timer starts at `plane_start` before
        // either); `frames` is `input.frames.len()`, i.e. every frame this
        // plane WOULD have processed, including any skipped for `w <= 0`.
        // Cosmetic today; split or rename if a later task budgets off this
        // event specifically.
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
        read_ms: read_duration_total.as_millis() as u64,
        deposit_ms: deposit_duration_total.as_millis() as u64,
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
/// `drop_shrink / 2 + 1` on every side. A non-finite map falls back to the
/// WHOLE source plane — correctness over speed on that degenerate path.
/// Returns `None` when the grown window has NO overlap with
/// `[0, width) x [0, height)` at all (a band that maps entirely off-frame,
/// e.g. the frame's own registration shifted it out of the reference —
/// Minor 8, fix round 1: the previous clamp order (`.max(0.0)` applied
/// AFTER `.min(width - 1.0)`) turned a negative upper bound into `0`
/// instead of an empty range, scanning one stray source column/row per
/// such band — harmless (nothing indexes out of range, nothing deposits,
/// since the empty side of the intersection still empties out), but an
/// explicit `None` is honest about the "there is nothing here" case rather
/// than relying on an accidental 1-pixel window plus a separately-empty
/// axis to make it a no-op).
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
) -> Option<(usize, usize, usize, usize)> {
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
        return Some((0, 0, width.saturating_sub(1), height.saturating_sub(1)));
    }
    let margin = drop_shrink / 2.0 + 1.0;
    let (x_lo_grown, x_hi_grown) = (sx_lo - margin, sx_hi + margin);
    let (y_lo_grown, y_hi_grown) = (sy_lo - margin, sy_hi + margin);
    if x_hi_grown < 0.0
        || x_lo_grown > width as f64 - 1.0
        || y_hi_grown < 0.0
        || y_lo_grown > height as f64 - 1.0
    {
        return None;
    }
    let x0 = x_lo_grown.floor().max(0.0) as usize;
    let x1 = x_hi_grown.ceil().min(width as f64 - 1.0).max(0.0) as usize;
    let y0s = y_lo_grown.floor().max(0.0) as usize;
    let y1 = y_hi_grown.ceil().min(height as f64 - 1.0).max(0.0) as usize;
    Some((x0, y0s, x1, y1))
}

/// "The pixel whose extent contains coordinate `v`" — `floor(v + 0.5)`,
/// deliberately not `f64::round` (ties-away-from-zero disagrees with this at
/// every negative half-integer: `round(-0.5) == -1` but
/// `floor(-0.5 + 0.5) == 0`), matching `geom::map_drop`'s own documented
/// convention. B4 (M3 final fix wave): the ONE helper both the `.rej`/LN
/// index (which used this inline already) and the tabulated-kernel LUT
/// deposit (which used `f64::round`, inconsistently) now share.
#[inline]
fn round_half_up(v: f64) -> f64 {
    (v + 0.5).floor()
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

    let Some((sx0, sy0, sx1, sy1)) = band_source_window(
        ctx.map,
        out_w,
        y0,
        rows,
        ctx.scale,
        ctx.src_width,
        ctx.src_height,
        ctx.drop_shrink,
    ) else {
        return; // Minor 8: this band maps entirely off-frame — nothing to deposit.
    };
    // Ruling R-M3-3's tabulated kernel weights sum to `drop_shrink²` in
    // SOURCE-pixel units (see `geom::kernel_table`'s own doc); scaled by
    // `scale²` here so `circle`/`gaussian` deposit the same mass per drop
    // as `square`'s exact clipping does in OUTPUT-pixel units. Minor 7
    // (fix round 1): this factor is unit-hygiene only — `I / W` and
    // `W / max(W)` both cancel any constant common to every deposit of a
    // single kernel choice, so no output-level test can distinguish
    // "present" from "missing" here. It is pinned directly instead, by
    // `circle_kernel_deposits_the_same_total_mass_as_square_for_the_same_drop`
    // below, which compares the RAW summed mass `deposit_band` writes into
    // `wb` (bypassing the public API's normalization) between the two
    // kernels for one isolated drop.
    let scale_sq = ctx.scale as f64 * ctx.scale as f64;

    for y in sy0..=sy1 {
        let row_off = y * ctx.src_width;
        for x in sx0..=sx1 {
            let d = ctx.src[row_off + x];
            if !d.is_finite() || d == 0.0 {
                continue;
            }
            let (u, v) = ctx.map.forward(x as f64, y as f64);
            if !u.is_finite() || !v.is_finite() {
                continue;
            }
            // Minor 10 (fix round 1): `floor(v + 0.5)`, not `f64::round`
            // (ties-away-from-zero) — matches `geom::map_drop`'s own
            // documented convention (geom.rs), rather than relying on the
            // range guards below to make the two agree by construction.
            let ix_f = round_half_up(u);
            let iy_f = round_half_up(v);

            if let Some(rb) = ctx.rej {
                // Ruling R-M3-4: out-of-range → not rejected. `(ix_f, iy_f)`
                // are REFERENCE-geometry coordinates (`ctx.map.forward`
                // maps subject → reference) — bounded by `ref_width`/
                // `ref_height`, not this frame's own `src_width`/
                // `src_height` (B1, M3 final fix wave).
                let rejected = ix_f >= 0.0
                    && iy_f >= 0.0
                    && (ix_f as usize) < ctx.ref_width
                    && (iy_f as usize) < ctx.ref_height
                    && rb.is_rejected(ctx.plane, ix_f as usize, iy_f as usize);
                if rejected {
                    continue;
                }
            }

            let nd = if let Some((a_plane, b_plane)) = ctx.ln {
                // Ruling R-M3-5: clamped (not skipped) to the reference
                // extent — the LN grid is always reference-geometry sized
                // (B1, M3 final fix wave).
                let ix = (ix_f as i64).clamp(0, ctx.ref_width as i64 - 1) as usize;
                let iy = (iy_f as i64).clamp(0, ctx.ref_height as i64 - 1) as usize;
                let idx = iy * ctx.ref_width + ix;
                a_plane[idx] * d + b_plane[idx]
            } else {
                ctx.pair.apply(d)
            };
            // Minor 13 (fix round 1): a non-finite `nd` (a NaN/inf LN grid
            // cell, or a degenerate `output_pair`) must not propagate — the
            // source SAMPLE is already checked above, the NORMALIZED value
            // was not.
            if !nd.is_finite() {
                continue;
            }

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
                        // B4 (M3 final fix wave): `round_half_up`, not
                        // `f64::round` — matches the rejection/LN index
                        // above and `geom::map_drop`'s own convention.
                        let px = round_half_up(ox) as i64;
                        let py = round_half_up(oy) as i64;
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

    fn dithered_fwhm(sigma: f64, drop_shrink: f64) -> f64 {
        let dir = tempfile::tempdir().unwrap();
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

    // R-M3-14 (fix round 1, review-confirmed): `measure_plane` runs on the
    // OUTPUT grid (mod.rs's own `measure_plane(plane_data, out_w, out_h,
    // ..)` call), so both `fwhm_drz(0.6)` and `fwhm_drz(1.0)` are already
    // in the SAME (output-px) units — the original test divided only one
    // side by 2 (a reference-px conversion applied to a single term), which
    // made the assertion trivially true (49% margin) regardless of whether
    // drop_shrink affected the deposit at all. The honest comparison never
    // rescales either side.
    #[test]
    fn sharpening_recovers_sub_pixel_dither() {
        let fwhm_07_06 = dithered_fwhm(0.7, 0.6);
        let fwhm_07_10 = dithered_fwhm(0.7, 1.0);
        let ratio_07 = fwhm_07_06 / fwhm_07_10;

        // The undersampled variant: the drop's own blur is a LARGER
        // fraction of an undersampled star's total width, so the coarser
        // drop_shrink=1.0 deposit should hurt it more — ratio(0.45) should
        // sit below ratio(0.7).
        let fwhm_045_06 = dithered_fwhm(0.45, 0.6);
        let fwhm_045_10 = dithered_fwhm(0.45, 1.0);
        let ratio_045 = fwhm_045_06 / fwhm_045_10;

        eprintln!(
            "sharpening test: sigma=0.7 fwhm(ds=0.6)={fwhm_07_06} fwhm(ds=1.0)={fwhm_07_10} ratio={ratio_07}; \
             sigma=0.45 fwhm(ds=0.6)={fwhm_045_06} fwhm(ds=1.0)={fwhm_045_10} ratio={ratio_045}"
        );

        assert!(
            fwhm_07_06 > 0.0 && fwhm_07_10 > 0.0,
            "sigma=0.7: fwhm must be > 0 (star must be detected)"
        );
        assert!(
            fwhm_045_06 > 0.0 && fwhm_045_10 > 0.0,
            "sigma=0.45: fwhm must be > 0 (star must be detected)"
        );

        assert!(ratio_07 < 0.985, "ratio_07={ratio_07}");
        assert!(
            ratio_045 < ratio_07,
            "ratio_045={ratio_045} ratio_07={ratio_07}"
        );
        assert!(ratio_045 < 0.97, "ratio_045={ratio_045}");
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

    // Minor 5 (fix round 1): `LnGrid::constant` makes `a`/`b` uniform, so an
    // index transpose (`ix*height+iy` instead of the correct
    // `iy*width+ix`) would read the SAME value everywhere and the test
    // above would not notice. Build a grid whose `b` genuinely varies with
    // grid COLUMN (x) only, independently reproduce the SAME
    // `evaluate_row_into` pass the driver runs internally to get ground
    // truth, and check the driver's actual output against it at two points
    // that differ only in x. `scale = 1` keeps output pixel `(x, y)` ==
    // source pixel `(x, y)` exactly (identity map, a single frame, Square
    // kernel with a drop fully inside its own output pixel), so the
    // predicted value is reproduced exactly rather than merely
    // approximately.
    #[test]
    fn local_normalization_index_is_not_transposed() {
        let dir = tempfile::tempdir().unwrap();
        let value = 0.2f32;
        let data = uniform(W, H, value);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();

        // `LnGrid`'s own `scale` field is the LN tile size
        // (`LnGrid::stride() = (scale/8).max(2)`) — unrelated to the
        // drizzle output scale below despite the shared field name. 256
        // gives stride 32, a real multi-node grid (gw=3, gh=3) across the
        // 64x48 plane.
        let mut grid = LnGrid::constant(W, H, 256, 1.0, 0.0);
        let (gw, gh) = (grid.gw, grid.gh);
        for j in 0..gh {
            for i in 0..gw {
                grid.b[j * gw + i] = i as f32 * 0.1;
            }
        }
        let grids = LnFrameGrids {
            channels: vec![grid.clone()],
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
        input.scale = 1; // output pixel == source pixel, exactly
        input.use_local_normalization = true;

        // Ground truth: the SAME evaluator the driver calls, run
        // independently against the same grid.
        let mut expected_a = vec![0f32; W * H];
        let mut expected_b = vec![0f32; W * H];
        let mut scratch = LnScratch::for_grid(&grid);
        for y in 0..H {
            let start = y * W;
            let end = start + W;
            grid.evaluate_row_into(
                y,
                &mut expected_a[start..end],
                &mut expected_b[start..end],
                &mut scratch,
            );
        }

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = W; // scale = 1

        for &(x, y) in &[(10usize, 20usize), (50usize, 20usize)] {
            let idx = y * W + x;
            let expected = expected_a[idx] * value + expected_b[idx];
            let actual = out.data[y * out_w + x];
            assert!(
                (actual - expected).abs() < 1e-4,
                "x={x} y={y} actual={actual} expected={expected}"
            );
        }

        // Not vacuous: the two predicted values must actually differ (the
        // gradient is genuinely being read, not some constant fallback that
        // would coincidentally satisfy the check above).
        let e_left = expected_a[20 * W + 10] * value + expected_b[20 * W + 10];
        let e_right = expected_a[20 * W + 50] * value + expected_b[20 * W + 50];
        assert!(
            (e_left - e_right).abs() > 0.01,
            "e_left={e_left} e_right={e_right}"
        );
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
    fn estimate_memory_bytes_matches_the_r_m3_16_formula() {
        // R-M3-16 (B5, M3 final fix wave, was R-M3-15 through fix round 1,
        // amends R-M3-7): the weight-map planes, `measure_plane`'s own
        // scaled copy, the star detector's own `lum` working copy and one
        // frame's `RejBitmap` are all counted now — checked across every
        // `(ln, write_weight_map, use_rejection)` combination.
        let (width, height, channels, scale) = (6224usize, 4168usize, 3usize, 3u32);
        let out_w = width as u64 * scale as u64;
        let out_h = height as u64 * scale as u64;
        let base = (channels as u64 + 2) * out_w * out_h * 4;
        let weight_map = channels as u64 * out_w * out_h * 4;
        let measure_scratch = out_w * out_h * 4;
        let detector_scratch = out_w * out_h * 4;
        let source = width as u64 * height as u64 * 4;
        let ln_planes = 2 * width as u64 * height as u64 * 4;
        let words = (width as u64).div_ceil(64);
        let rej_bitmap = channels as u64 * height as u64 * words * 8;

        for &ln in &[false, true] {
            for &wwm in &[false, true] {
                for &rej in &[false, true] {
                    let mut expected = base + measure_scratch + detector_scratch + source;
                    if wwm {
                        expected += weight_map;
                    }
                    if ln {
                        expected += ln_planes;
                    }
                    if rej {
                        expected += rej_bitmap;
                    }
                    assert_eq!(
                        estimate_memory_bytes(width, height, channels, scale, ln, wwm, rej),
                        expected,
                        "ln={ln} write_weight_map={wwm} use_rejection={rej}"
                    );
                }
            }
        }
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
            // Minor 6 (fix round 1): `write_weight_map: true` so this test
            // can also check the RAW weight (not just the I/W ratio) is
            // flat across a seam — I/W alone cannot tell a double deposit
            // (I and W both doubled together, ratio unchanged) from a
            // correct one; the weight map can.
            write_weight_map: true,
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

        // Minor 6: the weight map must be FLAT (no doubling, no gap) at the
        // exact seam rows and their immediate neighbours, compared against
        // an unambiguously interior reference row.
        let weight_map = out.weight.as_ref().expect("write_weight_map was on");
        let x = 20usize;
        let reference = weight_map[10 * out_w + x];
        for &seam in &[
            510usize, 511, 512, 513, 1022, 1023, 1024, 1025, 1534, 1535, 1536, 1537,
        ] {
            let v = weight_map[seam * out_w + x];
            assert!(
                (v - reference).abs() < 1e-6,
                "seam={seam} x={x} weight={v} reference={reference}"
            );
        }
    }

    // ── Important 3 (fix round 1): a short `weight`/`output_pair` slice is
    // loud, not a silent fallback. ──

    #[test]
    fn mismatched_weight_or_output_pair_length_is_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.2);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let map = identity_map();
        let measure = MeasureOptions::default();

        // A `weight` slice shorter than `channels`, with `use_weights` on.
        let short_weight: [f64; 0] = [];
        let pair = identity_pair();
        let f_bad_weight = DrizzleFrame {
            path: &p0,
            map: &map,
            weight: &short_weight,
            output_pair: &pair,
            ln: None,
            rej: None,
        };
        let frames = [f_bad_weight];
        let mut input = base_input(&frames, &measure);
        input.use_weights = true;
        let err =
            drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap_err();
        assert!(matches!(err, DrizzleError::BadInput(_)), "{err:?}");
        if let DrizzleError::BadInput(msg) = &err {
            assert!(msg.contains("weight"), "{msg}");
        }

        // An `output_pair` slice shorter than `channels` — checked
        // unconditionally, regardless of `use_weights`/LN settings.
        let weight = [1.0f64];
        let short_pair: [NormalizationPair; 0] = [];
        let f_bad_pair = DrizzleFrame {
            path: &p0,
            map: &map,
            weight: &weight,
            output_pair: &short_pair,
            ln: None,
            rej: None,
        };
        let frames2 = [f_bad_pair];
        let input2 = base_input(&frames2, &measure);
        let err2 =
            drizzle_group(&input2, &pool(), &AtomicBool::new(false), &no_progress()).unwrap_err();
        assert!(matches!(err2, DrizzleError::BadInput(_)), "{err2:?}");
        if let DrizzleError::BadInput(msg) = &err2 {
            assert!(msg.contains("output_pair"), "{msg}");
        }
    }

    // ── Minor 7 (fix round 1): make the `scale²` tabulated-kernel factor
    // OBSERVABLE — a white-box test that calls `deposit_band` directly
    // (bypassing `drizzle_group`'s public API, whose `I/W` and
    // `W/max(W)` both cancel any constant common to every deposit of one
    // kernel, which is exactly why the factor was unobservable through it)
    // and compares the RAW summed mass in `wb` between `square` and
    // `circle` for the SAME isolated drop. Both kernels must deposit the
    // same total mass — `scale² · dropShrink²` — for a single source pixel:
    // `square`'s exact-clip total is `scale² · dropShrink²` by construction
    // (pinned independently by Task 1's own geometry tests); `circle`'s
    // tabulated weights are renormalized to sum to `dropShrink²` in SOURCE
    // units regardless of the hard radius cutoff (Task 1's own
    // `circle_kernel_table_is_a_hard_radius_cutoff` pins the sum), so
    // without the `scale²` factor at the deposit site circle's total would
    // be `dropShrink²` alone — off by exactly `scale²` (4x at scale=2), a
    // discrepancy this test would catch immediately. ──

    #[test]
    fn circle_kernel_deposits_the_same_total_mass_as_square_for_the_same_drop() {
        let (width, height) = (20usize, 20usize);
        let mut src = vec![0f32; width * height];
        src[10 * width + 10] = 1.0;
        let map = identity_map();
        let scale = 2u32;
        let out_w = width * scale as usize;
        let out_h = height * scale as usize;
        let drop_shrink = 0.9;
        let pair = NormalizationPair::IDENTITY;

        let mut ib_sq = vec![0f32; out_w * out_h];
        let mut wb_sq = vec![0f32; out_w * out_h];
        let ctx_sq = FrameDepositCtx {
            src: &src,
            src_width: width,
            src_height: height,
            ref_width: width,
            ref_height: height,
            map: &map,
            scale,
            drop_shrink,
            kernel: DrizzleKernel::Square,
            kernel_table: None,
            rej: None,
            ln: None,
            pair,
            w: 1.0,
            plane: 0,
        };
        deposit_band(&mut ib_sq, &mut wb_sq, 0, out_w, &ctx_sq);
        let mass_sq: f64 = wb_sq.iter().map(|&v| v as f64).sum();

        let table = geom::kernel_table(DrizzleKernel::Circle, drop_shrink).unwrap();
        let mut ib_c = vec![0f32; out_w * out_h];
        let mut wb_c = vec![0f32; out_w * out_h];
        let ctx_c = FrameDepositCtx {
            src: &src,
            src_width: width,
            src_height: height,
            ref_width: width,
            ref_height: height,
            map: &map,
            scale,
            drop_shrink,
            kernel: DrizzleKernel::Circle,
            kernel_table: Some(&table),
            rej: None,
            ln: None,
            pair,
            w: 1.0,
            plane: 0,
        };
        deposit_band(&mut ib_c, &mut wb_c, 0, out_w, &ctx_c);
        let mass_c: f64 = wb_c.iter().map(|&v| v as f64).sum();

        let expected = (scale as f64).powi(2) * drop_shrink * drop_shrink;
        assert!(
            (mass_sq - expected).abs() < 1e-2,
            "mass_sq={mass_sq} expected={expected}"
        );
        assert!(
            (mass_c - expected).abs() < 1e-2,
            "mass_c={mass_c} expected={expected}"
        );
        assert!(
            (mass_sq - mass_c).abs() < 1e-2,
            "mass_sq={mass_sq} mass_c={mass_c}"
        );
    }

    // ── Minor 8 (fix round 1): `band_source_window` returns `None`, not an
    // accidental 1-pixel window, when a band maps entirely off-frame. ──

    #[test]
    fn band_source_window_returns_none_when_the_band_is_entirely_off_frame() {
        // A translation far enough that a band mapped through `inverse`
        // lands well outside `[0, width) x [0, height)` on every axis.
        let map = translation_map(-10_000.0, -10_000.0);
        let result = band_source_window(&map, 64, 0, 48, 2, 32, 24, 0.9);
        assert_eq!(
            result, None,
            "far off-frame band must be None, not a stray window"
        );
    }

    // ── Minor 13 (fix round 1): a non-finite normalized value (`nd`) must
    // be skipped, never propagate into `I`/`W`. ──

    #[test]
    fn non_finite_normalized_value_is_skipped_not_propagated() {
        let dir = tempfile::tempdir().unwrap();
        // One lit source pixel on a zero field (same shape as test (b)) so
        // the non-finite `nd` only ever afflicts that single drop — any
        // other pixel is already skipped for being exactly zero, so this
        // isolates the `nd`-finiteness guard specifically.
        let mut data = vec![0f32; W * H];
        data[10 * W + 10] = 1.0;
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let map = identity_map();
        let weight = [1.0f64];
        // `NaN * d + 0.0` is NaN for the one nonzero sample.
        let pair = [NormalizationPair {
            scale: f32::NAN,
            offset: 0.0,
        }];
        let frames = [frame(&p0, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        input.scale = 2;
        input.drop_shrink = 1.0;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        assert!(
            out.data.iter().all(|v| v.is_finite()),
            "no NaN must reach `data`"
        );
        let weight_map = out.weight.as_ref().expect("write_weight_map was on");
        assert!(
            weight_map.iter().all(|v| v.is_finite()),
            "no NaN must reach `weight`"
        );
        // The whole plane must be all-zero: the only nonzero source sample
        // was skipped for a non-finite `nd`, so nothing was ever deposited.
        assert!(
            out.data.iter().all(|&v| v == 0.0),
            "the skipped drop must leave the plane at zero"
        );
        assert_eq!(
            out.stats.coverage[0], 0.0,
            "coverage={}",
            out.stats.coverage[0]
        );
    }

    // ── B1 (M3 final fix wave, I1): a group whose members differ in native
    // geometry from the reference — the acceptance set's own shape (a
    // group's members are not required to match `DrizzleInput::width`/
    // `height` any more). ──

    #[test]
    fn a_frame_smaller_or_larger_than_the_reference_still_drizzles_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let background = 0.25f32;

        // Two frames at the reference's own geometry (W x H = 64x48).
        let normal = uniform(W, H, background);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &normal);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &normal);

        // The odd frame: native 70x50 — WIDER and TALLER than the
        // reference — uniform `background` everywhere except one marker
        // pixel at (x=5, y=10), well inside the reference's own 64x48
        // extent, bumped to a distinct value. A stride bug that indexed
        // this 70-wide plane with the reference's width (64) instead of its
        // own would misread row 10 entirely (a flat-offset shift of
        // `10 * (70 - 64) = 60` elements) — the marker would be read as
        // `background` instead of `marker`, silently failing the level
        // check below.
        let (odd_w, odd_h) = (70usize, 50usize);
        let marker = 0.9f32;
        let (mx, my) = (5usize, 10usize);
        let mut odd = uniform(odd_w, odd_h, background);
        odd[my * odd_w + mx] = marker;
        let p2 = write_mono(dir.path(), "f2.fits", odd_w, odd_h, &odd);

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
        input.scale = 2;
        input.drop_shrink = 1.0; // exact 2x2 output block per source pixel

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();

        // "the drizzled output is (64*s)x(48*s)" — reference geometry x
        // scale, unaffected by the odd frame's own (larger) native size.
        assert_eq!(out.width, W * 2);
        assert_eq!(out.height, H * 2);

        // "the level is preserved where all three frames overlap" — sampled
        // away from the marker's own output block and from the canvas
        // edges (the usual margin, ruling out a partial-coverage border
        // effect).
        let out_w = W * 2;
        for &(x, y) in &[(40usize, 20usize), (100, 60), (20, 60), (120, 10)] {
            let v = out.data[y * out_w + x];
            assert!(
                (v - background).abs() < 1e-6,
                "x={x} y={y} v={v} (expected background {background})"
            );
        }

        // The marker: correct indexing reads it at (mx, my) in the ODD
        // frame's plane and deposits it at output block
        // `(2*mx, 2*my)..=(2*mx+1, 2*my+1)` (drop_shrink=1.0, scale=2 —
        // same exact-block convention as `spread_single_pixel_lands_on_
        // four_output_pixels`). The other two frames contribute
        // `background` at the same spot, so the level-preserving `I/W`
        // weighted mean is `(background + background + marker) / 3`.
        let expected = (background as f64 * 2.0 + marker as f64) / 3.0;
        for &(x, y) in &[
            (2 * mx, 2 * my),
            (2 * mx + 1, 2 * my),
            (2 * mx, 2 * my + 1),
            (2 * mx + 1, 2 * my + 1),
        ] {
            let v = out.data[y * out_w + x] as f64;
            assert!(
                (v - expected).abs() < 1e-4,
                "x={x} y={y} v={v} expected={expected} \
                 (a stride bug reading the odd frame's 70-wide plane at the \
                 reference's width would read `background` here, not `marker`)"
            );
        }

        // "the odd frame's out-of-reference pixels never land": the output
        // canvas IS exactly the reference's own geometry (checked above) —
        // there is no output pixel a source coordinate past x>=64 or y>=48
        // could possibly land on, so this holds by construction; the
        // coverage figure below is the observable proxy (full coverage,
        // nothing dropped, nothing corrupted beyond the canvas).
        assert!(
            (out.stats.coverage[0] - 1.0).abs() < 1e-9,
            "coverage={}",
            out.stats.coverage[0]
        );
    }
}
