//! Prebuilt quad-index download (`solvemyastro` quad index packs).
//!
//! A quad-index pack is one `quads.smaq` file in its own directory under
//! `catalogs/smac_index/<dir>/`. Packs make blind and scale-free solves fast
//! (a lookup in a precomputed table of catalog quads instead of a spiral of
//! catalog cones). They are built once by `catalog-builder publish-index` and
//! hosted next to the star-catalog tiers; end users fetch the packs they want.
//!
//! Remote layout (base URL = [`catalog_base_url`]): `index_manifest.json`,
//! `index_<name>.zip` + `index_<name>.zip.sha256`; each zip holds
//! `<dir>/quads.smaq`. The download mirrors the tier path in
//! [`crate::catalog::gaia_prebuilt`] and shares its helpers: resumable Range
//! download -> SHA-256 verify -> zip-slip-safe extraction -> confirm the
//! solver can open the pack. Idempotent.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use solvemyastro::index::{FORMAT_VERSION, INDEX_FILE};

use crate::catalog::gaia_prebuilt::{
    catalog_base_url, download_resumable, extract_zip_file, fetch_expected_sha, fetch_small,
    http_client, is_safe_filename, sha256_file, GaiaPrebuiltProgress,
};
use crate::catalog::manifest::{IndexManifest, IndexManifestPack};
use crate::plate_solve::layers::{discover_index_packs, index_root};

fn manifest_cache_path(app_data: &Path) -> PathBuf {
    index_root(app_data).join("manifest.json")
}

/// Read the cached `smac_index/manifest.json` if present, else fetch
/// `<base>/index_manifest.json` and cache it. Same policy as the tier
/// manifest: the cache lets status work offline after the first fetch; the
/// fetch uses short timeouts so it fails fast when the host is unreachable.
pub fn load_or_fetch_index_manifest(app_data: &Path) -> Result<IndexManifest> {
    let cache = manifest_cache_path(app_data);
    if let Ok(bytes) = std::fs::read(&cache) {
        if let Ok(m) = IndexManifest::from_json_slice(&bytes) {
            return Ok(m);
        }
    }
    let url = format!("{}index_manifest.json", catalog_base_url());
    let bytes = fetch_small(&url, "index manifest")?;
    let manifest = IndexManifest::from_json_slice(&bytes)
        .with_context(|| format!("parse index manifest from {url}"))?;
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&cache, &bytes); // best-effort cache
    Ok(manifest)
}

/// Progress for the quad-index download.
pub enum IndexPrebuiltProgress {
    /// Starting pack `index+1` of `n_packs`.
    Pack { name: String, index: usize, n_packs: usize },
    Downloading { received: u64, total: u64 },
    Verifying,
    Extracting { done: usize, total: usize },
    Complete { packs: usize },
    Error(String),
}

impl IndexPrebuiltProgress {
    /// Map a shared-helper progress report (download / extract steps) to ours.
    fn from_helper(p: GaiaPrebuiltProgress) -> Option<Self> {
        match p {
            GaiaPrebuiltProgress::Downloading { received, total } => {
                Some(Self::Downloading { received, total })
            }
            GaiaPrebuiltProgress::Verifying => Some(Self::Verifying),
            GaiaPrebuiltProgress::Extracting { done, total } => {
                Some(Self::Extracting { done, total })
            }
            GaiaPrebuiltProgress::Complete { .. }
            | GaiaPrebuiltProgress::Error(_)
            | GaiaPrebuiltProgress::Tier { .. } => None,
        }
    }
}

/// Per-pack installed status for the UI (one entry per declared pack).
#[derive(Clone, Debug)]
pub struct IndexPackStatus {
    pub name: String,
    pub dir: String,
    /// The pack's `quads.smaq` is on disk and the solver can open it.
    pub installed: bool,
    /// The pack's format is one this build reads (`format == FORMAT_VERSION`).
    pub compatible: bool,
    /// Archive size from the manifest; `None` without one.
    pub size_bytes: Option<u64>,
    /// Smallest field (degrees) the pack serves: the manifest value, else the
    /// installed pack's own.
    pub min_fov_deg: f64,
    /// Why an installed file does not open, when it does not.
    pub error: Option<String>,
}

/// Merge the declared packs (manifest) with on-disk installed state.
///
/// With a manifest: one entry per declared pack (installed or not). Without
/// one: one entry per pack directory found on disk, so the UI still shows
/// what is installed offline.
pub fn index_status(app_data: &Path) -> Vec<IndexPackStatus> {
    let root = index_root(app_data);
    match load_or_fetch_index_manifest(app_data) {
        Ok(manifest) => manifest
            .packs
            .iter()
            .map(|p| {
                let (installed, installed_fov, error) = probe_installed(&root.join(&p.dir));
                IndexPackStatus {
                    name: p.name.clone(),
                    dir: p.dir.clone(),
                    installed,
                    compatible: manifest.format == FORMAT_VERSION,
                    size_bytes: Some(p.size_bytes),
                    min_fov_deg: if p.min_fov_deg > 0.0 {
                        p.min_fov_deg
                    } else {
                        installed_fov.unwrap_or(0.0)
                    },
                    error,
                }
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "no index manifest available for status, falling back to disk discovery");
            discover_index_packs(&root)
                .iter()
                .filter_map(|p| {
                    let dir = p.file_name()?.to_str()?.to_string();
                    let (installed, fov, error) = probe_installed(p);
                    Some(IndexPackStatus {
                        name: dir.clone(),
                        dir,
                        installed,
                        // It opened, so it is the current format; an unopenable
                        // file is one this build cannot read.
                        compatible: installed,
                        size_bytes: None,
                        min_fov_deg: fov.unwrap_or(0.0),
                        error,
                    })
                })
                .collect()
        }
    }
}

/// `(opens, min_fov_deg when it opens, why it does not)` for a pack dir. A dir
/// with no `quads.smaq` is simply not installed (no error).
fn probe_installed(dir: &Path) -> (bool, Option<f64>, Option<String>) {
    if !dir.join(INDEX_FILE).is_file() {
        return (false, None, None);
    }
    match solvemyastro::QuadIndex::open(dir) {
        Ok(ix) => (true, Some(ix.min_fov_deg()), None),
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "installed quad index pack does not open");
            (false, None, Some(format!("{e:#}")))
        }
    }
}

/// Validate a server-supplied manifest pack before any filesystem or network
/// access: its path-like fields are joined into local paths.
fn validate_pack(pack: &IndexManifestPack, format: u32) -> Result<()> {
    for (field, val) in [
        ("name", &pack.name),
        ("zip", &pack.zip),
        ("sha256", &pack.sha256),
        ("dir", &pack.dir),
    ] {
        if val.is_empty() || !is_safe_filename(val) {
            anyhow::bail!(
                "quad index: manifest pack {} has unsafe {field} field {:?} — rejected",
                pack.name,
                val
            );
        }
    }
    if format != FORMAT_VERSION {
        anyhow::bail!(
            "quad index pack {} uses index format {format}, but this version of Athenaeum reads \
             format {FORMAT_VERSION}; update Athenaeum to install it",
            pack.name
        );
    }
    Ok(())
}

/// Extract `zip_path` into `root` and confirm the solver opens the pack at
/// `root/<dir>`. A pack that does not open is deleted, so a bad download never
/// stays installed.
fn install_zip(
    zip_path: &Path,
    root: &Path,
    pack: &IndexManifestPack,
    cancel: &Arc<AtomicBool>,
    progress: &dyn Fn(IndexPrebuiltProgress),
) -> Result<()> {
    extract_zip_file(zip_path, root, INDEX_FILE, cancel, &|p| {
        if let Some(p) = IndexPrebuiltProgress::from_helper(p) {
            progress(p);
        }
    })?;
    let dir = root.join(&pack.dir);
    if !dir.join(INDEX_FILE).is_file() {
        anyhow::bail!("quad index pack {} archive did not contain {}/{INDEX_FILE}", pack.name, pack.dir);
    }
    if let Err(e) = solvemyastro::QuadIndex::open(&dir) {
        if let Err(rm) = std::fs::remove_file(dir.join(INDEX_FILE)) {
            tracing::warn!(path = %dir.display(), error = %rm, "removing the unopenable quad index pack failed");
        }
        anyhow::bail!("quad index pack {} was installed but does not open: {e:#}", pack.name);
    }
    Ok(())
}

/// Download the named quad-index packs into `catalogs/smac_index/<dir>/`.
/// Fetches the manifest, skips packs the solver already opens, and per pack:
/// resumable download -> SHA-256 verify -> extract -> confirm it opens.
/// Idempotent. Refuses a pack whose format this build cannot read.
pub fn download_index_packs(
    app_data: &Path,
    names: &[String],
    cancel: Arc<AtomicBool>,
    progress: &dyn Fn(IndexPrebuiltProgress),
) -> Result<PathBuf> {
    let root = index_root(app_data);
    std::fs::create_dir_all(&root)?;

    let manifest = load_or_fetch_index_manifest(app_data)?;
    let mut wanted = Vec::new();
    for name in names {
        let pack = manifest
            .packs
            .iter()
            .find(|p| &p.name == name)
            .with_context(|| format!("quad index pack {name:?} is not in the manifest"))?;
        validate_pack(pack, manifest.format)?;
        if wanted.iter().any(|w: &&IndexManifestPack| w.name == pack.name) {
            continue;
        }
        if probe_installed(&root.join(&pack.dir)).0 {
            continue;
        }
        wanted.push(pack);
    }
    if wanted.is_empty() {
        tracing::info!(requested = names.len(), "all requested quad index packs already installed");
        progress(IndexPrebuiltProgress::Complete { packs: 0 });
        return Ok(root);
    }

    let base = catalog_base_url();
    let client = http_client()?;
    let n_packs = wanted.len();
    let mut installed = 0usize;
    for (index, pack) in wanted.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        progress(IndexPrebuiltProgress::Pack { name: pack.name.clone(), index, n_packs });

        let zip_url = format!("{base}{}", pack.zip);
        let sha_url = format!("{base}{}", pack.sha256);
        let zip_path = app_data.join(&pack.zip);
        let part_path = app_data.join(format!("{}.part", pack.zip));

        let expected_sha = fetch_expected_sha(&client, &sha_url);
        if expected_sha.is_none() {
            tracing::warn!(sha_url, "no .sha256 sidecar, skipping integrity check");
        }
        let helper_progress = |p: GaiaPrebuiltProgress| {
            if let Some(p) = IndexPrebuiltProgress::from_helper(p) {
                progress(p);
            }
        };
        if !zip_path.exists() {
            download_resumable(&client, &zip_url, &part_path, &cancel, &helper_progress)?;
            std::fs::rename(&part_path, &zip_path).context("finalize index archive")?;
        }
        if let Some(want) = &expected_sha {
            progress(IndexPrebuiltProgress::Verifying);
            let got = sha256_file(&zip_path, &cancel)?;
            if &got != want {
                let _ = std::fs::remove_file(&zip_path);
                anyhow::bail!(
                    "quad index pack {} checksum mismatch (expected {want}, got {got})",
                    pack.name
                );
            }
        }
        let outcome = install_zip(&zip_path, &root, pack, &cancel, progress);
        let _ = std::fs::remove_file(&zip_path);
        outcome?;
        installed += 1;
    }
    progress(IndexPrebuiltProgress::Complete { packs: installed });
    Ok(root)
}

/// One row of `get_index_status` (camelCase on the wire; both hosts return
/// this exact shape).
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatusInfo {
    pub name: String,
    pub dir: String,
    pub installed: bool,
    pub compatible: bool,
    /// Archive size; `null` when no manifest is reachable.
    pub size_bytes: Option<u64>,
    pub min_fov_deg: f64,
    /// Why an installed file does not open; `null` otherwise.
    pub error: Option<String>,
}

impl From<IndexPackStatus> for IndexStatusInfo {
    fn from(s: IndexPackStatus) -> Self {
        IndexStatusInfo {
            name: s.name,
            dir: s.dir,
            installed: s.installed,
            compatible: s.compatible,
            size_bytes: s.size_bytes,
            min_fov_deg: s.min_fov_deg,
            error: s.error,
        }
    }
}

/// Payload of the `index-download-progress` event (camelCase on the wire).
/// `phase` is one of `pack`, `downloading`, `verifying`, `extracting`,
/// `complete`, `error`.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDownloadProgress {
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub pack_name: String,
    pub pack_index: usize,
    pub n_packs: usize,
    /// The failure text, on `phase == "error"` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Turns [`IndexPrebuiltProgress`] reports into [`IndexDownloadProgress`]
/// events, remembering which pack is current (the shared download helpers do
/// not know it). Shared by both hosts so the event is identical.
#[derive(Default)]
pub struct IndexProgressMapper {
    cur: std::sync::Mutex<(String, usize, usize)>, // (pack name, index, n_packs)
}

impl IndexProgressMapper {
    pub fn new() -> Self {
        Self::default()
    }

    fn event(&self, phase: &str, current: usize, total: usize, percent: f64, message: Option<String>) -> IndexDownloadProgress {
        let (name, index, n) = self.cur.lock().unwrap_or_else(|e| e.into_inner()).clone();
        IndexDownloadProgress {
            phase: phase.to_string(),
            current,
            total,
            percent,
            pack_name: name,
            pack_index: index,
            n_packs: n,
            message,
        }
    }

    /// The event for one progress report.
    pub fn map(&self, p: IndexPrebuiltProgress) -> IndexDownloadProgress {
        match p {
            IndexPrebuiltProgress::Pack { name, index, n_packs } => {
                *self.cur.lock().unwrap_or_else(|e| e.into_inner()) = (name, index, n_packs);
                self.event("pack", index, n_packs, 0.0, None)
            }
            IndexPrebuiltProgress::Downloading { received, total } => self.event(
                "downloading",
                received as usize,
                total as usize,
                if total > 0 { received as f64 / total as f64 * 100.0 } else { 0.0 },
                None,
            ),
            IndexPrebuiltProgress::Verifying => self.event("verifying", 0, 0, 0.0, None),
            IndexPrebuiltProgress::Extracting { done, total } => {
                self.event("extracting", done, total, 0.0, None)
            }
            IndexPrebuiltProgress::Complete { packs } => {
                self.event("complete", packs, packs, 100.0, None)
            }
            IndexPrebuiltProgress::Error(m) => self.error(m),
        }
    }

    /// The `error` event for a failed download (the core reports failure as
    /// the call's `Err`, so the host emits this itself).
    pub fn error(&self, message: String) -> IndexDownloadProgress {
        self.event("error", 0, 0, 0.0, Some(message))
    }
}

/// Build a tiny real quad index (one coarse band over a synthetic sky) into
/// `out_dir/quads.smaq`, for tests that need a file the solver opens.
#[cfg(test)]
pub(crate) fn build_tiny_index_for_tests(out_dir: &Path) {
    use solvemyastro::index::{build_index, BuildParams};
    use solvemyastro::{cache::build_cache, StarCache, StarRecord};
    let mut s = 7u64;
    let mut rnd = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    let stars: Vec<StarRecord> = (0..8_000)
        .map(|_| StarRecord {
            ra: rnd() * 360.0,
            dec: (rnd() * 2.0 - 1.0).asin().to_degrees(),
            mag: (4.0 + rnd() * 10.0) as f32,
            pmra_mas_yr: 0.0,
            pmdec_mas_yr: 0.0,
        })
        .collect();
    let cat = tempfile::tempdir().unwrap();
    build_cache(stars, cat.path(), 2016.0, |_| {}).unwrap();
    let cache = StarCache::open(cat.path()).unwrap();
    let params = BuildParams { min_depth: 2, max_depth: 2, stars_per_cell: 10, quads_per_cell: 8 };
    build_index(&[&cache], out_dir, &params, |_| {}).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::gaia_prebuilt::ENV_LOCK;

    fn pack(name: &str) -> IndexManifestPack {
        IndexManifestPack {
            name: name.to_string(),
            zip: format!("index_{name}.zip"),
            sha256: format!("index_{name}.zip.sha256"),
            dir: name.to_string(),
            size_bytes: 1234,
            min_fov_deg: 1.3,
        }
    }

    fn write_manifest(app: &Path, format: u32, packs: &[IndexManifestPack]) {
        let root = index_root(app);
        std::fs::create_dir_all(&root).unwrap();
        let m = IndexManifest { version: 1, format, catalog_epoch: 2016.0, packs: packs.to_vec() };
        std::fs::write(root.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    }

    fn zip_pack(zip_path: &Path, dir: &str, content: &[u8]) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let mut zw = zip::ZipWriter::new(std::fs::File::create(zip_path).unwrap());
        zw.start_file(format!("{dir}/quads.smaq"), SimpleFileOptions::default()).unwrap();
        zw.write_all(content).unwrap();
        zw.finish().unwrap();
    }

    #[test]
    fn status_without_a_manifest_lists_the_installed_dirs() {
        // Force the remote fetch to fail (RFC 2606 `.invalid` never resolves)
        // so the disk fallback is what is under test.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ATHENAEUM_CATALOG_BASE_URL", "http://catalog.invalid/");
        let app = tempfile::tempdir().unwrap();
        let root = index_root(app.path());
        build_tiny_index_for_tests(&root.join("base"));
        std::fs::create_dir_all(root.join("narrow")).unwrap();
        std::fs::write(root.join("narrow").join("quads.smaq"), b"damaged").unwrap();

        let st = index_status(app.path());
        std::env::remove_var("ATHENAEUM_CATALOG_BASE_URL");
        assert_eq!(st.len(), 2, "{st:?}");
        assert_eq!(st[0].name, "base");
        assert!(st[0].installed && st[0].compatible && st[0].error.is_none());
        assert_eq!(st[0].size_bytes, None);
        assert!(st[0].min_fov_deg > 0.0, "the installed pack's own value");
        assert_eq!(st[1].name, "narrow");
        assert!(!st[1].installed && !st[1].compatible);
        assert!(st[1].error.as_deref().unwrap_or("").contains("not a quad index"), "{:?}", st[1]);
    }

    #[test]
    fn status_merges_the_manifest_with_what_is_installed() {
        let app = tempfile::tempdir().unwrap();
        write_manifest(app.path(), FORMAT_VERSION, &[pack("base"), pack("narrow")]);
        build_tiny_index_for_tests(&index_root(app.path()).join("base"));
        let st = index_status(app.path());
        assert_eq!(st.len(), 2);
        assert!(st[0].installed && st[0].compatible);
        assert_eq!(st[0].size_bytes, Some(1234));
        assert_eq!(st[0].min_fov_deg, 1.3, "the manifest value wins");
        assert!(!st[1].installed);
        assert_eq!(st[1].error, None);

        // A manifest of another format marks every pack incompatible.
        write_manifest(app.path(), FORMAT_VERSION + 1, &[pack("base")]);
        assert!(!index_status(app.path())[0].compatible);
    }

    #[test]
    fn download_rejects_unsafe_unknown_and_incompatible_packs() {
        let app = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut evil = pack("evil");
        evil.dir = "../evil".to_string();
        write_manifest(app.path(), FORMAT_VERSION, &[evil, pack("base")]);
        // Validation fires before any network access, so the base URL is moot.
        let err = download_index_packs(app.path(), &["evil".into()], cancel.clone(), &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsafe"), "{err}");
        let err = download_index_packs(app.path(), &["nope".into()], cancel.clone(), &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("not in the manifest"), "{err}");

        write_manifest(app.path(), FORMAT_VERSION + 1, &[pack("base")]);
        let err = download_index_packs(app.path(), &["base".into()], cancel, &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("update Athenaeum"), "{err}");
    }

    #[test]
    fn download_of_an_installed_pack_is_a_no_op() {
        let app = tempfile::tempdir().unwrap();
        write_manifest(app.path(), FORMAT_VERSION, &[pack("base")]);
        build_tiny_index_for_tests(&index_root(app.path()).join("base"));
        let done = std::sync::Mutex::new(None);
        let root = download_index_packs(
            app.path(),
            &["base".into()],
            Arc::new(AtomicBool::new(false)),
            &|p| {
                if let IndexPrebuiltProgress::Complete { packs } = p {
                    *done.lock().unwrap() = Some(packs);
                }
            },
        )
        .unwrap();
        assert_eq!(root, index_root(app.path()));
        assert_eq!(*done.lock().unwrap(), Some(0), "nothing to fetch, no network used");
    }

    #[test]
    fn install_zip_extracts_and_confirms_the_pack_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("built");
        build_tiny_index_for_tests(&src);
        let cancel = Arc::new(AtomicBool::new(false));
        let p = pack("base");

        let good = tmp.path().join("good.zip");
        zip_pack(&good, "base", &std::fs::read(src.join(INDEX_FILE)).unwrap());
        let root = tmp.path().join("smac_index");
        install_zip(&good, &root, &p, &cancel, &|_| {}).unwrap();
        assert!(solvemyastro::QuadIndex::open(&root.join("base")).is_ok());

        // A pack that extracts but does not open is removed and refused.
        let bad = tmp.path().join("bad.zip");
        zip_pack(&bad, "base", b"garbage");
        let root2 = tmp.path().join("smac_index2");
        let err = install_zip(&bad, &root2, &p, &cancel, &|_| {}).unwrap_err().to_string();
        assert!(err.contains("does not open"), "{err}");
        assert!(!root2.join("base").join(INDEX_FILE).exists(), "a bad pack is not left installed");

        // An archive without the file is refused too.
        let empty = tmp.path().join("empty.zip");
        zip_pack(&empty, "other", b"x");
        let err = install_zip(&empty, &tmp.path().join("r3"), &p, &cancel, &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not contain"), "{err}");
    }

    #[test]
    fn the_progress_mapper_tracks_the_current_pack() {
        let m = IndexProgressMapper::new();
        let e = m.map(IndexPrebuiltProgress::Pack { name: "narrow".into(), index: 1, n_packs: 2 });
        assert_eq!((e.phase.as_str(), e.current, e.total), ("pack", 1, 2));
        let e = m.map(IndexPrebuiltProgress::Downloading { received: 50, total: 200 });
        assert_eq!((e.pack_name.as_str(), e.pack_index, e.n_packs), ("narrow", 1, 2));
        assert_eq!(e.percent, 25.0);
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["packName"], "narrow");
        assert!(json.get("message").is_none(), "no message outside an error");
        let e = m.error("boom".into());
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!((json["phase"].as_str(), json["message"].as_str()), (Some("error"), Some("boom")));
        let e = m.map(IndexPrebuiltProgress::Complete { packs: 2 });
        assert_eq!((e.phase.as_str(), e.percent), ("complete", 100.0));
        // The status row is camelCase too.
        let row = IndexStatusInfo {
            name: "base".into(), dir: "base".into(), installed: false, compatible: true,
            size_bytes: None, min_fov_deg: 1.3, error: None,
        };
        let json = serde_json::to_value(&row).unwrap();
        assert!(json["sizeBytes"].is_null() && json["minFovDeg"] == 1.3);
    }
}
