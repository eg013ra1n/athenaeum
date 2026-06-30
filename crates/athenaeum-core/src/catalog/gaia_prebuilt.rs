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

use crate::catalog::manifest::{Manifest, ManifestTier};
use crate::plate_solve::layers::discover_layers;

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

/// Resolve the catalog base URL (always ends in `/`).
///
/// `ATHENAEUM_CATALOG_BASE_URL` wins; the legacy `ATHENAEUM_STAR_CATALOG_URL` /
/// `ATHENAEUM_GAIA_PREBUILT_URL` (full `.zip` URLs) are accepted by stripping the
/// trailing filename to a base. Default `https://artfrom.space/catalogs/`.
pub fn catalog_base_url() -> String {
    fn with_slash(mut s: String) -> String {
        if !s.ends_with('/') {
            s.push('/');
        }
        s
    }
    if let Ok(b) = std::env::var("ATHENAEUM_CATALOG_BASE_URL") {
        return with_slash(b);
    }
    if let Ok(zip) = std::env::var("ATHENAEUM_STAR_CATALOG_URL")
        .or_else(|_| std::env::var("ATHENAEUM_GAIA_PREBUILT_URL"))
    {
        // Strip the trailing `<file>.zip` to its containing directory.
        if let Some(slash) = zip.rfind('/') {
            return zip[..=slash].to_string();
        }
    }
    "https://artfrom.space/catalogs/".to_string()
}

fn manifest_cache_path(app_data: &Path) -> PathBuf {
    app_data.join("catalogs").join("smac_gaia").join("manifest.json")
}

/// Read the cached `smac_gaia/manifest.json` if present, else fetch
/// `<base>/manifest.json` and cache it. The cache lets status + the FOV helper
/// work offline after the first fetch.
pub fn load_or_fetch_manifest(app_data: &Path) -> Result<Manifest> {
    let cache = manifest_cache_path(app_data);
    if let Ok(bytes) = std::fs::read(&cache) {
        if let Ok(m) = Manifest::from_json_slice(&bytes) {
            return Ok(m);
        }
    }
    let url = format!("{}manifest.json", catalog_base_url());
    let client = http_client()?;
    let bytes = client
        .get(&url)
        .send()
        .with_context(|| format!("fetch manifest {url}"))?
        .error_for_status()
        .with_context(|| format!("manifest HTTP error {url}"))?
        .bytes()
        .context("read manifest body")?;
    let manifest = Manifest::from_json_slice(&bytes)
        .with_context(|| format!("parse manifest from {url}"))?;
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&cache, &bytes); // best-effort cache
    Ok(manifest)
}

/// Progress for the prebuilt path.
pub enum GaiaPrebuiltProgress {
    Downloading { received: u64, total: u64 },
    Verifying,
    Extracting { done: usize, total: usize },
    Complete { files: usize },
    Error(String),
    /// Starting tier `index+1` of `n_tiers` (density label for the UI).
    Tier { density: u32, index: usize, n_tiers: usize },
}

/// Tiers with `density <= target_density` that are not already installed,
/// ascending by density (base first). `installed_dirs` are the `tier_<d>` dir
/// names already on disk.
fn tiers_to_fetch(
    manifest: &Manifest,
    installed_dirs: &[String],
    target_density: u32,
) -> Vec<ManifestTier> {
    let mut tiers: Vec<ManifestTier> = manifest
        .tiers
        .iter()
        .filter(|t| t.density <= target_density && !installed_dirs.iter().any(|d| d == &t.dir))
        .cloned()
        .collect();
    tiers.sort_by_key(|t| t.density);
    tiers
}

/// Download the additive density tiers up to `target_density` into
/// `catalogs/smac_gaia/tier_<d>/`. Fetches the manifest, skips already-installed
/// tiers, and per tier: resumable download → SHA-256 verify → extract. Idempotent.
pub fn download_catalog_layers(
    app_data: &Path,
    target_density: u32,
    cancel: Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<PathBuf> {
    let smac_root = app_data.join("catalogs").join("smac_gaia");
    std::fs::create_dir_all(&smac_root)?;

    let manifest = load_or_fetch_manifest(app_data)?;
    // Installed tier dir names (each `tier_<d>/` that holds a real stars.smac).
    let installed: Vec<String> = discover_layers(&smac_root)
        .iter()
        .filter_map(|p| p.file_name()?.to_str().map(|s| s.to_string()))
        .collect();
    let wanted = tiers_to_fetch(&manifest, &installed, target_density);
    if wanted.is_empty() {
        eprintln!("catalog: all tiers up to density {target_density} already installed");
        progress(GaiaPrebuiltProgress::Complete { files: 0 });
        return Ok(smac_root);
    }

    let base = catalog_base_url();
    let client = http_client()?;
    let n_tiers = wanted.len();
    let mut files = 0usize;
    for (index, tier) in wanted.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        progress(GaiaPrebuiltProgress::Tier { density: tier.density, index, n_tiers });

        let zip_url = format!("{base}{}", tier.zip);
        let sha_url = format!("{base}{}", tier.sha256);
        let zip_path = app_data.join(&tier.zip);
        let part_path = app_data.join(format!("{}.part", tier.zip));

        let expected_sha: Option<String> = client
            .get(&sha_url)
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.text().ok())
            .map(|s| s.split_whitespace().next().unwrap_or("").to_lowercase())
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        if expected_sha.is_none() {
            eprintln!("catalog: no .sha256 sidecar at {sha_url} — skipping integrity check");
        }

        if !zip_path.exists() {
            download_resumable(&client, &zip_url, &part_path, &cancel, progress)?;
            std::fs::rename(&part_path, &zip_path).context("finalize tier archive")?;
        }
        if let Some(want) = &expected_sha {
            progress(GaiaPrebuiltProgress::Verifying);
            let got = sha256_file(&zip_path, &cancel)?;
            if &got != want {
                let _ = std::fs::remove_file(&zip_path);
                anyhow::bail!("tier {} checksum mismatch (expected {want}, got {got})", tier.density);
            }
        }
        extract_tier_zip(&zip_path, &smac_root, &cancel, progress)?;
        let _ = std::fs::remove_file(&zip_path);
        if !smac_present(&smac_root.join(&tier.dir)) {
            anyhow::bail!("tier {} archive did not contain {}/stars.smac", tier.density, tier.dir);
        }
        files += 1;
    }
    progress(GaiaPrebuiltProgress::Complete { files });
    Ok(smac_root)
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

/// Extract a `tier_<d>/stars.smac` entry from `zip_path` into
/// `dest_root/tier_<d>/stars.smac`, preserving the `tier_<d>/` prefix.
/// Zip-slip-safe: rejects `..`, absolute, or drive-prefixed components.
fn extract_tier_zip(
    zip_path: &Path,
    dest_root: &Path,
    cancel: &Arc<AtomicBool>,
    progress: &dyn Fn(GaiaPrebuiltProgress),
) -> Result<()> {
    let file = File::open(zip_path).context("open tier archive")?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).context("read tier zip")?;
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
        let raw = entry.name().replace('\\', "/");
        let comps: Vec<&str> = raw.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
        if comps.is_empty()
            || comps.iter().any(|c| *c == ".." || c.contains(':'))
            || raw.starts_with('/')
            || comps.last() != Some(&"stars.smac")
            || comps.len() < 2
        {
            continue;
        }
        // dest_root / tier_<d> / stars.smac  (join only the safe components)
        let mut dest = dest_root.to_path_buf();
        for c in &comps {
            dest.push(c);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).context("create tier dir")?;
        }
        let mut out = File::create(&dest).context("create stars.smac")?;
        std::io::copy(&mut entry, &mut out).context("extract stars.smac")?;
        done += 1;
        progress(GaiaPrebuiltProgress::Extracting { done, total });
    }
    progress(GaiaPrebuiltProgress::Extracting { done, total });
    Ok(())
}

/// Per-tier installed status for the UI (one entry per declared tier).
pub struct TierStatus {
    pub density: u32,
    pub installed: bool,
    pub epoch: f64,
    pub star_count: u64,
    pub size_bytes: u64,
    pub min_fov_deg: f64,
}

/// Merge the declared tiers (manifest) with on-disk installed state. Returns an
/// empty Vec when no manifest is available (offline + never fetched).
pub fn tier_status(app_data: &Path) -> Vec<TierStatus> {
    let manifest = match load_or_fetch_manifest(app_data) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("catalog: no manifest available for status: {e}");
            return Vec::new();
        }
    };
    let smac_root = app_data.join("catalogs").join("smac_gaia");
    manifest
        .tiers
        .iter()
        .map(|t| {
            let dir = smac_root.join(&t.dir);
            let (installed, star_count, epoch) = match solvemyastro::StarCache::open(&dir) {
                Ok(c) => (true, c.star_count(), c.catalog_epoch()),
                Err(_) => (false, 0, manifest.catalog_epoch),
            };
            TierStatus {
                density: t.density,
                installed,
                epoch,
                star_count,
                size_bytes: t.size_bytes,
                min_fov_deg: t.min_fov_deg,
            }
        })
        .collect()
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

    #[test]
    fn base_url_default_and_overrides() {
        std::env::remove_var("ATHENAEUM_CATALOG_BASE_URL");
        std::env::remove_var("ATHENAEUM_STAR_CATALOG_URL");
        std::env::remove_var("ATHENAEUM_GAIA_PREBUILT_URL");
        assert_eq!(catalog_base_url(), "https://artfrom.space/catalogs/");

        std::env::set_var("ATHENAEUM_CATALOG_BASE_URL", "http://localhost:8000/cat");
        assert_eq!(catalog_base_url(), "http://localhost:8000/cat/"); // trailing slash added
        std::env::remove_var("ATHENAEUM_CATALOG_BASE_URL");

        // legacy var points at a .zip → strip filename to a base
        std::env::set_var("ATHENAEUM_STAR_CATALOG_URL", "https://x.example/c/smac_gaia.zip");
        assert_eq!(catalog_base_url(), "https://x.example/c/");
        std::env::remove_var("ATHENAEUM_STAR_CATALOG_URL");
    }

    #[test]
    fn load_manifest_prefers_local_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("catalogs").join("smac_gaia");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"),
            br#"{"version":1,"catalog_epoch":2016.0,"tiers":[
                {"density":500,"zip":"tier_500.zip","sha256":"tier_500.zip.sha256",
                 "dir":"tier_500","size_bytes":1,"min_fov_deg":0.6}]}"#).unwrap();
        // No network used because the cache exists.
        let m = load_or_fetch_manifest(tmp.path()).unwrap();
        assert_eq!(m.tiers.len(), 1);
        assert_eq!(m.tiers[0].density, 500);
    }

    fn mtier(density: u32) -> crate::catalog::manifest::ManifestTier {
        crate::catalog::manifest::ManifestTier {
            density, zip: format!("tier_{density}.zip"), sha256: format!("tier_{density}.zip.sha256"),
            dir: format!("tier_{density}"), size_bytes: 1, min_fov_deg: 0.5,
        }
    }

    #[test]
    fn tiers_to_fetch_selects_le_target_minus_installed() {
        let m = Manifest { version: 1, catalog_epoch: 2016.0,
            tiers: vec![mtier(500), mtier(2000), mtier(5000), mtier(8000)] };
        // target 5000, tier_500 already installed → fetch 2000 + 5000 (not 8000, not 500).
        let got = tiers_to_fetch(&m, &["tier_500".to_string()], 5000);
        let densities: Vec<u32> = got.iter().map(|t| t.density).collect();
        assert_eq!(densities, vec![2000, 5000]);
        // target 500, nothing installed → just the base.
        assert_eq!(tiers_to_fetch(&m, &[], 500).iter().map(|t| t.density).collect::<Vec<_>>(), vec![500]);
        // everything installed → nothing to fetch.
        let all: Vec<String> = (["tier_500","tier_2000","tier_5000","tier_8000"]).iter().map(|s| s.to_string()).collect();
        assert!(tiers_to_fetch(&m, &all, 8000).is_empty());
    }

    #[test]
    fn extract_tier_zip_preserves_prefix_and_is_zipslip_safe() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("tier_500.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let o = SimpleFileOptions::default();
            zw.start_file("tier_500/stars.smac", o).unwrap();
            zw.write_all(b"SMACDATA").unwrap();
            zw.start_file("../evil.smac", o).unwrap(); // zip-slip attempt
            zw.write_all(b"nope").unwrap();
            zw.finish().unwrap();
        }
        let dest = tmp.path().join("smac_gaia");
        extract_tier_zip(&zip_path, &dest, &Arc::new(AtomicBool::new(false)), &|_| {}).unwrap();
        assert_eq!(std::fs::read(dest.join("tier_500").join("stars.smac")).unwrap(), b"SMACDATA");
        assert!(!tmp.path().join("evil.smac").exists(), "zip-slip entry must be rejected");
    }

    #[test]
    fn tier_status_merges_manifest_with_installed() {
        use solvemyastro::{cache::build_cache, StarRecord as SmacRec};
        let tmp = tempfile::tempdir().unwrap();
        let smac_root = tmp.path().join("catalogs").join("smac_gaia");
        std::fs::create_dir_all(&smac_root).unwrap();
        // Cached manifest with two tiers.
        std::fs::write(smac_root.join("manifest.json"),
            br#"{"version":1,"catalog_epoch":2016.0,"tiers":[
                {"density":500,"zip":"tier_500.zip","sha256":"x","dir":"tier_500","size_bytes":10,"min_fov_deg":0.6},
                {"density":2000,"zip":"tier_2000.zip","sha256":"x","dir":"tier_2000","size_bytes":20,"min_fov_deg":0.3}]}"#).unwrap();
        // Install only tier_500 (one real star).
        build_cache(
            vec![SmacRec { ra: 10.0, dec: 20.0, mag: 8.0, pmra_mas_yr: 0.0, pmdec_mas_yr: 0.0 }],
            &smac_root.join("tier_500"), 2016.0, |_| {},
        ).unwrap();

        let st = tier_status(tmp.path());
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].density, 500);
        assert!(st[0].installed);
        assert_eq!(st[0].star_count, 1);
        assert_eq!(st[1].density, 2000);
        assert!(!st[1].installed);
        assert_eq!(st[1].min_fov_deg, 0.3);
    }
}
