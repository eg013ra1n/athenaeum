# Plate Solving

Athenaeum's plate solver determines sky coordinates and a full WCS (with SIP distortion) for a FITS/XISF image. It can solve blind: the only hard input is the image itself. Anything the frame knows about where it points (RA/Dec, OBJECT, FOCALLEN and XPIXSZ, DATE-OBS) is passed to the solver as a hint that narrows and speeds up the search, never as a requirement.

The solver is `solvemyastro`, a git submodule (`solvemyastro/` at the repo root, its own crate) that matches detected stars against tiered Gaia-derived catalog layers. Athenaeum wraps it in `crates/athenaeum-core/src/plate_solve/`.

> **Status of this document.** Current: Architecture Overview, Per-Frame Pipeline, Database Storage, Configuration, Key Source Files, Tauri Commands, the "frame-set clustering" failure mode, "Not Yet Implemented" and the 2026-10 changelog entry. Kept as history of the retired Tycho-2 quad-index solver (replaced by `solvemyastro` + Gaia layers): Pre-Built All-Sky Quad Index, Tiered Star Cache (the legacy deep + bright pair), the Per-Frame Pipeline of the Quad-Index Solver (with its Performance and Accuracy), Debug & Diagnostic Tools, the dense-field failure mode and the older changelog entries. Every file, type and tool named only in those history sections no longer exists. The input and acceptance gates are specified in [`input-and-acceptance-gates.md`](./input-and-acceptance-gates.md).

## Architecture Overview

```text
 Settings -> Plate Solving: download catalog tiers
        |  catalog::gaia_prebuilt::download_catalog_layers
        v
 <app-data>/catalogs/smac_gaia/tier_<density>/stars.smac   (additive tiers)

 plate_solve_batch(frame_ids)                        (Tauri command / Axum route)
   |
   |- load config (plate_solve.config), open the tier stack once
   |  (plate_solve::open_catalog_layers)
   |- per frame, on the calling thread:
   |     input gate  (service::input_gate_reason, from the stored analysis)
   |     hints       (hints::extract_hints + apply_object_name_fallback)
   |- worker pool (batch_concurrency), per frame:
   |     service::solve_frame_with_hints
   |       -> solvemyastro::solve(file, hints, Caches::layered, SolveConfig)
   |            star detection, streak cut, autoFOV ladder, quad match,
   |            affine seed, verification, refit, SIP fit
   |       -> SolveResult (WCS + SIP, inliers, rms, scale, derived focal length)
   |- back on the calling thread, one transaction:
         service::store_result
           -> acceptance gate (service::acceptance_refusal / blind_gate_ok)
           -> plate_solves row  (storage::insert_plate_solve)
           -> frames write-back (storage::update_frame_from_solve)
           -> frames.object label from the nearest DSO, if empty
```

A solve is refused or fails at one of three places, each with its own reason shown in the UI: the **input gate** (trailed stars, before any solver work), the **solver** (a `solvemyastro::SolveFailure`, translated by `plate_solve::failure::describe_solve_failure` into a stable code and a message), and the **acceptance gate** (`REJECTED_LOW_CONFIDENCE`, nothing written).

The `solver` cargo feature of `athenaeum-core` (on by default) pulls in `solvemyastro`; `cargo check --no-default-features` does not compile plate solving.

### The catalog layers

`solvemyastro` reads memory-mapped `stars.smac` caches organised as an **additive stack of density tiers** (disjoint magnitude bands, base to deepest) under `<app-data>/catalogs/smac_gaia/tier_<density>/`. `plate_solve::layers`:

- `discover_layers(root)` lists the `tier_<density>/` directories that hold a `stars.smac`, ascending by density. With no tiers but a bare `stars.smac` directly under the root, that single cache is a one-layer fallback (old installs keep solving). With neither, it returns nothing.
- `open_catalog_layers(app_data_dir)` opens each layer as a `solvemyastro::StarCache`. It is the one opener: both hosts and the stacking project path call it. A missing catalog is `CatalogOpenError::NotInstalled`; the hosts turn it into "download the catalog from Settings -> Plate Solving".
- `catalog_identity(app_data_dir)` is a stat-only fingerprint (tier directory name, `stars.smac` size and mtime) used in the hash of a cached stacking solve.

Inside `solvemyastro` (`Caches::Layered`), quad matching uses the base layer with fallback to the full union, and verification uses the full union. The Settings tab shows one row per tier from the published manifest, recommends a tier from the narrowest field of view among the library's light frames (`get_frame_fov_summary`, `plate_solve::frame_fov_summary`) and downloads the chosen tier plus every lower one not yet installed (`download_catalog_layers`). The design is in `docs/superpowers/specs/2026-06-29-tiered-additive-star-catalog-design.md` and `docs/superpowers/plans/2026-06-30-layered-solver.md`.

### Code organisation

| Layer | Location | Role |
| ---- | ---- | ---- |
| Solver | `solvemyastro/` (submodule) | Detection-to-WCS pipeline, star caches, SIP fit, structured failures. |
| Image types and detection | `rustafits` (`astroimage`) | FITS/XISF decode and fast star detection; `astroimage::platesolving` supplies the `SolveHints` and `WcsSolution` types the app-side API uses. |
| Orchestration | `crates/athenaeum-core/src/plate_solve/` | Hints, config, catalog layers, the solve adapter, the gates, storage, DSO lookup. |
| Commands | `crates/athenaeum-tauri/src/commands/plate_solve.rs`, `crates/athenaeum-web/src/routes/plate_solve.rs` | Batch driver, progress events, persistence. |
| Frontend | `src/components/plate-solve/` | Settings panel and batch panel. |

## Pre-Built All-Sky Quad Index

_History: the retired Tycho-2 quad-index solver (replaced by `solvemyastro` + Gaia layers); kept for the record._ `index_builder::IndexBuilder`, `QuadEntry`, `quad_index.bin`, `catalog/tycho2.rs` and `plate_solve/quad_index.rs` no longer exist.

Blind solving works because the solver has a pre-computed, memory-mapped index of every nearest-neighbor quad in the catalog, organised by a scale-invariant hash. A single hash lookup can find candidate matches anywhere on the sky — no trial positions, no sweeps.

### What a "quad" is

A quad is four stars — one star plus its three nearest neighbors by great-circle distance. Four stars form six pairwise distances. When those distances are sorted and divided by the longest, the result is **five dimensionless ratios** in `[0, 1]`. Those ratios depend only on the geometric shape of the four stars — they are scale-invariant, rotation-invariant, and reflection-invariant.

For every star in the Tycho-2 catalog brighter than the configured magnitude limit (`index_mag_limit`, default **13.0** — the practical Tycho-2 ceiling; previously 11.0), the index builder:

1. Loads the star and its nearest 3 neighbors (searched within the star's HEALpix pixel plus 8 neighbors, so quads are consistent across pixel boundaries).
2. Computes the 5 scale-invariant distance ratios.
3. Quantises each ratio to an integer using `hash_tolerance` (default 0.005) to produce a 5-element integer key.
4. Writes a `QuadEntry` to disk: the hash key, the longest side in degrees (for scale sanity at solve time), and the 4 stars' `(RA, Dec)` in f32.

Entries are bucketed by the first hash-key component for O(1) lookup with `±1` multi-probe.

### File format

File: `{app_data}/catalogs/tycho2/quad_index.bin`

```text
header:
  magic            [u8; 4]  = "QIDX"
  version          u32      = 1
  hash_tolerance   f64
  num_buckets      u32      = 1024
  star_count       u64
  quad_count       u64
bucket table:
  [u64 offset, u32 length] × num_buckets
entries:
  QuadEntry × quad_count   (56 bytes each)

QuadEntry (56 bytes):
  hash_key           [i32; 5]   (20 bytes)
  longest_dist_deg   f32        (4)
  stars_ra           [f32; 4]   (16)
  stars_dec          [f32; 4]   (16)
```

### Build performance

Tested on a MacBook Pro (M1) against the installed Tycho-2 catalog. The default was raised from mag 11 to mag 13 in 2026-04 after the mag-11 index was found to be too shallow for long-exposure OSC frames in medium-density fields (see **Known Failure Modes** below).

| Metric | Mag 11 (old default) | Mag 12 | **Mag 13 (default)** |
| ---- | ---- | ---- | ---- |
| Input | 49,152 HEALpix files, 192 MB total | same | same |
| Quads produced | 860,738 | 1,930,347 | **2,418,335** |
| Output file size | 46 MB | 103 MB | **129 MB** |
| Build time (M1) | ~12 s | ~26 s | **~41 s** |

Past mag 13 the Tycho-2 catalog itself is exhausted (99.9 % saturation), so values higher than 13.0 add no real stars and are treated as a no-op.

### Choosing a magnitude limit

Verified star counts within a 1.7° cone around three reference sky regions (covers a typical 3.2° FOV):

| Region | mag ≤ 11 | mag ≤ 12 | mag ≤ 12.5 | mag ≤ 13.0 | mag ≤ 14 |
| ---- | ---- | ---- | ---- | ---- | ---- |
| Barnard 150 (Cepheus) | 551 | 1,176 | 1,406 | 1,455 | 1,463 |
| Orion | 558 | 912 | 986 | 996 | 996 |
| Galactic centre | 785 | 1,684 | 1,857 | 1,872 | 1,873 |

Rule of thumb: if bright stars in your image saturate (common for 180 s+ OSC exposures of dense fields), you need a deeper index because the detector's top-N is shifted down into the mag 9–13 range that the old mag-11 index did not cover.
## Tiered Star Cache (Bright + Deep, 2026-05)

_History: the legacy deep + bright cache pair of the `solvemyastro` Gaia pipeline (2026-05), superseded by the additive density-tier stack described in Architecture Overview. `solvemyastro::Caches::Legacy`, `Caches::deep_only`/`tiered` and `service::solve_frame_tiered` still exist, but the hosts and the stacking solve open the tier stack (`open_catalog_layers`) and pass `Caches::layered`. `PlateSolveConfig::bright_cache_path` is unread, and `require_bright_cache` and `ServiceContext::bright_cache` (named below) were retired. The measurements and the on-disk audit below describe the `smac_gaia` / `smac_gaia_bright` caches of that period._

At that time the hot path consulted **two on-disk star caches**, not one:

| Cache | Convention path | Depth | Size | Used by |
| ---- | ---- | ---- | ---- | ---- |
| **Deep** | `<app-data>/catalogs/smac_gaia/` | Gaia DR3 G<19 | ~540 M stars | Verify stage (always) + quad-matching cone when no bright cache is configured **or** when the bright cone is too sparse |
| **Bright** (optional) | `<app-data>/catalogs/smac_gaia_bright/` | Hybrid (see below) | ~70.5 M stars (~2.0 GB) | Quad-matching cone, tried *first* |

Both are produced by `solvemyastro`'s SMAC writer. They are independent files — the bright cache is *derived* from the deep cache via the `build-bright-cache` subcommand, not from raw Gaia ingest.

### Hybrid build algorithm

Per HEALPix-6 cell (49,152 cells across the sky), the bright cache is filled with:

1. **Floor** — every star with `mag < bright_floor` (athenaeum ships with floor = **16**).
2. **Top-up** — if the floor count is below `target_density` (default 100), append the next-brightest stars (continuing past the floor up to the deep cache's G<19 ceiling) until reaching the target *or* exhausting the cell.
3. **No hard ceiling** — Milky-Way-plane cells keep all 500+ bright stars; sparse polar cells get e.g. 30 floor + 70 top-up = 100.

Net effect: bright-rich regions get a true G<16 sub-cache, sparse regions get an opportunistic density-100 sub-cache. The bright cache never goes deeper than its source (G<19), so the deepest "top-up" star is around G≈19, not G≈21.

> **Reality of the shipped cache (see "Verified on-disk contents" below):** in the build athenaeum actually ships, the `target_density: 100` top-up **never engaged** — even the sparsest HEALPix-6 cell already holds 144 stars brighter than G16, which is > 100. So the delivered `smac_gaia_bright` is effectively a *clean G≤16 magnitude cut*, not a per-cell density cap. The hybrid algorithm above is what the tool *would* do; the floor simply dominated everywhere.

### Why floor=16 (and not the CLI default of 14)

The `build-bright-cache` CLI defaults `--bright-floor` to 14, but athenaeum's documented build command uses **16**:

```bash
solvemyastro build-bright-cache --from <smac_gaia> --out <smac_gaia_bright> \
    --bright-floor 16 --target-density 100 --mode hybrid
```

Reasoning: typical user exposures (120–300 s OSC subs) detect down to m ≈ 14–16. A floor=14 bright cache would simply have no catalog counterpart for the dimmer half of the detections, the quad-matching cone would fall under threshold on most cells, and the auto-fallback would punt every cone to the deep cache anyway — wiping out the speedup. Floor=16 aligns the cache with what the cameras actually see while still being **7.7× smaller** than the deep G<19 (70.5 M vs 540 M).

### Auto-fallback rule

In `solvemyastro::orchestrate::cone_for_quad_match`:

1. If a bright cache is configured, the quad-matching cone is queried against it first.
2. If the cone returns **≥ `SolveConfig::bright_fallback_threshold` (default 30)** stars, those are used.
3. Otherwise the same cone is retried against the deep cache and *those* stars are used instead.

The fallback returns one set, never a union. The deep cache is therefore always reachable as a safety net for HEALPix singular caps or other geometric degeneracies.

### Verify stage is deep-only

The Bayesian log-odds gate in `solvemyastro::verify` needs `NR = true number of catalog stars in the FOV` to score solves correctly. Using the thinned bright cache there would inflate the log-odds and admit false solves, so the verify cone is hard-coded to the deep cache regardless of whether a bright cache is configured. **Only quad matching is tiered.**

### Athenaeum integration

Plumbing added in `feat(plate_solve): bright sub-catalog integration` (2026-05-25):

| Layer | Change |
| ---- | ---- |
| `athenaeum-core` | `PlateSolveConfig::bright_cache_path: Option<String>` (serde default `None` — old configs deserialize unchanged). `ServiceContext::bright_cache: Arc<RwLock<Option<Arc<StarCache>>>>`. New `service::solve_frame_tiered` convenience + a `bright_cache: Option<&StarCache>` parameter on `solve_frame_with_hints` (`None` = pre-bright behaviour). |
| `athenaeum-tauri` / `athenaeum-web` | `require_bright_cache(state) -> Option<Arc<StarCache>>` helper, mirroring `require_star_cache`. Resolution order: explicit `PlateSolveConfig::bright_cache_path` → convention path `<app-data>/catalogs/smac_gaia_bright/`. Cache is opened once per process and stashed in `ServiceContext`. Single-frame and batch entry points open it once and pass it through. |

Cache opening is **best-effort and never fatal**: a missing directory logs `plate_solve: no bright sub-catalog at <path> — using deep cache only` and the solve continues against the deep cache. A corrupt cache logs the open error and likewise falls back.

### Measured impact

Corpus_bench, 19 frames, M1:

| Metric | Deep-only | Tiered (bright + deep) |
| ---- | ---- | ---- |
| Truth-matched | 14/14 | **14/14** (no regression) |
| Successful solves | 19/19 | **19/19** |
| Wrong-position solves | 0 | **0** |
| Wall-clock total | 52.4 s | **19.66 s (−62 %)** |
| Typical hinted solve | ~1700 ms | **~230 ms (−86 %)** |
| Slowest blind solve | ~12 s | **~5 s** |

### One-time build

```bash
solvemyastro build-bright-cache \
    --from   <app-data>/catalogs/smac_gaia \
    --out    <app-data>/catalogs/smac_gaia_bright \
    --bright-floor 16 --target-density 100 --mode hybrid
```

~5 minutes on M1. Produces `stars.smac` plus a sidecar `bright.meta.json` recording `source_smac_size`, `source_epoch`, `bright_floor`, `target_density`, `mode`, `output_star_count` for stale-cache debugging.

`--mode` accepts `hybrid` (default; the algorithm above), `floor-only` (skip the density top-up), or `cap-only` (no floor, just take the brightest `target_density` per cell). Production uses `hybrid`.

### Verified on-disk contents (2026-06-01 audit)

Confirmed by parsing the `.smac` header + the full 49,152-entry HEALPix-6 pixel
directory + sampled record magnitudes (format spec in `solvemyastro/src/cache.rs`:
64-byte header with `u64 star_count`@16 / `f64 epoch`@24, then a `{u64 offset,
u32 count}` directory, then 28-byte records `f64 ra, f64 dec, f32 mag, f32 pmra,
f32 pmdec`, mag-sorted ascending within each cell). Per-cell counts sum exactly to
the header `star_count` in both files.

| Catalog | Stars | File size | Mag cut | Per-cell min / median / max | Stars/deg² (median / sparse) | Empty cells |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| **Deep** `smac_gaia` | 540,600,543 | ~15.1 GB | **hard G≤19.0** | 685 / 3,252 / 443,281 | 3,875 / 816 | 0 |
| **Bright** `smac_gaia_bright` | 70,541,101 | ~2.0 GB | **hard G≤16.0** | 144 / 656 / 46,497 | 782 / 172 | 0 |

Key findings:

- **Bright is a clean magnitude cut, not a density cap.** Every cell — sparsest
  (n=144) to densest (n=46,497) — ends at G≈16.00; the `target_density: 100`
  top-up never fired (sparsest cell already > 100 stars to G16). So dense
  galactic-plane cells still hold ~46 k stars and sparse polar cells ~144 — a
  ~320× spread, *not* a uniform ~100/cell. This is fine (arguably better:
  uniform G≤16 depth, full bright-star coverage for verification) — just don't
  read `bright.meta.json`'s `target_density: 100` as the operative limit.
- **Both catalogs cover the whole sky with zero empty cells**, north and south —
  hemisphere is irrelevant to the solver. Only galactic latitude (plane = dense,
  poles = sparse) changes density.
- **The deep catalog is much deeper than strictly needed for solving** (G≤19,
  540 M; ~26× the stars of a G<16 build). It is intentional: it both backs the
  verify stage and rescues hard sparse/long-focal-length fields. The solver reads
  it to `SolveConfig::catalog_mag_limit = 19.0` (set in `service.rs`).
- A third folder, **`<app-data>/catalogs/gaia_dr3/`** (~7.1 GB, 49,152
  `healpix_*.bin` raw bins), is the **build input only** — consumed by
  `build-cache` to produce `smac_gaia/stars.smac`; it is *not* read at solve
  time. Keep it only if you want to rebuild the `.smac` files without
  re-downloading from Gaia.

### When is the bright tier enough vs. when do you need deep?

You never choose manually — the per-cone auto-fallback (threshold 30, above)
switches automatically, and the verify stage always uses deep, so the deep cache
is **mandatory** and the bright cache is a speed layer on top. But it is useful to
know where the bright tier's *in-field* stars run thin, because that is what
determines whether matching leans on bright or on deep.

**The determinant is field of view (arcmin), not focal length on its own** —
focal length only matters together with sensor size:

| Field width | Bright (G≤16) stars in frame | Verdict |
| ---- | ---- | ---- |
| **≳ 20′** | ≥ ~20 even in sparse sky | Bright is plenty |
| **~10–20′** | ~8–20 (sparse) | Bright OK at median density; thin off the plane → deep carries it |
| **≲ 10′** | ≤ ~3 in sparse sky | Need deep (G≤19) for reliable matching + verification |

Translated to focal length for three sensor sizes (Gaia density: bright median
782 / sparse 172 per deg²; deep median 3,875 / sparse 816):

| Sensor (long axis) | Bright plenty up to | Transition | Need deep beyond |
| ---- | ---- | ---- | ---- |
| Small (ASI533, 11.3 mm) | ~1,950 mm | 1,950–3,900 mm | ~3,900 mm |
| APS-C (ASI2600/294, 15.6 mm) | ~2,700 mm | 2,700–5,400 mm | ~5,400 mm |
| Full-frame (ASI6200/QHY600, 36 mm) | ~6,200 mm | 6,200–12,000 mm | ~12,000 mm |

Worked example (APS-C 4144×2822 px @ 3.76 µm), catalog stars in the frame — the
bold sparse-sky figures are what bind first:

| Focal length | Field | Bright (med / sparse) | Deep (med / sparse) |
| ---- | ---- | ---- | ---- |
| 530 mm | 101′×69′ | 1511 / 332 | 7487 / 1577 |
| 1000 mm | 54′×37′ | 424 / 93 | 2103 / 443 |
| 2000 mm | 27′×18′ | 106 / **23** | 526 / 111 |
| 4000 mm | 13′×9′ | 27 / **6** | 131 / 28 |
| 6500 mm | 8′×6′ | 10 / **2** | 50 / 10 |

Takeaways:

- **Wide/medium fields (camera lenses through ~2,000–2,700 mm on APS-C, longer on
  full-frame):** hundreds of G≤16 stars in frame — bright alone would match fine.
- **Long focal lengths are where deep earns its keep:** below ~10′ field
  (≈ >5,400 mm on APS-C, >3,900 mm on a small sensor, >12,000 mm on full-frame),
  G≤16 drops to a handful in sparse sky and you need G≤19 to clear the 4-inlier
  acceptance floor (`DEFAULT_MIN_ABSOLUTE_INLIERS = 4`).
- **A bigger sensor pushes the whole boundary to longer focal length.** Example:
  6,500 mm is an 8′ field on APS-C (bright too thin → leans on deep) but a 19′
  field on full-frame (bright still plenty).
- **Recommendation: keep both caches.** Dropping the 15 GB deep (bright-only)
  is acceptable *only* if every rig you solve stays wide/medium-field (≳20′),
  and even then it weakens verification everywhere (verify is always deep) and
  breaks long-focal-length / sparse-sky solves. Deep is the load-bearing
  catalog; bright is a speed optimization on top of it.
## Per-Frame Pipeline

The batch driver is `plate_solve_batch` in the Tauri command and its Axum mirror. Core exposes `service::solve_frame`, `solve_frame_tiered`, `solve_frame_layered` (single-frame conveniences, mostly for tests and tools) and `solve_frame_with_hints`, the hot path both hosts and the stacking solve call. There is no single-frame solve command.

### 1. Load and gate (calling thread, holds the DB)

Per frame: `load_frame_with_path`, then the **input gate** `service::input_gate_reason`: when `input_gate_enabled` and the frame has a stored analysis whose `median_eccentricity >= input_max_eccentricity` AND `trail_r_squared >= input_min_trail_r2`, the frame is refused ("Stars are trailed ...") and never reaches the solver. A frame without an analysis is never refused here.

### 2. Hints

`hints::extract_hints(frame, conn)` builds a `SolveHints` (RA, Dec, FOV, pixel scale, epoch). Position priority: sexagesimal `objctra`/`objctdec` (the planned target), then numeric `ra`/`dec`, then the stored FITS-header WCS (`CRVAL1`/`CRVAL2`), then a nearby solved frame in the same directory. Sentinel positions (NULL, exact 0/0, `00:00:00`) are rejected. FOV and pixel scale always come from `focallen`, `xpixsz` and `naxis1`; `XPIXSZ` is already the effective pitch of the saved image, so binning is not applied again. If the header gave no usable position at all, `hints::apply_object_name_fallback` resolves the frame's OBJECT against the bundled DSO catalog (`plate_solve/dso_lookup.rs`) and uses the object's centre; a recorded position always wins. `service::solve_frame_with_hints` adds the real per-frame epoch (`observation_epoch`, from `date_obs`) so catalog proper motions are propagated to the shooting date.

### 3. Solve

`solve_frame_with_hints` calls `solvemyastro::solve(file, hints, Caches::layered(..), SolveConfig, cancel)` with a `SolveConfig` whose recipe is fixed in code: `quad_tolerance` 0.007, `catalog_mag_limit` 19.0, `fit_sip` true, `sip_order` from `PlateSolveConfig`. Changing any of those recipe constants, the gate or the catalog layering means bumping `config::SOLVE_RECIPE_VERSION`. The solver (see `solvemyastro/src/orchestrate.rs`) detects stars with `astroimage`'s fast detector, ranks them by SNR and drops streaks (eccentricity above 0.8), refuses a frame whose detections are almost all streaks (`looks_trailed`), then walks an autoFOV ladder (a hinted scale gives a short ladder around it; blind, it runs from 9.5 deg down to 0.38 deg in steps of 1/1.5) and a square-spiral search of sky cells, around the hinted position (default search radius 10 deg) or the whole sky. In each cell it cones the catalog, projects it, builds quads, matches and fits an affine, seeds a WCS, counts inliers, refits and fits SIP. The solver itself never emits a solution below 6 inliers (`MIN_ABSOLUTE_INLIERS`).

The `SolveResult` carries the WCS with its SIP polynomials, `matched_stars`, `total_detected`, the RMS residual in pixels and arcseconds, pixel scale, field rotation, solve time, `catalog_used` and `algorithm_used` (both reported by the solver), `inlier_ratio` (the solver's own), and:

- `expected_catalog_stars_in_fov`, counted by a cone of G <= 12 around the solved centre (the base layer, for a tier stack).
- `derived_focallen_mm`, inverted from the solved pixel scale and `XPIXSZ` (or the `camera_defaults` entry for the frame's `INSTRUME`/`TELESCOP` when `XPIXSZ` is missing). `focallen_corrected` is set when the header had no focal length, or when the derived value differs from it by more than 2 %, which means the header scale hint was wrong.

A failure is returned as the solver's own error so `describe_solve_failure` can downcast the structured `SolveFailure`. Each frame is wrapped in `catch_unwind`: a panic becomes a per-frame failure and the batch continues. A cancel flag (`cancel_plate_solve`) is passed to the solver.

### 4. Acceptance gate and persistence (calling thread, one transaction)

`service::store_result` first runs `acceptance_refusal` (only when `blind_gate_enabled`), which applies `blind_gate_ok`. A solve is refused when any of these holds:

- the RMS residual exceeds `blind_rms_max_px_mult` times the adaptive pixel tolerance, `clamp(base_verification_tolerance_arcsec / pixel_scale_arcsec, 4, 20)` px (`adaptive_tol_px`);
- the inlier count is below `blind_inlier_floor`;
- the field is dense (`expected_catalog_stars_in_fov > 100`) and `inlier_ratio` is below `blind_min_inlier_ratio` (the primary false-positive discriminator; sparse fields are exempt);
- the recovered pixel scale is outside `blind_scale_sanity_min`..`blind_scale_sanity_max` arcsec/px;
- the header declares a pixel scale (from `focallen` and `xpixsz`) and the recovered scale differs from it by more than the factor `blind_scale_header_tol`.

A refusal writes nothing and reaches the UI as a failure with code `REJECTED_LOW_CONFIDENCE`. The same pure check, `acceptance_refusal`, judges a frame in the stacking project path (`stacking::solve`), which also calls `solve_frame_with_hints` and `open_catalog_layers`; see `docs/stacking/README.md`. An accepted solve is written to `plate_solves` and the `frames` summary columns (see Database Storage), and, through `plate_solve::dso_lookup::DsoCatalog::find_best`, the nearest named object labels `frames.object` when it is empty.

## Per-Frame Pipeline of the Quad-Index Solver (History)

_History: the retired Tycho-2 quad-index solver (replaced by `solvemyastro` + Gaia layers); kept for the record._ `CatalogEngine`, `QuadIndex`, `try_solve_pass`, `required_inliers`, `retry_passes`, `use_fast_detection` and `max_image_stars` no longer exist, and the numbers below were measured on that solver. The current flow is the Per-Frame Pipeline above.

The solver entry point is `athenaeum_core::plate_solve::service::solve_frame()`.

### Inputs

```rust
pub fn solve_frame(
    frame: &Frame,
    file_path: &str,
    conn: &Connection,
    catalog: &CatalogEngine,
    index: &QuadIndex,
    config: &PlateSolveConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> Result<SolveResult>;
```

- **`frame`** — database record for the image. Its `focallen`, `xpixsz`, `naxis1`, `naxis2` and `date_obs` fields are read as hints for the scale filter and proper-motion epoch. If any of these are missing, the solver still runs; it just loses the scale filter optimization.
- **`file_path`** — filesystem path to the actual FITS/XISF file. This is the only hard requirement.
- **`conn`** — SQLite connection (used for reading settings and the neighbor-frame fallback in hints).
- **`catalog`** — `CatalogEngine` for the Tycho-2 HEALpix catalog. Used at the verification stage for a cone search around the derived position.
- **`index`** — pre-loaded all-sky `QuadIndex` (memory-mapped, shared across all solves in `ServiceContext`).
- **`config`** — `PlateSolveConfig` loaded from the `settings` table.
- **`thread_pool`** — optional rayon thread pool for the star detector.

### Step-by-step

#### 1. Star detection (≈ 0.3–0.5 s release, ~5 s debug, on a 6 k × 4 k frame)

Delegated to `rustafits::ImageAnalyzer`. Two modes available via `config.use_fast_detection`:

- **Fast (default, `use_fast_detection=true`)** — `ImageAnalyzer::detect_fast` runs FITS/XISF decode → debayer (green-channel interpolation for Bayer) → mesh-grid background → separable Gaussian matched-filter convolution → 5σ peak detection → intensity-weighted centroid extraction. No Moffat PSF fit, no two-pass FWHM calibration. Yields `(x, y, flux)` triples. ~400 ms release on a 6 k × 4 k frame.
- **Precise (`use_fast_detection=false`)** — full `ImageAnalyzer::analyze` pipeline with Moffat PSF fits, used by the analysis tab. Not significantly more accurate for solving purposes; kept only for comparison / debugging.

The solver keeps only the `(x, y, flux)` triples. Detection is capped at `max(retry_passes) × 1.67`, but not less than 500, so the retry loop has enough stars for its largest pass. If fewer than 20 stars are detected, the solve fails immediately.

#### 2. Progressive retry passes (Phase 4, 2026-04)

The solver **does not use `max_image_stars` directly**. Instead it runs up to 4 attempts in increasing size from `config.retry_passes` (default `[50, 150, 300, 600]`). Each pass builds its own image quads from the brightest N detected stars and runs the full hash-lookup → scale-filter → per-candidate verify cycle below. The first pass that meets the density-aware acceptance gate wins and the loop short-circuits.

Rationale:

- The 50-star preamble targets dense galactic-plane fields where only the very brightest detected stars reliably match indexed stars.
- Passes 2–4 back off to progressively larger star sets, catching sparse/dim fields and long-exposure frames where bright stars have saturated and were excluded by the detector.

Typical outcomes:

- Sparse high-galactic-latitude field → pass 1 solves with 100+ inliers in < 500 ms.
- Dense Milky Way field → needs pass 3 (300 stars) for enough real nearest-neighbour overlap with the deep catalog.

#### 3. Build image quads (≈ 1 ms per pass)

`rustafits::platesolving::build_quads(positions, pass_size)` takes the brightest `pass_size` detected stars and forms one quad per star: the star itself plus its 3 nearest neighbors in pixel space. Each quad stores:

- The 4 star indices into the original star list
- The 5 sorted, normalised distance ratios
- The quad centroid (centre of mass) in pixels
- The longest pairwise distance in pixels

`image_positions` is computed once per frame (not per pass) and shared across passes.

#### 4. Hash lookup (≈ 80–200 ms per pass)

For each image quad, quantise its 5 ratios with the index's `hash_tolerance` and look up the resulting hash key in `QuadIndex::lookup()`. The lookup checks the target bucket plus ±1 neighbor buckets, and verifies all 5 hash dimensions are within ±1 of the query.

A deeper mag-13 index returns 2–3× more candidate hash matches per lookup than the old mag-11 index, so the scale filter matters more.

#### 5. Scale filter — ±5 % (Phase 4, 2026-04)

If `FOCALLEN` and `XPIXSZ` are present in the frame metadata, the solver computes the expected pixel scale in arcsec/px:

```text
expected_scale = atan(xpixsz_mm / focallen_mm) × (180/π) × 3600   # XPIXSZ already includes binning (plate_solve/hints.rs)
```

Each hash-match candidate has an implied scale from `catalog_quad.longest_dist_deg × 3600 / image_quad.longest_dist`. Candidates whose implied scale disagrees with `expected_scale` by more than **±5 %** (`filter_scale_tolerance = 0.05`) are discarded.

Real cameras report pixel scale to < 1 % accuracy, so a 5 % band is still loose enough to never reject a correct candidate. Tightening from the previous ±10 % roughly halves the candidate count going into the verify loop, mattering most with the deeper mag-13 index. A separate looser `scale_tolerance = 0.10` is kept for downstream refit/WCS sanity checks so a fit that drifts slightly during convergence isn't prematurely rejected.

Frames without `FOCALLEN` or `XPIXSZ` skip this filter and try all candidates — slower but still works.

#### 6. Per-candidate solve + verify

For each remaining candidate:

1. **Resolve the 4-star correspondence** — brute-force all 24 permutations of catalog-to-image pairing, fit a similarity transform to each, pick the permutation with the smallest fitting residual.
2. **Fit similarity transform** — from the 4 best pairs, solve for `(xi, eta) = M × (px - cx, py - cy) + t` where `(xi, eta)` are tangent-plane coordinates (radians) around the catalog-quad centroid.
3. **Build a `WcsSolution`** — convert the affine to a standard WCS.
4. **Sanity check** — pixel scale in `[0.1, 30]` arcsec/px and, if the user supplied `FOCALLEN+XPIXSZ`, agreement within 10 %.
5. **Cone-search verify** — run a catalog cone search of radius `0.7 × FOV` around the derived position, project every catalog star through the WCS, and count inliers. Cone search results are cached per pass (Phase 3.1) — subsequent candidates whose FOV fits inside the cached cone reuse the same verify stars, saving ~200 ms on typical frames.
6. **Translation refit** — when the seed has ≥ 6 loose-tolerance inliers (counted at `2 × adaptive_tolerance`), iteratively fit a 4-parameter similarity (translation + rotation + uniform scale) in closed form via complex LSQ. Tolerance tightens from 3× → 1× adaptive across 4 iterations. The WCS returned by refit is **always re-counted at the tight tolerance** (Phase 4.1 fix) so `final_inliers` carries consistent semantics downstream.
7. **Early exit within a pass** — if `final_inliers ≥ required × 1.5` the loop stops (was `× 2` before Phase 4, which kept churning on already-good solves).

The candidate with the highest tight-tolerance inlier count is kept per pass.

#### 7. Adaptive verification tolerance

The fixed `verification_tolerance_px = 10` of the legacy pipeline was too tight on slightly-defocused frames and too loose on sharp narrow-FOV frames. Replaced by:

```text
tol_px = clamp(base_verification_tolerance_arcsec / pixel_scale_arcsec, 4 px, 20 px)
```

Default `base_verification_tolerance_arcsec = 8.0`. At 1.87 "/px this gives 4.3 px (tighter = fewer false matches); at 0.5 "/px it gives 16 px (looser = catches slightly-defocused stars).

#### 8. Density-aware acceptance gate (Phase 4.2)

Replaces the fixed `min_matched_stars ≥ 15` of the legacy pipeline. Given `E` catalog stars in the FOV (from the verify cone search) and `D` image stars detected, the effective density is:

```text
effective = min(E, D)
```

and the required inlier count is:

```text
if effective == 0:       floor (config.min_matched_stars, default 6)
elif effective <= 30:    max(6, floor)
elif effective <= 100:   max(round(0.20 × effective), floor)
else:                    max(round(min_inlier_ratio × effective), 20, floor)
```

Capping by `D` prevents the classic false-negative where a dense galactic-plane FOV with 3 500 catalog stars demands 350 inliers, but only 600 stars are detected so matching that threshold is physically impossible. For a Milky Way FOV with 3 500 catalog / 600 detected, required becomes `max(20, 0.10 × 600) = 60` — strict enough to reject coincidences, reachable when the correct candidate is found.

Failure messages include the seed RA/Dec, density counts, and a "consider rebuilding the quad index with a higher magnitude limit" hint for high-density FOVs.

#### 9. Assemble `SolveResult`

```rust
pub struct SolveResult {
    pub wcs: WcsSolution,           // full WCS: crpix, crval, cd, optional SIP
    pub matched_stars: usize,       // inlier count at TIGHT tolerance (Phase 4.1 fix)
    pub total_detected: usize,      // stars detected by ImageAnalyzer
    pub rms_residual_px: f64,       // RMS of the verified inlier residuals
    pub rms_residual_arcsec: f64,
    pub pixel_scale_arcsec: f64,    // from CD matrix
    pub field_rotation_deg: f64,    // from CD matrix (N through E)
    pub solve_time_ms: u64,
    pub catalog_used: String,       // "tycho2"
    pub algorithm_used: String,     // "blind_index"
    pub derived_focallen_mm: Option<f64>, // if the frame had no FL, computed from pixel_scale + xpixsz
    pub expected_catalog_stars_in_fov: usize, // NEW — from the verify cone search
    pub inlier_ratio: f64,          // NEW — matched_stars / expected_catalog_stars_in_fov
}
```

The `expected_catalog_stars_in_fov` and `inlier_ratio` fields were added in Phase 4.3. They're also persisted in the `plate_solves` table so a later review pass can sort solves by confidence independent of absolute inlier count.

### Performance

End-to-end solve against a 6248 × 4176 FITS file in **release mode** on M1 MacBook. Measurements taken against the Lemmon reference frame (RA 231.95°, Dec 18.67°, 1.87 "/px, 589 catalog stars in FOV):

| Stage | Phase 1–3 (mag 11) | Phase 4 + mag 13 |
| ---- | ---- | ---- |
| Star detection (fast path) | ~400 ms | ~400–500 ms |
| Hash lookup per pass | ~80 ms | ~150–200 ms |
| Scale filter (±5 %) | <1 ms | <1 ms |
| Per-candidate verify (with cone cache) | ~20 ms | ~30 ms |
| **Pass 1 (50 stars) with deep index** | solves | may fail → pass 2 |
| **End-to-end (sparse field)** | **~350 ms** | **~450 ms** |
| **End-to-end (dense Milky Way)** | fails | **~750–1100 ms** |

The mag-13 index roughly doubles hash-lookup candidates and therefore verify cost. This is a deliberate tradeoff: the old mag-11 default was fast on sparse fields but silently failed on medium-density long-exposure frames; the mag-13 default solves both.

Debug-mode timings are ~10× slower due to unoptimised `nalgebra` LSQ routines.

### Accuracy

#### Sparse field — Lemmon reference (cross-check against a known solve from an external plate solver)

| Metric | Phase 1–3 (mag 11) | **Phase 4 + mag 13** |
| ---- | ---- | ---- |
| RA/Dec offset from truth | 0.023° (83") | **0.010° (36")** |
| Pixel scale error | 0.5 % | 0.5 % |
| Inliers | 37 of 140 | **115–120 of 589** |

#### Dense field — Barnard 150 in Cepheus (4 × 180 s OSC frames)

Before Phase 4 / mag-13: 0 of 4 solved. After:

| Frame | Inliers | RA | Dec | RMS |
| ---- | ---- | ---- | ---- | ---- |
| 94648 | 147 | 312.850° | +60.157° | 2.29 px |
| 94652 | 123 | 312.853° | +60.154° | 2.32 px |
| 94656 | 120 | 312.853° | +60.155° | 2.18 px |
| 94660 | 107 | 312.859° | +60.153° | 2.09 px |

All 4 pin to Barnard 150 (RA 312.85°, Dec +60.15°) at 1.887 "/px, rotation 271.5°.

This precision (sub-arcminute, half-percent scale) is appropriate for coordinate recovery and frame-set clustering. A precise WCS refinement phase using Gaia DR3 — with SIP distortion correction for sub-pixel stacking accuracy — is planned as a follow-up.
## Database Storage

Accepted solves are persisted to two places in the SQLite database.

### `plate_solves` table (full WCS)

Defined in `crates/athenaeum-core/src/db/schema.rs`. One row per frame — re-solving replaces it (`UNIQUE(frame_id)`, `insert_plate_solve`). The row is built from the `SolveResult` by `service::record_from_result` and read back as `storage::PlateSolveRecord` (`get_plate_solve`; `PlateSolveRecord::to_solution` rebuilds the WCS and its SIP tables).

| Column | Type | Notes |
| ---- | ---- | ---- |
| `id` | `INTEGER PRIMARY KEY` | auto |
| `frame_id` | `INTEGER NOT NULL UNIQUE` | FK to `frames(id)` with `ON DELETE CASCADE` |
| `crpix1`, `crpix2` | `REAL NOT NULL` | Reference pixel |
| `crval1`, `crval2` | `REAL NOT NULL` | RA, Dec at the reference pixel, degrees |
| `cd1_1`, `cd1_2`, `cd2_1`, `cd2_2` | `REAL NOT NULL` | CD matrix in degrees/pixel |
| `sip_order` | `INTEGER` | Order of the fitted SIP polynomial; NULL when no SIP was fitted |
| `sip_a_coeffs`, `sip_b_coeffs` | `TEXT` | SIP forward polynomials, JSON triangular arrays |
| `sip_ap_coeffs`, `sip_bp_coeffs` | `TEXT` | SIP reverse polynomials, same encoding |
| `matched_stars` | `INTEGER NOT NULL` | Inlier count |
| `total_detected` | `INTEGER NOT NULL` | Stars the solver detected |
| `rms_residual_px`, `rms_residual_arcsec` | `REAL NOT NULL` | RMS of the inlier residuals |
| `pixel_scale_arcsec` | `REAL NOT NULL` | Solved pixel scale |
| `field_rotation_deg` | `REAL NOT NULL` | Field rotation, N through E |
| `solve_time_ms` | `INTEGER NOT NULL` | Wall time of `solve_frame_with_hints` |
| `catalog_used`, `algorithm_used` | `TEXT NOT NULL` | As reported by `solvemyastro` |
| `solved_at` | `TEXT NOT NULL` | UTC timestamp, `%Y-%m-%d %H:%M:%S` |
| `expected_catalog_stars_in_fov` | `INTEGER` | Catalog stars (G <= 12) in the solved field. Nullable: rows written before the column was added by the idempotent `ALTER TABLE` migration in `schema.rs` keep NULL |
| `inlier_ratio` | `REAL` | The solver's inlier ratio; nullable for the same reason |

### `frames` table (summary coordinates)

`storage::update_frame_from_solve` updates the frame row so that clustering, spatial queries, the calendar and the sky atlas can work from `frames` alone. It sets `override = 1`, so the scanner's non-destructive re-parse does not revert it.

| Frame column | Updated | Format |
| ---- | ---- | ---- |
| `ra`, `dec` | yes | Decimal degrees (f64) |
| `rotation` | yes | Decimal degrees, N through E |
| `objctra` | yes | Sexagesimal `HH:MM:SS.s` via `coordinates::format_ra_sexagesimal()` |
| `objctdec` | yes | Sexagesimal `+DD:MM:SS.s` via `coordinates::format_dec_sexagesimal()` |
| `focallen` | yes, when a derived value exists | Written when the frame had NULL; overwritten only when `focallen_corrected` is set (the header value was proven wrong) |
| `object` | only when empty | The nearest DSO designation, via `update_frame_object_if_missing` (also sets `override = 1`) |

`delete_plate_solve_for_frame` removes the `plate_solves` row (used by the metadata pane's "Revert WCS to FITS header" action so a stale solve does not outlive the cleared frame columns).

## Configuration

`PlateSolveConfig` lives in `crates/athenaeum-core/src/plate_solve/config.rs` and is persisted as JSON in the `settings` table under the key `plate_solve.config`. Every field is `#[serde(default)]` and unknown keys are ignored, so configs written by older versions (which carried `max_image_stars`, `min_matched_stars`, `verification_tolerance_px`, `index_mag_limit`, `hash_tolerance`, `use_fast_detection`, `retry_passes`, `min_inlier_ratio` and others) still load. `load_config` also normalises a stored `blind_inlier_floor` of 12, the pre-2026-05 default, to the current default.

| Field | Default | Meaning |
| ---- | ---- | ---- |
| `sip_order` | 3 | SIP polynomial order passed to the solver |
| `base_verification_tolerance_arcsec` | 8.0 | Per-frame pixel tolerance is `clamp(base / pixel_scale_arcsec, 4, 20)` px; scales the RMS ceiling of the acceptance gate |
| `autofind_tolerance_deg` | 0.5 | Maximum distance for the "Autofind object from coordinates" DSO label; unrelated to solving |
| `batch_concurrency` | 0 | Batch workers; 0 = auto, `(cores / 3).clamp(2, 8)` |
| `blind_gate_enabled` | `true` | Master switch of the acceptance gate (`store_result` and the stacking solve) |
| `blind_rms_max_px_mult` | 2.5 | Refuse when `rms_residual_px` exceeds this multiple of the adaptive pixel tolerance |
| `blind_min_inlier_ratio` | 0.04 | Minimum `inlier_ratio`, applied to dense fields only (more than 100 catalog stars in the field) |
| `blind_inlier_floor` | 6 | Absolute inlier floor, equal to the solver's own minimum; a backstop, not a discriminator |
| `blind_scale_sanity_min`, `blind_scale_sanity_max` | 0.05, 60.0 | Recovered pixel scale must lie in this arcsec/px range |
| `blind_scale_header_tol` | 8.0 | When the header implies a scale, the recovered scale must be within this factor of it |
| `input_gate_enabled` | `true` | Refuse trailed frames before solving |
| `input_max_eccentricity` | 0.85 | Input gate: median star eccentricity threshold (both this and the next must be crossed) |
| `input_min_trail_r2` | 0.65 | Input gate: trail-fit R² threshold |
| `camera_defaults` | empty | `INSTRUME` or `TELESCOP` to pixel size in micrometres; used to derive focal length when the header has no `XPIXSZ` |
| `bright_cache_path` | none | Unread since 2026-09-10; kept so a stored config still round-trips. The solver resolves its catalog layers itself |

The Settings -> Plate Solving tab (`src/components/plate-solve/PlateSolveSettingsPanel.tsx`, hosted by `src/components/settings/tabs/PlateSolvingTab.tsx`) renders three registered sections from `src/settings/registry.ts`:

- `plateSolving.catalog` ("Star Catalog"): the tier list, a recommendation from the library's fields of view, and the download.
- `plateSolving.solver` ("Solver Parameters"): `base_verification_tolerance_arcsec`, `sip_order`, `autofind_tolerance_deg`, `batch_concurrency`.
- `plateSolving.inputGate` ("Input Gate"): `input_gate_enabled`, `input_max_eccentricity`, `input_min_trail_r2`.

The `blind_*` fields and `camera_defaults` have no controls; they live only in the stored JSON. `solver_config_hash` covers the recipe version, `sip_order`, the tolerance and every `blind_*` field (the inputs of a cached stacking solve) and ignores the rest.

## Key Source Files

### Rust

| Path | Role |
| ---- | ---- |
| `solvemyastro/` (submodule) | The solver: `orchestrate.rs` (blind and hinted search), `cache.rs` (`StarCache`, `.smac` format), `select.rs`, `quad.rs`, `verify.rs`, `refine.rs`, `sip.rs`, `wcs.rs`, `diag.rs` (`SolveFailure`, `FailureClass`) |
| `crates/athenaeum-core/src/plate_solve/service.rs` | `solve_frame`, `solve_frame_tiered`, `solve_frame_layered`, `solve_frame_with_hints`, `store_result`, `acceptance_refusal`, `blind_gate_ok`, `input_gate_reason`, `adaptive_tol_px`, `record_from_result` |
| `crates/athenaeum-core/src/plate_solve/layers.rs` | `discover_layers`, `open_catalog_layers`, `catalog_root`, `catalog_identity`, `CatalogOpenError` |
| `crates/athenaeum-core/src/plate_solve/hints.rs` | `extract_hints`, `apply_object_name_fallback`, `fov_from_optics`, `frame_fov_summary` |
| `crates/athenaeum-core/src/plate_solve/config.rs` | `PlateSolveConfig`, `load_config`/`save_config`, `SOLVE_RECIPE_VERSION`, `solver_config_hash` |
| `crates/athenaeum-core/src/plate_solve/failure.rs` | `describe_solve_failure`: solver failure class to a stable code and a user message |
| `crates/athenaeum-core/src/plate_solve/storage.rs` | `PlateSolveRecord`, `insert_plate_solve`, `get_plate_solve`, `delete_plate_solve`, `update_frame_from_solve`, `update_frame_object_if_missing` |
| `crates/athenaeum-core/src/plate_solve/dso_lookup.rs` | Bundled DSO catalog: name resolution and nearest-object lookup |
| `crates/athenaeum-core/src/plate_solve/object_fill.rs` | The "Autofind object from coordinates" batch |
| `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs` | Catalog tier download and `tier_status`; `catalog/manifest.rs` the published manifest |
| `crates/athenaeum-core/src/stacking/solve.rs` | The stacking project path's solve, sharing the gate and the opener |

### Tauri/Web

| Path | Role |
| ---- | ---- |
| `crates/athenaeum-tauri/src/commands/plate_solve.rs` | The 13 commands below, the batch driver and its events |
| `crates/athenaeum-web/src/routes/plate_solve.rs` | Mirrors every command over HTTP, with SSE for progress |

### Frontend

| Path | Role |
| ---- | ---- |
| `src/types/plate-solve.ts` | Typed mirrors of the Rust config / result / progress structs |
| `src/hooks/usePlateSolveQueue.ts` | Batch-solve queue |
| `src/components/plate-solve/PlateSolveSettingsPanel.tsx` | Settings: catalog tiers and download, solver parameters, input gate |
| `src/components/plate-solve/cameraPresets.ts` | Tier recommendation from the narrowest field of view |
| `src/components/plate-solve/PlateSolveBatchPanel.tsx` | Batch "Plate Solve Selected / All" buttons |
| `src/components/plate-solve/PlateSolveIndexMissingModal.tsx` | Offers the catalog download when a batch is refused because no tier is installed |
| `src/components/plate-solve/FillObjectsPanel.tsx` | Autofind-object batch panel |
| `src/settings/registry.ts` | The Plate Solving sections (`plateSolving.catalog`, `.solver`, `.inputGate`) |

## Tauri Commands

All 13 are in `crates/athenaeum-tauri/src/commands/plate_solve.rs` and mirrored in `crates/athenaeum-web/src/routes/plate_solve.rs`.

| Command | Purpose |
| ---- | ---- |
| `get_plate_solve_config()` / `set_plate_solve_config(config)` / `reset_plate_solve_config()` | Settings CRUD for `PlateSolveConfig` |
| `plate_solve_batch(frame_ids)` | Gate, solve and persist a list of frames; emits `plate-solve-progress` per frame and `plate-solve-complete` at the end |
| `cancel_plate_solve()` | Set the cancel flag of the running batch |
| `autofind_objects_from_coordinates(frame_ids)` | Label frames that have coordinates but no object from the DSO catalog; emits `autofind-objects-progress` and `autofind-objects-complete` |
| `cancel_autofind_objects()` | Cancel that batch |
| `resolve_object_name(name)` | Does the DSO catalog know this name (the metadata editor asks while typing) |
| `get_plate_solve_result(frame_id)` | Fetch a stored `plate_solves` row |
| `delete_plate_solve_for_frame(frame_id)` | Delete the stored row |
| `get_catalog_status()` | One entry per catalog tier, merged with the on-disk install state |
| `get_frame_fov_summary()` | Narrowest computable field of view among the light frames (the tier recommendation input) |
| `download_catalog_layers(target_density)` | Download the chosen tier and every lower one not yet installed; emits `catalog-download-progress` |

## Debug & Diagnostic Tools

_History: the retired Tycho-2 quad-index solver (replaced by `solvemyastro` + Gaia layers); kept for the record._ `debug_plate_solve.rs`, `debug_plate_solve_viz.rs` and `rebuild_quad_index.rs` are gone from `crates/athenaeum-core/examples/`; to diagnose a failure today read the structured failure code and the `solve` logs (`docs/logging/README.md`).

Two standalone example binaries live under `crates/athenaeum-core/examples/` for diagnosing solve failures against real user frames. They all read the installed Tycho-2 catalog + quad index via the shared `ATHENAEUM_DB_PATH` / `ATHENAEUM_CATALOG` env vars (defaults to the macOS app-data dir).

### `debug_plate_solve` — variant sweep

Runs the solver against a single frame under 6 different config variants (default, precise detection, loose verify, lower threshold, more image stars, kitchen-sink) and prints a summary table. Useful when a frame fails and you want to know whether any reasonable tweak unlocks it.

```bash
cargo run --release -p athenaeum-core --example debug_plate_solve -- <frame_id>
```

### `debug_plate_solve_viz` — visualised detection + solve

Writes PNG visualisations of each stage to an output dir (default `/tmp`):

| PNG | Shows |
| ---- | ---- |
| `<id>_fast_stars.png` | Fast-detected stars (green circles) on the auto-stretched colour display image |
| `<id>_precise_stars.png` | Same for the precise (Moffat-fit) detector |
| `<id>_detection_view.png` | The **actual green-interpolated luminance image the detector thresholds against** (4× downscaled for display), with green circles on detected stars |
| `<id>_detection_native_crop.png` | A **1:1 native-resolution** 1200×1200 centre crop of the detection view — for verifying circles sit on stars |
| `<id>_crop_y_asis.png` / `_y_flipped.png` | Brightest-star crop sanity check for y-axis flip testing |
| `<id>_solve_overlay.png` | Catalog stars projected via the solved WCS (if solve succeeded), inlier matches connected by yellow lines |
| `<id>_solve_failures.png` | Catalog stars that did NOT land on an image star within tolerance |
| `<id>_solve_overlay_zoom.png` | Zoomed native-res view of the overlay |

```bash
cargo run --release -p athenaeum-core --example debug_plate_solve_viz -- <frame_id> [output_dir]
```

The detection view requires the `debug-pipeline` feature on rustafits (enabled in `athenaeum-core/Cargo.toml`), which exposes `astroimage::formats::read_image` + `astroimage::analysis::prepare_luminance` so this bench can produce the luminance image the detector actually operates on. Coord convention: the detection-view PNGs write FITS row 0 → PNG row 0 (no flip) so detector y-coordinates map directly; the colour PNGs use `ImageConverter::process` which presents the sky north-up (flipped), so those draw circles via `to_display_y(y) = height - 1 - y`.

### `rebuild_quad_index` — CLI quad-index rebuilder

Rebuilds the all-sky quad index at a user-supplied magnitude limit. Backs up the active `quad_index.bin` to `quad_index.bak` before overwriting, so rollback is trivial.

```bash
# Rebuild in place at mag 13 (default depth):
cargo run --release -p athenaeum-core --example rebuild_quad_index -- 13.0 quad_index.bin

# Build a side-by-side shallow index for comparison, no swap:
cargo run --release -p athenaeum-core --example rebuild_quad_index -- 11.0 quad_index_mag110.bin
```

Progress is printed every 5 % of pixels read. Expect ~40 s on an M1 at mag 13, ~12 s at mag 11.
## Known Failure Modes

### Long-exposure dense-field frames (quad-index solver)

_History: the retired Tycho-2 quad-index solver (replaced by `solvemyastro` + Gaia layers); kept for the record._

**Symptom:** frame fails with `best candidate has <N> inliers at RA=<correct> Dec=<correct> (required <M>, density <D> detected / <E> catalog in FOV) — dense field; consider rebuilding the quad index with a higher magnitude limit`.

**Cause:** brightest stars in the image saturate, so the detector's flux-sorted top-N falls into the mag 9–13 band. A shallow (mag 11) index has quads built only from mag 5–11 stars, so the image's nearest-neighbour quads don't match. Confirmed root cause during Phase 4 — all 4 Barnard 150 frames were failing at mag 11 and solved at mag 13.

**Fix:** rebuild the quad index at `index_mag_limit = 13.0` (the default since 2026-04).

### Frame-set clustering is NOT re-run after solve

**Symptom:** after solving a batch of previously-coordinate-less frames, the solved frames stay in whatever `frames_set` they were clustered into pre-solve — typically the "Unknown @ RA=00:00:00.0, Dec=+00:00:00.0" fallback bucket — instead of being regrouped by their now-correct sky coordinates.

**Cause:** `service::store_result` writes `ra`, `dec`, `rotation`, `objctra`, `objctdec` back to the `frames` table but does not touch `session_members` / `imaging_nights` / `frames_set`. The DBSCAN clustering in `auto_generate_frame_sets` currently excludes frames already in a set, so manually re-running it does not detach and re-cluster them either.

**Workaround:** delete the stale `frames_set` and run auto-generate again (manual, risky on mixed libraries). A cleaner fix is planned — either automatic re-cluster on solve, or an explicit "re-cluster solved frames" batch action.

## What Is NOT Yet Implemented

- **Auto re-cluster solved frames into the correct frame sets** — see Known Failure Modes above.
- **Gate controls in Settings** — the acceptance gate's `blind_*` fields and `camera_defaults` have no Settings UI (the input gate and the solver parameters do); see Configuration.

SIP distortion fitting and the Gaia catalog, listed here as planned while the quad-index solver was current, are implemented: the solver fits SIP and the `plate_solves` SIP columns are filled.

## Changelog

### 2026-10 — Document brought back in line with the code

- Rewrote Architecture Overview, Per-Frame Pipeline, Database Storage, Configuration, Key Source Files and Tauri Commands from the code; the retired quad-index sections are kept and marked as history.

### 2026-10 — Shared acceptance gate and catalog opener (stacking project path)

- The acceptance gate is the pure `plate_solve::service::acceptance_refusal`, shared by `store_result` and the stacking solve (`stacking::solve`), so a project frame is judged exactly as a catalog frame.
- `plate_solve::config::SOLVE_RECIPE_VERSION` is bumped whenever the recipe constants, the gate or the catalog layering change in a way that could change an outcome; it re-solves cached stacking outcomes (review I7).
- One catalog opener in core, `plate_solve::open_catalog_layers` / `catalog_identity` (`plate_solve/layers.rs`), is used by both hosts and the stacking solver. See `docs/stacking/README.md` (The project path, wave 6b-2) for the solve order and cache.

### 2026-06 — Catalog audit + bright-vs-deep FOV guidance (docs only)

No code change — documented the *verified on-disk reality* of the shipped caches
and added focal-length/FOV guidance. See [Verified on-disk contents](#verified-on-disk-contents-2026-06-01-audit)
and [When is the bright tier enough vs. when do you need deep?](#when-is-the-bright-tier-enough-vs-when-do-you-need-deep).

- **Verified contents:** deep `smac_gaia` = 540,600,543 stars, hard **G≤19.0**, ~15.1 GB; bright `smac_gaia_bright` = 70,541,101 stars, hard **G≤16.0**, ~2.0 GB; both depth-6, 0 empty cells all-sky. Raw `gaia_dr3/` (7.1 GB) is build-input only.
- **Correction:** the bright cache's `target_density: 100` top-up **never engaged** in the shipped build (sparsest cell already has 144 > 100 stars to G16), so it is effectively a clean G≤16 magnitude cut, not a per-cell density cap. Earlier doc size "~2.6 GB" corrected to ~2.0 GB (70.5 M × 28 B).
- **Boundary:** the bright tier is the field-of-view determinant — plenty ≳20′, marginal 10–20′, need deep <10′. By sensor: deep becomes load-bearing beyond ~3,900 mm (small chip), ~5,400 mm (APS-C), ~12,000 mm (full-frame). Hemisphere is irrelevant (all-sky, 0 empty cells).

### 2026-05 — Bright sub-catalog (tiered Gaia caches)

Catalog separation for the solvemyastro/Gaia pipeline. Full detail in the [Tiered Star Cache](#tiered-star-cache-bright--deep-2026-05) section above.

- **New optional second cache** at `<app-data>/catalogs/smac_gaia_bright/`, ~70.5 M stars (~2.0 GB) — derived from the deep G<19 cache via a per-HEALPix-cell hybrid algorithm (floor `mag < 16` + density top-up to 100; in the shipped build the floor dominated everywhere so it is effectively a clean G≤16 cut — see the 2026-06 entry).
- **Athenaeum picks it up automatically** via the convention path, or via the new `PlateSolveConfig::bright_cache_path` setting. Missing/corrupt is non-fatal; the solver falls back to deep-only and logs a warning.
- **Quad-matching cone tries bright first**, auto-falls-back to deep when the cone returns fewer than `SolveConfig::bright_fallback_threshold = 30` stars. **Verify always uses deep** (Bayesian NR correctness).
- **Why floor=16 (not the CLI default 14):** typical 120–300 s exposures detect to m ≈ 14–16, so a floor=14 cache would force the auto-fallback on most cells and erase the speedup.
- **Measured:** corpus_bench wall 52.4 s → **19.66 s (−62 %)**, 14/14 truth, 19/19 solves, 0 wrong.
- **New API:** `solvemyastro::Caches<'a> { deep, bright: Option<&StarCache> }`; `solve()` signature now takes `&Caches<'_>` instead of `&StarCache`. Athenaeum-side: `service::solve_frame_tiered`, `solve_frame_with_hints(bright_cache: Option<&StarCache>, …)`, `ServiceContext::bright_cache`.
- **Build once:** `solvemyastro build-bright-cache --from <smac_gaia> --out <smac_gaia_bright> --bright-floor 16 --target-density 100 --mode hybrid` (~5 min, writes a sidecar `bright.meta.json`).

### 2026-04 — Phase 4 + mag-13 default

Fixes driven by failure analysis on 4 × 180 s OSC Milky Way frames (Barnard 150 in Cepheus). Before the fixes: 0 / 4 solved. After: 4 / 4 solved at the correct sky position with 107–147 inliers and ~2.2 px RMS.

- **4.1** — Refit returns a tight-tolerance inlier count. Previously the loose-tolerance baseline set inside `translation_refit` could leak out as the reported final count, producing "refit 14 inliers at 6.0 px RMS" lines where 6.0 > 4.3 px tight tolerance (geometrically impossible). `try_solve_pass` now re-runs `count_inliers` at tight tolerance on the refit-returned WCS before recording.
- **4.2** — `required_inliers` takes `detected_count` and caps the effective density by `min(expected_in_fov, detected_count)`. Prevents the dense-galactic-plane false-negative where 3500 catalog stars would demand 349 inliers but only 600 stars are detectable. New unit test `required_inliers_capped_by_detected_count`.
- **4.3** — Persisted `expected_catalog_stars_in_fov` + `inlier_ratio` in `SolveResult` and `plate_solves` (migration via `ALTER TABLE ADD COLUMN`, nullable on pre-migration rows). Retry passes default changed from `[150, 300, 600]` → `[50, 150, 300, 600]`.
- **4.4** — Richer failure message including seed RA/Dec, detected/catalog density, and an "index rebuild" hint for high-density FOVs.
- **Index default mag 11 → 13** — the true Tycho-2 catalog ceiling. Adds ~280 % more quads (860 k → 2.42 M). Build time ~40 s on M1.
- **Scale filter tightened** — ±5 % (from ±10 %) when `FOCALLEN+XPIXSZ` hint is present, halving verify-loop work on the deeper index. A separate looser ±10 % is kept for downstream refit/WCS sanity.
- **Early-exit lowered** — `required × 2` → `required × 1.5`. Density-aware floor keeps false-positive rate low.
- **Cone-search cache** — the per-candidate cone search is cached per pass (Phase 3.1 earlier, re-tuned in Phase 4); subsequent candidates whose FOV fits inside the cached cone skip their own query.
- **Redundant Arc wrapping removed** — Tauri + Web batch handlers used to wrap `Arc<ThreadPool>` in a second Arc before passing into workers. Now each worker clones the original Arc directly.
- **`image_positions` hoisted** above the retry loop (was being rebuilt per pass).
- **New settings UI** — inline `at mag ≤ [N]` input next to the Rebuild button, with auto-save-before-build so user edits take effect immediately.
- **rustafits `pattern_matcher.rs` hygiene** — `fit_affine_from_centers` swapped LU-of-normal-equations for SVD pseudo-inverse (more stable on rank-deficient inputs). Not on the hot path (athenaeum uses the hash-bucket index), but called directly by rustafits standalone users.

Verified: 6 unit tests in `service::tests`, 5 integration tests in `blind_index_solve.rs`, release end-to-end on the Lemmon reference frame (118 inliers, 0.010° off truth, ~450 ms).

### 2025-11 — initial ship

- Blind solver with Tycho-2 mag-11 quad index, density-unaware `min_matched_stars=15` gate, fixed `verification_tolerance_px=10`, single-pass at `max_image_stars=300`.

## Related Documents

- [`star-detection.md`](./star-detection.md) — details of the rustafits `ImageAnalyzer` pipeline used by Step 1 of the solver.
- [`precise-registration.md`](./precise-registration.md) — the opt-in **precise WCS** pipeline (PSF centroids → annealed σ-clipped SIP → robust seed → refinement-helped guard); its image-registration half is superseded 2026-09-10 by the stacking program (`stacking/register/*`, runs inside a stacking run — `docs/stacking/README.md`), kept for history.

## Detail moved from CLAUDE.md (2026-10-20) - Plate solving rules

### Plate solving

`solvemyastro` (submodule) is the sole solver, driven from `crates/athenaeum-core/src/plate_solve/`. Three defences (2026-09-05) against wind-shaken frames being "solved" at a wrong scale: rustafits measures eccentricity on every fast detection, the solver drops streaks before quad selection and refuses a frame the cut emptied (`looks_trailed`), and `plate_solve/service.rs` has an analysis-based input gate (`median_eccentricity` AND `trail_r_squared`, both required) plus an acceptance gate fed the header pixel scale. **Only the input gate has a Settings section** (`plateSolving.inputGate`); the acceptance gate's `blind_*` fields live only in the stored `plate_solve.config` JSON. Known gap, its own cycle: the full analysis under-reports eccentricity on trailed frames. **Object-name fallback** (`plate_solve::hints::apply_object_name_fallback`): a header without usable RA/Dec resolves OBJECT against the bundled DSO catalog; a recorded position always wins; `resolve_object_name` on both hosts. Reference: `docs/platesolving/README.md` (pipeline) and `docs/platesolving/input-and-acceptance-gates.md` (the gates).
