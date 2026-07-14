# Collab Slice 5 — E2E + Payoff (Project WBPP Export, Notifications Polish) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The processor's payoff and the proof of the whole Stage II: a project-scoped WBPP export ("export combined dataset, calibration off" — spec §7), the three-instance E2E scenario (§11: contributor → gated publish → push-seed → moderation → swarm delivery → export), and the §9 notifications polish (replicated / downloaded / awaiting-approval).

**Architecture:** The export reuses the existing UNGATED WBPP machinery wholesale — `ExportFrame`/`ExportGroup`/`make_group_key`/`organize_files_wbpp` stay untouched; the only new piece is a collector that builds `ExportFrame`s from `project_contributions` (landed paths + snake_case `frame_meta` snapshots) unioned with the processor's own calibrated outputs for the project's linked sets. Notifications ride two existing seams: `SyncFinishedEvent.projectId` (already on the wire since slice 4) branches `notifyFinished` project-aware, and the poll diff gains an `awaitingApproval` kind. The E2E lives IN-CRATE (`#[cfg(test)]`) because its building blocks (`store_token_for_test`, `set_started_for_test`, seed helpers) are `cfg(test)`-only — the audited explorer verdict.

**Tech Stack:** Rust (existing export module — ungated; wiremock + LoopbackTransport), React 18 + TS.

## Global Constraints

- **Repo/branch:** athenaeum, continue on `0.5.0` (slice-4 tip `4ae96dfa`). No version bumps.
- **Two backends in sync:** the one new command ships Tauri + Axum in the same commit; wrappers thin; `#[tracing::instrument(skip_all, err)]` / `err(Debug)`.
- **Feature gating:** `export/` is UNGATED (verified: `lib.rs:24` no cfg) and the calibration-off path (`ExportMode::RawWithCalibrationSets`) touches no gated code — the project collector + export command live UNGATED (`api/collab_exchange.rs`). `cargo build -p perseus --no-default-features` stays green. The E2E may call render-gated `publish_collab_frames` — test targets build with default features.
- **Never swallow errors**; notifications only via `notify()`; design tokens; `api.listen` cancelled-flag; models.ts regenerated never hand-edited.
- **Export reuse contract (verified against code):** `ExportFrame` fields map from contributions as `file_path = landed_path`, `filename` = basename of `landed_path`; `filter`/`bayerpat`/`instrume`/`exptime` parse from `frame_meta` (a snake_case `models::Frame` snapshot — NO serde rename on `Frame`); grouping is `(filter, CameraType::from_bayerpat)` ONLY (no exposure/night); `organize_files_wbpp(output_dir, data, use_symlinks, config, emitter, frame_set_id, cancel_flag)` skips existing dests (never overwrites) and warns-not-fails on missing sources; the root folder is `sanitize_display_folder_name(data.frame_set_name)`.
- **Events:** project export reuses `"export-progress"`/`"export-complete"` (`ExportProgressEvent`/`ExportCompleteEvent`) with the sentinel `frame_set_id = -1` (Д3).
- **Tests:** slice gates: `cargo build --workspace`, `cargo test -p athenaeum-core` (FULL — incl. ts_contract), `cargo test -p perseus`, `cargo build -p perseus --no-default-features`, `npx tsc --noEmit`.
- **Commit identity:** `eg013ra1n <vilen.sharifov@gmail.com>`, never a Claude author/co-author line.

## Design decisions (Д1–Д5, for owner review)

- **Д1 — "Combined dataset" = received contributions ∪ own calibrated outputs.** Received: non-superseded `project_contributions` rows. Own: every LIGHT frame of the project's linked sets that has a `light_calibrations` row whose `output_path` exists on disk — NOT gate-filtered (the gate is render-gated and quality-gating is the contributor's publish-time choice; locally the processor stacks what it has). Own frames' metadata comes from the `frames` catalog row.
- **Д2 — Contribution filenames are publisher-prefixed:** `{sanitize_folder_name(publisher_display)}_{basename(landed_path)}` — collision-free across publishers inside one `lights/` folder AND self-documenting provenance in the WBPP tree. Own frames keep their plain calibrated-output basename.
- **Д3 — Project export rides the existing export surface with `frame_set_id = -1`** (sentinel, documented): same events, same `active_exports` cancel registry, same frontend `ExportProgress` context + `kind:'export'` completion notification — all for free. Limitation (accepted): one project export at a time; a concurrent frame-set export with id −1 is impossible (real ids are positive).
- **Д4 — Notifications polish:** (a) `downloadComplete` is REMOVED from the documented `PackageStateChange` kinds (it was never produced); the receive-side notification comes instantly from `SyncFinishedEvent.projectId` instead. (b) `notifyFinished` branches project-aware: sent+confirmed+projectId → "Contribution replicated — safe to go offline" (`kind:'project'`, link `/projects/<id>`, dedupe `collab-sent-<packageId>`); received+projectId → "Project package downloaded — N frames" (`kind:'project'`, dedupe `collab-recv-<packageId>`), and the sync-incoming-unconfigured NUDGE IS SKIPPED for project events (collab lands in the collaboration root, the nudge would mislead). (c) New poll-diff kind `awaitingApproval`: an unknown row arriving `pending && !own` (only coordinators ever receive foreign pending rows — hub visibility enforces the audience) → frontend "Contribution awaiting your approval" (`link:/projects/<id>`, dedupe `pkg-approval-<packageId>`).
- **Д5 — E2E is in-crate `#[cfg(test)]`, multi-thread tokio, `#[cfg(unix)]`** — mirrors `download_happy_path_over_loopback` (the only existing multi-endpoint collab test and the closest template), because the seams it needs (`wire_hub`, `store_token_for_test`, `set_started_for_test`, seed helpers) do not exist for out-of-crate integration targets.

## Task overview (4)

1. Project export collector + export runner (core, ungated) — `collect_project_export_data` + `export_project_for_wbpp`.
2. Export command wiring (both backends) + ProjectDetail export UI.
3. Notifications polish — `awaitingApproval` diff (rust) + project-aware `notifyFinished` + useProjects mapping (frontend).
4. Three-instance E2E (§11) — contributor→coordinator(approval)→processor(swarm download)→WBPP export, one wiremock hub, loopback network.

---

### Task 1: Project export collector + runner (ungated core)

**Files:**
- Create: `crates/athenaeum-core/src/export/project_collector.rs`; Modify: `crates/athenaeum-core/src/export/mod.rs` (declare + re-export)
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (the runner `export_project_for_wbpp`)

**Interfaces:**
- Consumes (all verified file:line): `db::collab_exchange::contributions_for_project(conn, project_id) -> Vec<ContributionRow>` (`collab_exchange.rs:380`; row: `landed_path`, `frame_meta: String`, `publisher_display`, `superseded: bool`, `frame_uuid`); `db::collab::linked_set_ids`; the frames junction query idiom (`api/collab.rs` `union_light_frames`); `db::light_calibrations::get_row_for_frame`-style lookups (read the actual fn names in `db/light_calibrations.rs`); `export::models::{ExportFrame, ExportGroup, ExportData, CameraType, make_group_key, make_display_name, sanitize_folder_name, sanitize_display_folder_name}` (models.rs:39-174); `export::file_organizer::organize_files_wbpp` (file_organizer.rs:112); `WbppExportConfig` (settings key `"export.wbpp_config"`); `crate::api::db` conn idiom; `active_exports` registry on `ServiceContext` (see `commands/export.rs:479-483` usage — the registry lives in core `ServiceContext`).
- Produces (BINDING for Tasks 2/4):

```rust
/// Д1: received (non-superseded) contributions ∪ own calibrated outputs of the
/// project's linked sets. Pure catalog/db read — no hub I/O, ungated.
pub fn collect_project_export_data(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> anyhow::Result<crate::export::ExportData>;

/// The runner: collect → organize under <output_dir>/<sanitized project title>/.
/// Rides the standard export events with the Д3 sentinel frame_set_id = -1.
pub async fn export_project_for_wbpp(
    ctx: &crate::services::ServiceContext,
    project_id: &str,
    output_dir: &str,
    use_symlinks: bool,
    emitter: Option<std::sync::Arc<dyn crate::events::ProgressEmitter>>,
) -> Result<crate::export::ExportResult, crate::api::ApiError>;
```

Collector rules (each a required behavior):
1. Project row must exist (`db::collab::get_project`) — missing ⇒ error "project not in the local cache". `ExportData.frame_set_name` = the project **title** (root folder via the organizer's `sanitize_display_folder_name`); `frame_set_id = -1`.
2. Received: `contributions_for_project`, skip `superseded` rows; per row parse `frame_meta` JSON (snake_case `Frame` snapshot): `filter`, `bayerpat`, `instrume`, `exptime` (+ `ccd_temp`/`gain`/`offset`/`binning`/`date_obs`/`focallen`/`xpixsz` where `ExportFrame` wants them — absent ⇒ None, never invented; malformed JSON ⇒ skip the row with `tracing::warn!`, count into a warnings list). `ExportFrame.file_path = landed_path`, `filename = {sanitize_folder_name(publisher_display)}_{basename(landed_path)}` (Д2).
3. Own: for each `linked_set_ids(conn, project_id)` set, the LIGHT frames (the `union_light_frames` join idiom), each with a `light_calibrations` row whose `output_path` exists on disk (missing file ⇒ skip + `warn!`); metadata from the `frames` row; `file_path = output_path`, `filename` = its basename.
4. Dedup guard: a frame that is BOTH own and received (the processor also contributed and later downloaded its own package — normally impossible since own announcements aren't downloaded, but a coordinator review copy of its own... cannot happen either [own announces skip pending-to-self]; still) — dedupe by `frame_uuid`: own wins, received duplicate skipped with `debug!`.
5. Group into `ExportGroup`s by `(filter, CameraType::from_bayerpat)` with `make_group_key`/`make_display_name`; each group gets ONE `CalibrationSubgroup`-shaped default subgroup with NO calibration nodes (mirror `build_calibration_subgroups`' default-subgroup arm, data_collector.rs:1010-1013 — read it and reproduce the struct shape exactly); totals (`total_exposure` from exptime sums) filled; legacy fields (`filters`, `master_plan`, `calibration_summary`) empty/default.
6. Empty result (no contributions AND no own frames) ⇒ error "nothing to export for this project".

Runner rules: register cancel flag under `-1` in the same `active_exports` registry the frame-set export uses (mirror `commands/export.rs:479-483` — move-into-core check: the registry is reachable from core via `ctx.active_exports`; read it first); emit `"export-progress"` phase `"collecting"` → collect (spawn_blocking) → `organize_files_wbpp(output_dir, &data, use_symlinks, &loaded WbppExportConfig, emitter, -1, &cancel_flag)` → emit `"export-complete"` (`ExportCompleteEvent { frame_set_id: -1, success, files_organized, warnings, error, output_dir }`) → deregister; errors emit `export-complete` with `success:false` + the error string (never swallow).

- [ ] **Step 1: failing collector tests** (in `project_collector.rs`, in-memory conn + `init_db`): (a) two contributions from two publishers with the same basename + different filters land in two groups with publisher-prefixed filenames; (b) a superseded contribution is excluded; (c) own calibrated frame joins its (filter, camera) group with plain basename, and a linked-set frame WITHOUT a calibration row (or with a missing output file — use a tempdir path that doesn't exist) is skipped with a warning; (d) malformed frame_meta skips the row, others survive; (e) empty ⇒ error. Fixtures: real `project_packages`+`project_contributions` inserts (reuse the db::collab_exchange test idioms) + real frames/files/light_calibrations rows for the own side (mirror `api/collab.rs`'s publish-test fixture SQL).
- [ ] **Step 2:** implement collector; focused tests green.
- [ ] **Step 3:** implement the runner + one api-level test: seed a project + one contribution with a REAL tiny FITS as the landed file (`fits_writer::write_fits_f32`), run `export_project_for_wbpp` into a tempdir with `use_symlinks:false`, assert the tree `<out>/<title>/camera_<instrume>/lights/<publisher>_<name>.fits` exists byte-identical, `ExportResult.files_organized == 1`.
- [ ] **Step 4:** gates (`cargo test -p athenaeum-core --lib export`, full core, perseus headless build, workspace) + commit `feat(collab): project-scoped WBPP export — contributions ∪ own calibrated outputs`.

### Task 2: Export command (both backends) + ProjectDetail UI

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/collab.rs` + `lib.rs` `invoke_handler![]`; `crates/athenaeum-web/src/routes/collab.rs` + `routes/mod.rs`
- Create: `src/components/collab/ProjectExportDialog.tsx`; Modify: `src/pages/ProjectDetail.tsx`

**Interfaces:**
- Command `export_collab_project` (Tauri) / `POST /api/export_collab_project` (web): args `{projectId: String, outputDir: String, useSymlinks: bool}` → `ExportResult` (type already TS-exported). Tauri wrapper builds `TauriProgressEmitter`; web uses `SseProgressEmitter` AND validates `output_dir` starts with `state.export_dir` (mirror `routes/export.rs:629-636` exactly — same error shape). Cancel: reuse the EXISTING `cancel_export` command with `frame_set_id = -1` (document on the wrapper).
- UI: an "Export for WBPP" button on the **Receive tab** (processor payoff; visible for the same roles as the tab) opening `ProjectExportDialog`: dir picker (Tauri `pickDirectory()` from the api/desktop idiom; web auto-fills from `get_export_dir` — mirror `ExportTab.tsx:152-158/:261`), symlink toggle (Tauri non-Windows only, mirror `ExportTab.tsx:299-305`), Export button → invoke. Progress + the completion notification are FREE: the global `useExportProgress`/`ExportProgressContext` already listens to `export-progress`/`export-complete` and notifies `kind:'export'` — verify it tolerates `frameSetId: -1` (read the hook; if it keys state by frameSetId a -1 entry is fine; if it looks up a frame-set NAME anywhere, guard it).

- [ ] **Step 1:** wrappers both backends (thin; instrument attrs; web export-dir validation) + registrations.
- [ ] **Step 2:** `ProjectExportDialog.tsx` + Receive-tab button (design tokens; inline `text-error` on failure; disabled while running).
- [ ] **Step 3:** `npx tsc --noEmit` clean; full gates; commit `feat(collab): project WBPP export command + Receive-tab export dialog`.

### Task 3: Notifications polish (Д4)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (poll diff: `awaitingApproval`; doc-comment kind list)
- Modify: `src/hooks/useSyncStatus.ts` (`notifyFinished` project-aware branches + nudge skip)
- Modify: `src/hooks/useProjects.ts` (map `awaitingApproval`)

**Interfaces:** `PackageStateChange.kind` gains `"awaitingApproval"`; documented kinds become `newPackage | approved | rejected | downloadFailed | awaitingApproval` (`downloadComplete` REMOVED — never produced; Д4a). `SyncFinishedEvent.projectId` (already generated in TS) drives the frontend branches.

Behaviors:
1. Rust: in `apply_announcements`, the unknown-row arm additionally emits `awaitingApproval` when `state == "pending" && !own` (hub visibility already restricts foreign pending rows to coordinators — no role check needed app-side; say so in a comment). Existing kinds unchanged.
2. `notifyFinished`: at the TOP, branch on `p.projectId != null`: sent+confirmed → title "Contribution replicated", detail "N frames delivered — safe to go offline", `kind:'project'`, `link:'/projects/'+projectId`, dedupe `collab-sent-<packageId>`; sent+failed → project-toned failure (kind 'project', warning); received ok → "Project package downloaded" + okCount detail, dedupe `collab-recv-<packageId>`, link to the project; received failed → warning equivalent. The sync-incoming-unconfigured NUDGE must not fire for project events (early return before it). Personal-sync behavior byte-identical when projectId is null.
3. `useProjects`: map `awaitingApproval` → notify "Contribution awaiting your approval" detail "<publisher?>" (detail optional — the change carries none; omit), `kind:'project'`, tone 'info', dedupe `pkg-approval-<packageId>`, `link:'/projects/'+projectId`.

- [ ] **Step 1:** rust poll test: coordinator-view fixture — an unknown foreign PENDING announcement produces `awaitingApproval` (and NOT `newPackage`); a second poll produces nothing (row known). Update the kind-list doc + any test asserting the old doc set.
- [ ] **Step 2:** frontend branches; `npx tsc --noEmit`.
- [ ] **Step 3:** full gates; commit `feat(collab): notifications polish — replicated/downloaded project toasts, awaiting-approval diff`.

### Task 4: Three-instance E2E (§11, Д5)

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_e2e_tests.rs`; Modify: `crates/athenaeum-core/src/api/mod.rs` (`#[cfg(test)] mod collab_e2e_tests;` — test-only module, gated additionally by the render feature since it drives `publish_collab_frames`: use `#[cfg(all(test, feature = "render"))]`)

**Interfaces (all verified by the harness explorer — copy-ready):**
- Template: `download_happy_path_over_loopback` (`api/collab_exchange.rs:2016-2209`) — three loopback endpoints, `ProjectReceiveHooks { gate, request_handler }`, `SyncRuntime::new()` + `set_started_for_test(ep, handle, "ticket")`, wiremock hub via `wire_hub(&ctx, &server.uri())` (`:1733`) + `store_token_for_test` (`account.rs:366`). NOTE: those two seams are `pub(crate)`/`#[cfg(test)]` in OTHER modules — check visibility from `api::collab_e2e_tests`; if `wire_hub`/`seed_*` helpers are module-private in `collab_exchange`'s test mod, REPRODUCE minimal local copies (they are small) rather than widening visibility.
- Publish fixtures: `api/collab.rs`'s publish tests build real tiny FITS via `write_fits_f32` + frames/files/light_calibrations/frame_analysis rows + a linked set + a `collab_projects` row with `members_json` — mirror that fixture SQL (read those tests first).
- `publish_collab_frames(ctx, collab_sender, project_id, emitter)` (`collab.rs:1127`), `refresh_project_packages` (`collab_exchange.rs:649`), `decide_announcement(ctx, announcement_id, approve, reason)` (T9), `download_project_package(ctx, sync, project, package)` (`:869`), `list_moderation_queue`, Task-1's `export_project_for_wbpp`, `wait_until` idiom (`collab_exchange.rs:1545-1556`), `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` + `#[cfg(unix)]`.

Scenario (one test, `three_instance_project_flow_publish_moderate_deliver_export`):
1. Three `test_ctx`-style contexts (tempdir file DBs): CONTRIB, COORD, PROC. One `LoopbackNetwork`; endpoints: CONTRIB send, COORD recv+send (serves after approval), PROC recv+request. One wiremock hub; each ctx `wire_hub` + distinct `store_token_for_test` token.
2. Seed the project in all three caches (`require_approval: true`; members_json: COORD = coordinator send_receive with COORD's recv node, CONTRIB = send with its node, PROC = send_receive with its node — REAL node ids from the loopback endpoints, base64-encoded).
3. CONTRIB: seed 2 gate-passing frames (real FITS + analysis + calibration rows); wiremock `POST /projects/{P}/announcements` returns `{id: "ann-1", state: "pending"}`. `publish_collab_frames` → assert `state == "pending"`, `seed_target == Some(coordinator display)`, and the loopback engine was enqueued toward COORD's node.
4. COORD: its receiver (gate = members contains CONTRIB's node for P) ingests the push-seed — `wait_until` COORD's `project_packages.local_status == 'complete'` (COORD's db needs the package row first: wiremock `GET /projects/{P}/announcements` serves the pending announcement WITH `aggregateStats.manifestXxh3` = the anchor CONTRIB computed — read it from CONTRIB's `project_packages` row after publish; mount AFTER publish). `list_moderation_queue` shows 1 item with 2 frames + metrics.
5. Approve: wiremock `POST /announcements/ann-1/approve` → `{id, state: "published"}`; `decide_announcement(approve)` → COORD local state `published`.
6. PROC: wiremock list now serves the announcement `published` with holders = [COORD's recv node pubkey b64] (remount the list mock — `mount_as_scoped` phases or a stateful Respond impl; keep it simple: unmount/mount sequence). COORD's receiver hooks include a `request_handler` wired like the template (spawns `handle_project_request` with COORD's ctx + a sender runtime). `download_project_package(PROC ctx, PROC sync runtime, P, pkg)` → `wait_until` PROC `local_status == 'complete'`; wiremock `POST /announcements/ann-1/have` `.expect(1..)` (PROC reports).
7. PROC: `export_project_for_wbpp` into a tempdir → assert the WBPP tree: `<out>/<project title>/camera_<instrume>/lights/` contains exactly 2 publisher-prefixed files, byte-identical to CONTRIB's stamped payloads; PROC `files`/`frames` counts still 0 (catalog isolation, §11).
8. Teardown: shutdown engines/receivers.

- [ ] **Step 1:** build the fixture layer (contexts, hub mocks, members_json with real node ids) — compile + a smoke assertion (publish returns pending).
- [ ] **Step 2:** the full scenario through step 7; deflake with `wait_until` on SQL state only (no fixed sleeps).
- [ ] **Step 3:** full gates (the test runs in `cargo test -p athenaeum-core`; keep it under ~30s wall) + commit `test(collab): three-instance E2E — publish → moderation → swarm delivery → project WBPP export`.

## Security requirements (bind Tasks 1-3)

- S1: the web export command validates `output_dir` under `state.export_dir` exactly like the frame-set export (path escape = same rejection); the Tauri side takes the user-picked dir as-is (desktop trust model, matches existing export).
- S2: collector paths come ONLY from local db rows (`landed_path`, `light_calibrations.output_path`) — never from user/hub input; the organizer's skip-existing + warn-on-missing semantics are preserved (no overwrite primitive).
- S3: publisher display names in filenames pass `sanitize_folder_name` (Д2) — no separator/traversal characters survive.
- S4: notification strings render as React text (no markup); project ids in links are route params, not interpolated HTML.
- S5: UI honesty — export success/failure from `ExportCompleteEvent` only; dialog errors inline; no optimistic states.

## Post-plan checklist (verification/ops notes, not tasks)

- The slice-4 live smoke is still OWED (two instances vs test-hub: publish → download → moderation) — slice 5's E2E covers the same flow in-process, but the live smoke exercises real iroh + the real hub; run it after this slice (plus watch observatory-Perseus personal sync after the connect gate).
- Hub follow-up (recorded, out of app scope): no un-have on the wire — stale have-reports for superseded packages persist hub-side.
- Deferred follow-ups from slice 4's final review carry unchanged (M1 connect-gate install-on-sign-in, M3 created_at format mixing, M4 spawn_blocking+tmp-rename for serve copies, dialog a11y sweep, formatBytes/http_util consolidation).

## Self-review notes (applied while writing)

- The export module is verified UNGATED and `RawWithCalibrationSets` is a no-op transform — the project export path needs no render code; only the E2E (which drives publish) is render-gated, expressed as `#[cfg(all(test, feature = "render"))]`.
- `frame_meta` keys are snake_case (`models::Frame` has no serde rename) — the collector parses `filter`/`bayerpat`/`instrume`/`exptime` etc. by those names; `filename`/`file_path` do NOT exist in frame_meta and come from `landed_path` (received) / `output_path` (own).
- `organize_files_wbpp` ignores `keyword_order` today (hard-coded nesting) — the project export inherits that; no new config surface.
- The `-1` sentinel cannot collide with real frame-set ids (AUTOINCREMENT positive); `cancel_export(-1)` rides the existing command.
- `useExportProgress` must be READ before Task 2 wiring — if it resolves a frame-set name anywhere, the -1 entry needs a guard (the task says so).
- Visibility of test seams from the new E2E module is explicitly flagged (reproduce-not-widen).
