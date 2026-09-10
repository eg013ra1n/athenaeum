//! `.rej` per-frame rejection-bitmap format (spec §6.2, M3 ruling R-M3-8):
//! `RejBitmapSet` (writer, one file per frame in a run's group directory),
//! `RejBitmap` (reader), and `RejPlaneSink` — the
//! `integration::source::RejectionBitSink` implementation the engine's band
//! loop writes to when drizzle wants the survivor mask.
//!
//! These files are per-run TEMPORARIES, not `stacking_artifacts` rows: the
//! run removes `rej/run-<id>` at its single exit path (success, failure or
//! cancel) unless the user asked to keep everything, and `stacking::paths`'
//! `cleanup_work` sweeps any that survive a crash. There is no checksum
//! trailer (contrast `stacking::ln::grid`'s `.athln` sidecars, which persist
//! across runs and are read back into a later run's cache) — a truncated or
//! foreign `.rej` file is simply refused on read, never silently trusted.
//!
//! On-disk layout (little-endian):
//! ```text
//! offset 0   magic     [u8; 8]   b"ATHREJ01"
//! offset 8   width     u32
//! offset 12  height    u32
//! offset 16  channels  u32
//! offset 20  words     u32       ceil(width / 64)
//! offset 24  body      channels × height × words  u64 words, row-major
//!                      per channel (channel 0's rows, then channel 1's, …)
//! ```
//! Body word `(plane * height + y) * words + word_x` holds bits
//! `[word_x*64, word_x*64+64)` of row `y`'s rejection mask for that plane —
//! bit `b` set means the source pixel at column `word_x*64 + b` was
//! rejected (present but not a survivor) for that plane.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{ensure, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use tracing::{debug, warn};

use crate::integration::source::RejectionBitSink;
use crate::integration::IntegrationError;

pub const REJ_MAGIC: &[u8; 8] = b"ATHREJ01";
/// magic (8) + width + height + channels + words (4 × u32 = 16) = 24 bytes.
const HEADER_LEN: u64 = 8 + 4 * 4;

#[inline]
fn words_per_row(width: usize) -> usize {
    width.div_ceil(64)
}

/// `channels × height × words`, `checked_mul`'d at every step (fix round 1,
/// M3) — unreachable for any real geometry (`channels ≤ 3`, `words ≲ 10^3`,
/// `height ≲ 10^5`), and always computed from CALLER-supplied geometry,
/// never from a file's own header, so there is no file-controlled
/// allocation to protect either way. Still checked, not assumed.
fn body_words(height: usize, channels: usize, words: usize) -> Result<usize> {
    channels
        .checked_mul(height)
        .and_then(|v| v.checked_mul(words))
        .with_context(|| {
            format!("rejection bitmap body size overflow: {channels} channels x {height} rows x {words} words/row")
        })
}

/// Body length in bytes (`body_words` × 8) — checked the same way.
fn body_len(height: usize, channels: usize, words: usize) -> Result<u64> {
    let n_words = body_words(height, channels, words)?;
    n_words
        .checked_mul(8)
        .map(|b| b as u64)
        .with_context(|| format!("rejection bitmap body byte size overflow: {n_words} words"))
}

/// Writes `bytes` at `offset`, looping until every byte has landed — a
/// single `write_at`/`seek_write` call is not guaranteed to write the whole
/// slice in one go.
#[cfg(unix)]
fn write_at(file: &File, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut written = 0usize;
    while written < bytes.len() {
        let n = file.write_at(&bytes[written..], offset + written as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "write_at wrote 0 bytes",
            ));
        }
        written += n;
    }
    Ok(())
}

#[cfg(windows)]
fn write_at(file: &File, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut written = 0usize;
    let mut off = offset;
    while written < bytes.len() {
        let n = file.seek_write(&bytes[written..], off)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "seek_write wrote 0 bytes",
            ));
        }
        written += n;
        off += n as u64;
    }
    Ok(())
}

/// One run's `.rej` bitmap files, one per frame, all in the same directory
/// (the caller — `stacking::run`, Task 5 — picks `dir` via
/// `stacking::paths::WorkingLayout::rej_dir`; this type only owns the
/// filenames it created inside it).
#[derive(Debug)]
pub struct RejBitmapSet {
    dir: PathBuf,
    paths: Vec<PathBuf>,
    width: usize,
    height: usize,
    channels: usize,
    words: usize,
    /// One file's exact on-disk size (header + body) — cached at
    /// construction so `bytes()` never has to re-run the checked
    /// multiplication `body_len` already proved safe for this geometry.
    file_len: u64,
    /// B3 (M3 final fix wave, I3): latched by [`RejPlaneSink::record_band`]
    /// on the FIRST write failure (open/write, e.g. `ENOSPC`/`EACCES`/an
    /// SMB hiccup) — a `.rej` write fault must not fail an otherwise-good
    /// integration. Once set, every later `record_band` call for this set
    /// becomes a no-op (`Ok(())`, nothing written) and the caller
    /// (`stacking::run::process_group_output`) reads [`RejBitmapSet::
    /// failure`] to skip drizzle for the group while keeping its master —
    /// the same outcome `create` failing already produces.
    failure: Mutex<Option<String>>,
}

/// Opens `path` for a fresh header + `set_len`'d body, refusing an existing
/// file. Fix round 1, M1: the "already exists" text is now attached ONLY to
/// `ErrorKind::AlreadyExists` — an `ENOSPC`/`EACCES`/permission failure
/// reaches the caller as itself, not misreported as a collision.
fn create_one_file(
    path: &Path,
    total_len: u64,
    width: u32,
    height: u32,
    channels: u32,
    words: u32,
) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow::anyhow!("{} already exists", path.display())
            } else {
                anyhow::Error::new(e).context(format!("create {}", path.display()))
            }
        })?;
    file.write_all(REJ_MAGIC)?;
    file.write_u32::<LittleEndian>(width)?;
    file.write_u32::<LittleEndian>(height)?;
    file.write_u32::<LittleEndian>(channels)?;
    file.write_u32::<LittleEndian>(words)?;
    file.set_len(total_len)?;
    Ok(())
}

impl RejBitmapSet {
    /// Creates `dir` (and any missing parents) and one zero-filled
    /// `<stem>.rej` per entry of `stems`, `set_len`'d to the exact final
    /// size with the header already written — `record_band` only ever does
    /// positional writes into an already-correctly-sized file. Refuses to
    /// overwrite an existing file (never overwrite; the run id already
    /// makes `dir` unique per run). Fix round 1, M4: a failure partway
    /// through `stems` rolls back every file this call itself created —
    /// harmless today (the run id makes `dir` unique per run, so a retry
    /// never collides with a PRIOR run's files), but a retry against the
    /// SAME call's own half-written output must not immediately trip the
    /// "already exists" refusal on stem 0.
    pub fn create(
        dir: &Path,
        stems: &[String],
        width: usize,
        height: usize,
        channels: usize,
    ) -> Result<RejBitmapSet> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let words = words_per_row(width);
        let total_len = HEADER_LEN + body_len(height, channels, words)?;
        let mut paths = Vec::with_capacity(stems.len());
        for stem in stems {
            let path = dir.join(format!("{stem}.rej"));
            match create_one_file(
                &path,
                total_len,
                width as u32,
                height as u32,
                channels as u32,
                words as u32,
            ) {
                Ok(()) => paths.push(path),
                Err(e) => {
                    for written in &paths {
                        if let Err(remove_err) = std::fs::remove_file(written) {
                            warn!(
                                path = %written.display(),
                                error = %remove_err,
                                "rejection bitmap set: cleanup after a partial create failed"
                            );
                        }
                    }
                    return Err(e);
                }
            }
        }
        debug!(
            path = %dir.display(),
            count = paths.len(),
            bytes = total_len.saturating_mul(paths.len() as u64),
            "rejection bitmap set created"
        );
        Ok(RejBitmapSet {
            dir: dir.to_path_buf(),
            paths,
            width,
            height,
            channels,
            words,
            file_len: total_len,
            failure: Mutex::new(None),
        })
    }

    /// The `RejectionBitSink` for one plane — every frame's file gets this
    /// plane's rows written into its own slot of the body.
    pub fn plane_sink(&self, plane: usize) -> RejPlaneSink<'_> {
        RejPlaneSink { set: self, plane }
    }

    pub fn path(&self, frame: usize) -> &Path {
        &self.paths[frame]
    }

    /// The frame count this set was created for — `integrate_group` checks
    /// this against the post-min-weight-drop included count before handing
    /// the set to `integrate_planes`.
    pub fn frames(&self) -> usize {
        self.paths.len()
    }

    /// The reference-geometry width this set was created for (Task 3's
    /// drizzle driver reads it back alongside `height`/`channels` when
    /// opening a frame's bitmap via [`RejBitmap::read`]).
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Total bytes on disk across every frame's file.
    pub fn bytes(&self) -> u64 {
        self.file_len.saturating_mul(self.paths.len() as u64)
    }

    /// B3 (M3 final fix wave, I3): the FIRST write failure any
    /// [`RejPlaneSink::record_band`] call for this set has latched, if any
    /// — `Some` means every band this set was asked to write past the
    /// first failure was silently skipped; the caller
    /// (`stacking::run::process_group_output`) reads this after
    /// integration to decide whether drizzle can trust this set's bitmaps.
    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Latches `error` (about `path`) as the set's failure reason, unless
    /// one is already latched (the FIRST failure wins) — logged once at
    /// `warn!` right here, so every caller of `record_band` gets the same
    /// single log line regardless of which frame's write actually failed.
    fn latch_failure(&self, path: &Path, error: &std::io::Error) {
        let mut guard = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            warn!(
                path = %path.display(),
                error = %error,
                "rejection bitmap write failed; drizzle will be skipped for this group"
            );
            *guard = Some(format!("{}: {error}", path.display()));
        }
    }
}

/// One plane's [`RejectionBitSink`] over a [`RejBitmapSet`]: `record_band`
/// opens each frame's file, writes the band's rows for that frame at the
/// plane's own offset, and closes it — frames are written sequentially, so
/// the set never holds more than one file handle open at a time (ruling
/// R-M3-8: the 208-open-files problem never arises).
pub struct RejPlaneSink<'a> {
    set: &'a RejBitmapSet,
    plane: usize,
}

impl RejectionBitSink for RejPlaneSink<'_> {
    fn words_per_row(&self) -> usize {
        self.set.words
    }

    fn frames(&self) -> usize {
        self.set.paths.len()
    }

    fn record_band(&self, y0: usize, rows: usize, bits: &[u64]) -> Result<(), IntegrationError> {
        // Fix round 1, M2: a caller mistake here (a wrong plane index, or a
        // band that runs past the set's own height) used to become a
        // silent EOF-extending write at the wrong offset, discovered only
        // much later by `RejBitmap::read`'s length/geometry check — after
        // the group's whole integration had already been paid for. Both
        // are refused loudly, at the exact call that would have been wrong.
        if self.plane >= self.set.channels {
            return Err(IntegrationError::BadInput(format!(
                "rejection band: plane {} out of range for {} channels",
                self.plane, self.set.channels
            )));
        }
        let band_end = y0.checked_add(rows);
        if band_end.is_none_or(|end| end > self.set.height) {
            return Err(IntegrationError::BadInput(format!(
                "rejection band: rows {y0}..{} out of range for height {}",
                y0 + rows,
                self.set.height
            )));
        }
        let n = self.set.paths.len();
        let words = self.set.words;
        let expected = rows * n * words;
        if bits.len() != expected {
            return Err(IntegrationError::BadInput(format!(
                "rejection band: {} bits, expected rows({rows}) x frames({n}) x words({words}) = {expected}",
                bits.len()
            )));
        }

        // B3 (M3 final fix wave, I3): once a WRITE failure has latched
        // (below), every later `record_band` call for this set — any
        // plane, any band — becomes a no-op. A mid-run I/O fault
        // (`ENOSPC`/`EACCES`/an SMB hiccup) is systemic, not per-band:
        // retrying wastes time and would only repeat the same warning.
        // This check is AFTER the geometry/shape validation above — those
        // are caller bugs (structural, not I/O), and stay loud on every
        // call regardless of a prior write failure.
        if self
            .set
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Ok(());
        }

        // Row offset into THIS plane's slice of the body, in words; the
        // body is channel-major (see the module doc's layout table).
        let row_words_offset = (self.plane * self.set.height + y0) as u64 * words as u64;
        let byte_offset = HEADER_LEN + row_words_offset * 8;
        for (frame_idx, path) in self.set.paths.iter().enumerate() {
            // Fix round 1, M7: `create` zero-filled every byte this band
            // could ever touch, and the engine's sequential band loop
            // writes each `(plane, band)` region at most once per run — so
            // a frame with NOTHING rejected in this band needs no write at
            // all. The common case by a wide margin (a handful of
            // rejections across a couple hundred frames), this removes
            // nearly every open/write/close for a typical run.
            let frame_has_bits = (0..rows).any(|row| {
                let base = (row * n + frame_idx) * words;
                bits[base..base + words].iter().any(|&w| w != 0)
            });
            if !frame_has_bits {
                continue;
            }
            // B3: an open/write failure here latches the set's failure
            // reason (the FIRST one wins) and returns `Ok(())` — a `.rej`
            // write fault must not fail an otherwise-good integration
            // (ruling: never fail a run because drizzle-support machinery
            // failed). `RejBitmap::read`'s own length/geometry check
            // refuses a truncated file outright, so nothing downstream can
            // mistake a partially-written set for a complete one; the
            // caller (`stacking::run::process_group_output`) reads
            // `RejBitmapSet::failure` after integration and skips drizzle
            // for the group entirely rather than trusting it.
            let file = match OpenOptions::new().write(true).open(path) {
                Ok(f) => f,
                Err(e) => {
                    self.set.latch_failure(path, &e);
                    return Ok(());
                }
            };
            let mut frame_buf = Vec::with_capacity(rows * words * 8);
            for row in 0..rows {
                for w in 0..words {
                    let word = bits[(row * n + frame_idx) * words + w];
                    frame_buf.extend_from_slice(&word.to_le_bytes());
                }
            }
            if let Err(e) = write_at(&file, byte_offset, &frame_buf) {
                self.set.latch_failure(path, &e);
                return Ok(());
            }
        }
        Ok(())
    }
}

/// One frame's bitmap, read whole (validated: file length, magic, geometry
/// == expected — length checked BEFORE any allocation proportional to the
/// file's declared size, so a short or crafted file can never trigger a
/// huge allocation).
#[derive(Debug)]
pub struct RejBitmap {
    width: usize,
    height: usize,
    channels: usize,
    words: usize,
    bits: Vec<u64>,
}

impl RejBitmap {
    pub fn read(path: &Path, width: usize, height: usize, channels: usize) -> Result<RejBitmap> {
        let words = words_per_row(width);
        let expected_len = HEADER_LEN + body_len(height, channels, words)?;
        let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        ensure!(
            meta.len() == expected_len,
            "{}: file length {} != expected length {expected_len} for {width}x{height}x{channels}",
            path.display(),
            meta.len()
        );

        // Fix round 1, I1: a bare `File` costs one `read(2)` syscall per
        // `read_u64` call — 409k of them for a realistic 6248×4176 mono
        // bitmap, measured at ≈130ms against ≈1ms buffered. `BufReader`
        // turns that into a handful of syscalls (its default 8KB capacity)
        // for the header AND the whole body, with no change to the
        // per-word reads below.
        let mut file =
            BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)
            .with_context(|| format!("{}: reading magic", path.display()))?;
        ensure!(&magic == REJ_MAGIC, "{}: bad magic", path.display());
        let file_width = file.read_u32::<LittleEndian>()? as usize;
        let file_height = file.read_u32::<LittleEndian>()? as usize;
        let file_channels = file.read_u32::<LittleEndian>()? as usize;
        let file_words = file.read_u32::<LittleEndian>()? as usize;
        ensure!(
            file_width == width && file_height == height && file_channels == channels && file_words == words,
            "{}: geometry {file_width}x{file_height}x{file_channels} (words {file_words}) != expected {width}x{height}x{channels} (words {words})",
            path.display()
        );

        // Fix round 1, M3: checked BEFORE the allocation just below — see
        // `body_words`'s own doc for why this is unreachable in practice
        // but checked anyway.
        let n_words = body_words(height, channels, words)?;
        let mut bits = vec![0u64; n_words];
        for w in bits.iter_mut() {
            *w = file
                .read_u64::<LittleEndian>()
                .with_context(|| format!("{}: reading body", path.display()))?;
        }
        Ok(RejBitmap {
            width,
            height,
            channels,
            words,
            bits,
        })
    }

    /// True when the source pixel `(x, y)` of plane `plane` was rejected
    /// (present but not a survivor). Out-of-range `plane`/`x`/`y` panics —
    /// callers (Task 3's drizzle driver) always index within the geometry
    /// this bitmap was read against.
    #[inline]
    pub fn is_rejected(&self, plane: usize, x: usize, y: usize) -> bool {
        debug_assert!(plane < self.channels && x < self.width && y < self.height);
        let word_idx = (plane * self.height + y) * self.words + x / 64;
        (self.bits[word_idx] >> (x % 64)) & 1 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_writes_correctly_sized_zero_filled_files_with_headers_that_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let stems = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (width, height, channels) = (100usize, 70usize, 2usize);
        let set = RejBitmapSet::create(dir.path(), &stems, width, height, channels).unwrap();

        let words = words_per_row(width);
        assert_eq!(words, 2, "ceil(100/64) == 2");
        let expected_len = HEADER_LEN + body_len(height, channels, words).unwrap();
        assert_eq!(expected_len, 24 + 2 * 70 * 2 * 8);

        for i in 0..3 {
            let meta = std::fs::metadata(set.path(i)).unwrap();
            assert_eq!(meta.len(), expected_len);
        }
        assert_eq!(set.bytes(), expected_len * 3);

        for i in 0..3 {
            let bm = RejBitmap::read(set.path(i), width, height, channels).unwrap();
            for p in 0..channels {
                for y in [0usize, 35, height - 1] {
                    for x in [0usize, 63, 64, width - 1] {
                        assert!(
                            !bm.is_rejected(p, x, y),
                            "freshly created bitmap must be all-zero at ({p},{x},{y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn record_band_writes_bits_at_the_right_offset_and_read_back_matches() {
        let dir = tempfile::tempdir().unwrap();
        let stems = vec!["f0".to_string(), "f1".to_string(), "f2".to_string()];
        let (width, height, channels) = (100usize, 70usize, 2usize);
        let set = RejBitmapSet::create(dir.path(), &stems, width, height, channels).unwrap();

        let words = words_per_row(width);
        let n = stems.len();
        let rows = 5usize;
        let mut bits = vec![0u64; rows * n * words];
        let row_in_band = 3usize;
        let frame = 2usize;
        let x = 65usize;
        bits[(row_in_band * n + frame) * words + (x / 64)] |= 1u64 << (x % 64);

        let sink = set.plane_sink(1);
        sink.record_band(10, rows, &bits).unwrap();

        let bm = RejBitmap::read(set.path(2), width, height, channels).unwrap();
        assert!(bm.is_rejected(1, 65, 13), "the set bit must round-trip");
        assert!(
            !bm.is_rejected(1, 64, 13),
            "the neighboring bit must stay clear"
        );
        assert!(
            !bm.is_rejected(0, 65, 13),
            "the other plane must stay clear"
        );

        // Every other frame's file must still be all-zero.
        for other in [0usize, 1] {
            let bm_other = RejBitmap::read(set.path(other), width, height, channels).unwrap();
            assert!(!bm_other.is_rejected(1, 65, 13), "frame {other} untouched");
        }
    }

    #[test]
    fn read_refuses_a_short_file() {
        let dir = tempfile::tempdir().unwrap();
        let set = RejBitmapSet::create(dir.path(), &["only".to_string()], 32, 16, 1).unwrap();
        let path = set.path(0).to_path_buf();
        let len = std::fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len - 8).unwrap();

        let err = RejBitmap::read(&path, 32, 16, 1).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("length"), "{err}");
    }

    #[test]
    fn read_refuses_a_geometry_mismatch_at_the_same_file_length() {
        let dir = tempfile::tempdir().unwrap();
        // width 32 and width 64 both round to 1 word/row, so a file created
        // for 32x16x1 has the SAME total length a 64x16x1 caller expects —
        // the length check alone cannot catch this; the geometry check must.
        let set = RejBitmapSet::create(dir.path(), &["only".to_string()], 32, 16, 1).unwrap();
        assert_eq!(words_per_row(32), words_per_row(64));

        let err = RejBitmap::read(set.path(0), 64, 16, 1).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("geometry"), "{err}");
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let stems = vec!["dup".to_string()];
        let _set = RejBitmapSet::create(dir.path(), &stems, 10, 10, 1).unwrap();
        let path = dir.path().join("dup.rej");
        let before = std::fs::read(&path).unwrap();

        let err = RejBitmapSet::create(dir.path(), &stems, 10, 10, 1).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("exists"), "{err}");

        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "the existing file must be left untouched");
    }

    #[test]
    fn record_band_refuses_a_mismatched_bit_buffer_length() {
        let dir = tempfile::tempdir().unwrap();
        let set = RejBitmapSet::create(dir.path(), &["a".to_string(), "b".to_string()], 32, 16, 1)
            .unwrap();
        let sink = set.plane_sink(0);
        let bad_bits = vec![0u64; 3];
        let err = sink.record_band(0, 5, &bad_bits).unwrap_err();
        assert!(matches!(err, IntegrationError::BadInput(_)), "{err:?}");
    }

    /// Fix round 1, I2 (this half is engine-side; the corresponding
    /// `integrate_group`-level check is Task 2 fix round 1's `integrate.rs`
    /// test) — `record_band`'s OWN guard: an out-of-range plane index, or a
    /// band that would run past the set's declared height, is refused
    /// before any positional write happens (M2).
    #[test]
    fn record_band_refuses_an_out_of_range_plane_or_band() {
        let dir = tempfile::tempdir().unwrap();
        let set = RejBitmapSet::create(dir.path(), &["a".to_string()], 32, 16, 2).unwrap();
        let words = words_per_row(32);
        let bits = vec![0u64; 5 * 1 * words];

        let err = set.plane_sink(2).record_band(0, 5, &bits).unwrap_err();
        assert!(matches!(err, IntegrationError::BadInput(_)), "{err:?}");

        // 12 + 5 = 17 > height 16.
        let err = set.plane_sink(0).record_band(12, 5, &bits).unwrap_err();
        assert!(matches!(err, IntegrationError::BadInput(_)), "{err:?}");
    }

    /// Fix round 1, I1: a stronger correctness pin for the buffered read —
    /// several bits scattered across multiple ROWS and multiple WORDS per
    /// row (`words_per_row(200) == 4`), read back one at a time via
    /// `is_rejected` at every pixel of a real (non-trivial) geometry. The
    /// existing round-trip tests already exercise `BufReader` incidentally;
    /// this one is built specifically to catch a chunk-boundary or
    /// row/word-indexing regression in that code path.
    #[test]
    fn read_reassembles_a_multi_word_multi_row_body_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let (width, height, channels) = (200usize, 40usize, 1usize);
        let set =
            RejBitmapSet::create(dir.path(), &["a".to_string()], width, height, channels).unwrap();
        let words = words_per_row(width);
        assert_eq!(words, 4, "ceil(200/64) == 4");

        let rows = height;
        let mut bits = vec![0u64; rows * 1 * words];
        let set_positions: [(usize, usize); 6] =
            [(0, 0), (10, 63), (10, 64), (20, 127), (39, 199), (5, 128)];
        for &(y, x) in &set_positions {
            bits[(y * 1 + 0) * words + (x / 64)] |= 1u64 << (x % 64);
        }
        set.plane_sink(0).record_band(0, rows, &bits).unwrap();

        let bm = RejBitmap::read(set.path(0), width, height, channels).unwrap();
        for y in 0..height {
            for x in 0..width {
                let expected = set_positions.contains(&(y, x));
                assert_eq!(
                    bm.is_rejected(0, x, y),
                    expected,
                    "mismatch at (x={x}, y={y})"
                );
            }
        }
    }

    /// Fix round 1, M4: a failure partway through `create` rolls back every
    /// file THIS call itself wrote — a retry against the same directory
    /// must not trip a leftover "already exists" on a stem that call
    /// already cleaned up.
    #[test]
    fn create_rolls_back_files_it_wrote_when_a_later_stem_fails() {
        let dir = tempfile::tempdir().unwrap();
        let stems = vec!["a".to_string(), "b".to_string()];
        // Pre-existing "b.rej" makes create() fail on the SECOND stem,
        // after "a.rej" has already been written by this same call.
        std::fs::write(dir.path().join("b.rej"), b"pre-existing").unwrap();

        let err = RejBitmapSet::create(dir.path(), &stems, 8, 8, 1).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("exists"), "{err}");
        assert!(
            !dir.path().join("a.rej").exists(),
            "the file this call itself wrote must be rolled back on failure"
        );
        assert_eq!(
            std::fs::read(dir.path().join("b.rej")).unwrap(),
            b"pre-existing",
            "a file this call did NOT write must never be touched"
        );

        // A retry (after clearing the real collision) must succeed — stem
        // "a" no longer trips a leftover "already exists" from the failed
        // attempt above.
        std::fs::remove_file(dir.path().join("b.rej")).unwrap();
        let set = RejBitmapSet::create(dir.path(), &stems, 8, 8, 1).unwrap();
        assert_eq!(set.frames(), 2);
    }

    /// Fix round 1, M7: a frame with NO bits set anywhere in the band must
    /// never be opened for writing — proven, not just asserted, by making
    /// that frame's file read-only first: `record_band` succeeding despite
    /// that means it never tried to open it.
    #[cfg(unix)]
    #[test]
    fn record_band_skips_opening_a_frames_file_when_the_band_has_no_bits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let set = RejBitmapSet::create(
            dir.path(),
            &["untouched".to_string(), "hot".to_string()],
            16,
            8,
            1,
        )
        .unwrap();
        let words = words_per_row(16);
        let rows = 4usize;
        let n = 2usize;
        let mut bits = vec![0u64; rows * n * words];
        // Only frame 1 ("hot") gets a bit; frame 0 ("untouched") stays all-zero.
        bits[(0 * n + 1) * words] |= 1;

        let path0 = set.path(0).to_path_buf();
        let mut perms = std::fs::metadata(&path0).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path0, perms).unwrap();

        let result = set.plane_sink(0).record_band(0, rows, &bits);

        // Restore writability unconditionally so tempdir cleanup can't fail.
        let mut perms = std::fs::metadata(&path0).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&path0, perms).unwrap();

        result.expect("a frame with no bits in the band must never be opened for writing");
    }

    /// B3 (M3 final fix wave, I3): a `.rej` write failure (here: the file
    /// this call would need to open was removed out from under it, an
    /// ENOENT stand-in for ENOSPC/EACCES/an SMB hiccup) must not fail
    /// `record_band` — it latches [`RejBitmapSet::failure`], returns
    /// `Ok(())`, and every LATER call for the same set (any plane, any
    /// band) becomes a no-op — proven here by writing bits for a DIFFERENT,
    /// still-present frame on a second call and checking its file stayed
    /// untouched.
    #[cfg(unix)]
    #[test]
    fn record_band_latches_a_write_failure_and_skips_later_bands() {
        let dir = tempfile::tempdir().unwrap();
        let set = RejBitmapSet::create(
            dir.path(),
            &["a".to_string(), "b".to_string()],
            16,
            8,
            1,
        )
        .unwrap();
        let words = words_per_row(16);
        let n = 2usize;
        let rows = 4usize;

        assert!(set.failure().is_none(), "no failure latched yet");

        // Remove frame "a"'s file so opening it for write fails — and set a
        // bit for it so `record_band` actually attempts that write.
        std::fs::remove_file(set.path(0)).unwrap();
        let mut bits = vec![0u64; rows * n * words];
        bits[(0 * n) * words] |= 1; // frame 0 ("a") has a bit, row 0

        let sink = set.plane_sink(0);
        let result = sink.record_band(0, rows, &bits);
        assert!(
            result.is_ok(),
            "a write failure must not fail record_band: {result:?}"
        );
        let failure = set.failure();
        assert!(failure.is_some(), "the failure must be latched");
        assert!(
            failure.unwrap().contains(&set.path(0).display().to_string()),
            "the latched reason should name the file that failed"
        );

        // A later call, for a DIFFERENT band, with bits for frame 1 ("b")
        // whose file IS present and writable, must still be skipped
        // entirely — proven by frame "b"'s file staying all-zero.
        let mut bits2 = vec![0u64; rows * n * words];
        bits2[(0 * n + 1) * words] |= 1; // frame 1 ("b") has a bit, row 0
        let result2 = sink.record_band(4, rows, &bits2);
        assert!(result2.is_ok(), "{result2:?}");

        let bm = RejBitmap::read(set.path(1), 16, 8, 1).unwrap();
        assert!(
            !bm.is_rejected(0, 0, 4),
            "a call after the latch must write nothing, even for an untouched file"
        );
    }
}
