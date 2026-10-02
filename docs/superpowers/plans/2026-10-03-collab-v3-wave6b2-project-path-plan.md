# Collab v3 wave 6b-2 — the project path up to the geometry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a collaboration project a working stacking subject up to (not including) registration onto its canonical grid. The project's plan is complete: groups by colour × canonical filter × exposure class, blockers, warnings, the grid state and the Frames rows. Its frames are prepared, solved where they have no WCS, measured and given a WCS-bearing reference. The grid itself exists end to end: the hub copy, the feed, the proposal and two commands. Frame-set stacking does not move: every key, stage hash, log line and error string stays byte-identical.

**Architecture:**
- **The wire is subject-aware** (`setId | projectId`, exactly one set) on the two events, `RunSummary`, `StackingRunRow` and `StackingPlan`. The frontend keeps frame sets working and drops project events until 6c.
- **Four config fields** land: `grouping.exposureMode` and `stopsWidth`, `output.coverageMap` and `cropToCoverage`. They enter no stage hash, which the golden literals prove. Exposure classes are a pure, collision-safe classifier shared by both frame sources.
- **`FrameOrigin::Project` carries everything a run needs about a landed replica**: its manifest `meta` (parsed once), its content identity and its `size:mtime` stamp. The compiler then names every catalog read that needs a project arm. A grep test keeps subject and origin dispatch inside named funnels.
- **`build_plan` splits into two arms and one shared core.** The frame-set arm is today's code, moved. The project arm resolves the seeded per-project config, the stackable frames, the grid state, its blockers and its warnings. The gates after that are shared.
- **Stage 1 dispatches.** A project's replicas, and an attested frame set's lights, run **Prepare** instead of Calibrate. Prepare uses a file in place when its pixels are already unit-domain float, converts it otherwise, and debayers it when `debayerOsc` is on.
- **A Solve sub-step at the start of Measure** solves project frames that have no manifest WCS, through an injectable solver. It caches every outcome as a `solve` artifact. The Reference stage then only picks a frame with a WCS.
- **The canonical grid has one typed, normalized, hashed shape.** The app sends it through `PUT /projects/{id}/grid` and keeps a copy refreshed with the project and moved by the feed. Two commands, on both hosts, propose and save it.

**Tech Stack:** Rust (`athenaeum-core`): rusqlite 0.40, serde, xxhash, blake3, rustafits (`astroimage`), solvemyastro. ts-rs (`tests/ts_contract.rs`). Axum (`athenaeum-web`) and Tauri (`athenaeum-tauri`) wrappers. React/TS: one hook guard and two test fixtures.

**Spec:** `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` (owner-approved, fable-reviewed). Read §2, §4–§9 and §13–§16 before any task. This plan implements §18 plan 6b steps **3, 4, 5, 6, 7 and 13**, with the wire change of §9.2 brought forward. Steps 8–12 and 14 are plan 6b-3: registration onto the grid, LN on overlap, coverage and crop, drizzle and output on the grid, the seven project-stacking commands, and the fake-hub e2e. In 6b-2, `start_stacking` for a project still refuses with `PROJECT_STACKING_UNAVAILABLE`; project stages are exercised only through `project_test_context` (Task 6b) in tests.

**Execution order:** Task 0, 1, 2, 3, 4, 5, 6a, 6b, **8a, 8b, 7**, 9a, 9b, 10a, 10b, 10c, 11. Tasks 8a and 8b (the grid) land before Task 7 (the project plan), because the plan reports the grid state. Tasks 6, 8, 9 and 10 are split into halves (6a/6b, 8a/8b, 9a/9b, 10a/10b/10c) at the review's cut points; each half has its own Files, Interfaces, Steps, gate and commit, and leaves every gate green on its own. The tasks keep the skeleton's numbers and appear in the document in execution order.

**Research behind this plan** (all in `docs/superpowers/research/6b/`, verified at `e03bc48f`):
- `2026-10-03-6b2-source-plan-groups.md`: steps 3, 7 and 13;
- `2026-10-03-6b2-prepare-solve.md`: steps 4 and 5;
- `2026-10-03-6b2-grid-geometry.md`: step 6, plus the 6b-3 geometry;
- `2026-10-03-6b2-commands-e2e.md`: the wire change, the layout and the grid commands;
- `2026-10-03-6b2-ln-coverage.md`: the output config fields;
- `2026-10-02-stack-subjects-assessment.md`: the funnels rule;
- the 6b-1 notes `2026-10-02-6b1-*.md`.

The foundation is `docs/stacking/README.md` § "Run subjects (wave 6b-1)".

## Global Constraints

- **Branch:** `collab-wave6b2`, cut from local `main` at `e03bc48f`, which carries plan 6b-1. Merge and push only on the owner's word.
- **Commits** are made as the configured user (`eg013ra1n` / `vilen.sharifov@gmail.com`). Every message ends with these two lines:
  `Co-Authored-By: <the model that wrote the code> <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK`
- **Frame-set behaviour is preserved, and this is checked at every task.**
  - Every existing test passes. An existing assertion may change only where a task names it. The full list:
    - `db/stacking.rs:~1786`, the project run's wire `frames_set_id` (Task 1);
    - `config_hash_is_canonical_and_pinned`'s literal (Task 2);
    - the `GroupingConfig { exposure_tolerance_sec: 2.0 }` literal at `groups.rs:~734` (Task 2);
    - `frame_source_returns_one_catalog_origin_per_frame`: its `Catalog { file_id }` value and its project-refusal tail (Task 6a);
    - the `projects` → `_projects` paths in `paths.rs:~1911` and `run.rs:~19936` / `~19986` (Task 5);
    - `a_project_plan_is_refused_until_6b2`, which is replaced (Task 7).
  - Mechanical edits are allowed anywhere the compiler demands them: a new struct field in a literal, `frame_source`'s config argument, `FanOutTicker::from_parts`'s subject argument.
  - Frame-set group keys, every stage hash (`stage_hashes_are_pinned_to_golden_literals` keeps its literals), frame-set log messages and fields, and error strings stay byte-identical. A log line that gains a subject-aware field records `set_id` as `Option<i64>`, which `tracing` records exactly as the old `i64` when it is `Some`, and `project_id` as `Option<&str>`, which records nothing when it is `None`.
  - `a_project_start_is_refused_until_6b2` (`run.rs:~19861`) stays green to the end of this plan.
- **TypeScript:** `src/types/` may change only in **Tasks 1, 2, 3, 7 and 8b**, and each of those tasks lists exactly what moves. Task 8a adds `CanonicalGrid` with a `ts_rs::TS` derive but does not register it, so its TS does not move. Every other task ends with `git diff --exit-code src/types`. An exported field added by this plan carries a `//` comment, never `///`, because ts-rs copies `///` docs into the generated files.
- **The headless build stays green:** `cargo check -p athenaeum-core --no-default-features`.
  - Ungated: `db::*`, `collab::grid`, `collab::hub_client`, `api::collab_exchange`, `services::*`.
  - Gated `all(render, solver)`: `stacking::*`, `api::stacking`, `api::collab`, `api::collab_grid`, `collab::frame_meta`, `collab::grid_geometry`, `plate_solve::*`, and the feed (`api::collab_live::feed`).
- **The replica is sacred.**
  - No code path writes, renames or deletes a landed file.
  - No file a stacking run writes carries `ATH_PRJ`, `ATH_FILT`, `CALSTAT` or `ATH_CSRC`.
  - Cleanup removes directories under the working layout, never a file by an artifact's path.
- **No project run starts.** `start_stacking(Project)` refuses. Project stages run only from tests, through `project_test_context` (Task 6b).
- **Errors and logging:**
  - Never swallow an error; log at `warn!` or `error!` before degrading.
  - `tracing` only, with a short stable message and snake_case fields. Zero `println!` outside tests.
  - New field names are collected in Task 11 into the logging dictionary. Each task names its own.
- **Build discipline (this Mac):**
  - `CARGO_BUILD_JOBS=4` and `--test-threads=4`, one compile at a time.
  - Format touched files with `rustfmt <files>`, never `cargo fmt -p`, and revert sibling-module reformatting that rustfmt drags in.
  - `cargo check -p athenaeum-core --lib` stays warning-free. Clippy is not a gate.
- **Two backends in sync.** Task 8b's two commands go on both hosts. Task 10a's core catalog opener is called by both hosts' plate-solve modules.
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
  - Run `npx vitest run <the task's touched frontend files>` too when the task touches `src/`.
  - The lib check must print no `warning`.
  - `git diff --stat src/types` must be empty except in Tasks 1, 2, 3, 7 and 8b.
  - Three tests are known to flake under full-suite load: the two wall-clock collab live tests and `landing_free_bytes_is_bounded_by_the_collaboration_root`. If one fails, re-run it alone and report both runs.
- **Copy rules:** comments, docs and commit messages are in English. Never name a third-party project in the repo.

## Review Focus

1. **Frame-set stacking does not move.** This covers group keys under `seconds`, every stage hash with the new config fields set, the calibrated-light generator's pixels after the debayer extraction, and the plan, run and logs of a frame set.
   Pinned in:
   - Task 2: the golden extension;
   - Task 3: the keys, and `seconds_classes` equal to the old clustering;
   - Task 9a: the generator pixel pin and `calibrate_fan_out_matches_the_sequential_stage_byte_for_byte`;
   - Task 10b: `the_solve_sub_step_is_a_no_op_for_a_frame_set`; Task 10c: `a_frame_set_run_has_no_wcs_rule`;
   - every task's full core gate.
2. **A project handle never reaches a catalog read, and the project path never writes the catalog.** The project frame source and Prepare run against a `frames` table that holds rows with the handles' ids and different data, and none of it is read. Solve never writes `plate_solves` or `frames`.
   Pinned in:
   - Task 6a: `the_project_frame_source_reads_the_manifest_never_the_catalog`;
   - Task 9b: `prepare_never_reads_the_catalog_for_a_project`;
   - Task 10b: `the_solve_sub_step_never_writes_plate_solves_or_frames`.
3. **The replica is sacred.**
   - Prepare leaves every landed file byte-identical.
   - No prepared file carries `ATH_PRJ`, `CALSTAT`, `ATH_CSRC` or `ATH_FILT`, and the scanner never catalogs one.
   - A `calibrated` artifact in place survives `CleanupWhat::All`.
   - A project working root that overlaps a scan root is a `folders` blocker.
   Pinned in:
   - Task 5: `_projects` and the overlap helper;
   - Task 7: the `folders` blocker;
   - Task 9a: the card, scanner and `prepared/` cleanup tests;
   - Task 9b: the landed-bytes test, and `CleanupWhat::All` leaving an in-place landed file on disk.
4. **A landed file that changes during a run never feeds a stage.** The identity check runs at Prepare, Solve, Measure, Register candidates and the Output member snapshot. A mismatch excludes the frame with "changed during the run". A changed reference fails the run.
   Pinned in:
   - Task 6b: the `landed_unchanged` unit test, the Measure boundary and `exclude_changed_members`;
   - Task 9b: the Prepare boundary;
   - Task 10b: the Solve boundary.
5. **The grid hash is one per grid.**
   - The grid is normalized before the PUT.
   - A jsonb echo with reordered keys, an integer-spelled float or `-0.0` hashes the same.
   - The local copy is written only after a 2xx.
   - A no-op echo emits nothing; the saving device gets exactly one event, from the command (review I5).
   - The proposal is deterministic for a manifest.

   Pinned in Task 8a (the hash, normalization and the proposal) and Task 8b (the 2xx write, the echo, the events).
6. **Exposure-class tokens can never collide** on `stacking_run_groups`' `UNIQUE(run_id, group_key)`. That includes two anchors that `fmt_num`'s two decimals would merge, and a dense sweep at the `stopsWidth` floor.

   Pinned in Task 3.

## Plan rulings (refine the spec where the code forced a choice)

The controller's skeleton (C1–C17) is binding. The reasons are recorded here; where this plan had to refine a ruling, the refinement is marked **Refinement** and repeated in "Writer's notes for the controller" at the end. The controller's rulings on the plan review are folded in and marked **Refinement (review)**, with the review's finding number (I1–I12, M1–M21).

- **C1 — The wire change comes first** (commands-e2e §2).
  - `StackingProgressEvent`, `StackingCompleteEvent`, `RunSummary`, `StackingRunRow` and `StackingPlan` each carry `set_id: Option<i64>` and `project_id: Option<String>`; exactly one is set. `RunSummary.project_id` is `#[serde(default)]`, because summaries are read back from `summary_json`.
  - `wire_subject(&StackSubject) -> (Option<i64>, Option<String>)` replaces `wire_set_id` at every call site, `FanOutTicker` and the integrate/drizzle tick helpers included.
  - `StackingRunRow` loses its `#[serde(skip)]` workaround: its wire `framesSetId` becomes `number | null` and `projectId` is added.
  - `useStackingRuns.ts` gets the minimal const-bound guard (`const setId = payload.setId; if (setId == null) return;`), which keeps `tsc` and vitest green until 6c.
  - *Reason:* every later task's project events would otherwise read `setId: 0` and need re-asserting. The change is self-contained.
- **C2 — The four config fields land in one commit.**
  - `grouping.exposureMode`: `seconds | stops | equivalent | none`, default `seconds`.
  - `grouping.stopsWidth`: default 1.0, clamped by `resolve_config` to `MIN_STOPS_WIDTH = 0.1` (**refinement:** the value of the "positive floor").
  - `output.coverageMap`: default false.
  - `output.cropToCoverage`: default 0.
  - All four are `#[serde(default)]`, with no `STACKING_CONFIG_VERSION` bump. `config_hash_is_canonical_and_pinned` is re-pinned with a "Changed here" paragraph. `stage_hashes_are_pinned_to_golden_literals` keeps its exact literals while the new fields carry non-default values.
  - *Reason:* `config_hash` moves once instead of twice, and the golden pin proves no stage hash moves.
- **C3 — Exposure classes** (spec §8).
  - Classing is pure.
    - `seconds` is today's clustering, byte for byte.
    - `stops` starts a class at the shortest remaining exposure `t0` and takes every exposure `≤ t0·2^W`.
    - `equivalent` does the same on `t/N²` with `N = FOCALLEN/APTDIA`. Frames whose N is unknown are classed by seconds, and the plan warns.
    - `none` puts every exposure of one bucket into a single class, `all`.
  - Tokens are `<t>s`, `st<t0>`, `eq<e0>`, `all` and `unknown`. A `st`/`eq` number is printed with four significant digits, trailing zeros trimmed. With the `stopsWidth` floor, two anchors of one bucket differ by more than ×1.07, and four significant digits move a value by at most 0.05 %, so two classes can never print the same token. A test pins two anchors that `fmt_num` merges.
  - `stacking_run_groups.exposure_class` is written (`NewGroup`, `insert_group`, `GROUP_COLUMNS`, `row_to_group`) and exported on `StackingRunGroupRow` and `PlanGroup`.
  - The four seconds consumers keep seconds: the EXPTIME warning, `PlanGroup.exposure_s`, `stacking_run_groups.exposure` and `master_file_name`. A class's `exposure_s` is the shortest member's exposure, never an `eq` anchor.
  - **Refinement:** frames without `EXPTIME` keep their own `unknown` class in every mode. So `none` means one class per colour × filter among frames with a known exposure, and the EXPTIME warning, keyed on `exposure_s.is_none()`, stays byte-identical.
- **C4 — The manifest meta parser.**
  - A typed `ProjectFrameMeta` is parsed once from `manifest_json`. `exptimeSec` and `dateObs` are top-level; `naxis1`, `naxis2`, `pixelScaleArcsec`, `wcs`, `bayerpat`, `focalLen`, `instrume` and `aperture` live under `meta`.
  - `meta.wcs` converts to a `PlateSolveRecord` with `crpix − 1`.
  - Publication adds `meta.aperture` (APTDIA from the stored header blob). `meta` is frozen at announce, so older frames lack it and fall back to seconds.
- **C5 — The stackable predicate** is `exportable ∧ local_state.servable() ∧ ¬own_staged`.
  - `own_staged` is read into `LocalFrameRow`. Its reason is "being re-staged": the row's `blake3` is the old version's while the bytes are new.
  - **Refinement:** a row with a NULL `size_mtime_seen` is not stackable either ("waiting for its new version"). A version bump clears the stamp, so the landed file is not this version, and the identity check (C9) would have no baseline.
  - **Refinement (review I3):** a non-NULL stamp that does not parse is corruption, not waiting: the frame is left out as "unreadable size:mtime stamp", with a `warn!` naming the stamp, and gets no handle.
- **C6 — The working layout is `<working>/_projects/<slug>`.**
  - No frame-set slug can start with `_`, because `sanitize_for_filename` trims it. So a frame set can never share a folder with a project.
  - A `folders` blocker fires when the project's working root overlaps any scan root, the Collaboration root included (it is a scan root). It reuses `check_scan_root_overlap`.
  - The two tests that pin `projects/` are updated.
- **C7 — `FrameOrigin`.**
  - It becomes `Catalog { file_id, attested: bool }` or `Project { project_id, frame_uuid, publisher, landed_path, blake3, byte_size, size_mtime, meta: ProjectFrameMeta }`.
  - Every `match` the compiler flags gets its project arm.
  - `publisher` is the publisher's display name.
  - The name collides with `db::collab_frames::FrameOrigin {Own, Replica}`: never import both unqualified in one file, and each gets a doc line pointing at the other.
- **C8 — The funnels grep test.**
  - No variant path (`StackSubject::…`, `FrameOrigin::…`) appears in the production half of `stacking/run.rs` or `stacking/plan.rs` outside the named funnels.
  - The funnels are `frame_source`, `subject_layout`, `subject_label`, `stored_reference_frame`, `hash_by_origin`, `stored_solve`, `cfa_geometry`, `resolve_reference`, `resolve_calibrate_job`, `stored_registrations`, `store_registration`, `stage_one_kind` and `landed_unchanged`.
  - Allowlisted, each with a reason: `heal_interrupted_runs`, which is log-only; `start_stacking`, the frame-set-only guard until 6b-3; `build_plan`, the subject dispatch to the two plan arms; and the `#[cfg(test)]` constructors `test_context` and `project_test_context`.
  - The test lands in Task 6a; `landed_unchanged` joins its `FUNNELS` list in Task 6b, with the function.
  - `origin.rs`, which the test does not scan, holds the per-frame dispatch points the run calls without naming a variant: `stored_solve`, `cfa_geometry`, `landed_unchanged`, `needs_prepare`, `prepare_input`, `solve_plan`, `wcs_tier` and `local_solve_outcome` (review M18: listed in the README too).
  - `last_run_reference_scale` and `cleanup_stale_note` take `&StackSubject`.
- **C9 — The landed-file identity check** is the funnel `origin::landed_unchanged(origin) -> Result<(), ExclusionReason>`, built on `collab::storage::sweep::{Stamp, stat_verdict}` (`"<len>:<mtime_secs>"`, 2 s tolerance). A mismatch excludes the frame with "changed during the run".
  - It is called at five stage boundaries: Prepare (Task 9b), Solve (Task 10b), Measure (~`run.rs:2983`), Register candidates (~4317) and the member snapshot in `process_group_output` (~6824).
  - Task 6b adds the funnel and the three existing-stage calls. The Prepare and Solve calls land with their stages.
  - **Refinement:** a changed group reference fails the run with "the reference frame changed during the run", because a reference cannot be excluded from its own group.
- **C10 — `build_plan`'s project arm writes.**
  - It mints handles (`project_frame_handles`, one IMMEDIATE transaction) and seeds the config document (`seed_project_config`, one IMMEDIATE transaction). This is a documented exception to "the plan reads only". The frame-set arm stays read-only.
  - **Refinement:** these are two atomic writes, not one transaction, because `start_stacking` mints through the same frame source.
  - `StackingPlan.readiness` stays non-optional: `ExportReadiness` derives `Default` and a project sends it empty.
  - The project blockers, in order, are `grid` (no confirmed grid), `reference`, `folders` (incl. the C6 overlap), `space` (grid-size registered artifacts plus the prepared conversions), `frames` (nothing stackable, or the ≥ 3 rule) and `unsupported` (the shared drizzle-scale check).
  - The warnings follow spec §7. Unsupported FILES are a plan warning, and Prepare excludes them per frame (C12).
  - **Refinement (review I1):** unsupported files are also plan-time exclusions. The shared core counts their handles out of every group's included frames and the ≥ 3 rule (`excluded_set ∪ unsupported_ids`), so a group Prepare would leave under three frames is blocked by `frames` at plan time instead of failing at run time. They are not added to `StackingPlan.excluded_frame_ids`, which stays the manual list, and no new blocker code exists.
- **C11 — R3's stacking half.**
  - The frame source sets `Catalog.attested` from `frames_set.calibrated_externally`.
  - An attested frame runs Prepare, not Calibrate. `HashMemo` gives it the Prepare hash over its catalog identity, and `resolve_calibrate_job` refuses it.
  - The plan gate drops `masters | links | masterFiles` and `masters_to_build` is empty for an attested set (Task 7). The attested hash is Task 9a; the run half is Task 9b.
- **C12 — Prepare.**
  - **In place** only when the file is a primary-HDU image with BITPIX −32 or −64 that the plane reader decodes, and it is mono, three-plane colour, or a CFA mosaic with `debayerOsc` off.
  - **Converted** to float32 FITS `prepared/<group>/p_<stem>.fits`:
    - integer BITPIX ÷ `scale_divisor_for_bitpix`;
    - XISF through `read_raw` ÷ 65535, within 1 ulp.
  - **Excluded** with "unsupported file": a tile-compressed FITS, an extension image or a distributed XISF.
  - **Refinement (review I2):** spec §5.1 row 1 lists BITPIX 8/16/32 as usable in place; this plan narrows it to float. An integer FITS is converted, so an attested set's 16-bit lights and a published replica share the calibrated-light generator's unit domain, and every stage after stage 1 sees one value domain.
  - **Refinement (review I10):** a file that cannot be opened is excluded "unsupported file: cannot open the file: …", and the decoder's fallback reason reads "not a 1- or 3-plane 8/16/32-bit or float primary image" (it also covers BITPIX 64 and 4-plane cubes).
  - **Refinement (review I12):** Prepare excludes a frame whose post-Prepare plane count contradicts its group's colour (`expected_stage_one_planes`), with "plane count {n} disagrees with the group's colour mode". The group's colour comes from the manifest (or the catalog), the debayer from the file's own header, and nothing else reconciles the two.
  - **Debayer** through `debayer_plane` and `debayered_cards`, extracted from the generator, which then calls them (pixels pinned byte-identical). `debayer_file(src, dest, CfaGeometry, …)` writes `p_<stem>_d.fits`.
  - **Output cards:** `REGISTERED_COPY_THROUGH`, the CFA cards when the output is still a mosaic, `IMAGETYP`, `ATH_STK = T`, `ATH_STKV` and `ATH_PREP`. Never `ATH_PRJ`.
  - **Hash:** `prepare_subtree = {debayerOsc, debayerMethod: "VNG", prepareVersion}`. The project hash covers `blake3`, the byte size and the subtree; the attested-catalog hash covers the catalog identity and the subtree.
  - **Artifacts:** `calibrated` points at the in-place or prepared file. `calibrated_mosaic` points at the pre-debayer file when Bayer drizzle wants one.
  - **CFA geometry** comes from the file's own header (`cfa_geometry_of_file`).
  - **Layout:** `WorkingLayout::prepared_root()`. Work usage counts it in `calibrated_bytes`, the Calibrate slot's counter, so the TS does not change. Cleanup removes it under `Intermediates` and `All`.
  - **Refinement:** `prepare_subtree` lands in Task 6a, because `HashMemo`'s project arm compiles there. The file classifier (`probe_file`, `prepare_action`) lands in Task 7, because the plan's `space` estimate and its unsupported-file warning need it. The writers, the debayer extraction and the attested hash are Task 9a; the stage is Task 9b.
- **C13 — The OSC plane rule.**
  - A project group never mixes three-plane and mosaic frames. Its colour token is `osc` for a frame that is three-plane after Prepare, and `cfa` for one that stays a mosaic (`debayerOsc` off and the file is a one-plane mosaic).
  - With the default `debayerOsc = true`, every OSC frame is `osc`.
  - The plane count is probed from the file only when `debayerOsc` is off.
  - **Refinement (review I12):** Prepare enforces the rule per frame (C12): one plane for mono; three for OSC with the debayer on; with it off, three for a project's `osc` group and one otherwise.
- **C14 — Solve.**
  - The acceptance gate is extracted from `store_result` into the pure `acceptance_refusal`, which `store_result` still calls. `record_from_result` is extracted too.
  - Catalog opening moves into core (`plate_solve::open_catalog_layers`), and both hosts call it.
  - `solver_config_hash` and `catalog_identity` are defined.
  - A `FrameSolver` trait object on `RunContext` defaults to `CatalogSolver`. Tests inject a fake that solves from a known WCS and counts its calls.
  - The Solve sub-step runs at the start of Measure. It covers project frames without a `meta.wcs`, solves the prepared file with hints from the manifest scale and the project target, and calls `solve_frame_with_hints`, never `solve_frame` or `store_result`. The gate uses the manifest scale.
  - Every outcome is cached as a `solve` artifact under `group_key = ""`: solved, failed or refused, keyed on `blake3`, the solver config hash and the catalog identity. `solve` stays out of `INTERMEDIATE_ARTIFACT_KINDS`.
  - `stored_solve`'s project arm reads the manifest WCS first, then the solve artifact.
  - A real-data probe reports the success rate on landed `.dev` replicas.
  - **Refinement (review I7):** the `solve` payload also records the app version that wrote it. A cached failure or refusal written by another app version is a miss and is tried once more; a cached solve stays. `SOLVE_RECIPE_VERSION`'s doc and the README carry the bump rule: bump it to re-solve frames cached as solved.
  - **Refinement (review I8, M16):** the probe samples mono and OSC frames separately (up to `limit / 2` each) and prints both sub-rates; a probe that solves nothing in either domain fails.
  - **Refinement (review I9, M11, M12):** a frame without a stage-1 file is skipped with a `debug!` and left to Measure; the FOV hint is the scale times `naxis1`, as the hosts compute it; `SolveRequest` carries the handle, which names the solve's span.
  - The gate extraction and the catalog opener are Task 10a; the seam and the sub-step Task 10b; the probe Task 10c.
- **C15 — The reference rule** (spec §5.4).
  - Candidates must have a WCS. Manifest-WCS frames qualify run-wide; locally solved ones qualify only when no frame of the run has a manifest WCS. With no candidate, the stage fails: "no frame of this project could be plate-solved; the grid needs one".
  - Auto takes the largest group that holds a candidate.
  - A native-mode group without a candidate falls back to the run reference.
  - The two-pass re-pick never lets a non-candidate win (`two_pass_pick_among`).
  - A manual pin without a WCS fails the stage.
  - **Refinement (review I6):** the manual pin's WCS check runs after the existing "the manual reference frame is excluded: {reason}" check, so a pin that stage 1 excluded keeps its real reason.
  - It lands in Task 10c.
- **C16 — The grid** (grid-geometry §1, §4–§5).
  - **Type:** `CanonicalGrid { scaleArcsec, widthPx, heightPx, crval1, crval2, rotationDeg }`, normalized before the PUT: `−0.0` becomes `0.0`, the rotation goes into `(−180, 180]` and `crval1` into `[0, 360)`.
  - **Hash:** the XXH3-64 of the typed struct's canonical JSON (sorted keys), as 16 hex digits. The hub's jsonb echo is never hashed.
  - **Client:** `CollabClient::put_grid` (204). `set_collab_project_grid` echoes its own normalized grid.
  - **Storage:** `ProjectWire.canonical_grid` carries the hub's copy. `collab_projects.canonical_grid_json` has a dedicated setter and getter. The refresh reports `grid_moved`.
  - **Feed:** `Kind::Grid` triggers the refresh. Every live-feed refresh site emits `collab-grid-changed { projectId }`.
  - **Refinement (review I5):** `set_collab_project_grid` takes the host's emitter (Tauri's emitter; web's `SseProgressEmitter::new(state.event_tx.clone())`) and emits `collab-grid-changed { projectId }` after the successful cache write, on every successful save. The saving device's own cache write makes its feed refresh a no-op, so it gets the event from the command; every other device gets it from the feed.
  - **Refinement (review I4):** `card_from_row` degrades an unreadable cached grid to none with a `warn!`, so one bad row cannot take the project list down; `get_collab_grid_proposal` and the plan stay strict.
  - **Refinement (review M7):** a hub 400 reaches the user as the hub client's wrapped message (`hub returned 400 Bad Request (put project grid): …`). Local validation mirrors the hub's text, so the wrapper shows only when the two disagree.
  - **Fake hub:** the PUT grid route, `canonicalGrid` on the page, and a settable target.
  - **Orientation:** the grid is sky-standard (north up, east left, det(CD) < 0). Every solve in the owner's catalog has det(CD) > 0, so a grid master is the mirror image of a raw frame, which is correct on the sky. 6b-3 must not copy the reference's `ROWORDER` onto a grid master.
  - **Proposal:** deterministic for a manifest.
    - Scale is the median `pixelScaleArcsec`, rounded to 0.01″. Centre is the target; rotation is 0.
    - Size is the bounding box, in the proposed TAN, of the WCS frames' footprints, clipped to the target circle, rounded up to a multiple of 64 and clamped to `[64, 65536]`. With no WCS frame, it is a square of twice the target radius.
    - `framesInside` counts the frames at least half inside.
  - **Commands:** `get_collab_grid_proposal { projectId } -> { current, proposal, framesInside, canEdit }` (`proposal` is `null` when no accepted frame has a pixel scale, and also when the median scale rounds outside the hub's `(0, 100]`, review M8), and `set_collab_project_grid { projectId, grid } -> grid`, on both hosts.
  - The type, hash, proposal geometry and cache columns are Task 8a; the hub client, refresh, feed, fake hub and commands Task 8b.
- **C17 — Command count.** 6b-2 adds 2 commands (297 → 299). 6b-3 adds 7 (→ 306; the spec's "305" is stale, because 6a added 3). CLAUDE.md is updated by 6b-3. Task 11 records the running count.

## File map

| File | Change |
| ---- | ---- |
| `crates/athenaeum-core/src/stacking/run.rs` | `wire_subject` (T1); `exposure_class` insert (T3); `resolve_calibrate_job`'s project arm, `test_context`'s origin (T6a); `changed_during_run`, `exclude_changed_members`, boundary calls, `project_test_context` (T6b); `stage_one_kind`, `stage_prepare`, `expected_stage_one_planes` (T9b); `solver`, `solve_project_frames` (T10b); `wcs_rule`, `wcs_tiers`, the reference rule (T10c) |
| `crates/athenaeum-core/src/stacking/provenance.rs` | `RunSummary` subject pair (T1) |
| `crates/athenaeum-core/src/db/stacking.rs` | `StackingRunRow` pair (T1); `exposure_class` (T3); project config CRUD + seed (T5); `project_frame_handles` returns at once for an empty list (T6a) |
| `crates/athenaeum-core/src/stacking/plan.rs` | `StackingPlan` pair (T1); golden extension (T2); equivalence warning, `PlanGroup.exposure_class` (T3); `resolve_project_config_and_exclusions` (T5); `HashMemo` arms, `prepare_hash_for_project`, subject-taking helpers (T6a); the two arms, `PlanGrid`, `PlanProjectFrame`, R3 gate, unsupported files out of the counts (T7); attested hash (T9a); solved-locally refinement (T10b) |
| `crates/athenaeum-core/src/stacking/config.rs` | `ExposureMode`, `stops_width`, `MIN_STOPS_WIDTH`, output fields, re-pin (T2); `project_seed_config` (T5); `prepare_subtree`, `PREPARE_VERSION` (T6a) |
| `crates/athenaeum-core/src/stacking/groups.rs` | classifier, tokens, `f_ratio`, `load_apertures` (T3); `frame_source(&StackingConfig)` with the project arm and `attested` (T6a) |
| `crates/athenaeum-core/src/fits_parser/stored_header.rs` | `aperture_mm_from_stored_header` (T3) |
| `crates/athenaeum-core/src/collab/frame_meta.rs` | `ProjectFrameMeta`, `ManifestWcs`, parser, `to_record`, `meta.aperture` (T4) |
| `crates/athenaeum-core/src/db/collab_frames.rs` | `own_staged`, `part_of_project`, `exportable`, `stackable`, `NotStackable` (T5) |
| `crates/athenaeum-core/src/export/project_collector.rs` | calls the moved `exportable` (T5) |
| `crates/athenaeum-core/src/stacking/paths.rs` | `PROJECTS_DIR`, `project_root_overlap` (T5); `estimate_project_bytes` (T7); `prepared_*`, usage, cleanup (T9a) |
| `crates/athenaeum-core/src/stacking/origin.rs` | the `Project` variant, `attested`, `cfa_geometry_of_file`, grep test (T6a); `landed_unchanged` (T6b); `needs_prepare`, `prepare_input` (T9b); `SolvePlan`, `local_solve_outcome`, solve-artifact arm (T10b); `WcsTier`, `wcs_tier` (T10c) |
| `crates/athenaeum-core/src/stacking/project_source.rs` | **new** — the project frame source (T6a) |
| `crates/athenaeum-core/src/stacking/prepare.rs` | **new** — classifier (T7); cards, converter, `debayer_file`, jobs (T9a) |
| `crates/athenaeum-core/src/stacking/solve.rs` | **new** — `FrameSolver`, `CatalogSolver`, `SolveOutcome`, `cached_outcome_holds` (T10b); probe (T10c) |
| `crates/athenaeum-core/src/stacking/test_fixtures.rs` | project frame fixture (T6a); `write_fits_i16` `pub(crate)`, `write_empty_primary_fits`, `ProjectFile::EmptyPrimary` (T7) |
| `crates/athenaeum-core/src/stacking/weights.rs` | `two_pass_pick_among` (T10c) |
| `crates/athenaeum-core/src/stacking/mod.rs` | `pub mod project_source;` (T6a), `pub mod prepare;` (T7), `pub mod solve;` (T10b) |
| `crates/athenaeum-core/src/export/calibrated_generator.rs` | `debayer_plane`, `debayered_cards` (T9a) |
| `crates/athenaeum-core/src/collab/grid.rs` | **new**, ungated — `CanonicalGrid` (T8a) |
| `crates/athenaeum-core/src/collab/grid_geometry.rs` | **new**, gated — proposal math (T8a) |
| `crates/athenaeum-core/src/collab/mod.rs`, `hub_client.rs`, `fake_hub.rs` | grid module decls (T8a); `ProjectWire.canonical_grid`, `put_grid`, fake route (T8b) |
| `crates/athenaeum-core/src/db/collab.rs` | `set_canonical_grid`, `canonical_grid` (T8a) |
| `crates/athenaeum-core/src/api/collab.rs` | `GridEcho`, refresh, `ProjectCard.canonical_grid` (degrades on an unreadable cache), `client_err` visibility (T8b) |
| `crates/athenaeum-core/src/api/collab_grid.rs` | **new** — event, `GridProposal`, the two commands' core; the save emits (T8b) |
| `crates/athenaeum-core/src/api/collab_live/feed.rs` | `Kind::Grid` consumer, `emit_grid_moves` (T8b) |
| `crates/athenaeum-core/src/api/lights.rs` | `ExportReadiness: Default` (T7) |
| `crates/athenaeum-core/src/plate_solve/{service,config,layers,mod}.rs` | `acceptance_refusal`, `record_from_result`, `solver_config_hash`, catalog opener (T10a) |
| `crates/athenaeum-core/src/scanner/mod.rs` | test: a prepared file is never cataloged (T9a) |
| `crates/athenaeum-core/src/ts_export.rs` | `ExposureMode` (T2); `PlanGrid`, `GridState`, `PlanProjectFrame`, import line (T7); `CanonicalGrid`, `GridProposal`, `CollabGridChanged` (T8b) |
| `crates/athenaeum-tauri/src/commands/{collab,plate_solve}.rs`, `lib.rs` | grid wrappers (T8b); core catalog opener (T10a) |
| `crates/athenaeum-web/src/routes/{collab,plate_solve,mod}.rs` | grid routes (T8b); core catalog opener (T10a) |
| `src/hooks/useStackingRuns.ts` (+ `.test.tsx` new) | the guard (T1) |
| `src/components/collab/project/testFixtures.ts`, `src/hooks/useCollabNotifications.test.tsx` | `canonicalGrid: null` (T8b) |
| `src/types/{stacking,models}.ts` | regenerated (T1, T2, T3, T7, T8b) |
| `docs/stacking/README.md`, `docs/transfers/README.md`, `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, `docs/superpowers/open-items.md` | T11 |

---

### Task 0: Baseline

**Files:** none.

- [ ] **Step 1: Confirm the branch and a clean tree**

Run: `git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum status -sb && git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum log -1 --format=%h`
Expected: `## collab-wave6b2` and `e03bc48f`, or the controller's plan commit on top of it. The only untracked files may be research/skeleton docs; there are no code changes.

- [ ] **Step 2: Record the baseline gates.** Run the Gate per task commands above, one at a time, from the repo root. Expected: all green, and `git diff --stat src/types` empty. Record every `test result` line; Task 11 compares against them. A test that is red here is pre-existing: note its name and do not fix it in this plan.

- [ ] **Step 3: Record the strict command count.** Run:
```bash
grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l
```
Expected: `297`.

---

### Task 1: The wire subject (C1, spec §9.2)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/run.rs` at these sites:
  - the structs, `~138-180`;
  - `wire_set_id`, `~525-530`;
  - `RunContext::progress`, `~460`;
  - `start_stacking`'s `RunSummary`, `~916`;
  - `run_thread`, at `~1036`, `~1163` and `~1199`;
  - `FanOutTicker`, `~2740-2860`;
  - the registration `to_record` call, `~4817`;
  - `emit_integrate_tick`, `~6536`, and its callers, `~7427-7550`;
  - `emit_drizzle_tick`, `~6690`, and its caller, `~8128`;
  - the LN ticks, `~9446` and `~9572`;
  - `test_context`, `~9888`;
  - the test `fan_out_ticker_is_monotonic_continues_and_always_lands_on_total`, `~14163`;
  - the test `cancel_before_admission_finishes_cancelled`, `~10240-10305`.
- Modify `crates/athenaeum-core/src/stacking/provenance.rs`: `RunSummary`, `~221`.
- Modify `crates/athenaeum-core/src/db/stacking.rs`: `StackingRunRow` and `row_to_run`, `~96-148`, and the test at `~1786`.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `StackingPlan`, `~232`, and its literal, `~2428`.
- Modify `src/hooks/useStackingRuns.ts`.
- Create `src/hooks/useStackingRuns.test.tsx`.
- Regenerate `src/types/stacking.ts`.

**Interfaces:**
```rust
// stacking/run.rs — replaces `wire_set_id`
pub(crate) fn wire_subject(subject: &StackSubject) -> (Option<i64>, Option<String>);
pub struct StackingProgressEvent { pub run_id: i64, pub set_id: Option<i64>, pub project_id: Option<String>, pub stage: Stage, /* rest unchanged */ }
pub struct StackingCompleteEvent { pub run_id: i64, pub set_id: Option<i64>, pub project_id: Option<String>, /* rest unchanged */ }
impl FanOutTicker { fn from_parts(emitter: Arc<dyn ProgressEmitter>, run_id: i64, subject: &StackSubject, stage: Stage, group_key: Option<String>, done_so_far: usize, total: usize, bytes_done: u64, bytes_total: u64) -> Self; }
fn emit_integrate_tick(state: &Mutex<IntegrateTickState>, emitter: &dyn ProgressEmitter, run_id: i64, subject: &StackSubject, group_key: &str, channels: usize, plane: usize, frac: f64, bytes_done: u64, bytes_total: u64, force: bool, message: Option<String>);
fn emit_drizzle_tick(state: &Mutex<DrizzleTickState>, emitter: &dyn ProgressEmitter, run_id: i64, subject: &StackSubject, group_key: &str, current: usize, total: usize, frame_id: Option<i64>, force: bool);
// stacking/provenance.rs
pub struct RunSummary { pub run_id: i64, pub set_id: Option<i64>, #[serde(default)] pub project_id: Option<String>, /* rest unchanged */ }
// db/stacking.rs
pub struct StackingRunRow { pub id: i64, pub frames_set_id: Option<i64>, pub project_id: Option<String>, /* rest unchanged */ }
// stacking/plan.rs
pub struct StackingPlan { pub set_id: Option<i64>, pub project_id: Option<String>, /* rest unchanged */ }
```
TS (`src/types/stacking.ts`, regenerated; these are the only moves):
- `StackingProgressEvent` and `StackingCompleteEvent`: `setId: number | null, projectId: string | null`;
- `RunSummary`: `setId: number | null, projectId: string | null`;
- `StackingPlan`: `setId: number | null, projectId: string | null`;
- `StackingRunRow`: `framesSetId: number | null, projectId: string | null`.

- [ ] **Step 1: Write the failing Rust tests**

In `run.rs`'s `mod tests`, next to `fan_out_ticker_is_monotonic_continues_and_always_lands_on_total`:
```rust
    /// Plan 6b-2 C1: one convention on every wire type — exactly one side of
    /// the subject pair is set.
    #[test]
    fn wire_subject_sets_exactly_one_side() {
        assert_eq!(wire_subject(&StackSubject::FrameSet(7)), (Some(7), None));
        assert_eq!(
            wire_subject(&StackSubject::Project("p1".into())),
            (None, Some("p1".to_string()))
        );
    }

    /// Plan 6b-2 C1: a frame-set tick still carries its set id (and a null
    /// project id); a project tick carries the project id and a null set id.
    #[test]
    fn a_tick_carries_the_subject_pair() {
        for (subject, set_id, project_id) in [
            (
                StackSubject::FrameSet(7),
                serde_json::json!(7),
                serde_json::Value::Null,
            ),
            (
                StackSubject::Project("p1".into()),
                serde_json::Value::Null,
                serde_json::json!("p1"),
            ),
        ] {
            let rec = Arc::new(Recording::new());
            let ticker = FanOutTicker::from_parts(
                rec.clone(),
                1,
                &subject,
                Stage::Measure,
                None,
                0,
                1,
                0,
                0,
            );
            ticker.tick(Some(3));
            let last = rec
                .events(STACKING_PROGRESS_EVENT)
                .pop()
                .expect("the first tick is always emitted");
            assert_eq!(last["setId"], set_id, "{last}");
            assert_eq!(last["projectId"], project_id, "{last}");
        }
    }
```
In `cancel_before_admission_finishes_cancelled`, after `assert_eq!(completes[0]["success"].as_bool(), Some(false));`, add:
```rust
        assert_eq!(completes[0]["setId"], serde_json::json!(fixture.set_id));
        assert!(completes[0]["projectId"].is_null(), "{:?}", completes[0]);
```
In `provenance.rs` (create `#[cfg(test)] mod tests { use super::*; … }` if the file has none):
```rust
    /// Plan 6b-2 C1: a `summary_json` written before 6b-2 (`"setId": 5`, no
    /// `projectId`) still decodes, with no project.
    #[test]
    fn a_summary_written_before_6b2_still_decodes() {
        let config = StackingConfig::default();
        let mode = config.reference.mode.clone();
        let scale_estimator = config.normalization.scale_estimator.clone();
        let summary = RunSummary {
            run_id: 1,
            set_id: Some(5),
            project_id: None,
            set_name: "LDN 1272".into(),
            app_version: "test".into(),
            started_at: "2026-10-03T00:00:00Z".into(),
            finished_at: None,
            status: "done".into(),
            config_hash: "h".into(),
            reference: SummaryReference {
                frame_id: None,
                filename: None,
                mode,
                weight: None,
                switched_from: None,
            },
            measurement: SummaryMeasurement {
                seed_source: "fast".into(),
                scale_estimator,
            },
            config,
            groups: Vec::new(),
            masters_built: Vec::new(),
            stages: Vec::new(),
            warnings: Vec::new(),
            error: None,
        };
        let mut v = serde_json::to_value(&summary).unwrap();
        v.as_object_mut().unwrap().remove("projectId");
        assert_eq!(v["setId"], serde_json::json!(5));
        let back: RunSummary = serde_json::from_value(v).unwrap();
        assert_eq!(back.set_id, Some(5));
        assert_eq!(back.project_id, None);
    }
```
In `db/stacking.rs` at `~1786`, replace `assert_eq!(row.frames_set_id, 0, "the wire value of a project run");` with:
```rust
        assert_eq!(row.frames_set_id, None, "a project run has no set id on the wire");
        assert_eq!(row.project_id.as_deref(), Some("p1"));
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 wire_subject a_tick_carries a_summary_written_before`

Expected: the build fails. `wire_subject` does not exist, `from_parts` takes an `i64`, and `RunSummary` has no `project_id`.

- [ ] **Step 3: Implement**

1. In `run.rs`, replace `wire_set_id` (`~525-530`) with:
```rust
/// The subject pair every wire payload carries — `setId` for a frame set,
/// `projectId` for a project, never both (plan 6b-2 C1). The frontend keys
/// its maps by `set:<id>` / `project:<uuid>` (spec §9.2, wave 6c).
pub(crate) fn wire_subject(subject: &StackSubject) -> (Option<i64>, Option<String>) {
    (
        subject.frame_set_id(),
        subject.project_id().map(str::to_string),
    )
}
```
2. Change the event structs. Field comments use `//` (ts-rs):
```rust
pub struct StackingProgressEvent {
    pub run_id: i64,
    // Exactly one of `set_id` / `project_id` is set (plan 6b-2 C1).
    pub set_id: Option<i64>,
    pub project_id: Option<String>,
    pub stage: Stage,
    // … every other field unchanged …
}
```
   Make the same change to `StackingCompleteEvent`.
3. Update every construction site:
   - **`RunContext::progress`:**
     ```rust
     let (set_id, project_id) = wire_subject(&self.subject);
     ```
     Then use `set_id, project_id,` in the literal.
   - **`run_thread`:** replace `let set_id = wire_set_id(&rc.subject);` with:
     ```rust
     let (set_id, project_id) = wire_subject(&rc.subject);
     ```
     Change the info line to:
     ```rust
     tracing::info!(run_id, set_id, project_id = project_id.as_deref(), duration_ms, outcome = status, "stacking run finished");
     ```
     `tracing` records `Option<i64>` exactly as the old `i64` when it is `Some`, and skips a `None`. The complete event takes `set_id, project_id: project_id.clone(),`.
   - **`start_stacking`'s `RunSummary`:** use `let (set_id, project_id) = wire_subject(&subject);`.
   - **`test_context`'s summary:** `set_id: Some(set_id), project_id: None`.
   - **The LN ticks (`~9446` and `~9572`):** bind `let (set_id, project_id) = wire_subject(&rc.subject);` and `let (build_set_id, build_project_id) = wire_subject(&rc.subject);`. The closures capture them and clone `project_id` into each event; `Option<String>::clone` is cheap.
4. `FanOutTicker`:
   - Replace the field `wire_set_id: i64` with `subject: StackSubject`.
   - In `new`, pass `&rc.subject`.
   - In `from_parts`, take `subject: &StackSubject` and store `subject.clone()`.
   - In `tick`, do `let (set_id, project_id) = wire_subject(&self.subject);` and fill the event.
   - Update the test caller at `~14163` mechanically: `1` becomes `&StackSubject::FrameSet(1)`.
5. `emit_integrate_tick` and `emit_drizzle_tick`:
   - Replace the `set_id: i64` parameter with `subject: &StackSubject`, and inside each do `let (set_id, project_id) = wire_subject(subject);`.
   - At the callers, replace `let set_id = wire_set_id(&rc.subject);` with `let tick_subject = rc.subject.clone();`, and pass `&tick_subject` where `set_id` went. The closures borrow it.
   - For the drizzle caller, replace `drizzle_tick_set_id` with `let drizzle_tick_subject = rc.subject.clone();`.
6. At `~4817`, the registration record's set id is a placeholder that `store_registration` overwrites (B10); keep it numeric:
```rust
                        let rec = to_record(
                            subject.frame_set_id().unwrap_or(0),
```
7. `provenance.rs` `RunSummary`:
```rust
    pub run_id: i64,
    // Exactly one of `set_id` / `project_id` is set (plan 6b-2 C1). A summary
    // written before 6b-2 has a number here and no `projectId`.
    pub set_id: Option<i64>,
    #[serde(default)]
    pub project_id: Option<String>,
```
8. `db/stacking.rs` `StackingRunRow`. Delete the `#[serde(skip)] #[ts(skip)]` attributes and the `///` doc on `project_id`:
```rust
    pub id: i64,
    // Exactly one of `frames_set_id` / `project_id` is set (the table's CHECK).
    pub frames_set_id: Option<i64>,
    pub project_id: Option<String>,
```
   `row_to_run` sets `frames_set_id: r.get(1)?,`, and `subject()` becomes:
```rust
    pub fn subject(&self) -> StackSubject {
        match (&self.project_id, self.frames_set_id) {
            (Some(id), _) => StackSubject::Project(id.clone()),
            (None, Some(id)) => StackSubject::FrameSet(id),
            // Unreachable for a row read from the database: the table's
            // CHECK guarantees exactly one of the pair.
            (None, None) => StackSubject::FrameSet(0),
        }
    }
```
9. `plan.rs` `StackingPlan`: same pair with the same `//` comment, `pub set_id: Option<i64>, pub project_id: Option<String>,`. In the literal at `~2428`:
   ```rust
   let (set_id, project_id) = crate::stacking::run::wire_subject(subject);
   ```
   then use `set_id, project_id,` in place of `set_id: frames_set_id,`.

- [ ] **Step 4: Frontend guard.** In `src/hooks/useStackingRuns.ts`, in the `stacking-progress` listener:
```ts
      .listen<StackingProgressEvent>('stacking-progress', (payload) => {
        if (cancelled) return;
        // A project run (no set id) is keyed and shown by the project's
        // Stacking tab — wave 6c. Until then this hook tracks frame sets only.
        const setId = payload.setId;
        if (setId == null) return;
        setProgress((prev) => {
          const next = new Map(prev);
          const existing = prev.get(setId);
          const startedAt =
            existing && existing.runId === payload.runId ? existing.startedAt : Date.now();
          next.set(setId, {
            runId: payload.runId,
            setId,
            // … the rest unchanged …
```
In the `stacking-complete` listener, add the same `const setId = payload.setId; if (setId == null) return;` right after `if (cancelled) return;`. Then replace every `payload.setId` below it with `setId`: the two maps, `RunOutcome.setId` and the notification `link`. `RunProgress.setId` and `RunOutcome.setId` stay `number`.

- [ ] **Step 5: Frontend test.** Create `src/hooks/useStackingRuns.test.tsx`:
```tsx
import { describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useStackingRuns } from './useStackingRuns';
import { api } from '../api';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

function progress(setId: number | null, projectId: string | null) {
  return {
    runId: 1, setId, projectId, stage: 'measure', groupKey: null, current: 1, total: 2,
    percent: 50, bytesDone: 0, bytesTotal: 0, frameId: null, message: null,
  };
}

function complete(setId: number | null, projectId: string | null) {
  return { runId: 1, setId, projectId, success: true, cancelled: false, error: null, warnings: [], masters: [] };
}

describe('useStackingRuns', () => {
  it('keys a frame-set run by its set id and ignores a project run until wave 6c', async () => {
    const handlers: Record<string, (p: unknown) => void> = {};
    vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
      handlers[ev] = cb;
      return Promise.resolve(() => {});
    }) as never);
    const { result } = renderHook(() => useStackingRuns());
    await waitFor(() => expect(handlers['stacking-complete']).toBeDefined());

    act(() => handlers['stacking-progress'](progress(null, 'p1')));
    expect(result.current.progress.size).toBe(0);
    act(() => handlers['stacking-complete'](complete(null, 'p1')));
    expect(result.current.lastOutcome.size).toBe(0);
    expect(notifyMock).not.toHaveBeenCalled();

    act(() => handlers['stacking-progress'](progress(7, null)));
    expect(result.current.progress.get(7)?.setId).toBe(7);
    act(() => handlers['stacking-complete'](complete(7, null)));
    expect(result.current.progress.has(7)).toBe(false);
    expect(result.current.lastOutcome.get(7)?.success).toBe(true);
    expect(notifyMock).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 6: Run the tests and watch them pass.** First the Rust tests from Step 2. Then regenerate the TS:
```bash
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract
git diff src/types
```
Expected: the diff shows exactly the five field moves listed under Interfaces. Then run `npx tsc --noEmit -p .` (expected clean) and `npx vitest run src/hooks/useStackingRuns.test.tsx` (expected 1 passed).

- [ ] **Step 7: Grep for completeness.** `grep -n 'wire_set_id' crates/athenaeum-core/src -r` must print nothing.

- [ ] **Step 8: Run the gate** (Global Constraints). TS diff is allowed: it must be exactly the Interfaces list.

- [ ] **Step 9: Commit** with the message `feat(stacking): the wire carries the run subject (setId | projectId)`, plus the trailers.

---

### Task 2: The four config fields (C2, spec §8, §9.4)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/config.rs`:
  - `GroupingConfig`, `~100-114`;
  - `OutputConfig`, `~310-315`;
  - `resolve_config`, `~427-438`;
  - the tests `serde_names_follow_the_spec` (`~998`) and `config_hash_is_canonical_and_pinned` (`~1478`), plus new tests.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `stage_hashes_are_pinned_to_golden_literals`, `~6278`.
- Modify `crates/athenaeum-core/src/stacking/groups.rs`: the test literal at `~734`.
- Modify `crates/athenaeum-core/src/ts_export.rs`: `stacking.ts`, add `ExposureMode` before `GroupingConfig`.
- Regenerate `src/types/stacking.ts`.

**Interfaces:**
```rust
// stacking/config.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ExposureMode { #[default] Seconds, Stops, Equivalent, #[serde(rename = "none")] NoSplit }
pub const MIN_STOPS_WIDTH: f64 = 0.1;
pub struct GroupingConfig { pub exposure_tolerance_sec: f64, pub exposure_mode: ExposureMode, pub stops_width: f64 }
pub struct OutputConfig { pub format: OutputFormat, pub cleanup: CleanupPolicy, pub coverage_map: bool, pub crop_to_coverage: u32 }
```
TS: `GroupingConfig` gains `exposureMode: ExposureMode, stopsWidth: number`; `OutputConfig` gains `coverageMap: boolean, cropToCoverage: number`; a new `export type ExposureMode = "seconds" | "stops" | "equivalent" | "none";` (with its doc). Nothing else moves.

- [ ] **Step 1: Write the failing tests**

`config.rs` `mod tests`:
```rust
    /// Plan 6b-2 C2: the four modes' wire names are the spec's.
    #[test]
    fn exposure_modes_use_the_spec_names() {
        for (mode, name) in [
            (ExposureMode::Seconds, "\"seconds\""),
            (ExposureMode::Stops, "\"stops\""),
            (ExposureMode::Equivalent, "\"equivalent\""),
            (ExposureMode::NoSplit, "\"none\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), name);
            assert_eq!(serde_json::from_str::<ExposureMode>(name).unwrap(), mode);
        }
    }

    /// Plan 6b-2 C2: a document written before the fields existed decodes to
    /// their defaults — a frame set's stored config regroups nothing.
    #[test]
    fn a_document_without_the_wave6_fields_decodes_to_their_defaults() {
        let cfg = resolve_config(
            Some(r#"{"grouping":{"exposureToleranceSec":3.0},"output":{"format":"fits"}}"#),
            None,
        )
        .unwrap();
        assert_eq!(cfg.grouping.exposure_tolerance_sec, 3.0);
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::Seconds);
        assert_eq!(cfg.grouping.stops_width, 1.0);
        assert!(!cfg.output.coverage_map);
        assert_eq!(cfg.output.crop_to_coverage, 0);
    }

    /// Plan 6b-2 C2/C3: `stopsWidth` at or below zero would make a class
    /// exclude its own anchor; it is clamped to `MIN_STOPS_WIDTH`.
    #[test]
    fn stops_width_is_clamped_to_its_floor() {
        for raw in ["0", "-1", "0.05"] {
            let json = format!(r#"{{"grouping":{{"stopsWidth":{raw}}}}}"#);
            let cfg = resolve_config(Some(&json), None).unwrap();
            assert_eq!(cfg.grouping.stops_width, MIN_STOPS_WIDTH, "{raw}");
        }
        let cfg = resolve_config(Some(r#"{"grouping":{"stopsWidth":2.0}}"#), None).unwrap();
        assert_eq!(cfg.grouping.stops_width, 2.0);
    }
```
Extend `serde_names_follow_the_spec`'s needle list with:
```rust
            "\"exposureMode\":\"seconds\"",
            "\"stopsWidth\":1.0",
            "\"coverageMap\":false",
            "\"cropToCoverage\":0",
```
`plan.rs`: rewrite `stage_hashes_are_pinned_to_golden_literals` so that the per-config line is one closure, called twice. Every literal stays byte-identical:
```rust
    #[test]
    fn stage_hashes_are_pinned_to_golden_literals() {
        use crate::stacking::config::{
            calibration_subtree, preset, stage_hash, ExposureMode, SourceIdentity, StackingPreset,
        };
        let mut custom = StackingConfig::default();
        custom.grouping.exposure_tolerance_sec = 5.0;
        custom.normalization.local.enabled = true;
        custom.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
        let configs = [
            ("default", StackingConfig::default()),
            ("fastPreview", preset(StackingPreset::FastPreview)),
            ("maximumQuality", preset(StackingPreset::MaximumQuality)),
            ("custom", custom),
        ];
        let masters = [
            "/m/dark.fits|100|2026-01-01T00:00:00+00:00",
            "/m/flat.fits|200|2026-01-01T00:00:00+00:00",
        ];
        let src = || {
            [SourceIdentity {
                file_id: 42,
                size: 123_456,
                modified_at: "2026-01-01T00:00:00Z".into(),
            }]
        };
        let calib_k = crate::stacking::ln::SeedsCalibration {
            frame_ids: vec![11, 22],
            k: vec![0.994],
        };
        let line = |name: &str, cfg: &StackingConfig| {
            let calib = stage_hash(&calibration_subtree(cfg), &masters, &src());
            let meas = measurement_hash_for(cfg, &calib);
            let reg = registration_hash_for(cfg, 7, "ref-calib", &calib, &meas);
            let ln_ref =
                normalization_hash_for(cfg, "r1,r2", "m1,m2", &[11, 22], "", None, Some(1.5));
            let ln = normalization_hash_for(
                cfg,
                &reg,
                &meas,
                &[11, 22],
                &ln_ref,
                Some(&calib_k),
                Some(1.5),
            );
            let ln_cal = seeds_calibration_hash_for(
                cfg,
                &ln_ref,
                &[(11, "r1", "m1"), (22, "r2", "m2")],
                Some(1.5),
            );
            format!("{name}: calib={calib} meas={meas} reg={reg} ln_ref={ln_ref} ln={ln} ln_cal={ln_cal}")
        };
        let got: Vec<String> = configs.iter().map(|(name, cfg)| line(name, cfg)).collect();
        // GOLDEN: recorded on main 17e2e96e before the Task 1 edit.
        assert_eq!(
            got,
            vec![
                "default: calib=f3cf4be2b2178358 meas=42d4b9fb12cf554e reg=d30b801f5d97b6ca ln_ref=004b8d8e1d474def ln=97b19ecf5b4bc774 ln_cal=0c2d2df3876ece32",
                "fastPreview: calib=f3cf4be2b2178358 meas=42d4b9fb12cf554e reg=8c11c39a4715fa26 ln_ref=004b8d8e1d474def ln=5aa2e757a576257b ln_cal=0c2d2df3876ece32",
                "maximumQuality: calib=f3cf4be2b2178358 meas=42d4b9fb12cf554e reg=6cc95c58dfa40ee0 ln_ref=75c6f0835d9d9cab ln=d8259ff416645417 ln_cal=60a1d2b69a0b80f9",
                "custom: calib=2b725e5e99c87f49 meas=ba5d1831b34dccf8 reg=ed7dc06cf6f32c9b ln_ref=75c6f0835d9d9cab ln=3d958bff2ee0136e ln_cal=60a1d2b69a0b80f9",
            ]
        );
        // Plan 6b-2 C2: the four wave-6 fields at non-default values move no
        // stage hash — an existing frame set keeps every cache.
        let with_wave6_fields: Vec<String> = configs
            .iter()
            .map(|(name, cfg)| {
                let mut cfg = cfg.clone();
                cfg.grouping.exposure_mode = ExposureMode::Stops;
                cfg.grouping.stops_width = 2.0;
                cfg.output.coverage_map = true;
                cfg.output.crop_to_coverage = 3;
                line(name, &cfg)
            })
            .collect();
        assert_eq!(with_wave6_fields, got);
        assert_eq!(
            serde_json::to_string(&calibration_subtree(&StackingConfig::default())["grouping"])
                .unwrap(),
            r#"{"exposureToleranceSec":2.0}"#
        );
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::config stacking::plan::tests::stage_hashes`

Expected: the build fails, because `ExposureMode`, `stops_width`, `coverage_map` and `crop_to_coverage` do not exist.

- [ ] **Step 3: Implement** (`config.rs`)
```rust
/// spec 2026-10-02 §8 (plan 6b-2 C2/C3): how a group's exposure classes
/// are formed. `seconds` is today's tolerance clustering — the frame-set
/// default, so an existing stack regroups nothing; `stops` takes every
/// exposure within `stopsWidth` stops of the class's shortest; `equivalent`
/// does the same on `t / N²` (`N = FOCALLEN / APTDIA`); `none` puts every
/// exposure of a colour × filter into one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ExposureMode {
    #[default]
    Seconds,
    Stops,
    Equivalent,
    // `none` on the wire; `NoSplit` in Rust so it never reads as `Option::None`.
    #[serde(rename = "none")]
    NoSplit,
}

/// The floor `resolve_config` clamps [`GroupingConfig::stops_width`] to
/// (plan 6b-2 C2). Below it two `stops`/`equivalent` class anchors could sit
/// so close that their tokens print the same (`groups::class_token_num` is
/// collision-safe only for anchors more than `2^MIN_STOPS_WIDTH` apart), and
/// a non-positive width would make a class exclude its own anchor.
pub const MIN_STOPS_WIDTH: f64 = 0.1;
```
In `GroupingConfig`, keep the existing field and its existing `///` doc, then add:
```rust
    // How exposure classes are formed (spec 2026-10-02 §8). Enters no stage
    // hash: `calibration_subtree` writes only `exposureToleranceSec`.
    pub exposure_mode: ExposureMode,
    // The class width of `stops` / `equivalent`, in stops. Clamped to
    // `MIN_STOPS_WIDTH` by `resolve_config`.
    pub stops_width: f64,
```
Set `Default` to `GroupingConfig { exposure_tolerance_sec: 2.0, exposure_mode: ExposureMode::Seconds, stops_width: 1.0 }`.

`OutputConfig`:
```rust
pub struct OutputConfig {
    pub format: OutputFormat,
    pub cleanup: CleanupPolicy,
    // Write the per-pixel frame count beside each master (spec §5.7): off for
    // a frame set, seeded on for a project. Enters no stage hash.
    pub coverage_map: bool,
    // Crop every output to the rectangle at least this many frames cover
    // (spec §5.9); 0 = no crop. Enters no stage hash.
    pub crop_to_coverage: u32,
}
```
In `resolve_config`, right after the `exposureToleranceSec` clamp:
```rust
    if !(config.grouping.stops_width >= MIN_STOPS_WIDTH) {
        warn!(
            value = config.grouping.stops_width,
            min = MIN_STOPS_WIDTH,
            "stacking config: stopsWidth below the minimum; clamped"
        );
        config.grouping.stops_width = MIN_STOPS_WIDTH;
    }
```
In `groups.rs` at `~734`, the test literal becomes `GroupingConfig { exposure_tolerance_sec: 2.0, ..GroupingConfig::default() }`.

`ts_export.rs` (`stacking.ts`): insert `crate::stacking::config::ExposureMode,` immediately before `crate::stacking::config::GroupingConfig,`.

- [ ] **Step 4: Re-pin the run fingerprint.**
  1. Run `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- stacking::config::tests::config_hash_is_canonical_and_pinned`. Expected: it FAILS, and the `left` side prints the new 16-hex fingerprint.
  2. Replace `"b3240460a4cec0de"` with that value.
  3. Add this paragraph above the assertion:
```rust
        // And here (collab v3 wave 6b-2 Task 2, plan ruling C2):
        // `grouping.exposureMode`/`stopsWidth` and `output.coverageMap`/
        // `cropToCoverage` join the config, all `#[serde(default)]` with no
        // version bump. No stage subtree folds them in —
        // `stage_hashes_are_pinned_to_golden_literals` proves it with every
        // field at a non-default value — so not one cached artifact goes
        // stale; only this whole-config fingerprint moves (and, once, the
        // plan gate's LN-exclusion memory, see `DRIZZLE_KERNEL_VERSION`).
```

- [ ] **Step 5: Run the tests and watch them pass.** First the commands from Step 2 and Step 4. Then the whole module, `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::` (expected all PASS). Then regenerate the TS and check the diff, which must be exactly the Interfaces list.

- [ ] **Step 6: Run the gate.**

- [ ] **Step 7: Commit** with the message `feat(stacking): exposure mode, stops width, coverage map and crop config fields`, plus the trailers.

---

### Task 3: Exposure classes, collision-safe tokens, `exposure_class` persisted (C3, spec §8)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/groups.rs`:
  - `IntegrationGroup`, `~86-110`;
  - `compose_key` and `exposure_token`, `~131-170`;
  - `cluster_indices`, `~403-442`, which is replaced;
  - `group_frames`, `~481-596`;
  - tests.
- Modify `crates/athenaeum-core/src/fits_parser/stored_header.rs`: add `aperture_mm_from_stored_header`.
- Modify `crates/athenaeum-core/src/db/stacking.rs`: `StackingRunGroupRow`, `row_to_group`, `GROUP_COLUMNS`, `NewGroup`, `insert_group`, and the test literals at `~1212` and `~1607`.
- Modify `crates/athenaeum-core/src/stacking/run.rs`: the `NewGroup` insert at `~884`, and every test `NewGroup` literal (`~10975`, plus any the compiler names).
- Modify `crates/athenaeum-core/src/stacking/plan.rs`:
  - `PlanGroup`, `~154-208`, and its literal at `~2315`;
  - the equivalence warning after the EXPTIME warning, `~1712-1743`;
  - the `NewGroup` test literals at `~4398`, `~5300` and `~5739`;
  - `paths.rs`'s `IntegrationGroup` test literal at `~1313`.
- Regenerate `src/types/stacking.ts`.

**Interfaces:**
```rust
// stacking/groups.rs
pub(crate) struct ClassInput { pub(crate) exposure_s: Option<f64>, pub(crate) f_ratio: Option<f64> }
pub(crate) struct ExposureClass { pub(crate) token: String, pub(crate) exposure_s: Option<f64>, pub(crate) members: Vec<usize> }
pub(crate) fn classify_exposures(inputs: &[ClassInput], cfg: &GroupingConfig) -> Vec<ExposureClass>;
pub(crate) fn class_token_num(x: f64) -> String;
pub(crate) fn f_ratio(focal_mm: Option<f64>, aperture_mm: Option<f64>) -> Option<f64>;
pub(crate) fn is_seconds_token(token: &str) -> bool;
pub(crate) fn sanitized_or(value: Option<&str>, fallback: &str) -> String;   // visibility only (Task 6a uses it)
pub struct IntegrationGroup { /* … */ pub exposure_class: String, /* … */ }
// fits_parser/stored_header.rs
pub fn aperture_mm_from_stored_header(format: &str, header: &str) -> Option<f64>;
// db/stacking.rs
pub struct StackingRunGroupRow { /* … */ pub exposure_class: Option<String> }
pub struct NewGroup<'a> { /* … */ pub exposure_class: Option<&'a str>, /* … */ }
// stacking/plan.rs
pub struct PlanGroup { /* … */ pub exposure_class: String, /* … */ }
```
TS: `StackingRunGroupRow` gains `exposureClass: string | null`; `PlanGroup` gains `exposureClass: string`. Nothing else moves. (`IntegrationGroup` is not in the TS registry.)

- [ ] **Step 1: Write the failing tests**

`groups.rs` `mod tests`:
```rust
    /// Plan 6b-2 C3: a class token is formatted at a precision that never
    /// merges two anchors — `fmt_num`'s two decimals merge these, and a
    /// merged token would collide on `stacking_run_groups`'
    /// `UNIQUE(run_id, group_key)`.
    #[test]
    fn class_tokens_never_merge_two_anchors_fmt_num_would() {
        use crate::calibration_library::paths::fmt_num;
        assert_eq!(fmt_num(0.002), fmt_num(0.004), "the premise: fmt_num merges them");
        assert_eq!(fmt_num(0.005), fmt_num(0.0101), "the premise: fmt_num merges them");
        assert_ne!(class_token_num(0.002), class_token_num(0.004));
        assert_ne!(class_token_num(0.005), class_token_num(0.0101));
        assert_eq!(class_token_num(300.0), "300");
        assert_eq!(class_token_num(0.5), "0.5");
        assert_eq!(class_token_num(2.4), "2.4");
        assert_eq!(class_token_num(9.566326530612244), "9.566");
        assert_eq!(class_token_num(0.005), "0.005");
        assert_eq!(class_token_num(0.0101), "0.0101");
        assert_eq!(class_token_num(12345.6), "12346");
    }

    /// Plan 6b-2 C3: at the `stopsWidth` floor, a dense sweep over five
    /// orders of magnitude never yields two classes with one token.
    #[test]
    fn stop_classes_never_share_a_token() {
        use crate::stacking::config::{ExposureMode, MIN_STOPS_WIDTH};
        let cfg = GroupingConfig {
            exposure_mode: ExposureMode::Stops,
            stops_width: MIN_STOPS_WIDTH,
            ..GroupingConfig::default()
        };
        let inputs: Vec<ClassInput> = (0..4000)
            .map(|k| ClassInput {
                exposure_s: Some(0.001 * 1.003f64.powi(k)),
                f_ratio: None,
            })
            .collect();
        let classes = classify_exposures(&inputs, &cfg);
        let mut tokens: Vec<&str> = classes.iter().map(|c| c.token.as_str()).collect();
        let n = tokens.len();
        assert!(n > 100, "the sweep spans many classes: {n}");
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), n, "two classes share a token");
    }

    /// Plan 6b-2 C3 / spec §8: the four modes, their tokens, the seconds
    /// value each class carries (never an `eq` anchor), and frames without
    /// EXPTIME in their own `unknown` class in every mode.
    #[test]
    fn the_four_exposure_modes_class_and_token_as_the_spec_says() {
        use crate::stacking::config::ExposureMode;
        let input = |t: Option<f64>, n: Option<f64>| ClassInput {
            exposure_s: t,
            f_ratio: n,
        };
        let inputs = [
            input(Some(60.0), Some(5.0)),
            input(Some(90.0), Some(5.0)),
            input(Some(120.0), None),
            input(Some(180.0), Some(2.0)),
            input(Some(300.0), Some(2.0)),
            input(Some(600.0), Some(2.0)),
            input(None, None),
        ];
        let run = |mode| {
            let cfg = GroupingConfig {
                exposure_mode: mode,
                ..GroupingConfig::default()
            };
            classify_exposures(&inputs, &cfg)
                .into_iter()
                .map(|c| (c.token, c.exposure_s, c.members))
                .collect::<Vec<_>>()
        };
        let row = |t: &str, e: Option<f64>, m: &[usize]| (t.to_string(), e, m.to_vec());

        assert_eq!(
            run(ExposureMode::Seconds),
            vec![
                row("60s", Some(60.0), &[0]),
                row("90s", Some(90.0), &[1]),
                row("120s", Some(120.0), &[2]),
                row("180s", Some(180.0), &[3]),
                row("300s", Some(300.0), &[4]),
                row("600s", Some(600.0), &[5]),
                row("unknown", None, &[6]),
            ]
        );
        assert_eq!(
            run(ExposureMode::Stops),
            vec![
                row("st60", Some(60.0), &[0, 1, 2]),
                row("st180", Some(180.0), &[3, 4]),
                row("st600", Some(600.0), &[5]),
                row("unknown", None, &[6]),
            ]
        );
        // e = t / N²: 2.4, 3.6 | 45, 75 | 150; the frame with no N is classed by seconds.
        assert_eq!(
            run(ExposureMode::Equivalent),
            vec![
                row("eq2.4", Some(60.0), &[0, 1]),
                row("eq45", Some(180.0), &[3, 4]),
                row("eq150", Some(600.0), &[5]),
                row("120s", Some(120.0), &[2]),
                row("unknown", None, &[6]),
            ]
        );
        assert_eq!(
            run(ExposureMode::NoSplit),
            vec![
                row("all", Some(60.0), &[0, 1, 2, 3, 4, 5]),
                row("unknown", None, &[6]),
            ]
        );
        assert!(is_seconds_token("120s"));
        assert!(!is_seconds_token("st60") && !is_seconds_token("eq2.4"));
        assert!(!is_seconds_token("all") && !is_seconds_token("unknown"));
    }

    /// Plan 6b-2 C3: a frame set in `equivalent` mode reads APTDIA from the
    /// stored header blob and FOCALLEN from the column; 60 s at f/5 and 240 s
    /// at f/10 are one class.
    #[test]
    fn a_frame_set_in_equivalent_mode_classes_by_focal_ratio() {
        use crate::stacking::config::ExposureMode;
        let f = test_fixtures::frame_set("LDN 1272");
        let spec = |stem: &'static str, exptime: f64| LightSpec {
            stem,
            instrume: "cam",
            filter: Some("L"),
            binning: 1,
            width: 64,
            height: 48,
            exptime,
            date_obs: "2025-01-01T20:00:00",
            bayerpat: None,
            write_file: false,
        };
        let (fast, _) = test_fixtures::add_light(&f, &spec("fast", 60.0));
        let (slow, _) = test_fixtures::add_light(&f, &spec("slow", 240.0));
        for (id, focal) in [(fast, 500.0), (slow, 1000.0)] {
            f.conn
                .execute("UPDATE frames SET focallen = ?2 WHERE id = ?1", rusqlite::params![id, focal])
                .unwrap();
            let card_text = format!(
                "{:<80}{:<80}",
                "APTDIA  =                100.0 / aperture diameter (mm)", "END"
            );
            f.conn
                .execute(
                    "INSERT INTO fits_header (file_id, header)
                     VALUES ((SELECT file_id FROM frames WHERE id = ?1), ?2)",
                    rusqlite::params![id, card_text],
                )
                .unwrap();
        }
        let seconds = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
        assert_eq!(seconds.len(), 2, "seconds keeps 60 s and 240 s apart");
        let cfg = GroupingConfig {
            exposure_mode: ExposureMode::Equivalent,
            ..GroupingConfig::default()
        };
        let groups = group_frames(&f.conn, f.set_id, &cfg).unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].key, "mono__L__bin1__eq2.4");
        assert_eq!(groups[0].exposure_class, "eq2.4");
        assert_eq!(groups[0].exposure_s, Some(60.0), "the class's seconds value is its shortest member's");
    }
```
`fits_parser/stored_header.rs` tests:
```rust
    #[test]
    fn aperture_reads_aptdia_finite_and_positive_only() {
        let fits = |v: &str| format!("{:<80}{:<80}", format!("APTDIA  = {v:>20} / aperture"), "END");
        assert_eq!(aperture_mm_from_stored_header("FITS", &fits("200.0")), Some(200.0));
        assert_eq!(aperture_mm_from_stored_header("FITS", &fits("0")), None);
        assert_eq!(aperture_mm_from_stored_header("FITS", &fits("-5")), None);
        assert_eq!(aperture_mm_from_stored_header("FITS", &format!("{:<80}", "END")), None);
        assert_eq!(aperture_mm_from_stored_header("ZIP", &fits("200.0")), None);
    }
```
`db/stacking.rs` tests:
```rust
    #[test]
    fn a_group_row_carries_its_exposure_class() {
        let c = conn();
        let set = seed_set(&c);
        let run = insert_run(&c, &new_run(&StackSubject::FrameSet(set))).unwrap();
        insert_group(
            &c,
            &NewGroup {
                run_id: run,
                group_key: "mono__L__bin1__st60",
                instrume: None,
                color_mode: "mono",
                filter: Some("L"),
                binning: Some(1),
                width: None,
                height: None,
                exposure: Some(60.0),
                exposure_class: Some("st60"),
                frame_count: 3,
                included_count: 3,
            },
        )
        .unwrap();
        let groups = list_groups(&c, run).unwrap();
        assert_eq!(groups[0].exposure_class.as_deref(), Some("st60"));
        assert_eq!(groups[0].exposure, Some(60.0), "exposure stays seconds");
    }
```
(`seed_set` and `new_run` are the module's existing test helpers. If `new_run` has another name, use the helper the neighbouring run tests call.)

`plan.rs` `mod tests`, beside `plan_warns_about_frames_without_exptime`:
```rust
    /// Spec §8: in `equivalent` mode, frames whose focal ratio is unknown are
    /// classed by seconds, and the plan says how many. The fixture's lights
    /// carry no `fits_header` blob, so `load_apertures` finds no aperture and
    /// every frame classes by seconds; a fixture that wrote a header blob with
    /// APTDIA would change what this test sees (review M5).
    #[test]
    fn plan_warns_when_exposure_equivalence_is_unknown() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
        }
        let mut cfg = StackingConfig::default();
        cfg.grouping.exposure_mode = crate::stacking::config::ExposureMode::Equivalent;
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            Some(cfg),
        )
        .unwrap();
        assert!(
            plan.warnings
                .iter()
                .any(|w| w == "exposure equivalence unknown for 3 frames — grouped by seconds"),
            "{:?}",
            plan.warnings
        );
        assert!(plan.groups.iter().all(|g| is_seconds_token(&g.exposure_class)));
    }
```
(Import `crate::stacking::groups::is_seconds_token` in the test, or put it in the test module's `use`.)

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 class_token stop_classes the_four_exposure a_frame_set_in_equivalent aperture_reads a_group_row_carries plan_warns_when_exposure`

Expected: the build fails, because none of the new items exist.

- [ ] **Step 3: Implement the classifier** (`groups.rs`). Replace `cluster_indices` with:
```rust
/// One frame's input to [`classify_exposures`] (plan 6b-2 C3): its exposure
/// (`None` = no `EXPTIME`) and its focal ratio (`None` = unknown; read only in
/// `equivalent` mode). Source-agnostic, so a frame set and a project class
/// by one rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ClassInput {
    pub(crate) exposure_s: Option<f64>,
    pub(crate) f_ratio: Option<f64>,
}

/// One exposure class: its group-key token, the SECONDS value its group's
/// `exposure_s` carries — the shortest member's exposure, never an `eq`
/// anchor, because the EXPTIME warning, `PlanGroup`, `stacking_run_groups.exposure`
/// and the master file name all read it as seconds — and its members'
/// indices in input order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExposureClass {
    pub(crate) token: String,
    pub(crate) exposure_s: Option<f64>,
    pub(crate) members: Vec<usize>,
}

/// Partition one bucket's frames into exposure classes (spec §8, plan 6b-2
/// C3). Frames without `EXPTIME` form their own `unknown` class in every
/// mode — a missing value is not "0 s" — so the EXPTIME warning stays
/// keyed on `exposure_s == None`. `cfg.stops_width` is trusted: `resolve_config`
/// clamps it to [`crate::stacking::config::MIN_STOPS_WIDTH`].
pub(crate) fn classify_exposures(inputs: &[ClassInput], cfg: &GroupingConfig) -> Vec<ExposureClass> {
    let (known, unknown): (Vec<usize>, Vec<usize>) =
        (0..inputs.len()).partition(|&i| inputs[i].exposure_s.is_some());
    let exposure = |i: usize| inputs[i].exposure_s.expect("known exposures only");
    let mut classes = match cfg.exposure_mode {
        ExposureMode::Seconds => seconds_classes(inputs, &known, cfg.exposure_tolerance_sec),
        ExposureMode::Stops => ratio_classes(inputs, &known, exposure, cfg.stops_width, "st"),
        ExposureMode::Equivalent => {
            let (with_n, without_n): (Vec<usize>, Vec<usize>) =
                known.iter().partition(|&&i| inputs[i].f_ratio.is_some());
            let equivalent = |i: usize| {
                let n = inputs[i].f_ratio.expect("partitioned on a known focal ratio");
                exposure(i) / (n * n)
            };
            let mut classes = ratio_classes(inputs, &with_n, equivalent, cfg.stops_width, "eq");
            classes.extend(seconds_classes(inputs, &without_n, cfg.exposure_tolerance_sec));
            classes
        }
        ExposureMode::NoSplit => {
            if known.is_empty() {
                Vec::new()
            } else {
                let shortest = known.iter().map(|&i| exposure(i)).fold(f64::INFINITY, f64::min);
                vec![ExposureClass {
                    token: "all".to_string(),
                    exposure_s: Some(shortest),
                    members: known,
                }]
            }
        }
    };
    if !unknown.is_empty() {
        classes.push(ExposureClass {
            token: "unknown".to_string(),
            exposure_s: None,
            members: unknown,
        });
    }
    classes
}

/// Today's clustering (owner decision 2026-09-10), byte for byte: sorted
/// ascending, a new class whenever the next exposure is more than
/// `tolerance` past the CURRENT class's first value (not the previous
/// frame's, so a slow drift cannot walk away from where the class started).
/// Members restored to input order. `tolerance` is clamped upstream to
/// `MIN_EXPOSURE_TOLERANCE_SEC`, which keeps `fmt_num`'s tokens distinct.
fn seconds_classes(inputs: &[ClassInput], idx: &[usize], tolerance: f64) -> Vec<ExposureClass> {
    let mut sorted: Vec<usize> = idx.to_vec();
    sorted.sort_by(|&a, &b| {
        inputs[a]
            .exposure_s
            .partial_cmp(&inputs[b].exposure_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut clusters: Vec<(f64, Vec<usize>)> = Vec::new();
    for i in sorted {
        let e = inputs[i].exposure_s.expect("known exposures only");
        match clusters.last_mut() {
            Some(last) if e - last.0 <= tolerance => last.1.push(i),
            _ => clusters.push((e, vec![i])),
        }
    }
    clusters
        .into_iter()
        .map(|(label, mut members)| {
            members.sort_unstable();
            ExposureClass {
                token: exposure_token(Some(label)),
                exposure_s: Some(label),
                members,
            }
        })
        .collect()
}

/// `stops` / `equivalent` (spec §8): values sorted ascending; a class starts
/// at the shortest remaining value `v0` and takes every value `≤ v0 · 2^W`.
/// Token `<prefix><class_token_num(v0)>`; the class's seconds value is its
/// shortest member's exposure.
fn ratio_classes(
    inputs: &[ClassInput],
    idx: &[usize],
    value: impl Fn(usize) -> f64,
    width_stops: f64,
    prefix: &str,
) -> Vec<ExposureClass> {
    let factor = 2f64.powf(width_stops);
    let mut sorted: Vec<(f64, usize)> = idx.iter().map(|&i| (value(i), i)).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<ExposureClass> = Vec::new();
    let mut anchor: Option<f64> = None;
    for (v, i) in sorted {
        if anchor.is_none_or(|a| v > a * factor) {
            anchor = Some(v);
            out.push(ExposureClass {
                token: format!("{prefix}{}", class_token_num(v)),
                exposure_s: None,
                members: Vec::new(),
            });
        }
        out.last_mut().expect("a class was pushed above").members.push(i);
    }
    for class in &mut out {
        class.members.sort_unstable();
        class.exposure_s = class
            .members
            .iter()
            .filter_map(|&i| inputs[i].exposure_s)
            .reduce(f64::min);
    }
    out
}

/// The number in a `st`/`eq` class token (plan 6b-2 C3): four significant
/// digits, trailing zeros trimmed, never an exponent. Two classes of one
/// bucket never print the same: consecutive anchors differ by more than
/// `2^MIN_STOPS_WIDTH` (×1.07), and rounding to four significant digits
/// moves a value by at most 0.05 %. A non-positive or non-finite anchor (an
/// `EXPTIME` of 0 in a frame set) prints in full, which is injective.
pub(crate) fn class_token_num(x: f64) -> String {
    if !x.is_finite() || x <= 0.0 {
        return format!("{x}");
    }
    let magnitude = x.log10().floor() as i32;
    let decimals = (3 - magnitude).max(0) as usize;
    let s = format!("{x:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Whether a class token is a seconds token (`<t>s`) — in `equivalent` mode,
/// the classes of frames whose focal ratio is unknown. A convention on the
/// string: the other four tokens never end in `s` (`st<num>`, `eq<num>`,
/// `all`, `unknown`), and a new token must keep it so (review M4).
pub(crate) fn is_seconds_token(token: &str) -> bool {
    token.ends_with('s')
}

/// `N = focal length / aperture` when both are known, finite and positive
/// (spec §8).
pub(crate) fn f_ratio(focal_mm: Option<f64>, aperture_mm: Option<f64>) -> Option<f64> {
    match (focal_mm, aperture_mm) {
        (Some(f), Some(a)) if f.is_finite() && a.is_finite() && f > 0.0 && a > 0.0 => Some(f / a),
        _ => None,
    }
}

/// `APTDIA` of each frame from its stored header blob (spec §8: `frames` has
/// no aperture column). One query per 500 ids; read only in `equivalent`
/// mode. The first `fits_header` row of a frame that carries one wins.
fn load_apertures(conn: &Connection, frame_ids: &[i64]) -> Result<HashMap<i64, f64>> {
    let mut out = HashMap::new();
    for chunk in frame_ids.chunks(500) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT f.id, fi.format, fh.header FROM frames f
             JOIN files fi ON fi.id = f.file_id
             JOIN fits_header fh ON fh.file_id = f.file_id
             WHERE f.id IN ({placeholders}) ORDER BY f.id, fh.id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (frame_id, format, header) = row?;
            if out.contains_key(&frame_id) {
                continue;
            }
            if let Some(a) =
                crate::fits_parser::stored_header::aperture_mm_from_stored_header(&format, &header)
            {
                out.insert(frame_id, a);
            }
        }
    }
    Ok(out)
}
```
Imports: `use crate::stacking::config::{ExposureMode, GroupingConfig};`. Make `sanitized_or` `pub(crate)`.

`compose_key` takes the token:
```rust
fn compose_key(color: ColorMode, filter_token: &str, binning: i64, class_token: &str) -> String {
    let color_tok = match color {
        ColorMode::Mono => "mono",
        ColorMode::Osc => "osc",
    };
    format!("{color_tok}__{filter_token}__bin{binning}__{class_token}")
}
```
`group_key` calls `compose_key(color, &filt, binning, &exposure_token(exposure_s))`. Its output is byte-identical.

Add to `IntegrationGroup`, after `exposure_s`:
```rust
    /// The group's exposure-class token (spec §8, plan 6b-2 C3): `<t>s`,
    /// `st<t0>`, `eq<e0>`, `all` or `unknown` — the key's last segment.
    pub exposure_class: String,
```
In `group_frames`:
  - After `load_solved_scales`:
    ```rust
    let apertures = if cfg.exposure_mode == ExposureMode::Equivalent { load_apertures(conn, &frame_ids)? } else { HashMap::new() };
    ```
  - Replace the loop header `for (label, idxs) in cluster_indices(&bucket.members, cfg) {` with:
```rust
        let inputs: Vec<ClassInput> = bucket
            .members
            .iter()
            .map(|m| ClassInput {
                exposure_s: m.exptime,
                f_ratio: f_ratio(m.focallen, apertures.get(&m.frame_id).copied()),
            })
            .collect();
        for class in classify_exposures(&inputs, cfg) {
            let idxs = class.members;
            let label = class.exposure_s;
```
  - Leave the body as is, then:
    - make the key `compose_key(color, &filter_token, binning, &class.token)`;
    - set the literal to `exposure_s: label,` with the new `exposure_class: class.token,` after it.
  - Comment references to `cluster_indices`, in `groups.rs` and in the `plan.rs:~1714` comment, now say `seconds_classes`.

`fits_parser/stored_header.rs`:
```rust
/// `APTDIA` (mm) of a stored header blob — finite and positive, else `None`
/// (plan 6b-2 C3/C4). `format` is `files.format` (`"FITS"` / `"XISF"`); any
/// other value reads as no aperture.
pub fn aperture_mm_from_stored_header(format: &str, header: &str) -> Option<f64> {
    let format = match format {
        "FITS" => FileFormat::FITS,
        "XISF" => FileFormat::XISF,
        _ => return None,
    };
    parse_stored_header_keys(format, header)
        .get("APTDIA")
        .and_then(|v| v.trim().trim_matches('\'').trim().parse::<f64>().ok())
        .filter(|a| a.is_finite() && *a > 0.0)
}
```

- [ ] **Step 4: Persist and export the class**
  - `db/stacking.rs`: add the field to `StackingRunGroupRow` after `beta`:
    ```rust
        // The group's exposure-class token (spec 2026-10-02 §8); `None` for a
        // run written before 6b-2. Plain comment: a doc comment would leak into
        // the generated TS.
        pub exposure_class: Option<String>,
    ```
    In the same file:
    - `GROUP_COLUMNS` gains `, exposure_class` at its end, and `row_to_group` gains `exposure_class: r.get(20)?,`.
    - `NewGroup` gains `pub exposure_class: Option<&'a str>,`.
    - `insert_group`'s column list gains `exposure_class`, with `?12` and `g.exposure_class` appended to the params.
  - `run.rs` `start_stacking` insert (`~884`): add `exposure_class: Some(&g.exposure_class),`. Every other `NewGroup { … }` literal the compiler names gets `exposure_class: None,`. These are test literals in `run.rs`, `plan.rs` and `db/stacking.rs`.
  - `plan.rs` `PlanGroup`: add after `exposure_s`:
    ```rust
        // The group's exposure-class token (spec §8) — what the Groups table
        // shows beside the seconds value.
        pub exposure_class: String,
    ```
    The literal at `~2315` gets `exposure_class: g.exposure_class.clone(),`.
  - `paths.rs`'s `IntegrationGroup` test literal (`~1313`) gets `exposure_class: "unknown".to_string(),`.
  - `plan.rs` `build_plan`: right after the EXPTIME-warning loop, add:
```rust
    // Spec §8: in `equivalent` mode a frame with no known focal ratio is
    // classed by seconds — say how many, once.
    if cfg.grouping.exposure_mode == crate::stacking::config::ExposureMode::Equivalent {
        let n: usize = groups
            .iter()
            .filter(|g| crate::stacking::groups::is_seconds_token(&g.exposure_class))
            .map(|g| g.frames.len())
            .sum();
        if n > 0 {
            warnings.push(format!(
                "exposure equivalence unknown for {n} frame{} — grouped by seconds",
                if n == 1 { "" } else { "s" }
            ));
        }
    }
```

- [ ] **Step 5: Run the tests and watch them pass.** First the Step 2 command (expected PASS). Then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::groups stacking::plan`, where every existing grouping and plan test passes unchanged: `key_format`, `exposure_outside_the_tolerance_splits` and `plan_warns_about_frames_without_exptime` among them. Then regenerate the TS: the diff must be exactly `exposureClass` on `StackingRunGroupRow` and `PlanGroup`.

- [ ] **Step 6: Run the gate.**

- [ ] **Step 7: Commit** with the message `feat(stacking): exposure classes (seconds, stops, equivalent, none) with collision-safe tokens`, plus the trailers.

---

### Task 4: The manifest meta parser and `meta.aperture` (C4)

**Files:**
- Modify `crates/athenaeum-core/src/collab/frame_meta.rs`:
  - new types and the parser;
  - `build_frame_meta` gains `aperture` (`~83-167`);
  - tests.

**Interfaces:**
```rust
// collab/frame_meta.rs (gated render+solver, `pub mod`)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectFrameMeta {
    pub exptime_sec: f64, pub date_obs: Option<String>,
    pub naxis1: Option<i64>, pub naxis2: Option<i64>, pub pixel_scale_arcsec: Option<f64>,
    pub bayerpat: Option<String>, pub focal_len: Option<f64>, pub aperture: Option<f64>,
    pub instrume: Option<String>, pub wcs: Option<ManifestWcs>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct ManifestWcs { pub crval1: f64, pub crval2: f64, pub crpix1: f64, pub crpix2: f64, pub cd: [f64; 4], #[serde(default)] pub sip: Option<ManifestSip> }
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ManifestSip { pub order: i32, pub a: Vec<Vec<f64>>, pub b: Vec<Vec<f64>>, #[serde(default)] pub ap: Option<Vec<Vec<f64>>>, #[serde(default)] pub bp: Option<Vec<Vec<f64>>> }
pub fn parse_project_frame_meta(manifest_json: &str) -> anyhow::Result<ProjectFrameMeta>;
impl ManifestWcs { pub fn to_record(&self, pixel_scale_arcsec: Option<f64>) -> PlateSolveRecord; }
```
Consumed by: Task 6a (the origin, the frame source), Task 8a (the proposal), Task 10b (the solve inputs).

- [ ] **Step 1: Write the failing tests** (`frame_meta.rs` `mod tests`):
```rust
    /// Plan 6b-2 C4: the round trip publish → manifest → stacking returns the
    /// catalog's own solve: crpix back to 0-based, CD and SIP tables equal,
    /// and the same pixel → sky mapping.
    #[test]
    fn a_published_wcs_reads_back_as_the_catalog_record() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some("L"), None);
        seed_solve(&conn, frame_id, Some(3));
        let original = crate::plate_solve::storage::get_plate_solve(&conn, frame_id)
            .unwrap()
            .unwrap();
        let m = build_frame_meta(&conn, frame_id).unwrap();
        let row = serde_json::json!({
            "exptimeSec": m.exptime_sec,
            "dateObs": m.date_obs,
            "meta": m.meta,
        });
        let parsed = parse_project_frame_meta(&row.to_string()).unwrap();
        assert_eq!(parsed.exptime_sec, 300.0);
        assert_eq!((parsed.naxis1, parsed.naxis2), (Some(6248), Some(4176)));
        assert_eq!(parsed.pixel_scale_arcsec, Some(2.0));
        let record = parsed.wcs.as_ref().unwrap().to_record(parsed.pixel_scale_arcsec);
        assert_eq!(record.frame_id, 0);
        assert_eq!((record.crpix1, record.crpix2), (original.crpix1, original.crpix2));
        assert_eq!(
            (record.cd1_1, record.cd1_2, record.cd2_1, record.cd2_2),
            (original.cd1_1, original.cd1_2, original.cd2_1, original.cd2_2)
        );
        assert_eq!(record.sip_order, original.sip_order);
        assert_eq!(record.sip_a_coeffs, original.sip_a_coeffs);
        assert_eq!(record.sip_bp_coeffs, original.sip_bp_coeffs);
        let (a, b) = (original.to_solution().unwrap(), record.to_solution().unwrap());
        for (x, y) in [(0.0, 0.0), (3123.5, 2087.5), (6000.0, 100.0)] {
            let (ra0, dec0) = a.pixel_to_sky(x, y);
            let (ra1, dec1) = b.pixel_to_sky(x, y);
            assert!((ra0 - ra1).abs() < 1e-12 && (dec0 - dec1).abs() < 1e-12);
        }
    }

    /// Plan 6b-2 C4: an own row (the `FrameInWire` shape, no `dateObs` when
    /// unknown), a replica row with every key, and a row with `meta: null`.
    #[test]
    fn the_manifest_parser_reads_own_replica_and_bare_rows() {
        let own = r#"{"frameUuid":"u1","fileName":"c.fits","blake3":"b","byteSize":1,"xxh3":"x",
            "filterRaw":"Red","filterCanonical":"R","channel":"osc","exptimeSec":120.0,"gateVersion":0,
            "meta":{"bayerpat":"RGGB","focalLen":400.0,"aperture":80.0,"instrume":"ASI2600MC"}}"#;
        let m = parse_project_frame_meta(own).unwrap();
        assert_eq!(m.exptime_sec, 120.0);
        assert_eq!(m.date_obs, None);
        assert_eq!(m.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!((m.focal_len, m.aperture), (Some(400.0), Some(80.0)));
        assert_eq!(m.wcs, None);
        let bare = parse_project_frame_meta(r#"{"exptimeSec":60.0,"meta":null}"#).unwrap();
        assert_eq!(bare, ProjectFrameMeta { exptime_sec: 60.0, ..ProjectFrameMeta::default() });
        assert!(parse_project_frame_meta(r#"{"meta":{}}"#).is_err(), "no exposure is an error");
        assert!(
            parse_project_frame_meta(r#"{"exptimeSec":1.0,"meta":{"wcs":{"cd":[1,2]}}}"#).is_err(),
            "a malformed WCS is an error, never a guess"
        );
    }

    /// Plan 6b-2 C4 / spec §8: publication carries the aperture from the
    /// stored header blob; without one the key is omitted.
    #[test]
    fn meta_carries_the_aperture_from_the_stored_header() {
        let conn = seed_db();
        let with = seed_frame(&conn, Some("L"), None);
        let without = seed_frame(&conn, Some("L"), None);
        let card_text = format!(
            "{:<80}{:<80}",
            "APTDIA  =                200.0 / aperture diameter (mm)", "END"
        );
        conn.execute(
            "INSERT INTO fits_header (file_id, header)
             VALUES ((SELECT file_id FROM frames WHERE id = ?1), ?2)",
            params![with, card_text],
        )
        .unwrap();
        assert_eq!(build_frame_meta(&conn, with).unwrap().meta["aperture"], 200.0);
        assert!(build_frame_meta(&conn, without).unwrap().meta.get("aperture").is_none());
    }
```
The existing `full_sip_order_5_meta_stays_under_the_hub_byte_limit` must still pass; the aperture adds about 18 bytes when present.

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 collab::frame_meta`

Expected: the build fails, because `parse_project_frame_meta` does not exist.

- [ ] **Step 3: Implement** (append to `frame_meta.rs`, above `#[cfg(test)]`; add `use anyhow::Context;` and `use serde::Deserialize;`):
```rust
/// What a stacking run reads from one project frame's manifest row (plan
/// 6b-2 C4, spec §4.3), parsed once from `project_frames_local.manifest_json`
/// (an own row's `FrameInWire` shape or a replica's `FrameViewWire` — the
/// keys read here are the same in both). `exptimeSec` and `dateObs` are
/// top-level fields; everything else lives under `meta`, which is frozen at
/// announce (a new content version never carries one), so an older frame
/// simply lacks the newer keys (`aperture`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectFrameMeta {
    pub exptime_sec: f64,
    pub date_obs: Option<String>,
    pub naxis1: Option<i64>,
    pub naxis2: Option<i64>,
    pub pixel_scale_arcsec: Option<f64>,
    pub bayerpat: Option<String>,
    pub focal_len: Option<f64>,
    pub aperture: Option<f64>,
    pub instrume: Option<String>,
    pub wcs: Option<ManifestWcs>,
}

/// `meta.wcs` as [`build_frame_meta`] writes it: TAN (no CTYPE), `crpix`
/// FITS 1-BASED, the CD matrix row-major, optional SIP tables.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestWcs {
    pub crval1: f64,
    pub crval2: f64,
    pub crpix1: f64,
    pub crpix2: f64,
    pub cd: [f64; 4],
    #[serde(default)]
    pub sip: Option<ManifestSip>,
}

/// `meta.wcs.sip`: the stored tables verbatim (square or triangular).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ManifestSip {
    pub order: i32,
    pub a: Vec<Vec<f64>>,
    pub b: Vec<Vec<f64>>,
    #[serde(default)]
    pub ap: Option<Vec<Vec<f64>>>,
    #[serde(default)]
    pub bp: Option<Vec<Vec<f64>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestRowWire {
    exptime_sec: f64,
    #[serde(default)]
    date_obs: Option<String>,
    #[serde(default)]
    meta: Option<MetaWire>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct MetaWire {
    naxis1: Option<i64>,
    naxis2: Option<i64>,
    pixel_scale_arcsec: Option<f64>,
    bayerpat: Option<String>,
    focal_len: Option<f64>,
    aperture: Option<f64>,
    instrume: Option<String>,
    wcs: Option<ManifestWcs>,
}

/// Parse one manifest row (plan 6b-2 C4). A missing exposure or a malformed
/// key is an error — the caller leaves the frame out with a reason, never a
/// guess. Unknown keys are ignored.
pub fn parse_project_frame_meta(manifest_json: &str) -> Result<ProjectFrameMeta> {
    let row: ManifestRowWire =
        serde_json::from_str(manifest_json).context("manifest row is not a frame row")?;
    let meta = row.meta.unwrap_or_default();
    Ok(ProjectFrameMeta {
        exptime_sec: row.exptime_sec,
        date_obs: row.date_obs,
        naxis1: meta.naxis1,
        naxis2: meta.naxis2,
        pixel_scale_arcsec: meta.pixel_scale_arcsec,
        bayerpat: meta.bayerpat,
        focal_len: meta.focal_len,
        aperture: meta.aperture,
        instrume: meta.instrume,
        wcs: meta.wcs,
    })
}

impl ManifestWcs {
    /// The `PlateSolveRecord` this WCS is (plan 6b-2 C4): `crpix` back to
    /// 0-based (the record's convention, `fits_writer::wcs` adds the 1),
    /// the SIP tables re-encoded as the JSON strings the catalog stores,
    /// `frame_id = 0` (a project frame has no catalog row), the scale from
    /// the manifest when it has one (else `sqrt|det CD|`), the rotation as
    /// `WcsSolution::field_rotation_deg` reports it, counts zero.
    pub fn to_record(&self, pixel_scale_arcsec: Option<f64>) -> PlateSolveRecord {
        let [cd1_1, cd1_2, cd2_1, cd2_2] = self.cd;
        let scale = pixel_scale_arcsec
            .unwrap_or_else(|| (cd1_1 * cd2_2 - cd1_2 * cd2_1).abs().sqrt() * 3600.0);
        let rotation = (-cd1_2).atan2(cd2_2).to_degrees().rem_euclid(360.0);
        let table = |t: &Vec<Vec<f64>>| serde_json::to_string(t).expect("a table of numbers serializes");
        PlateSolveRecord {
            id: None,
            frame_id: 0,
            crpix1: self.crpix1 - 1.0,
            crpix2: self.crpix2 - 1.0,
            crval1: self.crval1,
            crval2: self.crval2,
            cd1_1,
            cd1_2,
            cd2_1,
            cd2_2,
            sip_order: self.sip.as_ref().map(|s| s.order),
            sip_a_coeffs: self.sip.as_ref().map(|s| table(&s.a)),
            sip_b_coeffs: self.sip.as_ref().map(|s| table(&s.b)),
            sip_ap_coeffs: self.sip.as_ref().and_then(|s| s.ap.as_ref().map(table)),
            sip_bp_coeffs: self.sip.as_ref().and_then(|s| s.bp.as_ref().map(table)),
            matched_stars: 0,
            total_detected: 0,
            rms_residual_px: 0.0,
            rms_residual_arcsec: 0.0,
            pixel_scale_arcsec: scale,
            field_rotation_deg: rotation,
            solve_time_ms: 0,
            catalog_used: "manifest".to_string(),
            algorithm_used: "manifest".to_string(),
            solved_at: String::new(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        }
    }
}

/// `APTDIA` (mm) from the frame's stored header blob (spec §8: `frames` has
/// no aperture column). The first `fits_header` row that carries one wins.
fn aperture_mm_of_frame(conn: &Connection, frame_id: i64) -> Result<Option<f64>> {
    let mut stmt = conn.prepare(
        "SELECT fi.format, fh.header FROM frames f
         JOIN files fi ON fi.id = f.file_id
         JOIN fits_header fh ON fh.file_id = f.file_id
         WHERE f.id = ?1 ORDER BY fh.id",
    )?;
    let rows = stmt.query_map([frame_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (format, header) = row?;
        if let Some(a) =
            crate::fits_parser::stored_header::aperture_mm_from_stored_header(&format, &header)
        {
            return Ok(Some(a));
        }
    }
    Ok(None)
}
```
In `build_frame_meta`, right after the `focalLen` insert:
```rust
    if let Some(a) = aperture_mm_of_frame(conn, frame_id)? {
        meta.insert("aperture".into(), json!(a));
    }
```
Import `crate::plate_solve::storage::PlateSolveRecord` (a non-test `use`, beside `get_plate_solve`). Note that `PlateSolveRecord` derives no `PartialEq`, so `ManifestWcs` stays the comparable form.

- [ ] **Step 4: Run the tests and watch them pass.** Run the Step 2 command; expected PASS, including the three existing `frame_meta` tests and the 8192-byte cap.

- [ ] **Step 5: Run the gate.** TS must not move.

- [ ] **Step 6: Commit** with the message `feat(collab): parse a project frame's manifest meta; publish meta.aperture`, plus the trailers.

---

### Task 5: The project config store, the stackable predicate, the `_projects` layout (C5, C6, spec §7, §9.3, §9.4)

**Files:**
- Modify `crates/athenaeum-core/src/db/stacking.rs`: add `ProjectConfigRow`, `get_project_config`, `set_project_config` and `seed_project_config`, plus tests.
- Modify `crates/athenaeum-core/src/stacking/config.rs`: add `project_seed_config`.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: add `resolve_project_config_and_exclusions`, plus tests.
- Modify `crates/athenaeum-core/src/db/collab_frames.rs`:
  - `SELECT_COLS`, `LocalFrameRow` and `row_from_sql`;
  - add `part_of_project`, `exportable`, `NotStackable` and `stackable`;
  - tests.
- Modify every `LocalFrameRow { … }` literal to add `own_staged: false`. The sites are in:
  - `api/collab_exchange.rs`;
  - `api/collab_live/{test_support,executor,storage_task}.rs`;
  - `api/collab.rs` (`~7052` in production, and two test sites);
  - `export/project_collector.rs`;
  - `scanner/mod.rs`;
  - `db/collab_frames.rs` tests.

  Run `cargo check -p athenaeum-core --all-targets` for the exact list.
- Modify `crates/athenaeum-core/src/export/project_collector.rs`: its private `exportable` is deleted, and it calls `crate::db::collab_frames::exportable`.
- Modify `crates/athenaeum-core/src/stacking/paths.rs`:
  - `subject_layout` (`~34-60`);
  - add `PROJECTS_DIR` and `project_root_overlap`;
  - the test at `~1888`.
- Modify `crates/athenaeum-core/src/stacking/run.rs`: the tests at `~19936` and `~19986` change `"projects"` to `crate::stacking::paths::PROJECTS_DIR`.

**Interfaces:**
```rust
// db/stacking.rs (ungated)
pub struct ProjectConfigRow { pub config_json: String, pub excluded_frame_uuids: Vec<String>, pub reference_frame_uuid: Option<String>, pub updated_at: String }
pub fn get_project_config(conn: &Connection, project_id: &str) -> Result<Option<ProjectConfigRow>>;
pub fn set_project_config(conn: &Connection, project_id: &str, config_json: &str, excluded_frame_uuids: &[String], reference_frame_uuid: Option<&str>) -> Result<()>;
pub fn seed_project_config(conn: &Connection, project_id: &str, seed_json: &str) -> Result<ProjectConfigRow>;
// stacking/config.rs
pub fn project_seed_config(global: StackingConfig) -> StackingConfig;
// stacking/plan.rs
pub fn resolve_project_config_and_exclusions(conn: &Connection, settings: &SettingsManager, project_id: &str, config_override: Option<StackingConfig>) -> Result<(StackingConfig, Vec<String>), ApiError>;
// db/collab_frames.rs (ungated)
pub struct LocalFrameRow { /* … */ pub own_staged: bool }
pub fn part_of_project(r: &LocalFrameRow) -> bool;
pub fn exportable(r: &LocalFrameRow) -> bool;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum NotStackable { NotInProject, NotOnDevice, BeingRestaged, WaitingForNewVersion }
impl NotStackable { pub fn reason(self) -> &'static str }
pub fn stackable(r: &LocalFrameRow) -> Result<(), NotStackable>;
// stacking/paths.rs
pub const PROJECTS_DIR: &str = "_projects";
pub fn project_root_overlap(conn: &Connection, project_root: &Path) -> Result<Option<String>, ApiError>;
```
`resolve_project_config_and_exclusions` and `project_root_overlap` are `pub` (in `pub mod`s) so that the lib build carries no dead-code warning before Task 7 calls them.

- [ ] **Step 1: Write the failing tests**

`db/stacking.rs`:
```rust
    /// Plan 6b-2 C10: the seed is stored once; a second seed (another first
    /// plan racing) reads the stored document back and never overwrites it.
    #[test]
    fn a_project_config_is_seeded_once_and_never_overwritten() {
        let c = conn();
        insert_test_project(&c, "p1");
        assert!(get_project_config(&c, "p1").unwrap().is_none());
        let first = seed_project_config(&c, "p1", r#"{"version":1}"#).unwrap();
        assert_eq!(first.config_json, r#"{"version":1}"#);
        assert!(first.excluded_frame_uuids.is_empty());
        let again = seed_project_config(&c, "p1", r#"{"version":2}"#).unwrap();
        assert_eq!(again.config_json, r#"{"version":1}"#, "a stored document wins");
        set_project_config(&c, "p1", "{}", &["u1".to_string()], Some("u2")).unwrap();
        let row = get_project_config(&c, "p1").unwrap().unwrap();
        assert_eq!(row.config_json, "{}");
        assert_eq!(row.excluded_frame_uuids, vec!["u1".to_string()]);
        assert_eq!(row.reference_frame_uuid.as_deref(), Some("u2"));
    }
```
`plan.rs`:
```rust
    /// Spec §9.4 / plan 6b-2 C10: the first plan of a project stores the
    /// global default with `stops` and the coverage map on; a stored document
    /// is whole-config; an override wins without being stored.
    #[test]
    fn a_project_config_is_seeded_then_resolved_whole() {
        use crate::stacking::config::ExposureMode;
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let settings = SettingsManager::new();
        let (cfg, excluded) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", None).unwrap();
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::Stops);
        assert!(cfg.output.coverage_map);
        assert!(excluded.is_empty());
        let stored = crate::db::stacking::get_project_config(&f.conn, "p1").unwrap().unwrap();
        let stored_cfg: StackingConfig = serde_json::from_str(&stored.config_json).unwrap();
        assert_eq!(stored_cfg, cfg, "the seed is what was stored");

        crate::db::stacking::set_project_config(
            &f.conn,
            "p1",
            r#"{"grouping":{"exposureMode":"none"}}"#,
            &["u7".to_string()],
            None,
        )
        .unwrap();
        let (cfg, excluded) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", None).unwrap();
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::NoSplit);
        assert!(!cfg.output.coverage_map, "whole-config: no field-level merge with the seed");
        assert_eq!(excluded, vec!["u7".to_string()]);

        let mut over = StackingConfig::default();
        over.output.crop_to_coverage = 5;
        let (cfg, _) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", Some(over.clone()))
                .unwrap();
        assert_eq!(cfg, over);
        let stored = crate::db::stacking::get_project_config(&f.conn, "p1").unwrap().unwrap();
        assert_eq!(stored.config_json, r#"{"grouping":{"exposureMode":"none"}}"#, "an override is never stored");
    }
```
`db/collab_frames.rs` (the module's `conn()` seeds project `p1`; `view(uuid, mv)` is a published, accepted replica row):
```rust
    /// Plan 6b-2 C5: stackable = exportable ∧ servable ∧ ¬own_staged, and a
    /// recorded `size:mtime` — one case per leg.
    #[test]
    fn stackable_is_exportable_servable_not_restaged_and_stamped() {
        let c = conn();
        let land = |uuid: &str| set_landed(&c, "p1", uuid, &format!("/c/{uuid}.fits"), "100:1").unwrap();
        let verdict = |uuid: &str| stackable(&get(&c, "p1", uuid).unwrap().unwrap());

        upsert_from_manifest(&c, "p1", &view("held", 1)).unwrap();
        land("held");
        assert_eq!(verdict("held"), Ok(()));

        upsert_from_manifest(&c, "p1", &view("wanted", 2)).unwrap();
        assert_eq!(verdict("wanted"), Err(NotStackable::NotOnDevice));

        let mut excluded = view("excluded", 3);
        excluded.accepted = false;
        upsert_from_manifest(&c, "p1", &excluded).unwrap();
        land("excluded");
        assert_eq!(verdict("excluded"), Err(NotStackable::NotInProject));

        let mut pending = view("pending", 4);
        pending.own = true;
        pending.state = "pending".into();
        upsert_from_manifest(&c, "p1", &pending).unwrap();
        land("pending");
        assert_eq!(verdict("pending"), Err(NotStackable::NotInProject));

        let mut own = view("own", 5);
        own.own = true;
        upsert_from_manifest(&c, "p1", &own).unwrap();
        land("own");
        assert_eq!(verdict("own"), Ok(()), "own_held is stackable");
        c.execute("UPDATE project_frames_local SET own_staged = 1 WHERE frame_uuid = 'own'", []).unwrap();
        assert!(get(&c, "p1", "own").unwrap().unwrap().own_staged);
        assert_eq!(verdict("own"), Err(NotStackable::BeingRestaged));

        upsert_from_manifest(&c, "p1", &view("unstamped", 6)).unwrap();
        land("unstamped");
        c.execute(
            "UPDATE project_frames_local SET size_mtime_seen = NULL WHERE frame_uuid = 'unstamped'",
            [],
        )
        .unwrap();
        assert_eq!(verdict("unstamped"), Err(NotStackable::WaitingForNewVersion));

        upsert_from_manifest(&c, "p1", &view("gc", 7)).unwrap();
        land("gc");
        c.execute("UPDATE project_frames_local SET awaiting_gc = 1 WHERE frame_uuid = 'gc'", []).unwrap();
        assert_eq!(verdict("gc"), Err(NotStackable::NotOnDevice));

        assert_eq!(NotStackable::BeingRestaged.reason(), "being re-staged");
        assert!(exportable(&get(&c, "p1", "own").unwrap().unwrap()), "export ignores own_staged");
    }
```
`paths.rs`. Replace the assertion in `subject_layout_puts_a_project_under_projects_slug` with the following, and rename the test to `subject_layout_puts_a_project_under_the_projects_folder`:
```rust
        assert_eq!(
            project_layout.root,
            working.join(PROJECTS_DIR).join("orion-mosaic")
        );
```
Add:
```rust
    /// Plan 6b-2 C6: no frame-set slug can start with `_`, so no frame set
    /// can ever lay out at (or under) the projects folder.
    #[test]
    fn no_frame_set_slug_reaches_the_projects_folder() {
        for name in [
            "_projects", "__projects", "_", " _x", "projects", "Projects", "_ _", "/_projects",
            "..", "_projects_", "CON", "  _  ", "\t_tab", ":_colon", "_.", "._a",
        ] {
            let slug = crate::stacking::groups::set_slug(name);
            assert!(!slug.starts_with('_'), "{name:?} -> {slug:?}");
            assert_ne!(slug, PROJECTS_DIR);
        }
    }

    /// Plan 6b-2 C6: a project working root inside (or containing) any scan
    /// root — the Collaboration root is one — is refused with a sentence.
    #[test]
    fn a_project_root_overlapping_a_scan_root_is_reported() {
        let c = conn();
        let tmp = tempfile::tempdir().unwrap();
        let collab = tmp.path().join("work");
        std::fs::create_dir_all(&collab).unwrap();
        c.execute(
            "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
            [collab.to_str().unwrap()],
        )
        .unwrap();
        let inside = collab.join(PROJECTS_DIR).join("m31");
        let sentence = project_root_overlap(&c, &inside).unwrap().expect("overlap");
        assert!(sentence.contains("overlaps a monitored folder"), "{sentence}");
        let apart = tmp.path().join("elsewhere").join(PROJECTS_DIR).join("m31");
        assert_eq!(project_root_overlap(&c, &apart).unwrap(), None);
    }
```
`run.rs`: in `the_heal_marks_an_interrupted_project_run_failed_and_leaves_frame_sets_alone` and `work_usage_and_cleanup_of_a_frame_set_ignore_project_rows`, `.join("projects")` becomes `.join(crate::stacking::paths::PROJECTS_DIR)`. This is the mechanical edit C6 names.

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 a_project_config_is stackable_is no_frame_set_slug a_project_root_overlapping subject_layout`

Expected: the build fails, because the new items do not exist.

- [ ] **Step 3: Implement**

`db/stacking.rs`, next to `get_set_config`:
```rust
/// One project's stored stacking document (spec §9.1/§9.4): the whole
/// config, the manual exclusions and the manual reference, both by frame
/// uuid — a handle is minted only when a plan includes the frame.
pub struct ProjectConfigRow {
    pub config_json: String,
    pub excluded_frame_uuids: Vec<String>,
    pub reference_frame_uuid: Option<String>,
    pub updated_at: String,
}

pub fn get_project_config(conn: &Connection, project_id: &str) -> Result<Option<ProjectConfigRow>> {
    let row = conn
        .query_row(
            "SELECT config_json, excluded_frame_uuids_json, reference_frame_uuid, updated_at
             FROM stacking_project_config WHERE project_id = ?1",
            params![project_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((config_json, excluded_json, reference_frame_uuid, updated_at)) => {
            let excluded_frame_uuids: Vec<String> = serde_json::from_str(&excluded_json)?;
            Ok(Some(ProjectConfigRow {
                config_json,
                excluded_frame_uuids,
                reference_frame_uuid,
                updated_at,
            }))
        }
    }
}

pub fn set_project_config(
    conn: &Connection,
    project_id: &str,
    config_json: &str,
    excluded_frame_uuids: &[String],
    reference_frame_uuid: Option<&str>,
) -> Result<()> {
    let excluded_json = serde_json::to_string(excluded_frame_uuids)?;
    conn.execute(
        "INSERT INTO stacking_project_config
           (project_id, config_json, excluded_frame_uuids_json, reference_frame_uuid, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id) DO UPDATE SET
           config_json = excluded.config_json,
           excluded_frame_uuids_json = excluded.excluded_frame_uuids_json,
           reference_frame_uuid = excluded.reference_frame_uuid,
           updated_at = excluded.updated_at",
        params![project_id, config_json, excluded_json, reference_frame_uuid, now()],
    )?;
    Ok(())
}

/// The project's stored document, seeding `seed_json` first when it has
/// none (spec §9.4: the first plan stores the seeded default). One IMMEDIATE
/// transaction (read-then-write, CLAUDE.md), so two first plans racing on
/// one project store one document and both read it back; a stored document
/// is never overwritten. Never call with a transaction already open on
/// `conn`.
pub fn seed_project_config(conn: &Connection, project_id: &str, seed_json: &str) -> Result<ProjectConfigRow> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO stacking_project_config (project_id, config_json, updated_at)
         VALUES (?1, ?2, ?3) ON CONFLICT(project_id) DO NOTHING",
        params![project_id, seed_json, now()],
    )?;
    let row = get_project_config(&tx, project_id)?.ok_or_else(|| {
        anyhow::anyhow!("project {project_id} has no stacking config after seeding")
    })?;
    tx.commit()?;
    Ok(row)
}
```
`config.rs`:
```rust
/// The document a project's first plan stores (spec §9.4): the global
/// default with `grouping.exposureMode = stops` and `output.coverageMap = true`.
pub fn project_seed_config(global: StackingConfig) -> StackingConfig {
    let mut config = global;
    config.grouping.exposure_mode = ExposureMode::Stops;
    config.output.coverage_map = true;
    config
}
```
`plan.rs`, next to `resolve_config_and_exclusions`:
```rust
/// A project's effective config and manual exclusions (frame uuids) — plan
/// 6b-2 C10, spec §9.4. Seeds the stored document on the first plan (a
/// documented exception to "the plan reads only"; `seed_project_config` is
/// one IMMEDIATE transaction), then resolves whole-config like a frame set:
/// an override wins outright and is never stored, else the stored document
/// IS the config. `pub` so the lib build carries it before the project plan
/// arm (Task 7) and 6b-3's `get_project_stacking_config` call it.
pub fn resolve_project_config_and_exclusions(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
    config_override: Option<StackingConfig>,
) -> Result<(StackingConfig, Vec<String>), ApiError> {
    let global_json = read_global_config_json(conn, settings);
    let global = resolve_config(None, global_json.as_deref())
        .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?;
    let seed = serde_json::to_string(&crate::stacking::config::project_seed_config(global))
        .map_err(|e| ApiError::Internal(format!("failed to serialize the project seed config: {e}")))?;
    let row = crate::db::stacking::seed_project_config(conn, project_id, &seed)?;
    let config = match config_override {
        Some(cfg) => cfg,
        None => resolve_config(Some(&row.config_json), None)
            .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?,
    };
    Ok((config, row.excluded_frame_uuids))
}
```
`db/collab_frames.rs`:
- `SELECT_COLS` gains `, own_staged` at its end.
- `LocalFrameRow` gains:
```rust
    /// Own only (plan 6b-2 C5): set between `stage_own_file` and
    /// `set_own_version` — the row's bytes are the new version's while its
    /// `blake3` is still the old one's. Read here, never written by
    /// `record_own` (its `INSERT OR REPLACE` resets it, as before).
    pub own_staged: bool,
```
- `row_from_sql` gains `own_staged: row.get::<_, i64>(25)? != 0,`.
- Add:
```rust
/// Is a frame part of the project's published, accepted set (R31)? An own
/// pending frame is not; a coordinator-excluded one is not.
pub fn part_of_project(r: &LocalFrameRow) -> bool {
    r.state == "published" && r.accepted
}

/// Is a frame part of the project export (R31)? Published and accepted, on
/// disk at a recorded path, and not waiting for store GC. Moved here from
/// `export::project_collector` so export and stacking share one definition.
pub fn exportable(r: &LocalFrameRow) -> bool {
    part_of_project(r) && r.on_disk && !r.awaiting_gc && r.landed_path.is_some()
}

/// Why a project frame cannot be stacked on this device (plan 6b-2 C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStackable {
    NotInProject,
    NotOnDevice,
    BeingRestaged,
    WaitingForNewVersion,
}

impl NotStackable {
    pub fn reason(self) -> &'static str {
        match self {
            NotStackable::NotInProject => "not published or excluded",
            NotStackable::NotOnDevice => "not on this device",
            NotStackable::BeingRestaged => "being re-staged",
            NotStackable::WaitingForNewVersion => "waiting for its new version",
        }
    }
}

/// Can a stacking run read this frame (plan 6b-2 C5, spec §7)? Exportable,
/// servable (`held | own_held` — kept equal to `on_disk` by every writer,
/// belt and braces against a stale row), not an own row being re-staged
/// (its bytes changed under the old `blake3`), and with a recorded
/// `size:mtime` — a NULL stamp means the landed file is not verified as
/// this version (a version bump clears it), and the run's identity check
/// (`stacking::origin::landed_unchanged`) needs it as its baseline.
pub fn stackable(r: &LocalFrameRow) -> Result<(), NotStackable> {
    if !part_of_project(r) {
        return Err(NotStackable::NotInProject);
    }
    if !exportable(r) || !r.local_state.servable() {
        return Err(NotStackable::NotOnDevice);
    }
    if r.own_staged {
        return Err(NotStackable::BeingRestaged);
    }
    if r.size_mtime_seen.is_none() {
        return Err(NotStackable::WaitingForNewVersion);
    }
    Ok(())
}
```
- `export/project_collector.rs`: delete the private `fn exportable`, and call `crate::db::collab_frames::exportable(&row)` where it was called. Its tests must pass unchanged.

`paths.rs`:
```rust
/// The folder every project's working tree lives under (plan 6b-2 C6):
/// `<working>/_projects/<slug>`. No frame-set slug can start with `_`
/// (`sanitize_for_filename` trims it), so a frame set never lays out here.
pub const PROJECTS_DIR: &str = "_projects";
```
In `subject_layout`'s project arm, replace `&working_dir.join("projects")` with `&working_dir.join(PROJECTS_DIR)`, and update the function's doc to name `<working>/_projects/<set_slug(slug)>`. Then add:
```rust
/// Plan 6b-2 C6: a project's working root must not overlap any scan root —
/// the Collaboration root is one — because cleanup removes whole subtrees
/// under it, and a landed replica must never be in one. `Some(sentence)` is
/// the plan's `folders` blocker. Reuses the transfer folders' overlap check
/// (`api::scan_roots::check_scan_root_overlap`); reads the filesystem only
/// to canonicalize the scan roots.
pub fn project_root_overlap(conn: &Connection, project_root: &Path) -> Result<Option<String>, ApiError> {
    let root = crate::api::scan_roots::normalize_path(&canonical_or_nearest(project_root));
    match crate::api::scan_roots::check_scan_root_overlap(conn, &root) {
        Ok(()) => Ok(None),
        Err(ApiError::Conflict(msg)) => {
            tracing::warn!(path = %root.display(), error = %msg, "project working folder overlaps a scan root");
            Ok(Some(format!(
                "The project's working folder {} overlaps a monitored folder ({msg}); choose a working folder outside every scan root and the Collaboration folder",
                root.display()
            )))
        }
        Err(other) => Err(other),
    }
}

/// `path` with its nearest existing ancestor canonicalized and the missing
/// tail re-appended — a project root usually does not exist yet, while the
/// scan roots it is compared against are canonicalized (`/var` vs
/// `/private/var` on macOS would otherwise never match).
fn canonical_or_nearest(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
    let mut out = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    for name in tail.iter().rev() {
        out.push(name);
    }
    out
}
```

- [ ] **Step 4: Fix the `LocalFrameRow` literals.** Add `own_staged: false,` to every literal `cargo check -p athenaeum-core --all-targets` names. No other change.

- [ ] **Step 5: Run the tests and watch them pass.** Run the Step 2 command. Then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 db::collab_frames export::project_collector stacking::paths stacking::run::tests::the_heal_marks stacking::run::tests::work_usage_and_cleanup`. Expected: all PASS.

- [ ] **Step 6: Run the gate.** TS must not move.

- [ ] **Step 7: Commit** with the message `feat(stacking): project config store and seed, the stackable predicate, the _projects layout`, plus the trailers.

---

### Task 6a: `FrameOrigin::Project` and the project frame source; the funnel test (C7, C8; spec §4.3, §5.1, §7)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/origin.rs`:
  - the enum;
  - add `cfa_geometry_of_file`;
  - the project arms of `stored_solve` and `cfa_geometry`;
  - tests, including the funnel grep test.
- Create `crates/athenaeum-core/src/stacking/project_source.rs`, and declare `pub mod project_source;` in `stacking/mod.rs`.
- Modify `crates/athenaeum-core/src/stacking/groups.rs`: `frame_source` (`~605-620`), and drop the `PROJECT_STACKING_UNAVAILABLE` import.
- Modify `crates/athenaeum-core/src/stacking/config.rs`: add `PREPARE_VERSION` and `prepare_subtree`.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`:
  - `prepare_hash_for_project`;
  - `HashMemo::hash_by_origin` (`~380-391`);
  - `resolve_reference`'s file row (`~1006-1016`);
  - `last_run_reference_scale` (`~1068`) and `cleanup_stale_note` (`~1203`) take `&StackSubject`, with their two callers in `build_plan`;
  - `frame_source` callers: `build_plan` (`~1687`) and tests (`~6540`, `~6575`, `~6593`);
  - tests.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `resolve_calibrate_job`'s origin match (`~1689`);
  - `start_stacking`'s `frame_source` call (`~813`);
  - `test_context`'s origin literal (`~9883`);
  - the `solve_of` test literal (`~20082`).
- Modify `crates/athenaeum-core/src/db/stacking.rs`: `project_frame_handles` (`~1034`) returns at once for an empty list (review M2), plus a test.
- Modify `crates/athenaeum-core/src/stacking/test_fixtures.rs`: add the project frame fixture.
- Modify `crates/athenaeum-core/src/fits_parser/mod.rs`: `read_xisf_header` becomes `pub(crate)`. This is a visibility change only.

**Interfaces:**
```rust
// stacking/origin.rs
pub enum FrameOrigin {
    Catalog { file_id: i64, attested: bool },
    Project { project_id: String, frame_uuid: String, publisher: String, landed_path: String,
              blake3: String, byte_size: i64, size_mtime: String, meta: ProjectFrameMeta },
}
pub(crate) fn cfa_geometry_of_file(path: &Path) -> anyhow::Result<Option<CfaGeometry>>;
// stacking/config.rs
pub const PREPARE_VERSION: u32 = 1;
pub fn prepare_subtree(cfg: &StackingConfig) -> serde_json::Value;
// stacking/plan.rs
pub(crate) fn prepare_hash_for_project(cfg: &StackingConfig, blake3: &str, byte_size: i64) -> String;
fn last_run_reference_scale(conn: &Connection, subject: &StackSubject, groups: &[IntegrationGroup]) -> Result<Option<f64>, ApiError>;
fn cleanup_stale_note(conn: &Connection, subject: &StackSubject, plan_groups: &[PlanGroup], stale_stages: &[Stage]) -> Result<Option<String>, ApiError>;
// stacking/groups.rs
pub(crate) fn frame_source(conn: &Connection, subject: &StackSubject, cfg: &StackingConfig) -> Result<(Vec<IntegrationGroup>, FrameOrigins), ApiError>;
// stacking/project_source.rs (pub mod; pub items, so later tasks read them without dead-code noise)
pub struct SourceFrame { pub handle: Option<i64>, pub frame_uuid: String, pub file_name: String, pub publisher: String,
    pub filter_canonical: String, pub exptime_sec: f64, pub color_mode: ColorMode, pub meta: Option<ProjectFrameMeta>, pub not_stackable: Option<String> }
pub struct ProjectSource { pub groups: Vec<IntegrationGroup>, pub origins: FrameOrigins, pub frames: Vec<SourceFrame> }
pub fn project_frame_source(conn: &Connection, project_id: &str, cfg: &StackingConfig) -> Result<ProjectSource, ApiError>;
pub fn project_group_key(color_token: &str, canonical: &str, class_token: &str) -> String;
pub fn file_shape_quick(path: &Path) -> Option<(usize, usize, usize)>;
pub fn is_xisf(path: &Path) -> bool;
pub fn xisf_geometry(xml: &str) -> Option<(usize, usize, usize)>;
// stacking/test_fixtures.rs
pub(crate) enum ProjectFile { Mono32, Rgb32, Mosaic32, Mono16, Xisf32, Missing }
pub(crate) struct ProjectFrameSpec<'a> { pub uuid: &'a str, pub publisher: &'a str, pub filter: &'a str, pub exptime_sec: f64,
    pub date_obs: &'a str, pub width: usize, pub height: usize, pub file: ProjectFile, pub bayerpat: Option<&'a str>,
    pub pixel_scale_arcsec: Option<f64>, pub wcs: Option<serde_json::Value>, pub focal_len: Option<f64>, pub aperture: Option<f64>, pub accepted: bool }
pub(crate) fn project_frame_spec(uuid: &str) -> ProjectFrameSpec<'_>;
pub(crate) fn manifest_wcs_json(width: usize, height: usize, ra: f64, dec: f64, scale_arcsec: f64) -> serde_json::Value;
pub(crate) fn add_project_frame(conn: &Connection, dir: &Path, project_id: &str, spec: &ProjectFrameSpec<'_>) -> PathBuf;
```
Consumed by: Task 6b (the origin, the fixture), Task 7 (`ProjectSource.frames`, `file_shape_quick`), Tasks 9a and 9b (`cfa_geometry_of_file`, the fixture), Task 10b (the origin's `meta`).

- [ ] **Step 1: The project fixture** (`test_fixtures.rs`). Add `write_xisf_f32` to the existing `crate::fits_writer` import (`write_fits_f32` is already imported at `test_fixtures.rs:19`, so a second `use` of it is E0252; review M1), add `use crate::collab::hub_client::FrameViewWire;`, then append:
```rust
// ── Project frames (plan 6b-2) ──────────────────────────────────────────────

/// What lands on disk for one fixture project frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectFile {
    /// One-plane float32 FITS in the unit domain — a calibrated replica.
    Mono32,
    /// Three-plane float32 FITS — a debayered OSC replica.
    Rgb32,
    /// One-plane float32 CFA mosaic carrying BAYERPAT/XBAYROFF/YBAYROFF.
    Mosaic32,
    /// One-plane 16-bit FITS in ADU (BZERO 32768) — an attested original.
    Mono16,
    /// One-plane float32 XISF in the unit domain.
    Xisf32,
    /// Nothing on disk: the manifest row is never landed.
    Missing,
}

/// One project frame to seed via [`add_project_frame`].
pub(crate) struct ProjectFrameSpec<'a> {
    pub uuid: &'a str,
    pub publisher: &'a str,
    pub filter: &'a str,
    pub exptime_sec: f64,
    pub date_obs: &'a str,
    pub width: usize,
    pub height: usize,
    pub file: ProjectFile,
    pub bayerpat: Option<&'a str>,
    pub pixel_scale_arcsec: Option<f64>,
    pub wcs: Option<serde_json::Value>,
    pub focal_len: Option<f64>,
    pub aperture: Option<f64>,
    pub accepted: bool,
}

/// A mono, landed, accepted 64×48 frame: filter `L`, 300 s, 1.5″/px, no
/// WCS. A case overrides what it needs.
pub(crate) fn project_frame_spec(uuid: &str) -> ProjectFrameSpec<'_> {
    ProjectFrameSpec {
        uuid,
        publisher: "ann",
        filter: "L",
        exptime_sec: 300.0,
        date_obs: "2026-01-10T21:00:00",
        width: 64,
        height: 48,
        file: ProjectFile::Mono32,
        bayerpat: None,
        pixel_scale_arcsec: Some(1.5),
        wcs: None,
        focal_len: None,
        aperture: None,
        accepted: true,
    }
}

/// `meta.wcs` of a `width × height` frame centred on `(ra, dec)` at
/// `scale_arcsec` ″/px, unrotated, in the parity every solve in the owner's
/// catalog has (`det CD > 0`); `crpix` 1-BASED, as the manifest stores it.
pub(crate) fn manifest_wcs_json(
    width: usize,
    height: usize,
    ra: f64,
    dec: f64,
    scale_arcsec: f64,
) -> serde_json::Value {
    let s = scale_arcsec / 3600.0;
    serde_json::json!({
        "crval1": ra,
        "crval2": dec,
        "crpix1": (width as f64 + 1.0) / 2.0,
        "crpix2": (height as f64 + 1.0) / 2.0,
        "cd": [s, 0.0, 0.0, s],
    })
}

/// Seed one project frame the way the exchange would leave it.
/// - The manifest row goes through the real applier (`upsert_from_manifest`).
/// - The file is written under `dir/<publisher>/`, the landing's own layout. It
///   carries a real replica's cards, `CALSTAT`/`ATH_CSRC`/`ATH_PRJ` included, so
///   a test can prove nothing the run writes copies them.
/// - Unless the file is `Missing`, the frame is landed (`set_landed`, with the
///   file's real `size:mtime`).
/// Returns the landed path, or where the file would have landed. The project row
/// must exist (`db::stacking::insert_test_project`).
pub(crate) fn add_project_frame(
    conn: &Connection,
    dir: &Path,
    project_id: &str,
    spec: &ProjectFrameSpec<'_>,
) -> PathBuf {
    let ext = if spec.file == ProjectFile::Xisf32 { "xisf" } else { "fits" };
    let file_name = format!("c_{}.{ext}", spec.uuid);
    let folder = dir.join(spec.publisher);
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(&file_name);
    let (w, h) = (spec.width, spec.height);
    let stars = [
        (w as f64 * 0.3, h as f64 * 0.35, 0.5),
        (w as f64 * 0.62, h as f64 * 0.55, 0.8),
        (w as f64 * 0.45, h as f64 * 0.72, 0.3),
    ];
    let plane = synthetic_star_field(w, h, &stars, 3.0, 0.0, 0);
    let mut cards = vec![
        Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
        Card::new("FILTER", CardValue::Str(spec.filter.to_string())).unwrap(),
        Card::new("EXPTIME", CardValue::Real(spec.exptime_sec)).unwrap(),
        Card::new("DATE-OBS", CardValue::Str(spec.date_obs.to_string())).unwrap(),
        Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
        Card::new("ROWORDER", CardValue::Str(FIXTURE_ROW_ORDER.into())).unwrap(),
        Card::new("CALSTAT", CardValue::Str("BDF".into())).unwrap(),
        Card::new("ATH_CSRC", CardValue::Str(spec.uuid.to_string())).unwrap(),
        Card::new("ATH_PRJ", CardValue::Str(project_id.to_string())).unwrap(),
    ];
    match spec.file {
        ProjectFile::Mono32 => write_fits_f32(&path, w, h, 1, &plane, &cards).unwrap(),
        ProjectFile::Rgb32 => {
            let rgb: Vec<f32> = [1.0f32, 0.8, 0.6]
                .iter()
                .flat_map(|k| plane.iter().map(move |v| v * k))
                .collect();
            write_fits_f32(&path, w, h, 3, &rgb, &cards).unwrap();
        }
        ProjectFile::Mosaic32 => {
            cards.push(
                Card::new("BAYERPAT", CardValue::Str(spec.bayerpat.unwrap_or("RGGB").into()))
                    .unwrap(),
            );
            cards.push(Card::new("XBAYROFF", CardValue::Integer(0)).unwrap());
            cards.push(Card::new("YBAYROFF", CardValue::Integer(0)).unwrap());
            write_fits_f32(&path, w, h, 1, &plane, &cards).unwrap();
        }
        ProjectFile::Mono16 => {
            let data: Vec<i16> = plane
                .iter()
                .map(|&v| ((v * 65535.0).round().clamp(0.0, 65535.0) as i32 - 32768) as i16)
                .collect();
            write_fits_i16(&path, w, h, &data, &cards);
        }
        ProjectFile::Xisf32 => write_xisf_f32(&path, w, h, 1, &plane, &cards).unwrap(),
        ProjectFile::Missing => {}
    }

    let mut meta = serde_json::Map::new();
    meta.insert("naxis1".into(), serde_json::json!(w));
    meta.insert("naxis2".into(), serde_json::json!(h));
    meta.insert("instrume".into(), serde_json::json!("cam"));
    if let Some(s) = spec.pixel_scale_arcsec {
        meta.insert("pixelScaleArcsec".into(), serde_json::json!(s));
    }
    if let Some(b) = spec.bayerpat {
        meta.insert("bayerpat".into(), serde_json::json!(b));
    }
    if let Some(v) = spec.focal_len {
        meta.insert("focalLen".into(), serde_json::json!(v));
    }
    if let Some(v) = spec.aperture {
        meta.insert("aperture".into(), serde_json::json!(v));
    }
    if let Some(wcs) = &spec.wcs {
        meta.insert("wcs".into(), wcs.clone());
    }
    let (byte_size, blake3) = if spec.file == ProjectFile::Missing {
        (1000i64, "b".repeat(64))
    } else {
        let bytes = std::fs::read(&path).unwrap();
        (bytes.len() as i64, blake3::hash(&bytes).to_hex().to_string())
    };
    let seq: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_frames_local", [], |r| r.get(0))
        .unwrap();
    let view: FrameViewWire = serde_json::from_value(serde_json::json!({
        "frameUuid": spec.uuid, "frameSeq": seq + 1,
        "publisherAccountId": spec.publisher, "publisherDisplayName": spec.publisher, "own": false,
        "fileName": file_name, "contentVersion": 1, "blake3": blake3, "byteSize": byte_size,
        "xxh3": "0123456789abcdef", "filterRaw": spec.filter, "filterCanonical": spec.filter,
        "channel": if spec.bayerpat.is_some() { "osc" } else { "mono" },
        "exptimeSec": spec.exptime_sec, "dateObs": spec.date_obs,
        "meta": serde_json::Value::Object(meta), "gateVersion": 0,
        "accepted": spec.accepted, "acceptedReason": null, "state": "published",
        "rejectReason": null, "manifestVersion": seq + 1, "createdAt": "2026-10-01T00:00:00Z",
    }))
    .unwrap();
    crate::db::collab_frames::upsert_from_manifest(conn, project_id, &view).unwrap();
    if spec.file != ProjectFile::Missing {
        let stamp =
            crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(&path).unwrap()).encode();
        crate::db::collab_frames::set_landed(conn, project_id, spec.uuid, path.to_str().unwrap(), &stamp)
            .unwrap();
    }
    path
}
```

- [ ] **Step 2: Write the failing tests**

`stacking/project_source.rs` `#[cfg(test)] mod tests`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::stacking::insert_test_project;
    use crate::stacking::groups::ScaleSource;
    use crate::stacking::test_fixtures::{
        add_project_frame, manifest_wcs_json, project_frame_spec, ProjectFile,
    };

    fn test_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::SqliteConnectionManager::setup_connection(&c).unwrap();
        crate::db::schema::init_db(&c).unwrap();
        c
    }

    /// Spec §15.1 "Seam", Review Focus 2: catalog `frames` / `plate_solves`
    /// rows whose ids equal the handles, with different data, are never
    /// read — every field comes from the manifest and the landed file.
    #[test]
    fn the_project_frame_source_reads_the_manifest_never_the_catalog() {
        let conn = test_conn();
        insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        for uuid in ["r1", "r2", "r3"] {
            let mut spec = project_frame_spec(uuid);
            spec.filter = "R";
            spec.wcs = Some(manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
            add_project_frame(&conn, dir.path(), "p1", &spec);
        }
        let handles =
            crate::db::stacking::project_frame_handles(&conn, "p1", &["r1", "r2", "r3"]).unwrap();
        for (i, h) in handles.values().enumerate() {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'decoy.fits', 7, '2020-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![h, format!("/decoy/{i}.fits")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, filter, exptime, naxis1, naxis2, bayerpat)
                 VALUES (?1, ?1, 'Light', 'Z', 999.0, 7, 7, 'RGGB')",
                [h],
            )
            .unwrap();
            crate::stacking::test_fixtures::seed_plate_solve_scale(&conn, *h, 9.9);
        }
        let cfg = StackingConfig::default();
        let source = project_frame_source(&conn, "p1", &cfg).unwrap();
        assert_eq!(source.groups.len(), 1, "{:?}", source.groups);
        let g = &source.groups[0];
        assert_eq!(g.key, "mono__R__300s");
        assert_eq!((g.filter.as_deref(), g.binning, g.color_mode), (Some("R"), 1, ColorMode::Mono));
        for f in &g.frames {
            assert_eq!(f.exposure_s, Some(300.0));
            assert_eq!((f.width, f.height), (64, 48));
            assert_eq!(f.pixel_scale_arcsec, Some(1.5));
            assert_eq!(f.scale_source, Some(ScaleSource::Solve));
            assert_eq!(f.file_id, 0);
            assert!(Path::new(&f.path).starts_with(dir.path().join("ann")), "{}", f.path);
            match source.origins.get(&f.frame_id).unwrap() {
                FrameOrigin::Project { project_id, meta, .. } => {
                    assert_eq!(project_id, "p1");
                    assert!(meta.wcs.is_some());
                }
                other => panic!("{other:?}"),
            }
        }
        let again = project_frame_source(&conn, "p1", &cfg).unwrap();
        let ids = |s: &ProjectSource| s.groups[0].frames.iter().map(|f| f.frame_id).collect::<Vec<_>>();
        assert_eq!(ids(&source), ids(&again), "handles are stable across plans");
    }

    /// Spec §7: only stackable frames are grouped; every published, accepted
    /// frame is reported with why it is not stacked; a coordinator-excluded
    /// frame is absent altogether (R12). A landed stamp that does not parse is
    /// corruption, never "waiting for its new version" (review I3).
    #[test]
    fn only_stackable_frames_are_grouped_and_the_rest_are_reported() {
        let conn = test_conn();
        insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("a"));
        let mut missing = project_frame_spec("b");
        missing.file = ProjectFile::Missing;
        add_project_frame(&conn, dir.path(), "p1", &missing);
        let mut excluded = project_frame_spec("c");
        excluded.accepted = false;
        add_project_frame(&conn, dir.path(), "p1", &excluded);
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("d"));
        conn.execute("UPDATE project_frames_local SET own_staged = 1 WHERE frame_uuid = 'd'", []).unwrap();
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("e"));
        conn.execute("UPDATE project_frames_local SET size_mtime_seen = 'garbage' WHERE frame_uuid = 'e'", [])
            .unwrap();

        let source = project_frame_source(&conn, "p1", &StackingConfig::default()).unwrap();
        let grouped: Vec<&str> =
            source.groups.iter().flat_map(|g| g.frames.iter()).map(|f| f.filename.as_str()).collect();
        assert_eq!(grouped, vec!["c_a.fits"]);
        let reported: Vec<(&str, Option<&str>, bool)> = source
            .frames
            .iter()
            .map(|f| (f.frame_uuid.as_str(), f.not_stackable.as_deref(), f.handle.is_some()))
            .collect();
        assert_eq!(
            reported,
            vec![
                ("a", None, true),
                ("b", Some("not on this device"), false),
                ("d", Some("being re-staged"), false),
                ("e", Some("unreadable size:mtime stamp"), false),
            ]
        );
    }

    /// Plan 6b-2 C13: colour tokens — every OSC frame is `osc` with the
    /// debayer on; with it off, a one-plane mosaic is `cfa` and an already
    /// three-plane replica stays `osc`; the two never share a group.
    #[test]
    fn colour_tokens_follow_the_plane_rule() {
        let conn = test_conn();
        insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("m1"));
        let mut mosaic = project_frame_spec("o1");
        mosaic.file = ProjectFile::Mosaic32;
        mosaic.bayerpat = Some("RGGB");
        add_project_frame(&conn, dir.path(), "p1", &mosaic);
        let mut rgb = project_frame_spec("o2");
        rgb.file = ProjectFile::Rgb32;
        rgb.bayerpat = Some("RGGB");
        add_project_frame(&conn, dir.path(), "p1", &rgb);

        let keys = |debayer_osc: bool| {
            let mut cfg = StackingConfig::default();
            cfg.calibration.debayer_osc = debayer_osc;
            let mut keys: Vec<(String, usize)> = project_frame_source(&conn, "p1", &cfg)
                .unwrap()
                .groups
                .into_iter()
                .map(|g| (g.key, g.frames.len()))
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(
            keys(true),
            vec![("mono__L__300s".into(), 1), ("osc__L__300s".into(), 2)]
        );
        assert_eq!(
            keys(false),
            vec![("cfa__L__300s".into(), 1), ("mono__L__300s".into(), 1), ("osc__L__300s".into(), 1)]
        );
    }

    #[test]
    fn xisf_geometry_reads_the_first_image_element() {
        let xml = r#"<xisf><Image geometry="64:48:3" sampleFormat="Float32" location="attachment:4096:36864"/></xisf>"#;
        assert_eq!(xisf_geometry(xml), Some((64, 48, 3)));
        assert_eq!(xisf_geometry(r#"<Image geometry="10:20"/>"#), Some((10, 20, 1)));
        assert_eq!(xisf_geometry("<xisf/>"), None);
        assert_eq!(project_group_key("osc", "Ha OIII", "st300"), "osc__Ha_OIII__st300");
        assert_eq!(project_group_key("mono", "None", "all"), "mono__None__all");
    }
}
```
`origin.rs` `mod tests`:
- In `the_catalog_arms_read_what_they_read_today`, the literal becomes `FrameOrigin::Catalog { file_id: 0, attested: false }`.
- Add:
```rust
    /// Plan 6b-2 C12: CFA geometry from the file's own header — never the
    /// catalog — with an odd phase preserved.
    #[test]
    fn cfa_geometry_comes_from_the_files_own_header() {
        use crate::fits_writer::{write_fits_f32, Card, CardValue};
        use crate::fits_writer::keywords::Bayer;
        let dir = tempfile::tempdir().unwrap();
        let mosaic = dir.path().join("m.fits");
        let cards = vec![
            Card::new("BAYERPAT", CardValue::Str("GRBG".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
        ];
        write_fits_f32(&mosaic, 8, 8, 1, &vec![0.1; 64], &cards).unwrap();
        assert_eq!(
            cfa_geometry_of_file(&mosaic).unwrap(),
            Some(CfaGeometry { pattern: Bayer::Grbg, xoff: 1, yoff: 0 })
        );
        let mono = dir.path().join("n.fits");
        write_fits_f32(&mono, 8, 8, 1, &vec![0.1; 64], &[]).unwrap();
        assert_eq!(cfa_geometry_of_file(&mono).unwrap(), None);
    }

    /// Plan 6b-2 C8: no function in the production half of `run.rs` or
    /// `plan.rs` names a `StackSubject` or `FrameOrigin` variant outside the
    /// named funnels and the allowlist — so a third subject, or a later
    /// trait extraction, touches exactly these functions.
    #[test]
    fn subject_and_origin_variants_stay_in_their_funnels() {
        const FUNNELS: &[&str] = &[
            "frame_source", "subject_layout", "subject_label", "stored_reference_frame",
            "hash_by_origin", "stored_solve", "cfa_geometry", "resolve_reference",
            "resolve_calibrate_job", "stored_registrations", "store_registration",
            "stage_one_kind",
        ];
        const ALLOWLIST: &[(&str, &str)] = &[
            ("heal_interrupted_runs", "log-only: the frame-set and project heal lines carry different fields"),
            ("start_stacking", "frame-set-only guard: a project start is refused until 6b-3"),
            ("build_plan", "the subject dispatch to the two plan arms, and its per-subject log line"),
            ("test_context", "#[cfg(test)] constructor of a frame-set run"),
            ("project_test_context", "#[cfg(test)] constructor of a project run"),
        ];
        for (file, text) in [("run.rs", include_str!("run.rs")), ("plan.rs", include_str!("plan.rs"))] {
            let production = text.split("\nmod tests {").next().expect("split yields a first part");
            let mut current_fn = String::new();
            for (n, line) in production.lines().enumerate() {
                if let Some(name) = fn_name(line) {
                    current_fn = name;
                }
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains("StackSubject::") || line.contains("FrameOrigin::") {
                    let allowed = FUNNELS.contains(&current_fn.as_str())
                        || ALLOWLIST.iter().any(|(f, _)| *f == current_fn);
                    assert!(
                        allowed,
                        "{file}:{}: `{current_fn}` names a subject/origin variant outside the funnels: {}",
                        n + 1,
                        line.trim()
                    );
                }
            }
        }
    }

    /// The name of the function a source line declares, if it declares one.
    fn fn_name(line: &str) -> Option<String> {
        let mut t = line.trim_start();
        for prefix in ["pub(crate) ", "pub ", "async ", "const ", "unsafe "] {
            if let Some(rest) = t.strip_prefix(prefix) {
                t = rest;
            }
        }
        let rest = t.strip_prefix("fn ")?;
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        (!name.is_empty()).then_some(name)
    }
```
`plan.rs` tests:
- `frame_source_returns_one_catalog_origin_per_frame`:
  - call `frame_source(&f.conn, &StackSubject::FrameSet(f.set_id), &cfg)`;
  - expect `FrameOrigin::Catalog { file_id: fr.file_id, attested: false }`;
  - replace its project-refusal tail with:
```rust
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let (groups, origins) =
            frame_source(&f.conn, &StackSubject::Project("p1".into()), &cfg).unwrap();
        assert!(groups.is_empty() && origins.is_empty(), "a project with no landed frame");

        // Spec §13 (R3): an attested set's origins carry the flag.
        f.conn
            .execute("UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1", [f.set_id])
            .unwrap();
        let (_, origins) =
            frame_source(&f.conn, &StackSubject::FrameSet(f.set_id), &cfg).unwrap();
        assert!(origins
            .values()
            .all(|o| matches!(o, FrameOrigin::Catalog { attested: true, .. })));
```
- `resolve_reference_reads_the_catalog_only_for_a_frame_with_an_origin`: `frame_source(&f.conn, &subject, &cfg)`.
- `the_hash_memo_refuses_a_frame_it_has_no_origin_for`: the literal gets `attested: false`.
- Add:
```rust
    /// Plan 6b-2 C12: the Prepare subtree's exact shape, and the project
    /// stage-1 hash `HashMemo` gives a project frame — content and size,
    /// never a catalog identity; the debayer toggle moves it, an unrelated
    /// stage's config does not.
    #[test]
    fn the_prepare_subtree_and_the_project_hash_are_pinned() {
        use crate::stacking::config::{prepare_subtree, stage_hash, SourceIdentity};
        let cfg = StackingConfig::default();
        assert_eq!(
            serde_json::to_string(&prepare_subtree(&cfg)).unwrap(),
            r#"{"debayerMethod":"VNG","debayerOsc":true,"prepareVersion":1}"#
        );
        let blake3 = "ab".repeat(32);
        let expected = stage_hash(
            &prepare_subtree(&cfg),
            &[&format!("blake3:{blake3}")],
            &[SourceIdentity { file_id: 0, size: 4096, modified_at: String::new() }],
        );
        assert_eq!(prepare_hash_for_project(&cfg, &blake3, 4096), expected);
        let mut other = cfg.clone();
        other.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
        assert_eq!(prepare_hash_for_project(&other, &blake3, 4096), expected);
        other.calibration.debayer_osc = false;
        assert_ne!(prepare_hash_for_project(&other, &blake3, 4096), expected);
    }

    /// Plan 6b-2 C7: a project frame's manual reference resolves its file
    /// name and landed path from the origin, never the catalog.
    #[test]
    fn a_projects_manual_reference_resolves_from_its_origin() {
        use crate::stacking::groups::frame_source;
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        for uuid in ["r1", "r2", "r3"] {
            add_project_frame(&f.conn, f.dir.path(), "p1", &project_frame_spec(uuid));
        }
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let subject = StackSubject::Project("p1".into());
        let (_, origins) = frame_source(&f.conn, &subject, &cfg).unwrap();
        crate::db::stacking::set_project_config(&f.conn, "p1", "{}", &[], Some("r2")).unwrap();
        let mut blockers = Vec::new();
        let reference = resolve_reference(&f.conn, &subject, &origins, &cfg, &mut blockers).unwrap();
        assert!(blockers.is_empty(), "{blockers:?}");
        assert_eq!(reference.filename.as_deref(), Some("c_r2.fits"));
        assert!(reference.on_disk);
    }
```
`db/stacking.rs` tests:
```rust
    /// Review M2: an empty handle list mints nothing and returns at once,
    /// without the IMMEDIATE transaction (an empty project's plan must not
    /// make the live-exchange writer wait for the write lock).
    #[test]
    fn an_empty_handle_list_writes_nothing() {
        let c = conn();
        insert_test_project(&c, "p1");
        assert!(project_frame_handles(&c, "p1", &[]).unwrap().is_empty());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM stack_project_frames", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
```
This test is green before the change too: it pins the result, and the change removes the write lock.
`run.rs`: in the `solve_of` test at `~20082`, the literal becomes `FrameOrigin::Catalog { file_id: 0, attested: false }`.

- [ ] **Step 3: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 project_source cfa_geometry_comes subject_and_origin_variants the_prepare_subtree a_projects_manual_reference frame_source_returns an_empty_handle_list`

Expected: the build fails, because none of the new items exist.

- [ ] **Step 4: Implement `origin.rs`.** Replace the enum and add `cfa_geometry_of_file` below it. Keep the module doc, and update its "6b-1 has one variant" sentences to describe the two variants. Add one line to its last paragraph: "`db::collab_frames::FrameOrigin` (`Own | Replica`) is the other `FrameOrigin`; never import both unqualified in one file."
```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FrameOrigin {
    /// A catalog frame: `frames.id` is the run's frame id; `file_id` is
    /// `files.id` (the stage-1 hash's `SourceIdentity`). `attested` is the
    /// frame set's `calibrated_externally` (spec §13, R3): such a light is
    /// prepared, never calibrated.
    Catalog { file_id: i64, attested: bool },
    /// A collaboration project frame (spec §4.3): the run's frame id is its
    /// `stack_project_frames` handle. Everything a run reads about it is
    /// here — the landed replica (`landed_path`, its content `blake3` and
    /// `byte_size`, the `size:mtime` stamp the identity check compares
    /// against) and its manifest `meta`, parsed once. `publisher` is the
    /// publisher's display name.
    Project {
        project_id: String,
        frame_uuid: String,
        publisher: String,
        landed_path: String,
        blake3: String,
        byte_size: i64,
        size_mtime: String,
        meta: ProjectFrameMeta,
    },
}

/// The CFA mosaic geometry a FILE's own header declares (plan 6b-2 C12):
/// BAYERPAT and XBAYROFF/YBAYROFF, never the catalog. FITS through its
/// header cards, XISF through the scanner's own reader (FITSKeyword or the
/// native `<ColorFilterArray>`). A missing offset is 0 (with a `debug!`),
/// an unrecognized pattern is no mosaic (with a `warn!`) — the rules of
/// `light_resolve::resolve_cfa_geometry`. ROWORDER is not geometry: BAYERPAT
/// describes the mosaic in file row order.
pub(crate) fn cfa_geometry_of_file(path: &Path) -> anyhow::Result<Option<CfaGeometry>> {
    let (bayerpat, xoff, yoff) = if crate::stacking::project_source::is_xisf(path) {
        let f = crate::fits_parser::parse_xisf(path, 0)?;
        (f.bayerpat, f.xbayroff, f.ybayroff)
    } else {
        let header = crate::fits_parser::FitsHeader::from_path(path)?;
        let keys = crate::fits_parser::stored_header::parse_stored_header_keys(
            crate::models::FileFormat::FITS,
            &header.to_header_text(),
        );
        let f = crate::fits_parser::stored_header::cfa_fields_from_keys(&keys);
        (f.bayerpat, f.xbayroff, f.ybayroff)
    };
    let Some(raw) = bayerpat.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Some(pattern) = crate::fits_writer::keywords::Bayer::parse(raw) else {
        tracing::warn!(path = %path.display(), bayerpat = %raw, "unrecognized bayer pattern in the file's header; treated as no mosaic");
        return Ok(None);
    };
    if xoff.is_none() || yoff.is_none() {
        tracing::debug!(path = %path.display(), "cfa phase not declared in the file's header; assumes 0");
    }
    Ok(Some(CfaGeometry {
        pattern,
        xoff: xoff.unwrap_or(0),
        yoff: yoff.unwrap_or(0),
    }))
}
```
The new arms in `stored_solve` and `cfa_geometry`:
```rust
        // The manifest WCS (spec §5.2); Task 10b adds the run's local solve.
        FrameOrigin::Project { meta, .. } => Ok(meta
            .wcs
            .as_ref()
            .map(|w| w.to_record(meta.pixel_scale_arcsec))),
```
```rust
        FrameOrigin::Project { landed_path, .. } => cfa_geometry_of_file(Path::new(landed_path)),
```
Imports: `use std::path::Path;` and `use crate::collab::frame_meta::ProjectFrameMeta;`. Use `cfa_fields_from_keys` if `xbayroff` is `Option<i64>` there; read its struct at `stored_header.rs:~236` and adapt the destructuring to its exact field types.

- [ ] **Step 5: Implement `config.rs` and `plan.rs`**
```rust
// config.rs
/// Bumped when Prepare's output for an unchanged input changes — the
/// in-place rule, a conversion's value domain, the debayer (plan 6b-2 C12).
pub const PREPARE_VERSION: u32 = 1;

/// Stage 1's subtree for a PREPARED frame (spec §5.1, plan 6b-2 C12): of
/// the calibration options only `debayerOsc` applies (a prepared frame is
/// never calibrated), plus the debayer method — VNG, the only one — and
/// [`PREPARE_VERSION`]. Deliberately not `calibration_subtree`, whose
/// golden literals a frame set's caches rest on.
pub fn prepare_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({
        "debayerOsc": cfg.calibration.debayer_osc,
        "debayerMethod": "VNG",
        "prepareVersion": PREPARE_VERSION,
    })
}
```
```rust
// plan.rs, beside `calibration_hash_for`
/// Stage 1's hash for a prepared PROJECT frame (spec §5.1, plan 6b-2 C12):
/// the Prepare subtree, the frame's content (`blake3`) and byte size —
/// never a catalog identity. Every later stage hash chains on it exactly as
/// on a calibration hash.
pub(crate) fn prepare_hash_for_project(cfg: &StackingConfig, blake3: &str, byte_size: i64) -> String {
    stage_hash(
        &crate::stacking::config::prepare_subtree(cfg),
        &[&format!("blake3:{blake3}")],
        &[SourceIdentity {
            file_id: 0,
            size: byte_size,
            modified_at: String::new(),
        }],
    )
}
```
`HashMemo::hash_by_origin`:
```rust
        match origin_of(&self.origins, frame.frame_id)? {
            // An attested set keeps today's generation hash until the Prepare
            // stage lands (Task 9a splits this arm).
            FrameOrigin::Catalog { .. } => {
                calibration_hash_for(conn, cfg, frame, &mut self.divisors)
            }
            FrameOrigin::Project {
                blake3, byte_size, ..
            } => Ok(prepare_hash_for_project(cfg, blake3, *byte_size)),
        }
```
`resolve_reference`'s file row:
```rust
    let file_row: Option<(String, String)> = match origins.get(&reference_frame_id) {
        Some(FrameOrigin::Catalog { .. }) => conn
            .query_row(/* unchanged */)
            .optional()?,
        // A project frame's file is its landed replica; the landing writes
        // `<publisher folder>/<file name>`, so the base name is the file name.
        Some(FrameOrigin::Project { landed_path, .. }) => Some((
            Path::new(landed_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| landed_path.clone()),
            landed_path.clone(),
        )),
        None => None,
    };
```
`last_run_reference_scale` and `cleanup_stale_note` take `subject: &StackSubject` in place of `frames_set_id: i64`, and their `list_runs` calls pass `subject`. In `build_plan`, the two call sites pass `subject`. In `build_plan`, `frame_source(conn, subject, &cfg)`.

- [ ] **Step 6: Implement `groups.rs` and `project_source.rs`.** In `groups.rs`, replace `frame_source` and delete the `PROJECT_STACKING_UNAVAILABLE` import:
```rust
/// The frame source of a stacking subject: its integration groups plus the
/// origin of every frame in them (wave 6 spec §4.3). A frame set reads the
/// catalog through [`group_frames`] (its `calibrated_externally` flag rides
/// every origin, spec §13); a project reads its manifest mirror through
/// [`crate::stacking::project_source::project_frame_source`].
pub(crate) fn frame_source(
    conn: &Connection,
    subject: &StackSubject,
    cfg: &StackingConfig,
) -> Result<(Vec<IntegrationGroup>, FrameOrigins), ApiError> {
    match subject {
        StackSubject::FrameSet(frames_set_id) => {
            let groups = group_frames(conn, *frames_set_id, &cfg.grouping)?;
            let attested = crate::db::collab::frames_set_attested(conn, *frames_set_id)?;
            let origins = groups
                .iter()
                .flat_map(|g| g.frames.iter())
                .map(|f| (f.frame_id, FrameOrigin::Catalog { file_id: f.file_id, attested }))
                .collect();
            Ok((groups, Arc::new(origins)))
        }
        StackSubject::Project(project_id) => {
            let source = crate::stacking::project_source::project_frame_source(conn, project_id, cfg)?;
            Ok((source.groups, source.origins))
        }
    }
}
```
`run.rs` `start_stacking`: `frame_source(&conn, &subject, &plan.config)?`.

`project_source.rs`. Write the module doc, then:
```rust
//! A collaboration project as a stacking subject's frame source (spec
//! 2026-10-02 §4.3, §7, §8; plan 6b-2 Task 6a): the project's stackable
//! landed frames grouped by colour × canonical filter × exposure class
//! (W7: camera and binning are not keys, W9: colour modes and filters never
//! mix), each a `stack_project_frames` handle with a `FrameOrigin::Project`
//! carrying everything a run reads about it. Reads `project_frames_local`
//! and the landed files' headers — never the catalog — and mints handles
//! (`db::stacking::project_frame_handles`, one IMMEDIATE transaction).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use crate::api::ApiError;
use crate::collab::frame_meta::{parse_project_frame_meta, ProjectFrameMeta};
use crate::collab::storage::sweep::Stamp;
use crate::db::collab_frames::{self as collab_frames, LocalFrameRow};
use crate::stacking::config::StackingConfig;
use crate::stacking::groups::{
    classify_exposures, f_ratio, median_f64, sanitized_or, ClassInput, ColorMode, GroupFrame,
    IntegrationGroup, ScaleSource,
};
use crate::stacking::origin::{FrameOrigin, FrameOrigins};

/// One published, accepted frame of the project as the plan reports it
/// (spec §7's `projectFrames`): stackable or not, and why not.
#[derive(Debug, Clone)]
pub struct SourceFrame {
    /// The frame's handle — minted only for a stackable frame.
    pub handle: Option<i64>,
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    pub filter_canonical: String,
    pub exptime_sec: f64,
    pub color_mode: ColorMode,
    /// `None` when the manifest row did not parse (the frame is not stacked).
    pub meta: Option<ProjectFrameMeta>,
    /// Why the frame is not stacked on this device; `None` when it is.
    pub not_stackable: Option<String>,
}

/// A project's frames as one plan or run reads them.
pub struct ProjectSource {
    pub groups: Vec<IntegrationGroup>,
    pub origins: FrameOrigins,
    pub frames: Vec<SourceFrame>,
}

/// A project group's key (spec §8): `<mono|osc|cfa>__<canonical>__<class>`,
/// the filter token sanitized exactly as a frame set's, so the key is a
/// safe path component. No binning and no camera (W7).
pub fn project_group_key(color_token: &str, canonical: &str, class_token: &str) -> String {
    format!(
        "{color_token}__{}__{class_token}",
        sanitized_or(Some(canonical), "NoFilter")
    )
}

/// Whether a path names an XISF file (by extension, as the scanner decides).
pub fn is_xisf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"))
}

/// `geometry="w:h[:c]"` of the first `<Image` element of an XISF XML header.
pub fn xisf_geometry(xml: &str) -> Option<(usize, usize, usize)> {
    let image = &xml[xml.find("<Image")?..];
    let start = image.find("geometry=\"")? + "geometry=\"".len();
    let rest = &image[start..];
    let value = &rest[..rest.find('"')?];
    let mut parts = value.split(':').map(|s| s.trim().parse::<usize>().ok());
    let w = parts.next()??;
    let h = parts.next()??;
    let c = parts.next().flatten().unwrap_or(1);
    Some((w, h, c))
}

/// `(width, height, planes)` from a file's header alone: a FITS primary
/// image through `probe_fits` (its handle dropped at once — a run never
/// holds a landed file open), an XISF through its XML header. `None` when
/// neither reader can tell.
pub fn file_shape_quick(path: &Path) -> Option<(usize, usize, usize)> {
    if is_xisf(path) {
        let raw = crate::fits_parser::read_xisf_header(path).ok()?;
        xisf_geometry(&String::from_utf8_lossy(&raw))
    } else {
        crate::integration::banded::probe_fits(path).map(|(_file, info)| (info.w, info.h, info.naxis3))
    }
}

/// The colour token of a project group key (plan 6b-2 C13): `mono`; `osc`
/// for a colour frame that is three-plane after Prepare; `cfa` for one
/// that stays a one-plane mosaic (`debayerOsc` off and the file a mosaic).
/// The manifest's `bayerpat` decides colour vs mono (spec §5.1); the file is
/// probed only when `debayerOsc` is off.
fn color_token(meta: &ProjectFrameMeta, landed: &Path, debayer_osc: bool) -> &'static str {
    if meta.bayerpat.is_none() {
        return "mono";
    }
    if debayer_osc {
        return "osc";
    }
    match file_shape_quick(landed) {
        Some((_, _, 3)) => "osc",
        _ => "cfa",
    }
}

struct Member {
    frame: GroupFrame,
    canonical: String,
    color: ColorMode,
    instrume: Option<String>,
    f_ratio: Option<f64>,
}

/// The project's frame source (spec §4.3, §7, §8): every published,
/// accepted row is reported; the stackable ones (plan 6b-2 C5) get a handle,
/// a `GroupFrame` from their manifest and a `FrameOrigin::Project`, and are
/// grouped by colour token × canonical filter × exposure class. Groups sort
/// as a frame set's do (frames desc, total exposure desc, key).
pub fn project_frame_source(
    conn: &Connection,
    project_id: &str,
    cfg: &StackingConfig,
) -> Result<ProjectSource, ApiError> {
    let rows = collab_frames::list_for_project(conn, project_id)?;
    let mut frames: Vec<SourceFrame> = Vec::new();
    let mut stackable: Vec<(usize, LocalFrameRow, ProjectFrameMeta, Stamp)> = Vec::new();
    for row in rows {
        if !collab_frames::part_of_project(&row) {
            continue;
        }
        let meta = match parse_project_frame_meta(&row.manifest_json) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "project frame metadata unreadable; frame left out");
                None
            }
        };
        let stamp = row.size_mtime_seen.as_deref().and_then(Stamp::parse);
        let verdict: Result<Stamp, String> = match (&meta, collab_frames::stackable(&row), stamp) {
            (None, _, _) => Err("unreadable frame metadata".to_string()),
            (Some(_), Err(reason), _) => Err(reason.reason().to_string()),
            // `stackable` already refused a NULL stamp ("waiting for its new
            // version"). A non-NULL one that does not parse is corruption,
            // which no later sync repairs (review I3).
            (Some(_), Ok(()), None) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, stamp = ?row.size_mtime_seen, "landed stamp unreadable; frame left out");
                Err("unreadable size:mtime stamp".to_string())
            }
            (Some(_), Ok(()), Some(stamp)) => Ok(stamp),
        };
        let color_mode = if meta.as_ref().is_some_and(|m| m.bayerpat.is_some()) {
            ColorMode::Osc
        } else {
            ColorMode::Mono
        };
        frames.push(SourceFrame {
            handle: None,
            frame_uuid: row.frame_uuid.clone(),
            file_name: row.file_name.clone(),
            publisher: row.publisher_display.clone(),
            filter_canonical: row.filter_canonical.clone(),
            exptime_sec: meta.as_ref().map_or(0.0, |m| m.exptime_sec),
            color_mode,
            meta: meta.clone(),
            not_stackable: verdict.as_ref().err().cloned(),
        });
        if let (Ok(stamp), Some(meta)) = (verdict, meta) {
            stackable.push((frames.len() - 1, row, meta, stamp));
        }
    }

    let uuids: Vec<&str> = stackable.iter().map(|(_, r, _, _)| r.frame_uuid.as_str()).collect();
    let handles = crate::db::stacking::project_frame_handles(conn, project_id, &uuids)?;

    let mut origins: HashMap<i64, FrameOrigin> = HashMap::with_capacity(stackable.len());
    let mut buckets: BTreeMap<(&'static str, String), Vec<Member>> = BTreeMap::new();
    for (idx, row, meta, stamp) in stackable {
        let handle = *handles.get(&row.frame_uuid).ok_or_else(|| {
            ApiError::Internal(format!("no handle minted for project frame {}", row.frame_uuid))
        })?;
        frames[idx].handle = Some(handle);
        let landed = row
            .landed_path
            .clone()
            .ok_or_else(|| ApiError::Internal(format!("stackable frame {} has no landed path", row.frame_uuid)))?;
        let (width, height) = match (meta.naxis1, meta.naxis2) {
            (Some(w), Some(h)) => (w, h),
            _ => file_shape_quick(Path::new(&landed))
                .map(|(w, h, _)| (w as i64, h as i64))
                .unwrap_or((0, 0)),
        };
        let token = color_token(&meta, Path::new(&landed), cfg.calibration.debayer_osc);
        let frame = GroupFrame {
            frame_id: handle,
            file_id: 0,
            filename: row.file_name.clone(),
            path: landed.clone(),
            size: stamp.size as i64,
            modified_at: chrono::DateTime::<chrono::Utc>::from_timestamp(stamp.mtime, 0)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
            exposure_s: Some(meta.exptime_sec),
            date_obs: meta.date_obs.clone(),
            width,
            height,
            pixel_scale_arcsec: meta.pixel_scale_arcsec,
            scale_source: meta.pixel_scale_arcsec.map(|_| {
                if meta.wcs.is_some() {
                    ScaleSource::Solve
                } else {
                    ScaleSource::Header
                }
            }),
        };
        origins.insert(
            handle,
            FrameOrigin::Project {
                project_id: project_id.to_string(),
                frame_uuid: row.frame_uuid.clone(),
                publisher: row.publisher_display.clone(),
                landed_path: landed,
                blake3: row.blake3.clone(),
                byte_size: row.byte_size,
                size_mtime: stamp.encode(),
                meta: meta.clone(),
            },
        );
        buckets
            .entry((token, sanitized_or(Some(&row.filter_canonical), "NoFilter")))
            .or_default()
            .push(Member {
                frame,
                canonical: row.filter_canonical.clone(),
                color: if token == "mono" { ColorMode::Mono } else { ColorMode::Osc },
                instrume: meta.instrume.clone(),
                f_ratio: f_ratio(meta.focal_len, meta.aperture),
            });
    }

    let mut groups = Vec::new();
    for ((token, _filter_token), mut members) in buckets {
        members.sort_by(|a, b| {
            a.frame
                .date_obs
                .cmp(&b.frame.date_obs)
                .then(a.frame.frame_id.cmp(&b.frame.frame_id))
        });
        let inputs: Vec<ClassInput> = members
            .iter()
            .map(|m| ClassInput {
                exposure_s: m.frame.exposure_s,
                f_ratio: m.f_ratio,
            })
            .collect();
        for class in classify_exposures(&inputs, &cfg.grouping) {
            let group_frames: Vec<GroupFrame> =
                class.members.iter().map(|&i| members[i].frame.clone()).collect();
            let total_exposure_s = group_frames.iter().map(|f| f.exposure_s.unwrap_or(0.0)).sum();
            let mut scales: Vec<f64> = group_frames.iter().filter_map(|f| f.pixel_scale_arcsec).collect();
            let pixel_scale_arcsec = (!scales.is_empty()).then(|| median_f64(&mut scales));
            let first = &members[class.members[0]];
            let mut cameras: Vec<String> = class
                .members
                .iter()
                .filter_map(|&i| members[i].instrume.as_deref().map(str::trim))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            cameras.sort();
            cameras.dedup();
            if cameras.is_empty() {
                cameras.push("unknown".to_string());
            }
            groups.push(IntegrationGroup {
                key: project_group_key(token, &first.canonical, &class.token),
                instrume: first.instrume.clone(),
                color_mode: first.color,
                filter: Some(first.canonical.clone()),
                binning: 1,
                cameras,
                exposure_s: class.exposure_s,
                exposure_class: class.token,
                frames: group_frames,
                total_exposure_s,
                pixel_scale_arcsec,
            });
        }
    }
    groups.sort_by(|a, b| {
        b.frames
            .len()
            .cmp(&a.frames.len())
            .then_with(|| {
                b.total_exposure_s
                    .partial_cmp(&a.total_exposure_s)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.key.cmp(&b.key))
    });

    let stackable_count = origins.len();
    tracing::debug!(
        project_id,
        count = stackable_count,
        not_stackable = frames.len() - stackable_count,
        groups = groups.len(),
        "project frame source built"
    );
    Ok(ProjectSource {
        groups,
        origins: Arc::new(origins),
        frames,
    })
}
```
In `db/stacking.rs`, `project_frame_handles` starts with (review M2):
```rust
    // No frame, no write lock: an empty project, or one with nothing
    // stackable on this device, must not make the live-exchange writer wait.
    if frame_uuids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
```
In `fits_parser/mod.rs`, make `read_xisf_header` `pub(crate)`. If `median_f64` or `GroupFrame`'s fields are not reachable from a sibling module, make them `pub(crate)`; this is a visibility change only.

- [ ] **Step 7: Implement `run.rs`**
  1. `resolve_calibrate_job`'s origin match:
```rust
        match origin_of(&rc.origins, frame.frame_id) {
            Ok(FrameOrigin::Catalog { .. }) => resolve_generation_cached(
                &conn,
                frame.frame_id,
                &calibration_opts,
                scratch,
                rc.memo.divisors_mut(),
            )
            .map_err(|e| format!("{e:#}")),
            // A project frame is prepared, never calibrated (spec §5.1).
            Ok(FrameOrigin::Project { .. }) => {
                Err("a project frame is prepared, never calibrated".to_string())
            }
            Err(e) => Err(e.to_string()),
        }
```
  2. `test_context`: the origin literal becomes `FrameOrigin::Catalog { file_id: f.file_id, attested: false }`.

- [ ] **Step 8: Run the tests and watch them pass.** Run the Step 3 command. Then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking:: db::stacking`: every existing stacking test passes. A frame set's frame source, hashes and Calibrate are unchanged: `Catalog { attested: false }` takes today's arms.

- [ ] **Step 9: Run the gate.** TS must not move: `GroupFrame` and `FrameOrigin` are not exported.

- [ ] **Step 10: Commit** with the message `feat(stacking): FrameOrigin::Project and the project frame source`, plus the trailers.

---

### Task 6b: The landed-file identity check at Measure, Register and Output; the project test context (C9; spec §5.1)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/origin.rs`:
  - add `ExclusionReason`, `CHANGED_DURING_THE_RUN` and `landed_unchanged`;
  - the funnel test's `FUNNELS` gains `"landed_unchanged"`;
  - a test.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - add `changed_during_run` and `exclude_changed_members`, next to `exclude_frame_and_persist`;
  - the Measure (`~2983`), Register (`register_group_pass`, `~4304`) and Output (`process_group_output`, `~6804`) boundaries;
  - add `project_test_context`, after `test_context`;
  - tests: the helpers `project_conn`, `project_rc` and `seed_in_place_calibrated`, then two tests.

**Interfaces:**
```rust
// stacking/origin.rs
#[derive(Debug, Clone, PartialEq, Eq)] pub(crate) struct ExclusionReason(pub(crate) String);
pub(crate) const CHANGED_DURING_THE_RUN: &str = "changed during the run";
pub(crate) fn landed_unchanged(origin: &FrameOrigin) -> Result<(), ExclusionReason>;
// stacking/run.rs
fn changed_during_run(rc: &RunContext, frame_id: i64) -> Result<Option<String>, RunError>;
fn exclude_changed_members(rc: &mut RunContext, group: &IntegrationGroup) -> Result<usize, RunError>;
#[cfg(test)] pub(crate) fn project_test_context(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, run_id: i64, project_id: &str,
    config: StackingConfig, plan_groups: Vec<IntegrationGroup>, origins: FrameOrigins, layout: WorkingLayout, output_dir: PathBuf, group_ids: HashMap<String, i64>) -> RunContext;
```
Consumed by: Task 9b (the Prepare boundary; `project_test_context`, `project_rc`, `seed_in_place_calibrated`), Task 10b (the Solve boundary and the same helpers).

- [ ] **Step 1: Write the failing tests**

`origin.rs` `mod tests`: in `subject_and_origin_variants_stay_in_their_funnels`, `FUNNELS`' last line becomes `"stage_one_kind", "landed_unchanged",`. Add:
```rust
    /// Plan 6b-2 C9: a catalog frame always passes; a project frame passes
    /// while its landed file matches the recorded stamp, and is excluded
    /// "changed during the run" once rewritten or removed.
    #[test]
    fn landed_unchanged_follows_the_recorded_stamp() {
        let catalog = FrameOrigin::Catalog { file_id: 1, attested: false };
        assert_eq!(landed_unchanged(&catalog), Ok(()));

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        let path = test_fixtures::add_project_frame(
            &conn,
            dir.path(),
            "p1",
            &test_fixtures::project_frame_spec("a"),
        );
        let source = crate::stacking::project_source::project_frame_source(
            &conn,
            "p1",
            &crate::stacking::config::StackingConfig::default(),
        )
        .unwrap();
        let origin = source.origins.values().next().unwrap().clone();
        assert_eq!(landed_unchanged(&origin), Ok(()));
        std::fs::write(&path, vec![0u8; 4321]).unwrap();
        assert_eq!(
            landed_unchanged(&origin),
            Err(ExclusionReason(CHANGED_DURING_THE_RUN.to_string()))
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            landed_unchanged(&origin),
            Err(ExclusionReason(CHANGED_DURING_THE_RUN.to_string()))
        );
    }

```
`run.rs` tests (helpers first, then the two tests):
```rust
    fn project_conn(db_path: &Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    /// A project run's context over the project's own frame source (plan
    /// 6b-2): a real run row and group rows, so stages can persist.
    fn project_rc(
        ctx: &Arc<ServiceContext>,
        conn: &rusqlite::Connection,
        project_id: &str,
        cfg: StackingConfig,
        working: &Path,
        output: &Path,
    ) -> RunContext {
        let source =
            crate::stacking::project_source::project_frame_source(conn, project_id, &cfg).unwrap();
        let subject = StackSubject::Project(project_id.to_string());
        let run_id = insert_run(
            conn,
            &NewRun {
                subject: &subject,
                config_json: &serde_json::to_string(&cfg).unwrap(),
                config_hash: &config_hash(&cfg),
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.to_str().unwrap(),
                output_dir: output.to_str().unwrap(),
            },
        )
        .unwrap();
        let mut group_ids = HashMap::new();
        for g in &source.groups {
            let id = insert_group(
                conn,
                &NewGroup {
                    run_id,
                    group_key: &g.key,
                    instrume: g.instrume.as_deref(),
                    color_mode: color_mode_wire(g.color_mode),
                    filter: g.filter.as_deref(),
                    binning: Some(g.binning),
                    width: None,
                    height: None,
                    exposure: g.exposure_s,
                    exposure_class: Some(&g.exposure_class),
                    frame_count: g.frames.len() as i64,
                    included_count: g.frames.len() as i64,
                },
            )
            .unwrap();
            group_ids.insert(g.key.clone(), id);
        }
        let layout = subject_layout(conn, working, &subject).unwrap();
        project_test_context(
            ctx.clone(),
            Arc::new(NullEmitter),
            run_id,
            project_id,
            cfg,
            source.groups,
            source.origins,
            layout,
            output.to_path_buf(),
            group_ids,
        )
    }

    /// `calibrated` rows pointing at each project frame's landed file — what
    /// Prepare writes for a float replica used in place (Task 9b) — so
    /// Measure can run without Prepare.
    fn seed_in_place_calibrated(conn: &rusqlite::Connection, rc: &mut RunContext) {
        for g in rc.plan_groups.clone() {
            for f in &g.frames {
                let hash = rc.memo.calibration_hash_checked(conn, &rc.config, f).unwrap();
                let (size, modified_at) = file_identity(Path::new(&f.path)).unwrap();
                upsert_artifact(
                    conn,
                    &NewArtifact {
                        subject: &rc.subject,
                        frame_id: Some(f.frame_id),
                        group_key: &g.key,
                        kind: "calibrated",
                        path: Some(&f.path),
                        config_hash: &hash,
                        size: Some(size),
                        modified_at: Some(&modified_at),
                        payload_json: None,
                    },
                )
                .unwrap();
            }
        }
    }

    /// Plan 6b-2 C9 (the Measure boundary): a landed file the exchange
    /// rewrote after the frame source read its row is excluded "changed
    /// during the run"; its neighbours are not.
    #[test]
    fn measure_excludes_a_project_frame_whose_landed_file_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        let paths: Vec<PathBuf> = ["a", "b", "c", "d"]
            .iter()
            .map(|u| {
                test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u))
            })
            .collect();
        let mut rc = project_rc(
            &ctx,
            &conn,
            "p1",
            StackingConfig::default(),
            &tmp.path().join("work"),
            &tmp.path().join("out"),
        );
        seed_in_place_calibrated(&conn, &mut rc);
        std::fs::write(&paths[1], vec![0u8; 5000]).unwrap();

        // The synthetic frames may or may not measure well enough for a
        // viable group; the exclusion is recorded either way.
        if let Err(e) = stage_measure(&mut rc) {
            eprintln!("stage_measure ended with {e:?} (expected on tiny synthetic frames)");
        }
        let b = rc.plan_groups[0]
            .frames
            .iter()
            .find(|f| f.filename == "c_b.fits")
            .unwrap()
            .frame_id;
        let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
        assert_eq!(
            reasons.get(&b).map(String::as_str),
            Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
        );
        for f in rc.plan_groups[0].frames.iter().filter(|f| f.frame_id != b) {
            assert_ne!(
                reasons.get(&f.frame_id).map(String::as_str),
                Some(crate::stacking::origin::CHANGED_DURING_THE_RUN)
            );
        }
    }

    /// Plan 6b-2 C9 (the Register and Output boundaries): a still-included
    /// member whose landed file changed is excluded and persisted; a changed
    /// reference fails the run.
    #[test]
    fn exclude_changed_members_excludes_and_fails_on_a_changed_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        for u in ["a", "b", "c"] {
            test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u));
        }
        let mut rc = project_rc(
            &ctx,
            &conn,
            "p1",
            StackingConfig::default(),
            &tmp.path().join("work"),
            &tmp.path().join("out"),
        );
        let group = rc.plan_groups[0].clone();
        let entries: Vec<MeasuredFrame> = group
            .frames
            .iter()
            .map(|f| MeasuredFrame {
                frame: f.clone(),
                calibrated: Some(PathBuf::from(&f.path)),
                planes: 1,
                measurement: None,
                weight: None,
                included: true,
                reason: None,
                registration: None,
                cached_calibrated: true,
                cached_metrics: false,
                ln_scale: None,
                cached_ln: false,
            })
            .collect();
        rc.measured.insert(group.key.clone(), entries);

        std::fs::write(&group.frames[1].path, vec![0u8; 5000]).unwrap();
        assert_eq!(exclude_changed_members(&mut rc, &group).unwrap(), 1);
        let entry = &rc.measured[&group.key][1];
        assert!(!entry.included);
        assert_eq!(entry.reason.as_deref(), Some(crate::stacking::origin::CHANGED_DURING_THE_RUN));
        assert_eq!(exclude_changed_members(&mut rc, &group).unwrap(), 0, "an excluded frame is not checked again");

        rc.reference_frame_id = Some(group.frames[0].frame_id);
        std::fs::write(&group.frames[0].path, vec![0u8; 6000]).unwrap();
        let err = exclude_changed_members(&mut rc, &group).unwrap_err();
        assert!(
            matches!(err, RunError::Other(ref m) if m.contains("reference frame changed during the run")),
            "{err:?}"
        );
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 landed_unchanged measure_excludes exclude_changed_members subject_and_origin_variants`

Expected: the build fails, because the identity check, `project_test_context` and the helpers' callees do not exist.

- [ ] **Step 3: Implement `origin.rs`.** Add, after the enum:
```rust
/// Why a frame leaves a stacking run — stored verbatim as its
/// `stacking_run_frames.exclusion_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExclusionReason(pub(crate) String);

/// Spec §5.1: the live exchange landed a new version (or removed the file)
/// while the run was reading it.
pub(crate) const CHANGED_DURING_THE_RUN: &str = "changed during the run";

/// The landed-file identity check (plan 6b-2 C9): a project frame's landed
/// file must still match the `size:mtime` its row recorded (2 s mtime
/// tolerance, `collab::storage::sweep`). Called at every stage boundary
/// that reads it; reads never hold a file open across stages.
pub(crate) fn landed_unchanged(origin: &FrameOrigin) -> Result<(), ExclusionReason> {
    use crate::collab::storage::sweep::{stat_verdict, Stamp, StatVerdict};
    match origin {
        FrameOrigin::Catalog { .. } => Ok(()),
        FrameOrigin::Project {
            project_id,
            frame_uuid,
            landed_path,
            size_mtime,
            ..
        } => match stat_verdict(Path::new(landed_path), Stamp::parse(size_mtime)) {
            StatVerdict::Same => Ok(()),
            StatVerdict::Unreadable(e) => {
                tracing::warn!(project_id = %project_id, frame_uuid = %frame_uuid, path = %landed_path, error = %e, "landed project frame unreadable");
                Err(ExclusionReason(format!("landed file unreadable: {e}")))
            }
            verdict => {
                tracing::warn!(project_id = %project_id, frame_uuid = %frame_uuid, path = %landed_path, outcome = ?verdict, "landed project frame changed during the run");
                Err(ExclusionReason(CHANGED_DURING_THE_RUN.to_string()))
            }
        },
    }
}
```

- [ ] **Step 4: Implement `run.rs`**
  1. Next to `exclude_frame_and_persist`:
```rust
/// The run-thread side of the landed-file identity check (plan 6b-2 C9):
/// the exclusion reason when this frame's landed file changed since the
/// frame source read its row; `None` when it did not, and for every catalog
/// frame.
fn changed_during_run(rc: &RunContext, frame_id: i64) -> Result<Option<String>, RunError> {
    let origin = origin_of(&rc.origins, frame_id)?;
    Ok(crate::stacking::origin::landed_unchanged(origin)
        .err()
        .map(|reason| reason.0))
}

/// The Register and Output boundaries of the identity check (plan 6b-2
/// C9): every still-included member of `group` whose landed file changed is
/// excluded, and its frame row persisted, with the check's reason. A
/// changed group or run reference fails the run instead — a reference
/// cannot leave its own group. Returns how many frames it excluded.
fn exclude_changed_members(rc: &mut RunContext, group: &IntegrationGroup) -> Result<usize, RunError> {
    let included: Vec<i64> = rc
        .measured
        .get(&group.key)
        .map(|v| v.iter().filter(|e| e.included).map(|e| e.frame.frame_id).collect())
        .unwrap_or_default();
    let references: HashSet<i64> = rc
        .group_geometry
        .values()
        .map(|g| g.reference_frame_id)
        .chain(rc.reference_frame_id)
        .collect();
    let mut excluded = 0usize;
    for frame_id in included {
        let Some(reason) = changed_during_run(rc, frame_id)? else {
            continue;
        };
        if references.contains(&frame_id) {
            return Err(RunError::Other(format!(
                "the reference frame changed during the run (frame {frame_id})"
            )));
        }
        tracing::warn!(run_id = rc.run_id, frame_id, reason = %reason, "frame excluded");
        rc.runtime_exclusions.push((frame_id, reason.clone()));
        exclude_frame_and_persist(rc, group, frame_id, reason)?;
        excluded += 1;
    }
    Ok(excluded)
}
```
  2. **Measure** (`stage_measure`, right before `let planes = match PlaneReader::open(&calibrated_path) {`):
```rust
            // Plan 6b-2 C9: a project frame whose landed file changed since
            // the frame source read it leaves the run here.
            if let Some(reason) = changed_during_run(rc, frame.frame_id)? {
                tracing::warn!(run_id = rc.run_id, frame_id = frame.frame_id, reason = %reason, "frame excluded");
                rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                entries.push(MeasuredFrame {
                    frame: frame.clone(),
                    calibrated: None,
                    planes: 0,
                    measurement: None,
                    weight: None,
                    included: false,
                    reason: Some(reason),
                    registration: None,
                    cached_calibrated: false,
                    cached_metrics: false,
                    ln_scale: None,
                    cached_ln: false,
                });
                continue;
            }
```
     This mirrors the `no calibrated artifact on record` branch just above it: the same `entries.push` shape and no progress tick.
  3. **Register** (`register_group_pass`, right after its `included_count_of(...) < 3` early return):
```rust
    // Plan 6b-2 C9: the Register boundary of the landed-file identity check.
    if exclude_changed_members(rc, group)? > 0 && included_count_of(rc, &group.key) < 3 {
        return Ok(pass);
    }
```
  4. **Output** (`process_group_output`, first statement after the `zero` closure):
```rust
    // Plan 6b-2 C9: the Output boundary — the member snapshot below reads
    // the files Normalize, Integrate and Drizzle work from.
    exclude_changed_members(rc, group)?;
```
  5. After `test_context`:
```rust
/// [`test_context`] for a project subject (plan 6b-2): the project frame
/// source's own groups and origins, the project's handle namespace, a
/// summary carrying the project id. Project stages run only through this
/// until 6b-3 lets `start_stacking` start one.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_test_context(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    run_id: i64,
    project_id: &str,
    config: StackingConfig,
    plan_groups: Vec<IntegrationGroup>,
    origins: FrameOrigins,
    layout: WorkingLayout,
    output_dir: PathBuf,
    group_ids: HashMap<String, i64>,
) -> RunContext {
    let mut rc = test_context(
        ctx, emitter, run_id, 0, project_id, config, plan_groups, layout, output_dir, group_ids,
    );
    rc.subject = StackSubject::Project(project_id.to_string());
    rc.memo = HashMemo::new(origins.clone());
    rc.origins = origins;
    rc.summary.set_id = None;
    rc.summary.project_id = Some(project_id.to_string());
    rc
}
```

- [ ] **Step 5: Run the tests and watch them pass.** Run the Step 2 command. Then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::`: every existing stacking test passes. The frame-set Measure, Register and Output paths are unchanged, because `landed_unchanged(Catalog)` is `Ok`.

- [ ] **Step 6: Run the gate.** TS must not move.

- [ ] **Step 7: Commit** with the message `feat(stacking): the landed-file identity check at Measure, Register and Output`, plus the trailers.

---

### Task 8a: The canonical grid's type, hash and proposal geometry; its cache columns (C16; spec §6.1, §6.2)

*Executed after Task 6b; Task 8b and then Task 7 follow.*

**Files:**
- Create `crates/athenaeum-core/src/collab/grid.rs`, ungated. Declare it as `pub mod grid;` in `collab/mod.rs`.
- Create `crates/athenaeum-core/src/collab/grid_geometry.rs`, gated. Declare it as `#[cfg(all(feature = "render", feature = "solver"))] pub mod grid_geometry;` in `collab/mod.rs`.
- Modify `crates/athenaeum-core/src/db/collab.rs`:
  - add `set_canonical_grid` and `canonical_grid`;
  - update `upsert_project`'s doc;
  - extend `upsert_project_preserves_local_columns` (`~1012`), and add a test.

**Interfaces:**
```rust
// collab/grid.rs (ungated)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct CanonicalGrid { pub scale_arcsec: f64, pub width_px: i64, pub height_px: i64, pub crval1: f64, pub crval2: f64, pub rotation_deg: f64 }
pub const GRID_INVALID_MSG: &str;
impl CanonicalGrid {
    pub fn validate(&self) -> Result<(), &'static str>;
    pub fn normalized(&self) -> CanonicalGrid;
    pub fn canonical_json(&self) -> String;
    pub fn hash(&self) -> String;
    pub fn from_value(v: &serde_json::Value) -> Result<CanonicalGrid, serde_json::Error>;
}
// collab/grid_geometry.rs (gated)
pub struct GridTarget { pub ra_deg: f64, pub dec_deg: f64, pub radius_deg: f64 }
pub struct FrameFootprint { pub wcs: solvemyastro::wcs::WcsSolution, pub width: f64, pub height: f64 }
pub struct GridInput { pub pixel_scale_arcsec: Option<f64>, pub footprint: Option<FrameFootprint> }
pub fn grid_input_of(meta: &ProjectFrameMeta) -> GridInput;
pub fn grid_wcs(grid: &CanonicalGrid) -> solvemyastro::wcs::WcsSolution;
pub fn propose_grid(inputs: &[GridInput], target: &GridTarget) -> Option<CanonicalGrid>;
pub fn fraction_inside(footprint: &FrameFootprint, grid: &CanonicalGrid) -> f64;
pub fn count_inside(inputs: &[GridInput], grid: &CanonicalGrid) -> usize;
// db/collab.rs (ungated)
pub fn set_canonical_grid(conn: &Connection, project_id: &str, grid_json: Option<&str>) -> Result<bool>;
pub fn canonical_grid(conn: &Connection, project_id: &str) -> Result<Option<String>>;
```
TS: nothing moves. `CanonicalGrid` derives `ts_rs::TS` here, but Task 8b registers it.

Consumed by: Task 8b (everything here), Task 7 (`grid_input_of`, `count_inside`, `fraction_inside`, `GridTarget`, `canonical_json`, `set_canonical_grid`).

- [ ] **Step 1: Write the failing pure tests**

`collab/grid.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> CanonicalGrid {
        CanonicalGrid {
            scale_arcsec: 1.87,
            width_px: 3072,
            height_px: 2048,
            crval1: 10.68,
            crval2: 41.27,
            rotation_deg: 0.0,
        }
    }

    /// Plan 6b-2 C16: the canonical form is the typed struct's JSON with
    /// sorted keys, and the hash is XXH3-64 of exactly that, 16 hex digits.
    #[test]
    fn the_canonical_json_and_the_hash_are_pinned() {
        assert_eq!(
            grid().canonical_json(),
            r#"{"crval1":10.68,"crval2":41.27,"heightPx":2048,"rotationDeg":0.0,"scaleArcsec":1.87,"widthPx":3072}"#
        );
        assert_eq!(
            grid().hash(),
            format!("{:016x}", xxhash_rust::xxh3::xxh3_64(grid().canonical_json().as_bytes()))
        );
        assert_eq!(grid().hash().len(), 16);
    }

    /// The hub stores jsonb: keys come back reordered and a whole float may
    /// come back as an integer. The hash must not see either.
    #[test]
    fn a_jsonb_echo_hashes_like_the_grid_it_echoes() {
        let echo: serde_json::Value = serde_json::from_str(
            r#"{"widthPx":3072,"crval1":10.68,"heightPx":2048,"crval2":41.27,"scaleArcsec":1.87,"rotationDeg":0}"#,
        )
        .unwrap();
        assert_eq!(CanonicalGrid::from_value(&echo).unwrap().hash(), grid().hash());
    }

    /// Postgres `numeric` drops the sign of −0.0; normalization drops it first.
    #[test]
    fn negative_zero_hashes_like_zero() {
        let negative = CanonicalGrid { rotation_deg: -0.0, crval2: -0.0, ..grid() };
        let zero = CanonicalGrid { rotation_deg: 0.0, crval2: 0.0, ..grid() };
        assert_eq!(negative.normalized().hash(), zero.normalized().hash());
    }

    #[test]
    fn normalization_puts_rotation_and_crval1_in_their_ranges() {
        for (rotation, expected) in [(180.0, 180.0), (-180.0, 180.0), (540.0, 180.0), (-190.0, 170.0), (350.0, -10.0), (0.0, 0.0)] {
            let g = CanonicalGrid { rotation_deg: rotation, ..grid() }.normalized();
            assert_eq!(g.rotation_deg, expected, "{rotation}");
        }
        for (crval1, expected) in [(360.0, 0.0), (-10.0, 350.0), (370.5, 10.5), (0.0, 0.0)] {
            let g = CanonicalGrid { crval1, ..grid() }.normalized();
            assert_eq!(g.crval1, expected, "{crval1}");
        }
    }

    /// The hub's own bounds, verbatim (athenaeum-hub `put_grid`).
    #[test]
    fn validation_mirrors_the_hub() {
        assert_eq!(grid().validate(), Ok(()));
        for bad in [
            CanonicalGrid { scale_arcsec: 0.0, ..grid() },
            CanonicalGrid { scale_arcsec: 100.0001, ..grid() },
            CanonicalGrid { width_px: 63, ..grid() },
            CanonicalGrid { height_px: 65537, ..grid() },
            CanonicalGrid { crval1: 360.0, ..grid() },
            CanonicalGrid { crval2: 90.1, ..grid() },
            CanonicalGrid { rotation_deg: f64::NAN, ..grid() },
        ] {
            assert_eq!(bad.validate(), Err(GRID_INVALID_MSG), "{bad:?}");
        }
        for ok in [
            CanonicalGrid { scale_arcsec: 100.0, ..grid() },
            CanonicalGrid { width_px: 64, height_px: 65536, ..grid() },
            CanonicalGrid { crval2: -90.0, ..grid() },
        ] {
            assert_eq!(ok.validate(), Ok(()), "{ok:?}");
        }
    }
}
```
`collab/grid_geometry.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::frame_meta::{ManifestWcs, ProjectFrameMeta};

    const TARGET: GridTarget = GridTarget { ra_deg: 10.5, dec_deg: 41.25, radius_deg: 1.0 };

    /// A frame centred at `(ra, dec)`, `w × h` pixels at 2″/px, unrotated, in
    /// the frames' own parity (det CD > 0); `crpix` 1-based as the manifest has it.
    fn meta(ra: f64, dec: f64, w: i64, h: i64, scale: Option<f64>) -> ProjectFrameMeta {
        let s = 2.0 / 3600.0;
        ProjectFrameMeta {
            exptime_sec: 300.0,
            naxis1: Some(w),
            naxis2: Some(h),
            pixel_scale_arcsec: scale,
            wcs: Some(ManifestWcs {
                crval1: ra,
                crval2: dec,
                crpix1: (w as f64 + 1.0) / 2.0,
                crpix2: (h as f64 + 1.0) / 2.0,
                cd: [s, 0.0, 0.0, s],
                sip: None,
            }),
            ..ProjectFrameMeta::default()
        }
    }

    fn scale_only(scale: f64) -> ProjectFrameMeta {
        ProjectFrameMeta { exptime_sec: 300.0, pixel_scale_arcsec: Some(scale), ..ProjectFrameMeta::default() }
    }

    #[test]
    fn the_scale_is_the_median_rounded_and_no_scale_means_no_proposal() {
        let inputs: Vec<GridInput> = [1.861, 1.884, 1.9].iter().map(|s| grid_input_of(&scale_only(*s))).collect();
        assert_eq!(propose_grid(&inputs, &TARGET).unwrap().scale_arcsec, 1.88);
        let even: Vec<GridInput> = [1.0, 2.0].iter().map(|s| grid_input_of(&scale_only(*s))).collect();
        assert_eq!(propose_grid(&even, &TARGET).unwrap().scale_arcsec, 1.5);
        let none = vec![grid_input_of(&ProjectFrameMeta { exptime_sec: 1.0, ..ProjectFrameMeta::default() })];
        assert_eq!(propose_grid(&none, &TARGET), None);
    }

    /// No frame with a WCS: a square of twice the target circle's TAN radius,
    /// rounded up to 64 — 1° at 2″/px is 1800.18 px, so 3600.4 → 3648.
    #[test]
    fn without_a_wcs_the_grid_is_the_target_square() {
        let g = propose_grid(&[grid_input_of(&scale_only(2.0))], &TARGET).unwrap();
        assert_eq!((g.width_px, g.height_px), (3648, 3648));
        assert_eq!((g.crval1, g.crval2, g.rotation_deg), (10.5, 41.25, 0.0));
    }

    /// The footprints' bounding box, symmetric about the target, rounded up
    /// to 64; clipped to the target circle when the circle is smaller.
    #[test]
    fn the_size_is_the_footprint_box_clipped_to_the_target_circle() {
        let inputs = vec![grid_input_of(&meta(10.5, 41.25, 100, 80, Some(2.0)))];
        let g = propose_grid(&inputs, &TARGET).unwrap();
        assert_eq!((g.width_px, g.height_px), (128, 128));
        let tiny = GridTarget { radius_deg: 0.01, ..TARGET };
        let g = propose_grid(&inputs, &tiny).unwrap();
        assert_eq!((g.width_px, g.height_px), (64, 64), "36 px of circle round up to 64");
    }

    #[test]
    fn the_proposal_does_not_depend_on_frame_order() {
        let a = meta(10.5, 41.25, 100, 80, Some(2.0));
        let b = meta(10.52, 41.25, 100, 80, Some(2.1));
        let c = scale_only(1.9);
        let forward = propose_grid(&[grid_input_of(&a), grid_input_of(&b), grid_input_of(&c)], &TARGET);
        let reverse = propose_grid(&[grid_input_of(&c), grid_input_of(&b), grid_input_of(&a)], &TARGET);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn a_frame_counts_as_inside_when_at_least_half_of_it_is() {
        let grid = CanonicalGrid { scale_arcsec: 2.0, width_px: 128, height_px: 128, crval1: 10.5, crval2: 41.25, rotation_deg: 0.0 };
        let inside = grid_input_of(&meta(10.5, 41.25, 100, 80, Some(2.0)));
        assert_eq!(fraction_inside(inside.footprint.as_ref().unwrap(), &grid), 1.0);
        let away = grid_input_of(&meta(12.5, 41.25, 100, 80, Some(2.0)));
        assert_eq!(fraction_inside(away.footprint.as_ref().unwrap(), &grid), 0.0);
        assert_eq!(count_inside(&[inside, away, grid_input_of(&scale_only(2.0))], &grid), 1);
    }

    /// The grid is sky-standard (C16): det CD < 0, the centre pixel maps to
    /// the centre, and the rotation reads back as `field_rotation_deg`.
    #[test]
    fn the_grid_wcs_is_sky_standard_and_reads_back_its_rotation() {
        for rotation in [0.0, 30.0, -30.0] {
            let g = CanonicalGrid { scale_arcsec: 2.0, width_px: 128, height_px: 64, crval1: 10.5, crval2: 41.25, rotation_deg: rotation };
            let wcs = grid_wcs(&g);
            let det = wcs.cd[0][0] * wcs.cd[1][1] - wcs.cd[0][1] * wcs.cd[1][0];
            assert!(det < 0.0);
            let (ra, dec) = wcs.pixel_to_sky(wcs.crpix.0, wcs.crpix.1);
            assert!((ra - 10.5).abs() < 1e-12 && (dec - 41.25).abs() < 1e-12);
            assert!((wcs.field_rotation_deg() - f64::rem_euclid(rotation, 360.0)).abs() < 1e-9, "{rotation}");
            assert_eq!(wcs.crpix, (63.5, 31.5));
        }
    }
}
```
Also `db/collab.rs`: in `upsert_project_preserves_local_columns`, add `set_canonical_grid(&conn, "p-1", Some("{\"x\":1}")).unwrap();` before the second upsert, and `assert_eq!(canonical_grid(&conn, "p-1").unwrap().as_deref(), Some("{\"x\":1}"), "the grid must survive the poll");` after it. Add:
```rust
    #[test]
    fn set_canonical_grid_reports_whether_it_changed() {
        let conn = test_conn();
        upsert_project(&conn, &sample_row("p-1")).unwrap();
        assert_eq!(canonical_grid(&conn, "p-1").unwrap(), None);
        assert!(set_canonical_grid(&conn, "p-1", Some("a")).unwrap());
        assert!(!set_canonical_grid(&conn, "p-1", Some("a")).unwrap(), "same value: no change");
        assert!(set_canonical_grid(&conn, "p-1", None).unwrap());
        assert!(!set_canonical_grid(&conn, "p-1", None).unwrap());
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 collab::grid db::collab::tests::set_canonical_grid db::collab::tests::upsert_project_preserves`

Expected: the build fails, because the modules and functions do not exist.

- [ ] **Step 3: Implement the type, the geometry and the cache columns**

`collab/grid.rs`:
```rust
//! The project's canonical grid (spec 2026-10-02 §6, plan 6b-2 C16): the
//! output geometry every processor's project masters land on. The hub's
//! `projects.canonical_grid` is the source of truth; this is its typed,
//! normalized form. Ungated: the cache (`db::collab`) and the feed read it.
//!
//! Orientation: sky-standard — north up, east left, `det(CD) < 0`
//! (`collab::grid_geometry::grid_wcs`). Every solve in the owner's catalog has
//! `det(CD) > 0`, so a grid master is the mirror image of a raw frame,
//! which is correct on the sky; no `ROWORDER` of a source frame describes it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalGrid {
    pub scale_arcsec: f64,
    pub width_px: i64,
    pub height_px: i64,
    pub crval1: f64,
    pub crval2: f64,
    pub rotation_deg: f64,
}

/// The hub's refusal text for a grid outside its bounds (athenaeum-hub
/// `put_grid`), verbatim, so a local refusal reads as the hub's would.
pub const GRID_INVALID_MSG: &str = "grid needs scaleArcsec in (0,100], widthPx/heightPx in [64,65536], crval1 in [0,360), crval2 in [-90,90], finite rotationDeg";

fn unsigned_zero(x: f64) -> f64 {
    if x == 0.0 {
        0.0
    } else {
        x
    }
}

impl CanonicalGrid {
    /// The hub's own bounds, verbatim.
    pub fn validate(&self) -> Result<(), &'static str> {
        let ok = self.scale_arcsec.is_finite()
            && self.scale_arcsec > 0.0
            && self.scale_arcsec <= 100.0
            && (64..=65536).contains(&self.width_px)
            && (64..=65536).contains(&self.height_px)
            && (0.0..360.0).contains(&self.crval1)
            && (-90.0..=90.0).contains(&self.crval2)
            && self.rotation_deg.is_finite();
        if ok {
            Ok(())
        } else {
            Err(GRID_INVALID_MSG)
        }
    }

    /// One spelling per grid, applied before the PUT and before hashing:
    /// `−0.0` → `0.0` (Postgres `numeric` loses the sign), the rotation into
    /// `(−180, 180]` (the hub only checks it is finite), `crval1` into
    /// `[0, 360)`.
    pub fn normalized(&self) -> CanonicalGrid {
        let mut rotation = self.rotation_deg % 360.0;
        if rotation <= -180.0 {
            rotation += 360.0;
        } else if rotation > 180.0 {
            rotation -= 360.0;
        }
        let mut crval1 = self.crval1.rem_euclid(360.0);
        if crval1 >= 360.0 {
            crval1 -= 360.0;
        }
        CanonicalGrid {
            scale_arcsec: unsigned_zero(self.scale_arcsec),
            width_px: self.width_px,
            height_px: self.height_px,
            crval1: unsigned_zero(crval1),
            crval2: unsigned_zero(self.crval2),
            rotation_deg: unsigned_zero(rotation),
        }
    }

    /// The canonical JSON: the typed struct through `serde_json::Value`, so
    /// keys come out sorted (this workspace's `serde_json` has no
    /// `preserve_order`) and numbers in their shortest round-trip form.
    pub fn canonical_json(&self) -> String {
        let value = serde_json::to_value(self).expect("a grid serializes");
        serde_json::to_string(&value).expect("a json value serializes")
    }

    /// `ATH_GRID`: XXH3-64 of [`Self::canonical_json`], 16 hex digits.
    /// Hash a normalized grid — the hub's jsonb echo is parsed, never hashed.
    pub fn hash(&self) -> String {
        format!("{:016x}", xxhash_rust::xxh3::xxh3_64(self.canonical_json().as_bytes()))
    }

    /// Parse a stored or echoed grid (any key order, numbers as stored) and
    /// normalize it.
    pub fn from_value(v: &serde_json::Value) -> Result<CanonicalGrid, serde_json::Error> {
        Ok(serde_json::from_value::<CanonicalGrid>(v.clone())?.normalized())
    }
}
```
`collab/grid_geometry.rs`:
```rust
//! The canonical grid's geometry (spec §6.2, plan 6b-2 C16): its TAN
//! projection, the deterministic proposal from a project's manifest, and
//! the "at least half inside" count. Gated `render+solver`: it reads manifest
//! WCS through `PlateSolveRecord::to_solution()` (SIP applied), the one TAN
//! implementation the stacking run's WCS seed shares.

use solvemyastro::wcs::WcsSolution;

use crate::collab::frame_meta::ProjectFrameMeta;
use crate::collab::grid::CanonicalGrid;

pub struct GridTarget {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

/// A frame's WCS and pixel extent.
pub struct FrameFootprint {
    pub wcs: WcsSolution,
    pub width: f64,
    pub height: f64,
}

/// One accepted frame as the proposal sees it.
pub struct GridInput {
    pub pixel_scale_arcsec: Option<f64>,
    pub footprint: Option<FrameFootprint>,
}

/// `meta`'s scale, and its footprint when it carries a WCS (SIP included)
/// and its pixel size.
pub fn grid_input_of(meta: &ProjectFrameMeta) -> GridInput {
    let footprint = match (&meta.wcs, meta.naxis1, meta.naxis2) {
        (Some(wcs), Some(w), Some(h)) if w > 0 && h > 0 => wcs
            .to_record(meta.pixel_scale_arcsec)
            .to_solution()
            .map(|wcs| FrameFootprint {
                wcs,
                width: w as f64,
                height: h as f64,
            }),
        _ => None,
    };
    GridInput {
        pixel_scale_arcsec: meta.pixel_scale_arcsec.filter(|s| s.is_finite() && *s > 0.0),
        footprint,
    }
}

/// The grid's TAN (sky-standard, C16): `crpix` the 0-based centre
/// `((W−1)/2, (H−1)/2)`, CD in the FITS CROTA2 convention with
/// `CDELT1 = −s` (east left) and `CDELT2 = +s` — `det < 0` — so
/// `field_rotation_deg()` reads `rotationDeg` back.
pub fn grid_wcs(grid: &CanonicalGrid) -> WcsSolution {
    let s = grid.scale_arcsec / 3600.0;
    let (sin, cos) = grid.rotation_deg.to_radians().sin_cos();
    WcsSolution {
        crpix: (
            (grid.width_px as f64 - 1.0) / 2.0,
            (grid.height_px as f64 - 1.0) / 2.0,
        ),
        crval: (grid.crval1, grid.crval2),
        cd: [[-s * cos, -s * sin], [-s * sin, s * cos]],
        sip_forward: None,
        sip_reverse: None,
    }
}

/// A frame's outline: corners plus eight points per edge (SIP and the
/// TAN-to-TAN mapping curve the edges slightly), at the pixel extents.
fn boundary_points(w: f64, h: f64) -> Vec<(f64, f64)> {
    const STEPS: usize = 8;
    let (x0, x1, y0, y1) = (-0.5, w - 0.5, -0.5, h - 0.5);
    let mut pts = Vec::with_capacity(4 * STEPS);
    for k in 0..STEPS {
        let t = k as f64 / STEPS as f64;
        pts.push((x0 + t * (x1 - x0), y0));
        pts.push((x1, y0 + t * (y1 - y0)));
        pts.push((x1 - t * (x1 - x0), y1));
        pts.push((x0, y1 - t * (y1 - y0)));
    }
    pts
}

/// The proposal (spec §6.2): deterministic for a manifest — sorted inputs,
/// fixed rounding.
/// - Scale: the median `pixelScaleArcsec` (even count: the mean of the two
///   middle ones), rounded to 0.01″. `None` when no frame has a scale, or the
///   rounded scale is outside the hub's `(0, 100]`.
/// - Centre: the target. Rotation: 0.
/// - Size: the bounding box, in the proposed TAN, of the WCS frames'
///   footprints, symmetric about the target and clipped to the target circle's
///   TAN radius, rounded up to a multiple of 64 and clamped to `[64, 65536]`.
///   With no WCS frame, a square of twice the radius.
pub fn propose_grid(inputs: &[GridInput], target: &GridTarget) -> Option<CanonicalGrid> {
    let mut scales: Vec<f64> = inputs.iter().filter_map(|i| i.pixel_scale_arcsec).collect();
    if scales.is_empty() {
        return None;
    }
    scales.sort_by(f64::total_cmp);
    let n = scales.len();
    let median = if n % 2 == 1 {
        scales[n / 2]
    } else {
        0.5 * (scales[n / 2 - 1] + scales[n / 2])
    };
    let scale = (median * 100.0).round() / 100.0;
    if !(scale > 0.0 && scale <= 100.0) {
        return None;
    }
    let r_px = target.radius_deg.to_radians().tan().to_degrees() * 3600.0 / scale;
    // A 1×1 grid puts the reference pixel at (0, 0): grid pixels are offsets
    // from the target.
    let tan = grid_wcs(&CanonicalGrid {
        scale_arcsec: scale,
        width_px: 1,
        height_px: 1,
        crval1: target.ra_deg,
        crval2: target.dec_deg,
        rotation_deg: 0.0,
    });
    let (mut hx, mut hy, mut any) = (0.0f64, 0.0f64, false);
    for fp in inputs.iter().filter_map(|i| i.footprint.as_ref()) {
        for (x, y) in boundary_points(fp.width, fp.height) {
            let (ra, dec) = fp.wcs.pixel_to_sky(x, y);
            let (gx, gy) = tan.sky_to_pixel(ra, dec);
            if gx.is_finite() && gy.is_finite() {
                hx = hx.max(gx.abs());
                hy = hy.max(gy.abs());
                any = true;
            }
        }
    }
    let (hx, hy) = if any { (hx.min(r_px), hy.min(r_px)) } else { (r_px, r_px) };
    let size = |half: f64| (((2.0 * half) / 64.0).ceil() as i64 * 64).clamp(64, 65536);
    Some(
        CanonicalGrid {
            scale_arcsec: scale,
            width_px: size(hx),
            height_px: size(hy),
            crval1: target.ra_deg,
            crval2: target.dec_deg,
            rotation_deg: 0.0,
        }
        .normalized(),
    )
}

/// The fraction of a frame inside the grid: a 16 × 16 grid of the frame's
/// cell centres mapped onto the grid's pixels (deterministic; no polygon
/// clip needed for a "half inside" verdict).
pub fn fraction_inside(fp: &FrameFootprint, grid: &CanonicalGrid) -> f64 {
    const N: usize = 16;
    let g = grid_wcs(grid);
    let (w, h) = (grid.width_px as f64, grid.height_px as f64);
    let mut inside = 0usize;
    for j in 0..N {
        for i in 0..N {
            let x = (i as f64 + 0.5) * fp.width / N as f64 - 0.5;
            let y = (j as f64 + 0.5) * fp.height / N as f64 - 0.5;
            let (ra, dec) = fp.wcs.pixel_to_sky(x, y);
            let (gx, gy) = g.sky_to_pixel(ra, dec);
            if gx >= -0.5 && gx < w - 0.5 && gy >= -0.5 && gy < h - 0.5 {
                inside += 1;
            }
        }
    }
    inside as f64 / (N * N) as f64
}

/// How many frames with a footprint lie at least half inside the grid
/// (spec §6.2, §6.3). A frame without a WCS cannot be judged and is not
/// counted.
pub fn count_inside(inputs: &[GridInput], grid: &CanonicalGrid) -> usize {
    inputs
        .iter()
        .filter_map(|i| i.footprint.as_ref())
        .filter(|fp| fraction_inside(fp, grid) >= 0.5)
        .count()
}
```
`db/collab.rs`, beside `set_page_extras`. In the `upsert_project` doc, change "Eleven columns" to "Twelve columns", and add "`canonical_grid_json` ([`set_canonical_grid`])" to the list.
```rust
/// The hub's canonical grid as last cached (spec §6.1, plan 6b-2 C16): the
/// canonical JSON of the normalized typed grid, never the hub's own text.
/// Written only here (`upsert_project` leaves the column alone). Returns
/// whether the stored value changed — what the refresh reports as a grid
/// move.
pub fn set_canonical_grid(conn: &Connection, project_id: &str, grid_json: Option<&str>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE collab_projects SET canonical_grid_json = ?2
         WHERE project_id = ?1 AND canonical_grid_json IS NOT ?2",
        params![project_id, grid_json],
    )?;
    Ok(n > 0)
}

/// The cached canonical grid JSON (`None` when the hub has none).
pub fn canonical_grid(conn: &Connection, project_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT canonical_grid_json FROM collab_projects WHERE project_id = ?1",
            params![project_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}
```

- [ ] **Step 4: Run the tests and watch them pass.** Run the Step 2 command, then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 collab:: db::collab` (every existing collab test passes; nothing reads the new column yet).

- [ ] **Step 5: Run the gate.** TS must not move (`git diff --exit-code src/types`). The headless check compiles `collab::grid` and the two `db::collab` functions, which are ungated.

- [ ] **Step 6: Commit** with the message `feat(collab): CanonicalGrid type, hash, normalization and the proposal geometry`, plus the trailers.

---

### Task 8b: The canonical grid end to end — hub client, cache, feed, fake hub, two commands (C16, C17; spec §6, §9.1, §9.2)

*Executed after Task 8a and before Task 7, which reads the grid state.*

**Files:**
- Modify `crates/athenaeum-core/src/collab/hub_client.rs`: `ProjectWire` (`~78-97`) and `put_grid`.
- Modify `crates/athenaeum-core/src/api/collab.rs`:
  - `client_err` becomes `pub(crate)`;
  - add `GridEcho` and `grid_echo_of`;
  - `FetchedProject`, `fetch_one_project`, `RefreshReport` and the success arm (`~2328-2700`);
  - `ProjectCard` (`~139`) and `card_from_row` (`~2148`), which degrades an unreadable cached grid to none (review I4).
- Create `crates/athenaeum-core/src/api/collab_grid.rs`, gated `render+solver`. Declare it in `api/mod.rs` beside `collab_remap`.
- Modify `crates/athenaeum-core/src/api/collab_live/feed.rs`:
  - `ALL_KINDS` (`~31`);
  - `note_card` (`~253`);
  - `apply_contiguous` (`~733-756`) and `catch_up_project` (`~922`);
  - the six refresh sites (`~553`, `~745`, `~959`, `~1164`, `~1177`, `~1401`);
  - add `emit_grid_moves`;
  - tests.
- Modify `crates/athenaeum-core/src/collab/fake_hub.rs`:
  - `FakeProject` (`~191`) and `add_project`'s literal (`~958`);
  - add `set_target`, `set_grid` and `project_grid`;
  - `project_page` (`~2130`);
  - `route` (`~2104`);
  - add `put_grid`.
- Modify `crates/athenaeum-core/src/ts_export.rs` (`models.ts`).
- Modify `crates/athenaeum-tauri/src/commands/collab.rs` and `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler!`, after `commands::cancel_collab_remap,`).
- Modify `crates/athenaeum-web/src/routes/collab.rs` and `crates/athenaeum-web/src/routes/mod.rs` (after `/api/cancel_collab_remap`).
- Modify `src/components/collab/project/testFixtures.ts` and `src/hooks/useCollabNotifications.test.tsx`: add `canonicalGrid: null,`.
- Regenerate `src/types/models.ts`.

**Interfaces:**
```rust
// collab/hub_client.rs
pub struct ProjectWire { /* … */ #[serde(default)] pub canonical_grid: Option<serde_json::Value> }
impl CollabClient { pub async fn put_grid(&self, token: &str, project_id: &str, grid: &CanonicalGrid) -> Result<(), AccountClientError>; }
// api/collab.rs
pub(crate) enum GridEcho { Absent, Grid(String), Malformed }
pub(crate) fn grid_echo_of(page: &ProjectPageWire) -> GridEcho;
pub(crate) struct RefreshReport { /* … */ pub grid_moved: Vec<String> }
pub struct ProjectCard { /* … */ pub canonical_grid: Option<CanonicalGrid> }
// api/collab_grid.rs (gated)
pub const COLLAB_GRID_CHANGED_EVENT: &str = "collab-grid-changed";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct CollabGridChanged { pub project_id: String }
#[derive(Debug, Clone, Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct GridProposal { pub current: Option<CanonicalGrid>, pub proposal: Option<CanonicalGrid>, pub frames_inside: usize, pub can_edit: bool }
pub const NO_GRID_CAP_MSG: &str;
pub fn stored_grid(conn: &Connection, project_id: &str) -> Result<Option<CanonicalGrid>, ApiError>;
pub fn grid_inputs_of_project(conn: &Connection, project_id: &str) -> Result<Vec<GridInput>, ApiError>;
pub fn get_collab_grid_proposal(ctx: &ServiceContext, project_id: &str) -> Result<GridProposal, ApiError>;
pub async fn set_collab_project_grid(ctx: &ServiceContext, project_id: &str, grid: CanonicalGrid, emitter: Option<&dyn ProgressEmitter>) -> Result<CanonicalGrid, ApiError>;
// fake hub (cfg(test))
impl FakeHub { pub fn set_target(&self, pid: &str, name: &str, ra: f64, dec: f64, radius: f64); pub fn set_grid(&self, pid: &str, grid: &CanonicalGrid); pub fn project_grid(&self, pid: &str) -> Option<Value>; }
```
TS (`models.ts`), the only moves:
- new `CanonicalGrid`, `GridProposal` and `CollabGridChanged`;
- `ProjectCard` gains `canonicalGrid: CanonicalGrid | null`.

Commands: `get_collab_grid_proposal { projectId } -> GridProposal` and `set_collab_project_grid { projectId, grid } -> CanonicalGrid`, on both hosts. The strict count goes 297 → 299. `set_collab_project_grid` emits `collab-grid-changed { projectId }` itself after a successful save (review I5): this device's own cache write makes its feed refresh a no-op, so the saving device gets the event from the command and every other device gets it from the feed.

Consumed by: Task 7 (`stored_grid`, `PlanGrid`'s inputs).

- [ ] **Step 1: The hub client, the cache read and the refresh**

`hub_client.rs` `ProjectWire`, after `goals`:
```rust
    /// The project's canonical grid (spec 2026-10-02 §6.1) as the hub stores
    /// it — jsonb, so its key order is the database's; parse it through
    /// `collab::grid::CanonicalGrid::from_value`, never hash it raw. `None`
    /// when unset or on a hub that predates it.
    #[serde(default)]
    pub canonical_grid: Option<serde_json::Value>,
```
`CollabClient`, after `patch_frame_filter`:
```rust
    /// `PUT /projects/{id}/grid` — the project's canonical grid (requires
    /// `project.edit`; the hub validates the bounds `CanonicalGrid::validate`
    /// mirrors). The hub answers 204 with no body; 200 is accepted too.
    pub async fn put_grid(
        &self,
        token: &str,
        project_id: &str,
        grid: &crate::collab::grid::CanonicalGrid,
    ) -> Result<(), AccountClientError> {
        let resp = self
            .http
            .put(self.url(&format!("/projects/{project_id}/grid")))
            .bearer_auth(token)
            .json(grid)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status == StatusCode::OK || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        Err(classify(status, resp, "put project grid").await)
    }
```
`api/collab.rs`:
  - Make `client_err` `pub(crate)`.
  - Add, beside `page_extras_of`:
```rust
/// The grid a project page carries (plan 6b-2 C16): absent, a parsed and
/// normalized grid as its canonical JSON, or a value this build cannot read
/// — then the cached grid is kept, and the reason logged. `Absent` clears
/// the cached grid on purpose: the hub omits the key, or sends null, exactly
/// when no grid is set. A hub predating `canonicalGrid` would clear it too;
/// that is unreachable today, and keeping a grid the hub no longer has would
/// be the worse error (review M10).
pub(crate) enum GridEcho {
    Absent,
    Grid(String),
    Malformed,
}

pub(crate) fn grid_echo_of(page: &crate::collab::hub_client::ProjectPageWire) -> GridEcho {
    match page.project.canonical_grid.as_ref().filter(|v| !v.is_null()) {
        None => GridEcho::Absent,
        Some(v) => match crate::collab::grid::CanonicalGrid::from_value(v) {
            Ok(grid) => GridEcho::Grid(grid.canonical_json()),
            Err(e) => {
                tracing::warn!(project_id = %page.project.id, error = %e, "project page carries a grid this build cannot read; the cached grid is kept");
                GridEcho::Malformed
            }
        },
    }
}
```
  - Add `canonical_grid: GridEcho` to `FetchedProject`.
  - In `fetch_one_project`, right after `let (goals_json, members_seen_json) = page_extras_of(&page);`, add `let canonical_grid = grid_echo_of(&page);`, and return it in the struct literal.
  - `RefreshReport` gains:
    ```rust
        /// Ids whose cached canonical grid this refresh moved (a first sight
        /// counts). Every live-feed refresh site emits `collab-grid-changed`
        /// for each.
        pub grid_moved: Vec<String>,
    ```
  - In the success arm of `refresh_projects_reporting`, destructure `canonical_grid` too. After `set_page_extras(…)`, add:
```rust
                let grid_changed = match &canonical_grid {
                    GridEcho::Absent => {
                        crate::db::collab::set_canonical_grid(&conn, &row.project_id, None)
                    }
                    GridEcho::Grid(json) => {
                        crate::db::collab::set_canonical_grid(&conn, &row.project_id, Some(json))
                    }
                    GridEcho::Malformed => Ok(false),
                }
                .map_err(internal)?;
                if grid_changed {
                    report.grid_moved.push(row.project_id.clone());
                }
```
  - `ProjectCard` gains, after `can_edit_grid`:
    ```rust
        // The project's canonical grid as last cached (spec §6.1); `None` until
        // the coordinator confirms one. Plain comment: a doc comment would leak
        // into the generated TS.
        pub canonical_grid: Option<crate::collab::grid::CanonicalGrid>,
    ```
  - In `card_from_row`, the existing `db` block reads a third tuple element, and the literal sets `canonical_grid`. One unreadable cached grid must not take the whole project list down (review I4): the card degrades to none with a `warn!`, while `get_collab_grid_proposal` and the plan stay strict.
    ```rust
    let (linked_sets, publishing, canonical_grid) = {
        let db = db(ctx)?;
        let conn = db.conn();
        (
            crate::db::collab::linked_set_ids(&conn, &row.project_id)
                .map_err(internal)?
                .len() as i64,
            crate::db::collab::publishing_device(&conn, &row.project_id).map_err(internal)?,
            crate::api::collab_grid::stored_grid(&conn, &row.project_id).unwrap_or_else(|e| {
                tracing::warn!(project_id = %row.project_id, error = %e, "cached canonical grid unreadable; card shows none");
                None
            }),
        )
    };
    ```

- [ ] **Step 2: The core commands** (`api/collab_grid.rs`, declared `#[cfg(all(feature = "render", feature = "solver"))] pub mod collab_grid;` in `api/mod.rs`):
```rust
//! The project's canonical grid at the command layer (spec 2026-10-02 §6,
//! §9.1, plan 6b-2 C16): the proposal, the coordinator's save, and the
//! `collab-grid-changed` event. The saving device emits it from the save
//! command; every other device's live feed emits it when the hub's grid moves.

use rusqlite::Connection;

use crate::account::AccountClientError;
use crate::api::collab::{client_err, has_cap, internal};
use crate::api::{db, ApiError};
use crate::collab::grid::CanonicalGrid;
use crate::collab::grid_geometry::{count_inside, grid_input_of, propose_grid, GridInput, GridTarget};
use crate::collab::hub_client::CollabClient;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;

/// Emitted by a successful `set_collab_project_grid` on the saving device,
/// and by every live-feed refresh that moved a project's cached grid.
pub const COLLAB_GRID_CHANGED_EVENT: &str = "collab-grid-changed";

/// Payload of [`COLLAB_GRID_CHANGED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabGridChanged {
    pub project_id: String,
}

/// What `get_collab_grid_proposal` answers (spec §9.1): the confirmed
/// grid, the proposal from the whole manifest, how many accepted frames with
/// a WCS lie at least half inside the proposal, and whether this account may
/// save a grid. `proposal` is `None` when no accepted frame carries a pixel
/// scale, and also when the median scale rounds outside the hub's `(0, 100]`
/// (review M8).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GridProposal {
    pub current: Option<CanonicalGrid>,
    pub proposal: Option<CanonicalGrid>,
    pub frames_inside: usize,
    pub can_edit: bool,
}

pub const NO_GRID_CAP_MSG: &str =
    "This account cannot edit the project's grid (it needs project.edit and membership).";

/// The cached confirmed grid, parsed.
pub fn stored_grid(conn: &Connection, project_id: &str) -> Result<Option<CanonicalGrid>, ApiError> {
    match crate::db::collab::canonical_grid(conn, project_id).map_err(internal)? {
        None => Ok(None),
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|e| {
            tracing::error!(project_id, error = %e, "cached canonical grid is unreadable");
            ApiError::Internal(format!("cached canonical grid of {project_id} is unreadable: {e}"))
        }),
    }
}

/// The proposal's inputs: every published, accepted frame of the manifest
/// mirror (spec §6.2 — the whole manifest, so every device proposes the same
/// grid for one manifest version), in `frame_uuid` order. A row whose
/// manifest does not parse is logged and left out.
pub fn grid_inputs_of_project(conn: &Connection, project_id: &str) -> Result<Vec<GridInput>, ApiError> {
    let rows = crate::db::collab_frames::list_for_project(conn, project_id).map_err(internal)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter().filter(|r| crate::db::collab_frames::part_of_project(r)) {
        match crate::collab::frame_meta::parse_project_frame_meta(&row.manifest_json) {
            Ok(meta) => out.push(grid_input_of(&meta)),
            Err(e) => tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "project frame metadata unreadable; left out of the grid proposal"),
        }
    }
    Ok(out)
}

/// `get_collab_grid_proposal` (spec §6.2, §9.1). DB-only.
pub fn get_collab_grid_proposal(ctx: &ServiceContext, project_id: &str) -> Result<GridProposal, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
    let can_edit = has_cap(project.is_coordinator, &project.gov_caps_json, "project.edit");
    let current = stored_grid(&conn, project_id)?;
    let inputs = grid_inputs_of_project(&conn, project_id)?;
    let target = GridTarget {
        ra_deg: project.target_ra_deg,
        dec_deg: project.target_dec_deg,
        radius_deg: project.target_radius_deg,
    };
    let proposal = propose_grid(&inputs, &target);
    let frames_inside = proposal.as_ref().map(|g| count_inside(&inputs, g)).unwrap_or(0);
    tracing::debug!(project_id, frames_inside, count = inputs.len(), "grid proposed");
    Ok(GridProposal {
        current,
        proposal,
        frames_inside,
        can_edit,
    })
}

/// `set_collab_project_grid` (spec §6.3): normalize, validate (the hub's
/// bounds), refuse a cached card without `project.edit` before any request
/// (the hub's 403 stays the backstop), `PUT /projects/{id}/grid`, and only
/// after the hub's 2xx write the local copy and emit
/// [`COLLAB_GRID_CHANGED_EVENT`]. The event comes from here because this
/// device's own cache write makes its feed refresh a no-op (review I5); it
/// is emitted on every successful save, and a re-plan is idempotent.
/// Returns the grid it sent: the hub answers 204 with no body.
///
/// A hub 400 reaches the caller as the hub client's wrapped message
/// (`hub returned 400 Bad Request (put project grid): …`, review M7). Local
/// validation mirrors the hub's own text, so the wrapper shows only when the
/// two disagree.
pub async fn set_collab_project_grid(
    ctx: &ServiceContext,
    project_id: &str,
    grid: CanonicalGrid,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<CanonicalGrid, ApiError> {
    let grid = grid.normalized();
    grid.validate().map_err(|m| ApiError::Invalid(m.to_string()))?;
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut("Sign in to edit the project's grid.".into()));
    };
    {
        let database = db(ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
        if !has_cap(project.is_coordinator, &project.gov_caps_json, "project.edit") {
            tracing::warn!(project_id, "grid refused: the cached card lacks project.edit");
            return Err(ApiError::Forbidden(NO_GRID_CAP_MSG.into()));
        }
    }
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    client
        .put_grid(&token, project_id, &grid)
        .await
        .map_err(|e| match e {
            AccountClientError::Http { status: 400, message } => ApiError::Invalid(message),
            other => client_err(other),
        })?;
    {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::db::collab::set_canonical_grid(&conn, project_id, Some(&grid.canonical_json()))
            .map_err(internal)?;
    }
    tracing::info!(project_id, grid_hash = %grid.hash(), "grid saved");
    if let Some(em) = emitter {
        emit_event(
            em,
            COLLAB_GRID_CHANGED_EVENT,
            &CollabGridChanged {
                project_id: project_id.to_string(),
            },
        );
    }
    Ok(grid)
}
```
If `CollabClient::new` takes `impl AsRef<str>` or `&str`, match `exclude_collab_frame`'s call form exactly.

- [ ] **Step 3: The feed** (`api/collab_live/feed.rs`):
  - Make `ALL_KINDS` `[ChangeKind; 6]` with `ChangeKind::Grid` appended. Update its doc: a gap catches up on the grid too.
  - In both `small_docs` matches (`apply_contiguous`, `catch_up_project`), add `| ChangeKind::Grid`.
  - In `note_card`, extend the snapshot kinds to `ChangeKind::Meta | ChangeKind::Thresholds | ChangeKind::Dictionary | ChangeKind::Grid`.
  - Delete the `if ev.kinds.contains(&ChangeKind::Grid) { tracing::debug!(…"no consumer in this wave") }` block.
  - Add to `impl FeedApplier`:
```rust
    /// Plan 6b-2 C16: `collab-grid-changed` for every project whose cached
    /// grid a refresh just moved — driven by the refresh's own report, so a
    /// manual refresh, a gap catch-up and an epoch reload each emit once and
    /// an unchanged echo emits nothing.
    fn emit_grid_moves(&self, report: &crate::api::collab::RefreshReport) {
        for project_id in &report.grid_moved {
            tracing::debug!(project_id = %project_id, "project grid changed on the hub");
            if let Some(em) = self.emitter.as_deref() {
                crate::events::emit_event(
                    em,
                    crate::api::collab_grid::COLLAB_GRID_CHANGED_EVENT,
                    &crate::api::collab_grid::CollabGridChanged {
                        project_id: project_id.clone(),
                    },
                );
            }
        }
    }
```
  - Call `self.emit_grid_moves(&report);` right after each `refresh_projects_reporting` call in this file:
    - in `apply_contiguous`, `catch_up_project`, `reload_one_project` and the account-joined arm, after the existing `let report = …`;
    - at the hello-join site, change `Ok(_) =>` to `Ok(report) => { self.emit_grid_moves(&report); … }`;
    - in the account-left arm, bind the result `let report = …?;` and emit.

    The device-replace refresh (`collab_live/replace.rs`) and the host's `refresh_projects` hold no emitter. The command returns the cards, which carry `canonicalGrid`. The saving device gets its event from `set_collab_project_grid` itself (Step 2), because its own cache write makes this refresh a no-op.

Feed test (feed.rs `mod tests`):
```rust
    struct GridRec(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl ProgressEmitter for GridRec {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }
    impl GridRec {
        fn grid_events(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == crate::api::collab_grid::COLLAB_GRID_CHANGED_EVENT)
                .map(|(_, v)| v["projectId"].as_str().unwrap().to_string())
                .collect()
        }
    }

    /// Plan 6b-2 C16: a grid saved on the hub reaches this device through
    /// the feed — the cached copy moves and `collab-grid-changed` is emitted
    /// once; a members-only event that follows emits no grid event.
    #[tokio::test]
    async fn a_grid_event_refreshes_the_cached_grid_and_emits_once() {
        let (_t, ctx, hub, _) = rig().await;
        let rec = Arc::new(GridRec(std::sync::Mutex::new(Vec::new())));
        let client = CollabClient::new(hub.uri()).unwrap();
        let mut f = FeedApplier::new(Arc::clone(&ctx), client, "tok".into(), Some(rec.clone()));
        let mut h = NoHolders(vec![]);
        f.apply(LiveEvent::Hello(hello(&hub, "e1")), &mut h).await.unwrap();
        let grid = crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.87,
            width_px: 3072,
            height_px: 2048,
            crval1: 10.5,
            crval2: 41.25,
            rotation_deg: 0.0,
        };
        hub.set_grid(PID, &grid);
        let ev = project_event_from_hub(&hub, PID);
        assert_eq!(ev.kinds, vec![ChangeKind::Grid]);
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        let stored = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::canonical_grid(&conn, PID).unwrap()
        };
        assert_eq!(stored.as_deref(), Some(grid.canonical_json().as_str()));
        assert_eq!(rec.grid_events(), vec![PID.to_string()]);

        hub.add_member(PID, "acc-x", "send_receive", false);
        let ev = project_event_from_hub(&hub, PID);
        f.apply(LiveEvent::Project(ev), &mut h).await.unwrap();
        assert_eq!(rec.grid_events().len(), 1, "an unchanged grid emits nothing");
    }
```
Re-run the existing feed pins `catch_up_project_prunes_when_its_own_small_document_refresh_reveals_a_caps_change` and the `FeedEffect::MembersChanged` assertions; they must pass unchanged.

- [ ] **Step 4: The fake hub** (`collab/fake_hub.rs`):
  - `FakeProject` gains:
    ```rust
        /// The hub's `projects.canonical_grid` (plan 6b-2): `None` until a
        /// `project.edit` member PUTs one.
        pub canonical_grid: Option<Value>,
        /// The project target the page reports: `(name, raDeg, decDeg,
        /// radiusDeg)`. M31 by default; `set_target` changes it.
        pub target: (String, f64, f64, f64),
    ```
  - `add_project`'s literal gains `canonical_grid: None, target: ("M31".to_string(), 10.68, 41.27, 1.5),`.
  - `project_page` emits `"target": {"name": p.target.0, "raDeg": p.target.1, "decDeg": p.target.2, "radiusDeg": p.target.3},` and `"canonicalGrid": p.canonical_grid,` inside `project`.
  - In `route`, after `publishing-device`, add `("PUT", ["projects", pid, "grid"]) => put_grid(st, &acct, pid, req),`.
  - Add:
```rust
fn put_grid(st: &mut FakeHubState, acct: &FakeAccount, pid: &str, req: &Request) -> ResponseTemplate {
    let grid: crate::collab::grid::CanonicalGrid = match req.body_json() {
        Ok(g) => g,
        Err(e) => return error(422, format!("bad grid body: {e}")),
    };
    let Some(p) = st.projects.get_mut(pid) else {
        return not_found_project();
    };
    let Some(member) = p.member(&acct.account_id).cloned() else {
        return empty(403);
    };
    if !member.has_cap("project.edit") {
        return empty(403);
    }
    if grid.validate().is_err() {
        return error(400, crate::collab::grid::GRID_INVALID_MSG);
    }
    p.canonical_grid = Some(serde_json::to_value(grid).expect("a grid serializes"));
    let prev = p.version;
    p.bump();
    st.publish_bump(pid, prev, &["grid"], &[]);
    empty(204)
}
```
and on `impl FakeHub`:
```rust
    /// Change the project target the page reports (no bump, no event).
    pub fn set_target(&self, pid: &str, name: &str, ra: f64, dec: f64, radius: f64) {
        let mut st = self.lock();
        let p = st.projects.get_mut(pid).expect("fake hub: no such project");
        p.target = (name.to_string(), ra, dec, radius);
    }

    /// Store a grid as a coordinator's PUT would: bump, publish `grid`.
    pub fn set_grid(&self, pid: &str, grid: &crate::collab::grid::CanonicalGrid) {
        let mut st = self.lock();
        let p = st.projects.get_mut(pid).expect("fake hub: no such project");
        p.canonical_grid = Some(serde_json::to_value(grid).expect("a grid serializes"));
        let prev = p.version;
        p.bump();
        st.publish_bump(pid, prev, &["grid"], &[]);
    }

    /// The grid the hub holds for a project.
    pub fn project_grid(&self, pid: &str) -> Option<Value> {
        self.lock().projects.get(pid).and_then(|p| p.canonical_grid.clone())
    }
```

- [ ] **Step 5: API tests** (`api/collab_grid.rs` `mod tests`, modelled on `collab_remap`'s fixture at `~456`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_exchange::test_support::{test_ctx, wire_hub};
    use crate::collab::fake_hub::FakeHub;
    use crate::collab::grid::GRID_INVALID_MSG;
    use base64::Engine;
    use std::sync::Arc;

    struct Rec(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl ProgressEmitter for Rec {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }

    fn rec() -> Rec {
        Rec(std::sync::Mutex::new(Vec::new()))
    }

    async fn fixture(coordinator: bool) -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub) {
        let hub = FakeHub::start().await;
        let (tmp, ctx) = test_ctx();
        wire_hub(&ctx, &hub.uri(), "tok");
        let b64 = base64::engine::general_purpose::STANDARD;
        hub.add_account("tok", "acc-me", "Me", &b64.encode([7u8; 32]), None);
        hub.add_account("tok-o", "acc-o", "Other", &b64.encode([8u8; 32]), None);
        hub.add_project(
            "p1",
            "m31",
            &[("acc-me", "send_receive", coordinator), ("acc-o", "send_receive", false)],
            false,
        );
        hub.set_target("p1", "M31", 10.5, 41.25, 1.0);
        let s = 2.0 / 3600.0;
        let wcs = serde_json::json!({"crval1": 10.5, "crval2": 41.25, "crpix1": 50.5, "crpix2": 40.5, "cd": [s, 0.0, 0.0, s]});
        hub.seed_frames_with("p1", "acc-o", &["w1", "w2", "w3"], "published", |f| {
            f.meta = serde_json::json!({"pixelScaleArcsec": 2.0, "naxis1": 100, "naxis2": 80, "wcs": wcs.clone()});
        });
        hub.seed_frames_with("p1", "acc-o", &["n1"], "published", |f| {
            f.meta = serde_json::json!({"pixelScaleArcsec": 2.02});
        });
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None).await.unwrap();
        (tmp, Arc::new(ctx), hub)
    }

    fn valid_grid() -> CanonicalGrid {
        CanonicalGrid { scale_arcsec: 2.0, width_px: 128, height_px: 128, crval1: 10.5, crval2: 41.25, rotation_deg: 0.0 }
    }

    /// Spec §6.2: the proposal reads the whole manifest — median scale,
    /// centred on the target, sized to the WCS footprints.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_proposal_reads_the_whole_manifest() {
        let (_t, ctx, _hub) = fixture(true).await;
        let p = get_collab_grid_proposal(&ctx, "p1").unwrap();
        assert_eq!(p.current, None);
        assert!(p.can_edit);
        let g = p.proposal.expect("frames carry a scale");
        assert_eq!((g.scale_arcsec, g.width_px, g.height_px), (2.0, 128, 128));
        assert_eq!((g.crval1, g.crval2, g.rotation_deg), (10.5, 41.25, 0.0));
        assert_eq!(p.frames_inside, 3, "the frame without a WCS is not judged");
    }

    /// Spec §6.3, C16: normalized before the PUT, cached only after the hub
    /// answers, returned as sent; the saving device gets exactly one
    /// `collab-grid-changed` from the command (review I5); the card carries
    /// the grid; a refresh echoing it back moves nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_grid_is_saved_on_the_hub_then_cached() {
        let (_t, ctx, hub) = fixture(true).await;
        let sent = CanonicalGrid { crval1: 370.5, rotation_deg: 540.0, ..valid_grid() };
        let events = rec();
        let saved = set_collab_project_grid(&ctx, "p1", sent, Some(&events)).await.unwrap();
        assert_eq!((saved.crval1, saved.rotation_deg), (10.5, 180.0));
        assert_eq!(
            *events.0.lock().unwrap(),
            vec![(COLLAB_GRID_CHANGED_EVENT.to_string(), serde_json::json!({ "projectId": "p1" }))],
            "exactly one event, from the command"
        );
        let on_hub = CanonicalGrid::from_value(&hub.project_grid("p1").expect("stored on the hub")).unwrap();
        assert_eq!(on_hub.hash(), saved.hash());
        let stored = {
            let conn = db(&ctx).unwrap().conn();
            crate::db::collab::canonical_grid(&conn, "p1").unwrap()
        };
        assert_eq!(stored.as_deref(), Some(saved.canonical_json().as_str()));
        let card = crate::api::collab::list_projects(&ctx)
            .unwrap()
            .into_iter()
            .find(|c| c.project_id == "p1")
            .unwrap();
        assert_eq!(card.canonical_grid, Some(saved));
        let report = crate::api::collab::refresh_projects_reporting(&ctx, None).await.unwrap();
        assert!(report.grid_moved.is_empty(), "an echo moves nothing: {:?}", report.grid_moved);
        assert_eq!(get_collab_grid_proposal(&ctx, "p1").unwrap().current, Some(saved));
    }

    /// The local refusals: an invalid grid before anything else, a member
    /// without `project.edit` before any request reaches the hub.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_grids_and_members_without_the_cap_are_refused_locally() {
        let (_t, ctx, hub) = fixture(false).await;
        let events = rec();
        let err = set_collab_project_grid(&ctx, "p1", CanonicalGrid { width_px: 63, ..valid_grid() }, Some(&events))
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(ref m) if m == GRID_INVALID_MSG), "{err:?}");
        let err = set_collab_project_grid(&ctx, "p1", valid_grid(), Some(&events)).await.unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(ref m) if m == NO_GRID_CAP_MSG), "{err:?}");
        assert!(hub.project_grid("p1").is_none(), "nothing reached the hub");
        assert!(events.0.lock().unwrap().is_empty(), "a refused save emits nothing");
        assert!(!get_collab_grid_proposal(&ctx, "p1").unwrap().can_edit);
    }
}
```
If `FrameViewWire.meta` cannot be assigned in the `seed_frames_with` closure, or the closure must be `Fn` and capturing `wcs` by reference fails, clone `wcs` once outside and use `move`. The intent is fixed: three frames with this WCS and one with a scale only.

- [ ] **Step 6: The hosts.** Both pass the host's emitter, as `remap_collab_filter` does. `crates/athenaeum-tauri/src/commands/collab.rs`:
```rust
/// The grid proposal for a project (spec §6.2): the confirmed grid, the
/// proposal from the manifest, how many frames lie inside it, and whether
/// this account may save one.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_grid_proposal(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<athenaeum_core::api::collab_grid::GridProposal, String> {
    athenaeum_core::api::collab_grid::get_collab_grid_proposal(&state.ctx, &project_id)
        .map_err(|e| e.to_string())
}

/// Save the project's canonical grid on the hub (`project.edit`); returns
/// the normalized grid it saved and emits `collab-grid-changed`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_project_grid(
    state: State<'_, AppState>,
    app: AppHandle,
    project_id: String,
    grid: athenaeum_core::collab::grid::CanonicalGrid,
) -> Result<athenaeum_core::collab::grid::CanonicalGrid, String> {
    let emitter = TauriProgressEmitter(app);
    athenaeum_core::api::collab_grid::set_collab_project_grid(&state.ctx, &project_id, grid, Some(&emitter))
        .await
        .map_err(|e| e.to_string())
}
```
In `lib.rs`'s `invoke_handler!`, after `commands::cancel_collab_remap,`, add `commands::get_collab_grid_proposal, commands::set_collab_project_grid,`.

`crates/athenaeum-web/src/routes/collab.rs`:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCollabProjectGridArgs {
    project_id: String,
    grid: athenaeum_core::collab::grid::CanonicalGrid,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_grid_proposal(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<athenaeum_core::api::collab_grid::GridProposal>, (axum::http::StatusCode, String)> {
    athenaeum_core::api::collab_grid::get_collab_grid_proposal(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_project_grid(
    State(state): State<WebAppState>,
    Json(args): Json<SetCollabProjectGridArgs>,
) -> Result<Json<athenaeum_core::collab::grid::CanonicalGrid>, (axum::http::StatusCode, String)> {
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    athenaeum_core::api::collab_grid::set_collab_project_grid(&state.ctx, &args.project_id, args.grid, Some(&emitter))
        .await
        .map(Json)
        .map_err(api_err)
}

#[cfg(test)]
mod grid_route_tests {
    use super::*;
    use crate::routes::tests::test_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn set_grid_args_are_camel_case() {
        let a: SetCollabProjectGridArgs = serde_json::from_value(serde_json::json!({
            "projectId": "p",
            "grid": {"scaleArcsec": 1.5, "widthPx": 64, "heightPx": 64, "crval1": 1.0, "crval2": 2.0, "rotationDeg": 0.0},
        }))
        .unwrap();
        assert_eq!((a.project_id.as_str(), a.grid.width_px), ("p", 64));
    }

    /// Both routes are registered (a DB-less state fails inside the handler,
    /// never with 404).
    #[tokio::test]
    async fn the_grid_routes_are_registered() {
        for (uri, body) in [
            ("/api/get_collab_grid_proposal", r#"{"projectId":"p1"}"#),
            (
                "/api/set_collab_project_grid",
                r#"{"projectId":"p1","grid":{"scaleArcsec":1.5,"widthPx":64,"heightPx":64,"crval1":1.0,"crval2":2.0,"rotationDeg":0.0}}"#,
            ),
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
            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }
}
```
In `routes/mod.rs`, after the `cancel_collab_remap` route, add:
```rust
        .route("/api/get_collab_grid_proposal", post(collab::get_collab_grid_proposal))
        .route("/api/set_collab_project_grid", post(collab::set_collab_project_grid))
```
`ts_export.rs` (`models.ts`): right after `crate::api::collab::ProjectCard,`, add `crate::collab::grid::CanonicalGrid, crate::api::collab_grid::GridProposal, crate::api::collab_grid::CollabGridChanged,`.

Frontend: add `canonicalGrid: null,` after `canEditGrid: false,` in `src/components/collab/project/testFixtures.ts` and in `src/hooks/useCollabNotifications.test.tsx`.

- [ ] **Step 7: Run the tests and watch them pass.**
  1. `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 collab:: api::collab_grid api::collab_live::feed db::collab`
  2. `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 grid_route_tests`
  3. Regenerate the TS: the diff must be exactly the Interfaces list.
  4. `npx tsc --noEmit -p .` and `npx vitest run src/components/collab src/hooks/useCollabNotifications.test.tsx`.
  5. The strict count:
     ```bash
     grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l
     ```
     Expected: `299`.

- [ ] **Step 8: Run the gate.** TS diff is allowed: it must be exactly the `models.ts` list.

- [ ] **Step 9: Commit** with the message `feat(collab): the canonical grid end to end — hub client, cache, feed, two commands`, plus the trailers.

---

### Task 7: `build_plan`'s project arm — blockers, warnings, grid state, Frames rows; R3's plan gate (C10, C11; spec §7, §13)

*Executed after Tasks 8a and 8b.*

**Files:**
- Create `crates/athenaeum-core/src/stacking/prepare.rs`, holding the file classifier. Declare `pub mod prepare;` in `stacking/mod.rs`.
- Modify `crates/athenaeum-core/src/stacking/paths.rs`: add `estimate_project_bytes` (refactoring `estimate_bytes`, `~624`), plus a test.
- Modify `crates/athenaeum-core/src/api/lights.rs`: `ExportReadiness` derives `Default` (`~61`).
- Modify `crates/athenaeum-core/src/stacking/plan.rs`:
  - add `GridState`, `PlanGrid` and `PlanProjectFrame`;
  - `StackingPlan` gains two fields;
  - add `SubjectInputs`, `ProjectInputs`, `frame_set_inputs` and `project_inputs`;
  - `build_plan` (`~1671-2449`);
  - delete `a_project_plan_is_refused_until_6b2`;
  - new tests.
- Modify `crates/athenaeum-core/src/stacking/test_fixtures.rs`:
  - `write_fits_i16` becomes `pub(crate)`, a visibility change only;
  - add `write_empty_primary_fits` (below);
  - `ProjectFile` gains `EmptyPrimary`, an unsupported file (the plan test of review I1; Task 9b uses it too).
- Modify `crates/athenaeum-core/src/ts_export.rs`, `stacking.ts`:
  - `GridState`, `PlanGrid` and `PlanProjectFrame` go before `StackingPlan`;
  - the preamble imports `CanonicalGrid`.
- Regenerate `src/types/stacking.ts`.

**Interfaces:**
```rust
// stacking/prepare.rs (pub mod)
#[derive(Debug, Clone, PartialEq)] pub enum FileKind { FloatFits, IntFits { bitpix: i32 }, Xisf, Unsupported(String) }
#[derive(Debug, Clone, PartialEq)] pub struct FileShape { pub kind: FileKind, pub width: usize, pub height: usize, pub planes: usize, pub cfa: Option<CfaGeometry> }
#[derive(Debug, Clone, PartialEq)] pub enum PrepareRead { InPlaceFits, ScaledFits { divisor: f64 }, Xisf }
#[derive(Debug, Clone, PartialEq)] pub enum PrepareAction { InPlace, Write { read: PrepareRead, debayer: Option<CfaGeometry> }, Unsupported(String) }
pub fn probe_file(path: &Path) -> FileShape;
pub fn prepare_action(shape: &FileShape, debayer_osc: bool) -> PrepareAction;
pub fn prepared_bytes(shape: &FileShape, action: &PrepareAction) -> u64;
// stacking/paths.rs
pub fn estimate_project_bytes(i: &EstimateInputs<'_>, grid: (u64, u64), prepared_bytes: u64) -> u64;
// stacking/plan.rs (all ts-exported)
pub enum GridState { Unset, Proposed, Confirmed }
pub struct PlanGrid { pub state: GridState, pub current: Option<CanonicalGrid>, pub proposal: Option<CanonicalGrid>, pub frames_inside: usize, pub can_edit: bool }
pub struct PlanProjectFrame { pub handle: Option<i64>, pub frame_uuid: String, pub file_name: String, pub publisher: String,
    pub filter_canonical: String, pub exptime_sec: f64, pub color_mode: ColorMode, pub has_wcs: bool, pub on_disk: bool, pub reason: Option<String> }
pub struct StackingPlan { /* … */ pub grid: Option<PlanGrid>, pub project_frames: Vec<PlanProjectFrame> }
// stacking/test_fixtures.rs
pub(crate) enum ProjectFile { Mono32, Rgb32, Mosaic32, Mono16, Xisf32, Missing, EmptyPrimary }
pub(crate) fn write_empty_primary_fits(path: &Path);
```
Tasks 9a and 9b use `probe_file`, `prepare_action` and `PrepareAction`/`PrepareRead`.

TS (`stacking.ts`), the only moves:
- new `GridState`, `PlanGrid` and `PlanProjectFrame`;
- `StackingPlan` gains `grid: PlanGrid | null` and `projectFrames: Array<PlanProjectFrame>`;
- the preamble line becomes `import type { CanonicalGrid, ExportReadiness, FlatNormMode, LightCalParams, PathSetting } from './models';`.

`build_plan` structure after this task:
1. `subject_label`.
2. The **arm** for the subject, which returns `SubjectInputs`:
   - `frame_set_inputs`: today's config resolution, frame source, readiness, masters and gate 1, moved verbatim, with gate 1 skipped and no masters for an attested set;
   - `project_inputs`: the seeded config, the project source, the grid, the project warnings and the prepared bytes.
3. The **shared core**, in today's order: EXPTIME and equivalence warnings, then the arm's warnings, then gates 2–6, staleness, and the plan.

A project's unsupported files (review I1) are plan-time exclusions: the shared core counts them out of every group's included frames and the ≥ 3 rule, so a group that Prepare would leave under three frames is blocked by `frames` at plan time. They are not added to `StackingPlan.excluded_frame_ids`, which stays the manual list, and no new blocker code exists.

The order of blockers is therefore:
- frame set: `masters, links, masterFiles, reference, folders, space, frames, unsupported` (unchanged);
- project: `grid, reference, folders, space, frames, unsupported`.

- [ ] **Step 1: Write the failing tests**

`stacking/prepare.rs` tests (the module is created in Step 3; write its test module here):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::Bayer;
    use crate::fits_writer::{write_fits_f32, write_xisf_f32, Card, CardValue};

    fn mosaic_cards() -> Vec<Card> {
        vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(0)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
        ]
    }

    use crate::stacking::test_fixtures::write_empty_primary_fits as write_empty_primary;

    fn write_distributed_xisf(path: &Path) {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><xisf version="1.0"><Image geometry="8:8:1" sampleFormat="Float32" location="path(@header_dir/data.bin)"/></xisf>"#;
        let mut bytes = b"XISF0100".to_vec();
        bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(xml.as_bytes());
        std::fs::write(path, bytes).unwrap();
    }

    /// Plan 6b-2 C12: what each kind of file becomes — in place only for a
    /// float FITS the plane reader decodes that needs no debayer.
    #[test]
    fn the_prepare_action_of_each_file_kind() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        let rggb = CfaGeometry { pattern: Bayer::Rggb, xoff: 0, yoff: 0 };
        write_fits_f32(&p("mono.fits"), 8, 8, 1, &[0.1; 64], &[]).unwrap();
        write_fits_f32(&p("rgb.fits"), 8, 8, 3, &[0.1; 192], &[]).unwrap();
        write_fits_f32(&p("mosaic.fits"), 8, 8, 1, &[0.1; 64], &mosaic_cards()).unwrap();
        crate::stacking::test_fixtures::write_fits_i16(&p("adu.fits"), 8, 8, &[100i16; 64], &[]);
        write_xisf_f32(&p("x.xisf"), 8, 8, 1, &[0.1; 64], &[]).unwrap();
        write_empty_primary(&p("tile.fits"));
        write_distributed_xisf(&p("dist.xisf"));

        let action = |n: &str, debayer| prepare_action(&probe_file(&p(n)), debayer);
        assert_eq!(action("mono.fits", true), PrepareAction::InPlace);
        assert_eq!(action("rgb.fits", true), PrepareAction::InPlace);
        assert_eq!(
            action("mosaic.fits", true),
            PrepareAction::Write { read: PrepareRead::InPlaceFits, debayer: Some(rggb) }
        );
        assert_eq!(action("mosaic.fits", false), PrepareAction::InPlace);
        assert_eq!(
            action("adu.fits", true),
            PrepareAction::Write { read: PrepareRead::ScaledFits { divisor: 65535.0 }, debayer: None }
        );
        assert_eq!(action("x.xisf", true), PrepareAction::Write { read: PrepareRead::Xisf, debayer: None });
        assert_eq!(
            action("tile.fits", true),
            PrepareAction::Unsupported("not a 1- or 3-plane 8/16/32-bit or float primary image".into())
        );
        assert_eq!(action("dist.xisf", true), PrepareAction::Unsupported("distributed XISF".into()));
        // Review I10: a file that cannot be opened says so.
        assert!(
            matches!(action("absent.fits", true), PrepareAction::Unsupported(ref m) if m.starts_with("cannot open the file: ")),
            "{:?}",
            action("absent.fits", true)
        );

        let shape = probe_file(&p("adu.fits"));
        assert_eq!((shape.width, shape.height, shape.planes), (8, 8, 1));
        assert_eq!(prepared_bytes(&shape, &action("adu.fits", true)), 8 * 8 * 4);
        let mosaic = probe_file(&p("mosaic.fits"));
        assert_eq!(prepared_bytes(&mosaic, &action("mosaic.fits", true)), 3 * 8 * 8 * 4, "the debayered copy only");
        assert_eq!(prepared_bytes(&probe_file(&p("mono.fits")), &PrepareAction::InPlace), 0);
    }
}
```
`paths.rs` tests:
```rust
    /// Plan 6b-2 C10: a project's estimate puts every registered artifact,
    /// master and drizzle output at the grid's size and counts only what
    /// Prepare writes; a frame set's estimate does not move.
    #[test]
    fn a_project_estimate_uses_the_grid_and_the_prepared_bytes() {
        let groups = vec![group(ColorMode::Mono, 3, 64, 48)];
        let inputs = EstimateInputs {
            groups: &groups,
            write_registered: true,
            write_maps: false,
            drizzle: None,
            large_scale: false,
            drizzle_bayer: false,
        };
        let expected: u64 =
            12_345 + 3 * FITS_ARTIFACT_ESTIMATE_BYTES + 3 * 1000 * 1000 * 4 + 1000 * 1000 * 4;
        assert_eq!(estimate_project_bytes(&inputs, (1000, 1000), 12_345), expected);
        let frame_set: u64 =
            3 * 64 * 48 * 4 + 3 * FITS_ARTIFACT_ESTIMATE_BYTES + 3 * 64 * 48 * 4 + 64 * 48 * 4;
        assert_eq!(estimate_bytes(&inputs), frame_set);
    }
```
`plan.rs` tests. Delete `a_project_plan_is_refused_until_6b2` and add these, with one shared fixture helper:
```rust
    /// A project `p1` on `f.conn` (target 83.8, −5.4, radius 1°; not
    /// coordinator, no caps):
    /// - `r1..r3`: filter R, WCS;
    /// - `g1, g2`: filter G, WCS, plus `g3`, which never landed;
    /// - `o1`: filter L, an OSC mosaic with no WCS;
    /// - `x1`: coordinator-excluded.
    /// Returns the working and output folders a test may use.
    fn seed_project(f: &test_fixtures::Fixture) -> (std::path::PathBuf, std::path::PathBuf) {
        use crate::stacking::test_fixtures::{add_project_frame, manifest_wcs_json, project_frame_spec, ProjectFile};
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        let wcs = || Some(manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        for (uuid, filter) in [("r1", "R"), ("r2", "R"), ("r3", "R"), ("g1", "G"), ("g2", "G")] {
            let mut spec = project_frame_spec(uuid);
            spec.filter = filter;
            spec.wcs = wcs();
            add_project_frame(&f.conn, &landed, "p1", &spec);
        }
        let mut g3 = project_frame_spec("g3");
        g3.filter = "G";
        g3.file = ProjectFile::Missing;
        add_project_frame(&f.conn, &landed, "p1", &g3);
        let mut o1 = project_frame_spec("o1");
        o1.file = ProjectFile::Mosaic32;
        o1.bayerpat = Some("RGGB");
        add_project_frame(&f.conn, &landed, "p1", &o1);
        let mut x1 = project_frame_spec("x1");
        x1.accepted = false;
        add_project_frame(&f.conn, &landed, "p1", &x1);
        (f.dir.path().join("work"), f.dir.path().join("out"))
    }

    fn with_folders(working: &std::path::Path, output: &std::path::Path) -> StackingConfig {
        let mut cfg = StackingConfig::default();
        cfg.paths.working_dir = Some(working.to_string_lossy().into_owned());
        cfg.paths.output_dir = Some(output.to_string_lossy().into_owned());
        cfg
    }

    /// Spec §7: the project plan — groups by colour × canonical filter ×
    /// class (no binning), the grid blocker for a member who cannot edit it,
    /// the not-on-this-device and solved-locally warnings, the Frames rows
    /// (a coordinator-excluded frame absent), empty readiness and no masters.
    #[test]
    fn a_project_plan_reports_its_groups_frames_grid_and_warnings() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(with_folders(&working, &output)),
        )
        .unwrap();
        assert_eq!((plan.set_id, plan.project_id.as_deref()), (None, Some("p1")));
        // `insert_test_project` sets `title = project_id` (review M6).
        assert_eq!(plan.set_name, "p1");
        let mut keys: Vec<(&str, usize)> =
            plan.groups.iter().map(|g| (g.key.as_str(), g.frame_count)).collect();
        keys.sort();
        assert_eq!(keys, vec![("mono__G__300s", 2), ("mono__R__300s", 3), ("osc__L__300s", 1)]);
        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(codes, vec!["grid"], "{:?}", plan.blockers);
        assert_eq!(plan.blockers[0].message, "Waiting for the coordinator to confirm the grid");
        for expected in [
            "1 frame of the project is not on this device and is left out (G: 1)",
            "1 frame has no WCS in the manifest and will be plate-solved locally",
        ] {
            assert!(plan.warnings.iter().any(|w| w == expected), "{expected} not in {:?}", plan.warnings);
        }
        let grid = plan.grid.as_ref().unwrap();
        assert_eq!(grid.state, GridState::Proposed);
        assert!(!grid.can_edit);
        assert_eq!(grid.proposal.unwrap().scale_arcsec, 1.5);
        assert_eq!(grid.frames_inside, 5);
        assert_eq!(plan.project_frames.len(), 7, "x1 is excluded by the coordinator, so absent");
        let g3 = plan.project_frames.iter().find(|r| r.frame_uuid == "g3").unwrap();
        assert!(!g3.on_disk && g3.handle.is_none());
        assert_eq!(g3.reason.as_deref(), Some("not on this device"));
        let o1 = plan.project_frames.iter().find(|r| r.frame_uuid == "o1").unwrap();
        assert!(!o1.has_wcs && o1.on_disk && o1.handle.is_some());
        assert_eq!(o1.color_mode, ColorMode::Osc);
        assert!(plan.masters_to_build.is_empty());
        assert_eq!(plan.readiness.total, 0);
    }

    /// Spec §6.3: a confirmed grid lifts the blocker; frames it leaves
    /// outside are counted.
    #[test]
    fn a_confirmed_grid_lifts_the_blocker_and_counts_frames_outside() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let far = crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 128,
            height_px: 128,
            crval1: 93.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        };
        crate::db::collab::set_canonical_grid(&f.conn, "p1", Some(&far.canonical_json())).unwrap();
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(with_folders(&working, &output)),
        )
        .unwrap();
        assert!(plan.blockers.iter().all(|b| b.code != "grid"), "{:?}", plan.blockers);
        let grid = plan.grid.as_ref().unwrap();
        assert_eq!((grid.state, grid.current), (GridState::Confirmed, Some(far)));
        assert!(plan.warnings.iter().any(|w| w == "5 frames lie outside the grid"), "{:?}", plan.warnings);
    }

    /// Spec §9.4: the first plan seeds the config (`stops`), and the stored
    /// exclusions (uuids) become handles.
    #[test]
    fn a_project_plan_seeds_its_config_and_maps_exclusions_to_handles() {
        let f = test_fixtures::frame_set("unused");
        seed_project(&f);
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
        let first = plan(&f);
        assert_eq!(first.config.grouping.exposure_mode, crate::stacking::config::ExposureMode::Stops);
        assert!(first.groups.iter().any(|g| g.key == "mono__R__st300"), "{:?}", first.groups);
        let stored = crate::db::stacking::get_project_config(&f.conn, "p1").unwrap().unwrap();
        crate::db::stacking::set_project_config(&f.conn, "p1", &stored.config_json, &["r1".to_string()], None).unwrap();
        let second = plan(&f);
        let r1 = second.project_frames.iter().find(|r| r.frame_uuid == "r1").unwrap().handle.unwrap();
        assert_eq!(second.excluded_frame_ids, vec![r1]);
        let r = second.groups.iter().find(|g| g.key == "mono__R__st300").unwrap();
        assert_eq!((r.frame_count, r.included_count), (3, 2));
        let again = plan(&f);
        assert_eq!(
            again.project_frames.iter().find(|r| r.frame_uuid == "r1").unwrap().handle,
            Some(r1),
            "the handle is stable across plans"
        );
    }

    /// Plan 6b-2 C6: a project working root inside a scan root is a
    /// `folders` blocker (for a frame set the overlap only warns, at save).
    #[test]
    fn a_project_working_folder_inside_a_scan_root_is_blocked() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        std::fs::create_dir_all(&working).unwrap();
        f.conn
            .execute("INSERT INTO scan_roots (path) VALUES (?1)", [working.to_str().unwrap()])
            .unwrap();
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(with_folders(&working, &output)),
        )
        .unwrap();
        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(codes, vec!["grid", "folders"], "{:?}", plan.blockers);
        assert!(plan.blockers[1].message.contains("overlaps a monitored folder"));
    }

    /// Plan 6b-2 C10: a project's blockers come in a fixed order.
    #[test]
    fn a_projects_blocker_order_is_stable() {
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec, ProjectFile};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let mut gone = project_frame_spec("a");
        gone.file = ProjectFile::Missing;
        add_project_frame(&f.conn, f.dir.path(), "p1", &gone);
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(cfg),
        )
        .unwrap();
        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(codes, vec!["grid", "reference", "folders", "folders", "frames"]);
        assert_eq!(plan.blockers[4].message, "No frame of this project is on this device yet");
    }

    /// Review I1 (plan 6b-2 C10): an unsupported file leaves the plan's
    /// included counts, so a group that Prepare would leave under three frames
    /// is blocked at plan time, not at run time. The file is not a manual
    /// exclusion.
    #[test]
    fn an_unsupported_file_does_not_count_toward_three_frames() {
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec, ProjectFile};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        for uuid in ["a", "b"] {
            add_project_frame(&f.conn, &landed, "p1", &project_frame_spec(uuid));
        }
        let mut tile = project_frame_spec("t");
        tile.file = ProjectFile::EmptyPrimary;
        add_project_frame(&f.conn, &landed, "p1", &tile);
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            None,
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1, "{:?}", plan.groups);
        let g = &plan.groups[0];
        assert_eq!((g.frame_count, g.included_count), (3, 2));
        assert!(plan.excluded_frame_ids.is_empty(), "an unsupported file is not a manual exclusion");
        assert!(plan.blockers.iter().any(|b| b.code == "frames"), "{:?}", plan.blockers);
        assert!(
            plan.warnings.iter().any(|w| w.ends_with("(unsupported file): c_t.fits")),
            "{:?}",
            plan.warnings
        );
    }

    /// Spec §13 (R3): an attested set needs no masters — no masters / links
    /// / masterFiles blocker and nothing to build; its unattested twin does.
    #[test]
    fn an_attested_set_plans_without_masters() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
        }
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(&f.conn, &SettingsManager::new(), &PathPolicy::AllowAll, &StackSubject::FrameSet(f.set_id), None).unwrap()
        };
        let calibration_codes = ["masters", "links", "masterFiles"];
        assert!(plan(&f).blockers.iter().any(|b| calibration_codes.contains(&b.code.as_str())));
        f.conn
            .execute("UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1", [f.set_id])
            .unwrap();
        let attested = plan(&f);
        assert!(attested.readiness.attested);
        assert!(attested.masters_to_build.is_empty());
        assert!(
            attested.blockers.iter().all(|b| !calibration_codes.contains(&b.code.as_str())),
            "{:?}",
            attested.blockers
        );
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::prepare a_project_estimate a_project_plan a_confirmed_grid a_project_working_folder a_projects_blocker an_unsupported_file_does_not_count an_attested_set_plans`

Expected: the build fails. `prepare`, `estimate_project_bytes`, `StackingPlan.grid` and `project_frames` do not exist, and `build_plan` still refuses a project.

- [ ] **Step 3: The classifier** (`stacking/prepare.rs`; Task 9a extends this module):
```rust
//! Prepare — stage 1 of a run whose frames are already calibrated (spec
//! 2026-10-02 §5.1, §13; plan 6b-2 C12): a project's landed replicas and an
//! attested frame set's lights. A file the plane reader decodes as unit-
//! domain float32/64 is used IN PLACE; an integer FITS or an XISF is
//! converted to float32 FITS; a CFA mosaic is debayered when `debayerOsc`
//! is on; a file no reader here can decode is excluded "unsupported file".
//! This part classifies a file from its header alone (the plan's space
//! estimate and warnings, and the run, read the same answer).

use std::path::Path;

use crate::integration::cfa::CfaGeometry;
use crate::stacking::origin::cfa_geometry_of_file;
use crate::stacking::project_source::{is_xisf, xisf_geometry};

#[derive(Debug, Clone, PartialEq)]
pub enum FileKind {
    /// A primary-HDU FITS image, BITPIX −32 / −64.
    FloatFits,
    /// A primary-HDU FITS image, BITPIX 8 / 16 / 32 (BZERO / BSCALE applied
    /// by the reader: physical ADU, not unit domain).
    IntFits { bitpix: i32 },
    /// A monolithic XISF (attachment or embedded data).
    Xisf,
    /// A file Prepare cannot read, with why.
    Unsupported(String),
}

/// What a file is, from its header.
#[derive(Debug, Clone, PartialEq)]
pub struct FileShape {
    pub kind: FileKind,
    pub width: usize,
    pub height: usize,
    pub planes: usize,
    /// The file's own CFA declaration — only ever for a one-plane image.
    pub cfa: Option<CfaGeometry>,
}

/// How Prepare reads a file it writes a copy of.
#[derive(Debug, Clone, PartialEq)]
pub enum PrepareRead {
    /// A float FITS read as it is (only ever to debayer it).
    InPlaceFits,
    /// An integer FITS: physical values ÷ `divisor` (the calibrated-light
    /// generator's own `scale_divisor_for_bitpix`), into the unit domain.
    ScaledFits { divisor: f64 },
    /// An XISF through `read_raw`, ÷ 65535 (its documented scale).
    Xisf,
}

/// What Prepare does with one file (plan 6b-2 C12).
#[derive(Debug, Clone, PartialEq)]
pub enum PrepareAction {
    /// Used in place: the `calibrated` artifact points at the file itself.
    InPlace,
    /// A float32 FITS copy is written (converted when `read` is not
    /// `InPlaceFits`), then debayered when `debayer` is set.
    Write { read: PrepareRead, debayer: Option<CfaGeometry> },
    /// The frame is excluded "unsupported file: <why>".
    Unsupported(String),
}

/// Classify a file from its header alone; never fails — an unreadable file
/// is `Unsupported` with the reason.
pub fn probe_file(path: &Path) -> FileShape {
    let unsupported = |reason: String| FileShape {
        kind: FileKind::Unsupported(reason),
        width: 0,
        height: 0,
        planes: 0,
        cfa: None,
    };
    let cfa_of = |p: &Path| match cfa_geometry_of_file(p) {
        Ok(geometry) => geometry,
        Err(e) => {
            tracing::warn!(path = %p.display(), error = %format!("{e:#}"), "reading the file's cfa header failed; treated as no mosaic");
            None
        }
    };
    // Review I10: a file that cannot be opened (offline, renamed, archived)
    // says so, instead of looking like an image no reader here decodes.
    if let Err(e) = std::fs::File::open(path) {
        return unsupported(format!("cannot open the file: {e}"));
    }
    if is_xisf(path) {
        let xml = match crate::fits_parser::read_xisf_header(path) {
            Ok(raw) => String::from_utf8_lossy(&raw).into_owned(),
            Err(e) => return unsupported(format!("unreadable XISF header: {e:#}")),
        };
        if xml.contains("location=\"path(") || xml.contains("location=\"url(") {
            return unsupported("distributed XISF".to_string());
        }
        let Some((w, h, c)) = xisf_geometry(&xml) else {
            return unsupported("XISF without an image geometry".to_string());
        };
        if c != 1 && c != 3 {
            return unsupported(format!("XISF with {c} channels"));
        }
        return FileShape {
            kind: FileKind::Xisf,
            width: w,
            height: h,
            planes: c,
            cfa: if c == 1 { cfa_of(path) } else { None },
        };
    }
    match crate::integration::banded::probe_fits(path) {
        Some((_file, info)) => FileShape {
            kind: if matches!(info.bitpix, -32 | -64) {
                FileKind::FloatFits
            } else {
                FileKind::IntFits { bitpix: info.bitpix }
            },
            width: info.w,
            height: info.h,
            planes: info.naxis3,
            cfa: if info.naxis3 == 1 { cfa_of(path) } else { None },
        },
        // Also a tile-compressed FITS or an extension image (primary NAXIS 0),
        // BITPIX 64, or a 4-plane cube.
        None => unsupported("not a 1- or 3-plane 8/16/32-bit or float primary image".to_string()),
    }
}

/// The rule of plan 6b-2 C12: in place only for a float FITS that needs no
/// debayer; a one-plane mosaic is debayered when `debayer_osc` is on; an
/// integer FITS or an XISF is converted.
pub fn prepare_action(shape: &FileShape, debayer_osc: bool) -> PrepareAction {
    let debayer = if debayer_osc && shape.planes == 1 { shape.cfa } else { None };
    match &shape.kind {
        FileKind::Unsupported(reason) => PrepareAction::Unsupported(reason.clone()),
        FileKind::FloatFits => match debayer {
            None => PrepareAction::InPlace,
            Some(geometry) => PrepareAction::Write {
                read: PrepareRead::InPlaceFits,
                debayer: Some(geometry),
            },
        },
        FileKind::IntFits { bitpix } => PrepareAction::Write {
            read: PrepareRead::ScaledFits {
                divisor: crate::calibration_library::light_cal::scale_divisor_for_bitpix(Some(*bitpix)),
            },
            debayer,
        },
        FileKind::Xisf => PrepareAction::Write {
            read: PrepareRead::Xisf,
            debayer,
        },
    }
}

/// The bytes Prepare writes for one file: the converted copy (its own planes)
/// unless the file is read in place, plus three planes when debayered.
pub fn prepared_bytes(shape: &FileShape, action: &PrepareAction) -> u64 {
    match action {
        PrepareAction::InPlace | PrepareAction::Unsupported(_) => 0,
        PrepareAction::Write { read, debayer } => {
            let plane = (shape.width * shape.height) as u64 * 4;
            let converted = if *read == PrepareRead::InPlaceFits { 0 } else { shape.planes as u64 * plane };
            let debayered = if debayer.is_some() { 3 * plane } else { 0 };
            converted + debayered
        }
    }
}
```
In `test_fixtures.rs`, make `write_fits_i16` `pub(crate)` and add:
```rust
/// A FITS whose primary HDU holds no image (`NAXIS = 0`) — what a
/// tile-compressed file or an extension image looks like to the plane
/// reader (plan 6b-2 C12: "unsupported file").
pub(crate) fn write_empty_primary_fits(path: &Path) {
    let cards = [
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "EXTEND  =                    T",
        "END",
    ];
    let mut bytes: Vec<u8> = cards.iter().flat_map(|c| format!("{c:<80}").into_bytes()).collect();
    bytes.resize(2880, b' ');
    std::fs::write(path, bytes).expect("write the empty-primary fixture");
}
```
`ProjectFile` gains the variant
```rust
    /// A primary HDU with no image (`NAXIS = 0`): an unsupported file.
    EmptyPrimary,
```
and `add_project_frame`'s match gains the arm `ProjectFile::EmptyPrimary => write_empty_primary_fits(&path),`.

- [ ] **Step 4: The project estimate** (`paths.rs`). Rename the body of `estimate_bytes` to a private `estimate_bytes_with(i, geometry: Option<(u64, u64)>, calibrated_override: Option<u64>) -> u64`, and make:
```rust
pub fn estimate_bytes(i: &EstimateInputs<'_>) -> u64 {
    estimate_bytes_with(i, None, None)
}

/// [`estimate_bytes`] for a project run (spec §7, plan 6b-2 C10): every
/// registered artifact, master, rejection bitmap and drizzle output at the
/// GRID's size — the geometry a project run delivers — and the calibrated
/// term replaced by what Prepare writes (`prepared_bytes`; a file used in
/// place costs nothing, a Bayer mosaic kept for drizzle is one of them).
pub fn estimate_project_bytes(i: &EstimateInputs<'_>, grid: (u64, u64), prepared_bytes: u64) -> u64 {
    estimate_bytes_with(i, Some(grid), Some(prepared_bytes))
}
```
Inside `estimate_bytes_with`:
- start from `let mut total = calibrated_override.unwrap_or(0);`;
- add `calibrated_bytes` per group only when `calibrated_override.is_none()`;
- take `(max_w, max_h)` from `geometry.unwrap_or_else(|| <today's largest-member computation>)`;
- add the Bayer-mosaic term only when `calibrated_override.is_none()`.

Everything else is unchanged, and every existing estimate test passes as is.

- [ ] **Step 5: The plan types** (`plan.rs`, after `PlanReference`). In `api/lights.rs`, `ExportReadiness` gains `Default` in its derive list.
```rust
/// A project plan's grid state (spec §7): `unset` — no confirmed grid and
/// no proposal (no accepted frame has a pixel scale); `proposed` — a
/// proposal, not confirmed; `confirmed` — the hub has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum GridState {
    Unset,
    Proposed,
    Confirmed,
}

/// A project plan's grid (spec §6, §7).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanGrid {
    pub state: GridState,
    pub current: Option<crate::collab::grid::CanonicalGrid>,
    pub proposal: Option<crate::collab::grid::CanonicalGrid>,
    // Stackable frames with a WCS at least half inside the grid in use
    // (the confirmed one, else the proposal).
    pub frames_inside: usize,
    pub can_edit: bool,
}

/// One row of a project plan's Frames table (spec §7): every published,
/// accepted frame of the project, stackable on this device or not.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanProjectFrame {
    // The frame's handle — `None` for a frame not stackable here (a handle
    // is minted only when a plan includes the frame).
    pub handle: Option<i64>,
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    pub filter_canonical: String,
    pub exptime_sec: f64,
    pub color_mode: ColorMode,
    // The manifest carries a WCS (else the run plate-solves the frame).
    pub has_wcs: bool,
    // Stackable on this device.
    pub on_disk: bool,
    // Why the frame is not stacked here; `None` when it is.
    pub reason: Option<String>,
}
```
`StackingPlan` gains, after `active_run_id`:
```rust
    // A project plan's grid (spec §7); `None` for a frame set.
    pub grid: Option<PlanGrid>,
    // A project plan's Frames rows (spec §7); empty for a frame set.
    pub project_frames: Vec<PlanProjectFrame>,
```

- [ ] **Step 6: The two arms and the shared core** (`plan.rs`).

Add the inputs:
```rust
/// What a subject's own arm resolves before the shared gates (plan 6b-2 C10).
struct SubjectInputs {
    cfg: StackingConfig,
    hash: String,
    groups: Vec<IntegrationGroup>,
    origins: FrameOrigins,
    excluded_frame_ids: Vec<i64>,
    readiness: ExportReadiness,
    masters_to_build: Vec<PlanMaster>,
    /// Blockers ahead of every shared gate: a frame set's gate 1
    /// (masters / links / masterFiles), a project's `grid`.
    blockers: Vec<PlanBlocker>,
    /// A project's own warnings (spec §7); empty for a frame set.
    warnings: Vec<String>,
    project: Option<ProjectInputs>,
}

/// What only a project's plan carries.
struct ProjectInputs {
    grid: PlanGrid,
    frames: Vec<PlanProjectFrame>,
    /// The scale the M4b spread is measured against (spec §7): the
    /// confirmed grid's, else the proposal's.
    grid_scale: Option<f64>,
    /// The size of the grid in use — the space estimate's registered and
    /// master geometry. `None` when there is neither grid nor proposal.
    grid_dims: Option<(u64, u64)>,
    /// What Prepare writes (conversions and debayers).
    prepared_bytes: u64,
    /// The handles of the frames Prepare cannot read (review I1): they leave
    /// the plan's included counts and the ≥ 3 rule, not `excluded_frame_ids`.
    unsupported_ids: HashSet<i64>,
}
```
**`frame_set_inputs`** is today's code, moved. Cut, from the top of `build_plan`, everything from `let (cfg, excluded_frame_ids) = resolve_config_and_exclusions(…)` through `collect_masters_to_build`, together with the whole **Gate 1** block. Paste it into:
```rust
/// The frame-set arm of [`build_plan`] — today's code, moved: the stored or
/// global config, the catalog frame source, export readiness (over the
/// frames a run will touch), the masters to build and gate 1. Read-only.
/// An attested set (spec §13, R3) is prepared, never calibrated: it needs
/// no master, so it gets no gate 1 and builds nothing.
fn frame_set_inputs(
    conn: &Connection,
    settings: &SettingsManager,
    subject: &StackSubject,
    frames_set_id: i64,
    config_override: Option<StackingConfig>,
) -> Result<SubjectInputs, ApiError> {
    let (cfg, excluded_frame_ids) =
        resolve_config_and_exclusions(conn, settings, frames_set_id, config_override)?;
    let hash = config_hash(&cfg);
    let (groups, origins) = frame_source(conn, subject, &cfg)?;
    // … the moved `excluded_set` / `light_frame_ids` / readiness lines, verbatim …
    let mut blockers: Vec<PlanBlocker> = Vec::new();
    let masters_to_build = if readiness.attested {
        Vec::new()
    } else {
        let (masters_to_build, dropped_masters) = collect_masters_to_build(conn, &readiness);
        // … the moved Gate 1 block, verbatim, pushing into `blockers` …
        masters_to_build
    };
    Ok(SubjectInputs {
        cfg,
        hash,
        groups,
        origins,
        excluded_frame_ids,
        readiness,
        masters_to_build,
        blockers,
        warnings: Vec::new(),
        project: None,
    })
}
```
**`project_inputs`:**
```rust
/// The project arm of [`build_plan`] (spec §7, plan 6b-2 C10). It WRITES —
/// a documented exception to "the plan reads only": the config seed
/// (`resolve_project_config_and_exclusions`) and the handle mint (the
/// project frame source), each one IMMEDIATE transaction; never call it with
/// a transaction open on `conn`. The masters / links / masterFiles gate never
/// applies to a project (spec §5.0): readiness is empty, nothing is built.
fn project_inputs(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
    config_override: Option<StackingConfig>,
) -> Result<SubjectInputs, ApiError> {
    use crate::collab::grid_geometry::{count_inside, fraction_inside, grid_input_of, propose_grid, GridInput, GridTarget};
    use crate::db::collab_frames::NotStackable;
    use crate::stacking::prepare::{prepare_action, prepared_bytes, probe_file, PrepareAction};

    let (cfg, excluded_uuids) =
        resolve_project_config_and_exclusions(conn, settings, project_id, config_override)?;
    let hash = config_hash(&cfg);
    let source = crate::stacking::project_source::project_frame_source(conn, project_id, &cfg)?;
    let handle_of: HashMap<&str, i64> = source
        .frames
        .iter()
        .filter_map(|f| f.handle.map(|h| (f.frame_uuid.as_str(), h)))
        .collect();
    let excluded_frame_ids: Vec<i64> = excluded_uuids
        .iter()
        .filter_map(|u| handle_of.get(u.as_str()).copied())
        .collect();
    let excluded_set: HashSet<i64> = excluded_frame_ids.iter().copied().collect();
    let in_scope = |f: &&crate::stacking::project_source::SourceFrame| {
        f.handle.is_some_and(|h| !excluded_set.contains(&h))
    };

    // The grid (spec §6.2, §6.3).
    let project = crate::api::collab_exchange::live_project(conn, project_id)?;
    let can_edit = crate::api::collab::has_cap(project.is_coordinator, &project.gov_caps_json, "project.edit");
    let current = crate::api::collab_grid::stored_grid(conn, project_id)?;
    let all_inputs: Vec<GridInput> = source.frames.iter().filter_map(|f| f.meta.as_ref()).map(grid_input_of).collect();
    let target = GridTarget {
        ra_deg: project.target_ra_deg,
        dec_deg: project.target_dec_deg,
        radius_deg: project.target_radius_deg,
    };
    let proposal = propose_grid(&all_inputs, &target);
    let state = match (&current, &proposal) {
        (Some(_), _) => GridState::Confirmed,
        (None, Some(_)) => GridState::Proposed,
        (None, None) => GridState::Unset,
    };
    let in_use = current.or(proposal);
    let in_scope_inputs: Vec<GridInput> = source.frames.iter().filter(in_scope).filter_map(|f| f.meta.as_ref()).map(grid_input_of).collect();
    let frames_inside = in_use.as_ref().map(|g| count_inside(&in_scope_inputs, g)).unwrap_or(0);

    let mut blockers = Vec::new();
    if state != GridState::Confirmed {
        blockers.push(PlanBlocker {
            code: "grid".to_string(),
            message: if can_edit {
                "Confirm the project's grid in the Stacking tab".to_string()
            } else {
                "Waiting for the coordinator to confirm the grid".to_string()
            },
        });
    }

    let mut warnings = Vec::new();
    // Spec §7: frames of the project not on this device, per canonical filter.
    let mut missing: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for f in source.frames.iter().filter(|f| f.not_stackable.as_deref() == Some(NotStackable::NotOnDevice.reason())) {
        *missing.entry(f.filter_canonical.as_str()).or_default() += 1;
    }
    let n_missing: usize = missing.values().sum();
    if n_missing > 0 {
        let list = missing.iter().map(|(filter, n)| format!("{filter}: {n}")).collect::<Vec<_>>().join(", ");
        warnings.push(if n_missing == 1 {
            format!("1 frame of the project is not on this device and is left out ({list})")
        } else {
            format!("{n_missing} frames of the project are not on this device and are left out ({list})")
        });
    }
    // Every other reason a frame is left out (re-staged, a new version on
    // its way, unreadable metadata), one line per reason.
    let mut others: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for reason in source.frames.iter().filter_map(|f| f.not_stackable.as_deref()).filter(|r| *r != NotStackable::NotOnDevice.reason()) {
        *others.entry(reason).or_default() += 1;
    }
    for (reason, n) in others {
        warnings.push(format!("{n} frame{} left out: {reason}", if n == 1 { "" } else { "s" }));
    }
    // Spec §5.2: an in-scope frame without a manifest WCS is solved locally.
    let unsolved = source.frames.iter().filter(in_scope).filter(|f| f.meta.as_ref().is_some_and(|m| m.wcs.is_none())).count();
    if unsolved > 0 {
        warnings.push(format!(
            "{unsolved} frame{} no WCS in the manifest and will be plate-solved locally",
            if unsolved == 1 { " has" } else { "s have" }
        ));
    }
    // Spec §6.3: in-scope frames a confirmed grid leaves outside.
    if let Some(grid) = &current {
        let outside = source
            .frames
            .iter()
            .filter(in_scope)
            .filter_map(|f| f.meta.as_ref())
            .filter_map(|m| grid_input_of(m).footprint)
            .filter(|fp| fraction_inside(fp, grid) < 0.5)
            .count();
        if outside > 0 {
            warnings.push(format!(
                "{outside} frame{} outside the grid",
                if outside == 1 { " lies" } else { "s lie" }
            ));
        }
    }
    // Spec §5.1 / §7: what Prepare will write, and the files it cannot read.
    let mut prepared = 0u64;
    let mut unsupported: Vec<&str> = Vec::new();
    let mut unsupported_ids: HashSet<i64> = HashSet::new();
    for g in &source.groups {
        for f in g.frames.iter().filter(|f| !excluded_set.contains(&f.frame_id)) {
            let shape = probe_file(Path::new(&f.path));
            match prepare_action(&shape, cfg.calibration.debayer_osc) {
                PrepareAction::Unsupported(_) => {
                    unsupported.push(f.filename.as_str());
                    unsupported_ids.insert(f.frame_id);
                }
                action => prepared += prepared_bytes(&shape, &action),
            }
        }
    }
    if !unsupported.is_empty() {
        const NAME_CAP: usize = 5;
        let shown = unsupported.iter().take(NAME_CAP).copied().collect::<Vec<_>>().join(", ");
        let more = unsupported.len().saturating_sub(NAME_CAP);
        let suffix = if more > 0 { format!(" and {more} more") } else { String::new() };
        let n = unsupported.len();
        warnings.push(format!(
            "{n} file{} cannot be read and {} left out (unsupported file): {shown}{suffix}",
            if n == 1 { "" } else { "s" },
            if n == 1 { "is" } else { "are" },
        ));
    }

    let frames: Vec<PlanProjectFrame> = source
        .frames
        .iter()
        .map(|f| PlanProjectFrame {
            handle: f.handle,
            frame_uuid: f.frame_uuid.clone(),
            file_name: f.file_name.clone(),
            publisher: f.publisher.clone(),
            filter_canonical: f.filter_canonical.clone(),
            exptime_sec: f.exptime_sec,
            color_mode: f.color_mode,
            has_wcs: f.meta.as_ref().is_some_and(|m| m.wcs.is_some()),
            on_disk: f.not_stackable.is_none(),
            reason: f.not_stackable.clone(),
        })
        .collect();

    Ok(SubjectInputs {
        cfg,
        hash,
        groups: source.groups,
        origins: source.origins,
        excluded_frame_ids,
        readiness: ExportReadiness::default(),
        masters_to_build: Vec::new(),
        blockers,
        warnings,
        project: Some(ProjectInputs {
            grid: PlanGrid {
                state,
                current,
                proposal,
                frames_inside,
                can_edit,
            },
            frames,
            grid_scale: in_use.map(|g| g.scale_arcsec),
            grid_dims: in_use.map(|g| (g.width_px as u64, g.height_px as u64)),
            prepared_bytes: prepared,
            unsupported_ids,
        }),
    })
}
```
Change the **shared core** of `build_plan` as follows; everything else is untouched.
1. **The start:**
```rust
    let set_name = subject_label(conn, subject)?;
    let SubjectInputs {
        cfg,
        hash,
        groups,
        origins,
        excluded_frame_ids,
        readiness,
        masters_to_build,
        blockers: arm_blockers,
        warnings: arm_warnings,
        project,
    } = match subject {
        StackSubject::FrameSet(frames_set_id) => {
            frame_set_inputs(conn, settings, subject, *frames_set_id, config_override)?
        }
        StackSubject::Project(project_id) => {
            project_inputs(conn, settings, project_id, config_override)?
        }
    };
    // Review I1: a project's unsupported files leave the included counts
    // and the ≥ 3 rule (Prepare would exclude them), but not
    // `excluded_frame_ids`, which stays the user's manual list.
    let excluded_set: HashSet<i64> = excluded_frame_ids
        .iter()
        .copied()
        .chain(project.iter().flat_map(|p| p.unsupported_ids.iter().copied()))
        .collect();
    let mut blockers: Vec<PlanBlocker> = arm_blockers;
    let mut warnings: Vec<String> = Vec::new();
```
   The `PROJECT_STACKING_UNAVAILABLE` guard and the `frames_set_id` binding go. The constant itself stays: `start_stacking` still uses it.
2. **The warnings:** after the EXPTIME and equivalence warnings, add `warnings.extend(arm_warnings);`.
3. **`reference_scale`:**
```rust
    let reference_scale: Option<f64> = match (&project, reference.frame_id) {
        (Some(p), _) => p.grid_scale,
        (None, Some(id)) => find_group_frame(&groups, id).and_then(|gf| gf.pixel_scale_arcsec),
        (None, None) => last_run_reference_scale(conn, subject, &groups)?
            .or_else(|| largest_group_scale(&groups, &excluded_set)),
    };
```
   The "far from reference" warning text for a project reads:
   ```rust
   format!("group `{}` is at {group_scale:.2} \"/px — \u{d7}{ratio:.1} the grid's {ref_scale:.2} \"/px; it is resampled onto the grid", g.key)
   ```
   Choose it with `if project.is_some() { … } else { <today's text> }`.
4. **Gate 3:** inside `Ok(validated) => { … }`, after `free_bytes` is set, add:
```rust
                    // Plan 6b-2 C6: a project's working root must not overlap
                    // any scan root (the Collaboration root is one).
                    if project.is_some() {
                        let root = paths::subject_layout(conn, &validated.working, subject)
                            .map_err(|e| ApiError::Internal(format!("{e:#}")))?
                            .root;
                        if let Some(message) = paths::project_root_overlap(conn, &root)? {
                            blockers.push(PlanBlocker {
                                code: "folders".to_string(),
                                message,
                            });
                        }
                    }
```
5. **The estimate:** bind `let estimate_inputs = EstimateInputs { … unchanged … };` and then:
```rust
    let estimate_bytes = match project.as_ref().and_then(|p| p.grid_dims.map(|d| (d, p.prepared_bytes))) {
        Some((grid, prepared)) => paths::estimate_project_bytes(&estimate_inputs, grid, prepared),
        None => paths::estimate_bytes(&estimate_inputs),
    };
```
6. **Gate 5:**
```rust
    if !any_group_has_three_included {
        blockers.push(PlanBlocker {
            code: "frames".to_string(),
            message: if project.is_some() && groups.is_empty() {
                "No frame of this project is on this device yet".to_string()
            } else {
                "At least 3 included frames in one group".to_string()
            },
        });
    }
```
7. **The debug log:**
```rust
    match subject {
        StackSubject::FrameSet(id) => tracing::debug!(
            set_id = *id, groups = plan_groups.len(), frame_count, included_count,
            blockers = blockers.len(), stale_stages = stale_stages.len(), "stacking plan built"
        ),
        StackSubject::Project(id) => tracing::debug!(
            project_id = %id, groups = plan_groups.len(), frame_count, included_count,
            blockers = blockers.len(), stale_stages = stale_stages.len(), "stacking plan built"
        ),
    }
```
8. **The `StackingPlan` literal** (its `set_id` / `project_id` come from Task 1) gains:
```rust
        grid: project.as_ref().map(|p| p.grid.clone()),
        project_frames: project.map(|p| p.frames).unwrap_or_default(),
```
Update `build_plan`'s doc: "pure DB reads plus cheap filesystem probes — for a frame set; a project's plan also seeds its config and mints frame handles (plan 6b-2 C10)". Imports needed in `plan.rs`: `std::path::Path` (already there) and `HashMap` (already there).

- [ ] **Step 7: TS registry.** In `ts_export.rs` (`stacking.ts`):
  - add `crate::stacking::plan::GridState, crate::stacking::plan::PlanGrid, crate::stacking::plan::PlanProjectFrame,` immediately before `crate::stacking::plan::StackingPlan,`;
  - change the preamble import to `import type {{ CanonicalGrid, ExportReadiness, FlatNormMode, LightCalParams, PathSetting }} from './models';`. It is inside the `format!`, so the braces are doubled.

- [ ] **Step 8: Run the tests and watch them pass.** First the Step 2 command. Then `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::plan stacking::paths`. Every existing frame-set plan test passes unchanged, including `blocker_order_is_stable`, the scale-warning suite, `plan_blocks_without_masters` and the masters/links tests. Then regenerate the TS: the diff must be exactly the Interfaces list. Run `npx tsc --noEmit -p .` (clean).

- [ ] **Step 9: Run the gate.**

- [ ] **Step 10: Commit** with the message `feat(stacking): the project plan — grid state, Frames rows, blockers and warnings; R3 plan gate`, plus the trailers.

---

### Task 9a: The Prepare writers, the debayer extraction, the `prepared/` layout and the attested hash (C11, C12; spec §5.1, §13)

**Files:**
- Modify `crates/athenaeum-core/src/export/calibrated_generator.rs`:
  - add `debayer_plane` and `debayered_cards` after `bayer_for` (`~182-210`);
  - the debayer block (`~690-723`) calls them;
  - tests.
- Modify `crates/athenaeum-core/src/stacking/prepare.rs`: add `XISF_READ_SCALE`, `source_cards_of`, `prepare_cards`, `convert_to_float_fits`, `debayer_file`, `PrepareJob`, `PreparedFiles` and `execute_prepare`, plus tests.
- Modify `crates/athenaeum-core/src/stacking/paths.rs`:
  - add `prepared_root` and `prepared_dir`;
  - `work_usage` (`~741`) and `cleanup_work` (`~840`);
  - a test.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`:
  - add `prepare_hash_for_catalog`;
  - split `HashMemo::hash_by_origin`'s catalog arm;
  - a test.
- Modify `crates/athenaeum-core/src/scanner/mod.rs`: a test in `calibrated_light_scan_tests`.

**Interfaces:**
```rust
// export/calibrated_generator.rs
pub fn debayer_plane(data: &[f32], width: usize, height: usize, geom: CfaGeometry, pool: Option<&Arc<rayon::ThreadPool>>) -> Vec<f32>;
pub fn debayered_cards(cards: &mut Vec<Card>) -> Result<(), FitsWriteError>;
// stacking/prepare.rs
pub const XISF_READ_SCALE: f64 = 65535.0;
pub fn source_cards_of(path: &Path) -> anyhow::Result<Vec<Card>>;
pub fn prepare_cards(source: &[Card], mosaic: Option<CfaGeometry>, prepared: &str) -> anyhow::Result<Vec<Card>>;
pub fn convert_to_float_fits(src: &Path, dest: &Path, read: &PrepareRead, cards: &[Card], durability: Durability) -> anyhow::Result<(usize, usize, usize)>;
pub fn debayer_file(src: &Path, dest: &Path, geom: CfaGeometry, cards: &[Card], durability: Durability,
                    pool: Option<&Arc<rayon::ThreadPool>>, cancel: &AtomicBool) -> anyhow::Result<u64>;
#[derive(Debug, Clone)] pub struct PrepareJob { pub src: PathBuf, pub shape: FileShape, pub action: PrepareAction,
    pub converted_out: PathBuf, pub debayered_out: PathBuf, pub durability: Durability }
#[derive(Debug, Clone, PartialEq)] pub struct PreparedFiles { pub calibrated: PathBuf, pub mosaic: Option<PathBuf>, pub prepared: &'static str }
pub fn execute_prepare(job: &PrepareJob, pool: Option<&Arc<rayon::ThreadPool>>, cancel: &AtomicBool) -> anyhow::Result<PreparedFiles>;
// stacking/paths.rs
impl WorkingLayout { pub fn prepared_root(&self) -> PathBuf; pub fn prepared_dir(&self, group_key: &str) -> PathBuf; }
// stacking/plan.rs
pub(crate) fn prepare_hash_for_catalog(cfg: &StackingConfig, frame: &GroupFrame) -> String;
```
Consumed by: Task 9b (`execute_prepare`, `PrepareJob`, `PreparedFiles`, `prepared_dir`, `prepare_hash_for_catalog` through `HashMemo`), Task 10b (`source_cards_of` in `adu_scaled_copy`), Task 10c (`execute_prepare` in the real-data probe), plan 6b-3 (the prepared files Register reads).

- [ ] **Step 1: Write the failing tests**

`calibrated_generator.rs` tests:
```rust
    /// An RGGB catalog light whose mosaic phase is ODD in both axes
    /// (`XBAYROFF = YBAYROFF = 1`), so the phase fold is exercised.
    fn seed_osc_odd_phase(dir: &Path) -> Connection {
        let light = write_plane(&dir.join("light_o.fits"), rggb_light);
        let dark = write_plane(&dir.join("dark.fits"), spiky_dark);
        let flat = write_plane(&dir.join("flat.fits"), |_, _| 2000.0);
        let conn = seed_db();
        seed_light(&conn, 1, &light, Some("RGGB"), Some(1));
        seed_master_set(&conn, 10, "Dark", &dark);
        seed_master_set(&conn, 11, "Flat", &flat);
        add_link(&conn, 1, 10, "Dark");
        add_link(&conn, 1, 11, "Flat");
        conn
    }

    /// Generate frame 1 of `conn` with the debayer on or off; the written path.
    fn generate(conn: &Connection, dir: &Path, debayer_osc: bool) -> PathBuf {
        let opts = CalibratedLightOptions {
            debayer_osc,
            ..CalibratedLightOptions::default()
        };
        let spec = resolve_generation(conn, 1, &opts, dir).unwrap();
        let out = dir.join(spec.output_filename("light_o.fits"));
        execute_generation(
            &spec,
            &out,
            None,
            dir,
            &opts,
            &mut HashMap::new(),
            None,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        out
    }

    /// Plan 6b-2 C12 (Review Focus 1): the extracted debayer IS the
    /// generator's. The debayer-off output is the corrected mosaic; the
    /// extracted `debayer_plane` on it, with the frame's odd phase, equals the
    /// debayer-on output bit for bit. `debayered_cards` drops the mosaic and
    /// names the method.
    #[test]
    fn debayer_plane_is_the_generators_own_debayer() {
        let dir = tempfile::tempdir().unwrap();
        let conn = seed_osc_odd_phase(dir.path());
        let mosaic_path = generate(&conn, dir.path(), false);
        let rgb_path = generate(&conn, dir.path(), true);
        let (w, h, ch, mosaic) = read_written(&mosaic_path);
        assert_eq!((w, h, ch), (W, H, 1));
        let (_, _, ch, rgb) = read_written(&rgb_path);
        assert_eq!(ch, 3);
        let geom = CfaGeometry { pattern: Bayer::Rggb, xoff: 1, yoff: 1 };
        let ours = debayer_plane(&mosaic, W, H, geom, None);
        assert_eq!(ours.len(), rgb.len());
        assert!(
            ours.iter().zip(&rgb).all(|(a, b)| a.to_bits() == b.to_bits()),
            "the extracted debayer must reproduce the generator's pixels exactly"
        );

        let mut cards = vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("FILTER", CardValue::Str("L".into())).unwrap(),
        ];
        debayered_cards(&mut cards).unwrap();
        let keys: Vec<&str> = cards.iter().map(|c| c.keyword.as_str()).collect();
        assert_eq!(keys, vec!["FILTER", "ATH_CDBM"]);
        assert_eq!(cards[1].value, Some(CardValue::Str("VNG".into())));
    }

    /// Plan 6b-2 C12: Prepare's `debayer_file`, run on the generator's own
    /// debayer-off output, writes exactly the generator's debayer-on pixels.
    #[cfg(feature = "solver")]
    #[test]
    fn debayer_file_reproduces_the_generators_debayer() {
        let dir = tempfile::tempdir().unwrap();
        let conn = seed_osc_odd_phase(dir.path());
        let mosaic_path = generate(&conn, dir.path(), false);
        let rgb_path = generate(&conn, dir.path(), true);
        let dest = dir.path().join("p_light_o_d.fits");
        let geom = CfaGeometry { pattern: Bayer::Rggb, xoff: 1, yoff: 1 };
        crate::stacking::prepare::debayer_file(
            &mosaic_path,
            &dest,
            geom,
            &[],
            Durability::Durable,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        let (_, _, ch, ours) = read_written(&dest);
        let (_, _, _, theirs) = read_written(&rgb_path);
        assert_eq!(ch, 3);
        assert!(ours.iter().zip(&theirs).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
```
`stacking/prepare.rs` tests, appended to Task 7's module. They use `AtomicBool`, `PlaneReader`, `IntegrationError` and `Durability` through `use super::*`, from the module-level imports Step 4 adds.
```rust
    fn stamp_cards() -> Vec<Card> {
        let mut cards = mosaic_cards();
        for (k, v) in [
            ("FILTER", "L"),
            ("ROWORDER", "TOP-DOWN"),
            ("CALSTAT", "BDF"),
            ("ATH_CSRC", "u1"),
            ("ATH_PRJ", "p1"),
            ("ATH_FILT", "Lum"),
        ] {
            cards.push(Card::new(k, CardValue::Str(v.into())).unwrap());
        }
        cards.push(Card::new("EXPTIME", CardValue::Real(300.0)).unwrap());
        cards
    }

    /// Plan 6b-2 C12 (Review Focus 3): an integer FITS lands in the unit
    /// domain through the generator's own divisor. Its header keeps the
    /// copy-through cards and, while it is still a mosaic, the CFA cards. It
    /// never carries a calibration or project stamp.
    #[test]
    fn an_integer_fits_converts_into_the_unit_domain() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("adu.fits");
        // Stored i16 + BZERO 32768: physical 0, 1000, …, 63000.
        let data: Vec<i16> = (0..64).map(|i| (i * 1000 - 32768) as i16).collect();
        crate::stacking::test_fixtures::write_fits_i16(&src, 8, 8, &data, &stamp_cards());
        let shape = probe_file(&src);
        let cards = prepare_cards(&source_cards_of(&src).unwrap(), shape.cfa, "converted").unwrap();
        let dest = dir.path().join("out").join("p_adu.fits");
        let dims = convert_to_float_fits(
            &src,
            &dest,
            &PrepareRead::ScaledFits { divisor: 65535.0 },
            &cards,
            Durability::Durable,
        )
        .unwrap();
        assert_eq!(dims, (8, 8, 1));
        let got = PlaneReader::open(&dest).unwrap().read_plane(0).unwrap();
        for (i, v) in got.iter().enumerate() {
            let expected = ((i * 1000) as f64 / 65535.0) as f32;
            assert_eq!(v.to_bits(), expected.to_bits(), "pixel {i}");
        }
        let h = crate::fits_parser::FitsHeader::from_path(&dest).unwrap();
        for absent in ["CALSTAT", "ATH_CSRC", "ATH_PRJ", "ATH_FILT"] {
            assert_eq!(h.get_str(absent), None, "{absent} must never be copied");
        }
        assert_eq!(h.get_str("FILTER").as_deref(), Some("L"));
        assert_eq!(h.get_str("ROWORDER").as_deref(), Some("TOP-DOWN"));
        assert_eq!(h.get_f64("EXPTIME"), Some(300.0));
        assert_eq!(h.get_str("BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(h.get_i32("XBAYROFF"), Some(0));
        assert_eq!(h.get_str("IMAGETYP").as_deref(), Some("LIGHT"));
        assert_eq!(h.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(h.get_i32("ATH_STKV"), Some(1));
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));
    }

    /// Plan 6b-2 C12: an XISF is read through `read_raw` (×65535 by
    /// contract) and divided back, so every sample lands within one ulp of the
    /// value stored; its copy-through cards come from the XISF header.
    #[test]
    fn an_xisf_converts_into_the_unit_domain_within_one_ulp() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.xisf");
        let values: Vec<f32> = (0..64).map(|i| i as f32 / 64.0).collect();
        write_xisf_f32(&src, 8, 8, 1, &values, &stamp_cards()).unwrap();
        let cards = prepare_cards(&source_cards_of(&src).unwrap(), None, "converted").unwrap();
        let dest = dir.path().join("p_x.fits");
        convert_to_float_fits(&src, &dest, &PrepareRead::Xisf, &cards, Durability::Durable).unwrap();
        let got = PlaneReader::open(&dest).unwrap().read_plane(0).unwrap();
        for (i, (g, v)) in got.iter().zip(&values).enumerate() {
            let ulps = (g.to_bits() as i64 - v.to_bits() as i64).abs();
            assert!(ulps <= 1, "pixel {i}: {g} vs {v} ({ulps} ulp)");
        }
        let h = crate::fits_parser::FitsHeader::from_path(&dest).unwrap();
        assert_eq!(h.get_str("FILTER").as_deref(), Some("L"));
        assert_eq!(h.get_str("CALSTAT"), None);
        assert_eq!(h.get_str("ATH_PRJ"), None);
    }

    /// Plan 6b-2 C12: what `execute_prepare` hands on for each action — the
    /// file itself in place; a debayered copy plus the mosaic it came from
    /// (the file itself for a float mosaic, its converted copy for an integer
    /// one); an unsupported file is an error; a cancel before the debayer
    /// writes nothing.
    #[test]
    fn execute_prepare_hands_on_the_right_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        let out = dir.path().join("prepared");
        let mono: Vec<f32> = (0..256).map(|i| (i % 7) as f32 / 10.0).collect();
        write_fits_f32(&p("mono.fits"), 16, 16, 1, &mono, &[]).unwrap();
        write_fits_f32(&p("mosaic.fits"), 16, 16, 1, &mono, &stamp_cards()).unwrap();
        let adu: Vec<i16> = (0..256).map(|i| (i * 100 - 32768) as i16).collect();
        crate::stacking::test_fixtures::write_fits_i16(&p("adu_mosaic.fits"), 16, 16, &adu, &stamp_cards());
        write_empty_primary(&p("tile.fits"));
        let job = |name: &str| {
            let src = p(name);
            let stem = Path::new(name).file_stem().unwrap().to_str().unwrap().to_string();
            let shape = probe_file(&src);
            PrepareJob {
                action: prepare_action(&shape, true),
                shape,
                src,
                converted_out: out.join(format!("p_{stem}.fits")),
                debayered_out: out.join(format!("p_{stem}_d.fits")),
                durability: Durability::Volatile,
            }
        };
        let go = AtomicBool::new(false);

        assert_eq!(
            execute_prepare(&job("mono.fits"), None, &go).unwrap(),
            PreparedFiles { calibrated: p("mono.fits"), mosaic: None, prepared: "in_place" }
        );

        let float = execute_prepare(&job("mosaic.fits"), None, &go).unwrap();
        assert_eq!(
            float,
            PreparedFiles {
                calibrated: out.join("p_mosaic_d.fits"),
                mosaic: Some(p("mosaic.fits")),
                prepared: "debayered",
            }
        );
        let h = crate::fits_parser::FitsHeader::from_path(&float.calibrated).unwrap();
        assert_eq!(h.get_i32("NAXIS3"), Some(3));
        assert_eq!(h.get_str("BAYERPAT"), None);
        assert_eq!(h.get_str("ATH_CDBM").as_deref(), Some("VNG"));
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("debayered"));
        assert_eq!(h.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(h.get_str("CALSTAT"), None);

        let integer = execute_prepare(&job("adu_mosaic.fits"), None, &go).unwrap();
        assert_eq!(integer.calibrated, out.join("p_adu_mosaic_d.fits"));
        assert_eq!(integer.mosaic, Some(out.join("p_adu_mosaic.fits")));
        let h = crate::fits_parser::FitsHeader::from_path(&out.join("p_adu_mosaic.fits")).unwrap();
        assert_eq!(h.get_str("BAYERPAT").as_deref(), Some("RGGB"), "the converted copy is still a mosaic");
        assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));

        let err = execute_prepare(&job("tile.fits"), None, &go).unwrap_err();
        assert!(format!("{err:#}").starts_with("unsupported file: "), "{err:#}");

        let stop = AtomicBool::new(true);
        let mut cancelled = job("mosaic.fits");
        cancelled.debayered_out = out.join("never.fits");
        let err = execute_prepare(&cancelled, None, &stop).unwrap_err();
        assert!(matches!(err.downcast_ref::<IntegrationError>(), Some(IntegrationError::Cancelled)));
        assert!(!out.join("never.fits").exists());
    }
```
`paths.rs` tests:
```rust
    /// Plan 6b-2 C12: Prepare's copies live in `prepared/`, count as
    /// calibrated bytes (the Calibrate slot's counter, so the TS does not
    /// move) and go with the intermediates.
    #[test]
    fn prepared_copies_count_as_calibrated_and_go_with_the_intermediates() {
        let c = conn();
        c.execute("INSERT INTO frames_set (id, name) VALUES (1, 'S')", []).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(tmp.path(), "set");
        assert_eq!(layout.prepared_root(), tmp.path().join("set").join("prepared"));
        assert_eq!(layout.prepared_dir("g"), tmp.path().join("set").join("prepared").join("g"));
        write_bytes(&layout.calibrated_dir("g").join("c_a.fits"), 100);
        write_bytes(&layout.prepared_dir("g").join("p_b.fits"), 50);
        let usage = work_usage(&layout);
        assert_eq!(usage.calibrated_bytes, 150);
        assert_eq!(usage.total_bytes, 150);
        let freed =
            cleanup_work(&c, &StackSubject::FrameSet(1), &layout, CleanupWhat::Intermediates).unwrap();
        assert_eq!(freed, 150);
        assert!(!layout.prepared_root().exists());
        assert!(layout.root.exists());
    }
```
`plan.rs` test:
```rust
    /// Spec §13 (R3), plan 6b-2 C11: an attested light's stage-1 hash is the
    /// Prepare hash over its catalog identity. No master is resolved, so a
    /// light with no calibration link still hashes.
    #[test]
    fn an_attested_lights_hash_is_the_prepare_hash_over_its_catalog_identity() {
        use crate::stacking::config::{prepare_subtree, stage_hash, SourceIdentity};
        use crate::stacking::groups::frame_source;
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
        }
        f.conn
            .execute("UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1", [f.set_id])
            .unwrap();
        let cfg = StackingConfig::default();
        let (groups, origins) = frame_source(&f.conn, &StackSubject::FrameSet(f.set_id), &cfg).unwrap();
        let frame = groups[0].frames[0].clone();
        let expected = stage_hash(
            &prepare_subtree(&cfg),
            &[],
            &[SourceIdentity {
                file_id: frame.file_id,
                size: frame.size,
                modified_at: frame.modified_at.clone(),
            }],
        );
        assert_eq!(prepare_hash_for_catalog(&cfg, &frame), expected);
        let mut memo = HashMemo::new(origins);
        assert_eq!(memo.calibration_hash_checked(&f.conn, &cfg, &frame).unwrap(), expected);
    }
```
`scanner/mod.rs`, in `calibrated_light_scan_tests`:
```rust
    /// Plan 6b-2 C12 (Review Focus 3): a file Prepare writes from a calibrated
    /// replica carries `ATH_STK` and neither `CALSTAT` nor `ATH_CSRC`, and the
    /// scanner never catalogs it — through both scan entry points.
    #[cfg(all(feature = "render", feature = "solver"))]
    #[test]
    fn scan_never_catalogs_a_prepared_file() {
        use crate::stacking::prepare::{convert_to_float_fits, prepare_cards, source_cards_of, PrepareRead};
        for parallel in [false, true] {
            let outside = TempDir::new().unwrap();
            let src = outside.path().join("c_x.fits");
            write_calibrated_light(&src, "uuid-1", "x.fits", "BDF");
            let root = TempDir::new().unwrap();
            let dest = root.path().join("prepared").join("g").join("p_c_x.fits");
            let cards = prepare_cards(&source_cards_of(&src).unwrap(), None, "converted").unwrap();
            convert_to_float_fits(
                &src,
                &dest,
                &PrepareRead::InPlaceFits,
                &cards,
                crate::fits_writer::Durability::Durable,
            )
            .unwrap();
            let h = crate::fits_parser::FitsHeader::from_path(&dest).unwrap();
            assert_eq!((h.get_str("CALSTAT"), h.get_str("ATH_CSRC")), (None, None));
            assert_eq!(h.get_str("ATH_STK").as_deref(), Some("T"));

            let conn = fresh_db(root.path(), 1);
            let result = run_scan(parallel, root.path(), &conn, 1);
            assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
            let files_total: i64 =
                conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
            assert_eq!(files_total, 0, "parallel={parallel}: a prepared file is never cataloged");
        }
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 debayer_plane_is debayer_file_reproduces stacking::prepare prepared_copies an_attested_lights_hash scan_never_catalogs_a_prepared`

Expected: the build fails, because `debayer_plane`, `debayered_cards`, the Prepare writers, `prepared_root` and `prepare_hash_for_catalog` do not exist.

- [ ] **Step 3: The debayer extraction** (`calibrated_generator.rs`). After `bayer_for`, add:
```rust
/// The VNG debayer of one CFA plane, its `XBAYROFF`/`YBAYROFF` phase folded
/// in through [`bayer_for`] — the one debayer the generator and the stacking
/// run's Prepare share (plan 6b-2 C12). Full resolution, three planes,
/// planar RGB.
pub fn debayer_plane(
    data: &[f32],
    width: usize,
    height: usize,
    geom: CfaGeometry,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<f32> {
    let pattern = bayer_for(geom);
    match pool {
        Some(p) => p.install(|| vng_debayer_f32(data, width, height, pattern)),
        None => vng_debayer_f32(data, width, height, pattern),
    }
}

/// A debayered file's header (plan 6b-2 C12): the mosaic keywords removed
/// (they describe a mosaic that no longer exists in the file) and
/// `ATH_CDBM = 'VNG'` added.
pub fn debayered_cards(cards: &mut Vec<Card>) -> Result<(), crate::fits_writer::FitsWriteError> {
    cards.retain(|c| !MOSAIC_KEYWORDS.contains(&c.keyword.as_str()));
    cards.push(Card::new("ATH_CDBM", CardValue::Str("VNG".into()))?);
    Ok(())
}
```
In `execute_generation`, the debayer match becomes:
```rust
    let (data, channels) = match (spec.debayer, spec.cfa_geometry) {
        (true, Some(geom)) => (
            debayer_plane(&frame.data, frame.width, frame.height, geom, pool),
            3usize,
        ),
        _ => (frame.data, 1usize),
    };
```
and the final-header block becomes:
```rust
    let mut cards = spec.cards.clone();
    if debayered {
        debayered_cards(&mut cards)?;
    }
```
Every other line of `execute_generation` stays as it is.

- [ ] **Step 4: The Prepare writers** (`stacking/prepare.rs`). Extend the `use` lines:
```rust
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;

use crate::calibration_library::light_resolve::card_from_kv;
use crate::export::calibrated_generator::{debayer_plane, debayered_cards};
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::{write_fits_f32_with, Card, CardValue, Durability};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::IntegrationError;
use crate::models::FileFormat;
use crate::stacking::master_cards::ATH_STK_VERSION;
use crate::stacking::register::writer::{source_cards_from_file, REGISTERED_COPY_THROUGH};
```
Then append:
```rust
/// `astroimage::ImageConverter::read_raw` returns an XISF's samples
/// bounds-normalized and multiplied by this (the reader's documented
/// contract); Prepare divides it back out, which lands within one ulp of the
/// stored value.
pub const XISF_READ_SCALE: f64 = 65535.0;

/// The copy-through cards (`REGISTERED_COPY_THROUGH`) of a FITS or XISF
/// file, from its own header — no catalog.
pub fn source_cards_of(path: &Path) -> anyhow::Result<Vec<Card>> {
    if !is_xisf(path) {
        return source_cards_from_file(path);
    }
    let raw = crate::fits_parser::read_xisf_header(path)
        .with_context(|| format!("reading the XISF header of {}", path.display()))?;
    let keys = parse_stored_header_keys(FileFormat::XISF, &String::from_utf8_lossy(&raw));
    let mut cards: Vec<Card> = keys
        .iter()
        .filter(|(k, _)| REGISTERED_COPY_THROUGH.contains(&k.to_ascii_uppercase().as_str()))
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    cards.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    Ok(cards)
}

/// The header of a file Prepare writes (plan 6b-2 C12):
/// - the source's copy-through cards;
/// - the CFA cards when the output is still a mosaic;
/// - `IMAGETYP = 'LIGHT'`;
/// - `ATH_STK = T` (a stacking intermediate, which the scanner never catalogs);
/// - `ATH_STKV`;
/// - `ATH_PREP` (`converted` | `debayered`).
/// It never carries `CALSTAT`, `ATH_CSRC`, `ATH_PRJ` or `ATH_FILT`: none of
/// them is in the copy-through list.
pub fn prepare_cards(source: &[Card], mosaic: Option<CfaGeometry>, prepared: &str) -> anyhow::Result<Vec<Card>> {
    let mut cards: Vec<Card> = source
        .iter()
        .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
        .cloned()
        .collect();
    if let Some(g) = mosaic {
        cards.push(Card::new("BAYERPAT", CardValue::Str(g.pattern.as_str().to_string()))?);
        cards.push(Card::new("XBAYROFF", CardValue::Integer(g.xoff))?);
        cards.push(Card::new("YBAYROFF", CardValue::Integer(g.yoff))?);
    }
    cards.push(Card::new("IMAGETYP", CardValue::Str("LIGHT".into()))?);
    cards.push(
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacking intermediate; never cataloged"),
    );
    cards.push(Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?);
    cards.push(
        Card::new("ATH_PREP", CardValue::Str(prepared.to_string()))?
            .with_comment("how Prepare produced this file"),
    );
    Ok(cards)
}

/// `(width, height, planes, planar data)` of `src` in the unit domain, read
/// per `read`: a FITS through the plane reader (physical values, then ÷ the
/// divisor for an integer file), an XISF through `read_raw` ÷
/// [`XISF_READ_SCALE`].
fn read_unit_planes(src: &Path, read: &PrepareRead) -> anyhow::Result<(usize, usize, usize, Vec<f32>)> {
    match read {
        PrepareRead::InPlaceFits | PrepareRead::ScaledFits { .. } => {
            let reader = PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
            let (w, h, c) = (reader.width(), reader.height(), reader.channels());
            let mut data = Vec::with_capacity(w * h * c);
            for plane in 0..c {
                data.extend(reader.read_plane(plane).with_context(|| format!("reading {}", src.display()))?);
            }
            if let PrepareRead::ScaledFits { divisor } = read {
                for v in &mut data {
                    *v = (*v as f64 / divisor) as f32;
                }
            }
            Ok((w, h, c, data))
        }
        PrepareRead::Xisf => {
            let (meta, pixels) = astroimage::ImageConverter::read_raw(src)
                .map_err(|e| anyhow::anyhow!("{}: {e:#}", src.display()))?;
            let data: Vec<f32> = match pixels {
                astroimage::PixelData::Float32(v) => {
                    v.into_iter().map(|x| (x as f64 / XISF_READ_SCALE) as f32).collect()
                }
                // `read_raw` returns `Float32` for every XISF sample format
                // (review M21); this arm exists only because the enum has two
                // variants.
                astroimage::PixelData::Uint16(v) => {
                    v.into_iter().map(|x| (x as f64 / XISF_READ_SCALE) as f32).collect()
                }
            };
            Ok((meta.width, meta.height, meta.channels, data))
        }
    }
}

/// Read `src` into the unit domain per `read` and write it as a float32 FITS
/// at `dest` with `cards` (atomic: temp file + rename). Returns `(width,
/// height, planes)`.
pub fn convert_to_float_fits(
    src: &Path,
    dest: &Path,
    read: &PrepareRead,
    cards: &[Card],
    durability: Durability,
) -> anyhow::Result<(usize, usize, usize)> {
    let (w, h, c, data) = read_unit_planes(src, read)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32_with(dest, w, h, c, &data, cards, durability)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
    Ok((w, h, c))
}

/// Debayer a one-plane float FITS mosaic at `src` into a three-plane float32
/// FITS at `dest` (plan 6b-2 C12), through the generator's own
/// [`debayer_plane`] with `geom`'s phase. `cards` are written as given (the
/// caller applies [`debayered_cards`]). It checks `cancel` before the
/// debayer, the expensive part, and fails with
/// [`IntegrationError::Cancelled`]. Returns the written file's size.
pub fn debayer_file(
    src: &Path,
    dest: &Path,
    geom: CfaGeometry,
    cards: &[Card],
    durability: Durability,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    let (w, h, mosaic) = {
        let reader = PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
        if reader.channels() != 1 {
            anyhow::bail!("{}: a {}-plane image is not a CFA mosaic", src.display(), reader.channels());
        }
        let plane = reader.read_plane(0).with_context(|| format!("reading {}", src.display()))?;
        (reader.width(), reader.height(), plane)
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled.into());
    }
    let rgb = debayer_plane(&mosaic, w, h, geom, pool);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32_with(dest, w, h, 3, &rgb, cards, durability)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
    Ok(std::fs::metadata(dest)?.len())
}

/// One frame's Prepare work, resolved on the run thread: no catalog, no
/// `RunContext`.
#[derive(Debug, Clone)]
pub struct PrepareJob {
    /// The file Prepare reads: the landed replica, or an attested light.
    pub src: PathBuf,
    pub shape: FileShape,
    pub action: PrepareAction,
    /// `prepared/<group>/p_<stem>.fits`, for a converted copy.
    pub converted_out: PathBuf,
    /// `prepared/<group>/p_<stem>_d.fits`, for a debayered copy.
    pub debayered_out: PathBuf,
    pub durability: Durability,
}

/// What Prepare hands on for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedFiles {
    /// The file stage 1 hands on (the `calibrated` artifact).
    pub calibrated: PathBuf,
    /// The CFA mosaic the debayer read (the `calibrated_mosaic` artifact
    /// when Bayer drizzle wants one): the file itself for a float mosaic,
    /// its converted copy otherwise. `None` when nothing was debayered.
    pub mosaic: Option<PathBuf>,
    /// `in_place` | `converted` | `debayered` — the `prepared` log field.
    pub prepared: &'static str,
}

/// Prepare one frame (plan 6b-2 C12):
/// - in place: nothing is written;
/// - converted: the unit-domain copy;
/// - debayered: from the file itself (a float mosaic) or from its converted
///   copy.
/// Never writes, renames or deletes `job.src`.
pub fn execute_prepare(
    job: &PrepareJob,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> anyhow::Result<PreparedFiles> {
    match &job.action {
        PrepareAction::InPlace => Ok(PreparedFiles {
            calibrated: job.src.clone(),
            mosaic: None,
            prepared: "in_place",
        }),
        PrepareAction::Unsupported(why) => anyhow::bail!("unsupported file: {why}"),
        PrepareAction::Write { read, debayer } => {
            let source = source_cards_of(&job.src)?;
            let base = if *read == PrepareRead::InPlaceFits {
                job.src.clone()
            } else {
                let mosaic = if job.shape.planes == 1 { job.shape.cfa } else { None };
                let cards = prepare_cards(&source, mosaic, "converted")?;
                convert_to_float_fits(&job.src, &job.converted_out, read, &cards, job.durability)?;
                job.converted_out.clone()
            };
            match debayer {
                None => Ok(PreparedFiles {
                    calibrated: base,
                    mosaic: None,
                    prepared: "converted",
                }),
                Some(geom) => {
                    let mut cards = prepare_cards(&source, None, "debayered")?;
                    debayered_cards(&mut cards)?;
                    debayer_file(&base, &job.debayered_out, *geom, &cards, job.durability, pool, cancel)?;
                    Ok(PreparedFiles {
                        calibrated: job.debayered_out.clone(),
                        mosaic: Some(base),
                        prepared: "debayered",
                    })
                }
            }
        }
    }
}
```
Update the module doc's last sentence to: "The classifier reads a file from its header alone; the plan's space estimate and warnings and the run read the same answer. The writers below never touch the file they read."

- [ ] **Step 5: The layout** (`paths.rs`). Add to `impl WorkingLayout`, after `calibrated_dir`:
```rust
    /// `root/prepared` — Prepare's converted and debayered copies (plan
    /// 6b-2 C12). A file used in place is never here: its artifact points at
    /// the landed file, which no cleanup ever removes.
    pub fn prepared_root(&self) -> PathBuf {
        self.root.join("prepared")
    }

    /// `root/prepared/<group_key>`.
    pub fn prepared_dir(&self, group_key: &str) -> PathBuf {
        self.prepared_root().join(group_key)
    }
```
In `work_usage`, use `let calibrated_bytes = crate::api::sync::dir_size_bytes(&layout.calibrated_root()) + crate::api::sync::dir_size_bytes(&layout.prepared_root());`. Add one line to its doc: "Prepare's copies count as calibrated bytes, because they fill stage 1's slot." Do not touch `WorkUsage`'s field docs, which are exported. In `cleanup_work`, push `layout.prepared_root()` right after `layout.calibrated_root()` in both the `Intermediates` and the `All` arms. Add one sentence to its doc: "`prepared/` goes with `calibrated/`; a `calibrated` row pointing at a file used in place loses its row, never its file."

- [ ] **Step 6: The attested hash** (`plan.rs`). Beside `prepare_hash_for_project`:
```rust
/// Stage 1's hash for an ATTESTED light (spec §13, plan 6b-2 C11): the
/// Prepare subtree and the light's own `files` identity, with no master
/// upstream, because an attested light is never calibrated.
pub(crate) fn prepare_hash_for_catalog(cfg: &StackingConfig, frame: &GroupFrame) -> String {
    stage_hash(
        &crate::stacking::config::prepare_subtree(cfg),
        &[],
        &[SourceIdentity {
            file_id: frame.file_id,
            size: frame.size,
            modified_at: frame.modified_at.clone(),
        }],
    )
}
```
`HashMemo::hash_by_origin` becomes:
```rust
        match origin_of(&self.origins, frame.frame_id)? {
            FrameOrigin::Catalog { attested: false, .. } => {
                calibration_hash_for(conn, cfg, frame, &mut self.divisors)
            }
            // Spec §13 (R3): an attested light is prepared, never calibrated.
            FrameOrigin::Catalog { attested: true, .. } => Ok(prepare_hash_for_catalog(cfg, frame)),
            FrameOrigin::Project {
                blake3, byte_size, ..
            } => Ok(prepare_hash_for_project(cfg, blake3, *byte_size)),
        }
```
Delete Task 6a's "(Task 9a splits this arm)" comment. Until Task 9b lands the stage dispatch, an attested light is still calibrated; only its stage-1 hash key changes, and no test runs an attested set before 9b.

- [ ] **Step 7: Run the tests and watch them pass.** First the Step 2 command. Then the Review Focus 1 pins, which must pass unchanged:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 export::calibrated_generator calibrate_fan_out_matches_the_sequential_stage_byte_for_byte calibrate_stage_reuses_fresh_artifacts calibrate_outputs_disambiguate turning_bayer_on_regenerates stacking::paths stage_hashes_are_pinned scanner::
```
  Expected: all pass. The generator tests pin its pixels and cards, and `stage_hashes_are_pinned_to_golden_literals` is untouched.

- [ ] **Step 8: Run the gate.** TS must not move, because `WorkUsage`'s shape is unchanged: `git diff --exit-code src/types`.

- [ ] **Step 9: Commit** with the message `feat(stacking): Prepare writers and the debayer extraction`, plus the trailers.

---

### Task 9b: The Prepare stage for projects and attested sets (C11 run half, C12, C13; spec §5.1, §13)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/origin.rs`: add `needs_prepare`, `PrepareInput` and `prepare_input`.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - add `StageOneKind` and `stage_one_kind`;
  - `stage_calibrate`'s head (`~2001-2005`);
  - `resolve_calibrate_job`'s origin match;
  - add `PrepareResolved`, `PrepareRunJob`, `expected_stage_one_planes`, `resolve_prepare_job`, `commit_prepared` and `stage_prepare`;
  - tests.

**Interfaces:**
```rust
// stacking/origin.rs
pub(crate) fn needs_prepare(origin: &FrameOrigin) -> bool;
pub(crate) struct PrepareInput { pub(crate) path: PathBuf, pub(crate) frame_uuid: Option<String> }
pub(crate) fn prepare_input(origin: &FrameOrigin, frame: &GroupFrame) -> PrepareInput;
// stacking/run.rs
enum StageOneKind { Calibrate, Prepare }
fn stage_one_kind(rc: &RunContext) -> Result<StageOneKind, RunError>;
fn stage_prepare(rc: &mut RunContext) -> Result<(), RunError>;
fn expected_stage_one_planes(cfg: &StackingConfig, subject: &StackSubject, group: &IntegrationGroup) -> usize;
```
Consumed by: Task 10b (the `calibrated` artifact the Solve sub-step solves on), plan 6b-3 (the prepared files Register reads).

The prepared files' names keep the replica's own stem: a replica `c_<uuid>.fits` becomes `p_c_<uuid>.fits` and `p_c_<uuid>_d.fits` (through `calibrated_file_stem`, so two members sharing a stem stay apart). Progress and timings report Prepare under `Stage::Calibrate`: the board keeps its ten rows, and 6c names the row "Prepare" for a project.

- [ ] **Step 1: Write the failing tests**

`run.rs` tests (they use Task 6b's `project_conn` and `project_rc`, and Task 6a's fixture):
```rust
    /// Plan 6b-2 C12 (Review Focus 3): a unit-domain float replica, mono or
    /// three-plane, is used IN PLACE. Its `calibrated` artifact is the landed
    /// file, whose bytes and mtime never change. A second pass reuses it, and
    /// `CleanupWhat::All` leaves it on disk.
    #[test]
    fn prepare_uses_a_float_replica_in_place_and_never_touches_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        let mut paths = Vec::new();
        for u in ["m1", "m2", "m3"] {
            paths.push(test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u)));
        }
        for u in ["o1", "o2", "o3"] {
            let mut spec = test_fixtures::project_frame_spec(u);
            spec.file = test_fixtures::ProjectFile::Rgb32;
            spec.bayerpat = Some("RGGB");
            paths.push(test_fixtures::add_project_frame(&conn, &landed, "p1", &spec));
        }
        let before: Vec<(Vec<u8>, std::time::SystemTime)> = paths
            .iter()
            .map(|p| (std::fs::read(p).unwrap(), std::fs::metadata(p).unwrap().modified().unwrap()))
            .collect();
        let mut rc = project_rc(
            &ctx,
            &conn,
            "p1",
            StackingConfig::default(),
            &tmp.path().join("work"),
            &tmp.path().join("out"),
        );
        let mut keys: Vec<&str> = rc.plan_groups.iter().map(|g| g.key.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["mono__L__300s", "osc__L__300s"]);

        stage_calibrate(&mut rc).unwrap();
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        let artifacts =
            crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("calibrated")).unwrap();
        assert_eq!(artifacts.len(), 6);
        for a in &artifacts {
            let path = PathBuf::from(a.path.as_deref().unwrap());
            assert!(paths.contains(&path), "used in place: {path:?}");
        }
        assert!(!rc.layout.prepared_root().exists(), "nothing was written");
        assert!(rc.cached_calibrated.values().all(|cached| !cached));

        stage_calibrate(&mut rc).unwrap();
        assert!(rc.cached_calibrated.values().all(|cached| *cached), "the second pass reuses every frame");

        crate::stacking::paths::cleanup_work(
            &conn,
            &rc.subject,
            &rc.layout,
            crate::stacking::paths::CleanupWhat::All,
        )
        .unwrap();
        for (p, (bytes, mtime)) in paths.iter().zip(&before) {
            assert_eq!(&std::fs::read(p).unwrap(), bytes, "{p:?} was rewritten");
            assert_eq!(&std::fs::metadata(p).unwrap().modified().unwrap(), mtime, "{p:?} was touched");
        }
    }

    /// Plan 6b-2 C12, C13: a CFA mosaic replica is debayered into `prepared/`
    /// when `debayerOsc` is on:
    /// - the generator's own VNG;
    /// - the mosaic cards are gone and `ATH_PREP = 'debayered'`;
    /// - no project or calibration stamp;
    /// - the landed mosaic itself is kept for Bayer drizzle.
    /// With `debayerOsc` off it is used in place, as a `cfa` group.
    #[test]
    fn prepare_debayers_a_mosaic_and_keeps_the_landed_mosaic_for_bayer_drizzle() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        for u in ["o1", "o2", "o3"] {
            let mut spec = test_fixtures::project_frame_spec(u);
            spec.file = test_fixtures::ProjectFile::Mosaic32;
            spec.bayerpat = Some("RGGB");
            test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
        }
        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.bayer = true;
        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &tmp.path().join("work"), &tmp.path().join("out"));
        assert_eq!(rc.plan_groups[0].key, "osc__L__300s");
        stage_calibrate(&mut rc).unwrap();
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        let key = "osc__L__300s";
        for f in rc.plan_groups[0].frames.clone() {
            let row = |kind: &str| {
                crate::db::stacking::find_artifact(&conn, &rc.subject, key, kind, Some(f.frame_id))
                    .unwrap()
                    .and_then(|a| a.path)
                    .unwrap()
            };
            let stem = Path::new(&f.filename).file_stem().unwrap().to_str().unwrap().to_string();
            let calibrated = PathBuf::from(row("calibrated"));
            assert_eq!(calibrated, rc.layout.prepared_dir(key).join(format!("p_{stem}_d.fits")));
            assert_eq!(row("calibrated_mosaic"), f.path, "the landed mosaic itself");
            let h = FitsHeader::from_path(&calibrated).unwrap();
            assert_eq!(h.get_i32("NAXIS3"), Some(3));
            assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("debayered"));
            assert_eq!(h.get_str("ATH_CDBM").as_deref(), Some("VNG"));
            for absent in ["BAYERPAT", "CALSTAT", "ATH_CSRC", "ATH_PRJ"] {
                assert_eq!(h.get_str(absent), None, "{absent}");
            }
        }

        let mut off = StackingConfig::default();
        off.calibration.debayer_osc = false;
        let mut rc = project_rc(&ctx, &conn, "p1", off, &tmp.path().join("work2"), &tmp.path().join("out"));
        assert_eq!(rc.plan_groups[0].key, "cfa__L__300s");
        stage_calibrate(&mut rc).unwrap();
        for f in rc.plan_groups[0].frames.clone() {
            let a = crate::db::stacking::find_artifact(&conn, &rc.subject, "cfa__L__300s", "calibrated", Some(f.frame_id))
                .unwrap()
                .unwrap();
            assert_eq!(a.path.as_deref(), Some(f.path.as_str()), "a mosaic kept as a mosaic is used in place");
        }
    }

    /// Plan 6b-2 C12: an XISF replica and a 16-bit FITS are converted into
    /// float32 FITS under `prepared/`, in the unit domain. The 16-bit file
    /// becomes exactly its physical values ÷ 65535. A float FITS beside them
    /// stays in place.
    #[test]
    fn prepare_converts_xisf_and_integer_files_into_the_unit_domain() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        let mut landed_of = HashMap::new();
        for (u, file) in [
            ("m1", test_fixtures::ProjectFile::Mono32),
            ("x1", test_fixtures::ProjectFile::Xisf32),
            ("i1", test_fixtures::ProjectFile::Mono16),
        ] {
            let mut spec = test_fixtures::project_frame_spec(u);
            spec.file = file;
            landed_of.insert(u, test_fixtures::add_project_frame(&conn, &landed, "p1", &spec));
        }
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let key = "mono__L__300s";
        stage_calibrate(&mut rc).unwrap();
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        let calibrated_of = |uuid: &str| -> PathBuf {
            let f = rc.plan_groups[0]
                .frames
                .iter()
                .find(|f| f.path == landed_of[uuid].to_str().unwrap())
                .unwrap();
            PathBuf::from(
                crate::db::stacking::find_artifact(&conn, &rc.subject, key, "calibrated", Some(f.frame_id))
                    .unwrap()
                    .and_then(|a| a.path)
                    .unwrap(),
            )
        };
        assert_eq!(calibrated_of("m1"), landed_of["m1"]);
        for uuid in ["x1", "i1"] {
            let p = calibrated_of(uuid);
            assert_eq!(p, rc.layout.prepared_dir(key).join(format!("p_c_{uuid}.fits")));
            let h = FitsHeader::from_path(&p).unwrap();
            assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));
            assert_eq!((h.get_str("CALSTAT"), h.get_str("ATH_PRJ")), (None, None));
            let plane = PlaneReader::open(&p).unwrap().read_plane(0).unwrap();
            assert!(plane.iter().all(|v| (0.0..=1.0).contains(v)), "{uuid}: unit domain");
        }
        let physical = PlaneReader::open(&landed_of["i1"]).unwrap().read_plane(0).unwrap();
        let converted = PlaneReader::open(&calibrated_of("i1")).unwrap().read_plane(0).unwrap();
        for (p, c) in physical.iter().zip(&converted) {
            assert_eq!(c.to_bits(), ((*p as f64 / 65535.0) as f32).to_bits());
        }
    }

    /// Plan 6b-2 C12: a file no reader here can decode is excluded
    /// "unsupported file: …"; its neighbours are prepared.
    #[test]
    fn prepare_excludes_an_unsupported_file() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        for u in ["m1", "m2", "m3"] {
            test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u));
        }
        let mut tile = test_fixtures::project_frame_spec("t1");
        tile.file = test_fixtures::ProjectFile::EmptyPrimary;
        test_fixtures::add_project_frame(&conn, &landed, "p1", &tile);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        stage_calibrate(&mut rc).unwrap();
        let t1 = rc.plan_groups[0].frames.iter().find(|f| f.filename == "c_t1.fits").unwrap().frame_id;
        let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert_eq!(
            reasons[&t1],
            "unsupported file: not a 1- or 3-plane 8/16/32-bit or float primary image"
        );
    }

    /// Review I12 (plan 6b-2 C13): the group's colour comes from the manifest,
    /// Prepare's debayer from the file's own header. A file whose post-Prepare
    /// plane count contradicts its group is excluded, never fed to Measure.
    /// Here the manifest has no `bayerpat` (a `mono` group) while the file
    /// declares a mosaic, which Prepare would debayer to three planes.
    #[test]
    fn prepare_excludes_a_file_whose_planes_contradict_its_group() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        for u in ["m1", "m2", "m3"] {
            test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u));
        }
        // `Mosaic32` writes BAYERPAT RGGB into the file; `bayerpat: None`
        // keeps it out of the manifest.
        let mut odd = test_fixtures::project_frame_spec("x1");
        odd.file = test_fixtures::ProjectFile::Mosaic32;
        test_fixtures::add_project_frame(&conn, &landed, "p1", &odd);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        assert_eq!(rc.plan_groups.len(), 1, "every frame is in the mono group");
        assert_eq!(rc.plan_groups[0].key, "mono__L__300s");
        stage_calibrate(&mut rc).unwrap();
        let x1 = rc.plan_groups[0].frames.iter().find(|f| f.filename == "c_x1.fits").unwrap().frame_id;
        let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert_eq!(reasons[&x1], "plane count 3 disagrees with the group's colour mode");
        assert!(!rc.layout.prepared_root().exists(), "nothing was written for it");
    }

    /// Plan 6b-2 C9 (the Prepare boundary): a landed file the exchange
    /// rewrote after the frame source read its row is excluded "changed
    /// during the run" before Prepare reads it.
    #[test]
    fn prepare_excludes_a_project_frame_whose_landed_file_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        let paths: Vec<PathBuf> = ["a", "b", "c", "d"]
            .iter()
            .map(|u| test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u)))
            .collect();
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        std::fs::write(&paths[1], vec![0u8; 5000]).unwrap();
        stage_calibrate(&mut rc).unwrap();
        let b = rc.plan_groups[0].frames.iter().find(|f| f.filename == "c_b.fits").unwrap().frame_id;
        let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert_eq!(reasons[&b], crate::stacking::origin::CHANGED_DURING_THE_RUN);
        assert!(crate::db::stacking::find_artifact(&conn, &rc.subject, "mono__L__300s", "calibrated", Some(b))
            .unwrap()
            .is_none());
    }

    /// Spec §15.1 "Seam" (Review Focus 2): Prepare never reads the catalog
    /// for a project frame. `files`/`frames` rows whose ids equal the
    /// handles, pointing at nothing, are never followed, and the stage-1
    /// hash is the Prepare hash over the manifest's content identity.
    #[test]
    fn prepare_never_reads_the_catalog_for_a_project() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        let landed = tmp.path().join("collab");
        for u in ["a", "b", "c"] {
            test_fixtures::add_project_frame(&conn, &landed, "p1", &test_fixtures::project_frame_spec(u));
        }
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let handles: Vec<i64> = rc.origins.keys().copied().collect();
        for (i, h) in handles.iter().enumerate() {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'decoy.fits', 7, '2020-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![h, format!("/decoy/{i}.fits")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, filter, exptime, naxis1, naxis2, bayerpat)
                 VALUES (?1, ?1, 'Light', 'Z', 999.0, 7, 7, 'RGGB')",
                [h],
            )
            .unwrap();
        }
        stage_calibrate(&mut rc).unwrap();
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        for f in rc.plan_groups[0].frames.clone() {
            let a = crate::db::stacking::find_artifact(&conn, &rc.subject, "mono__L__300s", "calibrated", Some(f.frame_id))
                .unwrap()
                .unwrap();
            assert_eq!(a.path.as_deref(), Some(f.path.as_str()));
            let (blake3, size): (String, i64) = conn
                .query_row(
                    "SELECT blake3, byte_size FROM project_frames_local WHERE project_id = 'p1' AND landed_path = ?1",
                    [&f.path],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(a.config_hash, crate::stacking::plan::prepare_hash_for_project(&rc.config, &blake3, size));
        }
        let decoys: i64 = conn
            .query_row("SELECT COUNT(*) FROM frames WHERE filter = 'Z' AND exptime = 999.0", [], |r| r.get(0))
            .unwrap();
        assert_eq!(decoys, 3, "the decoy rows are untouched");
    }

    /// Spec §13 (R3), plan 6b-2 C11: an attested set (externally calibrated
    /// lights, no masters at all) runs end to end:
    /// - its plan asks for no master;
    /// - stage 1 prepares, converting the 16-bit lights into the unit domain
    ///   under `prepared/`, and reports in the stage-1 slot;
    /// - nothing is calibrated;
    /// - the run builds no master (review I11: read from the run's summary,
    ///   not from a catalog count that is zero either way).
    #[test]
    fn an_attested_set_stacks_without_masters() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
        let noise = [5.0f32; 4];
        let (fixture, light_ids, working, _output) =
            seed_star_group(&db_path, SET_NAME, &shifts, &noise);
        fixture
            .conn
            .execute("UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1", [fixture.set_id])
            .unwrap();
        let plan = crate::stacking::plan::build_plan(
            &fixture.conn,
            &crate::settings::SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(fixture.set_id),
            None,
        )
        .unwrap();
        assert!(plan.masters_to_build.is_empty(), "{:?}", plan.masters_to_build);
        let recorder = Arc::new(Recording::new());
        let started = start_stacking(
            ctx.clone(),
            recorder.clone(),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            None,
            None,
        )
        .expect("an attested set needs no masters to start");
        wait_for_run(&ctx, started.run_id);
        let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id).unwrap().unwrap();
        assert_eq!(run_row.status, "done", "{run_row:?}");

        let subject = StackSubject::FrameSet(fixture.set_id);
        let layout = crate::stacking::paths::subject_layout(&fixture.conn, working.path(), &subject).unwrap();
        let calibrated = crate::db::stacking::list_artifacts(&fixture.conn, &subject, Some("calibrated")).unwrap();
        assert_eq!(calibrated.len(), light_ids.len());
        for a in &calibrated {
            let p = PathBuf::from(a.path.as_deref().unwrap());
            assert!(p.starts_with(layout.prepared_root()), "{p:?}");
            let h = FitsHeader::from_path(&p).unwrap();
            assert_eq!(h.get_str("ATH_PREP").as_deref(), Some("converted"));
            assert_eq!(h.get_str("CALSTAT"), None);
        }
        assert!(!layout.calibrated_root().exists(), "nothing was calibrated");
        let summary: RunSummary =
            serde_json::from_str(run_row.summary_json.as_deref().expect("summary_json set")).unwrap();
        assert!(summary.masters_built.is_empty(), "{:?}", summary.masters_built);
        assert!(
            summary.stages.iter().any(|t| t.stage == Stage::Calibrate),
            "Prepare reports in the stage-1 slot: {:?}",
            summary.stages
        );
    }

    /// Plan 6b-2 C11: one stage-1 path per run. A run whose frames mix
    /// attested and calibrated origins is refused before any file is read.
    #[test]
    fn a_run_mixing_prepared_and_calibrated_frames_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)], &[5.0f32; 3]);
        let cfg = StackingConfig::default();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let (run_id, group_ids) =
            seed_run_and_groups(&fixture.conn, fixture.set_id, &plan_groups, working.path(), output.path());
        let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter),
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            layout,
            output.path().to_path_buf(),
            group_ids,
        );
        let first = rc.plan_groups[0].frames[0].clone();
        let mut origins: HashMap<i64, FrameOrigin> = (*rc.origins).clone();
        origins.insert(first.frame_id, FrameOrigin::Catalog { file_id: first.file_id, attested: true });
        rc.origins = Arc::new(origins);
        rc.memo = HashMemo::new(rc.origins.clone());
        let err = stage_calibrate(&mut rc).unwrap_err();
        assert!(
            matches!(err, RunError::Other(ref m) if m.contains("cannot mix prepared and calibrated frames")),
            "{err:?}"
        );
        assert!(!rc.layout.calibrated_root().exists() && !rc.layout.prepared_root().exists());
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 prepare_ an_attested_set_stacks a_run_mixing`

Expected: the build fails, because `stage_one_kind`, `stage_prepare` and `prepare_input` do not exist.

- [ ] **Step 3: The origin** (`origin.rs`). Add `use std::path::PathBuf;` and `use crate::stacking::groups::GroupFrame;`, then:
```rust
/// Whether stage 1 PREPARES this frame instead of calibrating it (spec
/// §5.1, §13): every project frame, and an attested set's lights.
pub(crate) fn needs_prepare(origin: &FrameOrigin) -> bool {
    match origin {
        FrameOrigin::Catalog { attested, .. } => *attested,
        FrameOrigin::Project { .. } => true,
    }
}

/// What Prepare reads for one frame, and the uuid its log line carries.
pub(crate) struct PrepareInput {
    pub(crate) path: PathBuf,
    pub(crate) frame_uuid: Option<String>,
}

/// The file Prepare reads: an attested light's catalog file (the frame
/// source's `GroupFrame.path`), or a project frame's landed replica.
pub(crate) fn prepare_input(origin: &FrameOrigin, frame: &GroupFrame) -> PrepareInput {
    match origin {
        FrameOrigin::Catalog { .. } => PrepareInput {
            path: PathBuf::from(&frame.path),
            frame_uuid: None,
        },
        FrameOrigin::Project {
            landed_path,
            frame_uuid,
            ..
        } => PrepareInput {
            path: PathBuf::from(landed_path),
            frame_uuid: Some(frame_uuid.clone()),
        },
    }
}
```

- [ ] **Step 4: The stage** (`run.rs`).
  1. `resolve_calibrate_job`'s origin match gains an attested arm, ahead of the catalog one:
```rust
            Ok(FrameOrigin::Catalog { attested: true, .. }) => {
                Err("an attested frame is prepared, never calibrated".to_string())
            }
            Ok(FrameOrigin::Catalog { .. }) => resolve_generation_cached(
```
  2. Add, above `stage_calibrate`:
```rust
/// Which stage 1 a run takes (plan 6b-2 C11, C12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageOneKind {
    Calibrate,
    Prepare,
}

/// Stage 1's kind for this run: Prepare for a project and for an attested
/// frame set, Calibrate otherwise. One stage-1 path per run: frames that
/// disagree refuse the run before any file is read. A frame with no origin
/// counts for neither; the stage that reads it excludes it, exactly as
/// before this dispatch existed (6b-1: a per-frame exclusion in Calibrate).
fn stage_one_kind(rc: &RunContext) -> Result<StageOneKind, RunError> {
    let (mut prepare, mut calibrate) = (0usize, 0usize);
    for f in rc.plan_groups.iter().flat_map(|g| g.frames.iter()) {
        match rc.origins.get(&f.frame_id) {
            Some(origin) if crate::stacking::origin::needs_prepare(origin) => prepare += 1,
            Some(_) => calibrate += 1,
            None => {}
        }
    }
    if prepare > 0 && calibrate > 0 {
        return Err(RunError::Other(format!(
            "a run cannot mix prepared and calibrated frames ({prepare} prepared, {calibrate} calibrated)"
        )));
    }
    Ok(if prepare > 0 || rc.subject.project_id().is_some() {
        StageOneKind::Prepare
    } else {
        StageOneKind::Calibrate
    })
}
```
  3. `stage_calibrate`'s first statements become:
```rust
    // Plan 6b-2 C11, C12: a project, or an attested set, prepares instead.
    if stage_one_kind(rc)? == StageOneKind::Prepare {
        return stage_prepare(rc);
    }
    // Calibrate only ever runs for a frame set; the id feeds the log lines.
    let Some(set_id) = rc.subject.frame_set_id() else {
        return Err(RunError::Other(PROJECT_STACKING_UNAVAILABLE.into()));
    };
```
  Add one sentence to `stage_calibrate`'s doc: "A project or an attested set takes [`stage_prepare`] instead (plan 6b-2 C11, C12)."
  4. Add after `commit_calibrated`:
```rust
/// The plane count every member of `group` has after stage 1 (plan 6b-2
/// C13, review I12): one for mono; three for OSC with the debayer on; with
/// it off, three for a project's `osc` group (its members are three-plane
/// files) and one otherwise (a frame set's OSC group or a project's `cfa`
/// group: mosaics kept as mosaics).
fn expected_stage_one_planes(cfg: &StackingConfig, subject: &StackSubject, group: &IntegrationGroup) -> usize {
    match group.color_mode {
        ColorMode::Mono => 1,
        ColorMode::Osc if cfg.calibration.debayer_osc => 3,
        ColorMode::Osc if subject.project_id().is_some() && group.key.starts_with("osc__") => 3,
        ColorMode::Osc => 1,
    }
}

/// One frame's Prepare RESOLVE outcome (plan 6b-2 C12), the twin of
/// [`CalibrateResolved`].
enum PrepareResolved {
    Reused { bytes: u64 },
    Excluded { reason: String },
    Job(Box<PrepareRunJob>),
}

/// One frame's resolved Prepare work: the pure job plus what the commit
/// writes back.
struct PrepareRunJob {
    frame_id: i64,
    group_key: String,
    hash: String,
    /// Bayer drizzle wants this frame's pre-debayer mosaic kept.
    keep_mosaic: bool,
    frame_uuid: Option<String>,
    job: crate::stacking::prepare::PrepareJob,
}

/// Prepare's per-frame RESOLVE phase, on the run thread:
/// - the identity check (C9's Prepare boundary);
/// - the stage-1 hash through `rc.memo`;
/// - the header-only classification;
/// - the plane-count check against the group's colour (review I12);
/// - the freshness lookup.
/// A frame whose file no reader decodes, or whose planes contradict its
/// group, is excluded here.
fn resolve_prepare_job(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    frame: &GroupFrame,
) -> Result<PrepareResolved, RunError> {
    use crate::stacking::prepare::{prepare_action, probe_file, PrepareAction, PrepareJob};
    if let Some(reason) = changed_during_run(rc, frame.frame_id)? {
        return Ok(PrepareResolved::Excluded { reason });
    }
    let hash = {
        let conn = db(&rc.ctx)?.conn();
        match rc.memo.calibration_hash_checked(&conn, cfg, frame) {
            Ok(hash) => hash,
            Err(e) => {
                return Ok(PrepareResolved::Excluded {
                    reason: format!("preparation failed: {e}"),
                })
            }
        }
    };
    let input = crate::stacking::origin::prepare_input(origin_of(&rc.origins, frame.frame_id)?, frame);
    let shape = probe_file(&input.path);
    let action = prepare_action(&shape, cfg.calibration.debayer_osc);
    if let PrepareAction::Unsupported(why) = &action {
        return Ok(PrepareResolved::Excluded {
            reason: format!("unsupported file: {why}"),
        });
    }
    // Review I12 (C13): the group's colour comes from the manifest (or the
    // catalog), Prepare's debayer from the file's own header. A file whose
    // post-Prepare plane count contradicts its group never reaches Measure.
    let planes = match &action {
        PrepareAction::Write { debayer: Some(_), .. } => 3,
        _ => shape.planes,
    };
    if planes != expected_stage_one_planes(cfg, &rc.subject, group) {
        return Ok(PrepareResolved::Excluded {
            reason: format!("plane count {planes} disagrees with the group's colour mode"),
        });
    }
    let keep_mosaic = wants_cfa_mosaic(cfg, group)
        && matches!(action, PrepareAction::Write { debayer: Some(_), .. });

    if rc.rerun_from != Some(Stage::Calibrate) {
        let conn = db(&rc.ctx)?.conn();
        let existing = crate::db::stacking::find_artifact(&conn, &rc.subject, &group.key, "calibrated", Some(frame.frame_id))?;
        let mosaic_fresh = !keep_mosaic
            || crate::db::stacking::find_artifact(&conn, &rc.subject, &group.key, "calibrated_mosaic", Some(frame.frame_id))?
                .is_some_and(|row| is_fresh(&row, &hash));
        if let Some(row) = existing {
            if is_fresh(&row, &hash) && mosaic_fresh {
                return Ok(PrepareResolved::Reused {
                    bytes: row.size.unwrap_or(0) as u64,
                });
            }
        }
    }

    let stem = calibrated_file_stem(group, frame);
    let dir = rc.layout.prepared_dir(&group.key);
    Ok(PrepareResolved::Job(Box::new(PrepareRunJob {
        frame_id: frame.frame_id,
        group_key: group.key.clone(),
        hash,
        keep_mosaic,
        frame_uuid: input.frame_uuid,
        job: PrepareJob {
            src: input.path,
            shape,
            action,
            converted_out: dir.join(format!("p_{stem}.fits")),
            debayered_out: dir.join(format!("p_{stem}_d.fits")),
            // Regenerable intermediates, like Calibrate's (perf tier 1 Task 6).
            durability: crate::fits_writer::Durability::Volatile,
        },
    })))
}

/// Write one prepared frame's artifact rows (`calibrated`, plus
/// `calibrated_mosaic` when kept) in one transaction; returns the bytes the
/// frame's `calibrated` file holds.
fn commit_prepared(
    rc: &mut RunContext,
    job: &PrepareRunJob,
    files: crate::stacking::prepare::PreparedFiles,
) -> Result<u64, RunError> {
    let (size, modified_at) =
        file_identity(&files.calibrated).map_err(|e| RunError::Other(format!("{e:#}")))?;
    let mosaic = match (&files.mosaic, job.keep_mosaic) {
        (Some(path), true) => Some((
            path.clone(),
            file_identity(path).map_err(|e| RunError::Other(format!("{e:#}")))?,
        )),
        _ => None,
    };
    tracing::debug!(
        run_id = rc.run_id,
        frame_id = job.frame_id,
        frame_uuid = job.frame_uuid.as_deref(),
        group_key = %job.group_key,
        prepared = files.prepared,
        "frame prepared"
    );
    // A deferred transaction on purpose, as `commit_calibrated` does: these
    // are write-only upserts, so the `BEGIN IMMEDIATE` rule for
    // read-then-write does not apply (review M20).
    let mut conn = db(&rc.ctx)?.conn();
    let tx = conn
        .transaction()
        .map_err(|e| RunError::Other(format!("opening the artifact transaction: {e}")))?;
    upsert_artifact(
        &tx,
        &NewArtifact {
            subject: &rc.subject,
            frame_id: Some(job.frame_id),
            group_key: &job.group_key,
            kind: "calibrated",
            path: files.calibrated.to_str(),
            config_hash: &job.hash,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: None,
        },
    )?;
    if let Some((path, (mosaic_size, mosaic_modified))) = &mosaic {
        upsert_artifact(
            &tx,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: Some(job.frame_id),
                group_key: &job.group_key,
                kind: "calibrated_mosaic",
                path: path.to_str(),
                config_hash: &job.hash,
                size: Some(*mosaic_size),
                modified_at: Some(mosaic_modified),
                payload_json: None,
            },
        )?;
    }
    tx.commit()
        .map_err(|e| RunError::Other(format!("committing the artifact rows: {e}")))?;
    Ok(size as u64)
}

/// Stage 1 for a project or an attested set (spec §5.1, §13; plan 6b-2 C12).
/// Calibrate's shape:
/// - per group, a resolve pass on the run thread ([`resolve_prepare_job`]);
/// - a fan-out of the pixel work across the image pool ([`execute_prepare`]:
///   nothing for a file used in place, a conversion, or a debayer);
/// - commits in frame order ([`commit_prepared`]).
/// A per-frame failure excludes that frame; a cancel unwinds as
/// [`RunError::Cancelled`]. Progress and timing report under
/// `Stage::Calibrate`, the stage-1 slot.
///
/// [`execute_prepare`]: crate::stacking::prepare::execute_prepare
fn stage_prepare(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let excluded_set: HashSet<i64> = rc.excluded.iter().copied().collect();
    let groups = rc.plan_groups.clone();
    let cfg = rc.config.clone();
    let total: usize = groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .filter(|f| !excluded_set.contains(&f.frame_id))
        .count();
    let bytes_total = calibrate_bytes_total(&groups, &excluded_set);

    tracing::info!(
        run_id = rc.run_id,
        set_id = rc.subject.frame_set_id(),
        project_id = rc.subject.project_id(),
        count = total,
        groups = groups.len(),
        config_hash = %rc.hash,
        "stacking run started"
    );
    if cfg.drizzle.enabled
        && cfg.drizzle.bayer
        && !cfg.calibration.debayer_osc
        && groups.iter().any(|g| g.color_mode == ColorMode::Osc)
    {
        tracing::warn!(
            run_id = rc.run_id,
            set_id = rc.subject.frame_set_id(),
            project_id = rc.subject.project_id(),
            "bayer drizzle ignored: the osc debayer is off for this run"
        );
        rc.warnings.push(
            "Bayer drizzle is ignored: the OSC debayer is off for this run, so no calibrated CFA \
             mosaic is kept and drizzle deposits each frame's calibrated planes as they are."
                .to_string(),
        );
    }

    let mut current = 0usize;
    let mut bytes_done = 0u64;
    rc.progress(Stage::Calibrate, None, current, total, bytes_done, bytes_total, None, None);

    for group in &groups {
        rc.check_cancel()?;
        let group_max_w = group.frames.iter().map(|f| f.width.max(0) as u64).max().unwrap_or(0);
        let group_max_h = group.frames.iter().map(|f| f.height.max(0) as u64).max().unwrap_or(0);

        let mut jobs: Vec<PrepareRunJob> = Vec::new();
        for frame in &group.frames {
            if excluded_set.contains(&frame.frame_id) {
                continue;
            }
            rc.check_cancel()?;
            match resolve_prepare_job(rc, &cfg, group, frame)? {
                PrepareResolved::Reused { bytes } => {
                    bytes_done += bytes;
                    rc.cached_calibrated.insert(frame.frame_id, true);
                    current += 1;
                    rc.progress(Stage::Calibrate, Some(group.key.clone()), current, total, bytes_done, bytes_total, Some(frame.frame_id), None);
                }
                PrepareResolved::Excluded { reason } => {
                    tracing::warn!(run_id = rc.run_id, frame_id = frame.frame_id, error = %reason, "preparation failed; frame excluded");
                    rc.runtime_exclusions.push((frame.frame_id, reason));
                    current += 1;
                    rc.progress(Stage::Calibrate, Some(group.key.clone()), current, total, bytes_done, bytes_total, Some(frame.frame_id), None);
                }
                PrepareResolved::Job(job) => jobs.push(*job),
            }
        }
        if jobs.is_empty() {
            continue;
        }

        let working_set_bytes = calibrate_working_set_bytes(group_max_w, group_max_h);
        let admission_n = admission(working_set_bytes, rc.ctx.image_pool.current_num_threads());
        log_admission(rc, Stage::Calibrate, &group.key, working_set_bytes, 0, admission_n);
        let ticker = FanOutTicker::new(rc, Stage::Calibrate, Some(group.key.clone()), current, total, bytes_done, bytes_total);
        let ticker_ref = &ticker;
        let cancel_ref: &AtomicBool = &rc.cancel;
        let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
        let job_refs: Vec<&PrepareRunJob> = jobs.iter().collect();
        let results = fan_out(job_refs, admission_n, cancel_ref, move |job: &PrepareRunJob| {
            let out = crate::stacking::prepare::execute_prepare(&job.job, Some(pool_ref), cancel_ref)
                .map_err(|e| {
                    if matches!(e.downcast_ref::<IntegrationError>(), Some(IntegrationError::Cancelled)) {
                        CANCELLED_MARKER.to_string()
                    } else {
                        format!("preparation failed: {e:#}")
                    }
                });
            ticker_ref.tick(Some(job.frame_id));
            out
        });
        rc.check_cancel()?;
        current = ticker.done();

        for (pos, res) in results.into_iter().enumerate() {
            let job = &jobs[pos];
            match res {
                None => return Err(RunError::Cancelled),
                Some(Err(msg)) if msg == CANCELLED_MARKER => return Err(RunError::Cancelled),
                Some(Err(msg)) => {
                    tracing::warn!(run_id = rc.run_id, frame_id = job.frame_id, error = %msg, "preparation failed; frame excluded");
                    rc.runtime_exclusions.push((job.frame_id, msg));
                }
                Some(Ok(files)) => {
                    bytes_done += commit_prepared(rc, job, files)?;
                    rc.cached_calibrated.insert(job.frame_id, false);
                }
            }
        }
        rc.progress(Stage::Calibrate, Some(group.key.clone()), current, total, bytes_done, bytes_total, None, None);
    }

    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Calibrate,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });
    Ok(())
}
```
  `rustfmt` will re-wrap the long single-line calls; that is expected. `stage_one_kind` is a funnel (C8) and names no variant. `resolve_prepare_job`, `commit_prepared` and `stage_prepare` name none, so the grep test of Task 6a stays green.

- [ ] **Step 5: Run the tests and watch them pass.** First the Step 2 command. Then, unchanged:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 calibrate_fan_out_matches_the_sequential_stage_byte_for_byte calibrate_stage_reuses_fresh_artifacts calibrate_outputs_disambiguate turning_bayer_on_regenerates subject_and_origin_variants_stay_in_their_funnels a_project_start_is_refused_until_6b2
```
  Expected: all pass. A frame set without attestation still takes Calibrate, and a project start is still refused.

- [ ] **Step 6: Run the gate.** TS must not move: `git diff --exit-code src/types`.

- [ ] **Step 7: Commit** with the message `feat(stacking): the Prepare stage for projects and attested sets`, plus the trailers.

---

### Task 10a: The plate-solve core — the extracted gate, the solver config hash, one catalog opener for both hosts (C14)

**Files:**
- Modify `crates/athenaeum-core/src/plate_solve/service.rs`:
  - add `acceptance_refusal` and `record_from_result`;
  - `store_result` calls both (`~387-470`);
  - tests.
- Modify `crates/athenaeum-core/src/plate_solve/config.rs`: add `SOLVE_RECIPE_VERSION`, `solver_config_value` and `solver_config_hash`, plus a test.
- Modify `crates/athenaeum-core/src/plate_solve/layers.rs`: add `CatalogOpenError`, `catalog_root`, `open_catalog_layers` and `catalog_identity`, plus a test.
- Modify `crates/athenaeum-core/src/plate_solve/mod.rs`: the re-exports.
- Modify `crates/athenaeum-tauri/src/commands/plate_solve.rs` (`resolve_layer_caches`, `~17-47`) and `crates/athenaeum-web/src/routes/plate_solve.rs` (`resolve_layer_caches`, `~38-78`): call the core opener.

**Interfaces:**
```rust
// plate_solve/service.rs
pub(crate) fn acceptance_refusal(result: &SolveResult, expected_scale_arcsec: Option<f64>, config: &PlateSolveConfig) -> Option<String>;
pub(crate) fn record_from_result(frame_id: i64, result: &SolveResult) -> PlateSolveRecord;
// plate_solve/config.rs
pub const SOLVE_RECIPE_VERSION: u32 = 1;
pub fn solver_config_value(cfg: &PlateSolveConfig) -> serde_json::Value;
pub fn solver_config_hash(cfg: &PlateSolveConfig) -> String;
// plate_solve/layers.rs (re-exported from plate_solve)
#[derive(Debug)] pub enum CatalogOpenError { NotInstalled { root: PathBuf }, Open { dir: PathBuf, error: String } }
pub fn catalog_root(app_data_dir: &Path) -> PathBuf;
pub fn open_catalog_layers(app_data_dir: &Path) -> Result<Vec<Arc<solvemyastro::StarCache>>, CatalogOpenError>;
pub fn catalog_identity(app_data_dir: &Path) -> Result<String, CatalogOpenError>;
```
Consumed by: Task 10b (`acceptance_refusal`, `record_from_result`, `solver_config_hash`, `open_catalog_layers`, `catalog_identity` in `CatalogSolver`), Task 10c (`open_catalog_layers` in the real-data probe). No stacking code changes here; no command is added or changed.

- [ ] **Step 1: Write the failing tests**

`plate_solve/service.rs` tests:
```rust
    /// Plan 6b-2 C14: the extracted gate is `store_result`'s own. It refuses
    /// a solve 16× off the expected scale, passes the same confidence at that
    /// scale, has nothing to compare without an expected scale, and refuses
    /// nothing with the gate off.
    #[test]
    fn acceptance_refusal_is_store_results_gate() {
        let cfg = PlateSolveConfig::default();
        let wrong = mk_result(120, 800, 0.15, 29.6, 1.0);
        assert_eq!(
            acceptance_refusal(&wrong, Some(1.873), &cfg).as_deref(),
            Some("Rejected: low confidence (120 inliers, ratio 0.15, 29.60\"/px)")
        );
        assert_eq!(acceptance_refusal(&mk_result(120, 800, 0.15, 1.9, 1.0), Some(1.873), &cfg), None);
        assert_eq!(acceptance_refusal(&wrong, None, &cfg), None);
        let off = PlateSolveConfig { blind_gate_enabled: false, ..PlateSolveConfig::default() };
        assert_eq!(acceptance_refusal(&wrong, Some(1.873), &off), None);
    }

    /// Plan 6b-2 C14: the row a solve becomes, the same for `store_result`
    /// and for a stacking run's cached solve.
    #[test]
    fn record_from_result_is_the_stored_row() {
        let r = record_from_result(7, &mk_result(120, 800, 0.15, 1.9, 1.0));
        assert_eq!((r.id, r.frame_id), (None, 7));
        assert_eq!((r.crval1, r.crval2), (123.45, 67.89));
        assert_eq!((r.cd1_1, r.cd1_2, r.cd2_1, r.cd2_2), (1e-4, 0.0, 0.0, 1e-4));
        assert_eq!((r.matched_stars, r.total_detected), (120, 600));
        assert_eq!((r.expected_catalog_stars_in_fov, r.inlier_ratio), (Some(800), Some(0.15)));
        assert_eq!((r.catalog_used.as_str(), r.algorithm_used.as_str()), ("smac_gaia", "blind_index"));
        assert_eq!(r.pixel_scale_arcsec, 1.9);
    }
```
`plate_solve/config.rs` tests:
```rust
    /// Plan 6b-2 C14: a cached solve depends on the recipe and on every field
    /// that shapes a solve or its acceptance, and on nothing else.
    #[test]
    fn the_solver_config_hash_covers_what_shapes_a_solve() {
        let cfg = PlateSolveConfig::default();
        assert_eq!(
            serde_json::to_string(&solver_config_value(&cfg)).unwrap(),
            r#"{"baseVerificationToleranceArcsec":8.0,"blindGateEnabled":true,"blindInlierFloor":6,"blindMinInlierRatio":0.04,"blindRmsMaxPxMult":2.5,"blindScaleHeaderTol":8.0,"blindScaleSanityMax":60.0,"blindScaleSanityMin":0.05,"recipe":1,"sipOrder":3}"#
        );
        let h = solver_config_hash(&cfg);
        assert_eq!(h.len(), 16);
        let mut same = cfg.clone();
        same.batch_concurrency = 4;
        same.autofind_tolerance_deg = 2.0;
        same.input_gate_enabled = false;
        same.camera_defaults.insert("cam".into(), 3.76);
        assert_eq!(solver_config_hash(&same), h);
        let mut sip = cfg.clone();
        sip.sip_order = 2;
        assert_ne!(solver_config_hash(&sip), h);
        let mut tol = cfg.clone();
        tol.blind_scale_header_tol = 4.0;
        assert_ne!(solver_config_hash(&tol), h);
    }
```
`plate_solve/layers.rs` tests:
```rust
    /// Plan 6b-2 C14: the catalog's identity names every tier with its
    /// `stars.smac` size and mtime, so replacing a tier moves it. With no
    /// catalog there is no identity and nothing to open.
    #[test]
    fn the_catalog_identity_follows_every_tier_file() {
        let app = tempfile::tempdir().unwrap();
        let root = catalog_root(app.path());
        assert_eq!(root, app.path().join("catalogs").join("smac_gaia"));
        assert!(matches!(catalog_identity(app.path()), Err(CatalogOpenError::NotInstalled { root: r }) if r == root));
        assert!(matches!(open_catalog_layers(app.path()), Err(CatalogOpenError::NotInstalled { .. })));
        for (tier, bytes) in [("tier_500", &b"x"[..]), ("tier_2000", &b"yy"[..])] {
            std::fs::create_dir_all(root.join(tier)).unwrap();
            std::fs::write(root.join(tier).join("stars.smac"), bytes).unwrap();
        }
        let id = catalog_identity(app.path()).unwrap();
        let parts: Vec<&str> = id.split(',').collect();
        assert_eq!(parts.len(), 2, "{id}");
        assert!(parts[0].starts_with("tier_500:1:"), "{id}");
        assert!(parts[1].starts_with("tier_2000:2:"), "{id}");
        std::fs::write(root.join("tier_500").join("stars.smac"), b"xyz").unwrap();
        let moved = catalog_identity(app.path()).unwrap();
        assert_ne!(moved, id, "a replaced tier moves the identity");
        assert!(moved.starts_with("tier_500:3:"), "{moved}");
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 acceptance_refusal record_from_result the_solver_config_hash the_catalog_identity`

Expected: the build fails. None of the new items exist.

- [ ] **Step 3: The plate-solve core** (`plate_solve/`).

`service.rs`. Add, after `store_result`:
```rust
/// The persist gate as a pure check (plan 6b-2 C14): `Some(user-facing
/// reason)` when `config` refuses `result`, given the scale the frame's own
/// optics, or its manifest, imply. [`store_result`] and a stacking run's
/// local solve share it.
pub(crate) fn acceptance_refusal(
    result: &SolveResult,
    expected_scale_arcsec: Option<f64>,
    config: &PlateSolveConfig,
) -> Option<String> {
    if !config.blind_gate_enabled {
        return None;
    }
    let m = BlindGateMetrics {
        inliers: result.matched_stars,
        expected_in_fov: result.expected_catalog_stars_in_fov,
        rms_px: result.rms_residual_px,
        adaptive_tol_px: adaptive_tol_px(
            result.pixel_scale_arcsec,
            config.base_verification_tolerance_arcsec,
        ),
        inlier_ratio: result.inlier_ratio,
        recovered_scale_arcsec: result.pixel_scale_arcsec,
        header_scale_arcsec: expected_scale_arcsec,
    };
    if blind_gate_ok(GateStage::ScaleCleared, &m, config) {
        None
    } else {
        Some(format!(
            "Rejected: low confidence ({} inliers, ratio {:.2}, {:.2}\"/px)",
            result.matched_stars, result.inlier_ratio, result.pixel_scale_arcsec
        ))
    }
}

/// The `plate_solves` row a solve becomes (pure; `solved_at` is now).
pub(crate) fn record_from_result(frame_id: i64, result: &SolveResult) -> PlateSolveRecord {
    PlateSolveRecord {
        id: None,
        frame_id,
        crpix1: result.wcs.crpix.0,
        crpix2: result.wcs.crpix.1,
        crval1: result.wcs.crval.0,
        crval2: result.wcs.crval.1,
        cd1_1: result.wcs.cd[0][0],
        cd1_2: result.wcs.cd[0][1],
        cd2_1: result.wcs.cd[1][0],
        cd2_2: result.wcs.cd[1][1],
        // SIP coefficients — populated from solvemyastro when fit_sip=true.
        // The DB columns already exist (schema unchanged); legacy solves wrote
        // NULL, now we write real values.
        sip_order: result.sip_order.map(|o| o as i32),
        sip_a_coeffs: result.sip_a_coeffs.clone(),
        sip_b_coeffs: result.sip_b_coeffs.clone(),
        sip_ap_coeffs: result.sip_ap_coeffs.clone(),
        sip_bp_coeffs: result.sip_bp_coeffs.clone(),
        matched_stars: result.matched_stars as i32,
        total_detected: result.total_detected as i32,
        rms_residual_px: result.rms_residual_px,
        rms_residual_arcsec: result.rms_residual_arcsec,
        pixel_scale_arcsec: result.pixel_scale_arcsec,
        field_rotation_deg: result.field_rotation_deg,
        solve_time_ms: result.solve_time_ms as i64,
        catalog_used: result.catalog_used.clone(),
        algorithm_used: result.algorithm_used.clone(),
        solved_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        expected_catalog_stars_in_fov: Some(result.expected_catalog_stars_in_fov as i32),
        inlier_ratio: Some(result.inlier_ratio),
    }
}
```
This is today's `PlateSolveRecord { … }` literal from `store_result` (`~439-468`), moved verbatim with `frame_id` the parameter; delete it from `store_result`. In `store_result`, the gate block becomes:
```rust
    if config.blind_gate_enabled {
        // The frame's own optics, when it declares them (2026-09-05: on
        // wind-shaken frames the solver walks its autoFOV ladder and returns a
        // confident-looking solve at 16–193× the header's scale). Read only
        // while the gate is on, as before.
        if let Some(reason) =
            acceptance_refusal(result, header_pixel_scale_arcsec(conn, frame_id), config)
        {
            tracing::warn!(
                frame_id,
                stage = "store",
                outcome = "rejected_low_confidence",
                inliers = result.matched_stars,
                inlier_ratio = result.inlier_ratio,
                scale_arcsec_px = result.pixel_scale_arcsec,
                "refusing to persist low-confidence solve, WCS/focal length not written back"
            );
            return Ok(StoreOutcome::RejectedLowConfidence { reason });
        }
    }

    let record = record_from_result(frame_id, result);
```
Keep `store_result`'s doc and its "Defense-in-depth" comment block above the gate. Every existing service test passes unchanged; this is the behaviour pin.

`config.rs`. Append after `save_config`:
```rust
/// Bumped when the solver recipe hard-coded in
/// [`crate::plate_solve::service::solve_frame_with_hints`] changes (quad
/// tolerance, catalog magnitude limit, SIP fitting), and whenever a solver
/// upgrade should re-solve frames a stacking run cached as SOLVED: bumping it
/// moves every cached solve's hash. A cached failure or refusal needs no
/// bump: one written by another app version is tried once more anyway
/// (review I7, `stacking::solve::cached_outcome_holds`).
pub const SOLVE_RECIPE_VERSION: u32 = 1;

/// What a cached solve depends on (plan 6b-2 C14): the recipe, the SIP
/// order, and every acceptance-gate field. Not the batch concurrency, the
/// autofind tolerance, the input gate (a run applies none) or the camera
/// defaults (they only back-fill a catalog row's focal length).
pub fn solver_config_value(cfg: &PlateSolveConfig) -> serde_json::Value {
    serde_json::json!({
        "recipe": SOLVE_RECIPE_VERSION,
        "sipOrder": cfg.sip_order,
        "baseVerificationToleranceArcsec": cfg.base_verification_tolerance_arcsec,
        "blindGateEnabled": cfg.blind_gate_enabled,
        "blindRmsMaxPxMult": cfg.blind_rms_max_px_mult,
        "blindMinInlierRatio": cfg.blind_min_inlier_ratio,
        "blindInlierFloor": cfg.blind_inlier_floor,
        "blindScaleSanityMin": cfg.blind_scale_sanity_min,
        "blindScaleSanityMax": cfg.blind_scale_sanity_max,
        "blindScaleHeaderTol": cfg.blind_scale_header_tol,
    })
}

/// [`solver_config_value`]'s canonical JSON (sorted keys) as XXH3-64, 16 hex
/// digits.
pub fn solver_config_hash(cfg: &PlateSolveConfig) -> String {
    let json = serde_json::to_string(&solver_config_value(cfg))
        .expect("a solver config value always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}
```
`layers.rs`. Add `use std::sync::Arc;` and `use std::time::UNIX_EPOCH;`, then append after `discover_layers`:
```rust
/// Why the plate-solve star catalog could not be opened.
#[derive(Debug)]
pub enum CatalogOpenError {
    /// No tier and no legacy `stars.smac` under `root`.
    NotInstalled { root: PathBuf },
    /// A tier exists, but it could not be opened or read.
    Open { dir: PathBuf, error: String },
}

impl std::fmt::Display for CatalogOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogOpenError::NotInstalled { root } => {
                write!(f, "the plate-solve star catalog is not installed under {}", root.display())
            }
            CatalogOpenError::Open { dir, error } => {
                write!(f, "Failed to open star cache at {}: {error}", dir.display())
            }
        }
    }
}

impl std::error::Error for CatalogOpenError {}

/// `<app-data>/catalogs/smac_gaia`, where the hosts install the catalog.
pub fn catalog_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("catalogs").join("smac_gaia")
}

/// Open the additive tier stack under [`catalog_root`] (base → deepest), or
/// a legacy single cache. This is the one opener: both hosts and a stacking
/// run call it. Opens fresh on each call; the mmap is cheap.
pub fn open_catalog_layers(
    app_data_dir: &Path,
) -> Result<Vec<Arc<solvemyastro::StarCache>>, CatalogOpenError> {
    let root = catalog_root(app_data_dir);
    let dirs = discover_layers(&root);
    if dirs.is_empty() {
        return Err(CatalogOpenError::NotInstalled { root });
    }
    let mut layers = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let cache = solvemyastro::StarCache::open(dir).map_err(|e| CatalogOpenError::Open {
            dir: dir.clone(),
            error: e.to_string(),
        })?;
        layers.push(Arc::new(cache));
    }
    tracing::debug!(count = layers.len(), path = %root.display(), "opened catalog layers");
    Ok(layers)
}

/// The installed catalog's identity, for a cached solve's hash: every tier's
/// directory name (`legacy` for a bare root cache), with its `stars.smac`
/// size and mtime in seconds, in tier order. Stat only, no read. It walks the
/// tiers apart from [`open_catalog_layers`]: a catalog swapped between the
/// two walks could pair the new identity with the old layers for one run,
/// which is negligible (review M15).
pub fn catalog_identity(app_data_dir: &Path) -> Result<String, CatalogOpenError> {
    let root = catalog_root(app_data_dir);
    let dirs = discover_layers(&root);
    if dirs.is_empty() {
        return Err(CatalogOpenError::NotInstalled { root });
    }
    let mut parts = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let meta = std::fs::metadata(dir.join("stars.smac")).map_err(|e| CatalogOpenError::Open {
            dir: dir.clone(),
            error: e.to_string(),
        })?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let name = if *dir == root {
            "legacy".to_string()
        } else {
            dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        };
        parts.push(format!("{name}:{}:{mtime}", meta.len()));
    }
    Ok(parts.join(","))
}
```
`mod.rs`: `pub use layers::{catalog_identity, catalog_root, discover_layers, open_catalog_layers, CatalogOpenError};` replaces `pub use layers::discover_layers;`.

- [ ] **Step 4: The hosts call the core opener.** Tauri (`commands/plate_solve.rs`): add `use athenaeum_core::plate_solve::CatalogOpenError;`, then replace `resolve_layer_caches`'s body after `let parent = …;`. The user-facing strings stay byte-identical, and the `debug!` moves into core with the same message and fields.
```rust
    athenaeum_core::plate_solve::open_catalog_layers(parent).map_err(|e| match e {
        CatalogOpenError::NotInstalled { root } => format!(
            "solvemyastro star catalog not found under {}. \
             Please download the Gaia DR3 prebuilt catalog from Settings → Plate Solving.",
            root.display()
        ),
        CatalogOpenError::Open { dir, error } => {
            format!("Failed to open star cache at {}: {error}", dir.display())
        }
    })
```
Web (`routes/plate_solve.rs`): the same `use`, and after `let parent = …;`:
```rust
    athenaeum_core::plate_solve::open_catalog_layers(parent).map_err(|e| match e {
        CatalogOpenError::NotInstalled { root } => (
            StatusCode::PRECONDITION_FAILED,
            format!(
                "solvemyastro star catalog not found under {}. \
                 Please download the Gaia DR3 prebuilt catalog.",
                root.display()
            ),
        ),
        CatalogOpenError::Open { dir, error } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to open star cache at {}: {error}", dir.display()),
        ),
    })
```
Drop the `catalog_root` bindings, loops and `debug!` the core function now does. Keep each function's doc comment. Remove any import that becomes unused (the compiler names it). No command is added or changed, so the strict count stays at 299.

- [ ] **Step 5: Run the tests and watch them pass.** First the Step 2 command. Then:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 plate_solve::
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-tauri
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 plate_solve
```
  Expected: all pass, including every existing `store_result` / `blind_gate` test, unchanged.

- [ ] **Step 6: Run the gate.** TS must not move: `git diff --exit-code src/types`.

- [ ] **Step 7: Commit** with the message `feat(plate_solve): the acceptance gate as a pure check, the solver config hash, one catalog opener for both hosts`, plus the trailers.

---

### Task 10b: The solver seam and the Solve sub-step (C14; spec §5.2, §15.2)

**Files:**
- Create `crates/athenaeum-core/src/stacking/solve.rs`, and declare `pub mod solve;` in `stacking/mod.rs`.
- Modify `crates/athenaeum-core/src/stacking/origin.rs`:
  - add `SolvePlan`, `solve_plan` and `local_solve_outcome`;
  - `stored_solve`'s project arm;
  - a test.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `RunContext` gains `solver`, with its two constructors (`start_stacking ~944`, `test_context ~9916`);
  - add `solve_project_frames`;
  - change `stage_measure`'s head;
  - tests.
- Modify `crates/athenaeum-core/src/stacking/plan.rs`: `project_inputs`'s solved-locally warning (Task 7), plus a test.

**Interfaces:**
```rust
// stacking/solve.rs (pub mod)
pub trait FrameSolver: Send + Sync {
    fn identity(&self) -> Result<String, String>;
    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome;
}
#[derive(Debug, Clone)] pub struct SolveRequest { pub handle: i64, pub path: PathBuf, pub hints: SolveHints, pub expected_scale_arcsec: Option<f64>,
    pub date_obs: Option<DateTime<Utc>>, pub focal_len: Option<f64>, pub instrume: Option<String>, pub naxis: Option<(i64, i64)> }
impl SolveRequest { pub fn for_frame(handle: i64, path: &Path, meta: &ProjectFrameMeta, target: Option<(f64, f64)>) -> Self; }
#[derive(Debug, Clone, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub enum SolveOutcome { Solved(PlateSolveRecord), Failed(String), Refused(String) }
impl SolveOutcome { pub fn label(&self) -> &'static str; }
#[derive(Debug, Clone, Serialize, Deserialize)] pub struct SolveArtifact { pub blake3: String, #[serde(default)] pub app_version: String, pub outcome: SolveOutcome }
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub fn solve_hash(blake3: &str, solver_identity: &str) -> String;
pub fn solve_with(request: &SolveRequest, caches: &solvemyastro::Caches<'_>, config: &PlateSolveConfig, cancel: &AtomicBool) -> SolveOutcome;
pub fn adu_scaled_copy(src: &Path, dest: &Path) -> anyhow::Result<()>;
pub(crate) fn cached_outcome_holds(row: &StackingArtifactRow, hash: &str) -> bool;
pub(crate) fn stored_outcome(conn: &Connection, project_id: &str, frame_id: i64, blake3: &str) -> anyhow::Result<Option<SolveOutcome>>;
pub(crate) fn write_outcome(conn: &Connection, project_id: &str, frame_id: i64, blake3: &str, hash: &str, outcome: SolveOutcome) -> anyhow::Result<()>;
pub(crate) fn write_artifact(conn: &Connection, project_id: &str, frame_id: i64, hash: &str, artifact: &SolveArtifact) -> anyhow::Result<()>;
pub struct CatalogSolver { /* private */ } impl CatalogSolver { pub fn new(ctx: Arc<ServiceContext>) -> Self; }
#[cfg(test)] pub(crate) struct FakeSolver { /* private */ }
// stacking/origin.rs
pub(crate) struct SolvePlan { pub(crate) project_id: String, pub(crate) frame_uuid: String, pub(crate) blake3: String, pub(crate) meta: ProjectFrameMeta }
pub(crate) fn solve_plan(origin: &FrameOrigin) -> Option<SolvePlan>;
pub(crate) fn local_solve_outcome(conn: &Connection, origin: &FrameOrigin, frame_id: i64) -> anyhow::Result<Option<SolveOutcome>>;
// stacking/run.rs
fn solve_project_frames(rc: &mut RunContext) -> Result<(), RunError>;
```
Consumed by: Task 10c (`stored_outcome` through `wcs_tier`, the helpers, the probe's `solve_with` and `adu_scaled_copy`), plan 6b-3 (`stored_solve`'s local WCS seeds registration onto the grid).

The solve artifact's hash is `solve_hash(blake3, identity)`, where `identity = "<solver_config_hash>:<catalog_identity>"`. It is not the Prepare hash, so toggling the debayer never re-solves. The artifact's payload carries the content's `blake3`, so `stored_solve` never serves a solve of an older version of the frame, even when the run could not refresh it. It also carries the app version that wrote it (review I7): a cached failure or refusal from another app version is a miss, and is tried once more, because a release may carry a solver that rescues the frame; a cached solve stays.

- [ ] **Step 1: Write the failing tests**

`stacking/solve.rs` tests (the module is created in Step 3; write its test module here):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Plan 6b-2 C14: the `solve` artifact's payload (the content, the app
    /// version that wrote it, review I7, and the outcome) and its hash over
    /// the frame's content and the solver's identity.
    #[test]
    fn the_solve_artifact_payload_and_hash_are_pinned() {
        let failed = SolveArtifact {
            blake3: "bb".into(),
            app_version: "1.2.3".into(),
            outcome: SolveOutcome::Failed("no match".into()),
        };
        assert_eq!(
            serde_json::to_string(&failed).unwrap(),
            r#"{"blake3":"bb","app_version":"1.2.3","outcome":{"failed":"no match"}}"#
        );
        let refused = SolveArtifact { outcome: SolveOutcome::Refused("r".into()), ..failed.clone() };
        assert_eq!(
            serde_json::to_string(&refused).unwrap(),
            r#"{"blake3":"bb","app_version":"1.2.3","outcome":{"refused":"r"}}"#
        );
        let back: SolveArtifact = serde_json::from_str(
            &serde_json::to_string(&SolveArtifact {
                outcome: SolveOutcome::Solved(FakeSolver::fixture_record()),
                ..failed.clone()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(back.outcome, SolveOutcome::Solved(ref r) if r.crval1 == 83.8 && r.crval2 == -5.4));
        let unversioned: SolveArtifact = serde_json::from_str(r#"{"blake3":"bb","outcome":{"failed":"x"}}"#).unwrap();
        assert_eq!(unversioned.app_version, "", "a payload without a version reads as another version");
        assert_eq!(APP_VERSION, env!("CARGO_PKG_VERSION"));
        let expected = stage_hash(&serde_json::json!({ "solver": "s:c" }), &["blake3:bb"], &[]);
        assert_eq!(solve_hash("bb", "s:c"), expected);
        assert_ne!(solve_hash("bb", "s:d"), expected);
        assert_ne!(solve_hash("cc", "s:c"), expected);
        assert_eq!(
            [
                SolveOutcome::Solved(FakeSolver::fixture_record()).label(),
                SolveOutcome::Failed(String::new()).label(),
                SolveOutcome::Refused(String::new()).label(),
            ],
            ["solved", "failed", "refused"]
        );
    }

    /// Spec §5.2: a project frame's request carries its handle (it names the
    /// solve's span, review M12), the project's target as the position hint,
    /// and the manifest's scale as both the scale hint and the gate's expected
    /// scale. The field of view is that scale times the frame's WIDTH, as the
    /// hosts compute it (review M11).
    #[test]
    fn a_solve_request_carries_the_manifest_scale_and_the_target() {
        let meta = ProjectFrameMeta {
            exptime_sec: 300.0,
            date_obs: Some("2026-01-10T21:00:00".into()),
            naxis1: Some(48),
            naxis2: Some(64),
            pixel_scale_arcsec: Some(1.5),
            bayerpat: None,
            focal_len: Some(400.0),
            aperture: None,
            instrume: Some("cam".into()),
            wcs: None,
        };
        let r = SolveRequest::for_frame(7, Path::new("/w/p.fits"), &meta, Some((83.8, -5.4)));
        assert_eq!(r.handle, 7);
        assert_eq!((r.hints.ra, r.hints.dec), (Some(83.8), Some(-5.4)));
        assert_eq!(r.hints.pixel_scale_arcsec, Some(1.5));
        assert_eq!(r.hints.fov_deg, Some(1.5 * 48.0 / 3600.0), "the width, not the larger side");
        assert_eq!(r.expected_scale_arcsec, Some(1.5));
        assert_eq!(r.date_obs.map(|d| d.to_rfc3339()).as_deref(), Some("2026-01-10T21:00:00+00:00"));
        assert_eq!((r.focal_len, r.naxis), (Some(400.0), Some((48, 64))));
        let bare = SolveRequest::for_frame(
            7,
            Path::new("/w/p.fits"),
            &ProjectFrameMeta { pixel_scale_arcsec: None, ..meta },
            None,
        );
        assert_eq!((bare.hints.ra, bare.hints.fov_deg, bare.expected_scale_arcsec), (None, None, None));
    }

    /// Review M17: without an installed catalog the production solver says
    /// so: `identity` fails, and `solve` reports the same reason without
    /// touching the file.
    #[test]
    fn the_catalog_solver_without_a_catalog_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let solver = CatalogSolver::new(Arc::new(ServiceContext::new_for_tests(tmp.path().join("catalog.db"))));
        let err = solver.identity().unwrap_err();
        assert!(err.contains("not installed"), "{err}");
        let request = SolveRequest::for_frame(1, Path::new("/nowhere/p.fits"), &ProjectFrameMeta::default(), None);
        assert!(matches!(solver.solve(&request, &AtomicBool::new(false)), SolveOutcome::Failed(ref e) if *e == err));
    }
}
```
The real-data probe is added in Task 10c.

`stacking/origin.rs` tests:
```rust
    /// Plan 6b-2 C14: a project frame's WCS. The manifest's comes first, then
    /// the run's own cached solve of THIS content (a solve of other content is
    /// never served). The test also covers which frames the Solve sub-step
    /// takes, and the outcome the plan reads.
    #[test]
    fn a_project_frames_wcs_is_the_manifests_then_the_runs_own_solve() {
        use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        test_fixtures::add_project_frame(&conn, dir.path(), "p1", &test_fixtures::project_frame_spec("a"));
        let mut with_wcs = test_fixtures::project_frame_spec("m");
        with_wcs.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 10.0, 20.0, 1.5));
        test_fixtures::add_project_frame(&conn, dir.path(), "p1", &with_wcs);
        let source = crate::stacking::project_source::project_frame_source(
            &conn,
            "p1",
            &crate::stacking::config::StackingConfig::default(),
        )
        .unwrap();
        let of = |uuid: &str| -> (i64, FrameOrigin, String) {
            source
                .origins
                .iter()
                .find_map(|(id, o)| match o {
                    FrameOrigin::Project { frame_uuid, blake3, .. } if frame_uuid == uuid => {
                        Some((*id, o.clone(), blake3.clone()))
                    }
                    _ => None,
                })
                .unwrap()
        };
        let (a_id, a, a_blake3) = of("a");
        let (m_id, m, _) = of("m");

        assert!(stored_solve(&conn, &a, a_id).unwrap().is_none());
        assert!(solve_plan(&a).is_some_and(|p| p.frame_uuid == "a" && p.blake3 == a_blake3));
        assert!(local_solve_outcome(&conn, &a, a_id).unwrap().is_none());

        write_outcome(&conn, "p1", a_id, &a_blake3, "h", SolveOutcome::Solved(FakeSolver::fixture_record())).unwrap();
        assert_eq!(stored_solve(&conn, &a, a_id).unwrap().unwrap().crval1, 83.8);
        assert!(matches!(local_solve_outcome(&conn, &a, a_id).unwrap(), Some(SolveOutcome::Solved(_))));

        write_outcome(&conn, "p1", a_id, "other-content", "h", SolveOutcome::Solved(FakeSolver::fixture_record())).unwrap();
        assert!(stored_solve(&conn, &a, a_id).unwrap().is_none(), "a solve of other content is not served");

        assert_eq!(stored_solve(&conn, &m, m_id).unwrap().unwrap().crval1, 10.0);
        assert!(solve_plan(&m).is_none());

        let catalog = FrameOrigin::Catalog { file_id: 1, attested: false };
        assert!(solve_plan(&catalog).is_none());
        assert!(local_solve_outcome(&conn, &catalog, 1).unwrap().is_none());
    }
```
`plan.rs` tests (Task 7's `seed_project` and `with_folders`):
```rust
    /// Plan 6b-2 C14: the plan reads the run's last cached solve outcome, so
    /// a frame that could not be solved is reported as such, not as "will
    /// be plate-solved".
    #[test]
    fn a_project_plan_reports_frames_that_could_not_be_solved() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                Some(with_folders(&working, &output)),
            )
            .unwrap()
        };
        let first = plan(&f);
        let o1 = first.project_frames.iter().find(|r| r.frame_uuid == "o1").unwrap().handle.unwrap();
        let blake3: String = f
            .conn
            .query_row(
                "SELECT blake3 FROM project_frames_local WHERE project_id = 'p1' AND frame_uuid = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        crate::stacking::solve::write_outcome(
            &f.conn,
            "p1",
            o1,
            &blake3,
            "h",
            crate::stacking::solve::SolveOutcome::Failed("no match".into()),
        )
        .unwrap();
        let second = plan(&f);
        assert!(
            second.warnings.iter().any(|w| w == "1 frame could not be plate-solved and cannot anchor the grid"),
            "{:?}",
            second.warnings
        );
        assert!(second.warnings.iter().all(|w| !w.contains("will be plate-solved locally")), "{:?}", second.warnings);
    }
```
`run.rs` tests. The helpers come first, then the tests:
```rust
    fn handle_of(rc: &RunContext, file_name: &str) -> i64 {
        rc.plan_groups
            .iter()
            .flat_map(|g| g.frames.iter())
            .find(|f| f.filename == file_name)
            .unwrap()
            .frame_id
    }

    /// R frames `r1..=r{n_r}` with no WCS and G frames `g1..=g{n_g}` with a
    /// manifest WCS, all landed under `landed`.
    fn seed_r_and_g(conn: &rusqlite::Connection, landed: &Path, project_id: &str, n_r: usize, n_g: usize) {
        for i in 1..=n_r {
            let uuid = format!("r{i}");
            let mut spec = test_fixtures::project_frame_spec(&uuid);
            spec.filter = "R";
            test_fixtures::add_project_frame(conn, landed, project_id, &spec);
        }
        for i in 1..=n_g {
            let uuid = format!("g{i}");
            let mut spec = test_fixtures::project_frame_spec(&uuid);
            spec.filter = "G";
            spec.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
            test_fixtures::add_project_frame(conn, landed, project_id, &spec);
        }
    }

    fn blake3_of(conn: &rusqlite::Connection, project_id: &str, uuid: &str) -> String {
        conn.query_row(
            "SELECT blake3 FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
            [project_id, uuid],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn use_solver(rc: &mut RunContext, fake: &Arc<crate::stacking::solve::FakeSolver>) {
        let solver: Arc<dyn crate::stacking::solve::FrameSolver> = fake.clone();
        rc.solver = solver;
    }

    /// Spec §5.2, §15.1 (Review Focus 2), plan 6b-2 C14: the Solve sub-step
    /// solves each project frame without a manifest WCS once, through the
    /// run's solver and on the file stage 1 handed on, and caches the
    /// outcome as a `solve` artifact. It never writes `plate_solves` or
    /// `frames`, not even a `frames` row whose id equals a handle. A second
    /// pass solves nothing, and `stored_solve` then serves the local WCS.
    #[test]
    fn the_solve_sub_step_never_writes_plate_solves_or_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let handles: Vec<i64> = rc.origins.keys().copied().collect();
        for (i, h) in handles.iter().enumerate() {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'decoy.fits', 7, '2020-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![h, format!("/decoy/{i}.fits")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, filter, exptime, naxis1, naxis2)
                 VALUES (?1, ?1, 'Light', 'Z', 999.0, 7, 7)",
                [h],
            )
            .unwrap();
            test_fixtures::seed_plate_solve_scale(&conn, *h, 9.9);
        }
        let snapshot = |c: &rusqlite::Connection| -> (i64, i64, String) {
            c.query_row(
                "SELECT (SELECT COUNT(*) FROM plate_solves), (SELECT COUNT(*) FROM frames),
                        (SELECT group_concat(filter || ':' || exptime || ':' || IFNULL(ra, ''), ',') FROM frames)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };
        let before = snapshot(&conn);
        let fake = Arc::new(crate::stacking::solve::FakeSolver::solving_all());
        use_solver(&mut rc, &fake);
        seed_in_place_calibrated(&conn, &mut rc);

        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 3);
        assert_eq!(snapshot(&conn), before, "plate_solves and frames are never written");
        let solves = crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve")).unwrap();
        assert_eq!(solves.len(), 3);
        assert!(solves.iter().all(|a| a.group_key.is_empty() && a.path.is_none()));

        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 3, "a cached outcome is never solved again");
        let h = handles[0];
        let record = crate::stacking::origin::stored_solve(&conn, &rc.origins[&h], h).unwrap().unwrap();
        assert_eq!((record.crval1, record.crval2, record.frame_id), (83.8, -5.4, 0));
    }

    /// Plan 6b-2 C14: a failed or refused solve is cached like a solved one,
    /// so it is not retried every run, and the frame stays in the run
    /// without a WCS.
    #[test]
    fn a_failed_or_refused_solve_is_cached_and_the_frame_stays() {
        use crate::stacking::solve::{FakeSolver, SolveOutcome};
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let fake = Arc::new(
            FakeSolver::solving_all()
                .with("c_r1.fits", SolveOutcome::Failed("no match".into()))
                .with("c_r2.fits", SolveOutcome::Refused("Rejected: low confidence (6 inliers, ratio 0.01, 1.50\"/px)".into())),
        );
        use_solver(&mut rc, &fake);
        seed_in_place_calibrated(&conn, &mut rc);
        solve_project_frames(&mut rc).unwrap();
        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 3, "every outcome is cached");
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        for (name, solved) in [("c_r1.fits", false), ("c_r2.fits", false), ("c_r3.fits", true)] {
            let h = handle_of(&rc, name);
            assert_eq!(
                crate::stacking::origin::stored_solve(&conn, &rc.origins[&h], h).unwrap().is_some(),
                solved,
                "{name}"
            );
        }
    }

    /// Review I7: a failure or refusal cached by ANOTHER app version is tried
    /// once more (a release may carry a solver that rescues the frame); a solve
    /// cached by another version is kept; this version's new outcomes then hold.
    #[test]
    fn a_failure_cached_by_another_app_version_is_solved_again() {
        use crate::stacking::solve::{solve_hash, write_artifact, FakeSolver, SolveArtifact, SolveOutcome};
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let fake = Arc::new(FakeSolver::solving_all());
        use_solver(&mut rc, &fake);
        seed_in_place_calibrated(&conn, &mut rc);
        for (uuid, outcome) in [
            ("r1", SolveOutcome::Failed("no match".into())),
            ("r2", SolveOutcome::Refused("refused".into())),
            ("r3", SolveOutcome::Solved(FakeSolver::fixture_record())),
        ] {
            let blake3 = blake3_of(&conn, "p1", uuid);
            write_artifact(
                &conn,
                "p1",
                handle_of(&rc, &format!("c_{uuid}.fits")),
                &solve_hash(&blake3, "fake:1"),
                &SolveArtifact { blake3, app_version: "0.0.0-older".into(), outcome },
            )
            .unwrap();
        }
        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 2, "the two failures are tried again; the solve is kept");
        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 2, "now cached by this version");
    }

    /// Plan 6b-2 C9 (the Solve boundary): a landed file that changed since
    /// the frame source read it is excluded "changed during the run" and is
    /// never handed to the solver.
    #[test]
    fn the_solve_sub_step_excludes_a_frame_whose_landed_file_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let fake = Arc::new(crate::stacking::solve::FakeSolver::solving_all());
        use_solver(&mut rc, &fake);
        seed_in_place_calibrated(&conn, &mut rc);
        let r2 = handle_of(&rc, "c_r2.fits");
        let r2_path = rc.plan_groups[0].frames.iter().find(|f| f.frame_id == r2).unwrap().path.clone();
        std::fs::write(&r2_path, vec![0u8; 5000]).unwrap();
        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 2);
        let reasons: HashMap<i64, String> = rc.runtime_exclusions.iter().cloned().collect();
        assert_eq!(reasons.get(&r2).map(String::as_str), Some(crate::stacking::origin::CHANGED_DURING_THE_RUN));
        assert!(crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(r2)).unwrap().is_none());
    }

    /// Plan 6b-2 C14: with no solver (no catalog installed) nothing is
    /// solved or cached, and the run says why.
    #[test]
    fn without_a_solver_nothing_is_solved_and_the_run_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        let fake = Arc::new(crate::stacking::solve::FakeSolver::unavailable("no star catalog"));
        use_solver(&mut rc, &fake);
        seed_in_place_calibrated(&conn, &mut rc);
        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 0);
        assert!(crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("solve")).unwrap().is_empty());
        assert_eq!(
            rc.warnings,
            vec!["3 frame(s) without a WCS in the manifest could not be plate-solved: no star catalog".to_string()]
        );
    }

    /// Review Focus 1: a frame-set run has nothing to solve. The sub-step
    /// never asks the solver anything, the run gains no warning, and stage 4
    /// has no WCS rule.
    #[test]
    fn the_solve_sub_step_is_a_no_op_for_a_frame_set() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)], &[5.0f32; 3]);
        let cfg = StackingConfig::default();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let (run_id, group_ids) =
            seed_run_and_groups(&fixture.conn, fixture.set_id, &plan_groups, working.path(), output.path());
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter),
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            WorkingLayout::new(working.path(), &set_slug(SET_NAME)),
            output.path().to_path_buf(),
            group_ids,
        );
        let fake = Arc::new(crate::stacking::solve::FakeSolver::unavailable("never asked"));
        use_solver(&mut rc, &fake);
        solve_project_frames(&mut rc).unwrap();
        assert_eq!(fake.calls(), 0);
        assert!(rc.warnings.is_empty(), "{:?}", rc.warnings);
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::solve a_project_frames_wcs a_project_plan_reports_frames_that the_solve_sub_step a_failed_or_refused a_failure_cached_by_another without_a_solver`

Expected: the build fails. None of the new items exist.

- [ ] **Step 3: The solver seam** (`stacking/solve.rs`, a new file; add `pub mod solve;` to `stacking/mod.rs` after `pub mod run;`):
```rust
//! The Solve sub-step's solver seam (spec 2026-10-02 §5.2, plan 6b-2 C14).
//! A project frame whose manifest carries no WCS is plate-solved by the run
//! itself: on the file stage 1 handed on, with hints from its manifest scale
//! and the project's target. Every outcome is cached as a `solve` artifact
//! (group key `""`), keyed on the frame's content, the solver's config and
//! the installed catalog. The run never writes `plate_solves` or `frames`.
//! [`FrameSolver`] is the seam: [`CatalogSolver`] in production, a fake in
//! tests.
//!
//! Concurrency (review M18): [`write_outcome`] is a single UPSERT, so it
//! needs no `BEGIN IMMEDIATE`; one run per subject is enforced upstream, by
//! `start_stacking`'s admission. [`CatalogSolver`] opens the catalog once per
//! run through a `OnceLock`; each concurrent run, like each host, maps its
//! own.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::collab::frame_meta::ProjectFrameMeta;
use crate::db::stacking::{find_artifact, upsert_artifact, NewArtifact, StackSubject, StackingArtifactRow};
use crate::fits_writer::{write_fits_f32_with, Durability};
use crate::integration::plane_reader::PlaneReader;
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::storage::PlateSolveRecord;
use crate::plate_solve::SolveHints;
use crate::services::ServiceContext;
use crate::stacking::config::stage_hash;

/// One solve the run asks for.
#[derive(Debug, Clone)]
pub struct SolveRequest {
    /// The frame's handle; it names the solve's tracing span (review M12).
    pub handle: i64,
    /// The file to solve: the `calibrated` artifact stage 1 handed on.
    pub path: PathBuf,
    pub hints: SolveHints,
    /// The scale the acceptance gate compares against: the manifest's.
    pub expected_scale_arcsec: Option<f64>,
    pub date_obs: Option<DateTime<Utc>>,
    pub focal_len: Option<f64>,
    pub instrume: Option<String>,
    pub naxis: Option<(i64, i64)>,
}

impl SolveRequest {
    /// The request for a project frame (spec §5.2):
    /// - the position hint is the project's target;
    /// - the scale hint and the gate's expected scale are the manifest's
    ///   `pixelScaleArcsec`;
    /// - the field of view is that scale times the frame's width, as the
    ///   hosts compute it from the optics (review M11).
    pub fn for_frame(handle: i64, path: &Path, meta: &ProjectFrameMeta, target: Option<(f64, f64)>) -> Self {
        let fov_deg = match (meta.pixel_scale_arcsec, meta.naxis1) {
            (Some(scale), Some(w)) if scale > 0.0 => Some(scale * w as f64 / 3600.0),
            _ => None,
        };
        SolveRequest {
            handle,
            path: path.to_path_buf(),
            hints: SolveHints {
                ra: target.map(|t| t.0),
                dec: target.map(|t| t.1),
                fov_deg,
                rotation: None,
                pixel_scale_arcsec: meta.pixel_scale_arcsec,
            },
            expected_scale_arcsec: meta.pixel_scale_arcsec,
            date_obs: meta
                .date_obs
                .as_deref()
                .and_then(|d| crate::fits_parser::parse_date_obs(d, None).ok()),
            focal_len: meta.focal_len,
            instrume: meta.instrume.clone(),
            naxis: meta.naxis1.zip(meta.naxis2),
        }
    }
}

/// What one solve came to. Every outcome is cached, so a frame that failed
/// or was refused is not tried again while its content, the solver config
/// and the catalog hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SolveOutcome {
    /// Accepted. The record's `frame_id` is 0, because there is no catalog row.
    Solved(PlateSolveRecord),
    /// The solver found no solution, or could not run.
    Failed(String),
    /// The solver answered, and the acceptance gate refused it.
    Refused(String),
}

impl SolveOutcome {
    /// The `outcome` log field.
    pub fn label(&self) -> &'static str {
        match self {
            SolveOutcome::Solved(_) => "solved",
            SolveOutcome::Failed(_) => "failed",
            SolveOutcome::Refused(_) => "refused",
        }
    }
}

/// A `solve` artifact's payload: the content it is about, the app version
/// that wrote it (review I7; empty for a payload without one), and the
/// outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveArtifact {
    pub blake3: String,
    #[serde(default)]
    pub app_version: String,
    pub outcome: SolveOutcome,
}

/// The app version a `solve` artifact records: the core crate's, which the
/// release bumps with the app.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether a cached `solve` artifact still answers for this frame (plan 6b-2
/// C14, review I7). Its hash must match (same content, solver config and
/// catalog), and it must be a solve, or a failure or refusal written by this
/// very app version. A release may carry a solver that rescues a frame, so a
/// failure from another version is tried once more; a solve is kept.
pub(crate) fn cached_outcome_holds(row: &StackingArtifactRow, hash: &str) -> bool {
    if row.config_hash != hash {
        return false;
    }
    let Some(payload) = row.payload_json.as_deref() else {
        tracing::warn!(frame_id = ?row.frame_id, "solve artifact without a payload; solving again");
        return false;
    };
    match serde_json::from_str::<SolveArtifact>(payload) {
        Ok(artifact) => matches!(artifact.outcome, SolveOutcome::Solved(_)) || artifact.app_version == APP_VERSION,
        Err(e) => {
            tracing::warn!(frame_id = ?row.frame_id, error = %e, "solve artifact unreadable; solving again");
            false
        }
    }
}

/// The `solve` artifact's hash: the frame's content and the solver's
/// identity ([`FrameSolver::identity`]). It is not the Prepare hash, so a
/// debayer toggle never re-solves.
pub fn solve_hash(blake3: &str, solver_identity: &str) -> String {
    stage_hash(
        &serde_json::json!({ "solver": solver_identity }),
        &[&format!("blake3:{blake3}")],
        &[],
    )
}

/// The cached outcome for a project frame's CURRENT content: `None` when
/// there is none, or when the cached one is about other content.
pub(crate) fn stored_outcome(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
    blake3: &str,
) -> anyhow::Result<Option<SolveOutcome>> {
    let subject = StackSubject::Project(project_id.to_string());
    let Some(row) = find_artifact(conn, &subject, "", "solve", Some(frame_id))? else {
        return Ok(None);
    };
    let Some(payload) = row.payload_json.as_deref() else {
        tracing::warn!(project_id, frame_id, "solve artifact without a payload; treated as no solve");
        return Ok(None);
    };
    match serde_json::from_str::<SolveArtifact>(payload) {
        Ok(artifact) => Ok((artifact.blake3 == blake3).then_some(artifact.outcome)),
        Err(e) => {
            tracing::warn!(project_id, frame_id, error = %e, "solve artifact unreadable; treated as no solve");
            Ok(None)
        }
    }
}

/// Cache one outcome as the frame's `solve` artifact (group key `""`, no
/// file), replacing any earlier one, stamped with this app version.
pub(crate) fn write_outcome(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
    blake3: &str,
    hash: &str,
    outcome: SolveOutcome,
) -> anyhow::Result<()> {
    write_artifact(
        conn,
        project_id,
        frame_id,
        hash,
        &SolveArtifact {
            blake3: blake3.to_string(),
            app_version: APP_VERSION.to_string(),
            outcome,
        },
    )
}

/// [`write_outcome`] with the whole payload given (a test writes another
/// app version's outcome through it).
pub(crate) fn write_artifact(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
    hash: &str,
    artifact: &SolveArtifact,
) -> anyhow::Result<()> {
    let subject = StackSubject::Project(project_id.to_string());
    let payload = serde_json::to_string(artifact)?;
    upsert_artifact(
        conn,
        &NewArtifact {
            subject: &subject,
            frame_id: Some(frame_id),
            group_key: "",
            kind: "solve",
            path: None,
            config_hash: hash,
            size: None,
            modified_at: None,
            payload_json: Some(&payload),
        },
    )?;
    Ok(())
}

/// The solver seam (plan 6b-2 C14).
pub trait FrameSolver: Send + Sync {
    /// The solver's identity for the cache key: its config and the installed
    /// catalog. `Err` when it cannot solve at all (no catalog).
    fn identity(&self) -> Result<String, String>;
    /// Solve one file. Never panics, and never writes the catalog.
    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome;
}

/// One solve through the real solver with catalog layers already open:
/// `solve_frame_with_hints` (never `solve_frame` or `store_result`), then
/// the acceptance gate at the request's expected scale. A solver panic is
/// caught and reported as a failure, as the hosts' batch does.
pub fn solve_with(
    request: &SolveRequest,
    caches: &solvemyastro::Caches<'_>,
    config: &PlateSolveConfig,
    cancel: &AtomicBool,
) -> SolveOutcome {
    let frame = Frame {
        // Only the solve's tracing span reads it (review M12).
        id: Some(request.handle),
        date_obs: request.date_obs,
        focallen: request.focal_len,
        instrume: request.instrume.clone(),
        naxis1: request.naxis.map(|(w, _)| w as i32),
        naxis2: request.naxis.map(|(_, h)| h as i32),
        ..Frame::default()
    };
    let path = request.path.to_string_lossy().into_owned();
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::plate_solve::solve_frame_with_hints(&frame, &path, &request.hints, caches, config, Some(cancel))
    }));
    match attempt {
        Ok(Ok(result)) => {
            match crate::plate_solve::service::acceptance_refusal(&result, request.expected_scale_arcsec, config) {
                Some(reason) => SolveOutcome::Refused(reason),
                None => SolveOutcome::Solved(crate::plate_solve::service::record_from_result(0, &result)),
            }
        }
        Ok(Err(e)) => SolveOutcome::Failed(format!("{e:#}")),
        Err(_) => {
            tracing::error!(path = %request.path.display(), "the plate solver panicked");
            SolveOutcome::Failed("the plate solver panicked".to_string())
        }
    }
}

/// An ADU-scaled (×65535) float32 copy of a unit-domain FITS, with its
/// copy-through cards. It is the real-data probe's comparison input, and the
/// fallback a probe result may call for (plan 6b-2 Task 10c Step 8). It
/// never touches `src`.
pub fn adu_scaled_copy(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let reader = PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
    let (w, h, c) = (reader.width(), reader.height(), reader.channels());
    let mut data = Vec::with_capacity(w * h * c);
    for plane in 0..c {
        let values = reader.read_plane(plane).with_context(|| format!("reading {}", src.display()))?;
        data.extend(values.into_iter().map(|v| v * 65535.0));
    }
    let cards = crate::stacking::prepare::source_cards_of(src)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_fits_f32_with(dest, w, h, c, &data, &cards, Durability::Volatile)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
    Ok(())
}

/// The production solver. It uses the installed catalog, opened on first use
/// (a run that solves nothing never opens it), and the stored plate-solve
/// config.
pub struct CatalogSolver {
    ctx: Arc<ServiceContext>,
    opened: OnceLock<Result<OpenedCatalog, String>>,
}

struct OpenedCatalog {
    layers: Vec<Arc<solvemyastro::StarCache>>,
    config: PlateSolveConfig,
    identity: String,
}

impl CatalogSolver {
    pub fn new(ctx: Arc<ServiceContext>) -> Self {
        CatalogSolver {
            ctx,
            opened: OnceLock::new(),
        }
    }

    fn opened(&self) -> Result<&OpenedCatalog, String> {
        self.opened
            .get_or_init(|| {
                let db = self.ctx.db.get().ok_or_else(|| "the database is not initialized".to_string())?;
                let db_path = db.path().to_path_buf();
                let app_data = db_path
                    .parent()
                    .ok_or_else(|| "cannot determine the app data directory".to_string())?;
                let config = crate::plate_solve::config::load_config(&db.conn());
                let layers = crate::plate_solve::open_catalog_layers(app_data).map_err(|e| e.to_string())?;
                let catalog = crate::plate_solve::catalog_identity(app_data).map_err(|e| e.to_string())?;
                let identity = format!("{}:{catalog}", crate::plate_solve::config::solver_config_hash(&config));
                Ok(OpenedCatalog {
                    layers,
                    config,
                    identity,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl FrameSolver for CatalogSolver {
    fn identity(&self) -> Result<String, String> {
        self.opened().map(|o| o.identity.clone())
    }

    fn solve(&self, request: &SolveRequest, cancel: &AtomicBool) -> SolveOutcome {
        let opened = match self.opened() {
            Ok(o) => o,
            Err(e) => return SolveOutcome::Failed(e),
        };
        let refs: Vec<&solvemyastro::StarCache> = opened.layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        solve_with(request, &caches, &opened.config, cancel)
    }
}

/// A solver for tests: it "solves" every file at the fixture target (83.8,
/// −5.4, 1.5″/px) unless told otherwise per file name, and counts its calls.
#[cfg(test)]
pub(crate) struct FakeSolver {
    identity: Result<String, String>,
    by_file: std::collections::HashMap<String, SolveOutcome>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeSolver {
    pub(crate) fn solving_all() -> Self {
        FakeSolver {
            identity: Ok("fake:1".to_string()),
            by_file: std::collections::HashMap::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Answer `outcome` for the file named `file_name`.
    pub(crate) fn with(mut self, file_name: &str, outcome: SolveOutcome) -> Self {
        self.by_file.insert(file_name.to_string(), outcome);
        self
    }

    /// A solver that cannot run at all.
    pub(crate) fn unavailable(reason: &str) -> Self {
        FakeSolver {
            identity: Err(reason.to_string()),
            ..Self::solving_all()
        }
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The fixture frame's solve: `manifest_wcs_json(64, 48, 83.8, -5.4, 1.5)`.
    pub(crate) fn fixture_record() -> PlateSolveRecord {
        let wcs: crate::collab::frame_meta::ManifestWcs = serde_json::from_value(
            crate::stacking::test_fixtures::manifest_wcs_json(64, 48, 83.8, -5.4, 1.5),
        )
        .expect("the fixture WCS parses");
        wcs.to_record(Some(1.5))
    }
}

#[cfg(test)]
impl FrameSolver for FakeSolver {
    fn identity(&self) -> Result<String, String> {
        self.identity.clone()
    }

    fn solve(&self, request: &SolveRequest, _cancel: &AtomicBool) -> SolveOutcome {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let name = request
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.by_file
            .get(&name)
            .cloned()
            .unwrap_or_else(|| SolveOutcome::Solved(Self::fixture_record()))
    }
}
```
If `ManifestWcs`, `ProjectFrameMeta` or `parse_date_obs` live under different paths, use the ones Task 4 and `fits_parser/mod.rs:947` define. All three exist by Task 4.

- [ ] **Step 4: The origin funnels** (`origin.rs`). Add `use crate::stacking::solve::{stored_outcome, SolveOutcome};`. Replace Task 6a's project arm in `stored_solve` with:
```rust
        // Spec §5.2: the manifest's WCS first, then the run's own cached
        // solve of this content.
        FrameOrigin::Project {
            project_id,
            blake3,
            meta,
            ..
        } => match &meta.wcs {
            Some(wcs) => Ok(Some(wcs.to_record(meta.pixel_scale_arcsec))),
            None => Ok(match stored_outcome(conn, project_id, frame_id, blake3)? {
                Some(SolveOutcome::Solved(record)) => Some(record),
                _ => None,
            }),
        },
```
and append:
```rust
/// What the Solve sub-step needs about a frame it must solve: a project
/// frame whose manifest carries no WCS (spec §5.2).
pub(crate) struct SolvePlan {
    pub(crate) project_id: String,
    pub(crate) frame_uuid: String,
    pub(crate) blake3: String,
    pub(crate) meta: ProjectFrameMeta,
}

/// `Some` only for a project frame without a manifest WCS. A run never
/// solves a catalog frame.
pub(crate) fn solve_plan(origin: &FrameOrigin) -> Option<SolvePlan> {
    match origin {
        FrameOrigin::Catalog { .. } => None,
        FrameOrigin::Project { meta, .. } if meta.wcs.is_some() => None,
        FrameOrigin::Project {
            project_id,
            frame_uuid,
            blake3,
            meta,
            ..
        } => Some(SolvePlan {
            project_id: project_id.clone(),
            frame_uuid: frame_uuid.clone(),
            blake3: blake3.clone(),
            meta: meta.clone(),
        }),
    }
}

/// The run's cached solve outcome for a frame's current content, which the
/// plan's "will be solved" and "could not be solved" warnings read. `None`
/// for a catalog frame, and for a frame never solved.
pub(crate) fn local_solve_outcome(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
) -> anyhow::Result<Option<SolveOutcome>> {
    match origin {
        FrameOrigin::Catalog { .. } => Ok(None),
        FrameOrigin::Project {
            project_id,
            blake3,
            ..
        } => stored_outcome(conn, project_id, frame_id, blake3),
    }
}
```

- [ ] **Step 5: The run** (`run.rs`).
  1. `RunContext` gains, after `origins`:
```rust
    /// The Solve sub-step's solver (plan 6b-2 C14): the installed catalog,
    /// opened on first use. Tests inject a fake.
    pub(crate) solver: Arc<dyn crate::stacking::solve::FrameSolver>,
```
  In `start_stacking`'s literal add `solver: Arc::new(crate::stacking::solve::CatalogSolver::new(ctx.clone())),`. In `test_context`, bind `let solver: Arc<dyn crate::stacking::solve::FrameSolver> = Arc::new(crate::stacking::solve::CatalogSolver::new(ctx.clone()));` before the literal, because `ctx` moves into it, and add `solver,`.
  2. `stage_measure`: right after `let stage_start = Instant::now();`:
```rust
    // Plan 6b-2 C14: a project's frames without a manifest WCS are solved
    // first; the Solve boundary's exclusions reach `stage1_failed` below.
    solve_project_frames(rc)?;
```
  3. Add before `stage_measure`:
```rust
/// The Solve sub-step (spec §5.2, plan 6b-2 C14), the first thing Measure
/// does. Every project frame still in the run whose manifest carries no WCS,
/// and that has a stage-1 file, is solved once per content version, solver
/// config and catalog (and once more per app version after a failure,
/// review I7). It is solved through `rc.solver`, on the file stage 1 handed
/// on, and its outcome (solved, failed or refused) is cached as a `solve`
/// artifact under group key `""`, so a regroup never re-solves. A failed or
/// refused solve leaves the frame in the run, without a WCS. A frame without
/// a stage-1 file is skipped with a `debug!` and left to Measure, which
/// excludes it (review I9). Nothing here writes `plate_solves` or `frames`.
/// A frame set has no such frame, and reads and logs nothing.
fn solve_project_frames(rc: &mut RunContext) -> Result<(), RunError> {
    use crate::stacking::solve::{solve_hash, write_outcome, SolveOutcome, SolveRequest};
    let excluded: HashSet<i64> = rc.excluded.iter().copied().collect();
    let failed: HashSet<i64> = rc.runtime_exclusions.iter().map(|(id, _)| *id).collect();
    let mut wanted = Vec::new();
    for g in &rc.plan_groups {
        for f in g
            .frames
            .iter()
            .filter(|f| !excluded.contains(&f.frame_id) && !failed.contains(&f.frame_id))
        {
            // A frame with no origin is left to Measure, which reports it as
            // it always has.
            let Some(origin) = rc.origins.get(&f.frame_id) else {
                continue;
            };
            if let Some(plan) = crate::stacking::origin::solve_plan(origin) {
                wanted.push((g.key.clone(), f.frame_id, plan));
            }
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let identity = match rc.solver.identity() {
        Ok(identity) => identity,
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                count = wanted.len(),
                error = %e,
                "plate solving unavailable; frames without a manifest wcs stay unsolved"
            );
            rc.warnings.push(format!(
                "{} frame(s) without a WCS in the manifest could not be plate-solved: {e}",
                wanted.len()
            ));
            return Ok(());
        }
    };
    let project_id = wanted[0].2.project_id.clone();
    let target = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::collab::get_project(&conn, &project_id)?.map(|p| (p.target_ra_deg, p.target_dec_deg))
    };

    let mut jobs = Vec::new();
    for (key, frame_id, plan) in wanted {
        // Plan 6b-2 C9: the Solve boundary of the landed-file identity check.
        if let Some(reason) = changed_during_run(rc, frame_id)? {
            tracing::warn!(run_id = rc.run_id, frame_id, reason = %reason, "frame excluded");
            rc.runtime_exclusions.push((frame_id, reason));
            continue;
        }
        let hash = solve_hash(&plan.blake3, &identity);
        let conn = db(&rc.ctx)?.conn();
        let cached = crate::db::stacking::find_artifact(&conn, &rc.subject, "", "solve", Some(frame_id))?
            .is_some_and(|row| crate::stacking::solve::cached_outcome_holds(&row, &hash));
        if cached {
            continue;
        }
        let calibrated = crate::db::stacking::find_artifact(&conn, &rc.subject, &key, "calibrated", Some(frame_id))?
            .and_then(|a| a.path);
        // No stage-1 file (its artifact row is gone): Measure reports and
        // excludes the frame (review I9).
        let Some(path) = calibrated else {
            tracing::debug!(run_id = rc.run_id, frame_id, "no stage-1 file; solve skipped");
            continue;
        };
        jobs.push((frame_id, hash, plan, PathBuf::from(path)));
    }

    let total = jobs.len();
    let mut solved = 0usize;
    for (done, (frame_id, hash, plan, path)) in jobs.into_iter().enumerate() {
        rc.check_cancel()?;
        rc.progress(Stage::Measure, None, done, total, 0, 0, Some(frame_id), Some("solving".to_string()));
        let request = SolveRequest::for_frame(frame_id, &path, &plan.meta, target);
        let solver = rc.solver.clone();
        let cancel = rc.cancel.clone();
        let started = Instant::now();
        let outcome = rc.ctx.image_pool.install(|| solver.solve(&request, &cancel));
        rc.check_cancel()?;
        let duration_ms = started.elapsed().as_millis() as u64;
        match &outcome {
            SolveOutcome::Solved(_) => {
                solved += 1;
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id,
                    frame_uuid = %plan.frame_uuid,
                    outcome = outcome.label(),
                    duration_ms,
                    "project frame solved"
                );
            }
            SolveOutcome::Failed(reason) | SolveOutcome::Refused(reason) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    frame_uuid = %plan.frame_uuid,
                    outcome = outcome.label(),
                    reason = %reason,
                    duration_ms,
                    "project frame could not be plate-solved"
                );
            }
        }
        let conn = db(&rc.ctx)?.conn();
        write_outcome(&conn, &plan.project_id, frame_id, &plan.blake3, &hash, outcome)?;
    }
    if total > 0 {
        rc.progress(Stage::Measure, None, total, total, 0, 0, None, Some("solving".to_string()));
    }
    tracing::info!(run_id = rc.run_id, project_id = %project_id, count = total, solved, "project frames solved");
    Ok(())
}
```

- [ ] **Step 6: The plan's refinement** (`plan.rs`, `project_inputs`). Replace Task 7's `unsolved` block (from `// Spec §5.2: an in-scope frame without a manifest WCS is solved locally.` through its `if unsolved > 0 { … }`) with:
```rust
    // Spec §5.2: an in-scope frame without a manifest WCS is solved by the
    // run. The last cached outcome for its content (plan 6b-2 Task 10b) says
    // which frames are still to solve and which could not be solved. The plan
    // reads the outcome, not its hash: after a solver or catalog change the
    // run re-solves, and the plan learns the new outcome only then.
    let (mut to_solve, mut unsolvable) = (0usize, 0usize);
    for f in source
        .frames
        .iter()
        .filter(in_scope)
        .filter(|f| f.meta.as_ref().is_some_and(|m| m.wcs.is_none()))
    {
        let Some(handle) = f.handle else { continue };
        let outcome =
            crate::stacking::origin::local_solve_outcome(conn, origin_of(&source.origins, handle)?, handle)
                .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
        match outcome {
            None => to_solve += 1,
            Some(crate::stacking::solve::SolveOutcome::Solved(_)) => {}
            Some(_) => unsolvable += 1,
        }
    }
    if to_solve > 0 {
        warnings.push(format!(
            "{to_solve} frame{} no WCS in the manifest and will be plate-solved locally",
            if to_solve == 1 { " has" } else { "s have" }
        ));
    }
    if unsolvable > 0 {
        warnings.push(format!(
            "{unsolvable} frame{} could not be plate-solved and cannot anchor the grid",
            if unsolvable == 1 { "" } else { "s" }
        ));
    }
```
  Task 7's `a_project_plan_reports_its_groups_frames_grid_and_warnings` keeps passing: no outcome is cached there, so `o1` still reads "will be plate-solved locally".

- [ ] **Step 7: Run the tests and watch them pass.** First the Step 2 command. Then:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking:: subject_and_origin_variants_stay_in_their_funnels
```
  Expected: all pass. A frame-set run's Measure is unchanged: the sub-step finds no frame to solve and returns before any read.

- [ ] **Step 8: Run the gate.** TS must not move: `git diff --exit-code src/types`.

- [ ] **Step 9: Commit** with the message `feat(stacking): the solver seam and the Solve sub-step`, plus the trailers.

---

### Task 10c: The WCS reference rule and the real-data solve probe (C14, C15; spec §5.4)

**Files:**
- Modify `crates/athenaeum-core/src/stacking/weights.rs`: add `two_pass_pick_among`, which `two_pass_pick` delegates to, plus a test.
- Modify `crates/athenaeum-core/src/stacking/origin.rs`: add `WcsTier` and `wcs_tier`, plus a test.
- Modify `crates/athenaeum-core/src/stacking/run.rs`:
  - `RunContext` gains `wcs_rule`, with its two constructors;
  - add `WcsRule`, `NO_PROJECT_REFERENCE`, `MANUAL_REFERENCE_WITHOUT_WCS`, `wcs_tiers`, `best_included_idx_where` and `best_candidate_idx`;
  - change `stage_reference`, `group_reference_idx` and `two_pass_refine`'s pick (`~5168`);
  - tests.
- Modify `crates/athenaeum-core/src/stacking/solve.rs`: the `#[ignore]` real-data probe; Step 8's conditional fallback.

**Interfaces:**
```rust
// stacking/origin.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub(crate) enum WcsTier { NotApplicable, Manifest, Local, Missing }
pub(crate) fn wcs_tier(conn: &Connection, origin: &FrameOrigin, frame_id: i64) -> anyhow::Result<WcsTier>;
// stacking/weights.rs
pub(crate) fn two_pass_pick_among(candidates: &[TwoPassCandidate], reference_idx: usize, diagonal_px: f64, min_gain_px: f64, allowed: &dyn Fn(usize) -> bool) -> Option<usize>;
// stacking/run.rs
pub(crate) struct WcsRule { pub(crate) candidates: HashSet<i64>, pub(crate) with_wcs: HashSet<i64> }
const NO_PROJECT_REFERENCE: &str = "no frame of this project could be plate-solved; the grid needs one";
const MANUAL_REFERENCE_WITHOUT_WCS: &str = "the manual reference frame has no WCS (none in the manifest, and the run could not plate-solve it); the grid needs one";
fn wcs_tiers(rc: &RunContext) -> Result<Option<WcsRule>, RunError>;
```
Consumed by: plan 6b-3 (`rc.wcs_rule` is the grid reference's rule).

`WcsRule`, the `wcs_rule` field and `wcs_tier` land here, not in Task 10b, because nothing reads them before the reference rule; in 10b they would be dead code in the lib check.

- [ ] **Step 1: Write the failing tests**

`stacking/origin.rs` tests:
```rust
    /// Plan 6b-2 C15: the tier the reference rule reads. It is the manifest's
    /// WCS, then the run's own solve of this content (a cached failure is no
    /// WCS), else none; a catalog frame has no rule.
    #[test]
    fn the_wcs_tier_is_the_manifest_then_the_local_solve() {
        use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        test_fixtures::add_project_frame(&conn, dir.path(), "p1", &test_fixtures::project_frame_spec("a"));
        let mut with_wcs = test_fixtures::project_frame_spec("m");
        with_wcs.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 10.0, 20.0, 1.5));
        test_fixtures::add_project_frame(&conn, dir.path(), "p1", &with_wcs);
        let source = crate::stacking::project_source::project_frame_source(
            &conn,
            "p1",
            &crate::stacking::config::StackingConfig::default(),
        )
        .unwrap();
        let of = |uuid: &str| -> (i64, FrameOrigin, String) {
            source
                .origins
                .iter()
                .find_map(|(id, o)| match o {
                    FrameOrigin::Project { frame_uuid, blake3, .. } if frame_uuid == uuid => {
                        Some((*id, o.clone(), blake3.clone()))
                    }
                    _ => None,
                })
                .unwrap()
        };
        let (a_id, a, a_blake3) = of("a");
        let (m_id, m, _) = of("m");
        assert_eq!(wcs_tier(&conn, &a, a_id).unwrap(), WcsTier::Missing);
        write_outcome(&conn, "p1", a_id, &a_blake3, "h", SolveOutcome::Failed("no match".into())).unwrap();
        assert_eq!(wcs_tier(&conn, &a, a_id).unwrap(), WcsTier::Missing, "a failure is no WCS");
        write_outcome(&conn, "p1", a_id, &a_blake3, "h", SolveOutcome::Solved(FakeSolver::fixture_record())).unwrap();
        assert_eq!(wcs_tier(&conn, &a, a_id).unwrap(), WcsTier::Local);
        assert_eq!(wcs_tier(&conn, &m, m_id).unwrap(), WcsTier::Manifest);
        let catalog = FrameOrigin::Catalog { file_id: 1, attested: false };
        assert_eq!(wcs_tier(&conn, &catalog, 1).unwrap(), WcsTier::NotApplicable);
    }
```
`stacking/weights.rs` tests:
```rust
    /// Plan 6b-2 C15: the re-pick among WCS candidates. The median still
    /// spans every usable frame; only an allowed frame can win; with nothing
    /// allowed but the reference itself there is no switch.
    #[test]
    fn two_pass_pick_among_never_lets_a_non_candidate_win() {
        let c = |idx: usize, weight: f64, tx: f64| TwoPassCandidate {
            idx,
            weight,
            rotation_deg: 0.0,
            translation: (tx, 0.0),
        };
        // Median translation x = (1 + 2) / 2 = 1.5; deviations 38.5, 1.5, 0.5, 0.5.
        let set = [c(0, 1.0, 40.0), c(1, 0.9, 0.0), c(2, 0.8, 1.0), c(3, 0.7, 2.0)];
        assert_eq!(two_pass_pick(&set, 0, 100.0, 4.0), Some(2), "a tie at 0.5 goes to the lower idx");
        assert_eq!(two_pass_pick_among(&set, 0, 100.0, 4.0, &|_| true), Some(2));
        assert_eq!(two_pass_pick_among(&set, 0, 100.0, 4.0, &|i| i != 2), Some(3));
        assert_eq!(two_pass_pick_among(&set, 0, 100.0, 4.0, &|i| i == 1), Some(1));
        assert_eq!(two_pass_pick_among(&set, 0, 100.0, 4.0, &|_| false), None);
    }
```
`run.rs` tests (they use Task 10b's `handle_of`, `seed_r_and_g` and `blake3_of`). The helper first, then the tests:
```rust
    /// Every frame of `rc` measured and included, its weight from `weights`
    /// by file name (0.5 by default), its calibrated file its own landed path:
    /// what stages 4 and 5 read, without running Measure.
    fn measure_all(rc: &mut RunContext, weights: &HashMap<&str, f64>) {
        for g in rc.plan_groups.clone() {
            let entries: Vec<MeasuredFrame> = g
                .frames
                .iter()
                .map(|f| {
                    let w = weights.get(f.filename.as_str()).copied().unwrap_or(0.5);
                    MeasuredFrame {
                        frame: f.clone(),
                        calibrated: Some(PathBuf::from(&f.path)),
                        planes: 1,
                        measurement: None,
                        weight: Some(FrameWeight {
                            channels: vec![w],
                            normalized: vec![w],
                            mean: w,
                            normalized_mean: w,
                            missing: None,
                        }),
                        included: true,
                        reason: None,
                        registration: None,
                        cached_calibrated: true,
                        cached_metrics: false,
                        ln_scale: None,
                        cached_ln: false,
                    }
                })
                .collect();
            rc.measured.insert(g.key.clone(), entries);
        }
    }

    /// Spec §5.4, plan 6b-2 C15: a project's auto reference is the best frame
    /// WITH a WCS, from the largest group that holds one. It is never taken
    /// from a larger, better-weighted group without one. In native geometry,
    /// a group with no candidate takes the run reference.
    #[test]
    fn a_projects_auto_reference_is_the_best_frame_with_a_wcs() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 4, 3);
        let weights: HashMap<&str, f64> = [
            ("c_r1.fits", 0.9),
            ("c_r2.fits", 0.9),
            ("c_r3.fits", 0.9),
            ("c_r4.fits", 0.9),
            ("c_g1.fits", 0.4),
            ("c_g2.fits", 0.7),
            ("c_g3.fits", 0.6),
        ]
        .into_iter()
        .collect();
        let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &work, &out);
        measure_all(&mut rc, &weights);
        stage_reference(&mut rc).unwrap();
        let g2 = handle_of(&rc, "c_g2.fits");
        assert_eq!(rc.reference_frame_id, Some(g2));
        assert_eq!(rc.summary.reference.filename.as_deref(), Some("c_g2.fits"));
        let rule = rc.wcs_rule.as_ref().unwrap();
        assert_eq!(rule.candidates.len(), 3);
        assert!(!rule.candidates.contains(&handle_of(&rc, "c_r1.fits")));

        let mut native = StackingConfig::default();
        native.registration.geometry = RegistrationGeometry::Native;
        let mut rc = project_rc(&ctx, &conn, "p1", native, &work, &out);
        measure_all(&mut rc, &weights);
        stage_reference(&mut rc).unwrap();
        assert_eq!(rc.group_geometry["mono__G__300s"].reference_frame_id, g2);
        assert_eq!(
            rc.group_geometry["mono__R__300s"].reference_frame_id, g2,
            "a group with no candidate takes the run reference"
        );
    }

    /// Spec §5.4, plan 6b-2 C15: a frame the run solved locally qualifies
    /// only when no included frame has a manifest WCS.
    #[test]
    fn a_locally_solved_frame_qualifies_only_without_a_manifest_wcs() {
        use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));

        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab1"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &work, &out);
        let r2 = handle_of(&rc, "c_r2.fits");
        write_outcome(&conn, "p1", r2, &blake3_of(&conn, "p1", "r2"), "h", SolveOutcome::Solved(FakeSolver::fixture_record())).unwrap();
        let weights: HashMap<&str, f64> =
            [("c_r1.fits", 0.9), ("c_r2.fits", 0.5), ("c_r3.fits", 0.8)].into_iter().collect();
        measure_all(&mut rc, &weights);
        stage_reference(&mut rc).unwrap();
        assert_eq!(rc.reference_frame_id, Some(r2), "the one frame with a WCS, not the best-weighted");

        crate::db::stacking::insert_test_project(&conn, "p2");
        seed_r_and_g(&conn, &tmp.path().join("collab2"), "p2", 3, 3);
        let mut rc = project_rc(&ctx, &conn, "p2", StackingConfig::default(), &work, &out);
        let r2 = handle_of(&rc, "c_r2.fits");
        write_outcome(&conn, "p2", r2, &blake3_of(&conn, "p2", "r2"), "h", SolveOutcome::Solved(FakeSolver::fixture_record())).unwrap();
        measure_all(&mut rc, &[("c_r2.fits", 0.99)].into_iter().collect());
        stage_reference(&mut rc).unwrap();
        let rule = rc.wcs_rule.as_ref().unwrap();
        assert!(!rule.candidates.contains(&r2) && rule.with_wcs.contains(&r2), "{rule:?}");
        let g: HashSet<i64> = ["c_g1.fits", "c_g2.fits", "c_g3.fits"].iter().map(|n| handle_of(&rc, n)).collect();
        assert!(g.contains(&rc.reference_frame_id.unwrap()));
    }

    /// Spec §5.4: with no frame carrying a WCS the reference stage fails, in
    /// the spec's words.
    #[test]
    fn with_no_frame_with_a_wcs_the_reference_stage_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 0);
        let mut rc = project_rc(&ctx, &conn, "p1", StackingConfig::default(), &tmp.path().join("work"), &tmp.path().join("out"));
        measure_all(&mut rc, &HashMap::new());
        let err = stage_reference(&mut rc).unwrap_err();
        assert!(matches!(err, RunError::Other(ref m) if m == NO_PROJECT_REFERENCE), "{err:?}");
        assert_eq!(NO_PROJECT_REFERENCE, "no frame of this project could be plate-solved; the grid needs one");
    }

    /// Plan 6b-2 C15: a manual pin without a WCS fails the stage; a pin with
    /// one is the reference.
    #[test]
    fn a_manual_pin_without_a_wcs_fails_the_reference_stage() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let conn = project_conn(&db_path);
        crate::db::stacking::insert_test_project(&conn, "p1");
        seed_r_and_g(&conn, &tmp.path().join("collab"), "p1", 3, 3);
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let (work, out) = (tmp.path().join("work"), tmp.path().join("out"));

        let mut rc = project_rc(&ctx, &conn, "p1", cfg.clone(), &work, &out);
        crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("r1")).unwrap();
        measure_all(&mut rc, &HashMap::new());
        let err = stage_reference(&mut rc).unwrap_err();
        assert!(matches!(err, RunError::Other(ref m) if m == MANUAL_REFERENCE_WITHOUT_WCS), "{err:?}");

        let mut rc = project_rc(&ctx, &conn, "p1", cfg, &work, &out);
        crate::db::stacking::set_project_config(&conn, "p1", "{}", &[], Some("g2")).unwrap();
        measure_all(&mut rc, &HashMap::new());
        stage_reference(&mut rc).unwrap();
        assert_eq!(rc.reference_frame_id, Some(handle_of(&rc, "c_g2.fits")));
    }

    /// Review Focus 1, plan 6b-2 C15: a frame-set run has no WCS rule, so its
    /// reference pick, its native groups and its two-pass re-pick take
    /// today's paths unchanged.
    #[test]
    fn a_frame_set_run_has_no_wcs_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)], &[5.0f32; 3]);
        let cfg = StackingConfig::default();
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        let (run_id, group_ids) =
            seed_run_and_groups(&fixture.conn, fixture.set_id, &plan_groups, working.path(), output.path());
        let rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter),
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg,
            plan_groups,
            WorkingLayout::new(working.path(), &set_slug(SET_NAME)),
            output.path().to_path_buf(),
            group_ids,
        );
        assert!(wcs_tiers(&rc).unwrap().is_none());
        assert!(rc.wcs_rule.is_none());
    }
```

- [ ] **Step 2: Run the tests and watch them fail.**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 the_wcs_tier_is two_pass_pick_among a_projects_auto_reference a_locally_solved with_no_frame_with_a_wcs a_manual_pin_without a_frame_set_run_has_no_wcs_rule`

Expected: the build fails. None of the new items exist.

- [ ] **Step 3: The re-pick among candidates** (`weights.rs`). Rename `two_pass_pick`'s body into `two_pass_pick_among` with the extra `allowed: &dyn Fn(usize) -> bool` parameter. Insert one statement right after `reference_px` is computed and before `usable.sort_by(…)`:
```rust
    // Plan 6b-2 C15: the median and the reference's own deviation span every
    // usable frame; only an allowed one enters the top-N cut and can win.
    usable.retain(|c| c.idx == reference_idx || allowed(c.idx));
```
`two_pass_pick` keeps its doc and signature and becomes `two_pass_pick_among(candidates, reference_idx, diagonal_px, min_gain_px, &|_| true)`. Give `two_pass_pick_among` the doc: "[`two_pass_pick`] restricted to the candidates `allowed` accepts (spec §5.4, plan 6b-2 C15). The median and the current reference's deviation still span every usable candidate, because where the set points does not depend on who may win. Only an allowed frame enters the top-N cut."

- [ ] **Step 4: The WCS tier** (`origin.rs`). Append:
```rust
/// Where a frame's WCS comes from, for the reference rule (spec §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WcsTier {
    /// A catalog frame: the rule does not apply.
    NotApplicable,
    Manifest,
    /// Solved by a run (the cached `solve` artifact of this content).
    Local,
    /// No WCS at all.
    Missing,
}

pub(crate) fn wcs_tier(conn: &Connection, origin: &FrameOrigin, frame_id: i64) -> anyhow::Result<WcsTier> {
    match origin {
        FrameOrigin::Catalog { .. } => Ok(WcsTier::NotApplicable),
        FrameOrigin::Project { meta, .. } if meta.wcs.is_some() => Ok(WcsTier::Manifest),
        FrameOrigin::Project {
            project_id,
            blake3,
            ..
        } => Ok(match stored_outcome(conn, project_id, frame_id, blake3)? {
            Some(SolveOutcome::Solved(_)) => WcsTier::Local,
            _ => WcsTier::Missing,
        }),
    }
}
```

- [ ] **Step 5: The run** (`run.rs`).
  1. Add, beside `GroupGeometry`:
```rust
/// A project run's reference candidates (spec §5.4, plan 6b-2 C15).
#[derive(Debug, Clone, Default)]
pub(crate) struct WcsRule {
    /// Frames that may be the reference: those with a manifest WCS or, only
    /// when no included frame has one, those the run solved locally.
    pub(crate) candidates: HashSet<i64>,
    /// Every measured frame with any WCS (what a manual pin needs).
    pub(crate) with_wcs: HashSet<i64>,
}

/// Spec §5.4: the grid needs a reference with a WCS.
const NO_PROJECT_REFERENCE: &str = "no frame of this project could be plate-solved; the grid needs one";

/// Plan 6b-2 C15: a manual pin must carry a WCS.
const MANUAL_REFERENCE_WITHOUT_WCS: &str =
    "the manual reference frame has no WCS (none in the manifest, and the run could not plate-solve it); the grid needs one";
```
  2. `RunContext` gains, after `solver`:
```rust
    /// A project run's reference candidates, set by stage 4 (spec §5.4).
    /// `None` for a frame set, which has no WCS rule.
    pub(crate) wcs_rule: Option<WcsRule>,
```
  In `start_stacking`'s and `test_context`'s literals add `wcs_rule: None,`.
  3. The reference rule. Replace `best_included_idx` with:
```rust
fn best_included_idx(entries: &[MeasuredFrame]) -> Option<usize> {
    best_included_idx_where(entries, |_| true)
}

/// [`best_included_idx`] among the included entries `keep` accepts (plan
/// 6b-2 C15: a project's WCS candidates).
fn best_included_idx_where(entries: &[MeasuredFrame], keep: impl Fn(&MeasuredFrame) -> bool) -> Option<usize> {
    let weights: Vec<FrameWeight> = entries
        .iter()
        .map(|e| {
            e.weight.clone().unwrap_or(FrameWeight {
                channels: Vec::new(),
                normalized: Vec::new(),
                mean: 0.0,
                normalized_mean: 0.0,
                missing: None,
            })
        })
        .collect();
    let included: Vec<bool> = entries.iter().map(|e| e.included && keep(e)).collect();
    let star_counts: Vec<usize> = entries
        .iter()
        .map(|e| e.measurement.as_ref().map(|m| m.min_stars()).unwrap_or(0))
        .collect();
    best_by_weight(&weights, &included, &star_counts)
}

/// The group's best reference candidate: [`best_included_idx`] when the run
/// has no WCS rule, else the best included frame among `rule.candidates`.
fn best_candidate_idx(entries: &[MeasuredFrame], rule: Option<&WcsRule>) -> Option<usize> {
    match rule {
        None => best_included_idx(entries),
        Some(rule) => best_included_idx_where(entries, |e| rule.candidates.contains(&e.frame.frame_id)),
    }
}
```
  The body of `best_included_idx_where` is today's `best_included_idx` body, verbatim, except the `included` line, which adds `&& keep(e)`. Move today's `best_included_idx` doc onto `best_included_idx_where`. Add:
```rust
/// The reference rule's candidates for this run (spec §5.4, plan 6b-2 C15).
/// `None` for a frame set. For a project: the measured frames with a
/// manifest WCS, or (only when no included frame has one) those the run
/// solved locally; plus every measured frame with any WCS.
fn wcs_tiers(rc: &RunContext) -> Result<Option<WcsRule>, RunError> {
    use crate::stacking::origin::{wcs_tier, WcsTier};
    if rc.subject.project_id().is_none() {
        return Ok(None);
    }
    let conn = db(&rc.ctx)?.conn();
    let (mut manifest, mut local) = (HashSet::new(), HashSet::new());
    for entry in rc.measured.values().flatten().filter(|e| e.calibrated.is_some()) {
        let id = entry.frame.frame_id;
        match wcs_tier(&conn, origin_of(&rc.origins, id)?, id)? {
            WcsTier::Manifest => {
                manifest.insert(id);
            }
            WcsTier::Local => {
                local.insert(id);
            }
            WcsTier::Missing | WcsTier::NotApplicable => {}
        }
    }
    let manifest_included = rc
        .measured
        .values()
        .flatten()
        .any(|e| e.included && manifest.contains(&e.frame.frame_id));
    let with_wcs: HashSet<i64> = manifest.union(&local).copied().collect();
    let candidates = if manifest_included { manifest } else { local };
    tracing::debug!(run_id = rc.run_id, count = candidates.len(), "reference candidates with a wcs");
    Ok(Some(WcsRule { candidates, with_wcs }))
}
```
  In `stage_reference`, right after `let cfg = rc.config.clone();`:
```rust
    // Spec §5.4, plan 6b-2 C15: a project's reference must carry a WCS.
    rc.wcs_rule = wcs_tiers(rc)?;
```
  In the `Manual` arm, right after the `calibrated_missing` block (the `if calibrated_missing { … return Err(…) }` that ends at `~run.rs:3745`), so that "the manual reference frame is excluded: {reason}" keeps precedence over a missing WCS (review I6):
```rust
            if rc.wcs_rule.as_ref().is_some_and(|rule| !rule.with_wcs.contains(&frame_id)) {
                return Err(RunError::Other(MANUAL_REFERENCE_WITHOUT_WCS.to_string()));
            }
```
  In the `Auto` arm, inside the group loop right after `if included == 0 { continue; }`:
```rust
                if let Some(rule) = &rc.wcs_rule {
                    let holds_candidate = rc.measured.get(&g.key).is_some_and(|v| {
                        v.iter().any(|e| e.included && rule.candidates.contains(&e.frame.frame_id))
                    });
                    if !holds_candidate {
                        continue;
                    }
                }
```
  Replace `let group = best_group.ok_or_else(…)?;` with the following. The frame-set text is unchanged:
```rust
            let group = match best_group {
                Some(g) => g,
                None if rc.wcs_rule.is_some() => {
                    return Err(RunError::Other(NO_PROJECT_REFERENCE.to_string()))
                }
                None => return Err(RunError::Other("no group has any included frame".to_string())),
            };
```
  and `best_included_idx(entries)` in that arm becomes `best_candidate_idx(entries, rc.wcs_rule.as_ref())`. In `group_reference_idx`, `best_included_idx(entries)` becomes `best_candidate_idx(entries, rc.wcs_rule.as_ref())`. A native project group without a candidate then gets `None` and takes the run geometry, as today's `None` arm does.
  4. `two_pass_refine` (`~5168`): replace the `let Some(new_idx) = two_pass_pick(…) else { return Ok(None); };` statement with:
```rust
    // Plan 6b-2 C15: on a project run only a WCS candidate may win the
    // re-pick; a frame set calls today's pick, unchanged.
    let allowed: Option<HashSet<usize>> = rc.wcs_rule.as_ref().map(|rule| {
        rc.measured
            .get(&group.key)
            .map(|v| {
                v.iter()
                    .enumerate()
                    .filter(|(_, e)| rule.candidates.contains(&e.frame.frame_id))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    });
    let pick = match &allowed {
        None => two_pass_pick(&candidates, reference_idx, diagonal_px, TWO_PASS_MIN_GAIN_PX),
        Some(allowed) => two_pass_pick_among(
            &candidates,
            reference_idx,
            diagonal_px,
            TWO_PASS_MIN_GAIN_PX,
            &|idx| allowed.contains(&idx),
        ),
    };
    let Some(new_idx) = pick else {
        return Ok(None);
    };
```
  Add `two_pass_pick_among` to the `weights` import.

- [ ] **Step 6: Run the tests and watch them pass.** First the Step 2 command. Then:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::weights stacking::origin two_pass stage_reference reference_ subject_and_origin_variants_stay_in_their_funnels
```
  Expected: all pass, including the frame-set reference and two-pass tests, which run unchanged.

- [ ] **Step 7: The real-data probe** (`stacking/solve.rs` tests). Add to the test module:
```rust
    fn describe(o: &SolveOutcome) -> String {
        match o {
            SolveOutcome::Solved(r) => format!(
                "solved {:.3}\"/px at ({:.4}, {:.4}), {} matched",
                r.pixel_scale_arcsec, r.crval1, r.crval2, r.matched_stars
            ),
            SolveOutcome::Failed(e) => format!("failed: {e}"),
            SolveOutcome::Refused(e) => format!("refused: {e}"),
        }
    }

    /// One kind's counts in the probe: mono, or OSC by the manifest's `bayerpat`.
    #[derive(Default)]
    struct Tally {
        tried: usize,
        unit_ok: usize,
        adu_ok: usize,
    }

    /// Plan 6b-2 C14, "real data first": the real solver and the real catalog
    /// on the landed project replicas of a COPY of the `.dev` catalog. These
    /// files are calibrated to 0..1, which no production solve has seen. It
    /// reports per frame and in total how many solve on their prepared pixels,
    /// and for the rest whether an ADU-scaled (×65535) copy would.
    /// - The sample is stratified (review I8): up to `limit / 2` frames with a
    ///   manifest `bayerpat` (OSC, debayered by Prepare into a three-plane file)
    ///   and up to `limit / 2` without; the summary prints both sub-rates.
    /// - A probe that solves nothing in either domain fails (review M16).
    /// - It is read-only on the catalog copy and on every landed file; prepared
    ///   and scaled copies go to a temp dir.
    #[test]
    #[ignore = "needs ATHENAEUM_SOLVE_PROBE_DB (a copy of the .dev catalog) and ATHENAEUM_SOLVE_PROBE_APP_DATA (the .dev app-data dir, for its star catalog)"]
    fn real_data_solve_probe() {
        use crate::stacking::prepare::{execute_prepare, prepare_action, probe_file, PrepareAction, PrepareJob};
        let db = std::env::var("ATHENAEUM_SOLVE_PROBE_DB").expect("ATHENAEUM_SOLVE_PROBE_DB");
        let app_data = std::env::var("ATHENAEUM_SOLVE_PROBE_APP_DATA").expect("ATHENAEUM_SOLVE_PROBE_APP_DATA");
        let limit: usize = std::env::var("ATHENAEUM_SOLVE_PROBE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let config = crate::plate_solve::config::load_config(&conn);
        let layers = crate::plate_solve::open_catalog_layers(Path::new(&app_data)).expect("the .dev star catalog");
        let refs: Vec<&solvemyastro::StarCache> = layers.iter().map(|l| l.as_ref()).collect();
        let caches = solvemyastro::Caches::layered(&refs);
        let scratch = tempfile::tempdir().unwrap();
        let go = AtomicBool::new(false);
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT project_id, frame_uuid, landed_path, manifest_json FROM project_frames_local
                     WHERE landed_path IS NOT NULL ORDER BY project_id, frame_uuid",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let (mut osc_rows, mut mono_rows) = (Vec::new(), Vec::new());
        for (project_id, uuid, landed, manifest_json) in rows {
            match crate::collab::frame_meta::parse_project_frame_meta(&manifest_json) {
                Ok(meta) if meta.bayerpat.is_some() => osc_rows.push((project_id, uuid, landed, meta)),
                Ok(meta) => mono_rows.push((project_id, uuid, landed, meta)),
                Err(e) => println!("{uuid}: manifest unreadable: {e:#}"),
            }
        }
        let half = limit / 2;
        let sample: Vec<(bool, (String, String, String, ProjectFrameMeta))> = osc_rows
            .into_iter()
            .take(half)
            .map(|r| (true, r))
            .chain(mono_rows.into_iter().take(half).map(|r| (false, r)))
            .collect();
        let (mut mono, mut osc) = (Tally::default(), Tally::default());
        let (mut unsupported, mut absent, mut manifest_wcs) = (0usize, 0usize, 0usize);
        for (is_osc, (project_id, uuid, landed, meta)) in sample {
            let tally = if is_osc { &mut osc } else { &mut mono };
            if meta.wcs.is_some() {
                manifest_wcs += 1;
            }
            let target = crate::db::collab::get_project(&conn, &project_id)
                .ok()
                .flatten()
                .map(|p| (p.target_ra_deg, p.target_dec_deg));
            let landed = PathBuf::from(landed);
            if !landed.exists() {
                absent += 1;
                println!("{uuid}: not on disk at {} (is its volume mounted?)", landed.display());
                continue;
            }
            let shape = probe_file(&landed);
            let action = prepare_action(&shape, true);
            let path = match &action {
                PrepareAction::Unsupported(why) => {
                    unsupported += 1;
                    println!("{uuid}: unsupported file: {why}");
                    continue;
                }
                PrepareAction::InPlace => landed.clone(),
                PrepareAction::Write { .. } => {
                    let job = PrepareJob {
                        src: landed.clone(),
                        shape: shape.clone(),
                        action: action.clone(),
                        converted_out: scratch.path().join(format!("p_{uuid}.fits")),
                        debayered_out: scratch.path().join(format!("p_{uuid}_d.fits")),
                        durability: Durability::Volatile,
                    };
                    execute_prepare(&job, None, &go).expect("prepare").calibrated
                }
            };
            tally.tried += 1;
            // No stacking handle exists outside a run; 0 names the span.
            let request = SolveRequest::for_frame(0, &path, &meta, target);
            let unit = solve_with(&request, &caches, &config, &go);
            let unit_solved = matches!(unit, SolveOutcome::Solved(_));
            if unit_solved {
                tally.unit_ok += 1;
            }
            let adu_note = if unit_solved {
                String::new()
            } else {
                let adu = scratch.path().join(format!("adu_{uuid}.fits"));
                adu_scaled_copy(&path, &adu).expect("adu-scaled copy");
                let rescued = solve_with(&SolveRequest { path: adu, ..request.clone() }, &caches, &config, &go);
                if matches!(rescued, SolveOutcome::Solved(_)) {
                    tally.adu_ok += 1;
                }
                format!(" | adu-scaled: {}", describe(&rescued))
            };
            println!(
                "{uuid} ({}, {}): {}{adu_note}",
                if is_osc { "osc" } else { "mono" },
                if meta.wcs.is_some() { "manifest wcs" } else { "no manifest wcs" },
                describe(&unit)
            );
        }
        println!(
            "solve probe: mono {}/{} solved on unit-domain pixels, {} rescued by the adu-scaled copy; osc {}/{} solved on unit-domain pixels, {} rescued by the adu-scaled copy; unsupported {unsupported}, not on disk {absent}, with a manifest wcs {manifest_wcs}",
            mono.unit_ok, mono.tried, mono.adu_ok, osc.unit_ok, osc.tried, osc.adu_ok
        );
        assert!(mono.tried + osc.tried > 0, "no landed project frame in the catalog copy");
        // Review M16: a probe that solved nothing is a red run, not a line to read.
        assert!(
            mono.unit_ok + mono.adu_ok + osc.unit_ok + osc.adu_ok > 0,
            "no frame solved, in either domain"
        );
    }
```
  Make the copy and run it. The copy lives under the session scratchpad; never commit it. The catalog and the landed files are only read.
```bash
SCR=/private/tmp/claude-501/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/4c1ccbcf-d168-4afc-ac2c-170661456637/scratchpad
DEV="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev"
sqlite3 -readonly "$DEV/athenaeum.db" ".backup $SCR/solve-probe.db"
ATHENAEUM_SOLVE_PROBE_DB=$SCR/solve-probe.db ATHENAEUM_SOLVE_PROBE_APP_DATA="$DEV" ATHENAEUM_SOLVE_PROBE_LIMIT=20 \
  CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --ignored --nocapture real_data_solve_probe
rm -f "$SCR/solve-probe.db" "$SCR/solve-probe.db-wal" "$SCR/solve-probe.db-shm"
```
  Expected: PASS, ending with the `solve probe:` line, which carries the mono and the OSC sub-rates. Record that line and every per-frame line in the report, and in Task 11's open-items entry. The research found the `.dev` catalog's one project with frames, "LDN1272-test Project": mono `L` (ATR2600M) and OSC (ASI2600MC Duo) replicas, all 180 s, every one with a manifest WCS, landed under `/Volumes/bigbase3/Projects/ldn1272-test-project/vilen/`. The probe solves them anyway, because the point is whether calibrated 0..1 pixels solve; the OSC replicas go through the debayer first, so the OSC sub-rate is the first solve ever run on a debayered three-plane float file.
  - If that volume is not mounted, every frame reports "not on disk" and the test fails on its first assertion. Say so in the report and ask for the volume; do not copy the replicas elsewhere.
  - If the `.dev` catalog has no landed project frame, the test fails with "no landed project frame in the catalog copy". Say so in the report. Do not substitute the production catalog.
  - If `$DEV/catalogs/smac_gaia` is missing, say so. Do not download a catalog.

- [ ] **Step 8: Only if either sub-rate's `rescued by the adu-scaled copy` count, mono or OSC, is above 0** (some frames solve only in the ADU domain), add the fallback to `CatalogSolver` (Task 10b's code):
  - `SolveRequest` gains `pub scratch_dir: Option<PathBuf>`, set to `None` in `for_frame` (the tests that build a request through `for_frame` need no change).
  - `solve_project_frames` sets `request.scratch_dir = Some(rc.layout.root.join("tmp"))` after `for_frame`.
  - `CatalogSolver::solve` ends with:
```rust
        let first = solve_with(request, &caches, &opened.config, cancel);
        let retry_dir = match (&first, &request.scratch_dir) {
            (SolveOutcome::Failed(_), Some(dir)) => Some(dir.clone()),
            _ => None,
        };
        let Some(dir) = retry_dir else {
            return first;
        };
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return first;
        }
        let stem = request.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let copy = dir.join(format!("adu_{stem}.fits"));
        if let Err(e) = adu_scaled_copy(&request.path, &copy) {
            tracing::warn!(path = %request.path.display(), error = %format!("{e:#}"), "adu-scaled solve copy failed; keeping the first outcome");
            return first;
        }
        let second = solve_with(&SolveRequest { path: copy.clone(), ..request.clone() }, &caches, &opened.config, cancel);
        if let Err(e) = std::fs::remove_file(&copy) {
            tracing::warn!(path = %copy.display(), error = %e, "removing the adu-scaled solve copy failed");
        }
        tracing::debug!(path = %request.path.display(), outcome = second.label(), "retried the solve on an adu-scaled copy");
        second
```
  - Re-run the probe after the change and record both `solve probe:` lines.
  - Without the condition, skip this step and record "unit-domain solving holds on real data, mono and OSC" in the report.

- [ ] **Step 9: Run the gate.** TS must not move: `git diff --exit-code src/types`.

- [ ] **Step 10: Commit** with the message `feat(stacking): the WCS reference rule; the real-data solve probe`, plus the trailers. If Step 8 ran, its fallback is part of this commit, and the message adds `; the adu-scaled solve fallback`.

---

### Task 11: Docs and the final gates (C16 orientation, C17)

**Files:**
- Modify `docs/stacking/README.md`: the 6b-1 block (three sentences), a new block "The project path (wave 6b-2)", and the key files.
- Modify `docs/transfers/README.md`: a new section at the end.
- Modify `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`: one dictionary paragraph after the wave 6a one (`~279`).
- Modify `docs/superpowers/open-items.md`: one entry at the top of "Unverified by hand", and one line under "Release notes owed at the next tag".

`CLAUDE.md` is not touched; its command count moves with 6b-3's docs (C17). No code changes in this task.

- [ ] **Step 1: The stacking reference** (`docs/stacking/README.md`).
  1. In the 6b-1 block, make three replacements:
     - `` `StackingPlan.setId`, filled by `run::wire_set_id`) are unchanged.`` becomes `` `StackingPlan.setId`, filled by `run::wire_set_id`) are unchanged (6b-2 changed them; see below).``
     - ``has one variant, `Catalog { file_id }`;`` becomes ``had one variant in 6b-1, `Catalog { file_id }` (6b-2's two are below);``
     - `` `<working>/projects/<set_slug(slug)>` for a project).`` becomes `` `<working>/_projects/<set_slug(slug)>` for a project; 6b-1 wrote `projects/`).``
  2. Replace the whole bullet that starts `- **Next (plan 6b-2, the project path).**`, through `take `frames_set_id` and wrap it into a subject).`, with:
```markdown
- **Resolved by 6b-2.**
  - (1) The namespace overlap: a project lives under `<working>/_projects/`, and no frame-set slug can start with `_`.
  - (3) The funnels: a grep test now enforces them, and `last_run_reference_scale` / `cleanup_stale_note` take `&StackSubject`.
  - (2), the project's `RegistrationRecord` shape, belongs to registration onto the grid (6b-3).
```
  3. Insert, between the 6b-1 block and `**Key files**`:
```markdown
**The project path (wave 6b-2, 2026-10-03)** (plan
`2026-10-03-collab-v3-wave6b2-project-path-plan.md`, spec
`2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §18 steps 3-7 and 13).
A collaboration project is a stacking subject up to, but not including,
registration onto its canonical grid. `start_stacking` still refuses a project
(`PROJECT_STACKING_UNAVAILABLE`). Project stages run only through
`run::project_test_context` in tests until 6b-3.

- **The wire is subject-aware.** The two events, `RunSummary`, `StackingRunRow`
  and `StackingPlan` carry `setId | projectId`, exactly one set
  (`run::wire_subject`). `RunSummary.projectId` is `#[serde(default)]`.
  `useStackingRuns` drops an event without a `setId` until 6c.
- **Four config fields.**
  - `grouping.exposureMode` (`seconds | stops | equivalent | none`, default `seconds`).
  - `grouping.stopsWidth` (default 1, clamped to `MIN_STOPS_WIDTH` = 0.1).
  - `output.coverageMap` and `output.cropToCoverage`.

  No stage hash moved: the golden literals hold with non-default values.
  `config_hash` was re-pinned once. A project's first plan seeds its stored
  document (`stacking_project_config`) with `exposureMode = stops`.
- **Exposure classes** (`groups::classify_exposures`, pure, shared by both frame sources).
  - `seconds` is the old clustering, byte for byte.
  - `stops` and `equivalent` anchor at the shortest remaining exposure and take
    everything within ×2^W. `equivalent` works on t/N², N = `meta.focalLen` /
    `meta.aperture`; without both it falls back to seconds, with a warning.
  - `none` is one class, `all`.
  - Frames without EXPTIME keep their own `unknown` class in every mode.
  - Tokens are `<t>s`, `st<num>`, `eq<num>`, `all` and `unknown`. The number has
    four significant digits, so two anchors (at least ×1.07 apart at the
    `stopsWidth` floor) never print alike.
  - `stacking_run_groups.exposure_class` holds the token; `exposure` stays seconds.
- **`FrameOrigin`** is `Catalog { file_id, attested }` or `Project { project_id,
  frame_uuid, publisher, landed_path, blake3, byte_size, size_mtime, meta }`.
  `meta` is the manifest's `collab::frame_meta::ProjectFrameMeta`, parsed once;
  `meta.wcs` becomes a `PlateSolveRecord` with crpix − 1. This is not
  `db::collab_frames::FrameOrigin` (`own | replica`).
- **The project frame source** (`stacking::project_source`) reads
  `project_frames_local` and the landed files' headers, never the catalog.
  - A frame is stackable when it is exportable, servable, not `own_staged`, and
    has a recorded `size_mtime_seen` that parses (one that does not is
    "unreadable size:mtime stamp", never "waiting"). Every other published,
    accepted frame is a Frames row with its reason.
  - Groups are colour × canonical filter × class, keyed
    `<mono|osc|cfa>__<filter>__<class>`, with no binning and no camera. `cfa`
    appears only when `debayerOsc` is off and the file is a one-plane mosaic.
  - Handles are minted in one IMMEDIATE transaction.
- **Funnels.** `origin::tests::subject_and_origin_variants_stay_in_their_funnels`
  keeps every `StackSubject::` / `FrameOrigin::` path in the production half of
  `run.rs` and `plan.rs` inside the named funnels, or in an allowlist where each
  entry has its reason. The per-frame dispatch points the run calls live in
  `stacking::origin`, which the test does not scan: `stored_solve`,
  `cfa_geometry`, `landed_unchanged`, `needs_prepare`, `prepare_input`,
  `solve_plan`, `wcs_tier` and `local_solve_outcome`. A third subject or origin
  touches exactly these and the named funnels.
- **The identity check.** `origin::landed_unchanged` compares a landed file with
  its row's `size:mtime` (2 s tolerance). It runs at Prepare, Solve, Measure,
  the Register candidates and the Output member snapshot. A mismatch excludes the
  frame "changed during the run"; a changed reference fails the run.
- **The plan** (`build_plan`'s project arm).
  - It seeds the config and mints handles: the documented exception to "the plan
    reads only". Readiness is empty and there are no masters.
  - Blockers, in order: `grid`, `reference`, `folders`, `space`, `frames`,
    `unsupported`. `folders` also fires when the project's working root
    (`<working>/_projects/<slug>`) overlaps any scan root; the Collaboration root
    is one.
  - Warnings (spec §7): frames not on this device, other reasons a frame is left
    out, frames to be solved locally or that could not be, frames outside the
    grid, unsupported files, and unknown exposure equivalence.
  - Unsupported files also leave every group's included count and the ≥ 3
    rule, so a group Prepare would leave short is blocked by `frames` at plan
    time; they are not manual exclusions.
  - It carries `grid` (state, current, proposal, framesInside, canEdit) and
    `projectFrames`.
- **R3 (attested sets).** A frame set with `calibrated_externally` needs no
  masters: no masters / links / masterFiles gate and nothing to build. Its lights
  are prepared, never calibrated; the stage-1 hash is `prepare_hash_for_catalog`.
- **Prepare** is stage 1 for a project and for an attested set. It reports under
  `Stage::Calibrate`, the stage-1 slot.
  - **In place:** a float FITS (BITPIX −32/−64) that needs no debayer is used as it
    is. The `calibrated` artifact is the landed file, which no cleanup removes.
  - **Converted:** an integer FITS (÷ `scale_divisor_for_bitpix`) or an XISF
    (`read_raw` ÷ 65535) becomes float32 FITS `prepared/<group>/p_<stem>.fits`.
    An integer FITS is never used in place, although spec §5.1 row 1 allows
    it: converting gives an attested set's 16-bit lights and a published
    replica the calibrated-light generator's one unit domain.
  - **Debayered:** a CFA mosaic, when `debayerOsc` is on, goes through the
    generator's own VNG (`debayer_plane`) into `p_<stem>_d.fits`. The
    pre-debayer mosaic is the `calibrated_mosaic` for Bayer drizzle.
  - **Excluded:** a tile-compressed FITS, an extension image, a distributed
    XISF or a file that cannot be opened, as "unsupported file: …" (the
    decoder's reason reads "not a 1- or 3-plane 8/16/32-bit or float primary
    image"); and a frame whose plane count after Prepare contradicts its
    group's colour, as "plane count {n} disagrees with the group's colour
    mode" (the group's colour comes from the manifest, the debayer from the
    file's own header).
  - Prepared files carry the copy-through cards, `ATH_STK`, `ATH_STKV` and
    `ATH_PREP`, and never `CALSTAT`, `ATH_CSRC`, `ATH_PRJ` or `ATH_FILT`, so the
    scanner never catalogs one.
  - The hash is `prepare_subtree` (`debayerOsc`, `debayerMethod: "VNG"`,
    `prepareVersion`) over `blake3` and byte size (a project) or the catalog
    identity (an attested light).
  - `prepared/` counts as calibrated bytes and goes with `Intermediates`.
- **Solve** is the first thing Measure does. A project frame without `meta.wcs`
  is solved by `rc.solver`:
  - `stacking::solve::FrameSolver`; `CatalogSolver` opens the catalog on first use
    through `plate_solve::open_catalog_layers`, the opener both hosts now call.
  - It solves the prepared file, with the project target and the manifest scale
    as hints, through `solve_frame_with_hints` and the extracted gate
    `acceptance_refusal` at the manifest scale.
  - Every outcome (solved, failed, refused) is a `solve` artifact: group key
    `""`, payload `{blake3, app_version, outcome}`, hash over blake3, the
    solver config hash and the catalog identity. A cached failure or refusal
    written by another app version is tried once more; a cached solve stays.
    Bump `plate_solve::config::SOLVE_RECIPE_VERSION` when a solver change
    should re-solve frames cached as solved.
  - `solve` is not an intermediate: `CleanupWhat::Intermediates` keeps it, and
    only `CleanupWhat::All` (every artifact row) deletes the solve cache.
  - `plate_solves` and `frames` are never written. `stored_solve`'s project arm
    reads the manifest WCS, then the run's solve of this content.
  - For 6c: the sub-step's own progress runs `Measure` to 100 % with the
    message `"solving"`, and Measure then restarts at 0, so a project run's
    Measure bar goes back once.
  - Real-data probe on the `.dev` replicas (mono and OSC sub-rates): <the `solve probe:` line from Task 10c Step 7>.
- **The reference rule** (spec §5.4). On a project run the reference must carry a WCS.
  - Candidates are the manifest-WCS frames, or the locally solved ones only when
    no included frame has a manifest WCS.
  - Auto takes the largest group that holds a candidate. A native group without
    one takes the run reference.
  - The two-pass re-pick never lets a non-candidate win (`two_pass_pick_among`).
  - A manual pin without a WCS fails the stage; a pin that stage 1 excluded
    still fails with its own reason ("the manual reference frame is excluded: …").
  - With no candidate, the stage fails with "no frame of this project could be
    plate-solved; the grid needs one".
- **The canonical grid** is described in `docs/transfers/README.md` (wave 6b-2).
  **Orientation:** the grid is sky-standard (north up, east left, det(CD) < 0).
  Every solve in the owner's catalog has det(CD) > 0, so a master on the grid is
  the mirror image of a raw frame, which is correct on the sky. 6b-3 must not
  copy the reference's `ROWORDER` onto a grid master.
- **Open for 6b-3:**
  - registration onto the grid through one composed map (with 6b-1's
    `RegistrationRecord` question);
  - LN on overlap, then coverage and crop;
  - drizzle and output on the grid;
  - the seven project commands (command count 299 → 306);
  - the fake-hub e2e and acceptance runs A and B.

  `start_stacking` lifts its project guard then.
```
  Replace the `<…>` placeholder in the Solve bullet with the actual line Task 10c Step 7 printed. If the probe could not run, write "not run: <why>".
  4. In `**Key files**`, add `stacking/{origin,project_source,prepare,solve}.rs`, `collab/{grid,grid_geometry,frame_meta}.rs` and `api/collab_grid.rs` to the core list, and `plate_solve/{service,layers}.rs` after them.

- [ ] **Step 2: The transfers reference** (`docs/transfers/README.md`). Append:
```markdown
## Collab wave 6b-2 — the canonical grid (2026-10-03)

Spec `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §6, §9.1 and §9.2; plan `docs/superpowers/plans/2026-10-03-collab-v3-wave6b2-project-path-plan.md` (C16, C17). The stacking side of the project path is in `docs/stacking/README.md` (The project path, wave 6b-2).

- **One shape, one hash.** `collab::grid::CanonicalGrid { scaleArcsec, widthPx, heightPx, crval1, crval2, rotationDeg }` is ungated.
  - `validate` mirrors the hub's own check, with the hub's text: scale in (0, 100], sides in [64, 65536], crval1 in [0, 360), crval2 in [−90, 90], a finite rotation.
  - `normalized` turns −0.0 into 0.0, the rotation into (−180, 180] and crval1 into [0, 360).
  - The hash is XXH3-64 of the typed struct's canonical JSON (sorted keys), as 16 hex digits. It is never taken over the hub's jsonb echo, so reordered keys, `2048.0` for `2048`, or `-0.0` all hash alike.
- **Hub and cache.**
  - `CollabClient::put_grid` sends `PUT /projects/{id}/grid` (204).
  - `ProjectWire.canonical_grid` carries the hub's copy. The refresh writes `collab_projects.canonical_grid_json` through `db::collab::set_canonical_grid` and reports the projects whose grid moved in `RefreshReport.grid_moved`. The setter compares with `IS NOT`, so an unchanged echo writes nothing.
  - `ProjectCard.canonicalGrid` exposes the cached grid. A cached grid that no longer parses shows as none on the card, with a warning; the proposal command fails on it instead.
  - A malformed echo keeps the cached grid, with a warning.
  - `set_collab_project_grid` writes the local copy only after the hub's 2xx.
- **Feed and event.** `ChangeKind::Grid` now triggers the project refresh. Each of the six live-feed refresh sites emits `collab-grid-changed { projectId }` for every project whose grid moved (`FeedApplier::emit_grid_moves`). The saving device gets the event from the command, and every other device from the feed: the saving device's own cache write makes its feed refresh a no-op. The hosts' `refresh_projects` command and the device-replace refresh emit nothing; their cards carry the grid.
- **Proposal** (`collab::grid_geometry::propose_grid`). It is deterministic for a manifest version.
  - Scale: the median `pixelScaleArcsec` of the accepted frames, rounded to 0.01″. Centre: the project target. Rotation: 0.
  - Size: the bounding box, in the proposed TAN, of the WCS frames' footprints (SIP applied), clipped to the target circle, rounded up to a multiple of 64 and clamped to [64, 65536]. With no WCS frame, a square of 2 × the target radius.
  - `framesInside` counts the frames at least half inside (a 16 × 16 sample).
  - With no accepted frame carrying a scale there is no proposal, and none either when the median scale rounds outside the hub's (0, 100].
- **Two commands, on both hosts** (strict count 297 → 299):
  - `get_collab_grid_proposal { projectId } -> { current, proposal, framesInside, canEdit }`, DB-only.
  - `set_collab_project_grid { projectId, grid } -> grid`. It validates and normalizes the grid, and refuses a caller without `project.edit` with "This account cannot edit the project's grid (it needs project.edit and membership)." It then PUTs, writes the cache, emits `collab-grid-changed` (on every successful save) and echoes its own normalized grid. A hub 400 is `Invalid`, carrying the hub client's wrapped message (`hub returned 400 Bad Request (put project grid): …`); local validation mirrors the hub's text, so that wrapper shows only when the two disagree.

  Plan 6b-3 adds the seven project-stacking commands (→ 306).
- **Fake hub:** `PUT /projects/{id}/grid`, `canonicalGrid` on the project page, and a settable target.
- **Orientation:** the grid is sky-standard (north up, east left, det CD < 0). See the stacking reference for what that means for masters.
```

- [ ] **Step 3: The logging dictionary** (`2026-07-03-logging-overhaul-design.md`). Insert after the wave 6a paragraph:
```markdown
**Dictionary extension (collab v3 wave 6b-2, `stacking::{project_source, origin, prepare, run, solve, plan}` + `collab::grid` + `api::{collab, collab_grid, collab_live::feed}` + `plate_solve::layers`, 2026-10-03):** the names the project path logs.

- **`not_stackable`** (usize): published, accepted project frames the frame source leaves out on this device. It appears beside `count` (the stackable ones) and `groups` on the debug `"project frame source built"`.
- **`prepared`** (`"in_place"` | `"converted"` | `"debayered"`): what Prepare did with one frame, on the debug `"frame prepared"`. It appears beside `run_id`, `frame_id`, `group_key`, and `frame_uuid` for a project frame.
- **`solved`** (usize): frames the Solve sub-step solved in this run, beside `count` (the frames it tried), on the info `"project frames solved"`.
- **`grid_hash`** (16-hex XXH3 of a canonical grid's canonical JSON) on the info `"grid saved"`.
- **`frames_inside`** (usize): frames at least half inside a proposed grid, on the debug `"grid proposed"`.
- **`bayerpat`** (string): a CFA pattern a file's own header declared that no reader recognizes, on the warn `"unrecognized bayer pattern in the file's header; treated as no mosaic"`.
- **`stamp`** (debug-formatted `Option<String>`): a landed frame's recorded `size:mtime` stamp that does not parse, on the warn `"landed stamp unreadable; frame left out"`.

Reused in their existing senses:
- `project_id`, `run_id`, `group_key`, `count`, `groups`, `path`, `error`, `duration_ms` and `config_hash`;
- `frame_uuid`;
- `frame_id`: a stacking frame id, which is a catalog `frames.id`, or a `stack_project_frames` handle on a project run (also on the solver's `"solve"` span for a project frame; the real-data probe, which runs outside a run, records 0);
- `reason`: an exclusion reason, `"changed during the run"` among them;
- `outcome`: a solve's `solved` / `failed` / `refused`, or the stat verdict on `"landed project frame changed during the run"`;
- `set_id`: stacking's own sense, a run's frame set. It is an `Option` now, recorded exactly as before for a frame set and absent on a project run.
```

- [ ] **Step 4: Open items** (`docs/superpowers/open-items.md`). Insert at the top of "## Unverified by hand", before "### Collab wave 6a — owner smoke (2026-10-02)":
```markdown
### Collab wave 6b-2 — the project path up to the geometry (2026-10-03)

Code-complete with green gates:
- the subject-aware wire and the four config fields;
- exposure classes;
- the project frame source and plan;
- Prepare, and Solve with the WCS reference rule;
- the canonical grid end to end (two commands);
- R3's stacking half.

Spec `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §5–§9 and §13; plan `docs/superpowers/plans/2026-10-03-collab-v3-wave6b2-project-path-plan.md`. Reference: `docs/stacking/README.md` (The project path, wave 6b-2) and `docs/transfers/README.md` (wave 6b-2). No project run starts until 6b-3, so there is no project UI to smoke. Owed:

- **Unit-domain solving on real data, mono and OSC.** Task 10c's probe on the `.dev` LDN1272-test replicas printed: <the `solve probe:` line, with both sub-rates>. If either sub-rate shows rescues by the ADU-scaled copy, the fallback (Task 10c Step 8) landed with it; re-check both sub-rates on the first real project run.
- **An attested set end to end on real data.** Mark a real externally calibrated set attested and stack it. Check that it runs without masters, that integer lights are converted under `prepared/`, and that the master looks right.
- **Unsupported files are excluded, not read.** A tile-compressed FITS, an image in an extension HDU and a distributed XISF leave a project run as "unsupported file", and the plan names them and counts them out of the ≥ 3 rule. Nothing in the owner's data needs them today. A reader for any of them is its own cycle.
- **A float file in the ADU domain is used in place as if it were 0..1.** Prepare does not look at pixel values, so a float FITS another tool wrote in ADU (0..65535) would be measured and stacked in ADU. Every replica a member publishes is 0..1 by construction; an attested set's float lights are the open case. Check the first real attested float set. If it is in ADU, add a value-range probe to `prepare_action`, in its own cycle.
- **The 25 % LN-overlap threshold cannot be re-measured on the acceptance set (6b-3).** LDN1272-test's two rigs are two groups whose members share their framing, so nothing falls under 25 %. The fallback is reachable only in synthetic fixtures and the fake-hub e2e. The owner's option is to publish set 195 "Ghost Nebula QHY" (~60 % overlap) into a test-hub project; that exercises partial overlap, not the < 25 % fallback.
- **The grid's orientation (6b-3).** A master on the grid will be the mirror image of a raw frame: the grid is sky-standard (det CD < 0), while every solve in the owner's catalog has det CD > 0. That is correct on the sky. Check the first grid master against a sky chart, and never copy `ROWORDER` onto it.
```
  Replace the `<…>` with the probe's line, or "not run: <why>".

  Under "## Release notes owed at the next tag", add:
```markdown
- **Externally calibrated frame sets stack without masters.** A frame set marked as calibrated elsewhere is now stacked as it is: its lights are used, or converted to the app's unit domain, instead of being calibrated, and no master is asked for or built.
```

- [ ] **Step 5: The final gates.** Run, one at a time, from the repo root:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --lib 2>&1 | grep -E '^warning|^error' ; echo "lib check done"
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-tauri
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract && git diff --stat src/types
npx tsc --noEmit -p .
npx vitest run src/hooks/useStackingRuns.test.tsx src/hooks/useCollabNotifications.test.tsx src/components/collab
grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l
git diff --stat e03bc48f -- src/types
```
  Expected:
  - every `test result` is `ok`, and the counts are Task 0's plus this plan's new tests, with no test lost (one `#[ignore]` more: the real-data probe);
  - the lib check prints no `warning`, and the headless check is clean;
  - `git diff --stat src/types` is empty: the TS was committed with its tasks;
  - the command count is `299`;
  - the last command lists only `src/types/stacking.ts` and `src/types/models.ts`.

  A known flake (the two wall-clock collab live tests, `landing_free_bytes_is_bounded_by_the_collaboration_root`) is re-run alone, and both runs are reported.

- [ ] **Step 6: Commit** with the message `docs: the project path (wave 6b-2) — stacking reference, the grid, the logging dictionary, open items`, plus the trailers.

---

## Writer's notes for the controller

Each item is a place where the skeleton and the code disagreed, or where the skeleton left a choice open. Each gives the fix this plan made. All of them are also marked **Refinement** in the rulings where they apply. Items 5, 9 and 20 were updated, and items 22 and 23 added, by the controller's rulings on the plan review.

1. **TS moves in five tasks, not three.**
   - Problem: the skeleton allows `src/types` diffs only in Tasks 1, 3 and 8. But Task 2's four config fields live in ts-exported `GroupingConfig`/`OutputConfig`/`ExposureMode`, and Task 7's `PlanGrid`/`GridState`/`PlanProjectFrame` and the two `StackingPlan` fields are exported.
   - Fix: the gate allows Tasks 1, 2, 3, 7 and 8b, and each lists its exact TS moves. (Task 8a adds `CanonicalGrid` unregistered, so its TS does not move.)
2. **The identity check's call sites are split.** Measure, Register and Output land in Task 6b with the funnel. Prepare and Solve land with their stages (Tasks 9b and 10b).
3. **`prepare_subtree` lands in Task 6a, the classifier in Task 7.**
   - `HashMemo`'s project arm must compile in Task 6a, and the plan's space estimate and its unsupported-file warning need `probe_file`/`prepare_action` in Task 7.
   - The writers and the debayer extraction are Task 9a; the stage is Task 9b.
   - The subtree key is the skeleton's `debayerMethod`.
4. **The stackable predicate also needs a recorded `size_mtime_seen`.** A version bump clears the stamp, so the landed file is not the manifest's version, and the identity check would have no baseline. The reason is "waiting for its new version".
5. **Who emits `collab-grid-changed`.** The six live-feed refresh sites, and the save command (review I5, controller ruling).
   - The saving device's own cache write makes its feed refresh a no-op, so `set_collab_project_grid` takes the host's emitter and emits after the successful cache write, on every successful save (a re-plan is idempotent). The saving device gets the event from the command; every other device gets it from the feed.
   - The hosts' `refresh_projects` command and the device-replace refresh still hold no emitter; the cards they return carry `canonicalGrid`.
6. **`none` keeps `unknown` apart.** Frames without EXPTIME keep their own `unknown` class in every mode, so `none` means one class per colour × filter among frames with a known exposure. This keeps the EXPTIME warning byte-identical.
7. **The token collision proof rests on two choices this plan made:**
   - `MIN_STOPS_WIDTH` = 0.1 (the skeleton said "a positive floor");
   - four significant digits.

   Two anchors of one bucket then differ by more than ×1.07, while printing moves a value by at most 0.05 %.
8. **C10's "one IMMEDIATE transaction" is two atomic writes.**
   - The config seed and the handle mint are separate IMMEDIATE transactions, because `start_stacking` (6b-3) mints through the same frame source.
   - Both are idempotent, so a crash between them is harmless.
9. **There is no project-only `unsupported` blocker, but unsupported files leave the plan's counts** (corrected by review I1).
   - Unsupported FILES are a plan warning plus a per-frame Prepare exclusion. The `unsupported` code stays the shared drizzle-scale check; a blocker would stop a project over one odd file that a member can simply re-publish.
   - They are also plan-time exclusions: the shared core counts their handles out of every group's included frames and the ≥ 3 rule (`excluded_set ∪ unsupported_ids`), so a group that Prepare would leave under three frames is blocked by `frames` at plan time instead of failing at run time. `StackingPlan.excluded_frame_ids` stays the manual list.
10. **Reference-rule details the spec left open:**
    - tiers are decided run-wide over included frames;
    - Auto takes the largest group that holds a candidate;
    - a native group without one takes the run reference;
    - the two-pass re-pick filters through `two_pass_pick_among` (the frame-set path still calls `two_pass_pick`);
    - a manual pin needs any WCS, manifest or local.
11. **A changed reference fails the run** ("the reference frame changed during the run"), because a reference cannot be excluded from its own group.
12. **The proposal is optional.** `GridProposal.proposal` / `PlanGrid.proposal` are `Option`: with no accepted frame carrying a scale there is no proposal, and the plan's grid state is `unset`.
13. **The grep test's allowlist.**
    - It also carries `build_plan` (the subject dispatch to the two plan arms and its per-subject debug line) and the two `#[cfg(test)]` constructors.
    - The test lands in Task 6a without `landed_unchanged` in `FUNNELS`; Task 6b adds it with the function.
    - The skeleton's "registration store" funnels are `stored_registrations` / `store_registration`, and "the `stage_prepare` dispatch" is the funnel `stage_one_kind`.
14. **`publisher` in `FrameOrigin::Project` is the display name** (`publisher_display`), as the Frames table shows it.
15. **`WorkUsage` does not move.**
    - Prepare's copies are counted in `calibrated_bytes`, stage 1's slot, so no TS regenerates in Task 9a.
    - The research assumed a new `prepared_bytes` field.
16. **The working root is `_projects`, per the skeleton.** Spec §9.3 still says `projects/` and is stale on this point.
17. **Prepared file names keep the replica's stem:** `p_c_<uuid>.fits`, and `p_c_<uuid>_d.fits` when debayered (through `calibrated_file_stem`).
18. **The `solve` payload carries the content's `blake3`.**
    - This is beyond the skeleton: `stored_solve` then never serves a solve of an older version of the frame, even when a run could not refresh it (no catalog).
    - The plan's "could not be plate-solved" warning reads that last outcome without its hash.
19. **No new `Stage` variant.** Prepare reports under `Stage::Calibrate` and Solve under `Stage::Measure` (message `"solving"`), so the board keeps its ten rows. Plan 6c names the Calibrate row "Prepare" for a project.
20. **R3 is live for frame sets in 6b-2.** An attested frame set now starts and stacks with no masters through `start_stacking`, because only the project guard stays (Task 9b's end-to-end test). This is user-visible before 6b-3. Controller ruling: kept; Task 11 adds a "release-note line owed" entry to `open-items.md` (externally calibrated frame sets now stack without masters), and the real-data smoke entry stays.
21. **Task 10c Step 8 is conditional on real data.** The ADU-scaled solve fallback is added only if the probe shows frames, mono or OSC, that solve only in the ADU domain. The probe samples both kinds (review I8) and needs the `.dev` replicas' volume (`/Volumes/bigbase3`) mounted.
22. **Integer FITS are converted, not used in place** (review I2). Spec §5.1 row 1 lists BITPIX 8/16/32 among the files used in place; this plan narrows it to float, so an attested set's 16-bit lights and a published replica share the calibrated-light generator's unit domain and every stage after stage 1 sees one value domain. Recorded in C12 and in the README's Prepare bullet.
23. **Three placements differ from the review's cut points, to keep each half's lib check warning-free.**
    - `needs_prepare`, `PrepareInput` and `prepare_input` land in Task 9b, not 9a: nothing calls them before the stage, so in 9a they would be dead code.
    - `WcsRule`, the `wcs_rule` field and `WcsTier`/`wcs_tier` land in Task 10c, not 10b, for the same reason; 10c adds `wcs_rule: None` to both constructors. Task 10b's no-op test drops its `wcs_tiers` assertion, and Task 10c's `a_frame_set_run_has_no_wcs_rule` carries it.
    - `ProjectFile::EmptyPrimary` moves to Task 7, which needs it for the review I1 plan test; Task 9b uses it.
