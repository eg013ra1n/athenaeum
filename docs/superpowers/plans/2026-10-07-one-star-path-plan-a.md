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
  point in ln β; athenaeum bumps the submodule pointer. The convergence-test change is conditional (Task 5).
- **athenaeum-lab** (`star-lab/`): new families, truth columns and cards, the probe plan for the new arms, the
  scorecard rows the bars read, and `bin/scorecard_diff.py`, the mechanical no-regression check.
- **Data**: families under `~/.athenaeum-acc/star-lab-v2/` (the one manifest `manifests/v2.json`); every Plan A
  probe run and scorecard under `~/.athenaeum-acc/star-lab-a/<step>/` (ruling PA-3).

**Tech stack:** Rust (athenaeum-core, rustafits), Python 3.14 + numpy 2.4 (lab, `unittest`), React/TS (the Measure
panel), `sqlite3` with `immutable=1` for reading acceptance copies, the athenaeum-lab acceptance harness
(`harness/acceptance/`).

**Spec:** `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` (HEAD 61a03558, branch
`one-star-path`). Review history: `.superpowers/sdd/2026-10-07-one-star-path-spec/spec-review.md`. Code maps:
`measure-path-map.md` and `analysis-consumers.md` in the same directory. Baseline:
`docs/superpowers/research/2026-10-07-star-measurement-baseline.md` (lab e65af49, probes from f5321979).

## Global Constraints

- No third-party program name anywhere in this repository (code, comments, docs, commit messages); the comparison
  program is "the ERP". Lab automation lives in athenaeum-lab only.
- Never read or write the production catalog; acceptance copies only as `sqlite3 "file:…?immutable=1"`, working
  copies via `prepare-catalog.sh`.
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `calibration_library/light_cal.rs`,
  `stacking/measure.rs`, `stacking/register/detect.rs`; no `cargo fmt`. Those files take hand edits only; `rustfmt`
  runs only on the new `star_measure/*.rs` files, each named by explicit path.
- Cargo with `CARGO_TARGET_DIR=.superpowers/target-acc`, `CARGO_BUILD_JOBS=4`, `--test-threads=4`; one heavy job at
  a time with `memory_pressure` ≥ 30 % free; stop below 100 GiB free disk.
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
  (Task 5 step 7).

## Review Focus

1. **Planes with NaN or zero-padded borders after registration** (`resampled/*` and every registered frame LN
   refits through `fit_stars_at`; `register/frame.rs` reads `.athf` of calibrated planes, LN of warped ones). The
   matched filter must treat a non-finite pixel as absent (no NaN propagation into the convolution, no seed on a
   border), the region growth skips non-finite pixels as today, the rim background start and the SNR annulus ignore
   them, and a star whose aperture leaves the finite area is refused `OutsideImage`. Pinned in Task 6
   (`nan_border_seeds_nothing_on_the_border_and_fits_every_interior_star`) and Task 11
   (`snr_annulus_skips_non_finite_pixels`); the probe's `nan_border_runs_every_path` keeps passing at every task.
2. **Very sparse frames under the β sample's minimum** (fewer than 8 free-β fits on unsaturated stars: a 3-star
   frame, a flat field, a frame whose bright stars all saturate). β falls back to 4.0 with a `warn!`,
   `PlaneMeasurement.beta_source = BetaSource::Fallback`, and the frame still measures. Pinned in Task 9
   (`beta_sample_below_eight_falls_back_to_four_and_says_so`) and Task 4 (a starless plane: zero seeds, zero
   outcomes, no panic — `starless_plane_has_no_outcomes_and_no_panic`).
3. **An all-saturated core field** (`saturated/f2.6/n60`'s 64 brightest, a deep twin with 235 of 629 clipped, a
   plane whose every bright star is flagged). The β sample, the width start, the aperture correction and the frame
   statistic use the unsaturated subset; when it is empty the frame statistic is `None` → `fwhm_px = 0` with
   `stars_unsaturated = 0` reported, never a statistic over clipped cores. Pinned in Task 8
   (`all_saturated_sample_leaves_the_frame_statistic_empty_not_wrong`) and Task 10
   (`aperture_correction_without_an_unsaturated_isolated_star_is_one_and_flagged`).
4. **A plane whose noise estimate is refused** (MRS at its floor on a constant or near-constant plane; `N*` under
   100 residuals). The matched filter's threshold has no σ: the detector returns no seeds with one `warn!`, the
   flat-top test and the SNR take `σ_local` from the star's own annulus (never the plane σ), and `NoiseModel.source
   = NoiseSource::None` is reported. Pinned in Task 6 (`refused_noise_estimate_seeds_nothing_and_warns`).
5. **A star at the image edge** (the nominal stamp partly outside; the region cap clipped by the border; an
   aperture or annulus crossing it). Admission refuses `OutsideImage` when the width-start box leaves the image;
   a region clipped by the border fits only when its radius still holds `0.5·growth·FWTM`; the SNR annulus uses the
   pixels present and reports `n_sky`. Pinned in Task 4 (`edge_seed_is_refused_outside_image_with_its_reason`) and
   Task 11 (`edge_star_snr_uses_the_present_annulus_pixels`).
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
  `twins/*` t1 noise replicas and the 7 `background/*` images, `k` is the smallest of {3.0, 3.5, 4.0, 4.5, 5.0, 6.0}
  at which the scorecard's `false detections / Mpx` cause `nothing` (a seed that matches no truth star, artefact or
  star wing) stays ≤ 0.5 per Mpx on every image. The chosen value is recorded in `docs/stacking/README.md` (ruling
  R-OSP-2) and in `seeds::DEFAULT_THRESHOLD`'s doc comment with the table it came from; the bars are measured after.
- **PA-3 Data roots.** New families are generated into `~/.athenaeum-acc/star-lab-v2/` (one manifest, one
  determinism gate). Every Plan A probe run, scorecard and gates scratch goes to `~/.athenaeum-acc/star-lab-a/<step>/`
  (`run_probe.py --out`, `scorecard.py --probe-dir`, `STARLAB_SCRATCH`), where `<step>` is the task's tag (`t02-extract`,
  `t04-f0`, …). At most two `<step>` directories exist at a time: a step's probe directory is deleted once its full
  scorecard is gzipped under `star-lab-a/results/<step>/` and its summary is committed. `star-lab-a` is capped at 4 GB.
- **PA-4 `MEASURE_VERSION = 4`** continues `PSF_FIT_VERSION`'s count (3 → 4) and is set once, in Task 3. Plan A bumps
  it no second time: the branch merges as one unit, and every acceptance copy Plan A stacks is a fresh copy run
  from Calibrate. Policy: bump once per merged plan whose code changes what a fit accepts or measures (spec §4.4);
  Plan C sets 5; a plan that changes no fit leaves it.
- **PA-5 F-5's flux lands in `StarFit.signal`** (the same `.athf` slot, `FITS_ARTIFACT_VERSION` unchanged); `area`
  keeps its FWTM-ellipse meaning, so `mean_flux()` stays monotonic in the flux. Register's flux ordering and
  LN's seed ordering read `signal` as before.
- **PA-6 `fit_stars_at`** returns the extended `FitOutcome { fits, beta, seeds, refused: RefusalCounts }`; LN keeps
  calling `fit_stars`/`fit_stars_with_beta` through `psf_signal`'s re-exports, which are thin wrappers over it.
- **PA-7 The rustafits change** is a NEW entry point `fit_moffat_2d_bounded(pixels, init…, beta_bounds: (f64, f64),
  max_iter, conv_tol, max_rejects) -> Option<Moffat2DBoundedResult>` whose eighth LM parameter is `ln β`, with the
  acceptance bound `beta_bounds` in place of the fixed `[1, 20]`; `fit_moffat_2d` and `fit_moffat_2d_fixed_beta` are
  untouched (their callers in `metrics.rs`, the debug binary and the pins keep their bytes). The result carries
  `iters`. No rustafits crate version bump in Plan A (a path dependency; `Cargo.lock` unchanged); a release is the
  owner's `rustafits-release` procedure, later.
- **PA-8 The flat-top test's constants** are fixed a priori, not measured: core = the pixels within 2 σ_local of the
  observed maximum inside a radius of `max(1.5, 0.5·FWHM_start)` px; the star is flagged saturated when that core
  holds ≥ 3 pixels AND the free-β wing fit (on the pixels outside the core, ≥ 12 of them) predicts, at the maximum's
  position, a value above the observed maximum by more than `max(3 σ_local, 0.01·observed maximum)`. The 3 is the
  bar's own `k`; the 1 % floor is half the generator's "within 2 % of the clip" gray zone.
- **PA-9 Donut family.** Profile `donut<R>`: a uniform disc of radius R px convolved with a Gaussian of FWHM 2.6
  (a new `render.Component` kind `disc`, `a = R`, pixel-integrated through `render_star_blurred`). Truth kind
  `donut` (not a width kind; no `fit_*` widths, no CRLB). Its row is "donut outcome": the share of matched donut
  stars refused (by reason) against the share accepted — the bar is "refused with its reason, never reported as a
  narrow fit": accepted donut fits = 0.
- **PA-10 The budget selects before fitting.** The seeds are ranked by matched-filter response; the 4 × 4 cells
  take `ceil(1.25·N/16)` brightest candidates each (round-robin by rank when a cell holds fewer), the candidates
  are fitted, refused and saturated ones drop, and the result is trimmed to N by SNR round-robin over the cells.
  `BudgetReport { cells_with_stars, cells_covered, candidates, kept }` is reported; the bar reads
  `cells_covered / cells_with_stars ≥ 0.9` (a cell "holds measurable stars" when any seed above threshold lies in it).
- **PA-11 SNR geometry (F-6).** Aperture radius `max(1.5·FWHM, 3)` px about the fitted centre; sky annulus
  `[max(3·FWHM, 6), max(3·FWHM, 6) + max(FWHM, 3)]` px; σ_sky = 1.4826·MAD of the annulus; `N_sky` its pixel count.
  The truth column `snr_ap` uses the same radii from the star's `fit_fwhm` geometric mean.
- **PA-12 Region growth (F-1).** `r_start = max(3, ceil(1.5·F_start))`, `r_max = clamp(ceil(6·F_start), 6, 96)`
  (and never past the border); `m_r` the square's median, `s_r` the median of its outer 1-px rim; grow while
  `(m_r − s_r) > 0.01·(max − s_r)`; the background start `b0 = s_r` of the final square; `F_start` from the star's
  own background-subtracted second moments in a box of radius `2·F_field` (the matched filter's field width),
  clamped to [0.6, 40] px.
- **PA-13 The real-data "before" trees** are `tierA-lnB-f1` (set 204, run 5), `lnB-runB-f1` (run B) and `hc11B-f1`
  (Horse C11, set 182, pinned frame 69845) — PROVIDED a Measure-equivalence check passes: on three calibrated frames
  of each tree, local main's `measure_probe` output equals the tree's stored `metrics` payload field for field. If it
  does not, the before run is re-made with `tier1/rerun-build.sh <local main sha> starA-before` for that dataset.
- **PA-14 LN scale rows are not judged by no-regression in Plan A.** Row 6's `arm *`, `sLib`, their slopes, and the
  injected LN pairs read the fitted `fwhm()` (F1's common F) and `signal` (psfSel), both of which Plan A changes by
  design; spec F-8 re-baselines them in Plan B. `scorecard_diff.py` prints them in their own table, never counts
  them. The Plan B re-baseline starts from Task 14's scorecard.
- **PA-15 The legacy analysis arm is the control.** The rustafits analysis path (`--paths legacy-analysis`, JSONL
  `path: "analysis"` unchanged through Plan A) is re-run at every step on the same u16 siblings; its rows must be
  bit-identical to the previous step's — a non-identical control row means the inputs or the tool moved, and the
  step stops. The module's Analysis mode is the new arm `analysis-mode`; Plan C swaps the labels.
- **PA-16 Each fix step's lab cycle** is: build the probe → `run_probe.py manifests/v2.json --out
  ~/.athenaeum-acc/star-lab-a/<step>/probe` → `--verify` → `scorecard.py` → `scorecard_summary.py` (committed) →
  `scorecard_diff.py` against the previous step's gzipped JSON (committed `noreg.txt`) → the task's own bar rows.
  Injections (`manifests/inject.json`) run at Tasks 6, 9 and 14 (the raw-mode bars) and the corpus at Task 12 (cost).

## File structure

athenaeum (`crates/athenaeum-core/src/` unless noted):

| File | Responsibility |
| ---- | ---- |
| `star_measure/mod.rs` | `MEASURE_VERSION`, `MeasureOptions`, `Mode`, `SeedSpec`, `PlaneMeasurement`, `measure_plane`, `fit_stars_at`, re-exports |
| `star_measure/domain.rs` | `Domain`, `ADU_SCALE`, `NoiseModel`/`NoiseSource`, `background_residual`, `noise_mrs` (moved) |
| `star_measure/seeds.rs` | the matched filter (Task 6): `MatchedFilter`, `detect`, `DEFAULT_THRESHOLD`, `FieldWidth` |
| `star_measure/fit.rs` | `Seed`, `FitParams`, `StarFit`, `StarFlags`, `FitOutcome`, `PsfModel`, `fit_one`, `accept`, `aperture`, `dedupe`, `fit_all`, `fit_stars`, `fit_stars_with_beta` (moved), canonical axes (Task 5), region/background/width start (Task 6) |
| `star_measure/beta.rs` | the per-frame β: discrete `AUTO_BETAS` search (moved) → continuous free-β median (Task 9), `BetaSource` |
| `star_measure/photometry.rs` | saturation test (Task 8), model flux + aperture correction (Task 10), SNR and HFR (Task 11) |
| `star_measure/frame.rs` | `frame_shape`, `signal_totals`, `FrameAggregates` (moved + the unsaturated statistic, Task 8) |
| `star_measure/budget.rs` | the Analysis-mode selection (Task 12) |
| `star_measure/diagnostics.rs` | `RefusalReason`, `Outcome`, `FitDiagnostics`, `RefusalCounts` (Task 4) |
| `star_measure/robust.rs` | moved verbatim from `stacking/robust.rs` |
| `stacking/measure.rs` | the adapter: `PlaneReader` → `Domain::Calibrated`, options from the config, `ChannelMeasurement` (+ `stars_refused`, `stars_unsaturated`, `refused_by_reason`), `.athf` write path unchanged |
| `stacking/psf_signal.rs` | PSFSW/PSFSNR, `group_beta`, re-exports of every moved item |
| `stacking/fits_artifact.rs` | the flags byte written and read (Task 4) |
| `stacking/config.rs` | `MEASURE_VERSION` in the three stage hashes (Task 3); `seed_source`, `seed_threshold`, the legacy decode (Task 7) |
| `examples/star_probe.rs` | refusal lines, flags, snr/hfr, `legacy-analysis`, `analysis-mode`, `--gain-from-header` |
| `src/types/stacking.ts`, `src/components/stacking/panels/MeasurePanel.tsx`, `src/components/stacking/stageSummary.ts` | the config migration's UI (Task 7) |
| `docs/stacking/README.md` | one "One star-measurement path, Plan A" section: rulings R-OSP-1…, every acceptance run |
| `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-a.md` | the "after Plan A" note (Task 14) |

rustafits (`rustafits/src/analysis/fitting.rs`): `fit_moffat_2d_bounded`, `Moffat2DBoundedResult`, the ln-β Jacobian
column, tests (Task 9); the convergence test only under Task 5's trigger.

athenaeum-lab (`star-lab/`):

| File | Responsibility |
| ---- | ---- |
| `starlab/render.py` | `Component` kind `disc` (PA-9) |
| `starlab/families.py` | families `sharp/{m4,gauss}/f{12.0,16.0}`, `donut/r{6,10}`; cards `EGAIN`, `ATH_CSCL`, `ATH_CFNM`; truth columns `clipped_core_px`, `sat_neighbour_only`, `sat_gray`, `snr_ap` |
| `bin/augment_v2.py` | the in-place card and truth-column augmentation of the existing families (Task 1) |
| `starlab/probe_io.py`, `starlab/probe_plan.py` | `refused` lines, the `analysis-mode` and `legacy-analysis` arms |
| `starlab/scorecard.py`, `scorecard_build.py` | rows: fit outcomes, donut outcome, axis order, saturation flag vs truth, star SNR vs truth, budget coverage, analysis-mode cost; `_threshold` reads `seedSource` |
| `bin/scorecard_diff.py` | the mechanical no-regression check (Task 1) |
| `bin/gates.py` | `STARLAB_ACC_COPY` for `probe_parity`; the two new families in `determinism` |
| `tests/test_*.py` | one test file per changed module |
| `results/plan-a/<step>/` | committed summaries, `noreg.txt`, bar tables per step |

Run the lab tests from `star-lab/`: `python3 -m unittest discover -s tests -v`.

## Interfaces Plan C consumes (written here so Plan C needs no new module surface)

- `star_measure::measure_plane(plane: &[f32], w: usize, h: usize, opts: &MeasureOptions, pool:
  Option<&Arc<rayon::ThreadPool>>) -> PlaneMeasurement` with `opts.mode = Mode::Budget { budget }` (D-3, default
  500), `opts.domain = Domain::RawAdu { clip, gain_e_per_adu }` (clip from BITPIX/BZERO or DATAMAX, the caller's job;
  gain from EGAIN), `opts.seeds = SeedSpec::MatchedFilter { threshold }` (the Settings → Analysis field in σ_f units,
  default `seeds::DEFAULT_THRESHOLD`), `opts.saturation_fraction` (Settings → Analysis).
- Per star (`StarFit`): `x, y, background, amplitude, fwhm_x ≥ fwhm_y, theta` (radians, image frame, on `fwhm_x`'s
  axis), `beta`, `residual`, `signal` (the F-5 flux), `snr` (F-6; `f64::NAN` when not measured), `hfr` (F-6, on the
  F-5 flux), `peak5`, `flags: StarFlags` (`SATURATED`, `BETA_AT_CAP`), plus `fwhm()`, `eccentricity()`.
- Per plane (`PlaneMeasurement`): `stars`, `beta`, `beta_source`, `frame: FrameAggregates { fwhm_px, eccentricity,
  snr_median, hfr_median, stars_fitted, stars_unsaturated, tflux, tmean_flux, mean_flux_rejected }` (the
  residual-weighted mean over accepted UNSATURATED fits is the one frame statistic, §4.2), `noise: NoiseModel { sigma,
  source, m_star, n_star }` (background and noise for `frame_snr`, `snr_weight`, `psf_signal`), `seeds` (stars
  detected), `refused: RefusalCounts` (`stars_refused` and per-reason counts), `budget: Option<BudgetReport>`,
  `snr_gain: Option<f64>` (None = the shot term was omitted), `diagnostics`.
- `star_measure::MEASURE_VERSION` for `AnalysisConfig::config_hash` and the `frame_analysis.measure_version` column.
- The trail metric (§4.3) is Plan C's: `frame.rs` leaves a documented slot (`FrameAggregates` is `#[non_exhaustive]`
  inside the crate, so Plan C adds `trail: Option<TrailMetric>` without touching callers).
- Mosaic input (§4.1 OSC): `measure_plane` takes a pixel list through `SeedSpec::Given` and a `Domain::RawAdu` plane;
  the green-site fitting is Plan C's own `fit_stars_at` call with a CFA pixel mask — the fitter already takes a
  `&[PixelSample]`, so no Plan A change is needed; `fit_stars_at` accepts `FitStarsOptions.mask` from
  Task 6 on (a pixel whose mask bit is false is treated as absent, like a non-finite one).

---

### Task 1: Lab first — families, cards, truth columns, the no-regression tool, the new arms (spec §5 step 1)

No product change. Everything the later tasks measure against exists after this task; the bars are the spec's and
are already fixed.

**Files:**
- Modify: `star-lab/starlab/render.py` (`Component` kind `disc`, `cutout_radius`, `evaluate`),
  `star-lab/starlab/families.py` (new families, `_cards`, `_stats`, `_truth_rows`, `_build_twins`, `COLS`),
  `star-lab/starlab/probe_io.py` (`refused` lines, `analysis-mode`), `star-lab/starlab/probe_plan.py`
  (`legacy-analysis`, `analysis-mode` jobs, `STARLAB_SCRATCH`), `star-lab/starlab/scorecard.py` (new rows,
  `_threshold`), `star-lab/starlab/scorecard_build.py` (wire the rows), `star-lab/bin/gates.py`
  (`STARLAB_ACC_COPY`, `STARLAB_SCRATCH`), `star-lab/starlab/__init__.py` (`SCRATCH_ROOT`)
- Create: `star-lab/bin/augment_v2.py`, `star-lab/bin/scorecard_diff.py`, `star-lab/tests/test_scorecard_diff.py`,
  `star-lab/tests/test_augment.py`, `star-lab/tests/test_donut.py`
- Test: `star-lab/tests/test_families.py`, `test_render.py`, `test_scorecard.py`, `test_run_probe.py` (additions)

**Interfaces:**
- Consumes: `manifests/v2.json` (164 families), the baseline full scorecard
  `~/.athenaeum-acc/star-lab-v2/results/baseline-2026-10-07-fw/scorecard.json.gz` (sha256 in the committed summary).
- Produces:
  - families `sharp/m4/f12.0`, `sharp/m4/f16.0`, `sharp/gauss/f12.0`, `sharp/gauss/f16.0` (`kind="field"`,
    `n_stars=300`, `lattice=True`, `min_sep=6·F` = 72 / 96 px, `bins="entry"`) and `donut/r6`, `donut/r10`
    (`kind="field"`, `prof="donut6"`/`"donut10"`, `fwhm=2.6` is the blur, `n_stars=200`, `lattice=True`,
    `min_sep=120`, `kind` of every star row `donut`);
  - FITS cards on every v2 image: `EGAIN` (e⁻/ADU = `gain/65535`), `ATH_CSCL = 65535.0`, `ATH_CFNM = 1.0` on the
    float file; `EGAIN` only on the `.u16.fits` sibling; the truth CSV columns `clipped_core_px` (the star's own
    core pixels — within `max(1, 0.5·fit_fwhm_gm)` px of its noiseless peak — at or above `sat` in the NOISY image),
    `sat_neighbour_only` (1 when `saturated = 1` and the star's own stamp + background never reaches `sat`),
    `sat_gray` (1 when the own noiseless peak + background is within 2 % of `sat`), `snr_ap` (the noiseless stamp
    summed in the PA-11 aperture over `sqrt(S/g + N_ap·(sky/g + rn²))`);
  - `bin/augment_v2.py manifests/v2.json` — rewrites every existing image's header in place with the three cards
    appended after the existing ones (data bytes untouched, verified by a sha256 of the data part before and
    after), appends the four truth columns (re-rendering the noiseless stars from the family's seeds without the
    CRLB; refuses an image whose re-rendered `flux_img` or `peak` differ from the stored truth by more than 1e-9),
    updates `sha256` in the manifest, records `augmented` in each record's `extra`;
  - `probe_io.load` → `{"seeds", "stars", "frames", "refused": {path: [...]}}`; a `refused` line is
    `{"kind":"refused","path":…, "plane":p, "seed":i, "reason":"regionTooSmall", "regionRadius":r,
    "backgroundStart":b, "widthStart":w, "growthSteps":g, "iterations":n|null}`;
  - `probe_plan`: the stack invocation stays `--paths measure,betas,register`; the u16 invocation becomes `--paths
    legacy-analysis` (JSONL path `analysis`); a NEW invocation per u16 sibling `--paths analysis-mode --domain adu
    --clip 65535` → `<name>.amode.jsonl` (added to the plan from Task 12 on through `probe_plan.ANALYSIS_MODE =
    True`; off until then so the diff tool's row sets stay comparable);
  - scorecard rows (all `_row(...)` with numeric fields so `scorecard_diff.py` can judge them):
    - row 1 `fit outcomes` per family × path: `accepted + refused = seeds` (`ok` boolean, per-reason counts);
    - row 1 `donut outcome` (families with kind `donut`): `accepted %`, `refused %` by reason over matched donuts;
    - row 4 `axis order`: the share of accepted fits with `fwhmX >= fwhmY` (n, share);
    - row 10 `saturation flag vs truth`: FP = flagged ∧ truth-unsaturated over truth-unsaturated (share), FN = not
      flagged ∧ `clipped_core_px >= 3` (count), plus the reported counts `neighbour_only`, `gray`, `under_3_core`;
    - row 6 `star SNR vs truth`: `(snr / snr_ap − 1)` median and 1.4826·MAD over matched unsaturated stars (bias,
      scatter, n), `n_no_gain` when the frame line says `snrGain: null`;
    - row 1 `budget coverage` (path `analysis-mode`): `cellsCovered / cellsWithStars` (share);
    - row 9 `invocation` for `analysis-mode` as for the other one-path tags;
    - `_threshold(path, frames)`: reads the frame line's `options.measure.seedSource`; `matchedFilter` →
      (`fisher_snr`, `seedThreshold`), `peak` → (`peak_snr`, `detectionSigma`), `structure` → (`fisher_snr`, 5.0);
  - `bin/scorecard.py --rows 1,…` (score only the named row groups; Task 6's threshold table reads row 1 alone);
    `run_probe.py` honours `STAR_PROBE_ARGS` (extra `star_probe` flags appended to every `measure` invocation,
    recorded in each run record's `cmd` so `--verify` and `complete()` tell the runs apart); `bin/path_compare.py
    --probe-dir` (part 2 reads a given probe directory instead of the v2 root's);
  - `bin/scorecard_diff.py <base.json[.gz]> <head.json[.gz]> --out noreg.txt [--design design.json]`: pairs rows by
    `(row, metric, family, path, plane, bin, truth, egroup)`; per row kind the standard error of the BASE row:
    `bias/scatter/n` rows → `SE = scatter/√n`; `median/scatter/n` rows → `SE = 1.2533·scatter/√n`; `share` or
    `completeness`/`raw`/`coverage` over `n` → `SE = √(p(1−p)/n)` floored at `1/n`; `per_mpx` counts → `√count/area`;
    row 7 `value` rows → `SE = value × (row 3 pointfit all-bin scatter of the same family/path/plane)/√n`;
    `spearman` rows → `SE = (1−ρ²)/√(n−1)`; cost rows → reported only; LN rows (PA-14) → reported only; a row
    whose status is `n/a` on either side → reported, not judged. Verdict per judged row: `|head − base| ≤ SE_base`.
    `--design` lists patterns (`{"row":4,"metric":"angle","family":"shape/*","path":"measure"}`) that are judged by
    the task's own bar instead and printed in their own table. Exit 1 when any judged row outside the design list
    moves beyond its SE (either direction: an unexplained improvement is still a change), with the row's base,
    head, SE and direction printed. A control section counts the `analysis` (legacy) rows that are not
    bit-identical (PA-15; must be 0).

- [ ] **Step 1: Write the failing lab tests** (`tests/test_donut.py`, `tests/test_augment.py`,
  `tests/test_scorecard_diff.py`, additions to `test_families.py`, `test_scorecard.py`, `test_run_probe.py`)

```python
"""Donut stars (PA-9): a disc convolved with a Gaussian, pixel-integrated, with no width truth."""
import math, unittest
import numpy as np
from starlab import render, families


class TestDonut(unittest.TestCase):
    def test_disc_component_has_a_central_dip_after_blur(self):
        comps = [render.Component(1.0, "disc", 6.0, 6.0)]
        img = np.zeros((81, 81))
        s, pk, _ = render.render_star_blurred(img, 40.0, 40.0, 1.0, comps, 0.0, 2.6, 2.6, [2.6])
        self.assertAlmostEqual(s, 1.0, delta=2e-3)
        ring = img[40, 46]; centre = img[40, 40]
        self.assertGreater(ring, 1.3 * centre)

    def test_donut_family_rows_are_kind_donut_without_widths(self):
        spec = dict(families.FAMILIES["donut/r6"]); spec.update(size=256, n_stars=4, min_sep=60.0)
        import tempfile, csv
        recs = families.build("donut/r6", spec, tempfile.mkdtemp(), seed=3)
        rows = list(csv.DictReader(open(recs[0]["truth"])))
        self.assertTrue(all(r["kind"] == "donut" for r in rows))
        self.assertTrue(all(r["fit_fwhm_minor"] in ("", "nan") for r in rows))
```

```python
"""augment_v2.py: cards appended, data bytes untouched, truth columns appended and reproducible."""
import os, tempfile, unittest, csv, hashlib
import numpy as np
from starlab import families, fitsio
import importlib.util
HERE = os.path.dirname(os.path.abspath(__file__))
_s = importlib.util.spec_from_file_location("augment_v2", os.path.join(HERE, "..", "bin", "augment_v2.py"))
aug = importlib.util.module_from_spec(_s); _s.loader.exec_module(aug)


class TestAugment(unittest.TestCase):
    def test_cards_appended_and_data_identical(self):
        d = tempfile.mkdtemp()
        spec = dict(families.FAMILIES["sharp/m4/f2.6"]); spec.update(size=256, density=150)
        rec = families.build("sharp/m4/f2.6", spec, d, seed=11, only_first=True)[0]
        before = aug.data_sha256(rec["path"])
        aug.augment_record(rec, spec, seed=11)
        _, h = fitsio.read_fits(rec["path"])
        self.assertAlmostEqual(float(h["EGAIN"]), spec["gain"] / 65535.0, places=9)
        self.assertEqual(float(h["ATH_CSCL"]), 65535.0); self.assertEqual(float(h["ATH_CFNM"]), 1.0)
        self.assertEqual(aug.data_sha256(rec["path"]), before)
        rows = list(csv.DictReader(open(rec["truth"])))
        for c in ("clipped_core_px", "sat_neighbour_only", "sat_gray", "snr_ap"):
            self.assertIn(c, rows[0])
        self.assertTrue(all(float(r["snr_ap"]) > 0 for r in rows if r["saturated"] == "0"))

    def test_augment_refuses_a_truth_that_does_not_reproduce(self):
        d = tempfile.mkdtemp()
        spec = dict(families.FAMILIES["sharp/m4/f2.6"]); spec.update(size=256, density=150)
        rec = families.build("sharp/m4/f2.6", spec, d, seed=11, only_first=True)[0]
        with self.assertRaises(aug.TruthMismatch):
            aug.augment_record(rec, spec, seed=12)      # another seed: flux_img differs
```

```python
"""scorecard_diff.py: the per-row standard error and the verdict."""
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
        rep = sd.compare(base, head, design=[{"row": 4, "metric": "angle", "family": "shape/*", "path": "measure"}])
        self.assertEqual(rep["moved"], []); self.assertEqual(len(rep["design"]), 1); self.assertEqual(len(rep["ln"]), 1)

    def test_legacy_analysis_control_counts_non_identical_rows(self):
        base = [row(path="analysis", bias=0.01)]; head = [row(path="analysis", bias=0.0100001)]
        self.assertEqual(sd.compare(base, head, design=[])["control_mismatch"], 1)
```

Additions: `test_families.py::test_fwhm16_families_are_isolated_at_six_fwhm` (min_sep ≥ 96 on `sharp/m4/f16.0`,
every pair of truth positions farther than that), `test_families.py::test_cards_carry_egain_and_cscl`,
`test_scorecard.py::test_fit_outcomes_row_sums_to_seeds`, `test_scorecard.py::test_saturation_flag_row_counts_fp_fn`,
`test_scorecard.py::test_star_snr_row`, `test_scorecard.py::test_axis_order_row`,
`test_scorecard.py::test_threshold_reads_seed_source`, `test_run_probe.py::test_plan_has_legacy_analysis_and_amode_off`.

- [ ] **Step 2: Run them and see them fail**

Run: `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab && python3 -m unittest discover -s tests -v 2>&1 | tail -30`
Expected: `AttributeError`/`ModuleNotFoundError` on `disc`, `augment_v2`, `scorecard_diff`, `fit outcomes`.

- [ ] **Step 3: Implement** render `disc` (evaluate: `w/(π a²)` inside radius `a`, else 0; `cutout_radius`: `a + 3 blur`),
  the family entries, `_cards`, `_stats` (the three saturation counts need the NOISY image: compute them in `_write`
  after `noise.apply`, before the CSV is written; `_build_twins` recomputes them per twin), `snr_ap`, `augment_v2.py`,
  `probe_io`/`probe_plan`, the rows, `scorecard_diff.py`, `gates.py` env overrides (`STARLAB_ACC_COPY` default
  `~/.athenaeum-acc/tierA-lnB-f1`; `STARLAB_SCRATCH` default `DATA_ROOT/gates`).

- [ ] **Step 4: Generate and augment** (heavy; one at a time; `memory_pressure` ≥ 30 % free; disk ≥ 100 GiB)

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab
python3 bin/make_v2.py --families sharp/m4/f12.0,sharp/m4/f16.0,sharp/gauss/f12.0,sharp/gauss/f16.0,donut/r6,donut/r10
python3 bin/augment_v2.py manifests/v2.json            # every pre-existing record; prints per family: cards, columns, sha
python3 bin/make_v2.py --check --families sharp/m4/f2.6,twins/sharp/m4/f2.6,background/neb3,osc/f2.6/m4,sharp/m4/f16.0,donut/r6
du -sk ~/.athenaeum-acc/star-lab-v2 | tee results/plan-a/t01-lab/data-root-kib.txt
```

Expected: six new families with their SNR-bin reports (the FWHM 12/16 families print `>300` bins as `n/a (ceiling)`
where the lattice's 300 stars cannot fill them — reported, allowed by P-23); `--check` `ok` on every named family
(the augmented record regenerates to the same bytes and the same truth); the data root ≤ 10 485 760 KiB.

- [ ] **Step 5: Rebuild the baseline rows the new rows need, without re-running probes.** The new rows (`fit
  outcomes`, `axis order`, …) need the probe's new lines, which the baseline binary never wrote; the new truth
  columns need no probe. Build the baseline scorecard ONCE more from the EXISTING probe outputs
  (`~/.athenaeum-acc/star-lab-v2/probe`, unchanged, verified) so the diff base has the new truth-only rows (`saturation
  flag vs truth` reads only truth + the baseline's lack of a flag → every baseline star unflagged: FP 0, FN = all).

```
python3 bin/run_probe.py manifests/v2.json --verify                     # 673 planned, 673 proven (the f5321979 binaries)
mkdir -p ~/.athenaeum-acc/star-lab-a/t01-lab
python3 bin/scorecard.py manifests/v2.json --probe-dir ~/.athenaeum-acc/star-lab-v2/probe --erp-dir ~/.athenaeum-acc/star-lab-v2/erp \
  --out ~/.athenaeum-acc/star-lab-a/t01-lab/scorecard.md --json ~/.athenaeum-acc/star-lab-a/t01-lab/scorecard.json
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-v2/results/baseline-2026-10-07-fw/scorecard.json.gz \
  ~/.athenaeum-acc/star-lab-a/t01-lab/scorecard.json --out results/plan-a/t01-lab/noreg-vs-baseline.txt
gzip -k ~/.athenaeum-acc/star-lab-a/t01-lab/scorecard.json; mkdir -p ~/.athenaeum-acc/star-lab-a/results/t01-lab; mv ~/.athenaeum-acc/star-lab-a/t01-lab/scorecard.json.gz ~/.athenaeum-acc/star-lab-a/results/t01-lab/
python3 bin/scorecard_summary.py ~/.athenaeum-acc/star-lab-a/t01-lab/scorecard.json --out results/plan-a/t01-lab/scorecard-summary.md --full ~/.athenaeum-acc/star-lab-a/results/t01-lab/scorecard.json.gz --title "Plan A t01: baseline rows with the new truth columns"
```

Expected: the diff reports 0 moved rows among the rows both files hold (the probe outputs are the same; only new
rows and the ERP cells can differ by nothing), the six new families appear with `output missing` rows (no probe
output yet — Task 2's run fills them), the legacy-analysis control mismatch is 0.

- [ ] **Step 6: Commit and push** (athenaeum-lab)

```
lab: Plan A step 1 - FWHM 12/16 and donut families, EGAIN/ATH_CSCL cards, saturation and SNR truth columns, the fit-outcome and saturation rows, scorecard_diff
```

**Acceptance:** all lab tests pass; `make_v2.py --check` ok on the six named families; data root ≤ 10 GB;
`run_probe.py --verify` 673/673; `noreg-vs-baseline.txt` 0 moved, control 0; the family count in the manifest is 170.
No bar is read in this task.

---

### Task 2: Extraction into `star_measure/` with no behaviour change (spec §5 step 2)

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/{mod.rs,domain.rs,fit.rs,beta.rs,frame.rs,robust.rs}`
- Modify: `crates/athenaeum-core/src/lib.rs` (`#[cfg(feature = "render")] pub mod star_measure;`),
  `crates/athenaeum-core/src/stacking/mod.rs` (`pub use crate::star_measure::robust;` replaces `pub mod robust;`),
  `crates/athenaeum-core/src/stacking/psf_signal.rs` (keeps `PSF_FIT_VERSION` until Task 3, PSFSW/PSFSNR,
  `group_beta`, the constants; re-exports), `crates/athenaeum-core/src/stacking/measure.rs` (hand edits: the body of
  `measure_plane_detailed` calls the module), `crates/athenaeum-core/examples/star_probe.rs` (imports only)
- Delete: `crates/athenaeum-core/src/stacking/robust.rs` (moved)

**Interfaces:**
- Produces (`star_measure/mod.rs`):

```rust
pub use domain::{Domain, NoiseModel, NoiseSource, ADU_SCALE, background_residual, noise_mrs};
pub use fit::{fwtm_from_alpha, FitOutcome, FitParams, PsfModel, Seed, StarFit};
pub use beta::{AUTO_BETAS, AUTO_SAMPLE};
pub use frame::{frame_shape, signal_totals, SignalTotals, RCR_LIMIT};

/// What the module is handed besides the plane (grows in Tasks 4–12).
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureOptions {
    pub domain: Domain,
    pub psf_model: PsfModel,
    pub max_stars: usize,
    pub seeds: SeedSpec,
    pub fit: FitParams,
}

/// Where the seeds come from. Task 2: the adapter detects (Peak/Structure) and passes them in.
#[derive(Debug, Clone, PartialEq)]
pub enum SeedSpec { Given(Vec<Seed>) }

#[derive(Debug, Clone, PartialEq)]
pub struct PlaneMeasurement {
    pub stars: Vec<StarFit>,
    pub beta: f64,
    pub frame: FrameAggregates,
    pub noise: NoiseModel,
    pub seeds: usize,
}

pub fn measure_plane(plane: &[f32], w: usize, h: usize, opts: &MeasureOptions,
                     pool: Option<&Arc<rayon::ThreadPool>>) -> PlaneMeasurement;

/// The refit entry point LN uses (PA-6): every seed fitted at `beta`.
pub fn fit_stars_at(plane: &[f32], w: usize, h: usize, seeds: &[Seed], beta: f64, fit: &FitParams,
                    pool: Option<&Arc<rayon::ThreadPool>>) -> FitOutcome;
```

  `Domain::Calibrated { gain_e_per_unit: Option<f64> }` (plane in [0, 1]; the module multiplies by `ADU_SCALE`
  exactly where `measure_plane_detailed` did) and `Domain::RawAdu { clip: f64, gain_e_per_adu: Option<f64> }`
  (plane already in ADU; no scaling). `NoiseModel { sigma: f64 /* ADU */, source: NoiseSource, m_star: f64, n_star:
  f64 }`. `FrameAggregates { fwhm_px, eccentricity, stars_fitted, tflux, tmean_flux, mean_flux_rejected }`
  (ADU units; the adapter divides as it does today).
- `stacking::psf_signal` re-exports: `pub use crate::star_measure::{Seed, FitParams, StarFit, FitOutcome, PsfModel,
  fwtm_from_alpha, background_residual, noise_mrs, frame_shape, signal_totals, SignalTotals, AUTO_BETAS,
  AUTO_SAMPLE, RCR_LIMIT};` and `pub fn fit_stars(..)`/`fit_stars_with_beta(..)` as one-line wrappers over
  `star_measure::fit::{fit_stars, fit_stars_with_beta}` — every current `use crate::stacking::psf_signal::…` in
  `ln/`, `register/`, `fits_artifact.rs`, `structure.rs`, the examples and the tests compiles unchanged.
- `measure_plane` scales a `Calibrated` plane by `ADU_SCALE` itself; `fit_stars_at` takes the plane in the
  module's working units (ADU-scaled), exactly as `fit_stars_with_beta` does today (LN and the probe already hand
  it that).
- `stacking::measure::measure_plane_detailed` keeps its signature and its seed detection (Peak/Structure/Full on the
  ADU-scaled copy) and hands `SeedSpec::Given(seeds)` to `star_measure::measure_plane`, then builds
  `ChannelMeasurement` from `PlaneMeasurement` + the sample statistics exactly as before.

- [ ] **Step 1: Write the failing tests** (in `star_measure/mod.rs` `#[cfg(test)]`; the moved tests of
  `psf_signal.rs` travel with their code into `fit.rs`/`frame.rs`/`domain.rs` unchanged; `measure.rs`'s test
  helper `field` becomes `pub(crate)` under `#[cfg(test)]` by a hand edit so the module's tests reuse the fixture)

```rust
#[test]
fn module_measure_equals_the_adapter_bit_for_bit() {
    // The adapter's own fixture field (measure.rs tests): 150 Gaussian stars, σ 1.8 px, bg 0.08, noise 0.002.
    let (data, w, h) = crate::stacking::measure::tests::field(2026_09_19, 1.0, 0.002);
    let adapter = crate::stacking::measure::measure_plane_with_seeds(&data, w, h,
        &crate::stacking::measure::MeasureOptions::default(), None, crate::stacking::measure::SeedSource::Fast);
    let (pm, seeds) = crate::stacking::measure::measure_plane_detailed(&data, w, h,
        &crate::stacking::measure::MeasureOptions::default(), None, crate::stacking::measure::SeedSource::Fast);
    let opts = MeasureOptions { domain: Domain::Calibrated { gain_e_per_unit: None }, psf_model: PsfModel::Auto,
        max_stars: 24576, seeds: SeedSpec::Given(seeds), fit: FitParams::default() };
    let m = measure_plane(&data, w, h, &opts, None);
    assert_eq!(m.stars, pm.fits);
    assert_eq!(m.beta, adapter.channel.beta);
    assert_eq!(m.frame.fwhm_px, adapter.channel.fwhm_px);
    assert_eq!(m.frame.tflux / ADU_SCALE as f64, adapter.channel.tflux);
}

#[test]
fn fit_stars_at_equals_fit_stars_with_beta() {
    use crate::test_support::{moffat_field, MoffatStar};
    let stars: Vec<MoffatStar> = (0..20).map(|i| MoffatStar { x: 20.0 + (i % 5) as f64 * 40.3, y: 20.0 + (i / 5) as f64 * 40.7,
        amp: 0.2, alpha_x: 5.0, alpha_y: 5.0, theta: 0.0 }).collect();
    let data = moffat_field(220, 180, &stars, 4.0, 0.05);
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect();
    let a = fit_stars_at(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
    let b = crate::stacking::psf_signal::fit_stars_with_beta(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
    assert_eq!(a, b);
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum && CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib star_measure -- --test-threads=4`
Expected: `error[E0433]: ... star_measure` (module absent).

- [ ] **Step 3: Move the code verbatim.** `robust.rs` → `star_measure/robust.rs` (file move; `git mv`). From
  `psf_signal.rs`: `Seed`, `FitParams`, `StarFit` (+`not_measured`), `FitOutcome`, `PsfModel`, `fwtm_from_alpha`,
  `initial_sigma`, `stamp_radius`, `moment_ellipse`, `push_finite_row`, `region_median`, `sampling_radius`,
  `FitScratch`, `fit_one`, `accept`, `aperture`, `dedupe`, `fit_all`, `fit_stars`, `fit_stars_with_beta` → `fit.rs`
  (the `AUTO_*` search body of `fit_stars` → `beta::auto_beta(data, w, h, seeds, sigma0, p, pool) -> f64`, called by
  `fit_stars`); `kahan_sum`, `signal_totals`, `SignalTotals`, `frame_shape`, `RCR_LIMIT` → `frame.rs`;
  `background_residual`, `noise_mrs`, `N_STAR_FROM_MAD`, `BACKGROUND_MODEL_CELL_PX`, `MRS_LAYERS`,
  `MRS_LAYER1_GAIN` → `domain.rs`. No arithmetic changes, no reordering of operations; `rustfmt
  crates/athenaeum-core/src/star_measure/*.rs` by path. `psf_signal.rs` is left with `PsfModel`'s re-export, the
  PSFSW/PSFSNR constants and functions, `group_beta`, `PSF_FIT_VERSION`, and the moved tests that read the weight
  formulas. `measure.rs`: hand edit — the block from `let params = FitParams::default();` to the `ChannelMeasurement`
  literal now reads `let m = star_measure::measure_plane(data, w, h, &opts_for_module, pool)` where the module gets
  the ALREADY-DETECTED seeds and computes background/MRS/fits/totals on its own ADU-scaled copy; the sample
  statistics stay in the adapter. (The module scales once; the adapter's `scaled` copy is only needed for seed
  detection — keep it for that, so the detector's input bytes are unchanged.)

- [ ] **Step 4: Build and test**

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib star_measure stacking::measure stacking::psf_signal stacking::ln stacking::register -- --test-threads=4
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features --features render
```

Expected: every pre-existing pin passes untouched, in particular
`the_shipped_none_path_measures_exactly_these_numbers`, `measure_plane_with_seeds_pinned_before_task_3b_rewrite`,
`seed_source_fast_is_bit_identical_to_measure_plane`,
`measure_plane_detailed_returns_the_seeds_it_fitted_and_the_same_measurement`, `fit_stars_with_beta_matches_fit_stars_for_the_resolved_beta`;
the headless build compiles (the module is `render`-only); the `render`-only build compiles without `solver`
(the module never names `stacking`).

- [ ] **Step 5: The lab gate — bit-identical after f32 narrowing and equal summary cells**

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo build --release -p athenaeum-core --example star_probe --example ln_scale_probe --example measure_probe
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab
STARLAB_SCRATCH=~/.athenaeum-acc/star-lab-a/t02-extract/gates python3 bin/gates.py --only probe_build,probe_parity,seed_parity | tee results/plan-a/t02-extract/gates.txt
python3 bin/run_probe.py manifests/v2.json --out ~/.athenaeum-acc/star-lab-a/t02-extract/probe && python3 bin/run_probe.py manifests/v2.json --out ~/.athenaeum-acc/star-lab-a/t02-extract/probe --verify
python3 bin/scorecard.py manifests/v2.json --probe-dir ~/.athenaeum-acc/star-lab-a/t02-extract/probe --erp-dir ~/.athenaeum-acc/star-lab-v2/erp --out ~/.athenaeum-acc/star-lab-a/t02-extract/scorecard.md --json ~/.athenaeum-acc/star-lab-a/t02-extract/scorecard.json
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-a/results/t01-lab/scorecard.json.gz ~/.athenaeum-acc/star-lab-a/t02-extract/scorecard.json --out results/plan-a/t02-extract/noreg.txt
python3 bin/scorecard_summary.py ~/.athenaeum-acc/star-lab-a/t02-extract/scorecard.json --out results/plan-a/t02-extract/scorecard-summary.md --full <gz> --title "Plan A t02: extraction"
diff <(grep -v '^From \|built ' results/baseline-2026-10-07/scorecard-summary.md) <(grep -v '^From \|built \|f12.0\|f16.0\|donut\|fit outcomes\|axis order\|saturation flag\|star SNR' results/plan-a/t02-extract/scorecard-summary.md)
```

Expected: `probe_parity` PASS with `0 mismatches` against `tierA-lnB-f1`'s stored `.athf` (the OLD binary's artifact
read by the NEW module's numbers — the bit-identity proof after one f32 narrowing); `seed_parity` PASS; the
scorecard diff: every judged row identical (0 moved, and the printed max |Δ| over all paired numeric fields is 0.0);
the summary diff: no line of the baseline summary changes (the only additions are the new families' and rows'
lines). The six new families now have measure rows (their numbers are simply recorded here; no bar).

- [ ] **Step 6: Commit** (athenaeum, no push; lab results committed and pushed)

```
refactor(core): star_measure module - the measure path's fit, beta, frame and noise code moved verbatim, stacking as the adapter (bit-identical: probe_parity 0 mismatches, every scorecard cell equal)
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
  `"psfFitVersion"` → `"measureVersion"`), `crates/athenaeum-core/src/stacking/plan.rs` (hand-edit the three
  comment lines 581, 3187, 6410 that name `PSF_FIT_VERSION`), `crates/athenaeum-core/src/stacking/measure.rs`
  (hand-edit the doc comment at line 1071), `docs/stacking/README.md` (the Tier C paragraph's `PSF_FIT_VERSION = 3`
  gets a pointer to the new section; the new section records the policy)

**Interfaces:**
- Produces: `star_measure::MEASURE_VERSION: u32 = 4` with the doc comment: 1–3 = `PSF_FIT_VERSION`'s history (copied
  verbatim), 4 = Plan A (the module, F-0…F-7), the bump policy (one bump per merged plan that changes what a fit
  accepts or measures; Plan C = 5). Folded into `measurement_subtree`, `registration_subtree`,
  `normalization_subtree` as `"measureVersion"`. `AnalysisConfig::config_hash` is Plan C's.

- [ ] **Step 1: Write the failing tests** (`config.rs` tests; hand-edited)

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
    assert_eq!(MEASURE_VERSION, 4, "Plan A's one bump (ruling PA-4)");
}
```

Update `stage_hashes_are_pinned_to_golden_literals` (its three fit-reading literals move; the calibration and
prepare literals must NOT move — assert that explicitly) and the Measure-only staleness test named in the spec
(`measure.rs:1071`'s doc comment and `plan.rs`'s `fits_artifact_fresh` tests, which key on `PSF_FIT_VERSION 3`:
they read the constant through `MEASURE_VERSION` now).

- [ ] **Step 2: Run, see them fail, implement, run again**

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::config stacking::plan stacking::measure -- --test-threads=4
grep -rn "PSF_FIT_VERSION\|psfFitVersion" crates/ rustafits/src docs/stacking/README.md | grep -v "history\|was \|formerly"
```

Expected: the grep finds only the history lines in the constant's doc comment and README's Tier C paragraph.

- [ ] **Step 3: Record the policy** in `docs/stacking/README.md` (new section "One star-measurement path, Plan A",
  ruling R-OSP-1: the constant, its value, the policy, and the risk line "the first run after Plan A re-measures,
  re-registers (mono) and re-normalizes every set").

- [ ] **Step 4: Commit**

```
feat(stacking): MEASURE_VERSION 4 replaces PSF_FIT_VERSION in the measure, register and normalize stage hashes (ruling R-OSP-1; one bump per merged plan)
```

**Acceptance:** the tests above green; no `PSF_FIT_VERSION` outside history lines; no lab run (the numbers do not
move; the hashes do, by design).

---

### Task 4: F-0 — `FitDiagnostics`, refusal reasons, `stars_refused`, the flags byte (spec §3 F-0, §5 step 3)

Numbers unchanged; every seed gets an outcome.

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/diagnostics.rs`
- Modify: `star_measure/fit.rs` (`fit_one` returns `Result<StarFit, RefusalReason>` + a diagnostics record;
  `dedupe` records `Duplicate`; `fit_all`/`fit_stars`/`fit_stars_with_beta` collect outcomes), `star_measure/mod.rs`
  (`PlaneMeasurement.refused`, `.diagnostics`; `MeasureOptions.diagnostics: bool`), `stacking/measure.rs` (hand
  edit: `ChannelMeasurement { stars_refused, refused_by_reason }`), `stacking/fits_artifact.rs` (flags byte),
  `examples/star_probe.rs` (`refused` lines; `--diagnostics`), `docs/stacking/README.md`

**Interfaces:**
- Produces:

```rust
// diagnostics.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefusalReason {
    /// The width-start box (Task 6) / the nominal stamp (today) leaves the image.
    OutsideImage,
    /// Fewer than 10 finite pixels in the region.
    TooFewPixels,
    /// The core is darker than the ring (Task 6's donut test).
    Donut,
    /// The fitter returned `None` (α < 0.3, A ≤ 0, β out of bounds).
    FitFailed,
    NonFinite,
    ResidualTooLarge,
    CentreMoved,
    CentreAtRim,
    RegionTooSmall,
    /// `min(fwhm_x, fwhm_y) < 0.8` px (Task 6; a hot pixel that got a seed).
    NarrowerThanPixel,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome { Accepted { star: usize }, Refused(RefusalReason) }

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
pub struct RefusalCounts { pub by_reason: BTreeMap<RefusalReason, usize> }
impl RefusalCounts { pub fn total(&self) -> usize; pub fn add(&mut self, r: RefusalReason); }
```

  `StarFlags(u8)` in `fit.rs`: `SATURATED = 1`, `BETA_AT_CAP = 2` (set from Task 8 / Task 9; written and read as the
  `.athf` flags byte now; a reader of an older file reads 0). `StarFit.flags: StarFlags` with `#[serde(default)]`.
  `FitOutcome.refused: RefusalCounts`. `ChannelMeasurement.stars_refused: usize` and `refused_by_reason:
  BTreeMap<String, usize>` (serde default; the `metrics` payload grows two keys, every older payload still decodes).
  `MeasureOptions.diagnostics: bool` (the probe sets it; the run never does — the per-seed vector costs memory on
  24576 seeds).
- The invariant `stars_fitted + stars_refused == stars_detected` is an assertion inside `measure_plane`
  (`debug_assert!` + a test), and a scorecard row.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn every_seed_has_exactly_one_outcome() {
    let (data, w, h) = crate::stacking::measure::tests::field(7, 1.0, 0.002);
    let (pm, seeds) = crate::stacking::measure::measure_plane_detailed(&data, w, h, &Default::default(), None, SeedSource::Fast);
    let m = measure_plane(&data, w, h, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(seeds.clone()), ..test_opts() }, None);
    assert_eq!(m.diagnostics.len(), seeds.len());
    assert_eq!(m.stars.len() + m.refused.total(), seeds.len());
    assert_eq!(m.stars, pm.fits, "the numbers do not move");
}

#[test]
fn edge_seed_is_refused_outside_image_with_its_reason() {
    let data = vec![0.05f32; 64 * 64];
    let m = measure_plane(&data, 64, 64, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(vec![Seed { x: 2.0, y: 30.0, peak: 0.5, flux: 5.0 }]), ..test_opts() }, None);
    assert_eq!(m.diagnostics[0].outcome, Outcome::Refused(RefusalReason::OutsideImage));
    assert_eq!(m.refused.by_reason[&RefusalReason::OutsideImage], 1);
}

#[test]
fn starless_plane_has_no_outcomes_and_no_panic() {
    let data = vec![0.05f32; 128 * 128];
    let m = measure_plane(&data, 128, 128, &MeasureOptions { diagnostics: true, seeds: SeedSpec::Given(vec![]), ..test_opts() }, None);
    assert!(m.stars.is_empty() && m.diagnostics.is_empty() && m.refused.total() == 0);
}

#[test]
fn duplicate_and_rim_refusals_are_counted_by_reason() {
    // two seeds 0.5 px apart on one star → one Accepted, one Duplicate; a seed whose region stays narrow → RegionTooSmall
    // (reuse the fixtures of `fits_are_rejected_when_they_wander_duplicate_or_touch_the_border` and
    // `a_fit_settled_at_the_region_rim_is_rejected`, now asserting the REASON, not only the absence)
}

// fits_artifact.rs
#[test]
fn flags_byte_round_trips_and_an_old_file_reads_zero() { /* write a fit with SATURATED|BETA_AT_CAP, read back equal; a hand-built v1 record with byte 0 reads StarFlags(0) */ }
```

- [ ] **Step 2: Run them and see them fail; implement; run again**

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib star_measure stacking::fits_artifact stacking::measure -- --test-threads=4
```

Expected: the pre-existing pins still exact (`the_shipped_none_path_measures_exactly_these_numbers`, …).

- [ ] **Step 3: The probe** — `fit_line` gains `"flags": ["saturated", …]`; `--diagnostics` (on by default in the
  lab's plan) writes one `refused` line per refused seed and `"refused": {"total": n, "byReason": {...}}` on the frame
  line; `seeds`/`fits`/`refused.total` must agree. Probe test `refused_lines_plus_fits_equal_seeds`.

- [ ] **Step 4: The lab cycle (PA-16)** for `t04-f0`: build, run, verify, scorecard, diff against t02, summary.
  Bar rows: row 1 `fit outcomes` — `ok` on every image (accepted + refused = seeds). Diff: 0 moved rows; the legacy
  control 0. Delete `~/.athenaeum-acc/star-lab-a/t02-extract/probe` after the t04 summary is committed (PA-3).

- [ ] **Step 5: Commit**

```
feat(star_measure): F-0 FitDiagnostics - every seed has an outcome and a refusal reason; stars_refused in ChannelMeasurement; the .athf flags byte (numbers unchanged, fit outcomes = seeds on every image)
```
```
results: Plan A t04 F-0 - fit-outcome rows and the zero-move diff
```

**Acceptance:** `fit outcomes` ok on 100 % of images; `noreg.txt` 0 moved; the F-0 bar "accepted +
refused-by-reason = seeds on every image; numbers unchanged against the extraction step" — both read from
`results/plan-a/t04-f0/`. Baseline for the record: `sharp/m4/f8.0` measure `82.7 % of 301 expected` against seeds
`92.0 %`; the row now names the reasons behind the gap (expected mostly `RegionTooSmall`; recorded, not judged
here — F-1 judges it).

---

### Task 5: F-2 — canonical axes and a conditioned fit (spec §3 F-2)

**Files:**
- Modify: `star_measure/fit.rs` (`accept`: axis order; `fit_one`: stamp-centred, amplitude-normalised pixel list and
  the back-transform), `examples/star_probe.rs` (nothing), `docs/stacking/README.md` (R-OSP-3)
- Conditional (step 7): `rustafits/src/analysis/fitting.rs` (the convergence test) on branch `one-star-path-beta`

**Interfaces:**
- Produces: every accepted `StarFit` has `fwhm_x >= fwhm_y` and `theta` is the axis of `fwhm_x`, in `[-π/2, π/2)`;
  `fwtm_x/fwtm_y` follow the swap; `aperture` reads the swapped pair (its ellipse is the same geometric object).
  Inside `fit_one` the fitter sees `PixelSample { x: px − cx, y: py − cy, value: (v − b0)/a0 }`, starts at `B = 0,
  A = 1, x0 = seed.x − cx, y0 = seed.y − cy`, and the result is mapped back (`x0 + cx`, `B·a0 + b0`, `A·a0`;
  `fit_residual` is already amplitude-normalised so it is unchanged in meaning). `conv_tol` and the cost test are
  unchanged in value.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn accepted_fits_have_canonical_axes() {
    // 40 Moffat stars, αx 3 / αy 6 at θ = 20° (minor axis along u): every fit must report fwhm_x ≥ fwhm_y and θ ≈ 110° (mod 180)
    use crate::test_support::{moffat_field, MoffatStar};
    let stars: Vec<MoffatStar> = (0..40).map(|i| MoffatStar { x: 30.0 + (i % 8) as f64 * 40.0, y: 30.0 + (i / 8) as f64 * 40.0,
        amp: 0.3, alpha_x: 3.0, alpha_y: 6.0, theta: 20f64.to_radians() }).collect();
    let data = moffat_field(360, 240, &stars, 4.0, 0.05);
    let seeds: Vec<Seed> = stars.iter().map(|s| Seed { x: s.x, y: s.y, peak: s.amp, flux: 1.0 }).collect();
    let out = fit_stars_at(&data, 360, 240, &seeds, 4.0, &FitParams::default(), None);
    assert!(out.fits.len() >= 36);
    for f in &out.fits {
        assert!(f.fwhm_x >= f.fwhm_y, "{f:?}");
        let want = (20f64 + 90.0).to_radians();
        let d = ((f.theta - want + std::f64::consts::FRAC_PI_2).rem_euclid(std::f64::consts::PI) - std::f64::consts::FRAC_PI_2).abs();
        assert!(d < 2f64.to_radians(), "theta {} vs {want}", f.theta);
        assert!((f.fwtm_x / f.fwhm_x - f.fwtm_y / f.fwhm_y).abs() < 1e-9);
    }
}

#[test]
fn fit_is_translation_and_scale_invariant() {
    // the same stamp at (30, 20) and at (3000, 2000), and at amplitude ×1000: x0/y0 differ by the offset,
    // widths/θ/residual equal to 1e-9 relative, amplitude/background scale by 1000
}

#[test]
fn eccentricity_and_fwhm_are_unchanged_by_the_swap() { /* fwhm() and eccentricity() of the swapped record equal the unswapped pair's */ }
```

- [ ] **Step 2: Run, fail, implement, run.** Then every pinned measurement literal in `measure.rs` and
  `psf_signal.rs` tests moves BY DESIGN (the conditioning changes last bits and the axis swap changes θ): update
  each literal with a one-line comment "F-2 (Task 5): conditioning moved the last bits; the previous literal was …".
  `fits_artifact::decode` of an OLD `.athf` keeps the old axis order (it is stale by `MEASURE_VERSION` anyway).

- [ ] **Step 3: Lab cycle** `t05-f2`, with `--design`:
  `{"row":4,"metric":"angle"}`, `{"row":4,"metric":"axis order"}`, `{"row":4,"metric":"ecc","family":"shape/*"}`,
  `{"row":3,"metric":"fwhm axis"}`, `{"row":2,"metric":"centroid"}` (the conditioning may move centroids within
  their own CRLB), `{"row":7}` (frame values follow the fits by last bits), `{"row":11}`, and every `measure`/`betas`
  row of rows 3, 5, 6 `captured fraction` within SE (judged, not design).

- [ ] **Step 4: Read the bar.** Row 4 `axis order` = 100 % on every family × path `measure`; row 4 `angle` on the
  `shape/*` families at e ≥ 0.3, the `eff` cell (prepixel CRLB): ≤ 1.5 on each of the 24 families with e ∈ {0.3,
  0.5, 0.7}. Baseline for the record: `shape/f2.6/e0.3/t30` rms 44.564°, `e0.5/t30` 32.339° (ERP 7.849° / 2.129°).

- [ ] **Step 5: `path_compare.py` part 2** re-run on the t05 outputs (`--data-root` pointing at the v2 root, the probe
  dir argument added in Task 1) → `results/plan-a/t05-f2/path-compare.txt`: the "fwhm_x < fwhm_y" counts must be 0
  of N on every `e0.3` family (baseline 71–145 of 346–427).

- [ ] **Step 6: No-regression**: `noreg.txt` 0 moved outside the design list; control 0.

- [ ] **Step 7 (conditional): the rustafits convergence test.** Trigger: ANY `shape` family with e ≥ 0.3 reads angle
  `eff > 1.5` after step 4. Then, and only then: create the rustafits branch `one-star-path-beta` (from `8321412`),
  change `lm_solve_moffat_with`'s `param_converged` to a per-parameter test on the conditioned parameters (each
  `|Δp_i| ≤ conv_tol·max(|p_i|, 1)` — the normalisation makes every parameter O(1)), keep `residual_converged`,
  add the rustafits test `conditioned_convergence_does_not_stop_on_a_theta_step` (a stamp whose only remaining
  error is 5° in θ at e 0.3 converges to < 1°), re-run the C3 pins (they must be told: the control flow changes, so
  the `iters` assertions in `c3_moffat_arithmetic_delta_against_the_pre_c3_solver` are updated with the reason),
  bump the athenaeum submodule pointer, and re-run steps 3–6. If the trigger does not fire, the rustafits branch is
  created in Task 9 instead. Either way the number that decided is recorded (R-OSP-3).

- [ ] **Step 8: Commit**

```
fix(star_measure): F-2 canonical axes (fwhm_x >= fwhm_y, theta on the major axis) and a stamp-centred, amplitude-normalised fit (angle eff <= 1.5 on shape e >= 0.3: <numbers>)
```

**Acceptance:** `axis order` 100 %; angle `eff` ≤ 1.5 on every `shape` family at e ≥ 0.3 (24 rows, each printed in
`results/plan-a/t05-f2/bars.txt`); part 2 counts 0; `noreg.txt` 0 moved outside the design list. A miss → the owner,
with the 24 cells, before Task 6.

---

### Task 6: F-1 with F-7 — the matched-filter seeds, the region, the rim background and the per-star width start (spec §3 F-1, F-7)

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/seeds.rs`
- Modify: `star_measure/fit.rs` (`fit_one`: width start, region rule PA-12, rim `b0`, the donut and
  narrower-than-pixel refusals, the pixel mask), `star_measure/mod.rs` (`SeedSpec::MatchedFilter { threshold }`,
  `FitStarsOptions { fit: FitParams, mask: Option<Vec<bool>> }` — `fit_stars_at(plane, w, h, seeds, beta,
  opts: &FitStarsOptions, pool)` replaces Task 2's `fit: &FitParams` argument, and `measure_plane` takes the mask
  through `MeasureOptions.mask: Option<Vec<bool>>`; `PlaneMeasurement.field_width`), `stacking/measure.rs` (hand edit: `SeedSource::Fast` keeps
  Peak for now — the config switch is Task 7; a probe override `SeedOverride::MatchedFilter` so the lab measures
  the new seeds before the config lands), `examples/star_probe.rs` (`--seeds matched|peak|structure`, default
  `matched` from this task on), `docs/stacking/README.md` (R-OSP-2 the threshold choice, R-OSP-4 the region rule)

**Interfaces:**
- Produces (`seeds.rs`):

```rust
/// A Gaussian matched filter at the field width, in a separable convolution over the finite pixels
/// (a non-finite or masked pixel contributes nothing and normalises nothing — the border is not a star).
pub struct MatchedFilter { pub fwhm_px: f64 }
pub struct FieldWidth { pub fwhm_px: f64, pub from_stars: usize }
pub const DEFAULT_THRESHOLD: f64 = <k chosen by PA-2>;
/// Two passes: a 3-px kernel finds the 100 brightest candidates, their moment widths give the field width
/// (median, clamped to [1.5, 24] px); the second pass runs at that width. A field width ≥ 6 px runs the second
/// pass on the 2×2 super-pixel plane (kernel halved; candidate positions ×2, refined on the full plane).
/// Candidates: local maxima of the filtered plane above `threshold·σ_f` (σ_f = 1.4826·MAD of the filtered plane
/// over the stratified sample) with ≥ 2 of 4 cross-neighbours above `background + threshold·σ_plane` on the
/// UNFILTERED plane (the hot-pixel rule). Position: the flux-weighted centroid in a (2·ceil(F)+1)² box on the
/// background-subtracted plane; `peak` = that box's maximum − background; `flux` = the filtered response (the
/// matched filter's integrated signal — the selection key). Sorted by `flux` descending, truncated to `max_stars`.
pub fn detect(plane: &[f32], w: usize, h: usize, noise: &NoiseModel, threshold: f64, max_stars: usize,
              mask: Option<&[bool]>, pool: Option<&Arc<rayon::ThreadPool>>) -> (Vec<Seed>, FieldWidth);
```

  `fit_one` (PA-12): `F_start` per star from background-subtracted second moments in a box of radius
  `2·F_field` (clamped [0.6, 40] px; the box's own rim median is the subtraction level), the region grows from
  `r_start = max(3, ceil(1.5·F_start))` to `r_max = clamp(ceil(6·F_start), 6, 96)` while `(m_r − s_r) >
  0.01·(max − s_r)`; `b0 = s_r` (the final rim median); `a0 = max − b0`; the start σ from `F_start` with the
  moment axis ratio as today. Refusals: `Donut` when the 3×3 core mean about the moment centroid is below
  `0.8 ×` the mean of the 1-px ring at radius `F_start/2.3548` (both background-subtracted) — a Gaussian reads a
  ratio ≈ 1.4; `NarrowerThanPixel` when the accepted `min(fwhm_x, fwhm_y) < 0.8` px. Admission (`OutsideImage`)
  tests the width-start box, no longer a field-level nominal stamp (`initial_sigma`/`stamp_radius` are retired).
- The adapter: `SeedOverride::MatchedFilter` (probe) runs `seeds::detect` on the ADU-scaled plane with the module's
  `NoiseModel`; `Peak`/`Structure` unchanged.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn matched_filter_finds_every_star_of_the_fixture_and_no_noise_peak() {
    let (data, w, h) = crate::stacking::measure::tests::field(7, 1.0, 0.002);   // 150 stars, σ 1.8 px
    let noise = domain::noise_model(&scaled(&data), w, h);
    let (seeds, fw) = seeds::detect(&scaled(&data), w, h, &noise, DEFAULT_THRESHOLD, 24576, None, None);
    assert_eq!(seeds.len(), 150);
    assert!((fw.fwhm_px - 1.8 * 2.3548).abs() < 0.3, "{fw:?}");
    assert!(seeds.windows(2).all(|p| p[0].flux >= p[1].flux));
}

#[test]
fn nan_border_seeds_nothing_on_the_border_and_fits_every_interior_star() {
    // the star_probe fixture: 6-px NaN border, 40 Gaussian stars at FWHM 3.5; no seed within 8 px of the border, 40 fits
}

#[test]
fn refused_noise_estimate_seeds_nothing_and_warns() {
    // a constant plane: NoiseModel.source == NoiseSource::None → detect returns no seeds; measure_plane reports seeds 0, no panic
}

#[test]
fn hot_pixel_is_not_seeded_and_if_given_is_refused_narrower_than_pixel() {
    // a single pixel at 0.9 on sky 0.05 with 0.002 noise: detect → 0 seeds; SeedSpec::Given at its position → Refused(NarrowerThanPixel)
}

#[test]
fn broad_stars_fit_inside_a_region_that_scales_with_their_own_width() {
    // 20 Moffat-4 stars at FWHM 14 px (α ≈ 16) on 1024²: every fit accepted, region_radius ≥ 0.5·growth·FWTM, no RegionTooSmall
    // (the D1 cause-1 regime: half-FWTM 14.2 px at β 4)
}

#[test]
fn donut_is_refused_with_its_reason() {
    // a 6-px disc convolved with a 2.6-px Gaussian, rendered directly in the test: Refused(Donut), never an accepted fit
}

#[test]
fn rim_background_start_is_the_sky_under_nebulosity() {
    // a star on a quadratic nebula ramp: |b0 − local sky| < 0.3 σ (the F-7 bar's own condition, unit-tested on one star)
}

#[test]
fn pixel_mask_excludes_masked_pixels_from_detection_and_fit() {
    // a checkerboard mask: the fit uses only unmasked pixels (its pixel count equals the unmasked count in the region)
}
```

- [ ] **Step 2: Run, fail, implement, run.** Then the pins: `measure.rs`'s pinned literals hold for the PEAK seeds
  (the adapter default is still Peak until Task 7) but `fit_one`'s region/background/width start moved the fits by
  design → the literals move again with their comment. `psf_signal.rs`'s
  `fixed_beta_fit_recovers_centre_width_background_and_aperture_signal` must still pass at its tolerances.

- [ ] **Step 3: Choose the threshold (PA-2) BEFORE any F-1 bar is read.** Build the probe with `--seeds matched
  --seed-threshold K`; run only the 46 `twins/*` t1 images and the 7 `background/*` images at K ∈ {3.0, 3.5, 4.0,
  4.5, 5.0, 6.0} (`run_probe.py --families … --out ~/.athenaeum-acc/star-lab-a/t06-k/K<k>/probe` with
  `STAR_PROBE_ARGS="--seeds matched --seed-threshold <k>"`, a pass-through the plan gains in Task 1), score row 1
  only (`scorecard.py --rows 1`, a flag added in Task 1), and tabulate `false detections / Mpx` cause `nothing` per
  image. `DEFAULT_THRESHOLD` = the smallest K with every image ≤ 0.5. Commit the table as
  `results/plan-a/t06-f1/threshold-choice.txt` and set the constant. Only then run step 4.

- [ ] **Step 4: Lab cycle** `t06-f1` (v2 AND `manifests/inject.json` → `~/.athenaeum-acc/star-lab-a/t06-f1/inject-probe`;
  the probe's default seeds are now `matched`). `--design`: `{"row":1}` (the seed population changes: completeness
  against the new threshold, false detections, bright coverage, doubles), `{"row":2,"path":"seeds"}`,
  `{"row":3}`, `{"row":5}`, `{"row":7}`, `{"row":10}`, `{"row":11}`, `{"row":4,"metric":"floor"}`,
  `{"row":6,"metric":"captured fraction"}` (the region and background move the signal) — i.e. F-1 touches most
  measure rows by design; the judged remainder is row 4 `ecc`/`angle`/`axis order` on `shape/*` (F-2's gains must
  hold), row 2 `measure` centroids, and every `register`/`analysis` row.

- [ ] **Step 5: Read the bars** (`results/plan-a/t06-f1/bars.txt`, cells copied verbatim):
  - fitted / seeded at FWHM 8: `sharp/m4/f8.0` frame line `fits / seeds` ≥ 99 % (baseline 82.7 % fitted of 92.0 %
    seeded, i.e. 0.899 of the seeds);
  - frame FWHM error vs `pointfit` ≤ 1 % on `sharp/m4/f{2.6,3.5,5.0,8.0,12.0,16.0}` (row 7 `frame FWHM` ÷ the
    `pointfit: med …` truth − 1); the `gauss` f12/f16 rows are RECORDED here and judged in Task 9 (β is still
    discrete: a Gaussian at β 10 reads −3.04 % by construction, baseline `sharp/gauss/f8.0` −2.58 %);
  - donut: row 1 `donut outcome` accepted = 0 % on `donut/r6` and `donut/r10`, refusals named;
  - F-7: row 5 `background` `background/neb3_4096` |bias| ≤ 0.3 σ (baseline +0.998 σ, scatter 1.055) and
    `background/{flat,grad10,vig20}` within ±0.1 σ (baseline −0.076 / −0.081 / −0.084 σ);
  - raw mode (injections): row 1 completeness of the injected stars above the threshold ≥ 99 % on the 7 raw
    entries and 3 mosaics (`inject/raw/*`, `inject/mosaic/*`; baseline `raw/corpus` 96.9–97.4 %, the Pro raw
    96.3–98.4 %); the injected FWHM bias recorded (judged at Task 9 with F-4);
  - `_DSC5767.fit` is the corpus's; its seed count is read at Task 12 (the corpus run).
  - Cost: the probe's `measure` wall on `background/neb3_4096` (baseline 0.39 s) and the m42 corpus measure batch are
    read at Task 12; here the per-family row 9 cells are recorded.

- [ ] **Step 6: Record** R-OSP-2 (the threshold table and value), R-OSP-4 (PA-12's rule and the donut/hot-pixel
  refusals) in `docs/stacking/README.md`. Commit.

```
fix(star_measure): F-1/F-7 matched-filter seeds at the field width (threshold <k>, ruling R-OSP-2), per-star width start, a region that scales with the star, the rim background start, donut and hot-pixel refusals (<bar cells>)
```

**Acceptance:** every bar in step 5 met, cells verbatim in `bars.txt`; `noreg.txt` 0 moved outside the design
list; control 0. A miss → the owner with the cells; Task 7 waits. (Every `<cells>` in a commit message of this
plan is filled with the measured numbers from that task's `bars.txt`; a commit never carries the angle brackets.)

---

### Task 7: The stacking Measure config — `seed_source`, the legacy decode, presets, the panel, R-M4c-11 (spec §2.2)

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (`MeasurementConfig` gains `seed_source: SeedSource`,
  `seed_threshold: f64`; a shadow-struct `Deserialize`; `clamp_seed_threshold`), `crates/athenaeum-core/src/stacking/measure.rs`
  (hand edits: `MeasureOptions.seed_source`, `SeedSource` → `SeedOverride` for the probe override, the adapter's
  dispatch; `DEFAULT_SEED_DETECTOR` retired), `crates/athenaeum-core/src/ts_export.rs` (`SeedSource`),
  `src/types/stacking.ts` (regenerated), `src/components/stacking/panels/MeasurePanel.tsx`,
  `src/components/stacking/stageSummary.ts` (`seedSourceLabel`), `src/settings/registry.ts` (no change: the
  stacking tab lists sections, not fields), `crates/athenaeum-core/src/api/stacking.rs` (nothing: presets and
  defaults flow through `StackingConfig`), `docs/stacking/README.md` (R-M4c-11 updated), `examples/star_probe.rs`
  (`--seeds` reads the config's `seedSource` when `--config` is given)

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

- [ ] **Step 1: Write the failing tests**

```rust
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
fn seed_source_moves_the_measurement_stage_hash_and_the_default_is_the_matched_filter() { … }
#[test]
fn seed_threshold_is_clamped_with_a_warn() { … }
```

  Frontend: `src/components/stacking/panels/MeasurePanel.test.tsx` (vitest): the select renders the three
  choices, the peak fields are disabled unless `peak`, the threshold field is disabled unless `matchedFilter`.

- [ ] **Step 2: Run, fail, implement, run.** Update `config_hash_is_canonical_and_pinned` (new fields in the
  canonical JSON → the literal moves; say why) and `stage_hashes_are_pinned_to_golden_literals`. Regenerate TS:
  `CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract`;
  `npx tsc --noEmit`; `npx vitest run src/components/stacking`.

- [ ] **Step 3: The adapter default flips to the matched filter** — the measure.rs pins
  (`the_shipped_none_path_measures_exactly_these_numbers`, `measure_plane_with_seeds_pinned_before_task_3b_rewrite`,
  `detection_sigma_steers_the_seed_population`, `the_median_prefilter_keeps_a_well_sampled_population`,
  `the_structure_detector_seeds_a_measurement`) are re-pinned on the matched-filter default with their old
  literals kept in a comment, and the Peak/Structure tests set `seed_source` explicitly.

- [ ] **Step 4: Lab cycle** `t07-config`: the probe now takes its seeds from the config's default — identical to
  Task 6's `--seeds matched` run. Gate: `noreg.txt` against t06: 0 moved rows, max |Δ| = 0 (the config switch is
  not a numeric change). The design list is empty.

- [ ] **Step 5: README.** R-M4c-11 gains: "superseded for the default by R-OSP-2 (2026-10): `seedSource =
  matchedFilter` is the default; `peak` and `structure` remain explicit choices with their own dials; a stored
  document decodes per spec §2.2". Commit.

```
feat(stacking): measurement.seedSource (matchedFilter default, peak, structure) with one legacy decode for every stored document, seedThreshold, the Measure panel and presets; R-M4c-11 updated
```

**Acceptance:** the six-fixture decode test; tsc, vitest and `ts_contract` green; `noreg.txt` vs t06 0 moved, max
|Δ| 0; both hosts' `get_stacking_presets`/`get_stacking_defaults` answer the new shape through the unchanged
handlers (`cargo test -p athenaeum-web` and `-p athenaeum-tauri --lib` green).

---

### Task 8: F-3 — the saturation test and the unsaturated statistics (spec §2.1, §3 F-3)

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/photometry.rs` (`saturation`)
- Modify: `star_measure/fit.rs` (after `accept`: the flag), `star_measure/beta.rs` (the discrete search samples
  the 64 brightest UNSATURATED accepted-by-prefit seeds), `star_measure/frame.rs` (`frame_shape` over the
  unsaturated subset; `stars_unsaturated`), `star_measure/mod.rs` (`MeasureOptions.saturation_fraction`),
  `stacking/measure.rs` (hand edit: `ChannelMeasurement.stars_unsaturated`), `examples/star_probe.rs` (`--clip`,
  `--saturation-fraction`), `docs/stacking/README.md` (R-OSP-5)

**Interfaces:**
- Produces:

```rust
// photometry.rs
pub struct SaturationVerdict { pub saturated: bool, pub clip_hit: bool, pub core_pixels: u16, pub overshoot_sigma: f64 }
/// PA-8. `clip`: Some(level) in the plane's units when the domain knows it (RawAdu: saturation_fraction × clip);
/// None on a calibrated plane. `sigma_local` from the star's own annulus (PA-11 radii), never the plane σ.
pub fn saturation(plane: &[f32], w: usize, h: usize, fit: &StarFit, region: &[PixelSample], clip: Option<f64>,
                  sigma_local: f64, pool_scratch: &mut FitScratch) -> SaturationVerdict;
```

  `StarFit.flags |= SATURATED` when the verdict says so; `stars_fitted` keeps counting every accepted fit (M-4);
  `FrameAggregates.stars_unsaturated`; `frame_shape(fits)` → `frame_shape(fits.iter().filter(|f| !f.flags.saturated()))`
  with `None` when empty (Review Focus 3); the β sample excludes flagged stars (the pre-fit at the sample's β=4
  decides the flag before the search — one extra fixed-β fit per sample star, 64 fits); the width start uses only
  unflagged stars for the field width (the matched filter's 100 candidates: those whose peak exceeds the known
  clip or whose core holds ≥ 3 pixels within 2 σ of their maximum are skipped before the field-width median).
- `Domain::Calibrated` → `clip = None`; `Domain::RawAdu { clip }` → `Some(saturation_fraction × clip)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_clipped_moffat_core_is_flagged_and_an_unclipped_one_is_not() {
    // one Moffat-4 star at FWHM 2.6 with amplitude 3× the clip, clipped at 0.95 (≥ 5 core pixels at the clip) → saturated,
    // the same star at 0.6× the clip → not saturated; a broad Gaussian (FWHM 10, SNR 150, unclipped) → not saturated
    // (the wing fit predicts its maximum within noise: the R-3 case)
}

#[test]
fn raw_domain_clip_clause_flags_a_peak_above_the_fraction_times_clip() { /* RawAdu { clip: 65535 }, fraction 0.95, a peak at 63000 → clip_hit */ }

#[test]
fn all_saturated_sample_leaves_the_frame_statistic_empty_not_wrong() {
    // 12 stars, every core clipped: stars_fitted 12, stars_unsaturated 0, frame.fwhm_px is None → the adapter reports 0.0 and warns once
}

#[test]
fn beta_sample_skips_saturated_stars() {
    // 70 Moffat-4 stars, the 40 brightest clipped: Auto resolves to 4.0 (the baseline flipped to 10 on such frames — twins/sharp/m4/f2.0)
}
```

- [ ] **Step 2: Run, fail, implement, run.** Re-pin the measure.rs literals (by design: the statistic's population).

- [ ] **Step 3: Lab cycle** `t08-f3`. `--design`: `{"row":7}` (the statistic's population), `{"row":11}` (the
  twins), `{"row":10,"metric":"auto beta vs saturated top-64 seeds"}`, `{"row":10,"metric":"saturation flag vs
  truth"}`, `{"row":3,"family":"saturated/*"}`, `{"row":3,"family":"twins/*"}`, `{"row":1,"metric":"bright
  coverage (300)"}` (unchanged by design — it counts saturated stars, N-5; judged, stays within SE).

- [ ] **Step 4: Read the bars** (`bars.txt`):
  - self-consistency (row 10 `saturation flag vs truth`, pooled over every family × `measure`): false positives
    ≤ 1 % of truth-unsaturated stars; false negatives 0 among stars with `clipped_core_px ≥ 3`; the
    `neighbour_only`, `gray` and `under_3_core` counts are printed beside (the v2 i0 truth has 81 neighbour-only and
    25 gray of 4105 saturated — reported, not judged);
  - twins (row 11, `base vs deep (2x flux)`): frame |ΔFWHM|/FWHM ≤ 1 % on every `twins/sharp/*` and `twins/mixture/*`
    family (baseline `twins/sharp/m4/f2.0` 9.99 %, `twins/sharp/m2.5/f1.6` 15.93 %, `twins/sharp/m4/f2.6` 2.93 %,
    `twins/mixture/b` 2.62 %); `auto beta agrees` 100 % of pairs on every family (β discrete: zero flips; baseline
    0.0 % of 1 pair on five families) — the twin rows are judged AGAIN at Task 9 (N-6), where "void at the cap" may
    apply.

- [ ] **Step 5: Record R-OSP-5 (PA-8's constants and the row); commit.**

```
fix(star_measure): F-3 one saturation test (raw clip clause + the flat-top wing test, ruling R-OSP-5); the beta sample, the width start and the frame statistic use unsaturated stars (twins: <cells>; flag FP/FN: <cells>)
```

**Acceptance:** FP ≤ 1 %, FN 0 on ≥ 3-clipped-core stars; every twin family ≤ 1 % with zero flips; `noreg.txt` 0
moved outside the design list; control 0.

---

### Task 9: F-4 — continuous per-frame β, the rustafits bounded free-β fit, the submodule bump (spec §3 F-4)

**Files:**
- rustafits (branch `one-star-path-beta` from `8321412`, or the Task 5 branch if it exists):
  `rustafits/src/analysis/fitting.rs` — `fit_moffat_2d_bounded`, `Moffat2DBoundedResult { b, a, x0, y0, alpha_x,
  alpha_y, theta, beta, converged, fit_residual, iters }`, `lm_solve_moffat_with` parametrised by `BetaParam::{Fixed(f64),
  Free { ln_min, ln_max }}`: the free arm carries `params[7] = ln β`, Jacobian column `j[7] = A·power·(−ln(base))·β`
  (chain rule), the step clamp in ln β to `[ln_min, ln_max]`, `MoffatExponent::new(exp(params[7]))`; tests.
- athenaeum: `star_measure/beta.rs` (continuous), `star_measure/fit.rs` (`BETA_AT_CAP`), `star_measure/mod.rs`
  (`BetaSource`, `PlaneMeasurement.beta_source`), `stacking/psf_signal.rs` (`PsfModel::Auto` doc, `group_beta` doc:
  the lower median of continuous βs — still "always one of the members' own βs"), `stacking/measure.rs` (hand
  edit: doc only), `src/components/stacking/panels/MeasurePanel.tsx` + `stageSummary.ts` (`psfModelLabel('auto')`
  → "Auto (per-frame β)", help text), `rustafits` submodule pointer, `Cargo.lock` (unchanged: path dep, same
  version), `docs/stacking/README.md` (R-OSP-6), `examples/star_probe.rs` (`--betas` keeps the discrete arm for row
  3's sensitivity line; frame line `betaSource`)

**Interfaces:**
- Produces (rustafits):

```rust
pub fn fit_moffat_2d_bounded(pixels: &[PixelSample], init_b: f64, init_a: f64, init_x0: f64, init_y0: f64,
    init_sigma_x: f64, init_sigma_y: f64, init_theta: f64, init_beta: f64, beta_bounds: (f64, f64),
    max_iter: usize, conv_tol: f64, max_rejects: usize) -> Option<Moffat2DBoundedResult>;
```

  Refuses (`None`) only `alpha < 0.3` or `A ≤ 0`; a β at a bound is RETURNED (the caller flags it). `fit_moffat_2d`
  keeps delegating to the old fixed-`[1.5, 20]`/`[1, 20]` arm byte for byte (its C3 pins stay).
- Produces (athenaeum): `beta::BETA_BOUNDS: (f64, f64) = (1.5, 100.0)`; `beta::frame_beta(plane, w, h, sample: &[Seed],
  fit: &FitParams, pool) -> (f64, BetaSource)` = the median of the bounded free-β fits of the 64 brightest
  unsaturated sample seeds (≥ 8 accepted fits, else `(4.0, BetaSource::Fallback)` with a `warn!`), clamped to the
  bounds; `BetaSource::{Fitted { sample: usize }, Fallback, Fixed}`; `StarFit.flags |= BETA_AT_CAP` when the frame
  β sits at a bound. `PsfModel::Auto` → `frame_beta`; `Moffat4`/`Fixed(b)` unchanged. `group_beta`: unchanged
  code (any number). `FitDiagnostics.iterations` filled from the bounded fit's `iters` on the sample; `None` on
  the fixed-β pass still (the fixed entry point reports none).

- [ ] **Step 1: Write the failing tests** (rustafits first, then athenaeum)

```rust
// rustafits/src/analysis/fitting.rs
#[test]
fn bounded_free_beta_reaches_a_gaussian_star_at_the_cap() {
    // a pixel-sampled Gaussian star (σ 2.0, SNR ∞): bounded fit with (1.5, 100) ends with β ≥ 95 and FWHM within 0.4 % of 2.3548σ;
    // the fixed-[1,20] fit_moffat_2d on the same stamp returns None or β 20 — the old behaviour, pinned so the two arms stay distinct
}
#[test]
fn bounded_fit_recovers_beta_4_and_2_5_inside_the_bounds() { /* |β − 4| < 0.05, |β − 2.5| < 0.03 on noiseless stamps */ }
#[test]
fn ln_beta_jacobian_matches_central_differences() { /* at β ∈ {2, 10, 60}: |analytic − numeric| < 1e-6 relative */ }
#[test]
fn fit_moffat_2d_is_byte_identical_to_before() { /* the C3 pin inputs through fit_moffat_2d: same params, same iters */ }

// athenaeum beta.rs
#[test]
fn frame_beta_is_the_median_of_the_sample_and_gaussian_reads_near_the_cap() {
    // 70 Moffat-4 stars → β ∈ [3.8, 4.2], BetaSource::Fitted{sample: 64}; 70 Gaussian stars → β ≥ 60, BETA_AT_CAP set when β == 100
}
#[test]
fn beta_sample_below_eight_falls_back_to_four_and_says_so() { /* 5 stars → (4.0, Fallback), one warn (tracing_test or the log capture used elsewhere) */ }
#[test]
fn every_star_is_fitted_at_the_frame_beta() { /* all fits carry the same beta, equal to PlaneMeasurement.beta */ }
```

- [ ] **Step 2: rustafits.** `cd rustafits && git checkout -b one-star-path-beta` (or the Task 5 branch);
  implement; `CARGO_TARGET_DIR=../.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p rustafits --lib analysis::fitting
  -- --test-threads=4`; commit in rustafits:

```
analysis: fit_moffat_2d_bounded - a free-beta Moffat fit in ln beta with caller bounds (the fixed [1, 20] entry points unchanged)
```

  Then in athenaeum: `git add rustafits` (the pointer), implement `beta.rs`, run the module tests.

- [ ] **Step 3: Lab cycle** `t09-f4` (v2 and injections). `--design`: `{"row":3}` (every width moves with β),
  `{"row":7}`, `{"row":11}`, `{"row":10,"metric":"auto beta vs saturated top-64 seeds"}`, `{"row":10,"metric":"auto
  beta agrees"}`, `{"row":6,"metric":"captured fraction"}` (the FWTM ellipse follows β), `{"row":4,"metric":"floor"}`,
  `{"row":4,"metric":"ecc"}`, `{"row":5}`, `{"row":2,"path":"measure"}` (within CRLB).

- [ ] **Step 4: Read the bars** (`bars.txt`):
  - frame FWHM error vs `pointfit` (row 7 `frame FWHM` ÷ truth `pointfit: med` − 1): ≤ 1 % at FWHM ≥ 2 px and ≤ 3 %
    below, on every `sharp/{gauss,m2.5,m4,m10,blur1,blur2.5}/f*` (42 families, incl. f12/f16) and `mixture/*`
    (4). Baseline (measure, pointfit, row 3 bias): `sharp/gauss/f2.6` −3.01 %, `sharp/gauss/f8.0` −2.58 %,
    `sharp/blur2.5/f2.6` −2.50 %, `sharp/blur2.5/f8.0` −2.07 %, `mixture/b` +3.28 % (frame +4.88 %), `sharp/m4/f1.2`
    +4.65 %, `sharp/blur2.5/f1.2` +21.38 % (the F 1.2 rows are judged at ≤ 3 %);
  - the twin rows again (N-6): frame |ΔFWHM|/FWHM ≤ 1 %; `|Δβ|/β ≤ 10 %` between base and deep on every twins
    family whose β is not at a bound on both sides (`void at the cap` rows listed, not judged);
  - raw mode (injections): the injected stars' FWHM bias (row 3 `fwhm` pointfit on `inject/*`): |bias| ≤ 1 % on
    the 7 raw + 3 mosaic + 9 calibrated entries at FWHM ≥ 2 px (baseline Horse C11 G −3.68 %, QHY set 107 +5.33 %,
    `raw/corpus` plane 1 +9.06 %, mosaics plane 2 −5.85 %); a VNG-plane row (planes 1/2 of mosaics) that misses is
    reported under the out-of-scope VNG item, not judged (spec §3 "Out of scope");
  - row 3's `beta sensitivity` line stays (the discrete arm is a lab diagnostic).

- [ ] **Step 5: The UI label, README R-OSP-6, commit** (athenaeum: the module, the pointer, the UI; lab results).

```
fix(star_measure): F-4 continuous per-frame beta in [1.5, 100] from a bounded free-beta sample fit (rustafits one-star-path-beta <sha>); Gaussian is the large-beta limit (<bar cells>)
```

**Acceptance:** every family row ≤ 1 % / ≤ 3 %; twins ≤ 1 % and |Δβ|/β ≤ 10 % (or void); injected FWHM ≤ 1 % on
the mono and ideal-plane entries; `noreg.txt` 0 moved outside the design list; the rustafits tests green on the
branch; `cargo tree -p athenaeum-core | grep rustafits` shows the path dep unchanged in version.

---

### Task 10: F-5 — the profile-independent flux and psfSel (spec §3 F-5)

**Files:**
- Modify: `star_measure/photometry.rs` (`model_flux_in_fwtm`, `aperture_correction`, `growth_curve`),
  `star_measure/fit.rs` (`aperture` replaced by the model flux × `c`; `area` unchanged), `star_measure/mod.rs`
  (`PlaneMeasurement.aperture_correction`; `FitStarsOptions` gains `flux_correction: FluxCorrection::{FromPlane,
  Given(f64)}`, default `FromPlane`), `stacking/psf_signal.rs` (re-exports), `stacking/ln/scale.rs` (NO edit: psfSel reads
  `signal`, which now IS the corrected flux — PA-5), `examples/star_probe.rs`, `examples/ln_scale_probe.rs` (nothing),
  `docs/stacking/README.md` (R-OSP-7)

**Interfaces:**
- Produces: `StarFit.signal` = `model_flux_in_fwtm(fit) × c`, where `model_flux_in_fwtm = A·π·αx·αy/(β−1)·(1 −
  10^{(1−β)/β})` (the analytic integral of the fitted Moffat inside its own FWTM ellipse) and `c` is the frame's
  aperture correction: over the 64 brightest unsaturated accepted fits with no other seed within `6·F_field` px,
  `c = median( (Σ_{r ≤ 5F} (v − b_rim)) / model_flux_in_fwtm )`, `F` the frame's fitted FWHM, `b_rim` the median of
  the `[6F, 8F]` annulus of the same star; `c = 1` with `aperture_correction: None` and a `warn!` when fewer than 5
  stars qualify (Review Focus 3). `fit_stars_at` computes `c` on the plane it fits (`FromPlane`) or takes it
  (`Given`), so LN's two sides each get their own `c`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn captured_flux_is_within_one_percent_of_the_total_for_gaussian_and_moffat_stars() {
    // the same 30-star field rendered as Gaussian (σ 2), Moffat 2.5, Moffat 4, Moffat 10 at equal total flux π·A·αx·αy/(β−1):
    // median signal / total within ±1 % on each; the spread of the four medians ≤ 2 % (the F-5 bar's own condition)
}
#[test]
fn aperture_correction_without_an_unsaturated_isolated_star_is_one_and_flagged() { /* a 3-star crowded field → c = 1, aperture_correction None */ }
#[test]
fn totals_and_weights_read_the_corrected_flux() { /* signal_totals().tflux == Σ signal; psf_signal_weight moves with it */ }
```

- [ ] **Step 2: Run, fail, implement, run.** Re-pin the measure.rs literals (tflux, tmean_flux, PSFSW, PSFSNR move by
  design) and `fixed_beta_fit_recovers_centre_width_background_and_aperture_signal`'s expected signal (now the
  TOTAL × c, not `0.8222·total`).

- [ ] **Step 3: Lab cycle** `t10-f5` (v2 incl. the LN probe on the pairs families — reported, PA-14). `--design`:
  `{"row":6,"metric":"captured fraction"}`, `{"row":10,"metric":"seed flux rank"}` (unchanged seeds: judged),
  the weights rows have no scorecard row (the real-data checkpoint reads them).

- [ ] **Step 4: Read the bars** (`bars.txt`): row 6 `captured fraction` medians over
  `sharp/{gauss,m10,m4,m2.5,blur1,blur2.5}/f2.6` and `/f8.0`: max − min ≤ 2 % of the mean at each F (baseline
  18.73 % at F 2.6 from 0.9078 / 0.7646; 22.16 % at F 8.0 from 0.9157 / 0.7496); psfSel (row 6 `arm psfSel`,
  `ln beta auto`): `pairs/iso/blur25mix` t60 and `pairs/iso/blur4` t60 within ±2 % (baseline −13.03 % / −7.28 %),
  every Moffat and Gaussian `pairs/*` target within ±1.2 % (baseline −1.60 … +1.16 %). F1's `arm f1_gc` rows are
  printed for Plan B (PA-14).

- [ ] **Step 5: R-OSP-7; commit.**

```
fix(star_measure): F-5 star flux = the fitted model's FWTM flux times a per-frame growth-curve correction; psfSel reads it (captured-fraction spread <cells>; blur pairs <cells>)
```

**Acceptance:** spread ≤ 2 % at F 2.6 and F 8.0; psfSel bars; `noreg.txt` 0 moved outside the design list.

---

### Task 11: F-6 — the per-star SNR and HFR (spec §3 F-6, §4.2 HFR)

**Files:**
- Modify: `star_measure/photometry.rs` (`star_snr`, `hfr`), `star_measure/fit.rs` (`StarFit.snr`, `.hfr`;
  non-persisted like `peak5`), `star_measure/frame.rs` (`snr_median`, `hfr_median`), `star_measure/mod.rs`
  (`PlaneMeasurement.snr_gain`), `star_measure/domain.rs` (`Domain::gain_e_per_working_unit()`),
  `stacking/measure.rs` (hand edit: the adapter reads `EGAIN`, `ATH_CSCL`, `ATH_CFNM` from the FITS header through
  `crate::fits_parser` and passes `gain_e_per_unit = EGAIN × ATH_CSCL / ATH_CFNM` — the P-21 rule without the
  flat's pixel, named as the approximation it is; `None` when EGAIN is absent), `examples/star_probe.rs` (same
  header read; `snr`, `hfr` on star lines; `snrGain` on the frame line), `docs/stacking/README.md` (R-OSP-8)

**Interfaces:**
- Produces: `star_snr(plane, w, h, fit, gain: Option<f64>) -> (f64 /* snr */, SnrTerms { s, n_ap, sigma_sky, n_sky,
  shot_term: bool })` per PA-11: `σ_S² = S/g + N_ap·σ_sky²·(1 + N_ap/N_sky)`; without `g` the first term is omitted
  and `shot_term = false`. `hfr(plane, w, h, fit) -> f64`: the radius about the fitted centre inside which the
  background-subtracted cumulative sum (pixels sorted by distance, `b = fit.background`) reaches `0.5 × fit.signal`,
  interpolated between the two bracketing pixels; `NaN` when the 5F circle holds less than half of `signal`.
  Non-finite and masked pixels are skipped and never counted in `N_ap`/`N_sky` (Review Focus 1, 5).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn snr_matches_the_analytic_value_on_a_white_noise_star() {
    // sky 0.05, gain 50000 e/unit, rn 0.0005 → σ_px known; a Moffat-4 star of flux F: snr within 5 % of F / sqrt(F/g + N_ap σ²) over 20 noise draws (median)
}
#[test]
fn snr_without_gain_omits_the_shot_term_and_says_so() { /* terms.shot_term == false; snr is larger than with the gain */ }
#[test]
fn snr_annulus_skips_non_finite_pixels() { /* a NaN wedge in the annulus: n_sky counts only finite pixels; snr finite */ }
#[test]
fn edge_star_snr_uses_the_present_annulus_pixels() { /* a star 4 px from the border: n_sky < full count, snr finite, not NaN */ }
#[test]
fn hfr_of_a_gaussian_is_the_half_flux_radius() { /* σ 2 → HFR ≈ 1.1774σ within 3 % on a noiseless stamp */ }
#[test]
fn the_adapter_reads_egain_cscl_cfnm_into_the_gain() { /* a FITS with EGAIN 0.763, ATH_CSCL 65535, ATH_CFNM 1 → gain_e_per_unit 50000 ± 1e-6 */ }
```

- [ ] **Step 2: Run, fail, implement, run.**

- [ ] **Step 3: Lab cycle** `t11-f6`. `--design`: `{"row":6,"metric":"star SNR vs truth"}` (new numbers), nothing
  else moves (the SNR and HFR are additive fields).

- [ ] **Step 4: Read the bar** (`bars.txt`): row 6 `star SNR vs truth` median bias ≤ 3 % on every `sharp/*` (42) and
  `background/*` (7) family (unsaturated, isolated by the families' own `min_sep`); `n_no_gain` must be 0 (every v2
  image carries EGAIN from Task 1). The u16 `analysis-mode` arm is Task 12's.

- [ ] **Step 5: R-OSP-8; commit.**

```
feat(star_measure): F-6 per-star SNR (S/σ_S with the EGAIN/ATH_CSCL gain, the shot term omitted and said when unknown) and HFR on the F-5 flux (SNR bias vs truth <cells>)
```

**Acceptance:** median bias ≤ 3 % on the 49 families; `noreg.txt` 0 moved outside the design list.

---

### Task 12: The Analysis mode and the budget — Plan C's interface, the cost bars (spec D-3, §2.1 `budget`, §3 budget and cost)

**Files:**
- Create: `crates/athenaeum-core/src/star_measure/budget.rs`
- Modify: `star_measure/mod.rs` (`Mode`, `PlaneMeasurement.budget`), `examples/star_probe.rs` (`--paths
  analysis-mode`, `--clip`, `--budget N`; the raw-domain read keeps `PlaneReader` — XISF and `read_raw` are Plan
  C's), `docs/stacking/README.md` (R-OSP-9), lab: `probe_plan.ANALYSIS_MODE = True`, `scorecard_build` scores the
  `analysis-mode` path with the measure rows (threshold key `fisher_snr`, `seedThreshold`)

**Interfaces:**
- Produces (`budget.rs`, PA-10): `pub struct BudgetReport { pub cells_with_stars: u8, pub cells_covered: u8, pub
  candidates: usize, pub kept: usize }`; `pub fn select_candidates(seeds: &[Seed], w: usize, h: usize, budget: usize)
  -> (Vec<usize>, [u16; 16])` and `pub fn trim(fits: &mut Vec<StarFit>, w, h, budget) -> BudgetReport`.
  `Mode::Budget { budget }` makes `measure_plane` fit only the selected candidates, drop refused and saturated ones,
  and trim; `Mode::All` is the stacking path. The per-frame β sample and the aperture correction use the budget's
  own candidates (they are the brightest anyway).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn budget_covers_every_cell_that_holds_a_star() {
    // 800 stars uniformly placed: Mode::Budget{500} keeps 500 fits and cells_covered == cells_with_stars == 16
}
#[test]
fn budget_keeps_the_brightest_per_cell_when_one_cell_dominates() { /* 600 stars in one cell, 10 elsewhere: every outside cell covered, the dense cell takes the remainder */ }
#[test]
fn budget_mode_measures_at_most_budget_stars_and_reports_refusals() { /* kept ≤ budget; refused.total() counts the candidates that failed, not the unselected */ }
```

- [ ] **Step 2: Run, fail, implement, run.**

- [ ] **Step 3: Lab cycle** `t12-amode`: v2 with the new `analysis-mode` arm on every u16 sibling, and the corpus
  (`manifests/corpus.json` → `~/.athenaeum-acc/star-lab-a/t12-amode/corpus-probe`: `measure` on the calibrated
  reads and `analysis-mode` on the raw files, one path per invocation). `--design`: none (the new path adds rows;
  the existing paths' rows are judged and must not move).

- [ ] **Step 4: Read the bars** (`bars.txt`):
  - budget: row 1 `budget coverage` ≥ 0.9 on every v2 image with ≥ 16 seeds (`cells_with_stars` printed);
  - Analysis cost: the corpus m42 `analysis-mode` invocation walls, mean over the 77 frames ≤ 0.6 s (baseline legacy
    analysis 25.2 s / 77 = 0.33 s), max printed; `background/neb3_4096` `analysis-mode` wall printed beside the
    legacy 0.21 s;
  - Measure cost, second read: the corpus m42 `measure` batch wall ≤ 1.2 × 223.1 s = 267.7 s (per-frame ≈ 2.9 s →
    ≤ 3.5 s; the first read is Task 13's stage time);
  - `_DSC5767.fit`: the `measure` frame line's `seeds` > 0 on each plane (baseline 0 on every plane); recorded — the
    signedness question is Plan C's.
  - bright coverage and completeness of `analysis-mode` against the legacy `analysis` on `sharp/m4/f2.6` recorded
    (baseline legacy 7.3 % of 300, 21.5 % of 554 expected) — Plan C's comparison, not a bar here.

- [ ] **Step 5: R-OSP-9; commit.**

```
feat(star_measure): Analysis mode - a budget of N good unsaturated stars spread over a 4x4 grid before fitting, the BudgetReport (coverage <cell>, m42 analysis-mode <s>/frame, measure batch <s>)
```

**Acceptance:** coverage ≥ 0.9 everywhere; m42 analysis-mode mean ≤ 0.6 s; m42 measure batch ≤ 267.7 s;
`noreg.txt` 0 moved; control 0. A cost miss → the owner with the walls before Task 13 (the +20 % bar binds there
too).

---

### Task 13: The real-data checkpoint at master level (spec §5 step 5, owner rule)

Under the owner's rule a numeric change is judged on the whole group at master level. The harness is reused:
`harness/acceptance/{prepare-catalog.sh,server.sh,api.sh,tier1/checkpoint.sh,tier1/rerun-build.sh,tier1/project-run.sh,
tier1/tier1-extract.py,tier1/tier1-compare.py}` and `harness/ln-model/horse-c11-prep.sh` + the controller's
`~/.athenaeum-acc/lnB/hc11-run.sh` (copied into `harness/acceptance/hc11-run.sh` in this task, the one script the
harness lacked). Nothing is built per cycle.

**Files:**
- Modify: `harness/acceptance/README.md` (item 12: this checkpoint's recipe), `harness/acceptance/hc11-run.sh`
  (added), `harness/acceptance/tier1/tier1-run.sh` (`ATH_ACC_SET_REFERENCE=<json>`: one `set_frame_set_reference`
  call after `set_stacking_config`, echoed into `run.txt`, a refusal stops the run), `harness/acceptance/tier1/tier1-compare.py` (`--numeric` gains the `beta` and `starsRefused` lines beside
  `fwhmPx`; already prints `beta` per L-25), `docs/stacking/README.md` (the acceptance run, R-OSP-10),
  `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-a.md` (§"Real data", written in Task 14)

**Interfaces:**
- Consumes: the "before" trees (PA-13): `~/.athenaeum-acc/tierA-lnB-f1` (set 204, run 5, reference 28934 after the
  two-pass pick, Measure stage 3.62 min, wall 901 s), `~/.athenaeum-acc/lnB-runB-f1` (run B, project
  `af136ec9-1885-4087-b684-9136ad655e9b`, wall 811 s), `~/.athenaeum-acc/hc11B-f1` (Horse C11 set 182, pinned frame
  69845, wall 661 s). `ATH_ACC_DB` for `prepare-catalog.sh` is the tierA template's DB for set 204
  (`checkpoint.sh` copies `tierA-template`), the 6b-3 catalog `.backup` for run B, the dev catalog `.backup` for
  Horse C11 (`horse-c11-prep.sh`, which calls `prepare-catalog.sh`).
- Produces: `~/.athenaeum-acc/tierA-starA` (set 204 after), `~/.athenaeum-acc/starA-runB`, `~/.athenaeum-acc/starA-hc11`,
  each with `extract.txt`, `compare.txt`, `run.txt`; `results/plan-a/t13-real/` in athenaeum-lab (the three compare
  files, the weight rank table, the stage table).

- [ ] **Step 0: Disk and the owner.** Each after-copy holds every intermediate (`cleanup keepAll`, the compare reads
  `.athln` sidecars and registered frames): set 204 ≈ 95 GB, run B ≈ 69 GB, Horse C11 ≈ 52 GB (today's copies'
  sizes). `/Volumes/BigMac` has 165 GiB free with a 100 GiB floor. The three cannot coexist with the floor; one at
  a time, each deleted after its rows are recorded, still needs 95 GB of headroom for set 204. **Before this task
  starts, the owner names which existing copies may go** (candidates with their accepted records already in
  `docs/stacking/README.md` and the LN acceptance report: `tierA-lnB-main` 95 GB, `lnB-runB-f1` 69 GB after its
  Measure-equivalence check below has been read, `hc11-main` 4.2 GB, `acc6b3-runB` 69 GB). Nothing is deleted on
  the implementer's initiative; the task records the owner's answer. Time Machine stays off during the runs
  (`checkpoint.sh` refuses while a backup runs).

- [ ] **Step 1: Measure-equivalence of the before trees (PA-13).** Build local main's `measure_probe` in a throwaway
  worktree target (`tier1/rerun-build.sh` does this for a run; here only the example is needed: `git worktree add
  /tmp/claude-501/.../main-wt ed5fa8de && CARGO_TARGET_DIR=.superpowers/target-main cargo build --release -p
  athenaeum-core --example measure_probe`), then for three calibrated frames of each tree (the first, the median
  and the last `calibrated` artifact of the mono group by `frames.id`, read from the copy with `immutable=1`):
  `measure_probe <calibrated.fits> auto` versus the tree's `stacking_artifacts.payload_json` (`kind = 'metrics'`)
  — every `ChannelMeasurement` field equal as printed (timings excluded). Equal on all nine → the trees are the
  before; a difference → that dataset's before is re-run with `rerun-build.sh ed5fa8de starA-before-<ds>` (set
  204 through `checkpoint.sh`'s own path; run B/Horse C11 through their scripts on the main worktree's build) and
  the owner is told the disk this costs. Record the nine comparisons in `results/plan-a/t13-real/before-equivalence.txt`.

- [ ] **Step 2: Pin the reference.** The before run's pick, per dataset: set 204 — `stacking_runs.reference_frame_id`
  of run 5 in `tierA-lnB-f1` (28934, the two-pass result); run B — the project's reference uuid in
  `stacking_project_config.reference_frame_uuid` of `lnB-runB-f1` (already a manual pin on the project path); Horse
  C11 — 69845 (already pinned by `hc11-run.sh`). The after runs send, before `start_stacking`, the before config
  with `reference.mode = "manual"` and `reference.twoPass = false` through `ATH_ACC_SET_CONFIG` (set 204:
  `~/.athenaeum-acc/starA/set204.json`, the `get_stacking_config` document of run 5 with those two fields changed and
  `paths` absent) and `set_frame_set_reference {"framesSetId": 204, "frameId": 28934}` (a line added to
  `tier1-run.sh` under `ATH_ACC_SET_REFERENCE=<json file>`, mirroring `ATH_ACC_SET_CONFIG`); run B through
  `ATH_ACC_PROJECT_CONFIG` with `reference.mode = "manual"` (the uuid is already stored); Horse C11 through
  `hc11-run.sh` as is. A reference that `get_stacking_plan` reports differently from the pin refuses the run.

- [ ] **Step 3: Build the branch's release server** (one compile): `CARGO_TARGET_DIR=.superpowers/target-acc
  CARGO_BUILD_JOBS=4 cargo build --release -p athenaeum-web -p athenaeum-core --example measure_probe`. Record
  `git rev-parse HEAD` and the submodule sha.

- [ ] **Step 4: Run, one at a time** (each: `memory_pressure` ≥ 30 %, `df` ≥ 100 GiB + the copy's size):

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/harness/acceptance
ATH_ACC_SET_CONFIG=~/.athenaeum-acc/starA/set204.json ATH_ACC_SET_REFERENCE=~/.athenaeum-acc/starA/set204-ref.json \
  zsh tier1/checkpoint.sh starA lnB-f1 --numeric                       # expected wall ≈ 901 s (+ Measure ≤ +20 %: 3.62 → ≤ 4.34 min)
python3 tier1/tier1-compare.py --ln-grids ~/.athenaeum-acc/tierA-starA ~/.athenaeum-acc/tierA-lnB-f1 >> ~/.athenaeum-acc/tierA-starA/compare.txt
# record, then free: rm -rf ~/.athenaeum-acc/tierA-starA/stackwork (keep athenaeum.db, stackout, extract/compare/run.txt, logs)
D=~/.athenaeum-acc/starA-runB; mkdir -p $D/library $D/stackwork $D/stackout; sqlite3 -readonly ~/.athenaeum-acc/acc6b3-runB/athenaeum.db ".backup $D/athenaeum.db"
sqlite3 $D/athenaeum.db "UPDATE settings SET value = replace(value, 'acc6b3-runB', 'starA-runB') WHERE value LIKE '%acc6b3-runB%';"
ATH_ACC_PROJECT_CONFIG=~/.athenaeum-acc/starA/runB.json zsh tier1/project-run.sh $ATHENAEUM_REPO $D 8961 af136ec9-1885-4087-b684-9136ad655e9b   # expected ≈ 811 s
python3 tier1/tier1-compare.py --pairs <head masters>=<lnB-runB-f1 masters> > $D/compare.txt
zsh ../ln-model/horse-c11-prep.sh ~/.athenaeum-acc/starA-hc11 8970 && cp ~/.athenaeum-acc/hc11B-f1/c11-config.json ~/.athenaeum-acc/starA-hc11/ && zsh hc11-run.sh $ATHENAEUM_REPO ~/.athenaeum-acc/starA-hc11 8971   # expected ≈ 661 s
python3 tier1/tier1-compare.py --pairs <head L and G masters>=<hc11B-f1 masters> > ~/.athenaeum-acc/starA-hc11/compare.txt
```

  Every master (both sides) is measured by `measure_probe` from THIS build (the new path) inside
  `tier1-compare.py` — that is "both masters are measured by the new path".

- [ ] **Step 5: Read the bars** per dataset (`results/plan-a/t13-real/bars.txt`, the compare files verbatim):
  - master median within ±0.1 %, MAD ±1 %, noise (MRS σ) ±2 %, FWHM ±1 % (the §8 rows of `--numeric`, `docs/stacking/
    README.md` "masters within 0.1 % median / 1 % MAD / 2 % noise / 1 % FWHM") — mono and every OSC plane, plain and
    drizzled; "or better" means a row that is inside the tolerance passes whatever the direction;
  - per-frame weights: Spearman ρ ≥ 0.9 against the before run (`stacking_run_frames.weight` joined by `frame_id`),
    and every frame whose weight rank moved by more than 10 % of N is listed with the scorecard row that explains
    it (F-5's captured fraction, F-3's unsaturated statistic, F-4's β) — a move without a row goes to the owner;
  - LN relative scale `s` per frame: median ratio within 1 ± 0.5 %, scatter ≤ 1 % (the §8 row), read from the
    `.athln` grids;
  - Measure stage wall ≤ 1.2 × the before tree's (set 204: 3.62 min → ≤ 4.34 min; run B and Horse C11 from their
    `extract.txt`); total wall reported (interleaving is not possible without a second 95 GB copy: the before walls
    are quoted with their dates, R-TA-9 noted);
  - the stacking `metrics` payload's `starsRefused` and `beta` per frame are reported (new fields), and the master
    `beta` line (L-25) beside the FWHM row.

- [ ] **Step 6: Record** the run (R-OSP-10) in `docs/stacking/README.md` with every row's number; commit the compare
  files in the lab; delete the after-copies' `stackwork` once the rows are recorded (keep the rest for the final
  gates' `probe_parity`, which reads `tierA-starA` — set `STARLAB_ACC_COPY=~/.athenaeum-acc/tierA-starA` in Task 14;
  the parity gate needs the `.athf` files under `stackwork/…`, so set 204's `stackwork` is deleted only AFTER Task 14).

```
docs(stacking): Plan A real-data checkpoint - set 204, run B, Horse C11 before/after at master level with the reference pinned (ruling R-OSP-10: <rows>)
```

**Acceptance:** every master row inside the tolerances; ρ ≥ 0.9 with every larger move explained; LN scale within
0.5 %; Measure ≤ +20 % on each dataset. A miss goes to the owner with the compare file before Task 14.

---

### Task 14: One full uniform lab run and the "after Plan A" note (spec §5 "At the end")

**Files:**
- Create: `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-a.md` (athenaeum; `XX` = the run's day)
- Modify: `docs/stacking/README.md` (the section's closing paragraph), `.superpowers/sdd/…/progress.md` (the ledger)
- Lab: `results/plan-a/final/{gates.txt,scorecard-summary.md,scorecard-inject-summary.md,divergent-families.txt,
  noreg-vs-baseline.txt,path-compare.txt,twin-saturation.txt,provenance.txt,run-log.txt}`

- [ ] **Step 1: One uniform run, in this order, one job at a time, every command and wall logged in `run-log.txt`:**

```
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum && git status --porcelain --untracked-files=no   # clean
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo build --release -p athenaeum-core --example star_probe --example ln_scale_probe --example measure_probe
cd ../athenaeum-lab/star-lab
python3 bin/run_probe.py manifests/v2.json     --out ~/.athenaeum-acc/star-lab-a/final/probe        && python3 bin/run_probe.py manifests/v2.json --out ~/.athenaeum-acc/star-lab-a/final/probe --verify
python3 bin/run_probe.py manifests/inject.json --out ~/.athenaeum-acc/star-lab-a/final/inject-probe && python3 bin/run_probe.py manifests/inject.json --out ~/.athenaeum-acc/star-lab-a/final/inject-probe --verify
python3 bin/run_probe.py manifests/corpus.json --out ~/.athenaeum-acc/star-lab-a/final/corpus-probe && python3 bin/run_probe.py manifests/corpus.json --out ~/.athenaeum-acc/star-lab-a/final/corpus-probe --verify
python3 bin/scorecard.py manifests/v2.json --probe-dir ~/.athenaeum-acc/star-lab-a/final/probe --erp-dir ~/.athenaeum-acc/star-lab-v2/erp --out … --json …
python3 bin/scorecard.py manifests/inject.json --probe-dir ~/.athenaeum-acc/star-lab-a/final/inject-probe --out … --json …
python3 bin/scorecard_summary.py …; python3 bin/baseline_flags.py …; python3 bin/twin_saturation.py …; python3 bin/path_compare.py … ; python3 bin/star_test_corpus.py table …
python3 bin/scorecard_diff.py ~/.athenaeum-acc/star-lab-v2/results/baseline-2026-10-07-fw/scorecard.json.gz <final json> --out results/plan-a/final/noreg-vs-baseline.txt --design results/plan-a/final/design-all.json
STARLAB_ACC_COPY=~/.athenaeum-acc/tierA-starA STARLAB_SCRATCH=~/.athenaeum-acc/star-lab-a/final/gates python3 bin/gates.py > results/plan-a/final/gates.txt
```

  `design-all.json` is the union of every task's design list (so the final diff against the ORIGINAL baseline
  prints the by-design rows in their table and judges the rest: 0 moved expected). The gates: 13 PASS expected
  (`probe_parity` now against `tierA-starA`'s artifacts written by the Task 13 build — the same code as this run's
  probe; `determinism` covers 170 families).

- [ ] **Step 2: The note** `2026-10-XX-star-measurement-after-plan-a.md`, beside the baseline and in its format
  (numbers copied as printed; derived numbers marked): §1 verdict per bar (F-0…F-7, the budget, the costs, the real
  data — one line each: bar, baseline cell, after cell, met/missed); §2 method (binaries' sha256, athenaeum HEAD,
  rustafits sha, lab sha, the one-binary proof counts); §3 rows 1–11 in the baseline's tables with the after cells;
  §4 real data (Task 13's rows); §5 what Plan B starts from (the LN rows, PA-14, printed); §6 what Plan C needs
  (the `analysis-mode` vs `analysis` comparison on the corpus and on `sharp/*`, the `_DSC5767` seed counts, the
  cost); §7 open items (VNG per-plane widths and the resampling broadening restated with their after numbers; any
  missed bar with the owner's ruling).

- [ ] **Step 3: The full core test suite before the owner's word** (`feedback_full_core_test_before_push`):

```
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | tail -5
CARGO_TARGET_DIR=.superpowers/target-acc CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -p athenaeum-tauri --lib -- --test-threads=4 2>&1 | tail -5
CARGO_TARGET_DIR=.superpowers/target-acc cargo check -p athenaeum-core --no-default-features
npx tsc --noEmit && npx vitest run
grep -rn -i -E "$(python3 -c 'import sys; sys.path.insert(0,"../athenaeum-lab/star-lab/bin"); import run_probe; print(run_probe.erp_pattern())')" crates/ src/ docs/ rustafits/src ../athenaeum-lab/star-lab | grep -v 'erp/run_job.sh' | wc -l   # 0 (the name is read from the launcher, never typed)
```

- [ ] **Step 4: Commit** (athenaeum, not pushed; lab pushed) and hand the branch to the final review.

```
docs(research): star measurement after Plan A - one uniform lab run, every bar against its baseline cell, the real-data checkpoint
```

**Acceptance:** 13 gates PASS; `noreg-vs-baseline.txt` 0 moved outside `design-all.json`; every Plan A bar line in
the note's §1 reads "met" or carries the owner's ruling; the full core suite green; the ERP's name absent.

---

## Spec coverage

| Spec item | Task |
| ---- | ---- |
| §2.1 module units `domain`, `fit`, `frame`, `robust` move | 2 |
| §2.1 `seeds` (matched filter, default seed source) | 6, 7 |
| §2.1 `beta` | 9 (discrete moved in 2) |
| §2.1 `photometry` (saturation test, flux, SNR) | 8, 10, 11 |
| §2.1 `budget` | 12 |
| §2.1 `diagnostics` | 4 |
| §2.1 public surface `measure_plane`, `fit_stars_at`, `MEASURE_VERSION` | 2, 3 |
| §2.1 the saturation test (clip clause, flat-top, truth flag, self-consistency row) | 1 (row), 8 |
| §2.1 exemptions (LN L-14/L-23/peak-SNR, registration cuts) | untouched (2, 10 leave `ln/selection.rs`, `register/detect.rs` as they are) |
| §2.2 stacking adapter, `psf_signal` formulas, LN refit via `fit_stars_at`, master statistics | 2, 10 |
| §2.2 `stars_fitted` meaning, unsaturated statistic, `stars_refused` + per-reason counts | 4, 8 |
| §2.2 Measure config: `seed_source`, legacy decode of every stored document, Median3 = Peak's, R-M4c-11 | 7 |
| §2.2 `psf_model` Auto = continuous β, Moffat4 kept, `STACKING_CONFIG_VERSION` unchanged | 9, 7 |
| §2.2 Analysis/Blink, not touched (plate-solve detector, rustafits analysis), the lab's two modes + `legacy-analysis` | 12 (the arm), Plan C (the switch) |
| §3 doctrine, F-0 | 4 |
| §3 F-1 (region, cap, rim background, width start, seeds, new families, donut) | 1, 6 |
| §3 F-2 (axes, conditioning, conditional convergence change) | 5 |
| §3 F-3 (unsaturated sample/statistic, twin bars, re-judged after F-4) | 8, 9 |
| §3 F-4 (continuous β, rustafits bound and ln β, Gaussian limit, every β a finite number) | 9 |
| §3 F-5 (flux, aperture correction, psfSel) | 10 |
| §3 F-6 (SNR definition, gain, lab EGAIN/`ATH_CSCL`, truth SNR) | 1, 11 |
| §3 F-7 (rim background under nebulosity) | 6 |
| §3 F-8, F-9 | Plan B (the LN rows are printed, PA-14) |
| §3 raw-mode truth (injections: completeness, FWHM) | 6, 9 |
| §3 the budget (≥ 90 % of cells) | 12 |
| §3 no regressions (SE rule, mechanical) | 1 (tool), every fix task |
| §3 cost (Measure ≤ +20 %, Analysis ≤ 0.6 s incl. on-demand) | 12, 13 |
| §3 out of scope (VNG, resampling) | 14 §7 (restated) |
| §4.4 `MEASURE_VERSION` in the three stage hashes, the pinned tests, the bump policy | 3 |
| §4.4 `measure_version`/`stars_refused` columns, `FrameAnalysis` fields, collab meta | Plan C |
| §5 step 1 (lab first) | 1 |
| §5 step 2 (extraction gate) | 2 |
| §5 step 3 (F-0) | 4 |
| §5 step 4 (one fix per step, config with F-1 and F-4) | 5–11, 7 |
| §5 step 5 (real-data checkpoint, pinned reference, pass bars) | 13 |
| §5 "At the end" (uniform run, after report) | 14 |
| §6 global constraints | Global Constraints |
| §7 risks (first run re-measures; speed) | 3 (risk line), 12, 13 |
