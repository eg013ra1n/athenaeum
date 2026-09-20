# Stacking compute Tier C — acceptance run (2026-09-20)

**Spec:** `docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md` (C1–C6, §8 the
gate, §9 rulings C-1…C-28). **Plan:** `docs/superpowers/plans/2026-09-20-stacking-compute-tierC-plan.md`
(Tasks 0–7, checkpoints C-1/C-2/C-3). **Audit:** `docs/superpowers/research/2026-09-19-stacking-compute-audit.md`
(§7 carries the measured Tier C column). **Branch:** `perf/stacking-tierC` (`3c90d83a` → the
acceptance head `682d218c` + the docs commits), rustafits `perf/stacking-kernels`
(`a35097e` → `d98629e`). **Ruler:** `.athenaeum-acc/tierA-baseline` (ruling C-19).

## 0. Verdict

**Every gated §8 row PASS against the Tier A ruler; 21.73 → 14.47 min on the reduced set
against a Tier A build re-run back to back (−33 %; −38 % against Tier A's own acceptance run
on a hotter evening; −46 % against the pre-audit baseline); the external-masters gate PASS.** The
masters are within 0.004 % (mono median) / 0.0015 % (OSC medians) of the ruler's, MAD within
0.1 %, noise within 0.6 %, FWHM within 0.72 % (mono) / 0.02 % (OSC), the rejected fraction
+0.002 pp, the per-frame weights identical (ρ = 1.0, top-20 20/20), the LN relative scale
−0.035 % with 0.28 % scatter. Two absolute rows read FAIL for data reasons the baseline shares
(rulings C-20/C-23, §4). Four of the six items shipped (LN from Measure's fits as seeds with a
calibrated per-channel `k`, Register from the fits on mono, the 64-phase drizzle table, the
Moffat LM arithmetic); two were built, measured on the full set and reverted with their
numbers (the `medfit_line` warm bracket, C-26; the binned LN background, C-29 — the latter
after a first acceptance attempt, C-3a, failed two §8 rows). The same-evening Tier A re-run
bracket is in §2's last column.

## 1. The measurement unit and method

The same unit as Tier A: the reduced set `LDN1272-test` (prod set 204, 92 mono ATR2600M
6224×4168 + 105 OSC ASI2600MC 6248×4176, 180 s), LN on (`rejection: local`), Bayer drizzle 2×,
distortion off, Auto two-pass reference, FITS output, `cleanup: keepAll`, every run a fresh copy
of `.athenaeum-acc/tierA-template` through `tier1/checkpoint.sh <name> baseline --numeric`
(release `athenaeum-web`, root profile `lto = "thin"` / `codegen-units = 4`, the 10-thread
`image_pool`, this 16 GB / 10-core Mac, Time Machine idle, no cargo alive). Tier C changes
outputs on purpose, so the gate is spec §8's tolerance table against the ruler, not byte
identity. Two differences from Tier A's method, both recorded in the ledger:

- **The ruler is `tierA-baseline`, not `tierA-tierA`** (ruling C-19): the Tier A acceptance
  proved the two trees byte-identical on every artifact the numeric compare reads (four masters,
  565 registration rows, 197 `.athln` sidecars, 302 calibrated frames); the `tierA-tierA` tree
  was lost to a harness invocation error before C-1 (`bash` on the zsh script shifted its
  positional parse and it re-copied the template over the base — the script now re-execs
  under zsh and refuses `name == base`, `38008c30`).
- **Weight maps are on** in the template from C-2 (so the coverage row has data); a weight map
  is an extra output file and changes no master or drizzle value.

The wall total is read against a re-run of the **Tier A build** (local main `e752aaa7`, a
throwaway worktree with its own target dir) launched back to back with the Tier C run
(ruling R-TA-9), and the external gate (`tierC-external.sh`, rulings C-8/C-9) compares the
Tier C masters with the external tool's masters of the same frames.

## 2. Stage table (minutes, `stage finished` durations)

| Stage | tierA (09-19 23:09Z) | C-1 (Group 1) | C-2 (Group 2) | C-3a (with Task 7, reverted) | tierC (C-3b) | tierA re-run (back to back) |
| ----- | ---- | ---- | ---- | ---- | ---- | ---- |
| Calibrate | 2.43 | 2.14 | 2.37 | 2.17 | 2.06 | 2.02 |
| Measure | 4.48 | 4.36 | 4.92 | 3.76 | **3.67** | 4.28 |
| Register | 2.36 | 1.71 | 1.76 | 1.53 | **1.52** | 1.76 |
| Normalize | 5.70 | 2.58 | 2.88 | 1.71 | **2.01** | 5.51 |
| Integrate | 4.19 | 4.08 | 4.67 | 3.97 | 4.03 | 4.02 |
| Drizzle | 4.26 | 4.41 | 1.29 | 1.22 | **1.18** | 4.14 |
| **Total** | **23.44** | 19.29 | 17.90 | 14.36 | **14.47** | **21.73** |

C-1, C-2 and C-3a were not bracketed (reported only); the Tier C total is read against the
same-evening Tier A re-run in the last column: **21.73 → 14.47 min, −7.26 min (−33 %)** on the
same machine state (the re-run's own build, 16:51–16:53Z, then its run 16:53–17:16Z, 40 min after
the Tier C run — the machine was cooler than on Tier A's acceptance evening, which is why the
re-run reads 21.73 where the acceptance read 23.44; every untouched stage — Calibrate,
Integrate — agrees between the two runs to 0.05 min, which is the bracket's own check). Per stage
against the re-run: Normalize 5.51 → 2.01 (−3.5), Drizzle 4.14 → 1.18 (−3.0), Measure 4.28 →
3.67 (−0.6), Register 1.76 → 1.52 (−0.2), Integrate 4.02 → 4.03 and Calibrate 2.02 → 2.06
(untouched). Against the pre-audit baseline of the same evening as Tier A (27.00): −46 %. C-3a is the run that failed §8 on Task 7's `B`
grids (mono MAD +1.9 %, OSC blue FWHM +2.7 %) and led to ruling C-29; its Normalize 1.71 is the
binned background's cost saving plus the machine's state, and is not part of the tier.

## 3. Per-frame medians (ms) — tierA → C-1 → C-2 → tierC

| Event | field | tierA | C-1 | C-2 | tierC | what moved it |
| ----- | ----- | ---- | ---- | ---- | ---- | ---- |
| frame plane measured | fit | 367 | 340 | 396 | **202** | C3 (Task 5; −30 % on the probe, −49 % here with the fixed group β) |
| frame plane measured | duration | 1906 | 1906 | 2230 | 1592 | (band + C3) |
| frame stars detected | detect | 730 | 268 | — | 236 | (Tier A T4; Register no longer calls it on mono) |
| frame registered | read / detect | 136 / 730 | 0 / 0 | 0 / 0 | **0 / 0** | C6 (Task 3: fits reused, header-only open) |
| ln frame normalized | ln_detect | 3969 | 2 | 3 | **2** | C1 (Task 2: seeds, no detection) |
| ln frame normalized | ln_fit / scale | — / 5277 | 1000 / 1026 | 1269 / 1310 | **606 / 636** | C1 + C3 |
| ln frame normalized | background | 391 | 637 | 633 | 463 | (band; C5 reverted) |
| plane integrated | combine_cpu | 557 s | 539 s | — | 539 s | (C4 measured out, ruling C-26) |
| drizzle plane deposited | deposit | 41 202 | 44 216 | 5 630 | **4 812** | C2 (Task 4: the phase table) |

`rejection_iters_mean = 2.31` and `medfit_evals_mean = 33.96` on every run — the rejection
algorithm's shape never moved (C4 was reverted).

## 4. The §8 table (tierC vs tierA-baseline; `tierC-compare.txt` beside the ledger)

| Row | tolerance | mono | OSC R / G / B | verdict |
| --- | --------- | ---- | ------------- | ------- |
| master median | ± 0.1 % | +0.0044 % | +0.0010 / −0.0015 / −0.0009 % | PASS |
| master MAD | ± 1 % | −0.068 % | +0.0004 / +0.052 / +0.094 % | PASS |
| master noise (MRS σ) | ± 2 % | −0.61 % | +0.034 / +0.26 / +0.061 % | PASS |
| FWHM | ± 1 % | +0.72 % | −0.021 / −0.017 / −0.0004 % | PASS |
| rejected fraction | ± 0.3 pp | +0.0019 pp | +0.0021 pp | PASS |
| per-frame weights | ρ ≥ 0.99, top-20 ≥ 18 | ρ = 1.0, 20/20 | ρ = 1.0 | PASS |
| LN relative scale | median 1 ± 0.5 %, scatter ≤ 1 % | (all frames) −0.035 %, 0.28 % | | PASS |
| drizzle level | 0.998–1.002 absolute | 0.999212 | 0.997995 / 0.999457 / 0.998555 | FAIL on R (C-20: the baseline itself is 0.998007; relative −0.001 %) |
| drizzle coverage | 1.0 absolute | 0.993484 | 1.0 / 1.0 / 1.0 − 2 px | FAIL (C-23: the mono footprint's edge wedges; two OSC-blue edge pixels) |
| wall | reported | 14.47 min | | see §2 |

The mono FWHM +0.72 % and noise −0.61 % are the one pair inside tolerance but not at zero;
they were already there at C-1 (Group 1) and did not move through C-2/C-3, so they belong to
the LN-scale change (seeds + `k`), not to the drizzle table or the fitter. The external gate
(`tierC-external.txt`) passes its two gated rows — the drizzled/undrizzled FWHM ratio within
25 % of the external tool's on all four planes and the per-frame weights against its log
(mono ρ 0.93, top-20 17/20; OSC 0.91 / 0.94 / 0.86, 16/20).

**Ruling C-28's added reads (tierC vs C-2, `c28_reads.py`):** `ln_cells_rejected` identical
on all 197 frames; the mono `.athln` sidecars byte-identical, the OSC ones within 2e-4 of
`A` and 3e-4 of sky in `B` (the fitter's ulp-level move through the calibration `k`); the
worst node's 512×512 neighbourhood on the master: median ratio 1.000000, MAD 0.99994.
**Coverage zero set** (`drizzle_probe --coverage-out --ref 6248x4176`, all 92 mono frames, the
run's own maps): the tabulated arm's union coverage mask equals the run's weight map
`W > 0` set **pixel for pixel over 104 366 592 pixels** (XOR = 0, 680 070 zeros); the exact
overlap differs from it by 36 pixels (23 exact-only, 13 table-only), every one at Chebyshev
distance 1 from the footprint boundary, none nearer than 4 px to the canvas border — 3.4e-7
of the grid. The absolute 0.993484 is therefore the 92 dithered frames' footprint on the
co-registered canvas, not the phase table (ruling C-23 closed). Incidental: on the full group
the deposit read −89.4 % (96.7 → 10.3 s), the per-frame table build amortising further than on
the 10-frame bracket.

## 5. What each task delivered

| Task | Item | Measured | Landed |
| ---- | ---- | -------- | ------ |
| 0 | harness: `--numeric` compare (§8 table), `tierC-external.sh` (C-8/C-9), coverage tolerance 0.999995 → 1.0 | — | yes |
| 1 | `fits` artifact per plane (`fits_artifact.rs`, `FITS_ARTIFACT_VERSION = 1`), group β (lower median, C-1a), `PsfModel::Fixed`, `PSF_FIT_VERSION = 3` | — | yes |
| 2 | LN scale from Measure's fits as SEEDS (C-12), per-channel stratified calibration `k` (C-14/C-15), hashes follow the Measure config (C-17) | LN scale 6.5 → 1.0 s per frame; hold-out signed median −0.05 % (was +0.44 % with the best-weighted sample); C-1 master median +0.004 % | yes (5 rounds + addendum) |
| 3 | Register reuses the fits on mono (C-6), registration hash follows the Measure config (C-18) | detect 268 → 0 ms, read → 0 (header-only) | yes |
| 4 | drizzle phase table, `PHASES = 64` (C-21), `DRIZZLE_KERNEL_VERSION = 2` | deposit −85 % (per-frame arm), −62 % (tiled); per-pixel ≤ 1.1–1.6 % of the exact clip, level 1 ± 8e-6 | yes |
| 5 | Moffat LM arithmetic (rustafits `18377bd`/`d98629e`): one transcendental per sample-iteration, residual reuse, Cholesky scratch | fit −30 % (Auto) / −34 % (β fixed); iterations identical 180/180; ≤ 7e-15 px (C-24: no version bump) | yes |
| 6 | `medfit_line` warm bracket | 0.966× combine — reverted (C-26); the exit-on-zero-rejection pre-existed and is pinned | pin only |
| 7 | LN background on a 4×4-binned plane | background 83 → 15 ms (4 thr) on the probe — but on the full set the `B` grid moved 2.4e-3 of sky at the median node (4× the fixture bar; the 3-frame probe read 9e-4), single nodes to 4× sky, and the masters failed §8 (mono MAD +1.9 %, OSC blue FWHM +2.7 %) — reverted (C-29) | doc verdict only |

## 6. Rulings that changed the plan

C-9 (external FWHM reported, not gated — the external masters are of the full set), C-12 (fits
are seeds, not fluxes: a Moffat fit's integrated signal is not warp-invariant — up to 20 %
FWHM-dependent), C-13/C-14/C-15 (the per-group, per-channel, stratified calibration `k`),
C-16 (the median estimator over the worst case), C-17/C-18 (the LN and registration hashes fold
the per-frame measurement hash), C-19 (the ruler), C-20/C-23 (the two absolute drizzle rows are
data facts on this set, reported not re-sized), C-21 (64 phases), C-24 (no fitter version bump),
C-26 (C4 measured out), C-27 (the derived 2e-3 bar), C-28 (C-3's added reads). The full text
is in the spec's §9 and the plan ledger.

## 7. What the tier established beyond numbers

- **A Moffat fit's integrated signal is not warp-invariant** (H2, Task 2): the same star on the
  native and the B-spline-warped frame fits to a signal differing by up to 20 %, FWHM-dependent,
  while aperture flux is conserved to 0.6 %. Any future "reuse the fit" idea across a resample
  must re-fit; positions carry over, fluxes do not.
- **The stage hashes now follow the Measure config** (C-17/C-18). Before Tier C a
  `seedDetector` change rewrote every `.athf` while the plan gate reported Normalize and
  Register cached — a stale-reuse hole that predated this tier and closed with it.
- **Two harness lessons**: a per-frame numeric change is accepted only after a full-set
  checkpoint reads the masters (Task 7's 3-frame probe understated its `B` move 2.5×); and
  `checkpoint.sh` must not be run under `bash` (fixed in the script — the Tier A ruler tree was
  lost to it, C-19).
- **The absolute drizzle rows need a comparative reading on a reduced set** — the level bound
  sits at the baseline's own value and coverage is a footprint fact; both are reported, not
  re-sized, and the zero-set comparison against the exact overlap (§4) is the comparative row
  that answers what the absolute one cannot.
- **Small gains do not buy numeric changes**: C4 (≈ 3 % of the combine) and C5 (≈ 0.4 min)
  were built, measured on real data and reverted with their numbers in the doc comments — the
  Tier A precedent (Task 9/10) applied to the numeric class.

## 8. Owed to the owner

- The push of `main` and both submodule branches — nothing is pushed.
- The desktop click-through: nothing in the tab changed by design, but the Normalize row's
  calibration progress message (`calibrating k · frame i/7`) and the Register rows at 0 ms are
  new to look at.
- The rustafits third-party-names question from Tier A still stands.
