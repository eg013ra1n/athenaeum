//! Build the 4 density-limited tier caches by slicing the G<21 bins.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use athenaeum_core::catalog::binary_format;
use solvemyastro::cache::{build_cache, BuildProgress};
use solvemyastro::StarRecord;

use crate::tiers::{cell_cum_counts, slice_select, TIER_DENSITIES};

/// Enumerate `healpix_*.bin` tiles in `bins_dir`, sorted for determinism.
fn bin_paths(bins_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(bins_dir)
        .with_context(|| format!("read bins dir {}", bins_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("healpix_") && n.ends_with(".bin"))
        })
        .collect();
    paths.sort();
    Ok(paths)
}

fn to_smac(r: &binary_format::StarRecord) -> StarRecord {
    StarRecord {
        ra: r.ra as f64,
        dec: r.dec as f64,
        mag: r.mag(),
        pmra_mas_yr: r.pmra_mas_yr(),
        pmdec_mas_yr: r.pmdec_mas_yr(),
    }
}

/// Build `out_dir/tier_<density>/stars.smac` for each tier. One pass over the
/// bins per tier (bounded RAM); each pass slices that tier's rank band per cell.
/// Returns `(density, star_count)` per tier in `TIER_DENSITIES` order.
pub fn build_layers(bins_dir: &Path, out_dir: &Path, epoch: f64) -> Result<Vec<(u32, usize)>> {
    let bins = bin_paths(bins_dir)?;
    let bounds = cell_cum_counts();
    let mut out = Vec::with_capacity(TIER_DENSITIES.len());

    for (k, density) in TIER_DENSITIES.iter().enumerate() {
        let (lo, hi) = (bounds[k], bounds[k + 1]);
        let tier_dir = out_dir.join(format!("tier_{density}"));

        // Lazily stream each cell's [lo,hi) band into build_cache.
        let records = bins.iter().flat_map(|p| {
            let data = match std::fs::read(p) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("warn: read {} failed: {e} — skipping", p.display());
                    Vec::new()
                }
            };
            let cell = binary_format::read_records_until_mag(&data, f32::MAX);
            slice_select(&cell, lo, hi).iter().map(to_smac).collect::<Vec<_>>()
        });

        let n = build_cache(records, &tier_dir, epoch, |p| match p {
            BuildProgress::Ingesting { records } => {
                if records % 20_000_000 == 0 {
                    println!("    tier_{density}: ingested {records} records…");
                }
            }
            BuildProgress::Finalizing { shards_done, shards_total } => {
                if shards_done == shards_total {
                    println!("    tier_{density}: finalized {shards_total} shards");
                }
            }
            BuildProgress::Complete { .. } => {}
        })
        .with_context(|| format!("build tier_{density}"))?;
        println!("  tier_{density}: {n} stars");
        out.push((*density, n));
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
