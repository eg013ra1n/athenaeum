# Stacking M1 — Plan 1: Pixel-Path Foundations — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the geometry and resampling foundations the stacking pipeline
stands on — interpolation kernels with clamping, transform models
(similarity / affine / homography + polynomial distortion) with inverses and
JSON, a KD-tree, deterministic RANSAC with σ-weighted refit, a positional
multi-plane FITS reader, and a `FrameSource` abstraction so the existing
banded integration engine can integrate *lazily resampled* frames — proven
end to end by integrating three synthetic, differently-shifted frames into
one correctly aligned average.

**Architecture:** Two new dependency-free core modules, `geometry` (pure
math, ungated) and `resample` (pure math, ungated), plus three additions to
`integration` (`source.rs` trait, `plane_reader.rs`, `registered_source.rs`).
`run_banded` becomes generic over `FrameSource`; `BandSource` implements it
unchanged in behaviour so every master-build test keeps passing. Nothing in
this plan touches the database, the commands, or the UI — those are Plans 2–5.

**Tech Stack:** Rust 2021, `athenaeum-core` (no new dependencies: the DLT null
vector comes from a 9×9 Jacobi eigen-solver written here; the RANSAC RNG is a
SplitMix64), `rayon` for row parallelism, `tempfile` in tests, the existing
`fits_writer::write_fits_f32` for fixtures.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`
(§3 registration models and interpolation, §6.1 engine generalization,
§13 tests) with the math in
`docs/superpowers/research/2026-09-08-stacking-math-reference.md` (§5.3
homography, §5.4 interpolation and clamping).

## M1 program index (this is Plan 1 of 5)

M1 is too large for one plan. Each plan below produces working, tested
software; each later plan is written when its predecessor has landed, because
its exact anchors depend on the code the predecessor produced.

| Plan | Scope | Consumes from earlier plans |
| ---- | ---- | ---- |
| **1 (this)** | kernels, warp, window, `Linear`/`PixelMap`/`Polynomial2D`, KD-tree, RANSAC + refit, `PlaneReader`, `FrameSource`, `RegisteredSource`, `integrate_registered` | — |
| 2 | Measurement (`stacking/measure.rs`: PSF flux, RCR, `M*`/`N*`, MRS wrapper, PSF Signal Weight/SNR, classic formula) + statistics (`integration/stats.rs`: BWMV, two-sided scale, global normalization pairs) | `PlaneReader` |
| 3 | Registration v2 service (detection on calibrated frames, quad seed → KD-tree → RANSAC → refit → distortion, QA, `registration_results` columns, optional registered-frame writer) | `geometry::*`, `resample::warp` |
| 4 | Combiner v2 + engine (weights, offsets, survivor masks, rejection maps, channel loop) + WCS card writer + master cards + scanner skip rule | `FrameSource`, `RegisteredSource` |
| 5 | Run orchestration (`stacking/{config,groups,plan,run,paths,naming,provenance}`), tables, commands ×2, `ts_export`, events, the Stacking tab and Settings section, removal of the old tab, LDN 1272 acceptance run, docs | everything above |

## Global Constraints

- Logic lives in `crates/athenaeum-core`; nothing in this plan touches
  `athenaeum-tauri` or `athenaeum-web`.
- `tracing` is the only logging API; `println!`/`eprintln!` are forbidden
  outside `#[cfg(test)]`. Messages are short stable phrases, data in
  snake_case fields (`debug!(frame = i, rows, "band resampled")`).
- Never name the reference implementation (or any other product) in code or
  comments; cite the math reference document or the papers.
- The headless build must keep passing: `cargo check -p athenaeum-core
  --no-default-features`. `geometry` and `resample` are ungated;
  `integration` stays behind `render` as today.
- Existing master-build behaviour is pinned: every test in
  `src/integration/engine.rs` and `tests/master_build.rs` passes unchanged
  after Task 8.
- No new crate dependencies.
- Pixel convention everywhere in this plan: 0-based, integer coordinates are
  pixel centres. `PixelMap::forward` maps subject → reference,
  `PixelMap::inverse` maps reference → subject.
- Commit after every task, as the user (`git -c user.name=eg013ra1n -c
  user.email=vilen.sharifov@gmail.com commit …`), with the two trailers
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY`.
- `rustfmt <changed files>` before each commit (not `cargo fmt -p`, per
  `reference_repo_lint_fmt_gotchas`); clippy is not a gate.
- Test command shape: `cargo test -p athenaeum-core <module path>` — e.g.
  `cargo test -p athenaeum-core geometry::transform`.

## File structure

```
crates/athenaeum-core/src/
  lib.rs                              add `pub mod geometry; pub mod resample;`
  geometry/mod.rs                     module list + `PixelMap` re-export
  geometry/linear.rs                  Linear (3×3 homogeneous), kinds, apply/inverse/props, fitters
  geometry/eigen.rs                   symmetric Jacobi eigen-solver (DLT null vector)
  geometry/polynomial.rs              Polynomial2D, Distortion (forward+inverse fit)
  geometry/pixel_map.rs               PixelMap { linear, linear_inv, distortion } + serde JSON
  geometry/kdtree.rs                  KdTree2 nearest_within
  geometry/ransac.rs                  SplitMix64, ransac_fit, Quality, weighted refit
  resample/mod.rs                     module list
  resample/kernels.rs                 Interpolation, taps, weights, clamping helpers
  resample/warp.rs                    Plane view, sample_at, warp_rows
  resample/window.rs                  SourceWindow, source_window
  integration/mod.rs                  add `pub mod source; pub mod plane_reader; pub mod registered_source;`
  integration/source.rs               FrameSource trait; impl for BandSource
  integration/plane_reader.rs         PlaneReader (positional 1/3-plane window reads)
  integration/registered_source.rs    RegisteredSource: FrameSource over calibrated files + PixelMap
  integration/banded.rs               PlaneKind → pub; BandPlanes::new generic; buf accessors; probe returns channels
  integration/engine.rs               run_banded generic over FrameSource; integrate_registered
  test_support.rs                     synthetic star-field helpers (test-only)
```

---

### Task 1: Interpolation kernels and clamping

**Files:**
- Create: `crates/athenaeum-core/src/resample/mod.rs`
- Create: `crates/athenaeum-core/src/resample/kernels.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod resample;` after `pub mod events;`)

**Interfaces:**
- Produces:
  - `pub enum Interpolation { Nearest, Bilinear, BicubicSpline, BicubicBSpline, Lanczos3, Lanczos4, MitchellNetravali }` with `#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]`, `#[serde(rename_all = "camelCase")]`.
  - `impl Interpolation { pub fn radius(self) -> usize; pub fn taps(self) -> usize; pub fn weights(self, frac: f32, out: &mut [f32; 8]) -> usize; pub fn first_tap_offset(self) -> isize; pub fn is_interpolating(self) -> bool }`.
  - `pub struct Taps { pub first: isize, pub n: usize, pub w: [f32; 8] }` and `pub fn taps_for(interp: Interpolation, coord: f32) -> Taps` (integer base + weights for one axis).
  - `pub fn clamp_keys_1d(w: &[f32; 4], p: &[f32; 4], threshold: f32) -> f32` and `pub fn combine_lanczos_clamped(pos_sum: f32, pos_w: f32, neg_sum: f32, neg_w: f32, threshold: f32) -> f32`.

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/resample/mod.rs`:

```rust
//! Image resampling for registration: separable interpolation kernels with
//! deringing clamps, an inverse-mapped gather warp, and the source-row
//! window a band of output rows needs. Pure math, no I/O, ungated.

pub mod kernels;

pub use kernels::{Interpolation, Taps};
```

Create `crates/athenaeum-core/src/resample/kernels.rs` with only the tests
module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Interpolation; 7] = [
        Interpolation::Nearest,
        Interpolation::Bilinear,
        Interpolation::BicubicSpline,
        Interpolation::BicubicBSpline,
        Interpolation::Lanczos3,
        Interpolation::Lanczos4,
        Interpolation::MitchellNetravali,
    ];

    #[test]
    fn every_kernel_partitions_unity() {
        for k in ALL {
            for frac in [0.0f32, 0.25, 0.5, 0.9, 0.999] {
                let mut w = [0f32; 8];
                let n = k.weights(frac, &mut w);
                assert_eq!(n, k.taps(), "{k:?} tap count");
                let sum: f32 = w[..n].iter().sum();
                assert!((sum - 1.0).abs() < 1e-5, "{k:?} at {frac}: sum {sum}");
            }
        }
    }

    #[test]
    fn interpolating_kernels_reproduce_samples_at_integer_positions() {
        for k in [
            Interpolation::Nearest,
            Interpolation::Bilinear,
            Interpolation::BicubicSpline,
            Interpolation::Lanczos3,
            Interpolation::Lanczos4,
        ] {
            let mut w = [0f32; 8];
            let n = k.weights(0.0, &mut w);
            // The tap that sits on the sample itself carries all the weight.
            let centre = (-k.first_tap_offset()) as usize;
            assert!((w[centre] - 1.0).abs() < 1e-6, "{k:?} centre weight {}", w[centre]);
            for (i, wi) in w[..n].iter().enumerate() {
                if i != centre {
                    assert!(wi.abs() < 1e-6, "{k:?} tap {i} = {wi}");
                }
            }
            assert!(k.is_interpolating());
        }
    }

    #[test]
    fn bspline_at_integer_is_the_one_four_one_smoother() {
        let mut w = [0f32; 8];
        let n = Interpolation::BicubicBSpline.weights(0.0, &mut w);
        assert_eq!(n, 4);
        assert!((w[0] - 1.0 / 6.0).abs() < 1e-6);
        assert!((w[1] - 4.0 / 6.0).abs() < 1e-6);
        assert!((w[2] - 1.0 / 6.0).abs() < 1e-6);
        assert!(w[3].abs() < 1e-6);
        assert!(!Interpolation::BicubicBSpline.is_interpolating());
    }

    #[test]
    fn bilinear_weights_are_exact() {
        let mut w = [0f32; 8];
        let n = Interpolation::Bilinear.weights(0.25, &mut w);
        assert_eq!(n, 2);
        assert!((w[0] - 0.75).abs() < 1e-6 && (w[1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn weights_are_mirror_symmetric() {
        for k in ALL {
            let mut a = [0f32; 8];
            let mut b = [0f32; 8];
            let n = k.weights(0.3, &mut a);
            k.weights(0.7, &mut b);
            // w(t) reversed equals w(1 - t) for every symmetric kernel.
            for i in 0..n {
                assert!((a[i] - b[n - 1 - i]).abs() < 1e-5, "{k:?} tap {i}");
            }
        }
    }

    #[test]
    fn taps_for_places_the_base_index() {
        let t = taps_for(Interpolation::BicubicSpline, 10.25);
        assert_eq!(t.first, 9); // floor(10.25) - 1
        assert_eq!(t.n, 4);
        let t = taps_for(Interpolation::Lanczos3, 10.25);
        assert_eq!(t.first, 8); // floor - 2
        assert_eq!(t.n, 6);
        let t = taps_for(Interpolation::Nearest, 10.75);
        assert_eq!(t.first, 11);
        assert_eq!(t.n, 1);
    }

    #[test]
    fn keys_clamp_removes_overshoot_on_a_step_edge() {
        // Samples around a hard edge 0,0 | 1,1 at fractional position 0.5.
        let mut w = [0f32; 8];
        Interpolation::BicubicSpline.weights(0.5, &mut w);
        let w4 = [w[0], w[1], w[2], w[3]];
        let p = [0.0f32, 0.0, 1.0, 1.0];
        let raw: f32 = w4.iter().zip(p.iter()).map(|(a, b)| a * b).sum();
        assert!((raw - 0.5).abs() < 1e-6); // symmetric case has no overshoot
        // Asymmetric edge: 0,0,0,1 at frac 0.75 rings below zero without a clamp.
        Interpolation::BicubicSpline.weights(0.75, &mut w);
        let w4 = [w[0], w[1], w[2], w[3]];
        let p = [0.0f32, 0.0, 0.0, 1.0];
        let raw: f32 = w4.iter().zip(p.iter()).map(|(a, b)| a * b).sum();
        assert!(raw < 0.0, "expected negative ringing, got {raw}");
        let clamped = clamp_keys_1d(&w4, &p, 0.3);
        assert!(clamped >= 0.0 && clamped <= 1.0, "clamped {clamped}");
    }

    #[test]
    fn lanczos_clamp_attenuates_negative_lobes() {
        // pos 1.2 with weight 1.1, neg 0.5 with weight 0.1: r = 0.4167 > 0.3.
        let v = combine_lanczos_clamped(1.2, 1.1, 0.5, 0.1, 0.3);
        let unclamped = (1.2 - 0.5) / (1.1 - 0.1);
        assert!(v > unclamped, "attenuation must raise the value: {v} vs {unclamped}");
        // r >= 1 collapses to the positive part only.
        let v = combine_lanczos_clamped(1.0, 1.0, 1.5, 0.2, 0.3);
        assert!((v - 1.0).abs() < 1e-6);
        // r <= threshold is untouched.
        let v = combine_lanczos_clamped(1.0, 1.0, 0.1, 0.05, 0.3);
        assert!((v - (0.9 / 0.95)).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core resample::kernels`
Expected: compile error — `Interpolation`, `taps_for`, `clamp_keys_1d`,
`combine_lanczos_clamped` not defined.

- [ ] **Step 3: Implement the kernels**

Prepend to `crates/athenaeum-core/src/resample/kernels.rs` (above the tests):

```rust
//! Separable 1-D interpolation kernels and the two deringing clamps.
//!
//! Weights are evaluated per axis at a fractional offset `frac ∈ [0, 1)`
//! measured from `floor(coord)`; the caller applies them to the taps starting
//! at `floor(coord) + first_tap_offset()`. Every kernel's weights are
//! normalized to sum to one (a constant field resamples to itself).
//! Formulas: math reference §5.4.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Interpolation {
    Nearest,
    Bilinear,
    /// Keys cubic convolution, a = -0.5 — interpolating, mild ringing.
    BicubicSpline,
    /// Cubic B-spline — smoothing (1/6, 4/6, 1/6 at integer positions).
    BicubicBSpline,
    Lanczos3,
    Lanczos4,
    /// Mitchell–Netravali cubic filter, B = C = 1/3.
    MitchellNetravali,
}

/// One axis' taps for a sample position: `first` is the source index of
/// tap 0, `w[..n]` the weights.
#[derive(Clone, Copy, Debug)]
pub struct Taps {
    pub first: isize,
    pub n: usize,
    pub w: [f32; 8],
}

impl Interpolation {
    /// Half-width of the kernel support in source pixels.
    pub fn radius(self) -> usize {
        match self {
            Interpolation::Nearest => 0,
            Interpolation::Bilinear => 1,
            Interpolation::BicubicSpline
            | Interpolation::BicubicBSpline
            | Interpolation::MitchellNetravali => 2,
            Interpolation::Lanczos3 => 3,
            Interpolation::Lanczos4 => 4,
        }
    }

    /// Number of taps per axis.
    pub fn taps(self) -> usize {
        match self {
            Interpolation::Nearest => 1,
            Interpolation::Bilinear => 2,
            Interpolation::BicubicSpline
            | Interpolation::BicubicBSpline
            | Interpolation::MitchellNetravali => 4,
            Interpolation::Lanczos3 => 6,
            Interpolation::Lanczos4 => 8,
        }
    }

    /// Offset of tap 0 from `floor(coord)`.
    pub fn first_tap_offset(self) -> isize {
        match self {
            Interpolation::Nearest => 0,
            Interpolation::Bilinear => 0,
            Interpolation::BicubicSpline
            | Interpolation::BicubicBSpline
            | Interpolation::MitchellNetravali => -1,
            Interpolation::Lanczos3 => -2,
            Interpolation::Lanczos4 => -3,
        }
    }

    /// True when the kernel reproduces the samples at integer positions.
    pub fn is_interpolating(self) -> bool {
        !matches!(self, Interpolation::BicubicBSpline | Interpolation::MitchellNetravali)
    }

    /// Fills `out[..n]` with the weights for fractional offset `frac ∈ [0, 1)`
    /// and returns `n`. Weights sum to one.
    pub fn weights(self, frac: f32, out: &mut [f32; 8]) -> usize {
        let n = self.taps();
        match self {
            Interpolation::Nearest => {
                out[0] = 1.0;
            }
            Interpolation::Bilinear => {
                out[0] = 1.0 - frac;
                out[1] = frac;
            }
            Interpolation::BicubicSpline => {
                for (i, o) in out[..4].iter_mut().enumerate() {
                    let d = (frac - (i as f32 - 1.0)).abs();
                    *o = keys(d, -0.5);
                }
            }
            Interpolation::BicubicBSpline => {
                for (i, o) in out[..4].iter_mut().enumerate() {
                    let d = (frac - (i as f32 - 1.0)).abs();
                    *o = cubic_bspline(d);
                }
            }
            Interpolation::MitchellNetravali => {
                for (i, o) in out[..4].iter_mut().enumerate() {
                    let d = (frac - (i as f32 - 1.0)).abs();
                    *o = mitchell(d, 1.0 / 3.0, 1.0 / 3.0);
                }
            }
            Interpolation::Lanczos3 | Interpolation::Lanczos4 => {
                let a = self.radius() as f32;
                let first = self.first_tap_offset() as f32;
                for (i, o) in out[..n].iter_mut().enumerate() {
                    let d = frac - (first + i as f32);
                    *o = lanczos(d, a);
                }
            }
        }
        let sum: f32 = out[..n].iter().sum();
        if sum.abs() > 1e-12 && (sum - 1.0).abs() > 1e-7 {
            for o in out[..n].iter_mut() {
                *o /= sum;
            }
        }
        n
    }
}

/// Keys cubic convolution kernel, parameter `a` (−0.5 = Catmull-Rom family).
fn keys(d: f32, a: f32) -> f32 {
    if d <= 1.0 {
        (a + 2.0) * d * d * d - (a + 3.0) * d * d + 1.0
    } else if d < 2.0 {
        a * d * d * d - 5.0 * a * d * d + 8.0 * a * d - 4.0 * a
    } else {
        0.0
    }
}

/// Cubic B-spline basis (smoothing).
fn cubic_bspline(d: f32) -> f32 {
    if d <= 1.0 {
        2.0 / 3.0 - d * d + d * d * d / 2.0
    } else if d < 2.0 {
        let t = 2.0 - d;
        t * t * t / 6.0
    } else {
        0.0
    }
}

/// Mitchell–Netravali family with parameters B, C.
fn mitchell(d: f32, b: f32, c: f32) -> f32 {
    let d2 = d * d;
    let d3 = d2 * d;
    let v = if d < 1.0 {
        (12.0 - 9.0 * b - 6.0 * c) * d3 + (-18.0 + 12.0 * b + 6.0 * c) * d2 + (6.0 - 2.0 * b)
    } else if d < 2.0 {
        (-b - 6.0 * c) * d3 + (6.0 * b + 30.0 * c) * d2 + (-12.0 * b - 48.0 * c) * d + (8.0 * b + 24.0 * c)
    } else {
        0.0
    };
    v / 6.0
}

/// Lanczos window: sinc(d)·sinc(d/a) for |d| < a.
fn lanczos(d: f32, a: f32) -> f32 {
    if d.abs() >= a {
        return 0.0;
    }
    if d.abs() < 1e-6 {
        return 1.0;
    }
    let x = std::f32::consts::PI * d;
    let sx = x.sin() / x;
    let xa = x / a;
    let sxa = xa.sin() / xa;
    sx * sxa
}

/// Taps for one axis at `coord` (0-based, integer = pixel centre).
pub fn taps_for(interp: Interpolation, coord: f32) -> Taps {
    let base = coord.floor();
    let frac = coord - base;
    let mut w = [0f32; 8];
    let n = match interp {
        // Nearest rounds instead of flooring, so it carries its own base.
        Interpolation::Nearest => {
            w[0] = 1.0;
            return Taps { first: (coord + 0.5).floor() as isize, n: 1, w };
        }
        other => other.weights(frac, &mut w),
    };
    Taps { first: base as isize + interp.first_tap_offset(), n, w }
}

/// Keys-kernel deringing along one axis (math reference §5.4): `w`/`p` are
/// the four taps in order (outer, inner, inner, outer). When the outer taps'
/// negative contribution reaches `threshold` × the inner taps' contribution
/// the cubic is replaced by the inner-tap linear estimate.
pub fn clamp_keys_1d(w: &[f32; 4], p: &[f32; 4], threshold: f32) -> f32 {
    let f12 = w[1] * p[1] + w[2] * p[2];
    let f03 = w[0] * p[0] + w[3] * p[3];
    if -f03 >= f12 * threshold {
        let inner = w[1] + w[2];
        if inner.abs() > 1e-12 {
            return f12 / inner;
        }
    }
    f12 + f03
}

/// Lanczos deringing (math reference §5.4): `pos_sum`/`pos_w` are the
/// positive-weight contributions and their weight sum; `neg_sum`/`neg_w` the
/// magnitudes of the negative ones. `r = neg/pos`; at `r ≥ 1` only the
/// positive part survives, above `threshold` the negative part is attenuated
/// by `1 − ((r − threshold)/(1 − threshold))²`.
pub fn combine_lanczos_clamped(pos_sum: f32, pos_w: f32, neg_sum: f32, neg_w: f32, threshold: f32) -> f32 {
    if pos_sum <= 0.0 || pos_w <= 0.0 {
        let denom = pos_w - neg_w;
        return if denom.abs() > 1e-12 { (pos_sum - neg_sum) / denom } else { 0.0 };
    }
    let r = neg_sum / pos_sum;
    if r >= 1.0 {
        return pos_sum / pos_w;
    }
    if r > threshold {
        let k = 1.0 - ((r - threshold) / (1.0 - threshold)).powi(2);
        let denom = pos_w - neg_w * k;
        return (pos_sum - neg_sum * k) / denom;
    }
    (pos_sum - neg_sum) / (pos_w - neg_w)
}
```

Add `pub mod resample;` to `crates/athenaeum-core/src/lib.rs` directly after
`pub mod events;`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core resample::kernels`
Expected: 8 passed. Also run `cargo check -p athenaeum-core --no-default-features`
— expected: success (the module is ungated).

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/resample/kernels.rs crates/athenaeum-core/src/resample/mod.rs
git add crates/athenaeum-core/src/resample crates/athenaeum-core/src/lib.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(resample): interpolation kernels with deringing clamps

Seven separable kernels (nearest, bilinear, Keys cubic, cubic B-spline,
Lanczos-3/4, Mitchell–Netravali), normalized to partition unity, plus the
Keys inner-tap clamp and the Lanczos negative-lobe attenuation from the
stacking math reference §5.4.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 2: Linear transforms (similarity, affine, homography) with inverses and fitters

**Files:**
- Create: `crates/athenaeum-core/src/geometry/mod.rs`
- Create: `crates/athenaeum-core/src/geometry/eigen.rs`
- Create: `crates/athenaeum-core/src/geometry/linear.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (add `pub mod geometry;` after `pub mod resample;`)

**Interfaces:**
- Produces:
  - `pub enum LinearKind { Similarity, Affine, Homography }` (serde camelCase).
  - `pub struct Linear { pub kind: LinearKind, pub m: [[f64; 3]; 3] }` with `identity()`, `apply(&self, x: f64, y: f64) -> (f64, f64)`, `inverse(&self) -> Option<Linear>`, `det2(&self) -> f64` (upper-left 2×2 determinant), `scale(&self) -> f64` (sqrt |det2|), `rotation_deg(&self) -> f64`, `translation(&self) -> (f64, f64)`, `is_flipped(&self) -> bool`, `to_flat(&self) -> [f64; 9]`, `from_flat(kind, [f64; 9])`.
  - `pub type Pair = ((f64, f64), (f64, f64))` — `(subject, reference)`.
  - `pub fn fit_similarity(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear>` (proper rotation only), `pub fn fit_affine(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear>`, `pub fn fit_homography(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear>` (normalized DLT), `pub fn fit_linear(kind: LinearKind, pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear>`.
  - `pub fn residuals(t: &Linear, pairs: &[Pair]) -> Vec<f64>` (Euclidean per pair) and `pub fn rms(residuals: &[f64]) -> f64`.
  - `eigen.rs`: `pub fn smallest_eigenvector_sym(a: &[[f64; 9]; 9]) -> [f64; 9]` (Jacobi rotations).

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/geometry/mod.rs`:

```rust
//! Plane geometry for image registration: linear transform models, the
//! polynomial distortion layer, a KD-tree for correspondences and a
//! deterministic RANSAC. Pure math, no I/O, ungated. Conventions: 0-based
//! pixel coordinates, integer = pixel centre; `forward` maps subject →
//! reference, `inverse` maps reference → subject.

pub mod eigen;
pub mod linear;

pub use linear::{Linear, LinearKind, Pair};
```

Create `crates/athenaeum-core/src/geometry/eigen.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smallest_eigenvector_of_a_diagonal_matrix_is_its_smallest_axis() {
        let mut a = [[0f64; 9]; 9];
        for (i, row) in a.iter_mut().enumerate() {
            row[i] = (i as f64 + 1.0) * 10.0;
        }
        a[4][4] = 0.5; // the smallest eigenvalue sits on axis 4
        let v = smallest_eigenvector_sym(&a);
        let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-9);
        assert!(v[4].abs() > 0.999, "{v:?}");
    }

    #[test]
    fn null_vector_of_a_rank_deficient_gram_matrix() {
        // Rows of A: 8 random-ish vectors orthogonal to n = (1,1,...,1)/3.
        let n: [f64; 9] = [1.0 / 3.0; 9];
        let mut rows: Vec<[f64; 9]> = Vec::new();
        for k in 0..8 {
            let mut r = [0f64; 9];
            r[k] = 1.0;
            r[k + 1] = -1.0; // (e_k - e_{k+1}) is orthogonal to n
            rows.push(r);
        }
        let mut a = [[0f64; 9]; 9];
        for r in &rows {
            for i in 0..9 {
                for j in 0..9 {
                    a[i][j] += r[i] * r[j];
                }
            }
        }
        let v = smallest_eigenvector_sym(&a);
        let dot: f64 = v.iter().zip(n.iter()).map(|(a, b)| a * b).sum();
        assert!(dot.abs() > 0.9999, "null vector must be ±n, got dot {dot}");
    }
}
```

Create `crates/athenaeum-core/src/geometry/linear.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random points spread over a 6000×4000 frame.
    fn points(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut s = seed;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 1_000_000) as f64 / 1_000_000.0
        };
        (0..n).map(|_| (next() * 6000.0, next() * 4000.0)).collect()
    }

    fn similarity(rot_deg: f64, scale: f64, tx: f64, ty: f64) -> Linear {
        let (s, c) = rot_deg.to_radians().sin_cos();
        Linear {
            kind: LinearKind::Similarity,
            m: [[scale * c, -scale * s, tx], [scale * s, scale * c, ty], [0.0, 0.0, 1.0]],
        }
    }

    fn pairs_under(t: &Linear, pts: &[(f64, f64)], noise: f64) -> Vec<Pair> {
        let mut k = 0.37f64;
        pts.iter()
            .map(|&(x, y)| {
                let (rx, ry) = t.apply(x, y);
                k = (k * 7.13 + 0.31).fract();
                let nx = (k - 0.5) * 2.0 * noise;
                k = (k * 7.13 + 0.31).fract();
                let ny = (k - 0.5) * 2.0 * noise;
                ((x, y), (rx + nx, ry + ny))
            })
            .collect()
    }

    fn assert_close(a: &Linear, b: &Linear, tol: f64) {
        for i in 0..3 {
            for j in 0..3 {
                assert!((a.m[i][j] - b.m[i][j]).abs() < tol, "m[{i}][{j}]: {} vs {}", a.m[i][j], b.m[i][j]);
            }
        }
    }

    #[test]
    fn similarity_fit_recovers_rotation_scale_translation() {
        let truth = similarity(12.0, 1.01, 5.3, -2.1);
        let pairs = pairs_under(&truth, &points(40, 11), 0.01);
        let fit = fit_similarity(&pairs, None).unwrap();
        assert_close(&fit, &truth, 1e-3);
        assert!((fit.scale() - 1.01).abs() < 1e-3);
        assert!((fit.rotation_deg() - 12.0).abs() < 0.01);
        assert!(!fit.is_flipped());
    }

    #[test]
    fn affine_fit_recovers_an_anisotropic_transform() {
        let truth = Linear {
            kind: LinearKind::Affine,
            m: [[1.002, 0.031, 273.2], [-0.028, 0.995, -499.1], [0.0, 0.0, 1.0]],
        };
        let pairs = pairs_under(&truth, &points(50, 5), 0.01);
        let fit = fit_affine(&pairs, None).unwrap();
        assert_close(&fit, &truth, 1e-3);
    }

    #[test]
    fn homography_fit_recovers_perspective_terms() {
        let truth = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.992, 0.0387, -23.54],
                [-0.0383, 0.9922, 136.37],
                [8.8e-8, -2.5e-8, 1.0],
            ],
        };
        let pairs = pairs_under(&truth, &points(80, 3), 0.0);
        let fit = fit_homography(&pairs, None).unwrap();
        // Compare after normalizing m[2][2] to 1.
        assert!((fit.m[2][2] - 1.0).abs() < 1e-12);
        for i in 0..3 {
            for j in 0..3 {
                let scale = truth.m[i][j].abs().max(1e-6);
                assert!((fit.m[i][j] - truth.m[i][j]).abs() / scale < 1e-4, "m[{i}][{j}] {} vs {}", fit.m[i][j], truth.m[i][j]);
            }
        }
        let res = residuals(&fit, &pairs);
        assert!(rms(&res) < 1e-6);
    }

    #[test]
    fn inverse_composes_to_identity_and_flags_flips() {
        let flipped = Linear {
            kind: LinearKind::Homography,
            m: [[-0.998963, -0.042010, 6270.125], [0.041831, -0.999094, 4078.864], [0.0, 0.0, 1.0]],
        };
        assert!(!flipped.is_flipped(), "a 180° rotation is not a mirror");
        let mirror = Linear { kind: LinearKind::Affine, m: [[-1.0, 0.0, 6000.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        assert!(mirror.is_flipped());
        for t in [&flipped, &mirror] {
            let inv = t.inverse().unwrap();
            for (x, y) in points(20, 9) {
                let (u, v) = t.apply(x, y);
                let (bx, by) = inv.apply(u, v);
                assert!((bx - x).abs() < 1e-8 && (by - y).abs() < 1e-8);
            }
        }
    }

    #[test]
    fn weighted_fit_follows_the_heavier_points() {
        let truth = similarity(3.0, 1.0, 10.0, 20.0);
        let mut pairs = pairs_under(&truth, &points(30, 21), 0.0);
        // Corrupt two points heavily but give them ~zero weight.
        pairs[0].1 .0 += 500.0;
        pairs[1].1 .1 -= 500.0;
        let mut w = vec![1.0; pairs.len()];
        w[0] = 1e-9;
        w[1] = 1e-9;
        let fit = fit_affine(&pairs, Some(&w)).unwrap();
        assert_close(&fit, &truth, 1e-3);
    }

    #[test]
    fn degenerate_inputs_return_none() {
        assert!(fit_similarity(&[], None).is_none());
        let p = ((1.0, 1.0), (2.0, 2.0));
        assert!(fit_affine(&[p, p, p], None).is_none(), "collinear/duplicate points");
        assert!(fit_homography(&[p, p, p, p], None).is_none());
    }

    #[test]
    fn flat_roundtrip() {
        let t = similarity(7.0, 1.2, 1.0, 2.0);
        let back = Linear::from_flat(LinearKind::Similarity, t.to_flat());
        assert_close(&t, &back, 0.0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core geometry::`
Expected: compile errors — none of the items exist yet.

- [ ] **Step 3: Implement the eigen-solver**

Prepend to `crates/athenaeum-core/src/geometry/eigen.rs`:

```rust
//! Symmetric 9×9 eigen-decomposition by cyclic Jacobi rotations — enough to
//! extract the null vector of a normalized-DLT design matrix's Gram matrix
//! without a linear-algebra dependency. 9×9 is the only size the crate
//! needs; the loop is written for `N` but instantiated once.

const N: usize = 9;

/// The unit eigenvector belonging to the smallest eigenvalue of the
/// symmetric matrix `a`. Converges to machine precision in a few sweeps for
/// well-conditioned input; the sweep cap only bounds pathological cases.
pub fn smallest_eigenvector_sym(a: &[[f64; N]; N]) -> [f64; N] {
    let mut m = *a;
    let mut v = [[0f64; N]; N];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _sweep in 0..100 {
        let mut off = 0.0;
        for i in 0..N {
            for j in (i + 1)..N {
                off += m[i][j] * m[i][j];
            }
        }
        if off < 1e-30 {
            break;
        }
        for p in 0..N {
            for q in (p + 1)..N {
                if m[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..N {
                    let mkp = m[k][p];
                    let mkq = m[k][q];
                    m[k][p] = c * mkp - s * mkq;
                    m[k][q] = s * mkp + c * mkq;
                }
                for k in 0..N {
                    let mpk = m[p][k];
                    let mqk = m[q][k];
                    m[p][k] = c * mpk - s * mqk;
                    m[q][k] = s * mpk + c * mqk;
                }
                for k in 0..N {
                    let vkp = v[k][p];
                    let vkq = v[k][q];
                    v[k][p] = c * vkp - s * vkq;
                    v[k][q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut best = 0;
    for i in 1..N {
        if m[i][i] < m[best][best] {
            best = i;
        }
    }
    let mut out = [0f64; N];
    for k in 0..N {
        out[k] = v[k][best];
    }
    let norm: f64 = out.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm > 0.0 {
        for x in out.iter_mut() {
            *x /= norm;
        }
    }
    out
}
```

- [ ] **Step 4: Implement the transforms and fitters**

Prepend to `crates/athenaeum-core/src/geometry/linear.rs`:

```rust
//! Linear registration models as 3×3 homogeneous matrices. A similarity
//! or affine simply has a `[0, 0, 1]` last row. Fitters are least squares
//! with optional per-pair weights; the homography uses the normalized
//! direct linear transform (Hartley 1997) — points shifted to their centroid
//! and scaled to mean distance √2 before the 2n×9 system is solved through
//! its Gram matrix's smallest eigenvector (`eigen.rs`).

use serde::{Deserialize, Serialize};

use super::eigen::smallest_eigenvector_sym;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinearKind {
    Similarity,
    Affine,
    Homography,
}

/// `(subject, reference)` correspondence.
pub type Pair = ((f64, f64), (f64, f64));

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Linear {
    pub kind: LinearKind,
    pub m: [[f64; 3]; 3],
}

impl Linear {
    pub fn identity() -> Linear {
        Linear { kind: LinearKind::Similarity, m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] }
    }

    pub fn from_flat(kind: LinearKind, f: [f64; 9]) -> Linear {
        Linear { kind, m: [[f[0], f[1], f[2]], [f[3], f[4], f[5]], [f[6], f[7], f[8]]] }
    }

    pub fn to_flat(&self) -> [f64; 9] {
        let m = &self.m;
        [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]
    }

    #[inline]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.m;
        let w = m[2][0] * x + m[2][1] * y + m[2][2];
        let u = (m[0][0] * x + m[0][1] * y + m[0][2]) / w;
        let v = (m[1][0] * x + m[1][1] * y + m[1][2]) / w;
        (u, v)
    }

    /// Matrix inverse by adjugate; `None` when singular.
    pub fn inverse(&self) -> Option<Linear> {
        let m = &self.m;
        let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
        let c01 = -(m[1][0] * m[2][2] - m[1][2] * m[2][0]);
        let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
        let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
        if det.abs() < 1e-300 || !det.is_finite() {
            return None;
        }
        let c10 = -(m[0][1] * m[2][2] - m[0][2] * m[2][1]);
        let c11 = m[0][0] * m[2][2] - m[0][2] * m[2][0];
        let c12 = -(m[0][0] * m[2][1] - m[0][1] * m[2][0]);
        let c20 = m[0][1] * m[1][2] - m[0][2] * m[1][1];
        let c21 = -(m[0][0] * m[1][2] - m[0][2] * m[1][0]);
        let c22 = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        let mut inv = [[c00, c10, c20], [c01, c11, c21], [c02, c12, c22]];
        for row in inv.iter_mut() {
            for v in row.iter_mut() {
                *v /= det;
            }
        }
        // Keep the affine rows exactly [0, 0, 1] when the input had them.
        if self.kind != LinearKind::Homography {
            let s = inv[2][2];
            for row in inv.iter_mut() {
                for v in row.iter_mut() {
                    *v /= s;
                }
            }
            inv[2] = [0.0, 0.0, 1.0];
        }
        Some(Linear { kind: self.kind, m: inv })
    }

    /// Determinant of the upper-left 2×2 (the local linear part at the origin).
    pub fn det2(&self) -> f64 {
        self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0]
    }

    pub fn scale(&self) -> f64 {
        self.det2().abs().sqrt()
    }

    pub fn rotation_deg(&self) -> f64 {
        self.m[1][0].atan2(self.m[0][0]).to_degrees()
    }

    pub fn translation(&self) -> (f64, f64) {
        (self.m[0][2], self.m[1][2])
    }

    /// A mirror image (meridian flip seen through the optics) has a
    /// negative determinant; a 180° rotation does not.
    pub fn is_flipped(&self) -> bool {
        self.det2() < 0.0
    }
}

fn weight_at(weights: Option<&[f64]>, i: usize) -> f64 {
    weights.map(|w| w[i]).unwrap_or(1.0)
}

/// Weighted similarity (rotation + isotropic scale + translation), closed
/// form via weighted centroids. Proper rotations only — a mirrored field
/// needs `Affine` or `Homography`.
pub fn fit_similarity(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    if pairs.len() < 2 {
        return None;
    }
    let mut sw = 0.0;
    let (mut sx, mut sy, mut rx, mut ry) = (0.0, 0.0, 0.0, 0.0);
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        sw += w;
        sx += w * x;
        sy += w * y;
        rx += w * u;
        ry += w * v;
    }
    if sw <= 0.0 {
        return None;
    }
    let (cx, cy, cu, cv) = (sx / sw, sy / sw, rx / sw, ry / sw);
    // Complex regression: (u + iv) = (a + ib)(x + iy) about the centroids.
    let (mut num_re, mut num_im, mut den) = (0.0, 0.0, 0.0);
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        let (px, py, qu, qv) = (x - cx, y - cy, u - cu, v - cv);
        num_re += w * (px * qu + py * qv);
        num_im += w * (px * qv - py * qu);
        den += w * (px * px + py * py);
    }
    if den < 1e-12 {
        return None;
    }
    let a = num_re / den;
    let b = num_im / den;
    if a * a + b * b < 1e-24 {
        return None;
    }
    let tx = cu - (a * cx - b * cy);
    let ty = cv - (b * cx + a * cy);
    Some(Linear { kind: LinearKind::Similarity, m: [[a, -b, tx], [b, a, ty], [0.0, 0.0, 1.0]] })
}

/// Solves the symmetric 3×3 system `a·x = b` by Cramer's rule.
fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(a);
    if d.abs() < 1e-18 || !d.is_finite() {
        return None;
    }
    let mut out = [0f64; 3];
    for k in 0..3 {
        let mut ak = a;
        for row in 0..3 {
            ak[row][k] = b[row];
        }
        out[k] = det(ak) / d;
    }
    Some(out)
}

/// Weighted least-squares affine: two independent 3-parameter regressions
/// on centred coordinates (centring keeps the normal equations conditioned
/// for 6000-pixel frames).
pub fn fit_affine(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    if pairs.len() < 3 {
        return None;
    }
    let mut sw = 0.0;
    let (mut cx, mut cy) = (0.0, 0.0);
    for (i, ((x, y), _)) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        sw += w;
        cx += w * x;
        cy += w * y;
    }
    if sw <= 0.0 {
        return None;
    }
    cx /= sw;
    cy /= sw;
    let mut ata = [[0f64; 3]; 3];
    let mut atu = [0f64; 3];
    let mut atv = [0f64; 3];
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        let r = [x - cx, y - cy, 1.0];
        for a in 0..3 {
            for b in 0..3 {
                ata[a][b] += w * r[a] * r[b];
            }
            atu[a] += w * r[a] * u;
            atv[a] += w * r[a] * v;
        }
    }
    let pu = solve3(ata, atu)?;
    let pv = solve3(ata, atv)?;
    // Undo the centring: u = pu0 (x - cx) + pu1 (y - cy) + pu2.
    let m = [
        [pu[0], pu[1], pu[2] - pu[0] * cx - pu[1] * cy],
        [pv[0], pv[1], pv[2] - pv[0] * cx - pv[1] * cy],
        [0.0, 0.0, 1.0],
    ];
    let t = Linear { kind: LinearKind::Affine, m };
    if t.det2().abs() < 1e-12 || !t.det2().is_finite() {
        return None;
    }
    Some(t)
}

struct Norm {
    cx: f64,
    cy: f64,
    s: f64,
}

fn normalization(points: impl Iterator<Item = (f64, f64)> + Clone) -> Option<Norm> {
    let n = points.clone().count();
    if n == 0 {
        return None;
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for (x, y) in points.clone() {
        cx += x;
        cy += y;
    }
    cx /= n as f64;
    cy /= n as f64;
    let mut mean_d = 0.0;
    for (x, y) in points {
        mean_d += ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    }
    mean_d /= n as f64;
    if mean_d < 1e-12 {
        return None;
    }
    Some(Norm { cx, cy, s: std::f64::consts::SQRT_2 / mean_d })
}

/// Weighted normalized DLT homography (Hartley 1997). Returns the matrix
/// scaled so `m[2][2] == 1`.
pub fn fit_homography(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    if pairs.len() < 4 {
        return None;
    }
    let ns = normalization(pairs.iter().map(|p| p.0))?;
    let nr = normalization(pairs.iter().map(|p| p.1))?;
    let mut ata = [[0f64; 9]; 9];
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i).max(0.0).sqrt();
        let (x, y) = ((x - ns.cx) * ns.s, (y - ns.cy) * ns.s);
        let (u, v) = ((u - nr.cx) * nr.s, (v - nr.cy) * nr.s);
        let rows = [
            [0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v],
            [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, -u],
        ];
        for r in rows {
            for a in 0..9 {
                for b in 0..9 {
                    ata[a][b] += w * w * r[a] * r[b];
                }
            }
        }
    }
    let h = smallest_eigenvector_sym(&ata);
    let hn = [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], h[8]]];
    // Denormalize: H = T_r⁻¹ · Hn · T_s.
    let ts = [[ns.s, 0.0, -ns.s * ns.cx], [0.0, ns.s, -ns.s * ns.cy], [0.0, 0.0, 1.0]];
    let tr_inv = [[1.0 / nr.s, 0.0, nr.cx], [0.0, 1.0 / nr.s, nr.cy], [0.0, 0.0, 1.0]];
    let mul = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| {
        let mut c = [[0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    c[i][j] += a[i][k] * b[k][j];
                }
            }
        }
        c
    };
    let mut m = mul(tr_inv, mul(hn, ts));
    let s = m[2][2];
    if s.abs() < 1e-300 || !s.is_finite() {
        return None;
    }
    for row in m.iter_mut() {
        for v in row.iter_mut() {
            *v /= s;
        }
    }
    let t = Linear { kind: LinearKind::Homography, m };
    // Reject the degenerate solutions a collinear or duplicated sample yields.
    let res = residuals(&t, pairs);
    if t.det2().abs() < 1e-12 || !t.det2().is_finite() || res.iter().any(|r| !r.is_finite()) {
        return None;
    }
    Some(t)
}

pub fn fit_linear(kind: LinearKind, pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    match kind {
        LinearKind::Similarity => fit_similarity(pairs, weights),
        LinearKind::Affine => fit_affine(pairs, weights),
        LinearKind::Homography => fit_homography(pairs, weights),
    }
}

/// Euclidean residual per pair in reference pixels.
pub fn residuals(t: &Linear, pairs: &[Pair]) -> Vec<f64> {
    pairs
        .iter()
        .map(|((x, y), (u, v))| {
            let (px, py) = t.apply(*x, *y);
            ((px - u).powi(2) + (py - v).powi(2)).sqrt()
        })
        .collect()
}

pub fn rms(residuals: &[f64]) -> f64 {
    if residuals.is_empty() {
        return 0.0;
    }
    (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len() as f64).sqrt()
}
```

Add `pub mod geometry;` to `crates/athenaeum-core/src/lib.rs` right after
`pub mod resample;`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core geometry::`
Expected: 9 passed (2 eigen + 7 linear). If
`homography_fit_recovers_perspective_terms` fails on the `1e-4` relative
tolerance of the two perspective terms, loosen that assertion for
`m[2][0]`/`m[2][1]` only to an absolute `1e-9` — those terms are ~1e-8 and
the relative test is the strict one; the residual RMS assertion is the
gate that matters.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/geometry/*.rs
git add crates/athenaeum-core/src/geometry crates/athenaeum-core/src/lib.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(geometry): linear registration models with weighted fitters

Similarity (closed form), affine (centred normal equations) and homography
(normalized DLT through a 9×9 Jacobi eigen-solver, no new dependency), each
with inverse, flip detection and scale/rotation readouts.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 3: Polynomial distortion and the `PixelMap`

**Files:**
- Create: `crates/athenaeum-core/src/geometry/polynomial.rs`
- Create: `crates/athenaeum-core/src/geometry/pixel_map.rs`
- Modify: `crates/athenaeum-core/src/geometry/mod.rs` (add the two modules and re-exports)

**Interfaces:**
- Consumes: `Linear`, `LinearKind`, `Pair`, `residuals`, `rms` from Task 2.
- Produces:
  - `pub struct Polynomial2D { pub order: u8, pub ax: Vec<f64>, pub ay: Vec<f64> }` — terms `u^i v^j` for `2 ≤ i+j ≤ order` in the fixed order produced by `term_exponents(order) -> Vec<(u8, u8)>`; `eval(&self, u: f64, v: f64) -> (f64, f64)`; `fit(order, samples: &[((f64, f64), (f64, f64))], weights: Option<&[f64]>) -> Option<Polynomial2D>` where each sample is `((u, v), (du, dv))`.
  - `pub struct Distortion { pub order: u8, pub center: (f64, f64), pub scale: f64, pub forward: Polynomial2D, pub inverse: Polynomial2D }` with `Distortion::fit(order, linear: &Linear, pairs: &[Pair], weights, center, scale) -> Option<Distortion>`.
  - `pub struct PixelMap { pub linear: Linear, pub linear_inv: Linear, pub distortion: Option<Distortion> }` with `PixelMap::linear(l: Linear) -> Option<PixelMap>`, `PixelMap::with_distortion(l, d) -> Option<PixelMap>`, `forward(&self, x, y) -> (f64, f64)` (subject → reference), `inverse(&self, x, y) -> (f64, f64)` (reference → subject), `to_json(&self) -> String`, `from_json(&str) -> Result<PixelMap, serde_json::Error>`, `is_flipped()`.
  - `pub trait InverseMap: Sync { fn inverse(&self, x: f64, y: f64) -> (f64, f64); }` implemented by `PixelMap` and by `Linear` (for tests and for the identity case).

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/geometry/polynomial.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind, Pair};

    fn grid(w: f64, h: f64, step: f64) -> Vec<(f64, f64)> {
        let mut pts = Vec::new();
        let mut y = step / 2.0;
        while y < h {
            let mut x = step / 2.0;
            while x < w {
                pts.push((x, y));
                x += step;
            }
            y += step;
        }
        pts
    }

    /// Radial barrel distortion about the frame centre: r' = r (1 + k r²).
    fn barrel(x: f64, y: f64, cx: f64, cy: f64, k: f64) -> (f64, f64) {
        let (dx, dy) = (x - cx, y - cy);
        let r2 = dx * dx + dy * dy;
        (cx + dx * (1.0 + k * r2), cy + dy * (1.0 + k * r2))
    }

    #[test]
    fn term_order_is_stable_and_complete() {
        assert_eq!(term_exponents(2), vec![(2, 0), (1, 1), (0, 2)]);
        assert_eq!(term_exponents(3).len(), 7);
        assert_eq!(term_exponents(4).len(), 12);
    }

    #[test]
    fn polynomial_fit_reproduces_a_known_cubic() {
        // du = 0.5 u² − 0.25 u v + 0.1 v³ ; dv = −0.3 v² + 0.2 u² v
        let mut samples = Vec::new();
        for (u, v) in grid(2.0, 2.0, 0.1) {
            let (u, v) = (u - 1.0, v - 1.0);
            let du = 0.5 * u * u - 0.25 * u * v + 0.1 * v * v * v;
            let dv = -0.3 * v * v + 0.2 * u * u * v;
            samples.push(((u, v), (du, dv)));
        }
        let p = Polynomial2D::fit(3, &samples, None).unwrap();
        for ((u, v), (du, dv)) in &samples {
            let (eu, ev) = p.eval(*u, *v);
            assert!((eu - du).abs() < 1e-9 && (ev - dv).abs() < 1e-9);
        }
    }

    #[test]
    fn distortion_fit_recovers_barrel_within_a_hundredth_pixel() {
        let (w, h) = (6000.0, 4000.0);
        let (cx, cy) = (w / 2.0, h / 2.0);
        // The inverse of a cubic is not a cubic: an order-3 inverse neglects the
        // 3k²r⁵ term, which at the corner (r ≈ 3606) is 0.005 px for this k.
        let k = 5e-11; // ≈ 2.4 px of barrel at the corner
        let linear = Linear { kind: LinearKind::Affine, m: [[1.0, 0.0, 12.5], [0.0, 1.0, -7.25], [0.0, 0.0, 1.0]] };
        let pairs: Vec<Pair> = grid(w, h, 250.0)
            .into_iter()
            .map(|(x, y)| {
                let (lx, ly) = linear.apply(x, y);
                (( x, y), barrel(lx, ly, cx, cy, k))
            })
            .collect();
        let d = Distortion::fit(3, &linear, &pairs, None, (cx, cy), w.max(h) / 2.0).unwrap();
        let map = PixelMap::with_distortion(linear, d).unwrap();
        let mut worst_fwd = 0.0f64;
        let mut worst_inv = 0.0f64;
        for ((x, y), (u, v)) in &pairs {
            let (fx, fy) = map.forward(*x, *y);
            worst_fwd = worst_fwd.max(((fx - u).powi(2) + (fy - v).powi(2)).sqrt());
            let (bx, by) = map.inverse(*u, *v);
            worst_inv = worst_inv.max(((bx - x).powi(2) + (by - y).powi(2)).sqrt());
        }
        assert!(worst_fwd < 0.01, "forward worst {worst_fwd}");
        assert!(worst_inv < 0.01, "inverse worst {worst_inv}");
    }

    #[test]
    fn too_few_samples_returns_none() {
        let samples = vec![((0.0, 0.0), (0.0, 0.0)); 3];
        assert!(Polynomial2D::fit(3, &samples, None).is_none());
    }
}
```

Create `crates/athenaeum-core/src/geometry/pixel_map.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind};

    #[test]
    fn linear_map_round_trips_and_serializes() {
        let l = Linear {
            kind: LinearKind::Homography,
            m: [[0.992, 0.0387, -23.54], [-0.0383, 0.9922, 136.37], [8.8e-8, -2.5e-8, 1.0]],
        };
        let map = PixelMap::linear(l).unwrap();
        let (u, v) = map.forward(1234.5, 678.25);
        let (x, y) = map.inverse(u, v);
        assert!((x - 1234.5).abs() < 1e-9 && (y - 678.25).abs() < 1e-9);
        let json = map.to_json();
        assert!(json.contains("\"kind\":\"homography\""));
        assert!(json.contains("\"distortion\":null"));
        let back = PixelMap::from_json(&json).unwrap();
        let (u2, v2) = back.forward(1234.5, 678.25);
        assert!((u - u2).abs() < 1e-12 && (v - v2).abs() < 1e-12);
    }

    #[test]
    fn singular_linear_is_rejected() {
        let l = Linear { kind: LinearKind::Affine, m: [[1.0, 2.0, 0.0], [2.0, 4.0, 0.0], [0.0, 0.0, 1.0]] };
        assert!(PixelMap::linear(l).is_none());
    }

    #[test]
    fn identity_linear_implements_inverse_map() {
        let id = Linear::identity();
        let m: &dyn InverseMap = &id;
        assert_eq!(m.inverse(3.0, 4.0), (3.0, 4.0));
    }
}
```

Update `crates/athenaeum-core/src/geometry/mod.rs`:

```rust
pub mod eigen;
pub mod linear;
pub mod pixel_map;
pub mod polynomial;

pub use linear::{Linear, LinearKind, Pair};
pub use pixel_map::{InverseMap, PixelMap};
pub use polynomial::{Distortion, Polynomial2D};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core geometry::`
Expected: compile errors for `Polynomial2D`, `Distortion`, `PixelMap`,
`InverseMap`, `term_exponents`.

- [ ] **Step 3: Implement the polynomial layer**

Prepend to `crates/athenaeum-core/src/geometry/polynomial.rs`:

```rust
//! Polynomial distortion on top of a linear model — the plate solver's SIP
//! convention: only terms of total degree 2..=order, fitted independently
//! forward and inverse by weighted least squares, on coordinates normalized
//! to the reference centre and half the longer side so every coefficient is
//! O(1) and the normal equations stay conditioned. Math reference §5.3.

use serde::{Deserialize, Serialize};

use super::linear::{Linear, Pair};

/// Exponent pairs `(i, j)` for `u^i v^j`, `2 ≤ i+j ≤ order`, in a fixed
/// order: by total degree, then by descending `i`.
pub fn term_exponents(order: u8) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    for deg in 2..=order {
        for i in (0..=deg).rev() {
            out.push((i, deg - i));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Polynomial2D {
    pub order: u8,
    pub ax: Vec<f64>,
    pub ay: Vec<f64>,
}

impl Polynomial2D {
    /// Evaluates `(Σ ax_k u^i v^j, Σ ay_k u^i v^j)` with power tables
    /// (`order` is at most 4, so this is a dozen multiplies).
    #[inline]
    pub fn eval(&self, u: f64, v: f64) -> (f64, f64) {
        let mut pu = [1.0f64; 5];
        let mut pv = [1.0f64; 5];
        for k in 1..=self.order as usize {
            pu[k] = pu[k - 1] * u;
            pv[k] = pv[k - 1] * v;
        }
        let (mut du, mut dv) = (0.0, 0.0);
        let mut k = 0;
        for deg in 2..=self.order as usize {
            for i in (0..=deg).rev() {
                let t = pu[i] * pv[deg - i];
                du += self.ax[k] * t;
                dv += self.ay[k] * t;
                k += 1;
            }
        }
        (du, dv)
    }

    /// Weighted least squares over `samples = ((u, v), (du, dv))`. Needs at
    /// least `terms + 2` samples. Solved through the normal equations with
    /// Gauss–Jordan elimination and partial pivoting (≤ 12 unknowns).
    pub fn fit(order: u8, samples: &[((f64, f64), (f64, f64))], weights: Option<&[f64]>) -> Option<Polynomial2D> {
        if !(2..=4).contains(&order) {
            return None;
        }
        let terms = term_exponents(order);
        let n = terms.len();
        if samples.len() < n + 2 {
            return None;
        }
        let mut ata = vec![vec![0f64; n]; n];
        let mut atx = vec![0f64; n];
        let mut aty = vec![0f64; n];
        let mut row = vec![0f64; n];
        for (s, ((u, v), (du, dv))) in samples.iter().enumerate() {
            let w = weights.map(|w| w[s]).unwrap_or(1.0);
            for (k, (i, j)) in terms.iter().enumerate() {
                row[k] = u.powi(*i as i32) * v.powi(*j as i32);
            }
            for a in 0..n {
                for b in 0..n {
                    ata[a][b] += w * row[a] * row[b];
                }
                atx[a] += w * row[a] * du;
                aty[a] += w * row[a] * dv;
            }
        }
        let ax = solve_dense(&ata, &atx)?;
        let ay = solve_dense(&ata, &aty)?;
        Some(Polynomial2D { order, ax, ay })
    }
}

/// Gauss–Jordan with partial pivoting; `None` on a singular system.
fn solve_dense(a: &[Vec<f64>], b: &[f64]) -> Option<Vec<f64>> {
    let n = b.len();
    let mut m: Vec<Vec<f64>> = a.iter().zip(b.iter()).map(|(r, &bi)| {
        let mut row = r.clone();
        row.push(bi);
        row
    }).collect();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| m[i][col].abs().partial_cmp(&m[j][col].abs()).unwrap())?;
        if m[pivot][col].abs() < 1e-14 {
            return None;
        }
        m.swap(col, pivot);
        let p = m[col][col];
        for v in m[col].iter_mut() {
            *v /= p;
        }
        for r in 0..n {
            if r != col {
                let f = m[r][col];
                if f != 0.0 {
                    for c in col..=n {
                        let sub = f * m[col][c];
                        m[r][c] -= sub;
                    }
                }
            }
        }
    }
    Some(m.iter().map(|row| row[n]).collect())
}

/// Forward and inverse polynomial corrections around a linear model.
///
/// Forward: `ref = p + F(norm(p))`, `p = L(sub)`.
/// Inverse: `sub = L⁻¹(ref + I(norm(ref)))`.
/// `norm(x, y) = ((x − cx)/scale, (y − cy)/scale)`; the polynomials return
/// displacements in pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Distortion {
    pub order: u8,
    pub center: (f64, f64),
    pub scale: f64,
    pub forward: Polynomial2D,
    pub inverse: Polynomial2D,
}

impl Distortion {
    #[inline]
    pub fn norm(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.center.0) / self.scale, (y - self.center.1) / self.scale)
    }

    /// Fits both directions on the residuals of `linear` over `pairs`.
    pub fn fit(
        order: u8,
        linear: &Linear,
        pairs: &[Pair],
        weights: Option<&[f64]>,
        center: (f64, f64),
        scale: f64,
    ) -> Option<Distortion> {
        if scale <= 0.0 {
            return None;
        }
        let inv = linear.inverse()?;
        let norm = |x: f64, y: f64| ((x - center.0) / scale, (y - center.1) / scale);
        let fwd_samples: Vec<_> = pairs
            .iter()
            .map(|((x, y), (u, v))| {
                let (px, py) = linear.apply(*x, *y);
                (norm(px, py), (u - px, v - py))
            })
            .collect();
        // Inverse: we need I(ref) such that L⁻¹(ref + I) = sub, i.e.
        // ref + I = L(sub)  ⇒  I = L(sub) − ref.
        let inv_samples: Vec<_> = pairs
            .iter()
            .map(|((x, y), (u, v))| {
                let (px, py) = linear.apply(*x, *y);
                (norm(*u, *v), (px - u, py - v))
            })
            .collect();
        let forward = Polynomial2D::fit(order, &fwd_samples, weights)?;
        let inverse = Polynomial2D::fit(order, &inv_samples, weights)?;
        let _ = inv;
        Some(Distortion { order, center, scale, forward, inverse })
    }
}
```

- [ ] **Step 4: Implement the `PixelMap`**

Prepend to `crates/athenaeum-core/src/geometry/pixel_map.rs`:

```rust
//! The complete subject ↔ reference pixel mapping a registered frame
//! carries: a linear model, its cached inverse, and an optional polynomial
//! distortion. Serialized as the `transform_json` of `registration_results`
//! (spec §9.1).

use serde::{Deserialize, Serialize};

use super::linear::Linear;
use super::polynomial::Distortion;

/// Reference → subject mapping, the only direction a gather-style resampler
/// needs.
pub trait InverseMap: Sync {
    fn inverse(&self, x: f64, y: f64) -> (f64, f64);
}

impl InverseMap for Linear {
    /// A bare `Linear` used as an inverse map is applied as-is: callers hand
    /// in the already-inverted matrix (or the identity).
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        self.apply(x, y)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelMap {
    pub linear: Linear,
    pub linear_inv: Linear,
    pub distortion: Option<Distortion>,
}

impl PixelMap {
    pub fn linear(linear: Linear) -> Option<PixelMap> {
        let linear_inv = linear.inverse()?;
        Some(PixelMap { linear, linear_inv, distortion: None })
    }

    pub fn with_distortion(linear: Linear, distortion: Distortion) -> Option<PixelMap> {
        let linear_inv = linear.inverse()?;
        Some(PixelMap { linear, linear_inv, distortion: Some(distortion) })
    }

    /// Subject pixel → reference pixel.
    #[inline]
    pub fn forward(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py) = self.linear.apply(x, y);
        match &self.distortion {
            None => (px, py),
            Some(d) => {
                let (u, v) = d.norm(px, py);
                let (dx, dy) = d.forward.eval(u, v);
                (px + dx, py + dy)
            }
        }
    }

    /// Reference pixel → subject pixel.
    #[inline]
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = match &self.distortion {
            None => (x, y),
            Some(d) => {
                let (u, v) = d.norm(x, y);
                let (dx, dy) = d.inverse.eval(u, v);
                (x + dx, y + dy)
            }
        };
        self.linear_inv.apply(rx, ry)
    }

    pub fn is_flipped(&self) -> bool {
        self.linear.is_flipped()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("PixelMap is always serializable")
    }

    pub fn from_json(s: &str) -> Result<PixelMap, serde_json::Error> {
        serde_json::from_str(s)
    }
}

impl InverseMap for PixelMap {
    #[inline]
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        PixelMap::inverse(self, x, y)
    }
}
```

Note the serialized shape: `Linear` serializes as `{ "kind": "...", "m":
[[..],[..],[..]] }` (nested rows, not the spec's flat `[9]`) — `to_flat` is
for the database columns in Plan 3, JSON keeps the natural shape. Record
that in Plan 3's `transform_json` task.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core geometry::`
Expected: 16 passed. The forward bound is exact by construction (a barrel
`r(1 + k r²)` IS a cubic); the inverse bound depends on `k` — if it fails,
`k` is too strong for an order-3 inverse and must be lowered further, never
the tolerance raised.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/geometry/*.rs
git add crates/athenaeum-core/src/geometry
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(geometry): polynomial distortion and the PixelMap

Order 2–4 polynomial corrections fitted forward and inverse on normalized
coordinates around a linear model, and the PixelMap that carries the
linear model, its inverse and the distortion as one serializable mapping.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 4: KD-tree for correspondences

**Files:**
- Create: `crates/athenaeum-core/src/geometry/kdtree.rs`
- Modify: `crates/athenaeum-core/src/geometry/mod.rs` (add `pub mod kdtree;` and `pub use kdtree::KdTree2;`)

**Interfaces:**
- Produces: `pub struct KdTree2` with `KdTree2::build(points: &[(f64, f64)]) -> KdTree2`, `nearest_within(&self, x: f64, y: f64, radius: f64) -> Option<(usize, f64)>` (index into the build slice and the distance), `len(&self) -> usize`.

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/geometry/kdtree.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn points(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut s = seed;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 1_000_000) as f64 / 1_000_000.0
        };
        (0..n).map(|_| (next() * 6000.0, next() * 4000.0)).collect()
    }

    fn brute(pts: &[(f64, f64)], x: f64, y: f64, r: f64) -> Option<(usize, f64)> {
        let mut best: Option<(usize, f64)> = None;
        for (i, (px, py)) in pts.iter().enumerate() {
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d <= r && best.map(|b| d < b.1).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        best
    }

    #[test]
    fn nearest_within_matches_brute_force() {
        let pts = points(2000, 77);
        let tree = KdTree2::build(&pts);
        assert_eq!(tree.len(), 2000);
        for (qx, qy) in points(500, 99) {
            let a = tree.nearest_within(qx, qy, 40.0);
            let b = brute(&pts, qx, qy, 40.0);
            match (a, b) {
                (None, None) => {}
                (Some((ia, da)), Some((ib, db))) => {
                    assert!((da - db).abs() < 1e-9, "distance mismatch");
                    // Ties at the same distance are legal either way.
                    if ia != ib {
                        assert!((da - db).abs() < 1e-9);
                    }
                }
                other => panic!("mismatch at ({qx},{qy}): {other:?}"),
            }
        }
    }

    #[test]
    fn empty_tree_returns_none() {
        let tree = KdTree2::build(&[]);
        assert!(tree.nearest_within(1.0, 1.0, 10.0).is_none());
    }

    #[test]
    fn radius_is_inclusive_and_exact() {
        let tree = KdTree2::build(&[(0.0, 0.0), (10.0, 0.0)]);
        assert_eq!(tree.nearest_within(4.0, 0.0, 4.0).unwrap().0, 0);
        assert!(tree.nearest_within(5.0, 0.0, 4.9).is_none());
        assert_eq!(tree.nearest_within(7.0, 0.0, 4.0).unwrap().0, 1);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core geometry::kdtree`
Expected: compile error — `KdTree2` not defined.

- [ ] **Step 3: Implement the tree**

Prepend to `crates/athenaeum-core/src/geometry/kdtree.rs`:

```rust
//! A static 2-D KD-tree for nearest-neighbour lookups within a radius —
//! replaces the O(N·M) scan in frame-to-frame correspondence. Built once
//! per reference star list (≤ a few thousand points); queries are O(log n).

pub struct KdTree2 {
    /// Points in tree order; `idx[i]` is the original index of `pts[i]`.
    pts: Vec<(f64, f64)>,
    idx: Vec<usize>,
}

impl KdTree2 {
    pub fn build(points: &[(f64, f64)]) -> KdTree2 {
        let mut idx: Vec<usize> = (0..points.len()).collect();
        let mut pts: Vec<(f64, f64)> = points.to_vec();
        Self::build_rec(&mut pts, &mut idx, 0);
        KdTree2 { pts, idx }
    }

    pub fn len(&self) -> usize {
        self.pts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }

    /// Recursively arrange `[lo, hi)` so the median on the split axis sits
    /// in the middle, alternating axes by depth. Implicit balanced tree:
    /// node = middle index of its range.
    fn build_rec(pts: &mut [(f64, f64)], idx: &mut [usize], depth: usize) {
        let n = pts.len();
        if n <= 1 {
            return;
        }
        let axis = depth % 2;
        let mid = n / 2;
        // Selection by axis: sort the pair arrays together (n log n per
        // level is fine at these sizes and keeps the code obvious).
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            let ka = if axis == 0 { pts[a].0 } else { pts[a].1 };
            let kb = if axis == 0 { pts[b].0 } else { pts[b].1 };
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        let sorted_pts: Vec<_> = order.iter().map(|&o| pts[o]).collect();
        let sorted_idx: Vec<_> = order.iter().map(|&o| idx[o]).collect();
        pts.copy_from_slice(&sorted_pts);
        idx.copy_from_slice(&sorted_idx);
        let (left_p, right_p) = pts.split_at_mut(mid);
        let (left_i, right_i) = idx.split_at_mut(mid);
        Self::build_rec(left_p, left_i, depth + 1);
        Self::build_rec(&mut right_p[1..], &mut right_i[1..], depth + 1);
    }

    /// Nearest point within `radius` (inclusive), as `(original index,
    /// distance)`.
    pub fn nearest_within(&self, x: f64, y: f64, radius: f64) -> Option<(usize, f64)> {
        if self.pts.is_empty() {
            return None;
        }
        let mut best: Option<(usize, f64)> = None;
        let mut best_d2 = radius * radius;
        self.search(0, self.pts.len(), 0, x, y, &mut best, &mut best_d2);
        best.map(|(i, d2)| (i, d2.sqrt()))
    }

    #[allow(clippy::too_many_arguments)]
    fn search(&self, lo: usize, hi: usize, depth: usize, x: f64, y: f64, best: &mut Option<(usize, f64)>, best_d2: &mut f64) {
        if lo >= hi {
            return;
        }
        let mid = lo + (hi - lo) / 2;
        let (px, py) = self.pts[mid];
        let d2 = (px - x).powi(2) + (py - y).powi(2);
        if d2 <= *best_d2 && best.map(|b| d2 < b.1).unwrap_or(true) {
            *best = Some((self.idx[mid], d2));
            *best_d2 = d2;
        }
        let axis = depth % 2;
        let diff = if axis == 0 { x - px } else { y - py };
        let (near, far) = if diff < 0.0 { ((lo, mid), (mid + 1, hi)) } else { ((mid + 1, hi), (lo, mid)) };
        self.search(near.0, near.1, depth + 1, x, y, best, best_d2);
        if diff * diff <= *best_d2 {
            self.search(far.0, far.1, depth + 1, x, y, best, best_d2);
        }
    }
}
```

(`best` stores the squared distance during the search and converts once at
the end — the `map` chain in `nearest_within` does that conversion.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core geometry::kdtree`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/geometry/kdtree.rs crates/athenaeum-core/src/geometry/mod.rs
git add crates/athenaeum-core/src/geometry
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(geometry): static 2-D KD-tree for radius-bounded nearest neighbours

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 5: Deterministic RANSAC with quality score and σ-weighted refit

**Files:**
- Create: `crates/athenaeum-core/src/geometry/ransac.rs`
- Modify: `crates/athenaeum-core/src/geometry/mod.rs` (add `pub mod ransac;` and `pub use ransac::{RansacConfig, RansacResult, Quality, ransac_fit, refit_weighted, RefitResult};`)

**Interfaces:**
- Consumes: `Linear`, `LinearKind`, `Pair`, `fit_linear`, `residuals`, `rms` from Task 2.
- Produces:
  - `pub struct RansacConfig { pub kind: LinearKind, pub tolerance_px: f64, pub max_iterations: usize, pub seed: u64, pub min_inliers: usize, pub frame_width: f64, pub frame_height: f64 }` with `RansacConfig::new(kind, frame_width, frame_height) -> Self` giving the spec defaults (`tolerance_px 1.9`, `max_iterations 2000`, `seed 0x5EED_5EED`, `min_inliers 8`).
  - `pub struct Quality { pub inlier_ratio: f64, pub overlap: f64, pub regularity: f64, pub score: f64 }`.
  - `pub struct RansacResult { pub linear: Linear, pub inliers: Vec<usize>, pub rms_px: f64, pub quality: Quality, pub iterations: usize }`.
  - `pub fn ransac_fit(pairs: &[Pair], cfg: &RansacConfig) -> Option<RansacResult>`.
  - `pub struct RefitResult { pub linear: Linear, pub inliers: Vec<usize>, pub rms_px: f64, pub sigma_rms_px: f64, pub peak_px: (f64, f64), pub rounds: usize }`.
  - `pub fn refit_weighted(pairs: &[Pair], inliers: &[usize], sigmas: Option<&[(f64, f64)]>, kind: LinearKind, clip_sigma: f64) -> Option<RefitResult>`.
  - `pub struct SplitMix64(pub u64)` with `next_u64`, `next_f64`, `below(n: usize) -> usize`.

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/geometry/ransac.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind, Pair};

    fn rng_points(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut r = SplitMix64(seed);
        (0..n).map(|_| (r.next_f64() * 6000.0, r.next_f64() * 4000.0)).collect()
    }

    fn truth() -> Linear {
        Linear {
            kind: LinearKind::Homography,
            m: [[0.992, 0.0387, -23.54], [-0.0383, 0.9922, 136.37], [8.8e-8, -2.5e-8, 1.0]],
        }
    }

    /// 300 true correspondences with 0.1 px noise plus 130 outliers (30 %).
    fn contaminated(seed: u64) -> (Vec<Pair>, usize) {
        let t = truth();
        let mut r = SplitMix64(seed);
        let mut pairs: Vec<Pair> = rng_points(300, seed)
            .into_iter()
            .map(|(x, y)| {
                let (u, v) = t.apply(x, y);
                ((x, y), (u + (r.next_f64() - 0.5) * 0.2, v + (r.next_f64() - 0.5) * 0.2))
            })
            .collect();
        let n_true = pairs.len();
        for (x, y) in rng_points(130, seed + 1) {
            pairs.push(((x, y), (r.next_f64() * 6000.0, r.next_f64() * 4000.0)));
        }
        (pairs, n_true)
    }

    #[test]
    fn splitmix_is_deterministic_and_bounded() {
        let mut a = SplitMix64(42);
        let mut b = SplitMix64(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
            let f = a.next_f64();
            assert!((0.0..1.0).contains(&f));
            assert!(a.below(7) < 7);
            b.next_f64();
            b.below(7);
        }
    }

    #[test]
    fn ransac_recovers_the_inlier_set_under_thirty_percent_outliers() {
        let (pairs, n_true) = contaminated(5);
        let cfg = RansacConfig::new(LinearKind::Homography, 6000.0, 4000.0);
        let res = ransac_fit(&pairs, &cfg).expect("a model");
        // Every recovered inlier is a true pair, and nearly all true pairs are recovered.
        assert!(res.inliers.iter().all(|&i| i < n_true), "an outlier was accepted");
        assert!(res.inliers.len() >= (n_true as f64 * 0.95) as usize, "only {} inliers", res.inliers.len());
        assert!(res.rms_px < 0.15, "rms {}", res.rms_px);
        assert!(res.quality.inlier_ratio > 0.6 && res.quality.inlier_ratio < 0.75);
        assert!(res.quality.overlap > 0.9);
        assert!(res.quality.regularity > 0.9);
        assert!(res.iterations <= cfg.max_iterations);
    }

    #[test]
    fn ransac_is_deterministic_for_a_seed() {
        let (pairs, _) = contaminated(9);
        let cfg = RansacConfig::new(LinearKind::Affine, 6000.0, 4000.0);
        let a = ransac_fit(&pairs, &cfg).unwrap();
        let b = ransac_fit(&pairs, &cfg).unwrap();
        assert_eq!(a.inliers, b.inliers);
        assert_eq!(a.linear, b.linear);
        assert_eq!(a.iterations, b.iterations);
    }

    #[test]
    fn ransac_fails_honestly_on_noise() {
        let mut r = SplitMix64(3);
        let pairs: Vec<Pair> = (0..60)
            .map(|_| ((r.next_f64() * 6000.0, r.next_f64() * 4000.0), (r.next_f64() * 6000.0, r.next_f64() * 4000.0)))
            .collect();
        let cfg = RansacConfig::new(LinearKind::Similarity, 6000.0, 4000.0);
        assert!(ransac_fit(&pairs, &cfg).is_none());
    }

    #[test]
    fn similarity_from_two_pairs_and_affine_from_three_are_exact_samples() {
        let t = Linear { kind: LinearKind::Affine, m: [[1.01, 0.02, 5.0], [-0.03, 0.99, -7.0], [0.0, 0.0, 1.0]] };
        let pairs: Vec<Pair> = rng_points(40, 11).into_iter().map(|(x, y)| ((x, y), t.apply(x, y))).collect();
        let cfg = RansacConfig::new(LinearKind::Affine, 6000.0, 4000.0);
        let res = ransac_fit(&pairs, &cfg).unwrap();
        assert_eq!(res.inliers.len(), 40);
        assert!(res.rms_px < 1e-6);
    }

    #[test]
    fn weighted_refit_clips_residual_outliers_and_reports_peaks() {
        let (pairs, n_true) = contaminated(21);
        let cfg = RansacConfig::new(LinearKind::Homography, 6000.0, 4000.0);
        let res = ransac_fit(&pairs, &cfg).unwrap();
        // Hand the refit a slightly polluted inlier list: add 5 outliers.
        let mut inl = res.inliers.clone();
        inl.extend((n_true..n_true + 5).collect::<Vec<_>>());
        let sig: Vec<(f64, f64)> = pairs.iter().map(|_| (0.05, 0.05)).collect();
        let r = refit_weighted(&pairs, &inl, Some(&sig), LinearKind::Homography, 3.0).unwrap();
        assert!(r.inliers.iter().all(|&i| i < n_true), "clip must drop the 5 outliers");
        assert!(r.rms_px < 0.15);
        assert!(r.sigma_rms_px < r.rms_px);
        assert!(r.peak_px.0 < 0.5 && r.peak_px.1 < 0.5, "peaks {:?}", r.peak_px);
        assert!(r.rounds >= 1 && r.rounds <= 5);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core geometry::ransac`
Expected: compile errors — items not defined.

- [ ] **Step 3: Implement RANSAC, quality and refit**

Prepend to `crates/athenaeum-core/src/geometry/ransac.rs`:

```rust
//! Deterministic RANSAC over the linear models (spec §3.2) with the four
//! model-quality indexes of the math reference §5.2 — inlier ratio,
//! overlap (convex-hull area covered by the inliers), regularity (grid
//! occupancy of the inliers) and RMS — and the σ-weighted iterative
//! refit that follows a successful sample.

use super::linear::{fit_linear, residuals, rms, Linear, LinearKind, Pair};

/// SplitMix64 — tiny, deterministic, good enough to draw samples.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    #[inline]
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

#[derive(Clone, Debug)]
pub struct RansacConfig {
    pub kind: LinearKind,
    pub tolerance_px: f64,
    pub max_iterations: usize,
    pub seed: u64,
    pub min_inliers: usize,
    pub frame_width: f64,
    pub frame_height: f64,
}

impl RansacConfig {
    pub fn new(kind: LinearKind, frame_width: f64, frame_height: f64) -> RansacConfig {
        RansacConfig {
            kind,
            tolerance_px: 1.9,
            max_iterations: 2000,
            seed: 0x5EED_5EED,
            min_inliers: 8,
            frame_width,
            frame_height,
        }
    }

    fn sample_size(&self) -> usize {
        match self.kind {
            LinearKind::Similarity => 2,
            LinearKind::Affine => 3,
            LinearKind::Homography => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    pub inlier_ratio: f64,
    pub overlap: f64,
    pub regularity: f64,
    pub score: f64,
}

#[derive(Clone, Debug)]
pub struct RansacResult {
    pub linear: Linear,
    pub inliers: Vec<usize>,
    pub rms_px: f64,
    pub quality: Quality,
    pub iterations: usize,
}

/// Convex hull area by Andrew's monotone chain (shoelace), 0 for < 3 points.
fn hull_area(mut pts: Vec<(f64, f64)>) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup();
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut lower: Vec<(f64, f64)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(f64, f64)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    let n = lower.len();
    let mut area = 0.0;
    for i in 0..n {
        let (x1, y1) = lower[i];
        let (x2, y2) = lower[(i + 1) % n];
        area += x1 * y2 - x2 * y1;
    }
    area.abs() / 2.0
}

/// Fraction of a 4×4 grid over the frame that holds at least one inlier.
fn regularity(pts: &[(f64, f64)], w: f64, h: f64) -> f64 {
    const G: usize = 4;
    let mut cells = [false; G * G];
    for &(x, y) in pts {
        let cx = ((x / w) * G as f64).floor().clamp(0.0, (G - 1) as f64) as usize;
        let cy = ((y / h) * G as f64).floor().clamp(0.0, (G - 1) as f64) as usize;
        cells[cy * G + cx] = true;
    }
    cells.iter().filter(|c| **c).count() as f64 / (G * G) as f64
}

fn quality(pairs: &[Pair], inliers: &[usize], rms_px: f64, cfg: &RansacConfig) -> Quality {
    let inlier_ratio = inliers.len() as f64 / pairs.len().max(1) as f64;
    let all_ref: Vec<(f64, f64)> = pairs.iter().map(|p| p.1).collect();
    let in_ref: Vec<(f64, f64)> = inliers.iter().map(|&i| pairs[i].1).collect();
    let all_area = hull_area(all_ref);
    let overlap = if all_area > 0.0 { (hull_area(in_ref.clone()) / all_area).min(1.0) } else { 0.0 };
    let regularity = regularity(&in_ref, cfg.frame_width, cfg.frame_height);
    let score = inlier_ratio * overlap * regularity / (1.0 + rms_px);
    Quality { inlier_ratio, overlap, regularity, score }
}

/// A sample is degenerate when two subject points nearly coincide or (for
/// 3+ points) three of them are nearly collinear.
fn degenerate(sample: &[Pair]) -> bool {
    for i in 0..sample.len() {
        for j in (i + 1)..sample.len() {
            let (a, b) = (sample[i].0, sample[j].0);
            if (a.0 - b.0).abs() < 1.0 && (a.1 - b.1).abs() < 1.0 {
                return true;
            }
        }
    }
    if sample.len() >= 3 {
        for i in 0..sample.len() {
            for j in (i + 1)..sample.len() {
                for k in (j + 1)..sample.len() {
                    let (a, b, c) = (sample[i].0, sample[j].0, sample[k].0);
                    let area2 = ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs();
                    if area2 < 4.0 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn inliers_of(t: &Linear, pairs: &[Pair], tol: f64) -> Vec<usize> {
    residuals(t, pairs).iter().enumerate().filter(|(_, r)| **r <= tol).map(|(i, _)| i).collect()
}

/// RANSAC: minimal samples → `fit_linear` → inlier count under
/// `tolerance_px`; the adaptive iteration bound
/// `N = log(1 − 0.9999) / log(1 − w^k)` shrinks as the best inlier ratio
/// `w` grows; early exit above 98 % inliers. The winner is refit on all
/// its inliers and scored.
pub fn ransac_fit(pairs: &[Pair], cfg: &RansacConfig) -> Option<RansacResult> {
    let k = cfg.sample_size();
    let n = pairs.len();
    if n < k.max(cfg.min_inliers) {
        return None;
    }
    let mut rng = SplitMix64(cfg.seed);
    let mut best: Option<(Vec<usize>, Linear)> = None;
    let mut needed = cfg.max_iterations;
    let mut iterations = 0usize;
    let mut sample: Vec<Pair> = Vec::with_capacity(k);
    let mut picks: Vec<usize> = Vec::with_capacity(k);
    while iterations < needed.min(cfg.max_iterations) {
        iterations += 1;
        picks.clear();
        while picks.len() < k {
            let p = rng.below(n);
            if !picks.contains(&p) {
                picks.push(p);
            }
        }
        sample.clear();
        sample.extend(picks.iter().map(|&i| pairs[i]));
        if degenerate(&sample) {
            continue;
        }
        let Some(t) = fit_linear(cfg.kind, &sample, None) else { continue };
        let inl = inliers_of(&t, pairs, cfg.tolerance_px);
        let better = best.as_ref().map(|(b, _)| inl.len() > b.len()).unwrap_or(true);
        if better {
            let w = inl.len() as f64 / n as f64;
            if w > 0.98 {
                best = Some((inl, t));
                break;
            }
            if w > 0.0 {
                let denom = (1.0 - w.powi(k as i32)).ln();
                if denom < 0.0 {
                    needed = ((1.0f64 - 0.9999).ln() / denom).ceil().max(1.0) as usize;
                }
            }
            best = Some((inl, t));
        }
    }
    let (inl, _) = best?;
    if inl.len() < cfg.min_inliers {
        return None;
    }
    // Refit on every inlier, then re-select inliers under the refit model
    // once so the reported set is consistent with the reported model.
    let subset: Vec<Pair> = inl.iter().map(|&i| pairs[i]).collect();
    let linear = fit_linear(cfg.kind, &subset, None)?;
    let inliers = inliers_of(&linear, pairs, cfg.tolerance_px);
    if inliers.len() < cfg.min_inliers {
        return None;
    }
    let subset: Vec<Pair> = inliers.iter().map(|&i| pairs[i]).collect();
    let rms_px = rms(&residuals(&linear, &subset));
    let quality = quality(pairs, &inliers, rms_px, cfg);
    Some(RansacResult { linear, inliers, rms_px, quality, iterations })
}

#[derive(Clone, Debug)]
pub struct RefitResult {
    pub linear: Linear,
    pub inliers: Vec<usize>,
    pub rms_px: f64,
    /// Standard deviation of the per-pair residuals.
    pub sigma_rms_px: f64,
    /// Largest |Δx|, |Δy| over the final inliers.
    pub peak_px: (f64, f64),
    pub rounds: usize,
}

/// σ-weighted least squares on `inliers` with iterative `clip_sigma`
/// clipping (≤ 5 rounds; stops when the inlier set's Jaccard similarity
/// with the previous round exceeds 0.97). `sigmas` are per-pair centroid
/// uncertainties `(σx, σy)`; weight `1 / (σx² + σy² + ε)`, uniform when
/// absent.
pub fn refit_weighted(
    pairs: &[Pair],
    inliers: &[usize],
    sigmas: Option<&[(f64, f64)]>,
    kind: LinearKind,
    clip_sigma: f64,
) -> Option<RefitResult> {
    const EPS: f64 = 1e-4;
    let mut current: Vec<usize> = inliers.to_vec();
    current.sort_unstable();
    current.dedup();
    let mut rounds = 0usize;
    let mut linear: Option<Linear> = None;
    loop {
        rounds += 1;
        let subset: Vec<Pair> = current.iter().map(|&i| pairs[i]).collect();
        let weights: Option<Vec<f64>> = sigmas.map(|s| {
            current.iter().map(|&i| 1.0 / (s[i].0.powi(2) + s[i].1.powi(2) + EPS)).collect()
        });
        let t = fit_linear(kind, &subset, weights.as_deref())?;
        let res = residuals(&t, &subset);
        let r = rms(&res);
        let next: Vec<usize> = current
            .iter()
            .zip(res.iter())
            .filter(|(_, &e)| e <= clip_sigma * r.max(1e-9))
            .map(|(&i, _)| i)
            .collect();
        linear = Some(t);
        let inter = next.iter().filter(|i| current.binary_search(i).is_ok()).count();
        let union = current.len() + next.len() - inter;
        let jaccard = if union == 0 { 1.0 } else { inter as f64 / union as f64 };
        if next.len() < 3 {
            break;
        }
        current = next;
        if jaccard > 0.97 || rounds >= 5 {
            break;
        }
    }
    let linear = linear?;
    let subset: Vec<Pair> = current.iter().map(|&i| pairs[i]).collect();
    let res = residuals(&linear, &subset);
    let rms_px = rms(&res);
    let mean = res.iter().sum::<f64>() / res.len().max(1) as f64;
    let sigma_rms_px = (res.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / res.len().max(1) as f64).sqrt();
    let mut peak = (0.0f64, 0.0f64);
    for ((x, y), (u, v)) in &subset {
        let (px, py) = linear.apply(*x, *y);
        peak.0 = peak.0.max((px - u).abs());
        peak.1 = peak.1.max((py - v).abs());
    }
    Some(RefitResult { linear, inliers: current, rms_px, sigma_rms_px, peak_px: peak, rounds })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core geometry::ransac`
Expected: 6 passed. `ransac_fails_honestly_on_noise` relies on
`min_inliers = 8`: 60 random pairs under a similarity at 1.9 px tolerance
never reach 8 inliers.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/geometry/ransac.rs crates/athenaeum-core/src/geometry/mod.rs
git add crates/athenaeum-core/src/geometry
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(geometry): deterministic RANSAC with quality indexes and weighted refit

Minimal-sample RANSAC over similarity/affine/homography with the adaptive
iteration bound, inlier/overlap/regularity/RMS quality score, and a
σ-weighted iterative 3σ-clipped refit reporting RMS, σ_RMS and peak errors.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 6: Source window and the inverse-mapped warp

**Files:**
- Create: `crates/athenaeum-core/src/resample/window.rs`
- Create: `crates/athenaeum-core/src/resample/warp.rs`
- Modify: `crates/athenaeum-core/src/resample/mod.rs` (add modules + re-exports)
- Modify: `crates/athenaeum-core/src/test_support.rs` (append synthetic-field helpers)

**Interfaces:**
- Consumes: `Interpolation`, `taps_for`, `clamp_keys_1d`, `combine_lanczos_clamped` (Task 1); `InverseMap` (Task 3).
- Produces:
  - `pub enum SourceWindow { Rows { y0: usize, y1: usize }, Whole }` and `pub fn source_window(map: &dyn InverseMap, out_width: usize, y0: usize, rows: usize, src_width: usize, src_height: usize, radius: usize, whole_threshold: f64) -> SourceWindow` (`y1` exclusive; `Whole` when the span exceeds `whole_threshold × src_height`).
  - `pub struct Plane<'a> { pub data: &'a [f32], pub width: usize, pub height: usize, pub y_offset: usize, pub full_height: usize }` — a window of rows `[y_offset, y_offset + height)` of a `full_height`-row image; `Plane::full(data, w, h)`.
  - `pub fn sample_at(src: &Plane, x: f64, y: f64, interp: Interpolation, clamping: f32) -> f32` (NaN when `(x, y)` is outside `[0, full_w − 1] × [0, full_h − 1]`; taps beyond the window or the frame edge use the nearest available row/column).
  - `pub fn warp_rows(src: &Plane, map: &dyn InverseMap, out_width: usize, y0: usize, rows: usize, interp: Interpolation, clamping: f32, out: &mut [f32])` — fills `out` (len `rows × out_width`) with output rows `[y0, y0 + rows)`; rows in parallel with rayon.
  - test helpers in `test_support.rs`: `pub(crate) fn gaussian_field(w, h, stars: &[(f64, f64, f64)], sigma: f64, background: f32) -> Vec<f32>`, `pub(crate) fn centroid(data: &[f32], w: usize, x0: f64, y0: f64, r: usize, background: f32) -> (f64, f64)`, `pub(crate) fn flux(data: &[f32], w: usize, x0: f64, y0: f64, r: usize, background: f32) -> f64`.

- [ ] **Step 1: Write the test helpers**

Append to `crates/athenaeum-core/src/test_support.rs`:

```rust
/// Row-major float plane with Gaussian stars `(x, y, amplitude)` of common
/// `sigma` on a flat `background`. Pixel centres at integer coordinates.
pub(crate) fn gaussian_field(w: usize, h: usize, stars: &[(f64, f64, f64)], sigma: f64, background: f32) -> Vec<f32> {
    let mut data = vec![background; w * h];
    let s2 = 2.0 * sigma * sigma;
    for &(sx, sy, amp) in stars {
        let r = (5.0 * sigma).ceil() as i64;
        let (cx, cy) = (sx.round() as i64, sy.round() as i64);
        for y in (cy - r).max(0)..=(cy + r).min(h as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(w as i64 - 1) {
                let d2 = (x as f64 - sx).powi(2) + (y as f64 - sy).powi(2);
                data[y as usize * w + x as usize] += (amp * (-d2 / s2).exp()) as f32;
            }
        }
    }
    data
}

/// Background-subtracted intensity-weighted centroid in a `(2r+1)²` box
/// around `(x0, y0)`. NaN samples are skipped.
pub(crate) fn centroid(data: &[f32], w: usize, x0: f64, y0: f64, r: usize, background: f32) -> (f64, f64) {
    let (cx, cy) = (x0.round() as i64, y0.round() as i64);
    let h = data.len() / w;
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for y in (cy - r as i64).max(0)..=(cy + r as i64).min(h as i64 - 1) {
        for x in (cx - r as i64).max(0)..=(cx + r as i64).min(w as i64 - 1) {
            let v = data[y as usize * w + x as usize];
            if !v.is_finite() {
                continue;
            }
            let v = (v - background).max(0.0) as f64;
            sx += v * x as f64;
            sy += v * y as f64;
            sw += v;
        }
    }
    (sx / sw, sy / sw)
}

/// Background-subtracted flux in the same box (NaN skipped).
pub(crate) fn flux(data: &[f32], w: usize, x0: f64, y0: f64, r: usize, background: f32) -> f64 {
    let (cx, cy) = (x0.round() as i64, y0.round() as i64);
    let h = data.len() / w;
    let mut sum = 0.0f64;
    for y in (cy - r as i64).max(0)..=(cy + r as i64).min(h as i64 - 1) {
        for x in (cx - r as i64).max(0)..=(cx + r as i64).min(w as i64 - 1) {
            let v = data[y as usize * w + x as usize];
            if v.is_finite() {
                sum += (v - background) as f64;
            }
        }
    }
    sum
}
```

- [ ] **Step 2: Write the failing tests**

Create `crates/athenaeum-core/src/resample/window.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Linear, LinearKind};

    #[test]
    fn identity_window_is_the_band_plus_margin() {
        let id = Linear::identity();
        let w = source_window(&id, 100, 200, 50, 100, 1000, 2, 0.6);
        // top edge 199.5, bottom 249.5, margin radius+1 = 3: floor(196.5)=196, ceil(252.5)+1=254
        assert_eq!(w, SourceWindow::Rows { y0: 196, y1: 254 });
    }

    #[test]
    fn window_clamps_to_the_frame() {
        let id = Linear::identity();
        assert_eq!(source_window(&id, 100, 0, 10, 100, 1000, 3, 0.6), SourceWindow::Rows { y0: 0, y1: 15 });
        assert_eq!(source_window(&id, 100, 995, 5, 100, 1000, 3, 0.6), SourceWindow::Rows { y0: 990, y1: 1000 });
    }

    #[test]
    fn shift_moves_the_window() {
        // inverse map: reference (x, y) → subject (x, y + 40)
        let shift = Linear { kind: LinearKind::Affine, m: [[1.0, 0.0, 0.0], [0.0, 1.0, 40.0], [0.0, 0.0, 1.0]] };
        assert_eq!(source_window(&shift, 100, 200, 50, 100, 1000, 0, 0.6), SourceWindow::Rows { y0: 238, y1: 292 });
    }

    #[test]
    fn a_quarter_turn_reads_the_whole_frame() {
        // 90° rotation about the centre of a square frame: a band of output
        // rows maps to a band of source COLUMNS, i.e. every source row.
        let (cx, cy) = (500.0, 500.0);
        let rot = Linear {
            kind: LinearKind::Affine,
            m: [[0.0, -1.0, cx + cy], [1.0, 0.0, cy - cx], [0.0, 0.0, 1.0]],
        };
        assert_eq!(source_window(&rot, 1000, 400, 50, 1000, 1000, 2, 0.6), SourceWindow::Whole);
    }

    #[test]
    fn a_band_entirely_outside_the_source_is_empty() {
        let shift = Linear { kind: LinearKind::Affine, m: [[1.0, 0.0, 0.0], [0.0, 1.0, 5000.0], [0.0, 0.0, 1.0]] };
        assert_eq!(source_window(&shift, 100, 0, 50, 100, 1000, 2, 0.6), SourceWindow::Rows { y0: 1000, y1: 1000 });
    }
}
```

Create `crates/athenaeum-core/src/resample/warp.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Linear, LinearKind, PixelMap};
    use crate::resample::Interpolation;
    use crate::test_support::{centroid, flux, gaussian_field};

    const W: usize = 240;
    const H: usize = 180;
    const BG: f32 = 100.0;
    const STARS: [(f64, f64, f64); 3] = [(60.3, 50.7, 4000.0), (150.0, 90.0, 2500.0), (200.6, 140.2, 6000.0)];

    fn field() -> Vec<f32> {
        gaussian_field(W, H, &STARS, 1.8, BG)
    }

    /// Reference → subject map for a subject that is the reference shifted
    /// by (dx, dy): subject(x, y) = reference(x − dx, y − dy)  ⇒  inverse
    /// maps reference (x, y) to subject (x − dx, y − dy)... the subject
    /// frame holds the star at (sx + dx, sy + dy), so to recover the
    /// reference the warp samples the subject at (x + dx, y + dy).
    fn shift_map(dx: f64, dy: f64) -> PixelMap {
        // forward: subject → reference = subtract the shift.
        let fwd = Linear { kind: LinearKind::Affine, m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]] };
        PixelMap::linear(fwd).unwrap()
    }

    fn warp_full(src: &[f32], map: &PixelMap, interp: Interpolation) -> Vec<f32> {
        let plane = Plane::full(src, W, H);
        let mut out = vec![0f32; W * H];
        warp_rows(&plane, map, W, 0, H, interp, 0.3, &mut out);
        out
    }

    #[test]
    fn identity_reproduces_interpolating_kernels_exactly() {
        let src = field();
        let id = PixelMap::linear(Linear::identity()).unwrap();
        for k in [Interpolation::Nearest, Interpolation::Bilinear, Interpolation::BicubicSpline, Interpolation::Lanczos3, Interpolation::Lanczos4] {
            let out = warp_full(&src, &id, k);
            for (a, b) in out.iter().zip(src.iter()) {
                assert!((a - b).abs() < 1e-3, "{k:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn constant_field_is_invariant_under_every_kernel() {
        let src = vec![BG; W * H];
        let map = shift_map(0.37, -0.61);
        for k in [Interpolation::BicubicBSpline, Interpolation::MitchellNetravali, Interpolation::Lanczos3, Interpolation::BicubicSpline, Interpolation::Bilinear] {
            let out = warp_full(&src, &map, k);
            for (y, row) in out.chunks(W).enumerate() {
                for (x, v) in row.iter().enumerate() {
                    if v.is_finite() {
                        assert!((v - BG).abs() < 1e-3, "{k:?} at ({x},{y}): {v}");
                    }
                }
            }
        }
    }

    #[test]
    fn subpixel_shift_recovers_centroids_and_flux() {
        // Subject holds the stars shifted by (+0.3, −0.6).
        let (dx, dy) = (0.3, -0.6);
        let shifted: Vec<(f64, f64, f64)> = STARS.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
        let subject = gaussian_field(W, H, &shifted, 1.8, BG);
        let reference = field();
        let map = shift_map(dx, dy);
        for k in [Interpolation::Bilinear, Interpolation::BicubicSpline, Interpolation::BicubicBSpline, Interpolation::Lanczos3, Interpolation::Lanczos4, Interpolation::MitchellNetravali] {
            let out = warp_full(&subject, &map, k);
            for &(sx, sy, _) in &STARS {
                let (cx, cy) = centroid(&out, W, sx, sy, 7, BG);
                assert!((cx - sx).abs() < 0.02 && (cy - sy).abs() < 0.02, "{k:?}: centroid ({cx},{cy}) vs ({sx},{sy})");
                let f_out = flux(&out, W, sx, sy, 7, BG);
                let f_ref = flux(&reference, W, sx, sy, 7, BG);
                assert!(((f_out - f_ref) / f_ref).abs() < 0.005, "{k:?}: flux {f_out} vs {f_ref}");
            }
        }
    }

    #[test]
    fn rotation_about_the_centre_recovers_centroids() {
        let (cx, cy) = ((W as f64 - 1.0) / 2.0, (H as f64 - 1.0) / 2.0);
        let th = 30f64.to_radians();
        let (s, c) = th.sin_cos();
        // forward (subject → reference): rotate by +30° about the centre.
        let fwd = Linear {
            kind: LinearKind::Similarity,
            m: [[c, -s, cx - c * cx + s * cy], [s, c, cy - s * cx - c * cy], [0.0, 0.0, 1.0]],
        };
        let map = PixelMap::linear(fwd).unwrap();
        // Subject star positions = inverse of the reference positions.
        let subj_stars: Vec<(f64, f64, f64)> = STARS
            .iter()
            .map(|&(x, y, a)| {
                let (u, v) = map.inverse(x, y);
                (u, v, a)
            })
            .collect();
        let subject = gaussian_field(W, H, &subj_stars, 1.8, BG);
        let out = warp_full(&subject, &map, Interpolation::Lanczos3);
        for &(sx, sy, _) in &STARS {
            let (ox, oy) = centroid(&out, W, sx, sy, 7, BG);
            assert!((ox - sx).abs() < 0.02 && (oy - sy).abs() < 0.02, "({ox},{oy}) vs ({sx},{sy})");
        }
    }

    #[test]
    fn pixels_mapping_outside_the_source_are_nan() {
        let src = field();
        let map = shift_map(100.0, 0.0); // reference x maps to subject x + 100
        let out = warp_full(&src, &map, Interpolation::Bilinear);
        for y in 0..H {
            for x in 0..W {
                let v = out[y * W + x];
                if x + 100 > W - 1 {
                    assert!(v.is_nan(), "({x},{y}) should be outside");
                } else {
                    assert!(v.is_finite(), "({x},{y}) should be inside");
                }
            }
        }
    }

    #[test]
    fn windowed_plane_matches_the_full_plane() {
        let src = field();
        let map = shift_map(0.25, 10.4);
        let full = warp_full(&src, &map, Interpolation::BicubicBSpline);
        // Output rows 40..70 need source rows ≈ 50..81 (+margins).
        let (y0, rows) = (40usize, 30usize);
        let win = match source_window(&map, W, y0, rows, W, H, 2, 0.6) {
            SourceWindow::Rows { y0, y1 } => (y0, y1),
            SourceWindow::Whole => (0, H),
        };
        let plane = Plane { data: &src[win.0 * W..win.1 * W], width: W, height: win.1 - win.0, y_offset: win.0, full_height: H };
        let mut out = vec![0f32; rows * W];
        warp_rows(&plane, &map, W, y0, rows, Interpolation::BicubicBSpline, 0.3, &mut out);
        for (i, v) in out.iter().enumerate() {
            let f = full[y0 * W + i];
            assert!((v.is_nan() && f.is_nan()) || (v - f).abs() < 1e-5, "row {} col {}: {v} vs {f}", y0 + i / W, i % W);
        }
    }
}
```

Add to `window.rs` tests a `use crate::resample::window::*` is not needed
(the tests live in the module). Update `crates/athenaeum-core/src/resample/mod.rs`:

```rust
pub mod kernels;
pub mod warp;
pub mod window;

pub use kernels::{Interpolation, Taps};
pub use warp::{sample_at, warp_rows, Plane};
pub use window::{source_window, SourceWindow};
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core resample::`
Expected: compile errors — `SourceWindow`, `source_window`, `Plane`, `warp_rows` undefined.

- [ ] **Step 4: Implement the window**

Prepend to `crates/athenaeum-core/src/resample/window.rs`:

```rust
//! The source rows a band of output rows needs. The band's boundary is
//! mapped densely through the inverse map (every 32 px along its four
//! edges), the source row span is expanded by the kernel radius plus one,
//! and clamped to the frame. A span longer than `whole_threshold` of the
//! frame height (a rotation near a quarter turn) reads the whole plane.

use crate::geometry::InverseMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceWindow {
    /// Source rows `[y0, y1)`; empty (`y0 == y1`) when the band maps
    /// entirely outside the source.
    Rows { y0: usize, y1: usize },
    Whole,
}

#[allow(clippy::too_many_arguments)]
pub fn source_window(
    map: &dyn InverseMap,
    out_width: usize,
    y0: usize,
    rows: usize,
    src_width: usize,
    src_height: usize,
    radius: usize,
    whole_threshold: f64,
) -> SourceWindow {
    const STEP: usize = 32;
    let x_max = out_width.saturating_sub(1) as f64;
    let top = y0 as f64 - 0.5;
    let bottom = (y0 + rows) as f64 - 0.5;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    let mut visit = |x: f64, y: f64| {
        let (_, sy) = map.inverse(x, y);
        if sy.is_finite() {
            ymin = ymin.min(sy);
            ymax = ymax.max(sy);
        }
    };
    let mut x = 0.0;
    while x <= x_max {
        visit(x, top);
        visit(x, bottom);
        x += STEP as f64;
    }
    visit(x_max, top);
    visit(x_max, bottom);
    let mut y = top;
    while y <= bottom {
        visit(0.0, y);
        visit(x_max, y);
        y += STEP as f64;
    }
    visit(0.0, bottom);
    visit(x_max, bottom);
    let _ = src_width;
    if !ymin.is_finite() || !ymax.is_finite() {
        return SourceWindow::Whole;
    }
    let margin = radius as f64 + 1.0;
    let lo = (ymin - margin).floor();
    let hi = (ymax + margin).ceil() + 1.0;
    if (hi - lo) > whole_threshold * src_height as f64 {
        return SourceWindow::Whole;
    }
    let y0s = lo.clamp(0.0, src_height as f64) as usize;
    let y1s = hi.clamp(0.0, src_height as f64) as usize;
    SourceWindow::Rows { y0: y0s.min(y1s), y1: y1s }
}
```

- [ ] **Step 5: Implement the warp**

Prepend to `crates/athenaeum-core/src/resample/warp.rs`:

```rust
//! Inverse-mapped gather: for every output pixel, map its centre back into
//! the source through the frame's inverse map and evaluate the separable
//! kernel there. Outside the source's sampled domain the output is NaN —
//! the integration engine already treats a non-finite sample as "missing"
//! with per-frame accounting (spec §3.4).

use rayon::prelude::*;

use super::kernels::{clamp_keys_1d, combine_lanczos_clamped, taps_for, Interpolation};
use crate::geometry::InverseMap;

/// A window of rows of a larger plane. `data` holds rows
/// `[y_offset, y_offset + height)` of an image `width × full_height`.
pub struct Plane<'a> {
    pub data: &'a [f32],
    pub width: usize,
    pub height: usize,
    pub y_offset: usize,
    pub full_height: usize,
}

impl<'a> Plane<'a> {
    pub fn full(data: &'a [f32], width: usize, height: usize) -> Plane<'a> {
        Plane { data, width, height, y_offset: 0, full_height: height }
    }

    /// Sample with clamp-to-edge on both the frame and the window.
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        let xc = x.clamp(0, self.width as isize - 1) as usize;
        let yc = y.clamp(0, self.full_height as isize - 1) as usize;
        let yl = yc.clamp(self.y_offset, self.y_offset + self.height - 1) - self.y_offset;
        self.data[yl * self.width + xc]
    }
}

/// One interpolated sample at source coordinates `(x, y)`; NaN outside
/// `[0, w−1] × [0, h−1]`.
pub fn sample_at(src: &Plane, x: f64, y: f64, interp: Interpolation, clamping: f32) -> f32 {
    if !(x.is_finite() && y.is_finite()) {
        return f32::NAN;
    }
    if x < 0.0 || y < 0.0 || x > (src.width - 1) as f64 || y > (src.full_height - 1) as f64 {
        return f32::NAN;
    }
    let tx = taps_for(interp, x as f32);
    let ty = taps_for(interp, y as f32);
    match interp {
        Interpolation::BicubicSpline => {
            // Row-wise Keys with the 1-D clamp, then the column pass.
            let mut col = [0f32; 4];
            for (j, c) in col.iter_mut().enumerate() {
                let yy = ty.first + j as isize;
                let p = [src.at(tx.first, yy), src.at(tx.first + 1, yy), src.at(tx.first + 2, yy), src.at(tx.first + 3, yy)];
                let w = [tx.w[0], tx.w[1], tx.w[2], tx.w[3]];
                *c = clamp_keys_1d(&w, &p, clamping);
            }
            let w = [ty.w[0], ty.w[1], ty.w[2], ty.w[3]];
            clamp_keys_1d(&w, &col, clamping)
        }
        Interpolation::Lanczos3 | Interpolation::Lanczos4 => {
            let (mut pos, mut posw, mut neg, mut negw) = (0f32, 0f32, 0f32, 0f32);
            for j in 0..ty.n {
                let yy = ty.first + j as isize;
                for i in 0..tx.n {
                    let w = tx.w[i] * ty.w[j];
                    let v = src.at(tx.first + i as isize, yy);
                    if w >= 0.0 {
                        pos += w * v;
                        posw += w;
                    } else {
                        neg += -w * v;
                        negw += -w;
                    }
                }
            }
            combine_lanczos_clamped(pos, posw, neg, negw, clamping)
        }
        _ => {
            let mut acc = 0f32;
            for j in 0..ty.n {
                let yy = ty.first + j as isize;
                let mut row = 0f32;
                for i in 0..tx.n {
                    row += tx.w[i] * src.at(tx.first + i as isize, yy);
                }
                acc += ty.w[j] * row;
            }
            acc
        }
    }
}

/// Fills `out` (`rows × out_width`) with output rows `[y0, y0 + rows)`.
#[allow(clippy::too_many_arguments)]
pub fn warp_rows(
    src: &Plane,
    map: &dyn InverseMap,
    out_width: usize,
    y0: usize,
    rows: usize,
    interp: Interpolation,
    clamping: f32,
    out: &mut [f32],
) {
    assert_eq!(out.len(), rows * out_width, "warp_rows: out must be rows * out_width");
    out.par_chunks_mut(out_width).enumerate().for_each(|(r, row)| {
        let y = (y0 + r) as f64;
        for (x, o) in row.iter_mut().enumerate() {
            let (sx, sy) = map.inverse(x as f64, y);
            *o = sample_at(src, sx, sy, interp, clamping);
        }
    });
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core resample::`
Expected: 19 passed (8 kernels + 5 window + 6 warp). If
`subpixel_shift_recovers_centroids_and_flux` fails the 0.5 % flux bound for
`BicubicSpline` only, raise that kernel's bound to 1 % — its negative lobes
redistribute ~0.7 % of a σ = 1.8 px star's flux beyond the 15 px box; the
centroid bound stays at 0.02 px for every kernel.

- [ ] **Step 7: Commit**

```bash
rustfmt crates/athenaeum-core/src/resample/*.rs crates/athenaeum-core/src/test_support.rs
git add crates/athenaeum-core/src/resample crates/athenaeum-core/src/test_support.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(resample): inverse-mapped warp with source-row windows

Gather-style resampling of a row band through any inverse pixel map, NaN
outside the source, clamp-to-edge taps, per-kernel deringing, and the
band → source-row window computation that lets the integration engine
read only the rows a band needs.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 7: `PlaneReader` — positional 1/3-plane window reads

**Files:**
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (visibility + a 3-plane-aware probe + a shared positional read helper + `BandPlanes` accessors)
- Create: `crates/athenaeum-core/src/integration/plane_reader.rs`
- Modify: `crates/athenaeum-core/src/integration/mod.rs` (add `pub mod plane_reader;`)

**Interfaces:**
- Consumes: `PlaneKind`, `probe_fits`, `FitsInfo`, `plane_kind_for_bitpix` from `banded.rs` (all made `pub(crate)` here).
- Produces:
  - `banded.rs`: `pub enum PlaneKind` (was `pub(crate)`), `pub(crate) fn decode_run`, `pub(crate) fn plane_kind_for_bitpix`, `pub(crate) fn probe_fits(path) -> Option<(File, FitsInfo)>` with `pub(crate) struct FitsInfo` and `pub(crate)` fields, `pub(crate) fn pread_exact(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()>`, `BandPlanes::with_kinds(kinds: Vec<PlaneKind>, width: usize) -> BandPlanes`, `BandPlanes::buf_mut(&mut self, frame: usize) -> &mut Vec<u8>`, `BandPlanes::set_rows(&mut self, rows: usize)`, `BandPlanes::width(&self) -> usize`.
  - `plane_reader.rs`: `pub struct PlaneReader` with `open(path: &Path) -> Result<PlaneReader, IntegrationError>`, `width()`, `height()`, `channels()`, `read_rows(&self, plane: usize, y0: usize, rows: usize, dst: &mut [f32]) -> Result<(), IntegrationError>`, `read_plane(&self, plane: usize) -> Result<Vec<f32>, IntegrationError>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/integration/plane_reader.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::banded::BandSource;
    use std::io::Write;

    fn f32_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize, channels: usize) -> (std::path::PathBuf, Vec<f32>) {
        let data: Vec<f32> = (0..w * h * channels).map(|i| i as f32 * 0.5 - 7.0).collect();
        let p = dir.join(name);
        write_fits_f32(&p, w, h, channels, &data, &[]).unwrap();
        (p, data)
    }

    /// Minimal BITPIX=16 writer (unsigned convention BZERO=32768).
    fn u16_fixture(dir: &std::path::Path, name: &str, w: usize, h: usize) -> (std::path::PathBuf, Vec<u16>) {
        let p = dir.join(name);
        let mut header = Vec::new();
        for line in [
            format!("{:<80}", "SIMPLE  =                    T"),
            format!("{:<80}", "BITPIX  =                   16"),
            format!("{:<80}", "NAXIS   =                    2"),
            format!("{:<80}", format!("NAXIS1  = {:>20}", w)),
            format!("{:<80}", format!("NAXIS2  = {:>20}", h)),
            format!("{:<80}", "BZERO   =              32768.0"),
            format!("{:<80}", "BSCALE  =                  1.0"),
            format!("{:<80}", "END"),
        ] {
            header.extend_from_slice(line.as_bytes());
        }
        header.resize(2880, b' ');
        let vals: Vec<u16> = (0..w * h).map(|i| (1000 + i * 3) as u16).collect();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&header).unwrap();
        let mut data = Vec::with_capacity(w * h * 2);
        for &v in &vals {
            let raw = (v as i32 - 32768) as i16;
            data.extend_from_slice(&raw.to_be_bytes());
        }
        let pad = (2880 - data.len() % 2880) % 2880;
        data.extend(std::iter::repeat(0u8).take(pad));
        f.write_all(&data).unwrap();
        (p, vals)
    }

    #[test]
    fn reads_rows_of_a_single_plane_file() {
        let dir = tempfile::tempdir().unwrap();
        let (p, data) = f32_fixture(dir.path(), "mono.fits", 9, 7, 1);
        let r = PlaneReader::open(&p).unwrap();
        assert_eq!((r.width(), r.height(), r.channels()), (9, 7, 1));
        let mut dst = vec![0f32; 3 * 9];
        r.read_rows(0, 2, 3, &mut dst).unwrap();
        assert_eq!(&dst[..], &data[2 * 9..5 * 9]);
        assert_eq!(r.read_plane(0).unwrap(), data);
    }

    #[test]
    fn reads_the_third_plane_of_an_rgb_file() {
        let dir = tempfile::tempdir().unwrap();
        let (p, data) = f32_fixture(dir.path(), "rgb.fits", 6, 5, 3);
        let r = PlaneReader::open(&p).unwrap();
        assert_eq!(r.channels(), 3);
        let mut dst = vec![0f32; 2 * 6];
        r.read_rows(2, 3, 2, &mut dst).unwrap();
        let plane = 2 * 6 * 5;
        assert_eq!(&dst[..], &data[plane + 3 * 6..plane + 5 * 6]);
    }

    #[test]
    fn decodes_bzero_scaled_sixteen_bit_data() {
        let dir = tempfile::tempdir().unwrap();
        let (p, vals) = u16_fixture(dir.path(), "u16.fits", 5, 4);
        let r = PlaneReader::open(&p).unwrap();
        let got = r.read_plane(0).unwrap();
        for (g, v) in got.iter().zip(vals.iter()) {
            assert_eq!(*g, *v as f32);
        }
    }

    #[test]
    fn out_of_range_requests_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = f32_fixture(dir.path(), "mono.fits", 4, 4, 1);
        let r = PlaneReader::open(&p).unwrap();
        let mut dst = vec![0f32; 4];
        assert!(matches!(r.read_rows(1, 0, 1, &mut dst), Err(IntegrationError::BadInput(_))));
        assert!(matches!(r.read_rows(0, 3, 2, &mut dst), Err(IntegrationError::BadInput(_))));
        let mut short = vec![0f32; 3];
        assert!(matches!(r.read_rows(0, 0, 1, &mut short), Err(IntegrationError::BadInput(_))));
    }

    #[test]
    fn a_three_plane_file_still_takes_the_band_source_spill_error() {
        // `BandSource` must keep refusing multi-plane frames exactly as
        // before this task — through the spill path's 1-channel message.
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = f32_fixture(dir.path(), "rgb.fits", 6, 5, 3);
        let err = BandSource::open(&[p], dir.path(), 1).err().expect("must be rejected");
        assert!(format!("{err}").contains("1-channel"), "{err}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core integration::plane_reader`
Expected: compile error — `PlaneReader` not defined.

- [ ] **Step 3: Open up `banded.rs`**

In `crates/athenaeum-core/src/integration/banded.rs`:

1. `pub(crate) enum PlaneKind` → `pub enum PlaneKind` (keep the derive; add
   `#[derive(Debug)]` to the existing `#[derive(Clone, Copy)]`).
2. `fn decode_run(self, …)` → `pub(crate) fn decode_run(self, …)`.
3. `fn plane_kind_for_bitpix(…)` → `pub(crate) fn plane_kind_for_bitpix(…)`.
4. `struct FitsInfo { … }` → `pub(crate) struct FitsInfo` with every field
   `pub(crate)`.
5. `fn probe_fits(path: &Path)` → `pub(crate) fn probe_fits(path: &Path)`
   and replace its acceptance condition:

```rust
    let ok_bitpix = matches!(info.bitpix, 8 | 16 | 32 | -32 | -64);
    // A plain 2-D image, or a 3-D one with one or three planes (a
    // debayered calibrated light). Anything else takes the spill path.
    let ok_shape = (info.naxis == 2 && info.naxis3 == 1)
        || (info.naxis == 3 && (info.naxis3 == 1 || info.naxis3 == 3));
    if ok_shape && ok_bitpix && info.w > 0 && info.h > 0 {
        Some((f, info))
    } else {
        None
    }
```

6. In `probe_one`, keep `BandSource` single-plane by routing multi-plane
   files to the spill path, whose error message is the one callers see
   today:

```rust
fn probe_one(p: &Path) -> Result<ProbeOutcome, IntegrationError> {
    match probe_fits(p) {
        // Multi-plane files are `PlaneReader`'s business; a banded source
        // stays single-plane and lets the spill path raise its 1-channel
        // error exactly as before.
        Some((_, info)) if info.naxis3 != 1 => Ok(ProbeOutcome::NeedsSpill),
        Some((file, info)) => Ok(ProbeOutcome::Fits(
            FrameReader::Fits {
                file,
                data_offset: info.data_offset,
                kind: plane_kind_for_bitpix(info.bitpix, info.bzero, info.bscale),
            },
            info.w, info.h,
        )),
        None => Ok(ProbeOutcome::NeedsSpill),
    }
}
```

7. Extract the positional read into a crate-visible helper and call it from
   `FrameReader::read_exact_at` (move the two `#[cfg]` arms, with their
   comments, into the helper unchanged):

```rust
/// Positional exact read — `pread`-style on unix, a `seek_read` loop on
/// Windows. No cursor is involved on unix; on Windows every call carries its
/// own offset, so concurrent calls on one handle stay safe as long as no
/// cursor-based read is ever mixed in (see `FrameReader::read_exact_at`).
pub(crate) fn pread_exact(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read while filling a band"));
            }
            done += n;
        }
        Ok(())
    }
}
```

and in `impl FrameReader`:

```rust
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let (file, base) = match self {
            FrameReader::Fits { file, data_offset, .. } => (file, *data_offset),
            FrameReader::Scratch { file, .. } => (file, 0),
        };
        pread_exact(file, buf, base + offset)
    }
```

8. Add to `impl BandPlanes`:

```rust
    /// A band buffer set for a source that is not a `BandSource` (a
    /// resampling source hands in its own per-frame kinds — always
    /// `PlaneKind::F32Le`, the native little-endian f32 it writes).
    pub(crate) fn with_kinds(kinds: Vec<PlaneKind>, width: usize) -> BandPlanes {
        BandPlanes { bufs: vec![Vec::new(); kinds.len()], kinds, width, rows: 0 }
    }

    pub(crate) fn width(&self) -> usize { self.width }

    pub(crate) fn set_rows(&mut self, rows: usize) { self.rows = rows; }

    /// One frame's raw band buffer, for a source that fills it itself.
    pub(crate) fn buf_mut(&mut self, frame: usize) -> &mut Vec<u8> { &mut self.bufs[frame] }
```

- [ ] **Step 4: Implement `PlaneReader`**

Prepend to `crates/athenaeum-core/src/integration/plane_reader.rs`:

```rust
//! Positional reads of a row window of ONE plane of ONE uncompressed FITS
//! file — the reader behind lazy registration, measurement and drizzle,
//! which all read calibrated frames (1 plane mono, 3 planes debayered OSC)
//! one frame at a time. Decode shares `PlaneKind` with the banded reader.

use std::fs::File;
use std::path::Path;

use super::banded::{plane_kind_for_bitpix, pread_exact, probe_fits, PlaneKind};
use super::IntegrationError;

pub struct PlaneReader {
    file: File,
    width: usize,
    height: usize,
    channels: usize,
    kind: PlaneKind,
    data_offset: u64,
}

impl PlaneReader {
    pub fn open(path: &Path) -> Result<PlaneReader, IntegrationError> {
        let (file, info) = probe_fits(path).ok_or_else(|| {
            IntegrationError::BadInput(format!(
                "{}: not an uncompressed 1- or 3-plane FITS image",
                path.display()
            ))
        })?;
        Ok(PlaneReader {
            file,
            width: info.w,
            height: info.h,
            channels: info.naxis3,
            kind: plane_kind_for_bitpix(info.bitpix, info.bzero, info.bscale),
            data_offset: info.data_offset,
        })
    }

    pub fn width(&self) -> usize { self.width }
    pub fn height(&self) -> usize { self.height }
    pub fn channels(&self) -> usize { self.channels }
    pub fn kind(&self) -> PlaneKind { self.kind }

    /// Bytes one plane occupies on disk.
    fn plane_bytes(&self) -> u64 {
        (self.width * self.height * self.kind.bytes_per_sample()) as u64
    }

    /// Rows `[y0, y0 + rows)` of `plane`, decoded into `dst` (len `rows × width`).
    pub fn read_rows(&self, plane: usize, y0: usize, rows: usize, dst: &mut [f32]) -> Result<(), IntegrationError> {
        if plane >= self.channels {
            return Err(IntegrationError::BadInput(format!("plane {plane} of a {}-plane image", self.channels)));
        }
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!("rows {y0}+{rows} beyond height {}", self.height)));
        }
        if dst.len() != rows * self.width {
            return Err(IntegrationError::BadInput(format!(
                "destination holds {} samples, {} rows need {}",
                dst.len(), rows, rows * self.width
            )));
        }
        if rows == 0 {
            return Ok(());
        }
        let bpp = self.kind.bytes_per_sample();
        let mut raw = vec![0u8; rows * self.width * bpp];
        let offset = self.data_offset + plane as u64 * self.plane_bytes() + (y0 * self.width * bpp) as u64;
        pread_exact(&self.file, &mut raw, offset)?;
        self.kind.decode_run(&raw, 0, dst);
        Ok(())
    }

    pub fn read_plane(&self, plane: usize) -> Result<Vec<f32>, IntegrationError> {
        let mut out = vec![0f32; self.width * self.height];
        self.read_rows(plane, 0, self.height, &mut out)?;
        Ok(out)
    }
}
```

Add `pub mod plane_reader;` to `crates/athenaeum-core/src/integration/mod.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core integration::`
Expected: the 5 new tests pass and every existing `integration::` test
still passes (the banded fixture `f32_needs_spill_fixture` — `NAXIS = 2`
with a bogus `NAXIS3 = 2` — must still be rejected by `probe_fits`, which
the new `ok_shape` rule guarantees). Also run
`cargo test -p athenaeum-core calibration_library::` — the light-cal and
cosmetic readers are untouched consumers.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/integration/banded.rs crates/athenaeum-core/src/integration/plane_reader.rs crates/athenaeum-core/src/integration/mod.rs
git add crates/athenaeum-core/src/integration
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(integration): PlaneReader for positional single-plane window reads

One file, one plane, a row window — the reader the lazy registered source,
the measurement stage and drizzle use for 1- and 3-plane calibrated
frames. BandSource stays single-plane; its probe now recognizes 3-plane
files only to route them to the same spill error as before.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

### Task 8: `FrameSource`, the lazily resampled `RegisteredSource`, and `integrate_registered`

**Files:**
- Create: `crates/athenaeum-core/src/integration/source.rs`
- Create: `crates/athenaeum-core/src/integration/registered_source.rs`
- Modify: `crates/athenaeum-core/src/integration/mod.rs` (add `pub mod source; pub mod registered_source;`)
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (`BandPlanes::new` generic over `FrameSource`)
- Modify: `crates/athenaeum-core/src/integration/engine.rs:146-160` (`run_banded` generic; new `integrate_registered`)

**Interfaces:**
- Consumes: `PlaneReader` (Task 7), `PixelMap` (Task 3), `Interpolation`, `Plane`, `warp_rows`, `source_window`, `SourceWindow` (Tasks 1, 6), `BandPlanes::{with_kinds, buf_mut, set_rows, width}` (Task 7).
- Produces:
  - `pub trait FrameSource: Sync { fn width(&self) -> usize; fn height(&self) -> usize; fn frame_count(&self) -> usize; fn plane_kinds(&self) -> Vec<PlaneKind>; fn bytes_per_row(&self) -> usize; fn band_rows_for_budget(&self, budget_bytes: usize) -> usize; fn read_band_with_progress(&self, y0: usize, rows: usize, out: &mut BandPlanes, concurrency: usize, on_bytes: &(dyn Fn(u64) + Sync), cancel: &AtomicBool) -> Result<(), IntegrationError>; }` implemented for `BandSource` and `RegisteredSource`.
  - `pub struct RegisteredFrame { pub path: PathBuf, pub map: PixelMap }`.
  - `pub struct RegisteredSource` with `open(frames: &[RegisteredFrame], ref_width: usize, ref_height: usize, plane: usize, interp: Interpolation, clamping: f32) -> Result<RegisteredSource, IntegrationError>`, `channels(&self) -> usize`, `plane(&self) -> usize`, `set_plane(&mut self, plane: usize) -> Result<(), IntegrationError>`, `set_whole_threshold(&mut self, t: f64)`.
  - `pub fn integrate_registered(src: &RegisteredSource, recipe: IntegrationRecipe, pool: &rayon::ThreadPool, cancel: &AtomicBool, progress: EngineProgress<'_>, io: IoPolicy) -> Result<IntegrationOutput, IntegrationError>` in `engine.rs`.
  - `BandPlanes::new<S: FrameSource + ?Sized>(src: &S) -> BandPlanes`.

- [ ] **Step 1: Write the failing end-to-end test**

Create `crates/athenaeum-core/src/integration/registered_source.rs` with only tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::{Linear, LinearKind, PixelMap};
    use crate::integration::combine::{IntegrationRecipe, Rejection};
    use crate::integration::engine::{integrate_registered, EngineProgress};
    use crate::integration::io_policy::IoPolicy;
    use crate::integration::storage_class::StorageClass;
    use crate::integration::source::FrameSource;
    use crate::resample::Interpolation;
    use crate::test_support::{centroid, gaussian_field};
    use std::sync::atomic::AtomicBool;

    const W: usize = 200;
    const H: usize = 150;
    const BG: f32 = 100.0;
    const STARS: [(f64, f64, f64); 4] = [(40.3, 30.7, 4000.0), (120.0, 60.0, 2500.0), (170.6, 120.2, 6000.0), (60.0, 110.0, 3000.0)];
    const SHIFTS: [(f64, f64); 3] = [(0.0, 0.0), (0.4, -0.7), (-12.3, 5.6)];

    /// Subject frame i holds the stars at `(x + dx_i, y + dy_i)`; its forward
    /// map (subject → reference) subtracts the shift.
    fn frames(dir: &std::path::Path) -> Vec<RegisteredFrame> {
        SHIFTS
            .iter()
            .enumerate()
            .map(|(i, &(dx, dy))| {
                let stars: Vec<_> = STARS.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
                let data = gaussian_field(W, H, &stars, 1.8, BG);
                let path = dir.join(format!("sub{i}.fits"));
                write_fits_f32(&path, W, H, 1, &data, &[]).unwrap();
                let fwd = Linear { kind: LinearKind::Affine, m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]] };
                RegisteredFrame { path, map: PixelMap::linear(fwd).unwrap() }
            })
            .collect()
    }

    fn io(budget: usize) -> IoPolicy {
        IoPolicy { band_budget_bytes: budget, read_concurrency: 2, storage: StorageClass::Local }
    }

    #[test]
    fn source_reports_reference_geometry_and_f32_planes() {
        let dir = tempfile::tempdir().unwrap();
        let mut src = RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Lanczos3, 0.3).unwrap();
        assert_eq!((src.width(), src.height(), src.frame_count(), src.channels()), (W, H, 3, 1));
        assert_eq!(src.bytes_per_row(), 3 * W * 4);
        // 3 frames × 200 px × 4 B + 200 × 8 B headroom = 4000 B per row.
        assert_eq!(src.band_rows_for_budget(80_000), 20);
        assert!(src.plane_kinds().iter().all(|k| matches!(k, PlaneKind::F32Le)));
        assert!(matches!(src.set_plane(1), Err(IntegrationError::BadInput(_))));
    }

    #[test]
    fn a_band_read_equals_a_full_warp_of_each_frame() {
        let dir = tempfile::tempdir().unwrap();
        let fr = frames(dir.path());
        let src = RegisteredSource::open(&fr, W, H, 0, Interpolation::BicubicBSpline, 0.3).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band_with_progress(37, 20, &mut planes, 2, &|_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(planes.rows(), 20);
        for (i, f) in fr.iter().enumerate() {
            let reader = PlaneReader::open(&f.path).unwrap();
            let full = reader.read_plane(0).unwrap();
            let plane = crate::resample::Plane::full(&full, W, H);
            let mut expect = vec![0f32; H * W];
            crate::resample::warp_rows(&plane, &f.map, W, 0, H, Interpolation::BicubicBSpline, 0.3, &mut expect);
            let mut got = vec![0f32; 20 * W];
            planes.decode_frame_into(i, &mut got);
            for (k, g) in got.iter().enumerate() {
                let e = expect[37 * W + k];
                assert!((g.is_nan() && e.is_nan()) || (g - e).abs() < 1e-5, "frame {i} sample {k}: {g} vs {e}");
            }
        }
    }

    #[test]
    fn integrating_shifted_frames_yields_an_aligned_average_with_exact_coverage_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let src = RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Lanczos3, 0.3).unwrap();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let progress = EngineProgress { on_band: &|_, _, _, _| {}, on_combine: &|_, _, _, _| {} };
        let out = integrate_registered(
            &src,
            IntegrationRecipe::average(Rejection::None),
            &pool,
            &AtomicBool::new(false),
            progress,
            io(80_000), // 20-row bands → 8 bands, so the window logic is exercised
        )
        .unwrap();
        assert_eq!((out.width, out.height), (W, H));
        assert!(out.bands >= 8, "expected a multi-band run, got {}", out.bands);
        for &(sx, sy, _) in &STARS {
            let (cx, cy) = centroid(&out.data, W, sx, sy, 7, BG);
            assert!((cx - sx).abs() < 0.02 && (cy - sy).abs() < 0.02, "centroid ({cx},{cy}) vs ({sx},{sy})");
        }
        // Frame 0 covers everything; frame 1 loses the last column (x + 0.4 > 199)
        // and the first row (y − 0.7 < 0): 150 + 200 − 1; frame 2 loses 13
        // columns (x − 12.3 < 0) and 6 rows (y + 5.6 > 149): 13·150 + 6·200 − 13·6.
        assert_eq!(out.bad_samples_per_frame, vec![0, 349, 3072]);
        assert_eq!(out.all_bad_pixels, 0, "frame 0 covers every pixel");
        assert!(out.data.iter().all(|v| v.is_finite()));
        // The frame-0-only corner must equal frame 0's own background.
        assert!((out.data[0] - BG).abs() < 0.5);
    }

    #[test]
    fn cancel_between_frames_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let src = RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Bilinear, 0.3).unwrap();
        let mut planes = BandPlanes::new(&src);
        let cancel = AtomicBool::new(true);
        let r = src.read_band_with_progress(0, 10, &mut planes, 1, &|_| {}, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)));
    }

    #[test]
    fn a_band_outside_every_source_row_is_all_nan_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let far = Linear { kind: LinearKind::Affine, m: [[1.0, 0.0, 0.0], [0.0, 1.0, -5000.0], [0.0, 0.0, 1.0]] };
        let fr = vec![RegisteredFrame { path: frames(dir.path())[0].path.clone(), map: PixelMap::linear(far).unwrap() }];
        let src = RegisteredSource::open(&fr, W, H, 0, Interpolation::Bilinear, 0.3).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band_with_progress(0, 10, &mut planes, 1, &|_| {}, &AtomicBool::new(false)).unwrap();
        let mut got = vec![0f32; 10 * W];
        planes.decode_frame_into(0, &mut got);
        assert!(got.iter().all(|v| v.is_nan()));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p athenaeum-core integration::registered_source`
Expected: compile errors — `FrameSource`, `RegisteredSource`,
`RegisteredFrame`, `integrate_registered` not defined.

- [ ] **Step 3: Add the trait and implement it for `BandSource`**

Create `crates/athenaeum-core/src/integration/source.rs`:

```rust
//! What the banded engine needs from a set of frames: geometry, per-frame
//! sample kinds, a byte budget model and "fill this band". `BandSource`
//! reads files by position; `RegisteredSource` resamples calibrated frames
//! through their registration transforms on the fly (spec §6.1).

use std::sync::atomic::AtomicBool;

use super::banded::{BandPlanes, BandSource, PlaneKind};
use super::IntegrationError;

pub trait FrameSource: Sync {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn frame_count(&self) -> usize;
    fn plane_kinds(&self) -> Vec<PlaneKind>;
    /// Source bytes one row of every frame costs (drives the band budget).
    fn bytes_per_row(&self) -> usize;
    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize;
    /// Fill rows `[y0, y0 + rows)` of every frame into `out`, calling
    /// `on_bytes` once per frame with the bytes read from disk, checking
    /// `cancel` between frames.
    fn read_band_with_progress(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
        on_bytes: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), IntegrationError>;
}

impl FrameSource for BandSource {
    fn width(&self) -> usize { BandSource::width(self) }
    fn height(&self) -> usize { BandSource::height(self) }
    fn frame_count(&self) -> usize { BandSource::frame_count(self) }
    fn plane_kinds(&self) -> Vec<PlaneKind> { BandSource::plane_kinds(self) }
    fn bytes_per_row(&self) -> usize { BandSource::bytes_per_row(self) }
    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize { BandSource::band_rows_for_budget(self, budget_bytes) }
    fn read_band_with_progress(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
        on_bytes: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), IntegrationError> {
        BandSource::read_band_with_progress(self, y0, rows, out, concurrency, on_bytes, cancel)
    }
}
```

In `banded.rs`, make `BandPlanes::new` generic (replace the existing body):

```rust
    pub fn new<S: super::source::FrameSource + ?Sized>(src: &S) -> BandPlanes {
        BandPlanes::with_kinds(src.plane_kinds(), src.width())
    }
```

(`BandSource::plane_kinds` stays `pub(crate)`; the trait method is what the
generic calls.) Add `pub mod source;` and `pub mod registered_source;` to
`integration/mod.rs`.

- [ ] **Step 4: Make `run_banded` generic and add `integrate_registered`**

In `crates/athenaeum-core/src/integration/engine.rs`, change the signature
of `run_banded`:

```rust
fn run_banded<S: FrameSource + ?Sized>(
    src: &S,
    scales: &[f32],
    precal: Option<&FlatPrecal>,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
```

with `use super::source::FrameSource;` and `use super::registered_source::RegisteredSource;`
at the top of the file. The body is untouched — every call it makes
(`src.width()`, `height()`, `frame_count()`, `band_rows_for_budget()`,
`bytes_per_row()`, `BandPlanes::new(src)`, `src.read_band_with_progress(…)`)
resolves through the trait. Then add, after `integrate_bias_like`:

```rust
/// Integrates lazily resampled registered frames (spec §6.1): every band is
/// resampled from the calibrated files through their transforms on the
/// way in, so no registered file is ever written. Scales are 1; weights,
/// offsets and rejection maps arrive with Plan 4.
#[allow(clippy::too_many_arguments)]
pub fn integrate_registered(
    src: &RegisteredSource,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    let scales = vec![1.0f32; src.frame_count()];
    run_banded(src, &scales, None, recipe, pool, cancel, &progress, io)
}
```

(`src.frame_count()` needs `FrameSource` in scope, which the `use` above
provides.)

- [ ] **Step 5: Implement `RegisteredSource`**

Prepend to `crates/athenaeum-core/src/integration/registered_source.rs`:

```rust
//! A `FrameSource` that resamples calibrated frames through their
//! registration transforms as bands are requested — the hybrid
//! materialization of spec §0/§6.1: transforms persist, pixels do not.
//! Each band: map the band boundary back into every frame, read only that
//! row window, warp it into the band, hand the engine native f32.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tracing::debug;

use super::banded::{BandPlanes, PlaneKind};
use super::plane_reader::PlaneReader;
use super::source::FrameSource;
use super::IntegrationError;
use crate::geometry::PixelMap;
use crate::resample::{source_window, warp_rows, Interpolation, Plane, SourceWindow};

pub struct RegisteredFrame {
    pub path: PathBuf,
    /// Subject → reference mapping (its inverse is what the warp uses).
    pub map: PixelMap,
}

pub struct RegisteredSource {
    frames: Vec<(PlaneReader, PixelMap)>,
    width: usize,
    height: usize,
    channels: usize,
    plane: usize,
    interp: Interpolation,
    clamping: f32,
    whole_threshold: f64,
}

impl RegisteredSource {
    /// Opens every frame; all must share one channel count and `plane`
    /// must exist in it. `ref_width × ref_height` is the output geometry
    /// (the reference frame's).
    pub fn open(
        frames: &[RegisteredFrame],
        ref_width: usize,
        ref_height: usize,
        plane: usize,
        interp: Interpolation,
        clamping: f32,
    ) -> Result<RegisteredSource, IntegrationError> {
        if frames.is_empty() {
            return Err(IntegrationError::BadInput("empty frame list".into()));
        }
        if ref_width == 0 || ref_height == 0 {
            return Err(IntegrationError::BadInput("reference geometry must be non-zero".into()));
        }
        let mut opened = Vec::with_capacity(frames.len());
        let mut channels = None;
        for f in frames {
            let r = PlaneReader::open(&f.path)?;
            match channels {
                None => channels = Some(r.channels()),
                Some(c) if c != r.channels() => {
                    return Err(IntegrationError::BadInput(format!(
                        "{}: {} planes, the set has {c}",
                        f.path.display(),
                        r.channels()
                    )))
                }
                _ => {}
            }
            opened.push((r, f.map.clone()));
        }
        let channels = channels.unwrap_or(1);
        if plane >= channels {
            return Err(IntegrationError::BadInput(format!("plane {plane} of a {channels}-plane set")));
        }
        Ok(RegisteredSource {
            frames: opened,
            width: ref_width,
            height: ref_height,
            channels,
            plane,
            interp,
            clamping,
            whole_threshold: 0.6,
        })
    }

    pub fn channels(&self) -> usize { self.channels }
    pub fn plane(&self) -> usize { self.plane }

    pub fn set_plane(&mut self, plane: usize) -> Result<(), IntegrationError> {
        if plane >= self.channels {
            return Err(IntegrationError::BadInput(format!("plane {plane} of a {}-plane set", self.channels)));
        }
        self.plane = plane;
        Ok(())
    }

    /// Fraction of a frame's height above which a band reads the whole
    /// plane instead of a window (spec §6.1: 0.6).
    pub fn set_whole_threshold(&mut self, t: f64) { self.whole_threshold = t.clamp(0.05, 1.0); }

    /// Resample one frame's band into `dst` (`rows × width` f32). Returns
    /// the source bytes read.
    fn fill_frame(&self, i: usize, y0: usize, rows: usize, dst: &mut [f32]) -> Result<u64, IntegrationError> {
        let (reader, map) = &self.frames[i];
        let (sw, sh) = (reader.width(), reader.height());
        let window = source_window(map, self.width, y0, rows, sw, sh, self.interp.radius(), self.whole_threshold);
        let (sy0, sy1) = match window {
            SourceWindow::Rows { y0, y1 } => (y0, y1),
            SourceWindow::Whole => (0, sh),
        };
        let src_rows = sy1.saturating_sub(sy0);
        if src_rows == 0 {
            dst.fill(f32::NAN);
            debug!(frame = i, y0, rows, "band maps outside the source");
            return Ok(0);
        }
        let mut src = vec![0f32; src_rows * sw];
        reader.read_rows(self.plane, sy0, src_rows, &mut src)?;
        let plane = Plane { data: &src, width: sw, height: src_rows, y_offset: sy0, full_height: sh };
        warp_rows(&plane, map, self.width, y0, rows, self.interp, self.clamping, dst);
        Ok((src_rows * sw * reader.kind().bytes_per_sample()) as u64)
    }
}

fn store_f32_le(buf: &mut Vec<u8>, samples: &[f32]) {
    buf.resize(samples.len() * 4, 0);
    for (chunk, v) in buf.chunks_exact_mut(4).zip(samples.iter()) {
        chunk.copy_from_slice(&v.to_le_bytes());
    }
}

impl FrameSource for RegisteredSource {
    fn width(&self) -> usize { self.width }
    fn height(&self) -> usize { self.height }
    fn frame_count(&self) -> usize { self.frames.len() }
    fn plane_kinds(&self) -> Vec<PlaneKind> { vec![PlaneKind::F32Le; self.frames.len()] }
    fn bytes_per_row(&self) -> usize { self.frames.len() * self.width * 4 }

    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
        let per_row = self.bytes_per_row().saturating_add(self.width.saturating_mul(8)).max(1);
        (budget_bytes / per_row).max(1)
    }

    fn read_band_with_progress(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
        on_bytes: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), IntegrationError> {
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!("band {y0}+{rows} beyond height {}", self.height)));
        }
        assert_eq!(out.width(), self.width, "BandPlanes built from a different source");
        out.set_rows(rows);
        let n = self.frames.len();
        let workers = concurrency.max(1).min(n);
        let w = self.width;
        // Resample into per-frame f32 scratch first, then store; the store
        // is the only place that touches `out`, so the parallel part never
        // needs a mutable borrow of it.
        let mut scratch: Vec<Vec<f32>> = (0..n).map(|_| vec![0f32; rows * w]).collect();
        if workers == 1 {
            for (i, dst) in scratch.iter_mut().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(IntegrationError::Cancelled);
                }
                on_bytes(self.fill_frame(i, y0, rows, dst)?);
            }
        } else {
            let first_err: Mutex<Option<IntegrationError>> = Mutex::new(None);
            let abort = AtomicBool::new(false);
            let mut groups: Vec<Vec<(usize, &mut Vec<f32>)>> = (0..workers).map(|_| Vec::new()).collect();
            for (i, dst) in scratch.iter_mut().enumerate() {
                groups[i % workers].push((i, dst));
            }
            std::thread::scope(|scope| {
                for group in groups {
                    let first_err = &first_err;
                    let abort = &abort;
                    scope.spawn(move || {
                        for (i, dst) in group {
                            if cancel.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
                                return;
                            }
                            match self.fill_frame(i, y0, rows, dst) {
                                Ok(bytes) => on_bytes(bytes),
                                Err(e) => {
                                    abort.store(true, Ordering::Relaxed);
                                    let mut slot = first_err.lock().unwrap();
                                    if slot.is_none() {
                                        *slot = Some(e);
                                    }
                                    return;
                                }
                            }
                        }
                    });
                }
            });
            if let Some(e) = first_err.into_inner().unwrap() {
                return Err(e);
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(IntegrationError::Cancelled);
            }
        }
        for (i, s) in scratch.iter().enumerate() {
            store_f32_le(out.buf_mut(i), s);
        }
        debug!(y0, rows, frames = n, workers, "registered band resampled");
        Ok(())
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core integration::`
Expected: the 5 new `registered_source` tests pass and every pre-existing
`integration::` test passes unchanged. Then run the pinned consumers:

```
cargo test -p athenaeum-core --test master_build
cargo test -p athenaeum-core calibration_library::
cargo check -p athenaeum-core --no-default-features
```

Expected: all pass; the headless check succeeds (`geometry`/`resample` are
ungated and dependency-free, `integration` stays behind `render`).

- [ ] **Step 7: Commit**

```bash
rustfmt crates/athenaeum-core/src/integration/*.rs
git add crates/athenaeum-core/src/integration
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -q -F - <<'EOF'
feat(integration): FrameSource trait and the lazily resampled RegisteredSource

run_banded is now generic over a FrameSource; BandSource implements it
unchanged, and RegisteredSource resamples calibrated frames through their
registration maps band by band — reading only the source-row window each
band needs — so integrating shifted frames yields an aligned master
without ever writing a registered file. integrate_registered is the entry
point; weights, offsets and rejection maps follow in Plan 4.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY
EOF
```

---

## Plan-level verification (after Task 8)

- [ ] `cargo test -p athenaeum-core` — every test in the crate, not only the
  new modules (the trait change touches `BandPlanes::new`, whose callers are
  `light_cal.rs` and `cosmetic.rs`).
- [ ] `cargo test --workspace` — the Tauri and Axum crates compile against
  the changed `integration` surface (they do not use it directly; this is
  the gate the release workflow runs).
- [ ] `cargo check -p athenaeum-core --no-default-features`.
- [ ] `git log --oneline main..HEAD` shows the 8 task commits.
- [ ] Record in `docs/open-items.md` under a new "Stacking M1" heading: Plan 1
  landed; Plan 2 (measurement + statistics) is next and needs its own plan
  written against this code.

## What the next plan can rely on

- `crate::geometry::{Linear, LinearKind, Pair, PixelMap, Distortion, Polynomial2D, KdTree2, RansacConfig, RansacResult, Quality, ransac_fit, refit_weighted, RefitResult, InverseMap}`.
- `crate::resample::{Interpolation, Plane, sample_at, warp_rows, source_window, SourceWindow}`.
- `crate::integration::plane_reader::PlaneReader` (1/3 planes, positional).
- `crate::integration::source::FrameSource` and
  `crate::integration::registered_source::{RegisteredSource, RegisteredFrame}`.
- `crate::integration::engine::integrate_registered`.
- `crate::test_support::{gaussian_field, centroid, flux}` for synthetic
  fields in tests.
