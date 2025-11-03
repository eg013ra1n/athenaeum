# LibVIPS Image Caching Technical Specification

## Overview

High-performance image caching system for Athenaeum using LibVIPS for FITS image processing with automatic AutoSTF stretching and JPEG output.

## Architecture

### System Components

```
┌─────────────────────────────────────────────────────────────┐
│                     Tauri Frontend                           │
│  (BlinkViewer.tsx)                                          │
└────────────┬────────────────────────────────────────────────┘
             │ invoke() commands
             ▼
┌─────────────────────────────────────────────────────────────┐
│                  Rust Backend (Tauri)                        │
│  ┌─────────────────────────────────────────────────────┐    │
│  │            Cache Manager (Facade)                   │    │
│  │  - Request handling                                 │    │
│  │  - Priority queue management                        │    │
│  │  - Cache lookup                                     │    │
│  └─────────────┬───────────────────────────────────────┘    │
│                │                                             │
│  ┌─────────────▼───────────────┬─────────────────────┐      │
│  │   VipsProcessor (FFI)        │   Worker Pool       │      │
│  │  - LibVIPS bindings          │  - Tokio runtime    │      │
│  │  - FITS → JPEG pipeline      │  - Job scheduling   │      │
│  │  - Multi-resolution          │  - Parallel workers │      │
│  └──────────────────────────────┴─────────────────────┘      │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐    │
│  │            Cache Storage                            │    │
│  │  - SQLite (cache.db) for metadata                   │    │
│  │  - Filesystem for JPEG files                        │    │
│  │  - Memory cache for hot images                      │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## AutoSTF Algorithm

### PixInsight-Compatible Screen Transfer Function

The AutoSTF implementation follows PixInsight's algorithm for automatic histogram stretching:

1. **Statistics Calculation**
   - Calculate median (m) and MAD (Median Absolute Deviation)
   - Scale MAD by 1.4826 for normal distribution equivalence

2. **Black Point Calculation**
   ```
   c0 = median + k * MAD * 1.4826
   where k = -2.8 (shadows clipping parameter)
   ```

3. **White Point Calculation**
   - Use 99.9th percentile of the histogram

4. **Midtones Transfer Function**
   ```
   MTF(x, m) = ((m - 1) * x) / ((2m - 1) * x - m)
   where m = 0.25 (target median)
   ```

### RGB Linked Stretching

For color images:
- Calculate combined statistics from all channels
- Apply same stretch parameters to preserve color balance
- Optional per-channel stretching for advanced users

## Performance Specifications

### Processing Pipeline Performance

| Stage | Time (4K Image) | Optimization |
|-------|----------------|--------------|
| FITS Load | 50ms | Memory mapping |
| Statistics | 20ms | Parallel histogram |
| AutoSTF Calc | 15ms | SIMD operations |
| Debayer | 30ms | Parallel processing |
| Stretch Apply | 15ms | LUT-based |
| JPEG Encode | 35ms | libjpeg-turbo |
| **Total** | **120-150ms** | 5-7x improvement |

### Memory Usage

- LibVIPS streaming: ~50MB per 4K image
- LRU memory cache: Configurable (default 100MB)
- Worker pool overhead: ~10MB per worker

### Storage Efficiency

| Format | Size (4K) | Quality | Speed |
|--------|-----------|---------|--------|
| PNG (current) | 16MB | Lossless | Slow |
| JPEG Q85 | 2-3MB | Excellent | Fast |
| JPEG Q70 (thumb) | 50KB | Good | Very Fast |

## Implementation Details

### VipsProcessor Module Structure

```rust
pub struct VipsProcessor {
    context: VipsContext,
    params: ProcessorParams,
}

pub struct ProcessorParams {
    pub jpeg_quality: u8,
    pub autostf: AutoSTFParams,
    pub resolutions: Vec<Resolution>,
}

pub struct AutoSTFParams {
    pub shadows_clipping: f32,  // -2.8 default
    pub target_median: f32,      // 0.25 default
    pub use_rgb_linking: bool,   // true default
}
```

### Worker Pool Configuration

```rust
pub struct WorkerPoolConfig {
    pub num_workers: usize,      // CPU cores
    pub queue_size: usize,       // 1000 default
    pub memory_limit: usize,     // 100MB default
    pub priorities: Vec<Priority>,
}

pub enum Priority {
    Immediate,  // User-requested
    High,       // Prefetch adjacent
    Normal,     // Batch processing
    Low,        // Background warming
}
```

### Cache Key Generation

Cache keys include all parameters affecting the output:
- File path + modification time
- AutoSTF parameters (or manual stretch)
- Resolution (thumb/preview/full)
- JPEG quality

Example: `{xxh3_64_hash}.jpg` where hash includes all parameters

### Cross-Platform Setup

#### macOS
```bash
brew install vips
# Includes libjpeg-turbo, libfitsio
```

#### Windows
```toml
[target.'cfg(windows)'.dependencies]
libvips = { version = "1.0", features = ["vendored"] }
```

#### Linux
```bash
# Debian/Ubuntu
apt-get install libvips-dev

# RHEL/Fedora
dnf install vips-devel
```

## API Specification

### Tauri Commands

```rust
#[tauri::command]
async fn get_cached_image_vips(
    file_path: String,
    resolution: String,
    stretch_mode: StretchMode,
) -> Result<Vec<u8>, String>

#[tauri::command]
async fn warm_cache_batch(
    file_paths: Vec<String>,
    priority: Priority,
) -> Result<CacheWarmingHandle, String>

#[tauri::command]
async fn get_cache_stats() -> Result<CacheStats, String>
```

### Frontend Integration

```typescript
interface ProcessedImage {
  thumbnail: Uint8Array;  // 256x256 JPEG
  preview: Uint8Array;    // 1024x1024 JPEG
  full: Uint8Array;       // Full resolution JPEG
  stretchParams: AutoSTFResult;
  metadata: ImageMetadata;
}

interface AutoSTFResult {
  blackPoint: number;
  whitePoint: number;
  midtones: number;
  median: number;
  mad: number;
}
```

## Migration Strategy

1. **Phase 1**: Add LibVIPS alongside existing code
2. **Phase 2**: Dual support (PNG + JPEG)
3. **Phase 3**: Migrate existing cache
4. **Phase 4**: Remove old PNG code

## Testing Strategy

### Performance Tests
- Benchmark FITS → JPEG conversion
- Measure memory usage under load
- Test parallel processing efficiency

### Quality Tests
- Compare AutoSTF with PixInsight
- Verify color preservation
- Test edge cases (saturated, very dark images)

### Integration Tests
- Cross-platform builds
- Cache consistency
- Worker pool resilience

## Future Enhancements

1. **GPU Acceleration** (CUDA/Metal/Vulkan)
2. **WebP Support** (better compression)
3. **Adaptive Quality** (based on image content)
4. **Cloud Cache** (shared processing)
5. **AVIF Format** (next-gen compression)

## References

- [LibVIPS Documentation](https://www.libvips.org/API/current/)
- [PixInsight AutoSTF](https://pixinsight.com/doc/tools/AutoHistogram/AutoHistogram.html)
- [libjpeg-turbo](https://libjpeg-turbo.org/)
- [FITS Standard](https://fits.gsfc.nasa.gov/fits_standard.html)