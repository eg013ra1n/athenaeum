//! One-time Gaia DR3 (G < 19) catalog ingest via the ESA TAP service.
//!
//! NOTE: prefer the `catalog-builder` crate, which orchestrates the full
//! download → bin → deep+bright `stars.smac` → zip pipeline. This example is
//! just the TAP-only binning step (the slower alternative to the bulk-CDN path).
//!
//! Usage:
//!   cargo run -p athenaeum-core --example ingest_gaia --release
//!   cargo run -p athenaeum-core --example ingest_gaia --release -- <app_data_dir>
//!
//! Pulls all 12,288 HEALPix-level-5 tiles from the ESA Gaia TAP service into
//! `<app_data_dir>/catalogs/gaia_dr3/` (the intermediate `healpix_*.bin`).
//! **Resumable**: a completed tile is recorded in
//! `<app_data_dir>/gaia_dr3_raw/done.manifest`; interrupt with Ctrl-C any time
//! and re-run — it skips finished tiles. The TAP queue can take a long while.
//! Afterwards, build `stars.smac` from these tiles with `catalog-builder
//! --skip-download` (or `solvemyastro build-cache`); the app reads `stars.smac`,
//! not the `.bin` tiles.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use athenaeum_core::catalog::gaia::{setup_gaia_dr3_catalog, GaiaProgress, GAIA_TILE_COUNT};

fn main() -> anyhow::Result<()> {
    let app_data_dir = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            let home = std::env::var("HOME")?;
            PathBuf::from(home).join("Library/Application Support/com.vsharifov.athenaeum")
        }
    };

    println!("Gaia DR3 (G≤16) ingest");
    println!("  app data : {}", app_data_dir.display());
    println!(
        "  target   : {}/catalogs/gaia_dr3",
        app_data_dir.display()
    );
    println!(
        "  {} TAP tiles · resumable (Ctrl-C safe, re-run to continue) · a few hours · ~4 GB\n",
        GAIA_TILE_COUNT
    );

    let cancel = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    let result = setup_gaia_dr3_catalog(&app_data_dir, cancel, &|p| match p {
        GaiaProgress::Started {
            total_tiles,
            already_done,
        } => println!(
            "  started: {already_done}/{total_tiles} tiles already done (resumed); contacting ESA…"
        ),
        GaiaProgress::Querying {
            tile,
            completed,
            total_tiles,
            stars,
        } => {
            let elapsed = start.elapsed().as_secs_f64();
            let eta = if completed > 0 {
                elapsed / completed as f64 * (total_tiles - completed) as f64
            } else {
                0.0
            };
            println!(
                "  tile {tile} done · {completed:>3}/{total_tiles}  (+{stars} stars)  elapsed {:.0}m  ETA ~{:.0}m",
                elapsed / 60.0,
                eta / 60.0
            );
        }
        GaiaProgress::Converting {
            stars_processed,
            total_stars,
        } => println!("  converting… ({stars_processed}/{total_stars})"),
        GaiaProgress::Complete { total_stars } => {
            println!("\n  done: {total_stars} stars in {:.0} min", start.elapsed().as_secs_f64() / 60.0)
        }
        GaiaProgress::Error(e) => eprintln!("  error: {e}"),
    })?;

    println!("\nGaia DR3 catalog installed at: {}", result.display());
    println!(
        "Next: run the solver corpus bench — \
         SOLVEMYASTRO_TIER_DIR=<dir with tier_*> cargo test --release \
         -p solvemyastro --test corpus_bench corpus_layered_tiers -- --ignored --nocapture"
    );
    Ok(())
}
