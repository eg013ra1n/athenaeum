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
//!
//! M4d Task 1 (ruling R-M4d-2) adds Bayer drizzle: with a
//! [`DrizzleFrame::cfa`] source, every output plane is deposited from the
//! frame's ONE calibrated CFA mosaic and a source pixel only reaches the
//! plane of its own colour ([`geom::cfa_plane_of`]) — registration,
//! weights, rejection and LN are untouched, all still the debayered run's
//! in reference geometry.

pub mod geom;
pub mod phase_table;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use astroimage::BayerPattern;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::geometry::{ForwardEval, PixelMap};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::NormalizationPair;
use crate::integration::IntegrationError;
use crate::stacking::drizzle::phase_table::PhaseTable;
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
    /// M4d Task 1 (ruling R-M4d-2): Bayer drizzle. When set, EVERY output
    /// plane is deposited from this ONE single-plane calibrated CFA mosaic
    /// instead of from `path`'s interpolated planes, and a source pixel
    /// only reaches the plane of its own colour
    /// ([`geom::cfa_plane_of`]). Requires a 3-channel group; `None` is the
    /// debayered deposit M1–M4c always did.
    ///
    /// Everything else stays the DEBAYERED run's, in reference geometry:
    /// the `map`, the weights, the `.rej` lookup and the LN grids (math
    /// §6.4 — "alignment data still come from the registration of the
    /// debayered frame"). The mosaic and the debayered frame share one
    /// geometry by construction (the debayer runs at native resolution),
    /// so the same `map` carries both.
    pub cfa: Option<CfaSource<'a>>,
}

/// The calibrated CFA mosaic one frame deposits from under Bayer drizzle
/// (M4d Task 1, ruling R-M4d-2): the `calibrated_mosaic` artifact's path
/// and the mosaic's phase-corrected pattern (the same value the debayer
/// itself was given, so the two can never disagree about which pixel is
/// red).
#[derive(Debug, Clone, Copy)]
pub struct CfaSource<'a> {
    pub path: &'a Path,
    pub pattern: BayerPattern,
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
    /// Forces the exact per-pixel clip everywhere, even where Tier C item
    /// C2's phase table would apply (ruling C-3). **The run never sets
    /// this** — `stacking::run` passes `false` and there is no config key
    /// for it. It exists so the `drizzle_probe` can time the two overlap
    /// arms against each other INTERLEAVED in one process (ruling R-TA-9:
    /// only interleaved before/after brackets are comparable on a machine
    /// that drifts 10-15 % across a session), rather than through two
    /// binaries or an environment variable nothing type-checks.
    pub force_exact_overlap: bool,
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
    /// Subject → reference, for the sparse band-window probe (which asks
    /// the INVERSE direction and goes through the exact path).
    map: &'a PixelMap,
    /// The frame's forward evaluator, built ONCE per (frame, plane) and
    /// shared by every band (ruling R-T4-6b): the deposit calls it twice
    /// per source pixel plus four times per drop corner, so a cache lock
    /// in there would be six locks per pixel.
    fwd: ForwardEval<'a>,
    /// Tier A task 11 (Z1): [`geom::drop_bound_half_diag`] of the frame's
    /// map, sampled once when this ctx is built — the per-frame constant
    /// `deposit_band`'s early band skip compares a mapped pixel centre
    /// against, before any per-pixel side effect.
    half_diag: f64,
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
    /// M4d Task 1 (ruling R-M4d-2): the mosaic's phase-corrected pattern
    /// when `src` is a CFA mosaic — a source pixel then reaches `plane`
    /// only when [`geom::cfa_plane_of`] says it carries that colour.
    /// `None` is the debayered deposit (`src` is already `plane`'s own
    /// interpolated plane, every pixel contributes).
    cfa: Option<BayerPattern>,
    /// Tier C item C2 (ruling C-3): where the `square` kernel gets ONE
    /// drop's output-pixel overlap areas — the exact per-pixel clip, a
    /// per-frame phase table, or a per-tile one. Resolved once per (frame,
    /// plane) by [`resolve_square_overlap`]; it decides only the GEOMETRY
    /// of a drop's overlap, never what is deposited or whether.
    overlap: SquareOverlap<'a>,
}

/// Tier C item C2 (spec §3.2, ruling C-3): the three ways one drop's
/// output-pixel overlap areas are obtained. Every arm feeds the SAME
/// accumulation path in [`deposit_band`] — the frame weight, the
/// normalized sample, the `.rej` verdict, the LN grid and the CFA routing
/// are all resolved above the dispatch and applied once.
#[derive(Clone, Copy)]
enum SquareOverlap<'a> {
    /// The exact Sutherland-Hodgman clip per source pixel — M1..M4d's own
    /// path, kept as the runtime path for `scale == 1 && dropShrink == 1.0`
    /// (ruling C-3: the table would be a 1x1 identity there, not worth a
    /// build) and as the oracle every C2 pin measures against.
    Exact,
    /// One table for the whole frame: with no distortion layer the mapped
    /// drop is one parallelogram, so only its sub-pixel phase varies.
    Frame(&'a PhaseTable),
    /// Rebuilt per [`phase_table::TILE`]-pixel source tile from the map's
    /// local Jacobian — a distortion layer varies the drop's shape across
    /// the frame.
    Tiled,
}

/// One source tile's table under [`SquareOverlap::Tiled`]: not built yet,
/// refused by [`PhaseTable::build`] (a degenerate local mapping — that
/// tile deposits through the exact clip instead), or built.
enum TileSlot {
    Empty,
    Refused,
    Built(PhaseTable),
}

/// Whether the phase table applies at all (ruling C-3): at `scale == 1`
/// with an unshrunk drop the mapped drop of an identity-ish map is one
/// whole output pixel, the table degenerates, and the exact clip is both
/// cheaper and exact. Every other `(scale, dropShrink)` pair tabulates.
fn phase_table_applies(scale: u32, drop_shrink: f64) -> bool {
    !(scale == 1 && drop_shrink == 1.0)
}

/// The `deposit_mode` field of the `drizzle plane deposited` event
/// (Tier C item C2): which overlap arm this plane's frames took —
/// `exact`, `table`, `tiled`, `mixed` when a plane used more than one,
/// and `none` when no frame was deposited at all (every frame's weight
/// was zero, or the group is empty).
fn overlap_mode_label(counts: &[usize; 3]) -> &'static str {
    match (counts[0] > 0, counts[1] > 0, counts[2] > 0) {
        (true, false, false) => "exact",
        (false, true, false) => "table",
        (false, false, true) => "tiled",
        (false, false, false) => "none",
        _ => "mixed",
    }
}

/// Accumulates one drop-to-output-pixel overlap. The ONE place `ib`/`wb`
/// are written, shared by both overlap arms (the exact clip and the phase
/// table) so the weight and coverage bookkeeping exists once.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn deposit_cell(
    ib: &mut [f32],
    wb: &mut [f32],
    out_w: usize,
    y0: usize,
    y1: usize,
    px: i64,
    py: i64,
    aw: f32,
    nd: f32,
) {
    if px < 0 || px as usize >= out_w || py < y0 as i64 || py >= y1 as i64 {
        return;
    }
    let idx = (py - y0 as i64) as usize * out_w + px as usize;
    ib[idx] += aw * nd;
    wb[idx] += aw;
}

/// [`SquareOverlap::Exact`]: map the drop's four corners and clip it
/// against every output pixel of its own bounding box (M1's path,
/// unchanged).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn deposit_exact(
    ib: &mut [f32],
    wb: &mut [f32],
    out_w: usize,
    y0: usize,
    y1: usize,
    ctx: &FrameDepositCtx<'_>,
    x: usize,
    y: usize,
    wf: f64,
    nd: f32,
) {
    let corners = geom::drop_corners(x, y, ctx.drop_shrink);
    let Some((quad, bbox)) = geom::map_drop(&ctx.fwd, &corners, ctx.scale) else {
        return;
    };
    let (bx0, by0, bx1, by1) = bbox;
    let px0 = bx0.max(0);
    let px1 = bx1.min(out_w as i64 - 1);
    let py0 = by0.max(y0 as i64);
    let py1 = by1.min(y1 as i64 - 1);
    for py in py0..=py1 {
        for px in px0..=px1 {
            let a = geom::clip_area(&quad, px, py);
            if a > 0.0 {
                deposit_cell(ib, wb, out_w, y0, y1, px, py, (a * wf) as f32, nd);
            }
        }
    }
}

/// [`SquareOverlap::Frame`]/[`SquareOverlap::Tiled`]: the drop's mapped
/// centre `(ox, oy)` splits into an integer output pixel and a sub-pixel
/// phase; the table answers "which neighbours, how much area" for that
/// phase with no clip and no division.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn deposit_tabulated(
    ib: &mut [f32],
    wb: &mut [f32],
    out_w: usize,
    y0: usize,
    y1: usize,
    table: &PhaseTable,
    ox: f64,
    oy: f64,
    wf: f64,
    nd: f32,
) {
    let cxf = ox.floor();
    let cyf = oy.floor();
    let (cells, areas) = table.lookup(ox - cxf, oy - cyf);
    let cx = cxf as i64;
    let cy = cyf as i64;
    for (&(dx, dy), &a) in cells.iter().zip(areas.iter()) {
        deposit_cell(
            ib,
            wb,
            out_w,
            y0,
            y1,
            cx + dx as i64,
            cy + dy as i64,
            (a as f64 * wf) as f32,
            nd,
        );
    }
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
        // M4d Task 1 (ruling R-M4d-2): a mosaic routes its pixels into R/G/B,
        // so a group that is not 3-channel has nothing to route them into.
        // The run never builds a `CfaSource` for a mono group (it ignores
        // `drizzle.bayer` there, silently, by design); a caller that does is
        // a bug worth naming rather than silently drizzling the mosaic as if
        // it were a luminance plane.
        if f.cfa.is_some() && input.channels != 3 {
            return Err(DrizzleError::BadInput(format!(
                "frame {idx} ({}): a CFA mosaic source needs a 3-channel group, got {}",
                f.path.display(),
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
    // Displacement-grid bytes handed back after each frame's deposit
    // (ruling R-T4-6c) — reported on the stage's finish event so a `tps`
    // run's grid churn is visible without a profiler.
    let mut released_bytes = 0u64;
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
        // Per-plane mirrors of `read_duration_total`/`deposit_duration_total`
        // (perf tier 1 Task 0) — reset each iteration so the plane's own
        // event can report its own split, not the group's running total.
        let mut plane_read = std::time::Duration::ZERO;
        let mut plane_deposit = std::time::Duration::ZERO;
        // Tier C item C2: how many of this plane's frames deposited
        // through the exact clip / a per-frame table / per-tile tables —
        // reported as `deposit_mode` on the plane event so a run can be
        // told to have taken the tabulated path without a profiler.
        let mut plane_overlap_modes = [0usize; 3];

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
            // M4d Task 1 (ruling R-M4d-2): under Bayer drizzle the source of
            // EVERY plane is the one single-plane mosaic — `plane 0` of it,
            // three times over (once per output plane), each pass keeping
            // only the pixels of that plane's own colour.
            //
            // Tier A task 11 (Z5, considered and NOT landed): this reads
            // the SAME mosaic file up to three times per frame (once per
            // `c` in `0..input.channels`) because the loop nest above is
            // `for c in 0..channels { for frame in frames { ... } }` — the
            // PLANE loop is outer, the frame loop inner, on purpose (this
            // is what keeps only ONE `(i_buf, w_buf)` accumulator pair
            // alive at a time — `estimate_memory_bytes`'s R-M3-7/16 doc
            // above states the formula's whole premise as "the channels
            // output-data planes plus the ONE `I`/`W` accumulator pair
            // alive at a time", not `channels` pairs). Caching a frame's
            // decoded mosaic across its three plane passes only pays off
            // if the SAME frame is visited for `c=1` right after `c=0`,
            // which this loop order never does — every OTHER frame in the
            // group is read for `c=0` in between. The only cache that
            // could still help is "every frame's mosaic held for the whole
            // group", which costs `frame_count × mosaic_bytes` ON TOP of
            // the existing per-plane terms `estimate_memory_bytes` already
            // budgets (104 MB per frame at 6248×4176 f32, so a 50-frame
            // OSC group would add ≈5.2 GB) — for any group past a handful
            // of frames this blows well past the R-M3-7 refusal ceiling
            // (half of probed total RAM, or the 4 GiB floor when unknown),
            // which the estimate above is checked against BEFORE any
            // output allocation happens. Restructuring the loop so frame
            // is outer and all `channels` accumulators are held live
            // instead trades that same memory pressure for a different
            // fixed cost (3 output planes × 4× area at 2× scale ≈ 3 × 417
            // MB ≈ 1.25 GB plus the weight-map planes on a real OSC
            // group's geometry) and is a materially bigger, riskier change
            // than this task's scope — so the re-read stays, and only the
            // per-pixel Z1 skip above lands this task.
            let src_path = match frame.cfa {
                Some(cfa) => cfa.path,
                None => frame.path,
            };
            let reader = PlaneReader::open(src_path)?;
            // B1 (M3 final fix wave, I1): only `channels` has to match the
            // group — a frame's own width/height is read straight off the
            // file and carried through as its SOURCE geometry; `map`
            // (subject → reference) is what places it correctly on the
            // shared output grid regardless of how it compares to the
            // reference's own size. A mosaic is single-plane by definition,
            // and its own geometry is the frame's native one (the debayer
            // runs at native resolution), so the same `map` carries it.
            let want_channels = if frame.cfa.is_some() {
                1
            } else {
                input.channels
            };
            if reader.channels() != want_channels {
                return Err(DrizzleError::BadInput(format!(
                    "{}: {} channel(s) != expected {}",
                    src_path.display(),
                    reader.channels(),
                    want_channels
                )));
            }
            let src_width = reader.width();
            let src_height = reader.height();
            if src_width == 0 || src_height == 0 {
                return Err(DrizzleError::BadInput(format!(
                    "{}: source geometry {src_width}x{src_height} is empty",
                    src_path.display()
                )));
            }
            let src = reader.read_plane(if frame.cfa.is_some() { 0 } else { c })?;
            let read_bytes = (src_width * src_height * 4) as u64;
            plane_bytes_read += read_bytes;
            bytes_read_total += read_bytes;
            let read_elapsed = read_start.elapsed();
            read_duration_total += read_elapsed;
            plane_read += read_elapsed;

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

            let fwd = frame.map.forward_eval();
            // Tier A task 11 (Z1), fix round 1 (ruling R-TA-10): a
            // per-frame constant, sampled across this frame's own native
            // geometry (NOT the reference geometry — the samples must
            // land inside the domain `deposit_band` actually indexes) —
            // see `geom::drop_bound_half_diag`'s doc for the exactness
            // argument and why it needed correcting.
            let half_diag = geom::drop_bound_half_diag(
                frame.map,
                src_width,
                src_height,
                input.drop_shrink,
                input.scale,
            );
            // Tier C item C2 (spec §3.2, ruling C-3): with no distortion
            // layer the mapped drop is ONE parallelogram for the whole
            // frame, so its overlap areas tabulate once here. Taken at the
            // frame's own CENTRE rather than at its origin: for a
            // Similarity/Affine map the position does not matter at all
            // (the Jacobian is position-independent), and for a Homography
            // — the pipeline's default registration output — it centres
            // the projective row's variation instead of leaving it
            // one-sided. A refused build (a degenerate local mapping)
            // falls back to the exact clip, never to a wrong table.
            let square_table = (input.kernel == DrizzleKernel::Square
                && !input.force_exact_overlap
                && phase_table_applies(input.scale, input.drop_shrink)
                && frame.map.distortion.is_none())
            .then(|| {
                geom::map_drop_at(
                    frame.map,
                    (src_width as f64 - 1.0) / 2.0,
                    (src_height as f64 - 1.0) / 2.0,
                    input.drop_shrink,
                    input.scale,
                )
                .and_then(|q| PhaseTable::build(&q, input.scale, phase_table::PHASES))
            })
            .flatten();
            let overlap = if input.kernel != DrizzleKernel::Square
                || input.force_exact_overlap
                || !phase_table_applies(input.scale, input.drop_shrink)
            {
                SquareOverlap::Exact
            } else if let Some(table) = square_table.as_ref() {
                SquareOverlap::Frame(table)
            } else if frame.map.distortion.is_some() {
                SquareOverlap::Tiled
            } else {
                // A linear map whose table refused to build.
                SquareOverlap::Exact
            };
            plane_overlap_modes[match overlap {
                SquareOverlap::Exact => 0,
                SquareOverlap::Frame(_) => 1,
                SquareOverlap::Tiled => 2,
            }] += 1;

            let ctx = FrameDepositCtx {
                src: &src,
                src_width,
                src_height,
                ref_width: input.width,
                ref_height: input.height,
                map: frame.map,
                fwd,
                half_diag,
                scale: input.scale,
                drop_shrink: input.drop_shrink,
                kernel: input.kernel,
                kernel_table: kernel_table.as_ref(),
                rej: rej_bitmap.as_ref(),
                ln: has_ln.then(|| (&a_plane_buf[..], &b_plane_buf[..])),
                pair,
                w,
                plane: c,
                cfa: frame.cfa.map(|s| s.pattern),
                overlap,
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
            let deposit_elapsed = deposit_start.elapsed();
            deposit_duration_total += deposit_elapsed;
            plane_deposit += deposit_elapsed;
            // Ruling R-T4-6c: this frame's pixel work is done for this
            // plane, so its displacement grid goes back now. The plane
            // loop is the OUTER one, so a frame's next plane is separated
            // from this one by every other frame's deposit — holding the
            // grid until then is exactly the "every frame's grid alive at
            // once" shape that made a `tps` drizzle thrash. An OSC frame
            // therefore rebuilds its forward grid once per plane; that is
            // the documented cost of the spline arm.
            drop(ctx);
            released_bytes += frame.map.release_grids() as u64;

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
            read_ms = plane_read.as_millis() as u64,
            deposit_ms = plane_deposit.as_millis() as u64,
            deposit_mode = overlap_mode_label(&plane_overlap_modes),
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
        grid_bytes_released = released_bytes,
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
        // A few hundred probes per band, and in the OPPOSITE direction
        // from the deposit that follows (which only ever asks `forward`).
        // Through the grid path this would build the whole INVERSE
        // displacement grid for a direction drizzle never uses again —
        // ruling R-T4-3a/c.
        let (sx, sy) = map.inverse_exact(rx, ry);
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
    // Tier A task 11 (Z1): the band's own continuous row extent in output
    // coordinates — pixel row `y0` covers `[y0 - 0.5, y0 + 0.5]` and the
    // band's last row is `y1 - 1` (this `y1` is one PAST the last row),
    // whose upper edge is `(y1 - 1) + 0.5 = y1 - 0.5`. A pixel's mapped
    // drop is skipped only when it PROVABLY cannot reach this span at all
    // — see the derivation on the skip check below.
    let band_y_lo = y0 as f64 - 0.5;
    let band_y_hi = y1 as f64 - 0.5;

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

    // Tier C item C2 (ruling C-3): [`SquareOverlap::Tiled`]'s per-tile
    // tables, one tile ROW at a time (see the arm below). Allocated lazily
    // — the other two arms never touch either of these.
    let tiles_x = ctx.src_width.div_ceil(phase_table::TILE).max(1);
    let mut tile_row = usize::MAX;
    let mut tile_cache: Vec<TileSlot> = Vec::new();

    for y in sy0..=sy1 {
        let row_off = y * ctx.src_width;
        for x in sx0..=sx1 {
            // M4d Task 1 (ruling R-M4d-2): under Bayer drizzle `ctx.src` is
            // the whole mosaic, so this plane only takes the pixels of its
            // own colour — every other one belongs to a different plane's
            // pass over the same file. Checked before the sample is even
            // read: it is a two-bit test, and three quarters of the pixels
            // fail it on the R and B passes.
            if let Some(pattern) = ctx.cfa {
                if geom::cfa_plane_of(pattern, x, y) != ctx.plane {
                    continue;
                }
            }
            let d = ctx.src[row_off + x];
            if !d.is_finite() || d == 0.0 {
                continue;
            }
            let (u, v) = ctx.fwd.at(x as f64, y as f64);
            if !u.is_finite() || !v.is_finite() {
                continue;
            }
            // Tier A task 11 (Z1), fix round 1 (ruling R-TA-10): this
            // pixel's mapped drop cannot possibly touch this band's row
            // range at all — skip before any per-pixel side effect below
            // (the rejection-bitmap lookup, the LN grid, either kernel's
            // dispatch). `oy` is the mapped pixel CENTRE, i.e. the drop's
            // own mapped centroid (exactly, for a Similarity/Affine map —
            // `geom::drop_bound_half_diag`'s doc; a measured bound for
            // Homography/distortion); the quad's continuous y-extent is
            // therefore contained in `[oy - ctx.half_diag, oy +
            // ctx.half_diag]`, and that interval has no overlap with
            // `[band_y_lo, band_y_hi]` exactly when `geom::band_skip`
            // (below) says so — the same "entirely above" / "entirely
            // below" split `band_source_window`'s own `None` case uses,
            // just on the tight per-pixel interval instead of the coarse
            // per-band probe. `band_skip` is its own function (not
            // inlined) so the exhaustive skip-safety test calls this
            // EXACT code, not a hand-copied restatement of it.
            let oy = geom::to_output(v, ctx.scale);
            if geom::band_skip(oy, ctx.half_diag, band_y_lo, band_y_hi) {
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
                    // Tier C item C2 (spec §3.2, ruling C-3): the three
                    // arms differ ONLY in where `(cells, areas)` come
                    // from. Everything that decides WHAT is deposited —
                    // the CFA colour routing, the `.rej` verdict, the LN
                    // grid or the global pair, the frame weight `wf` and
                    // the finite guards — is resolved above this match and
                    // applied once; both arms end in the same
                    // `deposit_cell`.
                    let wf = ctx.w as f64;
                    match ctx.overlap {
                        SquareOverlap::Exact => {
                            deposit_exact(ib, wb, out_w, y0, y1, ctx, x, y, wf, nd);
                        }
                        SquareOverlap::Frame(table) => {
                            let ox = geom::to_output(u, ctx.scale);
                            deposit_tabulated(ib, wb, out_w, y0, y1, table, ox, oy, wf, nd);
                        }
                        SquareOverlap::Tiled => {
                            // The tile the local Jacobian is taken at. The
                            // cache holds a whole tile ROW: this scan is
                            // row-major, so caching only the CURRENT tile
                            // would rebuild every column's table once per
                            // SOURCE ROW (≈ 25 rebuilds a row on a 6 k-wide
                            // frame) instead of once per tile. `y` only
                            // ever increases, so a changed tile row can
                            // drop the whole previous row.
                            let ty = y / phase_table::TILE;
                            if ty != tile_row {
                                tile_row = ty;
                                tile_cache.clear();
                                tile_cache.resize_with(tiles_x, || TileSlot::Empty);
                            }
                            let tx = (x / phase_table::TILE).min(tiles_x - 1);
                            if matches!(tile_cache[tx], TileSlot::Empty) {
                                let cx = (tx * phase_table::TILE + phase_table::TILE / 2)
                                    .min(ctx.src_width.saturating_sub(1))
                                    as f64;
                                let cy = (ty * phase_table::TILE + phase_table::TILE / 2)
                                    .min(ctx.src_height.saturating_sub(1))
                                    as f64;
                                tile_cache[tx] = match geom::map_drop_at(
                                    ctx.map,
                                    cx,
                                    cy,
                                    ctx.drop_shrink,
                                    ctx.scale,
                                )
                                .and_then(|q| PhaseTable::build(&q, ctx.scale, phase_table::PHASES))
                                {
                                    Some(t) => TileSlot::Built(t),
                                    None => TileSlot::Refused,
                                };
                            }
                            match &tile_cache[tx] {
                                TileSlot::Built(table) => {
                                    let ox = geom::to_output(u, ctx.scale);
                                    deposit_tabulated(ib, wb, out_w, y0, y1, table, ox, oy, wf, nd);
                                }
                                // A degenerate local mapping: this tile
                                // keeps the exact clip rather than
                                // depositing nothing.
                                TileSlot::Empty | TileSlot::Refused => {
                                    deposit_exact(ib, wb, out_w, y0, y1, ctx, x, y, wf, nd);
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
                        let (u2, v2) = ctx.fwd.at(x as f64 + dx, y as f64 + dy);
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

    /// Fix round 1 (controller ruling R-TA-10): a pure projective warp —
    /// `u = x / w(x, y)`, `v = y / w(x, y)`, `w = m20*x + m21*y + 1` — with
    /// no rotation/translation, isolating exactly the property the
    /// single-origin-drop version got wrong: `w`'s dependence on position
    /// makes the LOCAL Jacobian (and hence the drop's mapped shape) vary
    /// across the frame, unlike any Similarity/Affine map. `m20`/`m21`
    /// here are two orders of magnitude past the LDN 1272 checkpoint's own
    /// real homography fits (`m[2][0]`/`m[2][1]` around 1e-8..1e-7 at
    /// 6000-px scale), scaled up for this 64x48 fixture so `w` swings by
    /// several percent across it: at the far corner `(W-1, H-1) = (63,
    /// 47)`, `w = 5e-4*63 + 5e-4*47 + 1 = 1.055` — a 5.5% variation, far
    /// beyond anything a real fit produces.
    fn homography_with_projective_row(m20: f64, m21: f64) -> PixelMap {
        PixelMap::linear(Linear {
            kind: LinearKind::Homography,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [m20, m21, 1.0]],
        })
        .unwrap()
    }

    /// Fix round 1 (controller ruling R-TA-10): a thin-plate-spline
    /// distortion layer with AGGRESSIVE node displacement (several pixels
    /// across this 64x48 fixture — the node amplitudes below reach ~4.5 px
    /// in x and ~4.0 px in y) on top of the identity linear part, so the
    /// distortion's own local Jacobian genuinely varies node to node —
    /// exactly the property `drop_bound_half_diag`'s sampled-max must
    /// survive via its margin. The "inverse" spline is fit on the
    /// negated displacements (an approximation, not an exact inverse —
    /// fine here since the skip predicate and this test only ever drive
    /// the FORWARD direction), the same pattern
    /// `dropping_a_source_hands_back_its_frames_displacement_grids`
    /// (`integration/registered_source.rs`) uses for its own spline
    /// fixture.
    fn tps_map_with_aggressive_distortion() -> PixelMap {
        use crate::geometry::{DistortionModel, ThinPlateSpline};

        let nodes: Vec<(f64, f64)> = vec![
            (8.0, 8.0),
            (32.0, 8.0),
            (56.0, 8.0),
            (8.0, 24.0),
            (32.0, 24.0),
            (56.0, 24.0),
            (8.0, 40.0),
            (32.0, 40.0),
            (56.0, 40.0),
        ];
        let dx: Vec<f64> = nodes
            .iter()
            .map(|&(x, y)| 4.0 * (x / 20.0).sin() + 0.5 * (y / 15.0).cos())
            .collect();
        let dy: Vec<f64> = nodes
            .iter()
            .map(|&(x, y)| 3.5 * (y / 18.0).cos() - 0.5 * (x / 25.0).sin())
            .collect();
        let ndx: Vec<f64> = dx.iter().map(|v| -v).collect();
        let ndy: Vec<f64> = dy.iter().map(|v| -v).collect();
        let forward = ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).expect("9-node fit");
        let inverse = ThinPlateSpline::fit(&nodes, &ndx, &ndy, 0.0).expect("9-node fit");
        let model = DistortionModel::tps(forward, inverse, [0.0, 0.0, W as f64, H as f64]);
        PixelMap::with_distortion_model(Linear::identity(), model).unwrap()
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
            cfa: None,
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
            force_exact_overlap: false,
        }
    }

    /// Ruling R-T4-6c/d: after `drizzle_group` returns, no frame's map
    /// still holds a displacement grid — observed on the CALLER's own
    /// maps, which are the clones the run keeps until it ends.
    ///
    /// Drizzle is the stage that broke the acceptance run: it built a
    /// FORWARD grid per frame on top of the inverse grids registration had
    /// already left alive. This is the pin that would fail if its release
    /// were removed.
    #[test]
    fn drizzle_hands_back_every_frames_displacement_grid() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        use crate::geometry::{DistortionModel, ThinPlateSpline};

        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.25);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &data);

        let spline_map = || {
            let nodes: Vec<(f64, f64)> = (0..36)
                .map(|i| {
                    (
                        8.0 + (i % 6) as f64 * (W as f64 - 16.0) / 5.0,
                        8.0 + (i / 6) as f64 * (H as f64 - 16.0) / 5.0,
                    )
                })
                .collect();
            let dx: Vec<f64> = nodes.iter().map(|(x, _)| 0.2 * (x / 30.0).sin()).collect();
            let dy: Vec<f64> = nodes.iter().map(|(_, y)| 0.15 * (y / 25.0).cos()).collect();
            let ndx: Vec<f64> = dx.iter().map(|v| -v).collect();
            let ndy: Vec<f64> = dy.iter().map(|v| -v).collect();
            let model = DistortionModel::tps(
                ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).unwrap(),
                ThinPlateSpline::fit(&nodes, &ndx, &ndy, 0.0).unwrap(),
                [0.0, 0.0, W as f64, H as f64],
            );
            PixelMap::with_distortion_model(Linear::identity(), model).unwrap()
        };
        let (m0, m1) = (spline_map(), spline_map());
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [
            frame(&p0, &m0, &weight, &pair),
            frame(&p1, &m1, &weight, &pair),
        ];
        let measure = MeasureOptions::default();
        let input = base_input(&frames, &measure);

        crate::geometry::pixel_map::grid_counters::reset();
        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        assert!(out.stats.coverage[0] > 0.5, "the deposit really ran");

        for (i, m) in [&m0, &m1].iter().enumerate() {
            assert_eq!(
                m.distortion.as_ref().unwrap().grids_built(),
                (false, false),
                "frame {i}'s grid outlived the deposit"
            );
        }
        // v0.6.3: the release is per FRAME, not per call — the run pin
        // (`a_tps_run_never_holds_more_than_a_few_displacement_grids_at_once`)
        // bounds the peak by the spline-frame count, which integration
        // legitimately reaches, so a drizzle that held every forward grid
        // until its plane loop ended would hide under it. This is where
        // that regression shows: two frames, one grid alive at a time.
        let (builds, _alive, peak) = crate::geometry::pixel_map::grid_counters::snapshot();
        assert_eq!(builds, 2, "one forward grid per frame");
        assert_eq!(
            peak, 1,
            "drizzle must release each frame's grid before the next"
        );
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

        input.write_weight_map = true;

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let out_w = W * 2;
        for &(x, y) in &[(20usize, 20usize), (21, 20), (20, 21), (21, 21)] {
            let v = out.data[y * out_w + x];
            assert!((v - 1.0).abs() < 1e-6, "x={x} y={y} v={v}");
        }
        // Tier C item C2 (ruling C-3): the ring around the block is no
        // longer EMPTY. This fixture's identity map puts every mapped drop
        // centre at an exact half-pixel phase — the single worst case for
        // the phase table, whose nearest bin centre is 1/64 of an output
        // pixel away — so a sliver of the drop's own area lands one pixel
        // further out than the exact clip put it. What the pin protects is
        // unchanged and now stated directly: essentially ALL of the drop's
        // mass is still on the four-pixel block, and the ring's share of
        // it is ≤ 2 % (measured 1.6 % on the edge-adjacent pixels, 0.03 %
        // on the diagonal ones — spec §3.2's own "≤ 1.6 % per-frame weight
        // change" figure, arrived at independently here). The VALUE there
        // is still exactly 1.0: a sliver carries the same source pixel,
        // not a different one.
        let weight = out.weight.as_ref().expect("write_weight_map was on");
        let block_weight = weight[20 * out_w + 20];
        assert!(block_weight > 0.0);
        for &(x, y) in &[(19usize, 20usize), (22, 20), (20, 19), (20, 22), (19, 19)] {
            let w = weight[y * out_w + x];
            assert!(
                w <= 0.02 * block_weight,
                "x={x} y={y} weight={w} is more than a 2 % sliver of the block's {block_weight}"
            );
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
            cfa: None,
        };
        let rej1 = set.path(1).to_path_buf();
        let f1 = DrizzleFrame {
            path: &p1,
            map: &map,
            weight: &weight,
            output_pair: &pair,
            ln: None,
            rej: Some(&rej1),
            cfa: None,
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
            cfa: None,
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
            cfa: None,
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

    // ── Tier A task 11 (Z1 band skip, Z5 one-read-per-frame): a
    // bit-identity pin recorded against the unmodified driver
    // (`b55cd3a3`/`65619f0c`, before this task's changes) — see the task
    // report for how the expected checksums were captured. A FNV-1a-style
    // fold of every output f32's `to_bits()` so a single flipped bit
    // anywhere in `data`/`weight` changes the number, without needing a
    // golden file on disk.
    fn checksum_f32(data: &[f32]) -> u64 {
        data.iter().fold(0xcbf29ce484222325u64, |acc, v| {
            (acc ^ v.to_bits() as u64).wrapping_mul(0x100000001b3)
        })
    }

    #[test]
    fn drizzle_group_output_is_bit_identical_across_rotations() {
        let dir = tempfile::tempdir().unwrap();
        let stars = [
            (10.0, 12.0, 0.6),
            (40.0, 8.0, 0.35),
            (55.0, 40.0, 0.5),
            (5.0, 44.0, 0.2),
            (30.0, 24.0, 0.8),
        ];
        let data0 = gaussian_field(W, H, &stars, 1.4, 0.02);
        let data1 = gaussian_field(W, H, &stars, 1.1, 0.015);
        let p0 = write_mono(dir.path(), "f0.fits", W, H, &data0);
        let p1 = write_mono(dir.path(), "f1.fits", W, H, &data1);
        let cx = (W as f64 - 1.0) / 2.0;
        let cy = (H as f64 - 1.0) / 2.0;
        let w0 = [1.0f64];
        let w1 = [0.8f64];
        let pair = identity_pair();

        // Recorded against the unmodified driver; re-run after Z1/Z5 to
        // confirm the checksums do not move (task 11 report has the run
        // log for both sides).
        //
        // RE-PINNED, Tier C item C2 (ruling C-3): the deposit now takes a
        // drop's overlap areas from a per-phase table instead of clipping
        // every source pixel exactly, which rounds each drop's sub-pixel
        // phase to 1/64 of an output pixel — a deliberate NUMERIC change
        // (Tier C is the numeric tier; Tier A's own bit-identity is what
        // this pin used to carry). The three pre-C2 pairs were
        // `(0x8cb227895e93f504, 0xcf5743015bcedb75)`,
        // `(0x9427361e0439f161, 0x82f2ad1c84dd72c6)` and
        // `(0x502542b82e474955, 0x2a811e20249d5d31)`; the SIZE of the move
        // is pinned separately and quantitatively by
        // `the_phase_table_tracks_the_exact_clip_on_a_rotated_frame`. What
        // this pin still guards is unchanged and is why it is not deleted:
        // the driver must be DETERMINISTIC — same input, same bytes, band
        // parallelism and all (both values below were captured twice, on
        // separate runs, identical).
        //
        // Re-pinned once more in that item's fix round 1 (ruling C-21),
        // which took `phase_table::PHASES` from 32 to 64 — halving the
        // phase residual moves every tabulated pixel again. The
        // intermediate `PHASES = 32` triple, recorded here only so a
        // bisect can tell the two moves apart, was
        // `(0x55b2eb71dd060899, 0xf91233f30df26760)`,
        // `(0x830588d5d083ad47, 0xb4c714f38cdb3dc9)`,
        // `(0x8e4c7c798be423c9, 0xc2cd2f6c50af020e)`.
        let expected: [(u64, u64); 3] = [
            (0x81187c97b6136e5a, 0x9b0d3f76db4c3243), // 1 deg
            (0x0c4e834ed96964b7, 0x03253ee8448a38ca), // 5 deg
            (0x2058a2d2f620d9be, 0x9bf82f2ff73be09c), // 30 deg
        ];

        for (deg, (want_data, want_weight)) in [1.0_f64, 5.0, 30.0].into_iter().zip(expected) {
            let map0 = rotation_about_centre(deg, cx, cy);
            let map1 = rotation_about_centre(deg + 2.0, cx, cy);
            let f0 = frame(&p0, &map0, &w0, &pair);
            let f1 = frame(&p1, &map1, &w1, &pair);
            let frames = [f0, f1];
            let measure = MeasureOptions::default();
            let mut input = base_input(&frames, &measure);
            input.scale = 2;
            input.drop_shrink = 0.7;

            let out =
                drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
            let got_data = checksum_f32(&out.data);
            let got_weight = checksum_f32(out.weight.as_ref().unwrap());
            assert_eq!(got_data, want_data, "{deg}deg data checksum");
            assert_eq!(got_weight, want_weight, "{deg}deg weight checksum");
        }
    }

    // ── Tier A task 11 (Z1): exhaustive skip-predicate safety. For every
    // source pixel of the fixture, under each of five maps (fix round 1,
    // ruling R-TA-10: the three original rotations plus a homography with
    // a stress-case projective row and a TPS with aggressive node
    // displacement — the two arms the single-origin-drop version got
    // wrong), `deposit_band`'s early skip must never reject a pixel whose
    // drop's clipped area with the band is actually nonzero — checked by
    // replaying the SAME Square-kernel clip path (`geom::drop_corners` →
    // `geom::map_drop` → `geom::clip_area`) the driver itself uses, over a
    // band narrower than the frame so both "clearly touches" and "clearly
    // misses" pixels exist. Calls `geom::drop_bound_half_diag` and
    // `geom::band_skip` — the REAL functions `deposit_band` runs, not a
    // hand-copied restatement of either. ──

    #[test]
    fn deposit_skip_predicate_never_rejects_a_pixel_with_nonzero_clipped_area() {
        const DROP_SHRINK: f64 = 0.9;
        const SCALE: u32 = 2;
        let out_w = W * SCALE as usize;
        let out_h = H * SCALE as usize;
        // A sub-band, not the whole frame's output extent: at least one of
        // the five maps must leave some source pixels whose drop maps
        // entirely outside it, or the predicate is never exercised.
        let y0 = 0usize;
        let rows = out_h / 3;
        let y1 = y0 + rows;
        let band_y_lo = y0 as f64 - 0.5;
        let band_y_hi = y1 as f64 - 0.5;

        let cx = W as f64 / 2.0;
        let cy = H as f64 / 2.0;
        let cases: [(&str, PixelMap); 5] = [
            ("1deg", rotation_about_centre(1.0, cx, cy)),
            ("5deg", rotation_about_centre(5.0, cx, cy)),
            ("30deg", rotation_about_centre(30.0, cx, cy)),
            (
                "homography_stress",
                homography_with_projective_row(5e-4, 5e-4),
            ),
            ("tps_stress", tps_map_with_aggressive_distortion()),
        ];

        for (name, map) in &cases {
            // The per-frame bound the real driver computes once — sampled
            // across THIS frame's own native geometry, matching
            // `drizzle_group`'s own call (`src_width`/`src_height`, not
            // the reference geometry).
            let half_diag = geom::drop_bound_half_diag(map, W, H, DROP_SHRINK, SCALE);
            // The grid-cached evaluator `deposit_band` actually calls per
            // pixel (`ctx.fwd.at`) — NOT `forward_exact`, which only
            // `drop_bound_half_diag`'s sampling uses.
            let fwd = map.forward_eval();

            let mut examined = 0usize;
            let mut skipped = 0usize;
            let mut wrongly_skipped = 0usize;

            for y in 0..H {
                for x in 0..W {
                    examined += 1;
                    let (u, v) = fwd.at(x as f64, y as f64);
                    assert!(u.is_finite() && v.is_finite(), "{name}: x={x} y={y}");
                    let oy = geom::to_output(v, SCALE);
                    let would_skip = geom::band_skip(oy, half_diag, band_y_lo, band_y_hi);

                    // The actual clipped area this pixel's drop contributes
                    // to the band, via the exact same path `deposit_band`'s
                    // Square-kernel arm uses.
                    let corners = geom::drop_corners(x, y, DROP_SHRINK);
                    let mut total_area = 0.0_f64;
                    if let Some((quad, bbox)) = geom::map_drop(&fwd, &corners, SCALE) {
                        let (bx0, by0, bx1, by1) = bbox;
                        let px0 = bx0.max(0);
                        let px1 = bx1.min(out_w as i64 - 1);
                        let py0 = by0.max(y0 as i64);
                        let py1 = by1.min(y1 as i64 - 1);
                        for py in py0..=py1 {
                            for px in px0..=px1 {
                                total_area += geom::clip_area(&quad, px, py);
                            }
                        }
                    }

                    if would_skip {
                        skipped += 1;
                        if total_area > 0.0 {
                            wrongly_skipped += 1;
                        }
                    }
                }
            }

            assert_eq!(
                wrongly_skipped, 0,
                "{name}: {wrongly_skipped} of {examined} examined pixels wrongly skipped \
                 ({skipped} skipped in total)"
            );
            assert!(
                skipped > 0,
                "{name}: expected some skips over a {rows}-row sub-band of {out_h}, got 0 \
                 (the predicate is not being exercised)"
            );
        }
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
            force_exact_overlap: false,
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
            force_exact_overlap: false,
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
        //
        // Tier C item C2 (ruling C-3): the interior reference row is now
        // chosen with the SAME PARITY as the seam row under test. This
        // fixture's identity map puts every drop at an exact half-pixel
        // phase, whose nearest phase-table bin centre is 1/64 of an output
        // pixel away, and that residual splits a drop's area 0.884/0.916
        // between the two output rows it covers instead of 0.9/0.9 — so
        // even and odd output rows carry systematically different raw
        // weight (by 3.6 %) EVERYWHERE, seam or not. That is the
        // documented phase quantization (spec §3.2's "≤ 1.6 % per-frame
        // weight change", per axis), not a seam defect, and it is
        // invisible in `I / W` — which the flatness check above already
        // pins to 1e-6 across every row. What this check exists for — a
        // band boundary must neither DOUBLE a row's deposit nor DROP it —
        // is unchanged and still caught: a doubled row reads 2x its
        // parity-mate's weight and a dropped one reads 0.
        let weight_map = out.weight.as_ref().expect("write_weight_map was on");
        let x = 20usize;
        for &seam in &[
            510usize, 511, 512, 513, 1022, 1023, 1024, 1025, 1534, 1535, 1536, 1537,
        ] {
            let reference = weight_map[(10 + seam % 2) * out_w + x];
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
            cfa: None,
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
            cfa: None,
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
            fwd: map.forward_eval(),
            half_diag: geom::drop_bound_half_diag(&map, width, height, drop_shrink, scale),
            scale,
            drop_shrink,
            kernel: DrizzleKernel::Square,
            kernel_table: None,
            rej: None,
            cfa: None,
            ln: None,
            pair,
            w: 1.0,
            plane: 0,
            // Tier C item C2: this pin measures the RAW mass the exact
            // clip deposits against the tabulated kernel's, so it stays on
            // the exact arm — the phase table's own mass invariant is
            // pinned separately (`every_phase_row_sums_to_the_drops_own_area`).
            overlap: SquareOverlap::Exact,
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
            fwd: map.forward_eval(),
            half_diag: geom::drop_bound_half_diag(&map, width, height, drop_shrink, scale),
            scale,
            drop_shrink,
            kernel: DrizzleKernel::Circle,
            kernel_table: Some(&table),
            rej: None,
            cfa: None,
            ln: None,
            pair,
            w: 1.0,
            plane: 0,
            // Tier C item C2: this pin measures the RAW mass the exact
            // clip deposits against the tabulated kernel's, so it stays on
            // the exact arm — the phase table's own mass invariant is
            // pinned separately (`every_phase_row_sums_to_the_drops_own_area`).
            overlap: SquareOverlap::Exact,
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
            // Tier C item C2 (ruling C-3): 1e-2, not 1e-4. This fixture's
            // maps put every drop at an exact half-pixel phase — the phase
            // table's own worst case — so a 1.6 % sliver of the marker
            // pixel's mass lands outside the block and the block's
            // weighted mean moves by ≈ 0.7 % (0.4599 against 0.4667). The
            // pin's discriminating power is untouched: the failure it
            // exists to catch reads `background` = 0.25 here, 0.217 away
            // from `expected`, i.e. 21x this tolerance.
            assert!(
                (v - expected).abs() < 1e-2,
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

    // ── Cross-scale pin (M4b Task 4, ruling R-M4b-6): the SAME ×2
    // registration map the resampler pin uses
    // (`integration::registered_source::tests`) — a coarse subject
    // drizzled onto a finer reference. The forward-mapped drop already
    // handles any registration scale through the existing `PixelMap`/
    // `geom::clip_area` machinery: at drizzle scale 1× the drop is a
    // ~2×2 output-pixel footprint, ~4×4 at drizzle scale 2× (the ×2
    // registration scale composed with the drizzle output scale) — no new
    // code needed for mixed pixel scales. ──

    #[test]
    fn a_coarse_frame_drizzles_level_preserving_at_a_double_registration_scale() {
        const SUB_W: usize = 200;
        const SUB_H: usize = 150;
        const REF_W: usize = 400;
        const REF_H: usize = 300;
        const LEVEL: f32 = 0.25;

        // Subject → reference: the same pure ×2 registration scale as the
        // resampler pin — no rotation, no translation.
        let fwd = Linear {
            kind: LinearKind::Affine,
            m: [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]],
        };

        for &scale in &[1u32, 2u32] {
            let dir = tempfile::tempdir().unwrap();
            let data = uniform(SUB_W, SUB_H, LEVEL);
            let p0 = write_mono(dir.path(), "f0.fits", SUB_W, SUB_H, &data);
            let map = PixelMap::linear(fwd).unwrap();
            let weight = [1.0f64];
            let pair = identity_pair();
            let frames = [frame(&p0, &map, &weight, &pair)];
            let measure = MeasureOptions::default();
            let input = DrizzleInput {
                frames: &frames,
                width: REF_W,
                height: REF_H,
                channels: 1,
                scale,
                drop_shrink: 1.0, // exact quad, no shrink
                kernel: DrizzleKernel::Square,
                use_weights: true,
                use_rejection: false,
                use_local_normalization: false,
                write_weight_map: true,
                measure: &measure,
                ram_total_bytes: None,
                force_exact_overlap: false,
            };

            let out =
                drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();

            let out_w = REF_W * scale as usize;
            let out_h = REF_H * scale as usize;
            assert_eq!((out.width, out.height), (out_w, out_h));
            let weight_map = out.weight.as_ref().expect("write_weight_map was on");

            // "coverage 1.0 inside the mapped rectangle": every output
            // pixel a couple of pixels in from every edge (clear of the
            // subject's own domain boundary, where a drop's overshoot or
            // shortfall can leave a partial-weight sliver) must have
            // positive weight — the ×2-scaled drops gaplessly tile the
            // reference exactly like an identity-map drizzle does.
            let margin = 2usize;
            let mut checked = 0usize;
            for y in margin..out_h - margin {
                for x in margin..out_w - margin {
                    let idx = y * out_w + x;
                    let w = weight_map[idx];
                    assert!(w > 0.0, "scale={scale} x={x} y={y}: uncovered (W=0)");
                    checked += 1;

                    // "I/W = 0.25 within 1e-5 wherever W > 0" — level-
                    // preserving across the ×2 registration scale.
                    let v = out.data[idx] as f64;
                    assert!(
                        (v - LEVEL as f64).abs() < 1e-5,
                        "scale={scale} x={x} y={y} v={v}"
                    );
                }
            }
            assert!(checked > 0, "scale={scale}: the interior region was empty");
        }
    }

    // ── LN cross-scale pin (M4b Task 4 fix round 1, review finding I1,
    // ruling R-M4b-6): `local_normalization_index_is_not_transposed` above
    // only ever exercises `deposit_band`'s LN lookup at an IDENTITY map,
    // where "source pixel" and "reference pixel" are the same coordinate
    // and cannot distinguish a correct reference-coordinate lookup from a
    // hypothetical source-coordinate one. This drives the SAME real
    // `deposit_band` code (`ctx.map.forward`, `round_half_up`,
    // `a_plane[iy * ctx.ref_width + ix]`, ~L796-831 above) through the ×2
    // registration map Steps 1/2 use, where the two coordinates genuinely
    // differ, so the assertion below fails if that lookup ever indexed the
    // grid by the untransformed source pixel instead of the forward-mapped
    // reference one. ──

    #[test]
    fn a_coarse_frame_applies_local_normalization_at_the_mapped_reference_cell_not_the_source_one()
    {
        const SUB_W: usize = 200;
        const SUB_H: usize = 150;
        const REF_W: usize = 400;
        const REF_H: usize = 300;
        const VALUE: f32 = 0.2;

        let dir = tempfile::tempdir().unwrap();
        let data = uniform(SUB_W, SUB_H, VALUE);
        let p0 = write_mono(dir.path(), "f0.fits", SUB_W, SUB_H, &data);

        // The same ×2 registration map as the level/coverage pin above and
        // the resampler pin (`integration::registered_source::tests`):
        // subject (100, 75) forward-maps to reference (200, 150).
        let fwd = Linear {
            kind: LinearKind::Affine,
            m: [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]],
        };
        let map = PixelMap::linear(fwd).unwrap();
        let weight = [1.0f64];
        let pair = identity_pair();

        // `b` ramps by reference NODE column (stride 32 at scale 256), 0.1
        // per node — the same ramp `local_normalization_index_is_not_transposed`
        // above uses, built this time in the 400×300 REFERENCE geometry
        // rather than the (identity-mapped) 64×48 source/reference the
        // other test shares.
        let mut grid = LnGrid::constant(REF_W, REF_H, 256, 1.0, 0.0);
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
            cfa: None,
        };
        let frames = [f_ln];
        let measure = MeasureOptions::default();
        let input = DrizzleInput {
            frames: &frames,
            width: REF_W,
            height: REF_H,
            channels: 1,
            scale: 1, // output pixel == reference pixel, exactly
            drop_shrink: 1.0,
            kernel: DrizzleKernel::Square,
            use_weights: true,
            use_rejection: false,
            use_local_normalization: true,
            write_weight_map: true,
            measure: &measure,
            ram_total_bytes: None,
            force_exact_overlap: false,
        };

        // Ground truth: the SAME evaluator `deposit_band` calls, run
        // independently — once at the MAPPED reference row (150, what a
        // correct lookup reads) and once at the raw SOURCE row (75, what a
        // source-coordinate bug would read).
        let mut scratch = LnScratch::for_grid(&grid);
        let (mut a_ref_row, mut b_ref_row) = (vec![0f32; REF_W], vec![0f32; REF_W]);
        grid.evaluate_row_into(150, &mut a_ref_row, &mut b_ref_row, &mut scratch);
        let (mut a_src_row, mut b_src_row) = (vec![0f32; REF_W], vec![0f32; REF_W]);
        grid.evaluate_row_into(75, &mut a_src_row, &mut b_src_row, &mut scratch);
        let expected_at_reference_cell = a_ref_row[200] * VALUE + b_ref_row[200];
        let expected_if_indexed_by_source = a_src_row[100] * VALUE + b_src_row[100];
        assert!(
            (expected_at_reference_cell - expected_if_indexed_by_source).abs() > 0.01,
            "not vacuous: the reference-cell and would-be source-cell predictions \
             must differ ({expected_at_reference_cell} vs {expected_if_indexed_by_source})"
        );

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();

        // Subject (100, 75)'s drop (drop_shrink=1.0) lands entirely inside
        // output pixel (200, 150) at drizzle scale=1 — the only subject
        // pixel that reaches it — so that output pixel's value is exactly
        // this one deposit's normalized sample, nothing blended in.
        let actual = out.data[150 * REF_W + 200] as f64;
        assert!(
            (actual - expected_at_reference_cell as f64).abs() < 1e-4,
            "actual={actual} expected(reference cell)={expected_at_reference_cell} \
             expected(if indexed by source cell)={expected_if_indexed_by_source}"
        );
        assert!(
            (actual - expected_if_indexed_by_source as f64).abs() > 0.01,
            "actual={actual} must NOT match the source-indexed prediction \
             {expected_if_indexed_by_source} — that would mean the lookup used the \
             untransformed source coordinate"
        );
    }

    // ── Bayer drizzle (M4d Task 1, ruling R-M4d-2) ────────────────────────

    /// A mosaic whose every R site is `r`, every G site `g` and every B
    /// site `b` — the constant-per-colour fixture the level pin needs.
    fn rggb_mosaic(w: usize, h: usize, r: f32, g: f32, b: f32) -> Vec<f32> {
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = match geom::cfa_plane_of(BayerPattern::Rggb, x, y) {
                    0 => r,
                    1 => g,
                    _ => b,
                };
            }
        }
        data
    }

    fn cfa_frame<'a>(
        debayered: &'a std::path::Path,
        mosaic: &'a std::path::Path,
        map: &'a PixelMap,
        weight: &'a [f64],
        pair: &'a [NormalizationPair],
    ) -> DrizzleFrame<'a> {
        DrizzleFrame {
            path: debayered,
            map,
            weight,
            output_pair: pair,
            ln: None,
            rej: None,
            cfa: Some(CfaSource {
                path: mosaic,
                pattern: BayerPattern::Rggb,
            }),
        }
    }

    fn cfa_input<'a>(
        frames: &'a [DrizzleFrame<'a>],
        measure: &'a MeasureOptions,
        scale: u32,
    ) -> DrizzleInput<'a> {
        DrizzleInput {
            frames,
            width: W,
            height: H,
            channels: 3,
            scale,
            drop_shrink: 1.0,
            kernel: DrizzleKernel::Square,
            use_weights: true,
            use_rejection: false,
            use_local_normalization: false,
            write_weight_map: true,
            measure,
            ram_total_bytes: None,
            force_exact_overlap: false,
        }
    }

    /// Step 1's level pin: the three planes read exactly the mosaic's own
    /// per-colour levels wherever they are covered, and the coverage is the
    /// mosaic's own site density (a quarter for R and B, a half for G) —
    /// each output pixel covered by exactly one same-colour source pixel at
    /// scale 1, drop 1.0, identity map.
    #[test]
    fn bayer_drizzle_deposits_each_colours_own_level_at_scale_1() {
        let dir = tempfile::tempdir().unwrap();
        let mosaic = rggb_mosaic(W, H, 0.4, 0.6, 0.2);
        let mp = write_mono(dir.path(), "c_f0.fits", W, H, &mosaic);
        // The debayered sibling exists (the run always writes it) and must
        // NOT be read at all under Bayer drizzle: filled with a level no
        // assertion below could mistake for a mosaic sample.
        let dp = dir.path().join("c_f0_d.fits");
        write_fits_f32(&dp, W, H, 3, &vec![9.0f32; W * H * 3], &[]).unwrap();

        let map = identity_map();
        let weight = [1.0f64; 3];
        let pair = [NormalizationPair::IDENTITY; 3];
        let frames = [cfa_frame(&dp, &mp, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let input = cfa_input(&frames, &measure, 1);

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        assert_eq!((out.width, out.height, out.channels), (W, H, 3));

        let plane_px = W * H;
        let weight_map = out.weight.as_ref().expect("weight map requested");
        for (plane, expect) in [(0usize, 0.4f32), (1, 0.6), (2, 0.2)] {
            let mut covered = 0usize;
            for idx in 0..plane_px {
                let w = weight_map[plane * plane_px + idx];
                let v = out.data[plane * plane_px + idx];
                if w > 0.0 {
                    covered += 1;
                    assert!(
                        (v - expect).abs() < 1e-6,
                        "plane {plane} pixel {idx}: {v} != {expect}"
                    );
                } else {
                    assert_eq!(v, 0.0, "plane {plane} pixel {idx} uncovered but non-zero");
                }
            }
            let coverage = covered as f64 / plane_px as f64;
            let want = if plane == 1 { 0.5 } else { 0.25 };
            assert!(
                (coverage - want).abs() < 1e-9,
                "plane {plane} coverage {coverage} != {want}"
            );
            assert!(
                (out.stats.coverage[plane] - want).abs() < 1e-9,
                "plane {plane} reported coverage {} != {want}",
                out.stats.coverage[plane]
            );
        }
    }

    /// The same levels hold at scale 2 — `I / W` is level-preserving across
    /// the mask exactly as it is without one (ruling R-M3-2). Coverage is
    /// NOT pinned here: how much of a 2x grid a quarter-density mosaic
    /// reaches is a measured number (Task 6), not a contract.
    #[test]
    fn bayer_drizzle_is_level_preserving_at_scale_2() {
        let dir = tempfile::tempdir().unwrap();
        let mosaic = rggb_mosaic(W, H, 0.4, 0.6, 0.2);
        let mp = write_mono(dir.path(), "c_f0.fits", W, H, &mosaic);
        let dp = dir.path().join("c_f0_d.fits");
        write_fits_f32(&dp, W, H, 3, &vec![9.0f32; W * H * 3], &[]).unwrap();

        let map = identity_map();
        let weight = [1.0f64; 3];
        let pair = [NormalizationPair::IDENTITY; 3];
        let frames = [cfa_frame(&dp, &mp, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let input = cfa_input(&frames, &measure, 2);

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let plane_px = out.width * out.height;
        let weight_map = out.weight.as_ref().expect("weight map requested");
        for (plane, expect) in [(0usize, 0.4f32), (1, 0.6), (2, 0.2)] {
            let mut covered = 0usize;
            for idx in 0..plane_px {
                if weight_map[plane * plane_px + idx] > 0.0 {
                    covered += 1;
                    let v = out.data[plane * plane_px + idx];
                    assert!(
                        (v - expect).abs() < 1e-6,
                        "plane {plane} pixel {idx}: {v} != {expect}"
                    );
                }
            }
            assert!(covered > 0, "plane {plane} deposited nothing at scale 2");
        }
    }

    /// Colour purity (the controller's second pin): a mosaic whose R sites
    /// ramp while its G and B sites stay flat must produce a ramping R
    /// plane and FLAT G/B planes. A deposit that read the debayered planes
    /// — or routed a pixel to the wrong plane — would leak the ramp into G
    /// and B, which is exactly what interpolation does.
    #[test]
    fn bayer_drizzle_keeps_each_colour_pure() {
        let dir = tempfile::tempdir().unwrap();
        let mut mosaic = rggb_mosaic(W, H, 0.4, 0.6, 0.2);
        for y in 0..H {
            for x in 0..W {
                if geom::cfa_plane_of(BayerPattern::Rggb, x, y) == 0 {
                    // 0.4 .. 0.4 + 0.63 across the frame, by column.
                    mosaic[y * W + x] = 0.4 + x as f32 * 0.01;
                }
            }
        }
        let mp = write_mono(dir.path(), "c_f0.fits", W, H, &mosaic);
        let dp = dir.path().join("c_f0_d.fits");
        write_fits_f32(&dp, W, H, 3, &vec![9.0f32; W * H * 3], &[]).unwrap();

        let map = identity_map();
        let weight = [1.0f64; 3];
        let pair = [NormalizationPair::IDENTITY; 3];
        let frames = [cfa_frame(&dp, &mp, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let input = cfa_input(&frames, &measure, 1);

        let out = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        let plane_px = W * H;
        let weight_map = out.weight.as_ref().expect("weight map requested");

        // R follows the ramp at its own sites.
        for y in (0..H).step_by(2) {
            for x in (0..W).step_by(2) {
                let idx = y * W + x;
                assert!(weight_map[idx] > 0.0, "R site ({x}, {y}) not covered");
                let expect = 0.4 + x as f32 * 0.01;
                let v = out.data[idx];
                assert!((v - expect).abs() < 1e-5, "R ({x}, {y}): {v} != {expect}");
            }
        }
        // G and B stay exactly flat wherever they are covered.
        for (plane, expect) in [(1usize, 0.6f32), (2, 0.2)] {
            for idx in 0..plane_px {
                if weight_map[plane * plane_px + idx] > 0.0 {
                    let v = out.data[plane * plane_px + idx];
                    assert!(
                        (v - expect).abs() < 1e-6,
                        "plane {plane} pixel {idx} leaked the R ramp: {v} != {expect}"
                    );
                }
            }
        }
    }

    /// A mosaic source has nowhere to route its colours in a mono group —
    /// refused up front, never silently drizzled as a luminance plane.
    #[test]
    fn bayer_drizzle_is_refused_for_a_mono_group() {
        let dir = tempfile::tempdir().unwrap();
        let mosaic = rggb_mosaic(W, H, 0.4, 0.6, 0.2);
        let mp = write_mono(dir.path(), "c_f0.fits", W, H, &mosaic);

        let map = identity_map();
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [cfa_frame(&mp, &mp, &map, &weight, &pair)];
        let measure = MeasureOptions::default();
        let input = base_input(&frames, &measure);

        match drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()) {
            Err(DrizzleError::BadInput(msg)) => {
                assert!(msg.contains("3-channel group"), "{msg}");
            }
            other => panic!("expected a BadInput refusal, got {other:?}"),
        }
    }

    // ── Tier C item C2 (spec §3.2, ruling C-3): the phase table against
    // the exact clip it replaces.
    //
    // The comparison is a DELTA, not an identity — the table rounds a
    // drop's sub-pixel phase to the centre of its `1 / PHASES` bin, so the
    // drop's mass is split among its neighbours as if it sat up to
    // `1 / (2 · PHASES)` = 1/64 of an output pixel from where it really
    // is. What must NOT move is (1) the total deposited mass, which is
    // what level preservation (R-M3-2) rests on, and (2) which output
    // pixels are covered at all.
    //
    // These call `deposit_band` directly rather than `drizzle_group`: the
    // overlap arm is a per-(frame, plane) property of `FrameDepositCtx`,
    // and driving it from here lets the SAME source, map and geometry run
    // once through each arm with no global switch (which, with the band
    // loop on a rayon pool, could not be made race-free anyway). ──

    /// The bar pins (c) and (d) hold ONE FRAME's tabulated plane to,
    /// against the exact clip, per pixel — the Tier C plan's own Task 4
    /// gate.
    ///
    /// **This bar is fixed and `phase_table::PHASES` is what moves to meet
    /// it** (ruling C-21, reversing the first round, which had pinned 3 %
    /// to keep `PHASES = 32`). The deviation is the phase residual
    /// (≤ `1 / (2 · PHASES)` of an output pixel) multiplied by the local
    /// RELATIVE gradient, so it peaks on star wings — where a Gaussian's
    /// relative gradient is `r / σ²` per pixel — and scales as
    /// `1 / PHASES`. Measured on these two fixtures (max over every
    /// covered pixel):
    ///
    /// | `PHASES` | rotated 1° | tps |
    /// | -------- | ---------- | --- |
    /// | 16 | 4.04 % | 4.64 % |
    /// | 32 | 2.17 % | 2.55 % |
    /// | **64 (shipped)** | **1.09 %** | **1.63 %** |
    ///
    /// Not a fixture-size artefact: at the M3 fixtures' own 64x48 the
    /// rotated case still read 2.13 % at 32. A future change that raises
    /// the deviation belongs in `PHASES`, not here.
    const C2_MAX_PIXEL_DEVIATION: f64 = 0.02;

    /// A structured mono field — a sky pedestal, a smooth large-scale
    /// gradient and a handful of Gaussian stars. Deliberately NOT uniform:
    /// on a flat field every area split gives the same `I / W` and the
    /// phase quantization is invisible, so a uniform fixture would pin
    /// nothing. The stars are the realistic worst case (the steepest local
    /// gradient a calibrated light carries).
    fn structured_field(w: usize, h: usize) -> Vec<f32> {
        let mut v = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (xf, yf) = (x as f64, y as f64);
                v[y * w + x] =
                    (100.0 + 25.0 * (xf / 11.0).sin() * (yf / 13.0).cos() + 0.05 * xf) as f32;
            }
        }
        // A deterministic scatter of stars, sigma 1.8 px.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..40 {
            let cx = (next() % (w as u64 - 12) + 6) as f64;
            let cy = (next() % (h as u64 - 12) + 6) as f64;
            let amp = 200.0 + (next() % 3000) as f64;
            for y in (cy as usize).saturating_sub(6)..(cy as usize + 7).min(h) {
                for x in (cx as usize).saturating_sub(6)..(cx as usize + 7).min(w) {
                    let r2 = (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2);
                    v[y * w + x] += (amp * (-r2 / (2.0 * 1.8 * 1.8)).exp()) as f32;
                }
            }
        }
        v
    }

    /// Deposits one whole plane band by band through `deposit_band` with
    /// the given overlap arm, returning `(I, W)` in output geometry.
    #[allow(clippy::too_many_arguments)]
    fn deposit_whole_plane(
        map: &PixelMap,
        src: &[f32],
        src_w: usize,
        src_h: usize,
        ref_w: usize,
        ref_h: usize,
        scale: u32,
        drop_shrink: f64,
        tabulated: bool,
    ) -> (Vec<f32>, Vec<f32>) {
        let out_w = ref_w * scale as usize;
        let out_h = ref_h * scale as usize;
        let mut i_buf = vec![0f32; out_w * out_h];
        let mut w_buf = vec![0f32; out_w * out_h];

        // Resolved exactly the way `drizzle_group` resolves it, so the pin
        // exercises the production dispatch and not a test-local copy.
        let table =
            (tabulated && phase_table_applies(scale, drop_shrink) && map.distortion.is_none())
                .then(|| {
                    geom::map_drop_at(
                        map,
                        (src_w as f64 - 1.0) / 2.0,
                        (src_h as f64 - 1.0) / 2.0,
                        drop_shrink,
                        scale,
                    )
                    .and_then(|q| PhaseTable::build(&q, scale, phase_table::PHASES))
                })
                .flatten();
        let gate = tabulated && phase_table_applies(scale, drop_shrink);
        let overlap = match (gate, table.as_ref(), map.distortion.is_some()) {
            (false, _, _) => SquareOverlap::Exact,
            (true, Some(t), _) => SquareOverlap::Frame(t),
            (true, None, true) => SquareOverlap::Tiled,
            (true, None, false) => panic!("a sane linear map must tabulate"),
        };

        let ctx = FrameDepositCtx {
            src,
            src_width: src_w,
            src_height: src_h,
            ref_width: ref_w,
            ref_height: ref_h,
            map,
            fwd: map.forward_eval(),
            half_diag: geom::drop_bound_half_diag(map, src_w, src_h, drop_shrink, scale),
            scale,
            drop_shrink,
            kernel: DrizzleKernel::Square,
            kernel_table: None,
            rej: None,
            ln: None,
            pair: NormalizationPair::IDENTITY,
            w: 1.0,
            plane: 0,
            cfa: None,
            overlap,
        };
        for (band_idx, (ib, wb)) in i_buf
            .chunks_mut(DRIZZLE_BAND_ROWS * out_w)
            .zip(w_buf.chunks_mut(DRIZZLE_BAND_ROWS * out_w))
            .enumerate()
        {
            deposit_band(ib, wb, band_idx, out_w, &ctx);
        }
        map.release_grids();
        (i_buf, w_buf)
    }

    /// Compares the two arms' `I / W` planes.
    ///
    /// Returns `(max relative per-pixel deviation, level ratio,
    /// coverage_exact, coverage_table, real coverage differences)`.
    ///
    /// **What "coverage identical" means here.** The exact path and the
    /// table path cover the same REGION, but its one-pixel RIM is ragged
    /// between them: a 1/64-output-pixel phase residual moves the edge of
    /// the deposited area by that much, so a border pixel whose true edge
    /// happens to lie within 1/64 px of a pixel boundary flips. On a
    /// 160x120 fixture the deposited region's border is ≈ 500 pixels long,
    /// so ≈ 500/64 ≈ 8 such flips are expected and mean nothing (in a real
    /// run the covered region is the UNION over hundreds of dithered
    /// frames, whose border is defined by the outermost frame's geometry,
    /// not by one drop's last sliver). What would be a real difference —
    /// and is what this counts — is a hole punched INSIDE the covered
    /// region, or coverage appearing well OUTSIDE it: a pixel whose
    /// coverage differs while it sits in the strict interior (all four
    /// neighbours covered) or the strict exterior (no neighbour covered)
    /// of the exact arm's own covered set.
    fn compare_arms(
        exact: &(Vec<f32>, Vec<f32>),
        table: &(Vec<f32>, Vec<f32>),
        out_w: usize,
    ) -> (f64, f64, usize, usize, usize) {
        let (ie, we) = exact;
        let (it, wt) = table;
        let out_h = ie.len() / out_w;
        let covered_exact = |x: i64, y: i64| -> bool {
            if x < 0 || y < 0 || x as usize >= out_w || y as usize >= out_h {
                return false;
            }
            we[y as usize * out_w + x as usize] > 0.0
        };
        let mut max_rel = 0.0f64;
        let mut sum_e = 0.0f64;
        let mut sum_t = 0.0f64;
        let mut cov_e = 0usize;
        let mut cov_t = 0usize;
        let mut cov_diff = 0usize;
        for idx in 0..ie.len() {
            let (x, y) = ((idx % out_w) as i64, (idx / out_w) as i64);
            let ce = we[idx] > 0.0;
            let ct = wt[idx] > 0.0;
            cov_e += ce as usize;
            cov_t += ct as usize;
            if ce != ct {
                let neighbours = [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)];
                let on_rim = if ce {
                    // Lost: fine only where the exact arm's own coverage
                    // already ended — i.e. not in the strict interior.
                    !neighbours.iter().all(|&(nx, ny)| covered_exact(nx, ny))
                } else {
                    // Gained: fine only next to the exact arm's coverage.
                    neighbours.iter().any(|&(nx, ny)| covered_exact(nx, ny))
                };
                if !on_rim {
                    cov_diff += 1;
                }
                continue;
            }
            if !ce {
                continue;
            }
            let ve = (ie[idx] / we[idx]) as f64;
            let vt = (it[idx] / wt[idx]) as f64;
            sum_e += ve;
            sum_t += vt;
            if ve.abs() > 0.0 {
                let rel = (vt - ve).abs() / ve.abs();
                if rel > max_rel {
                    max_rel = rel;
                }
            }
        }
        (max_rel, sum_t / sum_e, cov_e, cov_t, cov_diff)
    }

    /// Pin (c): the M3 rotation fixtures at 1°/5°/30°, scale 2,
    /// `dropShrink` 0.9 — the tabulated plane must track the exact one to
    /// within [`C2_MAX_PIXEL_DEVIATION`] per pixel, hold the plane level to
    /// 1 ± 1e-3, and cover the same region (see [`compare_arms`] on what
    /// "same" means at its rim).
    #[test]
    fn the_phase_table_tracks_the_exact_clip_on_a_rotated_frame() {
        const W: usize = 160;
        const H: usize = 120;
        let src = structured_field(W, H);
        for &deg in &[1.0_f64, 5.0, 30.0] {
            let map = rotation_about_centre(deg, W as f64 / 2.0, H as f64 / 2.0);
            let exact = deposit_whole_plane(&map, &src, W, H, W, H, 2, 0.9, false);
            let table = deposit_whole_plane(&map, &src, W, H, W, H, 2, 0.9, true);
            let (max_rel, level, cov_e, cov_t, cov_diff) = compare_arms(&exact, &table, W * 2);
            assert_eq!(
                cov_diff, 0,
                "deg {deg}: {cov_diff} output pixels differ in COVERAGE away from the covered region's own rim ({cov_e} exact vs {cov_t} tabulated)"
            );
            eprintln!(
                "C2 pin (c) deg {deg}: max |Δ| {:.4} %, level {level:.9}, coverage {cov_e} vs {cov_t}",
                max_rel * 100.0
            );
            assert!(
                max_rel <= C2_MAX_PIXEL_DEVIATION,
                "deg {deg}: max per-pixel deviation {:.4} % exceeds {:.1} %",
                max_rel * 100.0,
                C2_MAX_PIXEL_DEVIATION * 100.0
            );
            assert!(
                (level - 1.0).abs() <= 1e-3,
                "deg {deg}: plane level ratio {level} outside 1 ± 1e-3"
            );
        }
    }

    /// Pin (d): the same bounds with a distortion layer, i.e. through the
    /// per-[`phase_table::TILE`] arm. The fixture is deliberately wider
    /// and taller than one tile (3 x 2 of them) so the tile-row cache, the
    /// tile-crossing rebuild and the local-Jacobian estimate are all
    /// exercised rather than collapsing onto a single table.
    #[test]
    fn the_phase_table_tracks_the_exact_clip_on_a_tps_frame() {
        use crate::geometry::{DistortionModel, ThinPlateSpline};
        const W: usize = 600;
        const H: usize = 400;
        assert!(
            W > 2 * phase_table::TILE && H > phase_table::TILE,
            "the fixture must span several tiles"
        );

        let mut nodes: Vec<(f64, f64)> = Vec::new();
        for gy in 0..4 {
            for gx in 0..5 {
                nodes.push((40.0 + gx as f64 * 130.0, 40.0 + gy as f64 * 105.0));
            }
        }
        // A smooth several-pixel displacement field — the scale M4c's own
        // real registrations fit, not a pathological one.
        let dx: Vec<f64> = nodes
            .iter()
            .map(|&(x, y)| 2.5 * (x / 180.0).sin() + 0.8 * (y / 140.0).cos())
            .collect();
        let dy: Vec<f64> = nodes
            .iter()
            .map(|&(x, y)| 2.0 * (y / 160.0).cos() - 0.7 * (x / 200.0).sin())
            .collect();
        let ndx: Vec<f64> = dx.iter().map(|v| -v).collect();
        let ndy: Vec<f64> = dy.iter().map(|v| -v).collect();
        let forward = ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).expect("20-node fit");
        let inverse = ThinPlateSpline::fit(&nodes, &ndx, &ndy, 0.0).expect("20-node fit");
        let model = DistortionModel::tps(forward, inverse, [0.0, 0.0, W as f64, H as f64]);
        let map = PixelMap::with_distortion_model(Linear::identity(), model).unwrap();

        let src = structured_field(W, H);
        let exact = deposit_whole_plane(&map, &src, W, H, W, H, 2, 0.9, false);
        let table = deposit_whole_plane(&map, &src, W, H, W, H, 2, 0.9, true);
        let (max_rel, level, cov_e, cov_t, cov_diff) = compare_arms(&exact, &table, W * 2);
        assert_eq!(
            cov_diff, 0,
            "tps: {cov_diff} output pixels differ in COVERAGE away from the covered region's own rim ({cov_e} exact vs {cov_t} tabulated)"
        );
        eprintln!(
            "C2 pin (d) tps: max |Δ| {:.4} %, level {level:.9}, coverage {cov_e} vs {cov_t}",
            max_rel * 100.0
        );
        assert!(
            max_rel <= C2_MAX_PIXEL_DEVIATION,
            "tps: max per-pixel deviation {:.4} % exceeds {:.1} %",
            max_rel * 100.0,
            C2_MAX_PIXEL_DEVIATION * 100.0
        );
        assert!(
            (level - 1.0).abs() <= 1e-3,
            "tps: plane level ratio {level} outside 1 ± 1e-3"
        );
    }

    /// Ruling C-3's own carve-out: at `scale == 1` with an unshrunk drop
    /// the deposit stays on the exact clip, so a default-geometry drizzle
    /// is bit-for-bit what M3 produced.
    #[test]
    fn scale_one_with_a_full_drop_keeps_the_exact_clip() {
        assert!(!phase_table_applies(1, 1.0));
        assert!(phase_table_applies(1, 0.9));
        assert!(phase_table_applies(2, 1.0));
        assert!(phase_table_applies(3, 0.8));

        const W: usize = 96;
        const H: usize = 72;
        let src = structured_field(W, H);
        let map = rotation_about_centre(7.0, W as f64 / 2.0, H as f64 / 2.0);
        // `tabulated = true` asks for the table; the gate refuses it at
        // `scale == 1` with a full drop and hands back `Exact`, so the two
        // runs must be BIT-identical — not merely close, which is what
        // every other C2 pin measures.
        let forced_exact = deposit_whole_plane(&map, &src, W, H, W, H, 1, 1.0, false);
        let through_gate = deposit_whole_plane(&map, &src, W, H, W, H, 1, 1.0, true);
        assert_eq!(forced_exact.0, through_gate.0, "I buffer");
        assert_eq!(forced_exact.1, through_gate.1, "W buffer");
        // And a shrunk drop at the same scale DOES tabulate — otherwise
        // the assertion above would pass for the wrong reason.
        let shrunk_exact = deposit_whole_plane(&map, &src, W, H, W, H, 1, 0.9, false);
        let shrunk_table = deposit_whole_plane(&map, &src, W, H, W, H, 1, 0.9, true);
        assert_ne!(
            shrunk_exact.1, shrunk_table.1,
            "at dropShrink 0.9 the gate must let the table through"
        );
    }
}
