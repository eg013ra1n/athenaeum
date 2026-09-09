# Checkpoint A — registration v2 against the external sidecars (LDN 1272)

**Date:** 2026-09-09 · **Plan:** `docs/superpowers/plans/2026-09-09-stacking-m1-plan3-registration.md` (Task 7) · **Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §3, §13 · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §5

## 1. Purpose

Plan 3 delivers the registration stage (detection → quad seed → correspondences → RANSAC → σ-weighted refit → re-pairing → optional polynomial distortion → QA → registered frame + `registration_results` row). Checkpoint A asks one question before Plan 4 builds the integration on top of it: **do our per-frame transforms agree with the external stacker's on the owner's real data, and is our residual at least as good as its own?** The comparison is against the `.xdrz` alignment sidecars the external run wrote for the same calibrated frames (a 3×3 reference→target matrix, no splines) and against the per-frame residuals in its log.

The checkpoint was run three times on the same ten subjects: run 1 on the branch as delivered by Tasks 1–6, run 2 after the domain clamp and the coverage gate (Task 8), run 3 after the re-pairing pass and the probe's origin fix (Task 9). Runs 1 and 3 are reported; run 2 only motivated Task 9.

## 2. Setup

- **Reference:** `c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits` (mono, 6224×4168 — the external run's reference too).
- **Subjects:** six mono frames (same camera) and four OSC frames (`c_<stem>_d.fits`, VNG-debayered, 6248×4176 — a cross-geometry group), spanning two nights, near-identity offsets, an 8° and an 11° rotation, and the ~180° meridian-flip frames.
- **Inputs:** our own calibrated frames from the calibrated-lights export (`LDN1272-ATH/LDN 1272/camera_*/lights/`), so the only difference between the two pipelines is the registration itself.
- **Sidecars / registered images:** `LDN1272-Output/registered/…_mono/<stem>_c_r.xdrz` and `…_RGB/<stem>_c_d_r.xdrz`; the mono `_c_r.xisf` registered image for the pixel-level check. Log: `LDN1272-Output/logs/20260908121336.log`.
- **Tool:** `examples/register_probe.rs` (release build), default `RegistrationConfig` (`model: auto`, `ransacTolerancePx 1.9`, detection `minSnr 10` / `maxEccentricity 0.8`, `maxStars 2000`); the OSC subjects once with `--distortion auto` and once with `off`. Batch script, per-run JSON and the summary scripts live in the session scratchpad; the plan's Task 7 lists the exact commands.
- **Metrics:** `rmsPx` / `sigmaRmsPx` / `peakPx` through the final map over the refit inliers; `cornerDeltaPx` = worst disagreement between our reference→subject inverse and the sidecar matrix at the four reference corners + centre, under both row-order hypotheses; `starDeltaPx` (added in Task 8) = median / p95 / max of the same disagreement over the reference stars both maps place inside the subject — the registration metric proper, since corners also measure extrapolation. The external log's `delta_RMS` (first StarAlignment block per frame) is the residual we must beat.

## 3. External baselines (from the log)

| Subject | Kind | Inliers | Overlap | Regularity | Quality | delta_RMS px | sigma_RMS px |
| ------- | ---- | ------- | ------- | ---------- | ------- | ------------ | ------------ |
| 2025-09-14 … 0000 | mono | 0.991 | 1.000 | 0.995 | 0.966 | 0.234 | 0.098 |
| 2025-09-14 … 0042 | mono | 0.995 | 1.000 | 0.995 | 0.964 | 0.258 | 0.102 |
| 2025-10-18 … 0087 | mono | 0.998 | 1.000 | 0.995 | 0.968 | 0.228 | 0.077 |
| 2025-10-18 … 0061 | mono | 0.971 | 1.000 | 0.992 | 0.903 | 0.669 | 0.320 |
| 2025-10-19 … 0105 | mono | 0.996 | 0.996 | 0.995 | 0.953 | 0.328 | 0.114 |
| 2025-10-19 … 0160 | mono | 0.998 | 1.000 | 0.995 | 0.964 | 0.261 | 0.101 |
| 2025-09-14 … 0000_d | OSC | 0.990 | 0.998 | 0.994 | 0.954 | 0.319 | 0.148 |
| 2025-09-14 … 0051_d | OSC | 0.995 | 1.000 | 0.995 | 0.956 | 0.318 | 0.144 |
| 2025-10-19 … 0168_d | OSC | 0.997 | 1.000 | 0.995 | 0.962 | 0.275 | 0.118 |
| 2025-10-19 … 0235_d | OSC | 0.998 | 1.000 | 0.995 | 0.963 | 0.267 | 0.111 |

The external detector found 17 755 stars on the reference and its alignment kept the 2000 brightest; ours detects with `maxStars 2000` directly. The log prints a second block per frame (its distortion-correction pass — worse on 0061: inliers 0.888, delta_RMS 0.800); the first block is the comparable one.

## 4. Run 1 — the branch as delivered by Tasks 1–6 (HEAD 88eb08c6)

| Subject | Run | Model | Pairs | Inliers | Ratio | RMS px | Overlap | Regularity | Corner Δ same / flipped px | Ext. delta_RMS |
| ------- | --- | ----- | ----- | ------- | ----- | ------ | ------- | ---------- | -------------------------- | -------------- |
| 0000 | mono | homography | 870 | 861 | 0.990 | 0.114 | 1.00 | 0.69 | 1.42 / 1100 | 0.234 |
| 0042 | mono | homography | 861 | 844 | 0.980 | 0.119 | 0.71 | 0.75 | 2.19 / 977 | 0.258 |
| 0087 | mono | homography | 1933 | 1838 | 0.951 | 0.099 | 1.00 | 1.00 | 0.04 / 4.2 | 0.228 |
| 0061 | mono | homography | 1470 | 1412 | 0.961 | 0.109 | 0.99 | 1.00 | 2.24 / 362 | 0.669 |
| 0105 | mono | homography | 716 | 689 | 0.962 | 0.098 | 1.00 | 0.56 | 0.45 / 176 | 0.328 |
| 0160 | mono | homography | 1858 | 1817 | 0.978 | 0.122 | 1.00 | 1.00 | 0.23 / 104 | 0.261 |
| 0000_d | OSC auto | homography+polynomial3 | 241 | 239 | 0.992 | 0.167 | 0.35 | 0.50 | **178.8** / 1470 | 0.319 |
| 0051_d | OSC auto | homography+polynomial3 | 1698 | 1693 | 0.997 | 0.162 | 1.00 | 1.00 | 1.50 / 1352 | 0.318 |
| 0168_d | OSC auto | homography+polynomial3 | 1791 | 1772 | 0.989 | 0.132 | 1.00 | 1.00 | 2.02 / 68 | 0.275 |
| 0235_d | OSC auto | homography+polynomial3 | 1819 | 1802 | 0.991 | 0.129 | 1.00 | 1.00 | 1.90 / 30 | 0.267 |
| 0000_d | OSC off | homography | 241 | 239 | 0.992 | 0.178 | 0.35 | 0.50 | 0.55 / 1454 | 0.319 |
| 0051_d | OSC off | homography | 1698 | 1693 | 0.997 | 0.236 | 1.00 | 1.00 | 1.43 / 1351 | 0.318 |
| 0168_d | OSC off | homography | 1791 | 1772 | 0.989 | 0.208 | 1.00 | 1.00 | 1.46 / 67 | 0.275 |
| 0235_d | OSC off | homography | 1819 | 1802 | 0.991 | 0.191 | 1.00 | 1.00 | 1.45 / 30 | 0.267 |

What run 1 established and exposed:

- Every subject aligned, 230–360 ms each; our RMS was below the external delta_RMS on every frame already. Their scale/rotation are the exact inverse of ours (reference→target vs subject→reference).
- **Same row order everywhere**: the flipped-hypothesis corner deltas are 30–1470 px.
- **Defect 1 — a runaway polynomial.** On 0000_d, `distortion: auto` fitted an order-3 polynomial on 239 inliers sitting in one corner of the matched set (overlap index 0.35, regularity 0.5) and extrapolated 178.8 px at the far corners.
- **Defect 2 — a fraction of the field paired.** Correspondences were built once through the quad seed within 3.8 px; the seed's accuracy falls off with distance from the matched quads, so the identity-like subjects paired 1858–1933 of 2000 stars but the rotated ones 716–870 and the cross-camera 11° subject 241.
- **Defect 3 — corner deltas of ≈ 1.4–2.2 px on every rotated subject**, later traced to the probe (§5, the origin shift), not to the registration.
- `--compare` found 0 stars in the externally registered `.xisf`: it reads as f32 with ADU-scale values (median 503, max 54 428) although its bounds attribute says 0:1; the probe now normalizes it.

## 5. Run 3 — final branch (HEAD 260fdf7d)

### 5.1 Alignment per subject

| Subject | Run | Model | Pairs (+re-paired) | Inliers | Ratio | RMS px | σ px | Scale | Rot ° | Translation px | ms |
| ------- | --- | ----- | ------------------ | ------- | ----- | ------ | ---- | ----- | ----- | -------------- | -- |
| 0000 | mono | homography | 1777 (+907) | 1750 | 0.985 | 0.166 | 0.083 | 1.00772 | 8.296 | 345.1, −458.2 | 237 |
| 0042 | mono | homography | 1803 (+942) | 1778 | 0.986 | 0.193 | 0.097 | 1.00803 | −173.969 | 6085.2, 4396.8 | 235 |
| 0087 | mono | homography | 1933 | 1838 | 0.951 | 0.099 | 0.057 | 1.00010 | −0.002 | −5.5, 1.8 | 263 |
| 0061 | mono | homography | 1470 | 1412 | 0.961 | 0.109 | 0.056 | 0.99992 | −177.585 | 6089.1, 4321.9 | 265 |
| 0105 | mono | homography | 1800 (+1084) | 1738 | 0.966 | 0.132 | 0.068 | 0.99997 | 0.126 | 11.0, −87.0 | 258 |
| 0160 | mono | homography | 1858 | 1817 | 0.978 | 0.122 | 0.066 | 1.00018 | 0.029 | 33.4, −52.6 | 269 |
| 0000_d | OSC auto | homography+polynomial3 | 1616 (+1375) | 1614 | 0.999 | 0.172 | 0.081 | 0.99744 | 11.092 | 431.0, −569.4 | 327 |
| 0051_d | OSC auto | homography+polynomial3 | 1698 | 1693 | 0.997 | 0.162 | 0.085 | 0.99759 | −171.181 | 5986.6, 4521.9 | 360 |
| 0168_d | OSC auto | homography+polynomial3 | 1791 | 1772 | 0.989 | 0.132 | 0.070 | 1.00707 | 179.974 | 6232.0, 4152.4 | 322 |
| 0235_d | OSC auto | homography+polynomial3 | 1819 | 1802 | 0.991 | 0.129 | 0.071 | 1.00748 | 179.840 | 6251.9, 4184.6 | 341 |
| 0000_d | OSC off | homography | 1616 (+1375) | 1614 | 0.999 | 0.177 | 0.082 | 0.99746 | 11.092 | 430.9, −569.4 | 260 |
| 0051_d | OSC off | homography | 1698 | 1693 | 0.997 | 0.236 | 0.119 | 0.99760 | −171.181 | 5986.8, 4521.7 | 292 |
| 0168_d | OSC off | homography | 1791 | 1772 | 0.989 | 0.208 | 0.108 | 1.00722 | 179.974 | 6232.5, 4152.5 | 252 |
| 0235_d | OSC off | homography | 1819 | 1802 | 0.991 | 0.191 | 0.097 | 1.00761 | 179.841 | 6252.4, 4184.8 | 263 |

Overlap index 0.993–1.0 and regularity 1.0 on every subject (not tabulated).

### 5.2 Against the sidecar and the external log

| Subject | Run | Their scale⁻¹ | −their rot ° | Corner Δ same / flipped px | Star Δ n | median px | p95 px | max px | Ext. delta_RMS px | Ext. inliers |
| ------- | --- | ------------- | ------------ | -------------------------- | -------- | --------- | ------ | ------ | ----------------- | ------------ |
| 0000 | mono | 1.00770 | 8.293 | 0.047 / 1100 | 1882 | 0.021 | 0.040 | 0.045 | 0.234 | 0.991 |
| 0042 | mono | 1.00803 | −173.955 | 0.076 / 975 | 1897 | 0.024 | 0.056 | 0.070 | 0.258 | 0.995 |
| 0087 | mono | 1.00010 | −0.002 | 0.041 / 4.2 | 2000 | 0.025 | 0.036 | 0.041 | 0.228 | 0.998 |
| 0061 | mono | 1.00005 | −177.585 | 1.167 / 363 | 1956 | **0.806** | 1.019 | 1.141 | 0.669 | 0.971 |
| 0105 | mono | 0.99999 | 0.127 | 0.138 / 175 | 1953 | 0.108 | 0.126 | 0.136 | 0.328 | 0.996 |
| 0160 | mono | 1.00017 | 0.029 | 0.225 / 104 | 1969 | 0.167 | 0.206 | 0.224 | 0.261 | 0.998 |
| 0000_d | OSC auto | 0.99748 | 11.094 | 0.224 / 1455 | 1827 | 0.042 | 0.091 | 0.166 | 0.319 | 0.990 |
| 0051_d | OSC auto | 0.99751 | −171.154 | 0.778 / 1350 | 1844 | 0.128 | 0.355 | 0.620 | 0.318 | 0.995 |
| 0168_d | OSC auto | 1.00740 | 179.971 | 0.810 / 67 | 1988 | 0.112 | 0.339 | 0.727 | 0.275 | 0.997 |
| 0235_d | OSC auto | 1.00769 | 179.840 | 0.710 / 31 | 2000 | 0.104 | 0.298 | 0.649 | 0.267 | 0.998 |
| 0000_d | OSC off | 0.99748 | 11.094 | 0.061 / 1454 | 1827 | 0.028 | 0.036 | 0.053 | 0.319 | 0.990 |
| 0051_d | OSC off | 0.99751 | −171.154 | 0.043 / 1350 | 1844 | 0.014 | 0.036 | 0.042 | 0.318 | 0.995 |
| 0168_d | OSC off | 1.00740 | 179.971 | 0.058 / 66 | 1988 | 0.017 | 0.047 | 0.057 | 0.275 | 0.997 |
| 0235_d | OSC off | 1.00769 | 179.840 | 0.053 / 31 | 2000 | 0.019 | 0.037 | 0.052 | 0.267 | 0.998 |

The scale/rotation columns are decompositions of two different parameterizations (their 3×3 vs our homography) and disagree at the 1e-4 / 0.01° level on the flipped frames while the star-level delta is 0.02 px — the per-star delta is the comparison; the decompositions are shown only to confirm the two transforms describe the same rotation and scale.

**Pixel-level check** (0087, our registered frame vs the externally registered image, 500 brightest stars each): 499 matched within 1.5 px, median Δx 0.014 px, Δy −0.021 px, flux ratio ours/theirs 1.013.

**Timing** (release, Apple silicon, single subject): registration 235–360 ms including the subject's detection; a whole probe run with the reference detection 0.51 s wall; with the registered frame written (bicubic B-spline warp of 26 Mpx + FITS) 0.83 s. Extrapolated to the 368-frame set single-threaded: ≈ 1.5 min of registration, ≈ 3.5 min with every registered frame written.

## 6. Verdict against the spec §13 targets

The plan's Task 7 targets — translation ≤ 0.05 px, rotation ≤ 0.001°, scale ≤ 1e-4 against the sidecar (spec §13 itself names only the RMS target) — are field-wide star-position bounds of 0.05 px (translation), 0.05 px at the field edge (0.001° × 3000 px) and 0.3 px at the edge (1e-4 × 3000 px); the star-level delta over the whole field tests all three at once.

| Target | Result |
| ------ | ------ |
| Our RMS ≤ the external delta_RMS, per frame | **Passed on all ten frames**, including both OSC settings (0.099–0.193 mono vs 0.228–0.669; 0.129–0.236 OSC vs 0.267–0.319). |
| Transform agreement (star Δ max ≤ 0.05 px ⇔ all three targets) | **Passed on 0000, 0042, 0087 and all four OSC subjects with distortion off** (max 0.041–0.070 px, median 0.014–0.028 px). |
| — near miss: 0105, 0160 | Star Δ 0.108–0.136 and 0.167–0.224 px, nearly uniform across the field, i.e. a translation offset of 0.11–0.17 px between two fits that are each self-consistent at ≈ 0.1 px RMS. Cause: centroid systematics between two different detectors on the same stars, not a model error (rotation and scale agree). Ruling: accepted; Checkpoint B (the stacked result) is the arbiter. |
| — miss: 0061 | Star Δ 0.81 px median, 1.14 max, non-uniform (Δscale −1.4e-4). The sidecar cannot adjudicate this frame: the external fit is its worst of the set (delta_RMS 0.669, sigma 0.320, second pass inliers 0.888 / 0.800 px) while ours is self-consistent at 0.109 px RMS over 1412 inliers (96 % inlier ratio). Recorded, not a defect on our evidence. |
| OSC with `distortion: auto` | The sidecar is linear-only, so the polynomial's own correction (p95 0.30–0.36, max 0.62–0.73 px at the field edge) shows up as disagreement while our RMS improves 0.19–0.24 → 0.13–0.17 px. Expected; the default stays `off` (§9.2) until M4 judges it on a whole stack. |
| Row order | Same row order on every subject (flipped deltas 30–1470 px). |

## 7. Findings and rulings taken from this checkpoint (all landed on the branch)

1. **Bounded distortion domain (Task 8).** `Distortion` records the normalized box it was fitted over (inlier bounding box, 10 % margin per side) and clamps evaluation into it, so a polynomial can never fold far pixels into the subject; `transform_json` gains `domain` (absent = unbounded, older rows stay valid). Plan 4 resamples through this.
2. **Coverage-gated `auto` distortion (Task 8).** `distortion: auto` requires, besides ≥ 200 inliers, the RANSAC overlap index ≥ 0.6 (matching consistency) and the regularity index ≥ 0.6 (field coverage); an explicit order is always honoured; the skip is a warning on the row. After Task 9 every subject of this set scores 1.0 on both — the gate is insurance for genuinely low-coverage subjects.
3. **Re-pairing through the refit model (Task 9).** Every subject star is paired again through the refit linear model and RANSAC + refit run again on the larger set (`Alignment.repaired`); pairs rose from 716–870 to 1777–1803 on the rotated mono subjects and from 241 to 1616 on the cross-camera one, with the sidecar agreement improving from 1.4 px to 0.02–0.04 px on the same frames. Cost: 5–10 ms per frame.
4. **The sidecar matrix needs no origin shift.** Applying the sidecar's `AlignmentOrigin` (0.5) on both sides of its matrix adds exactly |M·½ − ½| — 1.41 px under a 180° rotation, 0.10 px under 8°, nothing under identity — which is what every rotated subject showed until the probe stopped applying it (0235_d: 1.40 → 0.019 px). The matrix applies directly to pixel-centre coordinates; the attribute is not a coordinate offset for the matrix.
5. **`starDeltaPx` is the checkpoint metric**; corner deltas stay as the row-order check and an extrapolation indicator.
6. **Registration warnings reach the log** (`"registration warning"` warn, field `note`) — an auto-distortion skip, a failed re-pairing pass, an RMS above the soft limit were previously invisible outside the probe.
7. `ROWORDER` copies through to registered frames (orientation, not CFA); the probe normalizes an ADU-scale comparison image, matches under the same row order only, and exits 1 on any sidecar/write/compare error.

## 8. Consequences for Plan 4 and Plan 5

- **Plan 4** resamples through `PixelMap` — the clamp guarantees bounded displacements everywhere; the registered frames written here carry the final map verbatim in `ATH_REGT`. The bicubic B-spline warp of a 26 Mpx frame costs ≈ 0.3 s; writing every registered frame for 368 subjects is ≈ 2 min of I/O-bound work, well inside one `ComputeQueue` slot.
- **Plan 5** passes the reference's `ROWORDER` for cross-camera groups, sets one `maxStars` per group (detector fluxes depend on it), and must not read `inlier_ratio` or the overlap index as independent quality evidence after a re-pairing pass (both ≈ 1 by construction) — `rms_px`, `regularity` and the per-frame warnings are the discriminating numbers for the frame table.
- OSC subjects form a cross-geometry group; `distortion: auto` fits order 3 on well-covered frames and lowers their RMS by ≈ 30 %. Whether that is a sharper stack is Checkpoint B's question.

## 9. Open items

- 0105 / 0160: a 0.11–0.17 px translation offset against the sidecar with both fits self-consistent — resolve by measuring star FWHM on the integrated result (Checkpoint B), not by chasing the sidecar.
- 0061: the external fit is poor on this frame; if Checkpoint B shows our stack sharp on it, the sidecar was wrong; otherwise revisit detection on low-altitude frames (the plate-solve trailing gates may apply).
- The probe's `--compare` matching is nearest-neighbour, not one-to-one, and `starDeltaPx` silently excludes stars our map places outside the subject — `count` vs `referenceStars` (1827–2000 of 2000 here) is the tell.
