# One star-measurement path — design

Status: draft for the owner's review, 2026-10-07. Program: star measurement (part 1 was the truth lab; this spec
replaces the former parts 2 and 3). Baseline: `docs/superpowers/research/2026-10-07-star-measurement-baseline.md`
(lab athenaeum-lab e65af49, probes from f5321979).

## 1. Goal and owner decisions

Every star number the app shows or uses — the Analysis tab, Blink, the plate-solve input gate, the stacking weights,
LN, the master FWHM — comes from ONE measurement path, and that path is correct against synthetic truth and fast.

Owner decisions (2026-10-07):

- **D-1 One path.** The stacking measure path is the base ("Да согласен, и надо сделать этот путь хорошим"). It
  is fixed, extracted into its own module, and the Analysis tab is switched onto it. The rustafits full analysis
  is no longer called by the app.
- **D-2 FWHM is the fit width.** The "correct" FWHM for every consumer is the width a model fit to the pixel values
  gives — the lab's `pointfit` truth column. Prepixel (optical) and pixbox widths stay lab columns only.
- **D-3 Analysis star budget.** The Analysis tab and Blink measure at most 500 good unsaturated stars spread over
  the field, configurable in Settings → Analysis. Stacking keeps measuring every star.

Why the stacking path (baseline, medians over 137 synthetic families, measure vs analysis): per-star FWHM bias
0.10 % vs 0.69 %, scatter 0.56 % vs 0.87 %; frame FWHM error 0.54 % vs 0.64 %; frame ecc error 0.084 vs 0.096;
completeness 99.7 % vs 33 % of expected stars; bright-star coverage 99 % vs 38 %. Per star both cost about 0.5 ms;
the analysis path is 2–9× faster per frame only through its 500-star cap.

## 2. Architecture

### 2.1 The module

A new module `crates/athenaeum-core/src/star_measure/`, compiled under `feature = "render"` (as `analysis` is;
`stacking` needs `render + solver`, so the Analysis tab must not depend on `stacking`). It owns everything from a
plane of pixels to star and frame numbers:

| Unit | Responsibility |
| ---- | ---- |
| `domain` | input planes: calibrated float (unit domain) or raw ADU (u16/i16 with BZERO), CFA layout; the plane's noise (MRS) and background model |
| `seeds` | star detection; threshold in units of the plane's own measured noise; selection by integrated signal |
| `fit` | per-star region, background start, width start, the Moffat fit (rustafits `fit_moffat_2d_fixed_beta` and `fit_gaussian_2d` as library functions), canonical axes, acceptance, the per-star refusal reason |
| `beta` | the β choice on unsaturated stars |
| `photometry` | the profile-independent star flux, the saturation test, the per-star SNR |
| `frame` | frame aggregates: FWHM, ecc, SNR, the weight inputs, HFR, the trail metric |
| `budget` | the Analysis-mode selection: at most N stars spread over the field |
| `diagnostics` | `FitDiagnostics`: per-star outcome and refusal reason, region radius, background start, growth steps, iterations |

The module's public surface: `measure_plane(plane, opts) -> PlaneMeasurement` (stars + frame aggregates +
diagnostics) and the per-star refit entry point LN needs (`fit_stars_at(plane, seeds, beta, opts)`). One saturation
test, one SNR definition, one frame statistic — every consumer calls these, none re-implements them.

### 2.2 Consumers

- **Stacking.** `stacking/measure.rs` becomes a thin adapter that calls the module on every star and keeps today's
  artifacts (`.athf` per plane, the `metrics` payload `FrameMeasurement`, `ChannelMeasurement`). `psf_signal.rs`
  keeps the weight formulas (PSFSW, PSFSNR) and LN-specific pieces and takes its fits, flux and saturation test from
  the module. LN (`ln/scale.rs`) refits through `fit_stars_at`. The master statistics (`integrate.rs`
  `measure_plane` on the master) and mono registration (`register/frame.rs` reading the `.athf`) are unchanged
  consumers of the same numbers.
- **Analysis and Blink.** `analysis/analyzer.rs` (`map_result` is today's single writer of `FrameAnalysis` and
  `StarMetric`) calls the module in Analysis mode (§4). Commands, events, the batch job and the cache keep their
  shape.
- **Not touched.** The fast detector used by plate solving (solvemyastro) and by the registration detector
  (`stacking/register/detect.rs`). The rustafits analysis code stays in rustafits, unused by the app.
- **The lab.** `star_probe` runs the module in both modes (stacking: all stars; Analysis: the budget) and keeps the
  rustafits analysis as a `legacy-analysis` arm, so the scorecard shows before and after side by side.

## 3. What is fixed, and the bars

Every bar is fixed here, before anything is measured, and judged by the truth lab against synthetic truth
(athenaeum-lab `star-lab/`). The ERP stays a comparison column, never the referee. A missed bar goes to the owner with
its numbers; a bar is never moved, a row never re-sized, a generator never tuned toward a bar.

| # | Defect (baseline number) | Change | Bar |
| ---- | ---- | ---- | ---- |
| F-0 | No refusal reasons (`sharp/m4/f8.0`: 82.7 % fitted of 92.0 % seeded, no reason) | `FitDiagnostics` per seed; the `.athf` reserved flags byte carries the outcome code | 100 % of seeds have an outcome (accepted, or refused with a reason); numbers bit-identical to the extraction step |
| F-1 | Broad / defocused stars collapse (M51 Hα 6.53 px vs analysis 13.39, ERP 14.02; m82 11.43 vs 16.31 / 17.76; `_DSC5767` zero seeds) | region growth judged on (median − sky), not the absolute median; background start from the region's rim, not the plain median of a star-filled square; width start from the star's own moments, not one field σ0; detection by integrated signal (a matched filter at the field width, threshold from the measured noise of the filtered plane) instead of the peak pixel; new lab families FWHM 12 and 16 px and defocused annular ("donut") stars | fitted / seeded at FWHM 8: 82.7 % → ≥ 99 %; frame FWHM error vs `pointfit` ≤ 1 % at every FWHM up to 16 px; a donut star is refused with its reason, never reported as a narrow fit |
| F-2 | Angle: about 15 % of stars carry θ on the minor axis (90° off); otherwise about 7° error vs ERP 1.6° rms | canonical axes (fwhm_x ≥ fwhm_y, θ on the major axis); the fitter sees stamp-centred coordinates and amplitude-normalised values so its relative convergence test acts on O(1) parameters (no rustafits change unless the lab shows it is needed) | zero stars 90° off; angle scatter ≤ 1.5 × the angle CRLB on the `shape` families with e ≥ 0.3 |
| F-3 | β and frame FWHM move with the saturated set (2× flux twins: 9.99 %, 15.93 % with a β flip; 2.93 % with β held) | the β sample, the width start and the frame statistic use unsaturated stars only (one saturation test, §2.1) | base vs 2× twins: frame FWHM change ≤ 1 %, zero β flips |
| F-4 | Model-dependent width (Gaussian / blur pointfit −2.07…−3.04 %; β alone 9.60 %; `mixture/b` +4.88 %) | the β candidates extend to the Gaussian limit ({2.5, 4, 6, 10, Gaussian}), chosen on the unsaturated sample | frame FWHM error vs `pointfit` ≤ 1 % at FWHM ≥ 2 px, ≤ 3 % below, on every profile family |
| F-5 | Captured fraction depends on the profile (spread 18.73 % at F 2.6, 22.16 % at F 8.0) → PSFSW / PSFSNR / psfSel | star flux = fitted-model flux in the FWTM ellipse × a per-frame aperture correction from the growth curve (to 5F) of bright unsaturated isolated stars | spread across profile families ≤ 2 % at F 2.6 and F 8.0 |
| F-6 | No per-star SNR (stacking); the analysis SNR assumes gain = 1 e⁻/ADU (code reading) | one definition: S / σ_S in an aperture of radius max(1.5·FWHM, 3 px), σ_S² = S/g + N_ap·σ_sky²·(1 + N_ap/N_sky), σ_sky measured in the annulus; g from EGAIN (raw ADU) or EGAIN with the calibrated-frame cards (`ATH_CSCL`, `ATH_CFNM`); unknown gain → the shot term is omitted and the record says so. New lab row: measured vs the truth SNR of the same aperture | median bias vs the truth SNR ≤ 3 % where the gain is known |
| F-7 | Fit background under nebulosity +0.998σ (`background/neb3_4096`) | F-1's rim background | ≤ 0.3σ under nebulosity; within ±0.1σ on flat fields |
| F-8 | The F1 contradiction (synthetic over-read up to +11.84 %; real data within the bars; psfSel the opposite way) | Plan B (§5): test the referee pair first — lab field realism against the harness truth on real frames; F1 and the F1 v2 sky change only after | decided by that test; no F1 change before it |
| F-9 | The c = 1 growth floor (open owner question) | decided in Plan B on that plan's numbers | — |

No-regression bars (every fix step): every other scorecard row stays within its own standard error of the baseline;
the stacking Measure stage costs at most +20 % wall time on the acceptance sets; the Analysis mode costs at most 0.6 s
per 26 Mpx frame (today 0.26–0.56 s).

Out of scope (reported, not fixed here): the VNG per-plane width change (G +12.63 %, B −6.71 %) — a property of the
debayered planes the stack integrates, not a measurement error; the resampling broadening (FWHM² +2.596 px² vs
1.848 nominal) — the resampler's own question.

## 4. The Analysis switch-over

### 4.1 Input

The Analysis tab measures raw lights (uncalibrated ADU, pedestal, hot pixels, no flat; OSC as a Bayer mosaic).

- The frame is read through today's reader (FITS and XISF), which also closes the probe's XISF gap. BITPIX 16 with
  BZERO 0 is checked against the file `_DSC5767.fit` (the measure path's `PlaneReader` decodes it as signed, the
  rustafits reader as unsigned; one rule, pinned by a test).
- The pedestal and vignetting are absorbed by the local background (F-1, F-7).
- A hot pixel is rejected by the detector (neighbours above the threshold) and, if seeded, refused by the fit with
  the reason "narrower than the pixel".
- **OSC.** Detection runs on a 2×2 super-pixel plane; the fit uses only the green CFA sites at their true native
  coordinates (the fitter takes a pixel list, so a checkerboard sample needs no interpolation); FWHM is in native
  pixels. Stacking keeps measuring the debayered planes it integrates: the definition is the same, the input is not,
  so OSC numbers agree between the two consumers only to the VNG effect (§3, out of scope).

### 4.2 The numbers written

All NOT NULL columns of `frame_analysis` and the `star_metrics` row (schema `db/schema.rs:1917-1981`):

- From the module, shared with stacking: frame FWHM (fit width, D-2), frame ecc, per-star x, y, peak, flux, fwhm,
  fwhm_x, fwhm_y, ecc, θ (canonical, F-2), β, fit residual, `fit_method` (`moffat` / `gaussian`).
- **One frame statistic for both consumers:** the residual-weighted mean over accepted unsaturated fits (stacking's
  statistic with F-3's saturation rule). The Analysis tab's clipped weighted median is retired.
- New definitions: per-star SNR (F-6) and its median; HFR measured on the star's pixels (half-flux radius of the
  background-subtracted aperture).
- Kept formulas on the new inputs, so user thresholds keep their meaning: `frame_snr` (background / noise, in dB),
  `snr_weight` (median flux² / (noise² · background)), `psf_signal` (median peak / noise); background, noise,
  detection threshold, stars detected, width, height, source channels, median β.

### 4.3 Trails and the plate-solve gate

- The trail metric is ported unchanged in definition: the Rayleigh test on 2θ of the measured stars (R², p), the
  `possibly_trailed` rule (R² > threshold and p < 0.01, or R² > 0.15 with median ecc > 0.7 and p < 0.05, ≥ 20 stars,
  field FWHM ≥ 2 px) and the optical-aberration suppression (radial-angle coherence), today in
  `rustafits/src/analysis/mod.rs` from line 1049. It now reads the canonical θ (F-2).
- The plate-solve input gate (`plate_solve/service.rs:622-634`; `input_max_eccentricity` 0.85 AND
  `input_min_trail_r2` 0.65; calibrated on the old numbers: solvable frames ecc 0.62–0.72 with R² 0.30–0.57,
  hopeless 0.90–0.96 with 0.73–0.93) is recalibrated on the star-test frames: the seven wind-shaken frames of
  `trails/` and the frames that solve today (m42, m101, singles), plus the lab's `trails` family. New defaults are
  written only from those numbers; `docs/platesolving/input-and-acceptance-gates.md` records both calibrations.

### 4.4 Version and compatibility

- `frame_analysis` gains a nullable `measure_version INTEGER` (migration; NULL = written by the rustafits analysis).
  The module exports `MEASURE_VERSION`; `AnalysisConfig::config_hash` includes it, so every existing row becomes stale
  and is recomputed the next time its set is analysed (today's behaviour on a settings change); Blink's on-demand
  path recomputes a stale frame.
- The plate-solve input gate trusts only rows with the current `measure_version`; a stale row counts as "no
  analysis" (no refusal), never as a pass or a refusal on old numbers.
- `FrameAnalysis` gains `measure_version` with `#[serde(default)]`: the sync package and the receiver's ingest stay
  compatible both ways. The collab frame meta gains a `measureVersion` key (additive; older apps ignore it). The hub
  and the portal are not changed in this cycle; the release notes say FWHM and ecc values shift and project gate
  thresholds may need a review.

### 4.5 Settings → Analysis

- Kept: the star budget (`maxStars`, default 500; the separate `measureCap` folds into it), the detection threshold,
  the saturation fraction, the trail threshold, batch concurrency, and the frontend rejection defaults.
- Removed: `fitMaxIter`, `fitTolerance`, `fitMaxRejects`, `mrsLayers`, `minStarArea`, `maxStarArea` (rustafits fitter
  internals). A stored document with those keys still decodes (serde ignores them). `registry.ts`, the TS types
  (`ts_export.rs`), `get_settings_defaults` (both hosts call core) and `docs/settings/README.md` change together.
- No command is added or removed; both hosts keep their surfaces.

## 5. Order of work

One spec, three plans, executed in order (subagent-driven, each merged to local `main` when its final review is
clean; push only on the owner's word).

**Plan A — the module and the fixes.**
1. Lab first, no product change: families FWHM 12 and 16 px and donut stars; the SNR truth row (F-6); raw-mode
   inputs for Analysis (pedestal, hot pixels, mosaic) built from the existing raw injections. The bars above are
   already fixed.
2. Extraction into `star_measure/` with no behaviour change. Gate: the probe and the scorecard are bit-identical to
   the baseline (`probe_parity`, the full scorecard diff).
3. `FitDiagnostics` (F-0). Numbers unchanged.
4. One fix per step, each judged on its own bar plus the full no-regression table: F-2 (axes and convergence) → F-1
   with F-7 (region, background, width start, seeds) → F-3 (saturation) → F-4 (β to the Gaussian limit) → F-5 (flux)
   → F-6 (SNR).
5. Real-data checkpoint under the owner's rule (a numeric change is judged on the whole group at master level): the
   acceptance sets (set 204 mono and OSC, run B, Horse C11) are stacked before and after; weights, LN scales and the
   master FWHM are compared with the baseline's.

**Plan B — F1.** The referee-pair test (F-8), then the F1 / F1 v2 sky decision and the c = 1 floor (F-9), each on
its numbers.

**Plan C — the Analysis switch-over (§4).** `analyzer.rs` onto the module; raw and OSC input; the trail metric; the
plate-solve gate recalibration; `measure_version`; Settings. Checks: the star-test corpus measured by the legacy and
the new path frame by frame (comparison column; the corpus has no truth); the Analysis tab, Blink overlay and the
calibration panels checked by launching the app.

At the end: one full uniform lab run (all gates, the full scorecard) and an "after" report beside the baseline.

## 6. Global constraints

- No third-party program name anywhere in this repository (code, comments, docs, commit messages); the comparison
  program is "the ERP". Lab automation lives in athenaeum-lab only.
- Never read or write the production catalog; acceptance copies only as `sqlite3 "file:…?immutable=1"`, working copies
  via `prepare-catalog.sh`.
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `calibration_library/light_cal.rs`,
  `stacking/measure.rs`, `stacking/register/detect.rs`; no `cargo fmt`.
- Cargo with `CARGO_TARGET_DIR=.superpowers/target-acc`, `CARGO_BUILD_JOBS=4`, `--test-threads=4`; one heavy job at a
  time with `memory_pressure` ≥ 30 % free; stop below 100 GiB free disk.
- The full `cargo test -p athenaeum-core --all-targets` before any push.
- `tracing` only; never swallow an error; a refused star is a counted, reasoned outcome, not a silent drop.
- Two backends in sync if any command surface changes (none planned); serde snake_case ↔ camelCase verified against
  `src/types/models.ts`.

## 7. Risks

- **Collab mixed versions.** Members on older app versions publish legacy numbers into the same project until they
  update; `measureVersion` makes the mix visible, the hub-side handling is a later cycle.
- **Gate recalibration data.** The star-test frames are on this Mac; if they do not separate solvable from hopeless
  frames on the new numbers, the gate is reported to the owner with those numbers before any default changes.
- **F-1 on real defocus.** A heavily defocused (annular) star is not a Moffat; such frames get refusals with reasons
  and a frame FWHM only from the stars that fit, possibly none. The Analysis tab shows the refusal count.
- **Speed.** Matched-filter detection and per-star width starts add cost; the 0.6 s and +20 % bars hold them.
