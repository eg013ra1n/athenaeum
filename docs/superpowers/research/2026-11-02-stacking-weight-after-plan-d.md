# The stacking weight after Plan D: a frame-common star set (2026-11-02)

Plan D of the star measurement program (plan `docs/superpowers/plans/2026-10-31-one-star-path-plan-d.md`, mechanism research
`docs/superpowers/research/2026-10-31-plan-d-stacking-weight-mechanism.md`, scope «Только вес стека», owner 2026-10-31) ends here. This note sets the
code Plan D leaves behind (athenaeum `plan-d`, runs at **f899a691**, `MEASURE_VERSION` 8) beside the dbase it started from (a90e140b, the shipped Plan C):
what the weight reads now, what it did at master level on three real trees and on set 109, the owned quantities at close, the recorded limits with their
owners and the suites. No run was made for this note.

How to read the numbers:
- **Copied as printed.** Every number is copied from the file named in its section. A statement this note derives from two printed files is marked *(d)*.
- **Before.** "dbase" is Plan D's base: the three real trees run at a90e140b (hc11 run 6, runB run 7, s204 run 6), the lab cycle at a90e140b against Plan C's final
  state. "d1 … d3" are Plan D's own stage bases (kept as stripped comparator kits); "final" is f899a691 on the merged head (`main` e31a54c1 included).
- **Sources.** **F/** = athenaeum-lab `star-lab/results/plan-d/final/` (lab 975625f for the ln-truth, 91bf162 for the cycle), **S/** = `…/plan-d/set109/` (lab fbdfecb),
  **L** = the Plan D ledger `.superpowers/sdd/2026-10-31-plan-d/progress.md`, **R** = `rulings.md` in the same folder (PD-1 … PD-49), **RM** =
  `docs/stacking/README.md` R-OSP-18. The task and ruling files named in R live there too (local, outside the repository). **OI** = `docs/superpowers/open-items.md`.

## 1. For the astrophotographer

A sub's stacking weight used to count the light of every star the Measure stage fitted, and the few clipped (saturated) stars carried 60-82 % of it; the light
given to a clipped star is extrapolated from its wings and grows with the blur. So a soft sub was credited for light it did not have, and on set 109 the top
weight went to one of the widest frames. The weight now counts the same unclipped stars in every sub of the group and ignores the rest: it follows the sky's
transparency and the sharpness of the frame, nothing else. On the three real trees the sharpest frames stay on top (the sharpest decile's worst rank 9 of 73 on
hc11 L, 26 of 101 and 26 of 105 on the two colour sets) and the mono masters get sharper at slightly higher noise (hc11 L: FWHM -1.4 %, noise +2.0 %, PSF SNR +1.4 %; runB mono L's PSF SNR +27 %, mostly
the rejection change of R-SC-1, which the replay against d3 separates). On set 109 the top-weight frame is a sharp one (FWHM 1.65 px against a group median of 2.61) and the SNR
gain reads 8.05 for the mono group (run 9: 7.08) and 10.66 / 13.45 / 10.76 for the colour group (7.41 / 7.99 / 7.86). The cost: on the colour cameras' red plane the
master realizes none of the sharper weighting's star-peak gain (sections 4 and 7), and the first stack after the update re-measures every set once.

## 2. Data lines

| job | verdict | athenaeum | lab | machine |
| ---- | ---- | ---- | ---- | ---- |
| dbase `before` | COMPLETE (hc11 run 6, runB run 7, s204 run 6) | a90e140b | ada544e, 52f44c9 | dev Mac |
| dbase `cycle` vs c10b-final | PASS (noreg 0, B4, F-11, coverage 100 %, inject 109 / 109) | a90e140b | fa4d97c | runner |
| D1a `common` | PASS (20 lines, 0 missed) | d50be798 | cbde868 | dev Mac |
| D1m `ln-truth d1` | STOP -> PD-21 … PD-24; replay `bars d1` PASS; hc11 r2 DONE (B-E EQUAL 106 / 106) | 62d7017d, ee8da62e | 31dd1c0, 440962a, 99caa40 | dev Mac |
| D2 `cycle`, `ln-truth d2` | cycle PASS; ln-truth FAIL (OSC B-S rank 53; mono EQUAL) -> PD-26 … PD-28 | 5f3bfba8 | 84f4afe, fbfff03 | runner, dev Mac |
| D2b offline gate | STOP (G1 +7.95 / +6.86 %/px; G2 0.9816 / 0.9834) -> PD-29, PD-30 | 94f90476 | gate report | dev Mac |
| D2c gate, `cycle`, `ln-truth d2c` | gate PASS; cycle PASS; ln-truth STOP (mono noise 1.025, drizzle 1.0134) -> PD-32 … PD-35 | 68ec8758 | 67f64eb, d1b14c3 | dev Mac, runner |
| D2d `ln-truth d2d` | PASS x3 (mono bit-identical to d1) | fdafa818 | 7f1c1e3 | dev Mac |
| D3 `cycle`, `ln-truth d3` | cycle PASS (385 rows in 10 Blend families, all explained); ln-truth PASS x3 | 83cde199 | cafb7d1, 65acffa | runner, dev Mac |
| final `cycle final --prev d3` | PASS (noreg 0, cumulative 0, control 0; 925 s) | f899a691 | 91bf162 | runner |
| final `ln-truth final` vs dbase | FAIL: six cells, all ruled (PD-41 … PD-43) | f899a691 | 975625f | dev Mac |
| final the same, replay vs d3 (R-SC-1 alone) | PASS | f899a691 | 975625f | dev Mac |
| set 109 `set109` | harness FAIL (psf-slope; snr-gain after a WAL-aware read) -> PD-46 … PD-49 | f899a691 | fbdfecb | dev Mac |
| set 109 the re-read of run 9 (checkpointed copy, PD-46 / PD-48 / PD-49; no new stack) | PASS (psf-slope: the PD-47 limit on B; snr-gain 0.79 / 0.81 / 0.78 / 0.79 of the ceiling) | f899a691 | f011b9e (tools cecbbb1) | dev Mac |

## 3. What the weight reads now

Sources: R (PD-1 … PD-6, PD-30, PD-32), RM.
- **The set.** Per (group, plane), after the persisting registration pass, from the `.athf` fits Measure persisted mapped through each member's stored registration
  map (no pixel is read): the anchor is the lowest-FWHM member at or above the median star count; candidates are its unsaturated, isolated, SNR ≥ 20 fits; a star is
  a member when ≥ 90 % of the included aligned members hold it and the set has ≥ 20 stars, else the plane falls back whole (PD-6). Each frame's flux is a paired ratio
  to the anchor's, so the weight depends neither on itself nor on which frame is the registration reference (PD-1).
- **The flux term.** On a 3-plane image the sum of each star within 3 ring FWHM (`ap_sum`, `.athf` v3; the β-floor plane's model misses the wings; PD-30) with PD-9's
  rescued stars admitted (`StarFlags::SIBLING_POINT`); on a 1- or 2-plane image model × `c`, bit-identical to d1 / d2 (PD-32). A Blend-refused fit still isolates its
  neighbours (`StarFlags::BLEND_NEIGHBOUR`, PD-36).
- **The loop rule.** Stage 3's selection, the run reference and the two-pass pick keep the Measure-time weight and its floor; `min_weight_fraction` is applied to the
  common-set weight, never re-admitting a frame (PD-5, PD-17, PD-18). The registration reference stays on the Measure-time weight (PD-11).
- **Every mode.** The set is built in every weight mode, for the SNR gain; outside the PSF family the weights, `psfSnr` and the stored `metrics` are bit-identical to stage 3's (PD-19 b).
- **`c` and Blend.** `c`'s colour sample keeps point-like stars the knot flag took (PD-9): R's `c` sample depth p50 65 (D0: 219 / 222), kept / top 0.996, `c` p50 1.815 -> 1.231.
  The stacking Measure refuses Blend (PD-10): hc11 391, runB 68 361, s204 72 951 refusals (0.74 / 1.19 / 1.20 % of the fits).
- **Cost.** No loss anywhere: walls d1 415 / 690 / 691 s, final 384 / 690 / 691 s (hc11's +6.7 % over d3 is 24 s of start-up network waits before the run row; the DB duration is
  -0.6 %, PD-45); reweigh wall ≤ 2.3 s per 200 frames; the Measure probe 3.257 s per frame at D2c against 3.24 s at D2.

## 4. The final bars against dbase

Source: F/{hc11,runB,s204}/bars.txt, F/ln-truth-verdict.txt; replay F/replay-d3/. The bars: B-LN (PD-8, PD-22), B-M (PD-21, PD-24, PD-27, PD-33), B-W (PD-23), B-S, B-R, B-E.

| dataset | B-LN | B-M (PSF SNR min / FWHM max) | B-W | B-S (median / worst; dbase) |
| ---- | ---- | ---- | ---- | ---- |
| hc11 | MISS one cell: L p0 residual +0.0495 against 0.0452 (PD-41) | PASS 1.0061 / 0.9859 | PASS (L 0.9654; G 0.8752 by PD-23's escape) | L 4.5 / 9 of 73 (5.5 / 13); G 8 / 9 of 28 (13 / 16) |
| runB | PASS 0 of 4 | MISS OSC p0 0.9831, p2 FWHM 1.0103 (PD-42); mono L 1.2718 | PASS (mono 0.9787, OSC 0.9065) | OSC 13 / 26 of 101 (bar ≤ 28); mono L 5 / 10 of 83 (5 / 10) |
| s204 | MISS one cell: osc p2 k +0.202 -> +0.823 %/px, residual +0.0029 (PD-43) | MISS OSC p0 0.9882, drizzle p0 0.9883 (PD-42); the other six 0.9968-1.0065 | PASS (mono 0.9750, OSC 0.9174) | OSC 13 / 26 of 105 (bar ≤ 26); mono 5.5 / 10 of 92 (5.5 / 10) |

- **B-R** (hc11 L's eight C10 r1 frames, rank after / before): 69976 25 / 9, 69705 67 / 64, 70072 29 / 24, 69860 6 / 2, 69865 12 / 4, 69947 18 / 11, 69949 28 / 30, 69859 41 / 46 (reported).
- **R-REF** (PD-11; manual pins, unchanged): hc11 69845 FWHM rank 26 / 73, weight rank 20 (dbase 23); runB 123 35 / 101, 19 (23); s204 28949 22 / 105, 3 (1).
- **R-LN** (PD-8): s204 OSC p0 paired median +0.3380 pp against dbase (n 105, 2 SE 0.1585) beside C10 r1's -0.0220 pp; the bar's dF-neutral residual median is -0.0014 pp (k -1.28 -> -0.96 %/px).
- **common_stars at final = d3**: hc11 G 422, L 295; runB mono L 10 429, OSC 5763 / 7482 / 5482; s204 mono 10 137, OSC 5699 / 7425 / 5433.
- **The replay (final vs d3, R-SC-1 alone):** PASS on all three; B-W 1.0000, B-E 0 of 106 / 184 / 197 frames differ, B-M PSF SNR min 0.9942 / 1.0002 / 1.0000. s204 final is bit-identical to d3.
- **The DF acceptance rule (fable, PD-41 … PD-43):** all D bars against dbase PASS except hc11 L B-LN (PD-41), OSC p0 B-M x3 + runB p2 FWHM (PD-42), s204 osc p2 k (PD-43). No bar is
  redefined: a stage floor is not transitive, and a cumulative miss is attributed by the stage chain and ruled, never re-floored.
- **The chains.** OSC p0 PSF SNR runB: D1 0.9942 × D2 0.9949 × D3 1.0000 × R-SC-1 0.9942 = 0.9835; s204: 0.9943 × 0.9932 = 0.9875 (drizzle 0.9917 × 0.9965 = 0.9882). runB p2 FWHM:
  1.0022 × 1.0015 × 1.0065 = 1.0102. hc11 L B-LN: D1 +0.0468 against 0.0483 (PD-24's closest cell), d2d = d1, D3 -0.0001, final = d3. The weights predict a gain on p0 (+0.20 % runB,
  +0.19 % s204) and sit within 0.4 % of the plane's PSF-SNR optimum (ρ(w, w*) 0.961 -> 0.989); the master realizes the predicted noise and none of the +3.2 % star-peak gain.

## 5. Set 109

Source: S/ (checks.txt, groups.tsv, set109-verdict.txt of the lab f011b9e re-read; wal-aware-recheck.txt of the first read, lab fbdfecb), `task-DF-set109-report-r1.md`, PD-46 … PD-49. Dev Mac, release `athenaeum-web` on a copy of the dev catalog,
1212 s wall; the copy `~/.athenaeum-acc/plan-d/set109` (141 GB) is kept to 2026-12-07. The run ran `local` rejection (the copy's stored `scaleZeroOffset` was migrated).
- **Passes as printed.** β recorded (mono 4.077, OSC 2.305); the sharpest decile's median rank 15 of 208 and 8.5 of 160 (limit 0.2 N); the top-weight frame's FWHM 1.65 against 2.61 (mono)
  and 1.33 against 2.47 (OSC); reweigh floor drops 0 of 208 and 0 of 160; integration floor drops 0. The two-pass pick moved the registration reference from 29080 (FWHM 4.869 px) to 29375 (1.723 px).
- **The misses as the plan wrote the bars (first read, lab fbdfecb).** psf-slope: mono +7.7, OSC +55.4 %/px (R / G / B +41.4 / +67.5 / +47.4) against ±8; snr-gain: G 13.45 above √160 = 12.65 once the server's
  un-checkpointed WAL was read (the harness had opened the copy `immutable=1` and judged mono alone; PD-49).
- **The sky against the flux (PD-46).** `psfSnr` = K · `commonTflux`² / `noise`², so ln psfSnr's slope is 2 · (flux slope - noise slope): OSC +41.4 / +67.5 / +47.4 =
  2 · (+1.2 / +5.6 / -7.3 - (-19.5 / -28.2 / -31.0)). The group is two nights, 2025-09-13 sharp (FWHM 1.3-1.5 px) under a waning-gibbous Moon and 2025-10-18 wide (2.5-3.1 px) under a dark sky;
  night-pooled the noise slope is +1.8 / +2.1 / +3.7, so the sky step is the trend (mono's U-shape is its three nights). The weight's bar is the flux term against the run's own mono rig (same sky, a mono
  frame within 150 s; 140 of 160 OSC frames have one), night-pooled, |slope| ≤ 4 %/px (the mono group reads its own flux term): mono -2.2 ± 0.6, R +0.7 ± 0.6, G +1.8 ± 0.8 (PASS), B -6.2 ± 1.0 (the
  PD-47 limit: at or above 1.9 px B reads -2.6 over 89 pairs, below 51 pairs; the moonlit night's 55 frames sit 30-40 per plane at the 1.5 px field-width floor, B -20.3, R -5.8, G +2.3 %/px there
  against the dark night's B -2.6). The re-read prints `PLAND SET109: PASS` with that line (S/set109-verdict.txt, lab f011b9e).
- **The gain (PD-48).** The ceiling per plane is Σ w · SNR / (√Σ w² · SNR_b), b = `best_by_weight`'s frame: realized / predicted 10.66 / 13.45, 13.45 / 16.57, 10.76 / 13.76
  (OSC) and 8.05 / 10.16 (mono) = 0.79 / 0.81 / 0.78 / 0.79, one factor on four planes (no noise-correlation inflation: realized < predicted). The sharpest frame (28754) sits under the Moon (rank by psfSnr 106 / 121 / 111
  of 160), so √N, which presumes a top-SNR top-weight frame, is not the ceiling here; over the max-SNR sub the same master reads 8.40 / 9.17 / 9.29 (mono 9.28), all under √N.
- **PD-39 shares** on the run's four group-planes: mono 0.788, OSC 0.641 / 0.764 / 0.715, all above the 0.6 that fires now.

## 6. Owned quantities at close

The plan's table ("Owned quantities at close"), filled.

| quantity | owner | bar | after value |
| ---- | ---- | ---- | ---- |
| the frame-common star set (anchor, membership) | D1 (PD-3) | D1a: drift ≤ 4 %/px, noise median ≤ 2 % (PD-16; the plan's ρ ≥ 0.99 vs D0 (b) read as those) | D1a PASS, drift +2.16 / -0.96 / +1.22 / +1.45 / +1.34 %/px; final sets in section 4 |
| the weight's flux term (`commonTflux`, `commonTmeanFlux`) | D1 / D1w, D2c (PD-30), D2d (PD-32) | D bars; G1-G3 on OSC (PD-29) | section 4; D2c gate: G1 R / G / B +2.31 / +1.59 / +1.03 %/px, ρ(b-ap) 0.9931 / 0.9932, sharpest decile worst 26 / 26 |
| the star flux (model × `c`) | Measure (Plans A / B), read only | Plan B's bars | unchanged; 1- and 2-plane groups still weigh by it, bit-identical (PD-32) |
| `c`'s sample on a colour plane | D2 (PD-9) | cycle R13 0 over 2 SE (PD-28); real R depth / spread | depth p50 65 / 65 (≤ 80), kept / top 0.996 / 0.996 (≥ 0.9), R residual MAD 1.10 / 1.06 × G (≤ 1.5); R13 0 / 130 |
| the SNR gain and the master / best-sub PSF SNR | D1b | set 109 ≤ the run's own prediction (PD-48), above run 9's | set 109 mono 8.05, OSC 10.66 / 13.45 / 10.76; realized / predicted ≈ 0.8; `masterPsfSnr` a set-total (PD-35) |
| what a stacking fit accepts (Blend) | D3 (PD-10) | cycle Blend families; D bars | cycle d3: 385 moved rows in 10 Blend families, all explained; final: 0 moved against d3 |
| the registration reference | unchanged (PD-11) | reported FWHM rank | hc11 69845 26 / 73, runB 123 35 / 101, s204 28949 22 / 105 |

## 7. Recorded limits with owners

Sources: R, OI "Plan D", RM.

| limit | value | owner |
| ---- | ---- | ---- |
| the OSC R plane's master realizes none of the sharper weighting's star-peak gain (PD-42) | runB p0 PSF SNR 0.9831, s204 p0 0.9882 / drizzle 0.9883, runB p2 FWHM 1.0103; the weights predict +0.2 % | owner item: `core_replay.py` core-peak ratio per plane on the final clones |
| B / R below ≈ 1.9 px FWHM: the 3 F sum falls with FWHM (PD-47) | set 109's moonlit night, B -20.3 %/px | OI probe: D0's (b-ap) disc on ten 2025-09-13 B frames of the set109 copy (kept to 2026-12-07) |
| LN's scale against the flux truth on OSC planes, and hc11 L's 2024-10-05 night (PD-22, PD-41, PD-43) | s204 B k +0.82 %/px; hc11 L +0.0495 pp over 19 frames (CI includes 0) | none yet; the lever is LN's `referenceFrames` |
| LDN1272 mono loses about 2 % PSF SNR in the outer fifth while gaining 1-1.6 % at the centre (PD-24) | the favoured frames are sharper at the centre than at the corners | recorded for the owner |
| hc11 L's 2024-09-08 night (PD-9) | 8 of 73 frames, `c` +7 %, collimation off and wind (owner) | none |
| s204 OSC B-S on its bar (PD-34) | two decile frames (29002 at 26, 28977 at 25) read through the faint set's mean-flux term | none |
| a converged duplicate stays a Blend (PD-37); close unresolved pairs (R-PC-10a) | a few single stars with a stray second seed leave the statistic | OI Analysis items |
| the sharpest runB / s204 mono frames register fragilely onto an OSC reference (PD-39) | frame 162's ≤ 6.4 px map error is in every runB mono master `main` builds | the robust-seed plan `docs/superpowers/plans/2026-11-02-robust-quad-seed.md` |
| the registration reference stays on the Measure-time weight (PD-11) | rank in section 4 | none (ruled) |

## 8. Suites and reviews

Source: L (DF suites). Controller, dev Mac, single runs, at 584f6b35.
- Core `--all-targets`: **4807 passed / 1 failed / 17 ignored**; the one failure was `ts_contract` (DF-b's doc paragraph not yet regenerated into `src/types/stacking.ts`), regenerated in 174eea6f,
  PASS. Web 57 / 0; `--no-default-features` check Finished; vitest 120 files / 1146 tests; `npm run build` exit 0.
- The DF docs round (this note, RM, OI, CLAUDE.md, five doc-comment / test lines) re-ran `stacking::` 1037 passed, `ts_contract`, `vitest run src/components/stacking` 38 passed, `tsc --noEmit` clean.
- Reviews: an opus review per code task, each ending at an APPROVE (some after fix rounds and re-reviews); the DF code review over 32ab3c4b..f899a691: APPROVE (C0 / I0 / m2 / n3). The final
  whole-branch review (fable) and the merge follow this note.

## 9. What Plan D leaves

- OI "Plan D" lists the owner item (PD-42), PD-47's probe, the server's checkpoint-on-shutdown, the `fwhm_px` 0 on point-sampled fixtures, the `nch` / `channels` note, and the release notes owed.
- **The robust quad seed** (`docs/superpowers/plans/2026-11-02-robust-quad-seed.md`, RQ-1 … RQ-9; owner 2026-11-02 accepted RQ-5, RQ-6, RQ-7 and RQ-9) fixes the unclipped least-squares seed that
  mis-registers the sharpest mono frames (PD-39) and adds RQ-6's inlier warn, which replaces the PD-39 share warn as the map detector (PD-44). It lands after Plan D, moves runB / s204 mono registration rows, and
  bases its comparisons on the stripped `pland-<ds>-final` clones (RQ-9).
- A cheaper default lever for `local` rejection, the global pair scaled by `commonTflux` with no Normalize stage, is now buildable (OI, R-SC-1's leftovers); it is not built.
