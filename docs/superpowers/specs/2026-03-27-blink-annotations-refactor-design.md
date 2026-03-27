# Blink Annotations Refactor: Client-Side Rendering + Unified Analysis

**Date**: 2026-03-27
**Status**: Approved

## Problem

The blink viewer's annotation system has two architectural issues:

1. **Slow annotation loading**: The annotated image path does FITS-to-RGB conversion + full star analysis + annotation burning + JPEG encoding -- all inside `block_in_place()` on a contended rayon pool. With N concurrent requests, each holds a tokio worker thread while competing for the shared pool. This is triple the work of a plain image.

2. **Duplicated, divergent analysis paths**:
   - `analyze_frame_set` (batch) -- persists aggregates to `frame_analysis`, uses worker-queue threading
   - `read_fits_image_annotated` (blinker) -- runs the same analysis per-frame on-the-fly, burns results into a JPEG, persists nothing
   - These share no data. Batch-analyzed frames are re-analyzed when viewed in the blinker.

## Solution

Three changes:

1. **Client-side annotation rendering**: Return star position data instead of burning circles into a second JPEG. Draw ellipses on a canvas overlay in the frontend.
2. **Persist per-star data**: Store individual star metrics in a new `star_metrics` table alongside existing `frame_analysis` aggregates.
3. **Unify analysis**: One analysis codepath that always persists. The blinker loads from DB if available, triggers on-demand single-frame analysis if not.

## Design

### 1. Database Schema

New table for per-star data, loaded on-demand for annotation rendering:

```sql
CREATE TABLE IF NOT EXISTS star_metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    frame_analysis_id INTEGER NOT NULL,
    x REAL NOT NULL,
    y REAL NOT NULL,
    peak REAL NOT NULL,
    flux REAL NOT NULL,
    fwhm REAL NOT NULL,
    fwhm_x REAL NOT NULL,
    fwhm_y REAL NOT NULL,
    eccentricity REAL NOT NULL,
    snr REAL NOT NULL,
    hfr REAL NOT NULL,
    theta REAL NOT NULL,
    beta REAL,
    fit_method TEXT NOT NULL,
    fit_residual REAL NOT NULL,
    FOREIGN KEY (frame_analysis_id) REFERENCES frame_analysis(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_star_metrics_analysis_id ON star_metrics(frame_analysis_id);
```

No changes to `frame_analysis` -- it keeps existing aggregate medians. Cascading delete cleans up star rows when a `frame_analysis` is deleted.

**Scale**: 2,000 frames x 500 stars = 1M rows. SQLite handles this comfortably (~120-150MB, indexed lookups <1ms, bulk insert ~5ms per frame in a transaction).

### 2. Unified Analysis Pipeline

#### Core function

`analyze_and_persist(conn, path, frame_id, file_id, analyzer, config_hash) -> Result<FrameAnalysis>`:
- Runs `analyzer.analyze(path)` (star detection + PSF fitting)
- Maps result to `FrameAnalysis` + `Vec<StarMetrics>`
- Upserts `frame_analysis` row
- Deletes old `star_metrics` for that `frame_analysis_id`, bulk-inserts new ones
- Returns `FrameAnalysis`

#### Entry point 1: `analyze_frame_set` (batch)

Same worker-queue threading model (`std::thread::scope` with N workers). Calls `analyze_and_persist` per frame. Computes quality scores across batch afterward. Now also persists per-star data.

#### Entry point 2: `get_frame_star_metrics(frame_id) -> StarMetricsResponse`

New command (Tauri + web route):
- Checks `star_metrics` table for existing data
- If found and `config_hash` matches current config: return from DB (instant)
- If stale or missing: run single-frame analysis via `analyze_and_persist`, return result
- Response: `{ stars: Vec<StarMetric>, metrics: FrameAnalysis, image_width: u32, image_height: u32 }`

Image dimensions are included so the frontend can scale star coordinates from FITS pixel space to canvas space.

### 3. Frontend Annotation Rendering

#### Canvas architecture

Two-layer canvas stack (CSS `position: absolute`, same dimensions):

- **Base canvas** (unchanged): Draws plain JPEG with zoom/pan via existing `drawImageToCanvas()`
- **Overlay canvas** (new): Draws star ellipses using the same zoom/pan transform state from refs

Both share `zoomRef`, `panRef`, and image-fitting math (`baseWidth`, `baseHeight`, `offsetX`, `offsetY`). When the user pans/zooms, both redraw together. The overlay clears and redraws circles independently of the base image -- no need to re-decode the JPEG blob.

#### Drawing logic

When `showAnnotations` is true and star data is available:

```
for each star in starMetrics:
    // Scale from FITS pixel space to canvas space
    canvasX = offsetX + (star.x / imageWidth) * renderWidth
    canvasY = offsetY + (star.y / imageHeight) * renderHeight
    radiusX = (star.fwhm_x / imageWidth) * renderWidth * scaleFactor
    radiusY = (star.fwhm_y / imageHeight) * renderHeight * scaleFactor

    // Draw ellipse colored by annotation_settings.color_scheme
    ctx.ellipse(canvasX, canvasY, radiusX, radiusY, star.theta, 0, 2*PI)
    ctx.stroke()

    // Direction tick (theta indicator)
    if (annotationSettings.show_direction_tick):
        tickLength = radiusX * 0.5
        edgeX = canvasX + radiusX * cos(star.theta)
        edgeY = canvasY + radiusX * sin(star.theta)
        tipX = edgeX + tickLength * cos(star.theta)
        tipY = edgeY + tickLength * sin(star.theta)
        ctx.moveTo(edgeX, edgeY)
        ctx.lineTo(tipX, tipY)
        ctx.stroke()
```

Color grading follows existing `AnnotationSettings`: eccentricity-based (green/yellow/red thresholds), FWHM-based, or uniform color.

#### Data loading flow

1. User toggles annotations on
2. For current frame, call `get_frame_star_metrics(frame_id)`
3. If DB has fresh data: instant return, draw overlay
4. If not: backend analyzes on-demand, persists, returns
5. Star data cached in frontend `Map<number, StarMetricsResponse>` (lightweight -- just coordinate arrays, not image data)
6. Pre-fetch adjacent frames' star data in background by proximity

#### Changes to useBlinkCache

Simplifies significantly:
- Removes all `"annotated"` job type logic and `bgType` priority lane
- Only manages plain image loading
- A separate lighter `useStarMetricsCache` hook pre-fetches star data by proximity

### 4. Cleanup

#### Rust -- removed

| File | Removed |
| ---- | ---- |
| `athenaeum-core/src/rustafits_processor/mod.rs` | `process_fits_to_jpeg_annotated()`, `AnnotatedImageResult`, `AnnotationMetrics` struct |
| `athenaeum-tauri/src/commands_rustafits.rs` | `read_fits_image_annotated` command |
| `athenaeum-tauri/src/lib.rs` | Remove from `invoke_handler` |
| `athenaeum-web/src/routes/images.rs` | Annotated image endpoint |
| `athenaeum-core/src/services/mod.rs` | `annotation_metrics` field (in-memory HashMap cache) |

#### Rust -- modified

| File | Change |
| ---- | ---- |
| `AnnotationSettings` struct | Stays (frontend needs it for display prefs). Remove `to_rustafits_config()` -- no longer calling rustafits annotator. |
| `AnnotationMetrics` | Removed as separate struct. `FrameAnalysis` already carries all aggregate fields. |

#### Frontend -- removed

| File | Removed |
| ---- | ---- |
| `BlinkViewer.tsx` | `annotatedImages` Map, `loadAnnotatedImage()`, annotated branch in rendering |
| `useBlinkCache.ts` | All `"annotated"` job logic |

#### Frontend -- new

| File | Added |
| ---- | ---- |
| `useStarMetricsCache.ts` | Lightweight pre-fetcher for star data by proximity |
| Star overlay drawing logic | Canvas overlay -- ellipses, color grading, direction ticks |
| `types/models.ts` | `StarMetric` interface, `StarMetricsResponse` interface |

#### API layer (`src/api/`)

- Remove `read_fits_image_annotated` call
- Add `get_frame_star_metrics` call (Tauri IPC + HTTP route)

## Performance Impact

| Metric | Before | After |
| ---- | ---- | ---- |
| Toggle annotations: first frame | ~2-5s (full analysis + JPEG encode) | <1ms if batch-analyzed, ~2-3s if on-demand (one-time) |
| Toggle annotations: cached frame | <1ms (memory cache hit) | <1ms (DB query + canvas draw) |
| Annotation drawing | N/A (burned into JPEG) | <1ms (canvas ellipse rendering, ~500 draws) |
| Memory: annotated images | ~200KB per frame (second JPEG set) | ~20KB per frame (star coordinate arrays) |
| Backend concurrency | 2x rayon pool pressure (convert + analyze per request) | 1x (convert only; analysis is DB lookup) |
| Batch analysis write time | ~unchanged per frame | +~5ms per frame (star_metrics insert) |

## Future Capabilities Unlocked

Per-star data in a queryable table enables:
- Field curvature analysis (eccentricity/FWHM as function of distance from center)
- Star drift tracking across frames in a session
- Per-region focus quality maps
- Filtering frames by corner-star metrics
- Cross-session optical performance comparison
