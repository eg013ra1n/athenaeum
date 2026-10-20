# Star measurement after Plan A: one uniform lab run, every bar against its baseline cell (2026-10-20)

Plan A of the star measurement program (one star-measurement path, spec
`docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md`, plan
`docs/superpowers/plans/2026-10-07-one-star-path-plan-a.md`) ends here. This note sets the code Plan A leaves behind
(athenaeum `one-star-path` at c10be63b, Task 14b's squash) against the baseline
`docs/superpowers/research/2026-10-07-star-measurement-baseline.md` (**BN**), bar by bar, on one uniform lab run, and
closes with the real-data checkpoint, what Plan B and Plan C start from, and what stays open.

How to read the numbers:
- **Copied as printed.** Every number is copied from the file named beside it, as that file printed it. A number this
  note computes from printed numbers is marked *(d)*. Real-corpus FWHM values from the per-frame CSV are rounded to 4
  decimals, as BN rounded them.
- **Where each number lives.** Abbreviations used in every table:
  - **F/** = athenaeum-lab `star-lab/results/plan-a/final/` (the re-runs at c10be63b, lab cf07135);
  - **SS** = F/`scorecard-summary.md`, **SI** = F/`scorecard-inject-summary.md` (each line one family × path, every
    bin pooled);
  - **SC** / **SCI** = the full v2 / inject scorecards outside the repository,
    `~/.athenaeum-acc/star-lab-a/results/t14-final/scorecard{,-inject}.json.gz` (sha256 in section 2), read where a
    bar needs a cell the summaries do not print;
  - **T14B/** = athenaeum-lab `star-lab/results/plan-a/t14b/` (Task 14b's outputs);
  - **RM** = `docs/stacking/README.md` (the R-OSP rulings and every task's acceptance table);
  - **L** = `.superpowers/sdd/2026-10-07-one-star-path-plan-a/progress.md` (the ledger; the owner's rulings).
- **Step 1 and the re-runs.** Task 14 Step 1 ran the whole lab at 69b4710b (lab 10593e4). Task 14b then moved 18
  families by design, so everything 14b could move re-ran at c10be63b (section 2). Files that only Step 1 wrote are
  cited as Step 1's, with their head.
- **The ERP.** "The ERP" is the external reference pipeline, run on the same inputs. It is a comparison column, never a
  referee (spec S-9).
- **Referees.** Synthetic truth everywhere; the injected stars on real frames; the corpus has none.
- **Truth widths.** `prepixel`, `pixbox`, `pointfit` as in BN (spec S-2).
- **Paths.** `measure` is the stacking Measure path, `register` the registration star list, `analysis` the legacy
  Analysis path (unchanged by Plan A, PA-15), `analysis-mode` the Budget-mode Analysis path Plan C will switch to
  (R-OSP-10).

## 1. Verdict

Plan A closes with every bar met or carrying a ruling. Against BN's five defects:

1. **The captured fraction no longer depends on the profile.** The spread over the six `sharp` profiles is `+0.77 %`
   at F 2.6 and `+1.41 %` at F 8.0 *(d)*, against BN's `18.73 %` / `22.16 %` *(d)* (SS Row 6).
2. **The frame FWHM and the β no longer move with the saturated set.** `twins/sharp/m4/f2.0` base vs deep reads
   `median 0.06 %` (BN `median 9.99 %`); 0 of 46 deep pairs move more than 1 % (SS Row 11). β is continuous
   (R-OSP-6), and no judged pair moves more than 10 % (`twin-saturation.txt`).
3. **The F1 LN over-read is unchanged, by design.** `pairs/main/m4w10` t60 reads `s 1.2317 vs s_true 1.1: +11.97 %`
   (BN `+11.84 %`). The LN estimator is Plan B's (section 5).
4. **The VNG per-plane widths are smaller but present** (out of scope, spec l.384): `osc/f2.0/m4` p1 `bias +6.16 %`
   (BN `+12.63 %`), `osc/f2.6/m4` p2 `bias -3.27 %` (BN `-6.71 %`) (SS Row 8). On the injected Pro raw lights p2
   reads `bias -12.69 %` (BN `-6.29 %`) (SI Row 3). Section 7.
5. **The legacy Analysis path is unchanged; its replacement is measured.** `analysis-mode` reads bright coverage
   `99.3 % of 300` on `sharp/m4/f2.6` (legacy `7.3 % of 300`) and a round-star floor `median e 0.1719` (legacy
   `0.2666`). Plan C switches the tab (section 6).

Every bar, one line each. "Baseline cell" is BN's cell or, where BN printed none, the bar's own "before" with its
file. "After cell" is the c10be63b re-run unless the line says otherwise.

| bar (task) | baseline cell | after cell (file) | verdict, ruling |
| ---- | ---- | ---- | ---- |
| F-0 identity (4) | measure `82.7 % of 301 expected` vs seeds `92.0 %`, no refusal reasons (BN row 1) | accepted + refused + not selected + trimmed = seeds on 172 of 172 measure images (160 rows) and 170 of 170 `analysis-mode` images (158 rows) *(d)* (SS Row 1 `fit outcomes`) | met (F13) |
| F-1 fitted / seeded (6b) | 82.7 / 92.0 = 89.9 % *(d)* on `sharp/m4/f8.0` | `97.5 % of 551 expected` / `97.8 % of 551 expected` = 99.69 % *(d)* (SS Row 1) | met (PA-20 d) |
| F-1 frame FWHM, Moffat 4 (6b → 9) | t06a base f8.0 +5.56 % (lab `t06b-f1/bars.txt` §2) | frame FWHM ÷ pointfit on `sharp/m4/f2.0…f16.0`: −0.04 … +0.35 % over 11 images *(d)* (SS Row 7) | met (T8 R2, T9) |
| F-1 frame FWHM, Gaussian (6b → 9; PA-20 a) | `sharp/gauss/f8.0` `bias -2.58 %` (BN row 3) | −0.25 … −0.28 % at F ≥ 2 (10 images); f1.2 / f1.6 −0.76 / −0.34 % (bar 3 %) *(d)* (SS Row 7) | met (re-read after Task 9) |
| F-1 donut (6b) | `donut/r6` `accepted 24 of 139 matched (17.3 %)` (t06a, `t06b-f1/bars.txt` §3) | r6 `accepted 0 of 140 matched (0.0 %)`, r10 `accepted 0 of 129 matched (0.0 %)`, every one refused with its reason (SS Row 1 `donut outcome`) | met (owner 2026-10-08, defocused frames) |
| raw completeness (6b) | `raw/corpus` 96.9–97.4 %, Pro raw 96.3–98.4 % (BN §4.1) | judged at 6b: every non-edge plane 100 %. Fisher ≥ 20 incl. edge, 9 planes: mosaic 129 / 130, 131 / 132, 121 / 121; `raw/corpus` 169 / 172, 40 / 40, 42 / 42; Pro raw 126 / 129, 125 / 129, 119 / 123 *(d, bins pooled)* (SCI). Each is at or above 6b's own incl.-edge record (`t06b-f1/bars.txt` §5) | met at 6b and re-judged at c10be63b: non-edge Fisher >= 20 measure-path completeness 100 % on all 9 raw / mosaic planes (lab `results/plan-a/final/raw-completeness-e.txt`, `t06b-f1/bars.py` `inject_nonedge` on the t14-final inject probe; lab 4abf69b; owner 2026-10-08 a, edge stars not counted) |
| F-4 injected FWHM (9) | Horse C11 G `bias -3.68 %`, L `-0.57 %`, set 107 `+5.33 %` (BN §4.1, frame β) | `betas@4` pointfit `bias -0.17 %`, `-0.15 %`, `+0.08 %` (SCI); frame β beside: `+0.05 %`, `-0.31 %`, `-0.99 %` (SI Row 3) | met (C7-3) |
| F-2 axis order (5) | 71–145 of 346–427 fits minor-first per `e0.3` family (spec F-2) | `fwhm_x < fwhm_y` on 0 fits in all 24 `shape` families (F/`path-compare.txt` part 2); major-first on 109 944 of 109 944 measure and 72 106 of 72 106 `analysis-mode` fits *(d)* (SS Row 4) | met |
| F-2 angle efficiency (5, 9) | `rms 44.564 deg` on `shape/f2.6/e0.3/t30` (BN row 4) | eff 0.74 … 1.24 on 24 of 24 `shape` families, max `shape/f4.0/e0.5/t90`; t30 cell `rms 30.078 deg` (SS Row 4) | met (R-OSP-3) |
| F-3 FN (8, 9, 10c) | no flag (t08 base FN 5323 / 5323, L l.807) | mono FN 4 of 4 299 (0.093 %), OSC 0 of 1 229 *(d, 160 rows pooled)* (SS Row 10) | met (rulings 2026-10-09) |
| F-3 FP and C-11 (10c) | 10c base FP 0.242 / 0.397 % | 10c: 0.244 % / 0.410 %, C-11 max +0.00145 (`t10c-floor/task-10c-L3-report.md`). Now: FP mono 124 of 79 885 (0.155 %), OSC 98 of 23 920 (0.410 %) *(d)*, the same counts as 10c's own pool (124 of 76 070, 98 of 23 920; `t10c-floor/bars-t09-f4.txt` §4) | met at 10c (R10c-8, R10c-10, R10c-11); C-11 not re-judged, 0 rows moved outside the design lists since |
| F-3 / F-4 twins (8 → 9 → 10c) | `median 9.99 %` (m4/f2.0, β flip); m2.5/f1.6 `15.93 %`; m4/f2.6 `2.93 %` (BN row 11) | `median 0.06 %`, `0.06 %`, `0.16 %`; 0 of 46 deep pairs over 1 % (worst `twins/sharp/m10/f1.2` 0.69 %) (SS Row 11); β: 24 of 48 pairs judged (24 void, a twin above 10), 0 over 10 %, worst 6.98 % *(d)* (`twins/sharp/blur1/f1.2` 9.02 → 8.39, `twin-saturation.txt`) | met (C7-4) |
| F-3 M42 β (10c) | head 31.777 (SE 15.89); B1 1.79 ≤ 3.11, B2 0.500 > 0.144 (plan l.3797) | not re-measured (real frames). 10c: B1 3.60 ≤ 5.39, B3 0.051 ≤ 0.207; B2 0.155 > 0.046 (`t10c-floor/bar3.txt`) | B1, B3 met; B2 not a bar (R10c-7, then group β, R11b-12 / R11b-13) |
| F-4 frame FWHM (9) | `sharp/blur2.5/f2.6` frame `bias -2.07 %`; β spread `+9.60 %`; `mixture/b` +4.88 % (BN row 3, §5) | 0 of 58 images over (46 `sharp` + 4 `mixture` families) *(d)*; worst F ≥ 2 `mixture/b_clip` −0.65 % (pixbox), worst F < 2 `sharp/blur1/f1.2` −2.27 % (bar 3 %) *(d)* (SS Row 7) | met (owner E5, C7-2) |
| F-4 centroid (9) | t08 base efficiency 1.2182 / 1.1383 (lab `t09-f4/bars.txt` l.200–201, t08-f3 vs t09-f4 common rows; the c10be63b re-read's base on its own common rows 1.2183 / 1.1385) | re-read at c10be63b (t08-f3 → t14-final, F/`debt-lines-e.txt`): pooled measure ×1.0014, register ×1.0016 (≤ 1.01); F ≤ 2 split: measure ×1.0140 (mono ×1.0125), register ×1.0112 (mono ×1.0120) | met pooled (C7-1); the F ≤ 2 split is a Task 14 debt line under the 2026-10-19 debt ruling |
| F-4b M42 3c (11b) | own β 1.0883 (`t11b-l5/tables.txt` l.16) | not re-measured; 11b: 1.0431 ± 0.0013 against [1.044, 1.062] (l.20) | missed + R11b-13 (a limit of the composite-PSF definition; owner: no new rounds) |
| F-4b M42 3b (11b) | own β z₀ 4.63 / 5.47 | not re-measured; 11b: HFR 1.01 ≤ 2.66, FWHM_m2 5.16 > 2.16 (l.21–22) | HFR met; FWHM_m2 missed + R11b-13 |
| F-4b skew B-2 (11b) | gate zero: ours +2.88 pp vs the ERP −0.14 pp | not in the final run (the `skew/*` families live in Task 11b's own manifest, not in `manifests/v2.json`); 11b misses B-2 on `f1.6/s0.5`, `f1.6/s1` (`t11b-l2/tables.txt` l.34) | reported, β not a bar on the family (R11b-4); the single-frame ridge (+1.8 % per unit β) is a Task 14 debt line under the 2026-10-19 ruling |
| F-5 spread (10) | 18.73 % / 22.16 % *(d)* (BN §1 item 2) | f2.6 medians 0.9932 … 1.0008: +0.77 % *(d)*; f8.0 0.9865 … 1.0004: +1.41 % *(d)* (SS Row 6) | met (PA-20 c) |
| F-5 psfSel, blur pairs (10) | `pairs/iso/blur25mix` t60 `-13.03 %`, `blur4` t60 `-7.28 %` (BN row 6) | `+1.72 %`, `+1.19 %` (SS Row 6 `arm psfSel`, `ln beta auto`) | met |
| F-5 pure and mixture pairs (10) | `-1.60 %` (`pairs/main/m4mix` t25) … `+1.16 %` (BN row 6) | pure: 0 of 70 cells beyond ±1.2 %, worst `pairs/main/g-w5` t60 `-1.07 %`; mixtures: 0 of 20 beyond \|t01c\| + SE *(d, against `t12-amode/bars.txt` §4's t01c and SE)* (SS Row 6) | met (PA-20, F24) |
| F-6 SNR (11) | none (no per-star SNR in stacking) | 50 cells within 3 %, worst `-0.97 %` (`background/neb0.1`), `sharp` −0.90 … +0.51 % *(d)*; `n_no_gain` 0 over 192 measure row-6 cells (SC); `neb0.5` / `neb3` / `neb3_4096` `-16.54` / `-75.51` / `-60.65 %` reported | met (R11-4) |
| F-6 noise pull (11) | none | robust pull 0.92 … 1.25 on 50 planes (`osc`, `osc_pairs`, `resampled`) (SC) | met (R11-8) |
| F-7 background (6b → 9) | flat `-0.076 sigma`, `neb0.5` `-0.064 sigma`, `neb3_4096` `+0.998 sigma` (BN row 5) | flat `-0.053`, `neb0.1` `-0.076`, `neb0.5` `-0.149`, `neb3` `+0.053`, `neb3_4096` `-0.241 sigma` (SS Row 5) | met (owner 2026-10-08 b) |
| F-10 detection (10b) | recall 0.591 / 0.343; `halo` 256 false | seeds raw `77.7 % of 629` / `53.9 % of 629`; false 7 / 10 on 4.194304 Mpx = 1.67 / 2.38 per Mpx *(d)*; `halo` 2 (SS Row 1, SC counts) | met (C-1, C-3) |
| F-10 knots (10b) | completeness 0.978 / 0.714 / 0.424; density × nebula 0.367–0.681 | not in the final run (Task 10b's knot and density × nebula families are not in `manifests/v2.json`); 10b: pooled 1 / 1 / 2 of 216 (allowance 4 / 3 / 4), 0.607–0.862 (RM l.2844–2847) | met at 10b (Q7, L3-1) |
| F-10 field width (10b) | M42 L 0000 51 / 61, L 0008 51 / 60 | not re-read (a count on the M42 inject frames; SI prints pooled Fisher ≥ 6 cells); 10b: 45 / 45, 41 / 41, 45 / 45; L 0000 60 / 61, L 0008 59 / 60 (RM l.2848–2849) | met at 10b (A5) |
| F-10 trails (10b) | 947 / 786 fragments, `c` 4.48 / 6.89 (spec F-10) | not re-measured (real frames); 10b: 0 fits, share 0.916 / 0.933 (RM l.2851–2855). Task 14b run 6: L 0010 / L 0011 refused as trailed, 104 of 106 integrated | met; H 0029 (0.445) a recorded miss (Q5, Q6) |
| F-10 per-star sky (10b) | none (new quantity) | σ_i / σ median 0.994 … 1.017 on the 122 smooth-sky rows *(d)* (SC `ring sky vs truth`); 14b's `spikes/*` event planes 1.014 … 1.058 (3 above 1.05), their clean twins 0.995 … 1.000 *(d)*; nebula, OSC and `resampled` rows are outside the bar's scope | met |
| F-10 LN on nebula sky (10b) | 13 of 34 cells over, before (lab `t10b-onebg/a1c/p1-table.txt`) | not re-read; 10b: per side 13 of 34 (= the before), 15 pair-common; warped t25 +1.91 … +1.96 pp (RM l.2862–2863) | missed, reported + C-9 (Plan B's "before", section 5) |
| F-10 real sky S.1 (10b, 10c, 11) | M42 `c` sd 1.02 % | not re-measured (real frames); 0.67 % (10b), 0.81 % (10c, 11) (RM l.2866, l.3109) | met |
| cost M.1 (10b, 10c, 11) | 133.5 s (runner) | 74.7 / 76.8 / 76.9 s (RM l.2869) | met; superseded at master level by the M.2 chain |
| budget B1 (12) | none (new row) | 100.0 % on every image, 156 rows / 168 images; 2 `donut` rows `n/a (no matched star)` *(d)* (SS Row 1 `budget coverage`) | met (R12-6) |
| budget B2 (12) | none | identity on 170 of 170 `analysis-mode` images *(d)* (SS Row 1) | met |
| budget B3 (12 → 13b → 14b) | 25.2 s / 77 ≈ 0.33 s *(d)* (spec l.69) | 0.70 s (T12, missed, R12-8) → 0.464 s (13b) → 0.439 s, legacy 0.234 s (Step 1, F/`wall-amode-final.txt`, 33.8 / 18.0 s per 77 frames *(d)*) → **0.500 s per m42 frame, 0.937 × 69b4710b** (Task 14b Step 8 bracket, T14B/`e4-cost.txt`) | met |
| budget B4 (12) | 223.1 s; bar 267.7 s | 111.9 s (lab `t12-amode/measure-wall.txt`) | met |
| Task 11c spill (11c) | none | B / A 0.9885; re-run 0.2700 ≤ 0.6181; `.athf` identity 0 / 0 (lab `t11c-wall/bars.txt`) | met (R11c-7) |
| cost M.2 (11c, 13) | set 204 Measure 3.62 min (`tierA-lnB-f1` run 5) | 9.88 min, B / A 2.738 (lab `t13-real/bars.txt` l.50–51) | missed; superseded (owner 2026-10-16) by 13b → 13c → the fast-measurement bar (met, below) |
| real masters (13) | before trees (PA-13) | median sub-checks 1 / 6, 0 / 4, 0 / 8 (Horse C11 / run B / set 204) (`t13-real/bars.txt`) | missed + R13-3: a re-definition, report-only |
| real weights (13) | before trees | Horse C11 ρ 0.4642; run B 0.9553; set 204 0.9595 (`t13-real/bars.txt` l.11 / 27 / 44) | Horse C11 missed + R13-3 (accepted: the ERP join ρ 0.931 → 0.956 mono); run B, set 204 met |
| real LN s (13) | before trees | raw +3.00 / −0.95 / −0.97 %; re-based in section 4.3: set 204 −0.21 % mono, −0.06 / −0.06 / +0.13 % OSC | missed raw + R13-4: re-definition, the row re-based (bar ±0.5 %) |
| real Measure (13) | 1.65 / 3.55 min | 1.338 / 2.752 / 2.726 × before (`t13-real/bars.txt` l.14 / 30 / 47) | missed; owner → Task 13b |
| 13b speed (13b) | H 5836a970: 9.90 / 2.22 min | set 204 7.95 vs the ERP 3.77 min; Horse C11 1.43 vs 3.71 min; run B not run (lab `t13b/`) | set 204 missed + owner route A (2026-10-17): speed to its own cycle (13c, then fast measurement); Horse C11 met |
| 13b quality (13b) | H | identity 495 k lines, 0 differing; set 204 identical; `t13b-id`, `t13b-final` 0 moved | met (route A) |
| 13c owner bar (13c) | the ERP: set 204 1005 s; Horse C11 2787.6 s | runner Horse C11 317 s; dev Mac set 204 1081 s (RM R-OSP-13, l.3583–3585) | runner met; dev Mac missed, superseded by the fast-measurement bar (owner 2026-10-17, whole-run bar) |
| 13c identity (13c) | — | every artifact byte-identical at pool 1 / 4 / 10 / 16, admission 1, cap drop; swap 0; `t13c-id` 0 moved | met |
| fast-measurement bar (FM) | H 1082.2 s; the ERP 1005 s | **794.1 s** (N / H 0.734) at defaults, dev Mac; Horse C11 runner 305 → 267 s (`2026-10-18-fast-measurement.md` l.141; RM R-OSP-14) | met (FM-19, FM-28, FM-31) |
| the debt (5 → 14) | t01c-baseline | OPEN on fwhm scatter (measure, pointfit), centroid scatter (measure), centroid scatter (register); the four bias groups PASS (F/`debt-final.txt`, at c10be63b) | OPEN, carried: owner 2026-10-19 «Закрыть, долг перенести» ("close it, carry the debt") — centroid to the owed per-frame pixel-phase correction, FWHM scatter to research |
| owner ecc line (6b → 8 → 9) | t06a | measure, Fisher > 300, pooled: up 30 990 / down 27 150 (2√n 482), mean \|e\| error 0.09187 → 0.09339, WORSE (F/`ecc-lines-e.txt`, t06a → c10be63b) | reported: an owner line (C6 (2), 2026-10-09), under the 2026-10-19 debt ruling |
| no-regression (every step) | `baseline-2026-10-07-fw` | `PASS: moved beyond SE (outside the design list): 0 · one-sided keys missed: 0 · lost judged rows: 0 · control mismatch: 0` against the baseline (701-entry `design-cumulative.json`) and against fm-H, v2 and inject (396-entry t14b list) (F/`noreg-*.txt`) | met (F21, M-13, PA-19) |
| gates (2, 14) | `13 PASS, 0 FAIL, 0 SKIP` (BN §1) | `summary: 13 PASS, 0 FAIL, 0 SKIP` (F/`gates.txt`, set 204 run 7) | met (F12; probe_parity at the run's group β) |
| 14b R1, m101 | measure ÷ ERP 0.379 (2025) / 0.094 (2023) at 69b4710b (L); BN 0.9015 (28 frames) | `median 0.9469 over 15/15`, `median 0.9573 over 13/13`; `30 of 30 not at the floor`; `0015` `0.9505`; lowest frame 0.8705 *(d)* (F/`corpus-bars.txt`, F/`corpus-table.md`) | met (route E, owner 2026-10-19) |
| 14b R2, M51 Hα single | 0.094 at 69b4710b (L) | `ratio 0.9911 >= 0.85, at floor False` (F/`corpus-bars.txt`) | met |
| 14b R3, m42 and singles | m42 0.994, singles 0.996 (`off`) | m42 `median 0.9942 over 76/77`; singles `median 0.9973 over 32/38`; every frame within 1 % (m42, max 0.21 %) / 2 % (singles, max 0.23 %) of `off`; 6 probe gaps (M3) (F/`corpus-bars.txt`) | met |
| 14b R4, set 204 | `off` | 407 of 407 planes; frame FWHM max 0.05 %, median 0.000 %; median ÷ ERP 1.0619 → 1.0619; seeds 1.0000; 0 at the floor (T14B/`e4-r4-s204.txt`) | met |
| 14b R4, Horse C11 | `off` | 106 of 106 within 2 % or explained (14 moved: band 14, bracket 9, ERP 4, nearer 3); max 1.12 % on 91 judged planes; median ÷ ERP 0.9592 → 0.9590 (T14B/`e4-r4-hc11-q7.txt`) | met under Q7's reading |
| 14b lab bars L1–L5′, F-10, PA-2 | `off` | L1 0.9903 … 1.0000, L2 0.9968 … 0.9998, L3 pass1 ≥ `off`, L4 no floor, L5′ 1523 / 1523 cells in 137 families; recall 0.777 / 0.539; PA-2 0 per Mpx (T14B/`lab-bars.txt` for L1–L4, F-10, PA-2; L5′ 1523 / 137: RM R-OSP-15 and the ledger's 14b Step 6); carried to c10be63b by identity (F/`identity-vs-t14b.txt`) | met (S3-R1 … R4) |
| 14b median-pass cost | ≤ 0.35 ms / Mpx | 0.262 (m101 `0000`) / 0.193 (set 204 `0058`) ms / Mpx (T14B/`e4-cost.txt`) | met |
| 14b mono Measure peak | ≤ 8 planes | 8.83 planes; 69b4710b `measure_probe` 8.82 (L, Step 8) | missed + STOP C ruling (pre-existing, not route E) → open item `MEASURE_PLANES_RESIDENT` (section 7) |

The gates: `13 PASS, 0 FAIL, 0 SKIP` at lab 3d5506b, set 204 run 7.

## 2. Method

### Code and binaries

- athenaeum `one-star-path` at c10be63b (`c10be63b57f825a175cd1d28aba4aade9bb0d8a6`), tracked files clean.
  Submodules: rustafits `d0f0b16e02b2e9b07da14109cda8cc62079f7aed`, solvemyastro `c1c633c4f906…`. Plan A's code is
  Tasks 1–14b; Task 14 itself changes no code.
- athenaeum-lab at 3d5506b (`3d5506b08d7c859c34ee7eaec95c9fa28e07a524`) for the run, results committed in cf07135.
- **The probe binaries**, release examples built from c10be63b under `.superpowers/target-acc` (F/`run-log-e.txt`,
  build 92 s):
  - `star_probe` sha256 `695bb4935f6708b8fcc84835337663d2de6e96f3c4952ab715a8a60cf28d03b0`;
  - `ln_scale_probe` sha256 `76392f4ef7d146a71598e5d159cc096fadb2f62c4bfd463d22846299ce993107`.
- **The one-binary proof.** Each output set has one provenance record (F/`provenance.txt`, `athenaeum head c10be63b…,
  clean: True`, probe args `--diagnostics --seeds matched --seed-threshold 6.0`), and `--verify` proves it:

  | Output set | Proven |
  | ---- | ---- |
  | v2 | `1041 planned, 1041 proven, 0 not proven` (970 `star_probe`, 71 `ln_scale_probe`) |
  | injections | 109 proven of 109 (95 `star_probe`, 14 `ln_scale_probe`) |
  | star-test corpus | `420 planned, 420 proven, 0 not proven` |

  Files: `~/.athenaeum-acc/star-lab-a/results/t14-final/verify.txt`, `inject-verify.txt`, and
  `~/.athenaeum-acc/star-lab-a/t14-final/rerun-logs/corpus-verify.log`.
- **Identity with Task 14b's own outputs** (F/`identity-vs-t14b.txt`): `files same 1033 diff 0`, `files same 109 diff
  0`, `files same 420 diff 0` (v2, inject, corpus). Task 14b measured its outputs at c8cdc1b3. Its final fix round
  6cd9aed7, whose tree c10be63b squashes, changed production code (a thin-plane guard, `w >= 2 && h >= 2` on the
  all-clean median path; L), and the identity shows that it moves no output.

### What ran when

- **Step 1** (2026-10-19, 69b4710b, lab 10593e4; `task-14-report-r1.md`): v2 996 / inject 109 / corpus 420 outputs,
  0 failed, all proven against `star_probe a2d11d4a0a95…` (F/`run-log.txt`). Its as-run-only files stay its record:
  `noreg-vs-t12.txt`, `noreg-vs-t01c.txt`, `fmh-identity{,-inject}.txt`, `probe-parity-groupbeta.txt`,
  `wall-amode-final.{txt,json}`, `debt-lines.txt`, `m101-diag/`.
  - `noreg-vs-t12`: 13 973 moved, all `analysis-mode`: Task 13b's deliberate changes (the Budget selection and the I3
    trim), recorded by path in 13b's own outputs. The direct proof is the diff against fm-H:
    `numeric fields equal: 529094`, `differing fields: 2495` (lab-computed model fields only, max relative 1.3e-13),
    inject `differing fields: 0` (F/`fmh-identity*.txt`).
  - The gates read 12 of 13: `probe_parity` failed at its stale own-β premise. Under run 6's group β
    99.89317394922529 it passed (`20012 fits`, `0 mismatches`, F/`probe-parity-groupbeta.txt`). The gate was fixed
    in lab 8c3194c.
  - The m101 blocker (`measure` ÷ ERP 0.335 in `corpus-table.md`) led to the diagnostic (lab be106a6) and to Task 14b.
- **The re-runs after Task 14b** (2026-10-20, c10be63b, lab 3d5506b → cf07135; one script,
  F/`rerun_after_14b.sh`; F/`run-log-e.txt`, F/`verdict-e.txt`). 14b changed production code, so no 14b output
  served by its sha256 proviso. Everything ran again:
  - `lab_cycle --step t14-final --prev t14b`: v2 and inject probes and scorecards, `noreg` against t14b,
    `noreg-cumulative` against t01c, same-star, debt, truth sanity, ERP audit (F/`lab-cycle-verdict-e.txt`);
  - the corpus probe;
  - every Task 14 output in F/;
  - the gates on set 204 run 7;
  - the owner's ecc lines (F/`ecc-lines-e.txt`, added on the owner's question of 2026-10-20).

  Wall: probe 434 s, scorecard 176 s, inject probe 243 s; lab cycle 934 s; corpus probe 291 s; gates 2059 s.
- **Cited, not re-run** (plan, "After Task 14b lands"): B3 (Task 14b's Step 8 bracket replaces Step 1's 0.439 s), the
  bar inventory. (The centroid F ≤ 2 debt line was re-read at c10be63b, section 1.)

### Families and scorecards

| Scorecard | Rows | Scope | Built in | sha256 (head) |
| ---- | ---- | ---- | ---- | ---- |
| v2 | 73403 | 138 families, 296 images, `0 missing output(s)` | 175.3 s | `ac2c69140dfd9a25acb16ead7f8519a0412d07ab4583b4a16fe043dce75bc579` |
| injections | 3328 | 9 families, 22 images, `0 missing output(s)` | 13.1 s | `2e16ba6c9de9c5ad409eed3cb5672815ef3102552e1f47a17d853a39546f42d8` |

- `manifests/v2.json` now holds 185 families (the `determinism` gate's count; BN 164). Among the 21 new ones are
  Task 14b's `spikes/*` event families with their clean twins and `under/gauss/f1.5`.
- **The measure path's population changed.** Since Task 6a the measure path fits every seed of the matched filter at
  6σ (R-OSP-2). Its "expected" set is now `fisher_snr >= 6`, where BN's was `peak_snr >= 20`, and it holds fainter
  stars. On `sharp/m4/f2.6`: raw `68.7 % of 629` in BN, `85.7 % of 629` now (SS Row 1). Per-star cells in section 3
  therefore pool a fainter population than BN's. The same-star comparison is the owner ecc line and the debt check
  (section 1).
- **The ERP batch** is BN's, unchanged: `ERP dir: …/star-lab-v2/erp`. 15 of 204 units have no ERP file: the 14
  `spikes/*` images and `under/gauss/f1.5`, Task 14b's new families
  (`~/.athenaeum-acc/star-lab-a/results/t14-final/erp-audit.txt`). By the owner's rule the ERP re-runs only when
  synthetic frames change (L, 2026-10-20). Section 7.
- **Truth sanity:** `3 cells (3 explained, 0 new)`: `doubles/f4.0` (blend) and `resampled/f2.0`, `/f3.0` (warp).

### The gates

One run of `bin/gates.py` at lab 3d5506b (the slow gates parallel, owner-approved; output identical to the sequential
run), on set 204 run 7 (`STARLAB_ACC_RUN_ID=7`). File: F/`gates.txt`.

| gate | verdict and numbers, as printed |
| ---- | ---- |
| pixel_integration | `PASS  9 test(s) ok (tests.test_render)` |
| mosaic | `PASS  1 test(s) ok (tests.test_osc.TestOsc.test_every_cfa_pixel_equals_its_channel)` |
| injection | `PASS  22 of 22 injected entries bit-identical outside the stamps ({'calibrated': 9, 'mosaic': 3, 'pair': 3, 'raw': 7}); 26 refused, listed` |
| gain | `PASS  22 of 22 injected entries re-read in [0.9, 1.1] with their recorded mode, a in [0.912, 1.080]; 26 refused (listed, neither pass nor fail)` |
| determinism | `PASS  185 of 185 families regenerate their first image (hash equal or max \|delta\| <= 1e-7 with the same truth)` (wall 1397 s) |
| crlb | `PASS  7 test(s) ok (tests.test_crlb)` |
| matcher | `PASS  54 sharp images, 30676 truth stars matched against themselves: 100 %, zero unmatched` |
| truth_normal | `PASS` … `max \|gap\| 0.287 % (bar 0.3), slope -0.0063 %/px2 over FWHM2 (bar 0.1)` (the same as BN) |
| truth_crowded | `PASS` … `gaps -0.045, +0.154, +0.594 % (bar +-0.8)` (the same as BN) |
| erp_emulation | `PASS  erplib (Moffat4, from the ERP's per-star dumps) vs the ERP's own LN scale (ln.csv) on 105 pairs targets in 19 groups: max \|diff\| 0.0808 % (main/m4/e7t60), median \|diff\| 0.0048 %, 0 over the bar 0.1 %` |
| probe_build | `PASS  cargo check -p athenaeum-core --examples exit 0` |
| probe_parity | `PASS  20010 fits (measure) vs 20010 (artifact); 200100 persisted fields … equal bit for bit after ONE f32 narrowing of the probe value: 0 mismatches` … `channel 27 of 27 fields exactly equal`; `group beta 99.89286330415447 (f64, stacking_run_groups.beta run 7 group mono__NoFilter__bin1__180s)` |
| seed_parity | `PASS  pin passed (seeds.len() == stars_detected); 556 probe seeds brightest-first by flux: True; counts equal: True` |

## 3. Rows 1–11

BN's tables, the same families and paths, with the after cell beside BN's. `analysis-mode` is added where it has the
row. Cells as SS prints them (bins pooled). Divergent counts are from F/`divergent-families.txt` (**A**: \|bias\| > 2
× own scatter; **E**: worse than the ERP).

### Row 1: detection

| family | path | completeness, BN | completeness, after | bright coverage (300), BN → after |
| ---- | ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | 100.0 % of 407 expected · raw 74.2 % of 629 | 95.9 % of 534 expected · raw 82.2 % of 629 | 100.0 → 100.0 % |
| sharp/m4/f1.2 | analysis-mode | — | 92.9 % of 534 expected · raw 79.5 % of 629 | — → 100.0 % |
| sharp/m4/f2.6 | measure | 100.0 % of 364 expected · raw 68.7 % of 629 | 99.3 % of 537 expected · raw 85.7 % of 629 | 100.0 → 100.0 % |
| sharp/m4/f2.6 | register | 80.9 % of 492 expected · raw 63.3 % of 629 | 82.1 % of 492 expected · raw 64.2 % of 629 | 100.0 → 100.0 % |
| sharp/m4/f2.6 | analysis | 21.5 % of 554 expected · raw 18.9 % of 629 | 21.5 % of 554 expected · raw 18.9 % of 629 | 7.3 → 7.3 % |
| sharp/m4/f2.6 | analysis-mode | — | 92.0 % of 537 expected · raw 79.5 % of 629 | — → 99.3 % |
| sharp/m4/f8.0 | seeds | 92.0 % of 301 expected | 97.8 % of 551 expected · raw 86.0 % of 629 | — → 100.0 % |
| sharp/m4/f8.0 | measure | 82.7 % of 301 expected · raw 47.2 % of 629 | 97.5 % of 551 expected · raw 85.5 % of 629 | 91.3 → 100.0 % |
| sharp/m4/f8.0 | analysis-mode | — | 90.6 % of 551 expected · raw 79.5 % of 629 | — → 98.3 % |
| sharp/gauss/f2.6 | measure | 100.0 % of 375 expected · raw 70.0 % of 629 | 99.1 % of 555 expected · raw 88.2 % of 629 | 100.0 → 100.0 % |
| sharp/gauss/f2.6 | analysis-mode | — | 89.2 % of 555 expected · raw 79.5 % of 629 | — → 95.7 % |
| density/d1000 | measure | 70.4 % of 2392 expected · raw 45.4 % of 4194 · blends 7 | 87.2 % of 3635 expected · raw 76.1 % of 4194 · blends 13 | 73.3 → 97.0 % |
| density/d1000 | analysis-mode | — | 13.7 % of 3635 expected · raw 11.9 % of 4194 · blends 1 | — → 14.3 % |
| saturated/f2.6/n60 | measure | 100.0 % of 769 expected · raw 71.1 % of 1258 | 99.5 % of 1099 expected · raw 88.1 % of 1258 | 100.0 → 100.0 % (of 600) |
| saturated/f2.6/n60 | analysis-mode | — | 89.7 % of 1099 expected · raw 79.5 % of 1258 | — → 100.0 % (of 600) |
| trails | measure | 66.4 % of 149 expected · raw 32.0 % of 350 | 75.0 % of 272 expected · raw 59.1 % of 350 | 37.3 → 69.0 % |
| trails | register | 12.3 % of 243 expected · raw 8.6 % of 350 | 11.5 % of 243 expected · raw 8.0 % of 350 | 10.0 → 9.3 % |

- The measure path's raw share of all truth stars grew on every family above: the denominator moved from
  `peak_snr >= 20` to `fisher_snr >= 6`. The legacy `analysis` cells are BN's exactly.
- `analysis-mode` keeps the brightest stars that the legacy path dropped (bright coverage 99.3 % against 7.3 % on
  `sharp/m4/f2.6`). On `density/d1000` it reads `13.7 %`, because its budget is 500 stars (R-OSP-10).
- Divergent (E): `analysis`, 35 family-planes (BN 33). The worst is unchanged: `saturated/f2.6/n5 p0: 18.7 % of 555
  expected` against the ERP's `51.4 %`. No measure, register or seeds line is below the ERP.

### Row 2: centroid

Efficiency against the CRLB (x, y), BN → after, with the after cell's n:

| family | path | BN | after |
| ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | seeds | eff (2.34, 2.04) (n 180) | eff (2.03, 2.80) (n 213) |
| sharp/m4/f1.2 | measure | eff (1.00, 1.21) (n 180) | eff (1.44, 1.28) (n 206) |
| sharp/m4/f1.2 | register | eff (1.00, 1.21) (n 180) | eff (1.42, 1.22) (n 191) |
| sharp/m4/f2.6 | seeds | eff (1.83, 1.95) (n 205) | eff (2.63, 2.56) (n 264) |
| sharp/m4/f2.6 | measure | eff (1.03, 0.93) (n 205) | eff (1.04, 1.04) (n 264) |
| sharp/m4/f2.6 | register | eff (1.03, 0.91) (n 203) | eff (1.04, 0.96) (n 207) |
| sharp/m4/f2.6 | analysis | eff (0.90, 1.00) (n 84) | eff (0.90, 1.00) (n 84) |
| sharp/m4/f2.6 | analysis-mode | — | eff (1.04, 1.04) (n 260) |
| sharp/m4/f8.0 | measure | eff (0.89, 1.16) (n 117) | eff (1.02, 1.08) (n 261) |
| density/d600 | measure | eff (1.12, 0.98) (n 155) | eff (1.23, 1.00) (n 241) |
| saturated/f2.6/n60 | measure | eff (0.99, 1.07) (n 359) | eff (1.00, 1.06) (n 495) |
| resampled/f2.0 | measure | eff (0.99, 1.00) (n 239) | eff (1.00, 1.05) (n 265) |

- At F ≥ 2.6 the measure and register paths stay within 0.96–1.23 of the CRLB, on more stars than in BN. At F 1.2 it reads `eff (1.44, 1.28)`.
  The F ≤ 2 centroid split is the debt line of section 1.
- Divergent (A): one `analysis` family-plane, `spikes/c0010/m4/f12.0/clean p0: bias (-0.4366, -0.1360) px · scatter
  (0.0000, 0.0000)` (a 14b family, legacy path). No measure or register plane.

### Row 3: FWHM

Pointfit, BN → after (the prepixel and pixbox columns are in SS):

| family | path | BN | after | ERP (after) |
| ---- | ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | bias +4.65 % · scatter 0.90 % | bias +0.22 % · scatter 2.87 % | Moffat: bias +4.11 % · scatter 1.09 % |
| sharp/m4/f1.2 | analysis-mode | — | bias +0.28 % · scatter 2.84 % | |
| sharp/m4/f2.0 | measure | bias +0.10 % · scatter 0.60 % | bias +0.38 % · scatter 0.84 % | Moffat: bias +1.29 % · scatter 0.84 % |
| sharp/m4/f2.6 | measure | bias +0.09 % · scatter 0.58 % | bias +0.25 % · scatter 1.00 % | Moffat: bias +0.86 % · scatter 0.82 % |
| sharp/m4/f2.6 | analysis-mode | — | bias +0.17 % · scatter 0.99 % | |
| sharp/m4/f5.0 | measure | bias +0.03 % · scatter 0.38 % | bias +0.06 % · scatter 0.97 % | Moffat: bias +0.22 % · scatter 0.38 % |
| sharp/m4/f8.0 | measure | bias +0.02 % · scatter 0.29 % | bias -0.01 % · scatter 0.84 % | Moffat: bias +0.12 % · scatter 0.28 % |
| sharp/m2.5/f1.2 | measure | bias +8.62 % · scatter 0.80 % | bias +0.45 % · scatter 4.26 % | Moffat: bias +4.71 % · scatter 1.69 % |
| sharp/m2.5/f2.6 | measure | bias +0.18 % · scatter 0.54 % | bias +0.45 % · scatter 0.88 % | Moffat: bias +1.32 % · scatter 0.72 % |
| sharp/m2.5/f2.6 | analysis-mode | — | bias +4.71 % · scatter 1.07 % | |
| sharp/m10/f2.6 | measure | bias +0.04 % · scatter 0.49 % | bias +0.09 % · scatter 0.84 % | Moffat: bias -0.49 % · scatter 0.74 % |
| sharp/m10/f2.6 | analysis-mode | — | bias -3.87 % · scatter 0.87 % | |
| sharp/gauss/f2.6 | measure | bias -3.01 % · scatter 0.50 % | bias -0.29 % · scatter 0.88 % | n/a (the ERP accepted no star) |
| sharp/gauss/f2.6 | analysis-mode | — | bias -6.64 % · scatter 0.96 % | |
| sharp/gauss/f8.0 | measure | bias -2.58 % · scatter 0.36 % | bias -0.28 % · scatter 0.69 % | n/a (the ERP accepted no star) |
| sharp/gauss/f8.0 | analysis | bias -4.95 % · scatter 0.16 % | bias -4.95 % · scatter 0.16 % | |
| sharp/blur2.5/f1.2 | measure | bias +21.38 % · scatter 2.89 % | bias -0.52 % · scatter 1.00 % | Moffat: bias -9.31 % · scatter 3.63 % |
| sharp/blur2.5/f2.6 | measure | bias -2.50 % · scatter 0.54 % | bias -0.08 % · scatter 1.00 % | Moffat: bias -3.11 % · scatter 0.00 % |
| sharp/blur2.5/f8.0 | measure | bias -2.07 % · scatter 0.42 % | bias -0.04 % · scatter 0.89 % | n/a (the ERP accepted no star) |
| mixture/b | measure | bias +3.28 % · scatter 0.63 % | bias -1.70 % · scatter 1.06 % | Moffat: bias -0.70 % · scatter 0.60 % |
| mixture/b | analysis-mode | — | bias +7.99 % · scatter 1.50 % | |

| family | β sensitivity (betas), BN → after | Auto β pick, BN → after |
| ---- | ---- | ---- |
| sharp/m4/f2.6 | median +9.60 % → +8.64 % | beta 4 → beta 4.032 |
| sharp/m2.5/f2.6 | median +10.02 % → +9.20 % | beta 2.5 → beta 2.526 |
| sharp/gauss/f2.6 | median +8.72 % → +8.12 % | beta 10 → beta 99.942 |
| mixture/b | median +9.46 % → +9.64 % | beta 2.5 → beta 1.813 |
| resampled/f2.0 | median +8.80 % → +7.87 % | beta 10 → beta 7.43 |

- The measure path tracks pointfit on every profile at F ≥ 2: Gaussian and `blur2.5` read −0.04 … −0.29 % where BN
  read −2.07 … −3.01 % (continuous β, R-OSP-6). At F 1.2 the bias is gone (`+0.22 %`, `+0.45 %`, `-0.52 %`). The
  scatter is larger; the population is fainter (section 2).
- **`analysis-mode` reads non-Moffat-4 profiles off by a few percent** (`sharp/gauss/f2.6` `bias -6.64 %`,
  `mixture/b` `+7.99 %`): it fits Moffat 4 by default (R11b-12). This is Plan C's input (section 6).
- Divergent (pointfit): measure 56 family-planes (BN 77), `analysis` 70 (BN 67). The largest measure line is still
  `resampled/f2.0 p0: [pointfit] bias +24.22 % · scatter 0.95 % | ERP Moffat: [pointfit] bias +24.33 %` (section 7).

### Row 4: eccentricity and angle

| family | path | round-star floor, BN | after (n) | ERP |
| ---- | ---- | ---- | ---- | ---- |
| shape/f2.6/e0.0/t0 | measure | median e 0.1151 · scatter 0.0819 | median e 0.1689 · scatter 0.1502 (n 522) | Moffat: median e 0.1239 · scatter 0.0561 |
| shape/f2.6/e0.0/t0 | analysis-mode | — | median e 0.1715 · scatter 0.1456 | |
| shape/f4.0/e0.0/t0 | measure | median e 0.1033 · scatter 0.0747 | median e 0.1665 · scatter 0.1501 | Moffat: median e 0.1064 · scatter 0.0435 |
| sharp/m4/f1.2 | measure | median e 0.1388 · scatter 0.0885 | median e 0.2304 · scatter 0.1355 | Moffat: median e 0.1467 · scatter 0.0466 |
| sharp/m4/f2.6 | measure | median e 0.1202 · scatter 0.0784 | median e 0.1681 · scatter 0.1386 (n 508) | Moffat: median e 0.1411 · scatter 0.0577 |
| sharp/m4/f2.6 | analysis | median e 0.2666 · scatter 0.1202 | median e 0.2666 · scatter 0.1202 | |
| sharp/m4/f2.6 | analysis-mode | — | median e 0.1719 · scatter 0.1398 | |
| sharp/m4/f8.0 | measure | median e 0.0868 · scatter 0.0666 | median e 0.1515 · scatter 0.1353 | Moffat: median e 0.0741 · scatter 0.0255 |
| sharp/gauss/f2.6 | measure | median e 0.1170 · scatter 0.0851 | median e 0.1627 · scatter 0.1400 | n/a |

| family | path | ecc (pointfit), BN → after | angle (prepixel), BN → after |
| ---- | ---- | ---- | ---- |
| shape/f2.6/e0.3/t30 | measure | bias -0.0047 · scatter 0.0332 → bias +0.0114 · scatter 0.0543 | rms 44.564 deg → rms 30.078 deg |
| shape/f2.6/e0.5/t30 | measure | bias -0.0016 · scatter 0.0125 → bias +0.0021 · scatter 0.0232 | rms 32.339 deg → rms 21.140 deg |
| shape/f2.6/e0.7/t30 | measure | bias +0.0003 · scatter 0.0060 → bias +0.0008 · scatter 0.0124 | rms 21.125 deg → rms 14.583 deg |
| shape/f4.0/e0.5/t30 | measure | bias -0.0006 · scatter 0.0104 → bias +0.0006 · scatter 0.0229 | rms 22.743 deg → rms 21.831 deg |
| shape/f4.0/e0.7/t30 | measure | bias +0.0002 · scatter 0.0050 → bias +0.0001 · scatter 0.0124 | rms 8.185 deg → rms 14.512 deg |
| shape/f4.0/e0.7/t30 | analysis-mode | — → bias +0.0002 · scatter 0.0123 | — → rms 15.485 deg |

- **The measure path's round-star floor reads higher than BN's on every family above** (`sharp/m4/f2.6` `0.1202` →
  `0.1681`), and its ecc scatter is about twice BN's. These cells pool the fainter population (section 2). The
  same-star reading, the owner ecc line, is WORSE: 0.09187 → 0.09339 above Fisher 300 (section 1). It is carried
  under the debt ruling. The worse cells above are population, not same-star: the t06a / t06b / t06b-f1 set-asides in
  `design-cumulative.json` (matched filter +32 % seeds; more faint fits; same-star ecc / angle unchanged or better on the
  same stars). The one same-star WORSE is the owner ecc line.
- The angle is now measured on the major axis (F-2): every efficiency is ≤ 1.24.
- Divergent (pointfit): measure 148, `analysis` 89. The largest is still `doubles/f4.0 p0: [pointfit] bias +0.4787 ·
  scatter 0.1861` (ERP `+0.4801`).

### Row 5: background under the star

| family | BN | after |
| ---- | ---- | ---- |
| background/flat | bias -0.076 sigma · scatter 0.100 | bias -0.053 sigma · scatter 0.119 |
| background/grad10 | bias -0.081 sigma · scatter 0.096 | bias -0.051 sigma · scatter 0.115 |
| background/vig20 | bias -0.084 sigma · scatter 0.099 | bias -0.052 sigma · scatter 0.123 |
| background/neb0.5 | bias -0.064 sigma · scatter 0.309 | bias -0.149 sigma · scatter 0.361 |
| background/neb3 | bias +0.251 sigma · scatter 1.964 | bias +0.053 sigma · scatter 2.023 |
| background/neb3_4096 | bias +0.998 sigma · scatter 1.055 | bias -0.241 sigma · scatter 1.091 |
| halo | bias -0.033 sigma · scatter 0.096 | bias -0.021 sigma · scatter 0.085 |
| density/d1000 | bias -0.034 sigma · scatter 0.138 | bias -0.006 sigma · scatter 0.199 |
| resampled/f2.0 | bias +0.166 sigma · scatter 0.215 | bias +0.059 sigma · scatter 0.139 |
| sharp/m4/f2.6 | bias -0.088 sigma · scatter 0.127 | bias -0.066 sigma · scatter 0.148 |

`analysis-mode` carries the per-star background too (`background/flat` `bias -0.052 sigma`); the legacy, register
and seeds paths still do not (probe gaps).

### Row 6: flux and the LN scale

Captured fraction `signal / flux_img`, measure path, after (BN in its own table):

| F | gauss | m10 | m4 | m2.5 | blur1 | blur2.5 |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 1.2 | median 0.9995 · scatter 0.0114 | median 0.9973 · scatter 0.0108 | median 0.9968 · scatter 0.0106 | median 0.9889 · scatter 0.0150 | median 0.9992 · scatter 0.0110 | median 0.9994 · scatter 0.0097 |
| 2.0 | median 0.9986 · scatter 0.0100 | median 0.9988 · scatter 0.0132 | median 0.9978 · scatter 0.0107 | median 0.9920 · scatter 0.0112 | median 0.9995 · scatter 0.0115 | median 0.9978 · scatter 0.0097 |
| 2.6 | median 1.0008 · scatter 0.0117 | median 0.9990 · scatter 0.0121 | median 0.9995 · scatter 0.0128 | median 0.9932 · scatter 0.0120 | median 0.9998 · scatter 0.0124 | median 0.9993 · scatter 0.0144 |
| 5.0 | median 0.9995 · scatter 0.0088 | median 1.0002 · scatter 0.0159 | median 0.9994 · scatter 0.0140 | median 0.9932 · scatter 0.0181 | median 1.0010 · scatter 0.0139 | median 0.9997 · scatter 0.0131 |
| 8.0 | median 0.9991 · scatter 0.0098 | median 1.0004 · scatter 0.0133 | median 0.9992 · scatter 0.0135 | median 0.9865 · scatter 0.0191 | median 0.9999 · scatter 0.0141 | median 0.9999 · scatter 0.0142 |

BN read 0.7496–0.9157 in the same table. The legacy `analysis` cells are BN's exactly. `analysis-mode` reads 0.9813
… 1.0033, the furthest from 1 at F 8.0 on `gauss` (`median 0.9813 · scatter 0.0313`).

LN scale against `s_true` (`ln beta auto`, plane 0), BN → after:

| family | target | f1_gc, BN → after | psfSel, BN → after |
| ---- | ---- | ---- | ---- |
| truthcheck | t3 | +1.16 % → s 1.1130 vs s_true 1.1: +1.18 % | +0.34 % → s 1.1032 vs s_true 1.1: +0.29 % |
| truthcheck | slope | +0.0040 → +0.0072 %/px^2 (intercept +0.320 %) | +0.0088 → +0.0032 %/px^2 (intercept -0.021 %) |
| density_pairs/d200 | t3 | +1.26 % → s 1.2657 vs s_true 1.25: +1.25 % | +0.32 % → s 1.2500 vs s_true 1.25: +0.00 % |
| density_pairs/d600 | t3 | +4.40 % → s 1.3048 vs s_true 1.25: +4.38 % | +0.59 % → s 1.2559 vs s_true 1.25: +0.47 % |
| density_pairs/d600 | slope | +0.2538 → +0.2542 %/px^2 | +0.0386 → +0.0192 %/px^2 |
| pairs/iso/m4 | t60 | +7.06 % → s 1.1773 vs s_true 1.1: +7.02 % | +0.56 % → s 1.1112 vs s_true 1.1: +1.02 % |
| pairs/iso/g | t60 | +5.75 % → s 1.1700 vs s_true 1.1: +6.36 % | -0.97 % → s 1.1033 vs s_true 1.1: +0.30 % |
| pairs/iso/blur4 | t60 | +4.68 % → s 1.1520 vs s_true 1.1: +4.73 % | -7.28 % → s 1.1131 vs s_true 1.1: +1.19 % |
| pairs/main/m4 | t60 | +11.34 % → s 1.2198 vs s_true 1.1: +10.89 % | +0.32 % → s 1.0946 vs s_true 1.1: -0.50 % |
| pairs/main/m25 | t60 | +11.12 % → s 1.2203 vs s_true 1.1: +10.93 % | +0.98 % → s 1.0907 vs s_true 1.1: -0.85 % |
| pairs/main/g | t60 | +9.60 % → s 1.2148 vs s_true 1.1: +10.44 % | -0.93 % → s 1.0931 vs s_true 1.1: -0.62 % |
| pairs/main/m4w10 | t60 | +11.84 % → s 1.2317 vs s_true 1.1: +11.97 % | +0.60 % → s 1.0951 vs s_true 1.1: -0.45 % |
| pairs/main/m4w10 | slope | +0.4021 → +0.4072 %/px^2 (intercept +1.177 %) | +0.0305 → -0.0140 %/px^2 (intercept +0.040 %) |
| osc_pairs/f2.6 | t3 (p0) | +2.18 % → s 1.2760 vs s_true 1.25: +2.08 % | +1.67 % → s 1.2759 vs s_true 1.25: +2.07 % |

- F1 (`f1_gc`) is Plan B's and reads as in BN: slopes `+0.1418` (`pairs/iso/blur25`) … `+0.4072 %/px^2`
  (`pairs/main/m4w10`) on the 19 `pairs` families *(d range)*.
- psfSel holds every pure and blurred pair within ±1.72 %, with slopes −0.0472 … +0.0555 %/px^2 *(d range)*. The
  PA-14 LN table in F/`noreg-vs-baseline.txt` lists every LN row, never counted: `3220, changed 3220`.
- Divergent (E): F1 against the ERP's LN, 63 / 64 targets (β 4 / auto). The largest is `pairs/main/m4w10 p0 t60: s
  1.2317 vs s_true 1.1: +11.97 % | ERP s 1.1616 (+5.60 %)`.

### Row 7: frame aggregates

| family | path | frame FWHM, BN → after | frame ecc, BN → after |
| ---- | ---- | ---- | ---- |
| sharp/m4/f1.2 | measure | 1.4397 → 1.3711 | 0.1126 → 0.1888 |
| sharp/m4/f2.0 | measure | 2.1323 → 2.1305 | 0.0947 → 0.1118 |
| sharp/m4/f2.6 | measure | 2.7086 → 2.7022 | 0.0872 → 0.1013 |
| sharp/m4/f5.0 | measure | 5.0798 → 5.0532 / 5.0522 | 0.0678 → 0.0820 / 0.0834 |
| sharp/gauss/f2.6 | measure | 2.6188 → 2.6817 | 0.0936 → 0.0959 |
| sharp/gauss/f8.0 | measure | 7.8686 → 8.0069 | 0.0623 → 0.0656 |
| sharp/m2.5/f2.6 | measure | 2.7197 → 2.7125 | 0.0867 → 0.0998 |
| sharp/blur2.5/f2.6 | measure | 2.7192 → 2.7754 | 0.0940 → 0.0965 |
| mixture/a | measure | 3.1662 → 3.1836 | 0.0851 → 0.0981 |
| mixture/a_clip | measure | 3.4554 → 3.4437 | 0.0787 → 0.0965 |
| mixture/b | measure | 4.0020 → 3.7526 | 0.0867 → 0.0970 |
| mixture/b_clip | measure | 4.4144 → 4.4352 | 0.0892 → 0.1068 |
| density/d1000 | measure | 3.1026 → 3.0881 | 0.0856 → 0.0983 |
| trails | measure | 4.0849 → 4.3488 | 0.8125 → 0.8366 |

The statistic changed (F-3): `residual-weighted mean over the accepted fits not flagged saturated`. The legacy
`analysis` cells are BN's. Against the truth: 0 of 58 images beyond the F-4 bar (section 1).

### Row 8: OSC per plane

| family | plane | FWHM (pointfit) on the VNG plane, BN → after | on the ideal plane, after | flux ratio VNG, BN → after |
| ---- | ---- | ---- | ---- | ---- |
| osc/f2.0/m4 | 0 | +8.28 % → bias +2.24 % | bias +0.46 % (n 510) | 0.8865 → median 1.0711 |
| osc/f2.0/m4 | 1 | +12.63 % → bias +6.16 % | bias +0.35 % (n 517) | 0.8512 → median 0.9785 |
| osc/f2.0/m4 | 2 | -0.79 % → bias -4.96 % | bias +0.35 % (n 510) | 0.8805 → median 1.0864 |
| osc/f2.6/m4 | 0 | +2.75 % → bias +1.31 % | bias +0.21 % (n 515) | 0.8808 → median 1.0626 |
| osc/f2.6/m4 | 1 | +7.39 % → bias +5.50 % | bias +0.32 % (n 516) | 0.8509 → median 0.9959 |
| osc/f2.6/m4 | 2 | -6.71 % → bias -3.27 % | bias +0.21 % (n 500) | 0.8852 → median 1.0474 |
| osc/f3.5/m4 | 0 / 1 / 2 | +2.16 / +5.03 / -1.51 % → +2.61 / +4.66 / -2.07 % | +0.07 / +0.16 / +0.13 % | 0.8692 / 0.8406 / 0.8483 → 1.0417 / 1.0131 / 1.0375 |
| osc/f5.0/m4 | 0 / 1 / 2 | +1.38 / +0.54 / -1.35 % → +2.15 / +1.71 / -1.39 % | +0.10 / +0.05 / +0.00 % | 0.8497 / 0.8408 / 0.8373 → 1.0272 / 1.0132 / 1.0202 |
| osc/f2.6/blur1 | 0 / 1 / 2 | +0.30 / +1.84 / -6.63 % → +0.71 / +6.30 / -4.02 % | -0.29 / -0.30 / -0.24 % | 0.9276 / 0.9106 / 0.9057 → 1.0769 / 1.0129 / 1.0493 |

On the ideal planes the measure path is within ±0.46 % of pointfit, and the flux ratio is 0.9961–1.0005. On the VNG
planes G still reads wide and B narrow, and R / B flux reads 2.0–8.6 % high *(d)*. Section 7.

### Row 9: cost

| family | path | BN | after |
| ---- | ---- | ---- | ---- |
| sharp/m4/f2.6 | analysis | wall median 0.05 s · peak RSS 143 MB | wall median 0.04 s (max 0.04) · peak RSS 143 MB (one path) |
| sharp/m4/f2.6 | analysis-mode | — | wall median 0.14 s (max 0.14) · peak RSS 153 MB (one path) |
| sharp/m4/f2.6 | stack | wall median 0.21 s · peak RSS 143 MB | wall median 0.20 s (max 0.20) · peak RSS 161 MB (multi-path: measure+betas+register) |
| density/d1000 | stack | wall median 0.33 s · peak RSS 139 MB | wall median 0.37 s (max 0.37) · peak RSS 163 MB (multi-path: measure+betas+register) |
| background/neb3_4096 | measure | wall median 0.39 s · peak RSS 452 MB | wall median 0.41 s (max 0.41) · peak RSS 572 MB (one path) |
| background/neb3_4096 | analysis-mode | — | wall median 0.35 s (max 0.35) · peak RSS 542 MB (one path) |
| osc/f2.6/m4 | stack | wall median 0.76 s · peak RSS 232 MB | wall median 0.79 s (max 0.79) · peak RSS 263 MB (multi-path: measure+betas+register) |

Real-data cost is in section 4.6.

### Row 10: selection and seeds

| family | image | Auto β, BN → after | saturated in the Auto β sample (first 64 seeds), BN → after |
| ---- | ---- | ---- | ---- |
| saturated/f2.6/n20 | 0 / 1 | 4.00 / 4.00 → 4.00 / 4.03 | 0 / 0 → 20 / 20 |
| saturated/f2.6/n20_14bit | 0 / 1 | 4.00 / 4.00 → 3.87 / 4.12 | 20 / 20 → 20 / 20 |
| saturated/f2.6/n60 | 0 / 1 | 4.00 / 4.00 → 4.06 / 4.03 | 3 / 1 → 60 / 60 |
| sharp/gauss/f2.6 | 0 | 10.00 → 99.94 | 31 → 31 |
| sharp/m10/f2.6 | 0 | 10.00 → 10.09 | 28 → 31 |
| sharp/m4/f2.6 | 0 | 4.00 → 4.03 | 0 → 31 |

| family | seed flux rank, BN → after (seeds) |
| ---- | ---- |
| sharp/m4/f2.6 | Spearman flux 0.3625 (all) → 0.9995 (all) · 0.9994 (unsaturated, n 508) |
| saturated/f2.6/n60 | 0.5983 → 0.9992 (all) · 0.9990 (unsaturated, n 989) |
| density/d1000 | 0.0429 → 0.9991 (all) · 0.9990 (unsaturated, n 3053) |
| halo | -0.3107 → 0.9983 (all) · 0.9980 (unsaturated, n 522) |
| background/neb3 | 0.8443 → 0.9759 (all) · 0.9677 (unsaturated, n 307) |

- The seeds now rank by flux (Spearman 0.9995 on `sharp/m4/f2.6`), so the Auto β sample holds every saturated
  star. β on the Moffat 4 frames reads 3.87–4.12 with up to 60 saturated stars in the sample (F-3, F-4).
- The LN selection rows read as in BN (`selected 254 of 416 matched pairs (rule snr)` on `saturated/f2.6/n60` image
  0); they are LN-side, Plan B's.
- Saturation flag vs truth: section 1 (F-3).

### Row 11: stability

| family | path | noise twins, BN → after | base vs deep (2× flux), BN → after | Auto β base → deep, after (`twin-saturation.txt`) |
| ---- | ---- | ---- | ---- | ---- |
| twins/sharp/m4/f1.2 | measure | median 0.05 % → 0.26 % | median 7.71 % → 0.33 % | 3.98 → 3.95 |
| twins/sharp/m4/f2.0 | measure | median 0.05 % → 0.03 % | median 9.99 % → 0.06 % | 4.05 → 4.03 |
| twins/sharp/m4/f2.6 | measure | median 0.01 % → 0.06 % | median 2.93 % → 0.16 % | 4.03 → 4.00 |
| twins/sharp/m2.5/f1.6 | measure | median 0.01 % → 0.08 % | median 15.93 % → 0.06 % | 2.51 → 2.51 |
| twins/sharp/m2.5/f2.0 | measure | median 0.02 % → 0.01 % | median 3.89 % → 0.10 % | 2.51 / 2.52 → 2.52 / 2.52 |
| twins/mixture/b | measure | median 0.05 % → 0.05 % | median 2.62 % → 0.07 % | 1.81 → 1.81 |
| twins/sharp/m2.5/f2.0 | analysis | median 0.75 % → 0.75 % | median 27.50 % → 27.50 % | — |

- Noise twins on the measure path read 0.00 % (`twins/sharp/m4/f1.6`) to 0.26 % (`twins/sharp/m4/f1.2`) over 46
  families *(d)*. The `f1.2` families are Task 14b's floor-flag families.
- `Auto β agrees` prints `0.0 % of 1 pairs`: with a continuous β exact equality is no longer the measure. The β bar is
  the 10 % rule of section 1.
- Ladder: `Spearman 1.0000 · largest inversion 0.0000 px` on measure, `analysis` and `analysis-mode`.

## 4. Real data

### 4.1 Injected stars (`manifests/inject.json`; the injected stars are the referee)

The measure path, BN → after (SI Rows 1, 3, 4, 6):

| injected group | plane | FWHM (pointfit) | ecc (pointfit) | captured fraction | completeness |
| ---- | ---- | ---- | ---- | ---- | ---- |
| calibrated/hc11B-f1__mono__G | 0 | -3.68 % → bias +0.05 % · scatter 3.37 % | +0.1345 → bias +0.2488 · scatter 0.2297 | 0.8353 → median 1.0399 · scatter 0.0606 | 100.0 % of 72 → 96.9 % of 161 expected |
| calibrated/hc11B-f1__mono__L | 0 | -0.57 % → bias -0.31 % · scatter 3.35 % | +0.1346 → bias +0.2645 · scatter 0.2050 | 0.8275 → median 1.0630 · scatter 0.0470 | 100.0 % of 67 → 70.0 % of 170 expected |
| calibrated/star-lab-mono2__mono__L | 0 | +5.33 % → bias -0.99 % · scatter 5.77 % | +0.1480 → bias +0.2576 · scatter 0.2176 | 0.8228 → median 1.0342 · scatter 0.0465 | 100.0 % of 92 → 94.0 % of 184 expected |
| mosaic/tierA-lnB-f1__osc | 0 | -0.24 % → bias -6.78 % · scatter 6.48 % | +0.2364 → bias +0.3705 · scatter 0.1925 | 0.9038 → median 1.8134 · scatter 0.4394 | 98.2 % of 114 → 90.4 % of 177 expected |
| mosaic/tierA-lnB-f1__osc | 1 | +0.67 % → bias +1.25 % · scatter 5.64 % | +0.2460 → bias +0.3313 · scatter 0.2378 | 0.8679 → median 1.0847 · scatter 0.1023 | 98.2 % of 110 → 96.2 % of 184 expected |
| mosaic/tierA-lnB-f1__osc | 2 | -5.85 % → bias -6.22 % · scatter 7.18 % | +0.2575 → bias +0.3488 · scatter 0.2274 | 0.8665 → median 1.0787 · scatter 0.1333 | 99.0 % of 101 → 93.7 % of 175 expected |
| raw/corpus | 0 | -1.52 % → bias -1.68 % · scatter 3.83 % | +0.2025 → bias +0.3006 · scatter 0.2148 | 0.8389 → median 1.0583 · scatter 0.0565 | 96.9 % of 161 → 90.0 % of 250 expected |
| raw/corpus | 1 | +9.06 % → bias +10.27 % · scatter 4.46 % | +0.2215 → bias +0.2723 · scatter 0.1791 | 0.8416 → median 0.9758 · scatter 0.0494 | 97.4 % of 39 → 75.0 % of 60 expected |
| raw/corpus | 2 | -2.22 % → bias +4.56 % · scatter 6.58 % | +0.2861 → bias +0.3274 · scatter 0.1536 | 0.8900 → median 1.0471 · scatter 0.1098 | 97.3 % of 37 → 86.5 % of 52 expected |
| raw/star-lab-mono2__osc__raw | 0 | -1.90 % → bias -8.33 % · scatter 7.75 % | +0.2853 → bias +0.3758 · scatter 0.1797 | 0.9041 → median 1.9441 · scatter 0.4610 | 98.4 % of 122 → 89.4 % of 188 expected |
| raw/star-lab-mono2__osc__raw | 1 | -0.43 % → bias -5.51 % · scatter 6.02 % | +0.2899 → bias +0.3767 · scatter 0.1892 | 0.8744 → median 1.9952 · scatter 0.2093 | 96.7 % of 123 → 93.8 % of 194 expected |
| raw/star-lab-mono2__osc__raw | 2 | -6.29 % → bias -12.69 % · scatter 7.17 % | +0.2824 → bias +0.3653 · scatter 0.2046 | 0.8866 → median 2.3380 · scatter 0.5055 | 96.3 % of 107 → 90.5 % of 179 expected |

- The completeness denominators are `fisher_snr >= 6` now. The judged raw-completeness and injected-FWHM bars are in
  section 1.
- The mono captured fraction reads 1.03–1.06. Task 10b recorded the +4–6 % as a limit: the real halo carries beyond
  the fitted model (plan l.3694, I.1).
- **The OSC captured fraction is a scorecard / truth artefact, explained** (the recorded limit I.1, plan l.3694,
  extended to the OSC families and sized by `c`): it reads `median 1.8134` on the mosaic's plane 0 and `1.9441` /
   `1.9952` / `2.3380` on the Pro raw planes, against BN's 0.87–0.90. Row 6 on inject frames divides the measured flux (fitted FWTM flux x the plane's aperture correction `c`, F-5 / R-OSP-7, taken from the frame's REAL stars) by the flux of an injected Moffat-beta-4 star that has no wing. On undersampled OSC planes (beta at the 1.5 floor) the model holds ~40-75 % of a real star's 5F flux, so `c` is 1.9-2.7 and the injected stars read x`c`. Raw 0004 p0 / p1 / p2 `c` 2.097 / 2.099 / 2.437 → pooled 1.944 / 1.995 /
  2.338; mosaic p0 `c` 1.956 / 1.860 / 1.166 per frame → pooled 1.813 (the 0.44 scatter is between frames; within a
  frame 0.07–0.11). The c-free model / truth is 0.95–1.02; the injected stars' own aperture sum / truth 0.95–1.03 on all
  18 planes; real stars' same star across frames agrees to ≤ 3 % on G / B after `c`. It entered with 57b4d49f (F-5);
  the baseline probe had no `c`. Source: `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-14-report-capfrac.md`.
  Lab follow-up (not done): Row 6 prints the c-free ratio and `c`; the proper truth injects the frame's own empirical
  PSF (`inject.py` `_fwhm_pass` stamp). Product findings: section 7.
- The ecc column is the round-star floor of injected round stars on real frames: 0.25–0.38 here against 0.13–0.29 in
  BN, on a larger and fainter set of matched injected stars (the completeness column).
- `analysis-mode` keeps 500 stars per frame, so few injected stars are among them: `51.6 %` (Horse C11 G) down to
  `1.7 %` (`raw/corpus`) of expected.

### 4.2 Task 13's checkpoint at master level (R-OSP-11; RM l.3412–3490)

Each dataset is a clone of its before tree, run from Measure with the before run's reference pinned. The attribution
runs reproduce the before trees exactly, so every delta is Plan A's own. Copied from RM's table (B = b6c5d5b3 against
the before run):

| Row | Horse C11 | run B | set 204 |
| ---- | ---- | ---- | ---- |
| master median / MAD | G +0.03 / -0.18 %; L +10.32 / +2.01 % (1/6 pass) | L +0.22 / +2.12 %, OSC p0 +0.36 % (0/4) | mono +0.147 / +3.54 %, drizzled +0.131 %, OSC +0.38 / -0.22 / -0.31 % (0/8) |
| master noise | G -6.48 %, L -10.52 % (less noise) | L +0.62 %, OSC p0 +1.68 % (within) | mono +3.83 %, drizzle2x +4.60 %, OSC p0 +1.97 % (5/8) |
| master FWHM | G +0.185 %, L -0.42 % (beta 3.30 vs 3.39) | L -1.82 %, OSC p0 -1.17 % (sharper) | mono -2.04 %, drizzle2x -2.61 %, OSC p0 -3.37 % (5/8) |
| weights rho (bar >= 0.9) | **0.4642** (G -0.170, L 0.520; 78 movers, each with a scorecard row) | 0.9553 (L 0.942, OSC 0.814; 63 movers) | 0.9595 (mono 0.936, OSC 0.833; 63 movers) |
| LN s median (bar +-0.5 %), scatter | +3.00 %, 0.20 % | -0.95 %, 0.08 % | -0.97 %, 0.08 % |
| Measure, one run (before) | 2.21 (1.65) min = 1.338, drift-ambiguous | 9.78 (3.55) = 2.752 | 9.88 (3.62) = 2.726 |

Rulings (RM R-OSP-11):
- **R13-3** (Fable for the owner, 2026-10-16). Plan A's weights and masters are ACCEPTED. The ERP's rows say the
  weights track the ERP better: set 204 mono Spearman(ERP PSFSW, weight) 0.931 → 0.956, OSC 0.903 → 0.936. The
  sharper, noisier master is PSFSW on better inputs. Horse C11's ρ 0.46 comes from the old fixed-ellipse flux, and
  its L +10.3 % from pedestal data. The master-level shifts are a re-definition, report-only.
- **R13-4.** The LN row is re-based (section 4.3).
- The Measure wall went to Task 13b, then 13c, then the fast-measurement cycle (section 4.6).

### 4.3 The LN harness row, re-based (R13-4; the re-base owed at Task 14)

The LN reference is the top-N-by-weight integration (`build_reference`). Plan A's weights changed its composition,
so the raw s_B / s_A reads the reference's own star-flux change. The row is re-based to the reference-corrected
ratio (s_B / s_A) ÷ (phot_B / phot_A) − 1, bar ±0.5 %, with the raw ratio, the top-N overlap and the anchors
printed beside. Numbers from lab `t13-real/fable-ruling-ln.md` (l.7–12, l.17–20). No run.

| dataset, plane | raw s_B / s_A (reported) | phot_B / phot_A | reference-corrected | bar | top-N overlap, anchor |
| ---- | ---- | ---- | ---- | ---- | ---- |
| set 204 mono | 0.9877 | 0.9897 | −0.21 % | met | top-20 shared 16 / 20; anchor 29053 both |
| set 204 OSC p0 / p1 / p2 | 0.9933 / 0.9903 / 0.9887 | 0.9939 / 0.9909 / 0.9874 | −0.06 / −0.06 / +0.13 % | met | top-20 shared 11 / 20; anchor 29034 → 28949 |
| run B | −0.95 % (median) | — | reported, n/a (no truth CSVs) | — | — |
| Horse C11 | +3.00 % (median) | — | reported, n/a; closed: `referenceFrames` 5, the top-5 sets of A and B disjoint in both groups | — | disjoint |

Against aperture photometry on the registered planes the median |gap| moves A → B 0.348 → 0.339 % pooled (RM).

### 4.4 The star-test corpus after Task 14b (comparison only: no truth on these frames)

BN's format (§4.3). The medians are derived from F/`corpus-table.md` with BN's method (median ours, median ERP,
median of the per-frame ratio, Spearman). The `analysis` path is the legacy one, unchanged.

| Set | Path | Median FWHM ours / ERP *(d)* | Ours ÷ ERP *(d)* | Spearman *(d)* | BN ours ÷ ERP |
| ---- | ---- | ---- | ---- | ---- | ---- |
| m42 (76 of 77) | measure | 2.3667 / 2.3283 | 0.9942 | 0.9899 | 1.0243 |
| m42 (77) | analysis | 2.3014 / 2.3362 | 0.9708 | 0.9937 | 0.9708 |
| m101 (28, without the 2 known-bad ERP rows) | measure | 5.7896 / 5.8582 | 0.9513 | 0.9874 | 0.9015 |
| m101 (28) | analysis | 5.8318 / 5.8582 | 0.9732 | 0.9907 | 0.9732 |
| m101 2025 (15) | measure | 5.0003 / 5.2783 | 0.9469 (printed, R1) | 0.9321 | — |
| m101 2023 (13, without the 2 known-bad rows) | measure | 11.7344 / 12.2730 | 0.9573 (printed, R1) | 0.9780 | 0.8219 (BN, 2023 set) |
| singles (32 of 38) | measure | 2.6650 / 2.5933 | 0.9973 (printed, R3) | 0.9828 | 1.0127 |
| singles (32 of 38) | analysis | 2.7961 / 2.5933 | 0.9967 | 0.9729 | 0.9967 |

- **m101** (F/`corpus-bars.txt`, Task 14b's R1): `median 0.9469 over 15/15 >= 0.9` (2025), `median 0.9573 over
  13/13 >= 0.9` (2023), `30 of 30 not at the floor`, `0015` `0.9505 within +-1 % of 0.95`. The lowest counted frame
  is `2025-05-24_02-13-34_R_…_0009` at 0.8705 *(d)*, above the 0.85 floor. At Step 1 (69b4710b) the same column
  read 0.379 / 0.094: the field-width collapse that Task 14b fixed.
- **Broad singles**: `2024-03-01 … M 51 …H…0060` measure 13.8918 px on 319 fits (BN 6.53 on 27), ERP 14.017,
  analysis 13.3899; `ratio 0.9911 >= 0.85, at floor False` (R2). `m82-outoffocus` measure 16.7697 (BN 11.43), ERP
  17.76, analysis 16.3109.
- **Known-bad m101 ERP rows**, reported and never tuned toward: `…_0002.fit` ERP 1.3259 / 0.727, ours measure 19.6756
  / 0.7667 (BN 3.3054 / 0.6386); `…_0004.fit` ERP 32.32 / 0.947, ours measure 20.0930 / 0.7578 (BN 4.8155 /
  0.5926).
- **`_DSC5767.fit`** (no ERP row): measure 3.7011 px on 14 200 fits, where BN had 0 seeds; analysis 3.6916 on 500.
- **Floor flag on real frames**: `2026-04-10_00-06-14__-15.00_120.00s_37.15_0023.fits: atFloor True (allowed)`, a
  sub-floor star field (FWHM 1.3442 under both arms). The Gamma Scuti single `…_0010` is flagged too (RM R-OSP-15).
- **Probe gaps (M3)**, recorded, not misses: m42 `0054` (trailed, 0 fitted under `off` too) and the 5 XISF singles
  (FITS-only reader).
- **M2's share** (fits narrower than 0.715·F_field, reported, not ruled): m101 `0000` 279 / 1891 = 14.8 % (`off`
  18.3 %), `0010` 208 / 1623 = 12.8 % (`off` 15.1 %).

### 4.5 The calibrated sets under Task 14b (R4; T14B/`e4-r4-*.txt`, Step 5 at c8cdc1b3)

| Set | Field width | Frame FWHM \|Δ\| vs `off` | Set level, median frame FWHM ÷ ERP | Other |
| ---- | ---- | ---- | ---- | ---- |
| set 204 (407 planes, 197 artifacts) | 407 of 407 within 2 % or explained; min / median / max 1.672 / 2.404 / 4.774 | max 0.05 % (artifact 1013 p0), median 0.000 % | 1.0619 → 1.0619 | seeds 718937 vs 718937 (1.0000); 0 at the floor; `VERDICT: PASS` |
| Horse C11 (106 planes; Q7) | 106 of 106 within 2 % or explained; moved > 2 %: 14 planes, explained by band 14, bracket 9, erp 4, nearer 3; unexplained 0 | max 1.12 % (artifact 738 p0, 91 judged, 13 exempt), median 0.000 % | 0.9592 → 0.9590 | 0 at the floor; `VERDICT: PASS` |

Under Q7's reading the estimator's own noise is the bar's size on Horse C11: the HSM bootstrap sd is 2.0 %, the R4
bar. Step 7 (lab 2739765) ran set 204 run 7 and Horse C11 run 6 at the pinned references. `detection median plane
built` fired 92 / 106 times, once per White plane; `field width at the clamp floor` fired 0 / 0 times. Horse C11
`L 0010` / `L 0011` were refused as trailed, and run 6 integrated 104 of 106 frames. The gates re-ran at c10be63b on
run 7 (section 2). The outputs of 14b's measured head and of c10be63b are identical (section 2).

### 4.6 Cost on real data

| Measure | before | after | the ERP | file |
| ---- | ---- | ---- | ---- | ---- |
| set 204 Measure stage (dev Mac) | 3.61 min (A, ed5fa8de) | 9.88 min (13-P) → 7.95 min (13b) | 3.77 min (warm-up corrected) | RM R-OSP-11, R-OSP-12 |
| set 204 whole run (dev Mac) | 1237.7 s (13c start) | 1081 s (13c) → **794.1 s** (fast measurement, N / H 0.734) | 1005 s | RM R-OSP-13, R-OSP-14 |
| Horse C11 whole run (runner, 164 lights) | 377 s (13c H) | 317 s (13c) → 305 → **267 s** (69b4710b) | 2787.6 s | RM R-OSP-13, R-OSP-14; `2026-10-18-fast-measurement.md` l.141 |
| m42 Analysis mode, per frame (B3) | 0.70 s (Task 12) | 0.464 s (13b) → 0.500 s, 0.937 × 69b4710b (14b); the legacy path 0.234 s *(d)* | — | T14B/`e4-cost.txt`; F/`wall-amode-final.txt` |
| route E median pass | — | 0.262 / 0.193 ms / Mpx; run 7 Measure 294.9 s, of which the median pass is ~0.5 s | — | T14B/`e4-cost.txt` |

## 5. What Plan B starts from

PA-14: the LN rows are printed in their own table and never counted. The Plan B re-baseline starts from this run's
scorecard (SC, sha256 `ac2c6914…`).
- **F1** (`arm f1_gc`, the LN estimator now on): `pairs/main/m4w10` t60 `s 1.2317 vs s_true 1.1: +11.97 %` (BN
  `+11.84 %`); slopes `+0.1418` … `+0.4072 %/px^2` on the 19 `pairs` families *(d range)* (BN `+0.1533` …
  `+0.4021`); `truthcheck` slope `+0.0072 %/px^2`; `density_pairs/d600` t3 `+4.38 %` (SS Row 6).
- **psfSel** (`arm psfSel`): within ±1.72 % on every `pairs` cell (section 1, F-5); slopes `-0.0472` … `+0.0555
  %/px^2` *(d range)*. `sLib` tracks psfSel (`pairs/main/m4w10`: slope `-0.0140` both, t45 `0.9500` against `0.9501`).
- **Injected LN pairs** (`ln beta auto`, s_inj 0.8; SCI Row 6):

  | pair | plane | PSF signal | 2F circle | 3F circle |
  | ---- | ---- | ---- | ---- | ---- |
  | star-lab-mono2 mono L | 0 | median ratio 0.7911 vs s_inj 0.8: -1.11 % | -0.11 % | +0.60 % |
  | star-lab-mono2 OSC | 0 / 1 / 2 | -0.52 / -2.36 / -0.76 % | -0.31 / -0.40 / -0.70 % | -0.35 / -0.11 / -0.67 % |
  | tierA-lnB-f1 OSC | 0 / 1 / 2 | -0.00 / -0.68 / -0.30 % | -0.13 / +0.15 / +0.35 % | -0.25 / -0.11 / +0.17 % |

  BN's PSF-signal cells were +0.34 % (mono, auto) and -0.94 / +0.11 / -0.67 %, +0.15 / -0.27 / -0.55 % (OSC).
- **Nebula-sky LN, the "before"** (C-9): psfSel per side 13 of 34 cells over (pair-common 15); warped targets at t25
  +1.91 … +1.96 pp (lab `t10b-onebg/a1c/p1-table.txt`).
- **Real LN truth**: section 4.3 (R13-4) and lab `t13-real/ln-truth/gap-table.txt`, pooled median |gap| A 0.348 → B
  0.339 %, mono B −0.219 %.
- **Carried to Plan B by the spec and the plan**: the Measure aperture correction `c` open items (section 7: log `c` per plane first; weights and LN read `c`), the F-8 referee-pair test and the F-9 `c` = 1 floor (spec
  l.268–269); R11-3, the nebula flux error at 3.6–6 × σ_S (plan l.4093); Task 10's note that the growth / model fraction
  falls for fainter stars (all-pairs LN-like −4.7 %, plan l.3012); LN's second saturation definition
  (`unverdicted_clip_range`).

## 6. What Plan C needs

- **`analysis-mode` vs `legacy-analysis` on `sharp/*`** (46 (family, image) rows with both paths;
  F/`corpus-amode.md`, read from SS):

  | metric | analysis-mode median | legacy median | analysis-mode nearer the ideal |
  | ---- | ---- | ---- | ---- |
  | per-star FWHM \|bias\| vs pointfit, % | 4.795 | 0.51 | 10 of 46 |
  | per-star FWHM scatter, % | 1.185 | 2.215 | 21 of 46 |
  | per-star FWHM efficiency vs its own CRLB | 1.185 | 1.2 | 22 of 46 |
  | per-star ecc scatter | 0.1491 | 0.1084 | 9 of 46 |
  | round-star ecc floor (median e) | 0.1767 | 0.2341 | 23 of 46 |
  | completeness, % of each path's own expected set | 90 | 19.2 | 46 of 46 |
  | completeness, raw % of all truth stars | 79.5 | 17 | 46 of 46 |
  | bright coverage, % | 96.15 | 13.65 | 46 of 46 |

  The FWHM bias is the Moffat-4 default meeting non-Moffat-4 profiles (section 3 row 3; R11b-12). Plan C decides the
  profile.
- **On the corpus** (F/`corpus-amode.md`, budget 500; `corpus_amode.py` was fixed in lab 4abf69b, content identical; the probe
  directory it read is the c10be63b one, `t14-final/corpus-probe`):

  | set | frames | fitted median a / l | seeds median (a) | short a / l | FWHM median a / l | ecc median a / l | frame FWHM median a / l |
  | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
  | m42 | 77 | 500 / 500 | 9886 | 1 / 1 | 2.282 / 2.321 | 0.521 / 0.413 | 2.297 / 2.301 |
  | m101 | 30 | 297 / 234 | 2112 | 30 / 30 | 7.544 / 5.625 | 0.607 / 0.553 | 7.252 / 7.607 |
  | singles | 33 | 500 / 500 | 4867 | 7 / 15 | 2.573 / 2.855 | 0.582 / 0.515 | 2.548 / 2.816 |

  m42 `0054` (trailed) gives `analysis-mode` 0 fits of 13117 seeds, flagged `trailed`. m101 `0002` (2023) reads
  21.387 against legacy 2.983. Every m101 frame is short of 500: `analysis-mode` fits 116–430 of 1015–3929 seeds
  *(d range)*.
- **`_DSC5767` seed counts**: `analysis-mode` `seeds 15041, fitted 500, FWHM med 4.156, ecc med 0.756`; legacy `seeds
  None …, fitted 500, FWHM med 4.376`. BN read 0 seeds on the measure path (Task 12 per plane: 15041 / 15786 / 9039,
  RM l.3398).
- **The cost**: 0.500 s per m42 frame (B3, section 4.6), legacy 0.234 s *(d)* (Step 1).
- **Carried to Plan C** (RM R-OSP-10, R-OSP-15; plan l.2874, l.3012): Moffat 4 as the default (R11b-12); raw OSC as
  one plane and the CFA rule; the completeness bar at N ≥ seeds (Task 12 limit c); the XISF reader (`read_raw`, the 5
  XISF corpus singles are a probe gap); a CFA-masked plane gets `c` = 1; the legacy rustafits θ fix and the
  saturation-fraction setting; Task 14b's two silent limits, owned by Plan C's per-site table (section 7);
  `MEASURE_VERSION` 5 (one bump per merged plan).

## 7. Open items

- **VNG per-plane widths** (out of scope, spec l.384; BN verdict 3), after:
  - synthetic: `osc/f2.0/m4` p1 `bias +6.16 %` (BN `+12.63 %`), `osc/f2.6/m4` p2 `bias -3.27 %` (BN `-6.71 %`); the
    ideal planes stay within ±0.46 % (SS Row 8);
  - injected mosaic p2 `bias -6.22 %` (BN `-5.85 %`); the Pro raw lights p2 `bias -12.69 %` (BN `-6.29 %`), p0
    `-8.33 %` (SI Row 3);
  - the VNG-plane flux ratio reads 2.0–8.6 % high on R / B (SS Row 8).
- **Resampling broadening** (BN row 3): `resampled/f2.0` `[pointfit] FWHM^2 - truth^2: median 2.447 px^2 · scatter
  0.106` (BN `2.596`) against the nominal kernel `1.848 px^2`; the source growth (`sharp/m4/f2.0`, pointfit) `median
  0.035 px^2`; pointfit bias `+24.22 %` (BN `+25.54 %`), the ERP `+24.33 %` (SS Rows 3, 9; truth sanity
  `resampled-warp`).
- **The injected OSC captured fraction** (section 4.1): 1.81 on the mosaic's plane 0 and 1.94–2.34 on the Pro raw
  planes. Explained, not unruled: a scorecard / truth artefact, the recorded limit I.1 extended to the OSC families and
  sized by `c` (the injected Moffat-beta-4 star has no wing; `task-14-report-capfrac.md`).
- **The Measure aperture correction `c` on undersampled OSC planes** (product, found by that diagnostic; also the
  start of Plan B, section 5: weights and LN read `c`):
  - (a) `c` is neither persisted (`stacking/measure.rs:339`) nor logged at the level a run keeps, so real runs cannot be
    checked. First Plan B item: log `c` per plane.
  - (b) Hot pixels feed `c` on an uncorrected R plane: raw 0013 p0 `c` 1.334 vs 2.097 / 2.074 (0004 / 0007); 36 of its 64
    `c` stars are hot pixels; the same stars' 0004 → 0013 ratio 0.617 vs 1.007 / 0.970 on p1 / p2 (R weight ×0.40).
    Stacking is exposed only on a refused hot-pixel map (no `ATH_CHPX`) or a raw-mode measurement.
  - (c) Sharp R, probe path (own beta): mosaic 0176 → 0206 p0 ratio 0.863 after `c` vs ~0.985 on G / B (~×0.77 R
    weight); the product fits at the group beta (2.24 on set 204 OSC), so unverified there until (a).
  - (d) Brightness dependence on undersampled R / B: real-star aperture / model 1.57 (top 64) → ~1.2 (rank 500–2000) on
    raw 0004 p0; G flat (1.23 → 1.18). One `c` per plane overstates fainter stars there.
  - Set 204 run 7 LN's own `c` (log `ln scale: per-side ring`): OSC p0 1.00, p1 1.13, p2 1.12, mono 1.13, benign.
- **Missed bars and their rulings** (section 1):
  - F-4b 3c and 3b FWHM_m2 (R11b-13); the F-4b skew B-2 (R11b-4);
  - Task 13 masters and Horse C11 weights (R13-3); Task 13 LN s (R13-4, re-based);
  - M.2, the Task 13 Measure and 13b speed bars (owner, superseded and then met by the fast-measurement bar);
  - F-10 H 0029 (Q6) and the nebula-sky LN pairs (C-9, Plan B's before);
  - Task 14b's mono Measure peak (STOP C).
- **The debt, carried** (owner 2026-10-19, «Закрыть, долг перенести»):
  - OPEN on fwhm scatter (measure, pointfit): up 573 / down 355, worse 380 / better 177, median 0.01552 → 0.01682;
  - centroid scatter (measure): 1217 / 659, 632 / 85, 0.02371 → 0.0296;
  - centroid scatter (register): worse 161 / better 105 (F/`debt-final.txt`);
  - the Task 14 debt lines under the same ruling: the centroid F ≤ 2 split (re-read at c10be63b, t08-f3 → t14-final, F/`debt-lines-e.txt`: measure ×1.0140, mono ×1.0125;
    register ×1.0112, mono ×1.0120); the owner ecc items 6 / 7 (WORSE, F/`ecc-lines-e.txt`); the 11b
    single-frame ridge (+1.8 % per unit β; `0000` +4 %).
  - Owners: centroid → the owed per-frame pixel-phase correction; FWHM scatter → research (cause unknown).
- **Task 14b's limits** (RM R-OSP-15):
  - Two silent limits:
    1. A debayered (Mixed / Unknown) plane with warm pixels takes no median, so it raises no flag unless the pool
       itself lands under the floor. Triggers: a refused cosmetic map on an OSC stacking frame (no `ATH_CHPX`) and a
       debayered Analysis plane.
    2. On a masked plane whose median has some present cell, a mask that thins the windows.

    Owner: Plan C's per-site table.
  - Three recorded limits:
    - (a) an event inside a broad star's fit window refuses that one fit. L3 deficits against the twin: `spikes/a`
      −1.54, `b` −5.88, `c0000` −1.49, `c0010` −4.17 pp, identical under `off`. Owner: the fit-robustness cycle.
    - (b) half-sample-mode fragility on trail, flat-top and wind-shaken pools. On Horse C11, 14 of 106 planes move
      > 2 %, and the HSM bootstrap sd is 2.0 %. Owner: its own cycle.
    - (c) debayered planes. Owner: Plan C.
  - M2's sub-0.715·F_field share: m101 `0000` 14.8 %, `0010` 12.8 %, `spikes/c0000` 5.9 %. Reported, not ruled; the
    narrow-side refusal cycle starts there.
- **`MEASURE_PLANES_RESIDENT`**: 8 planes, fitted on an OSC frame, against a mono Measure peak of 8.82–8.83 planes,
  the same at 69b4710b. The working-set estimate is about 10 % low for mono (`docs/superpowers/open-items.md`).
- **The ERP refresh**: 15 of 204 units have no ERP file (the 14 `spikes/*` images and `under/gauss/f1.5`). The
  owner's rule is to re-run the ERP only when synthetic frames change.
- **Plan A's other recorded limits** (inventory §5):
  - the R / B κ under-read of 12–21 % (R11-2);
  - the C-12d heavy-wing ecc, +0.003;
  - the nebula `c` bias (`neb3` 0.880, `neb3_4096` 1.129) and the mesh noise ≈ 6× on `neb3` (spec F-10);
  - the per-stage swap max-rule misses on single N runs (fast measurement);
  - the 13c App Nap run (owner).
- **Repository state at the close**:
  - `one-star-path` c10be63b is not pushed (the owner's word);
  - rustafits d0f0b16e is on GitLab only and must reach GitHub before any GitHub push that pins it;
  - the Windows runner keeps `D:\t14b-avx2-target` (owner to delete).
