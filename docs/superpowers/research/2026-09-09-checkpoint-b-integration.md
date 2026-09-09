# Checkpoint B — weighted integration against the external masters (LDN 1272)

**Date:** 2026-09-09 · **Plan:** `docs/superpowers/plans/2026-09-09-stacking-m1-plan4-integration.md` (Task 8) · **Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §5.1, §6, §13 · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §3 · **Previous checkpoint:** `docs/superpowers/research/2026-09-09-checkpoint-a-registration.md`

## 1. Purpose

Plan 4 delivers the integration stage: the weighted, normalized combiner with survivor masks and rejection maps, the per-group driver, the WCS card writer and the master-light header. Checkpoint B asks: **does a master light built by our pipeline from the owner's calibrated frames match the external stacker's master on the same frames — in noise, in what it rejects, in what it leaves behind — and do our per-frame weights rank the frames the way the external run did?** It also settles the owner's detector question (step 4b): which star detector should seed the measurement stage.

Three integration runs are reported: the mono group with the default kernel (run 1), the mono group again with Lanczos-3 and the full per-frame seed report (run 2), and the OSC group (run 3, repeated once with the fixed probe for the pixel-level compare).

## 2. Setup

- **Inputs:** the app's own calibrated frames (`LDN1272-ATH/LDN 1272/camera_atr2600m/lights/c_*.fits`, 208 mono; `camera_zwoasi2600mcduo/lights/c_*_d.fits`, 160 OSC, VNG-debayered, 3 planes). The external run calibrated the same raw frames through its own calibration, so the inputs differ by the calibration stage only (see §6 for what that costs on OSC).
- **Reference:** `c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits` (mono) for both groups' registration (spec §2: one reference per set). The OSC group is integrated as its own group with its best-weighted member as the normalization reference (ruling, §10).
- **External masters:** `LDN1272-Output/master/masterLight_BIN-1_6224x4168_EXPOSURE-180.00s_FILTER-NoFilter_mono_(1).xisf` and `…_6248x4176_…_RGB_(1).xisf` (the `_(1)` files belong to log `20260908121336`; log times are UTC, file times local + 3 h). Each holds three images — `integration`, `rejection_low`, `rejection_high`; our XISF reader decodes the first, so the external maps are compared through the log's totals only.
- **External settings (from the log):** PSF Signal Weight, BWMV scale, linear fit clipping 5.0/3.5, range-low 0.0 on, output normalization additive with scaling, **rejection normalization local (LN — an M2 feature)**, registration with **bicubic B-spline interpolation, clamping 0.30** (`SA.pixelInterpolation = StarAlignment.BicubicBSpline` — the execution ledger had recorded Lanczos-3; that was wrong, and §5 is the consequence).
- **Ours:** `RegistrationConfig::default()` (bicubic B-spline, clamping 0.3, no distortion — the same kernel, so runs 1 and 3 are like-for-like), `IntegrationConfig::default()` (Auto → linear fit 5.0/3.5 at n ≥ 20, range-low 0.0, minWeight 0.005), `NormalizationConfig::default()` (additive with scaling / scale-zero-offset / BWMV), PSF Signal Weight from Plan 2's fitter on fast-detector seeds.
- **Tool:** `examples/integrate_probe.rs` (release; ffa9030c for run 1, 6b037ad7 for runs 2–3, bf9cbafa for the OSC compare re-run); the batch script, per-run JSON, masters, maps and the analysis scripts live in the session scratchpad; the plan's Task 8 lists the commands.
- **Per-frame external measurements** (FWHM, eccentricity, star count, PSF Signal Weight, PSF SNR, SNR, median, MAD, M*) were extracted from the log's measurement blocks for all 368 frames and joined by stem.

## 3. External baselines (log)

| Group | Frames | Rejected total (low + high) | Master noise (MRS) | Location | Scale (BWMV) | PSF SNR | PSFSW | SNR |
| ----- | ------ | --------------------------- | ------------------ | -------- | ------------ | ------- | ----- | --- |
| mono | 208 | 2.831 % (0.841 + 1.989) | 1.9570e-05 | 7.183e-03 | 2.0625e-04 | 1.2458e4 | 534.07 | 610.95 |
| OSC R/G/B | 160 | 2.508 / 2.790 / 2.639 % | 1.6716e-05 / 1.6631e-05 / 1.4975e-05 | 1.119e-03 / 2.706e-03 / 2.161e-03 | 1.400e-04 / 6.060e-05 / 4.491e-05 | 2318.6 / 4136.4 / 3232.1 | 106.6 / 173.9 / 130.6 | 170.1 / 172.5 / 125.9 |

The external PSF SNR / PSFSW are on that tool's own scale and are **not** compared with ours anywhere below (the per-frame ratio ours/theirs is ≈ 19× while the master ratio is 0.5–1.0 — different definitions). Where a PSF number of the external master is quoted, it is our estimator run on their image.

## 4. Mono group — run 1 (bicubic B-spline, like-for-like kernel)

| Metric | Ours | External (log) | External image by our estimator | Ours / external |
| ------ | ---- | -------------- | ------------------------------- | --------------- |
| Frames registered / included | 208 / 208 | 208 / 208 | — | — |
| Rejection | linear fit 5.0/3.5 (Auto) | linear fit 5.0/3.5 (LN) | — | — |
| Master MRS noise | 1.758e-05 | 1.957e-05 | 2.011e-05 | **0.90 / 0.87** |
| Location | 6.901e-03 | 7.183e-03 | 7.183e-03 | 0.961 |
| Scale (BWMV) | 1.916e-04 | 2.063e-04 | 2.062e-04 | 0.929 |
| Relative noise (noise / scale) | 0.0918 | 0.0949 | 0.0975 | 0.97 / 0.94 |
| FWHM px (our estimator) | 2.71 | — | 2.67 | 1.01 |
| Eccentricity (our estimator) | 0.30 | — | 0.30 | — |
| PSF SNR (our estimator) | 6535 | (1.2458e4, its own scale) | 4349 | 1.50 |
| Rejected % (low + high) | **0.653 (0.092 + 0.561)** | 2.831 (0.841 + 1.989) | — | 0.23 |
| Stars (300 brightest of each) | 299 matched within 1.5 px, median centroid Δ 0.029 px, median flux ratio 0.932 | | | |
| Pixel comparison (1.62 M stratified samples > 2σ) | median relative difference −4.0 % (one global level factor) | | | |

- **Noise:** ours is 10 % below the log's value and 13 % below their image measured by our own estimator; in relative terms (noise over the BWMV scale of the same image) 3–6 % lower. The ±5 % target is met in relative terms and missed favourably in absolute terms.
- **Level:** −3.9 % — expected: theirs is referenced to its LN reference image, ours to the reference frame; the pixel comparison shows one global factor, no structure.
- **Rejected 0.653 % vs 2.831 %** — a 4× under-rejection, the same on the OSC group (§6) and unchanged by the kernel (§5). Attribution in §10.
- **Artifacts:** both masters rendered at ¼ scale are indistinguishable; the high-rejection map carries 56.9 % of pixels with ≥ 1 rejection (mean 1.14 per pixel); its strongest 64-px cells form a diagonal band on the right edge (5632,1280)→(6080,2304) with maxima of 18–32 rejections out of 208 — a 400×400 crop of the master there shows round stars and a clean background, the same crop of the map shows a thin satellite/plane trail fully captured (absent from the master), ring-shaped rejections around bright star cores, and a broad diagonal density step that is the coverage boundary of the 180°-rotated frames (different stack membership), not an artefact. A per-row noise profile of the master (median |Δ| between adjacent rows and columns, every row) is flat over 0–4124 with identical horizontal and vertical values, rising only over the last 43 rows where the coverage thins — no band seams.
- **Weights:** 208 included, none below the weight floor; normalized weights 0.138–1.000 (PSFSW 0.64–4.66; the best five are 0077, 0080, 0076, 0074, 0060; the worst five 0050, 0046, 0039, 0053, 0016); Kish effective frame count 160.3 of 208; weighted exposure 16 194 s of 37 440.
- **Timing:** register 74 s, measure 332 s (1.6 s per frame, frames sequential), read 50 s for 21.6 GB, combine 18 s, write 0.2 s; total 8.1 min.

## 5. Mono group — run 2 (Lanczos-3, plus the full seed report)

Run 2 was meant to test the kernel hypothesis for the noise gap while the external run was believed to use Lanczos-3. The log shows it used bicubic B-spline, so run 2 became a characterization of the Lanczos-3 path instead.

| Metric | Run 2 (Lanczos-3) | Run 1 (bicubic B-spline) | External image by our estimator |
| ------ | ----------------- | ------------------------ | ------------------------------- |
| Master MRS noise | **3.906e-05** | 1.758e-05 | 2.011e-05 |
| Location / scale | 6.894e-03 / 1.953e-04 | 6.901e-03 / 1.916e-04 | 7.183e-03 / 2.062e-04 |
| FWHM px / ecc (fitted) | 2.08 / 0.34 | 2.71 / 0.30 | 2.67 / 0.30 |
| PSF SNR / PSFSW (our estimator) | 942 / 145 | 6535 / — | 4349 / 275 |
| Rejected % (low + high) | 0.661 (0.101 + 0.560) | 0.653 (0.092 + 0.561) | 2.831 |
| Stars | 300 / 299 matched, centroid Δ 0.031 px, flux ratio 0.938 | 299, 0.029 px, 0.932 | |
| Pixel median relative difference | −4.1 % | −4.0 % | |
| Per-row noise profile (σ from adjacent-row / adjacent-column differences) | 4.44e-05 / 4.44e-05, flat | 2.11e-05 / 2.12e-05, flat | |

- **The kernel sets the master's MRS noise by construction.** The bicubic B-spline is a smoothing kernel (its 2-D weight energy Σw² is ≈ 0.17 at typical sub-pixel phases); Lanczos-3 is interpolating (Σw² ≈ 0.75–1.0). For a noise-preserving kernel the expected master noise is σ_frame·√Σw²/Σw over the weights ≈ 5.5e-4 × 0.079 ≈ 4.4e-05, which is what run 2 measures; run 1's 1.76e-05 is that value times the B-spline's smoothing. The noise is uniform and isotropic in both masters (row and column profiles identical, no seams), so this is the kernel, not a resampler or band-window defect (the band window is sized by `interp.radius()`, and the source accessor asserts it in debug builds).
- **The fitted FWHM of the Lanczos master (2.08 px) is not physical** — no linear resampling sharpens below the inputs' 2.7–2.8 px. A bright star's radial profile in the two masters has FWHM 3.92 px (Lanczos-3) vs 4.04 px (B-spline), 3 % apart, non-negative everywhere (no Lanczos rings; the 0.3 clamp holds). The frame-level FWHM/PSF estimator is therefore sensitive to the noise texture at the 30 % level between kernels — a measurement-side note for the PSF audit line (M4), harmless here because every comparison in this checkpoint holds the kernel fixed.
- **The kernel does not move the rejection** (0.661 % vs 0.653 %), which removes the kernel from the under-rejection hypotheses.
- **Ruling:** the spec's default (bicubic B-spline, clamping 0.3 — also the external run's choice) stands for M1; Lanczos-3 stays selectable with its noise cost documented here.
- Timing: register 70 s, measure 293 s (1.41 s per frame), read 129 s, combine 19 s; the seed report (both detectors on every frame, each frame re-read) added ≈ 11 min; total 19.8 min.

## 6. OSC group (bicubic B-spline, distortion off)

| Plane | Ours noise | Ext noise (log) | Ratio | Ours location | Ext location | Ours scale | Ext scale | Ours FWHM / ecc |
| ----- | ---------- | --------------- | ----- | ------------- | ------------ | ---------- | --------- | --------------- |
| R | 1.651e-05 | 1.672e-05 | 0.99 | 2.603e-03 | 1.119e-03 | 1.778e-04 | 1.400e-04 | 2.97 / 0.31 |
| G | 1.975e-05 | 1.663e-05 | 1.19 | 8.272e-03 | 2.706e-03 | 8.369e-05 | 6.060e-05 | 2.87 / 0.30 |
| B | 1.938e-05 | 1.498e-05 | 1.29 | 7.043e-03 | 2.161e-03 | 6.605e-05 | 4.491e-05 | 2.69 / 0.29 |

- 160/160 registered onto the mono reference; the reference is not a group member (`referenceInGroup: false`, plane count 1 vs 3), so the group's normalization reference is its best-weighted frame, `…_0019` (weight 0.980); 160 included, none below the weight floor; Auto resolved linear fit 5.0/3.5; normalized weights 0.447–0.980; Kish effective frame count 154.4 of 160; weighted exposure 20 410 s of 28 800.
- **Level:** our master's per-plane level equals its normalization reference frame's (frame 172.7 / 544.0 / 462.8 ADU16 by a sampled median, master 170.6 / 542.2 / 461.6) — integration is level-preserving. The 2.3–3.3× gap to the external master, and the different channel ratios (G/R, B/R: ours 3.18 / 2.71, theirs 2.42 / 1.93), come from the reference convention (theirs is referenced to its LN reference image) and from our per-CFA-channel flat normalization (`ATH_CFNR/G/B`, calibrated-export v2) against the external whole-flat normalization — a calibration-stage convention, not an integration finding. A per-channel gain scales the noise with it, so the log's per-channel noise is not directly comparable to ours; the like-for-like numbers are our estimator on their image, below.
- **Rejected 0.632 % (0.089 low + 0.544 high) vs 2.51 / 2.79 / 2.64 % per channel** — the same 4× under-rejection as mono, on a different camera, a different night set and a 3-plane debayered stack.
- Timing: register 62 s, measure 444 s (2.8 s per 3-plane frame), read 123 s for 49.8 GB, combine 41 s, write 0.6 s; total 11.3 min.

**Pixel-level compare against the external RGB master** (re-run with the fixed probe bf9cbafa; 672 s wall; every integration number identical to the first run to four digits, as expected of a header-only fix):

| Plane | Ours noise | Theirs (our estimator) | Noise ratio | Scale ratio ours / theirs | Relative noise ours / theirs | PSF SNR ours / theirs | PSFSW ours / theirs | FWHM ours / theirs | Level ratio (median) |
| ----- | ---------- | ---------------------- | ----------- | ------------------------- | ---------------------------- | --------------------- | ------------------- | ------------------ | -------------------- |
| R | 1.651e-05 | 1.653e-05 | **1.00** | 1.27 | 0.0929 / 0.1181 = **0.79** | 2345 / 1262 = **1.86** | 79.7 / 65.0 | 2.97 / 2.70 | 2.33 |
| G | 1.975e-05 | 1.677e-05 | 1.18 | 1.38 | 0.236 / 0.277 = **0.85** | 3425 / 2313 = **1.48** | 119.5 / 95.4 | 2.87 / 2.77 | 3.06 |
| B | 1.938e-05 | 1.525e-05 | 1.27 | 1.47 | 0.293 / 0.340 = **0.86** | 2351 / 1847 = **1.27** | 86.6 / 72.8 | 2.69 / 2.75 | 3.26 |

- The noise ratio rises R → G → B exactly as the scale ratio does (1.27 / 1.38 / 1.47) — the signature of a per-channel gain between the two inputs, not of the integration. In gain-invariant terms our OSC master is the less noisy one on all three planes: relative noise 14–21 % lower, PSF SNR 1.3–1.9× (their master by our estimator), as on mono. The absolute per-channel noise ratio against the ±5 % target cannot be read without the external calibrated frames.
- Stars: all 244 of our brightest selection found in theirs within 1.5 px (204 of their 300 in ours — the two brightest-300 selections overlap partially because of the per-channel level), median centroid Δ **0.018 px**, median flux ratio 1.375 (the level gain).
- Our high-rejection map: 51.1 % of pixels carry ≥ 1 rejection (mean 0.87 per pixel, 0.55 % per frame); the strongest 64-px cells sit on the right coverage edge (x ≈ 6144) and at (4736, 3520) / (3456, 3456) with maxima of 9–15 rejections out of 160. Per-row noise profile of the R plane flat over the whole height (no row above 1.5× the median) — no band seams. A 320×320 crop of the master at (4600, 3400), around its strongest interior high-rejection cell, shows round stars, faint nebulosity and a clean background; the same crop of the high map shows a ≈ 30-px diffuse patch of high rejections (a transient present in a few frames, absent from the master) and rings around the bright stars' cores.
- Master header (the point of the re-run): `INSTRUME = 'ZWO ASI2600MC Duo'`, `OBJECT = 'LDN 1272'`, `DATE-OBS 2025-09-13T21:55:28.654` / `DATE-END 2025-10-19T02:26:08.703` (first and last OSC frames), `NCOMBINE 160`, `EXPTIME 20410.07`, `ATH_STKF …_0019`, `ATH_STKG 'ZWO ASI2600MC Duo__osc__NoFilter__bin1__6224x4168'`; the geometry is the mono reference's (6224×4168, NAXIS3 = 3), as spec §2 wants for one reference per set.

## 7. Per-frame weights against the external run

Every frame of both groups was matched by stem to its external measurement block (208/208 mono, 160/160 OSC). Rank correlation is what matters for weighting; the absolute values differ by definition.

| Per-frame metric vs the external log | Mono Spearman | Mono Pearson | OSC Spearman | OSC Pearson |
| ------------------------------------ | ------------- | ------------ | ------------ | ----------- |
| PSF Signal Weight | +0.924 | +0.984 | +0.780 | +0.820 |
| PSF SNR | +0.959 | +0.946 | +0.981 | +0.989 |
| FWHM | +0.989 | +0.995 | +0.987 | +0.996 |
| Eccentricity | +0.981 | +0.987 | +0.935 | +0.932 |
| Star count | +0.961 | +0.992 | +0.974 | +0.986 |
| Our normalized weight vs their PSFSW | +0.924 | — | +0.770 | — |

- Mono: the weights rank the frames as the external run does (ρ = 0.92, Pearson 0.98 — the relation is close to linear as well). Medians: FWHM 2.84 vs 2.69 px, eccentricity 0.42 vs 0.51, stars 10 540 vs 8 689.
- OSC: ρ = 0.78 on PSFSW while FWHM, eccentricity, star count and PSF SNR all correlate at 0.93–0.99. Two reasons, neither an integration matter: our OSC per-frame numbers are the mean over the three planes while the external measurement is one number per debayered frame, and the OSC group's weights are compressed (0.45–0.98 against 0.14–1.00 for mono), so rank noise costs more. The group spans two nights a month apart (2025-09-14, 2025-10-19) with a clear quality split (the 2025-09-14 frames hold the top of the ranking in both tools).

## 8. Detector comparison (owner's rule, step 4b)

Both detectors were run on every one of the 208 mono frames (the plan asked for three; the probe's `--seeds-report 208` made the full set cheap enough): the fast adaptive detector (`detect_fast_data`, the plate solver's falling-threshold ladder, `maxStars 24576`) that Plan 2 uses, and the two-pass calibrated-kernel detector (`analyze_data` with `with_measure_cap(0)` so every detection is measured) that the Analysis page uses. Each seed set fed the same `measure_plane` fits.

| Per frame (208 mono frames) | Fast adaptive detector | Two-pass calibrated-kernel detector |
| --------------------------- | ---------------------- | ----------------------------------- |
| Seeds (median; min–max) | 10 800 (6 524–22 290) | 5 957 measured (4 069–9 031); 6 198 detected |
| Agreement within 0.5 px | 37 % of fast seeds have a two-pass seed (21–49 %) | 82 % of two-pass seeds have a fast seed (33–95 %) |
| Detection time (median) | 224 ms | 413 ms |
| PSFSW from these seeds (median) | 1.725 | 0.0177 |
| PSF SNR from these seeds (median) | 8.71 | 0.0130 |
| **Spearman ρ of that PSFSW vs the external per-frame PSFSW** | **+0.924** | **+0.723** |

- The two-pass detector finds the brighter half of the fast detector's population (82 % of its seeds coincide with a fast seed; the fast detector's extra seeds are faint stars the two-pass kernel does not reach). Its PSFSW/PSF SNR are ≈ 100× smaller because both are quadratic in the fitted-star count — a normalization difference, which is why the decision reads the rank correlation, not the values.
- **Decision:** the measurement seeds stay with the fast detector. The rule was to switch only if the two-pass rank correlation were higher by more than 0.02; it is lower by 0.20. Registration keeps the fast detector as Checkpoint A established. The two-pass detector's time (0.41 s per frame) would have fitted the budget; its ranking does not.

## 9. Verdict against the targets

| Target (spec §13, plan Task 8) | Mono | OSC | Verdict |
| ------------------------------ | ---- | --- | ------- |
| Frames registered | 208/208 | 160/160 | pass |
| Master MRS noise within 5 % of the external master (same kernel) | 0.90 (log) / 0.87 (their image, our estimator); relative noise 0.97 / 0.94 | our estimator on their image: R 1.00, G 1.18, B 1.27, tracking the scale ratio 1.27 / 1.38 / 1.47 (a per-channel input gain, §6); relative noise 0.79 / 0.85 / 0.86; PSF SNR 1.86 / 1.48 / 1.27× | mono: near-miss, favourable (ours is the less noisy master); OSC: pass in gain-invariant terms, the absolute per-channel ratio indeterminate (input gain) |
| Rejected fraction 1–4 % (external 2.831 % / 2.51–2.79 %) | 0.653 % | 0.632 % | **miss** — attributed, ruling in §10 |
| Artifacts: no residual trail at 400 % where the external shows none | trail fully captured in the high map, clean master crop, no band seams, star centroids Δ 0.03 px | no band seams (per-row profile flat), star centroids Δ 0.018 px, a transient patch captured in the high map and absent from the master | pass |
| Integrate stage ≤ 8 min per group (read + combine + write) | 68 s | 165 s | pass |
| Register ≤ 5 min (both groups) | 74 s + 62 s = 136 s | | pass |
| Measure ≤ 5 min (368 frames) | 332 s + 444 s = 776 s with frames measured one at a time | | **miss ×2.6 as run** — the probe measures frames sequentially with the pool inside a frame; Plan 5 owns the frame-level concurrency (§11) |
| Whole set wall time ≤ 60 min (without LN/drizzle) | 8.1 min + 11.3 min = 19.4 min | | pass |
| `snr_gain` reported | 831 (run 1), 120 (run 2 — kernel-bound), 994–1130 per plane (OSC) | | reported; the number is a kernel-dependent estimator ratio (§5), not a quality target |
| Detector decision (step 4b) | fast stays (ρ 0.924 vs 0.723) | | decided |

No target is missed by a margin that names a defect in this plan's code: the noise misses are favourable, the rejection miss is attributed to a normalization feature the spec places in M2 plus a fit-robustness item already in M4, and the measurement-time miss is a concurrency matter that belongs to the orchestration plan. No fix round is opened.

## 10. Findings and rulings

1. **Under-rejection (0.65 % vs 2.8 %) — accepted for M1, attributed.** Three contributors, none in the combiner's arithmetic (the linear-fit routine was re-derived independently in the whole-branch review and pins the expected z-values): (a) the external run rejects on **locally normalized** frames (LN, spec M2), where seeing and transparency variations across the field are removed before the fit; with our global scale-zero-offset normalization the residual local mismatches inflate the fit's dispersion, and a wider `adev` clips fewer pixels — the ring-shaped rejections around star cores, which the external map shows more strongly, are exactly where local mismatch is largest; (b) the linear fit's dispersion is an ordinary least-squares fit — an outlier sets its own threshold (on the plan's spike fixture the first iteration's `adev` is 87× the converged value), which costs margin at n = 208 (M4: robust MAD line fit); (c) the kernel is excluded (§5). What matters for the master is caught: the trail is fully in the high map, and the master's noise is lower than the external one's with more samples retained. **Ruling:** the rejected fraction is not a quality target by itself; M2's LN and M4's robust fit carry the item, and the Plan 5 UI reports the fraction per group so a user sees it.
2. **The default kernel stands.** Bicubic B-spline with clamping 0.3, as the spec chose and as the external run used; Lanczos-3 doubles the master's MRS noise by construction (§5) and stays an option. The ledger's "Lanczos-3" was a misreading of the log, corrected in this note and the ledger.
3. **Cross-camera reference rule (already ruled during Task 7, inherited by Plan 5):** a reference of another plane count is registration-only; the group's normalization reference is its best-weighted member; the probe validates plane counts before the per-frame loop and reports `referenceInGroup` / `normalizationReference`. The whole-branch review found the probe still reading the master's copy-through cards from the global reference under that rule (an OSC master with the mono camera's INSTRUME/OBJECT/DATE-OBS) — fixed in bf9cbafa, verified on the OSC re-run's header (§6: INSTRUME, OBJECT, DATE-OBS/DATE-END and the group key all name the OSC camera and its frames).
4. **Level conventions.** Ours is referenced to the reference frame (spec §5.1), theirs to its LN reference; and on OSC our per-CFA-channel flat normalization changes the channel ratios relative to a whole-flat normalization. Neither affects a master's usability (colour calibration is post-processing), but a user comparing levels with another stacker will see it; the docs pass of Plan 5 says so.
5. **PSF/FWHM absolute values are estimator-bound.** Never compare PSF SNR/PSFSW across tools; within our own estimator the fitted frame-level FWHM moves 30 % between two kernels whose bright-star profiles differ 3 % (§5) — a note for the M4 PSF audit, no action in M1.
6. **Detector decision (step 4b):** fast detector for measurement seeds and registration (§8).
7. **Probe-only issues found by the whole-branch review** (recorded in the ledger, no measured number affected): seed source not recorded in the JSON (`snrGain` under `--seeds full` divides a fast-seeded master PSF SNR by a full-seeded best-sub one), the seed report's O(N·M) match and per-frame file re-read (≈ 11 min on 208 frames), the seed report measuring collapsed luminance on OSC.

## 11. Consequences for Plan 5 and M2

**Plan 5 (orchestration, tables, commands, Stacking tab):**

- **Measurement concurrency.** 1.4–1.6 s per mono frame and 2.8 s per 3-plane OSC frame with one frame at a time; the pool is used inside a frame. Plan 5's driver should measure several frames concurrently under the ComputeQueue slot and the memory budget (one frame's planes plus the fitter's stamps ≈ 100–300 MB) to bring 368 frames under the 5-minute target; registration (0.3–0.4 s per frame) and integration (68–165 s per group) already fit.
- **Memory budget for maps.** With rejection maps on, the engine holds two full-image f32 planes and the group driver accumulates every output plane plus both maps outside the band budget — ≈ 940 MB for a 3-plane 6248×4176 group; budget it or stream the maps.
- **Inherit the rulings:** cross-camera reference rule; `resolve_collision` is check-then-write — serialize master writes or switch to `resolve_collision_claim`; a NaN location/scale in one frame's measurement must drop that frame, not fail the group (`BadInput` today); `channels == 0` must be refused up front; record the seed source in the group's provenance/JSON.
- **Report per group:** rejected fraction low/high, frames dropped below the weight floor, the Kish effective frame count and the weighted exposure (all computed already), so the under-rejection is visible rather than silent.
- **Docs:** kernel and noise convention (bicubic B-spline default; Lanczos-3's noise cost), level conventions (§10.4).

**M2 (local normalization):** LN for rejection and output is the named cause of the rejection gap; the checkpoint re-run after M2 reads the same 2.831 % / 2.5–2.8 % baselines.

**M4 (rejection parity, PSF audit):** robust MAD line fit for the linear-fit dispersion; the FWHM estimator's texture sensitivity; the Average arm's redundant `out == 0.0` skip (NaN marks missing and `rangeLow` rejects zeros) and the exact-tie classification median are the combiner minors the review listed for this milestone.

## 12. Timing summary (this Mac, sequential per-frame stages)

| Stage | Mono run 1 (B-spline) | Mono run 2 (Lanczos-3 + seed report) | OSC (B-spline) | Spec target |
| ----- | --------------------- | ------------------------------------ | -------------- | ----------- |
| Register | 74 s | 70 s | 62 s | ≤ 5 min both groups |
| Measure | 332 s (1.6 s/frame) | 293 s (1.4 s/frame) | 444 s (2.8 s/frame, 3 planes) | ≤ 5 min for 368 frames |
| Read | 50 s (21.6 GB) | 129 s (21.6 GB) | 123 s (49.8 GB) | integrate ≤ 8 min per group |
| Combine | 18 s | 19 s | 41 s | (same) |
| Write | 0.2 s | 0.2 s | 0.6 s | (same) |
| Seed report | — | ≈ 11 min | — | — |
| Total | 8.1 min | 19.8 min | 11.3 min | whole set ≤ 60 min |
