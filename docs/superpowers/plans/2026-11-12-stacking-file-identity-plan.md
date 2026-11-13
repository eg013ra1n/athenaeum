# Stacking file identity without the modification time: implementation plan

> For agentic workers: the `lean-execution` skill governs. One fresh implementer per task (`rust-engineer`, sonnet); the controller re-gates every task.

**Spec:** `docs/superpowers/specs/2026-11-12-stacking-file-identity-design.md` (approved 2026-11-12, rulings FI-1 to FI-9); § numbers are the spec's. **Base:** `main` at
`0df147a7`. Every `path:line` here is of `0df147a7`; code paths are under `crates/athenaeum-core/src/`. The spec's §3-§9 lines are of `484fa75f`; "Spec corrections" lists every
difference that matters.

**Goal:** an own working file is judged by key + size; a light or master by size + sampled hash; a cancelled rewrite leaves no row over another config's file; a moved master and
a moved working folder recompute nothing; a mono regroup keeps its registrations when the reference stays.

## Global constraints

- No command, route, TS type, setting or schema change (the two-backend rule is untouched; `GroupFrame` keeps `modifiedAt`). No version constant moves (§5).
- Hashes move in T4b (stage 1 and everything chained on it) and T5 (registration of one-plane references), nowhere else: `stage_hashes_are_pinned_to_golden_literals`
  (`stacking/plan.rs:8722`) passes unchanged in every other task.
- `tracing` only; a new field name goes into `docs/logging/README.md` in the task that adds it. Every `Err` is logged before it is returned or turned into a fallback,
  except a failed `delete_artifact`, which is never a fallback: it stops the job with `?` at all seven write sites (a warn-and-continue reopens the hole of §4.2).
- Test names: letters, digits, `_`. No other code base is named anywhere; a comparison program is "the ERP". Commits: the owner's identity plus the harness's trailers.

## Summary

| Task | What becomes true | Files | Size | Depends on | Lands with |
| ---- | ---- | ---- | ---- | ---- | ---- |
| T1a | Calibrate, Prepare, pass 1 and `.athf` delete the old row (and a mosaic row whose file they overwrite) before they replace a file; `delete_artifact` | `db/stacking.rs`, `stacking/run/{calibrate,prepare,measure}.rs` | M | - | M1 |
| T1b | Register, LN sidecars and the LN reference do the same | `stacking/run/{register,normalize}.rs` | M | T1a | M1 |
| T2 | Own files read by key + size; Blink by size; an in-place row must name the frame's current input | `stacking/{plan,paths}.rs`, `stacking/run/{prepare,calibrate,measure_tests}.rs`, `api/stacking_aligned.rs` | M | T1a, T1b | M1 |
| T3 | The sample rule, the memo, the book type; `bank_content_hash`; one-stat row match | `stacking/source_identity.rs` (new), `db/operations.rs`, `duplicates/backfill.rs` | M | - | M2 |
| T4a | Every run samples its inputs once, before stage 1, and banks what it may; gate tests bank their fixtures | `stacking/{plan,test_fixtures}.rs`, `stacking/run/{calibrate,mod,measure,reference,register,output,masters_tests}.rs` | L | T3 | M2 (the T4 commit) |
| T4b | The stage-1 hash is built on samples; masters lose their path; goldens re-pinned | `stacking/{config,plan,source_identity}.rs`, `stacking/run/{masters,calibrate_tests}.rs` | M | T4a | M2 (the T4 commit) |
| T4c | `strict_mtime` is gone; coarse times and in-window edits pinned on a regroup rig | `stacking/{measure,pass1_artifact}.rs`, `stacking/run/{measure,measure_tests,normalize_tests}.rs` | M | T4b, T2 | M2 (the T4 commit) |
| T5 | The reference's measurement is folded for every one-plane frame (FI-6) | `stacking/plan.rs`, `stacking/run/{register,two_pass,normalize_tests}.rs` | S | T4c | M2 |
| T8a | The rebase rule, its check and its commit, with no caller | `stacking/rebase.rs` (new), `stacking/mod.rs`, `db/stacking.rs` | M | M2 | M3 |
| T8b | A run rebases rows whose files moved with the working folder | `stacking/run/{calibrate,normalize_tests,prepare_tests}.rs`, `docs/logging/README.md` | M | T8a, T2, T4a, T4c | M3 |
| T8c | The gate and Blink read through the rebase; previews follow the current folder | `stacking/{rebase,plan}.rs`, `stacking/run/{measure,register}.rs`, `api/{stacking,stacking_aligned}.rs`, docs | M | T8b | M3 |
| T6 | The references describe the new rules | `docs/stacking/README.md`, `docs/catalog/README.md`, `docs/superpowers/open-items.md` | S | M2, M3 | M4 |
| T7 | §9 passes on real data (A0-A3, A2b, A2c) | `docs/stacking/README.md` (numbers), `docs/superpowers/open-items.md` | L | everything | M4 |

Merges to local `main`, each `--no-ff`: **M1** = T1a + T1b + T2 (the time leaves own files only together with the write order, §10); **M2** = T3 + T4 (one commit) + T5; **M3** =
T8a + T8b + T8c; **M4** = T6 + T7. M1-M3 ship in one release with the frame-level cache (FI-4, FI-5). Nothing is pushed without the owner's word.

## Execution

- One worktree, `.claude/worktrees/file-identity`; branches cut from local `main` after the previous merge: `fi/own-files` (M1), `fi/sampled-identity` (M2), `fi/rebase` (M3),
  `fi/docs-acceptance` (M4). Merges run in the primary checkout: `git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum merge --no-ff fi/<branch>`.
- **T4 ends in one commit.** T4a, T4b, T4c are three implementers in sequence on `fi/sampled-identity`, each leaving a green tree and a WIP commit; before the T4+T5 review the
  controller squashes them (`git reset --soft <T3 commit> && git commit`, message `feat(stacking): a frame's stage-1 identity is its sampled content, not its modification
  time`); T5 is committed after the squash.
- **T8 starts after M2 is merged, never beside it**: T8b's regressions run on T4c's `RegroupRig` and `set_times`, and its trigger sits before T4a's `ensure_source_samples`
  (the spec's "nothing of T3 to T5", §10, holds for T8a's code only; correction 16). T8a, T8b, T8c are three implementers in sequence on `fi/rebase`.
- ONE cargo at a time (16 GB machine). Each implementer runs its task's module tests (given per task) and "the two checks":
  - `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo check -p athenaeum-core --no-default-features`
  - `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo check -p athenaeum-web -p athenaeum-tauri --all-targets`
- The controller alone runs the core lib suite (about 3 min) on each merge candidate: `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib`
- Independent reviews (opus, given the diff and the spec, not this plan's conclusions): after T1+T2, after T4+T5, after T8, and a whole-branch review of `0df147a7..main` before
  M4; a group merges after its review's fixes. Disk report (`python3 -I ~/.claude/skills/lean-execution/disk-report.py`) at each phase start; the cargo budget was 188 of 200 GB
  on 2026-11-12: sweep orphaned `.o` before the first build if it is hit.

## Reuse audit

| Existing | Where | Use |
| ---- | ---- | ---- |
| `duplicates::compute_xxhash` | `duplicates/mod.rs:15-57` | KEEP: the sample (first, middle, last 512 KiB) |
| `disk_matches_row`; the content index guard and UPDATE | `duplicates/backfill.rs:353-364`; `:255`, `:274-285` | ADAPT (T3): split out `metadata_matches_row`; the content index KEEPs its own (§7) |
| `bank_strong_hash` | `db/operations.rs:7565-7570` | MODEL for `bank_content_hash` (T3), which guards in its `WHERE` |
| `rename_files_path_prefix`, `path_prefix_upper` | `db/operations.rs:4298-4311`, `:38` | KEEP the byte-range predicate for T8a's candidate query |
| `Stamp::matches`, `MTIME_TOLERANCE_SECS` | `collab/storage/sweep.rs:48-53`, `:10` | ADAPT (T8a): the 2 s constant, compared at full precision (correction 6) |
| `find_artifact`, `find_artifact_sql`; `delete_artifacts` | `db/stacking.rs:1022-1057`; `:1096-1136` | KEEP; `delete_artifact` uses the same six expressions; the plural one deletes by kinds, not a point |
| `replace_frame_level_file` | `stacking/run/frames.rs:36-76` | KEEP; callers pass the row they read before the delete |
| `resync_catalog_rows_from_disk`, the re-parse's NULLs; zip extraction banking | `scanner/mod.rs:1350-1356`, `:1384-1396`; `zip_store/materialize.rs:360-366` | KEEP: a rebuilt or re-parsed file is sampled again; the existing second producer of `content_hash` |
| `master_identity`, `master_identity_in`; `HashMemo` | `stacking/plan.rs:410-454`; `:546-650` | REPLACE (T4b; the zip-marker query moves into `source_identity::input_row`); ADAPT (T4a, T4b) |
| `origin::prepare_input`, `subject_prepares`; `paths::subject_layout`, `WorkingLayout::*_root`; `normalize_path` | `stacking/origin.rs:79-93`, `:64-69`; `stacking/paths.rs:44-70`, `:173-245`; `api/scan_roots.rs:58-70` | KEEP: the in-place rule's input; old roots with the current slug; the run's spelling |
| `working_dir_as_run_spells_it` | `api/stacking.rs:845-860` | NOT reused (it goes through `validate_transfer_dir`); T2 adds a read-only `run_spelled_working` |
| `registered_artifact_is_fresh` | `stacking/run/register.rs:2183-2204` | ADAPT (T1b): return the row, the caller judges |
| fixtures `frame_set*`, `add_light*`, `add_master_dark_and_flat`, `file_identity`; `capture_logs`, `fan_out_items`, `seed_run_and_groups` | `stacking/test_fixtures.rs:45-160`, `:367-393`, `:587-592`; `stacking/run/tests.rs:437`, `:455` | KEEP; T4a adds `bank_samples` |
| `Pass1Rig` (`new`, `build`, `attested`), `RegisterRig`, `LnRig`, `CancelWhenRegisterDone` | `stacking/run/tests.rs:564-700`, `register_tests.rs:446-600`, `:458-470`, `normalize_tests.rs:273` | KEEP; T4c extracts `RegroupRig` from `normalize_tests.rs:745-860` |
| `PASS1_LOG`, `PASS1_CANCEL_AFTER`, `PASS1_TOUCH_AFTER`, `FITS_WRITE_FAILURE_LOG`; `stamp_later_mtime` | `stacking/measure.rs:1862-1880`, `stacking/run/measure.rs:22`; `scanner/mod.rs:3072-3082` | KEEP; MODEL for T3's hooks; tests use `File::set_modified` the same way |

Nothing the spec asks to write exists already: no artifact point delete, no stacking-side sample memo, no listing of a subject's working folders, no artifact path update.

## T1a: write order at Calibrate, Prepare, pass 1 and `.athf`

**Problem.** A writer replaces a frame-level file, then upserts its row. The Calibrate and Prepare commit loops return `Cancelled` at the first unfinished job
(`stacking/run/calibrate.rs:211-212`, `stacking/run/prepare.rs:552-557`), so files later jobs wrote keep the old row (old hash, equal size). Today only the time catches that (R7
b); T2 removes the time.

**Files.** `db/stacking.rs:1022-1057` (new `delete_artifact` after it; test beside `:2386`). `stacking/run/calibrate.rs:130-179` (`calibrate_queue`, worker `:152-177`),
`:363-574` (`resolve_calibrate_job`: rows `:388-411`, the mosaic find under `want_mosaic` `:399-409`, `out` `:542-545`, `mosaic_out` `:546-561`, `Ok(Job)` `:563-573`).
`stacking/run/prepare.rs:77-267` (`resolve_prepare_job`: rows `:174-193`, `converted_out` / `debayered_out` `:209-210`, `Ok(Job)` `:247-266`), worker `:527-547`.
`stacking/run/measure.rs:1150-1176` (`MeasureMember`), `:1306-1423` (`measure_two_pass_prepare`: rows `:1337-1356`, the `metrics` β `:1361-1366`, the `measured` freshness
`:1378` and its else arm `:1402`, owed set `:1414-1417`), the pass-2 validation of a pending row `:1673-1688` and fallback `:1758-1762`, `:1508-1525`,
`:1790-1800`, `:1825-1860` (`upsert_pass1_row`), `:1883-2015` (`persist_measurement`: C-30 comment `:1919-1944`, write `:1954`, old row `:1990-1997`). Tests in
`stacking/run/{calibrate_tests,prepare_tests,measure_tests}.rs` and `db/stacking.rs`.

**§4.2 against the code:**

| Kind | Old row read today | Deleted at |
| ---- | ---- | ---- |
| `calibrated`, `calibrated_mosaic` | `resolve_calibrate_job :388-411` (the mosaic only when `want_mosaic`, `:399`), carried `:565-566` (spec `:262-284`, `:438-439`) | right before `Ok(Job)` at `:563`, after every exclusion return; the mosaic row, read always, when `want_mosaic` or its path is `out` |
| Prepare's rows | `resolve_prepare_job :174-193`, carried `:250-251` (matches) | right before `Ok(Job)` at `:247`; the mosaic row when `keep_mosaic` or its path is `converted_out` / `debayered_out` |
| `measured` | `upsert_pass1_row :1837-1843`, after the write (matches) | in `measure_two_pass_prepare`, for every member left `Owed` without a fresh row (correction 4); the row is read today only when not `force_fresh` (`:1337`) and must be read always |
| `fits.<plane>` | `:1991-1997`, after the write (matches) | just before `write_fits` at `:1954` |

**Approach.**
1. `db/stacking.rs`: `pub fn delete_artifact(conn: &Connection, subject: &StackSubject, group_key: &str, kind: ArtifactKind, frame_id: Option<i64>) -> Result<bool>`: `DELETE FROM
   stacking_artifacts WHERE` the six expressions of `find_artifact_sql` (`:1025-1027`), same parameters, `kind.group_key(group_key)` included; returns whether a row went.
   At every site of T1a and T1b a failed delete stops the job with `?`, never a warn and a write.
2. `stacking/run/calibrate.rs`: `#[cfg(test)] pub(super) static STAGE_ONE_JOB_CANCELLED: std::sync::Mutex<Vec<(PathBuf, i64)>>`. A calibrate worker (`:152`) or prepare worker
   (`prepare.rs:528`) whose output lies under a listed dir and whose frame id matches returns `Err(CANCELLED_MARKER.to_string())` without work: the shape of "the first unfinished
   job"; the cancel flag stays clear, so later jobs still write.
3. Calibrate: the mosaic row is read for every frame (the `if want_mosaic` at `:399` goes; the reuse check keeps `!want_mosaic ||`). Right before
   `Ok(CalibrateResolved::Job(..))`, under one connection, `delete_artifact(.., Calibrated, ..)?` and `delete_artifact(.., CalibratedMosaic, ..)?` when `want_mosaic` OR the
   mosaic row's path is `out`: with the debayer off an OSC frame's calibrated file takes the mosaic's name `c_<stem>.fits` (`:546-561`, `export/models.rs:442-446`), and a row
   left there at the old hash over the new pixels is reused by a later Bayer run (correction 2). The job keeps `old_calibrated` / `old_mosaic`; `commit_calibrated` passes them
   to `replace_frame_level_file` as today (`:844-849`). `Reused` and `Excluded` delete nothing.
4. Prepare: the same before `Ok(PrepareResolved::Job(..))` (the mosaic row is read always already); `CalibratedMosaic` when `keep_mosaic` OR its path is `converted_out` or
   `debayered_out` (`prepare.rs:209-210`). `commit_prepared` unchanged.
5. Pass 1: `MeasureMember` gains `old_measured: Option<StackingArtifactRow>`; `measure_two_pass_prepare` reads the `measured` row for every member (move the find at `:1348-1354`
   out of `if !force_fresh`; its freshness use stays) and, after `need_pass1` (`:1415-1417`), deletes it (`?`, one connection) for every member left `Owed` without a fresh row
   (not `spill_pending`): each member of `need_pass1`, and one whose own β came from its `metrics` row (`:1361-1366`) while its `measured` row is not fresh at `pass1_key`
   (`:1378`, else arm `:1402`). That member is not in `need_pass1`; when it needs pass 2 its worker runs pass 1 and writes `layout_spill` (`:1758-1762`) under another key than
   the row's, so the spec's "the fallback writes under the SAME key" (§4.2) is wrong for it (correction 4). `upsert_pass1_row(rc, frame, spill, key, old:
   Option<&StackingArtifactRow>)` stops reading (`:1835-1843` go); callers `:1516`, `:1793` pass the member's `old_measured`. The fallback writes under the row's own key only
   for a `spill_pending` member refused at `:1673-1688`, whose row is kept; it passes the carried row, so a legacy `measured/<key>/` file is retired (R6 M1).
6. `.athf`: in `persist_measurement` move the row read before `write_fits`, then `delete_artifact(..)?`, write, on `Err` the existing warn and `continue` (no row), upsert,
   `replace_frame_level_file(old)`. Rewrite the C-30 comment: a failed write leaves no row; consumers fall back as for a missing row.

**Tests** (logs through `telemetry::capture_logs`, counts through `fan_out_items`).
- `delete_artifact_is_a_point_delete_on_the_artifacts_key` (`db/stacking.rs`): rows for two subjects and two frames, a `calibrated` row and an `ln` row under a group key; `(set
  A, "any_key", Calibrated, f1)` returns true and removes that row only; again returns false; the `ln` row goes only under its key.
- `a_cancelled_recalibration_leaves_no_row_over_its_files` (`calibrate_tests.rs`, `Pass1Rig::new()`, `calibrated_plane_bits` `:1160`): run A; run B with `calibration.flat_norm`
  flipped and the hook on the second frame ends `Cancelled`. Premises: frame 1's planes under B differ from A's (else pick another pixel-moving option); every file B wrote has
  the size A's row recorded (as `register_tests.rs:866-870`). Frame 1's row carries B's hash; frames 2.. have no `calibrated` row while their files exist; the gate at A's config
  counts 0 `calibrated_cached`; a run at A's config calibrates every frame and the planes equal A's bit for bit. Fails today (frames 3.. keep A's row).
- `a_debayer_off_run_retires_the_mosaic_row_it_overwrites` (`calibrate_tests.rs`, fixture and drizzle settings of
  `turning_bayer_on_regenerates_the_mosaic_of_an_already_calibrated_frame` `:1512`: `seed_osc_star_group`, `add_master_dark_and_flat`, drizzle on at scale 1, `KeepAll`): run A
  (debayer on, `drizzle.bayer` on): rows at `c_<stem>_d.fits`, mosaic rows at `c_<stem>.fits`; run B (debayer off, `calibration.flat_norm` flipped; premise: B's
  `c_<stem>.fits` has the size A's mosaic row recorded): no `calibrated_mosaic` row is left; run C (A's calibration, `drizzle.bayer` off); run D (A's config): the calibrate
  fan-out covers every frame. Fails today (run D reuses A's mosaic rows over B's pixels).
- `a_cancelled_prepare_leaves_no_row_over_its_files` (`prepare_tests.rs`, an attested fixture whose 16-bit lights Prepare converts, as `an_attested_set_stacks_without_masters`
  `:668`): run A; run B with `calibration.debayer_osc` flipped (moves the prepare hash) and the hook on the second frame; the same row assertions; a run at A's config prepares
  every frame and leaves every row at A's hash. Fails today on "no row". It proves the delete order only, not wrong pixels: the prepare hash moves only with `debayerOsc`
  (`stacking/config.rs:966-972`), and that flip writes the same bytes for a mono light.
- `a_cancelled_first_pass_leaves_no_row_of_the_old_key` (`measure_tests.rs`, `Pass1Rig`, `PASS1_CANCEL_AFTER` = 1 under the rig's working dir): run A; run B with
  `measurement.max_stars + 1` (moves `pass1_hash_for`) is cancelled; every `measured` row carries B's key or is absent, none A's. Fails today.
- `a_failed_fits_write_leaves_no_row` (`measure_tests.rs`, beside `a_fits_artifact_write_failure_degrades_to_detection` `:1806`, its failure trigger): run 1; run 2 at another
  measurement hash with one frame's `.athf` write failing: `FITS_WRITE_FAILURE_LOG` names it and that frame has no `fits.0` row. Fails today.
- The delete of a `metrics`-β member's `measured` row matters only on a kill (a cancel upserts first, `:1790-1800`): reviewed, not tested.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- db::stacking stacking::run::calibrate stacking::run::prepare stacking::run::measure`; the two checks.

**Acceptance.** Every stage-1, pass-1 and `.athf` writer deletes its row (and a mosaic row whose file it overwrites) before it replaces the file; a failed delete stops the
job; a cancel or a failed write leaves no row over another config's file; legacy files are still retired; no hash moves. **Coupling.** `delete_artifact`: owner T1a, used by
T1b. The write order of these four kinds: owner T1a, relied on by T2. `STAGE_ONE_JOB_CANCELLED`: T1a only. **Not in this task.** `registered`, `ln`, `ln_reference` (T1b);
the identity check (T2).

## T1b: write order at Register and LN

**Problem.** The register and LN fan-outs write files in workers and record rows afterwards; a cancel between leaves old-key rows over new files. The LN reference is rewritten on
the run thread before its row is.

**Files.** `stacking/run/register.rs`: `persist` `:701`; reuse branch `:858-931` (freshness `:887-896`, queue `:905-913`); `to_register` push `:961-978`; rewrite fan-out
`:1073-1155` (`fan_out` `:1090`, record `:1134-1140`); main fan-out `:1243` (a dry pass writes nothing, `:1315`); record `:1514-1527`; `record_registered_artifact` `:2136-2181`
(row read `:2157-2164`); `registered_artifact_is_fresh` `:2183-2204`. `stacking/run/normalize.rs`: reference row `:504-516`; `ln` rows `:785-809`; freshness loop `:971-1068`;
progress `:1075-1084`; fan-out `:1138`; `ln` upsert `:1287-1300`; `build_and_write_ln_reference` `:1679-1870` (`write_reference` `:1766`, upsert `:1853-1869`).

**§4.2 against the code.** `registered`: row read after the write (spec `:2158`, matches); queues `:905` and `:967` match, but `:967` is also reached by the dry pass, which
writes no file, so its delete is `persist`-only (correction 3). `ln`: no old-row read for replacement (matches; the freshness read is `:785-795`). `ln_reference`: the file is
written at `:1766`; `:1836-1870` is its identity and upsert (correction 2).

**Approach.**
1. `registered_artifact_is_fresh` becomes `fn registered_row(rc, group_key, frame_id) -> anyhow::Result<Option<StackingArtifactRow>>`; `:887` judges `is_fresh` on it (the error
   arm keeps its warn).
2. `register_group_pass` keeps `let mut old_registered: HashMap<i64, StackingArtifactRow>`. At the rewrite push (`:905`): delete the row (`?`), keep it. When the read failed
   (the error arm of `:887-896` still queues the rewrite), the delete runs by key all the same (a point delete needs no row) and nothing is kept. At the `to_register` push
   (`:967`), only when `persist`: read the row, delete it (`?`), keep it.
3. `record_registered_artifact(rc, group_key, frame_id, hash, out, old: Option<&StackingArtifactRow>)`: the read at `:2157-2164` goes; callers `:1134`, `:1514` pass
   `old_registered.get(&frame_id)`.
4. LN sidecars: after the freshness loop, before the progress at `:1075`, one connection, `delete_artifact(.., &group.key, ArtifactKind::Ln, Some(frame_id))?` for every member in
   `needs_normalize`. `ln_global` (payload-only) is untouched.
5. LN reference: right before `write_reference` (`:1766`), `delete_artifact(.., &group.key, LnReference, None)?`.
6. `#[cfg(test)] pub(super) static LN_CANCEL_BEFORE_FAN_OUT: Mutex<Vec<PathBuf>>`, checked right before `:1138`: when `rc.layout.root` starts with a listed dir, it sets
   `rc.cancel`.

**Tests.**
- `a_cancelled_rewrite_does_not_leave_another_runs_pixels_fresh` (`register_tests.rs:824`): scenario and end unchanged (gate stale, 4 rewrites, master bit-identical); the premise
  block `:861-871` becomes "`rows_b` is empty and B's four files exist", and its size premise (`:866-870`) stays, against the sizes `rows_a` recorded: without it the gate
  assertion (`:878`) could pass on a size change. Fails before T1b.
- `a_dry_pass_leaves_the_registered_rows_alone` (`register_tests.rs`): after a `RegisterRig` run, `register_group_pass(.., persist = false, ..)` against another reference (every
  frame a miss) leaves `registered_snapshot` unchanged. Green before and after; guards step 2's condition.
- `a_cancelled_normalization_leaves_no_row_of_the_old_hash` (`normalize_tests.rs`, `LnRig`): run 1 with LN; run 2 with `normalization.local.star_selection.min_peak_snr` moved and
  the hook set ends `Cancelled`; no `ln` row of the group carries run 1's hash. Fails today. It proves the delete order only, not wrong pixels: the hook cancels before any file
  is written.
- The LN reference has no cancel point between its write and its upsert (both on the run thread): reviewed, not tested.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::run::register stacking::run::normalize stacking::run::two_pass`; the two checks.

**Acceptance.** All seven write sites of §4.2 follow the order, and a failed delete stops the job; the dry pass deletes nothing; Blink reads a row a cancelled later run
deleted as `missing` (no code change, `api/stacking_aligned.rs:252`). **Coupling.** Uses `delete_artifact` (T1a); owns the register and LN write order. **Not in this task.**
The identity check (T2); payload-only kinds.

## T2: own files by key + size; Blink; the in-place rule

**Problem.** `file_matches_identity` compares the RFC 3339 time, so a copy that rounds it recomputes everything. With T1 in, key + size is enough for own files (FI-2). An
in-place Prepare row can name a light's OLD path after a relocate, where another same-size file may now sit (§4.1).

**Files.** `stacking/plan.rs:1049-1098` (`file_matches_identity`), `:1100-1124` (`is_fresh`), gate frame loop `:3776-3837`, folders gate `:3384-3448`, test `:4191-4235`;
`render_modified_at` (`:1045-1047`) stays. `stacking/paths.rs` beside `subject_layout` (`:44-70`). `stacking/run/prepare.rs:105` (`catalog_path`), `:143-172` (action,
`keep_mosaic`), `:194-204` (reuse). `api/stacking_aligned.rs:239-246` (Blink), test `:696-713`. `stacking/run/measure_tests.rs:1015`. `docs/logging/README.md:181`.
Comments that become false: `stacking/run/calibrate.rs:440-443`, `stacking/plan.rs:1100-1108`, `stacking/run/measure_tests.rs:793-796`.

**Approach.**
1. `pub(crate) fn file_matches_identity(path: &Path, size: Option<i64>) -> bool`: one stat; `NotFound` is `false` with no log; any other error keeps the warn "cached file cannot
   be stat-ed; counted as changed"; then `size == Some(len)`. The time branch and its `debug!` (`:1075-1097`) go. Doc: own files are judged by key + size (FI-2); the write order
   (T1) closes R7 b.
2. `is_fresh`: `file_matches_identity(Path::new(p), artifact.size)`. Blink: `Some(path) if size.is_some() && !file_matches_identity(&path, size) => Replaced`.
   `measure_tests.rs:1015`: the two-argument call.
3. In-place rule, run side (`resolve_prepare_job`, the action is known before the reuse check): `expected_calibrated = (zip_markers.is_none() && matches!(action,
   PrepareAction::InPlace)).then(|| catalog_path.clone())`; `expected_mosaic = (zip_markers.is_none() && keep_mosaic && matches!(action, PrepareAction::Write { read:
   PrepareRead::InPlaceFits, .. })).then(|| catalog_path.clone())`. Reuse (`:194-204`) also requires each row's `path` to equal its expected path when that is `Some`.
   `catalog_path` is the `prepare_input` path before any extraction (`:105`); a zip frame used in place is detached into `prepared/`, an own file. A miss becomes a Job (T1a
   deletes the row); `execute_prepare` writes nothing for `InPlace`; `commit_prepared` upserts the current path; `replace_frame_level_file` keeps the old path (outside
   `prepared_root`).
4. Gate side:
   - `stacking/paths.rs`: `pub(crate) fn run_spelled_working(working: &Path) -> Option<PathBuf>`: `None` when the folder does not exist, else
     `normalize_path(canonicalize(working))`; a canonicalize error is a `warn!` and `None`. Writes nothing.
   - In `build_plan`, after the folders gate and only when `subject_prepares(subject, &origins)`: `prepare_roots` = the subject root under the validated working folder as
     configured, plus the one under `run_spelled_working` when it differs (rows carry the run's spelling in the app, the configured one in tests); empty without a working folder.
   - `pub(crate) fn prepare_row_names_its_input(row_path: &Path, input: &Path, roots: &[PathBuf]) -> bool`: `row_path == input`, or it lies by components under `<root>/prepared`
     or `<root>/calibrated` of a root.
   - In the frame loop, for a Prepare subject, `calib_fresh` (`:3809-3812`) and the mosaic half (`:3826-3829`) also require `prepare_row_names_its_input(row,
     &prepare_input(origin, f).path, &prepare_roots)`.
5. `docs/logging/README.md:181`: the identity is the size; the warn stays; the `debug!` and its fields `recorded`, `current` leave this site.
6. The three comments: `calibrate.rs:440-443` ("keys on the config hash plus (size, mtime)": the hash plus the size; a truncated file is a size miss); the `is_fresh` doc
   `plan.rs:1100-1108` (key + size); the doc of `measure_pass1_artifact_survives_a_touched_source` (`measure_tests.rs:793-796`): the touched file is reused, not recalibrated.

**Tests.**
- `identity_is_the_size_never_the_time` (replaces `identity_refusal_by_mtime_says_why`): same size, time +5 s → true and no log (fails today); time truncated to whole seconds →
  true; one byte more → false; `NotFound` → false, no log; `"a\0b"` → false and a WARN "cannot be stat-ed".
- `aligned_file_is_judged_by_its_size_not_its_time` (replaces `aligned_file_that_is_not_the_recorded_one_is_replaced`): later time → `Available` (fails today); a byte appended →
  `Replaced`; its `registered` row deleted → `Missing`.
- `an_in_place_row_at_another_path_than_its_light_is_stale` (`prepare_tests.rs`, `Pass1Rig::attested()` of `stacking/run/tests.rs:641`: four float lights used in place; set the
  working and output settings as `normalize_tests.rs:788-793`). After run 1: copy light 0 to a new path with its time kept, `UPDATE files SET path, filename` (as
  `api/missing_files.rs:263-266`), and leave at the old path ANOTHER file of the same size carrying the row's recorded time. The gate's `calibrated_cached` is 3 of 4. Recompute
  the rig's plan groups (the run takes `GroupFrame.path` from them); run 2 writes nothing under `prepared/`, the row names the new path, pass 1 logs no read. Fails today (size
  and time both match the other file) and with step 1 alone.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::plan stacking::paths stacking::run api::stacking_aligned`; the two checks (`stacking::run` whole: `is_fresh` serves every stage).

**Acceptance.** An own file reads fresh at equal key and size whatever its time; Blink follows; an in-place row that does not name the current input reads stale in the gate and
the run, and the run re-points it. **Coupling.** `file_matches_identity(path, size)`: owner T2; used by `is_fresh`, Blink, T8b. `run_spelled_working`,
`prepare_row_names_its_input`, `prepare_roots`: owner T2; T8c adds the old roots. Relies on T1's order. **Not in this task.** Inputs (T3, T4); the rebase (T8).

## T3: the sample rule, the memo, the book type

**Problem.** The run and the gate need one rule for a light's or master's identity: size + sampled hash, trusted from the catalog while the row still describes the file, read
once otherwise, banked when safe (§4.4, §4.5).

**Files.** `stacking/source_identity.rs` (new; `pub(crate) mod source_identity;` in `stacking/mod.rs:62-96`); `db/operations.rs` (beside `:7565`);
`duplicates/backfill.rs:347-364`.

**Approach.**
1. `duplicates/backfill.rs`: `pub(crate) fn metadata_matches_row(meta: &std::fs::Metadata, size: i64, modified_at: &str) -> bool` (the body of `:355-361`); `disk_matches_row`
   calls it after its one stat.
2. `db::bank_content_hash(conn, file_id, size, modified_at, hash) -> Result<bool>`: the SQL of §4.5 verbatim.
3. `stacking/source_identity.rs`:
   ```rust
   pub(crate) enum SampleRead { CatalogOnly, Disk }
   pub(crate) struct ZipEntryStamp { pub crc32: Option<u32>, pub modified_at: String }
   pub(crate) struct InputRow { pub file_id: i64, pub path: PathBuf, pub size: i64, pub modified_at: String,
                                pub content_hash: Option<String>, pub zip_entry: Option<ZipEntryStamp> }
   pub(crate) enum InputIdentity { Sampled { size: i64, sample: String }, Zip { size: i64, token: String }, NotSampled(&'static str) }
   pub(crate) struct BankSample { pub file_id: i64, pub size: i64, pub modified_at: String, pub sample: String }
   pub(crate) struct Sampling { pub identity: InputIdentity, pub bank: Option<BankSample> }
   pub(crate) fn input_row(conn: &Connection, path: &str) -> rusqlite::Result<Option<InputRow>>
   pub(crate) fn input_identity(row: &InputRow, read: SampleRead, cancel: &AtomicBool) -> Sampling
   ```
   - `input_row`: `SELECT id, path, size, modified_at, content_hash, archive_zip_path, archive_path_in_zip, archive_entry_crc32 FROM files WHERE path = ?1` (`path` is UNIQUE);
     `zip_entry` when both zip columns are set; `Zip.token` is `format!("{:08x}", crc as u32)`, else the row's `modified_at`.
   - `input_identity` follows §4.4's order per mode. `Disk`: one `std::fs::metadata`; `NotFound` → the `CatalogOnly` answer; another error → `NotSampled("unreadable")` after
     `warn!(path, error, "input file cannot be stat-ed")`; trusted when `content_hash.is_some() && metadata_matches_row(..)`; then the memo; then, unless `cancel` is set
     (`NotSampled("cancelled")`), `compute_xxhash` and a second stat: a moved size or time → `NotSampled("changed while it was sampled")`; a read error →
     `NotSampled("unreadable")` after a `warn!`; `bank` only when the row matched at both stats and `content_hash` is NULL. `CatalogOnly` with nothing → `NotSampled("not sampled
     yet")`.
   - Memo: `static SAMPLE_MEMO: LazyLock<Mutex<HashMap<PathBuf, MemoEntry>>>`, `MemoEntry { row_size, row_time, live_size, live_time, sample }`, cleared at 65 536 entries, never
     persisted; `Disk` matches the live stamps, `CatalogOnly` the row stamps.
   - `#[cfg(test)] pub(crate) static SAMPLE_READ_LOG: Mutex<Vec<PathBuf>>` (one entry per read) and `SAMPLE_TOUCH_AFTER_READ: Mutex<Vec<PathBuf>>` (moves the time +10 s right
     after the read), on the model of `PASS1_LOG` / `PASS1_TOUCH_AFTER`.
4. The book, same module:
   ```rust
   pub(crate) struct SourceSamples { mode: BookMode, by_path: HashMap<String, InputIdentity> }
   enum BookMode { CatalogOnly, Run { resolved: bool } }
   pub(crate) enum BookMiss { NotInCatalog, OutsideRun, Unresolved }
   impl SourceSamples {
       pub(crate) fn catalog_only() -> Self;   pub(crate) fn for_run() -> Self;   pub(crate) fn is_resolved(&self) -> bool;
       pub(crate) fn identity(&mut self, conn: &Connection, path: &str) -> Result<InputIdentity, BookMiss>;
       pub(crate) fn read_rows(conn: &Connection, inputs: &[String]) -> anyhow::Result<Vec<(String, Option<InputRow>)>>;
       pub(crate) fn sample_rows(rows: Vec<(String, Option<InputRow>)>, cancel: &AtomicBool) -> Vec<(String, Sampling)>;
       pub(crate) fn bank(conn: &mut Connection, samplings: &[(String, Sampling)]) -> anyhow::Result<usize>;
       pub(crate) fn fill_run(&mut self, samplings: Vec<(String, Sampling)>);
   }
   ```
   `identity` in `CatalogOnly` reads the row lazily (no file I/O) and caches it; no row is `NotInCatalog`; in `Run` it only looks up. `sample_rows` holds no
   connection; a row-less input becomes `NotSampled("not in the catalog")`. `bank` is one transaction; a failed UPDATE is a `warn!`, the sample stays in the memo.

**Tests** (all new; `stacking/source_identity.rs` unless named; temp files, an in-memory catalog with `init_db`): `a_matching_row_is_trusted_without_a_read` (a banked
`"not-the-file"` comes back, `SAMPLE_READ_LOG` empty); `a_coarser_time_reads_again_and_finds_the_same_sample` (whole seconds, then 100 ns: the `compute_xxhash` value, `bank`
`None`, no write); `an_unbanked_matching_row_is_read_and_banked` (`bank` set and written; a second call reads nothing); `a_zip_row_is_its_crc_with_no_stat` (a CRC row at a
missing path → `Zip { token: "0000002a" }`; no CRC → the row's `modified_at`); `an_offline_input_keeps_its_banked_sample` (file removed → the banked value; NULL →
`NotSampled("not sampled yet")`); `catalog_only_never_reads_and_finds_the_memo_after_a_disk_pass` (an unbanked mismatching row: `NotSampled`, then after one `Disk` pass the read
sample; one read in all); `a_file_changed_during_the_read_is_not_sampled` (`SAMPLE_TOUCH_AFTER_READ`); `an_edit_inside_a_window_is_seen_and_one_outside_is_not` (4 MiB file: a
byte at 2 MiB moves the sample, a byte at 1 MiB does not; §6, pinned); `bank_content_hash_writes_only_over_a_matching_null_row` (`db/operations.rs`: once; not after a
re-parse-like UPDATE of `modified_at`, not over a value, not on a zip row). The existing `disk_matches_row` tests stay green.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::source_identity db::operations duplicates::backfill`; the two checks.

**Acceptance.** The rule, the memo and the book exist with no production caller; no hash moves. **Coupling.** Owns `metadata_matches_row`, `bank_content_hash`, `input_row`,
`input_identity`, `InputIdentity`, `SourceSamples`, `BookMiss`, the `NotSampled` reasons and the two test hooks; T4a and T4b consume them. **Not in this task.** Any caller; the
hash's token strings (T4b).

## T4a: the run's book

**Problem.** A run must sample its inputs once, never holding a connection across a file read, before any stage-1 hash, and bank what it may.

**Files.** `stacking/plan.rs:546-561` (`HashMemo`) and its constructors' callers `:3517`, tests `:4394`, `:9095`, `:9117`, `:9247`, `:11128`, `:11253`;
`stacking/project_source.rs:635`; `stacking/run/mod.rs:1160`, `:1796`, `:1853`; `stacking/run/prepare_tests.rs:1059`, `:1120`; `stacking/run/calibrate.rs:1131-1135`; stage
entries `stacking/run/measure.rs:125`, `reference.rs:290`, `register.rs:1872`, `output.rs:2719`. `stacking/test_fixtures.rs` (`bank_samples`) and the tests that form a
stage-1 hash before any run: `stacking/plan.rs:4367`, `:6659`, `:6748`, `one_group_calibration_hashes` `:6963`, `:7166`, `:7663`, `:7931`, `:8270`, `:9078`, `:11087`,
`:11379`; `stacking/project_source.rs:575`. Tests in `stacking/run/{calibrate_tests,masters_tests}.rs`.

**Approach.**
1. `HashMemo` gains `samples: SourceSamples`. `HashMemo::new` gives way to `for_gate(origins)` (a catalog-only book) and `for_run(origins)` (an unresolved run book), plus
   `samples_resolved(&self) -> bool` and `set_run_samples(&mut self, book)`. The gate, the plan tests and `project_source.rs:635` use `for_gate`; `run/mod.rs` and
   `prepare_tests.rs` use `for_run`. The hash ignores the book in T4a.
2. `stacking/run/calibrate.rs`: `pub(super) fn ensure_source_samples(rc: &mut RunContext) -> Result<(), RunError>`:
   - return when `rc.memo.samples_resolved()`;
   - inputs: every frame of `rc.plan_groups`, manually excluded ones included (correction 8): a catalog origin gives `frame.path`; a non-attested one also every path of
     `export::applied_master_paths(conn, frame_id, &rc.config.calibration)` (an `Err` is a `debug!`; the hash reports it); a project gives nothing. Deduplicated, sorted.
   - phase 1 under a connection: `read_rows`; phase 2 without one: ONE event `rc.progress(Stage::Calibrate, None, 0, n, 0, 0, None, Some(INPUTS_MESSAGE.to_string()))`
     (`n` the input count, sent only when `n > 0`: `total = 0` reads 100 %; `pub(super) const INPUTS_MESSAGE: &str = "checking the inputs";`), then `sample_rows`, whose
     `cancel` is checked per file, then `rc.check_cancel()?`. No per-file tick: `current == total` forces an emit (`run/mod.rs:552`), the row would reach 100 % and the stage's
     own first event (`calibrate.rs:1179-1188`) put it back to 0, failing `stage_sequence_and_monotonic` (`run/tests.rs:1161-1180`) and the pin at
     `calibrate_tests.rs:1328-1338`. Phase 3 under a connection: `bank` (an `Err` is a `warn!`, the run goes on); `fill_run`; `set_run_samples`.
   - `info!(run_id, count, sampled, banked, duration_ms, "stacking inputs checked")`; `sampled`, `banked` are new fields: one logging README bullet.
3. `ensure_source_samples(rc)?` is the first statement of `stage_calibrate_with` (before the Prepare dispatch at `:1133`) and of `stage_measure`, `stage_reference`,
   `stage_register`, `stage_output`: idempotent, so a test that enters at a later stage or resets `rc.memo` still gets a book.
4. `test_fixtures.rs`: `pub(crate) fn bank_samples(conn: &Connection) -> usize`: the content index's effect on a fixture catalog: every non-zip row with NULL `content_hash` gets
   `compute_xxhash(path)` when its file matches the row and `"unwritten-<file_id>"` when no file exists. The tests that form a stage-1 hash before any run (Files) call it
   after seeding; a no-op while the hash ignores the book, so T4b moves the hash with every gate test already banked. No run rig (`Pass1Rig`, `RegisterRig`, `LnRig`,
   `RegroupRig`) ever calls it: a run samples and banks its own inputs, and that path must stay exercised.

**Tests** (`calibrate_tests.rs`, `Pass1Rig::new()`): `a_run_banks_the_samples_of_its_inputs` (NULL `content_hash` for the 4 lights and 2 masters before; after `stage_calibrate`
each equals `compute_xxhash` of its file; the line reports `count=6`, `banked=6`; fails before); `a_second_run_reads_no_input` (`SAMPLE_READ_LOG` gains 6 entries under the
fixture dir in run 1, none in run 2); `the_input_check_says_so_and_stops_on_cancel` (a `Recording` emitter sees ONE Calibrate progress "checking the inputs", at `current = 0`,
before the stage's own first event; with `rc.cancel` set first, `stage_calibrate` returns `Cancelled` and nothing is banked).
- `a_master_rebuilt_in_the_run_is_sampled_after_the_build` (`masters_tests.rs`, fixture of `deleted_masters_are_rebuilt_by_stage_masters` `:19`): both masters'
  `content_hash` set to `"before-the-rebuild"`, their files deleted; after the run each equals `compute_xxhash` of its rebuilt file (the resync NULLs the old value, the book
  runs after `stage_masters`, `run/mod.rs:1545-1552`, spec §4.4). Fails if the book is moved before `stage_masters`.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::run stacking::plan stacking::project_source`; the two checks.

**Acceptance.** Every run banks its inputs' samples once, a rebuilt master after its build, says so in one event, honours a cancel; no hash moves. **Coupling.** Owns the
`HashMemo` constructors, `ensure_source_samples`, `INPUTS_MESSAGE`, the log line and `bank_samples`; T8b puts `rebase_working_rows` before `ensure_source_samples` (§4.10:
rebase, book, then the Prepare dispatch). **Not in this task.** The book in the hash (T4b).

## T4b: the stage-1 hash on samples; masters without the path; the pins

**Problem.** Stage 1 folds `(file_id, size, modified_at)` for lights and `<path>|<size>|<mtime>` for masters (`stacking/plan.rs:410-533`). It must fold §4.3's tokens, keep the
project hash's bytes, and re-pin every literal it moves in the same (squashed) commit.

**Files.** `stacking/config.rs:918-955` (`SourceIdentity`, `stage_hash`), tests `:1874-1905`, `:2289-2340`. `stacking/plan.rs:402-454` (replaced), `:459-533`
(`prepare_hash_for_catalog`, `prepare_hash_for_project`, `calibration_hash_for`), `:546-650` (`HashMemo`: dispatch `:564-580`, log `:594-604`). `stacking/source_identity.rs`
(renderers). `stacking/run/masters.rs:35-42` (doc). Tests in `stacking/plan.rs`: pins `:8722`, `:8831`, `:8927`; formulas `:8963`, `:9220`; project `:9261`; zip master
`:6208` (its `master_identity_in` part `:6256-6270`); the tests T4a banked (T4a's Files) take the book argument; `stacking/run/calibrate_tests.rs` (one new test).

**Approach.**
0. Before any edit: add to `the_prepare_subtree_and_the_project_hash_are_pinned` an `assert_eq!` of `prepare_hash_for_project(&cfg, &"ab".repeat(32), 4096)` against a literal
   recorded by running it once. It must not move below.
1. `config.rs`: `#[derive(Serialize)] #[serde(untagged)] pub enum SourceIdentity { Sampled { file_id: i64, size: i64, sample: String }, Project { file_id: i64, size: i64,
   modified_at: String } }` with `file_id()`, `size()`, `third()`; `stage_hash` sorts by file id, size, then the third field. `Project` serializes
   `{"file_id":0,"modified_at":"","size":N}` byte for byte (serde_json's sorted map, as the struct did). The two stability tests use `Sampled`.
2. `source_identity.rs`: `InputIdentity::master_token(&self) -> Result<String, &'static str>` (`"{size}|sample:{sample}"`, `"{size}|zip:{token}"`) and `light_source(&self,
   file_id) -> Result<SourceIdentity, &'static str>` (`Sampled { file_id, size, sample }`; a zip light's sample is `"zip:{token}"`, which no 16-hex disk sample can equal).
3. `plan.rs`:
   - `pub(crate) enum StageOneError { NotSampled { path: String, why: &'static str }, Other(ApiError) }`; `Display` `"{path}: {why}"` or the inner text; `From<ApiError>`;
     `From<StageOneError> for ApiError` (`Internal`). `BookMiss::NotInCatalog` → `NotSampled { why: "not in the catalog" }`; `OutsideRun` → `Other(Internal("{path} is outside the
     run's inputs"))`; `Unresolved` → `Other(Internal("the run's inputs were not checked before frame {id} was hashed"))`.
   - `master_identity`, `master_identity_in` go. `calibration_hash_for(conn, cfg, frame, samples: &mut SourceSamples, _divisors) -> Result<String, StageOneError>`: master tokens
     from `samples.identity(conn, path).map_err(..)?.master_token()`, the light from `light_source(frame.file_id)`. `prepare_hash_for_catalog(conn, cfg, frame, samples)
     -> Result<String, StageOneError>`. `prepare_hash_for_project` builds `SourceIdentity::Project`. A `BookMiss` carries neither the path nor the frame id its text needs, so
     the two call sites (`calibration_hash_for`, `prepare_hash_for_catalog`) turn it into a `StageOneError` with `map_err`, passing both; no blanket `From<BookMiss>`.
   - `hash_by_origin` passes `&mut self.samples`. `calibration_hash`: a `NotSampled` logs `debug!(frame_id, path, reason, "stacking: stage-1 input not sampled; treating as
     stale")`; other errors keep the `warn!` of `:597-601`. `calibration_hash_checked` still returns `ApiError`.
   - Docs `:402-454`, `run/masters.rs:35-42`: a master is named by size + sample, never its path (FI-3); a rebuild NULLs `content_hash` and is sampled again.
4. Tests: the ones T4a banked pass `&mut SourceSamples::catalog_only()`; run tests that read `rc.memo` before a stage call `ensure_source_samples(&mut rc)` first. No run rig
   (`Pass1Rig`, `RegisterRig`, `LnRig`, `RegroupRig`) ever calls `bank_samples`: a red run test is fixed in the run, never with the helper.
5. Pins (a dated comment keeps the old values, as the file does). Golden inputs: masters `["100|sample:00000000000000d1", "200|sample:00000000000000f1"]`, source `Sampled {
   file_id: 42, size: 123_456, sample: "0000000000004242" }`; `calib`, `meas`, `reg`, `ln` move on all four lines, `ln_ref`, `ln_cal` do not. The same inputs in
   `fit_stars_none_leaves_every_stage_hash_literal_unchanged` (re-pin `meas`, `reg`, `ln`). `pass1_hash_is_never_a_metrics_key` takes the new default `calib` and re-pins its key.
   The two formula tests are rewritten over the book.

**Tests.** The re-pinned goldens; the rewritten formula tests; the project literal of step 0, unchanged.
- `a_zip_master_token_is_its_size_and_crc` (rewrite of `:6256-6270`): the book's token differs before and after the zip rewrite, and neither holds the path.
- `a_relocated_master_keeps_every_lights_stage_one_hash` (FI-3): after `bank_samples`, hash a frame; relocate the master row's `path`; then a copy of its row under another root
  relinked in its place (a rescan): equal hashes.
- `the_gate_reads_an_unsampled_input_as_stale_quietly`: a `calibrated` row at the right hash, `content_hash` NULLed: Calibrate stale, a DEBUG line, no WARN.
- `an_input_changed_while_sampled_excludes_its_frame_and_the_rest_stack` (`calibrate_tests.rs`, `Pass1Rig::new()`): `SAMPLE_TOUCH_AFTER_READ` lists one light that is not the
  reference; the run ends `done`, that frame is excluded with a reason holding "changed while it was sampled", and the other three are integrated.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::config stacking::plan stacking::project_source stacking::source_identity stacking::run api::stacking api::stacking_aligned api::stacking_project_e2e`; the two checks.

**Acceptance.** Every frame set's stage-1 hash and everything chained on it moves once (FI-4); a project's does not; a moved master changes no hash; the gate reads no file; an
unsampled input excludes its frame with the reason; every literal moved is re-pinned in this commit. **Coupling.** Owns the hash shape (`SourceIdentity`, `master_token`,
`light_source`), `StageOneError` and its text; uses T3's book and T4a's constructors and `bank_samples`. **Not in this task.** `strict_mtime` (T4c); FI-6 (T5).

## T4c: `strict_mtime` goes; the coarse-time and in-window regressions

**Problem.** Pass 1 still compares the time of an in-place foreign file (§4.7); the sample in the prepare hash now covers it. The regressions need the regroup fixture as a rig.

**Files.** `stacking/measure.rs:1969-1991` (`from_record`), `:2101-2112` (`check_pass1_head`), `:2126-2148` (`check_stamp_len`), test call `:3991`.
`stacking/pass1_artifact.rs:294-305` (`read_pass1`), test `:615-680`, calls `:371`, `:422`, `:470`, `:602`, `:628-631`, `:646-657`, `:666`, `:677`.
`stacking/run/measure.rs:1144-1149`, `:1170-1172`, `:1184-1187`, `:1326`, `:1391`, `:1680`, `:1707-1722`, `:1742-1746`. `stacking/run/measure_tests.rs:817-866`.
`stacking/run/normalize_tests.rs:745-934`.

**Approach.**
1. Remove `MeasureMember.strict_mtime`, `owns_calibrated_file`, and the `strict_mtime` parameter of `validate_pass1_spill`, `check_pass1_head`, `read_pass1`,
   `FramePass1::from_record`, `check_stamp_len` and the pass-2 job tuple. `check_stamp_len` compares the length only; doc: the key covers an in-place foreign file (an attested
   light's sample is in the prepare hash; a replica has blake3 and `landed_unchanged`). `.athm` keeps `modified_ns`; no version bump.
2. Extract `struct RegroupRig` (in `normalize_tests.rs`) from `regroup_reuses_every_frame_level_stage`: `fn new(bayerpat: Option<&'static str>) -> Self` (the six Moffat lights at
   β 1.2, masters, Manual reference `light_ids[0]`, working folder `tmp/w1` and output under one tempdir so T8b can rename it), `fn cfg(&self, mode, tolerance) ->
   StackingConfig`, `fn stale(&self, &StackingConfig) -> Vec<Stage>`, `fn run(&self, &StackingConfig) -> (Vec<IntegrationGroup>, RunContext, String)`; the existing test keeps its
   assertions on it. Beside it `fn set_times(paths: &[PathBuf], f: impl Fn(SystemTime) -> SystemTime)`. `RegroupRig` never calls `bank_samples` (nor does any run rig): its
   runs sample and bank their own inputs.

**Tests.**
- `a_source_of_another_size_reads_stale_and_a_moved_time_does_not` (rewrite of `pass1_artifact.rs:619`): the strict calls go; a moved time reads fine from the whole read and the
  head check; another size is `Stale`.
- `measure_pass1_artifact_of_an_edited_in_place_source_reruns_pass_one` (`measure_tests.rs:822`): its simulated rescan also sets `content_hash = NULL`, as the scanner's re-parse
  does. Without that line it fails after step 1 (the banked sample is trusted), which is why it is there.
- `measure_pass1_artifact_of_an_edited_in_place_source_without_a_rescan_reruns_pass_one`: the same edit, the row untouched: the run samples again on the moved time; only the
  edited file re-runs pass 1.
- `coarser_times_reuse_every_frame_level_stage` (`RegroupRig::new(Some("RGGB"))`): before run 1 every light and master row has `content_hash` NULL; after run 1 the gate reads
  Calibrate fresh with no helper call. Then every light's, master's and working file's time to whole seconds, run 2; to 100 ns steps, run 3. Runs 2 and 3: calibrate, pass1,
  pass2, align fan out over 0 items; before each the gate reads Calibrate, Measure and Register fresh (run 1's banked samples). Fails on `main`.
- `an_in_window_edit_is_a_miss` (same rig): one light, a byte changed at its middle, equal size, time +5 s, no rescan: the gate before run 2 reads fresh (the documented residual,
  asserted so it stays visible); run 2 covers calibrate 1 and pass1 1.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::measure stacking::pass1_artifact stacking::run::measure stacking::run::normalize`; the two checks.

**Acceptance.** No time is compared in pass 1; a coarse copy of every time reuses every frame-level stage; an in-window edit recomputes exactly its frame. **Coupling.** Owns
`RegroupRig` and `set_times` (T5, T8b, T8c use them); needs T2 and T4b. **Not in this task.** FI-6 (T5); a moved working folder (T8).

## T5: the reference's measurement for every one-plane frame (FI-6)

**Problem.** A mono frame folds `ref_measure:` only when its reference sits in another group (`stacking/plan.rs:1286`), so a merge or split flips the term and re-registers frames
whose reference did not change.

**Files.** `stacking/plan.rs:1274-1297` (`reference_measurement_fold`), `:2042-2049` (gate), `:806-810` (doc), tests `:11026-11086`; `stacking/run/register.rs:600-631`
(`reference_measurement_term`), `:787`; `stacking/run/two_pass.rs:206-207`; `stacking/run/normalize_tests.rs`.

**Approach.**
1. `reference_measurement_fold(cfg, reference_group, prepare_path, reference_calibrated_hash, reference_group_beta) -> String`: `frame_group` and the `reference_group.key ==
   frame_group.key` branch go; it returns the reference group's `registration_measurement_fold`. A debayered-OSC reference still folds nothing.
2. `reference_measurement_term(rc, reference_frame_id, reference_hash, prepare_path)`: `group` goes; callers `register.rs:787`, `two_pass.rs:207`; the gate call drops `group`.
   Doc comments of both helpers and of `registration_hash_for`.

**Tests.**
- `registration_hash_folds_the_reference_measurement_whatever_its_group` (rename of `:11026`): the same-group assertion at `:11056` flips to non-empty; the OSC case folds
  nothing. `register_stale_follows_the_reference_measurement_for_an_osc_frame` (`:11087`) stays green.
- `a_same_group_mono_registration_hash_is_pinned`: `registration_hash_for(cfg, 7, "ref-calib", calib, meas, &reference_measurement_fold(cfg, &mono, false, "ref-calib",
  Some(1.5)))` on T4b's golden `calib` and `meas`: one literal.
- `mono_regroup_keeps_every_registration_when_the_reference_stays` (`RegroupRig::new(None)`, the Manual reference in the 60 s class): `seconds` → `stops` → `seconds`; runs 2 and
  3 fan out align and every frame-level stage over 0 items; the gate reads Register fresh before each. Fails before (run 2 re-registers the 90 s frames).

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::plan stacking::run::register stacking::run::two_pass stacking::run::normalize`; the two checks.

**Acceptance.** A mono merge or split keeps every registration when the reference and the βs stay. **Coupling.** Owns the fold signatures; uses `RegroupRig`. **Not in this
task.** The Auto re-pick, `reference.twoPass`, a moving β (by design, §4.8).

## T8a: the rebase rule, its check and its commit

**Problem.** Rows hold absolute paths, so a moved working folder recomputes everything (§4.10). The run (T8b) and the gate (T8c) need one rule for which row moves, where, and
on which check; this task writes it with no caller, as T3 does for the book.

**Files.** `stacking/rebase.rs` (new; `pub(crate) mod rebase;` in `stacking/mod.rs`); `db/stacking.rs`; tests in `stacking/rebase.rs`.

**Approach.**
1. `db/stacking.rs`: `pub fn subject_working_dirs(conn, subject) -> Result<Vec<String>>` (`SELECT working_dir FROM stacking_runs WHERE COALESCE(frames_set_id, 0) = ?1 AND
   COALESCE(project_id, '') = ?2 AND working_dir <> '' GROUP BY working_dir ORDER BY MAX(id) DESC`); `pub fn rebase_artifact_path(conn, row_id, old: &str, new: &str) ->
   Result<bool>` (`UPDATE stacking_artifacts SET path = ?3 WHERE id = ?1 AND path = ?2`).
2. `stacking/rebase.rs`:
   - `fn kind_subtrees(kind: &str) -> &'static [&'static str]`: `calibrated`, `calibrated_mosaic` → `calibrated`, `prepared`; `measured`; `fits.*` → `fits`; `registered`; `ln`,
     `ln_reference` → `ln`; else none.
   - `pub(crate) fn map_row(kind: &str, path: &Path, old_roots: &[PathBuf], current_root: &Path) -> Option<(usize, PathBuf)>`: the first old root (longest first) the path lies
     under by components (`strip_prefix`) whose tail starts with one of the kind's subtrees; `current_root.join(tail)`.
   - `pub(crate) fn old_roots(conn, subject, current_root) -> anyhow::Result<Vec<PathBuf>>`: `subject_layout` of each recorded folder (current slug); drops the current root, and
     one equal to it after `canonicalize` + `normalize_path` when both exist; deduplicated; longest first.
   - `pub(crate) struct Candidate { pub row_id: i64, pub kind: String, pub old_path: String, pub old_root: PathBuf, pub mapped: PathBuf, pub size: i64, pub modified_at: String
     }`; `pub(crate) fn candidates(conn, subject, current_root) -> anyhow::Result<Vec<Candidate>>`: per old root, the subject's rows with `path >= ?1 AND (?2 IS NULL OR path <
     ?2)` (`?1` = root + separator, `?2` = `path_prefix_upper(?1)`) and non-NULL `size`, `modified_at`; then `map_row`. No old root, no query.
   - `pub(crate) enum Check { Moved, Missing, OtherSize, OtherTime, NotAFile, Unreadable(String) }`; `pub(crate) fn check(c: &Candidate) -> Check`: one
     `std::fs::metadata(mapped)`; the time rule `within_tolerance(live, &c.modified_at)`: RFC 3339 parsed, `|live - recorded| <= MTIME_TOLERANCE_SECS` at full precision
     (unparsable → `OtherTime`).
   - `pub(crate) fn commit(conn: &mut Connection, moved: &[&Candidate]) -> anyhow::Result<usize>`: one transaction.
   - `pub(crate) fn left_warning(n: usize, root: &Path) -> String`: "{n} cached files were not found in the working folder {root} as recorded (missing, another size, or a time
     more than 2 s off); their frames are recomputed." `pub(crate) const REBASE_MESSAGE: &str = "finding the cached files";`

**Tests** (unit, `stacking/rebase.rs`, temp folders and an in-memory catalog with `init_db`).
- `rebase_maps_by_components_onto_the_kinds_own_subtree` (pure table: `/w/M31/calibrated/c_a_f1.fits` → `/n/M31/calibrated/c_a_f1.fits`; a legacy
  `fits/<group_key>/a_f1.p0.athf` keeps its sub-folder; `/w/M310/..`, `/w/M31/lights/x.fits` as `calibrated` and `/w/M31/registered/r.fits` as `calibrated` map to nothing;
  `/w/_projects/p/prepared/p_a_f1.fits` maps; `#[cfg(windows)]` `C:\w\M31\ln\g\a_f1.athln` → `\\nas\s\M31\ln\g\a_f1.athln`); `rebase_needs_the_size_and_a_time_within_two_seconds`
  (+1.9 s and -1.9 s `Moved`, for a recorded time with a fraction and a whole-second one; +2.5 s `OtherTime`; a byte more `OtherSize`; a folder `NotAFile`; nothing `Missing`;
  NULL `size` or `modified_at`: no candidate); `rebase_candidates_skip_the_current_root_in_place_rows_and_other_subjects` (also an old run folder that is the current one through
  a symlink); `rebase_moves_only_the_path_and_never_touches_the_old_folder` (other columns equal after `commit`; the old folder's walk of names, bytes and times equal);
  `rebase_update_is_a_no_op_when_the_row_moved_meanwhile`.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::rebase db::stacking`; the two checks.

**Acceptance.** The rule, the check and the commit exist with no production caller; no hash moves. **Coupling.** Owns `map_row`, `old_roots`, `candidates`, `check`, `commit`,
`Candidate`, `Check`, the two `db` functions, `REBASE_MESSAGE`, `left_warning`; T8b and T8c consume them. **Not in this task.** Any caller.

## T8b: a run rebases rows whose files moved with the working folder

**Problem.** The run must point a row at the current root when its file is there as recorded, and touch nothing else.

**Files.** `stacking/run/calibrate.rs:1131-1135`; `docs/logging/README.md`; tests in `stacking/run/normalize_tests.rs` (`RegroupRig`), `stacking/run/prepare_tests.rs`
(fixture of `:288`).

**Approach.**
1. `stacking/run/calibrate.rs`: `pub(super) fn rebase_working_rows(rc: &mut RunContext) -> Result<(), RunError>`, the first statement of `stage_calibrate_with`, before
   `ensure_source_samples` and the Prepare dispatch:
   - phase 1 under a connection: `candidates(conn, &rc.subject, &rc.layout.root)`; an `Err` is a `warn!` and the run goes on (a failed rebase only costs a recompute); none →
     return;
   - phase 2 without one: ONE event `rc.progress(Stage::Calibrate, None, 0, n, 0, 0, None, Some(REBASE_MESSAGE.to_string()))` (`n` the candidate count; no per-file tick, as
     T4a's), `rc.check_cancel()?` per file, `check`; a candidate not moved logs `debug!(path, reason, "cached file not rebased")` (`missing`, `another size`, `another time`,
     `not a file`), or `warn!(path, error, ..)` when `Unreadable`; it counts as gone when `!file_matches_identity(old_path, Some(size))`;
   - phase 3 under a connection: `commit`; `info!(run_id, old_root, new_root, moved, left, "working folder rows rebased")` once per old root with candidates; gone > 0 pushes
     `left_warning(gone, &rc.layout.root)` to `rc.warnings`.
2. `RegroupRig` gains `fn point_at(&mut self, working: &Path)` (the setting and the layout).
3. `docs/logging/README.md`: `old_root`, `new_root`, `left` (`moved` exists, zip extension, same meaning).

**Tests** (multi-run, on T4c's `RegroupRig` and `set_times` unless named).
- `a_moved_working_folder_reuses_every_frame_level_stage` (`RegroupRig`): run 1 under W1; `std::fs::rename` W1 → W2; every file's time to whole seconds; `point_at(W2)`; run 2:
  calibrate, pass1, pass2, align and LN fan out over 0 items; every file row names W2; W1 does not exist. Fails on `main`.
- `a_copied_working_folder_is_used_and_the_original_is_untouched`: copy with times kept; run 2 under W2: 0 items, rows name W2, W1's walk unchanged; W1 removed, run 3: 0 items.
- `a_diverged_copy_is_recomputed_not_read`: run A under W1; copy to W2 with times; run B under W2 with `hotPixelCorrection` flipped (premise: calibrated sizes equal); every W1
  time 1 h back; W2 removed; `point_at(W1)`; run C at B's config: Calibrate covers 6, `rc.warnings` holds `left_warning(n, ..)` with n the rows left, the planes equal run B's (as
  `recalibration_reproduces_the_planes_bit_for_bit`, `calibrate_tests.rs:1201`).
- `an_in_place_row_inside_the_working_root_is_never_rebased` (`prepare_tests.rs`, fixture of `:288`, debayer off): copy the working root, `landed/` included, to W2, point at it,
  run: the in-place rows still name W1's landed files.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::rebase stacking::run`; the two checks (`stacking::run` whole: every run passes the trigger).

**Acceptance.** A run reuses a moved or copied working folder; a diverged older copy is recomputed, never read; nothing on disk is moved, copied or deleted; no hash moves.
**Coupling.** Owns `rebase_working_rows`, `point_at` and the two log lines; uses T8a, T2's `file_matches_identity`, T4c's `RegroupRig` and `set_times`; runs before T4a's
book. **Not in this task.** The gate, Blink, previews (T8c); a frame-set rename (RB-2).

## T8c: the gate and Blink read through the rebase; previews follow the current folder

**Problem.** The gate writes nothing, so it must judge each candidate at its mapped path; Blink too (RB-3); the preview cache must not recreate a moved folder.

**Files.** `stacking/rebase.rs`; `stacking/plan.rs`: folders gate `:3384-3448`, `compute_register_stale` `:1902` (registered read `:2083-2090`; callers `:4053`, tests `:4501`,
`:11254`), `fits_artifact_fresh` / `fits_artifact_fresh_for_planes` `:1136-1186` (callers `:3863`, tests `:7103`, `:7130`), reads `:3691`, `:3783`, `:3819`, `:3940`;
`stacking/run/measure.rs:694`, `:1629`; `stacking/run/register.rs:408`; `api/stacking_aligned.rs:166-256` (`resolve`, registered read `:207-213`, callers `:260`, `:278`);
`api/stacking.rs:1242-1305` (`resolve_master_light`, run-row layout `:1269-1277`); docs.

**§4.10's six row reads checked:** `calibrated` `:3783-3789`, `calibrated_mosaic` `:3819-3825`, `registered` `:2083-2089`, `ln_reference` `:3691`, `ln` `:3940-3941`,
`fits.<plane>` `:1173-1180`: at the spec's lines. The `ln_global` read at `:3945-3952` is payload-only and takes no view.

**Approach.**
1. `pub(crate) struct RebaseView { by_row: HashMap<i64, String>, old_roots: Vec<PathBuf>, left_gone: usize }`: `for_gate(conn, subject, current_root) -> anyhow::Result<Self>`
   (T8b's phases 1 and 2 with T8a's `candidates` and `check`, under the gate's connection, no progress, no commit, `debug!` only), `none()`, `apply(&self, row:
   Option<StackingArtifactRow>) -> Option<StackingArtifactRow>` (a moved row comes back with its mapped path), `old_roots()`, `left_gone()`.
2. `build_plan`: after the folders gate, `for_gate` on the subject root under `run_spelled_working(validated working)`; no folder, or an `Err` (a `warn!`) → `none()`. `left_gone
   > 0` pushes `left_warning(..)` to `warnings`. `prepare_roots` (T2) extends with `view.old_roots()`.
3. Apply the view at the six reads; `compute_register_stale` and both `fits_artifact_fresh*` take `view: &RebaseView`; run callers and plan tests pass `&RebaseView::none()`.
4. `api/stacking.rs`: `pub(crate) fn current_working_dir(conn, settings, subject) -> Result<Option<PathBuf>, ApiError>`: the subject's config (`get_set_config` +
   `resolve_set_stacking_config`, or `plan::read_project_config`), `resolve_dirs(..).working`, spelled by `run_spelled_working` when the folder exists.
5. Blink: `resolve(conn, run_id, frame_ids, view)` applies the view at `:207-213`; both callers build it from `current_working_dir` (an existing folder) or pass `none()`.
   `resolve_master_light`: the layout from `current_working_dir`; the run row's `working_dir` only when none is configured.
6. Docs: a bullet "A working folder at another path" in the frame-level cache section of `docs/stacking/README.md` (the rule, the 2 s copy rule, what is never touched, the
   warning); open-items: one RB-2 line. (The logging fields are T8b's.)

**Tests.**
- `a_moved_working_folder_reuses_every_frame_level_stage` gains: before run 2 the gate reads Calibrate, Measure, Register and Normalize fresh, and every row still names W1 after
  `build_plan`. `a_diverged_copy_is_recomputed_not_read` gains: before run C the plan's warnings hold `left_warning(n, ..)`.
- `a_moved_working_folder_caches_previews_in_the_current_one` (`api/stacking.rs`, fixture of `cached_master_light_preview_is_permit_free_and_matches_the_render` `:2536`): after a
  move the preview is cached under W2 and W1 is not created. Fails on `main`.
- `blink_shows_a_moved_folders_frames_before_the_next_run` (`api/stacking_aligned.rs`, `seeded` / `run_once`): rename the working folder, point the setting: every frame
  `Available` under W2; the rows still name W1.

**Gates.** `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo test -p athenaeum-core --lib -- stacking::rebase stacking::plan stacking::run api::stacking api::stacking_aligned`; the two checks.

**Acceptance.** After a move or copy the plan reads the stages cached, Blink shows the frames, previews land in the current folder; gate and run agree unless a file changes
between them. **Coupling.** Owns `RebaseView`, `current_working_dir`, the README bullet; uses T8a, T8b's rig tests and T2. **Not in this task.** Any write from the gate or Blink.

## T6: the references

**Problem.** The references describe the time-based identity, `strict_mtime` and the old fold.

**Files and approach** (grep `modification time`, `modified_at`, `strict_mtime`, `file_matches_identity`, `ref_measure`):
- `docs/stacking/README.md`: rewrite "The registration fold rule" `:5224-5231` (FI-6), "Pass 1 and its stamp" `:5232-5237`, "Identity check" `:5238-5244` (key + size, the write
  order), "One-time recomputes" `:5274-5279` (FI-4: every frame set recomputes once; projects keep Prepare and Measure; a one-plane project reference re-registers once), "What a
  regroup does not reuse" `:5280-5285` (item 3 closed); the Blink row `:1888`, the identity paragraph `:1898-1901`, R11c-4 `:3833-3839`. New bullets "Input identity" (§4.3-§4.6:
  the book, banking, the gate's catalog-only reading) and "In-place rows" (§4.1).
- `docs/catalog/README.md:36` and `:113`: the sentences of §4.5.
- `docs/superpowers/open-items.md`: rewrite `:423` (regroup finding: decided by FI-6, items 1, 2, 4 stay) and `:425` (known gap: first half closed in the run; the gate residual;
  the second half's closer becomes a size check); `:418` takes §9 A0's expectation; add §7's two residuals. `:417` stays until T7 passes.

**Gate.** Review against the spec; `git diff --stat` shows docs only; no `CLAUDE.md` change (no rule, command surface or path changed). **Acceptance.** No reference describes a time comparison the code no longer makes. **Coupling.** T8c owns
the rebase bullet. **Not in this task.** Real-data numbers (T7).

## T7: acceptance on real data (§9)

**Problem.** The synthetic tests do not show the share's 100 ns times, the exFAT drive's rounding, or a first run over the dev catalog's existing caches. **Who.** A headless agent does everything except: the owner mounts the exFAT drive (Fotobase) and gives its mount point; the owner unmounts the share for A2b's offline run (or
allows the agent to); the project leg of A2b needs a project with 3 or more landed frames in the copy (otherwise skipped, and said so).

**Disk and time.** Raw: set 163, 16 OSC lights of about 52 MB = 0.85 GB; set 185, 67 mono lights of about 52 MB = 3.5 GB. By the owner's rule a full run is about twice the raw
size (1.7 GB, 7 GB); float32 working files can reach 4-6x raw (about 10 GB and 14 GB), so budget the larger figure. Peak: the catalog copy (`du` it first) and logs in
`~/.athenaeum-acc`; on the share 4.3 GB of lights and up to two working folders (about 48 GB); on the exFAT drive up to 48 GB. BigMac needs free >= estimate + 150 GB. Time: 163
cold about 3 min locally (2026-11-08 smoke: 2.3 min), 185 about 10 min; on the share the 11.7 MB/s read dominates (185 cold 40-60 min, warm 15-25 min: Integrate reads every
registered frame). About 5-7 h of machine time; the owner about 20 min.

**Steps.**
0. Setup.
   - Disk report. `mkdir -p ~/.athenaeum-acc/fi-accept/{db,work,out,bounce}`; REGISTRY row (`~/.athenaeum-acc/fi-accept`, `job`, `file-identity`, `<today>`, `<today + 7 d>`,
     `file-identity acceptance (spec §9): catalog copy, local working folder`). Share: `mount | grep Universe` (else `osascript -e 'mount volume
     "smb://192.168.31.208/Universe"'`), `mkdir -p /Volumes/Universe/athenaeum-temp/fi-accept/{work,work2,lights,masters}`, its own row. exFAT (owner mounts): `diskutil info
     /Volumes/Fotobase | grep -i 'file system'` reads ExFAT, `mkdir -p /Volumes/Fotobase/athenaeum-temp/fi-accept/{work,work-notimes,bounce}`, its own row.
   - Build in the worktree on `main`'s tip: `CARGO_TARGET_DIR=/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent cargo build -p athenaeum-web --release`
   - Catalog copy: `sqlite3 -readonly "$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db" ".backup $HOME/.athenaeum-acc/fi-accept/db/athenaeum.db"`; in
     the copy delete `settings` rows keyed `account.*` and `sync.*`, set `monitoring.enabled_global` false, check `file_operations` has nothing pending, point
     `calibration.library_dir`, `stacking.working_dir`, `stacking.output_dir` into `fi-accept`. KEEP the stacking rows of sets 163 and 185: A0 runs over them (their files stay in
     the owner's folder, which no run writes or deletes).
   - Projects in the copy: no release tag carries the time check (`89ccc6be` is after `v0.7.1`), so released catalogs lose nothing, but the dev catalog can hold rows a
     cancelled dev-build run left over a rewritten file, and a project's hash does not move, so after M1 such a row reads fresh. Before A0, delete every project's
     `stacking_artifacts` rows in the copy (the owner's files stay untouched), or say in the report which projects kept theirs.
   - Start `ATHENAEUM_DB_PATH=$HOME/.athenaeum-acc/fi-accept/db/athenaeum.db ATHENAEUM_PORT=3399
     /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum/target-agent/release/athenaeum-web` in the background, stopped after each session; logs in
     `fi-accept/db/logs/` (grep, never cat). Per set (`get_stacking_config` / `set_stacking_config`): `paths.workingDir` = `fi-accept/work`, `paths.outputDir` = `fi-accept/out`;
     set 163 keeps its 16 lights of 300 s and 600 s (exclude its 180 s frames); a Manual reference via `set_frame_set_reference` (2026-11-08 smoke: 65057 for 163, 93954 for 185;
     confirm they are in the sets).
   - Record per run: `staleStages` and warnings of `get_stacking_plan` just before; the summed `items` of the `fan-out finished` lines for calibrate, pass1, pass2, align;
     `stacking inputs checked` (`count`, `sampled`, `banked`); any `working folder rows rebased` (`moved`, `left`). Start with `start_stacking`; wait with one `timeout`-bounded
     poll of `get_stacking_run`.
1. **A0.** Run 1 on each set: every stage recomputed (calibrate = pass1 = pass2 = align = lights), `sampled` reported. Run 2: 0 / 0 / 0 / 0.
2. **A1, the share.** Working folder `/Volumes/Universe/athenaeum-temp/fi-accept/work`; one run with `"rerunFrom": "calibrate"` (it writes every file on the share; without it the
   rows left at A0's folder are read there), then a normal run: 0 for calibrate, pass1 and align. Then `rm -rf ~/.athenaeum-acc/fi-accept/work`.
3. **A2, copies with the time kept.**
   - `cp -p` the set's lights to `.../fi-accept/lights/<set>/` and one master to `.../masters/`; point the catalog at each copy with `relocate_missing_file`
     (`{"fileId":..,"newPath":".."}`). A rescan of the copies' root does not do for lights: it makes new rows and new frames outside the set.
   - Bounce the working folder to the same absolute path: `cp -Rp <share>/work /Volumes/Fotobase/athenaeum-temp/fi-accept/bounce`, `rm -rf <share>/work`, `cp -Rp` it back
     (through the exFAT drive the times return at 10 ms; if it is not mounted yet, bounce through `fi-accept/bounce` and record that the times stay equal).
   - `stat -f '%Fm %N'` on two copied lights and two working files against their rows' `modified_at`: record the precision (100 ns, 10 ms, whole seconds).
   - Run: 0 for calibrate, pass1 and align, `sampled` <= inputs. Repeat the lights leg on the exFAT drive.
4. **A3, regroup** on 185 with `measurement.psfModel = Moffat4`: `seconds`, then `stops`, then `seconds`. Align 0 on both regroup runs; `staleStages` lacks `register` before
   each.
5. **A2b, the working folder at another path** (163 and 185, after A1-A3).
   - `touch ~/.athenaeum-acc/fi-accept/marker`; `cp -Rp <share>/work /Volumes/Fotobase/athenaeum-temp/fi-accept/work`; keep the share copy; set `paths.workingDir` to the exFAT
     copy. `stat`: the copies' times rounded, within 2 s.
   - The plan reads Calibrate, Measure and Register cached; the run reports 0 / 0 / 0 / 0 and `working folder rows rebased` with `left=0`; `find <share>/work -newer
     ~/.athenaeum-acc/fi-accept/marker | wc -l` is 0 and the share's file count is unchanged.
   - Share unmounted (owner): run again, 0 again (offline lights and masters keep their banked samples). Remount.
   - A1 on exFAT: one run with `"rerunFrom": "calibrate"`, then a normal run: 0 for calibrate, pass1 and align.
   - Back: `cp -Rp` the exFAT folder to `<share>/work2`, point at it, run: 0, `left=0`.
   - One project, if the copy has one: the same move through `set_project_stacking_config` / `start_project_stacking`: Prepare and Measure kept; its in-place rows still name the
     Collaboration folder.
6. **A2c, a copy without its times** (skip if RB-1 is ruled the other way): `cp -R` (no `-p`) `<share>/work2` to `/Volumes/Fotobase/.../fi-accept/work-notimes`; remove
   `<share>/work2` (otherwise the left rows are read there and nothing recomputes, §4.10); point at the copy. The plan warning counts the files; the run recomputes them.
7. Close: stop the server; report `.superpowers/sdd/2026-11-12-file-identity/t7-report.md` (60 lines at most: the table per run, the precisions seen, a verdict per item); commit
   the numbers into the frame-level cache section's Acceptance bullet of `docs/stacking/README.md`; delete open-items `:417` and `:418` when A0-A2c pass; delete `fi-accept` on
   BigMac, the share and the exFAT drive with their three REGISTRY rows; disk report.

**Gate.** Every item passes, or the job STOPs with at most 10 lines of evidence. **Acceptance.** §9 holds on sets 163 and 185. **Coupling.** Reads the log lines and messages of
T4a, T8a, T8b, T8c. **Not in this task.** Code changes.

## Cross-task contracts

| Contract | Producer | Consumers |
| ---- | ---- | ---- |
| `db::stacking::delete_artifact(conn, subject, group_key, kind, frame_id) -> Result<bool>` | T1a | T1a, T1b |
| `plan::file_matches_identity(path, size) -> bool`; warn "cached file cannot be stat-ed; counted as changed" | T2 | `is_fresh`, Blink, `measure_tests.rs:1015`, T8b |
| `paths::run_spelled_working`; `plan::prepare_row_names_its_input`; `prepare_roots` | T2 | T2 gate, T8c (adds old roots), `current_working_dir` |
| `source_identity::{input_row, input_identity, InputIdentity, SourceSamples, BookMiss}` | T3 | T4a, T4b |
| `NotSampled` reasons: "not sampled yet", "changed while it was sampled", "unreadable", "not in the catalog", "cancelled" | T3 | T4b error text, tests |
| `SAMPLE_READ_LOG`, `SAMPLE_TOUCH_AFTER_READ`; `STAGE_ONE_JOB_CANCELLED`, `LN_CANCEL_BEFORE_FAN_OUT` | T3; T1a, T1b | their tests; T4a, T4b (the touch hook), T4c |
| `HashMemo::{for_gate, for_run, samples_resolved, set_run_samples}` | T4a | T4b, every former `HashMemo::new` caller |
| `ensure_source_samples(rc)`; ONE progress event "checking the inputs" at `current = 0`; info "stacking inputs checked" (`count`, `sampled`, `banked`) | T4a | T8b (order), T7 |
| `test_fixtures::bank_samples(conn) -> usize` | T4a | gate tests of T4a-T8c; never a run rig |
| `SourceIdentity::{Sampled, Project}`; `master_token`; `light_source`; `StageOneError` "{path}: {why}" inside "calibration failed: …" / "preparation failed: …" (a `BookMiss` mapped at its two call sites) | T4b | plan, run exclusions |
| `RegroupRig::{new, cfg, stale, run}`, `set_times` (+ `point_at` by T8b) | T4c | T5, T8b, T8c |
| `reference_measurement_fold(cfg, reference_group, prepare_path, ref_hash, beta)`; `reference_measurement_term(rc, ref_id, ref_hash, prepare_path)` | T5 | gate, register, two-pass |
| `rebase::{map_row, old_roots, candidates, check, commit, Candidate, Check}`; `db::stacking::{subject_working_dirs, rebase_artifact_path}` | T8a | T8b, T8c |
| "finding the cached files" (one event, as T4a's); `left_warning(n, root)` (§4.10's text) | T8a | T8b run, T8c gate, T7 |
| `rebase_working_rows(rc)`; info "working folder rows rebased"; debug "cached file not rebased" | T8b | T7 |
| `RebaseView::{for_gate, none, apply, old_roots, left_gone}`; `api::stacking::current_working_dir` | T8c | gate, Blink, preview cache |

## Existing tests this plan changes

Every test the spec names exists under that name; lines at `0df147a7`:

| Task | Tests (where) | Change |
| ---- | ---- | ---- |
| T1b | `a_cancelled_rewrite_does_not_leave_another_runs_pixels_fresh` (`stacking/run/register_tests.rs:824`) | premise: B's rows are gone, the size premise `:866-870` stays |
| T2 | `identity_refusal_by_mtime_says_why` (`stacking/plan.rs:4194`), `aligned_file_that_is_not_the_recorded_one_is_replaced` (`api/stacking_aligned.rs:696`); the call in `measure_pass1_rewritten_inside_a_failing_pass_two_records_its_row` (`measure_tests.rs:1015`); the doc of `measure_pass1_artifact_survives_a_touched_source` (`measure_tests.rs:793-796`) | replaced as named in T2; two-argument call; doc only |
| T4a | the tests that form a stage-1 hash before any run (T4a's Files); the `HashMemo::new` callers `stacking/plan.rs:4394`, `:9095`, `:9117`, `:9247`, `:11128`, `:11253`, `stacking/run/prepare_tests.rs:1059`, `:1120` | `bank_samples` after seeding; `for_gate` / `for_run` |
| T4b | `stage_hash_is_stable_and_sensitive`, `stage_hash_is_order_insensitive` (`stacking/config.rs:1874`, `:2289`); pins `:8722`, `:8831`, `:8927`; formulas `:8963`, `:9220`; project `:9261`; zip part of `:6208` (all `stacking/plan.rs`); the tests T4a banked | `Sampled` literals; re-pinned; over the book; literal recorded first; `a_zip_master_token_is_its_size_and_crc`; `&mut SourceSamples::catalog_only()` |
| T4c | `a_source_of_another_size_reads_stale` (`stacking/pass1_artifact.rs:619`), `measure_pass1_artifact_of_an_edited_in_place_source_reruns_pass_one` (`measure_tests.rs:822`), `regroup_reuses_every_frame_level_stage` (`normalize_tests.rs:745`) | renamed, strict calls go; the rescan NULLs `content_hash`; on `RegroupRig` |
| T5 | `registration_hash_folds_the_reference_measurement_across_groups_only` (`stacking/plan.rs:11026`, flip `:11056`) | renamed `..._whatever_its_group` |
| none | `register_stale_follows_the_reference_measurement_for_an_osc_frame` (`plan.rs:11087`), `render_modified_at_is_pinned_to_golden_literals` (`:4169`), the empty-source pins (`stacking/config.rs:1296-1300`, `:2040-2061`, `:2589`) | must not move |

## Risks: where a mistake gives a wrong master, not a recompute

1. **The write order (§4.2).** A writer that replaces a file and keeps its old row leaves an old-key row over another config's pixels of the same size; with the time gone that
   row reads fresh and the master is stacked from the wrong frames. Caught by `a_cancelled_recalibration_leaves_no_row_over_its_files`,
   `a_debayer_off_run_retires_the_mosaic_row_it_overwrites` (a mosaic row over another run's calibrated file), `a_cancelled_prepare_leaves_no_row_over_its_files`,
   `a_cancelled_first_pass_leaves_no_row_of_the_old_key`, `a_failed_fits_write_leaves_no_row`, `a_cancelled_rewrite_does_not_leave_another_runs_pixels_fresh`,
   `a_cancelled_normalization_leaves_no_row_of_the_old_hash`; the LN reference and the `metrics`-β member of pass 1 (kill-only) by review only. M1 is one merge, so the time never
   leaves before the order is in; the reviewer checks every `upsert_artifact` with a `path` follows a `delete_artifact` whose failure stops the job.
2. **The in-place row rule (§4.1).** An in-place `calibrated` row naming a light's old path, where a capture program wrote another file of the same size, feeds that file to
   Measure, Register and Integrate. Caught by `an_in_place_row_at_another_path_than_its_light_is_stale` (the other file carries the recorded time too, so the test also fails on
   `main`).
3. **The rebase's safety rule (§4.10).** Rebasing onto an older diverged copy stacks run A's calibrated frames under run B's key. Caught by
   `a_diverged_copy_is_recomputed_not_read` (the planes equal B's), with `rebase_needs_the_size_and_a_time_within_two_seconds` (the 2 s rule at full precision),
   `rebase_maps_by_components_onto_the_kinds_own_subtree` (no sibling folder, no wrong subtree) and `an_in_place_row_inside_the_working_root_is_never_rebased`.

Also data damage outside stacking: a wrong `content_hash` would let Duplicates offer a unique file for deletion and sync skip a file. Caught by
`bank_content_hash_writes_only_over_a_matching_null_row` and `a_file_changed_during_the_read_is_not_sampled`.

## Open calls for the owner

Closed on 2026-11-13: the owner approved the plan with RB-1 as written («Ок, начинай»; spec FI-10). A2c stays in T7.

- **RB-1 (spec §4.10).** A copied or moved working folder is reused only when its files kept their times within 2 s. Default: keep the 2 s rule in this release.
  - What he sees with it: a folder moved or copied by Finder, `cp -p` or any tool that keeps times is reused at once, on any volume (rounding stays under 2 s); a copy by a tool
    that resets times (`cp` without `-p`, some sync tools) is not adopted: the app reads the original while it exists, and once it is gone recomputes the set once, with one
    warning that counts the files. That is the one place left where "only the time changed" costs a recompute; the time is never a verdict inside a folder, only at a move.
  - The alternative, a content sample per working file, costs a new column, a sample at every write of the seven writers, up to 0.13 s per file on the share at each move, and
    one recompute at the first move after the update anyway (older rows have no sample). The direction of the error decides: the 2 s rule can only cost a recompute, the
    sample is new code exactly where a mistake gives a wrong master. If a tool he really uses drops times, the sample becomes its own follow-up.

## Spec corrections

1. §3-§9 lines are of `484fa75f`. Moved ones that matter: `resolve_calibrate_job` rows `:388-411`, job `:563-573` (spec `:262-284`, `:438-439`, `:498`); the cancel in the commit
   loop `:211-212` (spec `:198-202`); `stage_calibrate_with` `:1131` (§4.4 says `:1004-1008`); `run/mod.rs` `file_identity` `:622-626`; `bank_strong_hash` `:7565` (spec `:7606`);
   `masterFiles` `plan.rs:2687` (spec `:2679`); `test_fixtures::file_identity` `:587-592` (spec `:565-578`); Blink `api/stacking_aligned.rs:239-253`; `landed_unchanged`
   `origin.rs:176-202`; plan.rs tests after `:10464` moved +42 (`:11026`, `:11056`, `:11087`); open-items `:402/:403/:408/:410` are now `:417/:418/:423/:425`; README sections
   `:5224-:5285`, Blink row `:1888`, identity `:1898-1901`, R11c-4 `:3833-3839`.
2. §4.2: the LN reference's file is written by `write_reference` at `normalize.rs:1766` (`:1836-1870` is its identity and upsert, `upsert_artifact` at `:1855`); the delete goes
   before `:1766`. The mosaic row is read always and deleted whenever the job writes its path: when the job wants a mosaic (`want_mosaic`, `keep_mosaic`) or the row names a
   file the job writes (`out`; Prepare's `converted_out` / `debayered_out`). With the debayer off an OSC frame's calibrated file takes the mosaic's name `c_<stem>.fits`, and a
   mosaic row left there at the old hash is reused by a later Bayer run over another run's pixels [review].
3. §4.2: the `to_register` queue (`register.rs:967`) is also reached by the two-pass dry pass, which writes no file; its delete is `persist`-only.
4. §4.2: the `measured` row is read today only when not `force_fresh` (`:1337-1356`); it must be read for every member and carried to `upsert_pass1_row`, or a legacy file is
   never retired (R6 M1). `registered` needs the same carry (`record_registered_artifact` re-reads after the write today). The note "the fallback writes under the SAME key" is
   wrong for a member whose own β came from its `metrics` row and whose `measured` row is not fresh: it is not owed pass 1, its pass-2 worker writes under another key. The
   `measured` row of every member left `Owed` without a fresh row is deleted before pass 2's fan-out, not only `need_pass1`'s [review].
5. §4.1: "in place" is undefined. The run decides by Prepare's action (`InPlace`, or the kept float mosaic, for a frame not in a zip); the gate, which has no action, by the row
   lying outside the `prepared/` and `calibrated/` subtrees of the subject's roots, in both spellings [plan decision].
6. §4.10 rule 3 and §8: `Stamp::matches` truncates both times to whole seconds, so it accepts up to 2.999 s and "+2.5 s is OtherTime" fails for a whole-second recorded time;
   the rebase keeps `MTIME_TOLERANCE_SECS` and compares at full precision. Also: `moved` is already a dictionary field (zip), so only `old_root`, `new_root`, `left` are new.
7. §4.10: counting files "gone at the old place too" needs one more stat of the old path per left candidate [plan decision].
8. §4.4: the run's book also covers manually excluded frames, because every stage's hash call propagates `?` and a lookup outside the book would fail the run;
   `HashMemo::new(origins, samples)` becomes `for_gate` / `for_run`, so a run cannot fall back to a catalog-only book [plan decisions].
9. §4.3: a light inside a zip folds `SourceIdentity::Sampled { sample: "zip:<token>" }`; the spec gives only the master's form [plan decision].
10. §8: no strict pin at `stacking/measure.rs:2225` (that line is the `PASS1_TOUCH_AFTER` hook); the strict pins are in `pass1_artifact.rs:630-631`, `:649-658`.
11. §8: the in-place test's fixture is `Pass1Rig::attested()` (`stacking/run/tests.rs:641`, lights used in place); `prepare_tests.rs`' attested fixture converts 16-bit lights and
    has no in-place row. The other file must carry the row's recorded time, or the test passes on `main` by the time check.
12. §9 A1: the first run in a new working folder reads the rows left at the previous folder there and computes nothing; A1 needs `rerunFrom = calibrate` (or the previous folder
    removed) to write its files in place first.
13. §9 A2: lights must be relocated through Missing files; a rescan of the copies' root makes new rows and new frames outside the set. Only a master may be rescanned (FI-3).
14. §9 A2c: while the source copy exists, the left rows are read there (§4.10, "A row not rebased"): no warning, no recompute. The source must be gone first.
15. §4.2: a failed `delete_artifact` stops the job (`?`) at all seven sites; a warn-and-continue leaves the old row over the new file. The dry pass of the two-pass reference
    deletes nothing, because it persists nothing (correction 3) [review].
16. §10: T8's run regressions use T4c's `RegroupRig` and `set_times`, and its trigger sits before T4a's book, so the order is T1 → T2, T3 → T4 → T5, then T8; not T8 beside
    T3-T5 [review].

## Review

Reviewed on 2026-11-12 by an independent reviewer on the most capable model, against the spec and `main` at `0df147a7`: approve with changes. All six required and all eight
recommended changes are taken in above (the mosaic row, the `metrics`-β member, one progress event, `?` on every delete, no run rig banks, T8 after M2; the test premises, the T4
cut, the T8 split, the gates, `map_err`, two tests, the A0 note, three comments). Review: `.superpowers/sdd/2026-11-12-file-identity/plan-review-fable.md`.

## Outcome

Merged on local `main`, each merge after its review and fixes; this branch (`fi/acceptance`, T7 with the sync fix `9f404957`) is the last.

| Merge | Tasks | What its review found |
| ---- | ---- | ---- |
| `9214a07d` M1 | T1a, T1b, T2 | another subject's row over a replaced file (fixed: `delete_artifacts_naming_path`) |
| `97a5843e` M2 | T3, T4a-c, T5 | the scanner's move detection kept a sample over a rewritten time (fixed) |
| `e8f59465` M3 | T8a-c | no false move; a project gap and a warning that stayed (fixed) |
| `f2c3e967` | T6, the whole-branch review's fixes | the zip adoption door (fixed in `3cfd980e`) |

Acceptance (2026-11-13, real data, `docs/stacking/README.md`): every leg passed; it found the SMB share's refused full sync, fixed in `9f404957`. One owner call stays open: whether an own working file also needs a time within 2 s of its row (`docs/superpowers/open-items.md`, "Awaiting a call from the owner").
