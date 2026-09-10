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

/// One channel's local-normalization model on the stride grid (spec §5.2,
/// math §4.1/§4.4).
#[derive(Debug, Clone, PartialEq)]
pub struct LnGrid {
    pub ref_width: usize, // reference geometry the grid covers
    pub ref_height: usize,
    pub scale: u32, // the LN scale (1024); stride = scale / 8
    pub gw: usize,  // grid columns = ceil(ref_width / stride) + 1
    pub gh: usize,
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
    /// `a_row`/`b_row` (length `ref_width`) with the bicubic B-spline over
    /// the grid; node `(i, j)` sits at `(i·stride, j·stride)`. `O(4·gw +
    /// 4·ref_width)`: the four row-nodes nearest `y` are combined once into
    /// two temporary rows of length `gw`, then every output pixel
    /// interpolates from those with its own four column weights — never
    /// `O(4·gw·ref_width)`.
    pub fn evaluate_row(&self, y: usize, a_row: &mut [f32], b_row: &mut [f32]) {
        assert_eq!(a_row.len(), self.ref_width, "a_row must be ref_width long");
        assert_eq!(b_row.len(), self.ref_width, "b_row must be ref_width long");

        let stride = self.stride() as f32;
        let offset = Interpolation::BicubicBSpline.first_tap_offset();

        let ty = y as f32 / stride;
        let j0 = ty.floor() as isize;
        let fy = ty - j0 as f32;
        let mut wy = [0f32; 8];
        Interpolation::BicubicBSpline.weights(fy, &mut wy);

        let mut tmp_a = vec![0f32; self.gw];
        let mut tmp_b = vec![0f32; self.gw];
        for k in 0..4usize {
            let j = j0 + offset + k as isize;
            let w = wy[k];
            for i in 0..self.gw {
                tmp_a[i] += w * self.node(&self.a, j, i);
                tmp_b[i] += w * self.node(&self.b, j, i);
            }
        }

        for x in 0..self.ref_width {
            let tx = x as f32 / stride;
            let i0 = tx.floor() as isize;
            let fx = tx - i0 as f32;
            let mut wx = [0f32; 8];
            Interpolation::BicubicBSpline.weights(fx, &mut wx);

            let mut va = 0f32;
            let mut vb = 0f32;
            for k in 0..4usize {
                let i = i0 + offset + k as isize;
                va += wx[k] * Self::ghost_1d(&tmp_a, i);
                vb += wx[k] * Self::ghost_1d(&tmp_b, i);
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

/// One frame's sidecar: one grid per channel.
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
    /// Encodes the sidecar payload (everything except the trailing xxh3
    /// trailer) — shared by `write` (which hashes it) and nothing else.
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
    /// truncated/partial file at `path`.
    pub fn write(&self, path: &Path) -> Result<()> {
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
    /// silently parsed into a wrong value.
    pub fn read(path: &Path) -> Result<LnFrameGrids> {
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

        let mut channels = Vec::with_capacity(channel_count as usize);
        for _ in 0..channel_count {
            if cursor.len() < CHANNEL_HEADER_LEN {
                bail!(
                    ".athln sidecar truncated channel header: {}",
                    path.display()
                );
            }
            let gw = cursor.read_u32::<LittleEndian>()? as usize;
            let gh = cursor.read_u32::<LittleEndian>()? as usize;
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
}
