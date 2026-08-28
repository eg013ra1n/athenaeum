// Duplicate detection module
// Implements xxHash-based duplicate detection

pub mod backfill;

use std::path::Path;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use anyhow::Result;
use xxhash_rust::xxh3::Xxh3;

/// Compute XXH3_64 hash for a file using 3-position sampling
/// Samples: beginning (512KB), middle (512KB), end (512KB)
/// Uses 64KB buffer for efficient I/O
pub fn compute_xxhash(path: &Path) -> Result<String> {
    const BUFFER_SIZE: usize = 64 * 1024; // 64KB buffer
    const CHUNK_SIZE: usize = 512 * 1024; // 512KB per sample

    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len() as usize;
    let mut hasher = Xxh3::new();
    let mut buffer = vec![0; BUFFER_SIZE];

    // Helper to read and hash a chunk
    let mut read_and_hash = |file: &mut File, start_pos: u64, chunk_size: usize| -> Result<()> {
        file.seek(SeekFrom::Start(start_pos))?;
        let mut remaining = chunk_size;

        while remaining > 0 {
            let to_read = remaining.min(BUFFER_SIZE);
            let bytes_read = file.read(&mut buffer[..to_read])?;
            if bytes_read == 0 {
                break; // EOF
            }
            hasher.update(&buffer[..bytes_read]);
            remaining -= bytes_read;
        }
        Ok(())
    };

    // Position 1: Beginning (first 512KB from byte 0)
    read_and_hash(&mut file, 0, CHUNK_SIZE.min(file_size))?;

    // Position 2: Middle (512KB centered at midpoint)
    if file_size > CHUNK_SIZE {
        let middle_start = (file_size / 2).saturating_sub(CHUNK_SIZE / 2);
        read_and_hash(&mut file, middle_start as u64, CHUNK_SIZE.min(file_size - middle_start))?;
    }

    // Position 3: End (last 512KB)
    if file_size > CHUNK_SIZE * 2 {
        let end_start = file_size.saturating_sub(CHUNK_SIZE);
        read_and_hash(&mut file, end_start as u64, CHUNK_SIZE)?;
    }

    Ok(format!("{:016x}", hasher.digest()))
}

/// Hash a file's ENTIRE contents with XXH3_64.
///
/// The counterpart to [`compute_xxhash`], which samples three 512 KiB regions
/// and is documented as lossy. Sampling is not merely lossy here, it is
/// hopeless: measured over 20 000 trials, a sampling scheme's chance of
/// noticing a changed pixel equals the fraction of the file it reads, and the
/// real divergence between two PixInsight masters is ONE Float32 pixel — 4
/// bytes in 77 MiB. Spending more of the budget on more, smaller samples makes
/// it strictly worse. So masters are decided by reading everything.
///
/// Affordable only because the caller hashes a header-shortlisted subset (see
/// [`backfill::fill_master_strong_hashes`]): 7.5 GiB, not 2.62 TiB.
pub fn compute_full_xxhash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}

/// Group files by size and hash to find duplicates
/// This function is a wrapper - actual implementation is in db::find_duplicate_groups
#[allow(dead_code)]
pub fn find_duplicates(conn: &rusqlite::Connection, use_content_hash: bool) -> Result<Vec<DuplicateGroup>> {
    let key = crate::db::DuplicateKey::from_setting(use_content_hash);
    crate::db::find_duplicate_groups(conn, key).map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// Verify two files are byte-identical (optional safeguard).
/// Use as an opt-in deep-verify step before destructive operations on
/// duplicates — the sampling hash (`compute_xxhash`) is fast but lossy on
/// huge files where only the sampled regions are compared.
pub fn verify_byte_identical(path1: &Path, path2: &Path) -> Result<bool> {
    use std::io::Read;

    let file1 = File::open(path1)?;
    let file2 = File::open(path2)?;

    let mut reader1 = BufReader::new(file1);
    let mut reader2 = BufReader::new(file2);

    let mut buffer1 = vec![0; 8192];
    let mut buffer2 = vec![0; 8192];

    loop {
        let bytes1 = reader1.read(&mut buffer1)?;
        let bytes2 = reader2.read(&mut buffer2)?;

        if bytes1 != bytes2 {
            return Ok(false);
        }

        if bytes1 == 0 {
            return Ok(true);
        }

        if buffer1[..bytes1] != buffer2[..bytes2] {
            return Ok(false);
        }
    }
}

/// How [`crate::api::files::verify_duplicate_pair`] reached its verdict.
///
/// `Bytes` — both files were read now, in lockstep, and compared
/// byte-for-byte. `StoredHash` — both catalog rows carried a current
/// `files.strong_hash` (full-content xxh3, staleness-checked against the
/// disk's size/mtime), so the verdict came from the column without reading
/// either file. Hash inequality is sound in both directions here: equal
/// bytes cannot hash differently, and the equality direction carries the
/// same 64-bit trust the Master duplicate key already stakes deletions on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum VerifyMethod {
    Bytes,
    StoredHash,
}

/// Outcome of one duplicate-pair verification.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct VerifyPairResult {
    pub identical: bool,
    pub method: VerifyMethod,
}

/// [`verify_byte_identical`], but the read is not wasted: while comparing,
/// the first file's bytes feed an incremental XXH3 — so an identical pair
/// returns the full-content digest both files share, in exactly
/// [`compute_full_xxhash`]'s format (streaming xxh3 is buffer-size
/// independent), ready to be banked into `files.strong_hash`.
///
/// A mismatch still early-exits on the first differing chunk and returns
/// `None`: any digest computed by then would describe a prefix, not the
/// file, and finishing both reads just to hash two files that are NOT
/// copies of each other would spend the I/O this shortcut exists to save.
pub fn verify_byte_identical_hashing(
    path1: &Path,
    path2: &Path,
) -> Result<(bool, Option<String>)> {
    use std::io::Read;

    let file1 = File::open(path1)?;
    let file2 = File::open(path2)?;

    let mut reader1 = BufReader::new(file1);
    let mut reader2 = BufReader::new(file2);

    let mut buffer1 = vec![0; 8192];
    let mut buffer2 = vec![0; 8192];
    let mut hasher = Xxh3::new();

    loop {
        let bytes1 = reader1.read(&mut buffer1)?;
        let bytes2 = reader2.read(&mut buffer2)?;

        if bytes1 != bytes2 {
            return Ok((false, None));
        }

        if bytes1 == 0 {
            return Ok((true, Some(format!("{:016x}", hasher.digest()))));
        }

        if buffer1[..bytes1] != buffer2[..bytes2] {
            return Ok((false, None));
        }

        hasher.update(&buffer1[..bytes1]);
    }
}

use crate::models::DuplicateGroup;

#[cfg(test)]
mod verify_hashing_tests {
    use super::*;
    use std::io::Write as _;

    fn write_file(dir: &std::path::Path, name: &str, body: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        File::create(&p).unwrap().write_all(body).unwrap();
        p
    }

    /// Identical files: the compare must return the full-content hash it
    /// already paid to read, and that hash must be byte-for-byte the one
    /// `compute_full_xxhash` would produce — `files.strong_hash` stores the
    /// latter, so a differing format would poison the master key's column.
    #[test]
    fn identical_files_yield_the_stored_hash_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Larger than one 8 KiB compare buffer, so the incremental hash is
        // proven across chunk boundaries, not just on a single read.
        let body = vec![0xA7u8; 50_000];
        let a = write_file(tmp.path(), "a.fits", &body);
        let b = write_file(tmp.path(), "b.fits", &body);

        let (identical, digest) = verify_byte_identical_hashing(&a, &b).unwrap();
        assert!(identical);
        assert_eq!(digest.as_deref(), Some(compute_full_xxhash(&a).unwrap().as_str()));
    }

    /// Different files: no digest — the compare early-exits on the first
    /// differing chunk, so any digest it could return would describe a
    /// prefix, not the file. None is the only honest answer.
    #[test]
    fn different_files_yield_no_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = write_file(tmp.path(), "a.fits", &vec![1u8; 50_000]);
        let mut body = vec![1u8; 50_000];
        body[10_000] = 2; // differs inside the second compare chunk
        let b = write_file(tmp.path(), "b.fits", &body);

        let (identical, digest) = verify_byte_identical_hashing(&a, &b).unwrap();
        assert!(!identical);
        assert!(digest.is_none());
    }

    /// Same bytes, different lengths: not identical (the shorter is a prefix).
    #[test]
    fn prefix_files_are_not_identical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = write_file(tmp.path(), "a.fits", &vec![7u8; 20_000]);
        let b = write_file(tmp.path(), "b.fits", &vec![7u8; 20_001]);

        let (identical, digest) = verify_byte_identical_hashing(&a, &b).unwrap();
        assert!(!identical);
        assert!(digest.is_none());
    }
}
