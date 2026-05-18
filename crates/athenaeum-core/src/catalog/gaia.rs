//! Gaia DR3 (G ≤ 16) all-sky catalog ingest.
//!
//! Mirrors the [`super::tycho2`] pipeline, but Gaia is far too large to bulk
//! download (the `gaia_source` repo is HEALPix-partitioned, *not*
//! magnitude-partitioned — a G≤16 subset would still mean ~600 GB of
//! transfer). Instead we extract via the ESA TAP **async** service, tiled by
//! `source_id` range so the server filters `phot_g_mean_mag < 16` and
//! projects only the 5 columns we store. Each tile is an independent,
//! resumable checkpoint.
//!
//! `source_id` encodes position: the HEALPix level-*n* index is
//! `source_id / (2^35 · 4^(12−n))`. We tile at **level 3** (nested):
//! `12 · 4^3 = 768` pixels, divisor `2^35 · 4^9 = 2^53`. At G≤16 that is
//! ≈390 k rows/tile — comfortably under the 3 M-row anonymous async cap.
//!
//! Output goes to `catalogs/gaia_dr3/` in the existing depth-6 HEALPix
//! [`StarRecord`] format; [`crate::catalog::CatalogEngine::with_catalog_dir`]
//! auto-discovers it and `cone_search` prefers it (epoch 2016.0) over
//! Tycho-2 with no solver changes.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use super::binary_format::{self, StarRecord};
use super::healpix;

/// ESA Gaia archive TAP async endpoint.
pub const GAIA_TAP_ASYNC: &str = "https://gea.esac.esa.int/tap-server/tap/async";

/// Magnitude cut (Gaia G, Vega) applied server-side.
pub const GAIA_MAG_LIMIT: f32 = 16.0;

/// HEALPix level used to tile the `source_id` space for TAP extraction.
pub const GAIA_HEALPIX_LEVEL: u32 = 3;

/// Number of nested HEALPix level-3 pixels: `12 · 4^3`.
pub const GAIA_TILE_COUNT: u64 = 768;

/// `source_id` span per level-3 tile: `2^35 · 4^(12−3)` = `2^53`.
pub const SOURCE_ID_TILE_SPAN: u64 = 1 << 53;

/// Progress callback for the query and conversion phases (mirrors
/// [`super::tycho2::Tycho2Progress`]).
pub enum GaiaProgress {
    /// One TAP tile finished: `tile` of `total_tiles`, `stars` parsed from it.
    Querying {
        tile: u64,
        total_tiles: u64,
        stars: usize,
    },
    /// Finalizing the on-disk HEALPix-6 catalog.
    Converting {
        stars_processed: usize,
        total_stars: usize,
    },
    Complete {
        total_stars: usize,
    },
    Error(String),
}

/// Inclusive `source_id` range `[lo, hi]` covered by level-3 `tile`
/// (`0..GAIA_TILE_COUNT`). Tiles tile the `source_id` space contiguously.
pub fn tile_source_id_range(tile: u64) -> (u64, u64) {
    let lo = tile * SOURCE_ID_TILE_SPAN;
    let hi = (tile + 1) * SOURCE_ID_TILE_SPAN - 1;
    (lo, hi)
}

/// Build the ADQL for one level-3 tile: server-side magnitude cut + the
/// 5 columns we store, constrained to the tile's `source_id` range (which
/// is the spatial partition — `source_id BETWEEN` is primary-key indexed,
/// so this is the fast form).
pub fn tile_adql(tile: u64) -> String {
    let (lo, hi) = tile_source_id_range(tile);
    format!(
        "SELECT ra,dec,phot_g_mean_mag,pmra,pmdec FROM gaiadr3.gaia_source \
         WHERE phot_g_mean_mag < {} AND source_id BETWEEN {lo} AND {hi}",
        GAIA_MAG_LIMIT as u32
    )
}

/// Poll cadence and a generous safety cap so a stuck job can never spin
/// forever (ESA anon async job timeout is 90 min).
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_POLLS: u32 = 90 * 60 / 5;

/// A blocking HTTP client configured for ESA TAP (long timeout, polite UA).
pub fn tap_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .user_agent("athenaeum-catalog-ingest (astrophotography catalog builder)")
        .build()
        .context("build TAP HTTP client")
}

/// Submit an async ADQL job (`PHASE=RUN`) and return the job resource URL.
/// ESA responds 303 → the job resource; reqwest follows it, so the final
/// response URL is the job URL.
pub fn submit_tap_job(client: &reqwest::blocking::Client, adql: &str) -> Result<String> {
    let resp = client
        .post(GAIA_TAP_ASYNC)
        .form(&[
            ("REQUEST", "doQuery"),
            ("LANG", "ADQL"),
            ("FORMAT", "csv"),
            ("PHASE", "RUN"),
            ("QUERY", adql),
        ])
        .send()
        .context("submit TAP job")?;
    if !resp.status().is_success() {
        anyhow::bail!("TAP submit returned HTTP {}", resp.status());
    }
    let job_url = resp.url().as_str().trim_end_matches('/').to_string();
    if !job_url.contains("/async/") {
        anyhow::bail!("unexpected TAP job URL: {job_url}");
    }
    Ok(job_url)
}

/// Poll `{job}/phase` until terminal. Ok(()) on `COMPLETED`; Err on
/// `ERROR`/`ABORTED`, on the safety cap, or on cancel.
pub fn poll_job(
    client: &reqwest::blocking::Client,
    job_url: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    for _ in 0..MAX_POLLS {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let phase = client
            .get(format!("{job_url}/phase"))
            .send()
            .context("poll TAP phase")?
            .text()
            .context("read TAP phase")?;
        match phase.trim() {
            "COMPLETED" => return Ok(()),
            "ERROR" | "ABORTED" => {
                anyhow::bail!("TAP job {job_url} ended in phase {}", phase.trim())
            }
            _ => std::thread::sleep(POLL_INTERVAL),
        }
    }
    anyhow::bail!("TAP job {job_url} did not complete within the poll cap")
}

/// Fetch the completed job's CSV result body.
pub fn fetch_job_csv(client: &reqwest::blocking::Client, job_url: &str) -> Result<String> {
    let resp = client
        .get(format!("{job_url}/results/result"))
        .send()
        .context("fetch TAP result")?;
    if !resp.status().is_success() {
        anyhow::bail!("TAP result returned HTTP {}", resp.status());
    }
    resp.text().context("read TAP result body")
}

/// Parse one TAP CSV line (`ra,dec,phot_g_mean_mag,pmra,pmdec`) into a
/// [`StarRecord`]. Returns `None` for the header and malformed/short rows.
///
/// Gaia 2-parameter astrometric solutions have empty `pmra`/`pmdec`; those
/// stars are still useful for matching, so a missing PM is treated as 0.
/// `ra`/`dec`/`phot_g_mean_mag` must be present (the server-side
/// `phot_g_mean_mag < 16` filter already excludes null-G rows). `pmra` is
/// Gaia μα\* (cos δ included) — stored as-is; this matches the Tycho-2 path
/// and `cone_search`'s proper-motion expectation (asserted in Task 7).
pub fn parse_gaia_csv_row(row: &str) -> Option<StarRecord> {
    let mut f = row.split(',');
    let ra: f64 = f.next()?.trim().parse().ok()?;
    let dec: f64 = f.next()?.trim().parse().ok()?;
    let g: f32 = f.next()?.trim().parse().ok()?;
    let pmra: f64 = match f.next()?.trim() {
        "" => 0.0,
        s => s.parse().ok()?,
    };
    let pmdec: f64 = match f.next()?.trim() {
        "" => 0.0,
        s => s.parse().ok()?,
    };
    Some(StarRecord::from_values(ra as f32, dec as f32, g, pmra, pmdec))
}

/// RAM-safe streaming binner: the full G≤16 catalog (~300 M records ≈
/// 4.2 GB) cannot be held in memory like Tycho-2's 2.5 M, so we never
/// collect it. Each TAP tile (~390 k records ≈ 5.5 MB — bounded) is grouped
/// by depth-6 HEALPix pixel and **appended** to per-pixel scratch files
/// (`scratch/p_NNNNNN.raw`). [`HealpixBinner::finalize`] then makes one pass
/// per pixel: read it back, and write the final `healpix_NNNNNN.bin` via the
/// shared [`binary_format::write_records`] (same pixel assignment + same
/// mag-sort + same serialization as [`super::write_catalog_to_healpix`], so
/// the output is byte-identical — asserted in tests).
pub struct HealpixBinner {
    scratch_dir: PathBuf,
}

impl HealpixBinner {
    pub fn open(scratch_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(scratch_dir)
            .with_context(|| format!("create scratch dir {}", scratch_dir.display()))?;
        Ok(Self {
            scratch_dir: scratch_dir.to_path_buf(),
        })
    }

    /// Append one tile's records to their per-pixel scratch files. Only this
    /// tile is held in memory; scratch files are opened/closed per pixel
    /// (~64 depth-6 pixels per level-3 tile → cheap, no fd exhaustion).
    pub fn push_tile(&self, records: &[StarRecord]) -> Result<()> {
        let mut by_pixel: HashMap<u64, Vec<&StarRecord>> = HashMap::new();
        for r in records {
            let pixel = healpix::sky_to_pixel(r.ra as f64, r.dec as f64);
            by_pixel.entry(pixel).or_default().push(r);
        }
        for (pixel, recs) in by_pixel {
            let path = self.scratch_dir.join(format!("p_{:06}.raw", pixel));
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("append scratch {}", path.display()))?;
            let mut w = BufWriter::new(f);
            for r in recs {
                r.write_to(&mut w)?;
            }
            w.flush()?;
        }
        Ok(())
    }

    /// Read every pixel scratch file, mag-sort + serialize it into
    /// `out_dir/healpix_NNNNNN.bin` (via the shared writer), delete the
    /// scratch, and return the total record count.
    pub fn finalize(self, out_dir: &Path) -> Result<usize> {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("create catalog dir {}", out_dir.display()))?;
        let mut total = 0usize;
        for entry in std::fs::read_dir(&self.scratch_dir)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(pixel_str) = name.strip_prefix("p_").and_then(|s| s.strip_suffix(".raw"))
            else {
                continue;
            };
            let pixel: u64 = pixel_str.parse().context("parse scratch pixel id")?;

            let mut records: Vec<StarRecord> = Vec::new();
            let f = std::fs::File::open(&path)?;
            let mut r = BufReader::new(f);
            while let Ok(rec) = StarRecord::read_from(&mut r) {
                records.push(rec);
            }
            total += records.len();

            let out = out_dir.join(format!("healpix_{:06}.bin", pixel));
            let mut w = BufWriter::new(std::fs::File::create(&out)?);
            binary_format::write_records(&mut w, &mut records)?;
            w.flush()?;
            std::fs::remove_file(&path)?;
        }
        Ok(total)
    }
}

/// Placeholder so the module is usable before later tasks land; real
/// pipeline is `download_gaia_dr3` / `setup_gaia_dr3_catalog` (Tasks 4–5).
pub fn setup_gaia_dr3_catalog(
    _app_data_dir: &Path,
    _cancel_flag: Arc<AtomicBool>,
    _progress: &dyn Fn(GaiaProgress),
) -> Result<PathBuf> {
    anyhow::bail!("gaia_dr3 ingest not yet implemented (plan Tasks 2–5)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_range_partitions_source_id_space() {
        // Tile 0 starts at 0.
        assert_eq!(tile_source_id_range(0).0, 0);
        // Span is exactly 2^53.
        let (lo0, hi0) = tile_source_id_range(0);
        assert_eq!(hi0 - lo0 + 1, SOURCE_ID_TILE_SPAN);
        assert_eq!(SOURCE_ID_TILE_SPAN, 9_007_199_254_740_992);

        // Contiguous + non-overlapping across all 768 tiles.
        for t in 0..GAIA_TILE_COUNT - 1 {
            let (_, hi) = tile_source_id_range(t);
            let (next_lo, _) = tile_source_id_range(t + 1);
            assert_eq!(hi + 1, next_lo, "gap/overlap between tile {t} and {}", t + 1);
        }

        // 768 tiles cover [0, 768·2^53).
        let (last_lo, last_hi) = tile_source_id_range(GAIA_TILE_COUNT - 1);
        assert_eq!(last_lo, (GAIA_TILE_COUNT - 1) * SOURCE_ID_TILE_SPAN);
        assert_eq!(last_hi + 1, GAIA_TILE_COUNT * SOURCE_ID_TILE_SPAN);
        // …and stay within the u64 source_id space (768·2^53 ≈ 6.9e18 < 2^63).
        assert!(GAIA_TILE_COUNT * SOURCE_ID_TILE_SPAN < (1u64 << 63));
    }

    #[test]
    fn adql_is_well_formed() {
        let q0 = tile_adql(0);
        assert!(q0.contains("FROM gaiadr3.gaia_source"));
        assert!(q0.contains("phot_g_mean_mag < 16"));
        assert!(
            q0.contains("source_id BETWEEN 0 AND 9007199254740991"),
            "tile 0 range wrong: {q0}"
        );
        // Tile 1's lower bound is exactly 2^53.
        let q1 = tile_adql(1);
        assert!(
            q1.contains("BETWEEN 9007199254740992 AND "),
            "tile 1 lower bound wrong: {q1}"
        );
        // Only the 5 stored columns are projected.
        assert!(q0.starts_with("SELECT ra,dec,phot_g_mean_mag,pmra,pmdec FROM"));
    }

    #[test]
    fn parse_row_and_header() {
        // Header → None.
        assert!(parse_gaia_csv_row("ra,dec,phot_g_mean_mag,pmra,pmdec").is_none());
        // Full row.
        let s = parse_gaia_csv_row("86.682,0.042,14.231,12.5,-7.25").expect("row");
        assert!((s.ra - 86.682).abs() < 1e-3);
        assert!((s.dec - 0.042).abs() < 1e-3);
        assert!((s.mag() - 14.231).abs() < 1e-3);
        assert!((s.pmra_mas_yr() - 12.5).abs() < 0.01);
        assert!((s.pmdec_mas_yr() - (-7.25)).abs() < 0.01);
        // Null PM (2-parameter solution) → PM 0, still Some.
        let s2 = parse_gaia_csv_row("10.0,20.0,15.9,,").expect("null-pm row");
        assert_eq!(s2.pmra_mas_yr(), 0.0);
        assert_eq!(s2.pmdec_mas_yr(), 0.0);
        assert!((s2.mag() - 15.9).abs() < 1e-3);
        // Short/garbage rows → None.
        assert!(parse_gaia_csv_row("1.0,2.0").is_none());
        assert!(parse_gaia_csv_row("").is_none());
    }

    #[test]
    fn binner_roundtrips_and_sorts_byte_identical_to_write_catalog() {
        use crate::catalog::write_catalog_to_healpix;
        use tempfile::TempDir;

        // ~10 stars spanning several depth-6 pixels, magnitudes deliberately
        // out of order so the per-pixel mag sort is exercised.
        let recs: Vec<StarRecord> = vec![
            StarRecord::from_values(10.0, 10.0, 12.5, 1.0, -2.0),
            StarRecord::from_values(10.05, 10.02, 8.1, 0.0, 0.0),
            StarRecord::from_values(10.02, 9.98, 15.9, -3.0, 4.0),
            StarRecord::from_values(200.0, -40.0, 9.7, 5.0, 5.0),
            StarRecord::from_values(200.1, -40.1, 6.3, 0.0, 0.0),
            StarRecord::from_values(200.2, -39.9, 14.2, 2.0, -1.0),
            StarRecord::from_values(300.0, 70.0, 11.0, 0.0, 0.0),
            StarRecord::from_values(300.3, 70.2, 7.4, -1.0, 1.0),
            StarRecord::from_values(45.0, -5.0, 13.3, 0.0, 0.0),
            StarRecord::from_values(45.0, -5.0, 10.0, 0.0, 0.0),
        ];

        let scratch = TempDir::new().unwrap();
        let out_binner = TempDir::new().unwrap();
        let binner = HealpixBinner::open(scratch.path()).unwrap();
        // Two push_tile calls → exercises cross-tile append into the same
        // pixel scratch file.
        binner.push_tile(&recs[..5]).unwrap();
        binner.push_tile(&recs[5..]).unwrap();
        let total = binner.finalize(out_binner.path()).unwrap();
        assert_eq!(total, recs.len());

        let out_ref = TempDir::new().unwrap();
        write_catalog_to_healpix(&recs, out_ref.path()).unwrap();

        // Same set of pixel files, each byte-identical.
        let names = |d: &Path| {
            let mut v: Vec<String> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let bn = names(out_binner.path());
        let rn = names(out_ref.path());
        assert!(!bn.is_empty() && bn.len() >= 3, "expected multiple pixels: {bn:?}");
        assert_eq!(bn, rn, "pixel file sets differ");
        for n in &bn {
            let a = std::fs::read(out_binner.path().join(n)).unwrap();
            let b = std::fs::read(out_ref.path().join(n)).unwrap();
            assert_eq!(a, b, "pixel file {n} not byte-identical to write_catalog_to_healpix");
        }
        // Scratch fully consumed.
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn level3_has_768_pixels() {
        // Nested HEALPix: 12 · 4^level.
        assert_eq!(12 * 4u64.pow(GAIA_HEALPIX_LEVEL), GAIA_TILE_COUNT);
    }
}
