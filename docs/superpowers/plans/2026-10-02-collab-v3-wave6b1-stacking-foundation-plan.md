# Collab v3 wave 6b-1 — stacking foundation (subject seam, table rebuild, config pin) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the stacking pipeline subject-aware (`StackSubject::{FrameSet, Project}`) and give its tables a project column pair, without changing a single observable behaviour of frame-set stacking — every existing test passes unchanged except one tuple assertion, every cached artifact of every existing frame set stays fresh, and the generated TypeScript does not move.

**Architecture:**
- The four stacking tables that carry a subject (`stacking_runs`, `stacking_run_frames`, `stacking_artifacts`, `master_lights`) are rebuilt once in `init_db` with a subject pair (`frames_set_id | project_id`) and a frame pair (`frame_id | project_frame`), guarded, in one transaction with `foreign_key_check`.
- A new ungated `StackSubject` (in `db::stacking`, re-exported as `stacking::StackSubject`) replaces the frame-set id as the run's key in persistence, `RunContext`, `build_plan`, `start_stacking`, the heal sweep, the working layout and the run-id lookups. Every Project entry point refuses with one message until 6b-2.
- Catalog reads by frame id move behind a per-frame `FrameOrigin` (one variant, `Catalog`, in 6b-1), so 6b-2 adding the `Project` variant makes the compiler name every site that needs a project twin.
- `calibration_subtree` emits a fixed legacy grouping shape, and every stage hash of fixed configs is pinned to golden literals recorded before any change.

**Tech Stack:** Rust (`athenaeum-core`: rusqlite 0.40 (bundled SQLite), serde, xxhash), ts-rs (`tests/ts_contract.rs` pins every wire type). No frontend change, no host change, no new command.

**Spec:** `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` (owner-approved 2026-10-02, fable-reviewed). This plan implements §18 plan 6b steps **0, 1, 2** (§4.1, §4.2, §4.3, §8's cache rule). Read §2 (W8), §4 and §8 before any task. Steps 3–14 are plan 6b-2.

**Research behind this plan** (verified against HEAD `17e2e96e`, kept with the plan):
- the config/schema facts (`docs/superpowers/research/6b/2026-10-02-6b1-config-and-schema-facts.md`) — every DDL, every SQL statement on the four tables, the ON CONFLICT checks run in sqlite3 3.54, and a rehearsal of the rebuild on a copy of the real `.dev` catalog (4 runs, 8 groups, 1104 run-frames, 368 artifacts, 6 masters: counts unchanged, `foreign_key_check` 0);
- the seam inventory (`docs/superpowers/research/6b/2026-10-02-6b1-subject-seam-inventory.md`) — 42 production `rc.set_id` reads classified, 12 subject-keyed db functions, the catalog-read checklist plus one site the spec missed, ~215 one-token test edits.

## Global Constraints

- **Branch:** `collab-wave6b` (cut from local `main` `17e2e96e`). Merge and push only on the owner's word.
- **Commits:** as the configured user (`eg013ra1n` / `vilen.sharifov@gmail.com`). Every message ends with:
  `Co-Authored-By: <the model that wrote the code> <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK`.
- **Behaviour preservation is the acceptance criterion of this whole plan.**
  - Every existing test passes. The ONLY assertion allowed to change is the `(id, frames_set_id)` tuple in `list_unfinished_runs_reports_only_planning_and_running` (`db/stacking.rs:~1102`). Other test edits are mechanical (a `set_id` argument becomes `&StackSubject::FrameSet(set_id)`, a struct field `frames_set_id: x` becomes `subject: &StackSubject::FrameSet(x)`), never an expectation change.
  - `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` leaves `src/types/` byte-identical (`git diff --exit-code src/types`).
  - Log fields and error strings of the frame-set path stay byte-identical (`impl Display for StackSubject` prints `frame set {id}`).
- **Headless build:** `cargo check -p athenaeum-core --no-default-features` stays green — `db::stacking` and `services::StackHandle` are ungated; `stacking::*` and `api::stacking` are gated `all(render, solver)`.
- **The replica is sacred / no project behaviour:** no code path in this plan opens a project file, mints a handle outside tests, or runs a project stage. Project entry points refuse with `ApiError::Invalid(PROJECT_STACKING_UNAVAILABLE)`.
- **Errors and logging:** never swallow an error (log at `warn!`/`error!` before degrading); `tracing` only, short stable message, snake_case fields; zero `println!` in production code.
- **Build discipline (this Mac):** `CARGO_BUILD_JOBS=4`; `--test-threads=4`; one compile at a time; format touched files with `rustfmt <files>` (never `cargo fmt -p`) and revert sibling-module reformatting rustfmt drags in through `mod` declarations; clippy is not a gate.
- **Gate per task** (each task must leave all of these green): `cargo test -p athenaeum-core --all-targets` (examples included — `examples/ln_probe.rs` calls `list_runs`/`find_artifact`), `cargo check -p athenaeum-core --no-default-features`, `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`. Two wall-clock collab live tests and `landing_free_bytes_is_bounded_by_the_collaboration_root` are known to flake under full-suite load: a failure of one of them is re-run alone and reported with both runs.
- **Copy rules:** comments, docs and commit messages in English; never name a third-party project in the repo.

## Review Focus

1. **Upgrading a real catalog that already holds runs, artifacts and masters.** Every row survives with the same id and values; the children (`stacking_run_groups`, `stacking_run_frames`, `master_lights`) are not cascade-wiped when their parent table is dropped; `foreign_key_check` is clean; a second start is a no-op. Pinned in Task 2 (unit) and Task 2's rehearsal on a copy of the real `.dev` catalog.
2. **A fresh database and a migrated one end up with the same schema** — same columns, same CHECKs, same index names and uniqueness — and the OLD `stacking_artifacts_key` is never re-created on a later start (a second unique index on the new table would make upserts hit only one of them). Pinned in Task 2.
3. **A rebuild that cannot complete** (a pre-existing foreign-key violation in a stacking table) rolls back completely: the old shape and every row stay, `PRAGMA foreign_keys` is back ON, and the error is surfaced, not swallowed. Consequence the user sees: startup refuses on every launch until the violation is repaired, so the error text names the table, the `PRAGMA foreign_key_check` that lists the rows, and that deleting those orphan rows is safe (they are cache and run-history rows). Pinned in Task 2.
4. **An existing frame set keeps its caches across the upgrade.** An artifact written before the rebuild is found by `find_artifact(FrameSet)` after it, with the same id and hash, and every stage hash of fixed configs equals the golden literal recorded before the change — so the first run after the upgrade recomputes nothing. Pinned in Task 1 (golden) and Task 3 (lookup across the rebuild).
5. **A project-subject run row in the database** (written by SQL, as 6b-2 will) does not break the heal sweep, a frame set's run list, a frame set's admission check, work usage or cleanup; the heal marks it failed like any interrupted run. Pinned in Task 3 (db) and Task 4 (heal, work usage, cleanup).
6. **A large frame set's plan and run stay as fast as today.** `find_artifact` is called per frame per kind (8 sites in `build_plan`'s per-frame loop, 12 in the run); its new statement must still be a point lookup on the artifacts key, never a scan of every artifact of the set (the `IS` form of the first draft of this plan degraded to `idx_stacking_artifacts_set` plus a scan — ~40 M row visits per `build_plan` on a 1 000-frame set). Pinned in Task 3 by an `EXPLAIN QUERY PLAN` test.

## Plan rulings (refine the spec where the code forced a choice)

- **B1 — 6b is split.** 6b-1 = spec §18 steps 0–2 (this plan), behaviour-preserving and mergeable on its own. 6b-2 = steps 3–14 (the project path). Reason: step 1 alone touches ~250 call sites in a 19.7k-line `run.rs`; landing it as its own reviewed plan is what spec §16 asks for ("lands it first as a behaviour-preserving refactor").
- **B2 — Order inside 6b-1: config pin → table rebuild → db subject → run subject → frame origin.** The spec lists the seam before the rebuild; doing the rebuild first lets the db layer's Project arms be real SQL instead of "needs the rebuild" stubs. The rebuild adds columns and constraints only; no project behaviour.
- **B3 — `stacking_run_frames` keeps its table-level `UNIQUE(run_id, frame_id)` and gains `UNIQUE(run_id, project_frame)`** instead of the spec's two partial unique indexes. `upsert_frame_row`'s `ON CONFLICT(run_id, frame_id)` cannot match a partial index ("ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint", verified in sqlite3 3.54); SQLite treats NULLs as distinct in a UNIQUE constraint, so the two kinds of row never collide, and the existing statement stays valid byte for byte.
- **B4 — The artifacts key gets a new name**, `stacking_artifacts_subject_key`, on the six expressions of spec §4.2, and `upsert_artifact`'s `ON CONFLICT` lists the same six expressions token for token. Every stacking index moves out of the unconditional early index loop (`db/schema.rs:~1284-1310`) into one `create_stacking_indexes` helper that runs AFTER the rebuild point, and that helper first `DROP INDEX IF EXISTS stacking_artifacts_key`. Otherwise the second `init_db` would re-create the old key on the new table.
- **B5 — `foreign_key_check` runs INSIDE the rebuild transaction, before COMMIT**, scoped to the rebuilt tables plus `stacking_run_groups`; a violation rolls the whole rebuild back and `init_db` returns the error. (The `archive_operations` precedent checks after COMMIT; the SQLite recipe's own order is better.) `PRAGMA foreign_keys = OFF` is issued before BEGIN and back ON after COMMIT/ROLLBACK, whatever happened.
- **B6 — A fresh database is created in the NEW shape** (the early `CREATE TABLE IF NOT EXISTS` statements are updated, as the archive precedent did), so the probe (`stacking_runs` lacks `project_id`) skips the rebuild on a fresh DB. Both shapes come from one set of DDL constants, so they cannot drift.
- **B7 — `StackSubject` lives in `db::stacking`** (ungated) and is re-exported as `stacking::StackSubject`; it cannot live in the gated `stacking` module because `db::stacking` and `services::StackHandle` take it in a headless build.
- **B8 — Wire shapes do not move in 6b-1.**
  - `StackingRunRow` keeps `frames_set_id: i64` on the wire (`0` for a project run — unreachable until 6b-2) and gains a `#[serde(skip)] #[ts(skip)] project_id: Option<String>` plus `fn subject()`; code reads `subject()`.
  - `StackingRunFrameRow.frame_id` reads `COALESCE(frame_id, project_frame)` — the frame id in the run's own namespace (spec §9.2: "frameId of a project run is the handle").
  - The non-wire `StackingArtifactRow` and `MasterLight` replace `frames_set_id` with `subject: StackSubject`.
  - Events, `RunSummary` and `StackingPlan` keep `set_id: i64`, filled by one helper `wire_set_id(&subject)`; 6b-2's commands step makes them subject-aware together with the minimal frontend guard.
- **B9 — `FrameOrigin` has ONE variant in 6b-1: `Catalog { file_id }`.** 6b-2 adds `Project { … }`; every `match` on it then fails to compile at exactly the sites that need a project twin. The spec's checklist (§4.3) plus the site it missed — `stage_reference`'s Manual read of `get_frame_set_reference` (`run.rs:~3667`) — are the sites.
- **B10 — Subject-level reads get a subject dispatcher now, with real Project arms where they only need the schema**: the working layout (`<working>/projects/<slug>`, spec §9.3), the display label (`collab_projects.title`), the stored manual reference (`stacking_project_config.reference_frame_uuid` → handle). The registration store's Project arm stores nothing and reads nothing — a project run keeps its registration in the registered artifact's payload (spec §4.2, last paragraph), written in 6b-2. **Open for 6b-2:** `stored_registrations` returns the catalog-shaped `Vec<RegistrationRecord>` (it carries `frames_set_id`); 6b-2 either maps the registered artifacts' payloads into it or changes its consumers (`plan.rs:~1261, ~1910`, `run.rs:~5341, ~5598`) — 6b-2's plan decides, it does not inherit this shape unexamined.
- **B11 — `HashMemo` dispatches the stage-1 hash by origin** (constructed with the run's origin map; a frame with no origin is an error, never a silent catalog read), so its 8 run-side callers and the 9 direct `calibration_hash_for` test calls stay untouched.
- **B12 — Golden stage-hash pins use the pure helpers with literal inputs** (a DB-backed hash carries tempdir paths and mtimes, so it cannot be golden). A second, DB-backed test proves `calibration_hash_for` uses exactly the pinned formula.
- **B13 — Handles are minted by `db::stacking::project_frame_handles` (Task 2)**, in an IMMEDIATE transaction; 6b-1 calls it only from tests.
- **B14 — New schema that 6b-2 fills is created now** (spec §4.2 in one rebuild): `stack_project_frames`, `stacking_project_config`, `collab_projects.canonical_grid_json`, `stacking_run_groups.exposure_class`. Their readers/writers other than the handle minting are 6b-2.

## Review record (fable, 2026-10-02)

Verdict "ready after fixes"; rulings B1–B14 hold; the rebuild SQL, the six-expression ON CONFLICT, the CHECKs and the cascades were run in sqlite3 3.54 as written; the legacy fixture matches `schema.rs@17e2e96e`. Folded in:
- **C1** — `find_artifact` is written in the key's own six expressions (a covering point lookup), pinned by an `EXPLAIN QUERY PLAN` test; Review Focus 6.
- **I1** — the shape test normalizes the rename's quoting and the dropped `IF NOT EXISTS`. **I2** — the heal's log lines and the Conflict text are spelled out per subject. **I3** — `SqliteConnectionManager::setup_connection`. **I4** — the registration store lives in `plan.rs`, `pub(crate)`. **I5** — `solve_of` takes the origins; the fan-out clones the subject and the origins out of `rc`. **I6** — the formula test reads the master paths through the catalog. **I7** — the FK-violation error names the remedy; Review Focus 3 states the startup consequence. **I8** — mechanical completeness greps in Tasks 3 and 4.
- Minor: `subject_label`'s NotFound text, `StackSubject` camelCase serde, the `seed_frame` note, RF5's work usage/cleanup, `index_shape` via stored index SQL, the `RegistrationRecord` shape as a 6b-2 open item (B10), the busy-timeout note, the early index loop disappearing entirely, rusqlite 0.40.

## File map

| File | Change |
| ---- | ---- |
| `crates/athenaeum-core/src/stacking/config.rs` | `calibration_subtree` emits a fixed grouping object; stale comment at ~1488 fixed |
| `crates/athenaeum-core/src/stacking/plan.rs` | golden stage-hash test (Task 1); subject in `build_plan`, `fits_artifact_fresh*`, plan helpers; `HashMemo` origins (Tasks 3–5) |
| `crates/athenaeum-core/src/db/schema.rs` | DDL constants, new-shape fresh CREATEs, `rebuild_stacking_tables_for_subjects`, `create_stacking_indexes`, new tables/columns, tests |
| `crates/athenaeum-core/src/db/stacking.rs` | `StackSubject`; subject-keyed CRUD; NULL-safe readers; `project_frame_handles`; tests |
| `crates/athenaeum-core/src/services/mod.rs` | `StackHandle.subject` |
| `crates/athenaeum-core/src/stacking/mod.rs` | `pub use crate::db::stacking::StackSubject;` and `pub mod origin;` |
| `crates/athenaeum-core/src/stacking/origin.rs` | **new** — `FrameOrigin`, `stored_solve`, `cfa_geometry` |
| `crates/athenaeum-core/src/stacking/paths.rs` | `subject_layout`, `cleanup_work(&StackSubject)` |
| `crates/athenaeum-core/src/stacking/run.rs` | `RunContext.subject` (and `.origins`), `set_id` removed; `start_stacking`, heal, stage sites, `wire_set_id`, registration store, stored reference |
| `crates/athenaeum-core/src/stacking/groups.rs` | `frame_source` dispatcher returning origins |
| `crates/athenaeum-core/src/api/stacking.rs` | internal calls build `StackSubject::FrameSet(set_id)`; host-facing signatures unchanged |
| `crates/athenaeum-core/examples/ln_probe.rs` | `list_runs` / `find_artifact` call sites |
| `crates/athenaeum-web/src/routes/stacking.rs` | test literals only (`NewRun`, `NewMasterLight`) |
| `docs/stacking/README.md` | Task 6: the subject seam and the rebuilt tables |

---

### Task 0: Baseline

**Files:** none.

- [ ] **Step 1: Confirm the branch and a clean tree**

Run: `git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum status -sb`
Expected: `## collab-wave6b` and no changes.

- [ ] **Step 2: Record the baseline gates (numbers go into the SDD ledger)**

Run, one at a time, from the repo root:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-tauri
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract && git diff --exit-code src/types
```
Expected: all green; `git diff` empty. Record every `test result` line. A red test here is pre-existing: note its name, do not fix it in this plan.

---

### Task 1: The config pin (spec §8, §18 step 0)

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/config.rs` (`calibration_subtree` ~654-670; the comment at ~1488-1490)
- Test: `crates/athenaeum-core/src/stacking/plan.rs` (`mod tests`)

**Interfaces:**
- Produces: `calibration_subtree(cfg)["grouping"]` is exactly `{"exposureToleranceSec": <f64>}` whatever fields `GroupingConfig` gains later; the golden test `stage_hashes_are_pinned_to_golden_literals` that 6b-2 extends with its new config fields.

The current function serializes `cfg.grouping` whole, so any new grouping field (6b-2 adds `exposureMode`, `stopsWidth`) would move every set's calibration hash and, through it, the measurement, registration and LN hashes — every set would recompute once (fable finding C1). The golden literals must be recorded on the UNCHANGED code first.

- [ ] **Step 1: Write the golden test with placeholder literals** (bottom of `plan.rs`'s `mod tests`; `use super::*` already sees `measurement_hash_for`, `registration_hash_for`, `normalization_hash_for`, `seeds_calibration_hash_for`)

Verify the spelling of the config paths below against `config.rs` before compiling (`StackingPreset` variants, `preset`, `normalization.local.enabled`, `registration.distortion`, `DistortionChoice::Tps`); change the spelling if it differs, never the intent (one default, the two shipped non-default presets, one custom config with a non-default tolerance, LN on and TPS on).

```rust
    /// Spec 2026-10-02 §8 / plan 6b-1 Task 1 (fable C1): every stage hash of
    /// fixed configs, computed through the SAME pure formula the plan uses,
    /// pinned to literals recorded on `main` BEFORE `calibration_subtree`
    /// stopped serializing `grouping` whole. A later config field that leaks
    /// into a stage subtree moves one of these and fails here — instead of
    /// silently recomputing every frame set's cache on its next run.
    #[test]
    fn stage_hashes_are_pinned_to_golden_literals() {
        use crate::stacking::config::{
            calibration_subtree, preset, stage_hash, SourceIdentity, StackingPreset,
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
        let mut got = Vec::new();
        for (name, cfg) in &configs {
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
            got.push(format!(
                "{name}: calib={calib} meas={meas} reg={reg} ln_ref={ln_ref} ln={ln} ln_cal={ln_cal}"
            ));
        }
        // GOLDEN — recorded on main 17e2e96e before the Task 1 edit.
        assert_eq!(got, vec!["PLACEHOLDER"]);
        assert_eq!(
            serde_json::to_string(&calibration_subtree(&StackingConfig::default())["grouping"])
                .unwrap(),
            r#"{"exposureToleranceSec":2.0}"#
        );
    }
```

- [ ] **Step 2: Record the literals on the unchanged code**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::plan::tests::stage_hashes_are_pinned_to_golden_literals`
Expected: FAIL at the first `assert_eq!`; the `left` side prints the four real lines. Paste them, verbatim, as the four elements of the expected `vec![…]` (one string per config, in order). Re-run: PASS. Commit this test ALONE before touching `config.rs`:
```bash
git add crates/athenaeum-core/src/stacking/plan.rs
git commit -m "test(stacking): pin every stage hash of fixed configs to golden literals" # + trailers
```

- [ ] **Step 3: Add the DB-backed formula check** (same `mod tests`; imitate `fresh_artifacts_and_registration_leave_nothing_stale` at ~4298 for the fixture: `test_fixtures::frame_set`, `add_light`, `add_master_dark_and_flat`, and the link helpers it uses)

```rust
    /// Plan 6b-1 Task 1 (B12): the golden test above pins `stage_hash` over
    /// `calibration_subtree`; this proves `calibration_hash_for` — what a run
    /// and the plan actually call — is exactly that formula, so the golden
    /// literals guard the real cache keys.
    #[test]
    fn calibration_hash_for_is_the_pinned_formula() {
        // Build the same fixture `fresh_artifacts_and_registration_leave_nothing_stale`
        // builds: a frame set, one light, a master dark and flat linked to it.
        // `add_master_dark_and_flat` returns the two CALIBRATION-SET ids, not
        // paths. Read the master paths through the catalog, independently of
        // `calibration_hash_for` (that independence is what makes the test
        // prove the formula):
        //   SELECT fi.path FROM calibration_set_frames csf
        //   JOIN frames fr ON fr.id = csf.frame_id JOIN files fi ON fi.id = fr.file_id
        //   WHERE csf.set_id = ?1
        // (the same query the plan tests already run at plan.rs:~3452), once per set id.
        // Then, with `frame` = that light's `GroupFrame` (from `group_frames`):
        //
        // let expected = stage_hash(
        //     &calibration_subtree(&cfg),
        //     &paths.iter().map(|p| master_identity(p)).collect::<Vec<_>>()   // the fn at ~274
        //         .iter().map(String::as_str).collect::<Vec<_>>(),
        //     &[SourceIdentity { file_id: frame.file_id, size: frame.size, modified_at: frame.modified_at.clone() }],
        // );
        // assert_eq!(calibration_hash_for(&conn, &cfg, &frame, &divisors).unwrap(), expected);
        //
        // Order the master identities exactly as `calibration_hash_for` does
        // (read lines ~314-347 and follow its order; `stage_hash` sorts
        // `upstream` itself, so the order of the slice does not matter).
    }
```
Write the body for real from the fixture of `fresh_artifacts_and_registration_leave_nothing_stale` (the comment above is the recipe, not the code to commit — the committed test has no commented-out code). If `master_identity`'s visibility keeps it out of reach, call it through `super::` — it is in the same file.

Run it: PASS on the unchanged code.

- [ ] **Step 4: Change `calibration_subtree`** (`config.rs:~654-670`)

```rust
pub fn calibration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    // `calibration.format` is a container choice for the calibrated
    // artifact's own file, not a calibration INPUT: it must not move this
    // hash. Pinned by `format_moves_the_run_fingerprint_but_no_stage_hash`.
    let mut calibration =
        serde_json::to_value(&cfg.calibration).expect("CalibratedLightOptions serializes");
    if let Some(obj) = calibration.as_object_mut() {
        obj.remove("format");
    }
    // Spec 2026-10-02 §8 (fable C1): the grouping half is written out field
    // by field, in exactly today's shape. A new `GroupingConfig` field
    // regroups frames (its group keys change, so group-keyed artifacts are
    // recomputed under the new keys) but must never move this hash — that
    // would make every frame set recompute once. Pinned by
    // `stage_hashes_are_pinned_to_golden_literals`.
    serde_json::json!({
        "calibration": calibration,
        "grouping": { "exposureToleranceSec": cfg.grouping.exposure_tolerance_sec },
    })
}
```
Keep the existing first comment text if it differs from the one above; only the `grouping` line and the new comment are the change.

- [ ] **Step 5: Fix the false comment** at `config.rs:~1488-1490` ("the calibrate/measure/register stage hashes never fold in `grouping`"): it now reads that the calibration subtree folds in `grouping.exposureToleranceSec` only, in a fixed shape, and that every other grouping field stays out of every stage hash.

- [ ] **Step 6: Run** `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking::` — PASS, the golden test unchanged. Then the Gate per task (Global Constraints).

- [ ] **Step 7: Commit** — `feat(stacking): calibration subtree emits a fixed grouping shape (config pin)` (+ trailers).

---

### Task 2: The table rebuild, the new tables and the frame handles (spec §4.1, §4.2, §18 step 2)

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (the stacking CREATEs ~1167-1283; the early index loop ~1284-1310; the late section after the `stacking_run_groups.beta` ALTER ~2838; tests)
- Modify: `crates/athenaeum-core/src/db/stacking.rs` (`upsert_artifact`'s `ON CONFLICT` target; `project_frame_handles`; tests)

**Interfaces:**
- Produces:
  - the new shapes below (columns, CHECKs, `UNIQUE`s, index names);
  - `pub fn project_frame_handles(conn: &Connection, project_id: &str, frame_uuids: &[&str]) -> anyhow::Result<std::collections::HashMap<String, i64>>`;
  - `#[cfg(test)] pub(crate) fn downgrade_stacking_tables_to_legacy(conn: &Connection)` in `schema.rs` (Task 3 reuses it).

- [ ] **Step 1: Write the failing tests** (`schema.rs`, a new `#[cfg(test)] mod stacking_subject_schema_tests`; reuse `table_columns` at ~3553 by making it `pub(crate)` inside `mod tests` or copying its 6 lines — prefer making it reachable, no duplication)

The legacy fixture downgrades a FRESH database to the pre-6b shape, so the tests exercise exactly what an upgrading user's database looks like:

```rust
    /// The pre-6b shapes, verbatim from `schema.rs` at 17e2e96e (lines
    /// 1167-1283) — the upgrade input of every rebuild test.
    const LEGACY_STACKING_DDL: &str = "
        CREATE TABLE stacking_runs (
            id INTEGER PRIMARY KEY,
            frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE,
            status TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
            config_json TEXT NOT NULL, config_hash TEXT NOT NULL, reference_frame_id INTEGER,
            reference_mode TEXT NOT NULL, working_dir TEXT NOT NULL, output_dir TEXT NOT NULL,
            summary_json TEXT, error TEXT);
        CREATE TABLE stacking_run_frames (
            id INTEGER PRIMARY KEY,
            run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
            group_id INTEGER NOT NULL REFERENCES stacking_run_groups(id) ON DELETE CASCADE,
            frame_id INTEGER NOT NULL REFERENCES frames(id) ON DELETE CASCADE,
            included INTEGER NOT NULL, exclusion_reason TEXT, weight REAL,
            weight_channels_json TEXT, metrics_json TEXT, reg_status TEXT, reg_model TEXT,
            reg_rms_px REAL, reg_inliers INTEGER, reg_inlier_ratio REAL, reg_flipped INTEGER,
            rejected_fraction REAL,
            UNIQUE(run_id, frame_id));
        CREATE TABLE stacking_artifacts (
            id INTEGER PRIMARY KEY,
            frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE,
            frame_id INTEGER REFERENCES frames(id) ON DELETE CASCADE,
            group_key TEXT NOT NULL, kind TEXT NOT NULL, path TEXT, config_hash TEXT NOT NULL,
            size INTEGER, modified_at TEXT, payload_json TEXT, created_at TEXT NOT NULL);
        CREATE TABLE master_lights (
            id INTEGER PRIMARY KEY,
            frames_set_id INTEGER NOT NULL REFERENCES frames_set(id) ON DELETE CASCADE,
            run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
            group_key TEXT NOT NULL, kind TEXT NOT NULL, path TEXT NOT NULL, format TEXT NOT NULL,
            width INTEGER NOT NULL, height INTEGER NOT NULL, channels INTEGER NOT NULL,
            frames INTEGER NOT NULL, total_exposure_s REAL, created_at TEXT NOT NULL,
            UNIQUE(run_id, group_key, kind));
        CREATE INDEX idx_stacking_runs_set ON stacking_runs(frames_set_id);
        CREATE INDEX idx_stacking_run_frames_run ON stacking_run_frames(run_id);
        CREATE INDEX idx_stacking_run_frames_frame ON stacking_run_frames(frame_id);
        CREATE INDEX idx_stacking_run_frames_group ON stacking_run_frames(group_id);
        CREATE INDEX idx_stacking_artifacts_set ON stacking_artifacts(frames_set_id);
        CREATE INDEX idx_stacking_artifacts_frame ON stacking_artifacts(frame_id);
        CREATE UNIQUE INDEX stacking_artifacts_key
            ON stacking_artifacts(frames_set_id, group_key, kind, COALESCE(frame_id, 0));
        CREATE INDEX idx_master_lights_set ON master_lights(frames_set_id);
        CREATE INDEX idx_master_lights_run ON master_lights(run_id);";
```
Before relying on it, diff the four `CREATE TABLE` bodies above against `git show 17e2e96e:crates/athenaeum-core/src/db/schema.rs` lines 1167-1283 and fix any column the transcription got wrong — the fixture must be the real old shape.

```rust
    /// Turn a fresh (new-shape) database into the pre-6b shape: drop the four
    /// subject tables and the two new tables, re-create the legacy ones.
    /// MODULE LEVEL in `schema.rs` (outside any `mod tests`), `#[cfg(test)]`,
    /// together with `LEGACY_STACKING_DDL`, so Task 3 reaches it as
    /// `crate::db::schema::downgrade_stacking_tables_to_legacy`.
    #[cfg(test)]
    pub(crate) fn downgrade_stacking_tables_to_legacy(conn: &Connection) {
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute_batch(
            "DROP TABLE master_lights; DROP TABLE stacking_artifacts;
             DROP TABLE stacking_run_frames; DROP TABLE stacking_runs;
             DROP TABLE stacking_project_config; DROP TABLE stack_project_frames;",
        )
        .unwrap();
        conn.execute_batch(LEGACY_STACKING_DDL).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        // `canonical_grid_json` / `exposure_class` stay: an ALTER-added column
        // is guarded by `column_exists`, so leaving them is what an older
        // database that already got them would look like — harmless.
    }
```

Seed helper and the tests (seed through raw SQL in the legacy shape — the CRUD layer speaks the new shape):

```rust
    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap(); // db/mod.rs:~38-51, as stacking/test_fixtures.rs:~55 calls it
        init_db(&conn).unwrap();
        conn
    }

    /// A frame set with two lights, a run with one group, two run-frame rows,
    /// a per-frame and a group-level (NULL frame) artifact, a master light —
    /// written in the LEGACY shape (the CRUD layer speaks the new one). The
    /// parent INSERTs are the ones `db/stacking.rs`'s `seed_set`/`seed_frame`
    /// use (`frames_set (id, name)`, `files (id, path, filename, size,
    /// modified_at, format)`, `frames (id, file_id)`).
    fn seed_legacy_stacking_rows(conn: &Connection) -> (i64, i64, i64) {
        let (set, f1, f2) = (7_i64, 101_i64, 102_i64);
        conn.execute("INSERT INTO frames_set (id, name) VALUES (?1, 'LDN 1272')", [set]).unwrap();
        for f in [f1, f2] {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'f.fits', 100, '2026-09-09T00:00:00Z', 'FITS')",
                rusqlite::params![f, format!("/f/{f}.fits")],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id, file_id) VALUES (?1, ?1)", [f]).unwrap();
        }
        conn.execute_batch(&format!(
            "INSERT INTO stacking_runs (id, frames_set_id, status, started_at, config_json,
               config_hash, reference_mode, working_dir, output_dir)
             VALUES (1, {set}, 'done', 't', '{{}}', 'h', 'auto', '/w', '/o');
             INSERT INTO stacking_run_groups (id, run_id, group_key, color_mode, frame_count,
               included_count, status)
             VALUES (1, 1, 'mono__L__300s', 'mono', 2, 2, 'done');
             INSERT INTO stacking_run_frames (run_id, group_id, frame_id, included)
             VALUES (1, 1, {f1}, 1), (1, 1, {f2}, 0);
             INSERT INTO stacking_artifacts (frames_set_id, frame_id, group_key, kind, path,
               config_hash, created_at)
             VALUES ({set}, {f1}, 'mono__L__300s', 'calibrated', '/w/c1.fits', 'c1', 't'),
                    ({set}, NULL, 'mono__L__300s', 'ln_reference', '/w/lnref.fits', 'lr', 't');
             INSERT INTO master_lights (frames_set_id, run_id, group_key, kind, path, format,
               width, height, channels, frames, created_at)
             VALUES ({set}, 1, 'mono__L__300s', 'master', '/o/m.fits', 'fits', 10, 10, 1, 2, 't');"
        ))
        .unwrap();
        (set, f1, f2)
    }

    /// `(runs, groups, run_frames, artifacts, master_lights)` row counts.
    fn stacking_row_counts(conn: &Connection) -> (i64, i64, i64, i64, i64) {
        let n = |t: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()
        };
        (
            n("stacking_runs"),
            n("stacking_run_groups"),
            n("stacking_run_frames"),
            n("stacking_artifacts"),
            n("master_lights"),
        )
    }

```rust
    #[test]
    fn the_subject_rebuild_preserves_every_row_and_child() {
        let conn = mem_db();
        downgrade_stacking_tables_to_legacy(&conn);
        let (set, f1, _f2) = seed_legacy_stacking_rows(&conn);
        let before = stacking_row_counts(&conn); // (runs, groups, run_frames, artifacts, masters)
        init_db(&conn).unwrap();
        assert!(column_exists(&conn, "stacking_runs", "project_id").unwrap());
        assert_eq!(stacking_row_counts(&conn), before);
        // Children still join their parents (no cascade wipe):
        let joined: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stacking_run_frames rf
             JOIN stacking_runs r ON r.id = rf.run_id
             JOIN stacking_run_groups g ON g.id = rf.group_id", [], |r| r.get(0)).unwrap();
        assert_eq!(joined, 2);
        let (fs, frame, pf): (Option<i64>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT frames_set_id, frame_id, project_frame FROM stacking_artifacts WHERE kind = 'calibrated'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((fs, frame, pf), (Some(set), Some(f1), None));
        let violations: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0)).unwrap();
        assert_eq!(violations, 0);
        let fk_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk_on, 1);
        // Idempotent:
        init_db(&conn).unwrap();
        assert_eq!(stacking_row_counts(&conn), before);
    }

    #[test]
    fn fresh_and_migrated_stacking_tables_have_the_same_shape() {
        let fresh = mem_db();
        let migrated = mem_db();
        downgrade_stacking_tables_to_legacy(&migrated);
        init_db(&migrated).unwrap();
        init_db(&migrated).unwrap(); // a second start must not re-add the old key
        for table in [
            "stacking_runs", "stacking_run_frames", "stacking_artifacts", "master_lights",
            "stacking_run_groups", "stack_project_frames", "stacking_project_config",
        ] {
            assert_eq!(table_columns(&fresh, table), table_columns(&migrated, table), "{table}");
            assert_eq!(index_shape(&fresh, table), index_shape(&migrated, table), "{table}");
            assert_eq!(table_sql_normalized(&fresh, table), table_sql_normalized(&migrated, table), "{table}");
        }
        for conn in [&fresh, &migrated] {
            let old: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='stacking_artifacts_key'",
                [], |r| r.get(0)).unwrap();
            assert_eq!(old, 0, "the pre-6b artifacts key must never come back");
        }
    }
```
`index_shape(conn, table) -> Vec<(String, Option<String>)>`: every `(name, sql)` row of `SELECT name, sql FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 ORDER BY name`, with `sql` whitespace-collapsed (`sql` is NULL for an autoindex; its name follows the constraint order, which the shared DDL constants make identical). Comparing the stored `sql` text per index name is what compares expression indexes faithfully (`PRAGMA index_xinfo` reports an expression column as `cid = -2`, `name = NULL`). `table_sql_normalized(conn, table)` = the table's `sqlite_master.sql` with, in this order: `"<table>"` replaced by `T` (after `ALTER TABLE … RENAME` SQLite stores the new name double-quoted: `CREATE TABLE "stacking_runs" (`), then `<table>` replaced by `T`, then `IF NOT EXISTS ` removed (SQLite drops it from the stored text — verified), then whitespace collapsed. Once both shapes come from the same body constant, the two strings are equal.

```rust
    #[test]
    fn a_rebuild_with_a_foreign_key_violation_rolls_back_and_surfaces_the_error() {
        let conn = mem_db();
        downgrade_stacking_tables_to_legacy(&conn);
        let (_set, _f1, _f2) = seed_legacy_stacking_rows(&conn);
        // A run-frame pointing at a frame that does not exist, written with FKs off:
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute(
            "INSERT INTO stacking_run_frames (run_id, group_id, frame_id, included) VALUES (1, 1, 999999, 1)",
            []).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        let before = stacking_row_counts(&conn);
        let err = init_db(&conn).unwrap_err().to_string();
        assert!(err.contains("foreign-key"), "{err}");
        assert!(!column_exists(&conn, "stacking_runs", "project_id").unwrap(), "rolled back");
        assert_eq!(stacking_row_counts(&conn), before);
        let fk_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk_on, 1, "foreign keys back on after a failed rebuild");
    }

    #[test]
    fn subject_and_frame_pairs_are_enforced() {
        let conn = mem_db();
        // A run with both subjects, or neither, is refused:
        let both = conn.execute(
            "INSERT INTO stacking_runs (frames_set_id, project_id, status, started_at, config_json,
               config_hash, reference_mode, working_dir, output_dir)
             VALUES (NULL, NULL, 'done', 't', '{}', 'h', 'auto', '/w', '/o')", []);
        assert!(both.is_err());
        // An artifact with a project frame under a frame-set subject is refused
        // (seed a frames_set row first; the CHECK fires before the FK).
        // A run-frame with both frame columns, or neither, is refused.
    }

    #[test]
    fn deleting_a_frame_set_or_a_frame_still_cascades_after_the_rebuild() {
        let conn = mem_db();
        downgrade_stacking_tables_to_legacy(&conn);
        let (set, f1, _f2) = seed_legacy_stacking_rows(&conn);
        init_db(&conn).unwrap();
        conn.execute("DELETE FROM frames WHERE id = ?1", [f1]).unwrap();
        let rf: i64 = conn.query_row("SELECT COUNT(*) FROM stacking_run_frames WHERE frame_id = ?1", [f1], |r| r.get(0)).unwrap();
        assert_eq!(rf, 0);
        conn.execute("DELETE FROM frames_set WHERE id = ?1", [set]).unwrap();
        assert_eq!(stacking_row_counts(&conn), (0, 0, 0, 0, 0));
    }
```
Complete `subject_and_frame_pairs_are_enforced` with the two cases its comments name (real INSERTs, `is_err()` asserts). 

In `db/stacking.rs`'s `mod tests`:
```rust
    #[test]
    fn project_frame_handles_are_stable_and_never_reused() {
        let conn = conn(); // the module's existing helper (~902)
        insert_test_project(&conn, "p1");
        insert_test_project(&conn, "p2");
        let a = project_frame_handles(&conn, "p1", &["u-a", "u-b"]).unwrap();
        let b = project_frame_handles(&conn, "p1", &["u-b", "u-c"]).unwrap();
        assert_eq!(a["u-b"], b["u-b"], "a uuid keeps its handle");
        assert_ne!(b["u-c"], a["u-a"]);
        assert_ne!(b["u-c"], a["u-b"]);
        let other = project_frame_handles(&conn, "p2", &["u-a"]).unwrap();
        assert_ne!(other["u-a"], a["u-a"], "handles are per project");
        conn.execute("DELETE FROM collab_projects WHERE project_id = 'p1'", []).unwrap();
        let left: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stack_project_frames WHERE project_id = 'p1'", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "handles live as long as their project");
        let c = project_frame_handles(&conn, "p2", &[]).unwrap();
        assert!(c.is_empty());
    }
```
`insert_test_project(conn, id)` is ALWAYS defined as a module-level `#[cfg(test)] pub(crate) fn insert_test_project(conn: &Connection, project_id: &str)` in `db/stacking.rs` (Tasks 3 and 4 call it by this path). Its body may delegate to an existing `db::collab` test helper that builds a `CollabProjectRow` and calls `upsert_project` (grep `fn sample_project`, `fn project_row` in `db/collab.rs` tests and `test_support.rs`); otherwise it runs this SQL (every NOT NULL column of `collab_projects`, schema.rs ~2263-2286; ALTER-added columns all have defaults or are nullable — confirm while writing it). Slug and title are the project id:
```sql
INSERT INTO collab_projects (project_id, slug, title, data_role, target_name, target_ra_deg,
  target_dec_deg, target_radius_deg, membership_version, snapshot_payload_b64,
  snapshot_signature_b64, members_json)
VALUES (?1, ?1, ?1, 'processor', 'M42', 83.8, -5.4, 1.0, 1, '', '', '[]')
```

- [ ] **Step 2: Run them** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 stacking_subject_schema_tests project_frame_handles` → FAIL (no `project_id` column, no `stack_project_frames`, no `project_frame_handles`).

- [ ] **Step 3: The DDL constants and the fresh CREATEs** (`schema.rs`)

Define, near the stacking CREATEs, one body constant per rebuilt table. Copy every pre-existing column line VERBATIM from the current CREATE (the shapes below show the additions; the implementer keeps the existing columns' exact types and order and inserts the new ones where shown):

```rust
/// Wave 6b-1 (spec 2026-10-02 §4.2, plan rulings B3–B6): the stacking tables
/// that carry a run SUBJECT have a subject pair — `frames_set_id` for a frame
/// set, `project_id` for a collaboration project, exactly one set — and, per
/// frame, a frame pair — `frame_id` (a catalog `frames` row) or
/// `project_frame` (a `stack_project_frames` handle). ONE body per table,
/// used by both the fresh `CREATE TABLE IF NOT EXISTS` and the upgrade
/// rebuild, so the two shapes cannot drift
/// (`fresh_and_migrated_stacking_tables_have_the_same_shape`).
const STACKING_RUNS_BODY: &str = "(
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER REFERENCES frames_set(id) ON DELETE CASCADE,
    project_id TEXT REFERENCES collab_projects(project_id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    config_json TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    reference_frame_id INTEGER,
    reference_mode TEXT NOT NULL,
    working_dir TEXT NOT NULL,
    output_dir TEXT NOT NULL,
    summary_json TEXT,
    error TEXT,
    CHECK ((frames_set_id IS NULL) <> (project_id IS NULL))
)";

const STACKING_RUN_FRAMES_BODY: &str = "(
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
    group_id INTEGER NOT NULL REFERENCES stacking_run_groups(id) ON DELETE CASCADE,
    frame_id INTEGER REFERENCES frames(id) ON DELETE CASCADE,
    project_frame INTEGER REFERENCES stack_project_frames(id) ON DELETE CASCADE,
    included INTEGER NOT NULL,
    exclusion_reason TEXT,
    weight REAL,
    weight_channels_json TEXT,
    metrics_json TEXT,
    reg_status TEXT,
    reg_model TEXT,
    reg_rms_px REAL,
    reg_inliers INTEGER,
    reg_inlier_ratio REAL,
    reg_flipped INTEGER,
    rejected_fraction REAL,
    UNIQUE(run_id, frame_id),
    UNIQUE(run_id, project_frame),
    CHECK ((frame_id IS NULL) <> (project_frame IS NULL))
)";

const STACKING_ARTIFACTS_BODY: &str = "(
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER REFERENCES frames_set(id) ON DELETE CASCADE,
    project_id TEXT REFERENCES collab_projects(project_id) ON DELETE CASCADE,
    frame_id INTEGER REFERENCES frames(id) ON DELETE CASCADE,
    project_frame INTEGER REFERENCES stack_project_frames(id) ON DELETE CASCADE,
    group_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    path TEXT,
    config_hash TEXT NOT NULL,
    size INTEGER,
    modified_at TEXT,
    payload_json TEXT,
    created_at TEXT NOT NULL,
    CHECK ((frames_set_id IS NULL) <> (project_id IS NULL)),
    CHECK (frame_id IS NULL OR project_frame IS NULL),
    CHECK (frame_id IS NULL OR frames_set_id IS NOT NULL),
    CHECK (project_frame IS NULL OR project_id IS NOT NULL)
)";

const MASTER_LIGHTS_BODY: &str = "(
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER REFERENCES frames_set(id) ON DELETE CASCADE,
    project_id TEXT REFERENCES collab_projects(project_id) ON DELETE CASCADE,
    run_id INTEGER NOT NULL REFERENCES stacking_runs(id) ON DELETE CASCADE,
    group_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    format TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    channels INTEGER NOT NULL,
    frames INTEGER NOT NULL,
    total_exposure_s REAL,
    created_at TEXT NOT NULL,
    UNIQUE(run_id, group_key, kind),
    CHECK ((frames_set_id IS NULL) <> (project_id IS NULL))
)";
```
Replace the four early `CREATE TABLE IF NOT EXISTS` statements with `format!("CREATE TABLE IF NOT EXISTS stacking_runs {STACKING_RUNS_BODY}")` etc., issued the same way the current code issues them. `stacking_run_groups`, `stacking_set_config` keep their CREATEs. Every statement of the early index loop (~1284-1310) is a stacking statement, so the loop disappears entirely; its statements move to Step 4's helper, and its two comments (the rowid-alias note on `idx_stacking_set_config_set`, the M4d note on `master_lights`) move with them.

- [ ] **Step 4: The late section** (`schema.rs`, right after the `stacking_run_groups.beta` ALTER at ~2838-2840; `collab_projects` already exists there)

```rust
    // ── Wave 6b-1: stacking subjects (spec 2026-10-02 §4.1/§4.2) ─────────────
    // The project-frame handle: a project frame's stable i64 inside the
    // stacking pipeline, minted the first time a plan includes it and never
    // re-used. NO foreign key to `project_frames_local` on purpose — that
    // table's rows are deleted and re-inserted by ordinary exchange writes
    // (`delete_not_in`, `record_own`'s INSERT OR REPLACE), and a cascade from
    // there would wipe a frame's run rows and artifacts and re-mint its
    // handle. A handle lives as long as its project.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS stack_project_frames (
            id INTEGER PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES collab_projects(project_id) ON DELETE CASCADE,
            frame_uuid TEXT NOT NULL,
            UNIQUE (project_id, frame_uuid)
        )",
        [],
    )?;
    // The per-project stacking document (6b-2 reads and writes it): whole-
    // config precedence as for a set, manual exclusions and the manual
    // reference by frame uuid.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS stacking_project_config (
            project_id TEXT PRIMARY KEY REFERENCES collab_projects(project_id) ON DELETE CASCADE,
            config_json TEXT NOT NULL,
            excluded_frame_uuids_json TEXT NOT NULL DEFAULT '[]',
            reference_frame_uuid TEXT,
            updated_at TEXT NOT NULL
        )",
        [],
    )?;
    // The exposure-class token of a group (spec §8), shown by the Groups table.
    if !column_exists(conn, "stacking_run_groups", "exposure_class")? {
        conn.execute("ALTER TABLE stacking_run_groups ADD COLUMN exposure_class TEXT", [])?;
    }
    // The hub's canonical grid (spec §6.1), refreshed with the project meta by 6b-2.
    if !column_exists(conn, "collab_projects", "canonical_grid_json")? {
        conn.execute("ALTER TABLE collab_projects ADD COLUMN canonical_grid_json TEXT", [])?;
    }
    rebuild_stacking_tables_for_subjects(conn)?;
    create_stacking_indexes(conn)?;
```

The two functions (module level, near `column_exists`):

```rust
/// Upgrade an existing database's four subject tables to the wave-6b shape
/// (plan 6b-1 rulings B3–B6). SQLite cannot relax a NOT NULL or add a CHECK
/// through ALTER, so this is the 12-step rebuild: foreign-key enforcement
/// OFF before BEGIN (the pragma is a silent no-op inside a transaction, and
/// with it ON `DROP TABLE stacking_runs` would cascade-delete every group,
/// run-frame and master-light row), then one IMMEDIATE transaction that
/// creates, copies, drops and renames, and checks `foreign_key_check` on
/// the rebuilt tables BEFORE committing. A violation rolls everything back
/// and is returned (B5). Foreign keys go back ON whatever happened.
/// Detected by the absence of `stacking_runs.project_id`, so it runs once;
/// a fresh database is created in the new shape and never gets here.
fn rebuild_stacking_tables_for_subjects(conn: &Connection) -> Result<()> {
    if column_exists(conn, "stacking_runs", "project_id")? {
        return Ok(());
    }
    conn.pragma_update(None, "foreign_keys", false)?;
    let outcome = (|| -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        // Another process (desktop and web may share one catalog) may have
        // rebuilt between our probe and our write lock: `INIT_DB_LOCK` is
        // process-wide only. Re-check under the lock. (A second process whose
        // BEGIN IMMEDIATE waits past `busy_timeout` — 5 s — on a very large
        // catalog fails its startup once with SQLITE_BUSY; its next start
        // finds the rebuilt shape.)
        if column_exists(&tx, "stacking_runs", "project_id")? {
            return Ok(());
        }
        tx.execute_batch(&format!(
            "CREATE TABLE stacking_runs_new {STACKING_RUNS_BODY};
             INSERT INTO stacking_runs_new (id, frames_set_id, project_id, status, started_at,
               finished_at, config_json, config_hash, reference_frame_id, reference_mode,
               working_dir, output_dir, summary_json, error)
             SELECT id, frames_set_id, NULL, status, started_at, finished_at, config_json,
               config_hash, reference_frame_id, reference_mode, working_dir, output_dir,
               summary_json, error FROM stacking_runs;
             CREATE TABLE stacking_run_frames_new {STACKING_RUN_FRAMES_BODY};
             INSERT INTO stacking_run_frames_new (id, run_id, group_id, frame_id, project_frame,
               included, exclusion_reason, weight, weight_channels_json, metrics_json, reg_status,
               reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio, reg_flipped, rejected_fraction)
             SELECT id, run_id, group_id, frame_id, NULL, included, exclusion_reason, weight,
               weight_channels_json, metrics_json, reg_status, reg_model, reg_rms_px, reg_inliers,
               reg_inlier_ratio, reg_flipped, rejected_fraction FROM stacking_run_frames;
             CREATE TABLE stacking_artifacts_new {STACKING_ARTIFACTS_BODY};
             INSERT INTO stacking_artifacts_new (id, frames_set_id, project_id, frame_id,
               project_frame, group_key, kind, path, config_hash, size, modified_at, payload_json,
               created_at)
             SELECT id, frames_set_id, NULL, frame_id, NULL, group_key, kind, path, config_hash,
               size, modified_at, payload_json, created_at FROM stacking_artifacts;
             CREATE TABLE master_lights_new {MASTER_LIGHTS_BODY};
             INSERT INTO master_lights_new (id, frames_set_id, project_id, run_id, group_key, kind,
               path, format, width, height, channels, frames, total_exposure_s, created_at)
             SELECT id, frames_set_id, NULL, run_id, group_key, kind, path, format, width, height,
               channels, frames, total_exposure_s, created_at FROM master_lights;
             DROP TABLE master_lights;
             DROP TABLE stacking_artifacts;
             DROP TABLE stacking_run_frames;
             DROP TABLE stacking_runs;
             ALTER TABLE stacking_runs_new RENAME TO stacking_runs;
             ALTER TABLE stacking_run_frames_new RENAME TO stacking_run_frames;
             ALTER TABLE stacking_artifacts_new RENAME TO stacking_artifacts;
             ALTER TABLE master_lights_new RENAME TO master_lights;"
        ))?;
        for table in [
            "stacking_runs",
            "stacking_run_groups",
            "stacking_run_frames",
            "stacking_artifacts",
            "master_lights",
        ] {
            let violations: i64 = tx.query_row(
                &format!("SELECT COUNT(*) FROM pragma_foreign_key_check('{table}')"),
                [],
                |r| r.get(0),
            )?;
            if violations > 0 {
                tracing::error!(table, count = violations, "stacking subject rebuild refused: foreign-key violations");
                // `tx` is dropped un-committed below: rusqlite rolls it back.
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some(format!(
                        "stacking subject rebuild refused: {violations} foreign-key violation(s) \
                         in {table}; the database was left unchanged and the app will not start \
                         until they are repaired. `PRAGMA foreign_key_check('{table}')` lists the \
                         rows; deleting those orphan rows is safe (stacking cache and run-history \
                         rows only)."
                    )),
                ));
            }
        }
        tx.commit()?;
        tracing::info!("stacking tables rebuilt for run subjects");
        Ok(())
    })();
    let re_enable = conn.pragma_update(None, "foreign_keys", true);
    outcome?;
    re_enable?;
    Ok(())
}

/// Every index of the stacking tables, created AFTER the subject rebuild
/// point (B4): the early unconditional index loop would otherwise re-create
/// the pre-6b `stacking_artifacts_key` on the rebuilt table at the next
/// start. Every foreign-key child column has an index whose leftmost column
/// it is (`every_foreign_key_child_column_is_indexed`).
fn create_stacking_indexes(conn: &Connection) -> Result<()> {
    for stmt in [
        "DROP INDEX IF EXISTS stacking_artifacts_key",
        "CREATE INDEX IF NOT EXISTS idx_stacking_runs_set ON stacking_runs(frames_set_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_runs_project ON stacking_runs(project_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_run_groups_run ON stacking_run_groups(run_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_run ON stacking_run_frames(run_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_frame ON stacking_run_frames(frame_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_project_frame ON stacking_run_frames(project_frame)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_group ON stacking_run_frames(group_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_set ON stacking_artifacts(frames_set_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_project ON stacking_artifacts(project_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_frame ON stacking_artifacts(frame_id)",
        "CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_project_frame ON stacking_artifacts(project_frame)",
        "CREATE UNIQUE INDEX IF NOT EXISTS stacking_artifacts_subject_key ON stacking_artifacts(
           COALESCE(frames_set_id, 0), COALESCE(project_id, ''), group_key, kind,
           COALESCE(frame_id, 0), COALESCE(project_frame, 0))",
        "CREATE INDEX IF NOT EXISTS idx_stacking_set_config_set ON stacking_set_config(frames_set_id)",
        "CREATE INDEX IF NOT EXISTS idx_master_lights_set ON master_lights(frames_set_id)",
        "CREATE INDEX IF NOT EXISTS idx_master_lights_project ON master_lights(project_id)",
        "CREATE INDEX IF NOT EXISTS idx_master_lights_run ON master_lights(run_id)",
    ] {
        conn.execute(stmt, [])?;
    }
    Ok(())
}
```
Carry the explanatory comments that sat on the moved statements in the early loop (the `idx_stacking_set_config_set` rowid-alias note, the M4d note) into this helper. If `rusqlite::Transaction::new_unchecked` with `Immediate` is not how `schema.rs` already opens transactions, keep it anyway — `db/collab.rs:290` uses exactly this call.

- [ ] **Step 5: `upsert_artifact`'s conflict target** (`db/stacking.rs:~661`): replace `ON CONFLICT(frames_set_id, group_key, kind, COALESCE(frame_id, 0))` with
```sql
ON CONFLICT(COALESCE(frames_set_id, 0), COALESCE(project_id, ''), group_key, kind,
            COALESCE(frame_id, 0), COALESCE(project_frame, 0)) DO UPDATE SET
```
(the six expressions must match the index token for token; their order does not matter) and update its doc comment to name the new key. The INSERT column list does not change in this task (the new columns default to NULL).

- [ ] **Step 6: `project_frame_handles`** (`db/stacking.rs`, a new section after Master lights)

```rust
// ---------------------------------------------------------------------
// Project-frame handles (wave 6b, spec 2026-10-02 §4.1)
// ---------------------------------------------------------------------

/// The stable `i64` handle of each `(project_id, frame_uuid)`, minted on
/// first use and never re-used for another frame: inside the stacking
/// pipeline a project frame is this id, exactly as a catalog frame is its
/// `frames.id`. A new content version keeps the uuid and therefore the
/// handle (its artifacts go stale through the content hash). Handles live
/// as long as their project (`collab_projects` cascade) and are never
/// deleted otherwise.
///
/// One IMMEDIATE transaction (read-then-write, CLAUDE.md): never call this
/// with a transaction already open on `conn`.
pub fn project_frame_handles(
    conn: &Connection,
    project_id: &str,
    frame_uuids: &[&str],
) -> Result<std::collections::HashMap<String, i64>> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut out = std::collections::HashMap::with_capacity(frame_uuids.len());
    {
        let mut insert = tx.prepare_cached(
            "INSERT OR IGNORE INTO stack_project_frames (project_id, frame_uuid) VALUES (?1, ?2)",
        )?;
        let mut select = tx.prepare_cached(
            "SELECT id FROM stack_project_frames WHERE project_id = ?1 AND frame_uuid = ?2",
        )?;
        for uuid in frame_uuids {
            insert.execute(params![project_id, uuid])?;
            let id: i64 = select.query_row(params![project_id, uuid], |r| r.get(0))?;
            out.insert((*uuid).to_string(), id);
        }
    }
    tx.commit()?;
    Ok(out)
}
```

- [ ] **Step 7: Run the new tests, then the whole Gate per task.** Pay attention to: `every_foreign_key_child_column_is_indexed`, `init_db_twice_keeps_the_stacking_tables`, `artifact_key_treats_null_frame_as_one_row`, `master_light_round_trip_and_cascade`, `concurrent_init_db_on_shared_file_is_race_free`, every `archive_operations_rebuild_*` test. Expected: all PASS.

- [ ] **Step 8: Rehearse on a copy of the real catalog** (spec §4.2: "tested on a copy of a real database before merge")

Add an `#[ignore]`d test in `stacking_subject_schema_tests`:
```rust
    /// Run by hand: `ATHENAEUM_REHEARSAL_DB=<path to a COPY> cargo test -p athenaeum-core
    /// --lib -- --ignored the_subject_rebuild_rehearses_on_a_real_catalog_copy`.
    /// Never point it at a live database: it upgrades the file it is given.
    #[test]
    #[ignore = "needs ATHENAEUM_REHEARSAL_DB pointing at a copy of a real catalog"]
    fn the_subject_rebuild_rehearses_on_a_real_catalog_copy() {
        let path = std::env::var("ATHENAEUM_REHEARSAL_DB").expect("ATHENAEUM_REHEARSAL_DB");
        let conn = Connection::open(&path).unwrap();
        crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap();
        let before = stacking_row_counts(&conn);
        init_db(&conn).unwrap();
        assert!(column_exists(&conn, "stacking_runs", "project_id").unwrap());
        assert_eq!(stacking_row_counts(&conn), before);
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(violations, 0);
        init_db(&conn).unwrap();
        assert_eq!(stacking_row_counts(&conn), before);
    }
```
Make the copy and run it (the copy lives under the session scratchpad; never commit it):
```bash
SCR=/private/tmp/claude-501/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/4c1ccbcf-d168-4afc-ac2c-170661456637/scratchpad
sqlite3 -readonly "$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db" ".backup $SCR/rehearsal.db"
ATHENAEUM_REHEARSAL_DB=$SCR/rehearsal.db CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --ignored the_subject_rebuild_rehearses_on_a_real_catalog_copy
rm -f "$SCR/rehearsal.db" "$SCR/rehearsal.db-wal" "$SCR/rehearsal.db-shm"
```
Expected: PASS. Record in the report the row counts it saw (the research run saw runs 4, groups 8, run-frames 1104, artifacts 368, masters 6). If the `.dev` catalog is absent, say so in the report — do not substitute the production catalog.

- [ ] **Step 9: Commit** — `feat(db): rebuild the stacking tables with run-subject and frame pairs; project-frame handles` (+ trailers).

---

### Task 3: `StackSubject` and subject-keyed persistence (spec §4.3; seam inventory C1)

**Files:**
- Modify: `crates/athenaeum-core/src/db/stacking.rs` (the type; every subject-keyed fn; NULL-safe readers; tests)
- Modify: `crates/athenaeum-core/src/services/mod.rs` (`StackHandle`)
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (re-export)
- Modify: `crates/athenaeum-core/src/stacking/paths.rs` (`cleanup_work`)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (`fits_artifact_fresh`, `fits_artifact_fresh_for_planes`, call sites)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (`RunContext.subject` BESIDE `set_id`; every db call site moves to `&rc.subject`)
- Modify: `crates/athenaeum-core/src/api/stacking.rs`, `crates/athenaeum-core/examples/ln_probe.rs`, `crates/athenaeum-web/src/routes/stacking.rs` (call sites / test literals)

**Interfaces:**
- Consumes: Task 2's schema, `downgrade_stacking_tables_to_legacy`, `insert_test_project`.
- Produces (exact, later tasks and 6b-2 rely on them):

```rust
pub enum StackSubject { FrameSet(i64), Project(String) }   // Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize
impl StackSubject {
    pub fn frame_set_id(&self) -> Option<i64>;
    pub fn project_id(&self) -> Option<&str>;
}
impl std::fmt::Display for StackSubject; // "frame set {id}" | "project {id}"

pub struct NewRun<'a>        { pub subject: &'a StackSubject, /* other fields unchanged */ }
pub struct NewArtifact<'a>   { pub subject: &'a StackSubject, pub frame_id: Option<i64>, /* … */ }
pub struct NewMasterLight<'a>{ pub subject: &'a StackSubject, /* … */ }
pub struct StackingRunRow    { /* wire unchanged */ #[serde(skip)] #[ts(skip)] pub project_id: Option<String> }
impl StackingRunRow { pub fn subject(&self) -> StackSubject; }
pub struct StackingArtifactRow { pub subject: StackSubject, pub frame_id: Option<i64> /* namespace id */, /* … */ }
pub struct MasterLight       { pub subject: StackSubject, /* … */ }

pub fn list_runs(conn, subject: &StackSubject, limit: usize) -> Result<Vec<StackingRunRow>>;
pub fn active_run_for_subject(conn, subject: &StackSubject) -> Result<Option<i64>>; // renamed from active_run_for_set
pub fn list_unfinished_runs(conn) -> Result<Vec<(i64, StackSubject)>>;
pub fn upsert_frame_row(conn, subject: &StackSubject, f: &NewFrameRow<'_>) -> Result<i64>;
pub fn set_frame_rejected_fraction(conn, subject: &StackSubject, run_id: i64, frame_id: i64, fraction: f64) -> Result<()>;
pub fn find_artifact(conn, subject: &StackSubject, group_key: &str, kind: &str, frame_id: Option<i64>) -> Result<Option<StackingArtifactRow>>;
pub fn list_artifacts(conn, subject: &StackSubject, kind: Option<&str>) -> Result<Vec<StackingArtifactRow>>;
pub fn delete_artifacts(conn, subject: &StackSubject, kinds: &[&str]) -> Result<usize>;
pub fn delete_all_artifacts(conn, subject: &StackSubject) -> Result<usize>;
// services::StackHandle { pub cancel_flag: Arc<AtomicBool>, pub subject: StackSubject }
// paths::cleanup_work(conn, subject: &StackSubject, layout, what)
// plan::fits_artifact_fresh(conn, subject: &StackSubject, …), fits_artifact_fresh_for_planes(conn, subject: &StackSubject, …)
// RunContext { pub(crate) subject: StackSubject, pub(crate) set_id: i64 /* removed in Task 4 */, … }
```
`get_set_config`, `set_set_config`, `SetConfigRow`, `insert_run`'s other fields, `list_frame_rows`, `get_run`, `set_run_status`, `set_run_reference`, `finish_run`, the group functions, `list_master_lights`, `find_master_light` keep their signatures (they are keyed by run or are frame-set-only stores).

- [ ] **Step 1: Write the failing tests** (`db/stacking.rs` `mod tests`)

```rust
    #[test]
    fn subject_display_and_columns() {
        let fs = StackSubject::FrameSet(7);
        let pr = StackSubject::Project("p1".into());
        assert_eq!(fs.to_string(), "frame set 7");
        assert_eq!(pr.to_string(), "project p1");
        assert_eq!((fs.frame_set_id(), fs.project_id()), (Some(7), None));
        assert_eq!((pr.frame_set_id(), pr.project_id()), (None, Some("p1")));
    }

    #[test]
    fn frame_set_and_project_artifacts_never_collide() {
        let c = conn();
        let set = seed_set(&c);            // existing helper (~912)
        let frame = 501;
        seed_frame(&c, frame);             // existing helper (~926): inserts a `files` + `frames` row with this id
        insert_test_project(&c, "p1");
        let handle = project_frame_handles(&c, "p1", &["u1"]).unwrap()["u1"];
        let fs = StackSubject::FrameSet(set);
        let pr = StackSubject::Project("p1".into());
        for (subject, f, path) in [(&fs, frame, "/a"), (&pr, handle, "/b")] {
            for _ in 0..2 {
                upsert_artifact(&c, &NewArtifact {
                    subject, frame_id: Some(f), group_key: "g", kind: "calibrated",
                    path: Some(path), config_hash: "h", size: None, modified_at: None, payload_json: None,
                }).unwrap();
            }
        }
        assert_eq!(list_artifacts(&c, &fs, None).unwrap().len(), 1, "an upsert refreshes in place");
        assert_eq!(list_artifacts(&c, &pr, None).unwrap().len(), 1);
        let a = find_artifact(&c, &pr, "g", "calibrated", Some(handle)).unwrap().unwrap();
        assert_eq!((a.subject.clone(), a.frame_id, a.path.as_deref()), (pr.clone(), Some(handle), Some("/b")));
        let b = find_artifact(&c, &fs, "g", "calibrated", Some(frame)).unwrap().unwrap();
        assert_eq!((b.subject.clone(), b.frame_id, b.path.as_deref()), (fs.clone(), Some(frame), Some("/a")));
        // The first minted handle is 1, the catalog frame is 501: a frame id of
        // one namespace never finds the other subject's row.
        assert_ne!(handle, frame);
        assert!(find_artifact(&c, &fs, "g", "calibrated", Some(handle)).unwrap().is_none());
        assert!(find_artifact(&c, &pr, "g", "calibrated", Some(frame)).unwrap().is_none());
        assert_eq!(delete_all_artifacts(&c, &pr).unwrap(), 1);
        assert_eq!(list_artifacts(&c, &fs, None).unwrap().len(), 1, "the frame set's row survives");
    }

    #[test]
    fn run_frame_rows_of_both_subjects_upsert_in_place() {
        // A frame-set run and a project run (insert_run with each subject),
        // one group each (insert_group), `upsert_frame_row` twice per run with
        // the same frame (catalog id / handle) → one row per run;
        // `list_frame_rows(run)` returns the namespace id in `frame_id`;
        // `set_frame_rejected_fraction(&subject, run, frame, 0.25)` changes
        // only that run's row.
    }

    #[test]
    fn a_project_run_row_leaves_the_frame_set_queries_alone() {
        // Review Focus 5 (db half): with one frame-set run 'running' and one
        // project run 'running' in the table:
        //  - list_unfinished_runs == [(fs_run, FrameSet(set)), (pr_run, Project("p1"))] (by id);
        //  - list_runs(&FrameSet(set), 10) holds only the frame-set run;
        //  - active_run_for_subject is per subject;
        //  - get_run(pr_run).subject() == Project("p1"), and its wire frames_set_id == 0.
    }

    #[test]
    fn master_lights_carry_their_subject() {
        // insert_master_light with a project subject → list_master_lights(run)[0].subject == Project("p1");
        // the frame-set round trip (`master_light_round_trip_and_cascade`) keeps passing.
    }

    #[test]
    fn an_artifact_written_before_the_rebuild_is_found_after_it() {
        // Review Focus 4 (lookup half): a fresh db, `crate::db::schema::downgrade_stacking_tables_to_legacy`,
        // a legacy-shape artifact row by raw SQL (frames_set_id = set, frame_id = frame,
        // kind 'calibrated', config_hash 'c1'), `init_db` again, then
        // find_artifact(&FrameSet(set), group, "calibrated", Some(frame)) returns the
        // same id and config_hash 'c1'.
    }
```
Write the four commented tests' bodies for real (the comments are the exact assertions). Update the one allowed assertion in `list_unfinished_runs_reports_only_planning_and_running` (~1102): the tuple's second element is now `StackSubject::FrameSet(set)`.

- [ ] **Step 2: Run** → FAIL to compile (no `StackSubject`).

- [ ] **Step 3: The type** (`db/stacking.rs`, after `now()`)

```rust
/// What a stacking run stacks (wave 6, spec 2026-10-02 §4.3, ruling W8): a
/// catalog frame set, or a collaboration project. One pipeline, two
/// subjects; a run has exactly one. Lives here, in the ungated persistence
/// layer, because `db::stacking` and `services::StackHandle` take it in a
/// headless build (plan 6b-1 B7); re-exported as `stacking::StackSubject`.
///
/// A frame id is always read in the subject's namespace: a catalog
/// `frames.id` for a frame set, a `stack_project_frames.id` handle for a
/// project. The two never meet in one map or one query.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackSubject {
    FrameSet(i64),
    Project(String),
}

impl StackSubject {
    pub fn frame_set_id(&self) -> Option<i64> {
        match self {
            StackSubject::FrameSet(id) => Some(*id),
            StackSubject::Project(_) => None,
        }
    }

    pub fn project_id(&self) -> Option<&str> {
        match self {
            StackSubject::FrameSet(_) => None,
            StackSubject::Project(id) => Some(id),
        }
    }

    /// The `(frame_id, project_frame)` column pair for a frame id of this
    /// subject's namespace.
    fn frame_columns(&self, frame: Option<i64>) -> (Option<i64>, Option<i64>) {
        match self {
            StackSubject::FrameSet(_) => (frame, None),
            StackSubject::Project(_) => (None, frame),
        }
    }

    /// The run-frame column that holds this subject's frame id.
    fn frame_column(&self) -> &'static str {
        match self {
            StackSubject::FrameSet(_) => "frame_id",
            StackSubject::Project(_) => "project_frame",
        }
    }

    fn from_columns(frames_set_id: Option<i64>, project_id: Option<String>) -> rusqlite::Result<Self> {
        match (frames_set_id, project_id) {
            (Some(id), None) => Ok(StackSubject::FrameSet(id)),
            (None, Some(id)) => Ok(StackSubject::Project(id)),
            other => Err(rusqlite::Error::InvalidColumnType(
                1,
                format!("stacking subject pair {other:?}"),
                rusqlite::types::Type::Null,
            )),
        }
    }
}

impl std::fmt::Display for StackSubject {
    /// Byte-identical to the pre-6b error texts (`frame set {id}`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackSubject::FrameSet(id) => write!(f, "frame set {id}"),
            StackSubject::Project(id) => write!(f, "project {id}"),
        }
    }
}
```
Add `pub use crate::db::stacking::StackSubject;` to `stacking/mod.rs`.

- [ ] **Step 4: Rows and readers** — every reader reads the subject pair; a frame column is read in the subject's namespace:
  - `RUN_COLUMNS = "id, frames_set_id, project_id, status, …"` (all following indices shift by one in `row_to_run`); `row_to_run` sets `frames_set_id: r.get::<_, Option<i64>>(1)?.unwrap_or(0)` (the wire value, doc comment: "0 for a project run until 6b-2 makes the wire subject-aware; code reads `subject()`") and `project_id: r.get(2)?`. Add to `StackingRunRow`:
    ```rust
    /// The project of a project run (wave 6b). Not on the wire in 6b-1 (B8).
    #[serde(skip)]
    #[ts(skip)]
    pub project_id: Option<String>,
    ```
    and `pub fn subject(&self) -> StackSubject` (`project_id` set → `Project`, else `FrameSet(self.frames_set_id)`).
  - `FRAME_COLUMNS`: `frame_id` → `COALESCE(frame_id, project_frame)`; `StackingRunFrameRow.frame_id` doc: "the frame id in the run's namespace (a catalog id or a project handle)".
  - `StackingArtifactRow`: `frames_set_id: i64` → `subject: StackSubject`; `frame_id` doc as above; `ARTIFACT_COLUMNS = "id, frames_set_id, project_id, COALESCE(frame_id, project_frame), group_key, …"`; `row_to_artifact` builds `subject` with `StackSubject::from_columns(r.get(1)?, r.get(2)?)?`.
  - `MasterLight`: `frames_set_id` → `subject`, same pattern; `MASTER_LIGHT_COLUMNS` gains `project_id` after `frames_set_id`.

- [ ] **Step 5: Writers and queries** — keep each statement's text and add the pair, using `IS` so one statement serves both subjects:
  - `insert_run`: column list gains `project_id`; params `run.subject.frame_set_id(), run.subject.project_id()`.
  - `list_runs`: `WHERE frames_set_id IS ?1 AND project_id IS ?2 ORDER BY started_at DESC, id DESC LIMIT ?3`.
  - `active_run_for_subject` (renamed; update its 2 callers): `WHERE frames_set_id IS ?1 AND project_id IS ?2 AND status IN ('planning','running')`.
  - `list_unfinished_runs`: `SELECT id, frames_set_id, project_id …`, mapped with `StackSubject::from_columns`; doc: "`(id, subject)`".
  - `upsert_frame_row(conn, subject, f)`: the column that receives `f.frame_id` and the conflict target both come from `subject.frame_column()`:
    ```rust
    let col = subject.frame_column();
    let sql = format!(
        "INSERT INTO stacking_run_frames
         (run_id, group_id, {col}, included, exclusion_reason, weight, weight_channels_json,
          metrics_json, reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio,
          reg_flipped, rejected_fraction)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(run_id, {col}) DO UPDATE SET
           …the same SET list as today…
         RETURNING id"
    );
    ```
    For a frame set this is today's statement byte for byte. (The CHECK makes the other column NULL by construction.)
  - `set_frame_rejected_fraction(conn, subject, run_id, frame_id, fraction)`: `WHERE run_id = ?2 AND {col} = ?3`.
  - `upsert_artifact`: column list gains `project_id, project_frame`; `let (fid, pf) = a.subject.frame_columns(a.frame_id);` params `a.subject.frame_set_id(), a.subject.project_id(), fid, pf, …`; target as Task 2.
  - `find_artifact` — written in the unique key's OWN six expressions, so it stays a covering point lookup on `stacking_artifacts_subject_key` (Review Focus 6; an `IS`-based WHERE would fall back to `idx_stacking_artifacts_set` plus a scan of every artifact of the set):
    ```rust
    let (fid, pf) = subject.frame_columns(frame_id);
    let sql = format!(
        "SELECT {ARTIFACT_COLUMNS} FROM stacking_artifacts
         WHERE COALESCE(frames_set_id, 0) = ?1 AND COALESCE(project_id, '') = ?2
           AND group_key = ?3 AND kind = ?4
           AND COALESCE(frame_id, 0) = ?5 AND COALESCE(project_frame, 0) = ?6"
    );
    conn.query_row(
        &sql,
        params![
            subject.frame_set_id().unwrap_or(0),
            subject.project_id().unwrap_or(""),
            group_key,
            kind,
            fid.unwrap_or(0),
            pf.unwrap_or(0),
        ],
        row_to_artifact,
    )
    .optional()
    ```
    The `NULL ≡ 0` / `NULL ≡ ''` equivalence is the key's own (and `upsert_artifact` already relies on it); no frame id 0, set id 0 or project id `''` exists. Add a test:
    ```rust
    #[test]
    fn find_artifact_is_a_point_lookup_on_the_artifacts_key() {
        let c = conn();
        let sql = find_artifact_sql(); // factor the format! above into a `fn find_artifact_sql() -> String` used by find_artifact
        let plan: Vec<String> = c
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap()
            .query_map(params![7_i64, "", "g", "calibrated", 5_i64, 0_i64], |r| r.get::<_, String>(3)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap();
        assert!(plan.iter().any(|d| d.contains("stacking_artifacts_subject_key")), "{plan:?}");
    }
    ```
  - `list_artifacts`, `delete_artifacts`, `delete_all_artifacts`: `frames_set_id = ?1` → `frames_set_id IS ?1 AND project_id IS ?2` (renumber the following placeholders; `delete_artifacts` builds its `IN` placeholders from `?3`).
  - `insert_master_light`: column list gains `project_id`; `DO UPDATE SET frames_set_id = excluded.frames_set_id, project_id = excluded.project_id, …`.
  - Doc comments that say "for a frame set" become "for a subject".

- [ ] **Step 6: The rest of C1**
  - `services/mod.rs`: `StackHandle { pub cancel_flag, pub subject: crate::db::stacking::StackSubject }` (update the doc comment: "`subject` lets `cancel_stacking` and the per-subject admission check answer without a DB read").
  - `paths::cleanup_work(conn, subject: &StackSubject, layout, what)` — its db calls take `subject`. Its log line (~867) stays byte-identical for a frame set: `match subject` with two `tracing` calls, the FrameSet arm exactly today's (`frame_set_id = id`, same message), the Project arm the same message with `project_id = %id`.
  - `plan::fits_artifact_fresh(conn, subject, …)` and `fits_artifact_fresh_for_planes(conn, subject, …)`.
  - `RunContext` gains `pub(crate) subject: StackSubject` BESIDE `set_id`; its two literals (`start_stacking` ~911, `test_context` ~9837) set `subject: StackSubject::FrameSet(<the same id>)`. Every run.rs db call site of Step 5's functions and of `StackHandle` moves to `&rc.subject` / `rc.subject.clone()` (inventory: 24 artifact sites, 3 master-light sites, the `StackHandle` construction ~819 and the admission scan ~800 comparing `h.subject == subject`, `upsert_frame_row` ~5932, `set_frame_rejected_fraction` ~7778).
  - plan.rs and api/stacking.rs call sites build `StackSubject::FrameSet(set_id)` locally (or reuse one built at the top of the fn).
  - `examples/ln_probe.rs` (`list_runs` ~316, ~592; `find_artifact` ~342, ~356) and `crates/athenaeum-web/src/routes/stacking.rs` test literals (`NewRun` ~691, `NewMasterLight` ~713, ~866).
  - Test call sites: the ~110 mechanical edits of the inventory (a set id argument → `&StackSubject::FrameSet(id)`; a literal field `frames_set_id: x` → `subject: &StackSubject::FrameSet(x)`; a test emitter's `frames_set_id: i64` field and its `h.frames_set_id == …` read → `subject`). Test-only helpers (`test_context`, `seed_run_and_groups`, `seed_run_frame_rows`, `seed_registered_artifact`, `seed_fits_artifact`, `seed_artifact`, `seed_master_light`) keep their `i64` parameters and wrap inside. NO expectation changes besides Step 1's one tuple.

- [ ] **Step 7: Run the new tests, then the whole Gate per task** (the ts_contract regeneration must leave `src/types` unchanged — `#[ts(skip)]` keeps `StackingRunRow`'s TS identical). Mechanical completeness check: `grep -rn 'frames_set_id:' crates/athenaeum-core/src/stacking crates/athenaeum-core/src/api/stacking.rs crates/athenaeum-web/src/routes/stacking.rs crates/athenaeum-core/examples` must show only `RegistrationRecord` / `to_record` hits (and the `StackingRunRow` wire field); paste the output into the report.

- [ ] **Step 8: Commit** — `refactor(stacking): StackSubject keys the stacking persistence (behaviour-preserving)` (+ trailers).

---

### Task 4: The run, the plan and the entry points keyed by the subject (spec §4.3; inventory C2 + C4)

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs`, `stacking/plan.rs`, `stacking/paths.rs`, `api/stacking.rs`
- Test: `stacking/run.rs`, `stacking/plan.rs`, `stacking/paths.rs` `mod tests`

**Interfaces:**
- Consumes: Task 3's API.
- Produces:
```rust
// stacking/plan.rs
pub const PROJECT_STACKING_UNAVAILABLE: &str = "Stacking a collaboration project is not available yet.";
pub fn build_plan(conn: &Connection, settings: …, policy: …, subject: &StackSubject, config_override: …) -> Result<StackingPlan, ApiError>;
pub(crate) fn subject_label(conn: &Connection, subject: &StackSubject) -> Result<String, ApiError>;          // frame set name | project title
pub(crate) fn stored_reference_frame(conn: &Connection, subject: &StackSubject) -> anyhow::Result<Option<i64>>; // namespace id
// stacking/paths.rs
pub(crate) fn subject_layout(conn: &Connection, working_dir: &Path, subject: &StackSubject) -> anyhow::Result<WorkingLayout>;
// stacking/run.rs
pub fn start_stacking(ctx, emitter, policy, app_version, subject: StackSubject, config, rerun_from) -> Result<…, ApiError>;
pub(crate) fn wire_set_id(subject: &StackSubject) -> i64;   // the events' / RunSummary's / StackingPlan's `set_id`
// stacking/plan.rs, beside stored_reference_frame (plan.rs reads it; run.rs already imports from plan):
pub(crate) fn stored_registrations(conn: &Connection, subject: &StackSubject) -> anyhow::Result<Vec<RegistrationRecord>>;
pub(crate) fn store_registration(conn: &Connection, subject: &StackSubject, build: impl FnOnce(i64) -> RegistrationRecord) -> anyhow::Result<()>;
// RunContext loses `set_id`; keeps `subject` and `set_name` (the subject's display label)
```
(Use the exact parameter types `build_plan` / `start_stacking` already have for `settings`, `policy`, `config_override`, `config`, `rerun_from`; only the subject parameter changes.)

- [ ] **Step 1: Write the failing tests**

`plan.rs` tests:
```rust
    #[test]
    fn a_project_plan_is_refused_until_6b2() {
        let f = test_fixtures::frame_set();
        crate::db::stacking::insert_test_project(&f.conn, "p1"); // or the helper Task 2 chose
        let err = build_plan(&f.conn, /* the settings/policy args the existing build_plan tests pass */,
            &StackSubject::Project("p1".into()), None).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(ref m) if m == PROJECT_STACKING_UNAVAILABLE), "{err:?}");
    }

    #[test]
    fn subject_label_and_stored_reference_by_subject() {
        // FrameSet: subject_label == the frames_set name; stored_reference_frame == the
        // frame_set_reference row's reference_frame_id (set one with the existing
        // registration::db setter the plan tests already use).
        // Project: a collab_projects row titled "Orion Mosaic", a stacking_project_config row
        // with reference_frame_uuid 'u2' and handles minted for u1,u2 →
        // subject_label == "Orion Mosaic", stored_reference_frame == Some(handle of u2);
        // no config row → None.
    }
```
`paths.rs` tests:
```rust
    #[test]
    fn subject_layout_puts_a_project_under_projects_slug() {
        // FrameSet(set named "M42 2026") → WorkingLayout::new(working, &set_slug("M42 2026")) (equal roots);
        // Project("p1") with slug "orion-mosaic" → root == working/"projects"/"orion-mosaic".
    }
```
`run.rs` tests:
```rust
    #[test]
    fn a_project_start_is_refused_until_6b2() {
        // start_stacking(…, StackSubject::Project("p1".into()), …) → Err(Invalid(PROJECT_STACKING_UNAVAILABLE)),
        // no stacking_runs row written, no active_stacks entry.
    }

    #[test]
    fn the_heal_marks_an_interrupted_project_run_failed_and_leaves_frame_sets_alone() {
        // Review Focus 5 (heal half): a project run row in 'running' with no live handle (inserted
        // through insert_run with a Project subject) and a frame-set run 'running' WITH a live
        // StackHandle in ctx.active_stacks → heal_interrupted_runs marks only the project run
        // failed (status + error as for any interrupted run), the frame-set run untouched, and the
        // project run's rej dir sweep resolves its layout through subject_layout (no error).
        // While the project run row exists, `api::stacking::get_stacking_work_usage(set)` and
        // `api::stacking::cleanup_stacking_work(set, All)` for the frame set succeed and touch
        // only the frame set's folder and rows (the project's rows and folder are untouched).
    }
```
Write the bodies for real, imitating the neighbouring tests' fixtures (`test_fixtures::frame_set`, `test_ctx`, the existing heal tests — grep `heal_interrupted_runs` in run.rs tests).

- [ ] **Step 2: Run** → FAIL (no `PROJECT_STACKING_UNAVAILABLE`, `subject_label`, `subject_layout`, subject parameters).

- [ ] **Step 3: Subject-level helpers**
  - `subject_label`: FrameSet → today's `frame_set_name(conn, id)` (unchanged behaviour and error); Project → `SELECT title FROM collab_projects WHERE project_id = ?1`, missing row → `ApiError::NotFound(format!("project {id} not found"))` (mirrors `frame_set_name`'s `"frame set {id} not found"`, plan.rs:~822-829).
  - `subject_layout`: FrameSet → `WorkingLayout::new(working_dir, &set_slug(&frame_set_name(conn, id)?))` (exactly the 5 sites' current expression); Project → `SELECT slug FROM collab_projects WHERE project_id = ?1` → `WorkingLayout::new(&working_dir.join("projects"), &set_slug(&slug))` (spec §9.3).
  - `stored_reference_frame`: FrameSet → `registration::db::get_frame_set_reference(conn, id)?.map(|r| r.reference_frame_id)`; Project →
    ```sql
    SELECT h.id FROM stacking_project_config c
    JOIN stack_project_frames h ON h.project_id = c.project_id AND h.frame_uuid = c.reference_frame_uuid
    WHERE c.project_id = ?1
    ```
  - `wire_set_id(subject)`: `subject.frame_set_id().unwrap_or(0)` with the doc comment "the `setId` the events, `RunSummary` and `StackingPlan` carry until 6b-2 makes them subject-aware (B8); a project run never reaches an emitter in 6b-1".
  - Registration store in `plan.rs` (both `pub(crate)`, next to `stored_reference_frame`): `stored_registrations` (FrameSet → `get_registration_for_frame_set(conn, id)`; Project → `Ok(Vec::new())`) and `store_registration` (FrameSet → `upsert_registration(conn, &build(id))`; Project → `Ok(())`), both documented with B10 ("a project run keeps its registration in the registered artifact's payload, spec §4.2; it never writes `registration_results`").

- [ ] **Step 4: Move every subject read**
  - `RunContext.set_id` is removed. The 35 subject sites already use `rc.subject` (Task 3) or now use `wire_set_id(&rc.subject)` for event plumbing (inventory: 458, 2731, 2797, 7362, 8063, 9381, 9506 and `run_thread` 1002 → complete event 1167 + log 1131 — the log keeps field `set_id` with the same integer).
  - The 5 catalog-by-set reads: `stage_reference` Manual (~3667) → `stored_reference_frame(&conn, &rc.subject)`; registration reads (~5341 `register_native_groups`, ~5598 `stage_register`; plan.rs ~1261 `compute_register_stale`, ~1910 `ln_registration_by_frame`) → `stored_registrations`; the writes (~4908, ~4957, with the `to_record` calls at ~4761 and ~4946) → `store_registration(conn, &subject, |set_id| to_record(set_id, …))`. The registration fan-out (`register_group_pass`, ~4409) has no `rc`: today it copies `set_id` out at ~4512-4513; replace that copy with `let subject = rc.subject.clone();` beside `run_id`, and use `subject` inside the fan-out's builder closure (~4756-4766).
  - The 2 log-only reads in `stage_calibrate` (~1976, ~1997): Calibrate only runs for a frame set; take the id with `let Some(set_id) = rc.subject.frame_set_id() else { return Err(ApiError::Invalid(PROJECT_STACKING_UNAVAILABLE.into())) };` (or the stage's own error type with the same message) and keep the log lines byte-identical.
  - `build_plan(…, subject: &StackSubject, …)`: first statement
    ```rust
    let StackSubject::FrameSet(set_id) = *subject else {
        return Err(ApiError::Invalid(PROJECT_STACKING_UNAVAILABLE.into()));
    };
    ```
    (`StackSubject` is not `Copy`; bind with `let StackSubject::FrameSet(set_id) = subject else { … }` and deref `*set_id` where an `i64` is needed — no `expect` in production code). Inside, the subject-level calls take `subject` (`subject_label`, `stored_reference_frame` replacing `resolve_reference`'s direct `get_frame_set_reference` read at ~913 — keep the `frames ⋈ files` filename/path read beside it in the FrameSet path, it is catalog by nature; `list_runs`, `active_run_for_subject`, `fits_artifact_fresh*`, `find_artifact`, `list_frame_rows` unchanged). Frame-set-only steps (`resolve_config_and_exclusions`, the readiness gate, `collect_masters_to_build`, `group_frames`) keep `set_id`. `StackingPlan.set_id` = `set_id` (same value as today).
  - `start_stacking(…, subject: StackSubject, …)`: same guard first; then as today with `subject` where the run is keyed (`NewRun`, `StackHandle`, the admission scan, the Conflict message `format!("… {subject} …")` — it prints `frame set {id}` as today), `subject_layout` for the layout (~879-880), `subject_label` for `set_name`, `RunSummary.set_id = wire_set_id(&subject)`.
  - `heal_interrupted_runs`: iterate `list_unfinished_runs` `(run_id, subject)`; the "is a live handle covering it" check compares `h.subject == subject`; `remove_rej_dir_for_interrupted_run(conn, run_id, &subject)` resolves the layout with `subject_layout`. A project run's heal works for real (Review Focus 5).
    **Log lines stay byte-identical for a frame set.** The heal's warnings at ~566 and ~580 (`warn!(run_id, frames_set_id, …)`) and the rej-dir failure warning at ~579-584 become a `match subject` with two `tracing` calls: the FrameSet arm is today's line exactly (`frames_set_id = id`, same message); the Project arm logs the same message with `project_id = %id`. Never `subject = %subject`. The Conflict texts at ~756 and ~802 (`"… for frame set {frames_set_id}"`) become `format!("… for {subject}")`, which prints the same bytes for a frame set.
  - `api/stacking.rs`: every fn keeps its host-facing `set_id: i64` signature and builds `StackSubject::FrameSet(set_id)`; `get_stacking_work_usage` / `cleanup_stacking_work` / the preview's run-id lookup (~969-1007) use `subject_layout` (the preview gets the subject from `run_row.subject()`), `cleanup_stacking_work` uses `active_run_for_subject` and keeps its message text.
  - The test call sites: `build_plan(…, &StackSubject::FrameSet(f.set_id), …)` (59) and `run::start_stacking(…, StackSubject::FrameSet(set_id), …)` (42). `test_context` keeps its `set_id: i64` parameter and builds `subject: StackSubject::FrameSet(set_id)`.

- [ ] **Step 5: Run the new tests, then the whole Gate per task.** In particular every heal test, every cleanup/work-usage test, the preview tests (`api/stacking.rs` and web), and ts_contract with no `src/types` diff. Mechanical completeness check: `grep -c 'rc\.set_id\|self\.set_id' crates/athenaeum-core/src/stacking/run.rs` was 42 before this task and must be 0 now; paste both numbers into the report.

- [ ] **Step 6: Commit** — `refactor(stacking): the run, the plan and the entry points are keyed by StackSubject` (+ trailers).

---

### Task 5: Per-frame catalog reads behind `FrameOrigin` (spec §4.3 checklist; inventory C3)

**Files:**
- Create: `crates/athenaeum-core/src/stacking/origin.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (`pub mod origin;`), `stacking/groups.rs` (`frame_source`), `stacking/plan.rs` (`HashMemo`), `stacking/run.rs` (`RunContext.origins`, the solve and CFA reads)
- Test: `stacking/origin.rs`, `stacking/plan.rs`, `stacking/run.rs`

**Interfaces:**
- Consumes: Task 4's `RunContext.subject`.
- Produces:
```rust
// stacking/origin.rs
#[derive(Debug, Clone, PartialEq)]
pub enum FrameOrigin { Catalog { file_id: i64 } }   // 6b-2 adds Project { … }
pub type FrameOrigins = std::sync::Arc<std::collections::HashMap<i64, FrameOrigin>>;
pub(crate) fn origin_of(origins: &FrameOrigins, frame_id: i64) -> Result<&FrameOrigin, ApiError>;
pub(crate) fn stored_solve(conn: &Connection, origin: &FrameOrigin, frame_id: i64) -> anyhow::Result<Option<PlateSolveRecord>>;
pub(crate) fn cfa_geometry(conn: &Connection, origin: &FrameOrigin, frame_id: i64) -> <the exact return type of resolve_cfa_geometry>;
// stacking/groups.rs
pub(crate) fn frame_source(conn: &Connection, subject: &StackSubject, grouping: &GroupingConfig) -> Result<(Vec<IntegrationGroup>, FrameOrigins), ApiError>;
// stacking/plan.rs
impl HashMemo { pub(crate) fn new(origins: FrameOrigins) -> Self; /* calibration_hash / calibration_hash_checked unchanged signatures */ }
// RunContext gains `pub(crate) origins: FrameOrigins`
```
(Use `group_frames`' actual return/error types for `frame_source`; it wraps `group_frames` for a frame set and refuses a project with `PROJECT_STACKING_UNAVAILABLE`.)

- [ ] **Step 1: Write the failing tests**

`origin.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_without_an_origin_is_an_error_not_a_catalog_read() {
        let origins: FrameOrigins = std::sync::Arc::new(std::collections::HashMap::new());
        assert!(origin_of(&origins, 5).is_err());
    }

    #[test]
    fn the_catalog_arms_read_what_they_read_today() {
        // A fixture frame with a plate_solves row (use the plan/run tests' existing
        // solve seeding) and bayer columns: stored_solve(conn, Catalog{file_id}, frame)
        // == get_plate_solve(conn, frame); cfa_geometry(conn, Catalog{..}, frame)
        // == resolve_cfa_geometry(conn, frame).
    }
}
```
`plan.rs`:
```rust
    #[test]
    fn the_hash_memo_refuses_a_frame_it_has_no_origin_for() {
        // HashMemo::new(empty origins).calibration_hash(conn, cfg, &frame, …) → Err naming the frame id;
        // with the frame's Catalog origin → the same hash calibration_hash_for returns.
    }

    #[test]
    fn frame_source_returns_one_catalog_origin_per_frame() {
        // frame_source(conn, &FrameSet(set), &cfg.grouping): groups equal group_frames(conn, set, …);
        // origins has exactly the groups' frame ids, each Catalog{file_id: GroupFrame.file_id};
        // frame_source(conn, &Project(..)) → Err(Invalid(PROJECT_STACKING_UNAVAILABLE)).
    }
```
Write the bodies for real.

- [ ] **Step 2: Run** → FAIL (no `origin` module).

- [ ] **Step 3: `origin.rs`**

```rust
//! Where a frame of a stacking run comes from (wave 6, spec 2026-10-02
//! §4.3). Every read of the catalog BY FRAME ID on the run path after the
//! frame source goes through here, so a frame whose id is not a catalog
//! `frames.id` can never reach a catalog query. 6b-1 has one variant; 6b-2
//! adds `Project { frame_uuid, publisher, landed_path, blake3, size_mtime,
//! manifest }`, and every `match` below then fails to compile until it has a
//! project arm — the compiler is the checklist (plan 6b-1 B9).
//!
//! Not to be confused with `project_frames_local.origin` (`own | replica`),
//! which says who published a project frame, not where a run reads it from.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::api::ApiError;
use crate::plate_solve::PlateSolveRecord; // use the path run.rs imports it by

#[derive(Debug, Clone, PartialEq)]
pub enum FrameOrigin {
    /// A catalog frame: `frames.id` is the run's frame id; `file_id` is
    /// `files.id` (the stage-1 hash's `SourceIdentity`).
    Catalog { file_id: i64 },
}

/// A run's frame id → origin map, shared by the plan, `HashMemo` and the run.
pub type FrameOrigins = Arc<HashMap<i64, FrameOrigin>>;

pub(crate) fn origin_of(origins: &FrameOrigins, frame_id: i64) -> Result<&FrameOrigin, ApiError> {
    origins.get(&frame_id).ok_or_else(|| {
        tracing::error!(frame_id, "stacking frame has no origin");
        ApiError::Internal(format!("stacking frame {frame_id} has no origin"))
    })
}

/// The stored plate solve of a frame, if any. Callers keep their own error
/// policy: Register warns and carries on, Output fails the run.
pub(crate) fn stored_solve(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
) -> anyhow::Result<Option<PlateSolveRecord>> {
    match origin {
        FrameOrigin::Catalog { .. } => crate::plate_solve::storage::get_plate_solve(conn, frame_id),
    }
}
```
plus `cfa_geometry` with the same shape over `crate::calibration_library::light_resolve::resolve_cfa_geometry(conn, frame_id)` (copy its exact return type). Match the error/return types of the functions you wrap exactly — adapt the `anyhow::Result` above if `get_plate_solve` returns another type. Use `ApiError`'s internal-error variant (whatever it is named; `Internal` above is a guess — grep `api/mod.rs:134`).

- [ ] **Step 4: Wire it**
  - `groups::frame_source`: FrameSet → `group_frames(conn, set_id, grouping)` and an origins map built from every `GroupFrame` (`frame_id → Catalog { file_id }`); Project → `Err(ApiError::Invalid(PROJECT_STACKING_UNAVAILABLE.into()))`. `build_plan` (~1591) and `start_stacking` (~785) call `frame_source` instead of `group_frames`; the plan keeps the origins for its `HashMemo`, the run stores them in `RunContext.origins`.
  - `HashMemo::new(origins)` (constructions: plan ~1896, run ~932, `test_context` ~9858 — `test_context` builds the map from its `plan_groups` the same way `frame_source` does). Inside `calibration_hash` / `calibration_hash_checked`, before calling `calibration_hash_for`: `match origin_of(&self.origins, frame.frame_id)? { FrameOrigin::Catalog { .. } => calibration_hash_for(…) }`. Their signatures, and all 8 run-side call sites, do not change. If `calibration_hash` currently returns a type that cannot carry `ApiError`, convert with the same mapping its existing error path uses.
  - The two solve reads keep their caches and their policies. `solve_of` (~3973-3994) runs inside the registration fan-out, which has no `rc`: give it an `origins: &FrameOrigins` parameter; its three callers (`registration_gate_and_hint` ~4036 ← `register_group_pass` ~4409, `register_native_groups` ~5418, `stage_register` ~5613) pass it, and the fan-out clones the `Arc` out of `rc` before it starts (beside `subject`, Task 4). Inside, `solve_of` calls `stored_solve(conn, origin, frame_id)` after `origin_of(origins, frame_id)` and keeps its `warn!` + `None` on a read error; `stage_output` (~9684) calls the same and keeps `?` (fail the run). If `solve_of` cannot return a `Result`, an `origin_of` error there is logged at `error!` and treated as no solve, exactly like its read error today — never a panic.
  - Bayer drizzle (~7907): `cfa_geometry(&conn, origin_of(&rc.origins, frame_id)?, frame_id)` (same error handling as the current call).

- [ ] **Step 5: Run the new tests, then the whole Gate per task.**

- [ ] **Step 6: Commit** — `refactor(stacking): per-frame catalog reads go through FrameOrigin` (+ trailers).

---

### Task 6: Docs and the final gates

**Files:**
- Modify: `docs/stacking/README.md`
- Modify: `docs/superpowers/open-items.md` (only if a known flake fired during this plan)

- [ ] **Step 1: `docs/stacking/README.md`** — a short section "Run subjects (wave 6b-1)" in the reference's own style:
  - `StackSubject::{FrameSet, Project}` keys runs, run-frames, artifacts, masters and the heal sweep; a frame id is read in the subject's namespace (catalog `frames.id` | `stack_project_frames.id` handle); the two never meet in one map or query;
  - the four rebuilt tables (subject pair, frame pair, CHECKs, `UNIQUE(run_id, frame_id)` + `UNIQUE(run_id, project_frame)`, the six-expression `stacking_artifacts_subject_key`), the rebuild's guard, its in-transaction `foreign_key_check`, and that a fresh database is created in the new shape from the same DDL constants;
  - per-frame catalog reads go through `stacking::origin` (`FrameOrigin`), `HashMemo` dispatches the stage-1 hash by origin and refuses a frame with no origin;
  - the config pin: `calibration_subtree` writes grouping as `{"exposureToleranceSec": x}` only, pinned by `stage_hashes_are_pinned_to_golden_literals`; a new grouping or output field must leave those literals unchanged (only `config_hash_is_canonical_and_pinned` moves);
  - project stacking itself is plan 6b-2; every project entry point answers `PROJECT_STACKING_UNAVAILABLE` until then.
  Spaced table separators (`| ---- |`), no third-party names.

- [ ] **Step 2: Final gates**, one at a time:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
CARGO_BUILD_JOBS=4 cargo check --workspace
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4 2>&1 | grep -E '^test result|FAILED'
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract && git diff --exit-code src/types
npx tsc --noEmit -p .
npx vitest run src/components/stacking src/hooks src/contexts
```
Expected: all green; the `test result` totals equal Task 0's plus exactly the tests this plan added; `git diff src/types` empty. Report the numbers next to Task 0's.

- [ ] **Step 3: Commit** — `docs(stacking): run subjects, the rebuilt tables, the config pin` (+ trailers).
