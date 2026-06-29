# Tiered Additive Star Catalog — Design

**Date:** 2026-06-29
**Status:** Design approved; implementation pending.
**Repos touched:** `athenaeum` (catalog-builder + core + frontend), `solvemyastro` (submodule).

## Context

The plate-solver reads a star catalog built from Gaia DR3. Today it is a fixed
two-tier, **magnitude-limited** pair: a deep cache (`G≤19`, ~540 M stars,
load-bearing for verify) plus an optional bright sub-catalog (a flat `G≤16` cut)
used as a fast quad-matching first try. Two problems:

1. **Too sparse for small/sparse fields.** A flat magnitude cut leaves star-poor
   (high-galactic-latitude) sky with too few stars at small FOV — `G≤19` yields
   only ~800 stars/deg² in the sparsest cells, below what a ≲10′ field needs, so
   solving leans heavily on the deep fallback there.
2. **All-or-nothing download.** Users fetch one large catalog regardless of their
   rig's field of view.

ASTAP's modern databases solve exactly this by being **density-limited, not
magnitude-limited**: sorted to a target stars/deg², going as deep as `G≈21` in
star-poor sky and capping star-rich sky. The user wants the same idea, made
**additive**: download a sparse base, then download *only the extra stars* needed
for a smaller field — no re-download, no duplicated data — guided by a field-of-
view helper.

This spec replaces the magnitude-limited two-tier model with a **density-limited,
additive, multi-layer** catalog.

## Goals / non-goals

**Goals:** density-limited tiers covering wide→narrow FOV; additive layers with
zero star duplication; download only what a given FOV needs; a FOV helper that
recommends the layer set from focal length + sensor + pixel size; preserve (and
ideally improve) solve speed; pass the `corpus_bench` gate.

**Non-goals (this spec):** changing the solve algorithm itself; the deferred
"catalog v2" record (per-star σ, BP−RP colour); reactive download-on-solve-
failure (we chose proactive-only).

## 1. Catalog model — density-limited additive layers

- **4 cumulative tiers**, by density (stars/deg²). HEALPix-6 cell = 0.839 deg²:

  | Tier | density | per cell | covers FOV ≳ |
  | ---- | ---- | ---- | ---- |
  | base | 500 | 420 | 0.6° |
  | +Δ1 | 2 000 | 1 680 | 0.3° |
  | +Δ2 | 5 000 | 4 200 | 0.2° |
  | +Δ3 | 8 000 | 6 720 | 0.15° |

  (Density values follow ASTAP's tested FOV mapping; **our code uses its own
  naming** — `tier_<density>` — not ASTAP's letters.)

- **Each layer is a disjoint per-cell magnitude band.** Records are mag-sorted
  ascending within a cell, so each star has a deterministic *rank*; layers are
  rank ranges `[0,420) [420,1680) [1680,4200) [4200,6720)`. **A star lives in
  exactly one layer → zero duplication** in download, disk, or merge.
- **Stable per-cell ordering:** sort by `mag`, tie-break by `RA`, so a star's
  rank (hence layer) is identical across rebuilds — two equal-magnitude stars
  never drift between layers.
- **Sparse cells stop early.** A cell with few stars fills the base + part of Δ1
  and is simply absent from deeper layers — no padding. This is the density-
  limited behaviour: dense sky is capped, star-poor sky goes as deep as Gaia has.
- **Source depth:** built from a `G<21` Gaia ingest (Gaia's faint limit) so
  star-poor cells have faint stars to reach the higher densities.
- **On disk:** `catalogs/smac_gaia/` holding **per-tier directories**
  `tier_500/ … tier_8000/`, each containing a `stars.smac` (64-byte header +
  49 152-cell directory + 28-byte records). Per-tier dirs because
  `StarCache::open(dir)` reads `<dir>/stars.smac` (`cache.rs:120`) — so each
  layer is a cache dir, opened unchanged. The solver maps whichever tier dirs
  are present.

## 2. Build — `catalog-builder`

The existing `catalog-builder` crate is reused; the slicing and packaging change.

1. **Ingest `G<21`** from the raw Gaia mirror (`/Volumes/isos/gaia`) → HEALPix-6
   `healpix_*.bin` intermediate (the faint stars the old `G≤19` ingest dropped).
   The ingest magnitude limit (`gaia::GAIA_MAG_LIMIT`, `gaia.rs:53`) is raised to
   21.0 and becomes a `catalog-builder --mag-limit` flag (default 21).
2. **Slice 4 layers.** For each cell, take its mag-sorted records and cut the
   rank bands → 4 files via the existing `solvemyastro::cache::build_cache`. This
   is a small extension of `catalog-builder`'s per-cell selection
   (`hybrid_select` → `slice_select(lo_rank, hi_rank)`), with a unit test for
   band boundaries + disjointness. (Source of per-cell records — the mag-sorted
   `.bin` tiles directly vs an interim full-depth cache — is a Phase-1
   implementation choice; the deepest tier caps at ~330 M stars, so a full
   `G<21` deep is never stored as a product.)
3. **Package** each tier as `tier_<density>.zip` + `.sha256`, plus a
   **`manifest.json`**, into a ready-to-upload `--out/publish/` tree (exact
   layout + manifest schema in §5). The maintainer uploads its contents verbatim
   to `artfrom.space/catalogs/`. The old single `smac_gaia.zip` is retired with
   the two-tier model.

## 3. Solver — `solvemyastro`

Generalize the cache tier from a fixed `{deep, bright}` pair to an N-layer stack.
Verified integration points: `Caches` (`lib.rs:48`), `cone_cached`
(`cache.rs:272`), `PixelCache` (`cache.rs:447`), `cone_for_quad_match`
(`orchestrate.rs:267`), verify cone (`orchestrate.rs:1442`, on `caches.deep`,
`:868`).

- **`StarCache` (per-file reader) and `PixelCache` — unchanged.** Each layer is
  its own `StarCache` with its own per-solve `PixelCache` (the cache is keyed by
  `pixel_id` only, so each file needs its own).
- **New `LayeredCatalog`** holds the layers ordered base→deepest and exposes
  `cone_merged(ra, dec, radius, mag_limit, epoch, depth)`: query layers `[0..depth]`
  (each via its existing `cone_cached`) and **concatenate** the results. Because
  the bands are disjoint and consecutive in magnitude, the union is automatically
  mag-coherent and needs **no dedup**. The `mag_limit` early-exit works across
  files (each layer stops at its own slice).
- **`Caches` generalized:** `single(cache)` (one layer = today's behaviour) and
  `layered(vec)`.
- **Two call sites change:**
  - **quad-match (fast):** `cone_merged(depth = 1)` — base only; if the cone is
    too sparse (`bright_fallback_threshold`), retry `depth = all`. This
    generalizes today's bright→deep fallback.
  - **verify (NR count):** `cone_merged(depth = all)` — **union of all installed
    layers**. This is the real semantic change: today's deep is a *superset*
    catalog read as one file; the new layers are *disjoint deltas*, so full depth
    is a union, not a single read. `NR` = full installed-depth count, `mag_limit`
    respected.
- **Memory model (mmap):** opening a layer maps the file; only the ~590 KB
  directory is read up front, and record pages load on demand per touched pixel.
  A solve touches only its field's pixels → a few MB resident, never the whole
  catalog. N layers = N small directories + on-demand pages. (Same mmap model
  the previous single deep cache already used.)
- **Migration:** a legacy single `stars.smac` opens as a one-layer
  `LayeredCatalog`, so existing installs keep solving until layers are downloaded.
- **Gate:** this is the hot path → must re-pass `corpus_bench` (precision + no
  net-speed regression). With one layer, behaviour ≈ single-cache; the merge is
  cheap concatenation.
- **Layer discovery** (which `tier_*.smac` exist, ordering) lives in
  `athenaeum-core` (plate_solve); solvemyastro receives a ready `Vec`.

## 4. App — `athenaeum-core` + frontend (proactive model)

- **Layer download** (generalize `gaia_prebuilt.rs`): `download_catalog_layers(target_density)`
  reads `manifest.json`, downloads **only missing** tiers up to the target
  (resumable + SHA-256 + extract to `catalogs/smac_gaia/tier_*.smac`).
  Idempotent; no duplicate bytes.
- **Status** (generalize `get_catalog_status`): installed layers, total stars,
  effective density, and the smallest FOV covered.
- **FOV helper** — new sub-component in `PlateSolveSettingsPanel` ("Star catalog"
  section). Inputs: focal length (mm), sensor (camera presets with W×H + pixel,
  or manual pixel µm + resolution), binning. Computes (frontend)
  `pixel_scale = 206.265 · pixel_µm · binning / focal_mm` (″/px) and
  `FOV = pixel_scale · max(W,H)_px / 3600` (°), maps FOV → recommended tier
  (table §1), shows have-vs-need, and a "download the needed set" button. A few
  example rigs included for orientation.
- **Two backends (cardinal rule):** `download_catalog_layers` and
  `get_catalog_status` get the Tauri command **and** the mirrored Axum route in
  the same change; logic in `athenaeum-core`. FOV→tier math is frontend-only.
- **First-run default** (FOV not configured): recommend/download **base + Δ1
  (D20)** — covers most DSO fields.
- **UI** built with the project's frontend approach (design tokens, no raw
  colours); camera presets a static TS data file with manual override.

## 5. Hosting & on-disk layout

The app resolves a **catalog base URL** (default `https://artfrom.space/catalogs/`,
overridable via `ATHENAEUM_CATALOG_BASE_URL`; legacy `ATHENAEUM_STAR_CATALOG_URL`
/ `ATHENAEUM_GAIA_PREBUILT_URL` still accepted), fetches `manifest.json`, then the
tier zips. `catalog-builder` emits this exact tree under `--out/publish/`, which
the maintainer uploads verbatim to `artfrom.space/catalogs/`.

**Server (`artfrom.space/catalogs/`):**

```text
manifest.json
tier_500.zip    tier_500.zip.sha256
tier_2000.zip   tier_2000.zip.sha256
tier_5000.zip   tier_5000.zip.sha256
tier_8000.zip   tier_8000.zip.sha256
```

**Each `tier_<d>.zip` contains one cache dir — `tier_<d>/stars.smac`** — the
**delta layer** (disjoint magnitude band), *not* a cumulative catalog.

**Client install (`<app-data>/catalogs/smac_gaia/`):**

```text
tier_500/stars.smac   tier_2000/stars.smac
tier_5000/stars.smac  tier_8000/stars.smac
```

Extraction maps `tier_<d>/stars.smac` → `catalogs/smac_gaia/tier_<d>/stars.smac`
(zip-slip safe); the solver opens each `tier_<d>/` via `StarCache::open`. To
reach density `D`, the app downloads **every tier with `density ≤ D`** (base +
deltas).

**`manifest.json`** (filenames relative to the base URL, so mirrors / local test
servers work; no absolute URLs baked in):

```json
{
  "version": 1,
  "catalog_epoch": 2016.0,
  "tiers": [
    { "density": 500,  "zip": "tier_500.zip",  "sha256": "tier_500.zip.sha256",
      "dir": "tier_500",  "size_bytes": 0, "min_fov_deg": 0.6  },
    { "density": 2000, "zip": "tier_2000.zip", "sha256": "tier_2000.zip.sha256",
      "dir": "tier_2000", "size_bytes": 0, "min_fov_deg": 0.3  },
    { "density": 5000, "zip": "tier_5000.zip", "sha256": "tier_5000.zip.sha256",
      "dir": "tier_5000", "size_bytes": 0, "min_fov_deg": 0.2  },
    { "density": 8000, "zip": "tier_8000.zip", "sha256": "tier_8000.zip.sha256",
      "dir": "tier_8000", "size_bytes": 0, "min_fov_deg": 0.15 }
  ]
}
```

The downloader joins `<base>/<tier.zip>` and `<base>/<tier.sha256>` per tier and
verifies SHA-256 before extracting. Adding or retuning tiers is a manifest +
upload change — no app release.

## Phasing

| Phase | Repo | Work | Validation |
| ---- | ---- | ---- | ---- |
| **0. Re-bin (in progress)** | athenaeum | `G<21` ingest of the NAS mirror → `healpix_*.bin` on local disk | bins present; counts ≫ old `G≤19` |
| **1. Slice + build layers** | athenaeum (`catalog-builder`) | `--mag-limit` flag; `slice_select`; 4 tiers + `manifest.json` + zips | `cache-info` per tier; disjointness; per-cell density |
| **2. Layered solver** | solvemyastro | `LayeredCatalog` + `cone_merged` (union) + `Caches` stack + 2 call sites + migration | `corpus_bench`: 1-layer = baseline; multi-layer union solves |
| **3. App** | athenaeum (core + frontend) | discovery; `download_catalog_layers` + `get_catalog_status` (2 backends); FOV helper | end-to-end with locally-hosted test tiers |
| **4. Publish** | — (data) | build real tiers, host on artfrom.space, switch default, retire `smac_gaia.zip` | real sparse small-field solves; densities vs table |

Phase 0 is the prerequisite (the old local `G≤19` deep was deleted, so dev starts
from the new `G<21` bins). **MVP = Phase 1 + 2**: layered solve on real tiers,
before any UI — proves slice → union-query → correct solve + `corpus_bench`.

## Verification

- `cargo test --workspace` green incl. new tests: `slice_select` boundaries +
  disjointness; `cone_merged` union equals a single equivalent cache.
- `corpus_bench` re-passes (Phase 2) — the hard gate.
- `solvemyastro cache-info` on each tier: plausible counts + epoch J2016.
- Per-cell density of built tiers vs the §1 table (dense sky capped, sparse sky
  goes deep).
- End-to-end: `ATHENAEUM_STAR_CATALOG_URL`/manifest pointed at locally-hosted
  tiers → `download_catalog_layers` installs the right set → a known frame
  solves; a small sparse-sky field solves on the base alone less often than the
  old flat-G16 (the core-requirement check).

## Open items / future

- Ingest `--mag-limit` becomes a `catalog-builder` flag (Phase 1); the raised
  `GAIA_MAG_LIMIT` constant is the interim default.
- `build_cache`'s per-record scratch syscall is the slow step for large builds
  (`cache.rs`); batching per-pixel writes is an optional optimization if tier
  builds are too slow — out of scope unless it bites.
- Deferred "catalog v2" (per-star σ, BP−RP) is independent of this work.
