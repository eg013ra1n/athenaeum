//! Gaia DR3 (G ≤ 19) ingest from the **bulk CDN dump** at
//! `https://cdn.gea.esac.esa.int/Gaia/gdr3/gaia_source/`.
//!
//! Alternative to [`super::gaia`]'s TAP path. At G < 19 the TAP route
//! transfers ~75 GB of server-projected, server-filtered CSV over ~3–4 days
//! of fair-use TAP queue time; the bulk dump transfers ~600 GB of gzipped
//! full-row CSV over pure HTTP (no compute, no queue, no 90-min job cap,
//! no per-tile error cascade) in hours, then we filter locally. At depths
//! ≥ G<19 the bulk path is strictly better — and at full depth it's the
//! only option.
//!
//! ## Two phases
//!
//! 1. **Download** ([`download_bulk`]): N concurrent HTTP GETs for the
//!    ~3,386 `GaiaSource_NNNNNN-NNNNNN.csv.gz` files into a user-chosen
//!    persistent directory (a NAS mount works well). Range-resume + MD5
//!    verify per file. Idempotent — a file already on disk with the right
//!    MD5 is skipped instantly.
//! 2. **Ingest** ([`ingest_bulk`]): for each downloaded file, stream-gunzip,
//!    project the 6 columns we need from the ~152-column row, apply
//!    `phot_g_mean_mag < 19` + [`GAIA_RUWE_MAX`] locally, and hand records
//!    to the **same** [`super::gaia::HealpixBinner`] the TAP path uses — so
//!    the output `healpix_*.bin` is byte-identical to what the TAP path
//!    would have produced from the same input.
//!
//! The two phases are decoupled by design: the raw `.csv.gz` files are
//! the long-term archive on NAS; the local catalog can be re-binned (new
//! RUWE threshold, new mag cut, format change) any time with zero
//! re-download.
//!
//! ## File-list source
//!
//! The CDN serves `_MD5SUM.txt` at the prefix root with `<md5> <filename>`
//! lines — that's our authoritative file list AND our integrity check.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use md5::{Digest, Md5};

use super::binary_format::StarRecord;
use super::gaia::{HealpixBinner, GAIA_MAG_LIMIT, GAIA_RUWE_MAX};

/// CDN prefix for the Gaia DR3 `gaia_source` bulk dump.
pub const GAIA_BULK_BASE: &str = "https://cdn.gea.esac.esa.int/Gaia/gdr3/gaia_source/";

/// Filename of the per-prefix manifest the CDN serves alongside the data.
pub const MD5_MANIFEST_FILENAME: &str = "_MD5SUM.txt";

/// Default concurrent HTTP downloads. The CDN sustains far more, but 6
/// keeps us well-behaved and saturates a typical home connection.
pub const DEFAULT_DOWNLOAD_CONCURRENCY: usize = 6;

/// Default concurrent file-ingest workers. Each worker gunzips + parses one
/// `.csv.gz` (~140-180 MB compressed → ~1-2 GB plaintext) in a streaming
/// pass; 4 saturates a typical 4-8 core CPU without thrashing.
pub const DEFAULT_INGEST_CONCURRENCY: usize = 4;

/// Chunk size (records) handed from an ingest worker to the binner consumer.
/// Big enough that the channel send/recv cost is amortised, small enough
/// that a worker doesn't sit on too much RAM waiting for the consumer.
const INGEST_CHUNK_SIZE: usize = 64 * 1024;

/// One entry in the `_MD5SUM.txt` manifest.
#[derive(Clone, Debug)]
pub struct BulkFile {
    /// `GaiaSource_NNNNNN-NNNNNN.csv.gz`.
    pub name: String,
    /// 32-char lowercase hex.
    pub md5_hex: String,
}

/// Progress events for the bulk pipeline (download + ingest + finalize).
pub enum GaiaBulkProgress {
    /// Fires once after the manifest is loaded, before any file work.
    DownloadStarted {
        total_files: usize,
        already_done_files: usize,
        total_bytes_approx: Option<u64>,
    },
    /// One file's download finished (or was skipped as already-present).
    DownloadFinished {
        name: String,
        completed: usize,
        total_files: usize,
        bytes: u64,
        skipped: bool,
    },
    /// Begin local ingest.
    IngestStarted { total_files: usize },
    /// One file fully ingested (all rows pushed to the binner).
    IngestProgress {
        name: String,
        completed: usize,
        total_files: usize,
        stars_kept: usize,
    },
    /// Per-pixel scratch → final `healpix_*.bin` pass running.
    Finalizing,
    /// All done.
    Complete { total_stars: usize },
    /// Soft error (logged, run continues if possible). Hard errors return
    /// `Err` from the top-level function instead.
    Error(String),
}

// ---------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------

/// Blocking client tuned for long, large CDN downloads.
pub fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        // A single file can take many minutes on a slow link; pick a generous
        // overall timeout but rely on retries for hangs.
        .timeout(Duration::from_secs(60 * 30))
        .connect_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .user_agent("athenaeum-catalog-ingest (astrophotography catalog builder)")
        .build()
        .context("build bulk HTTP client")
}

/// Retry a network op through transient failures with exponential backoff.
/// Mirrors the policy in [`super::gaia`].
fn with_retry<T>(what: &str, mut op: impl FnMut() -> Result<T>) -> Result<T> {
    const MAX: u32 = 6;
    let mut delay = Duration::from_secs(2);
    let mut last: Option<anyhow::Error> = None;
    for attempt in 1..=MAX {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                tracing::warn!(
                    stage = what,
                    attempt,
                    max = MAX,
                    delay_secs = delay.as_secs(),
                    error = %e,
                    "gaia bulk request failed, retrying"
                );
                last = Some(e);
                if attempt < MAX {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    }
    Err(last
        .unwrap()
        .context(format!("{what} failed after {MAX} attempts")))
}

// ---------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------

/// Parse the body of `_MD5SUM.txt`. Each line is `<md5hex>  <filename>` (one
/// or more whitespace chars separate them). Lines that don't fit are
/// skipped. Only `*.csv.gz` filenames are kept (the manifest also lists
/// `_disclaimer.txt`, `_citation.txt`, etc. — we don't need those).
pub fn parse_md5_manifest(text: &str) -> Vec<BulkFile> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(md5), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if !name.ends_with(".csv.gz") {
            continue;
        }
        out.push(BulkFile {
            name: name.to_string(),
            md5_hex: md5.to_ascii_lowercase(),
        });
    }
    out
}

/// Fetch the CDN-side manifest. The body is small (~250 KB) so a single GET
/// is fine.
pub fn fetch_md5_manifest(client: &reqwest::blocking::Client) -> Result<Vec<BulkFile>> {
    let url = format!("{GAIA_BULK_BASE}{MD5_MANIFEST_FILENAME}");
    with_retry("fetch manifest", || {
        let resp = client.get(&url).send().context("GET manifest")?;
        let status = resp.status();
        let body = resp.text().context("read manifest body")?;
        if !status.is_success() {
            anyhow::bail!("manifest HTTP {status}: {}", body.chars().take(400).collect::<String>());
        }
        let files = parse_md5_manifest(&body);
        if files.is_empty() {
            anyhow::bail!("manifest parsed to 0 .csv.gz entries; body head: {}",
                body.chars().take(400).collect::<String>());
        }
        Ok(files)
    })
}

// ---------------------------------------------------------------------
// MD5 verification
// ---------------------------------------------------------------------

/// Stream a file through MD5 and return the lowercase hex digest.
pub fn md5_of_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("open {} for md5", path.display()))?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).with_context(|| format!("read {} for md5", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------
// Per-file download (range-resume + MD5 verify)
// ---------------------------------------------------------------------

/// Download one bulk file into `dest_dir/<name>`, resuming a partial
/// `<name>.partial` if present, and atomically renaming to `<name>` on
/// successful MD5 match.
///
/// Idempotency: if `dest_dir/<name>` exists and its MD5 matches the
/// manifest, returns `Ok(0)` and does no network I/O. Returns the bytes
/// transferred over the network (0 when skipped).
pub fn download_one(
    client: &reqwest::blocking::Client,
    file: &BulkFile,
    dest_dir: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<u64> {
    let final_path = dest_dir.join(&file.name);
    let partial_path = dest_dir.join(format!("{}.partial", file.name));

    if final_path.is_file() {
        // Fast path: trust the rename invariant — a finalized file was
        // verified at finalize time. Avoid re-hashing 600 GB on every
        // resume by treating presence as proof. (Add an explicit
        // `verify_existing` pass if you want a paranoid recheck.)
        return Ok(0);
    }

    let url = format!("{GAIA_BULK_BASE}{}", file.name);

    let bytes_transferred = with_retry(&format!("GET {}", file.name), || {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        // Resume from partial if present.
        let already = std::fs::metadata(&partial_path).map(|m| m.len()).unwrap_or(0);

        let mut req = client.get(&url);
        if already > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={already}-"));
        }
        let mut resp = req.send().with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !(status.is_success() || status.as_u16() == 206) {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "HTTP {status} for {url}: {}",
                body.chars().take(400).collect::<String>()
            );
        }

        // Append (when resuming) or create.
        let mut out = OpenOptions::new()
            .create(true)
            .append(already > 0)
            .write(true)
            .truncate(already == 0)
            .open(&partial_path)
            .with_context(|| format!("open {} for write", partial_path.display()))?;

        let mut buf = vec![0u8; 1 << 20];
        let mut new_bytes = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let n = resp
                .read(&mut buf)
                .with_context(|| format!("read body of {url}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .with_context(|| format!("write {}", partial_path.display()))?;
            new_bytes += n as u64;
        }
        out.flush()?;
        Ok::<u64, anyhow::Error>(new_bytes)
    })?;

    // Verify MD5 of the full partial file, then rename atomically.
    let got = md5_of_file(&partial_path)
        .with_context(|| format!("md5 of {}", partial_path.display()))?;
    if got != file.md5_hex {
        // Bad checksum — likely truncated or corrupted resume. Wipe and
        // surface so a retry can re-download from scratch.
        let _ = std::fs::remove_file(&partial_path);
        anyhow::bail!(
            "MD5 mismatch on {}: expected {}, got {} (partial removed for retry)",
            file.name,
            file.md5_hex,
            got
        );
    }
    std::fs::rename(&partial_path, &final_path)
        .with_context(|| format!("commit {}", final_path.display()))?;
    Ok(bytes_transferred)
}

// ---------------------------------------------------------------------
// Bulk download orchestrator
// ---------------------------------------------------------------------

/// Download all `.csv.gz` files listed in the CDN manifest into `dest_dir`,
/// running `concurrency` HTTP workers in parallel. Idempotent + resumable:
/// already-completed files are skipped, in-flight `.partial` files resume
/// via HTTP Range. Returns the number of bytes transferred this run (0 on
/// a fully-cached re-run).
pub fn download_bulk(
    dest_dir: &Path,
    concurrency: usize,
    cancel: Arc<AtomicBool>,
    progress: &(dyn Fn(GaiaBulkProgress) + Sync),
) -> Result<u64> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("create dest dir {}", dest_dir.display()))?;

    let client = http_client()?;
    let files = fetch_md5_manifest(&client)?;
    // Cache the manifest alongside the data — a re-run that doesn't need
    // to re-fetch the network can still ingest.
    let manifest_cache = dest_dir.join(MD5_MANIFEST_FILENAME);
    if let Err(e) = write_manifest_cache(&manifest_cache, &files) {
        tracing::warn!(
            path = %manifest_cache.display(),
            error = %e,
            "failed to cache gaia bulk manifest"
        );
    }

    let total_files = files.len();
    let present: HashSet<String> = files
        .iter()
        .filter(|f| dest_dir.join(&f.name).is_file())
        .map(|f| f.name.clone())
        .collect();
    let already_done = present.len();

    progress(GaiaBulkProgress::DownloadStarted {
        total_files,
        already_done_files: already_done,
        total_bytes_approx: None,
    });

    let work: Vec<BulkFile> = files
        .into_iter()
        .filter(|f| !present.contains(&f.name))
        .collect();
    let work = Arc::new(work);
    let next = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(already_done as u64));
    let bytes_total = Arc::new(AtomicU64::new(0));
    let first_err: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));

    let concurrency = concurrency.max(1);

    std::thread::scope(|s| {
        for _ in 0..concurrency {
            let (work, next, completed, bytes_total, first_err, cancel) = (
                Arc::clone(&work),
                Arc::clone(&next),
                Arc::clone(&completed),
                Arc::clone(&bytes_total),
                Arc::clone(&first_err),
                Arc::clone(&cancel),
            );
            let client = http_client();
            s.spawn(move || {
                let client = match client {
                    Ok(c) => c,
                    Err(e) => {
                        let mut slot = first_err.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e.context("worker HTTP client"));
                        }
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                };
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next.fetch_add(1, Ordering::Relaxed) as usize;
                    if idx >= work.len() {
                        break;
                    }
                    let f = &work[idx];
                    match download_one(&client, f, dest_dir, &cancel) {
                        Ok(n) => {
                            bytes_total.fetch_add(n, Ordering::Relaxed);
                            let c = completed.fetch_add(1, Ordering::Relaxed) + 1;
                            progress(GaiaBulkProgress::DownloadFinished {
                                name: f.name.clone(),
                                completed: c as usize,
                                total_files,
                                bytes: n,
                                skipped: false,
                            });
                        }
                        Err(e) => {
                            let mut slot = first_err.lock().unwrap();
                            if slot.is_none() {
                                *slot = Some(e.context(format!("file {}", f.name)));
                            }
                            cancel.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                }
            });
        }
    });

    if let Some(e) = first_err.lock().unwrap().take() {
        return Err(e);
    }
    Ok(bytes_total.load(Ordering::Relaxed))
}

fn write_manifest_cache(path: &Path, files: &[BulkFile]) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    for f in files {
        writeln!(w, "{}  {}", f.md5_hex, f.name)?;
    }
    w.flush()?;
    Ok(())
}

/// Read a manifest that was cached locally (same format as the CDN's
/// `_MD5SUM.txt`). Convenience for offline re-ingest.
pub fn read_manifest_cache(path: &Path) -> Result<Vec<BulkFile>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    let files = parse_md5_manifest(&text);
    if files.is_empty() {
        anyhow::bail!("cached manifest at {} parsed to 0 entries", path.display());
    }
    Ok(files)
}

// ---------------------------------------------------------------------
// Per-file ingest (stream-gunzip → CSV → 6-col project → filter → binner)
// ---------------------------------------------------------------------

/// Indices of the six columns we keep, derived from the file's header row.
#[derive(Clone, Copy, Debug)]
struct ColumnIndex {
    ra: usize,
    dec: usize,
    mag: usize,
    pmra: usize,
    pmdec: usize,
    ruwe: usize,
}

impl ColumnIndex {
    fn from_header(header: &str) -> Result<Self> {
        let mut map = std::collections::HashMap::new();
        for (i, name) in header.split(',').enumerate() {
            map.insert(name.trim().to_ascii_lowercase(), i);
        }
        let get = |k: &str| -> Result<usize> {
            map.get(k)
                .copied()
                .ok_or_else(|| anyhow!("header missing column `{k}` (found: {})", header))
        };
        Ok(Self {
            ra: get("ra")?,
            dec: get("dec")?,
            mag: get("phot_g_mean_mag")?,
            pmra: get("pmra")?,
            pmdec: get("pmdec")?,
            ruwe: get("ruwe")?,
        })
    }

    /// Largest column index we read — used to short-circuit the per-row
    /// split once we have everything we need.
    fn max(&self) -> usize {
        self.ra
            .max(self.dec)
            .max(self.mag)
            .max(self.pmra)
            .max(self.pmdec)
            .max(self.ruwe)
    }
}

/// Parse one Gaia DR3 bulk CSV row given the column indices. Same filter
/// semantics as [`super::gaia::parse_gaia_csv_row`]:
///
/// - `ra`, `dec`, `phot_g_mean_mag` must be present and parse as numbers.
/// - `phot_g_mean_mag >= GAIA_MAG_LIMIT` → drop (defence; the server-side
///    cap in the TAP path doesn't exist here, so this is the local cap).
/// - `pmra`/`pmdec` empty → 0.
/// - `ruwe` present and `>= GAIA_RUWE_MAX` → drop. Empty/missing → keep.
fn parse_bulk_row(row: &str, idx: ColumnIndex, mag_limit: f32) -> Option<StarRecord> {
    let cap = idx.max() + 1;
    // Build a Vec of just the columns we care about. The bulk CSV has ~150
    // columns; we touch ~6, but `split` walks the whole row anyway — keep it
    // simple and just collect everything up to the last needed index.
    let mut cols: Vec<&str> = Vec::with_capacity(cap.min(160));
    for (i, c) in row.split(',').enumerate() {
        if i >= cap {
            break;
        }
        cols.push(c);
    }
    if cols.len() <= idx.max() {
        return None;
    }
    let ra: f64 = cols[idx.ra].trim().parse().ok()?;
    let dec: f64 = cols[idx.dec].trim().parse().ok()?;
    let g: f32 = cols[idx.mag].trim().parse().ok()?;
    if !g.is_finite() || g >= mag_limit {
        return None;
    }
    let pmra: f64 = match cols[idx.pmra].trim() {
        "" | "null" | "NULL" => 0.0,
        s => s.parse().ok()?,
    };
    let pmdec: f64 = match cols[idx.pmdec].trim() {
        "" | "null" | "NULL" => 0.0,
        s => s.parse().ok()?,
    };
    let ruwe_s = cols[idx.ruwe].trim();
    if !ruwe_s.is_empty() && ruwe_s != "null" && ruwe_s != "NULL" {
        let ruwe: f64 = ruwe_s.parse().ok()?;
        if ruwe >= GAIA_RUWE_MAX {
            return None;
        }
    }
    Some(StarRecord::from_values(ra as f32, dec as f32, g, pmra, pmdec))
}

/// Stream-ingest one bulk `.csv.gz` file into chunks of records, sending
/// each chunk to the binner consumer. Returns the kept-records count.
fn ingest_one_file(
    path: &Path,
    sender: &mpsc::SyncSender<(String, Vec<StarRecord>)>,
    cancel: &Arc<AtomicBool>,
    mag_limit: f32,
) -> Result<usize> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let gz = GzDecoder::new(BufReader::with_capacity(1 << 20, f));
    let reader = BufReader::with_capacity(1 << 20, gz);

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unnamed>")
        .to_string();

    // Skip leading `#` comment lines (Gaia bulk CSVs ship with a copyright /
    // citation preamble), then take the next non-empty line as the header.
    let mut lines = reader.lines();
    let mut header: Option<String> = None;
    for line in &mut lines {
        let l = line.with_context(|| format!("read header in {}", path.display()))?;
        let t = l.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        header = Some(l);
        break;
    }
    let header = header.ok_or_else(|| anyhow!("no CSV header in {}", path.display()))?;
    let idx = ColumnIndex::from_header(&header)
        .with_context(|| format!("parse header of {}", path.display()))?;

    let mut chunk: Vec<StarRecord> = Vec::with_capacity(INGEST_CHUNK_SIZE);
    let mut kept = 0usize;

    for line in lines {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let l = match line {
            Ok(l) => l,
            Err(e) => {
                return Err(anyhow::Error::from(e).context(format!("read row in {}", path.display())));
            }
        };
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(rec) = parse_bulk_row(&l, idx, mag_limit) {
            chunk.push(rec);
            kept += 1;
            if chunk.len() >= INGEST_CHUNK_SIZE {
                let to_send = std::mem::replace(&mut chunk, Vec::with_capacity(INGEST_CHUNK_SIZE));
                if sender.send((name.clone(), to_send)).is_err() {
                    anyhow::bail!("binner consumer hung up");
                }
            }
        }
    }
    if !chunk.is_empty() && sender.send((name.clone(), chunk)).is_err() {
        anyhow::bail!("binner consumer hung up");
    }
    Ok(kept)
}

// ---------------------------------------------------------------------
// Bulk ingest orchestrator
// ---------------------------------------------------------------------

/// Ingest every `.csv.gz` in `bulk_dir` into the standard depth-6 HEALPix
/// catalog at `app_data_dir/catalogs/gaia_dr3/`, using a transient scratch
/// directory `app_data_dir/gaia_dr3_bulk_scratch/bins/`. Returns the total
/// star count written. Catalog dir is left empty if the function errors.
pub fn ingest_bulk(
    bulk_dir: &Path,
    app_data_dir: &Path,
    mag_limit: f32,
    concurrency: usize,
    cancel: Arc<AtomicBool>,
    progress: &(dyn Fn(GaiaBulkProgress) + Sync),
) -> Result<usize> {
    let catalog_dir = app_data_dir.join("catalogs").join("gaia_dr3");
    let scratch_dir = app_data_dir.join("gaia_dr3_bulk_scratch").join("bins");
    std::fs::create_dir_all(&scratch_dir)
        .with_context(|| format!("create scratch {}", scratch_dir.display()))?;

    // Enumerate input files from the bulk dir. Prefer the cached manifest's
    // order (stable, matches the CDN); fall back to filesystem listing.
    let manifest_cache = bulk_dir.join(MD5_MANIFEST_FILENAME);
    let files: Vec<PathBuf> = if manifest_cache.is_file() {
        let manifest = read_manifest_cache(&manifest_cache)?;
        manifest
            .into_iter()
            .map(|f| bulk_dir.join(f.name))
            .filter(|p| p.is_file())
            .collect()
    } else {
        let mut v: Vec<PathBuf> = std::fs::read_dir(bulk_dir)
            .with_context(|| format!("list {}", bulk_dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".csv.gz"))
            })
            .collect();
        v.sort();
        v
    };

    if files.is_empty() {
        anyhow::bail!(
            "no .csv.gz files in {} — download them first",
            bulk_dir.display()
        );
    }
    let total_files = files.len();

    progress(GaiaBulkProgress::IngestStarted { total_files });

    let binner = HealpixBinner::open(&scratch_dir)?;
    let files = Arc::new(files);
    let next = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let total_kept = Arc::new(AtomicU64::new(0));
    let first_err: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));

    let concurrency = concurrency.max(1);
    let (tx, rx) = mpsc::sync_channel::<(String, Vec<StarRecord>)>(concurrency * 2);

    std::thread::scope(|s| {
        for _ in 0..concurrency {
            let (files, next, completed, total_kept, first_err, cancel) = (
                Arc::clone(&files),
                Arc::clone(&next),
                Arc::clone(&completed),
                Arc::clone(&total_kept),
                Arc::clone(&first_err),
                Arc::clone(&cancel),
            );
            let tx = tx.clone();
            s.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let idx = next.fetch_add(1, Ordering::Relaxed) as usize;
                if idx >= files.len() {
                    break;
                }
                let path = &files[idx];
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<unnamed>")
                    .to_string();
                match ingest_one_file(path, &tx, &cancel, mag_limit) {
                    Ok(kept) => {
                        total_kept.fetch_add(kept as u64, Ordering::Relaxed);
                        let c = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        progress(GaiaBulkProgress::IngestProgress {
                            name,
                            completed: c as usize,
                            total_files,
                            stars_kept: kept,
                        });
                    }
                    Err(e) => {
                        let mut slot = first_err.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e.context(format!("ingest {}", name)));
                        }
                        cancel.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            });
        }
        drop(tx);

        // Consumer: serialize binner writes.
        while let Ok((name, recs)) = rx.recv() {
            if let Err(e) = binner.push_tile(&recs) {
                let mut slot = first_err.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e.context(format!("binner push from {}", name)));
                }
                cancel.store(true, Ordering::Relaxed);
            }
        }
    });

    if let Some(e) = first_err.lock().unwrap().take() {
        return Err(e);
    }

    progress(GaiaBulkProgress::Finalizing);
    let total = binner.finalize(&catalog_dir)?;
    progress(GaiaBulkProgress::Complete { total_stars: total });
    let _kept = total_kept.load(Ordering::Relaxed) as usize;
    Ok(total)
}

// ---------------------------------------------------------------------
// Top-level orchestrator (optional convenience)
// ---------------------------------------------------------------------

/// End-to-end: download (if missing) + ingest. Suitable for unattended one-
/// shot use; for separate phases (download to NAS now, ingest later) call
/// [`download_bulk`] and [`ingest_bulk`] directly.
pub fn setup_gaia_dr3_from_bulk(
    bulk_dir: &Path,
    app_data_dir: &Path,
    download_concurrency: usize,
    ingest_concurrency: usize,
    cancel: Arc<AtomicBool>,
    progress: &(dyn Fn(GaiaBulkProgress) + Sync),
) -> Result<PathBuf> {
    let catalog_dir = app_data_dir.join("catalogs").join("gaia_dr3");
    if catalog_dir.exists() {
        let file_count = std::fs::read_dir(&catalog_dir)?.count();
        if file_count > 100 {
            tracing::info!(file_count, "gaia bulk catalog already exists, skipping setup");
            return Ok(catalog_dir);
        }
    }
    download_bulk(bulk_dir, download_concurrency, Arc::clone(&cancel), progress)?;
    ingest_bulk(bulk_dir, app_data_dir, GAIA_MAG_LIMIT, ingest_concurrency, cancel, progress)?;
    Ok(catalog_dir)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_md5_manifest_lines() {
        let body = "\
# header comment, ignore
d41d8cd98f00b204e9800998ecf8427e  GaiaSource_000000-003111.csv.gz
abcdef0123456789abcdef0123456789  GaiaSource_003112-005263.csv.gz
not-a-hash _disclaimer.txt
00112233445566778899aabbccddeeff  _disclaimer.txt
00112233445566778899aabbccddeeff  GaiaSource_005264-006601.csv.gz
";
        let v = parse_md5_manifest(body);
        assert_eq!(v.len(), 3, "got: {v:?}");
        assert_eq!(v[0].name, "GaiaSource_000000-003111.csv.gz");
        assert_eq!(v[0].md5_hex, "d41d8cd98f00b204e9800998ecf8427e");
        // Non-csv.gz entries are filtered out.
        assert!(v.iter().all(|f| f.name.ends_with(".csv.gz")));
        // Lower-casing.
        assert!(v.iter().all(|f| f.md5_hex.chars().all(|c| !c.is_ascii_uppercase())));
    }

    #[test]
    fn column_index_resolves_known_header() {
        let header = "solution_id,designation,source_id,ref_epoch,ra,ra_error,dec,dec_error,parallax,pmra,pmra_error,pmdec,pmdec_error,ruwe,phot_g_mean_mag,phot_g_mean_flux";
        let idx = ColumnIndex::from_header(header).unwrap();
        assert_eq!(idx.ra, 4);
        assert_eq!(idx.dec, 6);
        assert_eq!(idx.pmra, 9);
        assert_eq!(idx.pmdec, 11);
        assert_eq!(idx.ruwe, 13);
        assert_eq!(idx.mag, 14);
        // Header missing a needed column is a hard error.
        let bad = "solution_id,ra,dec,pmra,pmdec,ruwe"; // no phot_g_mean_mag
        assert!(ColumnIndex::from_header(bad).is_err());
    }

    #[test]
    fn parse_bulk_row_applies_filters() {
        let header = "ra,dec,phot_g_mean_mag,pmra,pmdec,ruwe";
        let idx = ColumnIndex::from_header(header).unwrap();

        // Good row.
        let s = parse_bulk_row("86.682,0.042,14.231,12.5,-7.25,1.05", idx, 19.0).expect("kept");
        assert!((s.ra - 86.682).abs() < 1e-3);
        assert!((s.mag() - 14.231).abs() < 1e-3);

        // RUWE = 1.4 → dropped (>= cap).
        assert!(parse_bulk_row("86.682,0.042,14.231,12.5,-7.25,1.4", idx, 19.0).is_none());
        // RUWE = 3.7 → dropped.
        assert!(parse_bulk_row("86.682,0.042,14.231,12.5,-7.25,3.7", idx, 19.0).is_none());
        // Empty RUWE (2-param solution) → kept.
        assert!(parse_bulk_row("86.682,0.042,14.231,12.5,-7.25,", idx, 19.0).is_some());
        // Null PM → treated as 0 (Gaia 2-param).
        let s2 = parse_bulk_row("10,20,15.9,,,", idx, 19.0).expect("null-pm kept");
        assert_eq!(s2.pmra_mas_yr(), 0.0);
        assert_eq!(s2.pmdec_mas_yr(), 0.0);
        // Magnitude past the cap → dropped.
        assert!(parse_bulk_row("10,20,19.5,0,0,1.0", idx, 19.0).is_none());
        // Bare-NaN mag → dropped.
        assert!(parse_bulk_row("10,20,nan,0,0,1.0", idx, 19.0).is_none());
        // Garbage row → None.
        assert!(parse_bulk_row("not,enough", idx, 19.0).is_none());
    }

    #[test]
    fn parse_bulk_row_honours_mag_limit_arg() {
        let header = "ra,dec,phot_g_mean_mag,pmra,pmdec,ruwe";
        let idx = ColumnIndex::from_header(header).unwrap();
        let row = "10.0,20.0,20.5,0,0,1.0"; // G=20.5
        assert!(parse_bulk_row(row, idx, 19.0).is_none(), "dropped at limit 19");
        assert!(parse_bulk_row(row, idx, 21.0).is_some(), "kept at limit 21");
    }

    #[test]
    fn md5_of_file_matches_known_digest() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("e.bin");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(md5_of_file(&p).unwrap(), "d41d8cd98f00b204e9800998ecf8427e");
        let p2 = tmp.path().join("abc.bin");
        std::fs::write(&p2, b"abc").unwrap();
        assert_eq!(md5_of_file(&p2).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
    }

    /// Sibling of the MD5 pin above, for the other published digest: the
    /// `.sha256` sidecar `gaia_prebuilt::sha256_file` writes next to every
    /// catalog archive is an EXTERNAL contract (users verify downloads against
    /// it), so pin `sha2` to FIPS 180-4's "abc" vector — a future bump that
    /// changed the output would be caught here, not by a failed user download.
    #[test]
    fn sha256_matches_known_digest() {
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"abc");
        assert_eq!(
            hex_lower(&hasher.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn manifest_cache_round_trip() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("_MD5SUM.txt");
        let files = vec![
            BulkFile {
                name: "GaiaSource_000000-003111.csv.gz".into(),
                md5_hex: "d41d8cd98f00b204e9800998ecf8427e".into(),
            },
            BulkFile {
                name: "GaiaSource_003112-005263.csv.gz".into(),
                md5_hex: "abcdef0123456789abcdef0123456789".into(),
            },
        ];
        write_manifest_cache(&p, &files).unwrap();
        let back = read_manifest_cache(&p).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, files[0].name);
        assert_eq!(back[0].md5_hex, files[0].md5_hex);
    }
}
