# Requirements: Plate Solving for rustafits

## Purpose

A self-contained plate solving engine in rustafits — no external solver dependencies. Designed to serve both standalone plate solving and future star-matched registration/mosaic stitching in Athenaeum.

## What Plate Solving Does

Given an astronomical image with detected stars (pixel x, y, flux), determine:
1. Where in the sky the image points (RA, Dec of center)
2. The image orientation (rotation angle)
3. The precise pixel-to-sky coordinate mapping (WCS solution)
4. Optical distortion model (SIP polynomial coefficients)

## Architecture

```
StarDetector (already exists in rustafits)
  └── x, y, flux, FWHM per star

PatternMatcher (new core engine)
  ├── Quad descriptor: 4 brightest stars → 5 normalized distance ratios
  ├── Hash table: O(1) lookup of matching catalog patterns
  ├── RANSAC: outlier rejection for false matches
  └── Iterative refinement: predictor-corrector loop (PixInsight approach)

CatalogEngine (new)
  ├── Binary format: HEALpix-indexed star records (ra, dec, mag, proper motion)
  ├── Cone search: load only stars within search radius
  ├── Epoch propagation: correct proper motion to observation date
  └── Shipped catalog: Gaia DR3 subset (mag < 14-16), ~1-4 GB

TransformFitter (new, pluggable)
  ├── Affine (6 params) — basic
  ├── Projective/Homography (8 params) — perspective correction
  ├── SIP Polynomial (N params) — optical distortion modeling
  └── Thin Plate Spline — arbitrary local distortion (for future registration)

WCS Output
  ├── CRPIX1/2 — reference pixel
  ├── CRVAL1/2 — reference sky coordinate (RA, Dec)
  ├── CD matrix (2×2) — rotation + scale
  ├── SIP A/B coefficients — forward distortion
  ├── SIP AP/BP coefficients — reverse distortion
  └── Solve metadata: matched star count, RMS residual, solve time
```

## Algorithm: Quad Hash Matching (tetra3 approach)

### Why quads over triangles
- Triangle: 2 invariant ratios → high false match rate
- Quad: 5 invariant ratios → dramatically fewer false matches
- Hash table: O(1) lookup vs O(N²) brute force comparison
- Proven in tetra3 (ESA), astrometry.net, ASTAP

### Pattern encoding
1. Select 4 stars from detected stars (brightest first, breadth-first search)
2. Compute all 6 pairwise distances
3. Normalize by the longest distance → 5 ratios in [0, 1]
4. Sort ratios for canonical ordering (flip/rotation invariant)
5. Quantize into hash table key with configurable tolerance

### Matching procedure
1. Extract N brightest stars from image (typically 50-200)
2. Generate candidate quads from image stars
3. For each quad: compute hash → lookup in catalog hash table
4. Each match produces a candidate transform (4 star pairs → projective)
5. Apply candidate transform to all image stars → count catalog matches
6. RANSAC: reject outlier pairs, re-fit with inliers only
7. Iterate: apply refined transform → find more matches → re-fit
8. Accept solution when matched star count and RMS pass thresholds

### Hint-based solving (primary mode for Athenaeum)
- Athenaeum already has approximate RA/Dec, focal length, pixel size from FITS headers
- Cone search: load only catalog stars within ~2× FOV of hint position
- Scale hint: reject quads whose scale differs >20% from expected
- Reduces search space by orders of magnitude → solve in <1 second

### Blind solving (fallback)
- No hints needed — searches broader sky area
- Slower (seconds to tens of seconds) but still feasible
- Uses multi-scale index: try coarse scale first, refine

## Star Catalog Strategy

### Licensing constraints

Gaia DR3 is licensed **CC BY-NC 3.0 IGO** — free for non-commercial use with attribution, but commercial redistribution is restricted. PixInsight (commercial) works around this by having users download Gaia catalogs separately rather than shipping them. We follow the same approach.

### Three-tier catalog strategy

**Tier 1: Bundled — Tycho-2 (ships with rustafits)**

| Property | Value |
| ---- | ---- |
| Source | Tycho-2 (ESA, public domain) |
| License | Public domain — free for any use including commercial |
| Stars | ~2.5 million |
| Magnitude limit | V < 11.5 |
| Size | ~50-100 MB (binary HEALpix format) |
| Epoch | J2000.0 (proper motions included) |
| Use case | Works out of the box for most wide-field amateur imaging |

**Tier 2: User-downloaded — Gaia DR3 subset**

| Property | Value |
| ---- | ---- |
| Source | Gaia DR3 (ESA) |
| License | CC BY-NC 3.0 IGO — user downloads directly, we never redistribute |
| Stars | ~470 million (mag < 16) |
| Size | ~2-4 GB (binary HEALpix format after conversion) |
| Epoch | J2016.0 (proper motions included) |
| Use case | Narrow-field, faint targets, deep imaging, high-precision astrometry |

Built-in catalog downloader in Athenaeum settings fetches from ESA Gaia archive, converts to rustafits binary format locally. This sidesteps the redistribution question — the user obtains the data themselves.

**Tier 3: Online fallback — VizieR/Gaia TAP**

| Property | Value |
| ---- | ---- |
| Source | VizieR or ESA Gaia TAP service |
| License | N/A (querying, not redistributing) |
| Use case | No local catalog installed, occasional use, first-time setup |
| Limitation | Requires internet, slower, rate-limited |

### Catalog format (all tiers use the same binary format)

- HEALpix-indexed binary files (Level 6 = 49,152 sky pixels, ~0.84° each)
- Fixed-size records: ra (f64), dec (f64), mag (f32), pmra (f32), pmdec (f32) = 24 bytes/star
- Stars sorted by magnitude within each pixel for early termination
- Memory-mappable via `memmap2` for fast access
- Cone search: identify overlapping HEALpix pixels → load only those files
- Epoch propagation: linear proper motion correction to observation date

### Catalog selection in CatalogEngine

```rust
pub enum CatalogSource {
    Local(PathBuf),    // Tycho-2 or downloaded Gaia
    Online {           // VizieR/TAP fallback
        service: OnlineService,
        cache_dir: PathBuf,
    },
}
```

The solver tries: local Gaia → local Tycho-2 → online fallback. Athenaeum UI shows catalog status and offers download.

## WCS Fitting

### After matching N star pairs (pixel ↔ catalog):

**Step 1: Gnomonic (TAN) projection**
- Project catalog RA/Dec onto tangent plane at field center
- Standard coordinates (ξ, η) in radians

**Step 2: Fit CD matrix (linear least squares)**
- 6 unknowns: CRPIX1, CRPIX2 (or fix at image center), CD1_1, CD1_2, CD2_1, CD2_2
- Solve normal equations: x = (AᵀA)⁻¹Aᵀb
- Use `nalgebra` crate for matrix operations

**Step 3: Fit SIP distortion (if N > ~20 matched stars)**
- Order 2-5 polynomial (typically 3 for amateur telescopes)
- Additional least-squares fit for A_i_j, B_i_j coefficients
- Iterative: fit linear first, then add distortion terms progressively

**Step 4: Quality assessment**
- RMS residual: aim for < 1.0 pixel (< 0.5 arcsec for typical setups)
- Per-star residual inspection for remaining outliers
- Report: matched stars, RMS, solve time, distortion order

## API Design (rustafits public interface)

```rust
// Catalog
pub struct StarCatalog { /* HEALpix-indexed binary catalog */ }
impl StarCatalog {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn cone_search(&self, ra: f64, dec: f64, radius_deg: f64,
                       mag_limit: f32, epoch: f64) -> Vec<CatalogStar>;
}

// Solver
pub struct PlateSolver { /* holds catalog + config */ }
pub struct SolveHints {
    pub ra: Option<f64>,        // approximate RA (degrees)
    pub dec: Option<f64>,       // approximate Dec (degrees)
    pub fov_deg: Option<f64>,   // field of view (degrees)
    pub rotation: Option<f64>,  // approximate rotation (degrees)
}
pub struct SolveResult {
    pub wcs: WcsSolution,
    pub matched_stars: usize,
    pub rms_residual_px: f64,
    pub rms_residual_arcsec: f64,
    pub solve_time_ms: u64,
}
pub struct WcsSolution {
    pub crpix: (f64, f64),
    pub crval: (f64, f64),      // RA, Dec in degrees
    pub cd: [[f64; 2]; 2],      // CD matrix
    pub sip_a: Option<Vec<Vec<f64>>>,  // forward distortion
    pub sip_b: Option<Vec<Vec<f64>>>,
    pub sip_ap: Option<Vec<Vec<f64>>>, // reverse distortion
    pub sip_bp: Option<Vec<Vec<f64>>>,
}

impl PlateSolver {
    pub fn new(catalog: StarCatalog, config: SolveConfig) -> Self;
    pub fn solve(&self, stars: &[DetectedStar],
                 image_size: (u32, u32),
                 hints: &SolveHints) -> Result<SolveResult>;
}

impl WcsSolution {
    pub fn pixel_to_sky(&self, x: f64, y: f64) -> (f64, f64);  // → (RA, Dec)
    pub fn sky_to_pixel(&self, ra: f64, dec: f64) -> (f64, f64);  // → (x, y)
    pub fn to_fits_headers(&self) -> Vec<(String, String)>;
}
```

## Future: Registration & Mosaics (built on this foundation)

### Same-field registration
- Use PatternMatcher directly (image stars → reference image stars)
- No catalog needed — reference frame IS the catalog
- Output: pixel-to-pixel transform (affine/projective/TPS)

### Mosaic panel stitching
- Plate solve each panel independently → each gets WCS
- Reproject all panels to common sky grid via WCS
- Blend overlap regions (feathering / gradient domain)
- The WCS quality from plate solving directly determines mosaic accuracy

## Dependencies (Rust crates)

| Crate | Purpose |
| ---- | ---- |
| `nalgebra` | Linear algebra, matrix operations, least squares |
| `healpix` (or custom) | HEALpix pixel indexing for catalog |
| `memmap2` | Memory-mapped catalog files |
| `rayon` | Parallel quad candidate generation |
| `byteorder` | Binary catalog reading |

## Build Phases

| Phase | Deliverable | Enables |
| ---- | ---- | ---- |
| 1 | PatternMatcher + RANSAC + affine/projective fitter | Star matching engine |
| 2 | CatalogEngine (HEALpix binary Gaia subset) | Catalog queries |
| 3 | PlateSolver (hint-based) + WCS output | Plate solving in Athenaeum |
| 4 | SIP distortion fitting | Wide-field accuracy |
| 5 | Blind solving (multi-scale search) | No-hint fallback |
| 6 | Thin Plate Spline transform | Registration distortion |
| 7 | WCS reprojection + resampling | Mosaic stitching |

## References

- Lang et al. 2010 — "Astrometry.net: Blind Astrometric Calibration" (arXiv:0910.2233)
- Brown et al. 2017 — "TETRA: Star Identification with Hash Tables" (tetra3 algorithm)
- tetra3rs — Rust implementation (github.com/ssmichael1/tetra3rs)
- Siril plate solver — Triangle matching + SIP (gitlab.com/free-astro/siril)
- SIP Convention — Harvard/STScI distortion standard
- Gaia DR3 — ESA star catalog (cosmos.esa.int/web/gaia/dr3)
