# Athenaeum Export System Documentation

## Overview

The export system prepares astrophotography data for processing in Siril or PixInsight WBPP. It handles:
- Calibration frame matching and hierarchy building
- File organization into processing-ready folder structures
- Siril script generation for automated preprocessing
- Pipeline execution with progress tracking

---

## Export Workflow (High-Level)

```
1. User selects Frame Set
       ↓
2. Data Collection (collect_export_data_v3)
   ├─ Query light frames from frame set
   ├─ Get calibration chain for each light (flat → dark → bias)
   ├─ Group into "branches" (camera × calibration × filter)
   └─ Build master creation plan (topological sort) including the steps of calibration of branches, how many files will be registered with each other (osc and mono separetely) and what masters will be exported in the end
       ↓
3. User Reviews Calibration Tree
   ├─ See lights grouped by filter/camera
   ├─ Review calibration completeness
   └─ Check warnings (missing cals, temp mismatches)
       ↓
4. Export Execution
   ├─ Step 1: Organize files into folder structure
   ├─ Step 2: Generate Siril scripts 
   └─ Step 3: Execute scripts (DirectExecution mode)
       ↓
5. Results
   ├─ Master calibration frames
   ├─ Calibrated light frames
   └─ Stacked images per filter and camera type (osc and mono separetly)
```

---

## Key Concepts

### What is a "Branch"?

A **CalibrationBranch** represents a unique path through the calibration hierarchy:

```
Branch = Camera + Bias Set + Dark Set + Flat Set + Filter

Example branches in a typical export:
- QHY268M → Bias#23 → Dark#56 → Flat#38 → Ha filter (10 lights)
- QHY268M → Bias#23 → Dark#56 → Flat#39 → OIII filter (8 lights)
- ASI2600 → Bias#45 → Dark#67 → Flat#50 → L filter (15 lights)
```

Each branch gets its own folder and calibration workflow.

### Camera Type Detection

```
OSC (One-Shot Color): Has Bayer pattern (RGGB, BGGR, etc.)
Mono: No Bayer pattern or empty BAYERPAT field
```

**Critical**: OSC and Mono frames cannot be stacked together (different layer counts). The system creates separate pipelines.

### Master Creation Plan

Masters are created in dependency order:
```
1. Bias (no dependencies)
2. Dark (optionally uses Bias)
3. DarkFlat (optionally uses Bias)
4. Flat (uses DarkFlat OR Dark OR Bias - see fallback chain)
```

---

## Calibration Hierarchy & Fallback Chains

### For Light Frames
```
Light
├─ Flat (matched by: camera, filter, gain, offset, binning)
└─ Dark (matched by: camera, exptime, gain, offset, binning, temp)
```

### For Flat Masters (Complex!)
```
Flat calibration priority:
1. DarkFlat (same exposure as flat) ← BEST
2. Dark with exposure match (±30%) ← GOOD
3. Bias only ← FALLBACK (prevents over-subtraction)
4. No calibration ← LAST RESORT
```

### For Dark Masters
```
Dark calibration:
1. Bias (for dark current optimization)
2. No calibration (acceptable - darks are stable)
```

---

## Generated Siril Scripts

### Script 1: `00_create_masters.ssf`

Creates all master calibration frames in dependency order.

```bash
# For each master in topological order:
cd biases/set_23/
convert bias
stack bias rej sigma 2.5 2.5 -nonorm -out=../masters/master_bias_23.fit

cd ../darks/set_56/
convert dark
calibrate dark -bias=../masters/master_bias_23.fit
stack pp_dark rej sigma 2.5 2.5 -nonorm -out=../masters/master_dark_56.fit

cd ../flats/set_38_ha/
convert flat
calibrate flat -dark=../masters/master_dark_56.fit  # Or -bias if no matching dark
stack pp_flat rej sigma 2.5 2.5 -norm=mul -out=../masters/master_flat_38.fit
```

### Script 2: `01_calibrate_lights.ssf`

Calibrates all light frames using created masters.

```bash
# For each branch:
cd lights/branch_01_ha/
convert lights
calibrate lights -flat=../masters/master_flat_38.fit -dark=../masters/master_dark_56.fit -cc=dark
# Produces: pp_lights_00001.fit, pp_lights_00002.fit, ...
```

**OSC-specific flags:**
- `-cfa` for cosmetic correction (preserves Bayer pattern)
- `-debayer` (unless drizzle enabled)

### Script 3: `02_register_and_stack.ssf`

Registers all lights globally, then stacks per filter.

**Dual Pipeline Architecture** (when both OSC and Mono present):
```bash
# MONO PIPELINE
cd process/all_lights_mono/
convert pp_lights -out=. -fitseq
seqplatesolve pp_lights -focal=500.0 -pixelsize=3.76
register pp_lights -2pass
convert r_pp_lights -out=. -fitseq
# Stack per filter using frame selection
unselect r_pp_lights 1 30
select r_pp_lights 1 10        # Ha frames
stack r_pp_lights rej sigma 2.5 2.5 -filter-included -norm=addscale -out=../masters/ha_mono_stacked

# OSC PIPELINE
cd process/all_lights_osc/
# Same steps but with -rgb_equal for stacking
```

---

## Folder Structure

### Siril Export Structure
```
export_root/
├── biases/
│   └── set_23/           # Bias frames for set #23
├── darks/
│   └── set_56/           # Dark frames for set #56
├── flats/
│   ├── set_38_ha/        # Flat frames for set #38, Ha filter
│   └── set_39_oiii/      # Flat frames for set #39, OIII filter
├── lights/
│   ├── branch_01_ha/     # Light frames: branch 1, Ha
│   └── branch_02_oiii/   # Light frames: branch 2, OIII
├── masters/              # Output: master_bias_23.fit, etc.
└── process/
    ├── all_lights_mono/  # Collected calibrated mono frames
    └── all_lights_osc/   # Collected calibrated OSC frames
```

### WBPP Export Structure
```
export_root/
└── camera QHY268M/
    ├── darks/            # All dark-type frames (bias, dark, darkflat)
    └── flats_38/
        ├── flat_*.fit    # Flat frames
        └── lights/
            └── light_*.fit
```

---

## Exposure Time Grouping

When enabled, frames are grouped by similar exposure times before stacking:

```
Mode: Absolute (tolerance: 30s)
Frames: 60s, 60s, 65s, 300s, 300s, 305s
Result:
  - Group 1: 60/60/65 → "60s_stacked"
  - Group 2: 300/300/305 → "300s_stacked"

Mode: Relative (tolerance: 10%)
Frames: 30s, 33s, 300s, 330s
Result:
  - Group 1: 30/33 (within 10%) → "30s_stacked"
  - Group 2: 300/330 (within 10%) → "300s_stacked"
```

---

## Edge Cases & Failure Modes

### 1. Insufficient Frames (< 2)
**Behavior**: Branch skipped in script with comment
```bash
# SKIPPED: Siril requires at least 2 frames
```
**Impact**: Incomplete results, user must add more frames

### 2. Missing Calibrations
| Missing | Behavior | Impact |
|---------|----------|--------|
| Flat | Continue with Dark/Bias only | Vignetting not corrected |
| Dark | Continue with Flat/Bias only | Hot pixels not removed |
| Bias | Continue without | Acceptable for most sensors |
| All | Uncalibrated export | Poor quality |

### 3. Mixed Camera Types (OSC + Mono)
**Behavior**: Separate pipelines created automatically
**Impact**: Correct results but more complex scripts

### 4. Flat→Dark Exposure Mismatch
**Scenario**: 2s flats, only 300s darks available
**Behavior**: Falls back to Bias calibration
**Impact**: Better than over-subtraction (bright edges)

### 5. Missing FITS Keywords
| Keyword | Fallback | Impact |
|---------|----------|--------|
| INSTRUME | "unknown" | All frames grouped together |
| FOCALLEN | 500mm | Plate solving may fail |
| BAYERPAT | Assume Mono | OSC treated as Mono (wrong!) |
| FILTER | "Unfiltered" | Generic naming |

### 6. Temperature Mismatch
**Threshold**: Typically 2°C (configurable)
**Behavior**: Warning shown, export continues
**Impact**: Potential calibration artifacts

### 7. Date Mismatch
**Behavior**: Warning shown, export continues
**Impact**: Calibrations may not match current sensor state

---

## Known Weak Points

### 1. No Quality Scoring for Reference Frame
Currently uses Siril's `-2pass` auto-selection. The `AtheneumScoring` and `Manual` modes fall back to `-2pass`.

**Planned**: Quality scoring based on FWHM, star count, background level.

### 2. Pixel Size Not Stored
Defaults to 3.76μm (common for ASI/QHY). Incorrect pixel size breaks plate solving.

**Recommendation**: Store XPIXSZ/PIXSIZE from FITS headers.

### 3. Exposure Tolerance Clustering
Tolerance is checked against **first cluster member only**, not all members.

```
Example: 30s, 33s, 36s (each 10% from previous)
Result: Single cluster (36s is 20% from 30s!)
```

**Recommendation**: Check against cluster centroid or all members.

### 4. No Validation of Calibration Links
Database links are trusted without verifying files still exist.

**Failure mode**: Deleted calibration files → script fails at runtime.

### 5. Siril Process Timeout
macOS Siril can hang after "closing pipes". Current workaround: 30-second timeout.

**Impact**: May incorrectly report failures for long-running scripts.

### 6. No Resume/Retry Logic
If script fails mid-execution, must restart from beginning.

**Recommendation**: Track completed steps, allow resuming.

### 7. Frame Index Assumptions
Assumes Siril assigns sequence indices in filename sort order. If pp_lights files have gaps or unusual naming, frame selection may be wrong.

---

## Configuration Options

### Export Modes
- `generate_scripts` - Scripts only, no file organization
- `organize_files` - File organization only
- `organize_and_script` - Both (default)
- `direct_execution` - Full pipeline with Siril execution

### Siril Options
| Option | Values | Default |
|--------|--------|---------|
| Rejection Algorithm | sigma, percentile, linear_fit, gesd, mad | sigma |
| Rejection Thresholds | 0.0 - 10.0 | 2.5 / 2.5 |
| Image Weighting | none, wfwhm, stars, noise, exptime | wfwhm |
| Reference Frame | siril_auto, athenaeum_scoring, manual | siril_auto |
| Drizzle | disabled, x2, x3 | disabled |

### Exposure Time Grouping
| Option | Description |
|--------|-------------|
| disabled | Stack all same-filter frames together |
| absolute | Group frames within N seconds |
| relative | Group frames within N percent |

---

## Database Tables Involved

- `frames_set` - Frame set metadata
- `imaging_nights` - Nights within frame set
- `sessions` - Camera sessions within nights
- `session_members` - Frame membership in sessions
- `frames` - Individual frame metadata
- `files` - File paths
- `calibration_set` - Calibration set definitions
- `calibration_set_frames` - Frames in calibration sets
- `calibration_set_to_frames` - Calibration links (frame→set, set→set)

---

## Key Files

| File | Purpose |
|------|---------|
| `commands/export.rs` | Tauri commands (entry points) |
| `export/data_collector.rs` | Data collection & hierarchy building |
| `export/models.rs` | Data structures |
| `export/file_organizer.rs` | File organization |
| `export/folder_structures.rs` | Folder path utilities |
| `export/siril/script_generator.rs` | Siril script generation |
| `export/siril/cli_runner.rs` | Siril execution & progress |
| `calibration/configurable_matcher.rs` | Calibration matching logic |
| `calibration/hierarchy.rs` | Hierarchy building |
