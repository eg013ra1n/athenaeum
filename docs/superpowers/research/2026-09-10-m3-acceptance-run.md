# M3 acceptance run — drizzle 2× on LDN 1272 (2026-09-10)

## 1. Purpose

Spec §13's drizzle target: the drizzled/undrizzled FWHM ratio of our 2× drizzled masters within ±5 % of the external reference's ratio on the same frames, plus the M3 plan's Task 7 measurements — drizzle wall time per group (≤ 15 min), the `rej/` bitmap footprint and its removal, output sizes, level preservation, no band seam at the 512-row scheduler period, coverage, the weight map, and the tab's drizzle controls.

## 2. Setup

- **Builds:** `athenaeum-web` release from branch `worktree-stacking-m3` — `cd862317` for run 8/9 (Tasks 1–5; static bundle from `45bf209f`), `a4088d90` for run 10/11 (the whole-branch review's fix wave — the OSC group's drizzle needed it), `98ed9d85` for run 12 (Task 8, the sky-penalized normalization anchor; its fix round `5009552a` changes edge cases and docs only), served on port 8931 with `ATHENAEUM_STATIC_DIR` pointing at a same-origin Vite build (`NODE_ENV=development VITE_TARGET=web npx vite build --outDir …`), on the dev catalog (`com.vsharifov.athenaeum.dev`, 39 075 frames).
- **Set:** LDN 1272 (set 109): 208 mono lights (ATR2600M, 6224×4168) + 160 OSC lights (ZWO ASI2600MC Duo, 6248×4176), 180 s, one night span 2025-09-13 … 2025-10-18; groups `mono__NoFilter__bin1__180s` and `osc__NoFilter__bin1__180s` (M2's camera-agnostic rule).
- **Config:** the M2 acceptance run-6 config (defaults, reference manual = `…_0073` frame 29390, local normalization ON for output and rejection, linear fit 5.0/3.5 by the Auto rule) + `drizzle { enabled, scale 2, dropShrink 0.9, kernel square, useRejection, useWeights, useLocalNormalization, writeWeightMap }`, `output.cleanup keepAll`; config hash `baf93c50e634cc94`.
- **Cache state:** the M2 close-out's "Delete intermediates" had removed every intermediate, so run 8 is a FULL run (calibrate, measure, register, LN, integrate, drizzle) — `staleStages [calibrate, measure, normalize]` at plan time, the plan estimate 77.7 GB against 797 GB free.
- **External reference:** the same tool that produced the M1/M2 baselines also produced 2× drizzled masters of both groups (`…/LDN1272-Output/master/masterLight_…_mono_drizzle_2x.xisf`, `…_RGB_drizzle_2x.xisf`, 2026-09-08). Their FWHM was measured with OUR estimator (`examples/measure_probe`, XISF support added in Task 5) so both sides of every ratio come from one measurement:

| External master (our estimator, FITS path) | FWHM px | Ratio drizzled / (2 × undrizzled) |
| ---- | ---- | ---- |
| mono undrizzled | 2.6749 | — |
| mono drizzle 2× | 4.9535 | **0.9259** |
| RGB undrizzled | 2.7039 / 2.7709 / 2.7488 | — |
| RGB drizzle 2× | 5.1566 / 4.1370 / 4.0739 | **0.9536 / 0.7466 / 0.7410** |

"FITS path": the first `<Image>` of each XISF (raw Float32 attachment) written as a float32 FITS and measured with `measure_probe`'s FITS branch — the same code path as our own masters. `measure_probe`'s new XISF branch (Task 5) gave different numbers for every file (mono drizzle 4.3464 instead of 4.9535, RGB undrizzled 2.79/2.69/2.63 instead of 2.70/2.77/2.75); see finding 1.

The external R plane's drizzled FWHM (5.16 px, eccentricity 0.45, ratio 0.95) is out of line with its own G/B planes (0.74–0.75, eccentricity 0.32); see finding 4.

## 3. Run 8 (full run, drizzle 2× both groups)

Started 15:08:52Z, done 15:48:52Z — **40.0 min wall** (M2's run 6 took 49.4 min with the same stages and no drizzle).

| Stage | min | Note |
| ---- | ---- | ---- |
| calibrate | 5.31 | as M2 |
| measure | 12.47 | as M2 (memory-bound on 16 GB) |
| register | 0.01 | cached from M2 |
| normalize | 13.03 | **M2: ≈ 28** — the M2 final-wave hoist of the LN reference's star fit halved the stage |
| integrate | 4.43 | as M2 |
| drizzle | 4.74 | the mono group only (see below); read 7.4 s, deposit 233 s, 21.6 GB read |
| output | 4.75 | double-counts the drizzle duration (whole-branch review I2, fixed in the final wave) |

- **Mono group** (208/208, LN 208/208): master `LDN_1272_NoFilter_mono_180s_208x_3.fits` — rejected 0.182 % / 0.649 %, noise 1.8199e-05, FWHM 2.683: bit-for-bit the M2 run-6 master (the drizzle stage does not touch the master). Drizzle `…_3_drizzle2x.fits` 12448×8336 (415 MB) + `…_drizzle2x_weight.fits`: FWHM **4.9791**, eccentricity 0.312, coverage 1.0, 208 `.rej` bitmaps (3.27 MB each).
- **OSC group** (160/160): master `…_osc_180s_160x_3.fits` bit-for-bit M2's; **drizzle refused** — warning `drizzle failed: drizzle bad input: … c_2025-09-14_00-55-28_…_d.fits: geometry 6248x4176x3 …` — the driver compared each frame's NATIVE geometry with the run's reference geometry (the whole-branch review's I1, found while run 8 was calibrating; fixed in the final wave, re-run in §7). Loud and safe, exactly as the review predicted: the master stayed, the group stayed `done`, `drizzle_path` NULL.
- Sizes: `rej/run-8/` 368 bitmaps = 2.24 GB (the panel's estimate: 2.10 GiB); mono drizzle + weight map 830 MB; the plan's 77.7 GB estimate vs `work/` ≈ 71 GB actual.

**Run 9** (sensitivity, 15:55–16:06Z, 11.1 min from Integrate with everything cached): the same config with linear fit 3.5/2.5 (M2's run-7 setting, which reproduces the external tool's 2.9 % rejection): mono rejected 0.962/1.948 %, master FWHM 2.685, drizzle FWHM **4.9833** (ratio 0.9281 — unchanged from run 8's 0.9279). Rejection strength does not move the drizzled FWHM.

**Mono checks** (`drzcheck.py`, run 8): drizzled median 6.8741e-03 vs master 6.8776e-03 → **level ratio 0.99949** (R-M3-2); non-zero coverage 1.00000; the row profile of (drizzle − 2× nearest-upsampled master) folded at the 512-row band period: ptp 5.61e-05 / std 9.96e-06 against a random-period fold 5.99e-05 / 1.12e-05, FFT line at period 512 = 0.37× its neighbourhood → **no band seam**; weight map max 1.0, min 0.0375 (edges), interior zeros 0, interior median 0.945.

## 4. Acceptance table

| Target (spec §13 + plan Task 7) | Result | Verdict |
| ---- | ---- | ---- |
| Drizzled/undrizzled FWHM ratio within ±5 % of the external tool's (same frames, our estimator, FITS path) — mono | ours 0.9254 (run 12; 0.9279 in runs 8/9/11) vs external 0.9259 | **pass** (+0.05 %) |
| — OSC, G and B planes | ours 0.829 / 0.816 (run 12; 0.831 / 0.818 in run 11) vs external 0.747 / 0.741 | **miss** (+11 / +10 %), attributed-to-investigate (finding 8) |
| — OSC, R plane | ours 0.820 vs external 0.954 (their R drizzle plane is an outlier: eccentricity 0.45) | not comparable (finding 4) |
| Drizzle wall time per group ≤ 15 min (this Mac, 16 GB) | run 11 (clean machine): mono 4.2 min (deposit 244 s), OSC 13.5 min (deposit 636 s, 3 planes × 160 frames) | **pass** |
| `rej/` bitmaps written only with drizzle on, removed unless `keepAll` | 208 + 480 = 688 bitmaps per run, 2.24 GB (panel estimate 2.10 GiB); kept with `keepAll` (three runs → 6.28 GB in the working-folder line); removed by the cleanup in §7 | **pass** |
| Output sizes | mono drizzle 415 MB + weight map 415 MB; OSC drizzle 1.25 GB + weight map 1.25 GB | as estimated (3.11 GiB) |
| Level preserved (R-M3-2) | mono drizzled median / master median = 0.99949; OSC 0.99871 / 0.99999 / 0.99996 | **pass** |
| No band seam at the 512-row scheduler period | folded row profile equals a random-period fold (ptp 5.61e-5 vs 5.99e-5; FFT line 0.37× its neighbourhood) | **pass** |
| Coverage | 1.0 on every plane of both groups | **pass** |
| Weight map | max 1.0, min 0.0375 at the edges, no interior zeros, interior median 0.945 | **pass** |
| Master unchanged by the drizzle stage | runs 8/9/11 masters bit-for-bit M2's; run 12 differs by design (new anchor + polynomial distortion) | **pass** |
| Tab: Drizzle row, panel, results lines | §6 | **pass** (estimate constant 1.2 s/plane: measured 1.17 mono, 1.33 OSC) |
| Panel's `DRIZZLE_SECONDS_PER_PLANE_AT_2X = 1.2` | measured 1.17 (mono) / 1.33 (OSC per plane) on a clean machine | keep 1.2 (within 10 %); the OSC figure suggests 1.3 — note only |

## 5. Findings

1. **The FWHM-ratio target was first read as a 16 % miss, and the miss was a measurement artefact.** Measured through `measure_probe`'s new XISF branch, the external mono drizzle read 4.3464 px (ratio 0.8023) against our 4.9791 (0.9279). Three independent checks on the same pixels then showed the two drizzles are the SAME image: (a) radial profiles of the same five bright stars agree within ±0.01 at every radius from 0.2 to 9.8 output px and their core concentrations (r < 3 / r < 19) match to ±2 % (0.303/0.299, 0.286/0.282, 0.344/0.344, 0.382/0.384, 0.488/0.495); (b) background-subtracted second moments on the same stars: σ 1.611/1.632 vs 1.608/1.620 source px, isotropic (y/x 0.98–0.99); (c) background noise: σ/level 0.0111 vs 0.0113, lag-1 autocorrelation 0.753 vs 0.749, lag-2 0.539 vs 0.534. The external drizzle written to float32 FITS from its raw XISF attachment and measured through the FITS path reads **4.9535** — 0.5 % from ours — and the estimator is level-independent (ours scaled to their level: 4.9791 unchanged). The XISF branch (`astroimage::ImageConverter::read_raw`) disagrees with the raw attachment on every file we tried (mono master 2.7086 vs 2.6749; RGB master 2.79/2.69/2.63 vs 2.70/2.77/2.75) and is not trustworthy for this comparison — **M4 item**: fix or remove `measure_probe`'s XISF branch (select the integration `<Image>` explicitly, verify the plane layout against the raw attachment); every number in this note comes from the FITS path.
2. **The OSC group's drizzle was refused on run 8** (native 6248×4176 frames on the 6224×4168 reference): the driver validated each source plane against the reference geometry. The whole-branch review found it before the run reached the OSC group; the final fix wave separates source and reference geometry (per-frame source extent for the window and the pixel loop; reference extent for the output, the bitmaps and the LN grids) and pins it with a mixed-geometry composite test. Re-run in §7.
3. **`Output` timing double-counts drizzle** (run 8: output 4.75 min ≈ drizzle 4.74 + a few seconds) — the whole-branch review's I2; fixed in the final wave, the timings in §7 are from the fixed build.
4. **The external R-plane drizzle is broad and elongated** (5.16 px, eccentricity 0.45) where its own G/B planes read 4.14/4.07 and 0.32 — a property of the external file, not a target we can meet per plane; the OSC comparison in §4 uses the G and B planes and reports R separately.
5. **Drizzle time**: mono 4.7 min for 208 frames (deposit 233 s ≈ 1.1 s per frame for 26 Mpx at 2×, read 7 s) — the panel's provisional `1.2 s per plane at 2×` is right for mono; the OSC figure (3 planes, 160 frames) comes from §7.
6. **Rejection strength is not a drizzle lever** (run 9 vs run 8: 4.983 vs 4.979) — recorded so the M4 dispersion item is not conflated with drizzle quality.
7. **The owner's report on the OSC master ("brighter edges, other channel levels than the external tool") — not LN, the normalization reference.** Calibration was verified identical frame-for-frame on both nights (R/G/B edge profiles and levels agree to the third decimal with the external tool's calibrated frames); the mono master's large-scale shape equals the external's (columns/rows/corners to 0.003); 1024² crops of the OSC masters are indistinguishable on the same stretch. The difference is which frame anchors normalization: ours was the top-PSF-weight OSC member `2025-09-14_02-19-02_0019` (a bright-sky night, G background 0.0083, R corners 0.83 of centre), the external tool normalized to a dark-sky 2025-10-18 frame (G ≈ 0.0028, its LN reference's R corners 0.69) — the master inherits its reference's level and background shape (run 10 with LN off: same level, R corners 0.79–0.82 → LN is not the cause). Per-frame weights against the external log: mono Spearman 0.924 (top-20 overlap 18/20), OSC 0.768 (overlap 1/20) with the 09-14/10-18 night order inverted (ours 0.80/0.71, theirs 0.73/0.82) — a near tie (FWHM 1.7 vs 2.5 px against a 2.4× brighter sky) decided oppositely. Fix in this branch (Task 8, ruling R-M3-17): the normalization anchor and the LN-reference members are ordered by the sky-penalized score `w / sqrt(background)` over candidates that cover ≥ 97 % of the reference frame (a 32×32 inverse-mapped grid) — identical to the external tool's highest-weight rule on single-night data, dark-sky-first on mixed sets; the set's reference stays the registration reference. **M4 items:** the PSF Signal Weight sky penalty on OSC (audit against the external per-frame weights) and a two-pass registration-reference pick when the best frame's rotation/offset deviates from the set's median.
8. **The OSC drizzle is slightly blurred relative to the external one** (run 11: FWHM ratios [0.821, 0.831, 0.818] vs the external [0.954 (R outlier), 0.747, 0.741] — G/B +11/+10 %). Masters have equal absolute noise (3.5e-05 both), the drizzles do not: ours 4.6e-05 vs theirs 5.8e-05 with a higher lag-1 autocorrelation (0.750 vs 0.644; mono was 0.753 vs 0.749) — the signature of sub-pixel smearing. The OSC frames register onto the mono reference across two optical trains with a homography and distortion OFF; the external run enabled local distortion / surface splines. Run 12 (§7) repeated the set with `registration.distortion = polynomial3`: the OSC ratios did not move (0.820 / 0.829 / 0.816 vs 0.821 / 0.831 / 0.818) — **registration distortion is not the cause**. Ruled out so far: rejection strength (run 9), level conventions (level-matched re-measurement), registration distortion (run 12). What is measured: our OSC drizzle has 26 % less pixel-scale noise and a higher lag-1 autocorrelation than the external one at equal master noise, while the half-maximum radii of the same bright stars are 4 % SMALLER in ours — the fitted-FWHM difference lives in the faint-star population the estimator uses. **M4 item** (with the OSC weight audit, which changes which frames dominate the OSC stack): compare the two OSC drizzles star by star across magnitudes, and drizzle a common frame subset with both weight sets.
9. **The `Output` stage timing** in run 8/9 (4.75 min) double-counted the drizzle (whole-branch review I2); from run 11 on it reads the write time only (≈ 0.4 s → 0.00 min at two decimals).


## 6. Click-through

- Shot 01 (during run 8, ≈ 1250 CSS px on the owner's Windows Chrome over the LAN): the board's Drizzle row reads `2× · square kernel · drop 0.90` with its toggle on and `Queued`; the toolbar wraps cleanly (the M2 final-wave A4 fix verified). The Drizzle panel: scale 2×, drop shrink 0.9, kernel square, the four toggles on, the defaults line, and the estimate line `Bitmaps ≈ 2.10 GB (temporary) · output ≈ 3.11 GB · ≈ 13.8 min` — bitmaps 2.25e9 B (2.09 GiB), output 3.33e9 B (3.10 GiB), time (208 + 480) × 1.2 s = 13.8 min: all three reproduce the helper's formula by hand; every control disabled while the run is in progress.
- Results card (run 11, read through the page's accessible text — the owner's Windows Chrome renderer refused screenshots after the notifications dialog): the runs dropdown lists #1…#11; the notifications panel shows the new `· drizzled` suffix on run-complete entries; the mono card reads `Drizzle 2×: …_6_drizzle2x.fits`, `FWHM 4.98 px · coverage 100.0%`, `Weight map: …_6_drizzle2x_weight.fits` under the LN reference line; the OSC card `FWHM 4.62 / 4.56 / 4.27 px · coverage 100.0% / 100.0% / 100.0%`; the working-folder line itemizes `6.28 GB rejection bitmaps` (three kept runs).

## 7. Re-run and cleanup

- **Run 10** (17:44–17:49Z, from Integrate, LN off, drizzle off): the OSC master's level (0.00277 / 0.00834 / 0.00709) and large-scale shape are those of run 8's (R corners 0.79–0.82 vs 0.83–0.85 with LN) — the reference choice, not LN, sets them (finding 7).
- **Run 11** (16:54–17:17Z, 23.0 min from Integrate, build `a4088d90`): the OSC group drizzles (the whole-branch review's I1 fixed); mono identical to run 8; OSC drizzle 12448 × 8336 × 3, FWHM 4.617 / 4.561 / 4.266, coverage 1.0, ratios 0.821 / 0.831 / 0.818; `Output` timing now 0.00 min (I2 fixed); drizzle 17.75 min (mono 4.2, OSC 13.5) with the machine otherwise idle; the results card shows both drizzle lines and weight maps (§6).
- **Run 12** (17:55–18:48Z, 52.9 min from Register, build `98ed9d85`, `registration.distortion = polynomial3` — the machine shared with the branch's re-gate, so the stage times are inflated: register 1.36, normalize 14.22, integrate 7.30, drizzle 30.0): anchors chosen by the sky-penalized score — mono `2025-10-18_02-18-35_0077` (weight 1.0, background 0.00714), OSC **`2025-10-18_23-38-23_0150`** (weight 0.856, background 0.00214) — the very frame the external tool's LN-reference integration lists first. OSC master `…_osc_180s_160x_7.fits`: level 0.00134 / 0.00290 / 0.00233 (external 0.00125 / 0.00276 / 0.00220), R-plane columns `0.811 0.851 … 1.129 0.873` (external `0.816 0.856 … 1.122 0.881`), corners R 0.66–0.68 (external 0.68–0.70), G 0.94 (0.94–0.95), B 0.95–0.96 (0.95–0.96) — the owner's report is resolved: the OSC master now matches the external one in level and background shape to 1–2 %. Mono master `…_mono_180s_208x_7.fits`: noise 1.857e-05, FWHM 2.688 (its anchor is no longer the pinned `_0073`, which stays the registration reference). Drizzle: mono 4.975 (ratio 0.9254), OSC 4.548 / 4.533 / 4.244 (0.820 / 0.829 / 0.816); no warnings (every OSC frame passed the 97 % coverage filter against the mono reference).
- **Cleanup:** run 12's OSC drizzle checks — level ratios 0.99871 / 0.99999 / 0.99996, non-zero coverage 1.0 on every plane, folded row profiles at 512 rows below a random-period fold (FFT line 1.37 / 1.00 / 0.83× its neighbourhood, i.e. noise), weight maps max 1.0, min 0.10–0.12 at the edges, no interior zeros, interior median 0.94. Then "Delete intermediates" (`cleanup_stacking_work`, `intermediates`) freed **81.1 GB** — calibrated frames, `ln/` and every kept `rej/run-<id>` — leaving `runs/` (3.2 MB) and the output folder's masters, drizzles and weight maps untouched; `rejBytes` reads 0 afterwards.

## 8. Verdict

**Green with one attributed-to-investigate miss.** Drizzle works end to end on both groups of the real set: level-preserving, seam-free, fully covered, with a sane weight map, a scaled WCS and the tab's controls; the mono drizzled/undrizzled FWHM ratio equals the external tool's to 0.05 %. The OSC G/B ratio misses the ±5 % target by ≈ 10 % for a reason that is not registration distortion, rejection strength or level conventions (finding 8) — it goes to M4 together with the OSC weight audit. The run also surfaced and fixed two things outside drizzle: the normalization anchor (Task 8, ruling R-M3-17 — the OSC master now matches the external tool's level and background shape) and `measure_probe`'s XISF branch (finding 1, M4).

## 9. Release-note lines (drafts, English)

- Drizzle: the stacking run can now produce a 1×/2×/3× drizzled master light next to the regular one — square (exact clipping), circle or gaussian drops, using the run's frame weights, per-frame rejection and local normalization; optional weight map; the WCS is scaled with it.
- The Drizzle stage row and its panel in the Stacking tab; the drizzled master, its FWHM, coverage and weight map on the results card; the "Maximum quality" preset now turns on local normalization and 2× drizzle.
