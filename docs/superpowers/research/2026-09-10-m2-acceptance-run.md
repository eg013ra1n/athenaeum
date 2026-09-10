# M2 acceptance run — local normalization on LDN 1272

**Date:** 2026-09-10 · **Plan:** `docs/superpowers/plans/2026-09-10-stacking-m2-plan-local-normalization.md` (Task 9) · **Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §5.2, §13 · **Baselines:** `2026-09-09-checkpoint-b-integration.md` §3 (the external run's masters: mono rejected 2.831 %, noise 1.9570e-05; OSC rejected 2.51 / 2.79 / 2.64 %, noise 1.6716e-05 / 1.6631e-05 / 1.4975e-05 — integrated with local normalization on both sides) and `2026-09-09-m1-acceptance-run.md` §5 (our M1 masters, bit-identical to Checkpoint B's) · **Machine:** Mac mini, 10 cores, 16 GB RAM, macOS 26.5.2.

## 1. Purpose

The first run of stage 6 as a real stage: per group an LN reference, per frame a background model and a PSF-flux scale, `.athln` sidecars, and the engine applying `v′ = A·v + B` as output AND rejection normalization — on the same set, reference and geometry as M1, so every number below is directly comparable to the M1 run and to the external masters. The run also exercises the owner's grouping-rule change of the same day (Task 10: camera-agnostic groups keyed by colour mode, filter, binning and exposure).

## 2. Setup

- **Build:** `athenaeum-web` release from branch `worktree-stacking-m2` at `40b6780b` (Tasks 1–7, 10 landed; the run's numbers do not depend on the frontend Task 8 that landed during the run, `70c6a741`), separate target dir; frontend built with `NODE_ENV=development` and served by the server itself on `:8931`.
- **Catalog and folders:** the dev catalog, the same working/output folders as M1; the M1 intermediates (4.36 GB of stale calibrated artifacts under the old camera-keyed group folders) deleted through `cleanup_stacking_work` before the run.
- **Config:** the Default preset + `reference.mode = manual` pinned to `2025-10-18_02-02-02_0073` (the external stacker's reference, as in M1 run 2) + `normalization.local.enabled = true` (scale 1024, 20 reference frames, PSF model auto) + `normalization.rejection = "local"`; rejection Auto → linear fit 5.0 / 3.5 (n ≥ 20). Config hash `d4b7d9baaad97c05`.
- **Groups under the new rule:** `mono__NoFilter__bin1__180s` (208 frames, ATR2600M) and `osc__NoFilter__bin1__180s` (160, ZWO ASI2600MC Duo) — the same two groups as M1 (they differ by colour mode), now named without the camera.
- **Plan before the run:** no blockers, `staleStages = [calibrate, measure, normalize]`, masters to build 0 (the M1 run rebuilt them), estimate 72.1 GB, 761.7 GB free.

## 3. Run 6 — full run with LN on both sides

Started 07:51:48 Z from `start_stacking`, finished 08:41:12 Z: **49.4 min wall**.

| Stage | Wall | Notes |
| ----- | ---- | ----- |
| Calibrate | 5 min 19 s | 368 frames, 71.7 GB (regenerated under the new group keys) |
| Measure | 10 min 14 s | as M1 (memory-bound fan-out on 16 GB) |
| Register | 1 min 11 s | re-registered — the calibrated artifacts' hashes changed with the group keys |
| **Normalize (LN)** | **≈ 28 min** (not in the summary — finding 1) | mono: reference + 208 sidecars in ≈ 11 min (≈ 19 frames/min); OSC: reference + 160 three-plane sidecars in ≈ 16 min (≈ 10/min) |
| Integrate | 4 min 26 s | both groups, LN for rejection and output |
| Output | 0.3 s | `LDN_1272_NoFilter_mono_180s_208x.fits`, `LDN_1272_NoFilter_osc_180s_160x.fits` (the new naming) |

No warnings, no exclusions, `lnFrames` 208/208 and 160/160, every frame's sidecar 13.7 KB (mono) — `ln/` holds 405 MB, almost all of it the two reference frames.

**Mono** (LN both sides): rejected **0.182 % low / 0.649 % high = 0.831 %** (M1: 0.092 / 0.561 = 0.653 %; external 0.841 / 1.989 = 2.831 %); master MRS noise **1.8199e-05 = 0.93 × the external's 1.9570e-05** (M1: 1.7580e-05 — LN adds the reference's background noise, +3.5 %); FWHM 2.683 px (M1 2.707); ecc 0.304; SNR gain 627; LN scales 0.829–1.127 (median 0.988).
**OSC** (LN both sides): rejected **0.136 % / 0.602 % = 0.738 %** (M1 0.632 %; external 2.51–2.79 %); noise **[1.4863e-05, 1.5510e-05, 1.6856e-05] = 0.89 / 0.93 / 1.13 × the external's** (M1: 0.99 / 1.19 / 1.29 — LN pulls the G and B channels in by 22 % and 13 %); FWHM [2.81, 2.74, 2.61] (M1 [2.97, 2.87, 2.69]); LN scales 0.965–1.229 (median 1.005).

**Mesh imprint:** `master_LN − master_M1` (run 2's master, same geometry): the row and column mean profiles folded at the 128 px stride have a peak-to-peak of 1.57e-05 / 1.47e-05 with a scatter of 3.3e-06 — indistinguishable from a random fold (1.41e-05 / 1.61e-05, 3.1e-06); the FFT power at the stride period is 1.68× (rows) and 0.99× (columns) the neighbourhood median. No imprint above the noise.

## 4. Run 7 — sensitivity of the rejected fraction to the linear-fit thresholds

Re-run from Integrate (every cached flag true, sidecars included) with linear fit **3.5 / 2.5** instead of 5.0 / 3.5, LN unchanged:

Started 08:42:59 Z, finished 08:47:46 Z — **4 min 47 s**: calibrate 32 ms, measure 73 ms, register 0.5 s, integrate 4 min 45 s; `cached_calibrated`/`cached_metrics`/`cached_registration`/`cached_ln` all 368/368; `_2` masters written.

| Group | Rejected low / high / total | External | Master noise (× external) |
| ----- | --------------------------- | -------- | ------------------------- |
| mono, 5.0 / 3.5 (run 6) | 0.182 / 0.649 / **0.831 %** | 0.841 / 1.989 / 2.831 % | 1.8199e-05 (0.93) |
| mono, 3.5 / 2.5 (run 7) | 0.962 / 1.948 / **2.909 %** | 0.841 / 1.989 / 2.831 % | 1.9135e-05 (0.98) |
| OSC, 5.0 / 3.5 (run 6) | 0.136 / 0.602 / **0.738 %** | 2.51 / 2.79 / 2.64 % per channel | 0.89 / 0.93 / 1.13 |
| OSC, 3.5 / 2.5 (run 7) | 0.798 / 1.869 / **2.667 %** | 2.51 / 2.79 / 2.64 % | 0.95 / 1.00 / 1.23 |

At 3.5 / 2.5 both groups land on the external run's rejected fractions to within 3 % relative, low and high sides alike, and the mono noise is within 2 % of the external's. The external run's thresholds (5.0 / 3.5) therefore correspond to ≈ 3.5 / 2.5 in our dispersion units: **our linear-fit dispersion estimate is ≈ 1.4× the external's** (the `2·adev·sqrt(1+b²)` form of the math reference §3.4 against a σ-like robust estimate — for a Gaussian `2·adev ≈ 1.6σ`). That is the whole remaining gap; LN was a quarter of it.

## 5. Targets (spec §13, re-measured with LN on both sides)

| Target | Result | Verdict |
| ------ | ------ | ------- |
| Rejected fraction 1–4 % per group (external 2.83 % / 2.5–2.8 %) | 0.83 % mono, 0.74 % OSC — LN raised M1's 0.65 % / 0.63 % by 27 % / 17 % relative | **miss**, attributed (§6) |
| Master MRS noise within 5 % of the external master's | mono 0.93×; OSC 0.89 / 0.93 / 1.13× | mono and R/G below the external's (favourable, outside the ±5 % band on the good side); B 13 % above — **partial**, attributed (§6) |
| FWHM unchanged vs M1 (2.71 px) | 2.68 mono; OSC 2.81 / 2.74 / 2.61 (M1 2.97 / 2.87 / 2.69) | pass (better) |
| No residual trail; no mesh imprint | trail region clean as in M1 (same geometry); mesh check negative | pass |
| Frames 208/208 + 160/160 (or exclusions named) | all included, `lnFrames` = included | pass |
| LN stage ≤ 10 min for 368 frames | ≈ 28 min | **miss**, attributed (§6) |
| Sidecars ≤ 2 MB per frame | 13.7 KB | pass |
| `ln_probe`: residual background flat to within 2× the noise | Task 7's report: the residual shrinks measurably; the probe's number is recorded there | pass (Task 7) |
| Re-run from Integrate with sidecars cached → `cached_ln` all true; Delete intermediates removes `ln/` | run 7: `cached_ln` 368/368, 4 min 47 s; cleanup freed 72.1 GB incl. `ln/`, `lnBytes = 0` | pass |

## 6. Findings and rulings

1. **The Normalize stage pushes no `StageTiming`.** `RunSummary.stages` sums to 21.2 min for a 49.4 min run; the LN stage's ≈ 28 min are invisible in the summary, the provenance modal and the board's timings. Fix in the branch's final fix wave (push `Stage::Normalize`'s timing like every other stage).
2. **The rejected fraction is still 3.4× below the external run's.** LN was the named cause of the gap (Checkpoint B §10.1) and it moved the fraction by only +27 %. The remaining candidates are the linear-fit dispersion estimate (Checkpoint B §10.1 (b): the external's line fit is robust; ours is the `2·adev·sqrt(1+b²)` OLS form of the math reference) and the exact per-pixel rejection-normalization scale. Run 7 (§4) measures how far the thresholds alone move the fraction. Ruling: M2 ships with the fraction recorded as a miss and the M4 robust-line-fit item promoted to the first task of M4's rejection-parity work; the master quality (noise below the external's, FWHM better) is not harmed by the under-rejection on this data set (no trail residue, no artefacts).
3. **LN time is memory-bound on this machine**, exactly like Measure in M1: the fan-out admits `clamp(RAM/4 ÷ working set)` frames, and a 3-plane 6248×4176 warped frame plus its grids is ≈ 0.6 GB per worker, so the OSC group ran ≈ 6 workers at 10 frames/min and the mono group ≈ 19/min. A 64 GB machine gets the intended parallelism. The per-frame cost itself (warp + two background models + a star fit on both planes ≈ 3 s mono, 6 s OSC on one core) is an M4 performance item. Post-review: the reference's star fit was recomputed per frame; hoisted in the final fix wave — the per-frame PSF cost roughly halves, re-measured in M3's acceptance run.
4. **OSC blue-channel noise 13 % above the external's.** With LN the channel ratios are no longer the M1 picture (0.99 / 1.19 / 1.29); the residual B excess follows the per-CFA-channel flat normalization convention (Checkpoint B §10.4) and the external run's whole-flat normalization — a level/gain convention, not a stacking defect; recorded, no action in M2.
5. **Grouping rule v2 verified on the real set:** the two groups keep their membership (colour mode still splits), the keys and master names dropped the camera token, `ATH_STKC` lists the camera, the tab's groups table shows the cameras column.
6. **Click-through (over the LAN from the owner's Windows Chrome — the only connected browser):** the Local normalization row reads `Local · scale 1024 · ref 20 frames`, Integrate `… · LN rejection`, the LN block's controls are live (enable, scale, reference frames, PSF model; local scale disabled "arrives in M4"), the frames table has the Scale column, the results card names the LN reference. At that browser's ≈ 1250 CSS px width the toolbar row overflows the pane to the right — the narrow-layout item the owner already owns, now with evidence; the fix (wrap the toolbar below ≈ 1400 px) goes into the final fix wave.

## 7. Re-run and cleanup

Run 7 is the re-run-from-Integrate check: every cache flag true including `cached_ln` 368/368, only integration took time. **Delete intermediates** afterwards freed 72.1 GB — `calibrated/` and `ln/` (405 MB, the sidecars and both references) gone, `lnBytes = 0`, the working folder holds only `runs/` (1.6 MB of manifests) and an empty `tmp/`; the ten masters in the output folder untouched.

## 8. Verdict

M2's local normalization works end-to-end on the real set: every frame normalized, no exclusions, no mesh imprint, the masters' noise at or below the external run's, FWHM improved, and the OSC colour balance pulled in. Two targets are missed — the rejected fraction (attributed to the M4 robust line-fit item, with run 7 quantifying the threshold sensitivity) and the LN stage time (machine-memory-bound) — and one summary defect was found (the missing Normalize timing). M2 is **green with two attributed misses**; the fix wave closes the timing defect before the merge.

## 9. Release-note lines (draft, English)

- **Local normalization.** The stacking pipeline's Normalize stage can now build a per-group reference and correct every frame's background and scale locally (a background model on a coarse mesh plus a PSF-flux scale), for the master light and for outlier rejection — enable it in the stage's panel; sidecars are cached like every other stage.
- **Grouping by exposure, not by camera.** Frames from different cameras with the same colour mode, filter, binning and exposure (within the tolerance) now integrate together; master names read `<object>_<filter>_<mono|osc>_<exposure>_<n>x.fits`.
