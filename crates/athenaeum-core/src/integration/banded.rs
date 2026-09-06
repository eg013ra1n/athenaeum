//! Banded FITS reader + decode-and-spill fallback (spec §4).
//!
//! Plain uncompressed single-HDU FITS files (the common camera-output case)
//! get direct seek-reads of row bands straight off disk — BZERO/BSCALE
//! applied, big-endian decode, no stretch. Anything else (XISF, RGB FITS,
//! nonstandard headers) is decoded once via `astroimage::ImageConverter::read_raw`
//! and spilled to a raw little-endian f32 scratch file that is then band-read
//! the same way. Callers never hold more than N × one band in RAM.

use super::IntegrationError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const BLOCK: u64 = 2880;

/// How one frame's raw band bytes decode to physical samples. Carried per
/// frame because a set may legally mix bit depths, and because the
/// decode-and-spill fallback produces little-endian f32 while FITS is big.
#[derive(Clone, Copy)]
pub(crate) enum PlaneKind {
    U8 { bzero: f32, bscale: f32 },
    I16Be { bzero: f32, bscale: f32 },
    I32Be { bzero: f32, bscale: f32 },
    F32Be { bzero: f32, bscale: f32 },
    F64Be { bzero: f64, bscale: f64 },
    /// Decode-and-spill scratch: little-endian f32, already physical.
    F32Le,
}

impl PlaneKind {
    #[inline]
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            PlaneKind::U8 { .. } => 1,
            PlaneKind::I16Be { .. } => 2,
            PlaneKind::I32Be { .. } | PlaneKind::F32Be { .. } | PlaneKind::F32Le => 4,
            PlaneKind::F64Be { .. } => 8,
        }
    }

    #[inline]
    fn decode(self, b: &[u8], idx: usize) -> f32 {
        match self {
            PlaneKind::U8 { bzero, bscale } => b[idx] as f32 * bscale + bzero,
            PlaneKind::I16Be { bzero, bscale } => {
                let o = idx * 2;
                i16::from_be_bytes([b[o], b[o + 1]]) as f32 * bscale + bzero
            }
            PlaneKind::I32Be { bzero, bscale } => {
                let o = idx * 4;
                i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f32 * bscale + bzero
            }
            PlaneKind::F32Be { bzero, bscale } => {
                let o = idx * 4;
                f32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) * bscale + bzero
            }
            PlaneKind::F64Be { bzero, bscale } => {
                let o = idx * 8;
                let v = f64::from_be_bytes([
                    b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7],
                ]);
                (v * bscale + bzero) as f32
            }
            PlaneKind::F32Le => {
                let o = idx * 4;
                f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
            }
        }
    }

    /// Bulk decode of `dst.len()` consecutive samples starting at `start` —
    /// a tight typed loop per arm so the optimizer can vectorize it.
    fn decode_run(self, b: &[u8], start: usize, dst: &mut [f32]) {
        let bpp = self.bytes_per_sample();
        let src = &b[start * bpp..(start + dst.len()) * bpp];
        match self {
            PlaneKind::U8 { bzero, bscale } => {
                for (s, d) in src.iter().zip(dst.iter_mut()) { *d = *s as f32 * bscale + bzero; }
            }
            PlaneKind::I16Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(2).zip(dst.iter_mut()) {
                    *d = i16::from_be_bytes([c[0], c[1]]) as f32 * bscale + bzero;
                }
            }
            PlaneKind::I32Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *d = i32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f32 * bscale + bzero;
                }
            }
            PlaneKind::F32Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *d = f32::from_be_bytes([c[0], c[1], c[2], c[3]]) * bscale + bzero;
                }
            }
            PlaneKind::F64Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(8).zip(dst.iter_mut()) {
                    let v = f64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                    *d = (v * bscale + bzero) as f32;
                }
            }
            PlaneKind::F32Le => {
                for (c, d) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *d = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                }
            }
        }
    }
}

/// Build the decode kind for a probed FITS primary HDU. `bitpix` is always
/// one of `8 | 16 | 32 | -32 | -64` here — `probe_fits` validates that before
/// ever constructing a `FrameReader::Fits`, so no other value can reach this.
fn plane_kind_for_bitpix(bitpix: i32, bzero: f64, bscale: f64) -> PlaneKind {
    match bitpix {
        8 => PlaneKind::U8 { bzero: bzero as f32, bscale: bscale as f32 },
        16 => PlaneKind::I16Be { bzero: bzero as f32, bscale: bscale as f32 },
        32 => PlaneKind::I32Be { bzero: bzero as f32, bscale: bscale as f32 },
        -32 => PlaneKind::F32Be { bzero: bzero as f32, bscale: bscale as f32 },
        -64 => PlaneKind::F64Be { bzero, bscale },
        other => unreachable!("probe_fits only returns bitpix in {{8,16,32,-32,-64}}, got {other}"),
    }
}

enum FrameReader {
    /// Direct seek-read of an uncompressed single-HDU FITS.
    Fits { file: File, data_offset: u64, kind: PlaneKind },
    /// Raw little-endian f32 scratch spill (one full frame, row-major).
    Scratch { file: File, kind: PlaneKind },
}

impl FrameReader {
    #[inline]
    fn kind(&self) -> PlaneKind {
        match self {
            FrameReader::Fits { kind, .. } | FrameReader::Scratch { kind, .. } => *kind,
        }
    }
}

pub struct BandSource {
    readers: Vec<FrameReader>,
    width: usize,
    height: usize,
}

struct FitsInfo { data_offset: u64, bitpix: i32, naxis: i32, w: usize, h: usize, naxis3: usize, bzero: f64, bscale: f64 }

/// Scan primary-header blocks for END; harvest the handful of numeric cards
/// the direct reader needs. Returns None for anything that should take the
/// decode-and-spill fallback (never errors on odd files — fallback covers them).
fn probe_fits(path: &Path) -> Option<FitsInfo> {
    let mut f = File::open(path).ok()?;
    let mut info = FitsInfo { data_offset: 0, bitpix: 0, naxis: 0, w: 0, h: 0, naxis3: 1, bzero: 0.0, bscale: 1.0 };
    let mut block = [0u8; BLOCK as usize];
    let mut blocks = 0u64;
    'outer: loop {
        f.read_exact(&mut block).ok()?;
        blocks += 1;
        for card in block.chunks(80) {
            let key = std::str::from_utf8(&card[..8]).ok()?.trim_end();
            if key == "END" { break 'outer; }
            let val = || -> Option<f64> {
                let s = std::str::from_utf8(&card[10..]).ok()?;
                let s = s.split('/').next()?.trim();
                s.parse::<f64>().ok()
            };
            match key {
                "BITPIX" => info.bitpix = val()? as i32,
                "NAXIS" => info.naxis = val()? as i32,
                "NAXIS1" => info.w = val()? as usize,
                "NAXIS2" => info.h = val()? as usize,
                "NAXIS3" => info.naxis3 = val()? as usize,
                "BZERO" => info.bzero = val()?,
                "BSCALE" => info.bscale = val()?,
                _ => {}
            }
        }
        if blocks > 64 { return None; } // headers beyond 64 blocks: fall back
    }
    info.data_offset = blocks * BLOCK;
    let ok_bitpix = matches!(info.bitpix, 8 | 16 | 32 | -32 | -64);
    if info.naxis == 2 && info.naxis3 == 1 && ok_bitpix && info.w > 0 && info.h > 0 {
        Some(info)
    } else {
        None
    }
}

/// BITPIX of a FITS file's primary HDU, for callers that need the source bit
/// depth without opening a full `BandSource` (`None`: unreadable / non-simple —
/// exactly the files the decode-and-spill fallback covers, whose original bit
/// depth this probe cannot speak for).
pub fn probe_bitpix(path: &Path) -> Option<i32> {
    probe_fits(path).map(|i| i.bitpix)
}

fn spill_via_read_raw(path: &Path, scratch_dir: &Path, idx: usize)
    -> Result<(File, usize, usize), IntegrationError>
{
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path)
        .map_err(|e| IntegrationError::Decode(format!("{}: {e:#}", path.display())))?;
    // Every consumer of this reader (master build AND light calibration) works
    // strictly per-(x,y) on a 1-channel plane, so the message must not claim to
    // be about calibration *frames* only — it surfaces verbatim to a user whose
    // LIGHT is the multi-channel file.
    if meta.channels != 1 {
        return Err(IntegrationError::BadInput(format!(
            "{}: {}-channel image — frames must be 1-channel for calibration (CFA mosaics stay 1-channel; debayered files cannot be calibrated)",
            path.display(), meta.channels
        )));
    }
    let (w, h) = (meta.width, meta.height);
    // `idx` alone repeats across concurrent `BandSource::open` calls in the
    // same process (each call restarts its own per-frame index at 0), so two
    // builds racing the spill fallback at the same `idx` can collide on this
    // path — one truncates/removes the other's scratch file mid-read, causing
    // silent pixel corruption (mirrors the fits_writer/writer.rs tmp-suffix
    // fix on this branch). A process-wide atomic sequence makes every spill
    // path unique regardless of caller overlap.
    static SPILL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SPILL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch_path: PathBuf = scratch_dir.join(format!("athint_scratch_{}_{seq}_{idx}.f32", std::process::id()));
    {
        let mut out = std::io::BufWriter::new(File::create(&scratch_path)?);
        use std::io::Write;
        match &pixels {
            astroimage::PixelData::Float32(v) => {
                for &x in v { out.write_all(&x.to_le_bytes())?; }
            }
            astroimage::PixelData::Uint16(v) => {
                for &x in v { out.write_all(&(x as f32).to_le_bytes())?; }
            }
        }
        out.flush()?;
    }
    let file = File::open(&scratch_path)?;
    // Unlink immediately: the open handle keeps the data readable (POSIX);
    // on Windows removal is deferred by the OS — acceptable for temp data.
    let _ = std::fs::remove_file(&scratch_path);
    Ok((file, w, h))
}

impl BandSource {
    pub fn open(paths: &[PathBuf], scratch_dir: &Path) -> Result<BandSource, IntegrationError> {
        if paths.is_empty() {
            return Err(IntegrationError::BadInput("empty frame list".into()));
        }
        let mut readers = Vec::with_capacity(paths.len());
        let mut dims: Option<(usize, usize)> = None;
        for (i, p) in paths.iter().enumerate() {
            let (reader, w, h) = match probe_fits(p) {
                Some(info) => (
                    FrameReader::Fits {
                        file: File::open(p)?,
                        data_offset: info.data_offset,
                        kind: plane_kind_for_bitpix(info.bitpix, info.bzero, info.bscale),
                    },
                    info.w, info.h,
                ),
                None => {
                    let (file, w, h) = spill_via_read_raw(p, scratch_dir, i)?;
                    (FrameReader::Scratch { file, kind: PlaneKind::F32Le }, w, h)
                }
            };
            match dims {
                None => dims = Some((w, h)),
                Some(d) if d != (w, h) => {
                    return Err(IntegrationError::BadInput(format!(
                        "dimension mismatch: {} is {w}x{h}, expected {}x{}",
                        p.display(), d.0, d.1
                    )));
                }
                _ => {}
            }
            readers.push(reader);
        }
        let (width, height) = dims.unwrap();
        Ok(BandSource { readers, width, height })
    }

    pub fn width(&self) -> usize { self.width }
    pub fn height(&self) -> usize { self.height }
    pub fn frame_count(&self) -> usize { self.readers.len() }

    pub(crate) fn plane_kinds(&self) -> Vec<PlaneKind> {
        self.readers.iter().map(|r| r.kind()).collect()
    }

    /// Source bytes `read_band` actually pulls off disk for one row, summed
    /// over every frame in this source: `width * bytes_per_sample` per
    /// frame, where `bytes_per_sample` is `bitpix.unsigned_abs() / 8` for a
    /// direct FITS reader and 4 (f32) for the decode-and-spill scratch
    /// fallback. This counts SOURCE bytes — the same bytes a `BandPlanes`
    /// buffer now holds, since Task 4 stopped widening on read.
    pub fn bytes_per_row(&self) -> usize {
        self.readers.iter().map(|r| self.width * r.kind().bytes_per_sample()).sum()
    }

    /// Rows per band whose band buffers (one per frame, held in the SOURCE's
    /// own byte width — see [`BandPlanes`]) fit `budget_bytes`. Counts every
    /// frame's OWN bytes-per-row, so a BITPIX 16 set gets twice the rows an
    /// f32 set does for the same budget, plus two f32 rows of headroom — the
    /// margin the old `frame_count + 2` formula carried as two phantom
    /// frames.
    ///
    /// Floor of 1: the floor must never override the budget (2026-08-02 audit
    /// I5) — at very large frame counts a 16-row floor grew band memory
    /// unbounded. One row per band is slow but bounded.
    pub fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
        let per_row: usize = self
            .readers
            .iter()
            .map(|r| self.width.saturating_mul(r.kind().bytes_per_sample()))
            .sum::<usize>()
            .saturating_add(self.width.saturating_mul(8))
            .max(1);
        (budget_bytes / per_row).max(1)
    }

    /// Reads rows `[y0, y0+rows)` of every frame straight into `out`'s own
    /// per-frame byte buffers — no decode here any more (Task 4): BZERO/BSCALE
    /// and endianness are applied lazily by [`BandPlanes::sample`] /
    /// `decode_row_into` / `decode_frame_into` on read.
    pub fn read_band(&mut self, y0: usize, rows: usize, out: &mut BandPlanes) -> Result<(), IntegrationError> {
        assert_eq!(out.bufs.len(), self.readers.len());
        let w = self.width;
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!("band {y0}+{rows} beyond height {}", self.height)));
        }
        for (reader, buf) in self.readers.iter_mut().zip(out.bufs.iter_mut()) {
            let (file, base_offset, bpp) = match reader {
                FrameReader::Fits { file, data_offset, kind } => (file, *data_offset, kind.bytes_per_sample()),
                FrameReader::Scratch { file, kind } => (file, 0u64, kind.bytes_per_sample()),
            };
            let need = rows * w * bpp;
            buf.resize(need, 0u8);
            file.seek(SeekFrom::Start(base_offset + (y0 * w * bpp) as u64))?;
            file.read_exact(buf)?;
        }
        out.rows = rows;
        Ok(())
    }
}

/// One band of every frame, held in the SOURCE's own sample format.
///
/// Before 2026-09-06 the band was `Vec<Vec<f32>>`, so a BITPIX 16 camera file
/// was widened on the way in and every band cost twice what the data does.
/// Holding raw bytes halves band memory for the common case, which buys twice
/// the rows for the same budget — i.e. half the seek rounds. The widening
/// happens per sample inside the parallel combine, where it is nearly free.
pub struct BandPlanes {
    bufs: Vec<Vec<u8>>,
    kinds: Vec<PlaneKind>,
    width: usize,
    rows: usize,
}

impl BandPlanes {
    pub fn new(src: &BandSource) -> BandPlanes {
        BandPlanes {
            bufs: vec![Vec::new(); src.frame_count()],
            kinds: src.plane_kinds(),
            width: src.width(),
            rows: 0,
        }
    }

    pub fn frame_count(&self) -> usize { self.kinds.len() }
    pub fn rows(&self) -> usize { self.rows }

    /// One decoded sample. `idx` is `row_in_band * width + x`.
    #[inline]
    pub fn sample(&self, frame: usize, idx: usize) -> f32 {
        self.kinds[frame].decode(&self.bufs[frame], idx)
    }

    /// Every frame's samples for one row of the band, frame-major:
    /// `dst[frame * width + x]`. `dst.len()` must be `frame_count * width`.
    pub fn decode_row_into(&self, row_in_band: usize, dst: &mut [f32]) {
        let w = self.width;
        assert_eq!(dst.len(), self.frame_count() * w, "decode_row_into: dst must be frame_count * width");
        for (i, kind) in self.kinds.iter().enumerate() {
            kind.decode_run(&self.bufs[i], row_in_band * w, &mut dst[i * w..(i + 1) * w]);
        }
    }

    /// One frame's whole band. `dst.len()` must be `rows * width`.
    pub fn decode_frame_into(&self, frame: usize, dst: &mut [f32]) {
        assert_eq!(dst.len(), self.rows * self.width, "decode_frame_into: dst must be rows * width");
        self.kinds[frame].decode_run(&self.bufs[frame], 0, dst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use std::io::Write;

    fn f32_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize, fill: impl Fn(usize, usize) -> f32) -> std::path::PathBuf {
        let mut data = vec![0f32; w * h];
        for y in 0..h { for x in 0..w { data[y * w + x] = fill(x, y); } }
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
        p
    }

    /// Minimal BITPIX=16 writer (unsigned convention: BZERO=32768, BSCALE=1)
    /// so the u16 fast path is covered without real camera files.
    fn u16_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize, val: u16) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut header = Vec::new();
        for line in [
            format!("{:<80}", "SIMPLE  =                    T"),
            format!("{:<80}", "BITPIX  =                   16"),
            format!("{:<80}", "NAXIS   =                    2"),
            format!("{:<80}", format!("NAXIS1  = {:>20}", w)),
            format!("{:<80}", format!("NAXIS2  = {:>20}", h)),
            format!("{:<80}", "BZERO   =              32768.0"),
            format!("{:<80}", "BSCALE  =                  1.0"),
            format!("{:<80}", "END"),
        ] { header.extend_from_slice(line.as_bytes()); }
        header.resize(2880, b' ');
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&header).unwrap();
        let raw: i16 = (val as i32 - 32768) as i16; // stored = (phys - BZERO)/BSCALE
        let mut data = Vec::with_capacity(w * h * 2);
        for _ in 0..w * h { data.extend_from_slice(&raw.to_be_bytes()); }
        let pad = (2880 - data.len() % 2880) % 2880;
        data.extend(std::iter::repeat(0u8).take(pad));
        f.write_all(&data).unwrap();
        p
    }

    #[test]
    fn planes_decode_identically_to_the_old_f32_path() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |x, y| (y * 32 + x) as f32);
        let p2 = u16_fixture(dir.path(), "b.fits", 32, 24, 1000);
        let mut src = BandSource::open(&[p1, p2], dir.path()).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band(10, 4, &mut planes).unwrap();

        assert_eq!(planes.frame_count(), 2);
        assert_eq!(planes.rows(), 4);
        // frame 0: f32 gradient; row 10 col 0 is 10*32
        assert_eq!(planes.sample(0, 0), (10 * 32) as f32);
        assert_eq!(planes.sample(0, 4 * 32 - 1), (13 * 32 + 31) as f32);
        // frame 1: BITPIX 16 with BZERO 32768 — physical value, not stored
        assert_eq!(planes.sample(1, 0), 1000.0);

        // decode_row_into agrees with sample(), frame-major
        let mut row = vec![0f32; 2 * 32];
        planes.decode_row_into(2, &mut row);
        for x in 0..32 {
            assert_eq!(row[x], planes.sample(0, 2 * 32 + x), "frame 0 col {x}");
            assert_eq!(row[32 + x], planes.sample(1, 2 * 32 + x), "frame 1 col {x}");
        }

        // decode_frame_into agrees too
        let mut whole = vec![0f32; 4 * 32];
        planes.decode_frame_into(0, &mut whole);
        for i in 0..4 * 32 {
            assert_eq!(whole[i], planes.sample(0, i), "sample {i}");
        }
    }

    #[test]
    fn u16_sources_get_twice_the_rows_of_f32_sources() {
        let dir = tempfile::tempdir().unwrap();
        let u16s: Vec<_> = (0..4)
            .map(|i| u16_fixture(dir.path(), &format!("u{i}.fits"), 100, 200, 500))
            .collect();
        let f32s: Vec<_> = (0..4)
            .map(|i| f32_fixture(dir.path(), &format!("f{i}.fits"), 100, 200, |_, _| 1.0))
            .collect();
        let budget = 1024 * 1024;
        let u_rows = BandSource::open(&u16s, dir.path()).unwrap().band_rows_for_budget(budget);
        let f_rows = BandSource::open(&f32s, dir.path()).unwrap().band_rows_for_budget(budget);
        assert!(
            u_rows > f_rows,
            "BITPIX 16 bands are half the bytes of f32 bands, so the same budget must buy more rows: {u_rows} vs {f_rows}"
        );
    }

    #[test]
    fn band_rows_floor_never_overrides_budget() {
        // 3000 frames of width 9576 (2026-08-02 audit I5): one row per band is
        // slow but bounded; the floor must yield to the budget.
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| u16_fixture(dir.path(), &format!("t{i}.fits"), 9576, 8, 1))
            .collect();
        let src = BandSource::open(&paths, dir.path()).unwrap();
        assert_eq!(src.band_rows_for_budget(1), 1, "budget of 1 byte still yields exactly one row");
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |_, _| 0.0);
        let p2 = f32_fixture(dir.path(), "b.fits", 16, 24, |_, _| 0.0);
        assert!(matches!(BandSource::open(&[p1, p2], dir.path()), Err(IntegrationError::BadInput(_))));
    }

    #[test]
    fn concurrent_spills_at_same_idx_never_cross_contaminate() {
        // Root cause: `idx` is a per-`BandSource::open`-call frame index that
        // restarts at 0 every call, so two concurrent builds racing the
        // decode-and-spill fallback (e.g. both integrating their first
        // XISF/nonstandard frame) can land on `idx == 0` at the same time.
        // Before the fix that meant an identical scratch path
        // (`athint_scratch_{pid}_{idx}.f32`) — one thread's create/write
        // could race the other's read/rename, silently corrupting pixels.
        //
        // Crafting a real non-FITS/fallback-triggering fixture is heavy, so
        // this pins the fix at the actual seam: call the private
        // `spill_via_read_raw` helper directly from two threads with the
        // SAME `idx`, many times, each spilling a distinct known fill value,
        // and assert every read-back is exactly its own thread's value —
        // never the other thread's, and never garbage from a torn write.
        let scratch = tempfile::tempdir().unwrap();
        let scratch_dir = scratch.path().to_path_buf();

        let run = |tag: f32, scratch_dir: PathBuf| {
            let fixtures = tempfile::tempdir().unwrap();
            for i in 0..25 {
                let p = f32_fixture(fixtures.path(), &format!("f{i}.fits"), 12, 10, move |_, _| tag);
                let (mut file, w, h) = spill_via_read_raw(&p, &scratch_dir, 0).unwrap();
                let mut buf = vec![0u8; w * h * 4];
                std::io::Read::read_exact(&mut file, &mut buf).unwrap();
                for c in buf.chunks_exact(4) {
                    let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    assert_eq!(v, tag, "read back the other thread's (or torn) scratch data");
                }
            }
        };

        let (sd1, sd2) = (scratch_dir.clone(), scratch_dir.clone());
        let t1 = std::thread::spawn(move || run(1.0, sd1));
        let t2 = std::thread::spawn(move || run(2.0, sd2));
        t1.join().unwrap();
        t2.join().unwrap();
    }

    #[test]
    fn probe_bitpix_reports_source_depth() {
        let dir = tempfile::tempdir().unwrap();
        let f32_file = f32_fixture(dir.path(), "float.fits", 8, 8, |_, _| 1.0);
        assert_eq!(probe_bitpix(&f32_file), Some(-32));
        let u16_file = u16_fixture(dir.path(), "u16.fits", 8, 8, 1000);
        assert_eq!(probe_bitpix(&u16_file), Some(16));
        // Unreadable / non-FITS: None, so callers keep their own default rather
        // than acting on a guessed depth.
        assert_eq!(probe_bitpix(&dir.path().join("missing.fits")), None);
        let not_fits = dir.path().join("notfits.bin");
        std::fs::write(&not_fits, vec![0u8; 4096]).unwrap();
        assert_eq!(probe_bitpix(&not_fits), None);
    }

    #[test]
    fn bytes_per_row_counts_source_bytes_not_widened_f32() {
        let dir = tempfile::tempdir().unwrap();
        // f32 (BITPIX=-32): 4 bytes/sample, source == widened width.
        let f32_file = f32_fixture(dir.path(), "float.fits", 10, 4, |_, _| 1.0);
        // u16 (BITPIX=16): 2 bytes/sample on disk, even though the decoded
        // sample is f32.
        let u16_file = u16_fixture(dir.path(), "u16.fits", 10, 4, 1000);
        let src = BandSource::open(&[f32_file, u16_file], dir.path()).unwrap();
        assert_eq!(src.bytes_per_row(), 10 * 4 + 10 * 2);
    }
}
