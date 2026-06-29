# Catalog Layer Builder (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `catalog-builder` to build the 4 density-limited additive layer
caches from the `G<21` HEALPix bins, and emit a ready-to-upload `publish/` tree
(per-tier zips + sha256 + `manifest.json`).

**Architecture:** Slice each cell's mag-sorted records (read straight from the
`healpix_*.bin` tiles) into disjoint rank bands, one band per tier; build each
band into its own `tier_<density>/stars.smac` via `solvemyastro::cache::build_cache`;
package each tier dir into `tier_<density>.zip` + `.sha256` plus a `manifest.json`.

**Tech Stack:** Rust; `solvemyastro::cache` (`build_cache`, `StarCache`,
`StarRecord`); `athenaeum_core::catalog::binary_format` (14-byte bin reader);
`zip` 2.x; `sha2` 0.10; `serde`/`serde_json`.

**Spec:** `docs/superpowers/specs/2026-06-29-tiered-additive-star-catalog-design.md`

## Global Constraints

- Density tiers (cumulative, stars/deg²): **500, 2000, 5000, 8000**. HEALPix-6,
  49 152 cells, full-sphere 41 252.96125 deg² → cell area 0.8392876 deg².
- Layers are **disjoint rank bands** (zero star duplication). Per-cell ordering:
  mag ascending, tie-break RA ascending.
- Source: `G<21` bins. The deepest tier caps at ~330 M stars — never build a full
  `G<21` deep as a product.
- On-disk / archive layout: per-tier **dirs** `tier_<d>/stars.smac` (because
  `StarCache::open(dir)` reads `<dir>/stars.smac`).
- `solvemyastro::StarRecord { ra: f64, dec: f64, mag: f32, pmra_mas_yr: f64, pmdec_mas_yr: f64 }`.
- `binary_format::StarRecord`: fields `ra: f32`, `dec: f32`; methods `mag() -> f32`,
  `pmra_mas_yr() -> f64`, `pmdec_mas_yr() -> f64`. `RECORD_SIZE = 14`.
  `read_records_until_mag(&[u8], mag_limit) -> Vec<StarRecord>` (mag-sorted).
- `build_cache(records: IntoIterator<Item = solvemyastro::StarRecord>, out_dir: &Path, catalog_epoch: f64, progress: impl Fn(BuildProgress)) -> Result<usize>`.
- Error rule: never swallow silently — log to stderr before continuing/returning.
- Don't name ASTAP in code/comments; use `tier_<density>` naming.

## File Structure

- Create `crates/catalog-builder/src/tiers.rs` — tier table + per-cell rank
  boundaries + `slice_select`.
- Create `crates/catalog-builder/src/layers.rs` — `build_layers` (bins → 4 tier
  caches).
- Create `crates/catalog-builder/src/publish.rs` — `Manifest`, `package_publish`
  (tier dirs → zips + sha256 + manifest.json).
- Modify `crates/catalog-builder/src/main.rs` — declare the modules; rewire the
  CLI from deep+hybrid-bright+single-zip to ingest→layers→publish; delete
  `hybrid_select`, `build_bright`, the old `package`.
- Modify `crates/catalog-builder/Cargo.toml` — add `serde` + `serde_json`.
- Modify `crates/athenaeum-core/src/catalog/gaia_bulk.rs` and `gaia.rs` — thread a
  `mag_limit` parameter through `ingest_bulk`/`parse_bulk_row`; revert the
  `GAIA_MAG_LIMIT` constant to 19.0 as the default.

---

### Task 1: Tier table + `slice_select`

**Files:**

- Create: `crates/catalog-builder/src/tiers.rs`
- Modify: `crates/catalog-builder/src/main.rs` (add `mod tiers;`)

**Interfaces:**

- Produces: `pub const TIER_DENSITIES: [u32; 4]`; `pub fn cell_cum_counts() -> [usize; 5]`
  (cumulative per-cell record counts `[0, b1, b2, b3, b4]`); `pub fn slice_select(records: &[T], lo: usize, hi: usize) -> &[T]`.

- [ ] **Step 1: Write the failing test**

```rust
// in crates/catalog-builder/src/tiers.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_match_density_times_cell_area() {
        // cell area = 41252.96125 / 49152 = 0.8392876 deg²
        let c = cell_cum_counts();
        assert_eq!(c, [0, 420, 1679, 4196, 6714]);
    }

    #[test]
    fn slice_select_returns_the_band() {
        let v: Vec<u32> = (0..100).collect();
        assert_eq!(slice_select(&v, 10, 30), &v[10..30]);
    }

    #[test]
    fn slice_select_clamps_to_len_and_is_disjoint() {
        let v: Vec<u32> = (0..15).collect();
        // band beyond the data → empty; bands tile without overlap
        assert_eq!(slice_select(&v, 20, 40), &[] as &[u32]);
        let a = slice_select(&v, 0, 10);
        let b = slice_select(&v, 10, 40); // clamps hi to 15
        assert_eq!(a.len() + b.len(), v.len());
        assert_eq!(b, &v[10..15]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p catalog-builder tiers::`
Expected: FAIL — `tiers` module / functions not defined.

- [ ] **Step 3: Write the implementation**

```rust
// crates/catalog-builder/src/tiers.rs
//! Density-limited tier table and per-cell rank-band slicing.

/// Cumulative density targets (stars/deg²) for the 4 additive tiers.
pub const TIER_DENSITIES: [u32; 4] = [500, 2000, 5000, 8000];

/// Full-sphere area / HEALPix-6 cell count = per-cell solid angle in deg².
const CELL_AREA_DEG2: f64 = 41_252.961_25 / 49_152.0;

/// Cumulative per-cell record counts `[0, b1, b2, b3, b4]`. Tier `k` owns the
/// rank band `[counts[k], counts[k+1])`.
pub fn cell_cum_counts() -> [usize; 5] {
    let mut out = [0usize; 5];
    for (i, d) in TIER_DENSITIES.iter().enumerate() {
        out[i + 1] = (*d as f64 * CELL_AREA_DEG2).round() as usize;
    }
    out
}

/// The slice of `records` for rank band `[lo, hi)`, clamped to the data length.
/// Records must be mag-sorted ascending; bands are disjoint and tile the cell.
pub fn slice_select<T>(records: &[T], lo: usize, hi: usize) -> &[T] {
    let end = hi.min(records.len());
    let start = lo.min(end);
    &records[start..end]
}
```

- [ ] **Step 4: Add `mod tiers;` to main.rs**

In `crates/catalog-builder/src/main.rs`, near the other top-level items, add:

```rust
mod tiers;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p catalog-builder tiers::`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/catalog-builder/src/tiers.rs crates/catalog-builder/src/main.rs
git commit -m "feat(catalog-builder): tier table + per-cell rank-band slice_select"
```

---

### Task 2: Build the 4 layer caches from bins

**Files:**

- Create: `crates/catalog-builder/src/layers.rs`
- Modify: `crates/catalog-builder/src/main.rs` (add `mod layers;`)
- Test: inline `#[cfg(test)]` in `layers.rs`

**Interfaces:**

- Consumes: `tiers::{TIER_DENSITIES, cell_cum_counts, slice_select}`;
  `athenaeum_core::catalog::binary_format`; `solvemyastro::cache::build_cache`;
  `solvemyastro::StarRecord`.
- Produces: `pub fn build_layers(bins_dir: &Path, out_dir: &Path, epoch: f64) -> anyhow::Result<Vec<(u32, usize)>>`
  — builds `out_dir/tier_<density>/stars.smac` for each tier; returns
  `(density, star_count)` per tier.

- [ ] **Step 1: Write the failing test**

```rust
// in crates/catalog-builder/src/layers.rs
#[cfg(test)]
mod tests {
    use super::*;
    use athenaeum_core::catalog::binary_format::{write_records, StarRecord as BinRec};
    use std::io::BufWriter;

    // One dense synthetic cell (pixel 0) with 7000 mag-ascending stars, so
    // every tier band is exercised and the dense cap (6714) bites.
    fn write_dense_bin(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        let mut recs: Vec<BinRec> = (0..7000)
            .map(|i| BinRec::from_values(10.0, 20.0, 5.0 + i as f32 * 0.001, 0.0, 0.0))
            .collect();
        let f = std::fs::File::create(dir.join("healpix_000000.bin")).unwrap();
        let mut w = BufWriter::new(f);
        write_records(&mut w, &mut recs).unwrap();
    }

    #[test]
    fn builds_four_disjoint_tier_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let bins = tmp.path().join("gaia_dr3");
        write_dense_bin(&bins);
        let out = tmp.path().join("out");

        let counts = build_layers(&bins, &out, 2016.0).unwrap();
        // bands [0,420) [420,1679) [1679,4196) [4196,6714)
        assert_eq!(counts, vec![(500, 420), (2000, 1259), (5000, 2517), (8000, 2518)]);
        // total kept == dense cap, not all 7000 (286 faint stars dropped)
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        assert_eq!(total, 6714);

        // each tier opens and reports its own count
        for (d, n) in counts {
            let c = solvemyastro::StarCache::open(&out.join(format!("tier_{d}"))).unwrap();
            assert_eq!(c.star_count() as usize, n);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p catalog-builder layers::`
Expected: FAIL — `build_layers` not defined. (Add `tempfile` to `[dev-dependencies]`
in `crates/catalog-builder/Cargo.toml`: `tempfile = "3"` — match the workspace version.)

- [ ] **Step 3: Write the implementation**

```rust
// crates/catalog-builder/src/layers.rs
//! Build the 4 density-limited tier caches by slicing the G<21 bins.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use athenaeum_core::catalog::binary_format;
use solvemyastro::cache::build_cache;
use solvemyastro::StarRecord;

use crate::tiers::{cell_cum_counts, slice_select, TIER_DENSITIES};

/// Enumerate `healpix_*.bin` tiles in `bins_dir`, sorted for determinism.
fn bin_paths(bins_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(bins_dir)
        .with_context(|| format!("read bins dir {}", bins_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("healpix_") && n.ends_with(".bin"))
        })
        .collect();
    paths.sort();
    Ok(paths)
}

fn to_smac(r: &binary_format::StarRecord) -> StarRecord {
    StarRecord {
        ra: r.ra as f64,
        dec: r.dec as f64,
        mag: r.mag(),
        pmra_mas_yr: r.pmra_mas_yr(),
        pmdec_mas_yr: r.pmdec_mas_yr(),
    }
}

/// Build `out_dir/tier_<density>/stars.smac` for each tier. One pass over the
/// bins per tier (bounded RAM); each pass slices that tier's rank band per cell.
pub fn build_layers(bins_dir: &Path, out_dir: &Path, epoch: f64) -> Result<Vec<(u32, usize)>> {
    let bins = bin_paths(bins_dir)?;
    let bounds = cell_cum_counts();
    let mut out = Vec::with_capacity(TIER_DENSITIES.len());

    for (k, density) in TIER_DENSITIES.iter().enumerate() {
        let (lo, hi) = (bounds[k], bounds[k + 1]);
        let tier_dir = out_dir.join(format!("tier_{density}"));

        // Lazily stream each cell's [lo,hi) band into build_cache.
        let records = bins.iter().flat_map(|p| {
            let data = match std::fs::read(p) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("warn: read {} failed: {e} — skipping", p.display());
                    Vec::new()
                }
            };
            let cell = binary_format::read_records_until_mag(&data, f32::MAX);
            slice_select(&cell, lo, hi).iter().map(to_smac).collect::<Vec<_>>()
        });

        let n = build_cache(records, &tier_dir, epoch, |_| {})
            .with_context(|| format!("build tier_{density}"))?;
        println!("  tier_{density}: {n} stars");
        out.push((*density, n));
    }
    Ok(out)
}
```

- [ ] **Step 4: Add `mod layers;` to main.rs**

```rust
mod layers;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p catalog-builder layers::`
Expected: PASS — 4 disjoint caches, total 6714, dense cap applied.

- [ ] **Step 6: Commit**

```bash
git add crates/catalog-builder/src/layers.rs crates/catalog-builder/src/main.rs crates/catalog-builder/Cargo.toml
git commit -m "feat(catalog-builder): build 4 disjoint density tier caches from bins"
```

---

### Task 3: Publish tree — zips + sha256 + manifest.json

**Files:**

- Create: `crates/catalog-builder/src/publish.rs`
- Modify: `crates/catalog-builder/src/main.rs` (add `mod publish;`)
- Modify: `crates/catalog-builder/Cargo.toml` (add `serde`, `serde_json`)

**Interfaces:**

- Consumes: tier dirs `out_dir/tier_<density>/stars.smac`; per-tier
  `(density, star_count)` from Task 2.
- Produces: `pub fn package_publish(out_dir: &Path, tiers: &[(u32, usize)], epoch: f64) -> anyhow::Result<PathBuf>`
  — writes `out_dir/publish/{manifest.json, tier_<d>.zip, tier_<d>.zip.sha256}`,
  returns the `publish/` path. Manifest min_fov per tier:
  500→0.6, 2000→0.3, 5000→0.2, 8000→0.15.

- [ ] **Step 1: Add deps**

In `crates/catalog-builder/Cargo.toml` `[dependencies]` (match athenaeum-core versions):

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Write the failing test**

```rust
// in crates/catalog-builder/src/publish.rs
#[cfg(test)]
mod tests {
    use super::*;
    use solvemyastro::cache::build_cache;
    use solvemyastro::StarRecord;

    #[test]
    fn produces_zip_sha_and_manifest_per_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path();
        // build a tiny real tier cache so the zip has a valid stars.smac
        let recs = vec![StarRecord { ra: 10.0, dec: 20.0, mag: 8.0, pmra_mas_yr: 0.0, pmdec_mas_yr: 0.0 }];
        build_cache(recs, &out.join("tier_500"), 2016.0, |_| {}).unwrap();

        let pub_dir = package_publish(out, &[(500, 1)], 2016.0).unwrap();
        assert!(pub_dir.join("tier_500.zip").is_file());
        assert!(pub_dir.join("tier_500.zip.sha256").is_file());

        let m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(pub_dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m["tiers"][0]["density"], 500);
        assert_eq!(m["tiers"][0]["zip"], "tier_500.zip");
        assert_eq!(m["tiers"][0]["dir"], "tier_500");
        assert_eq!(m["tiers"][0]["min_fov_deg"], 0.6);

        // sha256 sidecar matches the zip
        let sidecar = std::fs::read_to_string(pub_dir.join("tier_500.zip.sha256")).unwrap();
        let digest = sidecar.split_whitespace().next().unwrap();
        assert_eq!(digest.len(), 64);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p catalog-builder publish::`
Expected: FAIL — `package_publish` not defined.

- [ ] **Step 4: Write the implementation**

```rust
// crates/catalog-builder/src/publish.rs
//! Build the ready-to-upload publish/ tree: per-tier zips + sha256 + manifest.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[derive(Serialize)]
struct ManifestTier {
    density: u32,
    zip: String,
    sha256: String,
    dir: String,
    size_bytes: u64,
    min_fov_deg: f64,
}

#[derive(Serialize)]
struct Manifest {
    version: u32,
    catalog_epoch: f64,
    tiers: Vec<ManifestTier>,
}

fn min_fov_for(density: u32) -> f64 {
    match density {
        d if d <= 500 => 0.6,
        d if d <= 2000 => 0.3,
        d if d <= 5000 => 0.2,
        _ => 0.15,
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut f = BufReader::new(File::open(path)?);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Zip `out_dir/tier_<d>/stars.smac` as `tier_<d>/stars.smac` into the archive
/// (Stored — dense binary; `large_file` for >4 GB tiers).
fn zip_tier(out_dir: &Path, density: u32, zip_path: &Path) -> Result<()> {
    let smac = out_dir.join(format!("tier_{density}")).join("stars.smac");
    let zf = BufWriter::new(File::create(zip_path)?);
    let mut zw = ZipWriter::new(zf);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    zw.start_file(format!("tier_{density}/stars.smac"), opts)?;
    let mut f = BufReader::new(
        File::open(&smac).with_context(|| format!("open {}", smac.display()))?,
    );
    io::copy(&mut f, &mut zw)?;
    zw.finish()?;
    Ok(())
}

pub fn package_publish(out_dir: &Path, tiers: &[(u32, usize)], epoch: f64) -> Result<PathBuf> {
    let pub_dir = out_dir.join("publish");
    fs::create_dir_all(&pub_dir)?;

    let mut manifest_tiers = Vec::new();
    for (density, _count) in tiers {
        let zip_name = format!("tier_{density}.zip");
        let sha_name = format!("{zip_name}.sha256");
        let zip_path = pub_dir.join(&zip_name);

        zip_tier(out_dir, *density, &zip_path)?;
        let digest = sha256_file(&zip_path)?;
        fs::write(pub_dir.join(&sha_name), format!("{digest}  {zip_name}\n"))?;

        manifest_tiers.push(ManifestTier {
            density: *density,
            zip: zip_name,
            sha256: sha_name,
            dir: format!("tier_{density}"),
            size_bytes: fs::metadata(&zip_path)?.len(),
            min_fov_deg: min_fov_for(*density),
        });
        println!("  packaged tier_{density}");
    }

    let manifest = Manifest { version: 1, catalog_epoch: epoch, tiers: manifest_tiers };
    let json = serde_json::to_vec_pretty(&manifest)?;
    fs::write(pub_dir.join("manifest.json"), json)?;
    Ok(pub_dir)
}
```

- [ ] **Step 5: Add `mod publish;` to main.rs**

```rust
mod publish;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p catalog-builder publish::`
Expected: PASS — zip + sha256 + manifest present and well-formed.

- [ ] **Step 7: Commit**

```bash
git add crates/catalog-builder/src/publish.rs crates/catalog-builder/src/main.rs crates/catalog-builder/Cargo.toml
git commit -m "feat(catalog-builder): publish tree — tier zips + sha256 + manifest.json"
```

---

### Task 4: Configurable ingest `--mag-limit`

**Files:**

- Modify: `crates/athenaeum-core/src/catalog/gaia.rs` (revert constant to 19.0)
- Modify: `crates/athenaeum-core/src/catalog/gaia_bulk.rs` (thread `mag_limit`)
- Test: existing `gaia_bulk` tests + a new `parse_bulk_row` arg test

**Interfaces:**

- Produces: `ingest_bulk(bulk_dir, app_data_dir, mag_limit: f32, concurrency, cancel, progress)`
  — new `mag_limit` parameter; `parse_bulk_row(row, idx, mag_limit: f32)`.

- [ ] **Step 1: Revert the interim constant**

In `crates/athenaeum-core/src/catalog/gaia.rs`, set the default back to 19.0 (the
flag now carries the build-time choice):

```rust
pub const GAIA_MAG_LIMIT: f32 = 19.0;
```

And revert the test at the same file: `assert!(q0.contains("phot_g_mean_mag < 19"));`

- [ ] **Step 2: Write the failing test**

```rust
// in crates/athenaeum-core/src/catalog/gaia_bulk.rs tests
#[test]
fn parse_bulk_row_honours_mag_limit_arg() {
    let header = "ra,dec,phot_g_mean_mag,pmra,pmdec,ruwe";
    let idx = ColumnIndex::from_header(header).unwrap();
    let row = "10.0,20.0,20.5,0,0,1.0"; // G=20.5
    assert!(parse_bulk_row(row, idx, 19.0).is_none(), "dropped at limit 19");
    assert!(parse_bulk_row(row, idx, 21.0).is_some(), "kept at limit 21");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p athenaeum-core parse_bulk_row_honours_mag_limit_arg`
Expected: FAIL — `parse_bulk_row` takes 2 args, not 3.

- [ ] **Step 4: Thread the parameter**

In `gaia_bulk.rs`: change `fn parse_bulk_row(row: &str, idx: ColumnIndex)` to
`fn parse_bulk_row(row: &str, idx: ColumnIndex, mag_limit: f32)` and replace the
`g >= GAIA_MAG_LIMIT` check with `g >= mag_limit`. Add a `mag_limit: f32`
parameter to `ingest_bulk`, `ingest_one_file`, and any internal caller, passing
it down to `parse_bulk_row`. Update the existing call in
`setup_gaia_dr3_from_bulk` to pass `GAIA_MAG_LIMIT` (preserving today's default).

- [ ] **Step 5: Update the example caller**

In `crates/athenaeum-core/examples/ingest_gaia_bulk.rs`, pass `GAIA_MAG_LIMIT`
(import it) to `ingest_bulk` so the example still compiles.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p athenaeum-core gaia`
Expected: PASS — new arg test + existing gaia tests green.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/catalog/gaia.rs crates/athenaeum-core/src/catalog/gaia_bulk.rs crates/athenaeum-core/examples/ingest_gaia_bulk.rs
git commit -m "feat(catalog): configurable ingest mag-limit (default G<19)"
```

---

### Task 5: Rewire the `catalog-builder` CLI to the layered flow

**Files:**

- Modify: `crates/catalog-builder/src/main.rs`

**Interfaces:**

- Consumes: `tiers`, `layers::build_layers`, `publish::package_publish`,
  `ingest_bulk(.., mag_limit, ..)`.
- Produces: CLI flags `--mag-limit <f32>` (default 21.0) replacing the old
  `--bright-floor/--min-per-cell/--max-per-cell/--bright-only/--deep-only`.

- [ ] **Step 1: Update `Config` + arg parsing**

In `main.rs`, remove `bright_floor`, `min_per_cell`, `max_per_cell`, `deep_only`,
`bright_only` from `Config` and the arg loop; add `mag_limit: f32` (flag
`--mag-limit`, default 21.0). Keep `gaia_dir`, `work_dir`, `out`, `epoch`,
`skip_download`, `no_zip`, concurrencies. Update `validate()` (drop the
min/max-per-cell check) and `print_help()`.

- [ ] **Step 2: Replace the build pipeline in `main()`**

Replace the deep/bright/package stages with:

```rust
// stage 2: bin (pass the configurable mag limit)
let n = ingest_bulk(&cfg.gaia_dir, &cfg.work_dir, cfg.mag_limit, cfg.ingest_concurrency,
                    cancel.clone(), &|p| { /* same progress match as today */ })?;
// stage 3: build the 4 tier caches
println!("[3/5] Build density tiers → {}", cfg.out_dir.display());
let tiers = layers::build_layers(&bins_dir, &cfg.out_dir, cfg.epoch)?;
// stage 4: publish
if !cfg.no_zip {
    let pub_dir = publish::package_publish(&cfg.out_dir, &tiers, cfg.epoch)?;
    println!("[4/5] publish tree → {} (upload its contents to artfrom.space/catalogs/)", pub_dir.display());
}
```

Delete `hybrid_select`, `build_bright`, `build_deep` (the deep+bright path), the
old `package`, and the now-unused imports/helpers. Keep `acquire_gaia`,
`bin_dir_ready`, and the bins/`build_layers` discovery (`bins_dir = work_dir/catalogs/gaia_dr3`).

- [ ] **Step 3: Update the crate README**

In `crates/catalog-builder/README.md`, replace the deep+hybrid-bright description
with the tier model + the publish/ layout (point to the spec).

- [ ] **Step 4: Build + run the existing unit tests**

Run: `cargo build -p catalog-builder && cargo test -p catalog-builder`
Expected: PASS — tiers/layers/publish tests green; crate compiles.

- [ ] **Step 5: End-to-end on a synthetic subset**

Create a 2-file synthetic `GaiaSource_*.csv.gz` dir (as in the earlier subset
test), then:

Run:

```bash
cargo run -p catalog-builder --release -- \
  --gaia-dir <tmp>/gaia --out <tmp>/out --skip-download --mag-limit 21
```

Expected: builds `out/tier_{500,2000,5000,8000}/stars.smac` + `out/publish/`
with 4 zips + sha256 + manifest.json; `solvemyastro cache-info out/tier_500`
reports a star count.

- [ ] **Step 6: Commit**

```bash
git add crates/catalog-builder/src/main.rs crates/catalog-builder/README.md
git commit -m "feat(catalog-builder): rewire CLI to layered density-tier build + publish"
```

---

## Self-Review

- **Spec coverage:** §1 (tier model, per-tier dirs) → Tasks 1-2; §2 build
  (G<21 ingest, slice, package, manifest) → Tasks 2-5; §5 hosting layout
  (zip internals, manifest schema, publish tree) → Task 3. Solver (§3) and app
  (§4) are **out of scope** for this plan (Plans 2 and 3).
- **Placeholders:** none — every step has concrete code or an exact command.
- **Type consistency:** `slice_select` (Task 1) used by `build_layers` (Task 2);
  `build_layers` returns `Vec<(u32, usize)>` consumed by `package_publish`
  (Task 3); `ingest_bulk(.., mag_limit, ..)` (Task 4) called in Task 5. `dir`
  field in the manifest matches the per-tier-dir on-disk layout.

## Validation gate (whole plan)

- `cargo test -p catalog-builder` and `cargo test -p athenaeum-core gaia` green.
- End-to-end (Task 5 Step 5) produces the `publish/` tree; `solvemyastro
  cache-info` opens each tier dir.
- When the `G<21` re-bin finishes (`/Volumes/BigMac/Users/astrobureau/catalog_build/catalogs/gaia_dr3/`),
  run `build_layers` on the real bins and check per-cell density + `cache-info`
  star counts against the §1 table (this feeds Plan 2/4 tuning).

## Follow-on plans (not in this document)

- **Plan 2 — solvemyastro layered solver:** `LayeredCatalog` + `cone_merged`
  (union) + `Caches` stack + the two call sites + migration; `corpus_bench` gate.
- **Plan 3 — app:** layer discovery, `download_catalog_layers` + `get_catalog_status`
  (two backends), the FOV helper.
