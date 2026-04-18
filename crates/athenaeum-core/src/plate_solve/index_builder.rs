//! Pre-built all-sky quad index builder.
//!
//! Reads the HEALpix-indexed Tycho-2 catalog and produces a single binary
//! file containing every nearest-neighbor quad, indexed by scale-invariant
//! hash keys. Used at solve time for direct O(1) lookup.
//!
//! File format (little-endian):
//! ```text
//! MAGIC:             [u8; 4] = "QIDX"
//! VERSION:           u32 = 1
//! HASH_TOLERANCE:    f64
//! NUM_BUCKETS:       u32
//! STAR_COUNT:        u64
//! QUAD_COUNT:        u64
//! BUCKET_OFFSETS:    [u64; NUM_BUCKETS]  (byte offset of each bucket's first entry)
//! BUCKET_LENGTHS:    [u32; NUM_BUCKETS]  (number of entries in each bucket)
//! ENTRIES:           [QuadEntry; QUAD_COUNT]
//! ```
//!
//! Each `QuadEntry` is 52 bytes:
//! ```text
//! hash_key:          [i32; 5]    (20 bytes)
//! longest_dist_deg:  f32         (4 bytes)
//! stars_ra:          [f32; 4]    (16 bytes)
//! stars_dec:         [f32; 4]    (16 bytes)  (28 + 4 = 32)  — 20 + 4 + 16 + 16 = 56
//! ```
//! Actual entry size = 56 bytes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use byteorder::{LittleEndian, WriteBytesExt};
use cdshealpix::nested;
use memmap2::Mmap;

use crate::catalog::binary_format::{read_records_until_mag, StarRecord};
use crate::catalog::healpix::HEALPIX_DEPTH;

/// Magic bytes for the index file.
pub const MAGIC: [u8; 4] = *b"QIDX";
/// File format version.
pub const VERSION: u32 = 1;
/// Number of lookup buckets (indexed by quantized first ratio).
pub const NUM_BUCKETS: u32 = 1024;
/// Size in bytes of one `QuadEntry` on disk.
pub const ENTRY_SIZE: usize = 56;
/// Default hash tolerance (quantization bin size for ratios).
pub const DEFAULT_HASH_TOLERANCE: f64 = 0.005;
/// Default magnitude limit when building the index.
pub const DEFAULT_MAG_LIMIT: f32 = 11.0;

/// Progress notification during index building.
#[derive(Clone, Debug)]
pub enum IndexBuildProgress {
    Reading {
        pixel: u64,
        total: u64,
        quads_so_far: u64,
    },
    Writing {
        bytes_written: u64,
        total_bytes: u64,
    },
    Complete {
        quad_count: u64,
        size_bytes: u64,
    },
}

/// An in-memory quad entry before writing to disk.
#[derive(Clone, Debug)]
struct Quad {
    hash_key: [i32; 5],
    longest_dist_deg: f32,
    stars_ra: [f32; 4],
    stars_dec: [f32; 4],
}

/// Build the all-sky quad index from a Tycho-2 HEALpix catalog.
pub struct IndexBuilder;

impl IndexBuilder {
    /// Build the index file.
    ///
    /// `catalog_dir` should contain the `healpix_NNNNNN.bin` files produced
    /// by the Tycho-2 converter. `output_path` is the destination for the
    /// pre-built index file.
    pub fn build(
        catalog_dir: &Path,
        output_path: &Path,
        mag_limit: f32,
        hash_tolerance: f64,
        cancel_flag: Arc<AtomicBool>,
        progress: &dyn Fn(IndexBuildProgress),
    ) -> Result<u64> {
        let total_pixels = nested::n_hash(HEALPIX_DEPTH);
        let mut quads: Vec<Quad> = Vec::with_capacity(2_500_000);
        let mut total_stars: u64 = 0;

        for pid in 0..total_pixels {
            if cancel_flag.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("Index build cancelled"));
            }

            // Load center stars
            let center_stars = load_pixel_stars(catalog_dir, pid, mag_limit)?;
            if center_stars.is_empty() {
                continue;
            }
            total_stars += center_stars.len() as u64;

            // Load neighbor stars so we can find nearest neighbors across pixel boundaries
            let mut neighbor_pids = Vec::with_capacity(8);
            nested::append_bulk_neighbours(HEALPIX_DEPTH, pid, &mut neighbor_pids);
            let mut all_stars: Vec<StarWithSource> = center_stars
                .iter()
                .map(|s| StarWithSource {
                    ra: s.ra as f64,
                    dec: s.dec as f64,
                    is_center: true,
                })
                .collect();
            for npid in &neighbor_pids {
                let ns = load_pixel_stars(catalog_dir, *npid, mag_limit)?;
                for s in ns {
                    all_stars.push(StarWithSource {
                        ra: s.ra as f64,
                        dec: s.dec as f64,
                        is_center: false,
                    });
                }
            }

            // For each CENTER star, find its 3 nearest neighbors in `all_stars`
            // (center + neighbors). Only center stars emit quads (dedup across
            // pixel boundaries).
            for (i, star_i) in all_stars.iter().enumerate() {
                if !star_i.is_center {
                    continue;
                }
                let quad = build_quad_for_star(&all_stars, i, hash_tolerance);
                if let Some(q) = quad {
                    quads.push(q);
                }
            }

            if pid % 512 == 0 || pid + 1 == total_pixels {
                progress(IndexBuildProgress::Reading {
                    pixel: pid + 1,
                    total: total_pixels,
                    quads_so_far: quads.len() as u64,
                });
            }
        }

        let quad_count = quads.len() as u64;
        eprintln!(
            "index_builder: built {} quads from {} stars",
            quad_count, total_stars
        );

        // Group quads into buckets by the first ratio of their hash key.
        let mut buckets: HashMap<u32, Vec<Quad>> = HashMap::new();
        for q in quads {
            let bucket = bucket_for_ratio(q.hash_key[0], hash_tolerance);
            buckets.entry(bucket).or_default().push(q);
        }

        // Sort quads within each bucket by first ratio for deterministic output
        for bucket in buckets.values_mut() {
            bucket.sort_by_key(|q| q.hash_key[0]);
        }

        // Write to disk
        let file = File::create(output_path)
            .with_context(|| format!("create {}", output_path.display()))?;
        let mut writer = BufWriter::new(file);

        // Header
        writer.write_all(&MAGIC)?;
        writer.write_u32::<LittleEndian>(VERSION)?;
        writer.write_f64::<LittleEndian>(hash_tolerance)?;
        writer.write_u32::<LittleEndian>(NUM_BUCKETS)?;
        writer.write_u64::<LittleEndian>(total_stars)?;
        writer.write_u64::<LittleEndian>(quad_count)?;

        // Bucket offsets + lengths (placeholder, patched after writing entries)
        let header_end = writer.stream_position()?;
        let bucket_table_size = (NUM_BUCKETS as u64) * (8 + 4);
        // Write zero placeholders
        for _ in 0..NUM_BUCKETS {
            writer.write_u64::<LittleEndian>(0)?;
            writer.write_u32::<LittleEndian>(0)?;
        }

        let entries_start = writer.stream_position()?;
        let mut bucket_offsets: Vec<u64> = vec![0; NUM_BUCKETS as usize];
        let mut bucket_lengths: Vec<u32> = vec![0; NUM_BUCKETS as usize];

        for bucket_id in 0..NUM_BUCKETS {
            if let Some(bucket) = buckets.get(&bucket_id) {
                let offset = writer.stream_position()? - entries_start;
                bucket_offsets[bucket_id as usize] = offset;
                bucket_lengths[bucket_id as usize] = bucket.len() as u32;
                for q in bucket {
                    write_quad_entry(&mut writer, q)?;
                }
            } else {
                let offset = writer.stream_position()? - entries_start;
                bucket_offsets[bucket_id as usize] = offset;
                bucket_lengths[bucket_id as usize] = 0;
            }
        }

        let file_end = writer.stream_position()?;

        // Patch bucket table
        writer.seek(SeekFrom::Start(header_end))?;
        for i in 0..NUM_BUCKETS as usize {
            writer.write_u64::<LittleEndian>(bucket_offsets[i])?;
            writer.write_u32::<LittleEndian>(bucket_lengths[i])?;
        }

        // Sanity check we're where we expect
        let patched_end = writer.stream_position()?;
        assert_eq!(patched_end, header_end + bucket_table_size);

        writer.flush()?;
        drop(writer);

        progress(IndexBuildProgress::Complete {
            quad_count,
            size_bytes: file_end,
        });

        Ok(quad_count)
    }
}

#[derive(Clone, Debug)]
struct StarWithSource {
    ra: f64,
    dec: f64,
    is_center: bool,
}

fn load_pixel_stars(
    catalog_dir: &Path,
    pixel_id: u64,
    mag_limit: f32,
) -> Result<Vec<StarRecord>> {
    let path = catalog_dir.join(format!("healpix_{:06}.bin", pixel_id));
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("mmap {}", path.display()))?;
    Ok(read_records_until_mag(&mmap, mag_limit))
}

/// Great-circle angular separation in degrees.
fn angular_separation_deg(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let r1 = ra1.to_radians();
    let r2 = ra2.to_radians();
    let d1 = dec1.to_radians();
    let d2 = dec2.to_radians();
    // Vincenty-style haversine for numerical stability at small angles
    let dra = r2 - r1;
    let ddec = d2 - d1;
    let a = (ddec / 2.0).sin().powi(2) + d1.cos() * d2.cos() * (dra / 2.0).sin().powi(2);
    2.0 * a.sqrt().asin().to_degrees()
}

fn build_quad_for_star(
    stars: &[StarWithSource],
    i: usize,
    hash_tolerance: f64,
) -> Option<Quad> {
    let center = &stars[i];

    // Find 3 nearest neighbors (by great-circle distance).
    let mut n1 = (usize::MAX, f64::INFINITY);
    let mut n2 = (usize::MAX, f64::INFINITY);
    let mut n3 = (usize::MAX, f64::INFINITY);

    for (j, s) in stars.iter().enumerate() {
        if j == i {
            continue;
        }
        let d = angular_separation_deg(center.ra, center.dec, s.ra, s.dec);
        if d < n1.1 {
            n3 = n2;
            n2 = n1;
            n1 = (j, d);
        } else if d < n2.1 {
            n3 = n2;
            n2 = (j, d);
        } else if d < n3.1 {
            n3 = (j, d);
        }
    }

    if n3.0 == usize::MAX {
        return None;
    }

    let indices = [i, n1.0, n2.0, n3.0];

    // Compute the 6 pairwise distances
    let mut distances = [0.0f64; 6];
    let mut k = 0;
    for a in 0..4 {
        for b in (a + 1)..4 {
            distances[k] = angular_separation_deg(
                stars[indices[a]].ra,
                stars[indices[a]].dec,
                stars[indices[b]].ra,
                stars[indices[b]].dec,
            );
            k += 1;
        }
    }

    // Sort ascending — the last element is the longest.
    distances.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let longest = distances[5];
    if longest <= 0.0 {
        return None;
    }

    // Ratios of the 5 shorter distances to the longest
    let ratios = [
        distances[0] / longest,
        distances[1] / longest,
        distances[2] / longest,
        distances[3] / longest,
        distances[4] / longest,
    ];

    let hash_key = [
        quantize_ratio(ratios[0], hash_tolerance),
        quantize_ratio(ratios[1], hash_tolerance),
        quantize_ratio(ratios[2], hash_tolerance),
        quantize_ratio(ratios[3], hash_tolerance),
        quantize_ratio(ratios[4], hash_tolerance),
    ];

    let stars_ra = [
        stars[indices[0]].ra as f32,
        stars[indices[1]].ra as f32,
        stars[indices[2]].ra as f32,
        stars[indices[3]].ra as f32,
    ];
    let stars_dec = [
        stars[indices[0]].dec as f32,
        stars[indices[1]].dec as f32,
        stars[indices[2]].dec as f32,
        stars[indices[3]].dec as f32,
    ];

    Some(Quad {
        hash_key,
        longest_dist_deg: longest as f32,
        stars_ra,
        stars_dec,
    })
}

/// Quantize a ratio in [0, 1] into an integer hash key.
pub fn quantize_ratio(ratio: f64, hash_tolerance: f64) -> i32 {
    (ratio / hash_tolerance).round() as i32
}

/// Map a quantized ratio to a bucket id in [0, NUM_BUCKETS).
pub fn bucket_for_ratio(quantized: i32, hash_tolerance: f64) -> u32 {
    // The first ratio is in [0, 1], quantized to `1 / hash_tolerance` bins.
    // Map that space to NUM_BUCKETS uniformly.
    let max_quantized = (1.0 / hash_tolerance).round() as i32;
    let clamped = quantized.clamp(0, max_quantized);
    let bucket = (clamped as u64 * NUM_BUCKETS as u64) / (max_quantized as u64 + 1);
    bucket.min(NUM_BUCKETS as u64 - 1) as u32
}

fn write_quad_entry<W: Write>(writer: &mut W, q: &Quad) -> Result<()> {
    for k in &q.hash_key {
        writer.write_i32::<LittleEndian>(*k)?;
    }
    writer.write_f32::<LittleEndian>(q.longest_dist_deg)?;
    for r in &q.stars_ra {
        writer.write_f32::<LittleEndian>(*r)?;
    }
    for d in &q.stars_dec {
        writer.write_f32::<LittleEndian>(*d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_roundtrip() {
        let t = 0.01;
        assert_eq!(quantize_ratio(0.0, t), 0);
        assert_eq!(quantize_ratio(0.1, t), 10);
        assert_eq!(quantize_ratio(0.5, t), 50);
        assert_eq!(quantize_ratio(1.0, t), 100);
    }

    #[test]
    fn bucket_spreads_uniformly() {
        let t = 0.005;
        let b0 = bucket_for_ratio(0, t);
        let b50 = bucket_for_ratio(100, t);
        let b_max = bucket_for_ratio(200, t);
        assert_eq!(b0, 0);
        assert!(b50 < b_max);
        // Last bucket should be near NUM_BUCKETS - 1
        assert!(b_max >= NUM_BUCKETS - 10);
        assert!(b_max < NUM_BUCKETS);
    }

    #[test]
    fn angular_sep_basic() {
        // Same point
        assert!(angular_separation_deg(0.0, 0.0, 0.0, 0.0).abs() < 1e-12);
        // 1 degree east along equator
        let d = angular_separation_deg(0.0, 0.0, 1.0, 0.0);
        assert!((d - 1.0).abs() < 1e-6, "expected ~1°, got {d}");
        // 90 degrees apart
        let d90 = angular_separation_deg(0.0, 0.0, 90.0, 0.0);
        assert!((d90 - 90.0).abs() < 1e-6);
    }

    #[test]
    fn build_quad_for_synthetic_field() {
        let stars = vec![
            StarWithSource { ra: 180.0, dec: 45.0, is_center: true },
            StarWithSource { ra: 180.1, dec: 45.05, is_center: false },
            StarWithSource { ra: 180.2, dec: 45.15, is_center: false },
            StarWithSource { ra: 180.3, dec: 45.0, is_center: false },
            StarWithSource { ra: 181.0, dec: 46.0, is_center: false }, // far
        ];
        let quad = build_quad_for_star(&stars, 0, 0.005);
        assert!(quad.is_some());
        let q = quad.unwrap();
        // All ratios should be in [0, 1]
        for r in &q.hash_key {
            assert!(*r >= 0, "hash key must be non-negative: {r}");
        }
        assert!(q.longest_dist_deg > 0.0);
    }
}
