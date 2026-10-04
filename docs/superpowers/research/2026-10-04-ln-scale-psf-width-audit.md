# LN relative scale: the PSF-width bias — root-cause audit (2026-10-04)

Branch `ln-scale-audit` from `main` 60df59c4. No production code changed. Tools: a research
probe `crates/athenaeum-core/examples/ln_scale_probe.rs` and analysis scripts in
`docs/superpowers/research/scripts/ln-scale-audit/`. The scripts reuse the acceptance
harness's truth photometry (`grid-report.py`'s `photometry`) and do not rebuild it. Data:
read-only acceptance copies `~/.athenaeum-acc/acc6b3-runB` (run B: the project grid, wave
6b-3 acceptance) and `~/.athenaeum-acc/tierA-baseline` (frame set 204, the September run
with LN on). The probe read scratch `.backup` copies of their catalogs. Intermediate data:
`~/.athenaeum-acc/ln-scale-audit/` (about 16 GB of dumped warped planes, deletable).

## Verdict

The LN scale compares partial-aperture fluxes. Each star's flux (`StarFit::signal`) is
the pixel sum inside the fitted FWTM ellipse, about one FWHM in radius, minus the fit's
own background. The ellipse and the background both come from a fixed-β Moffat fit.
The code assumes that this ellipse "encloses a fixed fraction of the profile regardless
of FWHM" (`scale.rs:665-678`). On real frames that is false: the enclosed fraction runs
from 0.78–0.83 on sharp frames and on the LN reference to 0.93 on frames at 5.5–6 px.
Broad frames are more compact for their FWHM.

The fit's free background adds a second, smaller term. It absorbs the model mismatch:
it is high on peaky profiles and low on compact ones. It is multiplied by an aperture
pixel count that grows as FWHM², so it weighs most on faint stars.

These two terms explain the measured gap member by member: r = 0.994 on both mono groups,
with a residual sd of 0.7 % against a gap sd of 5.2–5.4 %. The bias is the same on the
frame set. There it appears with the detection path, an `Auto` β of 6, the legacy overlap
and the on-the-fly warp. So the seeds path, the calibration `k`, the group β, the masked
grid and the registration kernel are all ruled out as causes.

At master level the bias is a uniform signal-scale error of the group's weighted mean
gap. The bit-exact window simulation shows that correcting the scales raises the
frame-set mono master's star flux by 5.53 ± 0.05 %. The OSC blue plane moves by 1.3 %,
so the colour balance moves too. Rejection is not what spreads the error: the cores
of broad frames are almost never clipped.

**Мой вывод:** the root cause is the flux definition itself, not a parameter. Recommended
fix: measure each matched star in a common circular aperture of radius 2–3 × max(FWHM_ref,
FWHM_member), with a local annulus sky and the same radii in both frames, restricted to
the brighter stars. Keep the RCR location as it is. On all four datasets this lands at a
median gap of +0.02…+0.19 % and a slope of about 0 %/px², against −0.9…−8.9 % and
−3.7…−4.6 %/px² today. It needs an owner ruling, because it leaves the math reference's
PSF-flux method (§4.3 step 3). It also needs a re-ruled §8 gate, because the master's
signal level and noise move by design.

## 1. The symptom

`grid-report.py --ln-truth` compares each member's applied scale (`ln_scale × k`) with an
adaptive-aperture photometric ratio. The gap is applied/truth − 1, and Δσ² =
σ²_member − σ²_reference, with σ = FWHM/2.3548 from the truth's half-maximum FWHM.

| Data | Scale path | β | n | Median gap | Slope (%/px²) | r |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| run B mono (ATR2600M) | seeds × k (production) | 10 (group) | 82 | −8.85 % | −3.90 (normal rows −4.74) | −0.93 |
| run B OSC (ASI2600MC Duo, 3 planes) | seeds × k | 2.5 (group) | 303 | −0.88 % | −4.56 | −0.59 |
| set 204 mono (September run) | detection | Auto → 6 | 91 | −7.30 % | −4.21 | −0.905 |
| set 204 OSC (13 members × 3 planes) | detection | Auto → 2.5/4/4 | 39 | −1.22 % | −3.65 | −0.70 |

Gap range on mono: −17.4 … +6.6 % (run B), −15.3 … +10.2 % (set 204).

## 2. How the scale measures a star (step 1)

- **Seeds.** The reference uses `detect_stars` (registration's detector, default cuts) once
  per group (`scale.rs:620-652`). A target uses Measure's persisted fit positions, mapped
  through `forward_exact`, saturation-guarded and pre-selected against the reference tree
  (the seeds path, `scale.rs:1193-1268`), or a fresh detection on the warped plane (the
  detection path, `scale.rs:782-805`). Seeds carry positions only (C-12).
- **Fit.** `fit_stars`/`fit_stars_with_beta` → `fit_one` (`psf_signal.rs:359-436`) fits an
  elliptical Moffat `fit_moffat_2d_fixed_beta` with **β fixed** to the reference's β:
  the group β (lower median of the members' Auto β; ruling C-1) since Tier C, the reference's
  own Auto β before it. Free parameters: x0, y0, background b, amplitude A, α_x, α_y, θ.
  The fit window is the adaptive square: start max(nominal/2, 3), grow while the median
  drops ≥ 1 %, cap min(2·nominal, 48); nominal = clamp(5·σ0, 6, 48) from the field σ0
  (`psf_signal.rs:373-382`). Acceptance (`accept`, `psf_signal.rs:438-505`): centroid within
  1.5 px, inner region, and 0.5·growth·FWTM ≤ r.
- **Flux.** `aperture` (`psf_signal.rs:511-536`) sums `pixel − b_fit` over the pixel
  centres inside the ellipse with semi-axes growth·FWTM/2, with growth = 1.0
  (`psf_signal.rs:100`). FWTM = 2α√(10^{1/β} − 1), so the aperture **radius is ≈ 1.0 FWHM**
  (FWTM/FWHM = 1.90 at β = 10, 2.18 at β = 2.5).
- **Scale.** `ratio_sample` (`scale.rs:376-400`) forms z = signal_ref / signal_tgt over
  pairs within 4 px. RCR with limit 0.3 takes the location (`scale.rs:842`, `:1301`).
  `normalize_frame` multiplies by the group's seeds calibration k on a seeds-path channel
  (`ln/mod.rs:1057-1064`) and writes A = s·k, B = B_ref − A·B_tgt (`ln/mod.rs:1125-1131`).
- **What k corrects.** k = median(s_detected / s_seeds) over seven frames (C-14/C-15). It
  maps the seeds path onto the detection path: 0.9925 on run B mono, 0.998/0.999/0.995 on
  OSC. Both paths share this flux, so k cannot touch this bias.
- **The premise.** "for a fixed β the FWTM ellipse encloses a fixed fraction of the
  profile regardless of FWHM, so `signal` is width-independent" (`scale.rs:665-678`). The
  premise holds only for a star that IS that Moffat. For a pure Moffat the enclosed fraction
  EF = 1 − 10^{(1−β)/β} = 0.749 / 0.822 / 0.853 / 0.874 at β = 2.5 / 4 / 6 / 10 (Gaussian
  0.900). For a real star, EF depends on the profile's shape, and the shape changes with
  seeing, focus and tracking.
- **Where FWHM can enter.** (a) the enclosed fraction of that one-FWHM aperture; (b) the
  free background b absorbing model mismatch, times N_pix ∝ FWHM²; (c) the fixed β itself;
  (d) the fit window truncating wings, which is not reached here: the aperture is about
  1 FWHM and the window is larger; (e) ratio noise, which biases s up as the target
  aperture grows, the opposite sign.

## 3. Evidence on real frames (step 2)

**The probe is the production code.** It runs `relative_scale_against[_masked]`, then
replicates every step with the same public calls and writes one row per matched star.
- Replicated s equals the library's s bit for bit on every member.
- Run B: the probe's detection-path s lies within ±0.5 % of the applied seeds×k scale.
- Set 204: within 4.9e-5 of the `.athln` global scale on all 91 members checked.
- Σp − N·b over the ellipse reproduces `signal` to 4e-15.
- The probe's own common-aperture photometry on bright isolated stars (5F circle) agrees
  with the harness truth to a median −0.53 %, slope −0.23 %/px².

**Hypotheses, one at a time.**

- **H1. The aperture fraction varies with the profile: confirmed, dominant.** EF = flux in
  the production ellipse (annulus sky) / flux in a 4F common circle, bright isolated stars:

  | Member FWHM (half-max) | run B mono, β 10 | set 204 mono, β 6 | run B OSC G, β 2.5 |
  | ---- | ---- | ---- | ---- |
  | LN reference (3.39 / 3.39 / 3.19 px) | 0.830 | 0.844 | 0.835 |
  | < 2.9 px | 0.826 (0.782–0.831) | 0.813 | 0.804 |
  | 2.9–3.5 px | 0.831 | 0.841 | 0.833 |
  | 3.5–4.3 px | 0.882 | 0.865 | 0.837 |
  | 4.3–4.8 px | 0.889 | 0.890 | — |
  | > 4.8 px | 0.910 (up to 0.931) | 0.895 (up to 0.925) | — |

  s_LN / s_true = EF_ref / EF_member. A 5.5–6 px member therefore reads 0.83/0.93 = −11 %
  from the aperture alone. The broad frames here are more compact than a Gaussian
  (EF 0.93 > 0.90): their half-max FWHM is 15 % above the β = 10 fit's FWHM, a flat-topped
  core typical of focus or tracking broadening. The reference's EF (0.83) equals a sharp
  single frame's, so stacking 20 frames does not make the reference different. Two sharp
  members, 162 and 74 (2.52/2.76 px), have heavier wings than the reference (EF 0.78/0.79)
  and read +6.6/+4.7 %. So the gap follows the profile shape, and FWHM is its main but not
  only predictor.

- **H2. The fit background absorbs model mismatch: confirmed, second term.** On the
  reference and on sharp frames, b_fit sits above the annulus sky by +0.2…+0.4 % of the 4F
  flux. On broad frames it sits below, by −0.5 % (4 px) to −1.7 % (> 4.8 px) of the 4F flux,
  for the median star. Faint stars carry most of it. On run B member 7 (5.5 px) the
  production ratio falls from 0.968 (brightest 2 %) to 0.950 (faintest 70 %). The same
  ellipse with the annulus sky stays at 0.980 → 1.005. RCR over thousands of mostly faint
  pairs therefore inherits the full background term.

- **H3. The fixed β: refuted as a lever.** The reference and the target were each fitted at
  their own Auto β, and the broad members still read −15…−17 %:

  | Member | FWHM | Gap: target at reference β (6) | Own β | Gap: each at its own β |
  | ---- | ---- | ---- | ---- | ---- |
  | 7 | 5.53 | −17.04 % | 10 | −15.84 % |
  | 136 | 5.75 | −18.17 % | 10 | −16.92 % |
  | 56 | 5.05 | −13.78 % | 10 | −12.58 % |
  | 162 | 2.52 | +7.54 % | 6 | +7.54 % |

  The β candidates cannot describe these profiles: β = 10 is the most compact candidate and
  is still less compact than the broad frames. M2's same-β ruling stands.

- **H4. The fit window truncates broad wings: refuted.** The aperture radius is about 1 FWHM,
  well inside the adaptive window (r_max = min(2·nominal, 48) ≥ 12 px here).

- **H5. A cause in the Tier C/6b-3 machinery: refuted.** These are the seeds path, k, the
  group β, the masked overlap and the materialized registered artifact. The September frame
  set predates all of them and shows the same bias (§4).

- **H6. The registration kernel (`bicubicBSpline`, +1/3 px² per axis): not a cause here,
  but a lever on the same mechanism.**
  - In a frame set, the LN reference integrates B-spline-warped members.
  - The registration reference frame, whose map is the identity, is smoothed too: its
    warped plane equals the (1/6, 4/6, 1/6)² smoothed calibrated plane to 2.6e-7 on all
    three planes of frame 28934.
  - Every target is warped through the same kernel.
  - In run B, every frame, the reference included, is resampled onto the grid.
  - So the kernel's variance sits on both sides and cancels to first order.

  To measure the term itself, nine set-204 mono members were warped with `lanczos3` instead
  and measured against the same B-spline reference. That is the asymmetric case the term
  would produce if the reference had not been smoothed:

  | Member FWHM (B-spline → Lanczos) | Scale change | Truth change |
  | ---- | ---- | ---- |
  | 2.76 → 2.26 px | +4.38 % | −1.54 % |
  | 3.19 → 2.76 px | +2.80 % | −0.10 % |
  | 5.17 → 4.92 px | +2.02 % | −0.08 % |
  | 4.37–5.53 px (6 members) | −0.23…+0.45 % | −0.18…+0.53 % |

  The term depends strongly on width. A fixed +1/3 px² changes a 2.3 px star's shape a lot
  and a 5 px star's almost not at all. This is the earlier synthetic finding (+1/3 px² →
  ≈ 2.6 %, plan 6b-3 Task 5a) and C-11's native-vs-warped fit divergence of up to 20 % on
  the sharpest frames. All three are one mechanism: the fit's flux responds to profile shape.

  Two consequences:
  - **Moving the default kernel to an interpolating one would make this bias larger on
    sharp members** unless the flux definition changes first. With Lanczos, member 28905's
    gap went from +7.3 % to +13.7 %.
  - The truth and the common-aperture flux barely move with the kernel.

**The decomposition.** Per member, in log ratios (%), the identity gap = bg + samp + ap + trut holds:
- bg = s_lib / s(same ellipse, annulus sky), all pairs: the background term;
- samp = s(ellipse, annulus sky, all pairs, RCR) / the same estimator's median on 150 bright
  isolated stars;
- ap = ellipse / 4F circle, on the bright isolated stars: the aperture term;
- trut = 4F circle / harness truth.

| Data | Gap median | bg (r with gap, slope) | ap (r, slope) | samp | trut | r(gap, bg + ap) | Residual sd vs gap sd |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| run B mono | −9.29 | −2.08 (0.963, −0.81) | −6.99 (0.988, −3.19) | +0.60 | −0.75 | **0.994** | 0.75 vs 5.37 |
| run B OSC | −0.86 | −0.45 (0.808, −0.86) | −0.01 (0.761, −4.61) | −0.35 | −0.07 | 0.792 | 1.32 vs 2.09 |
| set 204 mono (24) | −6.90 | −1.95 (0.959, −0.91) | −4.45 (0.991, −2.79) | +0.72 | −0.80 | **0.994** | 0.72 vs 5.20 |
| set 204 OSC (39) | −1.23 | −0.58 (0.808, −0.35) | −0.10 (0.855, −3.40) | −0.29 | +0.03 | 0.876 | 1.12 vs 2.24 |

The aperture term carries the bias's dependence on FWHM. The background term adds about
a quarter of it. The OSC rows are noisier: per-plane noise is higher and the FWHM spread
is small, Δσ² from −0.46 to +0.80 px².

**Run B mono, representative members.** FWHM is the truth's half-max value. The probe's
s_lib is the detection-path scale.

| Member | FWHM | Applied | s_lib | Truth | Gap | bg | ap | EF member (reference 0.83) |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 31 | 2.52 | 0.9943 | 0.9943 | 0.9977 | −0.34 % | −0.55 | +0.13 | 0.829 |
| 162 | 2.52 | 1.0550 | 1.0601 | 0.9895 | +6.62 % | −0.10 | +6.05 | 0.782 |
| 106 | 2.99 | 1.0171 | 1.0199 | 1.0417 | −2.36 % | −0.90 | −2.38 | 0.850 |
| 20 | 4.22 | 1.0362 | 1.0384 | 1.1131 | −6.90 % | −1.55 | −3.90 | 0.861 |
| 54 | 4.37 | 0.9130 | 0.9118 | 1.0178 | −10.29 % | −2.04 | −7.31 | 0.892 |
| 35 | 4.79 | 0.8726 | 0.8719 | 0.9833 | −11.26 % | −2.53 | −9.14 | 0.902 |
| 7 | 5.53 | 0.9633 | 0.9584 | 1.1499 | −16.22 % | −4.23 | −11.09 | 0.924 |
| 136 | 5.75 | 0.9548 | 0.9514 | 1.1561 | −17.41 % | −4.54 | −11.89 | 0.929 |
| 66 | 5.97 | 0.8507 | 0.8467 | 1.0066 | −15.49 % | −4.51 | −12.39 | 0.931 |

## 4. Frame-set reproduction, set 204 (step 3)

This is `tierA-baseline`'s run 5. Its LN ran the detection path with the reference's own
Auto β (6 on mono), the legacy overlap and the on-the-fly B-spline warp. Its registration
reference (OSC frame 28934) is resampled like every member (H6). The probe warped each
member through its stored registration map and read the scale back from the run's own
`.athln` sidecars. Results:
- **The bias is the same:** 91 mono members (one has too few isolated stars for a truth)
  give a median −7.30 %, range −15.3 … +10.2 %, slope −4.21 %/px², r −0.905. OSC planes
  give a median −1.22 %, slope −3.65 %/px².
- The decomposition (above) is the same as run B's: r(gap, bg + ap) 0.994.

So the bias predates Tier C and the project grid, and every frame set with a seeing or
focus spread carries it.

## 5. The effect (step 4)

**On one frame.** A = s_true·(1+ε) gives v′ − B_ref = (1+ε)·s_true·(v − B_tgt). The
background lands on the reference's exactly, since B absorbs it. Every signal above it
(stars, nebula) is off by ε: −15…−17 % on this set's 5.5–6 px frames, +5…+10 % on
heavy-winged sharp frames.

**Rejection.** This was measured with the window simulation below, in set 204's own
configuration: rejection normalization `local`, Average | Linear fit clip (5.0/3.5).
- Inside star windows, sharp members (< 3.6 px) lose 7.3–7.7 % of their core pixels
  (r ≤ F_ref), clipped on the high side.
- Broad members lose ≤ 0.6 % of their core pixels, so their under-scaled cores are
  essentially never clipped.
- Correcting the scales changes little: mean core rejection goes from 2.13 to 1.94 %,
  mean wing rejection from 4.10 to 3.93 %.

The scale error therefore passes into the master instead of being clipped. Rejection
costs star flux of its own (about 3 % here) because of the PSF spread, with or without
the correction.

**Master level.** Three measurements cover the whole group.

1. **Masters against their LN reference (harness truth photometry; no new run).** The
   LN-only prediction is 1/(1 + weighted-mean gap), with the run's own weights.

   | Master | Reference / master star flux | LN-only prediction |
   | ---- | ---- | ---- |
   | run B OSC R / G / B | 0.9991 / 1.0031 / 1.0132 (± 0.001–0.002) | 0.9981 / 1.0042 / 1.0135 |
   | run B mono | 1.122 ± 0.006 (probe 3F bright: 1.130) | 1.073 (weighted gap −6.80 %) |
   | set 204 mono | 1.061 ± 0.006 | 1.051 (weighted gap −4.86 %) |

   OSC matches the prediction to 0.1 %. The blue plane is 1.3 % low against red, a
   colour-balance shift that depends on the night's seeing mix. On mono the master is lower
   than LN alone predicts. In set 204 the simulation below assigns the remainder to rejection.
   Run B's larger remainder (≈ 4.5 %) is open: its rejection normalization is global
   (`scaleZeroOffset`) while its output normalization is LN.

2. **Window simulation, set 204 mono, bit-exact (`--stack-sim`).**
   - Setup: 400 bright isolated stars, a 65 × 65 window each, all 92 members.
     Each member's own `.athln` is evaluated per pixel exactly as the engine does
     (`LnGrid::evaluate_row`, `A·v + B`), then combined with the production
     `combine_pixel_weighted` at the run's weights and recipe.
   - It reproduces the real master's pixels **exactly** (max |Δ| = 0.0 over 1.69 M pixels).
   - The corrected variant scales each member by c = truth / applied, with the same
     B bookkeeping.

   | Variant | LN reference / master star flux (399 stars) |
   | ---- | ---- |
   | as run (= the real master) | 1.0716 ± 0.0034 |
   | scales corrected | 1.0213 ± 0.0016 |
   | as run, no rejection | 1.0443 ± 0.0002 |
   | corrected, no rejection | 0.9918 ± 0.0002 |

   The correction raises master star flux by **+5.53 ± 0.05 %** and leaves the rejection's
   own cost unchanged (−2.6 % as run, −2.9 % corrected).

3. **Predicted §8 rows for a corrected set 204 mono master.**
   - The weighted mean correction is +5.62 %.
   - Median +0.001 %, since the master's median is sky.
   - MAD +0.8 %.
   - Master noise +3.9 %: noise is now expressed in the reference's photometric units.
     SNR rises by 1.7 % (linear estimate).
   - FWHM about +1.1 %: the broad frames' true signal share rises. This is a
     signal-weighted variance-mixture estimate. The window proxy's median is unchanged
     and its mean is +2.9 %.

**What a full run adds, and its cost.** The window simulation covers star photometry
and rejection exactly but not extended emission, which follows the same linear factor
without the rejection term. Nor does it cover run B's global-rejection case. A full
master-level validation of a fix needs:
- set 204 on the branch with a same-evening `main` re-run (R-TA-8/9): about 15–27 min each.
  Tier C's acceptance run took 14.5 min; the September tree with drizzle took 26.6 min;
- run B on the grid twice, about 13.5 min each;
- in all about 1.2–1.5 h of a quiet machine, and about 135 GB free per checkpoint tree
  (`checkpoint.sh`).

## 6. Candidate fixes, ranked (step 5)

Every estimator was computed on the same matched pairs and located by the production RCR
(0.3). Two subsets: "all" pairs as production uses them, and "bright" = the quarter with
the largest reference 3F flux. Each value is the gap against the truth: median, median |g|,
and slope in %/px².

| Estimator | run B mono | set 204 mono | run B OSC | set 204 OSC |
| ---- | ---- | ---- | ---- | ---- |
| production | −8.85 / 8.85 / −3.90 | −6.67 / 6.69 / −3.82 | −0.88 / 1.40 / −4.56 | −1.22 / 1.52 / −3.65 |
| **F1: common circle 3F, annulus sky, bright** | **+0.02 / 0.26 / −0.41** | **+0.07 / 0.26 / −0.20** | **+0.11 / 0.40 / +0.68** | **+0.19 / 0.49 / −0.46** |
| F1 variant: circle 2F, bright | −0.36 / 0.37 / −0.40 | −0.26 / 0.36 / −0.20 | +0.04 / 0.38 / +0.81 | +0.06 / 0.29 / −0.08 |
| F1 without the brightness cut (3F, all) | +2.47 / 2.47 / +0.06 | +2.70 / 2.70 / +0.18 | +0.37 / 0.77 / +1.90 | +0.41 / 0.79 / +0.26 |
| F2: ellipse ×2 FWTM, annulus sky, all | −0.07 / 0.47 / −0.80 | +0.31 / 0.36 / −0.61 | +0.40 / 0.67 / +0.74 | +0.48 / 0.68 / −0.71 |
| F3: production × per-frame aperture correction | −2.55 / 2.55 / −1.05 | −2.24 / 2.24 / −1.04 | −0.91 / 1.57 / +1.28 | −0.78 / 1.08 / −0.03 |
| same ellipse, annulus sky (bg term only) | −7.04 / 7.07 / −3.40 | −4.63 / 4.86 / −2.99 | −0.40 / 1.14 / −3.42 | −0.55 / 1.00 / −3.32 |
| ellipse ×3 FWTM, fit background | −11.64 / 11.64 / −5.67 | −10.74 / 10.74 / −6.13 | −0.72 / 1.34 / −4.69 | −1.48 / 1.67 / −3.33 |
| analytic Moffat total A·π·αx·αy/(β−1) | −9.22 / 9.22 / −4.16 | −6.85 / 6.98 / −3.96 | −0.72 / 1.24 / −3.99 | −0.96 / 1.17 / −3.27 |
| each plane at its own Auto β (H3) | broad members still −13…−17 % | — | — | — |

1. **F1: a common-aperture photometric ratio (recommended).** Per matched pair, a circle of
   radius 2–3·F, with F = max of the two frames' median fitted FWHM, at each frame's own
   fitted centroid. The local sky is a median over an annulus with the same radii in both
   frames ([6F, 8F] in the probe). The ratio sample is restricted to the brighter stars,
   then RCR(0.3) as now. The fit stays the star finder and centroider; β no longer enters
   the scale.

   Expected effect: a gap within ±0.2 % median and a slope of about 0, so every member
   lands on the reference's photometric scale.

   Risks and interactions:
   - **Ratio noise.** Without a brightness or SNR cut the larger aperture's noise biases s
     high by +2.5–2.7 % on mono, flat in FWHM but member-noise dependent. The cut is part
     of the fix, and its threshold needs the master-level check.
   - **Crowding.** Neighbours inside the circle are ratio-neutral, since the same light is
     in both frames. Neighbours' wings at the rim are not. The bright quarter passed on
     this crowded field.
   - **Masked grid (6b-3 D6).** The annulus must stay inside the eroded overlap.
     `LN_MASK_MARGIN_PX` = 48 ≥ 8F only up to F = 6 px, so the code needs a clamp or skip
     rule.
   - **Seeds calibration.** C-14's k should collapse to about 1, because an aperture sum is
     warp-invariant (C-11 measured ≤ 0.6 %). Keep the machinery, measure k, retire it later.
   - **Local scale.** R-M4c-8's local-scale spline fits the same ratio sample and positions;
     nothing changes for it.
   - **Cost.** Aperture sums are cheaper than the fit already running.
   - **Hashes.** A new LN scale-estimator version must fold into the `.athln` and
     `ln_calibration` hashes, so every sidecar recomputes once.
   - **Owner ruling.** The fix deviates from math §4.3 step 3, the PSF-flux method.

2. **F2: growth ×2–3 FWTM ellipse with an annulus sky.** This is a smaller code change: the
   measuring aperture only, leaving `accept`'s growth test alone. The median is near 0, but
   a slope of −0.6…−0.8 %/px² remains on mono, about −3 % at Δσ² = 3.5 px², because the
   aperture is still shaped by each frame's own fit.

3. **F3: keep `signal` and apply a per-frame curve-of-growth correction.** This uses
   signal / 4F flux measured on bright isolated stars. It removes the bright-star aperture
   term but not the faint-star background term the RCR is dominated by: −2.2…−2.6 % on mono.

**Not recommended, measured here.**
- Each plane at its own β: H3; it also contradicts M2's same-β ruling.
- An analytic Moffat total: as biased as production.
- A larger ellipse with the fit's own background: worse, since the background error
  scales with the pixel count.
- A different registration kernel: H6. It is not the cause, and Lanczos makes the current
  estimator worse on sharp frames.

**Not measured.**
- PSF-matching (convolve the sharper frame to the broader PSF before measuring). F1
  already reaches the truth to ≤ 0.5 % for far less machinery.

## 7. Validating a fix at master level

1. **Cheap pre-checks, before any full run.**
   - The probe's estimator against the harness truth on every member of run B
     (83 + 101 × 3) and of set 204: median |gap| ≤ 0.5 %, |slope| ≤ 0.5 %/px², and no
     instrument offset.
   - `--stack-sim` on set 204 mono: master star flux within ±1 % of the LN reference (now
     7.2 %), with an unchanged rejection profile.
2. **Full runs.**
   - Set 204: `tier1/checkpoint.sh <name> <same-evening main re-run> --numeric`, then
     `tier1-compare.py --numeric`.
   - Run B: `project-run.sh` + `grid-report.py --ln-truth`.
   - §8 rows that must hold: median ±0.1 % (predicted +0.001 %), MAD ±1 % (predicted
     +0.8 %), rejected fraction ±0.3 pp, weights ρ ≥ 0.99 (LN does not feed weights),
     drizzle level and coverage as before.
3. **Rows that move by design (owner ruling needed).**
   - The LN-scale row (median ratio 1 ± 0.5 %, scatter ≤ 1 %): mono moves by about +5 to
     +7 % with about 5 % scatter.
   - Noise ±2 %: predicted +3.9 %, with SNR +1.7 %.
   - FWHM ±1 %: predicted about +1 %.

   Proposed replacements:
   - per-member scale against the truth (`--ln-truth`, median |gap| ≤ 0.5 %, no FWHM slope);
   - master star flux against the LN reference within ±1 % (`master_phot.py`);
   - master SNR (noise per unit of reference-scale signal) not below baseline.

## 8. Reproduction

From `~/.athenaeum-acc/ln-scale-audit/`, with the release probe built into
`.superpowers/target-acc` and `S` = `docs/superpowers/research/scripts/ln-scale-audit`:

```
# run B (project grid, registered artifacts, masked overlap)
ln_scale_probe --ref <runB>/ln/mono__L__180s/reference.fits --plane 0 --beta 10 --masked \
  --stars-dir runB-stars --member h<handle>=<registered r_c_*.fits> ... > runB-mono.jsonl
python3 $S/parse_report.py <runB>/grid-report.txt runB.db runB-truth.csv
python3 $S/decompose.py runB-mono.jsonl runB-truth.csv runB-stars runB-mono-decomp.csv
python3 $S/report.py runB-mono-decomp.csv; python3 $S/terms.py runB-mono-decomp.csv "run B mono"
ln_scale_probe --estimators runB-stars/h*.csv > runB-mono-est.jsonl
python3 $S/evaluate.py runB-mono-decomp.csv runB-mono-est.jsonl "run B mono"
# set 204 (frame set: warp through the stored map, legacy overlap, the reference's Auto β)
python3 $S/prepare_frameset.py tierA.db 204 5 <old work prefix> <tierA-baseline>/stackwork/LDN1272-test mono__NoFilter__bin1__180s 4
ln_scale_probe --ref <tierA>/ln/mono__NoFilter__bin1__180s/reference.fits --plane 0 --beta auto \
  --stars-dir tierA-stars --dump-warped tierA-warped $(cat frameset-mono__NoFilter__bin1__180s-args.txt) > tierA-mono.jsonl
python3 $S/truth.py <ln reference> 0 tierA-mono-truth.jsonl f<id>=tierA-warped/f<id>.p0.fits ...
python3 $S/frameset_decomp.py tierA-mono.jsonl tierA-mono-truth.jsonl tierA-stars tierA-mono-decomp.csv
# master level
python3 $S/master_phot.py <ln reference> <master.fits> <plane> <x0> <run summary json> <group> <decomp.csv>
python3 $S/sim_config.py frameset-frames.json mono__NoFilter__bin1__180s tierA-stars/reference.p0.csv \
  <member truth csv> tierA-warped <ln reference> <master> 3.385 3.742 sim.json
ln_scale_probe --stack-sim sim.json; python3 $S/sim_report.py sim-stars.csv sim-frames.csv <member truth csv>
```

## 9. Open questions

1. **The ruling.** Should the LN scale be the photometric (total-flux) ratio? That leaves
   the PSF-flux method of math §4.3 step 3. It raises every LN master's signal level by the
   group's weighted-mean gap (about +5–7 % mono here, up to 1.3 % per OSC plane), and it
   recomputes every `.athln` once.
2. **The §8 gate.** Which replacements for the LN-scale, noise and FWHM rows, which move by
   design?
3. **Run B mono's unexplained 4.5 %.** Its master is 12 % below the LN reference, and LN
   explains 6.8 %. Set 204's remainder (2.6–2.9 %) is rejection of sharp cores, simulated
   exactly. Run B uses global rejection normalization with LN output normalization; the
   window simulation does not model that combination.
4. **Rejection under a PSF spread.** Rejection removes about 3 % of star flux in a
   mixed-seeing group (sharp cores clipped high), independent of LN. It is a photometric
   non-linearity worth its own look, not part of this bug.
5. **F1's star selection.** Bright quarter or an SNR floor, and how it behaves on sparse
   fields near `MIN_MATCHES` = 20 (narrowband, short exposures).
6. **The interpolation default.** The external comparison on the C11 set suggests an
   interpolating kernel for FWHM. With today's estimator it would raise the LN bias on sharp
   frames (H6), so F1 should land first.
7. **Generality.** This set's broad frames are focus- or tracking-broadened (flatter than
   Gaussian). A set with pure seeing variation may show a smaller slope through the same
   mechanism. Not checked on a second target.
8. **Crowded-fallback truth rows** (no curve-of-growth correction) read 0.5–3 % low on the
   broadest members against the probe's 5F photometry. The conclusions hold on the normal
   rows alone (slope −4.74 %/px², r −0.93).
