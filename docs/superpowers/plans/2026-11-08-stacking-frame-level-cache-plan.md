# Stacking frame-level cache — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: the project's `lean-execution` skill (it overrides
> superpowers:subagent-driven-development where they conflict), or superpowers:executing-plans. Steps use checkbox
> (`- [ ]`) syntax for tracking.

**Goal:** re-stacking the same frames under another grouping, another weighting or after "Delete intermediates"
reuses every result that does not depend on that change; `measurement.maxStars` defaults to 9 000 for new documents.

**Architecture:** frame-intrinsic artifacts (`calibrated`, `calibrated_mosaic`, `prepared`, `measured`, `metrics`,
`fits.<plane>`, `registered`) move to `group_key = ""` and to flat folders, with a frame-only file stem; two stage
hashes stop folding fields that do not change their result (the grouping tolerance, the weight settings); OSC
registration stops folding the measurement hash; pass 1 survives a byte-identical recalibration and "Delete
intermediates"; the two-pass dry pass caches its decision. One migration re-keys existing rows without moving files.

**Tech Stack:** Rust (`athenaeum-core`), SQLite (`rusqlite`); one frontend removal (Task 13); no new command.

**Spec:** `docs/superpowers/specs/2026-11-08-stacking-frame-level-cache-design.md` (items 1–8 = spec §5.1–§5.8).

## Global Constraints

- No pixel and no number moves in Tasks 1–10: `MEASURE_VERSION` 8, `ALIGN_VERSION`, `PASS1_ARTIFACT_VERSION`,
  `FITS_ARTIFACT_VERSION`, `LN_MODEL_VERSION` unchanged; `stage_hashes_are_pinned_to_golden_literals` and the
  `plan.rs` goldens unchanged until Task 11, which re-records only the pins `maxStars` moves.
- No Tauri command or Axum route changes (two-backend rule untouched); no `ts_export` type changes.
- `tracing` only; messages are short stable phrases; reuse existing field names (`run_id`, `group_key`, `frame_id`,
  `count`, `path`, `error`); any new field name goes into `docs/logging/README.md`'s dictionary in the same task.
- Never swallow an error: every `Err` is logged before it is returned or turned into a fallback.
- Test names use letters, digits, `_` and `-` only. Synthetic fixtures for unit tests; the real-data smoke is Task 12.
- Disk rule: test runs, catalog copies and smoke folders live under `~/.athenaeum-acc/` with a `REGISTRY.tsv` row;
  agents build with `CARGO_TARGET_DIR=target-agent`. One implementer at a time (16 GB dev Mac).
- No other code base is named anywhere (code, comments, commits, docs); the comparison program is "the ERP".
- Commits are the owner's identity with the harness's attribution trailers.

## Review Focus

1. **A merged group whose β differs from both source groups:** pass 1 is reused, pass 2 reruns, OSC registration is
   reused, mono registration reruns. Pinned in Task 4 (`merged_group_with_a_new_beta_reruns_pass_two_only`).
2. **A row whose stored path still points into an old `<group_key>/` folder:** every reader opens the row's path;
   nothing recomputes it from the layout. Pinned in Task 5 (`a_migrated_row_is_read_from_its_stored_path`).
3. **A frame re-written under a new stem:** the old file is removed only when no other row points to it. Pinned in
   Task 2 (`rewriting_a_frame_removes_its_old_file_once_unreferenced`).
4. **A collab project subject** (`StackSubject::Project`, `project_frame`) takes the same frame-level keys. Pinned in
   Task 2 (`a_project_frame_uses_the_frame_level_key`).
5. **"Delete intermediates" with frame-level rows under `''`:** `calibrated/` goes, `measured/` and `fits/` stay with
   their referenced files, unreferenced files there go. Pinned in Task 7
   (`intermediates_sweep_keeps_referenced_pass1_files`).

---

## Summary

| # | Task | Spec | Size |
| ---- | ---- | ---- | ---- |
| 1 | Calibration hash ignores the grouping tolerance | §5.2 | S |
| 2 | Frame-level key helper, flat layout, frame-only stem; Calibrate / Prepare on it | §5.1 | L |
| 3 | Weight settings leave the measurement hash | §5.4 | S |
| 4 | Measure kinds (`measured`, `metrics`, `fits.<plane>`) on the frame key | §5.1 | L |
| 5 | `registered` on the frame key; every reader opens the row's path | §5.1, §5.7 | M |
| 6 | OSC registration hash without the measurement hash | §5.5 | S |
| 7 | Pass 1 survives "Delete intermediates"; unreferenced sweep for `measured/`, `fits/` | §5.3, §5.7 | M |
| 8 | Pass 1 valid on a byte-identical recalibration (size-only stamp), gated by an identity pin | §5.3 | M |
| 9 | Migration: re-key frame-level rows to `''`, keep the newest | §5.7 | M |
| 10 | Two-pass dry-pass decision cache; progress message per pass | §5.6 | M |
| 11 | `maxStars` default 9 000 | §5.8 | S |
| 12 | Regroup acceptance test, docs, real-data smoke | §7 | M |

Order is the table's. Tasks 1, 3, 6 and 11 are independent of the rest; 4 and 5 need 2; 7 needs 4; 8 needs 7;
9 needs 2, 4, 5; 12 needs all.

---

### Task 1: Calibration hash ignores the grouping tolerance

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/config.rs:977-1000` (`calibration_subtree`)
- Test: `config.rs` `mod tests`

**Interfaces:** Produces `pub const CALIBRATION_HASH_GROUPING_TOLERANCE_SEC: f64 = 2.0;` in `config.rs`.

- [ ] **Step 1: failing test** `calibration_hash_ignores_the_grouping_tolerance`: two configs, default and
  `grouping.exposure_tolerance_sec = 200.0`, plus one with `exposure_mode = Stops`; assert
  `stage_hash(&calibration_subtree(a), &["m"], &[])` is equal for all three, and equals the hash of the default.
- [ ] **Step 2:** `cargo test -p athenaeum-core --lib calibration_hash_ignores_the_grouping_tolerance` → FAIL.
- [ ] **Step 3:** `calibration_subtree` writes `"grouping": {"exposureToleranceSec": CALIBRATION_HASH_GROUPING_TOLERANCE_SEC}`.
  The doc comment says the constant holds the hash shape for byte-identical hashes on default sets and says nothing
  about the run; it names spec §5.2.
- [ ] **Step 4:** the new test and `stage_hashes_are_pinned_to_golden_literals` PASS.
- [ ] **Step 5:** commit `fix(stacking): the calibration hash no longer moves with the grouping tolerance`.

### Task 2: Frame-level key helper, flat layout, frame-only stem; Calibrate and Prepare on it

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/paths.rs` (`WorkingLayout` dir functions :205-265)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` — `calibrated_file_stem` :1965; `resolve_calibrate_job`
  :2000-2200 (lookups :2028/:2036, paths :2179/:2189); calibrate write :2446/:2461; `resolve_prepare_job`
  :2559-2700 (lookups :2658/:2666, dir :2697); prepare write :2819/:2834; `stage_calibrate_with` dir :3418;
  solve stage calibrated read :4908; measure calibrated read :5411; drizzle mosaic read :12711
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` — calibrated / calibrated_mosaic freshness :3601, :3627
- Modify: `crates/athenaeum-core/src/db/stacking.rs` — `upsert_artifact` caller contract (see Step 3)
- Test: `run.rs` `mod tests`, `paths.rs` `mod tests`, `plan.rs` `mod tests`

**Interfaces (produces, used by Tasks 4, 5, 7, 9, 10):**
- `paths.rs`: `pub const FRAME_LEVEL_KINDS: &[&str] = &["calibrated", "calibrated_mosaic", "measured", "metrics", "registered", "registration", "solve"];`
  (Prepare writes `calibrated` / `calibrated_mosaic`, `run.rs:2819-2845`; there is no `prepared` kind) plus
  `pub fn is_frame_level_kind(kind: &str) -> bool` (true for those and for any `fits.<n>`);
  `pub fn artifact_group_key<'a>(kind: &str, group_key: &'a str) -> &'a str` (`""` for a frame-level kind, else
  `group_key`). The hand-written `""` of `solve` / `registration` (`run.rs:4903`, `solve.rs:188`, `solve.rs:245`,
  `origin.rs:410`, `plan.rs:1275`) goes through it too, so it is the one rule.
- `WorkingLayout`: `calibrated_dir()` and `prepared_dir()` take no group key and return their root. (Task 4 does the
  same for `measured_dir()`, `measured_path(stem)`, `fits_dir()`, `fits_path(stem, plane)`; Task 5 for
  `registered_dir()`.) `ln_dir(group_key)` and the LN paths keep the group key.
- `run.rs` `mod tests`: `fn fan_out_items(logs: &str, stage: &str, pass: &str) -> usize` — the sum of `items` over the
  `fan-out finished` lines of that stage and pass in a `crate::stacking::telemetry::capture_logs` text, 0 when there
  is none. Every "N items" assertion in Tasks 2, 4, 5, 8, 10 and 12 uses it.
- `run.rs`: `pub(crate) fn frame_file_stem(frame: &GroupFrame) -> String` → `<file stem>_f<frame_id>`, replacing
  `calibrated_file_stem(group, frame)` at every call site: calibrated, registered via `registered_file_name`, measured
  spill, fits, and the LN sidecar path (`run.rs:13906-13913`; LN reuses by `row.path`, `:14127-14136`, so an old
  `.athln` is orphaned until "Delete intermediates", which removes `ln/` whole). Prepare's project stem (`run.rs:2695`)
  moves to `frame_file_stem` too; the attested `_f<id>` branch (`run.rs:2686-2693`) is then redundant and is folded.
- `run.rs`: `fn replace_frame_level_file(conn: &Connection, layout: &WorkingLayout, old: Option<&StackingArtifactRow>, new_path: &Path)`
  — after a successful write and upsert: when `old.path` differs from `new_path`, the old path lies under
  `layout.root` (a Prepare row may point at a landed file used in place, `paths.rs:1100-1104`), and no row of ANY
  subject still points to it, remove the old file; a failed removal is a `warn!` (`path`, `error`), never an error.

- [ ] **Step 1: failing tests**
  - `paths.rs`: `frame_level_kinds_use_the_empty_group_key` — `artifact_group_key("calibrated", "osc__x__bin1__180s") == ""`,
    `artifact_group_key("fits.2", "k") == ""`, `artifact_group_key("ln", "k") == "k"`, `artifact_group_key("two_pass", "k") == "k"`.
  - `run.rs`: `frame_file_stem_ignores_group_membership` — two `GroupFrame`s sharing `Light_0001.fits`, ids 7 and 9:
    stems `Light_0001_f7`, `Light_0001_f9`; a lone frame id 3 → `Light_0001_f3`.
  - `run.rs`: `calibrated_artifact_is_reused_across_a_regroup` — seed a 4-frame set (`seed_star_group`,
    `add_master_dark_and_flat`), run Calibrate under `exposure_mode = Seconds`, then under `NoSplit` (key `…__all`):
    the second run's `calibrated` rows keep their `created_at` and their files' mtime; rows have `group_key = ''`.
  - `run.rs`: `rewriting_a_frame_removes_its_old_file_once_unreferenced` — a row whose path is
    `calibrated/<old_key>/c_x.fits`, re-calibrated with `rerun_from = Calibrate`: new file at `calibrated/c_x_f<id>.fits`,
    old file gone; when a second row (another kind) points to the old path, it stays.
  - `run.rs`: `a_project_frame_uses_the_frame_level_key` — a project fixture (`add_project_frame`) prepared once: its
    `prepared` row has `group_key = ''` and `project_frame` set.
  - `plan.rs`: `plan_gate_reads_calibrated_rows_under_the_empty_key` — after a Calibrate under one grouping,
    `build_plan` under another grouping does not list `Calibrate` in `stale_stages`.
- [ ] **Step 2:** `cargo test -p athenaeum-core --lib frame_level` (and each name above) → FAIL.
- [ ] **Step 3:** implement the helpers; route every `find_artifact` / `upsert_artifact` for `calibrated`,
  `calibrated_mosaic`, `prepared` through `artifact_group_key`; flat dirs; `frame_file_stem`; call
  `replace_frame_level_file` after each calibrate / prepare upsert with the row read in the resolve step.
- [ ] **Step 4:** the new tests PASS; `cargo test -p athenaeum-core --lib stacking::` PASS (fixtures that asserted a
  `<group_key>/` sub-folder or a collision-only `_f<id>` suffix are updated to the new layout, each named in the
  commit body).
- [ ] **Step 5:** commit `feat(stacking): calibrated and prepared frames belong to the frame, not the group`.

### Task 3: Weight settings leave the measurement hash

**Files:** Modify `crates/athenaeum-core/src/stacking/config.rs:1029-1040` (`measurement_subtree_with_measure_version`).
Test: `config.rs` `mod tests`.

- [ ] **Step 1: failing test** `measurement_hash_ignores_the_weight_settings`: configs differing only in
  `weight_mode` (`Noise`, `Formula`), `formula.fwhm = 40.0`, and `weight_mode = Keyword` with `keyword = "WEIGHT"`
  (the keyword is read from the calibrated header at run time, `run.rs:5867-5871`) hash
  (`measurement_subtree`) equal to the default's; a config differing in `max_stars` or `seed_threshold` hashes
  differently.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** the subtree serializes a clone of `cfg.measurement` with `weight_mode`, `formula`, `keyword` set
  to `MeasurementConfig::default()`'s values. Doc comment: the weights are computed every run from stored measurements
  (`run.rs` `stage_measure_with`, `compute_weights`) and no fit reads these fields; spec §5.4.
- [ ] **Step 4:** the new test and the golden pins PASS (default config → byte-identical).
- [ ] **Step 5:** commit `fix(stacking): changing the frame weighting no longer re-measures`.

### Task 4: Measure kinds on the frame key

**Files:**
- Modify: `run.rs` — `measure_two_pass_prepare` :6210-6300 (lookups :6243/:6250, spill path :6231), metrics lookup
  :5610, measured upsert :6715, metrics upsert :6765, fits upsert :6860 and its path :6788, fits read :7737
  (`resolve_register_fits` path), LN seeds fits read :9853, LN reads :13876 when its kind is `fits.<n>`
- Modify: `plan.rs` — `gate_measure_betas` :748, fits freshness :1100 (`fits_artifact_fresh*`), metrics :3603
- Test: `run.rs`, `plan.rs` `mod tests`

**Interfaces:** consumes Task 2's `artifact_group_key`, `frame_file_stem`, flat `measured_path(stem)` /
`fits_path(stem, plane)`, `replace_frame_level_file`.

- [ ] **Step 1: failing tests**
  - `measure_rows_are_reused_across_a_regroup` — Measure under `Seconds`, then under `NoSplit`:
    `fan_out_items(logs, "measure", "pass1") == 0` and `… "pass2") == 0` in run 2, equal β, `measured` / `metrics` /
    `fits.*` rows unchanged (`created_at`).
  - `merged_group_with_a_new_beta_reruns_pass_two_only` — the two exposure classes rendered with different PSF shapes
    so their group βs differ: add `add_light_with_moffat_field(f, spec, stars, fwhm, beta, background, noise_sigma, seed)`
    to `test_fixtures.rs` beside `add_light_with_field_fwhm`, rendering through `crate::test_support::moffat_field`;
    one class at β 2.5, the other Gaussian (`add_light_with_field_fwhm`). First assert the merged group's β differs
    from at least one source group's; then pass-1 items 0, pass-2 items = members.
  - `the_spill_is_read_from_the_row_path` — a `measured` row whose path is in an old `measured/<key>/` folder is used
    as is (pass-1 items 0).
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** route the Measure kinds through `artifact_group_key`; the helpers that only read frame-level kinds
  drop their `group_key` parameter instead of being passed `""` (`gate_measure_betas` `plan.rs:748`,
  `fits_artifact_fresh*` `plan.rs:1062-1113`, `read_member_fits` `run.rs:9844`, `resolve_register_fits` `run.rs:7678`);
  in `measure_two_pass_prepare` take the spill path from the `measured` row when one exists, the layout path only for
  a new write; `replace_frame_level_file` after each measured / fits upsert.
- [ ] **Step 4:** new tests and `cargo test -p athenaeum-core --lib stacking::` PASS.
- [ ] **Step 5:** commit `feat(stacking): a frame's measurement survives a regroup`.

### Task 5: `registered` on the frame key; readers open the row's path

**Files:** `run.rs` — registered write :8260 (dir), :9707 (upsert), :9737 and :11475 (reads); `plan.rs` :1940
(`compute_register_stale`'s registered read). Test: `run.rs`, `plan.rs`.

- [ ] **Step 1: failing tests** `registered_frames_are_reused_across_a_regroup` (Register under `Seconds`, then
  `NoSplit`, same reference: the registered rows and files untouched, align items 0);
  `a_migrated_row_is_read_from_its_stored_path` (a `registered` row pointing into `registered/<old_key>/` feeds
  Integrate unchanged).
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** `artifact_group_key` for `registered`; flat `registered_dir()`; `replace_frame_level_file` after the
  upsert; every reader uses `row.path`.
- [ ] **Step 4:** PASS, plus `cargo test -p athenaeum-core --lib stacking::`.
- [ ] **Step 5:** commit `feat(stacking): registered frames belong to the frame`.

### Task 6: OSC registration hash without the measurement hash

**Files:** `plan.rs:800` (`registration_hash_for` doc), `plan.rs:1903` (gate), `run.rs:8000` (run). Test: `plan.rs`.

**Interfaces:** produces `pub(crate) fn register_reads_measure_fits(cfg: &StackingConfig, group: &IntegrationGroup, prepare_path: bool) -> bool`
in `plan.rs` = `stage_one_planes(cfg, group, prepare_path) == 1` (`plan.rs:1153`). `prepare_path` is true for a project
subject or an attested set, exactly as the gate already computes it (`plan.rs:3554`); the run derives the same flag
from `rc.subject` / `rc.origins`. NOT `expected_stage_one_planes` (`run.rs:2488`, always `prepare_path = true`): a frame
set's OSC group with `calibration.debayerOsc = false` calibrates to a 1-plane mosaic that DOES read `fits.0`
(`run.rs:7678-7685`), and that function reports 3 for it. The run and the gate pass `""` as `frame_measurement_hash`
when the predicate is false (the function folds nothing for `""`). An OSC frame folds nothing in its place: its
luminance detection reads only `cfg.registration` (`register/frame.rs:224-249`), already in `registration_subtree`.

- [ ] **Step 1: failing tests** `osc_registration_hash_ignores_the_group_beta` (debayered OSC: two βs → equal hash),
  `mono_registration_hash_follows_the_group_beta` (mono: two βs → different hashes), and
  `osc_without_debayer_registration_hash_follows_the_group_beta` (frame set, OSC, `debayer_osc = false`: two βs →
  different hashes).
- [ ] **Step 2:** run → FAIL (debayered OSC case).
- [ ] **Step 3:** both callers use `register_reads_measure_fits`. Update the C-18 doc on `registration_hash_for`: the
  fold follows what the frame's registration reads (spec §5.5).
- [ ] **Step 4:** PASS, plus the existing gate/run agreement tests.
- [ ] **Step 5:** commit `fix(stacking): an OSC frame does not re-register when its group's beta moves`.

### Task 7: Pass 1 survives "Delete intermediates"; unreferenced sweep

**Files:** `paths.rs:1030` (`INTERMEDIATE_ARTIFACT_KINDS`), `paths.rs:1110` (`cleanup_work`). Test: `paths.rs`.

**Interfaces:** produces `fn sweep_unreferenced(conn: &Connection, root: &Path) -> anyhow::Result<u64>` — walks
`root` recursively (migrated files sit in `measured/<old_key>/`), removes every file that no `stacking_artifacts.path`
of ANY subject names (`WorkingLayout::new(working_dir, set_slug(name))` lets two sets with one slug share a root,
`paths.rs:53`, `:163-165`), removes sub-folders left empty, returns the bytes freed; each failed removal is a `warn!`
and the sweep continues.

- [ ] **Step 1: failing test** `intermediates_sweep_keeps_referenced_pass1_files`: rows for two `measured` files and one
  `fits.0`, one unreferenced `.athm` and one unreferenced `.athf`; `cleanup_work(.., Intermediates)` → `calibrated/`
  gone, the referenced files present, the unreferenced ones gone, the `measured` rows present.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** drop `"measured"` from `INTERMEDIATE_ARTIFACT_KINDS`; the `Intermediates` arm no longer removes
  `measured_root()` and calls `sweep_unreferenced` on `measured_root()` and `fits_root()`; `All` unchanged. Update the
  R11c-6 comment.
- [ ] **Step 4:** PASS, plus the existing cleanup tests (adjust the one that expects `measured/` gone after
  `Intermediates`, named in the commit).
- [ ] **Step 5:** commit `feat(stacking): delete intermediates keeps the star measurements`.

### Task 8: Pass 1 valid on a byte-identical recalibration

**Files:** `measure.rs:1979-1985` (whole-record read) and `measure.rs:2098-2108` (`check_pass1_head`); test in
`run.rs` `mod tests`.

- [ ] **Step 1: gate pin** `recalibration_reproduces_the_planes_bit_for_bit`: calibrate a fixture frame, read its
  planes; delete the calibrated file and its row; calibrate again in a NEW run context; assert every plane equal as
  `f32::to_bits`. **If this pin fails, stop the task:** report it, keep the mtime stamp, skip Steps 2–5 (spec §8 Q1).
- [ ] **Step 2: failing test** `pass1_survives_a_byte_identical_recalibration`: run 1 Calibrate + Measure; then
  `cleanup_work(.., Intermediates)` (after Task 7 it removes `calibrated/` and keeps `measured/`), with NO
  `rerun_from` (a `rerun_from = Calibrate` forces Measure fresh: `run.rs:5075`, `stage_forces_fresh`); run 2
  recalibrates every frame and, in that same run, `fan_out_items(logs, "measure", "pass1") == 0`.
- [ ] **Step 3:** both stamp checks compare `len` only; `modified_ns` stays in the head (no format change,
  `PASS1_ARTIFACT_VERSION` unchanged) and the R11c-4 doc says validity follows the calibration hash in the row's key
  (spec §5.3). The `Stale` error stays for a size change.
- [ ] **Step 4:** PASS, plus `pass1_artifact.rs` tests (the R11c-4 test at :613-646 changes from "touched file is
  stale" to "a file of another size is stale").
- [ ] **Step 5:** commit `feat(stacking): a byte-identical recalibration keeps the first pass`.

### Task 9: Migration — frame-level rows to `''`

**Files:** `db/schema.rs` (after `migrate_rejection_normalization_local`, same `cfg` gate and call pattern). Test:
`db/schema.rs` tests.

**Interfaces:** `fn migrate_frame_level_artifact_keys(conn: &Connection) -> Result<()>`; marker row
`stacking.migration.frame_level_keys` in `settings`, written in the same `BEGIN IMMEDIATE` transaction; an error is
logged (`error`) and retried at the next start, never blocking `init_db`.

- [ ] **Step 1: failing test** `frame_level_rows_rekey_to_the_empty_group_keeping_the_newest`: rows for frame 1 kind
  `calibrated` under `…180s` (older) and `…300s` (newer), `fits.0` under `…180s`, `ln` under `…180s`; after the
  migration: one `calibrated` row under `''` = the newer one with its path unchanged, `fits.0` under `''`, `ln` still
  under `…180s`, the marker present; a second call changes nothing.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** the kind list comes from `paths::FRAME_LEVEL_KINDS` plus `kind LIKE 'fits.%'`, never a second
  literal (the function sits under the same `cfg(all(feature = "render", feature = "solver"))` gate as
  `migrate_rejection_normalization_local`, `schema.rs:3478`). One transaction: delete the non-newest duplicates per `(COALESCE(frames_set_id,0), COALESCE(project_id,''), kind, COALESCE(frame_id,0), COALESCE(project_frame,0))`
  among frame-level kinds (`ORDER BY created_at DESC, id DESC`, `ROW_NUMBER()`), then `UPDATE … SET group_key = ''`;
  two `info!` lines, "frame-level artifact keys migrated" and "duplicate frame-level artifacts dropped", each with
  `count`. No file is touched.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** commit `feat(stacking): existing frame-level artifacts move to the frame key`.

### Task 10: Two-pass decision cache; progress message per pass

**Files:** `run.rs` — `two_pass_refine` :8891, `stage_register` :9431-9560 (`dry_total`, progress), the register
progress emission in `register_group_pass` :7909. Test: `run.rs` (next to `two_pass_moves_the_reference_off_a_deviating_top_weight_frame` :19736).

**Interfaces:** `const TWO_PASS_ARTIFACT_KIND: &str = "two_pass";` (group-level, survives `Intermediates`, dropped
by `All`); `fn two_pass_key(cfg: &StackingConfig, candidate_id: i64, candidate_calib_hash: &str, ranked_ids: &[i64], allowed_ids: Option<&[i64]>, member_hashes: &[(i64, String)]) -> String`
= `stage_hash(&registration_subtree(cfg), [ref:<id>, the calib hash, ranked:<ids>, allowed:<ids> (when Some), member:<id>:<hash>…], [])`.
- `ranked_ids`: exactly what `two_pass_pick` ranks — the members after `two_pass_candidates`' `measure_floored`
  filter (`run.rs:8840-8850`), ordered by weight with its index tie-break (`weights.rs:644-656`), top
  `TWO_PASS_CANDIDATES`, ids in rank order.
- `allowed_ids`: a project's WCS-allowed candidates (`rc.wcs_rule.candidates`, `run.rs:8944-8955`), sorted; `None` on
  a frame set.
- `member_hashes`: every included member's `registration_hash_for` against the candidate (Task 6's rule), sorted by id.
- Payload `{"switchTo": Option<i64>}`. On a hit `rc.dry_pass_stars` is empty, so the `TwoPassSwitch`
  (`run.rs:8825-8834`) is built through the existing fresh-detection fallback (`run.rs:~9054`) — state it in the doc.
  Skipping the dry pass has no other effect: it excludes nobody and stores nothing (`run.rs:8616-8626`,
  `:8749-8755`); its `corner_px_*` are log-only.

- [ ] **Step 1: failing tests** `two_pass_decision_is_cached_on_the_second_run` (run 1 dry-pass align items = members;
  run 2 dry-pass items 0 and the same `switchedFrom` / final reference); `two_pass_cache_misses_when_a_member_changes`
  (one member recalibrated → dry pass reruns); `dry_pass_progress_names_its_pass` (a capturing `ProgressEmitter`: dry
  ticks carry `message = "checking the reference"`, real ticks `"aligning"`; with a cache hit the stage `total` equals
  the real pass's count).
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** before the dry pass compute `two_pass_key`; a fresh row → skip `register_group_pass(persist=false)`
  and build the `TwoPassSwitch` from `switchTo` through the code path a dry-pass pick uses today; a miss → dry pass,
  then upsert the row. `dry_total = 0` on a hit. Add `"two_pass"` to the group-level side of the kinds doc in
  `paths.rs`.
- [ ] **Step 4:** PASS, plus the existing two-pass tests (:19736, :20004, :20030).
- [ ] **Step 5:** commit `feat(stacking): the reference check is cached and named in the progress`.

### Task 11: `maxStars` default 9 000

**Files:** `config.rs:305` (default), `config.rs:1334`, `:2797`, `:2805` (pins), `measure.rs:126`
(`StarPopulation::probe_default`), `measure.rs:3128-3132` (serde pin), `examples/ln_scale_probe.rs:88,216`; the
`stage_hashes_are_pinned_to_golden_literals` and `plan.rs` golden literals.

- [ ] **Step 1:** change the pins to 9 000 (`assert_eq!(c.measurement.max_stars, 9000)`, the serde pin
  `"maxStars":9000`); add `a_stored_document_keeps_its_max_stars`: `resolve_config(Some(r#"{"measurement":{"maxStars":24576}}"#), None)`
  resolves to 24 576.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** `max_stars: 9_000`; the probe defaults follow; re-record only the golden literals the default
  moves (measurement, registration, normalization subtrees) — the commit body lists each old → new literal and says
  "owner ruling 2026-11-08, spec §5.8; no truth bar run". `fitStars` canonicalization and the
  `max(MIN_FIT_STARS, registration.maxStars)` floor are unchanged.
- [ ] **Step 4:** `cargo test -p athenaeum-core --lib stacking::config stacking::measure stacking::plan` PASS.
- [ ] **Step 5:** commit `feat(stacking): new stacking documents measure 9000 stars by default`.

### Task 12: Regroup acceptance, docs, real-data smoke

**Files:** `run.rs` `mod tests`; `docs/stacking/README.md` (a "Frame-level cache" section: kinds table, the two
constant-shaped hashes, OSC rule, two-pass cache, migration, maxStars ruling); the wave-6 spec 2026-10-02 §8 sentence
gets "superseded by 2026-11-08 frame-level cache"; `docs/superpowers/open-items.md` (the smoke, if owed).

- [ ] **Step 1: test** `regroup_reuses_every_frame_level_stage`: one fixture, three runs through
  `run_stages_for_test(.., Stage::Register)` + `stage_output` (pattern: `local_normalization_sidecars_are_cached_on_the_second_run`
  :23937): `Seconds` (2 groups), `Stops`, `Seconds` with tolerance 200, co-registered geometry (the default). Runs 2
  and 3: calibrate, pass-1, pass-2 and align items 0; LN and Integrate run; `build_plan` before runs 2 and 3 has no
  `Calibrate`, `Measure`, `Register` in `stale_stages`. (Under Native geometry each group has its own reference,
  `plan.rs:1806-1823`, so a regroup re-registers by `ref:<id>`; that is correct and not asserted here.)
- [ ] **Expectation to record (docs):** the migration keeps the newest row per frame. A set whose newest rows were
  written at a non-default tolerance (set 270: run 11 at 200 s) recalibrates once after the update, because Task 1's
  constant hash no longer matches them (spec §6 row 2). Not fixable in SQL; it is said, not hidden.
- [ ] **Step 2:** full gate (large merge): `cargo test -p athenaeum-core` and `cargo test --workspace`; known flakes
  per the pre-push memory are re-run once, not ignored.
- [ ] **Step 3: real-data smoke (dev app, `npm run tauri dev`, dev catalog):** a real OSC set with two exposure
  classes; stack under `Seconds`; switch to tolerance 200 and stack again. Pass: the second run's logs show
  `fan-out finished` with `items` 0 for calibrate, pass1, pass2 and align (or no such fan-out), and wall time ≈ LN +
  Integrate. Folders and catalog copies under `~/.athenaeum-acc/` with a `REGISTRY.tsv` row; delete them once the
  result is recorded. A smoke that cannot run goes to `open-items.md`.
- [ ] **Step 4:** docs as listed; commit `docs(stacking): the frame-level cache`.

### Task 13: Remove the stale tolerance hint (frontend, `frontend-dev`)

Added after the Task 1/3 review (I1). `src/components/stacking/GroupsTable.tsx:53-54` shows "The tolerance is part of
the calibration hash: changing it re-calibrates this set's lights on the next run." for every calibrating set in
`seconds` mode (`StackingTab.tsx` ~:1324). After Tasks 1 and 2 it is false, and it discourages the regroup this job
makes cheap.

**Files:** `src/components/stacking/GroupsTable.tsx`, `GroupsTable.test.tsx` (:21, :30-39, :113, :177-217),
`StackingTab.tsx` (the `toleranceInCalibrationHash` prop), `StackingTab.test.tsx` (:44-47);
`docs/stacking/README.md` (:1164-1166, :1672-1676).

- [ ] Remove `RECALIBRATE_HINT`, the `toleranceInCalibrationHash` prop end to end, and the tests that pin the hint;
  no replacement text. `npm run test -- GroupsTable StackingTab` and `npx tsc --noEmit` pass.
- [ ] Rewrite the two README passages: the tolerance is no longer part of the calibration hash (spec §5.2).
- [ ] Commit `fix(stacking-ui): the grouping tolerance no longer warns about recalibrating`.

Known residual (I1 review, M1), recorded in spec §5.4 and `docs/superpowers/open-items.md` by Task 12: after a
weighting edit the plan gate can report Register / Normalize cached while the run re-picks the reference by weight and
re-registers; the run's result is correct, the gate under-reports.

---

## Closing note — what each change keys on

| Artifact | Key | Hash folds | Recomputes on |
| ---- | ---- | ---- | ---- |
| `calibrated`, `calibrated_mosaic`, `prepared` | frame, `''` | masters, light file, calibration options (tolerance constant) | masters / options / file change |
| `measured` (pass 1) | frame, `''` | calibration hash, measurement options (weights constant) | the above, or measurement options |
| `metrics`, `fits.<n>` (pass 2) | frame, `''` | the above + group β (Auto) | β change |
| `registered` / registration row | frame, `''` / `(set, frame)` | reference, calibration hashes, measurement hash for mono only | reference change; mono β change |
| `two_pass` | group | candidate, candidates, members' registration hashes | membership or any member's registration input |
| `ln*`, rejection, master | group | as today | any group change |
