# Stacking pipeline — design

**Date:** 2026-09-08
**Status:** approved in dialogue (§1–§10 on 2026-09-08, screen layout A the
same day); spec under owner review.
**Math reference:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md`
(every formula, constant and default cited below lives there with its
provenance; this document only names them).
**Baseline data:** `~/Pictures/Calibration Test/LDN1272-Output/` — a WBPP 3.0.1
run over the same 368 lights (2 h 37 min, ~170 GB of intermediates, every
process parameter block in `logs/20260908121336.log`). Its two master lights
and two drizzled masters are the acceptance comparison.
**Supersedes:** the pixel half of
`docs/superpowers/plans/2026-06-10-stacking-engine-roadmap.md` (Phases B–E).
Phase A of that roadmap is done: the calibrated-export generator
(`docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md`) is the
pipeline's first stage.

---

## 0. Owner decisions (2026-09-08)

1. **Hybrid materialization.** Calibrated frames are written to a working
   folder; registration stores transforms, not pixels; integration, local
   normalization and drizzle resample calibrated frames on the fly, band by
   band. Writing registered frames is opt-in.
2. **Incremental milestones**, each its own spec review + implementation plan:
   M1 first master light; M2 local normalization; M3 drizzle; M4 polish.
   This document is the program design; the M1 plan is written from it.
3. **One reference frame per frame set.** Every integration group is
   registered onto the same reference geometry, so the masters are
   co-registered (what WBPP did on the owner's data: the OSC group was
   registered onto the mono reference and written at its 6224×4168 geometry).
4. **Screen layout A** — pipeline board + inspector (§11).

## 1. Scope and principles

**What it is.** A frame set becomes one master light per *integration group*
(camera × colour mode × filter × binning × geometry, optionally × exposure).
The pipeline starts from raw lights and built masters; calibration and VNG
debayer are its first stage and reuse the calibrated-export generator
unchanged.

**What it is not.** Not a mosaic tool (the output geometry is the reference
frame's, never a union), not a comet stacker, not a multi-set stacker, no GPU.
XISF output, cataloging of masters and Bayer drizzle are M4.

**Quality target.** The same algorithm family as the WBPP run, measured on
LDN 1272 against the WBPP masters (§13). Where a vendor parameter could not be
verified, the value is ours and is labelled so in the math reference.

**Rules carried over from `CLAUDE.md`.** Two backends in sync; logic in
`athenaeum-core`; `tracing` only, canonical field names; never name the
reference implementation in code or comments; the headless
`--no-default-features` build must pass (the whole stacking module is gated on
`render` + `solver` exactly like `registration` and `plate_solve`); engine
identity for existing master builds is pinned (the master-build path keeps its
fingerprint test).

## 2. Stages and grouping

| # | Stage | Input → output | Reused when |
| ---- | ---- | ---- | ---- |
| 0 | Plan | set + config → groups, gate blockers, disk estimate | never (cheap, pure DB) |
| 1 | Calibrate | raw light + linked masters → `calibrated/<group>/c_<stem>.fits` (mono: 1 plane; OSC: VNG-debayered, 3 planes) | artifact row exists, spec hash matches, file present |
| 2 | Debayer | folded into stage 1 for OSC groups (the generator debayers before writing); shown as its own row, whose status and progress mirror stage 1 for the OSC groups, so the user sees it | with stage 1 |
| 3 | Measure & select | calibrated frame → metrics, weight, included/excluded | metrics cached for (artifact, measurement config hash) |
| 4 | Reference | `frame_set_reference` if the user set one, else max weight over included frames of all groups | never |
| 5 | Register | detections vs reference detections → transform + QA | `registration_results` row for (reference, config hash, artifact unchanged) |
| 6 | Normalize | global: per frame per channel location/scale vs reference; M2: local normalization grids | global: recomputed (cheap); LN sidecars cached |
| 7 | Integrate | lazy registered band source → master, rejection maps, stats | never |
| 8 | Drizzle (M3) | calibrated + transform + rejection bitmaps + weights + LN → `_drizzle<s>x` | drizzle off |
| 9 | Output & finish | masters written, provenance rows, notification, cleanup policy | never |

**Grouping keys.** `INSTRUME` (sanitized), colour mode (mono / CFA, from the
Bayer cards), `FILTER` (sanitized, `NoFilter` when absent), `XBINNING`,
width × height. `splitByExposure` (default **off**, tolerance 2 s) adds
`EXPTIME`. The group key is a stable string
`<instrume>__<mono|osc>__<filter>__bin<n>__<w>x<h>[__<exp>s]` used in paths and
rows. Mixed exposures inside one group are handled by the weights and the
normalization; users who want HDR-separate masters turn the split on.

**Gate** (stage 0, the one gate for the Run button and for `start_stacking`):

- `check_mode_ready(calibratedLights)` from the export gate, unchanged: every
  linked calibration set is a built master, every light has at least one link,
  every master file is on disk. Blockers keep the export wording and the
  `→ Coverage` deep link.
- The reference frame (manual or auto) is on disk.
- Working and output folders validate (§9.4) and free space ≥ the estimate.
- At least 3 included frames in at least one group.

## 3. Registration

Registration runs on **calibrated** frames (flat-corrected, hot pixels
removed — cleaner detections, and what WBPP registers). Geometry equals the
raw frame's; the transform maps calibrated-frame pixels to reference-frame
pixels, 0-based pixel-centre convention throughout (FITS 1-based only at the
WCS card boundary).

### 3.1 Detection

`detect_fast` with Moffat centroid refinement (~0.05 px on well-sampled stars),
per-star σ from the fit; the detector is threshold-free (its adaptive ladder
targets `maxStars`), so there is no detection sigma. Cuts: saturation (peak ≥ `upperLimit` of the
plane's range), eccentricity > 0.8 (the existing `select` rule, moments-based),
SNR < `minSnr` (10). Keep the **2000 brightest** by flux (`maxStars`,
configurable). For RGB frames detection runs on the luminance
`0.25R + 0.5G + 0.25B`.

### 3.2 Matching

1. **Seed**: the existing scale-invariant quad matcher (distance ratios;
   mirror-invariant, which is why flipped frames register today) → seed
   similarity/affine from the quad centres.
2. **Correspondence**: project every subject detection through the seed,
   **KD-tree** nearest neighbour in the reference list within
   `ransacTolerancePx` × 2 (replaces the O(N·M) scan).
3. **RANSAC** on the configured model: minimal samples (2 pairs similarity, 3
   affine, 4 homography), inlier tolerance `ransacTolerancePx` (1.9),
   `ransacMaxIterations` (2000) with the adaptive stop
   `N = log(1 − 0.9999)/log(1 − w^k)`, early exit above 98 % inliers,
   deterministic seed (the solver's `ransac_seed` convention). Model score =
   inliers × overlap × regularity / (1 + RMS) with the three quality indexes
   defined as in the math reference §5.2.
4. **Refit** on the inliers by σ-weighted least squares
   (`w = 1/(σx² + σy² + ε)`, uniform when unrefined) with iterative 3σ
   clipping (≤ 5 rounds, stop on Jaccard > 0.97 of the inlier set).
5. Optional **distortion** on the residuals (§3.3).

### 3.3 Transformation models

| Model | Params | Use |
| ---- | ---- | ---- |
| `similarity` | 4 | same rig, few stars (auto below 12 inliers) |
| `affine` | 6 | auto for 12–29 inliers; the legacy `registration_results` shape |
| `homography` | 8 | **default**; normalized DLT (Hartley); flips are a negative determinant, nothing special |
| `polynomial2..4` | + (order+1)(order+2)−6 per axis, per direction | fitted on the residuals of the linear model; forward and inverse fitted independently (the plate solver's SIP convention); auto-enabled for cross-camera groups (different `INSTRUME` or geometry than the reference) with ≥ 200 inliers whose inliers are consistent (overlap index ≥ 0.6 — inlier hull over the matched pairs' hull) and cover the frame (regularity index ≥ 0.6 — fraction of a 4×4 grid holding an inlier); an explicit order is always honoured |
| `tps` (M4) | ≤ 4000 nodes | regularized thin-plate spline, smoothing λ, node pruning by surface simplification, outlier removal |

`model: auto` resolves per frame as above; the resolved model is recorded.

### 3.4 Output geometry and coverage

The reference frame's W×H, always. A source pixel that maps outside its frame
is **NaN** in the resampled band; the combiner already drops non-finite
samples per frame with accounting, so edges and rotated corners simply have
fewer samples. Frames of a different geometry than the reference (the OSC
camera here) are resampled into the reference geometry like any other.

### 3.5 Interpolation

`nearest`, `bilinear`, `bicubicSpline` (Keys, 4×4), **`bicubicBSpline`
(default — WBPP 3.0.1 sets it explicitly; smoother, better noise behaviour
for the rejection pass)**, `lanczos3`, `lanczos4`, `mitchellNetravali`.
`clampingThreshold` (0.30) applies the math reference's two clamping rules:
the per-row/column linear replacement for the bicubic spline, the
negative-lobe attenuation for Lanczos — both applied **separably**, once per
axis (a 2-D split of every product weight by sign over-counts the negative
lobes on smooth star flanks; measured on the M1 Plan 1 synthetic field it
inflated Lanczos-4 star flux by +1.5 %, the separable form leaves
+0.1–0.2 %, Lanczos-3 is unbiased). Two measured kernel properties the
acceptance run must keep in mind: windowed-sinc kernels carry a
phase-dependent first-moment error of ~0.02 px (uniform over a frame at a
given sub-pixel phase; the cubic kernels have linear precision and sit at
1e-4 px), and Lanczos-4 with the 0.3 clamp keeps the residual one-signed
+0.1–0.2 % flux inflation just named. The resampler is a gather: for every
output pixel, inverse-map to source coordinates, evaluate the kernel. The
inverse map is the stored inverse (homography inverse; polynomial inverse
coefficients), evaluated per pixel with Horner-form polynomials.

### 3.6 Per-frame QA and failure

Recorded per frame: inliers, inlier ratio, RMS, σ_RMS, peak error x/y,
scale, rotation, translation, flipped, quality score, model resolved, time.
A frame **fails** registration when RANSAC yields < 8 inliers, when the
linear fit's scale is outside `[0.8, 1.25]`, or when RMS > `maxRmsPx` (2.0)
with `failOnMaxRms` on (default off: warn, keep). A failed frame is excluded
from the run with reason `registration failed: …` when
`excludeOnRegistrationFailure` is on (default on), else the run fails.

### 3.7 Optional registered frames

`writeRegisteredFrames` (default off) writes `registered/<group>/r_<stem>.fits`
(float32, reference geometry, NaN coverage, copy-through cards + `ATH_REG`
cards with the transform) after registration, resampling once with the
configured kernel. They are artifacts (§9.3), never cataloged, and are not
read by later stages — the lazy source stays the single code path.

## 4. Measurement, weights, selection

Today's `frame_analysis.psf_signal` is `median(peak)/noise`, not the PSF
Signal Weight, and it measures raw frames. Stage 3 measures **calibrated**
frames and stores its own metrics on the run.

### 4.1 Metrics per calibrated frame, per channel

Star detection (structure-map front end configurable per math reference
§5.1; our detector's parameters are the ones exposed), PSF fitting with the
`psfModel` (`auto` = Moffat β ∈ {2.5, 4, 6, 10} best MAD, or `moffat4`), the
hybrid PSF/aperture flux at FWTM, RCR-cleaned and Winsorized mean fluxes,
`M*`/`N*` from the large-scale background residual (MMT residual, scale 256),
MRS noise σ_N, FWHM (weighted by fit residual), eccentricity, star count,
median, MAD, `MedianMeanDev`, classic `SNRWeight`. Formulas: math reference
§1–§2.

### 4.2 Weight modes

| Mode | Weight | Default |
| ---- | ---- | ---- |
| `psfSignalWeight` | `PSFSW = 5.326e-6·TFlux·TMeanFlux / (9.0e6·σ_N·M*)` | **yes** |
| `psfSnr` | `1.316e-7·TFlux² / (4.987e6·σ_N²)` | |
| `noise` | `(noiseScale/σ_N)²` | |
| `formula` | the classic WBPP formula with user weights A/B/C/D and pedestal P (defaults 15/15/20/0 + 50) | |
| `exposure` | `EXPTIME` | |
| `keyword` | a FITS keyword (default `SSWEIGHT`) | |
| `none` | 1 | |

Weights are per channel for RGB (the combiner runs per plane) and are
normalized by the maximum per channel inside a group. The frame-level weight
shown in the UI and used for ranking is the mean over channels.

### 4.3 Selection

In order: manual exclusions (per frame, persisted on the set config); frames
with normalized weight < `minWeightFraction` (0.05) of the group maximum; the
optional hard filters `maxFwhmPx`, `maxEccentricity`, `minStars`; registration
failures (§3.6). Every exclusion carries a reason string shown in the Frames
table. A group with < 3 included frames is skipped with a warning; a run with
no viable group fails at plan time.

### 4.4 Reference selection

`reference.mode = manual` uses `frame_set_reference` (set in the Analysis
tab, unchanged). `auto` picks the included frame with the highest frame-level
weight **inside the largest group** (most included frames; ties by total
exposure), ties inside the group broken by star count. Weights are not
compared across cameras — the PSF Signal Weight scale differs between a mono
and an OSC sensor — and the largest group is the one whose geometry the
other masters should adopt. On the owner's data this picks a frame of the
208-frame mono group, as WBPP did. Auto never writes `frame_set_reference`;
the run records `reference_frame_id` and `reference_mode`.

## 5. Normalization

### 5.1 Global (M1)

Per frame per channel, against the reference frame: location `m_i` = median,
scale `s_i` from the `scaleEstimator` (`bwmv` default, `mad`, `avgDev`),
two-sided and collapsed by the mean of the sides (math reference §2.4).
Both are measured on the **calibrated** (un-resampled) frame, on a 1/16
stratified pixel sample, during stage 3 — so no extra pass is needed and
every frame, the reference included, is measured the same way. The
resampling kernel changes the noise scale by a factor that is common to the
whole group (the reference is resampled through the same kernel with its
identity transform, so a smoothing kernel such as the bicubic B-spline
smooths it too); the ratios `s_ref/s_i` are therefore unaffected to first
order, and the residual second-order difference for interpolating kernels at
fractional phases is accepted. Output normalization modes: `none`, `additive`, **`additiveWithScaling`** (default,
`v′ = (v − m_i)·(s_ref/s_i) + m_ref`), `multiplicative`,
`multiplicativeWithScaling`. Rejection normalization: `none`,
**`scaleZeroOffset`** (default), `equalizeFluxes`, `local` (M2). The engine
represents both as per-frame `(offset, scale)` pairs; local normalization adds
per-frame grids.

### 5.2 Local (M2)

- **Reference** per group: integration of the best `referenceFrames` (20)
  included frames by weight, linear-fit rejection, global normalization,
  resampled lazily, kept in RAM for the LN pass and written to
  `ln/<group>/reference.fits` as an artifact.
- **Per frame**: reference and target background models (MMT residual at
  `scale` 1024 after hot-pixel median filter radius 2, low clip 4.5e-5, high
  clip 0.85 relative, deviation thresholds 3.0σ / 3.2σ, rejection limit 0.3
  per cell); global scale `s` from matched-star PSF-flux ratios cleaned by
  RCR (limit 0.3); `A(x,y) = s` (or a local scale spline when
  `localScale` is on), `B(x,y) = B_ref − s·B_tgt`, sampled on the
  `scale/8` grid (49×33 for the reference geometry) and interpolated with a
  bicubic B-spline; applied `v′ = A·v + B`. Written per frame as
  `ln/<group>/<stem>.athln` (binary: header, dims, stride, two f32 grids per
  channel, global scale/locations, relative scale factors).
- Used as output normalization and as rejection normalization when selected;
  drizzle reads the same grids at reference coordinates.

## 6. Integration

### 6.1 Engine generalization

`run_banded` gains a `FrameSource` trait:

```rust
pub trait FrameSource: Sync {
    fn frame_count(&self) -> usize;
    fn width(&self) -> usize; fn height(&self) -> usize; fn channels(&self) -> usize;
    fn bytes_per_row(&self, frame: usize) -> usize;              // for the band budget
    fn read_band(&self, plane: usize, y0: usize, rows: usize,
                 out: &mut BandPlanes, concurrency: usize,
                 on_bytes: &(dyn Fn(u64) + Sync), cancel: &AtomicBool) -> Result<()>;
}
```

- `FileSource` — today's `BandSource` (raw FITS by position), unchanged:
  1-plane, master builds keep using it and keep their fingerprint. Three-plane
  positional reads live in a small sibling, `PlaneReader` (one file, one
  plane at a time, the same `PlaneKind` decode), which is what
  `RegisteredSource`, the measurement stage and drizzle read calibrated
  frames through — no stage reads a multi-plane file banded across frames.
- `RegisteredSource` — one entry per included frame: calibrated file (f32,
  1 or 3 planes), inverse transform, kernel, clamping. `read_band` maps the
  band boundary densely through the inverse (every 32 px along the four
  edges), takes the source row span, expands by kernel radius + 1, clamps;
  if the span exceeds 60 % of the frame height it reads the whole plane. Reads
  are positional per plane; resampling fills f32 band rows, NaN outside.
  Parallelism: across frames with the storage-class read concurrency (a
  worker reads one frame's window, resamples into that frame's band slot,
  drops the window), rows within a frame by rayon. The band budget policy is
  unchanged and sees f32 sample widths.

Cost on this Mac for the 208-frame mono group: ≈ 5.4 Gpx × 16 taps ≈ 87 G
multiply-adds per pass ≈ 5–30 s of compute, against ≈ 21 GB of calibrated
reads ≈ 100–120 s at the measured disk rate — the pass is disk-bound, which
is the point of not writing registered frames.

`run_banded` further gains: per-frame `(offset, scale)` normalization (the
existing `scales` slot plus a per-frame offset), per-frame weights, an
optional per-frame LN grid, a channel loop (one pass per plane), and the
survivor masks described next.

### 6.2 Combiner v2

`combine_pixel_weighted(values: &mut [f32], weights: &[f32], recipe) →
(value, SurvivorMask)` where `SurvivorMask` is a bit set over frame index
(u64 words). Rejection runs on the (rejection-)normalized working copy and
returns the mask; the average is the weighted mean of the survivors' output-
normalized raw values (`x ≠ 0`, `w > 0`); median ignores weights. All-rejected
falls back to the median of all raw values. From the masks the engine
accumulates:

- **rejection maps** (`rejection_low`, `rejection_high`: count per pixel,
  written as float32 FITS when `writeRejectionMaps` is on),
- **per-frame rejection bitmaps** (`rej/<run>/<group>/<stem>.rej`, one bit
  per pixel per channel, LZ4-free plain bits — 3.2 MB per plane) only when
  drizzle is enabled; deleted at the end of the run unless intermediates are
  kept,
- per-frame rejected fraction (Frames table, group stats).

### 6.3 Rejection menu and the Auto rule

Existing: `none`, `percentileClip`, `sigmaClip`, `winsorizedSigma`,
`linearFitClip`, now weight- and mask-aware. Parity with the reference
semantics (math reference §3.4) is handled deliberately, because the same
functions build calibration masters and their output is fingerprint-pinned:

- `linearFitClip` adopts the reference dispersion
  `s = 2·adev·sqrt(1 + b²)` in M1 — otherwise the Auto thresholds 5.0/3.5
  would reject about twice as hard as the WBPP run they are copied from.
  Master builds never select linear fit automatically (their Auto is
  Winsorized / percentile / median), so only a master built with an explicit
  linear-fit recipe changes; that test pin is re-measured in the same task.
- `winsorizedSigma` keeps today's semantics in M1 (it is what every master
  with n ≥ 15 is built with). Aligning it with the reference (Sn
  initialization, the 1.5σ Winsorization with cutoff 5, the 1.134 factor)
  is an M4 change with its own fingerprint re-pin and a before/after
  measurement on the owner's masters.

M4 also adds `minMax`, `esd`, `rcr` and the `largeScale` low/high
post-processing. Parameters keep the two-axis `IntegrationRecipe` shape
(combination × rejection) the master builder uses.

Auto (WBPP 3.0.1 as observed on the owner's data): n < 8 → percentile
0.2/0.1; 8 ≤ n < 20 → Winsorized 4.0/3.0; n ≥ 20 → linear fit 5.0/3.5. Range
rejection: `rangeLow` 0.0 on, `rangeHigh` off (0.98 when on).
`minWeight` 0.005 as the engine floor below the UI's selection threshold.

### 6.4 Output

- Master: float32 FITS, 1 or 3 planes (`write_fits_f32`), path
  `<output>/<master name>` (§9.5). Header: copy-through cards from the
  reference frame (object, instrument, filter, dates, Bayer-free, `ROWORDER`),
  `IMAGETYP = 'MASTER LIGHT'`, `NCOMBINE`, `EXPTIME` = weighted total,
  `DATE-OBS`/`DATE-END` = earliest/latest, the **WCS** of the reference
  frame's plate solve rewritten by the new WCS card writer (CRPIX unchanged —
  the master is in reference geometry; SIP cards when the solve has them),
  and the provenance cards `ATH_STK = 1`, `ATH_STKV` (format version),
  `ATH_STKN` (frames), `ATH_STKR` (recipe string), `ATH_STKW` (weight mode),
  `ATH_STKO` (normalization), `ATH_STKF` (reference frame uuid), `ATH_STKG`
  (group key), `ATH_STKID` (run id).
- Rejection maps: `<master stem>_rejlow.fits` / `_rejhigh.fits` (optional).
- **Scanner rule**: a file carrying `ATH_STK` or `ATH_REG` is an Athenaeum
  artifact and is never cataloged, the same one-rule skip as
  `CALSTAT + ATH_CSRC` (calibrated intermediates already carry those).
  Cataloging masters is an M4 decision.
- Stats per group (`stats_json`): frames, included, rejected low/high
  fraction, MRS noise of the master, noise of the best sub, `snrGain` =
  ratio of PSF SNR of the master to the best sub, FWHM and eccentricity of
  the master (same measurement as stage 3), per-stage durations, bytes read.

## 7. Drizzle (M3)

Inverse-mapping drizzle (math reference §6.3): output scale 1, **2** or 3;
`dropShrink` 0.9; kernels `square` (default), `circle`, `gaussian`;
`kernelGridSize` 16. For each output tile (512×512 output pixels) the tile's
source window is the inverse image of its bounds plus one pixel; source
pixels are iterated, their drops mapped forward through the frame's
transform (homography/polynomial; spline in M4) and clipped exactly against
the output pixels they touch; contributions `a·w·N(d)` with `N` the frame's
normalization (LN grid at reference coordinates when available, else
scale + zero offset) accumulate into `I` and `W`; rejected pixels (the run's
bitmaps) and zero samples are skipped; final `I /= W / dropShrink²`, weight
map kept and written as `<stem>_drizzle<s>x_weight.fits` when asked.
Parallel by tile, one `I`/`W` pair per plane. Output `<master
stem>_drizzle<s>x.fits` with the WCS scaled (`CRPIX·s`, `CD/s`) and
`ATH_DRZ`, `ATH_DRZP`, `ATH_DRZK` cards. Bayer drizzle (CFA source → RGB
planes without demosaic) is M4 and needs stage 1 to also keep the calibrated
CFA mosaic.

## 8. Execution model

- **Job**: `ComputeJobKind::Stacking`, one job per run, label
  `Stacking · <set name>`; `active_stacks: HashMap<run_id, StackHandle>` on
  `ServiceContext`; the master-build thread pattern (`std::thread::Builder`,
  `catch_unwind`, handle removed and the completion event emitted exactly
  once). The permit is acquired first, so a queued run waits behind an
  analysis or a master build like everything else. Registration stops being
  a queue-less side path: it runs inside the stacking job.
- **Parallelism inside a run**: per-frame stages (calibrate, measure,
  register) fan out over frames on the image pool with an admission of
  `min(cores, memory budget / per-frame working set)` frames in flight —
  `RegisteredSource`'s read concurrency rules for reads, rayon for pixels.
  Banded stages use the band budget.
- **Progress**: `stacking-progress` (§10.2), throttled 300 ms, one monotonic
  `percent` per stage; the sidebar `ComputeQueueIndicator` shows the job with
  cancel. **Cancel** is cooperative: checked per frame in fan-out stages and
  per band (and per frame inside a band read) in banded stages; a cancelled
  run keeps its finished artifacts and writes no master.
- **Checkpointing**: every artifact is keyed by a config hash of the stage's
  inputs (§9.3). A re-run reuses valid artifacts; "Re-run from" lists the
  stages whose cached outputs would be reused above it. "Change rejection and
  re-integrate" costs one integration pass.
- **Cleanup policy** (`output.cleanup`): `keepAll` (default), `deleteRegistered`
  (registered frames and rejection bitmaps), `deleteIntermediates` (also
  calibrated frames and LN data). The Results panel shows the working
  folder's size per set with a "Delete intermediates" action.
- **Logging**: `info!` at run start (`run_id`, `set_id`, `count`, `groups`,
  `config_hash`) and end (`duration_ms`, `outcome`, per-stage `duration_ms`
  in fields `stage`), `debug!` per frame in registration/measurement
  (`frame_id`, `rms_px`, `inliers`, `weight`), `warn!` for every exclusion
  and every non-fatal fallback. New field names (`run_id`, `group_key`,
  `stage`, `inliers`, `rms_px`, `weight`) go into the logging spec's
  dictionary in the same change.

## 9. Data model, configuration, paths

### 9.1 Tables (all `CREATE TABLE IF NOT EXISTS`, indexes on the FKs)

```
stacking_runs(id PK, frames_set_id FK→frames_set ON DELETE CASCADE,
  status TEXT NOT NULL,          -- planning|running|done|failed|cancelled
  started_at TEXT NOT NULL, finished_at TEXT,
  config_json TEXT NOT NULL, config_hash TEXT NOT NULL,
  reference_frame_id INTEGER, reference_mode TEXT NOT NULL,   -- auto|manual
  working_dir TEXT NOT NULL, output_dir TEXT NOT NULL,
  summary_json TEXT, error TEXT)

stacking_run_groups(id PK, run_id FK ON DELETE CASCADE, group_key TEXT NOT NULL,
  instrume TEXT, color_mode TEXT NOT NULL, filter TEXT, binning INTEGER,
  width INTEGER, height INTEGER, exposure REAL,
  frame_count INTEGER NOT NULL, included_count INTEGER NOT NULL,
  master_path TEXT, drizzle_path TEXT, rejection_low_path TEXT, rejection_high_path TEXT,
  stats_json TEXT, status TEXT NOT NULL, error TEXT,
  UNIQUE(run_id, group_key))

stacking_run_frames(id PK, run_id FK ON DELETE CASCADE, group_id FK ON DELETE CASCADE,
  frame_id FK→frames ON DELETE CASCADE,
  included INTEGER NOT NULL, exclusion_reason TEXT,
  weight REAL, weight_channels_json TEXT, metrics_json TEXT,
  reg_status TEXT, reg_model TEXT, reg_rms_px REAL, reg_inliers INTEGER,
  reg_inlier_ratio REAL, reg_flipped INTEGER, rejected_fraction REAL,
  UNIQUE(run_id, frame_id))

stacking_artifacts(id PK, frames_set_id FK ON DELETE CASCADE,
  frame_id INTEGER FK→frames ON DELETE CASCADE,   -- NULL for group-level artifacts (ln_reference)
  group_key TEXT NOT NULL, kind TEXT NOT NULL,     -- calibrated|registered|ln|ln_reference|metrics
  path TEXT, config_hash TEXT NOT NULL, size INTEGER, modified_at TEXT,
  payload_json TEXT,                               -- metrics rows keep their values here
  created_at TEXT NOT NULL)
CREATE UNIQUE INDEX stacking_artifacts_key
  ON stacking_artifacts(frames_set_id, group_key, kind, COALESCE(frame_id, 0))
  -- an expression index, because a plain UNIQUE treats NULL frame_ids as distinct

stacking_set_config(frames_set_id PK FK ON DELETE CASCADE,
  config_json TEXT NOT NULL, excluded_frame_ids_json TEXT NOT NULL DEFAULT '[]',
  updated_at TEXT NOT NULL)
```

`registration_results` gains, via the guarded `ALTER TABLE` pattern:
`model TEXT`, `transform_json TEXT`, `inlier_ratio REAL`,
`peak_error_px REAL`, `scale REAL`, `rotation_deg REAL`,
`flipped INTEGER NOT NULL DEFAULT 0`, `config_hash TEXT`,
`source_kind TEXT` (`calibrated`). The `affine_*` columns keep the linear
part. Rows from the retired flow are simply overwritten by the
`UNIQUE(frames_set_id, frame_id)` upsert.

`transform_json` is `PixelMap::to_json()` verbatim — `{ "linear": { "kind":
"homography", "m": [[..],[..],[..]] }, "linearInv": { … }, "distortion": null |
{ "order": 3, "center": [cx, cy], "scale": s, "domain": [u0, v0, u1, v1],
"forward": { "order": 3, "ax": [..], "ay": [..] }, "inverse": { … } } }`. The
polynomial acts on coordinates normalized as `u = (x − cx)/s`, `v = (y − cy)/s`
(reference centre and half the longer side) for conditioning. `domain` is the
normalized box the polynomials were fitted over (the inliers' bounding box,
each side inflated by 10 %); evaluation clamps `(u, v)` into it so a far
corner gets the nearest fitted edge's displacement, never a polynomial
extrapolation (absent = unbounded, for rows written before the field).
`linearInv` is recomputed from `linear` on load, so a stored inverse can
never disagree with its forward matrix.
`stacking_runs.summary_json` and the per-run `runs/run-<id>.json` file (same
content: config, reference, groups, per-frame rows, stats) are the
provenance, modelled on `master_provenance`.

### 9.2 Configuration

One `StackingConfig` JSON (camelCase, `version: 1`, every field optional on
the wire with the defaults below; the same struct is exported to TS):

```
grouping:      { splitByExposure: false, exposureToleranceSec: 2.0 }
calibration:   CalibratedLightOptions (the export's: flat norm, hot pixels on, debayer on)
measurement:   { weightMode: "psfSignalWeight", psfModel: "auto", maxStars: 24576,
                 formula: { fwhm: 15, eccentricity: 15, snr: 20, stars: 0, pedestal: 50 },
                 keyword: "SSWEIGHT" }
selection:     { minWeightFraction: 0.05, maxFwhmPx: null, maxEccentricity: null,
                 minStars: null, excludeOnRegistrationFailure: true }
reference:     { mode: "auto" }
registration:  { model: "auto", distortion: "off", interpolation: "bicubicBSpline",
                 clampingThreshold: 0.30, maxStars: 2000, ransacTolerancePx: 1.9,
                 ransacMaxIterations: 2000, maxRmsPx: 2.0, failOnMaxRms: false,
                 detection: { minSnr: 10, maxEccentricity: 0.8 },
                 writeRegisteredFrames: false }
normalization: { output: "additiveWithScaling", rejection: "scaleZeroOffset",
                 scaleEstimator: "bwmv",
                 local: { enabled: false, scale: 1024, referenceFrames: 20,
                          psfModel: "auto", localScale: false } }      -- enabled: true from M2
integration:   { combination: "average", rejection: { method: "auto" },
                 minWeight: 0.005, rangeLow: 0.0, rangeHigh: null,
                 writeRejectionMaps: false }
drizzle:       { enabled: false, scale: 2, dropShrink: 0.9, kernel: "square",
                 useRejection: true, useWeights: true, useLocalNormalization: true,
                 writeWeightMap: false }
output:        { format: "fits", cleanup: "keepAll" }
paths:         { workingDir: null, outputDir: null }     -- null = the global default
```

Precedence: set config (`stacking_set_config`) > global defaults (settings key
`stacking.defaults`, JSON) > built-in defaults. Presets are built-in
transforms of the config: **Default** (above), **Fast preview** (bilinear,
sigma clip 4.0/3.0, LN off, drizzle off, `deleteIntermediates`), **Maximum
quality** (bicubic B-spline, polynomial-3 distortion, LN on, drizzle 2×,
rejection maps written). Editing any field makes the preset **Custom**. Settings → Stacking edits the
global defaults with the same inspector forms and holds the default folders
(`stacking.working_dir`, `stacking.output_dir`).

### 9.3 Artifacts and config hashes

Per stage, `config_hash = xxh3(canonical JSON of the stage's config
subtree + the upstream hashes it depends on + the source file identity
(`files.id`, size, `modified_at`))`. Stage 1 depends on the resolved master
paths and their identity; stage 3 on stage 1; stage 5 on stage 1 + the
reference id; LN on stage 5 + the reference set. An artifact is reused only
when its row's hash matches and the file exists with the recorded size. A
stale artifact is overwritten in place; orphan files in the working folder
are reported by the cleanup action, never deleted silently.

### 9.4 Paths

- Global defaults: `stacking.working_dir`, `stacking.output_dir` (empty =
  unset; the tab blocks the Run button with "Choose a working folder" until
  one exists). Per-set overrides in `paths`.
- Validation = `validate_transfer_dir`'s gate reused: absolute,
  `PathPolicy::check`, create-if-missing + write probe, the two folders may
  not be equal, the working folder may not sit inside the output folder.
  Overlap with a scan root is **allowed with a warning** — every artifact
  and every master carries a scanner-skip card.
- Web build: the folder browser gets a `stacking` scope backed by
  `browse_directories`; the same validation runs server-side.
- Free-space estimate before a run: calibrated `Σ frames × planes × W×H×4`,
  registered (if on) the same, LN reference per group, masters, drizzle
  `× s²`; compared with `statvfs` of the working folder's volume (the
  `diskspace` probe).

### 9.5 Working folder layout and names

```
<working>/<set slug>/
  calibrated/<group key>/c_<stem>.fits
  registered/<group key>/r_<stem>.fits          (optional)
  ln/<group key>/reference.fits, <stem>.athln    (M2)
  rej/run-<id>/<group key>/<stem>.rej            (drizzle runs only, temporary)
  runs/run-<id>.json
<output>/
  <set slug>_<filter>_<instrume>_<n>x<exp>s.fits            (equal exposures ±0.5 s)
  <set slug>_<filter>_<instrume>_<n>f_<total>s.fits          (mixed exposures)
  …_drizzle<s>x.fits, …_rejlow.fits, …_rejhigh.fits, …_drizzle<s>x_weight.fits
```

Slugs use the calibration library's sanitizer. A name collision in the output
folder gets `_2`, `_3`… (never overwrite; the run's rows point at the file
actually written).

## 10. Commands, events, types

### 10.1 Commands (Tauri `commands/stacking.rs` ↔ Axum `routes/stacking.rs`, logic in `api/stacking.rs`)

| Command | Args → result |
| ---- | ---- |
| `get_stacking_plan` | `{ setId, config? }` → `StackingPlan { groups[], blockers[], reference, frameCount, includedCount, estimateBytes, freeBytes, staleStages[] }` |
| `start_stacking` | `{ setId, config, rerunFrom? }` → `{ runId, jobId }` |
| `cancel_stacking` | `{ runId }` |
| `get_stacking_runs` | `{ setId, limit? }` → `StackingRunSummary[]` |
| `get_stacking_run` | `{ runId }` → `StackingRunDetail { run, groups[], frames[] }` |
| `get_stacking_config` / `set_stacking_config` | `{ setId }` → `StackingConfig` + excluded frame ids / `{ setId, config, excludedFrameIds }` |
| `get_stacking_defaults` / `set_stacking_defaults` / `reset_stacking_defaults` | global `StackingConfig` |
| `get_stacking_paths` / `set_stacking_paths` | `{ working: PathSetting, output: PathSetting }` / `{ working?, output? }` (`null` = reset) |
| `get_stacking_work_usage` / `cleanup_stacking_work` | `{ setId }` → bytes per artifact kind / `{ setId, what: "registered" | "intermediates" | "all" }` |

Retired: `register_frame_set`, `cancel_frame_set_registration`,
`get_frame_set_registration` (both backends, `ts_export`, TS types).
Kept: `set_frame_set_reference`, `get_frame_set_reference`.
Every command wears `#[tracing::instrument(skip_all, err)]`; new model types
go into `ts_export.rs`.

### 10.2 Events

```
stacking-progress { runId, setId, stage, groupKey: string|null, current, total,
                    percent, bytesDone, bytesTotal, frameId: number|null, message: string|null }
stacking-complete { runId, setId, success, cancelled, error: string|null,
                    warnings: string[], masters: [{ groupKey, path, drizzlePath: string|null }] }
```

`stage` ∈ `calibrate | measure | reference | register | normalize | integrate
| drizzle | output`. Web mirrors via `SseProgressEmitter`. The frontend
listens with the cancelled-flag pattern and notifies once per run
(`kind: 'stacking'`, `dedupeKey: 'stack-<runId>'`).

### 10.3 Feature gating

`athenaeum_core::stacking` is `#[cfg(all(feature = "render", feature =
"solver"))]` like `registration`; the Tauri commands and Axum routes follow
the same cfg pattern the registration commands use today, so the headless
check keeps passing.

## 11. The Stacking tab (layout A)

Replaces the Registration tab in `FrameSetDetail.tsx` (tab key `stacking`,
label **Stacking**, icon `SquareStack` — `Layers` is taken by Export;
`?tab=stacking` deep link).
Gated only on "the set has lights"; blockers are shown inside the tab, not by
greying the tab. The Analysis tab's "Set as reference" star stays and is the
manual reference path.

### 11.1 Structure

```
┌ toolbar ───────────────────────────────────────────────────────────────────┐
│ Preset ▾ · 📁 Working … · 📁 Output … · Free 1.9 TB · estimate 84 GB       │
│                                   ▶ Run stacking   Cancel   Re-run from ▾  │
├ Pipeline board (62 %) ───────────────────┬ Inspector (38 %) ───────────────┤
│ ● 1 · Calibrate      summary     Ready ▸ │ 5 · Register                    │
│ ● 2 · Debayer        summary     Ready ▸ │ Model            [Homography ▾] │
│ ● 3 · Measure & select …         Ready ▸ │ Distortion       [Off ▾]        │
│ ● 4 · Reference      …           Ready ▸ │ Interpolation    [Bicubic B-… ▾]│
│ ◉ 5 · Register  ▓▓▓▓▓▓░░ 142/208 Running │ Clamping 0.30    Max stars 2000 │
│ ○ 6 · Local normalization [on]   Queued  │ RANSAC 1.9 px    Max RMS 2.0 px │
│ ○ 7 · Integrate      …           Queued  │ ☐ Exclude frame on failure      │
│ ○ 8 · Drizzle [off]  …           Off     │ ☐ Write registered frames       │
│ ○ 9 · Output         …           Queued  │ ▸ Advanced                      │
│ Groups (2): key · camera · colour · …    │ Defaults = WBPP-equivalent      │
├ ▾ Frames (368) ──────────────────────────┴─────────────────────────────────┤
│ filename · group · weight · FWHM · ecc · stars · reg RMS · inliers · status · ☑ │
├ Results (run #12, 2026-09-08 14:07) ───────────────────────────────────────┤
│ [thumb] master name · 208 frames · 2.4 % rejected · noise · SNR gain · Reveal │
└────────────────────────────────────────────────────────────────────────────┘
```

Below 1200 px the inspector drops under the board as an accordion.

### 11.2 Components (`src/components/stacking/`)

- `StackingTab.tsx` — owns the plan fetch (`get_stacking_plan` on mount, on
  config change debounced 300 ms, and on the `library-updated` DOM event),
  the config state (`get/set_stacking_config`, submit state, never a
  re-read), the run state from `useStackingRuns`, and the layout.
- `PipelineBoard.tsx` + `StageRow.tsx` — nine rows; row state ∈ `ready |
  blocked | stale | queued | running | done | skipped | failed`; a running
  row shows the accent bar with `current / total · percent`; optional stages
  (LN, drizzle, registered frames) carry a toggle on the row; the summary
  line is a pure function of the config (`stageSummary(stage, config)`), so
  it never disagrees with the inspector; a blocked row shows the blocker
  inline with the `→ Coverage` link, like the export mode card.
- `StageInspector.tsx` — switch over the selected stage → one panel each:
  `CalibratePanel` (read-only resolved masters per group + the export's
  light-cal options), `DebayerPanel` (info), `MeasurePanel` (weight mode,
  formula sliders when `formula`, PSF model, max stars, the four filters,
  "Re-measure" which invalidates stage 3 artifacts), `ReferencePanel`
  (auto/manual, the chosen frame with its weight, "Choose in Analysis"),
  `RegisterPanel` (as drawn), `NormalizePanel` (output and rejection
  normalization, scale estimator; the LN block with enabled/scale/reference
  frames/PSF model/local scale), `IntegratePanel` (combination, rejection
  method with `ParamPair` inputs and the Auto note stating the resolved
  algorithm per group, min weight, range clipping, rejection maps),
  `DrizzlePanel` (enabled, scale, drop shrink, kernel, the three "use"
  toggles, weight map, disk/time estimate), `OutputPanel` (two `FolderCard`s
  with per-set override vs default, cleanup policy, format). Every numeric
  field uses the two-state numeric discipline from the export tab; every help
  line states the default.
- `GroupsTable.tsx`, `FramesTable.tsx` (sortable; the include checkbox writes
  the manual exclusion list through `set_stacking_config`; status chips
  reuse `getSeverityColor`), `ResultsPanel.tsx` (runs dropdown from
  `get_stacking_runs`, master cards with a static glyph in place of a
  thumbnail until M4 catalogs masters and the existing preview route can
  render them, a stats line, Reveal / Open / Provenance (the run JSON in a
  modal), working-folder usage with "Delete intermediates").
- `stackingPrefs.ts` — only UI conveniences (collapsed panels, selected
  stage) in `localStorage`; the config itself is server-side.
- Hook `src/hooks/useStackingRuns.ts` + `StackingContext.tsx` — modelled on
  `useMasterBuilds` (backend owns admission, events carry `runId`/`setId`,
  one completion per start, `notify()` on completion, `library-updated`
  dispatch); `StackingQueueIndicator.tsx` wraps `QueueIndicator` like the
  registration one it replaces; `ComputeJobKind` gains `stacking` on both
  sides.
- Settings → **Stacking** section: the same inspector panels bound to the
  global defaults, the two default `FolderCard`s, "Reset to built-in
  defaults".
- Notifications: `NotificationKind` gains `stacking` (+ icon); the retired
  `registration` kind stays for stored history.

### 11.3 Removed

`StackingPrepTab.tsx`, `useRegistrationProgress.ts`,
`RegistrationProgressContext.tsx`, `RegistrationQueueIndicator.tsx`, the
`REGISTRATION_ENABLED` flag (replaced by `STACKING_ENABLED =
import.meta.env.DEV` until M1 ships, then removed).

## 12. Web / Docker

Same commands over HTTP, progress over SSE, folder picking through the
`stacking` browser scope, paths checked by `PathPolicy` (`ATHENAEUM_ALLOWED_PATHS`).
Memory: the band budget already reads the cgroup limit; the fan-out admission
in §8 uses the same `total_ram_bytes()`. The image pool is shared with
analysis and blink as today.

## 13. Testing and acceptance

**Unit (every milestone):** kernels (each kernel reproduces a known analytic
sample; clamping rules on a step edge), transforms (each model round-trips
synthetic points with noise and 30 % outliers; RANSAC deterministic; inverse
polynomial error < 0.01 px over the frame), resampler (synthetic Gaussian
stars shifted by known fractions and rotated: recovered centroid error
< 0.02 px, flux within 0.5 %, NaN coverage exact), KD-tree vs brute force,
BWMV and MRS against tabulated values, weighted rejection with masks
(existing recipes byte-identical when all weights are 1 and masks unused —
the master-build fingerprint), rejection maps, PSF Signal Weight on the
synthetic calibration field (median ≈ 1), WCS card writer round-trip through
the parser, config hash stability, artifact reuse and invalidation, cancel
at every stage, headless build.

**Real data (LDN 1272, both groups):**

| Metric | Target |
| ---- | ---- |
| Registration RMS per frame | ≤ WBPP's `delta_RMS` for the same frame (0.28–0.72 px observed) |
| Frames registered | 208/208 and 160/160 |
| Master MRS noise | within 5 % of the WBPP master (same frames, same weights mode) |
| Rejected fraction | 1–4 % (WBPP: 1.4–3.8 %) |
| Artifacts | no residual satellite/plane trail visible at 400 % where WBPP shows none |
| Drizzle 2× (M3) | FWHM ratio drizzled/undrizzled within ±5 % of WBPP's |
| Wall time, whole set, this Mac | ≤ 60 min (WBPP 2 h 37 min) |
| Disk | ≤ 85 GB working folder with registered frames off (WBPP 170 GB) |

**Performance breakdown targets (this Mac, 208 mono + 160 OSC):** calibrate
≤ 8 min (already measured by the export), measure ≤ 5 min, register ≤ 5 min,
integrate ≤ 8 min per group, LN ≤ 15 min per group, drizzle ≤ 15 min per
group.

## 14. Milestones and task list

### M1 — first master light

1. **`FrameSource` trait + 3-plane reads** — `integration/source.rs`,
   `banded.rs` plane offsets; master builds unchanged (fingerprint test).
2. **Resampler** — `resample/{kernels,warp,window}.rs`: 7 kernels, clamping,
   inverse-mapped gather with NaN coverage, band-window computation, tests.
3. **Transform models** — `registration/{models,ransac,kdtree}.rs`:
   similarity/affine/homography/polynomial, forward + inverse, DLT, RANSAC
   with the quality score, σ-weighted refit; `Affine::inverse` upstream.
4. **Registration v2 service** — detection on calibrated frames, reference
   handling, per-frame QA, `registration_results` extension, optional
   registered-frame writer with `ATH_REG` cards; retire the old commands.
5. **Measurement** — `stacking/measure.rs`: PSF flux/FWTM aperture, RCR +
   Winsorization, `M*`/`N*`, MRS wrapper over rustafits, PSF Signal Weight,
   PSF SNR, classic formula, weight modes, selection filters, metrics
   artifacts.
6. **Statistics** — `integration/stats.rs`: BWMV, two-sided scales,
   global normalization pairs.
7. **Combiner v2 + engine** — weights, offsets, survivor masks, rejection
   maps, per-frame rejected fraction, channel loop, `RegisteredSource`.
8. **Headers** — `fits_writer/wcs.rs` (linear + SIP cards from a
   `WcsSolution`/`plate_solves` row), master card set, scanner skip rule.
9. **Run orchestration** — `stacking/{config,groups,plan,run,paths,naming,
   provenance}.rs`, artifacts + hashes, cleanup, events, cancel, logging.
10. **Data model** — the five tables, the `registration_results` columns,
    settings keys, `PathSetting` reuse.
11. **Commands** — `api/stacking.rs`, Tauri + Axum wrappers, `ts_export`,
    route tests, headless gating.
12. **Frontend** — the tab, board, inspector panels for stages 1–5, 7, 9
    (6 and 8 render with their toggles disabled and a "coming in M2/M3"
    note), tables, results, hook/context, queue indicator, notifications,
    Settings → Stacking, removal of the old tab.
13. **Acceptance run** — LDN 1272 both groups against the WBPP masters; the
    metrics table above filled in `docs/superpowers/research/` and the
    open-items ledger updated.
14. **Docs** — `CLAUDE.md` module map + a "Stacking" section, the logging
    dictionary additions, release-note lines.

### M2 — local normalization

Reference build per group, MMT background models, PSF-flux scale with RCR,
`.athln` sidecars, engine hook (output + rejection normalization),
`NormalizePanel` LN block live, acceptance re-run (noise target re-measured
with LN on both sides).

### M3 — drizzle

Kernels and exact clipping, forward mapping, tile scheduler, rejection
bitmaps from M1 turned on, weights and LN application, weight map, scaled
WCS, `DrizzlePanel` live, acceptance (FWHM ratio vs WBPP drizzle).

### M4 — polish

Thin-plate spline distortion (+ local distortion loop), ESD, RCR, min/max,
large-scale rejection, Bayer drizzle (stage 1 keeps the CFA mosaic), XISF
output, cataloging masters (a `master_light` entity linked to the set and a
preview in the Results cards), preset management.

## 15. Deferred and open

- Union/mosaic geometry, comet mode, multi-set stacking: not planned.
- `PSFScaleSNR` weights (needs LN relative scale factors): M2 follow-up.
- Adaptive normalization: not planned (LN covers the use case).
- Registration distortion `tps` smoothing and outlier defaults: our own
  values, to be set from the M4 acceptance run.
- Whether masters should be cataloged and how they appear in the Objects
  page: M4 decision, after the owner has used M1–M3 output for a while.
