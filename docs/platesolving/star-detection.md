# Star Detection

The plate solver's Step 1 delegates entirely to the rustafits crate's `ImageAnalyzer`. This document describes both code paths — the default **fast detector** and the legacy precise analyser — used from `crates/athenaeum-core/src/plate_solve/service.rs`.

**Default production path** (`use_fast_detection = true`) — calls `ImageAnalyzer::detect_fast`, takes **~400 ms release / ~5 s debug** on a 6248 × 4176 frame, returning `(x, y, flux)` triples only. No PSF metrics.

**Legacy precise path** (`use_fast_detection = false`) — calls `ImageAnalyzer::analyze`, the full DAOFIND-style pipeline described below. Used only when the user explicitly disables fast detection for diagnostic comparison; the solver gets no measurable accuracy win from it.

## Invocation (default fast path)

```rust
let max_detection_cap = config
    .retry_passes
    .iter()
    .copied()
    .max()
    .unwrap_or(config.max_image_stars)
    .max(500);

let mut analyzer = ImageAnalyzer::new()
    .with_detection_sigma(5.0)
    .with_max_stars(max_detection_cap);
if let Some(pool) = thread_pool {
    analyzer = analyzer.with_thread_pool(pool);
}
let r = analyzer.detect_fast(file_path)?; // FastAnalysisResult
```

`max_stars` is sized to fit the largest retry pass (default 600, from `retry_passes = [50, 150, 300, 600]`), with a 500-star floor. `detection_sigma` is 5.0 — "detect peaks 5σ above local noise". Every other knob (min/max area, saturation limit, etc.) uses rustafits defaults.

`detect_fast` runs: FITS/XISF decode → green-channel debayer for Bayer OSC → mesh-grid background → single-pass separable Gaussian matched-filter convolution → 5σ peak detection → connected-component centroid extraction. **No** Moffat PSF fit, **no** two-pass FWHM calibration, **no** per-star SNR photometry. Output: `FastAnalysisResult { width, height, stars: Vec<FastStar { x, y, peak, flux }>, background, noise, timing }`.

## Legacy precise pipeline (below)

The sections below describe `ImageAnalyzer::analyze` — the full precise pipeline used by the analysis tab and available as a fallback via `use_fast_detection = false`. The fast detector reuses stages 1–4 of this pipeline and skips 5–8.

## Pipeline Stages

All stages are in `rustafits/src/analysis/`.

### 1. Decode (`formats/fits.rs`, `formats/xisf.rs`)

Read the FITS or XISF file into a single-channel f32 buffer.

- FITS: big-endian `fitsio`-free reader that supports u16 and f32 `BITPIX`.
- XISF: little-endian reader with zlib / LZ4 / Zstd decompression.
- OSC / Bayer frames are debayered with a 2 × 2 super-pixel scheme in `processing/debayer.rs`.
- The resulting buffer is always planar f32. u16 input is scaled to `[0, 65535]`; XISF f32 `[0, 1]` is scaled to match the FITS range.

### 2. Background and noise estimation (`background.rs`)

Produces a per-cell background value and per-cell noise sigma, so the detection threshold adapts to vignetting, gradients, and nebulosity instead of using a single global value.

- The image is divided into a mesh of cells (`auto_cell_size()` picks the side based on image dimensions).
- Each cell's background is the median of its pixels; noise is the MAD (median absolute deviation) scaled by 1.4826.
- Optional **MRS wavelet decomposition** (`with_mrs_layers(n)`, default 0) estimates noise across multiple scales for better behavior in nebulosity. Not used by the plate solver today but available.
- Cells are interpolated to produce full `bg_map` and `noise_map` arrays aligned with the image.
- Mesh cells are computed in parallel via rayon.

### 3. Matched-filter convolution (`convolution.rs`)

The matched filter is a 2D Gaussian with `σ = FWHM / 2.3548`. This boosts SNR for point sources at the expected FWHM and suppresses noise at other scales.

- The kernel is separable, so it's applied as a 1D horizontal pass followed by a 1D vertical pass.
- Both passes are SIMD-accelerated (SSE2 baseline, AVX2 runtime detection on x86_64, NEON on aarch64) and parallelised row-by-row with rayon.
- Kernel radius scales with the initial guessed FWHM.

### 4. Peak detection (`detection.rs::detect_stars()`)

Runs across the convolved image to find candidate star centers.

- **Adaptive threshold** per pixel: `detection_sigma × noise_map[y, x] × √Σ kernel²`. With `detection_sigma = 5.0`, any pixel above 5σ of local noise (after matched filtering) is a candidate.
- **8-neighbor maximum test** — the candidate must strictly exceed all 8 direct neighbors in the convolved image.
- **Wing sanity check** — at least 3 of those 8 neighbors must themselves be above threshold. Real PSFs have wings; isolated noise peaks and hot pixels don't. This rejects cosmic rays, single-pixel defects, and shot-noise spikes.
- **Parallelised** — each row of the image runs on a separate rayon worker, reading only read-only slices of the convolution buffer.
- Saturated pixels (above `saturation_limit`, default 0.95 × 65535) are flagged so saturated stars are excluded from centroid computation.

### 5. Centroid extraction

For each surviving peak, the detector runs a small connected-component / weighted-moments calculation:

- Grow the component outward from the peak while pixels remain above threshold.
- Compute the intensity-weighted first moment to get a subpixel `(x, y)` centroid.
- Reject components whose pixel count is outside `[min_star_area, max_star_area]` (defaults 5 and 2000).
- Second-order moments give a rough eccentricity and position angle, used later for shape-based rejection of trailed or blended stars.

### 6. Two-pass FWHM calibration (`mod.rs::analyze_raw`)

The first pass used a guessed FWHM for the matched filter kernel. The detector now has a sample of real stars — it uses them to calibrate the actual field FWHM and re-runs detection with the correct kernel.

- **Pass 1**: detect with guessed FWHM, pick ~100 "calibration stars" with good shape and SNR, fit 2D Moffat profiles to each.
- Compute the median FWHM across the calibration set → `calibrated_fwhm`.
- **Pass 2**: re-run convolution and peak detection with the calibrated kernel. Existing detections are kept unchanged; new peaks that only appear under the calibrated kernel are added.

This two-pass scheme matters most when the image FWHM differs significantly from the default guess (e.g., out-of-focus or very sharp images).

### 7. Per-star PSF fitting (`fitting.rs`, `metrics.rs`)

For each detected star (up to `measure_cap`, default 500), the analyzer fits a 2D profile via Levenberg-Marquardt. The default fit method is Moffat with a fixed β (using the field median), but it tries free-β Moffat first on a calibration subset to determine β.

Fit methods (`FitMethod`):
- `FreeMoffat` — 8 parameters, highest accuracy, used for calibration.
- `FixedMoffat` — 7 parameters, uses the field β, used for the bulk of stars.
- `Gaussian` — 7 parameters, fallback when Moffat doesn't converge.
- `Moments` — windowed second-moment estimate, used when LM fails completely.

Each star ends up with:
- Subpixel centroid `(x, y)`
- Peak value, total flux
- FWHM in x and y (and the combined FWHM)
- Eccentricity `(FWHM_x − FWHM_y) / FWHM_x`
- Position angle (the PSF orientation)
- SNR
- HFR (half-flux radius)
- Fit residual (as a confidence metric)
- Optional arcsec values if `with_optics(focal_length_mm, pixel_size_um)` was set

### 8. Proximity and shape rejection

After fitting, the analyzer throws away stars that are too close to a brighter neighbor (greedy NMS with a minimum separation in FWHM units) and stars whose shape is badly degraded (too elongated or too far from the field median FWHM). This keeps the returned star list clean enough for plate solving to work without extra filtering downstream.

### 9. Return

`analyzer.analyze()` returns an `AnalysisResult` with:

```rust
pub struct AnalysisResult {
    pub width: usize,
    pub height: usize,
    pub background: f32,
    pub noise: f32,
    pub detection_threshold: f32,
    pub stars_detected: usize,
    pub stars: Vec<StarMetrics>,   // sorted by flux descending, capped at max_stars
    pub median_fwhm: f32,
    pub median_eccentricity: f32,
    pub median_snr: f32,
    pub median_hfr: f32,
    pub measured_fwhm_kernel: f32,
    pub calibrated_fwhm: f32,
    pub stars_measured: usize,
    pub stage_timing: StageTiming,
    // ... and more
}
```

The plate solver only reads `width`, `height`, and `stars` (specifically `stars[i].x`, `stars[i].y`, `stars[i].flux`). Everything else is ignored.

## Why the Precise Pipeline Was Expensive

`ImageAnalyzer::analyze` takes ~6 s (debug) / ~500 ms (release) because it does a lot more work than the solver strictly needs:

1. **Background mesh + optional MRS wavelet noise**: hundreds of ms for a 6k × 4k frame.
2. **Separable convolution**: ~2× because a two-pass FWHM calibration runs it twice.
3. **LM PSF fits**: 500 stars × a few dozen iterations each × a small window = several seconds in total.
4. **Shape/proximity filtering**: requires all the above metrics.
5. **Per-star SNR photometry, trail detection, stats**: another few hundred ms.

For plate solving we genuinely only need centroids and a brightness ordering. FWHM, eccentricity, SNR, HFR, fit residual, trail detection, and the two-pass calibration are all wasted work.

## Fast Detection (shipped)

The "fast detection-only" mode proposed here ships as `ImageAnalyzer::detect_fast` / `detect_fast_data` / `detect_fast_raw` in rustafits. The solver enables it via `use_fast_detection = true` (default). It keeps stages 1–4 of the precise pipeline (decode → debayer → background → convolution → peak detection → centroid) and skips stages 5–8 (PSF calibration, LM fits, SNR, stats, trail detection).

Measured speedup on the Lemmon reference frame (release, M1):
- Precise pipeline: ~1.5 s (analyze)
- Fast pipeline: ~0.4 s (detect_fast)
- ~4× speedup, ~75% of total per-frame budget recovered.

## Debug-mode visualisation

The `debug_plate_solve_viz` example binary writes a PNG of the **exact green-interpolated luminance image the detector runs peak-detection against**, with detected-star circles overlaid. Useful when the solver reports no match and you want to see whether detection quality is the bottleneck. See the main README's **Debug & Diagnostic Tools** section.

## Related Documents

- [`README.md`](./README.md) — the full plate-solving pipeline that consumes the detected stars.
