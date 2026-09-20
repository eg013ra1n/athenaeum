# Stacking compute Tier A — acceptance run (2026-09-19/20)

**Plan:** `docs/superpowers/plans/2026-09-19-stacking-compute-tierA-plan.md` (Tasks 0–12 plus 3b
and 6a, rulings R-TA-1…R-TA-11). **Audit:** `docs/superpowers/research/2026-09-19-stacking-compute-audit.md`
(§7 carries the measured column this report feeds). **Branch:** `perf/stacking-tierA`
(`548eeec3` → the acceptance head `cecc988d` + the fix wave), rustafits `perf/stacking-kernels`
(`4760e86` → `81ad260`).

## 0. Verdict

**Bit-identical on every artifact; against a baseline build re-run on the same evening: 27.00 → 23.44 min (−3.6 min, −13 %; wall 1622 → 1412 s); against the cool-machine baseline of the morning 26.6 → 22.3 (g2, −16 %).**
Every checkpoint (t0, g1, g2, g3, tierA) compared byte-equal to the baseline on the four
masters (mono + OSC, plain + 2× drizzle), 565 registration rows, 197 `.athln` sidecars and
302 calibrated frames. Two-thirds of the wall gain came from one decision, not from kernels:
the registered frame is written once and read by Normalize and Integrate instead of being
re-warped in each (owner decision 2026-09-19, Task 6a). Of the audit's kernel rewrites, the
detection-side ones paid (Register detect −45 %, LN detect −30 %, Measure −12 %); the
combine-kernel ones measured as regressions on this toolchain and were reverted with their
numbers (Task 9, Task 10). The audit's admission gain for Measure/LN was withdrawn after the
peak was located inside `noise_mrs` (R-TA-3).

## 1. The measurement unit and method

Reduced set `LDN1272-test` (prod set 204): 92 mono (ATR2600M 6224×4168) + 105 OSC (ASI2600MC
6248×4176) 180 s lights, LN on (`rejection: local`), Bayer drizzle 2×, distortion off, Auto
two-pass reference, FITS output, `cleanup: keepAll`. Every run: a fresh copy of
`.athenaeum-acc/tierA-template` through `tier1/checkpoint.sh <name>` (Task 0), release
`athenaeum-web` (root profile `lto = "thin"`, `codegen-units = 4`), the app's 10-thread
`image_pool`, this 16 GB / 10-core Mac, Time Machine idle, no cargo alive.

**Thermal drift is real and large**: the same binary measured 25.7 → 29.5 s on the
combine phase across one build-heavy session (Task 9). Per-task numbers were therefore taken
as INTERLEAVED before/after brackets (B A B A B A, ruling R-TA-8), and the acceptance total is
read against a baseline build re-run back-to-back on the same evening (ruling R-TA-9), not
against the cool-machine baseline measured 15 h earlier.

## 2. Stage table (minutes, `stage finished` durations)

| Stage | baseline (cool, 09-19 08:28Z) | t0 | g1 | g2 | g3 | tierA (09-19 23:09Z) | baseline re-run (09-19 23:33Z) |
| ----- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| Calibrate | 2.38 | 2.41 | 2.15 | 2.20 | 2.28 | 2.43 | 2.27 |
| Measure | 4.99 | 4.97 | 4.35 | 4.32 | 4.51 | 4.48 | 4.56 |
| Register | 0.68 | 0.69 | 0.73 | 1.82 | 1.81 | 2.36 | 0.67 |
| Normalize | 8.07 | 7.78 | 7.41 | 5.76 | 5.96 | 5.70 | 8.64 |
| Integrate | 6.03 | 5.86 | 5.52 | 3.99 | 4.13 | 4.19 | 6.39 |
| Drizzle | 4.43 | 4.41 | 4.26 | 4.21 | 4.39 | 4.26 | 4.45 |
| **Total** | **26.59** | 26.12 | 24.43 | **22.30** | 23.08 | **23.44** | **27.00** |

The same-evening baseline re-run (548eeec3, its own throwaway build, 23:43Z, right after
the tierA run) reads 27.00 — 1.5 % above the morning's 26.59 on every untouched stage,
which is the machine's hot state, and the honest denominator: **−3.56 min (−13.2 %)**.
Per stage against it: Normalize 8.64 → 5.70 (−2.9), Integrate 6.39 → 4.19 (−2.2),
Measure 4.56 → 4.48, Drizzle 4.45 → 4.26, Register 0.67 → 2.36 (+1.7 — the registered
write; the audit's Task 6a estimate was +0.5–0.8; the lever, if wanted, is overlapping the
write with the next frame's alignment). Register grew because it now writes the registered frame (plane at a
time, `Durability::Volatile`, OSC admission 6, mono 10); Normalize and Integrate fell by
2.3 + 2.0 min for it. g3 and tierA sit above g2 uniformly — every untouched sub-stage
(debayer, `combine_cpu_ms`) moved with them — the hot-machine band, not code.

## 3. Per-frame medians (ms) — baseline → tierA

| Event | field | baseline | tierA | Δ |
| ----- | ----- | ---- | ---- | -- |
| light calibrated | debayer | 2598 | 1812 | (untouched — the band) |
| frame plane measured | background | 481 | 415 | −14 % (T2 dead noise map) |
| frame plane measured | noise | 536 | 424 | −21 % (T3b ping-pong + band) |
| frame plane measured | detect | 200 | 165 | −18 % (T1, T4) |
| frame plane measured | fit | 359 | 367 | 0 (T5 −1…−7 % on the probe, inside noise here) |
| frame stars detected | detect | 1412 | 730 | **−48 %** (T4 hfd_at scratch / medians / histograms) |
| ln frame normalized | warp | 3450 | 192 | **−94 %** (T6a materialized frame) |
| ln frame normalized | background | 596 | 391 | −34 % (T12 parallel cells) |
| ln frame normalized | ln_detect | (4838 at t0) | 3969 | −18 % (T4 via the same detector) |
| ln frame normalized | scale | 6615 | 5277 | −20 % |
| plane integrated | read | 28 300 | 7 340 | **−74 %** (T6a + T7 lane) |
| plane integrated | combine | 62 700 | 57 486 | −8 % (band; T9/T10 neutral by design) |
| plane integrated | combine_cpu | (618 s at t0) | 557 s | −10 % (band) |
| drizzle plane deposited | deposit | 44 100 | 41 202 | −7 % (T11 neutral on a near-axis set) |

`rejection_iters_mean = 2.31` and `medfit_evals_mean = 33.94` on every run — the algorithm's
shape never moved. Admission lines: calibrate 4/4, measure 5/5, register 6/6/10 (OSC/OSC/mono),
normalize 5/5 — only Register's changed (T6a fix round 2: keyed on the measured plane count).

## 4. What each task delivered (probe-level, interleaved where marked ⟳)

| Task | Item | Measured | Landed |
| ---- | ---- | -------- | ------ |
| 0 | harness (`ATH_ACC_DB`, set-id, `paths` strip, `ATH_ACC_EXTRA_PATHS`, `checkpoint.sh`) + `ScaleTimings`, `combine_cpu_ms`, I/E counters | t0 identical, 26.12 | yes |
| 1 | bg/noise pair passed into the detector (rustafits) | detect −15…−18 ms/plane | yes |
| 2 | dead noise map skipped; single-channel `Cow` | background −40 ms/plane; RSS flat | yes |
| 3 | Register/LN copy chain; admission re-measure | detect −4 %; peak = `noise_mrs` (7.0 planes) → constants stay 8 | yes (R-TA-3) |
| 3b | `noise_mrs` ping-pong buffers | peak unchanged; bit-mask reverted (+5 % CPU for no admission) | partly |
| 4 | `hfd_at` scratch, static r² table, selection medians, parallel histograms | pipeline: Register detect −45 %, LN detect −30 % (the probe's +21 % was a probe-build artefact) | yes |
| 5 | incremental `sampling_radius` ring, per-worker fit scratch | fit −1…−7 % per plane | yes |
| 6a | registered frame written once (plane at a time, Volatile), read by Normalize + Integrate; plan-gate staleness; fallback | LN warp −96 %, Integrate read −75 %; Register +1.1 min; 41 GB | yes |
| 6 | warp interior fast path, all seven kernels pinned | warp 2.0× mono / 1.44× OSC wall ⟳ | yes |
| 7 | f32 band lane (F32Be file / F32Le scratch) | read −8.5 % real, sweep ≈ 3× micro | yes |
| 8 | `Linear::apply` without the division for affine maps (finite-guarded) | write 0.153 → 0.119 s ⟳ | yes |
| 9 | medfit/rofunc/sort rewrites | I1 1.4–1.6× SLOWER, I2 1.05–1.09× slower, I4 1.36× slower → reverted; I5 neutral | I5 only |
| 10 | rayon leaf size; `band_bits` reuse | `with_max_len(4)` +0.8 % → reverted; reuse landed | band_bits only |
| 11 | drizzle band skip (sampled-max bound + 1 px margin); mosaic cache | −5.8 % on a 2.45°-tilted sample, +0.7 % axis-aligned ⟳; Z5 refused (R-M3-7) | Z1 only |
| 12 | LN background parallel, allocation-free | background −45 % (4-thread probe) ⟳ | yes |

## 5. Rulings that changed the plan

- **R-TA-3** — Measure/LN residency constants stay 8: the peak is intrinsic to `noise_mrs`
  (input + w1 + two smoothing planes + mask); the "admission 4 → 6–8" expectation withdrawn.
- **R-TA-8** — interleaved brackets only; **R-TA-9** — the acceptance denominator is a
  same-evening baseline re-run; **R-TA-10** — the drizzle skip bound is a sampled max, not the
  origin drop (a homography's Jacobian is position-dependent); **R-TA-11** — the fix wave.
- The owner's 2026-09-19 decision ("disk as storage so nothing is computed twice") added
  Task 6a, the largest single gain of the tier.

## 6. What the tier established beyond numbers

- Micro-rewrites of the combine kernel do not pay on this toolchain (a cached product loses to
  a register multiply; the std stable sort beats pdqsort at n ≈ 200); the doc comments in
  `combine.rs`/`engine.rs` carry the numbers so nobody re-proposes them.
- Probe timings under thin-LTO swing by double digits with unrelated edits; the product-build
  checkpoint is the arbiter, and a `RUST_LOG=astroimage=debug` stage split beats `sample` for
  attribution (Task 4).
- Each checkpoint now needs ≈ 135 GB free (the registered artifacts count in the footprint);
  `tier1-run.sh` fails loudly on a plan blocker instead of waiting (`a8506f87`).
- The path to a radical gain is Tier C (LN from Measure's fits, the drizzle phase table) —
  spec `docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md`, plan
  `docs/superpowers/plans/2026-09-20-stacking-compute-tierC-plan.md`, opened on the owner's
  2026-09-20 word without an approval gate.

## 7. Owed to the owner

- The desktop click-through of the Register panel note (the "Write registered frames" toggle
  became a note, `dc9f6287`) and of the plan gate's `space` message on a small volume.
- Whether the third-party product names that pre-date this tier in the rustafits submodule's
  comments/docs (its own public repo) fall under the 2026-09-19 rule.
- The push of `main` and both submodule branches — nothing is pushed.
