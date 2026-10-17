# One Star-Measurement Path — Plan A (module and fixes) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** one star-measurement module (`star_measure/`, `render`-gated) that the stacking Measure stage, LN's refit
and the master statistics consume, with the measure path's defects F-0…F-7 fixed and judged against the truth lab,
the stacking Measure config migrated to `seed_source`, `MEASURE_VERSION` in every artifact hash, and one real-data
checkpoint at master level. Plan B (F1, the c = 1 floor) and Plan C (the Analysis switch-over) build on it; every
interface Plan C consumes is named here (§"Interfaces Plan C consumes").

**Architecture:**
- **athenaeum** (`crates/athenaeum-core/src/star_measure/`): `domain`, `seeds`, `fit`, `beta`, `photometry`,
  `frame`, `budget`, `diagnostics`, `robust` (moved from `stacking`). Public surface `measure_plane(plane, w, h,
  opts, pool) -> PlaneMeasurement`, `fit_stars_at(plane, w, h, seeds, beta, opts, pool) -> FitOutcome`,
  `MEASURE_VERSION`. `stacking/measure.rs` becomes the adapter (reads planes, builds `MeasureOptions`, keeps
  `ChannelMeasurement`/`FrameMeasurement`/`.athf`); `stacking/psf_signal.rs` keeps the PSFSW/PSFSNR formulas,
  `group_beta` and re-exports the fit types so no consumer import changes.
- **rustafits** (submodule, its own repository): one branch `one-star-path-beta` with F-4's bounded free-β entry
  point in ln β; athenaeum bumps the submodule pointer. The convergence-test change is conditional (Task 5) and,
  if triggered, is a NEW entry point or parameter (ruling F7): every existing entry point stays byte-identical.
- **athenaeum-lab** (`star-lab/`): new families, truth columns and cards, the probe plan for the new arms, the
  scorecard rows the bars read, and `bin/scorecard_diff.py`, the mechanical no-regression check.
- **Data**: families under `~/.athenaeum-acc/star-lab-v2/` (the one manifest `manifests/v2.json`); every Plan A
  probe run and scorecard under `~/.athenaeum-acc/star-lab-a/<step>/` (ruling PA-3).

**Tech stack:** Rust (athenaeum-core, rustafits), Python 3.14 + numpy 2.4 (lab, `unittest`), React/TS (the Measure
panel), `sqlite3` with `immutable=1` for reading acceptance copies, the athenaeum-lab acceptance harness
(`harness/acceptance/`).

**Spec:** `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` (HEAD 61a03558, branch
`one-star-path`). Review history: `.superpowers/sdd/2026-10-07-one-star-path-spec/spec-review.md`,
`plan-a-review.md`; the pre-flight scan and its binding rulings F1…F45:
`.superpowers/sdd/2026-10-07-one-star-path-plan-a/{preflight-scan.md,progress.md}`. Code maps:
`measure-path-map.md` and `analysis-consumers.md`. Baseline:
`docs/superpowers/research/2026-10-07-star-measurement-baseline.md` (lab e65af49, probes from f5321979).

## Global Constraints

- No third-party program name anywhere in this repository (code, comments, docs, commit messages); the comparison
  program is "the ERP". Lab automation lives in athenaeum-lab only.
- Never read or write the production catalog; the owner's acceptance copies under `~/.athenaeum-acc/` are opened
  only as `sqlite3 "file:…?immutable=1"` (every harness script included, ruling F19); working copies via
  `prepare-catalog.sh` or Task 13's clone recipe, and only those are written.
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `calibration_library/light_cal.rs`,
  `stacking/measure.rs`, `stacking/register/detect.rs`; no `cargo fmt`. Those files take hand edits only; `rustfmt`
  runs only on the new `star_measure/*.rs` files, each named by explicit path.
- Cargo with `CARGO_TARGET_DIR=.superpowers/target-acc`, `CARGO_BUILD_JOBS=4`, `--test-threads=4`; one heavy job at
  a time with `memory_pressure` ≥ 30 % free; stop below 100 GiB free disk. `cargo test` takes ONE positional filter
  (ruling F15): every multi-module run is written as `cargo test -p athenaeum-core --lib -- <common prefix>
  --test-threads=4` or as one invocation per filter.
- The full `cargo test -p athenaeum-core --all-targets` before any push.
- `tracing` only; never swallow an error; a refused star is a counted, reasoned outcome, not a silent drop.
- Two backends in sync if any command surface changes (none planned); serde snake_case ↔ camelCase verified against
  `src/types/models.ts`.
- Every ruling and acceptance run is recorded in `docs/stacking/README.md`; `docs/settings/README.md` and
  `docs/platesolving/input-and-acceptance-gates.md` change with their sections.
- Lab code is committed in athenaeum-lab (`main`) and pushed (`git push origin main`) after each task.
- athenaeum is committed on `one-star-path` and NOT pushed.
- Commits are authored as `eg013ra1n` / `vilen.sharifov@gmail.com`, every message ending with the trailers
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg`.
- The measurement doctrine (spec §3 preamble): every bar is fixed before anything is measured and never moved; a
  missed bar goes to the owner with its numbers; synthetic truth is the referee; the ERP is a comparison column
  only.
- One heavy job at a time (a cargo build, a probe batch, a scorecard build, a stacking run, a generator run); the
  lab runners already wait on `pgrep` and `memory_pressure`.
- `memory_pressure | tail -1` ≥ 30 % free before every heavy step.
- Disk stops below 100 GiB free on `/Volumes/BigMac` (`df -k`), checked before every step that writes.
- The lab data root `~/.athenaeum-acc/star-lab-v2/` stays at most 10 GB (9.1 GB today; the new families add about
  0.3 GB); Plan A's probe outputs and scorecards never go there — ruling PA-3 gives them their own root.
- The probe binaries (`star_probe`, `ln_scale_probe`, `measure_probe`) are rebuilt only by the task that changes the
  code they measure; every output's run record carries the sha256 of the binary that wrote it (`run_probe.py`'s
  provenance record) and `run_probe.py --verify` proves it before a scorecard is believed.
- rustafits public remotes (`origin` GitHub, `gitlab`) are pushed only by the owner; the branch stays local until
  the owner pushes it. A rustafits change beyond Task 9's named one happens only when a lab number triggers it
  (Task 5 step 7), and then as a new entry point or parameter (ruling F7). Push order (M-7): the owner pushes
  rustafits `one-star-path-beta` to both remotes BEFORE athenaeum `main`, or the GitHub run (`submodules:
  recursive`) cannot fetch the gitlink.
- Every test a task tells the implementer to write is written in full with its assertion (ruling F29); a reviewer
  rejects an empty or comment-only body. A test that cannot fail before its change is labelled a pin and sits
  outside the RED step (ruling F28).
- Each task updates the earlier tests its interface change breaks and names them (ruling F14); the review checks it.

## Review Focus

1. **Planes with NaN or zero-padded borders after registration** (`resampled/*` and every registered frame LN
   refits through `fit_stars_at`; `register/frame.rs` reads `.athf` of calibrated planes, LN of warped ones). The
   matched filter must treat a non-finite pixel as absent (no NaN propagation into the convolution, no seed on a
   border), the region growth skips non-finite pixels as today, the rim background start and the SNR annulus ignore
   them, and a star whose aperture leaves the finite area is refused `OutsideImage`. Pinned in Task 6a
   (`nan_border_seeds_nothing_on_the_border_and_fits_every_interior_star`), Task 6b
   (`border_limited_region_refuses_a_broad_star_region_too_small`) and Task 8
   (`annulus_sigma_is_the_mad_of_the_present_pixels`, the annulus the SNR reads, I-2; the saturation test reads
   the plane's σ since PA-23); the probe's `nan_border_runs_every_path` keeps passing at every task.
2. **Very sparse frames under the β sample's minimum** (fewer than 8 free-β fits on unsaturated stars: a 3-star
   frame, a flat field, a frame whose bright stars all saturate). β falls back to 4.0 with a `warn!`,
   `PlaneMeasurement.beta_source = BetaSource::Fallback`, and the frame still measures. Pinned in Task 9
   (`beta_sample_below_eight_falls_back_to_four_and_says_so`) and Task 4 (a starless plane: zero seeds, zero
   outcomes, no panic — `starless_plane_has_no_outcomes_and_no_panic`).
3. **An all-saturated core field** (`saturated/f2.6/n60`'s 64 brightest, a deep twin with 235 of 629 clipped, a
   plane whose every bright star is flagged). The β sample, the aperture correction and the frame statistic use the
   unsaturated subset (the field width reads every candidate and no verdict — PA-17, ruling F25 as folded); when the
   subset is empty the frame statistic is `None` → `fwhm_px = 0` with
   `stars_unsaturated = 0` reported, never a statistic over clipped cores. Pinned in Task 8
   (`all_saturated_sample_leaves_the_frame_statistic_empty_not_wrong`) and Task 10
   (`aperture_correction_without_an_unsaturated_isolated_star_is_one_and_flagged`).
4. **A plane whose noise estimate is refused** (MRS at its floor on a constant or near-constant plane; `N*` under
   100 residuals). The matched filter's threshold has no σ: the detector returns no seeds with one `warn!`, the
   SNR takes `σ_local` from the star's own annulus (never the plane σ), the saturation test's threshold reads the
   plane σ (PA-23; `sigma` is 0 here, so only given seeds reach the test, with one `warn!` per plane, Task 8 Q1), and
   `NoiseModel.source = NoiseSource::None` is reported. Pinned in Task 6a (`refused_noise_estimate_seeds_nothing_and_warns`).
5. **A star at the image edge** (the width-start box partly outside; the region cap clipped by the border; an
   aperture or annulus crossing it). Admission refuses `OutsideImage` when the width-start box leaves the image;
   a region clipped by the border fits only when its radius still holds `0.5·growth·FWTM` (reachable only for a
   star broader than the field width — PA-18 gives the geometry); the SNR annulus uses the pixels present and
   reports `n_sky`. Pinned in Task 4 (`edge_seed_is_refused_outside_image_with_its_reason`), Task 6b
   (`a_star_whose_width_start_box_leaves_the_image_is_outside_image`,
   `border_limited_region_refuses_a_broad_star_region_too_small`), Task 8
   (`annulus_sigma_is_the_mad_of_the_present_pixels`) and Task 11 (`edge_star_snr_uses_the_present_annulus_pixels`).
6. **A stored stacking config from each older shape** (pre-M4a: no `detectionSigma`; M4a: `detectionSigma` +
   `seedPrefilter`; M4c: + `seedDetector`; a user preset; the stored global document; Structure with Median3). One
   decode maps each to `seedSource` per spec §2.2, whole-config precedence kept, no value reinterpreted. Pinned in
   Task 7 (`every_older_measurement_shape_decodes_to_the_spec_seed_source`, six fixtures).

## Plan rulings (where the plan has to be more precise than the spec)

- **PA-1 Names.** The module's result is `star_measure::PlaneMeasurement` (the spec's name); the adapter keeps its
  own `stacking::measure::PlaneMeasurement { channel, fits }` (the probe and the run name it). The two are never
  glob-imported into one scope. The per-star record stays `StarFit` (its `.athf` layout, LN, Register and the
  probe all name it) and gains `flags`, `snr`, `hfr`; the spec's `StarRecord` is this type.
- **PA-2 The matched filter's threshold** `k` (in units of the filtered plane's measured σ_f) is chosen BEFORE any
  completeness or fitted/seeded bar is read, by a false-positive criterion on data no bar names: over the 46
  `twins/*` families run whole (`run_probe.py --families` is family-level, ruling F37; the row aggregates per
  family × path × plane, so the criterion is per family) and `artefacts` (its hot/cr rows exist), `k` is the
  smallest of {3.0, 3.5, 4.0, 4.5, 5.0, 6.0} at which the scorecard's `false detections / Mpx` cause `nothing` (a
  seed that matches no truth star, artefact or star wing) stays ≤ 0.5 per Mpx on every family, read on the `seeds`
  path (Task 6a, before any fit change). The 7 `background/*` images carry F-6's and F-7's bars, so they do not
  decide (M-9); they are printed beside the sweep as a reported column, because nebula knots are the false-positive
  regime the threshold must survive. The chosen value is recorded in `docs/stacking/README.md` (ruling R-OSP-2) and
  in `seeds::DEFAULT_THRESHOLD`'s doc comment with the table it came from; the bars are measured after.
- **PA-3 Data roots.** New families are generated into `~/.athenaeum-acc/star-lab-v2/` (one manifest, one
  determinism gate). Every Plan A probe run, scorecard and gates scratch goes to `~/.athenaeum-acc/star-lab-a/<step>/`
  (`run_probe.py --out`, `scorecard.py --probe-dir`, `STARLAB_SCRATCH`), where `<step>` is the task's tag
  (`t01c-baseline`, `t02-extract`, `t04-f0`, `t06a-seeds`, `t06b-f1`, …). At most two `<step>` directories exist
  at a time: a step's probe directory is deleted once its full scorecard is gzipped under
  `star-lab-a/results/<step>/` and its summary is committed. `star-lab-a` is capped at 4 GB.
- **PA-4 `MEASURE_VERSION = 4`** continues `PSF_FIT_VERSION`'s count (3 → 4) and is set once, in Task 3. Plan A bumps
  it no second time: the branch merges as one unit, and every acceptance run Plan A makes starts from Measure on a
  clone whose Calibrate artifacts carry no `MEASURE_VERSION` (Task 13). Policy: bump once per merged plan whose
  code changes what a fit accepts or measures (spec §4.4); Plan C sets 5; a plan that changes no fit leaves it.
- **PA-5 F-5's flux lands in `StarFit.signal`** (the same `.athf` slot, `FITS_ARTIFACT_VERSION` unchanged); `area`
  keeps its FWTM-ellipse meaning, so `mean_flux()` stays monotonic in the flux. Register's flux ordering and
  LN's seed ordering read `signal` as before.
- **PA-6 `fit_stars_at`** returns the extended `FitOutcome { fits, beta, seeds, refused: RefusalCounts }` (Task 6a
  adds `field_width`, Task 6b `diagnostics`, Task 10 `aperture_correction` — each declared in its task's
  Interfaces, ruling F14); LN keeps calling `fit_stars`/`fit_stars_with_beta` through `psf_signal`'s re-exports,
  which are thin wrappers over it.
- **PA-7 The rustafits change** is a NEW entry point `fit_moffat_2d_bounded(pixels, init…, beta_bounds: (f64, f64),
  max_iter, conv_tol, max_rejects) -> Option<Moffat2DBoundedResult>` whose eighth LM parameter is `ln β`, with the
  acceptance bound `beta_bounds` in place of the fixed `[1, 20]`. The solver takes `BetaParam::{Fixed(f64),
  FreeLinear, FreeLn { ln_min, ln_max }}` (ruling F8): `FreeLinear` IS today's free-β arm (`params[7] = β`, step
  clamp `[1.5, 20]`, acceptance `[1, 20]`, `j[7] = A·power·(−ln base)`) and `fit_moffat_2d` keeps selecting it, so
  `fit_moffat_2d` and `fit_moffat_2d_fixed_beta` stay byte-identical (their callers in `metrics.rs`, the debug binary
  and the pins keep their bytes; a golden test pins it). The result carries `iters`. No rustafits crate version bump
  in Plan A (a path dependency; `Cargo.lock` unchanged); a release is the owner's `rustafits-release` procedure.
- **PA-8 The flat-top test's constants** *(superseded by C-a, owner 2026-10-09: the test is PA-23. F9's wing-fit
  wording below goes with it; F9's order is kept in PA-23. The text stays as history.)* are fixed a priori, not
  measured: core = the pixels within 2 σ_local of the
  observed maximum inside a radius of `max(1.5, 0.5·FWHM_start)` px; the star is flagged saturated when that core
  holds ≥ 3 pixels AND the free-β wing fit (on the pixels outside the core, ≥ 12 of them) predicts, at the maximum's
  position, a value above the observed maximum by more than `max(3 σ_local, 0.01·observed maximum)`. The 3 is the
  bar's own `k`; the 1 % floor is half the generator's "within 2 % of the clip" gray zone. The verdict is computed
  PER SEED on the seed's own region, after the field width is known (the region's `F_start` reads `F_field`, PA-12)
  and before the β sample and the frame fits — order: seeds → field width → verdicts → β sample → fits at β
  (ruling F9); it takes the seed, its region, its peak and its width start, never a `StarFit`; the post-fit flag
  only copies it. One rule, and only one: the field width (PA-17) runs no saturation test at all — it is the
  median over every one of its candidates, saturated ones included — so no partial second rule exists (ruling F25
  as folded; cost if wrong: a field width biased on frames dominated by saturated stars, which the lab's
  `saturated/*` families show). The wing fitter is `fit_moffat_2d` (free β, bound [1, 20]) at Task 8 and
  `fit_moffat_2d_bounded` with `BETA_BOUNDS` from Task 9 on (I-3); a wing fit that returns `None` leaves the star
  UNFLAGGED with `SaturationVerdict.wing_fit = false` reported — never a flag from a missing fit. `σ_local` is
  `photometry::annulus_sigma` (Task 8, PA-11 radii).
- **PA-9 Donut family (ruling F1).** Profile `donut<R>`: an ANNULUS — a uniform disc of outer radius R px with a
  central obstruction of radius 0.4·R (`disc(R) − disc(0.4R)`, normalised to unit flux) — convolved with a Gaussian
  of FWHM 2.6 (a new `render.Component` kind `annulus`, `a = R`, `b = 0.4R`, pixel-integrated through
  `render_star_blurred`). Truth kind `donut` (not a width kind; no `fit_*` widths, no CRLB). Its row is "donut
  outcome": the share of matched donut stars refused (by reason) against the share accepted — the bar is "refused
  with its reason, never reported as a narrow fit": accepted donut fits = 0. The dip test reads the profile at
  r = 0 (inside the obstruction) against r = 0.7·R (on the ring).
- **PA-10 The budget selects before fitting.** The seeds are ranked by matched-filter response; the 4 × 4 cells
  take `ceil(1.25·N/16)` brightest candidates each (round-robin by rank when a cell holds fewer), the candidates
  are fitted, refused ones drop, saturated ones are accepted with their flag, and the result is trimmed to N by
  SNR round-robin over the cells. Every seed has an outcome in every mode (ruling F13): `Outcome::NotSelected`
  for a seed the budget never fitted, `Outcome::Trimmed` for an accepted fit the trim removed; the identity
  `accepted + refused + not_selected + trimmed = seeds` holds in `Mode::All` (both new counts 0) and `Mode::Budget`.
  `BudgetReport { cells_with_stars, cells_covered, candidates, kept, not_selected, trimmed }` is reported; the bar
  reads `cells_covered / cells_with_stars ≥ 0.9` (a cell "holds measurable stars" when any seed above threshold
  lies in it).
- **PA-11 SNR geometry (F-6).** Aperture radius `max(1.5·FWHM, 3)` px about the fitted centre; sky annulus
  `[max(3·FWHM, 6), max(3·FWHM, 6) + max(FWHM, 3)]` px; σ_sky = 1.4826·MAD of the annulus; `N_sky` its pixel count.
  The truth column `snr_ap` uses the same radii from the star's `fit_fwhm` geometric mean — the same quantity the
  module's fitted `fwhm()` estimates (D-2) — AND the same `(1 + N_ap/N_sky)` sky-estimate term counted on the truth
  grid (ruling F5: it is the estimator's own expected noise; N_ap/N_sky is 0.27 at F 2.6 and 0.32 at F 8, so leaving
  it out would make a sky-limited star read 11–13 % low by definition). The F-6 bar then measures the estimator.
- **PA-12 Region growth (F-1).** `r_start = max(3, ceil(1.5·F_start))`, `r_max = clamp(ceil(6·F_start), 6, 96)`
  (and never past the border); `m_r` the square's median, `s_r` the median of its outer 1-px rim; grow while
  `(m_r − s_r) > 0.01·(max − s_r)`; the background start `b0 = s_r` of the final square; `F_start` from the star's
  own background-subtracted second moments in a box of radius `2·F_field` (the matched filter's field width, or
  `seeds::field_width_of` for given seeds — I-4, PA-17), clamped to [0.6, 40] px.
- **PA-13 The real-data "before" trees** are `tierA-lnB-f1` (set 204, run 5), `lnB-runB-f1` (run B, run 6) and
  `hc11B-f1` (Horse C11, set 182, run 5, pinned frame 69845) — read through their CLONES after the clone's WAL is
  checkpointed (ruling F19: `hc11B-f1` carries a 6.8 MB un-checkpointed WAL, so an `immutable=1` read of the
  original shows run 5 as `running`) — PROVIDED a Measure-equivalence check passes: on three calibrated frames of
  each tree, local main's `measure_probe` output equals the tree's stored `metrics` payload field for field. If it
  does not, the before run is re-made on the same clone recipe with the main worktree's build (`clone-before.sh`
  + `tier1-run.sh`/`project-run.sh`/`hc11-run.sh` from `measure`, ruling F42), the disk it costs named first.
- **PA-14 LN scale rows are not judged by no-regression in Plan A.** Row 6's `arm *`, `sLib`, their slopes, and the
  injected LN pairs read the fitted `fwhm()` (F1's common F) and `signal` (psfSel), both of which Plan A changes by
  design; spec F-8 re-baselines them in Plan B. `scorecard_diff.py` prints them in their own table, never counts
  them. The Plan B re-baseline starts from Task 14's scorecard. Task 13's LN-scale row is reported, not a bar
  (ruling F23 e).
- **PA-15 The legacy analysis arm is the control.** The rustafits analysis path (`--paths legacy-analysis`, added
  to `star_probe` in Task 1b as an alias of today's `analysis` path — JSONL `path: "analysis"` unchanged through
  Plan A; an unknown `--paths` name is a non-zero exit, ruling F2) is re-run at every step on the same u16
  siblings; its rows must be bit-identical to the previous step's — a non-identical control row means the inputs
  or the tool moved, and the step stops. The control's own run-to-run determinism is proven once, at Task 2 step
  5 (I-7): the same binary twice on three u16 images, both outputs NON-EMPTY (≥ 1 star line) and `cmp` equal; if
  not equal, the control compares with a tolerance of 0 on counts and 1e-9 relative on floats, recorded in
  `results/plan-a/t02-extract/control-determinism.txt`. The module's Analysis mode is the new arm
  `analysis-mode`; Plan C swaps the labels.
- **PA-16 Each fix step's lab cycle** is: build the probe → `run_probe.py manifests/v2.json --out
  ~/.athenaeum-acc/star-lab-a/<step>/probe` (with `STAR_PROBE_ARGS="--diagnostics"` from Task 4 on, ruling F22) →
  `--verify` → `scorecard.py` → `scorecard_summary.py` (committed) → TWO diffs (ruling F21): `scorecard_diff.py`
  against the previous step's gzipped JSON with the step's own design list (`noreg.txt`: the step's own change)
  AND against `t01c-baseline` with the union of every design list so far (`noreg-cumulative.txt`: a non-design
  row that drifted beyond its baseline SE cumulatively stops the step) → the task's own bar rows. Injections
  (`manifests/inject.json`) run at Tasks 6b, 9 and 14 (the raw-mode bars) and the corpus at Task 12 (cost).
- **PA-17 The field-width estimator (ruling F16).** `seeds::field_width_of` and the matched filter's second-pass
  width both use the azimuthal half-maximum profile of the star, NOT second moments: on the background-subtracted
  stamp (background = the median of the box rim), pixels are binned by distance from the brightest pixel in 0.5-px
  rings, the ring means form a radial profile, and the FWHM is twice the radius where the profile crosses half of
  the brightest pixel's value (linear interpolation between the bracketing rings); the field width is the median
  over the ≤ 100 brightest seeds, ALL of them — saturated ones included: the estimator runs no saturation test
  (ruling F25 as folded; the per-seed verdict of PA-8 comes after it) — clamped to [1.5, 24] px. The box is 15 px
  half-width on the first pass and
  `3·F` (≤ 48) when the first pass reads ≥ 7 px. Verified by computation (the lab's own renderer, noiseless,
  phase (0.3, −0.2), against the `pointfit` truth): Moffat-4 F 5 +1.8 %, F 8 +0.7 %, F 16 +0.2 %; Moffat-2.5 F 5
  +2.2 %; Moffat-10 F 5 +1.5 %; Gaussian F 5 +1.2 %; at F 2.6 every profile reads +4.6 … +7.7 %; at F 2.0 +6.7 …
  +10.7 %; at F 1.2 +13.9 … +25.7 % (the pixel-integrated peak is low, so the half-maximum radius is wide — the
  fit resolves it; the field width is a start, never a measurement). Noisy Moffat-4 F 5 at peak SNR 50: median
  +1.3 %, p90 |error| 4.3 % over 30 draws. Second moments were rejected by the same computation: +19.1 … +20.5 %
  on Moffat-4 (box radius 1.5 F) and +26 … +33 % on Moffat-2.5, the wings; the matched filter's best-scale
  response read +8.4 … +9.4 % on Moffat-4 and +16 … +17 % on Moffat-2.5. The unit test's bar is the estimator's own
  10 % on a Moffat-4 F 5 field (expected +1.8 %, within 2.5 % under the test's noise).
  *Revised by Task 10b A2 (ruling A5, 2026-10-11) — which reads the median takes, not how one read is made:* the
  field width is the HALF-SAMPLE MODE of the half-maximum widths of EVERY candidate (the first pass's; for
  `field_width_of`, every given seed) whose read is READABLE — its brightest pixel at least 10 σ_rim above the read
  box's rim median, the width start's own gate (Task 6b R1, `WIDTH_START_MIN_PEAK_SNR`) — clamped to [1.5, 24] px.
  No count, no ranking (the given seeds' `peak` is no longer read), no saturation test (F25 stands: no second
  saturation rule). Derivation: every contaminant of a half-maximum read is WIDER than the PSF (a clipped core is
  read at half the clip, outside the PSF's half maximum; a blend, a knot or a sky bump is a broader profile) while the
  stars share one width, so the PSF is the reads' mode; the mode estimator is parameter-free (Bickel's half-sample
  mode). Chosen on the design run `t10b-a2-design` (the first pass's candidates dumped with their reads; 9 clean real
  frames — M42 mono2 0000/0008/0010, hc11B L 0000/0008, G 0001/0002, LDN 1272 D 0099 and mono 0000 — and 45 lab
  families; reference = the median read of the frame's good stars at read SNR ≥ 50: accepted unflagged fits, lab truth
  stars): the rule through Task 10 (median of the 100 brightest) read 1.04–4.43× the reference on the real frames
  (M42 4.0–4.4, LDN 1272 3.3, L 0000 2.0, D 0099 1.7, L 0008 1.6) and up to 1.48× on the lab (`density/d1000`,
  `density_pairs`, `neb3_4096`, `saturated/*/n60`; 12 cells beyond 10 %); the near-clip exclusion (median of the 100
  brightest not within the plateau-clip band) 1.02–1.24 (5 cells, and it needs the saturation plane given seeds
  lack, I-4); the mode of every readable read 0.92–0.99 real, 0.93–1.02 lab (none beyond 10 %). Recorded cost: the
  readable tier at SNR 10–20 reads 3–20 % narrow, so the mode sits 1–8 % under the bright reads on real frames (M42
  0.92–0.94, L 0000 0.93, G 0001 0.94). A median of the reads at SNR ≥ 20 or ≥ 50 read 0.98–1.03, but its threshold
  is not derived and its breakdown is half the bright tier — the clipped-hundred truth test's construction; not
  taken. Truth tests: `field_width_reads_the_stars_width_when_the_brightest_hundred_are_saturated`,
  `…_on_structured_sky`, `field_width_of_given_seeds_follows_the_same_rule` (old rule 4.40 / 10.00 vs the stars'
  2.76 / 3.30). M42's injected stars read 2.66 against the frame's own stars' 2.13: the injected PSF is broader than
  the frame's.
- **PA-18 The border test geometry (ruling F17).** `RegionTooSmall` at the border is reachable only for a star
  broader than the field width: the test puts one Moffat-4 star of FWHM 12 in a field whose width is 3 px, 10 px
  from the border — its width-start box (radius `2·F_field = 6`) fits, its region is clipped at r = 10, and
  `0.5·FWTM = 0.5·12·2.028 = 12.2 > 10` → `RegionTooSmall`. The `OutsideImage` case is separate: a star 4 px from
  the border in the same field (box radius 6 > 4).
- **PA-19 Design lists (ruling F20).** An entry is `{"row": int, "metric": str?, "family": glob?, "path": glob,
  "kind": "set-aside" | "one-sided" | "reported", "keys": [str]?}`; `path` is required. Matching: the most specific
  entry wins — specificity counts the matched keys (exact `family` > `family` glob; `metric` given > absent; `row`
  always given), ties go to the later entry in the list. Kinds: `set-aside` (judged by the task's own bar, printed
  in the design table), `one-sided` (each field the entry's `keys` names is judged on its own: a `bias…` key passes
  when `|head| ≤ |base| + SE_base`, a `scatter…` key when `head ≤ base + SE_base`, the SE that of the key's own
  axis — `scatter/√n`, or `scatter_x/√n` for `bias_x`/`scatter_x`; `keys` defaults to `["bias", "scatter"]`; a
  one-sided entry over a row that lacks any of its keys — `beta sensitivity`, `auto beta pick`, or a row-2 row
  under the default keys — is rejected by the tool at load time), `reported` (printed, never judged). Row 2's
  `centroid` rows carry `bias_x`, `bias_y`, `scatter_x`, `scatter_y` and no `bias`/`scatter`, so every row-2
  one-sided entry names those four: `"keys":["bias_x","bias_y","scatter_x","scatter_y"]` (ruling F10 as folded);
  row 3's `kernel growth` carries `scatter` alone and names `"keys":["scatter"]`. Paths: the betas arm's rows are
  `betas@2.5|4|6|10` — only row 3's `beta sensitivity` is on `betas` — so a list names the arm `betas@*`, and every
  `measure` entry on rows 3–7 and 11 is listed again on `betas@*` for the metrics that arm carries (the arm fits
  the same seeds through the same `fit_one`; ruling F20 as folded). The union file for the cumulative diff
  (`design-cumulative.json`) keeps each step's entries tagged with their step; when two steps' entries match one
  row, `set-aside` from any step wins over `one-sided` (a later step moved the row by design), and `one-sided`
  wins over SE. So Task 5's one-sided rows 3/5 are judged one-sided only until Task 6a sets them aside by its
  new seed population — stated here so M-13's "never set aside" reads as "never silently set aside".
- **PA-20 The bars as the spec wrote them (ruling F23).** (a) F-1's "frame FWHM error vs `pointfit` ≤ 1 % at every
  FWHM up to 16 px" is read on `sharp/m4/f{2.0,2.6,3.5,5.0,8.0,12.0,16.0}` AND `sharp/gauss/f{2.0,…,16.0}` at Task 6b;
  the `gauss` cells are expected to miss there by construction (β is still discrete: a Gaussian at β 10 reads
  −3.04 %, the spec's own figure) — a miss goes to the owner with the cells and the F-4 mechanism, and Task 9 reads
  the same rows again. (b) F-7's "≤ 0.3σ under nebulosity" is read on every `background/neb*` family (`neb0.1`,
  `neb0.5`, `neb3`, `neb3_4096`); "within ±0.1σ on flat fields" on `background/flat`; `grad10` and `vig20` are
  reported. (c) F-5's spread is `(max − min) / min` of the captured-fraction medians — the baseline's own
  definition (18.73 % = 0.9078/0.7646 − 1). (d) F-1's "fitted / seeded ≥ 99 %" is `matched fitted ÷ matched
  seeded` = the `measure` completeness cell over the `seeds` completeness cell on the same denominator (baseline
  82.7 / 92.0 = 89.9 %, derived; the spec quotes the two cells). (e) Task 13's LN-scale scatter is reported, not a
  bar (PA-14). psfSel's "±1.2 %" (ruling F24) applies to the pure Moffat and Gaussian pairs — every `pairs/*`
  family whose grid profile is a single Moffat or Gaussian, with or without the `w10` wing: `pairs/iso/{m4,g}`,
  `pairs/main/{m4,m25,g,m4w10}` and their siblings of the same profiles; the mixture pairs (`*mix*`, e.g.
  `pairs/main/m4mix`, −1.60 % at baseline) are judged no worse than the baseline beyond SE; the blur pairs carry the
  spec's ±2 %.
- **PA-21 One decode of the plane's domain (ruling F4).** The sample statistics (`stats::stratified_sample`, the
  clip, median/MAD/avg-dev, `location_scale`, `noise_scale_factors`) move into the module as `SkyLevel` in Task 2;
  `MeasureOptions.scale_estimator` carries the estimator the adapter keeps in lockstep with normalization; the clip
  range comes from `Domain`: `Calibrated` → `[CLIP_LO, CLIP_HI]` in unit domain (today's), `RawAdu { clip }` →
  `[1, clip − 1]` ADU. Stacking stays bit-identical (same functions, same sample, same clip).
- **PA-22 The calibrated-frame gain (ruling F6).** `g = EGAIN × ATH_CSCL` electrons per unit; the per-pixel flat
  factor `F(x, y)/ATH_CFNM` of P-21 is taken as 1 because Measure holds no flat — documented on the adapter, and
  the vignetting error it leaves (the flat's own range, up to the corner ratio the pedestal warning measures) is
  reported in the SNR row's note, not hidden. The lab writes `ATH_CFNM = 1.0` so the approximation is exact there.
- **PA-23 The saturation test (supersedes PA-8; owner rulings 2026-10-09, controller rulings C-a…C-g in
  `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-8-handoff-spec.md`).** PA-8 failed on lab truth (research
  R1: FN 60 % on exact ties, 97 % under 1 % flat scatter). The owner adopted an established stacking program's
  method as is; its lab check (research R2) flags ≥ 99.9 % of the clipped stars on exact ties, 1 % flat scatter and
  30 % vignetting. Spec §2.1 is the one definition; this ruling adds the plan-level precision.
  - **Substitutes (C-a).** Three, all in working units. bg = `SkyLevel.median` scaled to working units: the module's
    sampled, clipped sky median (PA-21), the substitute for the reference's full-plane median (the module computes no
    other). σ = `NoiseModel.sigma` (MRS; `N*` when MRS refused; 0 under `NoiseSource::None`, with one `warn!` per
    plane when seeds are given, Q1: with peak or structure seeds the threshold then falls to bg and every neighbour
    counts). norm = the plane's full scale in working units: `ADU_SCALE` for `Domain::Calibrated`; for
    `Domain::RawAdu` the stored integer type's full scale (2^bits − 1 from BITPIX/BZERO: 65535 for unsigned 16-bit,
    255 for 8-bit; never DATAMAX), a NEW field on `Domain::RawAdu` — `clip` keeps its readers (Q2); the plane max is
    read over the same working copy. pixel0 = the pixel at the rounded seed centre (`fit_one`'s `(cx, cy)`): for the
    matched filter the rounded centroid of the box about its local maximum, at most a pixel from that maximum on a
    plateau; for given seeds the caller's position. Its 8 neighbours are read from the seed's region; an absent
    neighbour is not above the threshold. Cost if wrong: FN/FP move away from R2's numbers; Phase C measures it
    against the bar.
  - **The verdict fit (C-a, C-b).** Every seed with a region gets one, as the reference method fits every
    candidate. It is `fit_moffat_2d` (free β, bound [1, 20]) at Task 8 and `fit_moffat_2d_bounded` with
    `BETA_BOUNDS` from Task 9 on. At Task 8 `fit_moffat_2d` still carries the legacy θ column (no conditioned twin;
    only `fit_moffat_2d_fixed_beta_conditioned` carries the corrected one, R-OSP-3); the verdict reads A and FWHM only,
    and R2's port used the same legacy solver, so its numbers stand. Task 9's bounded fitter carries the corrected
    column. It runs on the region's pixels below the level, from the module's own starts
    (PA-12: `b0`, `F_start`, the seed centre), with `3 × FitParams::max_iter` when any pixel is excluded. With
    ≤ 8 pixels left (the fit's 8 parameters) the seed is refused `RefusalReason::MostlySaturated`. Flag =
    A > range. A verdict fit that fails for any other reason (bounds, divergence) gives no flag and no refusal
    (Q4): `SaturationVerdict.fit = false`, the seed is out of the β sample and the payoff, and the probe prints it so
    Phase C counts it (R2: 0 bounds-rejected fits among the saturated). n ≤ p (≤ 8) is pinned as the reference does
    (Q5). Phase A reports the Measure-stage wall time on one real frame before and after; any restriction
    for speed goes to the owner with those numbers (cost if wrong: a slower Measure).
  - **Order (ruling F9, kept):** seeds → field width → verdicts → β sample → fits at β. The field width (PA-17)
    runs no saturation test (ruling F25 as folded). The unflagged bright seeds' verdict fits are the β sample:
    Task 8's discrete search takes the 64 brightest unflagged seeds, Task 9 takes their free β (C-b). Every star is
    then fitted at the frame β; only a FLAGGED star (own or cross-plane) carries its verdict fit's exclusion (Q6 as
    revised 2026-10-09).
  - **Flagged stars (C-c)** stay in `.athf` with `SATURATED`; the β sample and the frame statistic read unflagged
    stars only (F-3); the field width reads every candidate (F25) and the per-star width start is the star's own
    (Q8). The at-β fit applies the verdict fit's exclusion to FLAGGED stars only (Q6 as revised 2026-10-09 after the
    lab: censoring unflagged plateau stars cost 218 truth-unsaturated stars their width): an unflagged star excludes
    nothing and is byte-identical; a flagged star without a plateau excludes pixels ≥ norm. On a multi-plane frame or
    master the flags are united across planes before any β sample, statistic or at-β fit (owner ruling 2026-10-09;
    a `MostlySaturated` refusal spreads too; spec §2.1 "Multi-plane frames"). Registration's
    `passes_register_cuts` (`detect.rs:183–186`) still holds since censoring raises the fitted peak; the parity gate
    checks it. The known limit (small-core stars fitted uncensored, FWHM +31 %, no consumer) is spec §2.1's.
  - **Bar (C-d).** Row 10: FN ≤ 0.5 % among truth-saturated stars with `clipped_core_px ≥ 3`; FP reported as a
    count and share, no bar; the payoff (the censored verdict fit's FWHM error where the plateau test passed)
    reported. The F-3 twin bars and Task 8's carried items (the Task 8 brief's C1) are unchanged.

## File structure

athenaeum (`crates/athenaeum-core/src/` unless noted):

| File | Responsibility |
| ---- | ---- |
| `star_measure/mod.rs` | `MEASURE_VERSION`, `MeasureOptions` (+ `Default`), `Mode`, `SeedSpec`, `FitStarsOptions` (+ `Default`), `PlaneMeasurement` (+ `PhaseTimings`, `SkyLevel`), `measure_plane`, `fit_stars_at`, re-exports |
| `star_measure/domain.rs` | `Domain`, `ADU_SCALE` and `NoiseSource` (moved from `measure.rs`, re-exported there), `NoiseModel`, `noise_model`, `to_working_units`, `background_residual`, `noise_mrs` (moved), `sky_level` |
| `star_measure/seeds.rs` | the matched filter (Task 6a): `MatchedFilter`, `detect`, `DEFAULT_THRESHOLD`, `FieldWidth`, `field_width_of`, `halfmax_width` (PA-17) |
| `star_measure/fit.rs` | `Seed`, `FitParams`, `StarFit`, `StarFlags`, `FitOutcome`, `PsfModel`, `fit_one`, `accept`, `aperture`, `dedupe`, `fit_all`, `fit_stars`, `fit_stars_with_beta` (moved), canonical axes (Task 5), region/background/width start (Task 6b) |
| `star_measure/beta.rs` | the per-frame β: discrete `AUTO_BETAS` search (moved) → continuous free-β median (Task 9), `BetaSource` |
| `star_measure/photometry.rs` | `annulus_sigma` and the per-seed saturation verdict (Task 8), model flux + aperture correction (Task 10), SNR and HFR (Task 11) |
| `star_measure/frame.rs` | `frame_shape`, `signal_totals`, `FrameAggregates` (moved + the unsaturated statistic, Task 8) |
| `star_measure/budget.rs` | the Analysis-mode selection (Task 12) |
| `star_measure/diagnostics.rs` | `RefusalReason`, `Outcome` (incl. `NotSelected`, `Trimmed`), `FitDiagnostics`, `RefusalCounts` (Task 4); `MostlySaturated` and `FitDiagnostics.verdict` (Task 8) |
| `star_measure/robust.rs` | moved verbatim from `stacking/robust.rs` |
| `test_support.rs` | `fixture_field` (the 150-star field moved from `measure.rs`'s tests, ruling F44), `nan_border_field` (Task 6a), `StarFit::test_at` (Task 4) |
| `stacking/measure.rs` | the adapter: `PlaneReader` → `Domain::Calibrated`, options from the config, `ChannelMeasurement` (+ `stars_refused`, `stars_unsaturated`, `refused_by_reason`), `.athf` write path unchanged |
| `stacking/psf_signal.rs` | PSFSW/PSFSNR, `group_beta`, re-exports of every moved item |
| `stacking/fits_artifact.rs` | the flags byte written and read (Task 4) |
| `stacking/config.rs` | `MEASURE_VERSION` in the three stage hashes (Task 3); `seed_source`, `seed_threshold`, the legacy decode (Task 7) |
| `stacking/plan.rs` | the golden-literal test (hand edits in Tasks 3 and 7) |
| `examples/star_probe.rs` | `legacy-analysis` alias + unknown-path error (Task 1b), refusal lines, flags, `--diagnostics`, the effective seed source in `options` (Task 6a), `snr`/`hfr`, `analysis-mode` |
| `src/types/stacking.ts`, `src/components/stacking/panels/MeasurePanel.tsx`, `src/components/stacking/stageSummary.ts` | the config migration's UI (Task 7) |
| `docs/stacking/README.md` | one "One star-measurement path, Plan A" section: rulings R-OSP-1…, every acceptance run |
| `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-a.md` | the "after Plan A" note (Task 14) |

rustafits (`rustafits/src/analysis/fitting.rs`): `fit_moffat_2d_bounded`, `Moffat2DBoundedResult`, `BetaParam`
with its three arms, the ln-β Jacobian column, tests (Task 9); a convergence change only under Task 5's trigger and
only as a new entry point.

athenaeum-lab (`star-lab/`):

| File | Responsibility |
| ---- | ---- |
| `starlab/render.py` | `Component` kind `annulus` (PA-9) |
| `starlab/families.py` | families `sharp/{m4,gauss}/f{12.0,16.0}`, `donut/r{6,10}` (`Psf.__init__` gains the `donut` branch); cards `EGAIN`, `ATH_CSCL`, `ATH_CFNM` in `_write` and in `_build_resampled`; truth columns `clipped_core_px`, `sat_neighbour_only`, `sat_gray`, `snr_ap` |
| `bin/augment_v2.py` | the in-place card and truth-column augmentation of the existing families; skips the in-place `pairs` grid, `alias` entries and every record whose header already holds `EGAIN` (idempotent) (Task 1a) |
| `starlab/manifest.py` | `verify` reads the truth CSV even when the FITS hashes are equal (Task 1a, ruling F30) |
| `starlab/probe_io.py`, `starlab/probe_plan.py`, `bin/run_probe.py` | `refused` lines, the `analysis-mode` and `legacy-analysis` arms, `STAR_PROBE_ARGS` (Task 1b) |
| `starlab/scorecard.py`, `scorecard_build.py` | rows: fit outcomes, donut outcome, axis order, saturation flag vs truth, star SNR vs truth, budget coverage, analysis-mode cost; `_threshold` reads the frame line's effective seed source |
| `bin/scorecard_diff.py` | the mechanical no-regression check, PA-19's design-list semantics (Task 1c) |
| `harness/acceptance/clone-before.sh`, `tier1-run.sh`, `project-run.sh`, `hc11-run.sh`, `tier1-extract.py`, `tier1-compare.py` | the clone recipe (WAL checkpoint), `ATH_ACC_RERUN_FROM`, `ATH_ACC_SET_REFERENCE`, `--run-id`/`--project-id`, `--base-db`/`--base-run-id`, `immutable=1` reads, the fixed harness path (Task 13) |
| `bin/gates.py` | `STARLAB_ACC_COPY`, `STARLAB_ACC_RUN_ID`, `STARLAB_SCRATCH` for `probe_parity` |
| `tests/test_*.py` | one test file per changed module |
| `results/plan-a/<step>/` | committed summaries, `noreg.txt`, `noreg-cumulative.txt`, bar tables per step |

Run the lab tests from `star-lab/`: `python3 -m unittest discover -s tests -v`.

## Interfaces Plan C consumes (written here so Plan C needs no new module surface)

- `star_measure::measure_plane(plane: &[f32], w: usize, h: usize, opts: &MeasureOptions, pool:
  Option<&Arc<rayon::ThreadPool>>) -> PlaneMeasurement` with `opts.mode = Mode::Budget { budget }` (D-3, default
  500), `opts.domain = Domain::RawAdu { clip, full_scale (or bits), gain_e_per_adu }` (clip from BITPIX/BZERO or DATAMAX, the
  caller's job; `full_scale` the stored integer type's, 2^bits − 1, never DATAMAX; gain from EGAIN), `opts.seeds = SeedSpec::MatchedFilter { threshold }` (the Settings → Analysis field in σ_f units,
  default `seeds::DEFAULT_THRESHOLD`), `opts.colour` (the saturation test's mono/colour rule, spec §2.1; Plan C's
  mosaic input sets its own value: an undebayered CFA plane with a green-interpolated call uses the mono ≥ 3-of-8 rule,
  Q3/Q7), `opts.scale_estimator`. There is no
  `opts.saturation_fraction`: spec §2.1 has had no clip clause since 2026-10-09. The test's raw-ADU norm is
  2^bits − 1, carried by the new `Domain::RawAdu` field (Q2).
- Per star (`StarFit`): `x, y, background, amplitude, fwhm_x ≥ fwhm_y, theta` (radians, image frame, on `fwhm_x`'s
  axis), `beta`, `residual`, `signal` (the F-5 flux), `snr` (F-6; `f64::NAN` when not measured), `hfr` (F-6, on the
  F-5 flux), `peak5`, `flags: StarFlags` (`SATURATED`, `BETA_AT_CAP`), plus `fwhm()`, `eccentricity()`.
- Per plane (`PlaneMeasurement`): `stars`, `beta`, `beta_source`, `frame: FrameAggregates { fwhm_px: Option<f64>,
  eccentricity: Option<f64>, snr_median, hfr_median, stars_fitted, stars_unsaturated, tflux, tmean_flux,
  mean_flux_rejected }` (the residual-weighted mean over accepted UNSATURATED fits is the one frame statistic, §4.2;
  `None` when no unsaturated fit exists), `noise: NoiseModel { sigma, source, m_star, n_star }`, `sky: SkyLevel {
  median, mad, … }` (the stratified-sample sky statistic in the plane's own domain, PA-21: `frame_snr = sky.median /
  noise.sigma` and `snr_weight`'s denominator read `sky.median`, never `noise.m_star`, which is the median of the mesh
  RESIDUALS — I-8), `seeds` (stars detected), `refused: RefusalCounts` (`stars_refused` and per-reason counts),
  `budget: Option<BudgetReport>`, `snr_gain: Option<f64>` (None = the shot term was omitted), `diagnostics`,
  `timings: PhaseTimings`, `snr_missing`. `wing_fit_missing` is retired with PA-8: a seed the verdict refuses is
  counted under `mostlySaturated`.
- `star_measure::seeds::detect(plane, w, h, noise, threshold, max_stars, mask, pool)` is public: Plan C calls it on
  its own 2×2 super-pixel OSC plane and hands the native-coordinate sites to `fit_stars_at` with the CFA mask.
- `star_measure::MEASURE_VERSION` for `AnalysisConfig::config_hash` and the `frame_analysis.measure_version` column.
- The trail metric (§4.3) is Plan C's: Plan C adds `FrameAggregates.trail: Option<TrailMetric>` with
  `#[serde(default)]` (M-10).
- Mosaic input (§4.1 OSC): `measure_plane` takes a pixel list through `SeedSpec::Given` and a `Domain::RawAdu` plane;
  the green-site fitting is Plan C's own `fit_stars_at` call with a CFA pixel mask — the fitter already takes a
  `&[PixelSample]`, so no Plan A change is needed; `fit_stars_at` accepts `FitStarsOptions.mask` and
  `FitStarsOptions.field_width` from Task 6a on (a pixel whose mask bit is false is treated as absent, like a
  non-finite one; a `None` field width is computed from the given seeds), and `beta::frame_beta` takes the same
  `FitStarsOptions` (ruling F45), so Plan C's β sample reads the green sites, not the whole mosaic.

---

### Task 1a: Lab first — the generator: `annulus`, the FWHM 12/16 and donut families, the cards, the truth columns (spec §5 step 1)

No product change. Everything the later tasks measure against exists after Tasks 1a–1c; the bars are the spec's and
are already fixed. Task 1a is the generator half (data on disk); Task 1b the lab tooling that reads the probe's new
lines; Task 1c the no-regression tool and the first diff.

**Files:**
- Modify: `star-lab/starlab/render.py` (`Component` kind `annulus`, `cutout_radius`, `evaluate`),
  `star-lab/starlab/families.py` (new families; `Psf.__init__` gains the `donut` branch, ruling F31; `_cards`,
  `_write`, `_build_resampled` (its own card dict gets the three cards too), `_stats`, `_truth_rows`,
  `_build_twins`, `COLS`, a module flag `CARDS_V2 = True` — `False` writes what the pre-augment generator wrote:
  neither the three cards NOR the four truth columns, ruling F30 as folded), `star-lab/starlab/manifest.py`
  (`verify` compares the truth CSV even when the FITS sha256 is equal — today it returns "hash equal" without
  reading the truth, `manifest.py:21-22`)
- Create: `star-lab/bin/augment_v2.py`, `star-lab/tests/test_augment.py`, `star-lab/tests/test_donut.py`
- Test: `star-lab/tests/test_families.py`, `test_render.py`, `test_manifest.py` (additions)

**Interfaces:**
- Consumes: `manifests/v2.json` (164 families).
- Produces:
  - families `sharp/m4/f12.0`, `sharp/m4/f16.0`, `sharp/gauss/f12.0`, `sharp/gauss/f16.0` (`kind="field"`,
    `n_stars=300`, `lattice=True`, `min_sep=6·F` = 72 / 96 px, `bins="entry"`) and `donut/r6`, `donut/r10`
    (`kind="field"`, `prof="donut6"`/`"donut10"` → `Psf(model="donut")`: `render.Component(1.0, "annulus", R,
    0.4·R)` blurred by `[2.6]`, `fwhm=2.6` is the blur, `n_stars=200`, `lattice=True`, `min_sep=120`; every star
    row `kind="donut"`, the twelve `fwhm_*`/`pix_*`/`fit_*` width columns empty, no CRLB);
  - FITS cards on every v2 image, written by the generator LAST — after `BAYERPAT`, inside `_write` and inside
    `_build_resampled`'s own writer, never in `_cards` (M-5): the float file gets `EGAIN` (e⁻/ADU = `gain/65535`),
    `ATH_CSCL = 65535.0`, `ATH_CFNM = 1.0`; the `.ideal.fits` the same three; the `.u16.fits` sibling `EGAIN` only;
  - the truth CSV columns `clipped_core_px` (the star's own core pixels — within `max(1, 0.5·fit_fwhm_gm)` px of its
    noiseless peak — at or above `sat` in the NOISY image), `sat_neighbour_only` (1 when `saturated = 1` and the
    star's own stamp + background never reaches `sat`), `sat_gray` (1 when the own noiseless peak + background is
    within 2 % of `sat`), `snr_ap` (the noiseless stamp summed in the PA-11 aperture — radius
    `max(1.5·fit_fwhm_gm, 3)` — over `sqrt(S/g + N_ap·(sky/g + rn²)·(1 + N_ap/N_sky))`, `N_sky` the PA-11 annulus's
    pixel count on the truth grid — ruling F5); donut rows carry `snr_ap` empty (no `fit_fwhm_gm`);
  - `bin/augment_v2.py manifests/v2.json` — for every pre-existing record EXCEPT (a) the in-place `pairs` family
    (`spec.kind == "pairs_in_place"`: its files under `~/.athenaeum-acc/star-lab/grid` are never rewritten, ruling
    F31; the record is listed as skipped and its LN-only rows need no card), (b) an `alias` entry (`truthcrowd`
    holds copies of `density_pairs/d600`'s records — the same files: never augmented itself, its records are
    re-copied from the family it names once that family is done, so their `sha256` follows) and (c) any record
    whose float header already holds `EGAIN` (the six families step 4 generates BEFORE the augment, and every
    record of an earlier run: augment is idempotent, pinned — a second run changes no byte): rewrites the float
    file's header in
    place with the three cards appended in the generator's order (data bytes untouched, verified by a sha256 of the
    data part before and after), rewrites `.ideal.fits` (three cards) and `.u16.fits` (`EGAIN`) the same way,
    appends the four truth columns (re-rendering the noiseless stars from the family's seeds without the CRLB —
    `_build_resampled` records go through their source family's renderer and the stored warp; refuses an image
    whose re-rendered `flux_img` or `peak` differ from the stored truth by more than 1e-9), updates `sha256` in the
    manifest, records `augmented` in each record's `extra`. A regeneration and the augmented file agree byte for
    byte, truth CSV included (`make_v2.py --check` proves it: `manifest.verify` now compares the truth CSV even
    when the FITS hashes are equal — ruling F30 as folded).

- [ ] **Step 1: Write the failing lab tests** (`tests/test_donut.py`, `tests/test_augment.py`, additions)

```python
"""Donut stars (PA-9, ruling F1): an annulus convolved with a Gaussian, pixel-integrated, with no width truth."""
import csv, tempfile, unittest
import numpy as np
from starlab import render, families


class TestDonut(unittest.TestCase):
    def test_annulus_component_has_a_central_dip_after_blur(self):
        # os_=21: the hard-edged annulus at the exact pixel centre sums to 1 − 5.7e-4 there; the default sub-grid
        # (os 7 at total FWHM 3.68) reads 1 − 3.24e-3, outside the delta (computed, fold check)
        comps = [render.Component(1.0, "annulus", 6.0, 2.4)]           # outer R 6, obstruction 0.4 R
        img = np.zeros((81, 81))
        s, pk, _ = render.render_star_blurred(img, 40.0, 40.0, 1.0, comps, 0.0, 2.6, 2.6, [2.6], os_=21)
        self.assertAlmostEqual(s, 1.0, delta=2e-3)
        centre = img[40, 40]
        ring = 0.25 * (img[40, 44] + img[40, 36] + img[44, 40] + img[36, 40])   # r = 4.0 (0.7 R = 4.2)
        self.assertGreater(ring, 1.5 * centre)                         # ring/centre reads 8.1 (computed)

    def test_annulus_evaluate_is_zero_inside_the_obstruction_and_normalised(self):
        comps = [render.Component(1.0, "annulus", 6.0, 2.4)]
        self.assertEqual(float(render.evaluate(comps, 1.0, 0.0)), 0.0)
        area = np.pi * (6.0 ** 2 - 2.4 ** 2)
        self.assertAlmostEqual(float(render.evaluate(comps, 4.0, 0.0)) * area, 1.0, places=9)
        self.assertEqual(render.cutout_radius(comps, 6.0), 6 + 1)

    def test_donut_family_rows_are_kind_donut_without_widths(self):
        spec = dict(families.FAMILIES["donut/r6"]); spec.update(size=256, n_stars=4, min_sep=60.0)
        recs = families.build("donut/r6", spec, tempfile.mkdtemp(), seed=3)
        rows = list(csv.DictReader(open(recs[0]["truth"])))
        self.assertEqual(len(rows), 4)
        self.assertTrue(all(r["kind"] == "donut" for r in rows))
        for col in ("fit_fwhm_minor", "fwhm_major", "crlb_fwhm_gm", "snr_ap"):
            self.assertTrue(all(r[col] in ("", "nan") for r in rows), col)
        self.assertTrue(all(float(r["flux_img"]) > 0.99 * float(r["flux"]) for r in rows))
```

```python
"""augment_v2.py: cards appended, data bytes untouched, truth columns appended and reproducible (rulings F30, F31).
Every old record is built with CARDS_V2 = False — no cards AND no truth columns, as the pre-augment generator wrote
them — so an augment that leaves either half undone fails these tests."""
import os, tempfile, unittest, csv, importlib.util
import numpy as np
from starlab import families, fitsio
HERE = os.path.dirname(os.path.abspath(__file__))
_s = importlib.util.spec_from_file_location("augment_v2", os.path.join(HERE, "..", "bin", "augment_v2.py"))
aug = importlib.util.module_from_spec(_s); _s.loader.exec_module(aug)
V2 = ("clipped_core_px", "sat_neighbour_only", "sat_gray", "snr_ap")


class TestAugment(unittest.TestCase):
    def setUp(self):
        self.addCleanup(setattr, families, "CARDS_V2", families.CARDS_V2)
        families.CARDS_V2 = False                        # build as the pre-augment generator did

    def _old(self, name, **over):
        spec = dict(families.FAMILIES[name]); spec.update(over)
        return families.build(name, spec, tempfile.mkdtemp(), seed=11, only_first=True)[0], spec

    def test_cards_appended_and_data_identical(self):
        rec, spec = self._old("sharp/m4/f2.6", size=256, density=150)
        _, h0 = fitsio.read_fits(rec["path"])
        self.assertNotIn("EGAIN", h0)
        self.assertFalse(set(V2) & set(next(csv.DictReader(open(rec["truth"])))), "the old truth has no v2 column")
        before = aug.data_sha256(rec["path"])
        families.CARDS_V2 = True
        aug.augment_record(rec, spec, seed=11)
        _, h = fitsio.read_fits(rec["path"])
        self.assertAlmostEqual(float(h["EGAIN"]), spec["gain"] / 65535.0, places=9)
        self.assertEqual(float(h["ATH_CSCL"]), 65535.0); self.assertEqual(float(h["ATH_CFNM"]), 1.0)
        self.assertEqual(aug.data_sha256(rec["path"]), before)
        _, hu = fitsio.read_fits(rec["extra"]["u16"])
        self.assertIn("EGAIN", hu); self.assertNotIn("ATH_CSCL", hu)
        rows = list(csv.DictReader(open(rec["truth"])))
        for c in V2:
            self.assertIn(c, rows[0])
        self.assertTrue(all(float(r["snr_ap"]) > 0 for r in rows if r["saturated"] == "0"))
        self.assertTrue(rec["extra"]["augmented"])

    def test_augmented_file_equals_a_fresh_generation_byte_for_byte(self):
        rec, spec = self._old("osc/f2.6/m4", size=256, density=150)
        families.CARDS_V2 = True
        aug.augment_record(rec, spec, seed=11)
        fresh = families.build("osc/f2.6/m4", spec, tempfile.mkdtemp(), seed=11, only_first=True)[0]
        for a, b in ((rec["path"], fresh["path"]), (rec["extra"]["ideal"], fresh["extra"]["ideal"]),
                     (rec["extra"]["u16"], fresh["extra"]["u16"])):
            self.assertEqual(open(a, "rb").read(), open(b, "rb").read(), a)
        self.assertEqual(open(rec["truth"]).read(), open(fresh["truth"]).read())

    def test_augment_refuses_a_truth_that_does_not_reproduce(self):
        rec, spec = self._old("sharp/m4/f2.6", size=256, density=150)
        families.CARDS_V2 = True
        with self.assertRaises(aug.TruthMismatch):
            aug.augment_record(rec, spec, seed=12)      # another seed: flux_img differs

    def test_pairs_in_place_is_skipped(self):
        rec = {"family": "pairs", "name": "iso/m4/ref", "path": "/nonexistent", "truth": "/nonexistent",
               "extra": {"role": "reference", "u16": None}}
        self.assertEqual(aug.augment_record(rec, {"kind": "pairs_in_place"}, seed=None), "skipped: pairs_in_place")

    def test_augment_twice_changes_nothing_the_second_time(self):
        # a record whose float header already holds EGAIN is skipped (the fresh v2 families, an alias's copies, a
        # re-run): the cards are never appended twice
        rec, spec = self._old("sharp/m4/f2.6", size=256, density=150)
        families.CARDS_V2 = True
        aug.augment_record(rec, spec, seed=11)
        files = [open(p, "rb").read() for p in (rec["path"], rec["extra"]["u16"], rec["truth"])]
        aug.augment_record(rec, spec, seed=11)
        self.assertEqual([open(p, "rb").read() for p in (rec["path"], rec["extra"]["u16"], rec["truth"])], files)
        self.assertEqual(fitsio.card_keys(rec["path"]).count("EGAIN"), 1)
```

Addition to `tests/test_manifest.py` (ruling F30 as folded):

```python
    def test_verify_reads_the_truth_even_when_the_fits_hash_is_equal(self):
        d = tempfile.mkdtemp()
        spec = dict(families.FAMILIES["sharp/m4/f2.6"]); spec.update(size=128, density=150)
        rec = families.build("sharp/m4/f2.6", spec, os.path.join(d, "a"), seed=11, only_first=True)[0]
        def regenerate():
            new = families.build("sharp/m4/f2.6", spec, os.path.join(d, "b"), seed=11, only_first=True)[0]
            with open(new["truth"], "a") as f:
                f.write("\n")                                           # the same FITS bytes, a different truth
            return new
        ok, why = manifest.verify(rec, regenerate)
        self.assertFalse(ok, why)
```

Additions to `tests/test_families.py`:

```python
    def test_fwhm16_families_are_isolated_at_six_fwhm(self):
        spec = dict(families.FAMILIES["sharp/m4/f16.0"]); spec.update(size=1024, n_stars=40)
        recs = families.build("sharp/m4/f16.0", spec, tempfile.mkdtemp(), seed=5, only_first=True)
        r = rows(recs[0]["truth"]); xy = np.array([[float(s["x"]), float(s["y"])] for s in r])
        d = np.hypot(xy[:, None, 0] - xy[None, :, 0], xy[:, None, 1] - xy[None, :, 1]) + np.eye(len(xy)) * 1e9
        self.assertGreaterEqual(d.min(), 96.0)

    def test_cards_carry_egain_and_cscl_after_bayerpat(self):
        recs = self.build("osc/f2.6/m4", size=256, density=150)
        keys = fitsio.card_keys(recs[0]["path"])          # the header's keys in file order (new helper)
        self.assertLess(keys.index("BAYERPAT"), keys.index("EGAIN"))
        self.assertEqual(keys[keys.index("EGAIN"):keys.index("EGAIN") + 3], ["EGAIN", "ATH_CSCL", "ATH_CFNM"])
        _, hu = fitsio.read_fits(recs[0]["extra"]["u16"]); self.assertIn("EGAIN", hu); self.assertNotIn("ATH_CFNM", hu)
        _, hi = fitsio.read_fits(recs[0]["extra"]["ideal"]); self.assertIn("ATH_CFNM", hi)

    def test_clipped_core_px_counts_the_noisy_core(self):
        recs = self.build("saturated/f2.6/n20", size=512)
        r = rows(recs[0]["truth"]); img, _ = fitsio.read_fits(recs[0]["path"])
        sat = [s for s in r if s["saturated"] == "1" and s["sat_neighbour_only"] == "0"]
        self.assertGreater(len(sat), 0)
        for s in sat:
            x, y = int(round(float(s["x"]))), int(round(float(s["y"])))
            core = img[y - 1:y + 2, x - 1:x + 2]
            self.assertEqual(int(s["clipped_core_px"]) > 0, bool((core >= 0.95 - 1e-6).any()))
        self.assertTrue(all(s["sat_gray"] in ("0", "1") for s in r))

    def test_snr_ap_includes_the_sky_estimate_term(self):
        spec = dict(families.FAMILIES["sharp/m4/f2.6"]); spec.update(size=256, density=150)
        recs = families.build("sharp/m4/f2.6", spec, tempfile.mkdtemp(), seed=11, only_first=True)
        inside = lambda t: all(16.0 <= float(t[k]) <= 239.0 for k in ("x", "y"))   # its sky annulus wholly on the grid
        r = [s for s in rows(recs[0]["truth"]) if s["saturated"] == "0" and s["edge"] == "0" and inside(s)]
        s = max(r, key=lambda t: float(t["flux"]))
        snr = float(s["snr_ap"]); fisher = float(s["fisher_snr"])
        self.assertLess(snr, fisher)                     # an aperture SNR is below the Fisher bound
        self.assertGreater(snr, 0.5 * fisher)
        # ruling F5: this star's snr_ap recomputed from its own noiseless stamp summed in the PA-11 aperture, N_ap and
        # N_sky counted on the frame grid; the column must equal it WITH the (1 + N_ap/N_sky) term and not without
        img = families._make("sharp/m4/f2.6", spec, 11, 0)                # image 0 again, its stars drawn, no noise
        st = next(t for t in families._all_stars(img) if t["id"] == int(s["id"]))
        star = np.zeros((256, 256)); ya, yb, xa, xb = st["box"]; star[ya:yb + 1, xa:xb + 1] = st["flux"] * st["unit"]
        F = math.sqrt(float(s["fit_fwhm_minor"]) * float(s["fit_fwhm_major"]))
        yy, xx = np.mgrid[0:256, 0:256]; d = np.hypot(xx - float(s["x"]), yy - float(s["y"]))
        r_ap = max(1.5 * F, 3.0); r_in = max(3.0 * F, 6.0); r_out = r_in + max(F, 3.0)
        S = star[d <= r_ap].sum(); n_ap = int((d <= r_ap).sum()); n_sky = int(((d >= r_in) & (d <= r_out)).sum())
        g, rn, sky = spec["gain"], spec["rn"], float(s["sky_local"])
        want = S / math.sqrt(S / g + n_ap * (sky / g + rn * rn) * (1.0 + n_ap / n_sky))
        no_term = S / math.sqrt(S / g + n_ap * (sky / g + rn * rn))
        self.assertAlmostEqual(snr / want, 1.0, places=9)
        self.assertGreater(abs(snr / no_term - 1.0), 1e-3)                # the term is in the column
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab && python3 -m unittest discover -s tests -v 2>&1 | tail -30`
Expected: `AttributeError`/`ValueError`/`ModuleNotFoundError` on `annulus`, `donut6`, `augment_v2`, `CARDS_V2`,
`card_keys`.

- [ ] **Step 3: Implement** render `annulus` (evaluate: `w/(π(a² − b²))` for `b ≤ r ≤ a`, else 0; `cutout_radius`:
  an all-annulus component list returns `ceil(a) + 1`, ignoring the function's `3·FWHM_major` floor — the blur
  padding is `render_star_blurred`'s own), the `Psf` `donut` branch (`model = "donut"`,
  `widths()` returns the twelve width keys as NaN, `bounds()` returns None), the family entries, the cards in
  `_write` and `_build_resampled` (after `BAYERPAT`; the ideal and u16 dicts) and the four truth columns, BOTH behind
  `CARDS_V2` (ruling F30 as folded), `_stats`/`_write`
  (the three saturation counts need the NOISY image: computed in `_write` after `noise.apply`, before the CSV is
  written; `_build_twins` recomputes them per twin), `snr_ap` with the `(1 + N_ap/N_sky)` term, `fitsio.card_keys`,
  `manifest.verify` (the truth CSV compared even on equal hashes), `augment_v2.py` (`data_sha256`,
  `augment_record(rec, spec, seed) -> str`, `TruthMismatch`, the `pairs`, `alias` and `EGAIN`-present skips).

- [ ] **Step 4: Generate and augment** (heavy; one at a time; `memory_pressure` ≥ 30 % free; disk ≥ 100 GiB)

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab
mkdir -p results/plan-a/t01a-generator
python3 bin/make_v2.py --families sharp/m4/f12.0,sharp/m4/f16.0,sharp/gauss/f12.0,sharp/gauss/f16.0,donut/r6,donut/r10
python3 bin/augment_v2.py manifests/v2.json            # every pre-existing record but pairs, aliases and EGAIN-carrying records; prints per family: cards, columns, sha
python3 bin/make_v2.py --check --families sharp/m4/f2.6,twins/sharp/m4/f2.6,background/neb3,osc/f2.6/m4,resampled/f2.0,sharp/m4/f16.0,donut/r6
du -sk ~/.athenaeum-acc/star-lab-v2 | tee results/plan-a/t01a-generator/data-root-kib.txt
```

Expected: six new families with their SNR-bin reports (the FWHM 12/16 families print `>300` bins as `n/a (ceiling)`
where the lattice's 300 stars cannot fill them — reported, allowed by P-23); `--check` `ok` on every named family
(the augmented record regenerates to the same bytes AND the same truth CSV — compared by `manifest.verify` even
when the FITS hashes are equal, ruling F30 as folded — `resampled/f2.0` included); the data root ≤ 10 485 760 KiB.

- [ ] **Step 5: Commit and push** (athenaeum-lab)

```
lab: Plan A step 1a - FWHM 12/16 and annular donut families, EGAIN/ATH_CSCL/ATH_CFNM cards after BAYERPAT, saturation and aperture-SNR truth columns, augment_v2
```

**Acceptance:** all lab tests pass; `make_v2.py --check` ok on the seven named families; data root ≤ 10 GB; the
family count in the manifest is 170; every record's `extra.augmented`, a fresh generation, an alias copy, or the
`pairs` skip; a second `augment_v2.py` run changes no byte. No bar is read.

---

### Task 1b: Lab tooling — the probe's `legacy-analysis` alias, the new lines and arms, the scorecard rows, the plan and gate hooks

**Files:**
- Modify (athenaeum): `crates/athenaeum-core/examples/star_probe.rs` (ruling F2: `--paths legacy-analysis` is an
  alias of today's `analysis` path — the JSONL `path` stays `"analysis"`; `--paths` is validated: an unknown name
  dies with exit 1 before any output file exists)
- Modify (lab): `star-lab/starlab/probe_io.py` (`refused` lines, `analysis-mode`), `star-lab/starlab/probe_plan.py`
  (`legacy-analysis`, `analysis-mode` jobs, `STAR_PROBE_ARGS`), `star-lab/bin/run_probe.py` (`STAR_PROBE_ARGS`),
  `star-lab/starlab/scorecard.py` (new rows, `_threshold`), `star-lab/starlab/scorecard_build.py` (wire the rows),
  `star-lab/bin/scorecard.py` (`--rows`), `star-lab/bin/path_compare.py` (`--probe-dir`), `star-lab/bin/gates.py`
  (`STARLAB_ACC_COPY`, `STARLAB_ACC_RUN_ID`, `STARLAB_SCRATCH`), `star-lab/starlab/__init__.py` (`SCRATCH_ROOT`)
- Test: `examples/star_probe.rs` tests; `star-lab/tests/test_scorecard.py`, `test_run_probe.py`,
  `test_path_compare.py`, `test_gates.py` (additions)

**Interfaces:**
- Produces:
  - `star_probe --paths legacy-analysis` writes exactly what `--paths analysis` wrote at f5321979 (same lines, same
    `path: "analysis"`); `--paths bogus` exits 1 with `unknown path: bogus` and creates no output file;
  - `probe_io.load` → `{"seeds", "stars", "frames", "refused": {path: [...]}}`; a `refused` line is
    `{"kind":"refused","path":…, "plane":p, "seed":i, "reason":"regionTooSmall", "regionRadius":r,
    "backgroundStart":b, "widthStart":w, "growthSteps":g, "iterations":n|null}`; `notSelected`/`trimmed` are
    reasons too from Task 12 (ruling F13);
  - `probe_plan`: the stack invocation stays `--paths measure,betas,register`; the u16 invocation becomes `--paths
    legacy-analysis`; a NEW invocation per u16 sibling `--paths analysis-mode --domain adu --clip 65535` →
    `<name>.amode.jsonl` (added to the plan from Task 12 on through `probe_plan.ANALYSIS_MODE = True`; off until
    then so the diff tool's row sets stay comparable); `run_probe.py` honours `STAR_PROBE_ARGS` (extra `star_probe`
    flags appended to every `measure` invocation, recorded in each run record's `cmd` so `--verify` and
    `complete()` tell the runs apart);
  - scorecard rows (all `_row(...)` with numeric fields so `scorecard_diff.py` can judge them):
    - row 1 `fit outcomes` per family × path: `accepted + refused + not_selected + trimmed = seeds` (`ok` boolean,
      per-reason counts; `not_selected`/`trimmed` are 0 unless the path is `analysis-mode`, ruling F13);
    - row 1 `donut outcome` (families with kind `donut`): `accepted %`, `refused %` by reason over matched donuts;
    - row 4 `axis order`: the share of accepted fits with `fwhmX >= fwhmY` (n, share);
    - row 10 `saturation flag vs truth`: FP = flagged ∧ truth-unsaturated over truth-unsaturated (share), FN = not
      flagged ∧ `clipped_core_px >= 3` (count), plus the reported counts `neighbour_only`, `gray`, `under_3_core`,
      `wing_fit_missing` (the frame line's `wingFitMissing`, I-3);
    - row 6 `star SNR vs truth`: `(snr / snr_ap − 1)` median and 1.4826·MAD over matched unsaturated stars (bias,
      scatter, n), `n_no_gain` when the frame line says `snrGain: null`;
    - row 1 `budget coverage` (path `analysis-mode`): `cellsCovered / cellsWithStars` (share);
    - row 9 `invocation` for `analysis-mode` as for the other one-path tags;
    - `_threshold(path, frames)`: reads the frame line's `options.measure.seedSource` and `seedThreshold` — the
      EFFECTIVE source the probe ran (the override included, ruling F3); `matchedFilter` → (`fisher_snr`,
      `seedThreshold`), `peak` → (`peak_snr`, `detectionSigma`), `structure` → (`fisher_snr`, 5.0); an ABSENT key
      (every output written before Task 6a, the baseline's included) → `peak` with `detectionSigma`;
  - `bin/scorecard.py --rows 1,…` (score only the named row groups; Task 6a's threshold table reads row 1 alone);
    `bin/path_compare.py --probe-dir` (part 2 reads a given probe directory instead of the v2 root's); `gates.py`
    env overrides `STARLAB_ACC_COPY` (default `~/.athenaeum-acc/tierA-lnB-f1`), `STARLAB_ACC_RUN_ID` (ruling F12:
    the run whose config `probe_parity` reads; default = the one finished run as today, and with the variable set
    a DB holding several finished runs is read on that run), `STARLAB_SCRATCH` (default `DATA_ROOT/gates`);
    `determinism` covers the two new family kinds by construction (it walks the manifest).

- [ ] **Step 1: Write the failing tests**

athenaeum (`examples/star_probe.rs` tests):

```rust
#[test]
fn legacy_analysis_is_an_alias_of_analysis_and_an_unknown_path_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let data: Vec<f32> = gaussian_field(128, 128, 0, true).iter().map(|v| (v * 65535.0).round()).collect();
    let img = d.path().join("p.fits");
    write_fits_f32(&img, 128, 128, 1, &data, &[]).unwrap();
    let a = d.path().join("a.jsonl");
    cmd_measure(args(img.clone(), a.clone(), &["analysis"])).unwrap();
    let b = d.path().join("b.jsonl");
    cmd_measure(args(img.clone(), b.clone(), &["legacy-analysis"])).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    let lines = read_lines(&b);
    assert!(lines.iter().filter(|v| v["kind"] == "star").count() > 0, "the control is not empty");
    assert!(lines.iter().all(|v| v["path"] == "analysis"));
    let c = d.path().join("c.jsonl");
    let err = cmd_measure(args(img, c.clone(), &["bogus"])).unwrap_err();
    assert!(err.contains("unknown path: bogus"), "{err}");
    assert!(!c.exists(), "no output file for a refused path");
}
```

lab (`tests/test_scorecard.py` additions):

```python
    def test_fit_outcomes_row_sums_to_seeds(self):
        frames = [dict(path="measure", plane=0, image=0, seeds=10, fits=7,
                       refused={"total": 3, "byReason": {"regionTooSmall": 2, "duplicate": 1}})]
        r = scorecard.rows_fit_outcomes(frames, "measure", "fam")[0]
        self.assertTrue(r["ok"]); self.assertEqual(r["accepted"], 7); self.assertEqual(r["by_reason"]["duplicate"], 1)
        frames[0]["refused"]["total"] = 2
        self.assertFalse(scorecard.rows_fit_outcomes(frames, "measure", "fam")[0]["ok"])

    def test_fit_outcomes_identity_counts_not_selected_and_trimmed_in_budget_mode(self):
        frames = [dict(path="analysis-mode", plane=0, image=0, seeds=900, fits=500,
                       refused={"total": 20, "byReason": {"fitFailed": 20}}, budget={"notSelected": 350, "trimmed": 30})]
        self.assertTrue(scorecard.rows_fit_outcomes(frames, "analysis-mode", "fam")[0]["ok"])   # 500+20+350+30 == 900

    def test_saturation_flag_row_counts_fp_fn(self):
        t = truth(6)
        for i in (0, 1): t[i]["saturated"] = 1; t[i]["clipped_core_px"] = 4
        t[2]["saturated"] = 1; t[2]["clipped_core_px"] = 0; t[2]["sat_neighbour_only"] = 1
        meas = meas_at(t, flags=[["saturated"], [], [], ["saturated"], [], []])
        r = scorecard.rows_saturation_flag(t, meas, "measure", "fam", frames=[dict(path="measure", plane=0, wingFitMissing=1)])[0]
        self.assertEqual(r["fn"], 1)                        # star 1: clipped core, unflagged
        self.assertAlmostEqual(r["fp"], 1 / 3)              # star 3 flagged among 3 truth-unsaturated
        self.assertEqual(r["neighbour_only"], 1); self.assertEqual(r["wing_fit_missing"], 1)

    def test_star_snr_row(self):
        t = truth(4)
        for s in t: s["snr_ap"] = 100.0
        meas = meas_at(t, snr=[103.0, 101.0, 99.0, 102.0])
        r = scorecard.rows_star_snr(t, meas, "measure", "fam", frames=[dict(path="measure", plane=0, snrGain=50000.0)])[0]
        self.assertAlmostEqual(r["bias"], 0.015, places=9); self.assertEqual(r["n"], 4); self.assertEqual(r["n_no_gain"], 0)

    def test_axis_order_row(self):
        t = truth(3)
        meas = meas_at(t, fwhmX=[3.0, 2.0, 3.0], fwhmY=[2.0, 3.0, 3.0])
        r = scorecard.rows_axis_order(t, meas, "measure", "fam")[0]
        self.assertAlmostEqual(r["share"], 2 / 3); self.assertEqual(r["n"], 3)

    def test_threshold_reads_the_effective_seed_source(self):
        mf = [dict(path="measure", options={"measure": {"seedSource": "matchedFilter", "seedThreshold": 4.5, "detectionSigma": 20.0}})]
        self.assertEqual(scorecard._threshold("measure", mf), ("fisher_snr", 4.5))
        pk = [dict(path="measure", options={"measure": {"seedSource": "peak", "detectionSigma": 16.0}})]
        self.assertEqual(scorecard._threshold("measure", pk), ("peak_snr", 16.0))
        old = [dict(path="measure", options={"measure": {"detectionSigma": 20.0}})]
        self.assertEqual(scorecard._threshold("measure", old), ("peak_snr", 20.0))     # absent → peak (F3)

    def test_budget_coverage_row(self):
        frames = [dict(path="analysis-mode", plane=0, image=0, fits=500, budget={"cellsWithStars": 16, "cellsCovered": 15})]
        r = scorecard.rows_budget_coverage(frames, "analysis-mode", "fam")[0]
        self.assertAlmostEqual(r["share"], 15 / 16)
```

`meas_at(t, **cols)` is the existing helper (it builds a measured line per truth star; the new keyword columns
`flags`, `snr`, `fwhmX`, `fwhmY` are per-star lists). `tests/test_run_probe.py` additions:

```python
    def test_plan_has_legacy_analysis_and_amode_off(self):
        doc = {"families": {"sharp/m4/f2.6": {"spec": {"kind": "field", "size": 2048},
               "images": [{"name": "i0", "path": "/d/i0.fits", "truth": "/d/i0.csv", "extra": {"u16": "/d/i0.u16.fits"}}]}}}
        jobs = run_probe.plan(doc, "/out")
        tags = [j["tag"] for j in jobs]
        self.assertIn("stack", tags); self.assertIn("analysis", tags); self.assertNotIn("analysis-mode", tags)
        u16 = next(j for j in jobs if j["tag"] == "analysis")
        self.assertIn("legacy-analysis", u16["cmd"]); self.assertNotIn("analysis", u16["cmd"][u16["cmd"].index("--paths") + 1].split(",")[1:])
        probe_plan.ANALYSIS_MODE = True; self.addCleanup(setattr, probe_plan, "ANALYSIS_MODE", False)
        self.assertIn("analysis-mode", [j["tag"] for j in run_probe.plan(doc, "/out")])

    def test_star_probe_args_reach_the_cmd_and_the_record(self):
        os.environ["STAR_PROBE_ARGS"] = "--diagnostics --seed-threshold 4.5"; self.addCleanup(os.environ.pop, "STAR_PROBE_ARGS")
        doc = {"families": {"f": {"spec": {"kind": "field", "size": 2048}, "images": [{"name": "i0", "path": "/d/i0.fits", "truth": "/d/i0.csv", "extra": {"u16": None}}]}}}
        j = next(x for x in run_probe.plan(doc, "/out") if x["tag"] == "stack")
        self.assertEqual(j["cmd"][-5:], ["--diagnostics", "--seed-threshold", "4.5", "--out", "/out/f/i0.jsonl"])
        self.assertFalse(run_probe.complete(j, latest={j["out"]: {"cmd": j["cmd"][:-5] + ["--out", j["out"]], "rc": 0}}))
```

`tests/test_path_compare.py::test_probe_dir_argument` (part 2 reads `<probe-dir>/<family>/<name>.jsonl` when
given), `tests/test_gates.py::test_env_overrides_for_acc_copy_run_id_and_scratch` (`gates.ACC_COPY`,
`gates.ACC_RUN_ID`, `gates.SCRATCH` follow the environment at import; `probe_parity`'s run lookup takes
`ACC_RUN_ID` first when set).

- [ ] **Step 2: Run, fail, implement, run** (lab: `python3 -m unittest discover -s tests -v`; athenaeum:
  `CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --example star_probe --
  --test-threads=4`).

- [ ] **Step 3: Commit and push** (lab) and commit (athenaeum)

```
lab: Plan A step 1b - refused lines, legacy-analysis and analysis-mode arms, the fit-outcome / axis-order / saturation / SNR / budget rows, the effective seed source, --rows, STAR_PROBE_ARGS, gates env
```
```
feat(star_probe): legacy-analysis alias of the analysis path; an unknown --paths name is refused before any output
```

**Acceptance:** all lab tests pass; the probe test green; `python3 bin/run_probe.py manifests/v2.json --dry-run`
lists the baseline's 673 jobs of the 164 pre-existing families (the u16 ones now `--paths legacy-analysis` under
their old output names) plus one `stack` and one `legacy-analysis` job per image of Task 1a's six new families — a
total of 673 + 2 × (their image count), printed and checked (no stack output is invalidated: `complete()` reads the
recorded `cmd`, which for the u16 jobs now differs — those re-run at Task 2, the new families' jobs first run there).

---

### Task 1c: `scorecard_diff.py` and the baseline rows for the new columns

**Files:**
- Create: `star-lab/bin/scorecard_diff.py`, `star-lab/tests/test_scorecard_diff.py`
- Produce: `results/plan-a/t01c-baseline/{scorecard-summary.md,noreg-vs-baseline.txt}`

**Interfaces:**
- Consumes: the baseline full scorecard `~/.athenaeum-acc/star-lab-v2/results/baseline-2026-10-07-fw/scorecard.json.gz`
  (sha256 in the committed summary).
- Produces `bin/scorecard_diff.py <base.json[.gz]> <head.json[.gz]> --out noreg.txt [--design design.json]`: pairs
  rows by `(row, metric, family, path, plane, bin, truth, egroup)`; per row kind the standard error of the BASE row:
  `bias/scatter/n` rows → `SE = scatter/√n`; per-axis `bias_x/bias_y/scatter_x/scatter_y/n` rows (row 2
  `centroid`, row 10 `seed centroid by gradient`) → each axis's `bias_<a>` and `scatter_<a>` judged with its own
  `SE = scatter_<a>/√n` (ruling F10 as folded); `median/scatter/n` rows → `SE = 1.2533·scatter/√n`; `share` or
  `completeness`/`raw`/`coverage` over `n` → `SE = √(p(1−p)/n)` floored at `1/n`; `per_mpx` counts → `√count/area`;
  row 7 `value` rows → `SE = value × (row 3 pointfit all-bin scatter of the same family/path/plane)/√n`;
  `spearman` rows → `SE = (1−ρ²)/√(n−1)`; cost rows → reported only; LN rows (PA-14) → reported only; a row
  whose status is `n/a` on either side → reported, not judged. Verdict per judged row: `|head − base| ≤ SE_base`.
  `--design` takes PA-19's list (entries with `row`, optional `metric`/`family`, required `path`, `kind` ∈
  `set-aside | one-sided | reported`, optional `keys` — the fields a `one-sided` entry judges, default `["bias",
  "scatter"]`, each judged on its own; most-specific-wins matching; a `one-sided` entry over a row that lacks any
  of its keys is a load-time error). `design-cumulative.json` is the same shape with a `step` tag per entry and
  PA-19's kind precedence. Exit 1 when any judged row outside the design list moves beyond its SE (either direction:
  an unexplained improvement is still a change) or any `one-sided` row misses, with the row's base, head, SE and
  direction printed. A control section counts the `analysis` (legacy) rows that are not bit-identical (PA-15; must
  be 0).

- [ ] **Step 1: Write the failing tests** (`tests/test_scorecard_diff.py`)

```python
"""scorecard_diff.py: the per-row standard error, the design-list semantics (PA-19) and the verdict."""
import json, os, tempfile, unittest, importlib.util
HERE = os.path.dirname(os.path.abspath(__file__))
_s = importlib.util.spec_from_file_location("scorecard_diff", os.path.join(HERE, "..", "bin", "scorecard_diff.py"))
sd = importlib.util.module_from_spec(_s); _s.loader.exec_module(sd)


def row(**kw):
    d = dict(row=3, metric="fwhm", family="a", path="measure", plane=0, bin="all", truth="pointfit", n=100,
             ours="x", erp="—", ideal="—", bias=0.001, scatter=0.005)
    d.update(kw); return d


class TestDiff(unittest.TestCase):
    def test_bias_row_judged_by_scatter_over_sqrt_n(self):
        base = [row()]; head = [row(bias=0.0014)]          # SE = 0.005/10 = 0.0005: 0.0004 is inside
        rep = sd.compare(base, head, design=[])
        self.assertEqual(rep["judged"], 1); self.assertEqual(rep["moved"], [])
        rep = sd.compare(base, [row(bias=0.0017)], design=[])
        self.assertEqual(len(rep["moved"]), 1)

    def test_share_row_uses_binomial_se_with_a_floor(self):
        base = [row(row=1, metric="completeness", completeness=1.0, n=400, bias=None, scatter=None)]
        inside = [row(row=1, metric="completeness", completeness=0.998, n=400, bias=None, scatter=None)]
        self.assertEqual(sd.compare(base, inside, design=[])["moved"], [])     # SE floor 1/400 = 0.0025 ≥ 0.002
        moved = [row(row=1, metric="completeness", completeness=0.99, n=400, bias=None, scatter=None)]
        self.assertEqual(len(sd.compare(base, moved, design=[])["moved"]), 1)  # 0.01 > 0.0025

    def test_design_rows_are_set_aside_and_ln_rows_reported_only(self):
        base = [row(row=4, metric="angle", family="shape/f2.6/e0.3/t30", rms_deg=44.0, bias=3.0, scatter=20.0),
                row(row=6, metric="arm psfSel", path="ln beta auto", bin="t60", rel=-0.13, bias=None, scatter=None)]
        head = [row(row=4, metric="angle", family="shape/f2.6/e0.3/t30", rms_deg=8.0, bias=0.5, scatter=5.0),
                row(row=6, metric="arm psfSel", path="ln beta auto", bin="t60", rel=0.01, bias=None, scatter=None)]
        d = [{"row": 4, "metric": "angle", "family": "shape/*", "path": "measure", "kind": "set-aside"}]
        rep = sd.compare(base, head, design=d)
        self.assertEqual(rep["moved"], []); self.assertEqual(len(rep["design"]), 1); self.assertEqual(len(rep["ln"]), 1)

    def test_one_sided_design_rule_passes_an_improvement_and_fails_a_worsening(self):
        base = [row(bias=0.010, scatter=0.005, n=400)]                     # SE 0.00025
        better = [row(bias=0.002, scatter=0.004, n=400)]
        worse = [row(bias=0.012, scatter=0.005, n=400)]
        d = [{"row": 3, "metric": "fwhm", "path": "measure", "kind": "one-sided"}]
        self.assertEqual(sd.compare(base, better, design=d)["one_sided_missed"], [])
        self.assertEqual(len(sd.compare(base, worse, design=d)["one_sided_missed"]), 1)

    def test_most_specific_pattern_wins_and_path_is_required(self):
        base = [row(family="shape/f2.6/e0.3/t30", bias=0.010, scatter=0.005, n=400)]
        head = [row(family="shape/f2.6/e0.3/t30", bias=0.050, scatter=0.005, n=400)]
        d = [{"row": 3, "path": "measure", "kind": "one-sided"},
             {"row": 3, "family": "shape/*", "path": "measure", "kind": "set-aside"}]
        rep = sd.compare(base, head, design=d)
        self.assertEqual(rep["one_sided_missed"], []); self.assertEqual(len(rep["design"]), 1)   # the family glob won
        with self.assertRaises(sd.DesignError):
            sd.compare(base, head, design=[{"row": 3, "kind": "set-aside"}])                     # no path
        with self.assertRaises(sd.DesignError):
            sd.compare([row(row=3, metric="beta sensitivity", bias=None, scatter=None)], head,
                       design=[{"row": 3, "metric": "beta sensitivity", "path": "betas", "kind": "one-sided"}])

    def test_one_sided_on_row_2_names_the_axis_keys(self):
        # ruling F10 as folded: row 2 carries bias_x/bias_y/scatter_x/scatter_y and no bias/scatter; SE per axis 0.05/20
        c = dict(row=2, metric="centroid", bias=None, scatter=None, n=400)
        base = [row(**c, bias_x=0.010, bias_y=-0.004, scatter_x=0.05, scatter_y=0.05)]
        better = [row(**c, bias_x=0.002, bias_y=-0.005, scatter_x=0.049, scatter_y=0.051)]   # |−0.005| ≤ 0.004 + 0.0025
        worse = [row(**c, bias_x=0.010, bias_y=-0.004, scatter_x=0.05, scatter_y=0.06)]      # 0.06 > 0.05 + 0.0025
        d = [{"row": 2, "path": "measure", "kind": "one-sided", "keys": ["bias_x", "bias_y", "scatter_x", "scatter_y"]}]
        self.assertEqual(sd.compare(base, better, design=d)["one_sided_missed"], [])
        self.assertEqual(len(sd.compare(base, worse, design=d)["one_sided_missed"]), 1)
        with self.assertRaises(sd.DesignError):                                   # the default keys are absent on row 2
            sd.compare(base, better, design=[{"row": 2, "path": "measure", "kind": "one-sided"}])
        self.assertEqual(len(sd.compare(base, worse, design=[])["moved"]), 1)      # unlisted: judged per axis by SE

    def test_cumulative_union_prefers_set_aside_over_one_sided(self):
        base = [row(bias=0.010, scatter=0.005, n=400)]; head = [row(bias=0.050, scatter=0.005, n=400)]
        union = [{"step": "t05-f2", "row": 3, "metric": "fwhm", "path": "measure", "kind": "one-sided"},
                 {"step": "t06a-seeds", "row": 3, "path": "measure", "kind": "set-aside"}]
        rep = sd.compare(base, head, design=union)
        self.assertEqual(rep["one_sided_missed"], []); self.assertEqual(rep["design"][0]["kind"], "set-aside")

    def test_legacy_analysis_control_counts_non_identical_rows(self):
        base = [row(path="analysis", bias=0.01)]; head = [row(path="analysis", bias=0.0100001)]
        self.assertEqual(sd.compare(base, head, design=[])["control_mismatch"], 1)
```

- [ ] **Step 2: Run, fail, implement, run.**

- [ ] **Step 3: Rebuild the baseline rows the new rows need, without re-running probes.** The new rows (`fit
  outcomes`, `axis order`, …) need the probe's new lines, which the baseline binary never wrote; the new truth
  columns need no probe. Build the baseline scorecard ONCE more from the EXISTING probe outputs
  (`~/.athenaeum-acc/star-lab-v2/probe`, unchanged, verified) so the diff base has the new truth-only rows
  (`saturation flag vs truth` reads only truth + the baseline's lack of a flag → every baseline star unflagged: FP 0,
  FN = all; `_threshold` reads `peak` for every baseline frame line, ruling F3).

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab
mkdir -p results/plan-a/t01c-baseline ~/.athenaeum-acc/star-lab-a/t01c-baseline ~/.athenaeum-acc/star-lab-a/results/t01c-baseline
python3 bin/run_probe.py manifests/v2.json --verify                     # the stack/LN jobs proven (f5321979); the u16 jobs report "command differs" (expected, Task 1b)
python3 bin/scorecard.py manifests/v2.json --probe-dir ~/.athenaeum-acc/star-lab-v2/probe --erp-dir ~/.athenaeum-acc/star-lab-v2/erp \
  --out ~/.athenaeum-acc/star-lab-a/t01c-baseline/scorecard.md --json ~/.athenaeum-acc/star-lab-a/t01c-baseline/scorecard.json
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-v2/results/baseline-2026-10-07-fw/scorecard.json.gz \
  ~/.athenaeum-acc/star-lab-a/t01c-baseline/scorecard.json --out results/plan-a/t01c-baseline/noreg-vs-baseline.txt
gzip -k ~/.athenaeum-acc/star-lab-a/t01c-baseline/scorecard.json && mv ~/.athenaeum-acc/star-lab-a/t01c-baseline/scorecard.json.gz ~/.athenaeum-acc/star-lab-a/results/t01c-baseline/
python3 bin/scorecard_summary.py ~/.athenaeum-acc/star-lab-a/t01c-baseline/scorecard.json --out results/plan-a/t01c-baseline/scorecard-summary.md --full ~/.athenaeum-acc/star-lab-a/results/t01c-baseline/scorecard.json.gz --title "Plan A t01c: baseline rows with the new truth columns"
```

Expected: the diff reports 0 moved rows among the rows both files hold (the probe outputs are the same), the six
new families appear with `output missing` rows (no probe output yet — Task 2's run fills them), the legacy-analysis
control mismatch is 0.

- [ ] **Step 4: Commit and push**

```
lab: Plan A step 1c - scorecard_diff (per-row SE, PA-19 design lists, the one-sided rule, the cumulative union, the legacy control) and the baseline rows with the new truth columns
```

**Acceptance:** all lab tests pass; `noreg-vs-baseline.txt` 0 moved, control 0. No bar is read in Tasks 1a–1c.

---

### Task 2: Extraction into `star_measure/` with no behaviour change (spec §5 step 2)

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/{mod.rs,domain.rs,fit.rs,beta.rs,frame.rs,robust.rs}`
- Modify: `crates/athenaeum-core/src/lib.rs` (`#[cfg(feature = "render")] pub mod star_measure;`),
  `crates/athenaeum-core/src/stacking/mod.rs` (`pub use crate::star_measure::robust;` replaces `pub mod robust;`),
  `crates/athenaeum-core/src/stacking/psf_signal.rs` (keeps `PSF_FIT_VERSION` until Task 3, PSFSW/PSFSNR,
  `group_beta`, the constants; re-exports), `crates/athenaeum-core/src/stacking/measure.rs` (hand edits: the body
  of `measure_plane_detailed` calls the module; `ADU_SCALE` and `NoiseSource` become re-exports of
  `star_measure::domain` — ruling F43; `mod tests` → `pub(crate) mod tests`, its `field` helper delegates to
  `test_support::fixture_field`), `crates/athenaeum-core/src/test_support.rs` (`fixture_field`, ruling F44),
  `crates/athenaeum-core/examples/star_probe.rs` (imports only)
- Delete: `crates/athenaeum-core/src/stacking/robust.rs` (moved)

**Interfaces:**
- Produces (`star_measure/mod.rs`):

```rust
pub use domain::{Domain, NoiseModel, NoiseSource, ADU_SCALE, background_residual, noise_mrs, noise_model,
                 to_working_units, sky_level, SkyLevel};
pub use fit::{fwtm_from_alpha, FitOutcome, FitParams, PsfModel, Seed, StarFit};
pub use beta::{AUTO_BETAS, AUTO_SAMPLE};
pub use frame::{frame_shape, signal_totals, FrameAggregates, SignalTotals, RCR_LIMIT};

/// What the module is handed besides the plane (grows in Tasks 4–12). `Default` = `Domain::Calibrated` without a
/// gain, `PsfModel::Auto`, 24 576 stars, no seeds, `FitParams::default()`, no precomputed noise, `Bwmv` (ruling F14:
/// every test literal uses `..Default::default()` so a later field never breaks an earlier test).
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureOptions {
    pub domain: Domain,
    pub psf_model: PsfModel,
    pub max_stars: usize,
    pub seeds: SeedSpec,
    pub fit: FitParams,
    /// A noise model the caller already computed (the adapter's Median3 arm hands the MRS σ to its detector
    /// BEFORE detection, `measure.rs:383-404`); `None` = the module computes it. Nothing is computed twice (I-1).
    pub noise: Option<NoiseModel>,
    /// The scale estimator the adapter keeps in lockstep with normalization (PA-21; `measure.rs:516`).
    pub scale_estimator: crate::integration::stats::ScaleEstimator,
}
impl Default for MeasureOptions { … }

/// Wall time per phase the module owns (I-1); the adapter adds `read_ms` and `detect_ms` and keeps
/// `ChannelMeasurement.timings` exactly as today.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PhaseTimings { pub background_ms: u64, pub noise_ms: u64, pub fit_ms: u64 }

/// Where the seeds come from. Task 2: the adapter detects (Peak/Structure) and passes them in.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SeedSpec { #[default] None, Given(Vec<Seed>) }

#[derive(Debug, Clone, PartialEq)]
pub struct PlaneMeasurement {
    pub stars: Vec<StarFit>,
    pub beta: f64,
    pub frame: FrameAggregates,
    pub noise: NoiseModel,
    pub sky: SkyLevel,
    pub seeds: usize,
    pub timings: PhaseTimings,
}

pub fn measure_plane(plane: &[f32], w: usize, h: usize, opts: &MeasureOptions,
                     pool: Option<&Arc<rayon::ThreadPool>>) -> PlaneMeasurement;

/// The refit entry point LN uses (PA-6): every seed fitted at `beta`. The plane is in the module's working units
/// (ADU-scaled), exactly as `fit_stars_with_beta` takes it today.
pub fn fit_stars_at(plane: &[f32], w: usize, h: usize, seeds: &[Seed], beta: f64, fit: &FitParams,
                    pool: Option<&Arc<rayon::ThreadPool>>) -> FitOutcome;
```

```rust
// domain.rs
pub const ADU_SCALE: f32 = 65535.0;                       // moved from measure.rs (re-exported there)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NoiseSource { #[default] Mrs, BackgroundResidual }   // moved from measure.rs; `None` joins it in Task 6a
                                                               // (the `metrics` payload enum gains `none`, ruling F43)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Domain {
    /// A calibrated float plane in [0, 1]; the module multiplies by `ADU_SCALE` exactly where the adapter did.
    Calibrated { gain_e_per_unit: Option<f64> },
    /// A raw ADU plane; `clip` = the sensor's clip level in the plane's own units (BITPIX/BZERO or DATAMAX).
    RawAdu { clip: f64, gain_e_per_adu: Option<f64> },
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseModel { pub sigma: f64 /* working units */, pub source: NoiseSource, pub m_star: f64, pub n_star: f64 }
/// The ADU-scaled copy of a `Calibrated` plane, or the plane itself for `RawAdu`.
pub fn to_working_units(plane: &[f32], domain: &Domain) -> std::borrow::Cow<'_, [f32]>;
/// `background_residual` + `noise_mrs` with today's fallbacks and `warn!`s (the two `measure.rs` blocks, moved).
pub fn noise_model(working: &[f32], w: usize, h: usize, pool: Option<&Arc<rayon::ThreadPool>>) -> (NoiseModel, PhaseTimings);
/// The stratified-sample sky statistic (`stats::stratified_sample` → `clip_sample` → median / MAD / avg-dev /
/// `location_scale` / `noise_scale_factors`), moved from the adapter so Plan C reads one value (I-8, PA-21). The
/// clip range is the domain's: `Calibrated` → `[CLIP_LO, CLIP_HI]` on the NATIVE plane (today's bytes, today's
/// numbers), `RawAdu { clip }` → `[1, clip − 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyLevel { pub median: f64, pub mad: f64, pub median_mean_dev: f64, pub location: f64, pub scale: f64,
                      pub noise_scale_low: f64, pub noise_scale_high: f64 }
pub fn sky_level(native: &[f32], w: usize, h: usize, domain: &Domain, est: ScaleEstimator) -> SkyLevel;
```

  `FrameAggregates { fwhm_px: Option<f64>, eccentricity: Option<f64>, stars_fitted, tflux, tmean_flux,
  mean_flux_rejected }` (working units; `None` = no fit, where today's `frame_shape` returns `None` — ruling F14
  fixes the type now so Task 8 changes no test; the adapter maps `None` to `0.0` exactly as `unwrap_or((0.0, 0.0))`
  does today).
- `stacking::psf_signal` re-exports: `pub use crate::star_measure::{Seed, FitParams, StarFit, FitOutcome, PsfModel,
  fwtm_from_alpha, background_residual, noise_mrs, frame_shape, signal_totals, SignalTotals, AUTO_BETAS,
  AUTO_SAMPLE, RCR_LIMIT};` and `pub fn fit_stars(..)`/`fit_stars_with_beta(..)` as one-line wrappers over
  `star_measure::fit::{fit_stars, fit_stars_with_beta}` — every current `use crate::stacking::psf_signal::…` in
  `ln/`, `register/`, `fits_artifact.rs`, `structure.rs`, the examples and the tests compiles unchanged.
- `stacking::measure::measure_plane_detailed` keeps its signature and its seed detection (Peak/Structure/Full on the
  ADU-scaled copy) and hands `SeedSpec::Given(seeds)` to `star_measure::measure_plane`, then builds
  `ChannelMeasurement` from `PlaneMeasurement` (`sky` for `median`/`mad`/`median_mean_dev`/`location`/`scale`/the
  noise-scale factors; `frame.fwhm_px.unwrap_or(0.0)`; `timings` + its own `read_ms`/`detect_ms`) — nothing else
  stays in the adapter but the plane read and the detector (ruling F4 settles the earlier contradiction: the
  sample statistics MOVE). The Median3 arm computes the noise first (as today, through `domain::noise_model` on
  the same ADU copy) and passes it as `MeasureOptions.noise`; the `None` arm lets the module compute it — in both
  arms background/MRS run exactly once, in the same order on the same bytes.
- `test_support::fixture_field(seed: u64, amp_scale: f64, noise: f32) -> (Vec<f32>, usize, usize)` — the 150-star
  fixture moved out of `measure.rs` (it needs only `geometry::ransac::SplitMix64` and `gaussian_field`, both
  ungated), so the module's own tests build under `--features render` without `solver` (ruling F44); tests that
  call the ADAPTER (`crate::stacking::measure::…`) are gated `#[cfg(all(test, feature = "solver"))]`.

- [ ] **Step 1: Write the failing tests** (in `star_measure/mod.rs` `#[cfg(test)]`; the moved tests of
  `psf_signal.rs` travel with their code into `fit.rs`/`frame.rs`/`domain.rs` unchanged; `measure.rs`'s `mod tests`
  becomes `pub(crate) mod tests` and `field` becomes a one-line delegate to `test_support::fixture_field` — M-2,
  F44)

```rust
use super::*;
use crate::test_support::fixture_field;

#[cfg(feature = "solver")]
#[test]
fn module_measure_equals_the_adapter_bit_for_bit() {
    use crate::stacking::measure::{measure_plane_detailed, measure_plane_with_seeds, MeasureOptions as AdapterOptions, SeedSource};
    let (data, w, h) = fixture_field(2026_09_19, 1.0, 0.002);   // 150 Gaussian stars, σ 1.8 px, bg 0.08, noise 0.002
    let adapter = measure_plane_with_seeds(&data, w, h, &AdapterOptions::default(), None, SeedSource::Fast);
    let (pm, seeds) = measure_plane_detailed(&data, w, h, &AdapterOptions::default(), None, SeedSource::Fast);
    let opts = MeasureOptions { seeds: SeedSpec::Given(seeds), ..Default::default() };
    let m = measure_plane(&data, w, h, &opts, None);
    assert_eq!(m.stars, pm.fits);
    assert_eq!(m.beta, adapter.channel.beta);
    assert_eq!(m.frame.fwhm_px.unwrap(), adapter.channel.fwhm_px);
    assert_eq!(m.frame.tflux / ADU_SCALE as f64, adapter.channel.tflux);
    assert_eq!(m.noise.sigma / ADU_SCALE as f64, adapter.channel.noise);
    assert_eq!(m.sky.median, adapter.channel.median);
    assert_eq!(m.sky.scale, adapter.channel.scale);
    assert_eq!(m.sky.noise_scale_high, adapter.channel.noise_scale_high);
}

#[test]
fn a_precomputed_noise_model_is_used_and_not_recomputed() {
    let (data, w, h) = fixture_field(7, 1.0, 0.002);
    let given = NoiseModel { sigma: 123.0, source: NoiseSource::Mrs, m_star: 1.0, n_star: 2.0 };
    let m = measure_plane(&data, w, h, &MeasureOptions { noise: Some(given), ..Default::default() }, None);
    assert_eq!(m.noise, given);
    assert_eq!(m.timings.noise_ms, 0);
    let fresh = measure_plane(&data, w, h, &MeasureOptions::default(), None);
    assert_ne!(fresh.noise.sigma, 123.0);
}

#[test]
fn sky_level_clips_in_the_planes_own_domain() {
    // the same field as ADU: the RawAdu statistics equal the Calibrated ones × 65535 (clip [1, clip−1] ↔ [CLIP_LO, CLIP_HI])
    let (data, w, h) = fixture_field(7, 1.0, 0.002);
    let adu: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
    let a = sky_level(&data, w, h, &Domain::Calibrated { gain_e_per_unit: None }, ScaleEstimator::Bwmv);
    let b = sky_level(&adu, w, h, &Domain::RawAdu { clip: 65535.0, gain_e_per_adu: None }, ScaleEstimator::Bwmv);
    assert!((b.median / ADU_SCALE as f64 - a.median).abs() < 1e-6 * a.median);
    assert!((b.mad / ADU_SCALE as f64 - a.mad).abs() < 1e-6 * a.mad);
    assert!(b.median > 1000.0, "the ADU sample is not empty");
}

/// A pin, not RED (ruling F28): after the move the wrapper and the entry point share one body. The real pins of
/// this task are the probe parity gate and `measure.rs`'s literals.
#[test]
fn fit_stars_at_equals_fit_stars_with_beta() {
    use crate::test_support::{moffat_field, MoffatStar};
    let stars: Vec<MoffatStar> = (0..20).map(|i| MoffatStar { x: 20.0 + (i % 5) as f64 * 40.3, y: 20.0 + (i / 5) as f64 * 40.7,
        amp: 0.2, alpha_x: 5.0, alpha_y: 5.0, theta: 0.0 }).collect();
    let data = moffat_field(220, 180, &stars, 4.0, 0.05);
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect();
    let a = fit_stars_at(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
    let b = fit::fit_stars_with_beta(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
    assert_eq!(a, b);
    assert_eq!(a.fits.len(), 20);
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum && CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- star_measure --test-threads=4`
Expected: `error[E0433]: ... star_measure` (module absent).

- [ ] **Step 3: Move the code verbatim.** `robust.rs` → `star_measure/robust.rs` (file move; `git mv`). From
  `psf_signal.rs`: `Seed`, `FitParams`, `StarFit` (+`not_measured`), `FitOutcome`, `PsfModel`, `fwtm_from_alpha`,
  `initial_sigma`, `stamp_radius`, `moment_ellipse`, `push_finite_row`, `region_median`, `sampling_radius`,
  `FitScratch`, `fit_one`, `accept`, `aperture`, `dedupe`, `fit_all`, `fit_stars`, `fit_stars_with_beta` → `fit.rs`
  (the `AUTO_*` search body of `fit_stars` → `beta::auto_beta(data, w, h, seeds, sigma0, p, pool) -> f64`, called by
  `fit_stars`); `kahan_sum`, `signal_totals`, `SignalTotals`, `frame_shape`, `RCR_LIMIT` → `frame.rs`;
  `background_residual`, `noise_mrs`, `N_STAR_FROM_MAD`, `BACKGROUND_MODEL_CELL_PX`, `MRS_LAYERS`,
  `MRS_LAYER1_GAIN` → `domain.rs`; from `measure.rs`: `ADU_SCALE`, `NoiseSource`, the two noise blocks (→
  `noise_model`) and the sample-statistics block (→ `sky_level`) → `domain.rs`, with `measure.rs` re-exporting
  `ADU_SCALE` and `NoiseSource` so `ChannelMeasurement`'s serialized enum and every `measure::ADU_SCALE` caller are
  untouched. No arithmetic changes, no reordering of operations; `rustfmt crates/athenaeum-core/src/star_measure/*.rs`
  by path. `psf_signal.rs` is left with `PsfModel`'s re-export, the PSFSW/PSFSNR constants and functions,
  `group_beta`, `PSF_FIT_VERSION`, and the moved tests that read the weight formulas. `measure.rs`: hand edit — the
  body from the first `let t = Instant::now();` (background) to the `ChannelMeasurement` literal becomes: the
  detector (unchanged, on the adapter's own `scaled` copy; the Median3 arm calls `domain::noise_model` first and
  passes it as `noise`), then `let m = star_measure::measure_plane(data, w, h, &opts_for_module, pool)`, then the
  `ChannelMeasurement` literal filled from `m`.

- [ ] **Step 4: Build and test** (ruling F15: one filter per invocation)

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- star_measure --test-threads=4
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- stacking:: --test-threads=4
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib --no-default-features --features render -- star_measure --test-threads=4
```

Expected: every pre-existing pin passes untouched, in particular
`the_shipped_none_path_measures_exactly_these_numbers`, `measure_plane_with_seeds_pinned_before_task_3b_rewrite`,
`seed_source_fast_is_bit_identical_to_measure_plane`,
`measure_plane_detailed_returns_the_seeds_it_fitted_and_the_same_measurement`, `fit_stars_with_beta_matches_fit_stars_for_the_resolved_beta`,
`measure_timings_are_components_of_the_plane_duration` (I-1: the per-phase timings still reach `ChannelMeasurement`),
`the_median_prefilter_keeps_a_well_sampled_population` (the Median3 arm with the precomputed noise);
the headless build compiles (the module is `render`-only); the `render`-only TEST build compiles and runs the
module's own tests without `solver` (the module and its ungated tests never name `stacking`, ruling F44).

- [ ] **Step 5: The lab gate — bit-identical after f32 narrowing and equal summary cells** (ruling F33: the gz is
  produced here, `PROBE` is defined, every results dir is created, the summary diff reads the row tables only)

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo build --release -p athenaeum-core --example star_probe --example ln_scale_probe --example measure_probe
export PROBE=$PWD/.superpowers/target-acc/release/examples/star_probe
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab
S=~/.athenaeum-acc/star-lab-a/t02-extract; R=results/plan-a/t02-extract; mkdir -p $S/gates $R ~/.athenaeum-acc/star-lab-a/results/t02-extract
STARLAB_SCRATCH=$S/gates python3 bin/gates.py --only probe_build,probe_parity,seed_parity | tee $R/gates.txt
python3 bin/run_probe.py manifests/v2.json --out $S/probe && python3 bin/run_probe.py manifests/v2.json --out $S/probe --verify
python3 bin/scorecard.py manifests/v2.json --probe-dir $S/probe --erp-dir ~/.athenaeum-acc/star-lab-v2/erp --out $S/scorecard.md --json $S/scorecard.json
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-a/results/t01c-baseline/scorecard.json.gz $S/scorecard.json --out $R/noreg.txt
gzip -k $S/scorecard.json && mv $S/scorecard.json.gz ~/.athenaeum-acc/star-lab-a/results/t02-extract/
python3 bin/scorecard_summary.py $S/scorecard.json --out $R/scorecard-summary.md --full ~/.athenaeum-acc/star-lab-a/results/t02-extract/scorecard.json.gz --title "Plan A t02: extraction"
diff <(grep '^| ' results/baseline-2026-10-07/scorecard-summary.md) <(grep '^| ' $R/scorecard-summary.md | grep -v 'f12.0\|f16.0\|donut\|fit outcomes\|axis order\|saturation flag\|star SNR\|budget coverage') | tee $R/summary-rows-diff.txt
# I-7 + ruling F2: the legacy control's own determinism, once, with THIS binary; an empty output is a failure
for f in sharp/m4/f2.6/i0 background/neb3_4096/i0 osc/f2.6/m4/i0; do for r in 1 2; do $PROBE measure ~/.athenaeum-acc/star-lab-v2/$f.u16.fits --paths legacy-analysis --out $S/ctrl-$r.jsonl; done; n=$(grep -c '"kind": *"star"' $S/ctrl-1.jsonl); test "$n" -gt 0 && cmp $S/ctrl-1.jsonl $S/ctrl-2.jsonl && echo "$f: identical ($n stars)" || echo "$f: NOT identical or empty ($n stars)"; done | tee $R/control-determinism.txt
```

Expected: `probe_parity` PASS with `0 mismatches` against `tierA-lnB-f1`'s stored `.athf` (the OLD binary's artifact
read by the NEW module's numbers — the bit-identity proof after one f32 narrowing); `seed_parity` PASS; the
scorecard diff: every judged row identical (0 moved, and the printed max |Δ| over all paired numeric fields is 0.0);
`summary-rows-diff.txt` empty (the row tables of the baseline summary are reproduced line for line; the title,
`--full` and n/a-count lines are outside the table grep). The six new families now have measure rows (their numbers
are simply recorded here; no bar). The control determinism file reads `identical (N stars)` with N > 0 three
times, which arms PA-15's bit-identity rule; otherwise PA-15's tolerance rule applies from here on (the file says
which).

- [ ] **Step 6: Commit** (athenaeum, no push; lab results committed and pushed)

```
refactor(core): star_measure module - the measure path's fit, beta, frame, noise and sky-statistic code moved verbatim, stacking as the adapter (bit-identical: probe_parity 0 mismatches, every scorecard cell equal)
```
```
results: Plan A t02 extraction - gates, scorecard summary and the zero-move diff
```

**Acceptance:** parity gate 0 mismatches; `noreg.txt` 0 moved and max |Δ| = 0; every pinned test green; headless
and render-only checks green. Numbers: none read as a bar; the baseline cells are reproduced exactly.

---

### Task 3: `MEASURE_VERSION` replaces `PSF_FIT_VERSION` (spec §4.4, ruling PA-4)

**Files:**
- Modify: `crates/athenaeum-core/src/star_measure/mod.rs` (the constant), `crates/athenaeum-core/src/stacking/psf_signal.rs`
  (remove `PSF_FIT_VERSION`), `crates/athenaeum-core/src/stacking/config.rs` (lines 19, 806–878: the three
  subtrees, their doc comments, the `*_with_fit_version` helpers renamed `*_with_measure_version`; the JSON key
  `"psfFitVersion"` → `"measureVersion"`; `all_three_fit_derived_subtree_hashes_follow_the_psf_fit_version`
  (`config.rs:1594`) replaced by the test below), `crates/athenaeum-core/src/stacking/plan.rs` (hand-edit the three
  comment lines 581, 3187, 6410 that name `PSF_FIT_VERSION`, and the pinned literals of
  `stage_hashes_are_pinned_to_golden_literals` at line 7828), `crates/athenaeum-core/src/stacking/measure.rs`
  (hand-edit the doc comment at line 1071), `docs/stacking/README.md` (the Tier C paragraph's `PSF_FIT_VERSION = 3`
  gets a pointer to the new section; the new section records the policy)

**Interfaces:**
- Produces: `star_measure::MEASURE_VERSION: u32 = 4` with the doc comment: 1–3 = `PSF_FIT_VERSION`'s history (copied
  verbatim), 4 = Plan A (the module, F-0…F-7), the bump policy (one bump per merged plan that changes what a fit
  accepts or measures; Plan C = 5). Folded into `measurement_subtree`, `registration_subtree`,
  `normalization_subtree` as `"measureVersion"`. `AnalysisConfig::config_hash` is Plan C's.

- [ ] **Step 1: Write the failing test** (`config.rs` tests; hand-edited)

```rust
#[test]
fn every_fit_reading_stage_hash_follows_measure_version() {
    let cfg = StackingConfig::default();
    for (name, a, b) in [
        ("measurement", measurement_subtree_with_measure_version(&cfg, MEASURE_VERSION), measurement_subtree_with_measure_version(&cfg, MEASURE_VERSION + 1)),
        ("registration", registration_subtree_with_measure_version(&cfg, MEASURE_VERSION), registration_subtree_with_measure_version(&cfg, MEASURE_VERSION + 1)),
        ("normalization", normalization_subtree_with_measure_version(&cfg, MEASURE_VERSION), normalization_subtree_with_measure_version(&cfg, MEASURE_VERSION + 1)),
    ] {
        assert_ne!(stage_hash(&a, &[], &[]), stage_hash(&b, &[], &[]), "{name}");
        assert!(a.get("measureVersion").is_some() && a.get("psfFitVersion").is_none(), "{name}: one key, the new one");
    }
    // the calibration and prepare subtrees carry no measure version: a bump must not recalibrate
    assert!(calibration_subtree(&cfg).get("measureVersion").is_none());
    assert!(prepare_subtree(&cfg).get("measureVersion").is_none());
}
```

  `assert_eq!(MEASURE_VERSION, 4)` is a pin, not RED (ruling F28): it sits beside the constant with the comment
  "Plan A's one bump (PA-4)". `stage_hashes_are_pinned_to_golden_literals` (`plan.rs:7828`, hand edit, M-1): every
  column but `calib` moves — `meas`, `reg`, `ln_ref`, `ln`, `ln_cal` on every line (ruling F34: five columns, and
  the test has no prepare literal); the `calib` column is asserted unchanged with a comment naming this task. The
  Measure-only staleness test named in the spec (`measure.rs:1071`'s doc comment and `plan.rs`'s
  `fits_artifact_fresh` tests, which key on `PSF_FIT_VERSION 3`) reads the constant through `MEASURE_VERSION` now.

- [ ] **Step 2: Run, see it fail, implement, run again** (ruling F15)

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- stacking:: --test-threads=4
grep -rn "PSF_FIT_VERSION\|psfFitVersion" crates/ rustafits/src docs/stacking/README.md
```

Expected (ruling F34, the hits listed): the grep prints ONLY (1) the history lines inside `MEASURE_VERSION`'s doc
comment in `star_measure/mod.rs`, (2) the new test's `a.get("psfFitVersion").is_none()` line, (3) the README's
Tier C paragraph (today's line 1008) and its two earlier mentions (lines 322, 1050) plus the LN-fixes line
(1683) — each a historical record, (4) the three `plan.rs` comment lines rewritten as "formerly
`PSF_FIT_VERSION`". Anything else is a miss.

- [ ] **Step 3: Record the policy** in `docs/stacking/README.md` (new section "One star-measurement path, Plan A",
  ruling R-OSP-1: the constant, its value, the policy, and the risk line "the first run after Plan A re-measures,
  re-registers (mono) and re-normalizes every set").

- [ ] **Step 4: Commit**

```
feat(stacking): MEASURE_VERSION 4 replaces PSF_FIT_VERSION in the measure, register and normalize stage hashes (ruling R-OSP-1; one bump per merged plan)
```

**Acceptance:** the tests above green; the grep's hit list exactly as listed; no lab run (the numbers do not move;
the hashes do, by design).

---

### Task 4: F-0 — `FitDiagnostics`, refusal reasons, `stars_refused`, the flags byte (spec §3 F-0, §5 step 3)

Numbers unchanged; every seed gets an outcome.

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/diagnostics.rs`
- Modify: `star_measure/fit.rs` (`fit_one` returns `Result<StarFit, RefusalReason>` + a diagnostics record;
  `dedupe` records `Duplicate`; `fit_all`/`fit_stars`/`fit_stars_with_beta` collect outcomes; `StarFit.flags`),
  `star_measure/mod.rs` (`PlaneMeasurement.refused`, `.diagnostics`; `MeasureOptions.diagnostics: bool`),
  `stacking/measure.rs` (hand edit: `ChannelMeasurement { stars_refused, refused_by_reason }`),
  `stacking/fits_artifact.rs` (flags byte), `examples/star_probe.rs` (`refused` lines; `--diagnostics`),
  `crates/athenaeum-core/src/test_support.rs` (`StarFit::test_at`), the `StarFit` struct literals the new field
  breaks (ruling F36, hand edits in never-rustfmt files): `stacking/ln/scale.rs:2469` (`fit_at`),
  `stacking/ln/selection.rs:242`, `stacking/register/detect.rs:531-650`, `stacking/register/frame.rs:915`,
  `stacking/run.rs:16314` — each literal gains `flags: StarFlags::default()` (or calls `StarFit::test_at`),
  `docs/stacking/README.md`
- Lab: `probe_plan.STAR_PROBE_ARGS` default gains `--diagnostics` from this task on (ruling F22: the lab's own
  `run_probe.py` passes it; the flag exists from this task, so no earlier run ever saw it)

**Interfaces:**
- Produces:

```rust
// diagnostics.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefusalReason {
    /// The width-start box (Task 6b) / the nominal stamp (today) leaves the image.
    OutsideImage,
    /// Fewer than 10 finite pixels in the region.
    TooFewPixels,
    /// The core is darker than the ring (Task 6b's donut test).
    Donut,
    /// The fitter returned `None` (α < 0.3, A ≤ 0, β out of bounds).
    FitFailed,
    NonFinite,
    ResidualTooLarge,
    CentreMoved,
    CentreAtRim,
    RegionTooSmall,
    /// `min(fwhm_x, fwhm_y) < 0.8` px (Task 6b; a hot pixel that got a seed).
    NarrowerThanPixel,
    Duplicate,
}

/// Every seed's outcome (ruling F13): `NotSelected` and `Trimmed` are Budget-mode outcomes (Task 12), never seen
/// in `Mode::All`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome { Accepted { star: usize }, Refused(RefusalReason), NotSelected, Trimmed }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitDiagnostics {
    pub seed: usize,
    pub outcome: Outcome,
    pub region_radius: u16,
    pub background_start: f64,
    pub width_start: f64,
    pub growth_steps: u8,
    pub iterations: Option<u16>,   // None until the rustafits entry point reports it (Task 9)
    pub converged: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RefusalCounts { pub by_reason: BTreeMap<RefusalReason, usize>, pub not_selected: usize, pub trimmed: usize }
impl RefusalCounts {
    /// Refused seeds only (the F-0 count).
    pub fn total(&self) -> usize;
    pub fn add(&mut self, r: RefusalReason);
    /// `accepted + refused + not_selected + trimmed` — the identity's left-hand side (ruling F13).
    pub fn accounted(&self, accepted: usize) -> usize;
}
```

  `StarFlags(u8)` in `fit.rs`: `SATURATED = 1`, `BETA_AT_CAP = 2` (set from Task 8 / Task 9; written and read as the
  `.athf` flags byte now; a reader of an older file reads 0). `StarFit.flags: StarFlags` with `#[serde(default)]`;
  `test_support::StarFit::test_at(x, y, amplitude) -> StarFit` (`#[cfg(test)]`, every other field a plausible
  default) for the fixture sites. `FitOutcome.refused: RefusalCounts`. `ChannelMeasurement.stars_refused: usize`
  and `refused_by_reason: BTreeMap<String, usize>` (serde default; the `metrics` payload grows two keys, every
  older payload still decodes). `MeasureOptions.diagnostics: bool` (the probe sets it; the run never does — the
  per-seed vector costs memory on 24576 seeds).
- The invariant (ruling F13): `accepted + refused.total() + refused.not_selected + refused.trimmed ==
  seeds` in every mode — a `debug_assert!` inside `measure_plane`, a test, and the scorecard's `fit outcomes` row.
- How the outcomes are collected (M-4): the same `seeds.par_iter().map_init(FitScratch::default, …)` chain now
  yields `Outcome` per seed and is `collect()`ed into a `Vec` in seed order (rayon's indexed collect keeps the
  order); the accepted fits are then extracted sequentially — never a `Mutex` or a channel — so `dedupe`'s input
  and the Kahan sums of `signal_totals` see exactly the order they see today and the pins stay exact.

- [ ] **Step 1: Write the failing tests** (`star_measure/mod.rs` and `fit.rs` tests)

```rust
#[test]
fn every_seed_has_exactly_one_outcome_and_the_numbers_do_not_move() {
    let (data, w, h) = fixture_field(7, 1.0, 0.002);
    let seeds = test_seeds_of(&data, w, h);          // the 150 star centres as seeds (peak/flux from the fixture's amplitudes)
    let before = measure_plane(&data, w, h, &MeasureOptions { seeds: SeedSpec::Given(seeds.clone()), ..Default::default() }, None);
    let m = measure_plane(&data, w, h, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(seeds.clone()), ..Default::default() }, None);
    assert_eq!(m.diagnostics.len(), seeds.len());
    assert_eq!(m.refused.accounted(m.stars.len()), seeds.len());
    assert_eq!(m.refused.not_selected + m.refused.trimmed, 0, "Mode::All has no budget outcomes");
    let accepted = m.diagnostics.iter().filter(|d| matches!(d.outcome, Outcome::Accepted { .. })).count();
    assert_eq!(accepted, m.stars.len());
    assert_eq!(m.stars, before.stars, "diagnostics change no number");
}

#[test]
fn edge_seed_is_refused_outside_image_with_its_reason() {
    let data = vec![0.05f32; 64 * 64];
    let opts = MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(vec![Seed { x: 2.0, y: 30.0, peak: 0.5, flux: 5.0 }]), ..Default::default() };
    let m = measure_plane(&data, 64, 64, &opts, None);
    assert_eq!(m.diagnostics[0].outcome, Outcome::Refused(RefusalReason::OutsideImage));
    assert_eq!(m.refused.by_reason[&RefusalReason::OutsideImage], 1);
    assert_eq!(m.refused.total(), 1);
    assert!(m.stars.is_empty());
}

#[test]
fn starless_plane_has_no_outcomes_and_no_panic() {
    let data = vec![0.05f32; 128 * 128];
    let m = measure_plane(&data, 128, 128, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(vec![]), ..Default::default() }, None);
    assert!(m.stars.is_empty() && m.diagnostics.is_empty() && m.refused.total() == 0);
    assert_eq!(m.frame.fwhm_px, None);
}

#[test]
fn duplicate_and_rim_refusals_are_counted_by_reason() {
    use crate::test_support::{moffat_field, MoffatStar};
    // one star, two seeds 0.5 px apart → one Accepted, one Duplicate
    let star = MoffatStar { x: 40.3, y: 40.7, amp: 0.5, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 };
    let data = moffat_field(80, 80, &[star], 4.0, 0.05);
    let seeds = vec![Seed { x: 40.3, y: 40.7, peak: 0.5, flux: 3.0 }, Seed { x: 40.8, y: 40.7, peak: 0.49, flux: 2.9 }];
    let m = measure_plane(&data, 80, 80, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(seeds), psf_model: PsfModel::Moffat4, ..Default::default() }, None);
    assert_eq!(m.stars.len(), 1);
    assert_eq!(m.refused.by_reason.get(&RefusalReason::Duplicate), Some(&1));
    // a seed whose region cannot hold half the FWTM (the `a_fit_settled_at_the_region_rim_is_rejected` fixture:
    // a broad star, α 12, on a 60×60 plane whose border caps r at 20) → RegionTooSmall, not a silent drop
    let broad = MoffatStar { x: 30.0, y: 30.0, amp: 0.5, alpha_x: 12.0, alpha_y: 12.0, theta: 0.0 };
    let data = moffat_field(60, 60, &[broad], 4.0, 0.05);
    let m = measure_plane(&data, 60, 60, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(vec![Seed { x: 30.0, y: 30.0, peak: 0.5, flux: 50.0 }]), psf_model: PsfModel::Moffat4, ..Default::default() }, None);
    assert_eq!(m.diagnostics[0].outcome, Outcome::Refused(RefusalReason::RegionTooSmall));
    assert!(m.diagnostics[0].region_radius >= 3);
}

// fits_artifact.rs
#[test]
fn flags_byte_round_trips_and_an_old_file_reads_zero() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.athf");
    let mut fit = StarFit::test_at(10.0, 20.0, 1000.0);
    fit.flags = StarFlags::SATURATED | StarFlags::BETA_AT_CAP;
    write_fits(&p, &[fit]).unwrap();
    let back = read_fits(&p).unwrap();
    assert_eq!(back[0].flags, StarFlags::SATURATED | StarFlags::BETA_AT_CAP);
    // a v1 record with the reserved byte 0 (today's files)
    let mut bytes = std::fs::read(&p).unwrap();
    let last = bytes.len() - 1;
    bytes[last] = 0;
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(read_fits(&p).unwrap()[0].flags, StarFlags::default());
}
```

  `test_seeds_of(plane, w, h) -> Vec<Seed>` is a `#[cfg(test)]` helper in `star_measure/mod.rs` (ruling F38): the
  fixture's 150 grid positions (it rebuilds the same `SplitMix64` stream as `fixture_field`) with `peak` = the
  plane's maximum in a 3×3 box about each and `flux = 2π·σ²·peak`; brightest first.

- [ ] **Step 2: Run them and see them fail; implement; run again** (ruling F15)

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- star_measure --test-threads=4
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- stacking:: --test-threads=4
```

Expected: the pre-existing pins still exact (`the_shipped_none_path_measures_exactly_these_numbers`, …); the five
hand-edited `StarFit` literal sites compile (ruling F36).

- [ ] **Step 3: The probe** — `fit_line` gains `"flags": ["saturated", …]`; `--diagnostics` writes one `refused`
  line per refused seed and `"refused": {"total": n, "byReason": {...}, "notSelected": 0, "trimmed": 0}` on the
  frame line; `seeds`/`fits`/`refused` must satisfy the identity. Probe test:

```rust
#[test]
fn refused_lines_plus_fits_equal_seeds() {
    let d = tempfile::tempdir().unwrap();
    let mut a = args(d.path().join("p.fits"), d.path().join("p.jsonl"), &["measure"]);
    a.diagnostics = true;
    write_fits_f32(&a.image, 240, 240, 1, &gaussian_field(240, 240, 6, true), &[]).unwrap();   // the NaN border refuses some seeds
    cmd_measure(a).unwrap();
    let lines = read_lines(&d.path().join("p.jsonl"));
    let frame = lines.iter().find(|v| v["kind"] == "frame" && v["path"] == "measure").unwrap();
    let refused = lines.iter().filter(|v| v["kind"] == "refused").count();
    assert_eq!(refused as u64, frame["refused"]["total"].as_u64().unwrap());
    assert_eq!(frame["fits"].as_u64().unwrap() + refused as u64, frame["seeds"].as_u64().unwrap());
    assert!(lines.iter().filter(|v| v["kind"] == "refused").all(|v| v["reason"].is_string() && v["regionRadius"].is_u64()));
}
```

- [ ] **Step 4: The lab cycle (PA-16)** for `t04-f0` with `STAR_PROBE_ARGS="--diagnostics"` (ruling F22; from here
  on every lab run): build, run, verify, scorecard, `noreg.txt` against t02 (design list: none), `noreg-cumulative.txt`
  against t01c (ruling F21), summary. Bar rows: row 1 `fit outcomes` — `ok` on every image. Diff: 0 moved rows;
  the legacy control 0. Delete `~/.athenaeum-acc/star-lab-a/t02-extract/probe` after the t04 summary is committed
  (PA-3).

- [ ] **Step 5: Commit**

```
feat(star_measure): F-0 FitDiagnostics - every seed has an outcome and a refusal reason; stars_refused in ChannelMeasurement; the .athf flags byte (numbers unchanged, fit outcomes = seeds on every image)
```
```
results: Plan A t04 F-0 - fit-outcome rows and the zero-move diff
```

**Acceptance:** `fit outcomes` ok on 100 % of images; `noreg.txt` and `noreg-cumulative.txt` 0 moved; the F-0 bar
"accepted + refused-by-reason = seeds on every image; numbers unchanged against the extraction step" — both read
from `results/plan-a/t04-f0/`. Baseline for the record: `sharp/m4/f8.0` measure `82.7 % of 301 expected` against
seeds `92.0 %`; the row now names the reasons behind the gap (expected mostly `RegionTooSmall`; recorded, not
judged here — F-1 judges it).

---

### Task 5: F-2 — canonical axes and a conditioned fit (spec §3 F-2)

**Files:**
- Modify: `star_measure/fit.rs` (`accept`: axis order; `fit_one`: stamp-centred, amplitude-normalised pixel list and
  the back-transform), `examples/star_probe.rs` (nothing), `docs/stacking/README.md` (R-OSP-3)
- Conditional (step 7): `rustafits/src/analysis/fitting.rs` on branch `one-star-path-beta` — a NEW entry point
  only (ruling F7)

**Interfaces:**
- Produces: every accepted `StarFit` has `fwhm_x >= fwhm_y` and `theta` is the axis of `fwhm_x`, in `[-π/2, π/2)`;
  `fwtm_x/fwtm_y` follow the swap; `aperture` reads the swapped pair (its ellipse is the same geometric object).
  Inside `fit_one` the fitter sees `PixelSample { x: px − cx, y: py − cy, value: (v − b0)/a0 }`, starts at `B = 0,
  A = 1, x0 = seed.x − cx, y0 = seed.y − cy`, and the result is mapped back (`x0 + cx`, `B·a0 + b0`, `A·a0`;
  `fit_residual` is already amplitude-normalised so it is unchanged in meaning). `conv_tol` and the cost test are
  unchanged in value.

- [ ] **Step 1: Write the failing tests** (`fit.rs` tests)

```rust
use crate::test_support::{moffat_field, MoffatStar};

fn grid(n: usize, alpha_x: f64, alpha_y: f64, theta: f64) -> (Vec<MoffatStar>, Vec<Seed>) {
    let stars: Vec<MoffatStar> = (0..n).map(|i| MoffatStar { x: 30.0 + (i % 8) as f64 * 40.0, y: 30.0 + (i / 8) as f64 * 40.0,
        amp: 0.3, alpha_x, alpha_y, theta }).collect();
    let seeds = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect();
    (stars, seeds)
}

#[test]
fn accepted_fits_have_canonical_axes() {
    // αx 3 / αy 6 at θ = 20°: the generator's u axis (θ) is the MINOR one; the canonical fit must report the major
    // axis on fwhm_x and θ_canonical = 20° + 90° (mod 180°)
    let (stars, seeds) = grid(40, 3.0, 6.0, 20f64.to_radians());
    let data = moffat_field(360, 240, &stars, 4.0, 0.05);
    let out = fit_stars_at(&data, 360, 240, &seeds, 4.0, &FitParams::default(), None);
    assert!(out.fits.len() >= 36, "{}", out.fits.len());
    let want = (20f64 + 90.0).to_radians();
    for f in &out.fits {
        assert!(f.fwhm_x >= f.fwhm_y, "{f:?}");
        let d = ((f.theta - want + std::f64::consts::FRAC_PI_2).rem_euclid(std::f64::consts::PI) - std::f64::consts::FRAC_PI_2).abs();
        assert!(d < 2f64.to_radians(), "theta {} vs {want}", f.theta);
        assert!((f.fwtm_x / f.fwhm_x - f.fwtm_y / f.fwhm_y).abs() < 1e-9);
        assert!(f.theta >= -std::f64::consts::FRAC_PI_2 && f.theta < std::f64::consts::FRAC_PI_2);
    }
}

#[test]
fn fit_is_translation_and_scale_invariant() {
    let star = |x, y, amp| MoffatStar { x, y, amp, alpha_x: 3.0, alpha_y: 4.5, theta: 0.5 };
    let near = moffat_field(80, 80, &[star(30.3, 20.7, 0.3)], 4.0, 0.05);
    let far = moffat_field(3100, 2100, &[star(3000.3, 2000.7, 0.3)], 4.0, 0.05);
    let bright = moffat_field(80, 80, &[star(30.3, 20.7, 300.0)], 4.0, 50.0);
    let f = |d: &[f32], w, h, x, y, p| fit_stars_at(d, w, h, &[Seed { x, y, peak: p, flux: 1.0 }], 4.0, &FitParams::default(), None).fits[0];
    let a = f(&near, 80, 80, 30.3, 20.7, 0.3);
    let b = f(&far, 3100, 2100, 3000.3, 2000.7, 0.3);
    let c = f(&bright, 80, 80, 30.3, 20.7, 300.0);
    let rel = |p: f64, q: f64| ((p - q) / q).abs();
    assert!(rel(b.x - 2970.0, a.x) < 1e-9 && rel(b.y - 1980.0, a.y) < 1e-9, "translation: {a:?} {b:?}");
    for (p, q) in [(a.fwhm_x, b.fwhm_x), (a.fwhm_y, b.fwhm_y), (a.theta, b.theta), (a.residual, b.residual)] {
        assert!(rel(p, q) < 1e-9, "{p} vs {q}");
    }
    assert!(rel(c.amplitude, 1000.0 * a.amplitude) < 1e-9 && rel(c.background, 1000.0 * a.background) < 1e-9);
    assert!(rel(c.fwhm_x, a.fwhm_x) < 1e-9 && rel(c.residual, a.residual) < 1e-9);
}

/// A pin, not RED (ruling F28): `fwhm()` and `eccentricity()` are symmetric in x/y by construction.
#[test]
fn eccentricity_and_fwhm_are_unchanged_by_the_swap() {
    let mut f = StarFit::test_at(1.0, 1.0, 1.0);
    f.fwhm_x = 2.0; f.fwhm_y = 3.0;
    let (fw, e) = (f.fwhm(), f.eccentricity());
    std::mem::swap(&mut f.fwhm_x, &mut f.fwhm_y);
    assert_eq!((f.fwhm(), f.eccentricity()), (fw, e));
}
```

- [ ] **Step 2: Run, fail, implement, run.** Then every pinned measurement literal in `measure.rs` and
  `psf_signal.rs` tests moves BY DESIGN (the conditioning changes when every fit stops — today's `‖Δp‖/‖p‖ < 1e-5`
  spans absolute pixel coordinates — and the axis swap changes θ): update each literal with a one-line comment
  "F-2 (Task 5): conditioning moved the fit; the previous literal was …". `fits_artifact::decode` of an OLD `.athf`
  keeps the old axis order (it is stale by `MEASURE_VERSION` anyway). Earlier tests this task edits (ruling F14):
  none — the signatures are Task 2's.

- [ ] **Step 3: Lab cycle** `t05-f2`, design list (PA-19 syntax; every `betas@*` entry is its `measure` twin, ruling F20 as folded):
  `{"row":4,"metric":"angle","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"angle","path":"betas@*","kind":"set-aside"}`,
  `{"row":4,"metric":"axis order","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"axis order","path":"betas@*","kind":"set-aside"}`,
  `{"row":4,"metric":"ecc","family":"shape/*","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"ecc","family":"shape/*","path":"betas@*","kind":"set-aside"}`,
  `{"row":3,"metric":"fwhm axis","path":"measure","kind":"set-aside"}`, `{"row":3,"metric":"fwhm axis","path":"betas@*","kind":"set-aside"}`,
  `{"row":2,"metric":"centroid","path":"measure","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (the conditioning may move centroids within their CRLB),
  `{"row":7,"path":"measure","kind":"set-aside"}`, `{"row":7,"path":"betas@*","kind":"set-aside"}`, `{"row":11,"path":"measure","kind":"set-aside"}`,
  `{"row":11,"path":"betas@*","kind":"set-aside"}`,
  `{"row":3,"path":"measure","kind":"one-sided"}`, `{"row":3,"path":"betas@*","kind":"one-sided"}`, `{"row":5,"path":"measure","kind":"one-sided"}`,
  `{"row":5,"path":"betas@*","kind":"one-sided"}` (I-5: widths and backgrounds move by more than a last bit; a row that
  got worse is a miss that goes to the owner, one that got better is recorded; the `fwhm axis` set-asides above are
  the more specific entries and win on their rows),
  `{"row":3,"metric":"kernel growth","path":"measure","kind":"one-sided","keys":["scatter"]}`,
  `{"row":3,"metric":"kernel growth","path":"betas@*","kind":"one-sided","keys":["scatter"]}` (the row carries
  `scatter` and no `bias`, PA-19),
  `{"row":3,"metric":"beta sensitivity","path":"betas","kind":"reported"}`, `{"row":3,"metric":"auto beta pick","path":"measure","kind":"reported"}`
  (no bias/scatter — reported, PA-19), and — ruling F10 — `{"row":1,"path":"register","kind":"reported"}`,
  `{"row":2,"path":"register","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (the mono
  register rows derive from the Measure fits). Row 6 `captured fraction` (`measure` and `betas@*`) is judged within SE.

- [ ] **Step 4: Read the bar.** Row 4 `axis order` = 100 % on every family × path `measure`; row 4 `angle` on the
  `shape/*` families at e ≥ 0.3, the `eff` cell (prepixel CRLB): ≤ 1.5 on each of the 24 families with e ∈ {0.3,
  0.5, 0.7}. Baseline for the record: `shape/f2.6/e0.3/t30` rms 44.564°, `e0.5/t30` 32.339° (ERP 7.849° / 2.129°).

- [ ] **Step 5: `path_compare.py` part 2** re-run on the t05 outputs (`--probe-dir ~/.athenaeum-acc/star-lab-a/t05-f2/probe`,
  Task 1b) → `results/plan-a/t05-f2/path-compare.txt`: the "fwhm_x < fwhm_y" counts must be 0 of N on every `e0.3`
  family (baseline 71–145 of 346–427).

- [ ] **Step 6: No-regression**: `noreg.txt` 0 moved outside the design list and 0 one-sided misses; `noreg-cumulative.txt`
  (vs t01c, union of the lists so far) likewise; control 0.

- [ ] **Step 7 (conditional): the rustafits convergence change.** Trigger: ANY `shape` family with e ≥ 0.3 reads angle
  `eff > 1.5` after step 4. Then, and only then: create the rustafits branch `one-star-path-beta` (from `8321412`)
  and add a NEW entry point `fit_moffat_2d_fixed_beta_conditioned(…)` whose solver option `ConvergenceTest::PerParameter`
  replaces `param_converged` by `|Δp_i| ≤ conv_tol·max(|p_i|, 1)` per parameter (the normalisation makes every
  parameter O(1)), keeping `residual_converged`; `fit_moffat_2d` and `fit_moffat_2d_fixed_beta` keep
  `ConvergenceTest::VectorNorm` and stay byte-identical (ruling F7 — the legacy control cannot move; the C3 pins
  are untouched). The module's `fit_one` calls the new entry point. Rustafits test
  `conditioned_convergence_does_not_stop_on_a_theta_step`:

```rust
#[test]
fn conditioned_convergence_does_not_stop_on_a_theta_step() {
    // a noiseless elliptical Moffat (αx 2.5, αy 4, θ 30°, β 4) sampled on a 31×31 stamp, started with θ off by 5°
    // and everything else exact: the conditioned solver ends within 1° of 30°; the vector-norm solver is recorded
    let pixels = moffat_stamp(31, 15.0, 15.0, 2.5, 4.0, 30f64.to_radians(), 4.0);
    let r = fit_moffat_2d_fixed_beta_conditioned(&pixels, 0.0, 1.0, 15.0, 15.0, 2.5 * 0.6, 4.0 * 0.6, 35f64.to_radians(), 4.0, 100, 1e-5, 5).unwrap();
    assert!((r.theta.to_degrees() - 30.0).abs() < 1.0, "theta {}", r.theta.to_degrees());
}
```

  then bump the athenaeum submodule pointer and re-run steps 3–6. If the trigger does not fire, the rustafits
  branch is created in Task 9 instead. Either way the number that decided is recorded (R-OSP-3).

- [ ] **Step 8: Commit**

```
fix(star_measure): F-2 canonical axes (fwhm_x >= fwhm_y, theta on the major axis) and a stamp-centred, amplitude-normalised fit (angle eff <= 1.5 on shape e >= 0.3: <numbers>)
```

**Acceptance:** `axis order` 100 %; angle `eff` ≤ 1.5 on every `shape` family at e ≥ 0.3 (24 rows, each printed in
`results/plan-a/t05-f2/bars.txt`); part 2 counts 0; both diffs 0 moved outside the design list and 0 one-sided
rows missed. A miss → the owner, with the 24 cells, before Task 6a.

---

### Task 6a: F-1, the seeds — the matched filter, the mask, the field width, the PA-2 threshold sweep (spec §2.1 `seeds`, §3 F-1)

No fit change in this task (ruling F16): `fit_one` keeps `initial_sigma`/`stamp_radius` until Task 6b;
`field_width_of` serves the matched filter's second pass and `PlaneMeasurement.field_width` only. The seeds change;
the threshold is chosen on the `seeds` rows before any fitted bar exists (PA-2 made structural).

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/seeds.rs`
- Modify: `star_measure/mod.rs` (`SeedSpec::MatchedFilter { threshold }`, `MeasureOptions.mask: Option<Vec<bool>>`,
  `FitStarsOptions { fit: FitParams, mask: Option<Vec<bool>>, field_width: Option<f64> }` + `Default` —
  `fit_stars_at(plane, w, h, seeds, beta, opts: &FitStarsOptions, pool)` replaces Task 2's `fit: &FitParams`
  argument; `PlaneMeasurement.field_width: FieldWidth`), `star_measure/fit.rs` (the mask honoured in the region
  and the aperture — a masked pixel is absent like a non-finite one; `field_width` threaded to `fit_one` for Task
  6b, unread here), `star_measure/domain.rs` (`NoiseSource::None`; the `metrics` payload enum gains `none`, ruling
  F43), `stacking/measure.rs` (hand edit: `SeedSource` gains the variant `MatchedFilter` — ruling F26: the probe
  override enum keeps its name here, Task 7 renames it; the adapter default stays `Fast` → Peak until Task 7),
  `examples/star_probe.rs` (`--seeds matched|peak|structure`, `--seed-threshold K`; default `matched` from this
  task on; the frame line's `options.measure` gains `seedSource` and `seedThreshold` = the EFFECTIVE source and
  threshold the probe ran, override included — ruling F3), `crates/athenaeum-core/src/test_support.rs`
  (`nan_border_field(w, h, border, stars) -> Vec<f32>`, the probe's fixture moved so a lib test reaches it — ruling
  F38), `docs/stacking/README.md` (R-OSP-2 the threshold choice); `star_measure/fit.rs` also gains
  `FitOutcome.field_width` (Interfaces below, ruling F14)
- Earlier tests this task edits (ruling F14): Task 2's `fit_stars_at_equals_fit_stars_with_beta` and Task 5's three
  `fit_stars_at` calls take `&FitStarsOptions::default()`.

**Interfaces:**
- Produces (`seeds.rs`):

```rust
/// A Gaussian matched filter at the field width, in a separable convolution over the finite, unmasked pixels
/// (a non-finite or masked pixel contributes nothing and normalises nothing — the border is not a star).
pub struct MatchedFilter { pub fwhm_px: f64 }
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldWidth { pub fwhm_px: f64, pub from_stars: usize }
pub const DEFAULT_THRESHOLD: f64 = <k chosen by PA-2 in step 4>;
/// Two passes: a 3-px kernel finds the 100 brightest candidates, their half-maximum widths (`halfmax_width`,
/// PA-17) give the field width (median over ALL of them, saturated ones included — no saturation test runs here,
/// ruling F25 as folded; clamped to [1.5, 24] px); the second pass runs at that width. A field
/// width ≥ 6 px runs the second pass on the 2×2 super-pixel plane (kernel halved; candidate positions ×2,
/// refined on the full plane). Candidates: local maxima of the filtered plane above `threshold·σ_f` (σ_f =
/// 1.4826·MAD of the filtered plane over the stratified sample) with ≥ 2 of 4 cross-neighbours above `background
/// + threshold·σ_plane` on the UNFILTERED plane (the hot-pixel rule). Position: the flux-weighted centroid in a
/// (2·ceil(F)+1)² box on the background-subtracted plane; `peak` = that box's maximum − background; `flux` = the
/// filtered response (the matched filter's integrated signal — the selection key). Sorted by `flux` descending,
/// truncated to `max_stars`. A `NoiseModel` with `source == NoiseSource::None` returns no seeds and one `warn!`
/// (Review Focus 4). Public: Plan C calls it on its own 2×2 super-pixel OSC plane (I-8).
pub fn detect(plane: &[f32], w: usize, h: usize, noise: &NoiseModel, threshold: f64, max_stars: usize,
              mask: Option<&[bool]>, pool: Option<&Arc<rayon::ThreadPool>>) -> (Vec<Seed>, FieldWidth);
/// PA-17: the azimuthal half-maximum width of one star — rings of 0.5 px about the brightest pixel in a box of
/// half-width `box_r`, background = the box rim's median, FWHM = 2 × the radius where the ring-mean profile
/// crosses half the brightest pixel (linear interpolation between the bracketing rings). `None` when the profile
/// never crosses inside the box or the box leaves the image/mask.
pub fn halfmax_width(plane: &[f32], w: usize, h: usize, x: f64, y: f64, box_r: usize, mask: Option<&[bool]>) -> Option<f64>;
/// The field width of GIVEN seeds (I-4, PA-17): the median `halfmax_width` of the ≤ 100 brightest, all of them —
/// no saturation test (ruling F25 as folded) — (box 15 px; a first read ≥ 7 px is re-measured in a `3·F` box,
/// ≤ 48), clamped to [1.5, 24] px. `fit_stars_at` calls it when
/// `FitStarsOptions.field_width` is `None` (LN refits, Peak/Structure seeds, Plan C's mosaic sites); Task 6b's
/// width start reads it; this task's fit does not.
pub fn field_width_of(plane: &[f32], w: usize, h: usize, seeds: &[Seed], mask: Option<&[bool]>) -> FieldWidth;
```

- The adapter: `SeedSource::MatchedFilter` (probe override) runs `seeds::detect` on the ADU-scaled plane with the
  module's `NoiseModel`; `Fast`/`Full`/`Structure` unchanged.
- `FitOutcome` gains `pub field_width: FieldWidth` (ruling F14): the width `fit_stars_at` used —
  `FieldWidth { fwhm_px: f, from_stars: 0 }` when `FitStarsOptions.field_width` is `Some(f)`, `field_width_of` of
  the given seeds when it is `None`. `given_seeds_get_a_field_width_from_the_halfmax_profile` reads it.

- [ ] **Step 1: Write the failing tests** (`seeds.rs` and `mod.rs` tests; `domain::to_working_units` and
  `domain::noise_model` are Task 2's helpers — ruling F38)

```rust
use crate::test_support::{fixture_field, nan_border_field, moffat_field, MoffatStar};

fn working(data: &[f32]) -> Vec<f32> { data.iter().map(|v| v * ADU_SCALE).collect() }

#[test]
fn matched_filter_finds_every_star_of_the_fixture_and_no_noise_peak() {
    let (data, w, h) = fixture_field(7, 1.0, 0.002);          // 150 stars, σ 1.8 px (FWHM 4.24)
    let scaled = working(&data);
    let (noise, _) = domain::noise_model(&scaled, w, h, None);
    let (seeds, fw) = seeds::detect(&scaled, w, h, &noise, DEFAULT_THRESHOLD, 24576, None, None);
    assert_eq!(seeds.len(), 150);
    assert!((fw.fwhm_px - 1.8 * 2.3548).abs() < 0.3, "{fw:?}");
    assert!(seeds.windows(2).all(|p| p[0].flux >= p[1].flux));
    assert!(seeds.iter().all(|s| s.peak > 0.0));
}

#[test]
fn nan_border_seeds_nothing_on_the_border_and_fits_every_interior_star() {
    let data = nan_border_field(240, 240, 6, true);           // 40 Gaussian stars at FWHM 3.5, a 6-px NaN border
    let scaled = working(&data);
    let (noise, _) = domain::noise_model(&scaled, 240, 240, None);
    let (seeds, _) = seeds::detect(&scaled, 240, 240, &noise, DEFAULT_THRESHOLD, 24576, None, None);
    assert_eq!(seeds.len(), 40);
    assert!(seeds.iter().all(|s| s.x >= 14.0 && s.y >= 14.0), "no seed within 8 px of the border");
    let m = measure_plane(&data, 240, 240, &MeasureOptions { seeds: SeedSpec::MatchedFilter { threshold: DEFAULT_THRESHOLD }, ..Default::default() }, None);
    assert_eq!(m.stars.len(), 40);
}

#[test]
fn refused_noise_estimate_seeds_nothing_and_warns() {
    let data = vec![0.05f32; 256 * 256];                        // a constant plane: MRS at its floor, no residuals
    let m = measure_plane(&data, 256, 256, &MeasureOptions { seeds: SeedSpec::MatchedFilter { threshold: DEFAULT_THRESHOLD }, ..Default::default() }, None);
    assert_eq!(m.noise.source, NoiseSource::None);
    assert_eq!(m.seeds, 0);
    assert!(m.stars.is_empty());
}

#[test]
fn hot_pixel_is_not_seeded() {
    let (mut data, w, h) = fixture_field(3, 0.0, 0.002);       // the noise field without stars
    data[100 * w + 100] = 0.9;
    let scaled = working(&data);
    let (noise, _) = domain::noise_model(&scaled, w, h, None);
    let (seeds, _) = seeds::detect(&scaled, w, h, &noise, DEFAULT_THRESHOLD, 24576, None, None);
    assert!(seeds.iter().all(|s| (s.x - 100.0).abs() > 2.0 || (s.y - 100.0).abs() > 2.0), "{seeds:?}");
}

#[test]
fn given_seeds_get_a_field_width_from_the_halfmax_profile() {
    // PA-17: Moffat-4 at FWHM 5 (α = 5/(2√(2^{1/4} − 1)) = 5.748) reads +1.8 % noiseless; the bar is the estimator's own 10 %
    let stars: Vec<MoffatStar> = (0..30).map(|i| MoffatStar { x: 40.0 + (i % 6) as f64 * 60.0, y: 40.0 + (i / 6) as f64 * 60.0,
        amp: 0.3, alpha_x: 5.748, alpha_y: 5.748, theta: 0.0 }).collect();
    let mut data = moffat_field(400, 340, &stars, 4.0, 0.05);
    crate::test_support::add_noise(&mut data, 0.006, 1);        // peak SNR 50
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect();
    let fw = seeds::field_width_of(&working(&data), 400, 340, &seeds, None);
    assert_eq!(fw.from_stars, 30);
    assert!((fw.fwhm_px / 5.0 - 1.0).abs() < 0.10, "{fw:?}");
    let out = fit_stars_at(&working(&data), 400, 340, &seeds, 4.0, &FitStarsOptions::default(), None);
    assert!((out.field_width.fwhm_px - fw.fwhm_px).abs() < 1e-9, "fit_stars_at computed the same width");
}

#[test]
fn pixel_mask_excludes_masked_pixels_from_detection_and_fit() {
    let (data, w, h) = fixture_field(7, 1.0, 0.002);
    let mask: Vec<bool> = (0..w * h).map(|i| (i % w + i / w) % 2 == 0).collect();   // a checkerboard
    let scaled = working(&data);
    let (noise, _) = domain::noise_model(&scaled, w, h, None);
    let (seeds, _) = seeds::detect(&scaled, w, h, &noise, DEFAULT_THRESHOLD, 24576, Some(&mask), None);
    assert!(seeds.len() >= 140, "{}", seeds.len());
    let m = measure_plane(&data, w, h, &MeasureOptions { diagnostics: true, mask: Some(mask.clone()), seeds: SeedSpec::Given(seeds.clone()), ..Default::default() }, None);
    assert!(m.stars.len() >= 140);
    // the fit's pixel count is the unmasked count of its region: (2r+1)² / 2 ± 1
    for d in m.diagnostics.iter().filter(|d| matches!(d.outcome, Outcome::Accepted { .. })).take(5) {
        let r = d.region_radius as usize; let n = (2 * r + 1) * (2 * r + 1);
        assert!((d.pixels_used as i64 - (n / 2) as i64).abs() <= 1, "{d:?}");
    }
}
```

  `FitDiagnostics` gains `pixels_used: u32` here (the region's finite, unmasked pixel count). `MeasureOptions.mask`
  is `Option<Vec<bool>>` on the options (one plane, one mask; `fit_stars_at` takes it through `FitStarsOptions`).

- [ ] **Step 2: Run, fail, implement, run** (`cargo test -p athenaeum-core --lib -- star_measure --test-threads=4`,
  then `-- stacking:: --test-threads=4`). The `measure.rs` pins HOLD exactly: the adapter default is still Peak and
  this task changes no fit (ruling F16). `fit_stars_at_equals_fit_stars_with_beta` keeps passing with
  `&FitStarsOptions::default()`.

- [ ] **Step 3: Lab cycle** `t06a-seeds` with `--seeds matched` at a PROVISIONAL `K = 4.0` (the sweep follows; this run
  exists so the `seeds` rows and the fitted rows at the new seeds are recorded before step 4 chooses). Design list:
  `{"row":1,"path":"seeds","kind":"set-aside"}`, `{"row":1,"path":"measure","kind":"set-aside"}`, `{"row":2,"path":"seeds","kind":"set-aside"}`,
  `{"row":10,"metric":"seed flux rank","path":"seeds","kind":"set-aside"}`, `{"row":10,"metric":"LN selection keeps (peak_snr >= 40)","path":"ln self-pair","kind":"set-aside"}`
  (each on its row's own path, ruling F20 as folded),
  `{"row":3,"path":"measure","kind":"set-aside"}`, `{"row":3,"path":"betas@*","kind":"set-aside"}`, `{"row":3,"metric":"beta sensitivity","path":"betas","kind":"set-aside"}`,
  `{"row":5,"path":"measure","kind":"set-aside"}`, `{"row":5,"path":"betas@*","kind":"set-aside"}`,
  `{"row":7,"path":"measure","kind":"set-aside"}`, `{"row":7,"path":"betas@*","kind":"set-aside"}`, `{"row":11,"path":"measure","kind":"set-aside"}`,
  `{"row":11,"path":"betas@*","kind":"set-aside"}`,
  `{"row":4,"metric":"floor","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"floor","path":"betas@*","kind":"set-aside"}`,
  `{"row":6,"metric":"captured fraction","path":"measure","kind":"set-aside"}`, `{"row":6,"metric":"captured fraction","path":"betas@*","kind":"set-aside"}`
  (new seeds → a new fitted population on both arms; the fits themselves are unchanged), `{"row":1,"path":"register","kind":"reported"}`,
  `{"row":2,"path":"register","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (ruling F10).
  Judged within SE: row 4 `ecc`/`angle`/`axis order` on `shape/*` (`measure` and `betas@*`), row 2 `measure`, every
  `analysis` row.

- [ ] **Step 4: Choose the threshold (PA-2, ruling F37).** Build the probe once (it takes `--seed-threshold`); run the
  46 `twins/*` families WHOLE and `artefacts` at K ∈ {3.0, 3.5, 4.0, 4.5, 5.0, 6.0}:

```
for K in 3.0 3.5 4.0 4.5 5.0 6.0; do
  STAR_PROBE_ARGS="--diagnostics --seeds matched --seed-threshold $K" python3 bin/run_probe.py manifests/v2.json \
    --families "$(python3 -c 'import json; d=json.load(open("manifests/v2.json"))["families"]; print(",".join([f for f in d if f.startswith("twins/")] + ["artefacts", "background/flat", "background/grad10", "background/vig20", "background/neb0.1", "background/neb0.5", "background/neb3", "background/neb3_4096"]))')" \
    --out ~/.athenaeum-acc/star-lab-a/t06a-k/K$K/probe
  python3 bin/scorecard.py manifests/v2.json --rows 1 --probe-dir ~/.athenaeum-acc/star-lab-a/t06a-k/K$K/probe --out ~/.athenaeum-acc/star-lab-a/t06a-k/K$K/row1.md --json ~/.athenaeum-acc/star-lab-a/t06a-k/K$K/row1.json
done
python3 bin/threshold_table.py ~/.athenaeum-acc/star-lab-a/t06a-k --out results/plan-a/t06a-seeds/threshold-choice.txt   # (new, Task 1b's style: one line per family × K, the `nothing` rate on the seeds path)
```

  `DEFAULT_THRESHOLD` = the smallest K with every `twins/*` family and `artefacts` ≤ 0.5 `nothing` per Mpx on the
  `seeds` path (the row aggregates per family × path × plane). The `background/*` families are a REPORTED column
  of the same table (M-9). Commit the table, set the constant, rebuild the probe, re-run the unit tests
  (`cargo test -p athenaeum-core --lib -- star_measure --test-threads=4` — ruling F38: the tests above read
  `DEFAULT_THRESHOLD`), and re-run step 3's cycle at the chosen K (the committed `t06a-seeds` summary is the one at
  `DEFAULT_THRESHOLD`). The sweep's probe directories are deleted after the table is committed (PA-3's cap).

- [ ] **Step 5: Record R-OSP-2** (the table, the value, the data it was chosen on) in `docs/stacking/README.md`. Commit.

```
feat(star_measure): F-1 seeds - a two-pass matched filter at the field width (threshold <k>, ruling R-OSP-2), the pixel mask, the half-maximum field width of given seeds (PA-17); no fit change
```

**Acceptance:** the six tests green at the chosen K; the threshold table committed with the chosen K and its rule;
`noreg.txt` and `noreg-cumulative.txt` 0 moved outside the design list; control 0. No fitted bar is read here
(Task 6b).

---

### Task 6b: F-1 with F-7, the fit — the region, the rim background, the per-star width start, the donut and hot-pixel refusals (spec §3 F-1, F-7)

**Files:**
- Modify: `star_measure/fit.rs` (`fit_one`: width start, region rule PA-12, rim `b0`, the `Donut` and
  `NarrowerThanPixel` refusals; `initial_sigma` and `stamp_radius` retired), `star_measure/beta.rs`
  (`auto_beta`'s `sigma0` argument goes with them — the sample fits take the per-star width start),
  `star_measure/diagnostics.rs` (the two reasons were declared in Task 4), `examples/star_probe.rs` (nothing new),
  `docs/stacking/README.md` (R-OSP-4 the region rule)
- Earlier tests this task edits (ruling F14): Task 4's `duplicate_and_rim_refusals_are_counted_by_reason` (its
  `RegionTooSmall` fixture becomes PA-18's border geometry — the field-level nominal stamp it relied on is gone).

**Interfaces:**
- Produces: `fit_one` (PA-12): `F_start` per star from background-subtracted second moments in a box of radius
  `2·F_field` (`F_field` = `FieldWidth.fwhm_px`, from the matched filter or `field_width_of`; the box's own rim median
  is the subtraction level; clamped [0.6, 40] px), the region grows from `r_start = max(3, ceil(1.5·F_start))` to
  `r_max = clamp(ceil(6·F_start), 6, 96)` while `(m_r − s_r) > 0.01·(max − s_r)`; `b0 = s_r` (the final rim median);
  `a0 = max − b0`; the start σ from `F_start` with the moment axis ratio as today. Refusals: `Donut` when the 3×3 core
  mean about the moment centroid is below `0.8 ×` the mean of the 1-px ring at radius `max(1.5, F_start/2.3548)`
  (both background-subtracted; the 1.5-px floor is the fold check's ruling: at the unfloored radius a real star of
  FWHM ≤ 2.0 read 0.34–0.61 on the lab's renderer and would be refused — with the floor every real profile of FWHM
  1.2–16 reads ≥ 1.08 and the two donut families 0.02 / 0.29, computed; cost if wrong: a donut at an unusual R slips
  through, which the `donut outcome` row shows); `NarrowerThanPixel` when the accepted `min(fwhm_x, fwhm_y)
  < 0.8` px. Admission (`OutsideImage`) tests the width-start box (radius `2·F_field`), no longer a field-level
  nominal stamp.

- [ ] **Step 1: Write the failing tests** (`fit.rs` tests; the fixtures use `FitStarsOptions { field_width: Some(f),
  ..Default::default() }` so the geometry is the one PA-18 computes)

```rust
use crate::test_support::{moffat_field, MoffatStar, nan_border_field};

fn one(x: f64, y: f64, amp: f64, alpha: f64) -> MoffatStar { MoffatStar { x, y, amp, alpha_x: alpha, alpha_y: alpha, theta: 0.0 } }
fn seed(s: &MoffatStar) -> Seed { Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 } }
fn fit_with(data: &[f32], w: usize, h: usize, seeds: &[Seed], field: f64) -> (FitOutcome, Vec<FitDiagnostics>) {
    let opts = FitStarsOptions { field_width: Some(field), diagnostics: true, ..Default::default() };
    let out = fit_stars_at(data, w, h, seeds, 4.0, &opts, None);
    let d = out.diagnostics.clone();
    (out, d)
}

#[test]
fn broad_stars_fit_inside_a_region_that_scales_with_their_own_width() {
    // FWHM 14 at β 4: α = 14 / (2√(2^{1/4} − 1)) = 16.09; half-FWTM = 14.2 px — the D1 cause-1 regime
    let stars: Vec<MoffatStar> = (0..20).map(|i| one(120.0 + (i % 5) as f64 * 180.0, 120.0 + (i / 5) as f64 * 180.0, 0.3, 16.09)).collect();
    let data = moffat_field(1024, 840, &stars, 4.0, 0.05);
    let seeds: Vec<Seed> = stars.iter().map(seed).collect();
    let (out, diag) = fit_with(&data, 1024, 840, &seeds, 14.0);
    assert_eq!(out.fits.len(), 20, "{:?}", out.refused);
    for (f, d) in out.fits.iter().zip(&diag) {
        assert!((f.fwhm() / 14.0 - 1.0).abs() < 0.01, "{}", f.fwhm());
        assert!(d.region_radius as f64 >= 0.5 * f.fwtm_x.max(f.fwtm_y), "{d:?}");
        assert!(d.region_radius as f64 >= 1.5 * 14.0 && d.region_radius <= 96);
    }
}

#[test]
fn donut_is_refused_with_its_reason() {
    // an annulus (outer 6, obstruction 2.4) blurred by a Gaussian of σ 1.1 (FWHM 2.6), rendered by numeric convolution
    let (w, h) = (80usize, 80usize);
    let mut data = vec![0.05f32; w * h];
    for y in 0..h { for x in 0..w {
        let mut acc = 0.0f64;
        for (ux, uy) in ring_samples(40.0, 40.0, 2.4, 6.0) {   // 2000 samples of the annulus, uniform
            let d2 = (x as f64 - ux).powi(2) + (y as f64 - uy).powi(2);
            acc += (-d2 / (2.0 * 1.1 * 1.1)).exp();
        }
        data[y * w + x] += (0.3 * acc / 2000.0) as f32;
    } }
    let (out, diag) = fit_with(&data, w, h, &[Seed { x: 40.0, y: 40.0, peak: 0.1, flux: 1.0 }], 3.0);
    assert!(out.fits.is_empty(), "a donut must never fit: {:?}", out.fits);
    assert_eq!(diag[0].outcome, Outcome::Refused(RefusalReason::Donut));
}

/// A pin on the ring radius's 1.5-px floor (ruling F28: before this task no star is refused `Donut` at all, so it
/// cannot fail by behaviour; it fails on a Donut rule without the floor): a sharp real star is never a donut.
/// Computed on point-sampled Moffat-4 and Gaussian stars over 100 sub-pixel phases: core/ring ≥ 2.32 at FWHM 1.2,
/// ≥ 1.61 at 1.6, ≥ 1.37 at 2.0 (the unfloored radius read 0.29–0.59 there).
#[test]
fn sharp_stars_are_never_refused_as_donuts() {
    for (k, fwhm) in [1.2f64, 1.6, 2.0].into_iter().enumerate() {
        let alpha = fwhm / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        let stars: Vec<MoffatStar> = (0..25).map(|i| one(20.0 + (i % 5) as f64 * 30.1 - 0.2, 20.0 + (i / 5) as f64 * 30.1 - 0.2, 0.3, alpha)).collect();
        let mut data = moffat_field(160, 160, &stars, 4.0, 0.05);
        crate::test_support::add_noise(&mut data, 0.002, k as u64);
        let seeds: Vec<Seed> = stars.iter().map(seed).collect();
        let (_, diag) = fit_with(&data, 160, 160, &seeds, fwhm);
        assert_eq!(diag.len(), 25);
        assert!(diag.iter().all(|d| d.outcome != Outcome::Refused(RefusalReason::Donut)), "FWHM {fwhm}: {diag:?}");
    }
}

#[test]
fn a_given_hot_pixel_is_refused_narrower_than_pixel() {
    let (mut data, w, h) = crate::test_support::fixture_field(3, 0.0, 0.002);
    data[100 * w + 100] = 0.9;
    let (out, diag) = fit_with(&data, w, h, &[Seed { x: 100.0, y: 100.0, peak: 0.85, flux: 0.85 }], 3.0);
    assert!(out.fits.is_empty());
    assert_eq!(diag[0].outcome, Outcome::Refused(RefusalReason::NarrowerThanPixel));
}

#[test]
fn rim_background_start_is_the_sky_under_nebulosity() {
    // sky 0.05 + a quadratic ramp of amplitude 0.02 over the 80-px stamp; σ_px = 0.002; the F-7 condition at one star
    let (w, h) = (80usize, 80usize);
    let mut data = moffat_field(w, h, &[one(40.3, 39.7, 0.3, 3.0)], 4.0, 0.0);
    for y in 0..h { for x in 0..w { let r2 = ((x as f64 - 10.0).powi(2) + (y as f64 - 70.0).powi(2)) / 6400.0; data[y * w + x] += 0.05 + (0.02 * r2) as f32; } }
    crate::test_support::add_noise(&mut data, 0.002, 4);
    let local_sky = 0.05 + 0.02 * ((40.3f64 - 10.0).powi(2) + (39.7f64 - 70.0).powi(2)) / 6400.0;
    let (out, diag) = fit_with(&data, w, h, &[Seed { x: 40.3, y: 39.7, peak: 0.3, flux: 1.0 }], 3.0);
    assert_eq!(out.fits.len(), 1);
    assert!((diag[0].background_start - local_sky).abs() < 0.3 * 0.002, "b0 {} vs sky {local_sky}", diag[0].background_start);
    assert!((out.fits[0].background - local_sky).abs() < 0.3 * 0.002);
}

#[test]
fn border_limited_region_refuses_a_broad_star_region_too_small() {
    // PA-18: a FWHM-12 star (α 13.8) in a FWHM-3 field, 10 px from the border: the width-start box (radius 6) fits,
    // the region is clipped at r = 10, and 0.5·FWTM = 12.2 > 10 → RegionTooSmall
    let data = moffat_field(200, 200, &[one(10.0, 100.0, 0.3, 13.8)], 4.0, 0.05);
    let (out, diag) = fit_with(&data, 200, 200, &[Seed { x: 10.0, y: 100.0, peak: 0.3, flux: 1.0 }], 3.0);
    assert!(out.fits.is_empty());
    assert_eq!(diag[0].outcome, Outcome::Refused(RefusalReason::RegionTooSmall));
    assert_eq!(diag[0].region_radius, 10);
}

#[test]
fn a_star_whose_width_start_box_leaves_the_image_is_outside_image() {
    let data = moffat_field(200, 200, &[one(4.0, 100.0, 0.3, 3.0)], 4.0, 0.05);
    let (out, diag) = fit_with(&data, 200, 200, &[Seed { x: 4.0, y: 100.0, peak: 0.3, flux: 1.0 }], 3.0);
    assert!(out.fits.is_empty());
    assert_eq!(diag[0].outcome, Outcome::Refused(RefusalReason::OutsideImage));
}

#[test]
fn nan_border_region_growth_skips_non_finite_pixels() {
    let data = nan_border_field(240, 240, 6, true);
    let m = measure_plane(&data, 240, 240, &MeasureOptions { diagnostics: true, seeds: SeedSpec::MatchedFilter { threshold: seeds::DEFAULT_THRESHOLD }, ..Default::default() }, None);
    assert_eq!(m.stars.len(), 40);
    assert!(m.stars.iter().all(|f| f.background.is_finite() && f.fwhm().is_finite()));
}
```

  `ring_samples(cx, cy, r_in, r_out) -> Vec<(f64, f64)>` is a deterministic `#[cfg(test)]` helper (a 2000-point
  sunflower lattice over the annulus). `FitStarsOptions.diagnostics: bool` and `FitOutcome.diagnostics:
  Vec<FitDiagnostics>` join the refit path here so these tests read the diagnostics without the adapter.

- [ ] **Step 2: Run, fail, implement, run.** The `measure.rs` and `psf_signal.rs` literals move by design (region,
  background, width start) → re-pinned with their comment; `fixed_beta_fit_recovers_centre_width_background_and_aperture_signal`
  must still pass at its tolerances; Task 4's rim fixture is rewritten to PA-18's geometry.

- [ ] **Step 3: Lab cycle** `t06b-f1` (v2 AND `manifests/inject.json` → `~/.athenaeum-acc/star-lab-a/t06b-f1/inject-probe`;
  the probe's default seeds are `matched` at `DEFAULT_THRESHOLD`). Design list: `{"row":1,"metric":"fit outcomes","path":"measure","kind":"set-aside"}`,
  `{"row":1,"metric":"completeness","path":"measure","kind":"set-aside"}`, `{"row":1,"metric":"donut outcome","path":"measure","kind":"set-aside"}`,
  `{"row":3,"path":"measure","kind":"set-aside"}`, `{"row":3,"path":"betas@*","kind":"set-aside"}`, `{"row":3,"metric":"beta sensitivity","path":"betas","kind":"set-aside"}`,
  `{"row":5,"path":"measure","kind":"set-aside"}`, `{"row":5,"path":"betas@*","kind":"set-aside"}`,
  `{"row":7,"path":"measure","kind":"set-aside"}`, `{"row":7,"path":"betas@*","kind":"set-aside"}`, `{"row":10,"path":"measure","kind":"set-aside"}`,
  `{"row":11,"path":"measure","kind":"set-aside"}`, `{"row":11,"path":"betas@*","kind":"set-aside"}`,
  `{"row":4,"metric":"floor","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"floor","path":"betas@*","kind":"set-aside"}`,
  `{"row":6,"metric":"captured fraction","path":"measure","kind":"set-aside"}`, `{"row":6,"metric":"captured fraction","path":"betas@*","kind":"set-aside"}`
  (the region and background move the signal on both arms; F20 as folded), `{"row":1,"path":"register","kind":"reported"}`,
  `{"row":2,"path":"register","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (F10),
  `{"row":2,"path":"measure","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (the start
  moved; centroids within their CRLB). Judged within SE: row 4 `ecc`/`angle`/`axis order` on `shape/*` (`measure`
  and `betas@*`; F-2's gains must hold), row 1 `false detections` and `bright coverage` (the seeds are Task 6a's),
  every `analysis` row.

- [ ] **Step 4: Read the bars** (`results/plan-a/t06b-f1/bars.txt`, cells copied verbatim; PA-20):
  - fitted / seeded at FWHM 8: `sharp/m4/f8.0` — the `measure` completeness cell ÷ the `seeds` completeness cell
    (both `… % of N expected` on the same denominator) ≥ 99 % (baseline 82.7 / 92.0 = 89.9 %, derived);
  - frame FWHM error vs `pointfit` ≤ 1 % (row 7 `frame FWHM` ÷ the `pointfit: med …` truth − 1) on
    `sharp/m4/f{2.0,2.6,3.5,5.0,8.0,12.0,16.0}` AND `sharp/gauss/f{2.0,2.6,3.5,5.0,8.0,12.0,16.0}` — the `gauss`
    cells are expected to miss here by construction (β discrete: −3.04 % at β 10, baseline `sharp/gauss/f8.0`
    −2.58 %); a miss goes to the owner with the cells and the F-4 mechanism, and Task 9 reads the same rows again;
  - donut: row 1 `donut outcome` accepted = 0 % on `donut/r6` and `donut/r10`, refusals named;
  - F-7: row 5 `background` |bias| ≤ 0.3 σ on `background/neb0.1`, `background/neb0.5`, `background/neb3`,
    `background/neb3_4096` (baseline −0.064 / +0.251 / +0.998 σ on neb0.5 / neb3 / neb3_4096; neb0.1 as printed) and
    within ±0.1 σ on `background/flat` (baseline −0.076 σ); `grad10` / `vig20` reported (baseline −0.081 / −0.084 σ);
  - raw mode (injections): row 1 completeness of the injected stars above the threshold ≥ 99 % on the 7 raw
    entries and 3 mosaics (`inject/raw/*`, `inject/mosaic/*`; baseline `raw/corpus` 96.9–97.4 %, the Pro raw
    96.3–98.4 %); the injected FWHM bias recorded (judged at Task 9 with F-4);
  - `_DSC5767.fit` is the corpus's; its seed count is read at Task 12 (the corpus run);
  - cost: the per-family row 9 cells are recorded (the m42 corpus batch and the stage time are Tasks 12 and 13).

- [ ] **Step 5: Record** R-OSP-4 (PA-12's rule, PA-18's geometry and the donut/hot-pixel refusals) in
  `docs/stacking/README.md`. Commit.

```
fix(star_measure): F-1/F-7 per-star width start, a region that scales with the star, the rim background start, donut and hot-pixel refusals (<bar cells>)
```

**Acceptance:** every bar in step 4 met (or, for the `gauss` cells, reported to the owner with the mechanism), cells
verbatim in `bars.txt`; both diffs 0 moved outside the design list; control 0. A miss on any other row → the owner
with the cells; Task 7 waits. (Every `<cells>` in a commit message of this plan is filled with the measured numbers
from that task's `bars.txt`; a commit never carries the angle brackets.)

---

### Task 7: The stacking Measure config — `seed_source`, the legacy decode, presets, the panel, R-M4c-11 (spec §2.2)

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (`MeasurementConfig` gains `seed_source: SeedSource`,
  `seed_threshold: f64`; a shadow-struct `Deserialize`; `clamp_seed_threshold`), `crates/athenaeum-core/src/stacking/measure.rs`
  (hand edits: `MeasureOptions.seed_source`, the probe-override enum `SeedSource` → `SeedOverride` (ruling F26; its
  `MatchedFilter` variant arrived in Task 6a), the adapter's dispatch; `DEFAULT_SEED_DETECTOR` retired),
  `crates/athenaeum-core/src/stacking/plan.rs` (hand edit: the golden-literal test's `meas`/`reg`/`ln_*` columns
  move again — ruling F35), `crates/athenaeum-core/examples/weight_audit.rs` and `examples/integrate_probe.rs`
  (the rename reaches them too, M-3: `grep -rln 'SeedSource' crates/athenaeum-core` before the build so
  `--all-targets` is green), `crates/athenaeum-core/src/star_measure/mod.rs` tests (Task 2's and Task 4's
  `SeedSource::Fast` → `SeedOverride::Fast`, ruling F14), `crates/athenaeum-core/src/ts_export.rs` (`SeedSource`),
  `src/types/stacking.ts` (regenerated), `src/components/stacking/panels/MeasurePanel.tsx`,
  `src/components/stacking/stageSummary.ts` (`seedSourceLabel`), `src/settings/registry.ts` (no change: the
  stacking tab lists sections, not fields), `crates/athenaeum-core/src/api/stacking.rs` (nothing: presets and
  defaults flow through `StackingConfig`), `docs/stacking/README.md` (R-M4c-11 updated), `examples/star_probe.rs`
  (`--seeds` reads the config's `seedSource` when `--config` is given; the frame line keeps printing the effective
  source — ruling F3 — so no row moves)

**Interfaces:**
- Produces:

```rust
// config.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum SeedSource { #[default] MatchedFilter, Peak, Structure }

pub struct MeasurementConfig {
    …,
    /// The seed source (spec §2.2): the module's matched filter (default), the peak threshold (reads
    /// `detectionSigma` and `seedPrefilter`), or the structure map. Decoded from older documents by `legacy_seed_source`.
    pub seed_source: SeedSource,
    /// The matched filter's threshold in units of the filtered plane's noise (ruling R-OSP-2). Clamped to [2, 20].
    #[serde(default = "default_seed_threshold")]
    pub seed_threshold: f64,
    pub detection_sigma: f64,          // Peak only
    pub seed_prefilter: SeedPrefilter, // Peak only
    pub seed_detector: SeedDetector,   // kept on the wire for round-trips of old documents; `Structure` ⇔ seed_source Structure
}

/// One decode for every stored document (set, global, user preset — spec §2.2, R-2): a document without
/// `seedSource` maps Structure → Structure; Peak (or absent) with `seedPrefilter = median3` or a `detectionSigma`
/// other than 20 → Peak; otherwise → MatchedFilter. A stored value is never reinterpreted in other units.
fn legacy_seed_source(detector: Option<SeedDetector>, prefilter: Option<SeedPrefilter>, sigma: Option<f64>) -> SeedSource;
```

  `MeasurementConfig::measure_options` fills `MeasureOptions.seed_source` and `seed_threshold`. The measurement
  stage hash follows automatically (`cfg.measurement` is serialized whole). `preset()`: unchanged bodies (the
  presets never touched measurement). TS: `SeedSource = "matchedFilter" | "peak" | "structure"`; the panel's
  "Seed source" select replaces "Seed detector" (which stays as a hidden field on the wire); "Seed threshold (σ_f)"
  shown for `matchedFilter`, "Detection threshold (σ)" + "Seed pre-filter" for `peak`; help texts quote R-OSP-2 and
  R-M4c-11. `stageSummary`: `… · seeds: matched filter (σ_f 4.0)`.

- [ ] **Step 1: Write the failing tests** (`config.rs` tests)

```rust
use crate::api::stacking::NamedPreset;      // ruling F35

#[test]
fn every_older_measurement_shape_decodes_to_the_spec_seed_source() {
    let cases = [
        (r#"{"measurement":{}}"#, SeedSource::MatchedFilter),                                             // pre-M4a
        (r#"{"measurement":{"detectionSigma":20.0,"seedPrefilter":"none"}}"#, SeedSource::MatchedFilter),  // the old default
        (r#"{"measurement":{"detectionSigma":16.0}}"#, SeedSource::Peak),                                // an explicit σ
        (r#"{"measurement":{"seedPrefilter":"median3"}}"#, SeedSource::Peak),                            // the prefilter
        (r#"{"measurement":{"seedDetector":"structure","seedPrefilter":"median3"}}"#, SeedSource::Structure), // Median3 is Peak's only
        (r#"{"measurement":{"seedSource":"peak","detectionSigma":20.0}}"#, SeedSource::Peak),             // a new document says so
    ];
    for (json, want) in cases {
        let c = resolve_config(Some(json), None).unwrap();
        assert_eq!(c.measurement.seed_source, want, "{json}");
        assert_eq!(c.measurement.detection_sigma, if json.contains("16.0") { 16.0 } else { 20.0 }, "never reinterpreted: {json}");
    }
    // the global document and a user preset go through the same decode
    assert_eq!(resolve_config(None, Some(r#"{"measurement":{"detectionSigma":20.0}}"#)).unwrap().measurement.seed_source, SeedSource::MatchedFilter);
    let p: NamedPreset = serde_json::from_str(r#"{"name":"x","config":{"measurement":{"seedDetector":"structure"}}}"#).unwrap();
    assert_eq!(p.config.measurement.seed_source, SeedSource::Structure);
}

#[test]
fn seed_source_moves_the_measurement_stage_hash_and_the_default_is_the_matched_filter() {
    let cfg = StackingConfig::default();
    assert_eq!(cfg.measurement.seed_source, SeedSource::MatchedFilter);
    assert_eq!(cfg.measurement.seed_threshold, crate::star_measure::seeds::DEFAULT_THRESHOLD);
    let mut peak = cfg.clone();
    peak.measurement.seed_source = SeedSource::Peak;
    assert_ne!(stage_hash(&measurement_subtree(&cfg), &[], &[]), stage_hash(&measurement_subtree(&peak), &[], &[]));
    let mut k = cfg.clone();
    k.measurement.seed_threshold += 0.5;
    assert_ne!(stage_hash(&measurement_subtree(&cfg), &[], &[]), stage_hash(&measurement_subtree(&k), &[], &[]));
    assert_eq!(cfg.measurement.measure_options(ScaleEstimator::Bwmv).seed_source, SeedSource::MatchedFilter);
}

#[test]
fn seed_threshold_is_clamped_with_a_warn() {
    let lo = resolve_config(Some(r#"{"measurement":{"seedThreshold":0.5}}"#), None).unwrap();
    assert_eq!(lo.measurement.seed_threshold, 2.0);
    let hi = resolve_config(Some(r#"{"measurement":{"seedThreshold":99.0}}"#), None).unwrap();
    assert_eq!(hi.measurement.seed_threshold, 20.0);
    let ok = resolve_config(Some(r#"{"measurement":{"seedThreshold":4.5}}"#), None).unwrap();
    assert_eq!(ok.measurement.seed_threshold, 4.5);
}
```

  Frontend: `src/components/stacking/panels/MeasurePanel.test.tsx` (vitest): the select renders the three
  choices, the peak fields are disabled unless `peak`, the threshold field is disabled unless `matchedFilter`.

- [ ] **Step 2: Run, fail, implement, run.** Update `config_hash_is_canonical_and_pinned` (new fields in the
  canonical JSON → the literal moves; say why) and `stage_hashes_are_pinned_to_golden_literals` (`plan.rs`, hand
  edit). Regenerate TS: `CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core
  --test ts_contract`; `npx tsc --noEmit`; `npx vitest run src/components/stacking`.

- [ ] **Step 3: The adapter default flips to the matched filter** — the measure.rs pins
  (`the_shipped_none_path_measures_exactly_these_numbers`, `measure_plane_with_seeds_pinned_before_task_3b_rewrite`,
  `detection_sigma_steers_the_seed_population`, `the_median_prefilter_keeps_a_well_sampled_population`,
  `the_structure_detector_seeds_a_measurement`) are re-pinned on the matched-filter default with their old
  literals kept in a comment, and the Peak/Structure tests set `seed_source` explicitly.

- [ ] **Step 4: Lab cycle** `t07-config`: the probe now takes its seeds from the config's default — identical to
  Task 6b's run, and the frame line's effective `seedSource`/`seedThreshold` are unchanged (ruling F3). Gate:
  `noreg.txt` against t06b: 0 moved rows, max |Δ| = 0 (the config switch is not a numeric change); the design list
  is empty; `noreg-cumulative.txt` unchanged from t06b's.

- [ ] **Step 5: README.** R-M4c-11 gains: "superseded for the default by R-OSP-2 (2026-10): `seedSource =
  matchedFilter` is the default; `peak` and `structure` remain explicit choices with their own dials; a stored
  document decodes per spec §2.2". Commit.

```
feat(stacking): measurement.seedSource (matchedFilter default, peak, structure) with one legacy decode for every stored document, seedThreshold, the Measure panel and presets; R-M4c-11 updated
```

**Acceptance:** the six-fixture decode test; tsc, vitest and `ts_contract` green; `noreg.txt` vs t06b 0 moved, max
|Δ| 0; both hosts' `get_stacking_presets`/`get_stacking_defaults` answer the new shape through the unchanged
handlers (`cargo test -p athenaeum-web` and `-p athenaeum-tauri --lib` green).

---

### Task 8: F-3 — the per-seed saturation verdict and the unsaturated statistics (spec §2.1, §3 F-3)

**Rewritten 2026-10-09.** PA-8's flat-top wing test failed on lab truth (research R1); the owner adopted an
established stacking program's method as is (spec §2.1 as revised, PA-23). C-a (owner 2026-10-09) supersedes this
task's former interface (`region_verdict`, `SaturationVerdict { clip_hit, core_pixels, overshoot_sigma, wing_fit }`,
`MeasureOptions.saturation_fraction`, `PlaneMeasurement.wing_fit_missing`, the probe's `--saturation-fraction` and
`wingFitMissing`) and its tests. The text below replaces them; the superseded text is at 51021d06.

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/photometry.rs` (`annulus_sigma`, which Task 11's SNR reads and the
  saturation test no longer does; `SaturationPlane`, `plateau_level`, `SaturationVerdict`, `saturation_verdict`)
- Modify: `star_measure/diagnostics.rs` (`RefusalReason::MostlySaturated`; `FitDiagnostics.verdict`),
  `star_measure/fit.rs` (a verdict pass runs `fit_one`'s steps 1–3 — admission, width start, `grow_region` — per seed on
  the unnormalised region in stamp coordinates, runs `saturation_verdict`, and keeps one `Option<SaturationVerdict>`
  per seed, independent of `opts.diagnostics`; the donut test precedes it, so a `Donut` seed gets no verdict fit (the
  reference has no donut test, so this is no deviation from it); `auto_beta` takes the unflagged seeds; the fits at β
  regather the region, counted once in the Measure wall time Phase A reports (F9's order); `fit_one` copies the
  verdict's flag into `StarFit.flags` and applies the star's verdict level as its exclusion; `FitScratch` becomes
  `pub(crate)`),
  `star_measure/beta.rs` (the discrete search samples the 64 brightest seeds whose verdict is unflagged),
  `star_measure/frame.rs` (`frame_shape` over the unflagged subset; `stars_unsaturated`), `star_measure/mod.rs`
  (`MeasureOptions.colour`; one `SaturationPlane` per plane; `measure_plane` computes `sky`
  before the fits, since the verdict reads `sky.median`, while `fit_stars_at` computes none and runs no verdict (Q7);
  the order seeds → field width → verdicts → β sample → fits at β), `stacking/measure.rs` (hand edit: `ChannelMeasurement.stars_unsaturated`; `colour` for a debayered
  OSC group's planes), `examples/star_probe.rs` (each seed's verdict on its fit or refused line under
  `--diagnostics`; `colour` for a 3-plane input), `docs/stacking/README.md` (R-OSP-5)
- Not modified: `star_measure/seeds.rs` — the field width runs no saturation test (PA-17, ruling F25 as folded), so
  `detect` and `field_width_of` keep Task 6a's signatures (Plan C's `detect` included).
- Earlier tests this task edits (ruling F14): none break — `FrameAggregates.fwhm_px` was `Option<f64>` from Task 2.

**Interfaces:**
- Produces:

```rust
// photometry.rs
/// The star's own sky annulus (PA-11 radii: inner max(3·FWHM, 6), width max(FWHM, 3)): σ = 1.4826·MAD of the
/// finite, unmasked pixels present, and their count. Non-finite, masked and off-image pixels are absent (Review
/// Focus 1 and 5). `None` under 8 present pixels. Owned here (I-2) and reused by `star_snr` (Task 11).
pub fn annulus_sigma(plane: &[f32], w: usize, h: usize, x: f64, y: f64, fwhm: f64, mask: Option<&[bool]>)
    -> Option<(f64 /* sigma_sky */, usize /* n_sky */)>;

/// The plane-level inputs of the saturation test (spec §2.1, PA-23), once per plane, in working units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaturationPlane { pub bg: f64, pub threshold: f64, pub norm: f64, pub range: f64, pub colour: bool }
impl SaturationPlane {
    /// `threshold = bg + 5·sigma`, `range = min(plane_max, norm) − bg`; `plane_max` over the plane's finite,
    /// unmasked pixels.
    pub fn new(bg: f64, sigma: f64, plane_max: f64, norm: f64, colour: bool) -> Self;
}

/// The plateau test at the seed pixel: `region` is the unnormalised region in stamp coordinates (`x − cx`, `y − cy`,
/// plane values, as `fit_one` gathers it), so pixel0 is (0, 0) and `level` compares plane values; pixel0 = the
/// region's pixel at the rounded seed centre, its 8 neighbours from the region (an absent one is not above the threshold); the neighbours ≥ `threshold` must number ≥ 3 (mono)
/// or 8 (colour), with `meanhigh − bg ≥ 0.7·range` and `pixel0 − minhigh ≤ 0.1·range`. `Some(level)` with
/// `level = min(pixel0, norm) − 0.1·range` when it passes, else `None` (the level is then `norm`).
pub(crate) fn plateau_level(region: &[PixelSample], seed_x: f64, seed_y: f64, plane: &SaturationPlane) -> Option<f64>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaturationVerdict {
    pub plateau: bool,   // the plateau test passed
    pub level: f64,      // its level, else norm
    pub excluded: u32,   // the region's pixels ≥ level, left out of the verdict fit
    pub amplitude: f64,  // the verdict fit's A
    pub beta: f64,       // its free β (the β sample's fit, C-b)
    pub fwhm: f64,       // its FWHM (the payoff reads it, C-d)
    pub saturated: bool, // amplitude > range
    pub fit: bool,       // false = the verdict fit failed (not n ≤ p): amplitude, beta, fwhm NaN, saturated false
}

/// Spec §2.1 per seed (PA-23): the plateau test, the level, then the free-β verdict fit (`fit_moffat_2d` at this
/// task, `fit_moffat_2d_bounded` with `BETA_BOUNDS` from Task 9) on the region's pixels below the level, started at
/// `b0`, the seed and `f_start` (PA-12), with `3 × params.max_iter` when any pixel is excluded.
/// `Err(MostlySaturated)` when ≤ 8 pixels remain (the fit's 8 parameters).
pub fn saturation_verdict(region: &[PixelSample], seed_x: f64, seed_y: f64, b0: f64, f_start: f64,
                          plane: &SaturationPlane, params: &FitParams, scratch: &mut FitScratch)
    -> Result<SaturationVerdict, RefusalReason>;
```

- The plane's inputs: bg = `sky.median` scaled to working units (the sampled, clipped sky median, PA-21) and σ =
  `noise.sigma` (working units; 0 under `NoiseSource::None`, one `warn!` per plane when seeds are given); norm =
  `ADU_SCALE` for `Domain::Calibrated`, the stored integer type's full scale (the new `Domain::RawAdu` field) for raw
  ADU (Q2); `colour = MeasureOptions.colour` (default
  `false`; the stacking adapter sets it for a debayered OSC group's planes, the probe for a 3-plane input).
- Every seed whose region is gathered gets a verdict (C-b). A seed refused before its region (`OutsideImage`,
  `TooFewPixels`) or as a `Donut` has none; `Err(MostlySaturated)` refuses the seed, counted in `stars_refused` by reason.
  `FitDiagnostics.verdict: Option<SaturationVerdict>` records it.
- `fit_one` sets `StarFit.flags |= SATURATED` from the verdict (own or cross-plane); a FLAGGED star is fitted at the
  frame β with its verdict fit's exclusion (pixels ≥ its own level, norm without a plateau; ×3 iterations); an
  unflagged star excludes nothing and is byte-identical (Q6 as revised 2026-10-09) and stays in `.athf` with its flag (C-c, Q6). A verdict with `fit = false` is no
  flag and no refusal; that seed is out of the β sample and the payoff, and the probe prints it (Q4). `stars_fitted` keeps counting every accepted fit (M-4);
  `FrameAggregates.stars_unsaturated`; `frame_shape(fits)` → `frame_shape(fits.iter().filter(|f|
  !f.flags.saturated()))` with `None` when empty (Review Focus 3 — the adapter then reports `fwhm_px = 0.0`, which
  passes `select_frames`'s `max_fwhm_px` as the spec allows; the plan gate's and the UI's treatment of
  `stars_unsaturated = 0` is Plan C's, M-12); the β sample (discrete here) is the 64 brightest unflagged seeds with a successful verdict fit. The
  field width (PA-17) reads every candidate and no verdict, because the verdict's region needs it (ruling F25 as
  folded; cost if wrong: a field width biased on frames dominated by saturated stars).

- [ ] **Step 1: Write the failing tests** (`photometry.rs` and `mod.rs` tests). They pin the method on the module's
  own seed region (C-f; R2's fixtures, not R1's ± 6F box).

```rust
use crate::test_support::{moffat_field, MoffatStar, add_noise};

fn clipped(data: &mut [f32], at: f32) { for v in data.iter_mut() { if *v > at { *v = at; } } }
/// `data` measured at the given seeds through the module's own regions, diagnostics on, Moffat-4 (no β sample).
fn measured(data: &[f32], w: usize, h: usize, seeds: Vec<Seed>) -> PlaneMeasurement {
    measure_plane(data, w, h, &MeasureOptions { seeds: SeedSpec::Given(seeds), psf_model: PsfModel::Moffat4,
        diagnostics: true, ..Default::default() }, None)
}
fn verdict(m: &PlaneMeasurement, seed: usize) -> SaturationVerdict {
    m.diagnostics.iter().find(|d| d.seed == seed).and_then(|d| d.verdict).expect("a seed with a region has a verdict")
}
fn seed_at(x: f64, y: f64) -> Seed { Seed { x, y, peak: 0.9, flux: 1.0 } }
/// R2's single-star fixtures: FWHM 2.6 at β 4 (α 2.99), off the pixel grid.
fn small_core_star(x: f64, y: f64, amp: f64) -> MoffatStar { MoffatStar { x, y, amp, alpha_x: 2.99, alpha_y: 2.99, theta: 0.0 } }
/// A hand-built region, stamp-centred as `fit_one` hands it (pixel0 at (0, 0)): the 3x3 `core` (row-major, centre at index 4, `None` = absent), then `ring_n`
/// of the 16 pixels of the r = 2 ring at `ring`.
fn hand_region(core: [Option<f32>; 9], ring: f32, ring_n: usize) -> Vec<PixelSample> {
    let mut px: Vec<PixelSample> = core.iter().enumerate().filter_map(|(i, v)| v.map(|v| PixelSample {
        x: -1.0 + (i % 3) as f64, y: -1.0 + (i / 3) as f64, value: v as f64 })).collect();
    let ring_xy = (-2i64..=2).flat_map(|dy| (-2i64..=2).map(move |dx| (dx, dy))).filter(|&(dx, dy)| dx.abs() == 2 || dy.abs() == 2);
    px.extend(ring_xy.take(ring_n).map(|(dx, dy)| PixelSample { x: dx as f64, y: dy as f64, value: ring as f64 }));
    px
}
fn twelve_clipped() -> (Vec<f32>, Vec<Seed>) {
    let stars: Vec<MoffatStar> = (0..12).map(|i| MoffatStar { x: 30.0 + (i % 4) as f64 * 50.0, y: 30.0 + (i / 4) as f64 * 50.0,
        amp: 3.0, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 }).collect();
    let mut data = moffat_field(220, 170, &stars, 4.0, 0.05); clipped(&mut data, 0.95); add_noise(&mut data, 0.002, 5);
    (data, stars.iter().map(|s| seed_at(s.x, s.y)).collect())
}

#[test]
fn annulus_sigma_is_the_mad_of_the_present_pixels() {
    // PA-11 at FWHM 2.6: inner max(7.8, 6) = 7.8, width max(2.6, 3) = 3 — the ring 7.8..10.8 (180 pixels about an
    // interior pixel centre, counted). A star 4 px from the border: its ring is cut by the border to 119 in-image
    // pixels (counted), and σ comes from those only; the NaN corner holds the whole in-image ring of a star at (60, 60)
    let (w, h) = (64usize, 64usize);
    let mut data = vec![0.05f32; w * h];
    add_noise(&mut data, 0.004, 3);
    for y in 0..h { for x in 0..w { if x > 40 && y > 40 { data[y * w + x] = f32::NAN; } } }
    let (sigma, n) = annulus_sigma(&data, w, h, 4.0, 32.0, 2.6, None).unwrap();
    assert!(n > 0 && n < 150, "{n}");
    assert!((sigma / 0.004 - 1.0).abs() < 0.35, "{sigma}");
    assert!(annulus_sigma(&data, w, h, 60.0, 60.0, 2.6, None).is_none(), "the NaN corner: nothing present");
    let all = vec![false; w * h];
    assert!(annulus_sigma(&data, w, h, 32.0, 32.0, 2.6, Some(&all)).is_none(), "a fully masked annulus");
}

#[test]
fn the_range_is_the_plane_max_capped_at_full_scale_less_the_median() {
    let p = SaturationPlane::new(0.05, 0.002, 1.24, 1.0, false);
    assert!((p.range - 0.95).abs() < 1e-12 && (p.threshold - 0.06).abs() < 1e-12, "{p:?}");
    let p = SaturationPlane::new(0.05, 0.002, 0.6, 1.0, true);
    assert!((p.range - 0.55).abs() < 1e-12 && p.colour, "{p:?}");
}

#[test]
fn the_plateau_test_needs_three_neighbours_on_mono_all_eight_on_colour_and_a_high_flat_top() {
    // range 0.9, threshold 0.06; `c` at the clip, `k` at the sky (under the threshold); the level is 0.95 − 0.09
    let (c, k) = (Some(0.95f32), Some(0.05f32));
    let mono = SaturationPlane::new(0.05, 0.002, 0.95, 1.0, false);
    let colour = SaturationPlane { colour: true, ..mono };
    let level = |core: [Option<f32>; 9], p: &SaturationPlane| plateau_level(&hand_region(core, 0.3, 16), 0.0, 0.0, p);
    assert!(level([c, c, c, k, c, k, k, k, k], &mono).is_some(), "mono: 3 of 8 above the threshold");
    assert!(level([c, c, k, k, c, k, k, k, k], &mono).is_none(), "mono: 2 of 8");
    let l = level([c; 9], &colour).expect("colour: 8 of 8");
    assert!((l - 0.86).abs() < 1e-6, "{l}");
    assert!(level([c, c, c, c, c, c, c, c, None], &colour).is_none(), "colour: 7 of 8, one absent");
    assert!(level([c, c, c, c, c, Some(0.84), c, c, c], &mono).is_none(), "pixel0 − minhigh 0.11 > 0.1·range");
    let (h, p0) = (Some(0.5f32), Some(0.55f32));
    assert!(level([h, h, h, h, p0, h, h, h, h], &mono).is_none(), "a flat top at half the range: meanhigh too low");
}

#[test]
fn no_more_unexcluded_pixels_than_parameters_refuses_the_seed_mostly_saturated() {
    // the 3x3 is a plateau at the clip, so the level excludes all nine and the ring's k pixels are left for the
    // 8-parameter fit: 8 → refused (n ≤ p, Q5), 9 → the fit runs
    let plane = SaturationPlane::new(0.05, 0.002, 0.95, 1.0, false);
    let mut s = FitScratch::default();
    let r = saturation_verdict(&hand_region([Some(0.95); 9], 0.3, 8), 0.0, 0.0, 0.05, 2.6, &plane, &FitParams::default(), &mut s);
    assert_eq!(r, Err(RefusalReason::MostlySaturated));
    let r = saturation_verdict(&hand_region([Some(0.95); 9], 0.3, 9), 0.0, 0.0, 0.05, 2.6, &plane, &FitParams::default(), &mut s);
    assert_ne!(r, Err(RefusalReason::MostlySaturated), "nine pixels left: the fit runs");
}

#[test]
fn a_clipped_star_without_a_full_3x3_plateau_is_fitted_uncensored_and_still_flagged() {
    // amplitude 3× the clip 0.95: about eight pixels reach the clip, but one neighbour of the centre pixel sits ≈ 0.11
    // below it (> 0.1·range), so the plateau test fails, the level is norm and nothing is excluded; the uncensored
    // fit overshoots (R2: A/range 1.30, FWHM +48 %) — spec §2.1's known limit
    let mut hot = moffat_field(60, 60, &[small_core_star(30.3, 30.7, 2.85)], 4.0, 0.05); clipped(&mut hot, 0.95); add_noise(&mut hot, 0.002, 1);
    let v = verdict(&measured(&hot, 60, 60, vec![seed_at(30.3, 30.7)]), 0);
    assert!(!v.plateau && v.excluded == 0 && v.saturated, "{v:?}");
}

#[test]
fn a_planes_brightest_unclipped_star_is_flagged_because_its_own_peak_sets_the_range() {
    // by design (spec §2.1): the lone star at 0.6× the clip sets the plane max, so the range is its own brightest
    // pixel − bg, and the fitted peak between pixels tops it (R2: A/range 1.08)
    let mut cool = moffat_field(60, 60, &[small_core_star(30.3, 30.7, 0.57)], 4.0, 0.05); add_noise(&mut cool, 0.002, 1);
    let v = verdict(&measured(&cool, 60, 60, vec![seed_at(30.3, 30.7)]), 0);
    assert!(!v.plateau && v.saturated, "{v:?}");
}

#[test]
fn a_lone_broad_unclipped_star_passes_the_plateau_test_and_its_censored_fit_keeps_its_width() {
    // FWHM 9.01 (α 24, β 20), peak 0.3 on 0.05: its 3x3 lies within 0.1·range of the centre, so its top is censored
    // (R2: FWHM within 0.2 %). The flag is not pinned: the lone star sets the range itself (R2: A/range 0.994)
    let mut broad = moffat_field(120, 120, &[MoffatStar { x: 60.0, y: 60.0, amp: 0.3, alpha_x: 24.0, alpha_y: 24.0, theta: 0.0 }], 20.0, 0.05);
    add_noise(&mut broad, 0.002, 2);
    let v = verdict(&measured(&broad, 120, 120, vec![seed_at(60.0, 60.0)]), 0);
    let truth = 2.0 * 24.0 * (2f64.powf(1.0 / 20.0) - 1.0).sqrt();
    assert!(v.plateau && v.excluded >= 9, "{v:?}");
    assert!((v.fwhm / truth - 1.0).abs() < 0.03, "{} vs {truth}", v.fwhm);
}

#[test]
fn a_full_clipped_plateau_is_censored_flagged_and_keeps_its_width() {
    // every 3x3 of the twelve sits at the clip: the plateau test passes, the level excludes the plateau, and the
    // censored fit reads the width (R2: 12 of 12 censored and flagged, FWHM median −0.6 %)
    let (data, seeds) = twelve_clipped();
    let m = measured(&data, 220, 170, seeds);
    let truth = 2.0 * 3.0 * (2f64.powf(0.25) - 1.0).sqrt();
    let mut err: Vec<f64> = (0..12).map(|i| {
        let v = verdict(&m, i);
        assert!(v.plateau && v.excluded >= 9 && v.saturated, "{i}: {v:?}");
        (v.fwhm / truth - 1.0).abs()
    }).collect();
    err.sort_by(f64::total_cmp);
    assert!(err[6] < 0.03, "median |ΔF|/F {}", err[6]);
}

#[test]
fn the_capped_range_flags_a_small_core_star_beside_a_plateau_lifted_above_full_scale() {
    // a flat-divided field: vignetting 1 + 0.3·d²/28900 about (40, 40) lifts the full plateau at (190, 120) to
    // ≈ 1.24 (legal on a calibrated plane), which sets the plane max. The range stays 1 − bg, so the small-core star
    // keeps A/range ≈ 1.25 and is flagged; an uncapped range (1.24 − bg) would put it at ≈ 1.0
    let stars = [small_core_star(40.3, 40.7, 2.85), MoffatStar { x: 190.0, y: 120.0, amp: 3.0, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 }];
    let (w, h) = (240usize, 160usize);
    let mut data = moffat_field(w, h, &stars, 4.0, 0.05); clipped(&mut data, 0.95);
    for y in 0..h { for x in 0..w {
        let d2 = (x as f64 - 40.0).powi(2) + (y as f64 - 40.0).powi(2);
        data[y * w + x] *= (1.0 + 0.3 * d2 / 28900.0) as f32;
    } }
    add_noise(&mut data, 0.002, 3);
    let m = measured(&data, w, h, vec![seed_at(40.3, 40.7), seed_at(190.0, 120.0)]);
    let (small, lifted) = (verdict(&m, 0), verdict(&m, 1));
    assert!(!small.plateau && small.saturated, "{small:?}");
    assert!(lifted.plateau && lifted.excluded >= 9 && lifted.saturated, "{lifted:?}");
}

#[test]
fn all_saturated_sample_leaves_the_frame_statistic_empty_not_wrong() {
    let (data, seeds) = twelve_clipped();
    let m = measure_plane(&data, 220, 170, &MeasureOptions { seeds: SeedSpec::Given(seeds), psf_model: PsfModel::Moffat4, ..Default::default() }, None);
    assert_eq!(m.frame.stars_fitted, 12);
    assert_eq!(m.frame.stars_unsaturated, 0);
    assert_eq!(m.frame.fwhm_px, None);
    assert!(m.stars.iter().all(|f| f.flags.saturated()));
}

#[test]
fn beta_sample_skips_saturated_stars() {
    // 70 Moffat-4 stars, the 40 brightest clipped: the discrete Auto search must still resolve 4.0 (the baseline
    // flipped to 10); R2: 40 of 40 flagged, 0 of 30
    let stars: Vec<MoffatStar> = (0..70).map(|i| MoffatStar { x: 20.0 + (i % 10) as f64 * 40.3, y: 20.0 + (i / 10) as f64 * 40.7,
        amp: if i < 40 { 2.0 } else { 0.25 }, alpha_x: 5.0, alpha_y: 5.0, theta: 0.0 }).collect();
    let mut data = moffat_field(420, 320, &stars, 4.0, 0.05); clipped(&mut data, 0.95); add_noise(&mut data, 0.002, 6);
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp.min(0.9), flux: s.amp }).collect();
    let m = measure_plane(&data, 420, 320, &MeasureOptions { seeds: SeedSpec::Given(seeds), ..Default::default() }, None);
    assert_eq!(m.beta, 4.0, "the sample is the unsaturated 30, not the clipped 40");
    assert_eq!(m.frame.stars_unsaturated, 30);
}
```

- [ ] **Step 2: Run, fail, implement, run.** Re-pin the measure.rs literals (by design: the statistic's population).
  A fixture premise that does not hold on the module's region (the comments quote R2's port, which used the box
  ± ceil(6F)) is a STOP with the verdict's fields, not a retuned fixture. Report the Measure-stage wall time on one
  real calibrated frame, before (51021d06) and after (C-b); any restriction for speed goes to the owner with those
  numbers.

- [ ] **Step 3: Lab cycle** `t08-f3`. First (Q9) update `bars.py` and `design.json`: the C-d bars, row 10's column, the
  payoff from `FitDiagnostics.verdict` and the OSC judgement; then run. Design list: `{"row":7,"path":"measure","kind":"set-aside"}`,
  `{"row":7,"path":"betas@*","kind":"set-aside"}` (the statistic's population), `{"row":11,"path":"measure","kind":"set-aside"}`,
  `{"row":11,"path":"betas@*","kind":"set-aside"}` (the twins), `{"row":10,"metric":"auto beta vs saturated top-64 seeds","path":"measure","kind":"set-aside"}`,
  `{"row":10,"metric":"saturation flag vs truth","path":"measure","kind":"set-aside"}`,
  `{"row":3,"family":"saturated/*","path":"measure","kind":"set-aside"}`, `{"row":3,"family":"saturated/*","path":"betas@*","kind":"set-aside"}`,
  `{"row":3,"family":"twins/*","path":"measure","kind":"set-aside"}`, `{"row":3,"family":"twins/*","path":"betas@*","kind":"set-aside"}`,
  `{"row":1,"path":"register","kind":"reported"}`, `{"row":2,"path":"register","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (F10). Row 1
  `bright coverage (300)` is JUDGED within SE (it counts saturated stars, N-5 — unchanged by design; ruling F20
  removed it from the list).

- [ ] **Step 4: Read the bars** (`bars.txt`):
  - self-consistency (row 10 `saturation flag vs truth`, pooled over every family × `measure`; C-d): false
    negatives ≤ 0.5 % among truth-saturated stars with `clipped_core_px ≥ 3`; false positives reported as a count
    and a share of the truth-unsaturated stars, no bar (R2: 0.6–0.8 %, 7.5 % under 30 % vignetting); the
    `neighbour_only`, `gray`, `under_3_core` and `mostlySaturated` counts printed beside (the v2 i0 truth has 61
    neighbour-only and 90 gray, two-sided, among the saturated — reported, not judged), with the count of
    truth-saturated stars whose verdict fit failed (`fit = false`; R2: 0);
  - OSC (Q3): the families whose planes are debayered (`colour` = true) are judged on the same FN bar, read as their
    own line beside the mono pool; a miss is a STOP. C1 item 1's other-channel clipped-site count is printed beside it;
  - the payoff (C-d, reported): the verdict fit's |ΔFWHM|/FWHM vs truth (median / p90) over truth-saturated stars
    whose plateau test passed, beside those whose test failed (R2 on exact ties: 2.2 / 9.9 % censored, 30.9 /
    66.2 % on the failed ones);
  - twins (row 11, `base vs deep (2x flux)`): frame |ΔFWHM|/FWHM ≤ 1 % on every `twins/sharp/*` and `twins/mixture/*`
    family (baseline `twins/sharp/m4/f2.0` 9.99 %, `twins/sharp/m2.5/f1.6` 15.93 %, `twins/sharp/m4/f2.6` 2.93 %,
    `twins/mixture/b` 2.62 %); `auto beta agrees` 100 % of pairs on every family (β discrete: zero flips; baseline
    0.0 % of 1 pair on five families) — the twin rows are judged AGAIN at Task 9 (N-6), where "void at the cap" may
    apply.
  - **Outcome (t08-f3, rulings 2026-10-09, `progress.md`):** FN mono 3/4089 (0.073 %), OSC 0/1229 with the
    cross-plane flag (52 without it); FP mono 0.294 %, OSC 0.411 % (90 cross, all saturated in another channel);
    twins 44/46. MOVED TO TASK 9 with research R3's evidence (`task-8-research-R3-report.md`): `twins/sharp/m2.5/f1.6`
    and `twins/sharp/m4/f2.0` (discrete β near-ties on undersampled stars; samples saturation-free; held β ≤ 0.10 %),
    `sharp/m4/f2.0` i0 (+2.62 % at β 6, +0.27 % at the truth β 4), and, by owner ruling, `betas@10` / `betas@6` ecc
    above Fisher 300 and `betas@2.5` at 20–40 (inherited from 6b/7: the betas@ rows are identical t06b → t08). The
    68 planes' "β = t05" is met by its own terms: 19 exceptions, each the 6a seed source (R3 `qc-planes.tsv`); β vs
    the truth is Task 9's. Design additions: planes whose frame β changed (under the control: every unflagged fit
    equals its held-β fit, 37 278 / 37 278), `artefacts` rows 1 and 4 (`MostlySaturated`), OSC row 8 on
    `osc/f3.5`, `osc/f5.0/blur1`, `osc_pairs/f3.5` (the cross-plane population).

- [ ] **Step 5: Record R-OSP-5 and commit.** R-OSP-5 records spec §2.1's test with its constants (5σ, 0.7, 0.1,
  3 of 8 / 8 of 8, iterations ×3, n ≤ 8), the three substitutes (bg, σ, pixel0) and the known limit with its three
  consumers. It also records the per-seed order seeds → field width → verdicts → β sample → fits at β, the field
  width's absence of any saturation test with its cost, the row, the payoff and the Measure wall time.

```
fix(star_measure): F-3 one saturation test per seed after the field width (the plateau test, a censored free-beta verdict fit, flagged when its amplitude tops the plane's range; ruling R-OSP-5); the beta sample and the frame statistic use unflagged stars (twins: <cells>; flag FN/FP: <cells>)
```

**Acceptance:** FN ≤ 0.5 % on ≥ 3-clipped-core stars, with FP and the payoff reported; every twin family ≤ 1 % with
zero flips; both diffs 0 moved outside the design list; control 0; the Measure wall time before and after reported.

---

### Task 9: F-4 — continuous per-frame β, the rustafits bounded free-β fit, the submodule bump (spec §3 F-4)

**Files:**
- rustafits (branch `one-star-path-beta` from `8321412`, or the Task 5 branch if it exists):
  `rustafits/src/analysis/fitting.rs` — `fit_moffat_2d_bounded`, `Moffat2DBoundedResult { b, a, x0, y0, alpha_x,
  alpha_y, theta, beta, converged, fit_residual, iters }`, `lm_solve_moffat_with` parametrised by `BetaParam::{Fixed(f64),
  FreeLinear, FreeLn { ln_min, ln_max }}` (ruling F8): `FreeLinear` IS today's arm (`params[7] = β`, step clamp
  `[1.5, 20]`, acceptance `[1, 20]`, `j[7] = A·power·(−ln base)`) and `fit_moffat_2d` selects it, so it stays
  byte-identical; `FreeLn` carries `params[7] = ln β`, Jacobian column `j[7] = A·power·(−ln(base))·β` (chain
  rule), the step clamp in ln β to `[ln_min, ln_max]`, `MoffatExponent::new(exp(params[7]))`; tests.
- athenaeum: `star_measure/beta.rs` (continuous; `frame_beta` takes `&FitStarsOptions` — ruling F45),
  `star_measure/fit.rs` (`BETA_AT_CAP`), `star_measure/mod.rs` (`BetaSource`, `PlaneMeasurement.beta_source`),
  `star_measure/photometry.rs` (the verdict fit uses the bounded one: `saturation_verdict` swaps to
  `fit_moffat_2d_bounded` with `BETA_BOUNDS`, PA-23; the former "bounded wing fit", I-3, is superseded by C-a, owner
  2026-10-09), `stacking/psf_signal.rs` (`PsfModel::Auto` doc, `group_beta` doc: the lower median of continuous βs —
  still "always one of the members' own βs"), `stacking/measure.rs` (hand edit: doc only),
  `src/components/stacking/panels/MeasurePanel.tsx` + `stageSummary.ts` (`psfModelLabel('auto')` → "Auto (per-frame
  β)", help text), `rustafits` submodule pointer, `Cargo.lock` (unchanged: path dep, same version),
  `docs/stacking/README.md` (R-OSP-6), `examples/star_probe.rs` (`--betas` keeps the discrete arm for row 3's
  sensitivity line; frame line `betaSource`)
- Earlier tests this task edits (ruling F14, M-6): `group_beta_is_the_lower_median_and_always_an_auto_candidate`
  (`psf_signal.rs:1515` — the "always an `AUTO_BETAS` candidate" clause becomes "always one of the members' own
  βs") and `auto_model_prefers_the_generating_beta` (`:975` — asserts `|β − 4| < 0.2` instead of `== 4.0`), Task 8's
  `beta_sample_skips_saturated_stars` (`|m.beta − 4.0| < 0.2`), each with its comment.

**Interfaces:**
- Produces (rustafits):

```rust
pub fn fit_moffat_2d_bounded(pixels: &[PixelSample], init_b: f64, init_a: f64, init_x0: f64, init_y0: f64,
    init_sigma_x: f64, init_sigma_y: f64, init_theta: f64, init_beta: f64, beta_bounds: (f64, f64),
    max_iter: usize, conv_tol: f64, max_rejects: usize) -> Option<Moffat2DBoundedResult>;
```

  Refuses (`None`) only `alpha < 0.3` or `A ≤ 0`; a β at a bound is RETURNED (the caller flags it). `fit_moffat_2d`
  keeps delegating to `FreeLinear` byte for byte (its C3 pins stay; a golden test pins its output on the C3 stamp).
- Produces (athenaeum): `beta::BETA_BOUNDS: (f64, f64) = (1.5, 100.0)`; `beta::frame_beta(plane, w, h, sample: &[Seed],
  opts: &FitStarsOptions, pool) -> (f64, BetaSource)` = the median of the bounded free-β fits of the 64 brightest
  unsaturated sample seeds (≥ 8 accepted fits, else `(4.0, BetaSource::Fallback)` with a `warn!`), clamped to the
  bounds. Those free-β fits are the seeds' verdict fits (C-b, PA-23): `frame_beta` reads their β and does not fit
  them a second time. For a caller that ran no verdicts (LN's reference fit, F45's green-site call) `frame_beta` runs its own sample fit (Q7, Task 9's brief); `BetaSource::{Fitted { sample: usize }, Fallback, Fixed}`; `StarFit.flags |= BETA_AT_CAP` when the frame
  β sits at a bound. `PsfModel::Auto` → `frame_beta`; `Moffat4`/`Fixed(b)` unchanged. `group_beta`: unchanged
  code (any number). `FitDiagnostics.iterations` filled from the bounded fit's `iters` on the sample; `None` on
  the fixed-β pass still (the fixed entry point reports none).

- [ ] **Step 1: Write the failing tests** (rustafits first, then athenaeum)

```rust
// rustafits/src/analysis/fitting.rs — `moffat_stamp(n, x0, y0, ax, ay, theta, beta)` and `gauss_stamp(n, x0, y0, sigma)`
// are the test module's point-sampled renderers (gauss_stamp is new: B 0, A 1)
#[test]
fn bounded_free_beta_reaches_a_gaussian_star_at_the_cap() {
    let pixels = gauss_stamp(41, 20.0, 20.0, 2.0);
    let r = fit_moffat_2d_bounded(&pixels, 0.0, 1.0, 20.0, 20.0, 2.0, 2.0, 0.0, 4.0, (1.5, 100.0), 100, 1e-5, 5).unwrap();
    assert!(r.beta >= 95.0, "beta {}", r.beta);
    let fwhm = 2.0 * r.alpha_x * (2f64.powf(1.0 / r.beta) - 1.0).sqrt();
    assert!((fwhm / (2.0 * 2.354_820_045) - 1.0).abs() < 0.004, "fwhm {fwhm}");
    // the old entry point on the same stamp: None or β at its own 20 cap — the two arms stay distinct
    let old = fit_moffat_2d(&pixels, 0.0, 1.0, 20.0, 20.0, 2.0, 2.0, 0.0, 100, 1e-5, 5);
    assert!(old.map_or(true, |o| o.beta >= 19.9), "{:?}", old.map(|o| o.beta));
}

#[test]
fn bounded_fit_recovers_beta_4_and_2_5_inside_the_bounds() {
    for (beta, tol) in [(4.0, 0.05), (2.5, 0.03)] {
        let pixels = moffat_stamp(41, 20.0, 20.0, 3.0, 3.0, 0.0, beta);
        let r = fit_moffat_2d_bounded(&pixels, 0.0, 1.0, 20.0, 20.0, 1.5, 1.5, 0.0, 3.0, (1.5, 100.0), 100, 1e-5, 5).unwrap();
        assert!((r.beta - beta).abs() < tol, "beta {} vs {beta}", r.beta);
        assert!(r.iters > 0);
    }
}

#[test]
fn ln_beta_jacobian_matches_central_differences() {
    for beta in [2.0f64, 10.0, 60.0] {
        let (base, a) = (1.7f64, 0.8f64);             // 1 + Q at one pixel, the amplitude
        let analytic = a * base.powf(-beta) * (-base.ln()) * beta;            // ∂M/∂(ln β)
        let h = 1e-6;
        let numeric = (a * base.powf(-beta * (h).exp()) - a * base.powf(-beta * (-h).exp())) / (2.0 * h);
        assert!(((analytic - numeric) / numeric).abs() < 1e-6, "beta {beta}: {analytic} vs {numeric}");
    }
}

/// A pin, not RED (ruling F28): `fit_moffat_2d` through `BetaParam::FreeLinear` returns the C3 stamp's exact
/// parameters and iteration count recorded before this task (the golden values are read from
/// `c3_moffat_arithmetic_delta_against_the_pre_c3_solver`'s fixture at 8321412 and pasted here).
#[test]
fn fit_moffat_2d_is_byte_identical_to_before() {
    let pixels = c3_fixture_stamp();
    let r = fit_moffat_2d(&pixels, 100.0, 5000.0, 15.0, 15.0, 2.0, 2.0, 0.0, CALIBRATION_MAX_ITER, CALIBRATION_CONV_TOL, CALIBRATION_MAX_REJECTS).unwrap();
    assert_eq!((r.alpha_x, r.alpha_y, r.theta, r.beta, r.fit_residual), C3_GOLDEN_FREE_BETA);
}

// athenaeum beta.rs
#[test]
fn frame_beta_is_the_median_of_the_sample_and_gaussian_reads_near_the_cap() {
    use crate::test_support::{moffat_field, gaussian_field, MoffatStar};
    let stars: Vec<MoffatStar> = (0..70).map(|i| MoffatStar { x: 20.0 + (i % 10) as f64 * 40.3, y: 20.0 + (i / 10) as f64 * 40.7,
        amp: 0.2 + 0.05 * (i % 5) as f64, alpha_x: 5.0, alpha_y: 5.0, theta: 0.0 }).collect();
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: s.amp * 26.0 }).collect();
    let data = moffat_field(420, 320, &stars, 4.0, 0.05);
    let (beta, src) = frame_beta(&data, 420, 320, &seeds, &FitStarsOptions::default(), None);
    assert!((beta - 4.0).abs() < 0.2, "{beta}");
    assert_eq!(src, BetaSource::Fitted { sample: 64 });
    let g = gaussian_field(420, 320, &stars.iter().map(|s| (s.x, s.y, s.amp)).collect::<Vec<_>>(), 2.0, 0.05);
    let (beta, _) = frame_beta(&g, 420, 320, &seeds, &FitStarsOptions::default(), None);
    assert!(beta >= 60.0, "{beta}");
    let m = measure_plane(&g, 420, 320, &MeasureOptions { seeds: SeedSpec::Given(seeds.clone()), ..Default::default() }, None);
    assert_eq!(m.stars.iter().all(|f| f.flags.beta_at_cap()), m.beta >= BETA_BOUNDS.1 - 1e-9);
}

#[test]
fn beta_sample_below_eight_falls_back_to_four_and_says_so() {
    use crate::test_support::{moffat_field, MoffatStar};
    let stars: Vec<MoffatStar> = (0..5).map(|i| MoffatStar { x: 30.0 + i as f64 * 40.0, y: 30.0, amp: 0.3, alpha_x: 5.0, alpha_y: 5.0, theta: 0.0 }).collect();
    let data = moffat_field(240, 60, &stars, 2.5, 0.05);
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect();
    let (beta, src) = frame_beta(&data, 240, 60, &seeds, &FitStarsOptions::default(), None);
    assert_eq!((beta, src), (4.0, BetaSource::Fallback));
    let m = measure_plane(&data, 240, 60, &MeasureOptions { seeds: SeedSpec::Given(seeds), ..Default::default() }, None);
    assert_eq!(m.beta_source, BetaSource::Fallback);
    assert_eq!(m.stars.len(), 5, "the frame still measures");
}

#[test]
fn every_star_is_fitted_at_the_frame_beta() {
    let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
    let m = measure_plane(&data, w, h, &MeasureOptions { seeds: SeedSpec::MatchedFilter { threshold: seeds::DEFAULT_THRESHOLD }, ..Default::default() }, None);
    assert!(m.stars.len() >= 140);
    assert!(m.stars.iter().all(|f| f.beta == m.beta));
    assert!(matches!(m.beta_source, BetaSource::Fitted { .. }));
}
```

- [ ] **Step 2: rustafits.** `cd rustafits && git checkout -b one-star-path-beta` (or the Task 5 branch);
  implement; `CARGO_TARGET_DIR=../.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p rustafits --lib --
  analysis::fitting --test-threads=4`; commit in rustafits:

```
analysis: fit_moffat_2d_bounded - a free-beta Moffat fit in ln beta with caller bounds; BetaParam keeps the linear free-beta arm byte-identical for fit_moffat_2d
```

  Then in athenaeum: `git add rustafits` (the pointer), implement `beta.rs`, run the module tests.

- [ ] **Step 3: Lab cycle** `t09-f4` (v2 and injections). Design list: `{"row":3,"path":"measure","kind":"set-aside"}`
  (every width moves with β), `{"row":3,"path":"betas@*","kind":"set-aside"}`, `{"row":3,"metric":"beta sensitivity","path":"betas","kind":"set-aside"}`,
  `{"row":7,"path":"measure","kind":"set-aside"}`, `{"row":7,"path":"betas@*","kind":"set-aside"}`,
  `{"row":11,"path":"measure","kind":"set-aside"}`, `{"row":11,"path":"betas@*","kind":"set-aside"}`,
  `{"row":10,"metric":"auto beta vs saturated top-64 seeds","path":"measure","kind":"set-aside"}`,
  `{"row":10,"metric":"saturation flag vs truth","path":"measure","kind":"set-aside"}` (the verdict fit moved to the
  bounded fitter, PA-23; the F-3 bar is read again here and must still hold), `{"row":1,"metric":"fit outcomes","path":"measure","kind":"set-aside"}`
  (the bounded verdict fit and the continuous β change which seeds are refused and flagged), `{"row":6,"metric":"captured fraction","path":"measure","kind":"set-aside"}`,
  `{"row":6,"metric":"captured fraction","path":"betas@*","kind":"set-aside"}` (the FWTM ellipse follows β),
  `{"row":4,"metric":"floor","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"floor","path":"betas@*","kind":"set-aside"}`,
  `{"row":4,"metric":"ecc","path":"measure","kind":"set-aside"}`, `{"row":4,"metric":"ecc","path":"betas@*","kind":"set-aside"}`,
  `{"row":5,"path":"measure","kind":"set-aside"}`, `{"row":5,"path":"betas@*","kind":"set-aside"}`,
  `{"row":2,"path":"measure","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (within CRLB),
  `{"row":1,"path":"register","kind":"reported"}`,
  `{"row":2,"path":"register","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (F10). (The dead `auto beta agrees` row-10 entry is gone — it is a
  row-11 metric, covered by the row-11 entry; ruling F20.) LN's reference fit under `PsfModel::Auto`
  (`ln/scale.rs:993`) now runs the 64-fit bounded sample: the Normalize stage wall is not under the +20 % bar and is
  reported in Task 13's stage table.

- [ ] **Step 4: Read the bars** (`bars.txt`):
  - frame FWHM error vs `pointfit` (row 7 `frame FWHM` ÷ truth `pointfit: med` − 1): ≤ 1 % at FWHM ≥ 2 px and ≤ 3 %
    below, on every `sharp/{gauss,m2.5,m4,m10,blur1,blur2.5}/f*` (46 families after Task 1a, incl. f12/f16 — ruling
    F32) and `mixture/*` (4). Baseline (measure, pointfit, row 3 bias): `sharp/gauss/f2.6` −3.01 %, `sharp/gauss/f8.0`
    −2.58 %, `sharp/blur2.5/f2.6` −2.50 %, `sharp/blur2.5/f8.0` −2.07 %, `mixture/b` +3.28 % (frame +4.88 %),
    `sharp/m4/f1.2` +4.65 %, `sharp/blur2.5/f1.2` +21.38 % (the F 1.2 rows are judged at ≤ 3 %); the `sharp/gauss`
    rows Task 6b reported to the owner are read again here (PA-20 a);
  - the twin rows again (N-6): frame |ΔFWHM|/FWHM ≤ 1 %; `|Δβ|/β ≤ 10 %` between base and deep on every twins
    family whose β is not at a bound on both sides (`void at the cap` rows listed, not judged);
  - raw mode (injections): the injected stars' FWHM bias (row 3 `fwhm` pointfit on `inject/*`): |bias| ≤ 1 % on
    the 7 raw + 3 mosaic + 9 calibrated entries at FWHM ≥ 2 px (baseline Horse C11 G −3.68 %, QHY set 107 +5.33 %,
    `raw/corpus` plane 1 +9.06 %, mosaics plane 2 −5.85 %); a VNG-plane row (planes 1/2 of mosaics) that misses is
    reported under the out-of-scope VNG item, not judged (spec §3 "Out of scope");
  - row 3's `beta sensitivity` line stays (the discrete arm is a lab diagnostic);
  - F-3's self-consistency row again with the verdict fit on the bounded fitter (PA-23, C-d): FN ≤ 0.5 % on
    ≥ 3-clipped-core stars; FP (count and share), the payoff and the `mostlySaturated` refusals reported, no bar.
  - **Carried from Task 8 (2026-10-09):** the twins `twins/sharp/m2.5/f1.6` and `twins/sharp/m4/f2.0` (≤ 1 %,
    |Δβ|/β ≤ 10 %), `sharp/m4/f2.0` i0 frame FWHM (≤ 3 % below FWHM 2 px under F-4's own row, ≤ 1 % if it sits at
    ≥ 2 px), β vs the truth on the 19 planes of R3's table, OSC row 8 on the three cross-plane families re-read,
    and the owner ruling: same-star ecc above Fisher 300 on `betas@10` / `betas@6` and `betas@2.5` at 20–40 back to
    ≤ t06a by the sign rule — a miss there goes to the owner with a 6b cause study.

- [ ] **Step 5: The UI label, README R-OSP-6, commit** (athenaeum: the module, the pointer, the UI; lab results).

```
fix(star_measure): F-4 continuous per-frame beta in [1.5, 100] from a bounded free-beta sample fit (rustafits one-star-path-beta <sha>); Gaussian is the large-beta limit (<bar cells>)
```

**Acceptance:** every family row ≤ 1 % / ≤ 3 %; twins ≤ 1 % and |Δβ|/β ≤ 10 % (or void); injected FWHM ≤ 1 % on
the mono and ideal-plane entries; both diffs 0 moved outside the design list; the rustafits tests green on the
branch (the golden `fit_moffat_2d` pin included); `cargo tree -p athenaeum-core | grep rustafits` shows the path
dep unchanged in version.

**Outcome (2026-10-09).** Commits (athenaeum, branch `one-star-path`): 1d2d40cb (the first build: bounded free-β
sample fit, continuous β), 5aecffc9 (owner E5: the frame β from a pixel-integrated re-fit of the sample; review M-1…M-8),
de4e5b7b (A3: a sample/verdict solve that never left its start is refused), c7e6a2f8 (A4: the same for the fit at β),
61838081 (R2: the plateau-clip clause), then the close wave (review-2 minors, README, spec, plan). rustafits branch
`one-star-path-beta`: 0ebc618 → ad0fe66 (subsample k) → e9cfdb1 → c02fa93. Lab step `t09-f4` (athenaeum-lab bd2a059):
**BAR PASS**: control (fixed-β arms bit for bit but the 71 + 527 stuck lines A4 removed and one moved), frame FWHM 0/58
over, twins 0/46, inject mono at β 4 0/3, pooled centroid x1.0018 (measure) / x1.0021 (register) against 1.01, FN
mono 0.098 % / OSC 0 %, FP mono 0.156 %, neb3_4096 −0.253 σ, same-star FWHM pooled better, angle efficiency 1.25;
Measure wall +4.0 % vs t08-f3. Rulings of 2026-10-09: E5, C6 1-4, C7 1-4, R2, the ERP rule (README R-OSP-5 / R-OSP-6).

Carries. **Task 13:** the M42 / bright-nebula frame-β instability (MUST-RESOLVE before Plan A closes: R2 gives
13.96 / 3.14 / 3.34 on the mono2 frames, t08 read 2.5 / 2.5 / 6.0; the sample is nebulosity plus near-clip stars whose
calibrated clip is under 1.0; numbers in the ledger); real OSC planes at the β 1.5 bound; the drizzle widths 5-8 % low;
the defocused frame count. **Task 14:** the centroid F ≤ 2 debt line (x1.0136 / x1.0113), the owner ecc lines (item 6
pooled ecc > 300 WORSE, item 7 WORSE under a held β). **Later items (outside Plan A):** the per-frame pixel-phase
centroid correction; the LM λ start for the ~37 cost-stuck small stars (A4 concern 1: a minimum-progress check); an
inject ERP driver (owner option); the Windows / Linux runner check of the 1e-12 pins before any push to main; the legacy
rustafits θ fix and the Settings saturation-fraction question at Plan C.

---

### Task 10: F-5 — the profile-independent flux and psfSel (spec §3 F-5)

**Files:**
- Modify: `star_measure/photometry.rs` (`model_flux_in_fwtm`, `aperture_correction`, `growth_curve`),
  `star_measure/fit.rs` (`aperture` replaced by the model flux × `c`; `area` unchanged), `star_measure/mod.rs`
  (`PlaneMeasurement.aperture_correction`; `FitStarsOptions` gains `flux_correction: FluxCorrection::{FromPlane,
  Given(f64)}`, default `FromPlane`), `stacking/psf_signal.rs` (re-exports), `stacking/ln/scale.rs` (NO edit: psfSel reads
  `signal`, which now IS the corrected flux — PA-5), `examples/star_probe.rs` (frame line `apertureCorrection`),
  `examples/ln_scale_probe.rs` (nothing), `docs/stacking/README.md` (R-OSP-7)
- Earlier tests this task edits (ruling F14, F27): `fixed_beta_fit_recovers_centre_width_background_and_aperture_signal`
  (`psf_signal.rs:925`, a 3-star fixture: fewer than 5 isolated stars qualify, so `c = 1` and `signal =
  model_flux_in_fwtm ≈ 0.8222·total` — the re-pin asserts exactly that and `aperture_correction == None`); the
  measure.rs literals (tflux, tmean_flux, PSFSW, PSFSNR move by design).

**Interfaces:**
- Produces: `StarFit.signal` = `model_flux_in_fwtm(fit) × c`, where `model_flux_in_fwtm = A·π·αx·αy/(β−1)·(1 −
  10^{(1−β)/β})` (the analytic integral of the fitted Moffat inside its own FWTM ellipse) and `c` is the frame's
  aperture correction: over the 64 brightest unsaturated accepted fits with no other seed within `6·F_field` px,
  `c = median( (Σ_{r ≤ 5F} (v − b_rim)) / model_flux_in_fwtm )`, `F` the frame's fitted FWHM, `b_rim` the median of
  the `[6F, 8F]` annulus of the same star; `c = 1` with `aperture_correction: None` and a `warn!` when fewer than 5
  stars qualify (Review Focus 3). `fit_stars_at` computes `c` on the plane it fits (`FromPlane`) or takes it
  (`Given`), so LN's two sides each get their own `c`.

- [ ] **Step 1: Write the failing tests** (`photometry.rs` and `mod.rs` tests)

```rust
use crate::test_support::{moffat_field, gaussian_field, MoffatStar};

fn grid30(alpha: f64) -> Vec<MoffatStar> {
    (0..30).map(|i| MoffatStar { x: 60.0 + (i % 6) as f64 * 70.0, y: 60.0 + (i / 6) as f64 * 70.0, amp: 0.3, alpha_x: alpha, alpha_y: alpha, theta: 0.0 }).collect()
}
fn seeds_of(stars: &[MoffatStar]) -> Vec<Seed> { stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect() }

/// RED today (ruling F28): a Moffat-2.5 star's `signal` is 0.749 of its total at the FWTM (D4); after F-5 it is the total.
#[test]
fn captured_flux_is_the_total_flux_on_a_moffat_2_5_field() {
    let stars = grid30(5.0);                                               // α 5 at β 2.5: total π·A·α²/(β−1) = π·0.3·25/1.5
    let total = std::f64::consts::PI * 0.3 * 25.0 / 1.5;
    let data = moffat_field(480, 400, &stars, 2.5, 0.05);
    let m = measure_plane(&data, 480, 400, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), psf_model: PsfModel::Fixed(2.5), ..Default::default() }, None);
    assert_eq!(m.stars.len(), 30);
    let med = median(m.stars.iter().map(|f| f.signal / (total * ADU_SCALE as f64)));
    assert!((med - 1.0).abs() < 0.01, "captured fraction {med}");
    assert!(m.aperture_correction.is_some());
}

#[test]
fn captured_flux_spread_across_profiles_is_within_two_percent() {
    // the same field as Gaussian (σ 2.123 → FWHM 5), Moffat 2.5, 4, 10 at equal total flux; (max − min)/min of the four medians ≤ 2 %
    let total = 1.0f64;
    let mut meds = Vec::new();
    for (beta, alpha) in [(2.5, 5.0 / (2.0 * (2f64.powf(0.4) - 1.0).sqrt())), (4.0, 5.0 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt())), (10.0, 5.0 / (2.0 * (2f64.powf(0.1) - 1.0).sqrt()))] {
        let amp = total * (beta - 1.0) / (std::f64::consts::PI * alpha * alpha);
        let stars: Vec<MoffatStar> = grid30(alpha).into_iter().map(|s| MoffatStar { amp, ..s }).collect();
        let data = moffat_field(480, 400, &stars, beta, 0.05);
        let m = measure_plane(&data, 480, 400, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), ..Default::default() }, None);
        meds.push(median(m.stars.iter().map(|f| f.signal / ADU_SCALE as f64)));
    }
    let sigma = 5.0 / 2.354_820_045;
    let gstars: Vec<(f64, f64, f64)> = grid30(1.0).iter().map(|s| (s.x, s.y, total / (2.0 * std::f64::consts::PI * sigma * sigma))).collect();
    let g = gaussian_field(480, 400, &gstars, sigma, 0.05);
    let m = measure_plane(&g, 480, 400, &MeasureOptions { seeds: SeedSpec::Given(gstars.iter().map(|&(x, y, p)| Seed { x, y, peak: p, flux: 1.0 }).collect()), ..Default::default() }, None);
    meds.push(median(m.stars.iter().map(|f| f.signal / ADU_SCALE as f64)));
    let (lo, hi) = (meds.iter().cloned().fold(f64::INFINITY, f64::min), meds.iter().cloned().fold(0.0, f64::max));
    assert!((hi - lo) / lo < 0.02, "{meds:?}");
    assert!(meds.iter().all(|m| (m - total).abs() < 0.01), "{meds:?}");
}

#[test]
fn aperture_correction_without_an_unsaturated_isolated_star_is_one_and_flagged() {
    let stars = vec![MoffatStar { x: 40.0, y: 40.0, amp: 0.3, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 },
                     MoffatStar { x: 52.0, y: 40.0, amp: 0.3, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 },
                     MoffatStar { x: 46.0, y: 52.0, amp: 0.3, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 }];
    let data = moffat_field(100, 100, &stars, 4.0, 0.05);
    let m = measure_plane(&data, 100, 100, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), psf_model: PsfModel::Moffat4, ..Default::default() }, None);
    assert_eq!(m.aperture_correction, None);
    let f = &m.stars[0];
    let model = f.amplitude * std::f64::consts::PI * (f.fwhm_x / 2.0 / (2f64.powf(0.25) - 1.0).sqrt()) * (f.fwhm_y / 2.0 / (2f64.powf(0.25) - 1.0).sqrt()) / 3.0 * (1.0 - 10f64.powf(-0.75));
    assert!((f.signal / model - 1.0).abs() < 1e-9, "c = 1: signal is the bare model flux");
}

#[test]
fn ln_refit_takes_a_given_correction() {
    let stars = grid30(5.0);
    let data: Vec<f32> = moffat_field(480, 400, &stars, 4.0, 0.05).iter().map(|v| v * ADU_SCALE).collect();
    let own = fit_stars_at(&data, 480, 400, &seeds_of(&stars), 4.0, &FitStarsOptions::default(), None);
    let given = fit_stars_at(&data, 480, 400, &seeds_of(&stars), 4.0, &FitStarsOptions { flux_correction: FluxCorrection::Given(1.0), ..Default::default() }, None);
    assert!(own.aperture_correction.unwrap() > 1.05, "{:?}", own.aperture_correction);
    assert!((given.fits[0].signal * own.aperture_correction.unwrap() / own.fits[0].signal - 1.0).abs() < 1e-9);
}
```

  `median(iter)` is a `#[cfg(test)]` helper in `mod.rs`; `FitOutcome.aperture_correction: Option<f64>`.

- [ ] **Step 2: Run, fail, implement, run.** The re-pins named under Files, each with its comment.

- [ ] **Step 3: Lab cycle** `t10-f5` (v2 incl. the LN probe on the pairs families — reported, PA-14). Design list:
  `{"row":6,"metric":"captured fraction","path":"measure","kind":"set-aside"}`, `{"row":6,"metric":"captured fraction","path":"betas@*","kind":"set-aside"}`
  (every fit's `signal` is the new flux, F20 as folded), `{"row":1,"path":"register","kind":"reported"}`,
  `{"row":2,"path":"register","kind":"one-sided","keys":["bias_x","bias_y","scatter_x","scatter_y"]}` (F10:
  Register's flux ordering reads `signal`). Row 10 `seed flux
  rank` is JUDGED within SE (unchanged seeds; ruling F20 removed it from the list). The weights have no scorecard row
  (the real-data checkpoint reads them).

- [ ] **Step 4: Read the bars** (`bars.txt`, PA-20 c/d and F24): row 6 `captured fraction` medians over
  `sharp/{gauss,m10,m4,m2.5,blur1,blur2.5}/f2.6` and `/f8.0`: `(max − min)/min` ≤ 2 % at each F (baseline 18.73 % at
  F 2.6 from 0.9078 / 0.7646; 22.16 % at F 8.0 from 0.9157 / 0.7496); psfSel (row 6 `arm psfSel`, `ln beta auto`):
  `pairs/iso/blur25mix` t60 and `pairs/iso/blur4` t60 within ±2 % (baseline −13.03 % / −7.28 %); every pure Moffat
  and Gaussian pair target (`pairs/iso/{m4,g}`, `pairs/main/{m4,m25,g,m4w10}` and the other single-profile grid
  families) within ±1.2 % (baseline +0.56 / −0.97 / +0.32 / +0.98 / −0.93 / +0.60 % on their t60 targets); the
  mixture pairs (`*mix*`, e.g. `pairs/main/m4mix` t25 −1.60 %) no worse than the baseline beyond their SE. F1's
  `arm f1_gc` rows are printed for Plan B (PA-14).

- [ ] **Step 5: R-OSP-7; commit.**

```
fix(star_measure): F-5 star flux = the fitted model's FWTM flux times a per-frame growth-curve correction; psfSel reads it (captured-fraction spread <cells>; blur pairs <cells>)
```

**Acceptance:** spread ≤ 2 % at F 2.6 and F 8.0; psfSel bars as above; both diffs 0 moved outside the design list.

**Outcome (2026-10-11).** Commits (athenaeum, WIP branch `one-star-path-t10-wip`, to be squashed into ONE F-5 commit on
`one-star-path`): 7af3bdc9 (Phase A), 1d527bcb (round 2), d130ed60 (round 3, R6), e3312b03 (round 4: footprint, the
caller's full scale), then the close commit (the STOP-4 bound, README R-OSP-7). Lab step `t10-f5`: r1 STOP (LN `c` wrong:
clipped stars in the 64 and a non-linear median ring) -> research RF5 / RF5b / RF5c -> R6 -> RF6 / RF7 / RF8 -> r2 at
e3312b03 (lab 39bb670) **BAR PASS**: C1.1 0.76 % / 1.38 %, C1.2 +1.72 % / +1.19 %, C1.3 0 / 70, C1.4 0 / 20; control,
register guard and both no-regression diffs PASS. Rulings of 2026-10-11: C6 (R6: far-ring plain mean, seed mask, amplitude
screen), C7 (record the paired deltas, do not judge), RF7 (STOP 4: half/full bound 0.004 -> 0.030 = 3 sigma of 0.99 %, both
truth assertions kept), RF8 (KEEP R6, no per-star residual screen; the injected-star rows are a truth artefact). README
R-OSP-7, spec §2.1 / F-5 updated.

Carries. **Task 13:** the nebula seed flood (hc11B L 0010 at the 24 576-seed cap: isolation leaves junk qualifiers, `c`
1.137 +- 25 %; the same seed mechanism as the M42 frame-beta MUST-RESOLVE - re-read `c` there), the nebula-background `c`
bias (neb3 -11.5 % +- 5.6 % SE, neb3_4096 +13.9 % +- 2.3 %), the OSC LN reference R-plane `c` SE 2.19 %. **Plan B:**
growth/model falls for fainter stars (all-pairs LN-like -4.7 %). **Plan C:** a CFA-masked plane gets `c` = 1 (no qualifier).

---

### Task 10b: F-10 — one background (detection noise, knots, field width, per-star sky, σ_i) (spec §3 F-10)

Owner-approved scope (2026-10-11, "да, делай 10b сейчас"); the design is the controller's rulings
(`.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-10b-rulings.md`: A1–A6, B1–B5, C, the two addenda, P1–P5 on
the draft and R1–R3 on the plan review `review-t10b-plan.md`, every finding of which is applied here), drawn
from research RB1 (lab 5f705ad) and RB2, design studies D1 (lab d63dfc7, revision 2) and D2
(`task-10b-design-D2-report.md`), the ERP comparison on the trailed frames (lab 16edeaa) and the planning review
(`planning-vector-fable.md`). The spec row is F-10 because F-8 and F-9 already name Plan B's items. Principle: plan by
measured quantity — this task OWNS the detection noise σ_f, the seeds, the field width, the aperture-scale sky B_i and
the per-star noise σ_i (spec §3 "Owned quantities"); every consumer reads them and none re-estimates them; a fix round
that adds a second estimate of an owned quantity is a STOP and a re-audit of that quantity, and three fix rounds inside
one quantity re-audit it before a fourth.

**Out of scope (must not creep in):** LN's tile sky (`ln/background.rs`, Plan B), registration's detector, the budget
(Task 12), the SNR formula (Task 11), the c = 1 floor (F-9), any per-star residual screen on `c` (RF8's C_res, rejected),
sub-10F nebula structure (a recorded limit with SE), a hand retune of K, a Settings field, fixing the fit's b (F2,
rejected outright: D1 rev. 2), a product switch to the two-ring candidate K7 (reported beside R6 only), the near-clip
class-B stars behind M42 frame 0000's β (D2: a saturation item, not knots — Task 10c's, ruling P2; 10b reports the
M42 β with its mechanism split, row S.3).

**Base and phases (lean execution: a fresh agent per phase, short reports, the controller restarts between them).**
WIP branch `one-star-path-t10-wip` (Task 10 complete at fee44d50, not yet squashed); rustafits unchanged (45b5782); lab
main after 10b-L. A1 → A2 → A3 → A4 on the dev Mac (rust-engineer: A1, A2, A4 opus; A3, a refactor under a
byte-identity gate, sonnet — lean execution), each committed as `wip(star_measure): F-10 <phase> - …`; L on the runner
in parallel; L2 (lab tooling) on the runner once A4's interfaces are fixed; C on the runner once A4, L and L2 are in
(judgment run, sonnet or stronger); D last. No
lab bar is read before C: the A phases read their unit tests and a real-frame proof table only.
`MEASURE_VERSION` is not bumped (PA-4); `FITS_ARTIFACT_VERSION` is unchanged (B_i and σ_i are not persisted, like
`peak5`; `EXTENDED` is a named bit of the existing flags byte).

**Coupling checklist (the task; each phase repeats its part).**
- Plane sky and plane noise unchanged: the saturation range keeps `SkyLevel.median` and `NoiseModel.sigma`; the mesh
  M is the plane's one map (ruling A2) — nothing builds a second one.
- Seeds ↔ F_field ↔ `c`'s isolation (6·F_field) ↔ the ring's seed mask (3F): every seed change moves B_i on nebula
  and the isolation population; the isolation constant stays 6 unless the knots family says otherwise.
- The fit's b stays free (B1); B_i is a different scale, never a fit input.
- The β sample: Task 9's 64 brightest unflagged → plus centred and not extended; it moves the frame β, hence every fit
  at β and the model flux (c's denominator). `extended` also leaves `c`'s qualifiers and the frame FWHM/ecc statistic
  (ruling P3: the saturation flag's treatment), so the frame F — B_i's ring, `c`'s 5F — moves with it.
- Frame classes (ruling R1), rendered before any reading: `trailed` (L 0010, L 0011: streaks, no measurable PSF) is
  the A4 verdict's target; `elongated` (G 0003: measurable short dashes, ecc ≈ 0.65) is measured as is, its ecc
  reported, NO trailed verdict, and it leaves every clean pooled row — rejecting it is frame selection's job (an ecc
  gate), not measurement's.
- `c`'s qualifiers: ≥ 5 everywhere a clean plane had them, 64 on clean frames; the c = None fallback on junk planes.
- Rulings this task may revisit, each by its own rule: R-OSP-2 / PA-2 (K re-swept only if a flat-family row moves),
  PA-17 and F25 (the field width and its saturated candidates), C0's isolation, C6/C7 (R6 stays as B_i's geometry),
  Task 13's L 0010 item (moved here; its M42 β item moved on to Task 10c, ruling P2), PA-11's annulus for SNR
  (retired by B4; Task 11 reads σ_i).
- STOP rules (each with its numbers to the controller): a second estimate of an owned quantity; a moved `c` in A3; a
  moved pin; a test whose premise fails (P5); P.3's both-arms failure; three fix rounds inside one quantity; a streak
  refusal-share line that does not separate L 0010 / L 0011 from G 0003 and the clean frames (A4 step 2a: the table at the plan-fix of 2026-10-11 already shows the derived line firing on G 0003 — the first thing A4 hands the controller).

**Files:**
- Modify: `star_measure/seeds.rs` (A1: `detection_noise`, `DetectionNoise`, the knot metrics, bounds and verdict,
  `DetectionReport`; A2: the field width), `star_measure/photometry.rs` (A1: `SaturationVerdict.centred`; A3:
  `ring_sky`, `RingSky`, `growth_curve` and `aperture_correction` read B_i), `star_measure/fit.rs` (A1: the centre test
  on the verdict fit, `StarFlags::EXTENDED`; A3: B_i/σ_i on every accepted fit; A4: the trailed verdict from the refusal
  share, no per-star streak refusal, Q1), `star_measure/beta.rs` (A1: `sample_seeds`), `star_measure/frame.rs` (A1: the FWHM/ecc statistic
  excludes `EXTENDED` as it excludes `SATURATED`), `star_measure/diagnostics.rs` (A4: `Trailed` only),
  `star_measure/domain.rs` (A1: the mesh map computed once and shared, Q4), `star_measure/mod.rs` (the new `PlaneMeasurement` / `FitStarsOptions` fields, tests), `stacking/measure.rs` (hand
  edit: `trail_verdict` on the measure path, as `defocus_verdict`), `stacking/ln/scale.rs` (hand edit: the ring-geometry
  arm), `stacking/psf_signal.rs` (re-exports), `examples/star_probe.rs` (frame line `sigmaF`, `sigmaFMeasured`,
  `noiseGain`, `structureDropped`, `extendedSeeds`, `trailed`; star line `skyRing`, `skyRingSigma`, `skyRingN`,
  `skyOffset`, flag `extended`), `examples/ln_scale_probe.rs` (`--ln-ring per-side|pair-common`), `docs/stacking/README.md`
  (R-OSP-7b), the logging spec's field dictionary (`sigma_f`, `noise_gain`, `structure_dropped`, `extended`, `trail_share`,
  `trailed` — Task 9's precedent), the spec's F-10 row "as built" (phase D).
- Reuse (A1, review finding 13): D2's prototype `.superpowers/sdd/2026-10-07-one-star-path-plan-a/t10-scratch/d2/`
  `d2_mod.rs` + `patch_seeds.py` (the knot metrics with sub-pixel-centred kernels, the bounds, the gain read on the
  darkest boxes' high-pass residual) is A1's starting point, ported into `seeds.rs`, not re-designed; it also keeps A1
  inside one agent's budget.
- Earlier tests this task edits (ruling F14): every `seeds::detect` call (`seeds.rs` tests :876/:925/:952, `mod.rs`
  tests :1522/:1621) takes the third tuple element; `growth_curve`'s tests take B_i as an argument; `RefusalReason::ALL`
  (13 → 15) and every test that counts reasons; every `StarFit {` literal site (29 today, src + examples) gains the
  three ring fields (or moves to `StarFit::test_at`), hand-edited in the no-rustfmt files. A measure.rs pin that moves
  is a STOP with its mechanism unless the phase names it here first (the clean 150-star fixture is expected not to move
  in A1 and A3).

**Interfaces:**
- A1. `seeds::detection_noise(plane, w, h, mask, fwhm) -> DetectionNoise { sigma: Vec<f32> /* σ_f(x) = the mesh's
  per-cell clipped σ (its noise map) × gain */, gain: f64, measured: f64 /* 1.4826·MAD of the filtered plane over the
  stratified sample: today's σ_f */ }` with `median()`; the gain is the noise's filtered-to-pixel σ ratio at that kernel
  (1.00 on white noise): exactly 1 on a plane the caller states white (A1b/A1c, rulings C-7, C-10; `detect` takes
  `pixel_noise: PixelNoise { White, Mixed, Unknown }`, default `Unknown`), measured once per plane on a mixed or unknown one (debayered, resampled, integrated) (the estimator is the phase's; D2 read it on the
  high-pass residual of the darkest boxes). `seeds::detect(…) -> (Vec<Seed>,
  FieldWidth, DetectionReport { sigma_f, sigma_f_measured, noise_gain, structure_dropped: usize, extended: Vec<bool> })`,
  a candidate kept when `f(x) > K·σ_f(x)`. `seeds::knot_metrics(d, present, w, h, x, y, fwhm, sigma) -> KnotMetrics
  { snr, snr_2f, z, z_2f, rho, rho_z }` (D2's definitions: Gaussian kernels at `fwhm` and `2·fwhm` centred on the seed's
  sub-pixel position, truncated at 3σ of each; plain `f = Σk·d/√Σk²`; zero-sum responses over the 2F box with the
  box mean removed from each kernel; `rho`, `rho_z` the two kernels' correlations), `seeds::knot_bounds(fwhm) ->
  KnotBounds { plain, zero_sum }` (the same metrics on a pixel-centred, point-sampled Moffat β = `BETA_BOUNDS.0`
  stamp of FWHM `fwhm`), `seeds::knot_verdict(m, bounds, threshold, near_clip) -> KnotVerdict::{Star, Extended,
  Structure}` with `KNOT_Z = 3.09`: Structure when `snr_2f/snr > plain + KNOT_Z·√(1 + plain² − 2·plain·rho)/snr` and
  `z < threshold`; else Extended when `z > 0` and `z_2f/z > zero_sum + KNOT_Z·√(1 + zero_sum² − 2·zero_sum·rho_z)/z`;
  `near_clip` → Star, where `near_clip` reads Task 8's owned range, never a new maximum: the seed's peak −
  `SaturationPlane.bg` ≥ (1 − `PLATEAU_RANGE`)·`SaturationPlane.range` (the plateau-clip clause's own 0.9 level; whether
  that class is right is Task 10c's). Structure seeds are dropped and counted; `extended[i]` rides with seed `i`.
  `SaturationVerdict.centred: bool` (the verdict fit passes `accept`'s
  centre and region tests: `CentreMoved`, `CentreAtRim`, `RegionTooSmall`; false when `fit` is false).
  `beta::sample_seeds(passes, extended: &[bool])` keeps `fit && !saturated && centred && !extended` (an empty slice =
  none extended: given seeds). `StarFlags::EXTENDED = StarFlags(4)` on the accepted fit (named `"extended"`).
  `photometry::growth_curve`'s qualifier rule gains "and not `EXTENDED`" (beside "not saturated"); `frame.rs`:
  `fwhm_px` / `eccentricity` exclude `EXTENDED` as they exclude `SATURATED` (ruling P3).
  `PlaneMeasurement.detection: Option<DetectionReport>` (`None` for given seeds).
- A2. `seeds::field_width_of` and the matched filter's own width keep their signatures and PA-17's half-maximum
  estimator; what the phase changes (which candidates the median reads — F25 revisited — and how) is recorded in
  PA-17's text and R-OSP-7b. `FieldWidth` may gain fields the phase names.
- A3. `StarFit` gains, not persisted (NaN / 0 on a decoded fit, like `peak5`): `sky_ring: f64` (B_i, working units),
  `sky_ring_sigma: f64` (σ_i = the RMS of (v − B_i) over the same pixels), `sky_ring_n: u32` (n_i), and
  `fn sky_offset(&self) -> f64 { self.background - self.sky_ring }` (the near/far diagnostic). `photometry::ring_sky(data,
  mask, footprint, seeds: &SeedMask, w, h, x, y, fwhm) -> Option<RingSky { mean, sigma, n }>`: R6's pixel set (centres
  at 10F ≤ d ≤ 12F, present = finite, unmasked and inside the footprint, outside every seed's 3F disc), `None` under
  `ANNULUS_MIN_PIXELS` (8); computed once per accepted fit after the duplicate drop, F = the plane's frame FWHM (or
  `FitStarsOptions.ring_fwhm`). `growth_curve` takes B_i and keeps `c`'s own qualifier rule (the 12F disc inside the
  plane and the footprint, no absent pixel within 5F, B_i present); `aperture_correction` reads the stored B_i.
  `PlaneMeasurement.ring_sky_missing` / `FitOutcome.ring_sky_missing: usize`. `FitStarsOptions.ring_fwhm: Option<f64>`
  (`None` = the plane's own frame FWHM). `stacking::ln::scale`: `RingGeometry::{PerSide, PairCommon}` (pub(crate),
  default `PerSide` until phase C's decision); `PairCommon` hands both sides `ring_fwhm = max(F_R, F_T)` — the phase
  computes each side's `c` after both sides are fitted (no second fit) and says how.
- A4. `RefusalReason::Trailed`, declared after `MostlySaturated` (`Ord` kept; serde `"trailed"`; `RefusalReason::ALL`
  13 → 14). There is NO per-star streak refusal (ruling Q1: a fitted-ecc line at 0.8 refuses 2–32 % of clean real fits
  and the solver's own note puts 8–10 % on healthy frames — an elongated star is a real star, measured as such). The
  trailed verdict is FRAME-LEVEL ONLY, from the refusal structure (a fit cannot contain a star longer than its
  region): `fit::trail_share(refused) = (centreMoved + regionTooSmall + centreAtRim) / seeds judged` (A4-2), and
  `fit::TRAIL_REFUSAL_SHARE: f64 = 0.5`, the majority rule of ruling Q5 (A4 step 2a: most detected sources
  cannot be contained in a fit region; the derived clean-frame line does not separate, see the step's table); `TRAIL_VERDICT_MIN_SEEDS = 100` (the solver's `looks_trailed`
  minimum, private there, so quoted, not pinned). When it fires, as 6b's defocused verdict: every fit the plane would
  accept is refused `Trailed`, `c` None, the frame's weight inputs empty. ecc may be REPORTED beside the verdict,
  never a gate. `FitStarsOptions.trail_verdict: bool` (true only on the measure path, as `defocus_verdict`);
  `PlaneMeasurement.trailed: bool`, `.trail_share: f64`.

#### Phase A1 — detection: σ_f propagation, the knot tests, the β sample from centred seeds (rulings A1–A4)

Checklist: σ_f is read by the threshold and by the knot tests' z only; R-OSP-2's table was taken on the measured σ_f,
which equals the propagated one on clean fields (D2: 1.00–1.04), so K = 6 stays; more seeds on nebula move F_field's
first-pass candidates, `c`'s isolation and the β sample; the hot-pixel rule, the seed cap 24 576, `NoiseSource::None`
→ no seeds (Review Focus 4) and the NaN border (Review Focus 1: the knot metrics skip absent pixels) are unchanged.
`extended` now leaves `c`'s qualifiers and the frame statistic too (P3): `c` MAY move on frames with extended sample
seeds (M42: 13–24 of 64 in D2) — expected, proof-tabled with the qualifiers it lost; the A3 byte-identity gate is
unaffected (it compares A2's head with A3's). The knot exemption reads `SaturationPlane` (Task 8's owned range; if
measure_plane builds it after detection today, A1 moves its construction earlier, same inputs) — no new maximum.

- [ ] **Step 1: Write the failing tests** (`seeds.rs` tests; the β-sample and extended-seed tests in `mod.rs`)

```rust
use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

/// A noiseless round star of FWHM `fwhm` and peak 1 at (x, y) on an n × n zero plane: Moffat of `beta`, or Gaussian.
fn round_star(n: usize, x: f64, y: f64, fwhm: f64, beta: Option<f64>) -> Vec<f32> {
    match beta {
        Some(b) => {
            let a = fwhm / (2.0 * (2f64.powf(1.0 / b) - 1.0).sqrt());
            moffat_field(n, n, &[MoffatStar { x, y, amp: 1.0, alpha_x: a, alpha_y: a, theta: 0.0 }], b, 0.0)
        }
        None => gaussian_field(n, n, &[(x, y, 1.0)], fwhm / GAUSSIAN_FWHM_PER_SIGMA, 0.0),
    }
}

/// Controller arithmetic (a replica of D2's metric): Moffat β 1.5 at the field width reads plain / zero-sum
/// 0.9113 / 0.8854 at F 1.2, 0.9716 / 0.9351 at F 2.6, 0.9725 / 0.9328 at F 5, 0.9726 / 0.9326 at F 8.
#[test]
fn knot_bounds_are_a_moffat_one_and_a_half_star_at_the_field_width() {
    for (fwhm, plain, zero_sum) in [(1.2, 0.9113, 0.8854), (2.6, 0.9716, 0.9351), (5.0, 0.9725, 0.9328), (8.0, 0.9726, 0.9326)] {
        let b = knot_bounds(fwhm);
        assert!((b.plain - plain).abs() < 0.002 && (b.zero_sum - zero_sum).abs() < 0.002, "F {fwhm}: {b:?}");
    }
}

/// Real profiles at the field width are stars at every SNR (replica: Moffat 2.5 reads 0.9025 / 0.8921, the widest).
#[test]
fn star_profiles_are_neither_structure_nor_extended() {
    for fwhm in [2.6, 5.0] {
        let bounds = knot_bounds(fwhm);
        for beta in [Some(2.5), Some(4.0), None] {
            let d = round_star(121, 60.3, 59.8, fwhm, beta);
            let unit = knot_metrics(&d, None, 121, 121, 60.3, 59.8, fwhm, 1.0);
            for snr in [8.0, 2000.0] {
                let m = knot_metrics(&d, None, 121, 121, 60.3, 59.8, fwhm, unit.snr / snr);
                assert_eq!(knot_verdict(&m, bounds, DEFAULT_THRESHOLD, false), KnotVerdict::Star, "F {fwhm} β {beta:?} snr {snr}: {m:?}");
            }
        }
    }
}

/// Replica: a Gaussian blob of FWHM 4F at SNR 8 reads plain 1.70, z(F) 3.99 < K (structure); of FWHM 3F at SNR 40,
/// z(F) 26.6 ≥ K and zero-sum 1.48 (extended); a near-clip seed is exempt.
#[test]
fn a_faint_broad_blob_is_structure_and_a_bright_one_extended() {
    let f = 5.0;
    let bounds = knot_bounds(f);
    let at_snr = |k: f64, snr: f64| {
        let d = round_star(161, 80.3, 79.8, k * f, None);
        let unit = knot_metrics(&d, None, 161, 161, 80.3, 79.8, f, 1.0);
        knot_metrics(&d, None, 161, 161, 80.3, 79.8, f, unit.snr / snr)
    };
    let faint = at_snr(4.0, 8.0);
    assert!(faint.z < DEFAULT_THRESHOLD, "{faint:?}");
    assert_eq!(knot_verdict(&faint, bounds, DEFAULT_THRESHOLD, false), KnotVerdict::Structure);
    assert_eq!(knot_verdict(&faint, bounds, DEFAULT_THRESHOLD, true), KnotVerdict::Star);
    assert_eq!(knot_verdict(&at_snr(3.0, 40.0), bounds, DEFAULT_THRESHOLD, false), KnotVerdict::Extended);
}

/// The propagated σ_f is the filtered noise wherever the plane holds no structure (D2: 1.00–1.04). Controller
/// arithmetic: a 3×3 binomial smoothing gives the 3-px kernel a gain Σ(k∗b)²/(Σk²·Σb²) = 2.323² (simulated 2.337).
#[test]
fn propagated_noise_is_the_filtered_noise_on_white_and_correlated_noise() {
    let (w, h, sigma) = (512usize, 512usize, 0.002f32);
    let mut white = vec![0.05f32; w * h];
    add_noise(&mut white, sigma, 11);
    let tap = [1.0f32, 2.0, 1.0];
    let smooth: Vec<f32> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            let (mut s, mut k) = (0.0f32, 0.0f32);
            for dy in -1..=1i64 {
                for dx in -1..=1i64 {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64 {
                        let t = tap[(dx + 1) as usize] * tap[(dy + 1) as usize];
                        s += t * white[yy as usize * w + xx as usize];
                        k += t;
                    }
                }
            }
            s / k
        })
        .collect();
    for (plane, gain) in [(&white, 1.0), (&smooth, 2.33)] {
        let n = seeds::detection_noise(&working(plane), w, h, None, 3.0);
        assert!((n.median() / n.measured - 1.0).abs() < 0.04, "propagated {} vs measured {}", n.median(), n.measured);
        assert!((n.gain / gain - 1.0).abs() < 0.10, "gain {} vs {gain}", n.gain);
    }
}

/// Smooth structure the 128-px mesh cannot follow (2σ·sin·sin, period 40 px) reaches the measured σ_f (the filter
/// integrates it) but neither the propagated σ_f nor the gain. Controller arithmetic (numpy, 3-px kernel): measured
/// 4.73 σ, the mesh's per-cell clipped σ 1.45 σ.
#[test]
fn smooth_structure_reaches_the_measured_noise_but_not_the_propagated_one() {
    let (w, h, sigma) = (512usize, 512usize, 0.002f32);
    let tau = std::f64::consts::TAU;
    let mut data: Vec<f32> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f64, (i / w) as f64);
            0.05 + (2.0 * sigma as f64 * (tau * x / 40.0).sin() * (tau * y / 40.0).sin()) as f32
        })
        .collect();
    add_noise(&mut data, sigma, 5);
    let n = seeds::detection_noise(&working(&data), w, h, None, 3.0);
    let px = sigma as f64 * ADU_SCALE as f64;
    assert!(n.measured > 3.0 * px, "measured {} vs σ {px}", n.measured);
    assert!(n.median() > 1.2 * px && n.median() < 1.7 * px, "propagated {} vs σ {px}", n.median());
    assert!((0.95..1.10).contains(&n.gain), "the gain is the noise's, not the sky's: {}", n.gain);
}

/// The 150-star clean fixture: every star still seeded, nothing dropped or extended, σ_f propagated = measured.
#[test]
fn propagated_detection_noise_equals_the_measured_one_on_a_clean_field() {
    let (data, w, h) = fixture_field(7, 1.0, 0.002);
    let scaled = working(&data);
    let (noise, _) = domain::noise_model(&scaled, w, h, None);
    let (seeds, _, rep) = seeds::detect(&scaled, w, h, &noise, DEFAULT_THRESHOLD, 24576, None, None);
    assert_eq!(seeds.len(), 150);
    assert!((rep.sigma_f / rep.sigma_f_measured - 1.0).abs() < 0.04, "{rep:?}");
    assert_eq!(rep.structure_dropped, 0);
    assert!(rep.extended.iter().all(|&e| !e), "{rep:?}");
}

// mod.rs tests
/// Ruling A4 (D2: on the trailed L 0010 all 64 sample seeds were trail segments the fit refuses): the β sample takes
/// only seeds whose verdict fit stays centred. RED today: the 25 bright seeds, each 1.8 px off its β-1.8 star (over
/// the 1.5-px centre tolerance), join the sample and pull the frame β to ≈ 1.8. Premise check first: if HEAD already
/// reads β within 0.3 of 4, this is a pin — STOP with the numbers.
#[test]
fn beta_sample_takes_only_seeds_whose_verdict_fit_stays_centred() {
    use crate::test_support::{add_noise, integrated_field, IntegratedProfile};
    let (w, h) = (480usize, 400usize);
    let at = |i: usize| (40.0 + (i % 8) as f64 * 57.0, 40.0 + (i / 8) as f64 * 80.0);
    let bright: Vec<(f64, f64, f64)> = (0..25).map(|i| { let (x, y) = at(i); (x, y, 0.4) }).collect();
    let faint: Vec<(f64, f64, f64)> = (25..40).map(|i| { let (x, y) = at(i); (x, y, 0.2) }).collect();
    let a = integrated_field(w, h, &bright, IntegratedProfile::Moffat { fwhm: 5.0, beta: 1.8 }, 5, 0.05);
    let b = integrated_field(w, h, &faint, IntegratedProfile::Moffat { fwhm: 5.0, beta: 4.0 }, 5, 0.0);
    let mut data: Vec<f32> = a.iter().zip(&b).map(|(p, q)| p + q).collect();
    add_noise(&mut data, 0.002, 21);
    let seeds: Vec<Seed> = bright.iter().map(|&(x, y, p)| Seed { x: x + 1.8, y, peak: p, flux: 2.0 })
        .chain(faint.iter().map(|&(x, y, p)| Seed { x, y, peak: p, flux: 1.0 }))
        .collect();
    let m = measure_plane(&data, w, h, &MeasureOptions { seeds: SeedSpec::Given(seeds), diagnostics: true, ..Default::default() }, None);
    assert!((m.beta - 4.0).abs() < 0.3, "frame β {} from the 15 centred stars", m.beta);
    assert!(m.diagnostics[..25].iter().all(|d| d.sample_beta.is_none()), "no displaced seed in the sample");
    assert!(m.diagnostics[25..].iter().filter(|d| d.sample_beta.is_some()).count() >= 8);
}

/// Ruling P3 (D2's full proposal): an `extended` seed stays a seed and is fitted, but leaves `c`'s qualifiers and the
/// frame FWHM/ecc statistic as it leaves the β sample — the saturation flag's treatment. A Gaussian blob of FWHM 3F
/// (F = 4.35 px, `grid30(5.0)` at β 4) and peak 0.05 (25σ; zero-sum ratio ≈ 1.48, the extended replica above) sits
/// ≈ 50 px from its four neighbours (outside `c`'s 6F isolation). RED today: the blob's accepted fit is a 31st
/// qualifier and pulls the frame FWHM. Premise check first: if HEAD refuses the blob's fit (no 31st qualifier, frame
/// FWHM already within 1 %), this is a pin — STOP with the numbers.
#[test]
fn an_extended_seed_leaves_cs_qualifiers_and_the_frame_statistic() {
    use crate::test_support::{add_noise, gaussian_field};
    let stars = grid30(5.0);
    let plane = |blob: bool| {
        let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
        if blob {
            let f = 2.0 * 5.0 * (2f64.powf(0.25) - 1.0).sqrt();
            let g = gaussian_field(480, 400, &[(95.3, 94.8, 0.05)], 3.0 * f / 2.354_820_045, 0.0);
            for (d, b) in data.iter_mut().zip(g) { *d += b; }
        }
        add_noise(&mut data, 0.002, 35);
        let opts = MeasureOptions { seeds: SeedSpec::MatchedFilter { threshold: DEFAULT_THRESHOLD }, psf_model: PsfModel::Fixed(4.0), ..Default::default() };
        measure_plane(&data, 480, 400, &opts, None)
    };
    let (base, m) = (plane(false), plane(true));
    assert_eq!(base.aperture_stars, 30, "premise: the 30 stars qualify without the blob");
    assert_eq!(m.aperture_stars, 30, "the extended blob is not a qualifier");
    let (fb, fm) = (base.frame.fwhm_px.unwrap(), m.frame.fwhm_px.unwrap());
    assert!((fm / fb - 1.0).abs() < 0.01, "frame FWHM {fm} vs {fb} without the blob");
    let blob = m.stars.iter().find(|s| (s.x - 95.3).abs() < 2.0 && (s.y - 94.8).abs() < 2.0).expect("the blob is seeded and fitted");
    assert!(blob.flags.contains(StarFlags::EXTENDED), "{:?}", blob.flags);
}
```

- [ ] **Step 1b: One mesh per plane (ruling Q4; a refactor with no behaviour change).** The mesh map is computed ONCE
  per plane and shared: today `domain.rs:93` (`background_residual`, N*) and `seeds.rs:487` (the matched filter's
  background, now also σ_f's per-cell noise map) call `estimate_background_mesh` on the same plane at the same cell
  size, twice. A shared `domain::background_mesh` result is passed to both; consumers follow the code: N* (through
  `domain::background_residual`) and the matched filter / σ_f / the knot metrics (through the filtered plane); the
  saturation test reads `SkyLevel.median` (the stratified sample), NOT the mesh. A test pins the refactor: seeds and N*
  bit-identical to the pre-refactor head on the v2 quick manifest and the unit fixtures (a moved bit is a STOP, not
  a re-pin). The two calls are masked differently today (seeds.rs masks the plane first): the phase says whether the
  shared map is the masked or the unmasked one and proves the bit-identity either way before any σ_f code lands.
  **As built (ruling D-1, 2026-10-12):** the shared map is the masked one; on a MASKED plane `N*` therefore reads the masked
  mesh (an absent pixel is not sky; no production caller passes a mask today), a recorded change from the pre-Q4 value,
  pinned by `on_a_masked_plane_n_star_reads_the_masked_mesh`. Unmasked planes are bit-identical.
- [ ] **Step 2: Run, fail, implement, run** (`cargo test -p athenaeum-core --lib -- star_measure --test-threads=4`, then
  core `--all-targets`, `--no-default-features`). Start from D2's prototype (Files: Reuse) — port its metrics, bounds
  and gain read, then make the tests pass. The probe's frame and star lines gain A1's fields.
- [ ] **Step 3: Proof table** (release `star_probe`, base `t10-scratch/star_probe.base`-style build at fee44d50, 3
  interleaved runs): LDN_1272 mono + OSC `_0000_d`, hc11B L 0000 / 0010 / 0011 (the ERP replica set
  `~/.athenaeum-acc/star-lab-a/t10b/erp-l0010/`), M42 mono2 0000 / 0008 / 0010 — seeds, `structureDropped`,
  `extendedSeeds` (and extended in the β sample), `sigmaF` / `sigmaFMeasured` / `noiseGain`, fits, β, F_field, `c` +
  `apertureStars`, Measure wall. Expected from D2 (not a bar): M42 seeds and injected found up, L 0010's sample no longer
  trail segments. WIP commit.

#### Phase A2 — the field width (ruling A5; designed here, judged in C)

Checklist: F_field is read by the second pass's kernel, the width-start box ⌈2·F_field⌉ (PA-12) and so every region,
the donut test's box, `c`'s isolation 6·F_field, the knot bounds and the knot kernels (A1), and `field_width_of` for
given seeds (LN, Register); a change moves F-1's rows on every family, so they are re-read in C. D2: 8.2–8.8 px on M42
vs a fitted 1.7–2.4, 10.7 vs 5.6–7.0 on hc11B L 0000 / 0008 (saturated stars and the brightest candidates);
at the fitted F, M42 finds 57 / 62 / 60 injected stars.

- [ ] **Step 1: Design on numbers, not bars.** The phase may probe the inject frames and the `sharp/*`, `saturated/*`
  and `background/*` families as a design run (named `t10b-a2-design`, never a bar read), then picks the rule.
- [ ] **Step 2: The truth tests the phase writes in full before the code** (`seeds.rs`): (a)
  `field_width_reads_the_stars_width_when_the_brightest_hundred_are_saturated` (100 clipped stars among 300 of FWHM 2.4:
  F_field within 10 % of the unsaturated stars' width); (b) `field_width_reads_the_stars_width_on_structured_sky`
  (compact sky bumps among the brightest candidates: within 10 %); (c) `field_width_of_given_seeds_follows_the_same_rule`
  (I-4); PA-17's 10 % test and PA-18's border geometry kept unedited. A test the old rule passes is a pin (F28).
- [ ] **Step 3: Implement, run, proof table** as A1 step 3 plus F_field / frame FWHM per frame; PA-17's ruling text
  updated with the rule and the numbers it was chosen on. WIP commit.

#### Phase A3 — one aperture-scale sky and noise per star; `c` reads B_i; the LN ring switch (rulings B1–B4)

Checklist: B_i's inputs are the frame FWHM (F-4's statistic over unsaturated fits), the seed set (A1/A2) and the
mask / footprint; its readers are `c` now and Task 11's SNR / HFR later; `annulus_sigma` stays only as a test-backed
function with no SNR reader (B4); `c` must not move — this phase is a refactor for `c`, and a moved `c` is a STOP.

- [ ] **Step 1: Write the failing tests** (`mod.rs` / `photometry.rs` tests; `grid30`, `seeds_of`, `median` are the
  module's test helpers)

```rust
/// Rulings B2, B4: every accepted fit carries B_i (R6's ring) and σ_i. Flat sky 0.05, noise 0.002, β 4, FWHM 4.35:
/// the ring 43.5–52.2 px holds ≈ 2615 pixels; B_i within 4 SE of the sky, σ_i within 6 % (4 SE) of the noise.
#[test]
fn every_accepted_fit_carries_its_ring_sky_and_noise() {
    let stars = grid30(5.0);
    let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
    add_noise(&mut data, 0.002, 31);
    let m = measure_plane(&data, 480, 400, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), psf_model: PsfModel::Fixed(4.0), ..Default::default() }, None);
    assert_eq!(m.stars.len(), 30);
    let (sky, noise) = (0.05 * ADU_SCALE as f64, 0.002 * ADU_SCALE as f64);
    let f = m.frame.fwhm_px.unwrap();
    let full = std::f64::consts::PI * ((12.0 * f).powi(2) - (10.0 * f).powi(2));
    for s in &m.stars {
        let n = s.sky_ring_n as f64;
        assert!(n > 0.97 * full && n < 1.03 * full, "{n} of {full}");
        assert!((s.sky_ring - sky).abs() < 4.0 * noise / n.sqrt(), "B_i {} vs {sky}", s.sky_ring);
        assert!((s.sky_ring_sigma / noise - 1.0).abs() < 0.06, "σ_i {} vs {noise}", s.sky_ring_sigma);
        assert_eq!(s.sky_offset(), s.background - s.sky_ring);
    }
    assert_eq!(m.ring_sky_missing, 0);
}

/// Ruling B2: `c` reads each qualifier's stored B_i — recomputed here from the stored fields and the plane alone, it is
/// the plane's `c`. The replica's growth rule: every pixel whose CENTRE lies within 5F of the fit's centre, the value
/// in f32 working units (`v * ADU_SCALE`, the module's conversion) widened to f64, minus B_i, summed in row-major
/// order. The tolerance 1e-6 covers a different summation order; if the product's rule differs in the disc or the
/// units, the replica copies the product's rule — the tolerance is never widened.
#[test]
fn aperture_correction_is_the_growth_over_the_stored_ring_sky() {
    let stars = grid30(5.0);
    let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
    add_noise(&mut data, 0.002, 32);
    let m = measure_plane(&data, 480, 400, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), psf_model: PsfModel::Fixed(4.0), ..Default::default() }, None);
    assert_eq!(m.aperture_stars, 30);
    let (f, w) = (m.frame.fwhm_px.unwrap(), 480usize);
    let r2 = (5.0 * f).powi(2);
    let c = median(m.stars.iter().map(|s| {
        let growth: f64 = (0..400 * w)
            .filter(|&i| ((i % w) as f64 - s.x).powi(2) + ((i / w) as f64 - s.y).powi(2) <= r2)
            .map(|i| (data[i] * ADU_SCALE) as f64 - s.sky_ring)
            .sum();
        growth / model_flux_in_fwtm(s)
    }));
    assert!((c / m.aperture_correction.unwrap() - 1.0).abs() < 1e-6, "{c} vs {:?}", m.aperture_correction);
}

/// B_i is every accepted star's (its present ring pixels); `c`'s qualifier rule stays `c`'s: a star whose 12F disc
/// leaves the plane has a B_i over its partial ring and is not a qualifier.
#[test]
fn an_edge_star_has_a_ring_sky_but_does_not_qualify_for_c() {
    let mut stars = grid30(5.0);
    stars.push(MoffatStar { x: 20.0, y: 25.0, ..stars[0] });
    let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
    add_noise(&mut data, 0.002, 33);
    let m = measure_plane(&data, 480, 400, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), psf_model: PsfModel::Fixed(4.0), ..Default::default() }, None);
    assert_eq!(m.stars.len(), 31);
    let edge = m.stars.iter().find(|s| (s.x - 20.0).abs() < 1.0 && (s.y - 25.0).abs() < 1.0).unwrap();
    let f = m.frame.fwhm_px.unwrap();
    let full = std::f64::consts::PI * ((12.0 * f).powi(2) - (10.0 * f).powi(2));
    let n = edge.sky_ring_n as f64;
    assert!(n >= 8.0 && n < 0.75 * full, "{n} of {full}");
    assert!((edge.sky_ring - 0.05 * ADU_SCALE as f64).abs() < 4.0 * 0.002 * ADU_SCALE as f64 / n.sqrt());
    assert_eq!(m.aperture_stars, 30, "the edge star is not a qualifier");
}

/// Ruling B3's arm: a given ring width sets every ring (and `c`'s growth radius); at F 4 the ring 40–48 px holds
/// ≈ 2212 pixels, while the plane's own F 2.61 gives ≈ 940.
#[test]
fn a_given_ring_width_sets_every_ring() {
    let stars = grid30(3.0);
    let mut unit = moffat_field(480, 400, &stars, 4.0, 0.05);
    add_noise(&mut unit, 0.002, 34);
    let data: Vec<f32> = unit.iter().map(|v| v * ADU_SCALE).collect();
    let own = fit_stars_at(&data, 480, 400, &seeds_of(&stars), 4.0, &FitStarsOptions::default(), None);
    let common = fit_stars_at(&data, 480, 400, &seeds_of(&stars), 4.0, &FitStarsOptions { ring_fwhm: Some(4.0), ..Default::default() }, None);
    let ring = std::f64::consts::PI * (48.0f64.powi(2) - 40.0f64.powi(2));
    assert!(common.fits.iter().all(|f| (f.sky_ring_n as f64 / ring - 1.0).abs() < 0.03));
    assert!(own.fits.iter().all(|f| (f.sky_ring_n as f64) < 0.5 * ring));
    assert!(own.aperture_correction.is_some() && common.aperture_correction.is_some());
}
```

- [ ] **Step 2: Run, fail, implement, run** (as A1). LN: `RingGeometry` with both arms, `PerSide` default;
  `ln_scale_probe --ln-ring` prints which arm ran.
- [ ] **Step 3: The `c` gate.** `star_probe` at A2's head vs this head over v2's quick manifest + the inject frames:
  every plane's `apertureCorrection` and `apertureStars` byte-identical (B_i on a qualifier is R6's b by construction);
  a moved `c` is a STOP with the first plane and its mechanism. Proof table: per frame median σ_i / σ_MRS, median and
  p90 of `skyOffset` / σ_i, `ringSkyMissing`. WIP commit.

#### Phase A4 — the trailed verdict from the refusal structure (ruling A6, its addendum, and Q1–Q2)

Checklist (checked first, at fee44d50): the measure path has NO trail verdict (`trail` greps to 0 over `star_measure/`,
`stacking/measure.rs`, `psf_signal.rs`, `weights.rs`); the stacking selection has no eccentricity, FWHM or star-count
gate; the program's existing trail definitions are the plate solver's point-like line and share rule (`looks_trailed`)
and the Analysis-based plate-solve input gate (median ecc AND trail R², Plan C) — NONE is reused as a per-star
refusal (Q1: the line refuses real, elongated stars; `RefusalReason::Streak` does not exist). The signal is the
refusal structure the fits already produce: a trail longer than its region cannot be contained, so its seeds end
`centreMoved` / `regionTooSmall`. Readers of the verdict: the frame's `stars` (empty), `c` (None), the weights (PSF
signal weight → 0 of the group), `stars_refused` by reason. Before the code, render and label the 7 wind-shaken
`trails/` frames (the owner's lesson: look at the outlier frame first); 10b-L labels the inject frames and L 0011
(classes per R1/Q2: `trailed` / `elongated` / clean).

- [ ] **Step 1: Write the failing tests** (`mod.rs` tests). Fixtures: `trail`, `fixture` as below (trails of 20 px,
  round Moffat stars; the same builders as before), the assertions re-expressed for the frame-level verdict:

```rust
/// 120 trails and 5 round stars: 125 judged seeds, the trails' fits end centreMoved / regionTooSmall / centreAtRim above the
/// derived share → the plane is trailed, keeps no star, `c` is None. No `Streak` reason exists.
#[test]
fn a_plane_of_trails_is_trailed_by_its_refusal_share_and_keeps_no_star() {
    let (data, seeds) = trail_plane_fixture(540, 420, 5, 120, 42);
    let m = measure_plane(&data, 540, 420, &MeasureOptions { seeds: SeedSpec::Given(seeds), ..Default::default() }, None);
    assert!(m.trailed);
    assert!(m.trail_share >= fit::TRAIL_REFUSAL_SHARE, "{}", m.trail_share);
    assert!(m.stars.is_empty());
    assert_eq!(m.refused.by_reason.get(&RefusalReason::Trailed).copied().unwrap_or(0) > 0, true);
    assert_eq!(m.aperture_correction, None);
}

/// Under 100 judged seeds there is no verdict, however many fits end centreMoved / regionTooSmall / centreAtRim.
#[test]
fn fewer_than_the_minimum_judged_seeds_is_never_trailed() {
    let (data, seeds) = trail_plane_fixture(420, 310, 5, 15, 41);
    let m = measure_plane(&data, 420, 310, &MeasureOptions { seeds: SeedSpec::Given(seeds), ..Default::default() }, None);
    assert!(!m.trailed);
}

/// Elongated stars (eccentricity 0.7, the lab's most elongated `shape` family, and a G 0003-like 0.65 field) are
/// stars: no verdict, none refused for their shape, all 120 kept.
#[test]
fn elongated_stars_are_measured_as_is_with_no_verdict() {
    let (ax, ay) = (2.6 / 0.714 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt()), 2.6 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt()));
    let stars: Vec<MoffatStar> = (0..120).map(|i| MoffatStar { x: 30.0 + 40.0 * (i % 12) as f64, y: 30.0 + 40.0 * (i / 12) as f64, amp: 0.2, alpha_x: ax, alpha_y: ay, theta: 0.5 }).collect();
    let mut data = moffat_field(500, 420, &stars, 4.0, 0.05);
    add_noise(&mut data, 0.002, 43);
    let m = measure_plane(&data, 500, 420, &MeasureOptions { seeds: SeedSpec::Given(seeds_of(&stars)), ..Default::default() }, None);
    assert!(!m.trailed);
    assert_eq!(m.stars.len(), 120);
}
```

  (`trail_plane_fixture(w, h, round, trails, seed)` is `fixture` laid out on the 13-column grid of the previous
  version of this test; the phase writes it in full.) No test names a `Streak` reason or the solver's eccentricity
  line; the `solver`-feature pin of `MAX_ECCENTRICITY` is dropped with the line.

- [ ] **Step 2a: The derived line, before any code (a STOP rule).** The share of seeds refused `centreMoved` +
  `regionTooSmall` is computed from existing outputs — D2's `t10-scratch/d2/out/base3` (the same product code as
  t10-f5-r2; the `~/.athenaeum-acc/star-lab-inject/probe` dirs carry no refusal records and add nothing) — on every
  clean real and lab frame, and the line is median + 5·1.4826·MAD of that distribution, required to separate L 0010
  / L 0011 (verdict) from G 0003 and every clean frame (no verdict). Computed 2026-10-11 on the dev Mac (measure
  path, share = refused-of-those-two-kinds / seeds):

  | frames / families | share |
  | ---- | ---- |
  | L 0010 (`trailed`) | 21 939 / 24 576 = 0.893 |
  | lab `trails` (a `trailed` family) | 116 / 317 = 0.366 |
  | G 0003 (`elongated`) | 829 / 2129 = 0.389 |
  | clean real: LDN 1272 0077 / 0099 / 0127 | 0.024 / 0.041 / 0.054 |
  | clean real: hc11B G 0001 / 0002, L 0000 / 0008 | 0.074 / 0.138, 0.083 / 0.077 |
  | clean real: M42 mono2 0000 / 0008 / 0010 | 0.058 / 0.064 / 0.071 |
  | clean lab: `doubles/f4.0` / `/f2.6`, `halo`, `density/d1000` | 0.151 / 0.093, 0.047, 0.011 |
  | clean lab: every other family (≈ 45 cells; `sharp/*`, `shape/*`, `coma`, `fieldpsf`, `background/*`, …) | 0.000–0.006 |

  Result: over every clean frame (59) median = MAD = 0, so the line is 0.000 (degenerate: the lab families are
  almost all zero); over the 10 clean real frames alone it is 0.067 + 5·1.4826·0.0116 = 0.153. NEITHER separates the
  verdicts from G 0003 (0.389 > 0.153), and the lab `trails` family (0.366) sits below G 0003, so no derived line puts
  the lab trails over G 0003 either. The derivation does not separate as written: A4 STOPS here with this table to the
  controller before any code (the gap 0.389–0.893 exists for the real frames, but a line inside it is chosen, not
  derived; L 0011 is not in the table — its share is the first row A4 adds). The options for the controller, with the
  numbers: a second signal for the verdict (the refusal structure's `regionTooSmall` alone, or the share among seeds
  of SNR ≥ 10, each computed on the same outputs), a derived line over the REAL trailed vs elongated frames with the
  n = 1 caveat stated, or the verdict reported without a gate. ecc is never the gate (Q1).

  **Controller ruling Q5 (2026-10-11).** The lab `trails` family is seven wind-shaken frames whose stars are elongated
  but measurable: it is class `elongated`, like G 0003, not `trailed`. The verdict line is the MAJORITY rule, share
  > 0.5: most of the frame's detected sources cannot be contained in a fit region. That is a physical criterion, not a
  number fitted to one frame. On the table above it gives no verdict on every clean frame (≤ 0.151) and on both
  elongated cases (0.366, 0.389), and a verdict on L 0010 (0.893). A4 adds L 0011 as its first row: L 0011 at or below
  0.5 → STOP with the table. The rule rests on n = 1 real trailed frame, so phase L2 adds a synthetic long-trail family
  on the runner: streak lengths from 2F to 300F at three PSF widths, with the share measured against streak length. The
  crossing of 0.5 is expected near a streak length of one fit-region diameter. It is reported in C; it is not a bar.
  `fit::TRAIL_REFUSAL_SHARE = 0.5`, documented with this rationale.
- [ ] **Step 2b: Run, fail, implement, run** once step 2a is ruled. The verdict reads the share of the verdict fits'
  refusals (or, if the controller rules a pre-fit signal, the signal's own computation); whether it is decided
  before the verdict fits (saving L 0010's 24 576 fits) is the phase's choice; the F-0 identity holds either way (a
  seed the verdict pre-empts is refused `Trailed`).
- [ ] **Step 3: Proof table** on the 12 hc11B L frames of 2024-10-06 (the ERP replica set) and the 7 `trails/` frames:
  class, seeds, refusal share, fits, `trailed`, PSF signal weight, `c` + `apertureStars`, with the ERP's numbers and
  the plate solver's own verdict beside, and A3 head's fits per frame beside A4's (T.4 reads this phase's own delta);
  G 0003's share and ecc (T.8). On the dev Mac also (R3: LDN 1272 is not on the runner): LDN 1272's `c`
  frame-to-frame sd over its calibrated frames at A4's head vs fee44d50 (RF7's 2.02 %; reported, with M.2). WIP commit.

#### Phase L — 10b-L, the lab families (runner; done 2026-10-11, lab f95cd1a)

Checklist: each new family's truth sanity line before it referees; inject frames labelled by rendered class before
any of them judges `c`, β or a background (a trailed frame is never a referee).

Handoff `/Volumes/Universe/athenaeum-runner/handoffs/task-10b-L-handoff.md`: (a) psfSel pairs on nebula sky, (b)
B-spline-warped targets, (c) injection with the frame's own PSF, (d) a knots family, (e) density × nebula, (f) real-sky
stability rows; truth sanity per family, an ERP column for the new synthetic families, and one proof run at fee44d50 —
the "before" every new row of phase C is judged against. Phase C also needs the frames labelled by class, R1's three:
`trailed` (L 0010, L 0011), `elongated` (G 0003 — 10b-L's conservative "trailed" label becomes `elongated`), clean;
the 7 `trails/` frames and every inject frame — the label tooling is L2's. Done (lab f95cd1a, report
`handoffs/task-10b-L-report.md`): every family built; LDN 1272 NOT built (no frame on the runner: its rows run on the
dev Mac, reported, R3); L 0011 not among the 21 labelled frames (L2). A family that is not built is reported as not
built; its rows below turn `reported`, never half a referee.

#### Phase L2 — lab tooling (runner, after A4's interfaces are fixed; review finding 12)

Checklist: C's agent runs one command per step and is never blocked on tooling; Task 10's lab-cycle-tool brief is the
precedent (one tooling agent, its own report, a suite run). Nothing here reads a bar.

- [ ] **Step 1: The scorecard and the probe readers learn A1–A4's surface:** the reason `trailed`
  (`RefusalReason::ALL` 13 → 14 breaks the scorecard's reason enumeration — the `fit outcomes` identity must count
  them); the `extended` flag (a star-line flag, kept out of the β-sample and `c`-qualifier readers as `saturated`
  is); the frame keys A1–A4 add to `star_probe` (`sigmaF`, `sigmaFMeasured`, `noiseGain`, `structureDropped`,
  `extendedSeeds`, `trailed`, `trailShare`, and whatever the phases name) and the star keys (`skyRing`, `skyRingSigma`,
  `skyRingN`, `skyOffset`, flag `extended`).
- [ ] **Step 2: The rows C reads that no tool prints yet:** K.3's dropped / extended split by (size, amplitude) bin
  with K.1's per-bin expected pass rate beside (the knot tests' arithmetic, D2's `knots.py` replica); a verdict
  column for the T rows (frame, class, verdict, refusal share, the solver's verdict); W.1's injected-above-threshold
  count per frame (the raw-mode truth rule's computation); I.1's per-frame Δ against 10b-L's before with SE_c; P.1's
  Δ against the flat control with the < 20-matched n/a rule; S.1's sd with n and the derived factor.
- [ ] **Step 3: The driver and the data:** the `--ln-ring per-side|pair-common` arm in the LN driver (one probe run
  per arm); the 7 `trails/` frames and the 12 hc11B L replica frames on the runner with their classes; L 0011
  rendered and labelled; `star-lab/frame-classes.json` gains `elongated` (G 0003) and L 0011.
- [ ] **Step 4: Suite + one dry run** of every new reader on t10-f5-r2's outputs (byte-identical rows where the
  surface is unchanged); lab commit and push; a short report.

#### Phase C — one joint lab cycle `t10b-onebg` (runner, judgment run)

Checklist: every upstream quantity is re-read in this one cycle — seeds (D, K, N), the field width (W), the fits
(F.0–F.7, D.8), the frame β (F-3, F-4; the M42 β reported, S.3), B_i and σ_i (B), `c` (C1, I.1, S.1, T.3), trails
(T); a row that moves outside the design list is a STOP, never a design-list edit after the run. Frame classes (R1):
the `elongated` G 0003 and the `trailed` L 0010 / L 0011 sit in no clean pooled row (W.2, I.1, S.1, S.2, B); every
bar below is derived from data or from the PSF and the noise before the run, or reported with its reason (R2).

- [x] **Step 0: Runner baselines, before the run (ruling R3).** At t10-f5-r2's binary (fee44d50's product code), one
  job at a time on an otherwise idle runner: (a) the m42 corpus `measure` batch wall — M.1's base (the dev Mac's
  223.1 s is printed beside, never compared); (b) `c`'s frame-to-frame sd per dataset over its clean frames (M42's 12
  calibrated frames, hc11B L, G, the M42 inject frames) — S.1's base, 10b-L (f)'s values beside. Written to
  `results/plan-a/t10b-onebg/baseline-runner.txt` and committed before step 2. LDN 1272 is not on the runner: its rows
  are the dev Mac's (A4's proof table, M.2), reported.
- [x] **Step 1: Bars and design list first.** `results/plan-a/t10b-onebg/{bars.py,design.json}` encode the table below;
  run against t10-f5-r2's outputs, 10b-L's before and step 0 → `bars-baseline.txt` (C1.1–C1.4 pass on t10-f5-r2; every
  new row prints its before). D2 measured recall with its own matcher: where its HEAD number and the scorecard's
  t10-f5-r2 cell differ by more than 0.01 the controller reconciles BEFORE step 2 (a matcher difference, never a bar
  move); D.1's floors are then computed at the scorecard cell's n (D2's arm − 2 SE), and K.1's per-bin expected pass
  rate from A1's bounds by the knot tests' own arithmetic (D2's `knots.py` replica, each bin's size and SNR) — both
  printed in `bars-baseline.txt` before step 2.
  Design list: row 1 (`completeness`, `false detections`, `bright coverage`) on `seeds`, `measure`, `betas@*` set aside
  (judged below; `fit outcomes` stays the F-0 gate); rows 3–7 and 11 on `measure` and `betas@*` set aside on
  `background/*`, `density*`, `doubles/*`, `saturated/*` and the 10b-L families, one-sided everywhere else (row 2's keys
  per F10, row 3 `kernel growth` `["scatter"]`); row 6 `captured fraction` set aside (C1.1); register row 1 reported,
  row 2 one-sided (F10); row 10 judged by F-3's bar; LN: the C1.2–C1.4 cells and P.1/P.2 judged, every other arm
  reported (PA-14); the legacy control 0 mismatches. Anything else that moves → STOP with the first ≤ 10 rows and a
  mechanism.
- [x] **Step 2: Run** at A4's head, one job at a time: `python3 bin/lab_cycle.py --step t10b-onebg --prev t10-f5-r2
  --base-probe-dir <t10-f5-r2's probe dir> --inject --bars results/plan-a/t10b-onebg/bars.py --design
  results/plan-a/t10b-onebg/design.json --jobs <n>`, plus the 10b-L manifests, the LN probe once per `--ln-ring` arm,
  the 12 hc11B L replica frames, the 7 `trails/` frames (classes from L2), and the m42 corpus `measure` batch (M.1).
- [x] **Step 3: Read the bars.** A miss is a STOP with ≤ 10 lines of evidence, never a re-run with changed flags. B3's
  rule (P.3) is applied as written. K7's predicted rows (D1's dumps) are printed beside R6's, no switch. A miss the
  table already predicts (P.1 on `neb0.5`) is reported with its numbers like any other.

| # | Cell | Bar | Baseline (source) |
| ---- | ---- | ---- | ---- |
| C1.1–C1.4 | Task 10's C1 cells | verbatim | 0.76 / 1.38 %; +1.72 / +1.19 %; 0 of 70; 0 of 20 (t10-f5-r2, lab 39bb670) |
| F.0–F.7 | F-0 `fit outcomes`; F-1 fitted/seeded at `sharp/m4/f8.0` and frame FWHM ≤ 1 % to F 16, donut accepted 0; F-2 axis order, angle eff ≤ 1.5; F-3 twins ≤ 1 % / Δβ ≤ 10 %, row 10 FN ≤ 0.5 %; F-4 ≤ 1 % / ≤ 3 %; F-7 ≤ 0.3σ neb, ±0.1σ flat; raw-mode injected ≥ 99 % | the spec's bars | all PASS at t09-f4 / t10-f5-r2 (angle eff 1.25, FN 0.098 %, `neb3_4096` −0.253σ) |
| D.1 | row 1 `seeds` completeness `neb0.5`, `neb3`; false / Mpx | ≥ D2's arm minus 2 SE of the recall at the cell's n truth stars (0.777 − 0.034 = 0.743, 0.539 − 0.040 = 0.499 at v2 i0's n = 629; the usual one-sided ~2.3 % false-alarm level, Q3 — 1 SE would fail ~16 % of the time on noise alone; step 1 recomputes at the scorecard cell's n); ≤ 3 | 0.591, 0.343 (D2 HEAD); the arm 0.777, 0.539 (D2) |
| D.2 | `halo` false seeds | ≤ 5 (D2's arm 2 + 2 Poisson SE) | 256 (D2) |
| D.3 | completeness and false seeds on flat, grad10, vig20, `sharp/*`, `mixture/*`, `twins/*`, artefacts, coma, fieldpsf, `shape/*`, `saturated/*` | ≥ base − 0.005; false ≤ base | t10-f5-r2 |
| D.4 | `twins/*`, artefacts `nothing` / Mpx | ≤ 0.5 (R-OSP-2) | 0.00 at K 6 (t06a) |
| D.5 | `doubles/f2.6`, `/f4.0`, `d1000`; density pairs | ≥ base − 0.01; ≥ base − 0.03 | D2 HEAD 0.700 / 0.598 / 0.726; 0.907 |
| D.6 | σ_f propagated / measured, flat and `twins/*` | 1.00 ± 0.04 (a flat-family move re-sweeps K, PA-2) | D2 1.00–1.04 |
| D.7 | `neb3_4096` completeness, false / Mpx | reported | D2 0.364 / 0 → 0.565 / 67 |
| D.8 | frame FWHM, ecc, `c` on flat, `sharp/*`, `density/*`, `twins/*` (the `extended` flag's reach, P3) | within SE of base | t10-f5-r2 |
| K.1 | knots: accepted unflagged fits on truth knots, per (FWHM/F, peak) bin of FWHM ≥ 1.5F | ≤ the knot tests' expected pass rate at the bin's size and SNR (step 1, from A1's bounds) + 2 binomial SE at the bin's n; bins < 1.5F reported (the tests are blind there); the pooled ratio to base printed | 10b-L before: flat 80 % accepted (r0.75–r3 90–100 %, r5 57 %), `neb0.5` 31 %, `neb3` 0 seeded |
| K.2 | knots: truth-star completeness | ≥ base − 0.01 | 10b-L before |
| K.3 | knots: stars dropped / extended by size and amplitude bin | reported | — |
| N.1 | density × nebula: completeness; false / Mpx; `c` qualifiers | ≥ base − 0.03; ≤ 3; ≥ 5 per plane | 10b-L before |
| W.1 | M42 mono2 0000 / 0008 / 0010 injected found | per frame ≥ 99 % of the injected stars above the detection threshold at the frame's propagated σ_f (the raw-mode truth rule, F.0–F.7's computation); the plain found / injected reported | 42/60, 43/62, 38/62 (D2 HEAD); 57 / 62 / 60 at the fitted F |
| W.2 | injected found, G 0001 / 0002, L 0000 / 0008 (clean; the `elongated` G 0003 is T.8's) | ≥ base; no trailed verdict on any of them (T.4/T.6), so A4 costs none | 60/61, 61/61, 51/60, 49/61 (RB1) |
| W.3 | F_field / frame FWHM per frame and family; sample share flagged extended | reported | D2: M42 8.2–8.8 vs 1.7–2.4 |
| T.1 | L 0010, L 0011 accepted fits | ≤ 1 % of the clean median (≤ 24) | 947, 786 vs 2458 (ERP report) |
| T.2 | L 0010, L 0011 PSF signal weight | ≤ 0.01 × clean median (≤ 0.00182) | 0.0053, 0.0082 vs 0.182 |
| T.3 | L 0010, L 0011 `c` | None (< 5 qualifiers) | 4.48 (29), 6.89 (25) |
| T.4 | the 10 clean L siblings: fits; verdict | no verdict (the refusal share below the derived line, A4 step 2a); fits: the A4 phase's own delta (A4 head vs A3 head, A4's proof table) within √base — the verdict never fires, so A4 changes no clean fit; the A1 move (D2: G 0002 2738 → 2395, L 0008 2241 → 2597) is read by D.3 / D.8 and the proof tables, not here | 2068–2629 per frame (ERP report) |
| T.5 | the 7 `trails/` frames: verdict | = the rendered class (the refusal share against the derived line); the solver's verdict printed | A4 labels |
| T.6 | every other frame (G 0003 included, R1/Q2) and v2 family: verdict; `shape/*` (e ≤ 0.7), coma, fieldpsf `measure` completeness | no verdict; ≥ base − 0.005 (no per-star refusal exists to lose a star, Q1) | t10-f5-r2 |
| T.7 | lab `trails` family: refusals by kind (centreMoved / regionTooSmall / Trailed) and trail length, the share beside | reported | share 0.366 (A4 step 2a) |
| T.8 | G 0003 (`elongated`, ecc 0.65) | reported: NO verdict (Q2), refusal share, ecc, `c`, the ERP's weight beside; out of every clean pooled row. Should the verdict fire there, that is a ruling, not a bar | 10b-L class (relabelled `elongated`, R1); HEAD 356 fits, refusal share 0.389 (A4 step 2a) |
| B.1 | σ_i / truth noise, median (p90), flat, grad10, vig20, `neb0.1`, `sharp/*`, `density/*` | median 1.00 ± 0.05 | D1 D-6: 1.00 (≤ 1.05) |
| B.2 | B_i − `sky_local` (RB1's table), b − B_i at the qualifiers | reported, R6's column and K7's predicted rows beside | RB1 5f705ad; D1 d63dfc7 |
| B.3 | row 6 `neb3`, `neb3_4096` with SE; density × `neb3` row 6 d200 / d600 / d1000 | recorded limit | 0.880 ± 5.6 %, 1.129 ± 2.3 %; 1.058 / 1.135 / 1.069 (10b-L (e)) |
| P.1 | psfSel, nebula-sky pairs (iso/m4, iso/g, iso/blur25mix × t25/t45/t60 × `neb0.5`, `neb3`), both ring arms | Δ vs the flat control on the same star field within ±1.2 % (m4, g) / ±2 % (blur25mix) on `neb0.5`, the same + 2 SE on `neb3`; a cell refused for < 20 matched stars is n/a, not a miss. The before already misses on `neb0.5` (Δ vs flat, t25 / t45 / t60: m4 +1.85 / +1.50 / +2.49, g −0.07 / +3.28 / +3.84, b25mix t60 −2.17 pp): P.3's STOP branch is the likely outcome | 10b-L before; flat control m4 +0.22 / +0.21 / +0.30, g +0.01 / +0.17 / +0.24, b25mix +0.18 / +0.68 / +1.48 % |
| P.2 | psfSel, warped targets (the same families, flat sky) | the unwarped twin's tolerance | 10b-L before |
| P.3 | B3's decision | pair-common iff P.1 passes under it AND C1.1–C1.4 pass under it; P.1 failing under both arms → STOP (no switch to K7) | — |
| I.1 | inject row 6 with the frame's own PSF, clean frames (G 0003, L 0010 out) | Δ vs 10b-L's before per clean frame ≤ 2 % + 2·SE_c (a stability row); the +4–6 % halo (the real halo `c` carries beyond the fitted model) reported, a recorded limit | 10b-L (c) per frame; pooled G 1.0465, L 1.0642, mono2 1.0439 |
| S.1 | `c` frame-to-frame sd per dataset, clean frames | M42's 12 calibrated frames: ≤ step 0's sd × (1 + 2/√(2·11)) = 1.43 × (the sd's own relative SE at n = 12 is 21 %); hc11B L, G (n = 2 each) and the 3 M42 inject frames: reported, no bar | step 0 (runner, t10-f5-r2); 10b-L (f) beside: M42 12 1.02 %, L 1.94 %, G 1.43 %, M42 inject 1.28 % |
| S.2 | seeds / Mpx per clean frame vs its dataset's median | reported (R2: [0.5, 2] × was not derivable before the cycle; the flood it guards is T.1's) | 10b-L (f): M42 269–330 (max/median 1.15), hc11B G 47–83, L 58–60 |
| S.3 | M42 mono2 frame β 0000 / 0008 / 0010 | reported, mechanism split (knot / extended seeds vs near-clip class-B stars; D2: 35 of 52 sample stars at peak ≥ 0.95 of range); the bar is Task 10c's | 13.96 / 3.14 / 3.34 (t10-f5-r2); predicted miss on 0000 (D2: near-clip class B) |
| M.1 | m42 corpus `measure` batch wall, on the runner | ≤ 1.2 × step 0's wall at t10-f5-r2's binary (R3) | step 0; the dev Mac's 223.1 s (2026-10-07) printed beside, never compared |
| M.2 | LDN_1272 mono + OSC Measure wall, 3 interleaved vs fee44d50; LDN 1272 `c` frame-to-frame sd over its calibrated frames (dev Mac, R3) | reported | A-phase proof tables; RF7 2.02 % |

- [x] **Step 4: Results** committed and pushed in the lab (`results(plan-a): t10b-onebg - F-10 bars <ok|items>`);
  a STOP commits under `results/plan-a/t10b-onebg/stop-r<n>/`.

#### Phase D — one review of Tasks 10 + 10b, then the squash

Checklist: no consumer estimates an owned quantity a second time (a sky, noise or seed estimator outside its owner is
a finding); ruling F14's test edits named; README, spec and plan records match the code.

- [x] One review (the most capable model; Fable ran it) of the WIP branch from 588c4a5f; each fix round goes to a fresh agent and re-runs only the bars
  its change touches, under a new step name. Then README R-OSP-7b (rulings A1–A6, B1–B5, P1–P5, R1–R3, the decision
  P.3, the C table), the spec's F-10 row "as built", this task's Outcome block.
- [x] Core `--all-targets` and `--no-default-features` green; the WIP branch squashed onto `one-star-path` as two
  commits — Task 10's F-5 commit, then:

```
fix(star_measure): F-10 one background - propagated detection noise, knot tests, the field width, the trailed verdict from the refusal structure, one aperture-scale sky and noise per star (recall neb0.5/neb3 <cells>; L 0010/0011 <cells>; C1 <cells>)
```

**Acceptance:** every bar of phase C met or reported to the owner with its numbers (a predicted miss included); `c`
byte-identical across A3; both diffs 0 moved outside the design list; control 0; no owned quantity estimated twice.

**Outcome (2026-10-12).** Commits (athenaeum, WIP branch `one-star-path-t10-wip`, squashed as the F-10 commit on
`one-star-path` after Task 10's F-5 commit): the planning rounds de178d0b, ebf9ad46, ec2f0ec4, 945080aa; A1 16fa8813
(propagated σ_f, knot tests, centred β sample, one mesh per plane), e016d2cf (STOP A1-1 closed), 23816f3c (waste out
of the noise read), a1114959 (the read's lag sums in row order: the same gain on every run); A2 1bc29ef8 (the field
width as the half-sample mode); A2b ca273f66 (the knot bounds at the star-width envelope, E4n); A3 dbaf3348 (B_i / σ_i
per star, `c` reads B_i, the LN ring switch); A4 c472cc78 (the trailed verdict from the refusal structure); A4b
6a1addc6 (the half-maximum-contour centre test, the rim in the share); A1b 7fd911ab (gain 1 on unmixed planes); A1c
4c95b6b4 (the caller's `PixelNoise`); D fixes 759b9d6e; then this docs commit. Lab (athenaeum-lab, runner): 10b-L
f95cd1a, L2 b5b5c40 … fecabcd (tooling, frame classes, the long-trail family, Q7's allowance), L3 2332ce9 (K.1 at the
product's widths). Phase C `t10b-onebg` in three runs: 6a1addc6 (lab 895136a; STOPs D.1 `neb3`, P.1 / P.2, row 7),
7fd911ab (lab 07cd2cb; STOP-A the resampled flood, STOP-B row 7 on the base's stars), 4c95b6b4 (lab 4f9e21e) **BAR
PASS, no STOP open**: C1.1 0.76 % / 1.41 %, C1.2 +1.72 % / +1.19 %, C1.3 0 / 70, C1.4 0 / 20; F.0 885 / 885;
no-regression 0 moved / 0 missed / 0 lost, cumulative PASS, control 0; D.1 `neb0.5` 0.777 and `neb3` 0.539 (floors
0.743 / 0.499); `halo` false seeds 2; K.1 pooled 1 / 1 / 2 of 216 (allowance 4 / 3 / 4); W.1 45 of 45, 41 of 41, 45 of
45; L 0010 / L 0011 trailed with 0 fits and `c` None; 6 of the 7 `trails/` frames match their class (H 0029 the
recorded miss); B.1 σ_i / truth within 1.00 ± 0.05; S.1 0.67 % (bar 1.46 %); M.1 74.7 s (bar 159.8 s); same-star 645
cells, 19 worse (allowance 23); P.1 / P.2 reported (C-9). Phase D: review `review-t10-t10b.md` (0 Critical / 3
Important / 10 Minor; ruling D-1, all accepted), fix round 759b9d6e (`star_probe` and LN's library outputs
byte-identical to 4c95b6b4). Owed in the squash bodies (D-1): M-6 serde_json's `float_roundtrip` named in the F-5
commit; M-13 the LN half / full bound 0.004 → 0.030 named in the F-5 commit as RF7's ruled re-pin (it landed in 0515d8b4, Task 10). Rulings index
(README R-OSP-7b, one line each): the design A1–A6, B1–B5 and the two addenda; P1–P5 and R1–R3 (the plan review);
Q1–Q7 (the plan fixes); A1-1, A2-1…A2-3, L3-1, A4-1, A4-2 (the phases); C-1…C-12d (the cycle; C-7 = A1b, C-9 = P.3
per-side, C-10 = A1c); D-1 (the review). README R-OSP-7b complete (with the cell-by-cell C table); spec §3 F-10 as
built and as judged, §2.1, the owned-quantities table and §4 follow it.

Carries. **Task 10c:** the near-clip pool (LDN mono's whole β pool sits at the full well, frame β 2.0 → 1.5, A2-2;
clamp vs post-fit reject is 10c's question with its numbers, RB3); 653 of M42 0000's 1 213 unfitted added seeds sit on
the clipped Trapezium core plateau (the saturation mask should keep the plateau out of detection); PSFSW's TFlux is ≈
80 % saturated-model flux on hc11B L 0000 (A4-2; R.2 at the third run: M42 mono2 0.79–0.90, hc11B G 0.62–0.73), so a
frame's weight hangs on a few clipped stars' model flux; the M42 frame β 31.8 / 2.81 / 3.38 (S.3) against 10c's
derived bar. **Task 11 (RB4):** the debayer's noise correlation κ (lag-1 R 0.20 / G 0.29 / B 0.30 against mono 0.004),
the colour-plane gain read's nebula-texture leak (C-7 (a)), σ_i / σ_MRS 1.24–1.58 on debayered colour planes — the
SNR's noise there. **Plan B:** LN on nebula sky — the P.1 / P.2 tables (`a1c/p1-table.txt`: per-side 13 of 34 cells
over, pair-common 15; shifted / warped t25 +1.91…+1.96 pp) are its "before"; the error lives in LN's tile sky
(`ln/background.rs`). **A later fit-design task:** C-12d — on a heavy-wing profile (β 2.5) shape at the true width
reads +0.003 ecc on bright round stars and +0.02 at SNR < 20; whether shape wants a region wider than the F-sized
windows; with it the fit-side part of G 0003's rise (`regionTooSmall` 65 → 148 at F 9.89 → 8.36, A4-2). **A later
nebula-recall owner:** the mesh noise map reads ≈ 6× the true noise on `neb3` (structure inside a cell counts as
noise), so heavy-nebula recall stays ≈ 0.54 (C-7 (b)). **Plan C:** its CFA super-pixel `detect` call states
`PixelNoise::White` (undebayered sensor pixels, C-10); on its masked planes `N*` reads the masked mesh (D-1). **Task
13** (ruling Q6, not yet in its text): the weights on trailed / tailed frames against the ERP (L 0010, L 0011, H 0029)
— a tailed frame's `c` takes the tail light and PSFSW goes as `c`²; a fix, if one is needed, belongs to the weights'
owner.

---

### Task 10c: near-clip saturation (spec §3 F-3, owner of the saturation flag's near-clip class)

**Stub (ruling P2, 2026-10-11): scope, readers and the bar's derivation are fixed now; Files and Steps are to be
written after 10b's joint cycle.** Runs right after Task 10b and before Task 11 — `c`'s qualifiers, the frame
statistic and later F-6's SNR and HFR all read the saturation flag. Within Plan A's existing MUST-RESOLVE scope (the
M42 frame β, moved from Task 13 through Task 10b's S.3); F-3's owner (Task 8) is closed, so this task owns the class.

**Scope.** The saturation flag's near-clip class-B definition: bright stars whose peak sits near the clip yet carry no
`saturated` flag, so they enter the β sample, `c`'s qualifiers and the frame statistic (D2: on M42 mono2 0000, 35 of
its 52 accepted β-sample stars have β ≥ 10 and a peak at ≥ 0.95 of range; LDN 1272 0099 the same, 63 of 64; frame β
13.96 at t10-f5-r2 against 3.14 / 3.34 on 0008 / 0010). The class reads Task 8's `SaturationPlane` range (`bg`,
`range`) — never a new maximum; 10b's knot exemption (`near_clip`) reads the same level. Out of scope: the knot /
extended share of the M42 β (10b's, S.3), the plateau test's other clauses unless the class's definition needs them
(recorded with its numbers), the c = 1 floor (F-9).

**Owner of** the `saturated` flag's near-clip class (spec §3 owned quantities). Readers: the β sample, the frame
FWHM/ecc statistic, `c`'s qualifiers, later F-6's SNR and HFR, `.athf`.

**Bar — derived from data before any 10c code, never a fixed 10 %.** The M42 frame β judged against the set's own
spread: M42's 12 clean calibrated frames (10b-L (f): β 2.99–4.79 without 0000, 13.96 with it) and a clean comparison
set (hc11B L: 3.67–4.97); the statistic (0000's β inside the other frames' spread, that spread no wider than the
comparison set's) and its SE are written from 10b's S.3 report before code. F-3's twin rows and row 10 (FN ≤ 0.5 %)
and the no-regression table hold as everywhere.

**Inputs.** 10b's S.3 report (the mechanism split per frame), D2's `M0000_sample.png` and its dumps
(`t10-scratch/d2/out/`), Task 8's verdict and row 10.

**Files / Steps** (written 2026-10-12 from the design read `task-10c-report-design.md`; rulings R10c-0…R10c-6 in
`task-10c-rulings.md`; lean execution: one design read, one implementer, ONE lab step judging the bar with the F-3
rows; a miss goes to the owner with its numbers).

**The bar as fixed before code (R10c-0).** The literal min–max form fails at the head and after any fix for a reason
outside this task (0002's own PSF, β 4.9–5.6 in every brightness bin), so the bar is the robust form, M = the 11 other
M42 mono2 L calibrated frames, C = the hc11B L replica 0000–0009, s = 1.4826·MAD, SE_j the bootstrap SE (2000×) of
frame j's median over its own β sample: **B1** |β₀₀₀₀ − med M| / √(s_M² + SE₀²) ≤ k, k the largest such z any of the
21 clean frames produces left out against the rest of its own set (same run); **B2** SE₀/β₀₀₀₀ ≤ the largest SE_j/β_j
of the clean frames; **B3** s_M/med M ≤ s_C/med C. Head: 31.777 (SE 15.89): B1 1.79 ≤ 3.11, B2 0.500 > 0.144, B3
0.088 ≤ 0.207 → FAIL. Predicted after the class: 7.568 (1.13): B1 3.69 ≤ 4.31, **B2 0.149 > 0.059 → FAIL** (M00's
brightest unclipped stars, r0 0.8–0.9, read β 7.7 against 4.4–5.0 for its faint stars; diag D-M00).

**Files.** `star_measure/photometry.rs` (one near-clip test on `SaturationPlane`: the 3×3 maximum about the seed pixel
≥ bg + 0.9·range; clause 3 `by_near_clip` in `saturation_verdict` when the plateau test did not pass; the
`SaturationVerdict.by_near_clip` field), `star_measure/seeds.rs` (`near_clip` calls the same test — the knot exemption
and the flag read one quantity), `star_measure/beta.rs` (test), `examples/star_probe.rs` (`byNearClip`); spec §2.1 and
the owned-quantities row, README R-OSP-7c, this task's Outcome.

- [x] Phase A (implementer): the class, tests that fail first (`an_undersampled_star_clipped_on_one_pixel_is_flagged_near_clip`,
  `near_clip_reads_the_3x3_maximum`, `the_sample_skips_near_clip_stars`, the near-clip pins), the F14 sweep named;
  release probe frame β on M42 and the hc11B L replica equal to the design read's a3 column; core `--all-targets`,
  `--no-default-features`, `--examples` green; one commit on `one-star-path-t10c-wip`.
- [x] Diag D-M00 (beside Phase A): the mechanism of M00's bright-star β and whose job it is (its "doubled frame"
  reading rejected by the owner, R10c-6).
- [x] ONE lab step (runner, judgment run): the bar B1–B3; F-3 twins 48 / 48 (F ≤ 1 %, β void where either twin
  > 10); row 10 FN ≤ 0.5 % mono and OSC (predicted 0 / 12 169, 0 / 1 229), FP reported (predicted 1.02 % / 0.54 %);
  no-regression 0 moved outside the design list (the 19 β and 9 F planes of `t10-scratch/10c/labeval/a3.vs_base.tsv`;
  234 planes change their flagged set); control 0.
- [x] The remainder (B2) ruled before the lab step (owner 2026-10-12, after a Fable review of the plan,
  `task-10c-review-fable-plan.md`): M42 0000 is a clean frame (sources identical by eye, the reference program's ecc
  +0.001); its β 7.6 after the class is the frame β estimator's error — a one-sided shoulder in 0000's mean profile
  (ePSF cut +1 / +1.5 / +2 px: 0.53 / 0.45 / 0.26 vs 0001 0.59 / 0.33 / 0.16) that a single Moffat cannot represent,
  turned into a flat top by between-pixel cores and over-picked by the peak-ranked sample (53 % vs 32 %); ≈ 1 % of
  0000's FWHM. B2 is reported as a KNOWN MISS owned by Task 11b (F-4b), not a frame property.
- [x] Fix round (R10c-8 → R10c-10, added 2026-10-12): the fixed band's false positives cost frame ecc on lab truth,
  so the floor is derived per plane — design round D2 (`task-10c-report-design2.md`, arm cc), Phase A2 (one
  implementer), the lab step `t10c-floor` (bars: FN ≤ 0.5 %, FP ≈ base, C-11 ≈ a1c's, M42 B1 / B3 pass).
- [x] Review (`review-t10c.md` on 9f2cafc7, `review-t10c-2.md` on 7933f582); docs as built (spec §2.1 and F-3, README
  R-OSP-7c, this Outcome). Ledger items (Fable (d)): on a clip-free plane `range = min(plane max, norm) − bg` puts the
  brightest CLEAN star in the 0.9·range band (266 lab FP) — a limit Task 11b's sample rule must not inherit; the
  per-frame split share of the β sample before / after the class (11b's premise). Both carried below.
- [x] Squash the WIP branch onto `one-star-path` as one commit (the controller, 2026-10-12): WIP `one-star-path-t10c-wip`
  9f2cafc7 (the class, fixed band) → 7933f582 (the floor derived per plane) → 511087dd (review-2 comment minors) →
  1c3d8bf2 (these docs), one squashed commit on `one-star-path`; core all targets green before the push (GitLab only).

**Recorded, not built (R10c-2…R10c-5).** β 1.5 stays a clamp (a post-fit reject is a truncated median that biases β
up near the bound; LDN mono's 1.5 is a whole-frame property → the later fit-design task); the M42 plateau seeds move
nothing once the class is in (if wanted, one seed per clipped component in `seeds.rs`, F-1's owner); PSFSW's
saturated share (0.79–0.90 M42, 0.77–0.82 hc11B L; `frame.rs::signal_totals` reads no flag; relative M42 weights 0.40–1.00
→ 0.82–1.00 with flagged fits out) → Task 13; D0099's β 99.96 at every brightness → Task 13's frame classes.

**Outcome (2026-10-12).** Commits (athenaeum, WIP branch `one-star-path-t10c-wip` from 3deb6cb5, to be squashed as
ONE F-3 commit on `one-star-path`): 9f2cafc7 (Phase A: clause 3 at the fixed 0.9·range band, one
`SaturationPlane::near_clip` shared with the knot exemption; probe β / F / unsat equal to the design read's a3 column
on all 27 planes), 7933f582 (Phase A2: the clip floor derived per plane, R10c-10; the fixed band's F14 fixtures and the
four `stacking/measure.rs` re-pins reverted to 3deb6cb5's numbers; review 1's I-1 / I-2 / M-2 fixed, M-1 moot; core
lib 3871 passed / 0 failed / 9 ignored), then this docs commit. Design reads `task-10c-report-design.md` (bar R10c-0,
arms a / a3 / b / r15 / ps / r3.0) and `task-10c-report-design2.md` (round D2, arm cc); diag D-M00
`task-10c-report-M00.md` (its "doubled frame" reading REJECTED by the owner, R10c-6); Fable's plan review
`task-10c-review-fable-plan.md` (R10c-7). Lab (athenaeum-lab, runner): `t10c-nearclip` at 9f2cafc7 (lab 2971210) —
B1 3.69 ≤ 4.31, B3 0.071 ≤ 0.210, B2 0.149 > 0.059 (the known miss), twins 48 / 48, FN 0 / 11 778 · 0 / 1 229, FP
1.047 % / 0.543 %, every held bar ok, but C-11's frame ecc WORSE on 68 planes; L2 (same lab commit,
`l2-ecc-split.txt`) split it: the fixed band's FP set change, 0 truth-saturated among 871 newly flagged → R10c-8, and
`trails_i0` F referee-dependent → R10c-9. `t10c-floor` at 7933f582 (lab 7eb48fb) **every bar met, B2 the known miss**:
β₀₀₀₀ 31.777 → 7.568 (SE 1.173), B1 3.60 ≤ 5.39, B3 0.051 ≤ 0.207, B2 0.155 > 0.046; twins 48 / 48 (F worst 0.69 %,
β 6.8 %); FN 4 / 11 778 (= base) · 0 / 1 229; FP 0.244 % / 0.410 % (base 0.242 / 0.397); 0 of 333 planes moved vs
base; C-11 frame ecc vs a1c max +0.00145, 0 planes over +0.002; seeds identical 391 / 391; item 1 58 / 58; lab_cycle
0 MISS in 70 + 20; `c_judge` 82 ok; S.1 0.81 %; M.1 76.8 s; same-star 0 WORSE; one letter-STOP (a new C-11 WORSE
plane, `osc_pairs/f3.5` t3 p0, 1× the line) accepted as R10c-11. Reviews: `review-t10c.md` (9f2cafc7: Ready with
fixes, 0 / 2 / 3; M-3 = R10c-9's wording here), `review-t10c-2.md` (7933f582: Ready, 0 / 0 / 3; M-2 / M-3 = the floor's
limits (f) / (g), in spec §2.1 and README; the code comments are the controller's). Rulings index (README R-OSP-7c,
one line each): R10c-0 the bar; R10c-1 the class (a3); R10c-2 the clamp; R10c-3 plateau seeds; R10c-4 PSFSW → Task
13; R10c-5 D0099 → Task 13; R10c-6 the 0000 remainder (corrected by the owner); R10c-7 the order and Task 11b; R10c-8
the fix round; R10c-9 trails wording; R10c-10 the derived floor; R10c-11 L3's letter-STOP. README R-OSP-7c complete
(the C table, the per-frame β table, limits (a)–(g)); spec §2.1 (the clip floor), §3 F-3 as built and as judged, the
owned-quantities table and §5 follow it.

Carries. **Task 11b:** B2 (0000's β 7.568, SE 1.173: 0.155 > 0.046) with its truth family, gate zero and arms
(R10c-7); Fable's ledger items — (1) the clip-free plane's brightest clean star at the range: out of class B since
the floor (0 lab planes moved, FP back to base), still flagged by clauses 1 and 2 (limit (e)), so 11b's sample rule
must not inherit it; (2) the β sample's split share per frame before / after the class (D-M00 at the fixed band: 0000
53 % vs 32 % in the population), to be read at 7933f582 as 11b's premise. **Task 13:** PSFSW (R10c-4) and D0099
(R10c-5), already in its text. **The later fit-design task:** LDN mono's β 1.5 (R10c-2; a 3F region floor 1.50 →
1.78). **F-1's seeds, if wanted:** one seed per clipped component (R10c-3).

---

### Task 11: F-6 — the per-star SNR and HFR (spec §3 F-6, §4.2 HFR)

**Reads Task 10b's sky and noise (moved by Task 10b, 2026-10-11).** Task 11 estimates no sky and no noise: S, σ_sky
and N_sky are the star's stored `sky_ring`, `sky_ring_sigma` and `sky_ring_n` (B_i, σ_i, n_i), and HFR subtracts
`sky_ring`. PA-11 keeps the aperture radius; its annulus is retired for SNR (ruling B4), so the tests below that build
PA-11's annulus (`snr_matches_the_analytic_value_on_a_white_noise_star`'s `n_sky`,
`edge_star_snr_uses_the_present_annulus_pixels`, `snr_is_nan_and_counted_when_no_annulus_pixel_is_present`) are
rewritten on 10b's ring when this task is briefed, and the lab's `snr_ap` truth column takes the ring's pixel count.
Checklist: HFR ≤ 5F by construction (a c ≠ 1 case on Gaussian truth); SNR vs truth on `background/*` is now a real
bar; the gain plumbing is orthogonal.

**Files:**
- Modify: `star_measure/photometry.rs` (`star_snr` over the stored σ_i (moved by Task 10b, 2026-10-11; was Task 8's `annulus_sigma`), `hfr`), `star_measure/fit.rs`
  (`StarFit.snr`, `.hfr`; non-persisted like `peak5`; the `StarFit` literal sites of ruling F36 gain the two fields
  — the same five hand-edited sites as Task 4, or `StarFit::test_at` where it was adopted), `star_measure/frame.rs`
  (`snr_median`, `hfr_median`: medians over the finite values only, so a `snr_missing` NaN never poisons the frame
  median — pinned), `star_measure/mod.rs` (`PlaneMeasurement.snr_gain`, `.snr_missing`), `star_measure/domain.rs`
  (`Domain::gain_e_per_working_adu()`: the domain's gain in e⁻ per ADU of the working-unit plane), `stacking/measure.rs`
  (hand edit: the adapter reads `EGAIN` and `ATH_CSCL` from the FITS header through `crate::fits_parser` and builds
  `Domain::Calibrated { gain_e_per_unit: Some(EGAIN × ATH_CSCL) }` — e⁻ per calibrated unit, PA-22, ruling F6;
  `None` when either is absent), `examples/star_probe.rs` (same header read; `snr`, `hfr` on star lines;
  `snrGain` on the frame line), `docs/stacking/README.md` (R-OSP-8)

**Interfaces:**
- Produces: `star_snr(plane, w, h, fit, gain: Option<f64> /* e⁻ per working ADU */, mask: Option<&[bool]>) -> (f64 /* snr */, SnrTerms { s,
  n_ap, sigma_sky, n_sky, shot_term: bool })` per PA-11's aperture with `(σ_sky, N_sky)` = the fit's `(sky_ring_sigma,
  sky_ring_n)` (moved by Task 10b, 2026-10-11; was `photometry::annulus_sigma`; a NaN σ_i → `snr = NaN`, counted in
  `PlaneMeasurement.snr_missing`): `σ_S² = S/g + N_ap·σ_sky²·(1 +
  N_ap/N_sky)`; without `g` the first term is omitted and `shot_term = false`. `hfr(plane, w, h, fit, mask:
  Option<&[bool]>) -> f64`: the radius about the fitted centre inside which the background-subtracted cumulative sum
  (pixels sorted by distance, `b = fit.sky_ring` — moved by Task 10b, 2026-10-11; was `fit.background`) reaches `0.5 × fit.signal`, interpolated between the two
  bracketing pixels; `NaN` when the 5F circle holds less than half of `signal`. Non-finite and masked pixels are
  skipped and never counted in `N_ap`/`N_sky` (Review Focus 1, 5; ruling F39: both take the mask).

- [ ] **Step 1: Write the failing tests** (`photometry.rs` and `stacking/measure.rs` tests)

```rust
use crate::test_support::{moffat_field, gaussian_field, MoffatStar, add_noise};

fn one_star(amp: f64) -> (Vec<f32>, Seed) {
    let s = MoffatStar { x: 40.3, y: 39.7, amp, alpha_x: 2.99, alpha_y: 2.99, theta: 0.0 };
    (moffat_field(80, 80, &[s], 4.0, 0.05), Seed { x: s.x, y: s.y, peak: amp, flux: 1.0 })
}

#[test]
fn snr_matches_the_analytic_value_on_a_white_noise_star() {
    // sky 0.05, g 50000 e/unit, rn 0.0005: σ_px = sqrt(0.05/50000 + 0.0005²) = 0.001118; a star of total flux F;
    // the analytic SNR with PA-11's geometry; the median over 20 noise draws within 5 %
    let g = 50000.0;
    let (clean, seed) = one_star(0.2);
    let total = std::f64::consts::PI * 0.2 * 2.99 * 2.99 / 3.0;
    let sigma_px = (0.05 / g + 0.0005f64.powi(2)).sqrt();
    let mut snrs = Vec::new();
    for k in 0..20 {
        let mut noisy = clean.clone();
        add_noise(&mut noisy, sigma_px as f32, k);
        let data: Vec<f32> = noisy.iter().map(|v| v * ADU_SCALE).collect();
        let out = fit_stars_at(&data, 80, 80, &[seed], 4.0, &FitStarsOptions { gain_e_per_working_adu: Some(g / ADU_SCALE as f64), ..Default::default() }, None);
        snrs.push(out.fits[0].snr);
    }
    let r_ap = (1.5 * 2.6f64).max(3.0); let n_ap = std::f64::consts::PI * r_ap * r_ap;
    let r_in = (3.0 * 2.6f64).max(6.0); let r_out = r_in + 2.6f64.max(3.0); let n_sky = std::f64::consts::PI * (r_out * r_out - r_in * r_in);
    let s = total * (1.0 - (1.0 + (r_ap / 2.99).powi(2)).powi(-3));     // 0.949: the Moffat-4 fraction inside r_ap (analytic)
    let analytic = s / (s / g + n_ap * sigma_px * sigma_px * (1.0 + n_ap / n_sky)).sqrt();
    let med = median(snrs.iter().cloned());
    assert!((med / analytic - 1.0).abs() < 0.05, "snr {med} vs analytic {analytic}");
}

#[test]
fn snr_without_gain_omits_the_shot_term_and_says_so() {
    let (clean, seed) = one_star(0.2);
    let data: Vec<f32> = clean.iter().map(|v| v * ADU_SCALE).collect();
    let with = fit_stars_at(&data, 80, 80, &[seed], 4.0, &FitStarsOptions { gain_e_per_working_adu: Some(1.0), ..Default::default() }, None);
    let without = fit_stars_at(&data, 80, 80, &[seed], 4.0, &FitStarsOptions::default(), None);
    assert!(without.fits[0].snr > with.fits[0].snr);
    let (_, terms) = star_snr(&data, 80, 80, &without.fits[0], None, None);
    assert!(!terms.shot_term);
    let (_, terms) = star_snr(&data, 80, 80, &with.fits[0], Some(1.0), None);
    assert!(terms.shot_term);
}

#[test]
fn edge_star_snr_uses_the_present_annulus_pixels() {
    // a star 4 px from the border with a NaN wedge beside it: n_sky < the full count, snr finite (Task 8's annulus)
    let s = MoffatStar { x: 4.0, y: 40.0, amp: 0.3, alpha_x: 2.99, alpha_y: 2.99, theta: 0.0 };
    let mut data = moffat_field(80, 80, &[s], 4.0, 0.05);
    for y in 50..80 { for x in 0..20 { data[y * 80 + x] = f32::NAN; } }
    let fit = StarFit { x: 4.0, y: 40.0, background: 0.05, amplitude: 0.3, fwhm_x: 2.6, fwhm_y: 2.6, ..StarFit::test_at(4.0, 40.0, 0.3) };
    let (snr, terms) = star_snr(&data, 80, 80, &fit, None, None);
    assert!(snr.is_finite() && snr > 0.0);
    // PA-11 at FWHM 2.6: the ring 7.8..10.8 (π(10.8² − 7.8²) = 175; 180 pixels about an interior pixel centre);
    // here the border and the NaN block leave 110 of it present (counted)
    let (r_in, r_out) = ((3.0 * 2.6f64).max(6.0), (3.0 * 2.6f64).max(6.0) + 2.6f64.max(3.0));
    let full = (std::f64::consts::PI * (r_out * r_out - r_in * r_in)) as usize;
    assert!(terms.n_sky < full && terms.n_sky > full / 4, "{}", terms.n_sky);
}

#[test]
fn snr_is_nan_and_counted_when_no_annulus_pixel_is_present() {
    // ruling F29: only the star's own PA-11 annulus is masked (0.5 px wider on each side), so the star still fits and
    // the SNR alone has nothing to read; the unmasked run reads a finite SNR, so the test cannot pass on zero fits
    let (mut data, seed) = one_star(0.2);
    add_noise(&mut data, 0.002, 9);
    let opts = |mask: Option<Vec<bool>>| MeasureOptions { seeds: SeedSpec::Given(vec![seed]), mask, ..Default::default() };
    let open = measure_plane(&data, 80, 80, &opts(None), None);
    assert_eq!(open.stars.len(), 1);
    assert!(open.stars[0].snr.is_finite() && open.snr_missing == 0);
    let f = open.stars[0];
    let r_in = (3.0 * f.fwhm()).max(6.0);
    let (lo, hi) = (r_in - 0.5, r_in + f.fwhm().max(3.0) + 0.5);
    let mask: Vec<bool> = (0..80 * 80).map(|i| {
        let d = ((i % 80) as f64 - f.x).hypot((i / 80) as f64 - f.y);
        d < lo || d > hi
    }).collect();
    let m = measure_plane(&data, 80, 80, &opts(Some(mask)), None);
    assert_eq!(m.stars.len(), 1, "the star itself is unmasked and fits");
    assert!(m.stars[0].snr.is_nan());
    assert_eq!(m.snr_missing, 1);
    assert!(m.frame.snr_median.is_none());
}

#[test]
fn hfr_of_a_gaussian_is_the_half_flux_radius() {
    // ruling F29: six isolated stars, so the aperture correction is measured (≥ 5 qualify, Task 10) and `signal` is
    // the total flux; one star would leave c = 1 and signal = 0.898·total, the HFR 9 % low (computed). The phase
    // (0.3, 0.7) matters: this interpolated discrete HFR moves with the sub-pixel phase (σ 2: up to ±8 % over a
    // 0.1-px phase grid, computed); at this phase it reads +0.2 % with c measured
    let sigma = 2.0;
    let stars: Vec<(f64, f64, f64)> = (0..6).map(|i| (60.3 + (i % 3) as f64 * 60.0, 59.7 + (i / 3) as f64 * 60.0, 0.3)).collect();
    let g = gaussian_field(240, 180, &stars, sigma, 0.0);
    let data: Vec<f32> = g.iter().map(|v| v * ADU_SCALE).collect();
    let seeds: Vec<Seed> = stars.iter().map(|&(x, y, p)| Seed { x, y, peak: p, flux: 1.0 }).collect();
    let out = fit_stars_at(&data, 240, 180, &seeds, 100.0, &FitStarsOptions::default(), None);
    assert!(out.aperture_correction.is_some(), "c is measured on six isolated stars");
    let want = 1.177_41 * sigma;                                          // the Gaussian's half-flux radius √(2 ln 2) σ
    assert_eq!(out.fits.len(), 6);
    for f in &out.fits {
        assert!((f.hfr / want - 1.0).abs() < 0.03, "hfr {} vs {want}", f.hfr);
    }
}

// stacking/measure.rs tests (hand-edited)
#[test]
fn the_adapter_reads_egain_and_cscl_into_the_gain() {
    use crate::fits_writer::{write_fits_f32, Card, CardValue};
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("g.fits");
    let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
    let cards = [Card::new("EGAIN", CardValue::Real(50000.0 / 65535.0)).unwrap(),
                 Card::new("ATH_CSCL", CardValue::Real(65535.0)).unwrap(),
                 Card::new("ATH_CFNM", CardValue::Real(3000.0)).unwrap()];
    write_fits_f32(&p, w, h, 1, &data, &cards).unwrap();
    assert!((gain_from_header(&p).unwrap() - 50000.0).abs() < 1e-6, "EGAIN × ATH_CSCL, CFNM ignored (PA-22)");
    let q = d.path().join("n.fits");
    write_fits_f32(&q, w, h, 1, &data, &[]).unwrap();
    assert_eq!(gain_from_header(&q), None);
}
```

  Two gain names, one per unit (fold check): `Domain::Calibrated { gain_e_per_unit }` is e⁻ per CALIBRATED unit
  (= EGAIN × ATH_CSCL, 50000 in the adapter test); `FitStarsOptions.gain_e_per_working_adu: Option<f64>` — e⁻ per ADU
  of the working-unit plane (`gain_e_per_unit / ADU_SCALE` for a calibrated plane, 0.763 in the SNR test) — joins
  the options here, filled by `measure_plane` from `Domain::gain_e_per_working_adu()`; `measure.rs` gains
  `fn gain_from_header(path: &Path) -> Option<f64>` (the `fits_parser` read).

- [ ] **Step 2: Run, fail, implement, run.**

- [ ] **Step 3: Lab cycle** `t11-f6`. Design list: `{"row":6,"metric":"star SNR vs truth","path":"measure","kind":"set-aside"}`
  (new numbers); nothing else moves (the SNR and HFR are additive fields). `{"row":1,"path":"register","kind":"reported"}`
  stays listed for uniformity (no fit moved; it must read 0 moved).

- [ ] **Step 4: Read the bar** (`bars.txt`): row 6 `star SNR vs truth` median bias ≤ 3 % on every `sharp/*` (46
  families, ruling F32) and `background/*` (7) family (unsaturated, isolated by the families' own `min_sep`);
  `n_no_gain` must be 0 (every v2 image carries EGAIN from Task 1a). The u16 `analysis-mode` arm is Task 12's.

- [ ] **Step 5: R-OSP-8; commit.**

```
feat(star_measure): F-6 per-star SNR (S/σ_S with the EGAIN x ATH_CSCL gain, the shot term omitted and said when unknown) and HFR on the F-5 flux (SNR bias vs truth <cells>)
```

**Acceptance:** median bias ≤ 3 % on the 53 families; both diffs 0 moved outside the design list.

**Outcome (2026-10-12).** Commits (athenaeum, WIP branch `one-star-path-t11-wip` from 35870dbf, to be squashed as
ONE F-6 commit on `one-star-path`): 1224718a (Phase A: `star_snr` = S / σ_S on the star's own ring sky and noise with
κ(A) from the plane's one noise read and the shot term spread by the CFA site share, the EGAIN × ATH_CSCL header gain
(omitted and said when absent), `hfr` on the F-5 flux, the frame medians over the statistic's finite fits; probe seed /
star / frame lines byte-identical to 35870dbf after stripping the new keys), 12706d6c (the review round: a given-seed
`Mixed` / `Unknown` plane reads its noise correlation off the prep's one mesh, no second mesh build; one warn per
failed read; the `star_snr` lint allow dropped; doc wording; the probe output byte-identical on the five Phase A
inputs), then this docs commit. Design read `task-11-report-design.md` (Q1 arm (c); Q2 the structure term measured
out; Q3 the read is the one quantity; Q4 populations and HFR; Q5 the gain; Q6 the consumers). Rulings index (README
R-OSP-8, one line each): R11-1 σ_S (arm c: κ(A) from 10b's read, s = 1 / the site share); R11-2 the R / B κ
under-read (recorded limit); R11-3 the structure term, not in F-6; R11-4 the bar (50 cells, `neb0.5` / `neb3` /
`neb3_4096` reported, the noise-pull row; amends Step 4 and the Acceptance's 53 families; owner informed); R11-5 the
populations (amends Files' "finite values only"); R11-6 the header gain only; R11-7 additive; R11-8 the clipped pull
referee; R11-9 the pull row `reported` in the design list; R11-10 the control STOP. Review `review-t11.md` (1224718a:
Ready with fixes, 0 Critical / 1 Important / 7 Minor; I1 and m1–m5 fixed in 12706d6c, m6 a note, m7 the squash's
trailers). Lab (athenaeum-lab, runner): 70b116e (Part 1: the truth `snr_ap` on the star's own ring, 299 of 301 v2
truth files moved, only `snr_ap`; the noise-pull row; the Mixed note), b3d8434 (design list, bars), 1cd8704 (R11-10:
`scorecard_diff`'s set of truth steps), 60bbf27 (scripts), 8532f01 (results). `t11-f6` at 1224718a (base t10c-floor)
**every bar passes; one STOP, the PA-15 control (143 / 138 row-6 cells on path `analysis`: the truth moved, the legacy
estimator did not), resolved by R11-10**: row 6 worst 0.97 % on the 50 cells (`neb0.1`; `sharp/*` −0.90…+0.51 %),
`n_no_gain` 0; the nebula cells −16.54 / −75.51 / −60.65 % (= predicted); noise pull 0.92…1.25 on 50 planes
(`resampled` f2.0 1.07); row 6 on Mixed reported (`osc` −41.5…−24.4 %, `resampled` −5.9 / −1.9 %, as predicted);
3.84 M lines identical against t10c-floor after the new keys; no-regression and cumulative moved 0, control 0; every
held bar = t10c-floor (R10c-0 B1 3.60 ≤ 5.39, B3 0.051 ≤ 0.207, B2 0.155 > 0.046 the known miss; twins 48 / 48; FN
4 / 11 778 · 0 / 1 229; FP 0.244 % / 0.410 %; item 1 58 / 58; S.1 0.81 %; M.1 76.9 s; same-star 0 WORSE; `c_judge`
82 ok); HFR vs analytic and `snrNoise` (read 50, white 139, unread 0) reported. README R-OSP-8 complete (the rulings,
the cycle table, the recorded limits); spec §3 F-6 as built and as judged, §4.2's HFR line and the owned-quantities
table (the noise-correlation row) follow it. Cost, from an interleaved bracket (runner L3, lab 7a70351, 35870dbf vs
12706d6c): LN +1.3 pp over the null control, so no skip flag on LN's refits; the stack invocation +9.5 pp on small
synthetic frames; M.1 on the real M42 frames 76.9 s (76.8 s before).

Carries. **Task 12 (its cost bars):** the small-frame stack cost (+9.5 pp, not yet split between the per-star SNR / HFR
and the given-seed noise read on Mixed planes). **The detection owner (a later item):** R11-2 — the 10b read under-reads κ on R / B by 12–21 % (pull
1.16–1.28 on R / B at F 5); fixing it moves colour seeds (injected LDN OSC R recall −5…−7 pp with the VNG white-noise
table). **Plan B / the later nebula-photometry owner:** R11-3 — on nebula sky the real flux error is 3.6–6× σ_S (the
local sky mismatch under the disc); the RB4 structure term breaks 4 clean cells and its c = 0.8 is circular. **Plan
C** (the later readers of `snr` / `hfr`, the design read's "Later readers"): `analysis/analyzer.rs` (StarMetric `snr`
/ `hfr`, `FrameAnalysis.median_snr`) → `db/analysis.rs` / `schema.rs` (`star_metrics.snr`, `hfr`); `collab/gate.rs`
(`median_snr` rules) and the hub's `collab_rules.rs`; `collab/frame_meta.rs` (`medianSnr`); `api/collab*.rs`;
`api/sync.rs`; the UI `LightsAnalysisTable.tsx`, `LightsAnalysisView.tsx`, `analysis/LightsAnalysisChartView.tsx` +
`chartDataTransform.ts`, `FrameInfoPanel.tsx`, collab `project/frames.tsx` / `OverviewTab.tsx`, `types/models.ts`.

---

### Task 11b: F-4b — a frame-level β (spec §3 F-4; inserted 2026-10-12, owner: "согласен" to the Fable-reviewed order)

**Stub: scope, truth and bars fixed now; Files / Steps after Task 11.** Order 10c → 11 → 11b → 12: Task 11 reads no
β, this step has an unknown-size lab and fitter component, and Task 12 budgets the FINAL Measure cost. Evidence:
`task-10c-report-design.md`, `task-10c-report-M00.md` (its "doubled frame" reading rejected by the owner),
`task-10c-review-fable-plan.md`, figure `t10-scratch/10c-m00/png/owner_doubled.png`.

**The defect.** After Task 10c, M42 0000's frame β reads 7.6 against 2.6–4.8 on its 11 siblings (bar R10c-0 B2 miss):
its unflagged fits whose core falls between two pixels read point β 9.4 against 3.8 for pixel-centred cores (0001
4.8 / 4.4, 0008 3.4 / 3.2); the pixel-integrated sample β 9.78 / 4.91. Mechanism (Fable): a one-sided shoulder in the
frame's mean profile (≈ 12 % of peak at +1.5 px; invisible to the eye and to ecc) that the single elliptical Moffat
cannot represent × the between-pixel flat top × the peak-ranked sample. NOT pixel phase on symmetric stars (research
F4: the pixel-integrated model is phase-invariant within 0.1–0.7 % there; 0008 at F 1.62 shows none). Size: ≈ 1 % of
0000's FWHM (2.342 → 2.267 with the class); 0000 stays the widest frame by ≥ 8 % at any β. The step opens with a
definition and a referee (β exists for the at-β fits' FWHM and F-5's flux), not with candidates ("re-audit, stop
patching": E5, R2, A3, A4, the centred sample, the near-clip class all moved β).

**Truth family (lab; reuse, never rebuild).** The existing `ComaPsf` generator (`star-lab/starlab/families.py`: a round
Moffat core holding `core` of the flux, the rest displaced by d) extended to F 1.6–2.4, core 0.70–0.85, d 0.5–0.8 F,
random sub-pixel phases; the symmetric m-family at the same F and SNR as the control. **Gate zero: HEAD must FAIL the
family the way 0000 fails (split / single ≥ ≈ 2 on some cell, a bimodal sample) — else the family lacks the mechanism:
STOP, the owner is told, no code.**

**Bars (derived, never a fixed 10 %).** (1) Phase invariance: |β_split − β_single| / β ≤ the symmetric control's own
split spread at the same F and SNR. (2) The consequence: frame FWHM vs the family's model-free half-max (pixbox) within
F-4's 1 % / 3 %. (3) The M42 bar B1–B3 as fixed in R10c-0, plus 0000's frame FWHM vs its ePSF half-max. (4) No
regression on the 337 lab planes, the F-3 twins, item 1 ≥ 56 / 58, the Measure wall.

**Candidates, in order (one instrumented build, env-gated arms, the 10c design-read pattern):** (1) one joint sample
fit — β, α_x, α_y, θ shared by the frame, (x, y, A, B) per star over the 64 stars' pixels at once (block-diagonal LM);
(2) a shape-anchored β whose model growth curve matches the sample's measured one (F-5's 5F growth sums); (3) the sample
rule — rank by F-5 flux inside the band below 0.9·range so the sample is not peak-selected (an arm: it moves every
frame). Task 10b's 3F region floor (RB3) as an arm beside each. Out of scope: the pixel-phase centroid correction
(other quantity, ruled outside Plan A), C-12d (ecc's shape region), any per-star model change.

**Steps:** (i) the lab: the `coma` grid, gate zero on HEAD, the bars (one runner task); (ii) the design read with the
arms, judged on the family, the M42 bar and the no-regression table; (iii) one implementer for the winning arm; spec
F-4 and the owned-quantities row; Task 13's M42 line closed with the numbers.

**Outcome (as built, 2026-10-13).** Commits (athenaeum, branch `one-star-path-t11b-impl` from af7de707): 0f70e3c0
(F-4b: under `Auto` the stacking Measure fits every group member at the GROUP β — two-pass over `PlanePrep`,
`psf_signal::group_beta` of the members' own βs (the lower median, LN's value), the frame's own β kept as the
diagnostic `beta_own`; the measurement hash folds the group β under `Auto` only; `star_probe --psf-beta`), 3af1fd8d
(the review round: I1 pass-1 progress, I2 the doc note, M1–M4 incl. `beta_own_at_cap` and the fan-out length check as
an `error!` + `Err`), then this docs commit. Not merged, the archive: the WIP branch `one-star-path-t11b-wip` and its
bundles (the arms, `band`, `shoulder`), the A4 `fref` edits in a stash. The path (rulings R11b-0…R11b-13, README
R-OSP-9): D0 put 0000's separation in our free-β estimator on a 5 % wider, weakly skewed frame, not in its pixels'
shape (R11b-0/2); gate zero passed on the `skew` family (R11b-3/4); nine estimators (head and six arms in L2, `band`,
`shoulder`, the summed profile) and the reference-style F_ref (L4) were measured out (R11b-5…R11b-11): a single
Moffat on a composite PSF has a bimodal per-star β and a χ²-best β at a mismatch value; the second design ruling
(R11b-9) made the group β the consumed quantity; the owner challenged it (R11b-10), then decided (R11b-12): the group β
for stacking, Moffat 4 by default for Analysis / Blink (Plan C). Review `review-t11b.md` (0f70e3c0: Ready with fixes,
0 Critical / 3 Important / 6 Minor; I1, I2 (doc) and M1–M4 fixed in 3af1fd8d; I3 the upgrade ripple → README and the
release notes; M5 / M6 notes; the memory ruling → Task 11c). Gates: `stacking::` 835 / 0 / 2 ignored, `star_measure::`
172, `check --all-targets`, `ts_contract`. **The bars** (lab L5, athenaeum-lab 1e52339 at 0f70e3c0): Step 0 the
single-frame path unchanged in value (487 files, 1 849 458 lines identical but for the additive `betaOwn` == `beta`;
B-4 follows, every lab image being a group of one at its own β); M42 group β 3.3066 (0003's own β); **3c F(0000) /
F(0001) 1.0431 ± 0.0013 against [1.044, 1.062] — missed by 0.0009 (0.7 SE; own β 1.0883)**; **3b ours / HFR passes
(z₀ 1.01 ≤ k 2.66); 3b ours / FWHM_m2 misses (z₀ 5.16 > k 2.16, the sign flipped)**; **B2 is not a bar** (resolved by
the definition); B1 / B3 on `beta_own` = Task 10c's (3.60 ≤ 5.39, 0.051 ≤ 0.207); F / the ERP's F 0000 0.964 vs
0.973–1.010; the probe wall `--psf-beta` / default 0.992 (0000) / 1.024 (0008), information. Controller decision
(R11b-13): no further estimator (the owner: no new rounds); the group β is kept. Spec §3 F-4 as built and as judged,
the owned-quantities table (frame F, frame β, the group-β row, `betaAtCap`), §5's Plan C default PSF and §7's risk; the
logging dictionary (`beta_own`, `beta_own_min`, `beta_own_max`).

Limits (README R-OSP-9). 3c and 3b FWHM_m2 on M42: the composite-PSF definition limit (the independent referees span
1.043–1.062 on 0000). The single-frame path over-reads a skewed frame's half-max by the ridge (+1.8 % per unit β; 0000
+4 %). A mixed-profile group reads every F at one β (comparability over per-frame truth). Not judged here: the
stage-level Measure wall (M.1, Task 13).

Carries. **Task 11c** (below): the pass-1 spill — memory, I2, half the upgrade ripple. **Task 12:** the analysis
mode's default PSF is Moffat 4. **Task 13:** its M42 frame-β line is closed by definition; M.1 now covers the two-pass
Measure; the group β judged at master level. **Task 14:** the single-frame ridge as a debt line. **Plan C:** Moffat 4
by default, the own β shown as a diagnostic, Auto selectable; the Measure panel's "Auto (per-frame β)" label
(`stageSummary.ts`) now names the diagnostic. **The release notes:** the upgrade ripple — every existing `Auto` set
re-measures, re-registers and re-normalizes once (review I3).

---

### Task 11c: Spill the Measure first pass to the work dir (review memory ruling, I2; inserted 2026-10-13)

**Scope and bars fixed 2026-10-13 (the stub); Files / Interfaces / Steps by the design read
(`task-11c-report-design.md`), which sharpened the stub where the code contradicts it ("design read" below).** Task
11b holds every freshly measured member's pass-1 records in memory until pass 2 and stores none of them: per plane the
`PlanePrep` (the seeds, one `SeedPass` per seed, the `DetectionReport` per-seed vectors and its 5×5 noise correlation,
the `SaturationPlane`, the sky and noise models, the phase timings) and the adapter head (detector and module options;
its `mask` is `None`, its `Given` seeds are `prep.seeds` again, `measure.rs:944`). No plane-sized buffer anywhere.

**Change.** Pass 1 writes one per-frame artifact (every plane's records) to
`<working_dir>/<set_slug>/measured/<group_key>/<stem>.athm` (a project's under `_projects/<slug>/`), keyed on the calib
hash + the measurement subtree — NOT the group β — as the new `stacking_artifacts` kind `measured`; pass 2 reads it
back; a valid artifact skips all pass-1 work: no pass-1 plane read, no detection, no verdict pass, no own-β sample
(design read: pass 2 still reads the frame, its fits need the pixels). Cost: `Serialize, Deserialize` on 16
`star_measure` types (design read: not ≈ 10), one codec module, one artifact kind (it goes with `Intermediates`; the
work usage gains `measuredBytes`).

**Why.** (1) Memory: ≈ 250 B per seed, ≤ 6 MB per plane at the detector's 24 576 cap — a 300-frame OSC group holds
≈ 0.7 GB typical, 5.4 GB worst, uncounted by admission. (2) I2: a group-β move (an exclusion that moves the lower
median) re-runs pass 1 for every member today; with the spill only pass 2 re-runs. (3) Half the upgrade ripple
(R-OSP-9): the pass-1 artifact survives a re-key. A bound (drop the records over a budget, re-run pass 1 inside pass 2)
is rejected: it computes twice by design, against the owner's "disk as cache" rule.

**Bars.** (1) Byte-identical vs 11b: every `.athf` (bytes), `metrics` payload minus its timings (design read:
`durationMs` and each channel's `timings` hold wall-clock time; every other field byte-equal in the JSON), group β (f64
bits) and stage hash (`metrics`, `fits.<p>`) of a run equals ec39507e's (3af1fd8d's values) on the same inputs, fresh and from the
artifact. (2) A β move re-runs only pass 2: a run-level test (the
`measure_reruns_pass_two_for_the_group_when_the_group_beta_moves` pattern) proves no pass-1 work on a valid artifact
(design read: counted at `measure_frame_pass1`, the one entry of all pass-1 work). (3) Memory bounded to the admitted
workers: at most `admission_n` frames' pass-1 records alive at once (a test-only live counter's peak); the pass-1
results carry the own β only. (4) The wall, interleaved against ec39507e (3af1fd8d's code) on M42: Measure ≤ 1.2 ×;
a re-run after an exclusion that moves the group β ≤ (1 − s/2) × 11b's (design read: s = the pass-1 share of 11b's
fresh Measure, measured in the same bracket; a working spill saves ≈ s, a broken one ≈ 0). (5) Disk as cache: the
artifact is the one stored copy, written once (its row upserted right after pass 1, so a cancelled run's re-run reuses
it), read by pass 2 and by every later run whose key and source stamp match, never recomputed while valid.

**Rulings of the design read (R11c-1…R11c-6; README R-OSP-9, its 11c amendment).**
- **R11c-1 stored vs derived.** Stored, per plane: the `PlanePrep` verbatim, the head's `adapter_detect_ms` and
  `own_phases`, the pass-1 wall; per frame: `w`, `h`, the header gain, `read_ms`, `elapsed`, `beta_own`, the source
  stamp. Rebuilt from `opts` + plane index + plane count + the stored gain: the head's detector and module options,
  through ONE builder `module_options` that `adapter_head` itself calls (no second construction; `finish_plane` reads
  only `domain`, `psf_model`, `fit`, `mask`, `diagnostics`, `cfa_site_share`). Timings: the stored pass-1 phase times
  are the measurement's own (the `metrics` cache-hit convention: a reused row keeps its timings); pass 2 adds its read
  and fit time; the Measure `StageTiming` is this run's wall, the only timing the panel shows.
- **R11c-2 format.** postcard (already a core dependency; f64 as 8 raw LE bytes, so NaN, ±inf and every f64 round-trip
  exactly; ≈ 160 B a seed against ≈ 250 B in memory) behind a hand-rolled header in `.athf`'s style: `b"ATHM"` · u16
  `PASS1_ARTIFACT_VERSION` = 1 · u32 `MEASURE_VERSION` · u32 head length · u64 xxh3 of the head · the head (postcard
  `Pass1Head`: stamp, w, h, own βs) · u64 body length · the body (postcard) · u64 xxh3 of every byte before it. JSON is
  refused: the records hold NaN and ±inf (`SaturationPlane::UNTESTED`, `top3x3 = −inf`, a failed verdict's NaNs).
  postcard is not self-describing, so a golden pin of a hand-built record (length + xxh3) fails on any layout change
  and forces a version bump; a refused decode (magic, version, measure version, checksum, length) is a `warn!` and
  pass 1 re-runs, never a mis-decode. `PASS1_ARTIFACT_VERSION` versions the bytes, `MEASURE_VERSION` the numbers (in
  the subtree, so in the key; in the header as a loud second check). The key folds the format version too, so a bump
  re-keys every row (stale, `debug!`) instead of a warn per frame.
- **R11c-3 key.** `pass1_hash_for(cfg, calib) = stage_hash(&measurement_subtree(cfg), &[calib,
  "measure_pass1:v<PASS1_ARTIFACT_VERSION>"], &[])`: never a `metrics` key (those fold `measure_beta:…` or nothing),
  independent of the group β. `Auto` only: `Moffat4` / `Fixed` keep the single pass and write nothing.
- **R11c-4 identity.** The stamp is the read guard's `FileIdentity` at pass 1's first plane read: size and mtime (signed
  ns since the epoch; `None` where the filesystem has none). Usable iff it equals the calibrated file's identity now;
  else stale → pass 1 re-runs (`debug!` when found before use, `warn!` when the file changed after this run validated
  it), never an exclusion. Pass 2's own read guard is unchanged: a change between its plane reads is still refused.
- **R11c-5 flow.** (0) Per member, on the controller: the `metrics` row's own β (`pass1_own_betas`, unchanged), else the
  `measured` row (`is_fresh` at `pass1_hash_for`) + `read_pass1_head` + the stamp + the plane count → the head's own β;
  else pass 1 is owed. (1) Fanned out: `measure_frame_pass1` → `write_pass1` → the records dropped IN the worker, which
  returns the own β and the written file's identity; the `measured` rows are upserted on the controller at once. (2)
  The group β. (3) Per member needing pass 2, its source: the artifact (validated in (0) or (1), or validated now for a
  member whose own β came from its row) or pass 1 in the worker (11b's arm, for a member with no valid artifact — the
  upgrade ripple — which writes it). The worker `read_pass1`s (checksum, stamp) and falls back to pass 1 on any
  refusal. `force_fresh` (`rerun_from` ≤ Measure) skips the artifact as it skips the `metrics` row: pass 1 runs and the
  artifact is overwritten in place. A write failure (disk full) is a `warn!`, no row, the records dropped and pass 1
  re-run in that member's pass-2 worker — twice only on failure, never held, so bar 3 holds unconditionally (holding
  is refused: on a full disk the Mac cannot swap). Residency: each phase holds at most one frame's records per
  admitted worker; `fan_out`'s results hold the own β (1) and `(m, fits)` (3, as before). The progress rule is
  unchanged: a member whose pass 1 ran in this run's phase 1 ticks its second half only.
- **R11c-6 lifecycle.** `measured/` and its rows go with `Intermediates` and `All` (not with `metrics`: a
  re-calibration rewrites the calibrated file, and its new stamp stales every artifact anyway); an excluded frame's
  artifact stays (a restore reuses it); the project sweep drops its rows as every kind's (rows only). `WorkUsage` gains
  `measured_bytes` (TS `measuredBytes`, `ts_contract`); no Tauri / web command changes (both hosts already call
  `paths::work_usage` / `cleanup_work` through `api::stacking`). The space estimate gains no term (≈ 1.9 % of the
  calibrated term; its doc names it with the other uncounted intermediates). The plan gate is unchanged
  (`gate_measure_betas` reads `metrics` only: a member whose own β lives only in its artifact reads unresolved there,
  11b's conservative rule). Upgrade: no artifact on the first run; a member whose `metrics` row holds its pass 1 runs
  pass 1 inside pass 2 once, writing it.

**Files:**
- Create: `crates/athenaeum-core/src/stacking/pass1_artifact.rs` (the codec, `PASS1_ARTIFACT_KIND`, its tests;
  hand-formatted: the `rustfmt` rule names only `star_measure/*.rs`) and its `mod` line in `stacking/mod.rs`.
- Modify (derives only, no value moves; `rustfmt` by explicit path): `star_measure/mod.rs` (`PlanePrep`,
  `PhaseTimings`, `SnrNoise`; `#[cfg(test)] PlanePrep::for_pin`, the encoding pin), `domain.rs` (`NoiseModel`,
  `SkyLevel`), `fit.rs` (`Seed`, `SeedPass`, `SeedRegion`, `WidthStart`, `FitTrace`), `seeds.rs` (`FieldWidth`,
  `DetectionReport`, `WidthEnvelope`, `NoiseCorrelation`), `photometry.rs` (`SaturationPlane`, `SaturationVerdict`);
  `NoiseSource` and `RefusalReason` already derive both.
- Modify (hand edits only): `stacking/measure.rs` (`module_options`, the record view and `FramePass1::from_record`,
  `FramePass1::source`, the `#[cfg(test)]` hooks `PASS1_LOG` and `Pass1Live`), `stacking/plan.rs` (`pass1_hash_for`, its
  golden), `stacking/run.rs` (`measure_group_two_pass`: `Member.pass1` → a `Pass1Source`; its doc's "Cost, plainly"
  paragraph replaced by the spill; the tests), `stacking/paths.rs` (`measured_root` / `measured_dir` / `measured_path`,
  `WorkUsage`, `cleanup_work`, `INTERMEDIATE_ARTIFACT_KINDS` and its doc, `estimate_bytes`' doc).
- Modify: `src/types/stacking.ts` (generated `measuredBytes`), `docs/stacking/README.md` (R-OSP-9's 11c amendment:
  R11c-1…6, the bars and Step 5's numbers), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` (§9.3 one
  paragraph, §9.5 the `measured/` line). No new log field (`frame_id`, `path`, `error`, `reason`, `count`).
- Lab (athenaeum-lab, committed and pushed): `harness/acceptance/clone-before.sh` (Task 13's recipe exactly as Task 13
  states it, landed here; allow-list: the three before trees + `star-lab-mono2`), `tier1/tier1-run.sh`
  (`ATH_ACC_RERUN_FROM`, Task 13's edit, landed here), `tier1/tier1-extract.py` (`--measure-phases`: the pass-1
  share), `tier1/tier1-compare.py` (`--measure-identity`: `.athf` sha256, payload minus timings, group β, keys; every
  connect `immutable=1`), `results/plan-a/t11c-wall/`.
- Earlier tests this task edits (ruling F14): `measure_reruns_pass_two_for_the_group_when_the_group_beta_moves` (its
  doc and the pass-1 counts), `moffat4_measure_is_one_pass_and_unchanged` (+ no `measured` row: a pin line). No
  `WorkUsage` literal exists outside `work_usage`.

**Interfaces:**
- Produces:

```rust
// stacking/pass1_artifact.rs
pub const PASS1_ARTIFACT_KIND: &str = "measured";
pub const PASS1_ARTIFACT_VERSION: u16 = 1;
/// The calibrated file's identity at pass 1's first plane read (the read guard's `FileIdentity`, stored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStamp { pub len: u64, pub modified_ns: Option<i128> }
/// What step (0) reads without the body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pass1Head { pub source: Option<SourceStamp>, pub width: u32, pub height: u32, pub beta_own: Vec<f64> }
#[derive(Debug)]
pub enum Pass1ArtifactError { BadMagic { found: [u8; 4] }, BadVersion { found: u16 }, BadMeasureVersion { found: u32 },
    Checksum, Truncated(String), Decode(postcard::Error),
    Stale { stored: Option<SourceStamp>, now: Option<SourceStamp> }, Io(std::io::Error) }
/// tmp + `rename_replace` as `write_fits`, through a borrowed view (no clone of the records); the bytes written.
pub fn write_pass1(path: &Path, pass1: &FramePass1) -> std::io::Result<u64>;
/// Magic, versions, the head and its xxh3; the body is not read.
pub fn read_pass1_head(path: &Path) -> Result<Pass1Head, Pass1ArtifactError>;
/// The whole record: the checksum, the stamp against `calibrated` now (`Stale`), the heads rebuilt from `opts`.
pub fn read_pass1(path: &Path, calibrated: &Path, opts: &MeasureOptions) -> Result<FramePass1, Pass1ArtifactError>;

// stacking/plan.rs
pub(crate) fn pass1_hash_for(cfg: &StackingConfig, calibrated_hash: &str) -> String;
// stacking/paths.rs, impl WorkingLayout
pub fn measured_root(&self) -> PathBuf;                               // root/measured
pub fn measured_path(&self, group_key: &str, stem: &str) -> PathBuf;  // root/measured/<group>/<stem>.athm
// stacking/measure.rs
impl FramePass1 { pub(crate) fn source(&self) -> Option<SourceStamp>; }
#[cfg(test)] pub(crate) static PASS1_LOG: Mutex<Vec<PathBuf>>;       // one entry per `measure_frame_pass1` call
#[cfg(test)] pub(crate) fn pass1_peak(dir: &Path) -> usize;           // peak live `FramePass1` of a calibrated dir
```

- [ ] **Step 1: RED, run level** (`stacking::run::tests`; one filter, `stacking::run::tests::measure_`). First the two
  test-only hooks: `PASS1_LOG` pushed at the head of `measure_frame_pass1`; `Pass1Live`, a `#[cfg(test)]` field of
  `FramePass1` counting live values and their peak per calibrated directory (every construction carries one; the
  destructurings gain `..`). Then, each on `seed_star_group`'s four mono frames with the test pool's one thread
  (admission 1), "log" = `PASS1_LOG` entries under the test's own tree, nothing referencing Step 2's API:
  - `measure_reruns_pass_two_…` extended: log 4 after run 1, 0 after run 2, **0 after run 3** (the β move); four
    `measured` rows after run 1, their `created_at` unchanged by runs 2–3 (bar 5); the run-3 own-β assertion stays,
    its doc now says the own β is the artifact's. HEAD: no rows, run 3 logs 4.
  - `measure_pass1_residency_is_the_admitted_workers`: `pass1_peak` of the group's calibrated dir is 1 over a fresh
    run and over a β-move run (bar 3). HEAD: 4 on the fresh run.
  - `measure_pass1_from_the_artifact_after_a_lost_pass_two`: run 1; the `metrics` and `fits.0` rows deleted (a run
    cancelled after pass 1) → run 2 logs 0, four `metrics` rows again, the group β's f64 bits equal. HEAD: 4.
  - `measure_pass1_artifact_of_a_changed_source_reruns_pass_one`: run 1; one calibrated file's mtime + 10 s
    (`File::set_modified`), every `fits.0` row deleted → run 2 logs exactly that file, excludes nothing. HEAD: 4.
  - `measure_pass1_corrupt_artifact_is_recomputed`: run 1; one byte of one `.athm` body flipped, `fits.0` rows
    deleted → run 2 logs exactly that frame, excludes nothing. HEAD: no `measured` row to corrupt (the setup's
    `expect`).
  - `measure_pass1_write_failure_recomputes_in_pass_two`: a file planted at `<root>/measured/<group>` → run 1: no
    `measured` row, four `metrics` rows, log 8, peak 1. HEAD: log 4, peak 4.
  - `measure_pass1_rerun_from_measure_recomputes_it`: run 1; run 2 with `rerun_from = Some(Stage::Measure)` → log 4,
    four `measured` rows. HEAD: no rows.
  - `measure_pass1_upgrade_runs_it_inside_pass_two_once`: run 1; `delete_artifacts(…, &["measured"])` and the
    directory removed; the β moved (the extended test's lowered-own-β edit) → run 2 logs 4 and leaves four rows; moved
    again (two other rows lowered) → run 3 logs 0. HEAD: no rows.
  Run: each fails for the HEAD reason named.

- [ ] **Step 2: RED, codec, key and lifecycle** (they do not compile at HEAD: the module, `pass1_hash_for`,
  `measured_bytes` are absent):
  - `stacking::pass1_artifact::tests`: `round_trips_a_mono_and_a_three_plane_first_pass_bit_exactly` (a real
    `measure_frame_pass1` on `measure.rs`'s `write` fixtures, mono and 3-plane, and the hand-built `for_pin` record
    with NaN, ±inf and a starless plane: re-encoding the read-back value gives the written bytes);
    `pass_two_from_a_read_back_first_pass_equals_pass_two_in_memory` (mono and 3-plane: two pass-1 runs, one through
    the file, each through `measure_frame_pass2` at one β — the `FrameMeasurement` minus `duration_ms` / `timings` and
    every plane's fits Debug-equal, `a_two_member_group_is_fitted_at_the_shared_beta`'s convention);
    `refuses_bad_magic_versions_checksum_and_truncation` (one typed arm each, never `Ok`);
    `the_head_reads_without_the_body` (a corrupt body: the head `Ok`, `read_pass1` `Checksum`; a flipped head byte: the
    head refused); `a_changed_source_reads_stale` (`Stale` with both stamps).
  - `stacking::plan::tests::pass1_hash_is_never_a_metrics_key`: ≠ `measurement_hash_for` at `Some(b)` for b ∈ {1.5,
    3.3066, 4.0} and at `None`; moves with `measurement.maxStars` and with the calib hash; a golden literal beside
    `stage_hashes_are_pinned_to_golden_literals`'; the extended run-level test's `measured` rows carry it.
  - `stacking::paths::tests::measured_bytes_are_counted_and_intermediates_or_all_remove_them` (the `rej_bytes_…`
    pattern: in `measured_bytes` and `total_bytes`; `Registered` keeps the directory and the rows, `Intermediates`
    and `All` remove both).
  - Pin (ruling F28, outside RED): `star_measure::tests::the_plane_prep_encoding_is_pinned` — `for_pin`'s postcard
    bytes, length and xxh3 literal (hand-built values, so no platform's libm enters); a layout change fails it until
    `PASS1_ARTIFACT_VERSION` moves.

- [ ] **Step 3: Implement** R11c-1…R11c-6 in the Files order (derives → codec → key → paths → flow), each file's
  filter green before the next. Then the pins (F28; green at HEAD and after):
  `measure_pass1_spill_is_byte_identical_fresh_and_from_the_artifact` (run 1 fresh; `fits.0` rows deleted; run 2 from
  the artifact; both against an in-memory `measure_frame_pass1` + `measure_frame_pass2` at the run's group β per frame:
  `.athf` bytes, the payload minus timings, the group β's bits, every `metrics` / `fits.0` key — bar 1) and
  `moffat4_measure_is_one_pass_and_unchanged` (+ no `measured` row, no `measured/`).

- [ ] **Step 4: Gates.** `cargo test -p athenaeum-core --lib -- stacking:: --test-threads=4` (835 + the new, 0 failed,
  2 ignored), `cargo test -p athenaeum-core --lib -- star_measure:: --test-threads=4` (172 + the pin),
  `cargo check -p athenaeum-core --all-targets` (`star_probe` keeps `PlanesPass1` in memory), `cargo test -p
  athenaeum-core --test ts_contract` (`TS_RS_WRITE=1` once for `measuredBytes`).

- [ ] **Step 5: Bar 4, the wall (dev Mac only; one heavy job at a time; `memory_pressure` ≥ 30 % free and `df` ≥ 100 GiB
  plus ≈ 10 GB before each run; Time Machine off — manual since 2026-10-04).** M42 = `~/.athenaeum-acc/star-lab-mono2`
  set 107 (QHY268M, 12 lights, `stackwork/M_42/`). (a) `clone-before.sh` → `~/.athenaeum-acc/t11c-A` and `t11c-B` (each
  a `cp -Rc` clone, its paths rewritten); A served from a worktree of ec39507e made as `rerun-build.sh` makes one
  (submodules included, no catalog step), B from this branch; both release servers built before the bracket. (b) The
  set's stored config read once (`get_stacking_config`; `measurement.psfModel` must read `auto`, printed): `base.json`
  without an exclusion, `excl.json` with the frame of lowest `betaOwn` in A1's `metrics` rows (read `immutable=1`). (c)
  The bracket A1 B1 A2 B2 A3 B3, each = (i) `ATH_ACC_SET_CONFIG=base.json ATH_ACC_RERUN_FROM=measure zsh
  tier1/tier1-run.sh <repo> <clone> <port> 107` then (ii) the same with `excl.json` and `ATH_ACC_RERUN_FROM=reference`
  (Measure not forced: its own cache decides), `tier1-extract.py <clone> --run-id N --measure-phases` after each; pair 3
  runs A3(i) B3(i) A3(ii) B3(ii), so each state is compared before the next run overwrites it. Timed: the Measure
  `StageTiming` (`tier1-extract.py`'s `stacking stage finished` line). Each (ii)'s `stacking_run_groups.beta` must
  differ from its (i)'s, else the next-lowest frame, noted. (d) Bars, into `bars.txt`: **4a** the median over the pairs
  of B(i) / A(i) ≤ 1.2; **4b** the median of B(ii) / A(ii) ≤ 1 − s/2, s = the median over A(i) of the pass-1 share
  (`--measure-phases`: from the Measure admission event to `measure: group beta resolved`, over the Measure wall), 1 − s
  printed as the expected ratio; **bar 1 on real data**, `tier1-compare.py --measure-identity` A3 vs B3 after (i) and
  after (ii): 0 differences. Reported: the `.athm` bytes per plane (mean, max), B's `measuredBytes`, each run's wall.
  (e) **Bar 1 on real colour data (owner, 2026-10-13; R11c-7).** After the M42 bracket, set 204
  (`~/.athenaeum-acc/tierA-lnB-f1`, before run 5: `osc__NoFilter__bin1__180s` 105 frames + `mono__NoFilter__bin1__180s`
  92; full-run wall 901 s) through `clone-before.sh` (a before tree, already on its allow-list) → `~/.athenaeum-acc/t11c-osc-A` /
  `t11c-osc-B`, served by the same two builds. Runs A(i) B(i) A(ii) B(ii): (i) `ATH_ACC_RERUN_FROM=measure` with the
  set's stored config (`measurement.psfModel` must read `auto`, printed); (ii) `ATH_ACC_RERUN_FROM=reference` with
  the OSC member of lowest `betaOwn` in A(i)'s rows excluded (read `immutable=1`), its group β checked to move (else
  the next-lowest, noted). Bar: `tier1-compare.py --measure-identity` A vs B after (i) and after (ii), **0
  differences on both groups** (in (ii) the mono group is a cache hit on both sides; the OSC group is pass 2 from the
  artifact on B, pass 1 + pass 2 on A). Information, not a bar: the OSC pass-1 share s, the (ii) ratio B / A, the
  `.athm` bytes per plane on OSC (mean, max), B's `measuredBytes`. (≈ 1 h, ≈ 90 GB transient.)
  (f) Every clone and the A worktree with its target deleted; the lab committed and pushed. A miss goes to the owner
  with its numbers, before Task 12.

- [ ] **Step 6: Docs; commit** (`one-star-path`, not pushed): R-OSP-9's 11c amendment (R11c-1…6, the gates, bars 1–5
  with Step 5's numbers), spec §9.3 / §9.5.

```
feat(stacking): F-4b pass-1 spill - every Auto member's Measure first pass written once to measured/<group>/<stem>.athm (postcard records behind a magic, two versions and two checksums; keyed on the calib hash + measurement subtree, never the group beta; the source stamp stored, a changed file re-runs pass 1); a group-beta move re-runs only pass 2, pass-1 residency = the admitted workers (M42 Measure 0.99x, exclusion re-run 0.27x against 1 - s/2 = 0.62)
```

**Acceptance:** Step 1's eight fail at HEAD for the reasons named and pass; Step 2's pass; both pins hold; the gates
green; on M42 bars 4a, 4b and bar 1 hold; on set 204 bar 1 holds on the OSC and the mono group, fresh and from the
artifact. Carries: **Task 12** — a `PlanePrep` field bumps `PASS1_ARTIFACT_VERSION`
(the pin fails until it does); **Task 13** — `clone-before.sh` (allow-list of four) and `ATH_ACC_RERUN_FROM` exist,
its M.1 Measure includes the spill, its three datasets report the `.athm` bytes per plane; **the release notes** — the
first run after the upgrade also writes `measured/` (≈ 1.9 % of the calibrated footprint: set 204, 0.94 GB beside 50 GB).

**Outcome (2026-10-14; branch `one-star-path-t11c`, 6abffeb0 → a6a28f39 review fixes → 89958c3f; lab c4d9b92; not
pushed).** Pass 1 now spills: every `Auto` member's Measure first pass is written once to
`measured/<group>/<stem>.athm` (R11c-1 … R11c-6: the `PlanePrep` stored and the options rebuilt through one builder,
postcard behind two checksums, a key independent of the group β, the source stamp, the controller/worker flow, the
`Intermediates` lifecycle) and read back by pass 2; a group-β move re-runs pass 2 only (I2) and pass-1 residency is the
admitted workers (the memory ruling). Review: 0 Critical / 2 Important / 3 Minor, all fixed (a6a28f39, 89958c3f).
**The bars** (`t11c-wall/bars.txt`): 4a M42 fresh Measure B / A median 0.9885 against 1.2 PASS; 4b exclusion re-run
0.2700 against 1 − s/2 = 0.6181 (s = 0.7638, expected 1 − s = 0.2362) PASS; bar 1 `--measure-identity` M42 (i) 0 /
(ii) 0 and set 204 (OSC 105 + mono 92, R11c-7) (i) 0 / (ii) 0 PASS. Set 204, information: fresh Measure 678.7 → 643.0 s;
exclusion re-run Measure 396.9 → 83.0 s (0.209), run wall 1021 → 691 s; OSC pass-1 share 0.533; `.athm` per plane 1.94 MB
OSC / 3.63 MB mono; `measured/` 0.94 GB beside `calibrated/` 50 GB = about 1.9 % (the design read's "about 1 %" was
low). Gates: `stacking::` 853 / 0 / 2 ignored, `star_measure::` 173, `check --all-targets` clean, `ts_contract`. Attempt 1
of the driver stopped on a lab-tool bug (fixed, lab 6a8d4bb). Limits (README R-OSP-9, the 11c amendment): pass 2 still
holds every member's `(FrameMeasurement, fits)` until persisted (C2); the golden pin is the only layout guard and
`RefusalReason`'s variant order is format (C3); the plan gate reads `metrics` only (C4); no space-estimate term (C5).
Carries: **Task 12** a `PlanePrep` field bumps `PASS1_ARTIFACT_VERSION`; **Task 13** the M.2 stage wall; **release
notes** the first run after the upgrade also writes `measured/`.

---

### Task 12: The Analysis mode and the budget — Plan C's interface, the cost bars (spec D-3, §2.1 `budget`, §3 budget and cost)

**Design read (2026-10-15, `task-12-report-design.md`).** Scope and bars as fixed on 2026-10-07. The Files, Interfaces,
tests and Steps are reconciled with 8bbcc135 (written before Tasks 4–11c). Where the code contradicted the plan, the
text below says "design read". Rulings:
- **R12-0 (owner):** every lab run is on the dev Mac (no runner). The base is the dev-Mac cycle `t12-base` at 8bbcc135.
- **R12-1 (owner, 2026-10-15):** under the Analysis default `PsfModel::Moffat4` there is NO own-β work of any kind: no
  β sample, no extra fit, and not the verdict fits' β either. Moffat 4 means zero extra work, `beta` = 4 and
  `beta_source` = `BetaSource::Fixed`. The frame's β exists only when the caller selects `PsfModel::Auto`. The existing
  verdict sample then runs on the budget's candidates and is reported as today. This replaces the 2026-10-13
  "own β shown as a diagnostic" for the Analysis mode.
- **R12-2 (where the budget lives):** only in `measure_plane`. The selection runs after detection and before the
  verdict pass, so both per-seed fits (verdict and at-β) run on the candidates only. `PlanePrep` is not changed. The
  stacking halves `prepare_plane`, `finish_plane` and `plane_beta` stay `Mode::All` only. So `PASS1_ARTIFACT_VERSION`
  stays 1, and `the_plane_prep_encoding_is_pinned`, unchanged, is the proof.
- **R12-3 (the selection):** cells, ranks, quota and round-robin under Interfaces.
- **R12-4 (the trim):** unflagged fits first, then a round-robin by SNR.
- **R12-5 (neighbours):** the ring sky's seed discs and `c`'s isolation read every detected seed, not only the fitted
  ones.
- **R12-6 (what a cell with measurable stars is):** see `budget::report`.
- **R12-7 (the probe):** no seed lines; `analysis-mode` runs alone, on the raw file; the cost walls are read without
  `--diagnostics`.

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/budget.rs`.
- Modify `star_measure/mod.rs`:
  - add `pub mod budget` and the re-exports;
  - add `Mode`, `MeasureOptions.mode` and `PlaneMeasurement.budget`;
  - split `prepare_working` into `prepare_head` (noise, mesh, sky, saturation plane, detection, noise correlation) and
    `prepare_passes` (verdict pass, clip floor, near-clip class). This is a pure restructuring: `Mode::All` stays
    byte-identical;
  - add `finish_working_with(.., neighbours: Option<fit::Neighbours>)`; `finish_working` calls it with `None`;
  - add `measure_plane`'s Budget branch;
  - `prepare_plane` reads no `mode`: with `Mode::Budget` it logs a `warn!` and measures every seed.
- Modify `star_measure/fit.rs`: `pub(super) struct Neighbours`, `Correction.neighbours`; `ring_and_correction` hands
  them to the ring sky and to `c`.
- Modify `star_measure/photometry.rs`: `aperture_correction` gains `own: Option<&[usize]>`. It has one call site.
- Modify `stacking/measure.rs`: `module_options`, the ONE exhaustive `sm::MeasureOptions` literal in the tree, gains
  `mode: sm::Mode::All`. Hand edit; never rustfmt this file.
- Modify `examples/star_probe.rs` (hand edits):
  - the path `analysis-mode`, `--clip C`, `--budget N` and `--psf moffat4|auto`;
  - the module doc's paragraph;
  - the `tests::args` helper.
- Docs:
  - `docs/stacking/README.md`: R-OSP-10, with R12-0…R12-7 and the bars;
  - spec §2.1: the `budget` row as built; spec §3: the budget "as built and as judged";
  - the logging spec's field dictionary gains `budget`, `candidates`, `kept`, `not_selected`, `trimmed`,
    `cells_with_stars` and `cells_covered`.
- Lab (athenaeum-lab `main`, committed and pushed): the L1 list in Step 3.
- Formatting: `rustfmt` by explicit path on the four `star_measure` files only.
- Earlier code and tests this task edits (ruling F14):
  - `stacking/measure.rs::module_options` (one field);
  - the probe's `tests::args` gains `clip`, `budget` and `psf` (each `None`);
  - lab `tests/test_run_probe.py::{test_plan_has_legacy_analysis_and_amode_off,
    test_inject_and_corpus_run_the_analysis_arm_as_legacy_analysis}`: the switch is on, and the corpus gains its amode
    job.

  No `star_measure::` or `stacking::` test changes: every literal there uses `..Default::default()`, and
  `Mode::default()` is `All`.
- Not changed:
  - `PlanePrep`, so `PASS1_ARTIFACT_VERSION` stays 1;
  - `MEASURE_VERSION` (PA-4: `Mode::All` moves no number);
  - every TS type: `BudgetReport` reaches no `ts_export` type in Plan A, so there is no `ts_contract` move;
  - every command.

**Interfaces:**

```rust
// star_measure/mod.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode { #[default] All, Budget { budget: usize } }
pub struct MeasureOptions { /* … */ pub mode: Mode }                     // Default: Mode::All (the stacking path)
pub struct PlaneMeasurement { /* … */ pub budget: Option<BudgetReport> }   // Some exactly under Mode::Budget

// star_measure/budget.rs (PA-10, ruling F40; design read R12-3…R12-6)
pub const DEFAULT_BUDGET: usize = 500;                          // D-3; Plan C's `maxStars` default reads it
pub const ANALYSIS_PSF_MODEL: PsfModel = PsfModel::Moffat4;     // R11b-12, R12-1: Plan C's and the probe's default
pub const CANDIDATE_FACTOR: f64 = 1.25;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetReport { pub cells_with_stars: u8, pub cells_covered: u8, pub candidates: usize, pub kept: usize,
                          pub not_selected: usize, pub trimmed: usize }
/// The 4 × 4 cell of a position: `4·cy + cx`, `cx = ⌊4x/w⌋`, `cy = ⌊4y/h⌋`, each clamped to [0, 3].
pub fn cell_of(x: f64, y: f64, w: usize, h: usize) -> u8;
/// The seeds to fit, ascending (= rank order), and the per-cell seed counts (R12-3).
pub fn select_candidates(seeds: &[Seed], w: usize, h: usize, budget: usize) -> (Vec<usize>, [u32; 16]);
/// The fits to remove, ascending, so that at most `budget` remain; `cells[k]` = the cell of fit k's SEED (R12-4).
pub fn trim(fits: &[StarFit], cells: &[u8], budget: usize) -> Vec<usize>;
/// The report (R12-6) from every seed's cell, the candidates, the accepted fits before the trim and the kept fits
/// (seed indices), and whether a plane verdict (defocused / trailed) emptied the plane.
pub fn report(seed_cells: &[u8], candidates: &[usize], accepted: &[usize], kept: &[usize], plane_verdict: bool)
    -> BudgetReport;

// star_measure/fit.rs: the budget's fitted subset still masks and isolates against every seed (R12-5)
#[derive(Debug, Clone, Copy)]   // `Correction` is `Copy`
pub(super) struct Neighbours<'a> { pub(super) seeds: &'a [Seed], pub(super) of_fit: &'a [usize] }
pub(super) struct Correction<'a> { /* … */ pub(super) neighbours: Option<Neighbours<'a>> }  // None = the fitted seeds
// star_measure/photometry.rs: `own[k]` = the index into `seeds` of fit k's seed; None = the identity
pub(crate) fn aperture_correction(/* data, mask, w, h, fits, seeds, */ own: Option<&[usize]> /* , field_fwhm, … */)
    -> (Option<f64>, usize);
```

**The rules (deterministic; every tie-break named).**
- **Cell.** Every cell assignment uses `cell_of` of the SEED's position: the selection, the trim and the report alike.
  So coverage can never exceed 1. A fit that moved across a cell edge keeps its seed's cell.
- **R12-3, selection.**
  - Rank = seed index: the detector hands seeds sorted by matched-filter integrated signal (`flux`) descending, as a
    stable sort; given seeds count as brightest-first.
  - Quota `q = ⌈1.25·N/16⌉`; target `T = 16·q` (640 at N = 500).
  - Water-fill by round-robin: seed i's key is (its rank within its own cell, i), and the candidates are the first
    `min(T, seeds)` keys. In each round every cell that still holds seeds gives its next brightest. A short cell's
    unused share passes to the other cells in later rounds. The last partial round goes to the cells whose next seed
    ranks first.
  - Returned ascending, so the fit sees the candidates in rank order. With N ≥ seeds, every seed is a candidate and
    the result is `Mode::All`'s.
  - N = 0: nothing is selected, every seed is `NotSelected`, and there is one `warn!`.
  - Cost: one sort of at most 24 576 keys, under 2 ms.
  - Design read (PA-10's "round-robin by rank when a cell holds fewer" made exact): without the refill, test 2's
    field fits 31 of its 200.
- **R12-4, trim.** Tier 0 = `flags.in_statistic()` (neither saturated nor extended); tier 1 = the others. A fit's
  round r = its rank within (cell, tier), ordered by SNR descending (NaN last), then fit index. Keep the first N of
  (tier, r, SNR desc with NaN last, fit index). Ties fall to the lower fit index, which is the duplicate drop's
  amplitude order.
  - So flagged fits count against N when kept. They fill only the slots no unflagged fit wants (D-3: "good
    unsaturated stars").
  - PA-10's plain SNR round-robin would keep saturated stars first: they have the highest SNR, and the frame
    statistic ignores them.
- **R12-5, neighbours.** The ring sky masks the 3F disc of EVERY detected seed, and `c`'s isolation tests every seed
  (`Correction.neighbours = Some(Neighbours { seeds: S, of_fit: C })`). Then a candidate's B_i, σ_i, SNR and HFR are
  what `Mode::All` reads at the same ring width. On m42, about 97 % of the seeds are not candidates. Every other
  caller (LN's refits) passes `None`.
- **R12-6, the report.**
  - `cells_with_stars` = the cells holding an accepted fit before the trim, plus the cells holding a `NotSelected`
    seed and no accepted candidate (cells the budget might have covered).
  - `cells_covered` = the cells holding a kept fit.
  - A plane whose verdict is defocused or trailed has 0 / 0: by the module's own verdict it holds no measurable star,
    and the scorecard skips a 0-cell image (`rows_budget_coverage` divides only where `cellsWithStars > 0`).
  - `candidates` = |C|, `kept` = |stars|, `not_selected` = seeds − |C|, `trimmed` = accepted − kept.
  - Design read: PA-10's literal "any seed above threshold" would score the module's own refusals (a cell whose only
    seed is an edge star or a blend) as budget misses; a defocused donut image would read 0 / 16.
- **The flow under `Mode::Budget`** (`measure_plane` only, R12-2):
  1. `prepare_head` → all seeds S and the full detection report R.
  2. `select_candidates` → C.
  3. `prepare_passes` on S[C]. R is sub-indexed (`extended`, `filter_snr`) for the fit. The fit's field width is the
     head's, unchanged. The near-clip floor comes from C's verdicts.
  4. β from `opts.psf_model` as given: `Moffat4` 4 with no sample (R12-1); `Auto` the verdict sample over C; `Fixed(b)`
     b.
  5. The fits at β on S[C], the refusals, the plane verdicts (over C), the duplicate drop, the ring sky and `c` with
     the neighbours, SNR and HFR.
  6. `trim`: the removed fits' outcome becomes `Outcome::Trimmed`; their diagnostics records are kept.
  7. Assemble:
     - `stars` = the kept fits in the fit call's order; `seeds` = |S|; `detected` = S; `detection` = R (every seed);
     - `refused` = the fit call's `by_reason`, plus `not_selected` and `trimmed`;
     - `diagnostics` (when asked): one per seed of S, in seed order. C's records are re-indexed to S, and
       `Accepted { star }` is renumbered after the trim. A `NotSelected` record carries the not-reached values:
       `region_radius` 0, NaN starts, 0 steps and pixels, no iterations, `converged` false, `verdict` and
       `sample_beta` `None`;
     - over the kept fits: `frame`, `snr_missing`, `ring_sky_missing`;
     - the fit call's, over C: `aperture_correction`, `aperture_stars`, `trail_share`, `defocus_donuts`,
       `verdict_fit_failed`, `clip_floor`, `clip_evidence`;
     - `budget` = `Some(report)`.
  8. One `debug!` carries the report fields.
  - Multi-plane inputs: one plane at a time, with no cross-plane rule (Plan C's raw input is one plane).

- [x] **Step 1: Write the failing tests** (`budget.rs`, `mod.rs`, the probe).
  - Every new test fails to compile at 8bbcc135 (no `Mode`, `budget`, `Neighbours`, `own`, nor the probe path).
    That is their RED; each one's reason is in its doc line.
  - Pins (ruling F28, outside RED): `the_plane_prep_encoding_is_pinned` and
    `every_seed_has_exactly_one_outcome_and_the_numbers_do_not_move`, both unchanged, plus the new
    `the_default_mode_is_all`.

`budget.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn seed(x: f64, y: f64) -> Seed { Seed { x, y, peak: 1.0, flux: 1.0 } }
    /// A seed in the middle of cell `c` of a 400² plane (100-px cells).
    fn in_cell(c: u8) -> Seed { seed(50.0 + 100.0 * (c % 4) as f64, 50.0 + 100.0 * (c / 4) as f64) }
    fn fit(snr: f64, flags: StarFlags) -> StarFit {
        StarFit { x: 0.0, y: 0.0, background: 0.0, amplitude: 1.0, fwhm_x: 2.0, fwhm_y: 2.0, fwtm_x: 4.0, fwtm_y: 4.0,
                  theta: 0.0, beta: 4.0, residual: 0.0, signal: 1.0, area: 1.0, peak5: f64::NAN, sky_ring: 0.0,
                  sky_ring_sigma: 1.0, sky_ring_n: 10, snr, hfr: 1.0, flags }
    }

    #[test]
    fn a_cell_is_the_4x4_grid_index_of_the_position() {
        assert_eq!(cell_of(0.0, 0.0, 400, 400), 0);
        assert_eq!(cell_of(99.999, 0.0, 400, 400), 0);
        assert_eq!(cell_of(100.0, 0.0, 400, 400), 1);
        assert_eq!(cell_of(399.5, 399.5, 400, 400), 15);
        assert_eq!(cell_of(-0.5, 400.0, 400, 400), 12, "clamped into the grid");
        assert_eq!(cell_of(250.0, 150.0, 400, 300), 10);
    }

    /// R12-3: budget 64 → quota 5, target 80.
    #[test]
    fn every_cell_gets_its_quota_by_rank_and_a_short_cell_hands_its_share_on() {
        let full: Vec<Seed> = (0..160).map(|i| in_cell((i % 16) as u8)).collect();   // 10 per cell, interleaved by rank
        let (c, n) = select_candidates(&full, 400, 400, 64);
        assert_eq!(n, [10; 16]);
        assert_eq!(c, (0..80).collect::<Vec<_>>(), "the 5 brightest of every cell");
        let mut short: Vec<Seed> = (0..100).map(|_| in_cell(0)).collect();           // ranks 0..99 in cell 0
        short.extend((0..30).map(|i| in_cell(1 + (i / 2) as u8)));                   // ranks 100..129: two per cell 1..=15
        let (c, n) = select_candidates(&short, 400, 400, 64);
        assert_eq!(n[0], 100);
        assert!(n[1..].iter().all(|&k| k == 2));
        assert_eq!(c.len(), 80);
        assert!((100..130).all(|i| c.contains(&i)), "every short cell's seeds are candidates");
        assert_eq!(c[..50], (0..50).collect::<Vec<_>>()[..], "cell 0 takes the 50 slots left, brightest first");
        assert!(c.windows(2).all(|p| p[0] < p[1]), "ascending = rank order");
    }

    /// R12-3's tie-break: budget 25 → quota 2, target 32. Rounds 1–9 take 30 (nine each in cells 0–2, three in cell 3).
    /// Round 10 has 2 slots for cells 0, 1 and 2, whose tenth seeds rank 32, 31 and 30: cells 2 and 1 take them.
    #[test]
    fn the_last_partial_round_goes_to_the_cells_whose_next_seed_ranks_first() {
        let mut s: Vec<Seed> = (0..27).map(|r| in_cell((r % 3) as u8)).collect();
        s.extend((0..3).map(|_| in_cell(3)));
        s.extend([2u8, 1, 0, 0, 1, 2].map(in_cell));
        let (c, _) = select_candidates(&s, 400, 400, 25);
        assert_eq!(c, (0..32).collect::<Vec<_>>());
        assert_eq!(select_candidates(&s, 400, 400, 25).0, c, "the same input, the same selection");
    }

    #[test]
    fn a_budget_at_or_above_the_seed_count_takes_every_seed_and_zero_takes_none() {
        let s: Vec<Seed> = (0..40).map(|i| in_cell((i * 7 % 16) as u8)).collect();
        assert_eq!(select_candidates(&s, 400, 400, 40).0, (0..40).collect::<Vec<_>>());
        assert_eq!(select_candidates(&s, 400, 400, 1000).0.len(), 40);
        assert!(select_candidates(&s, 400, 400, 0).0.is_empty());
        assert!(trim(&vec![fit(10.0, StarFlags::default()); 5], &[0; 5], 5).is_empty());
    }

    /// R12-4. Cell 0 holds SNR 50 (saturated), 40, 30, 30, 20; cell 1 holds 25 and NaN; cell 2 holds 5 (extended).
    /// Unflagged first: round 1 is 40 and 25; round 2 is fit 2 (the 30-tie to the lower index) and the NaN; then
    /// 30 and 20; the flagged come last.
    #[test]
    fn the_trim_keeps_unflagged_fits_first_round_robin_by_snr() {
        let ok = StarFlags::default();
        let fits = [fit(50.0, StarFlags::SATURATED), fit(40.0, ok), fit(30.0, ok), fit(30.0, ok), fit(20.0, ok),
                    fit(25.0, ok), fit(f64::NAN, ok), fit(5.0, StarFlags::EXTENDED)];
        let cells = [0, 0, 0, 0, 0, 1, 1, 2];
        assert!(trim(&fits, &cells, 8).is_empty());
        assert_eq!(trim(&fits, &cells, 6), vec![0, 7], "both flagged go before any unflagged fit");
        assert_eq!(trim(&fits, &cells, 4), vec![0, 3, 4, 7], "round-robin: cell 1's NaN (round 2) before fit 3 (round 3)");
        assert_eq!(trim(&fits, &cells, 3), vec![0, 3, 4, 6, 7]);
    }

    /// R12-6. Seeds lie in cells 0, 0, 1, 2 and 3; the candidates are seeds 0, 2 and 3; the accepted fits are seeds 0
    /// and 3; seed 0 is kept.
    #[test]
    fn the_report_counts_the_cells_the_budget_could_have_covered() {
        let cells = [0u8, 0, 1, 2, 3];
        let r = report(&cells, &[0, 2, 3], &[0, 3], &[0], false);
        // cell 0: with stars, covered; cell 1: its only candidate refused, nothing unexamined, not counted; cell 2: accepted then
        // trimmed, with stars but not covered; cell 3: no candidate, seed 4 not selected, with stars
        assert_eq!((r.cells_with_stars, r.cells_covered), (3, 1));
        assert_eq!((r.candidates, r.kept, r.not_selected, r.trimmed), (3, 1, 2, 1));
        assert_eq!(report(&cells, &[0, 2, 3], &[], &[], true).cells_with_stars, 0, "a plane verdict: no measurable star");
    }
}
```

`mod.rs` (the plan's three, kept as written 2026-10-07 — they read the code at 8bbcc135 correctly — plus six):

```rust
// `moffat_field`, `MoffatStar` and `fixture_field` are already imported at the top of `mod tests` (8bbcc135 l. 1138-1139).
fn uniform(n: usize, w: usize, h: usize, seed: u64) -> Vec<MoffatStar> {
    let mut rng = crate::geometry::ransac::SplitMix64(seed);
    (0..n).map(|_| MoffatStar { x: 20.0 + rng.next_f64() * (w as f64 - 40.0), y: 20.0 + rng.next_f64() * (h as f64 - 40.0),
        amp: 0.05 + 0.3 * rng.next_f64(), alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 }).collect()
}

/// The 1024² field with pseudo-noise, as the probe's own fixture adds it (ruling F29): the MRS noise estimate needs
/// some, and a noiseless plane is a refused noise model that seeds nothing under `SeedSpec::MatchedFilter` (Review
/// Focus 4).
fn noisy_field(stars: &[MoffatStar], noise_seed: u64) -> Vec<f32> {
    let mut data = moffat_field(1024, 1024, stars, 4.0, 0.05);
    crate::test_support::add_noise(&mut data, 0.002, noise_seed);
    data
}

fn budget_opts(budget: usize) -> MeasureOptions {
    MeasureOptions { mode: Mode::Budget { budget }, seeds: SeedSpec::MatchedFilter { threshold: seeds::DEFAULT_THRESHOLD },
                     ..Default::default() }
}

#[test]
fn budget_covers_every_cell_that_holds_a_star() {
    let stars = uniform(800, 1024, 1024, 11);
    let data = noisy_field(&stars, 11);
    let m = measure_plane(&data, 1024, 1024, &budget_opts(500), None);
    let b = m.budget.unwrap();
    assert!(m.stars.len() <= 500 && m.stars.len() >= 480, "{}", m.stars.len());
    assert_eq!(b.cells_with_stars, 16);
    assert_eq!(b.cells_covered, 16);
    assert_eq!(m.stars.len() + m.refused.total() + m.refused.not_selected + m.refused.trimmed, m.seeds);
}

#[test]
fn budget_keeps_the_brightest_per_cell_when_one_cell_dominates() {
    let mut stars = uniform(600, 256, 256, 3);                              // all inside the top-left cell of a 1024² plane
    stars.extend((0..15).map(|i| MoffatStar { x: 300.0 + (i % 5) as f64 * 150.0, y: 300.0 + (i / 5) as f64 * 200.0, amp: 0.2, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 }));
    let data = noisy_field(&stars, 3);
    let m = measure_plane(&data, 1024, 1024, &budget_opts(200), None);
    let b = m.budget.unwrap();
    assert_eq!(b.cells_covered, b.cells_with_stars, "every cell with a star keeps one");
    let outside = m.stars.iter().filter(|f| f.x >= 256.0 || f.y >= 256.0).count();
    assert!(outside >= 14, "the sparse cells' stars are kept: {outside}");
    assert!(m.stars.len() <= 200);
}

#[test]
fn budget_mode_accounts_for_every_seed() {
    let stars = uniform(900, 1024, 1024, 5);
    let data = noisy_field(&stars, 5);
    let m = measure_plane(&data, 1024, 1024, &MeasureOptions { diagnostics: true, ..budget_opts(500) }, None);
    let b = m.budget.unwrap();
    assert!(b.not_selected > 0, "900 seeds, 1.25 × 500 / 16 per cell: many are never fitted");
    assert_eq!(m.diagnostics.iter().filter(|d| d.outcome == Outcome::NotSelected).count(), b.not_selected);
    assert_eq!(m.diagnostics.iter().filter(|d| d.outcome == Outcome::Trimmed).count(), b.trimmed);
    assert_eq!(m.stars.len() + m.refused.total() + b.not_selected + b.trimmed, m.seeds);
    assert_eq!(m.refused.not_selected, b.not_selected);
    assert!(m.diagnostics.iter().enumerate().all(|(i, d)| d.seed == i), "one record per detected seed, in seed order");
}

/// R12-2 / R12-3: a budget no smaller than the seed count selects every seed and trims nothing. The measurement is then
/// `Mode::All`'s bit for bit (Debug text, so NaN fields compare; the wall-clock timings set equal), on given and
/// matched-filter seeds, under both PSF models.
#[test]
fn a_budget_at_or_above_the_seed_count_is_mode_all_bit_for_bit() {
    let (fixture, fw, fh) = crate::test_support::fixture_field(7, 1.0, 0.002);
    let given = test_seeds_of(&fixture, fw, fh);
    let noisy = noisy_field(&uniform(300, 1024, 1024, 9), 9);
    for (plane, w, h, spec) in [(&fixture, fw, fh, SeedSpec::Given(given)),
                                (&noisy, 1024, 1024, SeedSpec::MatchedFilter { threshold: seeds::DEFAULT_THRESHOLD })] {
        for psf_model in [PsfModel::Auto, PsfModel::Moffat4] {
            let all = MeasureOptions { seeds: spec.clone(), psf_model, diagnostics: true, ..Default::default() };
            let a = measure_plane(plane, w, h, &all, None);
            let mut b = measure_plane(plane, w, h, &MeasureOptions { mode: Mode::Budget { budget: a.seeds }, ..all.clone() }, None);
            let r = b.budget.take().expect("a budget report");
            assert_eq!((r.not_selected, r.trimmed, r.candidates, r.kept), (0, 0, a.seeds, a.stars.len()));
            b.timings = a.timings;
            assert_eq!(format!("{b:?}"), format!("{a:?}"));
        }
    }
}

/// Determinism (no order-dependent container in the selection, the trim or the re-indexing): two runs, one result.
#[test]
fn the_budget_measurement_is_deterministic() {
    let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
    let opts = MeasureOptions { diagnostics: true, ..budget_opts(500) };
    let a = measure_plane(&data, 1024, 1024, &opts, None);
    let mut b = measure_plane(&data, 1024, 1024, &opts, None);
    b.timings = a.timings;
    assert_eq!(format!("{b:?}"), format!("{a:?}"));
}

/// F13 with the diagnostics off (the Analysis tab's setting): the counts are kept in `refused` and the report, and
/// asking for the per-seed records moves no number.
#[test]
fn the_budget_identity_holds_without_diagnostics_and_they_move_no_number() {
    let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
    let off = measure_plane(&data, 1024, 1024, &budget_opts(500), None);
    let on = measure_plane(&data, 1024, 1024, &MeasureOptions { diagnostics: true, ..budget_opts(500) }, None);
    let b = off.budget.unwrap();
    assert!(off.diagnostics.is_empty());
    assert_eq!(off.refused.accounted(off.stars.len()), off.seeds);
    assert_eq!((off.refused.not_selected, off.refused.trimmed), (b.not_selected, b.trimmed));
    assert_eq!((b.kept, b.candidates), (off.stars.len(), off.seeds - b.not_selected));
    assert_eq!(format!("{:?}", off.stars), format!("{:?}", on.stars), "diagnostics change no number");
}

/// R12-1 (owner): under the Analysis default, Moffat 4 is zero extra work. Every fit is at 4, the source is the
/// caller's, and no record carries a sample β. `Auto` runs the verdict sample on the candidates only.
#[test]
fn budget_with_moffat4_fits_at_four_and_runs_no_beta_sample() {
    let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
    let opts = |psf_model| MeasureOptions { psf_model, diagnostics: true, ..budget_opts(500) };
    assert_eq!(budget::ANALYSIS_PSF_MODEL, PsfModel::Moffat4);
    let m = measure_plane(&data, 1024, 1024, &opts(budget::ANALYSIS_PSF_MODEL), None);
    assert_eq!((m.beta, m.beta_source), (4.0, BetaSource::Fixed));
    assert!(m.diagnostics.iter().all(|d| d.sample_beta.is_none()), "R12-1: no beta sample under Moffat 4");
    assert!(m.stars.iter().all(|f| f.beta == 4.0 && !f.flags.beta_at_cap()));
    let a = measure_plane(&data, 1024, 1024, &opts(PsfModel::Auto), None);
    assert!(matches!(a.beta_source, BetaSource::Fitted { .. }), "{:?}", a.beta_source);
    let sampled: Vec<&FitDiagnostics> = a.diagnostics.iter().filter(|d| d.sample_beta.is_some()).collect();
    assert!(!sampled.is_empty());
    assert!(sampled.iter().all(|d| d.outcome != Outcome::NotSelected), "Auto's sample is the candidates'");
}

/// R12-5: the budget fits a subset. The ring sky still leaves out every seed's 3F disc, and `c`'s isolation still
/// reads every seed, exactly as when all of them are fitted. In working units at a given ring width and field width,
/// so the two calls fit A identically.
#[test]
fn the_ring_sky_and_the_isolation_read_every_seed_not_only_the_fitted_ones() {
    let (f_ring, field) = (2.6, Some(2.61));
    let star = |x: f64, y: f64, amp: f64| MoffatStar { x, y, amp, alpha_x: 3.0, alpha_y: 3.0, theta: 0.0 };
    let run = |stars: &[MoffatStar], fitted: usize, neighbours: bool| {
        let mut data = moffat_field(200, 200, stars, 4.0, 0.05);
        crate::test_support::add_noise(&mut data, 0.002, 7);
        let data: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp * ADU_SCALE as f64, flux: s.amp }).collect();
        let of_fit: Vec<usize> = (0..fitted).collect();
        let n = neighbours.then(|| fit::Neighbours { seeds: &seeds, of_fit: &of_fit });
        fit::fit_stars_with_beta_diagnosed(&data, 200, 200, &seeds[..fitted], 4.0, &FitParams::default(), None, field,
            None, false, false, false, fit::Correction { ring_fwhm: Some(f_ring), neighbours: n, ..Default::default() })
    };
    // B 11·F from A: inside A's [10F, 12F] ring.
    let ring = [star(100.0, 100.0, 0.3), star(100.0 + 11.0 * f_ring, 100.0, 0.3)];
    let (both, alone, with_n) = (run(&ring, 2, false), run(&ring, 1, false), run(&ring, 1, true));
    let a = both.fits.iter().find(|f| (f.x - 100.0).abs() < 1.0).unwrap();
    assert_eq!(with_n.fits[0].sky_ring.to_bits(), a.sky_ring.to_bits(), "B's disc is left out of A's ring");
    assert!(alone.fits[0].sky_ring > a.sky_ring, "without it, B lifts A's ring sky");
    // C (brighter, so the clip screen keeps A) 12 px from A: within 6 · field width, so A is not isolated.
    let iso = [star(100.0, 100.0, 0.3), star(100.0, 112.0, 0.6)];
    assert_eq!(run(&iso, 1, false).aperture_stars, 1, "alone, A qualifies");
    assert_eq!(run(&iso, 1, true).aperture_stars, 0, "with its neighbour, A is not isolated");
}

/// A pin (ruling F28): the default is the stacking path, so every existing literal stays on it.
#[test]
fn the_default_mode_is_all() {
    assert_eq!(MeasureOptions::default().mode, Mode::All);
    let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
    let m = measure_plane(&data, w, h, &MeasureOptions { seeds: SeedSpec::Given(test_seeds_of(&data, w, h)), ..Default::default() }, None);
    assert!(m.budget.is_none());
    assert_eq!(m.refused.not_selected + m.refused.trimmed, 0);
}
```

The probe (`examples/star_probe.rs` tests; `args` gains `clip: None, budget: None, psf: None`):

```rust
fn amode_args(img: PathBuf, out: PathBuf, budget: usize) -> MeasureArgs {
    MeasureArgs { domain: Domain::Adu, diagnostics: true, clip: Some(65535.0), budget: Some(budget),
                  ..args(img, out, &["analysis-mode"]) }
}

fn amode_frame(dir: &Path, a: impl FnOnce(PathBuf, PathBuf) -> MeasureArgs) -> (Vec<Value>, Value) {
    let data: Vec<f32> = gaussian_field(256, 256, 0, true).iter().map(|v| (v * 65535.0).round()).collect();
    let img = dir.join("p.fits");
    write_fits_f32(&img, 256, 256, 1, &data, &[]).unwrap();
    let out = dir.join("p.amode.jsonl");
    cmd_measure(a(img, out.clone())).unwrap();
    let lines = read_lines(&out);
    let f = lines.iter().find(|v| v["kind"] == "frame").cloned().unwrap();
    (lines, f)
}

/// Task 12 (R12-7, R12-1). On 256² with 40 stars in 12 cells and budget 16 (quota 2, target 32), there are 8 seeds not
/// selected and about 16 trimmed. No seed line is written (the lab merges every file's seed lines into the measure
/// path's). The frame line carries the counts, the report, the effective seed source and Moffat 4.
#[test]
fn analysis_mode_writes_the_budget_report_and_no_seed_line() {
    let d = tempfile::tempdir().unwrap();
    let (lines, f) = amode_frame(d.path(), |i, o| amode_args(i, o, 16));
    let n = |v: &Value| v.as_u64().unwrap();
    let (b, r) = (&f["budget"], &f["refused"]);
    assert!(lines.iter().all(|v| v["kind"] != "seed" && v["path"] == "analysis-mode"));
    assert_eq!(lines.iter().filter(|v| v["kind"] == "frame").count(), 1);
    assert_eq!(n(&f["fits"]) + n(&r["total"]) + n(&r["notSelected"]) + n(&r["trimmed"]), n(&f["seeds"]));
    assert_eq!((n(&r["notSelected"]), n(&r["trimmed"])), (n(&b["notSelected"]), n(&b["trimmed"])));
    assert_eq!(n(&b["candidates"]) + n(&b["notSelected"]), n(&f["seeds"]));
    assert_eq!(n(&b["kept"]), n(&f["fits"]));
    assert!(n(&f["fits"]) <= 16 && n(&b["notSelected"]) > 0 && n(&b["trimmed"]) > 0, "{f}");
    assert_eq!(b["cellsCovered"], b["cellsWithStars"]);
    assert_eq!(lines.iter().filter(|v| v["kind"] == "refused").count() as u64, n(&f["seeds"]) - n(&f["fits"]));
    assert_eq!(lines.iter().filter(|v| v["reason"] == "notSelected").count() as u64, n(&b["notSelected"]));
    let m = &f["options"]["measure"];
    assert_eq!((m["seedSource"].as_str(), m["seedThreshold"].as_f64()), (Some("matchedFilter"), Some(6.0)));
    assert_eq!((m["psfModel"].as_str(), m["budget"].as_u64()), (Some("moffat4"), Some(16)));
    assert_eq!((f["beta"].as_f64(), f["betaSource"].as_str()), (Some(4.0), Some("fixed")));
    assert!(f.get("betaOwn").is_none(), "R12-1: no own beta under Moffat 4");
    assert_eq!((f["domain"].as_str(), f["conversion"].as_str()), (Some("adu"), Some("none")));
}

#[test]
fn analysis_mode_with_psf_auto_reports_the_fitted_beta() {
    let d = tempfile::tempdir().unwrap();
    let (_, f) = amode_frame(d.path(), |i, o| MeasureArgs { psf: Some(PsfModel::Auto), ..amode_args(i, o, 16) });
    assert_eq!(f["options"]["measure"]["psfModel"], "auto");
    assert!(matches!(f["betaSource"].as_str(), Some("fitted" | "fallback")), "{f}");
    assert!(f["beta"].as_f64().unwrap().is_finite());
}

/// R12-7: the analysis-mode path runs alone, on a raw-domain file, with the matched filter, and a float file needs
/// `--clip`. Each refusal is an `Err` before any output file exists.
#[test]
fn analysis_mode_is_refused_before_any_output_when_its_input_is_not_the_analysis_tabs() {
    let d = tempfile::tempdir().unwrap();
    let field = gaussian_field(128, 128, 0, true);
    let raw: Vec<f32> = field.iter().map(|v| (v * 65535.0).round()).collect();
    let (ri, ui, out) = (d.path().join("r.fits"), d.path().join("u.fits"), d.path().join("o.jsonl"));
    write_fits_f32(&ri, 128, 128, 1, &raw, &[]).unwrap();
    write_fits_f32(&ui, 128, 128, 1, &field, &[]).unwrap();
    let a = || amode_args(ri.clone(), out.clone(), 16);
    for (args, want) in [
        (MeasureArgs { paths: vec!["analysis-mode".into(), "measure".into()], ..a() }, "runs alone"),
        (MeasureArgs { domain: Domain::Unit, ..a() }, "raw"),
        (amode_args(ui.clone(), out.clone(), 16), "raw-domain"),
        (MeasureArgs { seeds: Some(SeedChoice::Peak), ..a() }, "matched filter"),
        (MeasureArgs { clip: None, ..a() }, "--clip"),
    ] {
        let err = cmd_measure(args).unwrap_err();
        assert!(err.contains(want), "{err}");
        assert!(!out.exists(), "no output file for a refused invocation");
    }
}
```

- [x] **Step 2: Run, fail, implement, run.** Order:
  1. `budget.rs`'s pure functions → its six tests;
  2. `Neighbours` and `own` → `the_ring_sky_and_the_isolation_…`, with the whole `star_measure::` suite unchanged;
  3. the `prepare_working` split, then `the_default_mode_is_all` and the full `star_measure::` and `stacking::` suites
     before any Budget code (the `Mode::All` refactor proven on its own);
  4. `Mode`, `budget` and the Budget branch → the other mod.rs tests;
  5. `module_options`;
  6. the probe (below) → its three tests.

  **The probe's `analysis-mode` path (R12-7):**
  - Checked before any output file, each an `Err`:
    - the path runs alone (its planes are read raw: no `normalise`);
    - no `--rect`, `--debayer`, `--config` or `--psf-beta`;
    - `--domain` is `adu` or `auto`, never `unit`;
    - `raw_max > 1.5` (`analysis_domain_ok`);
    - `--seeds` absent or `matched`.
  - The domain is `Domain::RawAdu`:
    - `full_scale` = 2^bits − 1 from the header's BITPIX / BZERO (signed: 2^(bits−1) − 1); a float file has `--clip`;
    - `clip` = `--clip`, else `DATAMAX` > 0, else `full_scale`;
    - `gain_e_per_adu` = the header's `EGAIN`, else `None`.
  - `--budget` defaults to `budget::DEFAULT_BUDGET` (a value under 1 exits 1). `--psf moffat4|auto` (`MeasureArgs.psf:
    Option<PsfModel>`) defaults to `budget::ANALYSIS_PSF_MODEL`.
  - `pixel_noise`: as the flag says, else white for 1 plane and mixed for 3; `colour` = 3 planes. Each selected plane
    gets `star_measure::measure_plane` with `SeedSpec::MatchedFilter { threshold: --seed-threshold or
    DEFAULT_THRESHOLD }`, on its own.
  - Lines:
    - NO seed lines;
    - star lines `fit_line("analysis-mode", …)` plus `apSum` / `apN`; with `--diagnostics` also `seed` (the detected
      seed's index), `filterSnr`, `verdict` and, under Auto, `sampleBeta`;
    - with `--diagnostics`, `refused_line("analysis-mode", …)` for every record not accepted (reasons include
      `notSelected` and `trimmed`).
  - One frame line per plane, with the keys below. There is no `betaOwn` or `betaOwnAtCap` (R12-1).

    | Group | Keys |
    | ---- | ---- |
    | Identity | `kind: "frame"`, `path: "analysis-mode"`, `plane` |
    | β | `beta`, `betaSource`, `betaSample` |
    | Counts | `seeds`, `fits`, `refused` (`refused_counts`), `budget` (`BudgetReport`, camelCase) |
    | Frame statistic | `statistic` = "residual-weighted mean over the kept fits neither saturated nor extended", `starsUnsaturated`, `mostlySaturated`, `verdictFitFailed`, `colour` |
    | Correction and plane verdicts | `apertureCorrection`, `apertureStars`, `ringSkyMissing`, `clipFloor`, `clipEvidence`, `trailed`, `trailShare`, `defocused`, `frameFwhm`, `frameEcc` |
    | Detection | `sigmaF`, `sigmaFMeasured`, `noiseGain`, `pixelNoise`, `structureDropped`, `extendedSeeds`, `fieldWidth`, `fieldWidthSpread`, `fieldWidthEnvelope` |
    | SNR | `snrGain`, `snrMissing`, `snrMedian`, `snrNoise`, `shotSpread`, `hfrMedian` |
    | Timings | `timings {backgroundMs, noiseMs, detectMs, fitMs}` (the module's phases, so a cost miss can be attributed) |
    | Options | `options.measure {seedSource: "matchedFilter", seedThreshold, psfModel, budget, clip, fullScale, gainEPerAdu, maxStars}`, `options.rawMax` |
    | Read | `conversion: "none"`, `domain`, `nonFinite`, `wallMs` |

  - Energies are ÷ 65535, the lab's units, as on every line.
  - The reader stays `PlaneReader`: XISF and `read_raw` are Plan C's.
  - Gates (`CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4`, nothing else heavy running):
    - `cargo test -p athenaeum-core --lib -- star_measure:: --test-threads=4`;
    - `cargo test -p athenaeum-core --lib -- stacking:: --test-threads=4`;
    - `cargo test -p athenaeum-core --example star_probe -- --test-threads=4`;
    - `cargo check -p athenaeum-core --all-targets`;
    - `cargo check -p athenaeum-core --no-default-features`;
    - no `ts_contract` (no TS type moved).

- [x] **Step 3: Lab tools (L1), then the cycle `t12-amode`** (dev Mac, R12-0; one heavy job at a time;
  `memory_pressure` ≥ 30 % free; ≥ 100 GiB free).
  - **Order:** commit L1 only after `t12-base`'s scorecard is final. Once the switch is on, a re-score of `t12-base`
    would plan amode jobs and list them as missing.
  - **Keep the 8bbcc135 probe:** copy it before its worktree goes, from
    `/Volumes/BigMac/Users/astrobureau/Documents/Projects/acc-base-8bbcc135/.superpowers/target-acc/release/examples/star_probe`
    to `~/.athenaeum-acc/star-lab-a/t12-amode/bin/star_probe-8bbcc135`, recording its sha256. Delete the worktree after
    Step 4, not after Step 3.
  - **L1 (athenaeum-lab `star-lab/`, in one commit, then `git push origin main`):**
    1. `starlab/probe_plan.py`:
       - `ANALYSIS_MODE = True`;
       - `corpus_jobs` gains, per FITS frame, `_measure(fam, name, path, base, "analysis-mode", ("--domain", "adu"),
         sfx="amode", tag="analysis-mode")`. There is no `--clip` (the probe reads it from the header) and no
         `--debayer` (the raw mosaic is one plane; Plan C's CFA rule comes later);
       - XISF frames skipped with the reason;
       - the module docstring.
    2. `tests/test_run_probe.py`:
       - the two tests above;
       - new `test_the_corpus_runs_the_analysis_mode_arm_on_fits_frames_only`: args `[path, "--paths",
         "analysis-mode", "--domain", "adu", *star_probe_args(), "--out", <base>.amode.jsonl]`, and none for XISF.
    3. `starlab/scorecard_build.py`: `analysis-mode` joins the row-1 detection loop and row 2. It passes
       `frames=` its OWN frame lines, so `_threshold` reads its `options.measure` (`fisher_snr`, `seedThreshold`). It
       also joins rows 3–6 (flux unit 1.0; the SNR rows with its own frames) and the OSC G-plane remap beside
       `analysis`.
    4. `tests/test_scorecard.py`: `test_the_analysis_mode_output_is_read_when_the_plan_runs_it` is extended with two
       star lines. An `analysis-mode` `fwhm` row appears, and the measure path's `seeds` count is the measure file's
       alone.
    5. `starlab/scorecard.py`: `rows_budget_coverage`'s docstring and `ideal` text get R12-6's definition. The
       arithmetic is unchanged.
    6. `bin/measure_wall.py`:
       - new repeatable `--arm-probe NAME=PATH` (default `--probe`) and `--arm-path NAME=measure|analysis-mode|legacy-
         analysis` (default `measure`);
       - `analysis-mode` is `--paths analysis-mode --domain adu` and never `--debayer`;
       - every arm's sha256 goes into the provenance record; byte identity per arm across rounds, as today;
       - tests in `tests/test_t10b_l2.py`: the per-arm commands, both sha256s, and the defaults planning today's
         commands exactly.
    7. `results/plan-a/t12-amode/design.json` = `[{"row": 1, "path": "register", "kind": "reported"}]`.
       `results/plan-a/t12-amode/bars.py` comes with `tests/test_t12_bars.py`, whose fixtures cover: a 0 / 0 plane
       skipped and counted; `min_share` 0.89 → MISS; identity `ok: false` → MISS; the wall JSONs → mean and max; the
       `_DSC5767` seeds lines.

    Already there, reused unchanged:
    - `rows_fit_outcomes` (notSelected / trimmed via `_budget_count`), `rows_budget_coverage`, row 9 `invocation` and
      `rows_cost` for `analysis-mode` (`ONE_PATH_TAGS`), `_threshold("analysis-mode")`;
    - `probe_io`'s `notSelected` / `trimmed` reasons, `AMODE_ARGS`, `_analysis_jobs`' amode arm;
    - `run_probe.py` (the corpus driver: one path per invocation, `--verify`), `lab_cycle.py`, `measure_wall.py`'s
      interleaved rounds and idle wait.
  - **The cycle** (cwd athenaeum-lab):

    ```
    python3 star-lab/bin/lab_cycle.py --step t12-amode --prev t12-base --inject \
      --design star-lab/results/plan-a/t12-amode/design.json \
      --athenaeum /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
    ```

    Use the same `--jobs` as `t12-base`. It runs v2 with the `analysis-mode` arm on every u16 sibling (303 jobs: 305
    siblings minus `resampled`) and the inject manifest (its raw frames gain the arm). The existing paths' rows are
    judged and must not move; the new rows are only-head.
  - **The corpus:**

    ```
    ATHENAEUM_REPO=<the athenaeum checkout> STAR_PROBE_ARGS="--diagnostics --seeds matched --seed-threshold 6.0" \
      python3 star-lab/bin/run_probe.py star-lab/manifests/corpus.json --out ~/.athenaeum-acc/star-lab-a/t12-amode/corpus-probe
    ```

    Then the same with `--verify`. That gives 140 FITS frames × (`measure` on the calibrated read, i.e. the unit-domain
    read of the raw file, `legacy-analysis`, `analysis-mode` on the raw file), one path per invocation.

- [x] **Step 4: Walls and bars** (`bars.txt`; dev Mac, R12-0).
  - **Bar provenance:** all three fixed numbers come from the dev Mac on 2026-10-07: athenaeum star-truth-lab
    f5321979, `star_probe` sha256 `ef9be3c0…`, BEFORE Plan A (the old measure path, no `--diagnostics`).
    - Legacy analysis 25.2 s / 77 frames and m42 `measure` 223.1 s: athenaeum-lab
      `results/probe-runs-2026-10-07/batches.txt`, the sums of the 77 invocations' `/usr/bin/time` real.
    - `neb3_4096` legacy 0.21 s: `results/baseline-2026-10-07/scorecard-summary.md`, row 9.
    - The runner's M.1 series (133.2 s → 76.9 s through Task 11) was never compared with them. The dev Mac has no read
      after 2026-10-07; the brackets below are that read.
  - **The two brackets** (each `measure_wall.py`, 77 m42 frames, three interleaved rounds, nothing else running):

    ```
    # (a) Measure, before / after
    python3 star-lab/bin/measure_wall.py --probe <head star_probe> --arm A= --arm-probe A=<star_probe-8bbcc135> \
      --arm B= --set m42 --repeats 3 --root "$HOME/Pictures/star test" \
      --out ~/.athenaeum-acc/star-lab-a/t12-amode/wall-measure
    # (b) Analysis mode beside the same-day legacy control
    python3 star-lab/bin/measure_wall.py --probe <head star_probe> --arm amode= --arm-path amode=analysis-mode \
      --arm legacy= --arm-path legacy=legacy-analysis --set m42 --repeats 3 --root "$HOME/Pictures/star test" \
      --out ~/.athenaeum-acc/star-lab-a/t12-amode/wall-amode
    ```

    Both run with empty flags, i.e. without `--diagnostics`: production's setting and the 2026-10-07 command. Then
    `python3 star-lab/results/plan-a/t12-amode/bars.py` (or the cycle replayed with `--reuse-probe` and `--bars`).
  - **The bars:**
    - **Budget:**
      - row 1 `budget coverage`: `cellsCovered / cellsWithStars` ≥ 0.9 on every v2 image with `cellsWithStars > 0`
        (every family's `min_share`). The 0 / 0 planes (plane verdicts, R12-6) are counted and listed;
      - the `analysis-mode` `fit outcomes` row is `ok` on every family (the four-term identity, ruling F13).
    - **Analysis cost:**
      - (b)'s `amode` arm: the mean over the 77 frames of the invocation real (the median over rounds) ≤ 0.6 s;
      - printed beside it: the max, the frame lines' phase medians (from the corpus probe), and the same-day `legacy`
        mean beside 2026-10-07's 0.33 s;
      - `background/neb3_4096` `analysis-mode` wall (the cycle's row 9) printed beside the legacy 0.21 s and the same
        run's legacy row.
    - **Measure cost (second read):** (a)'s B median sum ≤ 1.2 × 223.1 s = 267.7 s (per frame ≈ 2.9 s → ≤ 3.5 s), read
      in absolute terms. B / A per round and their median are printed as the attribution:
      - if A also exceeds 267.7 s, the miss predates Task 12 (the territory of M.2, which is Task 13's) and goes to the
        owner with both walls;
      - B / A > 1.2 is Task 12's own miss;
      - expect B / A ≈ 1: `Mode::All` only got the structural split.

      The first read (M.1) is Task 10b's on the runner; Task 13's M.2 is the stage wall on set 204.
    - **Recorded:**
      - `_DSC5767.fit`: the corpus `measure` frame line's `seeds` per plane (baseline 0; `Mode::All` is unchanged, so
        it equals 8bbcc135's; the signedness question is Plan C's);
      - bright coverage and completeness of `analysis-mode` against legacy `analysis` on `sharp/m4/f2.6` (baseline
        legacy 7.3 % of 300, 21.5 % of 554 expected), which is Plan C's comparison;
      - the corpus `analysis-mode` coverage and identity on the real frames (information).

- [x] **Step 5: R-OSP-10 (R12-0…R12-7, the bars with Step 4's numbers), the spec rows, the logging dictionary;
  commit** (`one-star-path`, not pushed; lab committed and pushed).

```
feat(star_measure): Analysis mode - a budget of N good unsaturated stars spread over a 4x4 grid before fitting (water-filled candidates, unflagged-first SNR round-robin trim, ring and isolation against every seed), NotSelected/Trimmed outcomes, the BudgetReport, Moffat 4 with no beta work (coverage 100% worst, m42 analysis-mode 0.70 s/frame - B3 miss recorded, measure batch 111.9 s, B/A 1.028)
```

**Acceptance:**
- Step 1's tests fail at 8bbcc135 for the reasons named and pass; the pins hold (`PASS1_ARTIFACT_VERSION` 1).
- The Step 2 gates are green.
- Coverage ≥ 0.9 on every image that holds measurable stars; analysis-mode fit outcomes ok everywhere.
- m42 analysis-mode mean ≤ 0.6 s; m42 measure batch ≤ 267.7 s (B / A printed).
- Both diffs (v2, inject) 0 moved; the control 0.
- A cost miss → the owner with the walls and the phase medians before Task 13 (the +20 % bar binds there too).

**Outcome (2026-10-16; branch `one-star-path-t12`: 98f0c660 plan, a7ac7659 Steps 1-2, a00238f7 review fixes, then the
docs commit; lab b6dfc13 -> 92fbded; not pushed).** `Mode::Budget` is in: the selection (water-filled 4 x 4 candidates,
quota 1.25 N / 16), the unflagged-first SNR round-robin trim, ring sky and `c` against every seed (R12-5), the
`NotSelected` / `Trimmed` outcomes and records, the `BudgetReport`, Moffat 4 with no own-beta work of any kind (R12-1, owner),
and the probe's `analysis-mode` path (R12-7). `Mode::All` is op for op unchanged: `PlanePrep` untouched,
`PASS1_ARTIFACT_VERSION` 1, `MEASURE_VERSION` 4. Review (fable): READY WITH FIXES 0 / 1 / 5, all fixed in a00238f7.
Gates: `star_measure::` 190, `stacking::` 853 / 0 / 2 ignored, `star_probe` 19, `check --all-targets` and
`--no-default-features` as before. **The bars** (dev Mac, R12-0; `t12-amode-pn/bars.txt`): B1 coverage worst 100 % (141
planes, 2 donut planes 0 / 0 skipped) PASS; B2 143 / 143 PASS; B4 111.9 s <= 267.7 (A 108.9, B / A 1.028 - small, real,
unexplained) PASS; **B3 0.70 s against 0.6 s: a recorded miss (R12-8, owner)** - phase medians bg 178 / noise 234 /
detect 141 / fit 65 ms, the whole-plane passes are the stacking path's own code, so their speed-up is Measure-perf work
beside M.2, judged at master level, no Analysis-only shortcut. Both diffs 0 moved, control 0. Recorded: `_DSC5767` seeds
15041 / 15786 / 9039 (from 0); sharp/m4/f2.6 bright coverage 100 % vs legacy 7.3 %, completeness 92.6 % of 537 vs 21.5 % of
554; corpus coverage 99.9 % (worst 93.8 %). Rulings R12-0 ... R12-9 (README R-OSP-10). The first t12 lab round ran with the
pixel-noise statement unset (R12-9) and was redone as `t12-base-pn` / `t12-amode-pn`. Limits: candidates-only plane values,
raw OSC as one plane, the raw-mode completeness bar. Carries: **Task 13** the B3 whole-plane cost beside M.2; **Plan C** the
Analysis tab / Blink switch (`analysis/analyzer.rs map_result`), XISF / `read_raw`, the CFA rule for raw OSC, the
completeness bar at N >= seeds.

---
### Task 13: The real-data checkpoint at master level (spec §5 step 5, owner rule)

**Moved by Task 10b, 2026-10-11.** The M42 frame-β MUST-RESOLVE (Task 9's carry), the hc11B L 0010 seed flood and its
`c` re-read (Task 10's carry; L 0010 and L 0011 are trailed frames, now Task 10b's trailed verdict) and the first
Measure-wall read moved out of this task: W.1, T.1–T.3, M.1 are 10b's bars; the M42 β bar is 10c's (10b reports it,
S.3). This checkpoint owns no fix: a miss here goes back to its owner task as a fix round, and the checkpoint re-runs.
It reports the M42 β spread at master level and the OSC LN reference R-plane `c` SE (2.19 % at Task 10).

**Carried in by Task 10c (R10c-4, R10c-5).** PSFSW: `frame.rs::signal_totals` sums every accepted fit and reads no
flag; the saturated share of TFlux is 0.79–0.90 on M42 and 0.77–0.82 on hc11B L; relative M42 weights 0.40–1.00 →
0.82–1.00 with flagged fits out — the weights' owner judges it against the ERP at master level. D0099 (LDN 1272)
reads β 99.96 at every brightness (F 4.1 vs the set's 2.0) — a frame-class item (defocused / flat-topped).

**Closed by Task 11b (2026-10-13).** Its M42 frame-β line is closed by definition (the stacking group β, README
R-OSP-9: B2 is not a bar); the Measure-wall bar (M.1) now covers the two-pass Measure.

**Added by Task 11c (owner, 2026-10-14): M.2, the Measure STAGE wall on real data.** M.1 judges only the lab m42
corpus batch on the runner (74.7 s, bar 159.8 s) — small frames, where the cost does not show. On set 204 (197 frames:
105 OSC + 92 mono, 407 planes of 6248×4176, dev Mac, admission 5 on a 10-thread pool) the stage went **3.62 min before
Plan A** (`tierA-lnB-f1` run 5; per-plane medians read 0.04 / background 0.32 / noise 0.38 / detect 0.16 / **fit
0.21** / total 1.68 s) **→ 11.31 min at ec39507e** (= the 11c branch, 11c Step 5 run `OA-i`; read 0.12 / background
0.44 / noise 0.39 / detect 0.29 / **fit 2.14** / total 5.72 s): **3.1×, almost all of it the fit (10×)**. The owner:
"more than 3 times slower than it was and 4 times slower than the ERP does it". Task 13: (1) the Measure stage wall
and the per-plane sub-stage medians on each of its three datasets, beside the ERP's own measurement time on the same
frames; (2) a profile of the fit's 2.1 s a plane — stars fitted per plane against the 24 576 cap, LM iterations per
fit, the verdict pass's share, the two-pass share, the second read; (3) **bar M.2 (owner-accepted 2026-10-14; the owner
may amend it): set 204's Measure stage ≤ 1.2 × 3.62 min = 4.3 min on the dev Mac, interleaved bracket, with the ERP's
time printed beside**. This checkpoint owns no fix: a miss opens a Measure-performance task (scope by the owner)
before Plan A closes, under the method rules — a speedup keeps every `.athf` bit-identical or is judged on the whole
group at master level, and one that misses a tolerance row is reverted, never tuned toward it. **Added by Task 12 (R12-8):** the Analysis mode's B3 miss (0.70 s against 0.6 s per m42 frame, the three whole-plane passes: background 178 / noise 234 / detect 141 ms) is the same Measure-performance family; this checkpoint reads the whole-plane cost beside M.2.

**Reconciled by the Task 13 design read (2026-10-16, at b6c5d5b3; `.superpowers/sdd/2026-10-07-one-star-path-plan-a/
task-13-report-design.md`).** Scope and bars unchanged. Against the 2026-10-07 text: (a) the three before runs are
pinned by commit — all three were made by LN checkpoint B at **d5f26b7e** (2026-10-06, rustafits 83214127), whose
Measure is byte-equal to local main **ed5fa8de** (the one Measure diff between them is the `measure_plane_detailed`
refactor, pinned by its own same-output test; the gitlink is the same); (b) `hc11-run.sh` is not ported: Horse C11 runs
through `tier1-run.sh` like set 204; (c) Step 3 (c) (PA-13) reads the before run's own `stacking_run_frames.metrics_json`
(each tree carries two `metrics` rows per frame) and reproduces each set's measurement options (Horse C11's `maxStars
500` and `mad` scale estimator are not `measure_probe`'s defaults — as written it failed by construction there);
(d) project runs key frames by `project_frame` (`frame_id` is NULL) and write `run-project.txt` only; (e) M.2 gets its own
interleaved Measure-only bracket on two more clones, the fit profile one additive instrumentation step (13-P), the ERP
time its own step; (f) the clone recipe also rewrites `files.path` under the before tree (Horse C11's in-run masters);
(g) results live in athenaeum-lab `star-lab/results/plan-a/t13-real/`; (h) `--numeric`'s `NUMERIC:` line is Tier C's
table (ρ 0.99, rejected fraction, LN scatter), not this task's verdict.

Under the owner's rule a numeric change is judged on the whole group at master level. The harness is reused:
`harness/acceptance/{server.sh,api.sh,clone-before.sh,tier1/tier1-run.sh,tier1/project-run.sh,tier1/tier1-extract.py,
tier1/tier1-compare.py,tier1/t11c-tools.py}` and the lab's ERP job runner `star-lab/erp/run_job.sh`; one driver
`t13-real.sh` and one helper `tier1/t13-tools.py` are added. Nothing is built per cycle: two servers once (Step 1), the
13-P server once (Step 4).

**The clone recipe (deletes no acceptance copy, needs no owner decision).** `/Volumes/BigMac` is APFS, so `cp -Rc`
clones a before tree in seconds at zero cost until a file is rewritten; and the Calibrate stage hash carries no
`MEASURE_VERSION` (`calibration_subtree`, Task 3's test asserts its literal must NOT move), so the before trees'
`calibrated`/`prepared` artifacts are, by construction, exactly what the after run would write. Each after run
therefore starts from Measure on a clone of its before tree. **Order and lifetime (ruling F11): Horse C11 → run B →
set 204, one at a time; each clone is THIS task's scratch and is deleted (`rm -rf` of the whole clone — a clone
holds no byte the before tree lacks) as soon as its result files are committed in the lab, except set 204's, which
Task 14 reads (`probe_parity`); the two M.2 clones (`starA-m2-A`, `starA-m2-B`, Measure-only: `fits/` 0.13 GB and
`measured/` 0.94 GB each) are deleted after Step 5.** New bytes at peak are one dataset's: Horse C11 ≈ 26 GB, run B ≈
38 GB, set 204 ≈ 46 GB (t11c measured 44–47 GB per set-204 run), then 46 + 2.2 GB — far inside the 100 GiB floor
(939 GiB free on 2026-10-16). If the recipe fails at execution (a `cp -Rc` that copies, a path rewrite the server does
not honour, a refused `rerunFrom: "measure"`), the dataset STOPs with its numbers; nothing deletes a before tree or
falls back on its own. The fallback, taken only on the owner's word (Fable while the owner is away), is
`prepare-catalog.sh` into a fresh `~/.athenaeum-acc/starA-<ds>`, same order.

**Files** (athenaeum-lab unless named):
- Modify `harness/acceptance/clone-before.sh`: step (2) also runs `UPDATE files SET path = replace(path,'<before>',
  '<after>') WHERE path LIKE '<before>/%'` and prints its count (Horse C11's in-run masters under `library/`, 8+; 0 on
  the other two), so no run opens a before tree's byte through the catalog. Allow-list, refusals and step (4) unchanged
  (`c11-config.json` is no longer read: `make-config` builds all three configs).
- Modify `tier1/tier1-run.sh`: `ATH_ACC_SET_REFERENCE=<file>` (`{"framesSetId": N, "frameId": F}`, sent BEFORE the
  `ATH_ACC_SET_CONFIG` call so a manual config never meets an unset reference; anything but `null` refuses; echoed into
  `run.txt`); `ATH_ACC_EXPECT_REFERENCE=<frame id>` (after both calls `get_stacking_plan {setId}` must read
  `reference.mode == "manual"`, `reference.frameId == F` and no blocker, else the run refuses before `start_stacking`;
  the plan's reference line goes into `run.txt`); `ATH_ACC_STOP_AFTER=<stage>` (after the start, poll `<W>/logs/*.jsonl`
  for this run's `"stacking stage finished"` of that stage, then `cancel_stacking {runId}`, wait for its
  `stacking-complete`, write `stop_after <stage> duration_ms=<n>`); a `stacking-complete` without `"success":true` exits
  1 unless STOP_AFTER cancelled it; the server is stopped by its own pid (`project-run.sh`'s trap), never `pkill -f`.
- Modify `tier1/project-run.sh`: `ATH_ACC_RERUN_FROM` (replaces the hardcoded `"calibrate"`, default unchanged),
  `ATH_ACC_EXPECT_REFERENCE=<project frame id>` (`get_project_stacking_plan`), `ATH_ACC_STOP_AFTER` (as above).
- Modify `tier1/tier1-extract.py`: reads `run-project.txt` when `run.txt` is absent; prints 13-P's fields when present
  (`verdict_ms` on `"frame plane measured"`; the `"frame plane first pass"` line's `beta_ms`, `verdict_fits`,
  `verdict_iterations`, `seeds`, `duration_ms`).
- Modify `tier1/tier1-compare.py --numeric`: `--project-id <uuid>` (twin of `--set-id`: the latest `done` run with
  that `stacking_runs.project_id`, ruling F41); `--run-id N` / `--base-run-id M` (that run on its side, default the
  latest `done` as today — on a clone holding the before AND the after run the default is the AFTER run, so every
  base side names its run, F19 as folded); `--base-db <path>` (base rows from that DB; base masters and `.athln` still
  from `<W_base>`); frames joined by `COALESCE(frame_id, project_frame)` in `load_frame_weights` and the per-group
  breakdown; the §8 master rows also on each `_drizzleNx` head/base pair (beside the head-only level row);
  `starsRefused head=… base=…` under each plane's `beta` line; every `sqlite3.connect` (six: `run_identity`'s `reg`,
  `run_id_for_set`, `load_groups`, `load_frame_weights`, two in `report_weights`) → `ro_connect` (`immutable=1`, F19).
- Create `tier1/t13-tools.py` (stdlib, every connect `immutable=1`): `before <clone> <ds>` (the before run's row,
  `reference_mode`, `config_hash`, `summary_json.stages`; for Horse C11 / run B also the original's row — equal, or
  WAL-stale → base rows from the clone); `make-config <clone> <ds> <out> [--ref-out F]` (set: the stored
  `stacking_set_config` document, `paths` removed, `reference.mode = "manual"`, `reference.twoPass = false`, checked
  against the before run's `stacking_runs.config_json` — any other differing key STOPs; `--ref-out` = the run row's
  `reference_frame_id`; project: `stacking_project_config` → `{projectId, config, excludedFrameUuids,
  referenceFrameUuid}` = the uuid of the run row's reference project frame); `equivalence` (Step 3 (c)); `weights <head>
  <base db> --run R --base-run B` (Spearman ρ overall — the bar — and per group; every frame whose within-group rank
  moved by > 10 % of its group's N with before/after weight and rank, before β per plane, after `beta`/`betaOwn`,
  `starsFitted`/`starsUnsaturated`/`starsRefused`, `tflux`/`noise`/`mStar` ratios, `fwhmPx`, `eccentricity`, and a
  candidate row: F-4 (a before β off the group's modal before β), F-3 (unsaturated share < 0.9 or fwhm/ecc moved > 2 %),
  F-5 (`tflux` ratio off the group's median ratio by > 5 %), none); `profile <clone> --run N [--cap 24576]` (Step 4);
  `erp-sum <dir>` (Step 6); `bars <res>…` (Step 7: `bars.txt`, each dataset's ≤ 30-line `verdict.txt`, the M.2 line).
- Create `harness/acceptance/t13-real.sh` (the `t11c-wall.sh` pattern): `zsh t13-real.sh <ds>`, ds ∈ `hc11 | runB |
  s204 | p0 | m2 | erp | all` (`all` = the six in that order, stopping at the first STOP); `zsh t13-real.sh --cleanup
  <ds>` (that dataset's clones only; refuses a before tree). Env: `T13_A_REPO` (the ed5fa8de worktree; required),
  `T13_B_REPO` (default this checkout), `T13_P_REPO` (the 13-P worktree: `p0` and m2's B arm; unset → `p0` skipped,
  m2's B arm = `T13_B_REPO`), `ATH_ACC_EXTRA_PATHS` (default `$HOME/Pictures`: set 204's and run B's lights). It builds
  nothing (a missing binary refuses; each repo's HEAD and submodule shas recorded); before every clone and run:
  `memory_pressure` ≥ 30 % free, `df` ≥ 100 GiB + the step's peak, Time Machine idle, no cargo/rustc, no acceptance
  server — a miss STOPs (exit 3), never waits. Results in `star-lab/results/plan-a/t13-real/<ds>/` plus
  `run-log.txt`; stdout = that step's ≤ 30-line verdict. Exit 0 = done (a missed bar is a result); 3 resources; 4 a
  tool or run failure; 5 an equivalence, reference or config mismatch. A STOP leaves the step's clone for inspection,
  writes no before tree and stops its own server.
- Create `star-lab/erp/erp_subframes_t13.js`: Task 10b's subframe-measurement script (`results/plan-a/t10b/erp-l0010/
  erp_subframes.js`, same process settings) with its job path `$HOME/.athenaeum-acc/t13-erp/ss-job.json`.
- Modify `harness/acceptance/README.md`: item 12 (this recipe: `clone-before.sh`, the four env vars, `t13-real.sh`,
  `t13-tools.py`).
- athenaeum, Step 4 (13-P): `crates/athenaeum-core/src/stacking/measure.rs` (hand edits only),
  `crates/athenaeum-core/src/star_measure/mod.rs` (two read-only `PlanePrep` accessors),
  `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (the new field names in the dictionary).
- athenaeum, Step 8: `docs/stacking/README.md` (the acceptance run, R-OSP-11), this task's Outcome;
  `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-a.md` §4 "Real data" is written in Task 14.

**Interfaces:**
- Consumes the before trees (PA-13), read ONLY through their clones after the clone's checkpoint (ruling F19), all made
  at d5f26b7e by LN checkpoint B (2026-10-06):
  - `~/.athenaeum-acc/hc11B-f1` — Horse C11, set 182, run 5 (10:49–11:00 UTC): mono L 180 s (74 included) + G 120 s
    (28), 9576×6388; reference 69845 (manual, `twoPass` true); `maxStars 500`, scale estimator `mad`, LN F1 growth
    curve; stages masters 1.44 / calibrate 3.17 / **Measure 1.65** / register 1.08 / normalize 1.19 / integrate 1.98
    min, total 10.52; plane medians read 0.09 / background 0.40 / noise 0.64 / detect 0.23 / fit 0.08 / total 1.66 s;
    a 6.8 MB un-checkpointed WAL (an `immutable=1` read of the original shows run 5 `running`).
  - `~/.athenaeum-acc/lnB-runB-f1` — run B, project `af136ec9-1885-4087-b684-9136ad655e9b`, run 6 (10:24–10:37 UTC):
    `osc__None__180s` 101 + `mono__L__180s` 83; reference project frame 123 = `aaa72b42-65a9-493a-ad08-6628d0af0340`
    (manual, `twoPass` true; file `…_0171`); stages calibrate 1.80 / **Measure 3.55** / register 1.85 / normalize 2.66 /
    integrate 3.23 min (wall 811 s); measurement options = the defaults; `run-project.txt` only.
  - `~/.athenaeum-acc/tierA-lnB-f1` — set 204, run 5 (09:55–10:10 UTC): osc 105 + mono 92 (407 planes of 6248×4176);
    reference 28934 (auto, two-pass, switched from 29034); stages calibrate 2.23 / **Measure 3.62** / register 1.47 /
    normalize 2.46 / integrate 3.83 / drizzle 1.18 min (wall 901 s); measurement options = the defaults.
  - Before-run facts the bars read: no plane reaches the 24 576 cap (median detected 6.1 k OSC / 7.1 k mono); per-frame
    β was discrete and mixed inside every group (set 204 mono 78 × 10, 7 × 6, 4 × 2.5, 3 × 4; OSC 71 all-2.5, 34 with a
    4 or 6 on a plane; Horse C11 L 48 × 2.5, 25 × 4, 1 × 10) — what Plan A's group β (F-4b) and F-5 remove.
- Produces: clones `~/.athenaeum-acc/starA-{hc11,runB}` (deleted in Step 3), `starA-s204` (kept for Task 14),
  `starA-m2-{A,B}` (deleted after Step 5); `star-lab/results/plan-a/t13-real/{hc11,runB,s204,p0,m2,erp}/` (`clone.txt`,
  `before.txt`, `config.json`, `ref.json`, `run.txt`, `extract.txt`, `compare.txt`, `weights.txt`, `profile.txt`,
  `verdict.txt`), `before-equivalence.txt`, `bars.txt`, `run-log.txt`; athenaeum: the 13-P commit, the README record.

- [x] **Step 0: Resources, written before anything runs** (into `bars.txt`). `df -k /Volumes/BigMac` ≥ 100 GiB + 46 GB
  (set 204 kept) + 2.2 GB (M.2 clones) + the two worktree targets (≈ 6 GB each); the server's `space` blocker compares
  free bytes with the FULL footprint estimate (`stacking/plan.rs`, ≈ 99 GB for set 204, no discount for `rerunFrom`) —
  939 GiB free meets both. `tmutil status` `Running = 0`; `memory_pressure` ≥ 30 % free. Remove the worktree
  `acc-base-8bbcc135` and its target (Task 13 has no 8bbcc135 arm: Task 12 left `Mode::All` op for op unchanged).

- [x] **Step 1: Build two servers, once, one compile at a time.**
  A (local main, ed5fa8de): `git worktree add /Volumes/BigMac/Users/astrobureau/Documents/Projects/acc-base-ed5fa8de
  ed5fa8de && git -C …/acc-base-ed5fa8de submodule update --init`, then in it `CARGO_TARGET_DIR=<wt>/.superpowers/
  target-acc CARGO_BUILD_JOBS=4 cargo build --release -p athenaeum-web` AND, as a second call, `cargo build --release -p athenaeum-core --example
  measure_probe --example weight_audit` (one call naming `-p athenaeum-web` beside `--example` builds ONLY the examples) (`server.sh` serves `$REPO/.superpowers/target-acc`, so the worktree's target lives there,
  not in `target-main`). A is Step 3 (c)'s probe, M.2's before arm and — only if (c) fails — the F42 re-make's server.
  B (this checkout, b6c5d5b3): `CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo build --release -p
  athenaeum-web`, then `... cargo build --release -p athenaeum-core --example measure_probe` (the `measure_probe` that `tier1-compare.py` measures BOTH
  sides' masters with: "both masters by the new path"). Record both HEADs and submodule shas. From here no source in
  either checkout changes while a run goes (`server.sh`'s incremental build would rebuild mid-task): 13-P is written in
  its own worktree.

- [x] **Step 2: The lab tools** (Files above), with unit tests beside `t11c-tools.py`'s, and `zsh t13-real.sh --check`
  (every binary present, every before tree's run row readable, the configs buildable, nothing run). Lab commit + push.

- [x] **Step 3: The three datasets, one at a time, Horse C11 → run B → set 204** (`export T13_A_REPO=/Volumes/BigMac/
  Users/astrobureau/Documents/Projects/acc-base-ed5fa8de; cd athenaeum-lab/harness/acceptance; zsh t13-real.sh hc11 &&
  zsh t13-real.sh runB && zsh t13-real.sh s204`). Each step, in the driver:
  (a) `zsh clone-before.sh ~/.athenaeum-acc/<before> ~/.athenaeum-acc/starA-<ds>` → `clone.txt` (df within 1 GiB,
  checkpoint, the three rewrites with counts, stale-by-hash trees removed).
  (b) `t13-tools.py before` → `before.txt` (run row, effective reference, stage times; Horse C11: base rows from the
  clone via `--base-db`; run B: 123 ↔ `aaa72b42…` shown equal — the run row's effective pick is what is pinned).
  (c) **Measure-equivalence (PA-13).** Three frames per tree — the first, the median and the last by frame id of the
  mono group (Horse C11 L, run B `mono__L__180s`, set 204 mono) of the before run — measured by A's build on the
  clone's calibrated artifact: set 204 and run B `measure_probe <file> auto` (their options are
  `MeasureOptions::default()`: auto, 24 576, σ 20, peak, no prefilter, BWMV); Horse C11 `weight_audit --max-stars 500
  --sigma 20 --seed-detector peak --prefilter none --psf auto <file>`; every `ChannelMeasurement` field equal to the
  before run's `stacking_run_frames.metrics_json` (minus `durationMs`; Horse C11 also minus `location`/`scale`, the two
  fields the normalization scale estimator feeds — its config says `mad`, the probe has BWMV). Nine comparisons →
  `before-equivalence.txt`. A difference STOPs the dataset (exit 5); the F42 re-make (`clone-before.sh` → a second
  clone `starA-before-<ds>`, its run from `measure` on A's server, the disk named first) is then taken by the
  controller on a Fable ruling, never by the driver.
  (d) `t13-tools.py make-config` → `config.json` (+ `ref.json`): the before config with `reference.mode = "manual"`,
  `twoPass = false` (ruling F18: a two-pass or auto pick re-reads Plan A's metrics and could move the reference, and
  every downstream row would then differ by the reference instead of the measurement — set 204's own run switched
  29034 → 28934); pins 69845 / project frame 123 / 28934.
  (e) The run from Measure on B's server (`$R` = that dataset's `star-lab/results/plan-a/t13-real/<ds>`):
  ```
  ATH_ACC_RERUN_FROM=measure ATH_ACC_SET_CONFIG=$R/config.json ATH_ACC_SET_REFERENCE=$R/ref.json ATH_ACC_EXPECT_REFERENCE=69845 \
    zsh tier1/tier1-run.sh $T13_B_REPO ~/.athenaeum-acc/starA-hc11 8971 182                    # ≈ 8 min (Measure ≈ 3)
  ATH_ACC_RERUN_FROM=measure ATH_ACC_PROJECT_CONFIG=$R/config.json ATH_ACC_EXPECT_REFERENCE=123 \
    zsh tier1/project-run.sh $T13_B_REPO ~/.athenaeum-acc/starA-runB 8961 af136ec9-1885-4087-b684-9136ad655e9b   # ≈ 18 min
  ATH_ACC_RERUN_FROM=measure ATH_ACC_SET_CONFIG=$R/config.json ATH_ACC_SET_REFERENCE=$R/ref.json ATH_ACC_EXPECT_REFERENCE=28934 \
    zsh tier1/tier1-run.sh $T13_B_REPO ~/.athenaeum-acc/starA-s204 8950 204                    # ≈ 21.5 min (t11c: Measure 10.7)
  ```
  The after run id is read from `run.txt` / `run-project.txt` (expected 6 / 7 / 6); the before ids (5 / 6 / 5) from
  `before.txt`.
  (f) `tier1-extract.py <clone> --run-id R --measure-phases > extract.txt`; `tier1-compare.py --numeric <clone>
  ~/.athenaeum-acc/<before> {--set-id 182 | --project-id af136ec9-… | --set-id 204} --run-id R --base-run-id B
  [--base-db ~/.athenaeum-acc/starA-hc11/athenaeum.db] > compare.txt` (base masters from the original's `stackout`,
  opened read-only; both sides measured by B's `measure_probe`); `tier1-compare.py --ln-grids <clone>
  ~/.athenaeum-acc/<before> >> compare.txt`; `t13-tools.py weights … > weights.txt`; `t13-tools.py profile … >
  profile.txt`.
  (g) `t13-tools.py bars` → `verdict.txt`; lab commit `results(plan-a): t13-real <ds> - …`; then, for hc11 and runB
  only and only after the commit returned 0, `rm -rf ~/.athenaeum-acc/starA-<ds>`. Expected step walls ≈ 12 / 22 /
  27 min.

- [x] **Step 4: 13-P, the fit profile's instrumentation (athenaeum, additive, debug level, no number moves).** Written
  while Step 3 runs, in its own worktree `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-t13p` (branch
  `one-star-path-t13-p` from b6c5d5b3); built after Step 3 (its own `.superpowers/target-acc`). Measurable today
  without it: stars detected / fitted per plane against the cap and every named phase (`"frame plane measured"`), the
  two-pass share (`--measure-phases`), the second read (`read_ms` sums both passes' reads: mono 2 reads, OSC planes 0–1
  three, plane 2 two), the verdict fits' LM iterations on sampled frames (`star_probe measure --psf-beta B
  --diagnostics`). Not measurable: the verdict pass's share (folded into `fit_ms` via `PlanePrep::verdict_time`) and
  the β sample (`sm::plane_beta`, no timer — today only `duration_ms` minus the named phases, 2.3 s of t11c's 5.4 s a
  plane). The step: (i) in `measure_frame_pass1`, one `debug!` per plane after its first pass, `"frame plane first
  pass"` {`path`, `plane`, `seeds`, `verdict_ms`, `verdict_fits`, `verdict_iterations` (Σ LM iterations of the
  successful verdict fits), `beta_ms` (the wall of `sm::plane_beta`, timed in `measure_planes_pass1`), `beta_own`,
  `duration_ms`}; (ii) `"frame plane measured"` gains `verdict_ms` (read from the plane's `PlanePrep` before
  `finish_plane` consumes it). Read-only accessors `PlanePrep::verdict_time()` / `verdict_iterations()`; new values
  ride only in-memory structs: `PhaseTimings`, `PlanePrep` and the `.athm` record are untouched (`PASS1_ARTIFACT_VERSION`
  1, the encoding pin unchanged), `MeasureTimings` stays `#[serde(skip)]`. The fit at β returns no iteration count (the
  fixed-β conditioned entry; a rustafits change is out of scope) — the profile gives its ms per fitted star instead.
  Test: one log-asserting test (`docs/logging/README.md` pattern) on a synthetic two-pass group: both lines carry their
  fields and `verdict_ms ≤ fit_ms`. Gates: `cargo test -p athenaeum-core --lib -- star_measure:: --test-threads=4`, the
  same for `stacking::`, `cargo check -p athenaeum-core --all-targets` and `--no-default-features`; the full
  `--all-targets` suite on the runner from a git bundle (else here before any push); one review (fable, the diff).
  Data gate `zsh t13-real.sh p0`: clones `starA-m2-{A,B}` of `tierA-lnB-f1`; P0 = the 13-P server Measure-only
  (`ATH_ACC_RERUN_FROM=measure ATH_ACC_STOP_AFTER=measure`, Step 3 (d)'s set-204 config) on `starA-m2-B`, then
  `tier1-compare.py --measure-identity ~/.athenaeum-acc/starA-s204 ~/.athenaeum-acc/starA-m2-B --set-id 204 --run-id-a
  <s204 run> --run-id-b <P0 run>` → **0 differences** (both clones hold the same row history, as t11c's pair); P0's
  `profile.txt` is the first profile. A difference, a failed gate or a review that does not pass by Step 5's start drops
  13-P: the bracket runs on B and the profile prints what is measurable without it — 13-P is never fixed tonight.
  ```
  feat(stacking): Task 13 profile lines - the Measure first pass's verdict and beta-sample times and LM iterations at debug (set 204 metrics and .athf identical)
  ```

- [x] **Step 5: M.2, the interleaved Measure-only bracket on set 204** (`zsh t13-real.sh m2`, ≈ 50 min). A = ed5fa8de
  on `starA-m2-A`, B = 13-P (else b6c5d5b3) on `starA-m2-B`; rounds A1 B1 A2 B2 A3 B3, each `ATH_ACC_RERUN_FROM=measure
  ATH_ACC_STOP_AFTER=measure` with Step 3 (d)'s set-204 config (Measure reads only the measurement subtree; the
  reference is never reached; `rerunFrom ≤ measure` forces pass 1 fresh, R11c-5). Per round: the Measure stage wall, the
  pass-1 share, the plane medians (read / background / noise / detect / fit / total) per group; B rounds also
  `profile.txt` (Step 4's shares: verdict, fit at β, β sample, the whole-plane passes, reads; stars per plane against
  the cap; verdict iterations; fit-at-β ms per star). **Bar (owner, 2026-10-14):** median B ≤ 1.2 × median A — the
  owner's 4.3 min (= 1.2 × 3.62 of 2026-10-06) and A's today value against 3.62 printed beside — with the ERP's time
  on the same frames (Step 6) on the same line, and R12-8's whole-plane cost beside it (background / noise / detect
  plane medians A vs B; Analysis mode on m42 178 / 234 / 141 ms). Expected: a miss (t11c: 10.7 min). This checkpoint owns
  no fix: the miss opens the Measure-performance task (scope by the owner) with this profile. The clones are deleted
  after the lab commit.

- [x] **Step 6: The ERP's measurement time on the same frames** (`zsh t13-real.sh erp`, dev Mac — M.2's machine — last,
  with nothing else running; refuses beside an open instance and below 30 % free memory). Its subframe measurement
  process (the step its batch preprocessing runs before weighting), set up as Task 10b's script, on the files our
  Measure reads: set 204 = `starA-s204`'s 92 mono `calibrated/mono__…/c_*.fits` in chunks of 10 per instance and 105 OSC
  `c_*_d.fits` in chunks of 3 (the 16 GB rule: ≤ 10 × 26 Mpx per instance); Horse C11 = `hc11B-f1`'s 106 calibrated
  files (a file read, no catalog open) in chunks of 4. Time = the process's own in-instance time (around its execution,
  file loading included, as our Measure includes its reads), summed per group; the launches' wall and peak RSS beside.
  ≈ 25 + 15 min. Run B is not run: its 184 frames are set 204's cameras and nights (same reference file `…_0171`), so
  set 204's per-frame time × 184 is printed, labelled derived. A chunk that hangs past 10 min is killed (only the
  instance this step launched); the line then reads "not measured (<reason>)" and the 2026-10-04 batch log's
  measurement step is printed instead, labelled "another machine (16 threads), Horse C11 164 frames incl. R and B,
  170 s — not comparable". The OSC chunks under-use the cores, so its OSC time is an upper bound — said on the line.

- [x] **Step 7: Read the bars** (`t13-tools.py bars` → `star-lab/results/plan-a/t13-real/bars.txt`, compare files
  verbatim), per dataset:
  - master median within ±0.1 %, MAD ±1 %, noise (MRS σ) ±2 %, FWHM ±1 % (the §8 rows of `--numeric`, `docs/stacking/
    README.md` "masters within 0.1 % median / 1 % MAD / 2 % noise / 1 % FWHM") — mono and every OSC plane, plain and
    drizzled; "or better" means a row that is inside the tolerance passes whatever the direction; the rejected
    fraction and drizzle level / coverage rows are reported;
  - per-frame weights, on all three datasets (ruling F41): Spearman ρ ≥ 0.9 against the before run (overall, joined
    by `COALESCE(frame_id, project_frame)`, the runs `before.txt` and `run.txt` name), per-group ρ beside; every frame
    whose within-group rank moved by more than 10 % of N is listed with the scorecard row that explains it (F-5's
    captured fraction, F-3's unsaturated statistic, F-4's β) — a move without a row goes to the owner (Fable rules
    while the owner is away);
  - LN relative scale `s` per frame, on all three datasets: median ratio within 1 ± 0.5 % (the §8 row), read from the
    `.athln` grids; the scatter is REPORTED beside it, not a bar (ruling F23 e, PA-14);
  - Measure stage wall ≤ 1.2 × the before tree's (M.1's confirmation at master level): set 204 from Step 5's bracket
    (M.2); Horse C11 and run B against `before.txt` (1.65 / 3.55 min; no bracket — a ratio within 1.05–1.35 is printed
    "drift-ambiguous", R-TA-9, and gets one A Measure-only round on a clone before it is read); the Normalize stage wall
    is REPORTED beside it (M-6; set 204 before 2.46 min); the total wall is reported with the stages that did not run
    named (R-TA-9);
  - the stacking `metrics` payload's `starsRefused` and `beta` / `betaOwn` per frame are reported, the master `beta`
    and `starsRefused` lines (L-25) beside the FWHM row; the profile (Step 4) and the ERP line (Step 6) are printed.

- [x] **Step 8: Record** the run (R-OSP-11) in `docs/stacking/README.md` with every row's number and this task's Outcome;
  fast-forward `one-star-path-t13` to the 13-P commit when it passed; remove the A and 13-P worktrees and their targets.
  Only `starA-s204` stays on disk (`probe_parity` reads its `calibrated` + `fits.0` + `metrics` of one 6224×4168 mono
  frame on its run: `STARLAB_ACC_COPY=~/.athenaeum-acc/starA-s204 STARLAB_ACC_RUN_ID=<run>`); Task 14 deletes it at its
  end. No before tree is touched.

```
docs(stacking): Plan A real-data checkpoint - Horse C11, run B, set 204 before/after at master level on clones from Measure with the reference pinned, the M.2 bracket and the fit profile (ruling R-OSP-11: <rows>)
```

**Acceptance:** every master row inside the tolerances on the three datasets; ρ ≥ 0.9 on each with every larger move
explained; LN scale median within 0.5 % on each; Measure ≤ +20 % on each dataset (set 204 = M.2 on the interleaved
bracket, with the ERP's time and the fit profile printed); the clone recipe's `df` check held on every clone;
`before-equivalence.txt` equal on all nine; P0's identity 0 differences when 13-P is in. This checkpoint owns no fix: a
miss goes, with its compare file, to its owner task's fix round (weights / LN / master rows: F-5 Task 10, F-3 Tasks 8 /
10c, β Task 11b; Measure wall: the Measure-performance task the owner scopes) — Fable rules the routing while the owner
is away — and the checkpoint re-runs after the fix, before Task 14.

**Outcome (2026-10-16 night; branch `one-star-path-t13`: ba7a1ecf plan, then the docs commit; 13-P `40bad38c` on
`one-star-path-t13-p`, its review fixes still to fold in; lab d43e019 -> 8077164 -> f819e12 / 2260ab7 / 88f6cb2 -> c7f512f;
not pushed).** All three datasets ran as clones from Measure with the before reference pinned `manual` and `twoPass =
false`; the attribution (ed5fa8de, the A arm) reproduces the before trees exactly on all three, so every delta is Plan
A's own. **Rows:** weights rho 0.4642 (Horse C11, 78 movers) / 0.9553 (run B) / 0.9595 (set 204); masters 1/6, 0/4,
0/8 median sub-checks (sharper and, on set 204 mono, 3.8 % noisier; Horse C11 less noisy, L median +10.3 % on pedestal
data); LN s +3.00 / -0.95 / -0.97 %; Measure 1.338 / 2.752 / 2.726; **M.2: A 3.61 min, B 9.88 min, B / A 2.738, the owner's
4.34 min MISS, the ERP 4.00 min (ours / ERP 2.47x)**; profile: the seed cap on 50 / 92 mono planes, 22 961 fitted against
A 6 884 and the ERP 8 272, 0.102 against 0.027 ms per star. **Rulings:** R13-3 (Fable for the owner) weights and masters
ACCEPTED (ERP join rho 0.931 -> 0.956 mono, 0.903 -> 0.936 OSC; the master level is a re-definition); R13-4 the LN row is
a re-definition (the LN reference is the top-N by weight and moved with the weights; reference-corrected -0.21 % mono,
-0.06 / -0.06 / +0.13 % OSC), no fix round, Horse C11 closed (disjoint top-5 sets). **Limits:** the single-run walls
are one night; the harness LN row is not yet re-based (lab item). **Carries:** the Measure-performance task (the OWNER
scopes it; README R-OSP-11 holds Fable's five-line proposal) before Plan A closes; **Task 14** the LN-row re-base and
the 13-P review fixes folded into its commit. Tool defects: lab f819e12, 2260ab7, 88f6cb2; Step 1's build command.
README R-OSP-11. **Scoped by the owner 2026-10-16: Task 13b.**

---

### Task 13b: Measure performance — faster than the ERP (owner scope 2026-10-16; inserted before Task 14)

**Scope and bars fixed 2026-10-16 by the owner ("согласен, запиши это в задачу ускорения Measure и скорость должна быть
быстрее ERP"); Files / Interfaces / Steps by the task's design read, which may sharpen this stub where the code
contradicts it.** Inputs: Task 13's M.2 profile (`t13-real/p0/profile.txt`, `m2/`), Fable's Q5 five lines
(`t13-real/fable-ruling.md`, README R-OSP-11), R12-8 (the Analysis B3 miss), and the 2026-10-16 reads below.

**What Task 13 measured (why it is slow).** Set 204 Measure 9.88 min against 3.61 before Plan A and the ERP's 4.00:
3.3× more stars × a 4× slower per-star fit. Mono planes fit 22 961 stars (the 24 576 seed cap on 50 / 92 planes)
against A's 6 884 and the ERP's 8 272; the fixed-β fit costs 0.102 ms / star against A's 0.027; the verdict pass is
16 % (15.75 LM iterations / fit); the whole-plane passes (background 374 / noise 297 / detect 216 ms, two reads 73 ms)
barely moved.

**What the 2026-10-16 reads settled (owner questions; scripts in the session scratchpad, numbers here).**
- **The detections are stars, not noise.** Fits repeat on the next dithered frame at the same SKY position (affine
  match on the stars, ≤ 1.5 px): 0058 → 0059 88.2 % of 20 014 (dither 5.3 px), capped 0075 → 0076 95.2 % of 23 836
  (dither 2.6 px); chance at a shifted position 0.3–0.7 %; the same SENSOR pixel 0.0 % (no hot-pixel detections);
  SNR < 5 still repeats 68–86 %. The count is the deeper matched filter (6σ_f against the old 20σ peak) on a Milky Way
  field. The 50 capped planes are the sharp runs 0074–0082 and 0137–0144 (frame FWHM 2.1–3.3 px against 4–5); the ERP
  agrees (FWHM 2.0–2.7, 9–16 k stars against its usual ≈ 5 k). **The 6σ threshold stays** (Fable's out-of-bounds list).
- **The cap cuts the candidate list itself.** On a capped plane detection keeps the 24 576 strongest candidates by
  matched-filter SNR (0075: the last kept reads 9.4); visibly real fainter stars are never measured.
- **Faint stars are most of the cost and add little.** SNR < 10 is 24–51 % of the fits (0075 / 0058). They also carry
  the eccentricity noise tail: dropping the outlier tail (FWHM > 1.3F or ecc > 0.85) moves 0058's frame ecc by
  −0.018 — six times what the close pairs move.
- **Close pairs.** The fit takes every pixel of its box (no deblending; the neighbour seeds only mask the ring sky and
  `c`'s isolation), so a pair fits as one wide elongated star. Pairs whose companion has its own seed < 1.5F: 2.2 % of
  0058's fits (median FWHM 6.67 against 4.73, ecc 0.88 against 0.65), 0.7 % of 0075's; their residual-weighted share
  1.2 % / 0.5 % → frame FWHM +0.33 % / +0.05 %, ecc +0.002 / +0.0002. Merged pairs (no second seed; the shape FWHM >
  1.3F and ecc > 0.8, a mix of real pairs and faint junk on the contact sheet): +0.54 % / +0.14 %, ecc +0.003 / +0.001.
  Small today because the frame shape is residual-weighted; the effect grows with PSF width and crowding (≈ 5× from
  0075 to 0058). They are not the ERP FWHM gap (+8.6 % on 0058). The ERP drops clustered sources by default
  (`allowClusteredSources = false`).

**The rules (owner-agreed 2026-10-16).**
1. **Two populations, never one list cut for both.** (a) The **frame-statistic population** — FWHM, eccentricity,
   weights (PSFSW), β, `c`, the F-6 medians — is a budget of good unsaturated ISOLATED stars spread over the 4 × 4 grid
   and stratified across brightness (the mid band, never the N brightest; R-OSP-10's `Mode::Budget`, built in Task
   12). (b) The **star list Register and LN read** (the `.athf` per plane: `resolve_register_fits` for mono
   registration, `run_group_normalization` and `measure_ln_seeds_calibration` for LN) holds what THOSE stages need,
   sized by their own gates (the registration-parity gate, the LN rows) — measured, not assumed.
2. **Close pairs leave the frame statistic, not the catalogue.** A star with another seed within 1.5F is out of
   population (a); its fit is skipped only when list (b) does not need it. A merged pair (no second seed) can only be
   cut after its fit, by a shape rule the design read measures before adopting.
3. **Detection stays complete.** Every candidate keeps its record (position, filter SNR, flux; `PlaneMeasurement::
   detected`, the budget's `NotSelected` / `Trimmed` outcomes); the budget and the pair rule choose only which
   candidates get the PSF fit. The design read decides whether detection keeps the 24 576 cap once the fit no longer
   scales with it (detect is ≈ 0.2 s / plane).
4. **`Mode::All` stays**: the full fitted list for the probe, the Analysis star map, any caller that asks.

**Levers, in order (Fable Q5).** (1) The budgeted population — the 2–3× candidate; it moves `.athf` and every
statistic above, so it is judged on the whole group at master level. (2) The per-star cost — region pixels per fit,
LM iterations, allocation; a kernel change keeps every `.athf` bit-identical. Then the verdict pass — skip it for a
seed whose peak cannot reach the clip floor, bit-identical by construction or not at all. (3) The whole-plane passes
(background, noise, detect, the second read) with R12-8's Analysis B3 in the same family.

**Out of bounds** (Fable Q5 item 4): the 6σ threshold (R-OSP-2), the Moffat conditioning (R-OSP-3), the verdict rule
(R-OSP-5), the β estimator (R-OSP-6), the group-β two-pass (R-OSP-9). A speedup that misses a spec §8 tolerance row is
reverted with its numbers, never tuned toward the row.

**Bars.**
- **Speed (owner, 2026-10-16; supersedes M.2's 4.34 min): the Measure stage is faster than the ERP's measurement of
  the same frames on the same machine, on each of set 204, run B and Horse C11.** Ours = the stage wall, interleaved
  bracket, median of 3 (Task 13's m2 procedure). The ERP = its in-instance time including file loads (Task 13 Step 6).
  First re-time the ERP's OSC half in full chunks: Task 13's chunks of 3 under-used the cores, so 4.00 min is an upper
  bound and would make the bar soft. Run B's ERP time is measured, not derived. Horse C11 (ours 2.21 min against the
  ERP's 4.21) passes today and must stay faster. Reported beside: A (3.61 min), the per-plane sub-stage medians.
- **Analysis:** B3 ≤ 0.6 s per m42 frame (R12-8's miss, 0.70 s).
- **Quality:** the set 204 checkpoint re-run against 5836a970 at master level — masters, weights (with the ERP join),
  LN scale, registration parity — on the whole group. Any numeric move is a change of record with its numbers (R13-3's
  standard), never a silent one.

**Design read (2026-10-16 night; report `.superpowers/sdd/2026-10-07-one-star-path-plan-a/task-13b-report-design.md`,
scripts and outputs in the session scratchpad `t13b-design/`).** Scope, rules, lever order and bars above are kept;
where the code contradicts the stub the sharpening is marked **(S)**.

*The arithmetic (Q1).* Single-thread cost per plane (P-core s; `RAYON_NUM_THREADS=1` probe runs on set 204's
calibrated frames, attributed with the system sampler): mono wide 0058 12.7 (LM solves 7.7 — the verdict pass about
twice the pass-2 fit —, `signal_totals` 0.59), mono sharp 0075 7.3 (LM 2.6, `signal_totals` 0.84), OSC 0163 6.8 per
plane (LM 3.2); the whole-plane passes W (read to detection) 2.42–2.58 on the mono planes, the OSC planes taken
alike (≈ 2.6 with the second read and working copies). Per fit (verdict + pass 2 + photometry): 0.44 ms at
F 4.1, 0.13 ms at F 2.0, 0.30 ms on the OSC planes; flat across brightness (0058's top 1 910 / 3 810 / 9 502 seeds: 0.38 /
0.42 / 0.51 ms). The stage log (set 204 run 6, b6c5d5b3): Σ plane walls / 5 = the stage wall (mono 977 / 5 = 195 s
against 198; OSC 1 960 / 5 = 392 against 395), so the five workers are never idle; Σ single-thread cost ≈ 3 100 P-core s (modelled: 42 wide and 50 sharp mono
planes, 315 OSC planes)
against 593 s = 5.3 P-core equivalents (the dev Mac has 4 performance + 6 efficiency cores, ceiling ≈ 6.1–6.7):
concurrency headroom ≤ ~20 %, measured in Step 0. Phase means are 1.2–5.8× their medians in the same run (mono
background 1 929 / 334 ms, OSC detect 1 115 / 220): the per-phase walls carry the pool's queueing, so walls never rank
the costs — single-thread runs do. **W alone
is 407 planes × 2.6 = 1 060 P-core s = 200 s at today's throughput against the ERP's 240 s: lever 3 is required, not
optional (S).** Targets per plane: W ≤ 1.8; fits ≤ (mono 4 600, OSC 2 750) × the per-fit cost; totals ≤ 0.05 s → set
204 ≈ 1 140 P-core s ≈ 216 s at 5.3 — level with the ERP; the margin comes from the bit-identical items (≈ 5–10 %) and
concurrency (≤ 20 %). OSC planes are 75 % of the target cost: the ERP measures an OSC file at about its mono cost (1.18
against 1.26 s / frame), we measure three planes. No out-of-bounds item is a large cost once the population is budgeted
(the 6σ threshold drives detection only; the group-β two-pass costs a second read and working copy, ≈ 0.1 s / plane).
If Step 0's measured projection misses the re-timed ERP with every in-bounds lever at its target: STOP S1 to the owner
with the table (contingencies, not in this task: one detection per OSC frame shared by its planes; a warm-started pass-2
LM judged by tolerance).

*(S) Population (a) is not `Mode::Budget` as built.* R12-3 ranks a cell's candidates brightest-first and R12-4 trims by
SNR descending: the N brightest unsaturated stars per cell, which the owner's rule forbids (frame PSF statistics sample
across brightness, never the N brightest). One stratified selection (`star_measure/population.rs`) serves both modes;
`Mode::Budget`'s candidates switch to it (one path, R12-8; Plan C has no consumer yet; the Analysis rows are re-based).
Kept from Task 12: the 4 × 4 cells, the selection after detection and before the verdict pass (R12-2), every seed as a
neighbour (R12-5), the `NotSelected` / `Trimmed` records, the `BudgetReport`.

*(S) PSFSW is a sum, not a sample statistic.* `PSFSW ∝ TFlux · TMeanFlux / (σ_N · M*)`, TFlux and TMeanFlux sums over the
accepted fits (TMeanFlux RCR-winsorized: 6 482 of 20 014 values clipped on 0058), so it carries the star count. Over a
fixed-N budget the count cancels: the wide / sharp class ratio (0058/0059 against 0075/0076) moves +202 %; over the
fitted union (a) ∪ (b) it moves −12 … +16 % depending on M. The totals therefore run over the COMPLETE detection: an
accepted fit contributes its signal and mean flux; an unfitted seed its matched-filter flux × r (r = the plane's median
signal / flux over (a)'s accepted fits; rank correlation flux ↔ signal 0.971–0.980) over the frame-FWHM ellipse area,
counted with weight p (the accepted share of (a)'s faintest stratum). Measured with a simplified RCR: PSFSW / today
1.03–1.05, class differential 2 %. **Fable rules this estimator before Step 3 (a hard numeric choice).**

*(S) β and its sample stay.* The stub lists β in (a), but the β estimator and the group-β two-pass are out of bounds
(R-OSP-6, R-OSP-9; R11b-6: a band-sampled β arm did not move the 0000 defect). The β sample stays the first 64
qualifying seeds in brightness order — all inside list (b) — so every own β and the group β stay bit-identical (gated).

*(S) The verdict-pass skip is dropped.* It is not bit-identical by construction: the verdict fit's amplitude is
unbounded (`by_amplitude` cannot be ruled out from the peak), and `verdict_fit_failed` and the `.athm` `SeedPass` bytes
change. Its saving comes with lever 1: the verdict pass runs on the fitted set only (R12-2's order).

*(S) The kernel lever is small.* The LM solves are 36–61 % of a plane's CPU and cannot change bit-identically (a
continuous β takes `powf` per pixel where the old Moffat 4 took `powi`; pass 2 re-solves cold from moments); allocation
reuse, the region growth's exact median and buffer reuse are ≈ 5–10 %. The big bit-identical item is elsewhere:
`robust::rcr` is O(n²) per plane (`order.remove(0)`, the deviations re-sorted per removal): 0.22–0.84 s per plane.

*(S) LN rows cannot be an identity guard; registration can.* The LN reference is the top-N by weight (R13-4) and the
weights move, so the LN guard is the reference-corrected ratio (±0.5 %); the harness re-base (Task 14's carry) moves into
Step 0. Registration parity IS an identity: the reference is pinned, Register reads x, y, signal (sort key), amplitude,
background, fwhm, residual from `.athf` (no flags, no isolation; top 2 000 by signal after its cuts, `MIN_INLIERS` 8,
homography 30), and signal = model flux × `c` — a per-plane factor keeps the order.

*(S) ERP OSC chunks.* The 16 GB rule (≤ 10 targets of 26 Mpx per instance, one target at a time) caps an OSC chunk at 3
files; what chunks of 3 inflate is the per-instance warm-up, paid 35 times. The re-time measures the warm-up.

**Decisions (each with its evidence).**
- **List (b)** = the first M seeds in detector order (brightest-first by matched-filter flux). Mono: M = 2 ×
  `registration.max_stars` (4 000 at the default); OSC (Register re-detects on luminance, so LN only): M = 4 × LN's
  500-pair cap = 2 000. Evidence: on 0058 / 0059 / 0075 / 0076 the top 2 000 fits by signal that pass Register's cuts lie
  within seed rank 3 270 / 3 575 / 3 397 / 3 330 (M = 4 000 captures 100 %, M = 3 000 99.8–99.9 %); the top 500 within
  910–1 283; LN needs ≥ 20 matches, ≤ 500 pairs at peak SNR ≥ 40, 40 distinct for the spline, 100 for the growth curve.
  M folds into the measurement hash. The registration-identity gate decides; a miss raises the factor once (2 → 3, cost
  printed), else STOP S2.
- **Population (a)**, N = 1 000 for stacking (`Mode::Budget`'s N stays the caller's, 500 by default). Eligible: not
  extended; no other detected seed within 1.5 F (F = the detection field width; every seed counts, R12-5); not flagged by
  the pre-fit saturation clauses (plateau, near-clip, pixel 0, top 3 × 3); filter SNR ≥ 20 and below the eligible pool's
  95th percentile. Strata: 4 × 4 cells × 4 brightness quartiles (log filter SNR within the band) = 64, quota ⌈N / 64⌉
  water-filled, picks evenly spaced by rank inside a stratum (deterministic). Evidence (30 draws per N, four set 204 mono
  frames): FWHM sd N = 500 0.29–0.31 %, 1 000 0.15–0.24 %, 2 000 0.12–0.17 %; ecc sd 0.0042–0.0052 / 0.0029–0.0040 /
  0.0021–0.0027; HFR median 0.29–0.45 / 0.20–0.31 / 0.18–0.25 % — at N = 1 000 below the consecutive-frame difference
  (0058 → 0059: FWHM 0.37 %, ecc 0.005). Re-definition against today's all-fits value: FWHM +1.3–1.6 % on the wide
  frames, +0.0–0.35 % on the sharp; ecc −0.015 … −0.028; the SNR median 9–19 → 25–29.
- **(a)'s consumers:** `frame_shape` (FWHM, ecc), the F-6 medians, the ring width F (= the frame FWHM, so B_i, σ_i, SNR,
  HFR and `c` follow it), `c` (one qualifying fit per stratum = 64 = `CORRECTION_STARS`). Not (a): β (above), the PSFSW
  totals (above), the star counts.
- **Star counts:** `min_stars`, the D·ΔStars term and the `best_by_weight` / `sky_penalized_order` tie-breaks read
  `stars_detected` (the complete detection), so `minStars` keeps its meaning; `stars_fitted` = the accepted fits of
  (a) ∪ (b).
- **Close pairs:** the 1.5 F test runs on the seed list before the fit; a pair stays in (b) when ranked. **Merged pairs:
  no post-fit rule** — inside (a) they are 0.32–0.86 % of the fits (1.5–2.7 % of all fits: the SNR floor removes the
  faint junk) and move (a)'s FWHM +0.05–0.20 %, ecc +0.0003–0.0013, at or below the N = 1 000 sampling sd; re-opened if
  Step 5 prints a merged share > 2 % in (a) on a dataset.
- **The 24 576 cap stays:** it cuts at filter SNR 9.4 (0075); (a)'s floor is 20 and (b) ends near filter SNR 73–154;
  raising it costs detection (knot tests per candidate) and moves the ring masks on the 50 capped planes, for no consumer.
- **`Mode::All` stays op for op** (the probe's `measure` path, the Analysis star map, any caller that asks); the stacking
  Measure runs `Mode::Population`; the probe gains `--paths population` (the lab and `probe_parity`).
- **Versions:** `MEASURE_VERSION` 4 → 5 (every statistic moves); `PASS1_ARTIFACT_VERSION` 1 → 2 (`PlanePrep` gains the
  selection; the golden pin re-pinned). Release note: the first run after the upgrade re-measures.

**Files.**
- New: `crates/athenaeum-core/src/star_measure/population.rs` (`rustfmt` by path).
- `star_measure/mod.rs` (`Mode::Population`; `prepare_plane` / `plane_beta` / `finish_plane` under it; `PlanePrep`
  gains the selection; read→working fusion), `budget.rs` (`select_candidates` → the stratified rank), `fit.rs` (verdict
  pass and `fit_after_passes` over the fitted set; scratch reuse), `photometry.rs` (`aperture_correction`'s sample from
  the strata; ring F from (a); SNR / HFR buffers), `frame.rs` (`FrameAggregates` over (a), `signal_totals_complete`),
  `robust.rs` (`rcr` O(n) per removal, bit-identical), `domain.rs` (native sky sample at the read; mesh / MRS buffer
  reuse), `seeds.rs` (`d` fused into the first filter pass; buffer reuse).
- `stacking/measure.rs` (the adapter passes `Mode::Population` and M; 13b-P debug timers), `stacking/pass1_artifact.rs`
  (version 2), `stacking/plan.rs` (`measurement_hash_for` folds M), `stacking/weights.rs` (star counts →
  `stars_detected`), `stacking/run.rs` (hand edits only, and only if Step 4's scheduling item lands).
- `crates/athenaeum-core/examples/star_probe.rs` (`--paths population`).
- athenaeum-lab: `harness/acceptance/t13b.sh` (ERP warm-up rounds, the H / N brackets, CPU-s), `tier1/t13-tools.py`
  (means beside medians, CPU-s per plane, the reference-corrected LN row), the star-lab cycle `t13b-pop`.
- Docs: `docs/stacking/README.md` R-OSP-12, `docs/settings/README.md` (`minStars` reads the detected count), this
  task's Outcome.

**Interfaces.**

```rust
// star_measure/population.rs
pub const STATISTIC_STARS: usize = 1000;   // N, stacking
pub const STATISTIC_SNR_FLOOR: f64 = 20.0; // filter SNR
pub const STATISTIC_TOP_QUANTILE: f64 = 0.95;
pub const BRIGHTNESS_STRATA: usize = 4;
pub const ISOLATION_PER_FIELD_FWHM: f64 = 1.5;
pub struct Population { pub fitted: Vec<usize>, pub statistic: Vec<usize>, pub stratum: Vec<u8>, pub list: usize }
pub fn select(seeds: &[Seed], pre_saturated: &[bool], w: usize, h: usize, field_fwhm: f64,
              statistic: usize, list: usize) -> Population;
// star_measure
pub enum Mode { All, Budget { budget: usize }, Population { statistic: usize, list: usize } }
// star_measure::frame
pub fn signal_totals_complete(fits: &[StarFit], fit_seed: &[usize], seeds: &[Seed], pop: &Population,
                              area_f: f64) -> SignalTotals;
```

Unchanged: the `.athf` layout (its content = the accepted fits of `fitted`), Register and LN code, every command,
`ts_export` (no new type crosses; `stars_detected` is already serialized).

**Steps** (lean-execution: one phase per agent, reports ≤ 80 lines; one heavy job at a time on the dev Mac).
**Controller amendments (2026-10-16 night).** (1) 13-P is already on `one-star-path`: 40bad38c and its review fixes
c2713ca2 were cherry-picked and squashed into 5836a970 (`git diff c2713ca2 5836a970 -- crates/` is empty), so Task 14's
13-P carry is closed and Step 0 builds on HEAD. (2) The owner is asleep and delegated: STOP S1 / S2 / S3 go to a Fable
ruling (owner: «если что спрашивай у fable»), recorded in `task-13b-rulings.md` with its cost if wrong; the owner may
overturn it in the morning. (3) Push is allowed (owner: «пуш разрешаю тоже»): code reaches the runner through GitLab
`origin` work branches, and Step 6 pushes `one-star-path` after the full `--all-targets` re-gate.

- [x] **Step 0 — baseline and profile (dev Mac; sonnet; ≈ 3 h).** 13b-P on top of HEAD (13-P included, amendment 1):
  additive `debug!` timers (wall
  and thread CPU) for every untimed sub-step — working copies, `sky_level`, `saturation_plane` with its queue wait, mesh statistics
  against map evaluation, MRS layers against its serial loops, the detection passes, `signal_totals`, ring sky, `c`, SNR /
  HFR, the `.athf` write — folded into Step 1's commit. Lab tools (haiku): `t13b.sh`, the reference-corrected LN row, CPU-s
  per plane from `ps -o cputime` at the stage's bounds. Runs: the ERP re-time (set 204 OSC chunks of 3 with 1-file chunks
  interleaved, 3 rounds; mono 10-file against 1-file; run B measured the same way; Horse C11 warm-up only; the owner
  closes their own ERP instance first); one H Measure-only run on a set 204 clone at 5836a970 with 13b-P (CPU-s,
  per-sub-step means). Output: the projection table (each lever's measured share against the re-timed ERP). **STOP S1**
  if it misses with every in-bounds lever at its target.
- [x] **Step 1 — the bit-identical items (rust-engineer sonnet; ≈ 3 h; review opus).** `rcr` O(n) per removal (a
  property test against the old implementation on random and real vectors), read→working fusion with the native sky
  sample, `d` fused into the first filter pass, mesh / MRS / detection buffer reuse, SNR / HFR / LM scratch reuse (the
  rustafits LM's four allocations only if Step 0 shows malloc ≥ 5 % of LM time — then a new entry point, F7). Proof
  (haiku): `--measure-identity` bar 1 on M42 and set 204 (OSC 105 + mono 92) 0 / 0; the runner: `probe_parity` on the lab
  corpus 0 moved. Single-thread re-time of the three planes.
- [x] **Step 2 — Fable rules** the PSFSW totals estimator and confirms the (a) rule set (N, band, strata, `c`).
- [x] **Step 3 — populations (rust-engineer sonnet; ≈ 5 h; review opus).** `population.rs`, `Mode::Population` through
  the two passes, `Mode::Budget` on the shared rank, `signal_totals_complete`, the star-count consumers, versions,
  `--paths population`. Gates: `star_measure::`, `stacking::`, `star_probe`, `check --all-targets`,
  `--no-default-features`; the registration identity and the β identity on set 204 (STOP S2 on a miss after the one
  factor raise). The runner (sonnet, judgment): star-lab cycle `t13b-pop` — noreg against the Task 12 base with the spec
  §8 tolerance rows, cumulative report; Analysis rows B1 / B2 re-based.
- [x] **Step 4 — the measured remaining gap (rust-engineer sonnet; ≈ 3 h).** Only as far as Step 3's re-time leaves a gap:
  numeric whole-plane items (separable map evaluation; a map evaluated only where read), each with its tolerance row and
  master-level check; Measure frame workers scheduled as pool tasks if Step 0 showed throughput < 85 % of the core
  ceiling. A miss after these: STOP S3 with the numbers (never tuned toward a row).
- [x] **Step 5 — acceptance (dev Mac; sonnet judgment run; ≈ 5 h).** Measure-only brackets H N H N H N on clones of
  `tierA-lnB-f1` run 5, `lnB-runB-f1` run 6, `hc11B-f1` run 5 (before reference pinned `manual`, `twoPass = false`; H =
  5836a970); Analysis B3 (Task 12 bracket (b)); the set 204 quality run N from Measure on a clone, against
  `starA-s204` read-only as H (never modified). Prints: per-plane means and medians, CPU-s per plane, (a)'s merged
  share, fitted counts.
- [x] **Step 6 — review and record (opus review, Fable if it struggles; sonnet docs; ≈ 2 h).** README R-OSP-12, settings
  doc, this Outcome, ledger; squash on `one-star-path`, then push to GitLab `origin` after the full re-gate (amendment 3).

**Acceptance.**
- Speed: the Step 5 bracket median of N < the ERP's warm-up-corrected total (marginal per-file × files + one warm-up;
  the in-instance sum printed beside) on each of set 204, run B and Horse C11.
- Analysis: B3 ≤ 0.6 s per m42 frame.
- Quality (set 204, N against H, whole groups): registration results identical (identity mode); every plane's own β
  and the group β identical; weights ρ(N, H) ≥ 0.9 per group and the ERP join ρ(N) ≥ ρ(H) − 0.01 (H: mono 0.956, OSC
  0.936); LN reference-corrected ratio within ±0.5 % (raw ratio, top-N overlap and anchors beside); masters per Task
  13's sub-check rows, each move a change of record with its numbers.
- The lab: `t13b-pop` noreg PASS with every spec §8 tolerance row; Step 1's identity 0 / 0.
- Gates: as Step 3, and the full `cargo test -p athenaeum-core --all-targets` before any push.

**Outcome (2026-10-17; branch `one-star-path-t13b`: 76aa53d5 plan ... 3f275540 route A final, 3b8b1e41 review fixes, then
the docs commit; lab 591c556, 4b18f85, 086309c, 2b89a75; rustafits 07b8287 on `one-star-path-13b`; not squashed or
pushed to `one-star-path`).** **Route A (owner, 2026-10-17, "A: как раньше"): the stacking Measure's numbers are
5836a970's exactly** (probe identity 495 k lines, 0 differing; `MEASURE_VERSION` 4 and `PASS1_ARTIFACT_VERSION` 1
restored), only the bit-identical speedups remain: rustafits mesh 1.16 -> 0.58 s and MRS 0.62 -> 0.11 s per plane, `rcr`
by verified decisions x4.5 (rigorous bound, exact recompute on undecided iterations), `hfr` and ring-sky selection, the
13b-P timers gated on debug. **Brackets (H 5836a970, N 3f275540, interleaved x3):** set 204 **9.90 -> 7.95 min (N / H 0.803;
ERP 3.77 min, 2.11x, bar MISSED)**; Horse C11 **2.22 -> 1.43 min (0.647; ERP 3.71 min, 0.39x, bar MET)**; run B not run;
Analysis B3 (Task 12 bracket (b), m42, 3 rounds, 3b8b1e41 rebuilt): analysis-mode 35.7 s / 77 = **0.464 s per frame — PASS** (bar 0.6; Task 12 0.723), legacy analysis 18.5 s / 77 = 0.240 (Task 12 0.339) — the rustafits mesh / MRS cut. **Proofs:** lab `t13b-id` 591c556 and `t13b-final` 4b18f85 0 moved (only the analysis-mode rows
move, by the Budget selection); set 204 Step 5a master pixels, weights, LN and registration identical. **Refused (measured
out):** the budgeted population with imputed PSFSW totals (depth tilt -5.5 / -4.0 % against H -0.5 / -0.9 %, per-frame
tails +-40 %; rcr-bounds test C rho 0.56) and the (a) frame shape (mixed against truth, hc11B G +30 %); kept unused:
`Mode::Population`, `population::select`, the probe's `--paths population`. **Steps as run:** Steps 0-2 as planned; Step 3
built the population and was STOPPED at 3c (R13b-8 miss, R13b-9 moved the referee to synthetic truth after the miss,
R13b-11 refused the imputed totals); Step 4 became 4a (`rcr`) only; Step 5 was Step 5a (identity run) + 5b (Measure-only
brackets on set 204 and Horse C11); Step 6 holds the opus review (0 C / 3 I / 5 M, fixed in 3b8b1e41) and these docs; the
settings doc is unchanged (`minStars` reads `stars_fitted`). **Carries:** the weight-redesign cycle "faster than the ERP"
(a truth-validated weight; the (a) frame shape with a truth gate); Task 13c (parallelism); `c` against `c_true` on
undersampled OSC, doubles, nebulae; the knot exemption's plane-max band (Task 10c); the squash onto `one-star-path`, the
full `--all-targets` re-gate and the push. README R-OSP-12.

---

### Task 13c: Parallelism research — use the machine fully, numbers unchanged (owner, 2026-10-17; inserted before Task 14)

**Scope fixed 2026-10-17 by the owner ("Добавь исследование распараллеливания расчетов, чтобы оптимально и по
максимуму использовать ресурсы"); Files / Steps by the task's design read.** Task 13b closed the bit-identical CPU work
(rustafits mesh 2×, MRS 5×, `rcr` 4.5×, hfr) and left Measure at ≈ 477 s on set 204 with every seed fitted (route A).
This task asks the other question: is the machine fully used while the pipeline runs, and where does it wait.

**What is known.** Step 0c (Task 13b): the Measure stage kept ≈ 8.6 of 10 cores busy on the dev Mac (4 performance +
6 efficiency cores, 16 GB) with 5 frames admitted on a 10-thread pool, 12.6 CPU-s per plane; the design read estimated
≤ 20 % concurrency headroom from a single stage. Never measured: the other heavy stages (Calibrate, Register, Normalize,
Integrate, Drizzle), the performance- vs efficiency-core split, I/O wait, memory-bandwidth limits, serial sections
between stages, and the runner class (M4 Max, 16 cores, 128 GB), where admission and pool sizes tuned for 16 GB may leave
most of the machine idle.

**Questions (the research note answers each with measurements on BOTH machine classes):**
1. Per stage: wall, CPU-s, cores busy / available over time, P- vs E-core time, peak memory, I/O wait; where the
   pipeline is serial (Amdahl fraction per stage and between stages).
2. Admission and pools: the `ComputeQueue` / per-stage admission counts and rayon pool sizes — fixed numbers or derived
   from cores and RAM? Nested pools or oversubscription? A memory-budget-driven admission (frames in flight = what
   fits in RAM with a margin) instead of a constant.
3. Granularity: per frame vs per plane (an OSC frame's three planes), the work-stealing grain inside a plane, idle tails
   at the end of a fan-out (the last frames running alone).
4. Overlap: reading / decoding the next frame while the current one computes (prefetch), streaming frames between stages
   (a calibrated frame measured while the next calibrates) instead of stage barriers, disk-as-cache reads that could be
   memory-mapped.
5. Hardware: the efficiency cores (do they help or slow the tail), SIMD in the hot loops (auto-vectorized or not), and
   the GPU as research only (what would move there; no implementation in this task).

**Scaling to the machine (owner, 2026-10-17: «ресурсы могли динамически скейлится в зависимости от платформы и
доступных ресурсов» → clarified: «оно может быть подобрано под машину, но так чтобы утилизировать все доступное»).**
The goal is that every machine is used to the full of what it has — not one machine's constants applied everywhere. The
budget may be fixed per machine (chosen when the run starts); in-run adaptation is needed only where measurement shows
it is (e.g. memory pressure on a small machine):
- **Platform discovery at start:** physical / logical cores and the performance / efficiency split (Apple silicon
  perflevels, hybrid x86), total and available memory, and the container limits of the Docker / web build (cgroup v1 /
  v2 CPU quota and memory limit — a container must never size itself from the host's numbers); macOS, Windows and Linux.
- **Sizing for full use:** frames in flight and pool widths chosen from those numbers and the measured per-frame peak
  memory so the cores stay busy without swapping — on the 16 GB dev Mac AND on the 128 GB runner (where today's numbers,
  tuned for 16 GB, likely leave most of it idle). A guard only where measured: back off under memory pressure, and share
  the `ComputeQueue` with a concurrent scan or export instead of oversubscribing.
- **User control:** the Settings → Stacking / compute knobs stay as caps and overrides of the automatic budget
  (`docs/settings/README.md` contract), with the chosen budget logged at `info` (`stacking` start line: cores, memory,
  frames in flight, pool sizes, why).
- **Determinism:** results must not depend on the thread count or the admission level — the same bytes at 1, 4, 10 and 16
  threads and under a forced scale-down mid-run (a test matrix on the probe and one stacking run).

**Bars.** Every change is bit-identical (deterministic reduction order; the 13b identity procedure: probe `cmp` on
set 204 0058 / 0075 / OSC 0163, the lab cycle 0 moved, set 204 master pixels identical) at every thread count and
admission level; speed is judged by interleaved brackets on the dev Mac AND the runner (and one constrained container
run: a Docker build with a CPU quota and a memory limit below the host's); peak memory stays within the available budget
on every platform (the 16 GB rules on the dev Mac; no swap); no regression on the small machine. A change that pays < 5 % of a stage or costs memory the small machine does not have is recorded,
not merged.

**Deliverable.** A research note `docs/superpowers/research/2026-10-XX-stacking-parallelism.md` (the measurements, the
ranked levers with their measured gains), then the bit-identical changes that pass the bars, in phases (lean-execution).

---

### Task 14: One full uniform lab run and the "after Plan A" note (spec §5 "At the end")

**Files:**
- Create: `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-a.md` (athenaeum; `XX` = the run's day)
- Modify: `docs/stacking/README.md` (the section's closing paragraph), `.superpowers/sdd/2026-10-07-one-star-path-plan-a/progress.md`
  (the ledger)
- Lab: `results/plan-a/final/{gates.txt,scorecard-summary.md,scorecard-inject-summary.md,divergent-families.txt,
  noreg-vs-baseline.txt,noreg-vs-t12.txt,path-compare.txt,twin-saturation.txt,corpus-table.md,provenance.txt,
  run-log.txt}`, `results/plan-a/design-cumulative.json` (the union after Task 12, unchanged here)

- [ ] **Step 1: One uniform run, in this order, one job at a time, every command and wall logged in `run-log.txt`:**

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum && git status --porcelain --untracked-files=no   # clean
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo build --release -p athenaeum-core --example star_probe --example ln_scale_probe --example measure_probe
cd ../athenaeum-lab/star-lab
S=~/.athenaeum-acc/star-lab-a/final; R=results/plan-a/final; mkdir -p $S $R ~/.athenaeum-acc/star-lab-a/results/final
export STAR_PROBE_ARGS="--diagnostics"
python3 bin/run_probe.py manifests/v2.json     --out $S/probe        && python3 bin/run_probe.py manifests/v2.json     --out $S/probe        --verify
python3 bin/run_probe.py manifests/inject.json --out $S/inject-probe && python3 bin/run_probe.py manifests/inject.json --out $S/inject-probe --verify
python3 bin/run_probe.py manifests/corpus.json --out $S/corpus-probe && python3 bin/run_probe.py manifests/corpus.json --out $S/corpus-probe --verify
python3 bin/scorecard.py manifests/v2.json     --probe-dir $S/probe        --erp-dir ~/.athenaeum-acc/star-lab-v2/erp --out $S/scorecard.md        --json $S/scorecard.json
python3 bin/scorecard.py manifests/inject.json --probe-dir $S/inject-probe --out $S/scorecard-inject.md --json $S/scorecard-inject.json
python3 bin/scorecard_summary.py $S/scorecard.json        --out $R/scorecard-summary.md        --title "Plan A final: v2 after Task 13"
python3 bin/scorecard_summary.py $S/scorecard-inject.json --out $R/scorecard-inject-summary.md --title "Plan A final: injections after Task 13"
python3 bin/baseline_flags.py   $S/scorecard.json --out $R/divergent-families.txt
python3 bin/twin_saturation.py  manifests/v2.json --probe-dir $S/probe --out $R/twin-saturation.txt
python3 bin/path_compare.py     $R/scorecard-summary.md --data-root ~/.athenaeum-acc/star-lab-v2 --probe-dir $S/probe --out $R/path-compare.txt
python3 bin/star_test_corpus.py table manifests/corpus.json --probe-dir $S/corpus-probe --out $R/corpus-table.md
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-a/results/t12-amode/scorecard.json.gz $S/scorecard.json --out $R/noreg-vs-t12.txt            # no design list: Task 13's one code commit (13-P) adds debug log lines only, no number moves → 0 moved
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-v2/results/baseline-2026-10-07-fw/scorecard.json.gz $S/scorecard.json --out $R/noreg-vs-baseline.txt --design results/plan-a/design-cumulative.json
gzip -k $S/scorecard.json && mv $S/scorecard.json.gz ~/.athenaeum-acc/star-lab-a/results/final/
STARLAB_ACC_COPY=~/.athenaeum-acc/starA-s204 STARLAB_ACC_RUN_ID=6 STARLAB_SCRATCH=$S/gates python3 bin/gates.py > $R/gates.txt
grep -h '"kind": "provenance"' $S/probe/run_log.jsonl $S/inject-probe/run_log.jsonl $S/corpus-probe/run_log.jsonl > $R/provenance.txt   # the three one-binary records
```

  `results/plan-a/design-cumulative.json` is the union of every task's design list with each entry's `step` tag
  (PA-19), so the final diff against the ORIGINAL baseline prints the by-design rows in their table and judges the
  rest: 0 moved expected (ruling F21, M-13). Task 5's `one-sided` rows keep their rule in the final diff too —
  judged against the original baseline by the same one-sided test wherever no later step set them aside, never
  silently set aside. The gates: 13 PASS expected (`probe_parity` on the `starA-s204` clone's run 6 artifacts
  written by b6c5d5b3 (13-P adds debug lines only, no number — the same measurement code as this run's probe, ruling F12); `determinism` covers 170
  families). Only one clone is on disk for this task; no before tree was touched at any point.

- [ ] **Step 2: The note** `2026-10-XX-star-measurement-after-plan-a.md`, beside the baseline and in its format
  (numbers copied as printed; derived numbers marked): §1 verdict per bar (F-0…F-7, the budget, the costs, the real
  data — one line each: bar, baseline cell, after cell, met/missed; the F-1 `gauss` cells of Task 6b re-read after
  Task 9, PA-20 a); §2 method (binaries' sha256, athenaeum HEAD, rustafits sha, lab sha, the one-binary proof
  counts); §3 rows 1–11 in the baseline's tables with the after cells; §4 real data (Task 13's rows for the three
  datasets); §5 what Plan B starts from (the LN rows, PA-14, printed); §6 what Plan C needs (the `analysis-mode` vs
  `legacy-analysis` comparison on the corpus and on `sharp/*`, the `_DSC5767` seed counts, the cost); §7 open items
  (VNG per-plane widths and the resampling broadening restated with their after numbers; any missed bar with the
  owner's ruling).

- [ ] **Step 3: The full core test suite before the owner's word** (`feedback_full_core_test_before_push`), one
  filter-free run per crate set (ruling F15):

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | tail -5
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -p athenaeum-tauri --lib -- --test-threads=4 2>&1 | tail -5
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test --manifest-path rustafits/Cargo.toml --all-targets -- --test-threads=4 2>&1 | tail -5
CARGO_TARGET_DIR=.superpowers/target-acc cargo check -p athenaeum-core --no-default-features
npx tsc --noEmit && npx vitest run
grep -rn -i -E "$(python3 -c 'import sys; sys.path.insert(0,"../athenaeum-lab/star-lab/bin"); import run_probe; print(run_probe.erp_pattern())')" crates/ src/ docs/ rustafits/src ../athenaeum-lab/star-lab ../athenaeum-lab/harness | grep -v 'erp/run_job.sh' | wc -l   # 0 (the name is read from the launcher, never typed)
```

- [ ] **Step 4: Commit** (athenaeum, not pushed; lab pushed), then delete the last clone (`rm -rf
  ~/.athenaeum-acc/starA-s204` — its gates output is committed; the before tree `tierA-lnB-f1` stays), and hand the
  branch to the final review.

```
docs(research): star measurement after Plan A - one uniform lab run, every bar against its baseline cell, the real-data checkpoint
```

**Acceptance:** 13 gates PASS; `noreg-vs-t12.txt` 0 moved; `noreg-vs-baseline.txt` 0 moved outside
`design-cumulative.json` and 0 one-sided misses; every Plan A bar line in the note's §1 reads "met" or carries the
owner's ruling; the full core suite green on both repositories; the ERP's name absent.

---

## Spec coverage

| Spec item | Task |
| ---- | ---- |
| §2.1 module units `domain`, `fit`, `frame`, `robust` move; sample statistics (`SkyLevel`, ruling F4) | 2 |
| §2.1 `seeds` (matched filter, default seed source, field width PA-17 with no saturation test F25) | 6a, 7 |
| §2.1 `beta` | 9 (discrete moved in 2) |
| §2.1 `photometry` (saturation test, annulus σ, flux, SNR, HFR) | 8, 10, 11 |
| §2.1 `budget` | 12 |
| §2.1 `diagnostics` | 4 |
| §2.1 public surface `measure_plane`, `fit_stars_at`, `MEASURE_VERSION` | 2, 3 |
| §2.1 the saturation test as revised 2026-10-09 (plateau test, level, censored free-β verdict fit on every seed, flag A > range, known limit, truth flag, self-consistency row FN ≤ 0.5 %, FP reported; per-seed order F9; PA-23 supersedes PA-8) | 1b (row), 8, 9 (the verdict fit on the bounded fitter) |
| §2.1 exemptions (LN L-14/L-23/peak-SNR, registration cuts) | untouched (2, 10 leave `ln/selection.rs`, `register/detect.rs` as they are) |
| §2.2 stacking adapter, `psf_signal` formulas, LN refit via `fit_stars_at`, master statistics | 2, 6a (`field_width_of` for given seeds), 10 |
| §2.2 `stars_fitted` meaning, unsaturated statistic, `stars_refused` + per-reason counts | 4, 8 |
| §2.2 Measure config: `seed_source`, legacy decode of every stored document, Median3 = Peak's, R-M4c-11 | 7 |
| §2.2 `psf_model` Auto = continuous β (in stacking the group β), Moffat4 kept, `STACKING_CONFIG_VERSION` unchanged | 9, 7, 11b |
| §2.2 Analysis/Blink, not touched (plate-solve detector, rustafits analysis), the lab's two modes + `legacy-analysis` | 1b (the alias, F2), 12 (the arm), Plan C (the switch) |
| §3 doctrine, F-0 | 4 |
| §3 F-1 (region, cap, rim background, width start, seeds, new families, donut and its ring-radius floor) | 1a (families, PA-9), 6a, 6b |
| §3 F-2 (axes, conditioning, conditional convergence change as a new entry point F7) | 5 |
| §3 F-3 (unsaturated sample/statistic, twin bars, re-judged after F-4; the near-clip class and the M42 β bar) | 8, 9, 10c |
| §3 F-4 (continuous β, rustafits bound and ln β, Gaussian limit, every β a finite number, the linear arm kept F8) | 9; 11b (the stacking group β, the own β a diagnostic), 11c (the first pass spilled) |
| §3 F-5 (flux, aperture correction, psfSel, the pairs families F24) | 10 |
| §3 F-6 (SNR definition, gain `EGAIN × ATH_CSCL` F6, lab cards, truth SNR with the annulus term F5) | 1a, 8 (the annulus), 11 |
| §3 F-7 (rim background under nebulosity, every `neb*` family F23) | 6b |
| §3 F-8, F-9 | Plan B (the LN rows are printed, PA-14) |
| §3 F-10 (one background: propagated σ_f, knot tests, the field width, the trailed verdict from the refusal structure, B_i / σ_i per star, the LN ring arm, the owned-quantities table; one joint cycle re-judging F-0…F-5, F-7) | 10b (Task 11 reads B_i / σ_i; Task 13 confirms); 10c (the near-clip class) |
| §3 raw-mode truth (injections: completeness, FWHM) | 6b, 9 |
| §3 the budget (≥ 90 % of cells), the `NotSelected`/`Trimmed` identity F13 | 12 |
| §3 no regressions (SE rule incl. per-axis, mechanical, step and cumulative F21, design-list semantics F20, `betas@*`, row-2 keys F10) | 1c (tool), every fix task, 14 |
| §3 cost (Measure ≤ +20 %, Analysis ≤ 0.6 s incl. on-demand) | 10b (the first Measure read), 12, 13 |
| §3 out of scope (VNG, resampling) | 14 §7 (restated) |
| §4.4 `MEASURE_VERSION` in the three stage hashes, the pinned tests, the bump policy | 3 |
| §4.4 `measure_version`/`stars_refused` columns, `FrameAnalysis` fields, collab meta | Plan C |
| §5 step 1 (lab first) | 1a, 1b, 1c |
| §5 step 2 (extraction gate) | 2 |
| §5 step 3 (F-0) | 4 |
| §5 step 4 (one fix per step, config with F-1 and F-4) | 5, 6a, 6b, 8–11; the config in 7 and 9 |
| §5 step 5 (real-data checkpoint, pinned reference with `twoPass = false` F18, pass bars, clone order F11) | 13 |
| §5 "At the end" (uniform run, after report) | 14 |
| §6 global constraints | Global Constraints |
| §7 risks (first run re-measures; speed) | 3 (risk line), 12, 13 |
