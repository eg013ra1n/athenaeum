# Star measurement after Plan B: the aperture correction kept and screened, the LN estimator kept (2026-10-24)

Plan B of the star measurement program (spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md`, plan
`docs/superpowers/plans/2026-10-20-one-star-path-plan-b.md`, Task B6) ends here. This note sets the code Plan B leaves behind
(athenaeum `plan-b` at **5103310e**) beside B1's baseline: the owner's rows before and after, every bar as judged, the real
trees, the suites, the recorded limits with their owners, the owned quantities at close and what Plan C needs. No run was
made for this note.

How to read the numbers:
- **Copied as printed.** Every number is copied from the file named in its section, as that file printed it. A statement
  this note derives from two printed files is marked *(d)*.
- **Before.** "Before" is B1 unless a row names another base: the lab's base scorecard `t14-final` (Plan A's final lab
  state, which B1 re-baselined), B1's real-tree reads, or, for the master-level runs, B4's after-runs.
- **Sources.** **B6/** = athenaeum-lab `star-lab/results/plan-b/b6/` (lab main 60a5fc3), its index **AT** = B6/`after-table.md`
  (`B6 REPORT: COMPLETE`). **L** = the Plan B ledger `.superpowers/sdd/2026-10-20-plan-b/progress.md` section "B6" and
  **R** = `rulings.md` in the same folder (both local, outside the repository: the suites' counts and the B6 rulings live
  only there until the README's R-OSP-16 B6 block carries them). **RM** = `docs/stacking/README.md` (R-OSP-16).
  **OI** = `docs/superpowers/open-items.md`.
- **The ERP.** "The ERP" is the external reference pipeline run on the same inputs: a comparison column, never a referee.

## 1. For the astrophotographer

When Athenaeum stacks, it measures every star with a fitted model, and a model never catches all of a star's light: some
of it sits in the far wings. The **aperture correction `c`** is that missing share, measured on each frame: how much more
light a wide circle around the star collects than the model says. It feeds the frame weights (a frame whose stars look
fainter weighs less) and the order in which stars are picked. Plan B changed four things about it and one thing about how
frames are scaled to each other.

- **Each colour plane keeps its own `c`.** R, G and B are measured separately, keep their own `c` (with its uncertainty and
  the number of stars behind it) in the frame's measurement, and are weighted by their own value. The run log prints one
  median `c` per group and colour, so a strange plane is visible.
- **Hot pixels are no longer taken for stars in `c`'s sample.** On a colour frame a "star" that has no counterpart in the
  other colours is refused before `c` is measured, and only stars bright enough to measure well (signal-to-noise 20 or
  more) count. Before this, on a raw colour frame with an uncorrected red plane, 36 of the 64 stars behind red's `c` were hot
  pixels and red's weight dropped to × 0.40; now none of the 33 stars behind that plane's `c` is a hot pixel, and red's `c`
  moves with green's and blue's to within 0.17 % (OI, "The Measure aperture correction `c` on undersampled OSC planes").
- **A plane whose `c` cannot be measured** (fewer than 5 usable stars) now takes the median `c` of the other frames of its
  group on that colour, when at least two of them carry one, instead of falling back to 1 (no correction at all).
- **The first stack after the update re-measures every frame once.** The measurement's version number moves from 4 to 5
  (`MEASURE_VERSION` 5), so the first stack after the update re-measures, re-registers and re-normalizes every set once; it
  never re-calibrates (RM, R-OSP-16 B4).
- **Local normalization keeps its estimator.** Local normalization (LN) scales each frame's brightness to the reference
  frame's by comparing the same stars in a common circle. That default, shown in the Normalize panel as Star flux →
  "Common aperture" and called F1 in the lab, stays. The alternative, "PSF signal", reads 2–8 % low on star pairs whose
  sharpness differs from the reference's, so it stays an option, not the default (OI, "The LN estimator, Plan B B3").

On the three real trees (set 204, run B and the Horsehead C11 set) none of this moved anything: every frame's weight keeps
its rank (Spearman ρ 1.0000) and every master is identical to the one B4 made (section 4). The changes act where the
defects were: hot pixels in a raw colour plane, a plane with too few stars, and the lab's planes where `c` was off.

## 2. Data lines

One per job, as AT prints them (athenaeum sha, lab sha, machine, volume, date in UTC). The product code is the same in
every line: 5103310e (B6's code against 13aee664, B4's last product commit: the probe's `cRef` / `cTgt` keys, doc comments
and one test; L).

| job | verdict | athenaeum | lab | machine | volume | date (UTC) |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| cycle | B6 CYCLE: PASS (every bar) | 5103310e3737 | 4e4e757 | runner (SADM-OBE-47e52) | /System/Volumes/Data | 2026-10-23 18:53:43 |
| l10b | B6 L10B: PASS | 5103310e3737 | d44e3bb | runner (SADM-OBE-47e52) | /System/Volumes/Data | 2026-10-23 18:56:50 |
| ln-real | B6 LN-REAL: PASS (a read) | 5103310e3737 | 886d6a8 | dev (miniMac) | /Volumes/BigMac | 2026-10-23 20:47:17 |
| ln-truth | B6 LN-TRUTH: PASS (reported, never a bar) | 5103310e3737 | 087a22f | dev (miniMac) | /Volumes/BigMac | 2026-10-23 21:41:30 |
| gates | B6 GATES: PASS (gates 13 / 13) | 5103310e3737 | 8e9f305 | dev (miniMac) | /Volumes/BigMac | 2026-10-23 22:13:40 |

The scorecard identity (b6-final against B4's b4-c2, on the runner's stored scorecards, lab d44e3bb) and the suites (section
5) have no line of their own in AT; their machine is named where they are cited.

## 3. Every bar as judged

Sources: B6/`cycle/bars.txt` (the cycle's BAR line and each section), B6/`cycle-verdict.txt`, B6/`l10b-verdict.txt`,
B6/`ctable-verdict.txt`, B6/`gates-verdict.txt` and B6/`gates.txt`, B6/`ln-real-verdict.txt`, B6/`ln-truth-verdict.txt`,
B6/`identity-vs-b4c2.txt`. The cycle's base is `t14-final` = B1; its design list is B0's ∪ B4's (the families B4 moved by
design are set aside in the diffs and judged by B4's bars).

| bar | before (B1 unless named) | after (B6) | verdict | owner task |
| ---- | ---- | ---- | ---- | ---- |
| F5.1 row 6 captured-fraction spread of the six sharp families, f2.6 (≤ 2 %) | +0.76 % | +0.76 % | PASS | B4 (F-12) |
| F5.1 the same at f8.0 (≤ 2 %) | +1.41 % | +1.41 % | PASS | B4 |
| F5.2 psfSel blur pairs at t60 (± 2 %) | blur25mix +1.72 %, blur4 +1.19 % | +1.72 %, +1.19 % | PASS | B4 |
| F5.2 / F5.3 psfSel cells (pure Moffat / Gaussian targets ± 1.2 %), 12 families | per cell (e.g. `g-w5` t60 −1.07 %) | miss 0 of 60 (`g-w5` t60 −1.06 %) | PASS | B4 |
| R13 (STOP) row 6 cells with B1's median in 0.9865–1.001 stay within 2 SE | 130 cells in the window | 0 of 130 beyond 2 SE | PASS | B4 |
| HALO row 6 against truth 1.0 (never farther than B1 beyond 2 SE) | 0.9984 (SE 0.00062) | 0.9984 | PASS | B4 |
| REFUS the point-source screen on the clean colour families (a family misses only if the frame lines and the offline replica both flag it) | none (the screen is B4's) | 10 of 10 families ok (replica: 0 of the fitted SNR ≥ 20 stars refused); mono planes with a screened candidate 0 | ok on every row | B4 |
| FLOOR (i) planes (reported) | 0 of 396 | 0 of 396 | reported | B4 (R-PB4-5) |
| NEB row 6 on `background/neb*` (reported) | neb0.5 0.9920, neb3 0.8801 | 0.9920, 0.8801 | reported | B4 / B5 |
| F-11 no-regression, head vs `t14-final` | `t14-final` vs t14b: all 0 | moved 0, one-sided missed 0, lost 0, control 0 | PASS | B0 |
| F-11 no-regression, cumulative vs `t01c-baseline` | `t14-final` vs t14b: all 0 | moved 0, missed 0, lost 0, control 0 | PASS | B0 |
| Scorecard identity vs B4 (b6-final vs b4-c2, no design list, 73 403 rows) | b4-c2 | moved 0, max \|Δ\| 0 | identity | B4 / B6 |
| Probe and inject proofs | B0: 1041 / 1041, inject 109 / 109 (RM) | 1041 / 1041 proven; inject 109 / 109, 3 328 rows, 0 missing | proven | lab |
| HELD t10-f5 (F-5) items 1–4 and the control | t09-f4 spreads +0.76 / +1.41 % (t01c +18.72 / +22.15 %) | +0.76 / +1.41 %; pure-profile 0 of 70 over 1.2 % (worst +1.06 %); mixtures 0 of 20; control bit for bit | PASS | Plan A (held) |
| HELD t10b-onebg D.1–D.8 | base, e.g. D.1 neb0.5 recall 489 / 629 = 0.777 | 489 / 629 = 0.777; every row as at the base | D ROWS: PASS | Plan A (held) |
| HELD t11-f6 T11.1–T11.6 | absolute bars (no base column) | T11.1 flat −0.03 %; 50 pull cells over 18 families in [0.85, 1.3] | T11 ROWS: PASS | Plan A (held) |
| HELD t13b-final T13B rows | t13b-base: coverage 100.0 %, 156 planes, 0 below; 158 of 158 | 100.0 %, 156 planes, 0 below; 158 of 158; measure outcomes 192 of 192 | T13B ROWS: PASS | Plan A (held) |
| Gates (13, fixed answer 13 / 13) | B0's set 204 run: 13 / 13 | 13 / 13: determinism 185 / 185, `probe_parity` 20 010 fits, 200 100 fields, 0 mismatches | PASS | lab (R-PB6-2) |
| l10b R-PB5-2: F1's 34 nebula-sky cells not worse than B1's beyond 2 SE | 4 of 34 over the F-13 bar | 4 of 34; 0 worse, 0 lost; 36 cells SAME AS B5 | PASS | B5 |
| l10b psfSel on the same cells (reported, never a STOP) | 13 of 34 (B1 binary) | 15 of 34 | reported (R-PB5-3) | F-10 / B.3 |
| c-table: Measure's `c` against truth on the 60 l10b planes | median \|c − c*\| 0.36 % | 0.36 %; closer 1, farther 0, same 59; 105 cells SAME AS B5 | reported (R-PB5-3's standing check) | B4 |
| ln-real on the three trees (a read) | B1 per tree | every tree equal to B1 (section 4) | PASS (a read) | B3 |
| ln-truth, master level on hc11, run B, set 204 (reported) | B4's after-runs | EQUAL on all three; ρ 1.0000; 0 movers; masters 1.000000 | PASS (never a bar) | B3 / B4 |

The held chain as it ran is Plan A's `t13b-final` bar script, which chains `t11-f6`, `t10b-onebg` and t10-f5's C1 rows;
every other scorecard row of F-0…F-7 and F-10 is held by the two no-regression diffs (moved 0 against B1 and against t01c),
except the design list's set-aside rows, which B4's bars above judge.
Reported beside the bars (cycle-verdict): debt vs t01c OPEN on fwhm scatter (measure, pointfit), centroid scatter (measure,
register) (Task 14's judgement, unchanged); same-star WORSE bins none; truth sanity 3 cells, 3 explained, 0 new; the ERP
15 of 204 units due a refresh (stale 0, new-since 0, no file 15).

Data: cycle, l10b and gates lines (section 2).

## 4. The real trees

### ln-real: the LN scale against the photometric truth, the final binary on the three pre-Plan-A trees

Source: B6/`ln-real-trees-read.txt`, B6/`ln-real-trees-parity.txt`, B6/`real-table.md`. Gap = F1's scale over the
photometric truth − 1, per member plane; parity = the probe's F1 scale over the run's own `.athln` − 1; LN's own `c` = the
probe's new `cRef` / `cTgt` keys (median).

| tree | n | F1 gap median / max \|gap\|, B6 | B1 | parity median / max \|Δ\| | LN's own `c` (cRef / cTgt) |
| ---- | ---- | ---- | ---- | ---- | ---- |
| s204-mono | 91 | −0.108 / 1.688 % | −0.108 / 1.688 % | +0.0077 / 0.1940 % | 1.137 / 1.122 |
| s204-osc | 39 | −0.179 / 1.913 % | −0.179 / 1.913 % | +0.0034 / 0.2357 % | 1.123 / 1.133 |
| runB-mono | 82 | +0.092 / 2.131 % | +0.092 / 2.131 % | +0.0057 / 0.2362 % | 1.139 / 1.118 |
| runB-osc | 303 | −0.166 / 3.347 % | −0.166 / 3.347 % | +0.0051 / 0.2120 % | 1.126 / 1.131 |
| hc11-G | 28 | −0.114 / 2.586 % | −0.114 / 2.586 % | −0.0465 / 1.2263 % | 1.240 / 1.134 |

Pooled parity: n 543, median +0.0050 %, max |Δ| 1.2263 %. Every tree reads as at B1; verify 23 of 23 trees' and 34 of 34
planb-b4 invocations proven.

Against B2's referee cells (the final binary on B4's clones `planb-<ds>-b4`, the reference pinned to B0's pick; cell = n /
median gap % / median |gap| % / slope / outside). B1's columns are another reference and another truth, never a before →
after of the estimator (R-PB3-2); every summary row equals B3's table (`results/plan-b/b3/real-table.md`) *(d)*.

| dataset | counted | F1 as is, after | psfSel on B4's `c`, after | F1 (B1) | the ERP (B1) |
| ---- | ---- | ---- | ---- | ---- | ---- |
| s204-mono | 31 of 84 | 31 / −1.17 / 1.17 / +0.871 / 23 | 31 / −1.79 / 1.79 / +0.722 / 28 | 31 / −0.07 / 0.21 / +0.131 | 31 / −2.96 / 2.96 / −0.011 |
| s204-osc | 315 of 315 | 315 / −0.27 / 0.39 / +0.273 / 60 | 315 / −1.14 / 1.46 / −4.573 / 211 | 39 / −0.18 / 0.43 / −0.089 | 39 / +4.44 / 4.44 / −0.602 |
| runB-mono | 27 of 82 | 27 / −0.56 / 0.65 / +0.263 / 9 | 27 / −3.72 / 3.72 / −3.911 / 22 | 27 / −0.26 / 0.43 / +0.052 | 27 / −2.13 / 2.34 / −0.333 |
| runB-osc | 303 of 303 | 303 / −0.20 / 0.40 / +0.652 / 64 | 303 / −0.96 / 1.28 / −4.281 / 195 | 303 / −0.17 / 0.39 / +0.095 | 303 / +3.54 / 3.54 / −0.632 |

Data: ln-real line (section 2).

### ln-truth: the product at the final code, master level, against B4's after-runs

Source: B6/`ln-truth-verdict.txt` and B6/`{hc11,runB,s204}/{measure-equal,weights,masters,ln-vs-truth}.txt`. Each tree was
re-run end to end at 5103310e and compared with B4's after-run (71a4a738, before floor (i); EQUAL is the expected answer
where no plane sits at the fit-call floor). The files label the comparator column "B0"; their paths are B4's clones.

| tree | run | Measure payloads equal | weights ρ (n) | movers / groups worse | masters | LN vs photometric truth, median per group / plane (after = B4) |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| hc11 (set 182) | 6 | 106 / 106 | 1.0000 (101) | 0 / 0 | every plane stat 1.000000 | G −0.11 %, L −0.04 % |
| run B (project) | 7 | 184 / 184 | 1.0000 (184) | 0 / 0 | every plane stat 1.000000 | mono L −0.25 %; OSC p0 +0.19, p1 −0.16, p2 −0.96 % |
| set 204 | 6 | 197 / 197 | 1.0000 (197) | 0 / 0 | every plane stat 1.000000 | mono −0.10 %; OSC p0 +0.11, p1 −0.11, p2 −0.95 % |

"Measure payloads equal" ignores the one key `durationMs` (the Measure call's wall time; section 8). The re-based LN row is
+0.00 % on every group and plane; the per-frame LN scale's median ratio 1.000000, scatter 0.0000 %. Run B's
`grid-report --ln-truth` wrote 429 lines (B6/`runB/grid-ln-truth.txt`). Set 204's absolute drizzle checks read as at B4
(level 3 of 4 sub-checks, OSC p0 0.997462; coverage 2 of 4; head vs base drizzle pairs 1.000000), which B4 recorded as not a
B4 move (`results/plan-b/b4/verdict.md` l.89–90): a property of that data, not of Plan B. So the final product code gives B4's
numbers on all three real trees.

Data: ln-truth line (section 2).

### hc11-G: the 1.2 % parity row (B1's open item 1)

Source: `.superpowers/sdd/2026-10-20-plan-b/b6-parity-read-report.md` (local) and L. Verdict **(a), a probe re-computation
artefact; no action**. The probe re-measures the frames with today's star measurement while the `.athln` side is the
2026-10-06 run's: on f70055 plane 0 the probe finds 523 matches against the run's 384 and selects 180 stars against 220.
hc11's worst members are its noisiest (per-star σ 0.042–0.049 against 0.009 on an exact-agreement member), so a different
selection moves them most. The input difference is common to all three trees (all three runs are 2026-10-06; L); the noise is
what singles hc11 out. The check that matters for the master, the product at the final code against the photometric truth,
is ln-truth's hc11 row above: G −0.11 %, equal to B4's after-run. B1's open item 1 is closed.

## 5. Suites

Source: L ("Step 3 suites"), B6's own lab lines from L, and the final review `.superpowers/sdd/2026-10-20-plan-b/b6-review-report.md`.

- Core, all targets, on the runner at 5103310e (r2, `--no-fail-fast`, the three network modules `sharing::iroh`,
  `api::collab_live` and `collab_v3_live_e2e` skipped, 433 tests filtered): 28 targets ok, **3684 passed / 0 failed / 19
  ignored**.
- The 433 network tests on the dev Mac (`cargo test -p athenaeum-core --lib -- sharing::iroh api::collab_live
  collab_v3_live_e2e`): **432 passed / 0 failed / 1 ignored**.
- Together: **4116 passed / 0 failed / 20 ignored** (13aee664's 4115 plus the review fold's one test).
- Why the split: runner r1 read 3945 passed / 120 failed / 12 ignored, the 120 all in those modules (65 + 45 + 10), panics
  "sync control send … timed out" and "live run … Elapsed"; `iroh_roundtrip_two_endpoints_localhost` fails alone too (31 s,
  machine idle). The runner's localhost / QUIC networking is the cause, not Plan B: no sharing or collab code was touched.
- `cargo test -p athenaeum-web`: 51 passed / 0 failed. `cargo check --no-default-features`: Finished.
- vitest: 82 files, 898 tests passed.
- Lab: star-lab 1490 OK; harness 437 OK (2 skipped).
- Final whole-branch review (before the runs, at 4437eda8): READY, 0 critical / 0 important / 4 minor, 51 targeted tests
  passed; minors 1–3 folded in 5103310e, re-gate `stacking::run::tests::measure` + `stacking::measure` 55 passed / 0 failed.

## 6. Recorded limits with owners

Sources: AT ("Recorded limits"), B6/`l10b-verdict.txt`, R (R-PB5-1…3), OI (the Plan B B3, B4 and B5 entries), RM
(R-OSP-16 B1 open items; R11-3), `results/plan-b/b5/verdict.md` l.79–80.

| id | limit | value at B6 (before) | owner |
| ---- | ---- | ---- | ---- |
| R-PB5-2 / R-PB5-1 | F1 on the l10b nebula-sky cells (P.1 neb0.5 + neb3, P.2 shift + warp) over the F-13 bar | 4 of 34 (B1 4 of 34; the research's fifth cell was −1.193 against 1.20); worst neb0.5 g t60 −3.07, warp m4 t60 −2.18, neb3 b25mix t60 −1.73 pp | LN's default estimator: re-opened only by a real nebula miss, with a per-side, structure-aware sky, never a plain or clipped mean (R-PB5-1) |
| R-PB5-3 | "PSF signal" (non-default) on deep nebula: LN's own per-side `c`, the module's on nebula sky | 15 of 34 (13 before B4's `c`); worst neb3 g t45 −15.89, neb3 b25mix t25 −12.28, neb3 m4 t25 +6.14 | F-10 / B.3; the measured lever is the pair-common ring (neb3 g t45 −15.88 → −8.42); `planb.sh b5 ctable` is the standing check |
| R11-3 | the star SNR pull on nebula sky, neb0.5 / neb3 | 3.64 / 5.98 at the B5 binary (record 3.63 / 6.04); B6's cycle moved 0 against B4 *(d)* | none: a recorded limit (the owner's word at B5) |
| R-PB4-4 (OI B4 item 1) | the `extended` classifier on raw debayered planes with β at the 1.5 floor removes 50–95 % of the bright stars from `c`'s sample, frame-dependently | `c` follows by ± 20 % | Plan C |
| R-PB4-7 (item 2) | the crowded-field `c` bias | +0.8 % on density/d5 | Plan C |
| R-PB4-5 (item 3) | floor (i)'s stand-in is the group channel's median, not the nearest peers by seeing | the stand-in's R error p90 22 % | Plan C: a seeing-conditioned stand-in by `fwhm_px` |
| PB-13' (item 4) | OSC R / B `c` above truth whatever the sample | +4–7 % on 44 / 48 lab rows | Plan C |
| R-PB3-1 (OI B3) | F1's synthetic-vs-real size gap | +11.97 % at `pairs/main/m4w10` t60; ≈ +1.9 % at the real \|ΔF\|; real members −0.2…+0.1 % | Plan C: a synthetic family with a fixed-pixel halo settles it |
| R-PB3-1 (OI B3) | psfSel is no default candidate until it passes the seeing-lever pairs (\|gap\| ≤ 3 %, \|slope\| ≤ 1 %/px) | −2…−8 % at −1.54 %/px; −3.9…−4.6 %/px of ΔF on real OSC and run B mono | whoever proposes psfSel again |
| R-PB3-2 | the set 204 mono referee at n 10–11 sets no level | SE median 1.19 %, above its allowance on every counted cell | recorded only |
| Plan A 14b debt | the ERP has never run Task 14b's 15 new units | 15 of 204 units due a refresh (no file 15) | the lab, before an ERP column is next read on those units |
| RM B1 open item 2 | the ERP's real-set gap and its mono / OSC sign flip; cause not investigated in Plan B | median gap s204-mono −2.96, runB-mono −2.13, s204-osc +4.44, runB-osc +3.54 % (B1's column) | Plan C, if the comparison is to be explained; the ERP is never a referee |

Data: l10b and cycle lines (section 2) for the B6 values; R11-3 at the B5 binary on the dev Mac (32f61897, lab a03ce38; plan
Task B5 "As it ran").

## 7. Owned quantities at close

The plan's table (`docs/superpowers/plans/2026-10-20-one-star-path-plan-b.md`, "Owned quantities at close"), filled; owners
as the spec's table says, B5's kept screens as the spec's sentence under it records. Values from AT, B6/`cycle/bars.txt`,
B6/`ctable-verdict.txt`, B6/`l10b-verdict.txt`, B6/`ln-real-trees-read.txt` and B6/`ln-truth-verdict.txt`.

| quantity | owner | consumers | truth bar | propagation | B6's after value |
| ---- | ---- | ---- | ---- | ---- | ---- |
| `c` (with SE, count, the screen's count) | Task 10 (F-5) → B4 (qualifiers and sample) | PSFSW / PSFSNR, psfSel, Register's and LN's ordering, the kept payload (B0) | F-5; F-12 (B4); F-11 identity (B0) | weights, LN reference composition, psfSel s, inject row 6 (reported) | F5.1 +0.76 / +1.41 %; F5.2 / F5.3 miss 0 of 60; R13 0 of 130; halo 0.9984; floor (i) 0 of 396 planes; c-table median \|c − c*\| 0.36 % (1 closer, 0 farther); real weights ρ 1.0000 on all three trees |
| the LN scale s | the default estimator, F1 as is (B3, the owner's; R-PB3-1) | Normalize, the masters | F-8 estimator (B3) | the master-level rows | F1 gap per tree equal to B1 (medians −0.108 / −0.179 / +0.092 / −0.166 / −0.114 %); masters 1.000000 against B4 on all three trees |
| the referee (harness truth, both modes) | B2 (the owner's rule) | every real-data LN verdict | F-8 referee (B2) | B3, B4, B6 | B6's referee summary equal to B3's *(d)*; F1 median \|gap\| 0.39 % (s204-osc), 0.40 % (runB-osc) |
| seeds (LN's sides) | 10b → B5: out of scope (B5 took the F1 branch) | LN's pairs: LN keeps `detect_stars` on the reference and `k`, no consumer of the module's seeds | F-13 | `k` kept, not retired: it retires only with module seeds under a module-fed default (R-B9) | l10b F1 4 of 34, 36 cells as at B5 |
| `saturated` (LN's one definition) | 8 / 10c → B5: out of scope | LN's selection: not moved | F-13 | `unverdicted_clip_range` kept beside Register's 0.95, L-14's 0.75 × the side's maximum and L-23's map | unchanged (no code); l10b as at B5 |
| LN's aperture-scale sky | F1: LN's own [6F, 8F] median (C-9: no change, R-PB5-1) | the LN scale | F-8 | B3's choice (F1) | F1 4 of 34 nebula-sky cells over the F-13 bar (recorded limit, section 6) |

Data: cycle, l10b, ln-real and ln-truth lines (section 2).

## 8. What B6 found and fixed in the tooling

Sources: L and R ("B6"). No product code moved for any of these.

- **The determinism gate's in-memory family (R-PB6-2).** The first `b6 gates` read 11 of 13: determinism 185 of 186 with
  `truthcheck_w10: not in the manifest`, a family B2 added that is rendered in memory and never written to the manifest. The
  gate now skips in-memory-only families (lab 5f2eddc); 185 of 185 regenerate.
- **`probe_parity` needs an unstripped set 204 run.** B4's clone had been stripped by the results-only rule (calibrated
  frames missing), so the gate read SKIP; the set 204 B6 clone was kept until `b6 gates` passed on it (13 / 13), then stripped.
- **`durationMs` in measure-equal.** ln-truth's first pass stopped on hc11: 106 of 106 payloads "differed" only in the
  Measure call's wall time. Ignoring that key gave 106 of 106 EQUAL; the b3 flow's real-data path had never run before
  (B3 had no runs). Fixed and pinned in lab 087a22f.
- **The b3 flow's build record.** It checked the probes against B4's build record, which `ln_scale_probe` no longer
  matches (the new keys); it now reads B6's record (lab 130469a).
- **R-PB6-1 withdrawn.** The plan to run only run B at the final code rested on B4's after-runs being at 13aee664; they ran
  at 71a4a738, before floor (i), so all three trees ran at 5103310e with B4's after-runs as the comparator. Caught before
  ln-truth started; no run was lost.
- **The scorecard identity.** `b6 identity`'s default comparand (b4-c2's probe outputs) had been deleted by B4's cleanup; the
  runner diffed the stored scorecards b4-c2 and b6-final instead (73 403 rows, moved 0, max |Δ| 0; lab d44e3bb), and
  `b6 report` takes that file as the identity when the probe dirs are absent (lab 60a5fc3).
- **Running scripts.** A `b6 ln-real` job exited 1 after its verdict because its shell script was edited while it ran; the
  rule is now: never edit `planb.sh` while a planb job runs.
- **Hygiene.** `ln_scale_probe` prints LN's own `cRef` / `cTgt` per arm (4437eda8), so LN's `c` is a number, not an
  inference; the harness timing flake `exits_75` is deterministic and `star-lab/true/` is ignored (lab 4e4e757).

## 9. What Plan C needs

Sources: OI (the Plan B entries), RM, the plan (l.291), L.

- **The Plan C items, measured, not fixed** (section 6): B4's four `c` items (the `extended` classifier on raw debayered
  planes, the crowded-field bias, a seeing-conditioned floor (i) stand-in, OSC R / B above truth); F1's synthetic-vs-real size
  gap; psfSel's seeing-lever entry rule; "PSF signal" on deep nebula (F-10 / B.3); a real nebula miss re-opening F1's sky.
- **Plan C's own carries in RM:** a CFA-masked plane gets `c` = 1 (no production caller today; R-OSP-16 B4), and the Analysis
  switch takes `MEASURE_VERSION` 6.
- **The real trees on the runner (an owner question at Plan C's start).** Plan B read the real trees on the dev Mac: the
  runner holds no acceptance tree (the three are 95 + 69 + 52 = 216 GB) and never sees the volumes their masters link to
  (plan PB-17, l.289–294). Moving them, with their masters, to the runner would put every Plan C run on one machine.
- **The runner's localhost / QUIC test failures** (section 5): an environment item on the runner box, not code. Until it is
  fixed, the runner's core suite runs with the three network modules skipped and the dev Mac runs those 433 tests.
- **The ERP refresh** on Task 14b's 15 units, before an ERP column is read on them.
- **Before Plan C starts:** the owner's word on merging `plan-b` into local main and the push (PB-18); B0 reaches main only
  together with `MEASURE_VERSION` 5 (R-PB0-8, met on `plan-b`).
