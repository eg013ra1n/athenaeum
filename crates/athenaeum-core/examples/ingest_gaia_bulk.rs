//! Ingest a previously-downloaded Gaia DR3 bulk dump into the local
//! HEALPix-6 catalog used by the plate solver.
//!
//! Usage:
//!   cargo run -p athenaeum-core --example ingest_gaia_bulk --release -- <bulk_dir> [app_data_dir] [concurrency]
//!
//! - `<bulk_dir>`: where the `.csv.gz` files live (NAS mount or local).
//!   Same path you gave to `download_gaia_bulk`.
//! - `[app_data_dir]`: Athenaeum application support dir; defaults to the
//!   standard macOS location. Output goes to
//!   `<app_data_dir>/catalogs/gaia_dr3/`; scratch lives in
//!   `<app_data_dir>/gaia_dr3_bulk_scratch/bins/`.
//! - `[concurrency]`: parallel CSV ingest workers. Default 4. Each worker
//!   gunzips + parses one `.csv.gz` end-to-end.
//!
//! The ingest is single-shot — it does not maintain a manifest of done
//! files. To re-bin after a parameter change (RUWE threshold, mag cap),
//! delete `<app_data_dir>/catalogs/gaia_dr3/` and re-run. The source
//! `.csv.gz` files on NAS are never modified; zero network traffic.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use athenaeum_core::catalog::gaia_bulk::{
    ingest_bulk, GaiaBulkProgress, DEFAULT_INGEST_CONCURRENCY,
};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let bulk_dir = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!(
            "usage: ingest_gaia_bulk <bulk_dir> [app_data_dir] [concurrency]"
        ))?;
    let app_data_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_app_data_dir);
    let concurrency: usize = args
        .next()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(DEFAULT_INGEST_CONCURRENCY);

    println!("Gaia DR3 bulk-dump ingest");
    println!("  source       : {}", bulk_dir.display());
    println!("  catalog out  : {}/catalogs/gaia_dr3", app_data_dir.display());
    println!("  concurrency  : {concurrency}\n");

    let cancel = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    let total = ingest_bulk(&bulk_dir, &app_data_dir, concurrency, cancel, &|p| match p {
        GaiaBulkProgress::IngestStarted { total_files } => {
            println!("started: {total_files} files to ingest")
        }
        GaiaBulkProgress::IngestProgress {
            name,
            completed,
            total_files,
            stars_kept,
        } => {
            let mins = start.elapsed().as_secs_f64() / 60.0;
            let eta = if completed > 0 {
                mins / completed as f64 * (total_files - completed) as f64
            } else {
                0.0
            };
            println!(
                "  done {completed:>5}/{total_files}  {name}  (+{stars_kept} kept)  elapsed {mins:.0}m  ETA ~{eta:.0}m"
            );
        }
        GaiaBulkProgress::Finalizing => println!("\nfinalizing scratch → healpix_*.bin…"),
        GaiaBulkProgress::Complete { total_stars } => {
            println!("\ncomplete — {total_stars} stars in {:.0} min", start.elapsed().as_secs_f64() / 60.0)
        }
        GaiaBulkProgress::Error(e) => eprintln!("  error: {e}"),
        _ => {}
    })?;

    println!("\nCatalog installed: {} stars at {}/catalogs/gaia_dr3",
        total, app_data_dir.display());
    println!(
        "Next: build stars.smac with `solvemyastro build-cache --from {}/catalogs/gaia_dr3 \\\n  --out {}/catalogs/smac_gaia --epoch 2016.0`",
        app_data_dir.display(), app_data_dir.display()
    );
    Ok(())
}

fn default_app_data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join("Library/Application Support/com.vsharifov.athenaeum")
    } else {
        PathBuf::from(".")
    }
}
