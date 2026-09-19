# Stacking Compute — Tier A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take the bit-identical compute wins of the compute audit (`docs/superpowers/research/2026-09-19-stacking-compute-audit.md` §7 Tier A) — no output byte moves — and measure each one on the owner's reduced set, so that every task ends with a number, not an estimate.

**Architecture:** Five groups, one per kernel family (measurement harness → detection/fit → warp → integrate combine → drizzle + LN background). Each task is a local change to one kernel guarded by the existing M1–M4d pins plus one new "same value as before" pin, and ends with a PROBE measurement (seconds, minutes at most). Each group ends with a CHECKPOINT: one full stacking run of the reduced set through the acceptance harness, compared byte for byte against the group's baseline outputs and timed stage by stage. rustafits changes live on a submodule branch and bump the gitlink in the same superproject commit.

**Tech Stack:** Rust 2021, rayon, `tracing`; the acceptance harness (`docs/superpowers/research/scripts/acceptance/`, plus `tier1/` — driver, extractor, comparer); `sample` (macOS) for profiles; the release probes `measure_probe`, `ln_probe`, `integrate_probe`, `register_probe`.

**Spec:** `docs/superpowers/research/2026-09-19-stacking-compute-audit.md` (§3 per-kernel findings with file:line, §7 tiers). The audit's item ids (D3, W1, I4, …) are used below.

## Global Constraints

- **Output bytes unchanged — Tier A is bit-identical by definition.** Every FITS master, `registration_results.transform_json`, `.athln`, calibrated frame and `metrics` payload must be byte-equal to the pre-task output. An item that turns out to move a bit (a re-associated float sum that is NOT provably exact, a division replaced by a reciprocal multiply, a kernel weight table) is NOT Tier A: stop, report, and the controller moves it to Tier B. The exact-arithmetic exceptions used below are stated with their proof.
- **The reduced set is the measurement unit**: PROD catalog frame set 204 `LDN1272-test` (92 mono ATR2600M 6224×4168 + 105 OSC ASI2600MC 6248×4176 lights), config = LN on (`rejection: local`), Bayer drizzle 2×, distortion off, Auto two-pass reference, FITS output, `cleanup: keepAll`. The template copy is `/Volumes/BigMac/Users/astrobureau/.athenaeum-acc/tierA-template/athenaeum.db`; every checkpoint runs on a FRESH copy of it (never the real catalog). The full LDN 1272 set (109) is NOT run in this plan.
- **Zero-print rule**; `tracing` only; new log fields go into `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the same commit; never name the external stacking tool; no Tauri/Axum surface change.
- **rustafits is a git submodule** (`rustafits/`, detached at `4760e86`): create branch `perf/stacking-kernels` there on first touch; commit rustafits changes in the submodule; bump the gitlink in the superproject commit of the same task; `cargo test -p astroimage` (the crate name) must pass in the submodule. Never name the external tool there either (its own CLAUDE.md).
- **Gates**: `cargo test -p athenaeum-core --lib` (full, ≈ 80 s) before every commit that touches `integration/`, `stacking/` or `resample/`; `cargo check -p athenaeum-core --all-targets` after any public-signature change (the probes); `cargo check -p athenaeum-core --no-default-features` once per group.
- **Commit as the owner** (`eg013ra1n` / `vilen.sharifov@gmail.com`) with the `Co-Authored-By` / `Claude-Session` trailers; push only on the owner's word.
- **Machine-quiet measurements**: before every checkpoint, `tmutil status` must say `Running = 0` and no `cargo` may be alive; the acceptance folder stays excluded from Time Machine and Spotlight.

## File map

| File | Tasks |
| ---- | ----- |
| `docs/superpowers/research/scripts/acceptance/{prepare-catalog.sh, tier1/*}` | 0 (harness takes a DB path and a set id) |
| `crates/athenaeum-core/src/stacking/ln/{mod,scale}.rs` | 0 (LN sub-phase timings), 12 |
| `crates/athenaeum-core/src/integration/engine.rs` | 0 (`combine_cpu_ms`, I/E histograms), 9, 10 |
| `crates/athenaeum-core/src/integration/combine.rs` | 0, 9 |
| `rustafits/src/analysis/{mod,adaptive_detection,background}.rs` | 1, 2, 3, 4 |
| `crates/athenaeum-core/src/stacking/register/detect.rs`, `stacking/run.rs` | 3 |
| `crates/athenaeum-core/src/stacking/psf_signal.rs` | 5 |
| `crates/athenaeum-core/src/resample/{warp,kernels}.rs` | 6 |
| `crates/athenaeum-core/src/integration/{banded,registered_source}.rs` | 7 |
| `crates/athenaeum-core/src/geometry/linear.rs` | 8 |
| `crates/athenaeum-core/src/stacking/drizzle/mod.rs` | 11 |
| `crates/athenaeum-core/src/stacking/ln/background.rs` | 12 |

---

## Group 0 — the ruler

### Task 0: Measurement harness + the two missing timers

**Files:**
- Modify: `docs/superpowers/research/scripts/acceptance/prepare-catalog.sh` (an `ATH_ACC_DB` env override for the source DB; a third optional argument `set-id` that copies set 109's config onto it with FITS output — or leaves it when the set already has one)
- Modify: `docs/superpowers/research/scripts/acceptance/tier1/tier1-run.sh` (takes `SET_ID` as a fourth argument; default 109), `tier1-extract.py` (prints the LN sub-phases and `combine_cpu_ms` below), `tier1-compare.py` (unchanged)
- Create: `docs/superpowers/research/scripts/acceptance/tier1/checkpoint.sh` — `checkpoint.sh <name>`: fresh copy of the template into `.athenaeum-acc/tierA-<name>`, `server.sh` on a free port, `start_stacking {setId:204, rerunFrom:"calibrate"}`, wait for `stacking-complete`, extract, compare against `.athenaeum-acc/tierA-baseline`, print the stage table diff
- Modify: `crates/athenaeum-core/src/stacking/ln/scale.rs::relative_scale_against` (returns `ScaleTimings { detect_ms, refine_ms (= 0 until Task 3 splits it), fit_ms, match_ms }` inside `ScaleResult`), `crates/athenaeum-core/src/stacking/ln/mod.rs` (`LnFrameOutcome` gains the four; `run.rs`'s `"ln frame normalized"` debug logs `ln_detect_ms`, `ln_fit_ms`, `ln_match_ms`)
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (`BandStats.combine_cpu` accumulated from a per-leaf `Instant` delta into an `AtomicU64` inside `for_each_init`; `"plane integrated"` logs `combine_cpu_ms`), `combine.rs` (thread-local counters `LINEAR_FIT_ITERS`, `MEDFIT_EVALS` summed per plane and logged as `rejection_iters_mean`, `medfit_evals_mean` on the same event)
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary paragraph: `ln_detect_ms`, `ln_fit_ms`, `ln_match_ms`, `combine_cpu_ms`, `rejection_iters_mean`, `medfit_evals_mean`)
- Test: `ln/scale.rs` tests (the timings sum ≤ `scale_ms`), `engine.rs` tests (`combine_cpu_ms > 0` and `≤ combine_ms × threads`)

**Interfaces:**
- Produces: `pub struct ScaleTimings { pub detect_ms: u64, pub refine_ms: u64, pub fit_ms: u64, pub match_ms: u64 }` on `ScaleResult.timings`; `BandStats.combine_cpu: Duration`; `IntegrationOutput.combine_cpu_duration`; the six log fields.

- [ ] **Step 1: Harness first.** Make the three scripts take the set id and the DB path; run `checkpoint.sh baseline` on the CURRENT head (after the tier-1 merge) — this run IS the Tier A baseline: keep its `stackout/`, `stackwork/**/*.athln`, `registration_results`, `logs/` under `.athenaeum-acc/tierA-baseline`. Record its stage table in the ledger.
- [ ] **Step 2: Failing tests** for `ScaleTimings` and `combine_cpu` (compile errors).
- [ ] **Step 3: Implement** the timers (wrap `detect_seeds`, `fit_stars_with_beta`, the pairing + RCR block; the leaf timer in `process_row`'s caller; the two thread-local counters incremented in `reject_linear_fit`'s outer loop and in `rofunc`).
- [ ] **Step 4: Gate** `cargo test -p athenaeum-core --lib`; **commit** `stacking: LN sub-phase timings, combine CPU time and the medfit iteration/evaluation counts; the acceptance harness takes a catalog path and a set id`.
- [ ] **Step 5: Measure** — `checkpoint.sh t0` must be byte-identical to `baseline` (instrumentation only) and now prints, per LN frame, how `scale_ms` splits, and per plane, `combine_cpu_ms` and the I/E means. Put the numbers in the ledger — every later task's expected gain is checked against them.

---

## Group 1 — detection and fitting (rustafits + psf_signal)

### Task 1: `background_and_noise` computed once per detection (D3)

**Files:** `rustafits/src/analysis/mod.rs:1277` (already computes `(bg, noise)` for the result), `rustafits/src/analysis/adaptive_detection.rs:38-113,489` (`detect_stars_adaptive` gains `precomputed: Option<(f32, f32)>` and skips its own call when `Some`).
**Interfaces:** `pub fn detect_stars_adaptive(…, precomputed_bg_noise: Option<(f32, f32)>) -> …`; `run_fast_detection` passes `Some`.
- [ ] Failing test in `adaptive_detection.rs`: detection with `Some(background_and_noise(lum))` equals detection with `None` (same star list, `assert_eq!`).
- [ ] Implement; rustafits `cargo test`; bump gitlink; core gate; commit `rustafits: detect_stars_adaptive takes the caller's background/noise pair instead of recomputing it`.
- [ ] Measure: `measure_probe` on one OSC frame (`register_probe` for the refine path) before/after: expect `detect_ms` −50 ms per plane (the audit's number). Ledger it.

### Task 2: the dead noise map and the copy chain (D4, D5 — rustafits half)

**Files:** `rustafits/src/analysis/background.rs:27-172` (`estimate_background_mesh` gains `want_noise_map: bool`; the second `interpolate_grid_to_map` runs only when true; `BackgroundResult.noise_map` stays `Option`), `rustafits/src/analysis/mod.rs:1266` (`Cow::Borrowed` for `channels == 1` instead of `to_vec`), `crates/athenaeum-core/src/stacking/psf_signal.rs:687-693` (passes `false`), every other caller `true`.
- [ ] Failing tests: `background_map` identical with the flag false vs true; `run_fast_detection` on a single-channel slice gives the same result and (via a `#[cfg(test)]` allocation counter or by checking the `Cow` variant) borrows.
- [ ] Implement; rustafits tests; gitlink; core gate; commit.
- [ ] Measure: `measure_probe` `background_ms` −30–40 ms per plane; peak RSS via `/usr/bin/time -l` on `measure_probe` before/after (the audit expects the 742 MB peak to drop by ≥ 1 plane). Ledger it — **this number feeds Task 3's admission constant.**

### Task 3: the copy chain in Register/LN detection and the admission re-measure (D5 — core half)

**Files:** `crates/athenaeum-core/src/stacking/register/detect.rs:33-41` (`luminance` borrows the single plane instead of `to_vec`; the `fold(max)` at `:70-79` folded into the ADU-scale pass — same `max`, same `warn!`), `:81` (scale in place into one owned buffer), `crates/athenaeum-core/src/stacking/run.rs` (`MEASURE_PLANES_RESIDENT` / `LN_PLANES_RESIDENT` re-measured with `measure_probe` + `/usr/bin/time -l` exactly as Task 3 of the tier-1 plan did; the doc comments record the new peak and date).
- [ ] Failing test: `detect_stars` on a one-plane luminance returns the same `Vec<Star>` as before (pin against a stored fixture result); the max-fold warning still fires on a plane > `NATIVE_UNITS_MAX`.
- [ ] Implement; full core gate; commit.
- [ ] Measure: the new residency factor (expected 8 → 5–6 ⇒ admission on 16 GB 4 → 6–8 for Measure and LN); `register_probe` `detect_ms` before/after.

### Task 4: `hfd_at` on one window copy, constant `r²` table, selection medians (D7) + parallel histograms (D11)

**Files:** `rustafits/src/analysis/adaptive_detection.rs:222-228` (`median` → `select_nth_unstable_by(total_cmp)` with the same odd/even rule), `:236-380` (`hfd_at`: copy the `(2·rs+2·annulus_w+1)²` window once into a per-call scratch — border handled once by NaN-fill — a `const R2: [[f32; 35]; 35]` table, all five loops over the scratch), `:38-113` + `:147-179` (per-row partial `u32` histograms via `par_chunks`, integer reduce — identical counts).
- [ ] Failing pins: `hfd_at` returns the same `(hfd, centroid, …)` for a fixture set of candidates (record the current outputs in a test fixture first — that fixture IS the RED/GREEN oracle); `background_and_noise` and `star_levels` return identical values under the parallel histogram (they are integer counts).
- [ ] Implement; rustafits tests; gitlink; core gate; commit.
- [ ] Measure: `register_probe` `detect_ms` (expected −40–50 %), `measure_probe` `detect_ms`; `sample` the register probe and confirm `hfd_at`'s share dropped. Ledger.

### Task 5: `sampling_radius` incremental ring and `fit_one` scratch (D8)

**Files:** `crates/athenaeum-core/src/stacking/psf_signal.rs:207-272` (`region_median` keeps the running multiset; each growth step appends only the new ring's finite pixels; the median is `median_in_place` on the same multiset ⇒ identical), `:301-319` (per-worker `PixelSample`/`f32` scratch via `for_each_init`-style thread-local instead of two `Vec`s per seed).
- [ ] Failing pin: `sampling_radius` returns the same radius and the same median sequence as the rescanning version on a fixture star (record the sequence first).
- [ ] Implement; core gate; commit.
- [ ] Measure: `measure_probe` `fit_ms` (expected −5–8 %).

### CHECKPOINT 1
- [ ] `checkpoint.sh g1` on a quiet machine: byte-identical to `tierA-baseline`; stage table vs baseline; ledger the deltas for Measure / Register / Normalize and the admission lines. Expected: Measure −20–30 %, Register −30–40 %, LN detect half −30 %, admission 4 → 6–8.

---

## Group 2 — resampling

### Task 6a: the warp is computed ONCE — materialized registered frames read by Normalize and Integrate (owner decision 2026-09-19)

**Why:** the checkpoint t0 medians show the same warp computed twice per frame: Normalize warps the whole calibrated frame into the reference geometry (`ln frame normalized` `warp_ms` ≈ 3.4 s) and Integrate warps it again band by band through `RegisteredSource` (`plane integrated` `read_ms` ≈ 27 s per plane ≈ the same ~3 CPU-s per frame-plane). The registered frame (spec §3.7, `register/writer.rs`) is today an optional artifact "nothing downstream reads". The warp is a pure per-pixel function of the calibrated plane and the `PixelMap`, and an f32 plane round-trips a FITS float32 file losslessly, so reading the materialized frame instead of re-warping is **bit-identical** by construction. The owner accepted the disk cost ("if the disk can be used as storage so that nothing is computed twice — do it", 2026-09-19): ≈ the `calibrated/` footprint again (≈ 50 GB on the reduced set, ≈ 100 GB on the full LDN 1272 set).

**Files:** `crates/athenaeum-core/src/stacking/register/writer.rs` (the registered frame becomes the run's REQUIRED per-frame artifact when Normalize or Integrate will read it: `kind = "registered"` in `stacking_artifacts`, keyed by the registration config hash + the calibrated artifact's hash so a re-registration or a re-calibration invalidates it exactly like the LN sidecars; written by the same warp code the on-the-fly path uses — `warp_rows` over the full plane, NaN outside coverage — via tmp + atomic rename), `crates/athenaeum-core/src/stacking/run.rs` (stage 5 writes it inside the existing register fan-out, right after the `registration_results` row; stage 6 and stage 7 open it instead of constructing a warping `RegisteredSource`; a missing/unreadable registered artifact falls back to the on-the-fly warp with ONE `warn!` per frame, never a failure), `crates/athenaeum-core/src/stacking/ln/mod.rs` (`normalize_frame` takes the registered plane reader), `crates/athenaeum-core/src/stacking/integrate.rs` + `integration/banded.rs` (a `PlaneReader`-backed band source for registered frames — the same `BandPlanes` contract the warping source fulfils), `crates/athenaeum-core/src/stacking/plan.rs` (the byte-footprint estimate adds one calibrated-size term per frame; `INTERMEDIATE_ARTIFACT_KINDS` lists `registered` so `output.cleanup = deleteIntermediates` removes it), `stacking/paths.rs` (`registered/<group>/<stem>.fits` is already in the layout). Drizzle keeps reading the CALIBRATED frame through the forward map — it never needed the warp.
**Interfaces:** `RegisteredSource::open_materialized(path, map, geometry) -> RegisteredSource` (same `read_band` contract as the warping constructor); `stacking_artifacts.kind = "registered"`.
- [ ] Failing pins FIRST: (1) `warp_rows` over the full plane equals the concatenation of the per-band warps `RegisteredSource` produces today, bit-exact (`to_bits`), for a fixture plane under Similarity/Homography at BicubicBSpline and Lanczos3 — this is the identity the whole task rests on; (2) a fixture run with the materialized path yields masters bit-identical to the same run with the on-the-fly path (the M1–M4d run pins double as the oracle: they keep passing); (3) a deleted registered file mid-run falls back with the warning and the same master.
- [ ] Implement; full core gate; commit. Update the plan gate's space estimate and the stacking spec's §3.7 sentence ("nothing downstream reads them" is no longer true).
- [ ] Measure (checkpoint g2 carries it): Normalize `warp_ms` → the registered read (expected ≈ 3.4 s → < 0.3 s per frame), Integrate `read_ms` per plane (≈ 27 s → ≈ 5–8 s), stage 5 grows by one write per frame (≈ 0.4 s at SSD speed); total expected on the reduced set ≈ −3 min of 26. Byte-identical on every artifact, as every checkpoint.

### Task 6: `sample_at` specialised, interior fast path (W1)

**Files:** `crates/athenaeum-core/src/resample/warp.rs:34-124` (`sample_at` becomes `sample_at_interior::<K>` for a pixel whose 4×4 (or 8×8 for Lanczos4) footprint lies inside the plane — no clamps, no bounds checks, four hoisted row slices, const trip counts — and the existing generic path for the border; the choice made once per pixel by a range test on `(sx, sy)`), `kernels.rs:192-213` (`taps_for` returns weights into a caller-owned `[f32; 8]` — no 48-byte value return). **The weight formula, the renormalisation branch and the tap order are UNCHANGED** — this task hoists guards, it does not touch arithmetic.
- [ ] Failing pin: a full `warp_rows` of a fixture plane under Similarity/Affine/Homography maps at BicubicBSpline, Lanczos3, Lanczos4 and bilinear is bit-identical (`to_bits`) to the current implementation (record outputs first; keep the old function under `#[cfg(test)]` as the oracle).
- [ ] Implement; core gate (the M1–M4d pins are the second oracle); commit.
- [ ] Measure: `ln_probe` `warp_ms` and `integrate_probe` read phase before/after; expected 2.5–3.5× on the warp.

### Task 7: no f32 → bytes → f32 round trip for `F32Le` sources (W3)

**Files:** `crates/athenaeum-core/src/integration/banded.rs:791-841` (`BandPlanes` gains an `f32` fast lane for `PlaneKind::F32Le` — the buffer IS the f32 data; `sample`/`decode_row_into`/`decode_frame_into` read it directly), `registered_source.rs:260-265,410-412` (`store_f32_le` writes the f32 scratch straight into the lane).
- [ ] Failing pin: `BandPlanes::sample` on an `F32Le` band equals the current decode for random data incl. NaN/±Inf bit patterns (`to_bits`).
- [ ] Implement; core gate; commit.
- [ ] Measure: `integrate_probe` read phase and `ln_probe` `warp_ms`; expected −5–10 % of those phases and −104 MB per LN worker.

### Task 8: `Linear::apply` without the division when `w == 1` (W4)

**Files:** `crates/athenaeum-core/src/geometry/linear.rs:51-58`. **Exactness:** IEEE `x / 1.0 == x` for every finite and non-finite `x`; the skip is `if m[2] == [0.0, 0.0, 1.0]` computed ONCE (a `bool` on `Linear`, set by every constructor/normaliser), and the affine path then omits the `w` multiply-adds too — `w` was exactly `0·x + 0·y + 1 = 1`.
- [ ] Failing pin: `apply` equals the current implementation bit for bit over a grid for Similarity/Affine (w = 1) AND for a homography (division kept).
- [ ] Implement; core gate; commit.
- [ ] Measure: `integrate_probe` read phase (small); drizzle `deposit_ms` in the group-4 checkpoint.

### CHECKPOINT 2
- [ ] `checkpoint.sh g2`: byte-identical; stage deltas for Normalize (warp half) and Integrate (read half). Expected: LN `warp_ms` −60 %, Integrate read −50 %.

---

## Group 3 — the robust line

### Task 9: `rofunc` and the sorts, exact rewrites (I1–I5)

**Files:** `crates/athenaeum-core/src/integration/combine.rs:816-838` (`rofunc`: compute `t_i = b · i` once per element into the scratch and reuse it for the sign test — the SAME rounded product; four integer accumulators for the sign sum — addends are `±i ≤ 207`, partial sums ≤ 21 528, exact in f64; fuse the even-`m` `fold(max)` into the selection by taking `max(lo)` from the partition the selection already produced — the same element), `:901` (`work.sort_unstable_by(value.partial_cmp then frame index)` — the pre-sort order is ascending frame index at both push sites `engine.rs:960,1028`, so the stable sort's tie order equals the explicit tiebreak), `engine.rs:1146-1151` (the side-attribution median = `work[kept/2].0`; `reject_linear_fit`'s `(kept, true)` flag exposed instead of discarded at `combine.rs:339`), `engine.rs:845-857` (LN row buffers per THREAD via a thread-local, not per leaf).
- [ ] Failing pins: `medfit_line` returns bit-identical `(a, b)` for 1 000 random stacks incl. ties and NaN-free/NaN-carrying inputs vs the current implementation (keep the old under `#[cfg(test)]`); `reject_linear_fit` bit-identical survivors and counts; `integrate_stack` output bit-identical on the engine test fixtures (the existing pins).
- [ ] Implement; full core gate; commit.
- [ ] Measure: `integrate_probe --limit 60 --rejection linearFit` combine phase and the new `combine_cpu_ms`; `sample` before/after — `partition_at_index`'s share and the malloc share must drop. Expected −25–40 % of the combine.

### Task 10: rayon leaf size and the `band_bits` reuse (I6 tail, a9)

**Files:** `engine.rs:1224,1239` (`with_max_len(4)` on the row chunks), `:1142` (reuse one `band_bits` buffer across bands — `fill(0)` instead of `vec![0; …]`).
- [ ] Failing pin: none needed beyond the existing engine pins (scheduling only); add an assertion that `band_bits` is zero at the start of each band.
- [ ] Implement; core gate; commit.
- [ ] Measure: `integrate_probe` wall at 60 frames; expected 0–10 %.

### CHECKPOINT 3
- [ ] `checkpoint.sh g3`: byte-identical; Integrate stage delta. Expected: 10.7-equivalent → ≈ 7 on the reduced set's scale.

---

## Group 4 — drizzle and the LN background

### Task 11: drizzle band skip and one read per frame (Z1, Z5)

**Files:** `crates/athenaeum-core/src/stacking/drizzle/mod.rs:856-1016` (`deposit_band`: right after `ctx.fwd.at(x, y)` compute `oy = to_output(v)`; if `oy + half_diag < y0 || oy − half_diag > y1` skip — `half_diag` = half the mapped drop's diagonal, a per-frame constant computed ONCE from `map_drop` of the origin drop; a skipped pixel is one whose `py0..=py1` range would have been empty, so nothing changes), `:483-549` (Bayer path: read the mosaic once per frame and keep it for the three planes — the plane loop stays outer for the accumulators; the frame's mosaic is cached in a per-group `HashMap<frame, Arc<Vec<f32>>>` bounded to the group's frame count × 104 MB … **only if** the R-M3-7 memory estimate admits it; otherwise keep the re-read and report).
- [ ] Failing pin: `drizzle_group` output bit-identical on the M3 fixtures under a 1°, 5° and 30° rotation map (record outputs first); a unit test that the skip predicate never rejects a pixel whose clipped area is > 0 (exhaustive over one band of a rotated fixture).
- [ ] Implement; core gate; commit.
- [ ] Measure: `deposit_ms` per plane on the checkpoint run; expected −10–30 % depending on the set's rotations.

### Task 12: LN background — parallel, allocation-free, one median (L1, L2)

**Files:** `crates/athenaeum-core/src/stacking/ln/background.rs:99-158,177-300` (`clean_plane`'s two passes and the cell loop under `par_iter` on the caller's pool — cells are independent; per-worker scratch for `gather_cell`; `retain` instead of `filter().collect()`; `median_in_place`/`mad` on borrowed scratch), `ln/mod.rs:173-179` (`median_of_finite` uses `median_in_place` on the owned `finite` buffer — no second copy).
- [ ] Failing pins: `background_grid` returns bit-identical `cells`/`invalid_cells` on the LN fixtures (medians and MADs are order-independent selections; document that in the test); `median_of_finite` identical.
- [ ] Implement; core gate; commit.
- [ ] Measure: `ln_probe` `background_ms`; expected −50–70 %.

### CHECKPOINT 4 (= Tier A acceptance)
- [ ] `checkpoint.sh tierA`: byte-identical to `tierA-baseline` on every artifact; the full stage table baseline → Tier A; the per-frame medians baseline → Tier A; `sample` of `ln_probe` and `integrate_probe` after. Write `docs/superpowers/research/2026-09-XX-stacking-compute-tierA-acceptance.md` (same shape as the M-run reports) and update the compute audit's §7 with the measured column; update `CLAUDE.md`'s Stacking section (one paragraph: what Tier A changed, the new timing fields, the harness scripts). Commit. Expected on the reduced set: total −20–25 %.

---

## Self-review

**Spec coverage** (audit §7 Tier A list): D3 → T1; D4, D5 → T2, T3; D7, D11 → T4; D8 → T5; W1 → T6; W3 → T7; W4 → T8; I1–I6 → T9, T10; Z1, Z5 → T11; L1, L2 → T12; instrumentation (I11 + LN split) → T0. Z2 (constant parallelogram) was in the audit's Tier A list but its corner arithmetic `A·(x±h)` vs `A·x + A·(±h)` rounds differently — moved to Tier B; the audit's §3.4 table is corrected in T0's commit. The prefetch/pool gate on `StorageClass::Network` is in the tier-1 merge fix wave, not here.

**Exactness arguments stated**: T8 (division by exactly 1.0), T9 (integer sign sums; shared rounded product; selection returns the same order statistic; the stable-vs-unstable permutation), T11 (skip only empty deposits), T12 (order-independent selections), T4 (integer histograms; the median value of a set is sort-independent), T6/T7 (no arithmetic change). T5's incremental ring changes the ORDER in which the multiset is gathered — `median_in_place`'s selection is order-independent, so the value is identical; the pin proves it.

**Placeholders:** none — each task names its files, its pin and its measurement; the checkpoint script is defined in T0.

**Type consistency:** `ScaleTimings` (T0) is read by T3's measurement; `combine_cpu_ms` (T0) is T9/T10's ruler; `checkpoint.sh` (T0) is used by every checkpoint.
