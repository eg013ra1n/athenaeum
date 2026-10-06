# Star measurement, part 1: the truth lab and the scorecard

**Date:** 2026-10-06. **Branch:** `star-truth-lab` (athenaeum), `main` (athenaeum-lab).

**Amendment 2026-10-06, the plan review (owner-approved corrections).**
- The 14-bit saturation variant is 0.25 of full scale (16383/65535), not 0.5.
- `StarMetric` has no per-star saturation flag; `possibly_trailed` is frame-level (`models.rs`).
- Both were verified against the code and the arithmetic.
- The `analysis` path reads raw-domain files only: its detector bins the background in integer ADU. The lab
  writes a 16-bit sibling of every image for it (plan P-20).

**Amendment 2026-10-06, the independent review.** The review (`.superpowers/sdd/star-truth-lab-spec-review/review-fable.md`)
found 2 critical, 12 important and 12 minor points, and all of them are folded in here. The critical ones:
- selection and seeds now have a row and families (§3.2 `saturated`, §7 row 10);
- injection into registered frames is limited to the LN pair rows, and the warp gets its own synthetic family (§4,
  §3.2 `resampled`);
- the lab's own gates are restated with numbers that are reachable (§9).

**Program.** This is part 1 of three, in the order the owner approved on 2026-10-06:
1. Truth lab and scorecard (this spec). The product does not change.
2. The stacking PSF.
3. The Analysis-tab detector.

**Owner's goal.** The PSF that every stacking calculation uses must be measured correctly. That covers:
- the fit's background;
- local normalization (LN): seeds, selection, F1's common FWHM;
- weights and the FWHM and eccentricity filters;
- the master's FWHM.

Registration is good today, measured by the owner against the external reference pipeline (the ERP), and it stays as
it is.

**Notes this program argues from.** None of these is a spec:
- open items, "Measurement/detector cycle", and the LN spec §15 point 18;
- the LN Task 16b lab findings (athenaeum-lab `star-lab/findings.md`):
  - §3: ten definitional differences, each with its measured cost;
  - §2.2 and §5: the ERP's errors on truth, and the never-copy list;
- `star-lab/f1v2-findings.txt`: the F1 v2 geometry search;
- research `2026-10-04-ln-scale-psf-width-audit.md` and `2026-10-04-ln-math-review.md`;
- the 2026-09-07 eccentricity audit, on branch `feature/star-detection-fix`.

## 0. What part 1 delivers

A referee for star measurement, and the baseline numbers of today's code against it:

1. **Generator v2.** Synthetic frames with exact truth, extending the Task 16b generator (`star-lab/synth.py`) where
   it is blind today:
   - sharp and undersampled stars, saturation, OSC mosaics;
   - structured backgrounds and halos, densities;
   - field-dependent and asymmetric PSFs, master-like mixtures;
   - twins and ladders for stability;
   - doubles, trails, artefacts;
   - frames resampled through the production warp.
2. **Injection.** Synthetic stars with known parameters, written into real calibrated frames, so real noise,
   background and nebulosity are kept. Registered frames take them only for the LN pair rows.
3. **`star_probe`.** An athenaeum-core example that:
   - runs one image through the production measurement paths with the run's own options;
   - writes every seed and every measured star;
   - can warp an image through the production resampler.
4. **The scorecard.** It matches measured stars to truth and reports each path against each fundamental: bias and
   scatter separately, with the noise floor (Cramér–Rao lower bound, CRLB) beside them. The ERP's numbers on the same
   inputs are a comparison column.
5. **The baseline report** at the branch head, the record part 2's spec argues from.

**No product behaviour changes in part 1.** The athenaeum diff is the example, docs, the harness move already
committed (1bb4b277), and visibility-only changes the probe needs to reach a production entry point. For example,
`register::detect::stars_from_fits` goes from `pub(crate)` to `pub`, with a doc line saying the probe calls it. Each
such change is listed in the plan.

## 1. Doctrine (binding)

- **The truth is the referee.** It takes three forms:
  - synthetic ground truth;
  - photometric truth on real frames (the harness's growth-curve photometry, itself gated, §9);
  - injected stars on real frames.
- **The ERP is a comparison only.** It runs on the same inputs. It is never the referee and never a label, and its
  measured errors are never copied:
  - the +1 px on the measuring ellipse;
  - the unrotated ellipse;
  - a fixed β for FWHM and background;
  - the raw-maximum saturation reference.
- **Part 1 sets no bars on the product.** Part 2's spec declares its bars before any fix is measured. They are
  anchored on this baseline and on the CRLB, and are never loosened or tuned toward afterwards.
- **The CRLB is a floor, never a target** (§6).
- **No third-party program is named in athenaeum.** The ERP's automation lives only in athenaeum-lab.

## 2. Where things live

| What | Where |
| ---- | ---- |
| Generator v2, injection, matcher, CRLB, scorecard, run drivers, ERP automation | athenaeum-lab `star-lab/` (Python 3 + numpy; the ERP drivers stay under `star-lab/erp/`) |
| Grid and injection manifests (families, seeds, sizes, image hashes, source-frame hashes, numpy version) | athenaeum-lab `star-lab/manifests/` (committed) |
| Images, truth CSVs, probe outputs | `~/.athenaeum-acc/star-lab-v2/` (never committed; synthetic images regenerate from the manifests, injections regenerate while their source frames exist) |
| `star_probe` | athenaeum `crates/athenaeum-core/examples/star_probe.rs` (compiles with the product; built with `CARGO_TARGET_DIR=.superpowers/target-acc`) |
| This spec, the plan, the baseline report | athenaeum `docs/superpowers/{specs,plans,research}/` |
| Photometric truth on real data | athenaeum-lab `harness/acceptance/grid-report.py` (reused, not rebuilt) |

The lab finds athenaeum through `ATHENAEUM_REPO`, the same convention as the harness.

## 3. Generator v2

### 3.1 Rendering and truth

**Rendering.**
- Gaussian profiles are rendered with the exact erf pixel integral.
- Moffat, wing and blur profiles are integrated over each pixel by a midpoint `os × os` sub-grid inside ±3 FWHM of
  the centre, and centre-sampled beyond. `os` is 15 for FWHM < 4 px and 9 at or above.
- Measured: `os` 5 misses 1e-3 of the peak up to FWHM 3.5 (2.6e-3 at 2.5), and `os` 15 reads 8.5e-4 at FWHM 1.2.

**Truth per star.** Each star carries three FWHM columns and their eccentricity and angle (ruling S-2):
1. **The PSF before pixel integration.** This is the optical quantity: minor- and major-axis FWHM, eccentricity,
   angle, and β for Moffat families.
2. **The PSF convolved with the pixel box.**
3. **The point-sampled-fit equivalent.** This is the FWHM, eccentricity and angle that a point-sampled least-squares
   fit of the generating model returns on the noiseless pixel-integrated stamp. The lab computes it with its own
   numpy fit, never with ours. For a Gaussian it is `sqrt(FWHM² + 2.355²/12)` to first order.

**Flux truth.**
- `flux_img` is the sum of the rendered star, which is exactly what the image holds. Every flux row uses it.
- `flux` is the analytic total. Its difference from `flux_img` is the cutout's stated remainder: 1e-4 of a core and
  2e-3 of a wing.
- The truth CSV also gives:
  - `peak` (the noiseless maximum pixel above sky);
  - `peak_snr = peak / σ_sky,pixel`;
  - the Fisher SNR (§6);
  - `saturated`.

The 2026-09-07 geometry rule stays: the minor axis is the seeing and the major axis grows with eccentricity.

### 3.2 Families

Each family is a manifest entry: its parameters, a seed, and how many images. A frame is 2048 × 2048 unless stated,
float32 in [0, 1], with Poisson noise on sky and star (gain), read noise, and a saturation clip. Fluxes follow
N(>F) ∝ F^−slope, with the slope and range chosen so every SNR bin of §6 holds at least 50 stars per family.

| Family | Parameters | Serves |
| ---- | ---- | ---- |
| `sharp` | FWHM {1.2, 1.6, 2.0, 2.6, 3.5, 5.0, 8.0} × profile {Gaussian, Moffat β 2.5, 4, 10, blur (a Moffat 4 core convolved with a Gaussian seeing of 1× and 2.5× the core's FWHM)}; 150 stars/Mpx at a 25 px minimum separation | detection, FWHM and β over the whole width range; the 2026-09-07 F1/F2 (FWHM < 3) |
| `shape` | e {0, 0.3, 0.5, 0.7} × θ {0, 30, 60, 90}° at FWHM 2.6 and 4.0, Moffat 4; density as `sharp` | eccentricity and angle by SNR, the round-star floor |
| `coma` | a Moffat 4 core plus a second Moffat 4 with 30 % of the flux, displaced radially by 0.5 × FWHM, growing to 1.0 × FWHM at the corners; FWHM 3.0. Truth: the continuous profile's second-moment ellipse AND its half-maximum contour ellipse | which ellipse each path tracks on a star that is not an ellipse; angle error against each (audit F7) |
| `fieldpsf` | FWHM × 1.3 at the corners, tangential e up to 0.3, Moffat 4; density as `sharp` | frame aggregates and LN's common F on a varying PSF |
| `density` | 5, 40, 200, 600, 1000 stars/Mpx at FWHM 3.0, Moffat 4 | crowding: fit background, F1's annulus, completeness; 5–40 brackets the real fields |
| `background` | flat; linear gradient 10 % of sky across the frame; vignetting 20 % at the corners; fractal nebula (spectral slope −3) at 0.1, 0.5 and 3 × sky amplitude. One nebula image at 4096² for the `analysis` path, whose background mesh scales with the frame size | background under the star; seed centroid drag; false detections on nebula |
| `halo` | one star of 2000 × the saturation flux with a Moffat β 2 halo to 250 px (the Horse C11 case) over the `sharp` FWHM 4 field | fits whose region sits on another star's wings: background, region growth, F1's annulus |
| `saturated` | the `sharp` FWHM 2.6 and 4.0 fields with the flux tail set so that {0, 5, 20, 60} of the 64 brightest stars clip; plus a variant clipped at 0.25 of full scale (a 14-bit ADC in a 16-bit container: 16383/65535) | Auto β picked on the 64 brightest seeds; σ0 from clipped peaks; the relative-saturation test; `peak5` |
| `mixture` | equal-weight mixtures of the `sharp` profile at FWHM {2.6, 3.4, 4.5} and {3.0, 5.0, 7.0} (the set 204 spread); a clipped-core variant (the core above the mixture's 70th percentile replaced by it: a rejected master's profile) | the master FWHM on a master-like PSF; where the residual-vs-β curve is flat |
| `twins` | every `sharp` and `mixture` field rendered twice with different noise seeds, and once at 2 × the flux (a deeper master); plus a `ladder` of eight frames of one field at FWHM 2.0 … 6.0, the profile drifting from Moffat 2.5 to blur, as a night does | stability: Auto β agreement, frame FWHM and eccentricity between twins, monotonicity along the ladder (the β flip) |
| `resampled` | the `sharp` FWHM 2.0 and 3.0 fields, noise rendered before the warp, then warped by a half-pixel shift and a 0.3° rotation through the production resampler (`star_probe warp`, §5). Truth: the unwarped PSF, with the kernel's variance (+1/3 px² per axis at zero phase) stated beside it | what Measure, LN and the master measure see on a registered frame: FWHM, captured fraction and fitted background under correlated noise (audit H6: about 2.6 % on a sharp star) |
| `doubles` | pairs at separations {2, 3, 5, 7, 10} px, flux ratios 1:1 and 1:4, FWHM 2.6 and 4.0 | blend handling; the audit's contamination finding |
| `trails` | straight tracks 3–15 px, one curved track per star, and a two-head track with heads 7 px apart and a bridge at 20 % of the heads; minor axis at the seeing | trail truth for part 3 (the 2026-09-07 F4) |
| `artefacts` | hot pixels (40/Mpx, single and 2-px) and cosmic-ray hits (worm-shaped, 1–6 px) over the `sharp` FWHM 2.6 field; plus one hot pixel 2–4 px from the centre of 5 % of the stars | false detections; `peak5`, the moment ellipse and the region median with a hot pixel inside the fit region |
| `osc` | an RGGB mosaic of three channel renders (§3.3) at FWHM {2.0, 2.6, 3.5, 5.0} × profile {Moffat 4, blur} | per-plane flux and FWHM; the blue-plane −0.6 %. R and B are sampled at a 2 px pitch, so below FWHM 2.5 they are undersampled after VNG. |
| `pairs` | the Task 16b `iso` and `main` pair grids with known `s_true`; plus OSC pairs and `density` pairs at 200 and 600 stars/Mpx | LN relative scale, F1's slope, per plane |
| `truthcheck` | 300 stars on 2048² at a 120 px minimum separation, Moffat 4; targets at FWHM {3.0, 4.5, 6.0, 7.5} with known `s_true` against a FWHM 3.0 reference | the gate on the harness photometry's normal mode, up to FWHM 7.5 |
| `truthcrowd` | the `density` 600/Mpx field as pairs | the gate on the harness photometry's crowded fallback |

### 3.3 OSC

- **Channels.** Each one is rendered at full resolution with its own PSF width (R 1.00, G 0.95, B 1.10 × the
  family's FWHM: chromatic seeing). Its own sky level and star colours come from a spread of colour indices.
- **Mosaic.** The RGGB mosaic samples each channel at its CFA pixels, and noise is applied to the mosaic.
- **Paths.** The stacking paths take the planes from the production debayer
  (`astroimage::processing::vng::vng_debayer_f32`, through `star_probe --debayer`), as the Calibrate stage does. The
  `analysis` path takes the mosaic file as the Analysis tab does (§5).
- **The ideal plane.** The full-resolution render of each channel, with its own noise, is kept beside the debayered
  plane, so a per-plane error can be split into debayer and measurement.

### 3.4 Determinism

Same manifest, same seed, same images. Each manifest entry records:
- the SHA-256 of every image it produced;
- the numpy version.

The self-check regenerates one image per family:
- on the same machine, the hashes must match;
- across numpy versions, a regenerated image passes if `max |Δ| ≤ 1e-7` and its truth CSV is identical.

## 4. Injection into real frames

`star-lab/inject.py <frame.fits> --family <spec> --out <dir>` writes:
- the frame with injected stars;
- a truth CSV;
- a mask of the injection stamps.

**Where a star may be injected** (ruling S-10).
- **Calibrated frames serve every row.** Set 204 mono and OSC, Horse C11 L and G, run B's prepared frames, and the
  sharp frames of the star-test corpus with FWHM ≤ 2.6 px (singles: LDN 1272 B, B150, 2025-08-23 B, Gamma Scuti;
  m42 30 s).
- **Registered frames and the LN reference serve only the pair rows.**
  - The same star list goes into the member and the reference, with target flux = reference flux / `s_inj`.
  - Only the LN scale is read from them: F1 and PSF signal, through `ln_scale_probe`'s per-star pair dump, over the
    injected stars only.
  - The truth CSV records `psf_truth = "unwarped"`. The scorecard refuses to form any FWHM, eccentricity, background
    or CRLB row from it, because a registered frame's stars and noise are smoothed by the warp kernel.
  - **Optional mode:** inject into the calibrated frame, then warp it through the frame's stored registration map with
    the production resampler (`star_probe warp --map`). Every row is then valid, with truth = PSF ⊗ kernel at the
    local phase. The baseline report records whether this mode was done.

**Placement.** On a jittered grid, with these distances:
- at least `12 × FWHM_inj` from any real star (the harness's detection on the frame itself, dilated by
  `6 × FWHM_frame`) and from each other;
- no star on a pixel above 0.5 × the frame maximum;
- at least `12 × FWHM_inj` from any non-finite pixel.

A dedicated **edge set** sits at {1, 2, 3} F from the coverage edge or the frame border, and is reported as its own
column.

**Stars.** The parameters follow the `sharp` and `shape` families, at FWHMs near the frame's own (0.8×, 1.0×,
1.3×), across the SNR ladder.

**Noise and gain.**
- A star adds its own Poisson noise.
- **Gain per unit** of the calibrated [0, 1] plane is `EGAIN × 65535 / flat_norm`. It is multiplied by the frame's
  master flat at the injection pixel when the flat is reachable through the calibration link; otherwise by 1, with the
  omission recorded.
- **Without `EGAIN`,** the gain is estimated from the sky variance of star-free 64-px tiles against their level, and
  only when those levels span at least 20 % of the sky level. Otherwise the frame is refused for injection and named in
  the report.
- **OSC injection goes into the calibrated mosaic**, never into a VNG plane: the mosaic the run keeps for Bayer
  drizzle, or the mosaic rebuilt from the raw through `light_cal` with the same masters. The production VNG follows,
  as in §3.3.

**Provenance.** The injection manifest records the source frame's SHA-256. Nothing is written back to a catalog or to
a source file.

## 5. `star_probe` (athenaeum example)

```
star_probe measure <image> [--config <stacking config JSON>] [--plane N|all|lum] [--debayer RGGB|GRBG|GBRG|BGGR]
                           [--rect x,y,w,h] [--paths measure,betas,register,analysis] [--betas 2.5,4,6,10]
                           --out <file.jsonl>
star_probe warp <image> (--shift dx,dy --rotate deg | --map <stored registration map>) --out <image>
```

It measures through the production entry points, never a re-implementation (ruling S-3):

| Path | Entry point | What it feeds today |
| ---- | ---- | ---- |
| `measure` | `stacking::measure::measure_frame_with_fits`, with `MeasureOptions` built by `MeasurementConfig::measure_options` from `--config` (the per-set document or a run's config block), as the run builds them; the defaults without `--config` | weights, the FWHM and eccentricity filters, LN seeds; each frame fits at its own Auto β |
| `betas` | `stacking::psf_signal::fit_stars_with_beta` at each listed β, on the same fast-detector seeds the `measure` path uses | the β mechanism LN uses (group β, `run.rs` ~10768) and the β sensitivity of FWHM, flux and background |
| `register` | mono: `stacking::register::detect::stars_from_fits` on the `measure` fits, as mono registration builds its list. OSC: `detect_stars` with the default `DetectionConfig` on `--plane lum` (`register::detect::luminance`) | registration's star list; LN detection seeds |
| `analysis` | `analysis::analyzer::build_analyzer` with the app's `AnalysisConfig::default()`, then `analyze_frame` on the file, unconverted, as the Analysis tab does: a mosaic with `BAYERPAT` is green-interpolated, a three-plane file is luminance-combined; `--plane` and `--debayer` do not apply | the Analysis tab, the plate-solve input gate, the collab gate |

**Inputs.** A 16-bit or ADU-domain image is converted to [0, 1] the way the run's Prepare converts an attested
light (the `BZERO`/`BSCALE`-aware path), and the frame line records the conversion. `--rect` measures a
sub-rectangle the way the master measure does on a covered rectangle (`integrate.rs` ~1553).

**Output, one JSON line each, camelCase:**
- **Seed lines** (`measure` path). One per seed, in the detector's order, as `detect_fast_data` returned them:
  `x`, `y`, `peak`, `flux`, `hfd`, `snr`.
- **Star lines.** One per measured star per path (and per β):
  - `path`, `plane`, `beta`;
  - `x`, `y`;
  - `background`, `amplitude`, `signal`, `peak5`;
  - `fwhmX`, `fwhmY`, `fwhm`, `ecc`, `thetaDeg`;
  - `residual`, `snr`.

  The `analysis` path adds its `fitMethod`. `StarMetric` carries no per-star saturation flag, and
  `possiblyTrailed`/`trailRSquared` are frame-level (`FrameAnalysis`), so they are on the frame line.
- **Frame lines.** One per path, with that path's frame aggregates:
  - its reported FWHM and eccentricity, and the statistic used (the `measure` path's residual-weighted mean over all
    fits, or the `analysis` path's clipped weighted median over e ≤ 0.8);
  - the seed and fit counts;
  - the options used (`psfModel`, `detectionSigma`, `maxStars`, `seedPrefilter`, `seedDetector`, `scaleEstimator`);
  - wall time.

A field a path does not produce is `null`.

**Refusals are attributed by truth** (ruling S-11). Production returns only accepted fits and survivors: `fit_one`
gives no reason and `StarFit` carries no region radius or starting background. The probe therefore reports what
production reports, and the scorecard attributes the refused population through truth (the unmatched truth stars by
SNR bin, saturation, blend, edge). The fit's background setup is read through its consequence, the fitted
`background` row, by family and density. Part 2's first task adds a `FitDiagnostics` side channel (region radius,
`b0`, growth steps, convergence) before any fix is measured.

`ln_scale_probe` stays the LN measurer, unchanged: pairs, F1, selection, the per-star pair dumps.

## 6. Matching, SNR and the CRLB

**Match.**
- A truth star and a measured star match within `max(1.5 px, 0.5 × FWHM_truth)`, one-to-one, assigned greedily from
  the brightest truth star down (ruling S-4).
- In `doubles` at 2–3 px, a single blended detection matches the brighter member and the fainter counts as missed.
  That is intended: blends are reported in their own column, not as completeness.
- Every unmatched detection is classified by what lies under it in the lab: a hot pixel, a cosmic ray, nebula, the
  wing of a star, or nothing.

**SNR.** Each truth star's SNR is its Fisher (matched-filter) SNR, `sqrt(Σ μ_star² / var)` over its noiseless
rendered stamp, where var is sky plus star plus read noise. The bins are < 10, 10–20, 20–40, 40–100, 100–300,
> 300, and saturated.

**CRLB.**
- **The bound.** For each truth star, the Fisher matrix of (x, y, F, B, σ_minor, σ_major, θ) is taken under the
  generating profile with β known:
  - on a square of side `6 × FWHM_truth`, the same for every path;
  - with white noise, sky plus star plus read noise per pixel.

  The bound is `sqrt((F⁻¹)ᵢᵢ)`; FWHM and eccentricity take theirs by the delta method.
- **A floor, never a target.** The scorecard reports bias (the median of measured − truth) and scatter
  (1.4826 · MAD) separately, and `efficiency = scatter / CRLB`.
- **Model bound.** For a family whose generator is not an elliptical Moffat of the fit's own β (Gaussian, blur,
  wings, mixtures, coma), the row also shows the bound of the fit's own model at its best-fit parameters.
- **Not on registered-frame injections** (§4): no CRLB column is formed there.

## 7. The scorecard

`star-lab/scorecard.py <manifest> <probe-outputs…>` writes a Markdown table and a JSON. Each row is (path ×
fundamental × family × SNR bin), with three columns: ours, the ERP (where it measures the same thing), and the CRLB
or the ideal. The rows:

1. **Detection:**
   - completeness against each path's own declared threshold, as `measured / expected`, with the raw fraction beside
     it. The expected population is the truth stars above that threshold: true peak ≥ k σ for `measure` (its
     `detectionSigma`), Fisher SNR ≥ 10 for `register`, a matched-filter response ≥ 5 σ for `analysis`. A shortfall
     against the path's own threshold is a defect; a shortfall against 100 % is the threshold;
   - false detections per Mpx, by cause (§6);
   - blends;
   - bright coverage: the share of the 300 brightest unsaturated truth stars that the path measures (the 2026-09-07
     audit's metric).
2. **Centroid:** bias (x, y), scatter, and efficiency.
3. **FWHM:**
   - bias and scatter against each of the three truth columns (§3.1), by bin;
   - β sensitivity: the spread of each star's FWHM across the `betas` fits, with the β-free second-moment FWHM of
     the same stars beside it;
   - the Auto β pick distribution per family.
4. **Eccentricity and angle:**
   - eccentricity bias by bin at each truth e;
   - the round-star floor (the median measured e at truth e = 0);
   - angle error (circular rms) at e ≥ 0.3;
   - on `coma`, the error against both truth ellipses.
5. **Background under the star:** (fitted background − true local sky) / σ_sky,pixel, by family and density,
   including `halo` and `resampled`.
6. **Flux:**
   - the captured fraction, `signal / flux_img`, by FWHM and profile; and the fraction inside 2F and 5F circles
     (F1's geometry) on the same stars;
   - on `pairs`, the relative scale against `s_true` with its slope in %/px² of FWHM² difference
     (`ln_scale_probe`, per plane).
7. **Frame aggregates:** each path's reported frame FWHM and eccentricity, with its statistic named (§5), against the
   truth population's median and against its residual-weighted mean, over the stars the path measured and over all
   truth stars.
8. **OSC per plane:** flux ratio and FWHM against truth, on the ideal and on the debayered plane.
9. **Cost:** wall time per path per image, and peak RSS from `/usr/bin/time -l` with one path per invocation.
10. **Selection and seeds:**
    - the Auto β pick against the number of saturated stars among the 64 brightest seeds (`saturated`);
    - σ0 (recomputed from the dumped seeds with the production `initial_sigma` formula, labelled a diagnostic)
      against the truth σ, by the same count;
    - seed centroid error against truth on `background` and `halo`, by local gradient;
    - the Spearman rank of seed flux against truth flux, with and without saturated stars;
    - `peak5 − true peak` by FWHM and SNR, and the share of truth stars with `peak_snr ≥ 40` that the LN selection
      keeps;
    - the relative-saturation verdict against the truth `saturated` flag, on fields whose plane maximum is set by a
      hot pixel, by one bright star, and by the clip.
11. **Stability:**
    - the share of twin pairs whose Auto β agrees;
    - the median and p90 of |ΔFWHM| / FWHM and of |Δe| between twins, per path;
    - along the `ladder`, the Spearman rank of frame FWHM against truth and the largest inversion, with Auto β and
      with a fixed β.

## 8. Real-data rows

| Row | Data | Truth |
| ---- | ---- | ---- |
| Injected-star rows 1–7, 10 (as §7) | the calibrated frames of §4 | injection truth |
| LN scale per member | set 204 mono and OSC, run B, Horse C11 G | two truths: (1) the harness photometric truth (`grid-report.py`) on the run's own LN reference, with its mode named and, where the crowded fallback ran, the `truthcrowd` gap as its error bar; (2) the injected pairs (`s_inj`) |
| Frame FWHM and eccentricity | the star-test corpus (m42, m101, singles) | none absolute: the injected rows set the referee. The ERP's per-frame and per-star tables already beside the frames are a comparison column. |

## 9. Part 1's own gates (the lab is right before it judges)

| Gate | Bar |
| ---- | ---- |
| Pixel integration | Gaussian at FWHM {1.2, 2.0, 2.6, 3.5}: every pixel within 1e-3 of the peak of the exact erf integral. Moffat β 2.5 at FWHM 2.6 against an `os` 45 render: within 1e-3. `flux_img` equals the rendered sum exactly, and `flux − flux_img` is within the cutout's stated remainder (1e-4 core, 2e-3 wing). |
| Mosaic | every CFA pixel equals its channel render at that pixel |
| Injection | outside the stamps bit-identical to the source; inside it source + recorded star + recorded noise |
| Gain | on every injection frame, the sky variance of star-free 64-px tiles against their level reproduces the recorded gain and read noise within 10 % |
| Determinism | regenerated hashes equal the manifest's; across numpy versions, `max |Δ| ≤ 1e-7` with identical truth |
| CRLB | for a Gaussian of FWHM 10 px, flux 1e5 e⁻, zero sky and read noise, the centroid bound is within 0.5 % of σ/√N (the pixel-integrated bound is σ/√N × 1.0024 there). At FWHM 3 it is within 0.5 % of an independent one-dimensional erf-derivative Fisher sum. |
| Matcher | a field measured at the truth positions matches 100 % with zero false detections |
| Photometric truth, normal mode | on `truthcheck`, within ±0.3 % mean and ±0.1 %/px² slope of `s_true` up to FWHM 7.5, with at least 30 stars per target |
| Photometric truth, crowded fallback | on `truthcrowd`, within ±0.8 % mean (its documented systematic). The measured gap becomes the error bar of every real-data row that used that mode. |
| ERP emulation still faithful | `erplib.py` reproduces the ERP's own LN scale within 0.1 % on the `pairs` targets |
| `star_probe` builds | `cargo check -p athenaeum-core --examples` |
| Probe parity with a run | on one calibrated frame of set 204, `star_probe measure --config <that run's config>` writes fits equal, field for field, to the run's persisted `fits` artifact for that plane (decoded with `fits_artifact.rs`; `peak5` is not persisted), and its `ChannelMeasurement` equals the run's `metrics` row |
| Seed parity | the seed lines of a `sharp` FWHM 3.5 field equal the star list of `ImageAnalyzer::detect_fast_data` on the same image, star for star |
| Warp parity (only when the optional pre-warp mode of §4 is done) | `star_probe warp --map` on one set 204 frame reproduces that run's persisted registered frame bit for bit |

A gate that fails stops part 1, and the cause goes to the owner. The generator is fixed. The bar is not.

## 10. The baseline report

`docs/superpowers/research/<date>-star-measurement-baseline.md` in athenaeum, dated the day it is written:
- the scorecard for every family and every path at the branch head;
- the injected and real-data rows;
- the ERP column;
- whether the optional pre-warp injection mode was done.

It ends with the ranked list of what part 2 must fix, each item with its numbers. Part 2's spec sets its bars from
this report before any fix is measured.

## 11. Machine budget

16 GB RAM, swap nearly full:
- one heavy job at a time;
- `memory_pressure` checked before a batch;
- the ERP runs one instance at a time through `star-lab/erp/run_job.sh`, after the owner has closed any interactive
  instance.

A 2048² float32 frame is 16 MB, a 4096² one 64 MB. A family's images are generated and measured one at a time. Disk:
at most 10 GB under `~/.athenaeum-acc/star-lab-v2/`. `star_probe` is built once into `.superpowers/target-acc` with
`CARGO_BUILD_JOBS=4`.

## 12. Out of scope

- Any product change. Fixes are parts 2 and 3. Part 2's first task is the `FitDiagnostics` side channel (S-11).
- Registration (owner: it is good). Part 2 adds a registration-parity gate, because mono registration takes its stars
  from the Measure fits.
- The rustafits characterisation tests of the 2026-09-07 audit. Part 3 recreates them; the scorecard's `analysis`
  rows replace the lost `ecc_probe` and `ecc_floor` numbers as the baseline.
- A neural classifier for star / artefact / blend / trail. It is considered only if the scorecard shows the rules
  failing on false detections or blends, as an experiment with numbers.
- Moving existing acceptance records.

## 13. Rulings

- **S-1** Lab code, manifests and ERP automation live in athenaeum-lab. `star_probe` lives in athenaeum examples.
  The spec, plan and baseline report live in athenaeum docs (owner, 2026-10-06).
- **S-2** Each truth star carries three widths:
  1. the PSF before pixel integration;
  2. PSF ⊗ pixel;
  3. the point-sampled-fit equivalent.

  Each consumer has its own referee, and part 2's bars name it:
  - **Weights.** The default `PsfSignalWeight` reads the aperture `signal`, so its referee is total flux: the
    captured fraction `signal / flux_img` must not depend on the profile (row 6). The `Formula` mode reads FWHM and
    eccentricity normalized to the group range, so only cross-frame consistency matters (row 11). A constant offset
    is harmless; a β-dependent one is not.
  - **The FWHM and eccentricity filters** are absolute pixel thresholds the user types. Their referee is the owner's
    ruling between the three defined widths, asked in part 2 with numbers.
  - **F1's common F** is a radius. Its referee is that the 2F and 5F circles enclose a profile-independent fraction
    on both frames (row 6, the 2F/5F fractions), never a FWHM column.
  - **The master FWHM** needs an absolute number (the owner's ruling, as for the filters) and stability between
    near-identical images (row 11).
  - **Mono registration and the LN seeds** are positions. Their referee is the centroid CRLB and the pairing radius.
- **S-3** The probe calls production entry points only, with the run's own options (`--config`). A path measured
  through a re-implementation is not a baseline.
- **S-4** The match radius is `max(1.5 px, 0.5 × FWHM)`, one-to-one and greedy by truth brightness. Blends are their
  own column. The SNR is the Fisher SNR. The bins are as in §6.
- **S-5** Part 1 sets no product bars. Part 2 sets them before measuring a fix, anchored on this baseline and the
  CRLB.
- **S-6** Images are never committed. Manifests carry seeds, image hashes, source-frame hashes and the numpy version.
- **S-7** Injected stars carry Poisson noise at the frame's gain per unit, with the flat-field factor where the flat
  is reachable. A frame without a trustworthy gain is refused. The placement keeps `12 × FWHM` from real stars and
  from non-finite pixels, and the edge set is reported separately.
- **S-8** OSC goes through the production VNG for the stacking paths and through the Analysis tab's own handling of a
  mosaic for the `analysis` path. The ideal plane is kept beside it. OSC injection goes into the mosaic.
- **S-9** The ERP runs on the same inputs, as a comparison column. It is never a label, and its errors are never
  copied.
- **S-10** Registered frames and the LN reference take injected stars only for the pair rows (`psf_truth =
  "unwarped"`). The warp's own effect is measured on the synthetic `resampled` family, with exact truth.
- **S-11** Refusals are attributed through truth, because production exposes no refusal reason. The fit's
  background setup is read through the fitted background. `FitDiagnostics` is part 2's first task.
