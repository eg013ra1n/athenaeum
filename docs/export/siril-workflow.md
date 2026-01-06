# Siril Export Workflow

This document describes how Athenaeum generates Siril scripts for astrophotography image processing.

## Table of Contents

1. [Overview](#overview)
2. [Script Architecture](#script-architecture)
3. [Master Bias Creation](#master-bias-creation)
4. [Master Dark Creation](#master-dark-creation)
5. [Master DarkFlat Creation](#master-darkflat-creation)
6. [Master Flat Creation](#master-flat-creation)
7. [Light Calibration](#light-calibration)
8. [Registration](#registration)
9. [Stacking](#stacking)
10. [Configuration Reference](#configuration-reference)
11. [Complete Examples](#complete-examples)

---

## Overview

Athenaeum generates a 3-script workflow for Siril processing:

| Script | Purpose |
|--------|---------|
| `00_create_masters.ssf` | Create master calibration frames (bias, dark, flat) |
| `01_calibrate_lights.ssf` | Apply calibration to light frames |
| `02_register_and_stack.ssf` | Register and stack calibrated lights |

### Calibration Formula

Siril applies the standard calibration formula:

```
Calibrated_Light = (Light - Dark) / (Flat - Offset)
```

Where:
- **Dark** removes thermal noise (sensor heat)
- **Flat** corrects vignetting and dust shadows
- **Offset/Bias** removes readout noise

---

## Script Architecture

### Dependency Order

Masters are created in **topological order** based on dependencies:

```
Bias (no dependencies)
  └─> Dark (depends on Bias)
        └─> DarkFlat (depends on Bias)
              └─> Flat (depends on Dark/DarkFlat/Bias)
```

### Branch Organization

Light frames are organized into **branches** by:
- Camera (instrument)
- Calibration chain (which bias/dark/flat applies)
- Filter name

Example branch ID: `qhy268m_b55_d48_f20_r`
- `qhy268m`: Camera name
- `b55`: Bias set ID 55
- `d48`: Dark set ID 48
- `f20`: Flat set ID 20
- `r`: Red filter

---

## Master Bias Creation

Bias frames capture **readout noise** from the sensor electronics. They are zero-exposure frames.

### Siril Commands

```bash
cd /path/to/biases/set_55
convert bias
stack bias rej 2.5 2.5 -nonorm -out=/path/to/masters/master_bias_55.fit
```

### Command Reference

| Command | Description |
|---------|-------------|
| `cd {path}` | Change to directory containing bias frames |
| `convert bias` | Convert raw files to Siril sequence (creates `bias.seq`) |
| `stack bias rej {low} {high}` | Stack with sigma rejection |
| `-nonorm` | No normalization (preserve absolute values) |
| `-out={path}` | Output master file path |

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| Rejection Low | 2.5 | Lower sigma threshold |
| Rejection High | 2.5 | Upper sigma threshold |

### Why `-nonorm`?

Bias frames must preserve their absolute ADU values because they are **subtracted** from other frames. Normalization would alter these values and cause incorrect calibration.

---

## Master Dark Creation

Dark frames capture **thermal noise** from sensor heat. They should match the exposure time of lights.

### Siril Commands

**With Bias Calibration (Recommended):**
```bash
cd /path/to/darks/set_47
convert dark
calibrate dark -bias=/path/to/masters/master_bias_55
stack pp_dark rej 2.5 2.5 -nonorm -out=/path/to/masters/master_dark_47.fit
```

**Without Bias (if unavailable):**
```bash
cd /path/to/darks/set_47
convert dark
stack dark rej 2.5 2.5 -nonorm -out=/path/to/masters/master_dark_47.fit
```

### Command Reference

| Command | Description |
|---------|-------------|
| `calibrate dark -bias={path}` | Subtract master bias from dark frames |
| `stack pp_dark` | Stack the preprocessed (`pp_`) dark sequence |
| `-nonorm` | Preserve absolute values for subtraction |

### Sequence Prefix

When calibration is applied, Siril creates preprocessed files with `pp_` prefix:
- Input: `dark.seq`
- After calibration: `pp_dark.seq`

---

## Master DarkFlat Creation

DarkFlats are **short-exposure darks** that match flat frame exposure times (typically 1-5 seconds).

### Why DarkFlats?

Regular darks have long exposures (e.g., 120-300s) and contain much more thermal noise than short-exposure flats. Using regular darks for flat calibration causes **over-subtraction** leading to bright edges (inverse vignetting).

### Siril Commands

```bash
cd /path/to/darkflats/set_42
convert darkflat
calibrate darkflat -bias=/path/to/masters/master_bias_55
stack pp_darkflat rej 2.5 2.5 -nonorm -out=/path/to/masters/master_darkflat_42.fit
```

### Processing

Same as regular darks, but:
- Exposure time matches flat exposure
- Used exclusively for flat calibration

---

## Master Flat Creation

Flat frames capture **optical imperfections**: vignetting, dust, and uneven illumination.

### Calibration Priority

Athenaeum uses a **priority system** for flat calibration:

| Priority | Calibration | When Used |
|----------|-------------|-----------|
| 1 | DarkFlat | DarkFlat available with matching exposure |
| 2 | Dark | Dark exposure matches flat exposure (±30%) |
| 3 | Bias | No matching dark available (fallback) |
| 4 | None | No calibration available (not recommended) |

### Why This Priority?

Using a **wrong-exposure dark** for flat calibration causes severe artifacts:
- Long-exposure dark (300s) on short-exposure flat (2s) = over-subtraction
- Result: Bright edges (inverse vignetting)

Bias-only calibration removes only readout noise but is much better than wrong-exposure dark.

### Siril Commands

**With DarkFlat (Best):**
```bash
cd /path/to/flats/set_23_h
convert flat
calibrate flat -dark=/path/to/masters/master_darkflat_42
stack pp_flat rej 2.5 2.5 -norm=mul -out=/path/to/masters/master_flat_23.fit
```

**With Matching Dark:**
```bash
cd /path/to/flats/set_23_h
convert flat
calibrate flat -dark=/path/to/masters/master_dark_47
stack pp_flat rej 2.5 2.5 -norm=mul -out=/path/to/masters/master_flat_23.fit
```

**With Bias Only (Fallback):**
```bash
cd /path/to/flats/set_23_h
convert flat
calibrate flat -dark=/path/to/masters/master_bias_55
stack pp_flat rej 2.5 2.5 -norm=mul -out=/path/to/masters/master_flat_23.fit
```

**No Calibration:**
```bash
cd /path/to/flats/set_23_h
convert flat
stack flat rej 2.5 2.5 -norm=mul -out=/path/to/masters/master_flat_23.fit
```

### Command Reference

| Parameter | Value | Description |
|-----------|-------|-------------|
| `-dark={path}` | Master dark/darkflat/bias | Subtracted from flat |
| `-norm=mul` | Multiplicative | Normalize flats to mean of 1.0 |

### Why `-norm=mul`?

Flat fields are used for **division** (not subtraction). Multiplicative normalization:
- Normalizes average value to 1.0
- Preserves relative pixel sensitivity
- Enables proper vignetting correction

---

## Light Calibration

Light frames are the actual science images. Calibration removes artifacts:

```
Calibrated = (Light - Dark) / Flat
```

### Siril Commands

**Full Calibration (Mono Camera):**
```bash
cd /path/to/lights/branch_02_r
convert lights
calibrate lights -dark=/path/to/masters/master_dark_48 -cc=dark -flat=/path/to/masters/master_flat_20
```

**Flat Only (OSC Camera without Dark):**
```bash
cd /path/to/lights/branch_14_nofilter
convert lights
calibrate lights -flat=/path/to/masters/master_flat_26
preprocess pp_lights -debayer
```

### Command Reference

| Flag | Description |
|------|-------------|
| `-dark={path}` | Apply dark subtraction |
| `-flat={path}` | Apply flat field correction |
| `-cc=dark` | Cosmetic correction using dark (fixes hot pixels) |
| `-cfa` | Respect CFA pattern during dark subtraction (OSC only) |
| `-debayer` | Convert Bayer pattern to RGB (OSC only, after calibration) |

### Branch Skipping

Siril requires at least 2 frames to process. Single-frame branches are skipped:

```bash
# ========== Branch 1 of 15: qhy268m_b0_d0_f22_l ==========
# Camera: QHY268M
# Filter: L
# Lights: 1
# SKIPPED: Siril requires at least 2 frames
```

### OSC (One-Shot Color) Processing

For color cameras with Bayer pattern:
1. Calibrate without debayering (preserve CFA pattern)
2. Run `preprocess -debayer` after calibration
3. If drizzle enabled, debayer happens after stacking

---

## Registration

Registration aligns all light frames to a common reference.

### Global Registration Strategy

Athenaeum uses **global registration**:
1. Merge all calibrated lights from all branches into one sequence
2. Find best reference frame across entire dataset
3. Align all frames to that reference
4. Stack per filter using frame selection

### Siril Commands

```bash
cd /path/to/process

# Step 1: Merge all calibrated sequences
merge "/path/branch_01/pp_lights_" "/path/branch_02/pp_lights_" ... all_lights

# Step 2: Register with global reference
register all_lights -2pass

# Step 3: Apply registration transforms
seqapplyreg all_lights -framing=min
```

### Command Reference

| Command | Description |
|---------|-------------|
| `merge {seq1} {seq2} ... {output}` | Combine multiple sequences |
| `register {seq} -2pass` | Two-pass registration with auto reference |
| `seqapplyreg {seq} -framing=min` | Apply transforms, minimize black borders |

### Why `-2pass`?

Two-pass registration:
1. First pass: Quick star detection on all frames
2. Second pass: Selects best frame as reference, aligns others
3. Better results than single-pass for varied image quality

### Why `-framing=min`?

Framing options:
- `min`: Use minimum bounding box (less black borders)
- `max`: Include all frame areas (may have black corners)
- `cog`: Center of gravity alignment

---

## Stacking

Stacking combines aligned frames to reduce noise and reveal faint details.

### Frame Selection

For per-filter stacking from global sequence:

```bash
# Unselect all frames first
unselect r_all_lights_ 1 126

# Select only frames for this filter (e.g., Red filter, frames 1-7 and 13-15)
select r_all_lights_ 1 7
select r_all_lights_ 13 15
```

### Stack Command

```bash
stack r_all_lights_ rej sigma 2.5 2.5 -filter-included -norm=addscale -output_norm -weight=wfwhm -out=/path/masters/r_stacked.fit
```

### Rejection Algorithms

| Algorithm | Command | Use Case |
|-----------|---------|----------|
| Percentile | `rej percentile {low} {high}` | Small datasets (<20 frames) |
| Sigma | `rej sigma {low} {high}` | General purpose (default) |
| Linear Fit | `rej linear {low} {high}` | Large datasets with gradients |
| Generalized ESD | `rej gesd {low} {high}` | 50+ images, robust outlier detection |
| MAD | `rej mad {low} {high}` | Drizzled CFA data |

### Weighting Methods

| Method | Flag | Description |
|--------|------|-------------|
| WFWHM | `-weight=wfwhm` | Weighted by seeing (FWHM) - **recommended** |
| Star Count | `-weight=nbstars` | Weight by detected stars |
| Noise | `-weight=noise` | Weight by inverse noise level |
| Exposure Time | `-weight=itime` | Weight by integration time |
| None | (omit flag) | Equal weighting |

### Normalization

| Flag | Description |
|------|-------------|
| `-norm=addscale` | Additive + scale normalization (for lights) |
| `-norm=mul` | Multiplicative normalization (for flats) |
| `-nonorm` | No normalization (for bias/dark) |
| `-output_norm` | Save normalization factors |

### OSC-Specific Options

| Flag | Description |
|------|-------------|
| `-rgb_equal` | Equal RGB channel weighting (for Bayer cameras) |
| `-filter-included` | Stack only selected frames |

---

## Configuration Reference

### ExportConfig Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `rejection_algorithm` | Enum | `Sigma` | Pixel rejection method |
| `rejection_low` | f64 | 2.5 | Lower sigma threshold |
| `rejection_high` | f64 | 2.5 | Upper sigma threshold |
| `image_weighting` | Enum | `Wfwhm` | Frame weighting method |
| `reference_frame_mode` | Enum | `SirilAuto` | Reference frame selection |
| `drizzle_enabled` | bool | false | Enable super-resolution |
| `drizzle_scale` | Enum | None | Drizzle scale (2x, 3x) |
| `exptime_tolerance_mode` | Enum | Disabled | Exposure time grouping |
| `exptime_tolerance_value` | f64 | 30.0 | Tolerance (seconds or %) |
| `create_masters` | bool | true | Generate master creation script |

### Rejection Algorithm Options

```rust
pub enum RejectionAlgorithm {
    Percentile,  // Good for <20 frames
    Sigma,       // General purpose (default)
    LinearFit,   // Large sets with gradients
    Gesd,        // 50+ images
    Mad,         // Drizzled CFA data
}
```

### Image Weighting Options

```rust
pub enum ImageWeightingMethod {
    None,          // Equal weighting
    Stars,         // Number of stars detected
    Wfwhm,         // Weighted FWHM (default)
    Noise,         // Inverse noise level
    ExposureTime,  // Integration time
}
```

---

## Complete Examples

### Example 1: Master Bias Creation

```bash
############################################
# Master Bias (Set 55)
############################################
cd /Users/user/data/biases/set_55
convert bias
stack bias rej 2.5 2.5 -nonorm -out=/Users/user/data/masters/master_bias_55.fit
```

### Example 2: Master Dark with Bias

```bash
############################################
# Master Dark (Set 47) - 120s exposure
############################################
cd /Users/user/data/darks/set_47
convert dark
calibrate dark -bias=/Users/user/data/masters/master_bias_55
stack pp_dark rej 2.5 2.5 -nonorm -out=/Users/user/data/masters/master_dark_47.fit
```

### Example 3: Master Flat with Dark

```bash
############################################
# Master Flat (Set 23) - H-alpha filter
############################################
cd /Users/user/data/flats/set_23_h
convert flat
calibrate flat -dark=/Users/user/data/masters/master_dark_47
stack pp_flat rej 2.5 2.5 -norm=mul -out=/Users/user/data/masters/master_flat_23.fit
```

### Example 4: Light Calibration (Mono)

```bash
############################################
# Branch: qhy268m_b55_d48_f20_r
# Camera: QHY268M, Filter: R, Lights: 7
############################################
cd /Users/user/data/lights/branch_02_r
convert lights
calibrate lights -dark=/Users/user/data/masters/master_dark_48 -cc=dark -flat=/Users/user/data/masters/master_flat_20
```

### Example 5: Light Calibration (OSC)

```bash
############################################
# Branch: zwo_asi2600mc_air (OSC Camera)
# Lights: 27, No dark available
############################################
cd /Users/user/data/lights/branch_14_nofilter
convert lights
calibrate lights -flat=/Users/user/data/masters/master_flat_26
preprocess pp_lights -debayer
```

### Example 6: Registration and Stacking

```bash
############################################
# Global Registration and Per-Filter Stacking
############################################

cd /Users/user/data/process

# Merge all calibrated sequences
merge "/path/branch_02_r/pp_lights_" "/path/branch_04_b/pp_lights_" "/path/branch_06_g/pp_lights_" all_lights

# Register with global reference
register all_lights -2pass

# Apply registration transforms
seqapplyreg all_lights -framing=min

# Stack Red filter (frames 1-7, 13-15, 25-44)
unselect r_all_lights_ 1 126
select r_all_lights_ 1 7
select r_all_lights_ 13 15
select r_all_lights_ 25 44
stack r_all_lights_ rej sigma 2.5 2.5 -filter-included -norm=addscale -output_norm -weight=wfwhm -out=/Users/user/data/masters/r_stacked.fit

# Stack Blue filter (frames 8-12, 30-34, 55-64)
unselect r_all_lights_ 1 126
select r_all_lights_ 8 12
select r_all_lights_ 30 34
select r_all_lights_ 55 64
stack r_all_lights_ rej sigma 2.5 2.5 -filter-included -norm=addscale -output_norm -weight=wfwhm -out=/Users/user/data/masters/b_stacked.fit
```

---

## Troubleshooting

### Bright Edges on Flats

**Symptom:** Stacked images have bright edges instead of proper vignetting.

**Cause:** Flat frames calibrated with wrong-exposure dark (e.g., 300s dark for 2s flat).

**Solution:** Use DarkFlat or Bias instead. Athenaeum now automatically falls back to Bias if no matching-exposure dark is available (±30% tolerance).

### Black Borders After Registration

**Symptom:** Stacked images have large black borders.

**Cause:** Frames shifted significantly during registration.

**Solution:** Use `-framing=min` (default) to minimize borders.

### Single Frame Branches Skipped

**Symptom:** Some branches show "SKIPPED: Siril requires at least 2 frames".

**Cause:** Siril cannot process single-frame sequences.

**Solution:** Ensure each filter/camera combination has at least 2 light frames.
