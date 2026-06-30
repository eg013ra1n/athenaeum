# Plan 3 — Catalog Delivery (App) Design

**Status:** approved (brainstormed 2026-06-30), ready for an implementation plan.

**Baseline:** this refines §4 (App) + §5 (Hosting & on-disk layout) of
`2026-06-29-tiered-additive-star-catalog-design.md`. Plan 2 (layered solver)
is done; the solver already consumes a `tier_<d>/` stack via
`plate_solve::discover_layers` + `Caches::layered`. The gap: the app cannot
**acquire** the tiers — `gaia_prebuilt.rs` still downloads the single
`smac_gaia.zip` (deep cache), which no longer exists on disk or (soon) on the
server.

**Goal:** the app downloads the additive density tiers it needs (by FOV) from a
catalog server into `<app-data>/catalogs/smac_gaia/tier_<d>/`, reports per-tier
status, and offers a focal-length/sensor → recommended-tier helper — without
re-downloading already-installed tiers.

## Decisions (from brainstorm)

1. **Registration is NOT migrated here.** Plan 3 is plate-solve catalog delivery
   only. The registration path stays on the legacy deep(+bright) code (already
   non-functional without a catalog) until its own later plan. The
   `smac_gaia_bright` references in the registration routes are left alone.
2. **Full FOV helper** in the frontend (per spec §4), not a bare density
   selector.
3. **Manifest model moves to `athenaeum-core`** (`catalog::manifest`, `pub`,
   `Serialize + Deserialize`). `catalog-builder` (already a `core` dependent)
   imports it for writing; the new download path uses it for reading. One schema,
   no duplication.
4. **Acquisition stays user-triggered** (no startup auto-download exists today):
   the "Star catalog" panel button + the solve-time missing-catalog modal.
5. **Dev against a local test host**, real `artfrom.space` upload is Phase 4.

## Components & interfaces

### Backend — `athenaeum-core`

- **`catalog::manifest`** (new module): move `Manifest { version, catalog_epoch,
  tiers: Vec<ManifestTier> }` and `ManifestTier { density, zip, sha256, dir,
  size_bytes, min_fov_deg }` here from `catalog-builder/src/publish.rs` (add
  `Deserialize`). `catalog-builder` imports it.
- **`catalog::gaia_prebuilt` generalized in place** (reuses its private
  `http_client` / `download_resumable` / `sha256_file` / `smac_present` /
  `GaiaPrebuiltProgress` + the zip-slip component filter — no visibility
  changes):
  - `fn catalog_base_url() -> String` — `ATHENAEUM_CATALOG_BASE_URL` (default
    `https://artfrom.space/catalogs/`); accept legacy `ATHENAEUM_STAR_CATALOG_URL`
    / `ATHENAEUM_GAIA_PREBUILT_URL` by stripping the trailing filename to a base.
  - `fn download_catalog_layers(app_data: &Path, target_density: u32, cancel:
    Arc<AtomicBool>, progress: &dyn Fn(GaiaPrebuiltProgress)) -> Result<PathBuf>`
    — fetch+parse `manifest.json`; select tiers with `density ≤ target_density`
    that are **not already installed** (`discover_layers` / `smac_present` on
    `smac_gaia/tier_<d>/`); per tier: resumable download `tier_<d>.zip` → verify
    `.sha256` → `extract_tier_zip`. Idempotent; returns `smac_gaia/`.
  - `fn extract_tier_zip(zip_path, dest_root, cancel, progress)` — extract the
    `tier_<d>/stars.smac` entry to `dest_root/tier_<d>/stars.smac` (reuses the
    zip-slip filter; preserves the `tier_<d>/` prefix, unlike `extract_zip`'s
    deep/bright routing).
  - `GaiaPrebuiltProgress` gains a tier index/total (e.g. `Downloading { tier,
    n_tiers, received, total }`) so the UI can show "tier 2 of 3".

### Backend — commands (cardinal rule: Tauri command **and** Axum route together)

- **`download_catalog_layers(target_density)`** (new) — wraps
  `catalog::download_catalog_layers`; mirrors the existing
  `download_gaia_dr3_prebuilt_catalog` plumbing (`spawn_blocking` + map
  `GaiaPrebuiltProgress → CatalogDownloadProgress` + emit
  `catalog-download-progress`). Replaces the old command.
- **`get_catalog_status`** (generalize the existing one, both backends) — return
  one entry **per declared tier** (so have-vs-need works before anything is
  installed). Source = **manifest** (the full tier list + `min_fov_deg` /
  `size_bytes`) merged with **local installed state** (`discover_layers` +
  `StarCache::open().star_count()/catalog_epoch()` per installed tier). Manifest
  resolution: read the cached `smac_gaia/manifest.json` if present, else fetch it
  once from the base URL and cache it there (so status + the FOV helper work
  offline after the first fetch; download also refreshes the cache). If neither a
  cache nor the network is available, return an empty list (panel shows "not
  installed" + the download button, as today).
- **`CatalogStatusInfo`** extended (Rust ×2 + TS mirror): add `density: u32`,
  `min_fov_deg: f64`, `size_bytes: u64`; keep `installed`, `epoch`,
  `star_count_approx`, `mag_limit`. (`name` becomes the tier label.)
- **`CatalogDownloadProgress`** extended (Rust ×2 + TS): add tier index/total.

### Frontend — `PlateSolveSettingsPanel` "Star catalog" section

- **FOV helper** sub-component: inputs focal length (mm) + sensor (a few camera
  presets in a static TS data file with W×H + pixel µm, or manual µm + W×H px) +
  binning. Compute (frontend only) `pixel_scale = 206.265·µm·bin/focal` (″/px),
  `FOV = pixel_scale·max(W,H)/3600` (°); map FOV → recommended tier via the
  manifest `min_fov_deg` returned by `get_catalog_status`. A few example rigs.
- **Per-tier have-vs-need** table (from `get_catalog_status` + recommended
  target) + a **"Download needed set"** button → `download_catalog_layers(target)`.
- Wire the new per-tier types into `src/types/plate-solve.ts`. Design tokens, no
  raw colours.
- **`PlateSolveIndexMissingModal`** (solve with no catalog): offer the first-run
  default set — base + Δ1 (`target_density = 2000`).

## Verified reuse map (grounded in code, not invented)

| Reuse as-is | Where |
| ----------- | ----- |
| `http_client`, `download_resumable`, `sha256_file`, `smac_present`, `GaiaPrebuiltProgress`, zip-slip filter | `catalog/gaia_prebuilt.rs` (same module → private fns callable directly) |
| `discover_layers(root) -> Vec<PathBuf>` | `plate_solve/layers.rs` |
| `StarCache::open / star_count / catalog_epoch` | `solvemyastro` |
| command plumbing (`spawn_blocking` + progress→event + `catalog-download-progress`) | Tauri/Web `plate_solve.rs` |
| `CatalogStatusInfo` / `CatalogDownloadProgress` (Rust ×2 + TS) | extend, don't duplicate |

**New (not reusable as-is):** `catalog::manifest` (moved), `catalog_base_url`,
`download_catalog_layers`, `extract_tier_zip`, generalized `get_catalog_status`,
the FOV helper + camera-preset TS data.

## Dead code — remove AFTER the tier path works & is tested (own cleanup task)

- `download_gaia_dr3_prebuilt`, `prebuilt_urls`, `STAR_CATALOG_URL` (single-zip).
- `extract_zip`'s deep/bright routing + `bright_dir` param; the module's
  deep/bright doc comment.
- The old `download_gaia_dr3_prebuilt_catalog` command (Tauri + Axum) once the UI
  calls `download_catalog_layers`.
- **Keep:** `smac_gaia_bright` resolution in the *registration* routes (deferred
  feature) — only the bright **download/extract** in `gaia_prebuilt` is dead.

Sequencing: implement + wire the tier path first (build green throughout), then
delete the single-zip path in a final task — never mid-flight.

## Dev / validation / hosting

- **Local test host:** serve `~/catalog_out/publish/` over HTTP (e.g.
  `python3 -m http.server`); set `ATHENAEUM_CATALOG_BASE_URL=http://localhost:PORT/`.
- **End-to-end gate:** via the panel (or the command) download a FOV-selected
  set → `discover_layers` finds the tiers → a known frame solves (reuse the
  `corpus_layered_tiers`-style check against the installed app-data dir).
- **Phase 4 (separate, last):** upload `publish/` (manifest + 4 zips + sha256)
  to `artfrom.space/catalogs/` verbatim.

## Out of scope (YAGNI)

- Registration migration to tiers (deferred).
- Catalog v2 (per-star σ, BP−RP colour) and reactive download-on-solve-miss
  (deferred by the parent spec).
