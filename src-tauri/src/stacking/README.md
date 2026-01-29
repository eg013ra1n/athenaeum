# Stacking Module - Siril FFI Integration

This module provides direct Rust bindings to the Siril astrophotography processing library, enabling integrated stacking functionality without relying on CLI subprocess execution.

## Status

**Stage 1: Foundation** - Complete

- FFI type definitions for core Siril structures
- FFI function declarations (with stub fallbacks)
- Safe Rust wrappers for FITS image handling
- Feature flag (`siril-ffi`) for conditional compilation

**Stage 2: Master Calibration Creation** - Complete

- Configuration types (MasterConfig, FlatConfig, RejectionConfig)
- Sequence management (ImageSequence)
- Master creation functions (create_master_bias, create_master_dark, create_master_flat)
- Memory estimation utilities

**Stage 3: Light Frame Calibration** - Complete

- Calibration configuration (CalibrationConfig, DebayerConfig)
- Dark optimization modes (Off, ExposureBased, NoiseMinimization)
- Flat normalization (Auto, Manual)
- Debayer/demosaicing support (Bilinear, VNG, AHD, AMaZE, RCD)
- Bayer pattern support (RGGB, BGGR, GBRG, GRBG, Auto)
- Batch calibration with progress reporting
- FFI bindings for preprocessing and demosaicing functions

**Stage 4: Frame Quality Measurement** - Complete

- Star detection using peaker algorithm
- FWHM measurement (X, Y, and average)
- Roundness calculation for tracking quality
- Background level and noise estimation
- Quality scoring for frame ranking
- Best reference frame selection
- Batch quality measurement with filtering
- FFI bindings for star finder and PSF functions

**Stage 5: Plate-Solving and Registration** - Complete

- Plate solving configuration (solver, catalogue, scale, search radius)
- Support for Siril internal solver and astrometry.net
- Multiple star catalogues (Gaia DR3, Tycho-2, USNO-B1)
- WCS solution extraction and storage
- Astrometric registration using WCS coordinates
- Star-based registration fallback
- Homography transformation computation
- Multiple framing modes (Current, Max, Min, CenterOfGravity)
- Multiple interpolation methods (Nearest, Bilinear, Bicubic, Lanczos4)
- FFI bindings for plate solving, WCS, and image transformation

**Stage 6: Light Stacking** - Complete

- Frame grouping by filter and/or exposure time
- Configurable exposure tolerance for grouping
- Frame metadata extraction (filter, exposure, quality)
- Quality-based weight calculation (FWHM, star count, noise)
- Multi-group stacking with progress reporting
- Recommended configuration based on frame count
- Output size estimation
- Stack result tracking (frames, exposure, excluded frames)

**Stage 7: Pipeline Integration** - Complete

- FFI-based executor (`FfiExecutor`) for pipeline steps
- Unified configuration system (`ExecutorConfig`) with sections for:
  - Master creation settings
  - Calibration settings (dark optimization, flat normalization, debayer)
  - Registration settings (plate-solving, framing, interpolation)
  - Stacking settings (method, rejection, normalization, weighting)
- Step result tracking (`FfiStepResult`) with:
  - Success/failure status
  - Output file list
  - Skip detection (existing outputs)
  - Processing statistics (frames, exposure, timing)
- Progress callbacks for all operations
- Cancellation support via atomic flag
- Drop-in replacement for CLI-based execution

## Architecture

```text
src/stacking/
├── mod.rs           # Module exports and initialization
├── error.rs         # Error types (StackingError)
├── config.rs        # Configuration types (MasterConfig, etc.)
├── fits.rs          # Safe FitsImage wrapper
├── sequence.rs      # Sequence management for batch operations
├── master.rs        # Master calibration frame creation
├── calibration.rs   # Light frame calibration and debayering
├── quality.rs       # Frame quality measurement (FWHM, stars)
├── registration.rs  # Plate-solving and image alignment
├── stacking.rs      # Light frame stacking and grouping
├── executor.rs      # FFI-based pipeline step executor
├── ffi/
│   ├── mod.rs       # FFI submodule exports
│   ├── types.rs     # C struct definitions (Fits, Sequence, etc.)
│   └── functions.rs # C function bindings
└── README.md        # This file
```

## Building with Siril FFI

### Prerequisites

The Siril stacking library requires several dependencies:

**macOS (Homebrew):**

```bash
brew install cfitsio gsl wcslib fftw lcms2 glib
# Optional: opencv (for advanced registration)
```

**Linux (Debian/Ubuntu):**

```bash
sudo apt install libcfitsio-dev libgsl-dev wcslib-dev libfftw3-dev liblcms2-dev libglib2.0-dev
# Optional: libopencv-dev
```

### Option 1: Pre-built Siril Library (Recommended)

1. Build Siril as a static library:

```bash
cd src/stacking
# Create parent meson.build if needed
meson setup build --default-library=static -Denable-gui=false
meson compile -C build
```

1. Set environment variables and build:

```bash
export SIRIL_LIB_DIR=/path/to/src/stacking/build/src
export SIRIL_INCLUDE_DIR=/path/to/src/stacking/src
cargo build --features siril-ffi
```

### Option 2: Without FFI (Development/Testing)

Without the `siril-ffi` feature, the module compiles with stub implementations that return errors. This allows development and testing of the API without the C library.

```bash
cargo build  # No siril-ffi feature
```

## Usage

### Creating Master Calibration Frames

```rust
use std::path::Path;
use crate::stacking::{
    initialize, create_master_bias, create_master_dark, create_master_flat,
    MasterConfig, FlatConfig,
};

// Initialize the library
initialize()?;

// Create master bias from bias frames
let bias_paths = vec!["bias_001.fits", "bias_002.fits", "bias_003.fits"];
let bias_result = create_master_bias(
    &bias_paths,
    Path::new("master_bias.fits"),
    &MasterConfig::median(),
    None, // No progress callback
)?;
println!("Created master bias from {} frames", bias_result.frame_count);

// Create master dark with sigma rejection
let dark_paths = vec!["dark_001.fits", "dark_002.fits", "dark_003.fits"];
let dark_config = MasterConfig::mean_with_sigma(2.5, 2.5);
let dark_result = create_master_dark(
    &dark_paths,
    Path::new("master_dark.fits"),
    &dark_config,
    None,
)?;
println!("Total dark exposure: {:?}s", dark_result.total_exposure_seconds);

// Create master flat (calibrated with bias)
let flat_paths = vec!["flat_001.fits", "flat_002.fits", "flat_003.fits"];
let flat_config = FlatConfig::default()
    .with_bias("master_bias.fits");
let flat_result = create_master_flat(
    &flat_paths,
    Path::new("master_flat.fits"),
    &flat_config,
    None,
)?;
```

### Working with Sequences

```rust
use crate::stacking::{ImageSequence, FitsImage};

// Create a sequence from file paths
let paths = vec!["frame_001.fits", "frame_002.fits", "frame_003.fits"];
let mut sequence = ImageSequence::from_paths(&paths)?;

// Validate all frames have matching dimensions
sequence.validate_dimensions()?;

// Get sequence info
let summary = sequence.summary();
println!("Sequence: {}", summary);

// Load a specific frame
let frame = sequence.load_frame(0)?;
println!("Frame dimensions: {:?}", frame.dimensions());
```

### Memory Management

```rust
use crate::stacking::{estimate_memory_requirement, check_memory_available};

// Estimate memory for stacking 20 frames, 4656x3520, mono, 32-bit
let required = estimate_memory_requirement(20, 4656, 3520, 1, true);
println!("Estimated memory: {} MB", required / 1_000_000);

// Check if we have enough memory
check_memory_available(required)?;
```

### Light Frame Calibration

```rust
use std::path::PathBuf;
use crate::stacking::{
    FitsImage, calibrate_light, calibrate_lights_batch, debayer_frame,
    CalibrationConfig, DebayerConfig, BatchCalibrationConfig,
    DarkOptimization, FlatNormalization, DebayerMethod, BayerPattern,
};

// Load calibration masters
let bias = FitsImage::open("master_bias.fits")?;
let dark = FitsImage::open("master_dark.fits")?;
let flat = FitsImage::open("master_flat.fits")?;

// Configure calibration
let config = CalibrationConfig {
    use_bias: true,
    use_dark: true,
    use_flat: true,
    dark_optimization: DarkOptimization::ExposureBased,
    flat_normalization: FlatNormalization::Auto,
    output_32bit: true,
};

// Calibrate a single light frame
let mut light = FitsImage::open("light_001.fits")?;
calibrate_light(&mut light, Some(&bias), Some(&dark), Some(&flat), &config)?;

// Debayer if OSC camera
let debayer_config = DebayerConfig {
    method: DebayerMethod::Vng,
    pattern: BayerPattern::Auto, // Auto-detect from FITS header
};
debayer_frame(&mut light, &debayer_config)?;

// Save calibrated frame
light.save("calibrated_001.fits")?;
```

### Batch Calibration

```rust
use crate::stacking::{
    calibrate_lights_batch, BatchCalibrationConfig,
    CalibrationConfig, DebayerConfig, DebayerMethod,
};

// Configure batch processing
let batch_config = BatchCalibrationConfig {
    calibration: CalibrationConfig::default(),
    debayer: Some(DebayerConfig::default()),
    output_dir: PathBuf::from("./calibrated"),
    output_prefix: "cal_".to_string(),
};

// Get list of light frames
let light_paths: Vec<_> = vec![
    "light_001.fits",
    "light_002.fits",
    "light_003.fits",
];

// Run batch calibration with progress callback
let result = calibrate_lights_batch(
    &light_paths,
    Some(&bias),
    Some(&dark),
    Some(&flat),
    &batch_config,
    Some(Box::new(|progress, message| {
        println!("{:.0}%: {}", progress * 100.0, message);
    })),
)?;

println!("Calibrated {} frames, {} failed",
    result.calibrated_count,
    result.failed_count);
```

### Frame Quality Measurement

```rust
use crate::stacking::{
    FitsImage, measure_frame_quality, measure_quality_batch, select_best_reference,
    QualityConfig, PsfProfile,
};

// Configure quality measurement
let config = QualityConfig {
    detection_sigma: 3.0,     // Star detection threshold
    max_stars: 500,           // Max stars to analyze
    channel: None,            // Use first channel
    fwhm_min: Some(1.0),      // Filter hot pixels
    fwhm_max: Some(20.0),     // Filter extended objects
    roundness_limit: 0.5,     // Max ellipticity
    profile: PsfProfile::Gaussian,
};

// Measure single frame quality
let frame = FitsImage::open("light_001.fits")?;
let metrics = measure_frame_quality(&frame, &config)?;

println!("FWHM: {:.2} pixels", metrics.fwhm);
println!("FWHM: {:.2} arcsec", metrics.fwhm_arcsec.unwrap_or(0.0));
println!("Roundness: {:.3}", metrics.roundness);
println!("Stars: {} detected, {} used", metrics.star_count, metrics.stars_used);
println!("Quality score: {:.2}", metrics.quality_score);
```

### Batch Quality Measurement

```rust
use crate::stacking::{measure_quality_batch, select_best_reference, QualityConfig};

let light_paths = vec![
    "light_001.fits",
    "light_002.fits",
    "light_003.fits",
];

// Measure all frames with progress
let result = measure_quality_batch(
    &light_paths,
    &QualityConfig::default(),
    Some(Box::new(|current, total, msg| {
        println!("[{}/{}] {}", current, total, msg);
    })),
)?;

println!("Best frame: {} (index {})",
    result.best_frame_path,
    result.best_frame_index);
println!("Average FWHM: {:.2}", result.average_fwhm);
println!("Median FWHM: {:.2}", result.median_fwhm);

// Or simply select the best reference frame
let (best_idx, best_path) = select_best_reference(&light_paths, &QualityConfig::default())?;
println!("Use frame {} as reference", best_path.display());
```

### Plate-Solving

```rust
use crate::stacking::{
    FitsImage, platesolve_frame, platesolve_batch,
    PlateSolveConfig, Solver, Catalogue, MagnitudeLimit,
};

// Configure plate solving
let config = PlateSolveConfig {
    solver: Solver::SirilInternal,
    catalogue: Catalogue::GaiaDR3,
    pixel_size_um: 3.76,        // Camera pixel size
    focal_length_mm: 500.0,      // Telescope focal length
    search_radius_deg: 10.0,     // Search area
    magnitude_limit: MagnitudeLimit::Auto,
    distortion_order: 1,         // SIP polynomial order (1 = linear)
    scale_tolerance_percent: 5.0,
    max_stars: 500,
    downsample: false,
    initial_ra: Some(180.0),     // Hint: approximate RA in degrees
    initial_dec: Some(45.0),     // Hint: approximate Dec in degrees
};

// Plate solve a single frame
let mut frame = FitsImage::open("light_001.fits")?;
let result = platesolve_frame(&mut frame, &config)?;

if result.success {
    println!("Solved! Center: RA={:.4}°, Dec={:.4}°",
        result.ra_center, result.dec_center);
    println!("Scale: {:.3} arcsec/pixel", result.scale_arcsec_per_pixel);
    println!("Rotation: {:.2}°", result.rotation_deg);
    println!("Matched {} stars, RMS={:.2}\"",
        result.matched_stars, result.rms_arcsec);

    // Save frame with WCS
    frame.save("light_001_solved.fits")?;
} else {
    println!("Solve failed: {:?}", result.error);
}
```

### Batch Plate-Solving

```rust
use crate::stacking::{platesolve_batch, PlateSolveConfig};

let light_paths = vec![
    "light_001.fits",
    "light_002.fits",
    "light_003.fits",
];

// Batch solve with progress
let result = platesolve_batch(
    &light_paths,
    &PlateSolveConfig::default(),
    Some(Box::new(|current, total, msg| {
        println!("[{}/{}] {}", current, total, msg);
    })),
)?;

println!("Solved {}/{} frames", result.success_count,
    result.success_count + result.failed_count);
```

### Image Registration

```rust
use crate::stacking::{
    register_astrometric, register_star_alignment, apply_registration,
    RegistrationConfig, Framing, Interpolation, Transformation,
};

// Configure registration
let config = RegistrationConfig {
    framing: Framing::Max,           // Maximum coverage
    interpolation: Interpolation::Lanczos4,
    transformation: Transformation::Homography,
    output_scale: 1.0,               // 1.0 = native, 2.0 = 2x drizzle
    clamp: true,
    clamping_factor: 3.0,
    min_pairs: 10,
    max_stars: 500,
    two_pass: false,
};

// Get plate-solved frames
let frame_paths = vec![
    "light_001_solved.fits",
    "light_002_solved.fits",
    "light_003_solved.fits",
];

// Use best frame as reference (from quality measurement)
let reference_index = 1;

// Compute alignment using WCS (astrometric registration)
let result = register_astrometric(
    &frame_paths,
    reference_index,
    &config,
    Some(Box::new(|current, total, msg| {
        println!("[{}/{}] {}", current, total, msg);
    })),
)?;

println!("Output size: {}x{}", result.output_width, result.output_height);
println!("Registered {}/{} frames", result.success_count,
    result.success_count + result.failed_count);

// Access homographies for each frame
for (i, h) in result.homographies.iter().enumerate() {
    println!("Frame {}: {} star pairs, {} inliers",
        i, h.matched_pairs, h.inliers);
}
```

### Star-Based Registration (Fallback)

```rust
use crate::stacking::{register_star_alignment, RegistrationConfig};

// For frames without WCS, use star pattern matching
let result = register_star_alignment(
    &frame_paths,
    reference_index,
    &RegistrationConfig::default(),
    None,
)?;

println!("Aligned {} frames via star matching", result.success_count);
```

### Calculate Image Scale

```rust
use crate::stacking::calculate_image_scale;

// Calculate expected plate scale
let pixel_um = 3.76;     // Camera pixel size
let focal_mm = 500.0;    // Telescope focal length
let scale = calculate_image_scale(pixel_um, focal_mm);
println!("Expected scale: {:.3} arcsec/pixel", scale);
// Output: Expected scale: 1.551 arcsec/pixel
```

### Frame Grouping

```rust
use crate::stacking::{
    group_frames, FrameMetadata, GroupingConfig,
};

// Create frame metadata
let frames = vec![
    FrameMetadata {
        id: Some(1),
        path: PathBuf::from("l1.fits"),
        filter: "L".to_string(),
        exposure_time: 120.0,
        is_osc: false,
        quality: None,
    },
    FrameMetadata {
        id: Some(2),
        path: PathBuf::from("l2.fits"),
        filter: "L".to_string(),
        exposure_time: 120.0,
        is_osc: false,
        quality: None,
    },
    FrameMetadata {
        id: Some(3),
        path: PathBuf::from("ha1.fits"),
        filter: "Ha".to_string(),
        exposure_time: 300.0,
        is_osc: false,
        quality: None,
    },
];

// Configure grouping
let config = GroupingConfig {
    group_by_filter: true,
    group_by_exposure: false,
    exposure_tolerance_percent: 5.0,
    min_frames_per_group: 2,
};

// Group frames
let groups = group_frames(&frames, &config);
for group in &groups {
    println!("Group '{}': {} frames, {:.0}s total exposure",
        group.key, group.len(), group.total_exposure());
}
```

### Light Frame Stacking

```rust
use std::path::Path;
use crate::stacking::{
    stack_group, stack_all_groups, FrameGroup, LightStackConfig,
    StackingMethod, RejectionConfig, NormalizationMethod, WeightingMethod,
};

// Configure stacking
let config = LightStackConfig {
    method: StackingMethod::Mean,
    rejection: Some(RejectionConfig::sigma(2.5, 2.5)),
    normalization: NormalizationMethod::Additive,
    weighting: WeightingMethod::Fwhm,
    output_32bit: true,
    drizzle: None,
};

// Stack a single group
let result = stack_group(
    &group,
    Path::new("output/L_stacked.fits"),
    &config,
    Some(&Box::new(|p, msg| {
        println!("{:.0}%: {}", p * 100.0, msg);
    })),
)?;

println!("Stacked {} frames, total exposure: {:.0}s",
    result.frame_count, result.total_exposure);
```

### Multi-Group Stacking

```rust
use crate::stacking::{stack_all_groups, LightStackConfig};

// Stack all groups
let result = stack_all_groups(
    &groups,
    Path::new("output"),
    &LightStackConfig::default(),
    Some(Box::new(|group, progress, msg| {
        println!("[{}] {:.0}%: {}", group, progress * 100.0, msg);
    })),
)?;

println!("Stacked {} groups with {} total frames",
    result.stacks.len(), result.total_frames_stacked);

// Report any failures
for failed in &result.failed_groups {
    eprintln!("Failed to stack {}: {}", failed.group_key, failed.error);
}
```

### Weight Calculation

```rust
use crate::stacking::{calculate_weights, WeightingMethod, QualityMetrics};

// Quality metrics from frame quality measurement
let metrics: Vec<Option<QualityMetrics>> = vec![
    Some(QualityMetrics { fwhm: 2.0, ..Default::default() }),
    Some(QualityMetrics { fwhm: 3.0, ..Default::default() }),
    Some(QualityMetrics { fwhm: 4.0, ..Default::default() }),
];

// Calculate FWHM-based weights
let weights = calculate_weights(&metrics, WeightingMethod::Fwhm);
println!("Weights: {:?}", weights);
// Sharper frames (lower FWHM) get higher weight
```

### Recommended Configuration

```rust
use crate::stacking::recommended_config;

// Get recommended config based on frame count
let config = recommended_config(25);
// For 25 frames: Mean stacking with sigma rejection (2.5, 2.5)
// and FWHM weighting
```

## Configuration Types

### MasterConfig

Configuration for creating master bias, dark frames:

| Field          | Type                      | Default  | Description                |
|----------------|---------------------------|----------|----------------------------|
| `method`       | `StackingMethod`          | `Median` | Stacking algorithm         |
| `rejection`    | `Option<RejectionConfig>` | `None`   | Pixel rejection settings   |
| `output_32bit` | `bool`                    | `true`   | Output 32-bit float        |

### FlatConfig

Configuration for creating master flat frames:

| Field                 | Type            | Default  | Description                   |
|-----------------------|-----------------|----------|-------------------------------|
| `stacking`            | `MasterConfig`  | median   | Base stacking configuration   |
| `calibrate_with_dark` | `Option<String>`| `None`   | Path to master dark           |
| `calibrate_with_bias` | `Option<String>`| `None`   | Path to master bias           |
| `equalize_cfa`        | `bool`          | `true`   | Equalize CFA for OSC cameras  |

### RejectionConfig

Pixel rejection settings:

| Field        | Type                  | Default | Description                  |
|--------------|-----------------------|---------|------------------------------|
| `algorithm`  | `RejectionAlgorithm`  | `None`  | Rejection algorithm          |
| `sigma_low`  | `f64`                 | `3.0`   | Low sigma threshold          |
| `sigma_high` | `f64`                 | `3.0`   | High sigma threshold         |

### Rejection Algorithms

- `None` - No rejection
- `Sigma` - Sigma clipping
- `Mad` - Median Absolute Deviation
- `Winsorized` - Winsorized sigma clipping
- `LinearFit` - Linear fit clipping
- `Gesdt` - Generalized ESD test

### Stacking Methods

- `Median` - Robust against outliers, standard for calibration frames
- `Mean` - Higher SNR, use with rejection for light frames
- `Sum` - Preserves total signal

### CalibrationConfig

Configuration for light frame calibration:

| Field               | Type                | Default  | Description                     |
|---------------------|---------------------|----------|---------------------------------|
| `use_bias`          | `bool`              | `true`   | Apply bias subtraction          |
| `use_dark`          | `bool`              | `true`   | Apply dark subtraction          |
| `use_flat`          | `bool`              | `true`   | Apply flat division             |
| `dark_optimization` | `DarkOptimization`  | `Off`    | Dark scaling method             |
| `flat_normalization`| `FlatNormalization` | `Auto`   | Flat normalization method       |
| `output_32bit`      | `bool`              | `true`   | Output 32-bit float             |

### Dark Optimization Methods

- `Off` - No optimization, subtract dark as-is
- `ExposureBased` - Scale dark by exposure time ratio (dark_exp / light_exp)
- `NoiseMinimization` - Compute optimal scaling to minimize noise

### Flat Normalization Methods

- `Auto` - Compute normalization from flat center region
- `Manual(f32)` - Use specified normalization value

### DebayerConfig

Configuration for demosaicing CFA images:

| Field    | Type            | Default | Description                |
|----------|-----------------|---------|----------------------------|
| `method` | `DebayerMethod` | `Vng`   | Interpolation algorithm    |
| `pattern`| `BayerPattern`  | `Auto`  | CFA pattern                |

### Debayer Methods

- `Bilinear` - Fast, lower quality
- `Vng` - Variable Number of Gradients (good balance)
- `Ahd` - Adaptive Homogeneity-Directed (high quality)
- `Amaze` - AMaZE algorithm (very high quality, slower)
- `Rcd` - RCD algorithm (high quality, fast)
- `Auto` - Auto-select based on sensor type

### Bayer Patterns

- `Rggb` - Red-Green-Green-Blue pattern
- `Bggr` - Blue-Green-Green-Red pattern
- `Gbrg` - Green-Blue-Red-Green pattern
- `Grbg` - Green-Red-Blue-Green pattern
- `Auto` - Auto-detect from FITS header BAYERPAT keyword

### QualityConfig

Configuration for frame quality measurement:

| Field             | Type           | Default     | Description                          |
|-------------------|----------------|-------------|--------------------------------------|
| `detection_sigma` | `f64`          | `3.0`       | Star detection threshold             |
| `max_stars`       | `u32`          | `500`       | Maximum stars to analyze             |
| `channel`         | `Option<u32>`  | `None`      | Channel to analyze (None = first)    |
| `fwhm_min`        | `Option<f64>`  | `Some(1.0)` | Minimum FWHM filter (pixels)         |
| `fwhm_max`        | `Option<f64>`  | `Some(20.0)`| Maximum FWHM filter (pixels)         |
| `roundness_limit` | `f64`          | `0.5`       | Maximum roundness deviation          |
| `profile`         | `PsfProfile`   | `Gaussian`  | PSF fitting profile type             |

### QualityMetrics

Output from frame quality measurement:

| Field               | Type          | Description                              |
|---------------------|---------------|------------------------------------------|
| `fwhm`              | `f64`         | Average FWHM in pixels                   |
| `fwhm_arcsec`       | `Option<f64>` | FWHM in arcseconds (if scale known)      |
| `fwhm_x`            | `f64`         | FWHM in X direction                      |
| `fwhm_y`            | `f64`         | FWHM in Y direction                      |
| `roundness`         | `f64`         | Star roundness (1.0 = circle)            |
| `star_count`        | `u32`         | Total stars detected                     |
| `stars_used`        | `u32`         | Stars after filtering                    |
| `background_level`  | `f64`         | Mean background level                    |
| `background_noise`  | `f64`         | Background noise (std dev)               |
| `quality_score`     | `f64`         | Combined score (lower = better)          |
| `has_saturated_stars` | `bool`      | Whether saturated stars present          |

### PSF Profiles

- `Gaussian` - Standard Gaussian PSF model (fast, good for well-sampled images)
- `Moffat` - Moffat PSF model (better for atmospheric seeing, extended wings)

### PlateSolveConfig

Configuration for plate solving:

| Field                    | Type              | Default        | Description                          |
|--------------------------|-------------------|----------------|--------------------------------------|
| `solver`                 | `Solver`          | `SirilInternal`| Plate solver to use                  |
| `catalogue`              | `Catalogue`       | `GaiaDR3`      | Star catalogue for reference         |
| `pixel_size_um`          | `f64`             | `3.76`         | Camera pixel size in micrometers     |
| `focal_length_mm`        | `f64`             | `500.0`        | Telescope focal length in mm         |
| `search_radius_deg`      | `f64`             | `10.0`         | Search radius in degrees             |
| `magnitude_limit`        | `MagnitudeLimit`  | `Auto`         | Star magnitude limit                 |
| `distortion_order`       | `u32`             | `1`            | SIP polynomial order (1=linear)      |
| `scale_tolerance_percent`| `f64`             | `5.0`          | Scale matching tolerance             |
| `max_stars`              | `u32`             | `500`          | Max stars to use for solving         |
| `downsample`             | `bool`            | `false`        | Downsample before solving            |
| `initial_ra`             | `Option<f64>`     | `None`         | Initial RA hint (degrees)            |
| `initial_dec`            | `Option<f64>`     | `None`         | Initial Dec hint (degrees)           |

### Solvers

- `SirilInternal` - Siril's built-in plate solver (recommended)
- `AstrometryNet` - Local astrometry.net installation

### Catalogues

- `GaiaDR3` - Gaia Data Release 3 (recommended, most accurate)
- `GaiaEDR3` - Gaia Early Data Release 3
- `GaiaDR2` - Gaia Data Release 2
- `Tycho2` - Tycho-2 catalogue
- `UsnoB1` - USNO-B1 catalogue
- `Local` - Local catalogue file

### MagnitudeLimit

- `Auto` - Automatically determine from image depth
- `Fixed(f64)` - Use specified magnitude limit

### RegistrationConfig

Configuration for image registration:

| Field             | Type              | Default     | Description                          |
|-------------------|-------------------|-------------|--------------------------------------|
| `framing`         | `Framing`         | `Max`       | Output framing type                  |
| `interpolation`   | `Interpolation`   | `Lanczos4`  | Interpolation method                 |
| `transformation`  | `Transformation`  | `Homography`| Transformation type                  |
| `output_scale`    | `f64`             | `1.0`       | Output scale (1.0=native, 2.0=2x)    |
| `clamp`           | `bool`            | `true`      | Enable clamping for ringing          |
| `clamping_factor` | `f64`             | `3.0`       | Clamping factor                      |
| `min_pairs`       | `u32`             | `10`        | Minimum star pairs for alignment     |
| `max_stars`       | `u32`             | `500`       | Max candidate stars                  |
| `two_pass`        | `bool`            | `false`     | Two-pass registration                |

### Framing Types

- `Current` - Use reference frame dimensions
- `Max` - Union of all frames (maximum coverage, may have black borders)
- `Min` - Intersection of all frames (no black borders)
- `CenterOfGravity` - Center on mean position of all frames

### Interpolation Methods

- `Nearest` - Nearest neighbor (fastest, lowest quality)
- `Bilinear` - Bilinear (fast, good quality)
- `Bicubic` - Bicubic (slower, better quality)
- `Lanczos4` - Lanczos4 (slowest, best quality, recommended)
- `Area` - Area-based (good for downscaling)

### Transformation Types

- `Shift` - Translation only (2 DOF)
- `Similarity` - Shift + rotation + scale (4 DOF)
- `Affine` - Affine transformation (6 DOF)
- `Homography` - Full perspective transformation (8 DOF, recommended)

### WcsSolution

WCS solution data extracted from plate solving:

| Field    | Type     | Description                     |
|----------|----------|---------------------------------|
| `crpix1` | `f64`    | Reference pixel X (CRPIX1)      |
| `crpix2` | `f64`    | Reference pixel Y (CRPIX2)      |
| `crval1` | `f64`    | Reference RA in degrees         |
| `crval2` | `f64`    | Reference Dec in degrees        |
| `cd1_1`  | `f64`    | CD matrix element 1,1           |
| `cd1_2`  | `f64`    | CD matrix element 1,2           |
| `cd2_1`  | `f64`    | CD matrix element 2,1           |
| `cd2_2`  | `f64`    | CD matrix element 2,2           |
| `naxis1` | `i32`    | Image width                     |
| `naxis2` | `i32`    | Image height                    |
| `ctype1` | `String` | Coordinate type for axis 1      |
| `ctype2` | `String` | Coordinate type for axis 2      |

### HomographyData

Transformation matrix data:

| Field          | Type          | Description                     |
|----------------|---------------|---------------------------------|
| `h`            | `[[f64;3];3]` | 3x3 homography matrix           |
| `matched_pairs`| `i32`         | Number of matched star pairs    |
| `inliers`      | `i32`         | Number of inliers after RANSAC  |
| `frame_path`   | `String`      | Path to the frame               |
| `success`      | `bool`        | Whether registration succeeded  |

### GroupingConfig

Configuration for grouping frames before stacking:

| Field                      | Type    | Default | Description                          |
|----------------------------|---------|---------|--------------------------------------|
| `group_by_filter`          | `bool`  | `true`  | Group frames by filter name          |
| `group_by_exposure`        | `bool`  | `false` | Group frames by exposure time        |
| `exposure_tolerance_percent`| `f64`  | `5.0`   | Exposure matching tolerance          |
| `min_frames_per_group`     | `usize` | `3`     | Minimum frames per group             |

### FrameMetadata

Metadata for a frame used in grouping:

| Field          | Type                    | Description                     |
|----------------|-------------------------|---------------------------------|
| `id`           | `Option<i64>`           | Database ID (optional)          |
| `path`         | `PathBuf`               | Path to the frame file          |
| `filter`       | `String`                | Filter name                     |
| `exposure_time`| `f64`                   | Exposure time in seconds        |
| `is_osc`       | `bool`                  | Is one-shot color               |
| `quality`      | `Option<QualityMetrics>`| Quality metrics (if measured)   |

### FrameGroup

A group of frames to be stacked together:

| Field          | Type               | Description                     |
|----------------|--------------------|---------------------------------|
| `key`          | `String`           | Unique group key                |
| `filter_name`  | `String`           | Filter name for this group      |
| `exposure_time`| `f64`              | Representative exposure time    |
| `is_osc`       | `bool`             | Whether frames are OSC          |
| `frames`       | `Vec<FrameMetadata>`| Frames in this group           |

### StackResult

Result of stacking a single group:

| Field              | Type                | Description                     |
|--------------------|---------------------|---------------------------------|
| `group_key`        | `String`            | Group that was stacked          |
| `filter_name`      | `String`            | Filter name                     |
| `frame_count`      | `usize`             | Number of frames stacked        |
| `total_exposure`   | `f64`               | Total exposure time             |
| `output_path`      | `PathBuf`           | Output file path                |
| `processing_time_ms`| `u64`              | Processing time                 |
| `excluded_frames`  | `Vec<ExcludedFrame>`| Frames that were excluded       |

### MultiGroupStackResult

Result of stacking multiple groups:

| Field                 | Type              | Description                     |
|-----------------------|-------------------|---------------------------------|
| `stacks`              | `Vec<StackResult>`| Results for each group          |
| `failed_groups`       | `Vec<FailedGroup>`| Groups that failed to stack     |
| `total_frames_stacked`| `usize`           | Total frames stacked            |
| `total_exposure`      | `f64`             | Total exposure time             |

## FFI Types

The `ffi::types` module defines Rust representations of Siril's C structures:

| Rust Type           | C Type               | Description                                       |
|---------------------|----------------------|---------------------------------------------------|
| `Fits`              | `fits`               | Main image structure with pixel data and metadata |
| `Sequence`          | `sequence`           | Collection of images for batch processing         |
| `StackingArgs`      | `stacking_args`      | Configuration for stacking operations             |
| `PreprocessingData` | `preprocessing_data` | Calibration configuration                         |
| `RegistrationArgs`  | `registration_args`  | Registration configuration                        |
| `PsfStar`           | `psf_star`           | Star measurement result                           |
| `Homography`        | `Homography`         | 3x3 transformation matrix                         |

## FFI Functions

The `ffi::functions` module declares bindings to Siril C functions:

### FITS I/O

- `readfits()` - Load FITS file
- `savefits()` - Save FITS file
- `copyfits()` - Copy fits structure
- `clearfits()` - Free memory
- `new_fit_image()` - Allocate new image

### Arithmetic

- `soper()` - Scalar operations
- `imoper()` - Image-to-image operations
- `siril_fdiv()` - Flat division

### Stacking

- `stack_median()` - Median stacking
- `stack_mean_with_rejection()` - Mean with rejection
- `do_normalization()` - Compute normalization coefficients

### Registration

- `register_star_alignment()` - Star-based registration
- `compute_Hs_from_astrometry()` - Astrometric registration

### Preprocessing

- `preprocess_single_image()` - Calibrate a single image
- `preprocess_given_image()` - Calibrate image from file path
- `calibrate_single_image()` - Batch calibration entry point
- `clear_preprocessing_data()` - Clean up preprocessing resources
- `compute_flat_mean_from_rgb()` - Compute flat normalization value

### Demosaicing

- `debayer()` - Debayer a CFA image with specified pattern and method
- `debayer_if_needed()` - Auto-detect and debayer if needed
- `get_validated_cfa_pattern()` - Get Bayer pattern from FITS header
- `fit_is_cfa()` - Check if image has a CFA pattern
- `get_cfa_pattern_index_from_string()` - Parse pattern from string

### Star Finding

- `peaker()` - Main star detection algorithm
- `free_fitted_stars()` - Free star array memory
- `new_fitted_stars()` - Allocate star array
- `measure_image_FWHM()` - Quick FWHM measurement
- `FWHM_stats()` - Calculate FWHM statistics from stars
- `filtered_FWHM_average()` - Outlier-resistant FWHM average
- `sort_stars_by_mag()` - Sort stars by brightness
- `filter_stars_by_amplitude()` - Filter by amplitude threshold

### PSF Fitting

- `psf_get_fwhm()` - Get FWHM from image region
- `psf_get_minimisation()` - Fit PSF to star
- `new_psf_star()` - Create new star structure
- `free_psf()` - Free star structure
- `duplicate_psf()` - Copy star structure
- `fwhm_to_arcsec_if_needed()` - Convert FWHM to arcseconds

### WCS Functions

- `pix2wcs()` - Convert pixel to world coordinates
- `wcs2pix()` - Convert world to pixel coordinates
- `has_wcs()` - Check if FITS has valid WCS
- `free_wcs()` - Free WCS data structure
- `wcs_dup()` - Duplicate WCS data
- `load_WCS_from_fits()` - Load WCS from FITS header
- `save_wcs_to_file()` - Write WCS to FITS header

### Plate Solving

- `plate_solver()` - Main plate solver entry point
- `init_astrometry_data()` - Initialize astrometry config
- `free_astrometry_data()` - Free astrometry resources
- `siril_platesolve()` - Synchronous plate solve
- `compute_image_scale()` - Calculate arcsec/pixel from optics
- `get_center_from_wcs()` - Extract center from FITS WCS

### Astrometric Registration

- `compute_Hs_from_astrometry()` - Compute homographies from WCS
- `collect_sequence_astrometry()` - Gather WCS from sequence
- `sequence_has_astrometry()` - Check if sequence has WCS data

### Image Transformation

- `cvTransformImage()` - Apply homography to image
- `cvRemap()` - Remap using coordinate maps
- `cvComputeMaps()` - Compute coordinate maps from homography
- `cvFreeMaps()` - Free coordinate maps
- `cvInvertH()` - Invert homography matrix

### Drizzle

- `drizzle_image()` - Apply drizzle algorithm for upscaling

## Error Handling

All operations return `StackingResult<T>` (alias for `Result<T, StackingError>`).

Common error types:

- `NotInitialized` - Library not initialized or FFI not available
- `ReadError` / `WriteError` - File I/O failures
- `AllocationError` - Memory allocation failed
- `EmptySequence` - No frames provided
- `DimensionsMismatch` - Frames have different sizes
- `SirilError { code, message }` - Error from Siril C code

## Thread Safety

- `FitsImage` implements `Send` but not `Sync`
- `ImageSequence` is not thread-safe
- The initialization function uses atomic operations for thread-safe one-time init
- Individual FFI calls may not be thread-safe - coordinate access appropriately

## FFI Executor

The `executor` module provides a high-level `FfiExecutor` class that integrates with the existing export pipeline infrastructure.

### ExecutorConfig

Top-level configuration for the FFI executor:

| Field        | Type                  | Description                     |
|--------------|-----------------------|---------------------------------|
| `output_32bit` | `bool`              | Use 32-bit float output         |
| `master`     | `MasterConfig`        | Master frame creation settings  |
| `calibration`| `CalibrationSettings` | Light frame calibration settings|
| `registration`| `RegistrationSettings`| Registration and plate-solving |
| `stacking`   | `StackingSettings`    | Stacking method and options     |

### CalibrationSettings

| Field              | Type                   | Default | Description                |
|--------------------|------------------------|---------|----------------------------|
| `use_bias`         | `bool`                 | `true`  | Apply bias subtraction     |
| `use_dark`         | `bool`                 | `true`  | Apply dark subtraction     |
| `use_flat`         | `bool`                 | `true`  | Apply flat division        |
| `dark_optimization`| `DarkOptimizationMode` | `Off`   | Dark scaling method        |
| `flat_normalization`| `FlatNormalizationMode`| `Auto` | Flat normalization method  |
| `debayer_method`   | `DebayerMethodOption`  | `Vng`   | Demosaicing algorithm      |
| `output_32bit`     | `bool`                 | `true`  | Output 32-bit float        |

### RegistrationSettings

| Field              | Type                 | Default   | Description                |
|--------------------|----------------------|-----------|----------------------------|
| `use_plate_solving`| `bool`               | `true`    | Use astrometric alignment  |
| `focal_length_mm`  | `f64`                | `500.0`   | Telescope focal length     |
| `pixel_size_um`    | `f64`                | `3.76`    | Camera pixel size          |
| `search_radius_deg`| `f64`                | `10.0`    | Plate solve search radius  |
| `framing`          | `FramingOption`      | `Max`     | Output framing type        |
| `interpolation`    | `InterpolationOption`| `Lanczos4`| Interpolation method       |

### StackingSettings

| Field          | Type                 | Default          | Description              |
|----------------|----------------------|------------------|--------------------------|
| `method`       | `StackingMethodOption`| `Mean`          | Stacking algorithm       |
| `rejection`    | `RejectionOption`    | `Sigma`          | Pixel rejection method   |
| `sigma_low`    | `f64`                | `2.5`            | Low sigma threshold      |
| `sigma_high`   | `f64`                | `2.5`            | High sigma threshold     |
| `normalization`| `NormalizationOption`| `AdditiveScaling`| Frame normalization     |
| `weighting`    | `WeightingOption`    | `Fwhm`           | Frame weighting method   |
| `output_32bit` | `bool`               | `true`           | Output 32-bit float      |

### FfiStepResult

Result returned by all executor operations:

| Field          | Type              | Description                       |
|----------------|-------------------|-----------------------------------|
| `success`      | `bool`            | Whether the operation succeeded   |
| `output_files` | `Vec<String>`     | Paths to created output files     |
| `error_message`| `Option<String>`  | Error message if failed           |
| `skipped`      | `bool`            | Whether step was skipped          |
| `skip_reason`  | `Option<String>`  | Reason for skipping               |
| `stats`        | `StepStats`       | Processing statistics             |

### StepStats

| Field              | Type          | Description                     |
|--------------------|---------------|---------------------------------|
| `frames_processed` | `u32`         | Number of frames processed      |
| `frames_rejected`  | `u32`         | Number of frames rejected       |
| `total_exposure_time`| `f64`       | Total exposure time (seconds)   |
| `average_fwhm`     | `Option<f64>` | Average FWHM (if measured)      |
| `processing_time_ms`| `u64`        | Processing time (milliseconds)  |

### Usage Example

```rust
use athenaeum::stacking::{
    FfiExecutor, ExecutorConfig, CalibrationSettings,
    RegistrationSettings, StackingSettings, DarkOptimizationMode,
};

// Create executor with default configuration
let config = ExecutorConfig::default();
let executor = FfiExecutor::new(config);

// Check if FFI is available
if !FfiExecutor::is_available() {
    return Err("FFI not available".into());
}

// Create master bias
let bias_paths = vec!["bias_001.fits", "bias_002.fits"];
let result = executor.create_master_bias(
    &bias_paths,
    Path::new("master_bias.fits"),
    Some(Box::new(|p, msg| println!("{:.0}% - {}", p * 100.0, msg))),
)?;

if result.skipped {
    println!("Skipped: {}", result.skip_reason.unwrap_or_default());
} else {
    println!("Created master bias with {} frames", result.stats.frames_processed);
}

// Calibrate lights
let light_paths = vec!["light_001.fits", "light_002.fits"];
let result = executor.calibrate_lights(
    &light_paths,
    Path::new("calibrated/"),
    Some(Path::new("master_bias.fits")),
    Some(Path::new("master_dark.fits")),
    Some(Path::new("master_flat.fits")),
    None, // No progress callback
)?;

// Measure quality
let (quality_result, metrics) = executor.measure_quality(&calibrated_paths, None)?;
println!("Average FWHM: {:.2}", quality_result.stats.average_fwhm.unwrap_or(0.0));

// Register frames
let result = executor.register_frames(
    &calibrated_paths,
    0, // Reference index
    Path::new("registered/"),
    None,
)?;

// Stack frames
let result = executor.stack_frames(
    &registered_paths,
    Some(&metrics),
    Path::new("stacked/"),
    None,
)?;

// Cancellation support
let token = executor.cancellation_token();
// In another thread: token.store(true, Ordering::SeqCst);
executor.cancel(); // Or use the method directly
```

## Future Work

1. **XISF output** - Add XISF write support (currently read-only)
2. **Siril sequence creation** - Create FFI sequence from file paths for stacking
