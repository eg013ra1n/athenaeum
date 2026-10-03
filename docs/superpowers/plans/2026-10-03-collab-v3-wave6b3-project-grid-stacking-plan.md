# Collab v3 wave 6b-3 — a project stacks onto its canonical grid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A collaboration project stacks end to end on the processor's machine. Every frame is registered onto the coordinator-confirmed canonical grid through one composed map. Local normalization works on each member's overlap with the LN reference and falls back to global normalization below 25 %. The run writes a per-pixel coverage map, can crop to it, and writes grid masters (and drizzled masters) carrying the grid's WCS and the canonical filter. Seven project-stacking commands run on both hosts (strict count 299 → 306), `start_stacking(Project)` starts, a fake-hub end-to-end test proves the whole path, and acceptance runs A and B are prepared for the controller. Frame-set stacking stays byte-identical.

**Architecture:**
- **One composed `PixelMap` per frame.** Its affine part is fitted to the exact chain subject → reference (the star map) → sky (the reference's WCS with SIP) → grid (the grid's TAN). Its distortion layer is a new `DistortionModel::Sampled`: a displacement field evaluated exactly through the chain and sampled lazily into the same bilinear grids a spline uses. The writer, `RegisteredSource`'s warp fallback, LN, integration and drizzle all take this one object.
- **Two caches, two kinds.** The star map is a payload-only `registration` artifact, which survives every cleanup but `All`. The registered frame's hash adds the grid and the reference's WCS. So a grid change recomposes and rewrites registered frames with no star detection.
- **Seam 1 and seam 2 land together.** The engine learns to count, per output pixel, the frames whose sample is finite, and to leave a pixel nobody covers NaN. The count is a post-pass over the band already in memory, so the per-pixel combine loop is untouched. A project's LN reference is built with "empty → NaN", and its LN fits run on the member ∩ reference overlap, masked. Frame sets keep the legacy LN everywhere.
- **Coverage and crop are output features.** The coverage map is written beside the master (frame sets too, off by default). Crop is an opt-in deferred pass: outputs go to scratch FITS first, the rectangle is computed after the last group, and the final files are written once.
- **Output on the grid.** The master's WCS is a synthetic TAN record of the grid. `FILTER` is the canonical filter, `ROWORDER` is `BOTTOM-UP` (the grid's own), and `ATH_SPRJ`, `ATH_GRID` and `ATH_RGEO = 'grid'` are stamped. `ATH_PRJ` is never written. Masters land in `<output>/<project title>/`.
- **The commands are thin.** Seven core functions in `api/stacking.rs`, one wrapper each on Tauri and Axum.

**Tech Stack:** Rust (`athenaeum-core`): rusqlite 0.40, serde, xxhash, rayon, rustafits (`astroimage`), solvemyastro (`WcsSolution`). ts-rs (`tests/ts_contract.rs`). Axum (`athenaeum-web`) and Tauri (`athenaeum-tauri`) wrappers. Python 3 + numpy for the acceptance drivers. No frontend code (plan 6c).

**Spec:** `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` (owner-approved). Read §2, §4.2, §5.5–§5.9, §6, §7, §9, §14–§16 and §18 before any task. This plan implements §18 plan 6b steps **8, 9, 10, 11, 12 and 14**, and prepares the §15.3 acceptance runs A and B. The frontend is plan 6c. The wire types 6c reads are finished here.

**Execution order:** Task 0, 1, 2a, 2b1, 2b2, 3, 4, 5a, 5b, 7a, 6a, 6b, 7b1, 7b2, 8a, 8b, 8c, 9, 10, 11, then the acceptance runs (the controller's own agent). Tasks appear in this document in execution order. Each task leaves every gate green on its own. Task 7a runs before 6a because 6a measures grid-master statistics through 7a's `largest_covered_rect` (review I1); 2b and 7b are split at the review's cut points.

**Review folded in:** `.superpowers/sdd/6b3-planning/plan-review-fable.md` (0 Critical, 7 Important, 18 Minor) under the controller's rulings `.superpowers/sdd/6b3-planning/plan-review-rulings.md`. Changed rulings carry **Refinement (review)**; every fix names its finding (I1–I7, M1–M18) where it lands.

**What 6b-1 and 6b-2 built** (read before Task 1):
- `docs/stacking/README.md`: "Run subjects (wave 6b-1)" and "The project path, wave 6b-2", including its "Open for 6b-3" list.
- `docs/transfers/README.md`: "Collab wave 6b-2 — the canonical grid".
- `docs/superpowers/open-items.md`: the 6b-2 entry.
- The executed 6b-2 plan `docs/superpowers/plans/2026-10-03-collab-v3-wave6b2-project-path-plan.md` and its ledger `.superpowers/sdd/2026-10-03-collab-v3-wave6b2-project-path-plan/{progress,minors-rollup,final-review-fable}.md`.

**Research behind this plan** (`docs/superpowers/research/6b/`, verified at `e03bc48f`; every line number below was re-verified at `20c7c1ef`):
- `2026-10-03-6b2-grid-geometry.md`: the composed map, drizzle, output (§3–§5, §7 steps 8 and 11);
- `2026-10-03-6b2-ln-coverage.md`: LN seams, coverage, crop, acceptance (§1–§8);
- `2026-10-03-6b2-commands-e2e.md`: the seven commands, the e2e (§1, §3–§6).

## Global Constraints

- **Branch:** `collab-wave6b3`, cut from local `main` at `20c7c1ef`, which carries plan 6b-2. Merge and push only on the owner's word.
- **Commits** are made as the configured user (`eg013ra1n` / `vilen.sharifov@gmail.com`; the repo config already has them — pass no `-c user.*`). Every message ends with exactly these two lines (use your own model name if you are not Sonnet 5.5):
  ```
  Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK
  ```
- **Frame-set behaviour is preserved, and this is checked at every task.**
  - Every existing test passes. An existing assertion may change only where a task names it. The full list:
    - `a_project_start_is_refused_until_6b2` (`run.rs:~21195`), which is deleted, with `PROJECT_STACKING_UNAVAILABLE` (Task 8a);
    - the funnel test's `start_stacking` allowlist entry (`origin.rs:438`), which is removed (Task 8a);
    - any test that pins `INTERMEDIATE_ARTIFACT_KINDS` (Task 5b adds `ln_global`) or the cleanup directory list (Task 8a adds `tmp/`).
  - The one deliberate frame-set behaviour change is D17's: `Intermediates` and `All` cleanup now remove `tmp/` for a frame set too (a directory of crash leftovers with no artifact rows). No written output moves.
  - Mechanical edits are allowed anywhere the compiler demands them: a new field in a struct literal (`StackParams`, `GroupInput` (`coverage_map`, `stats_on_covered`), `GroupOutput`, `RegisteredCards`, `DrizzleInput`, `DrizzleOutput`, `RunContext`, `RunSummary`, `SummaryGroup`, `RegisteredFrameOutcome::Aligned`, `EstimateInputs` (`crop_scratch`); `MeasuredFrame` is NOT touched), a new argument at a call site (`build_reference`, `integrate_planes`, `record_registered_artifact`, `write_one_registered_frame`, `compute_register_stale`).
  - No new `StackingConfig` field and no `STACKING_CONFIG_VERSION` bump: `config_hash_is_canonical_and_pinned` and `stage_hashes_are_pinned_to_golden_literals` keep their literals in every task.
  - Frame-set group keys, every stage hash, frame-set log messages and fields, error strings and every written byte stay identical. The set-204 regression of the acceptance runs (last section) proves it on real data.
- **The funnels.** `origin::tests::subject_and_origin_variants_stay_in_their_funnels` scans the production halves of `run.rs` and `plan.rs`. New code there never names `StackSubject::…` or `FrameOrigin::…`; it calls `subject.project_id()` / `subject.frame_set_id()` or a function in `stacking::origin`, `stacking::paths` or the new `stacking::grid_map` (none of which the test scans).
- **TypeScript:** `src/types/` may change only in **Tasks 2a, 6a, 6b and 8b**, and each lists exactly what moves. Every other task ends with `git diff --exit-code src/types`. An exported field added by this plan carries a `//` comment, never `///` (ts-rs copies `///` into the generated files).
- **The headless build stays green:** `cargo check -p athenaeum-core --no-default-features`. `geometry::*` is ungated: the new `DisplacementField` trait names no solver or render type. `stacking::grid_map` lives under the existing `all(render, solver)` gate of `stacking`.
- **The replica is sacred.** No code path writes, renames or deletes a landed file. No file a stacking run writes carries `ATH_PRJ`, `ATH_FILT`, `CALSTAT` or `ATH_CSRC`. Cleanup removes directories under the working layout, and rows; never a file by an artifact's path.
- **Variable-length FITS cards** (the 6b-2 acceptance found an `ATH_REGR` overflow, fixed in `4779ce51`): a card whose value is caller text never carries a comment unless the whole card fits 80 columns. This plan's new ones: `FILTER` (canonical name, no comment), `ATH_SPRJ` (project id, comment only when it fits), `ATH_GRID` (16 hex, fixed). `ATH_STKG`/`ATH_STKF`/`ATH_STKC` keep their no-comment rule.
- **Errors and logging:**
  - Never swallow an error; log at `warn!` or `error!` before degrading.
  - `tracing` only, short stable message, snake_case fields. Zero `println!` outside tests.
  - New field names (`overlap`, `crop_x0`, `crop_y0`, `crop_width`, `crop_height`) go into the logging dictionary in Task 11. Each task names its own.
- **Build discipline (this Mac):**
  - `CARGO_BUILD_JOBS=4` and `--test-threads=4`, one compile at a time.
  - Format touched files with `rustfmt <files>`, never `cargo fmt -p`, and revert sibling-module reformatting rustfmt drags in.
  - **`stacking/run.rs` is not rustfmt-clean upstream.** Never run rustfmt on it: format your new hunks by hand to rustfmt style (or rustfmt a scratch copy and port only your hunks). Review packages use `git diff --diff-algorithm=histogram`.
  - `cargo check -p athenaeum-core --lib` stays warning-free. Clippy is not a gate.
- **Two backends in sync.** Task 8b lands the seven commands in core; Task 8c puts them on both hosts in one commit.
- **Gate per task.** Each task leaves all of these green:
  ```bash
  CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
  CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --lib 2>&1 | grep -E '^warning|^error' ; echo "lib check done"
  CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
  CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
  CARGO_BUILD_JOBS=4 cargo check -p athenaeum-tauri
  TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract && git diff --stat src/types
  npx tsc --noEmit -p .
  ```
  - The lib check prints no `warning` (the workspace note "profiles for the non root package will be ignored" is not a code warning).
  - The headless check keeps exactly its two pre-existing dead-code warnings (`exchange_meter`, `sharing/iroh/node.rs`); a task must not add a third.
  - `git diff --stat src/types` is empty except in Tasks 2a, 6a, 6b and 8b.
  - Known flakes under full-suite load: the wall-clock collab live tests (`edited_touched_and_deleted_replicas`, `a_publishing_switch_returns_at_once_while_the_reannounce_retries`, `unmapped_filter_then_mapped_and_an_attested_set`), `landing_free_bytes_is_bounded_by_the_collaboration_root`, `concurrent_gets_for_an_unreachable_provider_share_one_dial`, `local_normalization_sidecars_are_cached_on_the_second_run`. If one fails, re-run it alone and report both runs.
- **Tests that build displacement grids** (a spline's or the new sampled layer's) hold `crate::geometry::pixel_map::grid_counters::exclusive()` for their whole body (the run pin that measures resident grids depends on it). Every project run test builds them (a composed map is a sampled layer), so each one in this plan opens with `let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();`. The helpers (`seed_sky_project`, `run_sky_project`, `project_rc`) never take the lock themselves: the test holds it, so a helper can never deadlock against its caller.
- **Copy rules:** comments, docs and commit messages are in English. Never name a third-party project or program anywhere in the repo.

## Review Focus

1. **Frame-set stacking does not move.** Masters, registration rows, `.athln` sidecars, calibrated frames, cards and logs of a frame set are byte-identical.
   Pinned in:
   - Task 1: `TpsGrid::build` is `sample` over the spline (`the_tps_grid_agrees_with_the_exact_spline` unchanged);
   - Task 2a: a frame set's registered hash is its registration hash;
   - Task 4: the engine's coverage pass is off by default (`coverage_off_leaves_the_stack_bit_identical`);
   - Task 6a: `a_frame_sets_master_is_bit_identical_with_the_coverage_map_on_and_off`;
   - Task 6b: `drizzle_output_is_bit_identical_with_coverage_on_and_off`;
   - Task 6a: a frame set's statistics are untouched (`stats_on_covered` is `false` for a set);
   - Task 7b1: `a_full_rectangle_crop_writes_what_the_master_writer_writes` (review I7);
   - Task 7b2: `a_zero_crop_keeps_todays_output_path`, `a_full_rectangle_crop_writes_what_the_zero_crop_path_writes` (review I7);
   - every task's full core gate, and the acceptance runs' set-204 real-data regression against `main`.
2. **The composed map.** One `PixelMap` meets the exact chain within 0.01 px in both directions, for an identity, a homography, a polynomial and a spline star map, at 0° and 4° from the tangent point, with the owner's parity (det CD > 0) mirrored onto the grid's (det CD < 0). It is never serialized.
   Pinned in Task 1 (`the_composed_map_meets_the_chain_within_a_hundredth`), Task 2b1 (`a_sky_frame_puts_its_stars_where_its_wcs_says`, the fixture), Task 2b2 (`a_project_frame_registers_onto_the_grid`).
3. **Caches.** The star map survives `Intermediates`; a grid change re-composes and rewrites with no star detection; the plan and the run agree on "Register cached" and on the LN hash, including when the LN field moves (review I6: the plan twins the field).
   Pinned in Task 2a (`the_star_map_cache_survives_intermediates_cleanup`), Task 2b2 (`a_grid_change_recomposes_without_star_detection`, `a_project_plan_reports_register_cached_then_stale_on_a_grid_change`), Task 5b (the field twin at the end of `a_member_under_a_quarter_overlap_keeps_global_normalization`).
4. **LN on the overlap is project-only numerics.** Legacy LN is untouched for frame sets (every LN pin). Masked LN recovers the true scale on a half-covered member where the legacy path is biased; a member under 25 % keeps global normalization with no sidecar and a cache marker.
   Pinned in Task 5a (`masked_ln_recovers_a_half_covered_members_scale`, asserted on the eroded overlap only, review I2), Task 5b (`a_member_under_a_quarter_overlap_keeps_global_normalization`, which also registers the east pair's strip before the e2e does, review I5; `a_wide_grid_keeps_the_coverage_filter_on_the_groups_own_field`, `the_ln_field_forgives_one_lost_probe`, review I6).
5. **Coverage, statistics and crop.** Counts equal footprints; a grid master's statistics come from covered pixels only (review I1); a crop never loses a master; one rectangle per co-registered run, one per group in native; CRPIX shifts exactly; a full-rectangle finalization is the K = 0 writer's output (review I7).
   Pinned in Tasks 4, 7a, 6a (`a_grid_masters_statistics_ignore_uncovered_pixels`), 6b, 7b1, 7b2.
6. **Cards.** No written file carries `ATH_PRJ`. A grid output carries `ROWORDER = 'BOTTOM-UP'`, the canonical `FILTER`, `ATH_SPRJ`, `ATH_GRID`, `ATH_RGEO = 'grid'`, and no `PLTSOLVD`. A long project id or canonical filter never fails a write.
   Pinned in Task 3 (`a_project_run_writes_grid_masters_with_the_grid_wcs`, `no_file_a_project_run_writes_carries_ath_prj`, `grid_cards_survive_long_values`).
7. **Commands.** Seven on both hosts (306), `get_project_stacking_config` writes nothing, cleanup sweeps rows of frames that left the project, a steady-state plan takes no write lock.
   Pinned in Tasks 8a, 8b and 8c.
8. **The e2e** (Task 9) runs the whole path through the fake hub.
9. **Run A cannot measure the wrong thing silently** (reviews I3, I4): the procedure refuses without set 204's stacking config, prints and checks both references, and reads no row before the WCS probe passes; `--pairs` refuses differing pixel grids.

## Plan rulings (the controller's skeleton)

Numbered D1–D23. Each gives the decision and the evidence. Where a ruling departs from the spec, research or memory, it is repeated in "Writer's notes for the controller" at the end.

- **D1 — The composed map is a sampled displacement layer, not polynomial order 5.**
  - Decision: `DistortionModel::Sampled { field: Arc<dyn DisplacementField>, domain, grids }`. The field's exact path evaluates the chain; its pixel path reads bilinear `TpsGrid`s sampled at `TPS_GRID_PX` (8 px), built lazily and released exactly like a spline's (`TpsGrids`, ruling R-T4-6a). The inverse is Newton steps on the forward chain with the affine's Jacobian, never SIP's reverse tables or the star map's separately fitted inverse.
  - Evidence:
    - Research §3.6 (real SIP of frame 28747): affine + TPS at the 600-node cap misses the chain by 0.08 px; polynomial order 5 passes (0.0035 px) but only for an identity star map, and it needs the hard order cap 2..=4 raised in `Polynomial2D::{is_well_formed, fit, fit_with_affine, eval}` (`geometry/polynomial.rs:40, 54-55, 81, 129`), which every star registration uses.
    - A polynomial cannot follow a star map that carries its own spline (`registration.distortion = tps`, `DistortionChoice::Tps`, `register/mod.rs:128`), and its margin on a polynomial star map is unmeasured.
    - Bilinear sampling error on a smooth field is `h²/8 · |d''|`: for a SIP cubic of 11 px at u = 3124 that is ≈ 5·10⁻⁵ px at h = 8 (research §3.6). It does not depend on the star map's kind or on the offset from the tangent point.
    - The `TpsGrid`/`TpsGrids` machinery already carries the cache, the release and the test counters; drizzle already takes its `Tiled` arm for any distortion layer (`drizzle/mod.rs:450`).
  - The geometry module stays ungated: the trait names no WCS type. The chain (`ComposedField`) lives in the gated `stacking::grid_map`.
  - Never serialized: the variant is `#[serde(skip)]`. `to_record` serializes the star alignment's map before composition (`register/frame.rs:477`), and the composed map is re-derived from (star map, reference WCS, grid) whenever a cached star map is reused.
  - No per-frame runtime self-check against 0.01 px (it would build both grids per frame): the bound is pinned in tests; the run checks only that the inverse chain converges at the corners and the centre, and a frame that fails is excluded "grid composition failed: …".
  - **Refinement (review M1, M2):** `TpsGrid::sample`'s doc states its real arithmetic (two samples past `ceil(extent / step)` on the far side); `ComposedField`'s doc names the one edge cell of a spline star map, where the domain clamp leaves up to ≈ 0.012 px of bilinear error that no consumer reads (NaN outside the frame). The review also notes the degree argument: an order-3 star map composed with an order-3 SIP is degree 9, so no order-5 polynomial follows ANY star map with a distortion layer.
- **D2 — The star map is a payload-only `registration` artifact.** One row per handle, `group_key = ""`, `config_hash` = today's registration hash, `payload_json` = the `RegistrationRecord` (with `frames_set_id = 0`). `stored_registrations` / `store_registration` read and write it for a project; it stays out of `INTERMEDIATE_ARTIFACT_KINDS`, so only `CleanupWhat::All` drops it (research finding 5: cleanup erases the `registered` rows the spec meant to hold the star map). The `registered` artifact's hash is `grid_registered_hash(registration hash, reference WCS hash, grid hash)`, folding `COMPOSE_VERSION`. The reference's WCS is hashed because a local re-solve changes it independently of content.
- **D3 — Grid outputs state `ROWORDER = 'BOTTOM-UP'`.** The owner ruling forbids copying the reference's `ROWORDER` onto a grid master. A grid is sky-standard (north up when row 0 is at the bottom), so registered frames, masters, coverage and drizzle outputs on the grid write `BOTTOM-UP` explicitly: `RegisteredCards::reference_roworder` already replaces the card (`register/writer.rs:44-55, 102-108`), and the master gets the same through `stamp_grid_cards`. A missing card would read the same (`orientation.rs:93`), but the XISF writer states the effective order, so explicit is clearer. An XISF grid master therefore fires the existing "XISF keeps the bottom-up row order" warning, which is true. **Refinement (review M3):** the registered frame's `ROWORDER` comment reads "row order of the output grid", true for both kinds of grid; production passes `reference_roworder: None` for every frame set (`run.rs:6892`), so no frame-set byte moves.
- **D4 — The coverage count is a post-pass, not a change to the combine loop.** When asked, each band counts per pixel the frames whose raw sample is finite (geometric coverage before range and algorithm rejection), from the band planes already in memory, after the band's combine. With `empty_as_nan` a pixel whose count is 0 becomes NaN, and the engine's non-finite guard (`engine.rs:1576-1580`) skips exactly those pixels. The per-pixel loop (`engine.rs:1196-1416`) is textually untouched, so no timing bracket is owed. Research fact 1 is honoured: "empty" keys on the count, never on `work.is_empty()` (an all-range-rejected pixel stays 0.0).
  - **Refinement (review I1):** on a grid, every master's statistics (`GroupStats`: location, scale, noise, PSF SNR, SNR gain, FWHM, eccentricity) are measured over covered pixels only — the largest rectangle every pixel of which at least one frame covers (`crop::largest_covered_rect`, Task 7a) — for every grid master, coverage map or not. `GroupInput.stats_on_covered` (`rc.grid.is_some()`) implies the plane-0 count; the count taken only for the statistics is never written as a map. Frame sets are unchanged (`false`). Pinned by `a_grid_masters_statistics_ignore_uncovered_pixels` (Task 6a).
- **D5 — Seam 1 lands with seam 2.** Task 4 adds the `empty_as_nan` option with every caller passing `false`. Task 5a switches it on for a project's LN reference in the same commit that lands masked LN (research fact 2: NaN alone makes reference-side detection worse).
- **D6 — Masked LN is carried by `LnReferenceForDetection`.**
  - `LnOverlap::{Legacy, Masked}` is a field of `LnReferenceForDetection` (built once per group), so `normalize_frame` and the seeds calibration follow it with no signature change.
  - Masked mode, per channel:
    - detection planes fill non-overlap pixels with 0.0, which the fast detector ignores (`rustafits/src/analysis/adaptive_detection.rs:~66, ~130`: values ≤ 0 skip the histogram and mean, exact 0 skips the RMS);
    - seeds and fits are kept only inside the overlap eroded by 48 px, the PSF fit region's hard cap (`psf_signal.rs:380`, `.min(48)`);
    - the target background and a per-member reference background are both computed on the overlap.
  - The overlap fraction is measured on channel 0; under 25 % the frame returns `LnError::LowOverlap`.
  - Frame sets build `Legacy` and change nothing (every LN pin). A project's LN hashes fold the token `lnmask:1` through `plan::ln_registration_upstream`, never the frame-set formula.
  - **Refinement (review I2):** the masked fit defines the model on the overlap only. `masked_ln_recovers_a_half_covered_members_scale` asserts the additive term on the grid nodes inside the overlap eroded by the 48 px margin, and `normalize_frame`'s masked branch says what a node outside carries (`background_grid`'s extrapolation into masked cells), so nobody "fixes" it.
- **D7 — The < 25 % fallback.** The member keeps its global pair (`GroupInput.ln[i] = None`; the engine already handles it, `engine.rs:~950-976`), gets no sidecar, and a payload-only `ln_global` marker (hash = its LN hash) so the run and the plan count it as cached. One `warn!` and one run warning per group name the count. `TooFewMatches` keeps today's policy (excluded when LN drives output); only the spec's < 25 % rule falls back. `ln_global` joins `INTERMEDIATE_ARTIFACT_KINDS`.
- **D8 — Grid-mode admissibility uses the group's median footprint.** On a grid, the anchor pick and the LN member list measure each member against the cells at least ⌈n/2⌉ of the group's members cover (32 × 32 probes), not against `[0, grid_w) × [0, grid_h)`. Research finding 7: on a wide grid almost no member reaches `MIN_REFERENCE_COVERAGE = 0.97` of the grid, so the filter would drop for every group. The run reference's own footprint would fail a group shot at another field. Frame sets keep `reference_coverage` unchanged.
  - **Refinement (review I6):** the field is built from the group's IN-SCOPE frames' WCS footprints (manifest WCS, else the cached solve of the current content; `origin::ln_footprint`) on a lattice in FRAME units — 32 probes per median frame side measured on the grid (`weights::LN_FIELD_PROBES_PER_FRAME_SIDE`), so a wide grid or a second rig never coarsens it — and a member's coverage forgives one lost probe. One function, `origin::group_ln_field`, builds it for the run and for the plan (the twin); its hash, the frames that pass the 0.97 rule against it, folds into every member's LN hash (`plan::ln_upstream_on_field`), so a field change re-keys the group's LN in the plan and the run at once, never a run later. A frame with no WCS footprint never qualifies as an LN reference member or the anchor (it is still stacked).
- **D9 — Coverage outputs.**
  - Master: `<stem>_coverage.fits`, plane 0's count, always FITS, `BUNIT = 'frames'`, the master's WCS; kind `coverage`.
  - Drizzle: `<drizzle stem>_coverage.fits`, one plane per channel, counting frames whose deposited weight raised `W`. It is computed by a per-band snapshot of `W` around each frame's deposit, so `deposit_band`/`deposit_cell` do not change. A drop below `W`'s f32 resolution is not counted (documented). Kind `drizzle_coverage` (the `master_lights` unique key forbids reusing `coverage`).
  - `SummaryGroup.coverage_path` / `drizzle_coverage_path`. The drizzle memory estimate gains the count planes.
- **D10 — Crop is a deferred, opt-in pass.** With `cropToCoverage = K > 0`, every output of every group is written as FITS into `runs/run-<id>-uncropped/`, and a `CropItem` keeps the cards, WCS and stats. After the last group, `finish_crop` computes the rectangles and finalizes each group:
  - co-registered: one run-wide rectangle, the union of the groups' `coverage ≥ K` boxes;
  - native: one rectangle per group.
  - Finalizing writes the final files in the configured format through the same writers, refreshes `master_lights` rows (their upsert key is `(run, group, kind)`), group paths and summary paths, then deletes the scratch.
  - A group whose crop fails is finalized uncropped, with a warning; a master is never lost.
  - `K = 0` keeps today's code path literally.
  - Research facts 6/§4: XISF masters cannot be re-read, and the union is known only after the last group.
  - **Refinement (review I7, M6, M13):** a full-rectangle finalization is pinned against the K = 0 writer (Task 7b1's unit test, Task 7b2's run test: same pixels, same keywords but the run id); every fallback warning says the outputs were "written uncropped"; the plan's space estimate counts the scratch copies (`EstimateInputs.crop_scratch`); the drizzle WCS rescale repeats the first write's `scale_plate_solve` on the same record, documented as unable to fail there.
- **D11 — Output on the grid.**
  - The WCS is `grid_plate_solve`: `crpix` 0-based `((W−1)/2, (H−1)/2)` (spec §5.9 says "1-based"; `PlateSolveRecord.crpix` is 0-based and `wcs_cards` adds 1, `fits_writer/wcs.rs:206`), `catalog_used = algorithm_used = "grid"`, no SIP.
  - `stamp_grid_cards` sets the canonical `FILTER`, `ROWORDER = 'BOTTOM-UP'`, `ATH_RGEO = 'grid'`, `ATH_SPRJ`, `ATH_GRID`, and removes `PLTSOLVD` (a grid TAN is not a plate solve). It runs on the master's cards and again on the drizzle cards, because `build_drizzle_cards` re-stamps `ATH_RGEO` and the WCS block.
  - Masters land in `<output>/<sanitize_for_filename(title)>/`; names keep `master_file_name` (title, canonical filter, no binning token since a project group's binning is 1).
  - **Refinement (review M4):** an empty canonical filter writes no `FILTER` at all (the reference's raw card must not leak), said in `stamp_grid_cards`' doc, with one run warning per such group.
- **D12 — The seven commands.**
  - `get_project_stacking_config` is read-only: with no stored document it returns the seed it would store, `isDefault: true`.
  - `set_project_stacking_config` refuses a project this device left (`live_project`), stores sorted, de-duplicated exclusions, and an absent `referenceFrameUuid` clears the manual pin (Tauri cannot tell absent from null).
  - `cleanup_project_stacking_work` heals and refuses while a run is active (`Conflict`), then sweeps the artifact rows of handles whose `(project, uuid)` left `project_frames_local` (one `DELETE`, every level, before the "no working folder" return), then cleans the layout. **Refinement (review M5):** the sweep moved after the `Conflict` check, so a refused cleanup touches nothing.
  - Plan, start, usage and cleanup run under `spawn_blocking` on both hosts.
- **D13 — `start_stacking` starts a project.** The guard and `PROJECT_STACKING_UNAVAILABLE` go. A project run's `RunContext.grid` is `plan.grid.current` (the plan's `grid` blocker guarantees it), its `output_dir` is `<output>/<title>`, and `RunSummary.grid` records the grid. `stage_calibrate`'s project branch keeps its own message. A crate-internal `start_stacking_with_solver` is the test seam the e2e needs (spec §15.2: synthetic stars cannot be solved against a catalog).
- **D14 — The plan learns the WCS rule.** A `reference` blocker when the manual pin has no manifest WCS and its cached local solve failed or was refused, or when no in-scope frame has a manifest WCS and every in-scope frame's cached solve failed or was refused. A warning when the pin has no manifest WCS and no cached solve yet ("will be plate-solved; the run fails at Reference if it cannot be"). "Failed" is judged as the run's Solve sub-step judges it: the plan computes the installed solver's identity (`solve::installed_solver_identity`, the string `CatalogSolver` uses) and the same `solve_hash`, so a failure cached under another catalog, solver config or app version is retried by the run and blocks nothing. With no catalog installed, a frame without a manifest WCS or a cached solve can get none, and counts as unsolvable.
- **D15 — One plane rule for stage 1 and LN.** `plan::stage_one_planes(cfg, group, prepare_path)` replaces both `run::expected_stage_one_planes` and `plan::expected_ln_channels`. The plan passes `prepare_path = project || attested`, so an `osc__` Prepare group with the debayer off counts three LN channels, as the run writes.
- **D16 — A steady-state plan writes nothing.** Verified at HEAD: the only per-plan writes are the first plan's config seed (`plan.rs:1080-1089`, read first) and the mint of missing handles (`db/stacking.rs:1048-1100`, read first). Pinned by a test that runs a second plan while another connection holds `BEGIN IMMEDIATE` with `busy_timeout(0)`.
- **D17 — `tmp/` is swept** by `Intermediates` and `All`, for every subject (6b-2 final review M3). It holds no artifact rows, only crash leftovers (the solve's ADU copy, Calibrate's scratch); cleanup refuses while the subject has an active run. **Refinement (review, writer's note 3):** this IS a behaviour change for frame sets — a frame set's `Intermediates` cleanup now removes a directory it did not before — stated in Global Constraints; no written output moves. The 6b-2 open-items bullet it closes becomes one line of the 6b-3 entry.
- **D18 — Wire additions for 6c.** `RunSummary.grid` (the grid a project run used), `StackingRunFrameRow.frameUuid` (a project row's uuid, read through `stack_project_frames`, so run history maps a handle after the frame left the plan), `SummaryGroup.coveragePath` / `drizzleCoveragePath`, `MasterLightKind` `coverage` / `drizzleCoverage`, `StackingProjectConfig`.
- **D19 — The 25 % threshold is covered synthetically.** LDN1272-test's two rigs are two groups whose members share their framing, so nothing there falls under 25 % (open-items 6b-2). The threshold is pinned by `normalize_frame` unit tests, a run-level project test and the fake-hub e2e; acceptance run B reports each member's overlap; re-measuring it on real partial overlap needs the owner's set 195 published into a test project (open-items). **Refinement (review I5):** the east-pair geometry (offset, star spacing, seed) is one set of constants in `test_fixtures` that Task 5b's run test and Task 9's e2e share, so the strip registration is tried first in the cheaper test, which also asserts that the pair registered.
- **D20 — No hub change, no new event.** Grid moves picked up by `refresh_projects` still emit nothing (6b-2 note; 6c re-reads on a card refresh). The Measure progress restart after Solve stays (a 6c note). `get_collab_grid_proposal.framesInside` (vs the proposal) and `PlanGrid.framesInside` (vs current-or-proposal) are documented for 6c.
- **D21 — The acceptance drivers are code, the runs are not.** Task 10 adds `tier1/project-run.sh`, `tier1-compare.py --pairs`, `grid-report.py` and `run-a-grid.py` to the harness (reuse, never rebuild). The runs themselves are the last section, for the controller's own agent.
- **D22 — Runtime composition cost.** A cached star map is re-composed on the run thread (an affine fit over ≈ 25 k chain samples, tens of ms per frame); the grids build lazily in the consumers' own fan-outs. A spline star map makes each grid ≈ 1–2 s per frame (opt-in setting).
- **D23 — Run B's "noise per arcsec²" is σ_px · scale.** The research left the term undefined (6b-2 LN/coverage research §6). Master pixels are intensities (a mean of normalized frames, resampled intensity-preserving), so the mean over A arcsec² of white noise has σ = σ_px · scale / √A; at A = 1 it is σ_px · scale (value · arcsec). It compares grids of different scales; σ_px is printed beside it. Measured on the master cropped to the group's `coverage ≥ ⌈n/2⌉` rectangle (D8's median footprint), because `measure_probe` has no mask and the uncovered 0.0 pixels would enter its statistics. **Refinement (review caveat on note 7):** it is a WHITE-NOISE EQUIVALENT — resampling, and more so drizzle, correlates a stack's pixels, so it understates the true per-area noise by a scale-dependent factor; the report's header says so, beside σ_px.

## File map

| File | Change |
| ---- | ---- |
| `crates/athenaeum-core/src/geometry/pixel_map.rs` | `DisplacementField`, `DistortionModel::Sampled`, `TpsGrid::sample` (T1) |
| `crates/athenaeum-core/src/stacking/grid_map.rs` | **new** — `GridRun`, `ReferenceWcs`, `ComposedField`, `compose_to_grid`, hashes, `grid_plate_solve`, `crop_plate_solve`, `subject_dims` (T1); `reference_wcs_hash_of` (T2a) |
| `crates/athenaeum-core/src/stacking/mod.rs` | `pub mod grid_map;` (T1), `pub mod crop;` (T7a) |
| `crates/athenaeum-core/src/stacking/plan.rs` | project arms of `stored_registrations`/`store_registration`, `registered_hash_for`, grid-aware `compute_register_stale` and LN upstream (T2a); `ln_registration_upstream` token (T5a); `ln_global` cache rule, `stage_one_planes`, `ln_upstream_on_field` and the per-group field hash (T5b); `EstimateInputs.crop_scratch` (T7b1); WCS-rule blockers, `PROJECT_STACKING_UNAVAILABLE` removed (T8a); `read_project_config` (T8b) |
| `crates/athenaeum-core/src/stacking/run.rs` | `RunContext.grid`, `delivered_dims`, `Aligned.registered_hash`, `record_registered_artifact(hash)` (T2a); test helpers and the first grid test (T2b1); composition in Register (T2b2); grid WCS + `stamp_grid_cards` in Output and Drizzle (T3); masked LN switch + `LowOverlap` handling + the LN field (T5a, T5b); coverage file, `stats_on_covered` + drizzle coverage (T6a, T6b); crop deferral + `finish_crop` (T7b2); start guard lifted, `start_stacking_with_solver`, project output folder (T8a) |
| `crates/athenaeum-core/src/stacking/register/writer.rs` | `RegisteredCards.grid_hash` → `ATH_GRID`; the `ROWORDER` comment "row order of the output grid" (T2a, review M3) |
| `crates/athenaeum-core/src/stacking/master_cards.rs` | `stamp_grid_cards`, `card_with_optional_comment` (T3); `coverage_map_cards`, coverage in `write_master_light` (T6a); drizzle coverage in `write_drizzled_master` (T6b); `write_cropped_master`, `write_cropped_drizzle` (T7b1) |
| `crates/athenaeum-core/src/stacking/paths.rs` | `subject_output_dir` (T3); `ln_global` in `INTERMEDIATE_ARTIFACT_KINDS` (T5b); `uncropped_dir`, `EstimateInputs.crop_scratch` (T7b1); `tmp_root`, `tmp/` in cleanup (T8a) |
| `crates/athenaeum-core/src/integration/engine.rs` | `StackParams.{coverage, empty_as_nan}`, `StackOutput.coverage`, the post-pass (T4) |
| `crates/athenaeum-core/src/stacking/integrate.rs` | `Coverage` option on `integrate_planes`, `GroupInput.coverage_map`, `GroupOutput.coverage` (T4); `GroupInput.stats_on_covered`, statistics on the covered rectangle (T6a) |
| `crates/athenaeum-core/src/stacking/ln/{mod,reference,scale}.rs` | `build_reference(empty_as_nan)` (T4); `LnOverlap`, masked detection/background/seeds, `LnError::LowOverlap`, `eroded_mask` (T5a) |
| `crates/athenaeum-core/src/stacking/weights.rs` | `LN_FIELD_PROBES_PER_FRAME_SIDE`, `LnField`, `ln_field` (T5b) |
| `crates/athenaeum-core/src/stacking/drizzle/mod.rs` | `DrizzleInput.write_coverage`, `DrizzleOutput.coverage`, the W snapshot, the memory term (T6b) |
| `crates/athenaeum-core/src/stacking/crop.rs` | **new** — rectangle, `largest_covered_rect`, union, plane and file crop, `restate_crpix` (T7a) |
| `crates/athenaeum-core/src/stacking/provenance.rs` | `RunSummary.grid` (T2a); `SummaryGroup.coverage_path` (T6a), `drizzle_coverage_path` (T6b) |
| `crates/athenaeum-core/src/api/stacking.rs` | `MasterLightKind::{Coverage, DrizzleCoverage}` (T6a, T6b); seven project functions, `StackingProjectConfig` (T8b) |
| `crates/athenaeum-core/src/db/stacking.rs` | `StackingRunFrameRow.frame_uuid`, `sweep_departed_project_artifacts` (T8b) |
| `crates/athenaeum-core/src/stacking/origin.rs` | `ln_footprint`, `GroupLnField`, `group_ln_field` (T5b); `WcsOutlook`, `wcs_outlook`, `reference_wcs_check`; funnel allowlist: `start_stacking` entry removed (T8a) |
| `crates/athenaeum-core/src/stacking/solve.rs` | ignored real-data probe `real_data_grid_master_wcs_probe` (T3); `solver_identity`, `installed_solver_identity` (T8a) |
| `crates/athenaeum-core/src/stacking/test_fixtures.rs` | sky-field project frames, the east-pair constants, a fixture test (T2b1) |
| `crates/athenaeum-core/src/stacking/ln/reference.rs`, `examples/{integrate,ln}_probe.rs` | `build_reference(empty_as_nan)` and `coverage_map` literals (T4); `stats_on_covered` literals (T6a) |
| `crates/athenaeum-core/src/api/stacking_project_e2e.rs` | **new**, `#[cfg(test)]` — the fake-hub e2e (T9) |
| `crates/athenaeum-core/src/api/mod.rs` | `mod stacking_project_e2e;` (T9) |
| `crates/athenaeum-core/src/ts_export.rs` | `StackingProjectConfig` (T8b) |
| `crates/athenaeum-tauri/src/commands/stacking.rs`, `lib.rs` | seven wrappers, registration (T8c) |
| `crates/athenaeum-web/src/routes/stacking.rs`, `routes/mod.rs` | seven routes, registration, route tests (T8c) |
| `src/types/stacking.ts` | regenerated (T2a, T6a, T6b, T8b) |
| `docs/superpowers/research/scripts/acceptance/{README.md,tier1/project-run.sh,tier1/tier1-compare.py,grid-report.py,run-a-grid.py}` | the drivers (T10) |
| `docs/stacking/README.md`, `docs/transfers/README.md`, `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, `docs/superpowers/open-items.md`, `CLAUDE.md`, the spec's §9.1/§18 count note | T11 |

---

### Task 0: Baseline

**Files:** none.

- [ ] **Step 1: Confirm the branch and a clean tree**

Run: `git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum status -sb && git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum log -1 --format=%h`
Expected: `## collab-wave6b3` and `20c7c1ef`, or the controller's plan commit on top of it. No code changes.

- [ ] **Step 2: Record the baseline gates.** Run the Gate per task commands above, one at a time, from the repo root. Expected: all green, and `git diff --stat src/types` empty. Record every `test result` line (the 6b-2 final gate read core lib 3540 passed / 0 failed / 8 ignored); later tasks compare against them. A test red here is pre-existing: note its name and do not fix it in this plan.

- [ ] **Step 3: Record the strict command count.** Run:
```bash
grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l
```
Expected: `299`.

---

### Task 1: The composed map — a sampled distortion layer and `stacking::grid_map` (D1, D11; spec §5.5, §5.9)

**Files:**
- Modify `crates/athenaeum-core/src/geometry/pixel_map.rs`:
  - add the `DisplacementField` trait after `impl InverseMap for Linear` (`~36-43`);
  - add `TpsGrid::sample` and make `TpsGrid::build` call it (`~87-118`);
  - add the `DistortionModel::Sampled` variant (`~397-421`), its constructor, and its arm in `EvalLayer::of` (`~299-315`), `PartialEq` (`~424-445`), `is_well_formed`, `label`, `forward_grid`, `inverse_grid`, `release_grids`, `forward_displacement`, `inverse_displacement`, `forward_displacement_exact`, `inverse_displacement_exact`, `grids_built`, `grid_cache` (`~494-693`);
  - `PixelMap::to_json`'s doc (`~840`);
  - one test.
- Modify `crates/athenaeum-core/src/geometry/mod.rs`: re-export `DisplacementField`.
- Create `crates/athenaeum-core/src/stacking/grid_map.rs`.
- Modify `crates/athenaeum-core/src/stacking/mod.rs`: `pub mod grid_map;` beside the other modules.

**Interfaces:**
```rust
// geometry/pixel_map.rs (ungated)
pub trait DisplacementField: Send + Sync + std::fmt::Debug {
    fn forward(&self, x: f64, y: f64) -> (f64, f64);
    fn inverse(&self, x: f64, y: f64) -> (f64, f64);
    fn key(&self) -> &str;
}
impl TpsGrid { pub fn sample(domain: [f64; 4], f: impl Fn(f64, f64) -> (f64, f64) + Sync) -> TpsGrid; }
impl DistortionModel { pub fn sampled(field: Arc<dyn DisplacementField>, domain: [f64; 4]) -> DistortionModel; }
// stacking/grid_map.rs (gated with `stacking`)
pub const COMPOSE_SAMPLE_PX: usize = 32;
pub const COMPOSE_VERSION: u32 = 1;
#[derive(Debug, Clone)] pub struct GridRun { pub grid: CanonicalGrid, pub hash: String, pub tan: WcsSolution }
impl GridRun { pub fn new(grid: CanonicalGrid) -> GridRun; pub fn dims(&self) -> (usize, usize); }
#[derive(Debug, Clone)] pub struct ReferenceWcs { pub solution: WcsSolution, pub hash: String }
impl ReferenceWcs { pub fn from_record(record: &PlateSolveRecord) -> Option<ReferenceWcs>; }
pub fn wcs_record_hash(record: &PlateSolveRecord) -> String;
pub fn compose_to_grid(star: &PixelMap, subject_dims: (usize, usize), reference: &ReferenceWcs, grid: &GridRun) -> Result<PixelMap, String>;
pub fn grid_registered_hash(registration_hash: &str, reference_wcs_hash: &str, grid_hash: &str) -> String;
pub fn grid_plate_solve(grid: &CanonicalGrid, solved_at: &str) -> PlateSolveRecord;
pub fn crop_plate_solve(record: &PlateSolveRecord, x0: usize, y0: usize) -> PlateSolveRecord;
pub fn subject_dims(width: i64, height: i64, path: &Path) -> Result<(usize, usize), String>;
```
Consumed by: Task 2a (`GridRun`, `ReferenceWcs`, `grid_registered_hash`, `wcs_record_hash`), Task 2b2 (`compose_to_grid`, `subject_dims`), Task 3 (`grid_plate_solve`), Tasks 7a, 7b1 and 7b2 (`crop_plate_solve`).

- [ ] **Step 1: Write the failing tests**

`geometry/pixel_map.rs`, `mod tests`, after `the_tps_grid_agrees_with_the_exact_spline`:
```rust
    /// Plan 6b-3 D1: a sampled layer reads its field exactly on the exact
    /// path (no grid built) and through lazy bilinear grids on the pixel
    /// path, each direction's grid built only by its own direction; it is
    /// equal by key and domain, releasable, and never serialized.
    #[test]
    fn a_sampled_layer_reads_its_field_through_lazy_grids() {
        let _quiet = grid_counters::exclusive();
        #[derive(Debug)]
        struct Wobble;
        impl DisplacementField for Wobble {
            fn forward(&self, x: f64, y: f64) -> (f64, f64) {
                wobble(x, y)
            }
            fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
                let (a, b) = wobble(x, y);
                (-a, -b)
            }
            fn key(&self) -> &str {
                "wobble"
            }
        }
        let model = DistortionModel::sampled(Arc::new(Wobble), [0.0, 0.0, TW, TH]);
        let map = PixelMap::with_distortion_model(Linear::identity(), model).unwrap();
        let layer = map.distortion.as_ref().unwrap();
        assert!(layer.is_well_formed());
        assert_eq!(layer.label(), "sampled");
        let _ = map.forward_exact(10.0, 10.0);
        let _ = map.inverse_exact(10.0, 10.0);
        assert_eq!(layer.grids_built(), (false, false), "the exact path builds nothing");
        let mut worst = 0f64;
        for i in 0..40 {
            for j in 0..30 {
                let (x, y) = (i as f64 * TW / 40.0 + 3.3, j as f64 * TH / 30.0 + 2.1);
                let (ax, ay) = map.forward(x, y);
                let (bx, by) = map.forward_exact(x, y);
                worst = worst.max((ax - bx).hypot(ay - by));
            }
        }
        assert!(worst < 1e-3, "pixel path vs exact path: {worst} px");
        assert_eq!(layer.grids_built(), (true, false), "only the forward grid");
        let clone = map.clone();
        assert_eq!(clone, map, "equal by key and domain");
        assert!(
            serde_json::to_string(&map).is_err(),
            "a sampled layer is never serialized"
        );
        assert!(map.release_grids() > 0);
        assert_eq!(layer.grids_built(), (false, false));
    }
```

Create `crates/athenaeum-core/src/stacking/grid_map.rs` with the module (Step 3) and, at its end, these tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::CardValue;
    use crate::geometry::linear::LinearKind;
    use crate::geometry::pixel_map::grid_counters;
    use crate::geometry::{Distortion, Polynomial2D, ThinPlateSpline};

    const W: usize = 1600;
    const H: usize = 1200;
    const SCALE: f64 = 1.87;
    /// Cubic SIP terms: ≈ 10 px of non-affine displacement at this frame's
    /// corners, the size research §3.6 measured on the owner's rigs.
    const K3: f64 = 2.0e-8;

    /// A reference solve in the owner's parity (det CD > 0, every stored
    /// solve), unrotated, with SIP order 3.
    fn reference_record(ra: f64, dec: f64) -> PlateSolveRecord {
        let s = SCALE / 3600.0;
        let a = vec![
            vec![0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, K3],
            vec![0.0, 0.0],
            vec![K3],
        ];
        let b = vec![
            vec![0.0, 0.0, 0.0, K3],
            vec![0.0, 0.0, 0.0],
            vec![0.0, K3],
            vec![0.0],
        ];
        PlateSolveRecord {
            id: None,
            frame_id: 0,
            crpix1: (W as f64 - 1.0) / 2.0,
            crpix2: (H as f64 - 1.0) / 2.0,
            crval1: ra,
            crval2: dec,
            cd1_1: s,
            cd1_2: 0.0,
            cd2_1: 0.0,
            cd2_2: s,
            sip_order: Some(3),
            sip_a_coeffs: Some(serde_json::to_string(&a).unwrap()),
            sip_b_coeffs: Some(serde_json::to_string(&b).unwrap()),
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
            matched_stars: 100,
            total_detected: 200,
            rms_residual_px: 0.1,
            rms_residual_arcsec: 0.2,
            pixel_scale_arcsec: SCALE,
            field_rotation_deg: 0.0,
            solve_time_ms: 1,
            catalog_used: "test".into(),
            algorithm_used: "test".into(),
            solved_at: "2026-10-03T00:00:00Z".into(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }

    fn exact_chain(
        star: &PixelMap,
        reference: &ReferenceWcs,
        grid: &GridRun,
        x: f64,
        y: f64,
    ) -> (f64, f64) {
        let (rx, ry) = star.forward_exact(x, y);
        let (ra, dec) = reference.solution.pixel_to_sky(rx, ry);
        grid.tan.sky_to_pixel(ra, dec)
    }

    /// Composes `star` onto a grid `offset_deg` north of the reference's
    /// centre and checks the PIXEL path (the bilinear grids every consumer
    /// reads) against the exact chain at every 32-px cell centre, both
    /// directions, and the round trip.
    fn assert_pin(star: &PixelMap, offset_deg: f64) {
        let (ra, dec) = (83.8, -5.4);
        let reference = ReferenceWcs::from_record(&reference_record(ra, dec)).unwrap();
        let grid = GridRun::new(CanonicalGrid {
            scale_arcsec: SCALE,
            width_px: 2048,
            height_px: 1600,
            crval1: ra,
            crval2: dec + offset_deg,
            rotation_deg: 0.0,
        });
        let map = compose_to_grid(star, (W, H), &reference, &grid).unwrap();
        assert!(
            map.is_flipped(),
            "a det CD > 0 frame lands mirrored on the sky-standard grid"
        );
        let (mut fwd, mut back, mut round) = (0f64, 0f64, 0f64);
        let mut y = 16.0;
        while y < H as f64 {
            let mut x = 16.0;
            while x < W as f64 {
                let exact = exact_chain(star, &reference, &grid, x, y);
                let (gx, gy) = map.forward(x, y);
                fwd = fwd.max((gx - exact.0).hypot(gy - exact.1));
                let (sx, sy) = map.inverse(exact.0, exact.1);
                back = back.max((sx - x).hypot(sy - y));
                let (rx, ry) = map.inverse(gx, gy);
                round = round.max((rx - x).hypot(ry - y));
                x += 32.0;
            }
            y += 32.0;
        }
        assert!(fwd < 0.01, "forward pixel path vs the chain at {offset_deg}°: {fwd} px");
        assert!(back < 0.01, "inverse pixel path vs the chain at {offset_deg}°: {back} px");
        assert!(round < 0.01, "inverse(forward(s)) vs s at {offset_deg}°: {round} px");
        map.release_grids();
    }

    /// Spec §5.5, plan 6b-3 D1: the composed map meets the exact chain
    /// within 0.01 px in both directions for every kind of star map the
    /// registration can produce, at the tangent point and 4° from it.
    #[test]
    fn the_composed_map_meets_the_chain_within_a_hundredth() {
        let _quiet = grid_counters::exclusive();
        let identity = PixelMap::linear(Linear::identity()).unwrap();
        let similarity = PixelMap::linear(Linear {
            kind: LinearKind::Similarity,
            m: [[0.9998, -0.0175, 12.3], [0.0175, 0.9998, -7.8], [0.0, 0.0, 1.0]],
        })
        .unwrap();
        let homography = PixelMap::linear(Linear {
            kind: LinearKind::Homography,
            m: [[1.0, 0.002, 5.0], [-0.001, 1.0, 3.0], [1e-7, -2e-7, 1.0]],
        })
        .unwrap();
        // A cubic star-map distortion: index 3 is the u³ term of order 3.
        let mut ax = vec![0.0; 7];
        let mut ay = vec![0.0; 7];
        ax[3] = 0.6;
        ay[6] = -0.4;
        let polynomial = PixelMap::with_distortion(
            Linear::identity(),
            Distortion {
                order: 3,
                center: (W as f64 / 2.0, H as f64 / 2.0),
                scale: W as f64 / 2.0,
                domain: None,
                forward: Polynomial2D { order: 3, ax: ax.clone(), ay: ay.clone() },
                inverse: Polynomial2D {
                    order: 3,
                    ax: ax.iter().map(|v| -v).collect(),
                    ay: ay.iter().map(|v| -v).collect(),
                },
            },
        )
        .unwrap();
        let nodes: Vec<(f64, f64)> = (0..6)
            .flat_map(|i| (0..5).map(move |j| (i as f64 * 300.0 + 50.0, j as f64 * 270.0 + 40.0)))
            .collect();
        let dx: Vec<f64> = nodes.iter().map(|(x, y)| 1.2 * (x / 400.0).sin() * (y / 350.0).cos()).collect();
        let dy: Vec<f64> = nodes.iter().map(|(x, y)| 0.9 * (x / 380.0).cos() * (y / 300.0).sin()).collect();
        let ix: Vec<f64> = dx.iter().map(|v| -v).collect();
        let iy: Vec<f64> = dy.iter().map(|v| -v).collect();
        let spline = PixelMap::with_distortion_model(
            Linear::identity(),
            DistortionModel::tps(
                ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).unwrap(),
                ThinPlateSpline::fit(&nodes, &ix, &iy, 0.0).unwrap(),
                [0.0, 0.0, W as f64, H as f64],
            ),
        )
        .unwrap();
        for star in [&identity, &similarity, &homography, &polynomial, &spline] {
            for offset in [0.0, 4.0] {
                assert_pin(star, offset);
            }
        }
    }

    /// Spec §5.9, plan 6b-3 D11: a grid master's WCS is the grid's TAN —
    /// 0-based CRPIX at the centre (the card adds 1), sky-standard parity,
    /// the rotation read back; a crop moves CRPIX by its origin only, and
    /// drizzle's scaling commutes with a crop at twice the origin.
    #[test]
    fn the_grid_master_wcs_is_the_grids_tan() {
        let grid = CanonicalGrid {
            scale_arcsec: 2.0,
            width_px: 1024,
            height_px: 768,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: -30.0,
        };
        let rec = grid_plate_solve(&grid, "2026-10-03T00:00:00Z");
        assert_eq!((rec.crpix1, rec.crpix2), (511.5, 383.5));
        assert_eq!(rec.sip_order, None);
        assert_eq!(rec.catalog_used, "grid");
        let sol = rec.to_solution().unwrap();
        let (ra, dec) = sol.pixel_to_sky(511.5, 383.5);
        assert!((ra - 10.5).abs() < 1e-12 && (dec - 41.25).abs() < 1e-12);
        assert!((sol.field_rotation_deg() - 330.0).abs() < 1e-9);
        assert!((sol.pixel_scale_arcsec() - 2.0).abs() < 1e-9);
        assert!(sol.cd[0][0] * sol.cd[1][1] - sol.cd[0][1] * sol.cd[1][0] < 0.0);
        let cards = crate::fits_writer::wcs::wcs_cards(&rec).unwrap();
        let crpix1 = cards.iter().find(|c| c.keyword == "CRPIX1").unwrap();
        assert_eq!(crpix1.value, Some(CardValue::Real(512.5)));

        let cropped = crop_plate_solve(&rec, 100, 50);
        let a = rec.to_solution().unwrap().pixel_to_sky(300.0, 200.0);
        let b = cropped.to_solution().unwrap().pixel_to_sky(200.0, 150.0);
        assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
        let scaled_after = crate::fits_writer::wcs::scale_plate_solve(&cropped, 2).unwrap();
        let scaled_before = crop_plate_solve(
            &crate::fits_writer::wcs::scale_plate_solve(&rec, 2).unwrap(),
            200,
            100,
        );
        assert!((scaled_after.crpix1 - scaled_before.crpix1).abs() < 1e-12);
        assert!((scaled_after.crpix2 - scaled_before.crpix2).abs() < 1e-12);
    }

    /// Plan 6b-3 D2: the registered hash moves with each input; the WCS hash
    /// is the reference's geometry, not its solve metadata.
    #[test]
    fn the_registered_hash_moves_with_the_star_map_the_reference_wcs_and_the_grid() {
        let base = grid_registered_hash("reg", "wcs", "grid");
        assert_eq!(base.len(), 16);
        assert_eq!(base, grid_registered_hash("reg", "wcs", "grid"));
        for other in [
            grid_registered_hash("reg2", "wcs", "grid"),
            grid_registered_hash("reg", "wcs2", "grid"),
            grid_registered_hash("reg", "wcs", "grid2"),
        ] {
            assert_ne!(base, other);
        }
        let mut r = reference_record(83.8, -5.4);
        let h = wcs_record_hash(&r);
        r.matched_stars = 7;
        r.solved_at = "later".into();
        assert_eq!(h, wcs_record_hash(&r), "solve metadata is not geometry");
        r.crpix1 += 0.5;
        assert_ne!(h, wcs_record_hash(&r));
    }

    /// A frame with no pixels, or a reference solve whose SIP cannot be
    /// read, never composes.
    #[test]
    fn composition_refuses_what_it_cannot_compose() {
        let reference = ReferenceWcs::from_record(&reference_record(83.8, -5.4)).unwrap();
        let grid = GridRun::new(CanonicalGrid {
            scale_arcsec: SCALE,
            width_px: 2048,
            height_px: 1600,
            crval1: 83.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        });
        let identity = PixelMap::linear(Linear::identity()).unwrap();
        assert!(compose_to_grid(&identity, (0, H), &reference, &grid).is_err());
        let mut broken = reference_record(83.8, -5.4);
        broken.sip_b_coeffs = None;
        assert!(ReferenceWcs::from_record(&broken).is_none());
        assert_eq!(subject_dims(64, 48, Path::new("/nowhere")).unwrap(), (64, 48));
        assert!(subject_dims(0, 48, Path::new("/nowhere/x.fits")).is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_sampled_layer composed_map grid_master_wcs registered_hash_moves composition_refuses --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`DisplacementField`, `DistortionModel::sampled`, `stacking::grid_map` do not exist). Record the output.

- [ ] **Step 3: Implement**

`geometry/pixel_map.rs` — after `impl InverseMap for Linear { … }`:
```rust
/// The exact displacement field behind a [`DistortionModel::Sampled`] layer
/// (plan 6b-3 D1): a function the layer samples into its bilinear grids,
/// with no fitted coefficients of its own. The project path's composed
/// subject → grid map (`stacking::grid_map::ComposedField`) is its one
/// implementor; `geometry` itself stays free of WCS types.
pub trait DisplacementField: Send + Sync + std::fmt::Debug {
    /// Forward displacement at `(x, y)` in the linear model's OUTPUT space,
    /// exact: `forward(s) = L(s) + forward(L(s))`.
    fn forward(&self, x: f64, y: f64) -> (f64, f64);
    /// Inverse displacement at output pixel `(x, y)`, exact:
    /// `inverse(g) = L⁻¹(g + inverse(g))`.
    fn inverse(&self, x: f64, y: f64) -> (f64, f64);
    /// A stable identity: two fields with one key describe one map.
    fn key(&self) -> &str;
}
```

Replace `TpsGrid::build`'s body with a call to a new `sample` (the arithmetic is unchanged, so every spline grid stays bit-identical):
```rust
    /// Samples `f` over `domain` at [`TPS_GRID_PX`] spacing, starting at the
    /// domain's lower corner and running two samples past `ceil(extent /
    /// step)` on the far side (the `+ 2` below, as `build` has always done),
    /// so the last cell is complete. Rows are independent, so the build runs
    /// over `rayon`; it is a pure function of its inputs.
    pub fn sample(domain: [f64; 4], f: impl Fn(f64, f64) -> (f64, f64) + Sync) -> TpsGrid {
        let step = TPS_GRID_PX as f64;
        let [x0, y0, x1, y1] = domain;
        let nx = ((x1 - x0) / step).ceil().max(0.0) as usize + 2;
        let ny = ((y1 - y0) / step).ceil().max(0.0) as usize + 2;
        let f = &f;
        let samples: Vec<(f32, f32)> = (0..ny)
            .into_par_iter()
            .flat_map_iter(|j| {
                let y = y0 + j as f64 * step;
                (0..nx).map(move |col| {
                    let x = x0 + col as f64 * step;
                    let (dx, dy) = f(x, y);
                    (dx as f32, dy as f32)
                })
            })
            .collect();
        TpsGrid {
            x0,
            y0,
            x1,
            y1,
            step,
            nx,
            ny,
            samples,
        }
    }

    /// Samples `spline` over `domain` — [`TpsGrid::sample`] of the spline's
    /// own displacement.
    pub fn build(spline: &ThinPlateSpline, domain: [f64; 4]) -> TpsGrid {
        TpsGrid::sample(domain, |x, y| spline.displacement(x, y))
    }
```

The variant, inside `pub enum DistortionModel { … }` after `Tps { … }`:
```rust
    /// Plan 6b-3 D1: a displacement field sampled on demand — a project
    /// frame's composed subject → grid map. Its exact path is the field
    /// itself; its pixel path reads bilinear grids built lazily from the
    /// field, shared by every clone and released like a spline's. Never
    /// serialized: a registration record stores the star map the layer was
    /// composed from, and the layer is re-derived from it.
    #[serde(skip)]
    Sampled {
        field: Arc<dyn DisplacementField>,
        /// Output-space box `[x0, y0, x1, y1]` the field is sampled over;
        /// queries clamp into it, as a spline's do.
        domain: [f64; 4],
        grids: Arc<TpsGrids>,
    },
```
If the `Serialize` derive refuses the field type despite `#[serde(skip)]`, wrap the field in a private newtype `SampledField(Arc<dyn DisplacementField>)` whose hand-written `Serialize` returns `Err(S::Error::custom("a composed map is never serialized"))`, and record the change under "Deviations from the brief".

Constructor, in `impl DistortionModel` beside `tps`:
```rust
    /// A sampled layer with an empty grid cache (plan 6b-3 D1).
    pub fn sampled(field: Arc<dyn DisplacementField>, domain: [f64; 4]) -> DistortionModel {
        DistortionModel::Sampled {
            field,
            domain,
            grids: Arc::new(TpsGrids::default()),
        }
    }
```

The arms (each next to the `Tps` arm it mirrors):
- `PartialEq`: `(DistortionModel::Sampled { field: a, domain: d1, .. }, DistortionModel::Sampled { field: b, domain: d2, .. }) => a.key() == b.key() && d1 == d2,`
- `is_well_formed`: `DistortionModel::Sampled { domain, .. } => domain.iter().all(|v| v.is_finite()) && domain[0] <= domain[2] && domain[1] <= domain[3],`
- `label`: `DistortionModel::Sampled { .. } => "sampled".to_string(),`
- `forward_grid`: `DistortionModel::Sampled { field, domain, grids } => Some(TpsGrids::get_or_build(&grids.forward, || { let field = Arc::clone(field); TpsGrid::sample(*domain, move |x, y| field.forward(x, y)) })),`
- `inverse_grid`: the same with `grids.inverse` and `field.inverse`.
- `release_grids`: replace `let DistortionModel::Tps { grids, .. } = self else { return 0; };` with
  ```rust
        let grids = match self {
            DistortionModel::Polynomial(_) => return 0,
            DistortionModel::Tps { grids, .. } | DistortionModel::Sampled { grids, .. } => grids,
        };
  ```
- `forward_displacement` / `inverse_displacement`: `DistortionModel::Sampled { .. } => self.forward_grid().map(|g| g.displacement_at(x, y)).unwrap_or((0.0, 0.0)),` (inverse: `inverse_grid`).
- `forward_displacement_exact`: `DistortionModel::Sampled { field, domain, .. } => { let (x, y) = Self::clamp_to_domain(domain, x, y); field.forward(x, y) }` (inverse: `field.inverse`).
- `grids_built`: `DistortionModel::Tps { grids, .. } | DistortionModel::Sampled { grids, .. } => { … }` (the existing body).
- `grid_cache`: `DistortionModel::Tps { grids, .. } | DistortionModel::Sampled { grids, .. } => Some(grids),`
- `EvalLayer::of`: add an arm identical to the `Tps` one for `Some(model @ DistortionModel::Sampled { .. })`.

`PixelMap::to_json`'s doc gains: "A map with a [`DistortionModel::Sampled`] layer is never serialized (plan 6b-3 D1): its `expect` would fire, and no caller hands one in — `to_record` serializes the star map before composition." The `expect` text becomes `"PixelMap is serializable unless it carries a sampled layer, which no caller serializes"`.

`geometry/mod.rs`: `pub use pixel_map::{DisplacementField, DistortionModel, ForwardEval, InverseEval, InverseMap, PixelMap};`

`stacking/mod.rs`: `pub mod grid_map;` (beside `pub mod groups;`; the module inherits `stacking`'s `all(render, solver)` gate).

Create `crates/athenaeum-core/src/stacking/grid_map.rs` (the tests of Step 1 go at its end):
```rust
//! The canonical grid inside a project run (spec §5.5, §5.9; plan 6b-3 D1,
//! D2, D11): the grid's TAN, a reference frame's WCS, the composed subject →
//! grid map every pixel consumer reads, the hash a grid-registered frame is
//! cached under, and the synthetic WCS a grid master carries.
//!
//! The composed map is ONE `PixelMap`. Its affine part is fitted to the
//! exact chain subject → reference (the star map) → sky (the reference's
//! WCS, SIP included) → grid (pure TAN), sampled every
//! [`COMPOSE_SAMPLE_PX`] including the last row and column. Its distortion
//! layer is a [`DistortionModel::Sampled`] over [`ComposedField`]: the exact
//! path evaluates the chain (the inverse by Newton steps on the forward
//! chain, never SIP's reverse tables or the star map's own fitted inverse);
//! the pixel path reads bilinear grids built lazily from it.
//!
//! Orientation: every solve in the owner's catalog has det CD > 0 and the
//! grid is sky-standard (det CD < 0), so a composed map is a reflection
//! (`PixelMap::is_flipped`) and a grid master is the mirror image of a raw
//! frame — correct on the sky.

use std::path::Path;
use std::sync::Arc;

use solvemyastro::wcs::WcsSolution;

use crate::collab::grid::CanonicalGrid;
use crate::collab::grid_geometry::grid_wcs;
use crate::geometry::linear::{fit_affine, Linear, Pair};
use crate::geometry::{DisplacementField, DistortionModel, PixelMap};
use crate::plate_solve::storage::PlateSolveRecord;
use crate::stacking::config::stage_hash;

/// Spacing (px) of the chain samples the affine part is fitted on (spec §5.5).
pub const COMPOSE_SAMPLE_PX: usize = 32;

/// Bumped when the composition's arithmetic changes: it is folded into every
/// grid-registered frame's hash, so a bump rewrites them.
pub const COMPOSE_VERSION: u32 = 1;

/// Newton iterations of the inverse chain, and its convergence tolerance.
const INVERSE_MAX_ITER: usize = 30;
const INVERSE_TOL_PX: f64 = 1e-9;

/// How far (px) the sampled domain reaches beyond the footprint: enough
/// for any interpolation kernel's support at the frame's edge.
const DOMAIN_MARGIN_PX: f64 = 64.0;

/// What a pixel whose chain cannot be evaluated is displaced by: far
/// outside every frame, so every consumer reads NaN there, never pixel 0.
const OFF_FOOTPRINT_PX: f64 = 1.0e7;

/// A project run's grid: the confirmed grid, its hash (`ATH_GRID`) and its
/// TAN projection.
#[derive(Debug, Clone)]
pub struct GridRun {
    pub grid: CanonicalGrid,
    pub hash: String,
    pub tan: WcsSolution,
}

impl GridRun {
    pub fn new(grid: CanonicalGrid) -> GridRun {
        let grid = grid.normalized();
        GridRun {
            hash: grid.hash(),
            tan: grid_wcs(&grid),
            grid,
        }
    }

    /// `(width, height)` in pixels — the geometry every group of the run is
    /// delivered in.
    pub fn dims(&self) -> (usize, usize) {
        (self.grid.width_px as usize, self.grid.height_px as usize)
    }
}

/// A registration reference's WCS (manifest or local solve), with the hash
/// of its geometry.
#[derive(Debug, Clone)]
pub struct ReferenceWcs {
    pub solution: WcsSolution,
    pub hash: String,
}

impl ReferenceWcs {
    /// `None` when the record's SIP tables cannot be read
    /// (`PlateSolveRecord::to_solution` logs why).
    pub fn from_record(record: &PlateSolveRecord) -> Option<ReferenceWcs> {
        let solution = record.to_solution()?;
        Some(ReferenceWcs {
            hash: wcs_record_hash(record),
            solution,
        })
    }
}

/// XXH3-64 (16 hex) of a solve's GEOMETRY — CRPIX, CRVAL, CD and the SIP
/// tables — never its metadata (star counts, times), so a re-solve that
/// moved nothing keeps every grid-registered frame.
pub fn wcs_record_hash(r: &PlateSolveRecord) -> String {
    let text = format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}",
        r.crpix1,
        r.crpix2,
        r.crval1,
        r.crval2,
        r.cd1_1,
        r.cd1_2,
        r.cd2_1,
        r.cd2_2,
        r.sip_order,
        r.sip_a_coeffs.as_deref().unwrap_or(""),
        r.sip_b_coeffs.as_deref().unwrap_or(""),
        r.sip_ap_coeffs.as_deref().unwrap_or(""),
        r.sip_bp_coeffs.as_deref().unwrap_or(""),
    );
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(text.as_bytes()))
}

/// The exact chain subject → grid, as a [`DisplacementField`] around the
/// fitted affine `linear`.
///
/// Where the star map is a spline, its exact path clamps at the spline's
/// domain, so the field has a derivative kink there: the one 8-px cell
/// straddling a frame edge carries up to ≈ 0.012 px of bilinear error. Every
/// consumer reads NaN outside the frame, so the 0.01 px pin is a statement
/// about the frame's interior; do not tighten it at the edge.
#[derive(Debug)]
pub struct ComposedField {
    star: PixelMap,
    reference: WcsSolution,
    grid: WcsSolution,
    linear: Linear,
    linear_inv: Linear,
    key: String,
}

impl ComposedField {
    /// Subject pixel → grid pixel, exactly: the star map's exact path, the
    /// reference's WCS forward (SIP A/B), the grid's TAN backward.
    pub fn chain(&self, x: f64, y: f64) -> (f64, f64) {
        chain_of(&self.star, &self.reference, &self.grid, x, y)
    }

    /// The subject pixel whose chain lands on `(gx, gy)`: Newton steps from
    /// the affine's own inverse, with the affine's Jacobian. The residual
    /// the affine leaves is a smooth few-pixel field, so this contracts by
    /// orders of magnitude per step. `NaN` when the chain is undefined.
    pub fn chain_inverse(&self, gx: f64, gy: f64) -> (f64, f64) {
        let (mut sx, mut sy) = self.linear_inv.apply(gx, gy);
        let m = &self.linear_inv.m;
        for _ in 0..INVERSE_MAX_ITER {
            let (cx, cy) = self.chain(sx, sy);
            let (rx, ry) = (gx - cx, gy - cy);
            if !(rx.is_finite() && ry.is_finite()) {
                return (f64::NAN, f64::NAN);
            }
            if rx.abs().max(ry.abs()) <= INVERSE_TOL_PX {
                break;
            }
            sx += m[0][0] * rx + m[0][1] * ry;
            sy += m[1][0] * rx + m[1][1] * ry;
        }
        (sx, sy)
    }
}

fn chain_of(star: &PixelMap, reference: &WcsSolution, grid: &WcsSolution, x: f64, y: f64) -> (f64, f64) {
    let (rx, ry) = star.forward_exact(x, y);
    let (ra, dec) = reference.pixel_to_sky(rx, ry);
    grid.sky_to_pixel(ra, dec)
}

fn finite_or_off(d: (f64, f64)) -> (f64, f64) {
    if d.0.is_finite() && d.1.is_finite() {
        d
    } else {
        (OFF_FOOTPRINT_PX, OFF_FOOTPRINT_PX)
    }
}

impl DisplacementField for ComposedField {
    fn forward(&self, x: f64, y: f64) -> (f64, f64) {
        let (sx, sy) = self.linear_inv.apply(x, y);
        let (gx, gy) = self.chain(sx, sy);
        finite_or_off((gx - x, gy - y))
    }

    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (sx, sy) = self.chain_inverse(x, y);
        let (lx, ly) = self.linear.apply(sx, sy);
        finite_or_off((lx - x, ly - y))
    }

    fn key(&self) -> &str {
        &self.key
    }
}

/// `0, 32, 64, …` up to `n − 1`, the last pixel always included.
fn lattice(n: usize) -> Vec<f64> {
    let mut out: Vec<f64> = (0..n).step_by(COMPOSE_SAMPLE_PX).map(|v| v as f64).collect();
    let last = (n - 1) as f64;
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// The composed subject → grid map of one frame (spec §5.5, plan 6b-3 D1):
/// `star` is the frame's registration onto `reference`; `subject_dims` is
/// the frame's own `(width, height)`. `Err` names why it cannot be built —
/// a frame with no pixels, a chain undefined on the frame, a singular
/// affine, or an inverse chain that does not converge — and the caller
/// excludes the frame with that reason.
pub fn compose_to_grid(
    star: &PixelMap,
    subject_dims: (usize, usize),
    reference: &ReferenceWcs,
    grid: &GridRun,
) -> Result<PixelMap, String> {
    let (w, h) = subject_dims;
    if w == 0 || h == 0 {
        return Err("the frame has no pixels".to_string());
    }
    let (xs, ys) = (lattice(w), lattice(h));
    let mut pairs: Vec<Pair> = Vec::with_capacity(xs.len() * ys.len());
    for &y in &ys {
        for &x in &xs {
            let g = chain_of(star, &reference.solution, &grid.tan, x, y);
            if !(g.0.is_finite() && g.1.is_finite()) {
                return Err(format!("the grid chain is undefined at frame pixel ({x}, {y})"));
            }
            pairs.push(((x, y), g));
        }
    }
    let linear = fit_affine(&pairs, None).ok_or("the grid affine is singular")?;
    let linear_inv = linear.inverse().ok_or("the grid affine is singular")?;

    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for ((x, y), g) in &pairs {
        for (px, py) in [linear.apply(*x, *y), *g] {
            b[0] = b[0].min(px);
            b[1] = b[1].min(py);
            b[2] = b[2].max(px);
            b[3] = b[3].max(py);
        }
    }
    let domain = [
        b[0] - DOMAIN_MARGIN_PX,
        b[1] - DOMAIN_MARGIN_PX,
        b[2] + DOMAIN_MARGIN_PX,
        b[3] + DOMAIN_MARGIN_PX,
    ];
    let key = format!(
        "{:016x}",
        xxhash_rust::xxh3::xxh3_64(
            format!(
                "{}|{}|{}|{w}x{h}|{COMPOSE_VERSION}",
                star.to_json(),
                reference.hash,
                grid.hash
            )
            .as_bytes()
        )
    );
    let field = ComposedField {
        star: star.clone(),
        reference: reference.solution.clone(),
        grid: grid.tan.clone(),
        linear,
        linear_inv,
        key,
    };
    let (wl, hl) = ((w - 1) as f64, (h - 1) as f64);
    for (x, y) in [(0.0, 0.0), (wl, 0.0), (0.0, hl), (wl, hl), (wl / 2.0, hl / 2.0)] {
        let g = field.chain(x, y);
        let (bx, by) = field.chain_inverse(g.0, g.1);
        if !((bx - x).abs() <= 1e-6 && (by - y).abs() <= 1e-6) {
            return Err(format!(
                "the inverse grid chain does not converge at frame pixel ({x}, {y})"
            ));
        }
    }
    PixelMap::with_distortion_model(linear, DistortionModel::sampled(Arc::new(field), domain))
        .ok_or_else(|| "the grid affine is singular".to_string())
}

/// The hash a grid-registered frame's `registered` artifact is cached under
/// (spec §5.5, plan 6b-3 D2): today's registration hash (the star map), the
/// reference's WCS geometry and the grid, with [`COMPOSE_VERSION`].
pub fn grid_registered_hash(
    registration_hash: &str,
    reference_wcs_hash: &str,
    grid_hash: &str,
) -> String {
    stage_hash(
        &serde_json::json!({ "grid": grid_hash, "composeVersion": COMPOSE_VERSION }),
        &[registration_hash, &format!("refwcs:{reference_wcs_hash}")],
        &[],
    )
}

/// The synthetic plate solve a grid master carries (spec §5.9, plan 6b-3
/// D11): the grid's TAN with a 0-based CRPIX at the grid's centre (the card
/// writer adds 1), no SIP, `"grid"` as catalog and algorithm.
pub fn grid_plate_solve(grid: &CanonicalGrid, solved_at: &str) -> PlateSolveRecord {
    let g = grid.normalized();
    let tan = grid_wcs(&g);
    PlateSolveRecord {
        id: None,
        frame_id: 0,
        crpix1: tan.crpix.0,
        crpix2: tan.crpix.1,
        crval1: tan.crval.0,
        crval2: tan.crval.1,
        cd1_1: tan.cd[0][0],
        cd1_2: tan.cd[0][1],
        cd2_1: tan.cd[1][0],
        cd2_2: tan.cd[1][1],
        sip_order: None,
        sip_a_coeffs: None,
        sip_b_coeffs: None,
        sip_ap_coeffs: None,
        sip_bp_coeffs: None,
        matched_stars: 0,
        total_detected: 0,
        rms_residual_px: 0.0,
        rms_residual_arcsec: 0.0,
        pixel_scale_arcsec: g.scale_arcsec,
        field_rotation_deg: tan.field_rotation_deg(),
        solve_time_ms: 0,
        catalog_used: "grid".to_string(),
        algorithm_used: "grid".to_string(),
        solved_at: solved_at.to_string(),
        expected_catalog_stars_in_fov: None,
        inlier_ratio: None,
    }
}

/// `record` with CRPIX moved by a crop's origin. SIP terms are in pixel
/// offsets from CRPIX, so they need no change; the result is exact for a
/// frame-set master's SIP solve and a grid's TAN alike.
pub fn crop_plate_solve(record: &PlateSolveRecord, x0: usize, y0: usize) -> PlateSolveRecord {
    let mut out = record.clone();
    out.crpix1 -= x0 as f64;
    out.crpix2 -= y0 as f64;
    out
}

/// A frame's own `(width, height)`: the frame source's when it has one,
/// else the file's header.
pub fn subject_dims(width: i64, height: i64, path: &Path) -> Result<(usize, usize), String> {
    if width > 0 && height > 0 {
        return Ok((width as usize, height as usize));
    }
    let reader = crate::integration::plane_reader::PlaneReader::open(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok((reader.width(), reader.height()))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_sampled_layer composed_map grid_master_wcs registered_hash_moves composition_refuses the_tps_grid_agrees --test-threads=4 2>&1 | tail -20`
Expected: 6 passed. If `the_composed_map_meets_the_chain_within_a_hundredth` misses the 0.01 px pin, do not loosen it: report the worst numbers per star-map kind and offset (that is a design question for the controller).

- [ ] **Step 5: Run the gates.** All of Global Constraints' "Gate per task"; `git diff --stat src/types` empty.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/geometry/pixel_map.rs crates/athenaeum-core/src/geometry/mod.rs crates/athenaeum-core/src/stacking/grid_map.rs crates/athenaeum-core/src/stacking/mod.rs
git commit -m "feat(stacking): the composed subject-to-grid map, a sampled distortion layer

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 2a: The star-map cache, the grid in the run context, registered hashes (D2, D3, D18; spec §4.2, §5.5)

Nothing composes yet: `RunContext.grid` is `None` on every run until Task 2b1's test helpers and Task 8a's `start_stacking` set it. This task lands the caches and the plumbing so Task 2b2 only adds the composition.

**Files:**
- Modify `crates/athenaeum-core/src/stacking/plan.rs`:
  - `REGISTRATION_ARTIFACT_KIND`; the project arms of `stored_registrations` / `store_registration` (`~988-1009`), replacing the B10 comment above them;
  - `registered_hash_for`;
  - `compute_register_stale` (`~1426-1563`) and its one call (`~2938`) gain `origins` and the grid;
  - `ln_registration_by_frame` (`~2562-2568`) maps a project's registration hashes onto registered hashes;
  - imports: `list_artifacts`, `upsert_artifact`, `NewArtifact` from `db::stacking`;
  - tests.
- Modify `crates/athenaeum-core/src/stacking/grid_map.rs`: `reference_wcs_hash_of`.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `RunContext.grid` (after `wcs_rule`, `~305`), `grid: None` in `start_stacking`'s literal (`~1001`) and in `test_context` (`~11100`);
  - `delivered_dims`, used by `adopt_co_registered_geometry` (`~6385-6397`) and `set_group_geometry` (`~6613-6632`);
  - `RegisteredFrameOutcome::Aligned` gains `registered_hash` (`~2905-2912`); its two constructions (`~5366`, `~5999`) and two readers (`~5406-5408`, `~7976-7979`);
  - `ArtifactRewriteItem` gains `registered_hash` (`~4982-4989`);
  - `RegisteredWrite` gains `registered_hash` (`~4928-4940`);
  - `write_one_registered_frame` gains `grid_hash: Option<&str>` (`~6870-6909`) and both callers pass `None`;
  - `record_registered_artifact` takes the hash instead of the record (`~6911-6936`), and its two callers (`~5639`, `~5983`);
  - the `RunSummary` literal in `start_stacking` (`~951`) and `test_context` (`~11045`) gets `grid: None`.
- Modify `crates/athenaeum-core/src/stacking/register/writer.rs`: `RegisteredCards.grid_hash` (`~44-55`) → `ATH_GRID`; the literals in its tests; one test.
- Modify `crates/athenaeum-core/src/stacking/provenance.rs`: `RunSummary.grid` (`~221-244`) and the literal in `a_summary_written_before_6b2_still_decodes` (named change: it also asserts a summary without `grid` decodes).
- `src/types/stacking.ts`: regenerated; the only move is `RunSummary` gaining `grid: CanonicalGrid | null`.

**Interfaces:**
```rust
// stacking/plan.rs
pub(crate) const REGISTRATION_ARTIFACT_KIND: &str = "registration";
pub(crate) fn registered_hash_for(registration_hash: &str, grid: Option<(&str /* reference WCS hash */, &str /* grid hash */)>) -> String;
fn compute_register_stale(conn: &Connection, cfg: &StackingConfig, subject: &StackSubject, groups: &[IntegrationGroup], reference: &PlanReference, memo: &mut HashMemo, origins: &FrameOrigins, grid: Option<&crate::collab::grid::CanonicalGrid>) -> Result<bool, ApiError>;
// stacking/grid_map.rs
pub fn reference_wcs_hash_of(conn: &rusqlite::Connection, origins: &FrameOrigins, frame_id: i64) -> anyhow::Result<Option<String>>;
// stacking/run.rs
pub(crate) grid: Option<Arc<crate::stacking::grid_map::GridRun>>, // RunContext
fn delivered_dims(rc: &RunContext, native: (usize, usize)) -> (usize, usize);
RegisteredFrameOutcome::Aligned { map: PixelMap, record: RegistrationRecord, cached: bool, registered_hash: String }
fn write_one_registered_frame(calibrated: &Path, map: &PixelMap, ref_w: usize, ref_h: usize, interp: Interpolation, clamping: f32, reference_name: &str, rec: &RegistrationRecord, grid_hash: Option<&str>, pool: Option<&Arc<rayon::ThreadPool>>, out: &Path) -> anyhow::Result<()>;
fn record_registered_artifact(rc: &mut RunContext, group_key: &str, frame_id: i64, registered_hash: &str, out: &Path) -> anyhow::Result<()>;
// register/writer.rs
pub struct RegisteredCards<'a> { /* … */ pub reference_roworder: Option<&'a str>, pub grid_hash: Option<&'a str> }
// provenance.rs
pub struct RunSummary { /* … */ pub grid: Option<crate::collab::grid::CanonicalGrid> }
```
Consumed by: Task 2b2 (everything above), Task 3 (`RunContext.grid`), Task 5a (`ln_registration_by_frame`), Task 8a (`RunSummary.grid`, `RunContext.grid`).

- [ ] **Step 1: Write the failing tests**

`stacking/plan.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D2: a project's star map is a payload-only `registration`
    /// artifact — read back by `stored_registrations`, kept by an
    /// `Intermediates` cleanup, dropped only by `All`.
    #[test]
    fn the_star_map_cache_survives_intermediates_cleanup() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let handle =
            crate::db::stacking::project_frame_handles(&conn, "p1", &["u1"]).unwrap()["u1"];
        let subject = StackSubject::Project("p1".into());
        store_registration(&conn, &subject, |set_id| RegistrationRecord {
            frames_set_id: set_id,
            frame_id: handle,
            reference_frame_id: handle,
            is_reference: true,
            status: "reference".into(),
            config_hash: Some("reghash".into()),
            transform_json: Some(
                crate::geometry::PixelMap::linear(crate::geometry::Linear::identity())
                    .unwrap()
                    .to_json(),
            ),
            ..Default::default()
        })
        .unwrap();
        let rows = stored_registrations(&conn, &subject).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].frame_id, rows[0].frames_set_id), (handle, 0));
        assert_eq!(rows[0].config_hash.as_deref(), Some("reghash"));
        let row = find_artifact(&conn, &subject, "", REGISTRATION_ARTIFACT_KIND, Some(handle))
            .unwrap()
            .unwrap();
        assert_eq!((row.path, row.config_hash.as_str()), (None, "reghash"));

        let tmp = tempfile::tempdir().unwrap();
        let layout = crate::stacking::paths::WorkingLayout::new(tmp.path(), "p1");
        crate::stacking::paths::cleanup_work(
            &conn,
            &subject,
            &layout,
            crate::stacking::paths::CleanupWhat::Intermediates,
        )
        .unwrap();
        assert_eq!(
            stored_registrations(&conn, &subject).unwrap().len(),
            1,
            "Intermediates keeps the star map"
        );
        crate::stacking::paths::cleanup_work(
            &conn,
            &subject,
            &layout,
            crate::stacking::paths::CleanupWhat::All,
        )
        .unwrap();
        assert!(stored_registrations(&conn, &subject).unwrap().is_empty(), "All drops it");
    }

    /// Plan 6b-3 D2: a frame set's registered artifact is keyed on its
    /// registration hash exactly as before; on a grid it is the composed hash.
    #[test]
    fn a_frame_sets_registered_hash_is_its_registration_hash() {
        assert_eq!(registered_hash_for("abc", None), "abc");
        assert_eq!(
            registered_hash_for("abc", Some(("w", "g"))),
            crate::stacking::grid_map::grid_registered_hash("abc", "w", "g")
        );
    }
```

`stacking/register/writer.rs`, `mod tests` (add `grid_hash: None` to every existing `RegisteredCards { … }` literal there):
```rust
    /// Plan 6b-3 D2, D3: a grid-registered frame states the grid it was
    /// resampled onto (`ATH_GRID`) and the grid's own row order; a
    /// frame-set registered frame carries neither, byte for byte as before.
    #[test]
    fn a_grid_registered_frame_states_its_grid_and_row_order() {
        let source = vec![
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("FILTER", CardValue::Str("L".into())).unwrap(),
        ];
        let build = |grid_hash: Option<&str>| {
            build_registered_cards(
                &source,
                &RegisteredCards {
                    reference_name: "c_ref",
                    model: "affine",
                    transform_json: "{}",
                    interpolation: Interpolation::BicubicBSpline,
                    clamping: 0.3,
                    rms_px: 0.1,
                    reference_roworder: grid_hash.map(|_| "BOTTOM-UP"),
                    grid_hash,
                },
            )
            .unwrap()
        };
        let get = |cards: &[Card], k: &str| {
            cards
                .iter()
                .find(|c| c.keyword == k)
                .and_then(|c| c.value.clone())
        };
        let grid = build(Some("0123456789abcdef"));
        assert_eq!(get(&grid, "ATH_GRID"), Some(CardValue::Str("0123456789abcdef".into())));
        assert_eq!(get(&grid, "ROWORDER"), Some(CardValue::Str("BOTTOM-UP".into())));
        assert_eq!(grid.iter().filter(|c| c.keyword == "ROWORDER").count(), 1);
        let set = build(None);
        assert!(get(&set, "ATH_GRID").is_none());
        assert_eq!(get(&set, "ROWORDER"), Some(CardValue::Str("TOP-DOWN".into())));
    }
```

`stacking/provenance.rs`: in `a_summary_written_before_6b2_still_decodes`, add `grid: None,` to the literal and, after `v.as_object_mut().unwrap().remove("projectId");`, add `v.as_object_mut().unwrap().remove("grid");`; after the last assertion add `assert!(back.grid.is_none(), "a summary written before 6b-3 has no grid");`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- star_map_cache registered_hash_is grid_registered_frame_states summary_written_before --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`REGISTRATION_ARTIFACT_KIND`, `registered_hash_for`, `grid_hash`, `RunSummary.grid`).

- [ ] **Step 3: Implement**

`stacking/grid_map.rs` (after `wcs_record_hash`):
```rust
/// The WCS geometry hash of a registration reference — its manifest WCS,
/// else the run's local solve of it (`origin::stored_solve`) — or `None`
/// when it has neither. The plan and the run key a grid-registered frame on
/// it (plan 6b-3 D2).
pub fn reference_wcs_hash_of(
    conn: &rusqlite::Connection,
    origins: &crate::stacking::origin::FrameOrigins,
    frame_id: i64,
) -> anyhow::Result<Option<String>> {
    let origin = crate::stacking::origin::origin_of(origins, frame_id).map_err(anyhow::Error::from)?;
    Ok(crate::stacking::origin::stored_solve(conn, origin, frame_id)?
        .map(|record| wcs_record_hash(&record)))
}
```

`stacking/plan.rs` — replace the B10 comment and the two funnels:
```rust
/// The payload-only artifact a project run caches each frame's star map in
/// (spec §4.2, plan 6b-3 D2): one row per handle, `group_key = ""`, keyed
/// on the frame's registration hash, the `RegistrationRecord` as JSON with
/// `frames_set_id = 0`. Not in `paths::INTERMEDIATE_ARTIFACT_KINDS`, so only
/// `CleanupWhat::All` drops it: a cleanup that removes the registered
/// frames keeps the star matching. A project never writes
/// `registration_results` (catalog-keyed).
pub(crate) const REGISTRATION_ARTIFACT_KIND: &str = "registration";

/// The subject's stored registration rows: `registration_results` for a
/// frame set, the `registration` artifacts for a project. A project row
/// whose payload does not parse is skipped with a warning, and the frame
/// registers again.
pub(crate) fn stored_registrations(
    conn: &Connection,
    subject: &StackSubject,
) -> anyhow::Result<Vec<RegistrationRecord>> {
    match subject {
        StackSubject::FrameSet(id) => get_registration_for_frame_set(conn, *id),
        StackSubject::Project(project_id) => {
            let mut out = Vec::new();
            for row in list_artifacts(conn, subject, Some(REGISTRATION_ARTIFACT_KIND))? {
                match row
                    .payload_json
                    .as_deref()
                    .map(serde_json::from_str::<RegistrationRecord>)
                {
                    Some(Ok(record)) => out.push(record),
                    Some(Err(e)) => tracing::warn!(
                        project_id = %project_id,
                        frame_id = ?row.frame_id,
                        error = %e,
                        "stored star map unreadable; the frame registers again"
                    ),
                    None => tracing::warn!(
                        project_id = %project_id,
                        frame_id = ?row.frame_id,
                        "stored star map has no payload; the frame registers again"
                    ),
                }
            }
            Ok(out)
        }
    }
}

/// Upsert one registration row for the subject; `build` receives the frame
/// set id the record carries (0 for a project).
pub(crate) fn store_registration(
    conn: &Connection,
    subject: &StackSubject,
    build: impl FnOnce(i64) -> RegistrationRecord,
) -> anyhow::Result<()> {
    match subject {
        StackSubject::FrameSet(id) => upsert_registration(conn, &build(*id)).map(|_| ()),
        StackSubject::Project(_) => {
            let record = build(0);
            let payload = serde_json::to_string(&record)?;
            upsert_artifact(
                conn,
                &NewArtifact {
                    subject,
                    frame_id: Some(record.frame_id),
                    group_key: "",
                    kind: REGISTRATION_ARTIFACT_KIND,
                    path: None,
                    config_hash: record.config_hash.as_deref().unwrap_or_default(),
                    size: None,
                    modified_at: None,
                    payload_json: Some(&payload),
                },
            )?;
            Ok(())
        }
    }
}

/// The hash a frame's `registered` artifact is cached under (spec §5.5,
/// plan 6b-3 D2): its registration hash for a frame set; on a grid,
/// `grid_registered_hash` over the reference's WCS and the grid, so a grid
/// move or a re-solved reference rewrites the frame without star matching.
pub(crate) fn registered_hash_for(registration_hash: &str, grid: Option<(&str, &str)>) -> String {
    match grid {
        None => registration_hash.to_string(),
        Some((reference_wcs_hash, grid_hash)) => crate::stacking::grid_map::grid_registered_hash(
            registration_hash,
            reference_wcs_hash,
            grid_hash,
        ),
    }
}
```
Add `list_artifacts, upsert_artifact, NewArtifact` to the `use crate::db::stacking::{…}` import.

`compute_register_stale`: add the two parameters `origins: &FrameOrigins, grid: Option<&crate::collab::grid::CanonicalGrid>`. After `reference_hashes` is filled, add:
```rust
    // Plan 6b-3 D2: on a grid, the registered artifact also folds each
    // reference's WCS geometry and the grid; a reference without a WCS
    // cannot have produced one, so the stage reads stale.
    let grid_hash = grid.map(|g| g.hash());
    let mut reference_wcs_hashes: HashMap<i64, String> = HashMap::new();
    if grid_hash.is_some() {
        for &reference_frame_id in expected_reference.values() {
            if reference_wcs_hashes.contains_key(&reference_frame_id) {
                continue;
            }
            match crate::stacking::grid_map::reference_wcs_hash_of(conn, origins, reference_frame_id)
                .map_err(|e| ApiError::Internal(format!("{e:#}")))?
            {
                Some(h) => {
                    reference_wcs_hashes.insert(reference_frame_id, h);
                }
                None => return Ok(true),
            }
        }
    }
```
and replace the artifact check at the end of the loop:
```rust
        let artifact_hash = match &grid_hash {
            None => expected.clone(),
            Some(g) => match reference_wcs_hashes.get(&reference_frame_id) {
                Some(w) => registered_hash_for(&expected, Some((w, g))),
                None => return Ok(true),
            },
        };
        let artifact = find_artifact(conn, subject, &group.key, "registered", Some(frame_id))?;
        if !artifact.is_some_and(|row| is_fresh(&row, &artifact_hash)) {
            return Ok(true);
        }
```
At the call in `build_plan`: `compute_register_stale(conn, &cfg, subject, &groups, &reference, &mut memo, &origins, project_grid.as_ref())?`, with, right after the `SubjectInputs` destructuring, `let project_grid: Option<crate::collab::grid::CanonicalGrid> = project.as_ref().and_then(|p| p.grid.current);` (`CanonicalGrid` is `Copy`).

`ln_registration_by_frame`:
```rust
    let ln_registration_by_frame: HashMap<i64, Option<String>> = if local_normalization_active {
        let rows = stored_registrations(conn, subject)?;
        match project_grid.as_ref().map(|g| g.hash()) {
            None => rows.into_iter().map(|r| (r.frame_id, r.config_hash)).collect(),
            // Plan 6b-3 D2: on a grid, LN's registration upstream is the
            // registered (composed) hash the run folds in.
            Some(grid_hash) => {
                let mut wcs: HashMap<i64, Option<String>> = HashMap::new();
                let mut out = HashMap::with_capacity(rows.len());
                for r in rows {
                    let w = match wcs.get(&r.reference_frame_id) {
                        Some(w) => w.clone(),
                        None => {
                            let w = crate::stacking::grid_map::reference_wcs_hash_of(
                                conn,
                                &origins,
                                r.reference_frame_id,
                            )
                            .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
                            wcs.insert(r.reference_frame_id, w.clone());
                            w
                        }
                    };
                    let registered = match (r.config_hash, w) {
                        (Some(h), Some(w)) => Some(registered_hash_for(&h, Some((&w, &grid_hash)))),
                        _ => None,
                    };
                    out.insert(r.frame_id, registered);
                }
                out
            }
        }
    } else {
        HashMap::new()
    };
```
(`origins` is the `SubjectInputs.origins` binding. `build_plan` moves it into `HashMemo::new(origins)` at `~2549`; change that call to `HashMemo::new(origins.clone())` — `FrameOrigins` is an `Arc` — so the LN map above and `compute_register_stale` below can borrow it.)

`stacking/register/writer.rs`: add to `RegisteredCards`
```rust
    /// The grid hash of a frame registered onto a project's canonical grid
    /// (plan 6b-3 D2) — stamped as `ATH_GRID`; `None` for a frame set.
    pub grid_hash: Option<&'a str>,
```
and in `build_registered_cards`, the `ROWORDER` card's comment becomes `"row order of the output grid"` (review M3: it reads true for a frame set's reference grid and for a project's canonical grid; production passes `reference_roworder: None` for every frame set today, `run.rs:6892`, so no frame-set byte moves), and right after the `ATH_REGS` push (before `ATH_REGT`):
```rust
    if let Some(grid) = reg.grid_hash {
        cards.push(
            Card::new("ATH_GRID", CardValue::Str(grid.to_string()))?
                .with_comment("canonical grid hash"),
        );
    }
```

`stacking/provenance.rs`, `RunSummary`, after `config_hash`:
```rust
    // The canonical grid a project run was registered onto (plan 6b-3 D18);
    // `None` for a frame set and for a summary written before 6b-3.
    #[serde(default)]
    pub grid: Option<crate::collab::grid::CanonicalGrid>,
```

`stacking/run.rs`:
- `RunContext`, after `wcs_rule`:
  ```rust
    /// A project run's canonical grid (spec §5.5, plan 6b-3): every group is
    /// delivered in its geometry, through one composed map per frame.
    /// `None` for a frame set.
    pub(crate) grid: Option<Arc<crate::stacking::grid_map::GridRun>>,
  ```
  `grid: None,` in `start_stacking`'s `RunContext { … }` and in `test_context`'s; `grid: None,` in both `RunSummary { … }` literals.
- After `set_group_geometry`:
  ```rust
/// The geometry a group is delivered in (plan 6b-3): the grid's on a
/// project run, else the group reference's own `native` dimensions.
fn delivered_dims(rc: &RunContext, native: (usize, usize)) -> (usize, usize) {
    rc.grid.as_ref().map(|g| g.dims()).unwrap_or(native)
}
  ```
  `adopt_co_registered_geometry`: `let (width, height) = delivered_dims(rc, (rc.reference_width, rc.reference_height));`. `set_group_geometry`: compute `let (width, height) = delivered_dims(rc, (ref_stars.width, ref_stars.height));` before the `get_mut`, and assign `entry.width = width; entry.height = height;`. `rc.reference_width/height` keep the reference's own dimensions (the two-pass diagonal and the scale statistics read them).
- `RegisteredFrameOutcome::Aligned` gains, after `cached`:
  ```rust
        /// The hash this frame's `registered` artifact is cached under:
        /// its registration hash, or the composed one on a grid (plan 6b-3
        /// D2). Integration's materialized lookup and LN key on it.
        registered_hash: String,
  ```
  The reused branch (`~5366`): `registered_hash: row.config_hash.clone().unwrap_or_default(),`. The freshness check below it destructures `RegisteredFrameOutcome::Aligned { map, record, registered_hash, .. }` and passes `registered_hash` to `registered_artifact_is_fresh` and into the `ArtifactRewriteItem`. The persisting results loop (`~5999`): `registered_hash` from the `RegisteredWrite`. `process_group_output` (`~7976`): `Some(RegisteredFrameOutcome::Aligned { map, registered_hash, .. }) => (map.clone(), registered_hash.clone()),`.
- `ArtifactRewriteItem` gains `registered_hash: String`; the rewrite fan-out returns `(item.frame_id, item.registered_hash.clone(), Result<PathBuf, String>)` instead of the record, and the results loop calls `record_registered_artifact(rc, &group.key, frame_id, &registered_hash, &out_path)`.
- `RegisteredWrite` gains `registered_hash: String`, filled with `item.expected_hash.clone()` in the main fan-out (Task 2b2 composes it on a grid); the results loop calls `record_registered_artifact(rc, &group.key, frame.frame_id, &registered_hash, out_path)` and builds the outcome with it.
- `write_one_registered_frame`: new parameter `grid_hash: Option<&str>` after `rec`; `RegisteredCards { …, reference_roworder: grid_hash.map(|_| "BOTTOM-UP"), grid_hash }` (D3: a grid frame states the grid's own row order, never the reference's). Both callers pass `None` in this task.
- `record_registered_artifact`: replace `rec: &RegistrationRecord` with `registered_hash: &str` and use it as `config_hash`; its doc names D2.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- star_map_cache registered_hash_is grid_registered_frame_states summary_written_before stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass (the frame-set run tests are unchanged: a frame set's `registered_hash` is its registration hash).

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` shows only `src/types/stacking.ts` (`RunSummary.grid`).

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/stacking/grid_map.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/register/writer.rs crates/athenaeum-core/src/stacking/provenance.rs src/types/stacking.ts
git commit -m "feat(stacking): a project's star-map cache, registered hashes on a grid, the grid in the run context

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 2b1: The sky-field project fixture (D1, D22; first half of the review's registration split)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/test_fixtures.rs`: `add_project_frame`'s manifest-and-landing tail moves into a shared helper (a pure move); the sky-field project frames; the east-pair constants (review I5); one fixture test.
- Modify `crates/athenaeum-core/src/stacking/run.rs`, `mod tests` only: the 6b-3 run-test helpers (`seed_sky_project`, `sky_config`, `run_to_register`, `centroid_near`) and `a_project_frame_registers_onto_the_grid`, committed `#[ignore]`d with its reason.

No production code changes: an implementer who breaks the fixture never also holds a half-edited registration fan-out.

**Interfaces:**
```rust
// stacking/test_fixtures.rs (#[cfg(test)])
pub(crate) struct ManifestSeed<'a> { /* the manifest row and the landing of one fixture frame */ }
pub(crate) fn seed_manifest_and_land(conn: &Connection, project_id: &str, s: &ManifestSeed<'_>);
pub(crate) struct SkyFrame<'a> { pub uuid: &'a str, pub publisher: &'a str, pub filter: &'a str, pub width: usize, pub height: usize, pub scale_arcsec: f64, pub centre: (f64, f64), pub file: ProjectFile, pub bayerpat: Option<&'a str>, pub manifest_wcs: bool, pub noise_seed: u64, pub accepted: bool }
pub(crate) fn sky_frame(uuid: &str, centre: (f64, f64)) -> SkyFrame<'_>;
pub(crate) fn sky_stars(centre: (f64, f64), half_deg: f64, spacing_arcsec: f64, seed: u64) -> Vec<(f64, f64, f64)>;
pub(crate) fn sky_frame_record(f: &SkyFrame<'_>) -> PlateSolveRecord;
pub(crate) struct RenderedSkyFrame { pub path: PathBuf, pub file_name: String, pub meta: serde_json::Value, pub byte_size: i64, pub blake3: String }
pub(crate) fn render_sky_frame(dir: &Path, project_id: &str, f: &SkyFrame<'_>, stars: &[(f64, f64, f64)]) -> RenderedSkyFrame;
pub(crate) fn add_project_sky_frame(conn: &Connection, dir: &Path, project_id: &str, f: &SkyFrame<'_>, stars: &[(f64, f64, f64)]) -> PathBuf;
pub(crate) const EAST_PAIR_OFFSET_WIDTHS: f64 = 0.85;
pub(crate) const EAST_PAIR_SPACING_ARCSEC: f64 = 40.0;
pub(crate) const EAST_PAIR_STAR_SEED: u64 = 9;
// stacking/run.rs, mod tests
fn seed_sky_project(conn: &rusqlite::Connection, landed: &Path, n: usize) -> crate::collab::grid::CanonicalGrid;
fn sky_config(conn: &rusqlite::Connection) -> StackingConfig;
fn run_to_register(rc: &mut RunContext);
fn centroid_near(plane: &[f32], w: usize, x: f64, y: f64) -> (f64, f64);
```
Consumed by: Task 2b2 (the helpers, the ignored test), Tasks 3, 5b, 6a, 7b2, 8a (run-level project tests), Task 9 (`render_sky_frame`, `sky_frame_record`, `sky_stars`, the east-pair constants).

- [ ] **Step 1: Add the sky-field fixture**

`stacking/test_fixtures.rs` — first move the tail of `add_project_frame` (from `let mut meta = serde_json::Map::new();` to the end) into a helper both fixtures share, and make `add_project_frame` call it with exactly the values it computes today:
```rust
/// The manifest row and the landing of one fixture project frame — what
/// the exchange leaves behind — shared by [`add_project_frame`] and
/// [`add_project_sky_frame`].
pub(crate) struct ManifestSeed<'a> {
    pub uuid: &'a str,
    pub publisher: &'a str,
    pub filter: &'a str,
    pub exptime_sec: f64,
    pub date_obs: &'a str,
    pub file_name: &'a str,
    pub meta: serde_json::Value,
    pub osc: bool,
    pub accepted: bool,
    pub byte_size: i64,
    pub blake3: String,
    /// `Some(path)` lands the frame there with its real `size:mtime`.
    pub landed: Option<&'a Path>,
}

pub(crate) fn seed_manifest_and_land(conn: &Connection, project_id: &str, s: &ManifestSeed<'_>) {
    let seq: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_frames_local", [], |r| r.get(0))
        .unwrap();
    let view: FrameViewWire = serde_json::from_value(serde_json::json!({
        "frameUuid": s.uuid, "frameSeq": seq + 1,
        "publisherAccountId": s.publisher, "publisherDisplayName": s.publisher, "own": false,
        "fileName": s.file_name, "contentVersion": 1, "blake3": s.blake3, "byteSize": s.byte_size,
        "xxh3": "0123456789abcdef", "filterRaw": s.filter, "filterCanonical": s.filter,
        "channel": if s.osc { "osc" } else { "mono" },
        "exptimeSec": s.exptime_sec, "dateObs": s.date_obs,
        "meta": s.meta.clone(), "gateVersion": 0,
        "accepted": s.accepted, "acceptedReason": null, "state": "published",
        "rejectReason": null, "manifestVersion": seq + 1, "createdAt": "2026-10-01T00:00:00Z",
    }))
    .unwrap();
    crate::db::collab_frames::upsert_from_manifest(conn, project_id, &view).unwrap();
    if let Some(path) = s.landed {
        let stamp =
            crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(path).unwrap()).encode();
        crate::db::collab_frames::set_landed(conn, project_id, s.uuid, path.to_str().unwrap(), &stamp)
            .unwrap();
    }
}
```
`add_project_frame` keeps building `meta` (as a `serde_json::Map`) and the `(byte_size, blake3)` pair exactly as today, then calls `seed_manifest_and_land(conn, project_id, &ManifestSeed { uuid: spec.uuid, publisher: spec.publisher, filter: spec.filter, exptime_sec: spec.exptime_sec, date_obs: spec.date_obs, file_name: &file_name, meta: serde_json::Value::Object(meta), osc: spec.bayerpat.is_some(), accepted: spec.accepted, byte_size, blake3, landed: (spec.file != ProjectFile::Missing).then_some(path.as_path()) })` and returns `path`.

Then add the sky field:
```rust
// ── Sky-field project frames (plan 6b-3) ─────────────────────────────────────

/// One project frame rendered from a sky star list through its own WCS:
/// stars land where the frame's WCS puts them, so registration, the
/// composed map and the grid all have a ground truth. The WCS has the
/// parity of every solve in the owner's catalog (det CD > 0), unrotated.
pub(crate) struct SkyFrame<'a> {
    pub uuid: &'a str,
    pub publisher: &'a str,
    pub filter: &'a str,
    pub width: usize,
    pub height: usize,
    pub scale_arcsec: f64,
    /// RA/Dec of the frame's centre pixel.
    pub centre: (f64, f64),
    /// `Mono32` or `Rgb32`.
    pub file: ProjectFile,
    pub bayerpat: Option<&'a str>,
    /// `false` leaves `meta.wcs` out: the run plate-solves the frame.
    pub manifest_wcs: bool,
    pub noise_seed: u64,
    pub accepted: bool,
}

/// A mono `L` frame, 384 × 288 at 1.5″/px, with a manifest WCS.
pub(crate) fn sky_frame(uuid: &str, centre: (f64, f64)) -> SkyFrame<'_> {
    SkyFrame {
        uuid,
        publisher: "ann",
        filter: "L",
        width: 384,
        height: 288,
        scale_arcsec: 1.5,
        centre,
        file: ProjectFile::Mono32,
        bayerpat: None,
        manifest_wcs: true,
        noise_seed: uuid.bytes().map(u64::from).sum(),
        accepted: true,
    }
}

/// A deterministic star list `(ra, dec, amplitude)`: a lattice of
/// `spacing_arcsec` over a `half_deg` box around `centre`, each star
/// jittered by up to ±30 % of the spacing (a regular lattice would make
/// quad matching ambiguous), amplitudes 0.05–0.20 over the field's 0.08
/// background.
pub(crate) fn sky_stars(
    centre: (f64, f64),
    half_deg: f64,
    spacing_arcsec: f64,
    seed: u64,
) -> Vec<(f64, f64, f64)> {
    let step = spacing_arcsec / 3600.0;
    let n = (2.0 * half_deg / step).ceil() as i64;
    let cos_dec = centre.1.to_radians().cos();
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut out = Vec::new();
    for i in 0..=n {
        for j in 0..=n {
            let dx = (i as f64 - n as f64 / 2.0 + (next() - 0.5) * 0.6) * step;
            let dy = (j as f64 - n as f64 / 2.0 + (next() - 0.5) * 0.6) * step;
            let amplitude = 0.05 + 0.15 * next();
            out.push((centre.0 + dx / cos_dec, centre.1 + dy, amplitude));
        }
    }
    out
}

/// The frame's own solve: 0-based CRPIX at the centre pixel, det CD > 0.
pub(crate) fn sky_frame_record(f: &SkyFrame<'_>) -> crate::plate_solve::storage::PlateSolveRecord {
    let s = f.scale_arcsec / 3600.0;
    crate::plate_solve::storage::PlateSolveRecord {
        id: None,
        frame_id: 0,
        crpix1: (f.width as f64 - 1.0) / 2.0,
        crpix2: (f.height as f64 - 1.0) / 2.0,
        crval1: f.centre.0,
        crval2: f.centre.1,
        cd1_1: s,
        cd1_2: 0.0,
        cd2_1: 0.0,
        cd2_2: s,
        sip_order: None,
        sip_a_coeffs: None,
        sip_b_coeffs: None,
        sip_ap_coeffs: None,
        sip_bp_coeffs: None,
        matched_stars: 50,
        total_detected: 60,
        rms_residual_px: 0.1,
        rms_residual_arcsec: 0.1 * f.scale_arcsec,
        pixel_scale_arcsec: f.scale_arcsec,
        field_rotation_deg: 0.0,
        solve_time_ms: 1,
        catalog_used: "fixture".into(),
        algorithm_used: "fixture".into(),
        solved_at: "2026-10-03T00:00:00Z".into(),
        expected_catalog_stars_in_fov: None,
        inlier_ratio: None,
    }
}

pub(crate) struct RenderedSkyFrame {
    pub path: PathBuf,
    pub file_name: String,
    pub meta: serde_json::Value,
    pub byte_size: i64,
    pub blake3: String,
}

/// Writes the frame as a landed replica would look (unit-domain float32,
/// a replica's cards including `CALSTAT`/`ATH_CSRC`/`ATH_PRJ`) under
/// `dir/<publisher>/c_<uuid>.fits`, and returns its manifest `meta` (the
/// WCS with a 1-BASED CRPIX, as the manifest stores it).
pub(crate) fn render_sky_frame(
    dir: &Path,
    project_id: &str,
    f: &SkyFrame<'_>,
    stars: &[(f64, f64, f64)],
) -> RenderedSkyFrame {
    let record = sky_frame_record(f);
    let wcs = record.to_solution().expect("a linear solve converts");
    let (w, h) = (f.width, f.height);
    let pixels: Vec<(f64, f64, f64)> = stars
        .iter()
        .filter_map(|&(ra, dec, amplitude)| {
            let (x, y) = wcs.sky_to_pixel(ra, dec);
            (x > -8.0 && y > -8.0 && x < w as f64 + 8.0 && y < h as f64 + 8.0)
                .then_some((x, y, amplitude))
        })
        .collect();
    let plane = synthetic_star_field(w, h, &pixels, 3.0, 0.002, f.noise_seed);
    let file_name = format!("c_{}.fits", f.uuid);
    let folder = dir.join(f.publisher);
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(&file_name);
    let cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
        Card::new("FILTER", CardValue::Str(f.filter.to_string())).unwrap(),
        Card::new("EXPTIME", CardValue::Real(300.0)).unwrap(),
        Card::new("DATE-OBS", CardValue::Str("2026-01-10T21:00:00".into())).unwrap(),
        Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
        Card::new("ROWORDER", CardValue::Str(FIXTURE_ROW_ORDER.into())).unwrap(),
        Card::new("CALSTAT", CardValue::Str("BDF".into())).unwrap(),
        Card::new("ATH_CSRC", CardValue::Str(f.uuid.to_string())).unwrap(),
        Card::new("ATH_PRJ", CardValue::Str(project_id.to_string())).unwrap(),
    ];
    match f.file {
        ProjectFile::Mono32 => write_fits_f32(&path, w, h, 1, &plane, &cards).unwrap(),
        ProjectFile::Rgb32 => {
            let rgb: Vec<f32> = [1.0f32, 0.8, 0.6]
                .iter()
                .flat_map(|k| plane.iter().map(move |v| v * k))
                .collect();
            write_fits_f32(&path, w, h, 3, &rgb, &cards).unwrap();
        }
        other => panic!("a sky frame is Mono32 or Rgb32, not {other:?}"),
    }
    let mut meta = serde_json::json!({
        "naxis1": w,
        "naxis2": h,
        "instrume": "cam",
        "pixelScaleArcsec": f.scale_arcsec,
    });
    if let Some(b) = f.bayerpat {
        meta["bayerpat"] = serde_json::json!(b);
    }
    if f.manifest_wcs {
        meta["wcs"] = serde_json::json!({
            "crval1": record.crval1,
            "crval2": record.crval2,
            "crpix1": record.crpix1 + 1.0,
            "crpix2": record.crpix2 + 1.0,
            "cd": [record.cd1_1, record.cd1_2, record.cd2_1, record.cd2_2],
        });
    }
    let bytes = std::fs::read(&path).unwrap();
    RenderedSkyFrame {
        path,
        file_name,
        meta,
        byte_size: bytes.len() as i64,
        blake3: blake3::hash(&bytes).to_hex().to_string(),
    }
}

/// [`render_sky_frame`], then the manifest row and the landing.
pub(crate) fn add_project_sky_frame(
    conn: &Connection,
    dir: &Path,
    project_id: &str,
    f: &SkyFrame<'_>,
    stars: &[(f64, f64, f64)],
) -> PathBuf {
    let r = render_sky_frame(dir, project_id, f, stars);
    seed_manifest_and_land(
        conn,
        project_id,
        &ManifestSeed {
            uuid: f.uuid,
            publisher: f.publisher,
            filter: f.filter,
            exptime_sec: 300.0,
            date_obs: "2026-01-10T21:00:00",
            file_name: &r.file_name,
            meta: r.meta.clone(),
            osc: f.bayerpat.is_some(),
            accepted: f.accepted,
            byte_size: r.byte_size,
            blake3: r.blake3.clone(),
            landed: Some(&r.path),
        },
    );
    r.path
}
```
(`ProjectFile` derives `Debug`, `test_fixtures.rs:875`.)

Then, beside the sky fixture, the east-pair geometry Task 5b's run test and Task 9's e2e share (review I5):
```rust
/// How far east of the main field the low-overlap pair sits, in frame
/// widths (plan 6b-3 D7, review I5): each of the two frames overlaps the
/// main field on ≈ 15 % of its own footprint, under the 25 % LN rule.
pub(crate) const EAST_PAIR_OFFSET_WIDTHS: f64 = 0.85;
/// The star spacing of that field. The pair registers over its ≈ 15 %
/// strip (≈ 86″ × 432″ at 1.5″/px): 40″ puts ≈ 23 stars there, 50″ ≈ 15.
/// Task 5b's run test registers the strip first; Task 9's e2e reuses it.
pub(crate) const EAST_PAIR_SPACING_ARCSEC: f64 = 40.0;
/// The star-field seed of that geometry, shared for the same reason.
pub(crate) const EAST_PAIR_STAR_SEED: u64 = 9;
```
and one fixture test in `test_fixtures.rs`'s own `mod tests` (`~789`), so a rendering fault shows here and not as a registration failure later:
```rust
    /// Plan 6b-3 Task 2b1: a rendered sky frame puts each star where its own
    /// WCS says — the brightest pixel within 2 px of the predicted position
    /// is within 1 px of it — and its manifest `meta.wcs` carries the 1-based
    /// `crpix` the exchange writes.
    #[test]
    fn a_sky_frame_puts_its_stars_where_its_wcs_says() {
        let dir = tempfile::tempdir().unwrap();
        let f = sky_frame("s0", (83.8, -5.4));
        let stars = sky_stars((83.8, -5.4), 0.12, 50.0, 7);
        let r = render_sky_frame(dir.path(), "p1", &f, &stars);
        let wcs = sky_frame_record(&f).to_solution().unwrap();
        let plane = crate::integration::plane_reader::PlaneReader::open(&r.path).unwrap().read_plane(0).unwrap();
        let (w, h) = (f.width as i64, f.height as i64);
        let mut checked = 0usize;
        for &(ra, dec, _) in &stars {
            let (x, y) = wcs.sky_to_pixel(ra, dec);
            if x < 10.0 || y < 10.0 || x > (w - 11) as f64 || y > (h - 11) as f64 {
                continue;
            }
            let (cx, cy) = (x.round() as i64, y.round() as i64);
            let mut best = (f32::MIN, 0i64, 0i64);
            for dy in -2..=2 {
                for dx in -2..=2 {
                    let v = plane[((cy + dy) * w + (cx + dx)) as usize];
                    if v > best.0 {
                        best = (v, cx + dx, cy + dy);
                    }
                }
            }
            assert!(
                (best.1 as f64 - x).hypot(best.2 as f64 - y) <= 1.0,
                "star at ({x:.2}, {y:.2}) peaks at ({}, {})",
                best.1,
                best.2
            );
            checked += 1;
        }
        assert!(checked >= 20, "{checked} stars inside the frame");
        assert_eq!(r.meta["wcs"]["crpix1"], serde_json::json!(f.width as f64 / 2.0 + 0.5));
    }
```

- [ ] **Step 2: Write the run-test helpers and the first grid test, ignored**

`stacking/run.rs`, `mod tests`, after the 6b-2 project tests (Task 2b2 removes the `#[ignore]`):
```rust
    /// The project fixture of the 6b-3 run tests: `n` mono L frames dithered
    /// around the fixture target (83.8, −5.4), every one with a manifest
    /// WCS, and the grid they land on (1.5″/px, 512 × 384, north up).
    fn seed_sky_project(conn: &rusqlite::Connection, landed: &Path, n: usize) -> crate::collab::grid::CanonicalGrid {
        crate::db::stacking::insert_test_project(conn, "p1");
        let target = (83.8, -5.4);
        let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
        let uuids: Vec<String> = (0..n).map(|i| format!("l{i}")).collect();
        for (i, uuid) in uuids.iter().enumerate() {
            // Dithers of ±3.6 / ±1.2 px (RA) and 1.2 px (Dec): the LN field's
            // frame-unit lattice (D8, review I6) resolves a dither, and the
            // 0.97 rule fails a member that misses over 3 % of the field —
            // as it fails a frame set's member against the reference.
            let dither = ((i as f64 - 1.5) * 0.001, (i % 2) as f64 * 0.0005);
            let mut f = test_fixtures::sky_frame(uuid, (target.0 + dither.0, target.1 + dither.1));
            f.noise_seed = 100 + i as u64;
            test_fixtures::add_project_sky_frame(conn, landed, "p1", &f, &stars);
        }
        crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 512,
            height_px: 384,
            crval1: target.0,
            crval2: target.1,
            rotation_deg: 0.0,
        }
    }

    /// The project config of the 6b-3 run tests, stored as the project's
    /// document so a plan resolves the same groups and hashes: the seed,
    /// with the two-pass re-pick off (its dry pass always detects afresh).
    fn sky_config(conn: &rusqlite::Connection) -> StackingConfig {
        let mut cfg = crate::stacking::config::project_seed_config(StackingConfig::default());
        cfg.reference.two_pass = false;
        crate::db::stacking::set_project_config(conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
            .unwrap();
        cfg
    }

    fn run_to_register(rc: &mut RunContext) {
        stage_calibrate(rc).unwrap();
        stage_measure(rc).unwrap();
        stage_reference(rc).unwrap();
        stage_register(rc).unwrap();
    }

    /// Background-subtracted centroid of the brightest blob within 3 px of
    /// `(x, y)` (7 × 7 window).
    fn centroid_near(plane: &[f32], w: usize, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = (x.round() as i64, y.round() as i64);
        let mut vals = Vec::new();
        for dy in -3..=3 {
            for dx in -3..=3 {
                vals.push(((cx + dx) as f64, (cy + dy) as f64, plane[((cy + dy) as usize) * w + (cx + dx) as usize] as f64));
            }
        }
        let mut sorted: Vec<f64> = vals.iter().map(|v| v.2).collect();
        sorted.sort_by(f64::total_cmp);
        let floor = sorted[sorted.len() / 2];
        let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
        for (px, py, v) in vals {
            let wgt = (v - floor).max(0.0);
            sx += px * wgt;
            sy += py * wgt;
            sw += wgt;
        }
        (sx / sw, sy / sw)
    }

    /// Spec §5.5, plan 6b-3 D1, D2, D3: on a project's grid every registered
    /// frame has the grid's size, NaN outside its footprint, its stars where
    /// the grid's TAN puts their sky positions, `ATH_GRID` and the grid's own
    /// row order. The star map is cached as a `registration` artifact, never
    /// in `registration_results`.
    #[test]
    #[ignore = "plan 6b-3 Task 2b2: registration composes onto the grid"]
    fn a_project_frame_registers_onto_the_grid() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let cfg = sky_config(&conn);
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
        let run = Arc::new(crate::stacking::grid_map::GridRun::new(grid));
        rc.grid = Some(run.clone());
        run_to_register(&mut rc);

        let group = rc.plan_groups[0].key.clone();
        let entries = &rc.measured[&group];
        assert_eq!(entries.iter().filter(|e| e.included).count(), 4, "{entries:?}");
        for e in entries {
            let Some(RegisteredFrameOutcome::Aligned { map, registered_hash, .. }) = &e.registration else {
                panic!("frame {} did not register: {:?}", e.frame.filename, e.reason);
            };
            assert!(map.is_flipped(), "a det CD > 0 frame lands mirrored on the grid");
            let row = crate::db::stacking::find_artifact(&conn, &rc.subject, &group, "registered", Some(e.frame.frame_id))
                .unwrap()
                .expect("a registered artifact");
            assert_eq!(&row.config_hash, registered_hash);
            let path = PathBuf::from(row.path.unwrap());
            let reader = crate::integration::plane_reader::PlaneReader::open(&path).unwrap();
            assert_eq!((reader.width(), reader.height()), (512, 384));
            let plane = reader.read_plane(0).unwrap();
            assert!(plane[0].is_nan() && plane[512 * 384 - 1].is_nan(), "NaN outside the footprint");
            let header = crate::fits_parser::FitsHeader::from_path(&path).unwrap();
            assert_eq!(header.get_str("ATH_GRID").as_deref(), Some(run.hash.as_str()));
            assert_eq!(header.get_str("ROWORDER").as_deref(), Some("BOTTOM-UP"));
            assert!(header.get_str("ATH_PRJ").is_none());
            // The sky star nearest the target lands where the grid puts it.
            let stars = test_fixtures::sky_stars((83.8, -5.4), 0.12, 50.0, 7);
            let star = stars
                .iter()
                .min_by(|a, b| {
                    let da = (a.0 - 83.8).hypot(a.1 + 5.4);
                    let db = (b.0 - 83.8).hypot(b.1 + 5.4);
                    da.total_cmp(&db)
                })
                .unwrap();
            let (gx, gy) = run.tan.sky_to_pixel(star.0, star.1);
            let (mx, my) = centroid_near(&plane, 512, gx, gy);
            assert!((mx - gx).hypot(my - gy) < 0.5, "{}: star at ({mx}, {my}), grid says ({gx}, {gy})", e.frame.filename);
        }
        let stars_cached = crate::stacking::plan::stored_registrations(&conn, &rc.subject).unwrap();
        assert_eq!(stars_cached.len(), 4);
        let catalog_rows: i64 = conn.query_row("SELECT COUNT(*) FROM registration_results", [], |r| r.get(0)).unwrap();
        assert_eq!(catalog_rows, 0, "a project never writes registration_results");
    }
```

- [ ] **Step 3: Confirm the ignored test fails for the right reason, and the fixture test passes**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_sky_frame_puts_its_stars registers_onto_the_grid --include-ignored --test-threads=4 2>&1 | tail -30`
Expected: `a_sky_frame_puts_its_stars_where_its_wcs_says` passes. `a_project_frame_registers_onto_the_grid` FAILS at `assert!(map.is_flipped(), …)`: every frame registered, but its map is still its star map onto the reference (same parity), not the composed, mirrored map Task 2b2 builds. Record the output. Any earlier failure (a frame that did not register, a missing artifact, a fixture panic) is a fixture fault: fix it in this task.

- [ ] **Step 4: Run the gates.** The ignored test does not run in them. `git diff --exit-code src/types`.

- [ ] **Step 5: Commit**
```bash
git add crates/athenaeum-core/src/stacking/test_fixtures.rs crates/athenaeum-core/src/stacking/run.rs
git commit -m "test(stacking): sky-field project fixture and the first grid registration test (ignored until the composition lands)

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 2b2: Registration onto the grid (D1, D2, D22; spec §5.5)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/run.rs`, `register_group_pass` (`~5283-6077`):
  - the compose context, resolved once per persisting pass;
  - the reused branch composes the cached star map (`~5360-5380`);
  - the main fan-out composes after `to_record` and writes with the composed map (`~5790-5870`);
  - `RegisteredWrite` gains `map`; the results loop stores it (`~5995-6003`);
  - both `write_one_registered_frame` calls pass the grid hash;
  - tests: `a_project_frame_registers_onto_the_grid` loses its `#[ignore]`; two more.

**Interfaces:** nothing new — the seam is Task 1's `compose_to_grid`/`grid_registered_hash`/`subject_dims`, Task 2a's `RunContext.grid`/`Aligned.registered_hash`/`write_one_registered_frame(.., grid_hash)`, and Task 2b1's fixture.
Consumed by: Task 3 (grid-registered frames), Task 5a (`GroupMember.registration_hash` is the registered hash on a grid).

- [ ] **Step 1: Write the two remaining failing tests, and un-ignore the first**

In `stacking/run.rs`, `mod tests`, delete the `#[ignore = …]` line above `a_project_frame_registers_onto_the_grid` (Task 2b1), then add after it:
```rust
    /// Spec §5.5, plan 6b-3 D2: moving the grid re-composes the cached star
    /// maps and rewrites every registered frame — no star detection runs.
    #[test]
    fn a_grid_change_recomposes_without_star_detection() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let cfg = sky_config(&conn);
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
        run_to_register(&mut rc);
        let group = rc.plan_groups[0].key.clone();
        let before: HashMap<i64, String> = rc.measured[&group]
            .iter()
            .map(|e| {
                let row = crate::db::stacking::find_artifact(&conn, &rc.subject, &group, "registered", Some(e.frame.frame_id))
                    .unwrap()
                    .unwrap();
                (e.frame.frame_id, row.config_hash)
            })
            .collect();

        let moved = crate::collab::grid::CanonicalGrid { crval2: grid.crval2 + 0.005, ..grid };
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(moved)));
        let detected_before = crate::stacking::register::frame::DETECT_FRAME_STARS_LOG
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(tmp.path()))
            .count();
        stage_register(&mut rc).unwrap();
        let detected_after = crate::stacking::register::frame::DETECT_FRAME_STARS_LOG
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(tmp.path()))
            .count();
        assert_eq!(detected_before, detected_after, "a grid move runs no star detection");
        for e in &rc.measured[&group] {
            let Some(RegisteredFrameOutcome::Aligned { cached, .. }) = &e.registration else {
                panic!("{} lost its registration", e.frame.filename);
            };
            assert!(*cached, "the star map is reused");
            let row = crate::db::stacking::find_artifact(&conn, &rc.subject, &group, "registered", Some(e.frame.frame_id))
                .unwrap()
                .unwrap();
            assert_ne!(row.config_hash, before[&e.frame.frame_id], "the registered frame is rewritten");
        }
    }

    /// Plan 6b-3 D2: the plan and the run agree — after a run the plan reports
    /// Register cached; a moved grid makes it stale.
    #[test]
    fn a_project_plan_reports_register_cached_then_stale_on_a_grid_change() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let cfg = sky_config(&conn);
        for (key, dir) in [
            (crate::settings::keys::STACKING_WORKING_DIR, tmp.path().join("work")),
            (crate::settings::keys::STACKING_OUTPUT_DIR, tmp.path().join("out")),
        ] {
            crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
        }
        crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
        run_to_register(&mut rc);

        let subject = StackSubject::Project("p1".into());
        let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &subject, None).unwrap();
        assert!(!plan.stale_stages.contains(&Stage::Register), "{:?}", plan.stale_stages);

        let moved = crate::collab::grid::CanonicalGrid { crval1: grid.crval1 + 0.01, ..grid };
        crate::db::collab::set_canonical_grid(&conn, "p1", Some(&moved.canonical_json())).unwrap();
        let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &subject, None).unwrap();
        assert!(plan.stale_stages.contains(&Stage::Register), "{:?}", plan.stale_stages);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- registers_onto_the_grid recomposes_without_star_detection register_cached_then_stale --test-threads=4 2>&1 | tail -20`
Expected: the first test FAILS at `map.is_flipped()` (as recorded in Task 2b1), the second and third fail because nothing composes onto the grid yet (no composed, mirrored map; registered hashes without the grid). Record the output.

- [ ] **Step 3: Implement the composition in `register_group_pass`**

Imports at the top of `run.rs`: `use crate::stacking::grid_map::{compose_to_grid, grid_registered_hash, subject_dims, GridRun, ReferenceWcs};`

Right after `let origins = rc.origins.clone();` (`~5338`), resolve the compose context once per persisting pass:
```rust
    // Spec §5.5, plan 6b-3 D1: on a project's grid every frame this pass
    // writes is composed onto the grid through its reference's WCS. The WCS
    // rule (stage 4) guarantees the reference has one; its absence here is a
    // pipeline fault, so it fails the run rather than writing reference-geometry frames.
    let compose: Option<(Arc<GridRun>, Arc<ReferenceWcs>)> = match (&rc.grid, persist) {
        (Some(grid), true) => {
            let record = {
                let conn = db(&rc.ctx)?.conn();
                solve_of(solves, &conn, &origins, reference_frame_id)
            };
            let wcs = record.as_ref().and_then(ReferenceWcs::from_record).ok_or_else(|| {
                RunError::Other(format!(
                    "the registration reference (frame {reference_frame_id}) has no usable WCS; the grid needs one"
                ))
            })?;
            Some((Arc::clone(grid), Arc::new(wcs)))
        }
        _ => None,
    };
```

The reused branch: replace the `Ok(map) => { reused = Some(…) }` arm of `PixelMap::from_json(…)` with
```rust
                        Ok(star) => {
                            let registration_hash = row.config_hash.clone().unwrap_or_default();
                            let composed = match &compose {
                                None => Ok((star, registration_hash)),
                                Some((grid, reference)) => {
                                    subject_dims(frame.width, frame.height, &path)
                                        .and_then(|dims| compose_to_grid(&star, dims, reference, grid))
                                        .map(|map| {
                                            let hash = grid_registered_hash(&registration_hash, &reference.hash, &grid.hash);
                                            (map, hash)
                                        })
                                }
                            };
                            match composed {
                                Ok((map, registered_hash)) => {
                                    reused = Some(RegisteredFrameOutcome::Aligned {
                                        map,
                                        record: row.clone(),
                                        cached: true,
                                        registered_hash,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        run_id = rc.run_id,
                                        frame_id = frame.frame_id,
                                        error = %e,
                                        "grid composition of a cached star map failed; re-registering"
                                    );
                                }
                            }
                        }
```

The main fan-out: before `let results = fan_out(…)`, add `let compose_ref = &compose;`. Inside the closure, turn `let out = out.map(|(reg, dry_stars)| { … });` into `let out = out.and_then(|(reg, dry_stars)| -> Result<_, String> { … Ok((reg, dry_stars, persisted)) });`, and in its `Ok(alignment)` arm, after `let rec = to_record(…);`:
```rust
                        let (map, registered_hash) = match compose_ref {
                            None => (alignment.map.clone(), item.expected_hash.clone()),
                            Some((grid, reference)) => {
                                let dims = subject_dims(item.frame.width, item.frame.height, &item.path)
                                    .map_err(|e| format!("grid composition failed: {e}"))?;
                                let map = compose_to_grid(&alignment.map, dims, reference, grid)
                                    .map_err(|e| format!("grid composition failed: {e}"))?;
                                let hash = grid_registered_hash(&item.expected_hash, &reference.hash, &grid.hash);
                                (map, hash)
                            }
                        };
```
then write with `&map` (not `&alignment.map`), passing `compose_ref.as_ref().map(|(g, _)| g.hash.as_str())` as `grid_hash`, and build `Some(RegisteredWrite { rec, written, write_error, registered_hash, map })`. A composition error reaches the results loop as `Some(Err(msg))`, the existing registration-failure arm: the frame is excluded with "grid composition failed: …" (or the run fails, per `selection.excludeOnRegistrationFailure`, as for any registration failure).

`RegisteredWrite` gains `/// The map the registered artifact was written with: the star map, or its composition onto the grid. map: PixelMap,`; the results loop takes `map` from it (replacing `let map = alignment.map.clone();`).

The cache-hit rewrite batch passes `compose.as_ref().map(|(g, _)| g.hash.as_str())` as `grid_hash` (its items already carry the composed map and hash from the reused branch).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- registers_onto_the_grid recomposes_without_star_detection register_cached_then_stale stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass. If a sky frame fails to register (its exclusion reason says so), raise the star density of `seed_sky_project` (`sky_stars` spacing 40″) — never weaken an assertion — and report it.

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` empty.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/run.rs
git commit -m "feat(stacking): register project frames onto the canonical grid through one composed map

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 3: Output and drizzle on the grid (D3, D11; spec §5.8, §5.9)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/master_cards.rs`: `ATH_SPRJ`, `ATH_GRID`, `RGEO_GRID`, `GridStamp`, `stamp_grid_cards`, `card_with_optional_comment`; one test.
- Modify `crates/athenaeum-core/src/stacking/paths.rs`: `subject_output_dir`; one test.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `stage_output` (`~10826-10916`): the grid's synthetic WCS replaces the reference's solve on a grid run;
  - `process_group_output`: `stamp_grid_cards` on the master's cards (after `build_master_light_cards`, `~8825-8848`) and on the drizzle cards (after `build_drizzle_cards`, `~9330-9340`); one warning for a grid group with an empty canonical filter (review M4);
  - `start_stacking` (`~935`): `output_dir` through `subject_output_dir` (a frame set's is unchanged; the project branch is reached from Task 8a);
  - tests: `project_rc` (test helper) puts the output under `subject_output_dir`; two run tests.
- Modify `crates/athenaeum-core/src/stacking/solve.rs`: the ignored real-data probe `real_data_grid_master_wcs_probe` (run by the acceptance agent).

**Interfaces:**
```rust
// stacking/master_cards.rs
pub const ATH_SPRJ: &str = "ATH_SPRJ";
pub const ATH_GRID: &str = "ATH_GRID";
pub const RGEO_GRID: &str = "grid";
pub struct GridStamp<'a> { pub project_id: &'a str, pub grid_hash: &'a str, pub filter: Option<&'a str> }
pub fn stamp_grid_cards(cards: &mut Vec<Card>, stamp: &GridStamp<'_>) -> Result<(), FitsWriteError>;
pub fn card_with_optional_comment(keyword: &str, value: &str, comment: &str) -> Result<Card, FitsWriteError>;
// stacking/paths.rs
pub fn subject_output_dir(output_root: &Path, subject: &StackSubject, label: &str) -> PathBuf;
```
Consumed by: Task 6a (`coverage_map_cards` copies `ATH_GRID`/`ATH_SPRJ`), Tasks 7b1 and 7b2 (crop restates `CRPIX` on these cards), Task 8a (`subject_output_dir`).

- [ ] **Step 1: Write the failing tests**

`stacking/master_cards.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D3, D11 and the 6b-2 acceptance lesson (an `ATH_REGR`
    /// overflow): grid cards never fail a write — a long project id loses
    /// its comment, a long canonical filter is written whole — each key
    /// appears once, and a grid master claims no plate solve.
    #[test]
    fn grid_cards_survive_long_values() {
        let mut cards = vec![
            Card::new("FILTER", CardValue::Str("raw".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("TOP-DOWN".into())).unwrap(),
            Card::new("PLTSOLVD", CardValue::Logical(true)).unwrap(),
            Card::new(ATH_RGEO, CardValue::Str("coRegistered".into())).unwrap(),
        ];
        let project_id = "p".repeat(60);
        let filter = "Optolong L-eXtreme dual narrowband, an unusually long canonical filter name";
        stamp_grid_cards(
            &mut cards,
            &GridStamp {
                project_id: &project_id,
                grid_hash: "0123456789abcdef",
                filter: Some(filter),
            },
        )
        .unwrap();
        for c in &cards {
            crate::fits_writer::card::format_card(c).unwrap();
        }
        let get = |k: &str| cards.iter().filter(|c| c.keyword == k).collect::<Vec<_>>();
        assert_eq!(get("FILTER").len(), 1);
        assert_eq!(get("FILTER")[0].value, Some(CardValue::Str(filter.into())));
        assert_eq!(get("ROWORDER")[0].value, Some(CardValue::Str("BOTTOM-UP".into())));
        assert_eq!(get(ATH_RGEO).len(), 1);
        assert_eq!(get(ATH_RGEO)[0].value, Some(CardValue::Str(RGEO_GRID.into())));
        assert_eq!(get(ATH_SPRJ)[0].value, Some(CardValue::Str(project_id.clone())));
        assert!(get(ATH_SPRJ)[0].comment.is_none(), "a long id drops its comment");
        assert_eq!(get(ATH_GRID)[0].value, Some(CardValue::Str("0123456789abcdef".into())));
        assert!(get("PLTSOLVD").is_empty(), "a grid TAN is not a plate solve");
        let tmp = tempfile::tempdir().unwrap();
        crate::fits_writer::write_fits_f32(&tmp.path().join("g.fits"), 2, 2, 1, &[0.0; 4], &cards)
            .unwrap();

        let mut short = Vec::new();
        stamp_grid_cards(
            &mut short,
            &GridStamp {
                project_id: "af136ec9-1885-4087-b684-9136ad655e9b",
                grid_hash: "0123456789abcdef",
                filter: None,
            },
        )
        .unwrap();
        assert!(
            short.iter().find(|c| c.keyword == ATH_SPRJ).unwrap().comment.is_some(),
            "a UUID keeps its comment"
        );
        assert!(short.iter().all(|c| c.keyword != "FILTER"), "no canonical filter, no FILTER card");
    }
```

`stacking/paths.rs`, `mod tests`:
```rust
    /// Spec §5.9: a project's masters land in `<output>/<title>/`; a frame
    /// set's stay at the output root.
    #[test]
    fn a_projects_masters_folder_is_its_title() {
        let root = Path::new("/out");
        assert_eq!(subject_output_dir(root, &StackSubject::FrameSet(3), "LDN 1272"), PathBuf::from("/out"));
        assert_eq!(
            subject_output_dir(root, &StackSubject::Project("p".into()), "LDN1272-test Project"),
            PathBuf::from("/out").join(crate::archive::path_layout::sanitize_for_filename("LDN1272-test Project"))
        );
        assert_eq!(
            subject_output_dir(root, &StackSubject::Project("p".into()), "///"),
            PathBuf::from("/out/project"),
            "a title that sanitizes to nothing still gets a folder"
        );
    }
```

`stacking/run.rs`, `mod tests` — first change `project_rc` so the run's output folder is the one `start_stacking` will use:
```rust
        let output = crate::stacking::paths::subject_output_dir(
            output,
            &subject,
            &crate::stacking::plan::subject_label(conn, &subject).unwrap(),
        );
```
(placed after `let subject = …;`, and pass `output.clone()` / `output.to_str()` where `output` was used). Then:
```rust
    /// Runs the whole pipeline on the 4-frame sky project, on its grid, with
    /// `tweak` applied to the project's stored config. The files' raw
    /// `FILTER` is `L`; the canonical filter is `Lum`.
    fn run_sky_project(
        tmp: &Path,
        tweak: impl FnOnce(&mut StackingConfig),
    ) -> (Arc<ServiceContext>, rusqlite::Connection, RunContext, crate::collab::grid::CanonicalGrid) {
        let db_path = tmp.join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.join("collab"), 4);
        conn.execute(
            "UPDATE project_frames_local SET filter_canonical = 'Lum' WHERE project_id = 'p1'",
            [],
        )
        .unwrap();
        let mut cfg = sky_config(&conn);
        tweak(&mut cfg);
        crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
            .unwrap();
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.join("work"), &tmp.join("out"));
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
        run_pipeline(&mut rc).unwrap();
        (ctx, conn, rc, grid)
    }

    fn drizzle_2x(cfg: &mut StackingConfig) {
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
    }

    fn written_wcs(header: &crate::fits_parser::FitsHeader) -> solvemyastro::wcs::WcsSolution {
        let f = |k: &str| header.get_f64(k).unwrap_or_else(|| panic!("{k} missing"));
        solvemyastro::wcs::WcsSolution {
            crpix: (f("CRPIX1") - 1.0, f("CRPIX2") - 1.0),
            crval: (f("CRVAL1"), f("CRVAL2")),
            cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
            sip_forward: None,
            sip_reverse: None,
        }
    }

    fn nearest_sky_star() -> (f64, f64, f64) {
        test_fixtures::sky_stars((83.8, -5.4), 0.12, 50.0, 7)
            .into_iter()
            .min_by(|a, b| (a.0 - 83.8).hypot(a.1 + 5.4).total_cmp(&(b.0 - 83.8).hypot(b.1 + 5.4)))
            .unwrap()
    }

    /// Spec §5.8, §5.9, plan 6b-3 D3, D11: a project master lands in
    /// `<output>/<title>/` at the grid's size with the grid's TAN, and its
    /// stars sit where that WCS puts their sky positions — the drizzled
    /// master too, at twice the size. Both state the canonical filter, the
    /// grid's row order, the project and the grid, and claim no plate solve.
    #[test]
    fn a_project_run_writes_grid_masters_with_the_grid_wcs() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let (_ctx, conn, rc, grid) = run_sky_project(tmp.path(), drizzle_2x);
        let masters = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
        let master = masters.iter().find(|m| m.kind == "master").expect("a master");
        let drizzled = masters.iter().find(|m| m.kind == "drizzle").expect("a drizzled master");
        assert!(
            Path::new(&master.path).starts_with(tmp.path().join("out").join("p1")),
            "{}",
            master.path
        );
        assert_eq!((master.width, master.height), (512, 384));
        assert_eq!((drizzled.width, drizzled.height), (1024, 768));
        let star = nearest_sky_star();
        for (path, tol) in [(&master.path, 0.5), (&drizzled.path, 1.0)] {
            let header = crate::fits_parser::FitsHeader::from_path(Path::new(path)).unwrap();
            let wcs = written_wcs(&header);
            let reader = PlaneReader::open(Path::new(path)).unwrap();
            let plane = reader.read_plane(0).unwrap();
            let (x, y) = wcs.sky_to_pixel(star.0, star.1);
            let (mx, my) = centroid_near(&plane, reader.width(), x, y);
            assert!((mx - x).hypot(my - y) < tol, "{path}: star at ({mx}, {my}), the WCS says ({x}, {y})");
            assert_eq!(header.get_str("FILTER").as_deref(), Some("Lum"), "the canonical filter");
            assert_eq!(header.get_str("ROWORDER").as_deref(), Some("BOTTOM-UP"));
            assert_eq!(header.get_str("ATH_RGEO").as_deref(), Some("grid"));
            assert_eq!(header.get_str("ATH_SPRJ").as_deref(), Some("p1"));
            assert_eq!(header.get_str("ATH_GRID").as_deref(), Some(grid.hash().as_str()));
            assert!(header.get_str("PLTSOLVD").is_none(), "a grid TAN is not a plate solve");
        }
        let header = crate::fits_parser::FitsHeader::from_path(Path::new(&master.path)).unwrap();
        assert!((header.get_f64("CRVAL1").unwrap() - 83.8).abs() < 1e-9);
        assert!((header.get_f64("CRPIX1").unwrap() - 256.5).abs() < 1e-9, "1-based centre of 512 px");
        assert!(
            !rc.warnings.iter().any(|w| w.contains("no plate solve on the reference frame")),
            "a grid master always has a WCS: {:?}",
            rc.warnings
        );
    }

    /// Spec §5.1, §5.9 (review I3): no file a project run writes carries
    /// `ATH_PRJ`, and every one carries the scanner's skip marker
    /// (`ATH_STK` or `ATH_REG`).
    #[test]
    fn no_file_a_project_run_writes_carries_ath_prj() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let _run = run_sky_project(tmp.path(), drizzle_2x);
        let mut seen = 0usize;
        for root in [tmp.path().join("work"), tmp.path().join("out")] {
            for entry in walkdir::WalkDir::new(&root).into_iter().filter_map(Result::ok) {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) != Some("fits") {
                    continue;
                }
                let header = crate::fits_parser::FitsHeader::from_path(p).unwrap();
                assert!(header.get_str("ATH_PRJ").is_none(), "{} carries ATH_PRJ", p.display());
                assert!(
                    header.get_str("ATH_STK").is_some() || header.get_str("ATH_REG").is_some(),
                    "{} would be cataloged",
                    p.display()
                );
                seen += 1;
            }
        }
        assert!(seen >= 7, "4 registered frames, the master, the drizzled master, its weight map: {seen}");
    }
```

`stacking/solve.rs`, `mod tests`, after `real_data_solve_probe`:
```rust
    /// Spec §15.3 "WCS": an independent plate solve of a grid master agrees
    /// with the grid's WCS written on it within 0.5 px. Run by the 6b-3
    /// acceptance agent on a real project master.
    #[test]
    #[ignore = "needs ATHENAEUM_GRID_PROBE_MASTER (a grid master FITS) and ATHENAEUM_SOLVE_PROBE_APP_DATA (the .dev app-data dir, for its star catalog)"]
    fn real_data_grid_master_wcs_probe() {
        let master = PathBuf::from(
            std::env::var("ATHENAEUM_GRID_PROBE_MASTER").expect("ATHENAEUM_GRID_PROBE_MASTER"),
        );
        let app_data = std::env::var("ATHENAEUM_SOLVE_PROBE_APP_DATA")
            .expect("ATHENAEUM_SOLVE_PROBE_APP_DATA");
        let header = crate::fits_parser::FitsHeader::from_path(&master).unwrap();
        let f = |k: &str| header.get_f64(k).unwrap_or_else(|| panic!("{k} missing"));
        let written = solvemyastro::wcs::WcsSolution {
            crpix: (f("CRPIX1") - 1.0, f("CRPIX2") - 1.0),
            crval: (f("CRVAL1"), f("CRVAL2")),
            cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
            sip_forward: None,
            sip_reverse: None,
        };
        let (w, h) = {
            let r = PlaneReader::open(&master).unwrap();
            (r.width(), r.height())
        };
        let scale = written.pixel_scale_arcsec();
        let centre = written.pixel_to_sky((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
        let layers = crate::plate_solve::open_catalog_layers(Path::new(&app_data))
            .expect("the .dev star catalog");
        let refs: Vec<&solvemyastro::StarCache> = layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        let config = PlateSolveConfig::default();
        let scratch = tempfile::tempdir().unwrap();
        let go = AtomicBool::new(false);
        let request = SolveRequest {
            handle: 0,
            path: master.clone(),
            hints: SolveHints {
                ra: Some(centre.0),
                dec: Some(centre.1),
                fov_deg: Some(scale * w as f64 / 3600.0),
                rotation: None,
                pixel_scale_arcsec: Some(scale),
            },
            expected_scale_arcsec: Some(scale),
            date_obs: None,
            focal_len: None,
            instrume: None,
            naxis: Some((w as _, h as _)),
            scratch_dir: Some(scratch.path().to_path_buf()),
        };
        let outcome = solve_in_domains(&request, Some(scratch.path()), &go, &mut |r| {
            solve_with(r, &caches, &config, &go)
        });
        let SolveOutcome::Solved(record) = outcome else {
            panic!("the grid master did not solve: {outcome:?}");
        };
        let solved = record.to_solution().unwrap();
        let mut worst = 0f64;
        for i in 1..10 {
            for j in 1..10 {
                let (x, y) = (i as f64 * w as f64 / 10.0, j as f64 * h as f64 / 10.0);
                let (ra, dec) = written.pixel_to_sky(x, y);
                let (sx, sy) = solved.sky_to_pixel(ra, dec);
                worst = worst.max((sx - x).hypot(sy - y));
            }
        }
        println!(
            "grid master wcs probe: {}: worst distance written vs solved {worst:.3} px",
            master.display()
        );
        assert!(worst <= 0.5, "the grid WCS disagrees with an independent solve by {worst} px");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- grid_cards_survive masters_folder_is_its_title grid_masters_with_the_grid_wcs carries_ath_prj --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`stamp_grid_cards`, `subject_output_dir`).

- [ ] **Step 3: Implement**

`stacking/master_cards.rs`:
```rust
/// `ATH_SPRJ`: the collaboration project a master was stacked for (spec
/// §5.9). Never `ATH_PRJ`, which the scanner reads as "a landed replica".
pub const ATH_SPRJ: &str = "ATH_SPRJ";
/// `ATH_GRID`: the canonical grid's hash, 16 hex (spec §6.1).
pub const ATH_GRID: &str = "ATH_GRID";
/// `ATH_RGEO`'s value for a master on a project's canonical grid (spec §5.9).
pub const RGEO_GRID: &str = "grid";

/// What a master on a project's canonical grid states (spec §5.9, plan
/// 6b-3 D3, D11).
pub struct GridStamp<'a> {
    pub project_id: &'a str,
    pub grid_hash: &'a str,
    /// The group's canonical filter; `None` writes no `FILTER` card.
    pub filter: Option<&'a str>,
}

/// A string card that keeps `comment` only when the whole card fits one
/// 80-column record; the value is never shortened (a long one continues).
pub fn card_with_optional_comment(
    keyword: &str,
    value: &str,
    comment: &str,
) -> Result<Card, FitsWriteError> {
    let with = Card::new(keyword, CardValue::Str(value.to_string()))?.with_comment(comment);
    match crate::fits_writer::card::format_card(&with) {
        Ok(_) => Ok(with),
        Err(FitsWriteError::CommentTooLong(_)) => Card::new(keyword, CardValue::Str(value.to_string())),
        Err(e) => Err(e),
    }
}

/// Re-states a grid master's (or drizzled master's) cards (spec §5.9, plan
/// 6b-3 D3, D11): the canonical `FILTER` (the reference's copy-through card
/// carries a publisher's raw name), `ROWORDER = 'BOTTOM-UP'` (the grid's own;
/// never the reference's), `ATH_RGEO = 'grid'`, `ATH_SPRJ`, `ATH_GRID`, and
/// no `PLTSOLVD` (a grid TAN is not a plate solve). Every earlier card of
/// those keys is dropped first, and `ATH_PRJ` defensively too, so each
/// appears once. Variable-length values carry no comment unless it fits.
/// An empty canonical filter writes no `FILTER` at all (review M4): the
/// reference's card is a publisher's raw name and must not leak; the run
/// warns once for such a group.
pub fn stamp_grid_cards(cards: &mut Vec<Card>, stamp: &GridStamp<'_>) -> Result<(), FitsWriteError> {
    cards.retain(|c| {
        !matches!(
            c.keyword.as_str(),
            "FILTER" | "ROWORDER" | "PLTSOLVD" | "ATH_PRJ" | ATH_SPRJ | ATH_GRID | ATH_RGEO
        )
    });
    if let Some(filter) = stamp.filter.map(str::trim).filter(|f| !f.is_empty()) {
        cards.push(Card::new("FILTER", CardValue::Str(filter.to_string()))?);
    }
    cards.push(
        Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".into()))?
            .with_comment("row order of the canonical grid"),
    );
    cards.push(card_with_optional_comment(ATH_SPRJ, stamp.project_id, "collaboration project")?);
    cards.push(
        Card::new(ATH_GRID, CardValue::Str(stamp.grid_hash.to_string()))?
            .with_comment("canonical grid hash"),
    );
    cards.push(
        Card::new(ATH_RGEO, CardValue::Str(RGEO_GRID.into()))?
            .with_comment("registration geometry mode"),
    );
    Ok(())
}
```

`stacking/paths.rs`:
```rust
/// The folder a subject's masters land in (spec §5.9): the output root for
/// a frame set (today's layout), `<output>/<sanitized title>/` for a
/// project. A title that sanitizes to nothing becomes `project`.
pub fn subject_output_dir(output_root: &Path, subject: &StackSubject, label: &str) -> PathBuf {
    match subject {
        StackSubject::FrameSet(_) => output_root.to_path_buf(),
        StackSubject::Project(_) => {
            let name = crate::archive::path_layout::sanitize_for_filename(label);
            output_root.join(if name.is_empty() { "project".to_string() } else { name })
        }
    }
}
```

`stacking/run.rs`:
- `stage_output`: before the group loop, `let solved_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);`. Inside the loop, replace the `let wcs = if included_count_of(rc, &group.key) >= 3 { … }` block's inner part so a grid run never reads a frame's solve:
  ```rust
        let wcs = if included_count_of(rc, &group.key) >= 3 {
            match &rc.grid {
                // Spec §5.9, plan 6b-3 D11: every master of a project run
                // carries the grid's TAN.
                Some(grid) => Some(crate::stacking::grid_map::grid_plate_solve(&grid.grid, &solved_at)),
                None => {
                    // …the existing `solves` lookup, unchanged…
                }
            }
        } else {
            None
        };
  ```
- `process_group_output`: right after `let output_start = Instant::now();`:
  ```rust
    // Spec §5.9, plan 6b-3 D3, D11: what a grid master states — cloned out
    // so the drizzle block below can read it while `rc` is borrowed.
    let grid_stamp: Option<(Arc<crate::stacking::grid_map::GridRun>, String)> = rc
        .grid
        .clone()
        .zip(rc.subject.project_id().map(str::to_string));
  ```
  make `let cards = match build_master_light_cards(…)` `let mut cards`, and after it:
  ```rust
    if grid_stamp.is_some() && group.filter.as_deref().map(str::trim).is_none_or(str::is_empty) {
        // Review M4: a grid master never carries the reference's raw FILTER.
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            "the group's canonical filter is empty; the grid master carries no FILTER"
        );
        rc.warnings.push(format!(
            "group {}: its canonical filter is empty, so the master carries no FILTER card",
            group.key
        ));
    }
    if let Some((grid, project_id)) = &grid_stamp {
        if let Err(e) = stamp_grid_cards(
            &mut cards,
            &GridStamp {
                project_id,
                grid_hash: &grid.hash,
                filter: group.filter.as_deref(),
            },
        ) {
            let outcome = fail_group(rc, group, members.len(), &format!("failed to build the master header: {e}"))?;
            return Ok((outcome, normalize_dur, integrate_dur, Duration::ZERO, output_start.elapsed()));
        }
    }
  ```
  In the drizzle `'attempt` block, make `let drz_cards = match build_drizzle_cards(…)` `let mut drz_cards`, and after it:
  ```rust
                        if let Some((grid, project_id)) = &grid_stamp {
                            if let Err(e) = stamp_grid_cards(
                                &mut drz_cards,
                                &GridStamp {
                                    project_id,
                                    grid_hash: &grid.hash,
                                    filter: group.filter.as_deref(),
                                },
                            ) {
                                break 'attempt Err(DrizzleFailure::Other(e.to_string()));
                            }
                        }
  ```
  (`build_drizzle_cards` keeps the master's non-WCS cards and re-stamps `ATH_RGEO` and the WCS block, hence the second stamp.) Add `stamp_grid_cards, GridStamp` to the `master_cards` import.
- `start_stacking`: `let output_dir = crate::stacking::paths::subject_output_dir(Path::new(&output_dir_str), &subject, &plan.set_name);` replaces `let output_dir = PathBuf::from(&output_dir_str);`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- grid_cards_survive masters_folder_is_its_title grid_masters_with_the_grid_wcs carries_ath_prj stacking::master_cards stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass; every frame-set run and card test unchanged.

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` empty.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/master_cards.rs crates/athenaeum-core/src/stacking/paths.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/solve.rs
git commit -m "feat(stacking): grid masters carry the grid's WCS, the canonical filter and the grid's row order

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 4: The engine's coverage count and the "empty → NaN" option (D4, D5; spec §5.6 seam 1, §5.7)

Every caller passes `empty_as_nan = false` in this task; Task 5a switches it on for a project's LN reference together with masked LN (D5). The coverage count is consumed by Tasks 6a (the map, and the covered rectangle grid statistics are measured on) and 7b2 (the crop).

**Files:**
- Modify `crates/athenaeum-core/src/integration/engine.rs`:
  - `StackParams` (`~752-813`) gains `coverage` and `empty_as_nan`; `StackOutput` (`~820-833`) gains `coverage`;
  - `integrate_stack` (`~841-1611`): the coverage buffer, the per-band post-pass after the `match params.rejection_bits { … }` (`~1484-1571`), the relaxed non-finite guard (`~1576-1580`), the returned field;
  - the 20 `StackParams { … }` literals in its tests get `coverage: false, empty_as_nan: false`;
  - two tests.
- Modify `crates/athenaeum-core/src/stacking/integrate.rs`:
  - `CoverageOptions`; `integrate_planes` (`~570-768`) takes it and fills `StackParams` (`~730-744`);
  - `GroupInput` (`~317-366`) gains `coverage_map`; `GroupOutput` (`~436-463`) gains `coverage`;
  - `integrate_group` (`~879-1430`): both `integrate_planes` calls (`~1082`, `~1193`) pass `CoverageOptions { count: input.coverage_map, empty_as_nan: false }`, the tail keeps plane 0's count, and `GroupOutput { … }` (`~1396`) sets it;
  - its 12 `GroupInput` literals get `coverage_map: false`, its two direct `integrate_planes` test calls pass `CoverageOptions::default()`;
  - one test.
- Modify `crates/athenaeum-core/src/stacking/ln/reference.rs`: `build_reference` gains `empty_as_nan: bool` (passed as `CoverageOptions { count: false, empty_as_nan }`); its 3 test literals and 3 test calls.
- Mechanical: `stacking/run.rs` (2 `GroupInput` literals: `coverage_map: false`; `build_and_write_ln_reference` passes `false` to `build_ln_reference`), `stacking/master_cards.rs` (2 `GroupOutput` test literals: `coverage: None`), `examples/integrate_probe.rs` and `examples/ln_probe.rs` (one `GroupInput` literal each: `coverage_map: false`; `ln_probe`'s `build_reference` call: `false`).

**Interfaces:**
```rust
// integration/engine.rs
pub struct StackParams<'a, 'f> { /* … */ pub coverage: bool, pub empty_as_nan: bool }
pub struct StackOutput { /* … */ pub coverage: Option<Vec<f32>> }
// stacking/integrate.rs
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CoverageOptions { pub count: bool, pub empty_as_nan: bool }
pub(crate) fn integrate_planes<'g>(input: &GroupInput<'_>, frame_indices: &[usize], weights: &[Vec<f32>], output_mode: OutputNormalization, rejection_mode: RejectionNormalization, local_for_output: bool, ln: Option<&'g [Option<LnFrameGrids>]>, rej: Option<&RejBitmapSet>, forced: Option<&RejForcedSource>, recipe: IntegrationRecipe, write_maps: bool, coverage: CoverageOptions, pool: &Arc<rayon::ThreadPool>, cancel: &AtomicBool, progress: &GroupProgress<'_>, io: IoPolicy) -> Result<(Vec<StackOutput>, Vec<Vec<NormalizationPair>>), IntegrationError>;
pub struct GroupInput<'a> { /* … */ pub coverage_map: bool }
pub struct GroupOutput { /* … */ pub coverage: Option<Vec<f32>> }
// stacking/ln/reference.rs
pub fn build_reference(input: &GroupInput<'_>, included: &[usize], n: usize, pool: &Arc<rayon::ThreadPool>, cancel: &AtomicBool, io: IoPolicy, on_progress: &(dyn Fn(usize, usize) + Sync), empty_as_nan: bool) -> Result<LnReference, IntegrationError>;
```
Consumed by: Task 5a (`build_reference(…, true)` on a grid), Task 6a (`GroupInput.coverage_map`, `GroupOutput.coverage`), Task 7b2 (the coverage plane in crop mode).

- [ ] **Step 1: Write the failing tests**

`integration/engine.rs`, `mod tests`, after `stack_path_with_unit_inputs_equals_the_master_path`:
```rust
    /// Plan 6b-3 D4: the coverage count is the number of frames whose RAW
    /// sample is finite — a range-rejected sample still covers its pixel —
    /// and asking for it changes nothing else: data, maps and per-frame
    /// tallies are bit-identical with it off.
    #[test]
    fn coverage_off_leaves_the_stack_bit_identical() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (24usize, 10usize);
        let paths: Vec<_> = (0..5usize)
            .map(|i| {
                write(dir.path(), &format!("f{i}.fits"), w, h, move |x, y| {
                    if x < 2 * i {
                        f32::NAN
                    } else if (x, y, i) == (20, 5, 0) {
                        0.0
                    } else {
                        0.1 + 0.01 * ((x + y + i) % 7) as f32
                    }
                })
            })
            .collect();
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 5];
        let recipe = IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 });
        let run = |coverage: bool| {
            let params = StackParams {
                rejection: &ident, output: &ident, weights: &[1.0; 5],
                range_low: Some(0.0), range_high: None, rejection_maps: true,
                local: None, local_for_rejection: false, local_for_output: false,
                rejection_bits: None, forced_rejection: None,
                coverage, empty_as_nan: false,
            };
            integrate_stack(&src, &params, recipe, &pool(), &AtomicBool::new(false),
                EngineProgress { on_band: &nop(), on_combine: &nop() }, io(1 << 20)).unwrap()
        };
        let off = run(false);
        let on = run(true);
        assert!(off.coverage.is_none());
        for (a, b) in off.base.data.iter().zip(&on.base.data) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        for (a, b) in off.rejection_low.as_ref().unwrap().iter().zip(on.rejection_low.as_ref().unwrap()) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        assert_eq!(off.samples_per_frame, on.samples_per_frame);
        assert_eq!(off.rejected_per_frame, on.rejected_per_frame);
        let cov = on.coverage.unwrap();
        for y in 0..h {
            for x in 0..w {
                let expected = (0..5usize).filter(|&i| x >= 2 * i).count() as f32;
                assert_eq!(cov[y * w + x], expected, "({x}, {y})");
            }
        }
        assert_eq!(cov[5 * w + 20], 5.0, "a range-rejected sample still covers its pixel");
    }

    /// Plan 6b-3 D4 (research fact 1): with `empty_as_nan`, exactly the
    /// pixels no frame covers are NaN; a pixel whose samples were all
    /// range-rejected is covered and stays 0.0; every other pixel is
    /// bit-identical to the run without it.
    #[test]
    fn empty_as_nan_marks_only_uncovered_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (12usize, 6usize);
        let paths: Vec<_> = (0..3usize)
            .map(|i| {
                write(dir.path(), &format!("f{i}.fits"), w, h, move |x, y| {
                    if x < 3 {
                        f32::NAN
                    } else if x == 3 {
                        0.0
                    } else {
                        0.2 + 0.01 * ((x + y + i) % 5) as f32
                    }
                })
            })
            .collect();
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 3];
        let recipe = IntegrationRecipe::average(Rejection::None);
        let run = |empty_as_nan: bool| {
            let params = StackParams {
                rejection: &ident, output: &ident, weights: &[1.0; 3],
                range_low: Some(0.0), range_high: None, rejection_maps: false,
                local: None, local_for_rejection: false, local_for_output: false,
                rejection_bits: None, forced_rejection: None,
                coverage: false, empty_as_nan,
            };
            integrate_stack(&src, &params, recipe, &pool(), &AtomicBool::new(false),
                EngineProgress { on_band: &nop(), on_combine: &nop() }, io(1 << 20)).unwrap()
        };
        let plain = run(false);
        let nan = run(true);
        for y in 0..h {
            for x in 0..w {
                let k = y * w + x;
                let (a, b) = (plain.base.data[k], nan.base.data[k]);
                if x < 3 {
                    assert_eq!(a, 0.0);
                    assert!(b.is_nan(), "({x}, {y}) is covered by no frame");
                } else {
                    assert_eq!(a.to_bits(), b.to_bits(), "({x}, {y})");
                }
            }
        }
        assert_eq!(nan.base.data[3], 0.0, "all samples range-rejected: covered, so 0.0");
        assert!(nan.coverage.is_some(), "empty_as_nan counts");
    }
```

`stacking/integrate.rs`, `mod tests`: first move the fixture of `integrating_a_shifted_brighter_frame_lands_on_the_reference_level` (its constants, the three shifted frames written to `dir`, their measurements, and the `frames` vector) into a helper `fn three_shifted_frames(dir: &Path) -> Vec<StackFrame>` that the existing test calls (a pure move; its assertions are unchanged), then add:
```rust
    /// Plan 6b-3 D4, spec §5.7: `GroupInput.coverage_map` makes the group
    /// carry plane 0's coverage — whole frames covered everywhere inside,
    /// fewer at a shifted frame's edge — and the master is bit-identical
    /// with it off.
    #[test]
    fn the_group_coverage_is_a_side_output() {
        const FW: usize = 400;
        const FH: usize = 300;
        let dir = tempfile::tempdir().unwrap();
        let frames = three_shifted_frames(dir.path());
        let integration = IntegrationConfig::default();
        let normalization = NormalizationConfig::default();
        let make_input = |coverage_map: bool| GroupInput {
            frames: &frames,
            reference: 0,
            width: FW,
            height: FH,
            channels: 1,
            interpolation: Interpolation::BicubicBSpline,
            clamping: 0.3,
            integration: &integration,
            normalization: &normalization,
            ln: None,
            rej: None,
            second_pass_bitmaps: false,
            coverage_map,
        };
        let progress = GroupProgress {
            on_plane: &nop_plane(),
            engine: EngineProgress { on_band: &nop_band(), on_combine: &nop_band() },
        };
        let off = integrate_group(&make_input(false), &MeasureOptions::default(), &pool(), &AtomicBool::new(false), &progress, io(1 << 24)).unwrap();
        let on = integrate_group(&make_input(true), &MeasureOptions::default(), &pool(), &AtomicBool::new(false), &progress, io(1 << 24)).unwrap();
        assert!(off.coverage.is_none());
        for (a, b) in off.data.iter().zip(&on.data) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        let cov = on.coverage.expect("the coverage plane");
        assert_eq!(cov.len(), FW * FH);
        assert!(cov.iter().all(|&c| c == c.trunc() && (0.0..=3.0).contains(&c)));
        assert_eq!(cov[(FH / 2) * FW + FW / 2], 3.0, "the centre is covered by every frame");
        assert!(cov[0] < 3.0, "the (-5, 3)-shifted frame leaves the corner");
    }
```
(Match the field names and helper arguments of the existing `GroupInput` literal in the copied test; `integrate_group`'s signature is the one at `~879`.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- coverage_off_leaves empty_as_nan_marks group_coverage_is_a_side_output --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`coverage`, `empty_as_nan`, `coverage_map`).

- [ ] **Step 3: Implement**

`integration/engine.rs`, `StackParams`, after `forced_rejection`:
```rust
    /// Plan 6b-3 D4: count, per output pixel, the frames whose RAW sample is
    /// finite — geometric coverage, before range and algorithm rejection —
    /// into [`StackOutput::coverage`]. A post-pass over each band's
    /// already-read planes after its combine: the per-pixel combine loop
    /// never sees it, so `false` executes exactly what it did before.
    pub coverage: bool,
    /// Plan 6b-3 D4: a pixel no frame covers is NaN instead of 0.0 (a
    /// project's LN reference on its grid, spec §5.6). Implies the count. A
    /// pixel whose finite samples were all range-rejected is covered and
    /// stays 0.0.
    pub empty_as_nan: bool,
```
`StackOutput`, last field:
```rust
    /// Per output pixel, the frames whose raw sample was finite (`Some`
    /// when [`StackParams::coverage`] or [`StackParams::empty_as_nan`] was
    /// set).
    pub coverage: Option<Vec<f32>>,
```
`integrate_stack`, after `band_bits_buf` is built:
```rust
    // Plan 6b-3 D4: the coverage plane, full-image, filled one band at a time
    // under one lock per band (the rejection maps' own pattern).
    let count_coverage = params.coverage || params.empty_as_nan;
    let coverage_buf =
        std::sync::Mutex::new(if count_coverage { vec![0f32; w * h] } else { Vec::new() });
```
In the band closure, after the whole `match params.rejection_bits { … }` and before `Ok(())`:
```rust
        if count_coverage {
            let mut guard = coverage_buf.lock().unwrap();
            let cov_band = &mut guard[y0 * width..(y0 + rows) * width];
            out_band
                .par_chunks_mut(width)
                .zip(cov_band.par_chunks_mut(width))
                .enumerate()
                .for_each(|(row_in_band, (out_row, cov_row))| {
                    for x in 0..width {
                        let idx = row_in_band * width + x;
                        let covered = (0..n).filter(|&i| planes.sample(i, idx).is_finite()).count();
                        cov_row[x] = covered as f32;
                        if params.empty_as_nan && covered == 0 {
                            out_row[x] = f32::NAN;
                        }
                    }
                });
        }
```
Replace the guard after `band_loop`:
```rust
    let coverage = count_coverage.then(|| coverage_buf.into_inner().unwrap());
    // Every input sample was finiteness-checked above, so a non-finite
    // OUTPUT could only come out of the combiner itself — unreachable by
    // construction — except a pixel `empty_as_nan` left NaN on purpose.
    let uncovered = |k: usize| params.empty_as_nan && coverage.as_ref().is_some_and(|c| c[k] == 0.0);
    if let Some((_, bad)) = out.iter().enumerate().find(|(k, v)| !v.is_finite() && !uncovered(*k)) {
        return Err(IntegrationError::Decode(format!(
            "internal: non-finite value {bad} survived input filtering"
        )));
    }
```
and set `coverage,` in the returned `StackOutput`. Add `coverage: false, empty_as_nan: false` to every `StackParams { … }` literal in the tests.

`stacking/integrate.rs`:
```rust
/// What [`integrate_planes`] counts per output pixel (plan 6b-3 D4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CoverageOptions {
    /// Count plane 0's geometric coverage (one plane: debayered planes share
    /// the footprint; a Bayer mosaic is one plane).
    pub count: bool,
    /// Leave every plane's uncovered pixels NaN (a project's LN reference).
    pub empty_as_nan: bool,
}
```
`integrate_planes` takes `coverage: CoverageOptions` after `write_maps`, and its `StackParams` gets `coverage: coverage.count && p == 0, empty_as_nan: coverage.empty_as_nan,`.
`GroupInput`, last field:
```rust
    /// Plan 6b-3 D4, spec §5.7: keep plane 0's geometric coverage in
    /// [`GroupOutput::coverage`] — the coverage map, and the crop's
    /// rectangle. `false` for every caller that wants neither.
    pub coverage_map: bool,
```
`GroupOutput`, last field:
```rust
    /// Plane 0's per-pixel count of frames with a finite registered sample
    /// (`Some` when [`GroupInput::coverage_map`] was set). The pass whose
    /// output the master is (the second, when large-scale rejection ran)
    /// supplies it; both passes see the same geometry.
    pub coverage: Option<Vec<f32>>,
```
`integrate_group`: both `integrate_planes` calls pass `CoverageOptions { count: input.coverage_map, empty_as_nan: false }` after `input.integration.write_rejection_maps`; before the tail loop `let mut coverage: Option<Vec<f32>> = None;`; inside it, first thing for `p == 0`: `if p == 0 { coverage = out.coverage.clone(); }`; `GroupOutput { …, coverage }`.

`stacking/ln/reference.rs`: `build_reference` gains `empty_as_nan: bool` last; its `integrate_planes` call passes `CoverageOptions { count: false, empty_as_nan }` after `write_maps`; the doc names plan 6b-3 D4/D5 ("a project's LN reference marks the pixels no member covers NaN, so they are never data").

Mechanical edits listed under **Files**.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- coverage_off_leaves empty_as_nan_marks group_coverage_is_a_side_output integration::engine stacking::integrate stacking::ln --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass; every existing engine pin (`stack_path_with_unit_inputs_equals_the_master_path`, `non_finite_samples_are_excluded_not_propagated`, the forced-rejection and band-prefetch pins) unchanged.

- [ ] **Step 5: Run the gates.** The core gate is `--all-targets`, so the two examples compile. `git diff --stat src/types` empty.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/integration/engine.rs crates/athenaeum-core/src/stacking/integrate.rs crates/athenaeum-core/src/stacking/ln/reference.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/master_cards.rs crates/athenaeum-core/examples/integrate_probe.rs crates/athenaeum-core/examples/ln_probe.rs
git commit -m "feat(integration): per-pixel coverage count and an empty-to-NaN option, outside the combine loop

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 5a: LN on the overlap — seam 2, and seam 1 switched on (D5, D6; spec §5.6)

The numeric task of the wave (spec §16). Frame sets keep `LnOverlap::Legacy` everywhere: every LN pin (`ln/mod.rs`, `ln/scale.rs`, the run LN tests, the plan's LN staleness tests, the golden literals) stays green unchanged. A project's LN reference is built with "empty → NaN" (Task 4's option) and every member is fitted on its overlap with it. A member under 25 % overlap returns `LnError::LowOverlap` here; Task 5b turns that into the global fallback (until then the run excludes it like any LN failure, and no project run can start before Task 8a).

**Files:**
- Modify `crates/athenaeum-core/src/stacking/ln/mod.rs`:
  - `LnOverlap`, `LN_MIN_OVERLAP`, `LN_MASK_MARGIN_PX`, `eroded_mask`;
  - `LnError::LowOverlap` (`~172-207`: the enum, `Display`, `source`);
  - `LnReferenceForDetection` gains `overlap` (its one literal, in `build`, sets `Legacy`) and `build_masked`;
  - `normalize_frame` (`~571-958`): the masked branch;
  - tests.
- Modify `crates/athenaeum-core/src/stacking/ln/scale.rs`: `allowed_at`; `PreparedReferenceChannel::build` becomes `build_with(…, allowed: Option<&[bool]>)` plus `build_masked`; `relative_scale_against_core` and `relative_scale_from_seeds_core` take `allowed: Option<&[bool]>` (every existing wrapper passes `None`); `relative_scale_against_masked`, `relative_scale_from_seeds_masked`.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `LN_MASKED_TOKEN`, `ln_registration_upstream`; the per-frame LN check (`~2803-2830`) folds it for a project; one test.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `LN_MASKED_EXTRA_PLANES`, `ln_working_set_bytes_masked` (beside `ln_working_set_bytes`, `~3059`);
  - `build_and_write_ln_reference` (`~10752`): `build_ln_reference(…, empty_as_nan)` with `let empty_as_nan = rc.grid.is_some();` taken before the borrows;
  - `run_group_normalization` (`~9630-10554`): `grid_mode`, the LN upstream for every member (three read sites), `build_masked` on a grid, the masked admission;
  - one test.

**Interfaces:**
```rust
// stacking/ln/mod.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum LnOverlap { Legacy, Masked }
pub const LN_MIN_OVERLAP: f64 = 0.25;
pub const LN_MASK_MARGIN_PX: usize = 48;
pub fn eroded_mask(valid: &[bool], width: usize, height: usize, margin: usize) -> Vec<bool>;
pub enum LnError { TooFewMatches { matches: usize }, Io(std::io::Error), Other(String), LowOverlap { fraction: f64 } }
pub struct LnReferenceForDetection<'a> { pub sanitized_planes: Vec<Cow<'a, [f32]>>, pub locations: Vec<f64>, pub prepared: Vec<scale::PreparedReferenceChannel>, pub overlap: LnOverlap }
impl<'a> LnReferenceForDetection<'a> { pub fn build_masked(reference: &'a LnReference, psf: PsfModel, max_stars: usize, pool: Option<&Arc<rayon::ThreadPool>>) -> LnReferenceForDetection<'a>; }
// stacking/ln/scale.rs
impl PreparedReferenceChannel { pub fn build_masked(reference: &[f32], width: usize, height: usize, psf: PsfModel, max_stars: usize, pool: Option<&Arc<rayon::ThreadPool>>, allowed: &[bool]) -> PreparedReferenceChannel; }
pub fn relative_scale_against_masked(prepared: &PreparedReferenceChannel, target: &[f32], width: usize, height: usize, max_stars: usize, match_radius_px: f64, rcr_limit: f64, local_scale: bool, pool: Option<&Arc<rayon::ThreadPool>>, allowed: &[bool]) -> Result<ScaleResult, LnError>;
pub fn relative_scale_from_seeds_masked(prepared: &PreparedReferenceChannel, fits: &[StarFit], map: &PixelMap, target_plane: &[f32], ref_width: usize, ref_height: usize, max_stars: usize, match_radius_px: f64, rcr_limit: f64, local_scale: bool, pool: Option<&Arc<rayon::ThreadPool>>, allowed: &[bool]) -> Result<ScaleResult, LnError>;
// stacking/plan.rs
pub(crate) const LN_MASKED_TOKEN: &str = "lnmask:1";
pub(crate) fn ln_registration_upstream(registration_hash: &str, masked: bool) -> String;
// stacking/run.rs
pub(crate) const LN_MASKED_EXTRA_PLANES: u64 = 3;
pub(crate) fn ln_working_set_bytes_masked(w: u64, h: u64) -> u64;
```
Consumed by: Task 5b (`LnError::LowOverlap`, `ln_registration_upstream`).

- [ ] **Step 1: Write the failing tests**

`stacking/ln/mod.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D6: erosion by `margin` keeps a pixel only when every
    /// pixel within `margin` of it (inside the image) is valid; the image's
    /// own border is not an edge of the mask.
    #[test]
    fn eroded_mask_shrinks_the_valid_region_by_the_margin() {
        let (w, h) = (20usize, 10usize);
        let valid: Vec<bool> = (0..w * h).map(|k| k % w >= 5).collect();
        let e = eroded_mask(&valid, w, h, 3);
        for y in 0..h {
            for x in 0..w {
                assert_eq!(e[y * w + x], x >= 8, "({x}, {y})");
            }
        }
        assert!(eroded_mask(&vec![true; w * h], w, h, 3).iter().all(|&v| v));
    }

    /// Plan 6b-3 D6, spec §5.6: a member that covers only part of the LN
    /// reference — and a reference that covers only part of the member — is
    /// normalized on their overlap and recovers the truth `R = 1.25·T + 0.03`:
    /// the relative scale within 1 % and the additive term within 0.002 on
    /// every grid node inside the overlap eroded by the 48 px seed margin
    /// (review I2: what a node outside the overlap holds is
    /// `background_grid`'s extrapolation into masked cells, which the method
    /// does not define). Under a quarter of overlap the frame is refused
    /// `LowOverlap` before any fit.
    #[test]
    fn masked_ln_recovers_a_half_covered_members_scale() {
        use crate::fits_writer::write_fits_f32;
        use crate::geometry::{Linear, PixelMap};
        use crate::resample::Interpolation;
        use crate::stacking::measure::FrameMeasurement;
        use crate::stacking::test_fixtures::synthetic_star_field;
        use crate::stacking::weights::FrameWeight;

        const W: usize = 512;
        const H: usize = 384;
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for row in 0..6 {
            for col in 0..10 {
                let x = 26.0 + col as f64 * 46.0 + ((row * 7 + col) % 5) as f64;
                let y = 24.0 + row as f64 * 58.0 + ((row * 3 + col) % 4) as f64;
                stars.push((x, y, 0.10 + ((row * 10 + col) % 9) as f64 * 0.02));
            }
        }
        let field = synthetic_star_field(W, H, &stars, 4.2, 0.002, 11);
        // The reference covers x < 420; the member covers x >= `from`.
        let reference_plane: Vec<f32> = field
            .iter()
            .enumerate()
            .map(|(k, &v)| if k % W < 420 { v } else { f32::NAN })
            .collect();
        let target_of = |from: usize| -> Vec<f32> {
            field
                .iter()
                .enumerate()
                .map(|(k, &v)| if k % W >= from { (v - 0.03) / 1.25 } else { f32::NAN })
                .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let reference = LnReference {
            width: W,
            height: H,
            planes: vec![reference_plane],
            frames_used: Vec::new(),
        };
        let cfg = LocalNormalizationConfig {
            enabled: true,
            scale: 256,
            ..LocalNormalizationConfig::default()
        };
        let measure = crate::stacking::measure::MeasureOptions {
            psf_model: PsfModel::Moffat4,
            max_stars: 200,
            ..Default::default()
        };
        let for_detection =
            LnReferenceForDetection::build_masked(&reference, PsfModel::Moffat4, measure.max_stars, None);
        assert_eq!(for_detection.overlap, LnOverlap::Masked);
        let ref_bg = vec![background_grid(
            &reference.planes[0],
            W,
            H,
            &BackgroundParams { scale: cfg.scale, ..DEFAULT_PARAMS },
            None,
        )];
        let cancel = AtomicBool::new(false);
        let run = |from: usize, name: &str| {
            let path = dir.path().join(format!("{name}.fits"));
            write_fits_f32(&path, W, H, 1, &target_of(from), &[]).unwrap();
            let frame = StackFrame {
                path,
                map: PixelMap::linear(Linear::identity()).unwrap(),
                measurement: FrameMeasurement {
                    width: W,
                    height: H,
                    channels: vec![Default::default()],
                    duration_ms: 0,
                },
                weight: FrameWeight {
                    channels: vec![1.0],
                    normalized: vec![1.0],
                    mean: 1.0,
                    normalized_mean: 1.0,
                    missing: None,
                },
                exposure_s: 180.0,
                date_obs: None,
                frame_id: 1,
                registered_path: None,
            };
            let sidecar = dir.path().join(format!("{name}.athln"));
            normalize_frame(
                &reference,
                &for_detection,
                &ref_bg,
                &frame,
                LnScaleSeeds::ForcedDetection,
                &cfg,
                &measure,
                Interpolation::BicubicBSpline,
                0.3,
                &sidecar,
                None,
                None,
                &cancel,
            )
            .map(|outcome| (outcome, LnFrameGrids::read(&sidecar).unwrap()))
        };

        let (outcome, grids) = run(100, "half").expect("a 78 % overlap normalizes");
        assert!((outcome.scale - 1.25).abs() < 0.0125, "scale {}", outcome.scale);
        // The overlap is x in 100..420; eroded by LN_MASK_MARGIN_PX (48) it is
        // x in 148..372, y in 48..336. Node (i, j) sits at pixel (i, j)·stride.
        let g = &grids.channels[0];
        let stride = g.stride();
        let mut checked = 0usize;
        for j in 0..g.gh {
            for i in 0..g.gw {
                let (x, y) = (i * stride, j * stride);
                if !(148..372).contains(&x) || !(48..336).contains(&y) {
                    continue;
                }
                let b = g.b[j * g.gw + i];
                assert!((b - 0.03).abs() < 0.002, "B node at ({x}, {y}): {b}");
                checked += 1;
            }
        }
        assert!(checked >= 40, "{checked} nodes inside the eroded overlap");

        match run(400, "sliver") {
            Err(LnError::LowOverlap { fraction }) => {
                assert!(fraction < LN_MIN_OVERLAP, "{fraction}");
            }
            other => panic!("a 18 % overlap must be refused LowOverlap, got {other:?}"),
        }
    }
```
(`LnFrameOutcome` derives `Debug`, `ln/mod.rs:219`, which the `panic!` on `other` needs; `LnFrameGrids`/`LnGrid` already round-trip through `read`.)

`stacking/plan.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D6: a frame set's LN upstream is its registration hash,
    /// byte for byte; a project's carries the masked-LN token.
    #[test]
    fn the_ln_upstream_is_unchanged_for_a_frame_set() {
        assert_eq!(ln_registration_upstream("h", false), "h");
        assert_eq!(ln_registration_upstream("h", true), format!("h+{LN_MASKED_TOKEN}"));
    }
```

`stacking/run.rs`, `mod tests`:
```rust
    /// Spec §5.6 seam 1, plan 6b-3 D5: a project's LN reference lies on the
    /// grid with the pixels no member covers NaN (never data), and every
    /// member is normalized on its overlap and cached.
    #[test]
    fn a_project_ln_reference_leaves_uncovered_pixels_nan() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let (_ctx, conn, rc, _grid) =
            run_sky_project(tmp.path(), |cfg| cfg.normalization.local.enabled = true);
        let group = rc.plan_groups[0].key.clone();
        let reference = PlaneReader::open(&rc.layout.ln_reference_path(&group)).unwrap();
        assert_eq!((reference.width(), reference.height()), (512, 384));
        let plane = reference.read_plane(0).unwrap();
        assert!(plane[0].is_nan(), "a corner no member covers is NaN");
        assert!(plane[192 * 512 + 256].is_finite(), "the centre is data");
        for f in &rc.plan_groups[0].frames {
            assert!(
                crate::db::stacking::find_artifact(&conn, &rc.subject, &group, "ln", Some(f.frame_id))
                    .unwrap()
                    .is_some(),
                "{} has its sidecar",
                f.filename
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- eroded_mask masked_ln_recovers ln_upstream_is_unchanged ln_reference_leaves_uncovered --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`eroded_mask`, `build_masked`, `LnError::LowOverlap`, `ln_registration_upstream`).

- [ ] **Step 3: Implement**

`stacking/ln/mod.rs`:
```rust
/// How a group's LN fits treat what a member and the LN reference do not
/// both cover (spec §5.6, plan 6b-3 D6). `Legacy` — every frame set —
/// sanitizes non-finite pixels to the plane's median for detection, exactly
/// as before. `Masked` — a project on its grid — fits each member on its
/// overlap with the reference: detection planes fill the rest with 0.0
/// (the fast detector leaves values ≤ 0 out of its histogram and exact
/// zeros out of its RMS), seeds within [`LN_MASK_MARGIN_PX`] of the
/// overlap's edge are dropped, and both backgrounds are modelled on the
/// overlap only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LnOverlap {
    Legacy,
    Masked,
}

/// Spec §5.6: a member whose overlap with the LN reference is under this
/// fraction of its own footprint keeps global normalization (plan 6b-3 D7).
pub const LN_MIN_OVERLAP: f64 = 0.25;

/// The PSF fit region's hard cap (`psf_signal::fit_one`, `.min(48)`): a
/// seed this far inside the overlap fits on overlap pixels only.
pub const LN_MASK_MARGIN_PX: usize = 48;

/// `valid` eroded by `margin` (Chebyshev): a pixel stays valid when every
/// pixel within `margin` of it, inside the image, is valid. Two separable
/// passes over prefix sums, `O(width · height)`.
pub fn eroded_mask(valid: &[bool], width: usize, height: usize, margin: usize) -> Vec<bool> {
    let mut rows = vec![false; valid.len()];
    let mut prefix = vec![0u32; width.max(height) + 1];
    for y in 0..height {
        let row = &valid[y * width..(y + 1) * width];
        for x in 0..width {
            prefix[x + 1] = prefix[x] + (!row[x]) as u32;
        }
        for x in 0..width {
            let (a, b) = (x.saturating_sub(margin), (x + margin + 1).min(width));
            rows[y * width + x] = prefix[b] - prefix[a] == 0;
        }
    }
    let mut out = vec![false; valid.len()];
    for x in 0..width {
        for y in 0..height {
            prefix[y + 1] = prefix[y] + (!rows[y * width + x]) as u32;
        }
        for y in 0..height {
            let (a, b) = (y.saturating_sub(margin), (y + margin + 1).min(height));
            out[y * width + x] = prefix[b] - prefix[a] == 0;
        }
    }
    out
}
```
`LnError` gains
```rust
    /// Plan 6b-3 D7 (spec §5.6): the member overlaps the LN reference on
    /// less than [`LN_MIN_OVERLAP`] of its own footprint (channel 0). The
    /// run keeps it on its global normalization; it is never an exclusion.
    LowOverlap { fraction: f64 },
```
with `Display` `"local normalization: the frame overlaps the LN reference on {:.0} % of its footprint (< {:.0} %)"` (`fraction * 100.0`, `LN_MIN_OVERLAP * 100.0`) and `LnError::LowOverlap { .. }` in `source`'s `None` arm.

`LnReferenceForDetection` gains `pub overlap: LnOverlap,` (`build` sets `LnOverlap::Legacy`; nothing else in `build` changes), and:
```rust
    /// The `Masked` preparation (plan 6b-3 D6): non-finite reference pixels
    /// — the grid no member covers — become 0.0 for detection, never the
    /// median, and the reference's own stars within [`LN_MASK_MARGIN_PX`]
    /// of its coverage edge are dropped before they are fitted.
    pub fn build_masked(
        reference: &'a LnReference,
        psf: crate::stacking::psf_signal::PsfModel,
        max_stars: usize,
        pool: Option<&Arc<rayon::ThreadPool>>,
    ) -> LnReferenceForDetection<'a> {
        let mut sanitized_planes = Vec::with_capacity(reference.planes.len());
        let mut locations = Vec::with_capacity(reference.planes.len());
        let mut prepared = Vec::with_capacity(reference.planes.len());
        for plane in &reference.planes {
            let finite: Vec<bool> = plane.iter().map(|v| v.is_finite()).collect();
            let sanitized: Cow<'a, [f32]> = if finite.iter().all(|&f| f) {
                Cow::Borrowed(plane.as_slice())
            } else {
                Cow::Owned(plane.iter().map(|&v| if v.is_finite() { v } else { 0.0 }).collect())
            };
            let allowed = eroded_mask(&finite, reference.width, reference.height, LN_MASK_MARGIN_PX);
            prepared.push(scale::PreparedReferenceChannel::build_masked(
                &sanitized,
                reference.width,
                reference.height,
                psf,
                max_stars,
                pool,
                &allowed,
            ));
            locations.push(median_of_finite(plane));
            sanitized_planes.push(sanitized);
        }
        LnReferenceForDetection {
            sanitized_planes,
            locations,
            prepared,
            overlap: LnOverlap::Masked,
        }
    }
```

`normalize_frame`, per channel — right after `band.decode_frame_into(0, &mut target); warp_ms += …;`:
```rust
        // Plan 6b-3 D6 (spec §5.6 seam 2): on a grid, the member's models are
        // fitted on its overlap with the LN reference, never on sanitized NaN.
        // Only the overlap's grid nodes are defined by the fit: a node outside
        // it carries whatever `background_grid` extrapolates into masked
        // cells (on both sides, so it may or may not cancel). Integration
        // never reads it — the member is NaN there — so do not "fix" it.
        let masked = reference_for_detection.overlap == LnOverlap::Masked;
        let mut member_ref_bg: Option<BackgroundGrid> = None;
        let mut allowed: Option<Vec<bool>> = None;
        if masked {
            let ref_plane = &reference.planes[p];
            let mut overlap = vec![false; target.len()];
            let (mut covered, mut both) = (0usize, 0usize);
            for (k, o) in overlap.iter_mut().enumerate() {
                let in_target = target[k].is_finite();
                covered += in_target as usize;
                *o = in_target && ref_plane[k].is_finite();
                both += *o as usize;
            }
            if p == 0 {
                let fraction = if covered == 0 { 0.0 } else { both as f64 / covered as f64 };
                if fraction < LN_MIN_OVERLAP {
                    return Err(LnError::LowOverlap { fraction });
                }
            }
            for (v, &o) in target.iter_mut().zip(&overlap) {
                if !o {
                    *v = f32::NAN;
                }
            }
            let ref_masked: Vec<f32> = ref_plane
                .iter()
                .zip(&overlap)
                .map(|(&v, &o)| if o { v } else { f32::NAN })
                .collect();
            member_ref_bg = Some(background_grid(
                &ref_masked,
                reference.width,
                reference.height,
                &BackgroundParams { scale: cfg.scale, ..DEFAULT_PARAMS },
                pool,
            ));
            allowed = Some(eroded_mask(&overlap, reference.width, reference.height, LN_MASK_MARGIN_PX));
        }
```
The existing `target_bg`/`location_tgt` then run on the (masked) `target` unchanged. Replace the sanitize block's fill with `let fill = if masked { 0.0f32 } else { location_tgt as f32 };` (Legacy's value is exactly the old one). In `fall_back_to_detection`, call `scale::relative_scale_against_masked(…, a)` when `allowed.as_deref()` is `Some(a)`, else the existing `relative_scale_against(…)`; the seeds call likewise chooses `relative_scale_from_seeds_masked(…, a)`. Replace `let ref_bg = &ref_backgrounds[p];` with `let ref_bg = member_ref_bg.as_ref().unwrap_or(&ref_backgrounds[p]);`. The masked reference background uses the run's own reference parameters (`BackgroundParams { scale: cfg.scale, ..DEFAULT_PARAMS }`, as `run_group_normalization` builds `ref_backgrounds`), so its mesh equals the target's and the existing length check holds.

`stacking/ln/scale.rs`:
```rust
/// Whether a position, rounded to its pixel, lies inside `allowed` (plan
/// 6b-3 D6). Out of the image is not allowed.
fn allowed_at(allowed: &[bool], width: usize, x: f64, y: f64) -> bool {
    let (cx, cy) = (x.round(), y.round());
    if !(cx >= 0.0 && cy >= 0.0) || cx as usize >= width {
        return false;
    }
    allowed.get(cy as usize * width + cx as usize).copied().unwrap_or(false)
}
```
- `PreparedReferenceChannel::build` keeps its signature and calls a new private `build_with(…, None)`; `build_with` is today's body with `let mut ref_seeds = detect_seeds(…);` followed by `if let Some(a) = allowed { ref_seeds.retain(|s| allowed_at(a, width, s.x, s.y)); }`; `build_masked(…, allowed)` calls `build_with(…, Some(allowed))`.
- `relative_scale_against_core` gains a last parameter `allowed: Option<&[bool]>`: `let mut tgt_seeds = detect_seeds(…);` then `if let Some(a) = allowed { tgt_seeds.retain(|s| allowed_at(a, width, s.x, s.y)); }`. `relative_scale_against` and `relative_scale_against_with_diag` pass `None`; `relative_scale_against_masked` passes `Some(allowed)` and maps away the diag like `relative_scale_against`.
- `relative_scale_from_seeds_core` gains `allowed: Option<&[bool]>`: right after `let Some((mx, my)) = map_seed_position(…) else { continue; };`, add `if allowed.is_some_and(|a| !allowed_at(a, ref_width, mx, my)) { continue; }` (before `breakdown.in_coverage += 1`, so a masked seed reads as out of coverage). The three existing wrappers pass `None`; `relative_scale_from_seeds_masked` passes `Some(allowed)`.

`stacking/plan.rs`:
```rust
/// The token a project's LN hashes fold beside each frame's registered
/// hash (plan 6b-3 D6): masked LN is a different computation from the
/// frame-set one, so the two never share a sidecar. Bump it when masked
/// LN's arithmetic changes.
pub(crate) const LN_MASKED_TOKEN: &str = "lnmask:1";

/// A frame's registration upstream as every LN hash folds it: a frame
/// set's registration hash unchanged; on a grid, the registered hash plus
/// [`LN_MASKED_TOKEN`].
pub(crate) fn ln_registration_upstream(registration_hash: &str, masked: bool) -> String {
    if masked {
        format!("{registration_hash}+{LN_MASKED_TOKEN}")
    } else {
        registration_hash.to_string()
    }
}
```
In the per-frame LN check: `let frame_registration_hash = ln_registration_upstream(&hash_opt.clone().unwrap_or_default(), project_grid.is_some());`.

`stacking/run.rs`:
- beside `ln_working_set_bytes`:
  ```rust
/// Plan 6b-3 D6: masked LN also holds, per worker, the overlap and eroded
/// masks (a byte per pixel each) and a masked copy of the reference plane
/// (four bytes): counted as three f32 planes, rounded up from 1.5.
pub(crate) const LN_MASKED_EXTRA_PLANES: u64 = 3;

pub(crate) fn ln_working_set_bytes_masked(w: u64, h: u64) -> u64 {
    ln_working_set_bytes(w, h) + LN_MASKED_EXTRA_PLANES * w * h * 4
}
  ```
- `build_and_write_ln_reference`: `let empty_as_nan = rc.grid.is_some();` at its top; `build_ln_reference(input, included, n, pool, cancel, io, &on_build_progress, empty_as_nan)`.
- `run_group_normalization`: `let grid_mode = rc.grid.is_some();` at its top, and right after `members`' measurement hashes are built:
  ```rust
    // Plan 6b-3 D6: what every LN hash folds as a member's registration.
    let member_ln_upstreams: Vec<String> = members
        .iter()
        .map(|m| crate::stacking::plan::ln_registration_upstream(&m.registration_hash, grid_mode))
        .collect();
  ```
  then use `member_ln_upstreams[i].as_str()` at the three reads of `registration_hash` (the reference members' `member_reg_hashes`, the `member_hashes` closure (iterate with `enumerate`), and `calibration_frames`). Choose `LnReferenceForDetection::build_masked(…)` when `grid_mode`, else `build(…)` (same arguments). The admission uses `ln_working_set_bytes_masked` when `grid_mode`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- eroded_mask masked_ln_recovers ln_upstream_is_unchanged ln_reference_leaves_uncovered stacking::ln stacking::plan stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass; every legacy LN test unchanged. If `masked_ln_recovers_a_half_covered_members_scale` misses its tolerance, report the numbers and the matched-star count — do not widen the tolerance (a numeric ruling for the controller).

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` empty.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/ln/mod.rs crates/athenaeum-core/src/stacking/ln/scale.rs crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/stacking/run.rs
git commit -m "feat(stacking): local normalization on the member-reference overlap for project runs

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 5b: The low-overlap fallback, the LN field and its plan twin, one plane rule (D7, D8, D15, D19; spec §5.6)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `run_group_normalization`: `LN_GLOBAL_ARTIFACT_KIND`; read each member's marker beside `existing_ln_rows` (`~10017-10040`); `anything_stale` (`~10106-10111`) and the freshness loop (`~10146-10212`) treat a fresh marker as cached; the fan-out keeps the typed `LnError` (`~10306-10360`); the results loop turns `LowOverlap` into the fallback (`~10427-10510`); one warning per group;
  - `group_ln_field_for` and `member_coverages` (new): the group's LN field on the grid, used by `process_group_output`'s `pick_reference_idx` (`~8086-8105`) and `run_group_normalization`'s coverage mask (`~9688-9698`), whose member LN upstreams also fold the field's hash;
  - `expected_stage_one_planes` (`~2006-2013`) delegates to `plan::stage_one_planes(cfg, group, true)`;
  - one test.
- Modify `crates/athenaeum-core/src/stacking/weights.rs`: `LN_FIELD_PROBES_PER_FRAME_SIDE`, `LnField`, `ln_field`; two tests.
- Modify `crates/athenaeum-core/src/stacking/origin.rs`: `ln_footprint`, `GroupLnField`, `group_ln_field` — the one function the run and the plan both call (review I6).
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `stage_one_planes` replaces `expected_ln_channels` (`~907-925`), called with `prepare_path = project.is_some() || readiness.attested` (`~2674`); `ln_upstream_on_field`; the per-group field hash, and the per-frame LN check folds it and also accepts a fresh marker (`~2796-2840`); one test.
- Modify `crates/athenaeum-core/src/stacking/paths.rs`: `"ln_global"` joins `INTERMEDIATE_ARTIFACT_KINDS` (`~907-915`), with a doc line; update any test that pins the list.

**Interfaces:**
```rust
// stacking/run.rs
pub(crate) const LN_GLOBAL_ARTIFACT_KIND: &str = "ln_global";
fn group_ln_field_for(rc: &RunContext, group: &IntegrationGroup) -> Result<Option<GroupLnField>, RunError>;
fn member_coverages(field: Option<&GroupLnField>, members: &[GroupMember], ref_w: usize, ref_h: usize) -> Vec<f64>;
// stacking/weights.rs
pub(crate) const LN_FIELD_PROBES_PER_FRAME_SIDE: f64 = 32.0;
pub(crate) struct LnField { /* private */ }
pub(crate) fn ln_field(footprints: &[&FrameFootprint], grid: &CanonicalGrid) -> LnField;
impl LnField { pub(crate) fn is_empty(&self) -> bool; pub(crate) fn coverage(&self, fp: Option<&FrameFootprint>) -> f64; }
// stacking/origin.rs
pub(crate) fn ln_footprint(conn: &Connection, origin: &FrameOrigin, frame_id: i64) -> anyhow::Result<Option<FrameFootprint>>;
pub(crate) struct GroupLnField { pub(crate) hash: String, /* private */ }
impl GroupLnField { pub(crate) fn coverage_of(&self, frame_id: i64) -> f64; }
pub(crate) fn group_ln_field(conn: &Connection, origins: &FrameOrigins, in_scope: &[i64], grid: &CanonicalGrid) -> anyhow::Result<GroupLnField>;
// stacking/plan.rs
pub(crate) fn stage_one_planes(cfg: &StackingConfig, group: &IntegrationGroup, prepare_path: bool) -> usize;
pub(crate) fn ln_upstream_on_field(registration_hash: &str, field_hash: &str) -> String;
```
Consumed by: Task 9 (the e2e's fallback assertion).

- [ ] **Step 1: Write the failing tests**

`stacking/weights.rs`, `mod tests`:
```rust
    /// A TAN footprint of `w × h` px at 1.5″/px centred `dx` grid pixels east
    /// of (83.8, −5.4) (east is −x on a north-up grid).
    fn footprint_east(dx: f64, w: f64, h: f64) -> crate::collab::grid_geometry::FrameFootprint {
        let s = 1.5 / 3600.0;
        crate::collab::grid_geometry::FrameFootprint {
            wcs: solvemyastro::wcs::WcsSolution {
                crpix: ((w - 1.0) / 2.0, (h - 1.0) / 2.0),
                crval: (83.8 + dx * s / (-5.4f64).to_radians().cos(), -5.4),
                cd: [[-s, 0.0], [0.0, s]],
                sip_forward: None,
                sip_reverse: None,
            },
            width: w,
            height: h,
        }
    }

    /// Plan 6b-3 D8 (refined by review I6): on a grid wider than every frame
    /// the 0.97 rule is measured against the group's own field — the
    /// lattice probes at least half the members cover, spaced in FRAME units
    /// (a 300-px frame side over 32 ≈ 9.4 px here, whatever the grid's
    /// size) — so the members that share it pass and an offset one fails;
    /// against the whole grid every member would fail.
    #[test]
    fn a_wide_grid_keeps_the_coverage_filter_on_the_groups_own_field() {
        let grid = crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 1000,
            height_px: 400,
            crval1: 83.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        };
        let fps: Vec<_> = [0.0, 2.0, 4.0, 6.0, 200.0].iter().map(|&d| footprint_east(d, 300.0, 400.0)).collect();
        let refs: Vec<&crate::collab::grid_geometry::FrameFootprint> = fps.iter().collect();
        let field = ln_field(&refs, &grid);
        assert!(!field.is_empty());
        for (i, fp) in fps.iter().enumerate().take(4) {
            let c = field.coverage(Some(fp));
            assert!(c >= MIN_REFERENCE_COVERAGE, "member {i}: {c}");
        }
        let offset = field.coverage(Some(&fps[4]));
        assert!(offset < MIN_REFERENCE_COVERAGE, "the offset member: {offset}");
        assert_eq!(field.coverage(None), 0.0, "a frame with no WCS footprint never qualifies");
        assert!(ln_field(&[], &grid).is_empty());
    }

    /// Review I6: one lost probe never fails a member — a footprint that
    /// misses exactly one of a four-probe field still reads 1.0; missing two
    /// reads 0.75.
    #[test]
    fn the_ln_field_forgives_one_lost_probe() {
        let probe = |dra_deg: f64| (83.8 + dra_deg, -5.4);
        let field = LnField {
            sky: vec![probe(0.0), probe(0.004), probe(0.008), probe(0.03)],
            in_field: vec![true; 4],
            n_field: 4,
        };
        // 100 px at 1.5″/px reaches ±0.0208° in RA: three probes inside.
        assert_eq!(field.coverage(Some(&footprint_east(0.0, 100.0, 100.0))), 1.0);
        // 30 px reaches ±0.0063°: two probes inside.
        assert_eq!(field.coverage(Some(&footprint_east(0.0, 30.0, 30.0))), 0.75);
    }
```

`stacking/plan.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D15 (the 6b-2 leftover): one rule for stage 1 and LN — an
    /// `osc__` group on the Prepare path with the debayer off keeps three
    /// planes (and three LN channels); on the Calibrate path a mosaic stays
    /// one plane.
    #[test]
    fn stage_one_planes_follow_the_prepare_rule() {
        let mut cfg = StackingConfig::default();
        cfg.calibration.debayer_osc = false;
        let group = |key: &str, color_mode| IntegrationGroup {
            key: key.into(),
            instrume: None,
            color_mode,
            filter: None,
            binning: 1,
            cameras: Vec::new(),
            exposure_s: Some(300.0),
            exposure_class: "st300".into(),
            frames: Vec::new(),
            total_exposure_s: 0.0,
            pixel_scale_arcsec: None,
        };
        assert_eq!(stage_one_planes(&cfg, &group("osc__None__st300", ColorMode::Osc), true), 3);
        assert_eq!(stage_one_planes(&cfg, &group("cfa__None__st300", ColorMode::Osc), true), 1);
        assert_eq!(stage_one_planes(&cfg, &group("osc__None__bin1__300s", ColorMode::Osc), false), 1);
        assert_eq!(stage_one_planes(&cfg, &group("mono__L__st300", ColorMode::Mono), true), 1);
        cfg.calibration.debayer_osc = true;
        assert_eq!(stage_one_planes(&cfg, &group("osc__None__bin1__300s", ColorMode::Osc), false), 3);
    }
```

`stacking/run.rs`, `mod tests`:
```rust
    /// The fallback fixture: one `L` group of four frames on one field and
    /// two frames `EAST_PAIR_OFFSET_WIDTHS` (0.85) of a frame width east —
    /// their overlap with the other four is about 15 % of their own
    /// footprint — on a grid wide enough for both, with LN on. The star
    /// field is the e2e's own (review I5: the same spacing and seed, so the
    /// strip registration is tried here first, far cheaper than in Task 9).
    fn seed_two_field_project(conn: &rusqlite::Connection, landed: &Path) -> crate::collab::grid::CanonicalGrid {
        crate::db::stacking::insert_test_project(conn, "p1");
        let target = (83.8, -5.4);
        let east = test_fixtures::EAST_PAIR_OFFSET_WIDTHS * 384.0 * 1.5 / 3600.0 / target.1.to_radians().cos();
        let stars = test_fixtures::sky_stars(
            (target.0 + east / 2.0, target.1),
            0.2,
            test_fixtures::EAST_PAIR_SPACING_ARCSEC,
            test_fixtures::EAST_PAIR_STAR_SEED,
        );
        let mut uuids: Vec<(String, (f64, f64))> = (0..4)
            // seed_sky_project's dithers, for the same reason (D8, review I6).
            .map(|i| (format!("a{i}"), (target.0 + (i as f64 - 1.5) * 0.001, target.1 + (i % 2) as f64 * 0.0005)))
            .collect();
        uuids.extend((0..2).map(|i| (format!("b{i}"), (target.0 + east + i as f64 * 0.002, target.1))));
        for (i, (uuid, centre)) in uuids.iter().enumerate() {
            let mut f = test_fixtures::sky_frame(uuid, *centre);
            f.noise_seed = 300 + i as u64;
            test_fixtures::add_project_sky_frame(conn, landed, "p1", &f, &stars);
        }
        crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 768,
            height_px: 320,
            crval1: target.0 + east / 2.0,
            crval2: target.1,
            rotation_deg: 0.0,
        }
    }

    /// Spec §5.6, plan 6b-3 D7, D8: a member whose overlap with the LN
    /// reference is under 25 % keeps global normalization — no sidecar, a
    /// marker, still included — with one warning naming the count; a second
    /// run counts it as cached, and so does the plan. The east pair
    /// registers over its ≈ 15 % strip (review I5: a failure names itself
    /// here, never as a missing warning in Task 9). The plan's Normalize view
    /// moves with the LN field before any run does (review I6: the twin).
    #[test]
    fn a_member_under_a_quarter_overlap_keeps_global_normalization() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_two_field_project(&conn, &tmp.path().join("collab"));
        for (key, dir) in [
            (crate::settings::keys::STACKING_WORKING_DIR, tmp.path().join("work")),
            (crate::settings::keys::STACKING_OUTPUT_DIR, tmp.path().join("out")),
        ] {
            crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
        }
        crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
        let mut cfg = sky_config(&conn);
        cfg.normalization.local.enabled = true;
        crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
            .unwrap();
        let run = |cfg: StackingConfig| {
            let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
            rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
            run_pipeline(&mut rc).unwrap();
            rc
        };
        let rc = run(cfg.clone());
        let group = rc.plan_groups[0].key.clone();
        let entries = &rc.measured[&group];
        for e in entries.iter().filter(|e| e.frame.filename.starts_with("c_b")) {
            let Some(RegisteredFrameOutcome::Aligned { record, .. }) = &e.registration else {
                panic!("{} did not register over its ≈ 15 % strip: {:?}", e.frame.filename, e.reason);
            };
            assert!(
                record.matched_stars >= 4,
                "{} registered on {} matched stars",
                e.frame.filename,
                record.matched_stars
            );
        }
        for e in entries {
            assert!(e.included, "{} stays in the stack: {:?}", e.frame.filename, e.reason);
            let ln = crate::db::stacking::find_artifact(&conn, &rc.subject, &group, "ln", Some(e.frame.frame_id)).unwrap();
            let marker = crate::db::stacking::find_artifact(&conn, &rc.subject, &group, LN_GLOBAL_ARTIFACT_KIND, Some(e.frame.frame_id)).unwrap();
            if e.frame.filename.starts_with("c_b") {
                assert!(ln.is_none() && marker.is_some(), "{} falls back", e.frame.filename);
            } else {
                assert!(ln.is_some() && marker.is_none(), "{} is normalized locally", e.frame.filename);
            }
        }
        assert!(
            rc.warnings.iter().any(|w| w.contains("2 frame(s)") && w.contains("global normalization")),
            "{:?}",
            rc.warnings
        );

        let again = run(cfg);
        for e in again.measured[&group].iter().filter(|e| e.frame.filename.starts_with("c_b")) {
            assert!(e.cached_ln, "{}: the marker is a cache hit", e.frame.filename);
        }
        let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &StackSubject::Project("p1".into()), None).unwrap();
        assert!(!plan.stale_stages.contains(&Stage::Normalize), "{:?}", plan.stale_stages);

        // The twin: excluding a central member moves the field (five frames
        // in scope, three needed), so every LN hash moves — and the plan says
        // so now, not after the next run has re-derived the member list.
        crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &["a1".to_string()], None)
            .unwrap();
        let plan = build_plan(&conn, &ctx.settings, &PathPolicy::AllowAll, &StackSubject::Project("p1".into()), None).unwrap();
        assert!(plan.stale_stages.contains(&Stage::Normalize), "{:?}", plan.stale_stages);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- wide_grid_keeps_the_coverage forgives_one_lost_probe stage_one_planes_follow under_a_quarter_overlap --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`ln_field`, `LnField`, `stage_one_planes`, `LN_GLOBAL_ARTIFACT_KIND`).

- [ ] **Step 3: Implement**

`stacking/weights.rs`:
```rust
/// Probes per frame side on the LN field's lattice (plan 6b-3 D8, review
/// I6): the lattice step is the group's median frame short side, measured
/// on the grid, over this — so a wide grid probes each frame as finely as a
/// narrow one, and a second rig does not coarsen the first.
pub(crate) const LN_FIELD_PROBES_PER_FRAME_SIDE: f64 = 32.0;

/// A group's common field on a project's grid (plan 6b-3 D8, refined by
/// review I6): the lattice probes at least ⌈n/2⌉ of the group's in-scope
/// frames cover, judged from their WCS footprints — data the plan has too,
/// so it can compute the same field ([`crate::stacking::origin::group_ln_field`]).
pub(crate) struct LnField {
    /// Each probe's sky position (degrees).
    sky: Vec<(f64, f64)>,
    in_field: Vec<bool>,
    n_field: usize,
}

/// Whether `fp` covers the sky position `p` (pixel centres at integers, so
/// the frame spans `[-0.5, w - 0.5)`).
fn footprint_covers(fp: &crate::collab::grid_geometry::FrameFootprint, p: (f64, f64)) -> bool {
    let (x, y) = fp.wcs.sky_to_pixel(p.0, p.1);
    x >= -0.5 && x < fp.width - 0.5 && y >= -0.5 && y < fp.height - 0.5
}

/// The field of `footprints` on `grid`. With no footprint, or no probe at
/// least half of them cover, the field is empty: every frame reads 0.0 and
/// the callers drop the filter as they do today.
pub(crate) fn ln_field(
    footprints: &[&crate::collab::grid_geometry::FrameFootprint],
    grid: &crate::collab::grid::CanonicalGrid,
) -> LnField {
    let g = grid.normalized();
    let tan = crate::collab::grid_geometry::grid_wcs(&g);
    let grid_deg = g.scale_arcsec / 3600.0;
    let mut sides: Vec<f64> = footprints
        .iter()
        .map(|fp| {
            let cd = fp.wcs.cd;
            let scale_deg = (cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]).abs().sqrt();
            fp.width.min(fp.height) * scale_deg / grid_deg
        })
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    sides.sort_by(f64::total_cmp);
    let step = sides
        .get(sides.len() / 2)
        .map_or(1.0, |m| (m / LN_FIELD_PROBES_PER_FRAME_SIDE).max(1.0));
    let (gw, gh) = (g.width_px as f64, g.height_px as f64);
    let cols = (gw / step).ceil().max(1.0) as usize;
    let rows = (gh / step).ceil().max(1.0) as usize;
    let mut sky = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        let y = ((r as f64 + 0.5) * step).min(gh - 0.5);
        for c in 0..cols {
            let x = ((c as f64 + 0.5) * step).min(gw - 0.5);
            sky.push(tan.pixel_to_sky(x, y));
        }
    }
    let need = footprints.len().div_ceil(2);
    let in_field: Vec<bool> = sky
        .iter()
        .map(|&p| need > 0 && footprints.iter().filter(|fp| footprint_covers(fp, p)).count() >= need)
        .collect();
    let n_field = in_field.iter().filter(|&&f| f).count();
    LnField { sky, in_field, n_field }
}

impl LnField {
    pub(crate) fn is_empty(&self) -> bool {
        self.n_field == 0
    }

    /// The share of the field `fp` covers, with one probe of grace (review
    /// I6): a frame whose edge cuts one probe of the field still reads full,
    /// so the 0.97 rule never turns on a single probe. 0.0 for a frame with
    /// no footprint, or when the field is empty.
    pub(crate) fn coverage(&self, fp: Option<&crate::collab::grid_geometry::FrameFootprint>) -> f64 {
        let Some(fp) = fp else { return 0.0 };
        if self.n_field == 0 {
            return 0.0;
        }
        let covered = self
            .sky
            .iter()
            .zip(&self.in_field)
            .filter(|(&p, &f)| f && footprint_covers(fp, p))
            .count();
        (covered + 1).min(self.n_field) as f64 / self.n_field as f64
    }
}
```

`stacking/origin.rs`, after `wcs_tier`:
```rust
/// A project frame's footprint for the LN field (plan 6b-3 D8, review I6):
/// its manifest WCS, else the cached solve of its current content, at its
/// manifest size. `None` for a catalog frame, for a frame with neither, and
/// for a manifest without a size.
pub(crate) fn ln_footprint(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
) -> anyhow::Result<Option<crate::collab::grid_geometry::FrameFootprint>> {
    let FrameOrigin::Project { project_id, blake3, meta, .. } = origin else {
        return Ok(None);
    };
    if let Some(fp) = crate::collab::grid_geometry::grid_input_of(meta).footprint {
        return Ok(Some(fp));
    }
    let (Some(w), Some(h)) = (meta.naxis1, meta.naxis2) else {
        return Ok(None);
    };
    if w <= 0 || h <= 0 {
        return Ok(None);
    }
    Ok(match stored_outcome(conn, project_id, frame_id, blake3)? {
        Some(SolveOutcome::Solved(record)) => record.to_solution().map(|wcs| {
            crate::collab::grid_geometry::FrameFootprint {
                wcs,
                width: w as f64,
                height: h as f64,
            }
        }),
        _ => None,
    })
}

/// One group's LN field on a project's grid and its hash (plan 6b-3 D8,
/// review I6). The run and the plan both call this with the group's
/// IN-SCOPE frames — not manually excluded, not unsupported — so the plan
/// computes the field the run will use. Its hash names the frames that pass
/// the 0.97 rule against it, and every member's LN hash folds it
/// (`plan::ln_upstream_on_field`): a field change re-keys the group's LN in
/// the plan and in the run alike, never a run later.
pub(crate) struct GroupLnField {
    pub(crate) hash: String,
    field: crate::stacking::weights::LnField,
    footprints: HashMap<i64, crate::collab::grid_geometry::FrameFootprint>,
}

impl GroupLnField {
    /// A frame's coverage of the field (0.0 for a frame with no footprint).
    pub(crate) fn coverage_of(&self, frame_id: i64) -> f64 {
        self.field.coverage(self.footprints.get(&frame_id))
    }
}

pub(crate) fn group_ln_field(
    conn: &Connection,
    origins: &FrameOrigins,
    in_scope: &[i64],
    grid: &crate::collab::grid::CanonicalGrid,
) -> anyhow::Result<GroupLnField> {
    let mut ids: Vec<i64> = in_scope.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut footprints = HashMap::new();
    for &id in &ids {
        let Some(origin) = origins.get(&id) else { continue };
        if let Some(fp) = ln_footprint(conn, origin, id)? {
            footprints.insert(id, fp);
        }
    }
    let refs: Vec<&crate::collab::grid_geometry::FrameFootprint> =
        ids.iter().filter_map(|id| footprints.get(id)).collect();
    let field = crate::stacking::weights::ln_field(&refs, grid);
    let text = if field.is_empty() {
        "lnfield:1;none".to_string()
    } else {
        let eligible: Vec<String> = ids
            .iter()
            .filter(|id| field.coverage(footprints.get(id)) >= crate::stacking::weights::MIN_REFERENCE_COVERAGE)
            .map(|id| id.to_string())
            .collect();
        format!("lnfield:1;{}", eligible.join(","))
    };
    let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(text.as_bytes()));
    tracing::debug!(count = footprints.len(), "ln field built");
    Ok(GroupLnField { hash, field, footprints })
}
```
(`HashMap`, `Connection`, `stored_outcome`, `SolveOutcome` are imported in `origin.rs` and `FrameOrigins` is defined there, `origin.rs:13-24`; `MIN_REFERENCE_COVERAGE` is `pub(crate)` in `weights.rs:659`.)

`stacking/plan.rs` — replace `expected_ln_channels` with:
```rust
/// The plane count every member of `group` has after stage 1, and so the
/// channel count of its LN reference, `.athln` grids and seeds calibration
/// (plan 6b-3 D15; one rule where 6b-2 had two that disagreed). Calibrate
/// (a frame set): three for a debayered OSC group, one otherwise. Prepare
/// (a project or an attested set, plan 6b-2 C13): one for mono; three for
/// OSC with the debayer on; with it off, three for an `osc__` group
/// (three-plane files used as they are) and one for a `cfa__` group
/// (mosaics kept as mosaics).
pub(crate) fn stage_one_planes(
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    prepare_path: bool,
) -> usize {
    match group.color_mode {
        ColorMode::Mono => 1,
        ColorMode::Osc if cfg.calibration.debayer_osc => 3,
        ColorMode::Osc if prepare_path && group.key.starts_with("osc__") => 3,
        ColorMode::Osc => 1,
    }
}
```
At `~2674`: `let ln_channels = stage_one_planes(&cfg, g, project.is_some() || readiness.attested);` (both bindings are in scope there; borrow `project` as `project.is_some()` before it is moved at the end of `build_plan`).

Beside `ln_registration_upstream`:
```rust
/// A grid member's LN upstream with its group's field hash folded in (plan
/// 6b-3 D8, review I6): the masked-LN token, then `lnfield:<hash>`. The plan
/// and the run both build the field with `origin::group_ln_field`, so a
/// field change moves every member's LN hash on both sides at once.
pub(crate) fn ln_upstream_on_field(registration_hash: &str, field_hash: &str) -> String {
    format!("{}+lnfield:{field_hash}", ln_registration_upstream(registration_hash, true))
}
```
In the per-group loop, beside `ln_reference_info`, the group's field hash when LN runs on a grid (`excluded_set` is the manual list plus the unsupported files, exactly the run's in-scope rule):
```rust
        let ln_field_hash: Option<String> = match project_grid.as_ref() {
            Some(grid) if local_normalization_active => {
                let in_scope: Vec<i64> = g
                    .frames
                    .iter()
                    .map(|f| f.frame_id)
                    .filter(|id| !excluded_set.contains(id))
                    .collect();
                Some(
                    crate::stacking::origin::group_ln_field(conn, &origins, &in_scope, grid)
                        .map_err(|e| ApiError::Internal(format!("{e:#}")))?
                        .hash,
                )
            }
            _ => None,
        };
```
The per-frame LN check: compute the expected hash once and accept either a fresh sidecar or a fresh marker:
```rust
            if local_normalization_active {
                let expected_ln: Option<String> = match (&ln_reference_info, ln_registration_by_frame.get(&f.frame_id)) {
                    (Some(ref_info), Some(hash_opt)) => {
                        let registration = hash_opt.clone().unwrap_or_default();
                        let frame_registration_hash = match &ln_field_hash {
                            Some(field) => ln_upstream_on_field(&registration, field),
                            None => ln_registration_upstream(&registration, project_grid.is_some()),
                        };
                        let frame_measurement_hash = expected_measurement_hash.clone().unwrap_or_default();
                        Some(normalization_hash_for(
                            &cfg,
                            &frame_registration_hash,
                            &frame_measurement_hash,
                            &ref_info.reference_member_ids,
                            &ref_info.reference_hash,
                            ln_calibration.as_ref(),
                            ln_beta_by_group.get(&g.key).copied(),
                        ))
                    }
                    _ => None,
                };
                let ln_fresh = match &expected_ln {
                    None => false,
                    Some(expected) => {
                        find_artifact(conn, subject, &g.key, "ln", Some(f.frame_id))?
                            .is_some_and(|row| is_fresh(&row, expected))
                            // Plan 6b-3 D7: a member that keeps global
                            // normalization (under 25 % overlap) is cached
                            // through its payload-only marker.
                            || find_artifact(conn, subject, &g.key, crate::stacking::run::LN_GLOBAL_ARTIFACT_KIND, Some(f.frame_id))?
                                .is_some_and(|row| row.config_hash == *expected)
                    }
                };
                // …the existing `if ln_fresh { ln_cached += 1 } else if … { normalize_stale = true }` unchanged…
            }
```
(This keeps the M12 and C-17 comments of the block it replaces; carry them over above the new lines.)

`stacking/paths.rs`: add `"ln_global",` after `"ln_reference",` in `INTERMEDIATE_ARTIFACT_KINDS`, with the comment "plan 6b-3 D7: the payload-only marker of a member that keeps global normalization goes with the `.athln` sidecars it stands in for".

`stacking/run.rs`:
- Beside `LN_MASKED_EXTRA_PLANES`:
  ```rust
/// The payload-only marker of a member that keeps global normalization
/// because it overlaps the LN reference on under `ln::LN_MIN_OVERLAP` of
/// its footprint (spec §5.6, plan 6b-3 D7): `config_hash` is the member's LN
/// hash, so the run and the plan count it as cached while nothing upstream
/// moves; payload `{"overlap": <fraction>}`.
pub(crate) const LN_GLOBAL_ARTIFACT_KIND: &str = "ln_global";

/// The group's LN field on a project's grid (plan 6b-3 D8, review I6), over
/// the frames the plan counts in scope — not manually excluded, not
/// unsupported (Prepare's `"unsupported file: …"` exclusion, `~2070`) — so
/// the plan builds the same field and the same hash. `None` for a frame set.
fn group_ln_field_for(
    rc: &RunContext,
    group: &IntegrationGroup,
) -> Result<Option<crate::stacking::origin::GroupLnField>, RunError> {
    let Some(grid) = &rc.grid else {
        return Ok(None);
    };
    let manual: HashSet<i64> = rc.excluded.iter().copied().collect();
    let unsupported: HashSet<i64> = rc
        .runtime_exclusions
        .iter()
        .filter(|(_, why)| why.starts_with("unsupported file: "))
        .map(|(id, _)| *id)
        .collect();
    let in_scope: Vec<i64> = group
        .frames
        .iter()
        .map(|f| f.frame_id)
        .filter(|id| !manual.contains(id) && !unsupported.contains(id))
        .collect();
    let conn = db(&rc.ctx)?.conn();
    crate::stacking::origin::group_ln_field(&conn, &rc.origins, &in_scope, &grid.grid)
        .map(Some)
        .map_err(|e| RunError::Other(format!("{e:#}")))
}

/// Each member's coverage for the anchor pick and the LN member list:
/// against the reference rectangle for a frame set (unchanged), against the
/// group's own field on a project's grid (plan 6b-3 D8).
fn member_coverages(
    field: Option<&crate::stacking::origin::GroupLnField>,
    members: &[GroupMember],
    ref_w: usize,
    ref_h: usize,
) -> Vec<f64> {
    match field {
        Some(field) => members.iter().map(|m| field.coverage_of(m.frame_id)).collect(),
        None => members
            .iter()
            .map(|m| reference_coverage(&m.map, m.measurement.width, m.measurement.height, ref_w, ref_h))
            .collect(),
    }
}
  ```
  In `process_group_output`, before the `pick_reference_idx` closure: `let ln_field = group_ln_field_for(rc, group)?;` (the closure captures it by reference), and in the closure replace the `coverage` computation with `let coverage = member_coverages(ln_field.as_ref(), members, ref_width, ref_height);`. In `run_group_normalization`, right after `grid_mode`: `let ln_field = group_ln_field_for(rc, group)?;`; replace its `coverage` computation the same way, and Task 5a's `member_ln_upstreams` becomes
  ```rust
    let member_ln_upstreams: Vec<String> = members
        .iter()
        .map(|m| match &ln_field {
            Some(field) => crate::stacking::plan::ln_upstream_on_field(&m.registration_hash, &field.hash),
            None => crate::stacking::plan::ln_registration_upstream(&m.registration_hash, grid_mode),
        })
        .collect();
  ```
- `run_group_normalization`:
  - beside `existing_ln_rows`, build `existing_global_rows: Vec<Option<StackingArtifactRow>>` with `find_artifact(…, LN_GLOBAL_ARTIFACT_KIND, Some(m.frame_id))` in the same loop;
  - `anything_stale` becomes `force_fresh || (0..members.len()).any(|i| !(existing_ln_rows[i].as_ref().is_some_and(|row| is_fresh(row, &per_member_hash[i])) || existing_global_rows[i].as_ref().is_some_and(|row| row.config_hash == per_member_hash[i])))`;
  - in the freshness loop's final `else { needs_normalize.push(i); }`, first:
    ```rust
        } else if !force_fresh
            && existing_global_rows[i]
                .as_ref()
                .is_some_and(|row| row.config_hash == *frame_hash)
        {
            set_ln_summary(rc, &group.key, m.frame_id, None, true);
            low_overlap += 1;
        } else {
            needs_normalize.push(i);
        }
    ```
    with `let mut low_overlap = 0usize;` declared before the loop;
  - the fan-out closure returns the `normalize_frame` result unchanged (drop `.map_err(|e| e.to_string())`);
  - the results loop splits the error arm:
    ```rust
            Some(Err(LnError::LowOverlap { fraction })) => {
                let payload = serde_json::json!({ "overlap": fraction }).to_string();
                {
                    let conn = db(&rc.ctx)?.conn();
                    upsert_artifact(
                        &conn,
                        &NewArtifact {
                            subject: &rc.subject,
                            frame_id: Some(frame_id),
                            group_key: &group.key,
                            kind: LN_GLOBAL_ARTIFACT_KIND,
                            path: None,
                            config_hash: &per_member_hash[member_idx],
                            size: None,
                            modified_at: None,
                            payload_json: Some(&payload),
                        },
                    )?;
                }
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id,
                    overlap = fraction,
                    "ln: frame keeps global normalization (little overlap with the LN reference)"
                );
                set_ln_summary(rc, &group.key, frame_id, None, false);
                low_overlap += 1;
            }
            Some(Err(e)) => {
                fail_ln_frame(rc, &mut excluded_frame_ids, frame_id, e.to_string())?;
            }
    ```
    (`e.to_string()` is exactly the text the old `.map_err(|e| e.to_string())` produced, so every other LN failure reads as before; import `LnError` from `crate::stacking::ln`);
  - after the results loop:
    ```rust
    if low_overlap > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = low_overlap,
            "local normalization kept frames with little overlap on global normalization"
        );
        rc.warnings.push(format!(
            "group {}: local normalization kept {low_overlap} frame(s) on global normalization — under {:.0} % overlap with the LN reference",
            group.key,
            ln::LN_MIN_OVERLAP * 100.0
        ));
    }
    ```
  A member that keeps global normalization is absent from `sidecar_grids`, so `GroupInput.ln[i]` is `None` and the engine uses its global pair (`engine.rs:~950-976`); drizzle does the same through `output_pairs` (ruling R-M3-5).
- `expected_stage_one_planes` keeps its name and doc and returns `crate::stacking::plan::stage_one_planes(cfg, group, true)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- wide_grid_keeps_the_coverage forgives_one_lost_probe stage_one_planes_follow under_a_quarter_overlap stacking::ln stacking::plan stacking::paths stacking::origin stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass. If the east pair fails to register (the test's panic names it and its reason), lower `test_fixtures::EAST_PAIR_SPACING_ARCSEC` to 32″ — it is shared with Task 9's e2e, so the e2e inherits the fix — never move the pair closer (under 25 % is the point) and never weaken an assertion; report it.

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` empty.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/weights.rs crates/athenaeum-core/src/stacking/origin.rs crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/stacking/paths.rs
git commit -m "feat(stacking): low-overlap members keep global normalization; the LN field on the grid, twinned in the plan

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 7a: Crop helpers (D10, D4 refinement; spec §5.9)

Runs before Task 6a (review I1): 6a measures a grid master's statistics on `largest_covered_rect`. Nothing here depends on 6a or 6b.

**Files:**
- Create `crates/athenaeum-core/src/stacking/crop.rs`: `CropRect`, `coverage_rect`, `largest_covered_rect`, `union_rects`, `crop_plane`, `restate_crpix`, `crop_file`; tests.
- Modify `crates/athenaeum-core/src/stacking/mod.rs`: `pub mod crop;` (alphabetical, after `pub mod config;`).

No run code changes here: Task 6a uses `largest_covered_rect`, Task 7b2 wires the crop pass.

**Interfaces:**
```rust
// stacking/crop.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect { pub x0: usize, pub y0: usize, pub width: usize, pub height: usize }
impl CropRect {
    pub fn full(width: usize, height: usize) -> Self;
    pub fn is_full(&self, width: usize, height: usize) -> bool;
    pub fn fits_within(&self, width: usize, height: usize) -> bool;
    pub fn union(self, other: CropRect) -> CropRect;
    pub fn scaled(self, s: u32) -> CropRect;
}
pub fn coverage_rect(coverage: &[f32], width: usize, height: usize, k: u32) -> Option<CropRect>;
pub fn largest_covered_rect(coverage: &[f32], width: usize, height: usize, k: u32) -> Option<CropRect>;
pub fn union_rects(rects: impl IntoIterator<Item = CropRect>) -> Option<CropRect>;
pub fn crop_plane(plane: &[f32], width: usize, rect: CropRect) -> Vec<f32>;
pub fn restate_crpix(cards: &mut [Card], cropped: &PlateSolveRecord);
pub fn crop_file(src: &Path, dst: &Path, rect: CropRect, cards: &[Card], format: OutputFormat) -> anyhow::Result<()>;
```
Consumed by: Task 6a (`largest_covered_rect`, `crop_plane`), Tasks 7b1 and 7b2 (the rest).

- [ ] **Step 1: Write the module with its tests**

`crates/athenaeum-core/src/stacking/crop.rs`:
```rust
//! Crop to coverage (spec §5.9, plan 6b-3 D10): the rectangle a run keeps,
//! and the pure operations that apply it to a plane, a header and a file.
//!
//! The rectangle is in array pixels of the master (the grid, for a
//! project). A drizzled output uses the same rectangle `scale` times finer
//! ([`CropRect::scaled`]): every master pixel is `s × s` drizzle pixels, and
//! the drizzle WCS (`scale_plate_solve`) is defined on exactly that lattice.
//! Cropping moves only `CRPIX`: SIP terms are polynomials in
//! `(u, v) = p − CRPIX`, so they hold unchanged, and the WCS refers to array
//! coordinates, so the row order needs no special case.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::fits_writer::{Card, CardValue, OutputFormat, XisfBounds};
use crate::integration::plane_reader::PlaneReader;
use crate::plate_solve::storage::PlateSolveRecord;

/// Columns `x0 .. x0 + width`, rows `y0 .. y0 + height`, in array pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    pub x0: usize,
    pub y0: usize,
    pub width: usize,
    pub height: usize,
}

impl CropRect {
    /// The whole `width × height` image — what an uncropped finalization
    /// "crops" to.
    pub fn full(width: usize, height: usize) -> Self {
        Self { x0: 0, y0: 0, width, height }
    }

    pub fn is_full(&self, width: usize, height: usize) -> bool {
        *self == Self::full(width, height)
    }

    /// Non-empty and inside a `width × height` image.
    pub fn fits_within(&self, width: usize, height: usize) -> bool {
        self.width > 0
            && self.height > 0
            && self.x0 + self.width <= width
            && self.y0 + self.height <= height
    }

    /// The smallest rectangle holding both.
    pub fn union(self, other: CropRect) -> CropRect {
        let x0 = self.x0.min(other.x0);
        let y0 = self.y0.min(other.y0);
        let x1 = (self.x0 + self.width).max(other.x0 + other.width);
        let y1 = (self.y0 + self.height).max(other.y0 + other.height);
        CropRect { x0, y0, width: x1 - x0, height: y1 - y0 }
    }

    /// The same rectangle on an output `s` times finer (drizzle).
    pub fn scaled(self, s: u32) -> CropRect {
        let s = s as usize;
        CropRect {
            x0: self.x0 * s,
            y0: self.y0 * s,
            width: self.width * s,
            height: self.height * s,
        }
    }
}

/// The bounding box of the pixels at least `k` frames cover; `None` when no
/// pixel reaches `k`. A count is a whole number stored as f32, so `>=` is
/// exact; a NaN never qualifies.
pub fn coverage_rect(coverage: &[f32], width: usize, height: usize, k: u32) -> Option<CropRect> {
    debug_assert_eq!(coverage.len(), width * height);
    let k = k as f32;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..height {
        let row = &coverage[y * width..(y + 1) * width];
        let first = row.iter().position(|&c| c >= k);
        let Some(first) = first else { continue };
        let last = row.iter().rposition(|&c| c >= k).unwrap_or(first);
        x0 = x0.min(first);
        x1 = x1.max(last);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    (x0 != usize::MAX).then(|| CropRect { x0, y0, width: x1 - x0 + 1, height: y1 - y0 + 1 })
}

/// The largest-area rectangle whose EVERY pixel at least `k` frames cover
/// (plan 6b-3 review I1: a grid master's statistics are measured on covered
/// pixels only, and `measure_plane` takes a rectangle). The histogram
/// method, one pass per row, `O(width · height)`; of equal areas the first
/// found in row order wins. `None` when no pixel reaches `k` (NaN never
/// does).
pub fn largest_covered_rect(coverage: &[f32], width: usize, height: usize, k: u32) -> Option<CropRect> {
    debug_assert_eq!(coverage.len(), width * height);
    let k = k as f32;
    let mut heights = vec![0usize; width];
    let mut stack: Vec<usize> = Vec::with_capacity(width + 1);
    let mut best: Option<(usize, CropRect)> = None;
    for y in 0..height {
        let row = &coverage[y * width..(y + 1) * width];
        for (x, h) in heights.iter_mut().enumerate() {
            *h = if row[x] >= k { *h + 1 } else { 0 };
        }
        stack.clear();
        for x in 0..=width {
            let current = if x < width { heights[x] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] <= current {
                    break;
                }
                stack.pop();
                let h = heights[top];
                let left = stack.last().map_or(0, |&l| l + 1);
                let area = h * (x - left);
                if best.is_none_or(|(a, _)| area > a) {
                    best = Some((area, CropRect { x0: left, y0: y + 1 - h, width: x - left, height: h }));
                }
            }
            if x < width {
                stack.push(x);
            }
        }
    }
    best.map(|(_, r)| r)
}

/// The union of every rectangle; `None` for none.
pub fn union_rects(rects: impl IntoIterator<Item = CropRect>) -> Option<CropRect> {
    rects.into_iter().reduce(CropRect::union)
}

/// One plane's `rect`, row by row. `rect` must fit the plane.
pub fn crop_plane(plane: &[f32], width: usize, rect: CropRect) -> Vec<f32> {
    let mut out = Vec::with_capacity(rect.width * rect.height);
    for y in rect.y0..rect.y0 + rect.height {
        let start = y * width + rect.x0;
        out.extend_from_slice(&plane[start..start + rect.width]);
    }
    out
}

/// Re-states `CRPIX1`/`CRPIX2` in `cards` from `cropped`
/// ([`crate::stacking::grid_map::crop_plate_solve`] of the record the cards
/// were written from), as exactly the values `wcs_cards` writes for it
/// (1-based). Every other card, its comment and the presence or absence of
/// `PLTSOLVD` (a grid master has none) stay as they were. Cards with no WCS
/// are left alone.
pub fn restate_crpix(cards: &mut [Card], cropped: &PlateSolveRecord) {
    for card in cards.iter_mut() {
        let value = match card.keyword.as_str() {
            "CRPIX1" => cropped.crpix1 + 1.0,
            "CRPIX2" => cropped.crpix2 + 1.0,
            _ => continue,
        };
        card.value = Some(CardValue::Real(value));
    }
}

/// Reads the uncompressed float FITS at `src` (a scratch output), crops
/// every plane to `rect` and writes `dst` in `format` with `cards` — the
/// master writers' own call (`XisfBounds::Unit`). Refuses a rectangle that
/// does not fit the file.
pub fn crop_file(
    src: &Path,
    dst: &Path,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<()> {
    let reader = PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
    let (width, height, channels) = (reader.width(), reader.height(), reader.channels());
    anyhow::ensure!(
        rect.fits_within(width, height),
        "crop {rect:?} does not fit {} ({width} x {height})",
        src.display()
    );
    let mut data = Vec::with_capacity(rect.width * rect.height * channels);
    for plane in 0..channels {
        let full = reader
            .read_plane(plane)
            .with_context(|| format!("reading plane {plane} of {}", src.display()))?;
        data.extend(crop_plane(&full, width, rect));
    }
    crate::fits_writer::write_image_f32(
        dst,
        rect.width,
        rect.height,
        channels,
        &data,
        cards,
        format,
        XisfBounds::Unit,
    )
    .with_context(|| format!("writing {}", dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::wcs::wcs_cards;
    use crate::stacking::grid_map::crop_plate_solve;

    /// Plan 6b-3 D10: the rectangle bounds exactly the pixels at or above
    /// `k`, ignores NaN, and is `None` when nothing reaches `k`.
    #[test]
    fn coverage_rect_bounds_the_pixels_at_or_above_k() {
        let (w, h) = (8, 6);
        let mut cov = vec![0.0f32; w * h];
        for y in 1..5 {
            for x in 2..7 {
                cov[y * w + x] = 2.0;
            }
        }
        cov[2 * w + 3] = 3.0;
        cov[4 * w + 6] = 3.0;
        cov[5 * w + 7] = f32::NAN;
        assert_eq!(coverage_rect(&cov, w, h, 1), Some(CropRect { x0: 2, y0: 1, width: 5, height: 4 }));
        assert_eq!(coverage_rect(&cov, w, h, 2), Some(CropRect { x0: 2, y0: 1, width: 5, height: 4 }));
        assert_eq!(coverage_rect(&cov, w, h, 3), Some(CropRect { x0: 3, y0: 2, width: 4, height: 3 }));
        assert_eq!(coverage_rect(&cov, w, h, 4), None);
    }

    /// Plan 6b-3 D10: the run-wide rectangle is the union; a drizzle
    /// rectangle is the master's `s` times finer; fit checks both edges.
    #[test]
    fn rectangles_unite_scale_and_fit() {
        let a = CropRect { x0: 2, y0: 1, width: 5, height: 4 };
        let b = CropRect { x0: 4, y0: 0, width: 6, height: 2 };
        assert_eq!(union_rects([a, b]), Some(CropRect { x0: 2, y0: 0, width: 8, height: 5 }));
        assert_eq!(union_rects(std::iter::empty()), None);
        assert_eq!(a.scaled(2), CropRect { x0: 4, y0: 2, width: 10, height: 8 });
        assert!(a.fits_within(7, 5) && !a.fits_within(6, 5) && !a.fits_within(7, 4));
        assert!(!CropRect { x0: 0, y0: 0, width: 0, height: 3 }.fits_within(4, 4));
        assert!(CropRect::full(7, 5).is_full(7, 5) && !a.is_full(7, 5));
    }

    /// Review I1: the largest fully covered rectangle holds covered pixels
    /// only — of an L-shaped coverage it keeps the bigger arm, never a corner
    /// nothing covers — and there is none without a covered pixel.
    #[test]
    fn the_largest_covered_rectangle_holds_covered_pixels_only() {
        let (w, h) = (10usize, 8usize);
        let mut cov = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..6 {
                cov[y * w + x] = 2.0;
            }
        }
        for y in 5..8 {
            for x in 6..10 {
                cov[y * w + x] = 1.0;
            }
        }
        let r = largest_covered_rect(&cov, w, h, 1).unwrap();
        assert_eq!(r, CropRect { x0: 0, y0: 0, width: 6, height: 8 }, "48 px beats the 10 × 3 strip");
        for y in r.y0..r.y0 + r.height {
            for x in r.x0..r.x0 + r.width {
                assert!(cov[y * w + x] >= 1.0, "({x}, {y}) is uncovered");
            }
        }
        cov[3 * w + 2] = 0.0;
        let holed = largest_covered_rect(&cov, w, h, 1).unwrap();
        assert!(holed.width * holed.height < 48 && holed.width * holed.height >= 24, "{holed:?}");
        assert_eq!(largest_covered_rect(&vec![0.0; w * h], w, h, 1), None);
        assert_eq!(largest_covered_rect(&vec![f32::NAN; w * h], w, h, 1), None);
    }

    #[test]
    fn crop_plane_takes_the_rectangle_row_by_row() {
        let (w, h) = (5, 4);
        let plane: Vec<f32> = (0..w * h).map(|i| i as f32).collect();
        let out = crop_plane(&plane, w, CropRect { x0: 1, y0: 2, width: 3, height: 2 });
        assert_eq!(out, vec![11.0, 12.0, 13.0, 16.0, 17.0, 18.0]);
    }

    /// Spec §5.9 "CRPIX shifts by the crop origin", plan 6b-3 D10: the
    /// restated header carries exactly the `CRPIX` a header written from the
    /// cropped record carries, with SIP and every other card untouched —
    /// and, on a grid header, still no `PLTSOLVD`.
    #[test]
    fn restate_crpix_moves_only_the_reference_pixel() {
        let mut rec = crate::stacking::test_fixtures::sky_frame_record(
            &crate::stacking::test_fixtures::sky_frame("a", (83.8, -5.4)),
        );
        rec.sip_order = Some(2);
        rec.sip_a_coeffs = Some("[[0.0, 0.0, 1e-6], [0.0, 2e-6, 0.0], [3e-6, 0.0, 0.0]]".into());
        rec.sip_b_coeffs = Some("[[0.0, 0.0, -1e-6], [0.0, 1e-6, 0.0], [2e-6, 0.0, 0.0]]".into());
        let original = wcs_cards(&rec).unwrap();
        let mut cards: Vec<Card> = original.iter().filter(|c| c.keyword != "PLTSOLVD").cloned().collect();
        let cropped = crop_plate_solve(&rec, 40, 25);
        restate_crpix(&mut cards, &cropped);
        let expected = wcs_cards(&cropped).unwrap();
        let value = |cs: &[Card], k: &str| cs.iter().find(|c| c.keyword == k).and_then(|c| c.value.clone());
        for k in ["CRPIX1", "CRPIX2"] {
            assert_eq!(value(&cards, k), value(&expected, k), "{k}");
        }
        for c in &cards {
            if c.keyword != "CRPIX1" && c.keyword != "CRPIX2" {
                assert_eq!(c.value, value(&original, &c.keyword), "{} moved", c.keyword);
            }
        }
        assert!(cards.iter().all(|c| c.keyword != "PLTSOLVD"));
    }

    /// Plan 6b-3 D10: a three-plane scratch FITS crops plane by plane, to
    /// FITS and to XISF.
    #[test]
    fn crop_file_crops_every_plane() {
        let tmp = tempfile::tempdir().unwrap();
        let (w, h) = (6, 5);
        let data: Vec<f32> = (0..3 * w * h).map(|i| i as f32 * 0.01).collect();
        let src = tmp.path().join("scratch.fits");
        crate::fits_writer::write_fits_f32(&src, w, h, 3, &data, &[]).unwrap();
        let rect = CropRect { x0: 1, y0: 2, width: 3, height: 2 };
        let cards = vec![Card::new("BUNIT", CardValue::Str("x".into())).unwrap()];
        let fits = tmp.path().join("out.fits");
        crop_file(&src, &fits, rect, &cards, OutputFormat::Fits).unwrap();
        let reader = PlaneReader::open(&fits).unwrap();
        assert_eq!((reader.width(), reader.height(), reader.channels()), (3, 2, 3));
        for p in 0..3 {
            let expected = crop_plane(&data[p * w * h..(p + 1) * w * h], w, rect);
            assert_eq!(reader.read_plane(p).unwrap(), expected, "plane {p}");
        }
        let xisf = tmp.path().join("out.xisf");
        crop_file(&src, &xisf, rect, &cards, OutputFormat::Xisf).unwrap();
        let (meta, _) = astroimage::ImageConverter::read_raw(&xisf).unwrap();
        assert_eq!((meta.width, meta.height, meta.channels), (3, 2, 3));
        let err = crop_file(&src, &tmp.path().join("bad.fits"), CropRect { x0: 4, y0: 0, width: 3, height: 1 }, &cards, OutputFormat::Fits)
            .unwrap_err();
        assert!(format!("{err:#}").contains("does not fit"), "{err:#}");
    }
}
```

Verified at `20c7c1ef`: `crate::integration::plane_reader::PlaneReader` is public (`run.rs:54` imports it); `OutputFormat` and `XisfBounds` are public in `crate::fits_writer` (`fits_writer/mod.rs:43, 74`); a SIP table is the JSON `Vec<Vec<f64>>` of `order + 1` rows (`fits_writer/wcs.rs:15-30`).

`stacking/mod.rs`: `pub mod crop;`.

- [ ] **Step 2: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- stacking::crop --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: 6 passed.

- [ ] **Step 3: Run the gates.** `git diff --exit-code src/types`.

- [ ] **Step 4: Commit**
```bash
git add crates/athenaeum-core/src/stacking/crop.rs crates/athenaeum-core/src/stacking/mod.rs
git commit -m "feat(stacking): crop helpers — the coverage and largest covered rectangles, the union, plane and file crop, CRPIX restated

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 6a: The coverage map beside the master, and grid statistics on covered pixels (D9, D18, D4 refinement; spec §5.7)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/integrate.rs` (review I1):
  - `GroupInput` gains `stats_on_covered`;
  - `integrate_group` counts plane 0 when `coverage_map || stats_on_covered`, measures every plane's master statistics on `crop::largest_covered_rect(count, w, h, 1)` when `stats_on_covered`, and returns `GroupOutput.coverage` only when `coverage_map` was asked;
  - every `GroupInput` literal Task 4 touched gets `stats_on_covered: false` (the 12 in `integrate.rs`'s tests, the 3 in `ln/reference.rs`'s tests, the one each in `examples/integrate_probe.rs` and `examples/ln_probe.rs`, and `run.rs`'s LN-pass literal at `~8257`).
- Modify `crates/athenaeum-core/src/stacking/master_cards.rs`: `coverage_map_cards`; `WrittenMaster.coverage` (`~477-481`); `write_master_light` (`~680-740`) writes `<stem>_coverage.fits` when `output.coverage` is `Some`; one test.
- Modify `crates/athenaeum-core/src/api/stacking.rs`: `MasterLightKind::Coverage` ↔ `"coverage"` (`~897-923`); add it to the kind round-trip test (`~1838`).
- Modify `crates/athenaeum-core/src/stacking/provenance.rs`: `SummaryGroup.coverage_path` (`~142-205`).
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - the integration `GroupInput` (`~8523`) sets `coverage_map: rc.config.output.coverage_map` and `stats_on_covered: rc.grid.is_some()` (the LN pass's `GroupInput` at `~8257` keeps `false` for both);
  - after `write_master_light`, the `coverage` `master_lights` row (`~8969-8983`, in the same DB block as the master's row);
  - `push_summary_group` (`~7343`) gains `coverage_path: Option<String>`; its three calls (`skip_group`, `fail_group`: `None`; `process_group_output`: the written path);
  - two tests.
- `src/types/stacking.ts`: regenerated — `MasterLightKind` gains `"coverage"`, `SummaryGroup` gains `coveragePath: string | null`.

**Interfaces:**
```rust
// stacking/master_cards.rs
pub fn coverage_map_cards(master_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError>;
pub struct WrittenMaster { pub master: PathBuf, pub rejection_low: Option<PathBuf>, pub rejection_high: Option<PathBuf>, pub coverage: Option<PathBuf> }
// api/stacking.rs
pub enum MasterLightKind { Master, Drizzle, WeightMap, Coverage }
// stacking/provenance.rs
pub struct SummaryGroup { /* … */ pub coverage_path: Option<String> }
// stacking/integrate.rs
pub struct GroupInput<'a> { /* … */ pub stats_on_covered: bool }
```
Consumed by: Task 6b (`SummaryGroup`, `MasterLightKind`), Tasks 7b1 and 7b2 (the coverage plane and its cards).

- [ ] **Step 1: Write the failing tests**

`stacking/master_cards.rs`, `mod tests`:
```rust
    /// Spec §5.7, plan 6b-3 D9: the coverage map counts frames, carries the
    /// master's WCS and its trace cards (grid ones included), and never a
    /// card that would make it read as a light (`FILTER`) or a replica
    /// (`ATH_PRJ`).
    #[test]
    fn the_coverage_map_carries_the_masters_wcs_and_counts_frames() {
        let master = vec![
            Card::new("FILTER", CardValue::Str("L".into())).unwrap(),
            Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".into())).unwrap(),
            Card::new("CRVAL1", CardValue::Real(10.0)).unwrap(),
            Card::new("CD1_1", CardValue::Real(-1e-4)).unwrap(),
            Card::new("ATH_STKI", CardValue::Str("7".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("mono__L__st300".into())).unwrap(),
            Card::new(ATH_RGEO, CardValue::Str(RGEO_GRID.into())).unwrap(),
            Card::new(ATH_GRID, CardValue::Str("0123456789abcdef".into())).unwrap(),
            Card::new(ATH_SPRJ, CardValue::Str("p1".into())).unwrap(),
        ];
        let cards = coverage_map_cards(&master).unwrap();
        let get = |k: &str| cards.iter().find(|c| c.keyword == k).and_then(|c| c.value.clone());
        assert_eq!(get("IMAGETYP"), Some(CardValue::Str("Coverage Map".into())));
        assert_eq!(get("BUNIT"), Some(CardValue::Str("frames".into())));
        assert_eq!(get("ATH_STK"), Some(CardValue::Logical(true)));
        assert_eq!(get("CRVAL1"), Some(CardValue::Real(10.0)));
        assert_eq!(get("CD1_1"), Some(CardValue::Real(-1e-4)));
        for k in ["ROWORDER", "ATH_STKI", "ATH_STKG", ATH_RGEO, ATH_GRID, ATH_SPRJ] {
            assert!(get(k).is_some(), "{k} is copied");
        }
        assert!(get("FILTER").is_none() && get("ATH_PRJ").is_none());
    }
```

`stacking/run.rs`, `mod tests`, after `drizzle_off_leaves_the_master_byte_identical_to_drizzle_on`:
```rust
    /// Spec §5.7, plan 6b-3 D9: the coverage map is a side output — a frame
    /// set's master is bit-identical with it on and off — written as
    /// `<stem>_coverage.fits` (one plane, `BUNIT = 'frames'`) whose counts are
    /// the shift geometry, with its own `master_lights` row and summary path.
    #[test]
    fn a_frame_sets_master_is_bit_identical_with_the_coverage_map_on_and_off() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, _working, _output) = seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT);
        let run_with = |cfg: Option<StackingConfig>| -> (i64, String) {
            let started = start_stacking(
                ctx.clone(),
                Arc::new(NullEmitter),
                &PathPolicy::AllowAll,
                "test".to_string(),
                StackSubject::FrameSet(fixture.set_id),
                cfg,
                None,
            )
            .expect("the run starts");
            wait_for_run(&ctx, started.run_id);
            let row = crate::db::stacking::get_run(&fixture.conn, started.run_id).unwrap().unwrap();
            assert_eq!(row.status, "done", "{row:?}");
            let groups = crate::db::stacking::list_groups(&fixture.conn, started.run_id).unwrap();
            (started.run_id, groups[0].master_path.clone().expect("a master"))
        };
        let (off_run, master_off) = run_with(None);
        let mut cfg_on = StackingConfig::default();
        cfg_on.output.coverage_map = true;
        let (on_run, master_on) = run_with(Some(cfg_on));

        let (reader_off, reader_on) = (
            PlaneReader::open(Path::new(&master_off)).unwrap(),
            PlaneReader::open(Path::new(&master_on)).unwrap(),
        );
        for p in 0..reader_off.channels() {
            let (a, b) = (reader_off.read_plane(p).unwrap(), reader_on.read_plane(p).unwrap());
            for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                assert_eq!(x.to_bits(), y.to_bits(), "plane {p} pixel {i}");
            }
        }
        assert!(
            crate::db::stacking::list_master_lights(&fixture.conn, off_run)
                .unwrap()
                .iter()
                .all(|m| m.kind != "coverage"),
            "no map when it is off"
        );
        let lights = crate::db::stacking::list_master_lights(&fixture.conn, on_run).unwrap();
        let row = lights.iter().find(|m| m.kind == "coverage").expect("a coverage row");
        assert_eq!((row.channels, row.format.as_str()), (1, "fits"));
        let path = PathBuf::from(&row.path);
        assert!(path.to_str().unwrap().ends_with("_coverage.fits"), "{}", row.path);
        let header = crate::fits_parser::FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_str("BUNIT").as_deref(), Some("frames"));
        let reader = PlaneReader::open(&path).unwrap();
        assert_eq!(reader.channels(), 1);
        let (w, h) = (reader.width(), reader.height());
        let cov = reader.read_plane(0).unwrap();
        assert_eq!(cov[(h / 2) * w + w / 2], 4.0, "every frame covers the centre");
        assert!(cov[0] < 4.0, "the shifted frames leave the corner");
        let run_row = crate::db::stacking::get_run(&fixture.conn, on_run).unwrap().unwrap();
        let summary: RunSummary = serde_json::from_str(run_row.summary_json.as_deref().unwrap()).unwrap();
        assert_eq!(summary.groups[0].coverage_path.as_deref(), Some(row.path.as_str()));
    }

    /// Plan 6b-3 D4 refinement (review I1): a grid master's statistics are
    /// measured over covered pixels only, coverage map or not. On a grid
    /// three times the frames' area, three quarters of the plane are the
    /// uncovered 0.0; the master's location still reads the frames' sky
    /// level and its noise about theirs over √4 (resampling lowers it a
    /// little more), and with `coverageMap` off no map is written although
    /// the count was taken.
    #[test]
    fn a_grid_masters_statistics_ignore_uncovered_pixels() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let wide = crate::collab::grid::CanonicalGrid { width_px: 768, height_px: 576, ..grid };
        let mut cfg = sky_config(&conn);
        cfg.output.coverage_map = false;
        crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
            .unwrap();
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(wide)));
        run_pipeline(&mut rc).unwrap();

        let group = &rc.summary.groups[0];
        let stats = group.stats.as_ref().expect("the group's stats");
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let frames: Vec<&crate::stacking::measure::ChannelMeasurement> = rc.measured[&group.key]
            .iter()
            .filter(|e| e.included)
            .filter_map(|e| e.measurement.as_ref())
            .map(|m| &m.channels[0])
            .collect();
        let frame_location = median(frames.iter().map(|c| c.location).collect());
        let frame_noise = median(frames.iter().map(|c| c.noise).collect());
        assert!(
            (stats.master_location[0] - frame_location).abs() < 0.1 * frame_location.abs(),
            "master location {} vs the frames' {frame_location}",
            stats.master_location[0]
        );
        let ratio = stats.master_noise[0] / frame_noise;
        assert!((0.2..0.8).contains(&ratio), "master noise / frame noise = {ratio}");
        assert!(
            crate::db::stacking::list_master_lights(&conn, rc.run_id)
                .unwrap()
                .iter()
                .all(|m| m.kind != "coverage"),
            "the count taken for the statistics writes no map"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- coverage_map_carries bit_identical_with_the_coverage_map grid_masters_statistics_ignore --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`coverage_map_cards`, `coverage_path`, `stats_on_covered`).

- [ ] **Step 3: Implement**

`stacking/master_cards.rs`:
```rust
/// Header cards for a coverage map (spec §5.7, plan 6b-3 D9): `IMAGETYP =
/// 'Coverage Map'`, `ATH_STK`/`ATH_STKV` (the scanner's skip marker),
/// `BUNIT = 'frames'`, the master's own WCS block (the map IS the master's
/// grid, pixel for pixel), and the trace cards a reader needs without the
/// master: `ROWORDER`, `ATH_STKI`, `ATH_STKG`, `ATH_RGEO`, `ATH_GRID`,
/// `ATH_SPRJ`, each only when the master carries it (a frame-set master
/// may carry no `ROWORDER`: the orientation default, `orientation.rs`).
/// Never `FILTER` and never `ATH_PRJ`.
pub fn coverage_map_cards(master_cards: &[Card]) -> Result<Vec<Card>, FitsWriteError> {
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Coverage Map".into()))?,
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
        Card::new("BUNIT", CardValue::Str("frames".into()))?
            .with_comment("frames with a sample here"),
    ];
    for c in master_cards.iter().filter(|c| is_wcs_keyword(&c.keyword)) {
        cards.push(c.clone());
    }
    for kw in ["ROWORDER", "ATH_STKI", "ATH_STKG", ATH_RGEO, ATH_GRID, ATH_SPRJ] {
        if let Some(c) = master_cards.iter().find(|c| c.keyword == kw) {
            cards.push(c.clone());
        }
    }
    Ok(cards)
}
```
`WrittenMaster` gains `pub coverage: Option<PathBuf>,`. In `write_master_light`, after the two rejection maps:
```rust
    // Plan 6b-3 D9 (spec §5.7): the coverage map — always FITS, one plane,
    // the master's WCS — named from the master's resolved stem.
    let coverage = output
        .coverage
        .as_deref()
        .map(|data| -> anyhow::Result<PathBuf> {
            let path = resolve_collision(&dir.join(format!("{stem}_coverage.fits")));
            let map_cards = coverage_map_cards(cards)?;
            write_fits_f32(&path, output.width, output.height, 1, data, &map_cards)
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(path)
        })
        .transpose()?;
```
and return it in `WrittenMaster`. Update the `WrittenMaster` literals in its tests if any.

`api/stacking.rs`: `Coverage` in `MasterLightKind` with `"coverage"` both ways in `as_db_str`/`from_db_str`; its doc names plan 6b-3 D9.

`stacking/provenance.rs`, `SummaryGroup`, after `weight_map_path`:
```rust
    // The group's coverage map beside its master (spec §5.7, plan 6b-3 D9);
    // `None` when `output.coverageMap` is off and on a summary written before
    // 6b-3.
    #[serde(default)]
    pub coverage_path: Option<String>,
```

`stacking/integrate.rs` (review I1):
- `GroupInput`, after `coverage_map`:
  ```rust
    /// Plan 6b-3 D4 refinement (review I1): measure the master's statistics
    /// (`master_location`, `_scale`, `_noise`, `_psf_snr`, `snr_gain`, FWHM,
    /// eccentricity) on covered pixels only — a grid master's uncovered
    /// pixels are 0.0 and would read as data. Implies the plane-0 count.
    /// `false` for a frame set, whose statistics never move.
    pub stats_on_covered: bool,
  ```
- `integrate_group`: both `integrate_planes` calls pass `CoverageOptions { count: input.coverage_map || input.stats_on_covered, empty_as_nan: false }`; before the tail loop, beside Task 4's `let mut coverage`, `let mut measure_rect: Option<crate::stacking::crop::CropRect> = None;`; in the loop, replace the master measurement (`let plane_data = &out.base.data; let cm = pool.install(|| measure_plane(plane_data, …));`) with
  ```rust
        // Plan 6b-3 D4 refinement (review I1): on a grid the statistics come
        // from the largest rectangle every pixel of which at least one frame
        // covers (plane 0's count; the planes share the footprint).
        if p == 0 && input.stats_on_covered {
            measure_rect = coverage
                .as_deref()
                .and_then(|c| crate::stacking::crop::largest_covered_rect(c, input.width, input.height, 1));
            match measure_rect {
                Some(r) => debug!(
                    crop_x0 = r.x0,
                    crop_y0 = r.y0,
                    crop_width = r.width,
                    crop_height = r.height,
                    "master statistics on the covered rectangle"
                ),
                None => warn!("no covered pixel; master statistics on the whole plane"),
            }
        }
        let plane_data = &out.base.data;
        let cm = match measure_rect {
            Some(r) if !r.is_full(input.width, input.height) => {
                let sub = crate::stacking::crop::crop_plane(plane_data, input.width, r);
                pool.install(|| measure_plane(&sub, r.width, r.height, measure, None))
            }
            _ => pool.install(|| measure_plane(plane_data, input.width, input.height, measure, None)),
        };
  ```
  (`debug!`/`warn!` are the `tracing` macros the file already imports.) `GroupOutput { …, coverage: if input.coverage_map { coverage } else { None } }`: the count taken only for the statistics is never a coverage map.
- every `GroupInput` literal listed under **Files** gets `stats_on_covered: false`.

`stacking/run.rs`:
- the integration `GroupInput` (the one handed to `integrate_group`): `coverage_map: rc.config.output.coverage_map,` and `stats_on_covered: rc.grid.is_some(),`;
- in the DB block after `write_master_light` (where the master's row is inserted):
  ```rust
        let coverage_path_str = written.coverage.as_ref().map(|p| p.display().to_string());
        if let Some(coverage) = coverage_path_str.as_deref() {
            insert_master_light(
                &conn,
                &NewMasterLight {
                    subject: &rc.subject,
                    run_id: rc.run_id,
                    group_key: &group.key,
                    kind: MasterLightKind::Coverage.as_db_str(),
                    path: coverage,
                    format: "fits",
                    width: width as i64,
                    height: height as i64,
                    channels: 1,
                    frames: output.stats.included as i64,
                    total_exposure_s,
                },
            )?;
        }
  ```
  (declare `coverage_path_str` before the block so the summary call can use it);
- `push_summary_group` gains a last parameter `coverage_path: Option<String>` stored in `SummaryGroup`; `skip_group` and `fail_group` pass `None`; `process_group_output` passes `coverage_path_str`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- coverage_map_carries bit_identical_with_the_coverage_map grid_masters_statistics_ignore api::stacking stacking::master_cards stacking::integrate stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass; every frame-set statistics pin unchanged (`stats_on_covered` is `false` for a frame set).

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` shows only `src/types/stacking.ts` (`MasterLightKind`, `SummaryGroup.coveragePath`); `npx tsc --noEmit -p .` stays clean (no frontend switch is exhaustive over `MasterLightKind`).

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/master_cards.rs crates/athenaeum-core/src/api/stacking.rs crates/athenaeum-core/src/stacking/provenance.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/integrate.rs crates/athenaeum-core/src/stacking/ln/reference.rs crates/athenaeum-core/examples/integrate_probe.rs crates/athenaeum-core/examples/ln_probe.rs src/types/stacking.ts
git commit -m "feat(stacking): the per-pixel coverage map beside each master; grid-master statistics on covered pixels only

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 6b: Drizzle's own coverage (D9, D18; spec §5.8)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/drizzle/mod.rs`:
  - `DrizzleInput.write_coverage` (`~110-150`), `DrizzleOutput.coverage` (`~187-200`);
  - `estimate_memory_bytes` (`~277-305`) gains `write_coverage: bool` and its two calls (`~669`, the test at `~2438`);
  - `drizzle_group` (`~602-1060`): a per-plane count buffer, the deposit fan-out (`~956-966`) snapshots each band's `W` around `deposit_band` when counting, the plane's counts are kept;
  - `base_input` (test helper, `~1592`) and the other `DrizzleInput { … }` literals get `write_coverage: false`; one test.
- Modify `crates/athenaeum-core/src/stacking/master_cards.rs`: `WrittenDrizzle.coverage` (`~555-558`); `write_drizzled_master` (`~588-661`) writes `<drizzle stem>_coverage.fits` (FITS, one plane per channel, `coverage_map_cards` of the drizzle cards); the two `DrizzleOutput` test literals get `coverage: None`.
- Modify `crates/athenaeum-core/src/api/stacking.rs`: `MasterLightKind::DrizzleCoverage` ↔ `"drizzle_coverage"`; the round-trip test.
- Modify `crates/athenaeum-core/src/stacking/provenance.rs`: `SummaryGroup.drizzle_coverage_path`.
- Modify `crates/athenaeum-core/src/stacking/run.rs`: `DrizzleInput { …, write_coverage: rc.config.output.coverage_map }` (`~9265`); the `drizzle_coverage` row beside the weight map's (`~9414-9430`); `push_summary_group` gains `drizzle_coverage_path` (three calls).
- `examples/drizzle_probe.rs`: its `DrizzleInput` literal gets `write_coverage: false`.
- `src/types/stacking.ts`: regenerated — `MasterLightKind` gains `"drizzleCoverage"`, `SummaryGroup` gains `drizzleCoveragePath: string | null`.

**Interfaces:**
```rust
// stacking/drizzle/mod.rs
pub struct DrizzleInput<'a> { /* … */ pub write_coverage: bool }
pub struct DrizzleOutput { /* … */ pub coverage: Option<Vec<f32>> }
pub fn estimate_memory_bytes(width: usize, height: usize, channels: usize, scale: u32, ln: bool, write_weight_map: bool, use_rejection: bool, write_coverage: bool) -> u64;
// stacking/master_cards.rs
pub struct WrittenDrizzle { pub drizzle: PathBuf, pub weight_map: Option<PathBuf>, pub coverage: Option<PathBuf> }
// api/stacking.rs
pub enum MasterLightKind { Master, Drizzle, WeightMap, Coverage, DrizzleCoverage }
// stacking/provenance.rs
pub struct SummaryGroup { /* … */ pub drizzle_coverage_path: Option<String> }
```
Consumed by: Tasks 7b1 and 7b2 (the drizzle coverage file in crop mode).

- [ ] **Step 1: Write the failing tests**

`stacking/drizzle/mod.rs`, `mod tests`:
```rust
    /// Spec §5.8, plan 6b-3 D9: drizzle's own coverage counts, per output
    /// pixel, the frames whose deposit reached it — three frames, one moved
    /// 10 px, leave a strip of 20 output pixels per row covered by two — and
    /// asking for it changes no output byte; its memory is in the estimate.
    #[test]
    fn drizzle_output_is_bit_identical_with_coverage_on_and_off() {
        let dir = tempfile::tempdir().unwrap();
        let data = uniform(W, H, 0.25);
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| write_mono(dir.path(), &format!("f{i}.fits"), W, H, &data))
            .collect();
        let still = identity_map();
        let moved = translation_map(10.0, 0.0);
        let weight = [1.0f64];
        let pair = identity_pair();
        let frames = [
            frame(&paths[0], &still, &weight, &pair),
            frame(&paths[1], &still, &weight, &pair),
            frame(&paths[2], &moved, &weight, &pair),
        ];
        let measure = MeasureOptions::default();
        let mut input = base_input(&frames, &measure);
        let off = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        input.write_coverage = true;
        let on = drizzle_group(&input, &pool(), &AtomicBool::new(false), &no_progress()).unwrap();
        assert!(off.coverage.is_none());
        for (a, b) in off.data.iter().zip(&on.data) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        for (a, b) in off.weight.as_ref().unwrap().iter().zip(on.weight.as_ref().unwrap()) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        let cov = on.coverage.expect("the coverage plane");
        let out_w = W * 2;
        let row = &cov[H * out_w..(H + 1) * out_w];
        assert_eq!(row[out_w / 2], 3.0, "the centre is under every frame");
        let two = row.iter().filter(|&&c| c == 2.0).count();
        assert!((16..=24).contains(&two), "the moved frame's strip: {two} px");
        assert!(row.iter().all(|&c| c <= 3.0));
        let extra = estimate_memory_bytes(W, H, 1, 2, false, true, false, true)
            - estimate_memory_bytes(W, H, 1, 2, false, true, false, false);
        assert_eq!(extra, 2 * (W as u64 * 2) * (H as u64 * 2) * 4, "the kept planes and the counting plane");
    }
```

`stacking/run.rs`, `mod tests`, after `a_frame_sets_master_is_bit_identical_with_the_coverage_map_on_and_off` — the drizzle twin, on the same fixture with `cfg.drizzle.enabled = true; cfg.drizzle.scale = 2;` in both runs and `output.coverage_map` off then on:
```rust
    /// Spec §5.8, plan 6b-3 D9: with the coverage map on, drizzle writes its
    /// own coverage beside its weight map (kind `drizzle_coverage`, one plane
    /// per channel, the drizzle's size), and the drizzled master is
    /// bit-identical to the run without it.
    #[test]
    fn drizzle_writes_its_own_coverage_beside_the_weight_map() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let (fixture, light_ids, _working, _output) = seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4]);
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT);
        let run_with = |coverage_map: bool| -> i64 {
            let mut cfg = StackingConfig::default();
            cfg.drizzle.enabled = true;
            cfg.drizzle.scale = 2;
            cfg.output.coverage_map = coverage_map;
            let started = start_stacking(
                ctx.clone(),
                Arc::new(NullEmitter),
                &PathPolicy::AllowAll,
                "test".to_string(),
                StackSubject::FrameSet(fixture.set_id),
                Some(cfg),
                None,
            )
            .unwrap();
            wait_for_run(&ctx, started.run_id);
            started.run_id
        };
        let (off, on) = (run_with(false), run_with(true));
        let lights = |run: i64| crate::db::stacking::list_master_lights(&fixture.conn, run).unwrap();
        let drizzled = |run: i64| lights(run).into_iter().find(|m| m.kind == "drizzle").unwrap();
        let (a, b) = (
            PlaneReader::open(Path::new(&drizzled(off).path)).unwrap().read_plane(0).unwrap(),
            PlaneReader::open(Path::new(&drizzled(on).path)).unwrap().read_plane(0).unwrap(),
        );
        assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
        assert!(lights(off).iter().all(|m| m.kind != "drizzle_coverage"));
        let row = lights(on).into_iter().find(|m| m.kind == "drizzle_coverage").expect("a drizzle coverage row");
        let d = drizzled(on);
        assert_eq!((row.width, row.height, row.channels), (d.width, d.height, d.channels));
        assert!(row.path.ends_with("_coverage.fits"), "{}", row.path);
        let cov = PlaneReader::open(Path::new(&row.path)).unwrap().read_plane(0).unwrap();
        assert!(cov.iter().any(|&c| c == 4.0), "the interior is under every frame");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- bit_identical_with_coverage_on_and_off drizzle_writes_its_own_coverage --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`write_coverage`, `coverage`).

- [ ] **Step 3: Implement**

`stacking/drizzle/mod.rs`:
- `DrizzleInput`, after `write_weight_map`:
  ```rust
    /// Plan 6b-3 D9 (spec §5.8): count, per output pixel and plane, the
    /// frames whose deposit raised `W` there, into [`DrizzleOutput::coverage`].
    /// Counted by comparing each band's `W` before and after the frame's
    /// deposit, so `deposit_band` is untouched; a drop whose weight is below
    /// `W`'s f32 resolution at that pixel is not counted.
    pub write_coverage: bool,
  ```
- `DrizzleOutput`, after `weight`: `/// Same layout as `data`, present only when [`DrizzleInput::write_coverage`] was set. pub coverage: Option<Vec<f32>>,`
- `estimate_memory_bytes` gains `write_coverage: bool` last, and before `need` is returned:
  ```rust
    if write_coverage {
        // Plan 6b-3 D9: the kept count planes plus the one being counted.
        need += (channels as u64 + 1) * out_w * out_h * 4;
    }
  ```
  `drizzle_group` passes `input.write_coverage`.
- `drizzle_group`: beside `weight_all`, `let mut coverage_all = input.write_coverage.then(|| vec![0f32; plane_out_pixels * input.channels]);`; per plane beside `i_buf`/`w_buf`, `let mut cov_buf = if input.write_coverage { vec![0f32; plane_out_pixels] } else { Vec::new() };`; replace the deposit fan-out with
  ```rust
            pool.install(|| {
                if input.write_coverage {
                    i_buf
                        .par_chunks_mut(DRIZZLE_BAND_ROWS * out_w)
                        .zip(w_buf.par_chunks_mut(DRIZZLE_BAND_ROWS * out_w))
                        .zip(cov_buf.par_chunks_mut(DRIZZLE_BAND_ROWS * out_w))
                        .enumerate()
                        .for_each(|(band_idx, ((ib, wb), cb))| {
                            // One band-sized copy per (band, frame, plane) while the
                            // map is asked for (review M18): opt-in, so left as is;
                            // a per-worker buffer is the lever if a profile ever
                            // shows it.
                            let before = wb.to_vec();
                            deposit_band(ib, wb, band_idx, out_w, &ctx);
                            for ((c, &after), &prior) in cb.iter_mut().zip(wb.iter()).zip(&before) {
                                if after > prior {
                                    *c += 1.0;
                                }
                            }
                        });
                } else {
                    i_buf
                        .par_chunks_mut(DRIZZLE_BAND_ROWS * out_w)
                        .zip(w_buf.par_chunks_mut(DRIZZLE_BAND_ROWS * out_w))
                        .enumerate()
                        .for_each(|(band_idx, (ib, wb))| {
                            deposit_band(ib, wb, band_idx, out_w, &ctx);
                        });
                }
            });
  ```
  after the plane's frame loop: `if let Some(all) = coverage_all.as_mut() { all[c * plane_out_pixels..(c + 1) * plane_out_pixels].copy_from_slice(&cov_buf); }`; `DrizzleOutput { …, coverage: coverage_all }`.

`stacking/master_cards.rs`: `WrittenDrizzle` gains `pub coverage: Option<PathBuf>`; in `write_drizzled_master`, after the weight map:
```rust
    // Plan 6b-3 D9 (spec §5.8): drizzle's own coverage — always FITS, one
    // plane per channel, the drizzled master's WCS.
    let coverage = match output.coverage.as_deref() {
        Some(data) => {
            let resolved_stem = drizzle_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("drizzle")
                .to_string();
            let path = resolve_collision(&dir.join(format!("{resolved_stem}_coverage.fits")));
            let map_cards = coverage_map_cards(cards)?;
            write_fits_f32(&path, output.width, output.height, output.channels, data, &map_cards)
                .with_context(|| format!("writing {}", path.display()))?;
            Some(path)
        }
        None => None,
    };
```

`api/stacking.rs`: `DrizzleCoverage` ↔ `"drizzle_coverage"` (wire `drizzleCoverage`).

`stacking/provenance.rs`, `SummaryGroup`, after `coverage_path`:
```rust
    // Drizzle's own coverage beside its weight map (spec §5.8, plan 6b-3 D9).
    #[serde(default)]
    pub drizzle_coverage_path: Option<String>,
```

`stacking/run.rs`: `write_coverage: rc.config.output.coverage_map,` in the `DrizzleInput`; in the drizzle DB block, after the weight-map row:
```rust
                        if let Some(coverage) = drizzle_coverage_path_str.as_deref() {
                            insert_master_light(
                                &conn,
                                &NewMasterLight {
                                    subject: &rc.subject,
                                    run_id: rc.run_id,
                                    group_key: &group.key,
                                    kind: MasterLightKind::DrizzleCoverage.as_db_str(),
                                    path: coverage,
                                    format: "fits",
                                    width: stats.out_width as i64,
                                    height: stats.out_height as i64,
                                    channels: channels as i64,
                                    frames: output.stats.included as i64,
                                    total_exposure_s,
                                },
                            )?;
                        }
```
where `drizzle_coverage_path_str` is `wd.coverage.as_ref().map(|p| p.display().to_string())`, taken beside `weight_map_path_str` (`~9374`) and moved into a `let mut drizzle_coverage_path: Option<String> = None;` declared beside `weight_map_path` (`~9038`) where `weight_map_path = weight_map_path_str;` is set (`~9460`). `push_summary_group` gains a last parameter `drizzle_coverage_path: Option<String>` (after 6a's `coverage_path`): `None` from `skip_group`/`fail_group`, `drizzle_coverage_path` from `process_group_output`.

`db/stacking.rs`: the `MasterLight.kind` doc becomes `master | drizzle | weight_map | coverage | drizzle_coverage`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- bit_identical_with_coverage_on_and_off drizzle_writes_its_own_coverage stacking::drizzle api::stacking stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass, every existing drizzle pin unchanged.

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` shows only `src/types/stacking.ts`.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/drizzle/mod.rs crates/athenaeum-core/src/stacking/master_cards.rs crates/athenaeum-core/src/api/stacking.rs crates/athenaeum-core/src/stacking/provenance.rs crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/db/stacking.rs crates/athenaeum-core/examples/drizzle_probe.rs src/types/stacking.ts
git commit -m "feat(stacking): drizzle writes its own coverage beside the weight map

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 7b1: The crop writers (D10; spec §5.9; first half of the review's crop split)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/paths.rs`: `WorkingLayout::uncropped_dir(run_id)`; `EstimateInputs.crop_scratch` and the estimate's doubled output terms (review M6); one test.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `build_plan`'s `EstimateInputs` literal (`~2496`) sets `crop_scratch`.
- Modify `crates/athenaeum-core/src/stacking/master_cards.rs`: `#[derive(Debug, Clone)]` on `WrittenMaster` and `WrittenDrizzle`; `write_cropped_master`, `write_cropped_drizzle` and their private helpers; one test (review I7: the full-rectangle finalization against the K = 0 writer).

No run code changes: `pub fn`s in a `pub mod` raise no dead-code warning, so the lib check stays clean until Task 7b2 calls them.

**Interfaces:**
```rust
// stacking/paths.rs
impl WorkingLayout { pub fn uncropped_dir(&self, run_id: i64) -> PathBuf; } // root/runs/run-<id>-uncropped
pub struct EstimateInputs<'a> { /* … */ pub crop_scratch: bool }
// stacking/master_cards.rs
pub fn write_cropped_master(dir: &Path, scratch: &WrittenMaster, rect: CropRect, cards: &[Card], format: OutputFormat) -> anyhow::Result<WrittenMaster>;
pub fn write_cropped_drizzle(dir: &Path, master_stem: &str, scale: u32, scratch: &WrittenDrizzle, rect: CropRect, cards: &[Card], format: OutputFormat) -> anyhow::Result<WrittenDrizzle>;
```
Consumed by: Task 7b2 (the run's crop pass).

- [ ] **Step 1: Write the failing tests**

`stacking/master_cards.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D10, review I7: finalizing a scratch family with the FULL
    /// rectangle writes what the K = 0 writer writes — the same name, the
    /// same pixels and keywords (XISF is not byte-reproducible: it stamps its
    /// creation time) and byte-identical FITS maps — and a real crop takes
    /// every member of the family to the rectangle, its `CRPIX` moved.
    #[test]
    fn a_full_rectangle_crop_writes_what_the_master_writer_writes() {
        let (w, h, ch) = (8usize, 6usize, 3usize);
        let plane = w * h;
        let mut cards = HeaderBuilder::new(FrameKind::MasterLight).build().unwrap();
        cards.push(Card::new("ATH_STKI", CardValue::Str("run-7".into())).unwrap());
        cards.extend(wcs_cards(&record(false)).unwrap());
        let output = GroupOutput {
            width: w,
            height: h,
            channels: ch,
            data: (0..plane * ch).map(|i| i as f32 / 1000.0).collect(),
            rejection_low: Some(vec![1.0f32; plane * ch]),
            rejection_high: Some(vec![2.0f32; plane * ch]),
            coverage: Some((0..plane).map(|i| (i % 4) as f32).collect()),
            included: vec![0, 1, 2],
            output_pairs: vec![Vec::new(); ch],
            second_pass_rej_ok: false,
            stats: dummy_stats(),
        };
        let (direct, scratch, finals, cut) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let k0 = write_master_light(direct.path(), "master", &output, &cards, OutputFormat::Xisf).unwrap();
        let staged = write_master_light(scratch.path(), "master", &output, &cards, OutputFormat::Fits).unwrap();
        let full = write_cropped_master(finals.path(), &staged, CropRect::full(w, h), &cards, OutputFormat::Xisf).unwrap();
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name(&full.master), name(&k0.master));
        let (ma, pa) = astroimage::ImageConverter::read_raw(&k0.master).unwrap();
        let (mb, pb) = astroimage::ImageConverter::read_raw(&full.master).unwrap();
        assert_eq!((ma.width, ma.height, ma.channels), (mb.width, mb.height, mb.channels));
        match (pa, pb) {
            (astroimage::PixelData::Float32(a), astroimage::PixelData::Float32(b)) => {
                assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()), "the same pixels");
            }
            _ => panic!("expected Float32 samples"),
        }
        let keys = |p: &Path| {
            crate::fits_parser::stored_header::parse_stored_header_keys(
                crate::models::FileFormat::XISF,
                &crate::fits_parser::extract_xisf_header(p).unwrap(),
            )
        };
        assert_eq!(keys(&full.master), keys(&k0.master), "the same keywords");
        for (a, b) in [
            (&k0.rejection_low, &full.rejection_low),
            (&k0.rejection_high, &full.rejection_high),
            (&k0.coverage, &full.coverage),
        ] {
            let (a, b) = (a.as_ref().unwrap(), b.as_ref().unwrap());
            assert_eq!(name(a), name(b));
            assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap(), "{} differs", name(a));
        }

        let rect = CropRect { x0: 2, y0: 1, width: 4, height: 3 };
        let mut cropped_cards = cards.clone();
        crate::stacking::crop::restate_crpix(
            &mut cropped_cards,
            &crate::stacking::grid_map::crop_plate_solve(&record(false), 2, 1),
        );
        let c = write_cropped_master(cut.path(), &staged, rect, &cropped_cards, OutputFormat::Fits).unwrap();
        let reader = crate::integration::plane_reader::PlaneReader::open(&c.master).unwrap();
        assert_eq!((reader.width(), reader.height(), reader.channels()), (4, 3, 3));
        assert_eq!(
            reader.read_plane(1).unwrap(),
            crate::stacking::crop::crop_plane(&output.data[plane..2 * plane], w, rect)
        );
        let cov = crate::integration::plane_reader::PlaneReader::open(c.coverage.as_ref().unwrap()).unwrap();
        assert_eq!((cov.width(), cov.height(), cov.channels()), (4, 3, 1));
        let moved = crate::fits_parser::FitsHeader::from_path(c.coverage.as_ref().unwrap()).unwrap();
        let before = crate::fits_parser::FitsHeader::from_path(k0.coverage.as_ref().unwrap()).unwrap();
        assert!((moved.get_f64("CRPIX1").unwrap() - (before.get_f64("CRPIX1").unwrap() - 2.0)).abs() < 1e-9);
        assert!((moved.get_f64("CRPIX2").unwrap() - (before.get_f64("CRPIX2").unwrap() - 1.0)).abs() < 1e-9);
    }
```

`stacking/paths.rs`, `mod tests` (review M6):
```rust
    /// Review M6: a crop run holds every output twice for a moment — the
    /// scratch FITS, then the final file — so the estimate counts the master
    /// family and the drizzle family once more. The coverage maps (one f32
    /// plane each) stay uncounted, as before.
    #[test]
    fn a_crop_run_counts_its_scratch_outputs() {
        let groups = vec![group(ColorMode::Mono, 3, 10, 10)];
        let inputs = |crop_scratch: bool| EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: true,
            drizzle: Some((2, true, false)),
            large_scale: false,
            drizzle_bayer: false,
            crop_scratch,
        };
        let plain = estimate_bytes(&inputs(false));
        let crop = estimate_bytes(&inputs(true));
        // The master 400 B with its two maps, the drizzled master 1600 B and
        // its weight map 1600 B.
        assert_eq!(crop - plain, 3 * 400 + 2 * 1600);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- full_rectangle_crop_writes_what_the_master_writer crop_run_counts_its_scratch --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`write_cropped_master`, `crop_scratch`).

- [ ] **Step 3: Implement**

`stacking/paths.rs`, after `run_json`:
```rust
    /// `root/runs/run-<run_id>-uncropped` — plan 6b-3 D10: a crop run's
    /// outputs, written as FITS and held here until the run's crop rectangle
    /// is known; removed by the crop pass once every group is finalized,
    /// kept (and named in the run's warnings) when one could not be.
    pub fn uncropped_dir(&self, run_id: i64) -> PathBuf {
        self.runs_dir().join(format!("run-{run_id}-uncropped"))
    }
```

`stacking/paths.rs`, `EstimateInputs`, last field (review M6):
```rust
    /// Plan 6b-3 D10, review M6: `output.cropToCoverage > 0` — every output
    /// is written twice for a moment (the scratch FITS, then the final file),
    /// so the master and drizzle families count twice.
    pub crop_scratch: bool,
```
In `estimate_bytes_with`, beside `master_multiplier`: `let copies: u64 = if i.crop_scratch { 2 } else { 1 };`; the master term becomes `total += master_per_frame_bytes * master_multiplier * copies;` and the drizzle terms `total += drizzle_output_bytes * copies;` (both lines). Every `EstimateInputs { … }` literal in `paths.rs`'s tests (15) gets `crop_scratch: false`; `build_plan`'s (`plan.rs:~2496`) gets `crop_scratch: cfg.output.crop_to_coverage > 0,`.

`stacking/master_cards.rs`: `#[derive(Debug, Clone)]` on `WrittenMaster` and `WrittenDrizzle`; import `crate::stacking::crop::{crop_file, CropRect}`; then:
```rust
/// Plan 6b-3 D10 (spec §5.9): one group's master family — the master, its
/// rejection maps and its coverage map, written uncropped as FITS into the
/// run's scratch folder by [`write_master_light`] — re-read, cropped to
/// `rect` and written into `dir` in `format`, named as
/// [`write_master_light`] names them (the maps stay FITS). `cards` are the
/// master's with the cropped `CRPIX` ([`crate::stacking::crop::restate_crpix`]);
/// the maps' cards derive from them exactly as at the first write. On an
/// error every file this call wrote is removed; the scratch is never
/// touched.
pub fn write_cropped_master(
    dir: &Path,
    scratch: &WrittenMaster,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<WrittenMaster> {
    let mut made = Vec::new();
    let result = crop_master_family(dir, scratch, rect, cards, format, &mut made);
    if result.is_err() {
        remove_partial_outputs(&made);
    }
    result
}

fn crop_master_family(
    dir: &Path,
    scratch: &WrittenMaster,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
    made: &mut Vec<PathBuf>,
) -> anyhow::Result<WrittenMaster> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let scratch_stem = scratch
        .master
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("master");
    let master = resolve_collision(&dir.join(scratch_stem).with_extension(output_extension(format)));
    crop_file(&scratch.master, &master, rect, cards, format)?;
    made.push(master.clone());
    let stem = master
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("master")
        .to_string();
    // The maps stay FITS, named from the final master's stem, their cards
    // derived from the master's exactly as `write_master_light` derives them.
    let maps = [
        (&scratch.rejection_low, "rejlow", rejection_map_cards("Low", cards)?),
        (&scratch.rejection_high, "rejhigh", rejection_map_cards("High", cards)?),
        (&scratch.coverage, "coverage", coverage_map_cards(cards)?),
    ];
    let mut written: [Option<PathBuf>; 3] = [None, None, None];
    for (slot, (src, suffix, map_cards)) in written.iter_mut().zip(maps.iter()) {
        let Some(src) = src else { continue };
        let path = resolve_collision(&dir.join(format!("{stem}_{suffix}.fits")));
        crop_file(src, &path, rect, map_cards, OutputFormat::Fits)?;
        made.push(path.clone());
        *slot = Some(path);
    }
    let [rejection_low, rejection_high, coverage] = written;
    Ok(WrittenMaster {
        master,
        rejection_low,
        rejection_high,
        coverage,
    })
}

/// Plan 6b-3 D10: the drizzle family — the drizzled master, its weight map
/// and its coverage — re-read from the scratch folder, cropped to `rect`
/// (already `scale` times the master's, [`CropRect::scaled`]) and written
/// into `dir` in `format`, named from the FINAL master's stem as
/// [`write_drizzled_master`] names them. `cards` are the drizzle cards with
/// the cropped, scaled `CRPIX`. On an error every file this call wrote is
/// removed.
pub fn write_cropped_drizzle(
    dir: &Path,
    master_stem: &str,
    scale: u32,
    scratch: &WrittenDrizzle,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<WrittenDrizzle> {
    let mut made = Vec::new();
    let result = crop_drizzle_family(dir, master_stem, scale, scratch, rect, cards, format, &mut made);
    if result.is_err() {
        remove_partial_outputs(&made);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn crop_drizzle_family(
    dir: &Path,
    master_stem: &str,
    scale: u32,
    scratch: &WrittenDrizzle,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
    made: &mut Vec<PathBuf>,
) -> anyhow::Result<WrittenDrizzle> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let ext = output_extension(format);
    let (drizzle_name, _) = drizzle_file_names(master_stem, scale);
    let drizzle = resolve_collision(&dir.join(&drizzle_name).with_extension(ext));
    crop_file(&scratch.drizzle, &drizzle, rect, cards, format)?;
    made.push(drizzle.clone());
    let stem = drizzle
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("drizzle")
        .to_string();
    let weight_map = match &scratch.weight_map {
        Some(src) => {
            let path = resolve_collision(&dir.join(format!("{stem}_weight.{ext}")));
            crop_file(src, &path, rect, &weight_map_cards(cards)?, format)?;
            made.push(path.clone());
            Some(path)
        }
        None => None,
    };
    let coverage = match &scratch.coverage {
        Some(src) => {
            let path = resolve_collision(&dir.join(format!("{stem}_coverage.fits")));
            crop_file(src, &path, rect, &coverage_map_cards(cards)?, OutputFormat::Fits)?;
            made.push(path.clone());
            Some(path)
        }
        None => None,
    };
    Ok(WrittenDrizzle {
        drizzle,
        weight_map,
        coverage,
    })
}

/// Removes the files a failed crop wrote, logging each one it cannot.
fn remove_partial_outputs(made: &[PathBuf]) {
    for path in made {
        if let Err(error) = std::fs::remove_file(path) {
            tracing::warn!(path = %path.display(), error = %error, "removing a partial crop output failed");
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- full_rectangle_crop_writes_what_the_master_writer crop_run_counts_its_scratch stacking::master_cards stacking::paths stacking::plan --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass; every existing estimate pin unchanged (`crop_scratch: false` adds nothing).

- [ ] **Step 5: Run the gates.** `git diff --exit-code src/types`.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/paths.rs crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/stacking/master_cards.rs
git commit -m "feat(stacking): crop writers that finalize a scratch family, and a space estimate that counts the scratch

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 7b2: The deferred crop pass (D10; spec §5.9)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `CropItem`, `CropDrizzle`, `CropOutputs`; `RunContext.crop_items` (two literals: `start_stacking` `~980`, `test_context` `~11074`);
  - `process_group_output`: the integration `GroupInput`'s `coverage_map`; `let mut output`; the crop block before `output_start`; the scratch folder and FITS for every write in crop mode; the drizzle outcome carries its cards; the `CropItem` push before the last `push_summary_group`;
  - `finish_crop`, `write_crop_item`, `record_crop_item`; `stage_output` calls `finish_crop` after the group loop;
  - five tests.

**Interfaces:**
```rust
// stacking/run.rs (crate-internal)
pub(crate) struct CropItem { /* private fields */ }
pub(crate) struct CropDrizzle { /* private fields */ }
fn finish_crop(rc: &mut RunContext) -> Result<(), RunError>;
```
Consumed by: Task 9 (the e2e crops), Task 10 (run B reports the rectangle).

- [ ] **Step 1: Write the failing tests**

`stacking/run.rs`, `mod tests`, after the Task 6b test:
```rust
    fn master_row<'a>(rows: &'a [crate::db::stacking::MasterLight], kind: &str) -> &'a crate::db::stacking::MasterLight {
        rows.iter().find(|m| m.kind == kind).unwrap_or_else(|| panic!("no {kind} row in {rows:?}"))
    }

    /// Plan 6b-3 D10, Review Focus 1: `cropToCoverage = 0` is today's path —
    /// every output is written once, straight into the output folder in its
    /// own container (the maps stay FITS), and no scratch folder is made.
    #[test]
    fn a_zero_crop_keeps_todays_output_path() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let (fixture, light_ids, working, output) = seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4]);
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT);
        let mut cfg = StackingConfig::default();
        cfg.output.coverage_map = true;
        cfg.output.format = crate::stacking::config::OutputFormat::Xisf;
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 2;
        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg),
            None,
        )
        .unwrap();
        wait_for_run(&ctx, started.run_id);
        let rows = crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
        assert!(rows.len() >= 5, "master, coverage, drizzle, weight map, drizzle coverage: {rows:?}");
        for m in &rows {
            assert!(Path::new(&m.path).starts_with(output.path()), "{}", m.path);
            let expected = if m.kind.ends_with("coverage") { "fits" } else { "xisf" };
            assert_eq!(m.format, expected, "{}", m.kind);
        }
        assert!(
            walkdir::WalkDir::new(working.path())
                .into_iter()
                .filter_map(Result::ok)
                .all(|e| !e.file_name().to_string_lossy().ends_with("-uncropped")),
            "no scratch folder without a crop"
        );
    }

    /// Spec §5.9, plan 6b-3 D10: a co-registered run with `cropToCoverage =
    /// K` crops the master, its coverage map, the drizzled master, its weight
    /// map and its coverage to one rectangle — the uncropped run's `coverage
    /// ≥ K` box (×2 for the drizzle family) — with the uncropped run's pixels
    /// inside it, `CRPIX` moved by exactly the crop origin, rows, group paths
    /// and summary on the final files, and the scratch folder gone.
    #[test]
    fn a_co_registered_run_crops_every_output_to_one_rectangle() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, conn, full_rc, grid) = run_sky_project(tmp.path(), drizzle_2x);
        let full = crate::db::stacking::list_master_lights(&conn, full_rc.run_id).unwrap();
        let reader = PlaneReader::open(Path::new(&master_row(&full, "coverage").path)).unwrap();
        let rect = crate::stacking::crop::coverage_rect(
            &reader.read_plane(0).unwrap(),
            reader.width(),
            reader.height(),
            4,
        )
        .expect("the four frames overlap");
        assert!(rect.width < 512 && rect.height < 384, "{rect:?}");

        let mut cfg = sky_config(&conn);
        drizzle_2x(&mut cfg);
        cfg.output.crop_to_coverage = 4;
        crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
            .unwrap();
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
        run_pipeline(&mut rc).unwrap();

        let cropped = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
        for (kind, s) in [("master", 1), ("coverage", 1), ("drizzle", 2), ("weight_map", 2), ("drizzle_coverage", 2)] {
            let (a, b) = (master_row(&full, kind), master_row(&cropped, kind));
            let r = rect.scaled(s);
            assert_eq!((b.width as usize, b.height as usize), (r.width, r.height), "{kind}");
            assert!(Path::new(&b.path).starts_with(tmp.path().join("out")), "{kind}: {}", b.path);
            let (ra, rb) = (PlaneReader::open(Path::new(&a.path)).unwrap(), PlaneReader::open(Path::new(&b.path)).unwrap());
            for p in 0..ra.channels() {
                let expected = crate::stacking::crop::crop_plane(&ra.read_plane(p).unwrap(), ra.width(), r);
                let got = rb.read_plane(p).unwrap();
                assert!(
                    expected.iter().zip(&got).all(|(x, y)| x.to_bits() == y.to_bits()),
                    "{kind} plane {p}: the crop changes no pixel"
                );
            }
            let ha = crate::fits_parser::FitsHeader::from_path(Path::new(&a.path)).unwrap();
            let hb = crate::fits_parser::FitsHeader::from_path(Path::new(&b.path)).unwrap();
            for (key, origin) in [("CRPIX1", r.x0), ("CRPIX2", r.y0)] {
                let moved = ha.get_f64(key).unwrap() - origin as f64;
                assert!((hb.get_f64(key).unwrap() - moved).abs() < 1e-9, "{kind} {key}");
            }
        }
        let groups = crate::db::stacking::list_groups(&conn, rc.run_id).unwrap();
        assert_eq!(groups[0].master_path.as_deref(), Some(master_row(&cropped, "master").path.as_str()));
        assert_eq!(groups[0].drizzle_path.as_deref(), Some(master_row(&cropped, "drizzle").path.as_str()));
        let g = &rc.summary.groups[0];
        assert_eq!(g.master_path, groups[0].master_path);
        assert_eq!(g.coverage_path.as_deref(), Some(master_row(&cropped, "coverage").path.as_str()));
        assert_eq!(g.weight_map_path.as_deref(), Some(master_row(&cropped, "weight_map").path.as_str()));
        assert_eq!(
            g.drizzle_coverage_path.as_deref(),
            Some(master_row(&cropped, "drizzle_coverage").path.as_str())
        );
        assert!(!rc.layout.uncropped_dir(rc.run_id).exists(), "the scratch folder is removed");
    }

    /// Spec §5.9, plan 6b-3 D10: in native mode each group crops to its own
    /// `coverage ≥ K` box — two groups shot 40 px apart get two rectangles,
    /// each tight (its border rows and columns reach `K`) — and in
    /// co-registered mode both get the one, wider, union.
    #[test]
    fn native_mode_crops_each_group_to_its_own_rectangle() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let target = (83.8, -5.4);
        let east = 40.0 * 1.5 / 3600.0 / target.1.to_radians().cos();
        let stars = test_fixtures::sky_stars(target, 0.12, 50.0, 7);
        for i in 0..3 {
            let uuid = format!("r{i}");
            let mut f = test_fixtures::sky_frame(&uuid, (target.0 + east + i as f64 * 0.001, target.1));
            f.filter = "R";
            f.noise_seed = 200 + i as u64;
            test_fixtures::add_project_sky_frame(&conn, &tmp.path().join("collab"), "p1", &f, &stars);
        }
        conn.execute(
            "UPDATE project_frames_local
                SET filter_canonical = CASE WHEN frame_uuid LIKE 'r%' THEN 'Red' ELSE 'Lum' END
              WHERE project_id = 'p1'",
            [],
        )
        .unwrap();
        let run = |geometry: RegistrationGeometry| -> RunContext {
            let mut cfg = sky_config(&conn);
            cfg.registration.geometry = geometry;
            cfg.output.crop_to_coverage = 3;
            crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
                .unwrap();
            let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
            rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
            run_pipeline(&mut rc).unwrap();
            rc
        };
        // (group, CRPIX1, CRPIX2, width, height) of each master.
        let rects = |rc: &RunContext| -> Vec<(String, f64, f64, i64, i64)> {
            crate::db::stacking::list_master_lights(&conn, rc.run_id)
                .unwrap()
                .into_iter()
                .filter(|m| m.kind == "master")
                .map(|m| {
                    let h = crate::fits_parser::FitsHeader::from_path(Path::new(&m.path)).unwrap();
                    (m.group_key, h.get_f64("CRPIX1").unwrap(), h.get_f64("CRPIX2").unwrap(), m.width, m.height)
                })
                .collect()
        };

        let native = run(RegistrationGeometry::Native);
        let n = rects(&native);
        assert_eq!(n.len(), 2, "{n:?}");
        assert!((n[0].1 - n[1].1).abs() > 20.0, "two rectangles about 40 px apart: {n:?}");
        for m in crate::db::stacking::list_master_lights(&conn, native.run_id)
            .unwrap()
            .iter()
            .filter(|m| m.kind == "coverage")
        {
            let r = PlaneReader::open(Path::new(&m.path)).unwrap();
            let (w, h) = (r.width(), r.height());
            let c = r.read_plane(0).unwrap();
            let reaches = |mut it: Box<dyn Iterator<Item = f32> + '_>| it.any(|v| v >= 3.0);
            assert!(reaches(Box::new(c[..w].iter().copied())), "{}: first row", m.path);
            assert!(reaches(Box::new(c[(h - 1) * w..].iter().copied())), "{}: last row", m.path);
            assert!(reaches(Box::new((0..h).map(|y| c[y * w]))), "{}: first column", m.path);
            assert!(reaches(Box::new((0..h).map(|y| c[y * w + w - 1]))), "{}: last column", m.path);
        }

        let co = run(RegistrationGeometry::CoRegistered);
        let c = rects(&co);
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!((c[0].1, c[0].2, c[0].3, c[0].4), (c[1].1, c[1].2, c[1].3, c[1].4), "one rectangle for the run");
        assert!(c[0].3 > n[0].3.max(n[1].3), "the union is wider than either group's own: {c:?} vs {n:?}");
    }

    /// Plan 6b-3 D10: a crop pass that cannot write never loses a master —
    /// the rows and the summary keep naming the scratch files, which stay,
    /// and the run says where they are.
    #[test]
    fn a_failed_crop_keeps_the_masters_in_the_scratch_folder() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        let mut cfg = sky_config(&conn);
        cfg.output.crop_to_coverage = 2;
        crate::db::stacking::set_project_config(&conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], None)
            .unwrap();
        // A file where the output folder's parent should be: every final
        // write fails at `create_dir_all`.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &blocker.join("out"));
        rc.grid = Some(Arc::new(crate::stacking::grid_map::GridRun::new(grid)));
        run_pipeline(&mut rc).unwrap();

        let rows = crate::db::stacking::list_master_lights(&conn, rc.run_id).unwrap();
        let master = master_row(&rows, "master");
        let scratch = rc.layout.uncropped_dir(rc.run_id);
        assert!(Path::new(&master.path).starts_with(&scratch), "{}", master.path);
        assert!(Path::new(&master.path).exists(), "the master survives");
        assert_eq!(master.format, "fits");
        assert_eq!(rc.summary.groups[0].master_path.as_deref(), Some(master.path.as_str()));
        assert!(
            rc.warnings.iter().any(|w| w.contains("could not be finalized") && w.contains("-uncropped")),
            "{:?}",
            rc.warnings
        );
    }

    /// Review I7: a crop whose rectangle is the whole master finalizes
    /// through the writer the K = 0 path uses — an XISF master with the same
    /// pixels and keywords (all but the run id) — and says nothing about
    /// cropping.
    #[test]
    fn a_full_rectangle_crop_writes_what_the_zero_crop_path_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let (fixture, light_ids, _working, _output) = seed_star_group(&db_path, SET_NAME, &shifts, &[5.0f32; 4]);
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, STAR_FIELD_WIDTH, STAR_FIELD_HEIGHT);
        let run_with = |crop: u32| -> (String, RunSummary) {
            let mut cfg = StackingConfig::default();
            cfg.output.format = crate::stacking::config::OutputFormat::Xisf;
            cfg.output.crop_to_coverage = crop;
            let started = start_stacking(
                ctx.clone(),
                Arc::new(NullEmitter),
                &PathPolicy::AllowAll,
                "test".to_string(),
                StackSubject::FrameSet(fixture.set_id),
                Some(cfg),
                None,
            )
            .unwrap();
            wait_for_run(&ctx, started.run_id);
            let row = crate::db::stacking::get_run(&fixture.conn, started.run_id).unwrap().unwrap();
            assert_eq!(row.status, "done", "{row:?}");
            let summary: RunSummary = serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
            let rows = crate::db::stacking::list_master_lights(&fixture.conn, started.run_id).unwrap();
            (master_row(&rows, "master").path.clone(), summary)
        };
        let (k0, _) = run_with(0);
        let (k1, summary) = run_with(1);
        assert!(k1.ends_with(".xisf") && !k1.contains("-uncropped"), "{k1}");
        let read = |p: &str| astroimage::ImageConverter::read_raw(Path::new(p)).unwrap();
        let ((ma, pa), (mb, pb)) = (read(&k0), read(&k1));
        assert_eq!((ma.width, ma.height, ma.channels), (mb.width, mb.height, mb.channels));
        match (pa, pb) {
            (astroimage::PixelData::Float32(a), astroimage::PixelData::Float32(b)) => {
                assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()), "the same pixels");
            }
            _ => panic!("expected Float32 samples"),
        }
        let keys = |p: &str| {
            let mut k = crate::fits_parser::stored_header::parse_stored_header_keys(
                crate::models::FileFormat::XISF,
                &crate::fits_parser::extract_xisf_header(Path::new(p)).unwrap(),
            );
            k.remove("ATH_STKI");
            k
        };
        assert_eq!(keys(&k1), keys(&k0), "the same keywords but the run id");
        assert!(
            summary.warnings.iter().all(|w| !w.contains("crop") && !w.contains("uncropped")),
            "{:?}",
            summary.warnings
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_zero_crop_keeps a_co_registered_run_crops native_mode_crops a_failed_crop_keeps full_rectangle_crop_writes_what_the_zero_crop --test-threads=4 2>&1 | tail -20`
Expected: the crop tests fail (no crop pass yet: masters land uncropped, no scratch folder); `a_zero_crop_keeps_todays_output_path` passes (it pins today's path).

- [ ] **Step 3: Implement**

`stacking/run.rs`:

1. Imports: add `WrittenMaster`, `write_cropped_master`, `write_cropped_drizzle` to the `master_cards` use; `use crate::stacking::crop::{restate_crpix, union_rects, CropRect};` and `use crate::stacking::grid_map::crop_plate_solve;`.

2. The types, beside `AnchorPick` (`~7904`):
```rust
/// Plan 6b-3 D10: one group's outputs waiting in the scratch folder for the
/// run's crop rectangle, with everything [`finish_crop`] needs to finalize
/// them without re-reading a header.
pub(crate) struct CropItem {
    group_key: String,
    group_id: i64,
    /// The group's own `coverage ≥ K` box; `None` when no pixel reached `K`.
    rect: Option<CropRect>,
    width: usize,
    height: usize,
    channels: usize,
    frames: usize,
    total_exposure_s: Option<f64>,
    /// The master's cards as written: the uncropped WCS.
    cards: Vec<Card>,
    /// The record those cards' WCS block was written from.
    wcs: Option<PlateSolveRecord>,
    /// The scratch files.
    master: WrittenMaster,
    drizzle: Option<CropDrizzle>,
}

pub(crate) struct CropDrizzle {
    written: WrittenDrizzle,
    cards: Vec<Card>,
    scale: u32,
}

/// One group's final files after its crop pass.
struct CropOutputs {
    rect: CropRect,
    master: WrittenMaster,
    /// The drizzle family and its (scaled) rectangle.
    drizzle: Option<(WrittenDrizzle, CropRect)>,
}
```

3. `RunContext`, after `drizzle_attempted`:
```rust
    /// Plan 6b-3 D10: the groups a crop run has written to its scratch
    /// folder, finalized by [`finish_crop`] after the last group. Always
    /// empty when `output.cropToCoverage` is 0.
    pub(crate) crop_items: Vec<CropItem>,
```
and `crop_items: Vec::new(),` in both literals.

4. `process_group_output`:
- the integration `GroupInput` (Task 6a's line) becomes `coverage_map: rc.config.output.coverage_map || rc.config.output.crop_to_coverage > 0,` (the count is needed for the rectangle even when the map is not written; `stats_on_covered` stays `rc.grid.is_some()`);
- `let output = match integrate_group(` becomes `let mut output = match integrate_group(`;
- immediately before `let output_start = Instant::now();`:
  ```rust
    // Plan 6b-3 D10 (spec §5.9): with `cropToCoverage = K > 0` this group's
    // outputs are written as FITS into the run's scratch folder and wait for
    // the run's rectangle (`finish_crop`). The group's own `coverage ≥ K` box
    // is taken here, from the count the engine kept for it; when the
    // coverage map itself is off, that count is dropped before the write.
    let crop_k = rc.config.output.crop_to_coverage;
    let crop_mode = crop_k > 0;
    let group_rect = if crop_mode {
        output
            .coverage
            .as_deref()
            .and_then(|c| crate::stacking::crop::coverage_rect(c, output.width, output.height, crop_k))
    } else {
        None
    };
    if crop_mode && !rc.config.output.coverage_map {
        output.coverage = None;
    }
    let (out_dir, out_format) = if crop_mode {
        (
            rc.layout.uncropped_dir(rc.run_id),
            crate::stacking::config::OutputFormat::Fits,
        )
    } else {
        (rc.output_dir.clone(), rc.config.output.format)
    };
  ```
- `write_master_light(&rc.output_dir, &name, &output, &cards, rc.config.output.format)` becomes `write_master_light(&out_dir, &name, &output, &cards, out_format)`;
- `let output_format_str = enum_serde_name(&rc.config.output.format)?;` becomes `enum_serde_name(&out_format)?` (the row states the container on disk; the crop pass restates it);
- beside `let mut drizzle_stats …`: `let mut crop_drizzle: Option<CropDrizzle> = None;`;
- the drizzle outcome carries its cards: the annotation becomes `Result<(WrittenDrizzle, DrizzleStats, Vec<Card>), DrizzleFailure>`; `write_drizzled_master(&rc.output_dir, …, rc.config.output.format)` becomes `write_drizzled_master(&out_dir, &master_stem, &drz, &drz_cards, out_format)`; `Ok(wd) => Ok((wd, drz.stats))` becomes `Ok(wd) => Ok((wd, drz.stats, drz_cards))`; the arm `Ok((wd, stats)) => {` becomes `Ok((wd, stats, drz_cards)) => {` and gains, after `weight_map_path = weight_map_path_str;`:
  ```rust
                    if crop_mode {
                        crop_drizzle = Some(CropDrizzle {
                            written: wd.clone(),
                            cards: drz_cards,
                            scale,
                        });
                    }
  ```
- immediately before the final `push_summary_group(` call:
  ```rust
    if crop_mode {
        rc.crop_items.push(CropItem {
            group_key: group.key.clone(),
            group_id,
            rect: group_rect,
            width: output.width,
            height: output.height,
            channels,
            frames: output.stats.included,
            total_exposure_s,
            cards: cards.clone(),
            wcs: wcs.cloned(),
            master: written.clone(),
            drizzle: crop_drizzle,
        });
    }
  ```
  The summary entry pushed right after names the scratch files; `finish_crop` moves it.

5. The pass, after `stage_output`:
```rust
/// Plan 6b-3 D10 (spec §5.9): the crop pass, after the last group of a run
/// with `cropToCoverage = K > 0`. Co-registered: ONE rectangle, the union of
/// the groups' `coverage ≥ K` boxes (every group shares one pixel grid: the
/// run reference's, or the project's canonical grid), so the run's masters
/// stay pixel-aligned. Native: each group's own box. Each group's outputs
/// are re-read from the scratch folder, cropped and written in the
/// configured format into the output folder, and its rows, group paths and
/// summary paths move onto them. A group with no rectangle, or whose crop
/// fails, is finalized uncropped with a warning; a group that cannot be
/// finalized at all keeps its scratch files, which its rows already name —
/// a master is never lost. The scratch folder goes when every group is
/// final. Database errors fail the run, as everywhere in Output.
fn finish_crop(rc: &mut RunContext) -> Result<(), RunError> {
    let items = std::mem::take(&mut rc.crop_items);
    if items.is_empty() {
        return Ok(());
    }
    let k = rc.config.output.crop_to_coverage;
    let scratch = rc.layout.uncropped_dir(rc.run_id);
    let run_rect = match rc.config.registration.geometry {
        RegistrationGeometry::CoRegistered => Some(union_rects(items.iter().filter_map(|i| i.rect))),
        RegistrationGeometry::Native => None,
    };
    let mut kept = 0usize;
    for item in &items {
        let full = CropRect::full(item.width, item.height);
        let wanted = run_rect.unwrap_or(item.rect);
        let target = match wanted {
            Some(r) if r.fits_within(item.width, item.height) => r,
            Some(r) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    crop_x0 = r.x0,
                    crop_y0 = r.y0,
                    crop_width = r.width,
                    crop_height = r.height,
                    "crop rectangle outside the group's master; outputs written uncropped"
                );
                rc.warnings.push(format!(
                    "group {}: the run's crop rectangle does not fit its master; its outputs are written uncropped",
                    item.group_key
                ));
                full
            }
            None => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    count = k,
                    "no pixel reached the crop coverage; outputs written uncropped"
                );
                rc.warnings.push(format!(
                    "group {}: no pixel is covered by {k} frames; its outputs are written uncropped",
                    item.group_key
                ));
                full
            }
        };
        let outputs = match write_crop_item(rc, item, target) {
            Ok(o) => Ok(o),
            Err(e) if target != full => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    error = %format!("{e:#}"),
                    "crop failed; writing the outputs uncropped"
                );
                rc.warnings.push(format!(
                    "group {}: the crop failed ({e:#}); its outputs are written uncropped",
                    item.group_key
                ));
                write_crop_item(rc, item, full)
            }
            Err(e) => Err(e),
        };
        match outputs {
            Ok(o) => record_crop_item(rc, item, &o)?,
            Err(e) => {
                kept += 1;
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    path = %scratch.display(),
                    error = %format!("{e:#}"),
                    "crop outputs could not be finalized; kept in the scratch folder"
                );
                rc.warnings.push(format!(
                    "group {}: its outputs could not be finalized ({e:#}); they stay in {}",
                    item.group_key,
                    scratch.display()
                ));
            }
        }
    }
    if kept == 0 {
        if let Err(error) = std::fs::remove_dir_all(&scratch) {
            tracing::warn!(
                run_id = rc.run_id,
                path = %scratch.display(),
                error = %error,
                "removing the crop scratch folder failed"
            );
        }
    }
    Ok(())
}

/// The file half of one group's crop: the master family, then the drizzle
/// family, each into the output folder; a drizzle failure removes the master
/// family this call wrote, so a retry starts clean. `rect` may be the full
/// master: that finalization writes exactly what the K = 0 path writes, the
/// same writer on the same pixels and cards (review I7). The drizzle WCS is
/// `scale_plate_solve` of the master's record again (review M13): the first
/// write already scaled the same record, so it does not fail here; if it
/// ever did, the group would be finalized uncropped, or keep its scratch.
fn write_crop_item(rc: &RunContext, item: &CropItem, rect: CropRect) -> anyhow::Result<CropOutputs> {
    let format = rc.config.output.format;
    let mut cards = item.cards.clone();
    if let Some(wcs) = &item.wcs {
        restate_crpix(&mut cards, &crop_plate_solve(wcs, rect.x0, rect.y0));
    }
    let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let master = write_cropped_master(&rc.output_dir, &item.master, rect, &cards, format)?;
    let drizzle = match &item.drizzle {
        None => None,
        Some(d) => {
            let drect = rect.scaled(d.scale);
            let attempt = (|| -> anyhow::Result<WrittenDrizzle> {
                let mut dcards = d.cards.clone();
                if let Some(wcs) = &item.wcs {
                    let scaled = scale_plate_solve(wcs, d.scale)?;
                    restate_crpix(&mut dcards, &crop_plate_solve(&scaled, drect.x0, drect.y0));
                }
                let stem = master
                    .master
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("master");
                write_cropped_drizzle(&rc.output_dir, stem, d.scale, &d.written, drect, &dcards, format)
            })();
            match attempt {
                Ok(w) => Some((w, drect)),
                Err(e) => {
                    for path in [Some(&master.master), master.rejection_low.as_ref(), master.rejection_high.as_ref(), master.coverage.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        if let Err(error) = std::fs::remove_file(path) {
                            tracing::warn!(path = %path.display(), error = %error, "removing a partial crop output failed");
                        }
                    }
                    return Err(e);
                }
            }
        }
    };
    Ok(CropOutputs { rect, master, drizzle })
}

/// The record half: the group row's paths, one `master_lights` upsert per
/// final file (the key is `(run, group, kind)`, so each scratch row is
/// replaced), and the summary's paths.
fn record_crop_item(rc: &mut RunContext, item: &CropItem, out: &CropOutputs) -> Result<(), RunError> {
    let show = |p: &Path| p.display().to_string();
    let format_str = enum_serde_name(&rc.config.output.format)?;
    let master_path = show(&out.master.master);
    let rejection_low = out.master.rejection_low.as_deref().map(show);
    let rejection_high = out.master.rejection_high.as_deref().map(show);
    let coverage = out.master.coverage.as_deref().map(show);
    let drizzle = out.drizzle.as_ref().map(|(d, r)| (show(&d.drizzle), *r));
    let weight_map = out.drizzle.as_ref().and_then(|(d, _)| d.weight_map.as_deref().map(show));
    let drizzle_coverage = out.drizzle.as_ref().and_then(|(d, _)| d.coverage.as_deref().map(show));
    {
        let conn = db(&rc.ctx)?.conn();
        update_group(
            &conn,
            item.group_id,
            &GroupUpdate {
                master_path: Some(&master_path),
                rejection_low_path: rejection_low.as_deref(),
                rejection_high_path: rejection_high.as_deref(),
                drizzle_path: drizzle.as_ref().map(|(p, _)| p.as_str()),
                ..Default::default()
            },
        )?;
        let row = |kind: MasterLightKind, path: &str, format: &str, rect: CropRect, channels: usize| {
            insert_master_light(
                &conn,
                &NewMasterLight {
                    subject: &rc.subject,
                    run_id: rc.run_id,
                    group_key: &item.group_key,
                    kind: kind.as_db_str(),
                    path,
                    format,
                    width: rect.width as i64,
                    height: rect.height as i64,
                    channels: channels as i64,
                    frames: item.frames as i64,
                    total_exposure_s: item.total_exposure_s,
                },
            )
        };
        row(MasterLightKind::Master, &master_path, &format_str, out.rect, item.channels)?;
        if let Some(path) = coverage.as_deref() {
            row(MasterLightKind::Coverage, path, "fits", out.rect, 1)?;
        }
        if let Some((path, drect)) = drizzle.as_ref() {
            row(MasterLightKind::Drizzle, path, &format_str, *drect, item.channels)?;
            if let Some(path) = weight_map.as_deref() {
                row(MasterLightKind::WeightMap, path, &format_str, *drect, item.channels)?;
            }
            if let Some(path) = drizzle_coverage.as_deref() {
                row(MasterLightKind::DrizzleCoverage, path, "fits", *drect, item.channels)?;
            }
        }
    }
    if let Some(g) = rc.summary.groups.iter_mut().find(|g| g.key == item.group_key) {
        g.master_path = Some(master_path.clone());
        g.rejection_low_path = rejection_low;
        g.rejection_high_path = rejection_high;
        g.coverage_path = coverage;
        if let Some((path, _)) = drizzle {
            g.drizzle_path = Some(path);
            g.weight_map_path = weight_map;
            g.drizzle_coverage_path = drizzle_coverage;
        }
    }
    tracing::info!(
        run_id = rc.run_id,
        group_key = %item.group_key,
        crop_x0 = out.rect.x0,
        crop_y0 = out.rect.y0,
        crop_width = out.rect.width,
        crop_height = out.rect.height,
        path = %master_path,
        "outputs cropped"
    );
    Ok(())
}
```
`insert_master_light` returns the crate's `Result`; the `?` on `row(…)` converts as the existing calls in `process_group_output` do. If the `row` closure's borrow of `conn` and `rc` together trips the borrow checker (it borrows `rc` immutably while `conn` came from `rc.ctx`), inline the five calls instead.

6. `stage_output`, right after the `for group in &groups { … }` loop:
```rust
    // Plan 6b-3 D10: the crop pass, once every group has written.
    if cfg.output.crop_to_coverage > 0 {
        let crop_start = Instant::now();
        finish_crop(rc)?;
        output_total += crop_start.elapsed();
    }
```

New log fields: `crop_x0`, `crop_y0`, `crop_width`, `crop_height` (Task 11 adds them to the dictionary).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_zero_crop_keeps a_co_registered_run_crops native_mode_crops a_failed_crop_keeps full_rectangle_crop_writes_what_the_zero_crop stacking::crop stacking::master_cards stacking::run::tests --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass. If `native_mode_crops_each_group_to_its_own_rectangle` finds one master, the `R` frames were grouped with the `L` ones: print `rc.plan_groups` and check the `filter_canonical` update reached `project_frames_local` (the grouping keys on it, `project_source.rs:339`). Never loosen an assertion.

- [ ] **Step 5: Run the gates.** `git diff --exit-code src/types`.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/run.rs
git commit -m "feat(stacking): crop to coverage — one rectangle per co-registered run, one per group in native, a master is never lost

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 8a: A project run starts (D13, D14, D16, D17; spec §2, §5.4)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `start_stacking` (`~783`): the `FrameSet` guard goes; the body moves into `pub(crate) fn start_stacking_with_solver(…, solver)`, which `start_stacking` calls with the catalog solver; the run gets the plan's confirmed grid (`RunContext.grid`, `RunSummary.grid`);
  - `stage_calibrate` (`~2557`): its own message instead of `PROJECT_STACKING_UNAVAILABLE`; the import at `~91` goes;
  - the two `rc.layout.root.join("tmp")` (`~2589`, `~3558`) become `rc.layout.tmp_root()`;
  - tests: `a_project_start_is_refused_until_6b2` (`~21195`) is deleted (named change); one new test.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `PROJECT_STACKING_UNAVAILABLE` (`~941`) is deleted; `project_inputs` (`~1961`) takes the subject and adds the D14 check; `build_plan`'s dispatch passes it; three tests.
- Modify `crates/athenaeum-core/src/stacking/origin.rs`: `WcsOutlook`, `wcs_outlook`, `ReferenceWcsCheck`, `reference_wcs_check`; the funnel allowlist's `start_stacking` entry (`~437-440`) is removed (named change).
- Modify `crates/athenaeum-core/src/stacking/solve.rs`: `solver_identity`, `installed_solver_identity`; `CatalogSolver::prepared` calls `solver_identity`; one test.
- Modify `crates/athenaeum-core/src/stacking/paths.rs`: `WorkingLayout::tmp_root`; `cleanup_work` removes `tmp/` under `Intermediates` and `All`; one test.

**Interfaces:**
```rust
// stacking/run.rs
pub(crate) fn start_stacking_with_solver(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, policy: &PathPolicy, app_version: String, subject: StackSubject, config: Option<StackingConfig>, rerun_from: Option<Stage>, solver: Arc<dyn crate::stacking::solve::FrameSolver>) -> Result<StartedStacking, ApiError>;
// stacking/solve.rs
pub(crate) fn solver_identity(app_data: &Path, config: &PlateSolveConfig) -> Result<String, String>;
pub(crate) fn installed_solver_identity(conn: &Connection) -> Result<String, String>;
// stacking/origin.rs
pub(crate) enum WcsOutlook { NotApplicable, Manifest, Solved, ToSolve, Unsolvable }
pub(crate) fn wcs_outlook(conn: &Connection, origin: &FrameOrigin, frame_id: i64, identity: Option<&str>) -> anyhow::Result<WcsOutlook>;
pub(crate) struct ReferenceWcsCheck { pub blocker: Option<String>, pub warning: Option<String> }
pub(crate) fn reference_wcs_check(conn: &Connection, origins: &FrameOrigins, in_scope: &[i64], pinned: Option<i64>, identity: Option<&str>) -> anyhow::Result<ReferenceWcsCheck>;
// stacking/paths.rs
impl WorkingLayout { pub fn tmp_root(&self) -> PathBuf; } // root/tmp
```
Consumed by: Task 8b (`start_project_stacking`), Task 9 (`start_stacking_with_solver`).

- [ ] **Step 1: Write the failing tests**

`stacking/solve.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D14: the plan reads the solver's identity from the catalog
    /// connection alone, and it is the identity the run's solver reports —
    /// so the plan and the run agree on which cached failures stand. A
    /// database with no file has none.
    // `use super::*` brings the `FrameSolver` trait in for `identity()`
    // (review M15): keep the import if this test moves.
    #[test]
    fn the_installed_identity_is_the_runs_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let catalog = crate::plate_solve::catalog_root(dir.path());
        std::fs::create_dir_all(&catalog).unwrap();
        std::fs::write(catalog.join("stars.smac"), b"").unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let installed = installed_solver_identity(&conn).expect("a catalog is installed");
        assert_eq!(CatalogSolver::new(ctx).identity().unwrap(), installed);
        let memory = rusqlite::Connection::open_in_memory().unwrap();
        assert!(installed_solver_identity(&memory).is_err());
    }
```
(`crate::plate_solve::catalog_root` is re-exported, `plate_solve/mod.rs:12-13`; an empty `stars.smac` is enough for the stat-only identity, `plate_solve/layers.rs:40, 108-141`.)

`stacking/paths.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D17 (6b-2 final review M3): `tmp/` holds only crash
    /// leftovers (the solve's ADU copy, Calibrate's scratch) and no artifact
    /// rows, so `Intermediates` and `All` sweep it and `Registered` keeps it.
    #[test]
    fn cleanup_sweeps_the_tmp_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let layout = WorkingLayout::new(tmp.path(), "set");
        let leftover = layout.tmp_root().join("adu-copy.fits");
        let subject = StackSubject::FrameSet(1);
        for (what, kept) in [
            (CleanupWhat::Registered, true),
            (CleanupWhat::Intermediates, false),
            (CleanupWhat::All, false),
        ] {
            std::fs::create_dir_all(layout.tmp_root()).unwrap();
            std::fs::write(&leftover, b"x").unwrap();
            cleanup_work(&conn, &subject, &layout, what).unwrap();
            assert_eq!(leftover.exists(), kept, "{what:?}");
        }
    }
```

`stacking/plan.rs`, `mod tests` (after `a_project_plan_reports_frames_that_could_not_be_solved`):
```rust
    /// Plan 6b-3 D14 (spec §5.4): with no installed catalog nothing can be
    /// solved, so a project none of whose in-scope frames carries a manifest
    /// WCS is blocked at plan time — the run would fail at Reference. One
    /// frame with a manifest WCS lifts the block.
    #[test]
    fn a_project_with_no_possible_wcs_is_blocked_at_plan_time() {
        use crate::stacking::test_fixtures::{add_project_frame, manifest_wcs_json, project_frame_spec};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        for uuid in ["a", "b", "c"] {
            add_project_frame(&f.conn, &landed, "p1", &project_frame_spec(uuid));
        }
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                None,
            )
            .unwrap()
        };
        let blocked = plan(&f);
        let reference: Vec<&PlanBlocker> = blocked.blockers.iter().filter(|b| b.code == "reference").collect();
        assert_eq!(reference.len(), 1, "{:?}", blocked.blockers);
        assert_eq!(reference[0].message, crate::stacking::origin::NO_FRAME_WITH_WCS_NO_CATALOG);

        let mut d = project_frame_spec("d");
        d.wcs = Some(manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        add_project_frame(&f.conn, &landed, "p1", &d);
        let open = plan(&f);
        assert!(open.blockers.iter().all(|b| b.code != "reference"), "{:?}", open.blockers);
    }

    /// Plan 6b-3 D14: a manual pin without a manifest WCS is a warning while
    /// the run still has to try to solve it, and a `reference` blocker once
    /// its cached failure stands for this very solver and catalog. A failure
    /// cached under another solver identity is tried again, so it warns.
    #[test]
    fn a_pinned_frame_without_a_wcs_warns_then_blocks_on_a_standing_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let f = test_fixtures::frame_set_with_conn(rusqlite::Connection::open(&db_path).unwrap(), "unused");
        let catalog = crate::plate_solve::catalog_root(dir.path());
        std::fs::create_dir_all(&catalog).unwrap();
        std::fs::write(catalog.join("stars.smac"), b"").unwrap();
        let (working, output) = seed_project(&f);
        let mut cfg = with_folders(&working, &output);
        let first = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(cfg.clone()),
        )
        .unwrap();
        let o1 = first.project_frames.iter().find(|r| r.frame_uuid == "o1").unwrap().handle.unwrap();
        cfg.reference.mode = ReferenceMode::Manual;
        crate::db::stacking::set_project_config(&f.conn, "p1", &serde_json::to_string(&cfg).unwrap(), &[], Some("o1"))
            .unwrap();
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(&f.conn, &SettingsManager::new(), &PathPolicy::AllowAll, &StackSubject::Project("p1".into()), None)
                .unwrap()
        };
        let pin_blocked = |p: &StackingPlan| {
            p.blockers
                .iter()
                .any(|b| b.code == "reference" && b.message == crate::stacking::origin::PIN_WITHOUT_WCS)
        };

        let to_solve = plan(&f);
        assert!(to_solve.warnings.iter().any(|w| w == crate::stacking::origin::PIN_TO_SOLVE), "{:?}", to_solve.warnings);
        assert!(!pin_blocked(&to_solve), "{:?}", to_solve.blockers);

        let blake3: String = f
            .conn
            .query_row(
                "SELECT blake3 FROM project_frames_local WHERE project_id = 'p1' AND frame_uuid = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let identity = crate::stacking::solve::installed_solver_identity(&f.conn).unwrap();
        let failed = crate::stacking::solve::SolveOutcome::Failed("no match".into());
        crate::stacking::solve::write_outcome(
            &f.conn,
            "p1",
            o1,
            &blake3,
            &crate::stacking::solve::solve_hash(&blake3, &identity),
            failed.clone(),
        )
        .unwrap();
        let blocked = plan(&f);
        assert!(pin_blocked(&blocked), "{:?}", blocked.blockers);

        crate::stacking::solve::write_outcome(&f.conn, "p1", o1, &blake3, "another-solver", failed).unwrap();
        let retried = plan(&f);
        assert!(!pin_blocked(&retried), "{:?}", retried.blockers);
        assert!(retried.warnings.iter().any(|w| w == crate::stacking::origin::PIN_TO_SOLVE));
    }

    /// Plan 6b-3 D16: a project's steady-state plan writes nothing. With the
    /// config seeded and every handle minted by a first plan, a second plan
    /// succeeds while another connection holds the write lock and this one
    /// will not wait for it.
    #[test]
    fn a_steady_state_project_plan_takes_no_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let f = test_fixtures::frame_set_with_conn(rusqlite::Connection::open(&db_path).unwrap(), "unused");
        let (working, output) = seed_project(&f);
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                Some(with_folders(&working, &output)),
            )
        };
        let first = plan(&f).unwrap();
        let writer = rusqlite::Connection::open(&db_path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        f.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let second = plan(&f).expect("a steady-state plan only reads");
        assert_eq!(second.project_frames.len(), first.project_frames.len());
        assert_eq!(second.config_hash, first.config_hash);
        writer.execute_batch("ROLLBACK").unwrap();
    }
```

`stacking/run.rs`, `mod tests` — delete `a_project_start_is_refused_until_6b2`; add after the Task 7b2 tests:
```rust
    /// Spec §2, plan 6b-3 D13: `start_stacking` starts a project. The run
    /// queues as "Stacking · <title>", stacks onto the confirmed grid,
    /// records it in its summary, and writes into `<output>/<title>/`.
    #[test]
    fn a_project_run_starts_on_its_confirmed_grid() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let grid = seed_sky_project(&conn, &tmp.path().join("collab"), 4);
        for (key, dir) in [
            (crate::settings::keys::STACKING_WORKING_DIR, tmp.path().join("work")),
            (crate::settings::keys::STACKING_OUTPUT_DIR, tmp.path().join("out")),
        ] {
            crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
        }
        crate::db::collab::set_canonical_grid(&conn, "p1", Some(&grid.canonical_json())).unwrap();
        sky_config(&conn);

        let hold_flag = Arc::new(AtomicBool::new(false));
        let (hold, _hold_job) = ctx
            .compute_queue
            .acquire(ComputeJobKind::Analysis, "hold", hold_flag)
            .unwrap();
        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::Project("p1".into()),
            None,
            None,
        )
        .expect("a project run starts");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ctx.compute_queue.snapshot().iter().any(|e| e.label == "Stacking · p1") {
            assert!(Instant::now() < deadline, "{:?}", ctx.compute_queue.snapshot());
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(hold);
        wait_for_run(&ctx, started.run_id);

        let row = crate::db::stacking::get_run(&conn, started.run_id).unwrap().unwrap();
        assert_eq!(row.status, "done", "{row:?}");
        let summary: RunSummary = serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
        assert_eq!(summary.grid, Some(grid));
        assert_eq!(summary.project_id.as_deref(), Some("p1"));
        let masters = crate::db::stacking::list_master_lights(&conn, started.run_id).unwrap();
        let master = master_row(&masters, "master");
        assert!(Path::new(&master.path).starts_with(tmp.path().join("out").join("p1")), "{}", master.path);
        assert_eq!((master.width, master.height), (512, 384));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- the_installed_identity cleanup_sweeps_the_tmp a_project_with_no_possible_wcs a_pinned_frame_without_a_wcs a_steady_state_project_plan a_project_run_starts_on --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`installed_solver_identity`, `tmp_root`, the origin constants); after they compile, `a_steady_state_project_plan_takes_no_write_lock` passes (it pins today's D16 behaviour) and the rest fail.

- [ ] **Step 3: Implement**

`stacking/solve.rs`:
```rust
/// The production solver's identity (plan 6b-3 D14 shares it with the plan):
/// the stored plate-solve config's hash and the installed catalog's stat-only
/// identity under `app_data`. `Err` when no catalog is installed.
pub(crate) fn solver_identity(app_data: &Path, config: &PlateSolveConfig) -> Result<String, String> {
    let catalog = crate::plate_solve::catalog_identity(app_data).map_err(|e| e.to_string())?;
    Ok(format!(
        "{}:{catalog}",
        crate::plate_solve::config::solver_config_hash(config)
    ))
}

/// [`solver_identity`] read from a catalog connection alone — the app-data
/// folder is the database file's own folder, as for [`CatalogSolver`] — so
/// the plan computes the hash the run's cache check compares. `Err` for a
/// database with no file or no installed catalog.
pub(crate) fn installed_solver_identity(conn: &Connection) -> Result<String, String> {
    let db_path = conn
        .path()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "the database has no file".to_string())?;
    let app_data = Path::new(db_path)
        .parent()
        .ok_or_else(|| "cannot determine the app data directory".to_string())?;
    solver_identity(app_data, &crate::plate_solve::config::load_config(conn))
}
```
and in `CatalogSolver::prepared`, replace the `catalog`/`identity` lines with `let identity = solver_identity(&app_data, &config)?;` (the string is byte-identical to today's, so every cached `solve` hash stays valid).

`stacking/origin.rs`, after `wcs_tier`:
```rust
/// What a run will know about one project frame's WCS (plan 6b-3 D14),
/// judged as the run's Solve sub-step will judge it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WcsOutlook {
    /// A catalog frame: the reference WCS rule does not apply (review M16).
    NotApplicable,
    /// The manifest carries one.
    Manifest,
    /// A cached solve of this content, which the run reuses.
    Solved,
    /// The run will try to solve it.
    ToSolve,
    /// It will have none: a cached failure or refusal that stands for this
    /// solver, catalog and app version, or no solver can run at all.
    Unsolvable,
}

/// The [`WcsOutlook`] of one frame. `identity` is the installed solver's
/// ([`crate::stacking::solve::installed_solver_identity`]); `None` means no
/// solver can run, so only a cached solve helps. A catalog frame has no rule
/// and reads as `NotApplicable`.
pub(crate) fn wcs_outlook(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
    identity: Option<&str>,
) -> anyhow::Result<WcsOutlook> {
    use crate::stacking::solve::{cached_outcome_holds, solve_hash, SolveArtifact};
    let (project_id, blake3) = match origin {
        FrameOrigin::Catalog { .. } => return Ok(WcsOutlook::NotApplicable),
        FrameOrigin::Project { meta, .. } if meta.wcs.is_some() => return Ok(WcsOutlook::Manifest),
        FrameOrigin::Project { project_id, blake3, .. } => (project_id, blake3),
    };
    let Some(identity) = identity else {
        return Ok(match stored_outcome(conn, project_id, frame_id, blake3)? {
            Some(SolveOutcome::Solved(_)) => WcsOutlook::Solved,
            _ => WcsOutlook::Unsolvable,
        });
    };
    let subject = StackSubject::Project(project_id.clone());
    let Some(row) = crate::db::stacking::find_artifact(conn, &subject, "", "solve", Some(frame_id))? else {
        return Ok(WcsOutlook::ToSolve);
    };
    if !cached_outcome_holds(&row, &solve_hash(blake3, identity)) {
        return Ok(WcsOutlook::ToSolve);
    }
    let artifact: SolveArtifact = serde_json::from_str(row.payload_json.as_deref().unwrap_or("null"))?;
    Ok(match artifact.outcome {
        SolveOutcome::Solved(_) => WcsOutlook::Solved,
        _ => WcsOutlook::Unsolvable,
    })
}

/// Plan 6b-3 D14 blocker: the manual pin will have no WCS.
pub(crate) const PIN_WITHOUT_WCS: &str =
    "The manual reference frame has no WCS and could not be plate-solved; choose another reference";
/// … and no catalog is installed to try.
pub(crate) const PIN_WITHOUT_WCS_NO_CATALOG: &str =
    "The manual reference frame has no WCS and the plate-solve star catalog is not installed; choose another reference";
/// Plan 6b-3 D14 warning: the pin's WCS depends on a solve still to come.
pub(crate) const PIN_TO_SOLVE: &str =
    "The manual reference frame has no WCS yet: the run plate-solves it first, and fails at Reference if it cannot";
/// Plan 6b-3 D14 blocker: no in-scope frame will have a WCS.
pub(crate) const NO_FRAME_WITH_WCS: &str =
    "No frame of this project has a WCS and none could be plate-solved; the grid needs one";
/// … and no catalog is installed to try.
pub(crate) const NO_FRAME_WITH_WCS_NO_CATALOG: &str =
    "No frame of this project has a WCS and the plate-solve star catalog is not installed; the grid needs one";

/// The plan's half of the reference WCS rule (spec §5.4, plan 6b-3 D14).
#[derive(Debug, Default)]
pub(crate) struct ReferenceWcsCheck {
    /// A `reference` blocker: the run is certain to fail at Reference.
    pub blocker: Option<String>,
    pub warning: Option<String>,
}

/// Blocks a run certain to fail at Reference — a manual pin that will have no
/// WCS, or no in-scope frame that will have one — and warns when the pin's
/// WCS depends on a solve the run has yet to try. `pinned` is ignored when it
/// is not among `origins` (the plan's own reference gate reports that).
pub(crate) fn reference_wcs_check(
    conn: &Connection,
    origins: &FrameOrigins,
    in_scope: &[i64],
    pinned: Option<i64>,
    identity: Option<&str>,
) -> anyhow::Result<ReferenceWcsCheck> {
    let mut check = ReferenceWcsCheck::default();
    if let Some((pin, origin)) = pinned.and_then(|p| origins.get(&p).map(|o| (p, o))) {
        match wcs_outlook(conn, origin, pin, identity)? {
            WcsOutlook::Unsolvable => {
                check.blocker = Some(
                    if identity.is_some() { PIN_WITHOUT_WCS } else { PIN_WITHOUT_WCS_NO_CATALOG }.to_string(),
                );
                return Ok(check);
            }
            WcsOutlook::ToSolve => check.warning = Some(PIN_TO_SOLVE.to_string()),
            WcsOutlook::NotApplicable | WcsOutlook::Manifest | WcsOutlook::Solved => {}
        }
    }
    if in_scope.is_empty() {
        return Ok(check);
    }
    for &id in in_scope {
        let Some(origin) = origins.get(&id) else { continue };
        if wcs_outlook(conn, origin, id, identity)? != WcsOutlook::Unsolvable {
            return Ok(check);
        }
    }
    check.blocker = Some(
        if identity.is_some() { NO_FRAME_WITH_WCS } else { NO_FRAME_WITH_WCS_NO_CATALOG }.to_string(),
    );
    Ok(check)
}
```
Check `SolveArtifact`, `cached_outcome_holds` and `solve_hash` visibility (`pub(crate)`/`pub` in `solve.rs`, verified at `20c7c1ef`: `solve.rs:132, 148, 171`), and that `origin.rs` already imports `stored_outcome`, `SolveOutcome`, `StackSubject`, `FrameOrigins` (it uses all four in `wcs_tier`/`local_solve_outcome`).

In the funnel test's `ALLOWLIST`, delete the `("start_stacking", "frame-set-only guard: a project start is refused until 6b-3")` entry.

`stacking/plan.rs`:
- delete `PROJECT_STACKING_UNAVAILABLE` and its doc;
- `fn project_inputs(conn, settings, subject: &StackSubject, project_id: &str, config_override)`; `build_plan`'s project arm passes `subject`;
- in `project_inputs`, right after the `unsolvable` warning:
  ```rust
    // Plan 6b-3 D14 (spec §5.4): the grid needs a reference with a WCS.
    // Judged as the run's Solve sub-step will judge it (the installed
    // solver's identity), so a cached failure blocks only while it stands.
    let identity = match crate::stacking::solve::installed_solver_identity(conn) {
        Ok(identity) => Some(identity),
        Err(why) => {
            tracing::debug!(project_id = %project_id, error = %why, "no solver identity for the reference rule");
            None
        }
    };
    let pinned = if cfg.reference.mode == ReferenceMode::Manual {
        stored_reference_frame(conn, subject).map_err(|e| ApiError::Internal(format!("{e:#}")))?
    } else {
        None
    };
    let in_scope_handles: Vec<i64> = source.frames.iter().filter(in_scope).filter_map(|f| f.handle).collect();
    let check = crate::stacking::origin::reference_wcs_check(
        conn,
        &source.origins,
        &in_scope_handles,
        pinned,
        identity.as_deref(),
    )
    .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    if let Some(message) = check.blocker {
        blockers.push(PlanBlocker {
            code: "reference".to_string(),
            message,
        });
    }
    if let Some(note) = check.warning {
        warnings.push(note);
    }
  ```
  The blocker lands after `grid` and before every gate `build_plan` adds, so `a_projects_blocker_order_is_stable` keeps its list (its only frame is not on the device, so nothing is in scope and no pin is stored).

`stacking/paths.rs`:
```rust
    /// `root/tmp` — scratch a stage writes and removes itself (the solve's
    /// ADU copy, Calibrate's scratch); only a crash leaves anything here.
    /// Plan 6b-3 D17: swept by `Intermediates` and `All`.
    pub fn tmp_root(&self) -> PathBuf {
        self.root.join("tmp")
    }
```
and in `cleanup_work`, `dirs.push(layout.tmp_root());` in both the `Intermediates` and the `All` arm (after `rej_root`); one sentence in its doc.

`stacking/run.rs`:
- the two `rc.layout.root.join("tmp")` become `rc.layout.tmp_root()`;
- `stage_calibrate`: `return Err(RunError::Other("stage 1 calibrates a frame set only; a project prepares".into()));` and drop `PROJECT_STACKING_UNAVAILABLE` from the `plan` import;
- `start_stacking` becomes:
  ```rust
pub fn start_stacking(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    subject: StackSubject,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, ApiError> {
    let solver: Arc<dyn crate::stacking::solve::FrameSolver> =
        Arc::new(crate::stacking::solve::CatalogSolver::new(ctx.clone()));
    start_stacking_with_solver(ctx, emitter, policy, app_version, subject, config, rerun_from, solver)
}

/// [`start_stacking`] with the Solve sub-step's solver given — the seam the
/// fake-hub e2e uses (spec §15.2: a synthetic star field cannot be solved
/// against a real catalog). Plan 6b-3 D13: a project starts like a frame
/// set; its run stacks onto the plan's confirmed grid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_stacking_with_solver(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    subject: StackSubject,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
    solver: Arc<dyn crate::stacking::solve::FrameSolver>,
) -> Result<StartedStacking, ApiError> {
    // … today's body from `let db_handle = db(&ctx)?;` on, without the
    // `let StackSubject::FrameSet(_) = &subject else { … };` guard …
}
  ```
  and inside the moved body:
  - right after `let (plan_groups, origins) = frame_source(&conn, &subject, &plan.config)?;`:
    ```rust
    // Plan 6b-3 D13: a project run stacks onto the confirmed grid. The
    // plan's `grid` blocker guarantees one; a frame set has none.
    let grid = plan.grid.as_ref().and_then(|g| g.current);
    if subject.project_id().is_some() && grid.is_none() {
        return Err(ApiError::Internal(
            "the project plan reported no blockers but no confirmed grid".to_string(),
        ));
    }
    ```
  - `RunSummary { …, grid, … }` (Task 2a's `grid: None` there);
  - `RunContext { …, grid: grid.map(|g| Arc::new(crate::stacking::grid_map::GridRun::new(g))), …, solver, … }` (Task 2a's `grid: None` and today's `solver: Arc::new(CatalogSolver::new(ctx.clone()))`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- the_installed_identity cleanup_sweeps_the_tmp a_project_with_no_possible_wcs a_pinned_frame_without_a_wcs a_steady_state_project_plan a_project_run_starts_on stacking::origin stacking::plan stacking::paths --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass, the funnel test included. `grep -n PROJECT_STACKING_UNAVAILABLE -r crates/` prints nothing.

- [ ] **Step 5: Run the gates.** `git diff --exit-code src/types`.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/run.rs crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/stacking/origin.rs crates/athenaeum-core/src/stacking/solve.rs crates/athenaeum-core/src/stacking/paths.rs
git commit -m "feat(stacking): a project run starts on its confirmed grid; the plan blocks a reference that cannot carry a WCS

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 8b: The seven project commands, core (D12, D18; spec §9.1, §9.3)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `project_seed` (private) and `read_project_config` (`pub(crate)`); `resolve_project_config_and_exclusions` (`~1068`) seeds through `project_seed` (same bytes).
- Modify `crates/athenaeum-core/src/db/stacking.rs`: `StackingRunFrameRow.frame_uuid` (`~243`, `FRAME_COLUMNS` `~285`, `row_to_frame` `~264`); `sweep_departed_project_artifacts`; two tests.
- Modify `crates/athenaeum-core/src/api/stacking.rs`: `StackingProjectConfig`; `runs_for_subject` (shared by both history functions); `get_project_stacking_plan`, `start_project_stacking`, `get_project_stacking_runs`, `get_project_stacking_config`, `set_project_stacking_config`, `get_project_stacking_work_usage`, `cleanup_project_stacking_work`; three tests.
- Modify `crates/athenaeum-core/src/ts_export.rs`: `crate::api::stacking::StackingProjectConfig` after `StackingSetConfig` (`~446`).
- `src/types/stacking.ts`: regenerated — `StackingRunFrameRow.frameUuid: string | null`, the new `StackingProjectConfig`.

**Interfaces:**
```rust
// api/stacking.rs
pub struct StackingProjectConfig { pub config: StackingConfig, pub excluded_frame_uuids: Vec<String>, pub reference_frame_uuid: Option<String>, pub is_default: bool, pub updated_at: Option<String> }
pub fn get_project_stacking_plan(ctx: &ServiceContext, policy: &PathPolicy, project_id: &str, config: Option<StackingConfig>) -> Result<StackingPlan, ApiError>;
pub fn start_project_stacking(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, policy: &PathPolicy, app_version: String, project_id: &str, config: Option<StackingConfig>, rerun_from: Option<Stage>) -> Result<StartedStacking, ApiError>;
pub fn get_project_stacking_runs(ctx: &ServiceContext, project_id: &str, limit: Option<usize>) -> Result<Vec<StackingRunSummary>, ApiError>;
pub fn get_project_stacking_config(ctx: &ServiceContext, project_id: &str) -> Result<StackingProjectConfig, ApiError>;
pub fn set_project_stacking_config(ctx: &ServiceContext, project_id: &str, config: StackingConfig, excluded_frame_uuids: Vec<String>, reference_frame_uuid: Option<String>) -> Result<StackingProjectConfig, ApiError>;
pub fn get_project_stacking_work_usage(ctx: &ServiceContext, project_id: &str) -> Result<WorkUsage, ApiError>;
pub fn cleanup_project_stacking_work(ctx: &ServiceContext, project_id: &str, what: CleanupWhat) -> Result<u64, ApiError>;
// db/stacking.rs
pub struct StackingRunFrameRow { /* … */ pub frame_uuid: Option<String> }
pub fn sweep_departed_project_artifacts(conn: &Connection, project_id: &str) -> Result<usize>;
// stacking/plan.rs
pub(crate) fn read_project_config(conn: &Connection, settings: &SettingsManager, project_id: &str) -> Result<(StackingConfig, Option<ProjectConfigRow>), ApiError>;
```
Consumed by: Task 8c (the wrappers), Task 9 (the e2e).

- [ ] **Step 1: Write the failing tests**

`db/stacking.rs`, `mod tests` (after `a_project_config_is_seeded_once_and_never_overwritten`):
```rust
    /// Plan 6b-3 D18: a project run's frame row names its uuid through its
    /// handle — even after the frame left `project_frames_local` — and a
    /// frame set's row names none.
    #[test]
    fn a_run_frame_row_names_its_project_uuid() {
        let c = conn();
        insert_test_project(&c, "p1");
        let handles = project_frame_handles(&c, "p1", &["u1"]).unwrap();
        let project = StackSubject::Project("p1".into());
        let run = insert_run(&c, &new_run(&project)).unwrap();
        let group = insert_group(&c, &new_group(run)).unwrap();
        upsert_frame_row(&c, &project, &new_frame_row(run, group, handles["u1"], true)).unwrap();
        let rows = list_frame_rows(&c, run).unwrap();
        assert_eq!(rows[0].frame_id, handles["u1"]);
        assert_eq!(rows[0].frame_uuid.as_deref(), Some("u1"));

        let set = seed_set(&c);
        seed_frame(&c, 41);
        let set_subject = StackSubject::FrameSet(set);
        let set_run = insert_run(&c, &new_run(&set_subject)).unwrap();
        let set_group = insert_group(&c, &new_group(set_run)).unwrap();
        upsert_frame_row(&c, &set_subject, &new_frame_row(set_run, set_group, 41, true)).unwrap();
        assert_eq!(list_frame_rows(&c, set_run).unwrap()[0].frame_uuid, None);
    }

    /// Spec §9.3, plan 6b-3 D12: the sweep drops the artifact rows of a
    /// handle whose uuid left `project_frames_local` — every kind, rows only
    /// — and keeps a present frame's, a coordinator-excluded frame's, a
    /// group-level row and another project's.
    #[test]
    fn the_sweep_drops_only_departed_frames_rows() {
        let c = conn();
        insert_test_project(&c, "p1");
        insert_test_project(&c, "p2");
        let h = project_frame_handles(&c, "p1", &["kept", "excluded", "gone"]).unwrap();
        let other = project_frame_handles(&c, "p2", &["gone"]).unwrap();
        for (uuid, accepted) in [("kept", 1), ("excluded", 0)] {
            c.execute(
                "INSERT INTO project_frames_local (project_id, frame_uuid, content_version, origin,
                   publisher_account_id, publisher_display, file_name, filter_canonical, state,
                   accepted, byte_size, xxh3, blake3)
                 VALUES ('p1', ?1, 1, 'replica', 'a', 'Ann', 'f.fits', 'L', 'published', ?2, 1, 'x', 'b')",
                params![uuid, accepted],
            )
            .unwrap();
        }
        let p1 = StackSubject::Project("p1".into());
        let p2 = StackSubject::Project("p2".into());
        let row = |subject: &StackSubject, frame: Option<i64>, group: &str, kind: &str| {
            upsert_artifact(
                &c,
                &NewArtifact {
                    subject,
                    frame_id: frame,
                    group_key: group,
                    kind,
                    path: None,
                    config_hash: "h",
                    size: None,
                    modified_at: None,
                    payload_json: Some("{}"),
                },
            )
            .unwrap();
        };
        for uuid in ["kept", "excluded", "gone"] {
            row(&p1, Some(h[uuid]), "", "solve");
            row(&p1, Some(h[uuid]), "g", "registered");
        }
        row(&p1, None, "g", "ln_reference");
        row(&p2, Some(other["gone"]), "g", "registered");

        assert_eq!(sweep_departed_project_artifacts(&c, "p1").unwrap(), 2);
        assert!(find_artifact(&c, &p1, "", "solve", Some(h["gone"])).unwrap().is_none());
        assert!(find_artifact(&c, &p1, "g", "registered", Some(h["gone"])).unwrap().is_none());
        for uuid in ["kept", "excluded"] {
            assert!(find_artifact(&c, &p1, "", "solve", Some(h[uuid])).unwrap().is_some(), "{uuid}");
        }
        assert!(find_artifact(&c, &p1, "g", "ln_reference", None).unwrap().is_some());
        assert!(find_artifact(&c, &p2, "g", "registered", Some(other["gone"])).unwrap().is_some());
        assert_eq!(sweep_departed_project_artifacts(&c, "p1").unwrap(), 0, "idempotent");
    }
```
(`seed_frame(c, id)`, `seed_set`, `new_run`, `new_group`, `new_frame_row` and `conn()` are this module's own test helpers, `db/stacking.rs:1300-1330, 1779-1830`; `insert_test_project` comes in through `use super::*`.)

Verified with `sqlite3 :memory:` at plan time on the schema's own columns: the sweep's `DELETE` removes exactly the two `gone` rows of `p1`, and the correlated `frame_uuid` sub-select maps a handle to its uuid with no `project_frames_local` row.

`api/stacking.rs`, `mod tests`:
```rust
    /// Plan 6b-3 D12: a project's config reads without writing — the seed it
    /// would store, `isDefault` — and an unknown project is `NotFound`. A
    /// save stores sorted, de-duplicated exclusions; an absent reference
    /// clears the pin; a project this device left is refused.
    #[test]
    fn a_project_config_reads_without_writing_and_saves_whole() {
        let (_tmp, ctx) = test_ctx();
        {
            let handle = db(&ctx).unwrap();
            crate::db::stacking::insert_test_project(&handle.conn(), "p1");
        }
        let view = get_project_stacking_config(&ctx, "p1").unwrap();
        assert!(view.is_default && view.updated_at.is_none());
        assert_eq!(
            view.config,
            crate::stacking::config::project_seed_config(StackingConfig::default())
        );
        assert!(crate::db::stacking::get_project_config(&db(&ctx).unwrap().conn(), "p1")
            .unwrap()
            .is_none(), "a read stores nothing");
        assert!(matches!(get_project_stacking_config(&ctx, "nope"), Err(ApiError::NotFound(_))));

        let saved = set_project_stacking_config(
            &ctx,
            "p1",
            view.config.clone(),
            vec!["b".into(), "a".into(), "b".into()],
            Some("r".into()),
        )
        .unwrap();
        assert!(!saved.is_default && saved.updated_at.is_some());
        assert_eq!(saved.excluded_frame_uuids, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(saved.reference_frame_uuid.as_deref(), Some("r"));
        let cleared = set_project_stacking_config(&ctx, "p1", view.config.clone(), Vec::new(), None).unwrap();
        assert_eq!(cleared.reference_frame_uuid, None);
        assert!(cleared.excluded_frame_uuids.is_empty());

        crate::db::collab::mark_lost(&db(&ctx).unwrap().conn(), "p1").unwrap();
        let err = set_project_stacking_config(&ctx, "p1", view.config, Vec::new(), None).unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    /// Spec §9.3, plan 6b-3 D12: project cleanup sweeps the rows of frames
    /// that left the project — even with no working folder at all — then
    /// honours the project's own folder for usage and cleanup, and refuses
    /// while a project run is active (before the sweep, review M5).
    #[test]
    fn project_cleanup_sweeps_departed_frames_then_cleans_its_folder() {
        let (_tmp, ctx) = test_ctx();
        let subject = StackSubject::Project("p1".into());
        let gone = {
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            crate::db::stacking::insert_test_project(&conn, "p1");
            let h = crate::db::stacking::project_frame_handles(&conn, "p1", &["gone"]).unwrap();
            crate::db::stacking::upsert_artifact(
                &conn,
                &crate::db::stacking::NewArtifact {
                    subject: &subject,
                    frame_id: Some(h["gone"]),
                    group_key: "",
                    kind: "solve",
                    path: None,
                    config_hash: "h",
                    size: None,
                    modified_at: None,
                    payload_json: Some("{}"),
                },
            )
            .unwrap();
            h["gone"]
        };
        assert_eq!(cleanup_project_stacking_work(&ctx, "p1", CleanupWhat::Registered).unwrap(), 0);
        assert!(crate::db::stacking::find_artifact(&db(&ctx).unwrap().conn(), &subject, "", "solve", Some(gone))
            .unwrap()
            .is_none(), "swept with no working folder");

        let work = tempfile::tempdir().unwrap();
        let group_dir = work.path().join("_projects").join("p1").join("registered").join("g");
        std::fs::create_dir_all(&group_dir).unwrap();
        std::fs::write(group_dir.join("x.fits"), vec![0u8; 1024]).unwrap();
        let mut cfg = crate::stacking::config::project_seed_config(StackingConfig::default());
        cfg.paths.working_dir = Some(work.path().to_string_lossy().into_owned());
        set_project_stacking_config(&ctx, "p1", cfg, Vec::new(), None).unwrap();
        assert_eq!(get_project_stacking_work_usage(&ctx, "p1").unwrap().registered_bytes, 1024);
        assert_eq!(cleanup_project_stacking_work(&ctx, "p1", CleanupWhat::Registered).unwrap(), 1024);
        assert!(!group_dir.join("x.fits").exists());

        let run_id = {
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            let run_id = crate::db::stacking::insert_run(
                &conn,
                &crate::db::stacking::NewRun {
                    subject: &subject,
                    config_json: "{}",
                    config_hash: "h",
                    reference_frame_id: None,
                    reference_mode: "auto",
                    working_dir: "/w",
                    output_dir: "/o",
                },
            )
            .unwrap();
            crate::db::stacking::set_run_status(&conn, run_id, "running").unwrap();
            run_id
        };
        ctx.active_stacks.lock().unwrap().insert(
            run_id,
            crate::services::StackHandle {
                cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                subject: subject.clone(),
            },
        );
        let err = cleanup_project_stacking_work(&ctx, "p1", CleanupWhat::All).unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");
        let runs = get_project_stacking_runs(&ctx, "p1", None).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run.id, run_id);
    }

    /// Plan 6b-3 D12: a project plan heals an interrupted project run first,
    /// as a frame set's does.
    #[test]
    fn a_project_plan_heals_an_interrupted_project_run() {
        let (_tmp, ctx) = test_ctx();
        let run_id = {
            let handle = db(&ctx).unwrap();
            let conn = handle.conn();
            crate::db::stacking::insert_test_project(&conn, "p1");
            let run_id = crate::db::stacking::insert_run(
                &conn,
                &crate::db::stacking::NewRun {
                    subject: &StackSubject::Project("p1".into()),
                    config_json: "{}",
                    config_hash: "h",
                    reference_frame_id: None,
                    reference_mode: "auto",
                    working_dir: "/w",
                    output_dir: "/o",
                },
            )
            .unwrap();
            crate::db::stacking::set_run_status(&conn, run_id, "running").unwrap();
            run_id
        };
        let plan = get_project_stacking_plan(&ctx, &PathPolicy::AllowAll, "p1", None).unwrap();
        assert_eq!(plan.active_run_id, None);
        assert_eq!(plan.project_id.as_deref(), Some("p1"));
        let row = crate::db::stacking::get_run(&db(&ctx).unwrap().conn(), run_id).unwrap().unwrap();
        assert_eq!(row.status, "failed");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_run_frame_row_names the_sweep_drops a_project_config_reads project_cleanup_sweeps a_project_plan_heals --test-threads=4 2>&1 | tail -20`
Expected: compile errors (`frame_uuid`, `sweep_departed_project_artifacts`, the seven functions).

- [ ] **Step 3: Implement**

`stacking/plan.rs`:
```rust
/// The document a project's first plan stores (spec §9.4): the global
/// default with the project seed applied.
fn project_seed(conn: &Connection, settings: &SettingsManager) -> Result<StackingConfig, ApiError> {
    let global_json = read_global_config_json(conn, settings);
    let global = resolve_config(None, global_json.as_deref())
        .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?;
    Ok(crate::stacking::config::project_seed_config(global))
}

/// A project's stacking document, read only (plan 6b-3 D12): the stored one,
/// resolved whole, or — with no stored row — the seed the first plan would
/// store. Never writes. The config view, usage and cleanup read through
/// here; only a plan seeds.
pub(crate) fn read_project_config(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
) -> Result<(StackingConfig, Option<crate::db::stacking::ProjectConfigRow>), ApiError> {
    match crate::db::stacking::get_project_config(conn, project_id)? {
        Some(row) => {
            let config = resolve_config(Some(&row.config_json), None)
                .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?;
            Ok((config, Some(row)))
        }
        None => Ok((project_seed(conn, settings)?, None)),
    }
}
```
and in `resolve_project_config_and_exclusions`'s `None` arm, build the seed as `serde_json::to_string(&project_seed(conn, settings)?)` (the same bytes as today).

`db/stacking.rs`:
- `StackingRunFrameRow`, last field:
  ```rust
    // A project row's frame uuid, read through its handle, so run history
    // maps a handle after the frame left the plan (plan 6b-3 D18); `None`
    // for a frame set's row.
    pub frame_uuid: Option<String>,
  ```
- `FRAME_COLUMNS` gains `, (SELECT h.frame_uuid FROM stack_project_frames h WHERE h.id = stacking_run_frames.project_frame)` at its end, and `row_to_frame` reads `frame_uuid: r.get(16)?`;
- after `seed_project_config`:
  ```rust
/// Plan 6b-3 D12 (spec §9.3): drop the artifact rows of every handle whose
/// `(project, uuid)` has left `project_frames_local` — every kind, rows
/// only, never a file (the next directory-level cleanup takes those). A
/// coordinator-excluded frame is still in `project_frames_local`, so its
/// cache survives a restore; run history keeps its handles. One statement,
/// so no read-then-write transaction.
pub fn sweep_departed_project_artifacts(conn: &Connection, project_id: &str) -> Result<usize> {
    let count = conn.execute(
        "DELETE FROM stacking_artifacts
          WHERE project_id = ?1
            AND project_frame IN (
              SELECT h.id FROM stack_project_frames h
               WHERE h.project_id = ?1
                 AND NOT EXISTS (SELECT 1 FROM project_frames_local l
                                  WHERE l.project_id = h.project_id
                                    AND l.frame_uuid = h.frame_uuid))",
        params![project_id],
    )?;
    tracing::debug!(project_id, count, "stale project-frame artifacts swept");
    Ok(count)
}
  ```

`api/stacking.rs` — the DTO beside `StackingSetConfig`:
```rust
/// A project's stacking document (spec §9.1, plan 6b-3 D12): the whole
/// config, the manual exclusions and the manual reference by frame uuid.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingProjectConfig {
    pub config: StackingConfig,
    pub excluded_frame_uuids: Vec<String>,
    pub reference_frame_uuid: Option<String>,
    // `true` while the project has no stored document: `config` is then the
    // seed its first plan stores.
    pub is_default: bool,
    pub updated_at: Option<String>,
}
```
`get_stacking_runs`' body moves into a private `fn runs_for_subject(ctx: &ServiceContext, subject: &StackSubject, limit: Option<usize>) -> Result<Vec<StackingRunSummary>, ApiError>`, and `get_stacking_runs` calls it with `&StackSubject::FrameSet(set_id)`. Then, after `cleanup_stacking_work`:
```rust
// ── Project stacking (spec §9.1, plan 6b-3 D12) ──────────────────────────

/// A project's plan, [`get_stacking_plan`]'s twin. The first plan seeds the
/// project's config and a plan mints handles for new frames (both read
/// first, so a steady-state plan writes nothing — D16); the hosts run it on
/// a blocking thread.
pub fn get_project_stacking_plan(
    ctx: &ServiceContext,
    policy: &PathPolicy,
    project_id: &str,
    config: Option<StackingConfig>,
) -> Result<StackingPlan, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    run::heal_interrupted_runs(ctx, &conn)?;
    build_plan(
        &conn,
        &ctx.settings,
        policy,
        &StackSubject::Project(project_id.to_string()),
        config,
    )
}

/// Start a project run, [`start_stacking`]'s twin (the same drizzle clamp).
pub fn start_project_stacking(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    project_id: &str,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, ApiError> {
    let was_drizzle = rerun_from == Some(Stage::Drizzle);
    let rerun_from = clamp_rerun_from(rerun_from);
    if was_drizzle {
        tracing::debug!(project_id, "rerun from drizzle clamped to integrate");
    }
    run::start_stacking(
        ctx,
        emitter,
        policy,
        app_version,
        StackSubject::Project(project_id.to_string()),
        config,
        rerun_from,
    )
}

/// A project's run history, newest first (default limit 20).
pub fn get_project_stacking_runs(
    ctx: &ServiceContext,
    project_id: &str,
    limit: Option<usize>,
) -> Result<Vec<StackingRunSummary>, ApiError> {
    runs_for_subject(ctx, &StackSubject::Project(project_id.to_string()), limit)
}

/// The view of a project's document: the stored one, or the seed.
fn project_config_view(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
) -> Result<StackingProjectConfig, ApiError> {
    let (config, row) = crate::stacking::plan::read_project_config(conn, settings, project_id)?;
    Ok(match row {
        Some(row) => StackingProjectConfig {
            config,
            excluded_frame_uuids: row.excluded_frame_uuids,
            reference_frame_uuid: row.reference_frame_uuid,
            is_default: false,
            updated_at: Some(row.updated_at),
        },
        None => StackingProjectConfig {
            config,
            excluded_frame_uuids: Vec::new(),
            reference_frame_uuid: None,
            is_default: true,
            updated_at: None,
        },
    })
}

/// A project's stacking document (plan 6b-3 D12). Read only: with no stored
/// document it returns the seed the first plan would store, `isDefault`.
pub fn get_project_stacking_config(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<StackingProjectConfig, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    subject_label(&conn, &StackSubject::Project(project_id.to_string()))?;
    project_config_view(&conn, &ctx.settings, project_id)
}

/// Store a project's whole stacking document (plan 6b-3 D12) and return its
/// view. Refuses a project this device left. Exclusions are stored sorted
/// and de-duplicated. `reference_frame_uuid = None` CLEARS the manual pin:
/// Tauri cannot tell an absent argument from `null`, so a caller that keeps
/// the pin sends it back (the exclusions, by contrast, are required).
pub fn set_project_stacking_config(
    ctx: &ServiceContext,
    project_id: &str,
    config: StackingConfig,
    mut excluded_frame_uuids: Vec<String>,
    reference_frame_uuid: Option<String>,
) -> Result<StackingProjectConfig, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    crate::api::collab_exchange::live_project(&conn, project_id)?;
    excluded_frame_uuids.sort();
    excluded_frame_uuids.dedup();
    let reference = reference_frame_uuid
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let json = serde_json::to_string(&config)
        .map_err(|e| ApiError::Internal(format!("failed to serialize stacking config: {e}")))?;
    crate::db::stacking::set_project_config(&conn, project_id, &json, &excluded_frame_uuids, reference)?;
    tracing::info!(
        project_id,
        count = excluded_frame_uuids.len(),
        "project stacking config saved"
    );
    project_config_view(&conn, &ctx.settings, project_id)
}

/// The working folder a project's plan resolves: its document's
/// `paths.workingDir`, else the global setting. Reads only.
fn project_working_dir(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
) -> Result<Option<String>, ApiError> {
    let (cfg, _) = crate::stacking::plan::read_project_config(conn, settings, project_id)?;
    Ok(resolve_dirs(conn, settings, &cfg.paths).working)
}

/// A project's working-layout byte usage, as for a set. No working folder
/// reads as all zeros.
pub fn get_project_stacking_work_usage(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<WorkUsage, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let subject = StackSubject::Project(project_id.to_string());
    subject_label(&conn, &subject)?;
    let Some(working) = project_working_dir(&conn, &ctx.settings, project_id)? else {
        return Ok(WorkUsage::default());
    };
    let layout = layout_for(&conn, Path::new(&working), &subject)?;
    Ok(work_usage(&layout))
}

/// Clean a project's working layout (plan 6b-3 D12, spec §9.3). As for a
/// set: heal, then `Conflict` while a project run is active. Then, at every
/// level and even with no working folder, the artifact rows of frames that
/// left the project are swept (rows only; review M5 put the sweep after the
/// `Conflict` check, so a refused cleanup touches nothing). Then `Ok(0)`
/// with no working folder. A layout root that overlaps a scan root (the
/// Collaboration root is one) is refused, as the plan's `folders` blocker
/// refuses it (6b-2 C6) — cleanup removes whole subtrees and a landed
/// replica must never be in one.
pub fn cleanup_project_stacking_work(
    ctx: &ServiceContext,
    project_id: &str,
    what: CleanupWhat,
) -> Result<u64, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let subject = StackSubject::Project(project_id.to_string());
    subject_label(&conn, &subject)?;
    run::heal_interrupted_runs(ctx, &conn)?;
    if let Some(run_id) = active_run_for_subject(&conn, &subject)? {
        return Err(ApiError::Conflict(format!(
            "a stacking run (id {run_id}) is active for project {project_id}"
        )));
    }
    crate::db::stacking::sweep_departed_project_artifacts(&conn, project_id)?;
    let Some(working) = project_working_dir(&conn, &ctx.settings, project_id)? else {
        return Ok(0);
    };
    let layout = layout_for(&conn, Path::new(&working), &subject)?;
    if let Some(why) = crate::stacking::paths::project_root_overlap(&conn, &layout.root)? {
        tracing::warn!(project_id, path = %layout.root.display(), "project cleanup refused: the working folder overlaps a scan root");
        return Err(ApiError::Invalid(why));
    }
    Ok(cleanup_work(&conn, &subject, &layout, what)?)
}
```
Add the imports the compiler asks for (`Connection`, `SettingsManager`, `ProgressEmitter` are already used in this module).

`ts_export.rs`: `crate::api::stacking::StackingProjectConfig,` after `crate::api::stacking::StackingSetConfig,`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- a_run_frame_row_names the_sweep_drops a_project_config_reads project_cleanup_sweeps a_project_plan_heals db::stacking api::stacking stacking::plan --test-threads=4 2>&1 | grep -E '^test result|FAILED|panicked' | head`
Expected: all pass.

- [ ] **Step 5: Run the gates.** `git diff --stat src/types` shows only `src/types/stacking.ts`: `StackingRunFrameRow.frameUuid` and `StackingProjectConfig`. `npx tsc --noEmit -p .` stays clean (no frontend literal builds a `StackingRunFrameRow`; research 6b-2 §0.3).

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-core/src/stacking/plan.rs crates/athenaeum-core/src/db/stacking.rs crates/athenaeum-core/src/api/stacking.rs crates/athenaeum-core/src/ts_export.rs src/types/stacking.ts
git commit -m "feat(stacking): the seven project stacking commands in core; run history names a project frame's uuid

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 8c: The seven commands on both hosts (D12; spec §9.1)

**Files:**
- Modify `crates/athenaeum-tauri/src/commands/stacking.rs`: seven wrappers; `StackingProjectConfig` in the `pub use` list.
- Modify `crates/athenaeum-tauri/src/lib.rs`: register the seven after `commands::cleanup_stacking_work,` (`~464`).
- Modify `crates/athenaeum-web/src/routes/stacking.rs`: five arg structs, seven handlers, three tests.
- Modify `crates/athenaeum-web/src/routes/mod.rs`: seven routes after `/api/cleanup_stacking_work` (`~256`).

No core change, no TS change.

**Interfaces:** the wire names, identical on both hosts:

| Command | Args (camelCase) | Returns | Blocking thread |
| ---- | ---- | ---- | ---- |
| `get_project_stacking_plan` | `projectId`, `config?` | `StackingPlan` | yes |
| `start_project_stacking` | `projectId`, `config?`, `rerunFrom?` | `StartedStacking` | yes |
| `get_project_stacking_runs` | `projectId`, `limit?` | `StackingRunSummary[]` | no |
| `get_project_stacking_config` | `projectId` | `StackingProjectConfig` | no |
| `set_project_stacking_config` | `projectId`, `config`, `excludedFrameUuids` (required), `referenceFrameUuid?` | `StackingProjectConfig` | no |
| `get_project_stacking_work_usage` | `projectId` | `WorkUsage` | yes |
| `cleanup_project_stacking_work` | `projectId`, `what` | `number` (bytes freed) | yes |

- [ ] **Step 1: Write the failing web tests**

`crates/athenaeum-web/src/routes/stacking.rs`, `mod tests`:
```rust
    /// Plan 6b-3 Task 8c: every project route is registered past the auth
    /// layer — with no DB behind `test_state(None)` each handler's own DB
    /// lookup fails as 500, never 404 or 401.
    #[tokio::test]
    async fn project_stacking_routes_are_registered() {
        for (uri, body) in [
            ("/api/get_project_stacking_plan", r#"{"projectId":"p1"}"#),
            ("/api/start_project_stacking", r#"{"projectId":"p1"}"#),
            ("/api/get_project_stacking_runs", r#"{"projectId":"p1"}"#),
            ("/api/get_project_stacking_config", r#"{"projectId":"p1"}"#),
            ("/api/get_project_stacking_work_usage", r#"{"projectId":"p1"}"#),
            ("/api/cleanup_project_stacking_work", r#"{"projectId":"p1","what":"registered"}"#),
        ] {
            let router = crate::routes::build_router(test_state(None), None);
            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
        }
    }

    /// Plan 6b-3 D12: the project args are camelCase; `excludedFrameUuids`
    /// is required (an omitting caller must not wipe the exclusions) while
    /// `referenceFrameUuid`, `config`, `rerunFrom` and `limit` may be absent.
    #[test]
    fn project_stacking_args_are_camel_case() {
        let cfg = serde_json::to_value(StackingConfig::default()).unwrap();
        let set: SetProjectStackingConfigArgs = serde_json::from_value(serde_json::json!({
            "projectId": "p1",
            "config": cfg,
            "excludedFrameUuids": ["a"],
            "referenceFrameUuid": "r",
        }))
        .unwrap();
        assert_eq!(set.excluded_frame_uuids, vec!["a".to_string()]);
        assert_eq!(set.reference_frame_uuid.as_deref(), Some("r"));
        let no_pin: SetProjectStackingConfigArgs = serde_json::from_value(serde_json::json!({
            "projectId": "p1",
            "config": cfg,
            "excludedFrameUuids": [],
        }))
        .unwrap();
        assert_eq!(no_pin.reference_frame_uuid, None);
        assert!(serde_json::from_value::<SetProjectStackingConfigArgs>(serde_json::json!({
            "projectId": "p1",
            "config": cfg,
        }))
        .is_err());
        let start: StartProjectStackingArgs =
            serde_json::from_value(serde_json::json!({"projectId": "p1", "rerunFrom": "integrate"})).unwrap();
        assert_eq!(start.rerun_from, Some(Stage::Integrate));
        assert!(start.config.is_none());
        let runs: GetProjectStackingRunsArgs = serde_json::from_value(serde_json::json!({"projectId": "p1"})).unwrap();
        assert_eq!((runs.project_id.as_str(), runs.limit), ("p1", None));
    }

    /// Plan 6b-3 D12: on a real catalog the config route reads the seed
    /// (`isDefault`, coverage on) and stores nothing; a save without
    /// `excludedFrameUuids` is a 422.
    #[tokio::test]
    async fn project_config_routes_read_the_seed_and_require_exclusions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = athenaeum_core::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        db.conn()
            .execute(
                "INSERT INTO collab_projects (project_id, slug, title, data_role, target_name,
                   target_ra_deg, target_dec_deg, target_radius_deg, membership_version,
                   snapshot_payload_b64, snapshot_signature_b64, members_json)
                 VALUES ('p1', 'p1', 'p1', 'processor', 'M42', 83.8, -5.4, 1.0, 1, '', '', '[]')",
                [],
            )
            .unwrap();
        let state = db_test_state(db);
        let (status, body) =
            post_json(state.clone(), "/api/get_project_stacking_config", r#"{"projectId":"p1"}"#.into()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["isDefault"], true);
        assert_eq!(body["config"]["output"]["coverageMap"], true);
        assert_eq!(body["excludedFrameUuids"], serde_json::json!([]));

        let cfg = serde_json::to_string(&body["config"]).unwrap();
        let (status, _) = post_json(
            state.clone(),
            "/api/set_project_stacking_config",
            format!(r#"{{"projectId":"p1","config":{cfg}}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, body) = post_json(
            state,
            "/api/set_project_stacking_config",
            format!(r#"{{"projectId":"p1","config":{cfg},"excludedFrameUuids":["b","a"]}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["isDefault"], false);
        assert_eq!(body["excludedFrameUuids"], serde_json::json!(["a", "b"]));
    }
```
`post_json` (`~923`) returns `(StatusCode, serde_json::Value)` and reads a non-JSON body (the 422's plain-text rejection) as `Null`.

- [ ] **Step 2: Run the web tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- project_stacking project_config_routes --test-threads=4 2>&1 | tail -20`
Expected: compile errors (the arg structs).

- [ ] **Step 3: Implement**

`crates/athenaeum-tauri/src/commands/stacking.rs` — add `StackingProjectConfig` to the `pub use athenaeum_core::api::stacking::{…}` list, then after `cleanup_stacking_work`:
```rust
// ── Project stacking (collab wave 6b-3, spec §9.1) ───────────────────────

/// A project's plan. A project plan can seed its config and mint frame
/// handles, so it runs on a blocking thread like the set's.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_project_stacking_plan(
    state: State<'_, AppState>,
    project_id: String,
    config: Option<StackingConfig>,
) -> Result<StackingPlan, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || {
        api::get_project_stacking_plan(&ctx, &POLICY, &project_id, config)
    })
    .await
    .map_err(|e| format!("Plan task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

/// Start a project run; events arrive as for a set.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_project_stacking(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    project_id: String,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, String> {
    let ctx = state.ctx.clone();
    let emitter = Arc::new(TauriProgressEmitter(app_handle));
    tokio::task::spawn_blocking(move || {
        api::start_project_stacking(
            ctx,
            emitter,
            &POLICY,
            env!("CARGO_PKG_VERSION").to_string(),
            &project_id,
            config,
            rerun_from,
        )
    })
    .await
    .map_err(|e| format!("Start task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

/// A project's run history, newest first.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_project_stacking_runs(
    state: State<'_, AppState>,
    project_id: String,
    limit: Option<usize>,
) -> Result<Vec<StackingRunSummary>, String> {
    api::get_project_stacking_runs(&state.ctx, &project_id, limit).map_err(|e| e.to_string())
}

/// A project's stacking document; with none stored, the seed (read only).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_project_stacking_config(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<StackingProjectConfig, String> {
    api::get_project_stacking_config(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// Store a project's whole stacking document. An absent
/// `referenceFrameUuid` clears the manual reference (Tauri cannot tell
/// absent from `null`); `excludedFrameUuids` is required.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_project_stacking_config(
    state: State<'_, AppState>,
    project_id: String,
    config: StackingConfig,
    excluded_frame_uuids: Vec<String>,
    reference_frame_uuid: Option<String>,
) -> Result<StackingProjectConfig, String> {
    api::set_project_stacking_config(
        &state.ctx,
        &project_id,
        config,
        excluded_frame_uuids,
        reference_frame_uuid,
    )
    .map_err(|e| e.to_string())
}

/// A project's working-layout byte usage.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_project_stacking_work_usage(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<WorkUsage, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_project_stacking_work_usage(&ctx, &project_id))
        .await
        .map_err(|e| format!("Work-usage task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// Clean a project's working layout. Refuses while a project run is active;
/// otherwise sweeps the rows of frames that left the project first.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cleanup_project_stacking_work(
    state: State<'_, AppState>,
    project_id: String,
    what: CleanupWhat,
) -> Result<u64, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::cleanup_project_stacking_work(&ctx, &project_id, what))
        .await
        .map_err(|e| format!("Cleanup task panicked: {e}"))?
        .map_err(|e| e.to_string())
}
```

`crates/athenaeum-tauri/src/lib.rs`, after `commands::cleanup_stacking_work,`:
```rust
            // Project stacking (collab wave 6b-3)
            commands::get_project_stacking_plan,
            commands::start_project_stacking,
            commands::get_project_stacking_runs,
            commands::get_project_stacking_config,
            commands::set_project_stacking_config,
            commands::get_project_stacking_work_usage,
            commands::cleanup_project_stacking_work,
```

`crates/athenaeum-web/src/routes/stacking.rs` — `StackingProjectConfig` in the `pub use` list; the arg structs after `CleanupStackingWorkArgs`:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetProjectStackingPlanArgs {
    pub project_id: String,
    #[serde(default)]
    pub config: Option<StackingConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartProjectStackingArgs {
    pub project_id: String,
    #[serde(default)]
    pub config: Option<StackingConfig>,
    #[serde(default)]
    pub rerun_from: Option<Stage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetProjectStackingRunsArgs {
    pub project_id: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `get_project_stacking_config` and `get_project_stacking_work_usage`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStackingArgs {
    pub project_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetProjectStackingConfigArgs {
    pub project_id: String,
    pub config: StackingConfig,
    // REQUIRED, as for a set: an omitting caller must not wipe the
    // project's manual exclusions.
    pub excluded_frame_uuids: Vec<String>,
    // Absent clears the manual reference, exactly as on desktop.
    #[serde(default)]
    pub reference_frame_uuid: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupProjectStackingWorkArgs {
    pub project_id: String,
    pub what: CleanupWhat,
}
```
and the handlers after `cleanup_stacking_work`:
```rust
// ── Project stacking (collab wave 6b-3, spec §9.1) ───────────────────────

/// POST /api/get_project_stacking_plan
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_project_stacking_plan(
    State(state): State<WebAppState>,
    Json(args): Json<GetProjectStackingPlanArgs>,
) -> Result<Json<StackingPlan>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::get_project_stacking_plan(&ctx, &policy, &args.project_id, args.config)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Plan task panicked: {}", e)))?
    .map_err(api_err)?;
    Ok(Json(result))
}

/// POST /api/start_project_stacking
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_project_stacking(
    State(state): State<WebAppState>,
    Json(args): Json<StartProjectStackingArgs>,
) -> Result<Json<StartedStacking>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    let ctx = state.ctx.clone();
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    let result = tokio::task::spawn_blocking(move || {
        api::start_project_stacking(
            ctx,
            emitter,
            &policy,
            env!("CARGO_PKG_VERSION").to_string(),
            &args.project_id,
            args.config,
            args.rerun_from,
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Start task panicked: {}", e)))?
    .map_err(api_err)?;
    Ok(Json(result))
}

/// POST /api/get_project_stacking_runs
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_project_stacking_runs(
    State(state): State<WebAppState>,
    Json(args): Json<GetProjectStackingRunsArgs>,
) -> Result<Json<Vec<StackingRunSummary>>, (StatusCode, String)> {
    api::get_project_stacking_runs(&state.ctx, &args.project_id, args.limit)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_project_stacking_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_project_stacking_config(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectStackingArgs>,
) -> Result<Json<StackingProjectConfig>, (StatusCode, String)> {
    api::get_project_stacking_config(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/set_project_stacking_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_project_stacking_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetProjectStackingConfigArgs>,
) -> Result<Json<StackingProjectConfig>, (StatusCode, String)> {
    api::set_project_stacking_config(
        &state.ctx,
        &args.project_id,
        args.config,
        args.excluded_frame_uuids,
        args.reference_frame_uuid,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/get_project_stacking_work_usage
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_project_stacking_work_usage(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectStackingArgs>,
) -> Result<Json<WorkUsage>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::get_project_stacking_work_usage(&ctx, &args.project_id)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Work-usage task panicked: {}", e)))?
    .map_err(api_err)?;
    Ok(Json(result))
}

/// POST /api/cleanup_project_stacking_work
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cleanup_project_stacking_work(
    State(state): State<WebAppState>,
    Json(args): Json<CleanupProjectStackingWorkArgs>,
) -> Result<Json<u64>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::cleanup_project_stacking_work(&ctx, &args.project_id, args.what)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Cleanup task panicked: {}", e)))?
    .map_err(api_err)?;
    Ok(Json(result))
}
```
Format these two files with `rustfmt` (they are rustfmt-clean upstream).

`crates/athenaeum-web/src/routes/mod.rs`, after the `cleanup_stacking_work` route:
```rust
        .route("/api/get_project_stacking_plan", post(stacking::get_project_stacking_plan))
        .route("/api/start_project_stacking", post(stacking::start_project_stacking))
        .route("/api/get_project_stacking_runs", post(stacking::get_project_stacking_runs))
        .route("/api/get_project_stacking_config", post(stacking::get_project_stacking_config))
        .route("/api/set_project_stacking_config", post(stacking::set_project_stacking_config))
        .route("/api/get_project_stacking_work_usage", post(stacking::get_project_stacking_work_usage))
        .route("/api/cleanup_project_stacking_work", post(stacking::cleanup_project_stacking_work))
```

- [ ] **Step 4: Run the tests and the count**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'` then `CARGO_BUILD_JOBS=4 cargo check -p athenaeum-tauri` then `grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l`.
Expected: web all pass (the three new tests included), tauri check clean, count `306`.

- [ ] **Step 5: Run the gates.** `git diff --exit-code src/types`.

- [ ] **Step 6: Commit**
```bash
git add crates/athenaeum-tauri/src/commands/stacking.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/routes/stacking.rs crates/athenaeum-web/src/routes/mod.rs
git commit -m "feat(stacking): the seven project stacking commands on desktop and web (306)

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 9: The fake-hub end-to-end (spec §15.2; D7, D13, D19)

**Files:**
- Create `crates/athenaeum-core/src/api/stacking_project_e2e.rs` (test-only).
- Modify `crates/athenaeum-core/src/api/mod.rs`: register it beside `pub mod stacking;` (`~113`).

The lighter harness of `api/collab_grid.rs`'s tests (`FakeHub` + `collab_exchange::test_support::{test_ctx, wire_hub}` + `refresh_projects` + `sync_manifest`), no iroh, no live runtime. Frames are rendered by Task 2b1's `render_sky_frame` and landed with `set_landed`, as the exchange leaves them. The solve goes through `start_stacking_with_solver` with a `FakeSolver` answering the one frame without a manifest WCS with its true record (spec §15.2: a synthetic field cannot be solved against a real catalog).

The project (canonical names differ from every raw name, so the masters' `FILTER` proves the canonical stamp):

| Publisher | Frames | Raw `FILTER` | Canonical | Notes |
| ---- | ---- | ---- | ---- | ---- |
| Ann (`acc-a`) | `a0`–`a3` around the target | `Luminance` | `L` | `a3` excluded by the coordinator |
| Ann | `r0`–`r2` around the target | `Red` | `R` | the second group |
| Bob (`acc-b`) | `b0`, `b1` around the target; `b2` exactly at it with no manifest WCS | `Lum` | `L` | `b2` is solved by the run |
| Bob | `e0`, `e1` 0.85 frame widths east | `Lum` | `L` | under 25 % overlap: global normalization |

The `L` group stacks 8 frames (6 central, 2 east); the `R` group 3.

- [ ] **Step 1: Write the test**

`crates/athenaeum-core/src/api/mod.rs`, after `pub mod stacking;`:
```rust
// Plan 6b-3 Task 9: a collaboration project stacked end to end through the
// fake hub (spec §15.2). In-crate because the fake hub is `#[cfg(test)]`.
#[cfg(all(test, feature = "render", feature = "solver"))]
mod stacking_project_e2e;
```

`crates/athenaeum-core/src/api/stacking_project_e2e.rs`:
```rust
//! Spec §15.2, plan 6b-3 Task 9: a collaboration project stacked end to end
//! on the processor's machine, through the fake hub — two publishers, two
//! canonical filters, a coordinator exclusion, a frame the run plate-solves,
//! two members under 25 % overlap, the grid confirmed through the real
//! command, and a second run that solves nothing.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::api::collab_exchange::test_support::{test_ctx, wire_hub};
use crate::api::PathPolicy;
use crate::collab::fake_hub::FakeHub;
use crate::db::stacking::{StackSubject, StackingRunRow};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::stacking::config::StackingConfig;
use crate::stacking::solve::{FakeSolver, SolveOutcome};
use crate::stacking::test_fixtures::{
    render_sky_frame, sky_frame, sky_frame_record, sky_stars, EAST_PAIR_OFFSET_WIDTHS,
    EAST_PAIR_SPACING_ARCSEC, EAST_PAIR_STAR_SEED,
};

const TARGET: (f64, f64) = (83.8, -5.4);

struct Rec(Mutex<Vec<(String, serde_json::Value)>>);
impl ProgressEmitter for Rec {
    fn emit_json(&self, name: &str, payload: serde_json::Value) {
        self.0.lock().unwrap().push((name.to_string(), payload));
    }
}

fn db_conn(ctx: &ServiceContext) -> r2d2::PooledConnection<crate::db::SqliteConnectionManager> {
    crate::api::db(ctx).unwrap().conn()
}

fn count(ctx: &ServiceContext, table: &str) -> i64 {
    db_conn(ctx)
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// The catalog tables a project run must never write.
fn catalog_rows(ctx: &ServiceContext) -> (i64, i64, i64) {
    (count(ctx, "frames"), count(ctx, "plate_solves"), count(ctx, "registration_results"))
}

/// Waits for the run's terminal row and for its handle to leave.
async fn finished(ctx: &ServiceContext, run_id: i64) -> StackingRunRow {
    // Two runs on the test context's one image-pool thread under full-suite
    // load (review M11): generous, never the expected duration.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let row = crate::db::stacking::get_run(&db_conn(ctx), run_id).unwrap().unwrap();
        let live = ctx.active_stacks.lock().unwrap().contains_key(&run_id);
        if matches!(row.status.as_str(), "done" | "failed" | "cancelled") && !live {
            return row;
        }
        assert!(Instant::now() < deadline, "run {run_id} still {}", row.status);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn wcs_of(header: &crate::fits_parser::FitsHeader) -> solvemyastro::wcs::WcsSolution {
    let f = |k: &str| header.get_f64(k).unwrap_or_else(|| panic!("{k} missing"));
    solvemyastro::wcs::WcsSolution {
        crpix: (f("CRPIX1") - 1.0, f("CRPIX2") - 1.0),
        crval: (f("CRVAL1"), f("CRVAL2")),
        cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
        sip_forward: None,
        sip_reverse: None,
    }
}

/// One frame: rendered under `collab/<publisher>/`, published on the hub,
/// and (after the manifest sync) landed.
struct Planned {
    uuid: &'static str,
    account: &'static str,
    publisher: &'static str,
    raw: &'static str,
    canonical: &'static str,
    centre: (f64, f64),
    manifest_wcs: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_stacks_end_to_end_through_the_fake_hub() {
    let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
    let hub = FakeHub::start().await;
    let (tmp, ctx) = test_ctx();
    wire_hub(&ctx, &hub.uri(), "tok");
    let b64 = base64::engine::general_purpose::STANDARD;
    hub.add_account("tok", "acc-me", "Me", &b64.encode([7u8; 32]), None);
    hub.add_account("tok-a", "acc-a", "Ann", &b64.encode([8u8; 32]), None);
    hub.add_account("tok-b", "acc-b", "Bob", &b64.encode([9u8; 32]), None);
    hub.add_project(
        "p1",
        "m42",
        &[
            ("acc-me", "send_receive", true),
            ("acc-a", "send_receive", false),
            ("acc-b", "send_receive", false),
        ],
        false,
    );
    hub.set_target("p1", "M42", TARGET.0, TARGET.1, 1.0);

    let east = EAST_PAIR_OFFSET_WIDTHS * 384.0 * 1.5 / 3600.0 / TARGET.1.to_radians().cos();
    // The run tests' dithers (±3.6 px at most): the LN field's 0.97 rule
    // must keep every central L member (D8, review I6).
    let near = |i: f64| (TARGET.0 + (i - 1.5) * 0.001, TARGET.1 + (i % 2.0) * 0.0005);
    let frame = |uuid, account, publisher, raw, canonical, centre, manifest_wcs| Planned {
        uuid,
        account,
        publisher,
        raw,
        canonical,
        centre,
        manifest_wcs,
    };
    let planned = vec![
        frame("a0", "acc-a", "Ann", "Luminance", "L", near(0.0), true),
        frame("a1", "acc-a", "Ann", "Luminance", "L", near(1.0), true),
        frame("a2", "acc-a", "Ann", "Luminance", "L", near(2.0), true),
        frame("a3", "acc-a", "Ann", "Luminance", "L", near(3.0), true),
        frame("r0", "acc-a", "Ann", "Red", "R", near(0.5), true),
        frame("r1", "acc-a", "Ann", "Red", "R", near(1.5), true),
        frame("r2", "acc-a", "Ann", "Red", "R", near(2.5), true),
        frame("b0", "acc-b", "Bob", "Lum", "L", near(0.25), true),
        frame("b1", "acc-b", "Bob", "Lum", "L", near(2.75), true),
        frame("b2", "acc-b", "Bob", "Lum", "L", TARGET, false),
        frame("e0", "acc-b", "Bob", "Lum", "L", (TARGET.0 + east, TARGET.1), true),
        frame("e1", "acc-b", "Bob", "Lum", "L", (TARGET.0 + east + 0.002, TARGET.1 + 0.001), true),
    ];
    // Task 5b's star field, constant for constant (review I5): the east pair
    // registers over its ≈ 15 % strip there first, a hundred times cheaper.
    let stars = sky_stars((TARGET.0 + east / 2.0, TARGET.1), 0.2, EAST_PAIR_SPACING_ARCSEC, EAST_PAIR_STAR_SEED);
    let collab = tmp.path().join("collab");
    let mut b2_record = None;
    let mut rendered = Vec::new();
    for (i, p) in planned.iter().enumerate() {
        let mut f = sky_frame(p.uuid, p.centre);
        f.publisher = p.publisher;
        f.filter = p.raw;
        f.manifest_wcs = p.manifest_wcs;
        f.noise_seed = 500 + i as u64;
        let r = render_sky_frame(&collab, "p1", &f, &stars);
        if p.uuid == "b2" {
            b2_record = Some((r.file_name.clone(), sky_frame_record(&f)));
        }
        let (meta, file_name, blake3, size) = (r.meta.clone(), r.file_name.clone(), r.blake3.clone(), r.byte_size);
        let (raw, canonical) = (p.raw.to_string(), p.canonical.to_string());
        hub.seed_frames_with("p1", p.account, &[p.uuid], "published", move |w| {
            w.meta = meta.clone();
            w.file_name = file_name.clone();
            w.blake3 = blake3.clone();
            w.byte_size = size;
            w.xxh3 = "0123456789abcdef".to_string();
            w.filter_raw = raw.clone();
            w.filter_canonical = canonical.clone();
            w.channel = "mono".to_string();
            w.exptime_sec = 300.0;
            w.date_obs = Some("2026-01-10T21:00:00".to_string());
        });
        rendered.push((p.uuid, r.path));
    }
    crate::api::collab::refresh_projects(&ctx).await.unwrap();
    crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None).await.unwrap();
    {
        let conn = db_conn(&ctx);
        for (uuid, path) in &rendered {
            let stamp = crate::api::collab_exchange::size_mtime_from(&std::fs::metadata(path).unwrap());
            crate::db::collab_frames::set_landed(&conn, "p1", uuid, &path.to_string_lossy(), &stamp).unwrap();
        }
        for (key, dir) in [
            (crate::settings::keys::STACKING_WORKING_DIR, tmp.path().join("work")),
            (crate::settings::keys::STACKING_OUTPUT_DIR, tmp.path().join("out")),
        ] {
            crate::db::set_setting(&conn, key, dir.to_str().unwrap()).unwrap();
        }
    }
    let ctx = Arc::new(ctx);
    let before = catalog_rows(&ctx);

    // The first plan: the grid is not confirmed yet, the proposal is there,
    // and the frame without a manifest WCS will be solved.
    let first = crate::api::stacking::get_project_stacking_plan(&ctx, &PathPolicy::AllowAll, "p1", None).unwrap();
    assert!(first.blockers.iter().any(|b| b.code == "grid"), "{:?}", first.blockers);
    assert!(
        first.warnings.iter().any(|w| w.contains("will be plate-solved locally")),
        "{:?}",
        first.warnings
    );
    let proposal = first.grid.as_ref().and_then(|g| g.proposal).expect("a proposal");
    let grid = crate::api::collab_grid::set_collab_project_grid(&ctx, "p1", proposal, None).await.unwrap();
    crate::api::collab::exclude_collab_frame(&ctx, "p1", "a3", "trailed".into()).await.unwrap();

    let mut cfg = crate::api::stacking::get_project_stacking_config(&ctx, "p1").unwrap().config;
    cfg.normalization.local.enabled = true;
    cfg.reference.two_pass = false;
    crate::api::stacking::set_project_stacking_config(&ctx, "p1", cfg, Vec::new(), None).unwrap();

    let ready = crate::api::stacking::get_project_stacking_plan(&ctx, &PathPolicy::AllowAll, "p1", None).unwrap();
    assert!(ready.blockers.is_empty(), "{:?}", ready.blockers);
    assert!(ready.project_frames.iter().all(|f| f.frame_uuid != "a3"), "the excluded frame leaves the plan");
    assert_eq!(ready.grid.as_ref().and_then(|g| g.current), Some(grid));

    // The run.
    let (b2_name, b2_solve) = b2_record.unwrap();
    let solver = Arc::new(FakeSolver::solving_all().with(&b2_name, SolveOutcome::Solved(b2_solve)));
    let events = Arc::new(Rec(Mutex::new(Vec::new())));
    let started = crate::stacking::run::start_stacking_with_solver(
        ctx.clone(),
        events.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
        solver.clone(),
    )
    .unwrap();
    let row = finished(&ctx, started.run_id).await;
    assert_eq!(row.status, "done", "{row:?}");
    assert_eq!(solver.calls(), 1, "the one frame without a WCS is solved");

    let summary: crate::stacking::provenance::RunSummary =
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(summary.grid, Some(grid));
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.contains("kept 2 frame(s) on global normalization")),
        "{:?}",
        summary.warnings
    );
    let completes: Vec<serde_json::Value> = events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _)| n == crate::stacking::run::STACKING_COMPLETE_EVENT)
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(completes.len(), 1);
    assert_eq!(completes[0]["projectId"], "p1");
    assert!(completes[0]["setId"].is_null());

    // The masters: grid size, the canonical filter, coverage that counts the
    // footprints (6 central L members cover the target, all 8 the strip the
    // east pair shares with them; R: 3 everywhere they overlap).
    let lights = crate::db::stacking::list_master_lights(&db_conn(&ctx), started.run_id).unwrap();
    let masters: Vec<_> = lights.iter().filter(|m| m.kind == "master").collect();
    assert_eq!(masters.len(), 2, "{lights:?}");
    let mut filters = Vec::new();
    for m in &masters {
        assert_eq!((m.width, m.height), (grid.width_px, grid.height_px));
        assert!(Path::new(&m.path).starts_with(tmp.path().join("out").join("p1")), "{}", m.path);
        let header = crate::fits_parser::FitsHeader::from_path(Path::new(&m.path)).unwrap();
        let filter = header.get_str("FILTER").unwrap();
        let coverage = lights
            .iter()
            .find(|c| c.kind == "coverage" && c.group_key == m.group_key)
            .expect("a coverage map per group");
        let reader = crate::integration::plane_reader::PlaneReader::open(Path::new(&coverage.path)).unwrap();
        let plane = reader.read_plane(0).unwrap();
        let (x, y) = wcs_of(&header).sky_to_pixel(TARGET.0, TARGET.1);
        let at_target = plane[(y.round() as usize) * reader.width() + x.round() as usize];
        let max = plane.iter().cloned().fold(0f32, f32::max);
        match filter.as_str() {
            "L" => assert_eq!((at_target, max), (6.0, 8.0), "L coverage"),
            "R" => assert_eq!((at_target, max), (3.0, 3.0), "R coverage"),
            other => panic!("a master with FILTER {other:?}: the raw name leaked"),
        }
        filters.push(filter);
    }
    filters.sort();
    assert_eq!(filters, vec!["L".to_string(), "R".to_string()]);

    // Run history names uuids and never the excluded frame; the catalog is
    // untouched; nothing written carries ATH_PRJ.
    let detail = crate::api::stacking::get_stacking_run(&ctx, started.run_id).unwrap();
    assert_eq!(detail.frames.len(), 11, "12 published minus the excluded one");
    assert!(detail.frames.iter().all(|f| f.frame_uuid.as_deref() != Some("a3")));
    assert!(detail.frames.iter().all(|f| f.frame_uuid.is_some()));
    assert_eq!(catalog_rows(&ctx), before, "frames, plate_solves, registration_results untouched");
    for root in [tmp.path().join("work"), tmp.path().join("out")] {
        for entry in walkdir::WalkDir::new(&root).into_iter().filter_map(Result::ok) {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("fits") {
                let header = crate::fits_parser::FitsHeader::from_path(entry.path()).unwrap();
                assert!(header.get_str("ATH_PRJ").is_none(), "{} carries ATH_PRJ", entry.path().display());
            }
        }
    }

    // A second run solves nothing and finishes.
    let again = crate::stacking::run::start_stacking_with_solver(
        ctx.clone(),
        Arc::new(crate::events::NullEmitter),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::Project("p1".into()),
        None,
        None,
        solver.clone(),
    )
    .unwrap();
    assert_eq!(finished(&ctx, again.run_id).await.status, "done");
    assert_eq!(solver.calls(), 1, "the cached solve answers the second run");

    // Cleanup keeps the landed replicas byte for byte.
    let landed_before: Vec<Vec<u8>> = rendered.iter().map(|(_, p)| std::fs::read(p).unwrap()).collect();
    crate::api::stacking::cleanup_project_stacking_work(&ctx, "p1", crate::stacking::paths::CleanupWhat::All).unwrap();
    for ((uuid, path), bytes) in rendered.iter().zip(&landed_before) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{uuid} was touched");
    }
}
```
Verified at `20c7c1ef`: `crate::api::db` returns `&Database`, whose `conn()` is `r2d2::PooledConnection<crate::db::SqliteConnectionManager>` (`api/mod.rs:224`, `db/mod.rs:177`); `ProgressEmitter` and `NullEmitter` live in `crate::events` (`events.rs:19`); `StackingRunRow` in `db/stacking.rs:98`; `exclude_collab_frame(ctx, project_id, uuid, reason)` in `api/collab.rs:7497`; `set_collab_project_grid(ctx, project_id, grid, emitter)` in `api/collab_grid.rs:137`; `FakeSolver::solving_all().with(name, outcome)` and `calls()` in `stacking/solve.rs`. A float FITS is used in place by Prepare (`prepare.rs:152-166`), so the solve request's file name is the rendered `c_b2.fits`.

- [ ] **Step 2: Run it**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- stacking_project_e2e --test-threads=1 2>&1 | tail -30`
Expected: PASS within a few minutes (the test context's image pool has one thread).
If it fails, find the cause; never loosen an assertion:
- the east pair excluded at Register (`detail.frames` reasons): Task 5b's `a_member_under_a_quarter_overlap_keeps_global_normalization` registers the same strip with the same field and should have failed first; lower `test_fixtures::EAST_PAIR_SPACING_ARCSEC` to 32″ (both tests follow) and re-run both;
- `L coverage` off by one at the target: print `x, y` and the 3 × 3 neighbourhood — a dither landing a frame edge on the target pixel means the fixture's dither, not the count, is wrong; move `near()`'s step to 0.0015;
- `ready.blockers` not empty: print them; a `folders` blocker means the working folder overlaps the collab folder — keep `work/` and `collab/` siblings.

- [ ] **Step 3: Run the gates.** `git diff --exit-code src/types`.

- [ ] **Step 4: Commit**
```bash
git add crates/athenaeum-core/src/api/stacking_project_e2e.rs crates/athenaeum-core/src/api/mod.rs
git commit -m "test(stacking): a collaboration project stacked end to end through the fake hub

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 10: The acceptance drivers (D21, D23; spec §15.3)

**Files** (all under `docs/superpowers/research/scripts/acceptance/`; no Rust):
- Create `tier1/project-run.sh` — `tier1-run.sh`'s project twin.
- Modify `tier1/tier1-compare.py` — a `--pairs` mode (run A); `report_masters`' per-plane block moves into `compare_planes` (same output).
- Create `run-a-grid.py` — the grid of a reference frame's manifest WCS (run A) or any given grid (run B), stored in a catalog COPY only.
- Create `grid-report.py` — run B's per-group report.
- Modify `README.md` — steps 7–9.

Reuse, never rebuild: every script drives the existing `server.sh`, `api.sh`, `measure_probe` and `imgcmp.py` reader. None of them runs during this task except their own self-checks in Step 2; the runs are the last section's.

- [ ] **Step 1: Write the drivers**

`tier1/project-run.sh` (mode 755):
```zsh
#!/bin/zsh
# One project stacking run against an acceptance catalog copy (plan 6b-3 D21):
# tier1-run.sh's project twin.
#
#   project-run.sh <repo> <work-dir> <port> <project-id>
#
# Starts the release server on <work-dir>, subscribes to SSE, posts
# `start_project_stacking {projectId, rerunFrom: "calibrate"}`, waits for the
# `stacking-complete` event of THAT run id, records the wall time in
# <work-dir>/run-project.txt (beside tier1-run.sh's run.txt, never over it)
# and stops the server. Refuses to start while Time Machine is backing up or
# another cargo/rustc is alive, as checkpoint.sh does: a refusal is answered
# by stopping the backup or waiting for the build, never by editing this file.
if [ -z "${ZSH_VERSION:-}" ]; then
  exec zsh "$0" "$@"
fi
set -uo pipefail
REPO="${1:?repo}"; W="${2:?work dir}"; PORT="${3:?port}"; PID="${4:?project id}"
case "$PID" in
  ''|*[!A-Za-z0-9-]*) echo "refusing project id '$PID': letters, digits and - only" >&2; exit 1 ;;
esac
RUNNING="$(tmutil status 2>/dev/null | grep -o 'Running = [0-9]' | grep -o '[0-9]' || echo '?')"
if [ "$RUNNING" != "0" ]; then
  echo "refusing to start: tmutil status reports Running = $RUNNING (Time Machine is backing up)" >&2
  exit 1
fi
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
  echo "refusing to start: a cargo/rustc process is alive on this machine" >&2
  exit 1
fi
SRV="$REPO/docs/superpowers/research/scripts/acceptance/server.sh"
export ATHENAEUM_LOG="info,athenaeum_core::stacking=debug,athenaeum_core::integration=debug"
zsh "$SRV" "$W" "$PORT" > "$W/server-project.out" 2>&1 &
BODY="{\"projectId\":\"$PID\"}"
for i in $(seq 1 240); do
  if curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/get_project_stacking_plan" -H 'content-type: application/json' -d "$BODY" 2>/dev/null | grep -q '^200$'; then break; fi
  sleep 5
done
curl -sN "http://127.0.0.1:$PORT/api/events" > "$W/events-project.log" 2>/dev/null &
SSE_PID=$!
sleep 2
T0=$(date +%s); echo "start $(date -u +%FT%TZ) epoch=$T0 project=$PID" > "$W/run-project.txt"
RESP=$(curl -sS -X POST "http://127.0.0.1:$PORT/api/start_project_stacking" -H 'content-type: application/json' -d "{\"projectId\":\"$PID\",\"rerunFrom\":\"calibrate\"}")
echo "$RESP" >> "$W/run-project.txt"; echo >> "$W/run-project.txt"
if ! printf '%s' "$RESP" | grep -q '"runId"'; then
  echo "start_project_stacking refused for $PID: $RESP" >&2
  kill $SSE_PID 2>/dev/null
  pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web" 2>/dev/null
  exit 1
fi
RUN_ID=$(printf '%s' "$RESP" | sed -E 's/.*"runId":([0-9]+).*/\1/')
FINISHED=0
for i in $(seq 1 480); do
  if grep -A1 'stacking-complete' "$W/events-project.log" 2>/dev/null | grep -q "\"runId\":$RUN_ID[,}]"; then FINISHED=1; break; fi
  sleep 30
done
T1=$(date +%s); echo "end $(date -u +%FT%TZ) epoch=$T1 wall_s=$((T1-T0)) run_id=$RUN_ID" >> "$W/run-project.txt"
if [ "$FINISHED" != "1" ]; then
  # Review M10: a 4-hour wait that ends without the event is a timeout,
  # never a finished run.
  echo "TIMED OUT after 4 h waiting for stacking-complete of run $RUN_ID" | tee -a "$W/run-project.txt" >&2
fi
grep -A1 'stacking-complete' "$W/events-project.log" | grep "\"runId\":$RUN_ID[,}]" >> "$W/run-project.txt"
kill $SSE_PID 2>/dev/null
pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web" 2>/dev/null
sleep 3
cat "$W/run-project.txt"
```

`tier1/tier1-compare.py`:
- extract the per-plane block of `report_masters` (the `for i, (hc, bc) in enumerate(zip(h_ch, b_ch)):` loop with its two prints) into
  ```python
  def compare_planes(h_ch, b_ch, gate):
      """Rows 1-3 for one pair of masters, plane by plane (median/MAD, noise, FWHM)."""
      for i, (hc, bc) in enumerate(zip(h_ch, b_ch)):
          ...  # the moved block, unchanged
  ```
  and call it from `report_masters` where the loop was — the `--numeric` output is byte-identical;
- add the pairs mode, with a dimensions reader (FITS from its header; anything else through `imgcmp.read`, which `weight_map_coverage` already imports):
  ```python
  def plane_dims(path):
      """-> (width, height) of a master: NAXIS1/NAXIS2 of a FITS header, else the array shape."""
      if path.lower().endswith((".fits", ".fit")):
          cards, _, _ = fits_split(path)
          keys = {c[:8].strip(): c[10:].split("/")[0].strip() for c in cards if c[8:10] == "= "}
          return int(keys["NAXIS1"]), int(keys["NAXIS2"])
      from imgcmp import read
      shape = read(path).shape
      return shape[-1], shape[-2]


  def run_pairs(pairs):
      """Plan 6b-3 run A (spec §15.3): the first four rows of the Tier C §8 table on explicit
      (head, base) master pairs — a project's grid master against the personal master of the
      same frames on the same reference. Whole-plane statistics: run A's grid is the
      reference's own footprint, so both planes hold the same sky (the grid master is the
      mirror image in x, which none of the four rows reads). Exits 0 on PASS, 1 on FAIL."""
      gate = {"median_mad": [], "noise": [], "fwhm": []}
      print("=== run A: project masters (head) vs personal masters (base) ===")
      for head, base in pairs:
          print(f"  head {head}")
          print(f"  base {base}")
          # Review I4: the rows compare two planes of the same pixel grid only.
          h_dims, b_dims = plane_dims(head), plane_dims(base)
          if h_dims != b_dims:
              print(f"    FAIL pixel grids differ: head {h_dims} base {b_dims} — not comparable")
              for k in gate:
                  gate[k].append(False)
              continue
          h_ch, b_ch = measure_master(head), measure_master(base)
          if len(h_ch) != len(b_ch):
              print(f"    FAIL channel count differs: head={len(h_ch)} base={len(b_ch)}")
              for k in gate:
                  gate[k].append(False)
              continue
          compare_planes(h_ch, b_ch, gate)
      print()
      print("=== summary (Tier C §8, rows run A names) ===")
      labels = {
          "Master pixel median / MAD vs tierA-baseline": gate["median_mad"],
          "Master noise (MRS sigma) vs baseline": gate["noise"],
          "FWHM (mono, OSC per plane) vs baseline": gate["fwhm"],
      }
      ok_all = True
      for label, tol in TOLERANCE_TABLE:
          if label not in labels:
              continue
          checks = labels[label]
          ok = bool(checks) and all(checks)
          ok_all = ok_all and ok
          print(f"  {label}: [{tol}]  {verdict(ok)} ({sum(checks)}/{len(checks)} sub-checks)")
      print()
      print("PAIRS:", "PASS" if ok_all else "FAIL")
      return 0 if ok_all else 1
  ```
- `main()`: before the `--numeric` handling,
  ```python
      if "--pairs" in args:
          rest = [a for a in args if a != "--pairs"]
          if not rest or any("=" not in a for a in rest):
              print("usage: tier1-compare.py --pairs <head_master>=<base_master> [...]", file=sys.stderr)
              sys.exit(2)
          sys.exit(run_pairs([tuple(a.split("=", 1)) for a in rest]))
  ```
  and add the line to the module docstring's usage block.

`run-a-grid.py` (mode 755):
```python
#!/usr/bin/env python3
"""The canonical grid an acceptance run stacks a project onto, written to an
acceptance catalog COPY only (plan 6b-3 D21, spec §15.3).

    run-a-grid.py <copy.db> <project-id> --reference-uuid <uuid> [--apply]
    run-a-grid.py <copy.db> <project-id> --grid-json '<json>' [--apply]

--reference-uuid (run A): the grid of that frame's manifest WCS
(`project_frames_local.manifest_json` -> `meta.wcs`) — scale sqrt(|det CD|)·3600,
the frame's size (`meta.naxis1/2`), the sky position of its centre pixel
((W+1)/2, (H+1)/2) through the WCS with SIP, and the rotation the grid's own
TAN reads back (`field_rotation_deg`: atan2(-CD1_2, CD2_2) in [0, 360)). The
grid's CD is sky-standard (det < 0), so a det > 0 reference comes out mirrored
in x with north kept (6b-2 open item "the grid's orientation").
--grid-json (run B): any grid, e.g. the `proposal` of `get_collab_grid_proposal`.

Prints the normalized grid. --apply writes it to
`collab_projects.canonical_grid_json`. Refuses the live catalogs: a real
project's grid is its coordinator's, set on the hub, never here.
"""
import json
import math
import os
import sqlite3
import sys

LIVE = [
    os.path.realpath(os.path.expanduser(p))
    for p in (
        "~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db",
        "~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db",
    )
]


def sip(table, u, v):
    return sum(c * (u ** p) * (v ** q) for p, row in enumerate(table) for q, c in enumerate(row) if c)


def pixel_to_sky(wcs, x1, y1):
    """1-based pixel -> (ra, dec) degrees through TAN with the forward SIP."""
    u, v = x1 - wcs["crpix1"], y1 - wcs["crpix2"]
    s = wcs.get("sip")
    if s:
        du, dv = sip(s["a"], u, v), sip(s["b"], u, v)
        u, v = u + du, v + dv
    cd = wcs["cd"]  # [CD1_1, CD1_2, CD2_1, CD2_2]
    xi = math.radians(cd[0] * u + cd[1] * v)
    eta = math.radians(cd[2] * u + cd[3] * v)
    ra0, dec0 = math.radians(wcs["crval1"]), math.radians(wcs["crval2"])
    den = math.cos(dec0) - eta * math.sin(dec0)
    ra = ra0 + math.atan2(xi, den)
    dec = math.atan2(math.sin(dec0) + eta * math.cos(dec0), math.hypot(xi, den))
    return math.degrees(ra) % 360.0, math.degrees(dec)


def normalized(g):
    rot = math.fmod(g["rotationDeg"], 360.0)
    if rot <= -180.0:
        rot += 360.0
    elif rot > 180.0:
        rot -= 360.0
    # `+ 0.0` turns -0.0 into 0.0 on every float, as `CanonicalGrid::normalized`
    # does (review M9).
    return {
        "crval1": (g["crval1"] % 360.0) + 0.0,
        "crval2": g["crval2"] + 0.0,
        "heightPx": int(g["heightPx"]),
        "rotationDeg": rot + 0.0,
        "scaleArcsec": g["scaleArcsec"] + 0.0,
        "widthPx": int(g["widthPx"]),
    }


def grid_of_reference(db, pid, uuid):
    row = db.execute(
        "SELECT manifest_json FROM project_frames_local WHERE project_id = ? AND frame_uuid = ?",
        (pid, uuid),
    ).fetchone()
    if not row:
        sys.exit(f"no frame {uuid} in project {pid}")
    meta = json.loads(row[0]).get("meta") or {}
    wcs = meta.get("wcs")
    if not wcs:
        sys.exit(f"frame {uuid} has no manifest WCS: pick a reference that has one")
    w, h = int(meta["naxis1"]), int(meta["naxis2"])
    cd = wcs["cd"]
    ra, dec = pixel_to_sky(wcs, (w + 1) / 2.0, (h + 1) / 2.0)
    rot = math.degrees(math.atan2(-cd[1], cd[3])) % 360.0
    scale = math.sqrt(abs(cd[0] * cd[3] - cd[1] * cd[2])) * 3600.0
    # Review I4: the grid's pixels are square and unskewed; the reference's
    # are only to the extent its CD is. Print how far, so run A's report says
    # how much of a row difference geometry could explain.
    col1, col2 = math.hypot(cd[0], cd[2]), math.hypot(cd[1], cd[3])
    aspect = col1 / col2 if col2 else float("nan")
    skew = abs(cd[0] * cd[1] + cd[2] * cd[3]) / (col1 * col2) if col1 and col2 else float("nan")
    print(
        f"reference {uuid}: scale {scale:.6f}\"/px, rotation {rot:.4f} deg, "
        f"pixel aspect {aspect:.6f}, skew {skew:.2e}, det CD {'>' if cd[0] * cd[3] - cd[1] * cd[2] > 0 else '<'} 0",
        file=sys.stderr,
    )
    return {"scaleArcsec": scale, "widthPx": w, "heightPx": h, "crval1": ra, "crval2": dec, "rotationDeg": rot}


def main():
    args = sys.argv[1:]
    if len(args) < 4:
        sys.exit(__doc__)
    db_path, pid, mode, value = args[:4]
    apply = "--apply" in args[4:]
    if os.path.realpath(db_path) in LIVE:
        sys.exit("refusing: that is a live catalog — work on an acceptance copy")
    db = sqlite3.connect(db_path)
    if mode == "--reference-uuid":
        grid = grid_of_reference(db, pid, value)
    elif mode == "--grid-json":
        grid = json.loads(value)
    else:
        sys.exit(__doc__)
    grid = normalized(grid)
    ok = (
        0 < grid["scaleArcsec"] <= 100
        and 64 <= grid["widthPx"] <= 65536
        and 64 <= grid["heightPx"] <= 65536
        and -90 <= grid["crval2"] <= 90
    )
    if not ok:
        sys.exit(f"grid outside the hub's bounds: {grid}")
    text = json.dumps(grid, sort_keys=True, separators=(",", ":"))
    print(text)
    if apply:
        n = db.execute(
            "UPDATE collab_projects SET canonical_grid_json = ? WHERE project_id = ?", (text, pid)
        ).rowcount
        db.commit()
        if n != 1:
            sys.exit(f"project {pid} not in {db_path}")
        print(f"stored on project {pid} in {db_path}")


if __name__ == "__main__":
    main()
```

`grid-report.py` (mode 755):
```python
#!/usr/bin/env python3
"""Run B's report (spec §15.3, plan 6b-3 D21, D23): for each group of the
latest finished run of a project in an acceptance copy — the grid; the
coverage map (min, max, histogram, the coverage >= K rectangle); and, on the
master cropped to that rectangle, FWHM in px and arcsec, eccentricity, the
noise per pixel and per arcsec² (D23: sigma_px * scale, the noise of a
one-arcsec² mean of WHITE noise — a stack's pixels are correlated by
resampling, more so by drizzle, so this is a white-noise equivalent that
understates the true per-area noise by a scale-dependent factor; the
review's caveat on D23). No tolerance: the geometry differs from every
baseline.

    grid-report.py <work-dir> <project-id> [--k K] [--overlap]

K defaults to half the group's included frames, rounded up (D8's median
footprint). The crops are written under <work-dir>/grid-report/ and measured
with measure_probe (built into .superpowers/target-acc, as tier1-compare.py
expects). --overlap adds each member's overlap with its group's LN reference
on plane 0 (finite in both / finite in the member), the number D7's 25 %
rule reads; it reads every registered frame once.
"""
import json
import math
import os
import sqlite3
import subprocess
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from imgcmp import read  # noqa: E402

REPO_ROOT = HERE.parents[4]
MEASURE_PROBE = REPO_ROOT / ".superpowers" / "target-acc" / "release" / "examples" / "measure_probe"


def write_fits(path, data):
    """A minimal BITPIX -32 primary HDU: (h, w) or (c, h, w)."""
    dims = list(data.shape[::-1])
    cards = ["SIMPLE  =                    T", "BITPIX  =                  -32", f"NAXIS   = {len(dims):>20}"]
    cards += [f"NAXIS{i + 1:<3}= {d:>20}" for i, d in enumerate(dims)]
    cards.append("END")
    header = "".join(c.ljust(80) for c in cards).encode("ascii")
    header += b" " * (-len(header) % 2880)
    body = data.astype(">f4").tobytes()
    body += b"\0" * (-len(body) % 2880)
    Path(path).write_bytes(header + body)


def plane0(path):
    a = read(path)
    return a[0] if a.ndim == 3 else a


def main():
    args = sys.argv[1:]
    if len(args) < 2:
        sys.exit(__doc__)
    work, pid = Path(args[0]), args[1]
    k_arg = int(args[args.index("--k") + 1]) if "--k" in args else None
    want_overlap = "--overlap" in args
    if not MEASURE_PROBE.exists():
        sys.exit(f"measure_probe not found at {MEASURE_PROBE}: build it into .superpowers/target-acc first")
    db = sqlite3.connect(work / "athenaeum.db")
    row = db.execute(
        "SELECT id, summary_json FROM stacking_runs WHERE project_id = ? AND status = 'done' ORDER BY id DESC LIMIT 1",
        (pid,),
    ).fetchone()
    if not row:
        sys.exit(f"no finished run of project {pid}")
    run_id, summary = row[0], json.loads(row[1])
    grid = summary["grid"]
    scale = grid["scaleArcsec"]
    print("noise per arcsec^2 below is the white-noise equivalent sigma_px * scale (plan 6b-3 D23)")
    print(f"run {run_id}  grid {grid['widthPx']}x{grid['heightPx']} @ {scale:.4f}\"/px  "
          f"centre ({grid['crval1']:.5f}, {grid['crval2']:.5f})  rotation {grid['rotationDeg']:.3f} deg")
    out = work / "grid-report"
    out.mkdir(exist_ok=True)
    groups = db.execute(
        "SELECT group_key, included_count FROM stacking_run_groups WHERE run_id = ? ORDER BY group_key", (run_id,)
    ).fetchall()
    for key, included in groups:
        rows = dict(
            db.execute(
                "SELECT kind, path FROM master_lights WHERE run_id = ? AND group_key = ?", (run_id, key)
            ).fetchall()
        )
        if "master" not in rows or "coverage" not in rows:
            print(f"\n== {key}: no master or no coverage map (coverageMap off?) — skipped")
            continue
        cov = plane0(rows["coverage"])
        k = k_arg if k_arg is not None else max(1, math.ceil(included / 2))
        counts = np.bincount(np.nan_to_num(cov).astype(np.int64).ravel())
        ys, xs = np.nonzero(cov >= k)
        print(f"\n== {key}  ({included} frames, K = {k})")
        print(f"  coverage min {cov.min():.0f} max {cov.max():.0f}")
        print("  histogram " + ", ".join(f"{n}:{c}" for n, c in enumerate(counts) if c))
        if xs.size == 0:
            print(f"  no pixel reaches {k}: nothing measured")
            continue
        x0, x1, y0, y1 = xs.min(), xs.max(), ys.min(), ys.max()
        print(f"  coverage >= {k}: x {x0}..{x1}  y {y0}..{y1}  ({x1 - x0 + 1} x {y1 - y0 + 1} px)")
        master = read(rows["master"])
        crop = master[..., y0:y1 + 1, x0:x1 + 1]
        crop_path = out / f"run{run_id}_{key}.fits"
        write_fits(crop_path, crop)
        proc = subprocess.run([str(MEASURE_PROBE), str(crop_path)], capture_output=True, text=True, timeout=600)
        if proc.returncode != 0:
            print(f"  measure_probe failed: {proc.stderr.strip()[-400:]}")
            continue
        data = json.loads(proc.stdout)
        for i, c in enumerate(data if isinstance(data, list) else data["channels"]):
            print(
                f"  plane {i}: FWHM {c['fwhmPx']:.3f} px = {c['fwhmPx'] * scale:.3f}\"  "
                f"ecc {c['eccentricity']:.3f}  noise {c['noise']:.6g}/px = {c['noise'] * scale:.6g}/arcsec^2  "
                f"median {c['median']:.6g}  stars fitted {c['starsFitted']}"
            )
        if want_overlap:
            ref = db.execute(
                "SELECT path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? AND kind = 'ln_reference'",
                (pid, key),
            ).fetchone()
            if not ref or not ref[0]:
                print("  overlap: no LN reference for this group")
                continue
            ref_ok = np.isfinite(plane0(ref[0]))
            fractions = []
            for handle, path in db.execute(
                "SELECT project_frame, path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? AND kind = 'registered'",
                (pid, key),
            ):
                m = np.isfinite(plane0(path))
                fractions.append((float((m & ref_ok).sum()) / max(1, int(m.sum())), handle))
            fractions.sort()
            low = [f for f in fractions if f[0] < 0.25]
            print(f"  overlap with the LN reference: min {fractions[0][0]:.3f} median "
                  f"{fractions[len(fractions) // 2][0]:.3f}; {len(low)} under 25 %")
            for frac, handle in low:
                print(f"    handle {handle}: {frac:.3f}")


if __name__ == "__main__":
    main()
```

`README.md`, after step 6:
```markdown
7. `tier1/project-run.sh <repo> <work-dir> <port> <project-id>` — one
   project run (wave 6b-3): `start_project_stacking`, waits for that run's
   `stacking-complete`, writes `run-project.txt` beside `run.txt`. Same
   refusals as `checkpoint.sh`.
8. `run-a-grid.py <copy.db> <project-id> (--reference-uuid U | --grid-json J) [--apply]`
   — the grid of a reference frame's manifest WCS (acceptance run A) or any
   grid (run B), stored in a catalog COPY only; it refuses the live catalogs.
   An acceptance copy that stacks a project runs signed out: set its
   `account.hub_url` to `http://127.0.0.1:9` before the server starts (the
   feed would otherwise replace the local grid), and never call
   `account_sign_out` on a copy — the token store is shared with the real app.
9. `tier1/tier1-compare.py --pairs <head>=<base> …` — the first four Tier C
   rows on explicit master pairs (run A), refusing a pair whose pixel grids
   differ; `grid-report.py <work-dir>
   <project-id> [--k K] [--overlap]` — run B's per-group report (FWHM in
   arcsec, eccentricity, the white-noise-equivalent noise per arcsec² =
   σ_px·scale, coverage, member overlap with the LN reference).

A catalog copy is made with `sqlite3 -readonly <live db> ".backup <copy>"`,
never by reading the live catalog any other way, and `prepare-catalog.sh` is
fed that copy through `ATH_ACC_DB`. Working folders live on
`/Volumes/BigMac` (`$HOME/.athenaeum-acc/…`); `/Volumes/bigbase3` is a slow
HDD.
```

- [ ] **Step 2: Self-check the scripts (no run)**

Run:
```bash
cd docs/superpowers/research/scripts/acceptance
zsh -n tier1/project-run.sh && python3 -m py_compile run-a-grid.py grid-report.py tier1/tier1-compare.py && echo syntax-ok
python3 - <<'PY'
import importlib.util, math
spec = importlib.util.spec_from_file_location("g", "run-a-grid.py"); g = importlib.util.module_from_spec(spec); spec.loader.exec_module(g)
# A TAN WCS at (83.8, -5.4), 1.5"/px, no SIP: the centre pixel of a 384 x 288
# frame whose crpix is that centre maps back to crval exactly.
s = 1.5 / 3600
w = {"crval1": 83.8, "crval2": -5.4, "crpix1": 192.5, "crpix2": 144.5, "cd": [-s, 0.0, 0.0, s]}
ra, dec = g.pixel_to_sky(w, 192.5, 144.5)
assert abs(ra - 83.8) < 1e-12 and abs(dec + 5.4) < 1e-12, (ra, dec)
# One pixel east (x - 1 with CD1_1 < 0) moves RA by s / cos(dec).
ra2, _ = g.pixel_to_sky(w, 191.5, 144.5)
assert abs((ra2 - ra) - s / math.cos(math.radians(-5.4))) < 1e-9
assert g.normalized({"crval1": 370.0, "crval2": 1.0, "widthPx": 64, "heightPx": 64, "scaleArcsec": 1.0, "rotationDeg": 540.0})["rotationDeg"] == 180.0
print("run-a-grid ok")
PY
```
Expected: `syntax-ok` and `run-a-grid ok`.

The `--numeric` output must not move with the `compare_planes` extraction: `git diff --diff-algorithm=histogram -- docs/superpowers/research/scripts/acceptance/tier1/tier1-compare.py` shows the per-plane block moved verbatim (one indentation level shallower) and replaced by one call, nothing else in `report_masters` changed. The acceptance agent's first `--numeric` use is the live check.

- [ ] **Step 3: Commit**
```bash
git add docs/superpowers/research/scripts/acceptance/tier1/project-run.sh docs/superpowers/research/scripts/acceptance/tier1/tier1-compare.py docs/superpowers/research/scripts/acceptance/run-a-grid.py docs/superpowers/research/scripts/acceptance/grid-report.py docs/superpowers/research/scripts/acceptance/README.md
git commit -m "chore(acceptance): project run, run A grid, pairs compare and run B report drivers

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

### Task 11: Docs and the final gates

**Files:**
- Modify `docs/stacking/README.md`: replace the 6b-2 section's "Open for 6b-3" list (`~1382-1393`) with a pointer; add the 6b-3 section after it.
- Modify `docs/transfers/README.md`: a 6b-3 section after the 6b-2 one (`~293-326`).
- Modify `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`: a dictionary extension after the 6b-2 entry (`~280-290`).
- Modify `docs/superpowers/open-items.md`: a 6b-3 entry at the top of "Unverified by hand"; two 6b-2 items move or go.
- Modify `CLAUDE.md`: the command count and the Stacking summary.
- Modify `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md`: one count note in §9.1 and one in §18.

No code. Copy rules: English, short sentences, no third-party program named.

- [ ] **Step 1: The stacking reference**

In `docs/stacking/README.md`, replace the whole `- **Open for 6b-3:**` bullet (its list and the closing "`start_stacking` lifts its project guard then." line) with:
```markdown
- **Open for 6b-3:** all landed; see "The project path on the grid" below.
  The `expected_ln_channels` disagreement is gone: `plan::stage_one_planes`
  is the one plane rule for stage 1 and LN. An attested frame set of raw
  mosaics with the debayer off is keyed `osc__…` and counted as three planes
  by the plan and the run alike, while Prepare keeps its files as one-plane
  mosaics: both exclude such frames at stage 1, consistently. Mosaics stay
  mosaics only under `cfa__` keys, which frame sets never produce (review
  M7).
```
and add, right after it (before `**Key files**`):
```markdown
**The project path on the grid (wave 6b-3, 2026-10-03)** (plan
`docs/superpowers/plans/2026-10-03-collab-v3-wave6b3-project-grid-stacking-plan.md`,
rulings D1–D23; spec §5.5–§5.9, §9, §15). A project run starts, stacks every
group onto the coordinator-confirmed grid and writes grid masters.

- **One composed map per frame (D1).** `stacking::grid_map::compose_to_grid`
  fits an affine to the exact chain — star map, then the reference's WCS with
  SIP, then the grid's TAN — over a 32-px lattice, and keeps the rest as a
  `DistortionModel::Sampled` layer: a `ComposedField` evaluated exactly, read
  through lazy bilinear 8-px grids (`TpsGrids`, built and released like a
  spline's). The inverse is Newton steps on the forward chain. The map is
  never serialized; it is re-derived from (star map, reference WCS, grid)
  whenever a cached star map is reused. Pinned within 0.01 px of the chain for
  identity, homography, polynomial and spline star maps, at 0° and 4° from
  the tangent point.
- **Two caches (D2).** The star map is a payload-only `registration` artifact
  (group key `""`) outside `INTERMEDIATE_ARTIFACT_KINDS`, so only `All` drops
  it. The registered frame's hash is `grid_registered_hash(registration
  hash, reference WCS hash, grid hash)`: a grid change re-composes and
  rewrites the registered frames with no star detection. A frame set's
  registered hash is its registration hash, as before.
- **Row order (D3).** Everything written on the grid states `ROWORDER =
  'BOTTOM-UP'`: the grid is sky-standard (det CD < 0). A grid master is the
  mirror image of a raw frame from the owner's rigs (det CD > 0) and is right
  on the sky. An XISF grid master fires the existing bottom-up warning.
- **Coverage (D4, D9).** The engine counts, per output pixel, the frames
  whose sample is finite (before rejection), in a post-pass over the band
  already in memory; the combine loop is untouched. `output.coverageMap`
  writes `<stem>_coverage.fits` (`BUNIT = 'frames'`, the master's WCS, kind
  `coverage`) and drizzle's own `<drizzle stem>_coverage.fits` (kind
  `drizzle_coverage`, counted from a `W` snapshot around each deposit; a drop
  below `W`'s f32 resolution is not counted). A frame set's master is
  bit-identical with the map on and off. A project's seed turns it on. On
  a grid every master's statistics (`GroupStats`: location, scale, noise,
  PSF SNR, SNR gain, FWHM, eccentricity) are measured on the largest
  rectangle every pixel of which a frame covers, coverage map or not
  (review I1): the uncovered 0.0 pixels of a wide grid never read as data.
- **LN on the overlap (D5–D8).** A project's LN reference leaves uncovered
  pixels NaN, and each member is fitted on member ∩ reference: detection
  planes fill the rest with 0.0 (the detector ignores it), seeds stay 48 px
  inside the eroded overlap, and both backgrounds come from the overlap. A
  member under 25 % overlap keeps its global pair, gets no sidecar and an
  `ln_global` marker (a cache hit on the next run and in the plan), and the
  group warns once with the count. Admissibility on a grid uses the group's
  median footprint, not the whole grid. Frame sets keep the legacy LN; a
  project's LN hash folds `lnmask:1`. The field (review I6) is built from
  the in-scope frames' WCS footprints on a lattice of 32 probes per median
  frame side, forgives one lost probe, and its hash (the frames that pass
  against it) folds into every member's LN hash; the plan builds the same
  field (`origin::group_ln_field`), so its Normalize view moves with the
  field, never a run later.
- **Crop (D10).** With `output.cropToCoverage = K > 0` every output is
  written as FITS into `runs/run-<id>-uncropped/`; after the last group,
  `finish_crop` crops a co-registered run to ONE rectangle (the union of the
  groups' `coverage ≥ K` boxes) and a native run per group, writes the final
  files in the configured format, moves the rows, group paths and summary
  onto them, and removes the scratch. CRPIX moves by the crop origin (× the
  scale for drizzle outputs). A group with no rectangle, or whose crop fails,
  is finalized uncropped with a warning; one that cannot be finalized keeps
  its scratch files, which its rows name. `K = 0` is the old path; a
  full-rectangle finalization writes exactly what it writes (review I7). A
  crop does not re-measure `GroupStats`, and the plan's space estimate
  counts the scratch (review M6).
- **Output on the grid (D11).** The master's WCS is the grid's TAN
  (`grid_plate_solve`: the 0-based centre as `crpix`, no SIP).
  `stamp_grid_cards` writes the canonical `FILTER`, `ROWORDER`, `ATH_RGEO =
  'grid'`, `ATH_SPRJ` (with a comment only when it fits), `ATH_GRID`, and
  drops `PLTSOLVD`; `ATH_PRJ` is never written. Masters land in
  `<output>/<sanitized title>/`.
- **Starting (D13, D14, D16, D17).** `start_stacking` takes a project. The
  run's grid is the plan's confirmed one, recorded in `RunSummary.grid`. The
  plan blocks a run certain to fail at Reference — a manual pin, or every
  in-scope frame, that will have no WCS — judged with the installed solver's
  identity, so a cached failure blocks only while it stands; it warns when the
  pin's WCS waits on a solve. A steady-state plan writes nothing.
  `Intermediates` and `All` sweep `tmp/`.
- **Commands (D12).** Seven on both hosts (strict count 306):
  `get_project_stacking_plan`, `start_project_stacking`,
  `get_project_stacking_runs`, `get_project_stacking_config` (read only: the
  seed, `isDefault`, while nothing is stored), `set_project_stacking_config`
  (sorted unique exclusions; an absent `referenceFrameUuid` clears the pin),
  `get_project_stacking_work_usage`, `cleanup_project_stacking_work` (after
  the active-run check, sweeps the rows of frames that left the project;
  refuses a layout that overlaps a scan root). Run-history frame rows carry `frameUuid`.
- **Tests.** `api/stacking_project_e2e.rs` stacks a fake-hub project — two
  publishers, two canonical filters, a coordinator exclusion, a frame the run
  solves, a pair under 25 % overlap — twice. Every project run test holds
  `grid_counters::exclusive()`.
- **Open for 6c:** the project page's Stacking tab (the wire types are final),
  `/projects/<id>?tab=stacking` for the completion notification. The 25 %
  threshold stays synthetic until a real partial-overlap project exists
  (open-items).
```

- [ ] **Step 2: The transfers reference**

In `docs/transfers/README.md`, after the 6b-2 section:
```markdown
## Collab wave 6b-3 — project stacking (2026-10-03)

The stacking side is in `docs/stacking/README.md` (The project path on the grid, wave 6b-3).

- Seven project-stacking commands on both hosts (strict count 299 → 306).
- `cleanup_project_stacking_work`, once no project run is active, sweeps the stacking artifact rows of every handle whose `(project, uuid)` left `project_frames_local`: rows only, never a file. A coordinator-excluded frame is still local, so its cache survives a restore.
- A project run reads the replicas in place and writes nothing under the Collaboration root; no file it writes carries `ATH_PRJ`.
- An acceptance catalog copy that stacks a project runs signed out (its `account.hub_url` points at an unreachable address), so the feed never replaces its local grid. `account_sign_out` is never called on a copy: the token store is shared with the real app.
```

- [ ] **Step 3: The logging dictionary**

In `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, after the wave 6b-2 entry's last bullet (`stamp`):
```markdown

**Dictionary extension (collab v3 wave 6b-3, `stacking::{run, ln, crop}`, 2026-10-03):** five new names.

- **`overlap`** (f64 in [0, 1]): the fraction of a member's footprint its group's LN reference covers, measured on channel 0, on the debug `"ln: frame keeps global normalization (little overlap with the LN reference)"`.
- **`crop_x0`**, **`crop_y0`**, **`crop_width`**, **`crop_height`** (usize): a rectangle in the master's array pixels — the crop's, on the info `"outputs cropped"` and the warn `"crop rectangle outside the group's master; outputs written uncropped"`; the covered rectangle a grid master's statistics are measured on, on the debug `"master statistics on the covered rectangle"`.
```

- [ ] **Step 4: Open items**

In `docs/superpowers/open-items.md`:
- move the 6b-2 bullet "The solve's ADU copy survives a process death" into the new entry as its closing line (below; writer's note 3, corrected by the review);
- delete the 6b-2 bullet "The grid's orientation (6b-3)" (it moves into the new entry);
- add, as the first entry under "## Unverified by hand":
```markdown
### Collab wave 6b-3 — project stacking on the grid (2026-10-03)

Code-complete with green gates: the composed map, both caches, LN on the overlap with the 25 % fallback, the coverage maps, crop, grid output, the seven commands on both hosts, `start_stacking` for a project, and the fake-hub e2e. Spec `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §5.5–§5.9, §9, §15; plan `docs/superpowers/plans/2026-10-03-collab-v3-wave6b3-project-grid-stacking-plan.md`. Reference: `docs/stacking/README.md` (The project path on the grid, wave 6b-3). There is no project UI before 6c. Owed:

- **The acceptance runs** (the plan's last section): the set-204 frame-set regression against `main`, run A's four rows, run B's report and the WCS probe. Record each result here when it is run; a missed run-A row is reported with its numbers for an owner ruling, never tuned.
- **The grid's orientation.** A grid master is the mirror image of a raw frame from the owner's rigs: the grid is sky-standard (det CD < 0) and every solve in the owner's catalog has det CD > 0. Check the first real grid master against a sky chart. `ROWORDER` on the grid is always `BOTTOM-UP`.
- **The 25 % LN-overlap threshold is still synthetic** (see the 6b-2 entry): run B reports each member's overlap, but LDN1272-test has no member under 25 %.
- **Drizzle coverage resolution.** A drop whose weight is below `W`'s f32 resolution at a pixel is not counted (D9). Expected to be invisible at real weights; look at it if a drizzle coverage map shows holes where the drizzled master has signal.
- **Crop disk use.** A crop run holds every output twice for a moment (scratch FITS, then the final file); the plan's space estimate counts it (review M6). A failed finalization leaves `runs/run-<id>-uncropped/` in place on purpose.
- **Run B's "noise per arcsec²"** is σ_px · scale (D23), measured on the master cropped to `coverage ≥ ⌈n/2⌉`. It is a white-noise equivalent: resampling (and more so drizzle) correlates a stack's pixels, so it understates the true per-area noise by a factor that depends on the scale. Owner ruling owed if another definition is wanted.
- **Closed from 6b-2: the solve's ADU copy surviving a process death.** `Intermediates` and `All` now sweep `tmp/` for every subject (D17). That is a behaviour change for frame sets too: a frame set's `Intermediates` cleanup removes a directory it did not remove before (it holds no artifact rows, only crash leftovers).
```

- [ ] **Step 5: CLAUDE.md**

- In the Module Map's Tauri sentence: `— 299 \`#[tauri::command]\` functions` becomes `— 306 \`#[tauri::command]\` functions`, the recount date stays `2026-10-03`, and after `two added by wave 6b-2 (2026-10-03): \`get_collab_grid_proposal\`, \`set_collab_project_grid\`;` insert ` seven added by wave 6b-3 (2026-10-03): \`get_project_stacking_plan\`, \`start_project_stacking\`, \`get_project_stacking_runs\`, \`get_project_stacking_config\`, \`set_project_stacking_config\`, \`get_project_stacking_work_usage\`, \`cleanup_project_stacking_work\`;`.
- In the Stacking section, the bullet `- **19 commands** in \`api/stacking.rs\`, …, \`list/save/delete_stacking_preset\`.` becomes:
  ```markdown
  - **26 commands** in `api/stacking.rs`, mirrored on both hosts: plan/start/cancel, runs, config/defaults/presets, paths/work-usage/cleanup, `get_master_light_preview` (`maxPx` clamped to `[64, 2048]`), `list/save/delete_stacking_preset`, and the seven project twins (wave 6b-3). A project run stacks onto the coordinator's canonical grid: one composed map per frame, LN on the overlap, a coverage map and an optional crop (`docs/stacking/README.md`, The project path on the grid).
  ```

- [ ] **Step 6: The spec's count notes**

In the wave-6 spec, §9.1, after "The count goes 294 → 305.": ` (Count note, 2026-10-03: plan 6a added three commands and plan 6b-2 two before this table's last seven landed, so the strict count went 297 → 299 → 306.)` In §18's docs bullet, `command count 294 → 305` becomes `command count 294 → 305 (actual 306, see §9.1's count note)`.

- [ ] **Step 7: The final gates**

Run the Gate per task commands, one at a time, then:
```bash
grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l
grep -rn PROJECT_STACKING_UNAVAILABLE crates/ ; echo "no guard left: $?"
git diff --stat main...HEAD -- src/types
git diff main...HEAD --diff-algorithm=histogram --stat
```
Expected: every gate green with the known flakes handled as Global Constraints say; `306`; `no guard left: 1`; only `src/types/stacking.ts` under `src/types`. Read the added lines of the whole branch once for a third-party program name (the project rule): there must be none.

- [ ] **Step 8: Commit**
```bash
git add docs/stacking/README.md docs/transfers/README.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md docs/superpowers/open-items.md CLAUDE.md docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md
git commit -m "docs: the project path on the grid (wave 6b-3) — stacking and transfers references, logging dictionary, open items, command count

$(printf 'Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK')"
```

---

## Acceptance runs (the controller's own agent, after Task 11)

Spec §15.3 and Review Focus 1, on real data, with the Task 10 drivers on the finished branch. These are runs, not tasks: nothing is committed except the results the controller records in `docs/superpowers/open-items.md` (the 6b-3 entry). A missed row is reported with its numbers for an owner ruling, never tuned toward the tolerance.

**Ground rules (every run):**
- **One compile at a time.** No other cargo, and no other session's build, while a run is timed (the drivers refuse otherwise).
- **Time Machine.** Every driver refuses to start while a backup runs. The answer is `tmutil stopbackup`, then waiting until `tmutil status` shows `Running = 0`, then the same command again. Never edit, skip or script around the refusal.
- **Catalog copies.** Only `sqlite3 -readonly "<live db>" ".backup '<copy>'"` reads the live catalog; `prepare-catalog.sh` is fed that copy through `ATH_ACC_DB`. The live catalog is never served, never written and never passed to any other script.
- **Where.** Working folders live on `/Volumes/BigMac` under `$HOME/.athenaeum-acc/` (433 GiB free at plan time). `/Volumes/bigbase3` is a slow HDD: do not put a working folder there. Before each run read `df -h /Volumes/BigMac` and the plan's `estimateBytes`; stop and report if free space is under the estimate plus 20 GB. Keep every copy after its numbers are taken and report its size (`du -sh`); the owner decides what to delete. Never remove `tierA-template`, `tierA-baseline`, `tierA-main6b2` or `tierA-branch6b2`.
- **Signed out.** A copy that stacks a project runs with `account.hub_url = http://127.0.0.1:9` (set before its server starts), so no feed replaces its local grid and nothing talks to the hub as this device. Check `api.sh account_status '{}' <port>` shows `"signedIn":false`. Never call `account_sign_out` on a copy: the token store is shared with the real app.

Shared variables (the code blocks inside numbered steps are indented for the list: strip the three leading spaces when running them, so the heredoc terminators `SQL` and `PY` start at column 0):
```zsh
REPO=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
ACC=$REPO/docs/superpowers/research/scripts/acceptance
DEV="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db"
```

**Run 0 — tools.** Build `measure_probe` into the harness target dir once: `cd $REPO && CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=$REPO/.superpowers/target-acc cargo build --release -p athenaeum-core --example measure_probe`.

**Run 1 — the set-204 frame-set regression (Review Focus 1).**
```zsh
zsh $ACC/tier1/rerun-build.sh 20c7c1ef main6b3
zsh $ACC/tier1/checkpoint.sh branch6b3 main6b3
```
The base is a fresh run of `main` at the branch point (`20c7c1ef`, with 6b-2), back to back with the branch run (R-TA-9). Expected in `tierA-branch6b3/compare.txt`: every master (plain, drizzle, weight map) `raw_identical=True`, registration rows identical, `.athln` sidecars identical, calibrated frames identical. Any difference: stop and report it with the compare output; nothing else in this section is meaningful until it is explained.

**Run A — comparable geometry (spec §15.3).** The project LDN1272-test against a personal co-registered stack of the same frames, on the same reference, on a grid of that reference's own scale, rotation, centre and size.

1. The copy, signed out:
   ```zsh
   W=$HOME/.athenaeum-acc/acc6b3-runA
   mkdir -p "$W"
   sqlite3 -readonly "$DEV" ".backup '$W/dev-snapshot.db'"
   ATH_ACC_DB="$W/dev-snapshot.db" zsh $ACC/prepare-catalog.sh "$W" fits 204
   sqlite3 "$W/athenaeum.db" "INSERT OR REPLACE INTO settings (key, value) VALUES ('account.hub_url', 'http://127.0.0.1:9');"
   ```
2. The project and the reference X (Tier A's own reference when the project holds it with a manifest WCS):
   ```zsh
   PID=$(sqlite3 "$W/athenaeum.db" "SELECT project_id FROM collab_projects WHERE title = 'LDN1272-test Project'")
   X=$(sqlite3 "$HOME/.athenaeum-acc/tierA-main6b3/athenaeum.db" "SELECT reference_frame_id FROM stacking_runs WHERE frames_set_id = 204 AND status = 'done' ORDER BY id DESC LIMIT 1")
   UUID=$(sqlite3 "$W/athenaeum.db" "SELECT frame_uuid FROM project_frames_local WHERE project_id = '$PID' AND source_frame_id = $X AND accepted = 1 AND on_disk = 1 AND json_extract(manifest_json, '\$.meta.wcs') IS NOT NULL")
   echo "project $PID reference frame $X uuid $UUID"
   ```
   An empty `UUID` means Tier A's reference is not usable here: take the first accepted, on-disk OSC frame of the project with a manifest WCS (`json_extract(manifest_json, '\$.meta.bayerpat') IS NOT NULL`), set `X` to its `source_frame_id`, and report the choice.
3. The grid of X's manifest WCS, in the copy only:
   ```zsh
   python3 $ACC/run-a-grid.py "$W/athenaeum.db" "$PID" --reference-uuid "$UUID" --apply
   ```
   The grid's x axis is mirrored relative to X (D3); no run-A row reads orientation. The script also prints X's scale, rotation, pixel aspect and skew (review I4): the grid's pixels are square and unskewed, X's only as far as its CD is. Record the line in the report; an aspect off 1 by more than 1e-3, or a skew above 1e-3, means the two masters are two resamplings, and the WCS probe of step 8 decides whether the rows may be read.
4. The same frames, config and reference on both sides. The personal run excludes the set-204 lights the project does not stack (13 at plan time: 9 mono, 4 OSC). The project document is set 204's own config with the manual reference and the coverage map on. First, refuse to go on without set 204's stacking config (review I3: without it the statements below change nothing and both runs would start on defaults, silently):
   ```zsh
   if [ "$(sqlite3 "$W/athenaeum.db" "SELECT COUNT(*) FROM stacking_set_config WHERE frames_set_id = 204")" != "1" ]; then
     echo "STOP: set 204 has no stacking_set_config row in this snapshot (and set 109 none to copy) — report it; do not invent one" >&2
   fi
   ```
   On `STOP`, stop and report. Otherwise:
   ```zsh
   sqlite3 "$W/athenaeum.db" <<SQL
   UPDATE stacking_set_config
      SET config_json = json_set(config_json, '\$.reference.mode', 'manual'),
          excluded_frame_ids_json = (SELECT json_group_array(id) FROM (
            SELECT DISTINCT f.id AS id FROM session_members sm
              JOIN sessions s ON s.id = sm.session_id
              JOIN imaging_nights ino ON ino.id = s.imaging_night_id
              JOIN frames f ON f.id = sm.frame_id
             WHERE ino.frames_set_id = 204 AND f.imagetyp = 'Light'
               AND f.id NOT IN (SELECT source_frame_id FROM project_frames_local
                                 WHERE project_id = '$PID' AND accepted = 1 AND on_disk = 1
                                   AND source_frame_id IS NOT NULL)
             ORDER BY f.id))
    WHERE frames_set_id = 204;
   INSERT OR REPLACE INTO frame_set_reference (frames_set_id, reference_frame_id, set_at)
   VALUES (204, $X, datetime('now'));
   INSERT INTO stacking_project_config (project_id, config_json, excluded_frame_uuids_json, reference_frame_uuid, updated_at)
   SELECT '$PID', json_set(config_json, '\$.reference.mode', 'manual', '\$.output.coverageMap', json('true')), '[]', '$UUID', datetime('now')
     FROM stacking_set_config WHERE frames_set_id = 204
   ON CONFLICT(project_id) DO UPDATE SET
     config_json = excluded.config_json,
     excluded_frame_uuids_json = '[]',
     reference_frame_uuid = excluded.reference_frame_uuid,
     updated_at = excluded.updated_at;
   SQL
   sqlite3 "$W/athenaeum.db" "
     SELECT 'set', json_extract(config_json, '\$.reference.mode'), json_array_length(excluded_frame_ids_json)
       FROM stacking_set_config WHERE frames_set_id = 204;
     SELECT 'set pin', reference_frame_id FROM frame_set_reference WHERE frames_set_id = 204;
     SELECT 'project', json_extract(config_json, '\$.reference.mode'), reference_frame_uuid,
            json_extract(config_json, '\$.output.coverageMap')
       FROM stacking_project_config WHERE project_id = '$PID';"
   ```
   Expected, verbatim: `set|manual|13`, `set pin|$X`, `project|manual|$UUID|1`. Anything else: stop and report.
5. Both plans before either run (the drivers start their own server; this one is for the check only):
   ```zsh
   zsh $ACC/server.sh "$W" 8960 > "$W/server-check.out" 2>&1 &
   until curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8960/api/account_status -H 'content-type: application/json' -d '{}' | grep -q 200; do sleep 5; done
   zsh $ACC/api.sh account_status '{}' 8960
   zsh $ACC/api.sh get_stacking_plan '{"setId":204}' 8960 > "$W/plan-set.json"
   zsh $ACC/api.sh get_project_stacking_plan "{\"projectId\":\"$PID\"}" 8960 > "$W/plan-project.json"
   pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web"
   python3 - "$W" "$PID" "$UUID" "$X" <<'PY'
   import json, sqlite3, sys
   w, pid, uuid, x = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
   s, p = (json.load(open(f"{w}/{n}.json")) for n in ("plan-set", "plan-project"))
   handle = sqlite3.connect(f"{w}/athenaeum.db").execute(
       "SELECT id FROM stack_project_frames WHERE project_id = ? AND frame_uuid = ?", (pid, uuid)
   ).fetchone()
   print("set blockers", s["blockers"], "included", s["includedCount"], "estimate GB", s["estimateBytes"] / 1e9)
   print("set reference", s["reference"])
   print("project blockers", p["blockers"], "included", p["includedCount"], "grid", p["grid"]["state"], "estimate GB", p["estimateBytes"] / 1e9)
   print("project reference", p["reference"], "pin handle", handle)
   ok = (
       s["reference"]["mode"] == "manual" and s["reference"]["frameId"] == x
       and p["reference"]["mode"] == "manual" and handle is not None and p["reference"]["frameId"] == handle[0]
   )
   print("REFERENCE OK" if ok else "STOP: the two runs would not share the reference")
   PY
   ```
   Expected: `"signedIn":false`; no blockers on either; the grid `confirmed`; equal included counts (184 at plan time); both references manual on X (the project's on X's handle), `REFERENCE OK`. Anything else: stop and report (review I3).
6. The personal run, then the project run:
   ```zsh
   zsh $ACC/tier1/tier1-run.sh "$REPO" "$W" 8960 204
   python3 $ACC/tier1/tier1-extract.py "$W" > "$W/extract-set.txt"
   zsh $ACC/tier1/project-run.sh "$REPO" "$W" 8960 "$PID"
   ```
7. **Calibration equivalence, before reading any row.** The project stacks the PUBLISHED files (calibrated by the publisher, OSC as CFA mosaics Prepare debayers); the personal run calibrates from raw. For three mono and three OSC frames, compare the personal `calibrated` artifact with the project's input:
   ```zsh
   sqlite3 "$W/athenaeum.db" "
     SELECT s.path, COALESCE(pa.path, l.landed_path)
       FROM stacking_artifacts s
       JOIN project_frames_local l ON l.project_id = '$PID' AND l.source_frame_id = s.frame_id
       JOIN stack_project_frames h ON h.project_id = l.project_id AND h.frame_uuid = l.frame_uuid
       LEFT JOIN stacking_artifacts pa ON pa.project_id = l.project_id AND pa.project_frame = h.id AND pa.kind = 'calibrated'
      WHERE s.frames_set_id = 204 AND s.kind = 'calibrated'
      ORDER BY s.frame_id LIMIT 200" > "$W/calibrated-pairs.txt"
   ```
   Pick three rows whose personal file has no `_d` (mono) and three with `_d` (OSC), and run `python3 $ACC/imgcmp.py <personal> <project>` on each. Equal to float rounding (max |Δ| ≤ 1e-5): go on. Larger: stop before step 8 and report the six outputs — a calibration difference would read as a grid or LN difference.
8. **WCS first (spec §15.3, review I4).** Before any row is read, for each project master:
   ```zsh
   cd $REPO && ATHENAEUM_GRID_PROBE_MASTER=<project master path> \
     ATHENAEUM_SOLVE_PROBE_APP_DATA="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev" \
     CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib real_data_grid_master_wcs_probe -- --ignored --nocapture
   ```
   Expected: the independent solve agrees with the written WCS within 0.5 px. A master past 0.5 px: stop before step 9 and report — its rows would compare two geometries.

9. The rows (after step 8 passed):
   ```zsh
   sqlite3 "$W/athenaeum.db" "SELECT m.run_id, COALESCE(r.frames_set_id, r.project_id), m.group_key, m.kind, m.channels, m.path
     FROM master_lights m JOIN stacking_runs r ON r.id = m.run_id
    WHERE m.kind IN ('master', 'drizzle') ORDER BY m.run_id, m.kind, m.channels"
   python3 $ACC/tier1/tier1-compare.py --pairs "<project mono master>=<personal mono master>" "<project OSC master>=<personal OSC master>" > "$W/pairs.txt"
   python3 $ACC/tier1/tier1-compare.py --pairs "<project mono drizzle>=<personal mono drizzle>" "<project OSC drizzle>=<personal OSC drizzle>" > "$W/pairs-drizzle.txt"
   ```
   Run A's verdict is `pairs.txt` (masters: median ± 0.1 %, MAD ± 1 %, noise ± 2 %, FWHM ± 1 %). `--pairs` refuses a pair whose pixel grids differ (review I4). The drizzle pairs are reported for information only.
Report for run A: `pairs.txt` and `pairs-drizzle.txt` verbatim; the two `run*.txt` walls; the calibration-equivalence numbers; the WCS offsets; X, its scale/rotation/aspect/skew line, and the grid JSON; the step-4 and step-5 reference checks.

**Run B — the proposed grid (spec §15.3, D23).** The same project on the grid the app proposes (median scale, north up, target centre). No tolerance: the report goes to the owner.

1. The copy, signed out, as run A step 1 with `WB=$HOME/.athenaeum-acc/acc6b3-runB` (and `PID`, `X`, `UUID` read as in run A step 2).
2. The proposal, stored in the copy:
   ```zsh
   zsh $ACC/server.sh "$WB" 8961 > "$WB/server-check.out" 2>&1 &
   until curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:8961/api/account_status -H 'content-type: application/json' -d '{}' | grep -q 200; do sleep 5; done
   zsh $ACC/api.sh account_status '{}' 8961
   PROP=$(zsh $ACC/api.sh get_collab_grid_proposal "{\"projectId\":\"$PID\"}" 8961 | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["proposal"]))')
   pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web"
   python3 $ACC/run-a-grid.py "$WB/athenaeum.db" "$PID" --grid-json "$PROP" --apply
   ```
3. The project document of run A step 4 (its set-204 refusal check, then only its `INSERT INTO stacking_project_config …` statement and the `project` line of its check, against `$WB/athenaeum.db`): the same config and reference, so only the grid differs.
4. The plan check of run A step 5 for the project only (port 8961): no blockers, the grid `confirmed`, and the estimate under the free space. A `space` blocker: stop and report the estimate.
5. The run and the report:
   ```zsh
   zsh $ACC/tier1/project-run.sh "$REPO" "$WB" 8961 "$PID"
   python3 $ACC/grid-report.py "$WB" "$PID" --overlap > "$WB/grid-report.txt"
   ```
6. The WCS probe of run A step 8 on each run-B master.

Report for run B: `grid-report.txt` verbatim (per group: the grid, the coverage histogram and its `coverage ≥ ⌈n/2⌉` rectangle, FWHM in px and arcsec, eccentricity, noise per pixel and per arcsec², each member's overlap with the LN reference and the count under 25 %), the wall, the WCS offsets, and the warnings of the run (`summary.warnings` in `stacking_runs.summary_json`).

---

## Writer's notes for the controller

- D1 departs from the research's polynomial-order-5 option: the composed map is a sampled displacement layer, because a polynomial cannot follow a spline star map and needs the order cap raised for every registration (the review adds: an order-3 star map composed with an order-3 SIP is degree 9, so no order-5 polynomial follows any star map with a distortion layer).
- D2 departs from spec §4.2 (the star map in the `registered` rows): those rows are erased by `Intermediates`, so the star map is its own payload-only `registration` artifact that only `All` drops.
- D17 changes frame-set cleanup too: `Intermediates` and `All` now remove `tmp/` for every subject (6b-2 left it out for exactly that reason; it holds no artifact rows, only crash leftovers). Corrected per the review: this is a behaviour change for frame sets, now stated in Global Constraints beside "every written byte stays identical" (true of outputs only), and the 6b-2 open-items bullet it closes becomes one line of the 6b-3 entry (Task 11).
- D8 and D10 fill spec gaps (grid-mode admissibility on the median footprint; crop as a deferred scratch-FITS pass because an XISF master cannot be re-read and the union is known only after the last group). D8 is refined by review I6: a WCS-footprint field on a frame-unit lattice with one probe of grace, built by one function the run and the plan both call and folded into every LN hash.
- The e2e's east pair registers over a ≈ 15 % strip. Its geometry (offset, 40″ spacing, seed) is one set of `test_fixtures` constants that Task 5b's run test shares and tries first, a hundred times cheaper; the next lever is 32″ on the shared constant, never a weaker assertion (review I5).
- Task 8 is split three ways: 8a (a project run starts; the plan's WCS rule), 8b (the seven core commands; the only TS change of the three), 8c (the hosts, 299 → 306). The header, Global Constraints, Review Focus and File map follow the split.
- D23 is new (run B's "noise per arcsec²" = σ_px · scale on the `coverage ≥ ⌈n/2⌉` crop); the research left the term undefined, and an owner ruling is owed if another definition is wanted. With the review's caveat: it is a white-noise equivalent — resampling, and more so drizzle, correlates a stack's pixels, so it understates the true per-area noise by a scale-dependent factor; the report says so in its header, beside σ_px.
- D14 judges a cached solve with the installed solver's identity (`solve::installed_solver_identity`, shared with `CatalogSolver`), so a failure cached under another catalog never blocks a run that would retry it; the plan still reads only.
- Crop restates `CRPIX` on the written cards (`crop::restate_crpix`) instead of rebuilding the WCS block, which would bring `PLTSOLVD` back onto grid masters; `crop_plate_solve` supplies the values.
- Every project run test now holds `grid_counters::exclusive()` (a composed map is a sampled layer that builds grids); the guard was inserted into the Task 2b1/2b2, 3, 5a and 5b tests and stated in Global Constraints.
- `cleanup_project_stacking_work` also refuses a layout root that overlaps a scan root (`Invalid`), mirroring the plan's 6b-2 `folders` blocker (research §3.3); the spec text does not say it. After review M5 it sweeps departed frames' rows only once no project run is active.
- Spec §5.9's "crpix 1-based" is read as the card (1-based) over a 0-based `PlateSolveRecord.crpix` (D11), as every other writer does.
- Rewritten per the review (I1): every grid master's statistics are measured over covered pixels only — the largest rectangle every pixel of which a frame covers — coverage map or not; the defect was the uncovered zeros of every grid master, not the crop. A crop does not re-measure them, and the plan's space estimate now counts the crop scratch (review M6).
- The e2e answers its one unsolved frame with that frame's true record through `FakeSolver::with(file name)`; a float FITS is used in place, so the request's file name is the rendered one.
- Acceptance work dirs are on `/Volumes/BigMac` per the brief (the research proposed bigbase3); the copies run signed out through an unreachable `account.hub_url`, never `account_sign_out`.
- The set-204 regression's base is a fresh `rerun-build.sh 20c7c1ef` run back to back with the branch checkpoint, not the older `tierA-main6b2` checkpoint.
- Run A's reference comes from Tier A's own pick when the project holds it with a manifest WCS, else the first OSC frame with one; the grid is that frame's manifest WCS, so the project master is mirrored in x (no run-A row reads it). Per reviews I3 and I4, the procedure refuses without set 204's stacking config, prints and checks both references (mode and pin), prints X's pixel aspect and skew, runs the WCS probe before any row, and `--pairs` refuses differing pixel grids.
- The 25 % LN threshold stays synthetic on real data (D19); run B reports each member's overlap so the owner sees how far LDN1272-test is from it.
- The review fold: tasks 2b and 7b are split at the review's cut points (2b1 fixture with the first grid test ignored, 2b2 the composition; 7b1 the crop writers and the estimate, 7b2 the run's pass); Task 7a now runs before 6a (6a's I1 fix needs `largest_covered_rect`); Tasks 1 and 5a stay whole; I1–I7 and M1–M18 are applied where named, and every changed ruling is marked **Refinement (review)**.
- Every minor is applied as the review proposes; M3's comment change moves no frame-set byte because no frame set passes a reference row order today (`run.rs:6892`). Nothing was skipped.
- No task names a third-party program; the plan was written without running cargo, and the SQL it adds (the departed-frame sweep, the run-frame uuid sub-select, the test inserts, the run-A exclusion list) was checked with `sqlite3 :memory:` or `sqlite3 -readonly` on the live schema.
