# Plan 3 — Catalog Delivery (App) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the app download the additive density-tier star catalog it needs
(selected by field-of-view) from a catalog server into
`<app-data>/catalogs/smac_gaia/tier_<d>/`, report per-tier status, and replace the
obsolete single-`smac_gaia.zip` download path.

**Architecture:** Generalize `catalog/gaia_prebuilt.rs` from one-zip to a
manifest-driven tier downloader (reusing its private `http_client` /
`download_resumable` / `sha256_file` / `smac_present` helpers). The manifest model
moves to `athenaeum-core::catalog::manifest` (shared with `catalog-builder`).
`download_catalog_layers` + a generalized per-tier `get_catalog_status` get a
Tauri command **and** a mirrored Axum route. The frontend grows a FOV→tier helper
in `PlateSolveSettingsPanel`. The legacy single-zip path is removed last.

**Tech Stack:** Rust (`athenaeum-core`, `catalog-builder`, `athenaeum-tauri`,
`athenaeum-web`), `reqwest` (blocking), `zip`, `sha2`, `serde`; React/TS frontend
(design tokens, `lucide-react`).

**Spec:** `docs/superpowers/specs/2026-06-30-plan3-catalog-delivery-design.md`
(refines §4/§5 of `2026-06-29-tiered-additive-star-catalog-design.md`).

## Global Constraints

- **Two backends in sync.** Any Tauri command (`crates/athenaeum-tauri/src/commands/plate_solve.rs`)
  needs the matching Axum route (`crates/athenaeum-web/src/routes/plate_solve.rs`)
  in the same change; real logic lives in `athenaeum-core`.
- **Serde boundary.** `CatalogStatusInfo` and `CatalogDownloadProgress` cross the
  IPC boundary; keep the Rust structs and the `src/types/plate-solve.ts` mirror in
  lockstep (match the existing casing — `CatalogStatusInfo` is snake_case with no
  `rename_all`; `CatalogDownloadProgress` has `rename_all = "camelCase"`).
- **No `@tauri-apps/*` outside `src/api/`.** Frontend goes through the `api` object.
- **Design tokens, not raw colours** (`bg-surface`, `text-content-muted`, …).
- **Never swallow errors** — log to stderr/console before returning.
- **Don't name external tools** in code/comments.
- **Registration is out of scope** — do not touch `registration/service.rs` or the
  registration routes; leave `smac_gaia_bright` resolution there alone.
- **Catalog base URL:** `ATHENAEUM_CATALOG_BASE_URL` (default
  `https://artfrom.space/catalogs/`); legacy `ATHENAEUM_STAR_CATALOG_URL` /
  `ATHENAEUM_GAIA_PREBUILT_URL` accepted (strip trailing filename to a base).
- **First-run default target density:** `2000` (base + Δ1).
- **Client install layout:** `<app-data>/catalogs/smac_gaia/tier_<d>/stars.smac`.

## File Structure

- Create `crates/athenaeum-core/src/catalog/manifest.rs` — the shared manifest
  model + parse.
- Modify `crates/athenaeum-core/src/catalog/mod.rs` — register `manifest`.
- Modify `crates/catalog-builder/src/publish.rs` — import the shared model.
- Modify `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs` — base-URL resolver,
  `extract_tier_zip`, `download_catalog_layers`, `tier status`, new progress
  variants; (last task) remove the single-zip path.
- Modify `crates/athenaeum-tauri/src/commands/plate_solve.rs` +
  `crates/athenaeum-web/src/routes/plate_solve.rs` — `download_catalog_layers`
  command/route, generalized `get_catalog_status`, extended structs.
- Modify `crates/athenaeum-tauri/src/lib.rs` + `crates/athenaeum-web/src/routes/mod.rs`
  — register the new command/route.
- Modify `src/types/plate-solve.ts` — extended types.
- Create `src/components/plate-solve/cameraPresets.ts` — static rig presets.
- Modify `src/components/plate-solve/PlateSolveSettingsPanel.tsx` — FOV helper +
  per-tier table; `src/components/plate-solve/PlateSolveIndexMissingModal.tsx` —
  default-set download.

---

### Task 1: Shared manifest model in `athenaeum-core::catalog::manifest`

**Files:**
- Create: `crates/athenaeum-core/src/catalog/manifest.rs`
- Modify: `crates/athenaeum-core/src/catalog/mod.rs:1-6` (module list)
- Modify: `crates/catalog-builder/src/publish.rs:10-31` (remove local structs, import)
- Test: inline in `manifest.rs`

**Interfaces:**
- Produces: `pub struct Manifest { pub version: u32, pub catalog_epoch: f64, pub tiers: Vec<ManifestTier> }`
  and `pub struct ManifestTier { pub density: u32, pub zip: String, pub sha256: String, pub dir: String, pub size_bytes: u64, pub min_fov_deg: f64 }`,
  both `#[derive(Clone, Debug, Serialize, Deserialize)]`; plus
  `impl Manifest { pub fn from_json_slice(bytes: &[u8]) -> anyhow::Result<Self> }`.

- [ ] **Step 1: Write the failing test** (`manifest.rs`)

```rust
//! Shared `manifest.json` model for the density-tier catalog (written by
//! `catalog-builder`, read by the app's download path).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestTier {
    pub density: u32,
    pub zip: String,
    pub sha256: String,
    pub dir: String,
    pub size_bytes: u64,
    pub min_fov_deg: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub catalog_epoch: f64,
    pub tiers: Vec<ManifestTier>,
}

impl Manifest {
    /// Parse a `manifest.json` byte slice.
    pub fn from_json_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_manifest_shape() {
        let json = br#"{
          "version": 1, "catalog_epoch": 2016.0,
          "tiers": [
            {"density":500,"zip":"tier_500.zip","sha256":"tier_500.zip.sha256",
             "dir":"tier_500","size_bytes":578617584,"min_fov_deg":0.6},
            {"density":2000,"zip":"tier_2000.zip","sha256":"tier_2000.zip.sha256",
             "dir":"tier_2000","size_bytes":1733296370,"min_fov_deg":0.3}
          ]
        }"#;
        let m = Manifest::from_json_slice(json).unwrap();
        assert_eq!(m.version, 1);
        assert_eq!(m.tiers.len(), 2);
        assert_eq!(m.tiers[0].density, 500);
        assert_eq!(m.tiers[1].min_fov_deg, 0.3);
        // round-trips
        let back = Manifest::from_json_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back.tiers[0].zip, "tier_500.zip");
    }
}
```

- [ ] **Step 2: Register the module.** In `crates/athenaeum-core/src/catalog/mod.rs`, add to the module list (after `pub mod healpix;`):

```rust
pub mod manifest;
```

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p athenaeum-core manifest::`
Expected: PASS (`parses_real_manifest_shape`).

- [ ] **Step 4: Point `catalog-builder` at the shared model.** In
  `crates/catalog-builder/src/publish.rs`, delete the local `ManifestTier` and
  `Manifest` structs (the two `#[derive(Serialize)] struct …` blocks) and import
  instead. Replace the `use serde::Serialize;` line with:

```rust
use athenaeum_core::catalog::manifest::{Manifest, ManifestTier};
```

(`min_fov_for`, `sha256_file`, `zip_tier`, `package_publish` stay as-is — they now
construct the imported structs, whose fields are identical.)

- [ ] **Step 5: Build + test both crates**

Run: `cargo test -p athenaeum-core manifest:: && cargo test -p catalog-builder`
Expected: PASS (catalog-builder's `produces_zip_sha_and_manifest_per_tier` still passes).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/catalog/manifest.rs crates/athenaeum-core/src/catalog/mod.rs crates/catalog-builder/src/publish.rs
git commit -m "refactor(catalog): shared manifest model in athenaeum-core::catalog::manifest"
```

---

### Task 2: Base-URL resolver + manifest fetch/cache

**Files:**
- Modify: `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs` (add functions near `prebuilt_urls`)
- Test: inline in `gaia_prebuilt.rs` tests module

**Interfaces:**
- Consumes: `catalog::manifest::Manifest` (Task 1), existing `http_client()`.
- Produces:
  - `pub fn catalog_base_url() -> String` — trailing-slash-normalized base.
  - `fn manifest_cache_path(app_data: &Path) -> PathBuf` — `catalogs/smac_gaia/manifest.json`.
  - `pub fn load_or_fetch_manifest(app_data: &Path) -> anyhow::Result<Manifest>` —
    cached local file if present, else fetch from `<base>/manifest.json` and cache it.

- [ ] **Step 1: Write the failing test** (in `gaia_prebuilt.rs` `mod tests`)

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core base_url_default_and_overrides`
Expected: FAIL — `catalog_base_url` not defined.

- [ ] **Step 3: Implement** (add to `gaia_prebuilt.rs`, near `prebuilt_urls`; add `use crate::catalog::manifest::Manifest;` at the top of the file):

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core base_url_default_and_overrides load_manifest_prefers_local_cache`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/catalog/gaia_prebuilt.rs
git commit -m "feat(catalog): catalog_base_url + load_or_fetch_manifest (cached)"
```

---

### Task 3: `extract_tier_zip` (tier-prefix, zip-slip safe)

**Files:**
- Modify: `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs` (add `extract_tier_zip`)
- Test: inline

**Interfaces:**
- Produces: `fn extract_tier_zip(zip_path: &Path, dest_root: &Path, cancel: &Arc<AtomicBool>, progress: &dyn Fn(GaiaPrebuiltProgress)) -> Result<()>`
  — extracts the single `tier_<d>/stars.smac` entry to `dest_root/tier_<d>/stars.smac`.

- [ ] **Step 1: Write the failing test** (in `mod tests`)

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core extract_tier_zip_preserves_prefix`
Expected: FAIL — `extract_tier_zip` not defined.

- [ ] **Step 3: Implement** (in `gaia_prebuilt.rs`)

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p athenaeum-core extract_tier_zip_preserves_prefix`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/catalog/gaia_prebuilt.rs
git commit -m "feat(catalog): extract_tier_zip (tier-prefix, zip-slip safe)"
```

---

### Task 4: `download_catalog_layers` + tier selection + progress variant

**Files:**
- Modify: `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs` (progress variant, `tiers_to_fetch`, `download_catalog_layers`)
- Test: inline (`tiers_to_fetch` is pure → unit-tested; the full download is integration, run in Task 8)

**Interfaces:**
- Consumes: `load_or_fetch_manifest` (Task 2), `extract_tier_zip` (Task 3),
  existing `download_resumable`, `sha256_file`, `smac_present`,
  `plate_solve::layers::discover_layers`.
- Produces:
  - `GaiaPrebuiltProgress::Tier { density: u32, index: usize, n_tiers: usize }` (new variant).
  - `fn tiers_to_fetch(manifest: &Manifest, installed_dirs: &[String], target_density: u32) -> Vec<ManifestTier>`.
  - `pub fn download_catalog_layers(app_data: &Path, target_density: u32, cancel: Arc<AtomicBool>, progress: &dyn Fn(GaiaPrebuiltProgress)) -> Result<PathBuf>`.

- [ ] **Step 1: Add the progress variant.** In `gaia_prebuilt.rs`, add to `enum GaiaPrebuiltProgress`:

```rust
    /// Starting tier `index+1` of `n_tiers` (density label for the UI).
    Tier { density: u32, index: usize, n_tiers: usize },
```

- [ ] **Step 2: Write the failing test for tier selection** (`mod tests`)

```rust
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p athenaeum-core tiers_to_fetch_selects`
Expected: FAIL — `tiers_to_fetch` not defined.

- [ ] **Step 4: Implement** (in `gaia_prebuilt.rs`; add `use crate::catalog::manifest::ManifestTier;` and `use crate::plate_solve::layers::discover_layers;`):

```rust
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
```

- [ ] **Step 5: Run the unit test + build**

Run: `cargo test -p athenaeum-core tiers_to_fetch_selects && cargo build -p athenaeum-core`
Expected: PASS + compiles. (The full download is exercised end-to-end in Task 8.)

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/catalog/gaia_prebuilt.rs
git commit -m "feat(catalog): download_catalog_layers (FOV-target tier set, resumable+verify)"
```

---

### Task 5: Per-tier catalog status in core

**Files:**
- Modify: `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs` (add `tier_status` + `TierStatus`)
- Test: inline (uses a tiny real `stars.smac` built with `solvemyastro::cache::build_cache`)

**Interfaces:**
- Consumes: `load_or_fetch_manifest`, `discover_layers`, `solvemyastro::StarCache`.
- Produces:
  - `pub struct TierStatus { pub density: u32, pub installed: bool, pub epoch: f64, pub star_count: u64, pub size_bytes: u64, pub min_fov_deg: f64 }`
  - `pub fn tier_status(app_data: &Path) -> Vec<TierStatus>` — one entry per declared
    tier (manifest order), installed flag + star_count from the on-disk cache.

- [ ] **Step 1: Write the failing test** (`mod tests`; reuse the crate's `solvemyastro` dep)

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core tier_status_merges_manifest`
Expected: FAIL — `tier_status` not defined.

- [ ] **Step 3: Implement** (in `gaia_prebuilt.rs`)

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p athenaeum-core tier_status_merges_manifest`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/catalog/gaia_prebuilt.rs
git commit -m "feat(catalog): tier_status (per-tier installed + manifest merge)"
```

---

### Task 6: Commands — `download_catalog_layers` + generalized `get_catalog_status` (both backends)

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/plate_solve.rs` (struct fields, `get_catalog_status`, new `download_catalog_layers` command)
- Modify: `crates/athenaeum-tauri/src/lib.rs` (register command)
- Modify: `crates/athenaeum-web/src/routes/plate_solve.rs` (mirror)
- Modify: `crates/athenaeum-web/src/routes/mod.rs` (register route)
- Modify: `src/types/plate-solve.ts` (extended types)

**Interfaces:**
- Consumes: `athenaeum_core::catalog::gaia_prebuilt::{download_catalog_layers, tier_status, TierStatus}`.
- Produces: Tauri command `download_catalog_layers(target_density: u32)` + Axum
  route `/api/download_catalog_layers`; both emit `catalog-download-progress`.
  `get_catalog_status` returns `Vec<CatalogStatusInfo>` (now per tier).

- [ ] **Step 1: Extend the shared structs (Tauri).** In
  `crates/athenaeum-tauri/src/commands/plate_solve.rs`, replace the
  `CatalogStatusInfo` struct with per-tier fields:

```rust
#[derive(Clone, Serialize)]
pub struct CatalogStatusInfo {
    pub name: String,
    pub density: u32,
    pub installed: bool,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub size_bytes: u64,
    pub min_fov_deg: f64,
    pub mag_limit: f32,
}
```

and extend `CatalogDownloadProgress` with the optional tier context:

```rust
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDownloadProgress {
    phase: String,
    current: usize,
    total: usize,
    percent: f64,
    tier_density: u32,
    tier_index: usize,
    n_tiers: usize,
}
```

- [ ] **Step 2: Rewrite `get_catalog_status` (Tauri)** to map `tier_status`:

```rust
#[tauri::command]
pub async fn get_catalog_status(
    state: State<'_, AppState>,
) -> Result<Vec<CatalogStatusInfo>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let app_data = db.path().to_path_buf().parent()
        .ok_or("Cannot determine app data directory")?.to_path_buf();
    let rows = athenaeum_core::catalog::gaia_prebuilt::tier_status(&app_data);
    Ok(rows.into_iter().map(|t| CatalogStatusInfo {
        name: format!("Gaia tier {} (≤{:.2}° FOV)", t.density, t.min_fov_deg),
        density: t.density,
        installed: t.installed,
        epoch: t.epoch,
        star_count_approx: t.star_count,
        size_bytes: t.size_bytes,
        min_fov_deg: t.min_fov_deg,
        mag_limit: 19.0,
    }).collect())
}
```

- [ ] **Step 3: Add the `download_catalog_layers` command (Tauri)** next to the
  old download command. Track the current tier in the progress closure:

```rust
#[tauri::command]
pub async fn download_catalog_layers(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    target_density: u32,
) -> Result<String, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let app_data_dir = db.path().to_path_buf().parent()
        .ok_or("Cannot determine app data directory")?.to_path_buf();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let app_clone = app.clone();

    let result = tokio::task::spawn_blocking(move || {
        use athenaeum_core::catalog::gaia_prebuilt::GaiaPrebuiltProgress as P;
        let cur = std::sync::Mutex::new((0u32, 0usize, 0usize)); // (density, index, n_tiers)
        athenaeum_core::catalog::gaia_prebuilt::download_catalog_layers(
            &app_data_dir,
            target_density,
            cancel_flag,
            &|progress| {
                let (td, ti, nt) = *cur.lock().unwrap();
                let event = match progress {
                    P::Tier { density, index, n_tiers } => {
                        *cur.lock().unwrap() = (density, index, n_tiers);
                        CatalogDownloadProgress { phase: "tier".into(), current: index, total: n_tiers,
                            percent: 0.0, tier_density: density, tier_index: index, n_tiers }
                    }
                    P::Downloading { received, total } => CatalogDownloadProgress {
                        phase: "downloading".into(), current: received as usize, total: total as usize,
                        percent: if total > 0 { received as f64 / total as f64 * 100.0 } else { 0.0 },
                        tier_density: td, tier_index: ti, n_tiers: nt },
                    P::Verifying => CatalogDownloadProgress { phase: "verifying".into(), current: 0, total: 0,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt },
                    P::Extracting { done, total } => CatalogDownloadProgress { phase: "extracting".into(),
                        current: done, total, percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt },
                    P::Complete { files } => CatalogDownloadProgress { phase: "complete".into(), current: files,
                        total: files, percent: 100.0, tier_density: td, tier_index: ti, n_tiers: nt },
                    P::Error(_) => CatalogDownloadProgress { phase: "error".into(), current: 0, total: 0,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt },
                };
                let _ = app_clone.emit("catalog-download-progress", event);
            },
        )
    })
    .await
    .map_err(|e| format!("download task panicked: {e}"))?;

    result.map(|p| p.display().to_string()).map_err(|e| e.to_string())
}
```

- [ ] **Step 4: Register the Tauri command.** In `crates/athenaeum-tauri/src/lib.rs`,
  in the `invoke_handler` list (next to `commands::get_catalog_status,`):

```rust
            commands::download_catalog_layers,
```

- [ ] **Step 5: Mirror in the web backend.** Apply Steps 1–3 to
  `crates/athenaeum-web/src/routes/plate_solve.rs` with the Axum shapes: the
  `CatalogStatusInfo` struct (same fields), the generalized `get_catalog_status`
  (`Json(rows)` of the mapped `tier_status`), and a `download_catalog_layers`
  route taking `Json(TargetDensityArgs { target_density: u32 })`, using
  `state.event_tx.send(SseEvent { event_name: "catalog-download-progress", … })`
  in the progress closure (mirror the existing `download_gaia_dr3_prebuilt_catalog`
  route's SSE emit). Then register it in `crates/athenaeum-web/src/routes/mod.rs`:

```rust
        .route("/api/download_catalog_layers", post(plate_solve::download_catalog_layers))
```

- [ ] **Step 6: Update the TS types.** In `src/types/plate-solve.ts`, replace
  `CatalogStatusInfo` and extend `CatalogDownloadProgress`:

```typescript
export interface CatalogStatusInfo {
  name: string;
  density: number;
  installed: boolean;
  epoch: number;
  star_count_approx: number;
  size_bytes: number;
  min_fov_deg: number;
  mag_limit: number;
}

export interface CatalogDownloadProgress {
  phase: 'tier' | 'downloading' | 'verifying' | 'extracting' | 'complete' | 'error';
  current: number;
  total: number;
  percent: number;
  tierDensity: number;
  tierIndex: number;
  nTiers: number;
}
```

- [ ] **Step 7: Build both backends + tsc**

Run: `cargo build -p athenaeum-tauri -p athenaeum-web && npx tsc --noEmit`
Expected: all compile. (`src/components/.../PlateSolveSettingsPanel.tsx` may show
type errors against the new `CatalogStatusInfo`; those are fixed in Task 7 — if
`tsc` fails only there, proceed; otherwise fix the backend.)

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-tauri crates/athenaeum-web src/types/plate-solve.ts
git commit -m "feat(catalog): download_catalog_layers + per-tier get_catalog_status (both backends)"
```

---

### Task 7: Frontend — FOV helper + per-tier panel + missing modal

**Files:**
- Create: `src/components/plate-solve/cameraPresets.ts`
- Modify: `src/components/plate-solve/PlateSolveSettingsPanel.tsx`
- Modify: `src/components/plate-solve/PlateSolveIndexMissingModal.tsx`

**Interfaces:**
- Consumes: `get_catalog_status` → `CatalogStatusInfo[]` (per tier, with
  `density` + `min_fov_deg`), `download_catalog_layers({ targetDensity })`,
  `catalog-download-progress` (with `tierDensity`/`tierIndex`/`nTiers`).
- Produces: `recommendTier(fovDeg, tiers) -> number` (target density), the FOV
  helper UI, the per-tier have-vs-need table.

- [ ] **Step 1: Camera presets + FOV math** (`cameraPresets.ts`)

```typescript
// Static example rigs for the FOV helper. Users can override every field.
export interface CameraPreset {
  label: string;
  pixelUm: number;   // sensor pixel pitch, micrometers
  widthPx: number;
  heightPx: number;
}

export const CAMERA_PRESETS: CameraPreset[] = [
  { label: 'ASI2600 (IMX571, 3.76µm 6248×4176)', pixelUm: 3.76, widthPx: 6248, heightPx: 4176 },
  { label: 'ASI1600 (4.63µm 4656×3520)',          pixelUm: 4.63, widthPx: 4656, heightPx: 3520 },
  { label: 'ASI294 (4.63µm 4144×2822)',           pixelUm: 4.63, widthPx: 4144, heightPx: 2822 },
  { label: 'DSLR APS-C (3.9µm 6000×4000)',        pixelUm: 3.9,  widthPx: 6000, heightPx: 4000 },
];

/** Pixel scale in arcsec/px: 206.265 · pixelUm · binning / focalMm. */
export function pixelScaleArcsec(pixelUm: number, focalMm: number, binning: number): number {
  if (focalMm <= 0) return 0;
  return (206.265 * pixelUm * binning) / focalMm;
}

/** Field of view (long axis) in degrees. */
export function fovDeg(pixelScaleArcsec: number, widthPx: number, heightPx: number): number {
  return (pixelScaleArcsec * Math.max(widthPx, heightPx)) / 3600;
}

/**
 * Recommended target density: the smallest tier whose `min_fov_deg <= fov`
 * (deeper tiers support smaller fields). Falls back to the deepest tier when the
 * field is smaller than every tier's `min_fov_deg`.
 */
export function recommendTier(
  fov: number,
  tiers: { density: number; min_fov_deg: number }[],
): number {
  const asc = [...tiers].sort((a, b) => a.density - b.density);
  const hit = asc.find((t) => t.min_fov_deg <= fov);
  return (hit ?? asc[asc.length - 1])?.density ?? 2000;
}
```

- [ ] **Step 2: Wire the FOV helper + per-tier table into the panel.** (Use the
  `frontend-design` skill / a `frontend-dev` agent for the JSX — project convention
  for UI work.) In
  `PlateSolveSettingsPanel.tsx`: replace the single-catalog `STAR_CATALOG_FALLBACK`
  usage and the `downloadStarCatalog` callback's `api.invoke('download_gaia_dr3_prebuilt_catalog')`
  with `api.invoke('download_catalog_layers', { targetDensity })`. Add state for
  `focalMm`, `pixelUm`, `widthPx`, `heightPx`, `binning` (seed from the first
  preset), compute `pixelScale`/`fov`/`recommended` with the `cameraPresets`
  helpers, and render: (a) the FOV-helper inputs (focal mm, a preset `<select>` +
  manual µm/W/H, binning) with the computed `pixelScale`/`FOV`/recommended tier;
  (b) a per-tier table from `catalogs` (density, ✓installed / "needed", star count
  via `formatStarCount`, size); (c) a "Download needed set" button calling
  `downloadStarCatalog(recommended)`; (d) the existing progress UI, now labelled
  with `downloadProgress.tierDensity` / `tierIndex+1` of `nTiers`. Use design
  tokens throughout.

- [ ] **Step 3: Update the missing-catalog modal.** In
  `PlateSolveIndexMissingModal.tsx`, change its download trigger to
  `api.invoke('download_catalog_layers', { targetDensity: 2000 })` (first-run
  default: base + Δ1) and update the copy from "download the catalog" to "download
  the recommended star-catalog set".

- [ ] **Step 4: Type-check + lint**

Run: `npx tsc --noEmit`
Expected: PASS (no references to the removed `CatalogStatusInfo` fields remain).

- [ ] **Step 5: Commit**

```bash
git add src/components/plate-solve/cameraPresets.ts src/components/plate-solve/PlateSolveSettingsPanel.tsx src/components/plate-solve/PlateSolveIndexMissingModal.tsx
git commit -m "feat(catalog): FOV helper + per-tier catalog panel; download_catalog_layers"
```

---

### Task 8: End-to-end validation against a local test host

**Files:** none (validation only)

- [ ] **Step 1: Serve the built tiers locally**

Run (in a separate shell, from the publish dir):
```bash
cd /Volumes/BigMac/Users/astrobureau/catalog_out/publish && python3 -m http.server 8765
```
Expected: serves `manifest.json` + `tier_*.zip` at `http://localhost:8765/`.

- [ ] **Step 2: Point the app at the local host + clear any install**

```bash
export ATHENAEUM_CATALOG_BASE_URL=http://localhost:8765/
rm -rf "$HOME/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia"
```

- [ ] **Step 3: Download a FOV-selected set via the web backend**

Run `cargo run -p athenaeum-web` (with the env var set), then from the panel (or
`curl -XPOST localhost:<port>/api/download_catalog_layers -d '{"targetDensity":2000}'`)
trigger the download. Expected: `tier_500/` + `tier_2000/` land under
`catalogs/smac_gaia/`, `catalog-download-progress` events stream per tier, and a
second call is a no-op (already installed).

- [ ] **Step 4: Confirm status + a real solve**

`get_catalog_status` shows tier_500 + tier_2000 installed (others "needed"). Then
plate-solve a known wide-field frame (e.g. the veil) via the backend → it solves
(`discover_layers` finds the tiers). Expected: solved with SIP.

- [ ] **Step 5: Commit** (notes only, if any harness/docs changed; else skip)

---

### Task 9: Remove the dead single-zip download path

**Files:**
- Modify: `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs`
- Modify: `crates/athenaeum-tauri/src/commands/plate_solve.rs` + `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/plate_solve.rs` + `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `src/components/plate-solve/PlateSolveSettingsPanel.tsx` (drop the old invoke if any remains)

- [ ] **Step 1: Delete the single-zip core path.** In `gaia_prebuilt.rs` remove:
  `download_gaia_dr3_prebuilt`, `prebuilt_urls`, `STAR_CATALOG_URL`, `extract_zip`
  (the deep/bright router) and its `bright_dir` plumbing; update the module doc
  comment (drop the deep/bright archive description). Keep `http_client`,
  `download_resumable`, `sha256_file`, `smac_present`, `GaiaPrebuiltProgress`, and
  the tier functions. **Do not** touch `smac_gaia_bright` references in the
  registration routes.

- [ ] **Step 2: Delete the old command/route.** Remove
  `download_gaia_dr3_prebuilt_catalog` from `commands/plate_solve.rs` +
  `lib.rs` (Tauri) and `routes/plate_solve.rs` + `routes/mod.rs` (web).

- [ ] **Step 3: Build everything + the test suites**

Run: `cargo build --workspace --all-targets && cargo test -p athenaeum-core && npx tsc --noEmit`
Expected: compiles clean (no `download_gaia_dr3_prebuilt*` / `STAR_CATALOG_URL` /
`extract_zip` references left); core tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore(catalog): remove the legacy single-zip download path"
```

## Self-Review

- **Spec coverage:** manifest move → Task 1; base URL + manifest fetch/cache →
  Task 2; `extract_tier_zip` → Task 3; `download_catalog_layers` (FOV target,
  resumable, verify, idempotent) → Task 4; per-tier `get_catalog_status`/status →
  Tasks 5–6; two-backend commands → Task 6; FOV helper + per-tier UI + missing
  modal → Task 7; local-host validation → Task 8; dead-code cleanup → Task 9.
  Registration explicitly untouched (Global Constraints + Task 9 Step 1).
- **Placeholders:** none — every code step has concrete code; Task 7 Step 2 is a
  description of an inherently presentational change with the exact helpers/invoke
  named (the `cameraPresets` API + `download_catalog_layers` call), not a "TBD".
- **Type consistency:** `Manifest`/`ManifestTier` fields identical across Tasks
  1/2/4/5; `GaiaPrebuiltProgress::Tier{density,index,n_tiers}` produced in Task 4,
  consumed in Task 6; `TierStatus{density,installed,epoch,star_count,size_bytes,min_fov_deg}`
  produced in Task 5, mapped in Task 6; `CatalogStatusInfo`/`CatalogDownloadProgress`
  Rust ↔ TS mirrors match in Task 6; `recommendTier`/`pixelScaleArcsec`/`fovDeg`
  consistent in Task 7.

## Risks

- **Network/integration coverage:** the full download is only exercised in Task 8
  (local host). The pure pieces (`tiers_to_fetch`, base URL, `extract_tier_zip`,
  `tier_status`, FOV math) are unit-tested; keep Task 8 in the loop before merge.
- **Two-backend drift:** Task 6 Step 5 mirrors Tauri → web; re-grep both
  `plate_solve.rs` for `download_catalog_layers` + `get_catalog_status` before
  committing.

## Follow-on (not this plan)

- **Phase 4:** upload `~/catalog_out/publish/` to `artfrom.space/catalogs/` verbatim.
- **Registration migration** to tiers (deferred).
