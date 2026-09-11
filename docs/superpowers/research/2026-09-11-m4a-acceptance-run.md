# Stacking M4a — acceptance run (LDN 1272, 2026-09-11)

Plan: `docs/superpowers/plans/2026-09-10-stacking-m4a-plan-quality.md` (Task 7, rulings R-M4a-1…19). Prior notes: `2026-09-10-m3-acceptance-run.md` (run 12), `2026-09-10-m2-acceptance-run.md` (runs 6/7). The external reference is the same run of the external tool the M2/M3 notes compared against (its per-frame log and its calibrated/debayered frames and masters).

## 1. Setup

- **Build:** `athenaeum-web` release from branch `worktree-stacking-m4a` at `b39566ad` (Tasks 1–5 with their fix rounds; the Task 6 docs commit `9bf27170` landed during the run and changes no code); static bundle from the same commit; port 8931 against the dev catalog (set 109: 208 mono ATR2600M + 160 OSC ASI2600MC Duo lights, 180 s).
- **Config:** the M3 run-12 config with `reference = { mode: auto, twoPass: true }`, `registration.distortion = off`, `measurement = { psfSignalWeight, auto, maxStars 24576, detectionSigma 20, seedPrefilter none }` (the shipped defaults, spelled out), LN both sides (`local.enabled`, `rejection = local`, scale 1024, 20 reference frames), `integration.rejection = auto` (→ linear fit 5.0/3.5 at n ≥ 20), drizzle 2× (square, drop 0.9, rejection/weights/LN/weight map), `cleanup keepAll`.
- **Run 13:** a full run from a clean working folder (the M3 cleanup had removed every intermediate), 06:30:32Z → 07:26:33Z, 56.0 min, no errors, one warning (the two-pass switch, §4). Plan: no blockers, no warnings, estimate 77.7 GB against 594.5 GB free.

## 2. Stage timings

| Stage | Run 13 | Comparison |
| ---- | ---- | ---- |
| Calibrate | 5.06 min | run 8 (M3 full run): 5.31 |
| Measure | 10.04 min | run 8: 12.47 — the noise-relative seeds fit fewer stars (target ≤ 1.5×: 0.81×) |
| Register | 1.91 min | run 12 (one pass): 1.36 — the dry pass over the mono group costs ≈ 0.55 min |
| Normalize | 13.16 min | run 8: 13.03, run 12: 14.22 (M2's 28 min was the memory-bound first measurement; target ≤ 28) |
| Integrate | 10.85 min | run 11 (clean): 5.01, run 12 (loaded): 7.30 — the robust line's cost (ruling R-M4a-17); this figure is an upper bound: the controller's own comparison scripts ran on the same machine during the OSC integration (§9, finding 4) |
| Drizzle | 14.99 min | run 11: 17.75 (mono 4.3 min: read 7.6 s + deposit 236 s; OSC 10.1 min: read 18.8 s + deposit 585 s) |
| Output | 0.00 min | — |

## 3. Weights against the external log

Run 13's own per-frame metrics (`stacking_run_frames.metrics_json`, exported with the acceptance scratchpad's `db-to-jsonl.py`) graded by `docs/superpowers/research/scripts/weight_audit_compare.py` against the external log's per-channel terms — the same instrument Task 2 calibrated on the external tool's own calibrated frames, now on OUR calibrated frames of the same lights:

| Group / channel | Fits ratio per night (09-14 / 10-18 / 10-19) | PSFSW Spearman ρ | Fit-count ρ | Top-20 overlap |
| ---- | ---- | ---- | ---- | ---- |
| mono | 1.016 / 1.046 / 0.824 (all in [0.7, 1.4]) | **0.919** | 0.937 | **18/20** |
| OSC R | 3.82 / 1.58 / 1.32 | **0.930** | −0.43 | — |
| OSC G | 4.66 / 1.64 / 1.34 | 0.817 | −0.61 | — |
| OSC B | 7.01 / 2.40 / 1.77 | 0.683 | −0.73 | — |
| OSC frame-mean | — | — | — | **15/20** |

10 PASS / 12 MISS of the ruling R-M4a-2 targets — the same result Task 2 reached on the external tool's frames (10/12; baseline 7/15), which confirms the calibration carries over to our own calibration of the same lights. Mono meets every target; the OSC residual is the bright/sharp night (`2025-09-14`, FWHM 1.55 px) still yielding 3.8–7× the external tool's accepted fits — a peak-threshold detector cannot reproduce a structure-based one's sharpness behaviour (M4a fix rounds 1–2 tried the reference's 3×3 median pre-filter, shipped as an option; the structure-map detector is M4c Task 0, ruling R-M4c-11).

## 4. Two-pass reference

The pick fired on real data: stage 4 chose `2025-10-18_02-30-48_0080` (normalized weight 1.00) and the dry pass found its corner displacement from the mono group's median transform to be 36.2 px; among the top 10 by weight, `2025-10-18_02-02-02_0073` (weight 0.94) sits at 23.2 px, so the reference switched (gain 13.0 px ≥ the 4 px floor). `_0073` is the frame the owner had pinned manually as the reference throughout M2 and M3 — the automatic rule now reproduces that choice. The run row (`stacking_runs.reference_frame_id = 29390`), the summary (`reference.switchedFrom = 29351`) and the results card (`· switched from #29351 (two-pass)`) all carry it; registration RMS median 0.131 px (mono, max 0.235), 0.205 px (OSC, max 0.356); 368/368 frames registered and included.

## 5. Rejection fraction (the `LINEAR_FIT_SIGMA_SCALE` calibration)

| Group | Rejected low / high / total at Auto 5.0/3.5 | M2 (least-squares line, run 6) | External tool (same thresholds) |
| ---- | ---- | ---- | ---- |
| mono | 0.926 / 2.059 / **2.985 %** | 0.83 % | 2.5–2.8 % |
| OSC | 0.758 / 1.975 / **2.733 %** | 0.74 % | 2.5–2.8 % |

Both inside the 2.3–3.3 % band with `LINEAR_FIT_SIGMA_SCALE = 1.0` — the minimum-absolute-deviation line alone closes the M2 gap; the one calibration round ruling R-M4a-4 allowed was not needed and the constant stays 1.0 (documented as calibrated by this run).

## 6. Masters and drizzle against run 12 and the external tool

Every FWHM below is through THIS build's estimator (the M4a seeds changed the frame-FWHM statistic itself, so run 12's and the external tool's files were re-measured with the same `measure_probe`; the M3 note's 0.925/0.926 pair was the old estimator on the same files — both moved together).

| Quantity | Run 13 | Run 12 (re-measured) | External (re-measured) |
| ---- | ---- | ---- | ---- |
| Mono master FWHM / noise | 2.734 px / 1.83e-5 | 2.690 / 1.86e-5 | 2.680 / 2.01e-5 |
| Mono drizzle FWHM | 4.459 px | 4.363 | 4.343 |
| **Mono drizzled/undrizzled ratio** | **0.8154** | 0.8109 | **0.8102** (+0.6 %) |
| OSC master FWHM R/G/B | 2.811 / 2.822 / 2.717 | 2.758 / 2.734 / 2.607 | 2.687 / 2.764 / 2.757 |
| OSC master noise R/G/B | 1.67 / 1.73 / 1.64 e-5 | 1.60 / 1.66 / 1.55 | 1.65 / 1.68 / 1.53 |
| OSC drizzle FWHM R/G/B | 4.748 / 4.718 / 4.453 | 4.634 / 4.503 / 4.221 | 5.761 (outlier, ecc 0.41) / 4.111 / 4.064 |
| **OSC ratio G / B** | **0.836 / 0.819** | 0.824 / 0.810 | **0.744 / 0.737** (+12 / +11 %) |
| Mono master level / corners (TL TR BL BR) | 0.00637 / 0.899 0.867 0.939 0.908 | 0.00728 / 0.907 0.877 0.945 0.916 | 0.00736 / 0.908 0.878 0.952 0.921 |
| OSC master level R/G/B | 0.00133 / 0.00289 / 0.00233 | 0.00134 / 0.00290 / 0.00233 | 0.00125 / 0.00276 / 0.00220 |
| OSC R corners | 0.675 0.662 0.681 0.672 | 0.675 0.663 0.681 0.671 | 0.687 0.677 0.703 0.687 |

- **Mono:** the drizzled/undrizzled ratio equals the external tool's to 0.6 % (the M3 result holds under the new estimator); the master's noise is 9 % below the external's; its LEVEL follows the new normalization anchor — the sky-penalized order (ruling R-M3-17) now picks `2025-10-18_01-08-34_0060` (background 0.00624) where run 12 picked `_0077` (0.00714), so the master sits 13 % below the external's level with the same background shape (columns/rows within 0.005, corners within 0.013). A level is a convention (LN makes the master follow its anchor); the shape is the quality metric and it is unchanged.
- **OSC:** the anchor is the same `_0150` as run 12's (the frame the external tool's LN reference lists first), the level and corners are run 12's to 0.001 and the external's to 1–2 %. The G/B drizzle ratio is +12/+11 % over the external's — the M3 residual, unchanged in kind (run 12 re-measured: +11/+10 %); the weight change moved it by ≈ +1 % (the bright/sharp night now weighs less). Still not explained by rejection, level or distortion (M3 findings) — carried to M4c with the weight audit's structure-map detector.
- **Drizzle checks** (`drzcheck.py`): level ratio 0.99916 (mono), 0.9973 / 0.9994 / 0.9994 (OSC); coverage 1.0 on every plane; the 512-row fold shows no seam (folded ptp 5.4e-5 vs a random-period fold 5.8e-5 on mono; 3.2–5.0e-5 vs 4.7–5.9e-5 on OSC); weight maps max 1.0, interior median 0.93, minimum 0.038 (mono) / 0.107–0.123 (OSC) at the edges, no interior zeros.

## 7. Local normalization

368/368 sidecars, no exclusions, no frame under the 20-matched-star floor (the LN scale fit now runs on the changed `fit_one`, ruling R-M4a-15 recomputed every sidecar once); LN reference builds 14.7 s (mono) / 42.4 s (OSC); stage 13.2 min for both groups against M2's 28 min (memory-bound then) and run 12's 14.2 (Task 5's weight table and `Cow` planes; the per-frame PSF fit cost also dropped with the noise-relative seeds).

## 8. Acceptance table

| Target (plan Task 7) | Result | Verdict |
| ---- | ---- | ---- |
| Per-channel PSFSW Spearman vs the external tool ≥ 0.90, both groups | mono 0.919; OSC 0.930 / 0.817 / 0.683 | mono **pass**; OSC R pass, G/B **miss** (structural — M4c Task 0) |
| Fit-count ratio per night inside [0.7, 1.4] | mono 1.02 / 1.05 / 0.82; OSC 1.3–7.0 | mono **pass**; OSC **miss** (same cause) |
| Top-20 overlap ≥ 14/20 per group | 18/20, 15/20 | **pass** |
| Per-night OSC weight order = the external tool's (dark nights over the bright one) | OSC top 20: 8 × 09-14 + 8 × 10-18 + 4 × 10-19 (external 12 + 8); the bright night no longer monopolises the top | **pass** (the M3 inversion is gone) |
| Rejected fraction 2.3–3.3 % at 5.0/3.5, both groups | 2.985 %, 2.733 % | **pass**, no calibration round needed |
| Two-pass pick logged; master corner coverage ≥ run 12's | switched `_0080` → `_0073` (36.2 → 23.2 px); weight-map minimum 0.0377 vs 0.0375 | **pass** |
| Masters vs the external tool: OSC level ± 5 %, corners ± 0.02 | OSC level 0.00133/0.00289/0.00233 vs 0.00125/0.00276/0.00220 (+6 / +5 / +6 %), corners within 0.02 | **pass** (level as in run 12; the +5 % was accepted in M3) |
| Master FWHM / noise within ± 5 % of run 12's | mono 2.734 vs 2.690 (+1.6 %), noise −1.6 %; OSC +2 / +3 / +4 %, noise +4–6 % | **pass** |
| OSC drizzle G/B ratio within ± 5 % of the external's | 0.836 / 0.819 vs 0.744 / 0.737 | **miss** (+12 / +11 %, unchanged in kind since M3 — finding 3) |
| Mono drizzle ratio (M3 target, re-checked) | 0.8154 vs 0.8102 | **pass** (+0.6 %) |
| Measure stage ≤ 1.5× M3's | 10.0 vs 12.5 min | **pass** (0.81×) |
| LN stage ≤ M2's 28 min | 13.2 min | **pass** |
| Click-through (Measure σ field, Reference two-pass checkbox, the results reference line) | verified through the page's accessible text and element finds over the LAN browser: the Measure row reads `… · σ 20`, the panel field `Detection threshold (σ)` = 20 with its default badge; the Reference row `Auto (highest-weight frame · two-pass)` and the panel checkbox `Two-pass pick (re-choose the reference closest to the set's median transform)`; the results card `#13 · … · done`, `reference …_0073.fits · switched from #29351 (two-pass)`, `2.985% rejected · noise 1.830e-5`, `LN: 208/208 frames`, `Drizzle 2×: FWHM 4.46 px · coverage 100.0%` and the OSC lines | **pass** (screenshots refused by the narrow viewport again — text only) |

## 9. Findings

1. **The two-pass pick reproduces the owner's manual reference.** On this set the best-weighted frame is 36 px off the group's median framing at the corners; the automatic re-pick lands on `_0073`, the frame chosen by hand for M2/M3, at a cost of 0.55 min.
2. **The robust linear-fit line is the whole M2 dispersion fix.** 0.83/0.74 % → 2.985/2.733 % at the same nominal thresholds; the reserved calibration constant stays 1.0.
3. **The OSC drizzle residual is not a weights effect.** With the new weights the G/B ratio moved by +1 % (the sharp night contributes less); the +11–12 % over the external tool persists and joins the M3 findings (not rejection, level, distortion, or weights). Two more differences to test in M4c: the external drizzle's R plane is an outlier (ecc 0.41 — its own stack), and the external tool's LN was built on its 20-frame reference with a different anchor rule; a common-subset drizzle with both weight sets remains the plan.
4. **Integrate stage 10.85 min** is 2.2× run 11's clean 5.01 min, at the edge of ruling R-M4a-17's revisit threshold — but the controller's own numpy comparisons (four 400 MB–1.2 GB file diffs) ran on the same machine during the OSC integration, so the clean figure is lower; the mono group alone (`group integration finished`, 209 s) was 1.7× run 11's mono. The hybrid option (the robust line on iteration 1, least squares afterwards) stays recorded in open-items, not needed now.
5. **Mono master level follows the sky-penalized anchor** (13 % below the external's, same shape) — by design since ruling R-M3-17; recorded so the number is not mistaken for a calibration change.
6. **The FWHM estimator moved with the seeds**: the frame FWHM statistic (residual-weighted mean over accepted fits) reads 2.734 for the run-13 mono master where the M3 estimator read 2.688 for run 12's; comparisons across the estimator change must re-measure both sides (done here for every external file).

## 10. Verdict

**Green with one attributed miss.** Every M4a target passes on the mono group; on OSC the rejection fraction, the top-20 overlap, the night order and the master's level/shape pass, while the G/B channel weight correlation (0.82/0.68 against ≥ 0.90) and the drizzle G/B ratio (+11–12 %) miss for the reason M4a diagnosed and M4c Task 0 is planned to remove (a structure-map seed detector). The run is 56 min for 368 frames end to end, faster than M3's (measure −20 %, LN −7 %, drizzle −16 %) despite the two-pass dry pass and the robust rejection line.

## 11. Release-note drafts (English, for the next release's notes)

- **Smarter frame weighting.** Star detection for the quality measurement is now noise-relative (the new *Detection threshold (σ)* setting in the Measure panel, default 20), so a bright-sky night no longer floods the ranking with faint stars; on a two-camera, three-night set the mono frames now rank as the external tool ranks them.
- **Two-pass reference pick.** Registration first checks the best-weighted frame against the set's median framing and, when that frame is rotated or shifted against the rest, picks the closest top-weighted frame instead — the automatic choice now matches what an experienced user pins by hand. Off in the *Fast preview* preset.
- **Rejection that bites like the reference's.** Linear-fit clipping uses a robust line; at the default thresholds it now rejects ≈ 3 % of samples (satellites, cosmic rays, edges) where it used to reject under 1 %.
- **Faster local normalization** on large frames (the per-frame LN pass runs ≈ 7 % faster; measurement 20 % faster).
