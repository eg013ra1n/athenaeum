# LN model fixes — acceptance (checkpoint A, 2026-10-05)

Spec `docs/superpowers/specs/2026-10-04-ln-model-fixes-design.md` §11; plan
`docs/superpowers/plans/2026-10-05-ln-model-fixes-plan.md` Task 9. Branch head for the runs:
`a12f8068` (Tasks 1–8: the background model at the scale, the derived scale, the pedestal
warning, the harness). PSF-signal scale path unchanged at this head (selection and F1 come
after checkpoint A).

## 0. Verdict

**The background model does what it was built for.** Horse C11 is the dataset the cycle exists
for, and it passes every row:

- the halo around run 8's star is back, to within 0.07 points of the no-LN control;
- no B node near that star is out of line (0 of 840);
- the IC 434 boxes agree with the control to within 0.08 points;
- `main` on the same copy reproduces run 8's deficit exactly.

The scale path is bit-identical to `main` on set 204 (197/197 sidecars) and run B (184/184).

**Three §11.3 rows miss as written.** They go to the owner (§4); none was re-sized or tuned:

1. **Master MAD ±1 %** misses on set 204 and run B. Both are the same LDN 1272 dust field. The model at the scale no longer follows sub-scale dust, as spec §13 says it will.
2. **FWHM ±1 %** misses on set 204 OSC blue (+2.67 %). This is an artifact of the FWHM estimator; an independent measurement of the same stars reads 1.0000.
3. **Master median within ±0.1 % of the LN reference** misses on `main` too (set 204: −0.4 … −1.4 %), so the bar is not met by the code the cycle starts from. The branch is closer than `main` in every case.

## 1. Method

| Dataset | Head | Baseline | Gate read |
| ---- | ---- | ---- | ---- |
| Set 204 (92 mono + 105 OSC) | `checkpoint.sh lnA` | `rerun-build.sh 60df59c4 lnA-main`, same evening | `tier1-compare.py --ln-grids` (A + `global_scale` bit-identity), `--numeric` |
| Run B (project, 4480 × 5760 grid) | `project-run.sh` on a `.backup` copy of the 6b-3 catalog | the 6b-3 tree `acc6b3-runB` (its LN code is `main` 60df59c4's; controller ruling, wall not same-evening) | `--ln-grids`, `--pairs` |
| Horse C11 (set 182, L 76 + G 30) | branch, LN on (a) | branch, LN off (b) = the control; `main` 60df59c4, LN on (c) | `master_phot.py --noise`, `halo_profile.py --search`, `nebula_boxes.py`, B-node read, `scale_pairs.py` |

Horse C11 copies were made with `horse-c11-prep.sh`:
- the dev catalog's 50 offset-50 dark links were deleted on the copy;
- stacking folders were redirected into the copy;
- run 8's config was applied through the API with `paths` dropped and `reference.mode = manual`;
- the reference was pinned by filename to `2024-09-29_04-30-32_L_-9.70_180.00s_0009.fits`. That is run 8's frame 10666, dev id 69845; run 8 was made on another machine's catalog.

The IC 434 boxes were chosen on (b) and committed (`8fa74976`, `boxes-ic434-G.json`) before any LN master was read. An automatic first pick landed in a bright star's halo near (5960, 1960). It was discarded on sight, and the committed boxes sit on the IC 434 striations.

## 2. Rows — set 204 (branch vs same-evening `main`)

| Row | Bar | Result |
| ---- | ---- | ---- |
| `A` grids and `global_scale` | bit-identical | **PASS** 197/197 |
| B move (`--ln-grids`) | reported | median 2.24e-3 of sky, p99 2.99e-2, max 1.69, 6.4 % of nodes > 1 % |
| Master median vs LN reference | ±0.1 % | branch mono 0.99627 / OSC 0.98645, 0.99371, 0.99395; `main` 0.99583 / 0.98551, 0.99300, 0.99326 — **miss on both**; branch closer |
| Master MAD | ±1 % | mono −1.28 % (central 80 %: +0.89 %), OSC R +0.41 / **G +1.12** / B +0.66 — **miss** (§3) |
| Master noise (MRS σ) | ±2 % | PASS (−0.01 … −0.12 %) |
| FWHM | ±1 % | mono +0.01 %, OSC R −0.01 / G −0.01 / **B +2.67 %** — **miss**, estimator artifact (§3) |
| Rejected fraction | ±0.3 pp | PASS (+0.001 pp) |
| Weights | ρ ≥ 0.99, top-20 ≥ 18 | PASS (ρ 1.0, 20/20) |
| Drizzle level / coverage | C-20, C-23 | same FAILs as the baseline (data facts) |
| Wall | reported | 14.50 vs 14.02 min; Normalize 2.21 vs 1.88 min (+20 s; spec estimate +10 s) |

Master SNR (`k/σ`) is a checkpoint-B row. The scale path here is bit-identical, so `k` is equal on both sides.

## 3. Why MAD and FWHM moved (set 204)

- **MAD.** The master was split into a 128-px block-median field (large scale) and the residual (small scale):

  | Plane | Small-scale MAD | Block spread |
  | ---- | ---- | ---- |
  | mono | −0.6 % | −0.6 % |
  | OSC R | +0.24 % | −0.1 % |
  | OSC G | +0.35 % | **+2.5 %** |
  | OSC B | +0.24 % | **+3.0 %** |

  The mono whole-frame drop sits at the frame edges; the central 80 % reads +0.89 %. The model at scale 1024 no longer removes dust structure smaller than the scale, which spec §13 states as intended ("Structures smaller than the scale are no longer followed by the background model: they are signal"). A whole-frame MAD on a nebula field moves with it.
- **FWHM, OSC blue.** `measure_probe`'s `fwhmPx` reads 3.159 vs 3.077 px with near-identical star lists (14 380 vs 14 446). An independent second-moment FWHM on ~1 900 identical isolated stars reads 4.5563 vs 4.5597 px, a ratio of 0.99998 (R 1.00000, G 1.00002). The jump is the estimator's.
  - It is the same +2.67 % Tier C recorded for C-29 (the binned background), which was reverted partly on that number. Worth a separate look; not this cycle's.

## 4. Rows — run B (branch vs the 6b-3 tree)

| Row | Result |
| ---- | ---- |
| `A` grids and `global_scale` | **PASS** 184/184 |
| B move | median 2.23e-3 of sky, p99 2.09e-2, max 4.67, 5.7 % of nodes > 1 % |
| Masters (`--pairs`) | median +0.05 … +0.08 % PASS; noise ±0.02 % PASS; FWHM −0.22 … 0.00 % PASS; MAD mono +0.49, OSC R +0.62 / **G +1.23** / B +0.69 — same LDN 1272 field as set 204 |
| Wall | 781 s (6b-3 record 810 s, a different evening) |

## 5. Rows — Horse C11 G (the background model's own test)

Scale-normalized read (L-21), `k` from `master_phot.py` against the branch's LN reference:
- branch LN on: k = 0.95844, σ = 2.2844e-5, SNR 41 955;
- LN off: k = 0.88329, SNR 41 513;
- `main` LN on: k = 0.95817, SNR 41 939.

The resolved scale was 1536 (auto, 9576 × 6388).

| Row | Bar | Branch LN on | `main` LN on (run 8 stand-in) |
| ---- | ---- | ---- | ---- |
| Halo ring 60–70 px, norm (control 28.32 %) | ±1 pt of control | 28.39 % (+0.07) **PASS** | 16.91 % (−11.4) |
| Halo ring 80–90 px (control 13.55 %) | ±1 pt | 13.51 % (−0.04) **PASS** | 4.44 % (−9.1) |
| Halo ring 100–120 px (control 7.41 %) | ±1 pt | 7.38 % (−0.03) **PASS** | 1.19 % (−6.2) |
| Raw rings 60–70 / 80–90 / 100–120 | reported | 26.15 / 12.45 / 6.80 % | 15.58 / 4.09 / 1.09 % (run 8: 15.5 / 4.1 / 1.1) |
| B nodes within 600 px vs 8-neighbour mean | none > 2 % of sky | 0 of 840; worst 0.017 % **PASS** | 252 of 1 932; worst 35 % |
| IC 434 boxes, norm (control 6.519 / 6.030 / 4.201 %) | ±1 pt | +0.040 / −0.028 / +0.078 **PASS** | −0.608 / −0.037 / −0.104 |
| IC 434 boxes, raw (control 5.758 / 5.326 / 3.711 %) | reported | +0.31 / +0.22 / +0.25 | −0.30 / +0.21 / +0.08 |
| Master median vs LN reference | ±0.1 % | 0.99888 (central 80 %), 0.99939 (whole) | 0.99839 / 0.99887 |
| ERP slope, per-frame scale vs the reference pipeline's | reported (bar at checkpoint B) | −0.549 %/px², r −0.72 (equal to `main`; run 8's record −0.491) | −0.549 %/px² |
| Pedestal warning (§11.5) | shown | plan (L and G, "OFFSET 1, GAIN 100"), export readiness (106 of 106), run summary with the corner ratio (L 50 %, G 51 % → 2.0×) | — |
| Wall | reported | 631 s | 631 s (LN off 601 s) |

`main` LN on reproduces run 8's ring deficit to the tenth of a point. The copy is a faithful stand-in.

## 6. Owed to the owner

- **The MAD row** (±1 % vs baseline) on a nebula field. Writer's reading: the expected effect of item 1, not a regression. The checkpoint-B MAD row compares the two arms against each other and is unaffected.
- **The FWHM estimator jump** on OSC blue (§3).
- **The master-median row's bar** (±0.1 % of the LN reference), which `main` does not meet either.
- `nebula_boxes.py`'s `d_norm` column compares a master's normalized read with the control's RAW read. The table above uses norm − norm. It is a harness fix for the next harness task.
