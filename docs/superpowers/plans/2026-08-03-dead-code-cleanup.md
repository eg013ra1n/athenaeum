# Dead & Duplicate Code Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove verified-dead code (~2,900 lines), fix the one real bug the audit surfaced (invisible archive-rollback progress), and consolidate verified copy-paste logic — wrapper-layer business logic moves into `athenaeum-core`, row-mappers get single canonical copies.

**Architecture:** Three waves — (A) the rollback-progress bug fix, (B/C) pure deletions frontend-then-Rust, (D/E/F) consolidations: 4 features migrate from duplicated tauri+web wrappers into core, 3 row-mapper families collapse to shared helpers (fixing the latent CFA-`None` drift on the way), 2 frontend component extractions. Every item was adversarially re-verified on 2026-08-03 (two verification passes); refuted items are listed in **Out of scope** and must NOT be "fixed" in this cycle.

**Tech Stack:** Rust (rusqlite, tracing, ts-rs), React/TS (Vite, Tailwind design tokens).

## Global Constraints

- Branch: `0.5.1` (current). One commit per task, in task order.
- **Commit author is the user** (`eg013ra1n` / `vilen.sharifov@gmail.com`) — never add Claude as author/co-author, no `Co-Authored-By` / `Claude-Session` trailers.
- Gates (run before every commit that touches the respective side): `cargo build --workspace`; `cargo test -p <touched crate>` per task and `cargo test --workspace` at the end of phases C and E; `npx tsc --noEmit` for any TS change. Clippy is NOT a gate. Format only touched Rust files via `rustfmt <files>` (never `cargo fmt -p`).
- Two-backend rule: any change to a tauri command has its mirror in `crates/athenaeum-web/src/routes/` in the same commit.
- Logging: never swallow errors; `tracing` only; message = short stable phrase, data in snake_case fields from the spec dictionary.
- UI: design tokens only (`bg-surface`, `text-content-muted`, …), no raw colors.
- **Out of scope — do NOT touch** (verified alive or deliberately deferred):
  - `rebuild_master` and its wrappers (owner decision: leave as-is).
  - `WarningType::{GainOffsetMismatch,BinningMismatch,ExposureMismatch}` and `SkipReason::{OutsideThreshold,AlreadyInSet}` — never constructed in Rust BUT the frontend has live `case` handlers (`WarningsPanel.tsx:250-252`, `FilterGroupCard.tsx:42`, `FrameSetHistoryTab.tsx:162-164`); these are unfinished producers, not dead code.
  - `sharing/iroh/mod.rs` (`IrohTransport`) — test/loopback engine awaiting the documented "Task 3" migration; its announce-block duplication is NOT consolidated here.
  - Unreachable-from-UI backend commands (`set_archive_root_path`, `delete_archive`, `get_calibration_library_root`, `list_collab_contributions`, `set_compute_max_concurrent`, `get/set_sync_auto_mode`) — Rust side stays; only dead frontend *wrappers* are deleted (Task 4).
  - Generated type files (`src/types/models.ts`, `export.ts`, `archive.ts`, `calibration-config.ts`) — never hand-edit.
  - Documented vestigial items (`projects` / `export_templates` tables, `cache` module, `MoveStrategy::Delete`, Windows symlink branch).
  - `archive/path_layout.rs::add_suffix` vs `calibration_library/paths.rs::resolve_collision` (different user-visible formats), test-fixture dedup, frontend clusters C4–C9 of the audit — deferred.

---

## Phase A — the real bug

### Task 1: Archive rollback progress reaches the UI

**Files:**
- Modify: `crates/athenaeum-core/src/archive/rollback.rs`
- Modify: `crates/athenaeum-core/src/archive/restore.rs` (delete dead second emission at the `emit_event(emitter, "archive-restore-progress", …)` line, ~l.181)
- Modify: `src/components/archive/ArchiveProgress.tsx`
- Modify: `src/components/archive/ArchiveResumeBanner.tsx`

**Interfaces:**
- Produces: rollback emits `archive-progress` (existing `RollbackProgress` payload — shape already matches `ArchiveProgressEvent`) and a terminal `archive-finished { operation_id, outcome: "completed"|"failed", kind: "rollback" }`.

- [ ] **Step 1: Write the failing core test** in `rollback.rs`'s `#[cfg(test)]` module. Seed a rolled-backable operation by copying the seeding block from the existing restore test (`crates/athenaeum-core/src/archive/restore.rs` tests, lines ~766-798 — tempdir + `init_db` + scan_roots/frames_set/files/frames/archive_operations INSERT chain). Use a capturing emitter (reuse the crate's existing test emitter if one exists — `rg "impl ProgressEmitter for" crates/athenaeum-core/src` — otherwise define a local one):

```rust
struct CapturingEmitter(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
impl ProgressEmitter for CapturingEmitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        self.0.lock().unwrap().push((event.to_string(), payload));
    }
}

#[test]
fn rollback_emits_unified_progress_and_finished_events() {
    // …seeding block as in restore.rs tests, with one file whose
    // DeleteSource step is Done so the restore loop runs…
    let emitter = CapturingEmitter(Default::default());
    rollback_operation(&conn, op_id, &emitter).unwrap();
    let events = emitter.0.lock().unwrap();
    assert!(events.iter().any(|(n, _)| n == "archive-progress"));
    let (_, fin) = events.iter().find(|(n, _)| n == "archive-finished").expect("finished event");
    assert_eq!(fin["outcome"], "completed");
    assert_eq!(fin["kind"], "rollback");
    assert!(events.iter().all(|(n, _)| n != "archive-rollback-progress"));
}
```

(Adapt the `emit` signature to the real `ProgressEmitter` trait — check `crates/athenaeum-core/src/events.rs` first.)

- [ ] **Step 2: Run it, verify it fails** — `cargo test -p athenaeum-core rollback_emits` → FAIL (no `archive-progress`, no finished event).
- [ ] **Step 3: Implement.** In `rollback.rs`: change the event name at l.54 from `"archive-rollback-progress"` to `"archive-progress"`. After the final `update_operation_status(…, ArchiveStatus::RolledBack, …)` add:

```rust
#[derive(Serialize)]
struct RollbackFinished<'a> {
    operation_id: i64,
    outcome: &'a str,
    kind: &'a str,
}
emit_event(emitter, "archive-finished", &RollbackFinished {
    operation_id,
    outcome: "completed",
    kind: "rollback",
});
```

In `restore.rs`: delete the `"archive-restore-progress"` emission line (the `"archive-progress"` one above it stays).
- [ ] **Step 4: Run tests** — `cargo test -p athenaeum-core archive` → PASS (existing restore/rollback tests use `NullEmitter` and assert DB state only; no test asserts the old event names).
- [ ] **Step 5: Frontend — show the progress.** In `ArchiveProgress.tsx`: teach it the `rollback` kind — extend the verb/title/label logic:

```tsx
// verb in the notification (l.49):
const verb = payload.kind === 'restore' ? 'Restore'
  : payload.kind === 'rollback' ? 'Rollback' : 'Archive';
// title (l.113-118): treat rollback like its own mode
const isRollback = finished?.kind === 'rollback' || progress?.stage === 'restore_source';
// → `${isRestore ? 'Restore' : isRollback ? 'Rollback' : 'Archive'} operation #${operationId}`
// statusLabel: for finished.kind === 'rollback' && outcome === 'completed' → 'Rolled back'
```

In `ArchiveResumeBanner.tsx`: mount the widget during rollback so the events are actually seen — add `const [rollingBack, setRollingBack] = useState(false);`, set it `true` in the Roll back `onClick` before `await rollbackArchiveOperation(op.id)` (and don't `setDismissed(true)` until the widget's `onClose` fires), and render below the banner row:

```tsx
{rollingBack && (
  <div className="fixed bottom-4 right-4 z-50 w-80">
    <ArchiveProgress operationId={op.id} onClose={() => { setRollingBack(false); setDismissed(true); }} />
  </div>
)}
```

- [ ] **Step 6: Gates** — `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`.
- [ ] **Step 7: Commit** — `fix(archive): rollback progress emits on the unified archive-progress channel and the resume banner shows it`

---

## Phase B — frontend deletions (pure removals; gate: `npx tsc --noEmit`)

### Task 2: Delete orphan frontend files

**Files (delete — all verified zero-importer, including index.html, dynamic imports, string maps):**
- `src/App.css`
- `src/components/calendar/CalendarEventPopup.tsx`
- `src/components/CalibrationGroupsView.tsx`
- `src/components/CalibrationStatusBadges.tsx`
- `src/components/QuickStats.tsx`
- `src/components/SelectionToolbar.tsx`
- `src/components/SortableColumnHeader.tsx`
- `src/hooks/useViewportBounds.ts`
- `src/utils/coordinates.ts` (dead duplicate — the live `angularDistance` is in `ObjectsFilterPanel.tsx:238` and stays)
- `src/components/export/index.ts` (barrel bypassed by every consumer; its 6 re-exported components are alive via direct imports and stay)

- [ ] **Step 1:** `git rm` the 10 files.
- [ ] **Step 2:** `npx tsc --noEmit` → clean.
- [ ] **Step 3: Commit** — `chore(frontend): delete ten orphan files (audit 2026-08-03)`

### Task 3: Delete the dead calibration table subtree + prune its barrel

**Files:**
- Delete: `src/components/calibration/CalibrationSetsTable.tsx` (its 3 exports — `CalibrationSetsTable`, `typeColors`, `subCalTypeColors` — are all dead)
- Delete: `src/components/calibration/MatchBadges.tsx` (all 11 exports dead; NOTE: `CalibrationTableView.tsx:515` has an unrelated LOCAL `MatchBadge` — different shape, stays)
- Modify: `src/components/calibration/index.ts` — remove the re-export lines for: `CalibrationSetsTable`, `typeColors`, `subCalTypeColors`, `MatchBadge`, `MatchBadges`, `extractLightParams`, `exactMatchLevel`, `tempMatchLevel`, `fmtVal`, `exactTooltip`, `tempTooltip`, `matchStyles`, `MatchLevel`, `LightParams`, `buildFilterKey`, `collectFilterGroupWarnings`. The barrel FILE stays (Settings.tsx imports `CalibrationMatchingConfig` through it).
- Modify: `src/components/calibration/utils.ts` — delete only `buildFilterKey` and `collectFilterGroupWarnings`; the file stays (`buildCameraFilterTree`/`buildMergedCameraFilterTree` + interfaces are live via direct imports).

- [ ] **Step 1:** apply deletions/edits. **Step 2:** `npx tsc --noEmit` → clean. **Step 3: Commit** — `chore(frontend): delete superseded CalibrationSetsTable/MatchBadges and prune the calibration barrel`

### Task 4: Delete dead exports inside live frontend files

**Files (modify; every name verified zero-consumer):**
- `src/hooks/useTauri.ts`: delete `useScanRoots` (l.16-123), `useScan` (281-307), `useFiles` (312-340), `useFilesByDirectory` (345-378). Keep `useScanRootsWithAvailability`, `useDuplicates`, `useDuplicateFolders` (live).
- `src/hooks/useExportData.ts`: delete `useExportData`, `useExportableFrameSets`, `useCalibrationRoute`. Keep `useExportSummary`, `useWbppConfig`.
- `src/api/archive.ts`: delete `setArchiveRootPath` (l.20) and `deleteArchive` (l.114) wrappers (backend commands stay — out of scope).
- `src/components/Toolbar.tsx`: delete `ToolbarGroup` (+its props interface, l.21-36).
- `src/components/dualpane/types.ts`: delete `splitPath` (150) and `joinPath` (173).
- `src/utils/projectionBounds.ts`: delete `clampToProjectionBoundary` (29-76) and `getProjectionBoundingBox` (232-282); `clampRectangleToProjection` + `isPixelInProjection` stay.
- `src/types/selection.ts`: delete `SelectionBounds`, `SelectionData`, `SelectionState`; keep `DrawingMode`, `SelectionResult`, `SelectionCandidates`.
- `src/types/helpers.ts`: delete these 30 verified-dead names — functions/consts `createParameterConfig`, `exactMatch`, `warningMatch`, `ignoreParam`, `ignoreWithWarningSupport`, `isExactOrDisabled`, `DEFAULT_ANALYSIS_CONFIG`, `EXACT_OR_DISABLED_PARAMETERS`, `WARNING_CAPABLE_PARAMETERS`; interfaces/types `Day`, `Setup`, `CalibrationSet`, `Tag`, `FrameTag`, `ExportTemplate`, `DirectoryContents`, `Project`, `FramesSetMember`, `FitsHeader`, `SessionWithMetadata`, `SessionMember`, `FitsImageData`, `RefreshResult`, `CalibrationMatchResult`, `FilterPeriod`, `ReclassifyResult`, `ConfigurableParameter`, `ExactOrDisabledParameter`, `WarningCapableParameter`, `SetUpdateReport`. The file stays (its ~40 other exports are live).

- [ ] **Step 1:** apply all edits in one pass per file. **Step 2:** `npx tsc --noEmit` → clean; `npm run build:web` → succeeds (phase-B end gate). **Step 3: Commit** — `chore(frontend): remove dead exports from live hooks, api wrappers, types`

---

## Phase C — Rust deletions

### Task 5: Dismantle `gate_audit`, keep `GateStage`

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/service.rs` — `GateStage` enum MOVES here (it is genuinely alive: `blind_gate_ok` at l.519 takes it; ~10 construction sites). Delete the `#[cfg(test)]` `gate_audit_disabled_is_zero_behaviour_change` test (l.~573) — it only exercises `gate_audit::enabled()`.
- Delete: `crates/athenaeum-core/src/plate_solve/gate_audit.rs` (after the move: `record_event`, `record`, `sink`, `csv_header`, `csv_quote`, `opt`, `to_csv_row`, `GateAuditRecord`, `enabled`, `GateStage::from_params`, `as_str`, env-var handling, own tests — all dead).
- Modify: `crates/athenaeum-core/src/plate_solve/mod.rs` — drop `mod gate_audit;`, fix `use` paths for `GateStage`.

- [ ] **Step 1:** move the `GateStage` enum definition (WITHOUT `from_params`/`as_str`) into `service.rs`; update all `gate_audit::GateStage` paths. **Step 2:** delete `gate_audit.rs` + module decl + the service.rs test. **Step 3:** `cargo build --workspace && cargo test -p athenaeum-core plate_solve` → PASS. **Step 4: Commit** — `chore(plate-solve): remove unreachable gate-audit write path, GateStage lives in service`

### Task 6: Delete zero-reference core/tauri functions and dead scaffolding surfaces

**Files (modify — every item verified: only reference is its own declaration):**
- `crates/athenaeum-core/src/auto_merge/log_ops.rs`: `count_log_entries` (143)
- `crates/athenaeum-core/src/calibration/config.rs`: `get_clustering` (566)
- `crates/athenaeum-core/src/calibration/configurable_matcher.rs`: `find_bias_for_frame` (712)
- `crates/athenaeum-core/src/calibration/processor.rs`: `process_frame_set_with_progress` (319), `clear_calibration_links_for_frame_set` + `_with_options` (383, 394 — only call each other)
- `crates/athenaeum-core/src/coordinates/mod.rs`: `angular_distance_haversine` (152)
- `crates/athenaeum-core/src/db/analysis.rs`: `delete_frame_analysis`, `delete_analyses_for_frame_set`, `delete_analyses_for_missing_files` (113/118/131)
- `crates/athenaeum-core/src/db/calibration_links.rs`: `get_calibration_statistics` (411)
- `crates/athenaeum-core/src/db/operations.rs`: `get_all_settings` (2095), `update_frames_set_flat_pattern` (2248), `clone_session` (2999), `reclassify_excluded_frames` (3082), `get_frame_ids_for_file_ids` (3128), `get_excluded_frames` (3808 — superseded by live `get_excluded_frames_with_metadata`), `get_catalog_meta` (3870)
- `crates/athenaeum-core/src/rustafits_processor/mod.rs`: `process_fits_to_jpeg_cached` (204), `default_quality` (42)
- `crates/athenaeum-core/src/fits_writer/keywords.rs`: `radec` (152), `roworder_top_down` (167)
- `crates/athenaeum-core/src/settings/mod.rs`: `clear_runtime_override` (262), `clear_all_runtime_overrides` (270). For `keys::AUTO_MERGE_ON_BUTTON_CLICK` + `defaults::AUTO_MERGE_ON_BUTTON_CLICK`: KEEP, add `#[allow(dead_code)]` + the same "used by frontend via raw string, not read in Rust" comment its sibling `DUPLICATES_CONTENT_HASH_RESCANNED` carries (consistent convention; the setting itself is live from `Settings.tsx`/`FrameSetDetail.tsx`).
- `crates/athenaeum-core/src/services/operation_queue.rs`: delete the whole never-exposed inspector surface — `running_snapshot` (118) AND `pending_snapshot` (113) AND `QueueEntry` if nothing else references it (verify with `rg -w QueueEntry crates/` first; the analogous `ComputeQueue::snapshot` is live and untouched).
- `crates/athenaeum-tauri/src/commands/utils.rs`: `angular_distance` (49), `format_bytes` (61) + their `#[cfg(test)]` tests (100-115). If the file becomes empty, delete it + its `mod` decl + any `use` in `commands/mod.rs`.

- [ ] **Step 1:** apply all deletions in one pass. **Step 2:** `cargo build --workspace` → clean; `cargo test -p athenaeum-core` → PASS. **Step 3: Commit** — `chore(core): delete 30 zero-reference functions and the unshipped operation-queue inspector`

### Task 7: Delete the never-built file_op rollback lifecycle variants

**Files:**
- Modify: `crates/athenaeum-core/src/file_op/models.rs` — delete `FileOpStatus::RollingBack` (38), `FileOpStatus::RolledBack` (39), `FileOpStage::CommitDelete` (138), `FileOpStage::RollbackRestore` (140) + their `as_str()` arms (50-51, 61, 149-150).
- Modify: `crates/athenaeum-core/src/file_op/db.rs` — THREE sites, not one: (a) the `matches!()` terminal-state check at l.50-53; (b) the hand-coded SQL string in `list_unfinished_operations`: `WHERE status IN ('pending','running','rolling_back')` → drop `'rolling_back'`; (c) any other match arms the compiler flags.
- Do NOT touch `MoveStrategy::Delete` / `FileOpKind::Delete` (documented vestigial, loud-reject path is tested).

No DB migration risk: these status strings were never written by any code path, so no legacy rows can contain them. Not ts_rs-exported.

- [ ] **Step 1:** apply; let the compiler find every match arm. **Step 2:** `cargo build --workspace && cargo test -p athenaeum-core file_op` → PASS. **Step 3: Commit** — `chore(file-op): drop never-constructed rollback lifecycle variants (scaffolding for an unbuilt feature)`

### Task 8: Perseus — delete vestigial key helpers, reuse core's disk probe

**Files:**
- Modify: `crates/perseus/src/run.rs`:
  - Delete `load_or_create_device_key` (l.142), both `tighten_permissions_if_needed` cfg-variants (166/184), both `write_secret_0600` variants (190/205), `random_secret` if now unused, and the 3 `#[cfg(all(test, unix))]` tests that were their only callers (l.~2295+). Production is untouched: `Agent::start → SharedIrohNode::bind` already uses core's `DeviceKey::load_or_create_in` (`node.rs:914`) — verified, so the audit's "Windows ACL gap" was a false alarm; this is pure dead code.
  - Delete the local `disk_usage_pct` (l.1589-1611, byte-identical to core's) + its `cfg(not(unix))` stub; replace the one call site (l.~2054 `dirs.iter().map(disk_usage_pct).max()`) with `athenaeum_core::api::retention::disk_usage_pct` (already `pub`; `api::retention` has no render/solver feature dependency — perseus's `default-features = false` is fine).

- [ ] **Step 1:** apply. **Step 2:** `cargo build -p perseus && cargo test -p perseus` → PASS. **Step 3: Commit** — `chore(perseus): drop dead device-key helpers (prod uses core DeviceKey) and reuse core disk_usage_pct`

### Task 9: Remove five unused dependencies

Verified by actual compile in a throwaway worktree (clean 2m26s workspace build without them; `memmap2`/`base64` remain as transitive deps of iroh-blobs et al.).

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` — remove `memmap2`
- Modify: `crates/athenaeum-tauri/Cargo.toml` — remove `anyhow`
- Modify: `crates/athenaeum-web/Cargo.toml` — remove `futures`, `base64`
- Modify: `crates/catalog-builder/Cargo.toml` — remove `serde` (only `serde_json` is used; derives live in core)
- Refresh: `Cargo.lock` via `cargo check --workspace`

- [ ] **Step 1:** apply + `cargo check --workspace` (refreshes the lock). **Step 2 (phase-C end gate):** `cargo build --workspace && cargo test --workspace` → PASS. **Step 3: Commit** — `chore(deps): drop five unused direct dependencies`

---

## Phase D — wrapper business logic moves into core (CLAUDE.md rule repair)

Every pair below was diffed byte-for-byte identical (modulo error type / rustfmt). Pattern for all four tasks: move the tauri copy verbatim into core (it is the canonical one), adapt error handling to `anyhow::Result`, make both wrappers 3-6-line delegations (`.map_err(|e| e.to_string())` tauri-side, `(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())` web-side), keep command names/routes/`#[tracing::instrument]` attributes and event names EXACTLY as they are. If a moved struct derives `ts_rs::TS` and is registered in `ts_export.rs`, update only the `use` path in the registry — generated TS must not change (`git diff src/types/` must be empty after regeneration, if regeneration is part of the build).

### Task 10: Spatial + Calendar query layer → core

**Files:**
- Create: `crates/athenaeum-core/src/api/spatial.rs` — `pub fn get_imaging_locations(conn: &Connection) -> anyhow::Result<Vec<ImagingLocation>>` (move `commands/spatial.rs:16-195` verbatim incl. the UNION-ALL SQL, circular-mean rotation, FOV calc, pseudo-ID synthesis, and the `ImagingLocation`-family structs) and `pub fn query_frames_in_bounds(conn: &Connection, <same args as today>) -> anyhow::Result<…>` (move `commands/spatial.rs:210-269`, meridian-wrap query builder).
- Create: `crates/athenaeum-core/src/api/calendar.rs` — `pub fn get_calendar_month_data(conn: &Connection, year: i32, month: u32) -> anyhow::Result<CalendarMonthData>` (move ALL of `commands/calendar.rs:16-231`; structs move too).
- Modify: `crates/athenaeum-core/src/api/mod.rs` (or `lib.rs`, wherever `api` submodules are declared) — register both modules.
- Modify: `crates/athenaeum-tauri/src/commands/spatial.rs`, `commands/calendar.rs` — bodies become delegations.
- Modify: `crates/athenaeum-web/src/routes/spatial.rs` — spatial handlers delegate; the calendar handler MOVES to a new `crates/athenaeum-web/src/routes/calendar.rs` (restores the 1:1 mirror convention that broke here). Register in `routes/mod.rs`; the URL path (`/api/get_calendar_month_data`) must not change.

- [ ] **Step 1: Add core smoke tests first** (these queries had zero coverage). In `api/calendar.rs` test mod: seed an in-memory DB (clone the fixture pattern from `configurable_matcher.rs` tests ~l.936) with 2 LIGHT frames on two dates in one month; assert `get_calendar_month_data` returns those 2 day entries with correct counts. In `api/spatial.rs`: seed 2 frames with RA/Dec; assert `get_imaging_locations` returns 1 location and `query_frames_in_bounds` finds them within bounds and honors a meridian-wrapping window (min_ra > max_ra). Run → FAIL (functions don't exist yet).
- [ ] **Step 2:** move the code; make tests pass: `cargo test -p athenaeum-core api::` → PASS.
- [ ] **Step 3:** rewrite all wrappers as delegations; `cargo build --workspace && cargo test -p athenaeum-web` → PASS.
- [ ] **Step 4:** sanity-diff: the moved SQL strings are byte-identical to the pre-move tauri versions (`git diff` review).
- [ ] **Step 5: Commit** — `refactor(spatial,calendar): move duplicated query layer into core; wrappers delegate`

### Task 11: Export tree builder + exportable-sets query → core

**Files:**
- Create: `crates/athenaeum-core/src/export/frame_set_queries.rs` — `pub struct ExportableFrameSet` (currently defined TWICE, once per wrapper — single definition now), `pub fn get_exportable_frame_sets(conn: &Connection) -> anyhow::Result<Vec<ExportableFrameSet>>` (move `commands/export.rs:119-170`), `pub fn get_calibration_route(conn: &Connection, frame_set_id: i64) -> anyhow::Result<CalibrationRouteSummary>` (move `commands/export.rs:190-422` — recursive `CalibrationTreeNode` assembly, missing-calibration placeholders, summary aggregation; move the tree structs unless they already live in core's export models — check `export/models.rs` first and reuse if present).
- Modify: `crates/athenaeum-core/src/export/mod.rs` — register module.
- Modify: `crates/athenaeum-tauri/src/commands/export.rs`, `crates/athenaeum-web/src/routes/export.rs` — delegations (web's unrelated `get/set/reset_wbpp_export_config` handlers untouched).

- [ ] **Step 1:** core test first: seed a frame set with one Flat set linked (fixture pattern as Task 10); assert `get_exportable_frame_sets` returns it and `get_calibration_route` yields a tree whose root has one Flat child. Run → FAIL. **Step 2:** move code, tests PASS (`cargo test -p athenaeum-core export`). **Step 3:** delegate wrappers; `cargo build --workspace && cargo test -p athenaeum-web export` → PASS. **Step 4: Commit** — `refactor(export): calibration-route builder and exportable-sets query live in core`

### Task 12: `auto_generate_frame_sets` workflow → core

**Files:**
- Create: `crates/athenaeum-core/src/api/frame_sets.rs` — `pub fn auto_generate_frame_sets(ctx: &ServiceContext, _project_id: Option<i64>, emitter: &dyn ProgressEmitter) -> anyhow::Result<AutoGenerateResult>` — move `commands/frame_sets.rs:11-179` verbatim: membership filtering, clustering invocation, excluded-frame persistence, per-cluster set creation, session detection, AND the collab project-match block (`find_matching_projects` → `project-set-match` emission, "never auto-link"). Signature note: tauri passes `Some(project_id)` from its existing required `i64` IPC arg, web passes its existing `Option<i64>`; core ignores it (frame sets are global — CLAUDE.md).
- Modify: `crates/athenaeum-tauri/src/commands/frame_sets.rs`, `crates/athenaeum-web/src/routes/frame_sets.rs` — delegations (emitters stay as today: `TauriProgressEmitter` / `SseProgressEmitter`). External IPC/HTTP arg shapes DO NOT change.

- [ ] **Step 1:** core test first: seed 3 LIGHT frames with close coords + 1 far frame; assert one set is created containing 3, and the far frame lands in excluded with `NoCoords`-or-threshold reason as appropriate. Run → FAIL. **Step 2:** move, PASS. **Step 3:** delegate, `cargo build --workspace && cargo test --workspace` (frame-set tests both crates) → PASS. **Step 4: Commit** — `refactor(frame-sets): auto_generate_frame_sets workflow lives in core; wrappers delegate`

### Task 13: `load_frame_with_path` — one canonical copy

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` — add `pub fn load_frame_with_path(conn: &Connection, frame_id: i64) -> anyhow::Result<(Frame, String)>`: move the body of `registration/service.rs:670-739` (named-column access, all ~30 fields incl. the correctly-read CFA columns, RFC3339 → `"%Y-%m-%d %H:%M:%S"` date fallback) verbatim.
- Modify: `crates/athenaeum-core/src/registration/service.rs` — delete its private copy; call the new one at its single internal call site (l.~134).
- Modify: `crates/athenaeum-tauri/src/commands/plate_solve.rs` (delete l.864-926 copy) and `crates/athenaeum-web/src/routes/plate_solve.rs` (delete l.878-933 copy) — both call core, converting errors at the boundary (web keeps its `StatusCode::NOT_FOUND` mapping for the missing-frame case: match on the error or have core return a distinguishable `anyhow` context string checked by the wrapper — simplest faithful option: core returns `Result<Option<(Frame, String)>>`, `None` = not found, and each wrapper maps `None` to its own not-found shape).

- [ ] **Step 1:** implement with the `Option` return; adapt the 3 call sites. **Step 2:** `cargo build --workspace && cargo test -p athenaeum-core registration plate_solve` → PASS. **Step 3: Commit** — `refactor(db): single canonical load_frame_with_path replaces three copies`

---

## Phase E — row-mapper consolidation (+ the latent CFA fix)

### Task 14: Calibration group loaders — shared mapper, CFA columns populated

The three copies hardcode `bayerpat/xbayroff/ybayroff/roworder: None` (SELECTs omit the columns) — same bug family `420bde27` just fixed in two other loaders. Latent today (nothing downstream reads these fields off dark/bias/flat group frames yet) but these feed real matcher logic (`try_create_dark_for_frame`, `try_create_bias_for_frame`, `flat_matcher.rs:157`), so fix before it bites.

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/dark_bias_groups.rs` — `execute_dark_query` (~312) and `execute_bias_query` (~426): extend both SELECT lists (l.149, l.258) with `f.bayerpat, f.xbayroff, f.ybayroff, f.roworder`; replace both 68-line inline mappers with the shared fn below.
- Modify: `crates/athenaeum-core/src/calibration/flat_groups.rs` — same for `detect_flat_groups` (SELECT at l.97, mapper ~218).
- Create (in `crates/athenaeum-core/src/calibration/mod.rs` or a new `calibration/frame_row.rs`): `pub(crate) fn frame_from_group_row(row: &rusqlite::Row) -> rusqlite::Result<Frame>` + a shared `pub(crate) const GROUP_FRAME_SELECT_COLUMNS: &str` so the column order and the index mapping live in exactly one place (align the three SELECTs to that column list — the queries' WHERE/GROUP BY parts stay per-function).

- [ ] **Step 1: Failing test:** seed a DARK frame row with `bayerpat='RGGB', xbayroff=1, ybayroff=0, roworder='BOTTOM-UP'` (clone the fixture from this module's existing tests / `configurable_matcher.rs` builders); assert the `Frame` coming out of `detect_dark_groups` carries those values, not `None`. Run → FAIL.
- [ ] **Step 2:** implement shared const + mapper; wire all three loaders. Test PASS.
- [ ] **Step 3:** `cargo test -p athenaeum-core calibration` → all existing matcher/group tests PASS.
- [ ] **Step 4: Commit** — `fix(calibration): group loaders share one row mapper and carry the CFA columns`

### Task 15: `db/operations.rs` File+Frame mappers — one SELECT const, one mapper

**IMPORTANT: locate by function NAME, not by audit line numbers (they were stale):** `get_files` (~851), `get_files_by_directory` (~951), `get_files_by_directory_for_camera` (~1067). Differences to preserve exactly: `_for_camera` = INNER-join semantics + `AND fr.instrume = ?` + unconditional `Some(Frame{…})`; `get_files_by_directory` = LEFT JOIN + optional frame; both share `native_separator_of`/`path_prefix_upper`/`expected_depth` machinery and `ORDER BY f.filename`. Both also hardcode `xbayroff/ybayroff/roworder: None` — fix like Task 14 (add columns to the shared SELECT).

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` — follow the file's OWN existing precedent (`MISSING_METADATA_SELECT` const + `map_missing_metadata_row`): add `const FILE_FRAME_SELECT: &str` + `fn map_file_frame_row(row: &Row) -> rusqlite::Result<(File, Option<Frame>)>`; rewrite the three functions on top of it (camera filter = extra WHERE + param; `_for_camera` unwraps the `Option` it statically knows is `Some`).

- [ ] **Step 1: Failing test:** seed one file+frame (with CFA values) and one frame-less file in a directory; assert `get_files_by_directory` returns both (frame `Some`/`None` respectively, CFA populated) and `get_files_by_directory_for_camera` returns only the matching-camera one. Run → FAIL (CFA fields None today). **Step 2:** implement; PASS. **Step 3:** `cargo test -p athenaeum-core db` → PASS (existing dual-pane/browse tests must stay green — the directory-prefix machinery is untouched). **Step 4: Commit** — `refactor(db): shared file+frame row mapper; directory browse carries CFA columns`

### Task 16: `db/equipment.rs` — one parameterized camera-library query

Preserve the REAL divergences (verified): `get_camera_dark_library` = `is_master_library = 0`, NO imagetyp filter (it's "raw calibration library" despite the name), ORDER BY `cs.imagetyp, cs.exptime, cs.ccd_temp`; `get_camera_master_dark_library` = `+ imagetyp IN ('MasterDark','MasterBias','MasterDarkFlat')`, same ORDER; `get_camera_master_flat_library` = `+ imagetyp = 'MasterFlat'`, ORDER BY `cs.filter, cs.exptime, cs.ccd_temp`.

**Files:**
- Modify: `crates/athenaeum-core/src/db/equipment.rs` — private `fn query_camera_calibration_sets(conn, instrume: &str, is_master: bool, imagetyp_filter: Option<&[&str]>, order_by: &str) -> Result<Vec<…>>` holding the shared SELECT + row mapper; the three public functions become one-line calls with their exact parameter triples. Public signatures unchanged. The three trivial `has_*_library` COUNT wrappers stay as-is.

- [ ] **Step 1: Pin-behavior test first:** seed three sets (raw Dark / MasterDark / MasterFlat, one camera); assert each public fn returns exactly its own set (this pins the WHERE divergences). Run against CURRENT code first → must PASS (baseline — this is a refactor-safety net, so unlike the other tasks this test is written before the change but is not expected to fail). **Step 2:** consolidate. **Step 3:** `cargo test -p athenaeum-core equipment` → PASS. **Step 4: Commit** — `refactor(db): one parameterized query behind the three camera-library accessors`

### Task 17: `fits_parser` — shared FITS/XISF `Frame` assembly tail

The tails are word-for-word identical (`parse_fits` l.380-453 ↔ `parse_xisf` l.776-849): IMAGETYP→FRAME fallback, is_master heuristic (IMAGETYP prefix OR filename `"master"`/`"_calibrated_"`/`"-calibrated-"`), binning string, final ~35-field `Frame{}` literal. Only the header-lookup mechanism differs — so the shared helper takes already-resolved values, not a header object.

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/mod.rs` — extract `fn finalize_frame(/* the already-parsed locals both tails use: resolved keyword values, path, computed fields */) -> Frame`; both parsers call it. No behavior change; field-for-field identical output.

- [ ] **Step 1:** extract; compiler drives the parameter list. **Step 2:** `cargo test -p athenaeum-core fits_parser` → all existing FITS/XISF parse tests PASS (these have real coverage — they are the safety net). **Step 3 (phase-E end gate):** `cargo test --workspace` → PASS. **Step 4: Commit** — `refactor(fits-parser): single Frame-assembly tail shared by FITS and XISF paths`

---

## Phase F — frontend extractions

### Task 18: Generic `<QueueIndicator>` replaces the triplet

The three files are near-verbatim clones; verified real diffs to preserve: icon (`BarChart3`/`Layers`/`Crosshair`), context hook, label fallback (`'Analysis'`/`'Registration'`/`'Plate solve'`), cancel title, and **PlateSolve's two genuine behaviors** — `useSmoothedPercent(realPercent, total)` + `transition-[width] duration-100 ease-linear` (vs `transition-all duration-300`).

**Files:**
- Create: `src/components/QueueIndicator.tsx`:

```tsx
import type { LucideIcon } from 'lucide-react';
import { X } from 'lucide-react';

export interface QueueIndicatorProps {
  collapsed: boolean;
  icon: LucideIcon;
  active: boolean;
  label: string;            // already-resolved display name
  percent: number;          // 0-100, already smoothed if the caller smooths
  current?: number;
  total?: number;
  queueLength: number;
  cancelTitle: string;
  onCancelAll: () => void;
  /** 'linear' = plate-solve's JS-smoothed bar; 'smooth' = default CSS transition */
  barTransition?: 'smooth' | 'linear';
}
export function QueueIndicator(props: QueueIndicatorProps) { /* the shared body,
  Tailwind classes copied verbatim from AnalysisQueueIndicator, with
  barTransition === 'linear' ? 'transition-[width] duration-100 ease-linear'
                             : 'transition-all duration-300' */ }
```

- Modify: `AnalysisQueueIndicator.tsx`, `RegistrationQueueIndicator.tsx`, `PlateSolveQueueIndicator.tsx` — each becomes its context-hook glue (~15 lines) rendering `<QueueIndicator …/>`; PlateSolve keeps `useSmoothedPercent` at its call site and passes `barTransition="linear"`. Mount points in `Layout.tsx` unchanged.

- [ ] **Step 1:** implement; `npx tsc --noEmit` → clean. **Step 2:** visual check via `npm run dev:web` (three indicators render in the sidebar; run any analysis to see one live if convenient — optional). **Step 3: Commit** — `refactor(frontend): one generic QueueIndicator behind analysis/registration/plate-solve glue`

### Task 19: `CalibrationTableView.tsx` — deduplicate the Flats/Darks/Bias trio

There are FOUR tables; only `FlatsTable` (811-962) / `DarksTable` (968-1113) / `BiasTable` (1119-1246) are the near-identical trio. `LightsTable` (648-805) is structurally different — DO NOT touch it. Do NOT change BiasTable's pre-existing reversed `B,G,O` column order (preserve behavior; note it in the commit body if desired).

**Files:**
- Modify: `src/components/calibration/CalibrationTableView.tsx` (module-scope additions, no new file):
  - `function useTableSort<F extends string>(initial: F): { sortField: F; sortDir: SortDir; handleSort: (f: F) => void; thProps: … }` — replaces the three byte-identical sortField/sortDir/handleSort blocks.
  - `function MatchCells({ gMatch, bMatch, oMatch, order }: …)` — the three `MatchBadge` cells incl. the `Math.abs(diff) < 0.01` gain/offset and `===` binning comparisons, with `order` covering Bias's reversed sequence.
  - `function CreateMasterCell({ setId, onCreateMaster, buildStatusBySet }: …)` — the trailing Hammer-button + "building…" pulse cell.
  - Rewire the three tables onto these; per-table columns/colors/sub-cal links stay inline (they genuinely differ).

- [ ] **Step 1:** implement in one complete pass (project rule: no many small partial edits to a 1,652-line file). **Step 2:** `npx tsc --noEmit` → clean; `npm run build:web` → succeeds. **Step 3:** visual check on the calibration hierarchy view (sorting each of the three tables, create-master button states). **Step 4: Commit** — `refactor(frontend): shared sort/match/create-master cells for the calibration set tables`

---

## Final gate (after Task 19)

- [ ] `cargo build --workspace` && `cargo test --workspace` && `npx tsc --noEmit` && `npm run build:web` — all green.
- [ ] `git log --oneline` review: 19 commits, one per task, user as author.

## Explicitly deferred (tracked, not in this cycle)

1. `WarningType`/`SkipReason` unfinished producers — either wire the producers or delete variants + the frontend cases together (cross-language change; owner call).
2. `IrohTransport` retirement ("Task 3" migration) — the announce-block duplication resolves itself there.
3. Test-fixture `test_support` module (~1.5-2.5k duplicated test lines).
4. Frontend clusters: CameraFilterTree/Merged tree hook, master-library filter hook, calibration-set picker list, confirm/alert dialog hook, hierarchy-view parallels.
5. Collision-suffix helper unification (user-visible filename formats differ).
6. Unreachable backend commands + `rebuild_master` UI wiring — owner decisions.
7. `ObjectsFilterPanel.tsx`'s locally-exported `angularDistance` — alive but oddly placed; candidate for `src/utils/` in a later pass.
