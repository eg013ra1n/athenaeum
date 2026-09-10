# Stacking M2 — Local Normalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Local normalization (spec §5.2) — a per-group LN reference, per-frame background models and PSF-flux scale, `.athln` sidecars, the engine hook that applies them as output and rejection normalization, the live `NormalizePanel` LN block, and the LDN 1272 acceptance re-run with LN on both sides against the external masters' rejection and noise.

**Architecture:** stage 6 (`Stage::Normalize`) stops being a no-op when `normalization.local.enabled` is on. Per group it integrates the best `referenceFrames` included frames into an LN reference (kept in RAM, written to `ln/<group>/reference.fits`), then fans out per frame: warp the calibrated frame into the reference geometry through the existing `RegisteredSource`, model its background on the `scale/8` mesh (hot-pixel median, clipping, per-cell robust median, rejection limit), match its PSF fluxes to the reference's and take the RCR location as the global scale, and write `A = s`, `B = B_ref − s·B_tgt` as an `.athln` sidecar keyed like every other artifact (`stacking_artifacts.kind = "ln"`, per-frame config hash of the normalize stage). Integration evaluates the two grids with the bicubic B-spline at every band row and applies `v′ = A·v + B` in place of the global `(offset, scale)` pair — for output when LN is enabled, for rejection when `normalization.rejection = local`. Drizzle (M3) reads the same sidecars.

**Tech Stack:** Rust (athenaeum-core `stacking/ln/*`, `integration/engine.rs`), React/TS (`panels/NormalizePanel.tsx`, `stageSummary.ts`), ts-rs regeneration, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §5.2, §6.1, §6.3, §9.2, §9.3, §9.5, §11.2, §13, §14 M2 · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §1.3, §3.4 (RCR), §4 · **Baselines:** `docs/superpowers/research/2026-09-09-checkpoint-b-integration.md` §3 and `2026-09-09-m1-acceptance-run.md` §5 (our LN-off masters are bit-identical to Checkpoint B's; the external run used local rejection + output normalization).

## Global Constraints

- No new crate dependencies; `tracing` only (no `println!`/`eprintln!` outside tests/examples); never name other software in code or comments (the math reference is the only place that does, with provenance labels).
- Two backends in sync: no new commands are expected; if one is added, Tauri + Axum + `invoke_handler` + `build_router` + `ts_export.rs` in the same task.
- Headless build: `cargo check -p athenaeum-core --no-default-features` must stay clean — everything under `stacking/` is gated `#[cfg(all(feature = "render", feature = "solver"))]` like its siblings.
- New log field names go into the "Unified event schema" dictionary of `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the same task; messages are short stable phrases, data in snake_case fields.
- rustfmt only on new leaf files and files that are rustfmt-clean today (`rustfmt --check` first); never `integration/engine.rs`, `integrate.rs`, `stats.rs`, `weights.rs`, `run.rs`, `mod.rs`, `ts_export.rs`.
- Serde names are the spec §9.2 ones verbatim: `normalization.local.{enabled, scale, referenceFrames, psfModel, localScale}`, `normalization.rejection = "local"`.
- Every pixel path runs on the same units the M1 engine uses (float32 in [0, 1] as read from the calibrated artifacts; the measurement's ×65535 ADU copy is internal to `measure`).
- The engine's byte-identical pins from Plan 4 must keep passing with LN disabled (no behaviour change when `local.enabled = false` and `rejection ≠ local`).
- Commit as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit …` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session:` trailers.

---

## File structure

- Create `crates/athenaeum-core/src/stacking/ln/mod.rs` — module root: `LnConfig` re-export, the stage driver (`normalize_group`), artifact keys.
- Create `crates/athenaeum-core/src/stacking/ln/grid.rs` — `LnGrid` (one channel's `A`/`B` matrices on the stride grid + global scale and locations), the bicubic B-spline evaluator `evaluate_row`, and the `.athln` sidecar reader/writer.
- Create `crates/athenaeum-core/src/stacking/ln/background.rs` — the background model on the `scale/8` mesh: hot-pixel median filter (radius 2), low/high clipping, per-cell deviation clipping, rejection limit, neighbour fill.
- Create `crates/athenaeum-core/src/stacking/ln/scale.rs` — PSF-flux relative scale: star fits on reference and target, proximity match, RCR location.
- Create `crates/athenaeum-core/src/stacking/ln/reference.rs` — the LN reference per group (best-N integration through `integrate_group`'s machinery) + `reference.fits` writer/reader.
- Modify `crates/athenaeum-core/src/integration/engine.rs` — `StackParams` gains `local: Option<&[Option<LnFrameGrids>]>`; the band loop evaluates and applies the grids.
- Modify `crates/athenaeum-core/src/integration/stats.rs` — `RejectionNormalization::Local` resolution returns "use the grids" instead of a pair.
- Modify `crates/athenaeum-core/src/stacking/integrate.rs` — `GroupInput` carries the per-frame sidecars; `integrate_group` threads them into `StackParams`; `GroupStats` gains `ln_reference_frames` and `relative_scale` per frame? (no — per frame goes to `SummaryFrame.ln_scale`).
- Modify `crates/athenaeum-core/src/stacking/run.rs` — `Stage::Normalize` runs the driver with the measure-style fan-out, caches sidecars as artifacts (`kind = "ln"`, hash of the normalize stage), and passes them to integration; `SummaryFrame` gains `ln_scale: Option<f64>` and `cached_ln: bool`; `RunSummary.groups[].ln_reference_path`.
- Modify `crates/athenaeum-core/src/stacking/plan.rs` — `stale_stages` gains `normalize` (cacheable per frame like calibrate/measure/register); `StackingPlan.groups[].ln_cached`.
- Modify `crates/athenaeum-core/src/stacking/provenance.rs`, `api/stacking.rs`, `ts_export.rs` — the new summary fields; regenerate `src/types/stacking.ts`.
- Modify `src/components/stacking/panels/NormalizePanel.tsx`, `src/components/stacking/stageSummary.ts`, `src/components/stacking/ResultsPanel.tsx`, `src/components/stacking/FramesTable.tsx` — the LN block live, the stage summary, the LN reference in the results card, the per-frame scale column.
- Modify `crates/athenaeum-core/src/stacking/paths.rs` — `ln_reference_path(group)`, `ln_sidecar_path(group, stem)`; `cleanup_stacking_work` already counts `ln/`.
- Modify `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary: `ln_scale`, `ln_cells_rejected`, `ln_matches`), `CLAUDE.md` → Stacking (M2 paragraph), spec §9.3 (the `ln` artifact kind and its hash inputs) and §9.5 (`ln/<group>/reference.fits`, `<stem>.athln`).
- Create `crates/athenaeum-core/examples/ln_probe.rs` — dev probe: build the LN reference and one frame's sidecar from a group of calibrated frames + registration rows, print the scale, the grid ranges and the residual background after normalization.
- Create `docs/superpowers/research/2026-09-10-m2-acceptance-run.md` (Task 9).

---

### Task 1: `LnGrid`, the bicubic B-spline evaluator and the `.athln` sidecar

**Files:**
- Create: `crates/athenaeum-core/src/stacking/ln/mod.rs`, `crates/athenaeum-core/src/stacking/ln/grid.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (`pub mod ln;`), `crates/athenaeum-core/src/stacking/paths.rs` (two path helpers)
- Test: in-file `#[cfg(test)]`

**Interfaces:**
- Produces:
  ```rust
  /// One channel's local-normalization model on the stride grid (spec §5.2, math §4.1/§4.4).
  #[derive(Debug, Clone, PartialEq)]
  pub struct LnGrid {
      pub ref_width: usize,   // reference geometry the grid covers
      pub ref_height: usize,
      pub scale: u32,         // the LN scale (1024); stride = scale / 8
      pub gw: usize,          // grid columns = ceil(ref_width / stride) + 1
      pub gh: usize,
      pub a: Vec<f32>,        // gw × gh, row-major — local scale
      pub b: Vec<f32>,        // gw × gh — local zero offset
      pub global_scale: f64,  // s from the PSF method
      pub location_ref: f64,  // median of the reference plane
      pub location_tgt: f64,  // median of the target plane
  }
  impl LnGrid {
      pub fn stride(&self) -> usize { (self.scale / 8).max(2) as usize }
      pub fn constant(ref_width: usize, ref_height: usize, scale: u32, a: f32, b: f32) -> LnGrid;
      /// Evaluates A and B along output row `y` (reference coordinates) into `a_row`/`b_row`
      /// (length ref_width) with the bicubic B-spline over the grid; node (i, j) sits at
      /// (i·stride, j·stride); coordinates beyond the last node clamp to it.
      pub fn evaluate_row(&self, y: usize, a_row: &mut [f32], b_row: &mut [f32]);
      #[inline] pub fn apply(a: f32, b: f32, v: f32) -> f32 { a * v + b }
  }
  /// One frame's sidecar: one grid per channel.
  #[derive(Debug, Clone, PartialEq)]
  pub struct LnFrameGrids { pub channels: Vec<LnGrid> }
  impl LnFrameGrids {
      pub fn write(&self, path: &Path) -> anyhow::Result<()>;   // tmp + atomic rename
      pub fn read(path: &Path) -> anyhow::Result<LnFrameGrids>;
  }
  ```
- `paths.rs`: `pub fn ln_reference_path(&self, group_key: &str) -> PathBuf` (`ln/<group>/reference.fits`), `pub fn ln_sidecar_path(&self, group_key: &str, stem: &str) -> PathBuf` (`ln/<group>/<stem>.athln`).

**The sidecar format (binary, little-endian, spec §5.2 "header, dims, stride, two f32 grids per channel, global scale/locations, relative scale factors"):**

```
magic  b"ATHLN\0\0\0" (8 bytes)     version u32 = 1
ref_width u32   ref_height u32   scale u32   channels u32
per channel:
  gw u32   gh u32
  global_scale f64   location_ref f64   location_tgt f64   relative_scale f64 (= global_scale, reserved for the PSF-scale weight)
  a: gw*gh f32   b: gw*gh f32
trailer: xxh3_64 of everything before it (u64) — the reader refuses a mismatch
```

- [ ] **Step 1: Write the failing tests** (`grid.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_grid_evaluates_to_its_constants_everywhere() {
        let g = LnGrid::constant(300, 200, 128, 1.5, -0.25); // stride 16 → gw 20, gh 14
        let (mut a, mut b) = (vec![0.0; 300], vec![0.0; 300]);
        for y in [0, 1, 17, 199] {
            g.evaluate_row(y, &mut a, &mut b);
            assert!(a.iter().all(|v| (v - 1.5).abs() < 1e-6), "row {y}");
            assert!(b.iter().all(|v| (v + 0.25).abs() < 1e-6), "row {y}");
        }
    }

    #[test]
    fn linear_ramp_is_reproduced_by_the_spline_between_nodes() {
        // B-spline interpolation reproduces linear functions exactly (up to f32).
        let mut g = LnGrid::constant(257, 129, 128, 1.0, 0.0); // stride 16 → gw 17, gh 9
        for j in 0..g.gh { for i in 0..g.gw { g.b[j * g.gw + i] = (i * 16) as f32 * 0.01 + (j * 16) as f32 * 0.02; } }
        let (mut a, mut b) = (vec![0.0; 257], vec![0.0; 257]);
        g.evaluate_row(40, &mut a, &mut b);
        for x in [0usize, 5, 16, 100, 255] {
            let expect = x as f32 * 0.01 + 40.0 * 0.02;
            assert!((b[x] - expect).abs() < 2e-3, "x {x}: {} vs {expect}", b[x]);
        }
    }

    #[test]
    fn sidecar_round_trips_and_rejects_a_flipped_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.athln");
        let mut g = LnGrid::constant(64, 48, 128, 1.1, 0.01);
        g.global_scale = 1.1; g.location_ref = 0.2; g.location_tgt = 0.19;
        let frames = LnFrameGrids { channels: vec![g.clone(), g] };
        frames.write(&p).unwrap();
        assert_eq!(LnFrameGrids::read(&p).unwrap(), frames);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[40] ^= 0x01;
        std::fs::write(&p, bytes).unwrap();
        assert!(LnFrameGrids::read(&p).is_err());
    }
}
```

- [ ] **Step 2: Run them, see them fail** — `cargo test -p athenaeum-core --lib stacking::ln::grid` → compile errors (types missing).

- [ ] **Step 3: Implement `grid.rs`**

The evaluator: for row `y`, `ty = y / stride`, `j0 = floor(ty)`, `fy = ty − j0`; B-spline weights for `fy` come from `crate::resample::kernels::Interpolation::BicubicBSpline.weights(frac, &mut [f32; 8])` (4 taps, first tap offset −1; clamp node indices into `[0, gh−1]`). For each output `x`, the same in `x`. Cache the four `x`-weights per column across rows? No — evaluate the four row-nodes' contributions into two temporary rows of length `gw` (a and b), then interpolate each output `x` from those with the four `x` weights; that is `O(4·gw + 4·W)` per row. Node placement: node `i` at pixel `i·stride`, last node at `(gw−1)·stride ≥ ref_width − 1` (`gw = (ref_width − 1) / stride + 2`), coordinates ≥ the last node clamp. `constant` fills both matrices. The sidecar writer streams with a `BufWriter` and an `xxh3` hasher (`xxhash_rust::xxh3::Xxh3` is already a dependency of core — check `Cargo.toml`); the reader validates magic, version, sizes (`a.len() == gw·gh`) and the trailer.

- [ ] **Step 4: Run the tests, see them pass.**

- [ ] **Step 5: Wire the module and the two path helpers** (`stacking/mod.rs` `pub mod ln;` gated like `register`; `paths.rs` helpers + one test each) and **commit**: `feat(stacking): LnGrid, the B-spline evaluator and the .athln sidecar (M2 Task 1)`.

---

### Task 2: Background model on the `scale/8` mesh

**Files:**
- Create: `crates/athenaeum-core/src/stacking/ln/background.rs`
- Test: in-file

**Interfaces:**
- Consumes: nothing new (pure function over one plane).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy)]
  pub struct BackgroundParams {
      pub scale: u32,             // 1024 → stride 128
      pub hot_radius: usize,      // 2
      pub low_clip: f32,          // 4.5e-5 (absolute, on the [0,1] scale)
      pub high_clip_rel: f32,     // 0.85 (of the plane maximum)
      pub deviation_sigma: f32,   // 3.0 reference / 3.2 target
      pub rejection_limit: f32,   // 0.3 per cell
  }
  #[derive(Debug, Clone)]
  pub struct BackgroundGrid { pub gw: usize, pub gh: usize, pub cells: Vec<f32>, pub invalid_cells: usize }
  /// One plane → its large-scale background on the stride grid (node (i, j) = the robust
  /// level of the stride×stride cell centred on (i·stride, j·stride)).
  pub fn background_grid(plane: &[f32], width: usize, height: usize, p: &BackgroundParams) -> BackgroundGrid;
  ```

**Algorithm (math §4.2; the thresholds' meaning is our reading — say so in the doc comment):**
1. Hot-pixel pass: a value replaced by the median of its `(2·hot_radius+1)²` window when it exceeds that median by more than `5·1.4826·MAD_window`; operate on a copy, sampled — only pixels above `high_clip_rel · max` candidates need the window (cheap).
2. Clipping: pixels `< low_clip` or `> high_clip_rel · max(plane)` are excluded (NaN in the copy).
3. Per cell: gather the cell's finite pixels (stride-2 subsample when the cell has more than 4096 pixels), iterate `median ± deviation_sigma · 1.4826 · MAD` clipping until the kept set is stable (max 5 rounds); if the rejected fraction exceeds `rejection_limit`, the cell is invalid; the cell level is the median of the kept set.
4. Invalid cells are filled by the mean of their valid 8-neighbours, repeated until none is left (a plane with no valid cell at all → `Err`-like: return a grid whose `invalid_cells == gw·gh` and let the caller refuse).
5. `invalid_cells` is reported per frame (`ln_cells_rejected` in the log).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn flat_plane_with_stars_recovers_the_flat_level() {
    let (w, h) = (512, 384);
    let mut plane = vec![0.10f32; w * h];
    for k in 0..200 { let (x, y) = ((k * 37) % w, (k * 91) % h); plane[y * w + x] = 0.9; } // bright points
    let g = background_grid(&plane, w, h, &BackgroundParams { scale: 256, hot_radius: 2, low_clip: 4.5e-5, high_clip_rel: 0.85, deviation_sigma: 3.0, rejection_limit: 0.3 });
    assert_eq!((g.gw, g.gh), (17, 13)); // stride 32
    assert!(g.cells.iter().all(|c| (c - 0.10).abs() < 1e-4), "{:?}", &g.cells[..8]);
    assert_eq!(g.invalid_cells, 0);
}

#[test]
fn vertical_gradient_is_tracked_per_cell() {
    let (w, h) = (256, 256);
    let plane: Vec<f32> = (0..w * h).map(|i| 0.05 + (i / w) as f32 * 1e-4).collect(); // +0.0256 top to bottom
    let g = background_grid(&plane, w, h, &BackgroundParams { scale: 256, ..DEFAULT_PARAMS });
    let top = g.cells[0]; let bottom = g.cells[(g.gh - 1) * g.gw];
    assert!(bottom - top > 0.020 && bottom - top < 0.030, "{top} {bottom}");
}

#[test]
fn a_cell_that_is_mostly_star_is_invalid_and_filled_from_neighbours() {
    let (w, h) = (256, 256);
    let mut plane = vec![0.10f32; w * h];
    for y in 0..40 { for x in 0..40 { plane[y * w + x] = 0.8; } } // a galaxy core covering cell (0,0)
    let g = background_grid(&plane, w, h, &BackgroundParams { scale: 256, ..DEFAULT_PARAMS });
    assert_eq!(g.invalid_cells, 1);
    assert!((g.cells[0] - 0.10).abs() < 1e-3);
}
```

- [ ] **Step 2: Fail** → **Step 3: implement** → **Step 4: pass** → **Step 5: commit** `feat(stacking): LN background model on the scale/8 mesh (M2 Task 2)`.

---

### Task 3: Relative scale by matched PSF fluxes + RCR

**Files:**
- Create: `crates/athenaeum-core/src/stacking/ln/scale.rs`
- Test: in-file, with the synthetic star-field helper from `stacking/measure.rs` tests (`synthetic_field` — if it is private, lift it into `stacking/test_fixtures.rs` as `pub(crate) fn synthetic_star_field(w, h, stars: &[(f64, f64, f64)], fwhm, noise, seed)`).

**Interfaces:**
- Consumes: `crate::stacking::psf_signal::{fit_stars, StarFit, PsfModel}` (fits with `mean_flux()`/positions), `crate::stacking::register::detect::detect_stars` (seeds), `crate::stacking::robust::rcr(values, limit) -> RcrResult { location, scale, kept, rejected }`, `crate::geometry::kdtree::KdTree2::nearest_within(x, y, radius)`.
- Produces:
  ```rust
  pub struct ScaleResult { pub scale: f64, pub sigma: f64, pub matches: usize, pub rejected: usize }
  /// Global relative scale s = RCR location of z_k = flux_ref,k / flux_tgt,k over stars matched
  /// within `match_radius_px` (4.0); both planes already in the reference geometry.
  pub fn relative_scale(reference: &[f32], target: &[f32], width: usize, height: usize,
                        psf: PsfModel, max_stars: usize, match_radius_px: f64, rcr_limit: f64) -> Result<ScaleResult, LnError>;
  ```
  `LnError::TooFewMatches { matches }` when fewer than 20 pairs survive (the frame is then excluded from the LN pass — the run marks it `excluded: "local normalization: N matched stars"` unless `rejection ≠ local`, in which case the frame keeps global normalization and a warning is logged; ruling below).

**Algorithm (math §4.3):** detect + fit on both planes (fast seeds, `max_stars` from `measurement.maxStars`); build a `KdTree2` over the reference centroids; for each target fit take `nearest_within(x, y, 4.0)` (square half-side 4 in the reference; the circle of radius 4 is close enough — say so); `z_k = flux_ref / flux_tgt` for pairs with both fluxes > 0; RCR with limit 0.3 → `scale = location`, `sigma = scale`. The second-pass "barycentre" match is skipped in M2 (recorded in the plan header as deferred to M4 if the first pass matches < 80 %).

- [ ] **Step 1: Failing tests**: (a) the same field scaled ×0.8 in the target → `scale ≈ 1.25 ± 0.01`, matches ≥ 90 % of the stars; (b) 10 % of the target stars replaced by ×3 outliers → RCR rejects them, scale still ≈ 1.25; (c) a target with no stars → `TooFewMatches`.
- [ ] **Steps 2–5** as above; commit `feat(stacking): LN relative scale from matched PSF fluxes with RCR (M2 Task 3)`.

---

### Task 4: The LN reference per group

**Files:**
- Create: `crates/athenaeum-core/src/stacking/ln/reference.rs`
- Modify: `crates/athenaeum-core/src/stacking/integrate.rs` (expose a `pub(crate) fn integrate_planes(...)` entry that `integrate_group` and this task share — the per-plane loop over `RegisteredSource` + `integrate_stack` without the stats/writing tail)
- Test: in-file, on the Plan 4 synthetic group fixture (`stacking::integrate::tests` has one — reuse via `test_fixtures`).

**Interfaces:**
- Produces:
  ```rust
  pub struct LnReference { pub width: usize, pub height: usize, pub planes: Vec<Vec<f32>>, pub frames_used: Vec<usize> }
  /// The best `n` included frames by weight, linear-fit rejection (5.0/3.5), global
  /// additive+scaling normalization, no weights (equal), no maps → one plane per channel in RAM.
  pub fn build_reference(input: &GroupInput<'_>, included: &[usize], n: usize, pool: &rayon::ThreadPool,
                         cancel: &AtomicBool, on_progress: &dyn Fn(usize, usize)) -> Result<LnReference, IntegrationError>;
  pub fn write_reference(r: &LnReference, path: &Path, cards: &[Card]) -> anyhow::Result<()>; // float32 FITS via the existing writer, IMAGETYP 'LN Reference', ATH_STK* cards + scanner-skip card
  pub fn read_reference(path: &Path) -> anyhow::Result<LnReference>;
  ```
- Ruling: the reference uses the `referenceFrames` best-weighted INCLUDED members of the group (spec §5.2); fewer than 3 included → the group falls back to global normalization with a warning (never a hard failure).

- [ ] Tests: (a) `build_reference` on the fixture's 6 frames with `n = 3` uses the three best weights (`frames_used` sorted by weight desc) and its plane has lower noise than any single frame; (b) `write_reference` → `read_reference` round-trips planes and geometry; (c) the reference's cards carry `ATH_STKI` and the scanner-skip card (the scanner test helper from Plan 3: `scanner_skips_file`).
- [ ] Commit `feat(stacking): LN reference per group (M2 Task 4)`.

---

### Task 5: The per-frame LN driver and the sidecar cache

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/ln/mod.rs` (driver), `crates/athenaeum-core/src/stacking/run.rs` (`Stage::Normalize` body + fan-out + artifacts + summary fields), `crates/athenaeum-core/src/stacking/plan.rs` (`stale_stages` gains `normalize`; `ln_cached` per group), `crates/athenaeum-core/src/stacking/provenance.rs` (`SummaryFrame.ln_scale`, `SummaryFrame.cached_ln`, `SummaryGroup.ln_reference_path`), `crates/athenaeum-core/src/stacking/config.rs` (the normalize stage hash: `normalization` subtree + upstream `register` + the reference member list), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (`ln_scale`, `ln_cells_rejected`, `ln_matches`), spec §9.3 (the `ln` kind) and §9.5.
- Test: `run.rs` tests on the fixture (LN enabled → sidecars exist, cached on the second run; LN disabled → stage is a no-op and no `ln/` files).

**Interfaces:**
- Produces:
  ```rust
  pub struct LnFrameOutcome { pub sidecar: PathBuf, pub scale: f64, pub matches: usize, pub cells_rejected: usize }
  /// Warps `frame` into the reference geometry (one-frame RegisteredSource, whole plane per channel),
  /// models both backgrounds, takes the PSF scale, builds A = s, B = B_ref − s·B_tgt on the stride grid,
  /// writes `<stem>.athln`.
  pub fn normalize_frame(reference: &LnReference, ref_backgrounds: &[BackgroundGrid], frame: &StackFrame,
                         cfg: &LocalNormalizationConfig, measure: &MeasureOptions, sidecar: &Path,
                         cancel: &AtomicBool) -> Result<LnFrameOutcome, LnError>;
  ```
- The stage in `run.rs`: per group, if `cfg.normalization.local.enabled` (or `rejection == Local`): resolve the LN reference (cached `reference.fits` when its artifact hash matches, else build + write + upsert artifact `kind = "ln_reference"`, `frame_id = NULL`), model its backgrounds once, then fan out `normalize_frame` over the included frames with the measure stage's admission (working set = one warped frame per channel + two grids ≈ `channels × W × H × 4 × 2` bytes), skipping frames whose `ln` artifact is fresh (path + size + hash); progress ticks per frame; the outcomes fill `SummaryFrame.ln_scale` and the group's `ln_reference_path`; a `TooFewMatches` frame → excluded with reason when LN is the output normalization, else warned.
- The `normalize` stage hash: `stage_hash(config.normalization (+ measurement.psfModel, maxStars), &["register"], sources = the reference members' identities + this frame's registration row hash)`.

- [ ] Tests first (`run.rs`): `local_normalization_writes_one_sidecar_per_included_frame_and_a_reference`, `local_normalization_sidecars_are_cached_on_the_second_run` (`cached_ln` true, stage duration < 1 s), `local_normalization_off_leaves_no_ln_files`.
- [ ] Commit `feat(stacking): stage 6 builds the LN reference and per-frame sidecars, cached as artifacts (M2 Task 5)`.

---

### Task 6: The engine hook — LN as output and rejection normalization

**Files:**
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (`StackParams.local`, band loop), `crates/athenaeum-core/src/integration/stats.rs` (`RejectionNormalization::Local` arm), `crates/athenaeum-core/src/stacking/integrate.rs` (`GroupInput.ln: Option<&[Option<LnFrameGrids>]>`, threading, `GroupStats.ln_frames`)
- Test: `engine.rs` tests + the Plan 4 byte-identical pins (must still pass with `local: None`).

**Interfaces:**
- `StackParams<'a>` gains `pub local: Option<&'a [Option<&'a LnGrid>]>` (one grid per frame for the plane being integrated; `None` for a frame without a sidecar → that frame uses its global pair) and `pub local_for_rejection: bool`, `pub local_for_output: bool`.
- Inside `integrate_stack`'s band loop, per band: for each frame with a grid, evaluate `a_row`/`b_row` for every row of the band into a per-frame scratch (`rows × width × 2` f32 — budgeted: the band-row budget divides by `1 + 2·(frames with grids)/frames`… simpler ruling: the scratch is allocated once per band as `frames × 2 × width` and evaluated row by row inside the existing per-row loop, so the memory cost is `2·frames·width` floats, negligible), then apply `v′ = a·v + b` to the working copy before rejection when `local_for_rejection`, and to the output sample when `local_for_output` (in place of the global pair's `offset/scale` for that frame — the global pair still applies to frames without a grid).

- [ ] Tests first: (a) two frames, the second `= 0.5·first + gradient` — with LN grids (`A = 2`, `B = −2·gradient` sampled) and `local_for_output`, the average equals the first frame within 1e-4 everywhere; (b) with `local_for_rejection` and a hot pixel on the second frame, the linear-fit rejection rejects it where the global pair would not (the gradient hides it); (c) the Plan 4 byte-identical pin tests unchanged (`local: None`).
- [ ] Commit `feat(integration): local-normalization grids in the band loop (M2 Task 6)`.

---

### Task 7: Wiring, summary, TS types, the probe

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (pass the sidecars read back from artifacts into `GroupInput.ln`), `api/stacking.rs`, `ts_export.rs` (nothing new to register if only fields change — regenerate), `src/types/stacking.ts` (generated), `CLAUDE.md` → Stacking (M2 paragraph), spec §14 M2 marked as executed.
- Create: `crates/athenaeum-core/examples/ln_probe.rs` — `--set <id> --group <key> [--frames N] [--scale 1024]`: builds the reference from the cached registered rows, normalizes one named frame, prints the scale, sigma, matches, cells rejected, and the residual background (median of `(A·v+B) − ref` on the mesh) before/after.
- [ ] `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` + commit the generated file; `ts_contract` green.
- [ ] Commit `feat(stacking): LN summary fields, TS types, ln_probe (M2 Task 7)`.

---

### Task 8: The tab — `NormalizePanel` LN block live, stage summary, results

**Files:**
- Modify: `src/components/stacking/panels/NormalizePanel.tsx` (enable the LN block: `enabled`, `scale` (256–4096, step 256), `referenceFrames` (3–50), `psfModel`; `localScale` stays disabled with the tooltip "Local scale model arrives in M4"; the rejection select's `local` option enabled iff `local.enabled`), `src/components/stacking/stageSummary.ts` (Normalize summary: `Local · scale 1024 · ref 20 frames` / `Global · …`; Integrate summary mentions `LN rejection` when selected), `src/components/stacking/ResultsPanel.tsx` (the group card shows `LN reference: 20 frames` and the reference path with the copy button), `src/components/stacking/FramesTable.tsx` (a `Scale` column from `lnScale`, 3 decimals, `—` when absent), `src/components/settings/StackingSection.tsx` (nothing — it renders the same panels).
- Gates: `npx tsc --noEmit`, `VITE_TARGET=web npx vite build`; a recorded browser smoke (the controller's Chrome against the same-origin static build: toggle LN on, save, plan shows `normalize` stale; run; the frames table shows scales).
- [ ] Commit `feat(stacking-ui): local normalization block live; LN in summaries, results and the frames table (M2 Task 8)`.

---

### Task 9 (controller-run): M2 acceptance re-run on LDN 1272

**Files:**
- Create: `docs/superpowers/research/2026-09-10-m2-acceptance-run.md`; Modify: `docs/superpowers/open-items.md` (Stacking M2 subsection).

**Setup:** the same dev catalog, folders and web build as the M1 run (`2026-09-09-m1-acceptance-run.md` §2); the reference pinned to `2025-10-18_02-02-02_0073` (Checkpoint B's geometry); per-set config = Default + `normalization.local.enabled = true`, `normalization.rejection = "local"`. Calibrated and measured artifacts are cached from M1 if the working folder still holds them (else the run recalibrates — 7 min).

- [ ] **Step 1:** Run stacking (re-run from Register or a full run); record every stage's `durationMs`, the LN stage's per-frame rate, the sidecar bytes (`lnBytes`), the reference build time.
- [ ] **Step 2: Targets (spec §13, re-measured with LN on both sides):** rejected fraction **1–4 %** per group (external 2.831 % mono; 2.51 / 2.79 / 2.64 % OSC per channel) — the named cause of M1's 0.65 %; master MRS noise within **5 %** of the external master's (mono 1.9570e-05 by the log, i.e. ours ≤ 1.0 ratio expected — record the ratio; the OSC per-channel ratios vs 1.6716e-05 / 1.6631e-05 / 1.4975e-05); FWHM unchanged vs M1 (2.71 px); artifacts: no residual trail at 400 % around (5760, 1700), and **no mesh imprint** (a per-row and per-column profile of `master_LN − master_M1` shows no periodic structure at the 128 px stride); frames 208/208 + 160/160 (or exclusions named with `TooFewMatches` reasons); LN stage ≤ 10 min for 368 frames on this Mac (attributed like M1's measure time if memory-bound); disk: sidecars ≤ 2 MB per frame.
- [ ] **Step 3:** `ln_probe` on one mono frame: the residual background after normalization is flat to within 2× the noise (median absolute residual on the mesh < 2·MRS noise of the reference).
- [ ] **Step 4:** Re-run from Integrate with the sidecars cached → `cached_ln` 368/368; Delete intermediates removes `ln/` too (usage `lnBytes → 0`).
- [ ] **Step 5:** Write the note (setup, timings, the §13 table with verdicts, the pixel comparison vs the external masters via the Checkpoint B scripts, findings, rulings), update open-items, commit `docs(stacking): M2 acceptance run on LDN 1272`.

---

## Self-review (done while writing)

**Spec coverage.** §5.2 reference (Task 4), background models + PSF scale + grid construction + sidecar (Tasks 1–3, 5), output + rejection use (Task 6), `NormalizePanel` LN block (Task 8), §9.3 artifact caching (Task 5), §9.5 names (Task 1), §13 acceptance (Task 9), §14 M2 list — every item mapped. Not here by design: the local scale spline (`localScale`, TPS — M4), the barycentre second-pass match (M4 if needed), drizzle's use of the grids (M3 reads the same sidecars).

**Placeholder scan.** Every task names its files, functions, signatures and tests; the sidecar format is byte-specified; the algorithms cite the math reference sections whose meaning is our reading (background thresholds) and say so.

**Type consistency.** `LnGrid`/`LnFrameGrids` (Task 1) are what Task 5 writes and Task 6 consumes (`Option<&LnGrid>` per frame per plane); `BackgroundGrid` (Task 2) feeds the `B` matrix in Task 5; `ScaleResult.scale` (Task 3) is `LnGrid.global_scale`; `LnReference.planes` (Task 4) are the reference planes Task 5 models; `GroupInput.ln` (Task 6) is filled by Task 7's wiring; `SummaryFrame.ln_scale`/`cached_ln` and `SummaryGroup.ln_reference_path` (Task 5) are what Task 8 renders after Task 7 regenerates the TS types.
