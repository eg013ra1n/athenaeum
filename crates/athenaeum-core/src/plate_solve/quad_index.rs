//! Memory-mapped loader for the all-sky quad index.
//!
//! The file format is defined in `index_builder.rs`. This module opens the
//! index as a `memmap2::Mmap`, parses the header, and provides O(1) lookup
//! by scale-invariant hash key.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use byteorder::{ByteOrder, LittleEndian};
use memmap2::Mmap;

use super::index_builder::{
    bucket_for_ratio, quantize_ratio, ENTRY_SIZE, MAGIC, NUM_BUCKETS, VERSION,
};

/// A single lookup result — one catalog quad matching a query hash.
#[derive(Clone, Debug)]
pub struct QuadLookup {
    pub hash_key: [i32; 5],
    pub longest_dist_deg: f32,
    pub stars_ra: [f32; 4],
    pub stars_dec: [f32; 4],
}

/// Memory-mapped all-sky quad index.
pub struct QuadIndex {
    mmap: Mmap,
    hash_tolerance: f64,
    #[allow(dead_code)]
    star_count: u64,
    quad_count: u64,
    /// Offset into `mmap` where entries start (after header + bucket table).
    entries_start: u64,
    /// Per-bucket offset (relative to `entries_start`).
    bucket_offsets: Vec<u64>,
    /// Per-bucket entry count.
    bucket_lengths: Vec<u32>,
}

impl QuadIndex {
    /// Load an index file. The file is memory-mapped, not copied.
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("mmap {}", path.display()))?;

        if mmap.len() < 4 + 4 + 8 + 4 + 8 + 8 {
            return Err(anyhow::anyhow!("quad index file too small"));
        }

        // Parse header
        let mut p = 0usize;
        let magic = &mmap[p..p + 4];
        if magic != MAGIC {
            return Err(anyhow::anyhow!(
                "quad index magic mismatch: expected {:?}, got {:?}",
                MAGIC,
                magic
            ));
        }
        p += 4;

        let version = LittleEndian::read_u32(&mmap[p..p + 4]);
        p += 4;
        if version != VERSION {
            return Err(anyhow::anyhow!(
                "quad index version mismatch: expected {}, got {}",
                VERSION,
                version
            ));
        }

        let hash_tolerance = LittleEndian::read_f64(&mmap[p..p + 8]);
        p += 8;
        let num_buckets = LittleEndian::read_u32(&mmap[p..p + 4]);
        p += 4;
        if num_buckets != NUM_BUCKETS {
            return Err(anyhow::anyhow!(
                "quad index bucket count mismatch: expected {}, got {}",
                NUM_BUCKETS,
                num_buckets
            ));
        }
        let star_count = LittleEndian::read_u64(&mmap[p..p + 8]);
        p += 8;
        let quad_count = LittleEndian::read_u64(&mmap[p..p + 8]);
        p += 8;

        // Bucket table
        let bucket_table_size = (NUM_BUCKETS as usize) * (8 + 4);
        if p + bucket_table_size > mmap.len() {
            return Err(anyhow::anyhow!("quad index truncated in bucket table"));
        }

        let mut bucket_offsets = Vec::with_capacity(NUM_BUCKETS as usize);
        let mut bucket_lengths = Vec::with_capacity(NUM_BUCKETS as usize);
        for _ in 0..NUM_BUCKETS {
            bucket_offsets.push(LittleEndian::read_u64(&mmap[p..p + 8]));
            p += 8;
            bucket_lengths.push(LittleEndian::read_u32(&mmap[p..p + 4]));
            p += 4;
        }

        let entries_start = p as u64;

        Ok(QuadIndex {
            mmap,
            hash_tolerance,
            star_count,
            quad_count,
            entries_start,
            bucket_offsets,
            bucket_lengths,
        })
    }

    pub fn quad_count(&self) -> u64 {
        self.quad_count
    }

    pub fn hash_tolerance(&self) -> f64 {
        self.hash_tolerance
    }

    /// Look up a hash key. Checks the target bucket and its immediate
    /// neighbors (multi-probe tolerance). Returns up to a few matching
    /// catalog quads.
    pub fn lookup(&self, hash_key: &[i32; 5]) -> Vec<QuadLookup> {
        let bucket = bucket_for_ratio(hash_key[0], self.hash_tolerance);

        let mut results = Vec::new();

        // Check the target bucket and ±1 neighbors
        let start_b = bucket.saturating_sub(1);
        let end_b = (bucket + 1).min(NUM_BUCKETS - 1);

        for b in start_b..=end_b {
            let offset = self.bucket_offsets[b as usize];
            let length = self.bucket_lengths[b as usize];
            if length == 0 {
                continue;
            }

            let start = self.entries_start + offset;
            for i in 0..length {
                let entry_off = (start as usize) + (i as usize) * ENTRY_SIZE;
                if entry_off + ENTRY_SIZE > self.mmap.len() {
                    break;
                }
                let entry = parse_entry(&self.mmap[entry_off..entry_off + ENTRY_SIZE]);

                // Check all 5 hash dimensions within ±1 tolerance
                let mut ok = true;
                for k in 0..5 {
                    if (entry.hash_key[k] - hash_key[k]).abs() > 1 {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    results.push(entry);
                }
            }
        }

        results
    }
}

fn parse_entry(bytes: &[u8]) -> QuadLookup {
    debug_assert_eq!(bytes.len(), ENTRY_SIZE);
    let mut p = 0usize;
    let mut hash_key = [0i32; 5];
    for k in &mut hash_key {
        *k = LittleEndian::read_i32(&bytes[p..p + 4]);
        p += 4;
    }
    let longest_dist_deg = LittleEndian::read_f32(&bytes[p..p + 4]);
    p += 4;
    let mut stars_ra = [0.0f32; 4];
    for r in &mut stars_ra {
        *r = LittleEndian::read_f32(&bytes[p..p + 4]);
        p += 4;
    }
    let mut stars_dec = [0.0f32; 4];
    for d in &mut stars_dec {
        *d = LittleEndian::read_f32(&bytes[p..p + 4]);
        p += 4;
    }
    QuadLookup {
        hash_key,
        longest_dist_deg,
        stars_ra,
        stars_dec,
    }
}

/// Helper for callers to compute the hash key of a given ratio array.
pub fn hash_key_from_ratios(ratios: &[f64; 5], hash_tolerance: f64) -> [i32; 5] {
    [
        quantize_ratio(ratios[0], hash_tolerance),
        quantize_ratio(ratios[1], hash_tolerance),
        quantize_ratio(ratios[2], hash_tolerance),
        quantize_ratio(ratios[3], hash_tolerance),
        quantize_ratio(ratios[4], hash_tolerance),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tempfile::TempDir;

    use crate::catalog::binary_format::StarRecord;
    use crate::catalog::write_catalog_to_healpix;
    use crate::plate_solve::index_builder::{IndexBuilder, DEFAULT_HASH_TOLERANCE};

    #[test]
    fn build_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let catalog_dir = tmp.path().join("tycho2");
        let index_path = tmp.path().join("quad_index.bin");

        // Make a tiny catalog: 30 stars scattered in a small region
        let mut records = Vec::new();
        for i in 0..30 {
            let ra = 180.0 + (i as f32) * 0.1;
            let dec = 45.0 + ((i as f32) * 0.07) % 2.0;
            records.push(StarRecord::from_values(ra, dec, 6.0 + i as f32 * 0.1, 0.0, 0.0));
        }
        write_catalog_to_healpix(&records, &catalog_dir).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let count = IndexBuilder::build(
            &catalog_dir,
            &index_path,
            12.0,
            DEFAULT_HASH_TOLERANCE,
            cancel,
            &|_| {},
        )
        .expect("build should succeed");

        assert!(count > 0, "should build at least one quad");

        let index = QuadIndex::load(&index_path).expect("load should succeed");
        assert_eq!(index.quad_count(), count);
        assert!((index.hash_tolerance() - DEFAULT_HASH_TOLERANCE).abs() < 1e-12);
    }
}
