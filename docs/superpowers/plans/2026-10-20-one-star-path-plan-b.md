# One Star-Measurement Path — Plan B (F1, the LN referee and `c`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: the user skill `lean-execution` (`~/.claude/skills/lean-execution/SKILL.md`)
> above `superpowers:subagent-driven-development`: one phase per agent, a one-page handoff, reports of at most 80 lines,
> one command per lab cycle printing a verdict of at most 30 lines, `haiku` for proof runs (the answer fixed in advance,
> a hard STOP), `sonnet` or stronger for judgment runs. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the LN scale judged against a validated referee on Plan A's fits, `c` visible on every real run, `c`'s floors
and its sample settled, the default LN estimator the owner's choice with the numbers, and LN's second estimates of
Measure's quantities retired where the estimator the owner keeps is module-fed — stages B0 → B1 → B2 → B4 → B3 → B5 →
B6 of the spec (§5, R-B14), every bar fixed in the spec before any measurement.

**Architecture:**
- **athenaeum** (`crates/athenaeum-core/src/`): B0 adds three additive `Option`s to `stacking::measure::ChannelMeasurement`
  and three to the run summary's LN rows, plus one `info!` per (group, channel); B4 changes `star_measure::photometry::
  aperture_correction`'s qualifier screens and sample and bumps `MEASURE_VERSION` to 5; B3 changes a default only on the
  owner's word (`stacking::integrate::LocalNormalizationConfig`), with the module ring API change
  (`star_measure::photometry::ring_sky` taking a caller F and a union mask) only if the F1 v2 sky wins its entry test; B5
  moves LN's seeds onto the module (`stacking::ln::{mod,scale,selection,calibration}`) only if B3 keeps a module-fed
  estimator. No command surface changes; no rustafits change.
- **athenaeum-lab** (`star-lab/`, `harness/acceptance/`): the ERP column beside every LN arm (`starlab/scorecard.py`),
  an LN freshness audit (`bin/erp_audit.py`), the ERP LN driver for the real sets (`bin/erp_ln_real.py` over the lab's
  `erp/run_job.sh`), the truth tools (`bin/ln_truth_rebuild.py`, `bin/ln_truth_run.py`, `bin/truth_modes.py`), the
  seeing-lever injected pairs and the realistic-field family (`starlab/inject.py`, `synth.py` / `make_grid.py`), the
  floor-case builder (`bin/floor_cases.py`), the F1 v2 port's real read (`f1v2/`), the B3 verdict table (`bin/b3_table.py`),
  and one driver per stage (`harness/acceptance/planb.sh <stage> <job>`, the `t13c.sh` pattern: one command per job, a
  ≤ 30-line `verdict.txt`, the results-only disk rule).
- **Data and machines**: the dev Mac's `~/.athenaeum-acc` (the three acceptance trees, the lab grid, `star-lab-v2`,
  `star-lab-a`, the injected and mono2 frames, the F1 v2 port's runs), `/Volumes/bigbase2` and `/Volumes/bigbase3` (the
  raw OSC frames and the inject masters), the runner's `star-lab-l` (the nebula-sky pairs) and its lab data copy. The
  dev Mac runs every step that reads a tree or `/Volumes/bigbase*` (the master-level runs, `ln_real`, every ERP run on a
  real or injected frame), the builds and the parallel geometry cycle; the M4 Max runner runs the synthetic lab cycles
  and the ERP on synthetic frames (PB-16, PB-17). Plan B's probe runs, scorecards
  and clones go under `~/.athenaeum-acc/star-lab-b/<step>/` and `~/.athenaeum-acc/planb-<ds>-<stage>/` on the machine
  that runs the step (ruling PB-16); results are committed in athenaeum-lab under `star-lab/results/plan-b/<stage>/`.

**Tech stack:** Rust (athenaeum-core), Python 3.14 + numpy 2.4 (lab, `unittest`), `sqlite3` with `immutable=1` for reading
acceptance copies, the athenaeum-lab acceptance harness (`harness/acceptance/`), the ERP driven headless by the lab's
`erp/run_job.sh` on the machine that holds the frames.

**Spec:** `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` at 3c350f9d (branch `plan-b`): the
amendment header l.36–58, §3 rows F-8 (l.290), F-9 (l.291), F-11 (l.293), F-12 (l.294), F-13 (l.295), the "Plan B bars"
block l.368–527, the owned-quantities table l.528–561, §5's Plan B paragraph l.695–770, §6 l.781–797. Binding beside it:
`.superpowers/sdd/2026-10-20-plan-b/rulings.md` (R-B1…R-B17 and the owner's ERP YES of 2026-10-20) and the facts page
`.superpowers/sdd/2026-10-20-plan-b/design-read.md`; the reviews already folded: `spec-review.md`, `spec-fold-report.md`.
Plan A: `docs/superpowers/plans/2026-10-07-one-star-path-plan-a.md`; the after note
`docs/superpowers/research/2026-10-20-star-measurement-after-plan-a.md`. Every bar below is quoted from the spec with
its line; a task may not add, invent or loosen one.

## Global Constraints

- No third-party program name anywhere in this repository (code, comments, docs, commit messages); the comparison
  program is "the ERP". Lab automation lives in athenaeum-lab only (spec §6 l.783–784).
- Never read or write the production catalog; acceptance copies only as `sqlite3 "file:…?immutable=1"`, working copies
  via `prepare-catalog.sh` (l.785–786) or the clone recipe (`harness/acceptance/clone-before.sh`, Plan A Task 13: the
  ONE script that opens a clone's catalog writable). A before tree is never written; a clone is this plan's scratch.
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `calibration_library/light_cal.rs`,
  `stacking/measure.rs`, `stacking/register/detect.rs`; no `cargo fmt` (l.787–788). Hand edits only in those files;
  `rustfmt` runs only on the files a task names by path and that are not in that list.
- Cargo with `CARGO_TARGET_DIR=.superpowers/target-acc`, `CARGO_BUILD_JOBS=4`, `--test-threads=4`; one heavy job at a
  time with `memory_pressure` ≥ 30 % free; stop below 100 GiB free disk (l.789–790). `cargo test` takes ONE positional
  filter (Plan A ruling F15).
- The full `cargo test -p athenaeum-core --all-targets` before any push (l.791); `cargo check --no-default-features`
  compiles neither `integration/` nor `stacking/`, so the headless gate is not coverage for Plan B's code.
- `tracing` only; never swallow an error; a refused star is a counted, reasoned outcome, not a silent drop (l.792).
- Two backends in sync if any command surface changes (none planned); serde snake_case ↔ camelCase verified against
  `src/types/models.ts` (l.793–794). Plan B changes no command; `SummaryFrame` / `SummaryGroup` are TS-exported
  (`src/types/stacking.ts`), so B0 regenerates the TS types.
- Every ruling and acceptance run is recorded in `docs/stacking/README.md` as R-OSP-16 (l.795–796; the spec header l.45);
  `docs/settings/README.md` and `docs/platesolving/input-and-acceptance-gates.md` change with their sections (none
  planned).
- The measurement doctrine (spec §3 preamble, restated at l.371–373): a missed bar goes to the owner with its numbers and
  is never moved; a bar that cannot be derived is reported with its reason; a detail tagged (R-B13) is not re-opened.
- The ERP is a column in every LN table, synthetic and real, never the referee (the owner's rule); the ERP is re-run
  only when synthetic frames change, and every first ERP LN run Plan B names is cleared by the owner's ERP answer of
  2026-10-20 (rulings.md). The ERP's automation scripts never enter the athenaeum repository.
- Plan by measured quantity (memory `feedback_plan_by_measured_quantity.md`): one owner per shared input; a consumer
  reads, never re-estimates; a fix round that adds a second estimate of an owned quantity is a STOP and a re-audit.
  Every task below opens with its coupling checklist.
- Lab code is committed in athenaeum-lab (`main`) and pushed (`git push origin main`) after each task's lab step; the
  runner's lab commits may be unsigned (the owner approves per commit). athenaeum is committed on `plan-b` and NOT
  pushed (it reaches the runner as a git bundle through Universe, PB-17); the merge into local main and any push happen only on the owner's word (B6).
- Commits are authored as `eg013ra1n` / `vilen.sharifov@gmail.com`, every message ending with the trailers
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01NDfRyApE6tYi9ZMD7b4g8m`.
- One heavy job at a time on the dev Mac (16 GB): a cargo build, a probe batch, a scorecard build, a stacking run, an
  ERP instance, a generator run. ONE lock, `~/.athenaeum-acc/HEAVY.lock`, serializes Plan B and the geometry cycle (PB-16); the lab
  runners also wait on `pgrep` and `memory_pressure`. Disk: `df -k /Volumes/BigMac` ≥ 100 GiB + the step's peak before every step that writes.
- The results-only disk rule (memory `feedback_lab_results_only_disk_rule.md`): every job deletes its clone once its
  results are committed, except the clones a later stage's comparison still reads (PB-15 names them and when they go);
  a STOP keeps everything as evidence. Never delete `star-lab-a` (the full scorecards) or the three before trees.
- Every test a task tells the implementer to write is written in full with its assertion; a test that cannot fail
  before its change is labelled a pin and sits outside the RED step (Plan A rulings F28, F29).
- Every handoff states a tool-call budget (≤ 120–150 calls) and ONE job (memory `feedback_lean_execution.md`).

## Review Focus

1. **A `metrics` payload written before B0 read by B0's code, and a B0 payload read by 068cf2d6's code** (a user's
   catalog after the update; a rollback). The three new `Option`s decode `None` from an old payload and are absent from
   the JSON when `None`, so a plane without a measured `c` writes bytes identical to today's; `#[serde(default)]` on the
   struct already covers the reverse direction. Pinned in B0 (`channel_c_fields_round_trip_and_an_old_payload_decodes`,
   `channel_without_c_serializes_as_before`).
2. **A group whose every plane is at floor (i)** (a sparse field, a crop, a Plan C CFA-masked plane: `c = None` on every
   member). The per-(group, channel) `info!` must still fire with `planes = 0`, `count = n` and no median (the median
   field absent, never `NaN` in a log line), and the run must not panic on an empty median. Pinned in B0
   (`group_c_summary_with_every_plane_at_floor_reports_zero_planes`).
3. **A plane whose stratified sample cannot fill its strata** (a frame with 6 qualifiers; a frame whose qualifiers are
   all in one brightness decade; a frame where the point-source screen refuses every bright star). B4's sample must
   degrade to the qualifiers it has (never pad, never duplicate a star across strata), `c` must still be the median over
   ≥ 5 ratios or `None` with the existing `warn!`, and the screen's refusals must be counted in `skipped`. Pinned in B4
   (`stratified_sample_with_six_qualifiers_takes_all_six_once`, `point_source_screen_refusing_every_star_floors_c_and_warns`).
4. **A member whose LN F1 growth has 0 or 1–4 pairs on a real-sized frame** (F-9 (ii)'s floor reached in the product:
   a binned or heavily cropped frame, a masked-overlap corner). After B4's ruling the product behaviour is whatever the
   owner chose; whichever it is, the frame keeps a scale and a kept record of `ln_growth_pairs` (B0's summary field),
   and a 0-pair member still normalizes. Pinned in B0 (`ln_summary_keeps_growth_pairs_and_factor_for_a_zero_pair_member`)
   and re-pinned by B4 if the behaviour changes (`growth_floor_behaviour_matches_the_owner_ruling`).
5. **A run on a catalog whose cached Measure artifacts carry `measureVersion = 4` after B4 ships `5`** (every user's
   first run after the update). Measure, Register and Normalize re-run once, the Calibrate artifacts are reused, the
   stage-hash pins (B0) move by exactly the version and nothing else, and the release note names the ripple (spec
   §7's first risk). Pinned in B4 (`measure_version_five_moves_the_three_fit_reading_subtrees_only`, the existing
   `calibration_subtree` pin unchanged).

## Plan rulings (where the plan has to be more precise than the spec)

- **PB-1 B0's placement (R-B2 rules the placement; the names exist).** `ChannelMeasurement` gains
  `aperture_correction: Option<f64>`, `aperture_se: Option<f64>`, `aperture_stars: Option<usize>`, each
  `#[serde(skip_serializing_if = "Option::is_none")]` (camelCase `apertureCorrection` / `apertureSe` / `apertureStars` in
  the payload, the probe's spellings), copied from the adapter's reported `PlaneMeasurement` fields at the one place the
  channel is built (`stacking/measure.rs`, `let channel = ChannelMeasurement { … }`, l.1123 today). `MEASURE_VERSION`
  stays 4; no stage hash moves (the hash folds the config and the version, never the payload). The per-(group, channel)
  line is `tracing::info!` `"measure: aperture correction resolved per channel"`, emitted once per channel in
  `stacking/run.rs::measure_group_finish` after the fan-out, beside the existing once-per-group `"measure: group beta
  resolved; every member is fitted at it"`, with `run_id`, `group_key`, `plane` (the channel index, the Measure sense),
  `planes` (the planes of this channel that carry a measured `c`; reused in the Measure sense, the dictionary entry
  noting the reuse), `count` (the planes of this channel at floor (i): the "fewer than 5" warn fired), `aperture_correction`
  (the median of the measured planes' `c`; absent when `planes = 0`), `aperture_correction_min`, `aperture_correction_max`
  (the two new names, the `beta_own_min` / `beta_own_max` form, entered in the logging spec's dictionary by B0). The
  per-plane `"aperture correction measured"` stays `debug` (the logging rule).
- **PB-2 LN's kept place.** The cheapest kept place for LN's per-side `c`, F1's growth factor and `ln_growth_pairs` is the
  run summary (`stacking_runs.summary_json`, `stacking::provenance::SummaryFrame` / `SummaryGroup`, DB-visible, read by
  `tier1-extract.py` and the Results panel): `SummaryFrame` gains `ln_growth_factor: Option<f64>`, `ln_growth_pairs:
  Option<usize>` (the MINIMUM over a frame's planes, so an OSC channel at floor (ii) shows) and `ln_aperture_correction: Option<f64>` (the TARGET side's `c` as the module gave it on the registered
  plane, mean across planes like `fwhm_px`, `Some` only when every target channel carries `c` (R-PB0-5); the module measures the target's `c` under both estimators — F1 uses it to
  order the pairs — so it is `Some` for a frame LN computed in this run; `None` for a frame LN did not normalize and on an LN cache hit,
  which carries only the scale: `set_ln_summary(…, true)`, run.rs:12937); `SummaryGroup`
  gains `ln_reference_aperture_correction: Option<Vec<Option<f64>>>` (the reference's `c` per plane, `null` at floor (i); R-PB0-7). All `#[serde(default)]` on plain `Option`s (`null` in the payload: provenance.rs uses no `skip_serializing_if`, and a
  skipped `ts_rs` `Option` arrives `undefined`), camelCase, no new log field name: the values already travel
  on the `"ln relative scale"` debug (`ln_growth_factor`, `ln_growth_pairs`) and the `"ln scale: per-side ring"` debug
  (`aperture_correction`); B0 threads them from `ln::scale`'s event struct through `ln::LnFrameOutcome` (ln/mod.rs:354) to
  `run.rs::set_ln_summary`. TS: `src/types/stacking.ts` regenerated (`ts_export.rs` registry unchanged: the two structs
  are already registered). `ChannelMeasurement` is not TS-exported (verified: `ts_export.rs` names `SummaryFrame`, not
  `stacking::measure::ChannelMeasurement`), so its new fields need no TS change.
- **PB-3 B0's identity chain on real data (F-11's "every `.athf` byte-identical").** No run at 068cf2d6 exists on disk
  (`starA-s204` was deleted 2026-10-20; the trees' own runs are from the LN cycle, before Plan A), so the real-data
  identity is proven as a chain, each link its own fixed answer: (a) the lab identity: B0's `star_probe` / `ln_scale_probe`
  / `measure_probe` outputs equal `~/.athenaeum-acc/star-lab-a/t14b/probe` and `…/t14b/inject-probe` under
  `probe_identity.py --drop-key ms` (Task 14 proved t14-final's outputs equal t14b's: `results/plan-a/final/
  identity-vs-t14b.txt`), and `lab_cycle --prev t14-final` reads noreg 0 moved, max |Δ| 0, the LN rows included
  (`changed 0` where Plan A read `changed 3220`); (b) the three fit-reading stage-hash pins B0 adds to `stacking/config.rs` (the default config at `MEASURE_VERSION` 4; the
  existing test there is only relative) pass unchanged;
  (c) the gates on B0's fresh set 204 run (`STARLAB_ACC_COPY=<B0's clone> STARLAB_ACC_RUN_ID=<its run>`): 13 / 13, with
  `probe_parity` (the run's `.athf` equal the probe's on the same frames); (d) the kept values equal the debug
  `"aperture correction measured"` values plane by plane on the set 204 and Horse C11 runs (`ATH_ACC_LOG_EXTRA=
  athenaeum_core::star_measure=debug`, a lab tool joining the `metrics` payload rows to the log lines; the spec's own
  F-11 bar). (a) + (c) give the `.athf` identity by transitivity; (d) gives the kept value's identity.
- **PB-4 B1's synthetic source.** The per-member LN lines of `t14-final` are the probe outputs under
  `~/.athenaeum-acc/star-lab-a/t14b/probe/**/ln.p<p>.b<arm>.jsonl` (equal to t14-final's by the identity above; the
  `t14-final` step directory itself holds logs only, verified), plus `star-lab-a/results/t14-final/scorecard.json.gz`
  (73 403 rows carrying `s`, `s_true`, `rel` per member and arm; the ERP on `sLib` rows only). The ERP column comes from
  `~/.athenaeum-acc/star-lab-v2/erp/<family>/[grid/family/]ln.csv` (24 files). B1 re-scores from those outputs with the
  changed `rows_ln` (`scorecard.py --probe-dir star-lab-a/t14b/probe` into `star-lab-b/b1-synth/`); no probe runs.
- **PB-5 The F1 v2 port's real read (R-B16 I-4, R-B17).** Members: the 30 set 204 frame ids and the 28 Horse C11 G ids in
  `star-lab/f1v2/log-real-s204-bauto.txt` / `log-real-hc11G-bauto.txt` (the `f<id>` tokens; the subset file
  `erp-ln-s204/subset.txt` and the `ln-scale-audit/tierA-warped` members are gone, verified). Pairs: re-dumped by the
  current `ln_scale_probe` (`--stars-dir`) from `tierA-lnB-f1` run 5's `registered/` members and ITS LN reference
  (`stackwork/LDN1272-test/ln/mono__NoFilter__bin1__180s/reference.fits`, md5 `a3fe9d0453d3f7be6b1ffedd1a7383f3` per
  `ln-real-plan.json`) and from `hc11B-f1` run 5's. The 2026-10-06 numbers were read against the `tierA-baseline` reference
  (0.4 % apart, memory `project_ln_scale_psf_bias_2026_10_04.md`'s gotcha), so they are printed beside, never a bar; the
  truth for the subset is the 30 members' rows of `lnB/truth/s204mono-lnBref.csv` (measured on run 5's own reference).
- **PB-6 The ERP's LN on the real sets (the owner's ERP YES).** Driver: the lab's `erp/run_job.sh` with the LN script
  `~/.athenaeum-acc/star-lab/erp/run_ln.js` (uncommitted, verified present; it reads `ln-job.json` beside it; the
  parameters are the owner's 2026-10-04 batch) in the `real/run_hc11.sh` pattern (it, `prep_nan0.py` and `run_ln.js` are uncommitted, dev-Mac-only): NaN → 0 copies per
  chunk of ≤ 5 targets (`prep_nan0.py`), deleted after the chunk, memory guard before every launch, one instance at a time, the owner's own
  interactive instance closed first. Inputs per dataset: the LN reference and the registered members `ln_real.py`'s
  `DATASETS` resolve (`starlab/ln_real.py::reference` / `members`; OSC per plane as the probe's VNG planes, the way
  `star-lab/erp/erp_v2.py ln` runs `osc_pairs`), exported by `bin/erp_ln_real.py export <ds>` into `~/.athenaeum-acc/star-lab/real/
  <ds>/in/`. The `scale` parameter: the value the 30-member set 204 run used, recovered from the owner's batch record
  (`/Volumes/Universe/Development/wbpp/logs/20261004090704_ProcessContainer.xpsm`, memory
  `reference_erp_headless_on_dev_mac.md`) or, if the share is unreachable, Horse C11 G's recorded 1597 with the reason
  logged; one value per dataset, recorded in the results. Output: `<ds>/erp-ln.csv` (`member,plane,scale`), copied small into
  `star-lab/results/plan-b/b1/erp-ln-<ds>.csv` with the frames' sha256 (the freshness link). Expected wall ≈ 4 s per
  26-Mpx target (Horse C11 G: 5 targets in 18 s, peak RSS 4.4–5.2 GB) → set 204 mono 91 + OSC 39 + run B mono 82 + OSC
  303 = 515 target planes ≈ 40 min machine time plus the copies.
- **PB-7 The forced-mode harness read (B2; R-B3).** `harness/acceptance/grid-report.py::photometry` gains
  `force_mode=None` (`"normal"` / `"crowded"`: the loop runs that mode only; `None` = today's order, output byte-identical,
  pinned by `tests/test_grid_report_force_mode.py`). The lab tool `bin/truth_modes.py` reads every cell of `pairs/main`,
  `pairs/iso` (the stored grid targets, one unit per target, the SE by the harness's own bootstrap), `density_pairs` and
  `truthcheck` (rendered realizations through `gates.pooled_truth`, K per target from the gates' `size_k`) in both modes,
  prints per cell `mode, n, ratio, s_true, gap %, SE`, and judges each cell against the bar of the mode that read it
  (±0.3 % normal, ±0.8 % crowded; R-B17); a cell a mode cannot read (< 10 stars) is "n/a (<n> usable stars)".
- **PB-8 Field realism's two quantities (B2; R-B5).** `annulusCrowding` per real member = `ln_annulus_crowding` on the
  f1 arm lines of B1's `ln_real` probe outputs (L-26's metric); per synthetic family the same field on its f1 arm lines
  in `star-lab-a/t14b/probe`. The halo `c` excess per real member = the kept `c − 1` of B0's run payload (F-11); per
  synthetic family the probe's `apertureCorrection − 1` on the target frames (measure path, `t14b/probe`). Medians are
  compared, every per-target-FWHM value printed; a family differs when its median lies outside the real members'
  min…max over every real member B1 reads (R-B13). Both quantities on the same table; the verdict needs both.
- **PB-9 The realistic-field family, if built (B2; R-B5).** `synth.py` / `make_grid.py` gain `--halo-fixed-px <w>` (a
  β-2 Moffat halo of a FIXED width in pixels, its flux fraction sized so the family's median `c − 1` lands at the real
  members' median excess), `--crowding-target <x>` (the star density iterated until the f1 arm's `annulusCrowding` median
  lands inside the real range), the real seeing range (targets at 2.64 / 3.0 / 3.4 / 4.0 / 5.0 / 5.92 px against a 3.385 px
  reference), `ref_nframes 20` (a reference stacked from the best frames, as the `*mix` families), the `main` luminosity
  function (fmin 0.05, slope 0.8), cosmic rays and hot pixels (f1v2 item 9's generator, seeded). One profile (`m4`); data
  `~/.athenaeum-acc/star-lab/grid/real/m4/`; its ERP LN run once through `erp_v2.py ln` (a new grid family is new frames);
  s_true a generator input. Built ONLY if PB-8's table finds no pair family inside the real range on both quantities.
- **PB-10 The seeing-lever injected pairs (B2; F-8 referee).** `starlab/inject.py` gains `--stamp empirical`: the
  injected star's stamp is the frame's own empirical PSF — the normalized median stack of 50 isolated unsaturated stars
  sampled ACROSS brightness (never the brightest: memory `feedback_bright_stars_not_representative.md`) in a window of at
  least `ceil(8·F)` half-width (`c`'s 5F growth radius and F1's [6F, 8F] annulus lie inside it; `_fwhm_pass` gives F), its
  rim test rejecting a neighbour only, never a halo (the wing is what the pairs test), sub-pixel shifted to each
  injection position by bilinear interpolation — not a Moffat. The stamp's own 5F growth / model ratio is printed beside
  the frame's kept `c` (B0) as the realism check. Six pairs from the set 204 mono tree (`tierA-lnB-f1` run 5):
  reference = its LN reference; targets = the registered members nearest FWHM 2.64, 3.0, 3.4, 4.0, 5.0 and 5.92 px (from
  `ln-real-members.csv`), each carrying 300 injected stars at `s_inj 0.8` on the `main` luminosity function; truth = the
  injected list; the injected stars' own aperture sum over the injection printed (the check; today 0.95–1.03). Manifest
  `manifests/inject-lever.json`; scored by `rows_ln_injected`; their one ERP LN run (new frames) on the dev Mac (the source flat and dark live on `/Volumes/bigbase3`: PB-17).
- **PB-11 The floor cases (B4; F-9).** (i) Crops of the set 204 OSC CALIBRATED planes in `tierA-lnB-f1` (LDN1272; the raw 0004 / 0007 / 0013 on
  `/Volumes/bigbase2` are the 2024 Markarian lights, another dataset, kept for B4's × 0.40 case only; debayered by
  `star_probe debayer --pattern RGGB` if the tree holds them mosaiced, then cropped: 600 × 400, 400 × 300, 300 × 200 at four corners and the
  centre) and sparse lab fields (`density/d200` cropped the same): a case counts when the `"fewer than 5 isolated
  unsaturated stars"` warn fired; the probe gains `--dump-ratios` (the 1–4 growth / model ratios, a diagnostics field on
  `examples/star_probe.rs`, never the product) so arm 2 (the plane's own median over its 1–4 ratios) and arm 3 (the median
  kept `c` of the same channel over the group's other members, read from B0's payloads of the uncropped planes) are
  computed offline by `bin/floor_cases.py`; arm 1 is today's `c = 1`. The reference for a crop is the uncropped plane's
  `c` (≥ 5 qualifiers, kept by F-11); for a lab crop the truth (row 6's captured fraction). (ii) Crops (1500 × 1000,
  600 × 400, 300 × 200, 200 × 150), 2 × 2 binned copies and masked-overlap corners (the run B masked tree) of registered
  pairs from set 204 mono and run B mono, both sides cut the same, run through `ln_scale_probe --member` on the pair: a
  case counts when `ln_growth_pairs` 1–4 or the 0-pair warn fired with ≥ 20 matched pairs; arm "factor kept" = the
  probe's `s`, arm "factor 1" = `s / ln_growth_factor` (offline); the reference = the uncropped pair's F1 `s`. Synthetic
  (ii) cases from `pairs/main` targets cropped the same, truth s_true. The verdict needs ≥ 8 counted cases per floor over
  ≥ 2 datasets (R-B16); the per-case table prints the floored plane's weight relative to its group and its psfSel `s`
  (for (i)). Every new floor-(ii) frame that is a stored pair gets its one ERP LN run on the dev Mac (new frames).
- **PB-12 The point-source screen (B4; F-12) — REPLACED after the plan review (C-1).** The first design (fitted FWHM <
  0.715 · F_field) refuses none of raw 0013 p0's 36 hot pixels (the line is 1.090 px at the 1.524 floor; the 36 fit at
  1.124–1.348) and refuses 6.4–17.6 % of unflagged real fits on the raw planes. New screen: on a DEBAYERED multi-plane
  frame a `c` candidate with no fit on any sibling plane within 1.5 px is not a point source (the module's `crossFlags`
  logic; on the existing dumps G 0 of 36 hot pixels vs 21 of 21 real stars have a counterpart, B 0 of 36 vs 20 of 21).
  Mono planes: no screen (the cosmetic map owns hot pixels). The 6a neighbour share (`seeds.rs:674–700`) already runs and
  passes all 36, so it is no fallback. Counted in `skipped` under its own counter `point_source` (the per-plane debug
  line, `PlaneMeasurement`, the probe's `pointSourceScreened`; no `RefusalReason`; the dictionary gains the name, B4's
  file list). Judged FIRST on the lab truth stars of `osc/*` and `osc_pairs/*` (a refused real star counted per family,
  bar ≤ 1 % of qualifiers) in B4 Step 0b, before it enters code.
- **PB-13 The brightness-stratified sample (B4; F-12) — WITHDRAWN as a fixed ruling (I-1, I-2).** The 4 × 16 SNR ≥ 20
  sample moved nebula `c` +2.43 % / +23.16 % (neb0.5 / neb3) where no B4 bar reads, invented a second per-star SNR beside
  F-6's `StarFit.snr` and revisited R13b-6 uncited. The sample is settled by B4 Step 0b into the controller's ruling
  PB-13'; the per-tercile ratio report, `CORRECTION_STARS` 64 and `CORRECTION_MIN_RATIOS` 5 stand unless PB-13' says
  otherwise. The row-6 STOP keeps ONE fix round (the runner-up sample PB-13' names); a second miss goes to the owner.
- **PB-14 `MEASURE_VERSION` 5 lands in B4** (R-B8; one bump for Plan B), the constant's history comment extended; the
  README's policy line (`docs/stacking/README.md` l.1872 "Plan C … is 5") becomes "Plan B 5, Plan C 6"
  (`star_measure/mod.rs` already says Plan C = 6); the three stage-hash pins B0 added move to version 5; the release note names the
  once-only re-measure of every set.
- **PB-15 Master-level comparisons and what a clone keeps.** Masters: `tier1/tier1-compare.py --numeric` (median / MAD /
  noise / FWHM rows, `--base-db`, every catalog `immutable=1`); weights: `tier1/t13-tools.py weights` (Spearman ρ and the
  rank movers); the LN scale against the validated truth: `bin/ln_truth_run.py` (new: for a clone's run it resolves the
  run's LN reference and registered members, runs `grid-report.photometry` per member and plane with the mode recorded,
  writes `<ds>-truth.csv` with the reference md5 and the file's sha256 — the truth is ALWAYS re-read on the run's own LN
  reference, since `build_and_write_ln_reference` integrates the top-N by weight and a weight change moves its composition, R13-4)
  and then `ln_real.py`'s table on that truth (`--truth` override), the re-based row (R13-4) printed beside. A clone
  therefore keeps `athenaeum.db`, `logs/`, `stackout/`, `stackwork/*/{registered,ln}` until the comparison that reads it
  has committed its results, and drops `stackwork/*/{calibrated,measured,fits,rej,previews}` right after its own results
  commit (`planb.sh --strip-light <clone>`, a new mode beside `t13c.sh --strip`): B0's clones live until B4's after-run
  comparisons are committed, B4's until B3's, B3's until B6's. Peak new bytes: one dataset's run (set 204 ≈ 46 GB, run B
  ≈ 38 GB, Horse C11 ≈ 26 GB) beside the three light clones (registered + ln ≈ the raw size: ≈ 40 + 35 + 20 GB), under the
  440 GiB free of 2026-10-20 with the 100 GiB floor.
- **PB-16 Data roots and the compile slot.** Plan B's probe runs and scorecards go to `~/.athenaeum-acc/star-lab-b/<step>/`
  on whichever machine runs the step (`b0-ident`, `b1-synth`, `b2-truth`, `b4-c`, `b3-est`, `b5-seeds`, `b6-final`; at
  most two step directories at a time, a step's probe directory deleted once its scorecard is gzipped under
  `star-lab-b/results/<step>/`); clones to `~/.athenaeum-acc/planb-<ds>-<stage>/`. The calibration-geometry cycle
  (worktree `.claude/worktrees/calib-geometry`, target `.superpowers/target-geom`) compiles on the dev Mac in the coming
  days: the dev Mac is kept for Plan B's SHORT builds (the three probes and `athenaeum-web` into `target-acc`, one build
  per stage that changes code: B0, B4, B3 Step 2b / 4, B5) and the suites; each cargo invocation runs only when
  `pgrep -fl 'rustc|cargo'` is empty and `memory_pressure` ≥ 30 % free, waiting up to 20 min then STOP (the
  `run_probe.py` guard's rule, added to `planb.sh build` and used by `lab_cycle.py --wait-quiet`); the two cycles share
  no target directory, but RAM (16 GB) and CPU are shared, so ONE heavy-job lock covers both cycles (I-8):
  `~/.athenaeum-acc/HEAVY.lock` (`shlock` / `flock`, holder and start time written in it), taken by every cargo build or
  test of either cycle and every long run or ERP job; long runs are scheduled overnight in `progress.md`'s slot table.
  The code reaches the runner as a git bundle, no push (`/Volumes/Universe/athenaeum-runner/git` bundles + the lab head on GitLab; the runner builds its own `target` from the
  same commit — the certified bit-for-bit parity of 2026-10-10 — and `--expect-sha` is checked against the runner's own
  build, never the dev Mac's). An ERP instance never runs beside a compile on the same machine.
- **PB-17 The machine plan (C-2; rulings.md PB-19, settled 2026-10-20 from the runner's own answers and the owner's word).**
  Facts: the runner (M4 Max, 128 GB, agent `bridge:session_01CCQHmF9YhQtpfqymXJQ98i`) has 205 GiB free, the ERP, no
  acceptance tree (the three are 95 + 69 + 52 = 216 GB), `star-lab/grid/{iso,main}`, `star-lab-v2/{density_pairs,
  truthcheck,osc_pairs,erp}`, `star-lab-inject`, `star-lab-mono2` and `star-lab-l` (the nebula-sky pairs, only there); it
  never sees `/Volumes/bigbase2` / `bigbase3`; Universe is mounted on BOTH machines. The trees' lights link masters on
  bigbase3 (the stacking plan gate turns a missing master into a rebuild or a `masterFiles` blocker), so no master-level
  run can run on the runner, even with the trees copied.
  DEV MAC: every tree- and volume-reading step — the master-level runs of B0 → B4 → B3 → B5 (a before / after pair never
  splits), `ln_real`, B1's re-dump, B4's floor-(i) / (ii) crops and × 0.40 case, B2's lever inject, the ERP real-set,
  injected-pair, lever and floor-(ii) runs (the ERP is installed there, as the spec says), the gates (all 13 on a
  clone: `injection` and `gain` read the raw sources on bigbase2 / 3), the builds (PB-16) and the suites.
  RUNNER: the synthetic lab cycles only (`lab_cycle.py` incl. `--inject`, whose probe reads only `star-lab-inject`; the
  `truth_modes.py` read, the generator runs, the l10b cycle) and the ERP on synthetic frames (the nebula-sky pairs, the
  realistic family), after `star-lab-inject` (5.7 GB) and `star-lab-a/t14b/{probe,inject-probe}` are copied to it over
  Universe (B0 Step 0a).
  Driving: the controller alone writes a one-page handoff `/Volumes/Universe/athenaeum-runner/handoffs/planb-<step>.md`
  (one command, the fixed answers or the judgment asked, the hard STOP, the result paths) and relays it as
  `SendMessage "bridge:session_<id>"` (the id changes: ask); results come back as small files committed to athenaeum-lab
  plus a ≤ 30-line verdict on the share. The Windows runner has no Python (memory `reference_runner_windows.md`) and is
  not used.
  Wall estimate (I-8): dev Mac heavy ≈ 35–45 h (four stacking runs ≈ 6–8 h per stage × B0 / B4 / B3 / B5, `ln_real` ×3
  ≈ 45 min each, the ERP real sets ≈ 40 min plus the injected, lever and floor-(ii) pairs, the gates, the floor cases),
  runner ≈ 8–10 h (the cycles of B0 / B4 / B5 / B6, the both-modes read, l10b); `progress.md` carries the slot table.
- **PB-18 Owner STOP packages.** Every owner decision (the table below) is delivered as a ≤ 40-line plain-language note
  in Russian (memory `user_russian_dialogue.md`; the tables stay English) with the numbers, the recommended answer first
  (memory `feedback_notify_on_owner_question.md`: PushNotification + AskUserQuestion), and the execution continues on
  the stages that do not depend on it (B4's floor verdicts do not block B3's lab reads, only its product change).

## Data and machines (verified 2026-10-20 on the dev Mac)

| Item | Verified state | Used by |
| ---- | ---- | ---- |
| The ERP | installed on the dev Mac (the lab's `erp/run_job.sh` launches it headless; `~/.athenaeum-acc/star-lab/erp/run_ln.js` + `ln-job.json` present, uncommitted) and on the runner | every ERP LN run on a real or injected frame (dev Mac); synthetic frames (nebula-sky pairs, the realistic family): runner (PB-17) |
| `/Volumes/BigMac` | 440 GiB free; memory 75 % free | every dev-Mac step (floor 100 GiB) |
| `/Volumes/bigbase2` | mounted, 1187 GiB free (the 2024 Markarian raw OSC frames 0004 / 0007 / 0013; the runner never sees it) | B4's × 0.40 case (F-12), the gates |
| `/Volumes/bigbase3` | mounted, 6989 GiB free (the inject masters `manifests/inject.json` names, the trees' calibration masters; the runner never sees it) | B2's lever inject, the inject-OSC reads, the master-level runs, the gates |
| `~/.athenaeum-acc/tierA-lnB-f1` (run 5), `lnB-runB-f1` (run 6, project `_projects/ldn1272-test-project`), `hc11B-f1` (run 5) | present with `stackwork/*/{registered,ln,fits,calibrated,runs}` | B0 / B4 / B3 clones; B1 `ln_real`; B2 lever pairs |
| `~/.athenaeum-acc/lnB/truth/{s204mono,s204osc}-lnBref.csv` | present | B1, B2 |
| `ln-scale-audit/runB-truth.csv`, `ln-model-fixes-t16/hc11G-truth.csv`, `erp-ln-s204/`, `ln-scale-audit/tierA-warped`, `tierA-baseline`, `starA-s204` | ABSENT | B1 rebuilds the two CSVs (R-B10); PB-5 for the port's inputs |
| `star-lab/results/probe-runs-2026-10-07/{ln-real-members.csv,ln-real-plan.json,ln-real-summary.txt,batches.txt}` | present in the lab | B1 (R-B10) |
| `~/.athenaeum-acc/star-lab-a/t14b/{probe,inject-probe}` (2.9 GB), `star-lab-a/results/t14-final/scorecard.json.gz` | present; `star-lab-a/t14-final/` holds logs only | B0 identity, B1 synthetic (PB-3, PB-4) |
| `~/.athenaeum-acc/star-lab-v2/erp/**/ln.csv` (24) | present | B1 ERP column + freshness audit |
| `~/.athenaeum-acc/star-lab/grid/{iso,main}`, `star-lab-v2/{density_pairs,truthcheck,osc_pairs}` | present | B2 referee, B3 rows |
| `~/.athenaeum-acc/star-lab-inject`, `star-lab-mono2` | present | B4 inject row 6, the × 0.40 / inject-OSC reads (the runner holds copies of both) |
| `~/.athenaeum-acc/star-lab-l` (nebula-sky pairs) | not on this Mac; the runner only | B1, B5 (runner) |
| `star-lab/f1v2/{f1core.py,log-real-s204-bauto.txt (31 lines),log-real-hc11G-bauto.txt}` | present in the lab | B1 F1 v2 step |
| `.superpowers/target-acc` probe binaries | present; sha256 checked against `results/plan-a/final/provenance.txt` before B1 uses them | B1 |
| Worktree `.claude/worktrees/calib-geometry`, target `.superpowers/target-geom` | present (the parallel cycle) | PB-16 |
| The M4 Max runner (its answers of 2026-10-20) | 205 GiB free; the ERP; no tree; the synthetic lab data above; `star-lab-l`; Universe mounted on both machines | PB-17 |

**Runner facts.** The six questions were answered by the runner on 2026-10-20 (rulings.md PB-19) and are folded into
PB-17; B0 Step 0a only verifies the mounts and copies the synthetic data the runner's cycles need.
## File structure

athenaeum (`crates/athenaeum-core/src/` unless noted):

| File | Responsibility (task) |
| ---- | ---- |
| `stacking/measure.rs` (hand edits) | `ChannelMeasurement` + the three `c` `Option`s, copied at the channel build (B0); nothing else |
| `stacking/run.rs` (hand edits) | the per-(group, channel) `info!` in `measure_group_finish` (B0); `set_ln_summary` carrying the LN kept values (B0) |
| `stacking/ln/mod.rs`, `stacking/ln/scale.rs` (hand edits) | `LnFrameOutcome` / the scale event struct expose `growth_factor`, `growth_pairs`, the target's `aperture_correction` (B0); module seeds on both sides, `k` and `unverdicted_clip_range` retired (B5, if in scope) |
| `stacking/provenance.rs` | `SummaryFrame` / `SummaryGroup` additive LN fields (B0) |
| `star_measure/photometry.rs` | the point-source screen, the stratified sample, the tercile report (B4); `ring_sky` caller F + union mask (B3, only if F1 v2 wins) |
| `star_measure/mod.rs` | `MEASURE_VERSION = 5` with its history line (B4); `PlaneMeasurement.point_source_screened` (B4) |
| `stacking/config.rs` | the three fit-reading stage-hash pins at version 4 (B0, tests only); moved to version 5 (B4) |
| `stacking/integrate.rs` | `LocalNormalizationConfig::default` and the enums' `#[default]` (B3, only on the owner's word) |
| `stacking/ln/selection.rs`, `stacking/ln/calibration.rs` | the one saturation definition (the module's flag); `k` gone (B5, if in scope) |
| `examples/star_probe.rs`, `examples/ln_scale_probe.rs` | `--dump-ratios` (B4), `pointSourceScreened` in the diagnostics output (B4); unchanged otherwise |
| `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` | the dictionary entry for `aperture_correction_min` / `_max` and the `planes` / `count` senses on the new info (B0); `point_source` (B4) |
| `docs/stacking/README.md` | R-OSP-16: every Plan B ruling and acceptance run (each task appends its block; B6 closes it); the version policy line (B4) |
| `docs/superpowers/open-items.md` | the `c` entry under "Unverified by hand" (l.281) resolved by B0 / B4; the "F1 on small or edge-heavy frames" entry (l.317) closed by B4's F-9 verdict |
| `src/types/stacking.ts` (generated) | the `SummaryFrame` / `SummaryGroup` fields (B0) |
| `RELEASE_NOTES.md` | the once-only re-measure note (B4) |

athenaeum-lab (`star-lab/` unless noted):

| File | Responsibility (task) |
| ---- | ---- |
| `starlab/scorecard.py` (`rows_ln`, `rows_ln_injected`) | the ERP beside EVERY arm (B1) |
| `bin/erp_audit.py` | the LN freshness audit: each `ln.csv` linked by sha256 to the frames it read, like the star units (B1) |
| `bin/erp_ln_real.py` (new) | `export <ds>` / `run <ds>` / `collect <ds>` for the real sets and the injected pairs over `erp/run_job.sh` (B1; reused by B2 / B4 for new frames) |
| `bin/ln_truth_rebuild.py` (new) | `runB-truth.csv`, `hc11G-truth.csv` from `ln-real-members.csv`, sha256 recorded; every reference md5 checked against `ln-real-plan.json` (B1, R-B10) |
| `bin/ln_truth_run.py` (new) | the harness truth re-read on a clone's run (its LN reference, its registered members, per plane), the mode recorded (B0 records; B4 / B3 / B6 judge) |
| `bin/c_identity.py` (new) | the kept `c` of a run's `metrics` payloads against its debug lines, plane by plane (B0) |
| `bin/truth_modes.py` (new); `harness/acceptance/grid-report.py` (`force_mode`) | the referee in both modes on every cell (B2) |
| `bin/realism_table.py` (new) | `annulusCrowding` and the halo `c` excess, synthetic families vs real members (B2) |
| `synth.py`, `make_grid.py` | the realistic-field family's generator flags (B2, only if needed) |
| `starlab/inject.py`, `bin/inject.py`, `manifests/inject-lever.json` (new) | `--stamp empirical`, the six seeing-lever pairs (B2) |
| `bin/floor_cases.py` (new) | the (i) / (ii) floor cases, their arms, the bootstrap verdict (B4) |
| `bin/b3_table.py` (new) | every candidate on the same rows, the ERP beside, the bars per cell (B3) |
| `f1v2/real_read_b1.py` (new) | the port on the re-dumped pairs, the 2026-10-06 numbers beside (B1) |
| `bin/lab_cycle.py` | `--wait-quiet` before the build (B0, PB-16) |
| `harness/acceptance/planb.sh` (new), `harness/acceptance/tier1/planb-tools.py` (new) | one command per job: `clone`, `run`, `truth`, `compare`, `strip-light`, `bars` per stage; the resource guards; the results-only rule |
| `results/plan-b/<stage>/` | every verdict, table, run-log and ruling file; the small CSVs |

## Execution format

- One phase per agent; the handoff `.superpowers/sdd/2026-10-20-plan-b/task-<stage>-handoff-r<K>.md` (one page: state,
  commits, rulings, the exact next steps, the tool-call budget), the report `task-<stage>-report-r<K>.md` (≤ 80 lines:
  Status, Commits, Gates, Bars table, STOP causes, Proposals, Concerns). The ledger is `progress.md` there.
- Models: lab tool code and Rust code → `sonnet` (`rust-engineer` for Rust); proof runs with fixed answers → `haiku`
  (exact commands, a hard STOP, no destructive step — the controller deletes beforehand); judgment lab runs and every
  reading of real data → `sonnet`; the numerics that decide (B2's realism verdict, B4's verdict tables, B3's table) →
  `opus`; task review → `opus`; the final whole-branch review → Fable. The controller re-gates every task independently
  (re-runs the verdict command, reads the bars itself) and restarts its session at every stage boundary.
- Each stage's lab cycle is ONE command (`planb.sh <stage> <job>` or `lab_cycle.py`), fix rounds use a quick manifest,
  the full manifest once per task.
- Runner steps (PB-17): the controller alone talks to the runner's agent; an implementer or lab agent of this plan never
  contacts it. A runner handoff is one page on the share (`/Volumes/Universe/athenaeum-runner/handoffs/planb-<step>.md`:
  the lab head to pull, the athenaeum git bundle and commit to build, the ONE command, the fixed answers or the judgment asked, the
  hard STOP, the result paths, the cleanup — the results-only rule applies there too), the verdict comes back as
  `…/handoffs/planb-<step>-verdict.txt` (≤ 30 lines) and as the lab commit; the controller re-gates from the committed
  files. The dev Mac's and the runner's builds of one commit are interchangeable by the certified parity (2026-10-10),
  and every runner `--expect-sha` names the runner's own binaries.

## Owner STOP points

| When | What the owner decides | What the owner gets |
| ---- | ---- | ---- |
| B0 done | nothing (informed; R-B2 rules the placement) | the four runs' reference picks, the `c` table per group and channel (min / median / max, planes at floor (i)) |
| B1 done | nothing (informed with the baseline) | the synthetic and real LN tables with the ERP beside every arm, the freshness audit, the `f1_gc` ↔ `.athln` parity line |
| B2, the referee | the referee rule is the owner's: a harness-truth fix (a growth-corrected crowded mode) is REPORTED with a recommended answer, and B3's real read waits for the owner's word on it; the field-realism verdict informed | the both-modes table with every cell's gap, the realism table, the lever-pairs row |
| B4, F-9 (i) and (ii) | each floor's verdict: keep today's behaviour, or the arm the numbers favour (the product changes only on the owner's word; R-B1) | the per-case table, the paired difference with its bootstrap SE, "indistinguishable" / "too few cases" / "not reached" as the numbers say, the floored planes' weights |
| B4, F-12 STOP (if it fires twice) | the sample's design beyond PB-13''s one fix round | both row-6 tables, the tercile ratios, the refusals per family |
| B3 | the default LN estimator (L-24 was the owner's): F1 as is, psfSel on B4's `c`, or the F1 v2 sky if it entered | the candidates on the same rows, synthetic and real, the ERP beside, every bar per cell, the master-level rows; keeping F1 changes nothing in the product |
| B5 | R11-3's scope, if B5 names an owner | the pulls and the proposed owner with its bar |
| B6 | the merge into local main, the push, the close | the after report beside B1's baseline, every Plan B bar as judged, the suites' line |

---

### Task B0: `c` kept per plane and per LN side (spec §3 F-11 l.293; bars l.480–486; §5 B0 l.707–712; R-B2, R-B13, R-B15, R-B16)

**Why.** `c` moves the weights (∝ c², derived) and psfSel's s one to one, yet on a real run it is neither persisted nor
logged at a kept level; every later stage reads it (B2's realism, B4's arm 3, B3's judgement), so it is kept first, with
no change to any measured value. B0 also stacks the four master-level groups on fresh work dirs: the before-runs for B4.

**Coupling checklist.** Upstream quantities this task reads and must not re-estimate: `c` (owner Task 10 / F-5; B0 only
copies it), its SE and count (Task 13b), F1's growth factor and pairs (the LN cycle, I-2). Earlier rulings in play:
R-OSP-7 (floor (i): `c = None` under 5 ratios, one `warn!`), PA-4 / R-OSP-1 (`MEASURE_VERSION` stays 4; the payload is
not in the hash), R13-4 (the LN reference is not pinned by the registration reference), R-B15 (fresh work dirs).
Propagation: the kept `c` feeds B2's halo excess, B4's arm 3 and floor count, B3's reading, B6's report.

**Files.**
- Modify (hand edits): `crates/athenaeum-core/src/stacking/measure.rs` (`ChannelMeasurement` l.181–222: three fields
  after `field_width_at_floor`; the channel build at l.1123: copy `m.aperture_correction`, `m.aperture_se`,
  `Some(m.aperture_stars)` when `m.aperture_correction.is_some()` else `None` for all three), `stacking/run.rs`
  (`measure_group_finish` l.5278: the info line per channel; `set_ln_summary` l.12224: three more parameters),
  `stacking/ln/scale.rs` (the scale event struct `e` at l.834–846 already holds `growth_pairs`, `growth_factor`; expose
  them and `tgt_outcome.aperture_correction` on the returned sample), `stacking/ln/mod.rs` (`LnFrameOutcome` gains
  `growth_factor: Option<f64>`, `growth_pairs: Option<usize>`, `target_aperture_correction: Option<f64>`).
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (tests only: the I-5 pins), `crates/athenaeum-core/src/stacking/provenance.rs` (`SummaryFrame`: `ln_growth_factor`, `ln_growth_pairs`,
  `ln_aperture_correction`; `SummaryGroup`: `ln_reference_aperture_correction`), the logging spec's dictionary (one
  extension paragraph), `src/types/stacking.ts` (regenerated by the existing `ts_export` test / command), `docs/stacking/
  README.md` (the R-OSP-16 block opened), `docs/superpowers/open-items.md` l.281–290 (the `c` entry: "(a) logged / kept"
  resolved, (b)–(d) now B4's).
- Lab: `bin/c_identity.py`, `bin/ln_truth_run.py`, `bin/lab_cycle.py --wait-quiet`, `harness/acceptance/planb.sh`
  (`build`, `clone`, `run`, `truth`, `strip-light`), `harness/acceptance/tier1/planb-tools.py` (`reference-pick`, `c-table`,
  `run-log-count` reuse), `tests/test_planb_tools.py`, `tests/test_c_identity.py`, `tests/test_ln_truth_run.py`.

**Interfaces.**
- Produces: `ChannelMeasurement { aperture_correction: Option<f64>, aperture_se: Option<f64>, aperture_stars:
  Option<usize>, … }` (payload keys `apertureCorrection`, `apertureSe`, `apertureStars`); `SummaryFrame { ln_growth_factor:
  Option<f64>, ln_growth_pairs: Option<usize>, ln_aperture_correction: Option<f64>, … }`; `SummaryGroup {
  ln_reference_aperture_correction: Option<Vec<Option<f64>>>, … }`; the info `"measure: aperture correction resolved per channel"`
  with the PB-1 fields; `planb.sh run <ds> <stage>` writing `~/.athenaeum-acc/planb-<ds>-<stage>/` and
  `results/plan-b/<stage>/<ds>/{run.txt,reference-pick.txt,c-table.txt,verdict.txt}`; `ln_truth_run.py <clone> <ds>
  --out <csv>` (columns `member,plane,ratio,se,n,mode,ref_md5`).
- Consumed by: B2 (`c-table.txt`, the payloads), B4 (arm 3, the before-runs, `reference-pick.txt`), B3, B6.

**Bars (spec l.480–486, verbatim).**
- "Identity: probe outputs identical (`run_probe --verify`, `probe_parity`), every `.athf` byte-identical, every stage
  hash unchanged, an old payload decodes unchanged (the new fields None). The kept values equal the debug `"aperture
  correction measured"` values plane by plane on one set 204 run and one Horse C11 run. The per-(group, channel) `info!`
  uses the dictionary's names (`planes`, reused in the Measure sense, the dictionary entry noting the reuse; `count`;
  `aperture_correction`) and adds only a min / max pair in the `beta_own_min` / `beta_own_max` form, entered in the
  logging spec's dictionary in B0's change (R-B13)." — judged by the chain PB-3 (a)–(d).
- §5 B0 (l.707–712): "the four master-level groups (set 204 mono and OSC, run B, Horse C11) stacked on acceptance
  working copies, each re-measured on a fresh work dir (R-B15 …), each run's reference pick recorded: these are the
  before-runs for B4 (R-B16). Data: dev Mac, `~/.athenaeum-acc` (the three trees); no ERP. Bar: F-11's identity."
- No-change guards (l.393–402) bind B0: the LN rows of the lab cycle `changed 0`; the four runs are B4's before, so they
  carry no bar of their own beyond the identity and the staleness proof.

**Steps.**
- [x] **Step 0a — the runner's data and the mounts (controller, relayed; ≈ 30 min).** The six runner questions are
  answered (PB-17). Verify `mount | grep Universe` on both machines; copy over Universe (`rsync --inplace`) to the
  runner `star-lab-inject` (5.7 GB), `star-lab-a/t14b/{probe,inject-probe}` (2.9 GB) and
  `star-lab-a/results/t14-final/scorecard.json.gz`; hand it the git bundle of `plan-b` and the lab head (no push). The
  nebula-sky ERP LN runs through `erp_v2.py` (B1 Step 0). Record in `progress.md` and README R-OSP-16's B0 block.
- [x] **Step 0 — lab tools first (lab code; sonnet; ≈ 2.5 h; no build, no run).** `planb.sh` (`build` with the PB-16 wait;
  `clone <ds> <stage>` = `clone-before.sh <tree> ~/.athenaeum-acc/planb-<ds>-<stage>` + the stacking config check through
  `t13-tools.py before` / `make-config` with the reference mode left as stored (Auto for B0) and `ATH_ACC_RERUN_FROM=
  measure`; `run` = `tier1-run.sh` / `project-run.sh` with `ATH_ACC_LOG_EXTRA=athenaeum_core::star_measure=debug`, then
  `run_log_count.py` for `frame plane measured` = the included planes (the staleness proof: no cached payload reused),
  `planb-tools.py reference-pick` (the run row's `reference_frame_id`, `reference_mode`, the LN reference's md5 per group
  from `stackwork/*/ln/*/reference.fits`) and `c-table` (per group and channel from the `metrics` payloads: planes,
  min / median / max, planes at floor); `truth` = `ln_truth_run.py`; `strip-light`). `c_identity.py` (joins
  `stacking_artifacts` `metrics` payloads to the run's `"aperture correction measured"` lines by (frame, plane); prints
  `planes N equal N differ 0` and exit 1 on any differ). `ln_truth_run.py` (PB-15). Tests in full: `test_planb_tools.py`
  (a fixture DB with two runs: the pick and the table), `test_c_identity.py` (a payload and a log line that agree, one
  that differs → exit 1), `test_ln_truth_run.py` (a synthetic reference / member pair through `grid_report.photometry`
  gives ratio 0.8 ± SE, mode recorded; a missing registered file is named). Commit lab: `feat(planb): B0 tools - the
  stage driver, the c identity join, the truth re-read on a run's own LN reference`; push `origin main`.
- [x] **Step 1 — the failing tests (rust-engineer sonnet; ≈ 1 h).** In `stacking/measure.rs`'s tests:
  `channel_c_fields_round_trip_and_an_old_payload_decodes` (a `ChannelMeasurement` with `aperture_correction: Some(1.17)`,
  `aperture_se: Some(0.004)`, `aperture_stars: Some(64)` serializes with the three camelCase keys and round-trips; the
  string `{"starsDetected":5,"starsFitted":3}` decodes with all three `None`), `channel_without_c_serializes_as_before`
  (the three `None` → the JSON holds none of the three keys; the JSON equals the pre-B0 string of the same struct, a
  pin). In `stacking/config.rs`'s tests: `fit_reading_stage_hashes_at_measure_version_four_are_pinned` (the three
  fit-reading stage hashes of the default config, literals read off today's code once; a pin outside the RED step: the
  existing test there is only relative, so B4's bump needs this one to move; I-5). In `stacking/run.rs`'s tests (or `provenance.rs`): `summary_frame_ln_fields_default_none_and_round_trip`,
  `ln_summary_keeps_growth_pairs_and_factor_for_a_zero_pair_member` (a `LnFrameOutcome` with `growth_pairs: Some(0)`,
  `growth_factor: Some(1.0)` lands on the frame row). `group_c_summary_with_every_plane_at_floor_reports_zero_planes`
  (the summary function over members whose `c` are all `None` returns `planes 0`, `count n`, no median — a pure function
  `measure::channel_c_summary(&[Option<(f64, f64, usize)>]) -> ChannelCSummary { planes, count, median: Option<f64>,
  min: Option<f64>, max: Option<f64> }`, so it is testable without a run). Run: `cargo test -p athenaeum-core --lib --
  stacking::measure --test-threads=4 | grep -E '^test result|FAILED|panicked'` → the new tests (the pin excepted) FAIL (fields missing).
- [x] **Step 2 — the code (rust-engineer sonnet; ≈ 3 h).** The fields, the copy at the channel build, `channel_c_summary`,
  the info line in `measure_group_finish` (one per channel: `plane = p`, `planes`, `count`, `aperture_correction` (the
  median, only when `Some`), `aperture_correction_min`, `aperture_correction_max`), the LN plumbing (`scale.rs` → `mod.rs`
  → `run.rs::set_ln_summary(rc, group, frame, scale, cached, growth_factor, growth_pairs, target_c)`; the reference's `c`
  per plane from `build_and_write_ln_reference`'s prepared reference outcome onto the group row), `provenance.rs`, the TS regen,
  the logging dictionary paragraph (the two new names; `planes` and `count` in their B0 senses on this one event). Hand
  edits only in the never-rustfmt files. Run the Step 1 tests → PASS; `cargo test -p athenaeum-core --lib -- stacking
  --test-threads=4` green; `cargo test -p athenaeum-core --lib -- star_measure --test-threads=4` green. Commit athenaeum
  (`plan-b`): `feat(measure): Plan B B0 - c, its SE and count kept per plane (additive), one info per group and
  channel; LN's growth factor, pairs and per-side c on the run summary`.
- [x] **Step 3 — review (opus; ≈ 1 h)** of `git diff 068cf2d6..HEAD -- crates/ src/ docs/` against this task, PB-1, PB-2
  and the logging rule; fixes by a fresh sonnet agent with a handoff; the reviewer's re-read.
- [x] **Step 4 — the lab identity (haiku, fixed answers; ≈ 1.5 h; the RUNNER, PB-17; the handoff relayed by the
  controller; the dev Mac only builds).** On the dev Mac `planb.sh build` (waits for a quiet machine; builds the three
  probes release into `target-acc`; records the sha256s — the dev Mac's proof that the commit builds); on the runner the
  same commit built into its own target (`--expect-sha` = the runner's binaries), then
  `python3 bin/lab_cycle.py --step b0-ident --prev t14-final --inject --expect-sha <SP>,<LN> --bars none` and
  `probe_identity.py ~/.athenaeum-acc/star-lab-b/b0-ident/probe ~/.athenaeum-acc/star-lab-a/t14b/probe --drop-key ms`,
  the same for `inject-probe`. Fixed answers: identity `files same N diff 0 noref 0` on both; noreg vs t14-final `0 moved,
  max |Δ| 0`, the LN rows `changed 0`; the I-5 stage-hash pins unchanged (the suite's Step 2 pass). Any other
  value: STOP, no re-run, report the line.
  As it ran (R-PB0-10..12; the runner, steps `b0-ident` at 4566d628 and `b0-ident2` at 20697493): `heavy_lock.py run --holder <who> -- python3
  bin/lab_cycle.py --step b0-ident --prev t14-final --inject --wait-quiet --design <lab>/star-lab/results/plan-b/b0/design.json` (no `--bars`, no
  `--expect-sha`; design `[]`), then `probe_identity.py $A/b0-ident/{probe,inject-probe} $A/t14b/{probe,inject-probe} --drop-key ms --drop-path
  channel.apertureCorrection,channel.apertureSe,channel.apertureStars,options.analysis.batch_concurrency --check-channel-c` (`A=~/.athenaeum-acc/star-lab-a`).
- [x] **Step 5 — the four before-runs (sonnet judgment, ONE agent per dataset; ≈ 6–8 h machine time in all, one at a
  time under HEAVY.lock: Horse C11 → run B → set 204; the dev Mac, PB-17, overnight slots).** For each: `planb.sh clone <ds> b0`, `planb.sh run <ds> b0` (from Measure, fresh work dirs; the reference mode
  as stored), the staleness proof (`frame plane measured` = included planes; a short count is a STOP: delete the
  clone's `stackwork/*/measured` and re-run, no ruling), `reference-pick`, `c-table`, `c_identity.py` (set 204 and Horse
  C11: `differ 0`), `planb.sh truth <ds> b0` (the truth re-read on each run's own LN reference: `results/plan-b/b0/<ds>/
  truth.csv`), then `strip-light` ONLY after the results commit (`registered`, `ln`, `stackout`, the DB and logs stay —
  PB-15). Set 204's run also arms the gates: `STARLAB_ACC_COPY=~/.athenaeum-acc/planb-s204-b0 STARLAB_ACC_RUN_ID=<run>
  STARLAB_SCRATCH=<star-lab-b/b0-ident/gates> python3 bin/gates.py` → 13 / 13 (fixed answer; `probe_parity` included; all 13 on the dev Mac, PB-17).
  Verdict per dataset ≤ 30 lines. Commit lab: `results(plan-b/b0): the four before-runs - reference picks, the c table
  per group and channel, the truth on each run's own LN reference, gates 13 / 13, c identity 0 differ`.
- [x] **Step 6 — docs and the suite (sonnet docs; haiku suite with fixed answers; ≈ 1.5 h).** README R-OSP-16 opened
  with B0's block (the placement, the chain, the four picks, the `c` table); the open-items entry; `cargo test -p
  athenaeum-core --all-targets --test-threads=4 | grep -E '^test result|FAILED|panicked'` → 0 failed (the two documented
  flakes skipped by name as Task 14b did). Commit athenaeum: `docs(stacking): R-OSP-16 B0 - c kept per plane, the
  before-runs and their reference picks`.

**STOPs.** A lab identity miss (Step 4) → the controller (a code defect; a fresh sonnet fix round, Step 4 again). A
`c_identity` differ → the controller (the copy site is wrong). A staleness count short → the controller (delete the
clone's Measure work, re-run). Nothing here reaches the owner except the information package.

**Acceptance.** All Step 1 tests green; Step 4's fixed answers; four runs with recorded picks and `c` tables; the
suite green; the owner informed (PB-18).

---

### Task B1: the LN re-baseline on Plan A's fits, the ERP beside every arm (spec §3 F-8 l.290; bars l.374–392; §5 B1 l.713–731; R-B4, R-B10, R-B16 I-4, R-B17, the owner's ERP YES)

**Why.** Every LN arm is read once on Plan A's fits, synthetic and real, with the ERP a column beside every arm and the
freshness of every ERP LN proven; the truth CSVs run B and Horse C11 G need are rebuilt; the F1 v2 port gets its own
real read; the first ERP LN runs the owner cleared (the real sets, the injected pairs, the nebula-sky pairs) happen here.
A baseline carries no bar.

**Coupling checklist.** Reads: the `t14-final` outputs (Plan A's `c`, F, β), the harness truth files (B2 validates the
referee; B1 only prints against it), B0's `c` (the halo excess goes to B2). Re-estimates nothing. Rulings: R-B10 (the
truth CSVs rebuilt from `ln-real-members.csv`, md5s checked), R-B4 (the audit), PB-4 / PB-5 / PB-6. Propagation: B2's
realism inputs (`annulusCrowding` per member), B3's before column, B6's baseline beside every after number.

**Files.** Lab: `starlab/scorecard.py` (`rows_ln`: the `erp` cell on every metric and arm; `rows_ln_injected`: the ERP
beside when its `ln.csv` exists), `bin/erp_audit.py` (an `ln` section: per `ln.csv` the frames it read, their sha256
in the manifest in force at the file's mtime vs today's, CURRENT / header-only / STALE / NEW SINCE, the summary line),
`bin/erp_ln_real.py` (new, PB-6), `bin/ln_truth_rebuild.py` (new), `f1v2/real_read_b1.py` (new, PB-5),
`harness/acceptance/planb.sh b1 <job>` (`synth`, `audit`, `truth-rebuild`, `ln-real`, `f1v2`, `erp-<ds>`, `parity`,
`table`), `tests/test_rows_ln_erp.py`, `tests/test_erp_audit_ln.py`, `tests/test_erp_ln_real.py`, `tests/test_ln_truth_
rebuild.py`; `results/plan-b/b1/`. Runner: `/Volumes/Universe/athenaeum-runner/handoffs/planb-b1-l10b.md`.

**Interfaces.** Produces `results/plan-b/b1/{synth-ln-table.md, erp-ln-audit.txt, runB-truth.csv, hc11G-truth.csv (+
.sha256), ln-real-table.md, annulus-crowding.csv, f1v2-real.md, erp-ln-<ds>.csv, l10b-ln-table.md, parity.txt}`; the
rebuilt truth CSVs also at `~/.athenaeum-acc/ln-scale-audit/runB-truth.csv` and `ln-model-fixes-t16/hc11G-truth.csv`
(the paths `ln_real.py`'s `DATASETS` name). Consumed by B2 (crowding, the truths), B3 (the before column), B6.

**Bars (verbatim).**
- l.374–385: "Every LN table, synthetic and real, prints the ERP beside EVERY arm (`psfOff`, `psfSel`, `f1_2`, `f1_3`,
  `f1_gc` and the F1 v2 lab port), not on `sLib` rows only as today (`starlab/scorecard.py` `rows_ln`; a lab change).
  `erp_audit.py` gains an LN freshness audit: each `ln.csv` linked by sha256 to the frames it read, as the star units are
  (the 24 synthetic `ln.csv` are unaudited today; 5 of them, `truthcheck`, `density_pairs` ×2 and `osc_pairs` ×2, predate
  the 2026-10-08 header augment of their frames); an `ln.csv` re-runs only where its frames changed. On real data the
  existing ERP LN is always printed (set 204 30-member subset −3.09 % / −1.92 %/px², Horse C11 G −2.44 % / slope −0.06;
  lab `findings.md` §4). Where none exists the ERP runs ONCE, in B1 (the owner's ERP answer, 2026-10-20): set 204 full
  mono and OSC and run B mono and OSC from their exported registered members, and the injected pairs, on the dev Mac; the
  nebula-sky pairs on the runner. New frames get their one run where they are made: the seeing-lever injected pairs
  and the realistic-field family (B2), the synthetic and the cropped / binned floor-(ii) frames (B4). A cell reads "not
  run" only until its run has happened; the re-run rule governs every later run."
- l.386–392 (the 3 % rule) is applied to the real tables as a FLAG column (reported; the truth cells it flags are
  re-checked by B2), never to the synthetic rows.
- §5 B1 (l.729–731): "Bar: none (a baseline); the `f1_gc` ↔ `.athln` parity printed (2026-10-07: median +0.0097 %).
  Owner: informed with the baseline."

**Steps.**
- [x] **Step 0 — the runner first (sonnet; a Remote Control handoff; ≈ 1 h machine).** `planb-b1-l10b.md`: on the runner,
  at athenaeum 068cf2d6 + the lab head, `lab_cycle.py --step b1-l10b --manifest t10b-l --prev <the l10b a1c step>
  --bars none` (the nebula-sky pairs at the current binary: the 15 `l10b/pairs/*` families × t25 / t45 / t60), then the
  ERP LN once on those pairs (`erp_v2.py ln manifests/t10b-l.json --families l10b/pairs`), the audit, the table with the
  ERP beside every arm (after Step 2's lab change lands — the runner pulls the lab head first), the results committed
  from the runner (`results/plan-b/b1/l10b-ln-table.md`, the small `ln.csv` copies). A ≤ 30-line verdict on the share.
- [x] **Step 1 — the truth CSVs (lab code; sonnet; ≈ 1.5 h).** `ln_truth_rebuild.py`: from `ln-real-members.csv` the
  rows of `runB-mono`, `runB-osc`, `hc11-G` into the two CSVs in the exact column layout `ln_real.truth_rows` reads
  (the test pins it on the s204 CSV: rebuilding s204 from the members CSV reproduces `s204mono-lnBref.csv` row for row);
  sha256 of each output recorded in `results/plan-b/b1/truth-rebuild.txt`; then every dataset's reference md5 against
  `ln-real-plan.json` (`ln_real.py check-truth` + the md5 of each `reference.fits` of the three trees: `a3fe9d04…`,
  `e034aa6d…`, `8871ab3b…`, `712cd2fe…`, `e60b93f4…`) — a mismatch is a STOP (the tree changed; nothing runs). Commit lab.
- [x] **Step 2 — the ERP beside every arm and the LN audit (lab code; sonnet; ≈ 2.5 h).** `rows_ln`: the `erp` cell for
  every key in `vals` (the ERP's s against the same `s_true`); `rows_ln_injected`: the same when `<family>/ln.csv` exists;
  `erp_audit.py`'s `ln` section (the three-step link of the star units applied to the LN inputs: reference + targets).
  Tests: `test_rows_ln_erp.py` (two arms, one ERP file → both rows carry the ERP cell; no ERP file → `—`),
  `test_erp_audit_ln.py` (a fixture manifest history: CURRENT / header-only / STALE / NEW SINCE each once); the
  `RUNNER_ERP_BIN` override in `erp/run_job.sh` and the `JOB` path of `run_ln.js` read from an environment variable
  (the runner's nebula-sky ERP run). Then, on the dev Mac (a
  python re-score, no heavy job), `planb.sh b1 synth`: `scorecard.py <manifest> --probe-dir ~/.athenaeum-acc/star-lab-a/t14b/probe --erp-dir
  ~/.athenaeum-acc/star-lab-v2/erp --rows 6 --json --out <file>` (a positional manifest, `--json` required, `--out` a file:
  `bin/scorecard.py:57–60`; no probe run; PB-4), the LN
  table per family × β × arm with the ERP beside, the audit (`erp-ln-audit.txt`: expected 19 CURRENT hash-identical, 5
  CURRENT header-only, 0 STALE; any STALE re-runs that family's ERP LN once and is recorded). Commit lab; push.
- [x] **Step 3 — `ln_real` on the three trees (sonnet judgment; ≈ 1 h; the 068cf2d6 probe, sha256 checked; the dev Mac,
  PB-17).** `planb.sh
  b1 ln-real` = `ln_real.py run` (every arm; the 2026-10-07 wall 2690 s) → `verify` → `table --gates <the t14-final gates
  summary> --csv ln-real-members.csv` on the five datasets, with `annulus-crowding.csv` (per member, the f1 arm's
  `ln_annulus_crowding`) for B2 and the `f1_gc` ↔ `.athln` parity (`athln_for` on each tree's sidecars: median and max
  |Δ|). The 3 % flag column: a cell where an arm and the ERP (where present) both miss the truth > 3 % the same way.
- [x] **Step 4 — the F1 v2 port's real read (sonnet judgment; ≈ 1 h; the dev Mac, it reads the trees).** `planb.sh b1 f1v2`: the 30 + 28 members (PB-5)
  re-dumped by `ln_scale_probe --stars-dir` on the trees' own references, `f1core.py`'s V0-gc5 and L3-gc5 on those
  dumps, the gap against the truth rows per member (pooled median |gap|, median gap, slope), the 2026-10-06 numbers
  (set 204 −1.01 % vs −0.64 %; Horse C11 G +0.08 % vs −0.13 %) printed beside with the reference note; the port's
  reproduction of production (`f1_gc` on the same dumps) printed as the parity of this read.
- [x] **Step 5 — the ERP's LN on the real sets and the injected pairs (sonnet, fixed answers per chunk; ≈ 2 h machine, in
  chunks; the DEV MAC, where the ERP and the frames are — nothing is transported (C-2); the owner's instance closed, the
  memory guard under HEAVY.lock, no compile beside it).** `erp_ln_real.py export <ds>` into `~/.athenaeum-acc/star-lab/
  real/<ds>/in/` (the frames' sha256 travel with them — the freshness link), then `run <ds>` (chunks of 5 through
  `erp/run_job.sh`; each chunk's fixed answer: the done marker, `scale` lines = the chunk's targets, exit 0; otherwise
  STOP that dataset, no retry with other flags) → `collect <ds>` for `s204-mono`, `s204-osc`, `runB-mono`, `runB-osc`, then
  the three `inject.json` pairs (`erp_v2.py ln manifests/inject.json`; their flat and dark are on bigbase3). The existing
  real ERP LN (the 30-member subset, Horse C11 G) is printed from `findings.md` §4 and `ln-math/ext-G-pass2.txt` as the
  spec says; the new runs are appended as the ERP column of Step 3's table (`planb.sh b1 table`).
- [x] **Step 6 — the baseline package (sonnet; ≈ 1 h).** `results/plan-b/b1/README.md` (one data line per table naming
  machine and volume), the lab commit `results(plan-b/b1): the LN re-baseline on Plan A's fits - the ERP beside every arm,
  the LN freshness audit, the real tables on the rebuilt truths, the first ERP LN runs on the real sets, the injected and
  the nebula-sky pairs, the F1 v2 port's own read`; push. README R-OSP-16 B1 block in athenaeum (one commit).

**STOPs.** A reference md5 mismatch (Step 1) → the controller: the tree is not the one the truth was measured on; nothing
runs until it is explained. An ERP chunk failure → the controller (the ERP's own log; a memory refusal waits for a quiet
machine). A STALE `ln.csv` → re-run once, recorded (the rule). The 3 % flag on a real cell → carried to B2, never
resolved here.

**Acceptance.** Every table exists with the ERP beside every arm; the audit reads 0 STALE after any re-run; the two
truth CSVs with sha256; the parity line; the owner informed with the baseline (PB-18).

---

### Task B2: the referee — the harness truth in both modes, field realism, the seeing-lever pairs (spec §3 F-8 l.290; bars l.402–435; §5 B2 l.732–740; R-B3, R-B5, R-B12, R-B13, R-B17)

**Why.** The harness truth judges 50 of 91 set 204 and 49 of 82 run B members in its crowded mode, validated to FWHM 5
only; no real-data verdict is read before the referee is validated where it judges, the synthetic fields are shown to be
realistic (or one realistic family is built), and injected pairs across the real seeing range exist.

**Coupling checklist.** Reads: s_true (a generator input), the harness photometry (the referee under test), B1's
`annulusCrowding` per member and B0's `c` (the halo excess), F (the harness's own). Re-estimates nothing; a lab fix to the
harness truth is a change of the REFEREE, reported to the owner before B3 reads it (the referee rule is the owner's).
Rulings: R-B3 (bars ±0.3 / ±0.8 % per cell by the mode that reads it, R-B17), R-B5 / R-B13 (the min…max "differs"
test), R-B12 (the 3 % rule on estimated truths: the stamp is an estimate, so it applies to the lever pairs; never to
s_true). Propagation: B3's referee cells and their flags; B4's floor truth; B6's after report.

**Files.** Lab: `harness/acceptance/grid-report.py` (`photometry(…, force_mode=None)`; `tests/test_grid_report_force_
mode.py`: the default's output identical on a synthetic pair; each forced mode returns that mode), `bin/truth_modes.py`
(PB-7), `bin/realism_table.py` (PB-8), `starlab/inject.py` + `bin/inject.py` + `manifests/inject-lever.json` (PB-10),
`synth.py` / `make_grid.py` (PB-9, only if needed), `planb.sh b2 <job>` (`modes`, `realism`, `lever`, `lever-erp`,
`family`, `family-erp`, `table`), `tests/test_truth_modes.py` (a rendered pair with known s_true read in both modes within
the bars; a cell under 10 stars reads n/a), `tests/test_inject_stamp.py` (the empirical stamp's sum and width within 1 %
of the frame's own F; a Moffat stamp unchanged), `tests/test_realism_table.py`; `results/plan-b/b2/`.

**Interfaces.** Produces `results/plan-b/b2/{modes-table.md (every cell: family, target, mode, n, gap %, SE, bar, verdict),
realism-table.md, lever-row6.md, family/ (if built), referee-verdict.md}`; the lever pairs' truth lists and their ERP
`ln.csv`; the crowded-mode fix, if built, as `grid-report.py`'s `crowded_growth` option (default OFF until B3's handoff
names it ON, recorded) with its own validation table. Consumed by B3 (the referee per cell and its flags; the realism
verdict beside the F1 miss), B4 (the floor truth), B6.

**Bars (verbatim).**
- l.404–412: "The harness truth (…) is read in BOTH modes, the mode forced, on every cell of `pairs/main`, `pairs/iso`,
  `density_pairs` and `truthcheck` where the mode finds its stars, against s_true, the targets beyond FWHM 5 included
  (`pairs` t60 at 6.0 px, `truthcheck` t3 / t4 at 6 / 7.5 px). Bars: the normal mode within ±0.3 % per cell, the crowded
  within ±0.8 % per cell (validated so far: normal max abs(gap) 0.287 %; crowded −0.045 / +0.154 / +0.594 % at FWHM 3–5
  only, its stated systematic +0.2 % at β 3 and +0.6…+0.8 % at β 2.5)."
- l.413–416: "A crowded cell beyond FWHM 5 outside ±0.8 % is the F1-like over-read R-B3 names: the lab builds a
  growth-corrected crowded mode and re-reads the same cells on the same bars, and only then does B3 read real data. A
  normal-mode cell outside ±0.3 % has its systematic reported with the cells, and every real member that mode judges is
  flagged. A real member that only a failing mode can judge is reported, never a referee cell."
- l.417–420: the injected pairs on a seeing lever: "s_inj is set by construction, so the referee needs no bar on them (the
  estimator's bar on them is under *F-8 estimator*; the stamp is an estimate of the frame's PSF, so the 3 % rule applies,
  R-B12); the injected stars' own aperture sum over the injection printed as the check (today 0.95–1.03 on all 18 inject
  planes)."
- l.422–435 (field realism): "`annulusCrowding` (…) and the halo `c` excess (`c` − 1, …) of every synthetic pair family,
  against the real members. (…) Medians are compared, the per-target-FWHM values printed (…). A family differs when its
  median lies outside the real members' range (min…max over every real member B1 reads; R-B13). If no pair family lies
  inside that range on both quantities, ONE realistic-field pair family is built: (…) The ERP runs on it; the harness
  truth is validated on it on the referee bars above."

**Steps.**
- [x] **Step 1 — the forced mode and the modes tool (lab code; sonnet; ≈ 2.5 h).** `force_mode` (PB-7) with its identity
  pin; `truth_modes.py` over the stored `pairs` grid targets (105 + the elongated ones, one unit each) and the rendered
  `density_pairs` / `truthcheck` realizations (the gates' K); output the modes table with the bar per cell by the mode that
  read it. Tests in full. Commit lab; push.
- [x] **Step 2 — the both-modes read (sonnet judgment; ≈ 3–5 h machine on the RUNNER (PB-17: it holds `grid/{iso,main}`, `density_pairs`,
  `truthcheck`), `STARLAB_JOBS` per the gates' memory rule; the
  handoff relayed by the controller).** `planb.sh b2 modes` → `modes-table.md` + the verdict: every normal cell within ±0.3 %,
  every crowded cell within ±0.8 %, or the list of misses with FWHM. Reading: a crowded miss beyond FWHM 5 → Step 2b; a
  normal miss → the systematic reported, the real members that mode judges flagged in B1's table (a `flag` column).
- [x] **Step 2b — the growth-corrected crowded mode (only if Step 2 misses; lab code opus design + sonnet code; ≈ 4 h).**
  `grid-report.py`: option `crowded_growth` — in the crowded mode the curve-of-growth correction c = median flux(5F) /
  flux(8F) per frame over the same stars with an annulus [9F, 11F] (the crowded isolation 9F keeps the 8F circle clean of
  other candidates), applied as the normal mode applies its 3F / 8F ratio; the default stays OFF, the option recorded;
  the SAME cells re-read with the option on, the same bars; the validation table beside the old. Reported to the owner
  before B3 (the referee rule is the owner's; PB-18) — an information package with a recommended answer, not a menu.
  *As it ran (R-PB2-1, R-PB2-2; MEASURED OUT):* built (WO-1/2) and read (WO-3): 60 of 78 read cells missed against the plain mode's 12 of 73;
  `crowded_growth` pinned OFF, drivers retired, WO-4 cancelled, never re-proposed. The referee became the normal mode plus a far read
  (WO-5/5b/6), unreadable on the real sets (R-PB2-6): the ceiling allowance 1 %/px x abs(dF). README R-OSP-16 B2.
- [x] **Step 3 — field realism (sonnet table; opus verdict; ≈ 1.5 h).** `realism_table.py`: the 19 `pairs`, `density_pairs`,
  `osc_pairs`, `truthcheck` families' `annulusCrowding` medians and halo `c` excess medians (per target FWHM printed)
  against the real members' min…max (set 204 mono, set 204 OSC per plane, run B mono and OSC, Horse C11 G — from B1's
  `annulus-crowding.csv` and B0's `c-table`). Verdict: the families inside on both; if none → Step 3b.
  *As it ran (R-PB2-7):* mono realistic (19 of 22 families inside on both), OSC not (0 of 6 plane-rows); Step 3b not triggered.
- [ ] **Step 3b — the realistic family (only if Step 3 says so; lab code sonnet; generator run ≈ 1 h and the ERP once
  ≈ 10 min, both on the RUNNER; the family's frames stay there and are the runner's copy of record).**
  PB-9: build, verify determinism (two renders equal), its ERP LN run (`erp_v2.py ln` on the new grid family), the harness
  truth validated on it in both modes on the referee bars (Step 2's tool on the new family), its LN rows at the 068cf2d6
  binary (the `ln.*` jobs through `run_probe.py` on a quick manifest naming the family; a results run, not quick mode: the
  family is new, nothing to diff).
  *NOT TRIGGERED (R-PB2-7): the plan's rule asks for one realistic pair family, and 18 of 19 `pairs` families are inside the mono range.*
- [x] **Step 4 — the seeing-lever pairs (lab code sonnet ≈ 2 h; inject run ≈ 30 min; the probe LN rows ≈ 15 min; the ERP
  once ≈ 5 min; the inject run, the rows and the ERP all on the DEV MAC: the source flat and dark are on bigbase3, PB-17).** PB-10: `--stamp empirical`, the six pairs, `run_probe.py manifests/inject-lever.json` (plane 0, β auto
  and 4), `rows_ln_injected` with the ERP beside (its one run through `erp_v2.py ln manifests/inject-lever.json`), the
  injected stars' own aperture sum printed per pair (the check). The 3 % flag applies (the stamp is an estimate).
  *As it ran (R-PB2-4, R-PB2-5, R-PB2-8):* B-spline stamp (bilinear failed the 1 % width test), placement >= 8F from real peaks >= 20 sigma, and the
  gain gate became the free fit pooled over the six sources (the OFFSET-200 dark on OFFSET-30 lights, PRODUCT FINDING); six pairs, sums 0.997-1.005.
- [x] **Step 5 — the referee package (sonnet; ≈ 1 h).** `referee-verdict.md`: which mode judges which real member (from
  B1's table), each cell's flag, the realism verdict beside the F1 `pairs` miss (the spec's "the miss recorded beside B2's
  field-realism verdict"), the lever row. Lab commit `results(plan-b/b2): the referee in both modes on every cell, the
  realism table, the seeing-lever pairs`; push. README R-OSP-16 B2 block. The owner informed (PB-18); a crowded-mode fix
  waits for the owner's word before B3's real read uses it.
  *As it ran:* lab 03e300d (referee-verdict.md); the README block and the OFFSET entry in `docs/superpowers/open-items.md` in athenaeum. No crowded-mode fix
  exists to wait on (R-PB2-2).

**STOPs.** A crowded cell beyond FWHM 5 outside ±0.8 % → Step 2b (the lab), then the owner's word. A normal cell outside
±0.3 % → reported; the members it judges flagged (never a referee cell). A lever pair whose injected aperture sum is
outside 0.95–1.03 → the stamp is wrong (the controller; a fix round on `inject.py`, never a bar move; a still-missed
spec bar after that round goes to the owner, §3 doctrine).

**Acceptance.** The modes table complete; the realism verdict; six lever pairs with their rows and ERP; B3's referee
cells named per real member; the owner informed.

---

### Task B4: `c`'s floors and `c`'s sample — F-9 and F-12, `MEASURE_VERSION` 5 (spec §3 F-9 l.291, F-12 l.294; bars l.466–479, l.488–513; §5 B4 l.741–750; R-B1, R-B8, R-B11, R-B13, R-B14, R-B16, R-B17)

**Why.** `c`'s qualifiers admit hot pixels (raw 0013 p0: 36 of 64, R weight × 0.40) and its sample is the 64 brightest
(not representative on undersampled R / B: 1.57 → ≈ 1.2); both `c = 1` floors have no measured case. `c` moves the
weights, psfSel's s and the LN reference's composition, so its definition lands before B3 judges the estimator (R-B14).

**Coupling checklist.** Owns `c` (its qualifiers and sample; F-5's definition of the ratio unchanged). Reads: the
sibling planes' fits (the screen; the module's `crossFlags` logic), the fitted FWHM per star (Task 6b / 9), `StarFit.snr`
(F-6: the ONE per-star SNR) and B_i (10b),
the `saturated` and `extended` flags (8 / 10c / 10b), the growth curve (Task 10). Re-estimates nothing. Rulings revisited:
R13b-6 (`photometry.rs:1017–1019`: no per-stratum cap, revisited by name in Step 0b), R-OSP-7 (floor (i): kept unless the owner rules otherwise after the verdict), the LN cycle's I-2 (floor (ii): the same),
PA-4 / R-OSP-1 (one bump per plan → 5), I.1 (inject row 6 is REPORTED, R-B17), R-B11 (`c` moves by design: ρ reported,
the lab row 6 against generator truth is the bar). Propagation: every weight, psfSel's s, Register's and LN's ordering,
B3's candidates (read on the final `c`), B0's before-runs → B4's after-runs → B3's before.

**Files.**
- Modify: `crates/athenaeum-core/src/star_measure/photometry.rs` (`aperture_correction` l.994–1110: the point-source
  screen (PB-12'; sibling-plane fit lists passed in) inside the candidate filter, counted in `screened` and in a new `point_source: usize` returned with the SE;
  the stratified sample replacing the `candidates.sort_by … for … if ratios.len() == CORRECTION_STARS` loop; the
  per-tercile growth / model medians on the debug line go to the PROBE's diagnostics output
  (`apertureTerciles`), not the log: a new log field name needs a dictionary entry), `star_measure/mod.rs` (`MEASURE_VERSION = 5`, the history line; `PlaneMeasurement
  .point_source_screened`), `stacking/measure.rs` (hand edits: the adapter copies `point_source_screened` onto the
  reported `PlaneMeasurement` only — not persisted), `stacking/config.rs` (the I-5 pins to version 5), `examples/star_probe.rs`
  (`--dump-ratios`, `pointSourceScreened`, `apertureTerciles` in the JSON lines), the logging spec's dictionary (`point_source`), `docs/stacking/README.md` (R-OSP-16 B4
  block; l.1872 policy line), `RELEASE_NOTES.md` (the re-measure note), `docs/superpowers/open-items.md` (l.281–290 (b)–(d)
  resolved; l.317–323 closed by the (ii) verdict).
- Lab: `bin/floor_cases.py` (PB-11; `build-i`, `build-ii`, `run`, `verdict`), `planb.sh b4 <job>` (`cases`, `cases-erp`,
  `cycle`, `inject`, `x040`, `runs`, `compare`, `table`), `bars/planb_b4.py` (the lab-cycle `--bars` module: F-5's bars,
  the row-6 2-SE STOP against Plan A's medians per family, the refusals per family against truth), `tests/test_floor_
  cases.py` (two synthetic case sets: a paired difference > 2 SE → the arm; < 2 SE → "indistinguishable"; 5 cases → "too
  few cases (5)"; no case → "not reached"), `tests/test_planb_b4_bars.py`; `results/plan-b/b4/`.

**Interfaces.** Produces: `aperture_correction(…) -> (Option<f64>, usize, Option<f64>, ApertureScreens { point_source:
usize, … })` — the existing callers (`fit_stars_at`'s photometry stage, the probe) take the fourth value;
`MEASURE_VERSION = 5`; `PlaneMeasurement.point_source_screened: usize`; `results/plan-b/b4/{floor-i.md, floor-ii.md,
x040.md, inject-row6.md, lab-cycle verdict, runs/<ds>/…, weights-rho.md, masters.md, verdict.md}`; the four after-runs
(clones `planb-<ds>-b4`, B3's before). Consumed by B3 (the final `c`, the before-runs), B6.

**Bars (verbatim).**
- F-9 l.467–470: "A case counts only when the product reaches the floor on it: for (ii) `ln_growth_pairs` 1–4 or the
  0-pair warn, with ≥ 20 matched pairs; for (i) the "fewer than 5 isolated unsaturated stars" warn. The reference for a
  crop or a binned copy of a real frame is the same plane or pair uncropped (its `c` from ≥ 5 qualifiers, kept by F-11;
  F1's factor from its 100 growth pairs); for a synthetic case, the truth (row 6's captured fraction for (i), s_true for
  (ii))."
- F-9 l.471–479: "Verdict per floor (R-B13): the arm with the smaller median abs(error) against the reference over the
  counted cases, when the paired difference exceeds 2 SE; within 2 SE the verdict is "indistinguishable" and today's
  behaviour stays. A verdict needs at least 8 counted cases per floor over at least 2 datasets, its SE by bootstrap over
  the cases (R-B16); below that the verdict is "too few cases", reported with the count, distinct from "indistinguishable".
  A floor no case reaches is reported "not reached", with the lowest count the cases reached; floor (ii) may close as
  "not reached in the product" with that proof (`MIN_MATCHES` 20 excludes the member first; R-B13). For (i) the floored
  plane's weight relative to its group's other planes and its psfSel s are printed per case. Each verdict goes to the
  owner."
- F-12 l.489–492: "The × 0.40 R case: on raw 0013 p0 the 36 hot-pixel `c` stars are refused, and the same stars' 0004 →
  0013 ratio on p0 lies within 3 % of the mean of p1's and p2's (today 0.617 against 1.007 / 0.970 …)."
- F-12 l.493–501: inject row 6 "is REPORTED, not a bar (R-B17) (…). On the clean mono frames every frame is printed with
  its row 6, its `c`-free ratio (model flux / injected flux, which B4 does not move), its `c` and Plan A's row 6 beside;
  B4's `c` bars are the lab row 6 on the families whose generator flux includes a wing or halo (`m4w5`, `m4w10`, `g-w5`,
  `halo`), against that truth, and the R-B13 STOP; on the undersampled OSC planes (…) row 6 is printed beside its `c`-free
  ratio and `c` (a lab change) and reported."
- F-12 l.502–505: "Master level: the four master-level groups (…) stacked at the B4 binary with B0's reference picks
  pinned, against B0's runs; these after-runs are B3's before-runs (R-B16). `c` moves the weights by design, so the
  weights' ρ against B0's run is reported per group, each larger mover tied to its cause, not a bar (R-B11); the master
  rows reported."
- F-12 l.506–510: "Lab truth: F-5's bars hold (the spread across profile families ≤ 2 % at F 2.6 and F 8.0; psfSel within
  ±1.2 % / ±2 %), and every row 6 cell on the lab families, whose true captured fraction does not depend on brightness
  and where row 6 already reads 0.9865–1.0010 (…), stays within 2 SE of its Plan A median (R-B13): a drop there is the far
  ring's additive bias on faint stars (R-OSP-7, RF7) entering the sample, an estimator artefact, and a STOP. The
  qualifiers' growth / model ratios per brightness tercile are printed against truth beside it."
- F-12 l.511–513: "Reported: the screen's refusals on the clean lab families counted against truth, per family (a truth
  star refused is the screen's defect); the planes at floor (i) before and after; the sharp-R ratio (mosaic 0176 → 0206
  p0) at the group β, from F-11." and "`MEASURE_VERSION` 5 moves every stage hash; B4 lands before B3 (R-B14) …".
- No-change guards (l.393–402): F-5's bars and the no-regression rule (every row B4 does not change by design within its
  SE) bind the lab cycle; ρ is reported here (B4 moves the weights by design).

**Steps.**
- [x] **Step 0 — the floor cases, before any code (lab code sonnet ≈ 3 h; runs sonnet ≈ 2 h at the 068cf2d6 binaries,
  all on the dev Mac: the (i) crops of set 204 OSC calibrated planes, the (ii) crops / binned pairs from the trees, their
  ERP LN runs there too).**
  `floor_cases.py build-i` / `build-ii` (PB-11), `run` (the probes on every case; `--dump-ratios` is Step 2's, so arm 2
  of (i) is filled after Step 2 — the (i) cases are BUILT now, their reads completed at the B4 binary), the (ii) arms
  computed offline from the probe lines, each new stored pair's ERP LN run once (`planb.sh b4 cases-erp`). The (ii) read
  does not depend on B4's code (F1's growth is not what B4 changes) and completes here: `floor-ii.md` with the verdict
  by the rule (≥ 8 cases over ≥ 2 datasets; "not reached" with the lowest count if the product never reaches 1–4 pairs
  with ≥ 20 matches). Commit lab; push.
- [x] **Step 0b — `c`'s screen and sample, judged on existing frames BEFORE any code (research agent, opus; ≈ 3 h; no
  product code, no new run: the dumps of `t14b/probe`, `t14b/inject-probe` and `star-lab-v2`, the raw 0004 / 0013
  planes).** (1) PB-12's screen on the lab truth stars of `osc/*` and `osc_pairs/*` (a refused real star counted per
  family, bar ≤ 1 % of qualifiers) and on raw 0013 p0's 36 hot pixels; where in the multi-plane pass the sibling fit
  lists exist for `aperture_correction`. (2) The sample: today's top 64 vs brightness-stratified vs mid-band, reading
  `StarFit.snr` (no second per-star SNR), R13b-6 revisited by name, every family printed against `c` truth INCLUDING
  `background/neb*` (the review: a 4 × 16 sample moves neb0.5 +2.43 %, neb3 +23.16 %, sky structure between 5F and the
  [10F, 12F] ring that σ_i does not measure); a sample that moves any family's `c` AWAY from truth by more than 2 SE vs
  today is refused; the raw 0004 p0 brightness trend (1.57 → ≈ 1.2) explained, real vs fit bias, before a sample is
  chosen. Output one page → the controller's rulings PB-12' / PB-13', which Steps 1–2 implement.
- [x] **Step 1 — the failing tests (rust-engineer sonnet; ≈ 1.5 h; written after Step 0b: the shapes below, names and
  thresholds per PB-12' / PB-13').** `photometry.rs`: `point_source_screen_refuses_a_candidate_with_no_sibling_plane_fit`
  (a three-plane field of 40 stars on every plane plus 30 single-pixel spikes on plane 0 only → `point_source == 30`, `c`
  within 1 % of the clean field's), `point_source_screen_leaves_a_mono_plane_alone`, `point_source_screen_refusing_every_
  star_floors_c_and_warns` (a plane with only spikes → `c = None`, `count 0`, the warn), `stratified_sample_takes_sixteen_
  per_quartile_of_qualifiers_above_snr_20` (a field of 400 stars on a power-law luminosity function → the sample holds
  stars from every quartile, 64 in all, none twice), `stratified_sample_with_six_qualifiers_takes_all_six_once`, and a pin
  `c_on_a_bright_only_field_equals_the_top_sixty_four_median` (a field of exactly 64 equal-flux qualifiers: the stratified
  sample is the same 64). `config.rs`: `measure_version_five_moves_the_three_fit_reading_subtrees_only` (B0's three pins
  re-asserted at version 5, `calibration_subtree` unchanged: it FAILS until the bump because the constant is still 4, so
  the RED step holds no pin). Run → FAIL where expected.
- [x] **Step 2 — the code (rust-engineer sonnet; ≈ 4 h).** PB-12', PB-13', PB-14, the probe's `--dump-ratios`,
  `pointSourceScreened`, `apertureTerciles`. Step 1 green; `cargo test -p athenaeum-core --lib -- star_measure` and `--
  stacking` green. Commit athenaeum: `feat(star-measure): Plan B B4 - the point-source screen and the brightness-stratified
  sample for c; MEASURE_VERSION 5`.
- [x] **Step 3 — review (opus; ≈ 1 h)** against PB-12' / PB-13' and the F-12 row; fix round by a fresh sonnet agent.
- [x] **Step 4 — the lab cycle at the B4 binary (sonnet judgment; ≈ 1.5 h; the dev Mac builds in its compile slot, the
  cycle runs on the RUNNER at its own build of the same commit, the handoff relayed).** `planb.sh build`, then
  `lab_cycle.py --step b4-c --prev t14-final --inject --bars bars/planb_b4.py --design results/plan-b/b4/design.json`
  (the design list: every LN row and every `c`-reading row the task changes by design — row 6 on inject OSC, the weights
  rows; nothing else). Bars: F-5's (spread ≤ 2 % at F 2.6 / 8.0; psfSel ±1.2 % / ±2 %); the row-6 2-SE STOP on every lab
  family against Plan A's medians (the lab-cycle verdict prints each family's Δ / SE); the wing / halo families' row 6
  against generator truth (m4w5, m4w10, g-w5, halo); the refusals per clean family against truth; the planes at floor
  (i) before / after; inject mono row 6 per frame with the `c`-free ratio, `c`, Plan A's beside (reported); inject OSC
  row 6 beside `c`-free and `c` (reported). The row-6 STOP → PB-13's one fix round (the runner-up sample PB-13' names; Steps 2–4
  again on a quick manifest `quick:pairs/main/m4,pairs/main/m4w5,pairs/main/m4w10,g-w5,sharp/*,halo,background/neb*` first —
  the families B4's `c` bars read, row 6 on a pair family being the LN scale, plus neb* for the printed `c` — the full
  manifest once at the end); a second miss →
  the owner (PB-18).
- [x] **Step 5 — the × 0.40 R case and floor (i) (sonnet judgment; ≈ 1 h; the dev Mac — `/Volumes/bigbase2` /
  `bigbase3` are never reachable from the runner; short probe runs, no stacking).** `planb.sh b4 x040`: `star_probe` on raw 0004
  / 0007 / 0013 (debayered) at the B4 binary; the per-star dumps joined across frames: the 36 hot-pixel stars of 0013 p0
  screened (`pointSourceScreened ≥ 36` and none of the 36 among the qualifiers), the same real stars' 0004 → 0013 ratio
  on p0 within 3 % of the mean of p1's and p2's. `floor_cases.py run --arm2` on the (i) cases (now with `--dump-ratios`),
  arm 3 from B0's payloads, `verdict` → `floor-i.md` (the paired difference, the bootstrap SE, the weights and psfSel s per
  case). The sharp-R ratio (mosaic 0176 → 0206 p0) at the group β from B0's kept `c` (reported).
- [x] **Step 6 — the four after-runs (sonnet judgment; ≈ 6–8 h machine, one at a time: Horse C11 → run B → set 204; the
  dev Mac, as B0's — never split).**
  `planb.sh clone <ds> b4` (from the TREES, not B0's clones: a clone of the before tree keeps its Calibrate artifacts;
  `make-config --ref-out` with the reference pinned to B0's pick, `ReferenceMode::Manual`, `twoPass false`; `ATH_ACC_
  EXPECT_REFERENCE` = B0's pick), `run`, the staleness proof (version 5: every plane measured, registered and normalized
  in the window — `run_log_count.py` for `frame plane measured`, the registration and `ln frame normalized` lines),
  `truth` (`ln_truth_run.py` on the after-run's own LN reference), `compare` (`tier1-compare.py --numeric` vs B0's clone's
  DB: the master rows reported; `t13-tools.py weights` vs B0's run: ρ per group reported with each larger mover's cause —
  `c` before / after on that plane from the payloads; the LN scale against the after-run's truth per member, B0's beside,
  the re-based row printed). Results committed per dataset before the clone is stripped light (B3's before IS B4's
  after-run, so B0's clones are deleted entirely once B4's `compare` is committed, except their `results/plan-b/b0/<ds>/truth.csv`).
- [x] **Step 7 — the verdict package and docs (opus for the verdict tables; sonnet docs; haiku suite; ≈ 2.5 h).**
  `results/plan-b/b4/verdict.md`: both floor verdicts by the rule, the × 0.40 case, the lab bars, the row-6 reports, the
  master rows and ρ per group. README R-OSP-16 B4 block (the screen's design, the sample, the version bump and its ripple,
  every number), the policy line, `RELEASE_NOTES.md`, open-items. `cargo test -p athenaeum-core --all-targets` green.
  Commit athenaeum `docs(stacking): R-OSP-16 B4 - c's screen and sample, the floor verdicts, MEASURE_VERSION 5`; lab
  `results(plan-b/b4): …`; push lab. The owner's STOP: the two F-9 verdicts (PB-18); the product's floor behaviour changes
  only on the owner's word (a follow-up commit with its pin `growth_floor_behaviour_matches_the_owner_ruling` /
  `fit_call_floor_behaviour_matches_the_owner_ruling`).
  *As it ran:* the owner delegated both verdicts to fable (R-PB4-5 adopted in 13aee664 with its pin; R-PB4-6 keeps the code,
  the existing pins). Lab e3e85ec (`results/plan-b/b4/verdict.md`); the README R-OSP-16 B4 block, the R-OSP-1 policy line (Plan B 5,
  Plan C 6), the R-OSP-7 fallbacks, spec F-9 / F-12 and open-items in athenaeum. The suite is the controller's at 13aee664
  (`--all-targets` 4115 / 0 / 20). `RELEASE_NOTES.md` holds only the last release, so the re-measure note went to open-items'
  "Release notes owed at the next tag", where the LN cycle's lines wait.

**STOPs.** The row-6 2-SE drop (Step 4) → PB-13's one fix round, then the owner. A truth star refused by the screen on a
clean family → the controller (a fix round on the sibling-plane tolerance only if a real star is refused; the tolerance
is a ruling for the owner, never silently moved). The × 0.40 case missing its 3 % → the controller (one fix round from
Step 0b's numbers). After any such round a still-missed spec bar goes to the owner with its numbers (§3 doctrine). A
staleness count short → delete the clone's Measure / Register / LN work, re-run.
**Acceptance.** Step 1 tests green; the lab cycle's bars PASS (or the STOP resolved); the × 0.40 case; both floor
verdicts delivered; four after-runs with their comparisons committed; the suite green; the owner's two verdicts asked.

---

### Task B3: the LN estimator decision (spec §3 F-8 l.290; bars l.437–463; §5 B3 l.751–759; R-B6, R-B7, R-B11, R-B13, R-B14)

**Why.** F1 over-reads on synthetic pairs (+11.97 % at `pairs/main/m4w10` t60) and sits on the harness truth on real
data; psfSel has never been read on real data with Plan A's fits, and now reads on B4's `c`; the F1 v2 sky is a candidate
only under its entry test. The default estimator is the owner's choice (L-24 was), with every candidate on the same rows.

**Coupling checklist.** Reads: B4's `c` (final), B2's validated referee per cell with its flags, B1's before column
(Plan A's `c`), the ERP columns (B1, B2; no new run), s_true. Re-estimates nothing; the F1 v2 sky, if it enters, is an
owner API change on `ring_sky` (a caller F and a union mask), never a second sky inside LN. Rulings: R-B7 (master
tolerances 0.1 / 1 / 2 / 1 %, ρ ≥ 0.9 a guard, the reference pinned), R-B11 (s moves by design: pooled median |gap| not
worse, every member printed, the 3 % rule per cell, the re-based row printed), R-B6 / R-B13 (the entry test),
R-B16 I-2 / R-B17 (the per-cell synthetic bars). Propagation: B5's scope (module-fed or not), B6's after report.

**Files.** Lab: `bin/b3_table.py` (every candidate on the same rows: the 19 `pairs`, `density_pairs`, `osc_pairs`,
`truthcheck`, the injected pairs, the lever pairs, the realistic family if built; per cell s / s_true, the slope, the ERP
beside, the bar by the mode that reads the cell; the real datasets' median gap and slope against the validated truth
with the ERP beside and the flags), `f1v2/entry_test.py` (the pooled median |gap| of the port vs F1 on the same members
from B1 Step 4, and the three cosmic-ray seeds on `pairs/main` t60: `synth.py --cosmic-rays --seed k` renders, the port's
sky on each), `planb.sh b3 <job>` (`cycle`, `ln-real`, `entry`, `table`, `runs`, `compare`); `results/plan-b/b3/`.
athenaeum, only on the owner's word: `stacking/integrate.rs` (`LocalNormalizationConfig::default`, the enums'
`#[default]`; a stored document without the keys keeps today's decode rule), its tests; only if the F1 v2 sky won:
`star_measure/photometry.rs::ring_sky` (caller F, union mask), `stacking/ln/scale.rs` (a `CommonAperture` sky arm reading
the module ring at max(F_R, F_T) with the union of both fit lists, the pair's own two excepted; `LnFluxEstimator` gains
no variant until the owner picks — the arm is reachable by the probe first).

**Interfaces.** Produces `results/plan-b/b3/{synthetic-table.md (before = B1's rows at Plan A's `c`, after = the B4-binary
cycle), real-table.md (ln_real at the B4 binary on the three trees against B2's validated referee cells, B1's beside),
entry-test.md, masters/ (only if the default changes), verdict.md}`. Consumed by the owner (the choice), B5 (the scope
rule), B6.

**Bars (verbatim).**
- l.438–441: "B3 runs after B4 (R-B14), so every candidate is read on B4's `c` (`MEASURE_VERSION` 5 …): B1's synthetic
  rows (the `t14-final` probe outputs, Plan A's `c`) are the before column and one lab cycle at the B4 binary the after
  column; the real rows are `ln_real` on the three trees at the same binary, B1's beside."
- l.445–451: "The synthetic bars a new default meets, per cell against s_true at every target (R-B16): F-5's psfSel bar on
  the 19 `pairs` (within ±1.2 % on the Moffat and Gaussian pairs, ±2 % on the blur pairs); `truthcheck` and `density_pairs`
  per cell within the referee's own bar for the mode that reads that cell (±0.3 % normal, ±0.8 % crowded; R-B17);
  `osc_pairs` within ±2 % per plane; the injected pairs within ±2 % (s / s_inj); the realistic family, if built, within
  ±2 % (its s_true is a generator input). Keeping F1 as is misses the `pairs` bar (+11.97 % at `pairs/main/m4w10` t60);
  that is the owner's choice, the miss recorded beside B2's field-realism verdict."
- l.452–458: "The F1 v2 sky is a candidate only if (1) the lab port's pooled median abs(gap) against the validated truth
  over the real members is below F1's (R-B13; …) and (2) its sky is robust to cosmic rays: on each of three cosmic-ray
  seeds its `pairs/main` t60 cell moves by ≤ 0.53 pp (the median sky's worst; the plain mean moved +3.22 pp on one seed of
  three). Only then is the module ring API change in scope (…)."
- l.459–463: "Master level (R-B7), for any change of the default: whole groups set 204 mono and OSC, run B mono and OSC,
  Horse C11 L and G, stacked with the reference pinned (`ReferenceMode::Manual`, the pick B0 recorded), the before runs
  being B4's after-runs (R-B16): master median / MAD / noise / FWHM within 0.1 / 1 / 2 / 1 %; weights ρ ≥ 0.9 (a no-change
  guard). An estimator change moves s by design, so the LN scale is judged against the validated truth (R-B11): per
  dataset the pooled median abs(gap) of the after-run not worse than the before-run's, every member printed with its
  gap before and after, the 3 % rule per cell, the re-based LN row (R13-4 …) printed beside, not a bar."

**Steps.**
- [x] **Step 1 — the after column (sonnet; ≈ 1.5 h; no new build: B4's binary; the re-score where B4's cycle
  outputs are (the runner's, copied back small), `ln_real` on the dev Mac).** `planb.sh b3 cycle` = B4's `b4-c` cycle
  outputs re-scored for the LN rows with the ERP beside (no probe run; the B4 cycle already produced every LN line at the
  B4 binary), plus `ln_real.py run / table` at the B4 binary on the three trees (≈ 45 min) against B2's referee cells (the
  mode per member, its flag; a flagged cell printed but not counted), B1's beside; `b3_table.py` → `synthetic-table.md`,
  `real-table.md` with every bar per cell for every candidate (F1 as is, psfSel on B4's `c`; the F1 v2 port's column from
  Step 2 if it enters).
  *As it ran:* tools lab 9f74ee5; the before rows on the dev Mac (lab aedfe27), the after side on the runner from `b4-c2`'s
  scorecards, no probe re-run (lab fd54b5f); `ln_real` on the B4 after-run clones `planb-<ds>-b4`, not the three trees (B2's
  referee cells sit on B0's pinned references), Horse C11 L added: 885 member-planes, 767 counted (lab b1af182).
- [x] **Step 2 — the F1 v2 entry test (sonnet; ≈ 1.5 h).** `entry_test.py`: (1) the port's pooled median |gap| vs F1's on
  B1 Step 4's members and dumps (re-dumped at the B4 binary for consistency: `c` only orders the pairs under F1, so the
  port's numbers should not move — the Δ printed); (2) three cosmic-ray seeds on `pairs/main/m4w10` t60 (`synth.py` with
  the f1v2 cosmic-ray generator, seeds 1–3), the port's L3-gc5 cell per seed, the move ≤ 0.53 pp each. Both pass → the
  module ring API change is in scope: Step 2b. Either fails → the candidate is out, recorded with its numbers.
  *As it ran:* OUT on both (lab d3bd6f2): (1) the port 0.582 % against F1's 0.296 % over 58 members (Δ 0.0000 pp both);
  (2) moves +1.35 / +0.13 / +0.64 pp against ±0.53 (`synth.py` has no `--cosmic-rays`: the f1v2 generator on the rendered cell).
- [ ] **Step 2b — the F1 v2 sky through the module (only if Step 2 passes; rust-engineer sonnet code ≈ 4 h; opus review;
  a lab cycle sonnet ≈ 1.5 h).** `ring_sky(…, caller_f: Option<f64>, union_mask: Option<&SeedMask>)`, the LN arm, the probe
  arm `f1_v2`, tests (`ring_sky_at_a_caller_f_with_a_union_mask_equals_the_port_on_a_fixture` within 0.01 %; the default
  call byte-identical, a pin), the lab cycle at the new binary on a quick manifest (`quick:pairs/main/*,pairs/iso/*`) then
  full; the candidate's column added to both tables.
  *SKIPPED (R-PB3-1): the entry test is OUT (Step 2); `ring_sky` and the probe are unchanged.*
- [x] **Step 3 — the owner's package (opus; ≈ 1 h).** `verdict.md`: the candidates on the same rows, every bar per cell,
  the real tables, B2's realism verdict beside F1's `pairs` miss, the recommended default with one paragraph of why
  (memory `feedback_owner_questions_astrophotographer_language.md`: plain meaning and the effect on the master first).
  README R-OSP-16 B3 block (the tables' location, the entry test). PB-18 STOP: the owner chooses. Keeping F1 → Steps 4–5
  are skipped and the product is unchanged.
  *As it ran:* the owner's STOP delegated to fable: F1 as is stays the default (R-PB3-1; R-PB3-2 on B1's columns). Lab
  7e291fb (`results/plan-b/b3/verdict.md`, B5's branch included); README R-OSP-16 B3, spec F-8, open-items (the Plan C items).
- [ ] **Step 4 — the default change (only on the owner's word; rust-engineer sonnet ≈ 1 h; the literal and decode tests
  updated by name).** `integrate.rs` defaults; the stored-document rule unchanged (a document without the keys decodes as
  today — the existing test re-pinned with the new default named). Commit `feat(ln): Plan B B3 - the default LN estimator
  is <X> (owner, <date>)`.
  *SKIPPED (R-PB3-1): F1 as is stays the default; `integrate.rs` is untouched.*
- [ ] **Step 5 — master level (only after Step 4; sonnet judgment; ≈ 6–8 h machine, one at a time; the dev Mac,
  as B0's and B4's).** `planb.sh clone
  <ds> b3` (from the trees; the reference pinned to B0's pick; `ATH_ACC_RERUN_FROM=measure` — a clone of the TREE carries the LN cycle's artifacts, not B4's, so the
  run is from Measure again at the B4 binary + the new default — the Measure outputs equal B4's by construction (same
  binary for Measure), proven by the `metrics` payload equality per frame against B4's clone (`planb-tools.py
  measure-equal`), then Normalize differs by design), `run`, `truth` (on the after-run's own LN reference), `compare`
  vs B4's after-run (the master rows 0.1 / 1 / 2 / 1 %; ρ ≥ 0.9 the guard; the LN scale against the validated truth:
  pooled median |gap| not worse, every member with before / after, the 3 % flag, the re-based row beside). Results
  committed per dataset; B4's clones stripped light after the comparison commit; B3's kept for B6.
  *SKIPPED (R-PB3-1): no default change, so no master-level run; B4's after-runs stand as B3's.*

**STOPs.** A master row outside its tolerance or ρ < 0.9 under the new default → the owner with the numbers (never
tuned); the default is reverted on the owner's word. A pooled median |gap| worse than before's → the same. The F1 v2
entry test failing → recorded, no STOP.

**Acceptance.** Both tables complete with the ERP beside every candidate; the entry test recorded; the owner's choice
recorded in README; if the default changed, the master-level rows and the truth-judged LN rows committed.

---

### Task B5: LN's own estimates — module seeds, one saturation definition, the nebula sky, R11-3 (spec §3 F-13 l.295; bars l.516–527; §5 B5 l.760–765; R-B9, R-B11)

**Why.** LN still estimates seeds, saturation and the aperture-scale sky a second time (`k`, C-14; four saturation
definitions; LN's own tile sky on nebula: 13 of 34 P.1 cells over). If B3 kept a module-fed estimator, LN's seeds on both
sides come from the module, retiring `k` and `unverdicted_clip_range`; C-9 is in scope on P.1 / P.2; R11-3 stays a
recorded limit unless an owner is found.

**Coupling checklist.** Reads: the module's seeds (10b), its `saturated` flag (8 / 10c), B_i (10b) through psfSel's
refits, the group β (11b). A neutral swap: the no-change guards bind it (no synthetic LN cell worse than B3's beyond 2 SE;
the master rows; the re-based row ±0.5 %; ρ ≥ 0.9). Rulings: C-14 (`k`, retired), L-14 / L-23 (the two selection
screens: mapped to the flag or kept as a selection screen with the reason), Register's 0.95 (`register/detect.rs`
`SATURATION`: Register's own, kept — it is not LN's star selection; LN's warped guard reads it: mapped), C-9 (the tile
sky). Propagation: B6's after report; the owned-quantities table (`seeds`, `saturated` gain LN as a consumer).

**Files (if in scope).** `stacking/ln/mod.rs` (`LnScaleSeeds`: the reference side from the module's fits of the
integrated reference — `fit_stars_at` on the reference with its own seeds — instead of `register::detect::detect_stars`;
the target side `Measured` always), `stacking/ln/scale.rs` (the clip screen reads the flag; `unverdicted_clip_range`'s
call removed), `stacking/ln/selection.rs` (L-14 → the module flag; L-23's reference-frames map: kept as a selection
screen — it is a property of the INTEGRATED reference, not of a star's own plane — with the reason in the doc comment),
`stacking/ln/calibration.rs` (deleted; `measure_seeds_calibration` and the `ln_calibration` artifact gone; the run's
`"ln seeds calibration"` lines gone with their dictionary note marked retired), `star_measure/photometry.rs`
(`unverdicted_clip_range` deleted), `stacking/run.rs` (hand edits: the calibration call removed), tests: `ln_stars_carry_
the_module_saturated_flag` (a structural pin), `no_seeds_calibration_artifact_is_written` (the `k` is gone),
`unverdicted_clip_range_is_gone` (compile-time: the symbol does not exist). Lab: `planb.sh b5 <job>` (`cycle`, `l10b`
on the runner, `runs`, `compare`, `r11`), `results/plan-b/b5/`. C-9: `stacking/ln/background.rs` (the tile sky on nebula:
the owner of LN's background grid; the change is designed by an opus research step on the `l10b` tables — no
algorithm question to the owner), judged on P.1 / P.2.

**Bars (verbatim).**
- l.517–520: "P.1 (Plan A plan, row P.1): psfSel, and the default estimator if it is another, on the nebula-sky pairs
  (`iso/m4`, `iso/g`, `iso/blur25mix` × t25 / t45 / t60 × `neb0.5`, `neb3`; `manifests/t10b-l.json`, on the runner): Δ
  against the flat control on the same star field within ±1.2 % (m4, g) / ±2 % (blur25mix) on `neb0.5`, the same plus 2 SE
  on `neb3`; a cell refused for < 20 matched stars is n/a; before 13 of 34 cells over. P.2: warped targets within their
  unwarped twin's tolerance; before 3 cells at t25 over."
- l.521–525: "Module seeds (if in scope; a neutral swap, so the no-change guards bind it, R-B11): no synthetic LN cell
  worse against truth than B3's value beyond 2 SE; F-8's master rows on the real groups against B3's after-runs (the
  master tolerances, the re-based LN row within ±0.5 %, weights ρ ≥ 0.9); structural pins that LN's stars carry the
  module's `saturated` flag and that `k` and `unverdicted_clip_range` are gone."
- l.526–527: "R11-3: reported with its pulls, or its owner named with a bar of its own declared before it is measured."

**Steps.**
- [x] **Step 0 — scope (controller; 10 min).** From B3's verdict: module-fed default (psfSel or the F1 v2 arm through the
  module ring) → Steps 1–4; F1 as is → Steps 1–2 skipped, `k` and `unverdicted_clip_range` kept and recorded beside LN's
  other screens (the spec's l.562–564), Steps 3–4 run.
  *As it ran:* the F1 branch (B3 kept F1 as is, not module-fed; R-PB3-1): Steps 1–2 skipped; `k` and `unverdicted_clip_range`
  kept, recorded in the spec's owned-quantities rows and sentence; Steps 3–4 at B4's binary (B3 ran none, so "B3's" reads B4's).
- [ ] **Step 1 — module seeds (rust-engineer sonnet code ≈ 5 h; opus review; the failing tests first).** The three pins
  above written first (`ln_stars_carry_the_module_saturated_flag` on a fixture pair with one clipped star: it is excluded
  by the flag, not by L-14's 0.75 rule, which is gone), then the code; the LN library tests updated by name (the run tests
  pin `psfSignal` on the 192 × 144 field — unchanged); `cargo test -p athenaeum-core --lib -- stacking::ln` green. Commit.
  *SKIPPED (B3 kept F1, R-PB3-1): no module seeds; the three pins are not written; `k` and `unverdicted_clip_range` stay.*
- [ ] **Step 2 — the swap's guards (sonnet judgment; ≈ 1.5 h lab on the RUNNER + ≈ 6 h machine for the three runs on
  the dev Mac).** `lab_cycle.py
  --step b5-seeds --prev b3-est` (`b3-est` exists only if B3 Step 2b ran; else `b4-c`): every synthetic LN cell within 2 SE of B3's;
  `planb.sh clone / run / truth / compare <ds> b5` vs B3's after-runs (master rows, the re-based row ±0.5 %, ρ ≥ 0.9);
  results committed; B3's clones stripped light after the commit.
  *SKIPPED (B3 kept F1, R-PB3-1): no swap, so no guards; no B5 runs or clones.*
- [x] **Step 3 — C-9, the nebula sky (opus research ≈ 2 h on the l10b tables; sonnet code ≈ 3 h; the runner cycle ≈ 1 h).**
  The research names the change in `ln/background.rs` from the `p1-table.txt` cells (the per-side ring on nebula: which
  cells, which sign, the sub-10F structure share) with a prediction per cell BEFORE the cycle; the runner handoff
  `planb-b5-l10b.md`: the l10b cycle at the B5 binary, P.1 / P.2 per cell, B1's ERP column printed (no new run: the
  frames are unchanged). A miss on P.1 under the change → reverted, recorded (never tuned).
  *As it ran — no change (R-PB5-1):* the research (opus) put C-9 outside `ln/background.rs` and proposed F1's annulus mean; fable
  ruled it the F1 v2 study's L1-gc5, measured out on the real members (set 204 mono 30 of 30 worse, −0.40 pp); no code, no revert. The
  l10b cycle ran once at the B5 binary as the P.1 / P.2 table (runner, lab 6324185): `B5 L10B: PASS`, F1 = B1 bit for bit, 4 of 34
  over (R-PB5-2; 4, not 5); psfSel 13 → 15 of 34 at B4's `c`, checked by `planb.sh b5 ctable` (lab 75f4fcd) → R-PB5-3.
- [x] **Step 4 — R11-3 and the package (opus ≈ 1 h; sonnet docs).** R11-3's pulls (`neb0.5` 3.63, `neb3` 6.04) re-read at
  the B5 binary; its owner named only with a bar declared before measuring (a new row in a later plan), else "a recorded
  limit" — the owner decides the scope (PB-18). README R-OSP-16 B5 block; the owned-quantities table (`seeds`,
  `saturated`: LN a consumer; `unverdicted_clip_range` gone or kept with its reason). The suite green. Commits.
  *As it ran:* R11-3 re-read at 32f61897 (the 13aee664 code) on the dev Mac: 3.64 / 5.98, SAME (lab a03ce38); the owner's word
  (PB-18): a recorded limit. Package lab ff6070e (`results/plan-b/b5/verdict.md`); README R-OSP-16 B5, spec F-13 and the
  owned-quantities rows / sentence, open-items. No code changed, so no suite run (B4's core line stands).

**STOPs.** A synthetic LN cell worse than B3's beyond 2 SE, or a master row / re-based row / ρ miss → the swap is
reverted (a neutral swap that is not neutral is a defect), the controller re-audits the seeds' difference (R-B11's
spirit: never tuned). A P.1 miss under the C-9 change → reverted, recorded.

**Acceptance.** The structural pins green (if in scope); the guards PASS; P.1 / P.2 tables at the B5 binary; R11-3's
line; the owner's scope answer recorded.

---

### Task B6: the checkpoint and the "after" report (spec §5 B6 l.765–770)

**Why.** One uniform after-read of everything Plan B touched beside B1's baseline, the held bars re-judged, the suites
green, R-OSP-16 closed, the branch ready for the owner's merge and push.

**Coupling checklist.** A checkpoint owns no fix: a miss goes back to its owner task (B0 / B4 / B3 / B5) as a fix
round, and the checkpoint re-runs. Reads every table above.

**Files.** Lab: `planb.sh b6 <job>` (`cycle`, `ln-real`, `ln-truth`, `l10b` (runner), `report`); `results/plan-b/b6/`;
`star-lab/results/plan-b/README.md` (the index). athenaeum: `docs/superpowers/research/2026-10-2x-star-measurement-
after-plan-b.md` (the after report: the owner's rows, before / after, every bar as judged, the recorded limits with
owners), `docs/stacking/README.md` (R-OSP-16 closed: every ruling, every acceptance run), `docs/superpowers/open-items.md`
(the Plan B entries resolved; what stays), `CLAUDE.md` (the Stacking summary's one line on `c` kept and `MEASURE_VERSION`
5 — a rule changed, so the map changes).

**Bars (verbatim, l.769–770).** "Bar: every Plan B bar as judged plus the held bars (F-0…F-7, F-10, the no-regression
rule). Owner: the push and the close."

**Steps.**
- [ ] **Step 1 — the full uniform lab run (sonnet, bars and a design list; ≈ 2.5 h; on the RUNNER at its build of the final
  commit; the dev Mac builds in its compile slot if the binary is not the last one built).** `lab_cycle.py --step b6-final --prev t14-final --inject --bars bars/planb_b6.py` (every Plan B bar
  as judged + the held F-0…F-7, F-10 bars from Plan A's `bars.py`, cumulative), the gates on B3's (or B4's) set 204 clone
  run (13 / 13, on the dev Mac: `injection` and `gain` read bigbase2 / 3), `probe_identity` against the last stage's probe outputs (same), the runner's l10b cycle at the final
  binary (the nebula rows; B1's ERP column). Any gate off its fixed value → STOP, the owner task named.
- [ ] **Step 2 — the real reads (sonnet judgment; ≈ 2 h).** `ln_real.py run / table` at the final binary on the three
  trees against B2's referee cells (B1's beside); `grid-report.py <clone> <project> --ln-truth` on run B's final clone
  (the project path) and `ln_truth_run.py` + `ln_real` on the set runs — the LN scale beside the photometric truth at
  master level, reference pinned; the master rows of the final clones against B4's / B3's as the stage order says.
- [ ] **Step 3 — the suites (haiku, fixed answers; ≈ 45 min).** `cargo test -p athenaeum-core --all-targets --test-threads=4`
  → 0 failed (the documented flakes by name); `cargo test -p athenaeum-web`, `npm run test` (vitest) green; `cargo check
  --no-default-features` green.
- [ ] **Step 4 — the after report and the close (opus for the report; sonnet docs; Fable for the final whole-branch
  review).** The report beside B1's baseline: one data line per table (machine, volume), every bar with its verdict, the
  owned-quantities table (below), the recorded limits with their owners, what Plan C needs. README R-OSP-16 closed; the
  open-items ledger; `CLAUDE.md`. The lab's `results/plan-b/README.md`. The final review's fixes folded. The owner's STOP
  (PB-18): the merge into local main (`git merge --ff-only plan-b` after the owner's word, or a squash as Plan A did),
  the push.

**Acceptance.** Every bar in the after report with its verdict and its before value; suites green; R-OSP-16 complete; the
owner's word on the merge and push.

---

## Spec coverage

| Spec item | Task |
| ---- | ---- |
| §3 F-8 defect and change (re-baseline, the referee, the estimator) l.290 | B1, B2, B3 |
| §3 F-9 both floors l.291; bars l.466–479 | B4 (Step 0 (ii), Step 5 (i)); the owner's verdicts |
| §3 F-11 `c` kept l.293; bars l.480–486 | B0 |
| §3 F-12 the screen and the sample, `MEASURE_VERSION` 5 l.294; bars l.488–513 | B4 |
| §3 F-13 seeds, saturation, nebula sky, R11-3 l.295; bars l.516–527 | B5 |
| the ERP column, the audit, the first ERP runs l.374–384 | B1 (real sets, injected, l10b), B2 / B4 (new frames), PB-6 |
| the 3 % rule l.386–392 | B1 (flag column), B2 (re-check), B3 / B4 (per cell) |
| no-change guards / truth-judged moves l.393–402 | B0, B1, B5 (guards); B3, B4 (truth-judged) |
| F-8 referee l.402–420 | B2 Steps 1–2b, 4 |
| field realism l.422–435 | B2 Step 3 / 3b |
| F-8 estimator l.437–463 | B3 |
| owned-quantities rows `c`, `seeds`, `saturated`; the clip-screen sentence l.555–564 | B0 / B4 (`c`), B5 (seeds, flag, the sentence) |
| §5 Plan B preamble: the data lines, the run order, "a step runs where its data lives" l.695–706 | Data and machines; PB-16, PB-17 |
| §5 B0…B6 l.707–770 | B0, B1, B2, B4, B3, B5, B6 in run order |
| §6 global constraints l.781–797 | Global Constraints |
| §7 the first-run ripple (`MEASURE_VERSION`) | B4 Step 7 (the release note), Review Focus 5 |
| rulings.md R-B1…R-B17, the owner's ERP YES | cited per task; PB-1…PB-18 where the plan is more precise |

## Owned quantities at close (to be filled by B6's report; the owner column as the spec's table says)

| Quantity | Owner | Consumers | Truth bar | Propagation list |
| ---- | ---- | ---- | ---- | ---- |
| `c` (with SE, count, the screen's count) | Task 10 (F-5) → B4 (qualifiers and sample) | PSFSW / PSFSNR, psfSel, Register's and LN's ordering, the kept payload (B0) | F-5; F-12 (B4); F-11 identity (B0) | weights, LN reference composition, psfSel s, inject row 6 (reported) |
| the LN scale s | the default estimator (B3, the owner's) | Normalize, the masters | F-8 estimator (B3) | the master level rows |
| the referee (harness truth, both modes) | B2 (the owner's rule) | every real-data LN verdict | F-8 referee (B2) | B3, B4, B6 |
| seeds (LN's sides) | 10b → B5 (if in scope) | LN's pairs | F-13 | `k` retired |
| `saturated` (LN's one definition) | 8 / 10c → B5 (if in scope) | LN's selection | F-13 | `unverdicted_clip_range` retired |
| LN's aperture-scale sky | F1: LN's own [6F, 8F] median; psfSel: the module's B_i; F1 v2: the module ring at a caller F | the LN scale | F-8 | B3's choice |
