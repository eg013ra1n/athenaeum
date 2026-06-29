//! catalog-builder — offline tool to build & publish the Athenaeum star catalogs.
//!
//! Produces the two `stars.smac` caches the plate-solver uses and packages them
//! into the `smac_gaia.zip` archive the in-app downloader
//! (`gaia_prebuilt::download_gaia_dr3_prebuilt`) already knows how to fetch and
//! install from artfrom.space.
//!
//! Pipeline (each stage idempotent / skippable):
//!   1. Acquire — download the Gaia DR3 bulk dump, or reuse an existing copy.
//!   2. Bin     — HEALPix-6 bin into the intermediate `catalogs/gaia_dr3/*.bin`.
//!   3. Deep    — build `smac_gaia/stars.smac` (G≤19; registration + verify).
//!   4. Bright  — build `smac_gaia_bright/stars.smac` (hybrid: floor + sparse
//!                top-up + dense cap, so blind solving has enough bright stars
//!                even in sparse fields without bloating dense ones).
//!   5. Package — zip both caches → `smac_gaia.zip` + `.sha256`.
//!   6. Publish — print the scp command to upload to artfrom.space.
//!
//! This crate is build/dev tooling only — it is never shipped in the app or the
//! Docker image (excluded from the workspace there).
//!
//! Quick start (full build):
//!   cargo run -p catalog-builder --release -- \
//!     --gaia-dir /Volumes/NAS/gaia_dr3_bulk --out /Volumes/NAS/catalog_out
//!
//! Iterate on the bright cache only (reuses an existing deep cache under --out):
//!   cargo run -p catalog-builder --release -- \
//!     --out /Volumes/NAS/catalog_out --bright-only --min-per-cell 300

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use athenaeum_core::catalog::gaia_bulk::{
    download_bulk, ingest_bulk, GaiaBulkProgress, DEFAULT_DOWNLOAD_CONCURRENCY,
    DEFAULT_INGEST_CONCURRENCY,
};
use solvemyastro::cache::{build_cache, build_cache_from_legacy_dir, BuildProgress};
use solvemyastro::{StarCache, StarRecord};

mod layers;
mod publish;
mod tiers;

/// HEALPix depth-6 pixel count (12 · 4⁶). The fixed granularity of the
/// `stars.smac` format; solvemyastro keeps its `N_PIXELS` private, so we mirror
/// the constant here.
const N_PIXELS: u64 = 49_152;

/// Gaia DR3 reference epoch (J2016.0). Stored in the cache header for PM
/// propagation to each frame's observation epoch.
const DEFAULT_EPOCH: f64 = 2016.0;

// ── Hybrid bright-catalog defaults (tune against docs/platesolving/README.md) ──
// Keep stars brighter than the floor; if a cell has fewer than `min-per-cell`,
// go fainter to reach it (sparse-field guarantee); never keep more than
// `max-per-cell` (dense-field ceiling, bounds archive size).
const DEFAULT_BRIGHT_FLOOR: f32 = 14.0;
const DEFAULT_MIN_PER_CELL: usize = 200;
const DEFAULT_MAX_PER_CELL: usize = 400;

// ───────────────────────────── Configuration ──────────────────────────────

struct Config {
    gaia_dir: PathBuf,
    work_dir: PathBuf,
    out_dir: PathBuf,
    epoch: f64,
    bright_floor: f32,
    min_per_cell: usize,
    max_per_cell: usize,
    download_concurrency: usize,
    ingest_concurrency: usize,
    skip_download: bool,
    deep_only: bool,
    bright_only: bool,
    no_zip: bool,
}

fn req_val(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("missing value for {flag}"))
}

impl Config {
    /// Parse CLI args. Returns `Ok(None)` when `--help` was printed.
    fn from_args() -> Result<Option<Self>> {
        let mut gaia_dir: Option<PathBuf> = None;
        let mut work_dir: Option<PathBuf> = None;
        let mut out_dir: Option<PathBuf> = None;
        let mut epoch = DEFAULT_EPOCH;
        let mut bright_floor = DEFAULT_BRIGHT_FLOOR;
        let mut min_per_cell = DEFAULT_MIN_PER_CELL;
        let mut max_per_cell = DEFAULT_MAX_PER_CELL;
        let mut download_concurrency = DEFAULT_DOWNLOAD_CONCURRENCY;
        let mut ingest_concurrency = DEFAULT_INGEST_CONCURRENCY;
        let mut skip_download = false;
        let mut deep_only = false;
        let mut bright_only = false;
        let mut no_zip = false;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--gaia-dir" => gaia_dir = Some(PathBuf::from(req_val(&mut args, "--gaia-dir")?)),
                "--work-dir" => work_dir = Some(PathBuf::from(req_val(&mut args, "--work-dir")?)),
                "--out" => out_dir = Some(PathBuf::from(req_val(&mut args, "--out")?)),
                "--epoch" => epoch = req_val(&mut args, "--epoch")?.parse().context("--epoch")?,
                "--bright-floor" => {
                    bright_floor = req_val(&mut args, "--bright-floor")?
                        .parse()
                        .context("--bright-floor")?
                }
                "--min-per-cell" => {
                    min_per_cell = req_val(&mut args, "--min-per-cell")?
                        .parse()
                        .context("--min-per-cell")?
                }
                "--max-per-cell" => {
                    max_per_cell = req_val(&mut args, "--max-per-cell")?
                        .parse()
                        .context("--max-per-cell")?
                }
                "--download-concurrency" => {
                    download_concurrency = req_val(&mut args, "--download-concurrency")?
                        .parse()
                        .context("--download-concurrency")?
                }
                "--ingest-concurrency" => {
                    ingest_concurrency = req_val(&mut args, "--ingest-concurrency")?
                        .parse()
                        .context("--ingest-concurrency")?
                }
                "--skip-download" => skip_download = true,
                "--deep-only" => deep_only = true,
                "--bright-only" => bright_only = true,
                "--no-zip" => no_zip = true,
                "-h" | "--help" => {
                    print_help();
                    return Ok(None);
                }
                other => bail!("unknown argument: {other} (try --help)"),
            }
        }

        let out_dir = out_dir.ok_or_else(|| anyhow!("--out <dir> is required (try --help)"))?;
        let work_dir = work_dir.unwrap_or_else(|| out_dir.clone());
        // gaia-dir is only needed when we download/bin; --bright-only skips that.
        let gaia_dir = match gaia_dir {
            Some(d) => d,
            None if bright_only => out_dir.join("gaia_bulk"), // unused in this mode
            None => bail!("--gaia-dir <dir> is required unless --bright-only"),
        };

        Ok(Some(Config {
            gaia_dir,
            work_dir,
            out_dir,
            epoch,
            bright_floor,
            min_per_cell,
            max_per_cell,
            download_concurrency,
            ingest_concurrency,
            skip_download,
            deep_only,
            bright_only,
            no_zip,
        }))
    }

    fn validate(&self) -> Result<()> {
        if self.min_per_cell > self.max_per_cell {
            bail!(
                "--min-per-cell ({}) must be <= --max-per-cell ({})",
                self.min_per_cell,
                self.max_per_cell
            );
        }
        if self.deep_only && self.bright_only {
            bail!("--deep-only and --bright-only are mutually exclusive");
        }
        Ok(())
    }
}

fn print_help() {
    println!(
        "catalog-builder — build & package the Athenaeum Gaia DR3 star catalogs\n\
\n\
USAGE:\n  \
  catalog-builder --out <dir> [--gaia-dir <dir>] [options]\n\
\n\
PATHS:\n  \
  --gaia-dir <dir>   Bulk Gaia `.csv.gz` location (download target / existing copy).\n  \
                     Required unless --bright-only.\n  \
  --work-dir <dir>   Base for the intermediate `catalogs/gaia_dr3/*.bin` (default: --out).\n  \
  --out <dir>        Output dir for smac_gaia/, smac_gaia_bright/, smac_gaia.zip. Required.\n\
\n\
BUILD:\n  \
  --epoch <year>     Catalog epoch (default {DEFAULT_EPOCH}, Gaia DR3 = J2016.0).\n  \
  --bright-floor <m> Keep all stars brighter than this (default {DEFAULT_BRIGHT_FLOOR}).\n  \
  --min-per-cell <n> Sparse-field floor per HEALPix-6 cell (default {DEFAULT_MIN_PER_CELL}).\n  \
  --max-per-cell <n> Dense-field ceiling per cell (default {DEFAULT_MAX_PER_CELL}).\n\
\n\
STAGES (default: run all):\n  \
  --skip-download    Reuse the Gaia files already in --gaia-dir.\n  \
  --deep-only        Build the deep cache only (no bright sub-catalog).\n  \
  --bright-only      Rebuild the bright cache from an existing deep cache under --out.\n  \
  --no-zip           Skip packaging/publish.\n\
\n\
TUNING:\n  \
  --download-concurrency <n>  (default {DEFAULT_DOWNLOAD_CONCURRENCY})\n  \
  --ingest-concurrency <n>    (default {DEFAULT_INGEST_CONCURRENCY})\n"
    );
}

// ───────────────────────── Hybrid bright selection ─────────────────────────

/// Hybrid per-cell selection for the bright sub-catalog.
///
/// `records` is one HEALPix-6 cell's stars, **mag-sorted ascending** (the order
/// `StarCache::iter_pixel_records` returns). Strategy:
///  - keep all stars brighter than `floor`;
///  - if that prefix is shorter than `min_per_cell`, extend with the next
///    brightest (fainter than the floor) so sparse cells still reach the floor;
///  - never keep more than `max_per_cell`, bounding dense galactic-plane cells.
///
/// Because the input is mag-sorted, the result is simply the brightest
/// `clamp(floor_count, min, max)` stars. Caller guarantees `min <= max`.
fn hybrid_select(
    mut records: Vec<StarRecord>,
    floor: f32,
    min_per_cell: usize,
    max_per_cell: usize,
) -> Vec<StarRecord> {
    // All "brighter than floor" stars form the leading prefix of the sorted cell.
    let floor_count = records.partition_point(|r| r.mag < floor);
    let keep = floor_count
        .clamp(min_per_cell, max_per_cell)
        .min(records.len());
    records.truncate(keep);
    records
}

// ───────────────────────────────── Stages ──────────────────────────────────

fn acquire_gaia(cfg: &Config, cancel: &Arc<AtomicBool>) -> Result<()> {
    println!("[1/6] Acquire Gaia bulk dump → {}", cfg.gaia_dir.display());
    let start = Instant::now();
    let bytes = download_bulk(
        &cfg.gaia_dir,
        cfg.download_concurrency,
        cancel.clone(),
        &|p| match p {
            GaiaBulkProgress::DownloadStarted {
                total_files,
                already_done_files,
                ..
            } => println!(
                "  {already_done_files}/{total_files} already present; fetching {} more",
                total_files - already_done_files
            ),
            GaiaBulkProgress::DownloadFinished {
                completed,
                total_files,
                ..
            } => {
                if completed % 100 == 0 || completed == total_files {
                    println!("  downloaded {completed}/{total_files}");
                }
            }
            GaiaBulkProgress::Error(e) => eprintln!("  download error: {e}"),
            _ => {}
        },
    )
    .context("download Gaia bulk dump")?;
    println!(
        "  done: {:.1} GB in {:.0} min",
        bytes as f64 / 1_073_741_824.0,
        start.elapsed().as_secs_f64() / 60.0
    );
    Ok(())
}

fn ingest_gaia(cfg: &Config, cancel: &Arc<AtomicBool>) -> Result<usize> {
    println!(
        "[2/6] Bin → {}/catalogs/gaia_dr3",
        cfg.work_dir.display()
    );
    let start = Instant::now();
    let total = ingest_bulk(
        &cfg.gaia_dir,
        &cfg.work_dir,
        cfg.ingest_concurrency,
        cancel.clone(),
        &|p| match p {
            GaiaBulkProgress::IngestStarted { total_files } => {
                println!("  {total_files} files to ingest")
            }
            GaiaBulkProgress::IngestProgress {
                completed,
                total_files,
                ..
            } => {
                if completed % 200 == 0 || completed == total_files {
                    println!(
                        "  ingested {completed}/{total_files}  ({:.0} min)",
                        start.elapsed().as_secs_f64() / 60.0
                    );
                }
            }
            GaiaBulkProgress::Finalizing => println!("  finalizing scratch → healpix_*.bin…"),
            GaiaBulkProgress::Complete { total_stars } => {
                println!("  complete — {total_stars} stars")
            }
            GaiaBulkProgress::Error(e) => eprintln!("  ingest error: {e}"),
            _ => {}
        },
    )
    .context("bin Gaia files into HEALPix tiles")?;
    Ok(total)
}

fn build_deep(bins_dir: &Path, deep_dir: &Path, epoch: f64) -> Result<usize> {
    if !bins_dir.is_dir() {
        bail!(
            "intermediate bins dir not found: {} — run without --skip-download, \
             or point --work-dir at the dir whose catalogs/gaia_dr3 holds the *.bin tiles",
            bins_dir.display()
        );
    }
    build_cache_from_legacy_dir(bins_dir, deep_dir, epoch, |p| log_build("deep", p))
        .context("build deep stars.smac")
}

fn build_bright(deep_dir: &Path, bright_dir: &Path, cfg: &Config) -> Result<usize> {
    let deep = StarCache::open(deep_dir)
        .with_context(|| format!("open deep cache at {}", deep_dir.display()))?;
    let epoch = deep.catalog_epoch();
    println!(
        "  source deep cache: {} stars, epoch {epoch}",
        deep.star_count()
    );

    // Collect the hybrid selection across all cells. Bounded by
    // max_per_cell · N_PIXELS, so RAM stays modest (<1 GB) regardless of depth.
    let mut records: Vec<StarRecord> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    for px in 0..N_PIXELS {
        let cell = deep
            .iter_pixel_records(px)
            .with_context(|| format!("read deep pixel {px}"))?;
        if cell.is_empty() {
            continue;
        }
        let kept = hybrid_select(cell, cfg.bright_floor, cfg.min_per_cell, cfg.max_per_cell);
        counts.push(kept.len());
        records.extend(kept);
    }

    counts.sort_unstable();
    let (mn, md, mx) = if counts.is_empty() {
        (0, 0, 0)
    } else {
        (counts[0], counts[counts.len() / 2], counts[counts.len() - 1])
    };
    println!(
        "  per-cell kept: min {mn} / median {md} / max {mx}  over {} non-empty cells \
         (floor G≤{}, min {}, max {})",
        counts.len(),
        cfg.bright_floor,
        cfg.min_per_cell,
        cfg.max_per_cell
    );

    build_cache(records, bright_dir, epoch, |p| log_build("bright", p))
        .context("build bright stars.smac")
}

fn package(cfg: &Config, deep_dir: &Path, bright_dir: &Path) -> Result<()> {
    let zip_path = cfg.out_dir.join("smac_gaia.zip");
    let sha_path = cfg.out_dir.join("smac_gaia.zip.sha256");
    println!("[5/6] Package → {}", zip_path.display());

    let deep_smac = deep_dir.join("stars.smac");
    if !deep_smac.is_file() {
        bail!(
            "deep cache missing: {} — the archive requires it (the in-app downloader \
             bails without a deep stars.smac)",
            deep_smac.display()
        );
    }
    let bright_smac = bright_dir.join("stars.smac");

    {
        let zf = BufWriter::new(File::create(&zip_path).with_context(|| {
            format!("create archive {}", zip_path.display())
        })?);
        let mut zw = ZipWriter::new(zf);
        // Stored, not Deflated: stars.smac is dense binary (f64 RA/Dec dominate),
        // so deflate buys ~nothing yet costs hours on ~15 GB. `large_file` is
        // required for the >4 GB deep entry. Layout matches gaia_prebuilt::extract_zip.
        let opts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(true);
        add_file(&mut zw, &deep_smac, "smac_gaia/stars.smac", opts)?;
        if bright_smac.is_file() {
            add_file(&mut zw, &bright_smac, "smac_gaia_bright/stars.smac", opts)?;
        } else {
            println!(
                "  note: no bright cache at {} — producing a deep-only archive",
                bright_smac.display()
            );
        }
        zw.finish().context("finalize zip")?;
    }

    // Checksum sidecar: first whitespace token must be the 64-hex digest the
    // in-app downloader (gaia_prebuilt) parses.
    let digest = sha256_file(&zip_path)?;
    fs::write(&sha_path, format!("{digest}  smac_gaia.zip\n"))
        .with_context(|| format!("write {}", sha_path.display()))?;

    let size_gb = fs::metadata(&zip_path)?.len() as f64 / 1_073_741_824.0;
    println!("  archive : {} ({size_gb:.2} GB)", zip_path.display());
    println!("  sha256  : {digest}");

    println!("\n[6/6] Publish — upload BOTH files so they resolve at");
    println!("  https://artfrom.space/catalogs/smac_gaia.zip  (+ .sha256)");
    println!(
        "\n  scp -P 40022 '{}' '{}' \\\n    <user>@artfrom.space:/var/www/artfrom.space/catalogs/",
        zip_path.display(),
        sha_path.display()
    );
    Ok(())
}

// ───────────────────────────────── Helpers ─────────────────────────────────

fn log_build(label: &str, p: BuildProgress) {
    match p {
        BuildProgress::Ingesting { records } => {
            // Source throttles to ~1M-record intervals; thin further to ~50M.
            if records % 50_000_000 == 0 {
                println!("    {label}: ingested {records} records");
            }
        }
        BuildProgress::Finalizing {
            shards_done,
            shards_total,
        } => {
            if shards_done % 8192 == 0 || shards_done == shards_total {
                println!("    {label}: finalizing {shards_done}/{shards_total} shards");
            }
        }
        BuildProgress::Complete { records } => {
            println!("    {label}: complete — {records} records written");
        }
    }
}

fn add_file<W: Write + Seek>(
    zw: &mut ZipWriter<W>,
    path: &Path,
    name: &str,
    opts: SimpleFileOptions,
) -> Result<()> {
    println!("  + {name}  ({})", path.display());
    zw.start_file(name, opts)
        .with_context(|| format!("zip start_file {name}"))?;
    let mut f = BufReader::new(
        File::open(path).with_context(|| format!("open {}", path.display()))?,
    );
    io::copy(&mut f, zw).with_context(|| format!("zip write {name}"))?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut f = BufReader::new(File::open(path)?);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// True when the intermediate bins dir already holds a full binning
/// (>100 `healpix_*.bin`) so we can skip the ingest stage.
fn bin_dir_ready(bins_dir: &Path) -> bool {
    fs::read_dir(bins_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("healpix_") && n.ends_with(".bin"))
                })
                .count()
                > 100
        })
        .unwrap_or(false)
}

/// True when a cache dir already holds a non-trivial `stars.smac` (header +
/// directory is ~590 KB, so anything past that is a real build).
fn smac_ready(cache_dir: &Path) -> bool {
    fs::metadata(cache_dir.join("stars.smac"))
        .map(|m| m.len() > 600_000)
        .unwrap_or(false)
}

// ────────────────────────────────── main ───────────────────────────────────

fn main() -> Result<()> {
    let cfg = match Config::from_args()? {
        Some(c) => c,
        None => return Ok(()), // --help
    };
    cfg.validate()?;
    fs::create_dir_all(&cfg.out_dir)
        .with_context(|| format!("create --out {}", cfg.out_dir.display()))?;

    // The underlying download/ingest are resumable, so we don't install a
    // Ctrl-C handler (avoids an extra dep); SIGINT just aborts and you re-run.
    let cancel = Arc::new(AtomicBool::new(false));

    let deep_dir = cfg.out_dir.join("smac_gaia");
    let bright_dir = cfg.out_dir.join("smac_gaia_bright");
    let bins_dir = cfg.work_dir.join("catalogs").join("gaia_dr3");

    if cfg.bright_only {
        println!(
            "[1-3/6] skip (--bright-only) — reusing deep cache at {}",
            deep_dir.display()
        );
    } else {
        if cfg.skip_download {
            println!("[1/6] skip download (--skip-download)");
        } else {
            acquire_gaia(&cfg, &cancel)?;
        }

        if bin_dir_ready(&bins_dir) {
            println!(
                "[2/6] reuse existing bins at {} (delete to re-bin)",
                bins_dir.display()
            );
        } else {
            let n = ingest_gaia(&cfg, &cancel)?;
            println!("  binned {n} stars");
        }

        if smac_ready(&deep_dir) {
            println!(
                "[3/6] reuse existing deep cache at {} (delete to rebuild)",
                deep_dir.display()
            );
        } else {
            println!("[3/6] Build deep cache → {}", deep_dir.display());
            let n = build_deep(&bins_dir, &deep_dir, cfg.epoch)?;
            println!("  deep: {n} stars");
        }
    }

    if cfg.deep_only {
        println!("[4/6] skip bright (--deep-only)");
    } else {
        println!("[4/6] Build hybrid bright cache → {}", bright_dir.display());
        let n = build_bright(&deep_dir, &bright_dir, &cfg)?;
        println!("  bright: {n} stars");
    }

    if cfg.no_zip {
        println!("[5-6/6] skip packaging (--no-zip)");
    } else {
        package(&cfg, &deep_dir, &bright_dir)?;
    }

    println!("\nDone.");
    Ok(())
}

// ────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(mag: f32) -> StarRecord {
        StarRecord {
            ra: 10.0,
            dec: 20.0,
            mag,
            pmra_mas_yr: 0.0,
            pmdec_mas_yr: 0.0,
        }
    }

    /// Build a mag-sorted-ascending cell (the invariant iter_pixel_records gives).
    fn cell(mags: &[f32]) -> Vec<StarRecord> {
        let mut v: Vec<StarRecord> = mags.iter().map(|&m| rec(m)).collect();
        v.sort_by(|a, b| a.mag.partial_cmp(&b.mag).unwrap());
        v
    }

    #[test]
    fn sparse_cell_keeps_all_when_below_min() {
        // Only 3 stars exist; min wants 10 → top-up exhausts the cell, keep 3.
        let out = hybrid_select(cell(&[8.0, 10.0, 12.0]), 14.0, 10, 100);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn sparse_cell_tops_up_past_floor() {
        // 2 below floor(14); min=5 → go fainter to reach 5, keep the 5 brightest.
        let out = hybrid_select(cell(&[10.0, 13.0, 15.0, 16.0, 17.0, 18.0]), 14.0, 5, 100);
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(|r| r.mag <= 17.0));
    }

    #[test]
    fn dense_cell_capped_at_max() {
        // 10 stars below floor; max=4 → cap at the 4 brightest.
        let mags: Vec<f32> = (0..10).map(|i| 8.0 + i as f32 * 0.2).collect();
        let out = hybrid_select(cell(&mags), 14.0, 2, 4);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|r| r.mag <= 8.6 + 1e-6));
    }

    #[test]
    fn mid_cell_keeps_floor_count() {
        // 6 below floor, between min(2) and max(100) → keep exactly those 6.
        let out = hybrid_select(
            cell(&[8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 15.0, 16.0]),
            14.0,
            2,
            100,
        );
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|r| r.mag < 14.0));
    }

    #[test]
    fn empty_cell_stays_empty() {
        assert_eq!(hybrid_select(Vec::new(), 14.0, 10, 100).len(), 0);
    }
}
