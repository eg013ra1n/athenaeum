# Plan D D0: why the stacking weight follows clipped stars and seeing (2026-10-31)

**For the astrophotographer.**
1. A sub's stacking weight counts the light of every star the Measure stage fits, and the few clipped (saturated) stars carry 60–82 % of it.
2. The light given to a clipped star is extrapolated from its unclipped wings, and on the same star it grows 14–21 % per pixel of FWHM (up to 164 % at a low group β); as seeing softens, stars also stop clipping and move into the unclipped sum. The measured "total starlight" rises with blur while the same unclipped stars stay flat.
3. So the weight credits a soft sub for light it does not have. Measured at the β set 109's own frames give (3.07), an LDN1272 subset's top weight goes to a 4-px sub; refusing two or three clipped stars (Plan C C10 r1) drops one of hc11's sharpest subs from rank 73 of 74 to 35.
4. Summing only unclipped stars does not cure it: stars that stop clipping still join that sum (+14…+22 %/px). Summing the *same* unclipped stars in every sub does: that flux follows the sky's transparency only, no refusal can move it, and the sharpest subs stay on top in every group.
5. The aperture correction `c`: on the colour R plane the knot classifier throws out 85–100 % of the brightest stars from `c`'s sample in every sub, and `c` swings 1.55–2.12 with seeing; on mono it happens on one night of hc11 (`c` +7 %) and never on LDN1272.

**Method.** A probe (`crates/athenaeum-core/examples/d0_weight_probe.rs`, uncommitted) replays the run's Measure stage
(`measure_frame_pass1` / `measure_frame_pass2` at the stored group β, the clone's config) on the trees' calibrated frames and writes
per plane the channel, the totals over subsets of the accepted fits, `c`'s candidate pool and one row per fit. It is **bit-identical**
to the C10 clones' stored run metrics on all 867 frame-planes (tflux, `c`, FWHM, PSF signal weight; 2 hc11 frames without fits left out).
Set 109: a 34-frame sample at evenly spaced FWHM ranks (all five nights, its registration reference 29390 included), raw lights ÷ 65535
through the same two passes (`--adu`; no stored row to match). The **frame-common set**: stars unsaturated, isolated (no fit within 2 R)
and SNR ≥ 20 in the registration reference, matched through the stored registration homography, unsaturated in ≥ 90 % of the frames
(371–1 328 stars per group). "Aperture" = the sum in a fixed disc (R = 28 px hc11, 16 mono, 14 OSC) above the fit's ring sky.
A slope "%/px" is the least-squares slope of ln(quantity) on the frame FWHM. Detail: `~/.athenaeum-acc/plan-d/d0/` (`summary-all.txt`,
`beta-sensitivity-full.txt`, `stored-summary.txt`, per-frame tables `probe/<ds>/<group>.p<k>.frames.tsv`, scripts `bin/`).

## Q1 — `c` item 1 on the calibrated stacking path (R-PB4-4)

"Extended share" = the share of the 64 brightest `c` candidates (accepted, unsaturated, SNR ≥ 20, by model flux) the knot classifier
flags `extended`; "depth" = the pool rank of the 64th kept candidate. runB's G / B rows match s204's within 0.04 in `c` and 0.01 in share (the same LDN1272 frames).

| group (β group / own p50) | extended share p50 / p90 / max | depth p50 | `c` p10 / p50 / p90 | `c` min / max | `c` vs FWHM | `c` at share ≥ 0.5 vs ≤ 0.1 |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| hc11 L (3.78 / 3.77) | 0.00 / 0.55 / 0.86 | 64 | 1.253 / 1.288 / 1.356 | 1.214 / 1.427 | −0.4 %/px (ρ −0.25) | 1.375 (8) vs 1.285 (57): +7 % |
| s204 mono (99.9 / 99.9) | 0.00 / 0.02 / 0.19 | 64 | 1.060 / 1.132 / 1.261 | 1.010 / 1.349 | −7.1 %/px (ρ −0.65) | no frame ≥ 0.5 |
| runB mono (99.9 / 99.9) | 0.00 / 0.02 / 0.17 | 64 | 1.081 / 1.130 / 1.261 | 1.015 / 1.349 | −7.3 %/px (ρ −0.68) | no frame ≥ 0.5 |
| s204 OSC R (2.24 / 1.50 floor) | 0.95 / 1.00 / 1.00 (p10 0.85) | 219 | 1.550 / 1.815 / 2.113 | 1.176 / 2.279 | −21.9 %/px (ρ −0.88) | 1.823 (101) vs 1.215 (1) |
| runB OSC R (2.24 / 1.50 floor) | 0.95 / 1.00 / 1.00 (p10 0.88) | 222 | 1.575 / 1.823 / 2.123 | 1.215 / 2.279 | −20.3 %/px (ρ −0.86) | 1.826 (100) vs 1.215 (1) |
| s204 OSC G (2.24 / 2.79) | 0.02 / 0.07 / 0.97 | 65 | 1.159 / 1.193 / 1.245 | 1.103 / 1.485 | −12.0 %/px (ρ −0.60) | 1.424 (7) vs 1.190 (96): +20 % |
| s204 OSC B (2.24 / 2.34) | 0.02 / 0.20 / 0.50 | 65 | 1.167 / 1.214 / 1.280 | 1.117 / 1.410 | −9.2 %/px (ρ −0.55) | 1.379 (1) vs 1.204 (83) |

- On R the kept sample's median model flux is 0.46–0.47× the top 64's; the share is pinned near 1, so `c` follows FWHM (ρ −0.86/−0.88), not
  the share (ρ +0.21/+0.30). hc11's ≥ 0.5 frames are one night, 2024-09-08 (69696–69716, share 0.59–0.86).
- `c` vs FWHM on mono is `c` doing its job: the unsaturated common set's model flux reads +9.1…+9.3 %/px against the aperture at β 99.9,
  and the star flux (model × `c`) +1.3 %/px. On R the model reads +18.9/+20.4 %/px and the flux −7.1/−4.1 %/px (the R aperture itself
  misses the slow β ≈ 1.5 wings, so the residual's sign is not settled there).

**Verdict: reproduces, by size.** On OSC R in every frame (share 85–100 %, `c` 1.55–2.12 p10–p90, −20…−22 %/px with FWHM); on OSC G in
7 of ~100 frames (`c` +20 %); on hc11 L on one night (8 frames, `c` +7 %); never on LDN1272 mono (share ≤ 0.19).

## Q2 — why `tflux` trends

Every frame of the four groups is a 180 s exposure, so exposure explains nothing. Transparency = the common set's aperture flux.

| group (β) | tflux | saturated share p50 | same saturated stars | saturated count | unsaturated sum | common set (model × `c`) | common set aperture | model / aperture | flux / aperture |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| hc11 L (3.78) | +11.4 | 0.82 | +14.4 | −18.4 | +14.3 | −1.9 | −3.3 | +1.9 | +1.4 |
| s204 mono (99.9) | +11.9 | 0.60 | +20.4 | −31.5 | +16.2 | −0.8 | −2.1 | +9.1 | +1.3 |
| runB mono (99.9) | +12.7 | 0.60 | +21.0 | −31.5 | +16.7 | −0.3 | −1.5 | +9.3 | +1.3 |
| set 109 sample, raw (99.9) | +9.1 | 0.65 | +15.5 | −35.9 | +21.5 | 0.0 | −1.6 | +10.3 | +1.7 |
| set 109 sample, raw (3.07) | +52.6 | 0.77 | +111.7 | −38.7 | +22.1 | −0.6 | −1.6 | +12.7 | +1.0 |
| s204 mono, 24 frames (6.03) | +21.2 | 0.64 | +35.4 | −32.6 | +15.0 | −1.4 | −2.5 | +9.9 | +1.2 |
| s204 mono, 24 frames (3.07) | +70.7 | 0.81 | +163.6 | −35.9 | +14.6 | −1.8 | −2.8 | +11.7 | +1.0 |

(%/px except the share; "same saturated stars" = the reference's isolated saturated stars matched in each frame, flags ignored.)
OSC at β 2.24 says the same: G / B tflux +51 / +35 %/px, the same saturated stars +57 / +58, the common set +9 / +7 against an aperture
+10 / +4 (a brighter sky-transparency trend on that rig).

**The parts that make the flux width-dependent, with size:**
- **Censored saturated fits** — the main part. They hold 60–82 % of tflux, and the same clipped stars read +14…+21 %/px at the stored
  group β against −1.5…−3.3 %/px of aperture flux; the group β sets the size: +35 %/px at β 6.03, +112…+164 %/px at β 3.07.
- **The accepted set** — as seeing softens, stars stop clipping (saturated count −18…−39 %/px) and join the unsaturated sum, which
  grows +14…+22 %/px while the same unsaturated stars stay flat; the accepted count falls −6…−10 %/px on LDN1272 (hc11 fits a fixed 500).
- **The Moffat flux at the group β** (unsaturated, before `c`): +1.9 %/px at β 3.78, +9…+13 %/px at β 2.24–99.9 (R +19…+20).
- **`c`** removes it to −1.6…+2.3 %/px on mono and G / B; on R `c` moves −20…−22 %/px (Q1).
- **Exposure: none; transparency: −1.5…−3.3 %/px on mono** (flat), so not a cause.

**Set 109's 7.7×.** Not exposure (208 × 180 s) and not transparency (common aperture −1.6 %/px over the sample). It is the group β:
the sample's own lower-median β is 3.07 (s204, the wide-heavy subset of the same frames, resolves 99.9). At β 99.9 the sample's tflux
reads 1.17× from FWHM < 2 to > 4.5 px (PSF SNR ×1.6); at β 3.07 it reads 2.37× (PSF SNR ×5.5), and the calibrated s204 subset 2.76×
between FWHM quartiles (PSF SNR ×8.2) with the top weight on a 3.98-px frame (the sharpest decile ranked 4 and 7 of 24) — run 9's
failure in kind. The rest of the 7.7× (PSF SNR ×69) is not reproduced: run 9's 208-frame group β and its calibration are not recorded,
and raw own β may read low.

## Q3 — candidate weight inputs (offline, the run's σ and M\* kept)

(c) today; (a) tflux and TMeanFlux over the unsaturated accepted fits; (b) the common set's flux (model × `c`) and mean flux, as a ratio
to the reference; (b-ap) the same with the aperture flux. Sharpest decile = the 10 % lowest FWHM, rank 1 = top weight; OSC = the run's
rule (each plane's weight over its maximum, the frame weight the planes' mean). C10 r1 replay (hc11 only): in the 8 frames the
brightest saturated fits are removed until tflux falls to the C10 r1 ratio (2–3 stars, 24–34 %); for (a) the rest of 7 refusals are its
5 brightest unsaturated fits (a worst case).

| group | ρ vs today: (a) / (b) / (b-ap) | sharpest decile median / worst rank: today / (a) / (b) / (b-ap) | C10 r1 replay |
| ---- | ---- | ---- | ---- |
| hc11 L | 0.944 / 0.966 / 0.962 | 6 / 13 · 5 / 15 · 4 / 8 · 4 / 10 | today ρ 0.890, 69860 73 → 35 (C10 r1 read 72 → 35); (a) ρ 0.989, 69860 74 → 60; (b) ρ 1.0000, no move |
| s204 mono | 0.953 / 0.976 / 0.975 | 5 / 10 · 5 / 10 · 5 / 10 · 5 / 10 | — |
| runB mono | 0.961 / 0.979 / 0.979 | 4.5 / 9 · 4.5 / 9 · 4.5 / 8 · 4.5 / 8 | — |
| s204 OSC | 0.948 / 0.931 / 0.932 | 19 / 38 · 23.5 / 53 · 13.5 / 29 · 13.5 / 21 | — |
| runB OSC | 0.941 / 0.925 / 0.923 | 19 / 39 · 23.5 / 53 · 13.5 / 29 · 13.5 / 23 | — |
| s204 mono 24 frames at β 3.07 | 0.388 / 0.386 / 0.373 | today 4, 7 of 24 (top 3.98 px) · (a) 1, 2 · (b) 1, 2 · (b-ap) 1, 2 | — |

**Proposal: (b), the frame-common unsaturated set.** It is the only input whose flux follows the same stars (−1.9…0.0 %/px on mono,
the transparency), the only one no refusal of a clipped or blended star can move (ρ 1.0000 by construction: the set is unsaturated
and isolated), and the only one that lifts the sharpest decile in every group (OSC worst rank 38/39 → 29). (a) does not pass: its sum
still grows +14…+22 %/px because stars stop clipping as seeing softens, and the worst-case replay still demotes 69860 by 14 ranks.
The flux estimator inside (b) is open: (b-ap) avoids `c` (better on R, worse on G), model × `c` inherits Q1's R item. LN's per-frame
scale (matched stars, common aperture) already measures this ratio: ρ 0.92–0.97 against (b)'s flux on mono / G / B and 0.97–0.99
against its aperture flux — but LN's reference is the top-5 members by weight, a loop the plan must break.

## What the plan should own

| measured quantity | task | bar |
| ---- | ---- | ---- |
| the weight's flux term: saturated fits 60–82 % of tflux at +14…+164 %/px; the accepted set +14…+22 %/px | D1: the weight's TFlux / TMeanFlux from the frame-common unsaturated set (after Register, or from LN's scale with the reference loop broken) | R-PC-11 (b) ln-truth at master level on hc11 / runB / s204; the C10 r1 Blend replay moves 0 ranks; at β 3.07 the sharpest decile stays on top |
| PSF SNR (tflux² / σ²) in the SNR gain's best sub | D1b: the same flux term in `paired_snr_gain` | eight equal frames ≈ 0.9 √N as today; set 109 mono's gain within √N and not lifted by its FWHM spread |
| `c` on OSC R: extended share 85–100 % in every frame, `c` −20…−22 %/px; G 7 % of frames +20 %; hc11 one night +7 % | D2: `c`'s sample on a plane whose own β is at the floor (only if D1 keeps model × `c`; with (b-ap) it leaves the weight) | `planb.sh b5 ctable` (lab truth) unmoved; R's `c` spread across a group's frames against G / B |
| the group β swings the clipped stars' slope (+20 → +164 %/px) | none if D1 drops clipped stars; recorded | — |
| Blend on the stack: C10 r1 moved today's weight by ρ 0.890 | D3: `blend_refusal` on the stacking path after D1 | R-PC-11 (b) ln-truth; weights ρ ≥ 0.9 against D1's |
