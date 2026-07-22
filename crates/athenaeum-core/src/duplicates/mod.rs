// Duplicate detection module
// Implements xxHash-based duplicate detection

pub mod backfill;

use std::path::Path;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use anyhow::Result;
use xxhash_rust::xxh3::Xxh3;
use chrono::{DateTime, Utc};

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

/// Compute metadata hash (quick, no file I/O required)
/// Hashes: size + modified_time + filename
/// Returns 16-character hex string
pub fn compute_metadata_hash(size: i64, modified_at: &DateTime<Utc>, filename: &str) -> String {
    let mut hasher = Xxh3::new();

    // Hash file size
    hasher.update(&size.to_le_bytes());

    // Hash modified time (as unix timestamp)
    let timestamp = modified_at.timestamp();
    hasher.update(&timestamp.to_le_bytes());

    // Hash filename
    hasher.update(filename.as_bytes());

    format!("{:016x}", hasher.digest())
}

/// Group files by size and hash to find duplicates
/// This function is a wrapper - actual implementation is in db::find_duplicate_groups
#[allow(dead_code)]
pub fn find_duplicates(conn: &rusqlite::Connection, use_content_hash: bool) -> Result<Vec<DuplicateGroup>> {
    crate::db::find_duplicate_groups(conn, use_content_hash).map_err(|e| anyhow::anyhow!(e.to_string()))
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

use crate::models::DuplicateGroup;
