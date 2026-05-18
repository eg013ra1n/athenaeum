//! One-time Gaia DR3 (G ≤ 16) catalog ingest.
//!
//! Usage:
//!   cargo run -p athenaeum-core --example ingest_gaia --release
//!   cargo run -p athenaeum-core --example ingest_gaia --release -- <app_data_dir>
//!
//! Pulls all 768 HEALPix-level-3 tiles from the ESA Gaia TAP service into
//! `<app_data_dir>/catalogs/gaia_dr3/`. **Resumable**: a completed tile is
//! recorded in `<app_data_dir>/gaia_dr3_raw/done.manifest`; interrupt with
//! Ctrl-C any time and re-run this command — it skips finished tiles. Runs
//! for a few hours (overnight); ≈4 GB final. The desktop app / bench pick
//! the catalog up automatically once present (no further steps).

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
        GaiaProgress::Querying {
            tile,
            total_tiles,
            stars,
        } => {
            let done = tile + 1;
            let elapsed = start.elapsed().as_secs_f64();
            let eta = if done > 0 {
                elapsed / done as f64 * (total_tiles - done) as f64
            } else {
                0.0
            };
            println!(
                "  tile {done:>3}/{total_tiles}  (+{stars} stars)  elapsed {:.0}m  ETA ~{:.0}m",
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
        "Next: re-run the solver bench — \
         BENCH_SOLVER_BACKEND=astap BENCH_SKIP_ASTAP=1 cargo test --release \
         -p athenaeum-core --test bench_astap_vs_athenaeum -- --ignored --nocapture"
    );
    Ok(())
}
