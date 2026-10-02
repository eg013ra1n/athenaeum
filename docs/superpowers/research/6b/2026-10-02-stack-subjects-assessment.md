# Assessment — proposal (A) `stack_subjects` table and (B) frame-source trait

Read-only review of branch `collab-wave6b` (HEAD `c716f9ad` + the Task 3 working tree), the wave-6 spec, plan 6b-1 and the two 6b research notes. SQL claims below were run in `sqlite3 :memory:` (shape (A) prototype: cascades, upsert, EXPLAIN plans, and a legacy → (A) rebuild rehearsal with minting).

## Verdict in one paragraph

**(A): do not do it now, and probably never.** It is a correct design — the cascade chains reproduce today's behaviour, `find_artifact` stays a point lookup, the rebuild is only slightly more work — but it buys nothing the product has a use for. The two "third subjects" the caller named are already expressible as `StackSubject::FrameSet`: an ad-hoc selection of catalog frames IS a custom frame set (`create_frame_set_from_selection`, `crates/athenaeum-tauri/src/commands/frame_sets.rs:588-600`, `frames_set.is_custom`), and "several frame sets together" IS `merge_frame_sets` (`frame_sets.rs:139`). The stacking spec rules multi-set stacking out ("Union/mosaic geometry, comet mode, multi-set stacking: not planned", `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md:1811`). Meanwhile (A) costs a rewrite of the Task 2 DDL/rebuild/tests, every Task 3 SQL body, a second real-catalog rehearsal, and it breaks two things the plan's acceptance criterion protects (the `StackingRunRow.frames_set_id` wire column and the web test's raw `SELECT frames_set_id FROM stacking_runs`). The trigger that would make it worth doing is a subject whose parent is NOT a `frames_set` row and NOT a `collab_projects` row — nothing on any roadmap is that.

**(B): right direction, wrong timing — and it is not a trait-vs-enum question at all.** `FrameOrigin` is data (cloned into fan-out closures, serialisable); it should stay an enum. What a trait could own is the ~8 *behaviour* funnels the plan already names (`frame_source`, `subject_layout`, `subject_label`, `stored_reference`, `HashMemo` stage-1 hash, `stored_solve`, `cfa_geometry`, registration store). Those funnels are what matter; whether they are `match` arms inside eight functions or eight methods on one `impl` is a mechanical refactor that touches no table, no wire shape and no test expectation, so it can be made after 6b-2 shows how big the Project arms really are. Do not pre-commit 6b-2 to a trait object.

## Where the caller's answer is right, and where it is not

Right:
- The numeric core is subject-agnostic. Confirmed by the seam inventory §3: only `groups.rs`, `paths.rs`, `plan.rs`, `run.rs` touch `rusqlite` inside `stacking/`; `measure`, `register/*`, `ln/*`, `drizzle/*`, `integrate` are DB-free.
- It is a closed two-variant design. True, and that is a *feature* here (see "extensibility" below).
- Masters/Calibrate/readiness stay catalog-coupled inside `run.rs`. True — and (A) does nothing about that; it only moves two columns.

Not right:
- "A third subject would need another rebuild of the four tables." Only if the third subject has a *new parent table*. A subject that is a set of catalog frames is a frame set (custom or merged). The product already has the subject-minting primitive for that case; it is `frames_set`.
- "A new subject kind is then a column on one small table." Also a column on `stack_subjects`'s CHECK, a new config store (today `stacking_set_config` keyed by `frames_set_id` and `stacking_project_config` keyed by `project_id` — (A) does not unify those), a frame-handle namespace (the frame pair stays), a working layout, a label, a frame source, a preparation stage, commands on two hosts and a UI. The column pair is the cheapest 5 % of a new kind.
- "Tasks 4–5 are unaffected." Mostly, but not quite: `api/stacking.rs:977` reads `run_row.frames_set_id` for the preview path and the heal reads `(id, subject)` from `list_unfinished_runs` — both stay, but `RUN_COLUMNS` becomes a join and `row_to_run` changes, so Task 3 is re-done, not patched.

## 1. Correctness and data safety of (A)

Shape I prototyped (and would use if it were ever done):

```sql
CREATE TABLE stack_subjects (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  kind          TEXT NOT NULL,                       -- 'frameSet' | 'project'
  frames_set_id INTEGER UNIQUE REFERENCES frames_set(id)                ON DELETE CASCADE,
  project_id    TEXT    UNIQUE REFERENCES collab_projects(project_id)   ON DELETE CASCADE,
  CHECK ((frames_set_id IS NULL) <> (project_id IS NULL)),
  CHECK ((kind = 'frameSet') = (frames_set_id IS NOT NULL)));
-- each of stacking_runs / stacking_artifacts / master_lights:
--   subject_id INTEGER NOT NULL REFERENCES stack_subjects(id) ON DELETE CASCADE  (+ index)
-- artifacts key: UNIQUE (subject_id, group_key, kind, COALESCE(frame_id,0), COALESCE(project_frame,0))
```

- **Cascades reproduce today's behaviour.** Verified: `DELETE FROM frames_set WHERE id=10` removed that subject's run, its groups, run-frames, artifacts and master light (counts 3/3/5/5/2/3 → 2/2/3/3/1/2); `DELETE FROM collab_projects` removed the project subject, its run, groups, run-frames (via the run), artifacts, master and the `stack_project_frames` handles; `DELETE FROM frames` still cascades the frame pair; `foreign_key_check` empty. Two-level cascades are ordinary SQLite FK actions, no recursive-trigger setting needed. `stacking_run_groups` is untouched (it hangs off `stacking_runs.id`, which keeps its value), so its children cascade exactly as now.
- **The rebuild gets slightly harder, not easier.** The legacy → (A) rehearsal needs one extra `INSERT INTO stack_subjects … SELECT DISTINCT frames_set_id FROM (runs UNION artifacts UNION master_lights)` before the copies, and each copy becomes a `JOIN stack_subjects ON frames_set_id`. Verified: 2 runs / 2 groups / 3 artifacts / 1 master copied with their ids, three subjects minted (one for a set that has artifacts but no run). It stays one IMMEDIATE transaction under `foreign_keys = OFF` with the same `foreign_key_check` before COMMIT. Cost: a second rehearsal on the real `.dev` copy (the first found 368 artifacts, 6 masters, 4 runs — small, minutes not hours).
- **Frame sets without runs need no subject row.** Today `build_plan` is read-only for a frame set (no `conn.execute` in prod `plan.rs`; `set_set_config` appears only in its tests). With (A), a subject lookup that finds no row short-circuits every `find_artifact` to `None` — *no subject row ⇒ no artifacts* is a cheap invariant. The row is minted at the first write (`insert_run` in `start_stacking`, or the first `upsert_artifact`), by `INSERT OR IGNORE … ; SELECT id` — the UNIQUE per parent resolves any race between two processes even outside a transaction (verified: repeated mint returns the same id). Inside the run, `start_stacking` already runs under the orchestration gate, and `CLAUDE.md`'s rule puts the read-then-write under `BEGIN IMMEDIATE` as `project_frame_handles` already does (`db/stacking.rs:1017-1040`).
- **Race between minting and inserting a run.** None beyond today's: mint-then-insert in one IMMEDIATE transaction, or mint on its own (idempotent) and insert afterwards.
- **Orphan subject rows.** A subject whose runs and artifacts were all cleaned up keeps one row (ids, kind, parent). Harmless; it is removed by the parent's cascade. Nothing in the app lists `stack_subjects`, so nothing can be confused by it.

So (A) is *sound*. The question is only whether it is worth doing.

## 2. Query cost

- `find_artifact` — 8 prod sites in `build_plan`'s per-frame loop (`plan.rs`), 12 in the run (`run.rs`; today 14 hits incl. clones), per frame per kind. Under (A) the subject id is resolved **once** (in `build_plan` and in `start_stacking`, cached in `RunContext`/the plan as an `i64`), after which `find_artifact` is `SEARCH stacking_artifacts USING INDEX stacking_artifacts_subject_key (subject_id=? AND group_key=? AND kind=? AND <expr>=? AND <expr>=?)` — a five-column point lookup instead of today's six-expression one. Verified with EXPLAIN; neither form is "covering" (both read the row by rowid for the payload), which is the same as today.
- `list_runs` by subject: `SEARCH stack_subjects USING COVERING INDEX (frames_set_id=?)` then `SEARCH stacking_runs USING INDEX idx_runs_subject (subject_id=?)` — one extra unique-index probe per call. Negligible; but to fill `StackingRunRow.frames_set_id` on the wire the run read becomes a JOIN (`RUN_COLUMNS` and `row_to_run` change).
- `list_unfinished_runs` (heal, every API entry): `SCAN stacking_runs` + rowid probe of `stack_subjects` — today it is a `SCAN` on `status` too (no index on status), so unchanged.
- Net: (A) is not slower. It is also not faster in any way that matters: the COALESCE key the branch has now is already a point lookup (`find_artifact_is_a_point_lookup_on_the_artifacts_key`, `db/stacking.rs:1826-1843`).

## 3. Behaviour preservation and churn

(A) makes the plan's acceptance criterion harder in three concrete places:
1. **`StackingRunRow.frames_set_id` (wire, `src/types/stacking.ts:384` `framesSetId: number`).** B8 keeps it on the wire, filled from the column. Under (A) it is filled from a JOIN; TS stays byte-identical, but `RUN_COLUMNS`, `row_to_run`, `get_run`, `list_runs`, `list_unfinished_runs` all change. Doable, but it is Task 3 rewritten, not adjusted.
2. **`athenaeum-web/src/routes/stacking.rs:851-857`** does `SELECT frames_set_id FROM stacking_runs WHERE id = ?1` into an `i64`. That column no longer exists under (A). The test edit is a new SQL literal with a JOIN — more than the "one-token mechanical edit" the plan allows (it is a seed read, not an expectation, so it is defensible, but it is a visible deviation from the criterion).
3. **Rebuild tests.** `the_subject_rebuild_preserves_every_row_and_child`, `fresh_and_migrated_stacking_tables_have_the_same_shape`, `deleting_a_frame_set_or_a_frame_still_cascades_after_the_rebuild`, `subject_and_frame_pairs_are_enforced`, `LEGACY_STACKING_DDL`, `downgrade_stacking_tables_to_legacy`, `every_foreign_key_child_column_is_indexed`'s expectations, the `.dev`-copy rehearsal (`the_subject_rebuild_rehearses_on_a_real_catalog_copy`) — all redone. Plus the Task 3 bodies (`insert_run`, `list_runs`, `active_run_for_subject`, `list_unfinished_runs`, `upsert_artifact`, `find_artifact_sql`, `list/delete/delete_all_artifacts`, `insert_master_light`), their `IS ?1 AND … IS ?2` forms replaced by one `subject_id = ?`.

Nothing in Tasks 4–5 depends on the column pair (they see `&StackSubject` and `FrameOrigin`); the one exception is `api/stacking.rs:977` (`run_row.frames_set_id` → already slated to become `run_row.subject()`).

## 4. Real extensibility — is there a third subject?

I looked for one and did not find it:
- **Ad-hoc selection of catalog frames** = `create_frame_set_from_selection` (`commands/frame_sets.rs:588`), a `frames_set` row with `is_custom = 1` (`db/operations.rs:2632-2646`). Already `StackSubject::FrameSet`.
- **Several frame sets together** = `merge_frame_sets` (`commands/frame_sets.rs:139`; `auto_merge` module in core). Already a frame set. And the stacking spec rules multi-set stacking out (`2026-09-08-stacking-pipeline-design.md:1811`).
- **Stacking-prep feature** (memory `project_stacking_prep_feature.md`): registration *within* a frame set, keyed by `frames_set_id` — a stage, not a subject.
- Wave-6 spec §17 out-of-scope list: ZP, bin/scale study, portal grid editor, channel split, per-frame download, R10, hub batch remap, stale-export deletion, a separate export tab. None is a subject.
- Mosaic/union geometry: a different *output* geometry, not a different source of frames; the spec says "not a mosaic tool" (`:47`).

So the only kind of subject that would ever force a third variant is one whose frames do not come from the catalog and do not come from a collab project — i.e. a third *storage* of frames. The product has exactly two: the catalog (`frames`) and the project store (`project_frames_local`). The closed enum mirrors that fact; it is the honest shape. The column pair is not the thing that makes a third subject expensive — a new frame namespace, config store, frame source, preparation and UI are, and (A) touches none of those.

If a third storage ever appears, the migration pair → `stack_subjects` is the same 12-step rebuild the branch already has, with user databases holding project rows: the mint becomes `SELECT 'frameSet', frames_set_id, NULL … UNION SELECT 'project', NULL, project_id …` and the copies JOIN on either column — I ran the frame-set half of exactly that; the project half is symmetric. No data is lost (ids stay). Deferring costs one future rebuild of the same difficulty as the one just rehearsed.

## 5. Cost vs benefit

- Cost now: Task 2's DDL constants, rebuild function, index helper, `LEGACY_STACKING_DDL`, the downgrade helper and ~7 tests; Task 3's ~12 SQL bodies and ~20 tests (the working tree already carries 704 changed lines in `db/stacking.rs`); the web test; a second `.dev`-copy rehearsal; another fable review of the schema. Roughly the size of Tasks 2+3 again, with the Task 3 implementer mid-flight — a stop-the-line event.
- Benefit now: none observable. No query gets faster, no behaviour changes, no planned feature gets cheaper.
- Benefit later: a hypothetical third storage pays one 12-step rebuild either way — (A) now just moves that rebuild earlier, to a moment when no one knows the third kind's shape (`kind` TEXT with a CHECK is itself a guess).

## 6. (B) — trait vs enum for the 6b-2 frame source

Facts that bear on it:
- `fan_out` (`run.rs:2676-2710`) is `std::thread::scope` with `F: Fn(T) -> Result<R,String> + Sync`; the rayon pool is `rc.ctx.image_pool`. Closures borrow; nothing requires `'static`. A `&dyn Source` captured in the closure only needs `Source: Sync`. The run thread itself (`run.rs:976`, `spawn(move || run_thread(rc))`) needs `RunContext: Send + 'static`, so a trait object in `RunContext` would be `Arc<dyn SubjectSource + Send + Sync>` — no gymnastics, the same as `emitter: Arc<dyn ProgressEmitter>` already there (`run.rs:242`).
- `RunContext` is threaded `&mut` through stages; fan-out closures cannot borrow `rc` (the plan's I5 already clones `subject` out). An `Arc` field clones out the same way.
- Object safety: the funnels are plain `fn(&self, &Connection, frame_id) -> Result<…>` shapes — fine. `HashMemo` wants to dispatch per origin inside a memo keyed by frame id; it would take `&dyn SubjectSource` or hold the `Arc`.

Judgement:
- **Keep `FrameOrigin` an enum.** It is per-frame *data* (uuid, publisher, landed path, blake3, size:mtime, manifest), stored in a `HashMap<i64, FrameOrigin>`, cloned into closures and possibly persisted in artifact payloads. Data wants an enum; `match` on it is the checklist B9 describes ("every match fails to compile at exactly the sites that need a project twin").
- **The compiler-as-checklist property exists on both axes.** Adding a variant to an enum breaks every `match` (good when the set of kinds grows); adding a method to a trait breaks every `impl` (good when the set of operations grows). 6b-2 is a growth in *operations for one kind* (Prepare, solve, grid composition, LN on overlap, coverage) — exactly the axis where a trait is no checklist at all (one `impl` gains methods; nothing else is forced). The enum with funnels is the better checklist for 6b-2's actual work.
- **What a trait would buy** is cohesion: all Project behaviour in one `impl` file instead of eight funnels with Project arms. That is real, but it is a file-organisation refactor available at any time with zero schema or wire implications. The right moment is after 6b-2 lands, when the Project arms' sizes are known; if they stay ~10 lines each, eight `match`es in named funnels are clearer than a trait; if they grow into hundreds of lines, lift them into `impl SubjectSource for ProjectSource`.
- **What (B) must not do:** put subject behaviour that is really *stage* behaviour (Prepare vs Calibrate) behind a "frame source" trait. Prepare is the Calibrate slot of the Project path — that is a stage dispatch on the subject (spec §5.1), not a frame-source method.

Concrete recommendation for 6b-2's plan: name the funnels explicitly (the plan already does: `frame_source`, `subject_layout`, `subject_label`, `stored_reference`, `HashMemo` origin dispatch, `stored_solve`, `cfa_geometry`, registration store) and require that *no other* function in `run.rs`/`plan.rs` matches on `StackSubject`/`FrameOrigin` — pinned by a grep test like the plan's "mechanical completeness" greps. That rule is what keeps either a later trait extraction or a third variant cheap. Decide trait vs enum at the 6b-2 review, not before.

## Risks to pin if (A) were nevertheless done

1. `find_artifact` stays a point lookup on the new 5-column key (reuse `find_artifact_is_a_point_lookup_on_the_artifacts_key`), and `upsert_artifact`'s `ON CONFLICT` lists the same five expressions token for token (the existing C1 lesson).
2. The legacy → (A) rebuild on the `.dev` copy: every pre-existing artifact of every frame set is found with the same id/hash after the rebuild (a set with artifacts but no run must still get a subject row — my rehearsal covered that case: set 12).
