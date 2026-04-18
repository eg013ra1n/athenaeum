//! Rebuild the all-sky quad index at a user-supplied magnitude limit.
//!
//! Usage:
//!   cargo run -p athenaeum-core --example rebuild_quad_index --release -- <mag_limit> [output_name]
//!
//! The current `quad_index.bin` is NOT overwritten if `output_name` is
//! supplied; if you want to swap the active index, pass `quad_index.bin`
//! explicitly and the existing file is renamed to `quad_index.bak` first.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use athenaeum_core::plate_solve::index_builder::{
    IndexBuildProgress, IndexBuilder, DEFAULT_HASH_TOLERANCE,
};

fn main() -> anyhow::Result<()> {
    let mag_limit: f32 = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: rebuild_quad_index <mag_limit> [output_name]"))?
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid mag_limit: {e}"))?;
    let output_name = std::env::args()
        .nth(2)
        .unwrap_or_else(|| format!("quad_index_mag{:.0}.bin", mag_limit * 10.0));

    let home = std::env::var("HOME")?;
    let catalog_dir = PathBuf::from(&home)
        .join("Library/Application Support/com.vsharifov.athenaeum/catalogs/tycho2");
    let out_path = catalog_dir.join(&output_name);

    println!("catalog: {}", catalog_dir.display());
    println!("output:  {}", out_path.display());
    println!("mag≤:    {mag_limit}");
    println!("hash_tolerance: {DEFAULT_HASH_TOLERANCE}");

    // Swap-safety: if target is the active quad_index.bin, back it up first.
    if output_name == "quad_index.bin" {
        let bak = catalog_dir.join("quad_index.bak");
        if out_path.exists() {
            println!("moving existing quad_index.bin → quad_index.bak");
            std::fs::rename(&out_path, &bak)?;
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let last_pct_logged = -1i32;

    let quad_count = IndexBuilder::build(
        &catalog_dir,
        &out_path,
        mag_limit,
        DEFAULT_HASH_TOLERANCE,
        cancel,
        &|p| match p {
            IndexBuildProgress::Reading { pixel, total, quads_so_far } => {
                // Log every 5% of progress.
                let pct = (pixel * 100 / total.max(1)) as i32;
                if pct / 5 != last_pct_logged / 5 {
                    let _ = std::io::Write::write_all(
                        &mut std::io::stderr(),
                        format!(
                            "  {:>3}%  pixel {:>6}/{:<6}  quads so far: {}\n",
                            pct, pixel, total, quads_so_far
                        )
                        .as_bytes(),
                    );
                }
            }
            IndexBuildProgress::Writing { bytes_written, total_bytes } => {
                let pct = (bytes_written * 100 / total_bytes.max(1)) as i32;
                if pct / 10 != last_pct_logged / 10 {
                    eprintln!(
                        "  writing: {:>3}%  ({} of {} bytes)",
                        pct, bytes_written, total_bytes
                    );
                }
            }
            IndexBuildProgress::Complete { quad_count, size_bytes } => {
                eprintln!(
                    "  DONE:  {} quads, {} bytes ({:.1} MiB)",
                    quad_count,
                    size_bytes,
                    size_bytes as f64 / 1_048_576.0
                );
            }
        },
    )?;

    println!(
        "\nbuilt in {:.1}s: {} quads at {}",
        start.elapsed().as_secs_f64(),
        quad_count,
        out_path.display()
    );

    Ok(())
}
