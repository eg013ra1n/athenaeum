//! catalog-builder — offline tool to build & publish the Athenaeum star catalogs.
//!
//! Builds the density-limited, additive tier catalogs the plate-solver uses and
//! emits a ready-to-upload `publish/` tree (per-tier zips + sha256 +
//! `manifest.json`) for `artfrom.space/catalogs/`. Design:
//! `docs/superpowers/specs/2026-06-29-tiered-additive-star-catalog-design.md`.
//!
//! Pipeline (each stage idempotent / skippable):
//!   1. Acquire — download the Gaia DR3 bulk dump, or reuse an existing copy.
//!   2. Bin     — HEALPix-6 bin into the intermediate `catalogs/gaia_dr3/*.bin`.
//!   3. Tiers   — slice each cell into 4 disjoint density bands →
//!                `tier_<density>/stars.smac` (500/2000/5000/8000 stars/deg²).
//!   4. Publish — per-tier zip + sha256 + manifest.json into `--out/publish/`.
//!
//! This crate is build/dev tooling only — never shipped in the app or Docker.
//!
//! Quick start (full build):
//!   cargo run -p catalog-builder --release -- \
//!     --gaia-dir /Volumes/NAS/gaia_dr3_bulk --out /Volumes/NAS/catalog_out
//!
//! Reuse an existing G<21 bin set (skip download + re-bin):
//!   cargo run -p catalog-builder --release -- \
//!     --gaia-dir /Volumes/NAS/gaia_dr3_bulk --out /Volumes/NAS/catalog_out \
//!     --work-dir /path/with/catalogs/gaia_dr3 --skip-download

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use athenaeum_core::catalog::gaia_bulk::{
    download_bulk, ingest_bulk, GaiaBulkProgress, DEFAULT_DOWNLOAD_CONCURRENCY,
    DEFAULT_INGEST_CONCURRENCY,
};

mod layers;
mod publish;
mod tiers;

/// Gaia DR3 reference epoch (J2016.0). Stored in the cache header for PM
/// propagation to each frame's observation epoch.
const DEFAULT_EPOCH: f64 = 2016.0;

/// Default ingest magnitude limit (Gaia G). Gaia's faint end, so star-poor sky
/// has enough faint stars to fill the deeper density tiers.
const DEFAULT_MAG_LIMIT: f32 = 21.0;

// ───────────────────────────── Configuration ──────────────────────────────

struct Config {
    gaia_dir: PathBuf,
    work_dir: PathBuf,
    out_dir: PathBuf,
    epoch: f64,
    mag_limit: f32,
    download_concurrency: usize,
    ingest_concurrency: usize,
    skip_download: bool,
    no_zip: bool,
}

fn req_val(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next().ok_or_else(|| anyhow!("missing value for {flag}"))
}

impl Config {
    /// Parse CLI args. Returns `Ok(None)` when `--help` was printed.
    fn from_args() -> Result<Option<Self>> {
        let mut gaia_dir: Option<PathBuf> = None;
        let mut work_dir: Option<PathBuf> = None;
        let mut out_dir: Option<PathBuf> = None;
        let mut epoch = DEFAULT_EPOCH;
        let mut mag_limit = DEFAULT_MAG_LIMIT;
        let mut download_concurrency = DEFAULT_DOWNLOAD_CONCURRENCY;
        let mut ingest_concurrency = DEFAULT_INGEST_CONCURRENCY;
        let mut skip_download = false;
        let mut no_zip = false;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--gaia-dir" => gaia_dir = Some(PathBuf::from(req_val(&mut args, "--gaia-dir")?)),
                "--work-dir" => work_dir = Some(PathBuf::from(req_val(&mut args, "--work-dir")?)),
                "--out" => out_dir = Some(PathBuf::from(req_val(&mut args, "--out")?)),
                "--epoch" => epoch = req_val(&mut args, "--epoch")?.parse().context("--epoch")?,
                "--mag-limit" => {
                    mag_limit = req_val(&mut args, "--mag-limit")?.parse().context("--mag-limit")?
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
        let gaia_dir = gaia_dir.ok_or_else(|| anyhow!("--gaia-dir <dir> is required (try --help)"))?;

        Ok(Some(Config {
            gaia_dir,
            work_dir,
            out_dir,
            epoch,
            mag_limit,
            download_concurrency,
            ingest_concurrency,
            skip_download,
            no_zip,
        }))
    }
}

fn print_help() {
    println!(
        "catalog-builder — build & publish the Athenaeum density-tier star catalogs\n\
\n\
USAGE:\n  \
  catalog-builder --gaia-dir <dir> --out <dir> [options]\n\
\n\
PATHS:\n  \
  --gaia-dir <dir>   Bulk Gaia `.csv.gz` location (download target / existing copy). Required.\n  \
  --work-dir <dir>   Base for the intermediate `catalogs/gaia_dr3/*.bin` (default: --out).\n  \
  --out <dir>        Output dir for tier_<density>/ caches + publish/. Required.\n\
\n\
BUILD:\n  \
  --epoch <year>     Catalog epoch (default {DEFAULT_EPOCH}, Gaia DR3 = J2016.0).\n  \
  --mag-limit <m>    Ingest magnitude cut (default {DEFAULT_MAG_LIMIT}, Gaia faint end).\n\
\n\
STAGES (default: run all):\n  \
  --skip-download    Reuse the Gaia files already in --gaia-dir.\n  \
  --no-zip           Build the tier caches but skip the publish/ packaging.\n\
\n\
TUNING:\n  \
  --download-concurrency <n>  (default {DEFAULT_DOWNLOAD_CONCURRENCY})\n  \
  --ingest-concurrency <n>    (default {DEFAULT_INGEST_CONCURRENCY})\n"
    );
}

// ───────────────────────────────── Stages ──────────────────────────────────

fn acquire_gaia(cfg: &Config, cancel: &Arc<AtomicBool>) -> Result<()> {
    println!("[1/4] Acquire Gaia bulk dump → {}", cfg.gaia_dir.display());
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
    println!("[2/4] Bin → {}/catalogs/gaia_dr3", cfg.work_dir.display());
    let start = Instant::now();
    let total = ingest_bulk(
        &cfg.gaia_dir,
        &cfg.work_dir,
        cfg.mag_limit,
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

// ───────────────────────────────── Helpers ─────────────────────────────────

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

// ────────────────────────────────── main ───────────────────────────────────

fn main() -> Result<()> {
    let cfg = match Config::from_args()? {
        Some(c) => c,
        None => return Ok(()), // --help
    };
    fs::create_dir_all(&cfg.out_dir)
        .with_context(|| format!("create --out {}", cfg.out_dir.display()))?;

    // The underlying download/ingest are resumable, so we don't install a
    // Ctrl-C handler (avoids an extra dep); SIGINT just aborts and you re-run.
    let cancel = Arc::new(AtomicBool::new(false));
    let bins_dir = cfg.work_dir.join("catalogs").join("gaia_dr3");

    if cfg.skip_download {
        println!("[1/4] skip download (--skip-download)");
    } else {
        acquire_gaia(&cfg, &cancel)?;
    }

    if bin_dir_ready(&bins_dir) {
        println!(
            "[2/4] reuse existing bins at {} (delete to re-bin)",
            bins_dir.display()
        );
    } else {
        let n = ingest_gaia(&cfg, &cancel)?;
        println!("  binned {n} stars");
    }

    println!("[3/4] Build density tiers → {}", cfg.out_dir.display());
    if !bins_dir.is_dir() {
        bail!(
            "intermediate bins dir not found: {} — run without --skip-download, or point \
             --work-dir at the dir whose catalogs/gaia_dr3 holds the *.bin tiles",
            bins_dir.display()
        );
    }
    let tiers = layers::build_layers(&bins_dir, &cfg.out_dir, cfg.epoch)?;

    if cfg.no_zip {
        println!("[4/4] skip packaging (--no-zip)");
    } else {
        let pub_dir = publish::package_publish(&cfg.out_dir, &tiers, cfg.epoch)?;
        println!("[4/4] publish tree → {}", pub_dir.display());
        println!("  upload its contents to artfrom.space/catalogs/ :");
        println!(
            "  scp -P 40022 {}/* <user>@artfrom.space:/var/www/artfrom.space/catalogs/",
            pub_dir.display()
        );
    }

    println!("\nDone.");
    Ok(())
}
