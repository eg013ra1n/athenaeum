# Robust Quad Seed (PD-39) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: the user skill `lean-execution` (`~/.claude/skills/lean-execution/SKILL.md`)
> above `superpowers:subagent-driven-development`: one phase per agent, a one-page handoff, reports of at most 80 lines,
> one command per lab job printing a verdict of at most 30 lines, `haiku` for proof runs, `sonnet` or stronger for
> judgment runs. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a few false quad matches can no longer drag registration's quad seed off the field. `register/align.rs::seed_affine`
fits the seed by a clipped least squares over the quad matches. Today the sharpest mono frames registered onto an OSC
reference keep 87–355 inliers and land up to 6.4 px off. After the fix they register right, and a frame whose quad
matches are clean gets a bit-identical seed (while its true matches sit within the clip threshold, RQ-1).

**Architecture:**
- **athenaeum-core `stacking/register/align.rs`**: a new pure `robust_quad_seed`, called by `seed_affine` in place of the
  single `solvemyastro::quad::fit_affine` call. `fit_affine` itself is unchanged (the plate solver shares it), and each
  round of the clip calls it. `ALIGN_VERSION` lives here too.
- **`stacking/config.rs`**: `registration_subtree` folds `alignVersion`, so cached registration rows never hide the fix.
- **`stacking/register/mod.rs`**: a pure `low_inlier_frames`.
- **`stacking/run.rs`**: `warn_low_inlier_frames`, called at the end of the persisting registration pass. It writes a
  log line only: no row and no run warning.
- **`examples/regfrag_probe.rs`**: the research probe, committed. It gains the robust-seed stage columns and a `mapgrid`
  arm.
- **Unchanged:** no command, no route, no config field, no TS type, no wire. Both hosts keep their surfaces.
- **athenaeum-lab**:
  - `harness/acceptance/regfrag.sh` (jobs `offline`, `accept`, `set109`, `report`, `clean`), new;
  - `star-lab/bin/regfrag_bars.py` (the truth maps and the RQ bars) and `star-lab/tests/test_regfrag_tools.py`, new;
  - `pland.sh` gains `PLAND_OWNER` and `PLAND_BASE_RESULTS` (where the base's results are read); `rqs` / `rbase` join
    its comparator stages and post the migrated rejection mode, as DF's `final` does (PD-40).
  - Results go under `star-lab/results/regfrag/<job>/`.
- **Machines:**
  - **Dev Mac:** the offline gate (the research lists live there), the master-level runs (PD-12) and the set 109
    control.
  - **Runner:** the noreg lab cycle.
  - The dev Mac has 16 GB: one compile and one implementer at a time, agent builds in `target-agent`, heavy runs under
    `heavy_lock.py`.

**Tech stack:** Rust (athenaeum-core), Python 3.14 + numpy (lab, `unittest`), zsh drivers.

**Spec:**
- The research `docs/superpowers/research/2026-11-02-sharp-frame-registration-fragility.md` (922725a4).
- Its report and the controller's check, `.superpowers/sdd/2026-11-02-regfrag/report-r1.md`.
- The brief, `plan-brief-r1.md`.
- The fable review r1, `plan-review-r1.md`, folded here: C1 with the controller's PD-40 resolution, I1–I4, M1–M5.

Binding beside them:
- `docs/stacking/README.md`: R-T6-9 (the plate-solve seed is the fallback after a quad failure), R-M4b-2 (the per-frame
  scale gate), R-SC-1 (the default rejection normalization is `local`).
- Plan D rulings (`.superpowers/sdd/2026-10-31-plan-d/rulings.md`): PD-12 (where the jobs run), PD-39, and PD-40 (DF's
  `final` runs post the config the merged head's one-time migration leaves: `normalization.rejection`
  `scaleZeroOffset` → `local`).

## Global Constraints

- No third-party program name anywhere in this repository. The comparison program is "the ERP": a comparison, never the
  referee. Probes stay in core `examples/`; lab automation lives in athenaeum-lab.
- `solvemyastro/` is not touched: `quad.rs::fit_affine` is shared with the plate solver.
- Owner catalogs and clones are read-only (`sqlite3` `immutable=1` / `-readonly`). The set 109 control runs on a
  `prepare-catalog.sh` copy whose stacking folders point under `~/.athenaeum-acc/regfrag/set109/`.
- No `cargo fmt`. No `rustfmt` on `align.rs`, `run.rs`, `config.rs`, `plan.rs` or `register/mod.rs`: format them by hand.
  `rustfmt` is allowed only on `examples/regfrag_probe.rs`.
- Cargo: `CARGO_TARGET_DIR=target-agent` for agents, `.superpowers/target-acc` for the lab's release probe builds.
  `CARGO_BUILD_JOBS=4`, `--test-threads=4`. `cargo test` takes ONE positional filter.
- Logging is `tracing` only. `align.rs` stays pure geometry: it emits no new event (logging spec, l.152).
  - One new field name, `median_inliers`, goes into the dictionary of
    `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in R3.
  - Never swallow an error.
- **The new path mirrors the proven path.** The estimator is exactly the one the research measured (`q1/variants.py`
  `robust_seed`): the residual metric, the threshold, re-admission, the stop rule and the numpy median. Each round is
  the proven `fit_affine`, including its xy-scale sanity gate.
  - The truth tool mirrors `q1/truthmaps.py`: fits with `flags & 5 == 0`, a unique match per member, radii
    6 / 4 / 1.5 / 1.0 px.
- One owner per measured quantity (table at the end). A second truth reader or a second seed estimator anywhere is a
  STOP.
- Every test a task names is written with its assertions; a test that cannot fail before its change is a pin, outside
  the RED step. Any moved existing test is listed with its reason. A move that is not a pinned stage-hash literal (R2),
  or a scene whose quad matches carried a false match (R1), is a STOP.
- A missed bar goes to a fresh `fable` agent with the numbers and the options. Its verdict, after a truth check on
  existing data, is the ruling, and the owner is informed. Push, release and spending stay the owner's.
- **Branch and commits:**
  - Branch `robust-seed` from local `main` after Plan D's merge, in the main checkout, once Plan D's worktree is gone.
    Never touch `.claude/worktrees/plan-d`.
  - Pushed to GitLab for the runner, never `main`. Merged into local `main` after a clean final review; `main` is pushed
    only on the owner's word.
  - Lab work is committed and pushed in athenaeum-lab `main` after each lab step.
  - Commits use the git default author and end with the harness's `Co-Authored-By` line (and `Claude-Session` when the
    harness gives one).
- **Disk (`lean-execution` §7):**
  - Where data goes: the clones are `~/.athenaeum-acc/pland-<ds>-rqs` (and `-rbase` if R0 needs them). Probe outputs go
    to `~/.athenaeum-acc/regfrag/offline/`, set 109 to `~/.athenaeum-acc/regfrag/set109/`.
  - Registry: every path gets a REGISTRY row, owner `regfrag`, expiry 2026-11-30 unless R9 says otherwise.
  - Before each heavy job: `df` ≥ the estimate + 150 GB. `disk-report` runs at every task boundary.

## The RQ bars

| bar | definition | source of the number |
| ---- | ---- | ---- |
| O1 identity | On the three research list sets (`q1/lists_d2d`, `lists_f1`, `lists`) and the OSC batch (`q1/lists_osc`, 5 frames), every frame outside O2's list (the frames the research's base registered right), through the real `align()` at the fix head with no hint, has its inliers and its 9 map numbers string-equal to the base JSONL (`v_base`, `v_lists_f1_base`, `v_lists_base`, `v_osc`; under R5 Step 1's guard) | research Q4: 79 frames, map change 0.0000 px, inlier delta 0. OSC (review r1 I1): 3 of 5 frames carry 1–2 false matches; the clip moves their seed 6.6–15.5 px at the corners |
| O2 fixed | The research's bad frames (d2d: 162 / 31 / 49 / 39; f1: 31 / 74 / 49 / 39 / 148 / 161; skyRing: 31 / 49 / 39): seed `Quads`, inliers ≥ ½ the set's median, max ≤ 0.10 px off `q1/truth.json` | robust emulation: 1 616 / 1 583 / 1 614 / 1 612, max 0.06–0.08 |
| O3 emulation | Per frame, the fix head's map against the research's WCS-hint emulation (`v_robust`, `v_lists_*_robust`): max over the grid and the inlier delta, REPORTED. A frame differing by > 0.01 px or > 2 inliers is named with its reason | numpy `lstsq` vs nalgebra SVD seeds differ by ~1e-12 |
| O4 deletions | 162 / 49 / 39 / 74 × the 200 single-star deletions of `q1/drop.py` (every 10th of 2 000): 800 / 800 within 0.2 px of truth, none failed | robust: 200 / 200 each; base 162: 4 / 200 |
| O5 mechanism | On `lists_d2d`, every frame's clipped quad matches are exactly its false matches: catalog centre > 3 px from truth's image of the image centre. Truth is `q1/truth.json` for the 9 suspects it holds and the frame's base map (`v_base`) elsewhere, never the fix head's own map (circular; review r1 M2) | 61 frames carry 1–3 false; robust clipped 61 frames, max 3. Review r1: clipped set == false set on 83 / 83 in each list set |
| E | B-E EQUAL: the fix moves no Measure number (`pland.sh --measure equal`) | — |
| G | Every frame **good in base** has bit-identical registration rows at the fix. Good in base = aligned, quad-seeded (no `+wcs` under quad-first), inliers ≥ ½ its group's median. Rows compared: `registration_results` transform_json, matched_stars, rms_residual_px, inlier_ratio, peak_error_px, scale, rotation_deg, flipped, status, model, affine_*; `stacking_run_frames` reg_status, reg_model, reg_inliers, reg_rms_px. Not compared: config_hash, registered_at, compute_time_ms. `inlier_ratio` is inliers / pairs, the one pairs-derived column, and the pairs move with the seed while map and inliers stay (lists_f1 frames 26 / 153: pairs 1 753 → 1 754, 1 732 → 1 734; review r1 I1): it is compared bitwise only for frames with `clippedQuads == 0` (from P2's replay) and reported for the others. Every other column is compared bitwise on every good frame. Applies to every group of every dataset | O1's measured identity |
| F | runB mono L and s204 mono: every frame bad in base is aligned at the fix, inliers ≥ ½ the group median, ≤ 0.15 px max and ≤ 0.05 px median off truth over the 33 × 23 grid; seed kind reported | robust 0.06–0.08 max; good frames 0.03 median |
| H | hc11 L / G and every OSC group: a frame bad in base does not regress (inliers at fix ≥ base, max off truth ≤ base + 0.05 px, or failed with the same reason). Predicted bad: 70062 (31), 69954 (205), 69865 (211), 1 L and 2 G failed outright | research Q2: blunt frames, a different path |
| U | Every group with no moved registration row: master HDU pixel data byte-identical to base (headers carry run cards and hashes), and its per-member LN gaps in the b3 compare equal | integration determinism (Plan A Step 5a; pcores control) |
| M | `pland_bars.py` against the base: B-M (noise ratio ≤ 1.02, FWHM ratio ≤ 1.01), B-LN GROUPS WORSE 0 (PD-8's rule), B-W ρ ≥ 0.9 (expected ≥ 0.99 on the moved groups, 1.0 elsewhere), B-S | Plan D bars |
| S | **Star check under `local` (R-SC-1)**, per moved mono group, on its moved frame with the largest weight (runB: 162). The header names the rejection mode base and fix posted, per dataset (`<stage>/<ds>/config.json` `normalization.rejection`; `local` on both by PD-40). A base / fix mismatch is a STOP for U, M and S, never a judged bar (review r1 C1). S1: its rejected fraction (`stats_json.rejected_fraction_per_frame`) at the fix ≤ base + 0.1 pp, and ≤ the max over the group's other F ≤ 2.5 px frames at the fix + 0.25 pp. S2: bright stars (≥ 40 σ) binned by \|e\| = \|base map − truth\| at the star (0–0.5, 0.5–1.0, ≥ 1.0 px; at the `final` base, 162's base map is the 159-inlier one). Each bin is judged against the 0–0.5 bin, so the rule reads "no trend with \|e\|": its master FWHM (fix / base − 1) minus the 0–0.5 bin's ≤ +0.4 %, and its ellipticity Δ minus the 0–0.5 bin's ≤ +0.002. The 0–0.5 bin's own change is reported; a flat LN or weight shift is bar M's, and it cannot fail every bin here (review r1 M3). Core-peak ratio (`core_replay.py`'s) reported. A bin with no stars is reported as vacuous | Q3: floor ±0.4 % FWHM, ±0.002 e; 162 at 4.33 % (d2d) / 2.71 % (d3) vs 2.29–2.36 % (global) |
| D | `"registration kept under half its group's median inliers"` at the fix: runB 0, s204 0, hc11 = its bad-in-base frames under ½ (predicted 70062, 69954, 69865), set 109 0 | Q2 |
| C | Plan D's runB mono common set / candidates ≥ 0.9 at the fix (d3: 0.96, d2d: 0.54); DF-b's "far under the anchor's candidates" warn 0 times; set sizes vs base reported | PD-39 |
| P | P1: `register_check.py` base vs fix on s204's sharp + broad mono frames: share ≥ 0.98 (expected 1.000). P2: the fix clone's own `.athf` lists, replayed through `regfrag_probe batch` at the fix head, reproduce every quad-seeded frame's row (inliers and transform equal) in every group of runB, s204 and hc11. The replay's per-frame `clippedQuads` is G's `inlier_ratio` input | Plan A Task 14b registration bar; research exactness checks |
| V | Set 109: base run, then a fix run on the same copy. Calibrate and Measure are reused (0 recomputed) and Register recomputes every frame (the `alignVersion` fold). G on every frame; min inliers ≥ 1 414; D = 0; masters byte-identical | research Q2 (set 109) |
| N | Noreg lab cycle at the fix head vs the last Plan D cycle step: moved 0; cumulative vs t01c moved 0; inject 109 / 109 | `star_probe`'s `register` path is the star list, not `align` |

## Review Focus

1. **A field where most quad matches are false** (a repetitive or crowded field, a wrong-night pairing). Expected: never
   worse than today.
   - A random false majority usually fails round 1's own xy-scale gate (`None` → `NoSeed`, as today: 190 / 200
     synthetic seeds, review r1 I2).
   - A false share at one consistent offset (30–70 %) keeps the round-1 fit, bit-equal to today's (review r1's
     adversarial check on 162's true matches).
   - The clip never takes a refit that rests on fewer than 8 matches. Pairing then fails as it does today, and the
     plate-solve fallback (R-T6-9) takes over.
   - Pinned in R1 (`a_mostly_false_match_set_gives_todays_outcome`,
     `a_consistently_offset_false_share_keeps_todays_single_fit`).
2. **A frame with fewer than 8 quad matches** (hc11's blunt frames, sparse narrowband). Expected: today's single fit,
   bit for bit. Pinned in R1 (`a_small_match_set_is_never_clipped`).
3. **A wide-field frame whose true quad matches carry a few px of field distortion against the affine.** Expected: the
   5 px floor keeps them, so nothing is clipped and the seed is bit-identical. Beyond max(5 px, 5 × median) a true
   match is clipped: a narrower seed, not a wrong one (RQ-1's condition). Pinned in R1
   (`true_matches_under_a_four_pixel_radial_term_are_not_clipped`).
4. **A meridian-flipped or 2×-binned subject** (negative determinant; quad seed as the cross-scale fallback with
   `after_wcs`). Expected: unchanged outcomes. Pinned by the existing `a_mirrored_subject_is_flipped_not_failed`,
   `the_per_frame_gate_admits_the_binned_frame` and `an_unconfirmed_hint_falls_back_to_the_quad_seed`, which must stay
   green unedited (R1 Step 4).
5. **An existing set re-stacked after the update** (fresh cached registration rows from before the fix). Expected: the
   set re-registers once, and Calibrate / Measure are not recomputed. Pinned in R2
   (`the_align_version_rides_the_registration_subtree_only`, the golden re-record) and proven on set 109 (bar V).

## Rulings fixed by this plan

Recorded in `.superpowers/sdd/2026-11-02-regfrag/rulings.md` at R0, and in `docs/stacking/README.md` at R9. RQ-5,
RQ-6, RQ-7 and RQ-9 were proposals until the owner answered; the owner accepted all four on 2026-11-02 (**Owner questions (answered)** below).

| id | ruling |
| ---- | ---- |
| RQ-1 | **The estimator: clipped least squares over the quad-centre pairs, not RANSAC.** Round 1 is today's `fit_affine` over all matches. A residual is the Euclidean distance between the fit's image of a match's image-quad centre and its catalog-quad centre. Threshold τ = max(`SEED_CLIP_FLOOR_PX` 5.0, `SEED_CLIP_MEDIAN_FACTOR` 5.0 × the median residual of the kept set), numpy median (the mean of the two middle values for an even count). keep = r ≤ τ over ALL matches (a clipped match can return). Stop when keep repeats; at most `SEED_CLIP_MAX_FITS` 5 fits. **Measured reasons:** true quad matches agree to 0.10 px (5 px = 50×); on the contaminated first fit the far-side / median seed error is 3.18–3.36 (162 42.6 / 13.4, 31 50.0 / 14.9, 49 46.6 / 14.1, 39 44.5 / 13.5), so 5× clips no true match; the false offsets are 2 405 / 4 298 px. Measured outcome: 83 / 83 right on three list sets, 79 bit-identical, 800 / 800 deletions right. **The condition of the bit-identity (review r1 M1):** a clean set keeps the single fit bit for bit while every true residual ≤ max(5 px, 5 × median) (measured clean max 1.11 px, frame 41); field distortion > 5 px against the affine would clip true matches, which gives a narrower seed, not a wrong one. Re-admission was never observed (0 of 249 frame-sets; review r1 M5). RANSAC was rejected: it draws at random, and it cannot give a clean match set the bit-identical seed by construction. |
| RQ-2 | **Guards, never worse than today.** Clip only with ≥ `MIN_INLIERS` (8) matches. A round whose keep set has < 8 matches, or whose `fit_affine` returns `None` (the scale sanity gate), is not taken: the current fit stands. Round 1 `None` → `NoSeed`, exactly as today. |
| RQ-3 | **What stays.** The pairing radius 2 × `ransacTolerancePx` (1.9), the single 4b re-pair, `MIN_INLIERS`, `maxStars`, the registration list (`detect.rs`), the plate-solve fallback (R-T6-9) and `solvemyastro` stay. The dials are rejected as measured: tolerance 2.5 is a coin flip (Task-13 lists: 162 / 132 / 141 off 0.78–1.22 px); a FWHM-aware tolerance of 1.0 fails 11 frames; `maxStars` 1 000 halves the inliers (max error 0.12 vs 0.07 px); the detector list keeps the seed 5.7–40.9 px off. |
| RQ-4 | **`seed_matches` and `NoSeed { matches }` keep counting every quad match**, not the kept ones: stored and logged meanings are unchanged. Clipping emits no warning (it is ordinary: 61 of 83 frames) and no `Alignment` field. |
| RQ-5 | **`ALIGN_VERSION` 1, folded into `registration_subtree` as `"alignVersion"`** (LN_MODEL_VERSION's precedent, R-M4a-15's reasoning: no config field moves when `align` changes). Policy: one bump per merged plan that changes what `align` returns for an unchanged star list. Cost: every set re-registers (and re-normalizes, re-integrates) once; zero extra when it ships in the same release as Plan D's `MEASURE_VERSION` 8. Cost if wrong: one extra re-registration per set. |
| RQ-6 | **The diagnostic is a log line only.** It covers both seed kinds. The median runs over the group's aligned non-reference frames after the persisting pass, cached rows included. A frame with inliers < ½ that median gets one `warn!`, with `model` naming its seed. There is no run warning, no row and no UI. Reason: the defect stood through three catalog stages because nothing counts it; R-T6-9's precedent ("a distinct event makes the pattern countable"). |
| RQ-7 | **The probe goes into core `examples/`** as `regfrag_probe.rs`. Reasons: the offline gate (R5) and parity P2 need it at the fix head; probes stay in core and lab tools in athenaeum-lab (memory); it expires 2026-11-16 in the job folder; about 220 lines, built only with `--example`. |
| RQ-8 | **Truth and the bar rule.** Truth is a direct 8-parameter homography of the frame's unsaturated fits against 20 trusted members' fits mapped through their maps (iterated at 6 / 4 / 1.5 / 1.0 px, unique matches; `truthmaps.py`). Trusted = rows bit-identical between base and fix, and inliers ≥ 0.75 × the group median, taken evenly across the group; the frame itself is never its own member. A truth with < 10 000 pairs or rms > 0.5 px is flagged weak and reported. Truth is computed for moved frames, bad-in-base frames and 10 controls per mono group (controls must read ≤ 0.10 px max, else STOP). Maps are evaluated through `regfrag_probe mapgrid` (`PixelMap::from_json` + `forward_exact`), never re-implemented in Python. |
| RQ-9 | **The base.** If Plan D's `pland-<ds>-final` clones exist with their comparator kit and `git diff --quiet <final sha> <main> -- crates solvemyastro rustafits` holds, the base is `final`. Otherwise R0 runs `rbase` at `main`. The `final` runs post `local` on hc11, runB and s204 (PD-40), and the `rqs` runs post the same; R4's `PLAND_BASE_RESULTS` reads the base's results where they are (`results/plan-d/final/`). |

## Owner questions (answered)

Fable's recommendation (review r1) beside each. **The owner accepted all four on 2026-11-02 («Да, все четыре»)**; R0 copies them into `rulings.md`.

| question | fable's recommendation |
| ---- | ---- |
| RQ-5: `ALIGN_VERSION` in the registration hash | Yes. It is free in the release that carries Plan D's `MEASURE_VERSION` 8 (the registration subtree already carries `measureVersion`); later it costs one re-register + LN + integrate per set. |
| RQ-6: the diagnostic as a log line only | Yes. |
| RQ-7: the probe in core `examples/` (feature-gated, ~220 lines) | Yes: R5 and P2 need it at the fix head. |
| RQ-9: base = Plan D's `final` clones under the diff guard | Yes, with I3 fixed (`PLAND_BASE_RESULTS`, R4). |

Not open: the star-check mode on runB (C1) is settled by PD-40 (the `final` base runs `local`). Fable also backs the
landing after Plan D's merge, on `robust-seed` from local `main` (Global Constraints).

## What it moves downstream

- **Registration rows.** Only frames bad in base move. Predicted at the merged head (D3's lists, research Q2):
  - runB mono L: 162 (159 inliers), 49 (214), 39 (355), and 31 (today `+wcs`; with the robust quad seed `homography`,
    1 583 at d2d);
  - s204 mono: 28986 (88), 28994 (99), plus the plate-solve-seed frames 28964 (= runB's 162) and 29053, which may switch
    to quads.
  - Which sharp frame lands where is the fragility itself, so the bars judge by rule (G / F), never by this list.
- **Masters.** runB mono L and s204 mono move. Every other group is byte-identical (bar U).
- **LN.** Frame 162 is LN's first reference member (PD-39), so runB mono's LN reference and every member's `.athln`
  move. s204 mono's LN moves if a moved frame is among its top-5.
- **Plan D's common set.** It hangs on the anchor's map (162 on runB mono): the set size moves (d2d 5 830 → d3 10 429),
  and so do runB mono's weights (B-W).
- **The star-measurement program's comparators (`pland-*` clones).** `pland-<ds>-rqs` becomes the comparator of every
  later plan (re-baseline in R9). `final` / `rbase` are deleted once R6's results are committed.
- **Users.** The first stack after the update re-registers every set once (RQ-5). The owner's runB / s204 masters change
  only where frames were bad.
- **Lands after** Plan D merges into local `main` (Plan D is in DF).

## Tasks at a glance

| task | job | kind | model | depends on |
| ---- | ---- | ---- | ---- | ---- |
| R0 | ledger, rulings, branch, registry rows, the base decision | controller | — | Plan D merged |
| R1 | `robust_quad_seed` in `seed_affine` + the probe in `examples/` | code | rust-engineer (sonnet) | R0 |
| R4 | lab tools: `regfrag.sh`, `regfrag_bars.py`, tests; `pland.sh` owner + comparators | lab code | sonnet | R1 |
| R5 | the offline gate on the research lists (O1–O5) | lab | sonnet | R1, R4 |
| R2 | `ALIGN_VERSION` folded into the registration hash | code | rust-engineer (sonnet) | R5 PASS |
| R3 | the low-inlier diagnostic + dictionary | code | rust-engineer (sonnet) | R2 |
| R6 | master-level acceptance runB / s204 / hc11 (E G F H U M S D C P) | lab | sonnet | R3, base |
| R7 | set 109 control (V) | lab | haiku | R3 |
| R8 | noreg lab cycle on the runner (N) | lab | haiku | R3 |
| R9 | docs, re-baseline, suites, final review, merge | close | sonnet; final review fable | R6–R8 |

Serial: R5 is the early real-data check, and a FAIL there stops the plan before R2. Reviews: `opus` per code task (R1,
R2 + R3 together, R4).

---

### Task R0: Ledger, branch, registry, the base

**Files:** `.superpowers/sdd/2026-11-02-regfrag/{progress.md,rulings.md}`.

- [ ] **Step 1 (controller):** `git switch -c robust-seed` from local `main` (Plan D merged, its worktree removed); write
  `rulings.md` (RQ-1…RQ-9 copied; RQ-5, RQ-6, RQ-7 and RQ-9 with the owner's answers); `disk-report`.
- [ ] **Step 2:** extend the REGISTRY row `~/.athenaeum-acc/regfrag` to 2026-11-30, purpose "+ robust-seed plan inputs (q1
  lists, truth.json, base JSONL) until R9". Confirm `q1/{lists_d2d,lists_f1,lists}` (83 files each), `q1/lists_osc` (5
  files), `q1/ref123.tsv`, `q1/truth.json`,
  `q1/v_{base,robust,lists_f1_base,lists_f1_robust,lists_base,lists_robust,osc}.jsonl` and
  `q1/v_drop{162,49,39,74}.jsonl` exist. The offline gate reads only these, never the `pland-*-d2d/d3` clones or
  `plan-d/d3/growth`.
- [ ] **Step 3:** the base (RQ-9). Read the REGISTRY and the Plan D ledger. If `pland-<ds>-final` hold `athenaeum.db`,
  `stackout/`, `logs/` and `.athln`, and the diff guard holds, then base = `final`. Otherwise
  `PLAND_OWNER=regfrag PLAND_RESULTS=<lab>/star-lab/results/regfrag PLAND_BASE_RESULTS=<lab>/star-lab/results/plan-d
  zsh pland.sh ln-truth rbase <main sha> --base <the newest comparator left> --measure equal` (dev Mac, ~3 h; after R4,
  which adds `rbase` to the comparator stages, `PLAND_BASE_RESULTS` and the migrated rejection mode). If no comparator
  is left at all: STOP to the owner.
  - **The rejection mode (review r1 C1, PD-40).** Read, per dataset, the mode the base's runs posted
    (`<base results>/<ds>/config.json` `normalization.rejection`, and DF's verdict line for `final`) and record it in the
    ledger. Expected: `local` on hc11, runB and s204. A base whose posted mode differs from what the `rqs` runs post
    (`local`) is a STOP: a `local` fix is never compared against a global base for U, M or S.
- [ ] **Step 4:** push `robust-seed` to GitLab; ledger "RESUME HERE".

**Acceptance:** rulings file one page; the base named with its sha in the ledger; registry clean.

---

### Task R1: The robust quad seed (core) and the probe

**Coupling checklist:** owns the quad seed (RQ-1, RQ-2). Reads the quad matcher's output (`build_quads`, `match_quads`,
unchanged). Re-verifies: pairing, RANSAC / refit, 4b, the plate-solve fallback, all unchanged.

**Files:**
- Modify `crates/athenaeum-core/src/stacking/register/align.rs` (`seed_affine` l.323, the `use solvemyastro::quad`
  line l.10, its tests module);
- Create `crates/athenaeum-core/examples/regfrag_probe.rs` from `~/.athenaeum-acc/regfrag/regfrag_probe.rs`;
- Modify `crates/athenaeum-core/Cargo.toml` (an `[[example]]` entry, `required-features = ["render", "solver"]`, beside
  `d0_weight_probe`).

**Interfaces (produces):**
```rust
pub const SEED_CLIP_FLOOR_PX: f64 = 5.0;
pub const SEED_CLIP_MEDIAN_FACTOR: f64 = 5.0;
pub const SEED_CLIP_MAX_FITS: usize = 5;
#[derive(Debug, Clone)]
pub struct QuadSeed {
    pub affine: solvemyastro::quad::Affine,
    pub kept: Vec<bool>, // per match, in `matches` order: true = in the returned fit
    pub fits: usize,     // fit_affine calls behind the returned fit (1 = today's)
}
impl QuadSeed { pub fn clipped(&self) -> usize }
pub fn robust_quad_seed(matches: &[(usize, usize)], image_quads: &[Quad], catalog_quads: &[Quad]) -> Option<QuadSeed>;
```
`seed_affine` keeps its signature and return (`(Linear, matches.len())`, RQ-4). It calls `robust_quad_seed` and maps
`None` to `NoSeed { matches, after_wcs }` as today.

The algorithm (the only body this plan fixes). The doc comments cite RQ-1's numbers and its bit-identity condition, and
say that re-admission was never observed (0 of 249 frame-sets in the research):
```text
fit = fit_affine(all)?                       # None -> None (NoSeed upstream)
keep = [true; n]; fits = 1
if n < MIN_INLIERS: return (fit, keep, fits)
loop:
  r_i = |fit.apply(image centre_i) - catalog centre_i|   for all i (non-finite r_i counts as clipped)
  tau = max(FLOOR, FACTOR * numpy_median(r_i for kept i))
  new = [r_i <= tau]
  if new == keep or fits == MAX_FITS or count(new) < MIN_INLIERS: return (fit, keep, fits)
  refit = fit_affine(matches where new, original order); None -> return (fit, keep, fits)
  fit = refit; keep = new; fits += 1
```

- [ ] **Step 1: the failing tests** (`align.rs` tests).
  - **The quad-level helper.** Synthetic `Quad`s (`star_idx` any, `ratios` 0, `longest` 1) with image centres uniform
    over 6224 × 4168. Truth affine = similarity(0.99227, 180°) centring the field on 6248 × 4176 (runB's two cameras,
    research (c)). Catalog centres = truth + uniform ±0.10 px. `matches[i] = (i, i)`. Grid = 33 × 23 over the subject
    frame.
  - `a_clean_quad_match_set_keeps_the_single_fit_bit_for_bit`: 486 matches (the measured sharp-frame median), one of
    them displaced 4.9 px. Assert `fits == 1` and `clipped() == 0`, and the six coefficients' `to_bits()` equal
    `fit_affine(&matches, ..)`'s.
  - `two_far_false_quad_matches_are_clipped_and_the_seed_is_the_true_fit`: 486 true plus 2 false. The false catalog
    centres are truth + (−2 405, 0) for an image centre near (600, 600) and truth + (−4 298, 0) near (900, 3 500).
    - Guard: plain `fit_affine` over all 488 is ≥ 10 px off truth at the worst grid point (measured 42.6–50.0).
    - Assert `fits == 2`, `kept` false exactly at the two false indices, and coefficients bitwise equal to `fit_affine`
      over the 486 true matches in order.
    - Assert ≤ 0.5 px max over the grid (the measured clean-set bound).
    - Same with only the 2 405 px match: `clipped() == 1`, bitwise equal.
  - `a_small_match_set_is_never_clipped`: 7 matches, one 2 405 px false → bitwise `fit_affine` over all 7, `fits == 1`.
  - `a_mostly_false_match_set_gives_todays_outcome` (review r1 I2: an "always `Some`" assertion cannot pass, since
    `fit_affine` over all 40 fails the xy-scale gate in 190 / 200 seeds): 10 true plus 30 false (catalog centres uniform
    over the frame), over 20 RNG seeds. Per seed:
    - `robust_quad_seed(..).is_none() == fit_affine(all 40).is_none()`;
    - a `Some` with `clipped() == 0` has the six coefficients' `to_bits()` equal to `fit_affine(all 40)`'s;
    - `fits <= 5`, and no panic.
  - `a_consistently_offset_false_share_keeps_todays_single_fit`: 486 true matches plus false ones whose catalog centres
    all sit at truth + (−2 405, 0), at 30 %, 50 % and 70 % of the set. Each share → `Some`, `fits == 1`, the six
    coefficients' `to_bits()` equal to `fit_affine(all)`'s (review r1: 30–70 % consistent false on 162's true matches →
    fits 1, bit-equal).
  - `true_matches_under_a_four_pixel_radial_term_are_not_clipped`: 486 true matches whose catalog centres carry
    + k·ρ² (k sized so the corner displacement is 4.0 px) → `clipped() == 0`, `fits == 1`.
  - `seed_affine_is_bit_identical_to_the_plain_fit_on_a_clean_scene`: the `recovers_a_similarity_with_outliers_and_jitter`
    scene with 0 outliers. Guard: `robust_quad_seed` over its matches reports `clipped() == 0`. Assert `seed_affine`'s
    six coefficients bitwise equal a re-statement `build_quads` → `match_quads` → `fit_affine`.
  - **The decoy scene** (`decoy_scene(seed) -> (subject, reference, truth)`): N subject stars from `field(..)` on 6224 ×
    4168, start N = 1 000.
    - Reference = truth(subject) ± 0.05 px. A random 18 % are removed and as many reference-only stars added (overlap
      0.82, research Q1 step 1); both lists are shuffled.
    - Two subject asterisms (4 stars inside 20 px, at about (600, 600) and (900, 3 500)), with no other star within 80 px
      in either list. Their true images are in the reference, and translated copies of those images sit at (−2 405, 0)
      and (−4 298, 0) px, carved 80 px.
    - Guards, asserted first: true quad matches (catalog centre ≤ 3 px from truth) in [400, 600] (measured 429–577; tune
      N until it holds); ≥ 2 matches > 2 000 px from truth; plain `fit_affine` ≥ 10 px off at the worst grid point.
  - `two_decoy_asterisms_no_longer_pull_the_seed_off_the_field`: `align_default` on the decoy scene. Assert:
    - `seed == Quads`;
    - the map is within 0.1 px of truth at every grid point;
    - `inliers ≥ 0.95 ×` the pairs of `pair_through(truth)` at 3.8 px;
    - `repaired * 100 <= pairs`. RED: today's seed pairs only a band, so `repaired` is most of `pairs`, or the call
      fails.
  - `deleting_one_registration_star_never_flips_the_map`: the decoy scene, deleting subject star k for k = 0, 25, 50, …
    (40 deletions, never an asterism star). Each deletion must stay within 0.2 px of truth at every grid point, with
    `seed == Quads` and `repaired * 100 <= pairs` (research: 162 right in 4 / 200 today, 200 / 200 robust). The test
    takes ≤ 30 s in a debug build; if it is slower, use fewer deletions, but no fewer than 20.
- [ ] **Step 2:** `cargo test -p athenaeum-core stacking::register::align` → the new tests FAIL (compile: no
  `robust_quad_seed`).
- [ ] **Step 3:** implement per the Interfaces block. Change nothing else in `align()`.
- [ ] **Step 4:** run `cargo test -p athenaeum-core stacking::register` and then `cargo test -p athenaeum-core
  stacking::`. The new tests PASS. The Review Focus 4 pins pass unedited. List every other moved test with its scene's
  false quad matches; any other move is a STOP. Then run `cargo check -p athenaeum-core --no-default-features`. Commit
  `feat(stacking): the quad seed is a clipped least squares over the quad matches (RQ-1, PD-39)`.
- [ ] **Step 5 (the probe):** copy the research probe.
  - Drop "uncommitted" from its doc.
  - `stages()` additionally emits `robustSeed` (9 numbers), `seedFits` and `clippedQuads` (the indices into
    `quadPairs`), from `robust_quad_seed`. The plain `seed` stays beside them.
  - A new arm `mapgrid <in.jsonl> <out.jsonl>`: each line `{"key","transformJson","w","h"}` →
    `{"key","grid":[[x,y,fx,fy],…]}` on the 33 × 23 grid over (0..w, 0..h), through `PixelMap::from_json` +
    `forward_exact`.
  - Adapt the probe to the merged head's APIs if needed.
  - `CARGO_TARGET_DIR=target-agent cargo check -p athenaeum-core --examples --features render,solver`; `rustfmt` the
    file. Commit `feat(probe): regfrag_probe — the registration stages of one frame, robust seed and map grid (RQ-7)`.

**Acceptance:** the new tests are green; the moved-test list is reviewed; examples compile; the opus review is clean.
Budget ≤ 120 calls.

---

### Task R4: The lab tools

**Files (athenaeum-lab):** `harness/acceptance/regfrag.sh`, `star-lab/bin/regfrag_bars.py`,
`star-lab/tests/test_regfrag_tools.py`; `harness/acceptance/pland.sh` (`PLAND_OWNER`, default `plan-d`, in
`registry_add`; `PLAND_BASE_RESULTS`; `rqs` and `rbase` join the comparator stages `clean` strips);
`harness/acceptance/planb.sh` and `star-lab/bin/pland_bars.py` (the base's results path only);
`harness/acceptance/tests/test_pland_sh.py`.

- **`PLAND_BASE_RESULTS` (review r1 I3).** With `PLAND_RESULTS=…/results/regfrag` alone, `ln-truth rqs --base final`
  stops with `stop sources 4`: the base's truth is read under `$RES/$base/<ds>/`, and the `final` results sit under
  `results/plan-d/final/`. `PLAND_BASE_RESULTS` (default `$RES`, exported by `pland.sh`) replaces `$RES` in every read
  of the base's results:
  - `pland.sh` l.323 (the `<ds>-truth.csv` check);
  - `planb.sh` `RC="$(sres "$B3_CMP")/$ds"` (l.2051 b3 runs, l.2103 compare);
  - `pland_bars.py` l.1662–1664 (the base's truth CSV, today the `--dir`'s grandparent): a `--base-results` option,
    default unchanged.
  - The review's other option, copying `results/plan-d/final/` (16 MB), is not taken: it would commit a second set of
    the base's truth files.
- **The rejection mode (PD-40, review r1 C1).** `rqs` and `rbase` clones post the config DF's `make-config
  --migrate-rejection-local` leaves, as `final` does (stored `scaleZeroOffset` → posted `local`), and their verdict
  prints the posted mode per dataset.

- [ ] **Step 1: tests first** (`unittest`, synthetic fixtures):
  - `test_rows_compare_ignores_time_hash_and_compute_columns` (two SQLite fixtures differing only in `config_hash`,
    `registered_at` and `compute_time_ms` → identical);
  - `test_a_moved_good_frame_fails_bar_g`;
  - `test_g_compares_inlier_ratio_only_where_nothing_was_clipped` (I1): a good frame whose `inlier_ratio` alone moved,
    `clippedQuads` 2 → G PASS with the frame reported; `clippedQuads` 0 → G FAIL;
  - `test_good_in_base_rule` (failed, `+wcs`, < ½ median → bad; else good);
  - `test_a_base_fix_rejection_mode_mismatch_is_a_stop` (C1): base `config.json` `scaleZeroOffset`, fix `local` → `stars`
    and `reg` print STOP for U, M and S, not a verdict;
  - `test_truth_fit_recovers_a_known_homography`: 20 members' fits plus a subject displaced by a known homography and
    0.3 px noise → truth within 0.005 px max over the grid, members excluding the subject;
  - `test_e_bins_and_star_floor` (M3): synthetic masters. One bin's FWHM +0.5 % above the 0–0.5 bin's → S2 FAIL, +0.3 %
    → PASS; a flat +0.5 % on every bin → S2 PASS, with the 0–0.5 bin's change reported;
  - `test_offline_identity_and_mechanism` (two JSONL equal → O1 PASS; one map moved → FAIL; clipped == false set → O5
    PASS, with the false set read against `truth.json` / the base map, never the fix map);
  - `test_numpy_median_convention`;
  - `test_ln_truth_reads_the_base_truth_under_pland_base_results` (`test_pland_sh.py`, its stub pattern; I3): the base's
    `<ds>-truth.csv` only under `$PLAND_BASE_RESULTS/final/<ds>/` → no `stop sources`; `PLAND_BASE_RESULTS` unset → the
    `stop sources 4` of today.
- [ ] **Step 2: `regfrag_bars.py`** subcommands, each writing its file and printing ≤ 30 lines:
  - `offline` (O1–O5 from the probe JSONL, `truth.json` and the research JSONL; O1 includes the OSC batch);
  - `truth` (RQ-8, maps through `regfrag_probe mapgrid`);
  - `reg` (G, F, H, U per dataset from base and fix clones; G's `inlier_ratio` rule reads `clippedQuads` from `parity`'s
    replay; U is STOP on a base / fix rejection-mode mismatch);
  - `stars` (header: the rejection mode base and fix posted, per dataset, STOP on a mismatch; S1 from `stats_json`; S2
    ports `q3/master_shape.py`: master grid ↔ reference pixels by WCS-seeded star matching, adaptive Gaussian moments,
    the \|e\| bins judged against the 0–0.5 bin);
  - `parity` (P2, over every group's quad-seeded frames; it writes the per-frame `clippedQuads` G reads);
  - `logs` (D and C counts from the clone's JSONL logs).
- [ ] **Step 3: `regfrag.sh`** (planb / pland pattern; verdict line 1 `REGFRAG <JOB>: PASS|FAIL|STOP (...)`; heavy steps
  under `heavy_lock.py`; exit codes as `pland.sh`):
  - `offline <sha>` (dev Mac): release build of `regfrag_probe` at <sha> into `.superpowers/target-acc` (guard: checkout
    at <sha>). Then the three list-set batches and the OSC batch (`lists_osc`) with no hint, the 800 deletions and
    `regfrag_bars.py offline`. Data in `~/.athenaeum-acc/regfrag/offline/`.
  - `accept <sha> --base <stage>` (dev Mac, PD-12): `PLAND_OWNER=regfrag PLAND_RESULTS=…/results/regfrag
    PLAND_BASE_RESULTS=<base results> zsh pland.sh ln-truth rqs <sha> --base <stage> --measure equal`, where `<base
    results>` is `…/results/plan-d` for `final` and unset (`$RES`) for `rbase`. Then, per dataset, `parity` (first: G
    reads its `clippedQuads`) / `truth` / `reg` / `stars` / `logs`,
    and P1 (`register_check.py <out> base=<star_probe@base> fix=<star_probe@sha> --acc ~/.athenaeum-acc/pland-s204-rqs
    --run-id <its run>`).
    - The base `star_probe` is built in a throwaway worktree `~/.athenaeum-acc/acc-base-regfrag-<sha8>` with target
      `.superpowers/target-acc/tmp-regfrag`. Both get REGISTRY rows and both are removed at R9 (§7).
  - `set109 <base-sha> <sha>` (dev Mac): `prepare-catalog.sh` copy under `~/.athenaeum-acc/regfrag/set109` (REGISTRY
    row). Release `athenaeum-web` at <base-sha>, one run (`tier1-run.sh`), `athenaeum.db` snapshotted to `base.db`. Then
    the web build at <sha>, one run on the same copy, then `reg` (G, D, V, U).
  - `report`; `clean <job>`, which runs only once the job's results are committed.
- [ ] **Step 4:** tests green; commit and push lab `main`.

**Acceptance:** lab tests green; `regfrag_bars.py offline` on the research's own `v_robust*.jsonl` as the "fix" input
reads O2 / O4 PASS (the tool's calibration). Budget ≤ 120 calls.

---

### Task R5: The offline gate (dev Mac, lab agent sonnet)

- [ ] **Step 1:** the base-JSONL guard (review r1 M4): O1's base JSONL was made at the research head, so
  `git diff --quiet 922725a4 <R1 parent> -- crates/athenaeum-core/src/stacking/register solvemyastro` must hold. If it
  does not, re-make `v_base`, `v_lists_f1_base`, `v_lists_base` and `v_osc` at the R1 parent before this step's run,
  and O1 reads those. Then `zsh regfrag.sh offline <R1 head>`.
- [ ] **Step 2:** report O1–O5. Also the per-frame clipped count per list set and the OSC batch (expected on
  `lists_d2d`: 61 frames > 0, max 3; OSC: 3 of 5 > 0), and 162 / 31 / 49 / 39's inliers beside the emulation's
  1 616 / 1 583 / 1 614 / 1 612. Commit results to lab.

**Acceptance:** O1, O2, O4 and O5 PASS; O3 reported. Any miss is a STOP to fable before R2, with the frame's stage row
(`clippedQuads`, seed errors). Budget ≤ 60 calls.

---

### Task R2: `ALIGN_VERSION` folded into the registration hash (RQ-5)

**Files:**
- `register/align.rs` (`pub const ALIGN_VERSION: u32 = 1;` with its policy doc);
- `stacking/config.rs` (`registration_subtree_with_measure_version` l.1040 becomes
  `registration_subtree_with_versions(cfg, measure_version, align_version)`, emitting `"alignVersion"`; its tests);
- `stacking/plan.rs` (`stage_hashes_are_pinned_to_golden_literals`,
  `fit_stars_none_leaves_every_stage_hash_literal_unchanged`, and any other pinned literal that moves).

- [ ] **Step 1: the failing tests** (`config.rs`):
  - `the_align_version_rides_the_registration_subtree_only` (mirrors `the_ln_model_version_rides_the_normalization_subtree_only`):
    `registration_subtree(&cfg)["alignVersion"] == ALIGN_VERSION`; `measurement_subtree` and `normalization_subtree`
    carry no `alignVersion`.
  - `a_new_align_version_moves_the_registration_hash_and_not_the_measurement_hash`: `stage_hash` of
    `registration_subtree_with_versions(&cfg, MEASURE_VERSION, 0)` differs from the default; `measurement_subtree`'s hash
    is unchanged.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement.
- [ ] **Step 4:** re-record exactly the literals that move:
  - Expected to move (review r1 I4): `reg` and `ln` on every line of `stage_hashes_are_pinned_to_golden_literals`
    (fastPreview included), and `fit_stars_none_leaves_every_stage_hash_literal_unchanged`'s `reg` and its `ln` (that
    test computes `ln` from `reg`).
  - `ln_ref` and `ln_cal` do not move. `normalization_hash_for` takes the registration hash as an upstream string, and
    `ln_ref`'s call passes the literals `"r1,r2"` / `"m1,m2"`; `seeds_calibration_hash_for` folds `ln_ref`.
  - Each re-record carries the precedent's comment line ("re-recorded 2026-11-xx, robust quad seed RQ-5 (ALIGN_VERSION
    1; was …)").
  - calib or meas moving is a STOP.
  - Then `cargo test -p athenaeum-core stacking::`, `cargo check -p athenaeum-core --no-default-features` and `cargo check
    -p athenaeum-web`.
  - Commit `feat(stacking): ALIGN_VERSION in the registration hash, so cached rows never hide the robust seed (RQ-5)`.

**Acceptance:** tests green; only the listed literals moved. Budget ≤ 60 calls.

---

### Task R3: The low-inlier diagnostic (RQ-6)

**Files:** `stacking/register/mod.rs`, `stacking/run.rs` (`register_group_pass`, inside `if persist`, after the per-frame
loop and before the final progress tick), the logging dictionary.

**Interfaces:**
- `pub(crate) fn low_inlier_frames(rows: &[(i64, usize)]) -> Option<(f64, Vec<(i64, usize)>)>`: `rows` is (frame_id,
  inliers) of aligned non-reference frames. It returns `None` under 3 rows, else (numpy median, the frames with
  `inliers as f64 < 0.5 * median`, in input order).
- `fn warn_low_inlier_frames(run_id: i64, group_key: &str, rows: &[(i64, usize, String)])` in `run.rs`: (frame_id,
  inliers, model). One `tracing::warn!(run_id, group_key = %group_key, frame_id, inliers, median_inliers, model = %model,
  "registration kept under half its group's median inliers")` per flagged frame.
- The caller builds `rows` from `rc.measured[&group.key]` entries whose `registration` is `Aligned { record, .. }` with
  `!record.is_reference` (cached included): `(entry.frame.frame_id, record.matched_stars as usize,
  record.model.clone().unwrap_or_default())`.

- [ ] **Step 1: the failing tests:**
  - `low_inlier_frames_flags_frames_under_half_the_median` (`register/mod.rs`): rows (1, 1 700), (2, 1 745), (3, 1 800),
    (4, 87), (5, 214), (6, 900) → median 1 300.0, flagged [(4, 87), (5, 214)].
  - `low_inlier_frames_needs_three_frames` (2 rows → `None`).
  - `a_low_inlier_frame_is_warned_once_with_its_group_median` (`run.rs`, `stacking::telemetry::capture_logs`): rows with
    one 87-inlier frame among five at 1 600–1 900 → exactly one line with that message, carrying `frame_id`,
    `inliers=87`, `median_inliers` and `model`; with all rows ≥ ½, no line.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. No `rc.warnings` push.
- [ ] **Step 4:** dictionary entry in the logging spec: "**Dictionary extension (registration diagnostic,
  `stacking::run`, robust quad seed RQ-6, 2026-11-xx):** `median_inliers` (f64; the median refit inliers over a group's
  aligned non-reference frames after the persisting registration pass, cached rows included, numpy median) on the warn
  `"registration kept under half its group's median inliers"`, beside the canonical `run_id`, `group_key`, `frame_id`,
  `inliers`, `model`; no run warning." Tests green. Commit `feat(stacking): a registration warn when a frame keeps
  under half its group's median inliers (RQ-6)`.

**Acceptance:** tests green; the opus review of R2 + R3 is clean. Budget ≤ 80 calls.

---

### Task R6: Master-level acceptance (dev Mac, lab agent sonnet, ~3 h + bars)

- [ ] **Step 1:** `disk-report`; `df` ≥ estimate + 150 GB; no compile running. `zsh regfrag.sh accept <R3 head> --base
  <R0's base>` (hc11 → runB → s204; REGISTRY rows `pland-<ds>-rqs`, owner `regfrag`).
- [ ] **Step 2:** report:
  - the rejection mode posted per dataset, base and fix (`config.json` `normalization.rejection`; expected `local` on
    all six; a mismatch is a STOP before U, M and S, review r1 C1);
  - bars E, G, F, H, U, M, S, D, C, P;
  - the moved-row list per group (frame, inliers base → fix, seed kind, truth max / median);
  - runB mono common-set size and weights ρ;
  - frame 162's rejected fraction base → fix;
  - the LN members that moved.
  - Commit the results to lab.

**Acceptance:** every bar PASS. A moved good-in-base frame (G), a control truth > 0.10 px, a base / fix rejection-mode
mismatch, or any input unread is a STOP to fable with the frame's probe stage row and both maps. Budget ≤ 150 calls.

---

### Task R7: Set 109 control (dev Mac, lab agent haiku)

- [ ] **Step 1:** `zsh regfrag.sh set109 <main sha at R0> <R3 head>`.
- [ ] **Step 2:** bar V (with G, D, U inside it); commit the results.

**Acceptance:** V PASS. Hard STOP: any gate off means no re-run with other flags and no code edit; report the gate and
≤ 10 lines of evidence. If the owner declined RQ-5, the fix run is forced fresh from Register and V's "Register
recomputes" clause reads the forced re-run. Budget ≤ 60 calls.

---

### Task R8: Noreg lab cycle (runner, lab agent haiku)

- [ ] **Step 1 (controller):** the runner fetches `robust-seed` from GitLab. Copy the last Plan D cycle stage's
  `design.json` to `results/regfrag/rqs/design.json` (nothing new). Name `--prev` = that stage, from the Plan D ledger.
- [ ] **Step 2:** `PLAND_OWNER=regfrag PLAND_RESULTS=…/results/regfrag zsh pland.sh cycle rqs <R3 head> --prev <stage>`.

**Acceptance:** bar N (noreg moved 0, cumulative moved 0, inject 109 / 109). Hard STOP as R7. The controller runs
`lab_cycle.py --pa3-delete pland-rqs --yes` after the results commit. Budget ≤ 40 calls.

---

### Task R9: Docs, re-baseline, suites, review, merge

**Files:** `docs/stacking/README.md`, `CLAUDE.md`, `docs/superpowers/open-items.md`.
- **README:** a paragraph "**Robust quad seed (2026-11)**" after the R-SC-1 paragraph: RQ-1…RQ-9, the R5 / R6 / R7 / R8
  lines with lab commits, and what moved. `regfrag_probe.rs` joins the dev-probe list under **Key files**.
- **CLAUDE.md:** the Stacking line gains "registration's quad seed is a clipped least squares (`ALIGN_VERSION`, RQ-1 /
  RQ-5)".
- **open-items:** delete any PD-39 fragility entry; add the owed smoke "re-stack runB / LDN1272 and set 204 on the
  owner's catalog after the update: every mono frame ≥ ½ its group's median inliers".

- [ ] **Step 1: re-baseline.**
  - Strip `pland-<ds>-rqs` with `pland.sh clean rqs`: the comparator kit stays.
  - Re-label their REGISTRY rows as the star-measurement program's comparator. The expiry is the next plan's, set by the
    controller.
  - Delete `final` / `rbase` clones and rows (superseded). `git worktree remove` the `acc-base-regfrag-*` worktree, delete
    `.superpowers/target-acc/tmp-regfrag`, and delete both rows.
  - Cut `~/.athenaeum-acc/regfrag` to nothing once R5–R8 results are committed: the probe now lives in the repo and the
    results in lab. Delete its row.
  - `disk-report` clean.
- [ ] **Step 2: suites** (dev Mac, single runs):
  - `cargo test -p athenaeum-core --all-targets`, `cargo test -p athenaeum-web`;
  - `cargo check -p athenaeum-core --no-default-features`, `cargo check -p athenaeum-tauri`;
  - `cargo check -p athenaeum-core --examples --features render,solver`.
  - The controller re-runs them.
- [ ] **Step 3:** docs as above. Release-notes line owed: "Very sharp frames registered onto a reference from another
  camera no longer land a few pixels off; the first stack after the update re-registers every set once."
- [ ] **Step 4:** the final whole-branch review on `fable`, hunting the seams:
  - the bit-identity claim (round 1 untouched);
  - the guards (< 8 matches, `None` refit);
  - `seed_matches` meaning;
  - the hash fold's reach (calib / meas untouched);
  - the diagnostic's reference filter and cached rows.
- [ ] **Step 5:** merge `robust-seed` into local `main`; core suite on the merged tree; push only on the owner's word.
  The controller updates the memory pointers (comparator = `rqs`).

**Acceptance:** suites green; review clean; merged locally; registry and disk clean. Budget ≤ 100 calls.

## Owned quantities at close

| quantity | owner | consumers | bar | after value |
| ---- | ---- | ---- | ---- | ---- |
| the quad seed (clipped LS) | R1 (RQ-1, RQ-2) | pairing, RANSAC, every registration row | O1–O5, G, F | R9 |
| the registration hash's `alignVersion` | R2 (RQ-5) | every downstream stage hash | V, goldens | R9 |
| the low-inlier diagnostic (`median_inliers`) | R3 (RQ-6) | logs only | D | R9 |
| truth maps of a group's frames | R4 `regfrag_bars.py truth` (RQ-8), the one truth reader | F, H, S | controls ≤ 0.10 px | R6 |
| the registration list (`stars_from_fits`, the saturation cut) | Measure / `detect.rs`, unchanged | the seed | P1, E | — |
| masters, LN, Plan D's common set and weights | downstream, re-measured only | the program's comparators | M, U, S, C | R6 |

## Execution notes

- **Models:**
  - rust-engineer `sonnet` for R1–R3;
  - lab `sonnet` for R4–R6 (judgment: registration moves by design);
  - `haiku` for R7 / R8 (proof runs, hard STOP);
  - `opus` reviews per code task; the final review `fable`; a missed bar → `fable`.
- **Order:** R5 runs before R2 / R3, so a real-data defect in the estimator costs one task, not three.
- **Dev Mac:** R6 and R7 never run beside a compile or an implementer. Heavy runs take `heavy_lock.py`.
- **Spend, rough:** R1 an implementer, a review; R4 a lab agent and a review; R5 one lab job; R2 + R3 an implementer
  each and one review; R6 one long lab job; R7 / R8 one proof job each; R9 a docs agent, the suites, the fable review.
  About 12 agent phases, plus R0's `rbase` (~3 h) if RQ-9 needs it.
- **Controller:** reads only the reports and re-runs every gate itself. It restarts at task boundaries, keeps "RESUME
  HERE" current, and runs `disk-report` at each boundary.
