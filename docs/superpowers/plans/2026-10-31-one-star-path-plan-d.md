# One Star-Measurement Path — Plan D (the stacking weight) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: the user skill `lean-execution` (`~/.claude/skills/lean-execution/SKILL.md`)
> above `superpowers:subagent-driven-development`: one phase per agent, a one-page handoff, reports of at most 80 lines,
> one command per lab job printing a verdict of at most 30 lines, `haiku` for proof runs, `sonnet` or stronger for
> judgment runs. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the PSF-family stacking weight reads a frame-common set of unclipped stars, matched across the group after
Register, so that clipped stars and seeing stop driving it; the SNR gain reads the same flux term; `c`'s sample on a
colour plane stops losing its brightest stars to the knot classifier; Blend refusal joins the stacking Measure.

**Architecture:**
- **athenaeum-core `stacking/common_set.rs`** (new, pure): the anchor pick, the anchor's candidates, one member's match
  through its registration map, and the common-set flux of every member (paired ratios). No I/O, no weights.
- **`stacking/run.rs`**: `reweigh_groups` at the end of stage 5 (before both `write_frame_rows` calls, l.9276 and l.9455)
  reads each included, aligned member's `.athf` fits (the LN lookup pattern, l.13060–13088) and its
  `RegisteredFrameOutcome::Aligned.map` (l.3807), overwrites the PSF-family weight inputs in memory (the `take_group_c`
  precedent, `measure.rs:293`), recomputes the weights and applies the weight floor. Everything after stage 5 (frame rows,
  LN reference members, the normalization anchor, integration, drizzle, the best sub) reads the new weight.
- **`stacking/weights.rs`**: the weight gate factored out; stage 3 defers it for the PSF-family modes.
- **`stacking/integrate.rs`**: `paired_snr_gain` and `master_psf_snr` / `best_sub_psf_snr` on the common set
  (`GroupInput.common_stars`); `SNR_GAIN_VERSION` 2. **Frontend** `src/components/stacking/snrGain.ts` only.
- **`star_measure/`** (D2, D3): `c`'s candidate rule on a colour plane, `blend_refusal` on `finish_working`'s fit call
  (the stacking Measure's subs, the master and both SNR-gain crops; PD-10); `MEASURE_VERSION` 8 once.
- No command is added or changed: both hosts keep their surfaces. No TS-exported type gains a field.
- **athenaeum-lab:** one driver `harness/acceptance/pland.sh <job>` (built on `planb.sh`'s b3 / b6 / c10 functions, as
  `planc.sh c10` is) and `star-lab/bin/pland_bars.py`; results under `star-lab/results/plan-d/<job>/`.
- **Machines:** master-level runs and lab cycles on the runner (M4 Max SADM-OBE-47e52, 128 GB, the three trees in its
  `~/.athenaeum-acc`); the offline probe gate (D1a) and the set 109 smoke on the dev Mac (D0's dumps and set 109's lights
  live there). 16 GB dev Mac: one compile, one implementer at a time, builds in `target-agent`, heavy runs through
  `athenaeum-lab/harness/acceptance/heavy_lock.py`.

**Tech stack:** Rust (athenaeum-core), React/TS + vitest (one file), Python 3.14 + numpy (lab, `unittest`).

**Spec:** `docs/superpowers/research/2026-10-31-plan-d-stacking-weight-mechanism.md` (D0; "What the plan should own"),
`.superpowers/sdd/2026-10-31-plan-d/task-D0-report-r1.md`, the brief `task-DP-brief-r1.md` (the controller's ruling),
`docs/superpowers/open-items.md` → "Plan D — carried from Plans B and C" (scope: «Только вес стека»). Binding beside them:
`docs/stacking/README.md` R-OSP-16 B4 (l.3955–4054: `c`'s screen, sample, floor (i)), R-OSP-17 R-PC-10b / R-PC-11
(l.4320–4337), the SNR gain bullet (l.4413).

## Global Constraints

- No third-party program name anywhere in this repository; the comparison program is "the ERP". Lab automation lives in
  athenaeum-lab; probes stay in core `examples/`.
- Never read or write an owner catalog except `sqlite3 -readonly`; the set 109 smoke runs on a copy
  (`prepare-catalog.sh`) whose stacking working and output folders point under `~/.athenaeum-acc/plan-d/`.
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `stacking/measure.rs`,
  `stacking/register/detect.rs`; no `cargo fmt`. `rustfmt` only on `stacking/common_set.rs`.
- Cargo: `CARGO_TARGET_DIR=target-agent` for agents, `.superpowers/target-acc` for the lab's release builds;
  `CARGO_BUILD_JOBS=4`, `--test-threads=4`; `cargo test` takes ONE positional filter.
- `tracing` only; never swallow an error (log, then fall back); a fallback is a `warn!` plus a run warning. New log field
  names (`anchor_frame_id`, `common_stars`, `paired_stars`, `fallback` in D1w, with the dictionary's `reason` extended
  to D1w's fallback warn; `rescued` in D2) go into the dictionary of
  `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the task that adds them.
- `ChannelMeasurement`'s new fields are `Option` with `skip_serializing_if = "Option::is_none"`: a `metrics` payload
  written at Measure stays byte-identical; the fields live in memory and in `stacking_run_frames.metrics_json` only.
- Plan by measured quantity: the star flux (model × `c`) has one owner, Measure; the common set and the weight's flux
  term have one owner, D1. A second estimate of a star's flux anywhere in the stacking path is a STOP and a re-audit.
- The new path mirrors the proven path's flag handling: `StarFlags::in_statistic()` (not saturated, not extended) for
  "usable", the floor-(i) stand-in `aperture_correction_group` applied exactly as `take_group_c` applies it, the PB-12'
  match radius, `CORRECTION_ISOLATION_PER_FIELD_FWHM` for isolation, `CORRECTION_MIN_SNR` for the SNR floor.
- `MEASURE_VERSION`: ONE bump for Plan D (`star_measure/mod.rs:183`'s policy), 7 → 8, in the first of D2 / D3 that
  lands; the other rides it (PD-7's ruling: one bump, one release). Every lab run between them measures fresh (rerun
  from `measure`); no stack runs on the dev catalog between D2 and D3.
- A missed bar goes to a fresh `fable` agent with the numbers and the options; its verdict, after a truth check on
  existing data, is the ruling; the owner is informed. Push, release and spending stay the owner's.
- Branch `plan-d` from `main` at the commit holding D0's files; pushed to GitLab for the runner (never `main`); merged into
  local `main` after a clean final review; `main` pushed only on the owner's word. Lab work committed and pushed in
  athenaeum-lab `main` after each lab step.
- Commits authored by `eg013ra1n`, every message ending with the harness's `Co-Authored-By` and `Claude-Session` lines.
- Disk (`lean-execution` §7): every clone, catalog copy and dump under `~/.athenaeum-acc/plan-d/` (dev Mac) or the
  runner's `~/.athenaeum-acc/pland-*` with a REGISTRY row; `df` ≥ the job's estimate + 150 GB before each heavy job; a
  stage's clones are deleted once its compare is committed, except a comparator: D-before's and any stage a later stage
  names as `--base` (d1 for D2 / D3, d2 for D3) — `clean <stage>` strips those to `athenaeum.db`, `stackout/` and the truth CSV
  (planb.sh:2097 needs both clones' `athenaeum.db`) and DF deletes them at the close. `disk-report` at each task's start and close.
- Every test a task names is written with its assertions; a test that cannot fail before its change is a pin, outside
  the RED step. A moved existing test is listed with its reason; a move whose assertion is not the weight's old FWHM or
  clipped-star dependence is a STOP (memory `feedback_sweep_tests_for_invalidated_invariants.md`).

## The D bars (master level; `pland_bars.py`, every task that runs `pland.sh ln-truth` reads them)

| bar | definition | source of the number |
| ---- | ---- | ---- |
| B-LN | GROUPS WORSE 0, per group-plane: WORSE when the median over members of (\|gap after\| − \|gap before\|) exceeds max(0.02 pp, min(2 SE, 0.05 pp)), SE = 1.2533 · 1.4826 · MAD / √n of those paired differences; n, MAD, SE and the strict count (`planb-b3-tools.py`'s `>`) reported per group-plane | R-PC-11 (b); PD-8 |
| B-M | masters per plane: noise ratio ≤ 1.02, FWHM ratio ≤ 1.01; median / MAD reported | R-PC-11 (b)'s noise / FWHM tolerances |
| B-W | weights ρ ≥ 0.9 per group against the comparator, on the frame weight (OSC: the frame weight, as B-S; D0's (b) reads 0.925 / 0.931 on the OSC frame weight but 0.80 / 0.89 on the G / B planes); below → STOP to Fable | R-PC-11 (b); D0 Q3: 0.925–0.979 |
| B-S | the sharpest decile (10 % lowest frame FWHM, ≥ 1 frame): median and worst weight rank ≤ the comparator's, per group (OSC: the frame weight) | D0 Q3: (b) never lower, every group |
| B-R | the C10 r1 frames on hc11 L (69976, 69705, 70072, 69860, 69865, 69947, 69949, 69859): ranks reported; under D3 each moves ≤ 5 ranks | C10F, R-PC-10b |
| B-E | measure-equal (the `metrics` payloads): EQUAL when the task moves no Measure number (D1), REPORT otherwise | R-PC-11 (b) |

## Review Focus

1. **A group with no usable common set** (a sparse narrowband field, a field whose bright stars all clip, 3 frames on
   a nebula). Expected: that (group, plane) keeps its Measure-time weights, one `warn!` and one run warning, the run
   stacks. Pinned in D1 (`under_twenty_common_stars_there_is_no_common_flux`) and D1w
   (`a_group_without_a_common_set_keeps_its_measure_time_weights_and_warns`).
2. **A cloudy frame** (transparency × 0.5, a third of its stars lost). Expected: its weight falls by its transparency
   over the stars it kept, never to zero unless no common star survives. Pinned in D1
   (`a_cloudy_frame_weighs_by_its_transparency_on_the_stars_it_kept`).
3. **A meridian-flipped frame and a frame with distortion.** Expected: the match goes through the frame's own map
   (`PixelMap::forward_exact`), pairs found as on an unflipped frame. Pinned in D1
   (`the_match_goes_through_a_flipped_registration_map`).
4. **`PsfSnr` mode on a wide seeing spread** (set 109: PSF SNR ×69 from the sharpest to the widest frames). Expected:
   the sharpest frames are not dropped by the 5 % floor before Register and end on top. Pinned in D1w
   (`the_weight_floor_runs_on_the_common_set_weight_under_psf_snr`).
5. **A re-run from Normalize** (cached metrics, fits and registration rows). Expected: the reweigh reads the cached
   `.athf` and maps and gives the full run's weights bit for bit; an old run's frame row (no `commonTflux` key) and a
   row with a `NULL` weight (PD-6) decode.
   Pinned in D1w (`a_rerun_from_normalize_reweighs_to_the_same_weights`, `a_frame_row_written_before_plan_d_decodes`).

## Rulings fixed by this plan

Recorded in `.superpowers/sdd/2026-10-31-plan-d/rulings.md` at D-1 and in `docs/stacking/README.md` R-OSP-18 at DF.

| id | ruling |
| ---- | ---- |
| PD-1 | **The loop rule.** The Measure-time weight (today's formula) drives everything before the common set exists: stage 3's selection, the run reference (stage 4) and the two-pass pick. The common set is built after the persisting registration pass; the common-set weight drives everything after it and feeds nothing upstream. No iteration. The common set's flux is a paired ratio to a group member picked without any weight (PD-3), normalized to the group maximum, so the final weight depends neither on itself nor on which frame is the registration reference. LN's per-frame scale is not read: its reference is the top-5 by weight (the loop), it exists only with local normalization on, and it arrives in stage 6, after its first consumer. |
| PD-2 | **The matching seam.** End of stage 5, in `run.rs::reweigh_groups`, from the `.athf` fits Measure persisted (x, y, signal = model × `c`, area, flags; `fits_artifact.rs`) mapped through each member's stored registration map; no pixel is read. Pure logic in `stacking/common_set.rs`. |
| PD-3 | **The common set, per (group, plane).** Anchor: among the included, aligned members whose summed `stars_unsaturated` is at least the group median, the lowest `FrameMeasurement::mean_fwhm_px` (ties: lower `frame_id`); one anchor per group. Candidates: anchor fits with `in_statistic()`, finite positive `signal` and `area`, no other anchor fit within `CORRECTION_ISOLATION_PER_FIELD_FWHM` (6) × the anchor plane's `fwhm_px`, and `signal / (noise · √area)` ≥ `CORRECTION_MIN_SNR` (20; `.athf` stores no SNR, so the background-limited aperture SNR stands in). Match: a member fit within max(`POINT_SOURCE_MATCH_PX`, `POINT_SOURCE_MATCH_PER_FIELD_FWHM` × max(F_anchor, F_member)) of the candidate in group coordinates, unique (two within the radius = unmatched in that frame); usable = matched and `in_statistic()`. Membership: usable in ≥ 90 % of the included aligned members; at least 20 members of the set (`COMMON_MIN_STARS`), else no common flux. Flux, native units (÷ `ADU_SCALE`), each member signal × its plane's `aperture_correction_group.unwrap_or(1.0)`: over the stars usable in member f, r_f = Σ signal_f / Σ signal_anchor and q_f = Σ mean_f / Σ mean_anchor (mean = `StarFit::mean_flux`); `common_tflux` = r_f · Σ_S signal_anchor, `common_tmean_flux` = q_f · Σ_S mean_anchor; 0 usable stars → 0. The set differs from D0's (b) three ways: the anchor (not the registration reference), isolation 6 F (not 2 R), and the SNR proxy, which omits the shot term and so admits fainter stars at 20 than the dump's SNR; D1a bars the drift (ρ, the drift slope, the common-flux noise; PD-16 — (b) also ignores `extended`, a fourth difference). |
| PD-4 | **Model × `c`, not an aperture flux.** Measured reasons (D0): on mono and OSC G / B the common set's model × `c` reads the fixed aperture within +1.0…+1.7 %/px of FWHM (Q2) and `c` within −1.6…+2.3 %/px; the sharpest decile ties or beats (b-ap) on hc11 and mono (hc11 worst rank 8 vs 10); the two differ only on OSC (worst 29 vs 21 / 23), traced to `c` on R (Q1), which D2 owns, and the R aperture itself misses the β ≈ 1.5 wings. The star flux has one owner (Measure); an aperture flux would be a second estimate of it. Reversal bar (D2): after D2, the OSC sharpest decile's worst rank ≤ (b-ap)'s offline value + 5 (D1a reports it); a miss → Fable, the aperture option on the table. |
| PD-5 | **The weight floors.** Under `PsfSignalWeight` / `PsfSnr`, stage 3 refuses only a weight ≤ 0, with the reason "weight 0: no PSF signal" (the fraction text would be wrong under the deferral); `reweigh_groups` applies `min_weight_fraction` to the common-set weight and never re-admits a frame excluded before Register. One later floor with two readers takes the new weight through `StackFrame.weight`, unchanged: `IntegrationConfig.min_weight` (0.005 on the LOWEST per-channel normalized weight, the reference always kept; integrate.rs:49–68), expressed only in `included_after_min_weight` (l.1122–1137) and read by integration and by LN's member list, built "after the weight-floor drop" (ln/reference.rs:83). D1m reports both drops. Measured reason: set 109's PSF SNR spans ×69 (24.5 → 1686), so under `PsfSnr` the provisional 5 % floor drops the sharpest frames. Other modes: stage 3 unchanged, no reweigh. |
| PD-6 | **Fallback per (group, plane), never per frame.** Fewer than 20 common stars, or an included aligned member without a fresh readable `.athf` for the plane: every member's plane keeps its Measure-time weight inputs; one `warn!` naming the reason and a run warning. One scale per frames table: a frame of a reweighed group that `reweigh_groups` did not reweigh (excluded at stage 3, or at Register on a registration failure, run.rs:8488–8495) gets `weight = None`, because its Measure-time value is normalized to another maximum and `FramesTable.tsx:244, 395` would render it beside the common-set weights; its row's `exclusion_reason` already says why. A group whose every plane falls back keeps its Measure-time weights (its rows stay on stage 3's one scale), except that the deferred floor still runs: `compute_weights` + `weight_gate` on its Measure-time inputs, so a sub-5 % frame is excluded with the fraction text. |
| PD-7 | **Invalidation.** D1: no `MEASURE_VERSION` bump, no stage-hash or artifact-version move (the `metrics` payload and `.athf` are untouched; integration is never cached); LN's reference hash folds the reference members' registration and measurement hashes, keyed by the member LIST (run.rs:12749–12805), so a changed list rebuilds LN by itself, and an unchanged top-5 may reuse LN: its reference is integrated at equal 1.0 weights (ln/reference.rs:13–14), so the weight values never enter it; old runs keep the weights they were integrated with. D1b: `SNR_GAIN_VERSION` 2; a v1 group shows "SNR gain —" with "measured with an older formula — re-run to get the SNR gain". D2 / D3: `MEASURE_VERSION` 8 once; the `plan.rs` goldens (l.8487 stage hashes: meas, reg, ln_ref, ln, ln_cal move, calib does not; l.8703 the pass-1 key) re-recorded; the first stack after the update re-measures, re-registers and re-normalizes every set once, never re-calibrates. **Ruling (2026-11-01, the plan's): D2 and D3 ship under that one bump, in one release** — the policy is one bump per merged plan (`star_measure/mod.rs:183`), and two releases would re-measure every set twice for one change of what the stacking Measure reads. Between D2's and D3's commits v8 means "D2 without Blend", Plan C's "6 is burnt" window (mod.rs:174–177): no v8 artifact written before D3's commit survives into a D3 measurement — `clean d2` drops D2's working folders first (its comparator kit stays for `--base d2`), D3 runs on fresh clones, nothing stacks on the dev catalog between the two commits. |
| PD-8 | **B-LN with a paired noise allowance, capped.** WORSE when the paired median exceeds max(0.02 pp, min(2 SE, 0.05 pp)); n, MAD, SE and the strict count reported per group-plane. Measured reason: C10 r1 counted hc11 L WORSE at 0.187 → 0.191 % and s204 mono at 0.554 → 0.555 % (`star-lab/results/plan-c/c10-r1-v6/*/ln-members.txt`); a deliberate weight change moves LN's reference members, so the strict `>` counts noise. The cap: SE grows with the MAD of the paired differences, so an uncapped 2 SE lets a noisy group widen its own bar. On C10 r1's own per-member gaps every group-plane replays NOT WORSE under this rule (s204 OSC p0: paired median −0.022 pp, 2 SE 0.043; its +0.04 pp is the pooled-median shift, not the paired median); D-1 replays it as the tool's calibration, and DF reports s204 OSC p0's paired median beside C10 r1's. |
| PD-9 | **D2's lever**: on every plane of a 3-plane colour image, an `extended` `c` candidate re-enters `c`'s list when a seed of another plane that is NOT extended on its own plane lies within the PB-12' radius. Measured reason: OSC R loses 85–100 % of its brightest 64 to the knot flag in every frame (`c` sample depth 219–222 vs 65); G loses ≥ 50 % on 7 of ~100 frames (`c` +20 %), the same mechanism off the β floor. The frame statistic and the β sample still leave extended fits out. Mono has no sibling: hc11 L's 2024-09-08 night (69696–69716, extended share of the brightest 64 0.44–0.86 over its 8 included frames (69705 the low one), `c` +7 %, so +14 % weight on 8 of 73 frames) is a **recorded limit with a known cause** — the owner (2026-11-01): the C11's collimation was off and there was wind that night. The `extended` verdicts there are the optics' and the wind's real non-point-like profiles, not the estimator's, and `c` +7 % is the correct correction for light outside the model's FWTM, not a defect. It moves nothing: under D0's (b) those 8 frames rank 57–70 of 73 (r_sig 0.91–0.997, FWHM 7.9–8.1 px; `~/.athenaeum-acc/plan-d/d0/probe/hc11/mono__L__bin1__180s.p0.frames.tsv`), 60–72 without the +14 %. No look at a flagged star is owed. |
| PD-10 | **D3's scope**: `blend_refusal: true` on `finish_working`'s fit call (`star_measure/mod.rs:1519`), which every `measure_plane` consumer under `Mode::All` / `Brightest` / `Population` reaches (mod.rs:590): the stacking Measure's subs, the master measurement (integrate.rs:1749–1754, FM-28), both crops of `paired_snr_gain` (integrate.rs:787–803) and the lab's `Population` probes. The master and the gain's crops take it too, no flag threaded through `MeasureOptions`: the master FWHM / eccentricity and the gain are read beside subs measured under the same accept rule. So the master FWHM / eccentricity, `master_psf_snr` and the SNR gain move under D3, not only the subs. LN's refits (`FitStarsOptions::correction`, mod.rs:413) stay without it; mono registration inherits it through Measure's fits (`stars_from_fits`). |
| PD-11 | **The registration reference stays on the Measure-time weight** (ruling 2026-11-01, the plan's; a recorded limit). Reason: the program goal (owner 2026-10-06) — registration is fine and stays; the reference sets the output pixel grid and the two-pass pick only, while the normalization anchor, LN's members, drizzle and the best sub follow the new weight; moving it would re-register every set once. D1m and DF report its FWHM rank in its group. |

## Tasks at a glance

| task | job | kind | model | depends on |
| ---- | ---- | ---- | ---- | ---- |
| D-1 | ledger, rulings, commits of D0's files, branch, the lab driver, D-before on the runner | lab | sonnet | — |
| D1 | `stacking/common_set.rs` + the probe arm + the offline gate D1a | code + lab | rust-engineer (sonnet); lab sonnet | D-1 |
| D1w | the reweigh in the run, the floor deferral, logs | code | rust-engineer (sonnet) | D1 |
| D1b | the SNR gain and the PSF SNRs on the common set, `snrGain.ts` | code | rust-engineer (sonnet); frontend-dev | D1w |
| D1m | master-level D bars for D1 on the runner (checkpoint) | lab | sonnet | D1b, D-before |
| D2 | `c`'s colour sample (PD-9), `MEASURE_VERSION` 8 | code + lab | rust-engineer (sonnet); lab sonnet | D1m |
| D3 | Blend on the stacking Measure | code + lab | rust-engineer (sonnet); lab sonnet | D2 |
| DF | final runs, set 109 smoke, suites, docs, review, merge | close | sonnet; final review fable | D3 |

Serial. D-before runs on the runner while D1 / D1w are written on the dev Mac. Reviews: `opus` per code task.

---

### Task D-1: Ledger, branch, the lab driver, D-before

**Files:** `.superpowers/sdd/2026-10-31-plan-d/{progress.md,rulings.md}`; athenaeum-lab `harness/acceptance/pland.sh`,
`star-lab/bin/pland_bars.py`, `star-lab/tests/test_pland_tools.py`.

- [ ] **Step 1 (controller):** commit D0's research doc, `examples/d0_weight_probe.rs` and the open-items edit on `main`;
  `git switch -c plan-d`; write `rulings.md` (PD-1…PD-11 copied); push `plan-d` to GitLab.
- [ ] **Step 2 (lab agent, tests first):** `test_pland_tools.py`: `test_paired_worse_ignores_a_shift_inside_two_se`
  (100 members, |gap| after = before + N(0, 0.05 pp) → NOT WORSE; + 0.10 pp → WORSE; strict count reported);
  `test_paired_allowance_is_capped` (100 paired differences symmetric about +0.06 pp with MAD 0.4 pp, so 2 SE ≈
  0.15 pp → WORSE, where the uncapped rule says NOT WORSE; n, MAD, SE reported);
  `test_sharpest_decile_ranks` (12 frames, FWHM and weights given → median 2, worst 3; OSC frame weight = mean of the
  per-plane normalized weights); `test_bars_read_the_compare_files` (masters / weights / ln-members fixtures → B-M,
  B-W on the OSC frame weight, B-LN lines).
- [ ] **Step 3:** `pland_bars.py` (B-LN, B-M, B-W, B-S, B-R, B-E over one dataset's compare folder against a named
  comparator stage); `pland.sh` jobs, each `PLAND <JOB>: PASS|FAIL|STOP (...)`, under the heavy lock:
  `common <sha>` (dev Mac, D1a), `before <sha>` (runner: `planb.sh clone` + `run` + `truth` per dataset at stage
  `dbase`), `ln-truth <stage> <sha> [--base <stage>]` (runner: clone, run, truth, `planb-b3-tools.py` compare, then
  `pland_bars.py`; default base `dbase`), `cycle <stage> <sha> --prev <stage>` (runner: `lab_cycle.py` full manifest +
  inject, with `planb_b6.py`'s truth bars), `ctable <stage>` (B6's pattern, planb.sh:2357: the stage's l10b probe, then
  `planb.sh b5 ctable` with `PLANB_B5_L10B_ROOT` there), `set109 <sha>` (dev Mac), `report`, `clean <stage>`.
- [ ] **Step 4 (runner):** `zsh pland.sh before <base-sha>` on hc11 / runB / s204, then `zsh pland.sh cycle dbase
  <base-sha> --prev c10-final` (the c10 scorecard brought over from the dev Mac through Universe; without it the dbase
  cycle is the base, recorded). REGISTRY rows for the three `pland-*-dbase` clones.

**Acceptance:** lab tests green; `pland_bars.py` B-LN replayed on `star-lab/results/plan-c/c10-r1-v6/*/ln-members.txt` reads GROUPS
WORSE 0 (C10 r1's paired medians, s204 OSC p0 −0.022 pp: the tool's calibration); `PLAND BEFORE: COMPLETE` (three runs, three truth CSVs, weights and masters readable);
the dbase cycle's truth bars PASS (noreg vs c10-final moved 0 when the scorecard is there). Budget ≤ 120 calls.

---

### Task D1: The common set (core) and the offline gate D1a

**Coupling checklist:** reads the star flux (Measure: model × `c`, the floor-(i) stand-in), the flags (F-3 / F-10), the
field FWHM and the plane noise; estimates none of them. Rulings it may revisit: PD-3 (only through Fable).

**Files:** Create `crates/athenaeum-core/src/stacking/common_set.rs` (registered in `stacking/mod.rs`); modify
`crates/athenaeum-core/examples/d0_weight_probe.rs` (the `common` arm).

**Interfaces (produces):**
```rust
pub const COMMON_MIN_STARS: usize = 20;
pub const COMMON_MIN_PRESENCE: f64 = 0.9;
pub struct MemberPlane<'a> { pub fits: &'a [StarFit], pub map: &'a PixelMap, pub fwhm_px: f64,
    pub noise: f64 /* native */, pub c_stand_in: f64 /* aperture_correction_group or 1 */ }
pub struct Candidate { pub x: f64, pub y: f64, pub signal: f64, pub mean: f64 } // group coords, native
pub struct Usable { pub signal: f64, pub mean: f64 }                             // native, × c_stand_in
pub struct CommonFlux { pub tflux: f64, pub tmean_flux: f64, pub paired: usize } // native
pub enum CommonOutcome { Measured { positions: Vec<(f64, f64)>, per_member: Vec<CommonFlux> },
    TooFewStars { stars: usize } }
pub fn pick_anchor(mean_fwhm: &[f64], unsaturated: &[usize], frame_ids: &[i64]) -> Option<usize>;
pub fn candidates(anchor: &MemberPlane<'_>) -> Vec<Candidate>;
pub fn match_member(cands: &[Candidate], member: &MemberPlane<'_>) -> Vec<Option<Usable>>;
pub fn common_flux(cands: &[Candidate], usable: &[Vec<Option<Usable>>], anchor: usize) -> CommonOutcome;
```
`usable[k]` is member k's `match_member` result (the anchor included, in member order); `positions` are the set's
candidates in group coordinates (D1b reads them). Maps go through `PixelMap::forward_exact` (geometry/pixel_map.rs:907;
no grid build); matches through `geometry::kdtree::KdTree2::within` (geometry/kdtree.rs:69). One member's fits resident at a time.

- [ ] **Step 1: the failing tests** (`common_set.rs` `#[cfg(test)]`, synthetic `StarFit`s on a 2000 × 1500 field, 300
  stars, fluxes log-uniform over ×1000, identity maps unless said):
  - `a_common_set_flux_follows_transparency_not_width`: 6 members, transparency t = [1, 0.95, 0.9, 1, 0.85, 1], FWHM
    (area ∝ FWHM²) = [2, 2.5, 3, 3.5, 4, 4.5]; the brightest stars flagged `SATURATED` where peak > clip (the sharp
    members clip more), every unsaturated star present → `common_tflux` / anchor's = t within 1e-12 and
    `common_tmean_flux` / anchor's = t · A_anchor / A_k within 1e-12; anchor = member 0.
  - `clipped_and_non_member_fits_never_move_a_frames_common_flux` (the C10 r1 replay as a unit): drop member 3's
    saturated fits and every fit not in the set → its `CommonFlux` identical bit for bit.
  - `a_star_unusable_in_more_than_a_tenth_of_the_members_leaves_the_set`: 10 members, star j saturated in 2 → out; in 1 →
    in, and member with it saturated pairs without it.
  - `the_anchor_is_the_sharpest_member_with_at_least_the_median_star_count`: the sharpest has the fewest unsaturated
    stars → the next sharpest is picked; ties → lower `frame_id`.
  - `a_floored_plane_takes_its_group_c_in_the_common_flux`: member 2 `c_stand_in` 1.3 → its r_f × 1.3.
  - `an_ambiguous_match_is_not_paired`: two member fits within the radius of one candidate → `None` for it.
  - `under_twenty_common_stars_there_is_no_common_flux`: 19 → `TooFewStars { stars: 19 }`.
  - `a_cloudy_frame_weighs_by_its_transparency_on_the_stars_it_kept`: member at t 0.5 with a random third of its fits
    removed → r_f = 0.5 within 1e-12, `paired` = two thirds of the set ± 1.
  - `the_match_goes_through_a_flipped_registration_map`: member fits rotated 180° about the centre and shifted (17.3,
    −8.6), its map taking them back to group coordinates → every candidate paired; a map with a TPS layer → same pairs.
  - `candidates_are_isolated_unflagged_and_above_the_snr_floor`: an `EXTENDED` fit, a fit with a neighbour at 5.9 F,
    and one at SNR 19.9 (the proxy) are out; neighbour at 6.1 F in.
  - `common_weights_are_scale_invariant`: every signal and noise × 65535 → identical r_f, q_f.
- [ ] **Step 2:** `cargo test -p athenaeum-core stacking::common_set` → FAIL (module missing).
- [ ] **Step 3:** implement the interfaces per PD-3. Signals ÷ `stacking::measure::ADU_SCALE` before any sum.
- [ ] **Step 4:** the tests PASS; `rustfmt` the new file; commit `feat(stacking): the frame-common star set (Plan D D1)`.
- [ ] **Step 5 (probe):** `d0_weight_probe common --dumps ~/.athenaeum-acc/plan-d/d0/probe/<ds> --clone
  ~/.athenaeum-acc/planb-<ds>-c10`: reads D0's per-star dumps (x, y, signal, area, snr, flags, apFixed) and the clone's
  `registration_results.transform_json` (`PixelMap::from_json`, geometry/pixel_map.rs:944), and the stored σ, M*, FWHM;
  runs the product functions; writes per group-plane: set size, the anchor, the share of D0's (b) set the product set
  recovers (matched in group coordinates), the proxy-only share of the product's candidates (proxy SNR ≥ 20, dump SNR
  < 20); per frame: the product weight, D0's (b) weight (reference anchor, dump SNR ≥ 20, the 2R isolation, recomputed
  side by side), (b-ap); the C10 r1 replay. Ranks over included frames only: D0's `summary-all.txt` hc11 line "w_b …
  top 69696 fwhm 8.14" is a print artefact (69696 is `included 0`, w_b NaN; the real top is 69988, 5.61 px), and no
  report or the after note repeats it. Commit `feat(probe): the common-set arm (Plan D D1a)`.
- [ ] **Step 6 (lab agent, dev Mac): `zsh pland.sh common <sha>`** on hc11 / runB / s204 and the β 3.07 s204 subset.

**Acceptance (D1a):** per group: ρ(product, D0 (b)) ≥ 0.99; sharpest-decile median / worst rank within ±1 of D0's (b);
the C10 r1 replay moves 0 ranks; set size ≥ 100 per group-plane (D0: 371–1 328); **S1 drift** per group: |OLS slope of
ln(w_product / w_b) on frame FWHM| ≤ 4 %/px, SE reported; **S2 noise** per group-plane: median σ(r_f)/r_f ≤ 2 %, p90 ≤ 4 %,
max reported (PD-16, fable 2026-11-01: they replace "≥ 90 % of (b)'s set recovered" and "proxy-only ≤ 15 %", which test
agreement with a set PD-3's rules forbid; those shares are printed as notes); at β 3.07 the sharpest decile ranks 1–2 of 24; OSC (b-ap) worst rank recorded for PD-4. A miss →
Fable (options: the registration reference as anchor where it is a member; D0's isolation radius; a higher proxy
floor). Budget ≤ 150 calls.

---

### Task D1w: The reweigh in the run

**Coupling checklist:** consumes D1; reads the stored registration maps (stage 5) and fits (stage 3); changes only
in-memory weight inputs and the weights. Re-verifies: LN member choice and the normalization anchor follow it.

**Files:** Modify `stacking/run.rs` (`measure_group_finish` l.5865, `stage_register` before l.9276 and l.9455, new
`reweigh_groups`), `stacking/weights.rs`, `stacking/measure.rs` (`ChannelMeasurement` l.181); the logging dictionary.

**Interfaces:**
- Consumes: D1's `pick_anchor`, `candidates`, `match_member`, `common_flux`.
- Produces: `ChannelMeasurement.{common_tflux: Option<f64>, common_tmean_flux: Option<f64>, common_stars: Option<usize>}`
  (camelCase on the wire); `weights::weight_gate(w: &FrameWeight, cfg: &SelectionConfig) -> Option<String>` (today's
  reason text, factored out of `select_frames`); `weights::defers_weight_gate(mode: WeightMode) -> bool`;
  `weights::select_frames_deferring_weight_gate(inputs, weights, manual, cfg) -> Vec<Option<String>>` (the weight gate
  refuses only `normalized_mean <= 0`, reason "weight 0: no PSF signal"); `RunContext.common_stars: HashMap<String,
  Vec<Option<Vec<(f64, f64)>>>>` (group key → per plane, D1b reads it); `fn reweigh_groups(rc: &mut RunContext) ->
  Result<(), RunError>`.

- [ ] **Step 1: the failing tests.** `weights.rs`: `the_deferred_selection_keeps_a_low_weight_frame_but_refuses_a_zero_one`
  (0.01 kept, 0.0 refused with "weight 0: no PSF signal"); pin `weight_gate_keeps_its_reason_text` (the non-deferred
  text, "weight 0.020 below 0.05 of the group maximum", only).
  `run.rs`. The fixture follows `seed_sky_penalty_group` (run.rs:27750: `frame_set_with_conn`, `add_light_with_field`
  per frame, the working / output settings) with a per-frame width: `write_light_fits_field` renders at a fixed σ 1.6
  (test_fixtures.rs:625), so a new `test_fixtures::add_light_with_field_fwhm` takes the FWHM and converts it as
  `synthetic_star_field` does (test_fixtures.rs:771, σ = FWHM / `GAUSSIAN_FWHM_FACTOR`); bright stars clip at the u16
  clamp (l.640). Driven by `run_stages_for_test(.., Stage::Register)` (run.rs:16785); the re-run test uses two run
  contexts on one DB (run.rs:17062 / 17084).
  - `psf_weights_are_read_on_the_frame_common_set_after_register`: 6 frames, frame 5 soft (FWHM ×2) with its 3 brightest
    stars unclipped (they clip in the others): before, frame 5 tops the weights; after, the sharpest frame does;
    `metrics_json` carries `commonTflux`, `commonTmeanFlux`, `commonStars`; the `metrics` artifact payload is
    byte-identical to a run without the reweigh.
  - `the_weight_floor_runs_on_the_common_set_weight_under_psf_snr`: `PsfSnr`, the sharpest frame's provisional weight
    0.02 of the max → registered, kept, top weight.
  - `a_group_without_a_common_set_keeps_its_measure_time_weights_and_warns` (10 stars in the field): weights equal the
    Measure-time ones; the warn `"weights: no frame-common star set"` with `group_key`, `plane`, `common_stars`,
    `fallback = "measure_time"`, `reason`; one run warning; a seventh frame at 1 % of the top Measure-time weight is
    still excluded by the deferred floor, with the fraction text (PD-6).
  - `non_psf_weight_modes_are_untouched_by_the_reweigh` (`Noise`, `Exposure`): weights and frame rows identical to a run
    at the base.
  - `the_reweigh_never_readmits_a_frame_excluded_before_register` (and its frame row's `weight` is `NULL`, PD-6) and
    `the_reweigh_leaves_the_registration_reference_and_rows_unchanged`.
  - `a_rerun_from_normalize_reweighs_to_the_same_weights` (bit for bit) and `a_frame_row_written_before_plan_d_decodes`
    (a row without `commonTflux` and a row with a `NULL` weight).
- [ ] **Step 2:** run `cargo test -p athenaeum-core stacking::` → the new tests FAIL; record every OTHER failing test.
- [ ] **Step 3:** `weights.rs` helpers; `measure_group_finish` calls `select_frames_deferring_weight_gate` when
  `defers_weight_gate(cfg.measurement.weight_mode)`. `reweigh_groups`, for each plan group under a PSF-family mode, per
  plane: the included members with `RegisteredFrameOutcome::Aligned` and a measurement; the anchor by `pick_anchor`;
  each member's `.athf` by `find_artifact` + `plan::is_fresh` against `group_measurement_hash` (run.rs:6694; the LN
  pattern l.13060–13088); `candidates` → `match_member` per member (fits dropped after) → `common_flux`. On `Measured`:
  per member plane set the three fields and overwrite `psf_signal_weight` / `psf_snr` by
  `psf_signal::{psf_signal_weight, psf_snr}` (psf_signal.rs:81, 94) on (`common_tflux`, `common_tmean_flux`, `noise`,
  `m_star`), all native; store `positions` in `rc.common_stars`. On a fallback (PD-6) the plane keeps its Measure-time inputs. Either way, then
  `compute_weights` over those members (excluded all false), `weight_gate` → `included = false`, reason, the stage-3
  `"frame excluded"` warn; every other frame of the group gets `weight = None`, unless every plane fell back (PD-6);
  refresh `rc.summary.reference.weight` when the reference is a member. Fewer than 3 left: the
  existing output guard (run.rs:10793) skips the group. `rc.check_cancel()` per group. Logs: info `"weights:
  frame-common star set"` (`run_id`, `group_key`, `plane`, `anchor_frame_id`, `common_stars`, `count`), debug `"frame
  reweighed"` (`frame_id`, `plane`, `paired_stars`, `weight`), the fallback warn above (`reason`: under 20 common
  stars, or the member without a fresh `.athf`).
- [ ] **Step 4:** tests PASS; every other moved test listed with its reason (a weight-order assertion on a fixture whose
  frames differ in width or clipping); `cargo check -p athenaeum-core --no-default-features`.
- [ ] **Step 5:** dictionary entry; commit `feat(stacking): the weight reads the frame-common star set (Plan D D1)`.

**Acceptance:** tests green; no stage-hash golden moved; the moved-test list reviewed. Budget ≤ 150 calls.

---

### Task D1b: The SNR gain and the PSF SNRs on the common set

**Files:** `stacking/integrate.rs` (`GroupInput` l.433, `integrate_group` l.1784–1814, `paired_snr_gain` l.723,
`SNR_GAIN_VERSION` l.503), every `GroupInput {` site (28, recounted at 0769aa7c: integrate.rs 18, `ln/reference.rs` 6, run.rs 2, examples
`integrate_probe.rs` / `ln_probe.rs`; `common_stars: None` except run.rs's integrate call, which passes
`rc.common_stars[group]`); `src/components/stacking/snrGain.ts` and `snrGain.test.ts`; README's SNR-gain bullet at DF.

**Interfaces:** `GroupInput.common_stars: Option<&'a [Option<Vec<(f64, f64)>>]>` (per plane, the delivered grid's
coordinates); `fn common_psf_snr(fits: &[StarFit], positions: &[(f64, f64)], origin: (usize, usize), noise_native:
f64, field_fwhm: f64) -> Option<(f64, usize)>` (PSF SNR over the positions matched in the crop at `origin` with
`in_statistic()`, signals ÷ `ADU_SCALE`; `None` under `COMMON_MIN_STARS`).

- [ ] **Step 1: the failing tests.** `snr_gain_of_n_equal_frames_is_about_root_n` (l.2551) passes the fixture's common
  set and keeps its bound; `the_paired_gain_reads_only_the_common_stars` (the best sub with one extra clipped bright
  star outside the set → the gain within 1e-9 of the run without it); `snr_gain_does_not_grow_with_the_best_subs_width`
  (best sub FWHM ×1.6 with more unclipped flux → gain within 5 % of the sharp-sub case; today's figure moves > 20 %);
  `without_a_common_set_the_gain_is_zero_with_a_warn`; `snrGain.test.ts`: v2 shows the figure, v1 "—" with the
  older-formula tooltip, v0 its v0.7.0 tooltip, a v2 gain of 0 "—" with "not measured for this group".
- [ ] **Step 2:** run them → FAIL.
- [ ] **Step 3:** `paired_snr_gain` keeps its warp and rectangle; on both measured crops it pairs the common positions
  (minus `r.x0, r.y0`) in each crop's fits — `PlaneMeasurement.fits` (measure.rs:436) of every `MeasuredPlane`
  `measure_planes_diagnosed` returns — a star counts when matched and usable in BOTH — and reads `common_psf_snr`'s
  power on that pair; the gain is √ of the ratio; fewer than 20 paired or no positions → 0.0 and a warn.
  `master_psf_snr` reads `common_psf_snr` on the master's own `PlaneMeasurement.fits` (origin = `measure_rect`'s
  `x0, y0` when the master is cropped, integrate.rs:1747, else (0, 0)) when positions exist, else today's value;
  `best_sub_psf_snr` keeps reading `channels[p].psf_snr`, which D1w already made the common-set value (the raw sub's
  fits are in subject pixels, so no second pairing). `SNR_GAIN_VERSION` = 2 (and `snrGain.ts`'s).
- [ ] **Step 4:** tests PASS; `npx vitest run src/components/stacking`; commit `feat(stacking): the SNR gain on the
  frame-common set, SNR_GAIN_VERSION 2 (Plan D D1b)`.

**Acceptance:** tests green; the 28 sites compile; `cargo test -p athenaeum-core stacking::integrate` and
`ln::reference` green. Budget ≤ 120 calls.

---

### Task D1m: D1 at master level (checkpoint; owns no fix)

- [ ] **Step 1 (runner, lab agent sonnet):** `zsh pland.sh ln-truth d1 <head>` (hc11, runB, s204; base `dbase`).
- [ ] **Step 2:** report: the D bars, the reweigh's log lines per group (anchor, set size, fallbacks: expected none),
  the frames the deferred gate registered and the reweigh floor then dropped (expected none in the sharpest decile),
  `GroupStats.dropped_below_min_weight` per group (`GroupOutput.stats`, integrate.rs:525; expected 0), the frames the
  rows now show with a `NULL` weight, the registration reference's FWHM rank,
  wall of `reweigh_groups` per group (≤ 5 s for 200 frames), the 8 hc11 C10 r1 frames' ranks before / after.

**Acceptance:** B-LN, B-M, B-W, B-S PASS on all three; B-E EQUAL (D1 moves no Measure number: any difference = STOP);
any miss → back to D1 / D1w through Fable. `clean d1` after the results commit (a strip: D2 / D3 base on d1).

---

### Task D2: `c`'s colour sample (PD-9)

**Coupling checklist:** owns `c`'s candidate rule on colour planes; re-verifies `c` (lab truth), the common-set flux on
OSC (D1), the Measure-time weight; LN untouched (F1 is `c`-free). Rulings it may revisit: PD-4 (the reversal bar).

**Files:** `star_measure/photometry.rs` (`aperture_correction` l.1034, the `in_statistic` filter l.1092),
`star_measure/fit.rs` (`Correction` l.1891 gains `point_siblings: Option<&'a [Seed]>`), `star_measure/mod.rs`
(`sibling_seeds` l.1050 gains a twin `point_like_sibling_seeds` excluding each plane's `DetectionReport.extended`
seeds; `finish_working`'s `Correction` l.1519 passes it; `MEASURE_VERSION` l.186 → 8 with its doc line "8 = Plan D"),
`stacking/plan.rs` goldens (l.8487, l.8703), the dictionary (`rescued`, a usize on the two aperture-correction events).

- [ ] **Step 1: the failing tests** (`photometry.rs`): `an_extended_candidate_with_a_point_like_sibling_seed_joins_c`;
  `an_extended_candidate_whose_siblings_are_extended_too_stays_out`; pins `a_mono_plane_keeps_extended_candidates_out_of_c`,
  `the_frame_statistic_and_the_beta_sample_still_leave_extended_fits_out`; the PB-12' tests (l.3333–3452) unchanged.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement; candidates = not saturated AND (not extended OR rescued); the
  clip screen, PB-12' and the SNR floor then run as today; log `rescued` beside `point_source`, `below_snr`.
- [ ] **Step 4:** tests PASS; re-record the two goldens with the comment line of the precedent; list the moved
  `stacking::measure` pins (colour fixtures only); commit `feat(star_measure): c's colour sample keeps point-like
  stars the knot flag took, MEASURE_VERSION 8 (Plan D D2)`.
- [ ] **Step 5 (dev Mac, lab agent):** the D0 probe at the head on s204 / runB OSC (the measure arm): per frame R / G /
  B `c`, sample depth, the kept sample's median model flux over the top 64's (hc11's 2024-09-08 night needs no look:
  PD-9).
- [ ] **Step 6 (runner):** `zsh pland.sh cycle d2 <head> --prev dbase`; `zsh pland.sh ctable d2`;
  `zsh pland.sh ln-truth d2 <head> --base d1` on runB and s204.

**Acceptance:** ctable against B6's cells: farther 0; the cycle: moved rows only in colour families (a moved mono row =
STOP), on moved `c` rows the median |c / c* − 1| not larger than at dbase; real R: sample depth p50 ≤ 80 (was 219–222)
and the kept median model flux ≥ 0.9 × the top 64's (was 0.46–0.47), R's ln `c` residual MAD about its FWHM fit ≤ 1.5 ×
G's; master level vs D1: B-LN, B-M, B-W, B-S on runB / s204; PD-4's reversal bar. `clean d2` after the results commit (a
strip: D3 bases on d2). Budget ≤ 150 calls.

---

### Task D3: Blend on the stacking Measure (PD-10)

**Coupling checklist:** owns what a stacking fit accepts; re-verifies the common set (D1: a refused fit leaves only
that frame's pair), the frame statistic, mono registration's star list, the master FWHM / eccentricity,
`master_psf_snr` and the SNR gain (PD-10: the master and both gain crops refuse blends too). Rulings it may revisit:
none.

**Files:** `star_measure/mod.rs` (l.1519: `blend_refusal: true`; the R-PC-10b comment updated; `MEASURE_VERSION` stays
8, its doc line extended), `stacking/measure.rs` pins that move, one `stacking/integrate.rs` test, README at DF.

- [ ] **Step 1: the failing tests:** `the_stacking_measure_refuses_a_resolved_blend` (`Mode::All`, two seeded stars 1.7 F
  apart → one `blend` refusal in `refused_by_reason`); `the_master_measurement_refuses_a_resolved_blend`
  (`stacking/integrate.rs`: `measure_planes_diagnosed` with the master's options as integrate.rs:1749–1765 builds them,
  `fit_stars: None` and `PixelNoise::Mixed`, on the same two stars → one `blend`); pin
  `ln_refits_still_fit_without_the_blend_refusal`.
- [ ] **Step 2:** run → FAIL. **Step 3:** the one-line change. **Step 4:** tests PASS; moved pins listed; commit
  `feat(star_measure): the Blend refusal on the stacking Measure (Plan D D3)`.
- [ ] **Step 5 (runner):** `zsh pland.sh cycle d3 <head> --prev d2`; `zsh pland.sh ln-truth d3 <head> --base d2`
  (runB, s204) and `--base d1` (hc11).

**Acceptance:** the cycle: every moved row in a family with ≥ 1 stacking-path `Blend` refusal (R-PC-11 (a)'s clause),
the `Population`-mode rows expected to move under it (they reach `finish_working`, mod.rs:590), truth bars PASS;
master level: B-LN, B-M's noise ratio, B-W (ρ ≥ 0.9 against D2's / D1's), B-S, B-R (each ≤ 5 ranks); B-M's FWHM ratio
against D2 read as a stated D3 number (the master's own measurement now refuses blends, PD-10, so it carries the
measurement's move beside the image's), reported with the master's Blend count, above 1.01 a STOP to Fable; the master
PSF SNR and the SNR gain against D2's reported; Blend refusals per group reported (C10 r1: hc11 391, runB 68 361, s204
72 951). Budget ≤ 120 calls.

---

### Task DF: Final runs, set 109, suites, docs, review, merge

**Files:** `docs/stacking/README.md` (R-OSP-18: PD-1…PD-11, every R-PD ruling and run line; the SNR-gain bullet
l.4413; the Stacking rules list), `CLAUDE.md` (the Stacking line: the weight rule and `MEASURE_VERSION` 8),
`docs/superpowers/open-items.md` (Plan D closed; owed smokes; release notes owed), the after note
`docs/superpowers/research/2026-11-XX-stacking-weight-after-plan-d.md`.

- [ ] **Step 1 (runner):** `zsh pland.sh ln-truth final <head>` (base `dbase`, all three) and, if any code moved after
  D3's cycle, `zsh pland.sh cycle final <head> --prev d3`.
- [ ] **Step 2 (dev Mac): `zsh pland.sh set109 <head>`**: `prepare-catalog.sh` copy (REGISTRY row), its stacking
  folders under `~/.athenaeum-acc/plan-d/set109/`, the release `athenaeum-web` (headless harness), one run of set 109.
- [ ] **Step 3: suites** on the dev Mac, single runs: core `--all-targets`, `cargo test -p athenaeum-web`, `cargo check
  -p athenaeum-core --no-default-features`, `npx vitest run`, `npm run build`; the controller re-runs them.
- [ ] **Step 4: docs**, the owned-quantities table filled; release notes owed: the weight follows transparency and
  sharpness, not clipped stars; per-frame PSF SNR reads fewer stars (smaller numbers); the SNR gain re-measured (older
  runs show "—"); the first stack re-measures every set once; `c` on OSC; Blend on the stack.
- [ ] **Step 5:** final whole-branch review on `fable`, hunting the seams: units (working vs native), the floor-(i)
  stand-in, the floor deferral and re-admission, the per-plane fallback, the crop origin in D1b, the anchor's ties.
- [ ] **Step 6:** merge `plan-d` into local `main`, core suite on the merged tree; push only on the owner's word.

**Acceptance:** final: all D bars vs `dbase` on the three trees (B-M's FWHM ratio under D3's ruling, PD-10); set 109: the group β recorded (non-null) per group, the
sharpest decile's median rank ≤ 0.2 N per group, the top-weight frame's FWHM ≤ the group median, the SNR gain ≤ √N
(14.42 mono, 12.65 OSC) and above run 9's (7.08×; 7.41 / 7.99 / 7.86×), the per-frame common PSF SNR's ln-slope on FWHM
within ±8 %/px (run 9: ≈ FWHM²), the reweigh floor's drops reported (some soft frames may go under `PsfSnr`, none in
the sharpest decile); s204 OSC p0's paired median reported beside C10 r1's (−0.022 pp; PD-8); the registration
reference's FWHM rank per group (PD-11); suites green; review clean; merged locally; `disk-report` clean, `clean` of every stage
except what the after note cites.

---

## Owned quantities at close

| quantity | owner | consumers | bar | after value |
| ---- | ---- | ---- | ---- | ---- |
| the frame-common star set (anchor, membership) | D1 (PD-3) | the weight's flux term, the SNR gain | D1a ρ ≥ 0.99 vs D0 (b) | DF |
| the weight's flux term (`common_tflux`, `common_tmean_flux`) | D1 / D1w | `compute_weights` (PSF modes), the floor, LN members, the normalization anchor, integration, drizzle, the best sub | D bars (D1m, DF) | DF |
| the star flux (model × `c`) | Measure (Plans A / B), read only | the common set, `tflux` | Plan B's bars | — |
| `c`'s sample on a colour plane | D2 (PD-9) | the star flux on OSC | ctable farther 0; real R depth / spread | DF |
| the SNR gain and the master / best-sub PSF SNR | D1b | the Results panel | n equal frames ≈ 0.9 √N; set 109 ≤ √N | DF |
| what a stacking fit accepts (Blend) | D3 (PD-10) | every `finish_working` consumer: the subs, the master, both SNR-gain crops | cycle Blend families; D bars | DF |
| the registration reference | unchanged (PD-11, ruled) | registration | reported FWHM rank | DF |

## Execution notes

- **Models:** rust-engineer `sonnet` for D1, D1w, D1b, D2, D3; frontend-dev for `snrGain.ts`; lab jobs `sonnet`
  (every Plan D run is a judgment run: weights move by design); task reviews `opus`; the final review `fable`; a missed
  bar or a doubted verdict → `fable`.
- **Runner:** the controller hands it one command per job (`zsh pland.sh <job> <sha>` after fetching `plan-d` from
  GitLab or a bundle on `/Volumes/Universe/athenaeum-runner/git/`); its results come back through the lab repo.
- **Spend, rough:** D-1 one lab agent + the runner's before; D1 an implementer, a review, a lab agent; D1w and D1b an
  implementer and a review each; D1m one runner job; D2 and D3 an implementer, a review and two to three lab jobs each;
  DF two runner jobs, the set 109 smoke, the suites, a docs agent, the fable review. About 22 agent phases.
- **Controller:** reads only the reports, re-runs every gate itself, restarts at task boundaries or near 400k context,
  keeps the ledger's "RESUME HERE" current, `disk-report` at each task boundary.
