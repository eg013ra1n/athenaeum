# Calibrated-Lights Export v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Calibration (with hot-pixel correction and VNG debayer for OSC) becomes a stage of the WBPP export and the frame-set send; the standalone Calibrate Lights flow and its `light_calibrations` table are removed.

**Architecture:** A new full-res VNG kernel in the rustafits submodule; a hot-pixel module and a compute/write split in the B5 light-cal engine; a shared generator in `export/` that resolves per-frame masters up front (short DB borrow) and then calibrates straight into the export destination or the send package dir; collab publishing is honestly blocked (deferred rework, spec §8a).

**Tech Stack:** Rust (athenaeum-core, rustafits submodule, Tauri + Axum backends), React/TS frontend, SQLite.

**Spec:** `docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md` — read it first; every task below argues from it.

## Global Constraints

- Never name the reference stacker/solver software in code or comments (CLAUDE.md rule). Say "reference implementation" / "external reference".
- Two backends in sync: every Tauri command change gets the same Axum route change in the same task.
- `tracing` only; zero `println!`/`eprintln!` in production code. Message = short stable phrase, data in snake_case fields.
- `anyhow::Result` inside core; `.map_err(|e| e.to_string())` at command boundaries.
- Serde boundary: `#[serde(rename_all = "camelCase")]`; mirror TS types in `src/types/models.ts`.
- Frontend uses design tokens (`bg-surface`, `text-content-muted`, …), never raw colors; backend access only via the `api` object.
- Commit as the user: `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit …`. Never Claude as author/co-author.
- Gates per task: `cargo build --workspace`, `cargo test -p athenaeum-core <relevant filter>`, and `npx tsc --noEmit` whenever TS changed. rustafits tasks: `cargo test` inside `rustafits/`.
- rustafits work happens INSIDE the submodule on a new branch `feature/vng-debayer` (it is currently on a detached HEAD at `72aca7c`); the parent repo pins the submodule SHA in Task 12.

---

### Task 1: VNG debayer kernel in rustafits

**Files:**
- Create: `rustafits/src/processing/vng.rs`
- Modify: `rustafits/src/processing/mod.rs` (add `pub mod vng;`)

**Interfaces:**
- Consumes: `crate::types::BayerPattern` (`None | Rggb | Bggr | Gbrg | Grbg`, `rustafits/src/types.rs:3`).
- Produces (BINDING for Tasks 2 and 6):
  ```rust
  /// Full-resolution gradient-based demosaic. Input: one CFA plane, row-major,
  /// any float scale (negatives allowed). Output: PLANAR RGB, len 3*w*h,
  /// ordered [R plane | G plane | B plane]. `pattern` must not be `None`
  /// (debug_assert + fall back to replicating the input into all 3 planes).
  pub fn vng_debayer_f32(data: &[f32], width: usize, height: usize, pattern: BayerPattern) -> Vec<f32>
  ```

- [ ] **Step 1: Create the branch in the submodule**

```bash
git -C rustafits switch -c feature/vng-debayer
```

- [ ] **Step 2: Write the failing unit tests** (in `vng.rs` `#[cfg(test)]`)

```rust
fn cfa_fill(w: usize, h: usize, pattern: BayerPattern, r: f32, g: f32, b: f32) -> Vec<f32> {
    // Fill a mosaic where every R site = r, G site = g, B site = b for `pattern`.
    let (ri, g0, g1, bi) = match pattern {
        BayerPattern::Rggb => (0, 1, 2, 3),
        BayerPattern::Bggr => (3, 1, 2, 0),
        BayerPattern::Grbg => (1, 0, 3, 2),
        BayerPattern::Gbrg => (2, 3, 0, 1),
        BayerPattern::None => unreachable!(),
    };
    let vals = |slot: usize| [r, g, g, b][[ri, g0, g1, bi].iter().position(|&s| s == slot).unwrap()];
    (0..w * h).map(|i| { let (x, y) = (i % w, i / w); vals((y % 2) * 2 + (x % 2)) }).collect()
}

#[test]
fn constant_channels_reconstruct_exactly() {
    for pattern in [BayerPattern::Rggb, BayerPattern::Bggr, BayerPattern::Grbg, BayerPattern::Gbrg] {
        let (w, h) = (16, 12);
        let cfa = cfa_fill(w, h, pattern, 0.8, 0.5, 0.2);
        let rgb = vng_debayer_f32(&cfa, w, h, pattern);
        assert_eq!(rgb.len(), 3 * w * h);
        for (plane, want) in [(0, 0.8f32), (1, 0.5), (2, 0.2)] {
            for v in &rgb[plane * w * h..(plane + 1) * w * h] {
                assert!((v - want).abs() < 1e-6, "{pattern:?} plane {plane}: {v} != {want}");
            }
        }
    }
}

#[test]
fn horizontal_ramp_stays_monotonic_and_bounded() {
    // A luminance ramp (all channels equal, linear in x) must reconstruct to
    // values within the local input range — no over/undershoot beyond neighbors.
    let (w, h) = (32, 16);
    let cfa: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 / w as f32).collect();
    let rgb = vng_debayer_f32(&cfa, w, h, BayerPattern::Rggb);
    for plane in 0..3 {
        for y in 2..h - 2 {
            for x in 3..w - 3 {
                let v = rgb[plane * w * h + y * w + x];
                let lo = (x as f32 - 2.0) / w as f32;
                let hi = (x as f32 + 2.0) / w as f32;
                assert!(v >= lo - 1e-5 && v <= hi + 1e-5, "plane {plane} ({x},{y}): {v}");
            }
        }
    }
}

#[test]
fn negatives_pass_through() {
    let (w, h) = (8, 8);
    let cfa = cfa_fill(w, h, BayerPattern::Rggb, -0.1, -0.1, -0.1);
    let rgb = vng_debayer_f32(&cfa, w, h, BayerPattern::Rggb);
    assert!(rgb.iter().all(|v| (*v - -0.1).abs() < 1e-6));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd rustafits && cargo test vng`
Expected: FAIL — `vng_debayer_f32` not found.

- [ ] **Step 4: Implement the kernel**

Structure (classic 8-direction gradient-threshold demosaic over a 5×5 window):

```rust
pub fn vng_debayer_f32(data: &[f32], width: usize, height: usize, pattern: BayerPattern) -> Vec<f32> {
    debug_assert!(pattern != BayerPattern::None);
    let mut out = vec![0f32; 3 * width * height];
    if pattern == BayerPattern::None {
        for p in 0..3 { out[p * width * height..][..width * height].copy_from_slice(data); }
        return out;
    }
    // color_at(x, y) -> 0=R 1=G 2=B for `pattern` (fold x%2/y%2 per pattern).
    // Interior pass (2..height-2, 2..width-2):
    //   1. Eight gradients N,E,S,W,NE,SE,NW,SW. Axial (example N; E/S/W by
    //      rotation), p(dx,dy) = data[(y+dy)*width + (x+dx)]:
    //        G_N = |p(0,-1)-p(0,1)| + |p(0,-2)-p(0,0)|
    //            + (|p(-1,-1)-p(-1,1)| + |p(1,-1)-p(1,1)|
    //            +  |p(-1,-2)-p(-1,0)| + |p(1,-2)-p(1,0)|) / 2
    //      Diagonal (example NE; others by rotation):
    //        G_NE = |p(1,-1)-p(-1,1)| + |p(2,-2)-p(0,0)|
    //             + (|p(1,-2)-p(0,-1)| + |p(2,-1)-p(1,0)|
    //             +  |p(0,-1)-p(-1,0)| + |p(1,0)-p(0,1)|) / 2
    //   2. Threshold T = 1.5 * g_min + 0.5 * (g_max - g_min). Select every
    //      direction with g <= T (g_min == g_max selects all 8).
    //   3. For each selected direction, accumulate a per-color average of the
    //      CFA samples of that color inside the direction's 3-pixel arm
    //      (the arm of N = offsets (0,-1),(0,-2),(-1,-1),(1,-1),(-1,-2),(1,-2);
    //      each sample bucketed by color_at of its absolute position). Track
    //      per-color counts; skip colors absent from an arm.
    //   4. Estimate: let c = color_at(x,y), base = data[y*width+x].
    //      For each missing color m: out_m = base + (avg_m_sum - avg_c_sum) / n_dirs
    //      where avg_*_sum sums the per-direction averages over selected dirs
    //      and n_dirs = number of selected dirs. out_c = base.
    // Border pass (outer 2 rows/cols): bilinear — G at non-G sites = mean of
    //   in-bounds cardinal G neighbors; R/B at other sites = mean of in-bounds
    //   same-color neighbors at distance 1 (diagonal) or 2 (axial), G site's
    //   R/B = mean of the in-bounds adjacent sites of that color.
    out
}
```

Implement `color_at` via the pattern's 2×2 slot table (same mapping as the test helper). Keep it scalar and row-major; no SIMD in this task.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd rustafits && cargo test vng`
Expected: PASS (all three tests).

- [ ] **Step 6: Full submodule gate + commit (in the submodule)**

```bash
cd rustafits && cargo test && cargo build
git add src/processing/vng.rs src/processing/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(processing): full-resolution 8-gradient VNG demosaic"
```

---

### Task 2: VNG validation against the external reference

**Files:**
- Modify: `rustafits/src/processing/vng.rs` (append an `#[ignore]`d test)

**Interfaces:**
- Consumes: `vng_debayer_f32` (Task 1); `crate::formats::xisf::read_xisf_image(path: &Path) -> Result<(ImageMetadata, PixelData)>` (`PixelData::Float32(Vec<f32>)`, `ImageMetadata { width, height, channels, .. }`).
- Produces: the acceptance verdict for the kernel. Nothing downstream imports this.

- [ ] **Step 1: Write the ignored reference test**

```rust
/// Compares our VNG against the external reference's debayered output for the
/// same calibrated CFA input. Set:
///   VNG_REF_CAL = path to a calibrated CFA .xisf (single channel, RGGB)
///   VNG_REF_DEB = path to the matching debayered .xisf (3 channels)
/// e.g. the LDN 1272 reference pair
///   .../calibrated/Light_..._CFA_.../2025-09-14_00-55-28..._0000_c.xisf
///   .../debayered/Light_..._CFA_.../2025-09-14_00-55-28..._0000_c_d.xisf
#[test]
#[ignore = "needs real reference files via VNG_REF_CAL / VNG_REF_DEB"]
fn vng_matches_reference_debayer() {
    use crate::formats::xisf::read_xisf_image;
    let cal = std::env::var("VNG_REF_CAL").expect("VNG_REF_CAL");
    let deb = std::env::var("VNG_REF_DEB").expect("VNG_REF_DEB");
    let (mc, pc) = read_xisf_image(std::path::Path::new(&cal)).unwrap();
    let (md, pd) = read_xisf_image(std::path::Path::new(&deb)).unwrap();
    let cfa = match pc { crate::types::PixelData::Float32(v) => v, _ => panic!("expect f32") };
    let refi = match pd { crate::types::PixelData::Float32(v) => v, _ => panic!("expect f32") };
    assert_eq!(md.channels, 3);
    let (w, h) = (mc.width, mc.height);
    let ours = vng_debayer_f32(&cfa, w, h, crate::types::BayerPattern::Rggb);
    for plane in 0..3 {
        // Interior only — border policy legitimately differs.
        let mut diffs: Vec<f64> = Vec::with_capacity((w - 8) * (h - 8));
        let (mut so, mut sr, mut soo, mut srr, mut sor) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for y in 4..h - 4 {
            for x in 4..w - 4 {
                let o = ours[plane * w * h + y * w + x] as f64;
                let r = refi[plane * w * h + y * w + x] as f64;
                diffs.push((o - r).abs());
                so += o; sr += r; soo += o * o; srr += r * r; sor += o * r;
            }
        }
        let n = diffs.len() as f64;
        let corr = (sor - so * sr / n) / (((soo - so * so / n) * (srr - sr * sr / n)).sqrt());
        diffs.sort_by(f64::total_cmp);
        let med = diffs[diffs.len() / 2];
        let p999 = diffs[(diffs.len() as f64 * 0.999) as usize];
        println!("plane {plane}: corr={corr:.6} median|d|={med:.3e} p99.9|d|={p999:.3e}");
        assert!(corr > 0.999, "plane {plane} corr {corr}");
        assert!(med < 2.0e-5, "plane {plane} median {med}"); // signal scale ~7e-3
        // Channel-assignment errors show as gross p99.9 / corr failures.
    }
}
```

- [ ] **Step 2: Run it against the real reference pair**

Run (adjust to a real matching pair from `~/Pictures/LDN1272`):
```bash
cd rustafits
VNG_REF_CAL="/Users/vsharifov/Pictures/LDN1272/calibrated/Light_BIN-1_6248x4176_EXPOSURE-180.00s_FILTER-NoFilter_CFA_CAMERA-zwoasi2600mcduo_FLAT-911_BIAS-987_DARKS-1265/2025-09-14_00-55-28__-10.00_180.00s_0000_c.xisf" \
VNG_REF_DEB="/Users/vsharifov/Pictures/LDN1272/debayered/Light_BIN-1_6248x4176_EXPOSURE-180.00s_FILTER-NoFilter_CFA_CAMERA-zwoasi2600mcduo_FLAT-911_BIAS-987_DARKS-1265/2025-09-14_00-55-28__-10.00_180.00s_0000_c_d.xisf" \
cargo test --release vng_matches_reference_debayer -- --ignored --nocapture
```
Expected: PASS. If corr passes but median misses, iterate on the kernel's gradient/arm tables (the reference is the oracle); report the final numbers in the commit message. Bitwise equality is NOT expected.

- [ ] **Step 3: Commit (in the submodule)**

```bash
cd rustafits && git add src/processing/vng.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "test(processing): VNG pinned against external reference output"
```

---

### Task 3: Hot-pixel cosmetic module

**Files:**
- Create: `crates/athenaeum-core/src/calibration_library/cosmetic.rs`
- Modify: `crates/athenaeum-core/src/calibration_library/mod.rs` (add `pub mod cosmetic;` beside `light_cal`)

**Interfaces:**
- Consumes: `crate::integration::banded::BandSource` (full-plane read, same as `light_cal::read_full_flat_plane`), `crate::integration::cfa::{CfaGeometry, cfa_channel_at}`, `crate::integration::IntegrationError`.
- Produces (BINDING for Task 6):
  ```rust
  pub const HOT_SIGMA: f64 = 10.0; // reference parity: sigma-high 10
  pub struct HotPixelMap { /* width, height, sorted Vec<u32> indices */ }
  impl HotPixelMap { pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; }
  /// Reads the master dark's full plane and flags value > median + HOT_SIGMA*1.4826*MAD.
  pub fn hot_pixel_map_from_dark(dark_path: &Path, scratch_dir: &Path) -> Result<HotPixelMap, IntegrationError>;
  /// Replaces each mapped pixel with the median of its neighbors — 3×3 (8
  /// neighbors) for mono, the 8 same-channel stride-2 neighbors for CFA.
  /// Border pixels use the in-bounds subset. Returns the replaced count.
  pub fn apply_hot_pixel_correction(data: &mut [f32], width: usize, height: usize, map: &HotPixelMap, cfa: Option<CfaGeometry>) -> u64;
  ```

- [ ] **Step 1: Write the failing tests** (in-module `#[cfg(test)]`; write tiny FITS darks with `crate::fits_writer::write_fits_f32` into a tempdir)

```rust
#[test]
fn map_flags_exactly_the_spikes() {
    // 16x8 dark: flat 300.0 with spikes at idx 5 and 100 (values 5000.0).
    // Write with write_fits_f32, load map, assert len == 2 and both indices flagged.
}

#[test]
fn mono_replacement_uses_3x3_median() {
    // data flat 1.0, hot pixel at (4,4) = 9.0 in the map → after apply, (4,4) == 1.0, count == 1.
}

#[test]
fn cfa_replacement_stays_in_channel() {
    // Checkerboard CFA values: R sites 0.8, G 0.5, B 0.2 (RGGB, offsets 0).
    // Hot R site → replaced with 0.8 (never 0.5/0.2); hot G site → 0.5.
}

#[test]
fn uniform_dark_flags_nothing() {
    // All 300.0 → MAD 0 → threshold degenerate; guard: MAD == 0 uses a small
    // absolute floor (e.g. 1e-6) so a synthetic uniform dark yields an empty map.
}
```

- [ ] **Step 2: Run tests to verify they fail** — `cargo test -p athenaeum-core cosmetic` → FAIL (module missing).

- [ ] **Step 3: Implement** — median/MAD over the full dark plane (sort a copy; MAD = median of |v−med|); `apply` gathers neighbor values into a stack buffer `[f32; 8]`, sorts, takes the median of the in-bounds count. CFA neighbors: offsets `(-2..=2 step 2)` in x/y excluding (0,0). `cfa` chooses the neighbor stride only — the channel at the center is preserved by stride-2 geometry, so `cfa_channel_at` is not needed inside `apply` (keep the param for the doc contract and debug_assert the stride preserves phase).

- [ ] **Step 4: Run tests to verify they pass** — `cargo test -p athenaeum-core cosmetic` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/calibration_library/cosmetic.rs crates/athenaeum-core/src/calibration_library/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(calibration): hot-pixel map from master dark + CFA-aware correction"
```

---

### Task 4: Engine compute/write split + engine-version move

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs`

**Interfaces:**
- Consumes: existing `LightCalInputs`, `LightCalOutcome`, `calibrate_light_inner`.
- Produces (BINDING for Task 6):
  ```rust
  pub struct CalibratedFrame { pub width: usize, pub height: usize, pub data: Vec<f32> }
  /// The formula pass only — everything calibrate_light did EXCEPT the write
  /// and the hash. `outcome.output_hash` is empty here.
  pub fn calibrate_light_compute(inputs: &LightCalInputs, cancel: &AtomicBool) -> Result<(CalibratedFrame, LightCalOutcome), IntegrationError>;
  /// tmp + atomic write via write_fits_f32, then xxh3 of the file.
  pub fn write_calibrated_output(path: &Path, width: usize, height: usize, channels: usize, data: &[f32], cards: &[Card]) -> Result<String, IntegrationError>;
  /// Moved here from db::light_calibrations (that module dies in Task 11); bump by 1.
  pub const LIGHT_CAL_ENGINE_VERSION: i64 = /* previous value + 1 */;
  ```
  `calibrate_light(inputs, cancel)` remains, now composed as `calibrate_light_compute` + `write_calibrated_output(inputs.output_path, …, &inputs.cards)` — its signature and behavior unchanged, so every existing test keeps passing.

- [ ] **Step 1: Write one new test pinning the split**

```rust
#[test]
fn compute_then_write_equals_calibrate_light() {
    // Reuse the existing `inputs(..)` test helper on a small fixture. Run
    // calibrate_light into out_a.fits; run compute + write_calibrated_output
    // into out_b.fits; assert the two files' bytes are identical and the two
    // outcomes agree on calstat/flat_norm_divisor/output_hash.
}
```

- [ ] **Step 2: Run it** — `cargo test -p athenaeum-core light_cal::` → FAIL (functions missing).

- [ ] **Step 3: Refactor** — `calibrate_light_inner` keeps its body up to (and including) the band loop and the floored-pixels warn, then returns `(CalibratedFrame, LightCalOutcome)` with an empty hash. `write_calibrated_output` wraps `write_fits_f32` + `compute_xxhash` + the two `io_err` maps. `calibrate_light` composes and fills `outcome.output_hash`. Move `LIGHT_CAL_ENGINE_VERSION` here (grep its definition in `db/light_calibrations.rs`), bump by 1, and fix every import to the new path (grep `LIGHT_CAL_ENGINE_VERSION`); find where `ATH_CVER` is stamped (grep `ATH_CVER`) and make sure it reads this const.

- [ ] **Step 4: Run the whole engine suite** — `cargo test -p athenaeum-core light_cal` → PASS (old + new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "refactor(calibration): split light-cal engine into compute + write; move engine version"
```

---

### Task 5: Move per-frame resolution into calibration_library

**Files:**
- Create: `crates/athenaeum-core/src/calibration_library/light_resolve.rs`
- Modify: `crates/athenaeum-core/src/calibration_library/mod.rs`, `crates/athenaeum-core/src/api/lights.rs`

**Interfaces:**
- Produces (BINDING for Task 6): a PURE MOVE of these items from `api/lights.rs` into `light_resolve.rs`, re-exported `pub`: `ResolvedMaster` (fields `set_id, uuid, path`), `ResolvedFrameInputs` (fields as today: `frame_id, light_path, source_filename, source_uuid, object, instrume, date_obs_date, source_cards, cfa_geometry, dark, flat, bias`), `resolve_frame_inputs(conn, frame_id, flat_norm) -> Result<ResolvedFrameInputs, ApiError>` (`api/lights.rs:1293`), `resolve_master(conn, set_id)`, `link_set_id(links, cal_type)`, `source_cards_for_file(..)` and whatever private helpers they pull (follow compiler errors — move, don't duplicate). `ApiError` in signatures: change to `anyhow::Result` if `ApiError` would create an api→calibration_library cycle in reverse; `calibration_library` must NOT import `api` — convert `ApiError::NotFound(..)` sites to `anyhow::bail!` and let `api::lights` wrap.
- `api::lights` re-imports everything from the new module; its public surface is unchanged this task.

- [ ] **Step 1: Move + rewire** (no new tests — this is a behavior-preserving move; the existing api::lights and light_cal suites are the net).
- [ ] **Step 2: Gate** — `cargo build --workspace && cargo test -p athenaeum-core lights` → PASS.
- [ ] **Step 3: Commit** — `git add -A && git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "refactor(calibration): move per-frame master resolution out of the api layer"`

---

### Task 6: The calibrated-light generator

**Files:**
- Create: `crates/athenaeum-core/src/export/calibrated_generator.rs`
- Modify: `crates/athenaeum-core/src/export/mod.rs` (register + re-export)

**Interfaces:**
- Consumes: Task 3 (`hot_pixel_map_from_dark`, `apply_hot_pixel_correction`), Task 4 (`calibrate_light_compute`, `write_calibrated_output`), Task 5 (`resolve_frame_inputs`), `light_headers::{build_light_cal_cards, LightCalCardInputs}`, `rustafits::processing::vng::vng_debayer_f32`, `crate::integration::cfa::CfaGeometry`, `scale_divisor_for_bitpix`.
- Produces (BINDING for Tasks 8 and 9):
  ```rust
  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct CalibratedLightOptions {
      pub flat_norm: bool,
      pub flat_norm_mode: FlatNormMode,
      pub params: LightCalParams,
      pub hot_pixel_correction: bool, // default true
      pub debayer_osc: bool,          // default true
  }
  impl Default for CalibratedLightOptions { /* true/CentralThird/default/true/true */ }

  pub struct GeneratedLight { pub calstat: String, pub debayered: bool, pub hot_pixels_replaced: u64, pub output_hash: String, pub byte_size: u64 }

  /// Catalog-only phase (short conn borrow, no pixel I/O): resolution + cards
  /// prediction. Port the per-frame section of api::lights::run_light_cal —
  /// the calstat prediction (dark→"BD"/bias→"B"; +"F" with a flat), the
  /// bias_fallback policy, the divisor re-resolution for the header, and the
  /// LightCalCardInputs/LightCalInputs assembly — VERBATIM; that section then
  /// dies with the old worker in Task 11.
  pub fn resolve_generation(conn: &Connection, frame_id: i64, opts: &CalibratedLightOptions) -> anyhow::Result<GenerationSpec>;

  pub struct GenerationSpec { /* owns the assembled LightCalInputs (output_path left empty),
      the base Vec<Card>, cfa_geometry: Option<CfaGeometry>, dark_path: Option<PathBuf>,
      debayer: bool  — true iff opts.debayer_osc && cfa_geometry.is_some() */ }
  impl GenerationSpec { pub fn output_filename(&self, source_filename: &str) -> String; }
  // output_filename: "c_<stem>.fits", or "c_<stem>_d.fits" when self.debayer.

  /// Pixel phase (no DB): compute → hot-pixel fix → optional VNG → final cards → write.
  /// `hot_maps` caches maps per dark path across a batch.
  pub fn execute_generation(
      spec: &GenerationSpec,
      output_path: &Path,
      scratch_dir: &Path,
      opts: &CalibratedLightOptions,
      hot_maps: &mut HashMap<PathBuf, std::sync::Arc<HotPixelMap>>,
      cancel: &AtomicBool,
  ) -> anyhow::Result<GeneratedLight>;
  ```
- `execute_generation` order (spec §3/§5/§6/§7): `calibrate_light_compute` → if `opts.hot_pixel_correction` and `spec.dark_path` is Some: get/insert the map, `apply_hot_pixel_correction` (cfa = spec.cfa_geometry) → if `spec.debayer`: map `CfaGeometry.pattern` to `rustafits BayerPattern` honoring `xoff/yoff` parity (an odd offset shifts the pattern: RGGB with xoff 1 ⇒ GRBG, etc. — write a small `bayer_for(geom)` helper with a unit test for all 4 patterns × 4 offset parities) and run `vng_debayer_f32` → finalize cards: start from spec's base cards; when debayered REMOVE keywords `BAYERPAT`/`XBAYROFF`/`YBAYROFF` and push `Card::new("ATH_CDBM", CardValue::Str("VNG".into()))`; when correction ran push `ATH_CHPX` (integer count) → `write_calibrated_output(output_path, w', h', channels, data, cards)` (channels 3 + halved? NO — full res: same w/h, channels 3) → stat the file for `byte_size`.

- [ ] **Step 1: Write failing tests** — build a seeded in-memory catalog + tiny real files (mirror the fixture style of the existing `api/lights.rs` tests — grep `fn seed` / the `#[cfg(test)]` helpers there and adapt locally):
  - `mono_generation_produces_c_fits_with_calstat` — light + master dark linked; output exists, CALSTAT "BD", NAXIS3 absent, ATH_CHPX present, filename `c_x.fits`.
  - `osc_generation_debayers_to_3_planes` — RGGB light + dark + flat; output NAXIS3=3, no BAYERPAT card, ATH_CDBM='VNG', filename `c_x_d.fits`.
  - `debayer_off_keeps_cfa` — same catalog, `debayer_osc:false` → 1 plane, BAYERPAT retained.
  - `hot_map_cached_per_dark` — two frames, same dark → `hot_maps.len() == 1` after both.
- [ ] **Step 2: Run to fail** — `cargo test -p athenaeum-core calibrated_generator` → FAIL.
- [ ] **Step 3: Implement** as specified above.
- [ ] **Step 4: Run to pass** — same filter → PASS; also `cargo test -p athenaeum-core light_cal lights` still green.
- [ ] **Step 5: Commit** — `git add -A && git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(export): calibrated-light generator (resolve + compute + cosmetic + VNG + write)"`

---

### Task 7: Readiness gate rewrite + frontend prefs relocation

**Files:**
- Modify: `crates/athenaeum-core/src/api/lights.rs` (readiness), `crates/athenaeum-core/src/api/frame_set_send.rs`, `crates/athenaeum-tauri/src/commands/export.rs:151` (`get_export_readiness`), `crates/athenaeum-web/src/routes/export.rs` (mirror), `crates/athenaeum-core/src/ts_export.rs`
- Create: `src/components/export/lightCalPrefs.ts`
- Modify: `src/components/export/ExportTab.tsx`, `src/components/transfers/SendToNodeDialog.tsx`, `src/types/models.ts`

**Interfaces:**
- Produces (BINDING for Tasks 8/9):
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
  #[serde(rename_all = "camelCase")]
  pub struct ExportReadiness {
      pub total: i64,                       // in-scope LIGHT members
      pub unlinked_lights: i64,             // lights with ZERO calibration links
      pub raw_sets_without_master: i64,
      pub raw_set_ids_without_master: Vec<i64>,
      pub file_counts: ExportFileCounts,
  }
  pub fn get_export_readiness(ctx: &ServiceContext, set_id: i64) -> Result<ExportReadiness, ApiError>;
  pub fn check_mode_ready(r: &ExportReadiness, mode: ExportMode) -> Result<(), String>;
  // calibratedLights blockers, exact strings the UI shows:
  //   raw sets:  "Build masters first — {n} set{s} without a master"
  //   unlinked:  "{n} light{s} have no calibration links"
  // Readiness params dropped; generation options added (the CalibratedLights
  // transform needs debayer_osc to decide the `_d` output filenames):
  pub fn frame_set_entries(ctx, frame_set_id: i64, mode: ExportMode, gen_opts: &CalibratedLightOptions) -> Result<Vec<PayloadEntry>, ApiError>;
  ```
  TS: `ExportReadiness { total, unlinkedLights, rawSetsWithoutMaster, rawSetIdsWithoutMaster, fileCounts }`. New `src/components/export/lightCalPrefs.ts` exports `readFlatNormPref/readFlatNormModePref/readLightCalParamsPref` MOVED from `CalibrateLightsDialog.tsx` (same localStorage keys, verbatim), plus new `readHotPixelPref()/readDebayerPref()` (keys `athenaeum.lightcal.hotPixel` / `.debayer`, default `true`) and their `write*` setters.
- Rewrite `compute_export_readiness`: reuse the existing per-frame `classify` walk for raw-set detection; a light whose `get_links_for_frame` list is empty increments `unlinked_lights`. Drop `calibrated/stale/missing` computation and the `derive_status` dependency from readiness. `check_mode_ready(CalibratedLights)` = the two blockers above; other modes unchanged. `fileCounts.calibrated_lights` = light count.
- Both backends: `get_export_readiness` command loses `flat_norm/flat_norm_mode/params` args; `export_to_wbpp` and `enqueue_frame_set_send` KEEP their option args (they now feed generation, Tasks 8/9 — do not remove there). Frontend `useExportReadiness`-style call sites (grep `get_export_readiness` in `src/`) drop the extra args; `modeBlocker` in `ExportTab.tsx:41-49` swaps the calibratedLights branch to the two new counts. Update imports of the pref readers in ExportTab + SendToNodeDialog to the new file (`CalibrateLightsDialog.tsx` keeps a re-export shim until Task 11 deletes it).

- [ ] **Step 1: Write failing core tests** (in api/lights.rs tests, adapting the existing readiness fixtures): `unlinked_light_blocks_calibrated_mode`, `raw_linked_set_blocks_with_build_masters_message`, `partial_links_pass` (dark-only light + all-masters → `check_mode_ready(CalibratedLights)` Ok).
- [ ] **Step 2: Run to fail**, **Step 3: implement**, **Step 4: run** — `cargo test -p athenaeum-core lights frame_set_send && cargo build --workspace && npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** — `git add -A && git -c … commit -m "feat(export): masters-built readiness gate for calibrated-lights mode; prefs move to export"`

---

### Task 8: Export executor generation (both backends + UI toggles)

**Files:**
- Modify: `crates/athenaeum-core/src/export/file_organizer.rs`, `crates/athenaeum-core/src/export/data_collector.rs`, `crates/athenaeum-core/src/export/models.rs` (ExportFrame + ExportProgressEvent doc), `crates/athenaeum-tauri/src/commands/export.rs:192` (`export_to_wbpp`), `crates/athenaeum-web/src/routes/export.rs` (mirror), `src/components/export/ExportTab.tsx`, `src/hooks/useExportProgress.ts`, `src/types/models.ts`

**Interfaces:**
- Consumes: Task 6 (`resolve_generation/execute_generation/GenerationSpec/CalibratedLightOptions`), Task 7 gate.
- Produces:
  ```rust
  // models.rs — on ExportFrame:
  pub debayer_calibrated: Option<bool>, // None = copy; Some(d) = generate (d = debayer)
  // file_organizer.rs — on WbppPlacement:
  pub source: PlacementSource,
  #[derive(Debug, Clone, PartialEq)]
  pub enum PlacementSource { Copy, CalibrateLight { frame_id: i64, debayer: bool } }
  // organize_files_wbpp gains one arg:
  pub struct GenerationBatch<'a> {
      pub specs: HashMap<i64, GenerationSpec>, // frame_id → spec (pre-resolved)
      pub opts: CalibratedLightOptions,
      pub scratch_dir: PathBuf,
  }
  pub fn organize_files_wbpp(output_dir, data, use_symlinks, _config, emitter, frame_set_id, cancel_flag, generation: Option<&mut GenerationBatch>) -> Result<OrganizeResult>;
  ```
- `apply_export_mode` gains an options arg — `apply_export_mode(conn, data, mode, gen_opts: Option<&CalibratedLightOptions>)` (CalibratedLights requires `Some`; every other mode ignores it; update the existing test callers). `apply_calibrated_lights` (data_collector) rewrite: `drop_calibration_nodes`; per light, query `frames.bayerpat` (recognized pattern? reuse the same parse the resolver uses) → `frame.debayer_calibrated = Some(gen_opts.debayer_osc && recognized)`, and set `frame.filename` to `GenerationSpec::output_filename` shape (`c_<stem>.fits` / `c_<stem>_d.fits` — stem = source filename minus extension). It no longer reads any tracking table and never bails on missing artifacts (the gate handled readiness). The dedup/claims machinery then works on the OUTPUT names for free. `compute_wbpp_placements` maps `frame.debayer_calibrated` into `WbppPlacement.source` (`None` → `Copy`, `Some(d)` → `CalibrateLight { frame_id, debayer: d }`).
- `organize_files_wbpp` dispatch: `PlacementSource::Copy` → existing `copy_or_link` with exists-skip; `CalibrateLight` → look up the spec, `execute_generation(spec, &dest, …)` (overwrites via tmp+rename — NO exists-skip), warnings on per-frame error (`warnings.push`, continue), progress phase `"calibrating"` for those placements (`"copying"` stays for Copy). Maintain a local `hot_maps` HashMap for the run.
- Both host commands (`export_to_wbpp` Tauri + web mirror): gain `hot_pixel: Option<bool>, debayer: Option<bool>`; when resolved mode == CalibratedLights: build `CalibratedLightOptions` from args (defaults true), short conn borrow → `resolve_generation` for every marked light (a per-frame resolve error becomes a warning + that placement skipped), acquire ONE compute slot `state.ctx.compute_queue.acquire(ComputeJobKind::LightCalibration, &format!("export set {frame_set_id}"), cancel_flag.clone())` for the organize call, drop the permit after. `QueueCancelled` → the cancelled ExportResult.
- Frontend: two checkboxes in the Export Mode section (visible when `exportMode === 'calibratedLights'`): "Hot-pixel correction" and "Debayer OSC lights (VNG)", persisted via `readHotPixelPref/readDebayerPref`; pass through `startExport` (extend `useExportProgress`'s invoke args with `hotPixel`/`debayer`); phase label: show "Calibrating…" when `phase === 'calibrating'` wherever the copy phase renders.

- [ ] **Step 1: Write failing core test** — `organize_generates_calibrated_lights`: seeded conn + tiny fixtures (reuse Task 6 fixtures), `collect_export_data` + `apply_export_mode(CalibratedLights)` with debayer off, resolve specs, run `organize_files_wbpp` with the batch → `<out>/<set>/camera_x/lights/c_a.fits` exists, `files_organized` counts it, calibration folders absent. Plus `apply_calibrated_lights_marks_and_renames` (unit: frames get `Some(debayer)` + `c_*` names, nodes dropped).
- [ ] **Step 2: Run to fail**, **Step 3: implement core**, **Step 4: run core tests** → PASS.
- [ ] **Step 5: Wire both host commands + UI**, run `cargo build --workspace && npx tsc --noEmit` → PASS.
- [ ] **Step 6: Commit** — `"feat(export): calibrate lights during export — generation placements, compute-queue slot, calibrating phase, UI toggles"`

---

### Task 9: Send-prepare generation + receiver land-only

**Files:**
- Modify: `crates/athenaeum-core/src/api/frame_set_send.rs` (PayloadEntry + entries), `crates/athenaeum-core/src/api/sync.rs` (`plan_selection_package`, `PlannedSelection`, `enqueue_frame_set_send`), `crates/athenaeum-core/src/api/sync_prepare.rs` (`PrepareJob`, `stage_records`), `crates/athenaeum-core/src/sync/ingest.rs` (`process_calibrated_light`), Tauri `commands/` + web `routes/` for `enqueue_frame_set_send` (add `hot_pixel`/`debayer` args), `src/components/transfers/SendToNodeDialog.tsx`

**Interfaces:**
- Produces:
  ```rust
  // frame_set_send.rs (PayloadEntry is UNGATED — Perseus must not break):
  pub struct PayloadEntry { pub frame_id: i64, pub source_path: PathBuf, pub rel_path: String, pub kind: PayloadKind, pub generate: bool /* new, false everywhere but calibrated-lights sends */ }
  // sync.rs:
  pub(crate) enum PrepareSource { Copy(PathBuf), Generate { frame_id: i64 } }
  // PlannedSelection.records / PrepareJob.records: Vec<(PrepareSource, ManifestRecord)>
  // PlannedSelection + PrepareJob gain: pub gen_opts: Option<CalibratedLightOptions>
  ```
- `frame_set_entries` (CalibratedLights branch): `source_path` = the RAW light (placement.file_path already is, after Task 8's transform), `generate = true`, rel filename already `c_*`/`c_*_d` from the transform. Every other producer (selection sends, Perseus): `generate: false` — fix all struct literals.
- `plan_selection_package` gains `gen_opts: Option<CalibratedLightOptions>` as a parameter (stored on `PlannedSelection`); an entry with `generate` keeps the existing stat of `source_path` (raw light — exists check + `byte_size` ESTIMATE), `is_catalog_file = false` path (no bank, no analysis, fresh uuid — the CalibratedLight arm already does all three), and records `PrepareSource::Generate { frame_id }`; copies record `PrepareSource::Copy(path)`. `enqueue_frame_set_send` builds `CalibratedLightOptions` from its (extended) args and threads it into `PlannedSelection.gen_opts` → `PrepareJob.gen_opts`.
- `stage_records`: if any Generate record exists, acquire one compute slot (`ctx` is reachable — `spawn_prepare` owns `Arc<ServiceContext>`; pass `&ctx` into `stage_records`) with the prepare's cancel flag, and resolve specs in one short conn borrow BEFORE the loop (`resolve_generation` per frame; a resolve failure = `PrepareError::Failed` with the frame's rel_path as culprit). In the loop: `Copy` → `stage_payload` as today; `Generate` → `execute_generation(spec, &dest, scratch, opts, &mut hot_maps, cancelled)`, then `record.byte_size = generated.byte_size`, hash = `generated.output_hash`, emit the terminal file tick with the REAL size, and after the loop also update the outbound file row size: add `store` fn `update_outbound_file_size(conn, outbound_id, rel_path, size)` (plain UPDATE on `sync_outbound_files`) called per generated record (job.id is the outbound id). `total` for batch progress: recompute after generation is fine to leave as the estimate — document with a comment.
- Cancellation: `execute_generation` already takes the cancel flag; map its cancel error to `PrepareError::Cancelled` (match on the `IntegrationError::Cancelled` source via downcast, mirroring the `StageCancelled` arm).
- Receiver `process_calibrated_light`: keep the landing (file write/link machinery, receipt "ingested", history) and DELETE the adopt — no `light_calibrations` imports remain. The scanned-later case is covered by Task 10's scanner skip.
- Hosts: `enqueue_frame_set_send` command (grep it in `crates/athenaeum-tauri/src/commands/` and the web route) gains `hot_pixel: Option<bool>, debayer: Option<bool>`; `SendToNodeDialog.tsx` passes `readHotPixelPref()/readDebayerPref()`.

- [ ] **Step 1: Failing tests** — `plan_marks_generate_records` (entry.generate → PrepareSource::Generate + no bank candidate); `stage_records_generates_into_package` (fixture catalog: record Generate → dest exists, manifest xxh3 non-empty, byte_size == file size); `ingest_calibrated_light_lands_without_tracking` (adapt the existing ingest test that asserted the adopt — now asserts the file lands and NO table access happens).
- [ ] **Step 2: Run to fail**, **Step 3: implement**, **Step 4: run** — `cargo test -p athenaeum-core sync frame_set_send ingest && cargo build --workspace && npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** — `"feat(sync): generate calibrated lights during transfer preparation; receiver lands them without tracking"`

---

### Task 10: Scanner skip + collab decision-C neutering

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs`, `crates/athenaeum-core/src/collab/gate.rs`, `crates/athenaeum-core/src/api/collab.rs`, `crates/athenaeum-core/src/export/project_collector.rs`, `crates/athenaeum-core/src/api/collab_e2e_tests.rs`, `src/types/models.ts`, `src/hooks/useScanProgress.ts`

**Interfaces:**
- Scanner (`scanner/mod.rs:455-480` block): keep the `calibrated_light_identity` detection and the `ATH_PRJ → reconcile_project_contribution` routing EXACTLY as-is; replace the `reconcile_calibrated_light(..)` call with
  ```rust
  tracing::debug!(root_id, path = %current_path, "calibrated artifact (CALSTAT+ATH_CSRC) — never cataloged");
  return Ok(None);
  ```
  Delete `reconcile_calibrated_light`, `resolve_calibrated_source_frame`, `SourceResolution`, the `CalibratedDuplicate` struct, the `calibrated_duplicates_out` parameter chain and the scan-result field (grep `calibrated_duplicates`); remove the field from `src/types/models.ts` and its notification wiring in `useScanProgress.ts`. `fits_parser/calibrated_light.rs` STAYS (identity extraction feeds the skip + project routing). The second reconcile call site (`scanner/mod.rs:1928`) gets the same skip treatment.
- Collab (spec §8a): move the `LightCalStatus` enum definition into `collab/gate.rs` (delete the `db::light_calibrations` import); in `api::collab`, the gate-input resolution (grep `frame_cal_status` / the `cal_status:` construction) becomes
  ```rust
  // Decision C (spec 2026-08-31 §8a): light-cal artifacts are gone — collab
  // publish rework is a named follow-up. Until then no frame passes layer 1.
  cal_status: LightCalStatus::NotCalibrated,
  ```
  In `publish_collab_frames` (`api/collab.rs:1139`) and `project_collector.rs:88`: replace each `get_light_calibration_for_frame` lookup with an unconditional skip + `warn!("calibrated artifact unavailable — light calibration moved into export (collab rework pending)")` so the functions compile with the module gone and stay honest if ever reached. Delete `active_light_cal` usage in the collab test ctx ONLY if Task 11 hasn't run yet — otherwise skip (ordering: this task runs before Task 11, so leave `active_light_cal` alone here).
- Tests: collab/e2e tests that upsert `light_calibrations` rows to make frames publishable (grep `upsert_light_calibration` in `api/collab.rs`, `collab_e2e_tests.rs`, `project_collector.rs`) — rewrite each to assert the blocked behavior (`publishable == 0`, or `publish_collab_frames` → `Err(Invalid("no publishable frames"))`); a full publish→receive e2e that cannot run any other way gets `#[ignore = "collab publish rework pending — calibrated-export-v2 spec §8a"]`. Scanner tests asserting adopt/duplicate branches (grep `reconcile_calibrated_light` in tests, `scanner/mod.rs:3665` area) become one test: a CALSTAT+ATH_CSRC file scans to no `files`/`frames` row.

- [ ] **Step 1: Rewrite the affected tests first** (they define the new behavior), **Step 2: run to fail**, **Step 3: implement**, **Step 4: run** — `cargo test -p athenaeum-core scanner collab project_collector && npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** — `"feat(scanner)!: calibrated artifacts are never cataloged; collab publish honestly blocked (decision C)"`

---

### Task 11: Demolition of the standalone flow

**Files:**
- Delete: `crates/athenaeum-tauri/src/commands/lights.rs`, `crates/athenaeum-web/src/routes/lights.rs`, `crates/athenaeum-core/src/db/light_calibrations.rs`, `src/components/calibration/CalibrateLightsDialog.tsx`, `src/components/calibration/LightCalStatusBadge.tsx`
- Modify: `crates/athenaeum-tauri/src/lib.rs` (invoke_handler lines ~393-396 + `commands/mod.rs`), `crates/athenaeum-web/src/routes/mod.rs:257-260`, `crates/athenaeum-core/src/api/lights.rs`, `crates/athenaeum-core/src/api/mod.rs`, `crates/athenaeum-core/src/services/mod.rs` (`active_light_cal`), `crates/athenaeum-core/src/db/{mod.rs,schema.rs}`, `crates/athenaeum-core/src/ts_export.rs` (~lines 171-177), `src/pages/FrameSetDetail.tsx`, `src/types/models.ts`, the Coverage table component that renders the badge/tooltip (grep `LightCalStatusBadge` + `detailsByFrameId`)

**Interfaces:** consumes everything above; produces the final surface. Removal inventory (grep each name to zero):
- Commands both backends: `get_light_calibration_readiness`, `get_light_calibration_details`, `start_light_calibration`, `cancel_light_calibration`.
- `api::lights`: `run_light_cal_thread`, `run_light_cal`, `start_light_calibration`, `cancel_light_calibration`, `get_light_calibration_readiness`, `get_light_calibration_details`, `compute_details`, `LightCalScope`, `LightCalHandle`, `LightCalReadiness`, `LightFrameReadiness`, `LightCalDetails`, `CalibrationFinishedEvent`/progress events of that flow, the preflight handshake (`wait_for_preflight_builds`, `preflight_build_set_ids`), `master_filename` if now unused. KEEP: `get_export_readiness`, `check_mode_ready`, `ExportReadiness`, the CFA advisory helpers the readiness still uses.
- `ServiceContext.active_light_cal` field + every constructor (incl. `api/collab.rs:1910` test ctx).
- DB: delete `db/light_calibrations.rs` + its `db/mod.rs` export; in `schema.rs` remove the whole B5 block (`CREATE TABLE light_calibrations` ~1818-1877, its indexes and guarded ALTERs) and add, in the migrations section:
  ```rust
  // Calibrated-export v2 (2026-08-31): light calibration generates at export;
  // the tracking table is gone. Idempotent — a fresh DB never creates it.
  conn.execute_batch("DROP TABLE IF EXISTS light_calibrations")?;
  ```
  Check `schema.rs:158` (a trigger/constraint referencing light_calibrations) and remove that reference too.
- ts_export: drop `LightCalReadiness`, `LightCalDetails`, `LightCalScope` (keep `ExportReadiness`, `LightCalParams`, `FlatNormMode`); prune the same types + `LightFrameReadiness` from `src/types/models.ts`.
- Frontend `FrameSetDetail.tsx`: `showCalibrateDialog` state + dialog JSX (~1150-1168), `loadLightCalReadiness`/`loadLightCalDetails` + their state maps, `onCalibrateLights`/`readinessByFrameId`/`detailsByFrameId` props (and their consumption in the Coverage component), the `CalibrateLightsDialog` import (prefs readers already live in `lightCalPrefs.ts`; delete the shim with the file).
- The B5 `#[ignore]`d real-data e2e (`real_data_e2e_light_calibration` in `api/lights.rs`) is REPOINTED, not deleted (spec §11): rewrite it to drive `resolve_generation` + `execute_generation` over its env-var real-data catalog and assert the same outputs land — it becomes the export path's real-data harness.

- [ ] **Step 1: Delete + fix compilation** (compiler-error-driven; grep every name in the inventory to zero afterwards).
- [ ] **Step 2: Migration test** — add to schema tests: create a conn, run old `CREATE TABLE light_calibrations(id INTEGER PRIMARY KEY)` manually, `init_db` → table gone; fresh `init_db` twice → idempotent.
- [ ] **Step 3: Full gates** — `cargo build --workspace && cargo test -p athenaeum-core && npx tsc --noEmit` → PASS.
- [ ] **Step 4: Commit** — `"feat(calibration)!: remove the standalone Calibrate Lights flow and the light_calibrations table"`

---

### Task 12: Docs, open-items, submodule pin

**Files:**
- Modify: `CLAUDE.md`, `docs/export/README.md`, `docs/superpowers/open-items.md`, submodule pin (`git add rustafits`)

- [ ] **Step 1: CLAUDE.md** — rewrite the "In-App Light Calibration (B5)" section as "Calibrated-Lights Export" describing: generation at export/send-prepare (generator files, options, gate, `ATH_CHPX`/`ATH_CDBM` cards, VNG in rustafits), the scanner's never-catalog skip, decision C for collab (gate blocked, rework = follow-up). Update the frame-set-send bullet (`PayloadEntry.generate`) and the module map (`cosmetic`, `light_resolve`, `calibrated_generator`; `db::light_calibrations` gone). Also refresh the command-count line's accuracy note (4 commands removed).
- [ ] **Step 2: docs/export/README.md** — document the Calibrated lights mode's new behavior + the two toggles.
- [ ] **Step 3: open-items.md** — add: (a) owner smoke: real-data export of LDN 1272 in Calibrated lights mode, spot-compare against the reference outputs (math + VNG + hot-pixel counts); (b) owner smoke: two-instance calibrated-lights send; (c) follow-up cycle: collab publish rework (generate-at-publish, gate = masters-built) per spec §8a; (d) note: old `c_*` trees under the Calibration Library root are uncataloged leftovers — manual cleanup.
- [ ] **Step 4: Pin the submodule** — `git add rustafits CLAUDE.md docs && git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "docs: calibrated-export v2 — CLAUDE.md/export docs/open-items; pin rustafits VNG"`
- [ ] **Step 5: Final full gate** — `cargo build --workspace && cargo test --workspace && npx tsc --noEmit` → all green.
