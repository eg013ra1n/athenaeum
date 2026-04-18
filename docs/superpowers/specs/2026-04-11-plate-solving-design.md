# Plate Solving Design — rustafits 1.0.0 + Athenaeum 2.0.0

## Context

Athenaeum currently relies on FITS header metadata (RA, Dec, rotation) written by mount control or external plate-solving software. Many frames lack this metadata entirely, making them invisible to coordinate-based features (frame set clustering, sky chart, spatial queries). Users need a way to recover coordinates for these frames and to get precise WCS solutions for future registration workflows.

This design covers phases 1-4 from the [requirements document](2026-04-09-plate-solving-requirements.md): core pattern matching algorithms in rustafits, a HEALpix-indexed catalog engine in athenaeum-core, hint-based plate solving with SIP distortion correction, and UI integration for both batch and per-frame solving. Blind solving, Gaia DR3 download, and mosaic support are deferred to future releases.

rustafits becomes 1.0.0 with this release. Athenaeum becomes 0.2.0.

---

## Architecture

### Layer Boundary

**rustafits** (pure computation, no I/O):
- Quad hash pattern matching (two implementations behind a trait)
- RANSAC outlier rejection
- Affine/projective/SIP transform fitting
- WCS solution (pixel-to-sky, sky-to-pixel, FITS header generation)
- Gnomonic TAN projection
- Proper motion epoch propagation

**athenaeum-core** (I/O + orchestration):
- HEALpix-indexed binary catalog management (Tycho-2 bundled)
- Cone search with memmap2
- PlateSolveService orchestrating catalog → rustafits algorithms → DB storage
- Hint extraction from Frame metadata
- Shared processing queue with analysis tasks

**athenaeum-tauri + athenaeum-web** (thin command/route wrappers):
- Tauri commands and web routes mirror each other
- Progress events via Tauri events / SSE

### Module Layout

```
rustafits/src/platesolving/
├── mod.rs              — public re-exports
├── types.rs            — CatalogStar, StarMatch, SolveHints, ProjectedStar, configs
├── pattern_matcher.rs  — QuadHasher trait + PatternMatcher orchestrator
├── quad_distance.rs    — Approach A: astrometry.net 5 distance ratios
├── quad_interior.rs    — Approach B: tetra3 2 barycentric coordinates
├── ransac.rs           — RANSAC outlier rejection
├── transform.rs        — TransformFitter (affine, projective, SIP)
├── wcs.rs              — WcsSolution struct + pixel↔sky + FITS headers
├── projection.rs       — GnomonicProjection (sky↔tangent plane)
└── proper_motion.rs    — ProperMotionCorrector (epoch propagation)

athenaeum-core/src/catalog/
├── mod.rs              — CatalogEngine public API
├── binary_format.rs    — HEALpix binary record (12 bytes/star) read/write
├── healpix.rs          — HEALpix pixel indexing, cone search → pixel IDs
└── tycho2.rs           — Tycho-2 raw → binary conversion utility

athenaeum-core/src/plate_solve/
├── mod.rs              — PlateSolveService public API
├── service.rs          — Orchestration: hints → catalog → rustafits → result
├── storage.rs          — plate_solves table CRUD operations
└── hints.rs            — Extract SolveHints from Frame metadata

athenaeum-tauri/src/commands/plate_solve.rs   — Tauri commands
athenaeum-web/src/routes/plate_solve.rs       — Web routes + SSE
```

---

## rustafits Platesolving API

### New dependency

`nalgebra` — for least-squares WCS fitting (CD matrix, SIP coefficients).

### QuadHasher Trait

Both quad hash approaches implement a common trait for benchmarking:

```rust
pub trait QuadHasher: Send + Sync {
    fn build_table(&self, stars: &[ProjectedStar], config: &PatternMatcherConfig) -> HashTable;
    fn generate_quads(&self, stars: &[DetectedStar], image_size: (u32, u32),
                      config: &PatternMatcherConfig) -> Vec<QuadDescriptor>;
}
```

- `QuadDistanceHasher` — astrometry.net style: 6 pairwise distances → 5 normalized ratios → hash key
- `QuadInteriorHasher` — tetra3 style: 3-star enclosing triangle + 1 interior point → 2 barycentric coords → hash key

`PatternMatcher` accepts a `Box<dyn QuadHasher>` and orchestrates: build hash table from catalog stars → generate quads from image stars → hash lookup → candidate transforms → verification.

### Core Types

```rust
pub struct CatalogStar {
    pub ra: f64,      // degrees, epoch-corrected
    pub dec: f64,
    pub mag: f32,
}

pub struct ProjectedStar {
    pub xi: f64,      // gnomonic tangent plane
    pub eta: f64,
    pub mag: f32,
    pub ra: f64,
    pub dec: f64,
}

pub struct StarMatch {
    pub image_idx: usize,
    pub catalog_idx: usize,
    pub residual_px: f64,
}

pub struct PatternMatcherConfig {
    pub max_stars: usize,        // default: 100
    pub hash_tolerance: f64,     // quantization bin size
    pub scale_hint: Option<f64>, // expected arcsec/px, ±20% filter
    pub multi_probe: bool,       // check adjacent hash bins
}

pub struct RansacConfig {
    pub threshold_px: f64,       // inlier distance (default: 2.5)
    pub max_iterations: u32,     // default: 100
    pub min_inliers: usize,      // default: 6
}

pub enum FitModel {
    Affine,
    Projective,
    Sip { order: u8 },           // 2-5
}

pub struct SolveHints {
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub fov_deg: Option<f64>,
    pub rotation: Option<f64>,
    pub pixel_scale_arcsec: Option<f64>,
}
```

### WcsSolution

```rust
pub struct WcsSolution {
    pub crpix: (f64, f64),
    pub crval: (f64, f64),                    // RA, Dec degrees
    pub cd: [[f64; 2]; 2],                    // CD matrix
    pub sip_forward: Option<(SipCoefficients, SipCoefficients)>,  // (A, B)
    pub sip_reverse: Option<(SipCoefficients, SipCoefficients)>,  // (AP, BP)
}

impl WcsSolution {
    pub fn pixel_to_sky(&self, x: f64, y: f64) -> (f64, f64);
    pub fn sky_to_pixel(&self, ra: f64, dec: f64) -> (f64, f64);
    pub fn to_fits_headers(&self) -> Vec<(String, String)>;
    pub fn pixel_scale_arcsec(&self) -> f64;
    pub fn field_rotation_deg(&self) -> f64;
}
```

### Public Exports (added to rustafits lib.rs)

`PatternMatcher`, `PatternMatcherConfig`, `QuadDistanceHasher`, `QuadInteriorHasher`, `RansacFilter`, `RansacConfig`, `TransformFitter`, `FitModel`, `WcsSolution`, `SipCoefficients`, `GnomonicProjection`, `ProperMotionCorrector`, `CatalogStar`, `ProjectedStar`, `StarMatch`, `SolveHints`

Existing `DetectedStar` from `analysis/detection.rs` is reused directly — it already has x, y, flux, fwhm.

---

## Catalog Engine

### Tycho-2 Distribution

Tycho-2 (~2.5M stars, V < 11.5, public domain) ships as pre-converted HEALpix binary files bundled in app resources (~30-50 MB). Extracted to the catalog directory on first plate solve attempt (checked by `CatalogEngine::ensure_catalog_installed()`):
- Desktop: `{app_data_dir}/catalogs/tycho2/`
- Docker: `{ATHENAEUM_DATA_PATH}/catalogs/tycho2/`

### HEALpix Binary Format

Level 6 indexing = 49,152 sky pixels (~0.84° each).

14-byte fixed-size records per star:

| Offset | Type | Field | Notes |
| ---- | ---- | ---- | ---- |
| 0 | f32 | ra | degrees, catalog epoch |
| 4 | f32 | dec | degrees, catalog epoch |
| 8 | u16 | mag | magnitude x 1000 |
| 10 | i16 | pmra | proper motion RA in 0.01 mas/yr |
| 12 | i16 | pmdec | proper motion Dec in 0.01 mas/yr |

Total: 4+4+2+2+2 = 14 bytes per star (the requirements doc says 12 — arithmetic error there).

Stars sorted by magnitude ascending within each file for early termination on mag_limit.

### Cone Search

```
CatalogEngine::cone_search(ra, dec, radius_deg, mag_limit, epoch):
  1. cdshealpix crate → overlapping HEALpix pixel IDs
  2. memmap2 each pixel file
  3. Read 12-byte records until mag > mag_limit (early stop)
  4. rustafits::ProperMotionCorrector::propagate() for epoch correction
  5. Return Vec<CatalogStar>
```

Typical query: ~20 pixels x ~1000 stars = ~240 KB of disk reads, effectively instant with memmap.

### New Dependencies (athenaeum-core)

- `cdshealpix` — HEALpix pixel indexing (CDS Strasbourg, pure Rust)
- `memmap2` — memory-mapped file I/O
- `byteorder` — binary catalog reading

---

## Plate Solve Service

### Hint Resolution Strategy

The primary use case is frames without coordinates. Hints are resolved in priority order:

1. **Frame has RA/Dec** (from FITS header or prior solve) → use directly
2. **Frame has OBJECT keyword** → look up coordinates from a built-in target catalog (~15,000 entries: Messier, NGC, IC, named stars, common targets). No network needed.
3. **Nearby frame already solved** → if another frame in the same directory or session has been solved (or has coordinates), use its RA/Dec as hint. Frames from the same session typically point at the same region of sky.
4. **User provides manual hint** → UI allows entering approximate RA/Dec or target name before solving
5. **No hint available** → cannot solve (report to user: "No coordinate hint — provide target name or approximate RA/Dec"). Blind solving deferred to future release.

FOV and pixel scale hints always come from frame metadata (focallen + xpixsz + naxis1). If these are also missing, the solver reports an error since it cannot estimate search radius.

### Built-In Target Catalog

A lightweight CSV/binary file (~200 KB) bundled with the app containing:
- All 110 Messier objects
- NGC/IC catalog (~13,000 entries)
- Named stars and common asterisms
- Fields: name, aliases, RA (J2000), Dec (J2000), angular size

Loaded into memory on first use. Searched by exact and fuzzy match on the OBJECT keyword.

### Orchestration Pipeline

```
PlateSolveService::solve_frame(frame_id):
  1. Load Frame from DB → resolve SolveHints (hints.rs)
     - Try: frame.ra/dec → OBJECT lookup → nearby solved frame → fail
     - FOV from frame.focallen + frame.xpixsz + frame.naxis1
     - Pixel scale from frame.focallen + frame.xpixsz
  2. Run star detection: rustafits::ImageAnalyzer on the FITS file → DetectedStar[]
  3. CatalogEngine::cone_search(hint_ra, hint_dec, fov * 2, mag_limit, obs_epoch)
  4. GnomonicProjection::sky_to_tangent() on catalog stars
  5. PatternMatcher::build() from projected catalog stars
  6. PatternMatcher::match_stars() with detected image stars
  7. RansacFilter::filter() for outlier rejection
  8. TransformFitter::fit_wcs() with matched pairs
  9. Store SolveResult in plate_solves table
  10. Update frames.ra, frames.dec, frames.rotation, frames.objctra, frames.objctdec
```

### Shared Processing Queue

Plate solve batch tasks and analysis tasks share a single `ProcessingQueue` in `ServiceContext`:

```rust
enum ProcessingTask {
    Analysis { frame_set_id: i64, frame_ids: Vec<i64> },
    PlateSolve { frame_ids: Vec<i64> },
}
```

Tasks execute sequentially — a plate solve batch waits for any in-progress analysis and vice versa. Cancel cancels the current task and the queue advances.

### SolveResult

```rust
pub struct SolveResult {
    pub wcs: WcsSolution,
    pub matched_stars: usize,
    pub total_detected: usize,
    pub rms_residual_px: f64,
    pub rms_residual_arcsec: f64,
    pub pixel_scale_arcsec: f64,
    pub field_rotation_deg: f64,
    pub solve_time_ms: u64,
    pub catalog_used: String,      // "tycho2"
    pub algorithm_used: String,    // "quad_distance" | "quad_interior"
}
```

---

## Database Schema

### New Table: plate_solves

```sql
CREATE TABLE IF NOT EXISTS plate_solves (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    frame_id INTEGER NOT NULL UNIQUE,

    -- WCS core
    crpix1 REAL NOT NULL,
    crpix2 REAL NOT NULL,
    crval1 REAL NOT NULL,           -- RA at reference pixel (degrees)
    crval2 REAL NOT NULL,           -- Dec at reference pixel (degrees)
    cd1_1 REAL NOT NULL,
    cd1_2 REAL NOT NULL,
    cd2_1 REAL NOT NULL,
    cd2_2 REAL NOT NULL,

    -- SIP distortion (nullable)
    sip_order INTEGER,
    sip_a_coeffs TEXT,              -- JSON [[f64; 6]; 6]
    sip_b_coeffs TEXT,
    sip_ap_coeffs TEXT,
    sip_bp_coeffs TEXT,

    -- Quality metrics
    matched_stars INTEGER NOT NULL,
    total_detected INTEGER NOT NULL,
    rms_residual_px REAL NOT NULL,
    rms_residual_arcsec REAL NOT NULL,
    pixel_scale_arcsec REAL NOT NULL,
    field_rotation_deg REAL NOT NULL,

    -- Solve metadata
    solve_time_ms INTEGER NOT NULL,
    catalog_used TEXT NOT NULL,
    algorithm_used TEXT NOT NULL,
    solved_at TEXT NOT NULL,

    FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE
);

CREATE INDEX idx_plate_solves_frame_id ON plate_solves(frame_id);
```

On successful solve: insert/replace into `plate_solves`, update `frames.ra`, `frames.dec`, `frames.rotation`, generate `frames.objctra`/`frames.objctdec` via existing `format_ra_sexagesimal`/`format_dec_sexagesimal`.

On re-solve: `INSERT OR REPLACE` on UNIQUE frame_id — latest solve wins.

---

## Commands and Routes

### Tauri Commands (commands/plate_solve.rs)

| Command | Description |
| ---- | ---- |
| `plate_solve_frame(frame_id)` | Solve single frame, return SolveResult |
| `plate_solve_batch(frame_ids)` | Queue batch solve, emit progress events |
| `cancel_plate_solve()` | Cancel in-progress batch |
| `get_plate_solve_result(frame_id)` | Fetch stored solve result |
| `get_plate_solve_config()` | Load solver settings |
| `set_plate_solve_config(config)` | Save solver settings |
| `get_catalog_status()` | Which catalogs installed + paths |

### Web Routes (routes/plate_solve.rs)

Mirror of all Tauri commands. Batch solve uses SSE via `SseProgressEmitter`.

### Progress Events

```typescript
"plate-solve-progress": {
  frame_id: number,
  current: number,
  total: number,
  status: "solving" | "solved" | "failed",
  matched_stars?: number,
  rms_arcsec?: number,
  error?: string,
}

"plate-solve-complete": {
  solved: number,
  failed: number,
  total: number,
  total_time_ms: number,
}
```

### Solver Config

Stored in `settings` table as JSON under key `plate_solve.config` (same pattern as `calibration.matching_config`):

```typescript
interface PlateSolveConfig {
  algorithm: "quad_distance" | "quad_interior";
  max_stars: number;           // default: 100
  sip_order: number;           // default: 3
  ransac_threshold_px: number; // default: 2.5
  min_matched_stars: number;   // default: 6
  hash_tolerance: number;      // default: 0.01
  multi_probe: boolean;        // default: true
}
```

---

## Frontend UI

### Settings Page — New "Plate Solving" Tab

Added alongside existing tabs (general | calibration | analysis | **plate solving**):
- **Catalog status panel**: Shows Tycho-2 as "Bundled" with star count and magnitude range. Gaia DR3 shown as "Not installed" placeholder for future release.
- **Solver parameters**: Algorithm selector (dropdown), max stars, SIP order, RANSAC threshold, minimum matched stars. Grid layout matching existing analysis settings pattern.
- **Load/save** via `get_plate_solve_config` / `set_plate_solve_config` with error/success toast feedback. Reset to defaults button.

### FileManager — Batch Solve from Missing Metadata

On the existing "missing-metadata" tab, when the "Coordinates" category is selected:
- **"Plate Solve Selected (N)"** and **"Plate Solve All (N)"** action buttons
- Per-frame status column: "No coords" → "Queued" → "Solving..." → "Solved" / "Failed"
- Progress bar showing current file, batch progress, matched stars, RMS
- Frames that solve successfully are removed from the missing-metadata list reactively

### Frame Set Detail — Per-Frame Solve

In the calibration hierarchy view, each light frame shows:
- **WCS badge**: green "WCS" checkmark (solved), amber "No WCS" (unsolved), red "Failed" (solve failed)
- **Coordinate info** (when solved): RA, Dec, pixel scale, RMS
- **Action buttons**: "Plate Solve" (unsolved), "Re-solve" (solved), "Retry" (failed)

### Frontend Hook: usePlateSolveProgress

Extends/coordinates with `useAnalysisProgress` via the shared processing queue:
- Tracks active plate solves per frame
- `api.listen("plate-solve-progress")` and `api.listen("plate-solve-complete")`
- Shows "Waiting for analysis..." when queued behind analysis tasks
- Methods: `enqueuePlateSolve(frameIds)`, `cancelPlateSolve()`, `dismissCompleted()`

---

## Testing Strategy

### Level 1: Synthetic Data (rustafits, cargo test)

Synthetic field generator creates ground-truth WCS + star positions with configurable noise:

| Module | Test | Pass Criterion |
| ---- | ---- | ---- |
| GnomonicProjection | Forward + inverse roundtrip | Identity to < 1e-12 radians |
| ProperMotionCorrector | Barnard's star known values | Matches published positions |
| PatternMatcher (both hashers) | Synthetic field match | All true pairs found, zero false |
| RANSAC | Injected 30% outliers | All outliers rejected, inliers preserved |
| TransformFitter (affine) | Perfect pairs → fit | Recovered WCS matches ground truth |
| TransformFitter (SIP) | Field with known distortion | Recovered coefficients match |
| Both QuadHashers | Identical fields | Same match quality, timing comparison |

Stress matrix: noise (0-1 px), star count (10-1000), outliers (0-30%), rotation (0-170°), field size (0.5-10°).

All synthetic tests run in < 30 seconds, no files, no network.

### Level 2: Real Star Positions, Synthetic Image (rustafits)

Hardcoded const arrays of ~200 Orion Belt stars from Tycho-2. Project through known WCS → detected_stars. Run full pipeline.

Pass: recovered WCS gives RMS < 0.1 px.

### Level 3: Real FITS End-to-End (athenaeum-core)

Using existing test files in `rustafits/tests/`:

| File | Test Case |
| ---- | ---- |
| `cocoon.fits` | Dense star field, wide-field |
| `ldn621.fits`, `ldn621-2.fits`, `ldn621-3.fits` | Dark nebula region, multiple frames |
| `ldn1272.fits` | Dark nebula |
| `b150-nontrailed.fits` | Non-trailed wide field |
| `m82.fits`, `m82-good.fits`, `m82-2.fits` | Narrow-field galaxy |
| `m82-outoffocus.fits` | Poor seeing / out of focus |
| `ghost-railegh.fits` | Optical artifacts |
| `osc.fits` | OSC / Bayer pattern |
| `mono.fits` | Monochrome |
| `test.xisf` | XISF format |

Each file needs a PixInsight ground-truth WCS sidecar (JSON with reference RA/Dec, pixel scale, rotation).

Pass criteria: center RA/Dec within 30", pixel scale within 1%, rotation within 0.5°, RMS < 1.0 px, solve time < 2s (hint-based).

### Level 4: Catalog Tests (athenaeum-core)

Miniature test catalog: 5-10 HEALpix pixels, ~100 stars, correct binary format.

| Test | Validates |
| ---- | ---- |
| Cone search returns correct pixels | HEALpix indexing |
| Mag limit stops reading early | Sorted-by-mag early termination |
| Write → read roundtrip | Binary format correctness |
| Different epochs give different positions | Proper motion integration |
| RA=0/360 wraparound | Boundary handling |

### Benchmarks (criterion)

| Operation | Target |
| ---- | ---- |
| Pattern build (1000 catalog stars) | < 100 ms |
| Match (100 image stars) | < 50 ms |
| RANSAC (50 matches) | < 5 ms |
| TransformFitter SIP order 3 | < 10 ms |
| Full hint-based solve | < 500 ms |

Comparative benchmarks: `QuadDistanceHasher` vs `QuadInteriorHasher` on identical data sets.

---

## Verification

### End-to-End Test Workflow

1. Build: `cd src-tauri && cargo test` — all Level 1-4 tests pass
2. Run dev server: `npm run tauri dev`
3. Scan a directory containing FITS files without coordinates
4. Go to File Manager → Missing Metadata → Coordinates — verify frames listed
5. Click "Plate Solve All" — verify progress events, frames solve successfully
6. Verify solved frames now show RA/Dec in the file list
7. Go to Objects → frame set detail — verify WCS badges appear on solved frames
8. Re-solve a frame — verify plate_solves record is replaced
9. Check Settings → Plate Solving — verify config loads/saves/resets
10. Test web backend: `npm run dev:web` + `cargo run -p athenaeum-web` — verify same operations work via HTTP/SSE

### Build Sequence (Phases 1-4)

| Phase | Layer | Deliverable |
| ---- | ---- | ---- |
| 1 | rustafits | GnomonicProjection, ProperMotionCorrector, PatternMatcher (both hashers), RansacFilter, TransformFitter (affine/projective), WcsSolution |
| 2 | athenaeum-core | CatalogEngine (HEALpix binary format, Tycho-2, cone search with memmap2) |
| 3 | both | PlateSolveService orchestration, plate_solves table, hint extraction, Tauri commands + web routes, progress events |
| 4 | rustafits | SIP distortion fitting in TransformFitter |

Phase 1 delivers a working star-matching engine usable for registration immediately. Phases 2-3 can proceed in parallel after Phase 1.

---

## Files Modified (Existing)

| File | Change |
| ---- | ---- |
| `rustafits/Cargo.toml` | Version → 1.0.0, add nalgebra dependency |
| `rustafits/src/lib.rs` | Re-export platesolving module |
| `athenaeum-core/Cargo.toml` | Add cdshealpix, memmap2, byteorder |
| `athenaeum-core/src/lib.rs` | Export catalog and plate_solve modules |
| `athenaeum-core/src/db/schema.rs` | Add plate_solves table creation |
| `athenaeum-tauri/src/commands/mod.rs` | Add plate_solve module, re-exports |
| `athenaeum-tauri/src/lib.rs` | Register plate_solve commands in invoke_handler |
| `athenaeum-web/src/routes/mod.rs` | Register plate_solve routes |
| `package.json` | Version → 0.2.0 |
| `crates/athenaeum-core/Cargo.toml` | Version → 0.2.0 |
| `crates/athenaeum-tauri/Cargo.toml` | Version → 0.2.0 |
| `crates/athenaeum-tauri/tauri.conf.json` | Version → 0.2.0 |
| `crates/athenaeum-web/Cargo.toml` | Version → 0.2.0 |
| `src/pages/Settings.tsx` | Add "plate solving" tab |
| `src/types/models.ts` | Add PlateSolveResult, PlateSolveConfig interfaces |

## Files Created (New)

All files listed in the Module Layout section above, plus:
- `src/components/plate-solve/PlateSolveSettingsPanel.tsx`
- `src/components/plate-solve/PlateSolveBatchPanel.tsx`
- `src/hooks/usePlateSolveProgress.ts`
- `src/types/plate-solve.ts`
- `athenaeum-core/src/plate_solve/target_catalog.rs` — built-in target name resolver
- `resources/targets.csv` — bundled Messier/NGC/IC target catalog (~200 KB)
