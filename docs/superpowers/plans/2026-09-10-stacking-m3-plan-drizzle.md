# Stacking M3 — Drizzle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drizzle (spec §7, §14 M3) — the per-frame rejection bitmaps M1's engine already computes but never keeps, exact-clipping forward-mapped drops onto a 1×/2×/3× output grid with the run's weights and local normalization, the weight map, the scaled WCS, `DrizzlePanel` live, and the LDN 1272 acceptance run: the drizzled/undrizzled FWHM ratio against the external 2× drizzled masters.

**Architecture:** stage 7 (`Stage::Integrate`) gains an optional sink for the survivor masks it already builds per pixel: when drizzle is on (and `useRejection`), every band's per-frame "rejected" bits are written to `rej/run-<id>/<group>/<stem>.rej` (one bit per pixel per channel, plain words, temporary). Stage 8 (`Stage::Drizzle`) then runs per group right after the master is written, in the same `process_group_output` call: per plane it allocates one `I`/`W` accumulator pair in output geometry, reads each included frame's calibrated plane once, and deposits every finite non-zero non-rejected source pixel's drop — the square `[x ± dropShrink/2]` mapped forward through the frame's `PixelMap` (subject → reference) and scaled onto the output grid — into the output pixels it overlaps, with the exact polygon-clipped area as `a`, the frame's plane weight as `w`, and the frame's normalization `N` (its LN grid at the reference coordinate when available, else its global output pair) applied to the value. Work is split into bands of 512 output rows processed in parallel; each band iterates only the source window that is the inverse image of its rectangle. Final `I/W` where `W > 0` is level-preserving (a uniform field comes out at the input level), `W / max(W)` is the weight map. Output `<master stem>_drizzle<s>x.fits` with the reference's WCS scaled (the stored record's 0-based `crpix' = s·crpix + (s−1)/2` — the card, being `crpix + 1`, comes out as `s·CRPIX − (s−1)/2` in 1-based terms; `CD' = CD/s`; SIP `A'_pq = A_pq·s^(1−p−q)`) and the `ATH_DRZ`/`ATH_DRZP`/`ATH_DRZK` cards. The bitmaps are removed at the end of the run unless `output.cleanup = keepAll`.

**Tech Stack:** Rust (athenaeum-core `stacking/drizzle/*`, `stacking/rej.rs`, `integration/engine.rs`, `fits_writer/wcs.rs`), React/TS (`panels/DrizzlePanel.tsx`, `stageSummary.ts`, `ResultsPanel.tsx`), ts-rs regeneration, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §6.2 (per-frame rejection bitmaps), §7, §9.2 `drizzle:`, §9.5 (`rej/run-<id>/…`, `…_drizzle<s>x.fits`, `…_drizzle<s>x_weight.fits`), §10.2 (`drizzlePath`), §11.2 `DrizzlePanel`, §13 (drizzle 2× target, ≤ 15 min per group), §14 M3 · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §4.5 (LN inside drizzle), §6 (equations, inverse-mapping formulation, kernels) · **Baselines:** `docs/superpowers/research/2026-09-10-m2-acceptance-run.md` (run 6/7 masters, FWHM 2.683 mono / [2.81, 2.74, 2.61] OSC), the external 2× drizzled masters `~/Pictures/Calibration Test/LDN1272-Output/master/masterLight_…_mono_drizzle_2x.xisf` and `…_RGB_drizzle_2x.xisf`.

## Global Constraints

- No new crate dependencies; `tracing` only (no `println!`/`eprintln!` outside tests/examples); never name other software in code or comments (the math reference is the only place that does).
- Two backends in sync: no new commands are expected; if one is added, Tauri + Axum + `invoke_handler` + `build_router` + `ts_export.rs` in the same task.
- Headless build: `cargo check -p athenaeum-core --no-default-features` must stay clean — everything under `stacking/` is gated `#[cfg(all(feature = "render", feature = "solver"))]` like its siblings.
- New log field names go into the "Unified event schema" dictionary of `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the same task.
- rustfmt only on new leaf files and files that are rustfmt-clean today (`rustfmt --check` first); never `integration/engine.rs`, `integrate.rs`, `stats.rs`, `weights.rs`, `run.rs`, `plan.rs`, `mod.rs`, `ts_export.rs`, `schema.rs`.
- Serde names are the spec §9.2 ones verbatim: `drizzle.{enabled, scale, dropShrink, kernel, useRejection, useWeights, useLocalNormalization, writeWeightMap}`; `kernel ∈ square | circle | gaussian`.
- Every pixel path runs on the same units the engine uses (float32 in [0, 1] as read from the calibrated artifacts).
- The engine's byte-identical pins from Plan 4 and the M2 no-grid pins must keep passing with drizzle off (the new sink is `None` for every existing caller — no behaviour change).
- Coordinate convention (the whole pipeline's): integer pixel coordinates are pixel CENTRES; pixel `(x, y)` covers `[x − 0.5, x + 0.5] × [y − 0.5, y + 0.5]`; `PixelMap::forward` maps subject → reference, `inverse` reference → subject, both in that convention.
- Commit as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit …` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY` trailers.

## Rulings made while planning (binding unless the spec says otherwise)

- **R-M3-1 Deposition, not gathering.** Spec §7 names the inverse image only to bound each tile's source window; the per-pixel work is forward: map the drop's four corners, clip against the output pixels the mapped quad touches. This is the formulation the plan implements (math §6.3's per-output-pixel loop is equivalent and slower).
- **R-M3-2 Units and level.** `a` is measured in OUTPUT-pixel area units; `I += a·w·N(d)`, `W += a·w`; the result is `I / W` where `W > 0`, else `0`. No `s²` and no `dropShrink²` factor anywhere: a uniform input field comes out at exactly the input level for every `scale`/`dropShrink` (the pinned test). The math reference's `s²`/`dropShrink²` factors belong to a different area normalization and are deliberately not copied.
- **R-M3-3 Kernels.** `square` = exact polygon clipping (Sutherland–Hodgman of the convex mapped quad against each unit output pixel, shoelace area). `circle` and `gaussian` = the `kernelGridSize² = 16 × 16` micro-drop table (math §6.3): the drop is subdivided into 256 sub-drops of side `dropShrink/16`; each sub-drop carries the kernel's weight at its centre (circle: 1 inside radius `dropShrink/2`, 0 outside; gaussian: `exp(−r²/(2σ²))`, `σ = (dropShrink/2)/sqrt(−2·ln 0.025)`), the 256 weights normalized to sum to `dropShrink²` (so a square LUT would reproduce the exact-clip mass); each sub-drop's centre is mapped forward and its whole weight lands in the output pixel that contains it. Point deposition of 256 sub-drops is the tabulated-kernel approach; it is not exact clipping and the panel's help text says "square = exact, circle/gaussian = 16×16 tabulated".
- **R-M3-4 Rejection lookup per source pixel.** The frame's `.rej` bit is read at the rounded reference coordinate of the drop CENTRE (`round(u), round(v)`); a set bit skips the whole drop. (Math §6.3 looks it up per output pixel — same information, one lookup per source pixel instead of per overlap pair.)
- **R-M3-5 LN lookup per source pixel.** With `useLocalNormalization` and a frame that has LN grids, `N(d) = a·d + b` with `(a, b)` read at `(round(u), round(v))` from the frame's grid evaluated into two full reference-geometry planes once per frame per plane (`LnGrid::evaluate_row_into` over every row — the same cost the integration already pays per band; the two planes are reused buffers). Without LN (toggle off, LN disabled for the run, the group's ruling-R3 fallback, or a frame that has no sidecar) `N(d) = os·d + oo` with the frame's global OUTPUT pair — the very pair the integration applied, handed back by `integrate_group` (Task 2 adds `GroupOutput.output_pairs`).
- **R-M3-6 Whole source plane in RAM, bands of 512 output rows.** One `PlaneReader::read_plane` per included frame per plane (≈ 104 MB mono); bands are processed by `rayon` `par_chunks_mut` over `I`/`W` (512 output rows each, the last one shorter); a band's source window is the bounding box of the inverse image of its rectangle (the four corners plus one sample every 32 output px along all four edges, exactly `RegisteredSource`'s technique), grown by `dropShrink/2 + 1` source px on every side and clamped to the frame. Drops that straddle a band boundary are deposited by both bands, each into its own pixels only — no double counting, no seam (Task 7 checks a 512-row fold).
- **R-M3-7 Memory refusal, not swapping.** Before allocating, the stage computes `need` bytes; when `need > total_ram_bytes()/2` (the fan-out's existing probe; unknown total → refuse above 4 GB) the GROUP's drizzle fails with `"drizzle {s}x needs ≈ {need} but only {have} is available; use a smaller scale"` — the master is already written, the group row keeps its `master_path`, `drizzle_path` stays `NULL`, the run's warnings carry the message, the run itself is not failed. AMENDED by ruling R-M3-15 (Task 3 fix round 1, review found the original formula under-counted real peak by up to ~75%): `need = estimate_memory_bytes(width, height, channels, scale, ln, write_weight_map)` = `(channels + 2)·out_w·out_h·4` (the output planes plus one `I`/`W` accumulator pair) `+ (write_weight_map ? channels·out_w·out_h·4 : 0)` (the weight-map planes) `+ out_w·out_h·4` (`measure_plane`'s own ADU-scaled copy of the plane it is currently measuring) `+ width·height·4` (one full-resolution source plane) `+ (ln ? 2·width·height·4 : 0)` (two reference-geometry LN grid planes) — the original `w·h·4 + (2·w·h·4 if LN)` tail is a subset of this; the weight-map and measure-copy terms were the omission.
- **R-M3-8 `.rej` files are per-run temporaries.** Written only when `drizzle.enabled && drizzle.useRejection`; removed at the very end of the run — success, failure or cancel — unless `output.cleanup = keepAll`; `WorkUsage.rej_bytes` and `CleanupWhat::Intermediates`/`All` cover `rej/`. Files are opened per write (never 200 handles at once — macOS GUI processes default to a 256-descriptor soft limit).
- **R-M3-9 Re-run.** Drizzle has no cache of its own: `rerunFrom: "drizzle"` is clamped to `integrate` (the bitmaps come from integration) with a `debug!`; `stale_stages` never lists `drizzle`.
- **R-M3-10 Ranges.** `scale ∈ {1, 2, 3}` (anything else → the plan's `unsupported` blocker "drizzle scale must be 1, 2 or 3"), `dropShrink ∈ [0.5, 1.0]` (outside → clamped at run time with a `warn!` and a run warning). 1× drizzle is allowed (it is shift-and-add with sub-pixel drops).
- **R-M3-11 Normalize timing.** The M2 final fix wave (A1) gives `normalize` its own `StageTiming`; drizzle pushes `StageTiming { stage: Drizzle }` once after the group loop, between `integrate` and `output`, only when drizzle is on. `TIMED_STAGES` in `stageSummary.ts` gains `'drizzle'` after `'integrate'`.

---

## File structure

- Create `crates/athenaeum-core/src/stacking/drizzle/mod.rs` — the stage driver `drizzle_group`, `DrizzleInput`/`DrizzleOutput`/`DrizzleStats`, the band scheduler, the memory check.
- Create `crates/athenaeum-core/src/stacking/drizzle/geom.rs` — pure geometry: drop corners, forward mapping onto the output grid, convex-quad ∩ unit-pixel clipping, the micro-drop kernel table.
- Create `crates/athenaeum-core/src/stacking/rej.rs` — the `.rej` bitmap format: `RejBitmapSet` (writer, one file per frame), `RejBitmap` (reader), the engine-facing `RejectionBitSink` adapter.
- Modify `crates/athenaeum-core/src/integration/engine.rs` — `StackParams.rejection_bits: Option<&dyn RejectionBitSink>`; the band loop fills a `[row][frame][word]` bit buffer from the per-pixel `present && !survivor` test it already performs, hands it to the sink after each band.
- Modify `crates/athenaeum-core/src/integration/source.rs` (or `mod.rs`) — the `RejectionBitSink` trait lives next to `FrameSource`.
- Modify `crates/athenaeum-core/src/stacking/integrate.rs` — `GroupInput.rej: Option<&RejBitmapSet>`; `integrate_planes` binds the set to each plane; `GroupOutput.output_pairs: Vec<Vec<NormalizationPair>>` (per plane, per included frame, engine order).
- Modify `crates/athenaeum-core/src/fits_writer/wcs.rs` — `scale_plate_solve(&PlateSolveRecord, u32) -> PlateSolveRecord`.
- Modify `crates/athenaeum-core/src/stacking/master_cards.rs` — `drizzle_file_names`, `build_drizzle_cards`, `weight_map_cards`, `write_drizzled_master`.
- Modify `crates/athenaeum-core/src/stacking/paths.rs` — `rej_root`, `rej_run_dir(run_id)`, `rej_dir(run_id, group_key)`, `rej_path(run_id, group_key, stem)`; `WorkUsage.rej_bytes`; `cleanup_work` removes `rej/`; `estimate_bytes` gains the bitmaps and the drizzled outputs.
- Modify `crates/athenaeum-core/src/stacking/run.rs` — integration passes the sink when drizzle is on; the Drizzle stage per group after the master write; timings; summary/DB fields; `rej/run-<id>` removal at the run's single exit path; the `rerunFrom` clamp.
- Modify `crates/athenaeum-core/src/stacking/plan.rs` — Gate 6's "Drizzle arrives in M3" blocker replaced by the scale-range check; the estimate.
- Modify `crates/athenaeum-core/src/stacking/config.rs` — `MaximumQuality` preset turns drizzle 2× on (its doc comment and test updated).
- Modify `crates/athenaeum-core/src/stacking/provenance.rs` — `SummaryGroup.{drizzle_path, weight_map_path, drizzle: Option<DrizzleStats>}` (`#[serde(default)]`); `crates/athenaeum-core/src/db/stacking.rs` — `GroupUpdate.drizzle_path`; `crates/athenaeum-core/src/ts_export.rs` — `DrizzleStats`; regenerate `src/types/stacking.ts`.
- Modify `src/components/stacking/panels/DrizzlePanel.tsx` (live), `src/components/stacking/PipelineBoard.tsx` (the row toggle live), `src/components/stacking/stageSummary.ts` (`TIMED_STAGES`, the `unsupported` disambiguation, the estimate helper), `src/components/stacking/ResultsPanel.tsx` (drizzle lines on the master card), `src/components/stacking/StackingTab.tsx` (the toggle handler), `src/components/settings/StackingSection.tsx` (passes `onChange` to the panel).
- Modify `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary: `drizzle_scale`, `rej_bytes`, `out_width`, `out_height`), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §7 (the rulings above, one line each), `CLAUDE.md` → Stacking (M3 paragraph).
- Create `docs/superpowers/research/2026-09-1x-m3-acceptance-run.md` (Task 7).

---

### Task 1: Drizzle geometry — drops, mapping, exact clipping, kernel table

**Files:**
- Create: `crates/athenaeum-core/src/stacking/drizzle/mod.rs` (module root: `pub mod geom;` + the config-facing enum re-exports only, the driver arrives in Task 3), `crates/athenaeum-core/src/stacking/drizzle/geom.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (`pub mod drizzle;`)
- Test: in-file `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::geometry::pixel_map::PixelMap` (`forward(x, y) -> (f64, f64)`), `crate::stacking::config::DrizzleKernel`.
- Produces:
  ```rust
  /// Output-grid coordinate of a reference coordinate: `v = s·u + (s − 1)/2`
  /// (pixel centres convention on both sides; reference pixel `i` covers output pixels `s·i .. s·i + s − 1`).
  #[inline] pub fn to_output(u: f64, scale: u32) -> f64;
  /// Reference coordinate of an output coordinate (the inverse of `to_output`).
  #[inline] pub fn to_reference(v: f64, scale: u32) -> f64;
  /// The four corners of source pixel `(x, y)`'s drop, counter-clockwise:
  /// `(x − h, y − h), (x + h, y − h), (x + h, y + h), (x − h, y + h)` with `h = drop_shrink / 2`.
  pub fn drop_corners(x: usize, y: usize, drop_shrink: f64) -> [(f64, f64); 4];
  /// Map the four corners subject → reference → output grid. Returns the quad and its
  /// integer bounding box `(x_min, y_min, x_max, y_max)` in output pixels (inclusive, pixel
  /// index = round of the coordinate, i.e. the pixel whose extent contains it), or `None`
  /// when any mapped coordinate is not finite.
  pub fn map_drop(map: &PixelMap, corners: &[(f64, f64); 4], scale: u32) -> Option<(Quad, (i64, i64, i64, i64))>;
  pub type Quad = [(f64, f64); 4];
  /// Area of `quad ∩ [px − 0.5, px + 0.5] × [py − 0.5, py + 0.5]` by Sutherland–Hodgman
  /// clipping (four half-planes) and the shoelace formula; `quad` must be convex (an
  /// affine or mildly distorted image of a square is), any orientation. Returns 0 when
  /// the polygon is empty.
  pub fn clip_area(quad: &Quad, px: i64, py: i64) -> f64;
  /// Signed-area orientation fix: the clipping assumes a counter-clockwise polygon;
  /// a flipped `PixelMap` mirrors the quad, so `map_drop` reverses the vertex order when
  /// the shoelace area of the mapped quad is negative.
  /// The 16 × 16 micro-drop table for the tabulated kernels (ruling R-M3-3): sub-drop
  /// centre offsets from the pixel centre (source px) and weights summing to `drop_shrink²`.
  pub struct KernelTable { pub offsets: Vec<(f64, f64)>, pub weights: Vec<f64> }
  pub const KERNEL_GRID_SIZE: usize = 16;
  pub const KERNEL_EPSILON: f64 = 0.025;
  pub fn kernel_table(kernel: DrizzleKernel, drop_shrink: f64) -> Option<KernelTable>; // None for Square
  ```
- The whole `stacking/drizzle` module is gated like its siblings (`#[cfg(all(feature = "render", feature = "solver"))]` at the `mod` line in `stacking/mod.rs`).

- [ ] **Step 1: failing tests** (write them first, run `cargo test -p athenaeum-core --lib stacking::drizzle::geom` — they fail to compile):
  - (a) `to_output`/`to_reference` round-trip for s = 1, 2, 3 and `to_output(0.0, 2) == 0.5`, `to_output(-0.5, 2) == -0.5` (the left edge of reference pixel 0 is the left edge of output pixel 0), `to_output(0.5, 3) == 2.5`.
  - (b) identity map, s = 1, `drop_shrink = 1.0`: `clip_area` of pixel (3, 4)'s mapped drop against output pixel (3, 4) is `1.0 ± 1e-12`, against (4, 4) it is `0.0`.
  - (c) identity map, s = 2, `drop_shrink = 1.0`: pixel (0, 0)'s drop covers output pixels (0,0), (1,0), (0,1), (1,1) with area `1.0` each (sum 4 = s²); with `drop_shrink = 0.5` the same four pixels get `0.25` each (sum 1 = s²·dropShrink²).
  - (d) a translation-only map by (+0.5, 0) at s = 2, `drop_shrink = 1.0`: pixel (0,0)'s drop → output columns 1 and 2 get area 1.0 each in rows 0 and 1, columns 0 and 3 get 0 — the bbox from `map_drop` is `(1, 0, 2, 1)`.
  - (e) a 30° rotation about the origin, s = 1, `drop_shrink = 0.9`: the sum of `clip_area` over the bbox pixels equals `0.81 ± 1e-9` (area is conserved by the clipping).
  - (f) a flipped map (`Linear` with a negative determinant): `map_drop` returns a counter-clockwise quad (shoelace area > 0) and (e)'s conservation still holds.
  - (g) `kernel_table(Circle, 0.9)`: 256 entries, weights ≥ 0, sum `0.81 ± 1e-9`, every offset with `|o| > 0.45` has weight 0; `kernel_table(Gaussian, 0.9)`: the centre-most sub-drop has the largest weight and the corner ones are `≈ 0.025 ×` the centre's (within a factor 2 — the table is normalized, so compare ratios); `kernel_table(Square, _)` is `None`.
  - (h) `map_drop` with a `PixelMap` whose forward yields NaN (build a `Linear` with a NaN entry if the constructor allows; otherwise skip this case with a comment) → `None`.
- [ ] **Step 2: implement** `geom.rs` (pure `f64`, no allocation in `clip_area` — a fixed `[(f64, f64); 16]` vertex buffer is enough for a convex quad clipped by four half-planes), `mod.rs`, the `pub mod drizzle;` line.
- [ ] **Step 3: tests pass**, `rustfmt` the two new files, `cargo check -p athenaeum-core --no-default-features`.
- [ ] **Step 4: commit** `feat(stacking): drizzle geometry — drops, forward mapping, exact clipping, kernel table (M3 Task 1)`.

---

### Task 2: Per-frame rejection bitmaps — the `.rej` format and the engine sink

**Files:**
- Create: `crates/athenaeum-core/src/stacking/rej.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (`pub mod rej;`), `crates/athenaeum-core/src/integration/source.rs` (the trait), `crates/athenaeum-core/src/integration/engine.rs` (`StackParams.rejection_bits`, the band buffer), `crates/athenaeum-core/src/stacking/integrate.rs` (`GroupInput.rej`, `GroupOutput.output_pairs`, `integrate_planes` binding), `crates/athenaeum-core/src/stacking/paths.rs` (`rej_*` helpers, `WorkUsage.rej_bytes`, `cleanup_work`), `crates/athenaeum-core/src/stacking/ln/reference.rs` (its `integrate_planes` call passes `rej: None` — signature change only)
- Test: `rej.rs` in-file; `engine.rs` tests (the existing pins untouched + one sink test); `integrate.rs` test on the Plan 4 fixture; `paths.rs` usage/cleanup test.

**Interfaces:**
- Produces (`integration/source.rs`):
  ```rust
  /// Receives one band's per-frame rejection bits from `integrate_stack` (spec §6.2 "per-frame
  /// rejection bitmaps"). `bits` is laid out `[row_in_band][frame][word]` — `rows × n × words_per_row`
  /// u64 words, bit `x % 64` of word `x / 64` set when frame `frame` was PRESENT at `(x, y0 + row)` and
  /// NOT a survivor (algorithm or range rejection; a missing/non-finite sample is not a rejection).
  /// Called from the band loop's single-threaded tail, once per band, in band order.
  pub trait RejectionBitSink: Sync {
      fn words_per_row(&self) -> usize;   // == ceil(width / 64)
      fn frames(&self) -> usize;          // == n
      fn record_band(&self, y0: usize, rows: usize, bits: &[u64]) -> Result<(), IntegrationError>;
  }
  ```
- Produces (`stacking/rej.rs`):
  ```rust
  pub const REJ_MAGIC: &[u8; 8] = b"ATHREJ01";
  /// Header (24 bytes, little-endian): magic, u32 width, u32 height, u32 channels, u32 words_per_row;
  /// then `channels × height × words_per_row` u64 words. No trailer: a per-run temporary.
  pub struct RejBitmapSet { dir: PathBuf, paths: Vec<PathBuf>, width: usize, height: usize, channels: usize, words: usize }
  impl RejBitmapSet {
      /// Creates `dir` and one zero-filled file per stem (`<stem>.rej`, `set_len` to the exact size,
      /// header written). Refuses an existing file (never overwrite; the run id makes the dir unique).
      pub fn create(dir: &Path, stems: &[String], width: usize, height: usize, channels: usize) -> anyhow::Result<RejBitmapSet>;
      pub fn plane_sink(&self, plane: usize) -> RejPlaneSink<'_>;   // implements RejectionBitSink
      pub fn path(&self, frame: usize) -> &Path;
      pub fn bytes(&self) -> u64;                                    // total on disk
  }
  /// `record_band` opens each frame's file, `write_at`/`seek_write`s the band's rows for that
  /// frame (`rows × words × 8` bytes at `24 + (plane·height + y0)·words·8`), closes it. Frames are
  /// written sequentially; the 208-open-files problem never arises (ruling R-M3-8).
  pub struct RejPlaneSink<'a> { … }
  /// One frame's bitmap, read whole (validated: magic, geometry == expected, file length ==
  /// 24 + channels·height·words·8 BEFORE any allocation; a short/foreign file is an error).
  pub struct RejBitmap { width: usize, height: usize, channels: usize, words: usize, bits: Vec<u64> }
  impl RejBitmap {
      pub fn read(path: &Path, width: usize, height: usize, channels: usize) -> anyhow::Result<RejBitmap>;
      #[inline] pub fn is_rejected(&self, plane: usize, x: usize, y: usize) -> bool;
  }
  ```
- Produces (`integration/engine.rs`): `StackParams` gains `pub rejection_bits: Option<&'a (dyn RejectionBitSink + 'a)>` — every existing caller passes `None`. Inside the band job, when `Some`: allocate (once per band, outside the per-row parallel loop) `band_bits = vec![0u64; rows × n × words]`, zip `band_bits.par_chunks_mut(n × words)` into the existing `for_each_init` row iterator (the same `.zip` chain as `low_band`/`high_band`), and in the per-pixel loop where `row_rejected[i] += 1` is counted (`present && !survivor`) also `row_bits[i × words + x / 64] |= 1 << (x % 64)`; after the parallel loop, `sink.record_band(y0, rows, &band_bits)?`. When `None`, no buffer and no branch inside the loop body beyond the one `if let Some` that selects the closure variant — mirror how `maps` is handled so the no-sink path stays byte-identical.
- Produces (`stacking/integrate.rs`): `GroupInput.rej: Option<&'a RejBitmapSet>` (`None` for every M1/M2 caller and for `build_reference`); `integrate_planes(…, rej: Option<&RejBitmapSet>, …)` binds `rej.map(|r| r.plane_sink(p))` per plane; `GroupOutput.output_pairs: Vec<Vec<NormalizationPair>>` — per plane, per included frame (engine order), the OUTPUT pair `integrate_planes` computed from `stats.rs` (the same values it put into `StackParams.output`). `RejBitmapSet::create` is called by the RUN (Task 5), not here — `integrate_group` only consumes it; the set's `frames` must equal the number of INCLUDED frames and its stems be in engine order (`integrate_group` checks `rej.frames() == included.len()` → `IntegrationError::BadInput` otherwise).
- Produces (`stacking/paths.rs`): `rej_root()` = `root/rej`, `rej_run_dir(run_id)` = `root/rej/run-<id>`, `rej_dir(run_id, group_key)`, `rej_path(run_id, group_key, stem)` = `…/<stem>.rej`; `WorkUsage.rej_bytes` (+ in `total_bytes`); `cleanup_work`: `Intermediates` and `All` also remove `rej_root()`; `INTERMEDIATE_ARTIFACT_KINDS` unchanged (bitmaps are not artifacts).

- [ ] **Step 1: failing tests**:
  - `rej.rs`: (a) create a 3-frame set 100 × 70 × 2 planes → three files of `24 + 2·70·2·8` bytes, header round-trips; (b) `plane_sink(1).record_band(10, 5, bits)` with bit (x = 65, frame 2) set in row 3 → `RejBitmap::read(path(2))` reports `is_rejected(1, 65, 13) == true`, `is_rejected(1, 64, 13) == false`, `is_rejected(0, 65, 13) == false`; (c) `read` of a file whose length is 8 bytes short → `Err` mentioning "length", no panic; a file with the right length but `width` mismatching the expected geometry → `Err` mentioning "geometry"; (d) `create` over an existing `.rej` → `Err` ("exists"), the existing file untouched; (e) `record_band` with `bits.len() != rows·n·words` → `Err`.
  - `engine.rs`: (f) the Plan 4 fixture with one frame carrying a hot pixel at (7, 3) under sigma clipping: with a recording sink (a test `struct` collecting `(y0, rows, bits)`), exactly one bit is set over the whole run — frame `hot_idx`, row 3, bit 7 — and the OUTPUT data is byte-identical to the same run with `rejection_bits: None` (compare the two `Vec<f32>`s bit-for-bit).
  - `integrate.rs`: (g) `integrate_group` with `rej: Some(set)` on the fixture writes bitmaps whose rejected count summed over all frames/planes equals `Σ stats.rejected_fraction_per_frame[k] × samples` (use the engine's `rejected_per_frame` via a second run of `integrate_planes`, or assert equality with `rejected_low + rejected_high`); (h) `output_pairs.len() == channels` and each inner `len() == included.len()`, with the reference frame's pair being `(offset 0, scale 1)` under `additiveWithScaling` (the stats module's own contract — assert what `stats.rs` documents); (i) `rej.frames() != included` → `BadInput`.
  - `paths.rs`: (j) `work_usage` counts bytes under `rej/`; `cleanup_work(Intermediates)` removes `rej/` and `cleanup_work(Registered)` keeps it.
- [ ] **Step 2: implement**; keep the no-sink engine path free of new work (the pins prove it).
- [ ] **Step 3: gates** `cargo test -p athenaeum-core --lib integration::` and `stacking::` (counts), Plan 4 pins green, `cargo check -p athenaeum-core --no-default-features`, `rustfmt` `rej.rs` only.
- [ ] **Step 4: commit** `feat(stacking): per-frame rejection bitmaps — the .rej format and the engine sink (M3 Task 2)`.

---

### Task 3: The drizzle driver

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/drizzle/mod.rs`
- Test: in-file, synthetic frames written with `fits_writer::write_fits_f32` into a temp dir (the Plan 4 fixture builder in `stacking/test_fixtures.rs` already does this — reuse `pub(crate)` helpers, add one if needed).

**Interfaces:**
- Consumes: Task 1 (`geom`), Task 2 (`RejBitmap`), `crate::integration::plane_reader::PlaneReader` (`read_plane`), `crate::stacking::ln::grid::{LnFrameGrids, LnGrid, LnScratch}` (`evaluate_row_into`), `crate::integration::stats::NormalizationPair { scale: f32, offset: f32 }` (the engine applies `raw·scale + offset`), `crate::stacking::measure::{measure_plane, MeasureOptions}`, `crate::integration::band_budget::total_ram_bytes() -> Option<u64>` (already `pub`).
- Produces:
  ```rust
  pub struct DrizzleFrame<'a> {
      pub path: &'a Path,                 // calibrated frame (f32, 1 or 3 planes)
      pub map: &'a PixelMap,              // subject → reference
      pub weight: &'a [f64],              // per plane, normalized; ignored when `use_weights` is false
      pub output_pair: &'a [NormalizationPair], // per plane, the global output pair (R-M3-5 fallback)
      pub ln: Option<&'a LnFrameGrids>,   // per plane grids in reference geometry, when LN ran for it
      pub rej: Option<&'a Path>,          // its `.rej`, when `use_rejection`
  }
  pub struct DrizzleInput<'a> {
      pub frames: &'a [DrizzleFrame<'a>], // included frames only, engine order
      pub width: usize, pub height: usize, pub channels: usize,   // reference geometry
      pub scale: u32, pub drop_shrink: f64, pub kernel: DrizzleKernel,
      pub use_weights: bool, pub use_rejection: bool, pub use_local_normalization: bool,
      pub write_weight_map: bool,
      pub measure: &'a MeasureOptions,
  }
  #[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
  pub struct DrizzleStats {
      pub scale: u32, pub out_width: usize, pub out_height: usize, pub frames: usize,
      pub kernel: DrizzleKernel, pub drop_shrink: f64,
      pub used_weights: bool, pub used_rejection: bool,
      pub ln_frames: usize,               // frames whose LN grids were applied
      pub fwhm_px: Vec<f64>, pub eccentricity: Vec<f64>, pub noise: Vec<f64>,   // per plane, measured on the drizzled planes (output px)
      pub coverage: Vec<f64>,             // per plane, fraction of output pixels with W > 0
      pub read_ms: u64, pub deposit_ms: u64, pub bytes_read: u64,
  }
  pub struct DrizzleOutput { pub width: usize, pub height: usize, pub channels: usize, pub data: Vec<f32>, pub weight: Option<Vec<f32>>, pub stats: DrizzleStats }
  pub struct DrizzleProgress<'a> { pub on_frame: &'a (dyn Fn(usize, usize) + Sync) } // (done, total) over frames × planes
  pub const DRIZZLE_BAND_ROWS: usize = 512;
  pub fn estimate_memory_bytes(width: usize, height: usize, channels: usize, scale: u32, ln: bool) -> u64;  // R-M3-7 formula
  pub fn drizzle_group(input: &DrizzleInput<'_>, pool: &rayon::ThreadPool, cancel: &AtomicBool, progress: &DrizzleProgress<'_>) -> Result<DrizzleOutput, DrizzleError>;
  #[derive(Debug)] pub enum DrizzleError { Cancelled, Memory { need: u64, have: u64 }, Io(String), BadInput(String) }
  ```
- Algorithm (per plane `c`, sequential over planes; `out_w = width·s`, `out_h = height·s`):
  1. `I = vec![0f32; out_w·out_h]`, `W = same`.
  2. For each frame (sequential; cancel checked here): `PlaneReader::open(path)`, validate `channels`/geometry against the input (`BadInput` otherwise), `read_plane(c)` → `src`; if `use_rejection` and `rej.is_some()`: `RejBitmap::read`; if `use_local_normalization` and `ln.is_some()`: evaluate `ln.channels[c]` into `a_plane`/`b_plane` (two reused `Vec<f32>` of `width·height`, `evaluate_row_into` per row with one `LnScratch`); else `(os, oo)` from `output_pair[c]`; `w = if use_weights { weight[c] as f32 } else { 1.0 }`, skip the frame when `w <= 0`.
  3. `pool.install(|| I.par_chunks_mut(DRIZZLE_BAND_ROWS·out_w).zip(W.par_chunks_mut(…)).enumerate().for_each(|(band, (ib, wb))| …))`: band rect in output coords `[0, out_w) × [y0, y0 + rows)`; the source window = bbox of `map.inverse(to_reference(v), to_reference(u))` sampled at the four corners and every 32 px along the four edges (in reference coords, the corners of the band rect are at `to_reference(−0.5)`, `to_reference(out_w − 0.5)`, …), grown by `drop_shrink/2 + 1`, clamped to `[0, width) × [0, height)`; for every source pixel `(x, y)` in the window: `d = src[y·width + x]`; skip when `!d.is_finite() || d == 0.0`; centre `(u, v) = map.forward(x, y)`; if rejection: skip when `is_rejected(c, round(u), round(v))` (out-of-range → not rejected); `nd = if ln { a_plane[idx(round u, round v)]·d + b_plane[…] } else { os·d + oo }` (the same clamping to the reference extent for the LN index); then the kernel: `Square` → `map_drop` → for each output pixel `(px, py)` in the bbox ∩ band rect: `a = clip_area(&quad, px, py)`; if `a > 0`: `ib[(py − y0)·out_w + px] += (a·w) as f32·nd`, `wb[…] += (a·w) as f32`. `Circle`/`Gaussian` → for each sub-drop `(dx, dy, wt)`: `(u', v') = map.forward(x + dx, y + dy)`, `(px, py) = (round(to_output(u')), round(to_output(v')))`; inside the band rect → `ib += wt·w·nd`, `wb += wt·w`.
  4. After all frames: `data[c] = I / W` where `W > 0` else `0`; `weight[c] = W / max(W)` (max over the plane; all-zero → zeros); `coverage[c] = count(W > 0) / (out_w·out_h)`; measure `data[c]` with `measure_plane(…, input.measure, None)` inside `pool.install` → `fwhm_px`, `eccentricity`, `noise`.
  5. Progress: `on_frame(done, frames × channels)` after each frame.
- Memory: `estimate_memory_bytes` vs `total_ram_bytes()/2` (unknown → 4 GiB) checked BEFORE step 1 → `DrizzleError::Memory`.

- [ ] **Step 1: failing tests** (synthetic 64 × 48 frames, identity maps unless stated, `MeasureOptions::default()`-equivalent; the FWHM/eccentricity of a flat field is whatever `measure_plane` returns — do not assert them except in (e)):
  - (a) **level**: three uniform frames of value 0.25, s = 2, `drop_shrink = 0.9`, weights 1, no LN, pair (0, 1) → every interior output pixel of `data` is `0.25 ± 1e-6`, `weight` max is 1.0, `coverage == 1.0`; repeat for s = 1 and s = 3 and `drop_shrink = 0.5`.
  - (b) **spread**: one frame, a single pixel of value 1.0 at (10, 10) on zeros, s = 2, `drop_shrink = 1.0` → output (20,20), (21,20), (20,21), (21,21) are 1.0 and their neighbours 0 (zero samples are skipped, so `W` is 0 there and `data` is 0).
  - (c) **weights**: two frames, values 0.2 and 0.6, weights [1.0] and [0.0] with `use_weights = true` → output 0.2; with `use_weights = false` → 0.4.
  - (d) **rejection**: two uniform frames 0.2 and 0.6, a `.rej` for frame 1 with its plane fully set → output 0.2 everywhere; with `use_rejection = false` → 0.4.
  - (e) **sharpening**: 9 frames of one Gaussian star (σ = 0.7 px, peak 0.5 on a 0.01 background, the star rendered with sub-pixel centre `(20 + i/3, 20 + j/3)` for `i, j ∈ 0..3`, each frame's `map` the translation `(−i/3, −j/3)` so all register to (20, 20)); s = 2, `drop_shrink = 0.6` → `stats.fwhm_px[0] / 2.0 < 0.95 × fwhm_shift_and_add` where `fwhm_shift_and_add` is the FWHM of the same nine frames drizzled with `drop_shrink = 1.0` (the coarser drop) — the sub-pixel dither is recovered.
  - (f) **LN**: one uniform frame 0.2 with `ln = Some(LnFrameGrids { channels: [LnGrid::constant(64, 48, 1024, 2.0, 0.1)] })`, `use_local_normalization = true` → output `0.5 ± 1e-6`; the same with the toggle off and pair `(offset 0.1, scale 2.0)` → also 0.5 (`NormalizationPair { scale: 2.0, offset: 0.1 }` → `2.0·0.2 + 0.1 = 0.5`).
  - (g) **rotation conservation**: one uniform frame 0.3, `map` = 10° rotation about the frame centre, s = 2 → interior output pixels (those whose `W ≥ 0.99·max(W)`) are `0.3 ± 1e-4`.
  - (h) **cancel**: `cancel` set before the second frame → `Err(Cancelled)`.
  - (i) **memory**: `estimate_memory_bytes(6224, 4168, 3, 3, true)` equals the R-M3-7 formula; `drizzle_group` on an input whose estimate exceeds the injected total (make the RAM total injectable via a `#[cfg(test)]` override or a parameter `ram_total: Option<u64>` on `DrizzleInput`) → `Err(Memory { .. })`.
  - (j) **band seam**: one uniform frame 0.4, `height` such that `out_h = 3·512 + 100`, s = 2 → no output row differs from 0.4 by more than 1e-6 (rows 511/512, 1023/1024, 1535/1536 included).
- [ ] **Step 2: implement** `drizzle_group`; the hot loop must not allocate (per-band scratch only).
- [ ] **Step 3: tests pass**; `rustfmt` the file; headless check.
- [ ] **Step 4: commit** `feat(stacking): drizzle driver — banded forward deposition with weights, rejection and LN (M3 Task 3)`.

---

### Task 4: Scaled WCS, drizzle cards, the drizzled-master writer

**Files:**
- Modify: `crates/athenaeum-core/src/fits_writer/wcs.rs`, `crates/athenaeum-core/src/stacking/master_cards.rs`
- Test: in-file.

**Interfaces:**
- Produces (`wcs.rs`):
  ```rust
  /// The same solve on an image resampled by an integer factor `s` about the pixel grid (drizzle).
  /// `PlateSolveRecord.crpix1/2` are 0-BASED (the card writer adds 1), so they scale with Task 1's
  /// `to_output`: `crpix' = s·crpix + (s − 1)/2` (a reference pixel's centre lands between its s×s
  /// output pixels; on the 1-based card this reads `s·CRPIX − (s − 1)/2`), `CD' = CD / s` (every element), `pixel_scale' = pixel_scale / s`,
  /// SIP `A'_pq = A_pq · s^(1 − p − q)` (same for B; AP/BP if present), `width'/height' = s × …`
  /// when the record carries them. `s = 1` returns a clone.
  pub fn scale_plate_solve(solve: &PlateSolveRecord, s: u32) -> PlateSolveRecord;
  ```
  `PlateSolveRecord` (`plate_solve/storage.rs`) fields to scale: `crpix1`, `crpix2` (the CRPIX rule), `cd1_1`, `cd1_2`, `cd2_1`, `cd2_2` (÷ s), `pixel_scale_arcsec` (÷ s), `rms_residual_px` (× s — the same residual in output pixels), the SIP tables `sip_a_coeffs`/`sip_b_coeffs`/`sip_ap_coeffs`/`sip_bp_coeffs` (JSON strings — decode with the same helper `wcs_cards` uses, scale each `(p, q)` term by `s^(1 − p − q)`, re-encode; `None` stays `None`; a malformed table is an error, never a silent linear-only result — the `wcs_cards` contract). `crval*`, `field_rotation_deg`, `matched_stars` and the rest are untouched; `id`/`frame_id` are copied.
- Produces (`master_cards.rs`):
  ```rust
  pub const ATH_DRZ: &str = "ATH_DRZ";   // integer scale
  pub const ATH_DRZP: &str = "ATH_DRZP"; // drop shrink (real)
  pub const ATH_DRZK: &str = "ATH_DRZK"; // kernel serde name
  /// `<master stem>_drizzle<s>x.fits` and `<master stem>_drizzle<s>x_weight.fits` (spec §9.5).
  pub fn drizzle_file_names(master_stem: &str, scale: u32) -> (String, String);
  /// The master's cards with its WCS block (every card `wcs_cards` emits: WCSAXES, CTYPE*, CRVAL*, CRPIX*,
  /// CD*_*, CDELT*/CROTA* if any, A_*/B_*/AP_*/BP_*/A_ORDER/B_ORDER…) REPLACED by `wcs_cards(&scaled)`
  /// when `scaled` is Some (removed when None), plus ATH_DRZ/ATH_DRZP/ATH_DRZK. `NAXIS*` are the
  /// writer's business, never cards here.
  pub fn build_drizzle_cards(master_cards: &[Card], scaled: Option<&PlateSolveRecord>, scale: u32, drop_shrink: f64, kernel: DrizzleKernel) -> Result<Vec<Card>, FitsWriteError>;
  /// `IMAGETYP = 'Drizzle Weight'`, ATH_STK/ATH_STKV, BUNIT 'relative weight', ATH_STKI/ATH_STKG/ATH_DRZ copied through (the `rejection_map_cards` pattern).
  pub fn weight_map_cards(drizzle_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError>;
  pub struct WrittenDrizzle { pub drizzle: PathBuf, pub weight_map: Option<PathBuf> }
  /// Writes `output.data` (planar) as `<dir>/<drizzle name>` and, when `output.weight` is Some, the weight
  /// map; `resolve_collision` on both; the weight map's name derives from the RESOLVED drizzle stem.
  pub fn write_drizzled_master(dir: &Path, master_stem: &str, output: &DrizzleOutput, cards: &[Card]) -> anyhow::Result<WrittenDrizzle>;
  ```
- [ ] **Step 1: failing tests**: (a) `scale_plate_solve` with s = 2 on a record with 0-based `crpix` (100.0, 50.0), CD diag (1e-4, −1e-4), SIP order 2 with `A_2_0 = 1e-6`, `A_1_1 = 2e-6`, `A_0_2 = 3e-6` → `crpix` (200.5, 100.5) (so the written `CRPIX1` card is 201.5 = 2·101 − 0.5), CD diag (5e-5, −5e-5), `A'_2_0 = 5e-7`, `A'_1_1 = 1e-6`, `A'_0_2 = 1.5e-6`; s = 1 → identical; (b) a synthetic star at reference pixel (x, y) projects to the same RA/Dec through the original solve as output pixel `(to_output(x), to_output(y))` through the scaled solve (use the crate's existing pixel→sky evaluator if one exists; otherwise assert the linear part: `CD'·(p' − CRPIX') == CD·(p − CRPIX)` for three pixels); (c) `build_drizzle_cards` on a master card list with a WCS: exactly one `CRPIX1` card, equal to the scaled value; no leftover original SIP cards; the three ATH_DRZ* cards present with the right values (`ATH_DRZK = 'square'`); with `scaled = None` no WCS cards remain; (d) `drizzle_file_names("LDN_1272_NoFilter_mono_180s_208x", 2)` → `("LDN_1272_NoFilter_mono_180s_208x_drizzle2x.fits", "…_drizzle2x_weight.fits")`; (e) `write_drizzled_master` writes a 2-plane 8 × 6 output and its weight map into a temp dir, both readable by `PlaneReader` with the right geometry; a second write resolves to `_2` names and the weight map follows the resolved stem.
- [ ] **Step 2: implement**; **Step 3** tests + rustfmt (`wcs.rs` only if clean; `master_cards.rs` check first); **Step 4: commit** `feat(stacking): scaled WCS, drizzle cards and the drizzled-master writer (M3 Task 4)`.

---

### Task 5: Run wiring — bitmaps during integration, the Drizzle stage, summary, plan, config, cleanup

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs`, `crates/athenaeum-core/src/stacking/plan.rs`, `crates/athenaeum-core/src/stacking/paths.rs` (`estimate_bytes`), `crates/athenaeum-core/src/stacking/config.rs` (preset), `crates/athenaeum-core/src/stacking/provenance.rs`, `crates/athenaeum-core/src/db/stacking.rs` (`GroupUpdate.drizzle_path`), `crates/athenaeum-core/src/api/stacking.rs` (the `rerunFrom` clamp), `crates/athenaeum-core/src/ts_export.rs` (`DrizzleStats`), `src/types/stacking.ts` (regenerated), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (`drizzle_scale`, `rej_bytes`, `out_width`, `out_height`), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §7 (rulings R-M3-1…11 as a short "Implementation notes (M3)" list), `CLAUDE.md` → Stacking (M3 paragraph: the stage, the `.rej` temporaries, the output names, the rulings' one-liners).
- Test: `run.rs` composite tests on the existing synthetic-set harness; `plan.rs` gate tests; `config.rs` preset test; `cargo test -p athenaeum-core --test ts_contract`.

**Interfaces / behaviour:**
- `process_group_output`: (1) when `cfg.drizzle.enabled && cfg.drizzle.use_rejection`, before `integrate_group`: `RejBitmapSet::create(layout.rej_dir(run_id, &group.key), &stems_of_included_in_engine_order, width, height, channels)` — the included list in engine order is `members` after the min-weight drop exactly as `integrate_group` will see it (it reports `output.included` AFTER the fact; to build the set BEFORE, compute the same drop here: `integrate_group`'s min-weight rule must be extracted into a `pub(crate) fn included_after_min_weight(frames, cfg) -> Vec<usize>` in `integrate.rs` that both use — do not duplicate the rule). Pass `rej: Some(&set)` in `GroupInput`; on `create` failure → the group's drizzle is skipped with a warning (the master still integrates, `rej: None`). (2) After `write_master_light` and the DB update: if `cfg.drizzle.enabled`: emit `Stage::Drizzle` progress `0/total`, build `DrizzleInput` from `stack_frames`/`output.included`/`output.output_pairs`/`ln_grids`/the set's paths, call `drizzle_group`; on `Ok` → `build_drizzle_cards(&cards, wcs.map(|w| scale_plate_solve(w, s)).as_ref(), …)`, `write_drizzled_master(&rc.output_dir, &master_stem, &drz, &drz_cards)`, `update_group(GroupUpdate { drizzle_path: Some(..) })`, summary fields, `info!(run_id, group_key, path, drizzle_scale = s, out_width, out_height, duration_ms, "drizzled master written")`; on `Err(Cancelled)` → propagate (the run's cancel path); on `Err(Memory{..})`/`Io`/`BadInput` → `warn!` + `rc.warnings.push(..)` + the group stays `done` with `drizzle_path` NULL (ruling R-M3-7) — never `fail_group`, the master is already good. Return `(outcome, ln_dur, integrate_dur, drizzle_dur, output_dur)`; `stage_output` sums and pushes `StageTiming { Drizzle }` between `Integrate` and `Output` only when `cfg.drizzle.enabled` and at least one group attempted it.
- `run_thread`'s single exit path: after the summary is finalized, `if cfg.output.cleanup != CleanupPolicy::KeepAll { remove_dir_all(layout.rej_run_dir(run_id)) }` (a missing dir is fine; a failure is a `warn!`, never an error), for every outcome including cancel/failure/panic-recovery.
- `StackingCompleteEvent.masters[].drizzle_path` filled from the group rows.
- `api::stacking::start_stacking`: `rerun_from == Some(Stage::Drizzle)` → `Some(Stage::Integrate)` + `debug!("rerun from drizzle clamped to integrate")` (R-M3-9).
- `plan.rs`: Gate 6 — remove the "Drizzle arrives in M3" blocker; add `unsupported` "drizzle scale must be 1, 2 or 3" when `cfg.drizzle.enabled && !(1..=3).contains(&cfg.drizzle.scale)`; `estimate_bytes` gains `EstimateInputs { drizzle: Option<(u32 /*scale*/, bool /*weight map*/, bool /*rejection*/)> }` → per group `+ included·planes·ceil(W/64)·8·H` (rejection bitmaps, largest-member geometry like the master term) `+ planes·(W·s)·(H·s)·4` (+ the same again when the weight map is written).
- `config.rs`: `MaximumQuality` → `c.drizzle.enabled = true; c.drizzle.scale = 2;` (doc comment: LN stays off in the preset until… — NO: the M2 acceptance passed, so the preset ALSO turns `normalization.local.enabled = true` now, as the spec §9.2 text says; both in this task, the preset test updated for both).
- `plan.rs`: `PlanGroup.anchor_width`/`anchor_height: Option<i64>` (the anchor member's native geometry; `None` when the frame rows carry none) — Task 6's estimate line reads them.
- `provenance.rs`: `SummaryGroup.drizzle_path: Option<String>`, `weight_map_path: Option<String>`, `drizzle: Option<DrizzleStats>` — all `#[serde(default)]`; `push_summary_group` gains the three (pass `None` from every other call site).
- `db/stacking.rs`: `GroupUpdate.drizzle_path: Option<&str>` (the column exists since Plan 5a).
- `ts_export.rs`: register `crate::stacking::drizzle::DrizzleStats`; regenerate (`cargo test -p athenaeum-core --test ts_contract` — follow the test's own instructions for regenerating `src/types/stacking.ts`).

- [ ] **Step 1: failing tests** (`run.rs` harness — the existing synthetic set with 4–6 frames): (a) drizzle 2× on, `useRejection` on, `cleanup = deleteIntermediates` → the run succeeds, the group row's `drizzle_path` names an existing file of `out_w = 2·W`, `out_h = 2·H` (open with `PlaneReader`), `summary.groups[0].drizzle.scale == 2`, `stages` contains `drizzle` between `integrate` and `output`, `rej/run-<id>` does NOT exist after the run, `stacking-complete`'s `masters[0].drizzle_path` is `Some`; (b) same with `cleanup = keepAll` → `rej/run-<id>/<group>/` holds `included` `.rej` files of the right size; (c) drizzle on with `writeWeightMap` → `weight_map_path` exists; (d) drizzle off → no `drizzle` timing, no `rej/`, the master bytes identical to the same run before this task (the M2 pins — run the fixture twice, once via `git stash`-free means: compare against a hash pinned in the test); (e) a cancel raised during drizzle (the `on_frame` hook of the test harness flips the flag) → run `cancelled`, `rej/` removed, master present, `drizzle_path` NULL; (f) `rerun_from = Drizzle` → the plan/stage logic treats it as Integrate (assert the integrate stage ran: `stages` has `integrate` after a re-run); (g) `plan.rs`: `drizzle.enabled` no longer yields an `unsupported` blocker; `scale = 4` yields one; the estimate grows by the bitmap + output terms; (h) `config.rs`: `preset(MaximumQuality)` has `drizzle.enabled`, `scale 2`, `normalization.local.enabled`.
- [ ] **Step 2: implement**; keep `run.rs` edits in complete passes (the file is large — read the surrounding code before editing, no partial edits).
- [ ] **Step 3: gates** `cargo test -p athenaeum-core --lib stacking` (counts), `--test ts_contract`, `cargo test -p athenaeum-web`, `cargo check --workspace --all-targets`, `cargo check -p athenaeum-core --no-default-features`, `npx tsc --noEmit`; no rustfmt on `run.rs`/`plan.rs`/`integrate.rs`.
- [ ] **Step 4: commit** `feat(stacking): drizzle stage wired — bitmaps during integration, drizzled masters, summary, plan, preset (M3 Task 5)`.

---

### Task 6: The tab — `DrizzlePanel` live, the row toggle, results, timings

**Files:**
- Modify: `src/components/stacking/panels/DrizzlePanel.tsx`, `src/components/stacking/PipelineBoard.tsx`, `src/components/stacking/StackingTab.tsx`, `src/components/stacking/stageSummary.ts`, `src/components/stacking/ResultsPanel.tsx`, `src/components/stacking/StageInspector.tsx`, `src/components/settings/StackingSection.tsx`
- Gate: `npx tsc --noEmit`, `VITE_TARGET=web npx vite build`.

**Behaviour:**
- `DrizzlePanel({ config, onChange, disabled, defaults, plan })` mirrors `NormalizePanel`'s editing pattern (`patch`, `NumericField` for `dropShrink` with min 0.5 / max 1.0 / step 0.05 and the two-state discipline, a `<select>` for `scale` with options 1×/2×/3×, a `<select>` for the kernel with `kernelLabel`, four checkboxes: use rejection / use weights / use local normalization (disabled with the hint "local normalization is off for this set" when `config.normalization.local.enabled` is false — the toggle is kept as stored, only its effect is moot) / write weight map). Help lines state the defaults (`2×`, `0.90`, `Square`, all three "use" on, weight map off) and "square = exact clipping; circle/gaussian = 16×16 tabulated". An estimate line from `plan`: `Bitmaps ≈ X GB (temporary) · output ≈ Y GB · ≈ N min` with `X = Σ groups includedCount·planes·ceil(W/64)·8·H` (0 when `useRejection` off), `Y = Σ planes·(W·s)(H·s)·4·(writeWeightMap ? 2 : 1)`, `N = Σ includedCount·planes·DRIZZLE_SECONDS_PER_PLANE_AT_2X·(s/2)²/60` with `DRIZZLE_SECONDS_PER_PLANE_AT_2X = 1.2` (a constant in `stageSummary.ts`, re-fitted from Task 7 — say so in its comment). Group W/H come from `PlanGroup.anchorWidth`/`anchorHeight` — `PlanGroup` carries no geometry today (`StackingRunGroupRow` does), so Task 5 adds `anchor_width: Option<i64>` / `anchor_height: Option<i64>` (the anchor member's native geometry, the same member `groups.rs` picks for the run-group geometry) to `PlanGroup` and regenerates the TS type.
- `PipelineBoard`: the drizzle row toggle is live (`onToggleDrizzle(checked)` → `config.drizzle.enabled`), `note` removed; `StackingTab` passes the handler (same shape as `onToggleWriteRegisteredFrames`).
- `stageSummary.ts`: `TIMED_STAGES` gains `'drizzle'` after `'integrate'` (and `'normalize'` from the M2 fix wave is already there — verify); `isBlockedBy`'s `unsupported` disambiguation keeps working (the only `unsupported` left is the scale-range one, still on the drizzle row); `stageSummary('drizzle')` unchanged.
- `ResultsPanel` master card: when `group.drizzlePath` → a `Drizzle 2×` line with `RevealOrPath`, the drizzled FWHM per plane from `summary.groups[].drizzle.fwhmPx` formatted like the master's, `coverage` as a percent, and the weight map path when present; when the run had drizzle on but `drizzlePath` is null → `Drizzle: skipped — see warnings`.
- `StackingSection` (Settings) passes `onChange` to the panel like it does for the other stages (it renders the same inspector).
- Notification: unchanged (`stacking-complete` already carries `drizzlePath`; the toast text may append ` · drizzled` when any master has one).

- [ ] Implement; `tsc`; `vite build`; commit `feat(stacking-ui): drizzle panel and row toggle live; drizzled outputs in results (M3 Task 6)`.

---

### Task 7 (controller-run): M3 acceptance run on LDN 1272

**Setup:** release `athenaeum-web` from the branch head (the M2 acceptance's `.superpowers/target-acc` + `dist-acc` recipe), dev catalog, set 109 (LDN 1272), reference `_0073` manual, LN on both sides (the M2 run-6 config) + `drizzle { enabled, scale 2, dropShrink 0.9, square, useRejection, useWeights, useLocalNormalization, writeWeightMap true }`, `cleanup keepAll` for the first run (to size `rej/`). Everything before Integrate is cached from M2 (calibrate/measure/register/ln) — re-run from Integrate.

**Measurements (note `docs/superpowers/research/<date>-m3-acceptance-run.md`):**

| Metric | Target | How |
| ---- | ---- | ---- |
| Drizzled/undrizzled FWHM ratio, mono and OSC per plane | within ±5 % of the external's ratio | ours: `drizzle.fwhmPx / 2 ÷ stats.masterFwhmPx`; external: `measure_probe` on `…_mono_drizzle_2x.xisf` and `…_RGB_drizzle_2x.xisf` (FWHM in drizzled px / 2) ÷ the external master's FWHM measured the same way (Checkpoint B measured 2.67 mono with our estimator) |
| Drizzle wall time per group | ≤ 15 min | `stages[drizzle]` (sum) and the per-group `deposit_ms`/`read_ms` |
| `rej/` size | ≈ 208 × 3.27 MB + 160 × 3 × 3.26 MB ≈ 2.2 GB, removed on a `deleteIntermediates` re-run | `du`, `WorkUsage.rejBytes` |
| Output sizes | 2× mono 415 MB, OSC 1.25 GB (+ weight maps) | `ls` |
| Level | drizzled median within 1 % of the master's median (R-M3-2) | `fitsdiff.py`-style medians |
| Band seam | 512-row fold of the (drizzled − 2× nearest-upsampled master) row profile: no peak above the random fold | `meshcheck.py` with stride 512 |
| Coverage | ≥ 0.999 on both groups | `drizzle.coverage` |
| Weight map | max 1.0, no zero interior | header + stats |
| Click-through | DrizzlePanel live, toggle, results lines, estimate line | the LAN browser recipe |

Then a `deleteIntermediates` re-run from Integrate confirms `rej/` is gone and the drizzled outputs land beside the masters with `_2` suffixes (never overwritten). Record the fitted `DRIZZLE_SECONDS_PER_PLANE_AT_2X` and update the constant in the final fix wave if it is off by more than 2×.

**Docs:** the acceptance note, `docs/superpowers/open-items.md` (Stacking M3 subsection: owed owner click-through, Windows/Linux runs, release-note lines), `CLAUDE.md` Stacking (acceptance sentence).

---

## Self-review (done while writing)

- Spec coverage: §6.2 bitmaps (Task 2), §7 every sentence (Tasks 1/3/4/5 — kernels R-M3-3, tile scheduler R-M3-6, rejection R-M3-4, LN R-M3-5, weight map, scaled WCS, the three cards, Bayer drizzle deferred to M4 as the spec says), §9.5 names (Task 4), §10.2 `drizzlePath` (Task 5), §11.2 `DrizzlePanel` (Task 6), §13 target (Task 7), §14 M3 list complete.
- Placeholders: none — every value is literal (512, 16, 0.025, ranges, card names, formulas).
- Type consistency: `DrizzleStats`/`DrizzleOutput`/`DrizzleInput` (Task 3) are what Task 4's writer and Task 5's wiring consume; `RejBitmapSet`/`RejBitmap`/`RejectionBitSink` (Task 2) are what Task 3 reads and the engine writes; `GroupOutput.output_pairs` (Task 2) feeds `DrizzleFrame.output_pair` (Task 3); `to_output`/`to_reference` (Task 1) are the one coordinate rule Tasks 3 and 4 share (the 0-based record's `crpix' = s·crpix + (s−1)/2` IS `to_output`; the 1-based card reads `s·CRPIX − (s−1)/2`).
- Known seams for the reviewers: the min-weight rule extraction (Task 5) must not change `integrate_group`'s behaviour (the M1/M2 pins); the engine's no-sink path must stay byte-identical (Task 2's test f).
