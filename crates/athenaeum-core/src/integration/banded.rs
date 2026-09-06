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
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

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

    /// Fill `buf` from `offset`. Positional — no seek, no cursor — so several
    /// bands (and several frames of one band) can be read from a shared
    /// `&BandSource` at once.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let (file, base) = match self {
            FrameReader::Fits { file, data_offset, .. } => (file, *data_offset),
            FrameReader::Scratch { file, .. } => (file, 0),
        };
        #[cfg(unix)]
        {
            file.read_exact_at(buf, base + offset)
        }
        #[cfg(windows)]
        {
            // `seek_read` may return a short read; loop until the buffer is
            // full or the file ends. There is no `read_exact_at` on Windows.
            //
            // `seek_read` also moves the file's cursor, unlike a true `pread`
            // — safe here ONLY because nothing in this module reads through
            // a cursor any more (fix round 1, M5): every read in this file is
            // positional, so a moved cursor is never observed. Concurrent
            // calls on one handle are still safe because each `seek_read`
            // carries its own offset rather than relying on the moved
            // cursor. Do NOT reintroduce a seek-based (`Seek`/`SeekFrom`)
            // read anywhere in this module — it would silently race this
            // cursor against concurrent `read_exact_at` calls on the same
            // `File`, on Windows only, invisibly to every CI job (the
            // Windows build only runs on a tag, never a branch push).
            let mut done = 0usize;
            while done < buf.len() {
                let n = file.seek_read(&mut buf[done..], base + offset + done as u64)?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "short read while filling a band",
                    ));
                }
                done += n;
            }
            Ok(())
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

/// Test-only observation hook (fix round 1, I1 regression pin): records
/// which thread called `spill_via_read_raw`, keyed by path so a test can
/// look up its OWN call even if some other test is spilling concurrently in
/// the same process (cargo runs tests in parallel by default, and this is a
/// process-wide `static` — keying by path, unique per test's own `tempdir`,
/// is what keeps two tests' recordings from clobbering each other rather
/// than trusting "the last one" globally). Compiled only under `#[cfg(test)]`
/// — this is pure test instrumentation, never a release-build concern.
#[cfg(test)]
static SPILL_THREAD_LOG: std::sync::Mutex<Vec<(PathBuf, std::thread::ThreadId)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn spill_thread_for(path: &Path) -> Option<std::thread::ThreadId> {
    SPILL_THREAD_LOG.lock().unwrap().iter().rev().find(|(p, _)| p == path).map(|(_, t)| *t)
}

fn spill_via_read_raw(path: &Path, scratch_dir: &Path, idx: usize)
    -> Result<(File, usize, usize), IntegrationError>
{
    #[cfg(test)]
    SPILL_THREAD_LOG.lock().unwrap().push((path.to_path_buf(), std::thread::current().id()));

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

/// What a probe worker found for one path — the header-probe part only.
///
/// Fix round 1 (I1): the decode-and-spill fallback used to run INSIDE the
/// probed worker (the old `open_one`'s `None` arm called `spill_via_read_raw`
/// directly), so it ran on all `concurrency` workers at once.
/// `spill_via_read_raw` decodes an entire frame into RAM via
/// `ImageConverter::read_raw` — ~104 MB per frame per the throughput spec —
/// so that was up to `concurrency` decoded frames alive simultaneously
/// (~1.7 GB locally, ~7.8 GB under the network policy), none of which the
/// band budget this whole cycle exists to bound would ever see, for a
/// fallback that used to cost one frame at a time before this task. The
/// concurrency `open` adds is for `probe_fits`'s round trips (an `open` plus
/// a couple of small reads), never for the spill — so a `NeedsSpill` verdict
/// carries no reader yet, and `open` (below) performs the actual spill
/// serially, on the calling thread, after every probe worker has joined. Do
/// not fold the spill back into a worker.
enum ProbeOutcome {
    Fits(FrameReader, usize, usize),
    NeedsSpill,
}

/// Splits `n` items round-robin into `workers` groups (item `i` goes to
/// group `i % workers`), used by both `BandSource::open` and
/// `BandSource::read_band` to hand disjoint work to their scoped-thread
/// workers.
///
/// Fix round 1, I2: this replaces a contiguous `n.div_ceil(workers)`-sized
/// chunk split, which silently produced FEWER than `workers` non-empty
/// chunks whenever `workers` did not divide `n` evenly — 100 items at
/// `workers=32` gave chunks of `ceil(100/32) = 4`, hence only
/// `ceil(100/4) = 25` non-empty chunks, a 22% shortfall on exactly the
/// network read-concurrency case this exists for. Round-robin always
/// produces exactly `min(n, workers)` non-empty groups (pinned by
/// `round_robin_groups_never_under_fills_a_non_divisor_pair` below).
fn round_robin_groups<T>(items: impl IntoIterator<Item = T>, workers: usize) -> Vec<Vec<T>> {
    let mut groups: Vec<Vec<T>> = (0..workers).map(|_| Vec::new()).collect();
    for (i, item) in items.into_iter().enumerate() {
        groups[i % workers].push(item);
    }
    groups
}

/// Header-probe only — never spills. See [`ProbeOutcome`]'s doc comment for
/// why the spill lives outside this function.
fn probe_one(p: &Path) -> Result<ProbeOutcome, IntegrationError> {
    match probe_fits(p) {
        Some(info) => Ok(ProbeOutcome::Fits(
            FrameReader::Fits {
                file: File::open(p)?,
                data_offset: info.data_offset,
                kind: plane_kind_for_bitpix(info.bitpix, info.bzero, info.bscale),
            },
            info.w, info.h,
        )),
        None => Ok(ProbeOutcome::NeedsSpill),
    }
}

impl BandSource {
    /// Opens every path's reader. `probe_fits` is an `open` plus a couple of
    /// small reads PER FILE, so on a network mount a 100-frame set pays 100
    /// serial round trips before a single pixel is read — measured at 0.55s
    /// even locally. Probing across `concurrency` scoped threads amortizes
    /// that latency the same way `read_band` amortizes the per-band reads.
    /// The decode-and-spill fallback is deliberately NOT part of that
    /// parallel work — see [`ProbeOutcome`]'s doc comment.
    ///
    /// Readers are assembled back in the caller's `paths` order regardless of
    /// which worker finished first or last: every path has a fixed slot
    /// (indexed by its position in `paths`) that only its own round-robin
    /// owner ever writes, so completion order never leaks into reader order.
    /// That order is load-bearing — `bad_samples_per_frame` is indexed by it,
    /// and so is the per-pixel combine's column.
    ///
    /// Workers are assigned round-robin (`i % workers`), fix round 1 (I2),
    /// not contiguous chunks: a contiguous `n.div_ceil(workers)`-sized split
    /// silently under-fills whenever `workers` doesn't divide `n` evenly —
    /// 100 paths at `workers=32` gives chunk size 4, hence only 25 non-empty
    /// chunks, a 22% shortfall on exactly the network case this concurrency
    /// exists for. Round-robin always produces `min(n, workers)` non-empty
    /// groups.
    pub fn open(paths: &[PathBuf], scratch_dir: &Path, concurrency: usize) -> Result<BandSource, IntegrationError> {
        if paths.is_empty() {
            return Err(IntegrationError::BadInput("empty frame list".into()));
        }
        let n = paths.len();
        let workers = concurrency.max(1).min(n);

        let mut slots: Vec<Option<Result<ProbeOutcome, IntegrationError>>> = (0..n).map(|_| None).collect();
        let mut panicked = false;
        {
            let groups = round_robin_groups(paths.iter().zip(slots.iter_mut()), workers);
            std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(workers);
                for group in groups {
                    handles.push(scope.spawn(move || {
                        for (p, slot) in group {
                            *slot = Some(probe_one(p));
                        }
                    }));
                }
                for h in handles {
                    // A panicked probe thread must surface as an error, never
                    // as a silently missing reader — its slots stay `None`
                    // below, which the assembly loop turns into a hard error.
                    if h.join().is_err() {
                        panicked = true;
                    }
                }
            });
        }
        if panicked {
            return Err(IntegrationError::BadInput("a frame probe thread panicked".into()));
        }

        let mut readers = Vec::with_capacity(n);
        let mut dims: Option<(usize, usize)> = None;
        for (idx, (p, slot)) in paths.iter().zip(slots.into_iter()).enumerate() {
            let outcome = slot.ok_or_else(|| {
                IntegrationError::BadInput(format!("{}: never probed (owning thread panicked)", p.display()))
            })??;
            let (reader, w, h) = match outcome {
                ProbeOutcome::Fits(reader, w, h) => (reader, w, h),
                ProbeOutcome::NeedsSpill => {
                    // Serial, on THIS (the calling) thread — see
                    // `ProbeOutcome`'s doc comment. `idx` is this path's
                    // position in `paths`, matching the pre-parallel loop's
                    // per-`open`-call frame index exactly; path order is
                    // unaffected by which worker probed it.
                    let (file, w, h) = spill_via_read_raw(p, scratch_dir, idx)?;
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
    /// frame's OWN bytes-per-row, so a BITPIX 16 set gets MORE rows than an
    /// f32 set for the same budget, on top of a fixed `width * 8` bytes of
    /// headroom (replacing the old `frame_count + 2` phantom-frame margin).
    /// That headroom is a bigger share of a small set's per-row cost, so the
    /// ratio is `(4n+8)/(2n+8)` for n frames, not a flat 2x: 1.5x at n=4,
    /// 1.93x at n=50 — asymptotically twice the rows, but only in the limit.
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
    ///
    /// `concurrency` comes from `IoPolicy` (Task 5) and is deliberately NOT
    /// the rayon pool's width. A rayon pool caps parallelism at its own
    /// thread count, which is derived from CPU cores — and a network mount is
    /// latency-bound, so it needs MORE outstanding reads than the machine has
    /// cores to fill the link at all. Scoped OS threads give a pool-independent
    /// count — `min(frame_count, concurrency)` in flight, exactly, via
    /// round-robin assignment (fix round 1, I2; a contiguous chunk split
    /// under-fills whenever `concurrency` doesn't divide the frame count) —
    /// cost ~10-20 us to spawn against a band read measured in seconds, and
    /// are safe to use from inside a `pool.install(..)` (they are not rayon
    /// workers, so they cannot deadlock against the pool that called in).
    /// Taking `&self` (not `&mut self`) is what makes that safe: several
    /// bands, or several frames of one band, can be read from the same
    /// shared `BandSource` at once because there is no cursor to race —
    /// every read is positional.
    pub fn read_band(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
    ) -> Result<(), IntegrationError> {
        assert_eq!(out.bufs.len(), self.readers.len());
        // `out`'s `width`/`kinds` are snapshotted once at `BandPlanes::new` and
        // never re-derived here, so a `BandPlanes` built from a DIFFERENT
        // `BandSource` with the same frame count would otherwise decode with
        // the wrong row stride and the wrong kinds — silently, no panic. The
        // frame-count check above cannot catch that; this one can, at debug
        // cost only (production never pays for it, same as any other
        // debug_assert invariant check in this crate).
        debug_assert_eq!(
            out.width, self.width,
            "read_band: BandPlanes width {} does not match this BandSource's width {} — \
             was `out` built from a different BandSource?",
            out.width, self.width
        );
        let w = self.width;
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!("band {y0}+{rows} beyond height {}", self.height)));
        }
        out.rows = rows;

        let n = self.readers.len();
        // Fix round 1, M4: `n == 0` cannot happen through `BandSource::open`
        // (it rejects an empty path list before a `BandSource` ever exists),
        // but the guard used to be a `.max(1)` dressed up as protection while
        // still handing `per = n.div_ceil(workers) == 0` to a chunking call
        // that panics on it. An explicit early return says plainly what is
        // and isn't guaranteed, and costs nothing on the real path.
        if n == 0 {
            return Ok(());
        }
        let workers = concurrency.max(1).min(n);

        // Fix round 1, M3: every calibration/precal call site (light_cal.rs,
        // cosmetic.rs, `masters::load_precal_pixels`) and every test passes
        // `concurrency == 1` — 1-3 frame reads with no `IoPolicy` in scope
        // (spec §8). Skip `thread::scope` entirely for that case: it is a
        // straight loop identical to what this method did before this task,
        // and `scope.spawn` panics if the OS refuses a new thread, which
        // would otherwise turn a bare single-frame read into a new panic
        // path for zero parallelism gained.
        if workers == 1 {
            for (reader, buf) in self.readers.iter().zip(out.bufs.iter_mut()) {
                let bpp = reader.kind().bytes_per_sample();
                buf.resize(rows * w * bpp, 0u8);
                reader.read_exact_at(buf, (y0 * w * bpp) as u64)?;
            }
            return Ok(());
        }

        // Round-robin, not contiguous chunks (fix round 1, I2) — see
        // `round_robin_groups`'s doc comment. Always produces exactly
        // `workers` non-empty groups here (having passed the `workers == 1`
        // fast path above, and `workers` never exceeds `n`).
        let groups = round_robin_groups(self.readers.iter().zip(out.bufs.iter_mut()), workers);

        let mut errors: Vec<IntegrationError> = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for group in groups {
                handles.push(scope.spawn(move || -> Result<(), IntegrationError> {
                    for (reader, buf) in group {
                        let bpp = reader.kind().bytes_per_sample();
                        buf.resize(rows * w * bpp, 0u8);
                        reader.read_exact_at(buf, (y0 * w * bpp) as u64)?;
                    }
                    Ok(())
                }));
            }
            for h in handles {
                match h.join() {
                    Ok(Err(e)) => errors.push(e),
                    // A panicked reader must surface as an error, never as a
                    // silently short band.
                    Err(_) => errors.push(IntegrationError::BadInput(
                        "a band reader thread panicked".into(),
                    )),
                    Ok(Ok(())) => {}
                }
            }
        });
        // Fix round 1, M2: report every discarded error, not just the one
        // returned below — losing three frames mid-read used to surface only
        // one of them, the other two leaving no trace, against the repo's
        // never-swallow-an-error rule.
        if !errors.is_empty() {
            let mut errors = errors.into_iter();
            let first = errors.next().unwrap();
            for e in errors {
                tracing::warn!(error = %e, "read_band: reader-thread failure discarded in favor of an earlier one");
            }
            return Err(first);
        }
        Ok(())
    }
}

/// One band of every frame, held in the SOURCE's own sample format.
///
/// Before 2026-09-06 the band was `Vec<Vec<f32>>`, so a BITPIX 16 camera file
/// was widened on the way in and every band cost twice what the data does.
/// Holding raw bytes halves band memory for the common case, which buys MORE
/// rows for the same budget — not a flat 2x (see [`BandSource::band_rows_for_budget`]
/// for the real, headroom-adjusted ratio), i.e. fewer read rounds. The
/// widening happens per sample inside the parallel combine, where it is
/// nearly free.
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

    /// Fix round 1, I2: a contiguous `n.div_ceil(workers)`-sized chunk split
    /// gives FEWER than `workers` non-empty chunks whenever `workers` does
    /// not divide `n` evenly. The reviewer's exact example: 100 items at 32
    /// workers used to give chunks of `ceil(100/32) = 4`, hence only
    /// `ceil(100/4) = 25` non-empty chunks — a 22% shortfall on exactly the
    /// network read-concurrency case this exists for (`n=100, c=32` do not
    /// divide evenly, unlike the 1/4/10/20-reader timing sweep, which can't
    /// show this because every one of those divides 100 evenly). This is a
    /// direct assertion on the grouping itself, not a timing measurement.
    #[test]
    fn round_robin_groups_never_under_fills_a_non_divisor_pair() {
        let groups = round_robin_groups(0..100usize, 32);
        assert_eq!(groups.len(), 32, "one Vec per worker, always");
        assert_eq!(
            groups.iter().filter(|g| !g.is_empty()).count(),
            32,
            "every one of the 32 workers must get at least one item — the old contiguous \
             chunk split only ever populated 25 of them for this exact (n, workers) pair"
        );
        // Every item lands in exactly one group: none dropped, none duplicated.
        let mut seen: Vec<usize> = groups.into_iter().flatten().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..100).collect::<Vec<_>>());
    }

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

    /// A FITS file `probe_fits` rejects — bogus `NAXIS3` on an otherwise
    /// ordinary 2D f32 image — but `astroimage::ImageConverter::read_raw`
    /// (the decode-and-spill fallback's decoder) reads without complaint,
    /// since `NAXIS < 3` makes it ignore NAXIS3 entirely. The cheapest of
    /// `probe_fits`'s four rejection routes (a >64-block header, `NAXIS !=
    /// 2`, `NAXIS3 != 1`, or an out-of-set `BITPIX` all take the same
    /// fallback) — no XISF, no >64-block header padding, no RGB (which
    /// would trip `spill_via_read_raw`'s own channels-must-be-1 check).
    fn f32_needs_spill_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize, fill: impl Fn(usize, usize) -> f32) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut header = Vec::new();
        for line in [
            format!("{:<80}", "SIMPLE  =                    T"),
            format!("{:<80}", format!("BITPIX  = {:>20}", -32)),
            format!("{:<80}", format!("NAXIS   = {:>20}", 2)),
            format!("{:<80}", format!("NAXIS1  = {:>20}", w)),
            format!("{:<80}", format!("NAXIS2  = {:>20}", h)),
            // The bogus card: `probe_fits` requires `naxis3 == 1` (its
            // default when the card is absent) whenever `naxis == 2`.
            // `NAXIS < 3` means `astroimage`'s reader ignores this entirely.
            format!("{:<80}", format!("NAXIS3  = {:>20}", 2)),
            format!("{:<80}", "END"),
        ] { header.extend_from_slice(line.as_bytes()); }
        header.resize(2880, b' ');
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&header).unwrap();
        let mut data = Vec::with_capacity(w * h * 4);
        for y in 0..h { for x in 0..w { data.extend_from_slice(&fill(x, y).to_be_bytes()); } }
        let pad = (2880 - data.len() % 2880) % 2880;
        data.extend(std::iter::repeat(0u8).take(pad));
        f.write_all(&data).unwrap();
        p
    }

    /// Fix round 1, I1 (round 2): pins the two-phase probe/spill shape
    /// end-to-end through the PUBLIC `open`/`read_band` API. The only
    /// existing spill test, `concurrent_spills_at_same_idx_never_cross_contaminate`,
    /// calls the private `spill_via_read_raw` directly and so proves nothing
    /// about `ProbeOutcome::NeedsSpill`/`probe_one` or about which thread
    /// ends up running the spill.
    ///
    /// Two things are pinned, at `concurrency = 4` so the real multi-worker
    /// probe path runs:
    /// - FUNCTIONAL: the spilled frame's pixels come back correct AT ITS
    ///   ORIGINAL INDEX (placed non-edge — index 2 of 5 — so a reordering
    ///   bug would not hide at a first/last boundary), alongside its
    ///   ordinary neighbours.
    /// - STRUCTURAL, the one that actually protects I1: a functional-only
    ///   test would still pass if someone moved the spill back onto a probe
    ///   worker — it only checks pixels, and the spill produces byte-identical
    ///   output regardless of which thread runs it. `spill_thread_for`
    ///   (backed by the `#[cfg(test)]`-only `SPILL_THREAD_LOG` hook in
    ///   `spill_via_read_raw`) asserts the spill ran on THIS test's own
    ///   thread — `open`'s single-threaded post-join assembly loop — never
    ///   on a probe worker.
    #[test]
    fn decode_and_spill_runs_on_the_caller_not_a_probe_worker() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16, 12);
        let spilled_path = dir.path().join("spilled.fits");
        let bases = [0.0f32, 1000.0, 5000.0, 2000.0, 3000.0];
        let paths = vec![
            f32_fixture(dir.path(), "a.fits", w, h, move |x, y| bases[0] + (y * w + x) as f32),
            f32_fixture(dir.path(), "b.fits", w, h, move |x, y| bases[1] + (y * w + x) as f32),
            // Non-edge position — index 2 of 5 — where a probe/spill
            // ordering bug would not hide the way it might at index 0 or 4.
            f32_needs_spill_fixture(dir.path(), "spilled.fits", w, h, move |x, y| bases[2] + (y * w + x) as f32),
            f32_fixture(dir.path(), "c.fits", w, h, move |x, y| bases[3] + (y * w + x) as f32),
            f32_fixture(dir.path(), "d.fits", w, h, move |x, y| bases[4] + (y * w + x) as f32),
        ];
        assert_eq!(paths[2], spilled_path);

        let src = BandSource::open(&paths, dir.path(), 4).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band(0, h, &mut planes, 4).unwrap();

        // Functional: every frame's pixels are correct at its ORIGINAL index.
        for (i, &base) in bases.iter().enumerate() {
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        planes.sample(i, y * w + x),
                        base + (y * w + x) as f32,
                        "frame {i} ({x},{y}): pixel mismatch — a probe/spill ordering bug would show up here"
                    );
                }
            }
        }

        // Structural: the spill ran on THIS test's own thread, never a probe
        // worker — the assertion a functional-only pixel check cannot make.
        let spill_thread = spill_thread_for(&spilled_path)
            .expect("spill_via_read_raw must have run exactly once for this fixture");
        assert_eq!(
            spill_thread,
            std::thread::current().id(),
            "the decode-and-spill fallback must run on open()'s single-threaded assembly loop, \
             never on a probe worker thread (fix round 1, I1)"
        );
    }

    #[test]
    fn bands_are_read_positionally_from_a_shared_source() {
        // Reads must be positional: `pread` per frame, no shared cursor, so
        // the frames can be filled in parallel. Pinned by reading the SAME
        // source twice from a shared reference — which does not compile
        // against a `&mut self` reader, and would interleave cursors against
        // a seeking one.
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..8)
            .map(|i| f32_fixture(dir.path(), &format!("p{i}.fits"), 64, 40, move |x, y| (i * 10_000 + y * 64 + x) as f32))
            .collect();
        let src = BandSource::open(&paths, dir.path(), 4).unwrap();

        let read = |y0: usize, rows: usize| {
            let mut planes = BandPlanes::new(&src);
            src.read_band(y0, rows, &mut planes, 4).unwrap();
            (0..planes.frame_count())
                .map(|f| (0..rows * 64).map(|i| planes.sample(f, i)).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };

        let a = read(8, 4);
        let b = read(8, 4);
        assert_eq!(a, b, "the same band must read identically — a shared cursor would drift");
        for (f, plane) in a.iter().enumerate() {
            assert_eq!(plane[0], (f * 10_000 + 8 * 64) as f32, "frame {f} row 8 col 0");
        }

        // Two bands read concurrently off the same &BandSource.
        let (c, d) = rayon::join(|| read(0, 4), || read(20, 4));
        assert_eq!(c[3][0], (3 * 10_000) as f32);
        assert_eq!(d[3][0], (3 * 10_000 + 20 * 64) as f32);
    }

    #[test]
    fn planes_decode_identically_to_the_old_f32_path() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |x, y| (y * 32 + x) as f32);
        let p2 = u16_fixture(dir.path(), "b.fits", 32, 24, 1000);
        let src = BandSource::open(&[p1, p2], dir.path(), 1).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band(10, 4, &mut planes, 1).unwrap();

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
    fn u16_sources_get_more_rows_than_f32_sources() {
        let dir = tempfile::tempdir().unwrap();
        let u16s: Vec<_> = (0..4)
            .map(|i| u16_fixture(dir.path(), &format!("u{i}.fits"), 100, 200, 500))
            .collect();
        let f32s: Vec<_> = (0..4)
            .map(|i| f32_fixture(dir.path(), &format!("f{i}.fits"), 100, 200, |_, _| 1.0))
            .collect();
        let budget = 1024 * 1024;
        let u_rows = BandSource::open(&u16s, dir.path(), 1).unwrap().band_rows_for_budget(budget);
        let f_rows = BandSource::open(&f32s, dir.path(), 1).unwrap().band_rows_for_budget(budget);
        assert!(
            u_rows > f_rows,
            "BITPIX 16 bands are half the bytes of f32 bands, so the same budget must buy more rows: {u_rows} vs {f_rows}"
        );
    }

    #[test]
    fn band_rows_floor_never_overrides_budget() {
        // Width 9576 matches the real corpus that surfaced the bug (2026-08-02
        // audit I5); this fixture uses a small frame count (4), not that
        // corpus's actual ~3000, since only the per-row width drives the
        // formula under test. One row per band is slow but bounded; the floor
        // must yield to the budget regardless of frame count.
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| u16_fixture(dir.path(), &format!("t{i}.fits"), 9576, 8, 1))
            .collect();
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        assert_eq!(src.band_rows_for_budget(1), 1, "budget of 1 byte still yields exactly one row");
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |_, _| 0.0);
        let p2 = f32_fixture(dir.path(), "b.fits", 16, 24, |_, _| 0.0);
        assert!(matches!(BandSource::open(&[p1, p2], dir.path(), 1), Err(IntegrationError::BadInput(_))));
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
        let src = BandSource::open(&[f32_file, u16_file], dir.path(), 1).unwrap();
        assert_eq!(src.bytes_per_row(), 10 * 4 + 10 * 2);
    }

    // ── Task 4 fix-round-1 M4: table-driven pin for every `PlaneKind` arm ──
    //
    // The multi-band measurement gate is a BITPIX 16 corpus, so it only ever
    // exercises `I16Be`. `planes_decode_identically_to_the_old_f32_path`
    // above only exercises `F32Be`/`I16Be` too, and
    // `concurrent_spills_at_same_idx_never_cross_contaminate` pins the spill
    // *writer*'s little-endian encoding, not the `F32Le` *reader* arm. That
    // left `U8`, `I32Be`, `F64Be` and `F32Le` with inspection only behind a
    // byte-identical-pixels constraint. These tests hand-build byte arrays
    // (no FITS fixtures) and check both `decode` and `decode_run` for all six
    // variants.

    /// `decode` and `decode_run` must agree, sample by sample, over every
    /// sample in `bytes` — `decode_run` is the bulk path every production
    /// caller reaches through (`decode_frame_into`/`decode_row_into`), so it
    /// must never diverge from the scalar path `sample()` uses.
    fn assert_decode_and_run_agree(kind: PlaneKind, bytes: &[u8], n: usize) {
        let mut run = vec![0f32; n];
        kind.decode_run(bytes, 0, &mut run);
        for i in 0..n {
            assert_eq!(kind.decode(bytes, i), run[i], "decode/decode_run disagree at sample {i}");
        }
    }

    #[test]
    fn u8_applies_bscale_and_bzero() {
        // Non-unity scale AND non-zero offset: dropping either one changes
        // the result, so a lost `bscale` or a lost `bzero` both fail this.
        let kind = PlaneKind::U8 { bzero: 1.0, bscale: 2.5 };
        let bytes = [10u8, 20, 30];
        assert_eq!(kind.decode(&bytes, 0), 10.0 * 2.5 + 1.0);
        assert_eq!(kind.decode(&bytes, 1), 20.0 * 2.5 + 1.0);
        assert_eq!(kind.decode(&bytes, 2), 30.0 * 2.5 + 1.0);
        assert_decode_and_run_agree(kind, &bytes, 3);
    }

    #[test]
    fn i16be_negative_stored_value_with_unsigned_convention() {
        // The real unsigned-16 FITS convention: BZERO=32768, BSCALE=1, and
        // the on-disk value is SIGNED, with the negative half of the range
        // representing the upper half of the unsigned range. -500 stored
        // means physical 32268 — a wrong-endian read or a dropped BZERO both
        // fail this.
        let kind = PlaneKind::I16Be { bzero: 32768.0, bscale: 1.0 };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(-500i16).to_be_bytes());
        bytes.extend_from_slice(&(1000i16).to_be_bytes());
        assert_eq!(kind.decode(&bytes, 0), 32268.0);
        assert_eq!(kind.decode(&bytes, 1), 33768.0);
        assert_decode_and_run_agree(kind, &bytes, 2);
    }

    #[test]
    fn i32be_negative_stored_value() {
        let kind = PlaneKind::I32Be { bzero: 7.0, bscale: 3.0 };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(-123456i32).to_be_bytes());
        bytes.extend_from_slice(&(654321i32).to_be_bytes());
        assert_eq!(kind.decode(&bytes, 0), -123456.0 * 3.0 + 7.0);
        assert_eq!(kind.decode(&bytes, 1), 654321.0 * 3.0 + 7.0);
        assert_decode_and_run_agree(kind, &bytes, 2);
    }

    #[test]
    fn f32be_byte_order_matters() {
        // A value whose big-endian and little-endian byte patterns differ, so
        // a byte-order slip in the decode arm is caught rather than passing
        // by coincidence (a byte-palindromic bit pattern would not catch it).
        // Two distinct samples so `decode_run` is checked over an actual run,
        // not just a single-sample slice.
        let kind = PlaneKind::F32Be { bzero: 0.0, bscale: 1.0 };
        let v1: f32 = 12345.625;
        let v2: f32 = -876.5;
        let be1 = v1.to_be_bytes();
        let le1 = v1.to_le_bytes();
        assert_ne!(be1, le1, "test value's BE/LE byte patterns must differ to be a real test");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&be1);
        bytes.extend_from_slice(&v2.to_be_bytes());
        assert_eq!(kind.decode(&bytes, 0), v1);
        assert_eq!(kind.decode(&bytes, 1), v2);
        assert_decode_and_run_agree(kind, &bytes, 2);
    }

    #[test]
    fn f64be_casts_only_at_the_end() {
        // Catastrophic cancellation: `raw` and `-bzero` are equal to within a
        // residual f32 cannot resolve at this magnitude (f32's spacing at 1e8
        // is 8), so casting each operand to f32 BEFORE adding throws the
        // residual away entirely and gives exactly 0.0. Doing the same
        // subtraction in f64 first keeps the residual, and only THEN casting
        // to f32 preserves it. This is the textbook case for needing the
        // wider type — exactly what BITPIX -64 arithmetic depends on.
        // Verified below rather than hand-computed, so the test cannot be
        // fooled by an arithmetic slip of its own.
        let raw: f64 = 100_000_000.0;
        let bscale: f64 = 1.0;
        let bzero: f64 = -raw + 1e-7;
        let correct = (raw * bscale + bzero) as f32;
        let wrong_early_cast = (raw as f32) * (bscale as f32) + (bzero as f32);
        assert_ne!(
            correct, wrong_early_cast,
            "test input does not discriminate early-cast from late-cast — pick different numbers"
        );
        assert_ne!(correct, 0.0, "the whole point is that the f64 path keeps a nonzero residual");

        // A second, unrelated sample so `decode_run` is checked over an
        // actual multi-sample run, not just a single-sample slice.
        let raw2: f64 = 42.0;
        let expected2 = (raw2 * bscale + bzero) as f32;

        let kind = PlaneKind::F64Be { bzero, bscale };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&raw.to_be_bytes());
        bytes.extend_from_slice(&raw2.to_be_bytes());
        assert_eq!(kind.decode(&bytes, 0), correct);
        assert_eq!(kind.decode(&bytes, 1), expected2);
        assert_decode_and_run_agree(kind, &bytes, 2);
    }

    #[test]
    fn f32le_applies_no_scaling() {
        // Decode-and-spill scratch path (XISF / RGB FITS / nonstandard
        // headers): the spilled data is already physical, little-endian, no
        // BZERO/BSCALE. Confirm the raw value comes back completely
        // unscaled — there is no bzero/bscale field on this variant to drop,
        // so the only way to get this wrong is to apply some anyway.
        // Two samples so `decode_run` is checked over an actual multi-sample
        // run, not just a single-sample slice.
        let kind = PlaneKind::F32Le;
        let v1: f32 = -42.5;
        let v2: f32 = 1234.75;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&v1.to_le_bytes());
        bytes.extend_from_slice(&v2.to_le_bytes());
        assert_eq!(kind.decode(&bytes, 0), v1, "F32Le must not apply any scale/offset");
        assert_eq!(kind.decode(&bytes, 1), v2, "F32Le must not apply any scale/offset");
        assert_decode_and_run_agree(kind, &bytes, 2);
    }
}
