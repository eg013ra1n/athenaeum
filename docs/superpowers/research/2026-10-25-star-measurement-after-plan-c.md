# Star measurement after Plan C: the Analysis tab on the one star path (2026-10-25)

Plan C of the star measurement program (spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` §4, plan
`docs/superpowers/plans/2026-10-24-one-star-path-plan-c.md`, Task C10) ends here. This note sets the code Plan C leaves behind
(athenaeum `plan-c` at **16c8e2aa**, `MEASURE_VERSION` 7) beside the legacy analyzer and Plan B's stacking numbers: the corpus,
the raw-mode bars, the plate-solve gate, the cost, the Blend refusal's story, the owned quantities at close, the recorded
limits with their owners and the suites. No run was made for this note.

How to read the numbers:
- **Copied as printed.** Every number is copied from the file named in its section. A statement this note derives from two
  printed files is marked *(d)*.
- **Before.** "Before" is the legacy analyzer (`ImageAnalyzer`, the C0 / C9 legacy arm) for the Analysis numbers, and Plan B's
  B6 for the stacking numbers (B6's lab state `t14-final` for the scorecard, the B6 clones at master level).
- **Sources.** **C10/** = athenaeum-lab `star-lab/results/plan-c/c10/` (lab 05e46a8), its index **AT** = C10/`after-table.md`
  (`C10 REPORT: COMPLETE`). **R1/** = `star-lab/results/plan-c/c10-r1-v6/` (the failed v6 run, lab 67f13ad, kept at 804c771).
  **L** = the Plan C ledger `.superpowers/sdd/2026-10-24-plan-c/progress.md` and **R** = `rulings.md` in the same folder; the
  task reports cited by name live there too (local, outside the repository). **RM** = `docs/stacking/README.md` R-OSP-17.
  **OI** = `docs/superpowers/open-items.md`.
- **The ERP.** "The ERP" is the external reference pipeline run on the same inputs: a comparison column, never a referee.

## 1. For the astrophotographer

The Analysis tab, Blink's star overlay and the plate-solve input gate now measure stars with the same code the stacking
Measure uses, instead of the old analyzer. Widths agree with the old figures within 2 % on mono sets; colour (OSC) frames are
measured on their green pixels, so their widths change in their own right (−10 … −27 % on the mosaics, closer to the ERP).
A frame is given a verdict (Trailed / Defocused / No stars) instead of a trail R², and the plate-solve gate refuses a Trailed
frame before solving. Two stars inside one fit's half-maximum contour are refused as a `Blend` in the Analysis tab and Blink;
the stack does not refuse them (section 7), so stacking's numbers are exactly Plan B's.

## 2. Data lines

| job | verdict | athenaeum | lab | machine | date (UTC) |
| ---- | ---- | ---- | ---- | ---- | ---- |
| C0 before (census, both arms) | 152 files read | 78e37a87 | 73cdce2 | dev (miniMac) | 2026-10-24 |
| C5L r2 raw-mode bars | RAW BARS: MISS 6 / 10 (R-PC-9a) | 6ee0d9f5 | 8fa3ff8 | dev | 2026-10-24 |
| C7D gate data | GATE RULING: REMOVE | eaa1d88a | 28130b2 | dev | 2026-10-24 |
| C9L corpus + cost | PLANC CORPUS: PASS; PLANC COST: MET | 0c59fad1 | c54afc9 | dev | 2026-10-24 |
| C9BL Blend proof | SYNTH STOP / CORPUS PASS / COST MET | 41ca7bad | 425db82 | dev | 2026-10-24 |
| C10 r1 (v6) | ln-truth FAIL, the rest PASS | 41ca7bad | 67f13ad (tools 9ccf343) | dev | 2026-10-24 22:13 – 23:36 |
| C10 r2 (v7) | every job PASS | 16c8e2aa | 05e46a8 (tools 804c771) | dev, /Volumes/BigMac | 2026-10-25 00:45 – 02:23 |

Every C10 run used `PLANB_GUARD_SUBMODULES=all` (someone else's `solvemyastro` test-fixture commit; L, C10T).

## 3. The corpus, before and after

Source: `c9-report.md`, `task-C9L-report-r1.md`, lab `star-lab/results/plan-c/c9/` and `c9b/verdict.txt`. 152 images × 2 arms,
304 / 304 read (the 5 XISF that failed both arms at C0 read now).

| set | n | FWHM new / legacy | ecc new / legacy | after Blend, FWHM / ecc against C9 |
| ---- | ---- | ---- | ---- | ---- |
| m101 | 30 | 0.996 | 1.050 | 1.0000 / 1.0000 |
| m42 | 76 of 77 | 1.019 | 1.099 | 0.9998 / 1.0000 |
| singles | 38 | 1.006 | 1.074 | 1.0000 / 1.0000 |
| trails | 3 of 7 | 0.999 | 1.019 | 0.9996 / 0.9995 |

27 outliers, each explained (`task-C9L-report-r1.md` "Outliers"): 9 legacy trailed false alarms now clear, 5 hard-trailed frames
with no fit (all Trailed), the OSC mosaics measured on green sites, defocused m101 blobs (every reader disagrees), near-starless
frames. The legacy arm drifted from C0 on `_DSC5767.fit` only (the XBAYROFF fold). The C10 r2 Analysis path is C9BL's
(`Mode::Budget` is untouched by R-PC-10b) *(d)*, so the last column stands at the final code.

## 4. The raw-mode bars

Source: `task-C5L-report-r2.md` (lab 8fa3ff8, `c5-r2/raw-bars-misses.txt`), AT "Raw-mode bars". Bars: completeness ≥ 99 % of the
injected stars above the threshold (R-PC-6 / R-PC-7), FWHM ≤ 1 % at F ≥ 2.
- **C5L r2: completeness a RECORDED MISS (R-PC-9a, the owner's word)**: 6 of 10 entries short by 1–3 of 45–60 stars, all in R-PC-9's
  cells (CFA at native F < 3 px: ≤ 0.90 F stars not seeded or `centreMoved`; mono at F 1.6–1.9 px: `narrowerThanPixel` /
  `centreMoved`); 0206 / 0013 / 0004 / 0015 pass. The FWHM bar passes 10 / 10 (worst −0.73 %, 0004).
- **C10 r2: RAW VS C5L PASS**: every entry at its C5L r2 count (found below 0, above 0), refused Blend 0, FWHM bar 10 / 10. The
  completeness miss is unchanged; no Plan D item.

## 5. The plate-solve gate

Source: `task-C7D-report-r1.md`, lab `star-lab/results/plan-c/c7/` (`verdict.txt`, `gate-frames.tsv`). 95 frames, one per batch: 86
solvable, 9 hopeless; 4 of the 9 carry `frame_verdict = trailed` and fail in 0.8–1.3 s on the solver's own refusal; no solvable frame
carries the verdict (STOP check PASS). No (ecc, R²) pair refuses a candidate without refusing a solvable frame (`ldn621`, ecc 0.883 /
R² 0.865, solves) → **REMOVE** (PC-12): the gate is the verdict alone. Let through, 455 s of solve wall, no rule invented (PC-14):
M101 `…_020058_0002` (89.1 s), `…_020531_0004` (86.0 s), `…_021623_0009` (80.1 s), `_VSH8902.xisf` (122.8 s, no Analysis row), H 0029
(77.0 s, tracking jumps). The verdict gate saves the 4 trailed frames' 4.2 s.

## 6. The cost

Source: `c9-report.md`, `c9b/verdict.txt`. Bar 0.6 s per m42 frame. **MET**: m42 measure median 0.387 s (p90 0.409) at C9, 0.385 s
(p90 0.402) after Blend; OSC singles 0.279 s, mono singles 0.504 s; Blink m42 call 0.404 s at C9 (measure 0.387 + read / persist 0.018,
warm cache; not re-timed after Blend, OI Plan C smoke). Dev profile: rustafits at opt-level 2 (d811c054) took the dev Analysis from
6.82 to 0.36 s per 3124×2088 frame.

## 7. The Blend refusal, from the owner's look to the Analysis path only

Sources: R (R-PC-10, -10a, -11, -10b), `task-C9BL-report-r1.md`, `task-C10F-verdict-r1.md`, AT, R1/.
- **R-PC-10 (owner, C9 app look):** an accepted fit with another seed inside its half-maximum ellipse, ≥ 0.5 F from its own seed, is
  refused `Blend`, on every path; `MEASURE_VERSION` 5 → 6 (16c0a2ec / 41ca7bad).
- **C9BL (lab 425db82):** synthetic judgement STOP (`doubles/f4.0` 4.356 → 4.353 against truth 4.062: the companion at 0.5–1.25 F is
  usually no seed, 95 of 132), corpus PASS (ratios above, Blend ≤ 3 % of fits, no verdict change), isolated families identical, cost
  0.385 s. **R-PC-10a (owner):** ship as built; close unseeded pairs → Plan D.
- **C10 r1 at 41ca7bad (R1/): ln-truth FAIL.** The cycle passed under R-PC-11 (a) (noreg moved 1012 rows in 14 families, every one with
  Blend refusals), inject-raw and gates passed, but at master level hc11's weights read ρ 0.891 with 19 unexplained movers, one L master
  plane median −2.48 % / MAD −4.09 %, runB OSC plane 2 MAD −1.17 %, GROUPS WORSE 1 / 0 / 3. Blend refused ~1.6 % of stacking fits (hc11
  391, runB 68 361, s204 72 951 refusals): resolved pairs, not a misfire. The move came from `tflux`: it sums every accepted fit,
  saturated included, and saturated stars hold 53–64 % of a frame's signal, so a few saturated refusals re-ranked the weights against
  seeing (hc11's sharpest frames demoted, master noise +1.59 %); head and B6 equidistant from the photometric truth.
- **R-PC-10b (fable verdict B):** Blend on the Analysis path only (`Mode::Budget`; one bool, `fit::Correction::blend_refusal`); the
  stacking Measure, LN's refits and registration fit as at v5. `MEASURE_VERSION` 6 → 7 (6 burnt). Code 16c8e2aa.
- **C10 r2 at 16c8e2aa (C10/): identity is back.** Cycle noreg vs t14-final moved 0; ln-truth EQUAL to B6 on all three trees
  (measure-equal 0 of 106 / 184 / 197 differ, Blend 0, ρ 1.0000, masters PASS, GROUPS WORSE 0); gates 13 / 13; inject-raw as C5L r2.

## 8. Owned quantities at close

The plan's table ("Owned quantities at close"), filled.

| quantity | owner | bar | after value | source |
| ---- | ---- | ---- | ---- | ---- |
| raw input domain (clip, full scale, gain, signedness) | C2 (PC-2, PC-3; R-PC-1 / -1a / -2) | C2 tests, the census | 304 / 304 corpus reads; the one file with > 1 % negatives (`_DSC5767.fit`, 99.8 %) read at BZERO 32768 by rustafits 1.6 | C0 census, C9L |
| the CFA plane's sky and noise | C3 (PC-4) | C3 tests; raw-mode bars | FWHM bar 10 / 10 at C10 r2 (worst −0.73 %); one green phase, M7 recorded | AT, C5L r2, RM C3-review |
| the CFA seeds and their field width | C3 (R-PC-3, R-PC-4) | completeness ≥ 99 % | recorded miss (R-PC-9a): mosaics 44 / 44 / 45 of 45, RGGB 43 / 45 / 42 / 45 of 45, unchanged at C10 r2 | AT, C5L r2 |
| the frame statistic (FWHM, ecc, SNR, HFR) | Plan A (module), read only | Plan A's held bars; C10 identity | cycle noreg moved 0, B1 coverage 100 % (156 planes, 0 below), ln-truth EQUAL to B6; corpus FWHM new / legacy 0.996–1.019 | AT, C9L |
| trail R² | C4 (PC-6) | C4 tests; C7 | stored for compatibility; the gate does not read it (REMOVE); the Verdict column replaces it in the UI (PC-15) | C7D, C8 |
| the frame verdict | Plan A (F-10), C6 stores it (PC-13) | C7: no solvable frame carries Trailed | 0 of 86 solvable; 4 of 9 hopeless refused; 5 zero-fit corpus frames all Trailed; 9 legacy false alarms clear | C7D, C9L |
| the gate's ecc / R² pair | C7 (PC-12) | keep only with a separating pair | REMOVED: no pair on 95 frames | C7D |
| `c` on a CFA plane | PC-4 (= 1) | recorded limit | 1 by construction (M3: one `warn!` per OSC plane) | RM C3-review |

## 9. Recorded limits with owners

Sources: OI "Plan D — carried from Plans B and C" and "Plan C" smoke list, R, RM.

| limit | value | owner |
| ---- | ---- | ---- |
| the stacking weight hangs on saturated stars' `tflux` (C10F) | saturated stars 53–64 % of a frame's signal; r1: hc11 ρ 0.891 | Plan D (pre-Plan-C fragility); bar: R-PC-11 (b) ln-truth |
| Blend not on the stack (R-PC-10b) | the blends' 0.1–0.4 % FWHM and crowded-field ecc bias stay in stacking; the Analysis star list ≤ 2.3 % shorter | Plan D, after the weight item |
| close unseeded pairs (R-PC-10a) | 95 of 132 on `doubles/f4.0`; FWHM +7…+14 % at 0.5–1.25 F | Plan D: a residual second-peak test |
| the mild-trail class | `ldn621`, ecc 0.88, no verdict, solves | Plan D |
| near-starless frames report a FWHM without No stars | m82, singles 0008 | Plan D |
| 8 files without IMAGETYP never get an Analysis row | `_VSH8902.xisf` and 7 more | Plan D |
| hopeless frames the gate lets through (PC-14) | 5 frames, 455 s of solve wall | Plan D: a new signal |
| CFA completeness at native F < 3 px (R-PC-9 / -9a) | 6 of 10 entries short by 1–3 stars | none (owner's word) |
| OSC Analysis star flux at `c` = 1 (PC-4); no field-width median on a debayered plane (PC-5) | uncorrected / silent | recorded only |
| Blink HTTP cost after Blend | 0.404 s at C9, not re-timed | OI Plan C smoke |

Plan B's items moved to Plan D unchanged (the four `c` items, F1's size gap, psfSel's entry rule, PSF signal on deep nebula,
the ERP refresh); see the after-Plan-B note §6.

## 10. Suites

Source: L ("Step 2 suites", "C10 r2"), `task-C10T-report-r1.md`, `task-C10R-review-r1.md`. Controller, dev Mac, single runs.
- Core all targets `--no-fail-fast`: **4201 passed / 0 failed / 17 ignored** at 16c8e2aa (4197 / 0 / 17 at 41ca7bad); the dev Mac
  runs the network modules too, no split.
- At 41ca7bad: `cargo test -p athenaeum-web` 53 / 0; `cargo check -p athenaeum-core --no-default-features` Finished; vitest 88
  files / 930 passed.
- Lab (C10T, 9ccf343): star-lab 1556 OK, harness 468 OK (2 skipped).
- Final whole-branch review (fable, at 41ca7bad, before the runs): READY, 0 critical / 0 important / 5 minor; m1–m4 folded in
  e2ae53ff, m5 = RM R-OSP-17.
