# Requirements: Plate Solving — rustafits + Athenaeum

## Purpose

A plate solving system split across two layers:
- **rustafits** — pure computational algorithms (pattern matching, transform fitting, WCS math). No I/O, no filesystem, no network. Accepts data, returns data.
- **Athenaeum** — catalog management, file I/O, download, orchestration. Feeds data into rustafits algorithms and stores results.

This separation allows rustafits algorithms to also serve star-matched registration (no catalog needed — reference frame stars act as "catalog") and mosaic stitching.

## What Plate Solving Does

Given an astronomical image with detected stars (pixel x, y, flux), determine:
1. Where in the sky the image points (RA, Dec of center)
2. The image orientation (rotation angle)
3. The precise pixel-to-sky coordinate mapping (WCS solution)
4. Optical distortion model (SIP polynomial coefficients)

---

## Layer Boundary

### rustafits (library — pure computation)

All modules below accept in-memory data structures and return results. No `&Path`, no `File::open`, no network calls.

```
StarDetector (already exists)
  └── x, y, flux, FWHM per star

PatternMatcher (new)
  ├── Quad descriptor generation from star lists
  ├── Hash table build + lookup
  ├── Candidate transform from matched quads
  └── Reusable for both plate solve (image↔catalog) and registration (image↔image)

RansacFilter (new)
  └── Outlier rejection on matched star pairs

TransformFitter (new, pluggable)
  ├── Affine (6 params)
  ├── Projective/Homography (8 params)
  ├── SIP Polynomial (N params)
  └── Thin Plate Spline (for registration/mosaics)

GnomonicProjection (new)
  ├── sky_to_tangent(ra, dec, ra0, dec0) → (ξ, η)
  └── tangent_to_sky(ξ, η, ra0, dec0) → (ra, dec)

ProperMotionCorrector (new)
  └── propagate(ra, dec, pmra, pmdec, epoch_from, epoch_to) → (ra', dec')

WcsSolution (new)
  ├── pixel_to_sky(x, y) → (ra, dec)
  ├── sky_to_pixel(ra, dec) → (x, y)
  └── to_fits_headers() → Vec<(String, String)>
```

### Athenaeum (application — I/O + orchestration)

```
CatalogEngine (new)
  ├── HEALpix-indexed binary file management
  ├── Cone search across on-disk files (memmap2)
  ├── Catalog download from ESA Gaia TAP / VizieR
  ├── Tycho-2 → binary conversion (ships bundled)
  ├── Gaia DR3 → binary conversion (user-initiated download)
  └── Online fallback queries

PlateSolveService (new)
  ├── Orchestrates: CatalogEngine → rustafits algorithms → store result
  ├── Hint extraction from FITS headers (RA, Dec, FOCALLEN, PIXSIZE)
  ├── Catalog tier selection (Gaia local → Tycho-2 → online)
  └── Result storage in SQLite

CatalogDownloader (new)
  ├── ESA Gaia TAP bulk download
  ├── Progress tracking + resume
  └── Binary format conversion
```

---

## Architecture Overview

See **"Catalog Data Flow (detailed)"** section below for the complete data flow diagram with code examples, covering: disk layout → cone search → projection → pattern matching → WCS fitting, and the registration variant (no catalog).

---

## Algorithm: Quad Hash Matching (tetra3 approach)

### Why quads over triangles
- Triangle: 2 invariant ratios → high false match rate
- Quad: 5 invariant ratios → dramatically fewer false matches
- Hash table: O(1) lookup vs O(N²) brute force comparison
- Proven in tetra3 (ESA), astrometry.net, ASTAP

### Pattern encoding — two approaches to evaluate

**Approach A: Distance ratios (astrometry.net style)**
1. Select 4 stars from star list (brightest first, breadth-first)
2. Compute all 6 pairwise distances
3. Normalize by the longest distance → 5 ratios in [0, 1]
4. Sort ratios for canonical ordering (flip/rotation invariant)
5. Quantize into hash table key

**Approach B: Interior point (tetra3 style)**
1. Select 4 stars, identify 3 forming the enclosing triangle + 1 interior
2. Express interior star position as 2 barycentric-like coordinates in [0, 1]
3. Hash key = 2 quantized floats (more compact than 5)
4. Fewer collisions per bucket, faster lookup

Recommendation: evaluate both on real data. Approach B is more compact (smaller hash table, faster for blind solve), but Approach A may be more robust to star detection noise. For hint-based solving with narrow search space, difference is marginal.

### Hash tolerance strategy

Quantization bin size determines the tradeoff between missed matches and false collisions:
- Too fine → misses due to centroid noise (typically 0.1–0.3 px)
- Too coarse → too many collisions, slow verification
- Approach: multi-probe hashing — check adjacent bins (±1 in each dimension)
- Configurable via `SolveConfig::hash_tolerance`

### Matching procedure
1. Extract N brightest stars from image (typically 50–200)
2. Generate candidate quads from image stars
3. For each quad: compute hash → lookup in catalog hash table
4. Each match produces a candidate transform (4 star pairs → projective)
5. Apply candidate transform to all image stars → count catalog matches within tolerance
6. RANSAC: reject outlier pairs, re-fit with inliers only
7. Iterative refinement: apply refined transform → find more matches → re-fit
8. Accept solution when matched star count and RMS pass thresholds

### Hint-based solving (primary mode for Athenaeum)
- Athenaeum provides approximate RA/Dec, focal length, pixel size from FITS headers
- CatalogEngine loads only stars within ~2× FOV of hint position
- Scale hint: reject quads whose scale differs >20% from expected pixel scale
- Reduces search space by orders of magnitude → solve in <1 second

### Blind solving (fallback)
- No hints needed — searches broader sky area
- Slower (seconds to tens of seconds) but still feasible
- Uses multi-scale index: try coarse scale first, refine
- CatalogEngine provides progressively wider search regions

---

## Star Catalog Strategy

### Licensing constraints

Gaia DR3 is licensed **CC BY-NC 3.0 IGO** — free for non-commercial use with attribution, but commercial redistribution is restricted. PixInsight (commercial) works around this by having users download Gaia catalogs separately rather than shipping them. We follow the same approach.

### Three-tier catalog strategy

**Tier 1: Bundled — Tycho-2 (ships with Athenaeum)**

| Property | Value |
| ---- | ---- |
| Source | Tycho-2 (ESA, public domain) |
| License | Public domain — free for any use including commercial |
| Stars | ~2.5 million |
| Magnitude limit | V < 11.5 |
| Size | ~30–50 MB (compact binary format) |
| Epoch | J2000.0 (proper motions included) |
| Use case | Works out of the box for most wide-field amateur imaging |

Note: Tycho-2 ships with **Athenaeum**, not rustafits. rustafits has no bundled data — it is a pure library.

**Tier 2: User-downloaded — Gaia DR3 subset**

| Property | Value |
| ---- | ---- |
| Source | Gaia DR3 (ESA) |
| License | CC BY-NC 3.0 IGO — user downloads directly, we never redistribute |
| Stars | ~100 million (mag < 14) or ~470 million (mag < 16) |
| Size | ~1.2 GB (mag < 14) or ~5.6 GB (mag < 16), compact format |
| Epoch | J2016.0 (proper motions included) |
| Use case | Narrow-field, faint targets, deep imaging, high-precision astrometry |

Built-in catalog downloader in Athenaeum settings fetches from ESA Gaia archive, converts to binary format locally.

**Tier 3: Online fallback — VizieR/Gaia TAP**

| Property | Value |
| ---- | ---- |
| Source | VizieR or ESA Gaia TAP service |
| License | N/A (querying, not redistributing) |
| Use case | No local catalog installed, occasional use, first-time setup |
| Limitation | Requires internet, slower, rate-limited |

### Catalog binary format (defined by Athenaeum, consumed as `&[CatalogStar]` by rustafits)

- HEALpix-indexed binary files (Level 6 = 49,152 sky pixels, ~0.84° each)
- Compact fixed-size records: ra (f32), dec (f32), mag (f16/u16), pmra (f16), pmdec (f16) = 12 bytes/star
  - f32 for ra/dec gives ~0.005 arcsec precision — sufficient for plate solving
  - f16 for proper motion gives ~0.01 mas/yr precision — sufficient for epoch propagation
- Stars sorted by magnitude within each pixel for early termination
- Memory-mappable via `memmap2` for fast access
- Cone search: identify overlapping HEALpix pixels → load only those files

### Catalog tier selection (Athenaeum logic)

```
Athenaeum PlateSolveService:
  1. Check: local Gaia available + covers target region? → use it
  2. Else: local Tycho-2 available? → use it (works for most wide-field)
  3. Else: online fallback (VizieR/TAP query, cache result)
  4. Load stars into Vec<CatalogStar>
  5. Apply ProperMotionCorrector to observation epoch
  6. Pass &[CatalogStar] to rustafits PatternMatcher
```

See **"Catalog Data Flow (detailed)"** section for full code examples including cone search, memmap reading, projection, and the complete solve pipeline.

---

## Catalog Data Flow (detailed)

### Disk layout

```
~/.athenaeum/catalogs/
├── tycho2/                        # ships with Athenaeum
│   ├── meta.bin                   # version, epoch (J2000.0), mag range, record size
│   ├── healpix_000000.bin         # stars in HEALpix pixel 0, sorted by mag
│   ├── healpix_000001.bin
│   ├── ...
│   └── healpix_049151.bin         # 49,152 files total (HEALpix Level 6)
└── gaia_dr3/                      # user-downloaded
    ├── meta.bin                   # epoch J2016.0, mag < 14
    ├── healpix_000000.bin
    └── ...
```

Each HEALpix pixel covers ~0.84° of sky. Files are small (a few KB to ~100 KB depending on star density in that region).

### Binary record format (12 bytes per star)

```
Offset  Type   Field       Notes
0       f32    ra          degrees, catalog epoch
4       f32    dec         degrees, catalog epoch
8       u16    mag         magnitude × 1000 (e.g. 10500 = mag 10.5)
10      i16    pmra        proper motion RA in 0.01 mas/yr (±327 mas/yr range)
12      i16    pmdec       proper motion Dec in 0.01 mas/yr
─────────────────────────
Total: 12 bytes/star

Stars sorted by mag ascending within each file → early termination on mag_limit.
```

### Cone search: from sky position to star list

```rust
// Athenaeum CatalogEngine — all I/O happens here
fn cone_search(&self, ra: f64, dec: f64, radius_deg: f64,
               mag_limit: f32, epoch: f64) -> Result<Vec<CatalogStar>> {
    
    // Step 1: Which HEALpix pixels overlap the search circle?
    let pixel_ids = healpix::cone_search(ra, dec, radius_deg, HEALPIX_LEVEL);
    // Typical FOV 2° → ~20-30 pixels out of 49,152
    
    let mut stars = Vec::new();
    
    for pid in pixel_ids {
        // Step 2: Memory-map the file (OS loads pages on demand)
        let path = self.catalog_dir.join(format!("healpix_{:06}.bin", pid));
        let mmap = memmap2::Mmap::open(&path)?;
        
        // Step 3: Read records until mag_limit hit
        for chunk in mmap.chunks_exact(12) {
            let raw = parse_record(chunk);
            if raw.mag_f32() > mag_limit { break; }  // sorted → stop early
            
            // Step 4: Proper motion correction (pure rustafits function)
            let (ra_corr, dec_corr) = rustafits::ProperMotionCorrector::propagate(
                raw.ra as f64, raw.dec as f64,
                raw.pmra_mas_yr(), raw.pmdec_mas_yr(),
                self.catalog_epoch,  // J2000.0 for Tycho-2, J2016.0 for Gaia
                epoch,               // observation date, e.g. 2025.5
            );
            
            stars.push(CatalogStar { ra: ra_corr, dec: dec_corr, mag: raw.mag_f32() });
        }
    }
    
    Ok(stars)
}
```

Performance: ~20 pixels × ~1000 stars/pixel = ~20K stars × 12 bytes = **~240 KB** of actual disk reads. With memmap this is effectively instant.

### From catalog stars to pattern hash table

Before PatternMatcher can work, catalog star positions (RA/Dec in degrees on a sphere) must be projected onto a flat plane where Euclidean distances are meaningful:

```rust
// Athenaeum PlateSolveService — prepare data for rustafits
fn solve_frame(&self, detected: &[DetectedStar],
               image_size: (u32, u32), hints: &SolveHints) -> Result<SolveResult> {
    
    // 1. Load catalog stars (Athenaeum I/O)
    let catalog_stars = self.catalog.cone_search(
        hints.ra, hints.dec,
        hints.fov_deg * 2.0,   // search 2× FOV for margin
        14.0,                   // mag limit
        hints.observation_epoch,
    )?;
    
    // 2. Project catalog onto tangent plane (rustafits pure math)
    //    This converts spherical RA/Dec → flat (ξ, η) where distances are Euclidean
    let catalog_projected: Vec<ProjectedStar> = catalog_stars.iter()
        .map(|s| {
            let (xi, eta) = rustafits::GnomonicProjection::sky_to_tangent(
                s.ra, s.dec, hints.ra, hints.dec
            );
            ProjectedStar { xi, eta, mag: s.mag, ra: s.ra, dec: s.dec }
        })
        .collect();
    
    // 3. Build hash table from projected catalog stars (rustafits)
    let matcher = rustafits::PatternMatcher::build(&catalog_projected, &match_config);
    
    // 4. Match image stars against hash table (rustafits)
    let matches = matcher.match_stars(&detected, image_size);
    
    // 5. RANSAC outlier rejection (rustafits)
    let filtered = rustafits::RansacFilter::filter(
        &matches, &detected, &catalog_stars, &ransac_config
    );
    
    // 6. Fit WCS from matched pairs (rustafits)
    let pairs: Vec<_> = filtered.iter()
        .map(|m| (
            (detected[m.image_idx].x, detected[m.image_idx].y),
            (catalog_stars[m.catalog_idx].ra, catalog_stars[m.catalog_idx].dec),
        ))
        .collect();
    
    let wcs = rustafits::TransformFitter::fit_wcs(
        &pairs, FitModel::Sip { order: 3 }, image_center
    )?;
    
    // 7. Build result (Athenaeum stores in SQLite)
    Ok(SolveResult { wcs, matched_stars: filtered.len(), /* ... */ })
}
```

### Complete data flow diagram

```
Disk                  Athenaeum                    rustafits
────                  ─────────                    ─────────

healpix_*.bin ──► memmap read
                  parse 12-byte records
                  mag filter (early stop)
                  ──────────────────────────► ProperMotionCorrector
                  catalog_stars ◄────────────  propagate(ra, dec, pm, epoch)
                  
                  ──────────────────────────► GnomonicProjection
                  projected_stars ◄──────────  sky_to_tangent(ra, dec, ra0, dec0)
                  
                  ──────────────────────────► PatternMatcher::build()
                                               generates quads from projected stars
                                               computes hash for each quad
                                               builds HashMap<HashKey, Vec<Quad>>
                  
pixels ─────────► StarDetector ─────────────► detected_stars: Vec<DetectedStar>
                  (already exists)
                  
                  ──────────────────────────► matcher.match_stars(&detected)
                                               generates quads from image stars
                                               hash lookup → candidate transforms
                                               verify: transform all stars, count matches
                  matches ◄──────────────────  Vec<StarMatch>
                  
                  ──────────────────────────► RansacFilter::filter()
                                               iterative outlier rejection
                  filtered ◄─────────────────  Vec<StarMatch> (inliers only)
                  
                  ──────────────────────────► TransformFitter::fit_wcs()
                                               gnomonic projection of sky coords
                                               least-squares CD matrix fit
                                               SIP polynomial fit on residuals
                  wcs ◄──────────────────────  WcsSolution
                  
                  store in SQLite
                  pass to pipeline
```

### Registration flow (no catalog, no I/O)

The same PatternMatcher works for frame-to-frame registration without any catalog:

```
reference frame pixels ──► StarDetector ──► reference_stars
                                            │
                              PatternMatcher::build_from_pixels(&reference_stars)
                              (reference stars ARE the "catalog" — same hash algorithm)
                                            │
target frame pixels ─────► StarDetector ──► target_stars
                                            │
                              matcher.match_stars(&target_stars)
                              (same matching algorithm, no projection needed —
                               both sides are already in pixel space)
                                            │
                              RansacFilter::filter()
                                            │
                              TransformFitter::fit_pixel()
                              (affine/projective/TPS — pixel↔pixel, no WCS)
                                            │
                                        PixelTransform
```

No disk access, no catalog, no CatalogEngine. rustafits never touches the filesystem.

---

## Proper Motion Propagation

Lives in rustafits as a pure function:

```
ra'  = ra  + pmra  × (epoch_obs − epoch_cat) / cos(dec) / 3_600_000
dec' = dec + pmdec × (epoch_obs − epoch_cat) / 3_600_000
```

Where pm is in mas/yr, ra/dec in degrees.

Critical for long baselines: Tycho-2 epoch J2000 → observation in 2025 = 25 years. Fast stars (e.g. Barnard's star: ~10 arcsec/yr) shift by ~250 arcsec = ~4 arcmin. Without correction, these stars will not match catalog positions and could poison the solution.

---

## WCS Fitting

### After matching N star pairs (pixel ↔ catalog sky position):

**Step 1: Gnomonic (TAN) projection** (rustafits GnomonicProjection)
- Project catalog RA/Dec onto tangent plane at estimated field center
- Standard coordinates (ξ, η) in radians

**Step 2: Fit CD matrix** (rustafits TransformFitter with FitModel::Affine)
- 4 unknowns: CD1_1, CD1_2, CD2_1, CD2_2 (CRPIX fixed at image center)
- Linear least squares: solve normal equations Ax = b
- `nalgebra` crate for matrix operations

**Step 3: Fit SIP distortion** (rustafits TransformFitter with FitModel::Sip)
- Only if N > ~20 matched stars
- Order 2–5 polynomial (typically 3 for amateur telescopes)
- Iterative: fit linear first, then add distortion terms progressively
- Additional least-squares fit for A_i_j, B_i_j coefficients on residuals

**Step 4: Quality assessment** (rustafits SolveResult)
- RMS residual in pixels and arcseconds
- Per-star residual vector for remaining outlier inspection
- Target: RMS < 1.0 pixel (< 0.5 arcsec for typical setups)

---

## API Design

### rustafits public interface (pure computation)

```rust
/// Stars detected in an image (already exists)
pub struct DetectedStar {
    pub x: f64,
    pub y: f64,
    pub flux: f64,
    pub fwhm: f32,
}

/// Stars from a catalog, loaded by the application
pub struct CatalogStar {
    pub ra: f64,      // degrees, epoch-corrected
    pub dec: f64,     // degrees, epoch-corrected
    pub mag: f32,
}

/// Matched pair: image star index ↔ catalog star index
pub struct StarMatch {
    pub image_idx: usize,
    pub catalog_idx: usize,
    pub residual_px: f64,
}

// ── Pattern Matching ──────────────────────────────────

pub struct PatternMatcherConfig {
    pub max_stars: usize,           // max stars to use (default: 100)
    pub hash_tolerance: f64,        // quantization bin size
    pub scale_hint: Option<f64>,    // expected arcsec/px, ±20% filter
    pub multi_probe: bool,          // check adjacent hash bins
}

pub struct PatternMatcher { /* config + built hash table */ }

impl PatternMatcher {
    /// Build hash table from catalog/reference stars
    pub fn build(stars: &[CatalogStar], config: &PatternMatcherConfig) -> Self;
    
    /// Also works for registration: reference frame stars as "catalog"
    pub fn build_from_pixels(stars: &[DetectedStar], config: &PatternMatcherConfig) -> Self;
    
    /// Match image stars against the built hash table
    pub fn match_stars(&self, image_stars: &[DetectedStar],
                       image_size: (u32, u32)) -> Vec<StarMatch>;
}

// ── RANSAC ────────────────────────────────────────────

pub struct RansacConfig {
    pub threshold_px: f64,    // inlier distance threshold (default: 2.5)
    pub max_iterations: u32,  // default: 100
    pub min_inliers: usize,   // minimum to accept (default: 6)
}

pub struct RansacFilter;

impl RansacFilter {
    pub fn filter(matches: &[StarMatch],
                  image_stars: &[DetectedStar],
                  catalog_stars: &[CatalogStar],
                  config: &RansacConfig) -> Vec<StarMatch>;
}

// ── Transform Fitting ─────────────────────────────────

pub enum FitModel {
    Affine,
    Projective,
    Sip { order: u8 },        // 2–5
    ThinPlateSpline,
}

pub struct TransformFitter;

impl TransformFitter {
    /// Fit pixel↔sky transform from matched pairs
    pub fn fit_wcs(pairs: &[(/* pixel */ (f64, f64), /* sky */ (f64, f64))],
                   model: FitModel,
                   image_center: (f64, f64)) -> Result<WcsSolution>;
    
    /// Fit pixel↔pixel transform (for registration, no sky coords)
    pub fn fit_pixel(pairs: &[(/* source */ (f64, f64), /* target */ (f64, f64))],
                     model: FitModel) -> Result<PixelTransform>;
}

// ── WCS Solution ──────────────────────────────────────

pub struct SipCoefficients {
    pub order: u8,                  // 2–5
    pub coeffs: [[f64; 6]; 6],     // max order 5, upper triangle used
}

pub struct WcsSolution {
    pub crpix: (f64, f64),
    pub crval: (f64, f64),          // RA, Dec in degrees
    pub cd: [[f64; 2]; 2],         // CD matrix
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

// ── Projections ───────────────────────────────────────

pub struct GnomonicProjection;

impl GnomonicProjection {
    pub fn sky_to_tangent(ra: f64, dec: f64, ra0: f64, dec0: f64) -> (f64, f64);
    pub fn tangent_to_sky(xi: f64, eta: f64, ra0: f64, dec0: f64) -> (f64, f64);
}

// ── Proper Motion ─────────────────────────────────────

pub struct ProperMotionCorrector;

impl ProperMotionCorrector {
    /// Propagate star position from catalog epoch to observation epoch
    pub fn propagate(ra: f64, dec: f64,
                     pmra_mas_yr: f64, pmdec_mas_yr: f64,
                     epoch_from: f64, epoch_to: f64) -> (f64, f64);
}

// ── Pixel Transform (for registration) ────────────────

pub struct PixelTransform {
    pub model: FitModel,
    // internal representation depends on model
}

impl PixelTransform {
    pub fn transform(&self, x: f64, y: f64) -> (f64, f64);
    pub fn inverse(&self, x: f64, y: f64) -> Option<(f64, f64)>;
}
```

### Athenaeum interface (orchestration)

```rust
// ── Catalog Engine (Athenaeum, I/O layer) ─────────────

pub struct CatalogEngine {
    gaia_path: Option<PathBuf>,
    tycho2_path: Option<PathBuf>,
    online_cache: PathBuf,
}

impl CatalogEngine {
    /// Load catalog stars for a sky region, epoch-corrected
    pub fn cone_search(&self, ra: f64, dec: f64,
                       radius_deg: f64, mag_limit: f32,
                       epoch: f64) -> Result<Vec<CatalogStar>>;
    
    pub fn available_catalogs(&self) -> Vec<CatalogInfo>;
    pub fn catalog_coverage(&self, catalog: CatalogTier) -> SkyRegion;
}

pub struct CatalogDownloader;

impl CatalogDownloader {
    pub async fn download_gaia(config: GaiaDownloadConfig,
                               progress: impl Fn(DownloadProgress))
        -> Result<PathBuf>;
}

// ── Plate Solve Service (Athenaeum, orchestration) ────

pub struct PlateSolveService {
    catalog: CatalogEngine,
}

impl PlateSolveService {
    pub fn solve_frame(&self,
                       detected_stars: &[DetectedStar],
                       image_size: (u32, u32),
                       hints: &SolveHints) -> Result<SolveResult> {
        // 1. Extract hint RA/Dec/FOV from FITS metadata
        // 2. CatalogEngine.cone_search() → catalog_stars
        // 3. ProperMotionCorrector on catalog_stars
        // 4. PatternMatcher::build(&catalog_stars, &config)
        // 5. matcher.match_stars(&detected_stars, image_size)
        // 6. RansacFilter::filter(&matches, ...)
        // 7. TransformFitter::fit_wcs(&pairs, FitModel::Sip, center)
        // 8. Return SolveResult, store in SQLite
    }
}

pub struct SolveHints {
    pub ra: Option<f64>,
    pub dec: Option<f64>,
    pub fov_deg: Option<f64>,
    pub rotation: Option<f64>,
    pub pixel_scale_arcsec: Option<f64>,
}

pub struct SolveResult {
    pub wcs: WcsSolution,
    pub matched_stars: usize,
    pub total_detected: usize,
    pub rms_residual_px: f64,
    pub rms_residual_arcsec: f64,
    pub solve_time_ms: u64,
    pub catalog_used: CatalogTier,
    pub distortion_order: Option<u8>,
}
```

---

## Dual Use: Plate Solve + Registration

The same `PatternMatcher` serves both plate solving and star-matched registration:

**Plate solving** (image ↔ catalog):
```
Athenaeum loads catalog_stars from CatalogEngine
  → PatternMatcher::build(&catalog_stars, ...)
  → matcher.match_stars(&detected_stars, ...)
  → TransformFitter::fit_wcs(...)
  → WcsSolution
```

**Star-matched registration** (image ↔ reference image):
```
No catalog needed — reference frame stars ARE the "catalog"
  → PatternMatcher::build_from_pixels(&reference_stars, ...)
  → matcher.match_stars(&target_stars, ...)
  → TransformFitter::fit_pixel(...)
  → PixelTransform
```

**Astrometric registration** (image ↔ sky grid via WCS):
```
Each frame plate-solved independently
  → reproject via WcsSolution::sky_to_pixel / pixel_to_sky
  → all frames on common sky grid
  → highest precision, handles distortion natively
```

---

## Future: Mosaics (built on this foundation)

### Mosaic panel stitching
- Plate solve each panel independently → each gets WCS with SIP distortion
- Reproject all panels to common sky grid via WCS
- Blend overlap regions (feathering / gradient domain)
- WCS quality from plate solving directly determines mosaic accuracy

### Reprojection (future rustafits module)
- Input: source pixels + source WcsSolution + target WcsSolution
- For each target pixel: `target_pixel → target_sky (WCS⁻¹) → source_sky → source_pixel (WCS)`
- Lanczos-3 resampling at source pixel coordinates
- Pure computation — fits in rustafits

---

## Dependencies

### rustafits (library crate)

| Crate | Purpose |
| ---- | ---- |
| `nalgebra` | Linear algebra, least squares |
| `rayon` | Parallel quad generation |

No filesystem or network dependencies added.

### Athenaeum (application)

| Crate | Purpose |
| ---- | ---- |
| `healpix` (or custom) | HEALpix pixel indexing |
| `memmap2` | Memory-mapped catalog files |
| `byteorder` | Binary catalog reading |
| `reqwest` | Online catalog fallback (TAP/VizieR) |

---

## Build Phases

| Phase | Layer | Deliverable | Enables |
| ---- | ---- | ---- | ---- |
| 1 | rustafits | PatternMatcher + RansacFilter + TransformFitter (affine/projective) | Star matching (registration use case works immediately) |
| 2 | Athenaeum | CatalogEngine (HEALpix binary format, Tycho-2 bundled) | Catalog queries |
| 3 | Both | PlateSolveService + WcsSolution + GnomonicProjection | Hint-based plate solving in Athenaeum |
| 4 | rustafits | SIP distortion fitting in TransformFitter | Wide-field accuracy |
| 5 | rustafits | Blind solve support (multi-scale quad generation) | No-hint fallback |
| 6 | Athenaeum | Gaia DR3 downloader + converter | Deep/narrow-field solving |
| 7 | rustafits | Thin Plate Spline in TransformFitter | Arbitrary distortion for registration |
| 8 | rustafits | WCS reprojection + Lanczos resampling | Astrometric registration, mosaic stitching |

Note: Phase 1 delivers a working star-matching engine usable for same-field registration immediately, before any catalog work is done. Phase 2–3 can proceed in parallel.

---

## Error Budget

### Input requirements for successful solve
- Star centroid accuracy: < 0.3 px (typical for SNR > 10 stars)
- Minimum detected stars: 20+ for hint-based, 50+ for blind
- Minimum matched stars after RANSAC: 6 for affine, 10+ for SIP order 3
- FWHM range: 1.5–15 px (below 1.5 = undersampled, centroids unreliable)

### Output quality targets
- Hint-based solve: RMS < 0.5 px, < 0.3 arcsec, solve time < 1s
- Blind solve: RMS < 1.0 px, solve time < 30s
- SIP distortion: reduces corner residuals from ~5 px to < 0.5 px (typical 200mm FL)

---

## Testing and Development Strategy

### Level 1: Synthetic data (unit tests, rustafits, `cargo test`)

Foundation — tests pure math with no dependency on real images or catalogs.

**Synthetic field generator** (test utility inside rustafits):
```rust
fn generate_synthetic_field(config: &SyntheticConfig)
    -> (Vec<DetectedStar>, Vec<CatalogStar>, WcsSolution)
{
    // 1. Define "ground truth" WCS: center RA/Dec, pixel scale, rotation, SIP
    // 2. Generate N random sky points within FOV → catalog_stars
    // 3. Project through WCS.sky_to_pixel → detected_stars (+ Gaussian noise)
    // 4. Return both sides + ground truth WCS for comparison
}
```

**What each module tests with synthetic data:**

| Module | Test | Pass criterion |
| ---- | ---- | ---- |
| PatternMatcher | Generate field, run match | All true pairs found, zero false |
| TransformFitter | Perfect pairs → fit WCS | Recovered WCS matches ground truth to machine epsilon |
| SIP fitting | Field with known distortion | Recovered coefficients match ground truth |
| RANSAC | Add N% random false pairs | All outliers rejected, inliers preserved |
| GnomonicProjection | Forward + inverse roundtrip | Identity to <1e-12 radians |
| ProperMotionCorrector | Barnard's star known values | Matches published positions |

**Stress test parameter matrix:**

| Parameter | Values |
| ---- | ---- |
| Centroid noise | 0, 0.1, 0.3, 0.5, 1.0 px |
| Star count | 10, 50, 200, 1000 |
| Outlier fraction | 0%, 10%, 30% |
| Field size | 0.5°, 2°, 10° |
| Rotation | 0°, 45°, 170° |
| SIP distortion | 0, 5, 20 px at corners |

All synthetic tests run in <30 seconds total, no files, no network.

### Level 2: Real star positions, synthetic image (integration tests, rustafits)

Hardcoded arrays of real star positions from well-known fields (e.g. Orion Belt region, ~200 stars from Tycho-2). "Photograph" them through a known WCS → detected_stars. Run full pipeline: build → match → RANSAC → fit.

This catches bugs that synthetic random fields miss: real star distributions are clustered, have magnitude gaps, have close pairs, have empty regions.

Data is `const` arrays directly in test code — no files, rustafits stays pure.

**Pass criterion:** Recovered WCS gives pixel_to_sky with RMS residual < 0.1 px on known field.

### Level 3: Real FITS, end-to-end (integration tests, Athenaeum)

Test set: 10–20 real FITS files covering edge cases:

| Case | Challenge |
| ---- | ---- |
| Wide-field (50mm FL) | Strong SIP distortion at corners |
| Narrow-field (2000mm FL) | Few bright stars, faint targets |
| Dense Milky Way | Star confusion, many candidates |
| Sparse high-latitude | Few stars, risk of false match |
| OSC Bayer | Green channel extraction quality |
| Trailed frame | Elongated stars, centroid noise |
| Low SNR | Faint stars, high noise |
| Near celestial pole | RA wrap-around, projection singularity |

Each file has **ground truth WCS from PixInsight ImageSolver** (Gaia DR3, with distortion correction, thin plate splines + surface simplifiers). PixInsight achieves < 0.03 px accuracy — this is the gold standard reference.

**Pass criteria:**

| Metric | Threshold |
| ---- | ---- |
| Center RA/Dec difference vs PixInsight | < 30 arcsec |
| Pixel scale difference | < 1% |
| Rotation difference | < 0.5° |
| RMS residual | < 1.0 px |
| Solve time (hint-based) | < 2 seconds |

**Storage:** FITS files in git-lfs or separate test data repo (50–200 MB each). PixInsight reference WCS stored as JSON sidecar per file. CI downloads on PR to main, not on every commit.

### Level 4: Catalog tests (Athenaeum)

CatalogEngine tested with a miniature test catalog: 5–10 HEALpix pixels, ~100 stars, correct binary format.

| Test | Validates |
| ---- | ---- |
| Cone search returns correct pixels | HEALpix indexing |
| Mag limit stops reading early | Sorted-by-mag early termination |
| Write → read roundtrip | Binary format correctness |
| Different epochs give different positions | Proper motion integration |
| RA=0/360 wrap-around | Boundary handling |
| Pole regions | HEALpix polar pixel geometry |

Test catalog generated by a utility that writes hardcoded star arrays into binary format — a few KB, committed to tests.

### Performance benchmarks (`criterion`)

```rust
// rustafits/benches/plate_solve.rs
fn bench_pattern_build(c: &mut Criterion) {
    let stars = generate_catalog_stars(1000);
    c.bench_function("pattern_build_1000", |b| {
        b.iter(|| PatternMatcher::build(&stars, &config))
    });
}

fn bench_match(c: &mut Criterion) {
    let matcher = /* pre-built from 1000 catalog stars */;
    let detected = generate_detected_stars(100);
    c.bench_function("match_100_stars", |b| {
        b.iter(|| matcher.match_stars(&detected, (4096, 4096)))
    });
}

fn bench_full_solve(c: &mut Criterion) {
    c.bench_function("full_hint_solve", |b| {
        b.iter(|| /* build + match + ransac + fit */)
    });
}
```

**Target budgets:**

| Operation | Target |
| ---- | ---- |
| Pattern build (1000 catalog stars) | < 100 ms |
| Match (100 image stars) | < 50 ms |
| RANSAC (50 matches) | < 5 ms |
| TransformFitter SIP order 3 | < 10 ms |
| Full hint-based solve | < 500 ms |

### Debugging toolkit

**Diagnostic output** — `SolveResult` includes intermediate data beyond WCS:
- Quads generated / hash hits / candidate transforms tested
- Verification scores per candidate
- Per-star residual vectors after final fit
- Catalog stars that were NOT matched (useful for identifying missing detections)

**Visualization** (Athenaeum, not rustafits) — overlay plot showing:
- Detected stars (circles)
- Matched catalog stars (crosses, projected through recovered WCS)
- Residual vectors (lines from detected to projected catalog position)
- Systematic residual patterns → wrong distortion model
- Random large residuals → false matches survived RANSAC

**Regression capture** — when a FITS file fails to solve or gives poor results:
1. Add to Level 3 test set with annotation of what went wrong
2. Write targeted Level 1 synthetic test that reproduces the failure mode
3. Fix in rustafits, verify both levels pass

### CI pipeline

| Trigger | Tests | Duration |
| ---- | ---- | ---- |
| Every commit | Level 1 + Level 2 (synthetic + hardcoded Orion) | < 30 sec |
| PR to main | + Level 3 (real FITS, PixInsight reference) | ~ 5 min |
| Nightly | + Full benchmark suite, performance regression detection | ~ 15 min |

### Development sequence

| Step | Layer | Work | Tests |
| ---- | ---- | ---- | ---- |
| 1a | rustafits | GnomonicProjection + ProperMotionCorrector | Level 1: roundtrip, known stars |
| 1b | rustafits | PatternMatcher (quad hash build + match) | Level 1: synthetic fields, noise sweep |
| 1c | rustafits | RansacFilter | Level 1: synthetic with injected outliers |
| 1d | rustafits | TransformFitter (affine, projective) | Level 1: recovered vs ground truth WCS |
| 1e | rustafits | WcsSolution (pixel↔sky, FITS headers) | Level 1: roundtrip, known WCS |
| **M1** | rustafits | **Milestone: Level 2 passes — Orion field solves** | Level 2 |
| 2a | Athenaeum | Catalog binary format writer + reader | Level 4: mini catalog |
| 2b | Athenaeum | CatalogEngine (cone search, memmap) | Level 4: search + edge cases |
| 2c | Athenaeum | PlateSolveService (orchestration) | Level 3: first real FITS solve |
| **M2** | Both | **Milestone: Level 3 passes — real images solve against PixInsight reference** | Level 3 |
| 3a | rustafits | SIP distortion in TransformFitter | Level 1: synthetic distortion recovery |
| 3b | Both | Wide-field FITS test cases | Level 3: corner residuals < 0.5 px |
| **M3** | Both | **Milestone: wide-field with distortion correction** | Level 3 |
| 4a | Athenaeum | Gaia DR3 downloader + converter | Manual + Level 4 |
| 4b | Both | Narrow-field / faint target FITS tests | Level 3 |
| 5a | rustafits | Blind solve (multi-scale) | Level 1: synthetic no-hint |
| 6a | rustafits | Thin Plate Spline in TransformFitter | Level 1: synthetic arbitrary distortion |

---

## References

- Lang et al. 2010 — "Astrometry.net: Blind Astrometric Calibration" (arXiv:0910.2233)
- Brown et al. 2017 — "TETRA: Star Identification with Hash Tables" (tetra3 algorithm)
- tetra3rs — Rust implementation (github.com/ssmichael1/tetra3rs)
- Siril plate solver — Triangle matching + SIP (gitlab.com/free-astro/siril)
- SIP Convention — Harvard/STScI distortion standard (fits.gsfc.nasa.gov/registry/sip.html)
- Gaia DR3 — ESA star catalog (cosmos.esa.int/web/gaia/dr3)
- Calabretta & Greisen 2002 — "Representations of Celestial Coordinates in FITS" (WCS standard)
- PixInsight ImageSolver — Thin plate splines + surface simplifiers for distortion modeling
