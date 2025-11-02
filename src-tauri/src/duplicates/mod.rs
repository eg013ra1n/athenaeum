// Duplicate detection module
// Implements xxHash-based duplicate detection

use std::path::Path;
use std::fs::File;
use std::io::{BufReader, Read};
use anyhow::Result;
use xxhash_rust::xxh3::Xxh3;
use chrono::{DateTime, Utc};

/// Compute XXH3_64 hash for a file
pub fn compute_xxhash(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Xxh3::new();
    let mut buffer = vec![0; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
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
pub fn find_duplicates(conn: &rusqlite::Connection) -> Result<Vec<DuplicateGroup>> {
    crate::db::find_duplicate_groups(conn).map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// Verify two files are byte-identical (optional safeguard)
/// Used before destructive operations on duplicates
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
