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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::integration::source::RejectionBitSink;
use crate::integration::IntegrationError;

pub const REJ_MAGIC: &[u8; 8] = b"ATHREJ01";
/// magic (8) + width + height + channels + words (4 × u32 = 16) = 24 bytes.
const HEADER_LEN: u64 = 8 + 4 * 4;

#[inline]
fn words_per_row(width: usize) -> usize {
    width.div_ceil(64)
}

fn body_len(height: usize, channels: usize, words: usize) -> u64 {
    (channels as u64) * (height as u64) * (words as u64) * 8
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
}

impl RejBitmapSet {
    /// Creates `dir` (and any missing parents) and one zero-filled
    /// `<stem>.rej` per entry of `stems`, `set_len`'d to the exact final
    /// size with the header already written — `record_band` only ever does
    /// positional writes into an already-correctly-sized file. Refuses to
    /// overwrite an existing file (never overwrite; the run id already
    /// makes `dir` unique per run).
    pub fn create(
        dir: &Path,
        stems: &[String],
        width: usize,
        height: usize,
        channels: usize,
    ) -> Result<RejBitmapSet> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let words = words_per_row(width);
        let total_len = HEADER_LEN + body_len(height, channels, words);
        let mut paths = Vec::with_capacity(stems.len());
        for stem in stems {
            let path = dir.join(format!("{stem}.rej"));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("{} already exists", path.display()))?;
            file.write_all(REJ_MAGIC)?;
            file.write_u32::<LittleEndian>(width as u32)?;
            file.write_u32::<LittleEndian>(height as u32)?;
            file.write_u32::<LittleEndian>(channels as u32)?;
            file.write_u32::<LittleEndian>(words as u32)?;
            file.set_len(total_len)?;
            paths.push(path);
        }
        Ok(RejBitmapSet {
            dir: dir.to_path_buf(),
            paths,
            width,
            height,
            channels,
            words,
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
        let per_file = HEADER_LEN + body_len(self.height, self.channels, self.words);
        per_file * self.paths.len() as u64
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
        let n = self.set.paths.len();
        let words = self.set.words;
        let expected = rows * n * words;
        if bits.len() != expected {
            return Err(IntegrationError::BadInput(format!(
                "rejection band: {} bits, expected rows({rows}) x frames({n}) x words({words}) = {expected}",
                bits.len()
            )));
        }
        // Row offset into THIS plane's slice of the body, in words; the
        // body is channel-major (see the module doc's layout table).
        let row_words_offset = (self.plane * self.set.height + y0) as u64 * words as u64;
        let byte_offset = HEADER_LEN + row_words_offset * 8;
        for (frame_idx, path) in self.set.paths.iter().enumerate() {
            let file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(IntegrationError::Io)?;
            let mut frame_buf = Vec::with_capacity(rows * words * 8);
            for row in 0..rows {
                for w in 0..words {
                    let word = bits[(row * n + frame_idx) * words + w];
                    frame_buf.extend_from_slice(&word.to_le_bytes());
                }
            }
            write_at(&file, byte_offset, &frame_buf).map_err(IntegrationError::Io)?;
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
        let expected_len = HEADER_LEN + body_len(height, channels, words);
        let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        ensure!(
            meta.len() == expected_len,
            "{}: file length {} != expected length {expected_len} for {width}x{height}x{channels}",
            path.display(),
            meta.len()
        );

        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
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

        let n_words = channels * height * words;
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
        let expected_len = HEADER_LEN + body_len(height, channels, words);
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
}
