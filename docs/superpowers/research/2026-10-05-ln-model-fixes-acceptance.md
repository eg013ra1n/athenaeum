# LN model fixes — acceptance (checkpoint A 2026-10-05, checkpoint B 2026-10-06)

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

## 7. Owner decision (2026-10-05)

Checkpoint A accepted after an independent review (fable, `.superpowers/sdd/2026-10-05-ln-model-fixes-plan/checkpointA-review-fable.md`).

The review measured the following:
- The set-204 mono whole-frame MAD drop is the top and right 64-px edge. There `main` carried a dark rim inherited from the LN reference's partial-coverage edge:

  | Edge (top / right) | Offset from sky |
  | ---- | ---- |
  | LN reference | −4.7 / −9.8 % |
  | `main` | −3.5 / −5.6 % |
  | Branch | −1.7 / −4.4 % |
  | Raw registered frames | −0.6 / −2.5 % |

  The branch is the closest to the raw sky.
- OSC G/B keep the reference's 128–1000 px dust contrast (slope 0.996 / 0.973 against `main`'s 0.960 / 0.934).
- 82 % of the B nodes that moved by more than 1 % lie in NaN strips, where `main` held garbage.
- The FWHM jump is a β flip in the estimator; stacked star profiles agree to 1e-4 of peak.
- The median offset is uniform and identical on `main` (asymmetric integration rejection is the leading candidate).

Rulings:
- Continue to the star selection and F1.
- The master-median row reads **branch vs base ±0.1 %** from checkpoint B on.
- The uniform master-to-LN-reference median offset becomes an integration-side open item.

## 8. Checkpoint B (2026-10-06)

Branch head `d5f26b7e` (Tasks 10–16a: the star selection, F1, the reference-frames saturation map L-23). Two arms on
every dataset: **PSF signal** with the selection and the map (the default config), and **F1** = common aperture,
growth curve (`fluxEstimator: commonAperture`, `aperture: growthCurve`; the pre-check's step 2–3 pick, §8.5). The
baseline is checkpoint A's tree (`lnA`), which isolates items 3–4. The pre-B full gate was green (core all-targets
3737 passed / 0 failed, web 51, tauri, headless, `src/types`, tsc, vitest).

### 8.0 Verdict

**F1 measures the scale correctly on real data; the PSF signal does not.** Per member, against the photometric
truth, on the runs' own LN references:

| Dataset | F1 median gap / median \|gap\| / slope | PSF signal with selection |
| ---- | ---- | ---- |
| set 204 mono (91) | −0.10 % / 0.27 % / −0.11 %/px² | −5.98 % / 6.09 % / −3.29 %/px² |
| set 204 OSC (13 × 3) | −0.19 % / 0.44 % / −0.49 %/px² | −0.49 % / 1.13 % / −4.41 %/px² |
| run B mono (82) | +0.08 % / 0.33 % / −0.07 %/px² | −7.47 % / 7.47 % / −3.41 %/px² |
| run B OSC (101 × 3) | −0.13 % / 0.39 % / **+0.58 %/px²** | +0.03 % / 0.96 % / −4.85 %/px² |
| Horse C11 G (28) | −0.07 % / 0.32 % / −0.04 %/px² | −2.68 % / 2.68 % / −0.58 %/px² |

The background-model rows hold on both arms (Horse C11 halo, B nodes, IC 434, on G and now on L). Master SNR is
higher under F1 on every plane of every dataset.

**Rows that miss as written** (nothing re-sized or tuned; they go to the owner):

1. **Master MAD, F1 vs PSF, scale-normalized, ±1 %**: set 204 mono −2.37 %, run B mono −4.56 %; the raw read is
   +1.22 % / +2.07 %. OSC passes. The whole-frame MAD mixes a small-scale part that scales with `k` and a large-scale
   part held by the LN reference's background at the scale (§8.3); neither read can hold ±1 % for a +4…+7 % mono
   signal change on this nebula field.
2. **Per-member slope, run B OSC**: +0.58 %/px² against 0.5, pooled over the planes (known from the pre-check; the
   run applies the probe's scales to 0.02 %). Within-plane it is +0.21 %/px² [90 % interval −0.10, +0.49].
3. **FWHM vs the variance-mixture prediction, ±1 %**: run B mono, F1 vs `lnA`, −1.05 % (the master is *sharper* than
   predicted; the independent second-moment read moves +0.03 %). On the OSC planes where the measurement's β flips
   (set 204 R and B), the production `fwhmPx` jumps by −6.3…+6.7 % on either arm; the second-moment read is flat
   (±0.02 %). This is checkpoint A's estimator artifact.

### 8.1 Method

| Dataset | PSF arm | F1 arm | Baseline | Reads |
| ---- | ---- | ---- | ---- | ---- |
| Set 204 | `checkpoint.sh lnB-psf lnA --numeric` | same with `ATH_ACC_SET_CONFIG` (set 204's stored config, two fields changed) | `lnA`; same-evening `main` `lnB-main` | `tier1-compare.py --numeric` (four pairs), `master_phot.py --noise`, run-log seeds lines, `.athln` scales vs truth |
| Run B | `project-run.sh` on a `.backup` of the 6b-3 catalog | same with `ATH_ACC_PROJECT_CONFIG` | `lnA-runB` | `--pairs`, `master_phot.py --noise` (crop x0 73), catalog weights/rejection |
| Horse C11 (L + G) | `horse-c11-prep.sh` + `hc11-run.sh` | same with the F1 config | `hc11-ln` (`lnA`); control `hc11-noln` | `master_phot.py --noise`, `halo_profile.py --search 60`, `nebula_boxes.py`, `star_blocks.py`, B-node read, ERP scale log |

- Every run succeeded. The `checkpoint.sh` exit code 1 is the Tier C comparator's own §8 verdict, not a run failure.
- The LN references are bit-identical across `lnA`, `lnB-psf`, `lnB-f1` and `lnB-main` (set 204), pixel-identical
  to the pre-check's on run B, and md5-equal on Horse C11.
- `lnB-main` reproduces `lnA-main` bit for bit (median, MAD, scale and rejection all 1.000000).
- Controller scripts live outside the repo, under `~/.athenaeum-acc/lnB/`.

**The run applies the probe's scales.** On the same reference and registered frames, the probe's per-member `s` equals
the run's `.athln` relative scale:

- set 204 mono: to 0.03–0.05 % on three members;
- run B: to ≤ 0.02 % median on every plane;
- Horse C11 G: +0.07 % median.

The set-204 pre-check had measured against the September `tierA-baseline` LN reference (the same 20 members, a
different build). There both the probe's scales and the truth sit 0.4 % higher (truth new/old 0.9961). The set-204
truth was therefore re-measured against the run's own reference (`truth.py`; OSC per plane), and the §8.0 table uses
it. The pre-check's gaps on the old reference (−0.14 % / 0.26 % / −0.07) and the run's on its own agree.

### 8.2 Rows — set 204

| Row | Bar | PSF arm (vs `lnA`) | F1 arm (vs `lnA`; vs PSF where the row says so) |
| ---- | ---- | ---- | ---- |
| Master median, branch vs base | ±0.1 % | −0.005 / −0.003 / −0.005 / −0.009 % **PASS** | −0.026 / −0.001 / −0.002 / −0.004 % **PASS** |
| Master median vs same-evening `main` | reported | +0.077 / +0.074 / +0.061 / +0.057 % | +0.056 / +0.076 / +0.064 / +0.063 % |
| Master MAD | ±1 % vs PSF, normalized (raw) | — | mono **−2.37 %** (raw +1.22) **MISS**; OSC −0.31 / +0.12 / +0.14 (raw +0.04 / −0.24 / −0.49) PASS |
| Master SNR `k/σ` | ≥ baseline − 1 % | +0.07 / +0.09 / −0.01 / −0.22 % **PASS** | +0.39 / +0.57 / +0.20 / +0.09 % **PASS**; vs PSF +0.32 / +0.48 / +0.20 / +0.31 % |
| FWHM vs variance-mixture prediction | ±1 % | mono pred +0.15 %, meas +0.18 % PASS; OSC G, B PASS; OSC R β 4→2.5: −6.32 % (estimator) | mono pred +0.98 %, meas +0.78 % (−0.20) PASS; OSC R, G PASS; OSC B β 6→4: −2.60 % (estimator); second-moment read: mono +0.32 %, OSC −0.02 / +0.01 / −0.01 % |
| Rejected fraction | ±0.3 pp | −0.010 / +0.004 pp **PASS** | −0.047 / −0.021 pp **PASS** |
| Weights | ρ ≥ 0.99, top-20 ≥ 18 | ρ 1.0, 20/20 **PASS** | ρ 1.0, 20/20 **PASS** |
| Per-member scale vs truth (F1) | median \|gap\| ≤ 0.5 %, \|slope\| ≤ 0.5 %/px², offset ≤ 0.5 % | (reported, §8.0) | mono 0.27 / −0.11, OSC 0.44 / −0.49, offset +0.10 **PASS** |
| `--stack-sim` mono, no rejection (F1) | ±0.5 % of the truth-corrected arm | psfSel −4.16 % | −0.17 % **PASS**; arm / LN reference 1.0060 (truth 1.0078) |
| Seeds `k` per channel | reported (≈ 1 under F1) | mono 0.9974; OSC 0.9998 / 1.0043 / 1.0022 | mono 1.0001; OSC 1.0000 / 1.0000 / 1.0002 (`lnA`: 0.9916; 0.9965 / 0.9969 / 0.9925) |
| Drizzle level / coverage | as before | unchanged data facts (mono coverage 0.9935, OSC R level 0.9976) | same |
| Pedestal warning (§11.5) | none on set 204 | none in the run's warnings | none |
| Wall | reported | 901 s (Normalize 2.46 min) | 901 s (Normalize 2.46 min); `lnB-main` 851 s (1.89 min), `lnA` 870 s (2.21 min) |

Planes are listed mono, then OSC R / G / B. `k` is the master's star flux over the LN reference's (`master_phot.py`).
On mono that is 17–19 stars, SE 0.5–0.6 %. The direct F1/PSF master-to-master star ratio is 1.0429 ± 0.0012, against
1.0367 through the reference. The FWHM prediction is
`FWHM² = Σ w·(s/s_true)·F² / Σ w·(s/s_true)` with the run's weights and `.athln` scales. It holds within 0.03 points
whether member `F` is the truth's half-max or the measurement's fit, and whether `s_true` is the truth or the F1
scale. The harness half-max FWHM of the masters (3.742 / 3.826 / 3.909) counts pixels above half-max (`N` 11 → 12) and
says nothing at the 1 % level.

### 8.3 Why the MAD row misses

The master is split into the median field of 128-px blocks (large scale) and the residual (small scale)
(`mad_decomp.py`). Values are F1 vs PSF.

| Part | set 204 mono, raw / normalized | run B mono, raw / normalized |
| ---- | ---- | ---- |
| Whole frame | +1.22 / −2.37 % | +2.07 / −4.56 % |
| Small scale | +3.89 / +0.21 % | +5.73 / −1.14 % |
| Block field | +0.15 / −3.39 % | +0.12 / −6.38 % |

- **Small scale** (noise and structure under 128 px) follows the signal scale. Normalized, it is flat on set 204. On
  run B it drops 1.1 %: the noise rises less than the signal, which is the master SNR's +1.9 %.
- **Block field** does not move raw. Above the LN scale, the background comes from the LN reference through `B`,
  whatever the arm's `A`. Dividing it by a larger `k` shrinks it.
- **The whole-frame MAD** is a mix of the two. A +4 % (set 204) or +7 % (run B) mono scale change therefore cannot
  hold ±1 % on either read.

### 8.4 Rows — run B and Horse C11

**Run B** (F1 vs PSF unless noted):

| Row | Result |
| ---- | ---- |
| Master median | mono −0.04 %, OSC +0.001 / +0.003 / +0.004 %; both arms within 0.05 % of `lnA` — **PASS** |
| Master MAD | mono normalized **−4.56 %** (raw +2.07) **MISS**; OSC −0.03 / +0.24 / +0.29 % (raw −0.04 / −0.34 / −0.72) PASS |
| Master SNR | F1 vs `lnA` +2.21 / +0.42 / +0.09 / +0.08 %, PSF vs `lnA` +0.33 / +0.10 / +0.03 / −0.04 % — **PASS** |
| FWHM vs prediction | mono: pred +0.89 % (vs `lnA` +1.03), `fwhmPx` −0.02 % (−0.03) → −0.90 (**−1.05**); second-moment +0.03 %. OSC: pred +0.28 / +0.09 / +0.09, meas +0.01 / +0.06 / +0.04 PASS |
| Rejected fraction | identical to 6 decimals on both arms and `lnA` (rejection normalization is global here) — **PASS** |
| Weights | bit-identical, 184/184 — **PASS** |
| Wall | 811 s both arms (`lnA-runB` 781 s, a different evening) |

**Horse C11** (`k`: control 0.88329 G / 0.79741 L; PSF 0.96110 / 0.98976; F1 0.98457 / 0.99294):

| Row | Bar | PSF arm | F1 arm |
| ---- | ---- | ---- | ---- |
| G halo 60–70 / 80–90 / 100–120 px, normalized (control 28.33 / 13.55 / 7.41 %) | ±1 pt | +0.06 / −0.04 / −0.03 **PASS** | +0.03 / −0.03 / −0.03 **PASS** |
| L halo, normalized (control 29.60 / 14.93 / 8.50 %; first read) | ±1 pt | +0.21 / +0.03 / +0.05 **PASS** | +0.22 / +0.03 / +0.04 **PASS** (`main`: 17.46 / 5.36 / 1.81 %) |
| B nodes within 600 px, > 2 % of background | none | G 0 of 840 (worst 0.017 %), L 0 of 2 294 (0.047 %) **PASS** | G 0 of 840 (0.017 %), L 0 of 2 294 (0.038 %) **PASS** |
| IC 434 boxes, `d_norm` | ±1 pt | +0.034 / −0.036 / +0.070 **PASS** | −0.048 / −0.113 / +0.015 **PASS** |
| Star blocks, normalized rms (> 50 sky / > 200 sky / no star) | reported | G 0.40 / 0.40 / 0.38 %, L 0.42 / 0.47 / 0.43 % | G 0.42 / 0.47 / 0.37 %, L 0.42 / 0.48 / 0.44 % (`lnA` G 0.40 / 0.40 / 0.38; `main` 1.66 / 2.55 / 0.45) |
| Master SNR vs `lnA` | ≥ −1 % | G −0.01 %, L +0.02 % **PASS** | G +0.28 %, L +0.08 % **PASS** |
| ERP slope, per-frame scale / ERP (L-20: reported) | reported | −0.48 %/px² (r −0.85) | +0.07 %/px² (r +0.06); truth / ERP +0.11 |
| Seeds `k` | reported | L 1.0009, G 1.0019 | L 0.9999, G 1.0034 |
| Wall | reported | 631 s (Normalize 69.6 s) | 661 s (71.3 s); `hc11-ln` 631 s (59.1 s) |

The raw `star_blocks.py` read shows F1's star blocks at 2.30 % rms on G against 1.48 % for PSF. Divided by each arm's
`k` (`blocks_norm.py`), it is 0.42 % against 0.40 %. The difference is star signal level, not structure.

### 8.5 L-16 (spec §4.4), applied to these numbers

| Item | Result |
| ---- | ---- |
| 1. Truth per member: F1 passes step 1 on the four audited groups and Horse C11 G; PSF signal fails one on a mono group | **fails as written**: F1 passes four of five, run B OSC slope +0.58; PSF fails every mono group |
| 2. F1's no-rejection master star flux within ±0.5 % of the truth-corrected arm | **holds**: −0.17 % (PSF −4.16 %) |
| 3. Full runs: SNR ≥ PSF −1 %, rejection ±0.3 pp, MAD ±1 % normalized, ρ ≥ 0.99 | **fails on MAD** (mono, both datasets); SNR, rejection and ρ hold |
| 4. Margin: ≥ 1 point of median \|gap\| on a mono group and ≥ 1 % closer at master level | **holds**: 0.27 vs 6.09 % (set 204 mono); 0.17 vs 4.16 % |

By the pre-declared rule, the PSF signal stays the default unless the owner rules otherwise. The decision is Task 17's.
