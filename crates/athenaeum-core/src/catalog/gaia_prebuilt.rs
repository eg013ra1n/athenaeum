//! Prebuilt star-catalog download (`solvemyastro` `stars.smac`).
//!
//! The solver (`solvemyastro`) reads a memory-mapped `stars.smac` star cache.
//! Building that cache from Gaia DR3 is expensive (a multi-day TAP ingest plus
//! a `build-cache` pass) and is done **once** by a separate offline tool; the
//! resulting archive is hosted on our own server and end users just fetch +
//! extract it (the Gaia data licence permits redistribution of derived subsets
//! with ESA/Gaia/DPAC credit).
//!
//! Robust by design: HTTP Range **resume** for the big download (a drop near
//! the end doesn't restart the whole archive), SHA-256 integrity check before
//! extract, zip-slip-safe extraction, idempotent. Lands the deep cache at
//! `catalogs/smac_gaia/stars.smac` (what [`crate::services::ServiceContext`]
//! opens via `StarCache::open`) and, when the archive bundles it, the optional
//! bright sub-catalog at `catalogs/smac_gaia_bright/stars.smac`.
//!
//! Expected archive layout (one zip), produced by the offline build tool:
//!   * `stars.smac` (or `smac_gaia/stars.smac`)        → deep cache
//!   * `smac_gaia_bright/stars.smac` (optional)         → bright sub-catalog
//! Any other entries are ignored.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Default location of the prebuilt archive + its sidecar checksum.
/// Override with `ATHENAEUM_STAR_CATALOG_URL` (full URL to the `.zip`; the
/// checksum is that URL + `.sha256`). The legacy `ATHENAEUM_GAIA_PREBUILT_URL`
/// name is still honoured as a fallback.
pub const STAR_CATALOG_URL: &str = "https://artfrom.space/catalogs/smac_gaia.zip";

fn prebuilt_urls() -> (String, String) {
    let zip = std::env::var("ATHENAEUM_STAR_CATALOG_URL")
        .or_else(|_| std::env::var("ATHENAEUM_GAIA_PREBUILT_URL"))
        .unwrap_or_else(|_| STAR_CATALOG_URL.to_string());
    let sha = format!("{zip}.sha256");
    (zip, sha)
}

/// Progress for the prebuilt path.
pub enum GaiaPrebuiltProgress {
    Downloading { received: u64, total: u64 },
    Verifying,
    Extracting { done: usize, total: usize },
    Complete { files: usize },
    Error(String),
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1800))
        .connect_timeout(Duration::from_secs(30))
        .user_agent("athenaeum-catalog-ingest (astrophotography catalog builder)")
        .build()
        .context("build prebuilt HTTP client")
}

/// `stars.smac` header is 64 bytes + a 49 152-entry pixel directory; anything
/// smaller than this is a placeholder/truncated file, not a real cache.
const MIN_SMAC_SIZE: u64 = 64;

/// True when `dir/stars.smac` exists and is plausibly a real cache (not a
/// zero-byte placeholder).
fn smac_present(dir: &Path) -> bool {
    std::fs::metadata(dir.join("stars.smac"))
        .map(|m| m.is_file() && m.len() > MIN_SMAC_SIZE)
        .unwrap_or(false)
}

/// Download (Range-resumable), verify, and extract the prebuilt star catalog.
/// Idempotent: returns immediately if `catalogs/smac_gaia/stars.smac` is
/// already present. The deep-cache directory path is returned.
pub fn download_gaia_dr3_prebuilt(
    app_data_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<PathBuf> {
    let catalogs_dir = app_data_dir.join("catalogs");
    let deep_dir = catalogs_dir.join("smac_gaia");
    let bright_dir = catalogs_dir.join("smac_gaia_bright");
    if smac_present(&deep_dir) {
        eprintln!("star catalog: smac_gaia/stars.smac already present — skipping download");
        return Ok(deep_dir);
    }

    let (zip_url, sha_url) = prebuilt_urls();
    let client = http_client()?;
    let zip_path = app_data_dir.join("smac_gaia.zip");
    let part_path = app_data_dir.join("smac_gaia.zip.part");
    std::fs::create_dir_all(app_data_dir)?;

    // Expected checksum (best-effort: if the sidecar is missing we still
    // proceed, just without verification).
    let expected_sha: Option<String> = client
        .get(&sha_url)
        .send()
        .ok()
        .filter(|r| r.status().is_success())
        .and_then(|r| r.text().ok())
        .map(|s| s.split_whitespace().next().unwrap_or("").to_lowercase())
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if expected_sha.is_none() {
        eprintln!("star catalog: no .sha256 sidecar at {sha_url} — skipping integrity check");
    }

    if !zip_path.exists() {
        download_resumable(&client, &zip_url, &part_path, &cancel_flag, progress)?;
        std::fs::rename(&part_path, &zip_path).context("finalize downloaded archive")?;
    }

    if let Some(want) = &expected_sha {
        progress(GaiaPrebuiltProgress::Verifying);
        let got = sha256_file(&zip_path, &cancel_flag)?;
        if &got != want {
            let _ = std::fs::remove_file(&zip_path);
            anyhow::bail!(
                "prebuilt archive checksum mismatch (expected {want}, got {got}); \
                 deleted — re-run to download again"
            );
        }
    }

    let files = extract_zip(&zip_path, &deep_dir, &bright_dir, &cancel_flag, progress)?;
    // The extracted cache is the artifact; the zip is now redundant.
    let _ = std::fs::remove_file(&zip_path);

    if !smac_present(&deep_dir) {
        anyhow::bail!(
            "prebuilt archive did not contain a deep stars.smac \
             (expected 'stars.smac' or 'smac_gaia/stars.smac' inside the zip)"
        );
    }
    progress(GaiaPrebuiltProgress::Complete { files });
    Ok(deep_dir)
}

/// Stream the archive to `part_path`, resuming via HTTP Range if a partial
/// file is already present.
fn download_resumable(
    client: &reqwest::blocking::Client,
    url: &str,
    part_path: &Path,
    cancel: &Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<()> {
    use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};

    let resume_from = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header(RANGE, format!("bytes={resume_from}-"));
    }
    let resp = req.send().context("start archive download")?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("archive download HTTP {status} for {url}");
    }

    // 206 → server honored the range (append); else it sent the whole file
    // (start over from byte 0).
    let (mut file, mut received, total) = if status.as_u16() == 206 {
        let total = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.rsplit('/').next())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        (
            OpenOptions::new().append(true).open(part_path)?,
            resume_from,
            total,
        )
    } else {
        let total = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        (File::create(part_path)?, 0u64, total)
    };

    let mut reader = resp;
    let mut buf = vec![0u8; 256 * 1024];
    let mut last_emit = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let n = reader.read(&mut buf).context("read archive chunk")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("write archive chunk")?;
        received += n as u64;
        if received - last_emit >= 4 * 1024 * 1024 {
            last_emit = received;
            progress(GaiaPrebuiltProgress::Downloading { received, total });
        }
    }
    file.flush()?;
    progress(GaiaPrebuiltProgress::Downloading { received, total });
    Ok(())
}

fn sha256_file(path: &Path, cancel: &Arc<AtomicBool>) -> Result<String> {
    let mut f = BufReader::new(File::open(path).context("open archive for checksum")?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Extract the `stars.smac` cache(s) from the archive, zip-slip-safe.
///
/// Routing by the entry's relative path (after rejecting any `..` / absolute
/// component): an entry whose path contains a `smac_gaia_bright` component and
/// whose basename is `stars.smac` lands in `bright_dir`; any other
/// `stars.smac` lands in `deep_dir`. All other entries are ignored.
fn extract_zip(
    zip_path: &Path,
    deep_dir: &Path,
    bright_dir: &Path,
    cancel: &Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<usize> {
    std::fs::create_dir_all(deep_dir)?;
    let file = File::open(zip_path).context("open archive")?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).context("read zip")?;
    let total = zip.len();
    let mut done = 0usize;
    for i in 0..total {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let mut entry = zip.by_index(i).context("zip entry")?;
        if !entry.is_file() {
            continue;
        }
        // zip-slip safe: split the recorded name into components and reject
        // anything with a parent-dir (`..`), absolute, or prefix component.
        let raw = entry.name().replace('\\', "/");
        let comps: Vec<&str> = raw
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();
        if comps.is_empty()
            || comps.iter().any(|c| *c == ".." || c.contains(':'))
            || raw.starts_with('/')
        {
            continue;
        }
        let basename = *comps.last().unwrap();
        if basename != "stars.smac" {
            continue;
        }
        let is_bright = comps.iter().any(|c| *c == "smac_gaia_bright");
        let dest_dir = if is_bright { bright_dir } else { deep_dir };
        std::fs::create_dir_all(dest_dir).context("create catalog dir")?;
        let mut out = File::create(dest_dir.join("stars.smac")).context("create stars.smac")?;
        std::io::copy(&mut entry, &mut out).context("extract stars.smac")?;
        done += 1;
        progress(GaiaPrebuiltProgress::Extracting { done, total });
    }
    progress(GaiaPrebuiltProgress::Extracting { done, total });
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuilt_urls_env_override_and_legacy_fallback() {
        std::env::remove_var("ATHENAEUM_STAR_CATALOG_URL");
        std::env::remove_var("ATHENAEUM_GAIA_PREBUILT_URL");
        // New var wins.
        std::env::set_var("ATHENAEUM_STAR_CATALOG_URL", "https://x.test/s.zip");
        let (z, s) = prebuilt_urls();
        assert_eq!(z, "https://x.test/s.zip");
        assert_eq!(s, "https://x.test/s.zip.sha256");
        std::env::remove_var("ATHENAEUM_STAR_CATALOG_URL");
        // Legacy var honoured as fallback.
        std::env::set_var("ATHENAEUM_GAIA_PREBUILT_URL", "https://x.test/legacy.zip");
        let (z2, _) = prebuilt_urls();
        assert_eq!(z2, "https://x.test/legacy.zip");
        std::env::remove_var("ATHENAEUM_GAIA_PREBUILT_URL");
        // Default.
        let (z3, _) = prebuilt_urls();
        assert_eq!(z3, STAR_CATALOG_URL);
    }

    #[test]
    fn idempotent_when_smac_present() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let deep = tmp.path().join("catalogs").join("smac_gaia");
        std::fs::create_dir_all(&deep).unwrap();
        // A plausibly-real cache (> MIN_SMAC_SIZE bytes).
        std::fs::write(deep.join("stars.smac"), vec![0u8; 128]).unwrap();
        let got = download_gaia_dr3_prebuilt(tmp.path(), Arc::new(AtomicBool::new(false)), &|_| {})
            .unwrap();
        assert_eq!(got, deep);
    }

    #[test]
    fn extract_routes_deep_and_bright_and_is_zip_slip_safe() {
        use tempfile::TempDir;
        use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("a.zip");
        {
            let mut zw = ZipWriter::new(File::create(&zip_path).unwrap());
            let opt = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zw.start_file("stars.smac", opt).unwrap();
            zw.write_all(b"deep").unwrap();
            zw.start_file("smac_gaia_bright/stars.smac", opt).unwrap();
            zw.write_all(b"bright").unwrap();
            // malicious traversal + junk names must be ignored
            zw.start_file("../../evil/stars.smac", opt).unwrap();
            zw.write_all(b"evil").unwrap();
            zw.start_file("notes.txt", opt).unwrap();
            zw.write_all(b"junk").unwrap();
            zw.finish().unwrap();
        }
        let deep = tmp.path().join("out").join("smac_gaia");
        let bright = tmp.path().join("out").join("smac_gaia_bright");
        let n = extract_zip(
            &zip_path,
            &deep,
            &bright,
            &Arc::new(AtomicBool::new(false)),
            &|_| {},
        )
        .unwrap();
        assert_eq!(n, 2, "deep + bright extracted, evil/junk skipped");
        assert_eq!(std::fs::read(deep.join("stars.smac")).unwrap(), b"deep");
        assert_eq!(std::fs::read(bright.join("stars.smac")).unwrap(), b"bright");
        // zip-slip target must not be created anywhere outside dest dirs.
        assert!(!tmp.path().join("evil").exists());
    }
}
