# Star measurement baseline: the truth lab's scorecard of today's code (2026-10-07)

Part 1 of the star measurement program ends here. Part 1 built a synthetic truth lab, put production's star
measurement through it unchanged, and scored every path against exact truth. This report is that baseline. Part 2's
spec, the stacking PSF work, argues from it and sets its bars from it before any fix is measured.

How to read the numbers:
- **Copied as printed.** Every number is copied from the scorecard, a gate line or a lab table exactly as that file
  printed it. A number this report computes from printed numbers is marked *derived*.
- **Where each number lives.** Every number in sections 1, 3 and 7 is in a committed athenaeum-lab file under
  `star-lab/results/`:
  - `baseline-2026-10-07/scorecard-summary.md` and `scorecard-inject-summary.md`;
  - `baseline-2026-10-07/gates.txt`, `divergent-families.txt` and `erp-ln-estimates.txt`;
  - the `probe-runs-2026-10-07/` tables.

  The full scorecards are kept gzipped outside the repository, with their sha256 in the summaries.
- **The ERP.** "The ERP" is the external reference pipeline, run on the same inputs. It is a comparison column,
  never a referee (spec S-9).
- **Referees.** The synthetic truth is the referee everywhere. On real frames the injected stars are the referee;
  the corpus has none.
- **Truth widths.** The three truth widths (spec S-2) are named as the scorecard names them:
  - `prepixel`: the PSF before pixel integration;
  - `pixbox`: the PSF convolved with the pixel box;
  - `pointfit`: the point-sampled-fit equivalent.

Specs and plan:
- spec `docs/superpowers/specs/2026-10-06-star-truth-lab-design.md`;
- plan `docs/superpowers/plans/2026-10-06-star-truth-lab-plan.md`.

## 1. Verdict

Five defects stand out, largest first.

1. **The LN scale estimator now on by default (F1, common aperture at the growth curve) reads too high on synthetic
   pairs.**
   - Worst target: `pairs/main/m4w10` t60 (a target of FWHM 6 against a reference of FWHM 3) reads `s 1.2302 vs
     s_true 1.1: +11.84 %`.
   - Its slope against the FWHM² difference is `+0.1533 %/px^2` (`pairs/iso/blur25`) to `+0.4021 %/px^2`
     (`pairs/main/m4w10`) on all 19 `pairs` families.
   - The PSF-signal arm with the selection (`psfSel`) reads `s 1.1067 vs s_true 1.1: +0.60 %` on that same target,
     and the ERP reads `s 1.1616 (+5.60 %)`.
   - The ERP's number on t60 rests on `63 valid PSF fits, 16 PSF signal estimates`. Its t45 number rests on `208
     valid PSF fits, 145 PSF signal estimates` (`erp-ln-estimates.txt`).
   - On the isolated `truthcheck` field F1's slope is `+0.0040 %/px^2`; on `density_pairs/d600` t3 it reads `+4.40
     %`. The defect grows with the width ratio and with crowding: neighbours inside F1's `[6F, 8F]` sky annulus
     (section 4.2).
   - **Open.** On real data, against the harness truth, F1 shows no such over-read at comparable width ratios
     (section 4.2). The two referees disagree on F1, and part 1 does not resolve it.
   - It harms the LN normalisation of every member, so the master's photometry and its rejection.
2. **The captured fraction `signal / flux_img` (measure path) depends on the profile.**
   - At F 2.6: `sharp/gauss` reads `median 0.9078 · scatter 0.0216` and `sharp/m2.5` reads `median 0.7646 · scatter
     0.0161`, a derived spread of 18.73 % from the printed medians. At F 8.0 the derived spread is 22.16 %.
   - It harms the default `PsfSignalWeight`, whose referee is total flux (S-2).
3. **The production VNG debayer distorts per-plane star widths.**
   - `osc/f2.0/m4` plane 1 reads `bias +12.63 %` on the VNG plane against `ideal plane: bias +3.06 % (n 419)`.
   - `osc/f2.6/m4` plane 2 reads `bias -6.71 %` against `ideal plane: bias +0.08 % (n 391)`.
   - The injected real mosaics repeat it on plane 2:
     - set 204's mosaic: `bias -5.85 % · scatter 4.15 %`;
     - the ASI2600MC Pro raw lights: `bias -6.29 % · scatter 5.90 %`.
   - It harms the OSC FWHM filters, the per-plane weights, the master FWHM and F1's common F per plane.
4. **The frame FWHM moves with signal level, and Auto β flips with it.**
   - Between an image and its 2× flux twin (`base vs deep (2x flux)`):
     - `twins/sharp/m4/f2.0` measure: `median 9.99 % · p90 9.99 %`, with Auto β agreeing `0.0 % of 1 pairs`;
     - `twins/sharp/m2.5/f1.6`: `median 15.93 % · p90 15.93 %`;
     - `twins/sharp/m4/f2.6`, where β agrees: still `median 2.93 % · p90 2.93 %`.
   - Pure noise twins of the same frames read `median 0.01 % · p90 0.01 %` (`twins/sharp/m4/f2.6`).
   - It harms the Formula weights, the FWHM filters and the master FWHM's stability.
5. **The Analysis path discards the brightest stars of sharp frames, and its round-star floor is twice the measure
   path's.**
   - Bright coverage (the 300 brightest unsaturated truth stars):
     - `sharp/m4/f2.0` analysis reads `0.0 % of 300` and `sharp/m4/f2.6` reads `7.3 % of 300`;
     - the measure path reads `100.0 % of 300` on both.
   - At truth e 0, `sharp/m4/f2.6` analysis reads `median e 0.2666 · scatter 0.1202`, measure `median e 0.1202 ·
     scatter 0.0784`, the ERP `Moffat: median e 0.1411 · scatter 0.0577`.
   - It harms the Analysis tab (part 3).

The gates: `13 PASS, 0 FAIL, 0 SKIP`. Every lab gate ran in one uniform run at lab `2ce9f16`, with nothing skipped.
`erp_emulation` is no longer SKIP (section 2).

## 2. Method

### Code and binaries

- athenaeum `star-truth-lab` at f5321979 (`f53219795654b810d70d24f6e6c09889d5d46faa`). Tracked files are clean.
  Submodules: rustafits `8321412`, solvemyastro `c1c633c`. Part 1's production diff is the two items the plan names:
  the `measure_plane_detailed` extraction (P-2) and `stars_from_fits` made `pub`. `star_probe` is a new example.
- athenaeum-lab `main` at 29af8d2 for the results (the summaries regenerated from the same gzipped scorecards after the review); gates code at `2ce9f16`.
- **The probe binaries, both release examples built from f5321979 under `.superpowers/target-acc`:**
  - `star_probe` sha256 `ef9be3c0715f030c7ee00c4a75d25ea60039f67523c3af3df668cd44a74e7d84`;
  - `ln_scale_probe` sha256 `f738d56ce50ff96d09032c1cfc29c7e19f657b2bf5790df714b6f580804be270`.
- **The one-binary proof.** Every probe output's run record carries the sha256 of the binary that wrote it, and its
  command equals the planned one. `--verify` proves:

  | Output set | Proven |
  | ---- | ---- |
  | v2 | `673 planned, 673 proven, 0 not proven` |
  | injections | `90 planned, 90 proven, 0 not proven` |
  | star-test corpus | `280 planned, 280 proven, 0 not proven` |
  | real-data LN | `23 planned, 23 proven, 0 not proven` |

  File: athenaeum-lab `star-lab/results/probe-runs-2026-10-07/provenance.txt`.

### Families

- `manifests/v2.json`: 164 families, 417 images, generated with numpy 2.4.6.
- The scorecard scores 117 families and 269 images, with `0 missing output(s)`. Ladder frames and pair members are
  scored per image; aliases and the Task 16b `pairs` grids' own truth columns are noted in the summary.
- `twins/*` families serve row 11 only.

### Injections

`manifests/inject.json`: 22 injected, 26 refused.

Injected:
- ASI6200MM calibrated ×6: Horse C11 G and L, 3 each;
- QHY268M set 107 calibrated ×3, plus one mono LN run pair;
- ASI2600MC Duo calibrated mosaics ×3 (set 204), plus one OSC LN pair;
- QHY268M raw ×3 (m42, 2025-10-26), plus the Duo's b150 raw;
- ASI2600MC Pro raw ×3, plus one OSC run pair.

Refused, each with its reason in the manifest:

| Group | Count | Reason |
| ---- | ---- | ---- |
| ATR2600M set 204 mono | 3 calibrated + 1 pair | gain self-check ratio 1.459–1.586 (the dark offset mismatch) |
| ATR2600M run B mono | 3 calibrated + 1 pair | 1.344–1.600 |
| QHY268M set 173 | 3 calibrated + 1 pair | 1.109–1.167 |
| ASI294MM | 3 calibrated + 1 pair | 1.136–1.153, with the owner-ruled PTC gain |
| run B OSC | 3 prepared frames | 3-D |
| run B OSC | 1 pair | raw light not found |
| ASI2600MC Pro | 3 calibrated frames | 3-D (the reused config keeps only debayered planes; the Duo covers the calibrated-mosaic kind) |
| singles: `2025-08-23 … B …0012` | 1 | gain 1.121 |
| singles: Gamma Scuti (ASI2600MC Air) and `ldn1272.fits` | 2 | no pedestal source |

No Horse C11 G pair was made. Its LN reference and saturation map are on disk in `hc11B-f1`, but the catalog holds
no `ln_reference` row (Task 7). A pair can be made on them; part 1 did not.

### The ERP batch

- `erp_v2.py stars manifests/v2.json --force` with `Moffat4,Gaussian,Moffat,MoffatA`, then `erp_v2.py ln
  manifests/v2.json --force`. The ERP LN on `osc_pairs` runs on the probe's own VNG planes.
- The batch ran split by family group so that the data root's size could be checked between groups.
- 57 ERP launches, 0 failed, 0 refused; wall 376.36 s, peak RSS 2.54 GB.
- `erp_emulation` then reads `PASS: erplib (Moffat4, from the ERP's per-star dumps) vs the ERP's own LN scale (ln.csv) on 105 pairs targets in 19 groups: max |diff| 0.0808 % (main/m4/e7t60), median |diff| 0.0048 %, 0 over the bar 0.1 %`.
- The comparison body was written in Task 11. It compares the lab's emulation of the ERP LN (from the ERP's own
  per-star fits) against the ERP's own LN scale.
- **A guard added after the run.** After the review (lab 29af8d2), the gate also refuses:
  - a dump whose detection parameters differ from the LN batch's (`erp_v2.STAR_JOB`);
  - an image whose sha256 differs from the manifest's.

  The final `gates.txt` ran before this guard was added. The pairing it compared was verified by review: the same
  image files, the same detection parameters, and labels paired by file stem.

### The scorecards

The full scorecards live outside the repository, gzipped, with their sha256 in the committed summaries:

| Scorecard | Rows | Built in | Committed summary |
| ---- | ---- | ---- | ---- |
| v2 | 49529 | 437.5 s | `star-lab/results/baseline-2026-10-07/scorecard-summary.md` |
| injections | 2660 | 15.8 s | `star-lab/results/baseline-2026-10-07/scorecard-inject-summary.md` |

- The probe-gap row "converted ... as ADU" is absent from both (P-28).
- The per-row lists of families beyond 2 × their own scatter, or worse than the ERP, are in
  `star-lab/results/baseline-2026-10-07/divergent-families.txt` (`bin/baseline_flags.py`).

### The gates

One full uniform run of `bin/gates.py`, at lab `2ce9f16`. File:
`star-lab/results/baseline-2026-10-07/gates.txt`.

| gate | verdict and numbers, as printed |
| ---- | ---- |
| pixel_integration | PASS: 9 test(s) ok (tests.test_render)  [run 2026-10-07T15:43:48, wall 0s, peak RSS 276 MB] |
| mosaic | PASS: 1 test(s) ok (tests.test_osc.TestOsc.test_every_cfa_pixel_equals_its_channel)  [run 2026-10-07T15:43:48, wall 0s, peak RSS 36 MB] |
| injection | PASS: 22 of 22 injected entries bit-identical outside the stamps ({'calibrated': 9, 'mosaic': 3, 'pair': 3, 'raw': 7}); 26 refused, listed  [run 2026-10-07T15:43:48, wall 16s] |
| gain | PASS: 22 of 22 injected entries re-read in [0.9, 1.1] with their recorded mode, a in [0.912, 1.080]; 26 refused (listed, neither pass nor fail)  [run 2026-10-07T15:44:05, wall 0s] |
| determinism | PASS: 164 of 164 families regenerate their first image (hash equal or max \|delta\| <= 1e-7 with the same truth)  [run 2026-10-07T15:44:05, wall 2051s, peak RSS 2034 MB] |
| crlb | PASS: 7 test(s) ok (tests.test_crlb)  [run 2026-10-07T16:18:16, wall 0s, peak RSS 35 MB] |
| matcher | PASS: 44 sharp images, 27676 truth stars matched against themselves: 100 %, zero unmatched  [run 2026-10-07T16:18:16, wall 0s] |
| truth_normal | PASS: pooled over K seeded in-memory realizations (inverse-variance over the harness estimator's per-realization ratios); 4 targets, K [80, 140, 230, 320], dropped [0, 0, 0, 0], pooled SE [0.061, 0.059, 0.075, 0.06] % (aim 0.1), max \|gap\| 0.287 % (bar 0.3), slope -0.0063 %/px2 over FWHM2 (bar 0.1), pooled n [4800, 8400, 13800, 19200] (bar 30); t4 within 1 sigma of the bar  [run 2026-10-07T16:18:16, wall 1465s] |
| truth_crowded | PASS: pooled over K seeded in-memory realizations (inverse-variance over the harness estimator's per-realization ratios), CROWDED-MODE units only; 3 targets, K [10, 10, 60], crowded-mode units [5, 10, 5], dropped [0, 0, 55], pooled SE [0.097, 0.118, 0.394] % (aim 0.27, NOT met everywhere), gaps -0.045, +0.154, +0.594 % (bar +-0.8), pooled n [279, 216, 56] (bar 30); mixed modes at t1 (5 crowded, 5 normal units); t3 within 1 sigma of the bar; recorded as Task 11's error bar  [run 2026-10-07T16:42:42, wall 842s] |
| erp_emulation | PASS: erplib (Moffat4, from the ERP's per-star dumps) vs the ERP's own LN scale (ln.csv) on 105 pairs targets in 19 groups: max \|diff\| 0.0808 % (main/m4/e7t60), median \|diff\| 0.0048 %, 0 over the bar 0.1 %  [run 2026-10-07T16:56:44, wall 2s] |
| probe_build | PASS: cargo check -p athenaeum-core --examples exit 0  [run 2026-10-07T16:56:45, wall 1s, peak RSS 165 MB] |
| probe_parity | PASS: 5875 fits (measure) vs 5875 (artifact); 58750 persisted fields (x, y, background, amplitude, signal, fwhmX, fwhmY, thetaDeg, beta, residual) equal bit for bit after ONE f32 narrowing of the probe value: 0 mismatches, 0 fields skipped (absent from the probe line) (not compared: peak5 not persisted, fwhm and ecc derived, fwtm and area not dumped by the probe: a probe gap); channel 22 of 22 fields exactly equal; run config: set 204's only finished run (id 5), because no run carries the artifact's per-stage config_hash 4e4eb9915a9c5a93; confirmed by the exact channel match  [run 2026-10-07T16:56:46, wall 1s, peak RSS 696 MB] |
| seed_parity | PASS: pin passed (seeds.len() == stars_detected); 416 probe seeds brightest-first by flux: True; counts equal: True. A star-for-star external comparison is not available without a product change  [run 2026-10-07T16:56:47, wall 1s, peak RSS 163 MB] |

## 3. Rows 1–11

Each table gives the scorecard's own cells for a fixed set of families, with bins pooled. The full set is in the
summary.

A count of "divergent" family-planes lists those with:
- **A:** |bias| > 2 × their own scatter;
- **E:** a reading worse than the ERP on the same quantity.

Production paths only. Top lines are verbatim from `divergent-families.txt`.

### Row 1: detection

| family | path | completeness (own threshold) | bright coverage (300) | ERP completeness |
| ---- | ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | 100.0 % of 407 expected · raw 74.2 % of 629 | 100.0 % of 300 | Moffat: 45.2 % of 407 expected · raw 29.3 % of 629 |
| sharp/m4/f1.2 | register | 88.4 % of 493 expected · raw 69.3 % of 629 | 100.0 % of 300 | — |
| sharp/m4/f1.2 | analysis | 16.8 % of 546 expected · raw 14.6 % of 629 | 12.0 % of 300 | Moffat: 33.7 % of 546 expected · raw 29.3 % of 629 |
| sharp/m4/f2.6 | measure | 100.0 % of 364 expected · raw 68.7 % of 629 | 100.0 % of 300 | Moffat: 38.2 % of 364 expected · raw 22.1 % of 629 |
| sharp/m4/f2.6 | register | 80.9 % of 492 expected · raw 63.3 % of 629 | 100.0 % of 300 | — |
| sharp/m4/f2.6 | analysis | 21.5 % of 554 expected · raw 18.9 % of 629 | 7.3 % of 300 | Moffat: 25.1 % of 554 expected · raw 22.1 % of 629 |
| sharp/m4/f3.5 | measure | 99.7 % of 350 expected · raw 66.1 % of 629 | 100.0 % of 300 | Moffat: 42.3 % of 350 expected · raw 23.5 % of 629 |
| sharp/m4/f3.5 | register | 77.1 % of 494 expected · raw 60.6 % of 629 | 100.0 % of 300 | — |
| sharp/m4/f3.5 | analysis | 56.8 % of 572 expected · raw 51.7 % of 629 | 94.7 % of 300 | Moffat: 25.9 % of 572 expected · raw 23.5 % of 629 |
| sharp/m4/f8.0 | measure | 82.7 % of 301 expected · raw 47.2 % of 629 | 91.3 % of 300 | Moffat: 40.2 % of 301 expected · raw 19.2 % of 629 |
| sharp/m4/f8.0 | register | 57.6 % of 510 expected · raw 46.7 % of 629 | 91.3 % of 300 | — |
| sharp/m4/f8.0 | analysis | 36.3 % of 564 expected · raw 32.6 % of 629 | 68.3 % of 300 | Moffat: 21.5 % of 564 expected · raw 19.2 % of 629 |
| sharp/gauss/f2.6 | measure | 100.0 % of 375 expected · raw 70.0 % of 629 | 100.0 % of 300 | n/a (ERP free-β Moffat not run) |
| sharp/gauss/f2.6 | register | 79.8 % of 510 expected · raw 64.7 % of 629 | 100.0 % of 300 | — |
| sharp/gauss/f2.6 | analysis | 19.3 % of 575 expected · raw 17.6 % of 629 | 2.0 % of 300 | n/a (ERP free-β Moffat not run) |
| density/d1000 | measure | 70.4 % of 2392 expected · raw 45.4 % of 4194 · blends 7 | 73.3 % of 300 | Moffat: 19.6 % of 2392 expected · raw 11.2 % of 4194 · blends 2 |
| density/d1000 | register | 51.3 % of 3316 expected · raw 40.6 % of 4194 · blends 3 | 72.7 % of 300 | — |
| density/d1000 | analysis | 12.5 % of 3752 expected · raw 11.2 % of 4194 · blends 2 | 0.0 % of 300 | Moffat: 12.5 % of 3752 expected · raw 11.2 % of 4194 · blends 2 |
| saturated/f2.6/n60 | measure | 100.0 % of 769 expected · raw 71.1 % of 1258 | 100.0 % of 600 | Moffat: 64.0 % of 769 expected · raw 39.1 % of 1258 |
| saturated/f2.6/n60 | register | 76.5 % of 1009 expected · raw 61.4 % of 1258 | 100.0 % of 600 | — |
| saturated/f2.6/n60 | analysis | 22.8 % of 1129 expected · raw 20.4 % of 1258 | 11.7 % of 600 | Moffat: 43.6 % of 1129 expected · raw 39.1 % of 1258 |
| trails | measure | 66.4 % of 149 expected · raw 32.0 % of 350 | 37.3 % of 300 | n/a (ERP free-β Moffat not run) |
| trails | register | 12.3 % of 243 expected · raw 8.6 % of 350 | 10.0 % of 300 | — |
| trails | analysis | 24.7 % of 283 expected · raw 20.0 % of 350 | 23.3 % of 300 | n/a (ERP free-β Moffat not run) |

Paths have different denominators. `measure` counts truth stars with true peak ≥ its `detectionSigma` (`peak_snr >=
20`); `register` counts Fisher SNR ≥ 10; `analysis` counts a matched-filter response ≥ 5 σ (`fisher_snr >= 5`). A
shortfall against the path's own threshold is a defect.
- The measure path holds its threshold on sharp frames.
- On `sharp/m4/f8.0` it reads `82.7 % of 301 expected` against the seeds' `92.0 % of 301 expected`, so the fit
  rejects broad stars the detector found.
- On crowded fields it reads `70.4 % of 2392 expected` (`density/d1000`).
- On `trails` it reads `66.4 % of 149 expected`, and register reads `12.3 % of 243 expected`.

Divergent (E, completeness below the ERP's on the same denominator): analysis, 33 family-planes. The worst is
`saturated/f2.6/n5` analysis, `18.7 % of 555 expected` against the ERP's `51.4 % of 555 expected`. No measure,
register or seeds line is below the ERP.

False detections per Mpx are non-zero on the `artefacts`, `density`, `density_pairs` and `doubles` fields and on a
few OSC, broad and `trails` images (the full list is in the summary). The largest are:
- the hot pixels and cosmic rays of `artefacts`: measure `hot 2.86 · cr 9.06`;
- the star wings of the densest field: `density/d1000` seeds `star_wing 28.13`, measure `star_wing 2.62`.

### Row 2: centroid

| family | path | ours | efficiency and CRLB | ERP |
| ---- | ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | seeds | bias (-0.0002, -0.0004) px · scatter (0.0129, 0.0134) | eff (2.34, 2.04) (n 180) · CRLB (0.0194, 0.0193) | — |
| sharp/m4/f1.2 | measure | bias (+0.0006, -0.0000) px · scatter (0.0082, 0.0083) | eff (1.00, 1.21) (n 180) · CRLB (0.0194, 0.0193) | Moffat: bias (+0.0012, -0.0005) px · scatter (0.0067, 0.0064) |
| sharp/m4/f1.2 | register | bias (+0.0006, -0.0000) px · scatter (0.0082, 0.0083) | eff (1.00, 1.21) (n 180) · CRLB (0.0194, 0.0193) | — |
| sharp/m4/f1.2 | analysis | bias (+0.0028, +0.0008) px · scatter (0.0185, 0.0164) | eff (0.97, 1.02) (n 59) · CRLB (0.0173, 0.0172) | Moffat: bias (+0.0012, -0.0005) px · scatter (0.0067, 0.0064) |
| sharp/m4/f2.6 | seeds | bias (+0.0002, -0.0024) px · scatter (0.0164, 0.0186) | eff (1.83, 1.95) (n 205) · CRLB (0.0169, 0.0169) | — |
| sharp/m4/f2.6 | measure | bias (-0.0008, -0.0006) px · scatter (0.0092, 0.0075) | eff (1.03, 0.93) (n 205) · CRLB (0.0169, 0.0169) | Moffat: bias (+0.0010, -0.0015) px · scatter (0.0081, 0.0067) |
| sharp/m4/f2.6 | register | bias (-0.0007, -0.0006) px · scatter (0.0089, 0.0075) | eff (1.03, 0.91) (n 203) · CRLB (0.0168, 0.0168) | — |
| sharp/m4/f2.6 | analysis | bias (-0.0103, +0.0001) px · scatter (0.0419, 0.0383) | eff (0.90, 1.00) (n 84) · CRLB (0.0503, 0.0503) | Moffat: bias (+0.0010, -0.0015) px · scatter (0.0081, 0.0067) |
| sharp/m4/f8.0 | seeds | bias (-0.0004, -0.0689) px · scatter (0.0258, 0.1368) | eff (2.10, 3.70) (n 117) · CRLB (0.0202, 0.0202) | — |
| sharp/m4/f8.0 | measure | bias (-0.0003, +0.0001) px · scatter (0.0117, 0.0117) | eff (0.89, 1.16) (n 117) · CRLB (0.0202, 0.0202) | Moffat: bias (-0.0000, +0.0001) px · scatter (0.0072, 0.0078) |
| sharp/m4/f8.0 | register | bias (-0.0002, +0.0001) px · scatter (0.0117, 0.0114) | eff (0.90, 1.18) (n 114) · CRLB (0.0197, 0.0197) | — |
| sharp/m4/f8.0 | analysis | bias (-0.0008, +0.0001) px · scatter (0.0079, 0.0077) | eff (0.89, 1.23) (n 66) · CRLB (0.0090, 0.0090) | Moffat: bias (-0.0000, +0.0001) px · scatter (0.0072, 0.0078) |
| density/d600 | seeds | bias (-0.0017, -0.0025) px · scatter (0.0245, 0.0248) | eff (2.39, 1.99) (n 154) · CRLB (0.0142, 0.0142) | — |
| density/d600 | measure | bias (-0.0000, -0.0001) px · scatter (0.0091, 0.0084) | eff (1.12, 0.98) (n 155) · CRLB (0.0140, 0.0140) | Moffat: bias (+0.0001, -0.0003) px · scatter (0.0073, 0.0073) |
| density/d600 | register | bias (-0.0000, -0.0001) px · scatter (0.0089, 0.0083) | eff (1.09, 0.98) (n 151) · CRLB (0.0127, 0.0127) | — |
| density/d600 | analysis | bias (-0.0048, -0.0025) px · scatter (0.0726, 0.0745) | eff (1.24, 1.01) (n 67) · CRLB (0.0730, 0.0730) | Moffat: bias (+0.0001, -0.0003) px · scatter (0.0073, 0.0073) |
| saturated/f2.6/n60 | seeds | bias (-0.0017, -0.0032) px · scatter (0.0229, 0.0225) | eff (2.21, 2.02) (n 359) · CRLB (0.0202, 0.0202) | — |
| saturated/f2.6/n60 | measure | bias (+0.0001, +0.0003) px · scatter (0.0098, 0.0100) | eff (0.99, 1.07) (n 359) · CRLB (0.0202, 0.0202) | Moffat: bias (-0.0001, +0.0003) px · scatter (0.0062, 0.0062) |
| saturated/f2.6/n60 | register | bias (+0.0001, +0.0004) px · scatter (0.0097, 0.0100) | eff (0.99, 1.09) (n 356) · CRLB (0.0200, 0.0200) | — |
| saturated/f2.6/n60 | analysis | bias (+0.0005, -0.0002) px · scatter (0.0390, 0.0406) | eff (0.99, 1.10) (n 169) · CRLB (0.0509, 0.0509) | Moffat: bias (-0.0001, +0.0003) px · scatter (0.0062, 0.0062) |
| resampled/f2.0 | seeds | bias (-0.0006, -0.0018) px · scatter (0.0157, 0.0167) | eff (1.86, 2.01) (n 239) · CRLB (0.0184, 0.0184) | — |
| resampled/f2.0 | measure | bias (+0.0002, -0.0002) px · scatter (0.0072, 0.0071) | eff (0.99, 1.00) (n 239) · CRLB (0.0184, 0.0184) | Moffat: bias (+0.0001, -0.0003) px · scatter (0.0047, 0.0048) |
| resampled/f2.0 | register | bias (+0.0002, -0.0002) px · scatter (0.0072, 0.0071) | eff (0.99, 1.02) (n 237) · CRLB (0.0183, 0.0184) | — |

- The measure and register paths' efficiency against the CRLB is 0.89–1.21 in every cell above.
- The seeds read near 2, for example `eff (1.83, 1.95)` on `sharp/m4/f2.6`.
- The analysis path's scatter is larger by a factor shown in its own row, for example `scatter (0.0419, 0.0383)`
  against measure's `(0.0092, 0.0075)` on `sharp/m4/f2.6`.

Divergent (A): 0 family-planes on any production path.

### Row 3: FWHM

| family | path | prepixel | pixbox | pointfit | ERP (pointfit) |
| ---- | ---- | ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | bias +19.66 % · scatter 1.03 % | bias -2.24 % · scatter 0.84 % | bias +4.65 % · scatter 0.90 % | Moffat: [pointfit] bias +4.09 % · scatter 1.09 % |
| sharp/m4/f1.2 | analysis | bias +19.31 % · scatter 2.51 % | bias -2.52 % · scatter 2.05 % | bias +4.34 % · scatter 2.20 % | Moffat: [pointfit] bias +4.09 % · scatter 1.09 % |
| sharp/m4/f2.0 | measure | bias +6.24 % · scatter 0.64 % | bias -1.77 % · scatter 0.59 % | bias +0.10 % · scatter 0.60 % | Moffat: [pointfit] bias +1.29 % · scatter 0.84 % |
| sharp/m4/f2.0 | analysis | bias +7.40 % · scatter 5.21 % | bias -0.70 % · scatter 4.82 % | bias +1.19 % · scatter 4.91 % | Moffat: [pointfit] bias +1.29 % · scatter 0.84 % |
| sharp/m4/f2.6 | measure | bias +3.77 % · scatter 0.60 % | bias -1.02 % · scatter 0.57 % | bias +0.09 % · scatter 0.58 % | Moffat: [pointfit] bias +0.86 % · scatter 0.82 % |
| sharp/m4/f2.6 | analysis | bias +3.48 % · scatter 3.12 % | bias -1.30 % · scatter 2.98 % | bias -0.19 % · scatter 3.01 % | Moffat: [pointfit] bias +0.86 % · scatter 0.82 % |
| sharp/m4/f5.0 | measure | bias +1.02 % · scatter 0.38 % | bias -0.28 % · scatter 0.38 % | bias +0.03 % · scatter 0.38 % | Moffat: [pointfit] bias +0.22 % · scatter 0.38 % |
| sharp/m4/f5.0 | analysis | bias +1.25 % · scatter 0.27 % | bias -0.06 % · scatter 0.26 % | bias +0.25 % · scatter 0.26 % | Moffat: [pointfit] bias +0.22 % · scatter 0.38 % |
| sharp/m4/f8.0 | measure | bias +0.41 % · scatter 0.29 % | bias -0.10 % · scatter 0.29 % | bias +0.02 % · scatter 0.29 % | Moffat: [pointfit] bias +0.12 % · scatter 0.28 % |
| sharp/m4/f8.0 | analysis | bias +0.49 % · scatter 0.22 % | bias -0.02 % · scatter 0.22 % | bias +0.10 % · scatter 0.22 % | Moffat: [pointfit] bias +0.12 % · scatter 0.28 % |
| sharp/m2.5/f2.6 | measure | bias +4.03 % · scatter 0.56 % | bias -1.55 % · scatter 0.53 % | bias +0.18 % · scatter 0.54 % | Moffat: [pointfit] bias +1.32 % · scatter 0.72 % |
| sharp/m2.5/f2.6 | analysis | bias +5.02 % · scatter 2.72 % | bias -0.62 % · scatter 2.58 % | bias +1.13 % · scatter 2.62 % | Moffat: [pointfit] bias +1.32 % · scatter 0.72 % |
| sharp/m10/f2.6 | measure | bias +3.56 % · scatter 0.51 % | bias -0.44 % · scatter 0.49 % | bias +0.04 % · scatter 0.49 % | Moffat: [pointfit] bias -0.49 % · scatter 0.74 % |
| sharp/m10/f2.6 | analysis | bias +3.94 % · scatter 3.89 % | bias -0.07 % · scatter 3.74 % | bias +0.41 % · scatter 3.76 % | Moffat: [pointfit] bias -0.49 % · scatter 0.74 % |
| sharp/gauss/f2.6 | measure | bias +0.31 % · scatter 0.52 % | bias -3.05 % · scatter 0.50 % | bias -3.01 % · scatter 0.50 % | n/a (ERP free-β Moffat not run) |
| sharp/gauss/f2.6 | analysis | bias +2.92 % · scatter 3.37 % | bias -0.54 % · scatter 3.26 % | bias -0.49 % · scatter 3.26 % | n/a (ERP free-β Moffat not run) |
| sharp/gauss/f8.0 | measure | bias -2.23 % · scatter 0.36 % | bias -2.58 % · scatter 0.36 % | bias -2.58 % · scatter 0.36 % | n/a (ERP free-β Moffat not run) |
| sharp/gauss/f8.0 | analysis | bias -4.61 % · scatter 0.16 % | bias -4.95 % · scatter 0.16 % | bias -4.95 % · scatter 0.16 % | n/a (ERP free-β Moffat not run) |
| sharp/blur2.5/f2.6 | measure | bias +0.63 % · scatter 0.56 % | bias -2.60 % · scatter 0.54 % | bias -2.50 % · scatter 0.54 % | Moffat: [pointfit] bias -3.11 % · scatter 0.00 % |
| sharp/blur2.5/f2.6 | analysis | bias +2.26 % · scatter 4.05 % | bias -1.02 % · scatter 3.92 % | bias -0.92 % · scatter 3.92 % | Moffat: [pointfit] bias -3.11 % · scatter 0.00 % |
| sharp/blur2.5/f8.0 | measure | bias -1.82 % · scatter 0.42 % | bias -2.16 % · scatter 0.42 % | bias -2.07 % · scatter 0.42 % | n/a (ERP free-β Moffat not run) |
| sharp/blur2.5/f8.0 | analysis | bias -5.31 % · scatter 3.92 % | bias -5.64 % · scatter 3.90 % | bias -5.55 % · scatter 3.91 % | n/a (ERP free-β Moffat not run) |
| mixture/b | measure | bias +8.45 % · scatter 0.66 % | bias +4.90 % · scatter 0.64 % | bias +3.28 % · scatter 0.63 % | Moffat: [pointfit] bias -0.70 % · scatter 0.60 % |
| mixture/b | analysis | bias +3.69 % · scatter 0.81 % | bias +0.30 % · scatter 0.78 % | bias -1.26 % · scatter 0.77 % | Moffat: [pointfit] bias -0.70 % · scatter 0.60 % |

| family | path | β sensitivity (betas path) | Auto β pick |
| ---- | ---- | ---- | ---- |
| sharp/m4/f2.6 | betas | FWHM spread over beta [2.5, 4.0, 6.0, 10.0]: median +9.60 % | beta 4 x1 |
| sharp/m2.5/f2.6 | betas | FWHM spread over beta [2.5, 4.0, 6.0, 10.0]: median +10.02 % | beta 2.5 x1 |
| sharp/gauss/f2.6 | betas | FWHM spread over beta [2.5, 4.0, 6.0, 10.0]: median +8.72 % | beta 10 x1 |
| mixture/b | betas | FWHM spread over beta [2.5, 4.0, 6.0, 10.0]: median +9.46 % | beta 2.5 x1 |
| resampled/f2.0 | betas | FWHM spread over beta [2.5, 4.0, 6.0, 10.0]: median +8.80 % | beta 10 x1 |

| family | path | FWHM² growth over the source (pointfit) | ideal |
| ---- | ---- | ---- | ---- |
| resampled/f2.0 | measure | [pointfit] FWHM^2 - truth^2: median 2.596 px^2 · scatter 0.084 | kernel: 8 ln2 x 0.3333 = 1.848 px^2 · source growth (sharp/m4/f2.0, pointfit): median 0.009 px^2 · scatter 0.054 (n 445) |
| resampled/f3.0 | measure | [pointfit] FWHM^2 - truth^2: median 2.650 px^2 · scatter 0.103 | kernel: 8 ln2 x 0.3333 = 1.848 px^2 · source growth n/a (source family sharp/m4/f3.0 is not in the manifest: its frame was rendered for the warp only and never measured) |

Readings against the three truth columns:
- **Moffat families, F ≥ 2.0.** The measure path tracks `pointfit` within the printed scatter, for example `bias
  +0.09 % · scatter 0.58 %` on `sharp/m4/f2.6`.
- **Gaussian and `blur2.5` families.** It reads `pointfit` low at every F ≥ 2.0. Examples:
  - `sharp/gauss/f2.6` `bias -3.01 % · scatter 0.50 %`;
  - `sharp/gauss/f8.0` `bias -2.58 % · scatter 0.36 %`;
  - `sharp/blur2.5/f2.6` `bias -2.50 % · scatter 0.54 %`.
- **Undersampled F 1.2.** It reads high:
  - `sharp/m4/f1.2` `bias +4.65 % · scatter 0.90 %`;
  - `sharp/m2.5/f1.2` `bias +8.62 % · scatter 0.80 %`;
  - `sharp/blur2.5/f1.2` `bias +21.38 % · scatter 2.89 %`.
- **Analysis path at F 8.0.** It reads `pointfit` low on Gaussian and blur profiles: `sharp/gauss/f8.0` `bias -4.95
  % · scatter 0.16 %`, against measure's `bias -2.58 % · scatter 0.36 %`.
- **β sensitivity.** The fixed-β arms spread each star's FWHM by `median +9.60 %` over β 2.5–10 on `sharp/m4/f2.6`.
  A β choice alone moves the width by that much.
- **Resampled frames.** `resampled/f2.0` grows FWHM² by `median 2.596 px^2` (pointfit) against a nominal kernel of
  `8 ln2 x 0.3333 = 1.848 px^2` and a source growth of `median 0.009 px^2`. The production warp broadens beyond
  the nominal kernel.

Divergent family-planes:

| Path | prepixel | pixbox | pointfit |
| ---- | ---- | ---- | ---- |
| measure | 145 | 149 | 77 |
| analysis | 105 | 79 | 67 |

Largest:
- measure pointfit: `resampled/f2.0 p0: [pointfit] bias +25.54 % · scatter 0.74 % | ERP Moffat: [pointfit] bias
  +24.33 % · scatter 0.90 %`, then `sharp/blur2.5/f1.2` and `resampled/f3.0`;
- analysis pointfit: `sharp/blur2.5/f1.2`, then the OSC VNG planes, for example `osc/f2.6/m4 p1: [pointfit] bias
  +14.70 % · scatter 5.12 %`.

### Row 4: eccentricity and angle

| family | path | round-star floor (median e at truth e 0) | ERP |
| ---- | ---- | ---- | ---- |
| shape/f2.6/e0.0/t0 | measure | median e 0.1151 · scatter 0.0819 | Moffat: median e 0.1239 · scatter 0.0561 |
| shape/f2.6/e0.0/t0 | analysis | median e 0.2795 · scatter 0.1401 | Moffat: median e 0.1239 · scatter 0.0561 |
| shape/f4.0/e0.0/t0 | measure | median e 0.1033 · scatter 0.0747 | Moffat: median e 0.1064 · scatter 0.0435 |
| shape/f4.0/e0.0/t0 | analysis | median e 0.0751 · scatter 0.0397 | Moffat: median e 0.1064 · scatter 0.0435 |
| sharp/m4/f1.2 | measure | median e 0.1388 · scatter 0.0885 | Moffat: median e 0.1467 · scatter 0.0466 |
| sharp/m4/f1.2 | analysis | median e 0.2346 · scatter 0.0924 | Moffat: median e 0.1467 · scatter 0.0466 |
| sharp/m4/f2.6 | measure | median e 0.1202 · scatter 0.0784 | Moffat: median e 0.1411 · scatter 0.0577 |
| sharp/m4/f2.6 | analysis | median e 0.2666 · scatter 0.1202 | Moffat: median e 0.1411 · scatter 0.0577 |
| sharp/m4/f8.0 | measure | median e 0.0868 · scatter 0.0666 | Moffat: median e 0.0741 · scatter 0.0255 |
| sharp/m4/f8.0 | analysis | median e 0.0639 · scatter 0.0341 | Moffat: median e 0.0741 · scatter 0.0255 |
| sharp/gauss/f2.6 | measure | median e 0.1170 · scatter 0.0851 | n/a (ERP free-β Moffat not run) |
| sharp/gauss/f2.6 | analysis | median e 0.3509 · scatter 0.1415 | n/a (ERP free-β Moffat not run) |

| family | path | ecc (pointfit) | angle (prepixel) | ERP ecc |
| ---- | ---- | ---- | ---- | ---- |
| shape/f2.6/e0.3/t30 | measure | [pointfit] bias -0.0047 · scatter 0.0332 | [prepixel] rms 44.564 deg · bias +3.181 | Moffat: [pointfit] bias +0.0044 · scatter 0.0242 |
| shape/f2.6/e0.3/t30 | analysis | [pointfit] bias +0.0151 · scatter 0.1151 | [prepixel] rms 37.152 deg · bias -3.575 | Moffat: [pointfit] bias +0.0044 · scatter 0.0242 |
| shape/f2.6/e0.5/t30 | measure | [pointfit] bias -0.0016 · scatter 0.0125 | [prepixel] rms 32.339 deg · bias +1.421 | Moffat: [pointfit] bias +0.0001 · scatter 0.0110 |
| shape/f2.6/e0.5/t30 | analysis | [pointfit] bias -0.0019 · scatter 0.0463 | [prepixel] rms 20.828 deg · bias -1.141 | Moffat: [pointfit] bias +0.0001 · scatter 0.0110 |
| shape/f2.6/e0.7/t30 | measure | [pointfit] bias +0.0003 · scatter 0.0060 | [prepixel] rms 21.125 deg · bias +0.256 | Moffat: [pointfit] bias -0.0000 · scatter 0.0056 |
| shape/f2.6/e0.7/t30 | analysis | [pointfit] bias -0.0015 · scatter 0.0312 | [prepixel] rms 16.805 deg · bias +0.368 | Moffat: [pointfit] bias -0.0000 · scatter 0.0056 |
| shape/f4.0/e0.5/t30 | measure | [pointfit] bias -0.0006 · scatter 0.0104 | [prepixel] rms 22.743 deg · bias -0.105 | Moffat: [pointfit] bias +0.0006 · scatter 0.0066 |
| shape/f4.0/e0.5/t30 | analysis | [pointfit] bias +0.0003 · scatter 0.0060 | [prepixel] rms 6.558 deg · bias -0.308 | Moffat: [pointfit] bias +0.0006 · scatter 0.0066 |
| shape/f4.0/e0.7/t30 | measure | [pointfit] bias +0.0002 · scatter 0.0050 | [prepixel] rms 8.185 deg · bias +0.260 | Moffat: [pointfit] bias +0.0003 · scatter 0.0032 |
| shape/f4.0/e0.7/t30 | analysis | [pointfit] bias -0.0757 · scatter 0.1380 | [prepixel] rms 28.627 deg · bias -1.189 | Moffat: [pointfit] bias +0.0003 · scatter 0.0032 |

- **Round-star floor.**
  - The measure path's floor sits near the ERP's: `median e 0.1202` against `0.1411` on `sharp/m4/f2.6`.
  - The analysis floor on the 24 `sharp` families at F ≤ 2.6 runs from `median e 0.1959 · scatter 0.1016`
    (`sharp/m10/f1.6`) to `median e 0.3635 · scatter 0.1461` (`sharp/gauss/f2.0`).
  - At F ≥ 3.5 the analysis floor falls below the measure path's: `sharp/m4/f5.0` `median e 0.0750` against `0.0978`.
- **Eccentricity at e ≥ 0.3.** The measure path's bias is within the printed scatter.
- **Angle.** The angle rms at e 0.3 on F 2.6 is `rms 44.564 deg` (measure). At e 0.7 on F 4.0 the analysis path
  reads `[pointfit] bias -0.0757 · scatter 0.1380` and `rms 28.627 deg`.

Divergent family-planes:

| Path | prepixel | pixbox | pointfit | moment |
| ---- | ---- | ---- | ---- | ---- |
| measure | 104 | 94 | 100 | 8 |
| analysis | 90 | 81 | 85 | 7 |

Nearly all are the round and near-round families, where the floor is the bias. The largest is the close `doubles`:
`doubles/f4.0 p0: [pointfit] bias +0.4915 · scatter 0.1575`.

### Row 5: background under the star

| family | path | ours |
| ---- | ---- | ---- |
| background/flat | measure | bias -0.076 sigma · scatter 0.100 |
| background/grad10 | measure | bias -0.081 sigma · scatter 0.096 |
| background/vig20 | measure | bias -0.084 sigma · scatter 0.099 |
| background/neb0.5 | measure | bias -0.064 sigma · scatter 0.309 |
| background/neb3 | measure | bias +0.251 sigma · scatter 1.964 |
| background/neb3_4096 | measure | bias +0.998 sigma · scatter 1.055 |
| halo | measure | bias -0.033 sigma · scatter 0.096 |
| density/d1000 | measure | bias -0.034 sigma · scatter 0.138 |
| resampled/f2.0 | measure | bias +0.166 sigma · scatter 0.215 |
| sharp/m4/f2.6 | measure | bias -0.088 sigma · scatter 0.127 |

- On flat, gradient, vignetted, halo and dense fields the fitted background reads between `bias -0.033 sigma` and
  `bias -0.088 sigma`, within its scatter.
- On the fractal nebula it reads high: `background/neb3_4096` `bias +0.998 sigma · scatter 1.055`.
- On resampled frames: `bias +0.166 sigma · scatter 0.215`.

The analysis, register and seeds paths carry no per-star background; this is a probe gap, counted in the summary.

Divergent (A): 0 family-planes.

### Row 6: flux and the LN scale

Captured fraction `signal / flux_img`, measure path, `sharp/<profile>/f<F>`:

| F | gauss | m10 | m4 | m2.5 | blur1 | blur2.5 |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 1.2 | median 0.8946 · scatter 0.0426 | median 0.8966 · scatter 0.0425 | median 0.8504 · scatter 0.0378 | median 0.7821 · scatter 0.0191 | median 0.8845 · scatter 0.0480 | median 0.9069 · scatter 0.0459 |
| 2.0 | median 0.9137 · scatter 0.0250 | median 0.8853 · scatter 0.0263 | median 0.8332 · scatter 0.0123 | median 0.7647 · scatter 0.0186 | median 0.8752 · scatter 0.0168 | median 0.9070 · scatter 0.0241 |
| 2.6 | median 0.9078 · scatter 0.0216 | median 0.8771 · scatter 0.0152 | median 0.8327 · scatter 0.0162 | median 0.7646 · scatter 0.0161 | median 0.8692 · scatter 0.0157 | median 0.9027 · scatter 0.0163 |
| 5.0 | median 0.9120 · scatter 0.0073 | median 0.8754 · scatter 0.0091 | median 0.8245 · scatter 0.0067 | median 0.7533 · scatter 0.0070 | median 0.8666 · scatter 0.0078 | median 0.9054 · scatter 0.0081 |
| 8.0 | median 0.9157 · scatter 0.0069 | median 0.8745 · scatter 0.0052 | median 0.8232 · scatter 0.0044 | median 0.7496 · scatter 0.0071 | median 0.8659 · scatter 0.0051 | median 0.9076 · scatter 0.0074 |

The same on the analysis path at F 2.6 and F 8.0:

| F | gauss | m10 | m4 | m2.5 | blur1 | blur2.5 |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 2.6 | median 1.1650 · scatter 0.1566 | median 1.0970 · scatter 0.1535 | median 1.0448 · scatter 0.1157 | median 0.9386 · scatter 0.0698 | median 1.0392 · scatter 0.1045 | median 1.1371 · scatter 0.1340 |
| 8.0 | median 0.9510 · scatter 0.0025 | median 0.9337 · scatter 0.0030 | median 0.8960 · scatter 0.0035 | median 1.5129 · scatter 0.2634 | median 1.3877 · scatter 0.2363 | median 1.5035 · scatter 0.2410 |

The noiseless 2F and 5F circle fractions (F1's geometry) at F 2.6:

| F | gauss | m10 | m4 | m2.5 | blur1 | blur2.5 |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 2.6 | noiseless 2F 1.0000 · 5F 1.0000 | noiseless 2F 0.9992 · 5F 1.0000 | noiseless 2F 0.9866 · 5F 0.9999 | noiseless 2F 0.9393 · 5F 0.9953 | noiseless 2F 0.9971 · 5F 1.0000 | noiseless 2F 0.9997 · 5F 1.0000 |

LN scale against `s_true` (`ln beta auto`, plane 0):

| family | target | f1_gc (production default) | psfSel | ERP LN |
| ---- | ---- | ---- | ---- | ---- |
| truthcheck | t3 | s 1.1127 vs s_true 1.1: +1.16 % | s 1.1038 vs s_true 1.1: +0.34 % | s 1.1499 (+4.54 %) |
| truthcheck | t4 | s 0.8009 vs s_true 0.8: +0.11 % | s 0.8025 vs s_true 0.8: +0.31 % | s 0.8463 (+5.79 %) |
| truthcheck | slope | +0.0040 %/px^2 (intercept +0.335 %) | +0.0088 %/px^2 (intercept -0.021 %) | — |
| density_pairs/d200 | t3 | s 1.2658 vs s_true 1.25: +1.26 % | s 1.2540 vs s_true 1.25: +0.32 % | s 1.2953 (+3.62 %) |
| density_pairs/d200 | slope | +0.0676 %/px^2 (intercept +0.202 %) | +0.0178 %/px^2 (intercept +0.153 %) | — |
| density_pairs/d600 | t3 | s 1.3050 vs s_true 1.25: +4.40 % | s 1.2573 vs s_true 1.25: +0.59 % | s 1.2981 (+3.85 %) |
| density_pairs/d600 | slope | +0.2538 %/px^2 (intercept +0.183 %) | +0.0386 %/px^2 (intercept +0.004 %) | — |
| pairs/iso/m4 | t60 | s 1.1777 vs s_true 1.1: +7.06 % | s 1.1061 vs s_true 1.1: +0.56 % | s 1.1524 (+4.76 %) |
| pairs/iso/m4 | e7t30 | s 1.0019 vs s_true 0.97: +3.29 % | s 0.9742 vs s_true 0.97: +0.44 % | s 1.0091 (+4.03 %) |
| pairs/iso/m4 | slope | +0.2463 %/px^2 (intercept +0.658 %) | +0.0216 %/px^2 (intercept +0.190 %) | — |
| pairs/iso/g | t60 | s 1.1632 vs s_true 1.1: +5.75 % | s 1.0893 vs s_true 1.1: -0.97 % | s 1.1142 (+1.29 %) |
| pairs/iso/g | slope | +0.2004 %/px^2 (intercept -0.060 %) | -0.0375 %/px^2 (intercept +0.027 %) | — |
| pairs/iso/blur4 | t60 | s 1.1515 vs s_true 1.1: +4.68 % | s 1.0199 vs s_true 1.1: -7.28 % | s 1.0783 (-1.97 %) |
| pairs/iso/blur4 | slope | +0.1613 %/px^2 (intercept +0.113 %) | -0.3118 %/px^2 (intercept +0.123 %) | — |
| pairs/main/m4 | t60 | s 1.2247 vs s_true 1.1: +11.34 % | s 1.1035 vs s_true 1.1: +0.32 % | s 1.1591 (+5.37 %) |
| pairs/main/m4 | e7t30 | s 1.0281 vs s_true 0.97: +5.99 % | s 0.9747 vs s_true 0.97: +0.48 % | s 1.0135 (+4.48 %) |
| pairs/main/m4 | slope | +0.3508 %/px^2 (intercept +2.223 %) | +0.0178 %/px^2 (intercept +0.209 %) | — |
| pairs/main/m25 | t60 | s 1.2223 vs s_true 1.1: +11.12 % | s 1.1107 vs s_true 1.1: +0.98 % | s 1.1844 (+7.67 %) |
| pairs/main/m25 | slope | +0.3624 %/px^2 (intercept +1.398 %) | +0.0454 %/px^2 (intercept -0.107 %) | — |
| pairs/main/g | t60 | s 1.2056 vs s_true 1.1: +9.60 % | s 1.0897 vs s_true 1.1: -0.93 % | s 1.1181 (+1.65 %) |
| pairs/main/g | slope | +0.3180 %/px^2 (intercept +0.844 %) | -0.0416 %/px^2 (intercept +0.130 %) | — |
| pairs/main/m4w10 | t60 | s 1.2302 vs s_true 1.1: +11.84 % | s 1.1067 vs s_true 1.1: +0.60 % | s 1.1616 (+5.60 %) |
| pairs/main/m4w10 | slope | +0.4021 %/px^2 (intercept +1.237 %) | +0.0305 %/px^2 (intercept -0.031 %) | — |
| osc_pairs/f2.6 | t3 | s 1.2772 vs s_true 1.25: +2.18 % | s 1.2709 vs s_true 1.25: +1.67 % | s 1.1286 (-9.71 %) |
| osc_pairs/f2.6 | slope | +0.2668 %/px^2 (intercept +0.121 %) | +0.1888 %/px^2 (intercept +0.173 %) | — |

The synthetic pairs ran without a reference-frames saturation map: their probe lines read `refSaturation null`. The
real-data rows of section 4.2 ran with each run's own map.

Readings:
- The measure path's captured fraction is profile-dependent at every F (verdict 2). Its per-star scatter is small,
  for example `0.0162` on `sharp/m4/f2.6`, so the profile offset dominates.
- At F 2.0 and F 2.6 the analysis path's captured fraction exceeds 1 with large scatter, for example
  `sharp/gauss/f2.0` `median 1.2325 · scatter 0.2247`. At F 8.0 it does so on `m2.5`, `blur1` and `blur2.5`, for
  example `sharp/m2.5/f8.0` `median 1.5129 · scatter 0.2634`.
- F1's 2F circle encloses `0.9393` of an `m2.5` star against `1.0000` of a Gaussian (noiseless, F 2.6). The 5F
  circle encloses `0.9953` against `1.0000`.
- **LN, F1.** F1 over-reads every `pairs` target of large width ratio (verdict 1).
- **LN, PSF signal with selection.** It holds within `-1.60 %` (`pairs/main/m4mix` t25) to `+1.16 %`
  (`pairs/iso/m4w10` t60) on the Moffat and Gaussian `pairs` families. It fails on the blurred ones:
  `pairs/iso/blur25mix` t60 `-13.03 %`, `pairs/iso/blur4` t60 `-7.28 %`.

Divergent (E, F1 worse than the ERP's LN on the same target): 63 targets per β arm. The largest is
`pairs/main/m4w10 p0 t60: s 1.2302 vs s_true 1.1: +11.84 % | ERP s 1.1616 (+5.60 %)`.

### Row 7: frame aggregates

| family | path | frame FWHM | frame ecc |
| ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | 1.4397 (residual-weighted mean over all accepted fits) | 0.1126 (residual-weighted mean over all accepted fits) |
| sharp/m4/f1.2 | analysis | 1.4348 (clipped weighted median over e <= 0.8) | 0.2331 (clipped weighted median over e <= 0.8) |
| sharp/m4/f2.0 | measure | 2.1323 (residual-weighted mean over all accepted fits) | 0.0947 (residual-weighted mean over all accepted fits) |
| sharp/m4/f2.0 | analysis | 2.1479 (clipped weighted median over e <= 0.8) | 0.3004 (clipped weighted median over e <= 0.8) |
| sharp/m4/f2.6 | measure | 2.7086 (residual-weighted mean over all accepted fits) | 0.0872 (residual-weighted mean over all accepted fits) |
| sharp/m4/f2.6 | analysis | 2.6888 (clipped weighted median over e <= 0.8) | 0.2607 (clipped weighted median over e <= 0.8) |
| sharp/m4/f5.0 | measure | 5.0798 (residual-weighted mean over all accepted fits) | 0.0678 (residual-weighted mean over all accepted fits) |
| sharp/m4/f5.0 | analysis | 5.0630 (clipped weighted median over e <= 0.8) | 0.0748 (clipped weighted median over e <= 0.8) |
| sharp/gauss/f2.6 | measure | 2.6188 (residual-weighted mean over all accepted fits) | 0.0936 (residual-weighted mean over all accepted fits) |
| sharp/gauss/f2.6 | analysis | 2.6758 (clipped weighted median over e <= 0.8) | 0.3495 (clipped weighted median over e <= 0.8) |
| sharp/gauss/f8.0 | measure | 7.8686 (residual-weighted mean over all accepted fits) | 0.0623 (residual-weighted mean over all accepted fits) |
| sharp/gauss/f8.0 | analysis | 7.6315 (clipped weighted median over e <= 0.8) | 0.0500 (clipped weighted median over e <= 0.8) |
| sharp/m2.5/f2.6 | measure | 2.7197 (residual-weighted mean over all accepted fits) | 0.0867 (residual-weighted mean over all accepted fits) |
| sharp/m2.5/f2.6 | analysis | 2.7268 (clipped weighted median over e <= 0.8) | 0.2756 (clipped weighted median over e <= 0.8) |
| sharp/blur2.5/f2.6 | measure | 2.7192 (residual-weighted mean over all accepted fits) | 0.0940 (residual-weighted mean over all accepted fits) |
| sharp/blur2.5/f2.6 | analysis | 2.7539 (clipped weighted median over e <= 0.8) | 0.2713 (clipped weighted median over e <= 0.8) |
| mixture/a | measure | 3.1662 (residual-weighted mean over all accepted fits) | 0.0851 (residual-weighted mean over all accepted fits) |
| mixture/a | analysis | 3.2027 (clipped weighted median over e <= 0.8) | 0.1045 (clipped weighted median over e <= 0.8) |
| mixture/a_clip | measure | 3.4554 (residual-weighted mean over all accepted fits) | 0.0787 (residual-weighted mean over all accepted fits) |
| mixture/a_clip | analysis | 3.4602 (clipped weighted median over e <= 0.8) | 0.0955 (clipped weighted median over e <= 0.8) |
| mixture/b | measure | 4.0020 (residual-weighted mean over all accepted fits) | 0.0867 (residual-weighted mean over all accepted fits) |
| mixture/b | analysis | 3.7679 (clipped weighted median over e <= 0.8) | 0.0982 (clipped weighted median over e <= 0.8) |
| mixture/b_clip | measure | 4.4144 (residual-weighted mean over all accepted fits) | 0.0892 (residual-weighted mean over all accepted fits) |
| mixture/b_clip | analysis | 4.4782 (clipped weighted median over e <= 0.8) | 0.2906 (clipped weighted median over e <= 0.8) |
| density/d1000 | measure | 3.1026 (residual-weighted mean over all accepted fits) | 0.0856 (residual-weighted mean over all accepted fits) |
| density/d1000 | analysis | 3.2593 (clipped weighted median over e <= 0.8) | 0.3175 (clipped weighted median over e <= 0.8) |
| trails | measure | 4.0849 (residual-weighted mean over all accepted fits) | 0.8125 (residual-weighted mean over all accepted fits) |
| trails | analysis | 2.8808 (clipped weighted median over e <= 0.8) | 0.6888 (clipped weighted median over e <= 0.8) |

Each cell names its statistic. Section 5 sets the frame FWHM against the three truth widths.

### Row 8: OSC per plane

| family | plane | FWHM (pointfit) on the VNG plane | on the ideal plane | flux ratio VNG | flux ratio ideal |
| ---- | ---- | ---- | ---- | ---- | ---- |
| osc/f2.0/m4 | 0 | bias +8.28 % | ideal plane: bias +0.18 % (n 407) | median 0.8865 | ideal plane: median 0.8380 (n 407) |
| osc/f2.0/m4 | 1 | bias +12.63 % | ideal plane: bias +3.06 % (n 419) | median 0.8512 | ideal plane: median 0.8310 (n 419) |
| osc/f2.0/m4 | 2 | bias -0.79 % | ideal plane: bias +0.12 % (n 397) | median 0.8805 | ideal plane: median 0.8340 (n 397) |
| osc/f2.6/m4 | 0 | bias +2.75 % | ideal plane: bias +0.06 % (n 392) | median 0.8808 | ideal plane: median 0.8331 (n 392) |
| osc/f2.6/m4 | 1 | bias +7.39 % | ideal plane: bias +0.14 % (n 408) | median 0.8509 | ideal plane: median 0.8326 (n 408) |
| osc/f2.6/m4 | 2 | bias -6.71 % | ideal plane: bias +0.08 % (n 391) | median 0.8852 | ideal plane: median 0.8292 (n 391) |
| osc/f3.5/m4 | 0 | bias +2.16 % | ideal plane: bias +0.03 % (n 369) | median 0.8692 | ideal plane: median 0.8266 (n 369) |
| osc/f3.5/m4 | 1 | bias +5.03 % | ideal plane: bias +0.04 % (n 381) | median 0.8406 | ideal plane: median 0.8283 (n 381) |
| osc/f3.5/m4 | 2 | bias -1.51 % | ideal plane: bias +0.07 % (n 368) | median 0.8483 | ideal plane: median 0.8277 (n 368) |
| osc/f5.0/m4 | 0 | bias +1.38 % | ideal plane: bias +0.07 % (n 325) | median 0.8497 | ideal plane: median 0.8245 (n 325) |
| osc/f5.0/m4 | 1 | bias +0.54 % | ideal plane: bias +0.05 % (n 329) | median 0.8408 | ideal plane: median 0.8263 (n 329) |
| osc/f5.0/m4 | 2 | bias -1.35 % | ideal plane: bias -0.00 % (n 311) | median 0.8373 | ideal plane: median 0.8247 (n 311) |
| osc/f2.6/blur1 | 0 | bias +0.30 % | ideal plane: bias -0.26 % (n 399) | median 0.9276 | ideal plane: median 0.8681 (n 399) |
| osc/f2.6/blur1 | 1 | bias +1.84 % | ideal plane: bias -0.24 % (n 414) | median 0.9106 | ideal plane: median 0.8678 (n 414) |
| osc/f2.6/blur1 | 2 | bias -6.63 % | ideal plane: bias -0.23 % (n 405) | median 0.9057 | ideal plane: median 0.8693 (n 405) |

- On the ideal planes the measure path tracks pointfit within `+3.06 %` on every family.
- On the production VNG planes the measure path reads G high, B low (verdict 3), and every flux ratio high:
  `osc/f2.6/m4` plane 0 `median 0.8808` against the ideal `median 0.8331`.

### Row 9: cost

| family | path | ours |
| ---- | ---- | ---- |
| sharp/m4/f2.6 | analysis | wall median 0.05 s (max 0.05) · peak RSS 143 MB (one path) |
| sharp/m4/f2.6 | stack | wall median 0.21 s (max 0.21) · peak RSS 143 MB (multi-path: measure+betas+register) |
| density/d1000 | analysis | wall median 0.07 s (max 0.07) · peak RSS 144 MB (one path) |
| density/d1000 | stack | wall median 0.33 s (max 0.33) · peak RSS 139 MB (multi-path: measure+betas+register) |
| background/neb3_4096 | analysis | wall median 0.21 s (max 0.21) · peak RSS 539 MB (one path) |
| background/neb3_4096 | betas | wall median 0.43 s (max 0.43) · peak RSS 460 MB (one path) |
| background/neb3_4096 | measure | wall median 0.39 s (max 0.39) · peak RSS 452 MB (one path) |
| background/neb3_4096 | register | wall median 0.38 s (max 0.38) · peak RSS 454 MB (one path) |
| osc/f2.6/m4 | analysis | wall median 0.07 s (max 0.07) · peak RSS 143 MB (one path) |
| osc/f2.6/m4 | ideal | wall median 0.58 s (max 0.58) · peak RSS 176 MB (multi-path: measure+betas on 3 planes) |
| osc/f2.6/m4 | stack | wall median 0.76 s (max 0.76) · peak RSS 232 MB (multi-path: measure+betas+register) |

One path per invocation on the 4096² image: `background/neb3_4096` measure `wall median 0.39 s (max 0.39) · peak
RSS 452 MB (one path)`.

The real frames run one path per invocation. On the star-test corpus (`star-lab/results/probe-runs-2026-10-07/batches.txt`):
- the 61 Mpx m101 measure invocations peaked at 1716 MB and analysis at 1944 MB;
- the real-data LN invocations peaked at 3733 MB (Horse C11 G, 28 members in one call).

### Row 10: selection and seeds

| family | image | Auto β | ideal |
| ---- | ---- | ---- | ---- |
| saturated/f2.6/n20 | image 0 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 0; among the 64 brightest truth stars: 20; in the image: 20 |
| saturated/f2.6/n20 | image 1 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 0; among the 64 brightest truth stars: 20; in the image: 20 |
| saturated/f2.6/n20_14bit | image 0 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 20; among the 64 brightest truth stars: 20; in the image: 20 |
| saturated/f2.6/n20_14bit | image 1 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 20; among the 64 brightest truth stars: 20; in the image: 20 |
| saturated/f2.6/n60 | image 0 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 3; among the 64 brightest truth stars: 60; in the image: 60 |
| saturated/f2.6/n60 | image 1 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 1; among the 64 brightest truth stars: 60; in the image: 60 |
| sharp/gauss/f2.6 | image 0 | beta 10.00 | saturated among the first 64 seeds (the Auto beta sample): 31; among the 64 brightest truth stars: 31; in the image: 31 |
| sharp/m10/f2.6 | image 0 | beta 10.00 | saturated among the first 64 seeds (the Auto beta sample): 28; among the 64 brightest truth stars: 31; in the image: 31 |
| sharp/m4/f2.6 | image 0 | beta 4.00 | saturated among the first 64 seeds (the Auto beta sample): 0; among the 64 brightest truth stars: 31; in the image: 31 |

| family | ours | ideal |
| ---- | ---- | ---- |
| sharp/m4/f2.6 | Spearman flux 0.3625 (all) · 0.4584 (unsaturated, n 401) | 1 · beside it, unsaturated: seed peak vs true peak 0.9994, detector order vs truth flux 0.4584 |
| saturated/f2.6/n60 | Spearman flux 0.5983 (all) · 0.9712 (unsaturated, n 775) | 1 · beside it, unsaturated: seed peak vs true peak 0.9992, detector order vs truth flux 0.9125 |
| density/d1000 | Spearman flux 0.0429 (all) · 0.1347 (unsaturated, n 1728) | 1 · beside it, unsaturated: seed peak vs true peak 0.9982, detector order vs truth flux 0.1347 |
| halo | Spearman flux -0.3107 (all) · -0.1933 (unsaturated, n 347) | 1 · beside it, unsaturated: seed peak vs true peak 0.9991, detector order vs truth flux -0.1933 |
| background/neb3 | Spearman flux 0.8443 (all) · 0.7403 (unsaturated, n 166) | 1 · beside it, unsaturated: seed peak vs true peak 0.9928, detector order vs truth flux 0.7403 |

| family | image | ours | ideal |
| ---- | ---- | ---- | ---- |
| saturated/f2.6/n60 | image 0 | selected 254 of 416 matched pairs (rule snr) | truth unsaturated with peak_snr >= 40: 272 · per-star share n/a (probe gap: ln_scale_probe dumps no per-star LN selection verdict, only counts) |
| saturated/f2.6/n60 | image 1 | selected 251 of 447 matched pairs (rule snr) | truth unsaturated with peak_snr >= 40: 269 · per-star share n/a (probe gap: ln_scale_probe dumps no per-star LN selection verdict, only counts) |
| sharp/m4/f3.5 | image 0 | selected 165 of 430 matched pairs (rule snr) | truth unsaturated with peak_snr >= 40: 269 · per-star share n/a (probe gap: ln_scale_probe dumps no per-star LN selection verdict, only counts) |
| halo | image 0 | selected 167 of 411 matched pairs (rule snr) | truth unsaturated with peak_snr >= 40: 263 · per-star share n/a (probe gap: ln_scale_probe dumps no per-star LN selection verdict, only counts) |

- **Auto β.** On `saturated/f2.6/n20_14bit` the Auto β sample holds 20 saturated stars among its 64; β stays at
  `4.00`. On `sharp/gauss/f2.6` the sample holds 31 saturated stars and picks `10.00`, the Moffat nearest a
  Gaussian.
- **Seed flux.** Seed flux ranks truth flux poorly on clean sharp frames (`Spearman flux 0.3625 (all)`), and
  inversely on `halo` (`-0.3107`).
- **The LN selection** keeps fewer pairs than the truth's unsaturated `peak_snr >= 40` population: `selected 254 of
  416` against `272` on `saturated/f2.6/n60` image 0. The per-star verdicts are a probe gap.

### Row 11: stability

| family | path | twins | frame \|ΔFWHM\|/FWHM | Auto β agrees (measure) |
| ---- | ---- | ---- | ---- | ---- |
| twins/sharp/m4/f1.2 | measure | noise twins | median 0.05 % · p90 0.05 % | 100.0 % of 3 pairs |
| twins/sharp/m4/f1.2 | measure | base vs deep (2x flux) | median 7.71 % · p90 7.71 % | 0.0 % of 1 pairs |
| twins/sharp/m4/f1.2 | analysis | noise twins | median 0.89 % · p90 1.02 % | — |
| twins/sharp/m4/f1.2 | analysis | base vs deep (2x flux) | median 2.97 % · p90 2.97 % | — |
| twins/sharp/m4/f2.0 | measure | noise twins | median 0.05 % · p90 0.06 % | 100.0 % of 3 pairs |
| twins/sharp/m4/f2.0 | measure | base vs deep (2x flux) | median 9.99 % · p90 9.99 % | 0.0 % of 1 pairs |
| twins/sharp/m4/f2.0 | analysis | noise twins | median 0.96 % · p90 1.25 % | — |
| twins/sharp/m4/f2.0 | analysis | base vs deep (2x flux) | median 4.17 % · p90 4.17 % | — |
| twins/sharp/m4/f2.6 | measure | noise twins | median 0.01 % · p90 0.01 % | 100.0 % of 3 pairs |
| twins/sharp/m4/f2.6 | measure | base vs deep (2x flux) | median 2.93 % · p90 2.93 % | 100.0 % of 1 pairs |
| twins/sharp/m4/f2.6 | analysis | noise twins | median 1.23 % · p90 1.33 % | — |
| twins/sharp/m4/f2.6 | analysis | base vs deep (2x flux) | median 5.87 % · p90 5.87 % | — |
| twins/sharp/m2.5/f1.6 | measure | noise twins | median 0.01 % · p90 0.02 % | 100.0 % of 3 pairs |
| twins/sharp/m2.5/f1.6 | measure | base vs deep (2x flux) | median 15.93 % · p90 15.93 % | 0.0 % of 1 pairs |
| twins/sharp/m2.5/f1.6 | analysis | noise twins | median 0.32 % · p90 0.34 % | — |
| twins/sharp/m2.5/f1.6 | analysis | base vs deep (2x flux) | median 6.26 % · p90 6.26 % | — |
| twins/sharp/m2.5/f2.0 | measure | noise twins | median 0.02 % · p90 0.04 % | 100.0 % of 6 pairs |
| twins/sharp/m2.5/f2.0 | measure | base vs deep (2x flux) | median 3.89 % · p90 4.10 % | 100.0 % of 2 pairs |
| twins/sharp/m2.5/f2.0 | analysis | noise twins | median 0.75 % · p90 1.23 % | — |
| twins/sharp/m2.5/f2.0 | analysis | base vs deep (2x flux) | median 27.50 % · p90 28.09 % | — |
| twins/mixture/b | measure | noise twins | median 0.05 % · p90 0.06 % | 100.0 % of 3 pairs |
| twins/mixture/b | measure | base vs deep (2x flux) | median 2.62 % · p90 2.62 % | 100.0 % of 1 pairs |
| twins/mixture/b | analysis | noise twins | median 0.06 % · p90 0.06 % | — |
| twins/mixture/b | analysis | base vs deep (2x flux) | median 16.08 % · p90 16.08 % | — |

| path | ours |
| ---- | ---- |
| analysis | Spearman 1.0000 · largest inversion 0.0000 px |
| measure | Spearman 1.0000 · largest inversion 0.0000 px |

- Noise twins are stable on the measure path. Over all noise twins the frame FWHM moves by `median 0.00 %`
  (`twins/sharp/m4/f8.0`, `twins/sharp/m10/f1.6`) to `median 0.09 %` (`twins/sharp/gauss/f1.2`). On the families in
  the table it moves by `0.01 %`–`0.05 %`.
- A 2× flux change is not stable (verdict 4). Auto β flips between base and deep on `twins/sharp/m4/f1.2`, `f1.6`,
  `f2.0`, `twins/sharp/m10/f1.2` and `twins/sharp/m2.5/f1.6`.
- The ladder ranks perfectly on the measure and analysis paths (`Spearman 1.0000 · largest inversion 0.0000 px`). The
  fixed-β arms print the same and are in the full scorecard.

## 4. Real data

### 4.1 Injected stars (`manifests/inject.json`; the injected stars are the referee)

| injected group | path | plane | FWHM (pointfit) | ecc (pointfit) | captured fraction | completeness |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| calibrated/hc11B-f1__mono__G__bin1__120s | measure | 0 | bias -3.68 % · scatter 2.75 % | bias +0.1345 · scatter 0.0948 | median 0.8353 · scatter 0.0166 | 100.0 % of 72 expected · raw 55.3 % of 161 |
| calibrated/hc11B-f1__mono__G__bin1__120s | analysis | 0 | bias -0.99 % · scatter 1.79 % | bias +0.1382 · scatter 0.1174 | median 0.9679 · scatter 0.1110 | 37.3 % of 161 expected · raw 37.3 % of 161 |
| calibrated/hc11B-f1__mono__L__bin1__180s | measure | 0 | bias -0.57 % · scatter 1.86 % | bias +0.1346 · scatter 0.0694 | median 0.8275 · scatter 0.0118 | 100.0 % of 67 expected · raw 48.8 % of 170 |
| calibrated/hc11B-f1__mono__L__bin1__180s | analysis | 0 | bias +0.24 % · scatter 1.06 % | bias +0.1062 · scatter 0.0623 | median 0.9021 · scatter 0.0143 | 13.5 % of 170 expected · raw 13.5 % of 170 |
| calibrated/star-lab-mono2__mono__L__bin1__300s | measure | 0 | bias +5.33 % · scatter 1.43 % | bias +0.1480 · scatter 0.1016 | median 0.8228 · scatter 0.0211 | 100.0 % of 92 expected · raw 69.6 % of 184 |
| calibrated/star-lab-mono2__mono__L__bin1__300s | analysis | 0 | bias -0.40 % · scatter 1.27 % | bias +0.1874 · scatter 0.0727 | median 0.9230 · scatter 0.0268 | 7.6 % of 184 expected · raw 7.6 % of 184 |
| mosaic/tierA-lnB-f1__osc__NoFilter__bin1__180s | measure | 0 | bias -0.24 % · scatter 5.26 % | bias +0.2364 · scatter 0.1668 | median 0.9038 · scatter 0.0682 | 98.2 % of 114 expected · raw 69.6 % of 184 |
| mosaic/tierA-lnB-f1__osc__NoFilter__bin1__180s | measure | 1 | bias +0.67 % · scatter 3.93 % | bias +0.2460 · scatter 0.1547 | median 0.8679 · scatter 0.0329 | 98.2 % of 110 expected · raw 74.5 % of 184 |
| mosaic/tierA-lnB-f1__osc__NoFilter__bin1__180s | measure | 2 | bias -5.85 % · scatter 4.15 % | bias +0.2575 · scatter 0.1358 | median 0.8665 · scatter 0.0524 | 99.0 % of 101 expected · raw 70.1 % of 184 |
| mosaic/tierA-lnB-f1__osc__NoFilter__bin1__180s | analysis | 1 | bias +6.71 % · scatter 4.33 % | bias +0.1098 · scatter 0.0465 | median 0.8993 · scatter 0.0133 | 11.4 % of 184 expected · raw 11.4 % of 184 |
| raw/corpus | measure | 0 | bias -1.52 % · scatter 6.37 % | bias +0.2025 · scatter 0.1274 | median 0.8389 · scatter 0.0285 | 96.9 % of 161 expected · raw 72.8 % of 250 |
| raw/corpus | measure | 1 | bias +9.06 % · scatter 4.63 % | bias +0.2215 · scatter 0.1064 | median 0.8416 · scatter 0.0516 | 97.4 % of 39 expected · raw 73.3 % of 60 |
| raw/corpus | measure | 2 | bias -2.22 % · scatter 4.87 % | bias +0.2861 · scatter 0.1504 | median 0.8900 · scatter 0.0861 | 97.3 % of 37 expected · raw 73.3 % of 60 |
| raw/corpus | analysis | 1 | bias +11.22 % · scatter 0.80 % | bias +0.1588 · scatter 0.1087 | median 0.9206 · scatter 0.0209 | 6.7 % of 60 expected · raw 6.7 % of 60 |
| raw/star-lab-mono2__osc__NoFilter__bin1__300s__raw | measure | 0 | bias -1.90 % · scatter 5.67 % | bias +0.2853 · scatter 0.1557 | median 0.9041 · scatter 0.0781 | 98.4 % of 122 expected · raw 71.6 % of 194 |
| raw/star-lab-mono2__osc__NoFilter__bin1__300s__raw | measure | 1 | bias -0.43 % · scatter 4.55 % | bias +0.2899 · scatter 0.1319 | median 0.8744 · scatter 0.0423 | 96.7 % of 123 expected · raw 71.6 % of 194 |
| raw/star-lab-mono2__osc__NoFilter__bin1__300s__raw | measure | 2 | bias -6.29 % · scatter 5.90 % | bias +0.2824 · scatter 0.1443 | median 0.8866 · scatter 0.0710 | 96.3 % of 107 expected · raw 70.6 % of 194 |
| raw/star-lab-mono2__osc__NoFilter__bin1__300s__raw | analysis | 1 | n/a (no matched star) | n/a (no matched star) | n/a (no matched star) | 0.0 % of 194 expected · raw 0.0 % of 194 |

The injected stars are Moffat-4 at a chosen width. The measure path fits them at the frame's Auto β, so a β
mismatch shows as a width bias:
- Horse C11 G reads `bias -3.68 % · scatter 2.75 %`;
- QHY268M set 107 reads `bias +5.33 % · scatter 1.43 %`.

On the real frames the measure path's FWHM efficiency (pointfit; scatter over the CRLB) is 1.25–4.99. The `eff` cells are in
the inject summary.

The analysis path keeps 500 stars per frame, so few injected stars are among them:
- calibrated frames: `7.6 %`–`37.3 %` of expected;
- the Pro raw lights: `0.0 % of 194 expected`.

Injected LN pairs (`s_inj` 0.8, the scorecard's own rows; median ratio over the injected stars only):

| pair | plane | β | PSF signal | 2F circle | 3F circle |
| ---- | ---- | ---- | ---- | ---- | ---- |
| star-lab-mono2__mono__L__bin1__300s__pair | 0 | 4 | +0.32 % (ratio 0.8025, n 30) | -0.06 % (ratio 0.7995, n 30) | +0.53 % (ratio 0.8043, n 30) |
| star-lab-mono2__mono__L__bin1__300s__pair | 0 | auto | +0.34 % (ratio 0.8027, n 30) | -0.05 % (ratio 0.7996, n 30) | +0.55 % (ratio 0.8044, n 29) |
| star-lab-mono2__osc__NoFilter__bin1__300s__pair | 0 | 4 | -1.01 % (ratio 0.7919, n 29) | -0.06 % (ratio 0.7995, n 29) | -0.32 % (ratio 0.7975, n 29) |
| star-lab-mono2__osc__NoFilter__bin1__300s__pair | 0 | auto | -0.94 % (ratio 0.7925, n 29) | -0.05 % (ratio 0.7996, n 29) | -0.31 % (ratio 0.7975, n 29) |
| star-lab-mono2__osc__NoFilter__bin1__300s__pair | 1 | 4 | -0.12 % (ratio 0.7991, n 32) | -0.00 % (ratio 0.8000, n 32) | -0.01 % (ratio 0.7999, n 32) |
| star-lab-mono2__osc__NoFilter__bin1__300s__pair | 1 | auto | +0.11 % (ratio 0.8009, n 32) | -0.02 % (ratio 0.7998, n 32) | -0.01 % (ratio 0.7999, n 31) |
| star-lab-mono2__osc__NoFilter__bin1__300s__pair | 2 | 4 | -1.25 % (ratio 0.7900, n 27) | -0.72 % (ratio 0.7942, n 27) | -0.91 % (ratio 0.7927, n 27) |
| star-lab-mono2__osc__NoFilter__bin1__300s__pair | 2 | auto | -0.67 % (ratio 0.7946, n 27) | -1.24 % (ratio 0.7900, n 27) | -1.01 % (ratio 0.7919, n 27) |
| tierA-lnB-f1__osc__NoFilter__bin1__180s__pair | 0 | 4 | -0.07 % (ratio 0.7994, n 35) | -0.16 % (ratio 0.7987, n 35) | -0.26 % (ratio 0.7979, n 32) |
| tierA-lnB-f1__osc__NoFilter__bin1__180s__pair | 0 | auto | +0.15 % (ratio 0.8012, n 35) | -0.17 % (ratio 0.7986, n 35) | -0.37 % (ratio 0.7970, n 34) |
| tierA-lnB-f1__osc__NoFilter__bin1__180s__pair | 1 | 4 | -0.27 % (ratio 0.7979, n 35) | +0.15 % (ratio 0.8012, n 35) | -0.11 % (ratio 0.7991, n 35) |
| tierA-lnB-f1__osc__NoFilter__bin1__180s__pair | 1 | auto | -0.27 % (ratio 0.7979, n 35) | +0.15 % (ratio 0.8012, n 35) | -0.11 % (ratio 0.7991, n 35) |
| tierA-lnB-f1__osc__NoFilter__bin1__180s__pair | 2 | 4 | -0.14 % (ratio 0.7989, n 30) | +0.32 % (ratio 0.8026, n 30) | +0.04 % (ratio 0.8003, n 30) |
| tierA-lnB-f1__osc__NoFilter__bin1__180s__pair | 2 | auto | -0.55 % (ratio 0.7956, n 30) | +0.34 % (ratio 0.8027, n 30) | +0.26 % (ratio 0.8021, n 30) |

### 4.2 The LN scale per member on real data (truth 1: the harness photometry on each run's own reference)

- **Ours.** `ln_scale_probe` at the production default arm (F1, growth curve). It ran on each checkpoint-B run's own:
  - LN reference;
  - reference-frames saturation map;
  - recorded group β;
  - registered members (run B: `--masked`).
- **Parity.** The probe matches the runs' applied scales (`.athln`) to a median of:
  - set 204 mono `+0.0097 %`, OSC `+0.0028 %`;
  - run B mono `-0.0007 %`, OSC `+0.0084 %`;
  - Horse C11 G `-0.0719 %`, with the widest range, `-0.3585 .. +0.6784 %`.
- **Truth identity.** Run B's and Horse C11 G's truth was read on another run's files. The per-file check is in
  `star-lab/results/probe-runs-2026-10-07/truth-identity.txt`:
  - run B: the reference and all 82 mono + 101 OSC truth members are data-identical;
  - Horse C11 G: the reference is data-identical; 27 of 28 members are bit-equal, and the 28th is at max |diff| /
    max of `1.173e-08`.

| dataset | planes | n | F1 (production default): median gap · median \|gap\| · p90 \|gap\| · slope | PSF signal with selection | probe ÷ run `.athln` − 1 |
| ---- | ---- | ---- | ---- | ---- | ---- |
| s204-mono | pooled | 91 | -0.108 % · 0.293 % · 1.076 % · -0.107 %/px2 | -6.009 % · 6.128 % · 10.622 % · -3.353 %/px2 | +0.0097 % (-0.0760 .. +0.1873 %) |
| s204-osc | pooled | 39 | -0.168 % · 0.420 % · 1.058 % · -0.480 %/px2 | -0.430 % · 1.056 % · 4.162 % · -4.385 %/px2 | +0.0028 % (-0.0798 .. +0.1189 %) |
| s204-osc | plane 0 | 13 | +0.029 % · 0.380 % · 0.671 % · -0.266 %/px2 | -0.546 % · 1.371 % · 4.278 % · -5.356 %/px2 | +0.0167 % (-0.0026 .. +0.1189 %) |
| s204-osc | plane 1 | 13 | -0.152 % · 0.315 % · 0.546 % · -0.571 %/px2 | +0.068 % · 0.810 % · 2.343 % · -3.454 %/px2 | +0.0008 % (-0.0184 .. +0.0565 %) |
| s204-osc | plane 2 | 13 | -0.571 % · 0.790 % · 1.315 % · -0.986 %/px2 | -0.430 % · 1.180 % · 2.450 % · -3.749 %/px2 | -0.0093 % (-0.0798 .. +0.0479 %) |
| runB-mono | pooled | 82 | +0.084 % · 0.327 % · 0.905 % · -0.071 %/px2 | -7.467 % · 7.467 % · 10.290 % · -3.411 %/px2 | -0.0007 % (-0.0851 .. +0.2072 %) |
| runB-osc | pooled | 303 | -0.129 % · 0.391 % · 1.217 % · +0.582 %/px2 | +0.028 % · 0.960 % · 3.694 % · -4.850 %/px2 | +0.0084 % (-0.0698 .. +0.1185 %) |
| runB-osc | plane 0 | 101 | +0.057 % · 0.347 % · 0.759 % · +0.448 %/px2 | -0.370 % · 1.723 % · 4.122 % · -7.361 %/px2 | +0.0009 % (-0.0487 .. +0.0527 %) |
| runB-osc | plane 1 | 101 | -0.129 % · 0.242 % · 0.571 % · -0.163 %/px2 | +0.189 % · 0.714 % · 2.073 % · -4.289 %/px2 | +0.0154 % (-0.0371 .. +0.0741 %) |
| runB-osc | plane 2 | 101 | -0.647 % · 0.781 % · 1.586 % · +0.322 %/px2 | +0.087 % · 0.895 % · 2.178 % · -3.878 %/px2 | +0.0042 % (-0.0698 .. +0.1185 %) |
| hc11-G | pooled | 28 | -0.137 % · 0.203 % · 2.149 % · -0.045 %/px2 | -2.758 % · 2.758 % · 6.139 % · -0.545 %/px2 | -0.0719 % (-0.3585 .. +0.6784 %) |

Error bars come from the one full gates run, `star-lab/results/baseline-2026-10-07/gates.txt`. Its truth gates equal,
target for target, the numbers the 11a LN rows carried, so no bar was re-carried. Each member carries the bar of the
nearest target in its own truth mode:

| Truth mode | Target | Bar |
| ---- | ---- | ---- |
| normal | FWHM 3.0 | `+0.053 % ± 0.061 %` |
| normal | FWHM 4.5 | `-0.170 % ± 0.059 %` |
| normal | FWHM 6.0 | `-0.203 % ± 0.075 %` |
| normal | FWHM 7.5 | `-0.287 % ± 0.060 %` |
| crowded (crowded-mode units only) | FWHM 3.0 | `-0.045 % ± 0.097 %` (the verdict pools 5 of 10 read units) |
| crowded | FWHM 4.0 | `+0.154 % ± 0.118 %` |
| crowded | FWHM 5.0 | `+0.594 % ± 0.394 %` (SOFT: 5 of 60 realizations read) |

- **The crowded FWHM 5.0 target is soft.** It is quoted as |gap| + 2·SE = 1.38 %. Of the 60 seeded
  realizations, 5 were read and 55 dropped (1–9 usable stars, median 6). The bar is conditional on the harness
  returning a ratio, and the selection likely understates the crowding bias.
- 49 set 204 mono members and 43 run B mono members carry that soft bar. 10 and 7 of them sit beyond the crowded
  gate's 3–5 px range (5.53–5.97 px), each flagged on its row in `ln-real-members.csv`.

The real-data F1 median gaps:
- pooled: `-0.168 %` to `+0.084 %`;
- on single planes: down to `-0.647 %` (run B, plane 2).

Most mono members carry the soft crowded bar, 1.38 %. The normal-mode bars are gaps of 0.053–0.287 % with SEs of
0.059–0.075 %.

**The two referees disagree on F1, and the disagreement is open.**

The real groups do reach the synthetic conditions:
- Width ratio `fwhm_mem / fwhm_ref` (*derived* from `ln-real-members.csv`):
  - set 204 mono: up to 1.748 (FWHM² difference 23.6 px²) against a 3.385 px reference;
  - run B mono: up to 1.761 (24.1 px²) against 3.39 px.

  The synthetic t45 ratio is 1.5 and t60's is 2.0.
- Crowding: 50 of 91 set 204 mono members and 49 of 82 run B mono members took the harness's crowded fallback
  (`truth modes crowded 50, normal 41`; `crowded 49, normal 33`).

At those conditions the referees disagree:
- **Synthetic, exact truth.** F1 reads:
  - `s 1.0102 vs s_true 0.95: +6.33 %` at ratio 1.5 (`pairs/main/m4w10` t45);
  - `s 1.3050 vs s_true 1.25: +4.40 %` at ratio 1.67 (`density_pairs/d600` t3);
  - up to `+11.84 %` at ratio 2.0.
- **Real, harness truth.** F1 reads:
  - set 204 mono: median gap `-0.108 %`, slope `-0.107 %/px2`;
  - run B mono: median gap `+0.084 %`, slope `-0.071 %/px2`.

  Both sit inside the soft crowded bar of 1.38 % that most of these members carry.
- **The high-ratio members alone** (ratio ≥ 1.5, *derived*): the real F1 median gap is −1.076 % on set 204 mono
  (13 members) and −1.836 % on run B mono (7). These are low, where the synthetic rows read high.

Part 1 does not resolve this. The candidates, none of them tested here:
- the lab's fields differ from the real ones (flux distribution, wings, the sky step);
- the harness's crowded fallback shares F1's bias on real frames;
- the real members' neighbour density inside F1's annulus differs from the lab's.

**Crowding, as this report uses the word.** It means neighbours inside F1's sky annulus `[6F, 8F]`, where F is the
common FWHM of the pair (`SKY_INNER_FWHM 6.0`, `SKY_OUTER_FWHM 8.0` in `stacking/ln/aperture.rs`). At the t60
pair's common FWHM (6.08 px, `commonFwhmPx` in its probe line) the annulus spans 36.5–48.6 px (*derived*). The
grids' separations:

| Grid | Minimum separation | Neighbours inside the annulus at F 6.08 | F1 reading |
| ---- | ---- | ---- | ---- |
| `grid/iso` (the `pairs/iso/*` families) | 25 px (`--min-sep 25`, `make_grid.py`), 1904 stars on 2048² | yes | crowded by this definition despite its name |
| `grid/main` | none, 6000 stars on 2048² | yes | crowded |
| `truthcheck` | 120.0 px (`min_sep 120.0` in its v2 spec) | no | isolated: slope `+0.0040 %/px^2` |

The real members' annulus crowding is not summarised here. The probe lines carry the L-26 `annulusCrowding`
diagnostic per member.

### 4.3 The star-test corpus (comparison only: there is no truth on these frames)

The corpus is m42 (77), m101 (30) and singles (38). The ERP's per-frame tables are joined by basename. Table:
`star-lab/results/probe-runs-2026-10-07/star-test-frames.csv`. Summary: `star-test-summary.txt` beside it.

| Set | Path | Median FWHM ours / ERP | Ours ÷ ERP | Spearman |
| ---- | ---- | ---- | ---- | ---- |
| m42 | measure | 2.4749 / 2.3362 | 1.0243 | 0.9840 |
| m42 | analysis | 2.3014 / 2.3362 | 0.9708 | 0.9937 |
| m101 (28, without the 2 known-bad ERP rows) | measure | 5.5033 / 5.8582 | 0.9015 | 0.9776 |
| m101 (28) | analysis | 5.8318 / 5.8582 | 0.9732 | 0.9907 |
| singles (32 of 38) | measure | 2.6597 / 2.5933 | 1.0127 | 0.9622 |
| singles (32 of 38) | analysis | 2.7961 / 2.5933 | 0.9967 | 0.9729 |

The two known-bad m101 ERP rows are reported and never tuned toward:

| Frame | ERP FWHM / ecc | Ours, measure | Ours, analysis |
| ---- | ---- | ---- | ---- |
| `Light_M101_…_0002.fit` | 1.3259 / 0.727 | 3.3054 / 0.6386 | 11.2197 / 0.7133 |
| `Light_M101_…_0004.fit` | 32.32 / 0.947 | 4.8155 / 0.5926 | 14.0632 / 0.5125 |

n/a:
- The 5 XISF singles, on both paths. star_probe reads every input through its FITS reader, so the analysis path never
  sees an XISF. This is a probe gap, not a product gap; the fix is parked for part 3.
- `_DSC5767.fit` has no ERP row.

Findings from the real-data runs, as observed (Task 11a):
- **The measure path reads broad or defocused frames far below both the analysis path and the ERP.**
  - The m101 2023 defocused set: measure ÷ ERP median 0.8219 (0.7182 … 0.8778), analysis 0.9689 (derived from the
    committed table).
  - `2024-03-01 … M 51 …H…0060`: measure 6.53 px on 27 fits, ERP 14.02 px, analysis 13.39 px.
  - `m82-outoffocus`: measure 11.43, ERP 17.76, analysis 16.31.

  This is comparison only, with no truth on these frames. The synthetic `sharp/*/f8.0` rows (row 3) are where truth
  can judge broad stars.
- **`_DSC5767.fit`** (a 3-plane 16-bit DSLR file, BZERO 0, maximum 32767, plane MAD 0.11 of full scale) gives the
  measure path 0 seeds on every plane. The analysis path measures 500 stars at 3.69 px, ecc 0.602.
- **The B plane is the weakest under F1 on both OSC datasets:**
  - set 204: `median gap -0.571 %`, `median |gap| 0.790 %`, slope `-0.986 %/px2`;
  - run B: `-0.647 %`, `0.781 %`, `+0.322 %/px2`.

  Row 8's VNG B-plane width bias is the synthetic counterpart.
- **sLib duplicates psfSel.** `sLib`, the probe's direct `relative_scale_against` call, equals `psfSel` to the printed
  precision on four datasets. Horse C11 G reads `-2.755 %` against `-2.758 %`.
- **Horse C11 G's probe-vs-run parity is the widest:** `-0.3585 .. +0.6784 %` against ≤ `0.2072 %` elsewhere.

## 5. S-2 per consumer: the three truth widths side by side

The FWHM and eccentricity filters are absolute pixel thresholds. Their referee is the owner's ruling between the three
widths. The master FWHM needs the same ruling, and stability (row 11). Here are the frame FWHMs each path reports
against all three. The `mixture` families are master-like profiles: equal-weight mixtures at FWHM {2.6, 3.4, 4.5} (a)
and {3.0, 5.0, 7.0} (b); `_clip` is a high-rejection master.

| family | path | frame FWHM (ours) | truth widths (median over all truth stars and measured stars) |
| ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | 1.4397 (residual-weighted mean over all accepted fits) | prepixel: med 1.200 · wmean 1.200 · all 1.200 \| pixbox: med 1.469 · wmean 1.469 · all 1.469 \| pointfit: med 1.372 · wmean 1.372 · all 1.372 |
| sharp/m4/f1.2 | analysis | 1.4348 (clipped weighted median over e <= 0.8) | prepixel: med 1.200 · wmean 1.200 · all 1.200 \| pixbox: med 1.469 · wmean 1.469 · all 1.469 \| pointfit: med 1.372 · wmean 1.372 · all 1.372 |
| sharp/m4/f2.0 | measure | 2.1323 (residual-weighted mean over all accepted fits) | prepixel: med 2.000 · wmean 2.000 · all 2.000 \| pixbox: med 2.163 · wmean 2.163 · all 2.163 \| pointfit: med 2.123 · wmean 2.123 · all 2.123 |
| sharp/m4/f2.0 | analysis | 2.1479 (clipped weighted median over e <= 0.8) | prepixel: med 2.000 · wmean 2.000 · all 2.000 \| pixbox: med 2.163 · wmean 2.163 · all 2.163 \| pointfit: med 2.123 · wmean 2.123 · all 2.123 |
| sharp/m4/f2.6 | measure | 2.7086 (residual-weighted mean over all accepted fits) | prepixel: med 2.600 · wmean 2.600 · all 2.600 \| pixbox: med 2.726 · wmean 2.726 · all 2.726 \| pointfit: med 2.695 · wmean 2.695 · all 2.695 |
| sharp/m4/f2.6 | analysis | 2.6888 (clipped weighted median over e <= 0.8) | prepixel: med 2.600 · wmean 2.600 · all 2.600 \| pixbox: med 2.726 · wmean 2.726 · all 2.726 \| pointfit: med 2.695 · wmean 2.695 · all 2.695 |
| sharp/m4/f5.0 | measure | 5.0798 (residual-weighted mean over all accepted fits) | prepixel: med 5.000 · wmean 5.000 · all 5.000 \| pixbox: med 5.065 · wmean 5.065 · all 5.065 \| pointfit: med 5.050 · wmean 5.050 · all 5.050 |
| sharp/m4/f5.0 | analysis | 5.0630 (clipped weighted median over e <= 0.8) | prepixel: med 5.000 · wmean 5.000 · all 5.000 \| pixbox: med 5.065 · wmean 5.065 · all 5.065 \| pointfit: med 5.050 · wmean 5.050 · all 5.050 |
| sharp/gauss/f2.6 | measure | 2.6188 (residual-weighted mean over all accepted fits) | prepixel: med 2.600 · wmean 2.600 · all 2.600 \| pixbox: med 2.690 · wmean 2.690 · all 2.690 \| pointfit: med 2.689 · wmean 2.689 · all 2.689 |
| sharp/gauss/f2.6 | analysis | 2.6758 (clipped weighted median over e <= 0.8) | prepixel: med 2.600 · wmean 2.600 · all 2.600 \| pixbox: med 2.690 · wmean 2.690 · all 2.690 \| pointfit: med 2.689 · wmean 2.689 · all 2.689 |
| sharp/gauss/f8.0 | measure | 7.8686 (residual-weighted mean over all accepted fits) | prepixel: med 8.000 · wmean 8.000 · all 8.000 \| pixbox: med 8.029 · wmean 8.029 · all 8.029 \| pointfit: med 8.029 · wmean 8.029 · all 8.029 |
| sharp/gauss/f8.0 | analysis | 7.6315 (clipped weighted median over e <= 0.8) | prepixel: med 8.000 · wmean 8.000 · all 8.000 \| pixbox: med 8.029 · wmean 8.029 · all 8.029 \| pointfit: med 8.029 · wmean 8.029 · all 8.029 |
| sharp/m2.5/f2.6 | measure | 2.7197 (residual-weighted mean over all accepted fits) | prepixel: med 2.600 · wmean 2.600 · all 2.600 \| pixbox: med 2.748 · wmean 2.748 · all 2.748 \| pointfit: med 2.700 · wmean 2.700 · all 2.700 |
| sharp/m2.5/f2.6 | analysis | 2.7268 (clipped weighted median over e <= 0.8) | prepixel: med 2.600 · wmean 2.600 · all 2.600 \| pixbox: med 2.748 · wmean 2.748 · all 2.748 \| pointfit: med 2.700 · wmean 2.700 · all 2.700 |
| sharp/blur2.5/f2.6 | measure | 2.7192 (residual-weighted mean over all accepted fits) | prepixel: med 2.691 · wmean 2.691 · all 2.691 \| pixbox: med 2.781 · wmean 2.781 · all 2.781 \| pointfit: med 2.778 · wmean 2.778 · all 2.778 |
| sharp/blur2.5/f2.6 | analysis | 2.7539 (clipped weighted median over e <= 0.8) | prepixel: med 2.691 · wmean 2.691 · all 2.691 \| pixbox: med 2.781 · wmean 2.781 · all 2.781 \| pointfit: med 2.778 · wmean 2.778 · all 2.778 |
| mixture/a | measure | 3.1662 (residual-weighted mean over all accepted fits) | prepixel: med 3.071 · wmean 3.071 · all 3.071 \| pixbox: med 3.196 · wmean 3.196 · all 3.196 \| pointfit: med 3.211 · wmean 3.211 · all 3.211 |
| mixture/a | analysis | 3.2027 (clipped weighted median over e <= 0.8) | prepixel: med 3.071 · wmean 3.071 · all 3.071 \| pixbox: med 3.196 · wmean 3.196 · all 3.196 \| pointfit: med 3.211 · wmean 3.211 · all 3.211 |
| mixture/a_clip | measure | 3.4554 (residual-weighted mean over all accepted fits) | prepixel: med 3.350 · wmean 3.350 · all 3.350 \| pixbox: med 3.462 · wmean 3.462 · all 3.462 \| pointfit: med 3.452 · wmean 3.452 · all 3.452 |
| mixture/a_clip | analysis | 3.4602 (clipped weighted median over e <= 0.8) | prepixel: med 3.350 · wmean 3.350 · all 3.350 \| pixbox: med 3.462 · wmean 3.462 · all 3.462 \| pointfit: med 3.452 · wmean 3.452 · all 3.452 |
| mixture/b | measure | 4.0020 (residual-weighted mean over all accepted fits) | prepixel: med 3.634 · wmean 3.634 · all 3.634 \| pixbox: med 3.757 · wmean 3.757 · all 3.757 \| pointfit: med 3.816 · wmean 3.816 · all 3.816 |
| mixture/b | analysis | 3.7679 (clipped weighted median over e <= 0.8) | prepixel: med 3.634 · wmean 3.634 · all 3.634 \| pixbox: med 3.757 · wmean 3.757 · all 3.757 \| pointfit: med 3.816 · wmean 3.816 · all 3.816 |
| mixture/b_clip | measure | 4.4144 (residual-weighted mean over all accepted fits) | prepixel: med 4.355 · wmean 4.355 · all 4.355 \| pixbox: med 4.464 · wmean 4.464 · all 4.464 \| pointfit: med 4.460 · wmean 4.460 · all 4.460 |
| mixture/b_clip | analysis | 4.4782 (clipped weighted median over e <= 0.8) | prepixel: med 4.355 · wmean 4.355 · all 4.355 \| pixbox: med 4.464 · wmean 4.464 · all 4.464 \| pointfit: med 4.460 · wmean 4.460 · all 4.460 |
| density/d1000 | measure | 3.1026 (residual-weighted mean over all accepted fits) | prepixel: med 3.000 · wmean 3.000 · all 3.000 \| pixbox: med 3.109 · wmean 3.109 · all 3.109 \| pointfit: med 3.083 · wmean 3.083 · all 3.083 |
| density/d1000 | analysis | 3.2593 (clipped weighted median over e <= 0.8) | prepixel: med 3.000 · wmean 3.000 · all 3.000 \| pixbox: med 3.109 · wmean 3.109 · all 3.109 \| pointfit: med 3.083 · wmean 3.083 · all 3.083 |
| trails | measure | 4.0849 (residual-weighted mean over all accepted fits) | prepixel: med 4.848 · wmean 4.334 · all 5.786 \| pixbox: med 4.934 · wmean 4.418 · all 5.886 \| pointfit: med 4.578 · wmean 4.161 · all 5.252 |
| trails | analysis | 2.8808 (clipped weighted median over e <= 0.8) | prepixel: med 5.983 · wmean 5.126 · all 5.786 \| pixbox: med 6.117 · wmean 5.226 · all 5.886 \| pointfit: med 5.738 · wmean 4.821 · all 5.252 |

Derived from the cells above (frame FWHM ÷ truth − 1):

| Family | Path | vs prepixel | vs pixbox | vs pointfit |
| ---- | ---- | ---- | ---- | ---- |
| `sharp/m4/f2.6` | measure | +4.18 % | −0.63 % | +0.49 % |
| `sharp/gauss/f2.6` | measure | +0.72 % | −2.66 % | −2.62 % |
| `mixture/b` | measure | +10.14 % | +6.53 % | +4.88 % |
| `mixture/b` | analysis | +3.69 % | +0.30 % | −1.26 % |
| `mixture/b_clip` | measure | +1.37 % | −1.12 % | −1.03 % |
| `sharp/m4/f1.2` | measure | +19.98 % | −1.98 % | +4.93 % |

Readings by consumer:
- **Filters.** No single truth width is tracked by every profile. On Moffat profiles the measure frame FWHM sits
  closest to `pointfit`, on Gaussian ones to `prepixel`. The ruling between the three decides which bias the user's
  pixel threshold inherits.
- **The master FWHM.** On the master-like `mixture/b` the measure path's frame FWHM is +4.88 % above `pointfit`
  (derived), and the analysis path's is −1.26 %. Clipping the core (`b_clip`) brings the measure path to −1.03 %.
  Row 11's base-vs-deep instability adds `median 2.62 %` (`twins/mixture/b`, measure).
- **Weights.** The captured fraction depends on the profile (row 6).
- **F1's common F.** The 2F circle encloses a profile-dependent fraction, 0.9393–1.0000, while 5F holds 0.9953–1.0000
  (row 6).
- **Mono registration and the LN seeds.** These are positions. The measure and register centroids read an efficiency
  of 0.89–1.21 against the CRLB; the seeds read near 2 (row 2).

## 6. The pre-warp injection mode

Not done (plan ruling P-7). The warp-parity gate does not apply. The warp's own effect is measured on the synthetic
`resampled` family (row 3).

## 7. The ranked list for part 2

Each item names the defect, its row, its number and the consumer it harms. Part 2 sets its bars from these numbers
before any fix is measured.

1. **`FitDiagnostics`, the fit's side channel (S-11): part 2's first task.** Production exposes no refusal reason
   and no per-fit diagnostics. Part 1 attributes refusals through truth, and reads the background setup through the
   fitted background (row 5).
   - Rows: 1 (`sharp/m4/f8.0` measure `82.7 %` against seeds `92.0 %`), 5.
   - Consumer: every diagnosis that follows.
2. **Reconcile F1's synthetic over-read with its real-data agreement (open). This is the first numeric item.**
   - Synthetic, row 6:
     - `+11.84 %` worst (`pairs/main/m4w10` t60);
     - `+6.33 %` at t45 (width ratio 1.5);
     - `+4.40 %` on `density_pairs/d600` t3;
     - slopes `+0.1533`–`+0.4021 %/px^2` on all 19 `pairs` families, every one crowded by the annulus definition
       (`grid/iso` 25 px separation, `grid/main` none);
     - `+0.0040 %/px^2` on the isolated `truthcheck` (120 px).
   - Real data (section 4.2), at width ratios up to 1.748 and 1.761 with 50 of 91 and 49 of 82 members crowded:
     - F1 median gaps `-0.108 %` and `+0.084 %` against the harness truth, inside the soft 1.38 % bar;
     - the high-ratio members read low (*derived* −1.076 % and −1.836 %).
   - Part 2 must establish which referee is right on these conditions before it changes, or keeps, the default.
   - Consumer: LN normalisation, so the master's photometry and its rejection.
3. **The captured fraction depends on the profile.**
   - Row 6: 18.73 % derived spread at F 2.6, 22.16 % at F 8.0 (from the printed medians).
   - Consumer: `PsfSignalWeight`.
4. **The frame FWHM moves with SNR, and Auto β flips.**
   - Row 11: `median 9.99 %` (`twins/sharp/m4/f2.0`, β flipped), `median 15.93 %` (`twins/sharp/m2.5/f1.6`),
     `median 2.93 %` with β stable (`twins/sharp/m4/f2.6`).
   - Consumer: Formula weights, the FWHM filters, the master FWHM.
5. **VNG per-plane width distortion.**
   - Row 8: G `+12.63 %`, B `-6.71 %` against ideal-plane `+3.06 %` / `+0.08 %`.
   - Injected real mosaics, plane 2: `-5.85 %`, `-6.29 %`.
   - Real-data LN: plane B is the weakest under F1.
   - Consumer: OSC filters, per-plane weights, master FWHM, F1's common F.
6. **Model-dependent widths (the S-2 ruling's numbers).**
   - Row 3: Gaussian and `blur2.5` profiles read `pointfit` low by `-2.07 %` to `-3.04 %` at F ≥ 2.0 (measure).
     Undersampled F 1.2 reads high, `+4.65 %` (m4) to `+21.38 %` (`blur2.5`).
   - β alone spreads the width by `median +9.60 %`.
   - Master-like `mixture/b` sits +4.88 % (derived) above `pointfit`.
   - Consumer: the FWHM filters and the master FWHM, once the owner rules on the width.
7. **Resampling broadens beyond the nominal kernel.**
   - Row 3: FWHM² growth `median 2.596 px^2` against `1.848 px^2` nominal (`resampled/f2.0`). The pointfit bias on
     the registered frame is `+25.54 %`.
   - Consumer: the master FWHM, the registered frames' weights.
8. **Background under nebulosity.**
   - Row 5: `bias +0.998 sigma · scatter 1.055` (`background/neb3_4096`).
   - Consumer: the PSF signal and every flux read from the fit.
9. **The Analysis path (part 3, listed here for completeness).**
   - Bright coverage `0.0 %`–`23.7 %` on the 24 `sharp` families at F ≤ 2.6: of 300 on 23 of them; `sharp/m2.5/f2.0`
     reads `0.7 % of 600`.
   - Round-star floor `0.1959`–`0.3635` on the same families.
   - Captured fraction above 1 with large scatter at F 2.0–2.6.
   - `sharp/gauss/f8.0` width `-4.95 %`.
   - On the real corpus (comparison only), the measure path reads defocused frames far below the analysis path and
     the ERP.
   - Consumer: the Analysis tab.
