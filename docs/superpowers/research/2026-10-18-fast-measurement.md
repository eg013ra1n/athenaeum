# Fast-measurement cycle: `fitStars`, the GPU path and per-core levers (2026-10-18)

Plan `docs/superpowers/plans/2026-10-18-fast-measurement-plan.md` (FM-1...FM-31); ruling R-OSP-14 in
`docs/stacking/README.md`. Branch `one-star-path-fm`, final code head **61e526d6** (the branch is kept; squashed onto `one-star-path` at the close). Lab results are in athenaeum-lab
`star-lab/results/plan-a/fast-measure/`; the controller ledger and the phase reports (`fm-*-report-r1.md`) are in the local
SDD folder `.superpowers/sdd/2026-10-18-fast-measurement/`. "ERP" = the reference product the owner measures against.
Every number is a measurement unless it says "projected". Follows `2026-10-18-task-13c-parallelism.md`, whose carry 1 this is.

## 1. The question and the bars

After Task 13c the dev Mac ran set 204 in 1 081 s against the ERP's 1 005 s. The gap was Measure (+405 s). The cycle asked
three things: can fewer fitted stars (`measurement.fitStars = K`, the K brightest by signal) shorten Measure without
hurting weights and star quality; can the GPU take the fit work; and what is left per core.
- **Owner bar (Q1, 2026-10-18).** The whole run (Masters to Output) faster than the ERP's on set 204, met "with the fast
  mode", meaning the owner's own configuration. It ended up **met at the defaults** (All, GPU off), see section 6.
- **K ships only past a truth bar** (B-K): K1 frame-level estimators unbiased across families, K2 the weight ranks the true
  weight no worse, K3 no worse than All in any lab family, K4 injected real frames move at most 1 %, K5 identity at All.
- **GPU ships only past SG1 / SG6** (B-G): Kernel 2 end-to-end 3x on Integrate, and the GPU-able share of the run at least
  15 % (about 127 s) even at infinite GPU speed.
- A lever lands when it is bit-identical (or truth-gated), swaps no more than H in any stage, and is faster beyond its
  interleaved bracket's spread (R-OSP-13 ruling 6). A bracket is H N H N H N, median of 3.

## 2. The machines

| | dev Mac | Mac runner | Windows runner (OBS-PC1) |
| ---- | ---- | ---- | ---- |
| CPU / memory | M4, 4 P + 6 E, 16 GiB | M4 Max, 12 P + 4 E, 128 GiB | Core Ultra 7 258V, 8 cores, 32 GB |
| GPU adapter | Apple M4, Metal, subgroup 4..64, TIMESTAMP yes, 32 KB workgroup storage | Apple M4 Max, Metal, same limits | Intel Arc 140V, Vulkan 1.4 (driver 32.0.101.6737) and DX12, subgroup 16..32 |
| role | set 204 brackets, F1 proof, G0, S1 | F2b / F3 truth runs, Horse C11 brackets, S2b | G0 Windows, AVX2 identity, rustafits tests |

Windows has no Python and builds with `cargo -j 4`; its outputs travel over the Universe share and are diffed on the Mac.
wgpu 30.0.1 reports `vendor = 0` on Apple (a vendor id cannot name Metal; identify by name or backend + device type).

## 3. `fitStars` K: the cut table and the truth verdict

**F0 (cut table, numbers only).** Filter SNR at rank K, median of the +-1 % band; "TMeanFlux beyond K" is the share of H's
winsorized mean flux that K removes.

| class | K 4 000 | K 5 000 | K 8 000 |
| ---- | ---- | ---- | ---- |
| s204 mono wide | SNR 78.7, TMeanFlux 69 % beyond | 55.1, 62 % | 27.7, 40 % |
| s204 mono sharp | 97.0, 72 % | 68.2, 65 % | 34.3, 44 % |
| s204 OSC | 30.5, 45 % | 21.6, 34 % | 11.0, 14 % |
| Horse C11 (maxStars 500) | not cut at any K | | |

B_reg (deepest Register-usable rank) = 3 576, so the grid 4 000 / 5 000 / 8 000 stood unlifted; the Auto-beta sample reaches
rank 2 942. Projected Measure per run (count-driven share 0.71-0.84): K 4 000 220 s, K 5 000 244 s, K 8 000 316 s, against H
476.5 s. Horse C11 is All-only by construction (its stored `maxStars = 500`), so its K rows are identity, not evidence.

**F1 (code).** `measurement.fitStars` (All | K), threaded into the measurement hash subtree; the master is always measured at
All (FM-28). **K5 identity at All: PASS** at F1 and again in F3 (fm-H v2 + inject: 996 / 996 probes, 0 moved, max |delta| 0
against the 13c baseline; the F1 proof on 0058 / 0075 / OSC 0163: 0 differing; subset 791 files 0 / 0).

**F2 (dense truth family).** No v2 lab frame carries more than 4 194 stars, so a K near 4 000 cut nothing. F2 built three
dense families (set 204 mono sharp / wide / OSC; 68 frames each, 204 in all; cuts at the filter-SNR the real frame has at K).
The first wide family failed (T1 43.0 vs 46.1, T3 +11.5 %): the lab's faint end was depleted just above the 6.00 floor. The fix
(R-F2b-1) was the power law's direction (alpha .708, N_r 6 650), not a wider bar. H baselines rho(PSFSW, w_true):
0.53 / 0.85 / 0.61 (sharp / wide / OSC).

**F3 verdict, SK2: no K passes.** One run, bars fixed beforehand.

| bar | result at 4 000 / 5 000 / 8 000 |
| ---- | ---- |
| K1 estimator bias | MISS everywhere. H itself exceeds the 2 % FWHM-tercile tilt (+5.7 / +2.0 / +12.0 %); K 5 000 sharp passes the tilts and misses the p10-p90 span (8.5 vs 7.3) |
| K2 weight ranking | PASS everywhere (sharp .565 / .565 / .550 against H .534; wide .892 / .876 / .864 against .846; OSC .672 / .635 / .646 against .614) |
| K3 no worse in any family | MISS: 92 / 96 / 94 of 146 families worse (by at most 0.42 pp); pooled FWHM error .20 -> .21 % |
| K4 injected real frames within 1 % | MISS: 21 / 19 / 14 of 32 planes beyond 1 %; worst -20.8 % / -17.5 % / +18.0 %, ecc up to .337 |

K4 is the decisive miss: three sparse real frames move 5-21 % in FWHM because **bright stars are not representative on
sparse frames** (a K prefix of the signal order keeps the saturated and the broad bright fits and loses the mid band the
frame statistics sample). K2b tilt .88-1.02 against F0's 1.08 is a realism flag that is moot once K4 refuses K. The aperture
correction c is unmoved (c_K / c_H = 1.000). **Consequence (FM-19):** All stays, nothing user-facing, F4 and F5 are not run.
**What stays:** the `fitStars` config field (`#[serde(default)]`, All) and the `star_probe --fit-stars` flag. No
Settings UI, no preset, no panel.

## 4. The GPU path

**G0 (harness, no kernel).** Metal adapters as in section 2. Two-sum (the error-free float-float building block): **plain
WGSL fails on every backend tested** (Metal, Vulkan, DX12: the compiler optimizes the error term away; Kahan 3 187-3 188 of
4 096 differ); MSL passthrough without a pragma fails; **MSL with `#pragma METAL fp math_mode(safe)` passes**. So only
Metal has an escape, and any GPU sum must pass as plain f32 or use exact integer accumulation. Windows: Vulkan subgroup and
workgroup reductions are bit-identical (~152 us), DX12 workgroup ~212 us and DX12 reports SUBGROUP false (a requested subgroup
arm silently falls back to workgroup).

**G1 Kernel 2 (linear-fit combine), SG1 fires.** Op model written before timing: 259 / 319 SIMD instructions per sample on
real stacks (mean 30 rofunc evals per stack). End to end against the post-4d CPU combine (13.0 / 13.9 ns per sample): **1.08x
on set 204 (n 92), 0.99x on Horse C11 L (n 74)**. The M4 issues about 23-25 G SIMD-instr/s through wgpu, issue-bound at about
**30 % of nominal**; interleaved GPU work slows the CPU 6-7 %; plain f32 bisection flips roots on 0.3-0.4 % of stacks.
Integrate stays on the CPU; no projection shows an Integrate saving.

**G5-pre, SG6 fires.** A 24-frame subset sampled at the Measure and Normalize starts (set 204 stage walls Measure 318 s,
Normalize 132 s, run 847 s): the fixed-beta fits are 21 % of Measure and 17 % of Normalize wall, so the GPU-able time is
**89 s (81-96)**. Saving 59 / 71 / 80 / 89 s at speedup 3 / 5 / 10 / infinity = 7.0 / 8.4 / 9.5 / **10.5 %** of the run, below
the 15 % bar (127 s) **even at infinite GPU speed**. The verdict fits (48 % of Measure CPU) stay on the CPU by ruling. The
GPU star-fit path ends (FM-31); G2-G4, G6 and G7 are not run, and there is no `gpu` feature, no `compute.gpu`, no GPU module.

The prototype was removed in A2 (bbc5ef00); it lived in 4d50e226 / 2a13adb6 / bba8fb9c.

## 5. Per-core levers (FM-30)

The owner asked (2026-10-18) why a Rust app was slower than the ERP. Per stage we were 1.3-2.4x faster in four of six; the gap
was Measure (more work by choice: 5.72 M fitted stars against the ERP's 2.03 M) and Integrate (the ERP's implementation is
hand-SIMD with branchless selection, ours scalar). The S2 audit split Measure's 404.5 s gap: count 230 s (57 %), per-unit
about 120 s (30 %), per-plane about 55 s (13 %). All levers below keep every artifact byte, per platform (libm `pow` differs
between macOS and MSVC), and keep the scalar path as the oracle.

- **S1, Integrate selection on SIMD** (`integration/combine/simd.rs`; NEON on aarch64, AVX2 through
  `is_x86_feature_detected!`). A windowed order-statistic selection, element-wise residual / sign-sum kernels, a round cap
  `2 * bitlen(n) + 8` with scalar fallback. Combine 0.655x (set 204) / 0.633x (Horse C11 L) ns per sample.
  Identity: 4-d oracles on both kernels, 143 200 + 32 200 stress stacks (400 per size 3..360 across ten shapes), subnormals,
  subset 791 files 0 / 0, `integrate_probe` masters byte-identical. **Windows AVX2 ran** (VectorToken present, "vector ==
  scalar" stress line). **S1b bracket** (set 204 from Integrate, H ff3c0a83 / N ca9e39d9): Integrate 2.94 -> 2.07 min
  (0.706, spreads 1 / 9 %), window 0.948, identity PASS (1 310 files 0 / 0).
- **S1c, the Median combination and the attribution read** on S1's selection. Combine Median|none 0.68, Median|sigma 0.71;
  identity 24 / 24 H / N pairs on Horse C11. Portable selection, so no new AVX2 proof beyond the four tests run on Windows
  with `ATHENAEUM_REQUIRE_VECTOR_KERNEL=1`. Percentile, sigma and winsorized internals measured out (section 7).
- **S2b-1, rustafits LM pass** (`lm_solve_moffat_with`): the JtJ / Jtr accumulation was runtime-`np` heap-Vec work, about 595
  instr / pixel / iteration at np 7 against about 40 for the Jacobian. Now `jacobian_pass::<7|8>` monomorphized, NEON across
  the accumulators (separate multiply and add, never FMA, pixel order kept) and JtJ reused after a refused step. Profile
  first (self 56.6 / 41.3 / 48.8 % in the LM function on 0058 / 0075 / 0163_d). Probe-diff 0 / 0 / 0 (123 717 / 146 068 /
  225 442 lines); single thread N/H 0.569 / 0.686 / 0.623. Counters: pass 2 6.7-9.1 iterations / solve, refused steps 17-29 %.
- **S2b-2, point-model columns two pixels per NEON vector.** Probe-diff 0 / 0 / 0; single thread N/H 0.948 / 0.971 / 0.956
  (about 1.5-2 % of the run, kept above the 1 % rule); `jacobian_pass` self 22.1 -> 17.9 % on 0058.
- **Reviews** (opus): S1 approve with fixes (I1 vector kernel could be silently absent on x86 -> the env guard; round cap),
  S2b-1 approve 0C / 0I / 7M, S1c approve 0C / 0I / 4M (a NaN sort panic is unreachable: non-finite values are dropped
  before the combine), S2b-2 approve 0C / 0I / 4M. Minors fixed in rustafits d0f0b16 / athenaeum 301a777f; controller re-gate
  with the vector guard on: 428 / 0 / 4.
- **Horse C11 on the runner** (H ff3c0a83 / N 15b39466, 3 pairs from Calibrate): Measure 1.13 -> 0.97 min (0.860), Integrate
  1.42 -> 0.95 (0.669), whole run 5.16 -> 4.46 min (0.864); identity 3 / 3 (654 files 0 / 0); swap 0.
- **Windows identity** (S1 + S2b-1 head): AVX2 ran, core tests 397 / 0, `star_probe` probe-diff H vs N on the Mac 0 / 0 / 0;
  walls H -> N 7.44 -> 4.93, 4.37 -> 3.52, 12.57 -> 9.46 s.
- **Set 204 whole-run bracket** (H ff3c0a83 / N db762cd9, 3 rounds, Calibrate to Drizzle): **1 090.7 -> 847.1 s (0.777)**;
  Measure 8.16 -> 5.30 min (0.649), Integrate 0.750, Normalize 0.803, Drizzle 0.893, Calibrate 1.055 and Register 1.046 (no
  code change in either; inside run-to-run range); identity PASS (1 310 files). Normalize swap per run H 684 / 546 / -25 vs
  N 373 / 410 / 860 MB: the max rule is not met but the pattern is noise (R-S1b-1 / R-SB-1: peaks overlap, H 7.73-8.64 vs N
  7.66-8.16 GiB, Integrate swap <= 0 everywhere). That is **15.7 % faster than the ERP's 1 005 s at the defaults.**

### Final-head results (A1 and Windows, at 301a777f)

- **Set 204, dev Mac, interleaved H ff3c0a83 / N 301a777f:** whole run 1082.2 → 794.1 s (N/H 0.734; N runs 794.1 / 796.8 / 793.6), against the ERP's 1005 s (run 2: 968 s) — 21 % faster at defaults; Measure 0.624, Normalize 0.761, Integrate 0.699, Drizzle 0.844, Register 0.966, Calibrate 0.981; identity H3 / N3 1310 files + 197 rows, 0 differing; the per-stage swap max-rule misses come from single N runs and are the OS's response to read rate (Normalize reads ≈ 58 GB per run, now 25 % faster) — malloc at every stage start N ≤ H (lab `fast-measure/a1-mem/vmmap-compare.txt`); lab 1910574
- **Horse C11, Mac runner, same H / N:** 164 lights, 3 pairs from Calibrate: whole run 305 → 267 s (N/H 0.876; the ERP's 2788 s → 10.4x), Measure 0.885, Integrate 0.667; identity 3 / 3 pairs (1962 files); swap 0 in all six runs; lab ae93eac
- **Windows final identity (S1 / S1c tests with the vector guard, rustafits tests, `star_probe` H db762cd9 / N 301a777f):**
  MSVC / AVX2 on OBS-PC1, vector gate on ("S1 stress … vector == scalar"); `star_probe` probe-diff 0 / 0 / 0 on 0058 / 0075 / 0163_d; the only failing tests are the pre-existing Windows pins (open-items); lab 9d3035e

## 6. Verdict against the bar

Owner Q1 asked for the bar at the fast-mode configuration. No K ships and no GPU path exists, so the only configuration is the
default (All, GPU off), and the whole-run bracket above meets it there: 847 s against 1 005 s on the same machine and frames.
Nothing user-visible changed: no setting, no command, no event, no `ts_export` field; every artifact is byte-identical to
13c's at All.

## 7. Measured out: do not re-propose

- **K at 4 000 / 5 000 / 8 000** against truth (section 3): K1, K3, K4 miss at every value; K2's gain does not rescue them.
  F0 for scale: K removes 62-72 % of H's TMeanFlux on mono at 4 000-5 000.
- **Kernel 2 on the GPU** (1.08x / 0.99x end to end, issue-bound at 30 % of nominal).
- **The GPU star-fit path at All** (SG6: 10.5 % of the run at infinite speed, bar 15 %).
- **Percentile, sigma and winsorized internals on the vector selection.** Percentile's sort is its output order; sigma has no
  order statistic; the winsorized MAD quickselect loses to the scalar path.
- **Caching u / v / base from the cost pass** (S2b-2): three stores per trial step cost more than the recomputation.
- **Target-rank pivot** for S1's selection: measured out in the cycle (recorded in `combine/simd.rs` doc comments).
- **Plain f32 bisection on the GPU** flips roots on 0.3-0.4 % of stacks; float-float needs the Metal-only `math_mode(safe)`.

## 8. What remains

- A **vectorized `pow`** (libm `pow` + `__powidf2` are about 31 % of the fit thread in the cost pass). It changes numbers, so
  it is truth-gated and needs a full-group master-level checkpoint, never a few-frame probe.
- The **verdict fits are 48 % of Measure CPU**, and CPU-only by ruling.
- **Normalize**: the LN reference build is about 34 % of its wall and detection about 20 %. Both are future per-core levers.
- Linux and MSVC bit identity of the new kernels is by construction plus the Windows AVX2 proof; no Linux x86 run.

## 9. Process lesson

Compare **work units** with the ERP (stars fitted, fits per star, planes), never stage walls: Measure's 405 s gap was 57 %
work count by choice. And **per-core before threads or GPU** (owner, 2026-10-18): 13c took the threads, this cycle showed the
GPU cannot pay for itself, and a bit-identical per-core pass (S1 / S2b) took 22 % off the whole run. Every vector kernel keeps
its scalar oracle and a guard (`ATHENAEUM_REQUIRE_VECTOR_KERNEL`) so a green test cannot hide a missing vector path.
