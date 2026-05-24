//! Download the Gaia DR3 `gaia_source` bulk dump (~600 GB) to a directory
//! of your choice — typically a NAS mount.
//!
//! Usage:
//!   cargo run -p athenaeum-core --example download_gaia_bulk --release -- <dest_dir> [concurrency]
//!
//! - `<dest_dir>`: where the ~3,386 `.csv.gz` files go. Will be created if
//!   missing. **Reuse the same path across runs** to resume (already-
//!   downloaded files are skipped instantly).
//! - `concurrency`: parallel HTTP downloads. Default 6 (well-behaved on
//!   the ESA CDN). Try 8–12 if your connection has headroom.
//!
//! Ctrl-C is safe: the in-flight file is left as `<name>.partial` and the
//! next run resumes it via HTTP Range. After every file completes its MD5
//! is verified against the manifest before being committed to its final
//! name — a corrupted resume is detected and re-fetched automatically.
//!
//! After the download finishes, run `ingest_gaia_bulk` to bin the files
//! into `<app_data>/catalogs/gaia_dr3/`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use athenaeum_core::catalog::gaia_bulk::{
    download_bulk, GaiaBulkProgress, DEFAULT_DOWNLOAD_CONCURRENCY,
};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dest_dir = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!(
            "usage: download_gaia_bulk <dest_dir> [concurrency]\n  e.g. /Volumes/NAS/gaia_dr3_bulk"
        ))?;
    let concurrency: usize = args
        .next()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(DEFAULT_DOWNLOAD_CONCURRENCY);

    println!("Gaia DR3 bulk-dump downloader");
    println!("  dest         : {}", dest_dir.display());
    println!("  concurrency  : {concurrency}");
    println!("  ~3,386 files, ~600 GB total · resumable (Ctrl-C safe)\n");

    // Ctrl-C: the process gets SIGINT; any `.partial` left on disk is
    // resumed via HTTP Range on the next run. Finalized files are
    // intact (atomically renamed only after MD5 verify).
    let cancel = Arc::new(AtomicBool::new(false));

    let start = Instant::now();
    let bytes = download_bulk(&dest_dir, concurrency, cancel, &|p| match p {
        GaiaBulkProgress::DownloadStarted {
            total_files,
            already_done_files,
            ..
        } => println!(
            "started: {already_done_files}/{total_files} already present (resumed) · {} to fetch",
            total_files - already_done_files
        ),
        GaiaBulkProgress::DownloadFinished {
            name,
            completed,
            total_files,
            bytes,
            ..
        } => {
            let elapsed = start.elapsed().as_secs_f64();
            let mbps = if elapsed > 0.0 {
                (bytes as f64 / 1_048_576.0) / elapsed.max(0.1)
            } else {
                0.0
            };
            println!(
                "  done {completed:>5}/{total_files}  {name}  (+{:.1} MB, {:.1} MB/s avg this run)",
                bytes as f64 / 1_048_576.0,
                mbps
            );
        }
        GaiaBulkProgress::Error(e) => eprintln!("  error: {e}"),
        _ => {}
    })?;

    let mins = start.elapsed().as_secs_f64() / 60.0;
    println!(
        "\ndone — {:.1} GB transferred in {:.0} min ({:.1} MB/s avg)",
        bytes as f64 / 1_073_741_824.0,
        mins,
        if mins > 0.0 {
            (bytes as f64 / 1_048_576.0) / (mins * 60.0)
        } else {
            0.0
        }
    );
    println!("\nNext: cargo run -p athenaeum-core --example ingest_gaia_bulk --release -- {}",
        dest_dir.display());
    Ok(())
}
