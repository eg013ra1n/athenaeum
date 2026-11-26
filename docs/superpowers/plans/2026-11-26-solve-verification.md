# Solve verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repository the `lean-execution` skill overrides subagent-driven-development where they conflict (fresh agent per phase, handoff ≤ 1 page, report ≤ 80 lines, ledger, disk rule §7).

**Goal:** the solver accepts a solution only when a Bayes factor over the image stars says "real, not chance" at 10⁹ ([1], §2.4); the number travels on the solution, is stored and shown; the gate keeps backstops and re-reads it; validated in the lab before solvemyastro 0.4.0 and the Athenaeum bump.

**Architecture:** solvemyastro gains the decision (`verify::scan_log10_odds`) at the point where the bright-star completeness test sat, on the final (SIP) WCS, with each hypothesis's own image stars set aside and one hypothesis count per solve; `accept` loses the ratio rule and reads `log10_odds`. A lab tool in athenaeum-lab drives the branch's public API (provoked false solves, cloudy and narrow frames, the whole library interleaved with 0.3.0) against §7's bars. Athenaeum then pins 0.4.0, stores `log10_odds`, clears the buggy in-field count, bumps the solve recipe to 3 and shows the odds in the metadata pane.

**Tech Stack:** Rust 1.96.1 (solvemyastro, athenaeum-core), rusqlite, serde/serde_json, ts-rs 12, React/TS + vitest, Python 3 (lab verdict), GitHub Actions (solvemyastro release), GitLab EVO-X2 runner (`wintest/`).

**Spec:** `docs/superpowers/specs/2026-11-26-solve-verification-design.md` (owner calls SV-1…SV-11 settled, §3; SV-8 a contingency). Ids V-1…V-7 and §-numbers are the spec's. Inputs: `~/.athenaeum-acc/gate-measure/{builder-check.md,scan-report.md,fable-analysis.md}`.

## Summary

| Phase | Task | Repo | Deliverable | Agent / model | Gate to the next |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 0 | Setup | both | branch, job folder, tiers, packs, base test counts, owner asked for frames | controller | — |
| S | S1 | solvemyastro | corpus gate records every bench WCS, tiers pass and a blind index pass; baseline frozen at 0.3.0 | rust-engineer / sonnet; review opus | base corpus PASSED, 0 moved |
| S | S2 | solvemyastro | own image stars per hypothesis (spiral, recentred, index); H per solve; `solve_bounded`; own-set measurement on the corpus | rust-engineer / sonnet; review opus | 0 moved; no truth frame under 4 inliers outside its own set (else the owner) |
| S | S3 | solvemyastro | `verify::scan_log10_odds`, bar, clause (pure, unit-tested) | rust-engineer / sonnet; review opus | unit tests |
| S | S4 | solvemyastro | the decision replaces the completeness test; API fields; `NotConfident`; trace for the lab | rust-engineer / sonnet; review opus | 0 moved; new corpus asserts |
| S | S5 | solvemyastro | gate re-reads the odds, names its refusal; CLI JSON/text | rust-engineer / sonnet; review opus | unit tests |
| S | S6 | solvemyastro | CHANGELOG/README/doc comments, 0.4.0 + standalone lock, name scan, re-freeze | rust-engineer / sonnet; review opus | **Gate S** (whole-branch review, most capable model; owner approval A: push + CI) |
| L | L1 | athenaeum-lab | `sv-lab` (arms, packs, renderer, truth derivation, old-test recompute), manifest, verdict | rust-engineer / sonnet; review opus | tool tests |
| L | L2 | athenaeum-lab | `sv-cycle.sh` one command per cycle; smoke cycle with the V-2 yield projection | sonnet (judgment run); review opus | **Gate L** |
| R | R1 | athenaeum-lab | full run against V-1…V-7 (waits for SV-9's minimum set) | sonnet (judgment run); controller re-gates | **Gate R** (all PASS → owner approval B; any FAIL → owner) |
| P | P1 | solvemyastro | merge (merge commit), v0.4.0 tag, crates.io by its workflow, verified | controller | 0.4.0 on crates.io |
| A | A1 | Athenaeum | pointer bump to the tag; gate/config/failure/recipe 3; TS for the config | rust-engineer / sonnet; review opus | touched tests |
| A | A2 | Athenaeum | `plate_solves.log10_odds`, guarded clearing step, record, TS | rust-engineer / sonnet; review opus | touched tests, ts_contract |
| A | A3 | Athenaeum | metadata pane: confidence and the true denominator; batch panel styles `NOT_CONFIDENT` as a warning | frontend-dev; review opus | vitest, tsc, app launched |
| A | A4 | Athenaeum | docs of §8, CLAUDE.md clause, logging dictionary | sonnet; review opus | — |
| A | A5 | Athenaeum | full gates, `wintest/` run, whole-branch review (most capable model), merge | controller | owner approval C for `main` |

## Global Constraints

- Decision (spec §6.1, verbatim): per test star `ln(p_F/p_B) = ln( d + (1 − d) · A / (2π σ² N_R) · exp(−r² / 2σ²) )`; `r` = distance to the nearest unclaimed reference; beyond 3σ (fixed) the star adds `ln d`; `d = 0.25` (`SolveConfig::distractor_fraction`); `σ` = the matching tolerance `tol_px`, never fitted; `A = W × H` px²; a reference explains at most one test star; `log10_odds = max_n L_n / ln 10`; accept `log10_odds ≥ 9 + max(0, log10(H / 10⁶))`; H = hypotheses verified so far in this solve, all passes.
- Not changed (§2 non-goals, §6.2 "Stays"): quad matching, FOV ladder, the index agreement rule, the seed pre-filter at ln 10⁶ with its 4-inlier and 3σ backstops, `MIN_ABSOLUTE_INLIERS` 6, the verify cone and its G 12 / 14 / full ladder, detection and its 600 cap, tolerances, refit, SIP, registration's own inlier ratio, the `use_bayesian_verify = false` path.
- Gate order (§6.3): RMS ≤ 2.5 × `gate_tol_px`; matched ≥ 6 (final WCS); `log10_odds ≥ verify::ACCEPT_LOG10_ODDS` (9); scale 0.05–60″/px; scale within 8 × the header's. Code `REJECTED_LOW_CONFIDENCE` stays; solver-side refusal class `NOT_CONFIDENT`.
- `SOLVE_RECIPE_VERSION` = 3 (SV-7). Stored `plate_solves` verdicts are not re-judged; the one stored-row change is §6.5's guarded step setting `expected_catalog_stars_in_fov` to NULL (SV-6). Rows stored before keep their `matched_stars` and `inlier_ratio` (SV-11).
- No Settings change, no Tauri command change, so no Axum route change (two-backend rule satisfied by construction).
- No program (solver, stacker, processing tool) is named anywhere: code, comments, tests, docs, commits, CHANGELOG. Cite methods as spec §11 does: [1] authors, AJ 139, 1782–1800 (2010), arXiv:0910.2233, §2.4 (no title); [2], [3] with their titles. The doc comments that call a formula "of a common open solver" (`verify.rs:1,28,34`, `lib.rs:177`, `orchestrate.rs:2042`, `diag.rs:184`) are rewritten to cite [1].
- Tests pass on macOS, Linux and Windows; names use letters, digits, `_`, `-`; paths built with `Path::join`; no bare path compared with log text; a timing bound is a hang detector only.
- Athenaeum production code: `tracing` only; never swallow an error (log, then return); new log field names go into `docs/logging/README.md` (A4).
- Disk: job data in `JOB=~/.athenaeum-acc/solve-verify/` with a `REGISTRY.tsv` row; agent builds use `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent`, lab release builds `…/athenaeum/.superpowers/target-acc`; nothing else.
- 16 GB dev Mac: one implementer (anything that builds or tests) at a time; `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4`, `-- --test-threads=4`; lab commands under `heavy_lock.py` and `memrun.sh` (Setup); never analyse beside a run.
- Repos and branches: `W=…/athenaeum/.claude/worktrees/solve-verification` (Athenaeum branch `solve-verification`); solvemyastro work in `$W/solvemyastro` on branch `verification` (from `c606daf` = v0.3.0). During phases S, L, R Athenaeum's `Cargo.lock` in `$W` may change from `cargo test -p solvemyastro`; it stays uncommitted until A1. Athenaeum is never built against the branch in phases S–R.
- Commits: author `eg013ra1n`, with the session's `Co-Authored-By` and `Claude-Session` trailers, both repositories and athenaeum-lab. solvemyastro merges to its `main` with a merge commit; Athenaeum pins only the tagged commit on solvemyastro `main`.
- Outward actions on the owner's word only, grouped at approvals A (push `verification` to GitHub, CI), B (solvemyastro `main`, tag, crates.io), C (Athenaeum `main`, GitLab then GitHub). Free: solvemyastro branch to `gitlab`, Athenaeum `wintest/*` to `origin`, athenaeum-lab `main` (pre-authorized).
- Corpus gate command (named `CORPUS` below), run from `$W`: `SOLVEMYASTRO_TIER_DIR=$JOB/tiers SOLVEMYASTRO_BENCH_DIR="/Volumes/BigMac/Users/astrobureau/Pictures/star test/singles" SOLVEMYASTRO_INDEX_DIR="$JOB/packs/base" QUIET=1 CARGO_TARGET_DIR=<repo>/target-agent CARGO_BUILD_JOBS=4 cargo test -p solvemyastro --release --test corpus_bench corpus_layered_tiers -- --ignored --nocapture 2>&1 | grep -E "GATE|WCS gate|SKIP|FAIL|panicked"`. It passes only when it prints `GATE: layered-tier corpus PASSED` and a `WCS gate:` line; a `SKIP` line is a failure of the command, not a pass. The corpus frames and the owner's recorded false alignments live in `/Volumes/BigMac/Users/astrobureau/Pictures/star test/singles` (`corpus_bench.rs` defaults to a folder that does not exist): every corpus command in this plan (S1, S2 Step 5, S4, S6, Gate S, L2) sets `SOLVEMYASTRO_BENCH_DIR` to it, and the files there are read in place, never written or copied (C2). The blind index pass (S1) needs `SOLVEMYASTRO_INDEX_DIR`; without it that part prints `SKIP` and the command fails.

## Review Focus

1. **No reference star in the frame** (sparse narrow field, all verify stars off-frame after SIP): `N_R = 0` must refuse cleanly (−∞), never divide (`inlier_ratio`), never classify `NotConfident` on a non-finite best. Tests: S3 `degenerate_inputs_are_never_confident`, S4 `references_in_frame_keeps_finite_in_frame_projections`, S4 `a_non_finite_decision_falls_back_to_the_old_classes`.
2. **Junk at the top of the SNR order** (hot pixels, a saturated star split in two, cloud edges): two test stars on one reference — the second is a distractor; the maximum is not lowered by later junk. Tests: S3 `a_reference_explains_one_test_star`, `the_scan_peaks_at_the_running_maximum`.
3. **Exact edges** (r = σ, r = 3σ, odds exactly 9, H exactly 10⁶): inclusive rules pinned. Tests: S3 `the_cut_offs_are_inclusive_at_one_and_three_sigma`, `the_bar_rises_beyond_a_million_hypotheses`; S5 `gate_boundaries_are_pinned`.
4. **A catalog or cache written by 0.3.x** (stored config with `blind_min_inlier_ratio`, cached SOLVED payload without `log10_odds`, rows with in-field 0/1): decodes, re-solves once, reads as unknown. Tests: A1 `a_stored_config_with_the_ratio_key_decodes_and_saves_without_it`, A2 `a_cached_solve_without_odds_decodes`, `the_guarded_step_adds_log10_odds_and_clears_the_old_in_field_count`.
5. **An old row in the pane**: never "15 / 0" nor "odds 10^NaN". Tests: A3 `shows a dash for a solve stored before the change`, `never prints a zero denominator`.
6. **A thin frame whose own set holds every matched star**: the scan then has no star to score and returns −∞, a refusal by construction against SV-10's "accept on A alone". S2 Step 5 measures `inliers_outside_own` on every corpus truth frame; one under 4 goes to the owner before S3; L2's smoke repeats it for the quick set.

## Plan calls (refinements of the spec, for the reviewer and the owner)

| Id | Call | Why |
| ---- | ---- | ---- |
| PC-1 | The hypothesis's own stars, in SNR order, each claim their nearest unclaimed reference within 3σ before the scan and add no term. `matched_stars` = own stars within 1σ + scanned stars within 1σ: one assignment, the count the decision made. | §6.1 "their catalog stars are claimed from the start": a quad's catalog stars come from another list than R (spiral: the quad-match cone at G 19, `orchestrate.rs:1626-1678`; index: pack stars), so they are claimed through their image stars on the final WCS. |
| PC-2 | Own stars: spiral seed = every image star of every filtered quad match the seed came from (SVD and PROSAC paths alike, `orchestrate.rs:441-478`); recentred seed = that set ∪ its re-fit's matches; index hypothesis = its quad ∪ the earlier quad it agreed with (`:1816-1835`). | §6.1 per path; PROSAC picks among all filtered matches, so all of them chose the seed. |
| PC-3 | The four-star variant V-2 reports is computed per candidate into the trace: spiral = the image quad of the first filtered match; index = the hypothesis quad. | V-2 "with four stars set aside and with §6.1's set". |
| PC-4 | H is counted at entry to `verify_trials` for every candidate (recentred included) across steps A, B1, B2 **and C** of `solve_with_index`. | §6.1 "all passes"; §9 lists A, B1, B2 only, but step C (`lib.rs:439-448`) verifies too. |
| PC-5 | `NotConfident { best_log10_odds }` is the class when any candidate of any step reached the decision with a finite score; best = the maximum. | §6.4; the failure returned is step A's, the decision may come from B1/B2/C. |
| PC-6 | `accept::GateRefusal`'s `Display` is the one wording; the CLI prints `rejected: {r}`, the app `Rejected: {r}`. | One text for the same check (anti-goal: two tests that can disagree). |
| PC-7 | The regression gate is `corpus_layered_tiers`, extended to record and hold each frame's WCS; tiers 5000/8000 are fetched into `$JOB`. The legacy `corpus_bench` / `corpus_baseline.json` is not regenerated (it needs a deep `smac_gaia/stars.smac` the dev Mac no longer has); if the M4 Max has one, the controller regenerates it there, else it stays with a CHANGELOG note. | §8 "frozen baseline regenerated, every bench WCS unchanged": the layered test records no WCS and needs four tiers, two of which are not installed. |
| PC-8 | Arm 2 runs the hinted solve with the scale confined to a window of 0.9–1.1 × the wrong scale, through `orchestrate::solve_bounded` (S2: a public entry over `solve_inner`'s `scale_bounds`, the window B2 already uses; documented as B2's and the lab's). The production-ladder form (`ladder`: the wrong scale as the hint, the whole ladder) stays as an extra variant. | §7 asks for the window. Factors × 0.25…× 4 all lie inside the hinted ladder (1.5⁴ ≈ 5.06 each way, `orchestrate.rs:163-201`; B1/B2/C widen it, `lib.rs:395-448`), so the ladder form mostly ends at the true field and adds little to V-1's ≈ 650 false-provoking attempts; the window keeps the search on wrong scales. |
| PC-9 | Arm 3's index packs are built in the lab from filtered tiers with the public API: three restrictions, each as `base` and `narrow` — north-only, south-only, and the sky minus every provoke field's cone (radius 2 × the frame's diagonal + 1°; the cones together cover under 1 % of the sky). One pair serves every frame; each restricted cache set is deleted right after its pack build. Arm 3 verifies and spirals with the full tiers (the installed stack), so it can still end at the true field: "arm 3 ending true" joins the verdict's `apart` line. | Packs are not sky-partitioned (`index.rs:199-240`): "every pack but the true field's" is read as "the sky without the true field". A pack per field would cost ≈ 25 × (26 s + 55 s), 147 + 236 MB each (`QUAD_INDEX.md:21-22`), and a ≈ 2.3 GB copy of tier_500 + tier_2000 per restriction. |
| PC-10 | Arm 4 renders the permuted detection list into a synthetic FITS frame (Gaussian stars at each detection's flux and FWHM over a flat background with Gaussian noise) and solves that file. The lab README says the test list T is then the re-detected list, not the permuted one. | The public API solves image files only (`lib.rs:358-380`); the rendered frame is the honest end-to-end form. |
| PC-11 | Arms 1–4 also take the owner's narrow-field and DSLR frames, not only the 40 library frames and the three recorded false alignments. | The false solves came from there (§9); more production attempts only tighten V-1. |
| PC-12 | Library re-solves are hinted at the stored solve's centre and scale and run interleaved with 0.3.0 from crates.io (order alternating per frame), which gives V-7 and the "a frame the current solver accepts" check from one pass. | §7 V-7 compares with 0.3.0; this machine drifts ±10–15 % in a build-heavy session, only an interleaved bracket is comparable. |
| PC-13 | `VerifyTrace` carries the candidate's final WCS so the lab recomputes the 0.3.0 completeness test for the comparison table; no completeness code stays in the crate. | §7's table (A / old completeness / C); §6.2 removes the test. |
| PC-14 | `SolveFailure` carries `hypotheses_verified`, `prefilter_passed` (candidates past the seed pre-filter) and every scored candidate of all steps (`verifies`); the crate version becomes 0.4.0 in S6. | The trace is only logged today (`orchestrate.rs:1430-1438`); lowered runs fail every attempt, so the failure is the lab's only read; V-2's yield needs the pre-filter count beside the scored one. |
| PC-15 | Truth of an owner frame, never taken from the branch under test, first that applies: (1) a stored solve of this frame or of a neighbouring frame of the same pointing, from either catalog (cross-checked by the frame's header within 0.25 half-diagonals); (2) independent of the solver: the header pointing written by the capture software plus the optics scale (206.265 × XPIXSZ × XBINNING / FOCALLEN), cross-checked by a 0.3.0 hinted solve (`smc_old`) of a clear reference frame of the same set, accepted only when that solve agrees with the reference's header within 0.25 half-diagonals and 2 % in scale; the cloudy frame's truth is then its own header pointing shifted by the reference's (solve − header) offset, with the reference's scale and rotation; (3) the owner's target and scale, confirmed by the owner on a stretched image. `truth_source` records which. | A truth from the solver under test makes V-3 circular; 0.3.0 may refuse the very cloudy frames (as it would 28940). The 0.25 is this rig's: its mount pointing is off by 0.105 half-diagonals on the GAIA set. |
| PC-16 | `corpus_layered_tiers` also runs a blind pass through `solve_with_index` over the base pack for six truth frames, recorded and frozen in the same baseline. | S2's emit change and S4's decision change the index path, which no other phase-S gate runs; the first sign of a regression would otherwise be L2. |

## For the owner at approval

Three plan calls change §7's lab protocol (no SV decision is reopened):

- **PC-8** (wrong scale): the arm confines the solve to 0.9–1.1 × the wrong scale through a new bounded entry `solve_bounded`, as §7 says; the production-ladder form stays as an extra variant.
- **PC-9** (wrong sky): three restricted packs (north, south, the sky minus every provoke field), not one per field; arm 3 still verifies with the full catalog, so a run that ends at the true field is counted apart, not as a failure.
- **PC-10** (no sky): the shuffled star list is drawn into a synthetic frame and solved as a file, because the solver takes images only; the test then sees re-detected stars, not the shuffled ones.

## Setup (controller, before Task S1)

- [ ] `claude agents` / ListAgents (no other session on the worktree); `python3 -I ~/.claude/skills/lean-execution/disk-report.py`; `df -h /Volumes/BigMac` ≥ 30 GB free. Disk line of the job (written into the ledger): tiers (tier_500 and tier_2000 as APFS clones, tier_5000 2.79 GB and tier_8000 2.03 GB zipped, unzipped beside them); packs base + narrow (147 + 236 MB each pair, `QUAD_INDEX.md:21-22`); arm-3 packs 3 restrictions × (147 + 236 MB) ≈ 1.2 GB; restricted caches ≈ 2.3 GB at a time, deleted after each pack build; rendered and extracted frames in `$JOB/work/`, deleted after their attempts. Check that `/Volumes/BigMac/Users/astrobureau/Pictures/star test/singles` exists and holds the 21 corpus frames and the owner's frames (read in place, C2).
- [ ] `git -C $W/solvemyastro switch -c verification c606daf`.
- [ ] `mkdir -p $JOB/{tiers,packs,baseline,frames/owner,manifest,runs,work}`; append to `~/.athenaeum-acc/REGISTRY.tsv`: `~/.athenaeum-acc/solve-verify	job	solve-verification	2026-11-27	2026-12-31	tiers (4), index packs (base, narrow, arm-3 restrictions), owner-frame manifests, lab runs of the solve-verification job; delete at its close`. Copy `~/.athenaeum-acc/2026-11-16-osc-zp/memrun.sh` to `$JOB/memrun.sh` with `LOCK=$JOB/.memrun.lock`.
- [ ] Tiers: `cp -c -R "$HOME/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia/tier_500" "…/tier_2000" $JOB/tiers/` (APFS clones); fetch `manifest.json`, `tier_5000.zip` and `tier_8000.zip` from `https://artfrom.space/catalogs/` (the app's base URL, `catalog/gaia_prebuilt.rs:34-44`); the tier hashes live in the manifest (each tier entry names its `zip` and `sha256`; no `.sha256` sidecar is assumed): check `shasum -a 256` of each zip against that value, unzip into `$JOB/tiers/`, delete the zips. Expected: `$JOB/tiers/tier_{500,2000,5000,8000}/stars.smac`.
- [ ] Packs at the base commit: `CARGO_TARGET_DIR=<repo>/.superpowers/target-acc cargo build --release -p solvemyastro --features cli` (from `$W`), then under `memrun.sh 8000`: `solvemyastro build-index --tiers $JOB/tiers --out $JOB/packs/base --preset base` and the same with `narrow` (peak ~4.8 GB).
- [ ] Base test counts: from `$W`, `cargo test -p solvemyastro --all-features -- --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' > $JOB/baseline/sma-tests.txt`; `cargo test -p athenaeum-core plate_solve:: stacking::solve -- --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' > $JOB/baseline/core-tests.txt`. Both green, or STOP.
- [ ] Pre-fill `$JOB/frames/owner/frames.tsv` with what the owner has given (`owner-frames.md`; all files in `/Volumes/BigMac/Users/astrobureau/Pictures/star test/singles` unless a path is shown, read in place). Columns: `file, kind (false|cloudy|narrow|dslr; comma-joined when a frame is two), truth (stored:<catalog>[:<frame_id>] | corpus | radec:<ra>,<dec>,<scale> | derive:<reference file> | none), truth_source (stored | corpus | header+reference | owner), false_at (radec:<ra>,<dec>,<scale> | scale:<arcsec_per_px> | unknown | none), note`. `corpus` reads the corpus bench's own truth table (an independently established truth, PC-15 (1)'s kind); a stored row without `frame_id` is looked up by path. Rows:
  - `_DSC5767.fit`: `false,dslr`; `radec:10.727,41.706,2.770` (M31, corpus truth); `corpus`; false_at `unknown` (the 2026-09-05 false solve was at Dec −68, 0.2178″/px, 6 inliers; its RA was not recorded); note: 7360 × 4912 × 3 RGB, no header, blind only, needs Gaia-depth tiers.
  - `_VSH8902.xisf`: `false,dslr`; truth `none`; false_at `none`; note: 51.05″/px (Nikon Z 6II 5.94 µm, 24 mm), ~85° FOV, 6064 × 4040 × 3, no position in the header, outside the FOV ladder (14°): any accepted solve is false; two chance alignments before 0.2.0.
  - `2024-09-08_04-30-13_L_-9.90_180.00s_0008.fits`: `false,narrow`; `radec:85.391,-2.441,0.39`; `corpus`; false_at `scale:0.25`; note: probable false solve at 0.25″/px, blind with the narrow pack (commit ba6048a, 7 of 58 bright); the lab re-runs it blind with the narrow pack.
  - `_DSC2522`: `dslr`; `corpus` (M31, 1.004″/px); `corpus`; false_at `none`.
  - `/Volumes/bigbase3/Astrolib/Darks/Unsorted/pigmey/GAIA 1208036006713927936/Light_GAIA 1208036006713927936_30.0s_Bin1_20251025-193923_0055.fit`: `cloudy`; `stored:release` (the owner solved it in the release app: RA 231.68952, Dec 19.21590, ROT 197.18°; the lab reads the row by path); `stored`; false_at `none`; note: header RA 232.011 Dec 19.124 (mount pointing), the stored solve is 0.105 half-diagonals from it (inside the 0.25 cross-check); ASI2600MC Air (OSC, RGGB), 269 mm, 3.76 µm → 2.883″/px, 6248 × 4176; reference frame `…/Light_GAIA 1208036006713927936_15.0s_Bin1_20251025-185723_0005.fit` (header RA 231.737 Dec 18.818).
  - Truth for any later owner frame follows PC-15 in order (stored, then header + reference, then the owner's confirmed target); `sv_manifest.py` writes `truth_source` into the manifest and `sv-lab truth` (L1) resolves `derive:` rows; truth is never taken from the branch under test.
- [ ] Ask the owner (Russian, PushNotification + AskUserQuestion, recommended option first), one message. Answered and dropped: the three false alignments and the DSLR minimum (met by `_DSC2522` and `_DSC5767`, plus `_VSH8902`). Still asked: (a) cloudy frames, 2 given of ≥ 10 (the GAIA 1208036006713927936 pair's cloudy frame; the set also has the clear reference): ≥ 8 more cloudy frames (thin high cloud, passing cloud, a gap), each with a clear reference frame of the same set when no stored solve exists; (b) 2 more narrow-field frames at ≤ 0.4″/px (1 of 3 given: the 0.39″/px corpus frame; the 0.48″/px corpus frames are above the line); (c) the runner for the provocation cycle: the dev Mac (recommended: frames, tiers and packs are here; the library cycle must run here anyway; ~12 h overnight) or the M4 Max (needs ~10 GB through `/Volumes/Universe/athenaeum-runner/acc/solve-verify/` with a registry row on both machines). Frames dropped into `$JOB/frames/owner/` or named by path; the answers are appended to `frames.tsv`.
- [ ] Ledger `.superpowers/sdd/2026-11-26-solve-verification/progress.md`: Setup done, the base commit, the job row.

---

## Phase S — solvemyastro branch `verification` (no release)

### Task S1: the corpus gate records every bench WCS and holds it to a frozen baseline

**Files:**
- Modify: `solvemyastro/tests/corpus_bench.rs:1055-1239` (`corpus_layered_tiers`), `solvemyastro/.gitignore` (add `tests/corpus_layered_run.json`)
- Create: `solvemyastro/tests/corpus_layered_baseline.json` (frozen at 0.3.0)

**Interfaces:**
- Produces: run file `tests/corpus_layered_run.json` = `{"schema_version":1,"frames":[{"frame","solved","crval":[ra,dec],"crpix":[x,y],"cd":[cd11,cd12,cd21,cd22],"sip":{"a_order","a","b_order","b","ap_order","ap","bp_order","bp"}|null,"matched_stars","inlier_ratio","rms_px","elapsed_ms"}]}` (frames in file-name order, `SKIP_FRAMES` left out), and `"index_frames":[…same objects…]` for the blind index pass below. S2 adds the own-set keys (not compared), S4 adds `log10_odds`, `reference_stars`, `hypotheses`.
- Produces: the blind index pass (PC-16). After the tiers pass `corpus_layered_tiers` opens the pack in `SOLVEMYASTRO_INDEX_DIR` (`$JOB/packs/base`) and solves six truth frames blind through `solve_with_index`; the six are a fixed list written in the test (picked in Step 5). With the variable absent that part prints `SKIP: SOLVEMYASTRO_INDEX_DIR not set`, and the command fails (the `CORPUS` grep catches `SKIP`).
- Produces: `fn frame_json(name: &str, res: &anyhow::Result<SolveSolution>) -> serde_json::Value`, `fn wcs_moved(run: &serde_json::Value, base: &serde_json::Value) -> Option<String>` (the first moved key path, e.g. `"sip.a[2][1]"`; numbers equal when `|x − y| ≤ 1e-9 · max(|x|, |y|)`, exactly when both are 0; arrays of equal length; only `crval`, `crpix`, `cd`, `sip` are compared).

- [ ] **Step 1:** Test `wcs_compare_flags_a_moved_coefficient` (plain `#[test]` in `corpus_bench.rs`, no data): two frame values identical → `None`; one SIP `a` coefficient scaled by `1 + 1e-6` → `Some("sip.a[…]")`; scaled by `1 + 1e-12` → `None`; `crpix` differing by 1e-3 → `Some("crpix[0]")`.
- [ ] **Step 2:** `cargo test -p solvemyastro --test corpus_bench wcs_compare_flags_a_moved_coefficient` fails (functions missing).
- [ ] **Step 3:** Implement the two functions; in `corpus_layered_tiers` collect `frame_json` per frame (tiers pass, then the blind index pass into `index_frames`), write the run file; when `tests/corpus_layered_baseline.json` exists, assert the solved sets are equal and `wcs_moved` is `None` for every solved frame of both lists (the failure message lists every moved frame and key); print `WCS gate: <n> solved frames, <k> moved` (n and k count both lists), `index pass: <i> solved of 6`, or `WCS gate: no baseline, wrote the run`.
- [ ] **Step 4:** The unit test passes.
- [ ] **Step 5:** Pick the six index-pass frames: corpus truth frames that solve blind through the base pack at 0.3.0 (two at ≤ 0.5″/px, two at 1–3″/px, two above; M78 and `SKIP_FRAMES` excluded), the list and each frame's scale in a comment. Base capture: `CORPUS` → `GATE: layered-tier corpus PASSED`, `index pass: 6 solved of 6` and `WCS gate: no baseline, wrote the run`. `cp tests/corpus_layered_run.json tests/corpus_layered_baseline.json`; `CORPUS` again → `WCS gate: <n> solved frames, 0 moved`. A base failure (M78, a missed truth frame) is an environment STOP, never a code change.
- [ ] **Step 6:** Commit (in `$W/solvemyastro`): `test(corpus): the layered gate records each frame's WCS and holds it to a frozen baseline`.

Acceptance: both `CORPUS` runs as above (the second with `index pass: 6 solved of 6`); `cargo test -p solvemyastro --all-features` equals `$JOB/baseline/sma-tests.txt` plus one test.

### Task S2: the hypothesis trail — own image stars per search path and one hypothesis count (no verdict change)

**Owns:** the excluded set per search path (PC-2, PC-3); H (PC-4).

**Files:**
- Modify: `solvemyastro/src/index.rs:766-772` (`Hypothesis`), `:894` (emit), tests after `:1045`
- Modify: `solvemyastro/src/orchestrate.rs:349-361` (`TrialResult`), `:374-518` (`run_trial`), `:529-558` (`refit_seed_at` returns its matches' stars too), `:1014-1027` (`solve`, `solve_inner` signature), `:1263-1290` (`SearchCtx` construction), `:1430-1438` (failure return), `:1503-1545` (`SearchCtx` + `stats`), `:1720-1733` (recentred alternates), `:1766-1885` (`run_index_search`: `pending` keeps the quad; `TrialResult` fields), `:1891-1973` (`verify_trials`: count H, new trace fields)
- Modify: `solvemyastro/src/diag.rs:172-201` (`VerifyTrace` + three fields), new `SearchPath`
- Modify: `solvemyastro/src/lib.rs:373-452` (`solve_with_index`: one `SolveStats` for steps A, B1, B2, C), re-export of `solve_bounded` and the doc-hidden traced entry next to `solve`
- Modify: `solvemyastro/tests/corpus_bench.rs` (S1's recorder: the own-set keys; one `solve_bounded` check)

**Interfaces:**
- Produces: `index::Hypothesis { wcs, star_n, queries, star_idx: [usize; 4] }` — indices into the `pts` given to `index::search` (a prefix of the caller's SNR-ordered list).
- Produces: `diag::SearchPath { Spiral, Index }`, `fn as_str(&self) -> &'static str` (`"spiral"`, `"index"`); `VerifyTrace` gains `path: SearchPath`, `recentered: bool`, `hypotheses_so_far: usize`, and the own-set measure: `own_star_count: usize` (`own_stars.len()`), `image_star_count: usize` (`image_pts.len()`), `inliers_outside_own: usize` (of the seed pre-filter's inliers against `verify_stars`, `orchestrate.rs:2019-2063`, those whose image star is not in `own_stars`; the pre-filter's own inlier count stays as it is).
- Produces: `struct SolveStats { hypotheses: AtomicUsize, prefilter_passed: AtomicUsize, verifies: Mutex<Vec<VerifyTrace>> }` (crate-private) with `fn new() -> Self`, `fn next_hypothesis(&self) -> usize` (returns the count including this one), `fn hypotheses(&self) -> usize`, `fn count_prefilter_passed(&self)` (called where a candidate clears the seed pre-filter, before any scoring), `fn prefilter_passed(&self) -> usize`, `fn absorb(&self, trace: &SolveTrace)` (appends every `VerifyTrace` of every pass and rung, in order), `fn take_verifies(&self) -> Vec<VerifyTrace>`.
- Produces: `pub fn solve_bounded(image, hints, caches, indexes, cfg, cancel, scale_bounds: (f64, f64)) -> Result<SolveSolution>` in `orchestrate` (the units `solve_inner` takes), a thin wrapper over `solve_inner(.., Some(scale_bounds), &SolveStats::new())`, documented as the window B2 uses and the lab's wrong-scale arm (PC-8); and `#[doc(hidden)] pub fn solve_traced(…same as solve_with_index…) -> (Result<SolveSolution>, Vec<VerifyTrace>)`, whose last trace is the accepted candidate's, so the corpus recorder and the lab read the own-set keys on a success (the public `solve` signatures are unchanged).
- Produces: `orchestrate::solve_inner(image, hints, caches, indexes, cfg, cancel, scale_bounds, stats: &SolveStats)`; every return of `solve_inner` that holds a `SolveTrace` calls `stats.absorb(&trace)` first. `orchestrate::solve` keeps its public signature with its own `SolveStats`.
- Produces: `fn quad_match_stars(matches: &[(usize, usize)], iquads: &[Quad]) -> Vec<usize>` (sorted, deduplicated union of `iquads[ii].star_idx` over `matches`).
- Produces: `TrialResult` gains `own_stars: Vec<usize>`, `quad4: [usize; 4]`, `path: SearchPath`, `recentered: bool`. Spiral: `own_stars = quad_match_stars(&matches, iquads)`, `quad4 = iquads[matches[0].0].star_idx`; recentred alternate: `own_stars` = sorted union with the re-fit's `quad_match_stars`, `quad4` = the original's, `recentered = true`; index: `own_stars` = sorted union of `h.star_idx` and the agreeing `pending` entry's quad when the agreement path fired, `quad4 = h.star_idx`.
- H: `let h = ctx.stats.next_hypothesis();` at the top of each `'candidates` iteration; `hypotheses_so_far = h`.

- [ ] **Step 1:** Tests:
  - `orchestrate::…::quad_match_stars_is_the_sorted_union_of_image_quad_stars`: iquads with `star_idx` `[0,1,2,3]`, `[2,3,4,5]`, `[7,8,9,10]`; matches `[(0,9),(1,4),(0,2)]` → `vec![0,1,2,3,4,5]`.
  - `orchestrate::…::solve_stats_count_every_hypothesis_once`: `next_hypothesis()` returns 1, 2, 3; `hypotheses() == 3`; `count_prefilter_passed()` twice → `prefilter_passed() == 2`.
  - `index::tests::a_hypothesis_names_its_image_quad`: the synthetic index of `built_index_round_trips_and_lookups_match_a_scan` (40 000 random stars, depth-2 band); band quad 0's four stars projected about their centroid at 20″/px into a 3000 × 3000 frame as `pts[0..4]`, 40 random points after them; `search(…, emit)` with `emit` returning `true` → the first hypothesis's sorted `star_idx == [0, 1, 2, 3]`.
  - `diag::tests::search_path_names_are_stable`: `"spiral"`, `"index"`.
- [ ] **Step 2:** The four tests fail (items missing).
- [ ] **Step 3:** Implement as the Interfaces say. `debug_assert!` that every own-star index is `< image_pts.len()`.
- [ ] **Step 4:** `cargo test -p solvemyastro --all-features` green.
- [ ] **Step 5:** `CORPUS` → `WCS gate: <n> solved frames, 0 moved` (nothing reads H or the own stars yet). The recorder also writes, per accepted corpus candidate (from `solve_traced`'s last trace), the run-file keys `own`, `image_pts`, `linear_inliers`, `inliers_outside_own` (not compared with the baseline), and one plain check runs a corpus frame through `solve_bounded` with a window containing its true scale and compares the WCS with `solve`'s (equal). The S2 report shows the four keys per corpus frame and names every truth frame with fewer than 4 inliers outside its own set: such a frame would leave S3 no scanned star (−∞, a refusal by construction against SV-10's "accept on A alone"), so it goes to the owner before S3 starts. Nothing in S3 is dispatched until that line is clear.
- [ ] **Step 6:** Commit: `feat(verify): each hypothesis carries its own image stars; one hypothesis count per solve`.

### Task S3: the decision — a Bayes factor over the image stars

**Owns:** `log10_odds` (the formula); `matched_stars` on the final WCS (the count rule).

**Files:**
- Modify: `solvemyastro/src/verify.rs:1-35` (module doc and constant docs cite [1] §2.4: the seed pre-filter `log_odds` and the decision), add the items below after `:94`, tests after `:188`

**Interfaces:**
```rust
/// The decision's bar: posterior odds 10^3 over a prior of 10^-6 ([1], §2.4).
pub const ACCEPT_LOG10_ODDS: f64 = 9.0;
/// Beyond this many σ a test star has no counterpart and adds ln d.
pub const SCAN_CUTOFF_SIGMA: f64 = 3.0;
/// Hypotheses the prior's budget covers; beyond it the bar rises (§6.1 clause).
pub const HYPOTHESIS_BUDGET: f64 = 1e6;

pub struct ScanInput<'a> {
    pub image_stars: &'a [(f64, f64)], // every image star, SNR order, highest first (pixels)
    pub own_stars: &'a [usize],        // indices into image_stars: the hypothesis's own stars
    pub references: &'a [(f64, f64)],  // R: in-frame verification stars on the final WCS (pixels)
    pub sigma_px: f64,
    pub area_px2: f64,
    pub distractor: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ScanResult {
    pub log10_odds: f64,      // max over n ≥ 1 of L_n / ln 10; −∞ when degenerate
    pub peak_rank: usize,     // n at the maximum, 1-based over the scanned stars; 0 when none
    pub test_stars: usize,    // |T| = image_stars minus own_stars
    pub scan_matched: usize,  // scanned stars whose claimed reference lies within 1σ
    pub matched_stars: usize, // own stars within 1σ + scan_matched (SV-11's count)
}
pub fn scan_log10_odds(input: &ScanInput<'_>) -> ScanResult;
/// `base + max(0, log10(hypotheses / HYPOTHESIS_BUDGET))`.
pub fn required_log10_odds(base: f64, hypotheses: usize) -> f64;
```
Algorithm (fixes what the tests leave open: claim order, ties, inclusivity):
```text
degenerate (N_R = 0, σ ≤ 0, A ≤ 0, d ∉ (0,1), or no scanned star) → log10_odds −∞, peak_rank 0
c = (1 − d)·A / (2π σ² N_R); grid over references, cell 3σ, 3×3 neighbourhood
for i in own_stars ascending: nearest unclaimed reference with r ≤ 3σ (tie → lower index) → claimed; r ≤ σ → matched += 1
L = 0; for each scanned star in order (own stars skipped), n = 1, 2, …:
    nearest unclaimed reference with r ≤ 3σ (tie → lower index) → claimed; term = ln(d + c·exp(−r²/2σ²)); r ≤ σ → scan_matched += 1
    none → term = ln d
    L += term; L/ln10 > best → best, peak_rank = n
```

- [ ] **Step 1:** Tests (`verify::tests`), exact values where stated:
  - `degenerate_inputs_are_never_confident`: `N_R` 0, `σ` 0, `A` 0, and an empty `image_stars` each → `log10_odds == f64::NEG_INFINITY && peak_rank == 0`.
  - `an_unmatched_image_star_costs_ln_d`: 10 stars exactly on 10 of 50 references (σ 1.5, A 6e6); the same with one star far (> 3σ) from every reference put first → `Δ log10_odds` equals `0.25f64.log10()` within 1e-12 and `peak_rank` grows by 1.
  - `lost_catalog_stars_leave_the_odds_unchanged`: two reference lists of 50, equal on the 10 references under test stars, the other 40 placed differently but > 3σ from every test star → identical `ScanResult`.
  - `a_reference_explains_one_test_star`: two test stars on one reference → `scan_matched == 1`, `peak_rank == 1`, `log10_odds == (0.25 + c).log10()` within 1e-12.
  - `the_cut_offs_are_inclusive_at_one_and_three_sigma`: a star at exactly σ counts in `scan_matched`; at exactly 3σ it claims (term `ln(d + c·e^{−4.5})`) and is not counted; at `3σ·(1 + 1e-9)` its term is `ln d`.
  - `own_stars_claim_their_references_first`: own star 0 on reference 0, scanned star 1 on reference 0 → its term is `ln d`; `matched_stars == 1`, `scan_matched == 0`, `test_stars == image_stars.len() − 1`.
  - `the_scan_peaks_at_the_running_maximum`: two matches then 20 distractors → `peak_rank == 2`, `log10_odds == 2·(d + c).log10()` within 1e-12.
  - `the_bar_rises_beyond_a_million_hypotheses`: `required_log10_odds(9.0, 1) == 9.0`, `(9.0, 1_000_000) == 9.0`, `(9.0, 10_000_000)` = 10.0 within 1e-12, `(f64::INFINITY, 5) == f64::INFINITY`.
  - `the_uniform_null_rarely_reaches_a_thousand_to_one`: 10⁴ seeded hypotheses (`Xoshiro256StarStar::seed_from_u64(k)`), references and test stars uniform in the frame, no own stars, at two geometries — σ 13.8 px, `N_R` 61, 6000 × 4000, 600 test stars (the documented false solve, §6.1) and σ 1.6, `N_R` 476, 6252 × 4176, 584 test stars (frame 28940) → at most 15 of 10⁴ reach `log10_odds ≥ 3` at each (Ville bound 10, Poisson q95 15).
  - `the_per_star_ratio_averages_below_one`: one reference in a small frame (`N_R = 1`, its 3σ disc wholly inside), one test star placed on a regular lattice of positions (step 0.05σ) over the frame, one `scan_log10_odds` call per position → the mean of `10^log10_odds` (the per-star ratio, n = 1) equals `d + (1 − d)(1 − e^{-4.5})` within 1e-3. It pins the normalisation Ville's bound rests on, with no sampling noise.
  - `a_true_alignment_clears_the_bar_by_far`: σ 1.6, `N_R` 476, 6252 × 4176; 120 stars at references + 0.5σ, interleaved one in four among 464 uniform stars → `log10_odds ≥ 100`.
  - `the_recorded_false_solve_scores_under_the_bar`: σ 13.8, `N_R` 61, 6000 × 4000, 600 test stars, the first six on references, `own_stars = [0, 1, 2, 3]` → `3.0 < log10_odds < 9.0` (§6.1 reconstruction: about 5 decades).
- [ ] **Step 2:** The tests fail (items missing).
- [ ] **Step 3:** Implement (one grid built per call; `Xoshiro256StarStar` is already a dependency).
- [ ] **Step 4:** `cargo test -p solvemyastro verify::` green; the null test under 10 s in a debug build.
- [ ] **Step 5:** Commit: `feat(verify): the Bayes factor over image stars, its bar and the hypothesis clause`.

### Task S4: the decision replaces the bright-star completeness test

**Owns:** the test-star order T (all of `image_pts` as `select_fast_stars` ranks them, cap 600, minus own stars); `N_R` and its reference list; σ (`tol_px` of `verify_trials`, `adaptive_tol_px(trial.scale_arcsec, BASE_MATCH_ARCSEC)`); `inlier_ratio`.

**Files:**
- Modify: `solvemyastro/src/orchestrate.rs:601-641` (delete `bright_star_evidence`; add `references_in_frame`), `:2042-2044` (comment cites [1]), `:2169-2174` (floor reads `cfg.min_final_inliers`), `:2176-2185` (`inlier_ratio` moves after the decision), `:2374-2401` (the decision), `:2417-2432` (`SolveSolution` fields)
- Modify: `solvemyastro/src/verify.rs:96-121` (delete `BRIGHT_MATCH_PROB`, `BRIGHT_LOG_ODDS_MIN`, `bright_star_log_odds`)
- Modify: `solvemyastro/src/lib.rs:161-258` (`SolveConfig`; docs of `use_bayesian_verify` and `log_odds_threshold` name the seed pre-filter and cite [1]), `:260-308` (`SolveSolution`), `:373-452` (`solve_with_index` failure), `:469-507` (test literal)
- Modify: `solvemyastro/src/diag.rs:172-201` (`VerifyTrace` decision fields), `:247-292` (`FailureClass`), `:293-306` (`SolveFailure`), `:309-343` (`classify`), the funnel `Display` (prints `log10_odds` when reached)
- Modify: `solvemyastro/tests/corpus_bench.rs` (S1's recorder: three keys, three asserts)

**Interfaces:**
- Consumes: S2 (`TrialResult.own_stars/quad4/path/recentered`, `SolveStats`), S3 (`scan_log10_odds`, `required_log10_odds`, `ACCEPT_LOG10_ODDS`).
- Produces: `SolveConfig { accept_log10_odds: f64 /* default verify::ACCEPT_LOG10_ODDS */, min_final_inliers: usize /* default 6 = MIN_ABSOLUTE_INLIERS */ }`.
- Produces: `SolveSolution { log10_odds: f64, reference_stars_in_frame: usize, hypotheses_verified: usize }`; `matched_stars = scan.matched_stars`; `inlier_ratio = matched_stars as f64 / reference_stars_in_frame as f64`. Doc comments state the 0.4.0 meanings and that 0.3.x counted `matched_stars` on the linear WCS over a cone denominator.
- Produces: `fn references_in_frame(wcs: &WcsSolution, cat: &[CatalogStar], width: f64, height: f64) -> Vec<(f64, f64)>` — projections that are finite and in `[0, width) × [0, height)` (the rule `bright_star_evidence` used).
- Produces: `VerifyTrace` gains `tol_px: f64`, `reference_stars: usize`, `test_stars: usize`, `scan_matched: usize`, `matched_stars: usize`, `peak_rank: usize`, `log10_odds: Option<f64>`, `log10_odds_quad4: Option<f64>`, `required_log10_odds: f64`, `final_wcs: Option<WcsSolution>` (`None`/0 when the decision was not reached); `own_star_count`, `image_star_count`, `inliers_outside_own` are S2's.
- Produces: `FailureClass::NotConfident { best_log10_odds: f64 }`, `as_str` `"NOT_CONFIDENT"`; `FailureClass` derives `PartialEq` only (f64). `SolveFailure { class, best_attempt, message, hypotheses_verified: usize, prefilter_passed: usize, verifies: Vec<VerifyTrace> }`.
- Decision at the old completeness point, on `sip_wcs`: `refs = references_in_frame(&sip_wcs, &verify_stars, width, height)`; `scan` with `own_stars = &trial.own_stars`; `quad4` with `own_stars = &trial.quad4`; `required = required_log10_odds(cfg.accept_log10_odds, h)`; record the numbers, `final_wcs = Some(sip_wcs.clone())` into `rt.verifies.last_mut()`; refuse unless `scan.log10_odds >= required` (reason `"not confident: log10 odds {:.1} < {:.1}"`); `tracing::debug!("solve decision", frame, log10_odds, reference_stars, test_stars, peak_rank, scan_matched, hypotheses, required_log10_odds)` replaces `"bright-star completeness"`.
- `solve_with_index`, when every step failed and step A's error is a `SolveFailure` (`downcast_mut`): set `hypotheses_verified = stats.hypotheses()`, `prefilter_passed = stats.prefilter_passed()`, `verifies = stats.take_verifies()`, and the class to `NotConfident { max finite log10_odds }` when one exists (PC-5). A success carries `hypotheses_verified` = H at its acceptance.
- `classify` (one trace): `NotConfident` when any verify has a finite `log10_odds` (best = max), checked after the empty-verifies block and before `VerifyZeroInliers`/`VerifyGap`.

- [ ] **Step 1:** Tests:
  - `orchestrate::…::references_in_frame_keeps_finite_in_frame_projections`: a 100 × 100 px TAN WCS at 1″/px; stars at pixel (0, 0), (50, 50) kept; (100, 50) and (−0.5, 10) dropped; one 90° away (non-finite) dropped → 2.
  - `lib::…::solve_config_defaults_carry_the_decision`: `accept_log10_odds == verify::ACCEPT_LOG10_ODDS == 9.0`, `min_final_inliers == 6`.
  - `diag::tests::a_search_that_reached_the_decision_is_not_confident`: one pass, one rung, verifies with `log10_odds` `Some(6.2)` and `Some(7.5)` (seed inliers 30 / 40, required 10) → `NotConfident { best_log10_odds: 7.5 }`.
  - `diag::tests::a_non_finite_decision_falls_back_to_the_old_classes`: only `Some(f64::NEG_INFINITY)`, seed inliers 5, required 10 → `VerifyGap { best: 5, required: 10 }`.
  - `diag::tests::failure_codes_are_stable`: `NotConfident{…}.as_str() == "NOT_CONFIDENT"`, the old codes unchanged.
  - Corpus recorder: each solved frame gets `log10_odds`, `reference_stars`, `hypotheses`; asserts `log10_odds >= ACCEPT_LOG10_ODDS`, `|inlier_ratio − matched/reference_stars| < 1e-12`, `hypotheses >= 1`.
- [ ] **Step 2:** The unit tests fail.
- [ ] **Step 3:** Implement; delete the completeness items; update `harvest_tests`' `SolveFailure` literal.
- [ ] **Step 4:** `cargo test -p solvemyastro --all-features` green; `git grep -n -i "bright_star\|BRIGHT_LOG\|BRIGHT_MATCH" -- src` prints nothing.
- [ ] **Step 5:** `CORPUS` → `GATE … PASSED`, `WCS gate: <n> solved frames, 0 moved`, the new asserts hold. A moved frame or a refused truth frame is a STOP: report the frame, its old and new candidate and numbers; never re-freeze to pass (§8, anti-goal).
- [ ] **Step 6:** Commit: `feat(verify)!: one chance-or-real decision on the final WCS replaces the bright-star completeness test`.

### Task S5: the gate re-reads the solver's odds; the CLI says which check refused

**Files:**
- Modify: `solvemyastro/src/accept.rs` (whole file: `:1-16` doc and `DENSE_FIELD_MIN_EXPECTED` go, `:18-52` `Gate`, `:54-71` `GateMetrics`, `:73-104` `accepts`, `:117-162` `expected_bright_in_fov` goes with its radius formula and recipe note, tests `:164-383`)
- Modify: `solvemyastro/src/bin/solvemyastro/solve.rs:10`, `:55-87`, `:106-174`, tests `:176-209`

**Interfaces:**
- Produces: `Gate { base_tolerance_arcsec, rms_max_px_mult, inlier_floor, scale_sanity_min, scale_sanity_max, scale_header_tol }` (six fields, defaults 8.0, 2.5, 6, 0.05, 60.0, 8.0).
- Produces: `GateMetrics { inliers: usize, rms_px: f64, tol_px: f64, log10_odds: f64, recovered_scale_arcsec: f64, header_scale_arcsec: Option<f64> }`.
- Produces: `enum GateRefusal { LooseFit { rms_px: f64, limit_px: f64 }, TooFewMatched { matched: usize, needed: usize }, NotConfident { log10_odds: f64, needed: f64, matched: usize }, ImplausibleScale { scale_arcsec: f64, min: f64, max: f64 }, OffHeaderScale { scale_arcsec: f64, header_scale_arcsec: f64, factor: f64 } }` with `Display` (exact copy, PC-6):
  - `the stars fit too loosely ({rms_px:.2} px RMS, {limit_px:.2} px allowed)`
  - `only {matched} stars matched ({needed} needed)`
  - `the match could be a chance alignment (odds 10^{log10_odds:.1} to 1, 10^{needed} needed; {matched} stars matched)`
  - `the solved scale {scale_arcsec:.2}"/px is outside {min}..{max}"/px`
  - `the solved scale {scale_arcsec:.2}"/px is more than {factor}x off the header's {header_scale_arcsec:.2}"/px`
- Produces: `Gate::refusal(&self, m: &GateMetrics) -> Option<GateRefusal>` (order of §6.3; a non-finite RMS is `LooseFit`, a non-finite odds is `NotConfident`); `Gate::accepts(&self, m) -> bool` = `refusal(m).is_none()`. The odds check reads `verify::ACCEPT_LOG10_ODDS`.
- CLI `--json` keys: `ra, dec, scale, rot, matched, rms_px, inlier_ratio, log10_odds, reference_stars, hypotheses, gate`; text output prints `odds: 10^{:.1} to 1 ({} reference stars, {} hypotheses)` in place of `expected in field:`; a refusal is `Refused(format!("rejected: {r}"))`; the `--no-gate` warning logs `log10_odds` instead of `inlier_ratio`.

- [ ] **Step 1:** Tests:
  - `gate_names_each_refusal_in_order`: base (40 matched, rms 1.2, tol 6, odds 300, scale 1.8, header 1.9) passes; rms 20 → `LooseFit`; 5 matched → `TooFewMatched{5,6}`; odds 8.9 → `NotConfident`; odds NaN → `NotConfident`; scale 100 → `ImplausibleScale`; scale 20 with header 1.9 → `OffHeaderScale`; rms 20 with odds 3 → `LooseFit` (first check wins).
  - `gate_boundaries_are_pinned`: rms exactly 10.0 at tol 4 accepts, 10.5 refuses; odds exactly 9.0 accepts, 8.999 refuses; header ratio 8.0 and 0.125 accept, 8.5 and 0.1 refuse; 6 matched accepts, 5 refuses.
  - `gate_refuses_the_recorded_false_solve`: 2026-09-05 numbers — 6 matched, rms 1.159, `tol_px = gate_tol_px(0.2178, 8.0)`, scale 0.2178, no header, odds 4.8 → `Some(NotConfident { log10_odds: 4.8, needed: 9.0, matched: 6 })`, its text is exactly `the match could be a chance alignment (odds 10^4.8 to 1, 10^9 needed; 6 stars matched)`; the same field with odds 120 → accepted.
  - `refusal_texts_are_pinned`: the five texts for (20.0, 10.53), (5, 6), (6.2, 9.0, 12), (100.0, 0.05, 60.0), (29.6, 1.873, 8.0) → `the stars fit too loosely (20.00 px RMS, 10.53 px allowed)`, `only 5 stars matched (6 needed)`, `the match could be a chance alignment (odds 10^6.2 to 1, 10^9 needed; 12 stars matched)`, `the solved scale 100.00"/px is outside 0.05..60"/px`, `the solved scale 29.60"/px is more than 8x off the header's 1.87"/px`.
  - `gate_defaults_are_the_apps_defaults` (six fields); `gate_tol_px_clamps` unchanged; `the_verify_magnitude_limit_is_pinned` kept, its message saying the limit moves `N_R` and with it the odds and embedding apps' verdicts (recipe bump).
  - CLI: `json_keys_are_the_documented_set` (the eleven keys above); `a_refusal_reads_rejected_and_names_the_check` → `rejected: only 5 stars matched (6 needed)`.
- [ ] **Step 2:** Tests fail.
- [ ] **Step 3:** Implement; `git grep -n "expected_bright_in_fov\|expected_in_fov\|min_inlier_ratio\|DENSE_FIELD" -- src tests examples` prints nothing.
- [ ] **Step 4:** `cargo test -p solvemyastro --all-features` green.
- [ ] **Step 5:** Commit: `feat(accept)!: the gate re-reads the solver's odds and names the check that refuses`.

### Task S6: docs, 0.4.0 metadata, publication checks, re-frozen corpus baseline

**Files:** `solvemyastro/CHANGELOG.md` (new top section), `solvemyastro/README.md:36-50`, `solvemyastro/QUAD_INDEX.md:235` (the completeness-gate sentence) and `:263` (the `bright-star completeness` debug event row), `solvemyastro/Cargo.toml` (version), `solvemyastro/Cargo.lock`, doc comments left in `src/verify.rs`, `src/accept.rs`, `src/lib.rs`, `src/diag.rs`, `tests/corpus_layered_baseline.json`.

- [ ] **Step 1:** `CHANGELOG.md` `## [0.4.0] — unreleased`: **Verification** — the decision (one paragraph: what is scanned, what a lost catalog star costs, the bar from a prior of 10⁻⁶ and a 1999:1 cost ratio, the clause above 10⁶ hypotheses; [1] §2.4 cited as spec §11); the completeness test and the gate's ratio rule removed; **Breaking** — `SolveSolution` (three fields; `matched_stars` now on the final WCS, one detection per reference at 1σ; `inlier_ratio` over the in-frame reference stars), `SolveConfig` (two fields), `accept` (`GateMetrics`, `Gate::min_inlier_ratio`, `DENSE_FIELD_MIN_EXPECTED`, `expected_bright_in_fov` removed; `Gate::refusal`, `GateRefusal`), `diag` (`FailureClass::NotConfident`, no longer `Eq`; `SolveFailure` fields; `VerifyTrace` fields; `SearchPath`), `index::Hypothesis::star_idx`, `verify` items, the CLI JSON keys; the 0.3.0 known issue is gone with the function; the legacy `corpus_baseline.json` keeps 0.3.0 meanings (PC-7) unless regenerated.
- [ ] **Step 2:** README: the gate paragraph (RMS, 6 matched on the final WCS, odds ≥ 10⁹, scale checks), the JSON key list, the known-issue paragraph removed, two sentences on how a solution is judged. `QUAD_INDEX.md`: `:235` says the decision (the Bayes factor at `log(1e9)`-equivalent 10⁹ on the final WCS) in place of the completeness gate; `:263` names the `solve decision` event. S4's grep covered `src` only: `git grep -n -i "bright-star completeness\|bright_star" -- ':!CHANGELOG.md'` over the whole repository prints nothing.
- [ ] **Step 3:** `version = "0.4.0"`; commit `docs: 0.4.0 — the decision, the gate, the breaking fields`.
- [ ] **Step 4:** Standalone lock: `git clone $W/solvemyastro $JOB/sma-repo -b verification` (REGISTRY row, job class), there `cargo update -w`, copy `Cargo.lock` back, commit `chore: 0.4.0 lock`. In the clone: `cargo test --locked --all-features` green; `cargo package --list` shows no `docs/`, `.claude/`, `.superpowers/`, `tests/files/`; `cargo publish --dry-run --locked` passes.
- [ ] **Step 5:** Name scan as before starloom's publication, with the pattern list kept in this job's ledger folder, `.superpowers/sdd/2026-11-26-solve-verification/name-scan-patterns.txt` (git-ignored, never committed or copied into a repo file, because the patterns themselves name programs; the controller writes it from the owner's names-to-avoid list), over `git -C $W/solvemyastro ls-files | xargs cat | tr '\n' ' '`: no program names; only FITS/XISF/HEALPix/Gaia/ESA credits and [1]–[3]; `git grep -n -i "open solver" -- src` prints nothing.
- [ ] **Step 6:** Re-freeze: `CORPUS` → `0 moved` against S1's baseline; `cp tests/corpus_layered_run.json tests/corpus_layered_baseline.json`; `CORPUS` → `0 moved`; commit `test(corpus): baseline re-frozen with 0.4.0's match count, ratio and odds (every WCS unchanged)`.

### Gate S (controller)

- [ ] Re-run `cargo test -p solvemyastro --all-features` in `$W` and `cargo test --locked --all-features` in `$JOB/sma-repo` (after `git pull`); `CORPUS`. All as expected, or a fresh fix agent.
- [ ] Whole-branch review of `c606daf..verification` on the most capable model (fable at writing time), brief: spec §6.1–§6.4, §8, this plan's PC-1…PC-6, PC-13, PC-14; hunt plan-verbatim seams (the `GateRefusal` texts reused by the CLI and the app, the `VerifyTrace`/`SolveFailure` fields the lab reads, H across steps); report ≤ 80 lines; fixes go to a fresh rust-engineer with a handoff, then the gate re-runs.
- [ ] **Approval A** (owner, one question): push `verification` to `gitlab` and to GitHub (`all`), then `gh workflow run ci.yml -R eg013ra1n/solvemyastro --ref verification`. Expected: Linux (four layers), macOS, Windows and the publish dry-run green; read the Windows job.

---

## Phase L — the lab tool (athenaeum-lab `harness/acceptance/solve-verify/`)

### Task L1: `sv-lab`, the manifest and the verdict

**Files (create, in `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab`):**
- `harness/acceptance/solve-verify/README.md` (what each cycle measures, the bars, the commands, the data paths)
- `harness/acceptance/solve-verify/Cargo.toml` + `Cargo.lock`: `solvemyastro = { path = "../../../../athenaeum/.claude/worktrees/solve-verification/solvemyastro" }`; `smc_old = { package = "solvemyastro", version = "=0.3.0" }`; `astroimage = { package = "rustafits", path = "../../../../athenaeum/.claude/worktrees/solve-verification/rustafits" }`; `[patch.crates-io] rustafits = { path = <same> }`; serde, serde_json; `[workspace]`
- `src/main.rs` (subcommands), `src/arms.rs` (attempt plan: hints, config, packs per arm), `src/packs.rs` (restricted caches and packs), `src/render.rs` (arm 4), `src/old_completeness.rs` (0.3.0's test for the comparison table), `src/truth.rs` (PC-15 (2): the truth of a frame by header pointing and a reference solve), `src/out.rs` (TSV writers, resume)
- `sv_manifest.py`, `sv_verdict.py`
- `harness/acceptance/tests/test_sv_verdict.py`

**Interfaces:**
- `sv-lab packs --tiers DIR --out DIR (--hemisphere north|south | --exclude-file FIELDS.tsv) --preset base|narrow`: reads `tier_500` (+ `tier_2000` for narrow) with `StarCache::iter_pixel_records` over every depth-6 pixel, keeps the stars outside the excluded sky (`FIELDS.tsv` rows `ra, dec, radius_deg`: every provoke field's cone, one restriction for all frames), writes caches with `cache::build_cache`, builds the pack with `index::build_index(&layers, out, &Preset::X.params(), …)`, then deletes the restricted caches (PC-9). Three restrictions × two presets in all.
- `sv-lab truth --frames $JOB/frames/owner/frames.tsv --out $JOB/manifest/truth.tsv`: resolves each `derive:<reference file>` row (PC-15 (2)): the reference's 0.3.0 hinted solve (`smc_old`, hinted at its header pointing and the optics scale 206.265 × XPIXSZ × XBINNING / FOCALLEN) must agree with the reference's header within 0.25 half-diagonals and 2 % in scale, else exit 3 naming the frame; the cloudy frame's truth is its own header pointing shifted by the reference's (solve − header) offset (tangent plane, in arcsec), with the reference's scale and rotation. `stored:` and `corpus` rows are copied with their source; a stored truth is cross-checked against the frame's own header within 0.25 half-diagonals (outside → exit 3). Writes `truth_source` per row; nothing is taken from the branch under test.
- `sv-lab run --manifest provoke.tsv --cycle DIR [--arm N] [--mode production|lowered]` → appends `DIR/attempts.tsv` (one row per attempt: `attempt_key, frame_key, arm, variant, mode, outcome, ra, dec, scale, log10_odds, reference_stars, matched_stars, inlier_ratio, hypotheses, prefilter_passed, own_star_count, inliers_outside_own, failure_class, best_log10_odds, solve_ms`; the last two own-set columns read the accepted candidate's trace through `solve_traced` on a success, and are the failed attempt's best candidate's otherwise) and `DIR/candidates.tsv` (one row per scored candidate of a failed attempt's `SolveFailure.verifies` with a decision: `attempt_key, idx, path, recentered, hypotheses_so_far, ra, dec, scale, log10_odds, log10_odds_quad4, required, reference_stars, test_stars, own_star_count, image_star_count, inliers_outside_own, scan_matched, matched_stars, peak_rank, tol_px, old_completeness_log_odds`); an `attempt_key` already present is skipped (resume).
- `sv-lab library --manifest library.tsv --cycle DIR` → `DIR/library.tsv`: per frame both solvers hinted at the stored centre and scale (epoch from `date_obs`), order old-first on even rows and new-first on odd rows (PC-12): `frame_key, set_id, new_{outcome,ra,dec,scale,log10_odds,reference_stars,matched,hypotheses,ms}, old_{outcome,ra,dec,scale,gate,ms}`; the old gate is 0.3.0's `Gate::default().accepts` fed as the app fed it (`expected_bright_in_fov` included).
- Solver set-up (both cycles): `SolveConfig { quad_tolerance: 0.007, catalog_mag_limit: 19.0, fit_sip: true, sip_order: 3, ..Default::default() }` (the app's recipe, `crates/athenaeum-core/src/plate_solve/service.rs:188-195`); lowered: `accept_log10_odds: f64::INFINITY, min_final_inliers: 4`; caches `Caches::layered(&[tier_500, tier_2000])` from `$JOB/tiers` (the release app's installed stack); packs `$JOB/packs/{base,narrow}` for arms 1, 2, 4, 5 and F, the arm-3 restriction packs for arm 3 (index hypotheses only; the spiral and verification use the full tiers, PC-9), none for the library cycle (the release app has none installed). Zip-resident frames are extracted to `$JOB/work/` and deleted after their attempts; rendered frames likewise.
- Arms (§7; PC-8…PC-11):

| Arm | Variants | Hints | Packs | Modes | Frames |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 1 wrong position | 5°, 20°, 90°, 180° | truth centre moved along position angle 90°, radius 2°, truth scale | base + narrow | production, lowered | provoke set |
| 2 wrong scale | × 0.25, × 0.5, × 2, × 4 (window 0.9–1.1 × the wrong scale through `solve_bounded`); `ladder` × 0.25 … × 4 (production hint, whole ladder) | truth centre, default radius, truth scale × factor | base + narrow | both | provoke set |
| 3 wrong sky | hemisphere; fields-excluded | blind | the opposite-hemisphere pack pair; the fields-excluded pack pair (PC-9) | both | provoke set |
| 4 no sky | permutation k = 0…4, more until V-2's 10⁴ scored false candidates (cap 50 per frame) | blind, rendered frame | base + narrow | both | provoke set |
| F recorded false | false-hint | the recorded false centre and scale, radius 0.5°; blind when `false_at` is `scale:`, `unknown` or `none` | base + narrow | both | three recorded false alignments |
| 5 truth | hinted; blind | truth centre and scale; none | base + narrow | production | hinted: 40 library + every owner frame with truth; blind: 40 library |

  Provoke set = the 40 library frames + the owner's narrow and DSLR frames + the three recorded false alignments.
- `sv_manifest.py --job $JOB`: reads the read-only catalog snapshots `~/.athenaeum-acc/gate-measure/db/*.db` (`mode=ro`), `~/.athenaeum-acc/gate-measure/scan/rows_scan.csv` and `$JOB/frames/owner/frames.tsv`; writes `manifest/library.tsv` (every distinct frame of the 4 231 stored solves, 3 207; release row wins over dev), `manifest/set287.tsv` (the 68 `LDN 621` rows of set 287), `manifest/lib40.tsv` (10 from set 217; the 10 weakest by the scan's final-WCS count `m1`, 28940 among them; 10 of the scan's 20 median-control rows; 10 from set 258's camera — distinct frames, `random.Random(7)`), `manifest/provoke.tsv`, `manifest/quick.tsv`. Columns: `frame_key, kind, path, zip_path, zip_entry, width, height, date_obs, set_id, object, truth_ra, truth_dec, truth_scale, truth_source, false_ra, false_dec, false_scale, false_kind (radec|scale|unknown|none)`. `truth_source` is `stored`, `corpus`, `header+reference` or `owner` (PC-15) and comes from `manifest/truth.tsv`; `kind` is counted by membership of the comma-joined value. Exit 3 with the shortfall named unless SV-9's minimum is present: 3 recorded false alignments, ≥ 10 cloudy, ≥ 3 narrow at ≤ 0.4″/px, ≥ 2 DSLR, every owner frame but the `truth none` false ones with a truth and a `truth_source`.
- `sv_verdict.py DIR…` → `verdict.txt` (≤ 30 lines, template below), `tables/{bars,per_path,per_frame,set287,comparison}.tsv`; `per_frame.tsv` carries, for each good frame, `truth_source`, `matched_stars` beside `scan_matched`, `own_star_count`, `reference_stars` and `peak_rank`, so a V-3 or V-4 failure on a narrow frame is attributable without a second run (a brighter junk detection within 3σ takes a reference from its real star, lowering `matched_stars`, which the gate's floor of 6 reads). A candidate or solve is **true** when its centre is within 0.02 half-diagonals (`truth_scale · hypot(W, H) / 7200` degrees) and 1 % in scale of its truth, else **false** (V-5's tolerance); an arm 1/2 attempt that ends true is reported apart. C's tail per candidate: `λ = N_R · min(0.5, n·π·σ²/A)`, `n = test_stars + own_star_count`, `log10 P(Poisson(λ) ≥ matched_stars)`, C accepts at ≤ −12. Bars, fixed here (§7): V-1 0 false accepted in production arms 1–4 and F (report `3/N`); V-2 ≥ 10⁴ distinct scored false candidates (candidates of one frame and attempt whose centre and scale lie within V-5's tolerance of each other count once, the highest-odds one kept; raw and distinct are both printed, and re-verified twins never inflate the count), best false `log10_odds ≤ 6` (§6.1's set; quad-4 variant reported), counts at ≥ 2, 3, 4 decades ≤ q95 of Poisson(H_total · 10⁻ᵏ) (the bar stays on H_total as §7 writes it), each recorded false alignment ≤ 6 — scored as the best candidate within V-5's tolerance of `false_at` (`scale:` form: within 1 % of that scale); none found → `NOT REPRODUCED`, which is not a PASS and goes to the owner; `false_at` unknown → the frame's best false candidate, labelled `best false (position not recorded)` in the verdict line — all per path. An INFO line prints H_total, pre-filter passed and scored, and the same counts at ≥ 2, 3, 4 decades against `scored × 10⁻ᵏ` (the README states the selection caveat: scored candidates are the survivors of the pre-filter); if that line is exceeded while the bar on H_total passes, the verdict says so and the owner is told; V-3 0 good refused (arm 5, cloudy, library) and 28940 solved; V-4 weakest good `log10_odds ≥ 12` (min, p1, p5); V-5 every library re-solve true; V-6 largest H per frame (report; > 10⁶ flagged); V-7 median new ms ≤ 1.10 × median old ms; coverage ≥ 99 % of planned frames read, else STOP.

```text
SOLVE-VERIFY VERDICT  cycle=<c>  sma=<sha>  date=<d>
coverage   library <r>/<p>  set287 <r>/68  provoke <n> frames  owner set false 3 cloudy <c> narrow <n> dslr <d>
attempts   production <n> (a1 a2 a3 a4 F)  lowered <n>  arm5 hinted <n> blind 40
V-1  false accepted <k>/<n> (95% < <b>%)                      bar 0            PASS|FAIL
V-2  scored false raw <r> distinct <n> (spiral <s>, index <i>) bar ≥ 10000 distinct   …
V-2  best false <x> (quad4 <y>) spiral <s> index <i>          bar ≤ 6          …
V-2  ≥2/3/4 dec <a>/<b>/<c> vs q95 <A>/<B>/<C> (H_total <h>)  bar within       …
V-2  recorded false <f1>/<f2>/<f3> (a value, NOT REPRODUCED → OWNER, or best-false label)   bar ≤ 6 each   …
INFO V-2  H_total <h> prefilter <p> scored <s>; ≥2/3/4 dec vs scored×10^-k q95 <A>/<B>/<C>  [EXCEEDED while the bar passes: tell the owner]
V-3  good refused arm5 <a>/<n> cloudy <c>/<n> library <l>/<n>; 28940 <solved|REFUSED>   bar 0   …
V-4  weakest good min <m> p1 <p1> p5 <p5>                     bar ≥ 12         …
V-5  library same answer <k>/<n>                              bar all          …
V-6  largest H <h> (<frame>)                                  report           INFO|FLAG
V-7  median ms new <a> old <b> (<±x %>)                       bar ≤ +10 %      …
apart      arms 1-3 ending true: <n>
compare    false accepted A/old/C <a>/<o>/<c>; good refused A/old/C <a>/<o>/<c>
STOP       <none | one line per failed bar, with its table>
```

- [ ] **Step 1:** Tests:
  - Rust `permutation_keeps_each_axis_and_breaks_the_pairs`: 600 points, seed 3 → x multiset and y multiset unchanged, fewer than 5 % of (x, y) pairs kept; same seed → same output.
  - Rust `a_rendered_frame_detects_back_its_stars`: 300 stars on 2000 × 1500 written as a float FITS in a temp dir → rustafits `detect_fast` finds 270…330.
  - Rust `restricted_cache_keeps_only_the_kept_sky`: the 40 000-star synthetic cache (as solvemyastro's `index.rs` test builds it) filtered north → every record `dec ≥ 0`; filtered by a fields file of two cones, 10° at (120, 30) and 5° at (300, −40) → no record within either; the restricted cache directory is gone after the pack build.
  - Rust `derived_truth_shifts_the_header_pointing_by_the_reference_offset`: reference header (10.00, 0.00), its solve (10.05, 0.02), cloudy header (10.20, 0.10) → truth (10.25, 0.12) within 1e-6° with the reference's scale and rotation; `derived_truth_refuses_a_reference_that_disagrees_with_its_header`: a reference solve 0.3 half-diagonals from its header → an error naming the frame (exit 3); 0.2 half-diagonals → accepted; a scale 3 % off → refused.
  - Rust `old_completeness_is_the_030_formula`: k 58, matched 7, p_random 0.0149 → `7·ln(0.25/0.0149) + 51·ln(0.75/0.9851)` within 1e-12 (the clamp of 0.3.0's `bright_star_log_odds`).
  - Python `test_poisson_q95`: μ 10 → 15; μ 0.1 → 1; μ 0 → 0.
  - Python `test_v5_tolerance`: 6252 × 4176 at 1.88″/px — a centre 0.019 half-diagonals away is true, 0.021 false; scale × 1.009 true, × 1.011 false.
  - Python `test_chance_tail`: λ 0.1, m 6 → −8.894 ± 0.001.
  - Python `test_distinct_candidates`: three rows of one attempt, two within V-5's tolerance of each other and one apart → distinct 2 (the higher-odds twin kept), raw 3; the same centre in two different attempts → distinct 2.
  - Python `test_recorded_false_not_reproduced`: a recorded false alignment with no candidate within tolerance of its `false_at` → `NOT REPRODUCED`, the line reads `OWNER`, never `PASS`; with `false_at unknown` the line carries the `best false (position not recorded)` label.
  - Python `test_info_line_flags_exceeded_counts`: scored 100, counts at ≥ 3 decades above q95 of Poisson(100 · 10⁻³) while the bar on H_total passes → the INFO line reads `EXCEEDED`.
  - Python `test_verdict_on_a_fixture`: a 12-row fixture with one planted false accept in arm 1 production → the V-1 line reads `FAIL` and a `STOP` line names it; without it → `PASS`.
- [ ] **Step 2:** Tests fail; **Step 3:** implement (the README states T of arm 4 is the re-detected list, PC-10, and the selection caveat of the INFO line); **Step 4:** `cargo test --release` in the tool dir (`CARGO_TARGET_DIR=<repo>/.superpowers/target-acc`) and `python3 -I -m unittest harness/acceptance/tests/test_sv_verdict.py` green.
- [ ] **Step 5:** Commit in athenaeum-lab and push `main`: `feat(solve-verify): the lab tool for the chance-or-real decision (arms, packs, verdict)`.

### Task L2: one command per cycle, and the smoke run

**Files:** create `harness/acceptance/solve-verify/sv-cycle.sh`; README section "Cycles".

**Interfaces:** `sv-cycle.sh smoke|provoke|library|verdict`: builds `sv-lab` (release, target-acc, `CARGO_BUILD_JOBS=4`); writes `runs/<cycle>/build.txt` (`git -C $W/solvemyastro rev-parse HEAD`, dirty flag); runs `sv_manifest.py` when a manifest is older than its inputs; runs the subcommands under `heavy_lock.py run --holder solve-verify -- $JOB/memrun.sh 8000 …`; ends with `sv_verdict.py` and prints `verdict.txt` (≤ 30 lines). `smoke` uses `manifest/quick.tsv` (28940, one set-217 frame, one median frame, one owner frame if any; one variant per arm; both modes; 20 library frames). Data under `$JOB/runs/<cycle>/`; nothing written elsewhere.

- [ ] **Step 1:** `sv-cycle.sh smoke` (background; wait for its notification). Expected: a verdict; every planned attempt has a row; lowered attempts have candidates; `$JOB/work/` empty at the end; the `memrun` line under 8000 MB; per-attempt median and maximum time printed with a projection of `provoke` and `library`; per attempt by arm and path the scored false candidates (distinct and raw) and the pre-filter count, with the projected distinct total of the provoke cycle within arm 4's cap of 50 permutations per frame; for the quick set, each accepted candidate's `own_star_count` and `inliers_outside_own` (a quick-set truth frame under 4 inliers outside its own set goes to the owner before R1).
- [ ] **Step 2:** Report (`task-L2-report-r1.md`): the smoke verdict, the yield projection, the own-set table, 28940's numbers (the scan measured 314 decades with four stars set aside; the production set may be larger, PC-2), the projection.
- [ ] **Step 3:** Commit in athenaeum-lab: `feat(solve-verify): one command per cycle; smoke verdict` (scripts and the smoke `verdict.txt` only).

### Gate L (controller)

- [ ] Re-run the tool's tests and `sv_verdict.py $JOB/runs/smoke`; the same lines. The projection ≤ 24 h, else take the runner question back to the owner. The projected distinct scored false candidates ≥ 10⁴ within arm 4's cap, else the owner before R1 (V-2's minimum is a yield, not a time). Review (opus) of L1 + L2: verdict math, true/false rule, bars as fixed above, the old-test recompute, resume, the disk lines.

---

## Phase R — the lab run against §7's bars

### Task R1: provoke, library, verdict

**Preconditions:** `sv_manifest.py` passes SV-9's minimum (the release waits for it); `build.txt`'s commit = Gate S's reviewed head (any solvemyastro change since → Gate S again).

- [ ] **Step 1:** `sv-cycle.sh provoke` (background, resumable, ~12 h projected). Memory rule: nothing else heavy runs; no analysis beside it.
- [ ] **Step 2:** `sv-cycle.sh library` (~3 h).
- [ ] **Step 3:** `sv-cycle.sh verdict` (both run dirs) → final `verdict.txt`.
- [ ] **Step 4 (controller):** independent re-gate: `sv_verdict.py` on the run dirs; open 28940's row, the best false candidate's frame (a stretched JPG, its per-star walk from the trace numbers) and set 287's table.
- [ ] **Step 5:** All PASS → athenaeum-lab results commit (`verdict.txt`, `tables/*.tsv` only); a Russian note to the owner in astrophotographer terms (what odds of 10^X to 1 mean, the margins, what was provoked) → **Approval B**.
- [ ] **Step 6:** Any FAIL → STOP: no fix here; the report names the bar, its value, the frames, the evidence files. V-1…V-3: the frame first; a model fault (the own-star set, σ, a uniform background in a clustered field) goes to a fresh Phase S fix task and the whole run repeats from Step 1; a threshold is never moved alone. V-3 on a cloudy or narrow frame whose matches sit deep in its SNR order → the owner's SV-8 question. V-5 → the owner (a finding about the stored library). A recorded false alignment reading `NOT REPRODUCED` → the owner. V-6 > 10⁶ → reported (the clause is active). V-7 → profiled (`print_timing`) and reported.

Agent: sonnet (a judgment run with fixed bars); the controller's re-gate is never cut.

### Gate R

All of V-1…V-7 PASS (V-6 reported), coverage ≥ 99 %, the owner's go (Approval B) for the release.

---

## Phase P — solvemyastro 0.4.0

### Task P1: merge, tag, publish, verify (controller, Approval B)

- [ ] **Step 1:** On solvemyastro `main`: `git merge --no-ff verification -m "Merge verification: one chance-or-real decision (0.4.0)"`; `CHANGELOG.md` heading `## [0.4.0] — <date>`; commit `release: v0.4.0`; tag `v0.4.0`; push `main` and the tag to `all`. If the session may not push the tag, hand the owner the exact `git push all main` / `git push all v0.4.0` lines.
- [ ] **Step 2:** `release.yml` runs (tag check, notes from the section, crates.io with `CARGO_TOKEN`); re-run safe with `gh run rerun <id> --failed`.
- [ ] **Step 3:** Verify by fact: `curl -s -A release-check https://crates.io/api/v1/crates/solvemyastro | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["crate"]["max_version"])'` → `0.4.0`; the GitHub release shows the section; `CARGO_TARGET_DIR=<repo>/.superpowers/target-acc cargo install solvemyastro --version 0.4.0 --features cli --locked --root <scratch>/sma` and `<scratch>/sma/bin/solvemyastro solve --help` lists `--json` and `--no-gate`.
- [ ] **Step 4:** Ledger: the tag's commit. Delete `$JOB/sma-repo` and its row.

---

## Phase A — Athenaeum (branch `solve-verification`)

### Task A1: pointer bump to the tag; gate, config, failure, recipe 3

**Files:**
- Modify: `solvemyastro` (submodule pointer → the `v0.4.0` commit on solvemyastro `main`), `Cargo.lock`
- Modify: `crates/athenaeum-core/src/plate_solve/service.rs:27-63` (`SolveResult` + `log10_odds`; docs: `expected_catalog_stars_in_fov` = the in-frame reference stars `N_R`, `inlier_ratio` = matched / `N_R`), `:272-294` (no `expected_bright_in_fov`; take `reference_stars_in_frame`, `inlier_ratio`, `log10_odds` from the solution), `:345-365`, `:388-424` (`store_result`: comment and warn fields, `log10_odds` replaces `inlier_ratio`), `:462-488` (`acceptance_refusal`), tests `:626-655` (`mk_result(matched, in_field, ratio, scale, rms, odds)`), `:661-670`, `:870-1000`, `:1353-1370`
- Modify: `crates/athenaeum-core/src/plate_solve/config.rs:51-55`, `:125-127`, `:147-166`, `:176-188` (six fields), `:229-239` (doc + `SOLVE_RECIPE_VERSION: u32 = 3`, the note "3: solvemyastro 0.4.0 decides chance-or-real by a Bayes factor; the completeness test and the 4 % rule are gone"), `:241-257`, tests `:277-306`, `:312-322`, `:385-409`
- Modify: `crates/athenaeum-core/src/plate_solve/failure.rs:24-58` (the `NotConfident` arm)
- Modify: `crates/athenaeum-core/src/api/plate_solve.rs:453-454` (the `failure_code` doc comment names `NOT_CONFIDENT` beside `REJECTED_LOW_CONFIDENCE`)
- Modify: `crates/athenaeum-core/src/stacking/run/solve_tests.rs:123` (fixture text in the new form)
- Modify: `src/types/plate-solve.ts` (regenerated), `src/components/settings/tabs/registry-coverage.test.tsx:56`, `src/components/plate-solve/PlateSolveSettingsPanel.test.tsx:32` (the ratio key goes)

**Interfaces:**
- Consumes: solvemyastro 0.4.0 (`SolveSolution` fields, `GateMetrics`, `Gate::refusal`, `GateRefusal`, `FailureClass::NotConfident`, `verify::ACCEPT_LOG10_ODDS`).
- Produces: `SolveResult.log10_odds: f64`; `acceptance_refusal` returns `Some(format!("Rejected: {refusal}"))`; `describe_solve_failure` for `NotConfident { best_log10_odds }` → code `NOT_CONFIDENT`, message `A candidate position was found, but its star match could be chance (best odds 10^{best:.1} to 1; 10^{ACCEPT_LOG10_ODDS} needed).`
- `acceptance_refusal` always builds `GateMetrics` with the solution's `log10_odds`; with `blind_gate_enabled` false it returns `None` before building them, as today (§9's builder point settled).
- `solver_config_value` JSON pinned: `{"baseVerificationToleranceArcsec":8.0,"blindGateEnabled":true,"blindInlierFloor":6,"blindRmsMaxPxMult":2.5,"blindScaleHeaderTol":8.0,"blindScaleSanityMax":60.0,"blindScaleSanityMin":0.05,"recipe":3,"sipOrder":3}`.

- [ ] **Step 1:** `git -C solvemyastro fetch origin && git -C solvemyastro checkout v0.4.0`; `cargo update -p solvemyastro`; `cargo check -p athenaeum-core` lists the breaks (expected only in the files above).
- [ ] **Step 2:** Tests:
  - `acceptance_refusal_names_the_failed_check`: `mk_result(6, 61, 0.098, 0.2178, 1.159, 4.8)`, no header → `Rejected: the match could be a chance alignment (odds 10^4.8 to 1, 10^9 needed; 6 stars matched)`; `(5, 400, 0.0125, 1.9, 1.0, 50.0)` → `Rejected: only 5 stars matched (6 needed)`; `(120, 476, 0.25, 1.9, 20.0, 300.0)` → `Rejected: the stars fit too loosely (20.00 px RMS, 10.53 px allowed)`; `(120, 476, 0.25, 100.0, 1.0, 300.0)` → `Rejected: the solved scale 100.00"/px is outside 0.05..60"/px`; `(120, 476, 0.25, 29.6, 1.0, 300.0)` with header 1.873 → `Rejected: the solved scale 29.60"/px is more than 8x off the header's 1.87"/px`.
  - `acceptance_refusal_keeps_its_decisions`: the noise case refused; `(60, 61, 0.98, 0.2178, 1.159, 120.0)` accepted; gate off → `None`. `acceptance_refusal_is_store_results_gate` with the texts above.
  - `not_confident_reads_as_a_chance_match`: a `SolveFailure` with `NotConfident { best_log10_odds: 6.2 }` → code `NOT_CONFIDENT`, message `A candidate position was found, but its star match could be chance (best odds 10^6.2 to 1; 10^9 needed).`
  - `acceptance_gate_maps_every_field` (six distinct values, six fields); `the_default_gate_is_solvemyastros`; `legacy_config_json_loads_ignoring_removed_keys` (ratio assertion dropped); `a_stored_config_with_the_ratio_key_decodes_and_saves_without_it` (`{"blind_min_inlier_ratio":0.04,"blind_inlier_floor":20}` → floor 20; saved JSON lacks `blind_min_inlier_ratio`); `the_recipe_is_three`; `the_solver_config_hash_covers_what_shapes_a_solve` (the JSON above; pin the hash the test computes — the string assertion is the check on its input).
- [ ] **Step 3:** Implement. `git grep -n -e 'assert_eq!(h, "' -e 'hash, "' -- crates/athenaeum-core/src` → only the config pin moved.
- [ ] **Step 4:** `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then without `TS_RS_WRITE` → green (`PlateSolveConfig` lost `blind_min_inlier_ratio`); remove the two fixture lines.
- [ ] **Step 5:** `cargo test -p athenaeum-core plate_solve:: stacking::solve stacking::run::solve -- --test-threads=4`; `cargo check --workspace`; `npx tsc --noEmit`; `npx vitest run src/components/settings/tabs/registry-coverage.test.tsx src/components/plate-solve/PlateSolveSettingsPanel.test.tsx`.
- [ ] **Step 6:** Commit (with the pointer and `Cargo.lock`): `feat(plate-solve): solvemyastro 0.4.0 — the solver's odds decide, the gate re-reads them (recipe 3)`.

### Task A2: `plate_solves.log10_odds`; the old in-field count cleared to unknown

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs:2237-2266` (`log10_odds REAL` in the DDL), `:2275-2288` (new guarded step after it), tests (near `:4551`)
- Modify: `crates/athenaeum-core/src/plate_solve/storage.rs:10-45` (`#[serde(default)] pub log10_odds: Option<f64>`, docs of the three confidence fields with their recipe-3 meaning), `:117-175`, `:177-184`, `:186-216` (column 27), test helper `:356-390`
- Modify: `crates/athenaeum-core/src/plate_solve/service.rs:491-523` (`expected_catalog_stars_in_fov: Some(N_R)`, `inlier_ratio`, `log10_odds: Some(result.log10_odds)`), test `:1375-1391`
- Modify (add `log10_odds: None`, or `Some(…)` in a fixture that wants one): `analysis/transparency.rs:1083`, `analysis/zero_point.rs:2386`, `api/analysis.rs:2173`, `api/frame_sets.rs:2139`, `collab/frame_meta.rs:311` and `:471`, `fits_writer/wcs.rs:290` and `:438`, `stacking/grid_map.rs:410` and `:493`, `stacking/master_cards.rs:1318`, `stacking/register/wcs_seed.rs:95`, `stacking/solve.rs:727`, `stacking/test_fixtures.rs:1437`, `plate_solve/storage.rs:361` (all under `crates/athenaeum-core/src/`)
- Modify: `crates/athenaeum-core/src/stacking/solve.rs` tests (near `:770-815`); `src/types/plate-solve.ts` (regenerated)

**Interfaces:** Produces the column `plate_solves.log10_odds REAL` (nullable) and `PlateSolveRecord.log10_odds: Option<f64>`; TS `log10_odds: number | null` with its doc. Guarded step: when `pragma_table_info('plate_solves')` has no `log10_odds`, `UPDATE plate_solves SET expected_catalog_stars_in_fov = NULL` first, then `ALTER TABLE plate_solves ADD COLUMN log10_odds REAL`, both in one transaction (a stop between them can never leave the column present with the old count uncleared; the guard is the column's absence, so a re-run repeats the clearing); it runs once per catalog; `matched_stars` and `inlier_ratio` are not touched.

- [ ] **Step 1:** Tests:
  - `the_guarded_step_adds_log10_odds_and_clears_the_old_in_field_count`: a connection with the 0.3.x `plate_solves` DDL (the `:2237-2266` text without the column) and a row (in-field 1, ratio 0.2, matched 15) → `init_db` → the column exists, in-field NULL, ratio 0.2 and matched 15 kept; a row inserted after with in-field 476 and odds 314.4 → `init_db` again → both kept; a connection where only the clearing ran (no column, in-field already NULL) → `init_db` adds the column.
  - `a_record_round_trips_its_odds`: insert with `Some(314.4)` → `get_plate_solve` → `Some(314.4)`; `a_row_without_odds_reads_none` (insert through raw SQL without the column) → `None`.
  - `record_from_result_is_the_stored_row`: `(Some(476), Some(0.25), Some(300.0))` for in-field, ratio, odds.
  - `a_cached_solve_without_odds_decodes`: a `SolveArtifact` JSON whose `solved` record has no `log10_odds` key → decodes, `log10_odds == None`.
- [ ] **Step 2:** Tests fail; **Step 3:** implement.
- [ ] **Step 4:** `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then without it; `cargo test -p athenaeum-core plate_solve:: db::schema stacking::solve collab::frame_meta fits_writer stacking::grid_map -- --test-threads=4`; `cargo check --workspace`; `npx tsc --noEmit`.
- [ ] **Step 5:** Commit: `feat(catalog): plate_solves.log10_odds; the in-field count of the 4 % rule era is cleared to unknown`.

### Task A3: the metadata pane shows the odds and the true denominator

**Files:**
- Create: `src/components/dualpane/plateSolveText.ts`, `src/components/dualpane/plateSolveText.test.ts`
- Modify: `src/components/dualpane/MetadataPane.tsx:1296-1304` (Inliers cell) and a new `Confidence:` cell in the same grid (`:1266-1314`)
- Modify: `src/components/plate-solve/PlateSolveBatchPanel.tsx:306` (`rejected` is `code === 'REJECTED_LOW_CONFIDENCE' || code === 'NOT_CONFIDENT'`: both read as the warning style), `src/components/plate-solve/PlateSolveBatchPanel.test.tsx` (one case), `src/types/helpers.ts:420` (the `failure_code` doc comment names `NOT_CONFIDENT`)

**Interfaces:** `formatSolveOdds(log10Odds: number | null | undefined): string` → `odds 10^${Math.floor(x)} to 1`, `—` for null, undefined or a non-finite value; `formatInliers(matched: number, inField: number | null | undefined): string` → `${matched} / ${inField}` when `inField > 0`, else `${matched}`. Same label and value classes as the neighbouring cells (`text-content-muted`, `font-mono text-content`); no raw colours.

- [ ] **Step 1:** Tests (vitest): `formats odds rounding down` (314.9 → `odds 10^314 to 1`; 9.0 → `odds 10^9 to 1`); `shows a dash for a solve stored before the change` (null, undefined, NaN → `—`); `inliers show the in-frame reference count when known` (120, 476 → `120 / 476`; 15, null → `15`); `never prints a zero denominator` (15, 0 → `15`). In `PlateSolveBatchPanel.test.tsx`: `a NOT_CONFIDENT failure is styled as a warning` (a failed frame with `code: 'NOT_CONFIDENT'` renders the warning icon class, `text-warning`, as `REJECTED_LOW_CONFIDENCE` does).
- [ ] **Step 2:** Fail; **Step 3:** implement; the pane reads `plateSolve.log10_odds` and `plateSolve.expected_catalog_stars_in_fov` through the helpers.
- [ ] **Step 4:** `npx vitest run src/components/dualpane/plateSolveText.test.ts src/components/plate-solve/PlateSolveBatchPanel.test.tsx`; `npx tsc --noEmit`.
- [ ] **Step 5:** Launch the dev app (`npm run tauri dev`, the dev catalog): a frame solved before reads `Confidence: —`, `Inliers: <m>`; re-solve one frame → `Confidence: odds 10^N to 1`, `Inliers: m / N_R`. If the launch cannot happen in the session, the check goes to open-items as owed (A4).
- [ ] **Step 6:** Commit: `feat(ui): the plate-solve block shows the solve's odds and its true match denominator`.

### Task A4: docs of §8

**Files and edits:**
- `docs/platesolving/input-and-acceptance-gates.md:123-135`: the decision (one paragraph, [1] §2.4), the gate table (five rows of §6.3), history: the 4 % rule never armed (in-field count 0) and would have refused 220 of 4 231 good solves (212 from set 217); the completeness test would refuse frame 28940 on a re-solve; the per-star scan cleared 291 of 291 rows, weakest 314 decades; recipe 3.
- `docs/platesolving/precise-registration.md:75`: the `SolveSolution` field list gains `log10_odds`, `reference_stars_in_frame`, `hypotheses_verified`; `matched_stars` is the final-WCS count.
- `docs/platesolving/README.md:82` (recipe 3), `:355-374` (the `SolveResult` fields and the gate list, the `N_R` note), `:587` (`matched_stars`: final-WCS count from recipe 3, linear count before), `:613-639` (`blind_min_inlier_ratio` removed; a stored key is ignored).
- `docs/superpowers/open-items.md:442-449`: the section "The in-field count reads 0" is removed; owed smokes added: the pane's number on a fresh solve (unless A3 Step 5 ran), one stacking set re-solving once under recipe 3.
- `CLAUDE.md:132`: the Plate solving block gains one clause — the solver decides chance-or-real by a Bayes factor at 10⁹ (`verify::ACCEPT_LOG10_ODDS`); the gate holds backstops and re-reads it.
- `docs/stacking/README.md:1415-1421`: recipe 3 (solvemyastro 0.4.0's decision) re-solved cached SOLVED solves once.
- `docs/logging/README.md:110-112`: a dictionary extension for `log10_odds`, `reference_stars`, `test_stars`, `peak_rank`, `scan_matched`, `hypotheses`, `required_log10_odds` (solvemyastro's `solve decision` debug event; `store_result`'s refusal warn).

- [ ] **Step 1:** Edits; no program named. **Step 2:** Commit: `docs: solve verification — the decision, the gate, recipe 3`.

### Task A5: gates, Windows, review, merge (controller)

- [ ] **Step 1:** `cargo test --workspace -- --test-threads=4` once (known flakes per memory re-run once; rustafits `fit_moffat_2d_is_byte_identical_to_before` is the known red), `cargo check -p athenaeum-core --no-default-features`, `npx tsc --noEmit`, `npx vitest run`.
- [ ] **Step 2:** `git push origin <sha>:refs/heads/wintest/solve-verification`; read the EVO-X2 `test:windows` job through the GitLab API → green.
- [ ] **Step 3:** Whole-branch review of `d7608fee..solve-verification` on the most capable model: refusal-text seams (solvemyastro `GateRefusal` ↔ app tests ↔ stacking fixtures), the serde/TS seam, the recipe hash pin, the guarded step's idempotence, every `PlateSolveRecord` literal, the stored-row rule (SV-6, SV-11). Fixes → a fresh agent, then Steps 1–2 again.
- [ ] **Step 4:** Merge into local `main`; **Approval C** for `git push origin main` then `git push github main`; remove the worktree with `git worktree remove --force`.
- [ ] **Step 5:** Close: ledger `Disk:` line; delete `$JOB` and `~/.athenaeum-acc/gate-measure` with their rows and `.superpowers/target-acc/tmp-gate-measure` with its row (results are in the spec, the lab commit and the ledger); memory `project_solvemyastro.md` (0.4.0) and `project_solve_verification.md` cut to non-derivable facts; owed smokes stay in open-items.

---

## Consumers of the changed meanings

| Site | Reads | Change |
| ---- | ---- | ---- |
| solvemyastro `src/lib.rs:275-292` | `matched_stars`, `inlier_ratio` docs | S4: final-WCS count; over `N_R` |
| solvemyastro `QUAD_INDEX.md:235`, `:263` | the completeness gate, its debug event | S6 |
| `src/orchestrate.rs:2156-2185`, `:2374-2432` | the counts, the completeness test | S4 |
| `src/accept.rs` (whole) | `expected_in_fov`, `inlier_ratio`, ratio rule | S5: removed; `log10_odds` read |
| `src/bin/solvemyastro/solve.rs:10-174` | `expected_bright_in_fov`, ratio, JSON | S5 |
| `tests/corpus_bench.rs:481-483, :543-559, :650-680` (legacy bench) | records `matched_stars`, `inlier_ratio` | none in code; its committed baseline keeps 0.3.0 meanings (PC-7) |
| `tests/scale_fallback.rs:79` | `matched_stars >= 10` | none: the final count is larger (median 2.9 ×, §5.1) |
| `tests/synth_bench.rs:175`, `examples/count_pane4.rs:37` | print `matched_stars` | none |
| `README.md:36-50`, `CHANGELOG.md` | JSON keys, known issue | S6 |
| core `plate_solve/service.rs:27-63, :272-294, :345-365, :388-488, :491-523`, tests | all four fields, the gate | A1, A2 |
| core `plate_solve/config.rs` | `blind_min_inlier_ratio`, recipe, hash | A1 |
| core `plate_solve/failure.rs` | `FailureClass` (exhaustive match) | A1: `NOT_CONFIDENT` |
| core `plate_solve/storage.rs`, `db/schema.rs` | the columns | A2 |
| core `api/plate_solve.rs:875` (progress `matched_stars`), `:955-972` (refusal reason) | count, text | none in code: the queue gets the final count and the named refusal |
| core `stacking/run/normalize_tests.rs:3168` | `matched_stars >= 4` on a fixture | none (a fixture value) |
| core `api/plate_solve.rs:453-454` | `failure_code` doc comment | A1: names `NOT_CONFIDENT` |
| core `stacking/solve.rs:100-113, :319-327` | cached `PlateSolveRecord`, the gate | A2 decode test; recipe 3 re-solves cached SOLVED once (A1) |
| core `stacking/run/solve_tests.rs:123` | a fixture refusal text | A1 vocabulary |
| core `registration/db.rs`, `stacking/register/frame.rs:45-57`, `stacking/run/{frames.rs:161-174, output.rs:120-130, register.rs:543-553}`, `db/stacking.rs` `reg_inlier_ratio` | registration's own count and ratio | none (non-goal) |
| core `collab/frame_meta.rs:302-345` (`to_record`), `:85-200` (`build_frame_meta`), `:221-245` (`ManifestWcs`) | builds a record; the wire carries the WCS only | A2 `log10_odds: None`; **no changed field crosses the collab wire** (frozen, golden pins untouched) |
| athenaeum-hub (whole repo) | — | no plate-solve field (git grep empty): nothing crosses the hub |
| core `catalog_query/details.rs:158`, `mcp/plans/plate_solve.rs:434-478, :1123` | row presence, progress | none: MCP answers carry no solve-quality field |
| core `fits_writer/wcs.rs:143` | copies `matched_stars` | none; no FITS card carries a match count |
| core `api/spatial.rs:506-603`, `api/zip.rs:1011`, `catalog_query/summary_tests.rs:389`, `mcp/plans/plate_solve_tests.rs:96`, `api/collab.rs:6641` | INSERTs without the new column | none (nullable) |
| core `tests/blind_index_solve.rs:172-264` (ignored, local) | `matched_stars >= 15` | none (final count larger) |
| `src/components/dualpane/MetadataPane.tsx:1296-1304` | `matched_stars / expected` | A3 |
| `src/hooks/usePlateSolveQueue.ts:14, :470`, `src/hooks/usePlateSolveQueue.test.tsx`, `src/types/helpers.ts:417` | progress count | none (final count) |
| `src/components/plate-solve/PlateSolveBatchPanel.tsx:306`, `src/types/helpers.ts:420` | styles only `REJECTED_LOW_CONFIDENCE` as a warning; `failure_code` doc | A3: `NOT_CONFIDENT` is a warning too, one vitest case; the comment names it |
| `src/types/plate-solve.ts` | both TS types | A1 (config), A2 (record), regenerated |
| `registry-coverage.test.tsx:56`, `PlateSolveSettingsPanel.test.tsx:32` | the ratio key | A1 |
| `docs/platesolving/precise-registration.md:75` | `SolveSolution`'s field list | A4 |
| `docs/platesolving/*.md`, `docs/stacking/README.md`, `docs/logging/README.md`, `docs/superpowers/open-items.md`, `CLAUDE.md` | meanings, recipe, fields | A4 |

## Owned quantities

| Quantity | Owner task | Bar / test | Propagation (readers only) |
| ---- | ---- | ---- | ---- |
| Test-star order T | S4 (`image_pts`, `select_fast_stars` order, cap 600, minus own stars) | S3 order tests; V-3, V-4 | `scan_log10_odds`; `VerifyTrace.test_stars`; lab C tail |
| `N_R` and the reference list | S4 (`references_in_frame` on `sip_wcs` over `verify_stars`) | `references_in_frame_keeps_finite_in_frame_projections`; V-4 | scan; `SolveSolution.reference_stars_in_frame`; `inlier_ratio`; CLI `reference_stars`; app `expected_catalog_stars_in_fov` (A1/A2); pane denominator (A3); lab |
| σ | S4 (`tol_px` of `verify_trials`) | S3 tests take it as input; V-2, V-4 | scan; `VerifyTrace.tol_px`; lab C tail |
| Own stars per path | S2 (PC-2, PC-3) | `quad_match_stars_…`, `a_hypothesis_names_its_image_quad`; V-2 per path, both variants | scan `own_stars` (S4); `VerifyTrace.own_star_count`, `log10_odds_quad4`; lab |
| H | S2 (`SolveStats`, PC-4) | `solve_stats_count_every_hypothesis_once`, `the_bar_rises_beyond_a_million_hypotheses`; V-6 | the bar (S4); `hypotheses_verified` on solution and failure; CLI `hypotheses`; lab H_total |
| `log10_odds` | S3 | S3 unit tests; V-1…V-4 | decision (S4); `SolveSolution`; `VerifyTrace`; `GateMetrics` (S5); CLI; `NotConfident` (S4, A1); app result, record, column, refusal texts, pane (A1–A3) |
| `matched_stars` (final WCS) | S3 (count rule, PC-1); S4 stores it | `own_stars_claim_their_references_first`, `the_cut_offs_…`; corpus asserts | gate floor (S5); `inlier_ratio`; CLI `matched`; app column, pane, progress, refusal texts |
| `inlier_ratio` | S4 (`matched_stars / N_R`) | corpus assert `|ratio − matched/N_R| < 1e-12` | CLI JSON; app column (stored, never judged); docs |
| Own-set measure (`inliers_outside_own`) and `prefilter_passed` | S2 (`VerifyTrace`, `SolveStats`) | S2 Step 5 corpus keys; no truth frame under 4 | lab attempts/candidates tables; V-2's INFO line; L2 smoke |
| Truth of an owner frame (`truth_source`) | L1 (`sv-lab truth`, PC-15) | `derived_truth_…` tests; V-3, V-5 | `sv_manifest.py`; `per_frame.tsv` |
| Distinct scored false candidates | L1 (`sv_verdict.py`) | `test_distinct_candidates`; V-2 | verdict; Gate L projection |

## Design faults and open points found while planning

1. §8's frozen baseline: the legacy `corpus_bench` needs a deep `smac_gaia/stars.smac` (gone on the dev Mac) and `corpus_layered_tiers` records no WCS and needs `tier_5000`/`tier_8000` (not installed) → PC-7 (`tests/corpus_bench.rs:372-388, :1055-1110`).
2. §7 arm 2's "10 % window" has no public expression: the hinted ladder spans 1.5⁴ each way and B1/B2/C widen it (`orchestrate.rs:163-201`, `lib.rs:395-448`) → PC-8.
3. §7 arm 3's "every pack but the true field's": packs are presets, not sky regions (`index.rs:199-240`) → PC-9.
4. §7 arm 4: no public entry takes a detection list (`lib.rs:358-380`) → PC-10.
5. §9 threads H through A, B1, B2; step C verifies too (`lib.rs:439-448`) → PC-4.
6. §6.1 "their catalog stars are claimed": the quad's catalog stars are not in R's list → PC-1.
7. The lab cannot read scored candidates through 0.3.0's API (trace only logged, `orchestrate.rs:1430-1438`) → PC-14.
8. The own-star set on weak frames is unmeasured (the scan's proxy stopped at 28; all filtered matches can be more): S2 Step 5 now measures `inliers_outside_own` on every corpus truth frame (a frame under 4 goes to the owner before S3) and L2's smoke repeats it for the quick set; V-4 stays the end-to-end check.
9. Set 287 has 153 rows; the spec's "68 rows" are its `LDN 621` rows (the set also holds Cr 302 and one IC 4606 frame) — the manifest takes those 68.
10. The 0.39″/px recorded false alignment (commit ba6048a) is answered: a real corpus frame (`2024-09-08_04-30-13_L_-9.90_180.00s_0008.fits`, RA 85.391 Dec −2.441), probable false solve at 0.25″/px blind with the narrow pack; its recorded false position is only a scale, so arm F reads it as `scale:0.25` and a lab run that does not regenerate it reads `NOT REPRODUCED` and goes to the owner (finding 6).
11. `corpus_bench.rs` defaults to a bench folder that does not exist; every corpus command sets `SOLVEMYASTRO_BENCH_DIR` to `…/Pictures/star test/singles`, read in place (C2).
12. The false solve of `_DSC5767` was recorded without its RA: `false_at` is unknown and its score is the frame's best false candidate, labelled (finding 6, C3).

## Review fold (r1)

Findings are the planning review's (`plan-review-fable.md`); C1–C5 are the controller's rulings, U1–U2 the coordinator's update made while folding.

| Finding / ruling | Plan section changed |
| ---- | ---- |
| 1 own set on thin frames | S2 (VerifyTrace own-set keys, `solve_traced`, Step 5 measurement and owner stop), Review Focus 6, L1 candidates/attempts columns, L2 Step 1, open point 8, owned quantities |
| 2 blind index pass | S1 (Interfaces, Steps 3, 5, acceptance), PC-16, `CORPUS` command (`SOLVEMYASTRO_INDEX_DIR`) |
| 3 arm 2 window | S2 (`solve_bounded`), PC-8, L1 arm table, "For the owner at approval" |
| 4 V-2's yield | L1 verdict rules and template (distinct, raw), `test_distinct_candidates`, L2 Step 1, Gate L, PC-14 / S2 / S4 (`prefilter_passed`) |
| 5 Ville-bound check | L1 verdict INFO line and README caveat, `test_info_line_flags_exceeded_counts` |
| 6 recorded false alignments | L1 V-2 rule, template line, `test_recorded_false_not_reproduced`, `false_kind`, R1 Step 6, Setup rows, design faults 10 and 12 |
| 7 arm 3 cost | PC-9, L1 `packs` and arm table, Setup disk line, `apart` line, "For the owner at approval" |
| 8 truth of owner frames | PC-15, Setup (`frames.tsv`, `truth_source`), L1 `sv-lab truth`, manifest, `per_frame.tsv`, owned quantities |
| 9 normalisation test | S3 `the_per_star_ratio_averages_below_one` |
| 10 guarded step order | A2 Interfaces and test |
| 11 `NOT_CONFIDENT` style | A1 (doc comment), A3 (panel, helpers, vitest case), consumer table, summary table |
| 12 doc and consumer gaps | S6 Files and Steps 2, 5 (`QUAD_INDEX.md`), A4 (`precise-registration.md`), consumer table (two rows and more) |
| 13 name-scan pattern file | S6 Step 5 (ledger folder, git-ignored) |
| 14 tier hashes, `cargo install` target | Setup tiers step, P1 Step 3 |
| 15 `matched_stars` attribution | L1 `per_frame.tsv` |
| C1 truth rule (agreement 0.25 half-diagonals, U1) | PC-15, Setup, L1 `sv-lab truth` and its tests |
| C2 corpus folder | Global Constraints (`CORPUS`), Setup folder check, design fault 11 |
| C3 three false alignments | Setup `frames.tsv` rows, design faults 10 and 12 |
| C4 Setup's owner question | Setup (ask only cloudy, narrow, runner) |
| C5 owner at approval | "For the owner at approval" |
| U2 cloudy frame 0055 stored solve | Setup `frames.tsv` row (truth_source stored, cross-checked by its header) |
