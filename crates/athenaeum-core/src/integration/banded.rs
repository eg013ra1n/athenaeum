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

enum FrameReader {
    /// Direct seek-read of an uncompressed single-HDU FITS.
    Fits { file: File, data_offset: u64, bitpix: i32, bzero: f64, bscale: f64 },
    /// Raw little-endian f32 scratch spill (one full frame, row-major).
    Scratch { file: File },
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
    if meta.channels != 1 {
        return Err(IntegrationError::BadInput(format!(
            "{}: {}-channel image — calibration frames must be 1-channel (CFA mosaics included)",
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
                        bitpix: info.bitpix,
                        bzero: info.bzero,
                        bscale: info.bscale,
                    },
                    info.w, info.h,
                ),
                None => {
                    let (file, w, h) = spill_via_read_raw(p, scratch_dir, i)?;
                    (FrameReader::Scratch { file }, w, h)
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

    /// Reads rows [y0, y0+rows) of every frame into out[i] (len = rows*width),
    /// BZERO/BSCALE applied, native f32, no stretch, CFA untouched.
    pub fn read_band(&mut self, y0: usize, rows: usize, out: &mut [Vec<f32>]) -> Result<(), IntegrationError> {
        assert_eq!(out.len(), self.readers.len());
        let w = self.width;
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!("band {y0}+{rows} beyond height {}", self.height)));
        }
        for (reader, dst) in self.readers.iter_mut().zip(out.iter_mut()) {
            dst.clear();
            dst.reserve(rows * w);
            match reader {
                FrameReader::Fits { file, data_offset, bitpix, bzero, bscale } => {
                    let bpp = (bitpix.unsigned_abs() as usize) / 8;
                    let mut buf = vec![0u8; rows * w * bpp];
                    file.seek(SeekFrom::Start(*data_offset + (y0 * w * bpp) as u64))?;
                    file.read_exact(&mut buf)?;
                    let (bz, bs) = (*bzero as f32, *bscale as f32);
                    match *bitpix {
                        16 => for c in buf.chunks_exact(2) {
                            let raw = i16::from_be_bytes([c[0], c[1]]) as f32;
                            dst.push(raw * bs + bz);
                        },
                        -32 => for c in buf.chunks_exact(4) {
                            let raw = f32::from_be_bytes([c[0], c[1], c[2], c[3]]);
                            dst.push(raw * bs + bz);
                        },
                        32 => for c in buf.chunks_exact(4) {
                            let raw = i32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f32;
                            dst.push(raw * bs + bz);
                        },
                        -64 => for c in buf.chunks_exact(8) {
                            let raw = f64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                            dst.push((raw * *bscale + *bzero) as f32);
                        },
                        8 => for &b in buf.iter() {
                            dst.push(b as f32 * bs + bz);
                        },
                        other => return Err(IntegrationError::BadInput(format!("BITPIX {other}"))),
                    }
                }
                FrameReader::Scratch { file } => {
                    let mut buf = vec![0u8; rows * w * 4];
                    file.seek(SeekFrom::Start((y0 * w * 4) as u64))?;
                    file.read_exact(&mut buf)?;
                    for c in buf.chunks_exact(4) {
                        dst.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Band height so that (frame_count+2) * band_rows * width * 4 bytes stays
/// under budget_bytes (default caller passes 256 MiB), min 16 rows.
pub fn band_rows_for_budget(width: usize, frame_count: usize, budget_bytes: usize) -> usize {
    let per_row = (frame_count + 2).saturating_mul(width).saturating_mul(4).max(1);
    (budget_bytes / per_row).max(16)
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
    fn reads_f32_fits_bands_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |x, y| (y * 32 + x) as f32);
        let p2 = f32_fixture(dir.path(), "b.fits", 32, 24, |_, _| 7.0);
        let mut src = BandSource::open(&[p1, p2], dir.path()).unwrap();
        assert_eq!((src.width(), src.height(), src.frame_count()), (32, 24, 2));
        let mut out = vec![Vec::new(), Vec::new()];
        src.read_band(10, 4, &mut out).unwrap();
        assert_eq!(out[0].len(), 4 * 32);
        assert_eq!(out[0][0], (10 * 32) as f32);        // row 10, col 0
        assert_eq!(out[0][4 * 32 - 1], (13 * 32 + 31) as f32);
        assert!(out[1].iter().all(|&v| v == 7.0));
    }

    #[test]
    fn u16_bzero_applied() {
        let dir = tempfile::tempdir().unwrap();
        let p = u16_fixture(dir.path(), "d.fits", 16, 8, 1000);
        let mut src = BandSource::open(&[p], dir.path()).unwrap();
        let mut out = vec![Vec::new()];
        src.read_band(0, 8, &mut out).unwrap();
        assert!(out[0].iter().all(|&v| v == 1000.0), "physical = stored*BSCALE + BZERO");
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
    fn band_rows_budget_math() {
        // 100 frames of width 6248, budget 256 MiB:
        // rows = 256MiB / ((100+2) * 6248 * 4) ≈ 105 — must be >= 16 and <= height cap by caller.
        let rows = band_rows_for_budget(6248, 100, 256 * 1024 * 1024);
        assert!(rows >= 16 && rows <= 256, "{rows}");
        assert_eq!(band_rows_for_budget(10, 1, usize::MAX), usize::MAX.min(band_rows_for_budget(10, 1, usize::MAX))); // no panic on huge budgets
    }
}
