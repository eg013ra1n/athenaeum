//! Build the 4 density-limited tier caches by slicing the G<21 bins.
//!
//! Each tier is built with `solvemyastro::cache::build_cache_parallel`, which
//! saturates all cores (parallel positioned writes, no scratch files). We read
//! only the *prefix* of each bin (the bands are prefixes of the mag-sorted cell),
//! so dense galactic-plane cells are never fully parsed.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use athenaeum_core::catalog::binary_format::{self, RECORD_SIZE};
use solvemyastro::cache::build_cache_parallel;
use solvemyastro::StarRecord;

use crate::tiers::{cell_cum_counts, slice_select, TIER_DENSITIES};

fn to_smac(r: &binary_format::StarRecord) -> StarRecord {
    StarRecord {
        ra: r.ra as f64,
        dec: r.dec as f64,
        mag: r.mag(),
        pmra_mas_yr: r.pmra_mas_yr(),
        pmdec_mas_yr: r.pmdec_mas_yr(),
    }
}

/// Read pixel `px`'s band `[lo, hi)` from its `healpix_<px>.bin` tile (records
/// mag-sorted). Reads only the first `min(hi, total)` records — the bands are
/// prefixes of the sorted cell, so dense cells are never fully parsed. Missing
/// tiles (empty pixels) yield an empty band.
fn read_band(bins_dir: &Path, px: u64, lo: usize, hi: usize) -> Vec<StarRecord> {
    let path = bins_dir.join(format!("healpix_{px:06}.bin"));
    let total = std::fs::metadata(&path)
        .map(|m| m.len() as usize / RECORD_SIZE)
        .unwrap_or(0);
    let n = hi.min(total); // records to actually read (prefix)
    if n == 0 || lo >= n {
        return Vec::new();
    }
    let mut buf = vec![0u8; n * RECORD_SIZE];
    if let Err(e) = File::open(&path).and_then(|mut f| f.read_exact(&mut buf)) {
        eprintln!("warn: read {} failed: {e} — skipping", path.display());
        return Vec::new();
    }
    // The bin is mag-sorted, so the first n records are the n brightest.
    let cell = binary_format::read_records_until_mag(&buf, f32::MAX);
    slice_select(&cell, lo, hi).iter().map(to_smac).collect()
}

/// Build `out_dir/tier_<density>/stars.smac` for each tier from the G<21 bins.
/// Returns `(density, star_count)` per tier in `TIER_DENSITIES` order. Each tier
/// build saturates all cores; tiers run sequentially.
pub fn build_layers(bins_dir: &Path, out_dir: &Path, epoch: f64) -> Result<Vec<(u32, usize)>> {
    let bounds = cell_cum_counts();
    let mut out = Vec::with_capacity(TIER_DENSITIES.len());

    for (k, &density) in TIER_DENSITIES.iter().enumerate() {
        let (lo, hi) = (bounds[k], bounds[k + 1]);
        let tier_dir = out_dir.join(format!("tier_{density}"));
        println!("  building tier_{density} (band [{lo},{hi}))…");
        let n = build_cache_parallel(&tier_dir, epoch, |px| read_band(bins_dir, px, lo, hi))
            .with_context(|| format!("build tier_{density}"))?;
        println!("  tier_{density}: {n} stars");
        out.push((density, n));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use athenaeum_core::catalog::binary_format::{write_records, StarRecord as BinRec};
    use std::io::BufWriter;

    // One dense synthetic cell (pixel 0) with 7000 mag-ascending stars, so
    // every tier band is exercised and the dense cap (6714) bites.
    fn write_dense_bin(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        let mut recs: Vec<BinRec> = (0..7000)
            .map(|i| BinRec::from_values(10.0, 20.0, 5.0 + i as f32 * 0.001, 0.0, 0.0))
            .collect();
        let f = std::fs::File::create(dir.join("healpix_000000.bin")).unwrap();
        let mut w = BufWriter::new(f);
        write_records(&mut w, &mut recs).unwrap();
    }

    #[test]
    fn builds_four_disjoint_tier_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let bins = tmp.path().join("gaia_dr3");
        write_dense_bin(&bins);
        let out = tmp.path().join("out");

        let counts = build_layers(&bins, &out, 2016.0).unwrap();
        // bands [0,420) [420,1679) [1679,4196) [4196,6714)
        assert_eq!(counts, vec![(500, 420), (2000, 1259), (5000, 2517), (8000, 2518)]);
        // total kept == dense cap, not all 7000 (286 faint stars dropped)
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        assert_eq!(total, 6714);

        // each tier opens and reports its own count
        for (d, n) in counts {
            let c = solvemyastro::StarCache::open(&out.join(format!("tier_{d}"))).unwrap();
            assert_eq!(c.star_count() as usize, n);
        }
    }
}
