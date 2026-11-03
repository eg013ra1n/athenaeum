# Robust Quad Seed and the Coverage Gate (PD-39) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: the user skill `lean-execution` (`~/.claude/skills/lean-execution/SKILL.md`)
> above `superpowers:subagent-driven-development`: one phase per agent, a one-page handoff, reports of at most 80 lines,
> one command per lab job printing a verdict of at most 30 lines, `haiku` for proof runs, `sonnet` or stronger for
> judgment runs. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** registration takes the principle the owner chose on 2026-11-02 («Да, берём»):
1. **The initial estimate survives any share of false quad matches, not only a small one.**
   `register/align.rs::seed_affine` seeds from a deterministic RANSAC over the quad matches, then least squares over the
   consensus until it repeats.
2. **A good frame does not move.** A match set whose plain fit is clean keeps today's seed bit for bit.
3. **A map is accepted only if its inliers cover the overlap** (the coverage gate). Otherwise the frame takes the
   plate-solve seed when it has one. Without one, it fails with its reason. A frame never enters a stack on a wrong map.
4. **The plate solve stays an accelerator and a fallback, never a requirement.** A cross-scale frame still leads with it.

Today the sharpest mono frames registered onto an OSC reference keep 87–355 inliers and land up to 6.4 px off. After the
fix they register right; a map resting on a patch of stars is refused.

There is no intermediate version: the clipped estimator of this plan's first draft is dropped, not built (RQ-1).

**Architecture:**
- **athenaeum-core `stacking/register/align.rs`:**
  - a new pure `robust_quad_seed`, called by `seed_affine` in place of the single `solvemyastro::quad::fit_affine`
    call. `fit_affine` itself is unchanged (the plate solver shares it); every hypothesis and every consensus fit calls
    it.
  - Steps 2–4b of `align()` (pair through a seed, RANSAC, refit, the one re-pair) become one function `fit_from_seed`,
    whose last check is the coverage gate. The fallback of R-T6-9 therefore treats a refused map like a failed fit.
  - a pure `coverage` (R1, so the probe and the gate share one computation) and, in RG2, the gate:
    `AlignError::LowCoverage`, `Alignment.coverage`.
  - `ALIGN_VERSION` lives here too.
- **`geometry/ransac.rs`:** `degenerate` becomes `pub(crate)` (the seed's RANSAC reuses it with `SplitMix64`). Nothing
  else changes there.
- **`stacking/config.rs`**: `registration_subtree` folds `alignVersion`, so cached registration rows never hide the fix.
- **`stacking/register/mod.rs`**: a pure `low_inlier_frames`.
- **`stacking/register/frame.rs`**: the `"frame registered"` debug event gains `coverage`.
- **`stacking/run.rs`**: `warn_low_inlier_frames`, called at the end of the persisting registration pass. It writes a
  log line only: no row and no run warning.
- **`examples/regfrag_probe.rs`**: the research probe, committed. It gains the seed's stage columns, the coverage
  columns (through the core `coverage`), a `--seed plain|robust` switch, a per-frame scale-gate column and a `mapgrid`
  arm.
- **Unchanged:** no command, no route, no config field, no TS type, no wire. Both hosts keep their surfaces. A refused
  frame's reason is the stored `error` text, as every registration failure's is.
- **athenaeum-lab**:
  - `harness/acceptance/regfrag.sh` (jobs `offline`, `coverage`, `accept`, `set109`, `xscale`, `report`, `clean`), new;
  - `star-lab/bin/regfrag_bars.py` (the truth maps, the RQ bars, the coverage bars) and
    `star-lab/tests/test_regfrag_tools.py`, new;
  - `pland.sh` gains `PLAND_OWNER` and `PLAND_BASE_RESULTS` (where the base's results are read); `rqs` / `rbase` join
    its comparator stages and post the migrated rejection mode, as DF's `final` does (PD-40).
  - Results go under `star-lab/results/regfrag/<job>/`.
- **Machines:**
  - **Dev Mac:** the offline and coverage gates (the research lists and the `final` clones live there), the master-level
    runs (PD-12), the set 109 control and the cross-scale control.
  - **Runner:** the noreg lab cycle.
  - The dev Mac has 16 GB: one compile and one implementer at a time, agent builds in `target-agent`, heavy runs under
    `heavy_lock.py`.

**Tech stack:** Rust (athenaeum-core), Python 3.14 + numpy (lab, `unittest`), zsh drivers.

**Spec:**
- The research `docs/superpowers/research/2026-11-02-sharp-frame-registration-fragility.md` (922725a4).
- Its report and the controller's check, `.superpowers/sdd/2026-11-02-regfrag/report-r1.md`.
- The brief `plan-brief-r1.md`; the fable review r1, `plan-review-r1.md` (its C1 with the PD-40 resolution, I1–I4 and
  M1–M4 stay folded; M5 is superseded by r2's M2).
- The fable review r2, `plan-review-r2.md` (APPROVE WITH FIXES), folded here: C1, I1–I4, M1–M8. Its truth checks
  re-ran RQ-1 exactly as written (SplitMix64 draw order, `degenerate`, the xy-scale gate) and RQ-10's index on 1 759
  research maps.
- **The shore measurements of 2026-11-02**, made before this rewrite on the research's saved data, so that the
  principle is chosen once:
  - `~/.athenaeum-acc/regfrag/q1/ransac_seed.{py,out}`: the RANSAC + least-squares estimator against the clipped one.
  - `q1/ransac_adv.{py,out}`: heavy contamination on frame 162's 486 true quad matches.
  - `q1/gate_regularity_truth.{py,out}` and `q1/gate_regularity.py`: the coverage index against truth.
  - `q1/seed_distortion.py`: smooth differential distortion against the consensus loop.
- **How the ERP does it** (read in the ERP's shipped headers and scripts, 2026-11-02): its batch pipeline registers
  star to star and plate-solves only the finished masters. Its initial transform comes from a RANSAC-family estimator
  over the descriptor matches, refined by iterated least squares over the inliers. A model supported by a small cluster
  of pairs and extrapolated over the region is rejected; a frame that fails registration leaves the integration. This
  plan is that principle plus our plate-solve fallback.

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
- Owner catalogs and clones are read-only (`sqlite3` `immutable=1` / `-readonly`). The set 109 and cross-scale controls
  run on `prepare-catalog.sh` copies whose stacking folders point under `~/.athenaeum-acc/regfrag/`.
- No `cargo fmt`. No `rustfmt` on `align.rs`, `ransac.rs`, `frame.rs`, `run.rs`, `config.rs`, `plan.rs` or
  `register/mod.rs`: format them by hand. `rustfmt` is allowed only on `examples/regfrag_probe.rs`.
- Cargo: `CARGO_TARGET_DIR=target-agent` for agents, `.superpowers/target-acc` for the lab's release probe builds.
  `CARGO_BUILD_JOBS=4`, `--test-threads=4`. `cargo test` takes ONE positional filter.
- Logging is `tracing` only. `align.rs` stays pure geometry: it emits no new event (logging spec, l.152).
  - Two new field names go into the dictionary of `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`:
    `coverage` (RG2) and `median_inliers` (R3).
  - Never swallow an error.
- **The new path mirrors the proven path.**
  - The estimator is exactly the one measured in `q1/ransac_seed.py`: 3-match samples, consensus at 5 px, the adaptive
    bound and early exit of `geometry::ransac::ransac_fit`, then least squares over the consensus until it repeats at
    max(5 px, 5 × the numpy median), at most 5 fits.
  - Its sampling is `ransac_fit`'s: `SplitMix64` with a fixed seed, `degenerate` samples skipped, a strictly larger
    consensus replaces the best.
  - Every fit is the proven `fit_affine`, including its xy-scale sanity gate.
  - The coverage index mirrors the measured `regularity` (a 4 × 4 grid, a cell counts when it holds an inlier),
    restricted to the cells that can hold one (RQ-10).
  - The truth tool mirrors `q1/truthmaps.py`: fits with `flags & 5 == 0`, a unique match per member, radii
    6 / 4 / 1.5 / 1.0 px.
- One owner per measured quantity (table at the end). A second truth reader, a second seed estimator or a second
  coverage computation anywhere is a STOP.
- Every test a task names is written with its assertions; a test that cannot fail before its change is a pin, outside
  the RED step. Any moved existing test is listed with its reason. A move that is not a pinned stage-hash literal (R2),
  or a scene whose quad matches carried a false match (R1), is a STOP.
- A missed bar goes to a fresh `fable` agent with the numbers and the options. Its verdict, after a truth check on
  existing data, is the ruling, and the owner is informed. A lab agent never re-tunes a threshold. Push, release and
  spending stay the owner's.
- **Branch and commits:**
  - Branch `robust-seed` from local `main` after Plan D's merge, in the worktree `.claude/worktrees/robust-seed`
    (R0-3: the main checkout is shared by other sessions and the owner's interactive build). Agent builds use
    `CARGO_TARGET_DIR=<worktree>/target-agent`; `.superpowers/` stays in the main checkout. Never touch another
    session's worktree.
  - Pushed to GitLab for the runner, never `main`. Merged into local `main` after a clean final review; `main` is pushed
    only on the owner's word.
  - Lab work is committed and pushed in athenaeum-lab `main` after each lab step.
  - Commits use the git default author and end with the harness's `Co-Authored-By` line (and `Claude-Session` when the
    harness gives one).
- **Disk (`lean-execution` §7):**
  - Where data goes: the clones are `~/.athenaeum-acc/pland-<ds>-rqs` (and `-rbase` if R0 needs them). Probe outputs go
    to `~/.athenaeum-acc/regfrag/{offline,coverage}/`, set 109 to `~/.athenaeum-acc/regfrag/set109/`, the cross-scale
    copy to `~/.athenaeum-acc/regfrag/xscale/`.
  - Registry: every path gets a REGISTRY row, owner `regfrag`, expiry 2026-11-30 unless R9 says otherwise.
  - Before each heavy job: `df` ≥ the estimate + 150 GB. `disk-report` runs at every task boundary.

## The RQ bars

| bar | definition | source of the number |
| ---- | ---- | ---- |
| O1 identity | On the three research list sets (`q1/lists_d2d`, `lists_f1`, `lists`) and the OSC batch (`q1/lists_osc`, 5 frames), every frame outside O2's list (the frames the research's base registered right), through the real `align()` at the fix head with no hint, has its inliers and its 9 map numbers string-equal to the base JSONL (`v_base`, `v_lists_f1_base`, `v_lists_base`, `v_osc`; under R5 Step 1's guard). For the OSC batch, whose seeds carry 1–2 false matches on 3 of 5 frames, the map is compared to the emulation instead (O3) | research Q4: 79 frames, map change 0.0000 px, inlier delta 0 |
| O2 fixed | The research's bad frames (d2d: 162 / 31 / 49 / 39; f1: 31 / 74 / 49 / 39 / 148 / 161; skyRing: 31 / 49 / 39): seed `Quads`, inliers ≥ ½ the set's median, max ≤ 0.10 px off `q1/truth.json` | the emulation: 1 616 / 1 583 / 1 614 / 1 612, max 0.06–0.08 |
| O3 emulation | Per frame, the fix head's map against the research's emulation (`v_robust`, `v_lists_*_robust`): max over the grid and the inlier delta, REPORTED. A frame differing by > 0.01 px or > 2 inliers is named with its reason | the RANSAC estimator keeps the same match set as the emulated one on 1 054 / 1 054 sets (`q1/ransac_seed.out`); numpy `lstsq` vs nalgebra SVD differ by ~1e-12 |
| O4 deletions | 162 / 49 / 39 / 74 × the 200 single-star deletions of `q1/drop.py` (every 10th of 2 000): 800 / 800 within 0.2 px of truth, none failed | emulation: 200 / 200 each; base 162: 4 / 200 |
| O5 mechanism | On `lists_d2d`, every frame's dropped quad matches are exactly its false matches: catalog centre > 3 px from truth's image of the image centre. Truth is `q1/truth.json` for the 9 suspects it holds and the frame's base map (`v_base`) elsewhere, never the fix head's own map (circular; review r1 M2) | 61 frames carry 1–3 false; review r1: the dropped set == the false set on 83 / 83 in each list set |
| K1 gate vs truth (a pin) | At the R1 head, `--seed plain` (today's path) and `--seed robust` over the research lists and the 800 deletions, through the shipped `coverage`: every map ≤ 0.15 px max off truth has coverage ≥ `MIN_COVERAGE` (0.75), every map > 0.15 px off has coverage < 0.75. On these full-overlap lists the index is expected to track the reference-grid `regularity`, so K1 pins the transfer (reported: the maps where the two differ by more than one cell); it does not test what RQ-10 adds (eligibility, the overlap grid) — K2, RG2's scenes and X do | review r2: the subject-frame-grid form == `regularity` on 1 759 / 1 759 maps; ≤ 0.15 px → 1.0 (n 1 029); 0.15–1 px → 0.4375–0.5625 (n 386); > 1 px → 0.125–0.625 (n 196) |
| K2 gate vs good frames (carries the threshold) | At the R1 head, every non-reference frame of every group of `pland-{runB,s204,hc11}-final` (OSC groups included), replayed from the clone's `.athf` lists: each frame good in base has coverage ≥ 0.75. Reported: the minimum per group and every frame under 0.875. hc11 is reported per frame (coverage, paired cells, inliers, registration stars), its below-½ frames included. Every frame under 0.75 gets a truth map (RQ-8, base members as trusted); one whose map is ≤ 0.15 px off truth is a STOP (the gate would refuse a precise map) | research: every good frame 1.0; hc11 70062 is the one real few-star case (44 registration stars, 37 inliers). Risk on record: `AUTO_DISTORTION_MIN_REGULARITY`'s note saw whole-frame `regularity` 0.69 on a well-covered OSC subject; RQ-10's eligible cells and overlap grid exist for that case |
| K4 gate at master level | Per dataset at the fix: the gate refuses (`LowCoverage`) or sends to the plate-solve seed no frame good in base. Every frame it refuses or re-seeds is named with its base map's truth error; a refused frame whose base map is ≤ 0.15 px off truth is a STOP | RQ-10 |
| E | B-E EQUAL: the fix moves no Measure number (`pland.sh --measure equal`) | — |
| G | Every frame **good in base** (aligned, quad-seeded with no `+wcs` under quad-first, inliers ≥ ½ its group's median) is judged at the fix by P2's `droppedQuads` (R4-13, fable). (a) `droppedQuads == 0` (its seed is today's bits): every compared column bitwise equal, `inlier_ratio` included — a difference is a FAIL. (b) `droppedQuads > 0`: `inlier_ratio` reported, never compared; every other compared column bitwise equal → PASS. Otherwise the frame is a **moved good frame** and PASSES only with all four, on its own RQ-8 truth (a weak truth is a STOP to fable): (1) every dropped quad match is false (catalog centre > 3 px from the truth's image of its image centre, O5's test) and no kept match is; (2) still good at the fix — aligned, seed `Quads`, coverage ≥ 0.75, inliers ≥ base inliers − 2; (3) over the 33 × 23 grid, `fix_med ≤ base_med + 0.01 px` and `fix_max ≤ base_max + 0.05 px`; (4) the move (fix vs base) ≤ 0.10 px max — a larger move on a good frame is a STOP to fable. (1)–(3) missed → FAIL. The judge names every moved good frame (dropped count, inliers base → fix, move median / max, base and fix off truth median / max). A moved good frame is a moved row: its group is a moved group for U (judged by M); it is never S's frame. Rows compared: `registration_results` transform_json, matched_stars, rms_residual_px, inlier_ratio, peak_error_px, scale, rotation_deg, flipped, status, model, affine_*; `stacking_run_frames` reg_status, reg_model, reg_inliers, reg_rms_px; not compared: config_hash, registered_at, compute_time_ms. Applies to every group of every dataset | O1's measured identity; s204 28903 (R4-13): 3 false quads dropped, +57 pairs, move 0.009 / 0.079 px, base 0.036 / 0.189 vs fix 0.029 / 0.221 px off truth |
| F | runB mono L and s204 mono: every frame bad in base is aligned at the fix, inliers ≥ ½ the group median, ≤ 0.15 px max and ≤ 0.05 px median off truth over the 33 × 23 grid; seed kind reported | emulation 0.06–0.08 max; good frames 0.03 median |
| H | hc11 L / G and every OSC group: a frame bad in base either registers within F's precision at the fix, or fails (its old reason or `LowCoverage`), or keeps inliers ≥ base with max off truth ≤ base + 0.05 px. A frame aligned at the fix with max off truth > 0.15 px and coverage ≥ 0.75 is a STOP (the gate passed a wrong map). Predicted bad: 70062 (31), 69954 (205), 69865 (211), 1 L and 2 G failed outright | research Q2: blunt frames, a different path |
| U | Every group with no moved registration row and no refused frame: master HDU pixel data byte-identical to base (headers carry run cards and hashes), and its per-member LN gaps in the b3 compare equal | integration determinism (Plan A Step 5a; pcores control) |
| M | `pland_bars.py` against the base: B-M (noise ratio ≤ 1.02, FWHM ratio ≤ 1.01), B-LN GROUPS WORSE 0 (PD-8's rule), B-W ρ ≥ 0.9 (expected ≥ 0.99 on the moved groups, 1.0 elsewhere), B-S | Plan D bars |
| S | **Star check under `local` (R-SC-1)**, per moved mono group, on its bad-in-base moved frame with the largest weight (runB: 162; s204: 28994; R4-13: a moved good frame is never S's frame, and a moved group with no bad-in-base moved frame reports S vacuous). The header names the rejection mode base and fix posted, per dataset (`<stage>/<ds>/config.json` `normalization.rejection`; `local` on both by PD-40). A base / fix mismatch is a STOP for U, M and S, never a judged bar (review r1 C1). S1: its rejected fraction (`stats_json.rejected_fraction_per_frame`) at the fix ≤ base + 0.1 pp, and ≤ the max over the group's other F ≤ 2.5 px frames at the fix + 0.25 pp. S2: bright stars (≥ 40 σ) binned by \|e\| = \|base map − truth\| at the star (0–0.5, 0.5–1.0, ≥ 1.0 px; at the `final` base, 162's base map is the 159-inlier one). Each bin is judged against the 0–0.5 bin, so the rule reads "no trend with \|e\|": its master FWHM (fix / base − 1) minus the 0–0.5 bin's ≤ +0.4 %, and its ellipticity Δ minus the 0–0.5 bin's ≤ +0.002. The 0–0.5 bin's own change is reported; a flat LN or weight shift is bar M's, and it cannot fail every bin here (review r1 M3). Core-peak ratio (`core_replay.py`'s) reported. A bin with no stars is reported as vacuous | Q3: floor ±0.4 % FWHM, ±0.002 e; 162 at 4.33 % (d2d) / 2.71 % (d3) vs 2.29–2.36 % (global) |
| D | `"registration kept under half its group's median inliers"` at the fix: runB 0, s204 0, hc11 = its bad-in-base frames that stay aligned under ½ (predicted among 70062, 69954, 69865), set 109 0 | Q2 |
| C | Plan D's runB mono common set / candidates ≥ 0.9 at the fix (d3: 0.96, d2d: 0.54); DF-b's "far under the anchor's candidates" warn 0 times; set sizes vs base reported | PD-39 |
| P | P1: `register_check.py` base vs fix on s204's sharp + broad mono frames: share ≥ 0.98 (expected 1.000). P2: the fix clone's own `.athf` lists, replayed through `regfrag_probe batch` at the fix head, reproduce every quad-seeded frame's row (inliers and transform equal) in every group of runB, s204 and hc11. The replay's per-frame `droppedQuads` is G's `inlier_ratio` input | Plan A Task 14b registration bar; research exactness checks |
| V | Set 109: base run, then a fix run on the same copy. Calibrate and Measure are reused (0 recomputed) and Register recomputes every frame (the `alignVersion` fold). G on every frame; min inliers ≥ 1 414; D = 0; the gate refuses 0; masters byte-identical | research Q2 (set 109) |
| X | **Cross-scale control** (R7b) on the M4b acceptance sets: the set holding frames 74884 / 76261 / 63926 (M4b's set 166: ×0.207 and ×1.457 onto the OSC reference; its id moved since), set 108 (×0.502, bin 1 onto bin 2) and set 195 (×1.286 inside one narrowband group). X1: base and fix run on the same copy; G on every good-in-base frame of any seed kind, and K4. X2: every cross-scale group registers at its plan ratio within ±1 % (M4b: 0.2071, 1.4574, 0.5018, 1.2851). X3: the cross-scale frames replayed with no hint (`--seed robust`, the quad path forced) through the run's own per-frame scale gate (the probe's gate column = `scale_gate_for` at the group's plan ratio; review r2 C1: under the fixed `SCALE_RANGE` every cross-scale replay would fail and X3 could not fail). Three outcomes, each counted: registered within 0.5 reference px of its run's `+wcs` map (max over the 33 × 23 grid points of the subject frame that map inside the reference); failed, by reason; a quad-path map > 0.5 px off with coverage ≥ 0.75 is a STOP. The `ScaleOutOfRange` count must be 0. X4 (reported): set 195's H 448 mm frames against the O reference with no hint, quad-path successes at base and fix (M4b run 16: 14 of 30 failed with "0 inliers") | M4b acceptance run, `docs/superpowers/research/2026-09-11-m4b-acceptance-run.md` |
| N | Noreg lab cycle at the fix head vs the last Plan D cycle step: moved 0; cumulative vs t01c moved 0; inject 109 / 109 | `star_probe`'s `register` path is the star list, not `align` |

## Review Focus

1. **A field where most quad matches are false** (a repetitive or crowded field, a wrong-night pairing, a pair of
   filters). Expected: never an accepted wrong map.
   - A random false share is recovered: 30–85 % on 162's true matches give the true seed exactly
     (`q1/ransac_adv.out`; the clipped estimator was 16–3 321 px off there).
   - A false share at one consistent offset below the true share is recovered (30 %: exact).
   - A consistent false share as large as the true one (≥ 50 % at one offset; review r2: the false group on 5 of 10
     draws at 50 %) is ambiguous to any estimator from the matches alone, and RANSAC may take the false group's seed.
     A compact false group (a dense cluster, a repeated asterism) pairs few stars (`TooFewInliers`), or the coverage
     gate refuses the map. The frame then takes the plate-solve seed or fails.
   - A consistent copy spread over the whole field is accepted by design: no estimator can tell it from the field.
   - Pinned in R1 (`a_random_false_majority_is_recovered`, `a_consistent_false_minority_is_recovered`) and RG2
     (`a_compact_decoy_majority_is_refused`).
2. **A frame with fewer than 8 quad matches** (hc11's blunt frames, sparse narrowband). Expected: today's single fit,
   bit for bit; the gate still judges the map. Pinned in R1 (`a_small_match_set_keeps_the_single_fit`).
3. **A wide-field frame whose true quad matches carry field distortion against the affine.**
   - Up to 5 px the plain fit is clean: bit-identical seed.
   - Beyond 5 px, RANSAC's consensus starts at 5 px and the consensus loop re-admits within max(5 px, 5 × median). A
     smooth distortion converges to every match, so the seed is bit-identical too.
   - Only a strongly non-uniform term (≥ 10 px at the corners against the affine, modelled in
     `q1/seed_distortion.py`) leaves 2–29 of 486 corner quads out. The seed then moves ≤ 8 % of the distortion: a
     narrower seed, not a wrong one.
   - Pinned in R1 (`a_distorted_clean_set_converges_to_every_match_bit_for_bit`).
4. **A meridian-flipped or 2×-binned subject** (negative determinant; quad seed as the cross-scale fallback with
   `after_wcs`). Expected: unchanged outcomes. Pinned by the existing `a_mirrored_subject_is_flipped_not_failed`,
   `the_per_frame_gate_admits_the_binned_frame` and `an_unconfirmed_hint_falls_back_to_the_quad_seed`, which must stay
   green unedited (R1 Step 4, RG2 Step 4), and by X on real data.
5. **A partial overlap** (a mosaic panel, a large dither, a cross-scale footprint) **or a reference with a starless
   part** (clouds, a dense nebula). Expected: the gate counts only the cells whose stars land where the reference has
   stars, so a well-registered frame passes. Pinned in RG2 (`a_partial_overlap_passes_the_gate`,
   `a_cross_scale_subject_passes_the_gate`, `a_reference_with_a_starless_half_refuses_no_subject`); real data: X, K2.
6. **The gate's circularity.** The eligible cells and the overlap grid come from the map itself, so a grossly wrong map
   shrinks its own overlap. Such a map pairs few stars and fails `MIN_INLIERS` first; a compact false group pairs only
   inside its cluster and leaves the rest of its overlap empty. Pinned in RG2 (`a_compact_decoy_majority_is_refused`).
7. **An existing set re-stacked after the update** (fresh cached registration rows from before the fix). Expected: the
   set re-registers once, and Calibrate / Measure are not recomputed. Pinned in R2
   (`the_align_version_rides_the_registration_subtree_only`, the golden re-record) and proven on set 109 (bar V).

## Rulings fixed by this plan

Recorded in `.superpowers/sdd/2026-11-02-regfrag/rulings.md` at R0, and in `docs/stacking/README.md` at R9.

| id | ruling |
| ---- | ---- |
| RQ-1 | **The estimator: deterministic RANSAC over the quad-centre pairs, then least squares over the consensus — not clipped least squares.** A residual is the Euclidean distance between a fit's image of a match's image-quad centre and its catalog-quad centre. (a) The plain fit `fit_affine(all)` is today's call. If n < `MIN_INLIERS`, or every residual of the plain fit is ≤ `SEED_TOL_PX` 5.0 (absolute), it is returned: the clean route, bit for bit. (b) Otherwise RANSAC: `SplitMix64(SEED_RANSAC_SEED)`, samples of 3 distinct matches, `degenerate` samples skipped, a hypothesis is `fit_affine` over the 3 (`None` skipped), its consensus is r ≤ 5 px, a strictly larger consensus replaces the best, the bound N = ⌈ln(1 − 0.9999) / ln(1 − w³)⌉ shrinks with the best share w, exit above w = 0.98, at most `SEED_RANSAC_MAX_ITERATIONS` 2 000. (c) The consensus loop: `fit_affine` over the consensus in the original order; τ = max(5 px, `SEED_LO_MEDIAN_FACTOR` 5.0 × the numpy median residual of the kept set); keep = r ≤ τ over ALL matches; stop when keep repeats, at most `SEED_LO_MAX_FITS` 5 fits. **Measured reasons:** on the research's 1 054 match sets (3 × 83 list frames, 5 OSC, 800 deletions) the kept set equals the measured clipped estimator's on 1 054 / 1 054 (`q1/ransac_seed.out`), so its outcomes stand: 83 / 83 right, 79 bit-identical, 800 / 800 deletions right. Review r2 re-ran the pseudo-code exactly as written (SplitMix64 draw order, `degenerate`, the xy-scale gate on every fit): the same 1 054 / 1 054; routes Clean 49, Consensus 1 005; 1–3 hypotheses; the consensus loop's second fit re-admitted 1–7 true matches the 3-point hypothesis had left beyond 5 px on 8 of 1 005 sets (lists_f1 149: 627 → 634) and never dropped further. Under contamination on 162's 486 true matches (`q1/ransac_adv.out`, 10 draws each): random false 30 / 50 / 70 / 85 % → 0.0 px off the true seed (clipped: 15.8 / 2 261 / 2 793 / 3 321 px); one-offset false 30 % → 0.0 px (clipped 966 px). Review r2 in SplitMix64 order: the same, every true match kept on 10 / 10 draws; the plain fit is `None` at 30 / 70 / 85 %, so RQ-2's branch is what recovers them; at 85 % the 2 000-iteration cap binds (N needed 2 720) and still recovers, deterministically under the fixed seed. The clipped estimator is rejected because its threshold rests on a median that the false matches inflate: past a small false share it clips nothing and keeps the dragged fit — the version that would be thrown away. **The bit-identity condition:** a set whose plain fit is clean keeps it bit for bit; a set whose consensus converges to every match gets `fit_affine(all)` in the original order, the same bits. True matches lost to a strongly non-uniform distortion (≥ 10 px at the corners against the affine) give a narrower seed, not a wrong one. |
| RQ-2 | **Guards, never worse than today.** n < 8 → today's single fit (`None` → `NoSeed`). The plain fit `None` with n ≥ 8 → RANSAC still runs (today: `NoSeed`; review r1 I2 measured 190 / 200 such seeds of a 75 % random false set — RANSAC recovers them). A best consensus under 8 → today's plain fit (`None` → `NoSeed`). A consensus fit returning `None` → the last fit stands. Every outcome is judged by the coverage gate (RQ-10). |
| RQ-3 | **What stays.** The pairing radius 2 × `ransacTolerancePx` (1.9), the single 4b re-pair, `MIN_INLIERS`, `maxStars`, the registration list (`detect.rs`), the plate-solve fallback (R-T6-9), the registration RANSAC after pairing and `solvemyastro` stay. The dials are rejected as measured: tolerance 2.5 is a coin flip (Task-13 lists: 162 / 132 / 141 off 0.78–1.22 px); a FWHM-aware tolerance of 1.0 fails 11 frames; `maxStars` 1 000 halves the inliers (max error 0.12 vs 0.07 px); the detector list keeps the seed 5.7–40.9 px off. Making every frame lead with a plate-solve seed is rejected: 465 of 21 158 lights in the owner's catalog (2.2 %) and 1 659 of 21 170 in the dev catalog (7.8 %) carry a solve, the pipeline does not solve frames, and a frame that does not solve is often a good frame (catalog depth, a wrong FOCALLEN, the defocus gate). |
| RQ-4 | **`seed_matches` and `NoSeed { matches }` keep counting every quad match**, not the kept ones: stored and logged meanings are unchanged. The seed route emits no warning (dropping a false match is ordinary: 61 of 83 frames) and no `Alignment` field. |
| RQ-5 | **`ALIGN_VERSION` 1, folded into `registration_subtree` as `"alignVersion"`** (LN_MODEL_VERSION's precedent, R-M4a-15's reasoning: no config field moves when `align` changes). Policy: one bump per merged plan that changes what `align` returns for an unchanged star list; this plan's seed and gate share version 1. Cost: every set re-registers (and re-normalizes, re-integrates) once; zero extra when it ships in the same release as Plan D's `MEASURE_VERSION` 8. Cost if wrong: one extra re-registration per set. |
| RQ-6 | **The diagnostic is a log line only.** It covers both seed kinds. The median runs over the group's aligned non-reference frames after the persisting pass, cached rows included. A frame with inliers < ½ that median gets one `warn!`, with `model` naming its seed. There is no run warning, no row and no UI. Reason: the defect stood through three catalog stages because nothing counts it; R-T6-9's precedent ("a distinct event makes the pattern countable"). |
| RQ-7 | **The probe goes into core `examples/`** as `regfrag_probe.rs`. Reasons: the offline gate (R5), the coverage gate (RG1) and parity P2 need it at the fix head; probes stay in core and lab tools in athenaeum-lab (memory); about 300 lines, built only with `--example`. |
| RQ-8 | **Truth and the bar rule** (as amended by R4-14, fable). Truth is an 8-parameter homography correction C over the map under judgment, on the registration's own stars: the subject's register list (the list `regfrag_probe fits` / `detect` gives, as the P2 parity lists; never the unsaturated `fitsall` population) mapped through that map by `regfrag_probe mappoints` (its spline included) is paired uniquely with 20 trusted members' register lists mapped through their stored maps, iterated at 6 / 4 / 1.5 / 1.0 px with C refit after each radius; truth = C ∘ map, and the map's error at a grid point q (its `mapgrid` image) is \|C(q) − q\|. For a homography map this equals the direct truth to 0.001 px; for a TPS map it judges the map the run shipped. Trusted = rows bit-identical between base and fix, and inliers ≥ 0.75 × the group median, taken evenly across the group; every trusted member when 20 give < 10 000 pairs; the frame itself is never its own member. A truth with < 5 000 pairs or rms > 0.5 px is weak and reported. Every reading is the pair (hull median, hull max) over the 33 × 23 grid points inside the convex hull of the frame's own paired register stars (subject px); the full-grid max and the hull's point count are reported beside, never judged. The precision bars (F, H, K2, K4, G) read their max / median as hull max / hull median. Truth is computed for moved frames, bad-in-base frames, refused frames and 10 controls per mono group. A control is a good-in-base, unmoved, aligned frame taken evenly across the group; it passes when its truth is not weak and its hull median is ≤ 0.10 px; a control over that, a weak control, or a control whose hull max exceeds 0.30 px is a STOP to fable with its full row — a wrong truth tool, never a map verdict. A frame whose truth is weak is reported to fable and never fires K2 / K4 by itself. Maps are evaluated through `regfrag_probe mapgrid` / `mappoints` (`PixelMap::from_json` + `forward_exact`), never re-implemented in Python. |
| RQ-9 | **The base.** If Plan D's `pland-<ds>-final` clones exist with their comparator kit and `git diff --quiet <final sha> <main> -- crates solvemyastro rustafits` holds, the base is `final`. Otherwise R0 runs `rbase` at `main`. The `final` runs post `local` on hc11, runB and s204 (PD-40), and the `rqs` runs post the same; R4's `PLAND_BASE_RESULTS` reads the base's results where they are (`results/plan-d/final/`). |
| RQ-10 | **The coverage gate.** A map is accepted only if its inliers cover the overlap. The index is `coverage`, in subject pixels. (a) A subject registration star is *eligible* when its image through the refit's linear map lands inside the reference frame (no clamping into edge cells, unlike `regularity`; review r2 M3) in a cell of a `REFERENCE_OCCUPANCY_GRID` 16 × 16 grid over the reference that holds a reference registration star. The occupancy test keeps a reference whose list is empty in part of the frame (clouds, a dense nebula, a vignetted corner) from refusing every subject. (b) A `COVERAGE_GRID` 4 × 4 grid is laid over the bounding box of the eligible stars, so it always spans the overlap: the whole frame for a full overlap, a corner for a small one (a zero-width side of the box is one column or row). (c″) (fable K1 r1, amends (c′) / R4-34) coverage = the grid cells holding a refit inlier ÷ the cells holding an eligible star that the JUDGED map pairs: a subject star with a reference star within the pairing radius (2 × `ransac_tolerance_px`, 3.8 px) of its image through the refit's own linear map, a pairing the gate makes for itself (`pair_through(&refit.linear, …)`) and refits nothing on — never the pairing the refit was fitted on: that one was made through the previous fit (the seed's, or the first refit's under 4b) and reaches only as far as THAT fit was within the radius, so a map fitted on it is read over its own footprint and nothing else (K1 r1: 365 research maps 0.2-1.6 px off truth scored 0.78-1.0 over the 9-12 cells their first fit reached). A cell none of whose eligible stars the judged map pairs is overlap the reference cannot vouch for and counts for nothing; the eligible stars still set the grid's extent. On a dense field a map off by less than the radius pairs the real stars where it is wrong and a map off by more pairs chance stars there: either way those cells hold the denominator and no inlier. On a sparse field with no star to pair outside the part a map is right over it scores 1.0 — R4-34's accepted cost, unchanged. `coverage(subject, reference, inlier_subject_pts, pair_subject_pts, map, geometry)` keeps its signature; `fit_from_seed` and the probe's restated stages pass the fresh pairing's subject sides. `MIN_COVERAGE` = 0.75 (12 of 16 on a dense overlap). **No floor on the cell count (review r2 M4):** a flat floor (≥ 4 of 16 subject cells) would refuse a wide subject on a narrow reference — ×4.8, an overlap of about one subject cell in 16 — which registers today through its plate solve; the grid over the overlap gives that frame its full 16 cells instead, and a map resting on one corner of a large overlap still scores 1 / 16. **Accepted by design:** a consistent false copy spread over the whole field (no estimator can tell it from the field; review r2 I3). It is checked last in `fit_from_seed`, after the 4b re-pair, on every seed kind. A refused quad map falls back to the plate-solve seed when both frames are solved, and a refused plate-solve map to the quad seed (R-T6-9's two fallbacks). A frame refused both ways fails with `AlignError::LowCoverage { coverage, cells }`, "registration inliers cover {coverage:.2} of the overlap (min 0.75)". The switch warnings keep today's two shapes, with the coverage as the reason (review r2 M7): "quad seed failed (map covers 0.13 of the overlap); plate-solve seed used" through `quad_failure_reason`, and "wcs seed pairing failed (map covers 0.13 of the overlap); quad seed used". **Measured reasons:** the reference-grid `regularity` separates the research's maps with a gap — ≤ 0.15 px off truth → 1.0, 0.15–1 px → ≤ 0.5625, > 1 px → ≤ 0.625 — and review r2 measured the subject-frame-grid form of `coverage` equal to it cell for cell on all 1 759 research maps (full overlaps, eligible cells 16 / 16); 0.75 sits in the gap. The overlap-grid form (b) differs from it only by the margin between the outermost eligible stars and the frame edge, so K1 re-pins the shipped form on the same maps. The overlap grid and the eligible stars make the rule hold where the reference grid cannot: a cross-scale subject's footprint covers 1–2 reference cells at ×0.207, a partial overlap leaves reference cells empty by geometry, and a starless part of the reference leaves cells empty by the sky. K2 carries the threshold on real frames before RG2 writes code; RG2's scenes and X cover what the research data cannot; a constant, not a config field. |
| RQ-11 | **The cross-scale control is part of the acceptance** (bar X, R7b): the M4b sets carry the only real cross-scale registrations, and every one of them took the plate-solve seed there, so the quad path at scale is proven only by replay with no hint (X3). |

## Owner decisions (answered)

- **2026-11-02 «Да, все четыре»:** RQ-5, RQ-6, RQ-7, RQ-9 (fable's recommendation in review r1 beside each: yes, yes,
  yes, yes with I3 fixed).
- **2026-11-02 «я бы принял оба пункта»:** the cross-scale control (RQ-11) and a gate against a map that rests on a
  cluster of stars (RQ-10).
- **2026-11-02 «Да, берём»:** the principle in the Goal (a high-breakdown seed, coverage acceptance, the plate-solve
  fallback; no clipped version first). The owner's frame: «мы не настолько богаты чтобы строить mvp и потом его
  выкидывать».

Not open: the star-check mode on runB (C1) is settled by PD-40 (the `final` base runs `local`). Fable also backs the
landing after Plan D's merge, on `robust-seed` from local `main` (Global Constraints).

## What it moves downstream

- **Registration rows.** Only frames bad in base move or fail. Predicted at the merged head (D3's lists, research Q2):
  - runB mono L: 162 (159 inliers), 49 (214), 39 (355), and 31 (today `+wcs`; with the robust quad seed `homography`,
    1 583 at d2d);
  - s204 mono: 28986 (88), 28994 (99), plus the plate-solve-seed frames 28964 (= runB's 162) and 29053, which may switch
    to quads;
  - hc11: the blunt frames under ½ the median may now be refused by the gate when unsolved (RG1 names them).
  - Which sharp frame lands where is the fragility itself, so the bars judge by rule (G / F / H / K4), never by this
    list.
- **Masters.** runB mono L and s204 mono move. A group losing a refused frame moves. Every other group is
  byte-identical (bar U).
- **LN.** Frame 162 is LN's first reference member (PD-39), so runB mono's LN reference and every member's `.athln`
  move. s204 mono's LN moves if a moved frame is among its top-5.
- **Plan D's common set.** It hangs on the anchor's map (162 on runB mono): the set size moves (d2d 5 830 → d3 10 429),
  and so do runB mono's weights (B-W).
- **The star-measurement program's comparators (`pland-*` clones).** `pland-<ds>-rqs` becomes the comparator of every
  later plan (re-baseline in R9). `final` / `rbase` are deleted once R6's results are committed.
- **Users.** The first stack after the update re-registers every set once (RQ-5). The owner's runB / s204 masters change
  only where frames were bad. A frame whose registration rests on a patch of stars is now refused (or registered from
  its plate solve) instead of being stacked slightly off.
- **Lands after** Plan D merges into local `main` (done 2026-11-02, 91df3ff0).

## Tasks at a glance

| task | job | kind | model | depends on |
| ---- | ---- | ---- | ---- | ---- |
| R0 | ledger, rulings, branch, registry rows, the base decision | controller | — | Plan D merged |
| R1 | `robust_quad_seed` in `seed_affine` + the probe in `examples/` | code | rust-engineer (sonnet) | R0 |
| R4 | lab tools: `regfrag.sh`, `regfrag_bars.py`, tests; `pland.sh` owner + comparators | lab code | sonnet | R1 |
| R5 | the offline seed gate on the research lists (O1–O5) | lab | sonnet | R1, R4 |
| RG1 | the coverage measurement (K1, K2) | lab | sonnet | R5 PASS |
| RG2 | the coverage gate in `align` (RQ-10) | code | rust-engineer (sonnet) | RG1 PASS |
| R2 | `ALIGN_VERSION` folded into the registration hash | code | rust-engineer (sonnet) | RG2 |
| R3 | the low-inlier diagnostic + dictionary | code | rust-engineer (sonnet) | R2 |
| R6 | master-level acceptance runB / s204 / hc11 (E G F H U M S D C P K4) | lab | sonnet | R3, base |
| R7 | set 109 control (V) | lab | haiku | R3 |
| R7b | cross-scale control (X) | lab | sonnet | R3 |
| R8 | noreg lab cycle on the runner (N) | lab | haiku | R3 |
| R9 | docs, re-baseline, suites, final review, merge | close | sonnet; final review fable | R6–R8 |

Serial: R5 is the early real-data check of the seed and RG1 of the gate; a FAIL at either stops the plan before more
code. Reviews: `opus` per code task (R1, RG2, R2 + R3 together, R4).

---

### Task R0: Ledger, branch, registry, the base

**Files:** `.superpowers/sdd/2026-11-02-regfrag/{progress.md,rulings.md}`.

- [ ] **Step 1 (controller):** `git switch -c robust-seed` from local `main`; write `rulings.md` (RQ-1…RQ-11 copied,
  with the owner's answers); `disk-report`.
- [ ] **Step 2:** extend the REGISTRY row `~/.athenaeum-acc/regfrag` to 2026-11-30, purpose "+ robust-seed plan inputs (q1
  lists, truth.json, base JSONL, the shore measurements) until R9". Confirm `q1/{lists_d2d,lists_f1,lists}` (83 files
  each), `q1/lists_osc` (5 files), `q1/ref123.tsv`, `q1/truth.json`,
  `q1/v_{base,robust,lists_f1_base,lists_f1_robust,lists_base,lists_robust,osc}.jsonl`,
  `q1/v_drop{162,49,39,74}{,r}.jsonl` and `q1/{ransac_seed,ransac_adv,gate_regularity_truth}.out` exist. The offline
  gate reads only these, never the `pland-*-d2d/d3` clones or `plan-d/d3/growth`.
- [ ] **Step 3:** the base (RQ-9). Read the REGISTRY and the Plan D ledger. If `pland-<ds>-final` hold `athenaeum.db`,
  `stackout/`, `logs/`, `.athf` and `.athln`, and the diff guard holds, then base = `final`. Otherwise
  `PLAND_OWNER=regfrag PLAND_RESULTS=<lab>/star-lab/results/regfrag PLAND_BASE_RESULTS=<lab>/star-lab/results/plan-d
  zsh pland.sh ln-truth rbase <main sha> --base <the newest comparator left> --measure equal` (dev Mac, ~3 h; after R4,
  which adds `rbase` to the comparator stages, `PLAND_BASE_RESULTS` and the migrated rejection mode). If no comparator
  is left at all: STOP to the owner.
  - **The rejection mode (review r1 C1, PD-40).** Read, per dataset, the mode the base's runs posted
    (`<base results>/<ds>/config.json` `normalization.rejection`, and DF's verdict line for `final`) and record it in the
    ledger. Expected: `local` on hc11, runB and s204. A base whose posted mode differs from what the `rqs` runs post
    (`local`) is a STOP: a `local` fix is never compared against a global base for U, M or S.
- [ ] **Step 4:** locate the cross-scale sets in the dev catalog (read-only): the set holding frames 74884 / 76261 /
  63926 (M4b's 166), sets 108 and 195, and whether their M4b subsets (manual exclusions) still stand. Their raw frames
  live on `/Volumes/bigbase2` (review r2 M6). Record the ids, the subsets and the mount in the ledger; a set that is
  gone, or the mount absent when R7b starts, is a STOP to the owner for R7b only.
- [ ] **Step 5:** push `robust-seed` to GitLab; ledger "RESUME HERE".

**Acceptance:** rulings file one page; the base named with its sha in the ledger; the cross-scale sets named; registry
clean.

---

### Task R1: The robust quad seed (core) and the probe

**Coupling checklist:** owns the quad seed (RQ-1, RQ-2) and the coverage index (RQ-10 (a), (b), (c″); review r2 I1: one
computation, built once, read by the probe now and by RG2's gate later). Reads the quad matcher's output
(`build_quads`, `match_quads`, unchanged). Re-verifies: pairing, RANSAC / refit, 4b, the plate-solve fallback, all
unchanged; `coverage` is not called by `align()` yet.

**Files:**
- Modify `crates/athenaeum-core/src/stacking/register/align.rs` (`seed_affine` l.323, the `use solvemyastro::quad`
  line l.10, the new pure `coverage`, its tests module);
- Modify `crates/athenaeum-core/src/geometry/ransac.rs` (`degenerate` → `pub(crate)`, nothing else);
- Create `crates/athenaeum-core/examples/regfrag_probe.rs` from `~/.athenaeum-acc/regfrag/regfrag_probe.rs`;
- Modify `crates/athenaeum-core/Cargo.toml` (an `[[example]]` entry, `required-features = ["render", "solver"]`, beside
  `d0_weight_probe`).

**Interfaces (produces):**
```rust
pub const SEED_TOL_PX: f64 = 5.0;
pub const SEED_LO_MEDIAN_FACTOR: f64 = 5.0;
pub const SEED_LO_MAX_FITS: usize = 5;
pub const SEED_RANSAC_MAX_ITERATIONS: usize = 2000;
pub const SEED_RANSAC_SEED: u64 = 0x5EED_5EED;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRoute { Single, Clean, Consensus }
#[derive(Debug, Clone)]
pub struct QuadSeed {
    pub affine: solvemyastro::quad::Affine,
    pub kept: Vec<bool>,   // per match, in `matches` order: true = in the returned fit
    pub route: SeedRoute,  // Single: n < 8 or no consensus; Clean: the plain fit; Consensus: RANSAC + loop
    pub hypotheses: usize, // RANSAC samples drawn (0 off the Consensus route)
    pub fits: usize,       // fit_affine calls over ≥ 8 matches behind the returned fit
}
impl QuadSeed { pub fn dropped(&self) -> usize }
pub fn robust_quad_seed(matches: &[(usize, usize)], image_quads: &[Quad], catalog_quads: &[Quad]) -> Option<QuadSeed>;

pub const COVERAGE_GRID: usize = 4;
pub const REFERENCE_OCCUPANCY_GRID: usize = 16;
/// RQ-10 (a), (b), (c″): (coverage, cells holding an eligible star the judged map pairs).
/// `inlier_subject_pts` / `pair_subject_pts` are the subject sides of the refit's inlier pairs /
/// the pairs the judged map makes (the pairing carries coordinates, not star indices; review r2 I4).
pub fn coverage(subject: &[(f64, f64)], reference: &[(f64, f64)], inlier_subject_pts: &[(f64, f64)],
                pair_subject_pts: &[(f64, f64)], map: &Linear, reference_geometry: (usize, usize)) -> (f64, usize);
```
`seed_affine` keeps its signature and return (`(Linear, matches.len())`, RQ-4). It calls `robust_quad_seed` and maps
`None` to `NoSeed { matches, after_wcs }` as today.

The algorithm (the only body this plan fixes). The doc comments cite RQ-1's numbers and its bit-identity condition:
```text
n = |matches|; plain = fit_affine(all)                       # today's call, unchanged
if n < MIN_INLIERS: return (plain, all kept, Single)          # None -> None (NoSeed upstream)
if plain is Some and max_i r_i(plain) <= SEED_TOL_PX: return (plain, all kept, Clean)
best = {}; rng = SplitMix64(SEED_RANSAC_SEED); needed = MAX_IT; it = 0
while it < min(needed, MAX_IT):
  it += 1; draw 3 distinct indices (ransac_fit's draw); skip if degenerate(their image centres)
  h = fit_affine(the 3, in draw order); None -> continue
  c = { i : r_i(h) <= SEED_TOL_PX }                           # a non-finite r_i counts as out
  if |c| > |best|: best = c; w = |c| / n
    if w > 0.98: break
    needed = ceil(ln(1 - 0.9999) / ln(1 - w^3))
if |best| < MIN_INLIERS: return (plain, all kept, Single)     # today's outcome, None included
keep = best; fit = fit_affine(keep, original order); None -> return (plain, all kept, Single); fits = 1
loop:
  tau = max(SEED_TOL_PX, FACTOR * numpy_median(r_i(fit) for kept i)); new = { r_i(fit) <= tau }
  if new == keep or fits == MAX_FITS or |new| < MIN_INLIERS: return (fit, keep, Consensus)
  refit = fit_affine(new, original order); None -> return (fit, keep, Consensus)
  fit = refit; keep = new; fits += 1
```

- [ ] **Step 1: the failing tests** (`align.rs` tests).
  - **The quad-level helper.** Synthetic `Quad`s (`star_idx` any, `ratios` 0, `longest` 1) with image centres uniform
    over 6224 × 4168. Truth affine = similarity(0.99227, 180°) centring the field on 6248 × 4176 (runB's two cameras,
    research (c)). Catalog centres = truth + uniform ±0.10 px. `matches[i] = (i, i)`. Grid = 33 × 23 over the subject
    frame.
  - `a_clean_quad_match_set_keeps_the_single_fit_bit_for_bit`: 486 matches (the measured sharp-frame median), one of
    them displaced 4.9 px. Assert `route == Clean`, `dropped() == 0`, and the six coefficients' `to_bits()` equal
    `fit_affine(&matches, ..)`'s.
  - `a_distorted_clean_set_converges_to_every_match_bit_for_bit`: 486 true matches whose catalog centres carry
    + k·ρ², k sized so the plain fit's largest residual is between 5 px and 5 × its median (guard asserted). Assert
    `route == Consensus`, `dropped() == 0`, coefficients bitwise equal to `fit_affine(all)`. The doc comment carries
    review r2's numbers: radial terms of 15 / 20 / 30 px at the corners → Consensus, dropped 0 on 15 / 15 scenes (the
    consensus set's median equals the plain median, so τ re-admits the corners). This is the only test of the
    converge-to-every-match branch: no research set took it (0 of 1 005).
  - `two_far_false_quad_matches_are_dropped_and_the_seed_is_the_true_fit`: 486 true plus 2 false. The false catalog
    centres are truth + (−2 405, 0) for an image centre near (600, 600) and truth + (−4 298, 0) near (900, 3 500).
    - Guard: plain `fit_affine` over all 488 is ≥ 10 px off truth at the worst grid point (measured 42.6–50.0).
    - Assert `route == Consensus`, `kept` false exactly at the two false indices, and coefficients bitwise equal to
      `fit_affine` over the 486 true matches in order.
    - Assert ≤ 0.5 px max over the grid (the measured clean-set bound).
    - Same with only the 2 405 px match: `dropped() == 1`, bitwise equal.
  - `a_small_match_set_keeps_the_single_fit`: 7 matches, one 2 405 px false → `route == Single`, bitwise
    `fit_affine` over all 7.
  - `a_random_false_majority_is_recovered`: 486 true matches plus random false ones (catalog centres uniform over the
    reference) at 30 %, 50 %, 70 % and 85 % of the set, 10 RNG seeds each. Each → `Some`, `route == Consensus`, ≤ 0.5 px
    max over the grid off truth, and `kept` true on every true match. Where plain `fit_affine(all)` is `None`, the
    outcome is still `Some` (RQ-2). Measured: 0.0 px (`q1/ransac_adv.out`).
  - `a_consistent_false_minority_is_recovered`: 486 true plus false matches whose catalog centres all sit at truth +
    (−2 405, 0), at 10 % and 30 % of the set → ≤ 0.5 px max off truth.
  - `the_robust_seed_is_deterministic`: the 50 % random set twice → identical `kept`, `hypotheses` and coefficient bits.
  - `coverage_counts_only_stars_that_land_where_the_reference_has_stars` (RQ-10 (a), (b), (c″), hand-built lists, every star paired): a star
    mapped outside the reference frame, or into a 16 × 16 reference cell with no reference star, is not eligible; the
    4 × 4 grid spans the eligible stars' bounding box (a corner overlap gets 16 cells); a cell with an eligible star and
    no inlier counts against; a zero-width box is one column; a full overlap with inliers in 2 of 16 cells → 0.125.
  - `seed_affine_is_bit_identical_to_the_plain_fit_on_a_clean_scene`: the `recovers_a_similarity_with_outliers_and_jitter`
    scene with 0 outliers. Guard: `robust_quad_seed` over its matches reports `dropped() == 0`. Assert `seed_affine`'s
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
- [ ] **Step 4:** run `cargo test -p athenaeum-core stacking::register`, `cargo test -p athenaeum-core geometry::` and
  `cargo test -p athenaeum-core stacking::`. The new tests PASS. The Review Focus 4 pins pass unedited. List every other
  moved test with its scene's false quad matches; any other move is a STOP. Then run `cargo check -p athenaeum-core
  --no-default-features`. Commit `feat(stacking): the quad seed is a deterministic RANSAC + least squares over the quad
  matches (RQ-1, PD-39)`.
- [ ] **Step 5 (the probe):** copy the research probe.
  - Drop "uncommitted" from its doc.
  - `stages()` additionally emits `robustSeed` (9 numbers), `seedRoute`, `seedHypotheses`, `seedFits` and
    `droppedQuads` (the indices into `quadPairs`), from `robust_quad_seed`. The plain `seed` stays beside them.
  - `--seed plain|robust` (default `robust`) on `batch`: `plain` replays today's path through the research's restated
    stages (plain `fit_affine` seed → pairing → `ransac_fit` / `refit_weighted` → 4b), `robust` the real `align()`. Both
    emit `regularity` (the reference grid) and `coverage` / `coverageCells` per frame, through the core `coverage`
    (never a restatement).
  - A per-frame scale-gate column in the `batch` manifest (review r2 C1): a ratio r gives `scale_gate_for` at r, as a
    run builds it; `-` keeps `SCALE_RANGE`. Today's probe passes the fixed window, so a cross-scale replay could only
    fail.
  - A new arm `mapgrid <in.jsonl> <out.jsonl>`: each line `{"key","transformJson","w","h"}` →
    `{"key","grid":[[x,y,fx,fy],…]}` on the 33 × 23 grid over (0..w, 0..h), through `PixelMap::from_json` +
    `forward_exact`.
  - Adapt the probe to the merged head's APIs if needed.
  - `CARGO_TARGET_DIR=target-agent cargo check -p athenaeum-core --examples --features render,solver`; `rustfmt` the
    file. Commit `feat(probe): regfrag_probe — registration stages, robust seed, coverage and map grid (RQ-7)`.

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
  - `test_g_compares_inlier_ratio_only_where_nothing_was_dropped` (I1): a good frame whose `inlier_ratio` alone moved,
    `droppedQuads` 2 → G PASS with the frame reported; `droppedQuads` 0 → G FAIL;
  - `test_good_in_base_rule` (failed, `+wcs`, < ½ median → bad; else good);
  - `test_a_base_fix_rejection_mode_mismatch_is_a_stop` (C1): base `config.json` `scaleZeroOffset`, fix `local` → `stars`
    and `reg` print STOP for U, M and S, not a verdict;
  - `test_truth_fit_recovers_a_known_homography`: 20 members' fits plus a subject displaced by a known homography and
    0.3 px noise → truth within 0.005 px max over the grid, members excluding the subject;
  - `test_e_bins_and_star_floor` (M3): synthetic masters. One bin's FWHM +0.5 % above the 0–0.5 bin's → S2 FAIL, +0.3 %
    → PASS; a flat +0.5 % on every bin → S2 PASS, with the 0–0.5 bin's change reported;
  - `test_offline_identity_and_mechanism` (two JSONL equal → O1 PASS; one map moved → FAIL; dropped == false set → O5
    PASS, with the false set read against `truth.json` / the base map, never the fix map);
  - `test_coverage_bars` (K1 / K2 / K4): a precise map under 0.75 → K1 FAIL; a 0.3 px map at 0.80 → K1 FAIL; a good
    frame at 0.70 → K2 FAIL with the frame named; a refused frame whose base map is 0.08 px off truth → K4 STOP;
  - `test_cross_scale_bars` (X): a ratio 1.5 % off its plan ratio → X2 FAIL; a no-hint quad map 0.7 px off its `+wcs`
    map at coverage 0.9 → X3 STOP; a no-hint frame failed with `TooFewInliers` → X3 PASS, counted under its reason; a
    no-hint frame failed with `ScaleOutOfRange` → X3 FAIL (the replay used the wrong gate); the 0.5 px is taken only
    over grid points that map inside the reference;
  - `test_numpy_median_convention`;
  - `test_ln_truth_reads_the_base_truth_under_pland_base_results` (`test_pland_sh.py`, its stub pattern; I3): the base's
    `<ds>-truth.csv` only under `$PLAND_BASE_RESULTS/final/<ds>/` → no `stop sources`; `PLAND_BASE_RESULTS` unset → the
    `stop sources 4` of today.
- [ ] **Step 2: `regfrag_bars.py`** subcommands, each writing its file and printing ≤ 30 lines:
  - `offline` (O1–O5 from the probe JSONL, `truth.json` and the research JSONL; O1 includes the OSC batch);
  - `coverage` (K1 from the research lists and deletions under both `--seed` values, labelled by `truth.json`; K2 from
    the `final` clones' replay);
  - `truth` (RQ-8, maps through `regfrag_probe mapgrid`);
  - `reg` (G, F, H, U, K4 per dataset from base and fix clones; G's `inlier_ratio` rule reads `droppedQuads` from
    `parity`'s replay; U is STOP on a base / fix rejection-mode mismatch);
  - `stars` (header: the rejection mode base and fix posted, per dataset, STOP on a mismatch; S1 from `stats_json`; S2
    ports `q3/master_shape.py`: master grid ↔ reference pixels by WCS-seeded star matching, adaptive Gaussian moments,
    the \|e\| bins judged against the 0–0.5 bin);
  - `parity` (P2, over every group's quad-seeded frames; it writes the per-frame `droppedQuads` G reads and the
    `coverage` K2 reads);
  - `xscale` (X1–X4 from the cross-scale copy's base and fix databases and the no-hint replay);
  - `logs` (D and C counts, and the `LowCoverage` refusals, from the clone's JSONL logs).
- [ ] **Step 3: `regfrag.sh`** (planb / pland pattern; verdict line 1 `REGFRAG <JOB>: PASS|FAIL|STOP (...)`; heavy steps
  under `heavy_lock.py`; exit codes as `pland.sh`):
  - `offline <sha>` (dev Mac): release build of `regfrag_probe` at <sha> into `.superpowers/target-acc` (guard: checkout
    at <sha>). Then the three list-set batches and the OSC batch (`lists_osc`) with no hint, the 800 deletions and
    `regfrag_bars.py offline`. Data in `~/.athenaeum-acc/regfrag/offline/`.
  - `coverage <sha>` (dev Mac): the same probe build; the research lists and deletions under `--seed plain` and
    `--seed robust`; `parity` over every group of `pland-{runB,s204,hc11}-final` (read-only); `regfrag_bars.py
    coverage`. Data in `~/.athenaeum-acc/regfrag/coverage/`.
  - `accept <sha> --base <stage>` (dev Mac, PD-12): `PLAND_OWNER=regfrag PLAND_RESULTS=…/results/regfrag
    PLAND_BASE_RESULTS=<base results> zsh pland.sh ln-truth rqs <sha> --base <stage> --measure equal`, where `<base
    results>` is `…/results/plan-d` for `final` and unset (`$RES`) for `rbase`. Then, per dataset, `parity` (first: G
    reads its `droppedQuads`) / `truth` / `reg` / `stars` / `logs`,
    and P1 (`register_check.py <out> base=<star_probe@base> fix=<star_probe@sha> --acc ~/.athenaeum-acc/pland-s204-rqs
    --run-id <its run>`).
    - The base `star_probe` is built in a throwaway worktree `~/.athenaeum-acc/acc-base-regfrag-<sha8>` with target
      `.superpowers/target-acc/tmp-regfrag`. Both get REGISTRY rows and both are removed at R9 (§7).
  - `set109 <base-sha> <sha>` (dev Mac): `prepare-catalog.sh` copy under `~/.athenaeum-acc/regfrag/set109` (REGISTRY
    row). Release `athenaeum-web` at <base-sha>, one run (`tier1-run.sh`), `athenaeum.db` snapshotted to `base.db`. Then
    the web build at <sha>, one run on the same copy, then `reg` (G, D, V, U, K4).
  - `xscale <base-sha> <sha>` (dev Mac): a `prepare-catalog.sh` copy of the dev catalog under
    `~/.athenaeum-acc/regfrag/xscale` (REGISTRY row; estimate ~60 GB of stacking data) holding sets 258, 108 and 195
    (R0-2). The dev catalog keeps neither M4b's subsets nor any plate solve of these sets, so on the copy only: 30
    lights per group under test, evenly by DATE-OBS (258: OSC 2600MC Duo, 6200MM L bin 1 at 1960 mm, 294MM L bin 2;
    108: L bin 1, L bin 2; 195: H and O at 352 and 448 mm), the rest in the set's exclusion list; the kept lights are
    plate-solved through the app's own batch command (as M4b did) before the base run. Per set, one co-registered run at <base-sha> (Auto reference, as M4b runs 15 / 17 / 20),
    `base.db` snapshot, one run at <sha> on the same copy; then the no-hint replay of every cross-scale frame and of set
    195's H 448 mm frames through `regfrag_probe batch` at <sha>, each manifest line carrying its group's plan ratio in
    the scale-gate column, and `regfrag_bars.py xscale`.
  - `report`; `clean <job>`, which runs only once the job's results are committed.
- [ ] **Step 4:** tests green; commit and push lab `main`.

**Acceptance:** lab tests green; `regfrag_bars.py offline` on the research's own `v_robust*.jsonl` as the "fix" input
reads O2 / O4 PASS, and `regfrag_bars.py coverage` on the research's base JSONL reads the shore separation (the tools'
calibration). Budget ≤ 140 calls.

---

### Task R5: The offline seed gate (dev Mac, lab agent sonnet)

- [ ] **Step 1:** the base-JSONL guard (review r1 M4): O1's base JSONL was made at the research head, so
  `git diff --quiet 922725a4 <R1 parent> -- crates/athenaeum-core/src/stacking/register solvemyastro` must hold. If it
  does not, re-make `v_base`, `v_lists_f1_base`, `v_lists_base` and `v_osc` at the R1 parent before this step's run,
  and O1 reads those. Then `zsh regfrag.sh offline <R1 head>`.
- [ ] **Step 2:** report O1–O5. Also the per-frame route and dropped count per list set and the OSC batch (expected on
  `lists_d2d`: 61 frames > 0, max 3; OSC: 3 of 5 > 0), and 162 / 31 / 49 / 39's inliers beside the emulation's
  1 616 / 1 583 / 1 614 / 1 612. Commit results to lab.

**Acceptance:** O1, O2, O4 and O5 PASS; O3 reported. Any miss is a STOP to fable before RG1, with the frame's stage row
(`droppedQuads`, route, seed errors). Budget ≤ 60 calls.

---

### Task RG1: The coverage measurement (dev Mac, lab agent sonnet)

- [ ] **Step 1:** `disk-report`; no compile running. `zsh regfrag.sh coverage <R1 head>`.
- [ ] **Step 2:** truth maps (RQ-8) for every K2 frame under 0.75. At RG1 no fix clone exists, so trusted = the base's
  members with inliers ≥ 0.75 × the group median, taken evenly, the frame itself excluded.
- [ ] **Step 3:** report:
  - K1, a pin: the four counts (precise / imprecise × above / below 0.75) under both `--seed` values, and the maps
    where `coverage` and the reference-grid `regularity` differ by more than one cell.
  - K2, which carries the threshold: per dataset and group, frames, the minimum coverage and the frames under 0.875.
  - hc11 per frame: coverage, paired cells, inliers, registration stars, the below-½ frames included.
  - The frames the gate would refuse, each with its truth error.
  - Commit results to lab.

**Acceptance:** K1 and K2 PASS → `MIN_COVERAGE` 0.75 stands for RG2. Each of these is a STOP to fable, with the frame's
cells and its map's truth error:
- a good-in-base frame under 0.75;
- any frame under 0.75 whose map is ≤ 0.15 px off truth (the gate would refuse a precise map);
- a map > 0.15 px off truth at ≥ 0.75.

The lab agent proposes no threshold. Partial overlap and cross-scale stay with RG2's scenes until X. Budget ≤ 60 calls.

---

### Task RG2: The coverage gate in `align` (RQ-10)

**Coupling checklist:** owns the gate (RQ-10's acceptance and fallback); reads R1's `coverage`, the refit inliers and the
4b pairing; the fallback of R-T6-9 consumes its refusal. Re-verifies: every frame that passes the gate keeps today's
map, warnings and error texts bit for bit.

**Files:**
- Modify `crates/athenaeum-core/src/stacking/register/align.rs` (`align()` steps 2–4b → `fit_from_seed`; the gate;
  `AlignError::LowCoverage` with its `Display` and `quad_failure_reason`; `Alignment.coverage`; tests);
- Modify `crates/athenaeum-core/src/stacking/register/frame.rs` (`"frame registered"` gains `coverage`);
- Modify the logging dictionary in `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`.

**Interfaces:**
```rust
pub const MIN_COVERAGE: f64 = 0.75;
AlignError::LowCoverage { coverage: f64, cells: usize }
```
`fit_from_seed(seed, …) -> (Pairing, Result<(RansacResult, RefitResult, LinearKind, usize /*repaired*/), AlignError>,
Vec<String> /*warnings*/)` is steps 2–4b exactly as today, plus the gate last: `coverage(sub_pts, ref_pts, the
refit's inlier pairs' subject points, the subject points of a pairing made through `&refit.linear` (K1 r1), &refit.linear, reference_geometry)`. `align()` calls it for the leading seed and,
on any `Err`, for the fallback seed as today. A discarded attempt's warnings are dropped. The switch warnings keep
today's shapes with the coverage as the reason (RQ-10, review r2 M7): "quad seed failed (map covers 0.13 of the
overlap); plate-solve seed used" and "wcs seed pairing failed (map covers 0.13 of the overlap); quad seed used".

- [ ] **Step 1: the failing tests** (`align.rs` tests):
  - `a_band_supported_map_is_refused_for_low_coverage`: `fit_from_seed` on the decoy scene with a seed that is right
    only within a band (scale error 2e-3 about one corner, so pairing at 3.8 px holds only near it) →
    `Err(LowCoverage { coverage < 0.75, .. })`.
  - `a_reference_with_a_starless_half_refuses_no_subject`: the decoy scene with every reference star in the right half
    removed (a cloud) → `Ok`, `coverage ≥ 0.75`, within 0.1 px of truth over the left half.
  - `a_low_coverage_wcs_map_falls_back_to_the_quad_seed`: the same band hint, `WcsFirst`, the full decoy scene → `Ok`
    with `seed == Quads`, within 0.1 px of truth, one warning of the shape "wcs seed pairing failed (map covers … of
    the overlap); quad seed used".
  - `a_compact_decoy_majority_is_refused` (review r2 I3): a dense cluster of subject stars inside ≤ 2 of the 16
    subject cells, copied into the reference at one offset (−2 405, 0) with enough copies that its consistent false
    quad matches outnumber the true ones; no hint → `Err(LowCoverage | TooFewInliers)`, never `Ok` with a map > 1 px
    off truth. The doc comment states the other half of RQ-10: a copy spread over the whole field is accepted by
    design.
  - `a_partial_overlap_passes_the_gate`: subject and reference overlapping by 40 % of the frame (a mosaic step) → `Ok`,
    `coverage ≥ 0.75`, within 0.1 px of truth.
  - `a_cross_scale_subject_passes_the_gate`: a subject at ×0.207 onto the reference with a confirmed hint (`WcsFirst`)
    → `Ok`, `coverage ≥ 0.75`.
  - `a_wide_subject_on_a_narrow_reference_passes_the_gate` (review r2 M4): a subject at ×4.8 (its field about five
    times the reference's) with a confirmed hint → `Ok`, `coverage ≥ 0.75` over the 16 cells of its small overlap.
  - `a_sparse_field_passes_the_gate`: stars in 5 cells only, all paired → `coverage == 1.0`.
  - `every_existing_scene_keeps_its_map_bit_for_bit`: the existing `align` tests' scenes that end `Ok` (the decoy
    scenes, the jitter scene, the binned and mirrored scenes) re-run → the same map bits as before RG2 (recorded in the
    RED step from the R1 head) and `coverage ≥ 0.75`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement.
- [ ] **Step 4:** `cargo test -p athenaeum-core stacking::register`, then `cargo test -p athenaeum-core stacking::`;
  Review Focus 4's pins pass unedited; `cargo check -p athenaeum-core --no-default-features`; `cargo check -p
  athenaeum-core --examples --features render,solver`.
- [ ] **Step 5:** dictionary entry: "**Dictionary extension (registration coverage, `stacking::register::frame`, RQ-10,
  2026-11-xx):** `coverage` (f64; the share of a 4 × 4 grid over the subject frame whose cells holding an eligible star the judged map pairs hold a refit
  inlier; RQ-10 (c″), K1 r1) on the debug event `"frame registered"`." Commit `feat(stacking): a registration map must cover the overlap
  (RQ-10)`.

**Acceptance:** tests green; the existing-scene pin holds; the opus review is clean. Budget ≤ 100 calls.

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
  - bars E, G, F, H, U, M, S, D, C, P, K4;
  - the moved-row list per group (frame, inliers base → fix, seed kind, coverage, truth max / median);
  - the refused and re-seeded frames per group, with their base map's truth error;
  - runB mono common-set size and weights ρ;
  - frame 162's rejected fraction base → fix;
  - the LN members that moved.
  - Commit the results to lab.

**Acceptance:** every bar PASS. A moved good-in-base frame (G), a refused frame with a precise base map (K4), a wrong
map the gate passed (H), a control over RQ-8's control bar (R4-14), a base / fix rejection-mode mismatch, or any input unread is a STOP
to fable with the frame's probe stage row and both maps. Budget ≤ 150 calls.

---

### Task R7: Set 109 control (dev Mac, lab agent haiku)

- [ ] **Step 1:** `zsh regfrag.sh set109 <main sha at R0> <R3 head>`.
- [ ] **Step 2:** bar V (with G, D, U and K4 inside it); commit the results.

**Acceptance:** V PASS. Hard STOP: any gate off means no re-run with other flags and no code edit; report the gate and
≤ 10 lines of evidence. Budget ≤ 60 calls.

---

### Task R7b: Cross-scale control (dev Mac, lab agent sonnet)

- [ ] **Step 1:** `disk-report`; `df` ≥ 60 GB + 150 GB; no compile running. `zsh regfrag.sh xscale <main sha at R0>
  <R3 head>`.
- [ ] **Step 2:** report X1–X4 per set and group: aligned counts base / fix, seed kinds, scales against the plan ratio,
  coverage minimum, the no-hint quad-path outcome per cross-scale frame (registered within 0.5 px of its `+wcs` map /
  failed / STOP), and set 195's H 448 mm quad-path successes base → fix. Commit the results.

**Acceptance:** X1–X3 PASS; X4 reported. A STOP goes to fable with the frame's stage row and both maps; no re-run with
other flags. Budget ≤ 80 calls.

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
- **README:** a paragraph "**Robust quad seed and the coverage gate (2026-11)**" after the R-SC-1 paragraph: RQ-1…RQ-11,
  the R5 / RG1 / R6 / R7 / R7b / R8 lines with lab commits, and what moved. `regfrag_probe.rs` joins the dev-probe list
  under **Key files**.
- **CLAUDE.md:** the Stacking line gains "registration's quad seed is a deterministic RANSAC + least squares over the
  quad matches, and a map must cover the overlap (`ALIGN_VERSION`, RQ-1 / RQ-10)".
- **open-items:** delete any PD-39 fragility entry and the PD-39 anchor-map warn's "until RQ-6 lands" note; add the
  owed smoke "re-stack runB / LDN1272 and set 204 on the owner's catalog after the update: every mono frame ≥ ½ its
  group's median inliers, no frame refused for coverage".

- [ ] **Step 1: re-baseline.**
  - Strip `pland-<ds>-rqs` with `pland.sh clean rqs`: the comparator kit stays.
  - Re-label their REGISTRY rows as the star-measurement program's comparator. The expiry is the next plan's, set by the
    controller.
  - Delete `final` / `rbase` clones and rows (superseded). `git worktree remove` the `acc-base-regfrag-*` worktree, delete
    `.superpowers/target-acc/tmp-regfrag`, and delete both rows.
  - Delete the set 109 and cross-scale copies once their results are committed, with their rows.
  - Cut `~/.athenaeum-acc/regfrag` to nothing once R5–R8 results are committed: the probe now lives in the repo and the
    results in lab. Delete its row.
  - `disk-report` clean.
- [ ] **Step 2: suites** (dev Mac, single runs):
  - `cargo test -p athenaeum-core --all-targets`, `cargo test -p athenaeum-web`;
  - `cargo check -p athenaeum-core --no-default-features`, `cargo check -p athenaeum-tauri`;
  - `cargo check -p athenaeum-core --examples --features render,solver`.
  - The controller re-runs them.
- [ ] **Step 3:** docs as above. Release-notes line owed: "Very sharp frames registered onto a reference from another
  camera no longer land a few pixels off. A frame whose registration rests on a small patch of stars is now refused, or
  registered from its plate solve when it has one, instead of being stacked slightly misaligned. The first stack after
  the update re-registers every set once."
- [ ] **Step 4:** the final whole-branch review on `fable`, hunting the seams:
  - the bit-identity claim (the clean route; a consensus that converges to every match);
  - the guards (< 8 matches, no consensus, a `None` fit);
  - `seed_matches` meaning;
  - `fit_from_seed`: a frame that passes the gate keeps today's map and warnings; the fallback on a refusal;
  - the gate's paired cells (partial overlap, cross-scale, a grossly wrong map);
  - the hash fold's reach (calib / meas untouched);
  - the diagnostic's reference filter and cached rows.
- [ ] **Step 5:** merge `robust-seed` into local `main`; core suite on the merged tree; push only on the owner's word.
  The controller updates the memory pointers (comparator = `rqs`).

**Acceptance:** suites green; review clean; merged locally; registry and disk clean. Budget ≤ 100 calls.

## Owned quantities at close

| quantity | owner | consumers | bar | after value |
| ---- | ---- | ---- | ---- | ---- |
| the quad seed (RANSAC + consensus LS) | R1 (RQ-1, RQ-2) | pairing, RANSAC, every registration row | O1–O5, G, F, X3 | R9 |
| the coverage index | R1 (RQ-10 (a), (b), (c″)), the one computation | the probe, RG1, the gate | K1, K2 | R9 |
| the coverage gate | RG2 (RQ-10), its threshold carried by RG1's K2 | `align`'s acceptance and fallback; the `"frame registered"` log | K4, H, X | R9 |
| the registration hash's `alignVersion` | R2 (RQ-5) | every downstream stage hash | V, goldens | R9 |
| the low-inlier diagnostic (`median_inliers`) | R3 (RQ-6) | logs only | D | R9 |
| truth maps of a group's frames | R4 `regfrag_bars.py truth` (RQ-8), the one truth reader | F, H, S, K4 | controls: hull median ≤ 0.10 px, hull max ≤ 0.30 px, truth not weak (R4-14) | R6 |
| the registration list (`stars_from_fits`, the saturation cut) | Measure / `detect.rs`, unchanged | the seed | P1, E | — |
| masters, LN, Plan D's common set and weights | downstream, re-measured only | the program's comparators | M, U, S, C | R6 |

## Execution notes

- **Models:**
  - rust-engineer `sonnet` for R1, RG2, R2, R3;
  - lab `sonnet` for R4, R5, RG1, R6, R7b (judgment: registration moves by design);
  - `haiku` for R7 / R8 (proof runs, hard STOP);
  - `opus` reviews per code task; the final review `fable`; a missed bar → `fable`.
- **Order:** R0 → R1 → R4 → R5 → RG1 → RG2 → R2 → R3 → R6 → R7 → R7b → R8 → R9. R5 and RG1 run before the gate and
  the hash, so a real-data defect in the estimator or the gate costs one task, not four.
- **Dev Mac:** R6, R7 and R7b never run beside a compile, an implementer or each other. Heavy runs take `heavy_lock.py`.
- **Spend, rough:** R1 an implementer, a review; R4 a lab agent and a review; R5 and RG1 one lab job each; RG2 an
  implementer and a review; R2 + R3 an implementer each and one review; R6 one long lab job; R7 one proof job; R7b one
  lab job (~1 h of runs); R8 one proof job; R9 a docs agent, the suites, the fable review. About 16 agent phases, plus
  R0's `rbase` (~3 h) if RQ-9 needs it.
- **Controller:** reads only the reports and re-runs every gate itself. It restarts at task boundaries, keeps "RESUME
  HERE" current, and runs `disk-report` at each boundary.
