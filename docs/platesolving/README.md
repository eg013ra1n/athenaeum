# Plate Solving

Athenaeum's plate solver is a **truly blind** all-sky astrometric solver. It determines precise sky coordinates for any FITS image without requiring the user to provide any coordinate hints — the FITS header does not need to contain RA/DEC, OBJECT, or any other positional information.

The only inputs the solver strictly requires are the image pixels themselves. Focal length and pixel size (from `FOCALLEN` and `XPIXSZ` FITS keywords) are used as a consistency filter to reject false matches but are not required for the match itself to succeed.

This document describes the current implementation — architecture, pipeline, inputs, and outputs.

## Architecture Overview

```text
┌─────────────────────────────────────────────────────────┐
│                  One-time setup                         │
│                                                         │
│  Tycho-2 catalog (49,152 HEALpix files, 192 MB)         │
│            │                                            │
│            ▼                                            │
│  index_builder::IndexBuilder::build()                   │
│            │                                            │
│            ▼                                            │
│  quad_index.bin  (~46 MB, ~860 K quads, 12 sec build)   │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│                  Per-frame solve                        │
│                                                         │
│  FITS file ─▶ ImageAnalyzer (star detection)            │
│                    │                                    │
│                    ▼                                    │
│               (x, y, flux) for top 500 stars            │
│                    │                                    │
│                    ▼                                    │
│  pattern_matcher::build_quads() — nearest-neighbor      │
│                    │                                    │
│                    ▼                                    │
│  Hash lookup in QuadIndex (O(1) per image quad)         │
│                    │                                    │
│                    ▼                                    │
│  Scale filter (if FL + PIXSZ known)                     │
│                    │                                    │
│                    ▼                                    │
│  Per-candidate: try all 24 star permutations,           │
│  fit similarity transform, keep best                    │
│                    │                                    │
│                    ▼                                    │
│  Verify: cone-search catalog, project through WCS,      │
│  count image stars within tolerance                     │
│                    │                                    │
│                    ▼                                    │
│  WcsSolution + SolveResult ─▶ plate_solves + frames     │
└─────────────────────────────────────────────────────────┘
```

## Two-Layer Code Organisation

Plate solving is split across two crates, matching the workspace's general split between pure algorithms and application logic:

| Layer | Crate | Module | Role |
| ---- | ---- | ---- | ---- |
| Pure algorithms | `rustafits` | `src/platesolving/` | Quad generation, distance-ratio hashing, affine fitting, WCS math. No I/O, no database, no catalogs. |
| Orchestration | `athenaeum-core` | `src/plate_solve/` and `src/catalog/` | Catalog I/O, quad index build/load, solve pipeline, database storage. |
| Frontend | `src/components/plate-solve/` | — | React components, hooks, Tauri command invocation. |

## Pre-Built All-Sky Quad Index

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
| Orion                 | 558 |   912 |   986 |   996 |   996 |
| Galactic centre       | 785 | 1,684 | 1,857 | 1,872 | 1,873 |

Rule of thumb: if bright stars in your image saturate (common for 180 s+ OSC exposures of dense fields), you need a deeper index because the detector's top-N is shifted down into the mag 9–13 range that the old mag-11 index did not cover.

## Tiered Star Cache (Bright + Deep, 2026-05)

> Applies to the `solvemyastro` Gaia-DR3 pipeline that has replaced the legacy Tycho-2 `CatalogEngine` + `QuadIndex` flow in `solve_frame_with_hints`. The older sections above describe the original blind-index design and remain accurate for the Tycho-2 path; the parts that refer to a single `CatalogEngine` are superseded for the Gaia pipeline by the two-cache model below.

The hot path now consults **two on-disk star caches**, not one:

| Cache | Convention path | Depth | Size | Used by |
| ---- | ---- | ---- | ---- | ---- |
| **Deep** | `<app-data>/catalogs/smac_gaia/` | Gaia DR3 G<19 | ~540 M stars | Verify stage (always) + quad-matching cone when no bright cache is configured **or** when the bright cone is too sparse |
| **Bright** (optional) | `<app-data>/catalogs/smac_gaia_bright/` | Hybrid (see below) | ~70.5 M stars (~2.6 GB) | Quad-matching cone, tried *first* |

Both are produced by `solvemyastro`'s SMAC writer. They are independent files — the bright cache is *derived* from the deep cache via the `build-bright-cache` subcommand, not from raw Gaia ingest.

### Hybrid build algorithm

Per HEALPix-6 cell (49,152 cells across the sky), the bright cache is filled with:

1. **Floor** — every star with `mag < bright_floor` (athenaeum ships with floor = **16**).
2. **Top-up** — if the floor count is below `target_density` (default 100), append the next-brightest stars (continuing past the floor up to the deep cache's G<19 ceiling) until reaching the target *or* exhausting the cell.
3. **No hard ceiling** — Milky-Way-plane cells keep all 500+ bright stars; sparse polar cells get e.g. 30 floor + 70 top-up = 100.

Net effect: bright-rich regions get a true G<16 sub-cache, sparse regions get an opportunistic density-100 sub-cache. The bright cache never goes deeper than its source (G<19), so the deepest "top-up" star is around G≈19, not G≈21.

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

## Per-Frame Pipeline

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
expected_scale = atan(xpixsz_mm × xbinning / focallen_mm) × (180/π) × 3600
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

#### Sparse field — Lemmon reference (cross-check against known astrometry.net solve)

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

Successful solves are persisted to two places in the SQLite database.

### `plate_solves` table (full WCS)

Defined in `crates/athenaeum-core/src/db/schema.rs`. One row per frame — re-solving updates in place (`UNIQUE(frame_id)`).

| Column | Type | Populated by the blind solver? | Notes |
| ---- | ---- | ---- | ---- |
| `id` | `INTEGER PRIMARY KEY` | auto | — |
| `frame_id` | `INTEGER NOT NULL UNIQUE` | yes | FK to `frames(id)` with `ON DELETE CASCADE` |
| `crpix1`, `crpix2` | `REAL NOT NULL` | yes | Reference pixel (always image centre) |
| `crval1`, `crval2` | `REAL NOT NULL` | yes | RA, Dec at reference pixel, degrees |
| `cd1_1`, `cd1_2`, `cd2_1`, `cd2_2` | `REAL NOT NULL` | yes | CD matrix in degrees/pixel |
| `sip_order` | `INTEGER` | **no** (NULL) | Reserved for future precise-WCS refinement |
| `sip_a_coeffs`, `sip_b_coeffs` | `TEXT` | **no** (NULL) | Reserved for SIP forward polynomials (JSON) |
| `sip_ap_coeffs`, `sip_bp_coeffs` | `TEXT` | **no** (NULL) | Reserved for SIP reverse polynomials |
| `matched_stars` | `INTEGER NOT NULL` | yes | Verification inlier count |
| `total_detected` | `INTEGER NOT NULL` | yes | Total stars from star detection |
| `rms_residual_px` | `REAL NOT NULL` | yes | RMS of inlier residuals in pixels |
| `rms_residual_arcsec` | `REAL NOT NULL` | yes | `rms_residual_px × pixel_scale_arcsec` |
| `pixel_scale_arcsec` | `REAL NOT NULL` | yes | Derived from the CD matrix |
| `field_rotation_deg` | `REAL NOT NULL` | yes | Position angle of the Y axis, N through E |
| `solve_time_ms` | `INTEGER NOT NULL` | yes | End-to-end wall time |
| `catalog_used` | `TEXT NOT NULL` | yes | `"tycho2"` today; `"gaia_dr3"` once that's added |
| `algorithm_used` | `TEXT NOT NULL` | yes | `"blind_index"` today |
| `solved_at` | `TEXT NOT NULL` | yes | ISO 8601 UTC timestamp |
| `expected_catalog_stars_in_fov` | `INTEGER` | yes (Phase 4.3) | Count from the verify cone search. Nullable on rows written before the migration. |
| `inlier_ratio` | `REAL` | yes (Phase 4.3) | `matched_stars / expected_catalog_stars_in_fov`. Nullable on pre-Phase-4 rows. |

The SIP columns are intentionally unused today. They stay in the schema so the precise-WCS refinement stage can fill them without a migration.

The Phase 4.3 columns were added via an idempotent `ALTER TABLE ADD COLUMN` migration (`schema.rs`) — existing rows retain `NULL` values; the schema migration is non-destructive.

### `frames` table (summary coordinates)

The solver also updates the frame record directly via `update_frame_from_solve()` so that downstream features (clustering, spatial queries, calendar, sky atlas) can work from `frames` alone without joining `plate_solves`.

| Frame column | Updated | Format |
| ---- | ---- | ---- |
| `ra` | yes | Decimal degrees (f64) |
| `dec` | yes | Decimal degrees (f64) |
| `rotation` | yes | Decimal degrees, N through E |
| `objctra` | yes | Sexagesimal `HH:MM:SS.s` via `coordinates::format_ra_sexagesimal()` |
| `objctdec` | yes | Sexagesimal `±DD:MM:SS.s` via `coordinates::format_dec_sexagesimal()` |
| `focallen` | yes, **only if it was NULL** | mm, derived as `206265 × (xpixsz_mm) / pixel_scale_arcsec` |

Coordinate format rules follow the project-wide convention so new solved values are indistinguishable from values that originally came from FITS headers.

## Configuration

`PlateSolveConfig` lives in `crates/athenaeum-core/src/plate_solve/config.rs` and is persisted in the `settings` table under the key `plate_solve.config`. All fields have `#[serde(default)]`, so old configs migrate cleanly.

| Field | Default | Meaning |
| ---- | ---- | ---- |
| `max_image_stars` | 300 | Legacy; `retry_passes` now drives per-pass cap |
| `min_matched_stars` | 6 | **Absolute floor** for the density gate — never accept below this even if density is tiny (was 15 before Phase 4) |
| `verification_tolerance_px` | 10.0 | Legacy; superseded by `base_verification_tolerance_arcsec` |
| `index_mag_limit` | **13.0** | Faintest magnitude included when building the index (was 11.0; raised 2026-04) |
| `hash_tolerance` | 0.005 | Quantisation bin size for distance-ratio keys |
| `sip_order` | 3 | Reserved for the future refinement stage |
| `use_fast_detection` | `true` | Skip Moffat PSF fit during detection |
| `autofind_tolerance_deg` | 0.5 | DSO-labelling tolerance (unrelated to solving) |
| `batch_concurrency` | 0 | 0 = auto (`cores/3`, clamped 2–8) |
| `min_inlier_ratio` | 0.10 | **Density-aware gate**: dense fields (>100 FOV stars) require `round(min_ratio × effective_density)` inliers, floored at 20 |
| `retry_passes` | `[50, 150, 300, 600]` | Progressive star-count passes; first passing pass wins |
| `base_verification_tolerance_arcsec` | 8.0 | Adaptive pixel tolerance is `clamp(base/pixel_scale, 4, 20) px` |

`index_mag_limit` and `hash_tolerance` only take effect when the user rebuilds the quad index. The Settings → Plate Solving tab has an inline `at mag ≤ [N]` field next to the **Rebuild Quad Index** button; clicking rebuild auto-saves the config first, so whatever the user types is persisted and used for the rebuild immediately.

## Key Source Files

### Rust

| Path | Role |
| ---- | ---- |
| `rustafits/src/platesolving/pattern_matcher.rs` | `build_quads()`, `Quad`, `AffineTransform`, `fit_affine_from_centers()` |
| `rustafits/src/platesolving/projection.rs` | Gnomonic TAN projection (`sky_to_tangent`, `tangent_to_sky`) |
| `rustafits/src/platesolving/proper_motion.rs` | Linear proper-motion propagation for catalog stars |
| `rustafits/src/platesolving/wcs.rs` | `WcsSolution` + `pixel_to_sky`, `sky_to_pixel`, `to_fits_headers` |
| `rustafits/src/platesolving/transform.rs` | Affine / projective / SIP fitting (used later by precise WCS) |
| `rustafits/src/platesolving/ransac.rs` | RANSAC over individual star matches (reserved for refinement) |
| `crates/athenaeum-core/src/catalog/binary_format.rs` | 14-byte HEALpix star record format |
| `crates/athenaeum-core/src/catalog/healpix.rs` | `cdshealpix` wrapper: pixel lookup, cone search, neighbor walk |
| `crates/athenaeum-core/src/catalog/mod.rs` | `CatalogEngine::cone_search()` and `load_region()` |
| `crates/athenaeum-core/src/catalog/tycho2.rs` | Downloads and parses the Tycho-2 catalog from CDS |
| `crates/athenaeum-core/src/plate_solve/index_builder.rs` | Builds the all-sky `quad_index.bin` |
| `crates/athenaeum-core/src/plate_solve/quad_index.rs` | Loads and queries the index |
| `crates/athenaeum-core/src/plate_solve/service.rs` | `solve_frame()`, `store_result()`, `try_solve_pass()`, `required_inliers()`, `adaptive_tol_px()` — the solve pipeline (+ Phase 4 helpers) |
| `crates/athenaeum-core/src/plate_solve/hints.rs` | Metadata extraction: pixel scale (binning-aware), FOV, obs epoch |
| `crates/athenaeum-core/src/plate_solve/storage.rs` | `plate_solves` (incl. `expected_catalog_stars_in_fov` / `inlier_ratio`) and `frames` DB writes |
| `crates/athenaeum-core/src/plate_solve/config.rs` | `PlateSolveConfig` serde type (incl. `retry_passes`, `min_inlier_ratio`, `base_verification_tolerance_arcsec`) |
| `crates/athenaeum-core/examples/debug_plate_solve.rs` | Variant-sweep bench against a single frame |
| `crates/athenaeum-core/examples/debug_plate_solve_viz.rs` | Visualised detection + solve, writes PNGs |
| `crates/athenaeum-core/examples/rebuild_quad_index.rs` | Rebuild `quad_index.bin` at an arbitrary magnitude limit |

### Tauri/Web

| Path | Commands |
| ---- | ---- |
| `crates/athenaeum-tauri/src/commands/plate_solve.rs` | `plate_solve_frame`, `plate_solve_batch`, `cancel_plate_solve`, `get_plate_solve_config`, `set_plate_solve_config`, `reset_plate_solve_config`, `get_plate_solve_result`, `get_catalog_status`, `download_tycho2_catalog`, `get_quad_index_status`, `build_quad_index` |
| `crates/athenaeum-web/src/routes/plate_solve.rs` | Mirrors all Tauri commands over HTTP with SSE for progress |

### Frontend

| Path | Role |
| ---- | ---- |
| `src/types/plate-solve.ts` | Typed mirrors of the Rust config / result / progress structs |
| `src/hooks/usePlateSolveProgress.ts` | Batch-solve queue, listens to `plate-solve-progress` events |
| `src/components/plate-solve/PlateSolveSettingsPanel.tsx` | Settings tab: catalog download, quad-index build, solver parameters |
| `src/components/plate-solve/PlateSolveBatchPanel.tsx` | Batch "Plate Solve Selected / All" buttons for the FileManager missing-metadata tab |
| `src/pages/Settings.tsx` | Hosts the plate solving settings tab |
| `src/pages/FileManager.tsx` | Hosts the batch panel on the missing-metadata tab |

## Tauri Commands

| Command | Purpose |
| ---- | ---- |
| `plate_solve_frame(frame_id)` | Solve a single frame and store the result |
| `plate_solve_batch(frame_ids)` | Solve a list of frames sequentially with progress events |
| `cancel_plate_solve()` | Cancel the current batch |
| `get_plate_solve_result(frame_id)` | Fetch a stored solve record |
| `get_plate_solve_config()` / `set_plate_solve_config(config)` / `reset_plate_solve_config()` | Settings CRUD |
| `get_catalog_status()` | List installed catalogs (Tycho-2 today) |
| `download_tycho2_catalog()` | Download + convert Tycho-2 (~160 MB over HTTP, emits `catalog-download-progress`) |
| `get_quad_index_status()` | Check if `quad_index.bin` exists and read its header |
| `build_quad_index()` | Build the all-sky quad index from an installed catalog (emits `quad-index-progress`) |

## Debug & Diagnostic Tools

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

### Long-exposure dense-field frames

**Symptom:** frame fails with `best candidate has <N> inliers at RA=<correct> Dec=<correct> (required <M>, density <D> detected / <E> catalog in FOV) — dense field; consider rebuilding the quad index with a higher magnitude limit`.

**Cause:** brightest stars in the image saturate, so the detector's flux-sorted top-N falls into the mag 9–13 band. A shallow (mag 11) index has quads built only from mag 5–11 stars, so the image's nearest-neighbour quads don't match. Confirmed root cause during Phase 4 — all 4 Barnard 150 frames were failing at mag 11 and solved at mag 13.

**Fix:** rebuild the quad index at `index_mag_limit = 13.0` (the default since 2026-04).

### Frame-set clustering is NOT re-run after solve

**Symptom:** after solving a batch of previously-coordinate-less frames, the solved frames stay in whatever `frames_set` they were clustered into pre-solve — typically the "Unknown @ RA=00:00:00.0, Dec=+00:00:00.0" fallback bucket — instead of being regrouped by their now-correct sky coordinates.

**Cause:** `service::store_result` writes `ra`, `dec`, `rotation`, `objctra`, `objctdec` back to the `frames` table but does not touch `session_members` / `imaging_nights` / `frames_set`. The DBSCAN clustering in `auto_generate_frame_sets` currently excludes frames already in a set, so manually re-running it does not detach and re-cluster them either.

**Workaround:** delete the stale `frames_set` and run auto-generate again (manual, risky on mixed libraries). A cleaner fix is planned — either automatic re-cluster on solve, or an explicit "re-cluster solved frames" batch action.

## What Is NOT Yet Implemented

These items are explicitly out of scope for the current blind solver and are planned as follow-up work:

- **SIP distortion correction** — `sip_*` columns in `plate_solves` are reserved and always NULL for blind-solve results.
- **Gaia DR3 catalog** — the code treats `catalog_used` as dynamic, but only `tycho2` is currently wired up.
- **Sub-arcsecond precise refinement** — for stacking / registration accuracy, a second pass using Gaia DR3 and full WCS+SIP fitting is planned. The current blind solver intentionally stops at similarity-level accuracy.
- **Auto-build on first use** — the user must explicitly click "Build Quad Index" the first time after downloading the catalog.
- **Auto re-cluster solved frames into correct frame sets** — see Known Failure Modes above.

## Changelog

### 2026-05 — Bright sub-catalog (tiered Gaia caches)

Catalog separation for the solvemyastro/Gaia pipeline. Full detail in the [Tiered Star Cache](#tiered-star-cache-bright--deep-2026-05) section above.

- **New optional second cache** at `<app-data>/catalogs/smac_gaia_bright/`, ~70.5 M stars (~2.6 GB) — derived from the deep G<19 cache via a per-HEALPix-cell hybrid algorithm (floor `mag < 16` + density top-up to 100).
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
