# Calibration & Stacking Reference

Canonical reference for the astrophotography calibration pipeline as implemented in Athenaeum. Covers frame types, calibration chains, parameter matching, WBPP export semantics, and frame set clustering.

---

## 1. Frame Types

Every astronomical image is built from five core frame types. Each isolates a different source of noise or optical artifact so it can be subtracted from the final image.

| Type | What It Captures | When It's Taken | Why It's Needed |
| ---- | ---- | ---- | ---- |
| **Light** | Photons from the target object + sky background + sensor noise | During the imaging session, tracking the target | The actual data; everything else exists to clean these |
| **Dark** | Thermal noise (dark current) at a specific exposure time and temperature | With cap on, same exposure/gain/temp as lights | Subtracting a dark removes fixed-pattern thermal noise |
| **Flat** | Optical vignetting, dust shadows, and illumination gradients | Against a uniform light source (twilight sky, flat panel) | Dividing by a flat corrects per-pixel sensitivity differences |
| **Bias** | Read noise floor (zero-length exposure electronic pattern) | With cap on, shortest possible exposure | Subtracting bias isolates the pure read noise baseline |
| **DarkFlat** | Thermal noise at the flat's exposure time | With cap on, same exposure/gain/temp as flats | Like a dark but matched to the flat's short exposure |

**Master variants** (MasterDark, MasterFlat, MasterBias, MasterDarkFlat) are the median/sigma-clipped combination of many individual frames. A master has better signal-to-noise than any single calibration frame.

### IMAGETYP String Parsing

Athenaeum recognizes these case-insensitive FITS header values:

| Raw Value(s) | Mapped Type |
| ---- | ---- |
| `LIGHT`, `LIGHT FRAME` | Light |
| `DARK`, `DARK FRAME` | Dark |
| `FLAT`, `FLAT FIELD`, `FLAT FRAME`, `FLATFIELD`, `SKY FLAT`, `DOME FLAT`, `TWILIGHT FLAT` | Flat |
| `BIAS`, `BIAS FRAME`, `OFFSET`, `OFFSET FRAME` | Bias |
| `DARKFLAT`, `DARK FLAT`, `DARK FLAT FRAME`, `FLATDARK`, `FLAT DARK`, `FLAT DARK FRAME` | DarkFlat |

Master variants follow the same patterns prefixed with `MASTER` (e.g., `MASTER DARK`, `MASTER FLAT FRAME`).

---

## 2. Calibration Chains & Dependencies

Calibration frames form a directed dependency graph. Each arrow means "is calibrated by":

```
                    ┌──── Flat ◄──── DarkFlat ◄──── Bias
                    │       ▲              ▲
                    │       │              │
Light ◄─────────────┤       └── Dark ◄─────┘
                    │              ▲
                    ├──── Dark ◄───┤
                    │              │
                    └──── Bias     └──── Bias
```

### Dependency Rules

| Source | Needs | Notes |
| ---- | ---- | ---- |
| **Lights** | Flat, Dark, Bias | All three are independent calibrations of the light frame |
| **Flats** | DarkFlat or Dark or Bias | Fallback chain: try DarkFlat first, then Dark, then Bias |
| **Darks** | Bias (optional) | Only when "Bias for Dark Optimization" is enabled |

### Flat Fallback Chain

When calibrating flats, Athenaeum tries calibration types in order until one matches:

1. **DarkFlat** -- ideal because it matches the flat's short exposure time
2. **Dark** -- acceptable if exposure is within 30% tolerance of flat exposure
3. **Bias** -- last resort; removes read noise but not dark current

The fallback chain respects the `use_bias_if_no_darks` behavioral option (default: `true` for flats).

### Master Creation Order

Masters must be created in topological dependency order so each master can use the previous ones:

1. **Bias** (no dependencies)
2. **Dark** (calibrated with master bias)
3. **DarkFlat** (calibrated with master bias)
4. **Flat** (calibrated with master dark/darkflat or bias)

---

## 3. Why Parameters Must Match

Each matchable parameter controls a physical property of the imaging system. Mismatches introduce artifacts rather than removing them.

### instrume (Camera/Instrument)

**What it controls**: The specific sensor -- pixel size, read noise characteristics, quantum efficiency curve, well depth.

**Why mismatch matters**: Every sensor has a unique fixed-pattern noise signature. Calibration frames from camera A will add noise to images from camera B rather than subtracting it.

**Default mode**: Exact for all pairs.

### binning (e.g., "1x1", "2x2")

**What it controls**: How many physical pixels are combined into one logical pixel during readout.

**Why mismatch matters**: Binning changes the effective pixel geometry, noise characteristics, and spatial scale. A 2x2-binned dark has 4x the dark current per logical pixel compared to 1x1.

**Default mode**: Exact for all pairs.

### gain

**What it controls**: The analog amplification applied to the sensor signal before digitization (electrons per ADU).

**Why mismatch matters**: Higher gain amplifies both signal and noise differently. A dark frame at gain 100 has a different noise floor than one at gain 200; subtracting the wrong gain dark leaves residual structure.

**Default mode**: Exact for all pairs. Float tolerance: 0.01.

### offset

**What it controls**: A DC bias voltage added to the signal to prevent negative ADU values from clipping.

**Why mismatch matters**: Offset shifts the entire histogram baseline. Calibration with the wrong offset will shift the background level up or down across the image.

**Default mode**: Exact for all pairs. Float tolerance: 0.01.

### exptime (Exposure Time)

**What it controls**: How long the shutter stays open, directly scaling both signal and thermal noise accumulation.

**Why mismatch matters**: Dark current accumulates linearly with time. A 300s dark cannot properly calibrate a 600s light -- it will only remove half the thermal noise, leaving a warm glow.

**Default mode**: Warning for Dark pairs (warning: 1.0s, reject: 5.0s). Ignore for Bias/Flat pairs. Float tolerance: 0.1s.

### focallen (Focal Length)

**What it controls**: The optical system's focal length, which determines image scale (arcseconds per pixel) and the illumination cone reaching the sensor.

**Why mismatch matters**: Primarily affects flats -- vignetting patterns depend on the optical cone geometry. A flat taken at 400mm has a different illumination falloff than one at 600mm.

**Default mode**: Warning for Lights->Flat (warning: 5.0mm, reject: 10.0mm). Ignore for all other pairs. Float tolerance: 1.0mm.

### filter

**What it controls**: Which wavelengths of light reach the sensor (Ha, OIII, SII, L, R, G, B, etc.).

**Why mismatch matters**: Each filter has different transmission, thickness, and tilt. Flats must match filters because the dust shadow positions and vignetting profile change with each filter. Darks and biases are sensor-only measurements -- no light enters -- so the filter is irrelevant.

**Default mode**: Exact for Lights->Flat only. Ignore for all other pairs.

### ccd_temp (CCD Temperature)

**What it controls**: The sensor operating temperature, set by the cooler.

**Why mismatch matters**: Dark current doubles approximately every 6-8 degrees Celsius. A dark taken at -10C has roughly 4x less thermal noise than one at -4C. Subtracting the wrong-temperature dark leaves thermal residuals.

**Default mode**: Warning for Dark and Bias pairs (warning: 2.0C, reject: 5.0C). Ignore for Lights->Flat. Float tolerance: 2.0C.

### telescop (Telescope Name)

**What it controls**: Identifies the telescope/optical tube assembly.

**Why mismatch matters**: Different telescopes have different focal ratios, vignetting, and optical characteristics. However, this is often redundant with focallen+instrume, so it's ignored by default.

**Default mode**: Ignore for all pairs.

---

## 4. Default Matching Rules Matrix

The following tables show the default MatchMode for each parameter per source-to-calibration pair. All `required` parameters (marked with `*`) will skip the entire calibration type if the frame's value is NULL.

### Lights -> Flat

| Parameter | Mode | Warning Threshold | Reject Threshold |
| ---- | ---- | ---- | ---- |
| instrume* | Exact | -- | -- |
| binning* | Exact | -- | -- |
| gain* | Exact | -- | -- |
| offset* | Exact | -- | -- |
| filter* | Exact | -- | -- |
| focallen | Warning | 5.0 mm | 10.0 mm |
| exptime | Ignore | -- | -- |
| ccd_temp | Ignore | -- | -- |
| telescop | Ignore | -- | -- |

### Lights -> Dark

| Parameter | Mode | Warning Threshold | Reject Threshold |
| ---- | ---- | ---- | ---- |
| instrume* | Exact | -- | -- |
| binning* | Exact | -- | -- |
| gain* | Exact | -- | -- |
| offset* | Exact | -- | -- |
| exptime | Warning | 1.0 s | 5.0 s |
| ccd_temp | Warning | 2.0 C | 5.0 C |
| filter | Ignore | -- | -- |
| focallen | Ignore | -- | -- |
| telescop | Ignore | -- | -- |

### Lights -> Bias

| Parameter | Mode | Warning Threshold | Reject Threshold |
| ---- | ---- | ---- | ---- |
| instrume* | Exact | -- | -- |
| binning* | Exact | -- | -- |
| gain* | Exact | -- | -- |
| offset* | Exact | -- | -- |
| ccd_temp | Warning | 2.0 C | 5.0 C |
| exptime | Ignore | -- | -- |
| focallen | Ignore | -- | -- |
| filter | Ignore | -- | -- |
| telescop | Ignore | -- | -- |

### Flats -> DarkFlat / Flats -> Dark

| Parameter | Mode | Warning Threshold | Reject Threshold |
| ---- | ---- | ---- | ---- |
| instrume* | Exact | -- | -- |
| binning* | Exact | -- | -- |
| gain* | Exact | -- | -- |
| offset* | Exact | -- | -- |
| exptime | Warning | 1.0 s | 5.0 s |
| ccd_temp | Warning | 2.0 C | 5.0 C |
| filter | Ignore | -- | -- |
| focallen | Ignore | -- | -- |
| telescop | Ignore | -- | -- |

### Flats -> Bias / Darks -> Bias

| Parameter | Mode | Warning Threshold | Reject Threshold |
| ---- | ---- | ---- | ---- |
| instrume* | Exact | -- | -- |
| binning* | Exact | -- | -- |
| gain* | Exact | -- | -- |
| offset* | Exact | -- | -- |
| ccd_temp | Warning | 2.0 C | 5.0 C |
| exptime | Ignore | -- | -- |
| focallen | Ignore | -- | -- |
| filter | Ignore | -- | -- |
| telescop | Ignore | -- | -- |

### Match Modes Explained

| Mode | Behavior |
| ---- | ---- |
| **Exact** | Values must be identical (with float tolerance where applicable). Mismatch rejects the candidate. |
| **Warning** | Uses dual-threshold logic (see below). May accept with warning or reject. |
| **Ignore** | Parameter is not checked. Always passes. |

### Dual-Threshold Logic (Warning Mode)

When a parameter is set to Warning mode with both thresholds configured:

```
diff = |frame_value - calibration_value|

if diff > matching_threshold  ->  REJECT (candidate excluded)
if diff > warning_threshold   ->  ACCEPT with WARNING displayed
if diff <= warning_threshold  ->  ACCEPT clean (no warning)
```

---

## 5. WBPP Folder Hierarchy

PixInsight's Weighted Batch Pre-Processing (WBPP) reads a folder tree where **parent directories calibrate their children**. Athenaeum exports files into this nested structure so WBPP can automatically associate calibration frames.

### Full Folder Tree

```
{object_name}/
  camera_{instrume}/                           ← Camera grouping
    {filter}_{camera_type}/                    ← Filter + OSC/Mono grouping
      BIAS_{set_id}/                           ← Bias frames (outermost calibration)
        bias_001.fits
        bias_002.fits
        ...
        DARKS_{set_id}/                        ← Dark frames (calibrated by parent bias)
          dark_001.fits
          dark_002.fits
          ...
          FLAT_{set_id}/                       ← Flat frames (calibrated by parent dark)
            flat_001.fits
            flat_002.fits
            ...
            lights/                            ← Light frames (calibrated by all parents)
              light_001.fits
              light_002.fits
              ...
```

### Missing Calibration Levels

When a calibration type is unavailable, its folder level is simply omitted and the hierarchy collapses. For example, if no darks are available:

```
BIAS_{set_id}/
  FLAT_{set_id}/                  ← Flat becomes direct child of bias
    lights/
```

Or if only lights and flats are available:

```
FLAT_{set_id}/
  lights/
```

WBPP handles these collapsed hierarchies correctly -- it only applies calibration from folders that exist as ancestors.

### DarkFlat Placement

DarkFlat frames are placed alongside regular dark frames in the `DARKS_` folder. WBPP uses IMAGETYP headers to distinguish them from darks.

### Folder Name Sanitization

Athenaeum sanitizes folder names differently depending on context:

- **Technical names** (instrument, camera): lowercase, alphanumeric only. Example: `ZWO 2600MM Pro` becomes `zwo2600mmpro`
- **Display names** (object, filter): preserve letters, digits, spaces, hyphens, underscores, dots, parentheses. Special characters (`: / \ * ? " < > |`) become underscores. Consecutive underscores collapse.

### Frame Deduplication

Each calibration set is exported only once, even if it's shared by multiple subgroups. Athenaeum tracks exported set IDs in a `HashSet<i64>` and skips duplicates.

---

## 6. Camera Type: OSC vs Mono

### Bayer Pattern Detection

Athenaeum determines camera type from the `BAYERPAT` FITS header:

```rust
CameraType = if BAYERPAT is present and non-empty -> OSC
             else                                  -> Mono
```

Common Bayer patterns: `RGGB`, `BGGR`, `GRBG`, `GBRG`.

### Why OSC and Mono Must Be Separated

OSC (One-Shot Color) cameras have a Bayer filter matrix baked onto the sensor. Each pixel sees only one color. Mono cameras have no filter matrix -- every pixel sees all wavelengths passed by the external filter.

These two sensor types require fundamentally different debayering and stacking algorithms. Mixing them in the same stack produces color artifacts and incorrect noise statistics. WBPP needs to know the camera type to select the right integration pipeline.

### Export Grouping

Export groups are keyed by `{filter}_{camera_type}`. This means:

- Ha frames from an OSC camera and Ha frames from a Mono camera are separate groups
- Luminance (no filter) from a Mono camera is its own group
- Unfiltered OSC frames are grouped as `None_osc`

---

## 7. Temperature & Exposure Time

### Temperature: Physics of Thermal Noise

Semiconductor sensors generate thermally-excited electrons (dark current) that are indistinguishable from photon-generated electrons. The rate approximately doubles every 6-8 degrees Celsius, following the Arrhenius equation.

**Practical implications**:
- A dark frame at -10C removes thermal noise accumulated at -10C
- Using that dark on a light taken at -5C leaves residual thermal noise (the light has ~2x more dark current)
- Cooled cameras hold temperature to within ~0.1C, so darks from different nights at the same setpoint are interchangeable

### Temperature Matching Thresholds

| Threshold | Default | Meaning |
| ---- | ---- | ---- |
| Warning | 2.0 C | Diff > 2C shows a warning but still matches |
| Reject | 5.0 C | Diff > 5C excludes the candidate entirely |

These defaults apply to all dark and bias pairs (Lights->Dark, Lights->Bias, Flats->DarkFlat, Flats->Dark, Flats->Bias, Darks->Bias).

### Exposure Time: Linear Noise Scaling

Dark current accumulates linearly with exposure time. A 600s dark has exactly twice the thermal noise of a 300s dark (at the same temperature).

**Practical implications**:
- Darks must match the light's exposure time to properly subtract the accumulated thermal noise
- Bias frames are effectively zero-length exposures, so exposure matching is irrelevant
- Flat exposures are typically very short (1-10s); their dark calibration needs matching via the DarkFlat path

### Exposure Time Matching Thresholds

| Threshold | Default | Meaning |
| ---- | ---- | ---- |
| Warning | 1.0 s | Diff > 1s shows a warning |
| Reject | 5.0 s | Diff > 5s excludes the candidate |

### Flat-Dark Exposure Tolerance

When matching darks to flats during master creation, a separate tolerance is used:

```
FLAT_DARK_EXPOSURE_TOLERANCE = 0.30 (30%)

Match if |flat_exptime - dark_exptime| / max(flat_exptime, dark_exptime) <= 0.30
```

This looser tolerance accounts for the fact that flat exposures are short and thermal noise contribution is minimal.

---

## 8. Flat Matching Strategies

Athenaeum supports three strategies for matching flats to lights:

### Automatic (Default)

Finds the flat group taken **nearest in time** to the light frames. This is the safest default because dust positions and optical alignment drift slowly over time.

**Scoring formula**:

```
date_score  = 1.0 - min(age_days / max_age_days, 1.0)
temp_score  = 1.0 - min(temp_diff / 10.0, 1.0)
match_score = date_score * (1.0 - temp_weight) + temp_score * temp_weight
```

Where `temp_weight` is the `temperature_match_weight` from scoring config (default 0.3).

### Long-Term

Prefers the **oldest valid** flat group. Useful when your optical system is very stable (e.g., permanently mounted observatory) and you've verified that a single flat set remains accurate over time.

### Manual

User explicitly selects a specific flat calibration set. Used when automatic matching makes a suboptimal choice, or for special workflows like shared master flats.

### Flat Timing

Athenaeum tracks when flats were taken relative to the imaging session:

| Timing | Meaning |
| ---- | ---- |
| Before | Flat group taken before the light session |
| After | Flat group taken after the light session |
| During | Flat group taken during the session |

---

## 9. Calibration Scoring

When multiple calibration candidates match, Athenaeum ranks them by a composite score.

### Score Components

```
score = 1.0

// Recency: prefer calibration taken closer in time
date_score = 1.0 / (1.0 + days_apart / 30.0)
score *= date_score

// Temperature closeness (weighted)
temp_raw = 1.0 / (1.0 + temp_diff / temperature_scale)
temp_score = lerp(1.0, temp_raw, temperature_match_weight)
score *= temp_score

// Exposure closeness (weighted)
exp_raw = 1.0 / (1.0 + exp_diff / exposure_scale)
exp_score = lerp(1.0, exp_raw, exposure_match_weight)
score *= exp_score

final_score = clamp(score, 0.0, 1.0)
```

### Default Scoring Configuration

| Parameter | Default | Range |
| ---- | ---- | ---- |
| `temperature_match_weight` | 0.3 | 0.0 - 1.0 |
| `temperature_scale` | 2.0 | -- |
| `exposure_match_weight` | 0.4 | 0.0 - 1.0 |
| `exposure_scale` | 1.0 | -- |

Higher weight means the parameter has more influence on the final score. A weight of 0.0 means the parameter is ignored in scoring.

---

## 10. Calibration Clustering

Calibration sets are grouped by configurable time and temperature windows. These defaults control how frames are clustered into calibration sets during scanning.

| Calibration Type | Max Age (days) | Time Cluster (minutes) | Temp Threshold |
| ---- | ---- | ---- | ---- |
| Flat | 30 | 30 | 2.0 C |
| Dark | 365 | 43,200 (30 days) | 2.0 C |
| Bias | 365 | 43,200 (30 days) | 2.0 C |
| DarkFlat | 365 | 43,200 (30 days) | 2.0 C |

**Max Age**: Calibration older than this is not considered a candidate.

**Time Cluster**: Frames taken within this window are grouped into a single calibration set. Darks and biases use 30 days because they're stable over time at the same temperature.

**Temp Threshold**: Frames with temperature differences beyond this are placed in separate sets.

### Date Warning Thresholds

These thresholds trigger warnings about calibration age in the UI:

| Calibration Type | Warning After |
| ---- | ---- |
| Flat | 30 days |
| Dark | 365 days |
| DarkFlat | 365 days |

---

## 11. Frame Set Clustering (DBSCAN)

Athenaeum automatically groups LIGHT frames into frame sets by sky coordinates using a seed-and-grow clustering algorithm based on DBSCAN principles.

### Algorithm

1. **Normalize coordinates**: Parse RA/Dec from FITS headers (decimal degrees, HMS/DMS, or colon-separated) into decimal degrees
2. **Sort deterministically**: Order frames by RA, then Dec, then DATE-OBS for reproducible clustering
3. **Seed-and-grow**: For each unassigned frame:
   a. Create a new cluster with this frame as the seed
   b. Iteratively add all unassigned frames within the angular threshold
   c. Recompute the cluster center as a **spherical mean** after each addition
   d. Repeat until no more frames can be added
4. **Repeat** until all frames are assigned to a cluster

### Spherical Mean (RA Wraparound)

Simple arithmetic averaging fails for RA near 0/360 degrees (e.g., averaging 359 and 1 gives 180, not 0). Athenaeum solves this by:

1. Converting each (RA, Dec) to a 3D unit vector on the celestial sphere
2. Computing the mean of all unit vectors
3. Converting the mean vector back to (RA, Dec)

This correctly handles the wraparound at RA = 0/360.

### Cluster Naming

- If any single OBJECT header value accounts for >50% of frames in a cluster, that name is used
- Otherwise, the cluster is named by formatted coordinates: `Unknown @ RA=HHhMMmSS.Ss, Dec=+/-DDdMMmSS.Ss`

### Configurable Threshold

The clustering radius is controlled by the `grouping_threshold_arcmin` setting (default: 15 arcminutes). Frames within this angular distance of the cluster center are added to the cluster.

### Coordinate Parsing

Athenaeum supports three RA/Dec formats from FITS headers:

| Format | RA Example | Dec Example |
| ---- | ---- | ---- |
| Decimal degrees | `123.456` | `-45.678` |
| HMS/DMS strings | `12h34m56.7s` | `-45d40m30s` |
| Colon-separated | `12:34:56.7` | `-45:40:30` |

All values are normalized to decimal degrees: RA in [0, 360), Dec in [-90, 90].

---

## 12. Configuration Persistence

### Storage

Calibration matching configuration is stored as a single JSON blob in the `settings` table under key `calibration.matching_config`.

### Config Version

Current version: **3**

Version history:
- **v2**: Added telescop field, dual thresholds (warning + matching), locked/supports_warning flags
- **v3**: Unlocked instrume/binning/gain/offset (can now be set to Ignore by user)

### Master Preferences

Controls whether master frames or regular frame sets are preferred when both are available. Masters are always candidates for auto-link — this setting only orders the result list, it never filters:

| Option | Meaning |
| ---- | ---- |
| PreferMaster | Return master calibration sets first (default for all types) |
| PreferFrameset | Return regular frame sets first |
| NoPreference | No sorting preference — score order only |

### Behavioral Options

| Option | Default | Applies To | Effect |
| ---- | ---- | ---- | ---- |
| `use_bias_for_dark_optimization` | true | Lights, Flats, Darks | Enables Bias calibration for dark frames |
| `use_bias_if_no_darks` | true | Flats | Falls back to Bias if no Dark/DarkFlat found |

---

## Appendix: Key Source Files

| File | Responsibility |
| ---- | ---- |
| `src-tauri/src/calibration/config.rs` | Configuration structs, defaults, parameter definitions |
| `src-tauri/src/calibration/configurable_matcher.rs` | Matching engine, dual thresholds, scoring |
| `src-tauri/src/calibration/hierarchy.rs` | Calibration chain builder |
| `src-tauri/src/calibration/flat_matcher.rs` | Flat matching strategies, temporal scoring |
| `src-tauri/src/export/file_organizer.rs` | WBPP folder nesting logic |
| `src-tauri/src/export/models.rs` | ExportGroup, CameraType, MasterCreationPlan |
| `src-tauri/src/export/data_collector.rs` | Data collection, folder preview, master ordering |
| `src-tauri/src/clustering/mod.rs` | DBSCAN clustering, spherical mean, angular distance |
| `src-tauri/src/models.rs` | Frame struct, IMAGETYP classification |
| `src-tauri/src/coordinates/` | RA/Dec parsing and conversion |
