# 6b step 1 — the subject seam: inventory (branch `collab-wave6b`, HEAD 17e2e96e)

Read-only research for spec `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §4.3 / §18 step 1.
All paths are relative to `crates/athenaeum-core/src/` unless prefixed with a crate.

**Where production ends and tests begin (line boundaries used for every prod/test count below):**
`stacking/run.rs` prod 1–9875 (`#[cfg(test)] fn test_context` at 9794–9873 sits outside the mod; `mod tests` at 9877–19702) ·
`stacking/plan.rs` prod 1–2360 (tests 2362–5985) · `stacking/paths.rs` 1–875 · `stacking/groups.rs` 1–596 ·
`db/stacking.rs` 1–896 · `api/stacking.rs` 1–1148 · `athenaeum-web/src/routes/stacking.rs` 1–532.

## 0. The two facts that shape the design

1. **`db::stacking` is UNGATED; `stacking::*` and `api::stacking` are gated `all(render, solver)`.**
   `db/mod.rs:18` declares `pub mod stacking;` with no cfg; its own module doc (`db/stacking.rs:8-11`) says
   "this one is not [gated]: the tables and this CRUD layer must stay reachable from a headless build".
   `services::StackHandle` (`services/mod.rs:68-71`) is ungated too (only the `active_stacks` field, `services/mod.rs:92-93`, is gated).
   `lib.rs:55-56` gates `pub mod stacking`. So **`StackSubject` cannot live in `crate::stacking`** if `db::stacking::*`
   and `StackHandle` take it — `cargo check --no-default-features` would break. Define it in `db/stacking.rs`
   (or another ungated module) and `pub use` it from `stacking/mod.rs` so the spec's `stacking::StackSubject` path still works.
   `FrameOrigin` is only ever used by gated code → it can live in a new gated `stacking/origin.rs`.
2. **No host or frontend change is needed for a behaviour-preserving step 1** *if* the wire types keep their shape:
   every host command takes a frame-set id and stays a frame-set command (spec §9.1: "Frame-set commands do not change"),
   so `api::stacking::*` keeps its `set_id: i64` signatures and builds `StackSubject::FrameSet(set_id)` internally.
   Changing `StackingProgressEvent/StackingCompleteEvent.set_id` to optional (spec §9.2) regenerates
   `src/types/stacking.ts` (pinned by `crates/athenaeum-core/tests/ts_contract.rs`) and breaks
   `src/hooks/useStackingRuns.ts:76-145` (a `Map<number,…>` keyed by `payload.setId`, plus the
   `/objects/${payload.setId}?tab=stacking` link) — that is 6b-12/6c work, not 6b-1 (see §6).

## 1. Every signature that carries a frame-set id as the RUN SUBJECT

### 1a. `db/stacking.rs` (ungated persistence)

| Item | Line | Today | Prod callers | Test callers |
| ---- | ---- | ---- | ---- | ---- |
| `StackingRunRow.frames_set_id` | 26 | `i64`, ts-exported (`framesSetId`, `src/types/stacking.ts:384`) | read once: `api/stacking.rs:969` (preview) | none in tests; no frontend reader of `run.framesSetId` |
| `StackingArtifactRow.frames_set_id` | 188 | `i64`, not ts-exported | none | none |
| `MasterLight.frames_set_id` | 228 | `i64`, not ts-exported | none | `run.rs:16840`, `db/stacking.rs:1226` (asserts) |
| `NewArtifact.frames_set_id` | 271 | `i64` | 8 literals in run.rs: 1878, 1893 (Calibrate), 3182, 3252 (Measure), 5797 (Register), 8928, 9235, 9574 (LN) | 20 literals (db 3, paths 1 via `seed_artifact`, plan 15 incl. `seed_registered_artifact`/`seed_fits_artifact`, run 1) |
| `NewMasterLight.frames_set_id` | 283 | `i64` | run.rs 7760, 8188, 8205 | 10 (api 4, db 4, web 2: `routes/stacking.rs:713, 866`) |
| `NewRun.frames_set_id` | 297 | `i64` | run.rs:806 (`start_stacking`) | 19 (api 9, db 3, plan 4 incl. `seed_run_frame_rows`, run 2 incl. `seed_run_and_groups`, web 1: `routes/stacking.rs:691`) |
| `insert_run(conn, &NewRun)` | 373 | — | run.rs:806 | as `NewRun` |
| `list_runs(conn, frames_set_id: i64, limit)` | 439 | — | 6: `api/stacking.rs:242`; plan.rs 981, 1119, 1200, 1946, 1979 | 3 (api 1399, db 1, run 1) + `examples/ln_probe.rs:316, 592` |
| `active_run_for_set(conn, frames_set_id: i64)` | 458 | — | 2: `api/stacking.rs:844`, `plan.rs:2326` | 2 (db) |
| `list_unfinished_runs(conn) -> Vec<(i64, i64)>` | 476 | `(run_id, frames_set_id)` | 1: `run.rs:549` | 1: `db/stacking.rs:1071-1110` **destructures `(id, frames_set_id)` and asserts `== set`** — the one test whose assertion text must change |
| `upsert_artifact(conn, &NewArtifact)` | 654 | ON CONFLICT target `(frames_set_id, group_key, kind, COALESCE(frame_id, 0))` (must track the 6b-2 index) | as `NewArtifact` | as `NewArtifact` |
| `find_artifact(conn, frames_set_id, group_key, kind, frame_id)` | 686 | — | 20: plan.rs 759, 1312, 2002, 2022, 2052, 2054, 2077, 2153; run.rs 1598, 1606, 2909, 3041, 4124, 5825, 6801, 7897, 8592, 8782, 8820, 8869 | 3 (db 1, run 2) + `examples/ln_probe.rs:342, 356` |
| `list_artifacts(conn, frames_set_id, kind)` | 706 | — | **0 (test-only API)** | 36 (db 3, paths 2, run 31) |
| `delete_artifacts(conn, frames_set_id, kinds)` | 740 | — | 2: `paths.rs:852, 855` | 4 (db 1, paths 2, plan 1) |
| `delete_all_artifacts(conn, frames_set_id)` | 765 | — | 1: `paths.rs:862` | 1 (db) |
| `insert_master_light(conn, &NewMasterLight)` | 782 | ON CONFLICT `(run_id, group_key, kind)` updates `frames_set_id` | as `NewMasterLight` | as `NewMasterLight` |

**Run-keyed, unchanged in 6b-1 but frame-column-dependent in 6b-2** (spec §4.2 moves `stacking_run_frames.frame_id` to
`frame_id | project_frame`; `frame_id` is `REFERENCES frames(id)`, `db/schema.rs:1211-1229`):
`NewFrameRow.frame_id` 346 / `upsert_frame_row` 571 (ON CONFLICT `(run_id, frame_id)`; prod callers run.rs:5932 ←
`write_frame_rows` 5871 and `exclude_frame_and_persist` 5980), `set_frame_rejected_fraction(conn, run_id, frame_id, f)` 623
(prod run.rs:7778), `list_frame_rows` 636 (plan.rs 1265, 1947; api `get_stacking_run` ~270).
`set_run_reference` 401 (`reference_frame_id` has no FK — a handle fits as is). Decide in 6b-1 whether these take the
subject now (cheap: `rc.subject` is at hand) or in 6b-2.

**Frame-set-only by spec (§4.2: `stacking_set_config` unchanged) — leave `i64`:** `get_set_config` 850, `set_set_config` 878,
`SetConfigRow` 361. Prod callers: plan.rs:862; api 343, 364, 798, 851.

### 1b. `stacking/paths.rs`

| Item | Line | Today | Callers |
| ---- | ---- | ---- | ---- |
| `WorkingLayout::new(working_dir, set_slug: &str)` | 31 | **slug-keyed, not id-keyed**; doc at 22-24 says "one per frame set" | prod 5 sites, each `WorkingLayout::new(w, &set_slug(&frame_set_name(conn, id)?))` or `&set_slug(&plan.set_name)`: run.rs:634 (heal rej sweep), run.rs:880 (start), api 805 (work usage), api 857 (cleanup), api 1000-1007 (preview cache). Tests: 63 (run 53, paths 7, api 3) |
| fixed accessors | 40-67 (`calibrated_root`, `registered_root`, `ln_root`, `fits_root`, `runs_root`), rej 136-160, previews 176-199 | spec §9.3 adds `prepared` | — |
| `cleanup_work(conn, frames_set_id: i64, layout, what)` | 808 | artifact rows by set | prod 3: api 858, run 9759, 9766; tests 8 (paths 7, run 1); log field `frame_set_id` at 867 |

Recommendation: keep `WorkingLayout::new` (63 test callers) and add one subject resolver in `paths.rs`
(e.g. `subject_layout(conn, working, &StackSubject) -> Result<WorkingLayout>`: FrameSet → today's
`working/<set_slug(name)>`; Project → `working/projects/<collab_projects.slug>` from 6b-3) used at the 5 prod sites.

### 1c. `stacking/plan.rs`

| Item | Line | Today | Callers |
| ---- | ---- | ---- | ---- |
| `StackingPlan.set_id: i64`, `.set_name` | 231-232 | ts-exported (`ts_export.rs:422` → `src/types/stacking.ts:373`) | built once, 2338-2339 |
| `build_plan(conn, settings, policy, frames_set_id: i64, config_override)` | 1579 | — | prod 2: `api/stacking.rs:162`, `run.rs:743`; tests 59 (plan 50, run 9) |
| `fits_artifact_fresh(conn, frames_set_id, group_key, frame_id, payload, hash)` | 714 | — | prod: plan 2121, run 3057 |
| `fits_artifact_fresh_for_planes(conn, frames_set_id, …)` | 746 | — | prod: plan 728, run 4086 |
| `frame_set_name(conn, frames_set_id)` | 821 | reads `frames_set.name` → subject label | prod 5: plan 1586, run 633, api 804, 856, 969 |
| `resolve_config_and_exclusions(conn, settings, frames_set_id, cfg)` | 856 | reads `stacking_set_config` (FrameSet-only store) | 1588 |
| `resolve_reference(conn, frames_set_id, cfg, blockers)` | 885 | checklist (§3) | 1712 |
| `last_run_reference_scale(conn, frames_set_id, groups)` | 976 | `list_runs` | 1735 |
| `cleanup_stale_note(conn, frames_set_id, …)` | 1108 | `list_runs` | 2304 |
| `compute_register_stale(conn, cfg, frames_set_id, groups, reference, memo)` | 1192 | `list_runs`, registration rows, `find_artifact` | 2286 |
| `HashMemo` | 360-438 | memo keyed by frame id; funnels every stage-1 hash through `calibration_hash_for` | `HashMemo::new()` at plan 1896, run 932, run 9858 |

### 1d. `stacking/run.rs`

| Item | Line | Today | Notes |
| ---- | ---- | ---- | ---- |
| `StackingProgressEvent.set_id: i64` | 142 | wire `setId` (`ts_export.rs:434` → `stacking.ts:532`) | 6 constructions: 456 (`RunContext::progress`), 2795 (`FanOutTicker::tick`), 6526 (`emit_integrate_tick`), 6650 (`emit_drizzle_tick`), 9397 (`measure_ln_seeds_calibration`), 9516 (`build_and_write_ln_reference`) |
| `StackingCompleteEvent.set_id: i64` | 173 | wire `setId` (`ts_export.rs:436` → `stacking.ts:536`) | 1 construction: 1165 (`run_thread`) |
| `RunContext.set_id: i64`, `.set_name` | 243-244 | — | 2 literals: `start_stacking` 911, `test_context` 9837; **42 prod reads** of `rc.set_id`/`self.set_id` (categorised in §2) |
| `heal_interrupted_runs(ctx, conn)` | 545 | consumes `(run_id, frames_set_id)` 549-600 | prod callers 5: run 741; api 160, 240, 266, 841 |
| `remove_rej_dir_for_interrupted_run(conn, run_id, frames_set_id)` | 607 | layout via `frame_set_name` 633-634 | 1 caller (577) |
| `start_stacking(ctx, emitter, policy, app_version, frames_set_id: i64, config, rerun_from)` | 725 | build_plan 743; Conflict msg 756; frame source `group_frames` 785; per-subject admission scan 798-803; `NewRun` 806; `StackHandle` 819; layout 879-880; `RunSummary.set_id` 885; `RunContext.set_id` 915 | prod caller `api/stacking.rs:218`; **42 test calls** in run.rs |
| `FanOutTicker { set_id }` + `from_parts(…, set_id: i64, …)` | 2700, 2742 | event plumbing | `new` 5 prod; `from_parts` 1 test (14025) |
| `emit_integrate_tick(…, set_id: i64, …)` | 6471/6475 | event plumbing | 4 prod (via `set_id` copy at 7362), 2 test |
| `emit_drizzle_tick(…, set_id: i64, …)` | 6625/6629 | event plumbing | 1 prod (via `drizzle_tick_set_id` 8063) |
| ComputeQueue label | 1228 | `"Stacking · {rc.set_name}"` | spec wants `"Stacking · <project title>"` — `set_name` already is the subject's display name |
| `master_file_name(&rc.set_name, …)` | 7649-7656 | subject display name | — |
| `test_context(ctx, emitter, run_id, set_id: i64, set_name, config, plan_groups, layout, output_dir, group_ids)` | 9796 | cfg(test) | 39 test callers — keep the `i64` param and wrap, so they stay untouched |

### 1e. `stacking/provenance.rs`

`RunSummary.set_id: i64` 223, `.set_name` 224 — `Serialize + Deserialize + ts` (`ts_export.rs:433` → `stacking.ts:523`).
Written by `start_stacking` 883-886 / `test_context` 9809; persisted as `stacking_runs.summary_json` and `runs/run-<id>.json`;
parsed back by `api::get_stacking_run` (~272) and `remove_rej_dir_for_interrupted_run` (615-618). Any later reshaping
must keep old rows parseable (`#[serde(default)]`). Frontend reads only `summary.setName` (`ProvenanceModal.tsx:67`).
**Not subject:** `MasterBuilt.set_id` 45 (a calibration set).

### 1f. `services/mod.rs` — "ActiveStacks"

There is no `ActiveStacks` type: it is `ServiceContext::active_stacks: Arc<Mutex<HashMap<i64 /*run_id*/, StackHandle>>>`
(93, gated). The map key stays the run id; only `StackHandle.frames_set_id: i64` (70) becomes the subject.
Constructions: prod run.rs:819; tests run.rs:10267, api 1508, 1739. Reads: prod run.rs:800 (the per-subject
admission check under the lock); tests run.rs:14531, 18141, 18371 inside the test emitters `CancelOnFirstMeasure` (14516),
`CancelOnDrizzle` (18126), `DeleteRejOnFirstIntegrateTick` (18352), each holding a `frames_set_id: i64` field.
The ~20 `ServiceContext { active_stacks: Arc::new(Mutex::new(HashMap::new())) }` literals across core/tauri/web are unaffected.

### 1g. `stacking/groups.rs` — the catalog frame source

`group_frames(conn, frames_set_id, cfg)` 478 → `load_group_members` 252 (`session_members → sessions → imaging_nights →
frames → files`, 253-264) + `load_solved_scales` 300 (`plate_solves` by frame id). Prod callers: plan 1591, run 785.
Tests: 59 (groups 10, plan 9, run 40) + `ln_probe.rs:296`. It IS the Catalog frame source — keep its `i64` signature and
wrap it in a subject dispatcher (`frame_source(conn, &subject, grouping) -> (Vec<IntegrationGroup>, origins)`).
`set_slug(name)` 219 is a pure helper.

### 1h. `api/stacking.rs` (host-facing; recommended to keep `set_id: i64` signatures)

| Fn | Line | Subject use inside |
| ---- | ---- | ---- |
| `get_stacking_plan(ctx, policy, set_id, config)` | 151 | heal 160, `build_plan` 162 |
| `start_stacking(ctx, emitter, policy, app_version, set_id, config, rerun_from)` | 198 | log 217, `run::start_stacking` 218 |
| `get_stacking_runs(ctx, set_id, limit)` | 231 | heal 240, `list_runs` 242 |
| `get_stacking_config` / `set_stacking_config` | 335 / 356 | FrameSet-only store — unchanged |
| `get_stacking_work_usage(ctx, set_id)` | 795 | FrameSet config 798, `frame_set_name`+slug layout 804-805 |
| `cleanup_stacking_work(ctx, set_id, what)` | 831 | heal 841, `active_run_for_set` 844 (+ message 846), config 851, layout 856-857, `cleanup_work` 858 |
| `resolve_master_light(ctx, run_id, …)` (preview run-id lookup) | 941 | `run_row.frames_set_id` → `frame_set_name` 969 → layout 1000-1007 |
| `get_stacking_run(ctx, run_id)` (provenance run-id lookup) | 261 | needs no subject (reads `summary_json`); only heal |

### 1i. Hosts (no change needed in 6b-1)

`athenaeum-tauri/src/commands/stacking.rs`: `set_id: i64` params on `get_stacking_plan` 34/36, `start_stacking` 60/63,
`get_stacking_runs` 95/97, `get_stacking_config` 117/119, `set_stacking_config` 127/129, `get_stacking_work_usage` 222/224,
`cleanup_stacking_work` 237/239 (7 commands; run-id commands `cancel_stacking`, `get_stacking_run`, `get_master_light_preview` unaffected).
`athenaeum-web/src/routes/stacking.rs`: `set_id: i64` in `GetStackingPlanArgs` 36, `StartStackingArgs` 44,
`GetStackingRunsArgs` 60, `GetStackingConfigArgs` 74, `SetStackingConfigArgs` 80, `GetStackingWorkUsageArgs` 120,
`CleanupStackingWorkArgs` 126; handlers 140, 176, 219, 241, 252, 372, 393. Web tests seed `NewRun` (691), `NewMasterLight`
(713, 866) and read `SELECT frames_set_id FROM stacking_runs` (854) — the last one matters in 6b-2, not 6b-1.

## 2. `set_id` uses that are NOT the subject (leave alone in 6b-1)

The 42 prod `rc.set_id` reads in run.rs split as:
- **35 subject (stacking tables / events)**: artifacts 24 (1600, 1608, 1879, 1894, 2911, 3043, 3058, 3183, 3253, 4087, 4126, 5798,
  5827, 6803, 7899, 8592, 8784, 8822, 8869, 8929, 9236, 9575, 9759, 9766), master_lights 3 (7761, 8189, 8206),
  event plumbing 7 (458, 2731, 2797, 7362, 8063, 9381, 9506), `run_thread` 1002 (→ complete event 1167 + log 1131).
- **5 catalog-by-set reads (the checklist, §3)**: 3667, 4513 (→ `to_record` 4762), 4947, 5341, 5598.
- **2 log-only**: 1976, 1997 (`stage_calibrate`, field `set_id`).

Other non-subject uses:
- **Calibration-set ids (a different id space entirely):** `PlanMaster.set_id` (plan ~121, ts `stacking.ts:315`),
  `MasterBuilt.set_id` (provenance 45), `stage_masters` `item.set_id`/`source_set_id`/`master_set_id` (run 1361-1434, ~11 refs),
  `calibration_set_imagetyp_and_count(conn, set_id)` plan 1372, `calibration_set_label(conn, set_id, …)` plan 1399,
  `test_fixtures::add_raw_linked_*` returns, `csf.set_id` SQL in plan tests 3455, 3519.
- **Log fields (9 sites, three different spellings):** run 566 & 580 (`frames_set_id`), 1131 (`set_id`), 1976, 1997 (`set_id`);
  plan 2329 (`set_id`); api 217 (`set_id`); paths 867 (`frame_set_id`); groups 588 (`frame_set_id`).
  Keep byte-identical for FrameSet; a project run needs a `project_id` field — a logging-dictionary update the spec already
  defers to the docs plan.
- **Error strings (keep byte-identical via `impl Display for StackSubject` = `"frame set {id}"`):** run 756, 802; api 846;
  plan 828. No test or UI matches on them.
- **Catalog-only stages/gates that only ever run for a frame set** (stay inside a `FrameSet` arm, no origin twin in 6b-1):
  Masters (`stage_masters` 1318 → `api::masters::{resolve_rebuild_target 1371, build_master_inline 1377, master_file_path 1409}`),
  Calibrate (`resolve_calibrate_job` 1570 → `resolve_generation_cached` 1653; its `HashMemo` lookup 1580),
  the plan's readiness gate (`compute_export_readiness_for_frames` plan 1610 → `api/lights.rs:272` → `export::collect_export_data`;
  `collect_masters_to_build` plan 1611), the per-set config store (plan 862; api 343/364/798/851), the frame source (§1g).

## 3. Catalog reads by frame id (spec §4.3 checklist) — confirmed, plus extras

Only `groups.rs`, `paths.rs`, `plan.rs`, `run.rs` (and `test_fixtures.rs`) touch `rusqlite` inside `stacking/`;
`measure`, `register/*`, `ln/*`, `drizzle/*`, `integrate`, `master_cards`, `fits_artifact`, `rej` are DB-free
(`register/writer.rs::source_cards_from_file` reads the calibrated FILE's header, not the catalog).

### On the checklist — exact locations

| # | Read | Exact location | Callers / stage |
| ---- | ---- | ---- | ---- |
| 1 | `solve_of` → `get_plate_solve(conn, frame_id)` (`plate_solve/storage.rs:172`) | run.rs:3973-3994 (`SolveCache` alias 3968); a read error is a `warn!` + `None` | `registration_gate_and_hint` 4036-4037 (← `register_group_pass` 4409-4411), `register_native_groups` 5418, `stage_register` 5613 — Register |
| 2 | `stage_output` → `get_plate_solve(&conn, reference_frame_id)` | run.rs:9684, with its OWN `HashMap<i64, Option<PlateSolveRecord>>` at 9623; **a read error here fails the run (`?`)**, unlike #1 | Output (master WCS); warning 9704-9712 |
| 3 | Bayer drizzle `resolve_cfa_geometry(conn, frame_id)` (`calibration_library/light_resolve.rs:320`, reads `frames.bayerpat/xbayroff/ybayroff`) | run.rs:7907-7909 inside `process_group_output`, block 7889-7928 (also `find_artifact(…"calibrated_mosaic"…)` 7897) | Drizzle |
| 4 | `calibration_hash_for` → `resolve_generation_cached(conn, frame.frame_id, …)` (`export/calibrated_generator.rs:305` → `resolve_frame_inputs`) | plan.rs:314-347 (call 320-326; `SourceIdentity{file_id: frame.file_id}` 336-340) | via `HashMemo::calibration_hash` 385 (plan 1255, 1281, 2062) and `HashMemo::calibration_hash_checked` 410 — **8 run.rs call sites, 7 of them after Calibrate**: 1580 (Calibrate), 3033, 3172 (Measure), 3622 `frame_calibration_hash` (← 3549, 3571; Reference/group geometry), 4295 (Register), 5169 (`two_pass_refine`), 5574 (`stage_register`), 8543 (Normalize). Direct test calls: 9 in plan.rs tests (4066, 4337, 4751, 4994, 5374, 5441, 5587, 5688, 5805) |
| 5 | `resolve_reference` → `get_frame_set_reference` (`registration/db.rs:268`) + `SELECT fi.filename, fi.path FROM frames f JOIN files fi …` | plan.rs:913 and 916-923 | build_plan gate 2 (1712) |
| 6 | `registration_results` | reads `get_registration_for_frame_set` (`registration/db.rs:147`): plan 1261 (`compute_register_stale`), plan 1910 (`ln_registration_by_frame`), run 5341 (`register_native_groups`), run 5598 (`stage_register`); writes `upsert_registration` (`registration/db.rs:80`): run 4908, 4957; record built by `to_record(frames_set_id, …)` (`stacking/register/frame.rs:437`) at run 4761 (`set_id` copied at 4513 for the fan-out closure) and 4946 | Register; plan staleness. `RegistrationRecord.frames_set_id` is consumed only by the upsert (and a frame.rs test at 597) |

### NOT on the checklist (flag these to the planner)

- **run.rs:3667 — `stage_reference` (Manual) reads `get_frame_set_reference(&conn, rc.set_id)`.** Same table as #5 but on the
  run side; the spec lists only plan.rs `resolve_reference`. Needs the same subject-level twin
  (project: `stacking_project_config.reference_frame_uuid` → handle).
- **#4's seam belongs inside `HashMemo`, not at its 8+3 callers.** `HashMemo::new()` is built in 3 places (plan 1896, run 932,
  test_context 9858); giving the memo the run's origin map (Catalog arm = today's `calibration_hash_for`, unchanged) keeps all
  11 call sites and the 9 direct `calibration_hash_for` test calls untouched.
- **#1 and #2 are two separate solve caches with different error policies** (warn vs fail). One origin-aware
  `stored_solve(conn, origin, frame_id)` must keep both policies at the call sites.
- **Stacking-row frame column:** `upsert_frame_row` / `set_frame_rejected_fraction` / `list_frame_rows` store catalog frame ids
  in `stacking_run_frames.frame_id REFERENCES frames(id)`; not a catalog read, but a project handle written there violates the
  FK under `foreign_keys = ON` — must be subject-routed by 6b-2.
- Frame source internals `load_group_members` (groups 252) and `load_solved_scales` (groups 300, `plate_solves`) — catalog reads by
  frame id at plan/start time; covered by the frame-source split, not by an origin twin.
- `frame_set_name` (plan 821, `frames_set.name`) — subject label (5 callers); `resolve_config_and_exclusions` → `get_set_config`
  — subject config store. Both need a subject dispatch, neither is per-frame.
- Checked and clean (no action): `integration::io_policy::resolve(&conn, settings, …)` (run 7071) reads settings only;
  `paths::validate_dirs` (run 775, plan 1805) reads `scan_roots` overlap only; `MasterCardInputs` (run 7614-7630) takes
  `reference_cards` from the calibrated file and `reference_id` from `GroupFrame.filename`.
- Naming hazard: `project_frames_local.origin` is already a column (`'own'|'replica'`, `db/schema.rs:2418`); a Rust
  `FrameOrigin` next to it invites confusion in 6b-3's SQL.

## 4. `GroupFrame` and `SourceIdentity` today

- `GroupFrame` — `stacking/groups.rs:57-72` (struct at 59), `Serialize + Deserialize + ts_rs::TS` but **not registered in `ts_export.rs`**
  (only `ColorMode`, `ScaleSource` are). Fields: `frame_id, file_id, filename, path, size, modified_at, exposure_s, date_obs,
  width, height, pixel_scale_arcsec, scale_source`.
- Built in exactly ONE prod place: `to_group_frame` (groups.rs:350-371) from a `LightMember` row (227-249) loaded by
  `load_group_members` (252-298: `f.id, fi.id, fi.filename, fi.path, fi.size, fi.modified_at, …` WHERE
  `ino.frames_set_id = ?1 AND f.imagetyp = 'Light' ORDER BY f.date_obs, f.id`). Scale: `plate_solves.pixel_scale_arcsec`
  (Solve) else `header_pixel_scale_arcsec(xpixsz, focallen)` (Header). `width/height` = `naxis1/2` or 0.
  Test literals: groups.rs 2, paths.rs 2 (`fn frame` 1259) — unaffected.
- `SourceIdentity { file_id, size, modified_at }` — `stacking/config.rs:620-624`; `stage_hash` (626-648) sorts sources by
  `(file_id, size, modified_at)`. **The only prod construction is plan.rs:336-340** (stage-1 hash). Tests: config.rs ×6.
- **`GroupFrame.file_id` is consumed in exactly one place: plan.rs:337.** Everything else keys on `frame_id`
  (`HashMemo.by_frame`, `SolveCache`, `cached_calibrated`, `runtime_exclusions`, `dry_pass_stars`, artifact `frame_id`,
  `calibrated_file_stem`'s `_f<id>` collision suffix run 1535-1553). So `file_id = 0` for a project frame is safe as the spec says,
  provided the Project arm of the stage-1 hash never builds a `SourceIdentity`.

## 5. Test inventory for the seam

`#[test]`/`#[tokio::test]` counts:

| File | Tests | Touches the seam |
| ---- | ---- | ---- |
| `stacking/run.rs` | 81 | yes |
| `stacking/plan.rs` | 41 | yes |
| `stacking/paths.rs` | 21 | yes (cleanup/usage) |
| `stacking/groups.rs` | 12 | frame source only |
| `stacking/test_fixtures.rs` | 1 | fixture |
| other `stacking/**` (pure math: ln, drizzle, weights, register, config, rej, psf_signal, master_cards, measure, integrate, …) | 352 | no |
| **`stacking/` total** | **508** | |
| `db/stacking.rs` | 6 | yes |
| `api/stacking.rs` | 26 | yes |
| `athenaeum-web/src/routes/stacking.rs` | 8 | seeding literals only |
| `athenaeum-tauri/src/commands/stacking.rs` | 0 | — |
| `registration/db.rs` | 10 | no (catalog API unchanged) |
| `tests/ts_contract.rs` | 1 | pins every wire shape — the behaviour-preservation tripwire |
| `examples/ln_probe.rs` (compiles under `--all-targets`) | — | `list_runs` 316, 592; `find_artifact` 342, 356 (+ unchanged `group_frames` 296, `get_registration_for_frame_set` 331) |

Test helpers that carry a set id into the seam:
- run.rs: `test_context` (9796, 39 callers), `seed_run_and_groups(conn, frames_set_id, plan_groups, working, output)` (10847,
  38 callers; builds `NewRun`), `run_stages_for_test` (10895), `two_pass_context` (11939), `run_mixed_scale_register` (12481),
  `run_two_geometry_groups` (12810, returns a `RunContext`), the `run_pass` closure (13329), fixture builders `seed_ready` (9987),
  `seed_star_group` (10793), `seed_star_group_with_fields` (11866), `seed_mixed_scale_group` (12408),
  `seed_two_geometry_groups` (12697), `seed_ln_star_group` (14793), `seed_trail_group` (16259), `seed_osc_star_group` (17069),
  `seed_sky_penalty_group` (18781); test emitters with `frames_set_id` fields (14516, 18126, 18352).
- plan.rs: `seed_run_frame_rows(conn, frames_set_id, …)` (4141, `NewRun`), `seed_registered_artifact` (4212, `NewArtifact`),
  `seed_fits_artifact` (4261, `NewArtifact`); 50 direct `build_plan(…, f.set_id, …)` calls.
- paths.rs: `seed_artifact(c, frames_set_id, …)` (1571).
- db/stacking.rs: `conn` 902, `seed_set` 912, `seed_frame` 926.
- api/stacking.rs: `test_ctx` 1181, `seed_ready_fixture` 1197.
- web: `db_test_state` 626, `seed_master_light` 675.
- `test_fixtures.rs`: `Fixture { set_id }` 29-32, `frame_set` 42, `frame_set_with_conn` 56 — a natural home for a
  `fn subject(&self) -> StackSubject` helper.

Mechanical edits if production signatures take the subject and test-only helpers keep `i64` and wrap inside:

| Change | Test call sites |
| ---- | ---- |
| db layer (`NewRun`/`NewArtifact`/`NewMasterLight` literals, `StackHandle` literals, `find/list/delete(_all)_artifacts`, `list_runs`, `active_run_for_set`, `cleanup_work`, `list_unfinished_runs`) | 110 (plan 20, run 39, api 16, db 20, paths 12, web 3) + 3 `h.frames_set_id` reads + 4 in `ln_probe.rs` |
| `build_plan(…, &subject, …)` | 59 (plan 50, run 9) |
| `run::start_stacking(…, subject, …)` | 42 (run) — `api::start_stacking` keeps `i64`, its 3 api tests untouched |
| `emit_integrate_tick` / `FanOutTicker::from_parts` params | 3 (avoidable: keep them `i64` until the wire changes) |
| `test_context`, `seed_run_and_groups`, `seed_run_frame_rows`, `seed_*_artifact`, `seed_master_light` | 0 (keep `i64`, wrap in body) |
| `calibration_hash_for` (if the seam is put in `HashMemo`) | 0 |
| **assertion change** | 1: `db/stacking.rs:1102-1105` (`list_unfinished_runs` tuple) |

≈ 215 one-token edits; no other expectation changes. A literally zero-edit step 1 is only possible by leaving
`db::stacking` signatures `i64` until 6b-2 (they change then anyway when the struct literals gain the project column), so
it moves the churn rather than removing it. An `impl From<i64> for StackSubject` would also hide it but lets a frame id or
handle silently become a set id — the exact confusion §4.1 forbids. Not recommended.

## 6. Recommended decomposition (4 commits; each compiles, each passes the full suite)

Gate per commit: `cargo test -p athenaeum-core --all-targets` (examples included — `ln_probe`), `cargo check --no-default-features -p athenaeum-core`
(headless: proves the ungated placement), `cargo test -p athenaeum-web` (web tests seed the changed literals), `cargo check -p athenaeum-tauri`.
`ts_contract` must stay green with zero diff in `src/types/stacking.ts` in all four.

**C1 — `StackSubject` and subject-keyed persistence.**
`db/stacking.rs`: define `pub enum StackSubject { FrameSet(i64), Project(String) }` (Debug, Clone, PartialEq, Eq, Hash;
`frame_set_id()`, `project_id()`, `Display` = `"frame set {id}"`/`"project {id}"`); `NewRun/NewArtifact/NewMasterLight`
carry the subject; `list_runs`, `active_run_for_set` (→ per subject), `list_unfinished_runs -> Vec<(i64, StackSubject)>`,
`find/list/delete/delete_all_artifacts` take `&StackSubject`. FrameSet arm = today's SQL verbatim; Project arm returns an
`Err` ("needs the stacking-table rebuild"), never a panic — replaced by real SQL in 6b-2. Row structs keep `frames_set_id`
(+ `StackingRunRow::subject()`). `services/mod.rs`: `StackHandle.subject`. `stacking/mod.rs`: `pub use crate::db::stacking::StackSubject`.
`paths::cleanup_work(&StackSubject)`; `plan::fits_artifact_fresh(_for_planes)(&StackSubject)`.
`RunContext` gains `subject` BESIDE `set_id` (both set in the 2 literals) so the 28 run.rs db call sites move to `&rc.subject`
once; plan.rs/api call sites build `StackSubject::FrameSet(id)` locally. Tests: the 110 + 3 + 4 edits and the one assertion.
New unit tests: Project arm of each db fn errs cleanly; subject Display text.

**C2 — the run, the plan and the entry points keyed by subject.** (needs C1)
Remove `RunContext.set_id`; `build_plan(conn, settings, policy, &StackSubject, cfg)`; `run::start_stacking(…, StackSubject, …)`;
admission scan on `h.subject`; heal + `remove_rej_dir_for_interrupted_run(&subject)`; the `paths` subject→layout resolver used at the 5
`WorkingLayout::new` prod sites (run 634, 880; api 805, 857, 1000-1007); subject label (`frame_set_name` dispatch) and config store
(`resolve_config_and_exclusions` FrameSet arm); frame source dispatcher over `group_frames`; FrameSet-only gates (readiness, masters)
inside the FrameSet arm. Events/`RunSummary`/`StackingPlan` keep their wire shape: one mapping helper subject→`set_id: i64`
(FrameSet only reachable; the helper is the single place 6b-12 changes). `api::stacking` keeps every `i64` signature (resolve_master_light
takes the subject from `StackingRunRow::subject()`), so hosts and frontend are untouched. Subject-level catalog reads keep working
through `rc.subject.frame_set_id()` until C4. Tests: 59 `build_plan` + 42 `start_stacking` edits; `test_context` and seed helpers unchanged.

**C3 — per-frame catalog reads behind `FrameOrigin::Catalog`.** (needs C2)
New gated `stacking/origin.rs`: `FrameOrigin::{Catalog{file_id}, Project{frame_uuid, publisher, landed_path, blake3, size_mtime, manifest}}`
(Project fields `#[allow(dead_code)]` until 6b-3 constructs it). The frame source returns `(groups, HashMap<i64, FrameOrigin>)`;
`RunContext.origins` + plan-local origins. Dispatch: stage-1 hash inside `HashMemo` (checklist #4; Catalog arm = unchanged
`calibration_hash_for`), `stored_solve` replacing both solve caches' reads (#1 run 3979 with warn policy, #2 run 9684 with fail policy),
`cfa_geometry` (#3 run 7907). Project arms return a typed "not yet supported" error. Tests: none edited; new unit tests per dispatch.

**C4 — subject-level catalog reads.** (needs C2; independent of C3, can be squashed into it)
`stored_reference(conn, &subject)` for plan 913-923 AND run 3667; a registration store over `registration_results`
(reads plan 1261, 1910, run 5341, 5598; writes run 4908, 4957; `to_record`'s `frames_set_id` from the subject at 4761/4946).
Tests: none edited; new unit tests for the Project arms.

Compile order: C1 → C2 → {C3, C4}. If the planner wants 3 commits, fold C4 into C2 (it is subject-keyed by nature) and keep C3
as the origin commit.

Open decisions to put in the plan explicitly:
1. Do `upsert_frame_row` / `set_frame_rejected_fraction` / `list_frame_rows` take the subject in C1, or only in 6b-2 with the column split?
2. The wire change of spec §9.2 (`setId` optional + `projectId`) and `RunSummary`/`StackingPlan`/`StackingRunRow` reshaping:
   in 6b-2/6b-12 core, or with 6c? It regenerates `src/types/stacking.ts` and needs at least a guard in
   `useStackingRuns.ts:76-145`, which spec §18 assigns to 6c — some frontend edit lands in 6b or the TS build breaks between plans.
3. Log field for a project run (`project_id`) and the three existing spellings (`set_id`, `frames_set_id`, `frame_set_id`).
