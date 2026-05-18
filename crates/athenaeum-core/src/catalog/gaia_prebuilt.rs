//! Prebuilt Gaia DR3 (G≤16) catalog download.
//!
//! The from-source TAP ingest ([`super::gaia`]) runs ~hours and hammers ESA
//! — untenable per end user. Instead the catalog is built **once** and the
//! resulting ~4 GB HEALPix archive is hosted on our own server; end users
//! just fetch + extract that single artifact (the Gaia data licence permits
//! redistribution of derived subsets with ESA/Gaia/DPAC credit).
//!
//! Robust by design: HTTP Range **resume** for the big download (a drop near
//! the end doesn't restart 4 GB), SHA-256 integrity check before extract,
//! zip-slip-safe extraction, idempotent. Lands the catalog at
//! `catalogs/gaia_dr3/` exactly like the TAP path, so the solver seam is
//! unchanged.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Default location of the prebuilt archive + its sidecar checksum.
/// Override with `ATHENAEUM_GAIA_PREBUILT_URL` (full URL to the `.zip`;
/// the checksum is that URL + `.sha256`).
pub const GAIA_PREBUILT_URL: &str = "https://artfrom.space/catalogs/gaia_dr3_g16.zip";

fn prebuilt_urls() -> (String, String) {
    let zip = std::env::var("ATHENAEUM_GAIA_PREBUILT_URL")
        .unwrap_or_else(|_| GAIA_PREBUILT_URL.to_string());
    let sha = format!("{zip}.sha256");
    (zip, sha)
}

/// Progress for the prebuilt path. Separate from [`super::gaia::GaiaProgress`]
/// so the (working) TAP path is untouched.
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

/// Download (Range-resumable), verify, and extract the prebuilt catalog.
/// Idempotent: returns immediately if `catalogs/gaia_dr3/` is already
/// populated (mirrors [`super::gaia::setup_gaia_dr3_catalog`]).
pub fn download_gaia_dr3_prebuilt(
    app_data_dir: &Path,
    cancel_flag: Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<PathBuf> {
    let catalog_dir = app_data_dir.join("catalogs").join("gaia_dr3");
    if catalog_dir.exists() {
        let n = std::fs::read_dir(&catalog_dir)?.count();
        if n > 100 {
            eprintln!("gaia: catalog already exists with {n} files");
            return Ok(catalog_dir);
        }
    }

    let (zip_url, sha_url) = prebuilt_urls();
    let client = http_client()?;
    let zip_path = app_data_dir.join("gaia_dr3.zip");
    let part_path = app_data_dir.join("gaia_dr3.zip.part");
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
        eprintln!("gaia: no .sha256 sidecar at {sha_url} — skipping integrity check");
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

    let files = extract_zip(&zip_path, &catalog_dir, &cancel_flag, progress)?;
    // The extracted catalog is the artifact; the ~4 GB zip is now redundant.
    let _ = std::fs::remove_file(&zip_path);
    progress(GaiaPrebuiltProgress::Complete { files });
    Ok(catalog_dir)
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

/// Extract `healpix_NNNNNN.bin` entries (zip-slip-safe: basename only,
/// strict name pattern) into `catalog_dir`.
fn extract_zip(
    zip_path: &Path,
    catalog_dir: &Path,
    cancel: &Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<usize> {
    std::fs::create_dir_all(catalog_dir)?;
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
        // zip-slip safe: ignore any path, keep the basename only, and only
        // accept the exact catalog file pattern.
        let name = Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let ok = name.len() == 18 // "healpix_NNNNNN.bin"
            && name.starts_with("healpix_")
            && name.ends_with(".bin")
            && name[8..14].bytes().all(|b| b.is_ascii_digit());
        if !ok {
            continue;
        }
        let mut out =
            File::create(catalog_dir.join(&name)).context("create catalog file")?;
        std::io::copy(&mut entry, &mut out).context("extract catalog file")?;
        done += 1;
        if done % 512 == 0 {
            progress(GaiaPrebuiltProgress::Extracting { done, total });
        }
    }
    progress(GaiaPrebuiltProgress::Extracting { done, total });
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuilt_urls_env_override() {
        std::env::set_var("ATHENAEUM_GAIA_PREBUILT_URL", "https://x.test/g.zip");
        let (z, s) = prebuilt_urls();
        assert_eq!(z, "https://x.test/g.zip");
        assert_eq!(s, "https://x.test/g.zip.sha256");
        std::env::remove_var("ATHENAEUM_GAIA_PREBUILT_URL");
        let (z2, _) = prebuilt_urls();
        assert_eq!(z2, GAIA_PREBUILT_URL);
    }

    #[test]
    fn idempotent_when_catalog_present() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let cat = tmp.path().join("catalogs").join("gaia_dr3");
        std::fs::create_dir_all(&cat).unwrap();
        for i in 0..101 {
            std::fs::write(cat.join(format!("healpix_{i:06}.bin")), b"x").unwrap();
        }
        let got = download_gaia_dr3_prebuilt(
            tmp.path(),
            Arc::new(AtomicBool::new(false)),
            &|_| {},
        )
        .unwrap();
        assert_eq!(got, cat);
    }

    #[test]
    fn extract_is_zip_slip_safe_and_pattern_strict() {
        use tempfile::TempDir;
        use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("a.zip");
        {
            let mut zw = ZipWriter::new(File::create(&zip_path).unwrap());
            let opt = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zw.start_file("healpix_000042.bin", opt).unwrap();
            zw.write_all(b"good").unwrap();
            // malicious traversal + junk names must be ignored
            zw.start_file("../../evil.bin", opt).unwrap();
            zw.write_all(b"evil").unwrap();
            zw.start_file("notes.txt", opt).unwrap();
            zw.write_all(b"junk").unwrap();
            zw.finish().unwrap();
        }
        let out = tmp.path().join("out");
        let n = extract_zip(&zip_path, &out, &Arc::new(AtomicBool::new(false)), &|_| {})
            .unwrap();
        assert_eq!(n, 1);
        assert!(out.join("healpix_000042.bin").exists());
        assert!(!out.join("evil.bin").exists());
        assert!(!tmp.path().join("evil.bin").exists());
        assert!(!out.join("notes.txt").exists());
    }
}
