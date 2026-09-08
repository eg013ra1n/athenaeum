# Stacking pipeline — mathematical reference

**Date:** 2026-09-08
**Purpose:** the algorithm-level reference for the stacking pipeline design
(`docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`). Every
formula, constant and default a task in that program needs is pinned here with
its provenance, so the implementation never has to guess and never has to read
another product's source. Nothing here is copied code; it is the mathematics
and the conventions, described in our own words.

Provenance classes used below: **[code]** read from the open-source PixInsight
Class Library (PCL, GitLab mirror `gitlab.com/pixinsight/PCL`; the
ImageIntegration/Drizzle modules at their last open commit `fd570d58`,
2024-06-21); **[doc]** official documentation or tutorials; **[forum]** posts by
the vendor's staff; **[log]** the owner's WBPP 3.0.1 run on LDN 1272
(`~/Pictures/Calibration Test/LDN1272-Output/logs/20260908121336.log`);
**[paper]** the cited publication; **[unverified]** could not be traced to a
primary source and is a design choice of ours.

Rule carried from `CLAUDE.md`: the reference implementation is never named in
code or comments. Cite the papers there; cite this document.

---

## 1. Frame quality: PSF Signal Weight and PSF SNR

Reference: Conejero, Radice, Sartori, *Image Weighting* (official doc);
`PSFSignalEstimator` [code].

### 1.1 Quantities

For one image channel after star detection and PSF fitting, with `n` accepted
fits:

- `signal_i` — the **PSF flux** of star `i`: the sum of `(pixel − B_i)` over the
  pixels inside the elliptical region at the FWTM level of the fitted PSF
  (semi-axes `k·FWTMx/2`, `k·FWTMy/2`, growth `k = 1.0`). `B_i` is the fitted
  local background. The fitted amplitude is never used; the fit only defines
  the aperture and the background ("hybrid PSF/aperture photometry").
- `TFlux = Σ_i signal_i`.
- Mean PSF flux `mean_i = signal_i / A_i`, with `A_i` the analytic area of the
  measuring ellipse `A_i = π · (k/2)² · FWTMx_i · FWTMy_i`.
- The vector of `mean_i` is outlier-cleaned with Robust Chauvenet Rejection
  (§3.4, bulk mode, rejection limit 0.5) and **Winsorized** (rejected low
  values replaced by the lowest survivor, rejected high values by the highest
  survivor — nothing is dropped from the sum).
- `TMeanFlux = Σ_i mean_i` after Winsorization. Use compensated summation.
- `M*` and `N*` come from the large-scale local background residual `R`
  (§1.3): `M* = median(R)`, `N* = 2.03636 · Sn(R)` (or `2.48308 · MAD(R)`).

### 1.2 The estimators [code]

```
PSFSW  = (5.326e-6 · TFlux · TMeanFlux) / (9.0e+6 · σ_N · M*)
PSFSNR = (1.316e-7 · TFlux²)          / (4.987e+6 · σ_N²)
```

`σ_N` is the frame's noise estimate (MRS, §2.3; fall back to `N*` when MRS is
≤ 0). The constants were calibrated on synthetic frames so that a nominal frame
scores PSFSW = 1 [doc]. Numerator of PSFSW = total signal × signal
concentration (resolution); denominator = noise × mean background (sky
brightness / gradient penalty). Weights are relative — only ratios between
frames matter — so the constants are kept for cross-tool comparability, not
for correctness.

### 1.3 Large-scale background residual (M*, N*) [code]

1. Decimate the channel by an integer factor `r` with nearest-neighbour
   sampling (deliberately no linear component injected), run a multiscale
   median transform (MMT) with `n` layers keeping only the residual, map back
   to full size by nearest neighbour. `(n, r)` per model scale: 1024 → (7, 8),
   512 → (6, 8), **256 → (6, 4)** (default), 128 → (6, 2), 64 → (5, 2).
2. `R = { L(x,y) − v(x,y) : v ≠ 0 and v < L }` — the positive deviations of
   every pixel that lies *below* the large-scale model `L`. Roughly half the
   pixels of a frame contribute.

### 1.4 Which PSF fits count [code, doc]

- Detector: structure layers 5, clustered sources allowed, local-maxima test
  off, no star limit (the vendor UI caps at 24 576).
- Saturation threshold 1.0 absolute (RCR handles saturated fits); centroid
  tolerance 1.5 px.
- PSF model: **Auto** = fit Moffat β ∈ {2.5, 4, 6, 10} and keep the smallest
  robust MAD residual; fixed `Moffat4` is the cheap alternative. Elliptical
  fits, Levenberg–Marquardt, tolerance 1e-6.
- Adaptive sampling region: start from the detected structure's bounding
  square, grow 1 px per iteration while the region median keeps dropping by
  ≥ 1 %, stop when it stabilizes.
- Accept a fit only if the centroid lies inside the region shrunk by 15 % per
  side and within the centroid tolerance of the detected barycentre; then drop
  any star with another fitted PSF within ±1 px.
- FWHM/FWTM per model in σ units: Gaussian 2.35482σ / 4.29193σ; Moffat
  `2σ√(2^{1/β} − 1)` / `2σ√(10^{1/β} − 1)` (β = 4: 0.86996σ / 1.76440σ;
  β = 2.5: 1.13050σ / 2.45918σ; β = 6: 0.69989σ / 1.36792σ; β = 10:
  0.53581σ / 1.01769σ).

### 1.5 Other measurements [code 2018, doc]

- FWHM of a frame = weighted mean of `sqrt(σx_i·σy_i)` with weights
  `ω_i = MAD_min / MAD_i` (MAD = fit residual), converted with the constants
  above. Eccentricity = `Σ ω_i sqrt(1 − (σ_minor/σ_major)²) / Σ ω_i`.
- `Median`, `MedianMeanDev` (average absolute deviation about the median),
  `Noise` = MRS, `NoiseRatio` = fraction of noise pixels.
- Classic `SNRWeight = MedianMeanDev² / Noise²`.
- The classic WBPP weighting formula (kept as an option):
  `W = A·(1 − (FWHM − FWHM_min)/(FWHM_max − FWHM_min))
     + B·(1 − (Ecc − Ecc_min)/(Ecc_max − Ecc_min))
     + C·(SNRW − SNRW_min)/(SNRW_max − SNRW_min)
     + D·(Stars − Stars_min)/(Stars_max − Stars_min) + P`
  with user weights `A, B, C, D` and pedestal `P` (a common published instance
  is 15/15/20/0 + 50).

### 1.6 Selection and reference rules [log, forum]

- WBPP 3.0.1 on LDN 1272: weight = PSF Signal Weight; every frame with
  normalized weight above **0.05** accepted (`[Frames rejection] … 0.152 >
  0.050 | accepted`); filters off by default.
- Best reference frame (auto) = highest PSF Signal Weight across the whole
  set; both cameras' groups were registered onto that one frame [log].
- ImageIntegration itself excludes frames whose normalized weight
  (per channel, divided by the maximum) is below `minWeight = 0.005` [code].

---

## 2. Noise and robust scale estimators

### 2.1 Starlet (à-trous) transform [code]

Separable 5-tap B3 spline `[1/16, 1/4, 3/8, 1/4, 1/16]` with dyadic holes.
Gaussian-noise propagation factors per layer `k_j`:
`{0.8907, 0.2007, 0.0856, 0.0413, 0.0205, 0.0103, 0.0052, 0.0026, 0.0013, 0.0007}`.

### 2.2 K-sigma estimator [code]

On layer 0: iterate `σ = stddev(kept)`, keep `|w| < 3σ`, stop when the relative
change is below 0.01 or after 10 iterations. Return `σ / 0.8907`.

### 2.3 MRS estimator [code; Starck & Murtagh 1998]

Given `J` layers `w_0..w_{J−1}` and residual `c_J`:

1. Initialize `σ` with K-sigma on layer 0 (4-layer transform).
2. Iterate: a pixel is *noise* if it is inside the clipping range and
   `|w_j(p)| ≤ 3·σ·k_j` for every `j < J`. `σ_new = stddev({ v(p) − c_J(p) })`
   over noise pixels.
3. Stop when `|σ_new − σ| / σ_new < 1e-4`; give up (σ = 0) after 16 iterations
   or with fewer than 2 noise pixels.
4. Return `σ / 0.974`.
5. Wrapper: try `J = 4`; accept if converged and the noise-pixel count is at
   least `mrsMinDataFraction = 0.01` of the pixels; otherwise retry `J = 3, 2`;
   below that fall back to K-sigma with a warning.

`rustafits::analysis::background::estimate_noise_mrs` already implements this
family (B3 à-trous, 1–6 layers, 3σ threshold, `1.4826·MAD` per layer, 500k
subsample). Task: verify its constants against §2.1–2.3 and expose the wrapper.

### 2.4 Scale estimators [code, doc]

Computed per channel on values clipped to `[1/65535, 1 − 1/65535]`, two-sided
around the median `m` (separate low/high estimates for `x ≤ m` and `x > m`),
collapsed to a scalar by the arithmetic mean of the two sides.

- **AvgDev**: mean `|x − m|`.
- **MAD**: median `|x − m|` (×1.4826 for σ-consistency where σ is needed).
- **BWMV** (biweight midvariance, Wilcox): `u_i = (x_i − m) / (9·MAD)`;
  `BWMV = n · Σ_{|u|<1} (x_i − m)²(1 − u_i²)⁴ / [ Σ_{|u|<1} (1 − u_i²)(1 − 5u_i²) ]²`;
  scale = `√BWMV`, multiplied by 0.991 for σ-consistency. ~86 % Gaussian
  efficiency, breakdown 0.5. **Default weight scale.**
- Sn / Qn (Rousseeuw–Croux) with consistency constants 1.1926 / 2.2219 and the
  small-sample factors `c_n = {2: 0.743, 3: 1.851, 4: 0.954, 5: 1.351,
  6: 0.993, 7: 1.198, 8: 1.005, 9: 1.131, else n/(n−0.9) for odd n, 1 for even}`.
- Location = median. Relative scale of frame `i` to the reference (frame 0):
  `s_i = scale_0 / scale_i`.

### 2.5 Noise scaling factors for the classic SNR weight [code]

Per channel: clip to `[2/65535, 1 − 2/65535]`, `c = median`; low side:
restrict twice to `[max(clipLow, c − 4·stddev), c]` and take `σ_low = stddev`;
symmetric high side. Classic weight `w = (noiseScale / σ_n)²`.

---

## 3. Integration: rejection, normalization, weights

Reference: ImageIntegration module [code], its documentation [doc], and the
WBPP log [log].

### 3.1 Vendor defaults (for parity, not as our defaults)

| Parameter | Default | Note |
| ---- | ---- | ---- |
| combination | Average | Median, Minimum, Maximum |
| weightMode | PSFSignalWeight | ExposureTime, SNREstimate, PSFSNR, KeywordWeight, DontCare |
| weightScale | BWMV | AvgDev, MAD |
| minWeight | 0.005 | frames below are excluded |
| normalization (output) | AdditiveWithScaling | None, Additive, Multiplicative, MultiplicativeWithScaling, Local, Adaptive |
| rejectionNormalization | Scale + zero offset | None, EqualizeFluxes, Local, Adaptive |
| pcClipLow / High | 0.2 / 0.1 | |
| sigmaLow / High | 4 / 3 | |
| winsorizationCutoff | 5 | |
| linearFitLow / High | 5 / 4 (WBPP uses 5 / 3.5 [log]) | |
| esdOutliersFraction / esdAlpha / esdLowRelaxation | 0.30 / 0.05 / 1.0 | |
| rcrLimit | 0.1 | 0.5 = Chauvenet |
| clipLow / clipHigh | true / true | |
| rangeClipLow / rangeLow | true / 0.0 | |
| rangeClipHigh / rangeHigh | false / 0.98 | |
| largeScaleClipLow/High, protectedLayers, growth | off, 2, 2 | |
| noiseEvaluationAlgorithm | MRS | KSigma, NStar |
| psfStructureLayers / psfType | 5 / Moffat4 | |

WBPP's auto-selection observed in the log: 10–100 calibration frames →
Winsorized sigma clipping; 20 and 208 lights → Linear fit clipping. (Older
WBPP 2.4 documentation said < 6 percentile, 6–14 Winsorized, ≥ 15 ESD; the
3.0.1 log is the primary source for our Auto rule.)

### 3.2 Per-pixel stack pipeline [code]

1. **Range rejection** on raw values (`raw ≤ rangeLow`, `raw ≥ rangeHigh`).
2. **Rejection normalization** of a working copy (§3.3).
3. The **rejection algorithm** on the sorted, unrejected working values.
4. Optional **large-scale** post-processing of the rejection maps (§3.5).
5. **Output normalization** of the raw values of the survivors (§3.6).
6. **Combination** (§3.6).

If every sample is rejected the output is the median of all `n` raw values.
Zero-valued samples are skipped by additive normalizations and by the
weighted average (they mark missing coverage). After a rejection normalization
other than EqualizeFluxes, if any value went negative the whole stack is
shifted up by `−min` (a scale-invariant pedestal).

### 3.3 Rejection normalization [code]

- Scale + zero offset: `x′ = (x − m_i)·s_i + m_0`.
- Equalize fluxes: `x′ = x · m_0 / m_i`.
- Local: `x′ = LN_i(x, X, Y)` (§4).

### 3.4 Rejection algorithms [code, paper]

Notation: sorted sample `x_(1..n)`, median `m`, low/high sides gated by
`clipLow`/`clipHigh`.

- **Min/Max**: reject the `minMaxLow` smallest and `minMaxHigh` largest.
- **Percentile clipping** (single pass, n ≥ 2): reject low if
  `(m − x)/m > pcClipLow`, high if `(x − m)/m > pcClipHigh`.
- **Sigma clipping** (n ≥ 3): iterate { `σ` = sample standard deviation about
  the mean; reject low if `(m − x)/σ > σ_low`, high if `(x − m)/σ > σ_high` }
  until nothing is rejected or n < 3. Note: mean-based σ, median centre.
- **Winsorized sigma clipping** (n ≥ 3, Huber): Winsorization returns
  `(μ_w, σ_w)`: start `μ = median`, `σ = 1.1926·Sn`; loop { `t0 = μ − 1.5σ`,
  `t1 = μ + 1.5σ`; on the first pass with cutoff `c` (5): values below `t0`
  become `t0` if above `μ − cσ` else `μ` (extreme outliers go to the centre,
  not the neighbour), symmetric above; later passes cutoff = 0, plain
  clamping to `[t0, t1]`; `σ = 1.134 · stddev(v)`, `μ = mean(v)`; stop when
  `|Δσ|/σ < 0.0005` after ≥ 2 passes }. Then clip like sigma clipping with
  `(μ_w, σ_w)`; repeat until stable. `1.134` is the normal-distribution
  correction for the 1.5σ Winsorization point.
- **Linear fit clipping** (n ≥ 5): fit `y = a + b·j` to sorted values against
  rank `j` with a robust minimum-absolute-deviation line fit; dispersion
  `s = 2 · adev · sqrt(1 + b²)` (`adev` = mean absolute deviation from the
  line; the factor 2 makes the thresholds comparable with sigma clipping);
  reject low if `(ŷ_j − x_j)/s ≥ linearFitLow`, high if
  `(x_j − ŷ_j)/s ≥ linearFitHigh`; iterate until stable or n < 3.
  Slope map = `atan(b)/(π/4)` clamped to [0, 1].
- **Generalized ESD** (Rosner 1983; n ≥ 3): `k = clamp(trunc(f·n), 1, n−2)`,
  `f = esdOutliersFraction`. For `i = 0..k−1` on the current set `X` (size
  `n_i`): trimming counts `t_h = max(1, trunc(f·n_i) − i)`,
  `t_l = max(1, trunc((f/ρ)·n_i) − i)` with `ρ = esdLowRelaxation`; centre
  `μ` = trimmed mean (drop `t_l` lowest, `t_h` highest) when
  `t_l + t_h < n_i − 2`, else the median; `s_h = stddev(X; μ)`, `s_l = ρ·s_h`;
  studentized residual `r = (x − μ)/s_h` for `x ≥ μ`, `(μ − x)/s_l` otherwise;
  `T_i = max r`, remove that element, continue. The number of outliers is the
  first `i` for which `T_i < λ_i`. Critical value (two-tailed):
  `p = α / (2(n − i))`, `t = t_{p, ν}` with `ν = n − i − 2` (upper-tail
  Student's t quantile via the incomplete beta function),
  `λ_i = t·(n − i − 1) / sqrt((n − i − 2 + t²)(n − i))`.
- **RCR** (Maples et al. 2018; n ≥ 3): three phases of decreasing robustness,
  each iterated to convergence: (0) `μ = median`, `σ = LineFitDeviation`;
  (1) `μ = median`, `σ = SampleDeviation`; (2) `μ = mean`, `σ = stddev`. Each
  iteration computes `d_lo = n·Q((μ − x_min)/σ)`, `d_hi = n·Q((x_max − μ)/σ)`,
  `Q(z) = ½ erfc(z/√2)`; if the smaller is `< rcrLimit` reject that single
  extreme (ties → high), else the phase ends. `SampleDeviation =
  F(N) · quantile_{0.683}(|x − μ|)`, `F(N) = 1 / (1 − 2.9442·N^{−1.073})`.
  `LineFitDeviation`: sort `|x − μ|`, keep the first
  `n′ = trunc(0.683N + 0.317)` (needs ≥ 8, else SampleDeviation), regress
  against `x_i = √2·erf⁻¹((i + 1 − 0.317)/N)`, return `F(N)·ŷ(1)`.
- **CCD clip**: `σ = sqrt((RN/g)²/65535 + m/(g·65535) + sn²·m²/65535)` then
  sigma clipping about the median (16-bit DN model).

### 3.5 Large-scale pixel rejection [code]

Per frame and channel: binarize the low (or high) rejection map; MMT with
`protectedLayers` layers keeping only the residual (erases rejected blobs
smaller than ~2^protectedLayers px, keeps trails and other large structures);
dilate with a circular element of diameter `2·growth + 1`; every dilated pixel
becomes rejected; the stack is re-integrated from the maps.

### 3.6 Output normalization and combination [code]

- None; Additive `x + (m_0 − m_i)`; Multiplicative `x · m_0/m_i`;
  Additive with scaling `(x − m_i)·s_i + m_0`; Multiplicative with scaling
  `(x/m_i)·s_i·m_0`; Local (§4); Adaptive: a grid of `gridSize` (16) cells on
  the long side, per-cell median and two-sided scale, surfaces as thin-plate
  splines discretized on a 64 px grid, `x′ = (x − m_i(X,Y)) · [s_0/s_i](X,Y)
  side-selected by `x ≤ m_i` + `m_0(X,Y)`.
- Average: `Σ w_i x_i / Σ w_i` over survivors with `x ≠ 0` and `w > 0`;
  Median (mean of the central two for even n); Min; Max.
- Out-of-range result: if min < 0 rescale `(x − lo)/(hi − lo)`, else if
  max > 1 divide by max; or truncate when asked.

### 3.7 Weights [code]

Per channel, before normalization: PSFSignalWeight (default, §1.2), PSFSNR,
PSFScaleSNR (`w = 1/(r_c²·σ_n²)` with the LN relative scale `r_c`),
SNREstimate (`w = (noiseScale/σ_n)²`), ExposureTime, KeywordWeight, DontCare.
Then divide by the maximum weight per channel; drop frames whose minimum
channel weight is below `minWeight`; abort below 3 frames. Weighted total
exposure = `Σ expTime_i · w_i`. Written per frame to the drizzle sidecar:
location, reference location, scale factors, weights, rejection map.

---

## 4. Local normalization

Reference: the LN data format [code], the PSF scale estimator [code], the
1.8.9 release notes and WBPP threads [forum]. The LN process itself is
closed-source; §4.2 and §4.4 are our own design informed by the verified
pieces.

### 4.1 Normalization function [code]

Per channel `c` and pixel `i`:

```
v′(c,i) = U(A_c)(i) · (v(c,i) − C_c) + U(B_c)(i)
```

`A` = local scale matrix, `B` = local zero-offset matrix, `C` = input bias
(0), `U` = **bicubic B-spline** interpolation of the small matrix over the
reference geometry (`s_x = A.width/refWidth`). The matrices have the reference
dimensions divided by the normalization **scale** (minimum 16 px, typically
256–1024). The sidecar also carries the global step `v″ = S·(v′ − T) + R`
(scale, target location, reference location), the per-channel
`RelativeScaleFactors` consumed by the PSFScaleSNR weight, and the reference
geometry. The log's `.xnml` for a 6224×4168 frame at scale 1024 holds a
49×33 grid, i.e. a stride of 128 px = scale/8.

### 4.2 Background models [forum + §1.3]

"A multiscale local normalization algorithm based on the multiscale median
transform." Background models of reference and target are large-scale MMT
residuals (the §1.3 machinery: decimate → MMT residual → nearest-neighbour
upsample) at the model scale. Pixels are rejected from the models by a low
clipping level (4.5e-5), a high clipping level (0.85 relative to the
maximum), and deviation thresholds from the local level (reference 3.0 σ,
target 3.2 σ) with a hot-pixel median filter of radius 2 first [log parameter
block; the meaning of the thresholds is our reading, unverified].

### 4.3 Relative scale — PSF method [code]

1. Detect and fit PSFs on reference and target (§1.4).
2. Match stars by proximity: quad-tree search in a square of half-side
   4 px around each reference centroid, nearest wins; a second pass using
   barycentres runs when < 80 % matched, keeping the larger set.
3. Samples `z_k = signal_ref,k / signal_tgt,k` (background-subtracted PSF
   fluxes) with positions.
4. **RCR** on `{z_k}` (§3.4) with rejection limit 0.3 → `scale = μ`,
   `σ_z = σ`.
5. Optional local scale model (off by default): residuals `z − scale` are
   surface-simplified (tolerance `3·σ_z`, reject fraction 0.1) and, with
   4–2100 nodes, fitted with an approximating thin-plate spline
   (smoothing `5·σ_z`).

### 4.4 Our grid construction [design]

With `s` the global scale of §4.3 and `B_ref`, `B_tgt` the background models:
`A(x,y) = s` (or the local spline of §4.3 step 5 when enabled),
`B(x,y) = B_ref(x,y) − s · B_tgt(x,y)`, both sampled on the stride grid and
interpolated with the bicubic B-spline `U`. Used for output normalization and
for rejection normalization. The reference frame for a group is the
integration of the best `N` frames by weight (`N = 20`, linear-fit rejection,
global normalization) — the log's "Local normalization: reference frame
generated by integrating 20 frames".

### 4.5 Use inside integration and drizzle [code]

Output normalization: `x′ = LN_i(x, X, Y, c)`. Rejection normalization: the
same function on the working copy before rejection. Drizzle applies `LN` at
the reference coordinate of the output pixel (the grid lives in reference
geometry, no inverse mapping needed). Missing or invalid LN data → the frame is
excluded (output LN) or the run fails (rejection LN).

---

## 5. Registration

### 5.1 Star detection [code]

Defaults: structure layers 5, noise layers 0, hot-pixel filter radius 1,
sensitivity 0.5, peak response 0.5, bright threshold 3.0, max distortion 0.6,
local-maxima limit 0.75, upper limit 1.0.

Structure map: optional 3×3 median (hot pixels) → optional Gaussian low-pass of
size `1 + 2^noiseLayers` → high-pass by subtracting a Gaussian of size
`1 + 2^structureLayers` (5 layers ≈ 33 px, structures up to ~32 px), truncate
at 0, rescale → 3×3 dilation → adaptive binarization at
`median + 3·σ_noise` (σ_noise = K-sigma on wavelet layer 1 divided by
0.2007) → 3×3 erosion. A local-maxima map counts peaks per structure to reject
blends unless clustered sources are allowed.

Per candidate: reject if touching the border, `count < minStructureSize`,
`peak > upperLimit`, coverage `count/d² < (π/4)(1 − maxDistortion)`, or more
than one maximum without clustering. Local background and dispersion
(`1.4826·MAD`) from a ring around the box grown from 4 px until its median
stabilizes to 1 %. Barycentre from the box after truncating at
`median + 1.5·stddev`. Detection SNR `(peak − b)/σ_b ≥ 0.1 + 4.8·(1 −
sensitivity)` (2.5 by default); kurtosis of significant pixels
`≥ 0.1 + 9.8·(1 − peakResponse)` (5.0) unless `snr/snrThreshold ≥
brightThreshold`. Output sorted by flux descending.

Our detector (`detect_fast` with Moffat centroid refinement, ~0.05 px) already
covers the centroiding; the structure-map front end above is the option list
for the "detection" panel, not a replacement.

### 5.2 Matching [doc, log]

- **Triangle similarity** (Valdes 1995; Tabur 2007): triangles for the 200
  brightest stars plus `n` nearest-neighbour triangles per fainter star,
  matched by side-ratio invariants (translation/rotation/scale/mirror
  invariant).
- **Polygon descriptors** (vendor default): pentagons (`polygonSides 5`), the
  two most distant stars define a local frame, the other `N−2` stars hash to
  `2(N−2)` local coordinates; `descriptorsPerStar 20`; invariant to similarity
  and robust to distortion but **cannot handle mirroring**.
- Our seed is the existing scale-invariant **quad** matcher (distance ratios,
  mirror-invariant), which is why flipped frames register today.
- **RANSAC** on the model (4 pairs → homography by normalized DLT), inlier
  tolerance 1.9 px [log] (doc default 2 px), up to 2000 iterations [log],
  early exit above 98 % inliers, adaptive count `N = log(1 − p)/log(1 − w⁴)`
  with `p = 0.9999`. Four optimization criteria weighted 1.0 each: inliers,
  overlapping (convex-hull area covered by matches), regularity (uniform 2-D
  distribution of matches), RMS error; a combined quality score picks the
  model. Reported per frame [log]: inliers, overlapping, regularity, quality,
  `delta_RMS`, `sigma_RMS`, peak errors per axis, translation, rotation,
  scale, the 3×3 matrix.
- Both reference and target are limited to the **2000 brightest** stars [log].

### 5.3 Transformation models [doc, code]

- **Homography** (default): normalized DLT (Hartley 1997): normalize points
  (centroid to origin, mean distance √2), two rows per correspondence in a
  `2n×9` system, SVD null vector, denormalize. Rigid/similarity/affine are
  restricted variants.
- **Thin-plate spline** distortion: RBF `φ(r) = r² ln r` (order 2), separate
  splines for the X and Y displacements over a homography, smoothing
  `λ > 0` = approximating (regularized) spline, node cap 4000 [log], nodes
  pruned by shape-preserving surface simplification, evaluation discretized on
  an 8 px grid. Domain decomposition ("DDM") is the O(n²) solver for large
  node counts.
- **Local distortion correction** loop: fit the linear model → apply the
  current spline to all stars → regenerate pairs → RANSAC with increasing
  tolerance → corrector homography `H_c`; stop when `‖H_c − I‖ < T`.
- Polynomial (SIP-style) distortion is our cheaper intermediate: forward and
  inverse polynomials of order 2–4 fitted independently on the residuals of
  the linear model (the convention the plate solver already uses).

### 5.4 Pixel interpolation [code, doc]

- Nearest; bilinear; **bicubic spline** (Keys cubic convolution, 4×4) with
  **linear clamping**: for each row/column of the 4-tap kernel compute `f12`
  (inner taps) and `f03` (outer taps, negative lobes); if `−f03 ≥ f12 · c`
  (`c = clampingThreshold = 0.3`) replace the cubic by the inner-tap linear
  estimate; **bicubic B-spline**; cubic filters (Mitchell–Netravali
  `B = C = 1/3`, Catmull–Rom `B = 0, C = 0.5`, cubic B-spline `B = 1, C = 0`);
  **Lanczos-n** (3, 4, 5) with clamping: accumulate positive contributions
  `s⁺` and |negative| `s⁻`; `r = s⁻/s⁺`; if `r ≥ 1` return `s⁺/w⁺`; if `r > c`
  attenuate the negative part by `1 − ((r − c)/(1 − c))²`; result
  `(s⁺ − s⁻)/(w⁺ − w⁻)`.
- Vendor "Auto": cubic B-spline filter below scale 0.25, Mitchell–Netravali
  for 0.25–0.6, Lanczos-4 otherwise. WBPP 3.0.1 sets bicubic B-spline
  explicitly [log] — our default, for parity and for its noise behaviour.
- Alignment origin 0.5, 0.5 — pixel-centre convention [code].

### 5.5 Drizzle sidecar contents [code]

Source image, CFA source (pattern, channel), reference geometry, alignment
origin, alignment matrix (reference → target) and its inverse, alignment
splines X/Y and inverses (RBF name, order, smoothing, nodes, coefficients,
weights), pedestal, location estimates, reference location, scale factors,
weights, rejection map (uint8 per channel: bit 0x01 high, 0x02 low, 0x10/0x20
large-scale), adaptive-normalization vectors. Our sidecars carry the same
information in our own binary layout.

---

## 6. Drizzle

Reference: Fruchter & Hook 2002 (PASP 114, 144) [paper]; the vendor module
[code].

### 6.1 Defaults [code, log]

scale 2 (1–10), dropShrink 0.90, kernel Square (Circular, Gaussian,
Variable-shape), kernelGridSize 16, origin 0.5/0.5, CFA off, rejection on,
image weighting on, surface splines on, local distortion on, local
normalization on. Constants: bounds tolerance 1e-5, drop tolerance 1e-8,
kernel epsilon 0.025.

### 6.2 The equations [paper]

For input pixel `(x_i, y_i)` with value `d`, weight `w` and overlap area `a`
with output pixel `(x_o, y_o)`:

```
W′ = a·w + W
I′ = ( d·a·w·s² + I·W ) / W′
```

so finally `W_o = Σ a·w`, `I_o = Σ d·a·w·s² / W_o` over every input pixel of
every frame; `s` = output/input pixel scale, `p` = pixfrac (drop size in
input pixels). Interlacing is the limit `p → 0`, shift-and-add is `p = 1`.
Surface brightness is conserved through `s²`. Noise correlation ratio
`R = σ_c²/σ_p² = (1 − r/3)⁻²` for `r = p/s ≥ 1` (p = 0.6, s = 0.5 gives 1.66).

### 6.3 Inverse-mapping formulation [code]

Output pixel `(x_o, y_o)` covers reference rectangle
`[x_o·p, (x_o+1)·p] × [y_o·p, (y_o+1)·p]` with `p = 1/scale`. Map its four
corners to input coordinates with the homography (or splines, discretized on
an 8 px grid) after subtracting the alignment origin → a convex quad in input
space. Test every input pixel whose drop can intersect the quad's bounding
box. Drop of input pixel `(x, y)` = square `[x + δ0, x + δ1]`,
`δ0 = max(1e-8, (1 − dropShrink)/2)`, `δ1 = 1 − δ0`. Overlap `a` = exact
polygon clipping of the drop square (or circle of radius `dropShrink/2`) with
the quad (shoelace area; circular segments `R²(θ − sin θ)/2` for the circle).
Gaussian/variable kernels replace the area by the double integral of the
kernel over the intersection, tabulated on a `kernelGridSize²` micro-drop LUT
normalized to 1: Gaussian `exp(−r²/(2σ²))` with
`σ = (dropSize/2)/sqrt(−2 ln ε)`, variable `exp(−r^k/(k σ^k))`, `ε = 0.025`.

Accumulate per channel, skipping zero-valued and rejected samples:
`I_c += a · w_c · N_c(d)`, `W_c += a · w_c`, with `N_c` the normalization
(local, adaptive, or scale + zero offset from the sidecar). Rejection is
looked up at the output pixel's reference coordinate. Final
`I_c /= W_c / dropShrink²`, weight image divided by its maximum, out-of-range
handled as §3.6.

### 6.4 Bayer drizzle [code]

Source = the un-debayered calibrated mosaic; a CFA index from the pattern
string decides which plane `c` an input pixel `(x, y)` feeds
(`pattern[(y mod n)·n + (x mod n)] == "RGB"[c]`); only those samples drizzle
into plane `c`. Alignment data still come from the registration of the
debayered frame (identical geometry).

---

## 7. Papers

- **Fruchter & Hook 2002**, PASP 114, 144 (arXiv astro-ph/9808087) — drizzle,
  §6.2.
- **Rosner 1983**, Technometrics 25, 165 — generalized ESD; NIST handbook
  §1.3.5.17.3 gives `λ_i = (n − i) t_{p, n−i−1} / sqrt((n − i − 1 +
  t²)(n − i + 1))`, `p = 1 − α/(2(n − i + 1))`, number of outliers = the largest
  `i` with `R_i > λ_i` (n ≥ 25 accurate, n ≥ 15 reasonable).
- **Maples, Reichart et al. 2018**, ApJS 238, 2 (arXiv 1807.05276) — robust
  Chauvenet rejection, §3.4.
- **Wilcox 2012**, *Introduction to Robust Estimation*, §3.12.1 — biweight
  midvariance.
- **Starck & Murtagh 1998** — multiresolution support noise estimation.
- **Valdes et al. 1995**, PASP 107, 1119 (FOCAS) and **Tabur 2007**, PASA 24,
  189 — triangle matching; **Lang et al. 2010** — quad hashing (our seed).
- **Hartley 1997** — normalized DLT for the homography.
- **Bookstein 1989** — thin-plate splines.

## 8. Open items (unverified, ours to decide)

`matcherTolerance 0.05`, `splineSmoothness 0.005` and the spline outlier
parameters of the vendor's current registration; the meaning of LN's
`backgroundSamplingDelta` and `modelScalingFactor`; the exact WBPP 3.x frame
rejection threshold (0.05 observed in the log). Every such value in the spec is
labelled as our default, not a parity claim.
