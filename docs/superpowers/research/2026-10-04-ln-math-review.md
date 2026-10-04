# Local normalization: the mathematics, side by side with the external reference pipeline (2026-10-04)

Branch `ln-scale-audit`, research only, no production code changed. This review answers the
owner's five questions about Athenaeum's local normalization (LN) from first principles and
against the external reference pipeline (ERP) the owner compares masters with: its batch
preprocessing script, its LN process and the open-source part of its class library. Sources
used: the ERP run log of 2026-10-04 (every parameter, per-frame scale lines, reference
selection), the ERP's shipped LN reference documentation and its open class-library sources
(the PSF estimator, the PSF scale estimator, the star detector, the PSF fit, the LN data
format), the ERP batch script's LN operation, the Horse C11 masters (Athenaeum run 1 without
LN, run 8 with LN, the ERP G master), one ERP-calibrated G frame, the run B / set 204 per-star
dumps of the previous audit, and Athenaeum's own `stacking/ln/*` and `psf_signal.rs`. Scripts:
`docs/superpowers/research/scripts/ln-math/` (numpy only; they reuse the acceptance harness's
XISF reader). Scratch: `~/.athenaeum-acc/ln-math/`.

The previous audit (`2026-10-04-ln-scale-psf-width-audit.md`) is assumed read; this document
does not repeat its tables except where a number is re-derived here.

## 0. Verdict first

Athenaeum's LN is a faithful port of the ERP's **scale estimator** and a loose, mis-scaled
reimplementation of its **background model**. Both findings are now measured on the Horse C11
G group:

1. **The halo artifact is the background model, not the scale.** The star's 128-px cell is
   rejected at 24 % in a single frame (valid) and at 29–31 % in a low-noise reference (the
   30 % validity gate trips), so the reference's node is filled from its neighbours while the
   targets keep the halo; `B = B_ref − s·B_tgt` then carries a −34 % of-background error at
   that node, and the master loses the halo. Measured on the masters: the deficit integrates
   to one 128-px node of about −30 % of background; the ERP and the no-LN master agree with
   each other within 0.5 % in every ring. The ERP is immune because its model is built at the
   normalization scale (1536 px here) and a 200-px halo is a 0.1 % perturbation of such a
   sample, whatever is rejected. Athenaeum's "scale 1024" is in reality a 128-px model.
2. **The scale estimator is the same estimator as the ERP's** — the same FWTM ellipse, the
   same fitted background, the same ratio, the same robust rejection — so the PSF-width bias
   of the previous audit is shared in principle. On this set Athenaeum's scales sit 0.4–3.1 %
   below the ERP's with a slope of −0.49 %/px² of PSF variance (r = −0.87, 28 frames); that
   residual is the part the ERP avoids through star selection (bright stars only) and a fixed
   β. The aperture term, which the ERP does not avoid, is invisible in an ERP-vs-Athenaeum
   comparison and only shows against total-flux truth (−4…−5 %/px² on run B / set 204).
3. **All stars are not better.** The faint three quarters add almost no precision (their
   per-star scatter is 3–5× larger) and bring a −1.5 % (fitted-background) bias to the
   FWTM-ellipse ratio or a +2.5 % (ratio-noise) bias to a common-aperture ratio. The ERP's
   rule — peak SNR ≥ 40, at most the N brightest unsaturated stars — is the right regime;
   it leaves 150–300 stars with 2 % per-star scatter, i.e. 0.15 % standard error.

**Мой вывод:** fix the background model first (model resolution tied to the scale, rejection
consistent between reference and target, no noise-dependent validity gate), derive the scale
from the short side like the ERP, and change the scale estimator to a photometric
(common-aperture) ratio on a bright, SNR-selected subset. Validation at master level: Horse
C11 halo rings, set 204 and run B per the plan in §8.

## 1. The two models, symbol by symbol

Notation: `v` a target pixel, `v′` its normalized value, `R` the reference plane, `T` the
target plane (already registered to `R`'s geometry), `U(·)` the interpolation of a coarse
matrix over the full frame, `s` the global relative scale, `B_R`, `B_T` background models,
`σ_R`, `σ_T` noise standard deviations.

| Element | Athenaeum (`stacking/ln`) | External reference pipeline |
| ---- | ---- | ---- |
| Normalization function | `v′ = U(A)·v + U(B)`, `A ≡ s` (or the local-scale spline), `B = B_R − A·B_T` on the mesh (`ln/mod.rs`) | `v′ = U(A)·(v − C) + U(B)`, `C = 0`; optional global step `v″ = S·(v′ − T) + R` (LN data format source; §4.1 of the math reference) |
| Interpolation `U` | bicubic B-spline with node values as coefficients (`grid.rs::evaluate_row_into`, taps `j0−1…j0+2`), approximating: at a node the value is `(2/3)²·c + …` | "scale-dependent interpolation functional" over small matrices (data format doc); the geometry matches (49×33 for 6224×4168 at scale 1024, math reference §4.1) |
| Mesh | node `(i,j)` at pixel `(i·stride, j·stride)`, `stride = scale/8` (128 at 1024) | matrices of `reference dims / (scale/modelScalingFactor)`, `modelScalingFactor = 8` → the same 128-px stride at scale 1024, **192 px at 1536** |
| What a node holds | the robust level of ITS OWN `stride×stride` cell: iterative median ± `k·1.4826·MAD` clipping (`k` = 3.0 reference / 3.2 target, 5 rounds, 2×2 subsampling above 4096 px), after a hot-pixel pass (radius 2, > 0.85 and > median + 5σ) and the clip `[4.5e-5, 0.85 of full scale]` (`background.rs`) | a sample of a **smooth background model built at the normalization scale** ("for each pixel, LN evaluates a set of global normalization parameters in a subregion of this size around the pixel" — LN doc §3.2), from samples every `backgroundSamplingDelta = 32 px`; rejected pixels (low/high clip, `|v − local bg| > 3.0 σ_R` / `> 3.2 σ_T`) are "replaced with statistically plausible estimates computed from nearby image regions" (LN doc §3.5) |
| Cell validity | a cell that loses more than `rejection_limit = 0.3` of its finite pixels to the clip is **invalid** and is refilled from the mean of its valid 8-neighbours (`fill_invalid_cells`) | no cell gate; rejection is per pixel against the local level, with replacement |
| Model resolution | ≈ the stride: each node is an independent 128-px statistic, the B-spline smooths over ±2 strides → structures ≳ 250 px are followed | ≈ the scale: at 1536 px the model has about 4–6 degrees of freedom across the short side |
| Scale rule | fixed `normalization.local.scale` (default 1024, UI 256–4096) | batch script: `scale = min(width, height) / gridSize`, `gridSize` default 4 → 6388/4 = 1597; the process snaps to its list `32 … 1024, 1536, 2048, 3072, 4096, 6144, 8192` → "Normalization scale: 1536 px" in the log |
| Reference | integration of the best `referenceFrames` (default 20; **5 in run 8**) by weight, weights 1, linear-fit clip 5.0/3.5, global additive+scaling output / scale+zero-offset rejection normalization (`reference.rs`) | best `N = max(3, min(20, floor(n/3)))` frames by PSF Signal Weight (20 of 73 L, 10 of 30 G, 7 of 21 B): pass 1 normalizes them against the single best frame (the 2nd best as the reference's outlier-rejection image), integrates them with Winsorized 4/3 and LOCAL rejection normalization, additive+scaling output; pass 2 normalizes every frame against that integrated reference |
| Star detection for the scale | registration's detector (`detect_seeds`, flux SNR > 10, saturation 0.95), capped at `measurement.maxStars = 24576`; or Measure's persisted fits as seeds | the library's star detector with `structureLayers 5`, `noiseLayers 1`, `hotPixelFilterRadius 1`, `minStructureSize auto`, `allowClusteredSources`, upper limit `0.75 × image maximum`, **peak SNR `(peak − bkg)/σ ≥ 40`**, sorted by flux, the brightest `psfMaxStars = 500` |
| PSF fit | elliptical Moffat, **β fixed to the group β** (lower median of the members' Auto β; `PsfModel::Fixed`), adaptive square region (grow by 1 px while the median drops ≥ 1 %), acceptance: centroid within 1.5 px, inside the region deflated by 15 %, `0.5·growth·FWTM ≤ r` (`psf_signal.rs`) | elliptical Moffat, **β = 4 fixed** (`psfType = Moffat4`; Auto would try 2.5/4/6/10), the same adaptive region rule (`(m0 − m)/m0 < 0.01`), the same centroid tolerance (1.5 px) and 15 % deflation (`PSFEstimator.cpp`) |
| Per-star flux | `signal = Σ (p − b_fit)` over pixel centres inside the ellipse with semi-axes `growth·FWTM/2 = α·√(10^{1/β} − 1)` (`aperture`, growth 1.0) | `signal = Σ (p − B)` for `p − B > 0` inside the ellipse with semi-axes `k·σ + 1 px`, `k = √(10^{1/β} − 1)·growth` (`PSFFit.cpp`: `rx = k*sx + 1`, `in = pos && dx²/rx² + dy²/ry² <= 1`); growth 1.0 |
| Pairing | kd-tree, half-side 4 px, fitted centroids first, detection barycentres as pass 2 when < 80 % of the target's fits matched | quad-tree, `psfSearchTolerance = 4 px`, iteration 0 on `c0` (fitted centroid), iteration 1 on `b0` (barycentre), nearest wins |
| Ratio and location | `z = signal_R / signal_T`, RCR with limit 0.3 → `s = location`, `σ_z = dispersion` (`scale.rs`, `robust::rcr`) | `R[i].z = P1[i].signal / P2[i].signal`, Robust Chauvenet rejection in three phases (median + line-fit deviation → median + sample deviation → mean + standard deviation), `rejectionLimit = 0.3`; `scale = m`, `sigma = s`, `count = survivors` (`PSFScaleEstimator.cpp`) |
| Local scale | off by default; approximating TPS on `z − s`, smoothing `5·σ_z`, ≥ 40 distinct stars, ±25 % safety band | off by default (`localScaleCorrections = false`); surface simplification (reject fraction 0.1, tolerance `3·σ_z`), TPS smoothing `5·σ_z`, 4–2100 nodes |
| Use in integration | output normalization and/or rejection normalization per group config | `.xnml` consumed by the integration process: `LocalNormalization` output, `LocalRejectionNormalization` rejection (both in the ERP run) |

Two facts in this table decide everything that follows. First, the scale estimators are the
same estimator: Athenaeum's `aperture` and the library's `PSFGoodnessOfFitCore` sum the same
background-subtracted pixels inside the same tenth-maximum ellipse of the same fitted model
(the library adds one pixel to each semi-axis and drops negative residuals). Second, the
background models are not the same model at all: Athenaeum's node is a 128-px cell statistic;
the ERP's node is a sample of a model whose resolution is the normalization scale.

## 2. Question 1 — the scale

**Rule, verified in the batch script.** `imageRefrenceDimension = Math.min(width, height)`;
`LN.scale = imageRefrenceDimension / engine.localNormalizationGridSize`; the default grid size
is 4. For 9576×6388 that is 6388/4 = 1597, exactly the logged `LN.scale = 1597`. The LN
process does not run at 1597: its documentation limits the scale to a fixed list (32, 64,
128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192) and the log prints
`Normalization scale: 1536 px` for every group. The process default is 1024; the batch script
overrides it with the image-derived value.

**Why the image size.** The LN doc §3.2: "for each pixel of the image, LN evaluates a set of
global normalization parameters in a subregion of this size around the pixel. … if the scale
has a size similar to the whole image, the result would be a global normalization." LN
exists to equalize time-varying **gradients** (light pollution, sky brightness, flat residuals
— doc §2), which are image-scale structures. A grid size of 4 gives the model about four
independent patches across the short side on any sensor and any sampling, so the model can
follow a gradient and nothing smaller. A fixed pixel count means different things on a
2000-px and a 9576-px frame: on the latter, 1024 px is a tenth of the width and the model
begins to follow objects.

**What the scale controls in the ERP model.** (a) The background models of reference and
target — their adaptivity, hence which structures are "background" (removed by `B`) and which
are "signal" (multiplied by `s`); (b) the size of the artifacts an unrejected outlier can
make ("large blobs with a size comparable to the Scale parameter", doc §3.5); (c) through
`modelScalingFactor = 8`, the stride of the stored matrices. It does **not** enter the scale
estimator: `s` is one global number from PSF photometry (doc §3.6: "an estimate of the global
relative scale of the reference image with respect to each target frame").

**What the scale controls in Athenaeum today.** Only the stride: `stride = scale/8`, and each
node is its own cell's statistic. The B-spline reconstruction over ±2 strides is the only
smoothing. So Athenaeum at "1024" has the adaptivity the ERP would have at roughly 128–256,
and at the ERP-derived 1536 it would still have 192-px cells. §5 shows what that costs.

**What Athenaeum should do.** Two separate changes, in this order of importance:

1. Make the model's resolution the scale, not the stride (§8, item 1). Without this, no
   value of the number fixes the halo class of artifact; it only changes which stars trigger
   it (the 128-px test in §5: a 192-px cell stays valid for this star but the node level
   still differs by 3.5 % of background between a single frame and the reference).
2. Derive the default like the ERP: `scale = snap(min(w, h) / 4)` to the nearest supported
   size (1536 here), shown in the UI as the derived value with the manual override kept.
   The 256–4096 UI range is fine; the default should not be a constant.

## 3. Question 2 — the ERP's parameters, one by one, with Athenaeum's equivalent

| ERP parameter (run value) | Meaning (documentation / source) | Athenaeum equivalent |
| ---- | ---- | ---- |
| `scale = 1597` → 1536 | §2 above | `normalization.local.scale = 1024`, fixed |
| `noScale = false` | compute the multiplicative component (`A`) | always on |
| `globalLocationNormalization = false` | no extra global location step after LN (`S, T, R` left empty) | none |
| `truncate = true` | clip generated normalized **images** to [0, 1]; the integration process applies the functions "without any truncation or rescaling" (doc §3.3) | n/a (no normalized images are written) |
| `backgroundSamplingDelta = 32` | step of the background sampling grid: the fine grid on which local statistics are taken before the model is built at the scale (parameter of the 2022 redesign; name and the 1536 = 8 × 6 × 32 geometry; not in the shipped doc text) | none — the cell IS the node (128 px) |
| `modelScalingFactor = 8` | matrices stored at `scale/8` stride (192 px here; 49×33 at 1024 on 6224×4168) | `stride = scale/8` (128) — the one place the two agree |
| `rejection = true` | enable outlier rejection in the background models | always on |
| `referenceRejection = true` | also reject outliers in the reference, using the first target (pass 1) as the outlier image: "the target image … will be used to reject outlier pixels on the reference image" (doc §3.5); absent in pass 2 (an integrated reference has no outliers) | reference clipped with its own 3.0σ |
| `referenceRejectionThreshold = 3.0`, `targetRejectionThreshold = 3.2` | a pixel is an outlier if its difference from the local background exceeds `k × the noise standard deviation` of that image (doc §3.5, formula [2]-[3]) | `deviation_sigma` 3.0 / 3.2 — but applied as `k × 1.4826 × MAD` **of the cell**, iterated 5 rounds, and coupled to the 30 % validity gate |
| `rejectionLimit = 0.30` | the Chauvenet rejection limit of the PSF **scale** estimator (library: "larger values increase rejection"); not a background parameter | `rcr_limit = 0.3` in `relative_scale_against`; a second, unrelated 0.3 is `BackgroundParams::rejection_limit` (cell validity) |
| `lowClippingLevel = 4.5e-5`, `highClippingLevel = 0.85` | pixels at or below / at or above these are **replaced with estimates from nearby regions** before modelling (doc §3.5, figure 2 — the high clip exists exactly to prevent "large-scale artifacts … around saturated regions, such as the bright celestial object's core") | the same numbers, applied as NaN (exclusion) — then the excluded pixels count against nothing, but the star's wings count against the 30 % gate |
| `hotPixelFilterRadius = 2` | radius of a median filter applied before rejection in the background model | `hot_radius = 2`, but only pixels > 0.85 and > median + 5σ are replaced |
| `noiseReductionFilterRadius = 0` | optional Gaussian smoothing before modelling, off | none |
| `scaleEvaluationMethod = PSFSignal` | "PSF flux evaluation": detect, fit a PSF per star, measure flux in the fitted ellipse, ratio of matched pairs, robust location (doc §3.6) | the same method (`scale.rs` + `psf_signal.rs`) |
| `psfType = Moffat4` | β = 4 fixed for every star; Auto would pick per star among β 2.5/4/6/10 by residual | `psfModel = auto` → the **group β** (lower median of members' Auto β), fixed for the group (ruling C-1) |
| `psfGrowth = 1.00` | ellipse at one tenth of the fitted maximum (`k·σ + 1 px`), [0.5, 2.0] | `FitParams::growth = 1.0`, no +1 px |
| `psfMaxStars = 500` | the 500 brightest detections (by detector flux) are fitted; the log's "Fitting 243 stars" means fewer than 500 passed the detector's cuts | `measurement.maxStars = 24576` (effectively all) |
| `psfMinSNR = 40` | detector cut `(peak − bkg)/σ_local ≥ 40` — a **peak** SNR; "By requiring relatively high SNR levels … the accuracy and robustness of the scale evaluation process can be significantly improved. The default value is 40" (doc §3.6) | detector flux-SNR > 10 (the shipped `min_snr = 5` is inert below 10) |
| `psfAllowClusteredSources = true` | keep sources with several local maxima | detector default (no such switch) |
| `saturationThreshold = 0.75`, relative | a star with any pixel above 0.75 × the image maximum is excluded (`SetUpperLimit`) | `SATURATION = 0.95` absolute on the warped pixel / detector |
| `psfStructureLayers = 5`, `psfNoiseLayers = 1` | wavelet layers for structure detection (5 → structures up to ~32 px) and the number of small-scale layers removed as noise before detection | the detector's own scale handling (no layer parameters) |
| `psfHotPixelFilterRadius = 1`, `psfNoiseReductionFilterRadius = 0`, `psfMinStructureSize = 0 (auto)` | detector pre-filters; auto minimum structure size separates hot pixels from stars by size classes | none / detector internals |
| `localScaleCorrections = false` | no local scale spline (experimental per the doc) | `localScale = false` (run 8) |
| log: "N valid PSF fits, M PSF signal estimates" | `E.total` = fits, `E.count` = ratio samples surviving the Chauvenet rejection; G: 143–255 estimates per frame, `sigma` 0.013–0.062 | `ln_matches` (pairs before RCR) and `rejected`; thousands of pairs |

Two differences with no ERP counterpart deserve a note: Athenaeum's seeds path (Measure's fits
mapped through the registration, re-fitted on the warped plane, calibrated by `k`) is a
performance device the ERP does not need because it normalizes registered files; and
Athenaeum's reference is 5 frames here against the ERP's 10 (and 20 by default on both sides).

## 4. Question 3 — all matched stars, or a bright subset?

### 4.1 The mathematics

Let the per-star fluxes be `F_R = f_R + n_R`, `F_T = f_T + n_T` with true fluxes `f` and
zero-mean noise `n` of variance `σ²_F` (sky + read noise over the aperture's pixels, plus
shot noise). The sample is `z = F_R / F_T` and the estimate is a robust location of `{z}`.

- **Variance.** `Var(z)/s² ≈ 1/SNR²_R + 1/SNR²_T` with the aperture flux SNR
  `SNR = f / σ_F`. A star at SNR 10 contributes a sample 25× noisier (in variance) than a
  star at SNR 50. In an **unweighted** location (median, RCR, Chauvenet mean) every sample
  counts the same, so adding faint stars does not reduce the standard error the way
  inverse-variance weighting would; it mostly adds spread the rejection then has to fight.
- **Ratio-noise bias (Jensen).** `E[1/F_T] ≈ (1/f_T)(1 + σ²_F/f²_T)`, so
  `E[z] ≈ s·(1 + 1/SNR²_T)`: +1 % at SNR 10, +4 % at SNR 5. Noise in the denominator biases
  the ratio **up**, star by star, and a location over a sample dominated by faint stars
  inherits a fraction of it. This is the +2.5 % the previous audit measured for a common
  aperture without a brightness cut.
- **Sky/background error.** An error `δb` in the subtracted background enters as
  `δb·N_pix`, with `N_pix ∝ FWHM²` for an aperture scaled to the PSF. For a faint star
  `δb·N_pix` is a sizeable fraction of `f`; for a bright one it is nothing. With the fitted
  background `b_fit` this error is systematic (the fit absorbs model mismatch: high on peaky
  profiles, low on flat-topped ones — audit H2), so faint stars bias the FWTM-ellipse ratio
  **down** on broad frames.
- **Saturation.** A star with a clipped core loses flux in the reference or in the target
  depending on which frame is sharper; the ratio is wrong in either direction. Both pipelines
  exclude saturated stars; the ERP's 0.75 × maximum is stricter than Athenaeum's 0.95.

Hence the shape of the problem: the brightest unsaturated stars give unbiased, low-variance
samples; a few hundred of them at 2 % scatter give a standard error of 0.1–0.2 %, which is far
below any other error in the pipeline. Faint stars add bias of both signs and little
precision. "More stars" is better only under inverse-variance weighting and an unbiased flux
definition, and even then the gain past a few hundred bright stars is negligible.

### 4.2 Numbers on real data (run B mono, production fits, per-star dumps of the previous audit)

`prod` is the production ratio (FWTM ellipse, fitted background); `c3` is the common 3·F
circle with annulus sky; "truth" is the harness photometry; `peak SNR = amplitude/σ_px`;
the location is an iterated 3σ median clip (a stand-in for RCR). Member h7 (5.53 px against
the 3.39-px reference):

| Decile by brightness | n | median peak SNR | prod median | prod MAD | c3 median | c3 MAD |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 0 (brightest) | 497 | 464 | 0.979 | 0.043 | 1.119 | 0.038 |
| 2 | 497 | 93 | 0.972 | 0.043 | 1.151 | 0.047 |
| 4 | 497 | 38 | 0.963 | 0.057 | 1.204 | 0.097 |
| 6 | 497 | 18 | 0.942 | 0.072 | 1.249 | 0.185 |
| 8 | 497 | 11 | 0.940 | 0.088 | 1.364 | 0.355 |
| 9 (faintest) | 498 | 9 | 0.920 | 0.096 | 1.477 | 0.680 |

| Selection, h7 (truth 1.1499) | n | prod location (se) | gap | c3 location (se) | gap |
| ---- | ---- | ---- | ---- | ---- | ---- |
| all pairs (production) | 4971 | 0.9589 (0.0009) | −16.6 % | 1.1554 (0.0012) | +0.5 % |
| bright quarter | 1242 | 0.9737 (0.0012) | −15.3 % | 1.1292 (0.0012) | −1.8 % |
| peak SNR ≥ 40 | 2172 | 0.9735 (0.0010) | −15.3 % | 1.1403 (0.0011) | −0.8 % |
| peak SNR ≥ 40, 500 brightest (ERP rule) | 500 | 0.9769 (0.0019) | −15.0 % | 1.1193 (0.0017) | −2.7 % |

The same four selections on h1 (4.37 px): prod −8.8 / −8.3 / −8.4 / −8.0 %, c3 +1.0 / +0.3 /
+0.7 / −0.2 %; h54 (4.37 px): prod −10.0 / −9.6 / −9.7 / −9.3 %, c3 +0.2 / −0.8 / −0.4 /
−1.2 %; h106 (2.99 px): prod −2.4 / −2.5 / −2.4 / −2.7 %, c3 +0.5 / +0.1 / +0.3 / −0.3 %;
h31 (2.52 px): prod −0.2 / −0.3 / −0.2 / −0.6 %, c3 +0.3 / −0.2 / +0.1 / −0.5 %; h162
(2.52 px, heavy wings): prod +6.9 / +6.6 / +7.0 / +6.1 %, c3 +0.9 / +0.3 / +0.7 / 0.0 %.

What the numbers say:

- The faint deciles pull the production ratio down (0.979 → 0.920 on h7: the fitted-background
  term) and the common-aperture ratio up (1.119 → 1.477: ratio noise plus sky error). Their
  MAD is 2–3× (prod) to 18× (c3) the brightest decile's.
- The standard error is 0.05–0.2 % for every selection; the faint 75 % buy no precision.
- **Selection does not touch the aperture term.** On h7 the brightest decile already reads
  0.979 against a truth of 1.150: −15 % with the ERP's own selection rule. The FWTM-ellipse
  ratio is biased by the profile shape whoever measures it (§6).
- On the broadest members the bright-subset common-aperture ratio reads 1–3 % below the
  harness truth (h7 −1.8 … −2.7 %, h54 −0.8 … −1.2 %), while "all pairs" lands within 0.5 %
  by the accident of two biases cancelling. Part of that gap is the truth's own uncertainty
  (its curve-of-growth-corrected adaptive aperture against a 3·F circle on 5.5-px stars);
  the previous audit's F1 row (median +0.02…+0.19 %, slope −0.2…−0.4 %/px² over all
  members) is the group-level statement. The residual on the broadest members is a reason
  to validate F1's radius (2·F vs 3·F vs a curve-of-growth term) at master level, not a
  reason to keep the faint stars.

**Answer.** No — "по всем звёздам" is not more accurate. It is not materially more precise
either. The ERP's rule (peak SNR ≥ 40, the brightest few hundred below 0.75 of saturation)
is the right regime for Athenaeum too, and it removes the −1.5 … −4.5 % faint-star
background term the previous audit identified. It does not remove the aperture term; only
the flux definition does.

## 5. Question 4 — the halo around the star at (7556, 3212)

### 5.1 Measurement on the masters

Ring medians of `(pixel / background − 1)`, background = the 360–420 px annulus, three
masters in the same geometry (Athenaeum run 8 with LN, run 1 without, the ERP G master):

| Ring (px) | Athenaeum LN | Athenaeum no LN | ERP | LN − no LN | ERP − no LN |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 50–60 | +30.6 % | +37.2 % | +41.1 % | −6.6 | +3.9 |
| 60–70 | +15.5 % | +23.9 % | +24.9 % | −8.4 | +1.0 |
| 70–80 | +7.8 % | +16.8 % | +17.1 % | −9.0 | +0.3 |
| 80–90 | +4.1 % | +12.2 % | +12.9 % | −8.1 | +0.7 |
| 90–100 | +2.2 % | +9.3 % | +9.8 % | −7.1 | +0.5 |
| 100–120 | +1.1 % | +6.7 % | +7.0 % | −5.6 | +0.3 |
| 120–140 | +0.8 % | +4.6 % | +4.9 % | −3.8 | +0.3 |
| 140–160 | +0.9 % | +3.2 % | +3.5 % | −2.3 | +0.3 |
| 160–180 | +1.1 % | +2.4 % | +2.4 % | −1.3 | 0.0 |
| 180–200 | +1.1 % | +1.8 % | +1.8 % | −0.7 | 0.0 |
| 200–220 | +1.1 % | +1.4 % | +1.5 % | −0.3 | +0.1 |
| 220–250 | +1.1 % | +1.1 % | +1.2 % | 0.0 | +0.1 |

The owner's numbers are reproduced (80–90: 4.1 / 12.2 / 12.9 %; 100–120: 1.1 / 6.7 / 7.0 %).
The ERP and the no-LN master agree to ≤ 1 % everywhere outside the saturated core; the LN
master is short by a smooth amount that peaks at 60–80 px (−9 % of background) and vanishes
by 220 px. The deficit map `D = LN − k·noLN` (k from the far annulus) fitted on the 128-px
B-spline node basis reproduces the smoothed ring profile to ≤ 1.3 % and concentrates on the
node at pixel (7552, 3200) — the star sits 4 and 12 px from it — with the three nearest
neighbours carrying smaller shares. The area integral of the ring deficit inside 220 px is
≈ −4.9×10⁵ %·px² (−4.9×10³ in units of background × px²), i.e. **one 128-px node at
−29.6 % of background** (a node's basis integrates to `stride² = 16384 px²`). The shape is broader than a single basis function
(the halo itself is asymmetric — a +x lobe at +72 % at 60 px — and the two Athenaeum masters
are registered 4 px apart, which adds a dipole), but the amount is one node's worth.

### 5.2 The mechanism, reproduced on a real single frame

`scripts/ln-math/halo_cells.py` runs Athenaeum's `robust_cell_level` (the same hot-pixel
pass, clip, 2×2 subsampling, 5 rounds of median ± k·1.4826·MAD, 30 % limit) on the cell
centred on that node, for the ERP-calibrated single G frame of 2024-09-28 (σ/background
0.188 — the target case) and for a deep-reference proxy (the 28-frame no-LN master plus
Gaussian noise to the noise level of an n-frame integration — the reference case):

| Plane in the star's cell | rejected fraction | valid? | level / background − 1 |
| ---- | ---- | ---- | ---- |
| single frame, target rule (3.2) | 24.0 % | yes | +38.5 % |
| proxy at single-frame noise (n = 1, 4 seeds) | 23.3–24.4 % | yes | +41.6…+42.9 % |
| proxy, n = 2 | 25.5–26.6 % | yes | +38.7…+40.4 % |
| proxy, n = 3 | 27.1–28.4 % | yes | +37.6…+39.1 % |
| proxy, n = 5 (Athenaeum's reference), reference rule (3.0) | 28.6–30.2 % | knife edge: 3 of 5 seeds valid | +37.1…+37.7 % |
| proxy, n = 10 | 29.7–30.0 % | mostly not | +36.0…+36.6 % |
| proxy, n = 28 | 30.2–30.6 % | no | — |

The eight neighbouring cells reject 0.2–2 % and sit at +1.9…+6.6 % (the outer halo); the
fill for an invalid centre is their mean, about +4.5 %. So wherever the reference's cell is
invalid and the target's is valid, `B = B_ref − s·B_tgt` at that node is
`(+4.5 %) − (+38.5 %) ≈ −34 % of background`, spread by the B-spline into exactly the kind
of bump the master shows; where both are valid, the node still differs by the noise
dependence of the iterated clip (+42.8 % at single-frame noise against +36…+37 % at
reference noise: −6 % of background even without the gate). The measured −30 % per node is
the frame-weighted mix of the two cases (the real 5-frame reference evidently fell on the
invalid side; the proxy is on the edge).

The hypothesis in the brief is therefore right in substance and wrong in one detail: it is
not that "3σ clips the halo" — in the reference 3σ is 25 % of background and the halo at
60–220 px is 1–24 % — it is that the **star's own wings** take 24 % of the cell in a noisy
frame and 29–31 % in a low-noise one, because the MAD that sets the bound shrinks with the
noise while the wings do not. The validity gate at 30 % then flips between reference and
target, and the halo, which both models had captured consistently, is carried in one and
refilled away in the other.

Why the 128-px cell is the root: a 192-px cell (the ERP-derived scale) rejects 14 % in the
single frame and 22 % in the 5-frame proxy — valid on both sides for this star, with a
remaining level difference of 3.5 % of background; a 256-px cell 9 % / 15 %, difference
2.2 %. The next brighter star trips the same gate at 192 px. The gate is the wrong tool
and the cell is the wrong model.

### 5.3 Why the ERP does not show it

Three reasons, in decreasing weight:

1. **Model scale.** The ERP's background model is built at 1536 px with samples every 32 px.
   The star plus its halo out to 220 px is `π·220² ≈ 1.5×10⁵ px²`, 6 % of one 1536² patch;
   the part that exceeds 3σ of a 10-frame reference (`3 × 0.06 = 18 %` of background, i.e.
   r ≲ 70 px) is 1 % of it. Whether those pixels are rejected and replaced in the reference
   only, in the target only, or in both, the smooth model moves by ≲ 0.1–0.5 % of
   background. In Athenaeum the halo **is** the cell: any difference in treatment moves the
   node by tens of percent. This is the first-principles answer and it does not depend on
   the ERP's rejection details.
2. **Per-pixel rejection with replacement, no cell gate.** The ERP rejects pixels against the
   local level and replaces them "with statistically plausible estimates computed from nearby
   image regions"; there is no statistic that can invalidate a whole region and swap in a
   different estimator. Its thresholds are also noise-scaled per image (3.0 σ_R, 3.2 σ_T), so
   reference and target are rejected differently — but by item 1 that difference cannot
   reach the model.
3. **Its reference is built with LN rejection normalization** of 10 frames, and the whole
   halo is "signal" at scale 1536 on both sides, multiplied by `s` and nothing else.

Its `backgroundSamplingDelta = 32` is not the protection; it is the sampling step of the
fine statistics. Its 0.85 high clip (with replacement) is what keeps the saturated core from
making the dark ring the doc's figure 2 shows; Athenaeum's NaN exclusion does the same for
the core but leaves the wings to the gate.

### 5.4 The controller's "one shared mask from the reference" against the alternatives

The owner disagrees with it; here is the evaluation.

- **Shared mask (reference-derived, applied to every target).** It makes the two cell
  statistics consistent by construction and would remove this artifact: the star's cell
  would be invalid (or valid) on both sides and `B` would cancel. Costs: target-only
  outliers (satellites, planes, cosmic rays) are no longer rejected unless the target's own
  mask is unioned in, which reintroduces an asymmetry on exactly the pixels that matter
  least; and it treats the symptom (inconsistent gate) while keeping a model whose nodes are
  128-px cell medians — the noise-dependent level difference (−6 % of background at the node
  in the valid/valid case) survives a shared mask. The ERP does the opposite direction in
  pass 1 (the first target rejects the reference's outliers) and nothing of the kind in
  pass 2.
- **Drop the gate, keep the cells.** Removes the flip but keeps the −6 % noise-dependent
  level term and keeps a model that follows halos and nebulae at 128 px; the halo then
  cancels only to the extent reference and target clip identically.
- **Larger cells (scale 1536 → 192 px).** Helps this star, fails the next; same model class.
- **Model at the scale (recommended).** Fine samples (32–64 px robust medians after the hot
  pixel pass and the clip), per-sample rejection against a smooth fit (iterate twice), then
  a smooth surface with the scale as its correlation length (a least-squares B-spline or
  thin-plate fit with knot spacing ≈ scale, or a kernel of that support), sampled on the
  `scale/8` mesh. With four to six degrees of freedom across the short side, a halo, a
  galaxy or a bright nebula knot is signal on both sides and the question of "which side
  clipped it" disappears, as in the ERP. The sample mask can then be shared or not; it stops
  mattering. The validity concept survives as a sample-count floor (no sky in a region),
  not as a rejected-fraction gate.
- **Consistency guard regardless of model.** Evaluate `B_R` and `B_T` from the SAME
  statistic under the same effective noise: e.g. add the reference's noise deficit back
  (reject the reference with the TARGET's σ) or reject both against the model with a
  threshold in units of the target's noise. Cheap, and worth doing in any interim, but it is
  a patch on the gate, not on the resolution.

## 6. Question 5 — the scale bias audit, reviewed

**Is the mechanism right?** Yes, and it is now confirmed by two independent routes.
(a) §4.2: on h7 the brightest decile's production ratio is 0.979 against a common-aperture
1.119 — a 12.5 % aperture term on bright, isolated, high-SNR stars where neither noise nor
background can be blamed; the faint deciles add the background term on top. (b) The ERP's
scale estimator uses the same aperture (§1), and on Horse C11 G the Athenaeum/ERP ratio of
per-frame scales is 0.969–0.996 over 28 frames, mean −1.9 %, **slope −0.49 %/px² of PSF
variance (−1.09 %/px of FWHM), r = −0.87**. Two estimators that share the aperture
definition can only differ by what they do not share: the fitted-background term carried by
Athenaeum's faint stars (§4), the β (group Auto vs fixed 4; the audit's H3 bounds this at a
few percent on the broadest members), the +1 px and the positive-residual rule, and the
reference (5 frames vs 10, which sets the −1.9 % offset, not the slope). The slope against
the ERP is a tenth of the slope against total-flux truth on run B / set 204 (−3.9 … −4.7
%/px²), which is what one expects if the ERP carries most of the aperture term itself.

**Does the ERP's "PSF flux evaluation" avoid the bias?** By construction, no. Its `signal` is
the sum of positive background-subtracted residuals inside the ellipse of semi-axes
`√(10^{1/4} − 1)·σ + 1 px`, i.e. the tenth-maximum ellipse of a β = 4 Moffat fit (FWTM/FWHM
= 2.03 at β 4, 1.90 at β 10): a radius of about one FWHM, enclosing 82 % of a pure β = 4
Moffat and, on real profiles, 78–93 % depending on shape (audit H1). Its `flux` member — the
sum of ALL positive residuals in the fitting rectangle — would be closer to a total flux,
but the scale estimator reads `signal`, not `flux` (`R[i].z = P1[i].signal/P2[i].signal`).
What the ERP does avoid is the faint-star background term, through `psfMinSNR = 40` (a peak
SNR), `psfMaxStars`, and the 0.75-relative saturation cut; and the fixed β = 4 gives a
slightly larger and shape-insensitive aperture than β = 10 does. Whether the ERP's own
masters carry a −5…−10 % signal-scale error on focus-broadened sets cannot be measured from
masters alone (it needs its registered frames and a truth); on this seeing-broadened set the
ERP-vs-Athenaeum slope says the two agree to within 3 % across 4.7–7.5 px, so if one is
biased against the truth here, both are.

**Is F1 right, or should Athenaeum reproduce the ERP faithfully?** Reproducing the ERP
(Moffat4, peak SNR ≥ 40, ≤ 500 stars, positive residuals, +1 px) would close the 2–3 % gap
to the ERP on broad frames and make masters comparable to the owner's baseline; it keeps the
aperture term the two share. F1 (a common circular aperture of 2–3 × max FWHM at the fitted
centroids, annulus sky, bright subset, RCR) removes the aperture term to ≤ 0.5 % median on
all four audited datasets, puts extended emission on the transparency scale it physically
has (a nebula's surface brightness does not change with seeing, so any PSF-shape-dependent
`s` mis-scales it by the aperture term), and costs less than the fit it keeps as the
centroider. It deviates from the math reference's §4.3 step 3 and from the ERP; that is the
owner's ruling to make. The review's position: F1's flux definition with the **ERP's
selection rule** (peak SNR ≥ 40 mapped to Athenaeum's detector amplitude/σ, the brightest
≤ 500 below 0.75 of the plane maximum, instead of "bright quarter", which on a 9000-star
field reaches down to peak SNR ≈ 60 and on a sparse field may not exist) is the right
combination: it inherits the part of the ERP that is right (selection) and fixes the part
that is shared and wrong (aperture). Open point for validation: the 3·F circle reads 1–3 %
under the harness truth on the broadest members (§4.2); 2·F vs 3·F vs a per-frame
curve-of-growth term is a master-level decision, and the §8 gate rows that move by design
(LN-scale row, noise, FWHM) need the replacement rows the audit proposed.

One correction to the audit's framing: it presents the FWTM-ellipse premise as Athenaeum's
assumption ("the code assumes that this ellipse encloses a fixed fraction regardless of
FWHM"). It is the ERP's assumption too, inherited with the method; the audit's H3 (own β
per plane does not help) and its "analytic Moffat total is as biased as production" row both
say the fit cannot be made shape-proof, which is why the fix is the aperture, not the model.

## 7. The Horse C11 G group as a third dataset

Per frame, Athenaeum's applied `lnScale` (run 8, 5-frame reference) against the ERP's pass-2
scale (10-frame reference), sorted by Athenaeum's FWHM:

| FWHM (px) | ERP scale (σ_z, n) | Athenaeum scale | ratio |
| ---- | ---- | ---- | ---- |
| 4.70 | 0.9994 (0.021, 255) | 0.9940 | 0.995 |
| 5.18 | 1.0664 (0.035, 211) | 1.0600 | 0.994 |
| 5.46 | 1.0106 (0.015, 197) | 1.0066 | 0.996 |
| 5.70 | 1.0014 (0.018, 192) | 0.9924 | 0.991 |
| 6.00 | 0.9794 (0.015, 194) | 0.9658 | 0.986 |
| 6.28 | 1.0186 (0.059, 162) | 0.9933 | 0.975 |
| 6.55 | 0.9816 (0.017, 171) | 0.9628 | 0.981 |
| 6.85 | 0.9775 (0.013, 143) | 0.9508 | 0.973 |
| 7.08 | 0.9584 (0.016, 136) | 0.9328 | 0.973 |
| 7.47 | 0.9494 (0.017, 134) | 0.9202 | 0.969 |

(28 pairs in `~/.athenaeum-acc/ln-math/G-scales.json`; two frames are excluded by both
pipelines.) The ERP's `sigma` of 0.013–0.062 over 134–255 samples is a standard error of
0.1–0.5 % per frame — the precision argument of §4 in the ERP's own log. Athenaeum's
reference for this group is frames 10561, 10565, 10562, 10564, 10505 (FWHM 4.7, 7.5, 7.1,
6.9, 7.1 px); the ERP's is its ten best by PSF Signal Weight.

## 8. Ranked recommendation and the validation each needs

Order of value; 1–2 are independent of 3–4 and should land first because they are what the
owner sees in the master.

1. **Background model at the scale** (§5.4, last-but-one item). Samples on a fine grid
   (32–64 px robust medians with the existing hot-pixel pass and clip), two rounds of
   per-sample rejection against a smooth fit, a smooth surface with correlation length ≈
   scale, sampled on the `scale/8` mesh; validity = sample-count floor, no rejected-fraction
   gate; the same procedure and the same thresholds for reference and target. The B-spline
   evaluator, the `.athln` format and `B = B_R − A·B_T` are untouched. New hash version for
   `ln` and `ln_reference`.
   Validation: Horse C11 G, 28 frames, the star at (7556, 3212): ring medians at 60–70,
   80–90, 100–120 px within ±1 % of background of the no-LN master's (+23.9, +12.2, +6.7 %)
   — the ERP's are +24.9, +12.9, +7.0 %; no node of `B` within 600 px of the star deviates
   from its smooth neighbourhood by more than 2 % of background; the §8 master rows
   (median, MAD, noise, FWHM, rejected fraction) on set 204 and run B unchanged within their
   bars (LN's B should move sky only); plus the nebula check the owner cares about:
   IC 434's surface brightness relative to background on the LN master within 1 % of the
   no-LN master's.
2. **Scale rule.** Default `snap(min(w, h)/4)` to the supported sizes; UI shows the derived
   value; override kept. Validation is item 1's, run at the derived scale (1536 on Horse C11,
   1536 on the 6224×4168 set).
3. **Scale estimator: photometric ratio on an SNR-selected bright subset** (F1 + the ERP's
   selection, §6). Common circle `r = 2–3 × max(F_R, F_T)` at each frame's fitted centroid,
   annulus sky with the same radii on both sides, pairs restricted to peak SNR ≥ 40 and the
   ≤ 500 brightest below 0.75 of the plane maximum (fall back to the bright quarter when
   fewer than `MIN_MATCHES` survive), RCR 0.3 as now; the Moffat fit stays the finder and
   centroider; `k` is kept and measured (should go to 1). Owner ruling required (leaves
   math §4.3 step 3). Validation: the previous audit's §7 (probe vs truth on every member
   of run B and set 204: median |gap| ≤ 0.5 %, |slope| ≤ 0.5 %/px²; `--stack-sim` master
   star flux within ±1 % of the LN reference; full set 204 checkpoint and run B grid run
   with the re-ruled rows), plus on Horse C11: the probe on the G group's 28 registered
   members against its LN reference (needs the owner's work dir) with the same two bars, and
   the ERP comparison slope of §7 reduced from −0.49 to within ±0.2 %/px² while the mean
   offset is explained by the reference choice.
4. **Star selection alone** (if 3 is refused): the ERP rule on the existing estimator.
   Expected effect: broad frames up by 1–2.5 % (the background term), the Horse C11 slope
   against the ERP to about −0.1 %/px², the truth gap on run B improved from −8.9 to about
   −7 % median — i.e. visibly better and still wrong. Validation as in 3.
5. **Reference frames.** Match the ERP's `max(3, min(20, n/3))` as the default rule rather
   than a flat 20 (5 in run 8 was a manual choice); it is not a correctness item.

Not recommended, with the reason: a shared reference mask alone (§5.4: patches the gate,
keeps the resolution and the −6 % level term); dropping the gate alone (same); a larger
fixed scale without item 1 (the next star); reproducing the ERP's estimator verbatim
(keeps the shared aperture term; acceptable only as a staging step toward 3 for
comparability).

## 9. Reproduction

From `~/.athenaeum-acc/ln-math/`, `S = docs/superpowers/research/scripts/ln-math`:

```
# §5.1 rings, deficit map, node-basis fit (three masters, same geometry)
python3 $S/halo_profile.py <ath LN master.xisf> <ath no-LN master.xisf> <external master.xisf> 7556 3212
# §5.2 cell emulation (external-calibrated single frame vs deep proxy from the no-LN master)
python3 $S/halo_cells.py <single calibrated G frame.xisf> <ath no-LN master.xisf> 7570 3274 7556 3212
# §7 pairing of per-frame scales (external log pass-2 block vs run-8.json)
python3 $S/scale_pairs.py <external log> <first line> <last line> <run-8.json> G
# §4.2 deciles (run B per-star CSVs of the previous audit)
python3 $S/ratio_deciles.py ~/.athenaeum-acc/ln-scale-audit 7 1 31 162 54 106
```

Line ranges for the external log's pass-2 G block: the block that loads the integrated
reference `LN_Reference_…FILTER-G_mono.xisf` (lines 28516–29100 in the 2026-10-04 log).

## 10. Open items

1. The ERP's absolute gap against total-flux truth on a seeing-broadened set (§6) — needs its
   registered frames; the owner's machine has them.
2. The 3·F vs 2·F vs curve-of-growth question on the broadest members (§4.2, §6).
3. The exact snapping rule of the ERP process (1597 → 1536) is inside the closed process; the
   supported list is documented and the log shows the result, which is all the rule needs.
4. The two Athenaeum G masters (run 1 without distortion, run 8 with TPS) are registered
   4 px apart at this star; irrelevant to the ring medians, relevant to any pixel-level
   comparison of the two.
