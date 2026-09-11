//! `LnGrid`: one channel's local-normalization model on a coarse stride
//! grid in the reference geometry (spec §5.2, math §4.1/§4.4), evaluated
//! per output row by the cubic B-spline over the grid nodes — the same
//! kernel [`crate::resample::kernels::Interpolation::BicubicBSpline`] uses
//! for image resampling, reused here rather than re-derived. `LnFrameGrids`
//! is one frame's grids (one per channel) and the `.athln` binary sidecar
//! they round-trip through.
//!
//! Boundary handling: a tap that falls outside `[0, gw-1]`/`[0, gh-1]` is
//! NOT the last real node's value repeated — it is linearly extrapolated
//! from the two nearest real nodes (`ghost_1d`/`Self::node`). Repeating the
//! edge node breaks the cubic B-spline's affine-reproduction property in
//! exactly the direction it repeats (a constant grid is unaffected either
//! way, but a linear ramp picks up a visible bias at the edge); extrapolating
//! the two-node slope keeps that property intact all the way to the last
//! pixel, which is what `linear_ramp_is_reproduced_by_the_spline_between_nodes`
//! below pins.
//!
//! **Kernel semantics — read before touching `evaluate_row`.**
//! `BicubicBSpline` is the SMOOTHING cubic B-spline, applied directly to the
//! node values with no interpolating prefilter. That means the evaluated
//! surface does **not** pass through the node values: a node surrounded by
//! zeros evaluates to `2/3` at its own pixel (`cubic_bspline(0.0) == 2/3`,
//! `resample::kernels`), not `1.0`, and a quadratic sampled onto the nodes
//! carries a constant bias of `stride²·f″/6` wherever it is evaluated. What
//! it DOES reproduce exactly is an affine (constant or linear) function of
//! the node values, which is the property the tests below pin and the one
//! the math reference (§4.1/§4.4) relies on. **Controller ruling**: this is
//! the chosen behaviour, not a defect — at stride 128 the resulting bias on
//! the `B` surface is ≈0.26% of the field amplitude, this is the same
//! kernel the M1 resampler already uses elsewhere in the pipeline, and the
//! background model this feeds is smooth by construction, so the missing
//! prefilter costs nothing in practice. An interpolating prefilter (making
//! the surface pass through the node values exactly) is an M4 option, only
//! if the acceptance re-run ever shows a visible mesh imprint. Later tasks
//! must not expect `evaluate_row`/`evaluate_row_into` to reproduce `a`/`b`
//! at a node's own pixel.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use xxhash_rust::xxh3::Xxh3;

use crate::resample::kernels::Interpolation;

const MAGIC: &[u8; 8] = b"ATHLN\0\0\0";
const VERSION: u32 = 1;
/// Header size up to and including the channel count (magic + version +
/// ref_width + ref_height + scale + channels), all `u32`/8-byte magic.
const HEADER_LEN: usize = 8 + 4 * 5;
/// Per-channel fixed-size prefix (gw + gh + 4 f64s), before the `a`/`b`
/// float arrays.
const CHANNEL_HEADER_LEN: usize = 4 + 4 + 8 * 4;
/// Upper bound on the `channels` field a sidecar may declare — a real
/// frame has at most 3 planes (OSC), so 8 is generous headroom. Checked
/// BEFORE `read()` reserves a `Vec<LnGrid>` sized by that field: without
/// this bound, a corrupt or crafted `channels = 0xFFFFFFFF` asks for
/// hundreds of GB and the process aborts on allocation failure — the xxh3
/// trailer is a checksum, not a defence against a value it never
/// interprets as a size until after the hash already matched.
const MAX_CHANNELS: u32 = 8;

/// One channel's local-normalization model on the stride grid (spec §5.2,
/// math §4.1/§4.4). Evaluated with the SMOOTHING cubic B-spline (no
/// interpolating prefilter) — see the module doc's "Kernel semantics"
/// section before assuming `evaluate_row` reproduces `a`/`b` at a node's
/// own pixel; it does not, by design.
#[derive(Debug, Clone, PartialEq)]
pub struct LnGrid {
    pub ref_width: usize, // reference geometry the grid covers
    pub ref_height: usize,
    pub scale: u32,        // the LN scale (1024); stride = scale / 8
    pub gw: usize,         // grid columns = ceil((ref_width - 1) / stride) + 1 (Self::node_count)
    pub gh: usize,         // grid rows = ceil((ref_height - 1) / stride) + 1 (Self::node_count)
    pub a: Vec<f32>,       // gw × gh, row-major — local scale
    pub b: Vec<f32>,       // gw × gh — local zero offset
    pub global_scale: f64, // s from the PSF method
    pub location_ref: f64, // median of the reference plane
    pub location_tgt: f64, // median of the target plane
}

impl LnGrid {
    pub fn stride(&self) -> usize {
        (self.scale / 8).max(2) as usize
    }

    /// Minimal node count along one axis so the last node's pixel address
    /// (`(n-1)·stride`) reaches or passes the last valid pixel index
    /// (`extent - 1`) — node `i` sits at pixel `i·stride` (spec §5.2).
    fn node_count(extent: usize, stride: usize) -> usize {
        let span = extent.saturating_sub(1);
        span.div_ceil(stride.max(1)) + 1
    }

    /// `(gw, gh)` for a `width`×`height` plane at the given `stride` — the
    /// same mesh geometry as [`Self::node_count`], shared with the M2
    /// background model (`ln::background`) so the two never disagree about
    /// where a node sits.
    pub(crate) fn grid_dims(width: usize, height: usize, stride: usize) -> (usize, usize) {
        (
            Self::node_count(width, stride),
            Self::node_count(height, stride),
        )
    }

    pub fn constant(ref_width: usize, ref_height: usize, scale: u32, a: f32, b: f32) -> LnGrid {
        let stride = (scale / 8).max(2) as usize;
        let gw = Self::node_count(ref_width, stride);
        let gh = Self::node_count(ref_height, stride);
        LnGrid {
            ref_width,
            ref_height,
            scale,
            gw,
            gh,
            a: vec![a; gw * gh],
            b: vec![b; gw * gh],
            global_scale: 1.0,
            location_ref: 0.0,
            location_tgt: 0.0,
        }
    }

    /// Grid lookup at node row `j` (possibly outside `[0, gh-1]`), node
    /// column `i` (always in range — the caller only varies `j`). Out-of-
    /// range `j` linearly extrapolates from the two nearest real node rows;
    /// see the module doc for why this, not a repeated edge value.
    fn node(&self, plane: &[f32], j: isize, i: usize) -> f32 {
        let gh = self.gh as isize;
        if j >= 0 && j < gh {
            return plane[j as usize * self.gw + i];
        }
        if self.gh < 2 {
            return plane[i];
        }
        if j < 0 {
            let p0 = plane[i];
            let p1 = plane[self.gw + i];
            p0 + (p1 - p0) * j as f32
        } else {
            let last = self.gh - 1;
            let p_last = plane[last * self.gw + i];
            let p_prev = plane[(last - 1) * self.gw + i];
            p_last + (p_last - p_prev) * (j - last as isize) as f32
        }
    }

    /// Same extrapolation as [`Self::node`], on a plain 1-D row (used for
    /// the column/`x` pass over the two temporary rows `evaluate_row`
    /// builds).
    fn ghost_1d(values: &[f32], idx: isize) -> f32 {
        let n = values.len() as isize;
        if idx >= 0 && idx < n {
            return values[idx as usize];
        }
        if n < 2 {
            return values[0];
        }
        if idx < 0 {
            let p0 = values[0];
            let p1 = values[1];
            p0 + (p1 - p0) * idx as f32
        } else {
            let last = n - 1;
            let p_last = values[last as usize];
            let p_prev = values[(last - 1) as usize];
            p_last + (p_last - p_prev) * (idx - last) as f32
        }
    }

    /// Evaluates A and B along output row `y` (reference coordinates) into
    /// `a_row`/`b_row` (length `ref_width`) with the SMOOTHING bicubic
    /// B-spline over the grid (see the module doc's "Kernel semantics" —
    /// this does not reproduce `a`/`b` exactly at a node's own pixel, only
    /// affine functions of them); node `(i, j)` sits at `(i·stride,
    /// j·stride)`. Allocates a fresh [`LnScratch`] per call — prefer
    /// [`Self::evaluate_row_into`] with a scratch reused across rows in a
    /// loop (Task 6's band loop does).
    pub fn evaluate_row(&self, y: usize, a_row: &mut [f32], b_row: &mut [f32]) {
        let mut scratch = LnScratch::for_grid(self);
        self.evaluate_row_into(y, a_row, b_row, &mut scratch);
    }

    /// Same as [`Self::evaluate_row`], writing into caller-supplied
    /// `scratch` (sized by [`LnScratch::for_grid`]) instead of allocating
    /// two temporary rows on every call. `O(4·gw + 4·ref_width)`: the four
    /// row-nodes nearest `y` are combined once into `scratch`, then every
    /// output pixel interpolates from that with its own four column
    /// weights — never `O(4·gw·ref_width)`.
    pub fn evaluate_row_into(
        &self,
        y: usize,
        a_row: &mut [f32],
        b_row: &mut [f32],
        scratch: &mut LnScratch,
    ) {
        assert!(
            y < self.ref_height,
            "y ({y}) must be < ref_height ({})",
            self.ref_height
        );
        assert_eq!(a_row.len(), self.ref_width, "a_row must be ref_width long");
        assert_eq!(b_row.len(), self.ref_width, "b_row must be ref_width long");
        assert_eq!(
            scratch.a.len(),
            self.gw,
            "scratch must match this grid's gw"
        );
        assert_eq!(
            scratch.b.len(),
            self.gw,
            "scratch must match this grid's gw"
        );
        let stride_usize = self.stride();
        assert_eq!(
            scratch.wx_table.len(),
            stride_usize,
            "scratch must match this grid's stride"
        );

        let stride = stride_usize as f32;
        let offset = Interpolation::BicubicBSpline.first_tap_offset();

        let ty = y as f32 / stride;
        let j0 = ty.floor() as isize;
        let fy = ty - j0 as f32;
        let mut wy = [0f32; 8];
        Interpolation::BicubicBSpline.weights(fy, &mut wy);

        scratch.a.fill(0.0);
        scratch.b.fill(0.0);
        for k in 0..4usize {
            let j = j0 + offset + k as isize;
            let w = wy[k];
            for i in 0..self.gw {
                scratch.a[i] += w * self.node(&self.a, j, i);
                scratch.b[i] += w * self.node(&self.b, j, i);
            }
        }

        // Task 5 (M4a): `fx` above takes exactly `stride_usize` distinct
        // values as `x` sweeps `0..ref_width` — `x mod stride`, over
        // `stride` — so `scratch.wx_table` (built once per grid by
        // `LnScratch::for_grid`) already holds every 4-tap weight set this
        // loop would otherwise recompute via `BicubicBSpline::weights` on
        // EVERY pixel. `i0 = x / stride_usize` (integer division) is the
        // same value `tx.floor() as isize` would give for these input
        // ranges — see the module's Task 5 pin test.
        for x in 0..self.ref_width {
            let i0 = (x / stride_usize) as isize;
            let wx = &scratch.wx_table[x % stride_usize];

            let mut va = 0f32;
            let mut vb = 0f32;
            for k in 0..4usize {
                let i = i0 + offset + k as isize;
                va += wx[k] * Self::ghost_1d(&scratch.a, i);
                vb += wx[k] * Self::ghost_1d(&scratch.b, i);
            }
            a_row[x] = va;
            b_row[x] = vb;
        }
    }

    #[inline]
    pub fn apply(a: f32, b: f32, v: f32) -> f32 {
        a * v + b
    }
}

/// Reusable scratch buffers for [`LnGrid::evaluate_row_into`] — two rows of
/// length `gw`, avoiding a pair of heap allocations on every call when a
/// caller (Task 6's band loop) evaluates many rows against the same grid.
/// Fields are private: the only way to build one is [`Self::for_grid`],
/// which sizes it correctly for the grid it will be used with.
///
/// `wx_table` (M4a Task 5) is the per-column 4-tap `BicubicBSpline` weight
/// set for every `fx` value `evaluate_row_into`'s x-loop can ever see —
/// `stride` entries, indexed by `x % stride` — built once here instead of
/// recomputed on every one of a row's `ref_width` pixels.
pub struct LnScratch {
    a: Vec<f32>,
    b: Vec<f32>,
    wx_table: Vec<[f32; 4]>,
}

impl LnScratch {
    /// Buffers sized for `grid`'s `gw`, plus the `wx_table` for `grid`'s
    /// `stride()`. Reusable across calls against any grid that shares the
    /// same `gw` AND `stride` (typically every channel of one frame, since
    /// they share one reference geometry and one LN scale — see
    /// [`LnFrameGrids`]'s doc).
    pub fn for_grid(grid: &LnGrid) -> LnScratch {
        let stride = grid.stride();
        let mut wx_table = Vec::with_capacity(stride);
        for r in 0..stride {
            let frac = r as f32 / stride as f32;
            let mut w = [0f32; 8];
            Interpolation::BicubicBSpline.weights(frac, &mut w);
            wx_table.push([w[0], w[1], w[2], w[3]]);
        }
        LnScratch {
            a: vec![0.0; grid.gw],
            b: vec![0.0; grid.gw],
            wx_table,
        }
    }
}

/// One frame's sidecar: one grid per channel. The `.athln` format stores a
/// single reference geometry (`ref_width`/`ref_height`/`scale`) for the
/// whole file, taken from `channels[0]` on write — every channel MUST
/// share one geometry, or it would not round-trip (a later channel's own
/// `ref_width`/`ref_height`/`scale` would be silently dropped in favour of
/// the first). [`Self::write`] refuses a mismatched set with an error
/// rather than silently taking the first.
#[derive(Debug, Clone, PartialEq)]
pub struct LnFrameGrids {
    pub channels: Vec<LnGrid>,
}

/// A tmp sibling of `path` that will not collide with a concurrent writer
/// of the same sidecar (same scheme as `fits_writer::writer::write_fits_f32`:
/// process id + a per-process atomic sequence).
fn tmp_sidecar_path(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.tmp.{}.{}", std::process::id(), seq))
}

impl LnFrameGrids {
    /// Refuses a `channels` set whose members disagree on the one geometry
    /// the `.athln` format can actually store (see the struct doc). A
    /// single channel, or none, trivially agrees with itself.
    fn validate_uniform_geometry(&self) -> Result<()> {
        let Some(first) = self.channels.first() else {
            return Ok(());
        };
        for (idx, g) in self.channels.iter().enumerate().skip(1) {
            if g.ref_width != first.ref_width
                || g.ref_height != first.ref_height
                || g.scale != first.scale
            {
                bail!(
                    ".athln sidecar: channel {idx} geometry ({}x{}, scale {}) does not match channel 0's ({}x{}, scale {}) — every channel must share one reference geometry",
                    g.ref_width, g.ref_height, g.scale, first.ref_width, first.ref_height, first.scale
                );
            }
        }
        Ok(())
    }

    /// Encodes the sidecar payload (everything except the trailing xxh3
    /// trailer) — shared by `write` (which hashes it) and nothing else.
    /// Geometry (`ref_width`/`ref_height`/`scale`) is taken from
    /// `channels[0]` — [`Self::write`] has already refused a mismatched set
    /// via [`Self::validate_uniform_geometry`] by the time this runs.
    fn encode(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.write_u32::<LittleEndian>(VERSION)?;
        let (ref_width, ref_height, scale) = self
            .channels
            .first()
            .map(|g| (g.ref_width, g.ref_height, g.scale))
            .unwrap_or((0, 0, 0));
        buf.write_u32::<LittleEndian>(ref_width as u32)?;
        buf.write_u32::<LittleEndian>(ref_height as u32)?;
        buf.write_u32::<LittleEndian>(scale)?;
        buf.write_u32::<LittleEndian>(self.channels.len() as u32)?;
        for g in &self.channels {
            buf.write_u32::<LittleEndian>(g.gw as u32)?;
            buf.write_u32::<LittleEndian>(g.gh as u32)?;
            buf.write_f64::<LittleEndian>(g.global_scale)?;
            buf.write_f64::<LittleEndian>(g.location_ref)?;
            buf.write_f64::<LittleEndian>(g.location_tgt)?;
            // Relative scale factor: `= global_scale` today, reserved for a
            // future per-channel PSF-scale weight distinct from it.
            buf.write_f64::<LittleEndian>(g.global_scale)?;
            for v in &g.a {
                buf.write_f32::<LittleEndian>(*v)?;
            }
            for v in &g.b {
                buf.write_f32::<LittleEndian>(*v)?;
            }
        }
        Ok(buf)
    }

    /// Writes the sidecar: tmp file + atomic rename, never leaving a
    /// truncated/partial file at `path`. Refuses (before touching disk) a
    /// `channels` set whose members don't share one reference geometry —
    /// see the struct doc.
    pub fn write(&self, path: &Path) -> Result<()> {
        self.validate_uniform_geometry()?;
        let payload = self.encode()?;
        let hash = {
            let mut hasher = Xxh3::new();
            hasher.update(&payload);
            hasher.digest()
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {} for the .athln sidecar", parent.display()))?;
        }

        let tmp = tmp_sidecar_path(path);
        let write_result = (|| -> Result<()> {
            let file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            let mut w = BufWriter::new(file);
            w.write_all(&payload)?;
            w.write_u64::<LittleEndian>(hash)?;
            w.flush()?;
            // Power-loss durability: bytes on disk before the rename makes
            // the file visible under its final name.
            w.get_ref().sync_all()?;
            Ok(())
        })();

        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp);
            tracing::error!(path = %path.display(), error = %e, "failed to write .athln sidecar");
            return Err(e);
        }

        if let Err(e) = crate::fits_writer::writer::rename_replace(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            tracing::error!(path = %path.display(), error = %e, "failed to finalize .athln sidecar");
            return Err(e.into());
        }
        Ok(())
    }

    /// Reads and validates a sidecar written by [`Self::write`]. The xxh3
    /// trailer is checked BEFORE any field is interpreted, so any single
    /// flipped byte anywhere in the payload is refused rather than
    /// silently parsed into a wrong value. Every error path is logged
    /// (path + error) before returning — see [`Self::read_inner`] for the
    /// actual parse.
    pub fn read(path: &Path) -> Result<LnFrameGrids> {
        Self::read_inner(path).map_err(|e| {
            tracing::error!(path = %path.display(), error = %e, "failed to read .athln sidecar");
            e
        })
    }

    fn read_inner(path: &Path) -> Result<LnFrameGrids> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if bytes.len() < HEADER_LEN + 8 {
            bail!(".athln sidecar too short: {}", path.display());
        }
        let trailer_at = bytes.len() - 8;
        let payload = &bytes[..trailer_at];
        let stored_hash = (&bytes[trailer_at..]).read_u64::<LittleEndian>()?;
        let actual_hash = {
            let mut hasher = Xxh3::new();
            hasher.update(payload);
            hasher.digest()
        };
        if actual_hash != stored_hash {
            bail!(".athln sidecar checksum mismatch: {}", path.display());
        }

        let mut cursor = payload;
        let mut magic = [0u8; 8];
        cursor.read_exact(&mut magic)?;
        if &magic != MAGIC {
            bail!(".athln sidecar bad magic: {}", path.display());
        }
        let version = cursor.read_u32::<LittleEndian>()?;
        if version != VERSION {
            bail!(
                ".athln sidecar unsupported version {version}: {}",
                path.display()
            );
        }
        let ref_width = cursor.read_u32::<LittleEndian>()? as usize;
        let ref_height = cursor.read_u32::<LittleEndian>()? as usize;
        let scale = cursor.read_u32::<LittleEndian>()?;
        let channel_count = cursor.read_u32::<LittleEndian>()?;

        // Critical: bound `channels` BEFORE reserving anything sized by it.
        // The xxh3 trailer already matched at this point, but a checksum
        // only proves the bytes weren't corrupted in transit — it says
        // nothing about whether the value they encode is a sane size to
        // allocate, so a deliberately-crafted file with a valid trailer and
        // `channels = 0xFFFFFFFF` must still be refused here, not after
        // `Vec::with_capacity` has already asked the allocator for it.
        if channel_count == 0 || channel_count > MAX_CHANNELS {
            bail!(
                ".athln sidecar channels out of range ({channel_count}, expected 1..={MAX_CHANNELS}): {}",
                path.display()
            );
        }

        // Important: every channel's `gw`/`gh` must equal what the header's
        // own ref_width/ref_height/scale imply — computed once since the
        // geometry is shared across all channels (see `LnFrameGrids`'s
        // doc). A grid too small (or too large) for its declared geometry
        // would otherwise parse cleanly and silently extrapolate or
        // truncate the wrong surface.
        let stride = (scale / 8).max(2) as usize;
        let (expected_gw, expected_gh) = LnGrid::grid_dims(ref_width, ref_height, stride);

        let mut channels = Vec::with_capacity(channel_count as usize);
        for idx in 0..channel_count {
            if cursor.len() < CHANNEL_HEADER_LEN {
                bail!(
                    ".athln sidecar truncated channel header: {}",
                    path.display()
                );
            }
            let gw = cursor.read_u32::<LittleEndian>()? as usize;
            let gh = cursor.read_u32::<LittleEndian>()? as usize;
            if gw != expected_gw || gh != expected_gh {
                bail!(
                    ".athln sidecar channel {idx} grid dims {gw}x{gh} do not match the geometry the header implies ({expected_gw}x{expected_gh} for {ref_width}x{ref_height} at scale {scale}): {}",
                    path.display()
                );
            }
            let global_scale = cursor.read_f64::<LittleEndian>()?;
            let location_ref = cursor.read_f64::<LittleEndian>()?;
            let location_tgt = cursor.read_f64::<LittleEndian>()?;
            let _relative_scale = cursor.read_f64::<LittleEndian>()?; // reserved, unused today

            let n = gw
                .checked_mul(gh)
                .context(".athln sidecar grid dimensions overflow")?;
            if cursor.len() < n.saturating_mul(8) {
                bail!(".athln sidecar truncated grid data: {}", path.display());
            }
            let mut a = Vec::with_capacity(n);
            for _ in 0..n {
                a.push(cursor.read_f32::<LittleEndian>()?);
            }
            let mut b = Vec::with_capacity(n);
            for _ in 0..n {
                b.push(cursor.read_f32::<LittleEndian>()?);
            }
            channels.push(LnGrid {
                ref_width,
                ref_height,
                scale,
                gw,
                gh,
                a,
                b,
                global_scale,
                location_ref,
                location_tgt,
            });
        }
        Ok(LnFrameGrids { channels })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;

    /// M4a Task 5 pin: a VERBATIM copy of `evaluate_row_into`'s per-pixel
    /// loop as it stood before this task — `BicubicBSpline::weights`
    /// recomputed fresh for every output pixel instead of read from a
    /// precomputed `wx_table` — kept only so
    /// `evaluate_row_into_matches_the_pre_table_reference_implementation`
    /// below can check the table-based version against it. Do not "clean
    /// this up" to share code with the production loop: the whole point is
    /// that it is independent of whatever `evaluate_row_into` does next.
    fn evaluate_row_reference(grid: &LnGrid, y: usize, a_row: &mut [f32], b_row: &mut [f32]) {
        let stride = grid.stride() as f32;
        let offset = Interpolation::BicubicBSpline.first_tap_offset();

        let ty = y as f32 / stride;
        let j0 = ty.floor() as isize;
        let fy = ty - j0 as f32;
        let mut wy = [0f32; 8];
        Interpolation::BicubicBSpline.weights(fy, &mut wy);

        let mut a_scratch = vec![0f32; grid.gw];
        let mut b_scratch = vec![0f32; grid.gw];
        for k in 0..4usize {
            let j = j0 + offset + k as isize;
            let w = wy[k];
            for i in 0..grid.gw {
                a_scratch[i] += w * grid.node(&grid.a, j, i);
                b_scratch[i] += w * grid.node(&grid.b, j, i);
            }
        }

        for x in 0..grid.ref_width {
            let tx = x as f32 / stride;
            let i0 = tx.floor() as isize;
            let fx = tx - i0 as f32;
            let mut wx = [0f32; 8];
            Interpolation::BicubicBSpline.weights(fx, &mut wx);

            let mut va = 0f32;
            let mut vb = 0f32;
            for k in 0..4usize {
                let i = i0 + offset + k as isize;
                va += wx[k] * LnGrid::ghost_1d(&a_scratch, i);
                vb += wx[k] * LnGrid::ghost_1d(&b_scratch, i);
            }
            a_row[x] = va;
            b_row[x] = vb;
        }
    }

    #[test]
    fn evaluate_row_into_matches_the_pre_table_reference_implementation() {
        // A 7x5 node grid over an 800x600 reference at stride 128 (scale
        // 1024) — deliberately not derived via `node_count`, since this
        // test checks the evaluator's numbers, not the mesh geometry.
        let (gw, gh, ref_width, ref_height, scale) = (7usize, 5usize, 800usize, 600usize, 1024u32);
        let mut rng = SplitMix64(0xC0FFEE_u64);
        let n = gw * gh;
        let a: Vec<f32> = (0..n).map(|_| (rng.next_f64() as f32 - 0.5) * 4.0).collect();
        let b: Vec<f32> = (0..n).map(|_| (rng.next_f64() as f32 - 0.5) * 2.0).collect();
        let grid = LnGrid {
            ref_width,
            ref_height,
            scale,
            gw,
            gh,
            a,
            b,
            global_scale: 1.0,
            location_ref: 0.0,
            location_tgt: 0.0,
        };

        let mut scratch = LnScratch::for_grid(&grid);
        for &y in &[0usize, 77, ref_height - 1] {
            let (mut a_row, mut b_row) = (vec![0f32; ref_width], vec![0f32; ref_width]);
            let (mut a_ref, mut b_ref) = (vec![0f32; ref_width], vec![0f32; ref_width]);
            grid.evaluate_row_into(y, &mut a_row, &mut b_row, &mut scratch);
            evaluate_row_reference(&grid, y, &mut a_ref, &mut b_ref);
            for x in 0..ref_width {
                assert!(
                    (a_row[x] - a_ref[x]).abs() < 1e-6,
                    "row {y}, x {x}: a table {} vs reference {}",
                    a_row[x],
                    a_ref[x]
                );
                assert!(
                    (b_row[x] - b_ref[x]).abs() < 1e-6,
                    "row {y}, x {x}: b table {} vs reference {}",
                    b_row[x],
                    b_ref[x]
                );
            }
        }
    }

    #[test]
    fn constant_grid_evaluates_to_its_constants_everywhere() {
        let g = LnGrid::constant(300, 200, 128, 1.5, -0.25); // stride 16 → gw 20, gh 14
        let (mut a, mut b) = (vec![0.0; 300], vec![0.0; 300]);
        for y in [0, 1, 17, 199] {
            g.evaluate_row(y, &mut a, &mut b);
            assert!(a.iter().all(|v| (v - 1.5).abs() < 1e-6), "row {y}");
            assert!(b.iter().all(|v| (v + 0.25).abs() < 1e-6), "row {y}");
        }
    }

    #[test]
    fn linear_ramp_is_reproduced_by_the_spline_between_nodes() {
        // B-spline interpolation reproduces linear functions exactly (up to f32).
        let mut g = LnGrid::constant(257, 129, 128, 1.0, 0.0); // stride 16 → gw 17, gh 9
        for j in 0..g.gh {
            for i in 0..g.gw {
                g.b[j * g.gw + i] = (i * 16) as f32 * 0.01 + (j * 16) as f32 * 0.02;
            }
        }
        let (mut a, mut b) = (vec![0.0; 257], vec![0.0; 257]);
        g.evaluate_row(40, &mut a, &mut b);
        for x in [0usize, 5, 16, 100, 255] {
            let expect = x as f32 * 0.01 + 40.0 * 0.02;
            assert!((b[x] - expect).abs() < 2e-3, "x {x}: {} vs {expect}", b[x]);
        }
    }

    #[test]
    fn sidecar_round_trips_and_rejects_a_flipped_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let mut g = LnGrid::constant(64, 48, 128, 1.1, 0.01);
        g.global_scale = 1.1;
        g.location_ref = 0.2;
        g.location_tgt = 0.19;
        let frames = LnFrameGrids {
            channels: vec![g.clone(), g],
        };
        frames.write(&p).unwrap();
        assert_eq!(LnFrameGrids::read(&p).unwrap(), frames);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[40] ^= 0x01;
        std::fs::write(&p, bytes).unwrap();
        assert!(LnFrameGrids::read(&p).is_err());
    }

    /// Recomputes `bytes`' own trailer (the last 8 bytes) from everything
    /// before it — shared by every "corrupt but checksum-consistent" test
    /// helper below, so each one only has to describe the corruption, not
    /// re-derive the trailer.
    fn rehash(bytes: &mut [u8]) {
        let trailer_at = bytes.len() - 8;
        let hash = {
            let mut hasher = Xxh3::new();
            hasher.update(&bytes[..trailer_at]);
            hasher.digest()
        };
        bytes[trailer_at..].copy_from_slice(&hash.to_le_bytes());
    }

    /// Patches a little-endian `u32` field at `offset` in a sidecar file
    /// and recomputes the trailer, so a test can build a "corrupt but
    /// checksum-consistent" file without hand-duplicating `encode`'s
    /// layout.
    fn patch_u32_and_rehash(path: &Path, offset: usize, new_value: u32) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[offset..offset + 4].copy_from_slice(&new_value.to_le_bytes());
        rehash(&mut bytes);
        std::fs::write(path, &bytes).unwrap();
    }

    /// Same as [`patch_u32_and_rehash`] for a single byte — used for the
    /// 8-byte `MAGIC` field, which is not a `u32`.
    fn patch_byte_and_rehash(path: &Path, offset: usize, new_value: u8) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[offset] = new_value;
        rehash(&mut bytes);
        std::fs::write(path, &bytes).unwrap();
    }

    #[test]
    fn a_channel_count_beyond_max_is_refused_without_allocating_it() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let g = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        LnFrameGrids {
            channels: vec![g.clone(), g],
        }
        .write(&p)
        .unwrap();

        // `channels` is the last u32 of the fixed file header.
        let channels_at = HEADER_LEN - 4;
        patch_u32_and_rehash(&p, channels_at, 0xFFFF_FFFF);

        let err = LnFrameGrids::read(&p).unwrap_err();
        assert!(
            err.to_string().contains("channels"),
            "error must name the field: {err}"
        );
    }

    #[test]
    fn a_grid_too_small_for_its_declared_geometry_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let g = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        LnFrameGrids { channels: vec![g] }.write(&p).unwrap();

        // Channel 0's `gw` is the first u32 right after the file header.
        let gw_at = HEADER_LEN;
        let bytes = std::fs::read(&p).unwrap();
        let original_gw = u32::from_le_bytes(bytes[gw_at..gw_at + 4].try_into().unwrap());
        patch_u32_and_rehash(&p, gw_at, original_gw - 1);

        let err = LnFrameGrids::read(&p).unwrap_err();
        assert!(
            err.to_string().contains("grid dims"),
            "error should describe the mismatch: {err}"
        );
    }

    /// M9 (final fix wave): `read_inner`'s `too short` bail — shorter than
    /// `HEADER_LEN + 8` (the fixed header plus the trailer), caught before
    /// the trailer is even read, so no rehash is needed.
    #[test]
    fn a_too_short_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let g = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        LnFrameGrids { channels: vec![g] }.write(&p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&p, &bytes[..HEADER_LEN]).unwrap();

        let err = LnFrameGrids::read(&p).unwrap_err();
        assert!(err.to_string().contains("too short"), "{err}");
    }

    /// M9: `read_inner`'s `bad magic` bail — the first magic byte flipped,
    /// trailer recomputed so the checksum still matches.
    #[test]
    fn a_bad_magic_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let g = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        LnFrameGrids { channels: vec![g] }.write(&p).unwrap();
        patch_byte_and_rehash(&p, 0, b'X');

        let err = LnFrameGrids::read(&p).unwrap_err();
        assert!(err.to_string().contains("magic"), "{err}");
    }

    /// M9: `read_inner`'s `unsupported version` bail — `version` is the
    /// first `u32` right after the 8-byte magic.
    #[test]
    fn an_unsupported_version_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let g = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        LnFrameGrids { channels: vec![g] }.write(&p).unwrap();
        patch_u32_and_rehash(&p, 8, VERSION + 1);

        let err = LnFrameGrids::read(&p).unwrap_err();
        assert!(err.to_string().contains("version"), "{err}");
    }

    /// M9: `read_inner`'s `truncated grid data` bail — the payload cut off
    /// a handful of bytes into the first channel's `a`/`b` arrays (well
    /// short of `n * 8`), trailer recomputed over the truncated payload so
    /// the checksum still matches and the truncation itself is what's
    /// caught.
    #[test]
    fn truncated_grid_data_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let g = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        LnFrameGrids { channels: vec![g] }.write(&p).unwrap();
        let bytes = std::fs::read(&p).unwrap();

        let cut_at = HEADER_LEN + CHANNEL_HEADER_LEN + 4;
        let mut truncated = bytes[..cut_at].to_vec();
        truncated.extend_from_slice(&[0u8; 8]); // placeholder trailer, overwritten by rehash
        rehash(&mut truncated);
        std::fs::write(&p, &truncated).unwrap();

        let err = LnFrameGrids::read(&p).unwrap_err();
        assert!(err.to_string().contains("truncated grid data"), "{err}");
    }

    #[test]
    fn write_refuses_channels_with_different_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let a = LnGrid::constant(64, 48, 128, 1.0, 0.0);
        let b = LnGrid::constant(64, 48, 256, 1.0, 0.0); // different scale
        let err = LnFrameGrids {
            channels: vec![a, b],
        }
        .write(&p)
        .unwrap_err();
        assert!(err.to_string().contains("geometry"), "{err}");
        assert!(!p.exists(), "a refused write must not touch disk");
    }
}
