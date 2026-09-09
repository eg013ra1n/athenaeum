# Stacking M1 — Plan 4: combiner v2, weighted engine, headers, Checkpoint B

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn registered, measured, weighted frames into a master light — weighted combination with survivor masks and rejection maps, global normalization, the spec's Auto rejection rule, a WCS card writer, the master-light card set and file naming — and prove it on the owner's data against the external masters (**Checkpoint B**).

**Architecture:** The banded engine keeps one band loop (reads, progress, cancel, timing) and grows a second per-pixel path: every sample is range-checked, rejection-normalized for the rejection algorithm, output-normalized for the combination, weighted, and its survival recorded in a per-pixel bit mask from which rejection maps and per-frame rejected fractions are accumulated. The four rejection algorithms are made generic over the sample type so the index-carrying variant is the same code as the master builder's — masters stay byte-identical. A group driver runs the engine once per plane through `RegisteredSource`, measures the master, and hands planes + maps + stats to a writer that builds the master-light header (copy-through, WCS from the reference's plate solve, provenance) and the §9.5 file name.

**Tech Stack:** Rust (no new crate dependencies), the existing `integration`/`resample`/`geometry`/`stacking` modules, `fits_writer`, `plate_solve::storage::PlateSolveRecord`, `serde`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` (§5.1, §6, §9.2, §9.5, §13, §14 M1 items 7–8). **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §3. **Previous checkpoint:** `docs/superpowers/research/2026-09-09-checkpoint-a-registration.md`.

## M1 program index (this is Plan 4 of 5)

| Plan | Scope | Status |
| ---- | ---- | ---- |
| 1 | kernels, warp, window, `Linear`/`PixelMap`/`Polynomial2D`, KD-tree, RANSAC, `PlaneReader`, `FrameSource`, `RegisteredSource` | merged 2afa55ad |
| 2 | `integration/stats.rs`, `stacking/{robust,psf_signal,measure,weights}.rs`, `measure_probe` | merged 3a17ef95 |
| 3 | `stacking/register/*`, `registration_results` v2, scanner skip, `register_probe`, Checkpoint A | merged 3f7179bd |
| **4 (this)** | combiner v2 + weighted engine + normalization/rejection config + WCS writer + master cards/naming + `integrate_probe` + **Checkpoint B** | — |
| 5 | Orchestration (`stacking/{config,groups,plan,run,paths,provenance}.rs`), the five tables, commands ×2, `ts_export`, events, the Stacking tab, acceptance run | consumes everything below |

## Rulings made while writing this plan

1. **Masters stay byte-identical by construction, not by a second copy of the algorithms.** The four rejection routines in `combine.rs` become generic over a `Sample` trait (`f32` and `(f32, u16)`); `combine_pixel` keeps calling them with `f32` — same monomorphized code, same results — and the new `combine_pixel_weighted` calls them with index-carrying pairs. A pin test runs both on 2000 random stacks with unit weights and identity normalization and demands bit equality.
2. **Linear-fit dispersion parity lands here (spec §6.3).** `reject_linear_fit` adopts `s = 2·adev·sqrt(1 + b²)` (math reference §3.4). No master recipe auto-selects linear fit, so only an explicitly linear-fit master changes; the affected unit tests are re-pinned in the same task with the measured values and the reason.
3. **The survivor mask is caller-owned scratch** (`&mut [u64]`, `ceil(n/64)` words per pixel), not a heap-allocated type: the engine holds one per row worker. Low/high classification of a rejected sample: below the survivors' median of the rejection-normalized values → low, otherwise high (the reference counts the two sides separately; this is the side each algorithm rejected on, recovered after the fact without threading a side through every routine).
4. **Range rejection belongs to the engine, counts in the maps.** `raw ≤ rangeLow` (on by default at 0.0) and `raw ≥ rangeHigh` (off by default; 0.98 when on) are rejected before normalization and counted low/high — the reference's `mapRangeRejection = true`. NaN samples (no coverage) are neither rejected nor counted: they are missing, as today.
5. **One band loop.** `run_banded`'s read/progress/cancel/timing skeleton is extracted into `band_loop` with a per-band combine closure; the master path and the stacking path are two closures. The existing engine tests (cancel points, progress monotonicity, bytes accounting, flats) are the pin for the extraction.
6. **Rejection maps are counts** (spec §6.2), f32 planes written as `<master stem>_rejlow.fits` / `_rejhigh.fits` when `writeRejectionMaps` is on; the external maps are fractions (`count / n`) — the checkpoint divides ours by `n` to compare.
7. **`IMAGETYP` of a master light is the writer's existing `FrameKind::MasterLight` string (`Master Light`)**, not the spec's `'MASTER LIGHT'` — one convention for every master the app writes; the spec is corrected in Task 6.
8. **`ATH_STKID` is nine characters — FITS keywords are eight.** The run-id card is `ATH_STKI`; the spec is corrected in Task 6.
9. **CRPIX is 1-based on the card, 0-based in the record.** `PlateSolveRecord.crpix*` follows the solver (0-based pixel coordinates, `x − crpix`); the FITS card gets `crpix + 1`. SIP: `A_ORDER`/`B_ORDER` + `A_i_j`/`B_i_j` (and `AP_`/`BP_` when the reverse exists) from the stored `Vec<Vec<f64>>` (`coeffs[i][j]` for `u^i v^j`, i + j ≤ order); `CTYPE` gains the `-SIP` suffix only when SIP cards are written.
10. **The stacking rejection config is its own type** (`RejectionChoice`, camelCase per spec §9.2, with `auto`) that resolves to the master builder's `Rejection` (snake_case, `tag = "method"`) per group size: n < 8 → percentile 0.2/0.1; 8 ≤ n < 20 → Winsorized 4.0/3.0; n ≥ 20 → linear fit 5.0/3.5 (spec §6.3).
11. **Weights enter the engine per plane** as `FrameWeight.normalized[plane]` (Plan 2's per-channel max-normalized weights); a frame whose minimum channel weight is below `minWeight` (0.005) is dropped from the group before integration, and a group below 3 frames is refused (math reference §3.7).
12. **Normalization pairs come from stage-3 measurements** (`ChannelMeasurement::location_scale()` per channel, spec §5.1) — the driver computes `rejection_pair`/`output_pair` (Plan 2's `integration::stats`) against the reference's measurement; `RejectionNormalization::Local` is refused in M1 with a named error.
13. **The Checkpoint B comparison reads the `integration` image of the external master XISF** — that file holds three images (`integration`, `rejection_low`, `rejection_high`); the probe asserts the first image's `id` before comparing. The external light integrations used **local** rejection normalization (LN, M2) with additive-with-scaling output normalization and linear fit 5.0/3.5; ours uses `scaleZeroOffset` — the noise target (±5 %) still applies, the rejected-fraction target (1–4 %) is read against 2.831 % (mono) / 2.51–2.79 % (OSC).
14. **The external log's timestamps are UTC, file mtimes local (+3 h)**: the masters of run `20260908121336` are the `_(1)` files (`masterLight_…mono_(1).xisf` 16:49 local = 13:49 UTC in the log).
15. **`MRS_LAYER0_GAIN` is renamed `MRS_LAYER1_GAIN`** (Plan 2 carry-forward: the estimator's doc says layer 1) in the same task that first cites it in a checkpoint number.

## Carry-forwards taken up here (from Plans 1–3)

- Plan 1: `read_rows_with_scratch` gets its consumer — `RegisteredSource` workers reuse a byte scratch and an f32 window buffer per worker (Task 3). The warp runs on the caller's rayon context (the injected pool installs it); the "global pool vs injected pool" note is closed by running `warp_rows` inside `pool.install` in the engine (it already is: `read_band_with_progress` is called from the engine thread and its workers are the source's own; no change). Worker panics inside `read_band_with_progress`'s scoped workers propagate as a panic to the build thread's `catch_unwind` (masters) — Plan 5's stacking job uses the same thread pattern; noted, not changed.
- Plan 2: `MRS_LAYER0_GAIN` rename (ruling 15); `background_residual` `data.len() == w·h` precondition added where the checkpoint calls it (Task 7); `winsorize`'s ±∞ guard is a `stacking::robust` matter for M4.
- Plan 3 (for Plan 4): the σ-weighted refit is effectively unweighted (`sx/sy` are PSF widths) — no effect on integration; the domain clamp guarantees bounded displacements everywhere the engine resamples; `Alignment.repaired`/`inlier_ratio`/overlap are not quality evidence after re-pairing (Plan 5's frame table).

## Carry-forwards for Plan 5 (collected as this plan is executed; the executing controller appends)

- Plan 5 owns: `stacking_runs`/`stacking_groups`/… tables, config hash + artifact reuse, the reference's `ROWORDER` and `INSTRUME`-differs flag, `ATH_STKF` (reference identity string) and `ATH_STKI` (run id) values, the copy-through source (the reference's calibrated file), deleting intermediates, events and the tab.

## Global Constraints

- Never name other stacking programs / codebases in code or comments (docs may say "the external stacker"; file formats such as `.xdrz`/`.xisf` may be named).
- Two backends in sync — this plan adds **no** command or route; `athenaeum-tauri`/`athenaeum-web` are untouched. Every new serde type that can reach JSON storage is `#[serde(rename_all = "camelCase")]` with `#[serde(default)]` where old JSON must stay valid.
- `tracing` only; zero `println!`/`eprintln!` outside `#[cfg(test)]` and `examples/`; log messages are short stable phrases with snake_case fields; new field names go into `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`'s dictionary in the same task.
- No new crate dependencies; `Cargo.toml` changes only by one `[[example]]` block; `Cargo.lock` unchanged.
- `cargo check -p athenaeum-core --no-default-features` stays clean: `stacking` is gated `all(render, solver)`, `integration` is gated `render`; the new `fits_writer::wcs` is ungated (it depends on `plate_solve::storage`, which is ungated).
- Masters byte-identical: every existing `integration` test passes unchanged; the pin test of Task 1 is mandatory.
- rustfmt only on newly created leaf files and on files that are rustfmt-clean before the task (`combine.rs`, `engine.rs`, `registered_source.rs`, `stats.rs` are clean — check with `rustfmt --check` before editing; `banded.rs` and the never-rustfmt list — `mod.rs`, `lib.rs`, `schema.rs`, `scanner/mod.rs`, `registration/db.rs`, `test_support.rs` — are hand-edit only).
- Commits as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit` with the two trailers `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY`.
- Owner data for the checkpoint (read-only): `~/Pictures/Calibration Test/LDN1272-WBPP/LDN1272-ATH/LDN 1272/camera_atr2600m/lights/c_*.fits` (208 mono) and `camera_zwoasi2600mcduo/lights/c_*_d.fits` (160 OSC); reference `c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits`; external masters `~/Pictures/Calibration Test/LDN1272-Output/master/masterLight_BIN-1_6224x4168_EXPOSURE-180.00s_FILTER-NoFilter_mono_(1).xisf` and `…_RGB_(1).xisf`; log `~/Pictures/Calibration Test/LDN1272-Output/logs/20260908121336.log`.

## File structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/integration/combine.rs` (modify) | `Sample` trait, generic rejections, `combine_pixel_weighted`, mask helpers, linear-fit dispersion |
| `crates/athenaeum-core/src/integration/engine.rs` (modify) | `band_loop` extraction, `run_banded_stack`, `StackParams`, `StackOutput`, `integrate_stack` |
| `crates/athenaeum-core/src/integration/registered_source.rs` (modify) | per-worker scratch reuse |
| `crates/athenaeum-core/src/stacking/integrate.rs` (create) | config types (`IntegrationConfig`, `RejectionChoice`, `NormalizationConfig`), Auto rule, per-frame normalization/weights, `GroupInput` → per-plane engine runs → `GroupOutput` + `GroupStats` |
| `crates/athenaeum-core/src/fits_writer/wcs.rs` (create) | `wcs_cards(&PlateSolveRecord)` |
| `crates/athenaeum-core/src/stacking/master_cards.rs` (create) | master-light card set, `master_file_name`, `write_master_light`, rejection-map writers |
| `crates/athenaeum-core/examples/integrate_probe.rs` (create) | Checkpoint B driver: register + measure + weigh + integrate a folder, write the master, compare with an external master |
| `crates/athenaeum-core/src/stacking/mod.rs`, `fits_writer/mod.rs`, `lib.rs` (hand edit) | module declarations |
| docs: spec §6.4 (two corrections), logging dictionary, CLAUDE.md module map, `docs/superpowers/research/2026-09-09-checkpoint-b-integration.md` | |

---

### Task 1: `combine.rs` — generic rejections, `combine_pixel_weighted`, survivor masks

**Files:**
- Modify: `crates/athenaeum-core/src/integration/combine.rs`

**Interfaces:**
- Consumes: `IntegrationRecipe`, `Rejection`, `Combination`, the four `reject_*` routines, `sort_asc`, `median_sorted`, `mean` (all in this file).
- Produces: `pub trait Sample`, `pub fn combine_pixel_weighted(work: &mut [(f32, u16)], out_values: &[f32], weights: &[f32], recipe: IntegrationRecipe, mask: &mut [u64]) -> (f32, usize)`, `pub fn mask_words(n: usize) -> usize`, `pub fn mask_get(mask: &[u64], i: usize) -> bool`, `pub fn mask_clear(mask: &mut [u64])`, `pub fn mask_set(mask: &mut [u64], i: usize)`.

- [ ] **Step 1: Read the file end to end** (`combine.rs`, ~860 lines) — the four routines compact survivors into `values[..kept]` in place and report whether the prefix is sorted; `combine_pixel` combines the prefix. Note every place a value is compared, sorted, summed or clamped.

- [ ] **Step 2: Write the failing pin test** (add to the `tests` module):

```rust
    /// SplitMix64, so the pin needs no dependency.
    fn rng_next(state: &mut u64) -> f64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn random_stack(state: &mut u64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| {
                let g = rng_next(state) + rng_next(state) + rng_next(state) - 1.5;
                let outlier = if rng_next(state) < 0.05 { 6.0 * (rng_next(state) - 0.5) } else { 0.0 };
                // Never an exact zero: the weighted path skips zero-valued
                // samples (missing coverage), the plain mean averages them.
                (0.2 + 0.01 * g as f32 + outlier as f32).max(1e-4)
            })
            .collect()
    }

    #[test]
    fn weighted_combiner_with_unit_weights_is_bit_identical_to_combine_pixel() {
        let recipes = [
            IntegrationRecipe::average(Rejection::None),
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.2, high: 0.1 }),
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 4.0, sigma_high: 3.0 }),
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 }),
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 }),
            IntegrationRecipe::median(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 3.0 }),
            IntegrationRecipe::median(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 }),
        ];
        let mut state = 0x5EED_1234u64;
        for (k, recipe) in recipes.iter().enumerate() {
            for trial in 0..300 {
                let n = 3 + (trial % 30);
                let stack = random_stack(&mut state, n);
                let mut plain = stack.clone();
                let (v_plain, rej_plain) = combine_pixel(&mut plain, *recipe);
                let mut work: Vec<(f32, u16)> =
                    stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
                let weights = vec![1.0f32; n];
                let mut mask = vec![0u64; mask_words(n)];
                let (v_w, rej_w) =
                    combine_pixel_weighted(&mut work, &stack, &weights, *recipe, &mut mask);
                assert_eq!(
                    v_plain.to_bits(),
                    v_w.to_bits(),
                    "recipe {k} trial {trial}: {v_plain} vs {v_w}"
                );
                assert_eq!(rej_plain, rej_w, "recipe {k} trial {trial}");
                let survivors = (0..n).filter(|&i| mask_get(&mask, i)).count();
                assert_eq!(survivors, n - rej_w, "recipe {k} trial {trial}");
            }
        }
    }
```

- [ ] **Step 3: Run it to see it fail** — `cargo test -p athenaeum-core combine::tests::weighted_combiner` → compile error (no `Sample`, no `combine_pixel_weighted`).

- [ ] **Step 4: The `Sample` trait and the generic routines.** Add near the top of the file:

```rust
/// A stack element the rejection routines can order and read: the plain
/// sample for master builds, a `(value, frame index)` pair for the
/// stacking engine, which needs to know WHICH frames survived.
pub trait Sample: Copy {
    fn value(self) -> f32;
}

impl Sample for f32 {
    #[inline]
    fn value(self) -> f32 {
        self
    }
}

impl Sample for (f32, u16) {
    #[inline]
    fn value(self) -> f32 {
        self.0
    }
}
```

Then make `sort_asc`, `median_sorted`, `mean`, `apply_rejection`, `reject_percentile`, `reject_sigma_clip`, `reject_winsorized` and `reject_linear_fit` generic: `fn f<T: Sample>(values: &mut [T], …)`, every read of a value becomes `values[i].value()`, every comparison sorts by `a.value().total_cmp(&b.value())` — **the same comparator the f32 version uses today** (if today's code uses `partial_cmp(...).unwrap_or(Equal)`, keep exactly that expression on `.value()`; do not change the ordering semantics). Sums and statistics stay `f64`/`f32` exactly as today, reading `.value()`. The winsorized routine's "clamp the working copy but sum the originals" logic must keep both copies as `T` (the clamped copy is `Vec<T>` built by mapping the value — for `(f32, u16)` the index rides along unchanged). `combine_pixel` keeps its signature (`&mut [f32]`) and body — it simply calls the generic routines with `T = f32`. This is the template for one routine; the others follow the same mechanical rule:

```rust
fn reject_sigma_clip<T: Sample>(values: &mut [T], sigma_low: f64, sigma_high: f64) -> (usize, bool) {
    let n = values.len();
    if n < 3 {
        return (n, false);
    }
    let mut kept = n;
    for _ in 0..MAX_REJECTION_ITERS {
        let slice = &values[..kept];
        let m = {
            let mut tmp: Vec<f32> = slice.iter().map(|s| s.value()).collect();
            sort_asc_f32(&mut tmp);
            median_sorted_f32(&tmp) as f64
        };
        let mean = slice.iter().map(|s| s.value() as f64).sum::<f64>() / kept as f64;
        let var = slice.iter().map(|s| (s.value() as f64 - mean).powi(2)).sum::<f64>() / kept as f64;
        let sigma = var.sqrt();
        if sigma == 0.0 {
            break;
        }
        let lo = m - sigma_low * sigma;
        let hi = m + sigma_high * sigma;
        let mut w = 0;
        for r in 0..kept {
            let v = values[r].value() as f64;
            if v >= lo && v <= hi {
                values[w] = values[r];
                w += 1;
            }
        }
        if w == kept {
            break;
        }
        if w == 0 {
            break;
        }
        kept = w;
        if kept < 2 {
            break;
        }
    }
    (kept, false)
}
```

**This template is illustrative of the mechanics, not of the arithmetic**: keep the existing routine's exact arithmetic (its median source, its σ formula, its all-rejected guard, its iteration cap) and only change the element type. If the existing routine sorts the survivors, sort `T` by `.value()`. Keep `sort_asc`/`median_sorted` generic too (`sort_asc<T: Sample>(v: &mut [T])` sorting by `.value()`; `median_sorted<T: Sample>(v: &[T]) -> f32`).

- [ ] **Step 5: Mask helpers and `combine_pixel_weighted`.** Add below `combine_pixel`:

```rust
// ── Survivor masks ───────────────────────────────────────────────────────────

/// Words a survivor mask needs for `n` frames.
#[inline]
pub fn mask_words(n: usize) -> usize {
    n.div_ceil(64)
}

#[inline]
pub fn mask_clear(mask: &mut [u64]) {
    mask.iter_mut().for_each(|w| *w = 0);
}

#[inline]
pub fn mask_set(mask: &mut [u64], i: usize) {
    mask[i / 64] |= 1u64 << (i % 64);
}

#[inline]
pub fn mask_get(mask: &[u64], i: usize) -> bool {
    mask[i / 64] & (1u64 << (i % 64)) != 0
}

/// Weighted combination of one pixel column (spec §6.2, math reference §3.2
/// steps 3, 5, 6).
///
/// `work[k] = (rejection-normalized value, frame index)` for every frame
/// with a usable sample (the caller has already dropped missing and
/// range-rejected samples); it is reordered in place. `out_values[i]` is
/// frame `i`'s OUTPUT-normalized value and `weights[i]` its weight, both
/// indexed by frame — only the indices present in `work` are read. The
/// rejection runs on `work`; the result is the weighted mean of the
/// survivors' `out_values` (samples with `out_values == 0` or `weight <= 0`
/// are skipped, math reference §3.6) or their median (weights ignored).
/// Every survivor's bit is set in `mask` (the caller clears it first) and
/// the rejected count is returned. All rejected → the median of every
/// `out_values` present in `work`, no bit set.
pub fn combine_pixel_weighted(
    work: &mut [(f32, u16)],
    out_values: &[f32],
    weights: &[f32],
    recipe: IntegrationRecipe,
    mask: &mut [u64],
) -> (f32, usize) {
    let n = work.len();
    if n == 0 {
        return (0.0, 0);
    }
    let (kept, _sorted) = apply_rejection(work, recipe.rejection);
    if kept == 0 {
        let mut all: Vec<f32> = work.iter().map(|&(_, i)| out_values[i as usize]).collect();
        sort_asc(&mut all);
        return (median_sorted(&all), n);
    }
    for &(_, i) in &work[..kept] {
        mask_set(mask, i as usize);
    }
    let value = match recipe.combination {
        Combination::Average => {
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for &(_, i) in &work[..kept] {
                let x = out_values[i as usize];
                let w = weights[i as usize];
                if x != 0.0 && w > 0.0 {
                    num += x as f64 * w as f64;
                    den += w as f64;
                }
            }
            if den > 0.0 {
                (num / den) as f32
            } else {
                // Every survivor was a zero-valued or zero-weighted sample:
                // the plain mean of the survivors, as the unweighted path.
                let mut vals: Vec<f32> = work[..kept].iter().map(|&(_, i)| out_values[i as usize]).collect();
                mean(&mut vals)
            }
        }
        Combination::Median => {
            let mut vals: Vec<f32> = work[..kept].iter().map(|&(_, i)| out_values[i as usize]).collect();
            sort_asc(&mut vals);
            median_sorted(&vals)
        }
    };
    (value, n - kept)
}
```

**Precision contract for the pin:** the existing `mean` must be read to see how it accumulates (f64 sum then cast, or f32). The weighted average above accumulates in f64 with `w = 1.0` exactly, so it is bit-identical to an f64-accumulated mean; if `mean` accumulates in **f32**, change the weighted loop to accumulate `num`/`den` in f32 the same way (`num += x * w; den += w;`) so the pin holds — report which it was. `x != 0.0` skipping is new behaviour relative to `mean` (which averages zeros too): the pin test's fixture never produces exact zeros except through `.max(0.0)` on a negative outlier — **make the fixture avoid zeros** (`.max(1e-4)`) if the pin fails only on a zero-valued sample, and say so; zero-skipping is the spec's rule for missing coverage, not a defect.

- [ ] **Step 6: Run the pin and the whole combine module** — `cargo test -p athenaeum-core combine::` → all pass, including every pre-existing `legacy_equivalence_*` and rejection test unchanged.

- [ ] **Step 7: Behaviour tests for the weighted path** (add to `tests`):

```rust
    #[test]
    fn weighted_average_weights_survivors_and_skips_zero_and_unweighted_samples() {
        // frames: 0 → 1.0 (w 3), 1 → 2.0 (w 1), 2 → 0.0 (w 1, missing coverage), 3 → 4.0 (w 0)
        let stack = [1.0f32, 2.0, 0.0, 4.0];
        let mut work: Vec<(f32, u16)> = stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [3.0f32, 1.0, 1.0, 0.0];
        let mut mask = vec![0u64; 1];
        let (v, rej) = combine_pixel_weighted(
            &mut work,
            &stack,
            &weights,
            IntegrationRecipe::average(Rejection::None),
            &mut mask,
        );
        assert_eq!(rej, 0);
        assert!((v - (3.0 * 1.0 + 1.0 * 2.0) / 4.0).abs() < 1e-6, "{v}");
        assert!((0..4).all(|i| mask_get(&mask, i)));
    }

    #[test]
    fn weighted_median_ignores_weights_and_mask_names_the_survivors() {
        let stack = [0.10f32, 0.11, 0.12, 0.13, 0.90];
        let mut work: Vec<(f32, u16)> = stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [1.0f32, 100.0, 1.0, 1.0, 1.0];
        let mut mask = vec![0u64; 1];
        let (v, rej) = combine_pixel_weighted(
            &mut work,
            &stack,
            &weights,
            IntegrationRecipe::median(Rejection::SigmaClip { sigma_low: 2.0, sigma_high: 2.0 }),
            &mut mask,
        );
        assert_eq!(rej, 1, "the 0.90 outlier");
        assert!(!mask_get(&mask, 4) && (0..4).all(|i| mask_get(&mask, i)));
        assert!((v - 0.115).abs() < 1e-6, "median of the four survivors: {v}");
    }

    #[test]
    fn rejection_normalized_values_decide_survival_but_output_values_are_averaged() {
        // Rejection copy says frame 2 is an outlier; its output value is ordinary.
        let rej = [1.0f32, 1.0, 9.0, 1.0, 1.0];
        let out = [0.5f32, 0.5, 0.5, 0.5, 0.5];
        let mut work: Vec<(f32, u16)> = rej.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [1.0f32; 5];
        let mut mask = vec![0u64; 1];
        let (v, rejected) = combine_pixel_weighted(
            &mut work,
            &out,
            &weights,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 2.0, sigma_high: 2.0 }),
            &mut mask,
        );
        assert_eq!(rejected, 1);
        assert!(!mask_get(&mask, 2));
        assert_eq!(v, 0.5);
    }

    #[test]
    fn all_rejected_falls_back_to_the_median_of_the_output_values() {
        // PercentileClip with zero thresholds rejects everything but the median.
        let stack = [0.2f32, 0.3, 0.4];
        let mut work: Vec<(f32, u16)> = stack.iter().enumerate().map(|(i, &v)| (v, i as u16)).collect();
        let weights = [1.0f32; 3];
        let mut mask = vec![0u64; 1];
        let mut plain = stack.to_vec();
        let (v_plain, r_plain) = combine_pixel(
            &mut plain,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.0, high: 0.0 }),
        );
        let (v, r) = combine_pixel_weighted(
            &mut work,
            &stack,
            &weights,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.0, high: 0.0 }),
            &mut mask,
        );
        assert_eq!((v.to_bits(), r), (v_plain.to_bits(), r_plain));
    }

    #[test]
    fn mask_helpers_cover_word_boundaries() {
        let mut m = vec![0u64; mask_words(130)];
        assert_eq!(m.len(), 3);
        for i in [0usize, 63, 64, 127, 128, 129] {
            mask_set(&mut m, i);
        }
        assert!(mask_get(&m, 0) && mask_get(&m, 63) && mask_get(&m, 64) && mask_get(&m, 129));
        assert!(!mask_get(&m, 1) && !mask_get(&m, 65));
        mask_clear(&mut m);
        assert!(m.iter().all(|&w| w == 0));
    }
```

If the percentile-clip all-rejected fixture does not actually reject everything under the existing routine's semantics, replace it with `Rejection::SigmaClip { sigma_low: 0.0, sigma_high: 0.0 }` on `[0.2, 0.3, 0.4]` and report which; the assertion (bit equality with `combine_pixel`) is the point.

- [ ] **Step 8: Gates** — `cargo test -p athenaeum-core integration::` (every existing test unchanged), `cargo check -p athenaeum-core --no-default-features`, `cargo check --workspace --all-targets` zero new warnings, `rustfmt --edition 2021 crates/athenaeum-core/src/integration/combine.rs` (it is rustfmt-clean today — verify with `--check` before editing; if not, hand-edit only).

- [ ] **Step 9: Commit** — `feat(integration): generic rejection routines and the weighted, mask-reporting combiner`.

---

### Task 2: `combine.rs` — linear-fit dispersion parity

**Files:**
- Modify: `crates/athenaeum-core/src/integration/combine.rs`

**Interfaces:** `reject_linear_fit` only; no signature change.

- [ ] **Step 1: Read the routine.** Today `d` is the mean absolute deviation of the residuals; the spec (§6.3) and the math reference (§3.4) want `s = 2·adev·sqrt(1 + b²)` where `b` is the fitted slope against rank and `adev` the mean absolute deviation from the line — so thresholds compare with sigma clipping.

- [ ] **Step 2: Write the failing test:**

```rust
    #[test]
    fn linear_fit_dispersion_is_twice_adev_times_slope_factor() {
        // A perfect ramp of slope 0.01 per rank plus one spike. With the
        // old dispersion (adev alone) a 2.5-adev deviation is rejected at
        // thresholds 3.0; with s = 2·adev·sqrt(1 + b²) it survives.
        let mut ramp: Vec<f32> = (0..20).map(|j| 0.5 + 0.01 * j as f32).collect();
        // deviations: ±0.004 alternating, one at +0.009 (≈ 2.25 adev of 0.004)
        for (j, v) in ramp.iter_mut().enumerate() {
            *v += if j % 2 == 0 { 0.004 } else { -0.004 };
        }
        ramp[10] += 0.009;
        let mut a = ramp.clone();
        let (_, rejected) = combine_pixel(
            &mut a,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert_eq!(rejected, 0, "a 2.25-adev deviation survives at 3.0 with the doubled dispersion");
        let mut b = ramp.clone();
        b[10] += 0.05; // ≈ 12 adev — still rejected
        let (_, rejected) = combine_pixel(
            &mut b,
            IntegrationRecipe::average(Rejection::LinearFitClip { sigma_low: 3.0, sigma_high: 3.0 }),
        );
        assert_eq!(rejected, 1);
    }
```

Run it: the first assertion fails today (rejected == 1).

- [ ] **Step 3: Change the dispersion** in `reject_linear_fit`: where `d` (the mean absolute deviation of residuals) is computed, define `let s = 2.0 * adev * (1.0 + b * b).sqrt();` with `b` the fitted slope per rank unit, and use `s` in both threshold comparisons. Update the routine's doc comment: "dispersion `s = 2·adev·sqrt(1 + b²)` (math reference §3.4) so the thresholds compare with sigma clipping". Keep everything else (fit, iteration, guards) unchanged.

- [ ] **Step 4: Re-pin the affected tests.** Run `cargo test -p athenaeum-core combine::` — `linear_fit_keeps_clean_ramp`, `linear_fit_rejects_spike`, `linear_fit_terminates_on_constant_stack`, `linear_fit_all_rejected_uses_intact_stack_not_corruption` and the Task 1 pin (which uses linear fit at 5.0/3.5) must pass; if a spike fixture is no longer rejected at its threshold, double the spike or halve the threshold in **that test only** and record the measured old/new rejection in the test's comment ("dispersion doubled 2026-09-09; spike raised from x to y"). Do not touch any other test.

- [ ] **Step 5: Commit** — `fix(integration): linear-fit clipping dispersion follows the reference (2·adev·sqrt(1+b²))`.

---

### Task 3: `engine.rs` — one band loop, the weighted stacking path; `RegisteredSource` scratch reuse

**Files:**
- Modify: `crates/athenaeum-core/src/integration/engine.rs`
- Modify: `crates/athenaeum-core/src/integration/registered_source.rs`

**Interfaces:**
- Consumes: `combine_pixel_weighted`, mask helpers (Task 1); `FrameSource`, `BandPlanes`, `IoPolicy`, `EngineProgress`, `IntegrationOutput`; `NormalizationPair` (`integration::stats`).
- Produces:

```rust
/// Per-frame inputs of the stacking path, all indexed by the source's frame order.
pub struct StackParams<'a> {
    /// Rejection-normalization pair per frame (applied to the working copy).
    pub rejection: &'a [NormalizationPair],
    /// Output-normalization pair per frame (applied to the averaged values).
    pub output: &'a [NormalizationPair],
    /// Weight per frame (the plane's normalized weight; ≥ 0).
    pub weights: &'a [f32],
    /// Range rejection on RAW values: reject `raw <= range_low` (when Some) and `raw >= range_high` (when Some).
    pub range_low: Option<f32>,
    pub range_high: Option<f32>,
    /// Accumulate per-pixel low/high rejection counts.
    pub rejection_maps: bool,
}

pub struct StackOutput {
    pub base: IntegrationOutput,
    /// Per-pixel rejected-sample counts, low and high sides (`Some` when requested).
    pub rejection_low: Option<Vec<f32>>,
    pub rejection_high: Option<Vec<f32>>,
    /// Rejected samples per frame (range + algorithm), indexed by frame.
    pub rejected_per_frame: Vec<u64>,
    /// Samples the frame actually contributed (finite, in coverage), per frame.
    pub samples_per_frame: Vec<u64>,
    pub rejected_low: u64,
    pub rejected_high: u64,
}

pub fn integrate_stack<S: FrameSource + ?Sized>(
    src: &S,
    params: &StackParams<'_>,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<StackOutput, IntegrationError>;
```

`integrate_registered` (Plan 1) stays as the unweighted convenience and now delegates to `integrate_stack` with identity pairs, unit weights, no range rejection and no maps — its Plan 1 test must still pass.

- [ ] **Step 1: Extract the band loop.** In `run_banded`, everything from `let mut planes = BandPlanes::new(src);` through the end of the `for (band_idx, y0)` loop is the skeleton; the per-band combine (`pool.install(|| out_band.par_chunks_mut(w)…)`) is the only part the two paths differ in. Introduce:

```rust
/// What a band-combine closure receives: the decoded band, the output rows
/// it must fill, the band's first global row, and the progress hook it must
/// tick once per output row (`rows_done_total, total_rows, bytes_read, bytes_total`).
struct BandJob<'a> {
    planes: &'a BandPlanes,
    out_band: &'a mut [f32],
    y0: usize,
    rows: usize,
    width: usize,
}

/// The read / progress / cancel / timing skeleton shared by every banded
/// integration. `combine(job, tick)` fills `job.out_band` (rows × width) and
/// calls `tick()` once per finished row.
#[allow(clippy::too_many_arguments)]
fn band_loop<S: FrameSource + ?Sized>(
    src: &S,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
    io: IoPolicy,
    out: &mut [f32],
    combine: &(dyn Fn(BandJob<'_>, &(dyn Fn() + Sync)) -> Result<(), IntegrationError> + Sync),
) -> Result<BandStats, IntegrationError>;

struct BandStats {
    read_duration: std::time::Duration,
    combine_duration: std::time::Duration,
    band_rows: usize,
    bands: usize,
    bytes_read: u64,
}
```

Move the loop body into `band_loop` verbatim — the `bytes_before_this_band`/`band_bytes_so_far` Mutex logic, the three cancel checks, `read_band_with_progress`, the timings, the `(progress.on_band)(…)` end-of-band call — and replace the `pool.install(…)` block by `pool.install(|| combine(BandJob { planes: &planes, out_band, y0, rows, width: w }, &tick))?` where `tick` is the closure that today does `let done = rows_combined.fetch_add(1, Relaxed) + 1; if done % COMBINE_TICK_ROWS == 0 || done == h { (progress.on_combine)(done, h, bytes_read, bytes_total); }` (`bytes_read` frozen for the band as today — capture the value, not the variable). Keep every comment that explains a fix-wave decision next to the code it explains.

`run_banded` becomes: allocate `out`, the `bad_samples`/`all_bad`/`rejected` counters, call `band_loop` with a closure that is **today's per-pixel body verbatim** (precal, `scales[i]`, finiteness, `combine_pixel`), then the non-finite output check and the `IntegrationOutput` assembly. **Every existing engine test passes unchanged — that is the pin for this step**; run `cargo test -p athenaeum-core engine::` before going on.

- [ ] **Step 2: Write the failing stacking-path test** (in `engine.rs`'s `tests`, next to the existing fixtures; it uses the module's `write`/`pool`/`io`/`nop` helpers — read them first):

```rust
    #[test]
    fn stack_path_weights_normalizes_and_counts_rejections_per_side_and_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        // Four frames: 0 and 1 flat 0.20; 2 flat 0.40 (twice the level — the
        // output pair maps it back onto 0.20); 3 flat 0.20 with a hot pixel
        // 0.95 at (3,2) and a dead pixel 0.0 at (5,5) (range-low).
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.20),
            write(dir.path(), "b.fits", w, h, |_, _| 0.20),
            write(dir.path(), "c.fits", w, h, |_, _| 0.40),
            write(dir.path(), "d.fits", w, h, |x, y| {
                if (x, y) == (3, 2) { 0.95 } else if (x, y) == (5, 5) { 0.0 } else { 0.20 }
            }),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = NormalizationPair::IDENTITY;
        let half = NormalizationPair { scale: 0.5, offset: 0.0 };
        let params = StackParams {
            rejection: &[ident, ident, half, ident],
            output: &[ident, ident, half, ident],
            weights: &[1.0, 1.0, 1.0, 3.0],
            range_low: Some(0.0),
            range_high: None,
            rejection_maps: true,
        };
        let progress = EngineProgress { on_band: &nop(), on_combine: &nop() };
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 2.0 }),
            &pool(),
            &AtomicBool::new(false),
            progress,
            io(1 << 20),
        )
        .unwrap();
        let px = |x: usize, y: usize| out.base.data[y * w + x];
        // An ordinary pixel: all four survive, weighted mean of 0.20s = 0.20.
        assert!((px(0, 0) - 0.20).abs() < 1e-6, "{}", px(0, 0));
        // Hot pixel: frame 3 rejected high; the rest average to 0.20.
        assert!((px(3, 2) - 0.20).abs() < 1e-6, "{}", px(3, 2));
        assert_eq!(out.rejection_high.as_ref().unwrap()[2 * w + 3], 1.0);
        assert_eq!(out.rejection_low.as_ref().unwrap()[2 * w + 3], 0.0);
        // Dead pixel: frame 3 range-rejected low, counted low.
        assert!((px(5, 5) - 0.20).abs() < 1e-6, "{}", px(5, 5));
        assert_eq!(out.rejection_low.as_ref().unwrap()[5 * w + 5], 1.0);
        assert_eq!(out.rejected_per_frame, vec![0, 0, 0, 2]);
        assert_eq!(out.samples_per_frame, vec![128, 128, 128, 128]);
        assert_eq!((out.rejected_low, out.rejected_high), (1, 1));
        assert!(out.base.data.iter().all(|v| (v - 0.20).abs() < 1e-6));
    }

    #[test]
    fn stack_path_with_unit_inputs_equals_the_master_path() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (24usize, 10usize);
        let paths: Vec<_> = (0..7)
            .map(|i| {
                write(dir.path(), &format!("f{i}.fits"), w, h, move |x, y| {
                    0.1 + 0.01 * ((x * 7 + y * 3 + i * 11) % 13) as f32 + if (x + y + i) % 17 == 0 { 0.3 } else { 0.0 }
                })
            })
            .collect();
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let recipe = IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 });
        let master = integrate_bias_like(&paths, recipe, &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() }, io(1 << 20)).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 7];
        let params = StackParams {
            rejection: &ident, output: &ident, weights: &[1.0; 7],
            range_low: None, range_high: None, rejection_maps: false,
        };
        let stack = integrate_stack(&src, &params, recipe, &pool(), &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() }, io(1 << 20)).unwrap();
        assert_eq!(master.data.len(), stack.base.data.len());
        for (i, (a, b)) in master.data.iter().zip(&stack.base.data).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "pixel {i}: {a} vs {b}");
        }
        assert_eq!(master.rejected_fraction, stack.base.rejected_fraction);
        assert!(stack.rejection_low.is_none() && stack.rejection_high.is_none());
    }
```

(`write` in the engine tests takes a closure `Fn(usize, usize) -> f32`; if its signature differs, adapt the calls, not the intent. If `nop()` returns a closure by value, take a reference as the existing tests do.)

- [ ] **Step 3: Run to see them fail** (no `StackParams`/`integrate_stack`).

- [ ] **Step 4: Implement the stacking path.** Add the types from the Interfaces block, then:

```rust
/// Weighted, normalized banded integration with survivor accounting (spec
/// §6.1–6.2). Per sample: finiteness (missing coverage is skipped, not
/// rejected), range rejection on the raw value, then the rejection copy
/// `raw·rs + ro` decides survival and the output copy `raw·os + oo` is what
/// the survivors' weighted mean (or median) is taken over.
#[allow(clippy::too_many_arguments)]
pub fn integrate_stack<S: FrameSource + ?Sized>(
    src: &S,
    params: &StackParams<'_>,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<StackOutput, IntegrationError> {
    use rayon::prelude::*;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    if params.rejection.len() != n || params.output.len() != n || params.weights.len() != n {
        return Err(IntegrationError::BadInput(format!(
            "stack params for {} / {} / {} frames, source has {n}",
            params.rejection.len(),
            params.output.len(),
            params.weights.len()
        )));
    }
    if params.weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
        return Err(IntegrationError::BadInput("weights must be finite and non-negative".into()));
    }
    if n > u16::MAX as usize {
        return Err(IntegrationError::BadInput(format!("{n} frames exceed the 65535-frame stack limit")));
    }
    let mut out = vec![0f32; w * h];
    let rejected = AtomicUsize::new(0);
    let rejected_low = AtomicU64::new(0);
    let rejected_high = AtomicU64::new(0);
    let bad_samples: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    let rejected_per_frame: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(0)).collect();
    let samples_per_frame: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(0)).collect();
    let all_bad = AtomicUsize::new(0);
    let maps = params.rejection_maps;
    let mut map_low = if maps { vec![0f32; w * h] } else { Vec::new() };
    let mut map_high = if maps { vec![0f32; w * h] } else { Vec::new() };
    let map_low_cell = std::sync::Mutex::new(&mut map_low);
    let map_high_cell = std::sync::Mutex::new(&mut map_high);
    // The maps are written per ROW by the worker that owns that row — the
    // Mutex above only hands out the row slices once per band (below),
    // never per pixel.

    let stats = band_loop(src, pool, cancel, &progress, io, &mut out, &|job, tick| {
        let BandJob { planes, out_band, y0, rows, width } = job;
        let _ = rows;
        let words = combine::mask_words(n);
        let mut low_guard = map_low_cell.lock().unwrap();
        let mut high_guard = map_high_cell.lock().unwrap();
        let low_band: &mut [f32] = if maps { &mut low_guard[y0 * width..(y0 + out_band.len() / width) * width] } else { &mut [] };
        let high_band: &mut [f32] = if maps { &mut high_guard[y0 * width..(y0 + out_band.len() / width) * width] } else { &mut [] };
        let low_rows: Vec<&mut [f32]> = if maps { low_band.chunks_mut(width).collect() } else { Vec::new() };
        let high_rows: Vec<&mut [f32]> = if maps { high_band.chunks_mut(width).collect() } else { Vec::new() };
        out_band
            .par_chunks_mut(width)
            .enumerate()
            .zip_eq(low_rows.into_par_iter().zip_eq(high_rows.into_par_iter()).map(Some).chain(rayon::iter::repeatn(None, if maps { 0 } else { usize::MAX })).take_any(if maps { usize::MAX } else { 0 }))
            .for_each(|((row_in_band, out_row), map_rows)| {
                let mut work: Vec<(f32, u16)> = Vec::with_capacity(n);
                let mut out_vals = vec![0f32; n];
                let mut mask = vec![0u64; words];
                for (x, out_px) in out_row.iter_mut().enumerate() {
                    work.clear();
                    combine::mask_clear(&mut mask);
                    let idx = row_in_band * width + x;
                    let mut low_here = 0u32;
                    let mut high_here = 0u32;
                    for i in 0..n {
                        let raw = planes.sample(i, idx);
                        if !raw.is_finite() {
                            bad_samples[i].fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        samples_per_frame[i].fetch_add(1, Ordering::Relaxed);
                        if let Some(lo) = params.range_low {
                            if raw <= lo {
                                low_here += 1;
                                rejected_per_frame[i].fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                        }
                        if let Some(hi) = params.range_high {
                            if raw >= hi {
                                high_here += 1;
                                rejected_per_frame[i].fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                        }
                        let rej = params.rejection[i].apply(raw);
                        let outv = params.output[i].apply(raw);
                        if !rej.is_finite() || !outv.is_finite() {
                            bad_samples[i].fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        out_vals[i] = outv;
                        work.push((rej, i as u16));
                    }
                    if work.is_empty() {
                        *out_px = 0.0;
                        all_bad.fetch_add(1, Ordering::Relaxed);
                    } else {
                        let present: Vec<u16> = work.iter().map(|&(_, i)| i).collect();
                        let (val, rej_count) = combine::combine_pixel_weighted(
                            &mut work,
                            &out_vals,
                            params.weights,
                            recipe,
                            &mut mask,
                        );
                        *out_px = val;
                        if rej_count > 0 {
                            rejected.fetch_add(rej_count, Ordering::Relaxed);
                            // Side of each rejected sample: below the survivors'
                            // median of the rejection copy → low, else high.
                            let mut surv: Vec<f32> = work
                                .iter()
                                .filter(|&&(_, i)| combine::mask_get(&mask, i as usize))
                                .map(|&(v, _)| v)
                                .collect();
                            let median = if surv.is_empty() {
                                let mut all: Vec<f32> = work.iter().map(|&(v, _)| v).collect();
                                all.sort_by(|a, b| a.total_cmp(b));
                                all[all.len() / 2]
                            } else {
                                surv.sort_by(|a, b| a.total_cmp(b));
                                surv[surv.len() / 2]
                            };
                            for &(v, i) in work.iter() {
                                if !combine::mask_get(&mask, i as usize) {
                                    rejected_per_frame[i as usize].fetch_add(1, Ordering::Relaxed);
                                    if v < median { low_here += 1 } else { high_here += 1 }
                                }
                            }
                        }
                        let _ = present;
                    }
                    if low_here > 0 { rejected_low.fetch_add(low_here as u64, Ordering::Relaxed); }
                    if high_here > 0 { rejected_high.fetch_add(high_here as u64, Ordering::Relaxed); }
                    if let Some((low_row, high_row)) = map_rows.as_ref() {
                        let _ = (low_row, high_row);
                    }
                    if maps {
                        if let Some((low_row, high_row)) = &map_rows {
                            let _ = (low_row, high_row);
                        }
                    }
                }
                tick();
            });
        Ok(())
    })?;
    // …
}
```

**The zip of map rows above is deliberately NOT the final shape** — it is unreadable. Implement the map accumulation the simple way instead: give each row worker its own two `Vec<f32>` row buffers (`low_row`, `high_row`, length `width`, zeroed per row), write `low_here`/`high_here` into them per pixel, and after the row is done copy them into the band's map slices through a per-band `Mutex<Vec<(usize, Vec<f32>, Vec<f32>)>>` **or** — simpler still and what this plan requires — split the maps into per-row mutable chunks **before** the parallel loop with `par_chunks_mut(width)` on `map_low`/`map_high` slices of the band, zipped with `out_band.par_chunks_mut(width)` via `rayon`'s `zip` (all three iterators have exactly `rows` items when `maps` is true; when `maps` is false use two empty dummy vectors of length `rows * width` allocated once per run — 2 × w × h × 4 bytes is the same size as the maps themselves, so allocate them only once, outside the closure, and reuse across bands). With that, the per-pixel code is: `low_row[x] = low_here as f32; high_row[x] = high_here as f32;` when `maps`. Delete the `present`/`map_rows` scaffolding from the sketch. The per-worker `work`/`out_vals`/`mask` buffers stay as written (allocated once per row, not per pixel).

After `band_loop` returns: the same non-finite output check as `run_banded`, then

```rust
    let total_samples = (w * h * n).max(1);
    Ok(StackOutput {
        base: IntegrationOutput {
            width: w,
            height: h,
            data: out,
            rejected_fraction: rejected.load(Ordering::Relaxed) as f64 / total_samples as f64,
            flat_norm: None,
            bad_samples_per_frame: bad_samples.into_iter().map(|a| a.into_inner()).collect(),
            all_bad_pixels: all_bad.into_inner(),
            read_duration: stats.read_duration,
            combine_duration: stats.combine_duration,
            band_rows: stats.band_rows,
            bands: stats.bands,
            bytes_read: stats.bytes_read,
        },
        rejection_low: maps.then_some(map_low),
        rejection_high: maps.then_some(map_high),
        rejected_per_frame: rejected_per_frame.into_iter().map(|a| a.into_inner()).collect(),
        samples_per_frame: samples_per_frame.into_iter().map(|a| a.into_inner()).collect(),
        rejected_low: rejected_low.into_inner(),
        rejected_high: rejected_high.into_inner(),
    })
```

`rejected_fraction` counts algorithm rejections only (as the master path); the checkpoint compares `(rejected_low + rejected_high) / Σ samples_per_frame`, which includes range rejections, with the external "Total" line — document both on the struct.

`integrate_registered` becomes a thin wrapper: identity pairs, unit weights, `range_low: None`, `range_high: None`, `rejection_maps: false`, returning `.base` (its signature and the Plan 1 test are unchanged).

- [ ] **Step 5: `RegisteredSource` scratch reuse.** In `read_band_with_progress`, each worker (the `workers == 1` loop and each scoped worker thread) owns `let mut raw_scratch: Vec<u8> = Vec::new(); let mut src_buf: Vec<f32> = Vec::new();` and passes them to `fill_frame(i, y0, rows, dst, &mut raw_scratch, &mut src_buf)`, which calls `reader.read_rows_with_scratch(self.plane, sy0, src_rows, &mut src_buf[..src_rows * sw], &mut raw_scratch)` after `src_buf.resize(src_rows * sw, 0.0)` (grow only). The `Whole` window fallback reuses the same buffers. Existing `registered_source` tests pass unchanged; add one assertion-free smoke: nothing — the existing `integrating_shifted_frames_yields_an_aligned_average_with_exact_coverage_accounting` test exercises multi-band multi-frame fills through the reused buffers.

- [ ] **Step 6: Gates** — `cargo test -p athenaeum-core integration::` (all, including Plan 1's registered-source tests), headless check, workspace check (zero new warnings), rustfmt on `engine.rs` and `registered_source.rs` if they were clean before (`--check` first).

- [ ] **Step 7: Commit** — `feat(integration): one band loop; weighted, normalized stacking path with rejection maps and per-frame accounting`.

---

### Task 4: `stacking/integrate.rs` — configuration, the Auto rule, per-frame normalization and weights, the group driver

**Files:**
- Create: `crates/athenaeum-core/src/stacking/integrate.rs`
- Modify (hand edit): `crates/athenaeum-core/src/stacking/mod.rs` — add `pub mod integrate;` and one sentence to the module doc.

**Interfaces:**
- Consumes: `integration::{combine::{IntegrationRecipe, Rejection, Combination}, engine::{integrate_stack, StackParams, StackOutput, EngineProgress}, registered_source::{RegisteredSource, RegisteredFrame}, stats::{LocationScale, OutputNormalization, RejectionNormalization, ScaleEstimator, NormalizationPair, output_pair, rejection_pair}, io_policy::IoPolicy, IntegrationError}`, `stacking::measure::{FrameMeasurement, measure_plane, MeasureOptions, ADU_SCALE}`, `stacking::weights::FrameWeight`, `stacking::psf_signal::noise_mrs`, `geometry::PixelMap`, `resample::Interpolation`.
- Produces (all `pub`):

```rust
/// spec §9.2 `integration:`; every field defaulted, camelCase on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IntegrationConfig {
    pub combination: Combination,          // serde: the master builder's snake_case names ("average"/"median") — one enum, one spelling
    pub rejection: RejectionChoice,
    pub min_weight: f64,                    // 0.005
    pub range_low: Option<f64>,             // Some(0.0)
    pub range_high: Option<f64>,            // None (0.98 when the user turns it on)
    pub write_rejection_maps: bool,         // false
}

/// spec §9.2 `normalization:` (global part; `local` is M2 and is carried as an opaque, defaulted value).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NormalizationConfig {
    pub output: OutputNormalization,        // additiveWithScaling
    pub rejection: RejectionNormalization,  // scaleZeroOffset
    pub scale_estimator: ScaleEstimator,    // bwmv
}

/// spec §6.3: the user's rejection choice, resolved per group size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "method")]
pub enum RejectionChoice {
    Auto,
    None,
    PercentileClip { low: f64, high: f64 },
    SigmaClip { sigma_low: f64, sigma_high: f64 },
    WinsorizedSigma { sigma_low: f64, sigma_high: f64 },
    LinearFitClip { sigma_low: f64, sigma_high: f64 },
}

impl RejectionChoice {
    /// n < 8 → percentile 0.2/0.1; 8 ≤ n < 20 → Winsorized 4.0/3.0; n ≥ 20 → linear fit 5.0/3.5.
    pub fn resolve(self, n: usize) -> Rejection;
}

/// One frame of a group, ready to integrate.
pub struct StackFrame {
    pub path: PathBuf,
    /// Subject → reference (identity for the reference frame).
    pub map: PixelMap,
    pub measurement: FrameMeasurement,
    pub weight: FrameWeight,
    pub exposure_s: f64,
    /// `DATE-OBS` as stored (ISO-8601 text), for the header's earliest/latest.
    pub date_obs: Option<String>,
}

pub struct GroupInput<'a> {
    pub frames: &'a [StackFrame],
    /// Index into `frames` of the reference (its measurement normalizes the others).
    pub reference: usize,
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub interpolation: Interpolation,
    pub clamping: f32,
    pub integration: &'a IntegrationConfig,
    pub normalization: &'a NormalizationConfig,
}

pub struct GroupStats {
    pub frames: usize,
    pub included: usize,
    pub dropped_below_min_weight: usize,
    pub recipe: String,                 // IntegrationRecipe::describe()
    pub rejected_low_fraction: f64,     // Σ rejected_low / Σ samples, over all planes
    pub rejected_high_fraction: f64,
    pub rejected_per_frame: Vec<f64>,   // per included frame, over all planes
    pub master_noise: Vec<f64>,         // MRS noise of the master per plane, native units
    pub master_location: Vec<f64>,
    pub master_scale: Vec<f64>,
    pub best_sub_noise: Vec<f64>,       // per plane, of the included frame with the highest weight
    pub master_psf_snr: Vec<f64>,
    pub best_sub_psf_snr: Vec<f64>,
    pub snr_gain: Vec<f64>,             // master_psf_snr / best_sub_psf_snr
    pub master_fwhm_px: Vec<f64>,
    pub master_eccentricity: Vec<f64>,
    pub weighted_exposure_s: f64,       // Σ exposure_i · w_i (mean over planes of the normalized weight)
    pub total_exposure_s: f64,
    pub read_ms: u64,
    pub combine_ms: u64,
    pub bytes_read: u64,
}

pub struct GroupOutput {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    /// Planar, `channels × width × height`.
    pub data: Vec<f32>,
    pub rejection_low: Option<Vec<f32>>,   // same layout, when requested
    pub rejection_high: Option<Vec<f32>>,
    /// Indices into the input `frames` that were integrated (after the min-weight drop), in engine order.
    pub included: Vec<usize>,
    pub stats: GroupStats,
}

pub struct GroupProgress<'a> {
    /// `(plane_index, planes_total)` at the start of each plane.
    pub on_plane: &'a (dyn Fn(usize, usize) + Sync),
    pub engine: EngineProgress<'a>,
}

pub fn integrate_group(
    input: &GroupInput<'_>,
    measure: &MeasureOptions,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &GroupProgress<'_>,
    io: IoPolicy,
) -> Result<GroupOutput, IntegrationError>;
```

- [ ] **Step 1: Failing tests** (in the new file's `tests` module; synthetic frames through `test_support::gaussian_field` + `fits_writer::write_fits_f32`; `measure_frame` from Plan 2 gives real measurements — call it on each synthetic frame with `MeasureOptions::default()`; if a synthetic 200×150 field yields too few stars for a PSF fit, use 400×300 with 12 stars — report the size used):

```rust
    #[test]
    fn auto_rule_follows_the_group_size() {
        assert_eq!(RejectionChoice::Auto.resolve(3), Rejection::PercentileClip { low: 0.2, high: 0.1 });
        assert_eq!(RejectionChoice::Auto.resolve(7), Rejection::PercentileClip { low: 0.2, high: 0.1 });
        assert_eq!(RejectionChoice::Auto.resolve(8), Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 });
        assert_eq!(RejectionChoice::Auto.resolve(19), Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 });
        assert_eq!(RejectionChoice::Auto.resolve(20), Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 });
        assert_eq!(RejectionChoice::Auto.resolve(208), Rejection::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 });
        assert_eq!(
            RejectionChoice::SigmaClip { sigma_low: 2.0, sigma_high: 2.5 }.resolve(500),
            Rejection::SigmaClip { sigma_low: 2.0, sigma_high: 2.5 }
        );
    }

    #[test]
    fn config_serde_names_match_the_spec() {
        let d: IntegrationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(d.combination, Combination::Average);
        assert_eq!(d.rejection, RejectionChoice::Auto);
        assert_eq!(d.min_weight, 0.005);
        assert_eq!(d.range_low, Some(0.0));
        assert_eq!(d.range_high, None);
        assert!(!d.write_rejection_maps);
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["rejection"]["method"], "auto");
        assert_eq!(j["minWeight"], 0.005);
        assert_eq!(j["writeRejectionMaps"], false);
        let explicit: IntegrationConfig = serde_json::from_str(
            r#"{"rejection":{"method":"linearFitClip","sigmaLow":5.0,"sigmaHigh":3.5},"rangeHigh":0.98}"#,
        )
        .unwrap();
        assert_eq!(explicit.rejection, RejectionChoice::LinearFitClip { sigma_low: 5.0, sigma_high: 3.5 });
        assert_eq!(explicit.range_high, Some(0.98));
        let n: NormalizationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(serde_json::to_value(&n).unwrap()["output"], "additiveWithScaling");
        assert_eq!(serde_json::to_value(&n).unwrap()["rejection"], "scaleZeroOffset");
        assert_eq!(serde_json::to_value(&n).unwrap()["scaleEstimator"], "bwmv");
    }

    #[test]
    fn local_rejection_normalization_is_refused_in_m1() {
        // build a 3-frame group input with NormalizationConfig { rejection: Local, .. }
        // → integrate_group returns Err(IntegrationError::BadInput(msg)) with msg containing "local normalization"
    }

    #[test]
    fn frames_below_min_weight_are_dropped_and_fewer_than_three_is_refused() {
        // 4 synthetic frames; weights normalized [1.0, 0.8, 0.5, 0.001] → the last is dropped (included.len() == 3,
        // stats.dropped_below_min_weight == 1); with weights [1.0, 0.001, 0.001, 0.001] → Err(BadInput) containing "fewer than 3".
    }

    #[test]
    fn integrating_a_shifted_brighter_frame_lands_on_the_reference_level() {
        // Three synthetic frames of the same star field: frame 0 (reference) background 100, frame 1 = frame 0 shifted (2.4, −1.3) px,
        // frame 2 = frame 0 × 2.0 (twice the level) shifted (−5.0, 3.0). Maps: identity, and the inverse shifts.
        // Measurements via measure_frame (location/scale per plane). With additiveWithScaling + scaleZeroOffset:
        //   the master's background must be within 1 % of 100 (frame 2 was mapped back by its output pair),
        //   star centroids within 0.05 px of the reference positions (Plan 1's registered-source test style),
        //   stats.included == vec![0, 1, 2], stats.master_noise[0] > 0, stats.snr_gain[0] > 1.0,
        //   stats.weighted_exposure_s == Σ exposure_i · mean normalized weight_i (compute by hand from the weights passed).
    }

    #[test]
    fn a_three_plane_group_integrates_plane_by_plane_with_one_map_set_per_plane() {
        // Two-plane or three-plane synthetic frames (write_fits_f32 with channels = 3, planes = R, G, B levels 0.1/0.2/0.3);
        // three frames with identity maps; write_rejection_maps = true → data.len() == 3·w·h, plane p ≈ its level,
        // rejection_low/high Some with len 3·w·h, on_plane called with (0,3),(1,3),(2,3).
    }
```

Write the three sketched tests in full (they are the plan's acceptance for this task; the sketch tells you what to assert, the numbers come from your fixture — record any bound you had to pick in the test's comment).

- [ ] **Step 2: Implement.** Key points of `integrate_group`:
  1. Validate: `frames.len() ≥ 3`; `reference < frames.len()`; every `measurement.channels.len() == channels`; `normalization.rejection != Local` (else `BadInput("local normalization is M2; use scaleZeroOffset or equalizeFluxes")`).
  2. **Min-weight drop**: a frame whose `weight.normalized.iter().cloned().fold(f64::INFINITY, f64::min) < integration.min_weight` is dropped (`warn!(path, weight, min_weight, "frame dropped below the minimum weight")`); the reference is never dropped (warn if it would have been); `included` = the surviving indices in input order; `< 3` → `BadInput("fewer than 3 frames after the weight floor")`.
  3. Recipe: `IntegrationRecipe { combination, rejection: integration.rejection.resolve(included.len()) }`; `info!(frames = included.len(), recipe = %recipe.describe(), "group integration started")`.
  4. Per plane `p` in `0..channels`: `(progress.on_plane)(p, channels)`; pairs for each included frame `i`: `rejection_pair(ref_ls, frame_ls, normalization.rejection).unwrap()` and `output_pair(ref_ls, frame_ls, normalization.output)` where `ref_ls = frames[reference].measurement.channels[p].location_scale()`; weights `frame.weight.normalized[p] as f32` (missing channel → 0.0); `RegisteredSource::open(&registered_frames, width, height, p, interpolation, clamping)` with `RegisteredFrame { path, map }`; `integrate_stack` with `StackParams { rejection, output, weights, range_low: integration.range_low.map(|v| v as f32), range_high: …, rejection_maps: integration.write_rejection_maps }`; append `out.base.data` to `data`, maps to their vectors; accumulate `rejected_low/high`, `samples`, per-frame rejected, read/combine durations, bytes.
  5. Master measurement per plane: `measure_plane(&plane_data, width, height, measure, Some(pool))` → `noise` (already native? `ChannelMeasurement.noise` — read Plan 2's `measure_plane` to see whether `noise` is reported in native units or ADU; convert to native `[0, 1]` units for `master_noise` and state it in the doc comment), `location`, `scale`, `psf_snr`, `fwhm_px`, `eccentricity`; best sub = the included frame with the highest `weight.normalized_mean`: its `measurement.channels[p].noise`/`psf_snr`; `snr_gain = master_psf_snr / best_sub_psf_snr` (`NaN` → 0.0 with a warn).
  6. Exposure: `total_exposure_s = Σ exposure`; `weighted_exposure_s = Σ exposure_i · w̄_i` with `w̄_i` = mean of `weight.normalized` over planes.
  7. Cancel: check `cancel` before each plane (the engine checks inside); `debug!(plane, rejected_low, rejected_high, duration_ms, "plane integrated")`; `info!(frames, planes, rejected_fraction, duration_ms, "group integration finished")`.
  8. Logging dictionary: reuse `frames` (usize), `planes` (usize — new: "planes the group integrated"), `plane` (usize — index), `rejected_low` / `rejected_high` (u64 — rejected samples per side), `rejected_fraction` (f64), `recipe`, `weight`, `min_weight` (f64 — the floor), `duration_ms`, `path`. Add the new names to the logging spec's dictionary in a "stacking integration" paragraph.

- [ ] **Step 3: Gates** — `cargo test -p athenaeum-core stacking::integrate`, `stacking::` whole, headless (the module is under the gated `stacking`), workspace check, rustfmt on the new file only.

- [ ] **Step 4: Commit** — `feat(stacking): integration and normalization config, the Auto rejection rule, and the per-group weighted integration driver`.

---

### Task 5: `fits_writer/wcs.rs` — WCS cards from a stored plate solve

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/wcs.rs`
- Modify (hand edit): `crates/athenaeum-core/src/fits_writer/mod.rs` — `pub mod wcs;`

**Interfaces:**
- Consumes: `plate_solve::storage::PlateSolveRecord` (crpix 0-based, crval deg, CD matrix deg/px, `sip_order`, `sip_{a,b,ap,bp}_coeffs` JSON `Vec<Vec<f64>>` with `coeffs[i][j]` for `u^i v^j`), `fits_writer::{Card, CardValue, FitsWriteError}`.
- Produces: `pub fn wcs_cards(solve: &PlateSolveRecord) -> Result<Vec<Card>, FitsWriteError>`.

- [ ] **Step 1: Failing tests:**

```rust
    fn record(sip: bool) -> PlateSolveRecord {
        PlateSolveRecord {
            id: None, frame_id: 1,
            crpix1: 3111.5, crpix2: 2083.5,           // 0-based
            crval1: 316.25, crval2: 70.45,
            cd1_1: -2.5e-4, cd1_2: 1.0e-6, cd2_1: -1.0e-6, cd2_2: -2.5e-4,
            sip_order: sip.then_some(2),
            sip_a_coeffs: sip.then(|| "[[0.0,0.0,1.0e-7],[0.0,2.0e-7],[3.0e-7]]".to_string()),
            sip_b_coeffs: sip.then(|| "[[0.0,0.0,-1.0e-7],[0.0,-2.0e-7],[-3.0e-7]]".to_string()),
            sip_ap_coeffs: None, sip_bp_coeffs: None,
            matched_stars: 100, total_detected: 200, rms_residual_px: 0.3, rms_residual_arcsec: 0.27,
            pixel_scale_arcsec: 0.9, field_rotation_deg: 0.0, solve_time_ms: 10,
            catalog_used: "test".into(), algorithm_used: "test".into(), solved_at: "2026-09-09T00:00:00Z".into(),
            expected_catalog_stars_in_fov: None, inlier_ratio: None,
        }
    }

    fn value<'a>(cards: &'a [Card], kw: &str) -> &'a CardValue {
        cards.iter().find(|c| c.keyword == kw).unwrap_or_else(|| panic!("no {kw}")).value.as_ref().unwrap()
    }

    #[test]
    fn linear_wcs_cards_are_one_based_and_tan() {
        let cards = wcs_cards(&record(false)).unwrap();
        assert_eq!(value(&cards, "CTYPE1"), &CardValue::Str("RA---TAN".into()));
        assert_eq!(value(&cards, "CTYPE2"), &CardValue::Str("DEC--TAN".into()));
        assert_eq!(value(&cards, "CRPIX1"), &CardValue::Real(3112.5));
        assert_eq!(value(&cards, "CRPIX2"), &CardValue::Real(2084.5));
        assert_eq!(value(&cards, "CRVAL1"), &CardValue::Real(316.25));
        assert_eq!(value(&cards, "CD1_1"), &CardValue::Real(-2.5e-4));
        assert_eq!(value(&cards, "CD2_2"), &CardValue::Real(-2.5e-4));
        assert_eq!(value(&cards, "CUNIT1"), &CardValue::Str("deg".into()));
        assert_eq!(value(&cards, "RADESYS"), &CardValue::Str("ICRS".into()));
        assert_eq!(value(&cards, "EQUINOX"), &CardValue::Real(2000.0));
        assert_eq!(value(&cards, "WCSAXES"), &CardValue::Integer(2));
        assert!(cards.iter().all(|c| !c.keyword.starts_with("A_") && c.keyword != "A_ORDER"));
    }

    #[test]
    fn sip_cards_follow_the_stored_triangular_table() {
        let cards = wcs_cards(&record(true)).unwrap();
        assert_eq!(value(&cards, "CTYPE1"), &CardValue::Str("RA---TAN-SIP".into()));
        assert_eq!(value(&cards, "A_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "B_ORDER"), &CardValue::Integer(2));
        assert_eq!(value(&cards, "A_0_2"), &CardValue::Real(1.0e-7));
        assert_eq!(value(&cards, "A_1_1"), &CardValue::Real(2.0e-7));
        assert_eq!(value(&cards, "A_2_0"), &CardValue::Real(3.0e-7));
        assert_eq!(value(&cards, "B_2_0"), &CardValue::Real(-3.0e-7));
        // zero coefficients are not written; no AP/BP without a reverse solution
        assert!(cards.iter().all(|c| c.keyword != "A_0_0" && c.keyword != "A_0_1"));
        assert!(cards.iter().all(|c| !c.keyword.starts_with("AP_") && !c.keyword.starts_with("BP_")));
    }

    #[test]
    fn malformed_sip_json_is_an_error_not_a_silent_linear_header() {
        let mut r = record(true);
        r.sip_a_coeffs = Some("not json".into());
        assert!(wcs_cards(&r).is_err());
    }

    #[test]
    fn cards_round_trip_through_the_header_reader() {
        // write a tiny FITS with these cards via write_fits_f32, read back with FitsHeader::from_path,
        // assert get_f64("CRPIX1") == Some(3112.5) and get_str("CTYPE1") == Some("RA---TAN-SIP"), get_f64("A_2_0") ≈ 3e-7.
    }
```

- [ ] **Step 2: Implement** — cards in this order: `WCSAXES` 2, `CTYPE1`/`CTYPE2` (`RA---TAN`/`DEC--TAN`, `-SIP` suffix when any SIP table is present), `CRPIX1`/`CRPIX2` (`crpix + 1.0`, comment "1-based reference pixel"), `CRVAL1`/`CRVAL2` (comment "deg"), `CD1_1`…`CD2_2`, `CUNIT1`/`CUNIT2` `deg`, `RADESYS` `ICRS`, `EQUINOX` 2000.0, then `A_ORDER`/`B_ORDER` + non-zero `A_i_j`/`B_i_j` (`i + j ≤ order`, `i + j ≥ 2` — the linear terms belong to the CD matrix; if the table carries non-zero `i + j < 2` entries, return `FitsWriteError::InvalidValue`-style error naming the term — pick the existing error variant that fits a bad value), then `AP_ORDER`/`BP_ORDER` + `AP_i_j`/`BP_i_j` when both reverse tables are present. Parse the JSON with `serde_json::from_str::<Vec<Vec<f64>>>`; an error maps to the writer's error type (`FitsWriteError` has variants for invalid keyword/value — map to the value one with the message). Add a `PLTSOLVD` logical `true` card with comment "plate solved by Athenaeum".

- [ ] **Step 3: Gates** — `cargo test -p athenaeum-core fits_writer::wcs`, headless check (ungated module), rustfmt on the new file.

- [ ] **Step 4: Commit** — `feat(fits_writer): WCS and SIP cards from a stored plate solve`.

---

### Task 6: `stacking/master_cards.rs` — master-light header, file naming, writers; spec corrections

**Files:**
- Create: `crates/athenaeum-core/src/stacking/master_cards.rs`
- Modify (hand edit): `crates/athenaeum-core/src/stacking/mod.rs` — `pub mod master_cards;`
- Modify: `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §6.4 (two corrections, rulings 7 and 8)

**Interfaces:**
- Consumes: `fits_writer::{Card, CardValue, FitsWriteError, keywords::{HeaderBuilder, FrameKind}, write_fits_f32, wcs::wcs_cards}`, `stacking::register::writer::{REGISTERED_COPY_THROUGH, source_cards_from_file}`, `plate_solve::storage::PlateSolveRecord`, `archive::path_layout::sanitize_for_filename`, `calibration_library::paths::resolve_collision`, `stacking::integrate::{GroupOutput, GroupStats}`.
- Produces:

```rust
pub const ATH_STK_VERSION: i64 = 1;

pub struct MasterCardInputs<'a> {
    /// Copy-through cards of the REFERENCE frame (`source_cards_from_file`).
    pub reference_cards: &'a [Card],
    /// The reference's plate solve, when it has one.
    pub wcs: Option<&'a PlateSolveRecord>,
    pub frames: usize,
    pub weighted_exposure_s: f64,
    /// Earliest and latest `DATE-OBS` among the included frames (ISO text as stored).
    pub date_obs_first: Option<&'a str>,
    pub date_obs_last: Option<&'a str>,
    pub recipe: &'a str,            // IntegrationRecipe::describe()
    pub weight_mode: &'a str,       // the WeightMode serde name
    pub normalization: &'a str,     // "<output>/<rejection>" serde names, e.g. "additiveWithScaling/scaleZeroOffset"
    pub reference_id: &'a str,      // ATH_STKF — the reference frame's identity (Plan 5 decides the string)
    pub group_key: &'a str,         // ATH_STKG
    pub run_id: &'a str,            // ATH_STKI
    pub app_version: &'a str,
}

pub fn build_master_light_cards(inputs: &MasterCardInputs<'_>) -> Result<Vec<Card>, FitsWriteError>;

/// §9.5: `<set slug>_<filter>_<instrume>_<n>x<exp>s.fits` when every exposure is within ±0.5 s of the first, else `<set slug>_<filter>_<instrume>_<n>f_<total>s.fits`; every part sanitized; `exp`/`total` printed as integers when whole, else one decimal.
pub fn master_file_name(set_name: &str, filter: Option<&str>, instrume: Option<&str>, exposures_s: &[f64]) -> String;

/// Writes the master (planar `channels × w × h`) and, when present, the two maps as `<stem>_rejlow.fits` / `<stem>_rejhigh.fits`; returns the paths written. Never overwrites: `resolve_collision` on the master path, the map names derived from the resolved stem.
pub fn write_master_light(dir: &Path, file_name: &str, output: &GroupOutput, cards: &[Card]) -> anyhow::Result<WrittenMaster>;

pub struct WrittenMaster { pub master: PathBuf, pub rejection_low: Option<PathBuf>, pub rejection_high: Option<PathBuf> }
```

- [ ] **Step 1: Failing tests:**

```rust
    #[test]
    fn master_name_follows_the_layout_rules() {
        assert_eq!(master_file_name("LDN 1272", Some("NoFilter"), Some("atr2600m"), &[180.0; 208]), "LDN_1272_NoFilter_atr2600m_208x180s.fits");
        assert_eq!(master_file_name("M 31", Some("Ha"), Some("ASI 2600MM"), &[300.0, 300.4, 299.6]), "M_31_Ha_ASI_2600MM_3x300s.fits");
        assert_eq!(master_file_name("M 31", None, None, &[120.0, 180.0, 300.0]), "M_31_NoFilter_unknown_3f_600s.fits");
        assert_eq!(master_file_name("a/b:c", Some("L"), Some("cam"), &[0.5, 0.5]), "a_b_c_L_cam_2x0.5s.fits");
    }

    #[test]
    fn master_cards_carry_copy_through_wcs_and_provenance() {
        let reference_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("LDN 1272".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2025-10-18T02:02:02".into())).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(), // must not survive
        ];
        let solve = /* the wcs test's record(true) shape, build inline */;
        let cards = build_master_light_cards(&MasterCardInputs {
            reference_cards: &reference_cards, wcs: Some(&solve), frames: 208, weighted_exposure_s: 36000.0,
            date_obs_first: Some("2025-09-14T00:56:00"), date_obs_last: Some("2025-10-19T05:25:52"),
            recipe: "Average | Linear fit clip (5.0/3.5)", weight_mode: "psfSignalWeight",
            normalization: "additiveWithScaling/scaleZeroOffset", reference_id: "frame:73", group_key: "atr2600m__mono__NoFilter__bin1__6224x4168", run_id: "run-7", app_version: "0.5.7",
        }).unwrap();
        let kw = |k: &str| cards.iter().find(|c| c.keyword == k).map(|c| c.value.clone().unwrap());
        assert_eq!(kw("IMAGETYP"), Some(CardValue::Str("Master Light".into())));
        assert_eq!(kw("NCOMBINE"), Some(CardValue::Integer(208)));
        assert_eq!(kw("EXPTIME"), Some(CardValue::Real(36000.0)));
        assert_eq!(kw("DATE-OBS"), Some(CardValue::Str("2025-09-14T00:56:00".into())));
        assert_eq!(kw("DATE-END"), Some(CardValue::Str("2025-10-19T05:25:52".into())));
        assert_eq!(kw("ROWORDER"), Some(CardValue::Str("TOP-DOWN".into())));
        assert_eq!(kw("OBJECT"), Some(CardValue::Str("LDN 1272".into())));
        assert_eq!(kw("INSTRUME"), Some(CardValue::Str("cam".into())));
        assert_eq!(kw("BAYERPAT"), None);
        assert_eq!(kw("CTYPE1"), Some(CardValue::Str("RA---TAN-SIP".into())));
        assert_eq!(kw("ATH_STK"), Some(CardValue::Logical(true)));
        assert_eq!(kw("ATH_STKV"), Some(CardValue::Integer(1)));
        assert_eq!(kw("ATH_STKN"), Some(CardValue::Integer(208)));
        assert_eq!(kw("ATH_STKR"), Some(CardValue::Str("Average | Linear fit clip (5.0/3.5)".into())));
        assert_eq!(kw("ATH_STKW"), Some(CardValue::Str("psfSignalWeight".into())));
        assert_eq!(kw("ATH_STKO"), Some(CardValue::Str("additiveWithScaling/scaleZeroOffset".into())));
        assert_eq!(kw("ATH_STKF"), Some(CardValue::Str("frame:73".into())));
        assert_eq!(kw("ATH_STKG"), Some(CardValue::Str("atr2600m__mono__NoFilter__bin1__6224x4168".into())));
        assert_eq!(kw("ATH_STKI"), Some(CardValue::Str("run-7".into())));
        assert!(kw("SWCREATE").is_some());
        // exactly one of each — the reference's EXPTIME/DATE-OBS were replaced, not duplicated
        assert_eq!(cards.iter().filter(|c| c.keyword == "EXPTIME").count(), 1);
        assert_eq!(cards.iter().filter(|c| c.keyword == "DATE-OBS").count(), 1);
    }

    #[test]
    fn writer_lands_master_and_maps_without_overwriting() {
        // GroupOutput 3 planes 8×6 with rejection maps → dir has master + _rejlow + _rejhigh; write again → _2 names; headers: master IMAGETYP "Master Light", NAXIS3 3; map files carry IMAGETYP "Rejection Map Low"/"Rejection Map High" and ATH_STK true
    }
```

- [ ] **Step 2: Implement.** `build_master_light_cards`: start with `HeaderBuilder::new(FrameKind::MasterLight).swcreate(app_version).build()?`, then append the reference's cards filtered by `REGISTERED_COPY_THROUGH` **minus** `EXPTIME` and `DATE-OBS` (replaced below), then `NCOMBINE` (Integer), `EXPTIME` (weighted total, comment "weighted total exposure, s"), `DATE-OBS`/`DATE-END` when given, then `wcs_cards(solve)?` when `Some`, then the `ATH_STK*` provenance cards with comments (`ATH_STK` "stacked by Athenaeum; never cataloged", `ATH_STKV` "stacking header version", `ATH_STKN` "frames combined", `ATH_STKR` "combination | rejection", `ATH_STKW` "weight mode", `ATH_STKO` "output/rejection normalization", `ATH_STKF` "reference frame", `ATH_STKG` "group key", `ATH_STKI` "stacking run"). `master_file_name`: `sanitize_for_filename` on each part; `filter.unwrap_or("NoFilter")`, `instrume.unwrap_or("unknown")`; equal exposures = every `|e − e0| ≤ 0.5`; number formatting: `if x == x.trunc() { format!("{x:.0}") } else { format!("{x:.1}") }`. `write_master_light`: `resolve_collision(&dir.join(file_name))` for the master; maps: `<stem>_rejlow.fits`/`_rejhigh.fits` also through `resolve_collision`; map cards: `IMAGETYP` `Rejection Map Low`/`High` (plain `Card`, not `FrameKind`), `ATH_STK` true, `ATH_STKV`, `BUNIT` `count`, and the master's `ATH_STKI`/`ATH_STKG` cards copied; `write_fits_f32(path, w, h, channels, data, cards)`.

- [ ] **Step 3: Spec corrections** in §6.4 (`docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`): `IMAGETYP = 'MASTER LIGHT'` → `IMAGETYP = 'Master Light'` (the writer's `FrameKind::MasterLight`, one spelling for every master the app writes); `ATH_STKID` → `ATH_STKI` with the parenthetical "(run id; FITS keywords are eight characters)".

- [ ] **Step 4: Gates** — `cargo test -p athenaeum-core stacking::master_cards`, headless, workspace, rustfmt on the new file.

- [ ] **Step 5: Commit** — `feat(stacking): master-light header, file naming and writers`.

---

### Task 7: `examples/integrate_probe.rs`, `MRS_LAYER1_GAIN` rename, docs

**Files:**
- Create: `crates/athenaeum-core/examples/integrate_probe.rs`
- Modify: `crates/athenaeum-core/Cargo.toml` — `[[example]] name = "integrate_probe"`, `required-features = ["render", "solver"]` (after `register_probe`)
- Modify: `crates/athenaeum-core/src/stacking/psf_signal.rs` — rename `MRS_LAYER0_GAIN` → `MRS_LAYER1_GAIN` (doc: "the estimator reports layer-1 coefficients"), `background_residual` gains `assert!(data.len() == w * h)`… no — a `debug_assert!` is not enough for a public fn: return `None` with a `warn!` when `data.len() != w * h`.
- Modify: `CLAUDE.md` module map sentence (append `integrate`/`master_cards` and `fits_writer::wcs`, `examples/integrate_probe.rs`); logging spec dictionary (Task 4's names, if not already added there).

**The probe** (`cargo run --release -p athenaeum-core --example integrate_probe -- <reference.fits> <dir-with-c_*.fits> --out <dir> [--limit N] [--osc] [--distortion off|auto] [--rejection auto|linearFit|winsorized] [--maps] [--compare <external master .xisf>] [--json <path>]`):

1. Reference detection (`reference_stars`), then for every `c_*.fits` (mono) or `c_*_d.fits` (`--osc`) in the folder (sorted, `--limit`): `register_frame` (Plan 3) — skip failures with a warning; `measure_frame` (Plan 2, `MeasureOptions::default()`); collect `StackFrame` (exposure from the header `EXPTIME` via `FitsHeader`, `DATE-OBS` text).
2. `compute_weights(WeightMode::PsfSignalWeight)`; `integrate_group` with `IntegrationConfig { rejection: per flag, write_rejection_maps: --maps, ..default }`, `NormalizationConfig::default()`; progress printed per plane/band to stderr every ~2 s.
3. Write the master through `master_file_name` + `build_master_light_cards` (reference cards from the reference file; `wcs: None` in the probe — the probe has no catalog) + `write_master_light`.
4. JSON to stdout (or `--json`): frames, registered, failed, included, recipe, per-plane stats (`master_noise`, `location`, `scale`, `psf_snr`, `snr_gain`, `fwhm`, `eccentricity`), `rejected_low/high_fraction`, timings (register ms, measure ms, integrate read/combine ms, write ms, total), bytes read.
5. `--seeds fast|full` (default `fast`): which detector seeds the measurement fits — `fast` = `detect_fast_data` as `measure_plane` does today; `full` = `ImageAnalyzer::analyze_data` on the same ADU-scaled plane (`with_max_stars(opts.max_stars)`, `with_mrs_layers` as the analyzer's default) whose `stars` become the seeds (x, y, flux, and the analyzer's FWHM as the size hint). Implement it as a `SeedSource` argument on a new `measure_plane_with_seeds(data, w, h, opts, pool, seeds: SeedSource)` in `stacking/measure.rs` — `measure_plane` keeps its signature and calls it with `SeedSource::Fast`; the `Full` arm maps `AnalysisResult.stars` into the existing `Seed` shape. `--seeds-report <n>`: for the first `n` frames also run the other detector and print, per frame, both star counts and the fraction of each seed set within 0.5 px of the other (Checkpoint B step 4b consumes this).
6. `--compare`: read the external master with `astroimage::ImageConverter::read_raw` (its first image; assert the header's first `<Image` has `id="integration"` by scanning the first 1 MB of the file for `id="` — report the id), normalize by 65535 when the finite max exceeds 1.5 (Checkpoint A's finding on registered images; masters carry bounds 0:1 and are expected to be in range — report which); per plane: `noise_mrs` on both (native units), `location_scale` (median/BWMV) on both, the ratio ours/theirs, and a pixel-level comparison on the 1/16 stratified sample: median of `ours − theirs` and median of `|ours − theirs| / theirs` over pixels where `theirs > 2·noise`, plus star-level: `detect_stars` on both (top 300), match within 1.5 px, median flux ratio and median centroid delta. When `--maps` and the external file has `rejection_low`/`rejection_high` images, compare mean map values (ours ÷ n vs theirs).

Read `register_probe.rs` and `measure_probe.rs` first and reuse their argument parsing style, JSON shape and the `read_luminance_any` helper (copy it; examples do not share code).

- [ ] Step 1: write the example; Step 2: build in release; Step 3: smoke on `--limit 12` mono frames (≈ 10 s), report the JSON; Step 4: gates (`cargo build --release --example integrate_probe`, headless, workspace zero warnings, rustfmt on the example and `psf_signal.rs` if clean); Step 5: commit `chore(stacking): integrate_probe example, MRS gain rename, docs`.

---

### Task 8 (controller-run): Checkpoint B on LDN 1272

**Files:**
- Create: `docs/superpowers/research/2026-09-09-checkpoint-b-integration.md`

**Inputs:** `ATH="…/LDN1272-WBPP/LDN1272-ATH/LDN 1272"`, `OUT="…/LDN1272-Output"`; reference `$ATH/camera_atr2600m/lights/c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits`; external masters `$OUT/master/masterLight_BIN-1_6224x4168_EXPOSURE-180.00s_FILTER-NoFilter_mono_(1).xisf` (208 frames, PSF Signal Weight, linear fit 5.0/3.5, additive-with-scaling output normalization, **local** rejection normalization, range-low on; log: rejected 2.831 % = 0.841 % low + 1.989 % high; master noise 1.9570e-05, location 7.183186e-03, scale 2.062522e-04, PSF SNR 1.2458e+04, PSFSW 534.07, 58 350 PSF fits) and `…_RGB_(1).xisf` (160 frames, per channel rejected 2.508/2.790/2.639 %, noise 1.6716e-05/1.6631e-05/1.4975e-05, location 1.119e-03/2.706e-03/2.161e-03, PSF SNR 2318.6/4136.4/3232.1).

- [ ] **Step 1:** `cargo build --release -p athenaeum-core --example integrate_probe`.
- [ ] **Step 2: Mono group** — `integrate_probe "$REF" "$ATH/camera_atr2600m/lights" --out <scratch>/cpB/mono --rejection auto --maps --compare "<mono master (1)>" --json <scratch>/cpB/mono.json` (208 frames: expect ≈ 1 min registration + ≈ 3 min measurement + the integration pass at disk speed — note the wall time and bytes read).
- [ ] **Step 3: OSC group** — same with `--osc` on `camera_zwoasi2600mcduo/lights` and the RGB master, `--distortion off` and once with `--distortion auto` if time allows.
- [ ] **Step 4: Targets (spec §13):** master MRS noise within ±5 % of the external master per plane; rejected fraction 1–4 % (report ours vs 2.831 % / 2.5–2.8 % with the LN caveat, ruling 13); frames registered 208/208 and 160/160; artifacts: view the mono master at 400 % around the brightest trail the external maps show (`rejection_high` image of the external file has the trails — locate the top three connected high-rejection blobs from its map and check ours at those coordinates); wall time ≤ 8 min per group integrate stage; `snr_gain` reported. A target missed by a wide margin is a defect to fix in this plan (a fix round on the responsible task) before Plan 5; a near miss is recorded with its cause and a ruling.
- [ ] **Step 4b: Detector comparison (owner's rule, 2026-09-09).** The measurement stage seeds its PSF fits with the fast adaptive detector (`ImageAnalyzer::detect_fast_data`, the plate solver's falling-threshold ladder); the analysis page uses the two-pass calibrated-kernel detector (`analyze_data`: mesh background + MRS noise → pass 1 with a 3 px kernel → free-β Moffat PSF calibration → pass 2 with the refined kernel). On three mono subjects spanning the weight range (the best, the median and the worst PSFSW of the group) run both: the fast seeds as Plan 2 uses them (`max_stars 24576`) and `analyze_data` with the same cap; report star counts, the fraction of fast seeds within 0.5 px of a two-pass detection and vice versa, and PSFSW/PSF SNR recomputed by `measure_plane` from each seed set (the probe gains a `--seeds fast|full` switch for this). Then the rank correlation of our per-frame PSFSW against the external log's per-frame weights (`WBPPWGHT` values in the log's `II.images` block for the mono integration) with each seed set. **Ruling for Plan 5:** registration keeps the fast detector (Checkpoint A proved it); the measurement seeds switch to the two-pass detector only if its rank correlation is higher by more than 0.02 and the per-frame cost stays within the spec's 5-minute measurement budget for 368 frames — record the numbers and the decision in the note.

- [ ] **Step 5: Write the note** (tables: external baselines; our per-plane stats; ratios; pixel/star-level comparison; the detector comparison; timing; verdict per target; findings and rulings; carry-forwards for Plan 5 and M2) and commit: `docs(stacking): checkpoint B — weighted integration against the external masters`.

---

## Self-review (done while writing)

**Spec coverage.** §6.1 engine generalization: weights, offsets (pairs), channel loop, survivor masks (Tasks 3–4); the optional LN grid is M2 (refused with a named error). §6.2 combiner v2 with masks, rejection maps and per-frame rejected fraction (Tasks 1, 3, 4); per-frame rejection bitmaps are drizzle-only (M3). §6.3 rejection menu, linear-fit parity now, Winsorized parity M4, the Auto rule, range rejection, `minWeight` (Tasks 2, 4). §6.4 master output, copy-through, WCS writer, provenance cards, rejection-map files, naming (Tasks 5, 6); the scanner rule already exists (Plan 3); stats per group (Task 4). §5.1 global normalization from stage-3 statistics (Task 4). §13 unit items: weighted rejection with masks byte-identical (Task 1 pin + Task 3 pin), rejection maps (Task 3), WCS round trip (Task 5); real-data items: Checkpoint B (Task 8). §14 items 7 and 8 covered; item 9+ is Plan 5.

**Placeholder scan.** Task 3's sketch contains an explicitly rejected zip construction with the required replacement spelled out; Task 4's three sketched tests name their assertions and require the implementer to write them in full; no "TBD".

**Type consistency.** `StackParams`/`StackOutput`/`integrate_stack` (Task 3) are what Task 4 consumes; `GroupOutput`/`GroupStats` (Task 4) are what Task 6's writer and Task 7's probe consume; `wcs_cards` (Task 5) is what Task 6 calls; `RejectionChoice::resolve` returns the existing `Rejection`; `FrameKind::MasterLight` exists; `PlateSolveRecord` fields are as listed; `NormalizationPair`, `output_pair`, `rejection_pair`, `LocationScale`, `ChannelMeasurement::location_scale`, `FrameWeight.normalized`, `measure_plane`, `noise_mrs`, `RegisteredSource::open`, `write_fits_f32`, `resolve_collision`, `sanitize_for_filename`, `source_cards_from_file`, `REGISTERED_COPY_THROUGH` all exist with the signatures used above (verified 2026-09-09).
