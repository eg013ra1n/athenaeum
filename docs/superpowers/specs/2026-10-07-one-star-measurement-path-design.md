# One star-measurement path — design

Status: draft for the owner's review, 2026-10-07; revised after the independent spec review (Critical 3,
Important 12, Minor 14 — all folded in). Program: star measurement (part 1 was the truth lab; this spec replaces
the former parts 2 and 3). Baseline: `docs/superpowers/research/2026-10-07-star-measurement-baseline.md` (lab
athenaeum-lab e65af49, probes from f5321979); the path comparison below is
`star-lab/results/baseline-2026-10-07/path-compare.txt` (lab 8b58a90, computed from the committed summary and
the existing probe outputs).

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

Why the stacking path (`path-compare.txt` part 1: medians over the (family, image) rows where both paths print the
metric, measure vs analysis):

| Metric | Rows | Measure | Analysis | Measure nearer the ideal |
| ---- | ---- | ---- | ---- | ---- |
| per-star FWHM abs(bias) vs pointfit | 137 | 0.1 % | 0.69 % | 108 of 137 |
| per-star FWHM scatter | 137 | 0.56 % | 0.87 % | 79 of 137 |
| frame FWHM abs(error) vs pointfit median | 137 | 0.5417 % | 0.6388 % | 77 of 137 |
| frame ecc abs(error), all rows (round rows read the floor) | 137 | 0.0838 | 0.0955 | 90 of 137 |
| frame ecc abs(error), elongated truth only | 27 | 0.0032 | 0.0015 | 9 of 27 |
| completeness, % of expected | 121 | 99.7 | 33.3 | 121 of 121 |
| bright coverage, % | 121 | 99.3 | 38.3 | 118 of 121 |
| angle rms | 26 | 26.35° | 17.63° | 5 of 26 |

The analysis path is better on two rows (frame ecc of elongated frames, angle); both are defects of the measure path
this spec fixes (F-2). Cost (`probe-runs-2026-10-07/batches.txt`, m42, 77 frames): measure 223.1 s, analysis 25.2 s
— about 2.9 s vs 0.33 s per frame (derived); at 5417 fits per frame the measure path spends about 0.53 ms per star
(derived). The analysis path is faster per frame only through its 500-star cap.

## 2. Architecture

### 2.1 The module

A new module `crates/athenaeum-core/src/star_measure/`, compiled under `feature = "render"` (as `analysis`;
`stacking` needs `render + solver`, so the Analysis tab must not depend on `stacking`). Dependencies the extraction
moves or leaves: `stacking::robust` (`rcr`, `winsorize`, used by TMeanFlux) moves into the module;
`stacking::prefilter` and `stacking::structure` stay in stacking as adapter-side seed sources (§2.2);
`integration::{plane_reader, stats}` and `astroimage` are already `render`.

| Unit | Responsibility |
| ---- | ---- |
| `domain` | input planes: calibrated float (unit domain) or raw ADU, CFA layout; the plane's noise (MRS) and background model |
| `seeds` | star detection: a matched filter at the field width, threshold in units of the filtered plane's measured noise (selection by integrated signal) |
| `fit` | per-star region, background start, width start, the Moffat fit (rustafits `fit_moffat_2d` / `fit_moffat_2d_fixed_beta` as library functions), canonical axes, acceptance, the per-star outcome |
| `beta` | the per-frame β (§3 F-4) on unsaturated stars |
| `photometry` | the profile-independent star flux (F-5), the saturation test, the per-star SNR (F-6) |
| `frame` | frame aggregates: FWHM, ecc, SNR, the weight inputs, HFR, the trail metric |
| `budget` | the Analysis-mode selection: at most N stars spread over the field |
| `diagnostics` | `FitDiagnostics`: per-star outcome and refusal reason, region radius, background start, growth steps, iterations |

Public surface: `measure_plane(plane, opts) -> PlaneMeasurement` (stars, frame aggregates, diagnostics, refusal
counts) and the refit entry point LN needs, `fit_stars_at(plane, seeds, beta, opts)`. The module exports
`MEASURE_VERSION` (§4.4).

**The saturation test (one definition per domain, used by every module caller):**
- raw ADU: the star's peak pixel ≥ `saturation_fraction` × the sensor clip (from BITPIX/BZERO, or DATAMAX when
  present);
- calibrated float: the relative rule today's LN uses (L-14: the largest pixel within max(2, FWHM) px of the star
  above 0.75 × the hot-pixel-corrected plane maximum), because a calibrated plane's clip level varies per pixel
  ("saturated cores above 1.0 are legal", `docs/stacking/README.md`). The lab's `twins` and `saturated` families are
  unit-domain and are judged with this rule.

**Exemptions (unchanged consumers of the numbers, not callers of the module's tests):** LN keeps its L-14 / L-23
reference-frames mask and its peak-SNR cut on warped planes (`ln/selection.rs:129-157`; L-14 calls the module's
function, L-23 and the peak-SNR cut stay LN's); registration keeps `passes_register_cuts` (0.95 × 65535 and
1/residual ≥ 10, `register/detect.rs:182-195`). Routing them through the module is a later question, not this spec.

### 2.2 Consumers

- **Stacking.** `stacking/measure.rs` becomes a thin adapter that calls the module on every star and keeps today's
  artifacts (`.athf` per plane, the `metrics` payload `FrameMeasurement`, `ChannelMeasurement`). `psf_signal.rs`
  keeps the weight formulas (PSFSW, PSFSNR) and the LN-specific pieces and takes fits, flux and the saturation test
  from the module. LN (`ln/scale.rs`) refits through `fit_stars_at`. The master statistics (`integrate.rs`) and mono
  registration (`register/frame.rs` reading the `.athf`) consume the same numbers. `stars_fitted` keeps its meaning
  (every accepted fit; the Formula weight's `stars` term and `min_stars` read it); the frame statistic uses the
  unsaturated subset (F-3) and `ChannelMeasurement` gains `stars_refused` with per-reason counts.
- **The stacking Measure config** (`MeasurementConfig`, presets, `MeasurePanel.tsx`, the TS types, the measurement
  stage hash), changed together:
  - `detection_sigma` (a peak threshold, default 20) is retired for a new `#[serde(default)]` field in the matched
    filter's units; whole-config precedence means a stored 20 is never reinterpreted;
  - `seed_prefilter` (None / Median3) and `seed_detector` (Peak / Structure) stay as adapter-side seed sources that
    feed `fit_stars_at`; ruling R-M4c-11 in `docs/stacking/README.md` is updated;
  - `psf_model`: `Auto` becomes the continuous per-frame β (F-4), `Moffat4` stays a fixed choice;
  - `STACKING_CONFIG_VERSION` is unchanged (new fields are serde defaults).
- **Analysis and Blink.** `analysis/analyzer.rs` (`map_result` is today's single writer of `FrameAnalysis` and
  `StarMetric`) calls the module in Analysis mode (§4). Commands, events, the batch job and the cache keep their
  shape. `get_frame_star_metrics` re-analyses on demand without a ComputeQueue slot; the 0.6 s bar (§3) covers it.
- **Not touched.** The fast detector used by plate solving (solvemyastro) and by the registration detector
  (`stacking/register/detect.rs`). The rustafits analysis code stays in rustafits, unused by the app.
- **The lab.** `star_probe` runs the module in both modes (stacking: all stars; Analysis: the budget) and keeps the
  rustafits analysis as a `legacy-analysis` arm, so the scorecard shows before and after side by side.

## 3. What is fixed, and the bars

Every bar is fixed here, before anything is measured, and judged by the truth lab against synthetic truth
(athenaeum-lab `star-lab/`). The ERP stays a comparison column, never the referee. A missed bar goes to the owner with
its numbers; a bar is never moved, a row never re-sized, a generator never tuned toward a bar.

| # | Defect (baseline) | Change | Bar |
| ---- | ---- | ---- | ---- |
| F-0 | No refusal reasons (`sharp/m4/f8.0`: 82.7 % fitted of 92.0 % seeded) | `FitDiagnostics` per seed; `stars_refused` + per-reason counts in `ChannelMeasurement` and `FrameAnalysis` (§4.2); the `.athf` flags byte carries per-accepted-fit flags (saturated, β at its cap), the file holds accepted fits only | accepted + refused-by-reason = seeds on every image; numbers unchanged against the extraction step |
| F-1 | Broad / defocused stars collapse (M51 Hα 6.53 px vs analysis 13.39, ERP 14.02; m82 11.43 vs 16.31 / 17.76; `_DSC5767` zero seeds) | region growth judged on (median − sky), not the absolute median; the region cap scales with the star's own width start (≥ 6 FWHM, cap 96 px; today `min(2·nominal, 48)`); background start from the region's rim, not the plain median of a star-filled square; width start from the star's own moments, not one field σ0; matched-filter seeds (§2.1); new lab families FWHM 12 and 16 px and defocused annular ("donut") stars, isolated (`min_sep` ≥ 6 FWHM) | fitted / seeded at FWHM 8: 82.7 % → ≥ 99 %; frame FWHM error vs `pointfit` ≤ 1 % at every FWHM up to 16 px; a donut star is refused with its reason, never reported as a narrow fit |
| F-2 | Angle: measure rms 44.564° / 32.339° on `shape/f2.6/e0.3|e0.5/t30` vs ERP 7.849° / 2.129° (summary row 4); 79–158 of 376–458 fits per `e0.3` family have fwhm_x < fwhm_y (θ on the minor axis; `path-compare.txt` part 2) | canonical axes (fwhm_x ≥ fwhm_y, θ on the major axis); the fitter sees stamp-centred coordinates and amplitude-normalised values so its relative convergence test acts on O(1) parameters (a rustafits change only if the lab shows it is needed) | structural: every accepted fit has fwhm_x ≥ fwhm_y and θ on fwhm_x's axis, pinned by a test; the row-4 angle `eff` cell (prepixel CRLB) ≤ 1.5 on the `shape` families with e ≥ 0.3 |
| F-3 | β and frame FWHM move with the saturated set (2× flux twins: 9.99 %, 15.93 % with a β flip; 2.93 % with β held) | the β sample, the width start and the frame statistic use unsaturated stars only (§2.1's test) | base vs 2× twins: frame FWHM change ≤ 1 %; β change ≤ 10 % relative |
| F-4 | Model-dependent width (Gaussian / blur pointfit −2.07…−3.04 %; β alone 9.60 %; `mixture/b` +4.88 %) | β becomes continuous per frame: the free-β Moffat fit (`fit_moffat_2d`) on the unsaturated bright sample gives the frame's β (median), clamped to [1.5, 30]; every star is then fitted at it. The Gaussian is the large-β limit; there is no separate Gaussian model, so β stays a finite number everywhere (`.athf` f32, `metrics` JSON, `stacking_run_groups.beta`, `group_beta`, `PsfModel::Fixed`, `star_metrics.beta`, `median_beta`; `fit_method` is `moffat`; `FITS_ARTIFACT_VERSION` unchanged) | frame FWHM error vs `pointfit` ≤ 1 % at FWHM ≥ 2 px, ≤ 3 % below, on every family whose stars share one profile (`gauss`, `m*`, `blur*`, `mix*`: the lab's pointfit truth there is a free-β fit, `families.py:113-134`) |
| F-5 | Captured fraction depends on the profile (spread 18.73 % at F 2.6, 22.16 % at F 8.0) → PSFSW / PSFSNR / psfSel | star flux = fitted-model flux in the FWTM ellipse × a per-frame aperture correction from the growth curve (to 5F) of bright unsaturated isolated stars; psfSel reads this flux | spread across profile families ≤ 2 % at F 2.6 and F 8.0; psfSel: `pairs/iso/blur25mix` t60 −13.03 % and `pairs/iso/blur4` t60 −7.28 % → within ±2 %, the Moffat and Gaussian pairs stay within ±1.2 % |
| F-6 | No per-star SNR (stacking); the analysis SNR treats ADU as electrons (gain 1, `rf/analysis/snr.rs`) | one definition: S / σ_S in an aperture of radius max(1.5·FWHM, 3 px), σ_S² = S/g + N_ap·σ_sky²·(1 + N_ap/N_sky), σ_sky measured in the annulus; g from EGAIN (raw ADU) or EGAIN with the calibrated-frame cards (`ATH_CSCL`, `ATH_CFNM`); unknown gain → the shot term is omitted and the record says so. Lab: the generator writes EGAIN (and `ATH_CSCL` on unit-domain planes) from its own gain; the truth SNR is the noiseless stamp summed in the same aperture over σ from the generator's gain, read noise and sky | median bias vs the truth SNR ≤ 3 % on the unsaturated isolated families (`sharp/*`, `background/*`) |
| F-7 | Fit background under nebulosity +0.998σ (`background/neb3_4096`) | F-1's rim background | ≤ 0.3σ under nebulosity; within ±0.1σ on flat fields |
| F-8 | The F1 contradiction (synthetic over-read up to +11.84 %; real data within the bars; psfSel the opposite way) | Plan B (§5): first re-baseline the LN rows on Plan A's fits, then test the referee pair (lab field realism against the harness truth on real frames); F1 and the F1 v2 sky change only after | decided by that test; no F1 change before it |
| F-9 | The c = 1 growth floor (open owner question) | decided in Plan B on that plan's numbers | — |

**Raw-mode truth.** Raw-mode truth exists only for injected stars (`manifests/inject.json`: 7 raw entries, 3
mosaics). In raw mode the bars are completeness ≥ 99 % of the injected stars above the detection threshold and
F-1 / F-4's FWHM bar on the injected stars; F-6 and F-7 have no raw truth.

**The budget (D-3).** The 500 stars cover ≥ 90 % of the 4 × 4 field cells that hold measurable stars.

**No regressions (every fix step).** Every scorecard row a fix does not change by design stays within its own
standard error of the baseline; each fix names the rows it changes by design and judges them by its own bar. Cost
(`probe-runs-2026-10-07/batches.txt`): the stacking Measure stage at most +20 % wall time; the Analysis mode at most
0.6 s per 26 Mpx frame (today 0.33 s on m42), the on-demand Blink path included.

**Out of scope (reported, not fixed):** the VNG per-plane width change (G +12.63 %, B −6.71 %) — a property of the
debayered planes the stack integrates; the resampling broadening (FWHM² +2.596 px² vs 1.848 nominal).

## 4. The Analysis switch-over

### 4.1 Input

The Analysis tab measures raw lights (uncalibrated ADU, pedestal, hot pixels, no flat; OSC as a Bayer mosaic).

- **Reader.** Analysis mode reads through `ImageConverter::read_raw` → `PixelData` (FITS and XISF, `flip_vertical`,
  `bayer_pattern` — the Blink overlay and the OSC rule need both); stacking keeps `PlaneReader` (calibrated FITS).
  XISF closes the probe's gap. BITPIX 16 with BZERO 0 is signed (the FITS standard; `PlaneReader` reads it so):
  `_DSC5767.fit` (BITPIX 16, BZERO 0, NAXIS3 3) is read signed, pinned by a test; the rustafits reader's unsigned
  reinterpretation (`rf/formats/fits.rs:191-199`) is reviewed against the corpus in Plan C, and a file whose data
  makes sense only unsigned is reported, not guessed.
- The pedestal and vignetting are absorbed by the local background (F-1, F-7).
- A hot pixel is rejected by the detector (neighbours above the threshold) and, if seeded, refused by the fit with
  the reason "narrower than the pixel".
- **OSC.** Detection runs on a 2×2 super-pixel plane; the fit uses only the green CFA sites at their true native
  coordinates (the fitter takes a pixel list, so a checkerboard sample needs no interpolation); FWHM is in native
  pixels. This replaces today's green interpolation (`rf/analysis/mod.rs:661-665`) — an OSC width change in its own
  right, named in the release note. Stacking keeps measuring the debayered planes it integrates: the definition is
  the same, the input is not, so OSC numbers agree between the two consumers only to the VNG effect (§3).

### 4.2 The numbers written

All NOT NULL columns of `frame_analysis` and the `star_metrics` row (`db/schema.rs:1917-1981`):

- From the module, shared with stacking: frame FWHM (fit width, D-2), frame ecc, per-star x, y, peak, flux, fwhm,
  fwhm_x, fwhm_y, ecc, θ (radians, image pixel frame, canonical per F-2 — the Blink overlay already draws fwhm_x on θ
  and negates θ under `flip_vertical`, so it is unchanged by design), β, fit residual, `fit_method` (`moffat`).
- **One frame statistic for both consumers:** the residual-weighted mean over accepted unsaturated fits. The
  columns and fields keep their names (`median_fwhm`, `median_eccentricity`; no rename — `FrameAnalysis` stays
  wire- and sync-compatible).
- New: per-star SNR (F-6) and its median (`median_snr`); HFR = the radius enclosing half of the star's F-5 flux,
  measured on its background-subtracted pixels; `stars_refused` (nullable INTEGER, beside `measure_version`).
  `quality_score` stays NULL (no producer today or after).
- **Formulas kept, values shift:** `frame_snr` (background / noise — a ratio, not dB; the "dB" labels in
  `LightsAnalysisTable.tsx` and `FrameInfoPanel.tsx` are wrong today and are fixed in Plan C), `snr_weight`
  (median flux² / (noise² · background)), `psf_signal` (median peak / noise); background, noise, detection threshold
  (now in the matched filter's units), stars detected, width, height, source channels, median β.
- **Stored thresholds the switch invalidates** (named in the release note): `analysis.rejection_defaults` (fwhm,
  ecc, frameSnr, snrWeight); each frame set's stacking selection `max_fwhm_px` / `max_eccentricity`; collab project
  rules on `fwhm_arcsec`, `eccentricity`, `median_snr`, `snr_weight`, `frame_snr` (mirrored in the hub's
  `collab_rules.rs`). The round-star ecc floor alone moves from 0.27 to 0.12 at F 2.6 (report row 4).

### 4.3 Trails and the plate-solve gate

- The trail metric is ported unchanged in definition: the Rayleigh test on 2θ of the measured stars (R², p =
  exp(−nR²)), the `possibly_trailed` rule (R² > threshold and p < 0.01, or R² > 0.15 with median ecc > 0.7 and
  p < 0.05; ≥ 20 stars; field FWHM ≥ 2 px) and the optical-aberration suppression (radial-angle coherence), today in
  `rustafits/src/analysis/mod.rs` from line 1049. It now reads the canonical θ (F-2).
- The plate-solve input gate (`plate_solve/service.rs:622-634`; `input_max_eccentricity` 0.85 AND
  `input_min_trail_r2` 0.65; calibrated on the old numbers: solvable frames ecc 0.62–0.72 with R² 0.30–0.57,
  hopeless 0.90–0.96 with 0.73–0.93) is recalibrated on the star-test frames: the seven wind-shaken frames of
  `trails/` and the frames that solve today (m42, m101, singles), plus the lab's `trails` family. New defaults are
  written only from those numbers; `docs/platesolving/input-and-acceptance-gates.md` records both calibrations.

### 4.4 Version and compatibility

- **One constant.** `star_measure::MEASURE_VERSION` replaces `psf_signal::PSF_FIT_VERSION` (re-exported where
  `stacking/config.rs` uses it) and is folded into the Measure, Register and Normalize stage hashes
  (`stacking/config.rs:812-878`) AND `AnalysisConfig::config_hash`. It is bumped once per merged plan whose code
  changes what a fit accepts or measures (A, C). `config_hash_is_canonical_and_pinned` and the Measure-only
  staleness test are updated with it.
- `frame_analysis` gains nullable `measure_version INTEGER` and `stars_refused INTEGER` (migration; NULL = written
  by the rustafits analysis). Stale rows are recomputed the next time their set is analysed (today's behaviour on a
  settings change); Blink's on-demand path recomputes a stale frame.
- The plate-solve input gate trusts only rows with the current `measure_version`; a stale row counts as "no
  analysis" (no refusal). The collab gate keeps reading whatever row exists (a stale legacy row still gates;
  `measureVersion` makes it visible).
- `FrameAnalysis` gains `measure_version` and `stars_refused` with `#[serde(default)]` (it derives without
  `deny_unknown_fields`): the sync package and the receiver's ingest stay compatible both ways. The collab frame
  meta gains a `measureVersion` key (additive; older apps ignore it). The hub and the portal are not changed here.

### 4.5 Settings → Analysis

- Kept: the star budget (`maxStars`, default 500; `measureCap` folds into it), the detection threshold, the
  saturation fraction, the trail threshold, batch concurrency, and the frontend rejection defaults.
- Removed: `fitMaxIter`, `fitTolerance`, `fitMaxRejects`, `mrsLayers`, `minStarArea`, `maxStarArea`. The
  `analysis.psf` section is removed with them (`src/settings/registry.ts`, `docs/settings/README.md`). A stored
  document with those keys still decodes (serde ignores them). The TS types (`ts_export.rs`) and
  `get_settings_defaults` change together.
- No command is added or removed; both hosts keep their surfaces. `stars_refused` is shown in
  `LightsAnalysisTable` and `FrameInfoPanel` (models.ts updated).

## 5. Order of work

One spec, three plans, executed in order (subagent-driven, each merged to local `main` when its final review is
clean; push only on the owner's word).

**Plan A — the module and the fixes.**
1. Lab first, no product change: families FWHM 12 and 16 px and donut stars (isolated); EGAIN / `ATH_CSCL` on the
   generated frames and the SNR truth row (F-6); raw-mode inputs from the existing injections. The bars above are
   already fixed.
2. Extraction into `star_measure/` with no behaviour change. Gate: the probe output is bit-identical after the
   artifact's f32 narrowing (`probe_parity`) and every scorecard summary cell equals the baseline's.
3. `FitDiagnostics` and `stars_refused` (F-0). Numbers unchanged.
4. One fix per step, each judged on its own bar plus the no-regression table: F-2 (axes, convergence) → F-1 with F-7
   (region, background, width start, seeds) → F-3 (saturation) → F-4 (continuous β) → F-5 (flux, psfSel) → F-6
   (SNR). The stacking Measure config changes (§2.2) land with F-1 and F-4.
5. **Real-data checkpoint** under the owner's rule (a numeric change is judged on the whole group at master level),
   with the athenaeum-lab acceptance harness (`harness/acceptance/`, reused): set 204 mono and OSC, run B, Horse C11
   stacked before and after, with the reference frame pinned (`ReferenceMode::Manual`, the before run's pick) so a
   weight change cannot move the geometry. Both masters are measured by the new path. Pass: master
   median / MAD / noise / FWHM within the master tolerances recorded in `docs/stacking/README.md` (0.1 % / 1 % / 2 % /
   1 %) or better;
   weight rank correlation ρ ≥ 0.9, every larger change tied to a scorecard row; LN scale within 0.5 %; Measure wall
   ≤ +20 %.

**Plan B — F1.** Re-baseline the LN rows on Plan A's fits; the referee-pair test (F-8); then the F1 / F1 v2 sky
decision and the c = 1 floor (F-9), each on its numbers.

**Plan C — the Analysis switch-over (§4).** `analyzer.rs` onto the module; `read_raw` input and OSC; the trail
metric; the plate-solve gate recalibration; `measure_version` / `stars_refused`; Settings; the "dB" labels. Checks:
the star-test corpus measured by the legacy and the new path frame by frame (a comparison; the corpus has no truth);
the Analysis tab, Blink overlay and the calibration panels checked by launching the app.

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
- Every ruling and acceptance run is recorded in `docs/stacking/README.md`; `docs/settings/README.md` and
  `docs/platesolving/input-and-acceptance-gates.md` change with their sections.

## 7. Risks

- **The first run after Plan A** re-measures, re-registers (mono) and re-normalizes every set (`MEASURE_VERSION` in
  the stage hashes) — intended, and named in the release note.
- **Stored thresholds shift** (§4.2): frame-set selection filters, Analysis rejection defaults and collab project
  rules may select different frames until reviewed.
- **Collab mixed versions.** Members on older app versions publish legacy numbers into the same project until they
  update; `measureVersion` makes the mix visible, the hub-side handling is a later cycle.
- **Gate recalibration data.** The star-test frames are on this Mac; if they do not separate solvable from hopeless
  frames on the new numbers, the gate is reported to the owner with those numbers before any default changes.
- **F-1 on real defocus.** A heavily defocused (annular) star is not a Moffat; such frames get refusals with reasons
  and a frame FWHM only from the stars that fit, possibly none. The Analysis tab shows the refusal count.
- **Speed.** Matched-filter detection, per-star width starts and the free-β sample fit add cost; the 0.6 s and +20 %
  bars hold them.
