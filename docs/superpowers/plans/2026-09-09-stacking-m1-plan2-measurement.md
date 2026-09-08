# Stacking M1 — Plan 2: Measurement and Statistics — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure a calibrated light frame the way the stacking pipeline
needs it — per channel: PSF fits, the hybrid PSF/aperture flux at FWTM, the
robust totals behind the PSF Signal Weight and PSF SNR, the large-scale
background residual (`M*`, `N*`), MRS noise, the two-sided normalization
statistics — and turn a group of measurements into weights, a selection with
reasons, and the per-frame `(scale, offset)` normalization pairs the
integration engine will apply.

**Architecture:** One new `render`-gated statistics module,
`integration/stats.rs` (median / MAD / average deviation / biweight
midvariance, two-sided; the stratified sample; normalization pairs), and a
new `stacking` module gated on `render + solver` per spec §10.3 with four
leaves: `robust.rs` (erf family, Robust Chauvenet Rejection, Winsorization),
`psf_signal.rs` (Moffat fits on detections, FWTM aperture, totals, `M*`/`N*`,
MRS wrapper, the two estimators), `measure.rs` (per-plane / per-frame
driver over `PlaneReader`, serializable results), `weights.rs` (weight
modes, selection, best-by-weight). Nothing here touches the database, the
commands or the UI — those are Plans 4–5. Two carry-forwards from Plan 1's
final review land first (`PlaneReader` scratch reuse + contract tests, a
stale `FrameSource` doc sentence).

**Tech Stack:** Rust 2021, `athenaeum-core`, the `rustafits` submodule (lib
name `astroimage`: `ImageAnalyzer::detect_fast_data`,
`analysis::fitting::fit_moffat_2d_fixed_beta`,
`analysis::background::{estimate_background_mesh, estimate_noise_mrs}`),
`rayon`, `serde`/`serde_json`, `tempfile` in tests, `fits_writer::write_fits_f32`
for fixtures. No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`
(§4 measurement / weights / selection, §5.1 global normalization, §6.3
`minWeight`, §9.2 config names, §10.3 gating, §13 unit tests) with the math
in `docs/superpowers/research/2026-09-08-stacking-math-reference.md` (§1
PSF Signal Weight / PSF SNR, §1.3 `M*`/`N*`, §1.4 fit acceptance, §1.5
classic formula, §2.3 MRS, §2.4 scale estimators, §2.5 noise scale factors,
§3.4 RCR, §3.6 output normalization).

## M1 program index (this is Plan 2 of 5)

| Plan | Scope | Status |
| ---- | ---- | ---- |
| 1 | kernels, warp, window, `Linear`/`PixelMap`/`Polynomial2D`, KD-tree, RANSAC, `PlaneReader`, `FrameSource`, `RegisteredSource`, `integrate_registered` | merged to local `main` 2afa55ad |
| **2 (this)** | `integration/stats.rs` + `stacking/{robust,psf_signal,measure,weights}.rs` + carry-forwards + `measure_probe` example | — |
| 3 | Registration v2 service → Checkpoint A | after this |
| 4 | Combiner v2 + engine (weights, offsets, masks, rejection maps, channel loop) + WCS writer + master cards → Checkpoint B | consumes `NormalizationPair`, `FrameWeight` |
| 5 | Orchestration, tables, commands ×2, `ts_export`, events, the Stacking tab, acceptance run | consumes `MeasureOptions`, `FrameMeasurement`, `WeightMode`, `SelectionConfig`, `best_by_weight` |

**Checkpoint B addition (from this plan):** the acceptance comparison gains
"PSF Signal Weight rank correlation between our per-frame values and the
WBPP log's per-frame weights ≥ 0.9 on the 208-frame mono group" — the
constants in §1.2 are kept for cross-tool comparability, so the *ranking*
is the testable claim, not the absolute value. `examples/measure_probe.rs`
(Task 8) is the tool for that comparison.

## Rulings made while writing this plan (recorded so executors do not re-derive them)

1. **Module gating.** `stacking` is `#[cfg(all(feature = "render", feature = "solver"))]` exactly as spec §10.3 pins it, although nothing in this plan needs `solver` — the cfg never has to move when Plan 3/5 add registration consumers. `integration/stats.rs` sits under the existing `render`-gated `integration` so `stacking` can use it (`render + solver ⊂ render`).
2. **`M*`/`N*` model.** The reference's large-scale model is a decimated multiscale *median* transform at model scale 256. We use the mesh background estimator already in `astroimage` (`estimate_background_mesh`, cell 128 px ⇒ structures below roughly the model scale are smoothed away) as the model `L`; `R = {L − v : v ≠ 0, v < L}` over the 1/16 stratified sample; `M* = median(R)`, `N* = 2.48308·MAD(R)`. Cost if wrong: `M*` enters every frame's PSFSW denominator the same way, so relative weights shift by a few percent at most; the Checkpoint B rank correlation is the guard.
3. **Absolute floors in `astroimage`.** `estimate_noise_mrs` and the fast detector carry floors tuned for 16-bit ADU data (`0.001`), which a calibrated `[0, 1]` frame with 20–60 ADU of sky noise would sit on. Detection, fitting, MRS and the background model therefore run on a copy scaled by `ADU_SCALE = 65535`; every reported quantity is divided back (PSFSW and PSF SNR are scale-invariant by construction). Test `mrs_noise_tracks_the_true_sigma_and_survives_stars` pins a 20-ADU case.
4. **Auto PSF model per frame, not per star.** `Auto` fits the 64 brightest seeds with each β ∈ {2.5, 4, 6, 10}, keeps the β with the smallest median normalized residual, then fits every seed with that β. Per-star β selection would quadruple the fitting cost of the measure stage for a second-order effect on `TMeanFlux`.
5. **Fit acceptance** follows §1.4: converged, finite, positive amplitude and widths, centre within 1.5 px of the detection and inside the stamp shrunk by 15 % per side, the FWTM ellipse inside the stamp, then one fit per ±1 px (brighter wins). No saturation filter — RCR on the mean fluxes handles saturated fits, as the reference does.
6. **Line-fit deviation** (RCR phase 0) regresses the lowest `trunc(0.683N + 0.317)` sorted deviations on the half-normal quantiles with a line **through the origin** (`b = Σxy/Σx²`, `ŷ(1) = b`); quantiles use linear interpolation at position `p·(n − 1)`.
7. **Two-sided scale clipping** excludes samples outside `(1/65535, 1 − 1/65535)` (zeros mark missing coverage, the top marks saturation); it does not clamp them.
8. **`noise` weight mode** uses the mean of the two noise-scale factors (§2.5) as `noiseScale`.
9. **Formula mode degenerate ranges** (`max == min` inside the group) contribute the term's full weight.
10. **Selection normalization** — the group maximum excludes manually excluded frames; a missing `EXPTIME` / keyword value is its own exclusion reason, checked before the weight gate.
11. **Frame-level filters** use the mean over channels for FWHM / eccentricity and the minimum over channels for the star count.
12. **`measure_probe`** is a dev probe (prints one frame's `FrameMeasurement` as JSON), not the real-data harness the owner declined.

## Global Constraints

- Logic lives in `crates/athenaeum-core`; nothing in this plan touches
  `athenaeum-tauri`, `athenaeum-web`, the database or the frontend.
- `tracing` is the only logging API; `println!`/`eprintln!` are forbidden
  outside `#[cfg(test)]` and `examples/`. Messages are short stable phrases,
  data in snake_case fields; the field names this plan introduces are listed
  in Task 8 and added to the logging spec's dictionary there.
- Never name the reference implementation (or any other product) in code or
  comments; cite the math reference document or the papers.
- The headless build must keep passing: `cargo check -p athenaeum-core
  --no-default-features`. `integration` (and `stats.rs`) stay behind
  `render`; `stacking` is behind `render + solver`.
- No new crate dependencies. `serde_json`, `serde`, `rayon`, `tracing`,
  `tempfile` are already there.
- Every serde-visible enum in this plan uses `#[serde(rename_all =
  "camelCase")]` and the variant names in spec §9.2, pinned by tests — Plan 5
  copies them, never guesses.
- Pixel convention: 0-based, integer coordinates are pixel centres. Rotation
  convention for elliptical fits: `u = dx·cosθ + dy·sinθ`, `v = −dx·sinθ +
  dy·cosθ` (the fitter's own).
- Calibrated frames are float32 in `[0, 1]`; the reported statistics are in
  those native units.
- Commit after every task, as the user (`git -c user.name=eg013ra1n -c
  user.email=vilen.sharifov@gmail.com commit …`), with the two trailers
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY`.
- `rustfmt` only on files this plan creates (`stats.rs`, `robust.rs`,
  `psf_signal.rs`, `measure.rs`, `weights.rs`, `stacking/mod.rs`,
  `measure_probe.rs`). Never run rustfmt on `integration/mod.rs`, `lib.rs`,
  `plane_reader.rs`, `source.rs`, `test_support.rs` or `banded.rs` — hand
  edits only there (`reference_repo_lint_fmt_gotchas`). Clippy is not a gate.
- Test command shape: `cargo test -p athenaeum-core <module path>` — e.g.
  `cargo test -p athenaeum-core stacking::robust`.
- Test fixtures never seed a raw `canonicalize()` result (the
  `fixtures_never_seed_a_raw_canonicalized_path` guard); these tasks only
  need `tempfile::tempdir()` paths passed straight to the writer/reader.

---

## File structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/integration/plane_reader.rs` (modify) | `read_rows_with_scratch`, contract tests |
| `crates/athenaeum-core/src/integration/source.rs` (modify, doc only) | reword the `on_bytes` sentence |
| `crates/athenaeum-core/src/integration/stats.rs` (create) | robust location/scale, stratified sample, noise-scale factors, normalization pairs + enums |
| `crates/athenaeum-core/src/integration/mod.rs` (modify) | `pub mod stats;` |
| `crates/athenaeum-core/src/test_support.rs` (modify) | `add_noise`, `MoffatStar`, `moffat_field` |
| `crates/athenaeum-core/src/stacking/mod.rs` (create) | module doc + leaves |
| `crates/athenaeum-core/src/stacking/robust.rs` (create) | erf/erfc/erfinv, `F(N)`, deviations, `rcr`, `winsorize` |
| `crates/athenaeum-core/src/stacking/psf_signal.rs` (create) | `PsfModel`, `Seed`, `FitParams`, `StarFit`, `fit_stars`, aperture, `signal_totals`, `frame_shape`, `background_residual`, `noise_mrs`, `psf_signal_weight`, `psf_snr` |
| `crates/athenaeum-core/src/stacking/measure.rs` (create) | `MeasureOptions`, `ChannelMeasurement`, `FrameMeasurement`, `measure_plane`, `measure_frame` |
| `crates/athenaeum-core/src/stacking/weights.rs` (create) | `WeightMode`, `FormulaWeights`, `WeightInput`, `FrameWeight`, `compute_weights`, `SelectionConfig`, `select_frames`, `best_by_weight` |
| `crates/athenaeum-core/src/lib.rs` (modify) | gate `stacking` |
| `crates/athenaeum-core/examples/measure_probe.rs` (create) + `Cargo.toml` `[[example]]` | dev probe |
| `CLAUDE.md`, `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (modify) | module map, dictionary rows |

---

### Task 1: `PlaneReader` scratch reuse, contract tests, `FrameSource` doc reword

**Files:**
- Modify: `crates/athenaeum-core/src/integration/plane_reader.rs` (`read_rows` at the `/// Rows [y0, y0 + rows)` doc comment; tests module at the end)
- Modify: `crates/athenaeum-core/src/integration/source.rs` (the three doc lines beginning `/// Fill rows` inside `trait FrameSource`)

**Interfaces:**
- Consumes: `PlaneReader::{open, width, height, channels, read_rows, read_plane}`, `IntegrationError::BadInput` (Plan 1).
- Produces: `PlaneReader::read_rows_with_scratch(&self, plane: usize, y0: usize, rows: usize, dst: &mut [f32], scratch: &mut Vec<u8>) -> Result<(), IntegrationError>`; `read_rows` keeps its signature and delegates.

- [ ] **Step 1: Write the failing tests** — append inside `mod tests` of `plane_reader.rs`:

```rust
    #[test]
    fn zero_rows_is_ok_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = f32_fixture(dir.path(), "mono.fits", 4, 4, 1);
        let r = PlaneReader::open(&p).unwrap();
        let mut dst: [f32; 0] = [];
        r.read_rows(0, 2, 0, &mut dst).unwrap();
        let mut scratch = vec![7u8; 3];
        r.read_rows_with_scratch(0, 4, 0, &mut dst, &mut scratch).unwrap();
        assert_eq!(scratch, vec![7u8; 3], "zero rows must not touch the scratch buffer");
    }

    #[test]
    fn a_non_fits_file_is_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"hello, not a fits file").unwrap();
        assert!(matches!(
            PlaneReader::open(&p),
            Err(IntegrationError::BadInput(_))
        ));
    }

    #[test]
    fn read_plane_returns_the_middle_plane_of_an_rgb_file() {
        let dir = tempfile::tempdir().unwrap();
        let (p, data) = f32_fixture(dir.path(), "rgb.fits", 6, 5, 3);
        let r = PlaneReader::open(&p).unwrap();
        assert_eq!(r.read_plane(1).unwrap(), data[30..60].to_vec());
    }

    #[test]
    fn scratch_reuse_matches_fresh_reads() {
        let dir = tempfile::tempdir().unwrap();
        let (p, data) = f32_fixture(dir.path(), "mono.fits", 9, 7, 1);
        let r = PlaneReader::open(&p).unwrap();
        let mut scratch = Vec::new();
        let mut a = vec![0f32; 2 * 9];
        r.read_rows_with_scratch(0, 1, 2, &mut a, &mut scratch).unwrap();
        assert_eq!(&a[..], &data[9..27]);
        let mut b = vec![0f32; 4 * 9];
        r.read_rows_with_scratch(0, 3, 4, &mut b, &mut scratch).unwrap();
        assert_eq!(&b[..], &data[27..63]);
        assert!(scratch.len() >= 4 * 9 * 4, "scratch grows to the largest read");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p athenaeum-core integration::plane_reader`
Expected: compile error — `read_rows_with_scratch` not found.

- [ ] **Step 3: Implement** — replace the body of `read_rows` and add the scratch variant (keep the validation exactly as it is today, just moved):

```rust
    /// Rows `[y0, y0 + rows)` of `plane`, decoded into `dst` (len `rows × width`).
    /// Allocates a fresh byte buffer per call; a loop over many bands should
    /// use [`PlaneReader::read_rows_with_scratch`].
    pub fn read_rows(
        &self,
        plane: usize,
        y0: usize,
        rows: usize,
        dst: &mut [f32],
    ) -> Result<(), IntegrationError> {
        let mut scratch = Vec::new();
        self.read_rows_with_scratch(plane, y0, rows, dst, &mut scratch)
    }

    /// Same as [`PlaneReader::read_rows`], reusing `scratch` for the raw
    /// bytes (grown as needed, never shrunk; untouched when `rows == 0`).
    pub fn read_rows_with_scratch(
        &self,
        plane: usize,
        y0: usize,
        rows: usize,
        dst: &mut [f32],
        scratch: &mut Vec<u8>,
    ) -> Result<(), IntegrationError> {
        if plane >= self.channels {
            return Err(IntegrationError::BadInput(format!(
                "plane {plane} of a {}-plane image",
                self.channels
            )));
        }
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!(
                "rows {y0}+{rows} beyond height {}",
                self.height
            )));
        }
        if dst.len() != rows * self.width {
            return Err(IntegrationError::BadInput(format!(
                "destination holds {} samples, {} rows need {}",
                dst.len(),
                rows,
                rows * self.width
            )));
        }
        if rows == 0 {
            return Ok(());
        }
        let bpp = self.kind.bytes_per_sample();
        let need = rows * self.width * bpp;
        if scratch.len() < need {
            scratch.resize(need, 0);
        }
        let offset =
            self.data_offset + plane as u64 * self.plane_bytes() + (y0 * self.width * bpp) as u64;
        pread_exact(&self.file, &mut scratch[..need], offset)?;
        self.kind.decode_run(&scratch[..need], 0, dst);
        Ok(())
    }
```

In `source.rs`, replace the three doc lines

```rust
    /// Fill rows `[y0, y0 + rows)` of every frame into `out`, calling
    /// `on_bytes` once per frame with the bytes read from disk, checking
    /// `cancel` between frames.
```

with

```rust
    /// Fill rows `[y0, y0 + rows)` of every frame into `out`, calling
    /// `on_bytes` once per frame with that frame's accounted share of the
    /// band (see the contract below — not its true disk traffic), checking
    /// `cancel` between frames.
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core integration::plane_reader` → all pass (10 tests).
Run: `cargo test -p athenaeum-core integration::` → everything still green.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/integration/plane_reader.rs crates/athenaeum-core/src/integration/source.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "refactor(integration): PlaneReader scratch-reusing reads and contract tests" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 2: `integration/stats.rs` — robust location and scale, stratified sample, normalization pairs

**Files:**
- Create: `crates/athenaeum-core/src/integration/stats.rs`
- Modify: `crates/athenaeum-core/src/integration/mod.rs` (add `pub mod stats;` between `pub mod source;` and `pub mod storage_class;`)
- Modify: `crates/athenaeum-core/src/test_support.rs` (append `add_noise`)

**Interfaces:**
- Consumes: nothing beyond `serde`.
- Produces (all `pub`, module `crate::integration::stats`):
  - constants `MAD_TO_SIGMA: f32 = 1.4826`, `AVGDEV_TO_SIGMA: f32 = 1.2533`, `BWMV_TO_SIGMA: f32 = 0.991`, `CLIP_LO: f32 = 1/65535`, `CLIP_HI: f32 = 1 − 1/65535`, `NOISE_CLIP_LO: f32 = 2/65535`, `NOISE_CLIP_HI: f32 = 1 − 2/65535`, `SAMPLE_STRIDE: usize = 4`
  - `fn median_in_place(values: &mut [f32]) -> f32`, `fn median_of(values: &[f32]) -> f32`, `fn mad_about(values: &[f32], m: f32) -> f32` (raw), `fn avg_dev_about(values: &[f32], m: f32) -> f32`, `fn bwmv_scale_about(values: &[f32], m: f32, mad: f32) -> f32` (σ-consistent)
  - `enum ScaleEstimator { Bwmv, Mad, AvgDev }` (serde `bwmv|mad|avgDev`, `Default = Bwmv`), `struct LocationScale { location: f32, scale: f32 }`
  - `fn clip_sample(values: &[f32], lo: f32, hi: f32) -> Vec<f32>`, `fn location_scale(values: &[f32], est: ScaleEstimator) -> Option<LocationScale>`
  - `fn for_each_stratified(width: usize, height: usize, f: impl FnMut(usize))`, `fn stratified_sample(data: &[f32], width: usize, height: usize) -> Vec<f32>`
  - `fn noise_scale_factors(values: &[f32]) -> Option<(f32, f32)>`
  - `enum OutputNormalization { None, Additive, AdditiveWithScaling, Multiplicative, MultiplicativeWithScaling }` (serde `none|additive|additiveWithScaling|multiplicative|multiplicativeWithScaling`, `Default = AdditiveWithScaling`), `enum RejectionNormalization { None, ScaleZeroOffset, EqualizeFluxes, Local }` (serde `none|scaleZeroOffset|equalizeFluxes|local`, `Default = ScaleZeroOffset`), `struct NormalizationPair { scale: f32, offset: f32 }` with `IDENTITY` and `apply(v) = v·scale + offset`, `fn output_pair(reference: LocationScale, frame: LocationScale, mode: OutputNormalization) -> NormalizationPair`, `fn rejection_pair(reference, frame, mode: RejectionNormalization) -> Option<NormalizationPair>` (`None` for `Local`)
  - test support: `pub(crate) fn add_noise(data: &mut [f32], sigma: f32, seed: u64)`

- [ ] **Step 1: Add the test helper** — append to `crates/athenaeum-core/src/test_support.rs`:

```rust
/// Adds zero-mean Gaussian noise of `sigma` in place (Box–Muller over a
/// SplitMix64 stream seeded by `seed`), so a fixture's noise is reproducible.
pub(crate) fn add_noise(data: &mut [f32], sigma: f32, seed: u64) {
    let mut rng = crate::geometry::ransac::SplitMix64(seed);
    let mut i = 0;
    while i < data.len() {
        let u1 = rng.next_f64().max(1e-12);
        let u2 = rng.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (2.0 * std::f64::consts::PI * u2).sin_cos();
        data[i] += (r * c) as f32 * sigma;
        if i + 1 < data.len() {
            data[i + 1] += (r * s) as f32 * sigma;
        }
        i += 2;
    }
}
```

- [ ] **Step 2: Write the failing tests** — create `stats.rs` with only this test module (the implementation comes in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::add_noise;

    fn gaussian(n: usize, sigma: f32, seed: u64) -> Vec<f32> {
        let mut v = vec![0.5f32; n];
        add_noise(&mut v, sigma, seed);
        v
    }
    fn close(a: f32, b: f32, rel: f32) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    #[test]
    fn median_conventions() {
        assert_eq!(median_of(&[5.0, 1.0, 4.0, 2.0, 3.0]), 3.0);
        assert_eq!(median_of(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median_of(&[7.0]), 7.0);
        assert!(median_of(&[]).is_nan());
    }

    #[test]
    fn mad_and_avg_dev_by_hand() {
        let v = [1.0, 2.0, 3.0, 4.0, 100.0];
        let m = median_of(&v);
        assert_eq!(m, 3.0);
        assert_eq!(mad_about(&v, m), 1.0);
        assert!((avg_dev_about(&v, m) - 101.0 / 5.0).abs() < 1e-5);
    }

    #[test]
    fn estimators_are_sigma_consistent_on_gaussian_noise() {
        let v = gaussian(20_000, 0.01, 1);
        let m = median_of(&v);
        let mad = mad_about(&v, m);
        assert!(close(mad * MAD_TO_SIGMA, 0.01, 0.03));
        assert!(close(avg_dev_about(&v, m) * AVGDEV_TO_SIGMA, 0.01, 0.03));
        assert!(close(bwmv_scale_about(&v, m, mad), 0.01, 0.03));
        for est in [ScaleEstimator::Bwmv, ScaleEstimator::Mad, ScaleEstimator::AvgDev] {
            let ls = location_scale(&v, est).unwrap();
            assert!(close(ls.location, 0.5, 0.001), "{est:?} location {}", ls.location);
            assert!(close(ls.scale, 0.01, 0.03), "{est:?} scale {}", ls.scale);
        }
        assert!(location_scale(&[0.1, 0.2, 0.3], ScaleEstimator::Mad).is_none());
    }

    #[test]
    fn robust_scales_ignore_symmetric_outliers() {
        let mut v = gaussian(20_000, 0.01, 2);
        for i in 0..200 {
            v[i * 100] = 0.5 + 0.2; // 1 % at +20σ
            v[i * 100 + 50] = 0.5 - 0.2; // 1 % at −20σ
        }
        let m = median_of(&v);
        let mad = mad_about(&v, m);
        assert!(close(mad * MAD_TO_SIGMA, 0.01, 0.05));
        assert!(close(bwmv_scale_about(&v, m, mad), 0.01, 0.05));
        let sd = stddev_about_mean(&v) as f32;
        assert!(sd > 0.025, "plain stddev must be blown up by the outliers: {sd}");
        assert!(close(location_scale(&v, ScaleEstimator::Bwmv).unwrap().scale, 0.01, 0.05));
        assert!(close(location_scale(&v, ScaleEstimator::Mad).unwrap().scale, 0.01, 0.05));
    }

    #[test]
    fn two_sided_equals_one_sided_on_symmetric_data() {
        let v = gaussian(20_000, 0.02, 3);
        let m = median_of(&v);
        let one = mad_about(&v, m) * MAD_TO_SIGMA;
        let two = location_scale(&v, ScaleEstimator::Mad).unwrap().scale;
        assert!(close(two, one, 0.03));
    }

    #[test]
    fn clip_drops_zeros_saturation_and_nan() {
        let v = [0.0, 0.5, 1.0, f32::NAN, 0.25];
        assert_eq!(clip_sample(&v, CLIP_LO, CLIP_HI), vec![0.5, 0.25]);
    }

    #[test]
    fn stratified_sample_counts() {
        let d = vec![1.0f32; 16 * 16];
        assert_eq!(stratified_sample(&d, 16, 16).len(), 16);
        let d = vec![1.0f32; 17 * 9];
        assert_eq!(stratified_sample(&d, 17, 9).len(), 13);
        let mut d = vec![1.0f32; 8 * 8];
        d[0] = f32::NAN;
        assert_eq!(stratified_sample(&d, 8, 8).len(), 3);
        let mut seen = Vec::new();
        for_each_stratified(8, 8, |i| seen.push(i));
        assert_eq!(seen, vec![0, 4, 33, 37]);
    }

    #[test]
    fn noise_scale_factors_are_symmetric_on_gaussian_noise() {
        let v = gaussian(50_000, 0.01, 4);
        let (lo, hi) = noise_scale_factors(&v).unwrap();
        assert!(lo > 0.0046 && lo < 0.0062, "{lo}");
        assert!(hi > 0.0046 && hi < 0.0062, "{hi}");
        assert!((lo - hi).abs() < 0.0005);
        assert!(noise_scale_factors(&[0.5; 4]).is_none());
    }

    #[test]
    fn normalization_pairs_by_hand() {
        let r = LocationScale { location: 0.10, scale: 0.02 };
        let f = LocationScale { location: 0.15, scale: 0.04 };
        let p = output_pair(r, f, OutputNormalization::AdditiveWithScaling);
        assert!((p.scale - 0.5).abs() < 1e-6 && (p.offset - 0.025).abs() < 1e-6);
        assert!((p.apply(0.15) - 0.10).abs() < 1e-6);
        let p = output_pair(r, f, OutputNormalization::Additive);
        assert_eq!(p.scale, 1.0);
        assert!((p.offset + 0.05).abs() < 1e-6);
        let p = output_pair(r, f, OutputNormalization::Multiplicative);
        assert!((p.scale - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(p.offset, 0.0);
        let p = output_pair(r, f, OutputNormalization::MultiplicativeWithScaling);
        assert!((p.scale - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(p.offset, 0.0);
        assert_eq!(output_pair(r, f, OutputNormalization::None), NormalizationPair::IDENTITY);
        assert_eq!(
            rejection_pair(r, f, RejectionNormalization::ScaleZeroOffset),
            Some(output_pair(r, f, OutputNormalization::AdditiveWithScaling))
        );
        assert_eq!(
            rejection_pair(r, f, RejectionNormalization::EqualizeFluxes),
            Some(output_pair(r, f, OutputNormalization::Multiplicative))
        );
        assert_eq!(rejection_pair(r, f, RejectionNormalization::None), Some(NormalizationPair::IDENTITY));
        assert_eq!(rejection_pair(r, f, RejectionNormalization::Local), None);
        let zero = LocationScale { location: 0.0, scale: 0.0 };
        assert_eq!(output_pair(r, zero, OutputNormalization::AdditiveWithScaling).scale, 1.0);
    }

    #[test]
    fn serde_names_match_the_spec() {
        assert_eq!(serde_json::to_string(&ScaleEstimator::AvgDev).unwrap(), "\"avgDev\"");
        assert_eq!(serde_json::to_string(&ScaleEstimator::Bwmv).unwrap(), "\"bwmv\"");
        assert_eq!(
            serde_json::to_string(&OutputNormalization::AdditiveWithScaling).unwrap(),
            "\"additiveWithScaling\""
        );
        assert_eq!(
            serde_json::to_string(&RejectionNormalization::ScaleZeroOffset).unwrap(),
            "\"scaleZeroOffset\""
        );
        let back: OutputNormalization = serde_json::from_str("\"multiplicativeWithScaling\"").unwrap();
        assert_eq!(back, OutputNormalization::MultiplicativeWithScaling);
        assert_eq!(ScaleEstimator::default(), ScaleEstimator::Bwmv);
        assert_eq!(OutputNormalization::default(), OutputNormalization::AdditiveWithScaling);
        assert_eq!(RejectionNormalization::default(), RejectionNormalization::ScaleZeroOffset);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Add `pub mod stats;` to `integration/mod.rs`, then run `cargo test -p athenaeum-core integration::stats` → compile errors (nothing defined).

- [ ] **Step 4: Implement** — the top of `stats.rs`, above the tests:

```rust
//! Robust location and scale statistics for frame normalization and
//! weighting (math reference §2.4–2.5, spec §5.1): median / MAD / average
//! deviation / biweight midvariance evaluated two-sided about the median,
//! the 1/16 stratified pixel sample every caller shares, the noise-scale
//! factors of the classic SNR weight, and the per-frame `(scale, offset)`
//! pairs the integration engine applies. Pure functions over `f32` samples.

use serde::{Deserialize, Serialize};

/// σ-consistency factor for the median absolute deviation.
pub const MAD_TO_SIGMA: f32 = 1.4826;
/// σ-consistency factor for the average absolute deviation (√(π/2)).
pub const AVGDEV_TO_SIGMA: f32 = 1.2533;
/// σ-consistency factor applied to √BWMV.
pub const BWMV_TO_SIGMA: f32 = 0.991;
/// Samples outside `(CLIP_LO, CLIP_HI)` are excluded from scale estimates:
/// zeros mark missing coverage, the top value marks saturation.
pub const CLIP_LO: f32 = 1.0 / 65535.0;
pub const CLIP_HI: f32 = 1.0 - 1.0 / 65535.0;
/// The tighter clip the noise-scale factors use (§2.5).
pub const NOISE_CLIP_LO: f32 = 2.0 / 65535.0;
pub const NOISE_CLIP_HI: f32 = 1.0 - 2.0 / 65535.0;
/// Stride of the stratified sample: one pixel in `STRIDE²` = 1/16.
pub const SAMPLE_STRIDE: usize = 4;

/// Median with the even-`n` convention "mean of the central two". Reorders
/// `values`; the caller has removed NaNs. NaN for an empty slice.
pub fn median_in_place(values: &mut [f32]) -> f32 {
    let n = values.len();
    if n == 0 {
        return f32::NAN;
    }
    let mid = n / 2;
    let (_, hi, _) = values.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let hi = *hi;
    if n % 2 == 1 {
        hi
    } else {
        let lo = values[..mid]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        0.5 * (lo + hi)
    }
}

pub fn median_of(values: &[f32]) -> f32 {
    let mut v = values.to_vec();
    median_in_place(&mut v)
}

/// Raw median absolute deviation about `m` (no consistency factor).
pub fn mad_about(values: &[f32], m: f32) -> f32 {
    let mut d: Vec<f32> = values.iter().map(|&x| (x - m).abs()).collect();
    median_in_place(&mut d)
}

/// Mean absolute deviation about `m`.
pub fn avg_dev_about(values: &[f32], m: f32) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    let s: f64 = values.iter().map(|&x| (x - m).abs() as f64).sum();
    (s / values.len() as f64) as f32
}

/// σ-consistent biweight midvariance scale about `m` (Wilcox 2012
/// §3.12.1): `u = (x − m)/(9·MAD)`, `BWMV = n·Σ_{|u|<1}(x − m)²(1 − u²)⁴ /
/// [Σ_{|u|<1}(1 − u²)(1 − 5u²)]²`, scale `= √BWMV · 0.991`. `mad` is the
/// raw MAD about `m`. Falls back to the MAD scale when no sample lies in
/// the biweight window or the denominator is not positive.
pub fn bwmv_scale_about(values: &[f32], m: f32, mad: f32) -> f32 {
    let n = values.len();
    if n == 0 {
        return f32::NAN;
    }
    if !(mad > 0.0) {
        return 0.0;
    }
    let w = 9.0 * mad as f64;
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for &x in values {
        let d = (x - m) as f64;
        let u2 = (d / w) * (d / w);
        if u2 < 1.0 {
            let a = 1.0 - u2;
            num += d * d * a.powi(4);
            den += a * (1.0 - 5.0 * u2);
        }
    }
    if den <= 0.0 || num <= 0.0 {
        return mad * MAD_TO_SIGMA;
    }
    let bwmv = n as f64 * num / (den * den);
    (bwmv.sqrt() as f32) * BWMV_TO_SIGMA
}

fn stddev_about_mean(v: &[f32]) -> f64 {
    let n = v.len();
    if n < 2 {
        return 0.0;
    }
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n as f64;
    let ss = v
        .iter()
        .map(|&x| {
            let d = x as f64 - mean;
            d * d
        })
        .sum::<f64>();
    (ss / (n - 1) as f64).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ScaleEstimator {
    #[default]
    Bwmv,
    Mad,
    AvgDev,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationScale {
    pub location: f32,
    pub scale: f32,
}

/// Keep the finite samples strictly inside `(lo, hi)`.
pub fn clip_sample(values: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > lo && *v < hi)
        .collect()
}

fn side_scale(side: &[f32], m: f32, est: ScaleEstimator) -> Option<f32> {
    if side.len() < 2 {
        return None;
    }
    Some(match est {
        ScaleEstimator::Mad => mad_about(side, m) * MAD_TO_SIGMA,
        ScaleEstimator::AvgDev => avg_dev_about(side, m) * AVGDEV_TO_SIGMA,
        ScaleEstimator::Bwmv => bwmv_scale_about(side, m, mad_about(side, m)),
    })
}

/// Location (median) and two-sided scale of an already clipped sample: the
/// estimator runs separately on `x ≤ m` and `x > m` and the two are
/// averaged; an empty side defers to the other. `None` below 4 samples.
pub fn location_scale(values: &[f32], est: ScaleEstimator) -> Option<LocationScale> {
    if values.len() < 4 {
        return None;
    }
    let m = median_of(values);
    let (low, high): (Vec<f32>, Vec<f32>) = values.iter().partition(|&&x| x <= m);
    let scale = match (side_scale(&low, m, est), side_scale(&high, m, est)) {
        (Some(a), Some(b)) => 0.5 * (a + b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return None,
    };
    Some(LocationScale { location: m, scale })
}

/// Visit the 1/16 stratified pixel indices: rows `0, 4, 8, …`; in row `y`
/// the columns start at `(y / 4) % 4` and step by 4, so the four row phases
/// cover all four column phases.
pub fn for_each_stratified(width: usize, height: usize, mut f: impl FnMut(usize)) {
    let mut y = 0;
    while y < height {
        let mut x = (y / SAMPLE_STRIDE) % SAMPLE_STRIDE;
        while x < width {
            f(y * width + x);
            x += SAMPLE_STRIDE;
        }
        y += SAMPLE_STRIDE;
    }
}

/// The finite values at the stratified indices of a plane.
pub fn stratified_sample(data: &[f32], width: usize, height: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(data.len() / (SAMPLE_STRIDE * SAMPLE_STRIDE) + 1);
    for_each_stratified(width, height, |i| {
        let v = data[i];
        if v.is_finite() {
            out.push(v);
        }
    });
    out
}

/// Noise scaling factors (math reference §2.5): `(σ_low, σ_high)` — the
/// standard deviation of each side of the clipped sample after two
/// restrictions to `[c − 4σ, c]` / `[c, c + 4σ]` about the median `c`.
/// `None` below 8 samples or when a side empties.
pub fn noise_scale_factors(values: &[f32]) -> Option<(f32, f32)> {
    let v = clip_sample(values, NOISE_CLIP_LO, NOISE_CLIP_HI);
    if v.len() < 8 {
        return None;
    }
    let c = median_of(&v);
    let mut low: Vec<f32> = v.iter().copied().filter(|&x| x <= c).collect();
    let mut high: Vec<f32> = v.iter().copied().filter(|&x| x >= c).collect();
    for _ in 0..2 {
        let lo = (c as f64 - 4.0 * stddev_about_mean(&low)).max(NOISE_CLIP_LO as f64) as f32;
        low.retain(|&x| x >= lo);
        let hi = (c as f64 + 4.0 * stddev_about_mean(&high)).min(NOISE_CLIP_HI as f64) as f32;
        high.retain(|&x| x <= hi);
    }
    if low.len() < 2 || high.len() < 2 {
        return None;
    }
    Some((stddev_about_mean(&low) as f32, stddev_about_mean(&high) as f32))
}

/// Output normalization modes (spec §5.1, math reference §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum OutputNormalization {
    None,
    Additive,
    #[default]
    AdditiveWithScaling,
    Multiplicative,
    MultiplicativeWithScaling,
}

/// Rejection normalization modes (math reference §3.3); `Local` carries
/// per-frame grids instead of a pair (M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RejectionNormalization {
    None,
    #[default]
    ScaleZeroOffset,
    EqualizeFluxes,
    Local,
}

/// `v′ = v · scale + offset`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizationPair {
    pub scale: f32,
    pub offset: f32,
}

impl NormalizationPair {
    pub const IDENTITY: NormalizationPair = NormalizationPair { scale: 1.0, offset: 0.0 };

    #[inline]
    pub fn apply(&self, v: f32) -> f32 {
        v * self.scale + self.offset
    }
}

fn safe_ratio(num: f32, den: f32) -> f32 {
    if den != 0.0 && num.is_finite() && den.is_finite() {
        num / den
    } else {
        1.0
    }
}

/// The pair that maps a frame with statistics `frame` onto `reference`
/// (`m` = location, `s` = scale, index 0 = reference, i = frame):
/// additive `x + (m₀ − mᵢ)`; additive with scaling `(x − mᵢ)·(s₀/sᵢ) + m₀`;
/// multiplicative `x·m₀/mᵢ`; multiplicative with scaling `(x/mᵢ)·(s₀/sᵢ)·m₀`.
pub fn output_pair(
    reference: LocationScale,
    frame: LocationScale,
    mode: OutputNormalization,
) -> NormalizationPair {
    let (m0, s0, mi, si) = (reference.location, reference.scale, frame.location, frame.scale);
    match mode {
        OutputNormalization::None => NormalizationPair::IDENTITY,
        OutputNormalization::Additive => NormalizationPair { scale: 1.0, offset: m0 - mi },
        OutputNormalization::AdditiveWithScaling => {
            let k = safe_ratio(s0, si);
            NormalizationPair { scale: k, offset: m0 - mi * k }
        }
        OutputNormalization::Multiplicative => NormalizationPair { scale: safe_ratio(m0, mi), offset: 0.0 },
        OutputNormalization::MultiplicativeWithScaling => NormalizationPair {
            scale: safe_ratio(s0, si) * safe_ratio(m0, mi),
            offset: 0.0,
        },
    }
}

/// The pair applied to the working copy before rejection; `None` for
/// `Local`, whose grids the caller supplies.
pub fn rejection_pair(
    reference: LocationScale,
    frame: LocationScale,
    mode: RejectionNormalization,
) -> Option<NormalizationPair> {
    Some(match mode {
        RejectionNormalization::None => NormalizationPair::IDENTITY,
        RejectionNormalization::ScaleZeroOffset => {
            output_pair(reference, frame, OutputNormalization::AdditiveWithScaling)
        }
        RejectionNormalization::EqualizeFluxes => {
            output_pair(reference, frame, OutputNormalization::Multiplicative)
        }
        RejectionNormalization::Local => return None,
    })
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core integration::stats` → 10 pass.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `rustfmt crates/athenaeum-core/src/integration/stats.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/integration/stats.rs crates/athenaeum-core/src/integration/mod.rs crates/athenaeum-core/src/test_support.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(integration): robust location/scale statistics and normalization pairs" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 3: `stacking/robust.rs` — erf family, Robust Chauvenet Rejection, Winsorization

**Files:**
- Create: `crates/athenaeum-core/src/stacking/mod.rs`, `crates/athenaeum-core/src/stacking/robust.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` — after the two lines `#[cfg(all(feature = "render", feature = "solver"))] pub mod registration;` add:

```rust
// stacking builds on astroimage (measurement) and, from Plan 3, registration.
#[cfg(all(feature = "render", feature = "solver"))]
pub mod stacking;
```

**Interfaces:**
- Consumes: `crate::test_support::add_noise` (tests).
- Produces (`crate::stacking::robust`): `fn erf(x: f64) -> f64`, `fn erfc(x: f64) -> f64`, `fn erfinv(x: f64) -> f64`, `fn gauss_tail(z: f64) -> f64` (= ½ erfc(z/√2)), `fn small_sample_factor(n: usize) -> f64`, `pub(crate) fn quantile_sorted(sorted: &[f64], p: f64) -> f64`, `pub(crate) fn sample_deviation(devs_sorted: &[f64]) -> f64`, `pub(crate) fn line_fit_deviation(devs_sorted: &[f64]) -> f64`, `struct RcrResult { location: f64, scale: f64, kept: Vec<bool>, rejected: usize }`, `fn rcr(values: &[f64], limit: f64) -> RcrResult`, `fn winsorize(values: &[f64], kept: &[bool]) -> Vec<f64>`.

- [ ] **Step 1: Create `stacking/mod.rs`**

```rust
//! Stacking pipeline (spec `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`).
//! Plan 2 lands the measurement stage: robust rejection (`robust`), the
//! PSF-signal estimators (`psf_signal`), per-frame measurement (`measure`)
//! and weighting / selection (`weights`). Orchestration, configuration and
//! persistence follow in later plans.

pub mod robust;
```

(Tasks 4, 6 and 7 add `pub mod psf_signal;`, `pub mod measure;`, `pub mod weights;` — keep the list alphabetical.)

- [ ] **Step 2: Write the failing tests** — `robust.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn gaussian(n: usize, seed: u64) -> Vec<f64> {
        let mut v = vec![0.0f32; n];
        crate::test_support::add_noise(&mut v, 1.0, seed);
        v.iter().map(|&x| x as f64).collect()
    }

    #[test]
    fn erf_family_reference_values() {
        assert!((erf(0.5) - 0.5204998778).abs() < 2e-7);
        assert!((erf(1.0) - 0.8427007929).abs() < 2e-7);
        assert!((erf(2.0) - 0.9953222650).abs() < 2e-7);
        assert!((erfc(3.0) - 2.2090497e-5).abs() < 3e-7);
        assert_eq!(erf(0.0), 0.0);
        assert!((erf(-1.0) + erf(1.0)).abs() < 1e-12);
        assert!((erfc(-1.0) - (2.0 - erfc(1.0))).abs() < 1e-12);
        assert!((gauss_tail(1.959964) - 0.025).abs() < 1e-6);
    }

    #[test]
    fn erfinv_inverts_erf() {
        assert_eq!(erfinv(0.0), 0.0);
        assert!((erfinv(0.5) - 0.4769362762).abs() < 1e-5);
        for &x in &[-0.999, -0.99, -0.9, -0.5, -0.1, 0.1, 0.3, 0.7, 0.9, 0.99, 0.999] {
            assert!((erf(erfinv(x)) - x).abs() < 1e-5, "x = {x}");
        }
        assert_eq!(erfinv(1.0), f64::INFINITY);
        assert_eq!(erfinv(-1.0), f64::NEG_INFINITY);
    }

    #[test]
    fn small_sample_factor_values() {
        assert!((small_sample_factor(100) - 1.0215).abs() < 1e-3);
        assert!((small_sample_factor(1000) - 1.0018).abs() < 5e-4);
        assert!(small_sample_factor(3) > 5.0);
        assert_eq!(small_sample_factor(2), 20.0);
    }

    #[test]
    fn quantile_convention() {
        let s = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(quantile_sorted(&s, 0.0), 1.0);
        assert_eq!(quantile_sorted(&s, 1.0), 4.0);
        assert!((quantile_sorted(&s, 0.5) - 2.5).abs() < 1e-12);
        assert!((quantile_sorted(&s, 1.0 / 3.0) - 2.0).abs() < 1e-12);
        assert!(quantile_sorted(&[], 0.5).is_nan());
    }

    #[test]
    fn deviations_estimate_sigma() {
        let v = gaussian(4000, 11);
        let mut devs: Vec<f64> = v.iter().map(|x| x.abs()).collect();
        devs.sort_by(|a, b| a.total_cmp(b));
        assert!((sample_deviation(&devs) - 1.0).abs() < 0.1, "{}", sample_deviation(&devs));
        assert!((line_fit_deviation(&devs) - 1.0).abs() < 0.1, "{}", line_fit_deviation(&devs));
        // fewer than 8 regression points → the sample deviation
        let short: Vec<f64> = devs[..10].to_vec();
        assert_eq!(line_fit_deviation(&short), sample_deviation(&short));
    }

    #[test]
    fn rcr_keeps_a_clean_gaussian_sample() {
        let v = gaussian(500, 12);
        let r = rcr(&v, 0.5);
        assert!(r.rejected <= 25, "{}", r.rejected);
        assert!(r.location.abs() < 0.15, "{}", r.location);
        assert!((r.scale - 1.0).abs() < 0.15, "{}", r.scale);
        assert_eq!(r.kept.len(), 500);
    }

    #[test]
    fn rcr_rejects_gross_outliers_and_keeps_the_bulk() {
        let mut v = gaussian(200, 13);
        v.extend(std::iter::repeat(10.0).take(20));
        v.extend(std::iter::repeat(-8.0).take(5));
        let r = rcr(&v, 0.5);
        for i in 200..225 {
            assert!(!r.kept[i], "outlier {i} survived");
        }
        let bulk_rejected = r.kept[..200].iter().filter(|k| !**k).count();
        assert!(bulk_rejected <= 10, "{bulk_rejected}");
        assert_eq!(r.rejected, 25 + bulk_rejected);
        assert!(r.location.abs() < 0.2, "{}", r.location);
        assert!((r.scale - 1.0).abs() < 0.2, "{}", r.scale);
    }

    #[test]
    fn rcr_leaves_tiny_samples_alone() {
        let r = rcr(&[1.0, 2.0], 0.5);
        assert_eq!(r.rejected, 0);
        assert_eq!(r.kept, vec![true, true]);
        assert_eq!(r.location, 1.5);
        assert_eq!(rcr(&[], 0.5).rejected, 0);
        // identical values: σ = 0, nothing rejected
        let r = rcr(&[3.0; 12], 0.5);
        assert_eq!(r.rejected, 0);
    }

    #[test]
    fn winsorize_replaces_rejects_with_the_nearest_survivor_extreme() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0, 100.0, -50.0];
        let kept = [true, true, true, true, true, false, false];
        assert_eq!(winsorize(&v, &kept), vec![1.0, 2.0, 3.0, 4.0, 5.0, 5.0, 1.0]);
        assert_eq!(winsorize(&v, &[false; 7]), v.to_vec());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p athenaeum-core stacking::robust` → compile errors.

- [ ] **Step 4: Implement** — above the tests in `robust.rs`:

```rust
//! Robust Chauvenet Rejection (Maples et al. 2018, ApJS 238, 2; math
//! reference §3.4) for one-dimensional samples, the Winsorization that
//! follows it in the PSF-signal estimator (§1.1), and the error-function
//! family they need. Pure `f64` math, no I/O.

/// erf via Abramowitz & Stegun 7.1.26 (|error| ≤ 1.5e-7).
pub fn erf(x: f64) -> f64 {
    if x < 0.0 {
        -erf(-x)
    } else {
        1.0 - erfc_pos(x)
    }
}

/// Complementary error function, computed without cancellation in the tail.
pub fn erfc(x: f64) -> f64 {
    if x < 0.0 {
        2.0 - erfc_pos(-x)
    } else {
        erfc_pos(x)
    }
}

fn erfc_pos(x: f64) -> f64 {
    const P: f64 = 0.3275911;
    const A: [f64; 5] = [0.254829592, -0.284496736, 1.421413741, -1.453152027, 1.061405429];
    let t = 1.0 / (1.0 + P * x);
    let poly = t * (A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4]))));
    poly * (-x * x).exp()
}

/// Inverse error function (Giles 2010, the single-precision coefficient
/// set evaluated in f64: relative error ≈ 1e-6 on (−1, 1)).
pub fn erfinv(x: f64) -> f64 {
    if x <= -1.0 {
        return f64::NEG_INFINITY;
    }
    if x >= 1.0 {
        return f64::INFINITY;
    }
    let w = -((1.0 - x) * (1.0 + x)).ln();
    let p = if w < 5.0 {
        let w = w - 2.5;
        let mut p = 2.81022636e-08;
        p = 3.43273939e-07 + p * w;
        p = -3.5233877e-06 + p * w;
        p = -4.39150654e-06 + p * w;
        p = 0.00021858087 + p * w;
        p = -0.00125372503 + p * w;
        p = -0.00417768164 + p * w;
        p = 0.246640727 + p * w;
        1.50140941 + p * w
    } else {
        let w = w.sqrt() - 3.0;
        let mut p = -0.000200214257;
        p = 0.000100950558 + p * w;
        p = 0.00134934322 + p * w;
        p = -0.00367342844 + p * w;
        p = 0.00573950773 + p * w;
        p = -0.0076224613 + p * w;
        p = 0.00943887047 + p * w;
        p = 1.00167406 + p * w;
        2.83297682 + p * w
    };
    p * x
}

/// Upper Gaussian tail `Q(z) = ½·erfc(z/√2)`.
pub fn gauss_tail(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

/// Small-sample correction `F(N) = 1 / (1 − 2.9442·N^{−1.073})`, capped at
/// 20 where the denominator is not usefully positive (N ≤ 2).
pub fn small_sample_factor(n: usize) -> f64 {
    let d = 1.0 - 2.9442 * (n as f64).powf(-1.073);
    if d <= 0.05 {
        20.0
    } else {
        1.0 / d
    }
}

/// Linear-interpolation quantile of a sorted slice (position `p·(n − 1)`).
pub(crate) fn quantile_sorted(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    let pos = p * (n - 1) as f64;
    let i = pos.floor() as usize;
    let f = pos - i as f64;
    if i + 1 >= n {
        sorted[n - 1]
    } else {
        sorted[i] + f * (sorted[i + 1] - sorted[i])
    }
}

/// `F(N) · quantile_{0.683}(|x − μ|)` over the sorted deviations.
pub(crate) fn sample_deviation(devs_sorted: &[f64]) -> f64 {
    small_sample_factor(devs_sorted.len()) * quantile_sorted(devs_sorted, 0.683)
}

/// Regress the lowest `trunc(0.683N + 0.317)` sorted deviations against
/// the half-normal quantiles `√2·erfinv((i + 1 − 0.317)/N)` with a line
/// through the origin and return `F(N)·ŷ(1)`; below 8 regression points
/// defers to [`sample_deviation`].
pub(crate) fn line_fit_deviation(devs_sorted: &[f64]) -> f64 {
    let n = devs_sorted.len();
    let m = (0.683 * n as f64 + 0.317).trunc() as usize;
    if m < 8 {
        return sample_deviation(devs_sorted);
    }
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, &y) in devs_sorted.iter().take(m).enumerate() {
        let x = std::f64::consts::SQRT_2 * erfinv((i as f64 + 1.0 - 0.317) / n as f64);
        sxy += x * y;
        sxx += x * x;
    }
    if sxx <= 0.0 {
        return sample_deviation(devs_sorted);
    }
    small_sample_factor(n) * (sxy / sxx)
}

fn median_f64(v: &mut Vec<f64>) -> f64 {
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn stddev(v: &[f64], mu: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    (v.iter().map(|x| (x - mu) * (x - mu)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

#[derive(Debug, Clone, PartialEq)]
pub struct RcrResult {
    /// Centre from the last evaluated phase (the mean once phase 2 ran).
    pub location: f64,
    /// Dispersion from the last evaluated phase.
    pub scale: f64,
    pub kept: Vec<bool>,
    pub rejected: usize,
}

/// Bulk-mode RCR: three phases of decreasing robustness (median + line-fit
/// deviation, median + sample deviation, mean + standard deviation), each
/// iterated: while `n·Q(|extreme − μ|/σ) < limit` reject the single most
/// extreme value (ties go to the high side). `limit = 0.5` is Chauvenet's
/// criterion. Below 3 values nothing is rejected.
pub fn rcr(values: &[f64], limit: f64) -> RcrResult {
    let n = values.len();
    let mut kept = vec![true; n];
    if n < 3 {
        let mut v = values.to_vec();
        return RcrResult { location: median_f64(&mut v), scale: 0.0, kept, rejected: 0 };
    }
    let (mut location, mut scale) = (f64::NAN, f64::NAN);
    for phase in 0..3 {
        loop {
            let idx: Vec<usize> = (0..n).filter(|&i| kept[i]).collect();
            let m = idx.len();
            if m < 3 {
                break;
            }
            let cur: Vec<f64> = idx.iter().map(|&i| values[i]).collect();
            let (mu, sigma) = if phase < 2 {
                let mut c = cur.clone();
                let med = median_f64(&mut c);
                let mut devs: Vec<f64> = cur.iter().map(|x| (x - med).abs()).collect();
                devs.sort_by(|a, b| a.total_cmp(b));
                let s = if phase == 0 { line_fit_deviation(&devs) } else { sample_deviation(&devs) };
                (med, s)
            } else {
                let mu = mean(&cur);
                (mu, stddev(&cur, mu))
            };
            location = mu;
            scale = sigma;
            if !(sigma > 0.0) {
                break;
            }
            let (mut imin, mut imax) = (idx[0], idx[0]);
            for &i in &idx {
                if values[i] < values[imin] {
                    imin = i;
                }
                if values[i] > values[imax] {
                    imax = i;
                }
            }
            let d_lo = m as f64 * gauss_tail((mu - values[imin]) / sigma);
            let d_hi = m as f64 * gauss_tail((values[imax] - mu) / sigma);
            if d_lo.min(d_hi) < limit {
                if d_hi <= d_lo {
                    kept[imax] = false;
                } else {
                    kept[imin] = false;
                }
            } else {
                break;
            }
        }
    }
    let rejected = kept.iter().filter(|k| !**k).count();
    RcrResult { location, scale, kept, rejected }
}

/// Replace every rejected value by the nearest survivor extreme: below the
/// survivors' minimum → that minimum, above their maximum → that maximum.
/// Order is preserved; nothing is dropped. With no survivors the input is
/// returned unchanged.
pub fn winsorize(values: &[f64], kept: &[bool]) -> Vec<f64> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (v, k) in values.iter().zip(kept) {
        if *k {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
    }
    if !lo.is_finite() {
        return values.to_vec();
    }
    values
        .iter()
        .zip(kept)
        .map(|(&v, &k)| {
            if k {
                v
            } else if v < lo {
                lo
            } else if v > hi {
                hi
            } else {
                v
            }
        })
        .collect()
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core stacking::robust` → 9 pass.
Run: `cargo check -p athenaeum-core --no-default-features` → clean (the module is gated off).
Run: `rustfmt crates/athenaeum-core/src/stacking/robust.rs crates/athenaeum-core/src/stacking/mod.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/stacking crates/athenaeum-core/src/lib.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): robust Chauvenet rejection, Winsorization and the erf family" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 4: `stacking/psf_signal.rs` — Moffat fits on seeds, FWTM aperture flux

**Files:**
- Create: `crates/athenaeum-core/src/stacking/psf_signal.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (add `pub mod psf_signal;`)
- Modify: `crates/athenaeum-core/src/test_support.rs` (append `MoffatStar`, `moffat_field`)

**Interfaces:**
- Consumes: `astroimage::analysis::fitting::{fit_moffat_2d_fixed_beta, Moffat2DResult, PixelSample}` — signature `fit_moffat_2d_fixed_beta(pixels: &[PixelSample], init_b, init_a, init_x0, init_y0, init_sigma_x, init_sigma_y, init_theta, beta, max_iter: usize, conv_tol: f64, max_rejects: usize) -> Option<Moffat2DResult { b, a, x0, y0, alpha_x, alpha_y, theta, beta, converged, fit_residual }>`, with `fwhm_x()`/`fwhm_y()` = `2α√(2^{1/β} − 1)`; the fitter converts `init_sigma` (a Gaussian σ) to its α by matching FWHM, and its rotation is `u = dx·cosθ + dy·sinθ`, `v = −dx·sinθ + dy·cosθ`. `crate::integration::stats::median_in_place`. `rayon::prelude::*`.
- Produces (`crate::stacking::psf_signal`): `enum PsfModel { Auto, Moffat4 }` (serde `auto|moffat4`, `Default = Auto`), `const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0]`, `const AUTO_SAMPLE: usize = 64`, `struct Seed { x, y, peak, flux: f64 }` (`Copy`), `struct FitParams { centroid_tolerance_px: f64 (1.5), growth: f64 (1.0), max_iter: usize (50), conv_tol: f64 (1e-5), max_rejects: usize (5) }` (`Default`), `struct StarFit { x, y, background, amplitude, fwhm_x, fwhm_y, fwtm_x, fwtm_y, theta, beta, residual, signal, area: f64 }` (serde camelCase) with `mean_flux()`, `fwhm()`, `eccentricity()`, `struct FitOutcome { fits: Vec<StarFit>, beta: f64, seeds: usize }`, `fn fwtm_from_alpha(alpha: f64, beta: f64) -> f64`, `fn fit_stars(data: &[f32], w: usize, h: usize, seeds: &[Seed], model: PsfModel, p: &FitParams) -> FitOutcome`, `pub(crate) fn aperture(data, w, h, f: &mut StarFit, k: f64)`.
- Test support: `pub(crate) struct MoffatStar { x, y, amp, alpha_x, alpha_y, theta: f64 }`, `pub(crate) fn moffat_field(w, h, stars: &[MoffatStar], beta: f64, background: f32) -> Vec<f32>`.

- [ ] **Step 1: Add the fixture** — append to `test_support.rs`:

```rust
/// One elliptical Moffat star for [`moffat_field`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct MoffatStar {
    pub x: f64,
    pub y: f64,
    pub amp: f64,
    pub alpha_x: f64,
    pub alpha_y: f64,
    pub theta: f64,
}

/// Row-major float plane of elliptical Moffat stars
/// `A·(1 + (u/αx)² + (v/αy)²)^(−β)` with `u = dx·cosθ + dy·sinθ`,
/// `v = −dx·sinθ + dy·cosθ`, rendered out to `12·max(αx, αy)` on a flat
/// `background`. Pixel centres at integer coordinates. The total flux of
/// one star is `π·A·αx·αy/(β − 1)`; the flux inside its FWTM ellipse is
/// that times `1 − 10^{1/β − 1}`.
pub(crate) fn moffat_field(
    w: usize,
    h: usize,
    stars: &[MoffatStar],
    beta: f64,
    background: f32,
) -> Vec<f32> {
    let mut data = vec![background; w * h];
    for s in stars {
        let r = (12.0 * s.alpha_x.max(s.alpha_y)).ceil() as i64;
        let (cx, cy) = (s.x.round() as i64, s.y.round() as i64);
        let (st, ct) = s.theta.sin_cos();
        for y in (cy - r).max(0)..=(cy + r).min(h as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(w as i64 - 1) {
                let (dx, dy) = (x as f64 - s.x, y as f64 - s.y);
                let u = dx * ct + dy * st;
                let v = -dx * st + dy * ct;
                let q = (u / s.alpha_x).powi(2) + (v / s.alpha_y).powi(2);
                data[y as usize * w + x as usize] += (s.amp * (1.0 + q).powf(-beta)) as f32;
            }
        }
    }
    data
}
```

- [ ] **Step 2: Write the failing tests** — `psf_signal.rs` test module (the totals/estimator tests are added in Task 5):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{gaussian_field, moffat_field, MoffatStar};

    fn round(x: f64, y: f64, amp: f64) -> MoffatStar {
        MoffatStar { x, y, amp, alpha_x: 5.0, alpha_y: 5.0, theta: 0.0 }
    }
    fn seeds_from(stars: &[MoffatStar], beta: f64) -> Vec<Seed> {
        stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: std::f64::consts::PI * s.amp * s.alpha_x * s.alpha_y / (beta - 1.0),
            })
            .collect()
    }
    fn nearest<'a>(fits: &'a [StarFit], x: f64, y: f64) -> &'a StarFit {
        fits.iter()
            .min_by(|a, b| {
                ((a.x - x).abs() + (a.y - y).abs()).total_cmp(&((b.x - x).abs() + (b.y - y).abs()))
            })
            .unwrap()
    }

    #[test]
    fn fixed_beta_fit_recovers_centre_width_background_and_aperture_signal() {
        let stars = [round(50.3, 60.7, 0.5), round(140.2, 130.9, 0.3), round(30.1, 150.4, 0.1)];
        let data = moffat_field(200, 200, &stars, 4.0, 0.05);
        let out = fit_stars(&data, 200, 200, &seeds_from(&stars, 4.0), PsfModel::Moffat4, &FitParams::default());
        assert_eq!(out.beta, 4.0);
        assert_eq!(out.seeds, 3);
        assert_eq!(out.fits.len(), 3);
        let fwhm = 2.0 * 5.0 * (2f64.powf(0.25) - 1.0).sqrt(); // 4.3498
        let fwtm = 2.0 * 5.0 * (10f64.powf(0.25) - 1.0).sqrt(); // 8.8220
        assert!((fwtm_from_alpha(5.0, 4.0) - fwtm).abs() < 1e-12);
        for s in &stars {
            let f = nearest(&out.fits, s.x, s.y);
            assert!((f.x - s.x).abs() < 0.05 && (f.y - s.y).abs() < 0.05, "centre {:?}", (f.x, f.y));
            assert!((f.fwhm_x - fwhm).abs() < 0.03 * fwhm && (f.fwhm_y - fwhm).abs() < 0.03 * fwhm);
            assert!((f.fwtm_x - fwtm).abs() < 0.03 * fwtm);
            assert!((f.background - 0.05).abs() < 0.002, "background {}", f.background);
            let total = std::f64::consts::PI * s.amp * 25.0 / 3.0;
            let expected = total * (1.0 - 10f64.powf(0.25 - 1.0)); // 0.8222·total
            assert!((f.signal - expected).abs() < 0.06 * expected, "signal {} vs {expected}", f.signal);
            assert!((f.area - std::f64::consts::PI * 0.25 * f.fwtm_x * f.fwtm_y).abs() < 1e-9);
            assert!((f.mean_flux() - f.signal / f.area).abs() < 1e-12);
            assert!(f.residual < 0.02);
            assert!(f.eccentricity() < 0.15);
        }
    }

    #[test]
    fn auto_model_prefers_the_generating_beta() {
        let stars: Vec<MoffatStar> = (0..70)
            .map(|i| {
                round(
                    20.0 + (i % 10) as f64 * 40.3,
                    20.0 + (i / 10) as f64 * 40.7,
                    0.2 + 0.05 * (i % 5) as f64,
                )
            })
            .collect();
        let data = moffat_field(420, 320, &stars, 4.0, 0.05);
        let out = fit_stars(&data, 420, 320, &seeds_from(&stars, 4.0), PsfModel::Auto, &FitParams::default());
        assert_eq!(out.beta, 4.0);
        assert!(out.fits.len() >= 60, "{}", out.fits.len());

        let gstars: Vec<(f64, f64, f64)> = stars.iter().map(|s| (s.x, s.y, s.amp)).collect();
        let g = gaussian_field(420, 320, &gstars, 2.0, 0.05);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 2.0 * std::f64::consts::PI * 4.0 * s.amp })
            .collect();
        let out = fit_stars(&g, 420, 320, &seeds, PsfModel::Auto, &FitParams::default());
        assert_eq!(out.beta, 10.0, "a Gaussian field is closest to the largest β");
    }

    #[test]
    fn aperture_follows_the_fitted_ellipse_orientation() {
        let s = MoffatStar { x: 100.4, y: 90.6, amp: 0.4, alpha_x: 6.0, alpha_y: 3.0, theta: 30f64.to_radians() };
        let data = moffat_field(200, 200, &[s], 4.0, 0.05);
        let total = std::f64::consts::PI * s.amp * 18.0 / 3.0;
        let seeds = [Seed { x: s.x, y: s.y, peak: s.amp, flux: total }];
        let out = fit_stars(&data, 200, 200, &seeds, PsfModel::Moffat4, &FitParams::default());
        assert_eq!(out.fits.len(), 1);
        let f = &out.fits[0];
        let expected = total * (1.0 - 10f64.powf(0.25 - 1.0));
        assert!(
            (f.signal - expected).abs() < 0.06 * expected,
            "signal {} vs {expected}: the aperture rotation must follow the fitter's θ convention",
            f.signal
        );
        assert!((f.fwhm().powi(2) - f.fwhm_x * f.fwhm_y).abs() < 1e-9);
        assert!(f.eccentricity() > 0.8 && f.eccentricity() < 0.9, "{}", f.eccentricity());
    }

    #[test]
    fn fits_are_rejected_when_they_wander_duplicate_or_touch_the_border() {
        let stars = [round(60.0, 60.0, 0.5), round(8.0, 100.0, 0.5)];
        let data = moffat_field(160, 160, &stars, 4.0, 0.05);
        let good = Seed { x: 60.0, y: 60.0, peak: 0.5, flux: 13.1 };
        let off = Seed { x: 63.5, ..good }; // 3.5 px off: the fit walks back > 1.5 px
        let dup = Seed { x: 60.4, y: 60.3, ..good };
        let border = Seed { x: 8.0, y: 100.0, ..good }; // stamp radius 11 leaves the image
        let out = fit_stars(&data, 160, 160, &[good, off, dup, border], PsfModel::Moffat4, &FitParams::default());
        assert_eq!(out.fits.len(), 1, "{:?}", out.fits);
        assert!((out.fits[0].x - 60.0).abs() < 0.05);
        assert!(fit_stars(&data, 160, 160, &[], PsfModel::Auto, &FitParams::default()).fits.is_empty());
    }

    #[test]
    fn psf_model_serde_names() {
        assert_eq!(serde_json::to_string(&PsfModel::Auto).unwrap(), "\"auto\"");
        assert_eq!(serde_json::to_string(&PsfModel::Moffat4).unwrap(), "\"moffat4\"");
        assert_eq!(PsfModel::default(), PsfModel::Auto);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Add `pub mod psf_signal;` to `stacking/mod.rs`; run `cargo test -p athenaeum-core stacking::psf_signal` → compile errors.

- [ ] **Step 4: Implement** — above the tests:

```rust
//! The PSF-signal family of frame-quality estimators (math reference §1;
//! spec §4.1): elliptical Moffat fits on detected stars, the hybrid
//! PSF/aperture flux inside each fit's FWTM ellipse, robust totals, the
//! large-scale background residual, MRS noise, and the PSF Signal Weight /
//! PSF SNR formulas. Coordinates are 0-based pixel centres.

use std::collections::HashMap;

use astroimage::analysis::fitting::{fit_moffat_2d_fixed_beta, Moffat2DResult, PixelSample};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::integration::stats::median_in_place;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PsfModel {
    /// Fit the brightest `AUTO_SAMPLE` seeds with every β in `AUTO_BETAS`
    /// and keep the β with the smallest median residual.
    #[default]
    Auto,
    Moffat4,
}

pub const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0];
pub const AUTO_SAMPLE: usize = 64;

/// A detection to fit: centroid, background-subtracted peak and flux.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seed {
    pub x: f64,
    pub y: f64,
    pub peak: f64,
    pub flux: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitParams {
    /// A fit is accepted only if its centre stays within this distance of the seed.
    pub centroid_tolerance_px: f64,
    /// Aperture growth `k`: the ellipse semi-axes are `k·FWTM/2`.
    pub growth: f64,
    pub max_iter: usize,
    pub conv_tol: f64,
    pub max_rejects: usize,
}

impl Default for FitParams {
    fn default() -> Self {
        FitParams { centroid_tolerance_px: 1.5, growth: 1.0, max_iter: 50, conv_tol: 1e-5, max_rejects: 5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarFit {
    pub x: f64,
    pub y: f64,
    pub background: f64,
    pub amplitude: f64,
    pub fwhm_x: f64,
    pub fwhm_y: f64,
    pub fwtm_x: f64,
    pub fwtm_y: f64,
    pub theta: f64,
    pub beta: f64,
    /// Normalized fit residual, `sqrt(cost/n)/amplitude`.
    pub residual: f64,
    /// Background-subtracted flux inside the FWTM ellipse.
    pub signal: f64,
    /// Analytic area of that ellipse, `π·(k/2)²·fwtm_x·fwtm_y`.
    pub area: f64,
}

impl StarFit {
    pub fn mean_flux(&self) -> f64 {
        self.signal / self.area
    }
    pub fn fwhm(&self) -> f64 {
        (self.fwhm_x * self.fwhm_y).sqrt()
    }
    pub fn eccentricity(&self) -> f64 {
        let (a, b) = if self.fwhm_x >= self.fwhm_y { (self.fwhm_x, self.fwhm_y) } else { (self.fwhm_y, self.fwhm_x) };
        if a <= 0.0 {
            0.0
        } else {
            (1.0 - (b / a).powi(2)).max(0.0).sqrt()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FitOutcome {
    pub fits: Vec<StarFit>,
    pub beta: f64,
    /// Seeds handed in.
    pub seeds: usize,
}

/// Full width at tenth maximum of a Moffat profile, `2α√(10^{1/β} − 1)`.
pub fn fwtm_from_alpha(alpha: f64, beta: f64) -> f64 {
    2.0 * alpha * (10f64.powf(1.0 / beta) - 1.0).sqrt()
}

/// Field-level initial σ from the seeds' flux/peak ratio (a Gaussian's
/// `flux/peak = 2πσ²`): median over the 100 brightest, clamped to [0.7, 10].
fn initial_sigma(seeds: &[Seed]) -> f64 {
    let mut s: Vec<f64> = seeds
        .iter()
        .take(100)
        .filter(|s| s.peak > 0.0 && s.flux > 0.0)
        .map(|s| (s.flux / (2.0 * std::f64::consts::PI * s.peak)).sqrt())
        .collect();
    if s.is_empty() {
        return 2.0;
    }
    s.sort_by(|a, b| a.total_cmp(b));
    s[s.len() / 2].clamp(0.7, 10.0)
}

fn stamp_radius(sigma: f64) -> usize {
    ((5.0 * sigma).ceil() as usize).clamp(6, 48)
}

/// Fit one seed with a fixed β; `None` when the stamp leaves the image,
/// the fit fails, or the acceptance rules (math reference §1.4) reject it.
fn fit_one(data: &[f32], w: usize, h: usize, seed: &Seed, sigma0: f64, beta: f64, p: &FitParams) -> Option<StarFit> {
    let r = stamp_radius(sigma0) as i64;
    let (cx, cy) = (seed.x.round() as i64, seed.y.round() as i64);
    if cx - r < 0 || cy - r < 0 || cx + r >= w as i64 || cy + r >= h as i64 {
        return None;
    }
    let cap = ((2 * r + 1) * (2 * r + 1)) as usize;
    let mut px = Vec::with_capacity(cap);
    let mut vals = Vec::with_capacity(cap);
    let mut peak = f64::NEG_INFINITY;
    for y in cy - r..=cy + r {
        for x in cx - r..=cx + r {
            let v = data[y as usize * w + x as usize];
            if !v.is_finite() {
                continue;
            }
            peak = peak.max(v as f64);
            vals.push(v);
            px.push(PixelSample { x: x as f64, y: y as f64, value: v as f64 });
        }
    }
    if px.len() < 10 {
        return None;
    }
    let b0 = median_in_place(&mut vals) as f64;
    let a0 = (peak - b0).max(1e-9);
    let fit = fit_moffat_2d_fixed_beta(
        &px, b0, a0, seed.x, seed.y, sigma0, sigma0, 0.0, beta, p.max_iter, p.conv_tol, p.max_rejects,
    )?;
    let mut f = accept(&fit, seed, cx as f64, cy as f64, r as f64, p)?;
    aperture(data, w, h, &mut f, p.growth);
    Some(f)
}

fn accept(m: &Moffat2DResult, seed: &Seed, cx: f64, cy: f64, r: f64, p: &FitParams) -> Option<StarFit> {
    let finite = [m.b, m.a, m.x0, m.y0, m.alpha_x, m.alpha_y, m.theta, m.fit_residual]
        .iter()
        .all(|v| v.is_finite());
    if !m.converged || !finite || m.a <= 0.0 || m.alpha_x <= 0.0 || m.alpha_y <= 0.0 {
        return None;
    }
    if (m.x0 - seed.x).abs() > p.centroid_tolerance_px || (m.y0 - seed.y).abs() > p.centroid_tolerance_px {
        return None;
    }
    let inner = 0.85 * r;
    if (m.x0 - cx).abs() > inner || (m.y0 - cy).abs() > inner {
        return None;
    }
    let (fwtm_x, fwtm_y) = (fwtm_from_alpha(m.alpha_x, m.beta), fwtm_from_alpha(m.alpha_y, m.beta));
    if 0.5 * p.growth * fwtm_x.max(fwtm_y) > r {
        return None;
    }
    Some(StarFit {
        x: m.x0,
        y: m.y0,
        background: m.b,
        amplitude: m.a,
        fwhm_x: m.fwhm_x(),
        fwhm_y: m.fwhm_y(),
        fwtm_x,
        fwtm_y,
        theta: m.theta,
        beta: m.beta,
        residual: m.fit_residual,
        signal: 0.0,
        area: 0.0,
    })
}

/// Sum `pixel − background` over the pixels whose centres lie inside the
/// ellipse with semi-axes `k·fwtm/2`, rotated by `theta` about the fitted
/// centre (`u = dx·cosθ + dy·sinθ`, `v = −dx·sinθ + dy·cosθ`); `area` is
/// the ellipse's analytic area.
pub(crate) fn aperture(data: &[f32], w: usize, h: usize, f: &mut StarFit, k: f64) {
    let (a, b) = (0.5 * k * f.fwtm_x, 0.5 * k * f.fwtm_y);
    let (st, ct) = f.theta.sin_cos();
    let hx = ((a * ct).powi(2) + (b * st).powi(2)).sqrt();
    let hy = ((a * st).powi(2) + (b * ct).powi(2)).sqrt();
    let x0 = (f.x - hx).floor().max(0.0) as usize;
    let x1 = ((f.x + hx).ceil().max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = (f.y - hy).floor().max(0.0) as usize;
    let y1 = ((f.y + hy).ceil().max(0.0) as usize).min(h.saturating_sub(1));
    let mut sum = 0.0f64;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f64 - f.x, y as f64 - f.y);
            let u = dx * ct + dy * st;
            let v = -dx * st + dy * ct;
            if (u / a).powi(2) + (v / b).powi(2) <= 1.0 {
                let pv = data[y * w + x];
                if pv.is_finite() {
                    sum += pv as f64 - f.background;
                }
            }
        }
    }
    f.signal = sum;
    f.area = std::f64::consts::PI * a * b;
}

/// Drop every fit that has a brighter accepted fit within ±1 px.
fn dedupe(mut fits: Vec<StarFit>) -> Vec<StarFit> {
    fits.sort_by(|a, b| b.amplitude.total_cmp(&a.amplitude));
    let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    let mut out = Vec::with_capacity(fits.len());
    for f in fits {
        let (gx, gy) = (f.x.floor() as i64, f.y.floor() as i64);
        let mut clash = false;
        'scan: for dy in -1..=1 {
            for dx in -1..=1 {
                if let Some(list) = grid.get(&(gx + dx, gy + dy)) {
                    for &(x, y) in list {
                        if (x - f.x).abs() <= 1.0 && (y - f.y).abs() <= 1.0 {
                            clash = true;
                            break 'scan;
                        }
                    }
                }
            }
        }
        if clash {
            continue;
        }
        grid.entry((gx, gy)).or_default().push((f.x, f.y));
        out.push(f);
    }
    out
}

fn fit_all(data: &[f32], w: usize, h: usize, seeds: &[Seed], sigma0: f64, beta: f64, p: &FitParams) -> Vec<StarFit> {
    seeds.par_iter().filter_map(|s| fit_one(data, w, h, s, sigma0, beta, p)).collect()
}

/// Fit every seed with the chosen model. `Auto` fits the brightest
/// `AUTO_SAMPLE` seeds with each β in `AUTO_BETAS`, keeps the β with the
/// smallest median residual over its accepted fits (β = 4 when fewer than
/// 8 fits are accepted for every candidate), then fits all seeds with it.
/// Seeds are expected brightest-first (the detector's order).
pub fn fit_stars(data: &[f32], w: usize, h: usize, seeds: &[Seed], model: PsfModel, p: &FitParams) -> FitOutcome {
    let sigma0 = initial_sigma(seeds);
    let beta = match model {
        PsfModel::Moffat4 => 4.0,
        PsfModel::Auto => {
            let sample = &seeds[..seeds.len().min(AUTO_SAMPLE)];
            let mut best: Option<(f64, f64)> = None; // (median residual, β)
            for &b in &AUTO_BETAS {
                let mut res: Vec<f64> = fit_all(data, w, h, sample, sigma0, b, p).iter().map(|f| f.residual).collect();
                if res.len() < 8 {
                    continue;
                }
                res.sort_by(|x, y| x.total_cmp(y));
                let med = res[res.len() / 2];
                if best.map_or(true, |(m, _)| med < m) {
                    best = Some((med, b));
                }
            }
            best.map_or(4.0, |(_, b)| b)
        }
    };
    let fits = dedupe(fit_all(data, w, h, seeds, sigma0, beta, p));
    FitOutcome { fits, beta, seeds: seeds.len() }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core stacking::psf_signal` → 5 pass. If
`aperture_follows_the_fitted_ellipse_orientation` fails on the signal
while the round-star test passes, the fitter's θ convention differs from
the documented one: read `rustafits/src/analysis/fitting.rs` (the `u =`/`v =`
lines of the Moffat residual) and make `aperture` use the same rotation —
do not loosen the tolerance.
Run: `rustfmt crates/athenaeum-core/src/stacking/psf_signal.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/stacking/psf_signal.rs crates/athenaeum-core/src/stacking/mod.rs crates/athenaeum-core/src/test_support.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): Moffat fits on detections and the FWTM aperture flux" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 5: `psf_signal.rs` — totals, frame shape, `M*`/`N*`, MRS wrapper, the estimators

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/psf_signal.rs` (append below `fit_stars`; add tests)

**Interfaces:**
- Consumes: `crate::stacking::robust::{rcr, winsorize}`, `crate::integration::stats::{for_each_stratified, median_in_place, mad_about}`, `astroimage::analysis::background::{estimate_background_mesh, estimate_noise_mrs}` (`estimate_background_mesh(data, w, h, cell) -> BackgroundResult { background, noise, background_map: Option<Vec<f32>>, noise_map }` — full-resolution map; `estimate_noise_mrs(data, w, h, layers) -> f32`, floored at `0.001`).
- Produces: constants `PSFSW_NUM = 5.326e-6`, `PSFSW_DEN = 9.0e6`, `PSFSNR_NUM = 1.316e-7`, `PSFSNR_DEN = 4.987e6`, `N_STAR_FROM_MAD = 2.48308`, `BACKGROUND_MODEL_CELL_PX: usize = 128`, `MRS_LAYERS: usize = 4`, `RCR_LIMIT = 0.5`; `struct SignalTotals { tflux, tmean_flux: f64, rejected: usize }`, `fn signal_totals(fits: &[StarFit]) -> SignalTotals`, `fn frame_shape(fits: &[StarFit]) -> Option<(f64, f64)>` (FWHM px, eccentricity), `fn background_residual(data: &[f32], w, h) -> Option<(f64, f64)>` (`M*`, `N*`), `fn noise_mrs(data: &[f32], w, h) -> Option<f32>`, `fn psf_signal_weight(tflux, tmean_flux, sigma_n, m_star: f64) -> f64`, `fn psf_snr(tflux, sigma_n: f64) -> f64`.

- [ ] **Step 1: Write the failing tests** — append inside `mod tests`:

```rust
    use crate::test_support::add_noise;

    fn fit_with(mean: f64) -> StarFit {
        StarFit {
            x: 0.0, y: 0.0, background: 0.0, amplitude: 1.0,
            fwhm_x: 3.0, fwhm_y: 3.0, fwtm_x: 6.0, fwtm_y: 6.0,
            theta: 0.0, beta: 4.0, residual: 0.01,
            signal: mean * 10.0, area: 10.0,
        }
    }

    #[test]
    fn totals_winsorize_the_rcr_rejects_but_sum_every_signal() {
        let mut fits: Vec<StarFit> = (0..40).map(|j| fit_with(0.9 + 0.2 * j as f64 / 39.0)).collect();
        fits.push(fit_with(50.0)); // a saturated fit
        fits.push(fit_with(0.01)); // a blended / failed fit
        let t = signal_totals(&fits);
        assert_eq!(t.rejected, 2);
        assert!((t.tflux - 900.1).abs() < 1e-6, "{}", t.tflux);
        assert!((t.tmean_flux - 42.0).abs() < 1e-6, "{}", t.tmean_flux);
        let empty = signal_totals(&[]);
        assert_eq!((empty.tflux, empty.tmean_flux, empty.rejected), (0.0, 0.0, 0));
    }

    #[test]
    fn frame_shape_weights_by_residual() {
        let mut a = fit_with(1.0);
        a.fwhm_x = 4.0;
        a.fwhm_y = 4.0;
        a.residual = 0.01;
        let mut b = fit_with(1.0);
        b.fwhm_x = 8.0;
        b.fwhm_y = 2.0;
        b.residual = 0.03;
        // ω = 1 and 1/3: FWHM = (4 + 4/3)/(4/3) = 4; ecc = (0 + 0.96825/3)/(4/3) = 0.24206
        let (fwhm, ecc) = frame_shape(&[a, b]).unwrap();
        assert!((fwhm - 4.0).abs() < 1e-9, "{fwhm}");
        assert!((ecc - 0.24206).abs() < 1e-4, "{ecc}");
        assert!(frame_shape(&[]).is_none());
    }

    #[test]
    fn background_residual_of_pure_noise_is_half_normal() {
        let (w, h) = (512, 512);
        let mut data = vec![0.1f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] += 0.005 * x as f32 / w as f32; // a 5 % gradient the model must follow
            }
        }
        add_noise(&mut data, 0.01, 21);
        let (m, n) = background_residual(&data, w, h).unwrap();
        assert!((m - 0.006745).abs() < 0.15 * 0.006745, "M* {m}");
        assert!((n - 0.01).abs() < 0.10 * 0.01, "N* {n}");
        assert!(background_residual(&[0.5; 64], 8, 8).is_none());
    }

    #[test]
    fn mrs_noise_tracks_the_true_sigma_and_survives_stars() {
        let (w, h) = (512, 512);
        let mut plain = vec![1000.0f32; w * h]; // ADU-scaled units, 20 ADU of noise
        add_noise(&mut plain, 20.0, 31);
        let n = noise_mrs(&plain, w, h).unwrap();
        assert!((n - 20.0).abs() < 0.05 * 20.0, "pure noise: {n}");
        let stars: Vec<(f64, f64, f64)> = (0..200)
            .map(|i| (16.0 + (i % 20) as f64 * 24.0, 16.0 + (i / 20) as f64 * 48.0, 500.0 + 100.0 * (i % 7) as f64))
            .collect();
        let mut field = gaussian_field(w, h, &stars, 2.0, 1000.0);
        add_noise(&mut field, 20.0, 32);
        let n = noise_mrs(&field, w, h).unwrap();
        assert!((n - 20.0).abs() < 0.08 * 20.0, "star field: {n}");
        assert!(noise_mrs(&[5.0; 256], 16, 16).is_none());
    }

    #[test]
    fn estimator_formulas_by_hand() {
        let w = psf_signal_weight(1000.0, 50.0, 0.01, 0.007);
        assert!((w - 5.326e-6 * 1000.0 * 50.0 / (9.0e6 * 0.01 * 0.007)).abs() < 1e-12);
        assert_eq!(psf_signal_weight(1000.0, 50.0, 0.0, 0.007), 0.0);
        assert_eq!(psf_signal_weight(1000.0, 50.0, 0.01, 0.0), 0.0);
        let s = psf_snr(1000.0, 0.01);
        assert!((s - 1.316e-7 * 1e6 / (4.987e6 * 1e-4)).abs() < 1e-9);
        assert_eq!(psf_snr(1000.0, 0.0), 0.0);
        // scale invariance: everything ×65535 gives the same weights
        let k = 65535.0;
        assert!((psf_signal_weight(1000.0 * k, 50.0 * k, 0.01 * k, 0.007 * k) - w).abs() < 1e-9 * w);
        assert!((psf_snr(1000.0 * k, 0.01 * k) - s).abs() < 1e-9 * s);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core stacking::psf_signal` → compile errors (5 new tests).

- [ ] **Step 3: Implement** — append after `fit_stars`:

```rust
pub const PSFSW_NUM: f64 = 5.326e-6;
pub const PSFSW_DEN: f64 = 9.0e6;
pub const PSFSNR_NUM: f64 = 1.316e-7;
pub const PSFSNR_DEN: f64 = 4.987e6;
/// `N* = 2.48308·MAD(R)`: the MAD of a half-normal sample to its σ.
pub const N_STAR_FROM_MAD: f64 = 2.48308;
/// Mesh cell of the large-scale background model (model scale ≈ 256 px).
pub const BACKGROUND_MODEL_CELL_PX: usize = 128;
pub const MRS_LAYERS: usize = 4;
/// Chauvenet's criterion, the rejection limit for the mean-flux vector.
pub const RCR_LIMIT: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalTotals {
    /// `Σ signal_i` over every accepted fit.
    pub tflux: f64,
    /// `Σ mean_i` after RCR and Winsorization of the mean fluxes.
    pub tmean_flux: f64,
    /// Mean fluxes RCR flagged (and Winsorization replaced).
    pub rejected: usize,
}

fn kahan_sum(it: impl Iterator<Item = f64>) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for x in it {
        let y = x - c;
        let t = s + y;
        c = (t - s) - y;
        s = t;
    }
    s
}

/// `TFlux` and `TMeanFlux` (math reference §1.1): the mean-flux vector is
/// cleaned by RCR (limit 0.5) and Winsorized so nothing leaves the sum.
pub fn signal_totals(fits: &[StarFit]) -> SignalTotals {
    if fits.is_empty() {
        return SignalTotals { tflux: 0.0, tmean_flux: 0.0, rejected: 0 };
    }
    let means: Vec<f64> = fits.iter().map(StarFit::mean_flux).collect();
    let r = crate::stacking::robust::rcr(&means, RCR_LIMIT);
    let w = crate::stacking::robust::winsorize(&means, &r.kept);
    SignalTotals {
        tflux: kahan_sum(fits.iter().map(|f| f.signal)),
        tmean_flux: kahan_sum(w.iter().copied()),
        rejected: r.rejected,
    }
}

/// Frame FWHM and eccentricity: residual-weighted means over the fits
/// (`ω_i = res_min / res_i`, residuals floored at 1e-6). `None` without fits.
pub fn frame_shape(fits: &[StarFit]) -> Option<(f64, f64)> {
    if fits.is_empty() {
        return None;
    }
    let res_min = fits.iter().map(|f| f.residual.max(1e-6)).fold(f64::INFINITY, f64::min);
    let (mut sw, mut sf, mut se) = (0.0, 0.0, 0.0);
    for f in fits {
        let w = res_min / f.residual.max(1e-6);
        sw += w;
        sf += w * f.fwhm();
        se += w * f.eccentricity();
    }
    Some((sf / sw, se / sw))
}

/// `M*`, `N*` of the large-scale background residual (math reference
/// §1.3): the model `L` is the mesh background (cell 128 px);
/// `R = {L − v : v ≠ 0, v < L}` over the stratified sample;
/// `M* = median(R)`, `N* = 2.48308·MAD(R)`. `None` below 100 residuals.
pub fn background_residual(data: &[f32], w: usize, h: usize) -> Option<(f64, f64)> {
    let bg = astroimage::analysis::background::estimate_background_mesh(data, w, h, BACKGROUND_MODEL_CELL_PX);
    let model = bg.background_map?;
    let mut r: Vec<f32> = Vec::with_capacity(data.len() / 32);
    crate::integration::stats::for_each_stratified(w, h, |i| {
        let (v, l) = (data[i], model[i]);
        if v.is_finite() && l.is_finite() && v != 0.0 && v < l {
            r.push(l - v);
        }
    });
    if r.len() < 100 {
        return None;
    }
    let m = median_in_place(&mut r);
    let mad = crate::integration::stats::mad_about(&r, m);
    Some((m as f64, N_STAR_FROM_MAD * mad as f64))
}

/// MRS noise (`estimate_noise_mrs`, 4 layers). The estimator carries an
/// absolute floor tuned for 16-bit ADU data, so callers feed it ADU-scaled
/// values (`measure::ADU_SCALE`); `None` when the result sits on that
/// floor (a constant or near-constant plane).
pub fn noise_mrs(data: &[f32], w: usize, h: usize) -> Option<f32> {
    let n = astroimage::analysis::background::estimate_noise_mrs(data, w, h, MRS_LAYERS);
    if n.is_finite() && n > 0.002 {
        Some(n)
    } else {
        None
    }
}

/// `PSFSW = (5.326e-6 · TFlux · TMeanFlux) / (9.0e6 · σ_N · M*)`; 0 when
/// the denominator is not positive.
pub fn psf_signal_weight(tflux: f64, tmean_flux: f64, sigma_n: f64, m_star: f64) -> f64 {
    if !(sigma_n > 0.0) || !(m_star > 0.0) {
        return 0.0;
    }
    let w = PSFSW_NUM * tflux * tmean_flux / (PSFSW_DEN * sigma_n * m_star);
    if w.is_finite() && w > 0.0 {
        w
    } else {
        0.0
    }
}

/// `PSFSNR = (1.316e-7 · TFlux²) / (4.987e6 · σ_N²)`; 0 when σ_N ≤ 0.
pub fn psf_snr(tflux: f64, sigma_n: f64) -> f64 {
    if !(sigma_n > 0.0) {
        return 0.0;
    }
    let s = PSFSNR_NUM * tflux * tflux / (PSFSNR_DEN * sigma_n * sigma_n);
    if s.is_finite() {
        s
    } else {
        0.0
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::psf_signal` → 10 pass.
`mrs_noise_tracks_the_true_sigma_and_survives_stars` is the spec's "MRS
against tabulated values" check: if the estimator lands outside the 5 % /
8 % windows, report the measured values in the task report (DONE_WITH_CONCERNS)
instead of loosening the tolerance — the controller rules on a calibration
factor.
Run: `rustfmt crates/athenaeum-core/src/stacking/psf_signal.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/psf_signal.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): PSF-signal totals, background residual, MRS wrapper and the two estimators" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 6: `stacking/measure.rs` — per-plane and per-frame measurement

**Files:**
- Create: `crates/athenaeum-core/src/stacking/measure.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (add `pub mod measure;`)

**Interfaces:**
- Consumes: `astroimage::ImageAnalyzer` (`new()`, `with_max_stars(usize)`, `with_detection_sigma(f32)`, `with_centroid_refine(bool)`, `with_thread_pool(Arc<rayon::ThreadPool>)`, `detect_fast_data(&self, data: &[f32], width, height, channels) -> Result<FastAnalysisResult { stars: Vec<FastStar { x, y, peak, flux, snr, .. }>, .. }>` — stars brightest-first, peak/flux background-subtracted); `crate::integration::plane_reader::PlaneReader`; `crate::integration::IntegrationError`; `crate::integration::stats::*`; `super::psf_signal::*`.
- Produces (`crate::stacking::measure`): `const ADU_SCALE: f32 = 65535.0`, `struct MeasureOptions { psf_model: PsfModel, max_stars: usize, scale_estimator: ScaleEstimator, detection_sigma: f32, min_snr: f32 }` (serde camelCase; `Default` = `Auto, 24576, Bwmv, 5.0, 5.0`), `enum NoiseSource { Mrs, BackgroundResidual }` (serde `mrs|backgroundResidual`), `struct ChannelMeasurement { stars_detected, stars_fitted: usize, beta, fwhm_px, eccentricity, tflux, tmean_flux, m_star, n_star, noise: f64, noise_source, median, mad, median_mean_dev, snr_weight, noise_scale_low, noise_scale_high, location, scale, psf_signal_weight, psf_snr: f64 }` (serde camelCase, `PartialEq`) with `location_scale() -> LocationScale`, `struct FrameMeasurement { width, height: usize, channels: Vec<ChannelMeasurement>, duration_ms: u64 }` with `mean_fwhm_px()`, `mean_eccentricity()`, `mean_psf_signal_weight()`, `min_stars()`, `fn measure_plane(data: &[f32], w, h, opts: &MeasureOptions, pool: Option<&Arc<rayon::ThreadPool>>) -> ChannelMeasurement`, `fn measure_frame(path: &Path, opts: &MeasureOptions, pool: Option<&Arc<rayon::ThreadPool>>, cancel: &AtomicBool) -> Result<FrameMeasurement, IntegrationError>`.

- [ ] **Step 1: Write the failing tests** — `measure.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::ransac::SplitMix64;
    use crate::test_support::{add_noise, gaussian_field};
    use std::path::PathBuf;

    /// 150 Gaussian stars (σ 1.8 px) on a jittered 15×10 grid, amplitudes
    /// `(0.1..0.3)·amp_scale`, background 0.08, Gaussian noise `noise`.
    fn field(seed: u64, amp_scale: f64, noise: f32) -> (Vec<f32>, usize, usize) {
        let (w, h) = (768, 512);
        let mut rng = SplitMix64(seed);
        let mut stars = Vec::new();
        for j in 0..10 {
            for i in 0..15 {
                let x = 40.0 + i as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
                let y = 40.0 + j as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
                stars.push((x, y, (0.1 + 0.2 * rng.next_f64()) * amp_scale));
            }
        }
        let mut data = gaussian_field(w, h, &stars, 1.8, 0.08);
        add_noise(&mut data, noise, seed + 100);
        (data, w, h)
    }

    fn write(dir: &Path, name: &str, planes: &[&[f32]], w: usize, h: usize) -> PathBuf {
        let mut all = Vec::new();
        for p in planes {
            all.extend_from_slice(p);
        }
        let path = dir.join(name);
        write_fits_f32(&path, w, h, planes.len(), &all, &[]).unwrap();
        path
    }

    #[test]
    fn measures_a_synthetic_mono_frame() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "mono.fits", &[&data], w, h);
        let m = measure_frame(&path, &MeasureOptions::default(), None, &AtomicBool::new(false)).unwrap();
        assert_eq!((m.width, m.height, m.channels.len()), (w, h, 1));
        let c = &m.channels[0];
        assert!(c.stars_detected >= c.stars_fitted);
        assert!(c.stars_fitted >= 120, "fitted {}", c.stars_fitted);
        let fwhm = 2.3548 * 1.8;
        assert!((c.fwhm_px - fwhm).abs() < 0.1 * fwhm, "fwhm {}", c.fwhm_px);
        assert!(c.eccentricity < 0.15, "ecc {}", c.eccentricity);
        assert!(c.beta >= 6.0, "beta {}", c.beta);
        assert!((c.noise - 0.002).abs() < 0.1 * 0.002, "noise {}", c.noise);
        assert_eq!(c.noise_source, NoiseSource::Mrs);
        assert!((c.median - 0.08).abs() < 0.001, "median {}", c.median);
        assert!((c.scale - 0.002).abs() < 0.1 * 0.002, "scale {}", c.scale);
        assert!((c.n_star - 0.002).abs() < 0.15 * 0.002, "N* {}", c.n_star);
        assert!(c.m_star > 0.0 && c.tflux > 0.0 && c.tmean_flux > 0.0);
        assert!(c.psf_signal_weight > 0.0 && c.psf_signal_weight.is_finite());
        assert!(c.psf_snr > 0.0 && c.snr_weight > 0.0);
        assert!(c.noise_scale_low > 0.0 && c.noise_scale_high > 0.0);
        assert!(m.duration_ms < 60_000);
        assert_eq!(m.min_stars(), c.stars_fitted);
    }

    #[test]
    fn psf_signal_weight_scales_with_signal_squared_and_inverse_noise_squared() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MeasureOptions { psf_model: PsfModel::Moffat4, ..MeasureOptions::default() };
        let run = |name: &str, amp: f64, noise: f32| {
            let (d, w, h) = field(11, amp, noise);
            let p = write(dir.path(), name, &[&d], w, h);
            measure_frame(&p, &opts, None, &AtomicBool::new(false)).unwrap().channels.remove(0)
        };
        let a = run("a.fits", 1.0, 0.002);
        let b = run("b.fits", 2.0, 0.002);
        let c = run("c.fits", 1.0, 0.004);
        let same_stars = |x: &ChannelMeasurement, y: &ChannelMeasurement| {
            (x.stars_fitted as f64 - y.stars_fitted as f64).abs() <= 0.05 * x.stars_fitted as f64
        };
        assert!(same_stars(&a, &b) && same_stars(&a, &c), "{} {} {}", a.stars_fitted, b.stars_fitted, c.stars_fitted);
        let r = b.psf_signal_weight / a.psf_signal_weight;
        assert!(r > 3.4 && r < 4.6, "signal ×2 → PSFSW ratio {r}");
        let r = b.psf_snr / a.psf_snr;
        assert!(r > 3.6 && r < 4.4, "signal ×2 → PSFSNR ratio {r}");
        let r = c.psf_signal_weight / a.psf_signal_weight;
        assert!(r > 0.19 && r < 0.31, "noise ×2 → PSFSW ratio {r}");
        let r = c.psf_snr / a.psf_snr;
        assert!(r > 0.2 && r < 0.3, "noise ×2 → PSFSNR ratio {r}");
    }

    #[test]
    fn rgb_frames_measure_each_plane_and_honour_cancel() {
        let (r, w, h) = field(3, 1.0, 0.002);
        let g: Vec<f32> = r.iter().map(|v| v * 0.5).collect();
        let b = r.clone();
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "rgb.fits", &[&r, &g, &b], w, h);
        let opts = MeasureOptions { psf_model: PsfModel::Moffat4, ..MeasureOptions::default() };
        let m = measure_frame(&path, &opts, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(m.channels.len(), 3);
        let (cr, cg, cb) = (&m.channels[0], &m.channels[1], &m.channels[2]);
        assert!((cg.median - 0.5 * cr.median).abs() < 0.001);
        assert!(
            (cg.psf_signal_weight - cr.psf_signal_weight).abs() < 0.15 * cr.psf_signal_weight,
            "PSFSW is scale-invariant: {} vs {}",
            cg.psf_signal_weight,
            cr.psf_signal_weight
        );
        assert!((cb.stars_fitted as i64 - cr.stars_fitted as i64).abs() <= 2);
        assert!((cb.psf_signal_weight - cr.psf_signal_weight).abs() < 1e-3 * cr.psf_signal_weight);
        assert!(m.mean_fwhm_px() > 3.0 && m.min_stars() >= 120);
        assert!(m.mean_eccentricity() < 0.15 && m.mean_psf_signal_weight() > 0.0);
        assert!(matches!(
            measure_frame(&path, &opts, None, &AtomicBool::new(true)),
            Err(IntegrationError::Cancelled)
        ));
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"psfSignalWeight\"") && json.contains("\"noiseSource\":\"mrs\""));
        let back: FrameMeasurement = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        let bad = dir.path().join("nope.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(matches!(
            measure_frame(&bad, &opts, None, &AtomicBool::new(false)),
            Err(IntegrationError::BadInput(_))
        ));
    }

    #[test]
    fn a_starless_plane_measures_with_zero_weight() {
        let (w, h) = (256, 256);
        let mut data = vec![0.1f32; w * h];
        add_noise(&mut data, 0.002, 41);
        let c = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert_eq!(c.stars_fitted, 0);
        assert_eq!(c.psf_signal_weight, 0.0);
        assert_eq!(c.psf_snr, 0.0);
        assert_eq!((c.fwhm_px, c.eccentricity), (0.0, 0.0));
        assert!((c.noise - 0.002).abs() < 0.1 * 0.002);
        assert!((c.location - 0.1).abs() < 0.001);
    }

    #[test]
    fn options_serde_defaults() {
        let d = MeasureOptions::default();
        assert_eq!(d.max_stars, 24576);
        assert_eq!(d.psf_model, PsfModel::Auto);
        assert_eq!(d.scale_estimator, ScaleEstimator::Bwmv);
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"psfModel\":\"auto\"") && json.contains("\"maxStars\":24576"));
        assert!(json.contains("\"scaleEstimator\":\"bwmv\"") && json.contains("\"minSnr\":5.0"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `pub mod measure;` to `stacking/mod.rs`; run `cargo test -p athenaeum-core stacking::measure` → compile errors.

- [ ] **Step 3: Implement** — above the tests:

```rust
//! Per-frame, per-channel measurement of calibrated frames (spec §4.1):
//! detection, PSF fits, the PSF-signal totals, the large-scale background
//! residual, MRS noise, the normalization statistics and the classic SNR
//! weight — everything the weighting, selection and normalization stages
//! consume. Reads through `PlaneReader`, one plane at a time.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use astroimage::ImageAnalyzer;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::psf_signal::{self, FitParams, PsfModel, Seed};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::stats::{self, LocationScale, ScaleEstimator, CLIP_HI, CLIP_LO};
use crate::integration::IntegrationError;

/// Calibrated frames are float32 in `[0, 1]`; the detector and the MRS
/// estimator carry absolute floors tuned for 16-bit ADU data, so they see a
/// copy scaled by this factor. Every estimator this module reports is
/// scale-invariant (PSF Signal Weight, PSF SNR) or divided back into
/// native units.
pub const ADU_SCALE: f32 = 65535.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureOptions {
    pub psf_model: PsfModel,
    /// Detection cap (spec §9.2 `maxStars`).
    pub max_stars: usize,
    pub scale_estimator: ScaleEstimator,
    pub detection_sigma: f32,
    /// Detections below this aperture SNR are not fitted.
    pub min_snr: f32,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        MeasureOptions {
            psf_model: PsfModel::Auto,
            max_stars: 24576,
            scale_estimator: ScaleEstimator::Bwmv,
            detection_sigma: 5.0,
            min_snr: 5.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoiseSource {
    Mrs,
    /// MRS was unavailable; `noise` is `N*`.
    BackgroundResidual,
}

/// One channel's measurement, in native `[0, 1]` units.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMeasurement {
    pub stars_detected: usize,
    pub stars_fitted: usize,
    pub beta: f64,
    pub fwhm_px: f64,
    pub eccentricity: f64,
    pub tflux: f64,
    pub tmean_flux: f64,
    pub m_star: f64,
    pub n_star: f64,
    pub noise: f64,
    pub noise_source: NoiseSource,
    pub median: f64,
    pub mad: f64,
    pub median_mean_dev: f64,
    /// Classic `MedianMeanDev² / Noise²`.
    pub snr_weight: f64,
    pub noise_scale_low: f64,
    pub noise_scale_high: f64,
    /// Normalization location (median) and two-sided scale.
    pub location: f64,
    pub scale: f64,
    pub psf_signal_weight: f64,
    pub psf_snr: f64,
}

impl ChannelMeasurement {
    pub fn location_scale(&self) -> LocationScale {
        LocationScale { location: self.location as f32, scale: self.scale as f32 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMeasurement {
    pub width: usize,
    pub height: usize,
    pub channels: Vec<ChannelMeasurement>,
    pub duration_ms: u64,
}

impl FrameMeasurement {
    fn mean_of(&self, f: impl Fn(&ChannelMeasurement) -> f64) -> f64 {
        if self.channels.is_empty() {
            0.0
        } else {
            self.channels.iter().map(f).sum::<f64>() / self.channels.len() as f64
        }
    }
    pub fn mean_fwhm_px(&self) -> f64 {
        self.mean_of(|c| c.fwhm_px)
    }
    pub fn mean_eccentricity(&self) -> f64 {
        self.mean_of(|c| c.eccentricity)
    }
    pub fn mean_psf_signal_weight(&self) -> f64 {
        self.mean_of(|c| c.psf_signal_weight)
    }
    pub fn min_stars(&self) -> usize {
        self.channels.iter().map(|c| c.stars_fitted).min().unwrap_or(0)
    }
}

/// Measure one plane. Detection, fitting, the background model and MRS run
/// on an ADU-scaled copy; the sample statistics run on the native data.
pub fn measure_plane(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> ChannelMeasurement {
    let scaled: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();

    let mut analyzer = ImageAnalyzer::new()
        .with_max_stars(opts.max_stars.max(8))
        .with_detection_sigma(opts.detection_sigma)
        .with_centroid_refine(false);
    if let Some(p) = pool {
        analyzer = analyzer.with_thread_pool(Arc::clone(p));
    }
    let seeds: Vec<Seed> = match analyzer.detect_fast_data(&scaled, w, h, 1) {
        Ok(r) => r
            .stars
            .iter()
            .filter(|s| s.snr >= opts.min_snr && s.peak > 0.0)
            .map(|s| Seed { x: s.x as f64, y: s.y as f64, peak: s.peak as f64, flux: s.flux as f64 })
            .collect(),
        Err(e) => {
            warn!(error = %e, "star detection failed; the plane measures as starless");
            Vec::new()
        }
    };
    let stars_detected = seeds.len();

    let params = FitParams::default();
    let outcome = match pool {
        Some(p) => p.install(|| psf_signal::fit_stars(&scaled, w, h, &seeds, opts.psf_model, &params)),
        None => psf_signal::fit_stars(&scaled, w, h, &seeds, opts.psf_model, &params),
    };
    let totals = psf_signal::signal_totals(&outcome.fits);
    let (fwhm_px, eccentricity) = psf_signal::frame_shape(&outcome.fits).unwrap_or((0.0, 0.0));
    let (m_star, n_star) = psf_signal::background_residual(&scaled, w, h).unwrap_or((0.0, 0.0));
    let (noise_adu, noise_source) = match psf_signal::noise_mrs(&scaled, w, h) {
        Some(n) => (n as f64, NoiseSource::Mrs),
        None => {
            warn!(n_star, "MRS noise unavailable; using the background residual scale");
            (n_star, NoiseSource::BackgroundResidual)
        }
    };

    let sample = stats::stratified_sample(data, w, h);
    let clipped = stats::clip_sample(&sample, CLIP_LO, CLIP_HI);
    let (median, mad, median_mean_dev) = if clipped.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        let m = stats::median_of(&clipped);
        (m as f64, stats::mad_about(&clipped, m) as f64, stats::avg_dev_about(&clipped, m) as f64)
    };
    let ls = stats::location_scale(&clipped, opts.scale_estimator)
        .unwrap_or(LocationScale { location: median as f32, scale: 0.0 });
    let (noise_scale_low, noise_scale_high) = stats::noise_scale_factors(&sample)
        .map(|(a, b)| (a as f64, b as f64))
        .unwrap_or((0.0, 0.0));

    let s = ADU_SCALE as f64;
    let noise = noise_adu / s;
    let snr_weight = if noise > 0.0 { (median_mean_dev / noise).powi(2) } else { 0.0 };
    ChannelMeasurement {
        stars_detected,
        stars_fitted: outcome.fits.len(),
        beta: outcome.beta,
        fwhm_px,
        eccentricity,
        tflux: totals.tflux / s,
        tmean_flux: totals.tmean_flux / s,
        m_star: m_star / s,
        n_star: n_star / s,
        noise,
        noise_source,
        median,
        mad,
        median_mean_dev,
        snr_weight,
        noise_scale_low,
        noise_scale_high,
        location: ls.location as f64,
        scale: ls.scale as f64,
        // Both estimators are scale-invariant, so the ADU-unit inputs give
        // the native-unit answer.
        psf_signal_weight: psf_signal::psf_signal_weight(totals.tflux, totals.tmean_flux, noise_adu, m_star),
        psf_snr: psf_signal::psf_snr(totals.tflux, noise_adu),
    }
}

/// Measure every plane of a calibrated frame; checks `cancel` before each
/// plane.
pub fn measure_frame(
    path: &Path,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FrameMeasurement, IntegrationError> {
    let start = Instant::now();
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    let mut channels = Vec::with_capacity(reader.channels());
    for plane in 0..reader.channels() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let data = reader.read_plane(plane)?;
        let t = Instant::now();
        let m = measure_plane(&data, w, h, opts, pool);
        debug!(
            path = %path.display(),
            plane,
            stars_detected = m.stars_detected,
            stars_fitted = m.stars_fitted,
            beta = m.beta,
            fwhm_px = m.fwhm_px,
            eccentricity = m.eccentricity,
            noise = m.noise,
            psf_signal_weight = m.psf_signal_weight,
            psf_snr = m.psf_snr,
            duration_ms = t.elapsed().as_millis() as u64,
            "frame plane measured"
        );
        channels.push(m);
    }
    Ok(FrameMeasurement { width: w, height: h, channels, duration_ms: start.elapsed().as_millis() as u64 })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::measure` → 5 pass (each synthetic
frame measures in well under a second in debug builds; if a test takes
longer than 30 s, report it — do not shrink the fixtures).
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `rustfmt crates/athenaeum-core/src/stacking/measure.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/measure.rs crates/athenaeum-core/src/stacking/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): per-plane and per-frame measurement of calibrated frames" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 7: `stacking/weights.rs` — weight modes, selection, best-by-weight

**Files:**
- Create: `crates/athenaeum-core/src/stacking/weights.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (add `pub mod weights;`)

**Interfaces:**
- Consumes: `super::measure::{ChannelMeasurement, FrameMeasurement}`.
- Produces (`crate::stacking::weights`): `enum WeightMode { PsfSignalWeight, PsfSnr, Noise, Formula, Exposure, Keyword, None }` (serde `psfSignalWeight|psfSnr|noise|formula|exposure|keyword|none`, `Default = PsfSignalWeight`), `struct FormulaWeights { fwhm, eccentricity, snr, stars, pedestal: f64 }` (serde camelCase; `Default = 15, 15, 20, 0, 50`), `struct WeightInput<'a> { measurement: &'a FrameMeasurement, exposure_s: Option<f64>, keyword_value: Option<f64> }`, `struct FrameWeight { channels: Vec<f64>, normalized: Vec<f64>, mean: f64, normalized_mean: f64, missing: Option<String> }` (serde camelCase), `fn compute_weights(inputs: &[WeightInput], mode: WeightMode, formula: &FormulaWeights, excluded: &[bool]) -> Vec<FrameWeight>`, `struct SelectionConfig { min_weight_fraction: f64, max_fwhm_px: Option<f64>, max_eccentricity: Option<f64>, min_stars: Option<usize> }` (serde camelCase; `Default = 0.05, None, None, None`), `fn select_frames(inputs: &[WeightInput], weights: &[FrameWeight], manual_excluded: &[bool], cfg: &SelectionConfig) -> Vec<Option<String>>`, `fn best_by_weight(weights: &[FrameWeight], included: &[bool], star_counts: &[usize]) -> Option<usize>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::measure::NoiseSource;

    fn meas(psfsw: &[f64], fwhm: f64, ecc: f64, stars: usize, snrw: f64) -> FrameMeasurement {
        FrameMeasurement {
            width: 10,
            height: 10,
            duration_ms: 0,
            channels: psfsw
                .iter()
                .map(|&w| ChannelMeasurement {
                    stars_detected: stars,
                    stars_fitted: stars,
                    beta: 4.0,
                    fwhm_px: fwhm,
                    eccentricity: ecc,
                    tflux: 1.0,
                    tmean_flux: 1.0,
                    m_star: 1.0,
                    n_star: 1.0,
                    noise: 0.01,
                    noise_source: NoiseSource::Mrs,
                    median: 0.1,
                    mad: 0.01,
                    median_mean_dev: 0.012,
                    snr_weight: snrw,
                    noise_scale_low: 0.006,
                    noise_scale_high: 0.006,
                    location: 0.1,
                    scale: 0.01,
                    psf_signal_weight: w,
                    psf_snr: w * 2.0,
                })
                .collect(),
        }
    }
    fn input(m: &FrameMeasurement) -> WeightInput<'_> {
        WeightInput { measurement: m, exposure_s: Some(120.0), keyword_value: None }
    }

    #[test]
    fn psf_signal_weights_normalize_per_channel_and_average() {
        let ms = [meas(&[2.0, 1.0], 3.0, 0.1, 100, 5.0), meas(&[1.0, 4.0], 3.0, 0.1, 100, 5.0)];
        let inputs: Vec<WeightInput> = ms.iter().map(input).collect();
        let w = compute_weights(&inputs, WeightMode::PsfSignalWeight, &FormulaWeights::default(), &[false, false]);
        assert_eq!(w[0].channels, vec![2.0, 1.0]);
        assert_eq!(w[0].normalized, vec![1.0, 0.25]);
        assert_eq!(w[1].normalized, vec![0.5, 1.0]);
        assert!((w[0].mean - 1.5).abs() < 1e-12);
        assert!((w[0].normalized_mean - 0.625).abs() < 1e-12);
        assert!(w.iter().all(|x| x.missing.is_none()));
    }

    #[test]
    fn excluded_frames_do_not_set_the_maximum() {
        let ms = [meas(&[10.0], 3.0, 0.1, 100, 5.0), meas(&[1.0], 3.0, 0.1, 100, 5.0)];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let w = compute_weights(&inputs, WeightMode::PsfSignalWeight, &FormulaWeights::default(), &[true, false]);
        assert_eq!(w[0].normalized, vec![10.0]);
        assert_eq!(w[1].normalized, vec![1.0]);
    }

    #[test]
    fn other_modes() {
        let m = meas(&[2.0], 3.0, 0.1, 100, 5.0);
        let inputs = [
            WeightInput { measurement: &m, exposure_s: Some(300.0), keyword_value: Some(0.7) },
            WeightInput { measurement: &m, exposure_s: None, keyword_value: None },
        ];
        let f = FormulaWeights::default();
        let none = [false, false];
        let w = compute_weights(&inputs, WeightMode::PsfSnr, &f, &none);
        assert_eq!(w[0].channels, vec![4.0]);
        let w = compute_weights(&inputs, WeightMode::Noise, &f, &none);
        assert!((w[0].channels[0] - (0.006f64 / 0.01).powi(2)).abs() < 1e-12);
        let w = compute_weights(&inputs, WeightMode::Exposure, &f, &none);
        assert_eq!(w[0].channels, vec![300.0]);
        assert_eq!(w[1].channels, vec![0.0]);
        assert_eq!(w[1].missing.as_deref(), Some("no exposure time"));
        assert_eq!(w[0].normalized, vec![1.0]);
        let w = compute_weights(&inputs, WeightMode::Keyword, &f, &none);
        assert_eq!(w[0].channels, vec![0.7]);
        assert_eq!(w[1].missing.as_deref(), Some("no weight keyword value"));
        let w = compute_weights(&inputs, WeightMode::None, &f, &none);
        assert_eq!(w[0].normalized, vec![1.0]);
        assert_eq!(w[1].normalized, vec![1.0]);
    }

    #[test]
    fn formula_mode_ranks_by_the_four_terms() {
        let ms = [
            meas(&[1.0], 2.0, 0.2, 300, 9.0),
            meas(&[1.0], 4.0, 0.2, 100, 1.0),
            meas(&[1.0], 3.0, 0.2, 200, 5.0),
        ];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let f = FormulaWeights { fwhm: 15.0, eccentricity: 15.0, snr: 20.0, stars: 10.0, pedestal: 50.0 };
        let w = compute_weights(&inputs, WeightMode::Formula, &f, &[false; 3]);
        assert!((w[0].channels[0] - 110.0).abs() < 1e-9, "{}", w[0].channels[0]); // 15 + 15 + 20 + 10 + 50
        assert!((w[1].channels[0] - 65.0).abs() < 1e-9, "{}", w[1].channels[0]); // 0 + 15 + 0 + 0 + 50
        assert!((w[2].channels[0] - 87.5).abs() < 1e-9, "{}", w[2].channels[0]); // 7.5 + 15 + 10 + 5 + 50
        assert_eq!(w[0].normalized, vec![1.0]);
    }

    #[test]
    fn selection_reasons_in_order() {
        let ms = [
            meas(&[1.0], 3.0, 0.1, 100, 5.0),
            meas(&[0.02], 3.0, 0.1, 100, 5.0),
            meas(&[0.9], 5.0, 0.1, 100, 5.0),
            meas(&[0.9], 3.0, 0.7, 100, 5.0),
            meas(&[0.9], 3.0, 0.1, 10, 5.0),
            meas(&[0.9], 3.0, 0.1, 100, 5.0),
        ];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let manual = [false, false, false, false, false, true];
        let w = compute_weights(&inputs, WeightMode::PsfSignalWeight, &FormulaWeights::default(), &manual);
        let cfg = SelectionConfig {
            min_weight_fraction: 0.05,
            max_fwhm_px: Some(4.0),
            max_eccentricity: Some(0.5),
            min_stars: Some(50),
        };
        let r = select_frames(&inputs, &w, &manual, &cfg);
        assert_eq!(r[0], None);
        assert_eq!(r[1].as_deref(), Some("weight 0.020 below 0.05 of the group maximum"));
        assert_eq!(r[2].as_deref(), Some("FWHM 5.00 px above 4.00"));
        assert_eq!(r[3].as_deref(), Some("eccentricity 0.70 above 0.50"));
        assert_eq!(r[4].as_deref(), Some("10 stars below 50"));
        assert_eq!(r[5].as_deref(), Some("excluded manually"));
        let included: Vec<bool> = r.iter().map(|x| x.is_none()).collect();
        assert_eq!(best_by_weight(&w, &included, &[100, 100, 100, 100, 10, 100]), Some(0));
        assert_eq!(best_by_weight(&w, &[false; 6], &[100; 6]), None);
        let r = select_frames(&inputs, &w, &manual, &SelectionConfig::default());
        assert_eq!(r[2], None);
        assert_eq!(r[4], None);
    }

    #[test]
    fn ties_in_weight_break_on_star_count() {
        let ms = [meas(&[1.0], 3.0, 0.1, 100, 5.0), meas(&[1.0], 3.0, 0.1, 150, 5.0)];
        let inputs: Vec<_> = ms.iter().map(input).collect();
        let w = compute_weights(&inputs, WeightMode::PsfSignalWeight, &FormulaWeights::default(), &[false, false]);
        assert_eq!(best_by_weight(&w, &[true, true], &[100, 150]), Some(1));
    }

    #[test]
    fn missing_inputs_are_named_before_the_weight_gate() {
        let m = meas(&[1.0], 3.0, 0.1, 100, 5.0);
        let inputs = [
            WeightInput { measurement: &m, exposure_s: Some(60.0), keyword_value: None },
            WeightInput { measurement: &m, exposure_s: None, keyword_value: None },
        ];
        let w = compute_weights(&inputs, WeightMode::Exposure, &FormulaWeights::default(), &[false, false]);
        let r = select_frames(&inputs, &w, &[false, false], &SelectionConfig::default());
        assert_eq!(r[0], None);
        assert_eq!(r[1].as_deref(), Some("no exposure time"));
    }

    #[test]
    fn serde_names_match_the_spec() {
        assert_eq!(serde_json::to_string(&WeightMode::PsfSignalWeight).unwrap(), "\"psfSignalWeight\"");
        assert_eq!(serde_json::to_string(&WeightMode::PsfSnr).unwrap(), "\"psfSnr\"");
        assert_eq!(serde_json::to_string(&WeightMode::None).unwrap(), "\"none\"");
        assert_eq!(WeightMode::default(), WeightMode::PsfSignalWeight);
        let f = serde_json::to_string(&FormulaWeights::default()).unwrap();
        assert!(f.contains("\"fwhm\":15.0") && f.contains("\"pedestal\":50.0") && f.contains("\"stars\":0.0"));
        let s = serde_json::to_string(&SelectionConfig::default()).unwrap();
        assert!(s.contains("\"minWeightFraction\":0.05") && s.contains("\"maxFwhmPx\":null"));
        let back: WeightMode = serde_json::from_str("\"keyword\"").unwrap();
        assert_eq!(back, WeightMode::Keyword);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `pub mod weights;` to `stacking/mod.rs`; run `cargo test -p athenaeum-core stacking::weights` → compile errors.

- [ ] **Step 3: Implement**

```rust
//! Frame weights, selection and reference choice (spec §4.2–4.4, math
//! reference §1.2, §1.5, §2.5, §3.7). Weights are per channel and
//! normalized by the group maximum per channel; the frame-level weight is
//! the mean over channels.

use serde::{Deserialize, Serialize};

use super::measure::{ChannelMeasurement, FrameMeasurement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum WeightMode {
    #[default]
    PsfSignalWeight,
    PsfSnr,
    /// `(noiseScale / σ_N)²` with `noiseScale` the mean of the two noise-scale factors.
    Noise,
    /// The classic formula with the user's `FormulaWeights`.
    Formula,
    Exposure,
    Keyword,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormulaWeights {
    pub fwhm: f64,
    pub eccentricity: f64,
    pub snr: f64,
    pub stars: f64,
    pub pedestal: f64,
}

impl Default for FormulaWeights {
    fn default() -> Self {
        FormulaWeights { fwhm: 15.0, eccentricity: 15.0, snr: 20.0, stars: 0.0, pedestal: 50.0 }
    }
}

pub struct WeightInput<'a> {
    pub measurement: &'a FrameMeasurement,
    /// `EXPTIME`, for `WeightMode::Exposure`.
    pub exposure_s: Option<f64>,
    /// The configured keyword's value, for `WeightMode::Keyword`.
    pub keyword_value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameWeight {
    /// Raw weight per channel.
    pub channels: Vec<f64>,
    /// `channels / max over the group's non-excluded frames`, per channel.
    pub normalized: Vec<f64>,
    pub mean: f64,
    pub normalized_mean: f64,
    /// Why this frame has no usable weight (a missing exposure or keyword).
    pub missing: Option<String>,
}

#[derive(Clone, Copy)]
struct Range {
    lo: f64,
    hi: f64,
}

impl Range {
    fn empty() -> Self {
        Range { lo: f64::INFINITY, hi: f64::NEG_INFINITY }
    }
    fn add(&mut self, v: f64) {
        self.lo = self.lo.min(v);
        self.hi = self.hi.max(v);
    }
    /// `(v − lo)/(hi − lo)`; a degenerate range scores full credit.
    fn credit_up(&self, v: f64) -> f64 {
        if self.hi > self.lo {
            ((v - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
    /// `1 − (v − lo)/(hi − lo)`; a degenerate range scores full credit.
    fn credit_down(&self, v: f64) -> f64 {
        if self.hi > self.lo {
            (1.0 - (v - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

#[derive(Clone, Copy)]
struct FormulaRanges {
    fwhm: Range,
    ecc: Range,
    snr: Range,
    stars: Range,
}

fn formula_ranges(inputs: &[WeightInput], excluded: &[bool], nch: usize) -> Vec<FormulaRanges> {
    let mut out = vec![
        FormulaRanges { fwhm: Range::empty(), ecc: Range::empty(), snr: Range::empty(), stars: Range::empty() };
        nch
    ];
    for (i, inp) in inputs.iter().enumerate() {
        if excluded.get(i).copied().unwrap_or(false) {
            continue;
        }
        for (c, ch) in inp.measurement.channels.iter().enumerate().take(nch) {
            out[c].fwhm.add(ch.fwhm_px);
            out[c].ecc.add(ch.eccentricity);
            out[c].snr.add(ch.snr_weight);
            out[c].stars.add(ch.stars_fitted as f64);
        }
    }
    out
}

/// `W = A·(1 − ΔFWHM) + B·(1 − ΔEcc) + C·ΔSNRW + D·ΔStars + P` with each Δ
/// normalized to the group range (math reference §1.5).
fn formula_weight(ch: &ChannelMeasurement, f: &FormulaWeights, r: &FormulaRanges) -> f64 {
    f.fwhm * r.fwhm.credit_down(ch.fwhm_px)
        + f.eccentricity * r.ecc.credit_down(ch.eccentricity)
        + f.snr * r.snr.credit_up(ch.snr_weight)
        + f.stars * r.stars.credit_up(ch.stars_fitted as f64)
        + f.pedestal
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// Weights for one group. `excluded[i]` marks frames that must not set the
/// per-channel maximum (manual exclusions); their weights are still
/// computed. Non-finite or non-positive raw weights become 0.
pub fn compute_weights(
    inputs: &[WeightInput],
    mode: WeightMode,
    formula: &FormulaWeights,
    excluded: &[bool],
) -> Vec<FrameWeight> {
    let nch = inputs.iter().map(|i| i.measurement.channels.len()).max().unwrap_or(0);
    let ranges = (mode == WeightMode::Formula).then(|| formula_ranges(inputs, excluded, nch));
    let mut out: Vec<FrameWeight> = inputs
        .iter()
        .map(|inp| {
            let mut missing = None;
            let channels: Vec<f64> = (0..nch)
                .map(|c| {
                    let ch = match inp.measurement.channels.get(c) {
                        Some(ch) => ch,
                        None => return 0.0,
                    };
                    let w = match mode {
                        WeightMode::PsfSignalWeight => ch.psf_signal_weight,
                        WeightMode::PsfSnr => ch.psf_snr,
                        WeightMode::Noise => {
                            let ns = 0.5 * (ch.noise_scale_low + ch.noise_scale_high);
                            if ch.noise > 0.0 {
                                (ns / ch.noise).powi(2)
                            } else {
                                0.0
                            }
                        }
                        WeightMode::Formula => formula_weight(ch, formula, &ranges.as_ref().unwrap()[c]),
                        WeightMode::Exposure => match inp.exposure_s {
                            Some(e) if e > 0.0 => e,
                            _ => {
                                missing = Some("no exposure time".to_string());
                                0.0
                            }
                        },
                        WeightMode::Keyword => match inp.keyword_value {
                            Some(k) if k.is_finite() => k,
                            _ => {
                                missing = Some("no weight keyword value".to_string());
                                0.0
                            }
                        },
                        WeightMode::None => 1.0,
                    };
                    if w.is_finite() && w > 0.0 {
                        w
                    } else {
                        0.0
                    }
                })
                .collect();
            FrameWeight { mean: mean(&channels), channels, normalized: Vec::new(), normalized_mean: 0.0, missing }
        })
        .collect();
    for c in 0..nch {
        let max = out
            .iter()
            .enumerate()
            .filter(|(i, w)| !excluded.get(*i).copied().unwrap_or(false) && w.missing.is_none())
            .map(|(_, w)| w.channels[c])
            .fold(0.0f64, f64::max);
        for w in &mut out {
            w.normalized.push(if max > 0.0 { w.channels[c] / max } else { 0.0 });
        }
    }
    for w in &mut out {
        w.normalized_mean = mean(&w.normalized);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionConfig {
    /// Frames below this fraction of the group's maximum weight are excluded.
    pub min_weight_fraction: f64,
    pub max_fwhm_px: Option<f64>,
    pub max_eccentricity: Option<f64>,
    pub min_stars: Option<usize>,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        SelectionConfig { min_weight_fraction: 0.05, max_fwhm_px: None, max_eccentricity: None, min_stars: None }
    }
}

/// One exclusion reason per frame (`None` = included), checked in the
/// spec's order: manual exclusion, missing weight input, the weight gate,
/// then the optional FWHM / eccentricity / star-count filters (frame-level:
/// channel means for FWHM and eccentricity, the channel minimum for stars).
pub fn select_frames(
    inputs: &[WeightInput],
    weights: &[FrameWeight],
    manual_excluded: &[bool],
    cfg: &SelectionConfig,
) -> Vec<Option<String>> {
    inputs
        .iter()
        .enumerate()
        .map(|(i, inp)| {
            if manual_excluded.get(i).copied().unwrap_or(false) {
                return Some("excluded manually".to_string());
            }
            let w = &weights[i];
            if let Some(m) = &w.missing {
                return Some(m.clone());
            }
            if w.normalized_mean < cfg.min_weight_fraction {
                return Some(format!(
                    "weight {:.3} below {:.2} of the group maximum",
                    w.normalized_mean, cfg.min_weight_fraction
                ));
            }
            let m = inp.measurement;
            if let Some(max) = cfg.max_fwhm_px {
                let f = m.mean_fwhm_px();
                if f > max {
                    return Some(format!("FWHM {f:.2} px above {max:.2}"));
                }
            }
            if let Some(max) = cfg.max_eccentricity {
                let e = m.mean_eccentricity();
                if e > max {
                    return Some(format!("eccentricity {e:.2} above {max:.2}"));
                }
            }
            if let Some(min) = cfg.min_stars {
                let s = m.min_stars();
                if s < min {
                    return Some(format!("{s} stars below {min}"));
                }
            }
            None
        })
        .collect()
}

/// The included frame with the highest normalized mean weight, ties broken
/// by star count (spec §4.4 — the caller restricts this to one group).
pub fn best_by_weight(weights: &[FrameWeight], included: &[bool], star_counts: &[usize]) -> Option<usize> {
    (0..weights.len())
        .filter(|&i| included.get(i).copied().unwrap_or(false))
        .max_by(|&a, &b| {
            weights[a]
                .normalized_mean
                .total_cmp(&weights[b].normalized_mean)
                .then(star_counts.get(a).cmp(&star_counts.get(b)))
        })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::weights` → 8 pass.
Run: `cargo test -p athenaeum-core stacking::` → all stacking tests pass.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `rustfmt crates/athenaeum-core/src/stacking/weights.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/weights.rs crates/athenaeum-core/src/stacking/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): weight modes, frame selection with reasons and best-by-weight" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 8: `measure_probe` example, module map, logging dictionary

**Files:**
- Create: `crates/athenaeum-core/examples/measure_probe.rs`
- Modify: `crates/athenaeum-core/Cargo.toml` — after the existing `[[example]] name = "band_profile"` block add:

```toml
[[example]]
name = "measure_probe"
required-features = ["render", "solver"]
```

- Modify: `CLAUDE.md` line 53 (the `**athenaeum-core** … Top-level domains:` line): append `, \`geometry\`, \`resample\`, \`integration\`, \`stacking\`` after `` `rustafits_processor` `` (before the final period).
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` — insert a new paragraph directly before the line beginning `**Dictionary note (fix round 2, 2026-09-06):**`:

```markdown
**Dictionary extension (stacking measurement, `stacking::measure`, 2026-09-09):** `plane` (usize; the 0-based plane index of a multi-plane calibrated frame on the `"frame plane measured"` debug), `stars_detected` / `stars_fitted` (usize; detections handed to the PSF fitter and fits accepted, same event), `beta` (f64; the Moffat β the plane was fitted with), `fwhm_px` / `eccentricity` (f64; the residual-weighted frame FWHM in pixels and eccentricity), `noise` (f64; the plane's noise σ in native [0, 1] units — MRS, or the background-residual scale when MRS was unavailable, which the `"MRS noise unavailable; using the background residual scale"` warn announces with `n_star` (f64; that scale, in the estimator's ADU-scaled units)), `psf_signal_weight` / `psf_snr` (f64; the two PSF-signal estimators, math reference §1.2). Reuses `path` and `duration_ms` (the plane's measurement time) on that debug.
```

**Interfaces:**
- Consumes: `athenaeum_core::stacking::measure::{measure_frame, MeasureOptions}`, `athenaeum_core::stacking::psf_signal::PsfModel`, `serde_json`.

- [ ] **Step 1: Write the example**

```rust
//! Dev probe: measure one calibrated FITS frame and print its
//! `FrameMeasurement` as JSON.
//!
//! `cargo run --release -p athenaeum-core --example measure_probe -- <file.fits> [auto|moffat4]`

use std::path::Path;
use std::sync::atomic::AtomicBool;

use athenaeum_core::stacking::measure::{measure_frame, MeasureOptions};
use athenaeum_core::stacking::psf_signal::PsfModel;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: measure_probe <file.fits> [auto|moffat4]");
            std::process::exit(2);
        }
    };
    let model = match args.next().as_deref() {
        Some("moffat4") => PsfModel::Moffat4,
        _ => PsfModel::Auto,
    };
    let opts = MeasureOptions { psf_model: model, ..MeasureOptions::default() };
    match measure_frame(Path::new(&path), &opts, None, &AtomicBool::new(false)) {
        Ok(m) => println!("{}", serde_json::to_string_pretty(&m).expect("serializable")),
        Err(e) => {
            eprintln!("measure failed: {e}");
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 2: Build it and run it on a synthetic file**

Run: `cargo build -p athenaeum-core --example measure_probe` → builds.
Run: `cargo test -p athenaeum-core stacking::measure::tests::measures_a_synthetic_mono_frame -- --nocapture` (to confirm the test fixture path still works) — optional; the example itself needs a FITS file: create one with a 5-line throwaway `cargo test`-style snippet is not required. Instead run `cargo run -p athenaeum-core --example measure_probe` with no argument and confirm the usage line and exit code 2:

```bash
cargo run -p athenaeum-core --example measure_probe; echo "exit=$?"
```
Expected: `usage: measure_probe <file.fits> [auto|moffat4]` and `exit=2`.

- [ ] **Step 3: Make the doc edits** (CLAUDE.md module line; the logging dictionary paragraph) exactly as specified above.

- [ ] **Step 4: Full gate**

Run: `cargo test -p athenaeum-core` → everything green (lib tests ≥ 1807 + the new ones; the pre-existing 12 ignored stay ignored).
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `cargo check --workspace --all-targets` → no new warnings from `athenaeum-core`.
Run: `rustfmt crates/athenaeum-core/examples/measure_probe.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/examples/measure_probe.rs crates/athenaeum-core/Cargo.toml CLAUDE.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "chore(stacking): measure_probe example, module map and logging dictionary rows" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

## Self-review (done while writing)

**Spec coverage.** §4.1 metrics: detection + PSF fits (Task 4), hybrid flux at FWTM (4), RCR + Winsorized means (3, 5), `M*`/`N*` (5), MRS σ_N (5), FWHM weighted by residual + eccentricity + star count (5), median / MAD / `MedianMeanDev` / classic `SNRWeight` (6). §4.2 all seven weight modes with per-channel normalization and the channel mean (7); the `noise` mode's factors (2 + 6). §4.3 selection order and reasons (7); the group gate (< 3 frames) and registration failures are Plan 5 orchestration. §4.4 best-by-weight with the star-count tie break (7); the "largest group" restriction is the caller's (Plan 5). §5.1 two-sided scales on the 1/16 sample of the calibrated frame, the estimator menu, output and rejection modes as `(scale, offset)` pairs (2, 6). §6.3 `minWeight 0.005` is the engine's floor (Plan 4) — `FrameWeight.normalized` is what it will read. §9.2 names: `psfModel`, `maxStars`, `formula{fwhm,eccentricity,snr,stars,pedestal}`, `weightMode`, `minWeightFraction`, `maxFwhmPx`, `maxEccentricity`, `minStars`, `scaleEstimator`, `output`, `rejection` values — all pinned by serde tests. §10.3 gating (3). §13: "BWMV and MRS against tabulated values" (2, 5), "PSF Signal Weight on the synthetic calibration field" (6, as scaling laws plus the Checkpoint B rank test), "headless build" (every task). Carry-forwards: `PlaneReader` scratch + contract tests and the `source.rs` sentence (1).

**Placeholder scan.** No TBD/TODO; every code step carries the code; the one conditional instruction (θ convention) names the file and the rule.

**Type consistency.** `LocationScale` (Task 2) is what `ChannelMeasurement::location_scale()` returns (6) and `output_pair` consumes; `Seed`/`FitParams`/`PsfModel`/`StarFit` (4) are what `measure_plane` (6) and `signal_totals`/`frame_shape` (5) use; `ChannelMeasurement`/`FrameMeasurement` (6) are what `weights.rs` (7) reads (`psf_signal_weight`, `psf_snr`, `noise`, `noise_scale_low/high`, `snr_weight`, `fwhm_px`, `eccentricity`, `stars_fitted`, `mean_fwhm_px()`, `mean_eccentricity()`, `min_stars()`); `add_noise` (2), `MoffatStar`/`moffat_field` (4) and `gaussian_field` (Plan 1) are the fixtures every later test uses; `SplitMix64::next_f64` exists on `crate::geometry::ransac::SplitMix64`.
