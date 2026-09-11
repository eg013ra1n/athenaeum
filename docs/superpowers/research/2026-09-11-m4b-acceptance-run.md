# Stacking M4b — mixed pixel scales: acceptance run (2026-09-11)

Plan: `docs/superpowers/plans/2026-09-10-stacking-m4b-plan-mixed-pixel-scales.md` (Task 6, rulings R-M4b-1…9 in its header; the execution-time rulings R-T1-1, R-T2-1, R-T3-1…3, R-T6-1…10 are quoted where they bind). Spec: `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §3.8, §15. Build: the M4b branch at `ad152733` (Tasks 1–5 + the acceptance fixes T6-F1 and R-T6-9), the web server on the dev catalog, port 8932; the acceptance drove the app through its own commands only (calibration auto-link, the plate-solve batch, `set_stacking_config`, `start_stacking`).

## 1. Data — three real sets of the owner's catalog (R-M4b-9, R-T6-3, R-T6-5)

The owner's directions: use real frames of the same target at different fields of view (2026-09-11), and keep the sets small — the best frames — so plate-solving and the runs stay short. Selection scripts live in the session scratchpad (`find-mixed-fov.py`, `mixed-fov-detail.py`, `m4b-subsets.py`).

| Set | Case | Groups under test (frames kept of available) | Scales | Ratio |
| ---- | ---- | ---- | ---- | ---- |
| 195 "Ghost Nebula QHY" | WITHIN one group: one camera (QHY268M, bin 1), two focal lengths | `mono__H__bin1__300s`: 30 of 50 at 352 mm + 30 of 68 at 448 mm; `mono__O__bin1__300s`: 30 of 61 + 30 of 60 | 2.226 / 1.731 "/px | ×1.286 |
| 108 "M 78" | CROSS-group binning: ASI294MM Pro at 1000 mm, bin 1 vs bin 2 | `mono__L__bin1__120s`: all 33; `mono__L__bin2__120s`: 27 of 550 (R-T6-8) | 0.480 / 0.960 "/px | ×2.0 |
| 166 (RA 21h08 Dec +68) | CROSS-group, large field ratio: ASI6200MM at 1960 mm vs ASI294MM bin 2 at 347 mm, plus an OSC ASI2600MC Duo at 414 mm | `mono__L__bin1__180s`: 30 of 108; `mono__L__bin2__180s`: 30 of 80; `osc__NoFilter__bin1__180s`: 30 of 398 | 0.393 / 2.749 / 1.887 "/px | ×7.0 and ×4.8 |
| 138 (negative case) | the plan's original ×2.82 set — its 352-mm 20 s groups have no flats at that focal length and no 20 s darks in the catalog (the app's own matcher refuses the 30 s / −10 °C darks) | none — its plan must show the `links` blocker | 0.772 / 2.226 | ×2.9 |

"Best" frames: no `frame_analysis` rows exist for any of these sets, so each subset is the group's frames ranked by the plate solve's `matched_stars` (transparency/focus proxy), ties by `rms_residual_px`; the pipeline's own Measure stage then weighs them. Everything else in a set is excluded through the set's manual exclusion list — the tab's one frame-level write — which is what surfaced T6-F1 (§4). Set 108's Manual reference is pinned in the bin-2 group (frame 100625, 97 matched stars); sets 195 and 166 run Auto with two-pass.

Preparation, all through the app: calibration auto-link on 195 (480 full + 15 partial + 0 unlinked), 138 (93 + 0 + 52 unlinked) and 166 (938 + 214 + 0); plate solves for 1 809 frames in two batches (1 781 solved, 124 failed; 3–7 s per frame, the batch persists at its end). Solves beyond the subsets stay in the catalog.

## 2. Measurement 1 — the header convention holds on real headers (R-M4b-1)

Solve / header-implied scale (`206.2648 · XPIXSZ / FOCALLEN`, no binning factor) per rig over every solved light of the four sets (`m4b-scale-check.py`):

| Rig | n | header "/px | solve median | ratio (min–max) |
| ---- | ---- | ---- | ---- | ---- |
| QHY268M 352 mm | 178 | 2.203 | 2.226 | 1.0105 (1.008–1.015) |
| QHY268M 448 mm | 303 | 1.731 | 1.730 | 0.9991 (0.983–1.000) |
| QHY268M-a137314 352 / 1000 mm | 52 / 92 | 2.203 / 0.776 | 2.226 / 0.772 | 1.0104 / 0.9953 |
| ASI294MM Pro 1000 mm bin 1 / bin 2 (`XPIXSZ` 2.315 / 4.63) | 33 / 545 | 0.478 / 0.955 | 0.480 / 0.960 | 1.0051 / 1.0051 |
| ASI6200MM Pro 1960 mm | 78 | 0.396 | 0.393 | 0.9935 |
| ASI294MM Pro 347 mm bin 2 | 80 | 2.752 | 2.749 | 0.9989 |
| ASI2600MC Duo 414 mm | 389 | 1.873 | 1.887 | 1.0073 |

0 of 1 781 solved frames beyond 2 %. The bin-2 rows are the proof that `XPIXSZ` is the effective, already-binned pixel size — a binning factor would have doubled them.

## 3. Measurement 2 — solve-to-solve jitter and the WCS-seed trigger (R-T2-1 → R-T6-4)

Within one rig |s/median − 1| has p50 ≈ 1e-4–3e-4 but tails of 0.8–1.6 % (LDN 1272: 64/208 + 56/160 frames beyond 1e-3; set 108 bin 2: 107/545; set 195 448 mm: 6 beyond 1e-2; none beyond 5e-2 anywhere). With the Task 2 threshold of 1e-3 a third of the M4a acceptance set would have taken the WCS-seed path against its own reference; ruling R-T6-4 set `WCS_SEED_RATIO_EPS = 0.05` (3× above the measured tail, 5× below the 1.25 gate), and the runs below confirm no same-rig frame takes the seed.

## 4. What the plan gate showed, and the two fixes it forced

- Set 138 (negative case): `links` blocker "52 lights have no calibration links" and the ×2.9 warnings on its three 352-mm groups against the 0.77 "/px largest group — as designed.
- Set 195 / 108 on the Tasks 1–5 build: a `masters` blocker "Build masters first — 1 set cannot be built: fewer than 3 frames" on calibration sets linked ONLY to manually excluded lights (a 1-frame L flat), and — after run 14 — stage 0.5 building a B flat no included frame used, whose matched pre-calibration dark came from the same camera in another ROI (6384×4258 vs 9576×6388) and failed the run. The plan gate and the build list evaluated readiness over the whole set. **T6-F1 (rulings R-T6-6/7):** readiness and `mastersToBuild` over the frames that will actually run; the plan-time scale statistics (group median, the largest-group fallback, `scale_source`) over included members. Both sets then plan clean. The matcher's blindness to frame geometry is recorded in open-items (not M4b's).
- Set 108 still carried one `masters` blocker on a single-frame L flat that IS the matched flat of 3 kept bin-2 frames — a genuine catalog limitation; those 3 frames were excluded (R-T6-8; 27 + 33 = 60 kept).
- Set 195, run 16: the same-rig H-alpha 448-mm frames against the O-filter reference lost 14 of 30 to the quad seed ("only 0 inliers"), on both field rotations, every one with a good plate solve — while their 352-mm siblings, which got a WCS hint (ratio 1.28), registered 21/30. **R-T6-9:** the plate-solve seed is also the fallback after the quad seed fails, regardless of the ratio; the M1 path is untouched for frames the quad seed carries. One H frame had been ALIGNED at rms 44 px and KEPT (`failOnMaxRms` off = "warn, keep", spec §3.6) — recorded in open-items.

## 5. Runs

| Run | Set | Mode | Time | Reference | Result |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 15 | 166 | co-registered | 10.2 min | Auto → OSC 74884 (two-pass switched from 75233) | done, 3 masters in the OSC geometry |
| 16 | 195 | co-registered | 10.9 min | Auto → O 448 mm 25548 (switched from 25480) | done; H 448 mm subset 16/30 (→ R-T6-9) |
| 17 | 108 | co-registered | 9.1 min | Manual bin-2 100625 | done, 2 masters 4144×2822 |
| 18 | 166 | native | 10.4 min | OSC 74884 run-level; per group 74884 / 76261 / 63926 | done, 3 masters in their own geometries |
| 19 | 108 | native | 3.7 min | Manual 100625 run-level and bin-2; bin-1 → 95162 | done, 4144×2822 + 8288×5644 |
| 20 | 195 | co-registered (re-run from Register on the R-T6-9 build) | 5.7 min | Auto → O 448 mm 25548 | done; H 448 mm subset 30/30 (14 through the plate-solve fallback) |
| 21 | 195 | native | 5.8 min | run-level 25548; per group O 25548 / H 25758 | done; H master in its own WCS |

### 5.1 Set 166 — ×7.0 and ×4.8 (runs 15 and 18)

Co-registered (run 15): `mono__L__bin1__180s` (ASI6200MM, 0.393 "/px) registered DOWN onto the OSC reference — 30/30 aligned, 30/30 `homography+wcs`, scale median 0.2071 (0.2060–0.2114; the plan ratio 0.2083 → 0.6 %), rms median 0.31 / max 0.49 px; `mono__L__bin2__180s` (ASI294MM bin 2, 2.749 "/px) registered UP — 30/30, 30/30 `+wcs`, scale 1.4574 (1.4572–1.4575; ratio 1.4568 → 0.04 %), rms 0.56 / 0.61 px; the OSC group 29/30 + the reference at scale 1.0002 with NO seed (same rig, R-T6-4), rms 0.12. Masters: all three 6248×4176 with the reference's WCS (CRPIX 3124.5 = the solve's 0-based 3123.5 + 1; the field rotation of 85.6° puts the 1.887 "/px scale in the off-diagonal CD terms — sqrt|det| = 1.8873"), `ATH_RGEO = 'coRegistered'`, `INSTRUME`/`ATH_STKC` per group; LN 30/30 in every group; rejected 1.07 / 0.78 / 0.63 % (30-frame stacks, not the 368-frame M4a regime). Stage times: masters 0.36, calibrate 1.21, measure 2.65, register 0.56, normalize 3.66, integrate 1.69 min. The register stage with two WCS-seeded groups took 0.56 min for 90 frames — the seed skips the quad search, as the plan's time line expected.

Native (run 18): Calibrate/Measure came from run 15's cache (0.01 / 0.00 min); each group got its own reference and geometry — OSC 74884 (the run-level reference, the largest group — R-M4b-4) at 6248×4176; the 6200MM group 76261 at **9576×6388** (its native full frame, its own solve's CRVAL); the 294MM group 63926 at **4144×2822**; `ATH_RGEO = 'native'`; within-group registration at scale 1.0000, rms 0.09–0.40 px, no `+wcs`; LN 30/30 ×3. Resolution is consistent on the sky between the modes: the 6200MM master reads 6.14 px native (0.393 "/px → 2.41") vs 1.28 px co-registered (1.887 "/px → 2.42"); the 294MM master 2.29 px native (6.3") vs 3.23 px co-registered (6.1") — ruling R-M4b-6 on real data. The OSC master is BIT-IDENTICAL between the two runs (26 091 648 px × 3 planes, max|d| = 0): the reference's own group takes the same path in both modes (R-M4b-5).

### 5.2 Set 108 — bin 1 onto bin 2 (runs 17 and 19)

Co-registered (run 17, Manual bin-2 reference): `mono__L__bin1__120s` 32/33 aligned (one weight-floor exclusion), 32/32 `homography+wcs`, scale median 0.5018 (0.5015–0.5020; plan ratio 0.5005 → 0.26 %, inside the ±1 % target), rms 0.79 / 1.02 px; the bin-2 group 26/27 + the reference at 1.0032 (1.0000–1.0039), rms 0.83, no seed. Both masters 4144×2822 (the targeted geometry), `ATH_RGEO = 'coRegistered'`, LN 32/32 + 27/27. Same-star centroids between the two masters (`m4b-compare.py`, the 50 brightest isolated maxima → 10 usable in this sparse field): |d| median 0.152 px, p90 0.185, max 0.204 — inside the ≤ 0.5 px target across a ×0.5 registration. Level: the bin-1 master reads 0.862 of the bin-2 master (sky percentile 0.855) — the plan's "within 1 %" line was written against the wrong invariant: the two masters are two GROUPS, each normalized to its own sky-penalized anchor (R-M3-17, different nights), and no cross-group level match is promised; R-M4b-6's level guarantee is per frame and pinned in Task 4 (ruling R-T6-10, informational). Stage 0.5 took 6.98 min building only the kept frames' darks and flats (R-T6-6).

Native (run 19, 3.7 min, cached calibrate/measure): the Manual pin stays the run-level AND its own group's reference (R-T3-2); the bin-1 group picked its own reference 95162 and its native **8288×5644** geometry (its own solve), 31/32 at scale 1.0001, rms 0.22 / 0.46; `ATH_RGEO = 'native'`; the bin-2 master is BIT-IDENTICAL to run 17's (11 694 368 px, max|d| = 0). Bin-1 fwhm 5.89 px native (0.48 "/px → 2.83") vs 3.13 px co-registered (0.96 "/px → 3.0").

### 5.3 Set 195 — ×1.286 within one group (runs 16, 20, 21)

Run 16 (Tasks 1–5 build, co-registered): Auto reference = O 448-mm frame 25548 (two-pass switched from 25480). `mono__O__bin1__300s`: the 352-mm subset 22/30 aligned, all `+wcs`, scale median 1.2851 (expected 2.226/1.731 = 1.286), rms 0.60 / 0.64 px; the 448-mm subset 29/30 + the reference at 0.9995 with no seed; 52 included (weight-floor exclusions), LN 52/52, rejected 2.35 %. `mono__H__bin1__300s`: the 352-mm subset 21/30 `+wcs` at 1.2823, rms 0.58; the 448-mm subset 16/30 — 14 quad-seed failures (§4) → R-T6-9. Masters 6252×4176, `ATH_RGEO = 'coRegistered'`, WCS CRPIX 3126.5. Stage 0.5 built the H/O flats and darks (3.19 min).

Run 20 (R-T6-9 build, re-run from Register, 5.7 min): the same-rig H 448-mm subset is now **30/30 aligned — 14 through the plate-solve fallback (`homography+wcs`), 16 through quads as before**; the H group has 51 included frames (was 37), master `Ghost_Nebula_QHY_H_mono_300s_51x.fits` fwhm 2.43 px; the O group is unchanged (52 included, 22/30 `+wcs` at 1.2851). The one quad-aligned H frame at rms 44 px is still kept — it "succeeded", so the fallback never fired (open-items, §7).

Run 21 (native, 5.8 min): `mono__H__bin1__300s` picked its OWN reference 25758 (an H 448-mm frame): the 352-mm subset 21/30 `+wcs` at 1.2837 (rms 0.60 / 0.62), the 448-mm subset 29/30 + the reference at 1.0003 through quads with rms 0.10 / 0.11 — an H→H registration is clean, which places the run-16 outlier on the H→O quad match; the H master is 6252×4176 with its own WCS (CRVAL 14.8997 vs O's 14.9253), `ATH_RGEO = 'native'`, 51 included, LN 51/51, rejected 1.90 %. The run-level reference stays 25548 (the largest group's, O — R-M4b-4). The O master is BIT-IDENTICAL between runs 20 and 21 (26 108 352 px, max|d| = 0) — as on the other two sets, the reference's own group does not see the mode.

## 6. Targets

| Target (plan Task 6) | Result |
| ---- | ---- |
| Header-implied vs solved scale within 2 % on every solved frame | PASS — 0 of 1 781 beyond 2 % (§2) |
| Plan warnings: 195 "mixes pixel scales (1.73–2.23)" per narrowband group; 108 "×2.0"/"×0.5" for the bin-1 group; the ×2.8-class warnings on the swapped set; no blocker | PASS — 195: both groups; 108: ×0.5 against the Manual reference; 166: ×0.2 and ×1.5 (the reference in the OSC group); 138 (negative): `links` blocker + ×2.9 warnings |
| Groups table: the Scale column reads the solved value with the `×r` badge | PASS through the plan JSON (`pixelScaleArcsec`, `scaleSource = solve`, `scaleRatioToReference`); the visual click-through is owed to the owner (open-items) |
| Set 195 run A: ≥ 95 % of the 352-mm frames aligned at scale 1.27 ± 1 % with `+wcs`, RMS ≤ 1.0 px | PASS on every 352-mm frame that reached registration: O 22/22 and H 21/21 at 1.285 ± 0.3 % with `+wcs`, rms ≤ 0.64 px (the 8 + 9 others are weight-floor selections, not registration failures); the same-rig 448-mm H subset needed R-T6-9 (run 16: 16/30 → run 20: 30/30) |
| Set 108 run A: bin-1 onto the bin-2 reference at 0.50 ± 1 %, master 4144×2822, level within 1 %, 50 centroids within 0.5 px | PASS on scale (0.5018), geometry and centroids (0.15 px); the level line reinterpreted (R-T6-10) |
| Large-field set run A: the foreign groups onto the reference with `+wcs` on ≥ 95 %, RMS ≤ 1.5 px; masters in one geometry | PASS — 30/30 and 30/30 `+wcs` at ×0.207 and ×1.457, rms ≤ 0.61 px, one geometry |
| Run B (native): each group's master in its own geometry with its own reference, `ATH_RGEO = 'native'`, no cross-group registration | PASS on all three sets (166: 3 geometries; 108: 4144×2822 + 8288×5644; 195: the H group on its own reference and WCS) |
| Same-scale groups unchanged | PASS — the reference's own group is bit-identical between modes on all three sets; no same-rig frame carries `+wcs` unless the quad seed failed first (R-T6-9) |
| Time: the register stage with a WCS seed ≤ the quad-seed time | PASS — 0.56 min for 90 frames (166), 0.18 min for 60 (108) |

## 7. Residuals and open items

Recorded in `docs/superpowers/open-items.md` → Stacking M4b: the quad seed's failure on an H-alpha field against an O-filter reference of the same rig (now covered by the WCS fallback; the quad matcher's own robustness is an M4c registration item); `failOnMaxRms` off letting a 44 px transform into a stack; the calibration matcher's blindness to frame geometry; the noisy "skipped: fewer than 3 included frames" warning per excluded group; the owed owner smoke and click-throughs; the deferred review minors.
