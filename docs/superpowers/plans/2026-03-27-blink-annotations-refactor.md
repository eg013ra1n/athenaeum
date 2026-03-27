# Blink Annotations Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace server-side annotation burning with client-side canvas overlay drawing, unify the two analysis codepaths, and persist per-star data to a new `star_metrics` table.

**Architecture:** The backend gains a `star_metrics` DB table and a new `get_frame_star_metrics` command that returns star positions from DB (or runs on-demand analysis). The frontend draws star ellipses on a second canvas layer using the same zoom/pan transforms. The `read_fits_image_annotated` command and `process_fits_to_jpeg_annotated` function are removed entirely.

**Tech Stack:** Rust (rusqlite, rayon, tokio, serde), TypeScript/React (Canvas API), Tauri IPC, Axum (web backend)

---

## File Map

### New files

| File | Responsibility |
| ---- | ---- |
| `src/hooks/useStarMetricsCache.ts` | Priority-queue pre-fetcher for star metrics by proximity to current frame |
| `src/components/blink/StarOverlay.ts` | Pure functions for drawing star ellipses, direction ticks, and color grading on a canvas |

### Modified files

| File | Change summary |
| ---- | ---- |
| `crates/athenaeum-core/src/db/schema.rs` | Add `star_metrics` table creation + migration |
| `crates/athenaeum-core/src/db/analysis.rs` | Add star_metrics CRUD functions |
| `crates/athenaeum-core/src/models.rs` | Add `StarMetric` struct |
| `crates/athenaeum-core/src/analysis/analyzer.rs` | Return `Vec<StarMetric>` alongside `FrameAnalysis` |
| `crates/athenaeum-core/src/services/mod.rs` | Remove `annotation_metrics` field |
| `crates/athenaeum-core/src/rustafits_processor/mod.rs` | Remove `process_fits_to_jpeg_annotated`, `AnnotatedImageResult`, `AnnotationMetrics`, `to_rustafits_config()` |
| `crates/athenaeum-tauri/src/commands/analysis.rs` | Add `get_frame_star_metrics` command; update `analyze_frame_set` and `analyze_single_frame` to persist stars |
| `crates/athenaeum-tauri/src/commands_rustafits.rs` | Remove `read_fits_image_annotated` |
| `crates/athenaeum-tauri/src/lib.rs` | Remove `read_fits_image_annotated` from handler; add `get_frame_star_metrics`; remove `annotation_metrics` init |
| `crates/athenaeum-web/src/routes/analysis.rs` | Add `get_frame_star_metrics` route; update batch/single to persist stars |
| `crates/athenaeum-web/src/routes/images.rs` | Remove `read_fits_image_annotated` endpoint |
| `crates/athenaeum-web/src/routes/mod.rs` | Swap route registration |
| `crates/athenaeum-web/src/main.rs` | Remove `annotation_metrics` init |
| `src/types/models.ts` | Add `StarMetric`, `StarMetricsResponse`; remove `AnnotationMetrics`, `AnnotatedImageResponse` |
| `src/components/blink/types.ts` | Update `FrameInfoPanelProps` to use `FrameAnalysis` |
| `src/components/blink/FrameInfoPanel.tsx` | Read metrics from `FrameAnalysis` instead of `AnnotationMetrics` |
| `src/components/BlinkViewer.tsx` | Remove annotated image state/loading; add overlay canvas + star metrics integration |
| `src/hooks/useBlinkCache.ts` | Remove all annotated job logic |

---

## Task 1: Database Schema — `star_metrics` Table

**Files:**

- Modify: `crates/athenaeum-core/src/db/schema.rs:682-691`

- [ ] **Step 1: Add star_metrics table creation after frame_analysis indexes**

In `crates/athenaeum-core/src/db/schema.rs`, after the `idx_frame_analysis_file_id` index creation (line 691), add:

```rust
    // Per-star metrics table — individual star detection results for overlay rendering
    conn.execute(
        "CREATE TABLE IF NOT EXISTS star_metrics (
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
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_star_metrics_analysis_id ON star_metrics(frame_analysis_id)",
        [],
    )?;
```

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/athenaeum-core && cargo check`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs
git commit -m "feat: add star_metrics table for per-star detection data"
```

---

## Task 2: Rust Model — `StarMetric` Struct

**Files:**

- Modify: `crates/athenaeum-core/src/models.rs:734`

- [ ] **Step 1: Add StarMetric struct after FrameAnalysis**

In `crates/athenaeum-core/src/models.rs`, after the `FrameAnalysis` struct closing brace (line 734), add:

```rust

/// Individual star detection result with position, shape, and quality metrics.
/// Used for client-side annotation overlay rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarMetric {
    pub id: Option<i64>,
    pub frame_analysis_id: i64,
    pub x: f64,
    pub y: f64,
    pub peak: f64,
    pub flux: f64,
    pub fwhm: f64,
    pub fwhm_x: f64,
    pub fwhm_y: f64,
    pub eccentricity: f64,
    pub snr: f64,
    pub hfr: f64,
    pub theta: f64,
    pub beta: Option<f64>,
    pub fit_method: String,
    pub fit_residual: f64,
}

/// Response for the get_frame_star_metrics command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarMetricsResponse {
    pub stars: Vec<StarMetric>,
    pub metrics: FrameAnalysis,
    pub image_width: i64,
    pub image_height: i64,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/athenaeum-core && cargo check`
Expected: compiles (structs are defined but not yet used — serde derive keeps them alive)

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/models.rs
git commit -m "feat: add StarMetric and StarMetricsResponse models"
```

---

## Task 3: Database Operations — Star Metrics CRUD

**Files:**

- Modify: `crates/athenaeum-core/src/db/analysis.rs`

- [ ] **Step 1: Add star metrics DB functions**

In `crates/athenaeum-core/src/db/analysis.rs`, add the import and new functions. First, update the imports at the top of the file:

```rust
use rusqlite::{params, Connection, Result};
use crate::models::{FrameAnalysis, StarMetric};
```

Then add these functions after the existing `delete_analyses_for_missing_files` function (after line 136), before the private `row_to_analysis` helper:

```rust

/// Bulk-insert star metrics for a frame analysis.
/// Deletes any existing stars for this analysis_id first, then inserts all new ones.
pub fn upsert_star_metrics(conn: &Connection, analysis_id: i64, stars: &[StarMetric]) -> Result<()> {
    conn.execute(
        "DELETE FROM star_metrics WHERE frame_analysis_id = ?1",
        params![analysis_id],
    )?;

    let mut stmt = conn.prepare(
        "INSERT INTO star_metrics (
            frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y,
            eccentricity, snr, hfr, theta, beta, fit_method, fit_residual
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
    )?;

    for s in stars {
        stmt.execute(params![
            analysis_id, s.x, s.y, s.peak, s.flux, s.fwhm, s.fwhm_x, s.fwhm_y,
            s.eccentricity, s.snr, s.hfr, s.theta, s.beta, s.fit_method, s.fit_residual,
        ])?;
    }
    Ok(())
}

/// Get all star metrics for a frame analysis.
pub fn get_star_metrics(conn: &Connection, analysis_id: i64) -> Result<Vec<StarMetric>> {
    let mut stmt = conn.prepare(
        "SELECT id, frame_analysis_id, x, y, peak, flux, fwhm, fwhm_x, fwhm_y,
                eccentricity, snr, hfr, theta, beta, fit_method, fit_residual
         FROM star_metrics WHERE frame_analysis_id = ?1"
    )?;

    let rows = stmt.query_map(params![analysis_id], |row| {
        Ok(StarMetric {
            id: row.get(0)?,
            frame_analysis_id: row.get(1)?,
            x: row.get(2)?,
            y: row.get(3)?,
            peak: row.get(4)?,
            flux: row.get(5)?,
            fwhm: row.get(6)?,
            fwhm_x: row.get(7)?,
            fwhm_y: row.get(8)?,
            eccentricity: row.get(9)?,
            snr: row.get(10)?,
            hfr: row.get(11)?,
            theta: row.get(12)?,
            beta: row.get(13)?,
            fit_method: row.get(14)?,
            fit_residual: row.get(15)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get star metrics for a frame by frame_id (joins through frame_analysis).
pub fn get_star_metrics_by_frame_id(conn: &Connection, frame_id: i64) -> Result<Vec<StarMetric>> {
    let mut stmt = conn.prepare(
        "SELECT sm.id, sm.frame_analysis_id, sm.x, sm.y, sm.peak, sm.flux,
                sm.fwhm, sm.fwhm_x, sm.fwhm_y, sm.eccentricity, sm.snr,
                sm.hfr, sm.theta, sm.beta, sm.fit_method, sm.fit_residual
         FROM star_metrics sm
         INNER JOIN frame_analysis fa ON fa.id = sm.frame_analysis_id
         WHERE fa.frame_id = ?1"
    )?;

    let rows = stmt.query_map(params![frame_id], |row| {
        Ok(StarMetric {
            id: row.get(0)?,
            frame_analysis_id: row.get(1)?,
            x: row.get(2)?,
            y: row.get(3)?,
            peak: row.get(4)?,
            flux: row.get(5)?,
            fwhm: row.get(6)?,
            fwhm_x: row.get(7)?,
            fwhm_y: row.get(8)?,
            eccentricity: row.get(9)?,
            snr: row.get(10)?,
            hfr: row.get(11)?,
            theta: row.get(12)?,
            beta: row.get(13)?,
            fit_method: row.get(14)?,
            fit_residual: row.get(15)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/athenaeum-core && cargo check`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/db/analysis.rs
git commit -m "feat: add star_metrics CRUD operations"
```

---

## Task 4: Analyzer — Return Per-Star Data

**Files:**

- Modify: `crates/athenaeum-core/src/analysis/analyzer.rs`

- [ ] **Step 1: Update analyze_frame to return stars alongside FrameAnalysis**

The `analyze_frame` function currently returns `Result<FrameAnalysis>`. Change it to return both the analysis and per-star data. Update the full function:

Replace the existing imports and `analyze_frame` function (lines 1-68) with:

```rust
use anyhow::Result;
use astroimage::ImageAnalyzer;
use std::sync::Arc;

use crate::models::{FrameAnalysis, StarMetric};
use super::config::AnalysisConfig;

/// Build an `ImageAnalyzer` from an `AnalysisConfig`.
/// Used by both batch analysis and blink annotation to ensure identical parameters.
pub fn build_analyzer(
    config: &AnalysisConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> ImageAnalyzer {
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(config.detection_sigma as f32)
        .with_min_star_area(config.min_star_area as usize)
        .with_max_star_area(config.max_star_area as usize)
        .with_saturation_fraction(config.saturation_fraction as f32)
        .with_max_stars(config.max_stars as usize)
        .with_trail_threshold(config.trail_threshold as f32)
        .with_mrs_layers(config.mrs_layers as usize)
        .with_measure_cap(config.measure_cap as usize)
        .with_fit_max_iter(config.fit_max_iter as usize)
        .with_fit_tolerance(config.fit_tolerance)
        .with_fit_max_rejects(config.fit_max_rejects as usize);

    if let Some(pool) = thread_pool {
        analyzer = analyzer.with_thread_pool(pool);
    }
    analyzer
}

/// Analyze a single frame file and return aggregate metrics + per-star data.
/// The returned FrameAnalysis has `frame_id` and `file_id` set to 0 — the caller must fill these in.
/// Accepts a pre-computed `config_hash` to avoid recomputing SHA256 per frame.
pub fn analyze_frame(
    path: &str,
    analyzer: &ImageAnalyzer,
    config_hash: &str,
) -> Result<(FrameAnalysis, Vec<StarMetric>)> {
    let result = analyzer.analyze(path)?;

    let stars: Vec<StarMetric> = result.stars.iter().map(|s| StarMetric {
        id: None,
        frame_analysis_id: 0, // Caller fills in after DB insert
        x: s.x as f64,
        y: s.y as f64,
        peak: s.peak as f64,
        flux: s.flux as f64,
        fwhm: s.fwhm as f64,
        fwhm_x: s.fwhm_x as f64,
        fwhm_y: s.fwhm_y as f64,
        eccentricity: s.eccentricity as f64,
        snr: s.snr as f64,
        hfr: s.hfr as f64,
        theta: s.theta as f64,
        beta: s.beta.map(|v| v as f64),
        fit_method: format!("{:?}", s.fit_method),
        fit_residual: s.fit_residual as f64,
    }).collect();

    let analysis = FrameAnalysis {
        id: None,
        frame_id: 0,
        file_id: 0,
        stars_detected: result.stars_detected as i64,
        median_fwhm: result.median_fwhm as f64,
        median_eccentricity: result.median_eccentricity as f64,
        median_snr: result.median_snr as f64,
        median_hfr: result.median_hfr as f64,
        frame_snr: result.frame_snr as f64,
        snr_weight: result.snr_weight as f64,
        psf_signal: result.psf_signal as f64,
        background: result.background as f64,
        noise: result.noise as f64,
        detection_threshold: result.detection_threshold as f64,
        width: result.width as i64,
        height: result.height as i64,
        source_channels: result.source_channels as i64,
        trail_r_squared: result.trail_r_squared as f64,
        possibly_trailed: result.possibly_trailed,
        median_beta: result.median_beta.map(|v| v as f64),
        quality_score: None,
        config_hash: Some(config_hash.to_string()),
        analyzed_at: chrono::Utc::now().to_rfc3339(),
    };

    Ok((analysis, stars))
}
```

The `compute_quality_scores` and `normalize` functions (lines 70-126) remain unchanged.

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/athenaeum-core && cargo check`
Expected: Compile errors in callers of `analyze_frame` (Tauri/web commands) — this is expected and will be fixed in the next tasks.

- [ ] **Step 3: Commit**

```bash
git add crates/athenaeum-core/src/analysis/analyzer.rs
git commit -m "feat: return per-star data from analyze_frame"
```

---

## Task 5: Tauri Commands — Update Analysis + Add `get_frame_star_metrics`

**Files:**

- Modify: `crates/athenaeum-tauri/src/commands/analysis.rs`

- [ ] **Step 1: Update imports**

Replace the existing imports (lines 1-10):

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use serde::Serialize;
use tauri::{Emitter, State};
use athenaeum_core::analysis::config::{self, AnalysisConfig};
use athenaeum_core::analysis::analyzer;
use athenaeum_core::db::analysis as db_analysis;
use athenaeum_core::models::{FrameAnalysis, StarMetric, StarMetricsResponse};

use super::AppState;
```

- [ ] **Step 2: Update analyze_frame_set worker loop to persist stars**

In the `analyze_frame_set` function, replace the worker loop result handling (lines 146-157) where `analyze_frame` is called:

```rust
                        let result = match analyzer::analyze_frame(&path, &img_analyzer, &config_hash) {
                            Ok((mut analysis, stars)) => {
                                analysis.frame_id = frame_id;
                                analysis.file_id = file_id;
                                Ok((frame_id, analysis, stars))
                            }
                            Err(e) => {
                                let msg = format!("{}: {}", path, e);
                                eprintln!("Analysis failed for {}", msg);
                                Err(msg)
                            }
                        };
```

Update the results type on line 136 to include stars:

```rust
        let results: std::sync::Mutex<Vec<Result<(i64, FrameAnalysis, Vec<StarMetric>), String>>> =
            std::sync::Mutex::new(Vec::with_capacity(total));
```

And update the return type from `spawn_blocking` accordingly on line 134:

```rust
    let results: Vec<Result<(i64, FrameAnalysis, Vec<StarMetric>), String>> = tokio::task::spawn_blocking(move || {
```

- [ ] **Step 3: Update result partitioning and persistence to include stars**

Replace the result partitioning and persistence section (lines 183-238):

```rust
    // Partition results
    let mut all_analyses: Vec<(i64, FrameAnalysis, Vec<StarMetric>)> = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(triple) => all_analyses.push(triple),
            Err(msg) => errors.push(msg),
        }
    }
    let analyzed = all_analyses.len();
    let failed = errors.len();

    // Separate stars from analyses for quality scoring
    let mut stars_by_frame: std::collections::HashMap<i64, Vec<StarMetric>> = std::collections::HashMap::new();
    let mut analyses: Vec<FrameAnalysis> = Vec::new();
    for (frame_id, analysis, stars) in all_analyses {
        stars_by_frame.insert(frame_id, stars);
        analyses.push(analysis);
    }
    analyzer::compute_quality_scores(&mut analyses, &analysis_config);

    // Persist all results in a single transaction
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        // If incremental (not force), combine with existing analyses for scoring
        if !analyses.is_empty() && !force {
            let existing = db_analysis::get_frame_analyses_for_frame_set(&conn, frame_set_id)
                .map_err(|e| e.to_string())?;

            let mut combined: Vec<FrameAnalysis> = Vec::new();
            let new_frame_ids: std::collections::HashSet<i64> = analyses.iter().map(|a| a.frame_id).collect();

            for existing_a in existing {
                if !new_frame_ids.contains(&existing_a.frame_id) {
                    combined.push(existing_a);
                }
            }
            combined.append(&mut analyses);
            analyzer::compute_quality_scores(&mut combined, &analysis_config);

            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            for a in &combined {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| {
                    let _ = conn.execute_batch("ROLLBACK");
                    e.to_string()
                })?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars).map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        e.to_string()
                    })?;
                }
            }
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        } else if !analyses.is_empty() {
            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            for a in &analyses {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a).map_err(|e| {
                    let _ = conn.execute_batch("ROLLBACK");
                    e.to_string()
                })?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars).map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        e.to_string()
                    })?;
                }
            }
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        }
    }
```

- [ ] **Step 4: Update analyze_single_frame to persist stars**

Replace the `analyze_single_frame` function body (lines 252-296):

```rust
#[tauri::command]
pub async fn analyze_single_frame(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<FrameAnalysis, String> {
    let (analysis_config, file_id, path) = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();

        let analysis_config = config::load_config(&conn);

        let (file_id, path): (i64, String) = conn.query_row(
            "SELECT fi.id, fi.path FROM frames f
             INNER JOIN files fi ON fi.id = f.file_id
             WHERE f.id = ?1",
            rusqlite::params![frame_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|e| format!("Frame not found: {}", e))?;

        (analysis_config, file_id, path)
    };

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = analysis_config.config_hash();
    let path_owned = path.clone();

    let (mut analysis, stars) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| format!("Analysis panicked: {}", e))?
        .map_err(|e| format!("Analysis failed: {}", e))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0);

    // Persist analysis + stars
    {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        let conn = db.conn();
        let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis)
            .map_err(|e| e.to_string())?;
        db_analysis::upsert_star_metrics(&conn, analysis_id, &stars)
            .map_err(|e| e.to_string())?;
    }

    Ok(analysis)
}
```

- [ ] **Step 5: Add get_frame_star_metrics command**

Add after the `delete_analysis_for_frame_set` function (after line 320):

```rust

/// Get star metrics for a frame. Returns from DB if fresh, otherwise analyzes on-demand.
#[tauri::command]
pub async fn get_frame_star_metrics(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<StarMetricsResponse, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let analysis_config = config::load_config(&conn);
    let current_hash = analysis_config.config_hash();

    // Check if we have fresh analysis data
    if let Ok(Some(existing)) = db_analysis::get_frame_analysis(&conn, frame_id) {
        if existing.config_hash.as_deref() == Some(&current_hash) {
            // Fresh data — load stars from DB
            if let Ok(stars) = db_analysis::get_star_metrics_by_frame_id(&conn, frame_id) {
                if !stars.is_empty() {
                    return Ok(StarMetricsResponse {
                        image_width: existing.width,
                        image_height: existing.height,
                        metrics: existing,
                        stars,
                    });
                }
            }
        }
    }

    // Stale or missing — analyze on-demand
    let (file_id, path): (i64, String) = conn.query_row(
        "SELECT fi.id, fi.path FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         WHERE f.id = ?1",
        rusqlite::params![frame_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|e| format!("Frame not found: {}", e))?;

    // Drop the DB reference before spawning blocking work
    drop(conn);
    drop(db);

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = current_hash.clone();
    let path_owned = path.clone();

    let (mut analysis, mut stars) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| format!("Analysis panicked: {}", e))?
        .map_err(|e| format!("Analysis failed: {}", e))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0);

    // Persist
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis)
        .map_err(|e| e.to_string())?;

    for s in &mut stars {
        s.frame_analysis_id = analysis_id;
    }
    db_analysis::upsert_star_metrics(&conn, analysis_id, &stars)
        .map_err(|e| e.to_string())?;

    Ok(StarMetricsResponse {
        image_width: analysis.width,
        image_height: analysis.height,
        metrics: analysis,
        stars,
    })
}
```

- [ ] **Step 6: Verify it compiles**

Run: `cd crates/athenaeum-tauri && cargo check`
Expected: May show warnings about unused imports from `commands_rustafits.rs` (annotated command still exists but will be removed in Task 7). The analysis module itself should compile.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-tauri/src/commands/analysis.rs
git commit -m "feat: persist per-star data in analysis commands + add get_frame_star_metrics"
```

---

## Task 6: Web Routes — Mirror Tauri Changes

**Files:**

- Modify: `crates/athenaeum-web/src/routes/analysis.rs`
- Modify: `crates/athenaeum-web/src/routes/mod.rs`

- [ ] **Step 1: Update web analyze_single_frame to persist stars**

In `crates/athenaeum-web/src/routes/analysis.rs`, update the `analyze_single_frame` handler (lines 127-175). The changes mirror the Tauri command: destructure the tuple from `analyze_frame`, persist stars after upsert.

Update the return from `spawn_blocking` (around line 157-161):

```rust
    let (mut analysis, stars) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| err(format!("Analysis panicked: {}", e)))?
        .map_err(|e| err(format!("Analysis failed: {}", e)))?;
```

Update the persistence section (around lines 167-172):

```rust
    // Persist
    {
        let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
        let conn = db.conn();
        let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis).map_err(err)?;
        db_analysis::upsert_star_metrics(&conn, analysis_id, &stars).map_err(err)?;
    }
```

- [ ] **Step 2: Update web analyze_frame_set worker loop and persistence**

Apply the same pattern as the Tauri command in Task 5 — the worker loop returns `(i64, FrameAnalysis, Vec<StarMetric>)` tuples, and the persistence section writes stars for each analysis. Add `StarMetric` to the imports:

```rust
use athenaeum_core::models::{FrameAnalysis, StarMetric, StarMetricsResponse};
```

- [ ] **Step 3: Add get_frame_star_metrics web route handler**

Add the handler in `crates/athenaeum-web/src/routes/analysis.rs` (mirrors the Tauri command logic from Task 5 Step 5, but using Axum extractors):

```rust
/// POST /api/get_frame_star_metrics
pub async fn get_frame_star_metrics(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<StarMetricsResponse>, (StatusCode, String)> {
    let frame_id = args.frame_id;

    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();

    let analysis_config = config::load_config(&conn);
    let current_hash = analysis_config.config_hash();

    // Check if we have fresh analysis data
    if let Ok(Some(existing)) = db_analysis::get_frame_analysis(&conn, frame_id) {
        if existing.config_hash.as_deref() == Some(&current_hash) {
            if let Ok(stars) = db_analysis::get_star_metrics_by_frame_id(&conn, frame_id) {
                if !stars.is_empty() {
                    return Ok(Json(StarMetricsResponse {
                        image_width: existing.width,
                        image_height: existing.height,
                        metrics: existing,
                        stars,
                    }));
                }
            }
        }
    }

    // Stale or missing — analyze on-demand
    let (file_id, path): (i64, String) = conn.query_row(
        "SELECT fi.id, fi.path FROM frames f
         INNER JOIN files fi ON fi.id = f.file_id
         WHERE f.id = ?1",
        rusqlite::params![frame_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|e| err(format!("Frame not found: {}", e)))?;

    drop(conn);
    drop(db);

    let pool = Arc::clone(&state.ctx.image_pool);
    let img_analyzer = analyzer::build_analyzer(&analysis_config, Some(Arc::clone(&pool)));
    let config_hash = current_hash.clone();
    let path_owned = path.clone();

    let (mut analysis, mut stars) = tokio::task::spawn_blocking(move || {
        analyzer::analyze_frame(&path_owned, &img_analyzer, &config_hash)
    }).await
        .map_err(|e| err(format!("Analysis panicked: {}", e)))?
        .map_err(|e| err(format!("Analysis failed: {}", e)))?;

    analysis.frame_id = frame_id;
    analysis.file_id = file_id;
    analysis.quality_score = Some(1.0);

    let db = state.ctx.db.get().ok_or_else(|| err("Database not initialized"))?;
    let conn = db.conn();
    let analysis_id = db_analysis::upsert_frame_analysis(&conn, &analysis).map_err(err)?;

    for s in &mut stars {
        s.frame_analysis_id = analysis_id;
    }
    db_analysis::upsert_star_metrics(&conn, analysis_id, &stars).map_err(err)?;

    Ok(Json(StarMetricsResponse {
        image_width: analysis.width,
        image_height: analysis.height,
        metrics: analysis,
        stars,
    }))
}
```

- [ ] **Step 4: Register the new route**

In `crates/athenaeum-web/src/routes/mod.rs`, add the new route near the other analysis routes (around line 166):

```rust
        .route("/api/get_frame_star_metrics", post(analysis::get_frame_star_metrics))
```

- [ ] **Step 5: Verify it compiles**

Run: `cd crates/athenaeum-web && cargo check`
Expected: compiles (annotated image route still exists, will be removed in Task 7)

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-web/src/routes/analysis.rs crates/athenaeum-web/src/routes/mod.rs
git commit -m "feat: add get_frame_star_metrics web route + persist stars in analysis"
```

---

## Task 7: Cleanup — Remove Annotated Image Pipeline (Backend)

**Files:**

- Modify: `crates/athenaeum-core/src/rustafits_processor/mod.rs`
- Modify: `crates/athenaeum-core/src/services/mod.rs`
- Modify: `crates/athenaeum-tauri/src/commands_rustafits.rs`
- Modify: `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/images.rs`
- Modify: `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `crates/athenaeum-web/src/main.rs`

- [ ] **Step 1: Remove from rustafits_processor**

In `crates/athenaeum-core/src/rustafits_processor/mod.rs`:

- Delete the `AnnotationMetrics` struct (lines 154-167)
- Delete the `AnnotationSettings::to_rustafits_config()` method (lines 209-229)
- Delete the `AnnotatedImageResult` struct (lines 231-236)
- Delete the `process_fits_to_jpeg_annotated` function (lines 243-336)
- Remove the `use crate::analysis::{analyzer::build_analyzer, config::AnalysisConfig};` import if present
- Remove `use astroimage::annotate_image;` import if present

Keep: `AnnotationSettings` struct and its `Default` impl (frontend still needs it for display preferences).

- [ ] **Step 2: Remove annotation_metrics from ServiceContext**

In `crates/athenaeum-core/src/services/mod.rs`:

- Remove line 8: `use crate::rustafits_processor::AnnotationMetrics;`
- Remove lines 34-35: the `annotation_metrics` field and its comment
- Remove `use std::collections::HashMap;` if no longer used by other fields (check — `active_scans` and `active_exports` still use it, so keep it)

- [ ] **Step 3: Remove read_fits_image_annotated from Tauri commands**

In `crates/athenaeum-tauri/src/commands_rustafits.rs`:

- Delete the entire `read_fits_image_annotated` function (lines 134-275)
- Delete the `AnnotatedImageResponse` struct if defined there (check — it's at lines 13-16)
- Clean up any imports that are no longer needed (e.g., `AnnotationSettings`, `analysis_config`, `AnnotationMetrics`)

- [ ] **Step 4: Update Tauri invoke_handler**

In `crates/athenaeum-tauri/src/lib.rs`:

- Remove line 269: `commands_rustafits::read_fits_image_annotated,`
- Add: `commands::get_frame_star_metrics,` (near the other analysis commands, around line 267)
- Remove line 76: `annotation_metrics: Arc::new(Mutex::new(HashMap::new())),`
- Clean up any now-unused imports

- [ ] **Step 5: Remove annotated endpoint from web routes**

In `crates/athenaeum-web/src/routes/images.rs`:

- Delete the `read_fits_image_annotated` handler function (lines 155-274)
- Clean up unused imports

In `crates/athenaeum-web/src/routes/mod.rs`:

- Remove line 156: `.route("/api/read_fits_image_annotated", post(images::read_fits_image_annotated))`

- [ ] **Step 6: Remove annotation_metrics from web main.rs**

In `crates/athenaeum-web/src/main.rs`:

- Remove line 132: `annotation_metrics: Arc::new(Mutex::new(HashMap::new())),`
- Clean up any now-unused imports

- [ ] **Step 7: Verify full workspace compiles**

Run: `cargo check --workspace`
Expected: compiles with no errors. There may be warnings about unused imports in frontend-related code — those will be cleaned up in frontend tasks.

- [ ] **Step 8: Commit**

```bash
git add crates/
git commit -m "refactor: remove annotated image pipeline from backend"
```

---

## Task 8: TypeScript Types — StarMetric + StarMetricsResponse

**Files:**

- Modify: `src/types/models.ts`

- [ ] **Step 1: Add StarMetric and StarMetricsResponse, remove old types**

In `src/types/models.ts`, replace the `AnnotationMetrics` and `AnnotatedImageResponse` interfaces (lines 875-894) with:

```typescript
/** Individual star detection result for client-side overlay rendering */
export interface StarMetric {
  id: number | null;
  frame_analysis_id: number;
  x: number;
  y: number;
  peak: number;
  flux: number;
  fwhm: number;
  fwhm_x: number;
  fwhm_y: number;
  eccentricity: number;
  snr: number;
  hfr: number;
  theta: number;
  beta: number | null;
  fit_method: string;
  fit_residual: number;
}

/** Response from get_frame_star_metrics command */
export interface StarMetricsResponse {
  stars: StarMetric[];
  metrics: FrameAnalysis;
  image_width: number;
  image_height: number;
}
```

- [ ] **Step 2: Commit**

```bash
git add src/types/models.ts
git commit -m "feat: add StarMetric/StarMetricsResponse types, remove AnnotationMetrics"
```

---

## Task 9: Update Blink Sub-Component Types

**Files:**

- Modify: `src/components/blink/types.ts`
- Modify: `src/components/blink/FrameInfoPanel.tsx`

- [ ] **Step 1: Update FrameInfoPanelProps to use FrameAnalysis**

In `src/components/blink/types.ts`, replace the import on line 1 and update FrameInfoPanelProps:

```typescript
import type { FileWithFrame, FrameAnalysis } from "../../types/models";
```

Update the `FrameInfoPanelProps` interface (lines 62-65):

```typescript
/** Props for the FrameInfoPanel component */
export interface FrameInfoPanelProps {
  currentFrame: FileWithFrame | undefined;
  metrics: FrameAnalysis | null;
}
```

- [ ] **Step 2: Update FrameInfoPanel component**

In `src/components/blink/FrameInfoPanel.tsx`, the component already reads `metrics.median_fwhm`, `metrics.stars_detected`, etc. — these field names are identical between `AnnotationMetrics` and `FrameAnalysis`, so the component body needs no changes. Just verify the import on line 3 doesn't reference `AnnotationMetrics`:

```typescript
import type { FrameInfoPanelProps } from "./types";
```

This import is already correct — it reads from `types.ts` which now uses `FrameAnalysis`.

- [ ] **Step 3: Commit**

```bash
git add src/components/blink/types.ts src/components/blink/FrameInfoPanel.tsx
git commit -m "refactor: update FrameInfoPanel to use FrameAnalysis instead of AnnotationMetrics"
```

---

## Task 10: Star Overlay Drawing Module

**Files:**

- Create: `src/components/blink/StarOverlay.ts`

- [ ] **Step 1: Create the star overlay drawing module**

Create `src/components/blink/StarOverlay.ts` with pure canvas drawing functions:

```typescript
import type { StarMetric } from "../../types/models";
import type { AnnotationSettings } from "../../types/analysis-config";

/** Parameters needed to map FITS pixel coords to canvas coords */
export interface OverlayTransform {
  /** Canvas pixel offset X (top-left of rendered image) */
  offsetX: number;
  /** Canvas pixel offset Y (top-left of rendered image) */
  offsetY: number;
  /** Rendered image width in canvas pixels (after zoom) */
  renderWidth: number;
  /** Rendered image height in canvas pixels (after zoom) */
  renderHeight: number;
  /** Original FITS image width in pixels */
  imageWidth: number;
  /** Original FITS image height in pixels */
  imageHeight: number;
}

/** Compute color for a star based on annotation settings color scheme */
function starColor(star: StarMetric, settings: AnnotationSettings): string {
  if (settings.color_scheme === "uniform") {
    return "rgba(0, 255, 0, 0.8)";
  }

  let value: number;
  let good: number;
  let warn: number;

  if (settings.color_scheme === "eccentricity") {
    value = star.eccentricity;
    good = settings.ecc_good;
    warn = settings.ecc_warn;
  } else {
    // fwhm
    value = star.fwhm;
    good = settings.fwhm_good;
    warn = settings.fwhm_warn;
  }

  if (value <= good) return "rgba(0, 255, 0, 0.8)";   // Green — good
  if (value <= warn) return "rgba(255, 255, 0, 0.8)";  // Yellow — warning
  return "rgba(255, 0, 0, 0.8)";                        // Red — bad
}

/** Draw all star annotations on the overlay canvas */
export function drawStarOverlay(
  ctx: CanvasRenderingContext2D,
  canvasWidth: number,
  canvasHeight: number,
  stars: StarMetric[],
  settings: AnnotationSettings,
  transform: OverlayTransform,
): void {
  ctx.clearRect(0, 0, canvasWidth, canvasHeight);

  const scaleX = transform.renderWidth / transform.imageWidth;
  const scaleY = transform.renderHeight / transform.imageHeight;

  for (const star of stars) {
    // Map FITS pixel coords to canvas coords
    const cx = transform.offsetX + star.x * scaleX;
    const cy = transform.offsetY + star.y * scaleY;

    // Scale FWHM to canvas pixels, clamped by min/max radius settings
    const rawRadiusX = (star.fwhm_x / 2) * scaleX;
    const rawRadiusY = (star.fwhm_y / 2) * scaleY;
    const minR = settings.min_radius * Math.min(scaleX, scaleY);
    const maxR = settings.max_radius * Math.min(scaleX, scaleY);
    const radiusX = Math.max(minR, Math.min(maxR, rawRadiusX));
    const radiusY = Math.max(minR, Math.min(maxR, rawRadiusY));

    if (radiusX < 0.5 || radiusY < 0.5) continue; // Too small to draw

    const color = starColor(star, settings);

    // Draw ellipse
    ctx.beginPath();
    ctx.ellipse(cx, cy, radiusX, radiusY, star.theta, 0, 2 * Math.PI);
    ctx.strokeStyle = color;
    ctx.lineWidth = settings.line_width;
    ctx.stroke();

    // Direction tick (theta indicator)
    if (settings.show_direction_tick) {
      const tickLen = radiusX * 0.5;
      const edgeX = cx + radiusX * Math.cos(star.theta);
      const edgeY = cy + radiusY * Math.sin(star.theta);
      const tipX = edgeX + tickLen * Math.cos(star.theta);
      const tipY = edgeY + tickLen * Math.sin(star.theta);

      ctx.beginPath();
      ctx.moveTo(edgeX, edgeY);
      ctx.lineTo(tipX, tipY);
      ctx.strokeStyle = color;
      ctx.lineWidth = settings.line_width;
      ctx.stroke();
    }
  }
}

/** Clear the overlay canvas */
export function clearStarOverlay(
  ctx: CanvasRenderingContext2D,
  canvasWidth: number,
  canvasHeight: number,
): void {
  ctx.clearRect(0, 0, canvasWidth, canvasHeight);
}
```

- [ ] **Step 2: Commit**

```bash
git add src/components/blink/StarOverlay.ts
git commit -m "feat: add StarOverlay canvas drawing module"
```

---

## Task 11: Star Metrics Cache Hook

**Files:**

- Create: `src/hooks/useStarMetricsCache.ts`

- [ ] **Step 1: Create the hook**

Create `src/hooks/useStarMetricsCache.ts`:

```typescript
import { useEffect, useRef, useState, useCallback } from "react";
import { api } from "../api";
import type { StarMetricsResponse } from "../types/models";

interface UseStarMetricsCacheArgs {
  frameIds: number[];         // frame_id for each index position
  currentIndex: number;
  enabled: boolean;           // Only fetch when annotations are on
}

interface UseStarMetricsCacheResult {
  /** Get cached star metrics for a frame index (null if not yet loaded) */
  getMetrics: (index: number) => StarMetricsResponse | null;
  /** Whether the current frame's metrics are still loading */
  isLoading: boolean;
}

const MAX_CONCURRENT = 2; // Light — star data is small, don't flood backend with analysis

export function useStarMetricsCache({
  frameIds,
  currentIndex,
  enabled,
}: UseStarMetricsCacheArgs): UseStarMetricsCacheResult {
  const cacheRef = useRef(new Map<number, StarMetricsResponse>());
  const inflightRef = useRef(new Set<number>());
  const unmountedRef = useRef(false);
  const [isLoading, setIsLoading] = useState(false);
  // Trigger re-renders when cache updates
  const [cacheVersion, setCacheVersion] = useState(0);

  const getMetrics = useCallback((index: number): StarMetricsResponse | null => {
    return cacheRef.current.get(index) ?? null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheVersion]);

  const fetchMetrics = useCallback(async (index: number) => {
    const frameId = frameIds[index];
    if (!frameId || cacheRef.current.has(index) || inflightRef.current.has(index)) return;

    inflightRef.current.add(index);
    try {
      const response = await api.invoke<StarMetricsResponse>("get_frame_star_metrics", {
        frameId,
      });
      if (!unmountedRef.current) {
        cacheRef.current.set(index, response);
        setCacheVersion((v) => v + 1);
      }
    } catch (err) {
      console.error(`Failed to load star metrics for frame ${frameId}:`, err);
    } finally {
      inflightRef.current.delete(index);
    }
  }, [frameIds]);

  // Fetch current frame + pre-fetch neighbors when enabled
  useEffect(() => {
    if (!enabled || frameIds.length === 0) return;

    unmountedRef.current = false;
    setIsLoading(!cacheRef.current.has(currentIndex));

    // Build priority list: current frame first, then by proximity
    const priorities: number[] = [currentIndex];
    const total = frameIds.length;
    for (let offset = 1; offset < total && priorities.length < MAX_CONCURRENT; offset++) {
      const idx = (currentIndex + offset) % total;
      if (!cacheRef.current.has(idx) && !inflightRef.current.has(idx)) {
        priorities.push(idx);
      }
    }

    for (const idx of priorities) {
      fetchMetrics(idx).then(() => {
        if (idx === currentIndex && !unmountedRef.current) {
          setIsLoading(false);
        }
      });
    }

    return () => {
      unmountedRef.current = true;
    };
  }, [enabled, currentIndex, frameIds, fetchMetrics]);

  return { getMetrics, isLoading };
}
```

- [ ] **Step 2: Commit**

```bash
git add src/hooks/useStarMetricsCache.ts
git commit -m "feat: add useStarMetricsCache hook for proximity pre-fetching"
```

---

## Task 12: Simplify useBlinkCache — Remove Annotated Job Logic

**Files:**

- Modify: `src/hooks/useBlinkCache.ts`

- [ ] **Step 1: Remove annotated image support from the hook**

Replace the entire file content:

```typescript
import { useEffect, useRef, useState, useCallback } from "react";
import type { FileWithFrame } from "../types/models";

interface UseBlinkCacheArgs {
  frames: FileWithFrame[];
  currentIndex: number;
  cacheModeReady: boolean;
  maxConcurrent: number;
  loadedImages: Map<number, string>;
  loadImage: (index: number) => Promise<void>;
}

interface UseBlinkCacheResult {
  isCaching: boolean;
  cacheProgress: { current: number; total: number };
  cacheStats: { elapsedMs: number; frameCount: number } | null;
}

const DEFAULT_MAX_CONCURRENT = 8;

/**
 * Priority-queue caching controller for BlinkViewer plain images.
 *
 * Manages a single pool of MAX_CONCURRENT in-flight slots.
 * Priority: current frame first, then by proximity.
 */
export function useBlinkCache({
  frames,
  currentIndex,
  cacheModeReady,
  maxConcurrent,
  loadedImages,
  loadImage,
}: UseBlinkCacheArgs): UseBlinkCacheResult {
  const [isCaching, setIsCaching] = useState(false);
  const [cacheProgress, setCacheProgress] = useState({ current: 0, total: 0 });
  const [cacheStats, setCacheStats] = useState<{ elapsedMs: number; frameCount: number } | null>(null);

  const framesRef = useRef(frames);
  const currentIndexRef = useRef(currentIndex);
  const loadedImagesRef = useRef(loadedImages);
  const loadImageRef = useRef(loadImage);

  framesRef.current = frames;
  currentIndexRef.current = currentIndex;
  loadedImagesRef.current = loadedImages;
  loadImageRef.current = loadImage;

  const dispatchedRef = useRef(new Set<number>());
  const inflightCountRef = useRef(0);
  const cacheStartTimeRef = useRef(0);
  const unmountedRef = useRef(false);

  const pickNextJob = useCallback((): number | null => {
    const total = framesRef.current.length;
    if (total === 0) return null;

    const ci = currentIndexRef.current;

    const isAvailable = (idx: number) =>
      !loadedImagesRef.current.has(idx) && !dispatchedRef.current.has(idx);

    // Current frame first
    if (isAvailable(ci)) return ci;

    // Then by proximity
    for (let offset = 1; offset < total; offset++) {
      const idx = (ci + offset) % total;
      if (isAvailable(idx)) return idx;
    }

    return null;
  }, []);

  const tryDispatch = useCallback(() => {
    if (unmountedRef.current) return;

    const limit = maxConcurrent || DEFAULT_MAX_CONCURRENT;
    while (inflightCountRef.current < limit) {
      const idx = pickNextJob();
      if (idx === null) break;

      dispatchedRef.current.add(idx);
      inflightCountRef.current++;

      loadImageRef.current(idx).finally(() => {
        inflightCountRef.current--;
        tryDispatch();
      });
    }
  }, [pickNextJob, maxConcurrent]);

  // Progress tracking
  useEffect(() => {
    if (!isCaching) return;
    const total = frames.length;
    if (total === 0) return;
    setCacheProgress({ current: loadedImages.size, total });
    if (loadedImages.size >= total) {
      setIsCaching(false);
      if (cacheStartTimeRef.current > 0) {
        setCacheStats({
          elapsedMs: Date.now() - cacheStartTimeRef.current,
          frameCount: total,
        });
      }
    }
  }, [loadedImages, frames.length, isCaching]);

  // Main effect: start caching
  useEffect(() => {
    if (!cacheModeReady || frames.length === 0) return;

    unmountedRef.current = false;
    dispatchedRef.current = new Set();
    inflightCountRef.current = 0;
    cacheStartTimeRef.current = Date.now();
    setIsCaching(true);
    setCacheStats(null);
    setCacheProgress({ current: 0, total: frames.length });

    tryDispatch();

    return () => {
      unmountedRef.current = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheModeReady, frames.length]);

  // Re-evaluate priorities on navigation
  useEffect(() => {
    tryDispatch();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentIndex]);

  return { isCaching, cacheProgress, cacheStats };
}
```

- [ ] **Step 2: Commit**

```bash
git add src/hooks/useBlinkCache.ts
git commit -m "refactor: simplify useBlinkCache — remove annotated image logic"
```

---

## Task 13: BlinkViewer — Integrate Overlay Canvas + Star Metrics

**Files:**

- Modify: `src/components/BlinkViewer.tsx`

This is the largest frontend change. It removes annotated image state/loading and adds the overlay canvas + star metrics integration.

- [ ] **Step 1: Update imports**

Replace lines 1-12:

```typescript
import React, { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { api } from '../api';
import { isTauri } from '../utils/platform';
import {
  X,
  Loader2,
  Trash2,
  AlertTriangle,
} from "lucide-react";
import type { FileWithFrame, StarMetricsResponse } from "../types/models";
import type { AnnotationSettings } from "../types/analysis-config";
import { DEFAULT_ANNOTATION_SETTINGS } from "../types/analysis-config";
import { ToolBar, FrameList, FrameInfoPanel } from "./blink";
import { useBlinkCache } from "../hooks/useBlinkCache";
import { useStarMetricsCache } from "../hooks/useStarMetricsCache";
import { drawStarOverlay, clearStarOverlay } from "./blink/StarOverlay";
import type { OverlayTransform } from "./blink/StarOverlay";
```

- [ ] **Step 2: Replace annotation state with star metrics state**

Remove lines 52-57 (annotated image state):

```typescript
  // Annotation state
  const [showAnnotations, setShowAnnotations] = useState(false);
  const [annotatedImages, setAnnotatedImages] = useState<Map<number, string>>(new Map());
  const [frameMetrics, setFrameMetrics] = useState<Map<number, AnnotationMetrics>>(new Map());
  const annotatedImagesRef = useRef(annotatedImages);
  annotatedImagesRef.current = annotatedImages;
```

Replace with:

```typescript
  // Annotation state
  const [showAnnotations, setShowAnnotations] = useState(false);
  const [annotationSettings, setAnnotationSettings] = useState<AnnotationSettings>(DEFAULT_ANNOTATION_SETTINGS);
  const overlayCanvasRef = useRef<HTMLCanvasElement>(null);
```

- [ ] **Step 3: Add star metrics cache hook**

After the `fitsFrames` useMemo, add the frame IDs array and star metrics cache:

```typescript
  const frameIds = useMemo(() => fitsFrames.map(f => f.frame?.id ?? 0), [fitsFrames]);

  const { getMetrics: getStarMetrics, isLoading: isLoadingMetrics } = useStarMetricsCache({
    frameIds,
    currentIndex,
    enabled: showAnnotations,
  });
```

- [ ] **Step 4: Load annotation settings on mount**

Add an effect to load annotation display settings:

```typescript
  // Load annotation display settings
  useEffect(() => {
    api.invoke<string>("get_setting", { key: "blink.annotation_config" })
      .then((json) => {
        if (json) {
          try {
            setAnnotationSettings(JSON.parse(json));
          } catch { /* use defaults */ }
        }
      })
      .catch(() => { /* use defaults */ });
  }, []);
```

- [ ] **Step 5: Remove loadAnnotatedImage function**

Delete the entire `loadAnnotatedImage` callback (lines 162-189).

- [ ] **Step 6: Simplify useBlinkCache call**

Replace the useBlinkCache invocation (lines 192-202) — remove annotated-related args:

```typescript
  const { isCaching, cacheProgress, cacheStats } = useBlinkCache({
    frames: fitsFrames,
    currentIndex,
    cacheModeReady,
    maxConcurrent,
    loadedImages,
    loadImage,
  });
```

- [ ] **Step 7: Add overlay drawing function**

Add a function to draw/clear the star overlay. Place it after `drawImageToCanvas`:

```typescript
  const drawOverlay = useCallback(() => {
    const overlay = overlayCanvasRef.current;
    const baseCanvas = canvasRef.current;
    if (!overlay || !baseCanvas) return;

    const ctx = overlay.getContext("2d");
    if (!ctx) return;

    // Sync overlay canvas size
    if (overlay.width !== baseCanvas.width || overlay.height !== baseCanvas.height) {
      overlay.width = baseCanvas.width;
      overlay.height = baseCanvas.height;
    }

    if (!showAnnotations) {
      clearStarOverlay(ctx, overlay.width, overlay.height);
      return;
    }

    const metricsResponse = getStarMetrics(currentIndex);
    if (!metricsResponse || !currentImageRef.current) {
      clearStarOverlay(ctx, overlay.width, overlay.height);
      return;
    }

    // Recompute the same transform used by drawImageToCanvas
    const img = currentImageRef.current;
    const canvasAspect = baseCanvas.width / baseCanvas.height;
    const imageAspect = img.width / img.height;

    let baseWidth: number, baseHeight: number;
    if (imageAspect > canvasAspect) {
      baseWidth = baseCanvas.width;
      baseHeight = baseCanvas.width / imageAspect;
    } else {
      baseHeight = baseCanvas.height;
      baseWidth = baseCanvas.height * imageAspect;
    }

    const zoom = zoomRef.current;
    const renderWidth = baseWidth * zoom;
    const renderHeight = baseHeight * zoom;
    const centerX = baseCanvas.width / 2 + panRef.current.x;
    const centerY = baseCanvas.height / 2 + panRef.current.y;
    const offsetX = centerX - renderWidth / 2;
    const offsetY = centerY - renderHeight / 2;

    const transform: OverlayTransform = {
      offsetX, offsetY, renderWidth, renderHeight,
      imageWidth: metricsResponse.image_width,
      imageHeight: metricsResponse.image_height,
    };

    drawStarOverlay(ctx, overlay.width, overlay.height, metricsResponse.stars, annotationSettings, transform);
  }, [showAnnotations, currentIndex, getStarMetrics, annotationSettings]);
```

- [ ] **Step 8: Update render effect to draw overlay after base image**

Replace the "Render current image" effect (lines 319-328):

```typescript
  // Render current image + overlay
  useEffect(() => {
    const imageValue = loadedImages.get(currentIndex);
    if (imageValue) renderImage(imageValue);
  }, [currentIndex, loadedImages, renderImage]);

  // Draw overlay whenever annotations, current frame, or metrics change
  useEffect(() => {
    drawOverlay();
  }, [drawOverlay]);
```

- [ ] **Step 9: Update resize handler**

In the resize effect (lines 331-355), replace the annotated image logic. The resize handler should just re-render the plain image and overlay:

```typescript
  useEffect(() => {
    const updateCanvasSize = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const newWidth = window.innerWidth * 0.75;
      const newHeight = window.innerHeight - 48;

      if (canvas.width !== newWidth || canvas.height !== newHeight) {
        canvas.width = newWidth;
        canvas.height = newHeight;
        const imageValue = loadedImagesRef.current.get(currentIndexRef.current);
        if (imageValue) renderImage(imageValue);
        drawOverlay();
      }
    };

    updateCanvasSize();
    window.addEventListener("resize", updateCanvasSize);
    return () => window.removeEventListener("resize", updateCanvasSize);
  }, [renderImage, drawOverlay]);
```

- [ ] **Step 10: Update cleanup effect**

Remove the annotated blob URL cleanup (lines 311-314 in the unmount effect):

```typescript
  useEffect(() => {
    return () => {
      loadedImagesRef.current.forEach((value) => {
        if (value.startsWith("blob:")) {
          URL.revokeObjectURL(value);
        }
      });
    };
  }, []);
```

- [ ] **Step 11: Call drawOverlay after zoom/pan changes**

In the existing `handleWheel` and `handleMouseMove` callbacks, add `drawOverlay()` after `drawImageToCanvas()` is called. Find every place `drawImageToCanvas()` is invoked and add `drawOverlay()` immediately after. This ensures the overlay stays in sync during zoom/pan.

- [ ] **Step 12: Update canvas JSX — add overlay canvas**

Replace the canvas element section (around lines 762-774) — add a second canvas on top:

```tsx
          <div className="flex-1 relative bg-black flex items-center justify-center">
            <canvas
              ref={canvasRef}
              className="max-w-full max-h-full"
              style={{
                imageRendering: "pixelated",
                cursor: displayZoom > 1 ? (isPanning ? "grabbing" : "grab") : "default",
              }}
            />
            <canvas
              ref={overlayCanvasRef}
              className="absolute inset-0 max-w-full max-h-full pointer-events-none"
            />
            {/* The mouse event handlers need to be on a layer that captures events */}
            <div
              className="absolute inset-0"
              style={{
                cursor: displayZoom > 1 ? (isPanning ? "grabbing" : "grab") : "default",
              }}
              onWheel={handleWheel}
              onMouseDown={handleMouseDown}
              onMouseMove={handleMouseMove}
              onMouseUp={handleMouseUp}
              onMouseLeave={handleMouseLeave}
            />
```

Note: Mouse events move to a transparent div on top so they work through the overlay canvas.

- [ ] **Step 13: Update FrameInfoPanel metrics prop**

Replace the FrameInfoPanel usage (around line 812-815):

```tsx
          <FrameInfoPanel
            currentFrame={currentFrame}
            metrics={showAnnotations ? (getStarMetrics(currentIndex)?.metrics ?? null) : null}
          />
```

- [ ] **Step 14: Verify it compiles and renders**

Run: `npm run build` (or `npm run dev` for hot reload)
Expected: compiles with no TypeScript errors

- [ ] **Step 15: Commit**

```bash
git add src/components/BlinkViewer.tsx
git commit -m "feat: client-side star annotation overlay in BlinkViewer"
```

---

## Task 14: Export Blink Sub-Components

**Files:**

- Modify: `src/components/blink/index.ts` (if exists, otherwise check how blink components are exported)

- [ ] **Step 1: Ensure StarOverlay is accessible**

Verify the blink barrel export includes `StarOverlay` if other components need it. The current import from `BlinkViewer.tsx` uses a direct path (`./blink/StarOverlay`), which is fine. No action needed unless the project uses a barrel export pattern.

- [ ] **Step 2: Commit** (skip if no changes)

---

## Task 15: Manual Testing with Real Data

- [ ] **Step 1: Test batch analysis persists stars**

1. Open Athenaeum, navigate to a frame set
2. Run "Analyze" on the frame set
3. Verify analysis completes with no errors
4. Check the database: `sqlite3 <db_path> "SELECT COUNT(*) FROM star_metrics;"`
5. Expected: rows exist (e.g., ~500 per analyzed frame)

- [ ] **Step 2: Test blink viewer with annotations**

1. Open the blink viewer for an analyzed frame set
2. Press `A` to toggle annotations on
3. Verify: star ellipses appear as a canvas overlay on top of the image
4. Verify: ellipses follow zoom and pan correctly
5. Verify: direction ticks appear when `show_direction_tick` is enabled
6. Verify: FrameInfoPanel shows analysis metrics
7. Navigate to different frames — ellipses update

- [ ] **Step 3: Test on-demand analysis**

1. Open the blink viewer for a frame set that has NOT been batch-analyzed
2. Toggle annotations on
3. Verify: first frame takes a moment to analyze (loading indicator), then stars appear
4. Check the database: verify `frame_analysis` and `star_metrics` rows were created

- [ ] **Step 4: Test blink speed without annotations**

1. Open blink viewer, keep annotations OFF
2. Play blink at various speeds
3. Verify: performance is same as before (no regression from overlay canvas)

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: address issues found during manual testing"
```
