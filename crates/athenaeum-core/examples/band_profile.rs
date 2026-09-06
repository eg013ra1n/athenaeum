//! Profiling harness for the banded integration engine — the measurement gate
//! for `docs/superpowers/plans/2026-09-06-integration-throughput.md`.
//!
//! Usage:
//!     cargo run --release -p athenaeum-core --example band_profile -- <dir> [name-substring] [budget_mb] [threads] [readers]
//!
//! `budget_mb` defaults to 0, meaning "resolve the budget the way the app
//! does"; pass a number to force one. `threads` 0 means "all cores". `readers`
//! defaults to 0, meaning "decide from the detected storage class" — pass a
//! number to force a read-concurrency override.
//!
//! COLD RUNS ONLY MEAN ANYTHING. `purge(8)` is refused to non-root users and
//! `F_NOCACHE` is unreliable on APFS (it reported 103 MB/s and 252 MB/s for
//! the same configuration on consecutive runs). Evict by streaming more
//! unrelated data than the machine has RAM before each run, e.g.
//!
//!     find <some other 25 GB of files> -name '*.fit*' | head -400 \
//!       | tr '\n' '\0' | xargs -0 -n4 cat | wc -c
use athenaeum_core::integration::combine::{IntegrationRecipe, Rejection};
use athenaeum_core::integration::engine::{integrate_bias_like, EngineProgress};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: band_profile <dir> [name-substring] [budget_mb] [threads]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let pat = args.get(2).cloned().unwrap_or_default();
    let budget_mb: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(0);
    let threads: usize = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(0);

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "fits" || e == "fit").unwrap_or(false))
        .filter(|p| pat.is_empty() || p.file_name().unwrap().to_string_lossy().contains(&pat))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no frames matched");

    let budget = if budget_mb == 0 {
        athenaeum_core::integration::band_budget::auto_budget_bytes()
    } else {
        budget_mb * 1024 * 1024
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let bytes_on_disk: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();

    let storage = athenaeum_core::integration::storage_class::classify_all(&paths);
    let readers: usize = args.get(5).map(|s| s.parse().unwrap()).unwrap_or(0);
    let io = athenaeum_core::integration::io_policy::IoPolicy {
        band_budget_bytes: budget,
        read_concurrency: athenaeum_core::integration::storage_class::read_concurrency(
            storage, readers, pool.current_num_threads(),
        ),
        storage,
    };
    println!("storage {:?}, {} readers", io.storage, io.read_concurrency);

    println!(
        "{} frames, {:.2} GB on disk, budget {} MiB, {} threads",
        paths.len(),
        bytes_on_disk as f64 / 1e9,
        budget / (1024 * 1024),
        pool.current_num_threads()
    );

    let on_band = |cur: usize, total: usize, _done: u64, _all: u64| {
        if cur == 1 { println!("bands: {total}"); }
    };
    let t = Instant::now();
    let out = integrate_bias_like(
        &paths,
        IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
        &pool,
        &std::env::temp_dir(),
        &AtomicBool::new(false),
        EngineProgress { on_band: &on_band },
        io,
    )
    .expect("integration failed");
    let all = t.elapsed();

    let read_s = out.read_duration.as_secs_f64();
    println!("bands     {:>9}   ({} rows each)", out.bands, out.band_rows);
    println!("read      {:>9.2?}   {:>6.0} MB/s", out.read_duration, out.bytes_read as f64 / read_s / 1e6);
    println!("combine   {:>9.2?}", out.combine_duration);
    println!(
        "TOTAL     {:>9.2?}   read {:.0}%  combine {:.0}%",
        all,
        100.0 * read_s / all.as_secs_f64(),
        100.0 * out.combine_duration.as_secs_f64() / all.as_secs_f64()
    );
    // Human-readable sanity check — NOT the fingerprint later tasks must
    // reproduce: a scalar sum lets equal-and-opposite drift cancel exactly,
    // and at seven significant digits on a real 6248x4176 bias set (sum
    // ~2.6e10) a whole 105-row band can drift 0.01 ADU without moving a
    // printed digit.
    let sum: f64 = out.data.iter().map(|&v| v as f64).sum();
    println!("sum       {:.6e}  ({}x{})", sum, out.width, out.height);
    // The actual fingerprint: bytewise hash of every f32, so any pixel
    // drift at all — not just drift that survives summation — changes it.
    // Tasks 3, 4 and 5 must reproduce this exactly.
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    for &v in &out.data {
        hasher.update(&v.to_le_bytes());
    }
    println!("fingerprint {:016x}  ({}x{})", hasher.digest(), out.width, out.height);
}
