# Rustafits Optimization Recommendations for Athenaeum

**Date**: 2026-03-27
**From**: Athenaeum development analysis
**Context**: Batch analysis of 2000+ astronomical frames (6252x4176 mono, QHY268M) and on-demand blink viewer analysis

---

## Executive Summary

Athenaeum uses rustafits `ImageAnalyzer` for star detection, PSF fitting, and quality metrics. After profiling the batch analysis pipeline, we identified three high-impact optimizations in rustafits that would significantly improve throughput for Athenaeum's use cases.

The current architecture — N worker threads calling `pool.install(|| analyze_impl())` on a shared rayon pool — is fundamentally sound. The bottlenecks are inside the per-frame analysis pipeline.

---

## Recommendation 1: Parallelize MRS Noise Estimation

**Priority**: High
**Estimated Impact**: ~500ms → ~100-150ms per frame on 8 cores

### Problem

`background::estimate_noise_mrs()` runs wavelet transforms sequentially. On a 6252x4176 image with `noise_layers=4`, this takes ~500ms of single-threaded work. During this time, the rayon pool threads sit idle — only the calling worker thread is active.

This is the largest sequential bottleneck in the analysis pipeline. All other expensive stages (background mesh, convolution, PSF fitting, SNR) already use `par_iter()` or `par_chunks_mut()`.

### Suggestion

The MRS wavelet computation processes layers independently in each direction. The row and column transforms within each layer can be parallelized with `par_chunks_mut()`, matching the pattern already used in `convolution.rs:59-77`:

```rust
// Current (sequential):
for layer in 0..n_layers {
    for y in 0..height {
        convolve_row(...);  // row transform
    }
    for x in 0..width {
        convolve_col(...);  // column transform
    }
}

// Suggested (parallel):
for layer in 0..n_layers {
    buffer.par_chunks_mut(width).for_each(|row| {
        convolve_row(row, ...);
    });
    // Column pass needs transposition or vertical par_chunks
}
```

### Context

Athenaeum's batch analysis processes 200-2000 frames per target. With `batch_concurrency=3`, three frames are analyzed concurrently. Each frame spends ~500ms in MRS noise estimation where only 1 of 8 pool threads is active. Parallelizing this stage would recover ~350ms per frame × 2000 frames = ~12 minutes saved per full batch.

---

## Recommendation 2: `analyze_data()` API for Pre-Loaded Pixel Data

**Priority**: High
**Estimated Impact**: Enables I/O overlap, ~50-100ms hidden per frame on HDD/NAS

### Problem

The current `ImageAnalyzer::analyze(path)` method reads the file internally via `formats::read_image(path)`. This couples I/O with CPU analysis inside `pool.install()`, meaning:

1. A rayon pool slot is consumed during the disk read (~50-100ms for a 50MB FITS file)
2. The caller cannot overlap I/O of frame N+1 with analysis of frame N
3. On network storage (NAS/SMB), I/O latency can be 200-500ms per file

### Suggestion

Add a public method that accepts pre-loaded data:

```rust
impl ImageAnalyzer {
    /// Analyze pre-loaded image data.
    /// `meta` provides width, height, channels, bayer_pattern.
    /// `pixel_data` is the raw pixel values (u16 or f32).
    pub fn analyze_data(
        &self,
        meta: ImageMetadata,
        pixel_data: PixelData,
    ) -> Result<AnalysisResult> {
        // Same as analyze_impl() but skips formats::read_image()
        let f32_data = match pixel_data {
            PixelData::Float32(d) => d,
            PixelData::Uint16(d) => u16_to_f32(&d),
        };
        // ... debayer check, then run_analysis()
    }
}
```

This would also require making `ImageMetadata` and `PixelData` public if they aren't already.

### Context

Athenaeum processes frames from user-configured directories that may be on local SSDs, HDDs, or network mounts. With `analyze_data()`, Athenaeum can pre-read files in a dedicated I/O thread and feed the data to workers, fully decoupling I/O from CPU work:

```rust
// I/O thread reads ahead
let (meta, pixels) = formats::read_image(path)?;
// Worker thread analyzes (no I/O wait)
let result = analyzer.analyze_data(meta, pixels)?;
```

---

## Recommendation 3: Half-Resolution Analysis Mode

**Priority**: Medium-High
**Estimated Impact**: ~4x faster analysis for preview/overlay use cases

### Problem

Athenaeum's blink viewer displays frames at "preview" resolution (2x2 binned, ~half dimensions). When the user toggles star annotations, the viewer needs star positions for overlay rendering. Currently it runs full-resolution analysis (6252x4176), but the displayed image is only 3126x2088.

Full-resolution analysis takes 2-3 seconds per frame. For on-demand analysis (frame not previously batch-analyzed), this creates a noticeable delay when toggling annotations.

### Suggestion

Add a `with_analysis_downscale(factor)` configuration:

```rust
impl ImageAnalyzer {
    /// Set downscale factor for analysis. Factor of 2 bins 2x2 before
    /// detection/measurement, reducing analysis time by ~4x.
    /// Star coordinates will be in the downscaled pixel space.
    pub fn with_analysis_downscale(mut self, factor: usize) -> Self {
        self.config.analysis_downscale = factor;
        self
    }
}
```

Inside `analyze_impl()`, apply binning before `run_analysis()`:

```rust
let (data, width, height) = if self.config.analysis_downscale > 1 {
    let (binned, nw, nh) = binning::bin_2x2_float(&f32_data, width, height);
    (binned, nw, nh)
} else {
    (f32_data, width, height)
};
```

Star coordinates in the result would be in the downscaled space, and `result.width/height` would reflect the downscaled dimensions. This aligns naturally with the annotation coordinate transform in `compute_annotations()`.

### Context

Athenaeum has two analysis paths:
1. **Batch analysis** — runs on all frames in a target, stores results to DB. Full resolution is appropriate here.
2. **On-demand analysis** — triggered when the user views a frame in the blinker that hasn't been batch-analyzed. Speed matters more than sub-pixel accuracy.

With half-resolution analysis, on-demand analysis would complete in ~0.5-0.7 seconds instead of 2-3 seconds, making the blink viewer feel responsive.

---

## Lower Priority Suggestions

### 4. Expose Per-Stage Timing

Adding optional timing instrumentation (behind a feature flag or config option) would help downstream applications profile their specific workloads:

```rust
pub struct AnalysisResult {
    // ... existing fields ...
    /// Per-stage timing in milliseconds (None if timing disabled)
    pub stage_timing: Option<StageTiming>,
}

pub struct StageTiming {
    pub io_ms: u64,
    pub background_ms: u64,
    pub noise_ms: u64,
    pub detection_pass1_ms: u64,
    pub calibration_ms: u64,
    pub detection_pass2_ms: u64,
    pub measurement_ms: u64,
    pub snr_ms: u64,
}
```

### 5. Batch-Analyze API

For batch processing, a method that accepts an iterator of paths and handles worker orchestration internally would simplify callers and enable internal optimizations (e.g., shared background models across frames taken close together):

```rust
impl ImageAnalyzer {
    pub fn analyze_batch<P: AsRef<Path>>(
        &self,
        paths: impl Iterator<Item = P>,
        concurrency: usize,
        progress: impl Fn(usize, usize) + Send + Sync,
    ) -> Vec<Result<AnalysisResult>> { ... }
}
```

This would let rustafits own the worker thread management and apply optimizations that callers can't easily do (e.g., reusing memory allocations across frames).

---

## Current Performance Baseline

Measured on Athenaeum with QHY268M frames (6252x4176, mono, 16-bit):

| Stage | Time (approx) | Parallel? |
| ---- | ---- | ---- |
| File I/O | 50-100ms | No |
| u16 → f32 | 10-20ms | No |
| Background mesh | 100-150ms | Yes (cells) |
| MRS noise | 400-500ms | **No** |
| Pass 1 detection | 200-300ms | Partial (conv yes, CC no) |
| Calibration PSF | 50-100ms | Yes (100 stars) |
| Pass 2 detection | 200-300ms | Partial |
| PSF measurement | 500-1500ms | Yes (up to 500 stars) |
| SNR computation | 50-100ms | Yes (per-star) |
| Statistics | 5-10ms | No |
| **Total** | **~1.5-3.0s** | |

With `batch_concurrency=3` on 8-core system: ~3 frames overlap, achieving ~1.0-1.5 frames/second throughput.
