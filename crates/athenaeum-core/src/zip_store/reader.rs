//! Reading a zip: its directory, the first bytes of its entries, and verified extraction of one entry.
//!
//! Read sizes are measured (docs/superpowers/research/2026-10-27-zip-probe-measurement.md): the
//! header pass reads through a 4 KiB buffer (0.06% of a zip off a NAS), extraction through 1 MiB
//! (a 4 KiB buffer streams a whole entry at a few MiB/s over a share). `bytes_read` is counted below
//! the buffer, so it is what the file system was asked for, not what the caller consumed.

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use xxhash_rust::xxh3::Xxh3;

/// Buffer under the directory and header pass.
pub const HEADER_BUF_BYTES: usize = 4 * 1024;
/// Buffer under a whole-entry extraction.
pub const EXTRACT_BUF_BYTES: usize = 1024 * 1024;

const FITS_BLOCK: usize = 2880;
const FITS_CARD: usize = 80;
/// A FITS header with no END card in this many blocks is not a header.
const MAX_FITS_HEADER_BLOCKS: usize = 200;
/// An XISF header declaring more than this is refused rather than allocated.
const MAX_XISF_HEADER_BYTES: usize = 16 * 1024 * 1024;
const FRAME_EXTENSIONS: [&str; 4] = ["fits", "fit", "fts", "xisf"];

/// One non-skipped entry of a zip's directory.
#[derive(Debug, Clone)]
pub struct ZipEntryInfo {
    pub index: usize,
    /// The entry's name as stored (forward slashes).
    pub path: String,
    pub size: u64,
    pub compressed_size: u64,
    pub crc32: u32,
    /// `true` when the entry is stored, `false` when it is compressed.
    pub stored: bool,
    pub modified: Option<NaiveDateTime>,
    /// A FITS/XISF file name (case-insensitive extension).
    pub is_frame: bool,
}

#[derive(Debug, Clone)]
pub struct ZipListing {
    pub entries: Vec<ZipEntryInfo>,
    /// Entries that are not listed, with why: `"unsafe name"`, `"appledouble"`, `"nested zip"`,
    /// `"encrypted"`, `"unreadable"`. Directories are not entries and are not reported.
    pub skipped: Vec<(String, &'static str)>,
    /// Bytes pulled from the zip file for this listing.
    pub bytes_read: u64,
}

/// What a finished extraction wrote.
#[derive(Debug, Clone)]
pub struct Extracted {
    pub bytes: u64,
    /// xxh3-64 over the full bytes, 16 lowercase hex digits: the `package::xxh3_full_file` format.
    pub strong_hash: String,
}

/// Counts the bytes that reach the underlying file.
struct Counting<R> {
    inner: R,
    bytes: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

type Archive = zip::ZipArchive<BufReader<Counting<File>>>;

fn open_archive(zip: &Path, buf_bytes: usize) -> Result<(Archive, Arc<AtomicU64>)> {
    let file = File::open(zip).with_context(|| format!("open zip {}", zip.display()))?;
    let bytes = Arc::new(AtomicU64::new(0));
    let reader = BufReader::with_capacity(buf_bytes, Counting { inner: file, bytes: bytes.clone() });
    let archive = zip::ZipArchive::new(reader).with_context(|| format!("read the directory of {}", zip.display()))?;
    Ok((archive, bytes))
}

/// `true` when `name` can be joined under a directory without leaving it: not empty, no NUL, not
/// absolute (`/x`, `\x`, `C:x`), and no `..` component (split on both `/` and `\`).
pub fn is_safe_entry_name(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.starts_with('/') || name.starts_with('\\') {
        return false;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return false;
    }
    !name.split(['/', '\\']).any(|c| c == "..")
}

fn last_component(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

fn has_extension(name: &str, wanted: &[&str]) -> bool {
    let file = last_component(name);
    match file.rsplit_once('.') {
        Some((_, ext)) => wanted.iter().any(|w| ext.eq_ignore_ascii_case(w)),
        None => false,
    }
}

fn is_frame_name(name: &str) -> bool {
    has_extension(name, &FRAME_EXTENSIONS)
}

/// Why an entry is not listed, judged by its name alone (`None`: list it).
fn skip_reason_for_name(name: &str) -> Option<&'static str> {
    if !is_safe_entry_name(name) {
        return Some("unsafe name");
    }
    if name.split(['/', '\\']).any(|c| c == "__MACOSX") || last_component(name).starts_with("._") {
        return Some("appledouble");
    }
    if has_extension(name, &["zip"]) {
        return Some("nested zip");
    }
    None
}

fn to_naive(dt: zip::DateTime) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(i32::from(dt.year()), u32::from(dt.month()), u32::from(dt.day()))?.and_hms_opt(
        u32::from(dt.hour()),
        u32::from(dt.minute()),
        u32::from(dt.second()),
    )
}

/// Read the zip's directory through a 4 KiB buffer. Reads the central directory plus each listed
/// entry's local header (to place its data); no entry data.
pub fn list_entries(zip: &Path) -> Result<ZipListing> {
    let (mut archive, counter) = open_archive(zip, HEADER_BUF_BYTES)?;
    let mut entries = Vec::new();
    let mut skipped: Vec<(String, &'static str)> = Vec::new();
    for index in 0..archive.len() {
        let name = match archive.name_for_index(index) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // A directory entry is not a file.
        if name.ends_with('/') || name.ends_with('\\') {
            continue;
        }
        if let Some(reason) = skip_reason_for_name(&name) {
            tracing::debug!(zip_path = %zip.display(), zip_entry = %name, "zip entry skipped");
            skipped.push((name, reason));
            continue;
        }
        // Raw access: reads the local header but needs neither a password nor a decoder.
        let info = match archive.by_index_raw(index) {
            Ok(f) => {
                if f.encrypted() {
                    tracing::debug!(zip_path = %zip.display(), zip_entry = %name, "zip entry skipped");
                    skipped.push((name, "encrypted"));
                    continue;
                }
                ZipEntryInfo {
                    index,
                    is_frame: is_frame_name(&name),
                    path: name.clone(),
                    size: f.size(),
                    compressed_size: f.compressed_size(),
                    crc32: f.crc32(),
                    stored: f.compression() == zip::CompressionMethod::Stored,
                    modified: f.last_modified().and_then(to_naive),
                }
            }
            Err(e) => {
                tracing::warn!(zip_path = %zip.display(), zip_entry = %name, error = %e, "zip entry unreadable");
                skipped.push((name, "unreadable"));
                continue;
            }
        };
        entries.push(info);
    }
    Ok(ZipListing { entries, skipped, bytes_read: counter.load(Ordering::Relaxed) })
}

/// Read decompressed bytes from the start of `entry` until the header is complete: FITS up to and
/// including the block holding the END card, XISF the 16-byte signature block plus the declared
/// header length. `name` picks the format by extension.
fn read_header_bytes(entry: &mut impl Read, name: &str) -> Result<Vec<u8>> {
    if has_extension(name, &["xisf"]) {
        let mut sig = [0u8; 16];
        entry.read_exact(&mut sig).with_context(|| format!("{name}: XISF signature unreadable"))?;
        if &sig[..8] != b"XISF0100" {
            bail!("{name}: not an XISF signature");
        }
        let len = u32::from_le_bytes([sig[8], sig[9], sig[10], sig[11]]) as usize;
        if len > MAX_XISF_HEADER_BYTES {
            bail!("{name}: XISF header declares {len} bytes");
        }
        let mut out = Vec::with_capacity(16 + len);
        out.extend_from_slice(&sig);
        out.resize(16 + len, 0);
        entry.read_exact(&mut out[16..]).with_context(|| format!("{name}: XISF header truncated"))?;
        return Ok(out);
    }
    let mut out = Vec::with_capacity(FITS_BLOCK * 4);
    let mut block = [0u8; FITS_BLOCK];
    for _ in 0..MAX_FITS_HEADER_BLOCKS {
        entry.read_exact(&mut block).with_context(|| format!("{name}: header ended without END"))?;
        out.extend_from_slice(&block);
        if block.chunks(FITS_CARD).any(|card| card.starts_with(b"END") && card[3..].iter().all(|&b| b == b' ')) {
            return Ok(out);
        }
    }
    bail!("{name}: no END card in {MAX_FITS_HEADER_BLOCKS} blocks")
}

/// Header bytes of several entries through one open of the zip and a 4 KiB buffer. A bad entry is an
/// `Err` in its own slot; the zip failing to open is the outer `Err`. Returns the bytes read.
pub fn read_headers(zip: &Path, entries: &[&str]) -> Result<(Vec<(String, Result<Vec<u8>>)>, u64)> {
    let (mut archive, counter) = open_archive(zip, HEADER_BUF_BYTES)?;
    let mut out = Vec::with_capacity(entries.len());
    for &name in entries {
        let header = archive
            .by_name(name)
            .with_context(|| format!("open entry {name} of {}", zip.display()))
            .and_then(|mut f| read_header_bytes(&mut f, name));
        out.push((name.to_string(), header));
    }
    Ok((out, counter.load(Ordering::Relaxed)))
}

/// Header bytes of one entry, and the bytes read from the zip to get them.
pub fn read_entry_header(zip: &Path, entry: &str) -> Result<(Vec<u8>, u64)> {
    let (mut headers, bytes_read) = read_headers(zip, &[entry])?;
    let (_, header) = headers.pop().context("no header result")?;
    Ok((header?, bytes_read))
}

/// The file an extraction writes into before it renames onto `dest`: `<dest file name>.partial`.
/// (Appended to the whole name, not swapped for the extension, so `L.fit` and `L.fits` never share one.)
pub fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    dest.with_file_name(name)
}

/// Removes the partial file unless the extraction finished.
struct PartialGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for PartialGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Err(e) = std::fs::remove_file(&self.path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %self.path.display(), error = %e, "could not remove partial extraction");
                }
            }
        }
    }
}

/// Extract `entry` to `dest` through a 1 MiB buffer.
///
/// The bytes go to `<dest>.partial` and the file is renamed onto `dest` only after the zip reader's
/// CRC32 check passed (and, with `expected_crc`, the catalog's CRC32 matched the zip's). On any error
/// or on cancel the partial file is deleted and `dest` is not touched. `on_bytes` is called with the
/// size of each chunk written (a delta, not a running total). `cancel` is checked before each chunk.
pub fn extract_entry(
    zip: &Path,
    entry: &str,
    expected_crc: Option<u32>,
    dest: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Extracted> {
    let (mut archive, _) = open_archive(zip, EXTRACT_BUF_BYTES)?;
    let mut file = archive
        .by_name(entry)
        .with_context(|| format!("open entry {entry} of {}", zip.display()))?;
    if let Some(expected) = expected_crc {
        if file.crc32() != expected {
            bail!(
                "entry changed since it was cataloged: {entry} in {} has crc32 {:08x}, the catalog has {expected:08x}",
                zip.display(),
                file.crc32()
            );
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let partial = partial_path(dest);
    let mut guard = PartialGuard { path: partial.clone(), armed: true };
    let mut out = File::create(&partial).with_context(|| format!("create {}", partial.display()))?;

    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; EXTRACT_BUF_BYTES];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("extraction cancelled: {entry} in {}", zip.display());
        }
        // The zip reader checks the CRC32 at the end of the entry and errors on a mismatch.
        let n = file.read(&mut buf).with_context(|| format!("extract {entry} from {}", zip.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).with_context(|| format!("write {}", partial.display()))?;
        total += n as u64;
        on_bytes(n as u64);
    }
    if total != file.size() {
        bail!("{entry} in {}: extracted {total} bytes, the directory says {}", zip.display(), file.size());
    }
    out.flush().with_context(|| format!("flush {}", partial.display()))?;
    drop(out);
    std::fs::rename(&partial, dest).with_context(|| format!("rename {} to {}", partial.display(), dest.display()))?;
    guard.armed = false;
    Ok(Extracted { bytes: total, strong_hash: format!("{:016x}", hasher.digest()) })
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::archive::restore::tests::write_minimal_fits;
    use crate::archive::zip_writer::{build_zip, ZipEntry};
    use tempfile::TempDir;

    /// Zip made of `(name in zip, bytes)` pairs.
    fn make_zip(dir: &Path, files: &[(&str, Vec<u8>)], compression: ArchiveCompression) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let entries: Vec<ZipEntry> = files
            .iter()
            .enumerate()
            .map(|(i, (name, bytes))| {
                let p = src.join(format!("src_{i}"));
                std::fs::write(&p, bytes).unwrap();
                ZipEntry { source_path: p, path_in_zip: (*name).to_string() }
            })
            .collect();
        let zip = dir.join("t.zip");
        build_zip(&zip, &entries, compression).unwrap();
        zip
    }

    fn minimal_fits_bytes(dir: &Path) -> Vec<u8> {
        let p = dir.join("min.fits");
        write_minimal_fits(&p);
        std::fs::read(p).unwrap()
    }

    /// Deterministic noise: deflate cannot shrink it, so a compressed entry is about as large as a stored one.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn card(s: &str) -> Vec<u8> {
        let mut c = vec![b' '; 80];
        c[..s.len()].copy_from_slice(s.as_bytes());
        c
    }

    /// A FITS file whose header spans `header_blocks` blocks (END in the last one), then `data_len` bytes of noise.
    fn fits_with_header_blocks(header_blocks: usize, data_len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(card("SIMPLE  =                    T"));
        out.extend(card("BITPIX  =                    8"));
        out.extend(card("NAXIS   =                    1"));
        out.extend(card(&format!("NAXIS1  = {data_len:>20}")));
        while out.len() < (header_blocks - 1) * FITS_BLOCK + 80 {
            out.extend(card("COMMENT filler so the header spans several blocks"));
        }
        out.extend(card("END"));
        while out.len() % FITS_BLOCK != 0 {
            out.push(b' ');
        }
        assert_eq!(out.len(), header_blocks * FITS_BLOCK);
        out.extend(noise(data_len));
        out
    }

    fn xisf_with_header_len(header_len: usize, data_len: usize) -> Vec<u8> {
        let mut out = b"XISF0100".to_vec();
        out.extend((header_len as u32).to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((0..header_len).map(|i| b'a' + (i % 26) as u8));
        out.extend(noise(data_len));
        out
    }

    #[test]
    fn lists_frames_and_skips_non_frames() {
        for compression in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            let tmp = TempDir::new().unwrap();
            let fits = minimal_fits_bytes(tmp.path());
            let zip = make_zip(
                tmp.path(),
                &[("L_001.fits", fits.clone()), ("notes.txt", b"hello hello hello hello".to_vec()), ("sub/L_002.FIT", fits.clone())],
                compression,
            );
            let listing = list_entries(&zip).unwrap();
            assert!(listing.skipped.is_empty(), "{:?}", listing.skipped);
            assert!(listing.bytes_read > 0);
            let by_name = |n: &str| listing.entries.iter().find(|e| e.path == n).unwrap();
            assert_eq!(listing.entries.len(), 3);
            assert!(by_name("L_001.fits").is_frame);
            assert!(by_name("sub/L_002.FIT").is_frame);
            assert!(!by_name("notes.txt").is_frame);
            let f = by_name("L_001.fits");
            assert_eq!(f.size, fits.len() as u64);
            assert_eq!(f.stored, matches!(compression, ArchiveCompression::Store));
            assert!(f.modified.is_some());
            assert_ne!(f.crc32, 0);
        }
    }

    #[test]
    fn unsafe_entry_names_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let fits = minimal_fits_bytes(tmp.path());
        let hostile = [
            "../x.fits",
            "/abs.fits",
            "a/../../b.fit",
            "__MACOSX/._L.fit",
            "._L.fit",
            "sub/._L.fits",
            "inner.zip",
            "C:evil.fits",
            "..\\w.fits",
        ];
        let mut files: Vec<(&str, Vec<u8>)> = hostile.iter().map(|n| (*n, fits.clone())).collect();
        files.push(("good.fits", fits.clone()));
        files.push(("sub/notes.txt", b"x".to_vec()));
        let zip = make_zip(tmp.path(), &files, ArchiveCompression::Store);

        let listing = list_entries(&zip).unwrap();
        let kept: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(kept, vec!["good.fits", "sub/notes.txt"]);
        let skipped: Vec<&str> = listing.skipped.iter().map(|(n, _)| n.as_str()).collect();
        for n in hostile {
            assert!(skipped.contains(&n), "{n} should be skipped, skipped = {skipped:?}");
        }
        let reason = |n: &str| listing.skipped.iter().find(|(s, _)| s == n).unwrap().1;
        assert_eq!(reason("../x.fits"), "unsafe name");
        assert_eq!(reason("__MACOSX/._L.fit"), "appledouble");
        assert_eq!(reason("._L.fit"), "appledouble");
        assert_eq!(reason("inner.zip"), "nested zip");
    }

    #[test]
    fn safe_names_predicate() {
        for ok in ["a.fits", "a/b/c.fit", "a..b/c.fits", "..a/b", "dir/.hidden.fits"] {
            assert!(is_safe_entry_name(ok), "{ok}");
        }
        for bad in ["", "/a", "\\a", "../a", "a/..", "a/../b", "a\\..\\b", "C:/a", "a\0b"] {
            assert!(!is_safe_entry_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn header_read_stops_at_end_card_store_and_deflate() {
        let tmp = TempDir::new().unwrap();
        let file = fits_with_header_blocks(3, 8 * 1024 * 1024);
        for compression in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            let dir = tmp.path().join(format!("{compression:?}"));
            std::fs::create_dir_all(&dir).unwrap();
            let zip = make_zip(&dir, &[("Lights/L_001.fits", file.clone())], compression);
            let zip_len = std::fs::metadata(&zip).unwrap().len();
            assert!(zip_len > 7 * 1024 * 1024, "noise must not compress away, zip is {zip_len}");

            let (header, bytes_read) = read_entry_header(&zip, "Lights/L_001.fits").unwrap();
            assert_eq!(header, file[..3 * FITS_BLOCK], "{compression:?}");
            assert!(bytes_read <= 64 * 1024, "{compression:?}: header read pulled {bytes_read} bytes");

            let listing = list_entries(&zip).unwrap();
            assert!(listing.bytes_read <= 64 * 1024, "{compression:?}: listing pulled {} bytes", listing.bytes_read);
        }
    }

    #[test]
    fn read_headers_reports_a_bad_entry_in_its_own_slot() {
        let tmp = TempDir::new().unwrap();
        let fits = minimal_fits_bytes(tmp.path());
        let zip = make_zip(
            tmp.path(),
            &[("a.fits", fits.clone()), ("junk.fits", vec![b'x'; 100]), ("b.xisf", b"not xisf at all, not at all".to_vec())],
            ArchiveCompression::Deflate,
        );
        let (res, bytes) = read_headers(&zip, &["a.fits", "junk.fits", "b.xisf", "missing.fits"]).unwrap();
        assert!(bytes > 0);
        assert_eq!(res.len(), 4);
        assert_eq!(res[0].0, "a.fits");
        assert_eq!(res[0].1.as_ref().unwrap(), &fits);
        assert!(res[1].1.is_err(), "100 bytes cannot hold a header block");
        assert!(res[2].1.as_ref().unwrap_err().to_string().contains("not an XISF signature"));
        assert!(res[3].1.is_err());
        assert!(read_entry_header(&zip, "missing.fits").is_err());
        assert!(read_headers(&tmp.path().join("nope.zip"), &["a.fits"]).is_err());
    }

    #[test]
    fn header_without_end_card_is_refused() {
        let tmp = TempDir::new().unwrap();
        let mut file = Vec::new();
        for _ in 0..(MAX_FITS_HEADER_BLOCKS + 2) * (FITS_BLOCK / 80) {
            file.extend(card("COMMENT never ends"));
        }
        let zip = make_zip(tmp.path(), &[("a.fits", file)], ArchiveCompression::Store);
        let err = read_entry_header(&zip, "a.fits").unwrap_err().to_string();
        assert!(err.contains("no END card"), "{err}");
    }

    #[test]
    fn xisf_header_read_uses_the_declared_length() {
        let tmp = TempDir::new().unwrap();
        let file = xisf_with_header_len(5000, 200_000);
        for compression in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            let dir = tmp.path().join(format!("{compression:?}"));
            std::fs::create_dir_all(&dir).unwrap();
            let zip = make_zip(&dir, &[("a.xisf", file.clone())], compression);
            let (header, _) = read_entry_header(&zip, "a.xisf").unwrap();
            assert_eq!(header.len(), 16 + 5000);
            assert_eq!(header, file[..16 + 5000]);
        }
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn extract_matches_source_and_hashes_like_xxh3_full_file() {
        for compression in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            let tmp = TempDir::new().unwrap();
            // 2.5 MiB: more than one 1 MiB chunk, plus a short tail.
            let bytes = fits_with_header_blocks(2, 5 * 512 * 1024);
            let zip = make_zip(tmp.path(), &[("Lights/L_001.fits", bytes.clone())], compression);
            let listing = list_entries(&zip).unwrap();
            let crc = listing.entries[0].crc32;

            let dest = tmp.path().join("out/deeper/L_001.fits");
            let mut seen = 0u64;
            let mut chunks = 0u32;
            let x = extract_entry(&zip, "Lights/L_001.fits", Some(crc), &dest, &never(), &mut |n| {
                seen += n;
                chunks += 1;
            })
            .unwrap();
            assert_eq!(std::fs::read(&dest).unwrap(), bytes);
            assert_eq!(x.bytes, bytes.len() as u64);
            assert_eq!(seen, bytes.len() as u64);
            assert!(chunks >= 1);
            assert_eq!(x.strong_hash, crate::package::xxh3_full_file(&dest).unwrap());
            assert!(!partial_path(&dest).exists());
        }
    }

    #[test]
    fn extract_rejects_a_wrong_expected_crc_and_leaves_no_file() {
        let tmp = TempDir::new().unwrap();
        let fits = minimal_fits_bytes(tmp.path());
        let zip = make_zip(tmp.path(), &[("a.fits", fits)], ArchiveCompression::Store);
        let crc = list_entries(&zip).unwrap().entries[0].crc32;
        let dest = tmp.path().join("a.fits");
        let err = extract_entry(&zip, "a.fits", Some(crc ^ 1), &dest, &never(), &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("changed since it was cataloged"), "{err}");
        assert!(!dest.exists());
        assert!(!partial_path(&dest).exists());
    }

    #[test]
    fn corrupt_entry_data_fails_the_crc_and_leaves_no_file() {
        let tmp = TempDir::new().unwrap();
        let bytes = fits_with_header_blocks(1, 300_000);
        let zip = make_zip(tmp.path(), &[("a.fits", bytes.clone())], ArchiveCompression::Store);
        // Stored data sits in the zip verbatim: flip one byte deep inside it.
        let mut raw = std::fs::read(&zip).unwrap();
        let at = raw.windows(1024).position(|w| w == &bytes[FITS_BLOCK + 1000..FITS_BLOCK + 2024]).expect("stored data in the zip");
        raw[at + 500] ^= 0xFF;
        std::fs::write(&zip, raw).unwrap();

        let dest = tmp.path().join("out/a.fits");
        let err = extract_entry(&zip, "a.fits", None, &dest, &never(), &mut |_| {}).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("checksum"), "{err:#}");
        assert!(!dest.exists());
        assert!(!partial_path(&dest).exists());
    }

    #[test]
    fn cancelled_extract_leaves_no_partial_file() {
        let tmp = TempDir::new().unwrap();
        let bytes = fits_with_header_blocks(1, 8 * 1024 * 1024);
        let zip = make_zip(tmp.path(), &[("a.fits", bytes.clone())], ArchiveCompression::Store);

        // Cancelled before the first chunk.
        let dest = tmp.path().join("pre/a.fits");
        let cancel = AtomicBool::new(true);
        let err = extract_entry(&zip, "a.fits", None, &dest, &cancel, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert!(!dest.exists() && !partial_path(&dest).exists());

        // Cancelled inside the entry: after the first chunk the file is partly written.
        let dest = tmp.path().join("mid/a.fits");
        let cancel = AtomicBool::new(false);
        let mut seen = 0u64;
        let err = extract_entry(&zip, "a.fits", None, &dest, &cancel, &mut |n| {
            seen += n;
            cancel.store(true, Ordering::Relaxed);
        })
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert!(seen > 0 && seen < bytes.len() as u64, "cancel must stop mid-entry, saw {seen}");
        assert!(!dest.exists() && !partial_path(&dest).exists());
    }

    #[test]
    fn extract_of_a_missing_entry_or_zip_fails_cleanly() {
        let tmp = TempDir::new().unwrap();
        let fits = minimal_fits_bytes(tmp.path());
        let zip = make_zip(tmp.path(), &[("a.fits", fits)], ArchiveCompression::Store);
        let dest = tmp.path().join("o/x.fits");
        assert!(extract_entry(&zip, "nope.fits", None, &dest, &never(), &mut |_| {}).is_err());
        assert!(extract_entry(&tmp.path().join("gone.zip"), "a.fits", None, &dest, &never(), &mut |_| {}).is_err());
        assert!(!dest.exists() && !partial_path(&dest).exists());
    }
}
