# Scale-Recovery Fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a hinted plate solve fails but a position prior exists, recover the true scale (B1) or sweep a bounded scale range (B2) at the known position before the existing all-sky degrade (C) — so frames with a wrong/aperture focal length solve in ~1 s instead of ~160 s.

**Architecture:** Solver-only change in the `solvemyastro` submodule. `lib.rs::solve` becomes a 4-rung fallback ladder (A → B1 → B2 → C). B1 reads the recovered scale already present in the structured `SolveFailure.best_attempt` and re-runs at that scale (re-centring the FOV ladder). B2 re-runs with a bounded, asymmetric FOV ladder (hint ÷8 … ×2) at the known position; it needs an internal `solve_inner(..., scale_bounds)` so the public `SolveHints`/`SolveConfig`/`solve` signatures stay unchanged (no athenaeum edit).

**Tech Stack:** Rust, `anyhow` (error downcast), existing solvemyastro modules (`orchestrate`, `quad`, `diag`, `verify`), `cargo test`.

---

## Deviations from the spec (`2026-05-31-scale-recovery-fallback-design.md`) — confirm before executing

Two refinements were discovered while reading the code. Both are flagged to the user; the spec will be amended to match in Task 8.

1. **`b1_min_inliers` default 3 → 2.** B1's retry must itself pass the full Bayesian verification gate, so a weak/wrong harvested scale cannot produce a false solve — it only costs one quick extra pass (then B2/C run). Because it is correctness-safe, a threshold of **2** is strictly better: it catches the motivating Pane 4 case (recovered scale 0.879″/px with exactly 2 inliers) via the proven-fast B1 path rather than relying on B2.
2. **Acceptance test = dedicated real-pixels test, not "drop frame in the corpus dir."** The corpus harness `hints_from_fits` prefers an existing-WCS scale and otherwise derives scale from the file's own `FOCALLEN`/`XPIXSZ`. The motivating frame is `_lps_r` (carries a WCS) and its file header lacks the bad focal length (the bad value lived in the DB, since corrected). So a full-corpus frame would solve at step A and never exercise the fallback. Instead, add a dedicated test that loads the **real Pane 4 pixels** and supplies the **real-world wrong hint** (aperture-derived 3.52″/px, no WCS, position kept) — faithful to the actual bug and deterministic. The full-corpus no-regression gate is unchanged and still proves the fast path is untouched.

---

## File Structure (all in `solvemyastro/`)

| File | Responsibility | Change |
| ---- | ---- | ---- |
| `src/lib.rs` | Public API + `solve()` fallback orchestration | Add 4 `SolveConfig` fields + defaults; add `harvest_scale()` + `b2_bounds()` helpers; rework `solve()` into A→B1→B2→C |
| `src/orchestrate.rs` | Search core | Add `fov_rungs_bounded()`; rename body to `pub(crate) fn solve_inner(..., scale_bounds: Option<(f64,f64)>)`; keep `pub fn solve` as a 1-line wrapper |
| `tests/scale_fallback.rs` | New integration test | Real Pane 4 pixels + wrong hint → asserts fallback solves, correct WCS, `cone_calls` under budget |
| `tests/corpus_bench.rs` | Existing gate | No code change; re-run to confirm no regression |
| `docs/.../2026-05-31-scale-recovery-fallback-design.md` | Spec | Amend `b1_min_inliers` to 2 + test-approach note (Task 8) |

---

## Task 1: Add `SolveConfig` knobs

**Files:**
- Modify: `solvemyastro/src/lib.rs:103-153` (`SolveConfig` struct + `Default`)
- Test: `solvemyastro/src/lib.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `src/lib.rs` (create one at the bottom if absent):

```rust
#[cfg(test)]
mod fallback_config_tests {
    use super::SolveConfig;

    #[test]
    fn scale_fallback_defaults() {
        let c = SolveConfig::default();
        assert!(c.scale_fallback_enabled);
        assert_eq!(c.b1_min_inliers, 2);
        assert_eq!(c.b2_scale_lo_factor, 0.125);
        assert_eq!(c.b2_scale_hi_factor, 2.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd solvemyastro && cargo test --lib scale_fallback_defaults`
Expected: FAIL — `no field scale_fallback_enabled on type SolveConfig`.

- [ ] **Step 3: Add the fields**

In `SolveConfig` (after `bright_fallback_threshold: usize,` at line 136):

```rust
    /// Enable the scale-recovery fallback ladder (B1 harvest + B2 bounded
    /// sweep) on hinted-solve failure with a position prior. Default `true`.
    /// Set `false` to restore the prior degrade-to-blind-position behaviour.
    pub scale_fallback_enabled: bool,
    /// Minimum inliers in the failed attempt's best candidate before B1 trusts
    /// its recovered scale. Default `2`. Safe to keep low: B1's retry must pass
    /// full verification, so a wrong estimate only costs one extra pass.
    pub b1_min_inliers: usize,
    /// B2 bounded-sweep lower scale factor (× the hint). Default `0.125` (÷8) —
    /// covers aperture-as-focal-length errors, which always make the true scale
    /// finer than the hint.
    pub b2_scale_lo_factor: f64,
    /// B2 bounded-sweep upper scale factor (× the hint). Default `2.0` (×2).
    pub b2_scale_hi_factor: f64,
```

In `impl Default for SolveConfig` (after `bright_fallback_threshold: 30,` at line 150):

```rust
            scale_fallback_enabled: true,
            b1_min_inliers: 2,
            b2_scale_lo_factor: 0.125,
            b2_scale_hi_factor: 2.0,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd solvemyastro && cargo test --lib scale_fallback_defaults`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C solvemyastro add src/lib.rs
git -C solvemyastro commit -m "feat(solve): add scale-recovery fallback config knobs"
```

---

## Task 2: Add `fov_rungs_bounded`

**Files:**
- Modify: `solvemyastro/src/orchestrate.rs` (after `fov_rungs`, ~line 176)
- Test: `solvemyastro/src/orchestrate.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to (or create) the test module at the bottom of `src/orchestrate.rs`:

```rust
#[cfg(test)]
mod bounded_ladder_tests {
    use super::{fov_rungs_bounded, FOV_MIN_DEG, FOV_MAX_DEG};

    #[test]
    fn bounded_span_is_clamped_and_descending() {
        // long_px=5496, hint 3.52"/px -> bounds ÷8..×2 = 0.44..7.04"/px.
        let lo = 0.44_f64;
        let hi = 7.04_f64;
        let rungs = fov_rungs_bounded(5496, lo, hi);
        assert!(!rungs.is_empty());
        // Descending in FOV.
        for w in rungs.windows(2) {
            assert!(w[0].fov_deg > w[1].fov_deg, "rungs must descend");
        }
        // Every rung inside the global clamp.
        for r in &rungs {
            assert!(r.fov_deg >= FOV_MIN_DEG - 1e-9 && r.fov_deg <= FOV_MAX_DEG + 1e-9);
        }
        // Reaches near the finer (lo) end: smallest scale ≤ ~1.05× lo.
        let smallest = rungs.last().unwrap().scale_arcsec;
        assert!(smallest <= lo * 1.5 + 1e-6, "must reach the fine end: {smallest}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd solvemyastro && cargo test --lib bounded_span_is_clamped_and_descending`
Expected: FAIL — `cannot find function fov_rungs_bounded`.

- [ ] **Step 3: Implement `fov_rungs_bounded`**

Add directly after `fov_rungs` (after line 176) in `src/orchestrate.rs`:

```rust
/// FOV ladder over an explicit `[lo_arcsec, hi_arcsec]` scale band (B2 of the
/// scale-recovery fallback). Rungs descend from the coarse (hi) bound by
/// `FOV_DIV`, clamped to `[FOV_MIN_DEG, FOV_MAX_DEG]`. Always includes a rung at
/// (or just below) the fine bound so a finer-than-hint true scale is reached.
fn fov_rungs_bounded(long_px: usize, lo_arcsec: f64, hi_arcsec: f64) -> Vec<FovRung> {
    let lp = long_px.max(1) as f64;
    let mk = |fov: f64| FovRung { fov_deg: fov, scale_arcsec: fov * 3600.0 / lp };
    let lo_fov = (lo_arcsec * lp / 3600.0).clamp(FOV_MIN_DEG, FOV_MAX_DEG);
    let hi_fov = (hi_arcsec * lp / 3600.0).clamp(FOV_MIN_DEG, FOV_MAX_DEG);
    let (lo_fov, hi_fov) = if lo_fov <= hi_fov { (lo_fov, hi_fov) } else { (hi_fov, lo_fov) };

    let mut rungs = Vec::new();
    let mut fov = hi_fov;
    while fov > lo_fov {
        rungs.push(mk(fov));
        fov /= FOV_DIV;
    }
    rungs.push(mk(lo_fov)); // guarantee the fine end is tried
    rungs
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd solvemyastro && cargo test --lib bounded_span_is_clamped_and_descending`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C solvemyastro add src/orchestrate.rs
git -C solvemyastro commit -m "feat(solve): bounded FOV ladder for scale sweep"
```

---

## Task 3: Refactor `orchestrate::solve` → `solve_inner(scale_bounds)`

**Files:**
- Modify: `solvemyastro/src/orchestrate.rs:570-576` (signature) and line 623 (rung construction)

- [ ] **Step 1: Rename and add the parameter**

Change the function at line 570 from:

```rust
pub fn solve(
    image: &Path,
    hints: &SolveHints,
    caches: &crate::Caches<'_>,
    cfg: &SolveConfig,
    cancel: Option<&AtomicBool>,
) -> Result<SolveSolution> {
```

to:

```rust
/// Internal entry: `scale_bounds = Some((lo_arcsec, hi_arcsec))` drives a
/// bounded FOV ladder (B2); `None` uses the hint-derived/blind ladder.
pub(crate) fn solve_inner(
    image: &Path,
    hints: &SolveHints,
    caches: &crate::Caches<'_>,
    cfg: &SolveConfig,
    cancel: Option<&AtomicBool>,
    scale_bounds: Option<(f64, f64)>,
) -> Result<SolveSolution> {
```

- [ ] **Step 2: Use the bounds for the ladder**

Change line 623 from:

```rust
    let rungs = fov_rungs(long_px, hints);
```

to:

```rust
    let rungs = match scale_bounds {
        Some((lo, hi)) => fov_rungs_bounded(long_px, lo, hi),
        None => fov_rungs(long_px, hints),
    };
```

- [ ] **Step 3: Add the public wrapper**

Immediately before `pub(crate) fn solve_inner` (line 570), add:

```rust
/// Public solve entry — hint-derived/blind FOV ladder (no scale-bound override).
pub fn solve(
    image: &Path,
    hints: &SolveHints,
    caches: &crate::Caches<'_>,
    cfg: &SolveConfig,
    cancel: Option<&AtomicBool>,
) -> Result<SolveSolution> {
    solve_inner(image, hints, caches, cfg, cancel, None)
}
```

- [ ] **Step 4: Verify the crate builds and existing tests pass (no behaviour change with `None`)**

Run: `cd solvemyastro && cargo build && cargo test --lib`
Expected: PASS (all existing unit tests green; `solve_inner(None)` is byte-identical to the old `solve`).

- [ ] **Step 5: Commit**

```bash
git -C solvemyastro add src/orchestrate.rs
git -C solvemyastro commit -m "refactor(solve): thread optional scale_bounds via solve_inner"
```

---

## Task 4: B1 scale-harvest helper (`harvest_scale`)

**Files:**
- Modify: `solvemyastro/src/lib.rs` (add helper near `solve`)
- Test: `solvemyastro/src/lib.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to the test module in `src/lib.rs`:

```rust
#[cfg(test)]
mod harvest_tests {
    use super::harvest_scale;
    use crate::diag::{BestAttempt, FailureClass, SolveFailure};

    fn failure_with(scale: f64, inliers: usize) -> SolveFailure {
        SolveFailure {
            class: FailureClass::VerifyGap { best: inliers, required: 6 },
            message: String::new(),
            best_attempt: Some(BestAttempt {
                pass_balance: true, pass_idx: 1, rung_idx: 4, rung_total: 6,
                seed_ra: 36.7, seed_dec: 60.8, scale_arcsec_per_px: scale,
                cat_in_fov: 237, inliers, required: 6, rms_px: 0.0,
                seed_rms_px: 2.7, log_odds: 12.9,
                dist_from_truth_deg: None, scale_ratio_to_truth: None,
            }),
        }
    }

    #[test]
    fn harvests_when_scale_diverges_and_inliers_ok() {
        // true 0.879 vs supplied 3.52 (4x) -> harvest.
        assert_eq!(harvest_scale(&failure_with(0.879, 2), 2, Some(3.52)), Some(0.879));
    }
    #[test]
    fn rejects_low_inliers() {
        assert_eq!(harvest_scale(&failure_with(0.879, 1), 2, Some(3.52)), None);
    }
    #[test]
    fn rejects_when_scale_matches_hint() {
        // recovered scale within 15% of the supplied hint -> A already searched there.
        assert_eq!(harvest_scale(&failure_with(3.50, 5), 2, Some(3.52)), None);
    }
    #[test]
    fn rejects_unphysical_scale() {
        assert_eq!(harvest_scale(&failure_with(0.0, 5), 2, Some(3.52)), None);
        assert_eq!(harvest_scale(&failure_with(500.0, 5), 2, Some(3.52)), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd solvemyastro && cargo test --lib harvest_tests`
Expected: FAIL — `cannot find function harvest_scale`.

- [ ] **Step 3: Implement `harvest_scale`**

Add above `pub fn solve` in `src/lib.rs` (and ensure `use crate::diag::SolveFailure;` is in scope):

```rust
/// B1: decide the scale to retry at, given a failed attempt. Returns the
/// recovered `best_attempt` scale only when it is physically plausible, backed
/// by enough inliers, and meaningfully different from the supplied hint (so we
/// don't re-search where step A already did). Correctness-safe: the caller's
/// retry must still pass full verification.
fn harvest_scale(
    failure: &SolveFailure,
    min_inliers: usize,
    supplied_scale_arcsec: Option<f64>,
) -> Option<f64> {
    let ba = failure.best_attempt.as_ref()?;
    let s = ba.scale_arcsec_per_px;
    if !s.is_finite() || s < 0.05 || s > 120.0 {
        return None;
    }
    if ba.inliers < min_inliers {
        return None;
    }
    if let Some(hint) = supplied_scale_arcsec {
        if hint > 0.0 && (s - hint).abs() / hint <= 0.15 {
            return None;
        }
    }
    Some(s)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd solvemyastro && cargo test --lib harvest_tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git -C solvemyastro add src/lib.rs
git -C solvemyastro commit -m "feat(solve): B1 scale-harvest gate helper"
```

---

## Task 5: B2 bounds helper (`b2_bounds`)

**Files:**
- Modify: `solvemyastro/src/lib.rs` (add helper near `harvest_scale`)
- Test: `solvemyastro/src/lib.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod b2_bounds_tests {
    use super::b2_bounds;
    use crate::{SolveConfig, SolveHints};

    #[test]
    fn bounds_from_pixel_scale_hint() {
        let cfg = SolveConfig::default(); // ÷8 .. ×2
        let hints = SolveHints { pixel_scale_arcsec: Some(3.52), ..Default::default() };
        let (lo, hi) = b2_bounds(&hints, &cfg).expect("bounds");
        assert!((lo - 3.52 * 0.125).abs() < 1e-9);
        assert!((hi - 3.52 * 2.0).abs() < 1e-9);
    }
    #[test]
    fn none_when_no_scale_hint() {
        // No scale hint -> step A already ran the full position-constrained
        // ladder, so B2 adds nothing.
        let cfg = SolveConfig::default();
        let hints = SolveHints { ra: Some(1.0), dec: Some(2.0), ..Default::default() };
        assert_eq!(b2_bounds(&hints, &cfg), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd solvemyastro && cargo test --lib b2_bounds_tests`
Expected: FAIL — `cannot find function b2_bounds`.

- [ ] **Step 3: Implement `b2_bounds`**

Add below `harvest_scale` in `src/lib.rs`:

```rust
/// B2: derive the bounded scale band `[lo, hi]` (arcsec/px) from the supplied
/// hint. Returns `None` when no scale hint was given — step A already ran the
/// full position-constrained ladder in that case, so a sweep adds nothing.
fn b2_bounds(hints: &SolveHints, cfg: &SolveConfig) -> Option<(f64, f64)> {
    let hint = hints.pixel_scale_arcsec?;
    if !hint.is_finite() || hint <= 0.0 {
        return None;
    }
    Some((hint * cfg.b2_scale_lo_factor, hint * cfg.b2_scale_hi_factor))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd solvemyastro && cargo test --lib b2_bounds_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git -C solvemyastro add src/lib.rs
git -C solvemyastro commit -m "feat(solve): B2 bounded-sweep bounds helper"
```

---

## Task 6: Wire the A→B1→B2→C ladder into `solve()`

**Files:**
- Modify: `solvemyastro/src/lib.rs:210-245` (`solve` body)

- [ ] **Step 1: Replace the `solve` body**

Replace lines 217-244 (everything between the signature's `{` and the final `}`) with:

```rust
    // Step A — hinted (or blind) solve.
    let result = orchestrate::solve(image, hints, caches, cfg, cancel);
    if result.is_ok() {
        return result;
    }

    let has_position = hints.ra.is_some() || hints.dec.is_some();
    let cancelled = || cancel
        .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(false);

    // Scale-recovery fallback (B1 harvest, then B2 bounded sweep) — position
    // kept. Only fires after step A failed, so correctly-hinted frames pay
    // nothing. Each retry must still pass full verification.
    if cfg.scale_fallback_enabled && has_position && !cancelled() {
        // B1 — retry once at the scale recovered from step A's best attempt.
        let harvested = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<SolveFailure>())
            .and_then(|f| harvest_scale(f, cfg.b1_min_inliers, hints.pixel_scale_arcsec));
        if let Some(scale) = harvested {
            eprintln!(
                "[solvemyastro] hinted solve failed — retrying at recovered scale \
                 {scale:.3}\"/px (position kept)"
            );
            let b1_hints = SolveHints {
                pixel_scale_arcsec: Some(scale),
                fov_deg: None,
                ..*hints
            };
            if let Ok(sol) = orchestrate::solve(image, &b1_hints, caches, cfg, cancel) {
                return Ok(sol);
            }
        }

        // B2 — bounded scale sweep at the known position.
        if !cancelled() {
            if let Some((lo, hi)) = b2_bounds(hints, cfg) {
                eprintln!(
                    "[solvemyastro] retrying with bounded scale sweep \
                     [{lo:.3}..{hi:.3}]\"/px (position kept)"
                );
                let sweep_hints = SolveHints {
                    pixel_scale_arcsec: None,
                    fov_deg: None,
                    ..*hints
                };
                if let Ok(sol) =
                    orchestrate::solve_inner(image, &sweep_hints, caches, cfg, cancel, Some((lo, hi)))
                {
                    return Ok(sol);
                }
            }
        }
    }

    // Step C — existing degrade-to-blind: clear the position prior, keep scale.
    if has_position && !cancelled() {
        eprintln!(
            "[solvemyastro] hinted solve failed — retrying with the position prior cleared \
             (blind position, scale kept)"
        );
        let blind_hints = SolveHints { ra: None, dec: None, ..*hints };
        if let Ok(sol) = orchestrate::solve(image, &blind_hints, caches, cfg, cancel) {
            return Ok(sol);
        }
    }

    // All rungs failed — surface step A's original (richest) error.
    result
```

- [ ] **Step 2: Verify it builds and all unit tests pass**

Run: `cd solvemyastro && cargo build && cargo test --lib`
Expected: PASS. If `SolveFailure` is unresolved, add `use crate::diag::SolveFailure;` at the top of `lib.rs`.

- [ ] **Step 3: Commit**

```bash
git -C solvemyastro add src/lib.rs
git -C solvemyastro commit -m "feat(solve): A->B1->B2->C scale-recovery fallback ladder"
```

---

## Task 7: Integration test — real frame + wrong hint solves via the fallback

**Files:**
- Create: `solvemyastro/tests/scale_fallback.rs`

**Preconditions (same as `corpus_bench`):** the deep cache at `…/com.vsharifov.athenaeum/catalogs/smac_gaia` and the frame at `…/Pictures/Astro/Platesolve Bench/Light_Pane 4_…_O_…_0002_c_lps_r.xisf` exist locally. The test reads the cache path from `$ATHENAEUM_SMAC_DIR` (fallback to the app-data convention path) and skips with a printed notice if absent — never a false failure in CI.

- [ ] **Step 1: Write the test**

```rust
//! Scale-recovery fallback: a real frame solved with a deliberately wrong
//! (aperture-derived) scale hint must still solve via B1/B2 at the known
//! position, cheaply — not via the ~160 s all-sky degrade. Local-only (needs
//! the deep cache + the bench frame); skips cleanly when they are absent.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use solvemyastro::{solve, Caches, SolveConfig, SolveHints, StarCache};

fn smac_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ATHENAEUM_SMAC_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia")
}

fn frame() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(
        "Pictures/Astro/Platesolve Bench/\
         Light_Pane 4_300.0s_Bin1_O_gain111_20211011-024956_-10.0C_0002_c_lps_r.xisf",
    )
}

#[test]
fn wrong_scale_hint_recovers_at_known_position() {
    let (dir, f) = (smac_dir(), frame());
    if !dir.join("stars.smac").exists() || !f.exists() {
        eprintln!("SKIP wrong_scale_hint_recovers_at_known_position: cache or frame absent");
        return;
    }
    let deep = StarCache::open(&dir).expect("open deep cache");
    let caches = Caches::deep_only(&deep);

    // The real-world bug: aperture (140 mm) recorded as focal length -> 3.52"/px
    // (true is 0.879"/px). Position is correct.
    let wrong = SolveHints {
        ra: Some(36.7017),
        dec: Some(60.8800),
        fov_deg: None,
        pixel_scale_arcsec: Some(3.52),
        search_radius_deg: None,
        epoch: Some(2021.7737),
    };
    let cfg = SolveConfig { print_timing: true, ..SolveConfig::default() };

    let cancel = AtomicBool::new(false);
    let sol = solve(&f, &wrong, &caches, &cfg, Some(&cancel))
        .expect("fallback must solve the wrong-scale frame");

    // Correct sky + scale (truth from a clean solve).
    assert!((sol.wcs.crval.0 - 36.70).abs() < 0.2, "RA {:.3}", sol.wcs.crval.0);
    assert!((sol.wcs.crval.1 - 60.88).abs() < 0.2, "Dec {:.3}", sol.wcs.crval.1);
    assert!((sol.pixel_scale_arcsec - 0.879).abs() < 0.05,
            "scale {:.3}", sol.pixel_scale_arcsec);
    assert!(sol.matched_stars >= 10, "inliers {}", sol.matched_stars);

    // Cheap path: must NOT be the all-sky degrade. The all-sky run took
    // ~160 s / ~146k cone calls; B1/B2 are position-constrained (<10 s).
    assert!(sol.solve_time_ms < 30_000,
            "fallback too slow ({} ms) — likely fell through to all-sky", sol.solve_time_ms);
}
```

- [ ] **Step 2: Run the test (local, with cache present)**

Run: `cd solvemyastro && cargo test --test scale_fallback -- --nocapture`
Expected: PASS. The stderr should show `retrying at recovered scale 0.879"/px` (B1) and a final `SOLVED … scale=0.879"/px`, with `solve_time_ms` well under 30 s. (If it shows the bounded-sweep line instead, that is also acceptable — B2 caught it.)

> Safety note for the implementer: this test runs a real solve. If iterating, run it backgrounded with a watchdog (`RAYON_NUM_THREADS` capped + a kill timer) — a regression that drops to the all-sky path can otherwise saturate the machine. The `solve_time_ms < 30_000` assertion is the in-test guard, but the process itself is not bounded.

- [ ] **Step 3: Commit**

```bash
git -C solvemyastro add tests/scale_fallback.rs
git -C solvemyastro commit -m "test(solve): real wrong-scale frame recovers via fallback"
```

---

## Task 8: No-regression gate + spec amendment + submodule bump

**Files:**
- Modify: `docs/superpowers/specs/2026-05-31-scale-recovery-fallback-design.md` (in the **athenaeum** repo)
- Run: `solvemyastro/tests/corpus_bench.rs` (no change)

- [ ] **Step 1: Confirm the fast path is untouched (no-regression gate)**

Run: `cd solvemyastro && cargo test --test corpus_bench -- --nocapture`
Expected: `GATE: ALL assertions PASSED`, **0 precision regressions**, and net-wall within 1.30× baseline. Every existing frame still solves at step A (the new rungs never fire on success), so `cone_calls` per frame is unchanged.

- [ ] **Step 2: Amend the spec to match the two deviations**

In `docs/superpowers/specs/2026-05-31-scale-recovery-fallback-design.md`:
- §3.4: change `b1_min_inliers … default **3**` to `default **2**`, and add: "Safe to keep low — B1's retry is fully verified, so a wrong estimate cannot produce a false solve, only one extra pass."
- §5: replace the "add frame to the corpus" bullet with the dedicated-test approach (real Pane 4 pixels + reconstructed wrong hint), and note the corpus harness's existing-WCS preference as the reason.
- §6: update the `b1_min_inliers = 3` risk bullet to reflect the new default of 2 and the verified-retry safety argument.

- [ ] **Step 3: Commit the spec amendment (athenaeum repo)**

```bash
git add docs/superpowers/specs/2026-05-31-scale-recovery-fallback-design.md
git commit -m "docs(plate_solve): amend scale-recovery spec (b1 threshold 2, dedicated test)"
```

- [ ] **Step 4: Bump the submodule pointer in athenaeum**

After the solvemyastro commits land on its branch, record the new pointer:

```bash
git add solvemyastro
git commit -m "chore: bump solvemyastro (scale-recovery solve fallback)"
```

Expected: `git -C solvemyastro log --oneline -1` matches the pointer recorded in the athenaeum commit.

---

## Self-Review

- **Spec coverage:** A→B1→B2→C ladder (Tasks 3,6) ✓; B1 harvest + gate (Task 4) ✓; B2 bounded sweep + bounds (Tasks 2,5) ✓; config knobs (Task 1) ✓; cancellation + enabled flag (Task 6) ✓; no-athenaeum-API-change via `solve_inner` (Task 3) ✓; corpus no-regression + dedicated real-frame test (Tasks 7,8) ✓; spec amended for the two deviations (Task 8) ✓.
- **Placeholder scan:** none — every code/command step is concrete.
- **Type consistency:** `solve_inner(.., scale_bounds: Option<(f64,f64)>)`, `fov_rungs_bounded(usize,f64,f64)`, `harvest_scale(&SolveFailure, usize, Option<f64>) -> Option<f64>`, `b2_bounds(&SolveHints,&SolveConfig) -> Option<(f64,f64)>` are used identically across tasks. `BestAttempt` field names match `diag.rs`. `SolveConfig` field names match Task 1.
