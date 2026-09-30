# Collab observability wave 2 — frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the four-tab collab project page with the six-tab, table-driven page of the approved spec
(Overview · My frames · Library · Members · Exchange · Moderation). Make the live exchange visible on the project
page, on Transfers and in the sidebar indicator.

**Architecture:** One generic `ProjectFrameTable` handles every frame list. It is built on pure modules: facets,
grouping, aggregates, sort, selection and windowing live in `table/model.ts`, and the frame view-model, columns and
per-tab configs live in `frames.tsx`. Every row type (`OwnFrameRow`, `ProjectFrameView`, `ModerationFrameView`) is
mapped into one `FrameVM`, so the columns are written once. Live traffic comes from one app-root
`CollabExchangeProvider`. It takes a snapshot from `get_collab_exchange`, merges `collab-exchange-progress` events into
it, and clears on `collab-live-status`. The project Exchange tab, Transfers, the Transfers panel and the sidebar
indicator all read that provider. `ProjectDetail.tsx` shrinks to a shell: header, tabs, drawer and publish
orchestration. Each tab is its own component under `src/components/collab/project/`.

**Tech Stack:** React 18 + TypeScript, Tailwind design tokens, `lucide-react`, vitest + Testing Library. Rust
(`athenaeum-core`, Tauri command, Axum route) for Tasks 1–4 only.

**Spec:** `docs/superpowers/specs/2026-09-29-collab-project-observability-design.md` (§4, §5.5, §6.4, §7.3, §8, §10
frontend, §13 amendments). **Visual reference:** `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
(open it in a browser; its `COLS`, `GROUPS`, `TABLES`, `peerRow`, `liveHtml` and the Overview/Members renderers are
the layout contract). Where the mockup shows ZP, Download, Stop keeping or Ask to exclude, this plan wins
(see Scope rulings).

## Scope rulings (owner, 2026-09-30, two rounds before execution)

- **Publish acts on the selection.** `publish_collab_frames` gains an optional `frameIds` (Task 1, both hosts). With
  no selection, the primary action publishes the whole *filtered view*, and those ids are always sent explicitly.
- **Republish acts on the selection too** (`republish_collab_frames { projectId, frameIds? }`, Task 1). "Recalibrate
  and republish all" stays. **Every republish goes through a guard dialog, so nobody re-announces 100 TB with one
  click** (Task 16): the dialog states the frame count and the total size of what will be regenerated. For "all", or
  a selection above `REPUBLISH_TYPED_CONFIRM_ABOVE = 100` frames, the Republish button stays disabled until the user
  types the exact frame count.
- **Exclude and Restore (coordinator) are built.** The hub already has `PATCH /projects/{id}/frames/{uuid}`
  (`accepted=false` + reason 1..=500 chars / `accepted=true`, cap `data.moderate`). Task 3 adds the core client and
  the `exclude_collab_frame` / `restore_collab_frame` commands on both hosts. UI: an `Exclude` action (reason
  required) in Published and Library for a coordinator, and Exclude/Restore in the drawer.
- **Not built this cycle:**
  - **Ask to exclude** (contributor request) needs a hub mechanism.
  - **Download** and **Stop keeping** per frame need a new core model: a per-frame want/unwant override on top of the
    replication policy, plus replica deletion.

  Neither is rendered, not even disabled. Both are recorded in `docs/superpowers/open-items.md` (Task 18) with a
  short design note.
- **Library actions:** **Keep again** on `not_kept` frames (`keep_collab_frames_again`, existing), plus **Exclude** for
  a coordinator. `CollabAttention` stays above the Library table. It owns changed files, deletion choices and
  last-copy warnings.
- **Moderation actions:** **Approve** (primary, with the existing "Trust this publisher" choice) and **Reject** (one
  reason for all selected), looping the existing per-frame commands.
- **No ZP column anywhere** (spec §13.3).
- **The page no longer calls `evaluate_collab_gate`.** Held back's per-Reason actions work from the group's own rows
  (`OwnFrameRow.failures[].kind`, `setId`, `setName`), which is more precise than project-wide blockers. The frame
  set's Project block still uses the gate (untouched). Spec §5.1's "evaluate_collab_gate stays" still holds for that
  consumer; recorded as an amendment in Task 18.
- **Drawer gate section is rule-by-rule, as the spec says** (`rule · value · needs · ✓/✕`). The verdicts come from the
  core (Task 4 adds `rules[]` to `FrameGateRow`, produced inside `evaluate_frame` itself, one derivation, and carried
  on `OwnFrameRow`). Precondition failures (no analysis, no coordinates, not calibrated…) are listed above the rules.
  **The drawer shows the frame's local path** (Task 4 adds `path` to `OwnFrameRow`).
- **"Received <date> from <member>"** needs data the frontend cannot read today (`list_sync_history` now excludes
  collab landings). Task 2 adds three read-only fields to `ProjectFrameView`.

## Global Constraints

- No `@tauri-apps/*` imports outside `src/api/`; every backend call is `api.invoke` / `api.listen`.
- Tailwind design tokens only (`bg-surface`, `bg-surface-elevated`, `bg-surface-hover`, `text-content`,
  `text-content-secondary`, `text-content-muted`, `border-border`, `bg-accent`, `text-accent`, `bg-accent-muted`,
  `text-success`, `bg-success`, `text-warning`, `bg-warning`, `text-error`, `bg-error`, `bg-info`, `bg-orange`,
  `bg-purple`, and their `/NN` opacity forms). The one exception is the existing filter colour helper
  `getFilterColor` (`src/utils/filterColors.ts`), used as an inline `backgroundColor` for filter dots, as it is
  elsewhere.
- No new npm dependencies. Windowing is hand-written with a fixed row height (29 px incl. border).
- Every `api.listen` uses the StrictMode-safe pattern from CLAUDE.md (cancelled flag, `.then(fn => cancelled ? fn() :
  unlisten = fn)`, `.catch(console.error)`).
- Never swallow errors. Every `catch` logs `console.error('[<area>] <what> failed:', err)` first, then shows inline
  text or a `notify()` with `kind: 'project'`, following the existing page. Notifications only on discrete outcomes,
  never on progress.
- Timestamps via `formatTimestamp` (`src/utils/dateFormatting.ts`), shown as `YYYY-MM-DD HH:MM:SS`.
- Actions act on the eligible part of the selection and say so (`Publish 34 of 50`). With no selection the primary
  action covers the filtered view (`Publish all 214`). Zero eligible means disabled, never hidden.
- Table state (facets, grouping, sort, expansion) goes through `useSessionState`, keyed
  `collab.<projectId>.<tableId>.<part>`. Column visibility goes to `localStorage` key `collab.table.<tableId>.cols`,
  and every read and write is wrapped in try/catch.
- Serde boundary: Rust structs `#[serde(rename_all = "camelCase")]`. Axum `Json` arg structs **must** carry
  `#[serde(rename_all = "camelCase")]` (a missing one was wave 1's Critical finding).
- Rust: two backends in sync (Tauri command + Axum route); `#[tracing::instrument(skip_all, err)]` stays on both;
  TS types regenerate through `ts_export.rs` (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`).
- Commit as the user; message trailers:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_015RRpMcroNShaEgp8Q33fR3`.
- Frontend gate per task: `npx tsc --noEmit` clean + `npx vitest run <the task's test files>` green. Final gate
  (Task 18): `npm test` (the whole vitest suite), `npx tsc --noEmit`, `cargo check --workspace`,
  `cargo test -p athenaeum-core` (all targets; run `collab_live` live tests alone with `--test-threads=2` if they
  flake under load — known machine note).

## Review Focus

1. **Empty project** (no linked sets, or zero rows in a segment). Every tab and segment shows its empty-state
   sentence, not a header-only table, and every action is disabled (`Publish all 0` is disabled). Pinned in Task 10
   and Task 7.
2. **Missing values** (`night` null, `camera` `""`/null, null metrics). They group under "Unknown night" / "Unknown
   camera", sort after real values in both directions, and are ignored by medians. Pinned in Task 5 and Task 6.
3. **Selection outliving its rows.** After a publish, the published rows leave Ready. The selection is pruned to the
   keys that still exist, and the action label never counts vanished rows. Pinned in Task 7.
4. **Exchange events naming a device the snapshot never named.** The row shows the short device id immediately. The
   snapshot is refetched once (not per event) and the name then appears. Pinned in Task 8.
5. **Stale deep links** persisted in notification history (`?tab=receive`, `?tab=contribute`). They land on Library /
   My frames rather than silently falling back to Overview. Pinned in Task 16.

---

## File Structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/api/collab.rs` (modify) | `publish_collab_frames` / `republish_collab_frames` take `frame_ids: Option<&[i64]>`; `run_publish` filters the gate rows to it; `exclude_collab_frame` / `restore_collab_frame`; `OwnFrameRow.rules/path/accepted`. |
| `crates/athenaeum-core/src/collab/hub_client.rs` (modify) | `set_frame_acceptance` (`PATCH /projects/{id}/frames/{uuid}`). |
| `crates/athenaeum-core/src/collab/gate.rs` (modify) | `RuleVerdict`; `FrameGateRow.rules` produced by `evaluate_frame`. |
| `crates/athenaeum-tauri/src/{commands/collab.rs,lib.rs}`, `crates/athenaeum-web/src/routes/{collab.rs,mod.rs}` (modify) | `frameIds` on publish/republish; exclude/restore commands registered on both hosts. |
| `crates/athenaeum-core/src/api/collab_exchange.rs` (modify) | `ProjectFrameView.receivedAt / receivedFromDevice / receivedFromMember`. |
| `src/types/models.ts` (regenerated) | ts-rs output for the two Rust changes. |
| `src/components/collab/format.ts` (modify) | + `formatDuration`, `formatRate`, `formatRelative`. |
| `src/components/collab/project/table/model.ts` (new) | Pure: facets, facet counts, sort, grouping tree, flatten, selection, action labels, windowing, natural orders. |
| `src/components/collab/project/frames.tsx` (new) | `FrameVM` + mappers, column defs, group defs, facet accessors, the five table configs, reason/device labels. |
| `src/components/collab/project/table/ProjectFrameTable.tsx` (new) | The one table component (facet row, group row, totals strip, windowed body, selection). |
| `src/components/collab/exchange/state.ts` (new) | Pure exchange reducer (snapshot/progress/clear, names, rate history, totals). |
| `src/contexts/CollabExchangeContext.tsx` (new) | App-root provider + `useCollabExchange()`. Mounted in `Layout.tsx`. |
| `src/components/collab/project/FrameDrawer.tsx` (new) | Right-side drawer: identity + path, metrics, rule-by-rule gate, exclusion, holders, provenance. |
| `src/components/collab/project/ExcludeDialog.tsx` (new) | Reason-required exclusion of one or more frames (coordinator). |
| `src/components/collab/project/RepublishGuardDialog.tsx` (new) | Scale-stating republish confirm; typed count for "all" or > 100 frames. |
| `src/components/collab/project/ReasonGroupAction.tsx` (new, replaces `GateBlockers.tsx`) | The per-Reason fix button on Held back group headers. |
| `src/components/collab/project/MyFramesTab.tsx` (new) | Segments Ready / Published / Held back + linked objects + solve/analyze/map orchestration. |
| `src/components/collab/project/LibraryTab.tsx` (new, replaces `ReceiveTab.tsx`) | Other members' frames; attention, export, keep-again. |
| `src/components/collab/project/ModerationTab.tsx` (new, replaces `ModerationQueue.tsx`) | Pending frames table; approve/reject in batch. |
| `src/components/collab/project/MembersTab.tsx` (new) | People table from `get_collab_member_summary`. |
| `src/components/collab/project/ExchangeTab.tsx` (new) | Live per-peer rows both directions + receive sessions history. |
| `src/components/collab/project/OverviewTab.tsx` (new) | Integration bars per filter (goals), my numbers, needs attention, exchange now, thresholds. |
| `src/components/collab/project/memberColors.ts` (new) | Stable member → token class mapping. |
| `src/components/collab/project/usePublishing.ts` (new) | Publish/republish/switch orchestration moved out of the page. |
| `src/pages/ProjectDetail.tsx` (rewrite) | Shell: header, tabs + badges, deep links, drawer, publish dialogs. |
| `src/pages/Transfers.tsx`, `src/components/transfers/{types.ts,TransferRow.tsx,TransfersPanel.tsx,TransferIndicator.tsx}`, `src/components/transfers/CollabTrafficGroups.tsx` (new) | Collab groups, sessions in history, panel block, indicator. |
| Deleted | `src/components/collab/{ReceiveTab,ModerationQueue,GateBlockers}.tsx` + their tests (their behaviours move into new tests). |

---

### Task 1: Core — publish and republish a selection (`frameIds`), both hosts

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs` (`publish_collab_frames` ≈3968, `run_publish` ≈4057, tests module ≈9097)
- Modify: `crates/athenaeum-tauri/src/commands/collab.rs:125-137`
- Modify: `crates/athenaeum-web/src/routes/collab.rs:288-310` (publish + republish routes)
- Modify: every other caller of `publish_collab_frames(` / `republish_collab_frames(` (grep the workspace; tests pass `None`)

**Interfaces:**
- Produces: `pub async fn publish_collab_frames(ctx: &ServiceContext, project_id: &str, frame_ids: Option<&[i64]>, emitter: Option<Arc<dyn ProgressEmitter>>) -> Result<PublishResult, ApiError>`. Frontend: `api.invoke<PublishResult>('publish_collab_frames', { projectId, frameIds })`, where `frameIds: number[] | null`.
- Produces: `pub async fn republish_collab_frames(ctx, project_id: &str, frame_ids: Option<&[i64]>, emitter) -> Result<PublishResult, ApiError>`. Frontend: `api.invoke<PublishResult>('republish_collab_frames', { projectId, frameIds })`.
- Semantics: `Some(ids)` restricts the run to gate rows whose `frame_id` is in `ids`. Rows outside the selection
  are neither candidates nor `heldBack`. An id that is not a gate row of the project is ignored. `None` keeps
  today's behaviour. Auto-publish always passes `None`. Republish walks the same gate candidates (`force = true`
  turns an own row into an `Update`), so one filter serves both commands. One consequence must be pinned by a test:
  a republish restricted to published frames never announces an unselected ready frame.

- [ ] **Step 1: Write the failing test** (in the `mod` that holds `publish_writes_once_into_the_collab_folder_and_seeds_by_reference`, same fixture helpers):

```rust
        /// Wave 2 (plan 2026-09-30 Task 1): a selection publishes exactly the
        /// chosen frames; the rest stay unpublished and are not reported as
        /// held back, and a later plain publish picks them up.
        #[tokio::test]
        async fn publish_with_frame_ids_announces_only_the_selected_frames() {
            let fx = fixture(3).await;
            mount_hub(&fx.server, "published").await;

            let pick = [fx.frame_ids[1]];
            let res = publish_collab_frames(&fx.ctx, PID, Some(&pick), None)
                .await
                .unwrap();
            assert_eq!((res.announced, res.updated), (1, 0), "{res:?}");
            assert!(res.held_back.is_empty(), "{:?}", res.held_back);
            let mut names: Vec<String> = std::fs::read_dir(own_dir(&fx))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            assert_eq!(names, vec!["c_L_0001.fits"]);

            let rest = publish_collab_frames(&fx.ctx, PID, None, None).await.unwrap();
            assert_eq!(rest.announced, 2, "{rest:?}");
        }

        #[tokio::test]
        async fn publish_with_an_empty_selection_publishes_nothing() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            let res = publish_collab_frames(&fx.ctx, PID, Some(&[]), None).await.unwrap();
            assert_eq!((res.announced, res.updated), (0, 0), "{res:?}");
            assert!(res.held_back.is_empty(), "{:?}", res.held_back);
        }

        /// A republish of a selection regenerates only the selected published
        /// frame and never announces an unselected, still-ready one.
        #[tokio::test]
        async fn republish_with_frame_ids_touches_only_the_selection() {
            let fx = fixture(3).await;
            mount_hub(&fx.server, "published").await;
            let first = [fx.frame_ids[0], fx.frame_ids[1]];
            publish_collab_frames(&fx.ctx, PID, Some(&first), None).await.unwrap();

            let pick = [fx.frame_ids[0]];
            let res = republish_collab_frames(&fx.ctx, PID, Some(&pick), None)
                .await
                .unwrap();
            assert_eq!(res.announced, 0, "the ready frame 2 must not be announced: {res:?}");
            assert_eq!(res.updated + res.unchanged, 1, "{res:?}");
        }
```

  (`republish_forces_regeneration_but_respects_identical_output` shows how the hub mock answers a version post; mount
  the same mocks here if the update path needs them.)

- [ ] **Step 2: Run it and watch it fail to compile** (arity):
  `cargo test -p athenaeum-core --lib publish_with_frame_ids -- --nocapture` → error `this function takes 3 arguments but 4 were supplied`.

- [ ] **Step 3: Implement.** In `collab.rs`:

```rust
pub async fn publish_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
    frame_ids: Option<&[i64]>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    let lock = publish_lock(ctx, project_id)?;
    let _run = claim_publish(&lock, project_id)?;
    run_publish(ctx, project_id, emitter, false, frame_ids, None).await
}
```

  `republish_collab_frames` gets the same new `frame_ids: Option<&[i64]>` parameter (after `project_id`) and passes it
  through as `run_publish(ctx, project_id, emitter, true, frame_ids, None)`. `auto_publish_collab_frames` passes
  `None`. Change
  `run_publish`'s signature to `(ctx, project_id, emitter, force: bool, only: Option<&[i64]>, after_split:
  AfterSplit<'_>)`, and update the three test call sites (`run_publish(&fx.ctx, PID, None, false, None,
  Some(&hook))`). Right after the gate block (`let (project, gated, binding) = { … };`), insert:

```rust
    // Wave 2 (plan 2026-09-30 Task 1): a selection restricts the run to the
    // chosen gate rows; the rest are neither candidates nor held back.
    let gated: Vec<_> = match only {
        None => gated,
        Some(ids) => {
            let keep: std::collections::HashSet<i64> = ids.iter().copied().collect();
            let before = gated.len();
            let kept: Vec<_> = gated
                .into_iter()
                .filter(|(_, row)| keep.contains(&row.frame_id))
                .collect();
            tracing::info!(
                project_id,
                count = kept.len(),
                selected = ids.len(),
                candidates = before,
                "publish: restricted to a selection"
            );
            kept
        }
    };
```

  (If `selected`/`candidates` are not in the logging field dictionary, use `count` only and put the rest in the
  message-free form the spec allows. Check `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` "Unified
  event schema" and prefer existing names.)

  Tauri (`commands/collab.rs`):

```rust
pub async fn publish_collab_frames(
    state: State<'_, AppState>,
    app: AppHandle,
    project_id: String,
    frame_ids: Option<Vec<i64>>,
) -> Result<PublishResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    api::publish_collab_frames(&state.ctx, &project_id, frame_ids.as_deref(), Some(emitter))
        // …existing tail unchanged
```

  Web (`routes/collab.rs`): add beside `ProjectIdArgs`:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishArgs {
    pub project_id: String,
    #[serde(default)]
    pub frame_ids: Option<Vec<i64>>,
}
```

  and use `Json(args): Json<PublishArgs>` in both the publish and the republish route, passing
  `args.frame_ids.as_deref()`. The Tauri `republish_collab_frames` gains `frame_ids: Option<Vec<i64>>` the same way. Add a web deserialization unit test beside the existing route tests
  (or at the bottom of the file under `#[cfg(test)]`):

```rust
#[test]
fn publish_args_read_camel_case_frame_ids() {
    let a: PublishArgs =
        serde_json::from_str(r#"{"projectId":"p","frameIds":[3,5]}"#).unwrap();
    assert_eq!((a.project_id.as_str(), a.frame_ids), ("p", Some(vec![3, 5])));
    let b: PublishArgs = serde_json::from_str(r#"{"projectId":"p"}"#).unwrap();
    assert_eq!(b.frame_ids, None);
}
```

- [ ] **Step 4: Run.** `cargo test -p athenaeum-core --lib with_frame_ids -- --nocapture` and `… --lib empty_selection` → all PASS.
  `cargo test -p athenaeum-web publish_args` → PASS. `cargo check --workspace` → clean.
- [ ] **Step 5: Commit** `feat(collab): publish and republish a selection of frames (frameIds) on both hosts`.

---

### Task 2: Core — "received from" on `ProjectFrameView`

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (`ProjectFrameView` ≈732 and the function that builds it for `list_collab_frames`)
- Regenerate: `src/types/models.ts` (ts-rs)

**Interfaces:**
- Produces on `ProjectFrameView` (camelCase in TS):
  - `receivedAt: string | null`: `finished_at` of the newest collab landing row for the frame.
  - `receivedFromDevice: string | null`: that row's `peer_device` (base64 device id, or `"local"`).
  - `receivedFromMember: string | null`: the member display name owning that device, per the same device → member
    mapping `get_collab_frame_holders` uses (`collab::snapshot::member_node_ids` over the project's
    `members_json`). `null` for `"local"` and for an unknown device.
- A collab landing row is `sync_history` with `project = ?1 AND package_id IS NULL AND direction = 'received'`
  (spec §7.2). Own frames and never-landed frames get all three `null`.

- [ ] **Step 1: Write the failing test.** Find the existing `list_collab_frames` test that builds a non-own frame
  (search `fn list_collab_frames` in the tests of `collab_exchange.rs` / `collab.rs`) and add a sibling. It must:
  seed a non-own manifest row for frame uuid `U` by member `M` whose `members_json` node list contains device `D`;
  insert via `crate::sync::store::insert_history_row` a row `{frame_uuid: U, peer_device: D, direction: Received,
  project: Some(PID), package_id: None, finished_at: Some("2026-09-30T10:00:00Z"), outcome: "ingested", … }`, plus a
  **personal** row for the same uuid with `package_id: Some("pkg")` and a newer `finished_at` (it must be ignored);
  then assert:

```rust
    let v = rows.iter().find(|r| r.frame_uuid == U).unwrap();
    assert_eq!(v.received_at.as_deref(), Some("2026-09-30T10:00:00Z"));
    assert_eq!(v.received_from_device.as_deref(), Some(D));
    assert_eq!(v.received_from_member.as_deref(), Some(M_NAME));
```

  Add a second case in the same test: a row whose `peer_device = "local"` for another uuid gives
  `(Some(at), Some("local"), None)`. A frame with no landing row gives `(None, None, None)`.

- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib received_from` → fails to compile (fields missing).
- [ ] **Step 3: Implement.** Add the three `pub … : Option<String>` fields (doc comments as above) to
  `ProjectFrameView`. In the builder, before the per-row loop, read one map for the project:

```rust
    // Wave 2 Task 2: the newest collab landing per frame (spec §7.2) — one
    // query per list, never per row.
    let mut landed: HashMap<String, (String, String)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT frame_uuid, finished_at, peer_device FROM sync_history
             WHERE project = ?1 AND package_id IS NULL AND direction = 'received'
               AND frame_uuid IS NOT NULL AND finished_at IS NOT NULL
             ORDER BY finished_at ASC",
        )?;
        let it = stmt.query_map([project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in it {
            let (uuid, at, dev) = row?;
            landed.insert(uuid, (at, dev)); // ASC order: the newest wins
        }
    }
```

  Check the actual `sync_history` column names and the `direction` literal in `db/schema.rs` / `sync/store.rs`
  before writing the SQL, and match them exactly. Build the device → member-name map once from the project's
  `members_json` with the helper `get_collab_frame_holders` already uses. Reuse it; do not write a second parser.
  Fill the three fields for non-own rows only. Errors propagate with `context(...)` like the surrounding code; never
  `unwrap_or_default()` a failed query.
- [ ] **Step 4: Regenerate TS and run.** `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` (updates
  `src/types/models.ts`), then `cargo test -p athenaeum-core --lib received_from` → PASS,
  `cargo test -p athenaeum-core --test ts_contract` → PASS, `npx tsc --noEmit` → fix the two test fixtures that
  build a `ProjectFrameView` literal (`src/components/collab/ReceiveTab.test.tsx`,
  `src/pages/ProjectDetail.test.tsx`) by adding the three fields as `null`.
- [ ] **Step 5: Commit** `feat(collab): library frames carry when and from whom they were received`.

---

### Task 3: Core — Exclude and Restore a frame (coordinator), both hosts

**Files:**
- Modify: `crates/athenaeum-core/src/collab/hub_client.rs` (beside `reject_frame` ≈652)
- Modify: `crates/athenaeum-core/src/api/collab.rs` (beside `reject_collab_frame` ≈5812; tests beside `approve_then_sync_marks_published` ≈8039)
- Modify: `crates/athenaeum-core/src/api/mod.rs` if `api::` re-exports the collab commands by name
- Modify: `crates/athenaeum-tauri/src/commands/collab.rs`, `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler![]`)
- Modify: `crates/athenaeum-web/src/routes/collab.rs`, `crates/athenaeum-web/src/routes/mod.rs` (`build_router`)

**Interfaces:**
- Produces (core): `CollabClient::set_frame_acceptance(&self, token: &str, project_id: &str, frame_uuid: &str, accepted: bool, reason: Option<&str>) -> Result<(), AccountClientError>`, which sends `PATCH /projects/{id}/frames/{uuid}` with body `{"accepted": false, "acceptedReason": "<reason>"}` or `{"accepted": true}` and accepts 200 or 204; `pub async fn exclude_collab_frame(ctx: &ServiceContext, project_id: &str, frame_uuid: &str, reason: String) -> Result<(), ApiError>`; `pub async fn restore_collab_frame(ctx: &ServiceContext, project_id: &str, frame_uuid: &str) -> Result<(), ApiError>`.
- Produces (frontend): `api.invoke('exclude_collab_frame', { projectId, frameUuid, reason })`, `api.invoke('restore_collab_frame', { projectId, frameUuid })`.
- Rules: the reason is trimmed and must be 1..=500 **characters** (`chars().count()`, the hub's own rule, which
  differs from reject's byte rule), validated before any hub call → `ApiError::Invalid("an exclusion reason of 1 to
  500 characters is required")`. Signed out → `ApiError::SignedOut("Sign in to moderate frames.")`. The project must
  be live (`collab_exchange::live_project`). Hub errors map through the existing `client_err` (a 403 surfaces as the
  hub's text). After success: `tracing::info!(project_id, frame_uuid, "excluded frame")` / `"restored frame"`, then a
  best-effort `sync_manifest` exactly like approve/reject (a failure is a `warn!`, not the command's error).

- [ ] **Step 1: Write the failing tests** (wiremock, the `approve_then_sync_marks_published` pattern):

```rust
    #[tokio::test]
    async fn exclude_sends_the_patch_then_syncs_the_manifest() {
        let server = MockServer::start().await;
        Mock::given(wm_method("PATCH"))
            .and(wm_path("/api/v1/projects/p-1/frames/u1"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "accepted": false, "acceptedReason": "wrong target"
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v1/projects/p-1/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projectVersion": 3, "rows": [], "hasMore": false, "next": null
            })))
            .mount(&server)
            .await;
        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
        }
        exclude_collab_frame(&ctx, "p-1", "u1", "  wrong target  ".into()).await.unwrap();
    }

    #[tokio::test]
    async fn restore_sends_accepted_true_without_a_reason() {
        // same shape: body_json({"accepted": true}), 200, expect(1)
    }

    #[tokio::test]
    async fn exclude_refuses_an_empty_or_overlong_reason_before_the_hub() {
        let (_tmp, ctx) = test_ctx();
        let empty = exclude_collab_frame(&ctx, "p-1", "u1", "   ".into()).await.unwrap_err();
        assert!(matches!(empty, ApiError::Invalid(_)), "{empty:?}");
        let long = exclude_collab_frame(&ctx, "p-1", "u1", "é".repeat(501)).await.unwrap_err();
        assert!(matches!(long, ApiError::Invalid(_)), "{long:?}");
        // 500 multi-byte characters are allowed (character rule, not bytes):
        // assert through a mounted hub that "é".repeat(500) reaches the PATCH.
    }
```

  Write the restore test out in full, same shape as the exclude test. For the 500-character case, mount a PATCH mock
  with `.expect(1)` and assert `Ok`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib exclude_ restore_sends` → compile failure.
- [ ] **Step 3: Implement** the client method (mirror `reject_frame`: `self.http.patch(...)`, `bearer_auth`,
  `json`, `classify` on any other status), the two api functions, the two Tauri commands
  (`#[tauri::command] #[tracing::instrument(skip_all, err)] pub async fn exclude_collab_frame(state, project_id:
  String, frame_uuid: String, reason: String)` / `restore_collab_frame(state, project_id, frame_uuid)`,
  `.map_err(|e| e.to_string())` like their neighbours), register both in `invoke_handler![]`, and mirror both as Axum
  routes with `#[derive(Deserialize)] #[serde(rename_all = "camelCase")]` arg structs (`ExcludeArgs { project_id,
  frame_uuid, reason }`, reuse the existing frame-uuid args struct for restore if there is one), registered in
  `build_router` beside `approve_collab_frame`. Add a web deserialization unit test for `ExcludeArgs` reading
  `{"projectId":"p","frameUuid":"u","reason":"r"}`.
- [ ] **Step 4: Run** the three core tests → PASS; `cargo test -p athenaeum-web exclude_args` → PASS;
  `cargo check --workspace` → clean.
- [ ] **Step 5: Commit** `feat(collab): coordinator excludes and restores a frame on both hosts`.

---

### Task 4: Core — rule-by-rule gate verdicts, frame path and acceptance on `OwnFrameRow`

**Files:**
- Modify: `crates/athenaeum-core/src/collab/gate.rs` (`FrameGateRow` ≈123, `evaluate_frame` ≈146, its tests)
- Modify: `crates/athenaeum-core/src/api/collab.rs` (`OwnFrameRow` ≈1531 and `list_project_own_frames`' builder)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `RuleVerdict`)
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Produces:

```rust
/// One threshold rule's verdict for one frame (spec 2026-09-29 §4.1 drawer:
/// rule · value · needs · ✓/✕). Produced inside `evaluate_frame`, in the same
/// loop that writes the failure texts — never a second evaluation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct RuleVerdict {
    pub metric_key: String,
    /// "FWHM", "eccentricity", "stars", "trailed", else the metric key.
    pub label: String,
    /// The frame's value, formatted like the failure text ("3.42″", "500",
    /// "no"/"yes" for trailed); `None` when the input is missing.
    pub value: Option<String>,
    /// "≤ 3.00″", "≥ 200", "not trailed".
    pub needs: String,
    /// `None` = not evaluated (missing input — the precondition failure
    /// already blocks the frame).
    pub pass: Option<bool>,
}
```

  `FrameGateRow` gains `pub rules: Vec<RuleVerdict>` (one entry per rule `evaluate_frame` understood; an unknown
  metric, op or non-numeric value is still skipped with its existing `warn!` and gets no entry). `OwnFrameRow` gains
  `pub rules: Vec<RuleVerdict>` (copied from its gate row), `pub path: Option<String>` (the source light's catalog
  file path, the `files` row the frame belongs to, read in the same batch query that fills `night`/`camera`) and
  `pub accepted: Option<bool>` (announced frames only: the manifest row's `accepted`; `Some(false)` = excluded).
  TS: `RuleVerdict`, `FrameGateRow.rules`, `OwnFrameRow.rules/path/accepted`.
- Formatting follows the failure texts: an `lte` rule value/needs with 2 decimals and the unit (`3.42″`, `≤ 3.00″`),
  a `gte` rule with 0 decimals (`500`, `≥ 200`). `not_trailed` gives value `yes`/`no` and needs `not trailed`.

- [ ] **Step 1: Write the failing tests** in `gate.rs`'s test module (use its existing input/rule builders):

```rust
    #[test]
    fn evaluate_frame_reports_a_verdict_per_rule() {
        let rules = vec![rule("fwhm_arcsec", "lte", json!(3.0)), rule("stars_detected", "gte", json!(200)), rule("not_trailed", "reject_if", json!(true))];
        let row = evaluate_frame(&input_with(/* fwhm 3.42″ after scale, 500 stars, not trailed */), &target(), &rules);
        assert_eq!(row.rules.len(), 3);
        assert_eq!((row.rules[0].label.as_str(), row.rules[0].value.as_deref(), row.rules[0].needs.as_str(), row.rules[0].pass), ("FWHM", Some("3.42″"), "≤ 3.00″", Some(false)));
        assert_eq!((row.rules[1].value.as_deref(), row.rules[1].needs.as_str(), row.rules[1].pass), (Some("500"), "≥ 200", Some(true)));
        assert_eq!((row.rules[2].value.as_deref(), row.rules[2].needs.as_str(), row.rules[2].pass), (Some("no"), "not trailed", Some(true)));
        assert_eq!(row.failures, vec!["FWHM 3.42″ > 3.00″".to_string()]); // texts unchanged
    }

    #[test]
    fn a_rule_without_its_input_is_listed_but_not_evaluated() {
        let rules = vec![rule("fwhm_arcsec", "lte", json!(3.0))];
        let row = evaluate_frame(&input_without_analysis(), &target(), &rules);
        assert_eq!((row.rules[0].value.as_deref(), row.rules[0].pass), (None, None));
    }

    #[test]
    fn an_unknown_metric_gets_no_verdict() {
        let rules = vec![rule("zero_point", "gte", json!(20))];
        assert!(evaluate_frame(&input_with(/* any */), &target(), &rules).rules.is_empty());
    }
```

  Adapt the builder names to the ones the module already has. Add missing tiny helpers in the test module. In
  `collab.rs` tests, extend the existing `list_project_own_frames` test to assert that a ready frame carries
  `path == Some(<the seeded light's path>)` and `rules.len() == <the project's rule count>`, and that an announced own
  row carries `accepted == Some(true)`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --lib collab::gate` → FAIL.
- [ ] **Step 3: Implement.** In `evaluate_frame`, push a `RuleVerdict` in each rule branch where the failure text is
  decided today. `pass = Some(!failed)` when the metric exists, else `None` with `value: None`. Keep every failure
  string byte-identical (other tests pin them). Fix every `FrameGateRow { … }` literal the compiler flags (add
  `rules: vec![]`). Fill the three `OwnFrameRow` fields in `list_project_own_frames`.
- [ ] **Step 4: Regenerate TS and run.** `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then
  `cargo test -p athenaeum-core --lib collab::gate list_project_own_frames` → PASS, `cargo test -p athenaeum-core
  --test ts_contract` → PASS, `npx tsc --noEmit` → fix any TS literal of `FrameGateRow`/`OwnFrameRow` in tests
  (`rules: []`, `path: null`, `accepted: null`).
- [ ] **Step 5: Commit** `feat(collab): gate verdict per rule, frame path and acceptance on own frames`.

---

### Task 5: Pure table model

**Files:**
- Create: `src/components/collab/project/table/model.ts`
- Test: `src/components/collab/project/table/model.test.ts`

**Interfaces (Produces — every later table task relies on these exact names):**

```ts
export type SortDir = 1 | -1;
export interface SortState { col: string; dir: SortDir }
export type NightWindow = 'all' | '1' | '7' | '30';
export const NIGHT_WINDOWS: NightWindow[];
export interface Facets { filters: string[]; camera: string | null; night: NightWindow; publisher: string | null; state: string | null; search: string }
export const EMPTY_FACETS: Facets;
export interface ColumnDef<R> { id: string; label: string; width: number; numeric?: boolean; value: (r: R) => string | number | null; cell: (r: R) => ReactNode; aggregate?: (rows: R[]) => number | null; renderAggregate?: (rows: R[]) => ReactNode }
export interface GroupDef<R> { id: string; label: string; key: (r: R) => string; renderLabel: (key: string) => ReactNode; order: (a: string, b: string) => number }
export interface FacetAccess<R> { name: (r: R) => string; filter: (r: R) => string; camera: (r: R) => string; night: (r: R) => string | null; publisher?: (r: R) => string | null; states?: (r: R) => string[] }
export interface FacetCounts { filters: Map<string, number>; cameras: Map<string, number>; publishers: Map<string, number>; states: Map<string, number>; nights: Record<NightWindow, number> }
export interface GroupNode<R> { id: string; key: string; depth: number; def: GroupDef<R>; rows: R[]; children: GroupNode<R>[] | null }
export type VisibleRow<R> = { kind: 'group'; node: GroupNode<R> } | { kind: 'frame'; row: R; depth: number };
export const ROW_H = 29;
export const FILTER_ORDER: string[];           // L R G B Ha OIII SII OSC
export const BLOCKER_ORDER: string[];          // mirror of core collab/gate.rs BLOCKER_ORDER
export function localToday(now?: Date): string; // YYYY-MM-DD, local
export function nightWithin(night: string | null, w: NightWindow, today: string): boolean;
export function matches<R>(r: R, f: Facets, a: FacetAccess<R>, today: string, except?: keyof Facets): boolean;
export function applyFacets<R>(rows: R[], f: Facets, a: FacetAccess<R>, today: string): R[];
export function facetCounts<R>(rows: R[], f: Facets, a: FacetAccess<R>, today: string): FacetCounts;
export function activeFacetCount(f: Facets): number;
export function median(values: (number | null)[]): number | null;
export function sum(values: (number | null)[]): number;
export function sortRows<R>(rows: R[], col: ColumnDef<R> | undefined, dir: SortDir, name: (r: R) => string): R[];
export function buildTree<R>(rows: R[], levels: GroupDef<R>[], col: ColumnDef<R> | undefined, dir: SortDir, name: (r: R) => string, parentId?: string, depth?: number): GroupNode<R>[];
export function flatten<R>(nodes: GroupNode<R>[], expanded: ReadonlySet<string>): VisibleRow<R>[];
export function initialExpanded<R>(nodes: GroupNode<R>[]): string[];
export function allGroupIds<R>(nodes: GroupNode<R>[]): string[];
export function checkState<R>(rows: R[], selected: ReadonlySet<string>, key: (r: R) => string): 'none' | 'some' | 'all';
export function actionTargets<R>(view: R[], selected: ReadonlySet<string>, key: (r: R) => string, eligible: (r: R) => boolean): { targets: R[]; selectedCount: number };
export function actionLabel(verb: string, eligible: number, selectedCount: number): string;
export function windowSlice(scrollTop: number, viewportH: number, total: number, rowH?: number, overscan?: number): { start: number; end: number; padTop: number; padBottom: number };
export function filterOrder(a: string, b: string): number;
export function nightOrderDesc(a: string, b: string): number;
export function reasonOrder(a: string, b: string): number;
export function alphaOrder(a: string, b: string): number;
```

- [ ] **Step 1: Write the failing tests** (`model.test.ts`):

```ts
import { describe, expect, it } from 'vitest';
import {
  EMPTY_FACETS, actionLabel, actionTargets, applyFacets, buildTree, checkState, facetCounts, filterOrder,
  flatten, initialExpanded, median, nightWithin, reasonOrder, sortRows, sum, windowSlice,
  type ColumnDef, type FacetAccess, type GroupDef,
} from './model';

interface Row { id: string; name: string; filter: string; camera: string; night: string | null; fwhm: number | null; exp: number; states: string[] }
const r = (id: string, o: Partial<Row> = {}): Row => ({ id, name: `f${id}.fits`, filter: 'L', camera: 'CamA', night: '2026-09-29', fwhm: 2, exp: 300, states: [], ...o });
const A: FacetAccess<Row> = { name: (x) => x.name, filter: (x) => x.filter, camera: (x) => x.camera, night: (x) => x.night, states: (x) => x.states };
const fwhm: ColumnDef<Row> = { id: 'fwhm', label: 'FWHM″', width: 60, numeric: true, value: (x) => x.fwhm, cell: () => null, aggregate: (rs) => median(rs.map((x) => x.fwhm)) };
const exp: ColumnDef<Row> = { id: 'exp', label: 'Exp / Σ', width: 60, numeric: true, value: (x) => x.exp, cell: () => null, aggregate: (rs) => sum(rs.map((x) => x.exp)) };
const byFilter: GroupDef<Row> = { id: 'filter', label: 'Filter', key: (x) => x.filter, renderLabel: (k) => k, order: filterOrder };
const byNight: GroupDef<Row> = { id: 'night', label: 'Night', key: (x) => x.night ?? '', renderLabel: (k) => k, order: (a, b) => (a < b ? 1 : a > b ? -1 : 0) };
const TODAY = '2026-09-30';

describe('nightWithin', () => {
  it('last night includes today and yesterday, not two days ago', () => {
    expect(nightWithin('2026-09-30', '1', TODAY)).toBe(true);
    expect(nightWithin('2026-09-29', '1', TODAY)).toBe(true);
    expect(nightWithin('2026-09-28', '1', TODAY)).toBe(false);
  });
  it('a row with no night only matches "all"', () => {
    expect(nightWithin(null, 'all', TODAY)).toBe(true);
    expect(nightWithin(null, '30', TODAY)).toBe(false);
  });
});

describe('facets', () => {
  const rows = [r('1', { filter: 'L' }), r('2', { filter: 'Ha' }), r('3', { filter: 'Ha', camera: 'CamB' })];
  it('filters on every active facet', () => {
    expect(applyFacets(rows, { ...EMPTY_FACETS, filters: ['Ha'], camera: 'CamB' }, A, TODAY).map((x) => x.id)).toEqual(['3']);
  });
  it('each facet count ignores its own facet but honours the others', () => {
    const c = facetCounts(rows, { ...EMPTY_FACETS, filters: ['Ha'], camera: 'CamB' }, A, TODAY);
    expect(c.filters.get('L')).toBeUndefined(); // camera CamB excludes row 1
    expect(c.filters.get('Ha')).toBe(1);
    expect(c.cameras.get('CamA')).toBe(1); // filter Ha, camera facet ignored
    expect(c.cameras.get('CamB')).toBe(1);
  });
  it('search is case-insensitive on the file name', () => {
    expect(applyFacets(rows, { ...EMPTY_FACETS, search: 'F2' }, A, TODAY).map((x) => x.id)).toEqual(['2']);
  });
  it('a state facet matches any of the row states', () => {
    const s = [r('1', { states: ['solve', 'threshold'] }), r('2', { states: ['threshold'] })];
    expect(applyFacets(s, { ...EMPTY_FACETS, state: 'solve' }, A, TODAY).map((x) => x.id)).toEqual(['1']);
  });
});

describe('median / sum', () => {
  it('ignore nulls and non-finite values', () => {
    expect(median([3, null, 1, 2])).toBe(2);
    expect(median([1, 2, 3, 4])).toBe(2.5);
    expect(median([null])).toBeNull();
    expect(sum([1, null, 2])).toBe(3);
  });
});

describe('sortRows', () => {
  it('nulls sort last in both directions, ties break on name', () => {
    const rows = [r('b', { fwhm: null }), r('a', { fwhm: 3 }), r('c', { fwhm: 1 }), r('d', { fwhm: 3 })];
    expect(sortRows(rows, fwhm, 1, (x) => x.name).map((x) => x.id)).toEqual(['c', 'a', 'd', 'b']);
    expect(sortRows(rows, fwhm, -1, (x) => x.name).map((x) => x.id)).toEqual(['a', 'd', 'c', 'b']);
  });
});

describe('buildTree / flatten', () => {
  const rows = [
    r('1', { night: '2026-09-28', filter: 'Ha', exp: 300 }),
    r('2', { night: '2026-09-29', filter: 'L', exp: 60 }),
    r('3', { night: '2026-09-29', filter: 'Ha', exp: 600 }),
  ];
  it('groups by level, natural order when the sort column has no aggregate', () => {
    const t = buildTree(rows, [byNight, byFilter], undefined, 1, (x) => x.name);
    expect(t.map((n) => n.key)).toEqual(['2026-09-29', '2026-09-28']); // newest night first
    expect(t[0].children!.map((n) => n.key)).toEqual(['L', 'Ha']); // L before Ha
  });
  it('groups sort by the sort column aggregate when it has one', () => {
    const t = buildTree(rows, [byFilter], exp, -1, (x) => x.name);
    expect(t.map((n) => n.key)).toEqual(['Ha', 'L']); // Σ 900 > 60
  });
  it('flatten shows only expanded groups; initialExpanded opens the first group and its first child', () => {
    const t = buildTree(rows, [byNight, byFilter], undefined, 1, (x) => x.name);
    const open = new Set(initialExpanded(t));
    const v = flatten(t, open);
    expect(v.map((x) => (x.kind === 'group' ? `g:${x.node.key}` : `f:${x.row.id}`))).toEqual([
      'g:2026-09-29', 'g:L', 'f:2', 'g:Ha', 'g:2026-09-28',
    ]);
  });
});

describe('selection and actions', () => {
  const view = [r('1'), r('2'), r('3')];
  const key = (x: Row) => x.id;
  it('tri-state group checkbox', () => {
    expect(checkState(view, new Set(), key)).toBe('none');
    expect(checkState(view, new Set(['1']), key)).toBe('some');
    expect(checkState(view, new Set(['1', '2', '3']), key)).toBe('all');
  });
  it('targets are the eligible part of the selection inside the view, or of the whole view with no selection', () => {
    const elig = (x: Row) => x.id !== '2';
    expect(actionTargets(view, new Set(['1', '2', 'gone']), key, elig)).toEqual({ targets: [view[0]], selectedCount: 2 });
    expect(actionTargets(view, new Set(), key, elig)).toEqual({ targets: [view[0], view[2]], selectedCount: 0 });
  });
  it('labels say what they act on', () => {
    expect(actionLabel('Publish', 34, 50)).toBe('Publish 34 of 50');
    expect(actionLabel('Publish', 50, 50)).toBe('Publish 50');
    expect(actionLabel('Publish', 214, 0)).toBe('Publish all 214');
  });
});

describe('windowSlice', () => {
  it('slices around the viewport with overscan and pads the rest', () => {
    expect(windowSlice(29 * 100, 29 * 20, 5000, 29, 10)).toEqual({ start: 90, end: 130, padTop: 90 * 29, padBottom: (5000 - 130) * 29 });
  });
  it('an unmeasured viewport (jsdom) renders the first 60 rows', () => {
    expect(windowSlice(0, 0, 5000)).toMatchObject({ start: 0, end: 60 });
    expect(windowSlice(0, 0, 3)).toMatchObject({ start: 0, end: 3, padBottom: 0 });
  });
});

describe('orders', () => {
  it('filters follow L R G B Ha OIII SII OSC, unknown after, alphabetical', () => {
    expect(['Zz', 'Ha', 'L', 'OSC', 'Aa'].sort(filterOrder)).toEqual(['L', 'Ha', 'OSC', 'Aa', 'Zz']);
  });
  it('reasons follow the core BLOCKER_ORDER', () => {
    expect(['threshold', 'solve', 'analyze'].sort(reasonOrder)).toEqual(['analyze', 'solve', 'threshold']);
  });
});
```

- [ ] **Step 2: Run** `npx vitest run src/components/collab/project/table/model.test.ts` → FAIL (module not found).
- [ ] **Step 3: Implement** `model.ts`:

```ts
import type { ReactNode } from 'react';

/* ── Types ──────────────────────────────────────────────────────────────── */

export type SortDir = 1 | -1;
export interface SortState { col: string; dir: SortDir }
export type NightWindow = 'all' | '1' | '7' | '30';
export const NIGHT_WINDOWS: NightWindow[] = ['all', '1', '7', '30'];

/** One table's facet selection (spec §4.1 row 1). Empty/null = inactive. */
export interface Facets {
  filters: string[];
  camera: string | null;
  night: NightWindow;
  publisher: string | null;
  state: string | null;
  search: string;
}
export const EMPTY_FACETS: Facets = { filters: [], camera: null, night: 'all', publisher: null, state: null, search: '' };

export interface ColumnDef<R> {
  id: string;
  /** Header text, units included (`FWHM″`, `Exp / Σ`). */
  label: string;
  width: number;
  numeric?: boolean;
  /** Sort key; `null` sorts last in both directions. */
  value: (r: R) => string | number | null;
  cell: (r: R) => ReactNode;
  /** Numeric group aggregate: groups sort by it when this column is the sort column. */
  aggregate?: (rows: R[]) => number | null;
  /** What the group row shows under this column (Σ exposure, x̃ FWHM, status breakdown…). */
  renderAggregate?: (rows: R[]) => ReactNode;
}

export interface GroupDef<R> {
  id: string;
  label: string;
  key: (r: R) => string;
  renderLabel: (key: string) => ReactNode;
  /** Natural order of group keys (night newest first, filters L R G B…, reasons by BLOCKER_ORDER). */
  order: (a: string, b: string) => number;
}

export interface FacetAccess<R> {
  name: (r: R) => string;
  filter: (r: R) => string;
  camera: (r: R) => string;
  night: (r: R) => string | null;
  publisher?: (r: R) => string | null;
  /** Every state-facet value the row matches (a held-back frame matches each of its failure kinds). */
  states?: (r: R) => string[];
}

export interface FacetCounts {
  filters: Map<string, number>;
  cameras: Map<string, number>;
  publishers: Map<string, number>;
  states: Map<string, number>;
  nights: Record<NightWindow, number>;
}

export interface GroupNode<R> {
  /** Path id, unique in the tree: `/night:2026-09-29/filter:Ha`. Expansion state keys on it. */
  id: string;
  key: string;
  depth: number;
  def: GroupDef<R>;
  /** Every frame under this node, sorted. */
  rows: R[];
  /** Sub-groups, or `null` at the last level (the frames are then `rows`). */
  children: GroupNode<R>[] | null;
}

export type VisibleRow<R> = { kind: 'group'; node: GroupNode<R> } | { kind: 'frame'; row: R; depth: number };

export const ROW_H = 29;

/* ── Natural orders ─────────────────────────────────────────────────────── */

export const FILTER_ORDER = ['L', 'R', 'G', 'B', 'Ha', 'OIII', 'SII', 'OSC'];
/** Mirror of `BLOCKER_ORDER` in `crates/athenaeum-core/src/collab/gate.rs` — keep in lockstep. */
export const BLOCKER_ORDER = ['analyze', 'solve', 'linkCalibration', 'buildMasters', 'attest', 'mapFilter', 'threshold', 'uuid', 'outsideTarget'];

function rankOrder(list: string[]) {
  return (a: string, b: string): number => {
    const x = list.indexOf(a);
    const y = list.indexOf(b);
    if (x !== -1 && y !== -1) return x - y;
    if (x !== -1) return -1;
    if (y !== -1) return 1;
    return a.localeCompare(b);
  };
}
export const filterOrder = rankOrder(FILTER_ORDER);
export const reasonOrder = rankOrder(BLOCKER_ORDER);
export const alphaOrder = (a: string, b: string): number => a.localeCompare(b);
/** Newest night first; the empty key ("Unknown night") last. */
export const nightOrderDesc = (a: string, b: string): number => {
  if (a === b) return 0;
  if (a === '') return 1;
  if (b === '') return -1;
  return a < b ? 1 : -1;
};

/* ── Facets ─────────────────────────────────────────────────────────────── */

export function localToday(now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

function daysBetween(today: string, night: string): number {
  return Math.round((Date.parse(`${today}T00:00:00Z`) - Date.parse(`${night}T00:00:00Z`)) / 86_400_000);
}

/** A catalog night is labelled by its evening date, so "last night" is today or yesterday. */
export function nightWithin(night: string | null, w: NightWindow, today: string): boolean {
  if (w === 'all') return true;
  if (!night) return false;
  const d = daysBetween(today, night);
  return d >= 0 && d <= Number(w);
}

export function matches<R>(r: R, f: Facets, a: FacetAccess<R>, today: string, except?: keyof Facets): boolean {
  if (except !== 'filters' && f.filters.length > 0 && !f.filters.includes(a.filter(r))) return false;
  if (except !== 'camera' && f.camera !== null && a.camera(r) !== f.camera) return false;
  if (except !== 'night' && !nightWithin(a.night(r), f.night, today)) return false;
  if (except !== 'publisher' && f.publisher !== null && a.publisher && a.publisher(r) !== f.publisher) return false;
  if (except !== 'state' && f.state !== null && a.states && !a.states(r).includes(f.state)) return false;
  const q = f.search.trim().toLowerCase();
  if (except !== 'search' && q !== '' && !a.name(r).toLowerCase().includes(q)) return false;
  return true;
}

export function applyFacets<R>(rows: R[], f: Facets, a: FacetAccess<R>, today: string): R[] {
  return rows.filter((r) => matches(r, f, a, today));
}

function inc(m: Map<string, number>, k: string) {
  m.set(k, (m.get(k) ?? 0) + 1);
}

/** Every count is computed against all OTHER active facets (spec §4.1). */
export function facetCounts<R>(rows: R[], f: Facets, a: FacetAccess<R>, today: string): FacetCounts {
  const out: FacetCounts = {
    filters: new Map(), cameras: new Map(), publishers: new Map(), states: new Map(),
    nights: { all: 0, '1': 0, '7': 0, '30': 0 },
  };
  for (const r of rows) {
    if (matches(r, f, a, today, 'filters')) inc(out.filters, a.filter(r));
    if (matches(r, f, a, today, 'camera')) inc(out.cameras, a.camera(r));
    if (a.publisher && matches(r, f, a, today, 'publisher')) {
      const p = a.publisher(r);
      if (p !== null) inc(out.publishers, p);
    }
    if (a.states && matches(r, f, a, today, 'state')) for (const s of new Set(a.states(r))) inc(out.states, s);
    if (matches(r, f, a, today, 'night')) for (const w of NIGHT_WINDOWS) if (nightWithin(a.night(r), w, today)) out.nights[w] += 1;
  }
  return out;
}

export function activeFacetCount(f: Facets): number {
  return (f.filters.length ? 1 : 0) + (f.camera !== null ? 1 : 0) + (f.night !== 'all' ? 1 : 0) +
    (f.publisher !== null ? 1 : 0) + (f.state !== null ? 1 : 0) + (f.search.trim() ? 1 : 0);
}

/* ── Aggregates ─────────────────────────────────────────────────────────── */

export function median(values: (number | null)[]): number | null {
  const v = values.filter((x): x is number => x !== null && Number.isFinite(x)).sort((a, b) => a - b);
  if (v.length === 0) return null;
  const m = v.length >> 1;
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
}

export function sum(values: (number | null)[]): number {
  let s = 0;
  for (const x of values) if (x !== null && Number.isFinite(x)) s += x;
  return s;
}

/* ── Sort and grouping ──────────────────────────────────────────────────── */

export function sortRows<R>(rows: R[], col: ColumnDef<R> | undefined, dir: SortDir, name: (r: R) => string): R[] {
  return [...rows].sort((x, y) => {
    if (col) {
      const a = col.value(x);
      const b = col.value(y);
      if (a === null || b === null) {
        if (a !== b) return a === null ? 1 : -1;
      } else if (a !== b) {
        const c = typeof a === 'number' && typeof b === 'number' ? a - b : String(a).localeCompare(String(b));
        if (c !== 0) return c * dir;
      }
    }
    return name(x).localeCompare(name(y));
  });
}

export function buildTree<R>(
  rows: R[], levels: GroupDef<R>[], col: ColumnDef<R> | undefined, dir: SortDir, name: (r: R) => string,
  parentId = '', depth = 0,
): GroupNode<R>[] {
  const [def, ...rest] = levels;
  if (!def) return [];
  const byKey = new Map<string, R[]>();
  for (const r of rows) {
    const k = def.key(r);
    const list = byKey.get(k);
    if (list) list.push(r);
    else byKey.set(k, [r]);
  }
  const nodes: GroupNode<R>[] = [...byKey].map(([key, rs]) => {
    const id = `${parentId}/${def.id}:${key}`;
    return {
      id, key, depth, def,
      rows: sortRows(rs, col, dir, name),
      children: rest.length ? buildTree(rs, rest, col, dir, name, id, depth + 1) : null,
    };
  });
  const agg = col?.aggregate;
  nodes.sort((a, b) => {
    if (agg) {
      const x = agg(a.rows);
      const y = agg(b.rows);
      if (x !== null && y !== null && x !== y) return (x - y) * dir;
      if ((x === null) !== (y === null)) return x === null ? 1 : -1;
    }
    return def.order(a.key, b.key);
  });
  return nodes;
}

export function flatten<R>(nodes: GroupNode<R>[], expanded: ReadonlySet<string>, out: VisibleRow<R>[] = []): VisibleRow<R>[] {
  for (const n of nodes) {
    out.push({ kind: 'group', node: n });
    if (!expanded.has(n.id)) continue;
    if (n.children) flatten(n.children, expanded, out);
    else for (const row of n.rows) out.push({ kind: 'frame', row, depth: n.depth + 1 });
  }
  return out;
}

export function initialExpanded<R>(nodes: GroupNode<R>[]): string[] {
  const first = nodes[0];
  if (!first) return [];
  return first.children?.[0] ? [first.id, first.children[0].id] : [first.id];
}

export function allGroupIds<R>(nodes: GroupNode<R>[]): string[] {
  const ids: string[] = [];
  const walk = (ns: GroupNode<R>[]) => ns.forEach((n) => { ids.push(n.id); if (n.children) walk(n.children); });
  walk(nodes);
  return ids;
}

/* ── Selection and actions ──────────────────────────────────────────────── */

export function checkState<R>(rows: R[], selected: ReadonlySet<string>, key: (r: R) => string): 'none' | 'some' | 'all' {
  let n = 0;
  for (const r of rows) if (selected.has(key(r))) n += 1;
  return n === 0 ? 'none' : n === rows.length ? 'all' : 'some';
}

/** An action's frames: the eligible part of the selection that is still in the
 *  filtered view, or of the whole view when nothing is selected. */
export function actionTargets<R>(view: R[], selected: ReadonlySet<string>, key: (r: R) => string, eligible: (r: R) => boolean): { targets: R[]; selectedCount: number } {
  if (selected.size === 0) return { targets: view.filter(eligible), selectedCount: 0 };
  const sel = view.filter((r) => selected.has(key(r)));
  return { targets: sel.filter(eligible), selectedCount: sel.length };
}

export function actionLabel(verb: string, eligible: number, selectedCount: number): string {
  if (selectedCount === 0) return `${verb} all ${eligible}`;
  return eligible === selectedCount ? `${verb} ${eligible}` : `${verb} ${eligible} of ${selectedCount}`;
}

/* ── Windowing ──────────────────────────────────────────────────────────── */

export function windowSlice(scrollTop: number, viewportH: number, total: number, rowH = ROW_H, overscan = 10) {
  if (viewportH <= 0) {
    const end = Math.min(total, 60);
    return { start: 0, end, padTop: 0, padBottom: (total - end) * rowH };
  }
  const start = Math.max(0, Math.floor(scrollTop / rowH) - overscan);
  const end = Math.min(total, Math.ceil((scrollTop + viewportH) / rowH) + overscan);
  return { start, end, padTop: start * rowH, padBottom: (total - end) * rowH };
}
```

- [ ] **Step 4: Run** the test file → PASS; `npx tsc --noEmit` → clean.
- [ ] **Step 5: Commit** `feat(collab): pure table model for the project frame tables`.

---

### Task 6: Frame view-model, columns, groups, table configs + formatters

**Files:**
- Modify: `src/components/collab/format.ts` (+ `formatDuration`, `formatRate`, `formatRelative`)
- Create: `src/components/collab/project/frames.tsx`
- Test: `src/components/collab/project/frames.test.tsx`, `src/components/collab/format.test.ts` (create if absent)

**Interfaces:**
- Consumes: Task 5 types; `OwnFrameRow`, `ProjectFrameView`, `ModerationFrameView`, `InFlightView` from `models.ts`.
- Produces:

```ts
export type TableId = 'ready' | 'held' | 'published' | 'library' | 'moderation';
export type DeviceState = 'have' | 'downloading' | 'queued' | 'missing' | 'notKept' | 'needsChoice' | 'changed' | 'notReplicated';
export interface FrameVM {
  key: string;                    // frameUuid, else `id:<frameId>`
  frameId: number | null;
  frameUuid: string | null;
  setId: number | null;
  setName: string | null;
  fileName: string;
  night: string | null;
  filter: string;
  filterMapped: boolean;
  camera: string;                 // '' = unknown
  publisher: string | null;       // display name
  publisherAccountId: string | null;
  exptimeSec: number | null;
  byteSize: number | null;
  fwhm: number | null; ecc: number | null; stars: number | null; snr: number | null;
  failures: { kind: string; text: string }[];
  contentVersion: number | null;
  pubState: string | null;        // pending | published | rejected
  excluded: boolean;              // accepted === false (coordinator exclusion)
  acceptedReason: string | null;
  holdersOnline: number | null;
  holdersTotal: number | null;    // OTHER member devices (core doc) — see copies()
  disk: 'on' | 'missing' | 'changed' | null;   // own published only
  publishedAt: string | null;
  device: DeviceState | null;     // library only
  missingWhy: 'holder offline' | 'publisher offline' | null;
  progress: number | null;        // 0..100 while downloading
  submittedAt: string | null;     // moderation only
  states: string[];               // this table's state-facet values
  own: OwnFrameRow | null;
  lib: ProjectFrameView | null;
  mod: ModerationFrameView | null;
}
export function fromOwn(r: OwnFrameRow): FrameVM;
export function fromLibrary(r: ProjectFrameView, inFlight: ReadonlyMap<string, { done: number; size: number }>): FrameVM;
export function fromModeration(m: ModerationFrameView, byUuid: ReadonlyMap<string, ProjectFrameView>): FrameVM;
export function copies(vm: FrameVM): number | null;      // holdersTotal + (this device holds it ? 1 : 0)
export const REASON_LABEL: Record<string, string>;
export const DEVICE_LABEL: Record<DeviceState, string>;
export const COLUMNS: Record<string, ColumnDef<FrameVM>>;
export const GROUPS: Record<string, GroupDef<FrameVM>>;
export const FRAME_ACCESS: FacetAccess<FrameVM>;
export interface TableConfig { id: TableId; columns: string[]; defaultColumns: string[]; groupings: string[]; defaultGrouping: [string, string]; stateFacet: { label: string; options: [string, string][] } | null; publisherFacet: boolean }
export const TABLES: Record<TableId, TableConfig>;
// format.ts
export function formatDuration(seconds: number): string;   // 0 → '0m', 5400 → '1h 30m', 45 → '45s', 360000 → '100h'
export function formatRate(bps: number): string;           // bytes/s → '31.0 MB/s', '950 KB/s', '0 B/s'
export function formatRelative(iso: string, now: number): string; // 'just now' | '5 min ago' | '3 h ago' | '3 days ago'
```

Rules the mappers implement (and the tests pin):
- `excluded`: own → `accepted === false`; library → `!accepted`; moderation → `false`. The Status column and the
  `status` group read `excluded` (reason on the chip's `title`) before `pubState`. Every table's name cell appends a
  muted `excluded` chip to an excluded frame.
- `fromOwn`: `disk` from `localState`: `own_held` → `on`, `own_missing` → `missing`, `own_changed` → `changed`, else
  `null`. `states` by segment: held → the distinct `failures[].kind` values; published → `[excluded ? 'excluded' : pubState ?? 'published']`
  plus `'single'` when `copies === 1` plus `'disk'` when `disk` is `missing`/`changed`; ready → `[]`. `camera` null →
  `''`.
- `fromLibrary`: `device` from `localState`: `held` → `have`; `wanted` → `downloading` if the frame uuid is in
  `inFlight`, else `missing` when `waitingForPublisher` or `holdersOnline === 0`, else `queued`; `missing` →
  `missing`; `not_kept` → `notKept`; `awaiting_choice` → `needsChoice`; `quarantined` → `changed`; `idle` →
  `notReplicated`; any `own_*` → `null`. `missingWhy`: `waitingForPublisher` → `'publisher offline'`, else
  `holdersOnline === 0` → `'holder offline'`, else `null` (only when `device === 'missing'`). `progress` =
  `round(done / size * 100)` clamped to 0..100 when downloading. `states = [device]`.
- `copies`: own published → `holdersTotal + (disk === 'on' ? 1 : 0)`; library → `holdersTotal + (device === 'have' ?
  1 : 0)`; `null` when `holdersTotal` is null.
- `fromModeration`: the pending view has only filter/exp/FWHM/created. It fills
  `night/camera/ecc/stars/snr/byteSize` from `byUuid.get(frameUuid)` when the manifest mirror has the row, else
  `null`/`''`. `submittedAt = createdAt`, `publisher`, `publisherAccountId` from the moderation row.

Columns (ids → label, width, numeric; `cell` / `renderAggregate` follow the mockup's `COLS`, ZP removed). Numbers use
`tabular-nums`; a null value renders `—` in `text-content-muted`:

| id | label | agg (group row) |
| ---- | ---- | ---- |
| `name` | Frame (w 260) | — (group label occupies it) |
| `publisher` | Publisher (108) | one publisher → name, else `N members` |
| `night` | Night (98) | one → the date, else `N nights` |
| `filter` | Filter (70) | dot + name, or `N filters` |
| `camera` | Camera (122) | one → name (`Unknown camera` for `''`), else `N cameras` |
| `exp` | Exp / Σ (82, num) | Σ seconds via `formatDuration`; aggregate = sum |
| `fwhm` | FWHM″ (66, num, 2 dp) | `x̃ 2.41`; aggregate = median |
| `ecc` | Ecc (60, num, 2 dp) | `x̃ 0.42`; median |
| `stars` | Stars (64, num, en-US grouping) | `x̃ 1,204`; median |
| `snr` | SNR (58, num, 1 dp) | `x̃ 18.3`; median |
| `size` | Size (78, num, `formatBytes`) | Σ; sum |
| `reason` | Why held back (230) | first failure as chip (`text-warning`, or `text-error` for `threshold`) + `+N`; agg: `N causes` when mixed |
| `version` | Ver (44, num) | `v3` |
| `status` | Status (112) | chip published/pending/rejected; agg: breakdown chips of non-published, else `N published` |
| `holders` | Holders (88, num) | `copies === 1` → chip `1 copy` (`text-warning`), else `online on / total+own`; agg: `N single` chip or `min N` |
| `disk` | On disk (84) | `yes` muted / chip `missing` (error) / chip `changed` (warning); agg: count of not-on |
| `publishedAt` | Published (142) | `formatTimestamp`, muted |
| `device` | On this device (168) | have ● success / downloading bar + `NN%` / `○ queued` / chip `missing` + reason / `not kept` muted / `needs your choice` warning / `changed` warning / `not replicated` muted; agg: stacked bar (have success, downloading accent, queued accent-muted, missing error, not kept border) + text (`N missing` / `N to go` / `have/total`) |
| `submitted` | Submitted (142) | `formatTimestamp` |

Groups (`GROUPS`): `night` (key `night ?? ''`, label = date + weekday `2026-09-29 · Tue`, `''` → `Unknown night`, order
`nightOrderDesc`), `filter` (dot + name, `filterOrder`), `camera` (`''` → `Unknown camera`, alpha), `object` (key
`setName ?? ''`, `''` → `No object`, alpha), `reason` (key `failures[0]?.kind ?? ''`, label `REASON_LABEL[k]`,
`reasonOrder`), `status` (key `excluded ? 'excluded' : pubState ?? 'published'`, alpha), `publisher` (key `publisher ?? ''`, alpha), `none`
(label `None`; never passed to `buildTree`: it means "no level").

`REASON_LABEL`: analyze "No analysis", solve "No coordinates or pixel scale", linkCalibration "Not calibrated — no
calibration linked", buildMasters "Not calibrated — masters not built", attest "Not calibrated", mapFilter "Filter
needs a mapping", threshold "Quality thresholds", uuid "No frame uuid — re-scan the folder", outsideTarget "Outside
the target".

`TABLES` (spec §4.2 minus ZP):

```ts
export const TABLES: Record<TableId, TableConfig> = {
  ready: { id: 'ready', columns: ['name', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'size'], defaultColumns: ['name', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'size'], groupings: ['night', 'filter', 'camera', 'object', 'none'], defaultGrouping: ['night', 'filter'], stateFacet: null, publisherFacet: false },
  held: { id: 'held', columns: ['name', 'reason', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'size'], defaultColumns: ['name', 'reason', 'night', 'filter', 'exp', 'fwhm', 'ecc', 'stars'], groupings: ['reason', 'night', 'filter', 'camera', 'object', 'none'], defaultGrouping: ['reason', 'night'], stateFacet: { label: 'Reason', options: BLOCKER_ORDER.map((k) => [k, REASON_LABEL[k]]) }, publisherFacet: false },
  published: { id: 'published', columns: ['name', 'night', 'filter', 'camera', 'exp', 'version', 'status', 'holders', 'disk', 'publishedAt', 'fwhm', 'ecc', 'size'], defaultColumns: ['name', 'filter', 'exp', 'version', 'status', 'holders', 'disk', 'publishedAt', 'size'], groupings: ['night', 'filter', 'status', 'camera', 'none'], defaultGrouping: ['night', 'filter'], stateFacet: { label: 'Status', options: [['published', 'Published'], ['pending', 'Pending approval'], ['rejected', 'Rejected'], ['excluded', 'Excluded'], ['single', 'Only one copy'], ['disk', 'Not on disk / changed']] }, publisherFacet: false },
  library: { id: 'library', columns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'holders', 'device', 'size'], defaultColumns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'holders', 'device', 'size'], groupings: ['publisher', 'filter', 'camera', 'night', 'none'], defaultGrouping: ['publisher', 'filter'], stateFacet: { label: 'On this device', options: (['have', 'downloading', 'queued', 'missing', 'notKept', 'needsChoice', 'changed', 'notReplicated'] as DeviceState[]).map((k) => [k, DEVICE_LABEL[k]]) }, publisherFacet: true },
  moderation: { id: 'moderation', columns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr', 'size', 'submitted'], defaultColumns: ['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'fwhm', 'ecc', 'stars', 'snr'], groupings: ['publisher', 'night', 'filter', 'camera', 'none'], defaultGrouping: ['publisher', 'night'], stateFacet: null, publisherFacet: true },
};
```

- [ ] **Step 1: Write the failing tests.** `format.test.ts`: `formatDuration(0) === '0m'`, `(45) === '45s'`,
  `(5400) === '1h 30m'`, `(360000) === '100h'`; `formatRate(0) === '0 B/s'`, `(972800) === '950 KB/s'`,
  `(32505856) === '31.0 MB/s'`; `formatRelative` with a fixed `now`: 10 s → `just now`, 5 min → `5 min ago`, 3 h →
  `3 h ago`, 3 days → `3 days ago`, 1 day → `1 day ago`. `frames.test.tsx` (build fixtures with full
  `OwnFrameRow`/`ProjectFrameView` literals; add a local `own(o)`/`lib(o)` factory at the top):

```ts
it('fromLibrary: wanted + in flight is downloading with a clamped percent', () => {
  const vm = fromLibrary(lib({ frameUuid: 'u1', localState: 'wanted' }), new Map([['u1', { done: 150, size: 100 }]]));
  expect([vm.device, vm.progress]).toEqual(['downloading', 100]);
});
it('fromLibrary: wanted with the publisher offline is missing, with that reason', () => {
  const vm = fromLibrary(lib({ localState: 'wanted', waitingForPublisher: true, holdersOnline: 0 }), new Map());
  expect([vm.device, vm.missingWhy]).toEqual(['missing', 'publisher offline']);
});
it('fromLibrary: wanted with no online holder is missing (holder offline); with one it is queued', () => {
  expect(fromLibrary(lib({ localState: 'wanted', holdersOnline: 0 }), new Map()).missingWhy).toBe('holder offline');
  expect(fromLibrary(lib({ localState: 'wanted', holdersOnline: 2 }), new Map()).device).toBe('queued');
});
it('copies counts this device when it holds the frame', () => {
  expect(copies(fromOwn(own({ segment: 'published', localState: 'own_held', holdersTotal: 0 })))).toBe(1);
  expect(copies(fromLibrary(lib({ localState: 'held', holdersTotal: 1 }), new Map()))).toBe(2);
});
it('a published own frame with one copy and a missing file matches both extra states', () => {
  const vm = fromOwn(own({ segment: 'published', pubState: 'published', localState: 'own_missing', holdersTotal: 1 }));
  expect(vm.states.sort()).toEqual(['disk', 'published', 'single']);
});
it('a held frame matches each of its failure kinds; the reason group is the first', () => {
  const vm = fromOwn(own({ segment: 'held', failures: [{ kind: 'solve', text: 'no coordinates' }, { kind: 'threshold', text: 'FWHM 3.42″ > 3.00″' }] }));
  expect(vm.states).toEqual(['solve', 'threshold']);
  expect(GROUPS.reason.key(vm)).toBe('solve');
});
it('unknown camera and night group under readable labels', () => {
  const vm = fromOwn(own({ camera: '', night: null }));
  render(<>{GROUPS.camera.renderLabel(GROUPS.camera.key(vm))}{GROUPS.night.renderLabel(GROUPS.night.key(vm))}</>);
  expect(screen.getByText('Unknown camera')).toBeInTheDocument();
  expect(screen.getByText('Unknown night')).toBeInTheDocument();
});
it('fromModeration fills manifest-only metrics when the mirror has the frame', () => {
  const m = { frameUuid: 'u9', fileName: 'a.fits', publisher: 'Olga', publisherAccountId: 'acc-o', filter: 'Ha', exptimeSec: 300, fwhmArcsec: 2.1, createdAt: '2026-09-30T08:00:00Z' };
  const vm = fromModeration(m, new Map([['u9', lib({ frameUuid: 'u9', night: '2026-09-29', camera: 'QHY268M', eccentricity: 0.4 })]]));
  expect([vm.night, vm.camera, vm.ecc, vm.submittedAt]).toEqual(['2026-09-29', 'QHY268M', 0.4, '2026-09-30T08:00:00Z']);
});
it('an excluded own frame reads as excluded in Status and in the state facet', () => {
  const vm = fromOwn(own({ segment: 'published', pubState: 'published', accepted: false, acceptedReason: 'wrong target', localState: 'own_held', holdersTotal: 2 }));
  expect(vm.excluded).toBe(true);
  expect(vm.states).toContain('excluded');
  expect(GROUPS.status.key(vm)).toBe('excluded');
});
it('no table offers a ZP column', () => {
  for (const t of Object.values(TABLES)) expect(t.columns).not.toContain('zp');
});
```

- [ ] **Step 2: Run** both test files → FAIL.
- [ ] **Step 3: Implement** `format.ts` additions and `frames.tsx` per the rules and tables above. The `cell` renderers
  are small JSX spans. Put the chip classes in one local helper `chip(tone)` returning
  `rounded px-1.5 py-0.5 text-[10px] font-medium` + tone classes (`bg-success/20 text-success`, `bg-warning/20
  text-warning`, `bg-error/20 text-error`, `bg-surface-hover text-content-muted`). Filter dot:
  `<span className="mr-1 inline-block h-2 w-2 rounded-full" style={{ backgroundColor: getFilterColor(f) }} />`.
  Weekday in the night label: `new Date(`${k}T00:00:00Z`).getUTCDay()` → `Sun…Sat`.
  `FRAME_ACCESS = { name: (v) => v.fileName, filter: (v) => v.filter, camera: (v) => v.camera, night: (v) => v.night,
  publisher: (v) => v.publisher, states: (v) => v.states }`.
- [ ] **Step 4: Run** → PASS; `npx tsc --noEmit` clean.
- [ ] **Step 5: Commit** `feat(collab): frame view-model, columns and per-tab table configs`.

---

### Task 7: `ProjectFrameTable` component

**Files:**
- Create: `src/components/collab/project/table/ProjectFrameTable.tsx`
- Test: `src/components/collab/project/table/ProjectFrameTable.test.tsx`

**Interfaces:**
- Consumes: Task 5 model, Task 6 `FrameVM`, `COLUMNS`, `GROUPS`, `FRAME_ACCESS`, `TABLES`, `TableId`.
- Produces:

```ts
export interface TableAction {
  id: string;
  verb: string;                       // 'Publish', 'Solve', 'Keep again', 'Approve', 'Reject'
  eligible: (r: FrameVM) => boolean;
  primary?: boolean;                  // accent button
  busy?: boolean;                     // disables + spinner
  run: (targets: FrameVM[]) => void;  // receives actionTargets(...).targets
}
export interface ProjectFrameTableProps {
  tableId: TableId;
  scope: string;                      // projectId — the session-state key prefix
  rows: FrameVM[];
  actions: TableAction[];
  onOpen: (r: FrameVM) => void;
  emptyText: string;                  // shown when rows is empty (not when facets hide everything)
  groupAction?: (node: GroupNode<FrameVM>) => ReactNode;  // Held back Reason headers
  toolbarExtra?: ReactNode;           // e.g. the moderation trust checkbox
  today?: string;                     // test seam; default localToday()
}
export default function ProjectFrameTable(props: ProjectFrameTableProps): JSX.Element;
```

Behaviour (spec §4.1; mockup `tableHtml`/`renderBody`):
1. **Facet row:** filter chips (every filter present in `rows`, ordered by `filterOrder`, each with its count from
   `facetCounts`; clicking toggles it in `facets.filters`); Camera `<select>` (All + cameras with counts, `''` shown
   as `Unknown camera`); Night `<select>` (All nights · Last night · Last 7 nights · Last 30 nights with counts);
   Publisher `<select>` when `publisherFacet`; state `<select>` when `stateFacet` (options with counts, zero-count
   options still listed); search `<input placeholder="Search frames">`; `Clear filters` button, disabled when
   `activeFacetCount === 0`.
2. **Group row:** `Group by` select (level 1 from `groupings`) `▸` select (level 2 = the remaining groupings, `None`
   allowed; hidden when level 1 is `none`); `Expand all` / `Collapse all`; `Columns` button opening a checklist of
   `config.columns` (the `name` column cannot be hidden).
3. **Totals strip** over the filtered set: `N of M frames`, `Σ formatDuration(sum exp)`, `formatBytes(sum size)`,
   `x̃ FWHM` (median, 2 dp, `—` when null); when something is selected: `K selected · Clear`; then one button per
   `action`, labelled `actionLabel(verb, targets.length, selectedCount)`, disabled when `targets.length === 0` or
   `busy`; then `toolbarExtra`.
4. **Table:** a scroll container (`max-h-[calc(100vh-22rem)] overflow-auto`) with a sticky header (a checkbox for the
   whole filtered view, then the visible columns in `config.columns` order; clicking a header sorts: same column
   flips `dir`, new column starts at 1), rows exactly `h-[29px]` with `border-b border-border/40`, right-aligned
   `tabular-nums` for numeric columns. Rendering is windowed: keep `scrollTop` and the container's `clientHeight` in
   state (`onScroll` + a `ResizeObserver` guarded by `typeof ResizeObserver !== 'undefined'`), take `windowSlice`
   over `flatten(...)` (or over the sorted frames when level 1 is `none`), and render top/bottom spacer `<tr>`s with
   the pads.
   - A group row: tri-state checkbox (`checkState` over `node.rows`; `some` → `indeterminate` via ref), a caret, the
     label `node.def.renderLabel(node.key)`, the frame count, `groupAction?.(node)` when provided, and under every
     other visible column its `renderAggregate?.(node.rows)`. Indent `depth * 16px`. Clicking the row (outside the
     checkbox/action) toggles expansion.
   - A frame row: checkbox, then cells. Clicking the row (outside the checkbox) calls `onOpen(row)`.
5. **State:** `useSessionState` keys `collab.${scope}.${tableId}.facets` (`Facets`), `.group` (`[string, string]`),
   `.sort` (`SortState`, default `{ col: 'name', dir: 1 }`), `.expanded` (`string[] | null`; `null` means "not
   initialised for this grouping", which triggers `initialExpanded` on the next render; changing the grouping sets
   it back to `null`). Column visibility: `localStorage` key `collab.table.${tableId}.cols` (JSON string array), read
   once with try/catch (fall back to `defaultColumns`), written on change with try/catch; both catches
   `console.warn('[collab-table] column prefs unavailable:', err)`.
6. **Selection:** local `useState<Set<string>>`. Whenever `rows` changes, prune the set to keys still present (a
   `useEffect` on `rows`).
7. **Empty states:** `rows.length === 0` → only `<p className="text-sm text-content-muted">{emptyText}</p>` (no facet
   row, no table). Rows exist but the facets hide all of them → the table header plus one row `No frames match these
   filters · Clear filters`.

- [ ] **Step 1: Write the failing tests** (wrap renders in `<SessionStateProvider>`; build `FrameVM`s with
  `fromOwn(own(...))` from a local factory):

```tsx
function renderTable(p: Partial<ProjectFrameTableProps> & { rows: FrameVM[] }) {
  const onOpen = vi.fn();
  const utils = render(
    <SessionStateProvider>
      <ProjectFrameTable tableId="ready" scope="p1" actions={[]} onOpen={onOpen} emptyText="Nothing ready." today="2026-09-30" {...p} />
    </SessionStateProvider>,
  );
  return { ...utils, onOpen };
}

it('an empty table shows only its sentence and no actions', () => {
  renderTable({ rows: [], actions: [{ id: 'pub', verb: 'Publish', eligible: () => true, primary: true, run: vi.fn() }] });
  expect(screen.getByText('Nothing ready.')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: /Publish/ })).toBeNull();
});

it('the primary action covers the filtered view, then the eligible part of the selection', () => {
  const run = vi.fn();
  const rows = [ready('1', { filter: 'L' }), ready('2', { filter: 'Ha' }), ready('3', { filter: 'Ha' })];
  renderTable({ rows, actions: [{ id: 'pub', verb: 'Publish', eligible: (r) => r.frameId !== 3, primary: true, run }] });
  expect(screen.getByRole('button', { name: 'Publish all 2' })).toBeEnabled();
  fireEvent.click(screen.getByRole('button', { name: /^Ha/ })); // filter chip
  expect(screen.getByRole('button', { name: 'Publish all 1' })).toBeEnabled();
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
  fireEvent.click(screen.getByRole('button', { name: 'Publish 1 of 2' }));
  expect(run).toHaveBeenCalledWith([rows[1]]);
});

it('selection is pruned when rows disappear', () => {
  const rows = [ready('1'), ready('2')];
  const { rerender } = renderTable({ rows, actions: [{ id: 'pub', verb: 'Publish', eligible: () => true, run: vi.fn() }] });
  fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
  expect(screen.getByRole('button', { name: 'Publish 2' })).toBeInTheDocument();
  rerender(<SessionStateProvider><ProjectFrameTable tableId="ready" scope="p1" rows={[rows[1]]} actions={[{ id: 'pub', verb: 'Publish', eligible: () => true, run: vi.fn() }]} onOpen={vi.fn()} emptyText="x" today="2026-09-30" /></SessionStateProvider>);
  expect(screen.getByRole('button', { name: 'Publish 1' })).toBeInTheDocument();
});

it('default grouping opens the first group and its first child; clicking a frame opens it', () => {
  const rows = [ready('1', { night: '2026-09-29', filter: 'L' }), ready('2', { night: '2026-09-28', filter: 'Ha' })];
  const { onOpen } = renderTable({ rows });
  expect(screen.getByText('f1.fits')).toBeInTheDocument();
  expect(screen.queryByText('f2.fits')).toBeNull(); // second night collapsed
  fireEvent.click(screen.getByText('f1.fits'));
  expect(onOpen).toHaveBeenCalledWith(rows[0]);
});

it('facet counts ignore their own facet', () => {
  renderTable({ rows: [ready('1', { filter: 'L' }), ready('2', { filter: 'Ha' })] });
  fireEvent.click(screen.getByRole('button', { name: /^Ha/ }));
  expect(screen.getByRole('button', { name: /^L\s*1/ })).toBeInTheDocument(); // still counted
});

it('renders a window, not 5000 rows', () => {
  const rows = Array.from({ length: 5000 }, (_, i) => ready(String(i + 1)));
  renderTable({ rows });
  fireEvent.change(screen.getByLabelText('Group by'), { target: { value: 'none' } });
  expect(screen.getAllByRole('row').length).toBeLessThan(100);
});

it('a null metric sorts after real values', () => {
  const rows = [ready('1', { fwhm: null }), ready('2', { fwhm: 3 }), ready('3', { fwhm: 1 })];
  renderTable({ rows });
  fireEvent.change(screen.getByLabelText('Group by'), { target: { value: 'none' } });
  fireEvent.click(screen.getByRole('columnheader', { name: /FWHM/ }));
  const names = screen.getAllByRole('row').map((r) => r.textContent ?? '').filter((t) => t.includes('.fits'));
  expect(names.map((t) => t.match(/f\d+\.fits/)![0])).toEqual(['f3.fits', 'f2.fits', 'f1.fits']);
});
```

  (`ready(id, o)` = `fromOwn(own({ frameId: Number(id), frameUuid: null, fileName: `f${id}.fits`, segment: 'ready',
  ...o-mapped }))`. Keep the factory in the test file. Give the "select all" header checkbox `aria-label="Select all
  shown"` and the group-by select `aria-label="Group by"`. Put filter chips' counts inside the button text.)
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** per the behaviour list. Use `useMemo` for `filtered`, `counts`, `tree`, `visible`. The
  visual rhythm follows the mockup (compact 12 px text, `bg-surface-elevated` group rows, sticky header
  `bg-surface`).
- [ ] **Step 4: Run** → PASS; `npx tsc --noEmit` clean.
- [ ] **Step 5: Commit** `feat(collab): ProjectFrameTable — grouped, filtered, windowed frame table`.

---

### Task 8: Exchange state + app-root provider

**Files:**
- Create: `src/components/collab/exchange/state.ts`, `src/contexts/CollabExchangeContext.tsx`
- Modify: `src/components/Layout.tsx` (mount the provider inside `TransfersProvider`)
- Test: `src/components/collab/exchange/state.test.ts`, `src/contexts/CollabExchangeContext.test.tsx`

**Interfaces:**
- Consumes: `ExchangeSnapshot`, `CollabExchangeProgress`, `ProjectFlows`, `FlowView`, `DeviceNameView`,
  `CollabLiveStatus` from `models.ts`.
- Produces:

```ts
// state.ts
export interface ExchangeState {
  projects: Record<string, ProjectFlows>;
  names: Record<string, DeviceNameView>;   // key nameKey(projectId, device)
  rates: Record<string, number[]>;         // key flowKey(flow), last 40 rateBps samples
}
export const EMPTY_EXCHANGE: ExchangeState;
export function nameKey(projectId: string, device: string): string;
export function flowKey(f: FlowView): string;                  // `${projectId}|${direction}|${device}`
export function applySnapshot(s: ExchangeState, snap: ExchangeSnapshot, scope: string | null): ExchangeState;
export function applyProgress(s: ExchangeState, ev: CollabExchangeProgress): ExchangeState;
export function clearFlows(s: ExchangeState): ExchangeState;
export function unknownDevices(s: ExchangeState): string[];    // nameKeys of flows with no name entry
export function peerLabel(s: ExchangeState, projectId: string, device: string): { member: string | null; device: string };
export function exchangeTotals(s: ExchangeState): { recvBps: number; sendBps: number; active: boolean };
// CollabExchangeContext.tsx
export function CollabExchangeProvider({ children }: { children: ReactNode }): JSX.Element;
export function useCollabExchange(): { state: ExchangeState; refreshProject: (projectId: string) => Promise<void> };
```

Rules:
- `applySnapshot(scope = null)` replaces `projects` wholesale (the no-arg command returns only projects with flows);
  `scope = id` replaces only that project (dropped when the answer has none for it). Names always merge. Rates are
  seeded with one sample per flow.
- `applyProgress`: for each project in the event, keep only flows where `moving || inFlight.length > 0 ||
  rateBps > 0`. No flow left → delete the project; else set it with `waitingForPublisher` carried over from the
  previous entry (events carry `null`). Append each kept flow's `rateBps` to its ring (max 40). Flows and rings of
  projects not in the event are untouched.
- `clearFlows` empties `projects` and `rates`, keeps `names`.
- `peerLabel`: member name from `names` when known, device = `deviceName ?? device.slice(0, 8)`.
- Provider: on mount `api.invoke<ExchangeSnapshot>('get_collab_exchange', { projectId: null })` → `applySnapshot(…,
  null)`; listen `collab-exchange-progress` → `applyProgress`; if `unknownDevices(next).length > 0` and no refetch is
  in flight, refetch the global snapshot once (a ref flag, cleared when it settles). Listen `collab-live-status` →
  `clearFlows` when `state !== 'live'`. `refreshProject(id)` invokes `get_collab_exchange { projectId: id }` →
  `applySnapshot(…, id)`. Every failure → `console.error('[collab-exchange] … failed:', err)` and the state stays as
  it was. `useCollabExchange` throws `useCollabExchange must be used within CollabExchangeProvider` outside it (the
  existing context convention).

- [ ] **Step 1: Write the failing tests.** `state.test.ts`:

```ts
const flow = (o: Partial<FlowView>): FlowView => ({ projectId: 'p1', device: 'devA', direction: 'recv', bytesSession: 0, rateBps: 1000, etaSecs: null, moving: true, completed: 0, inFlight: [], ...o });
const proj = (o: Partial<ProjectFlows>): ProjectFlows => ({ projectId: 'p1', recv: [], send: [], toGo: 0, waitingForPublisher: null, ...o });

it('a progress event keeps waitingForPublisher from the snapshot', () => {
  let s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({})], waitingForPublisher: 12 })], names: [] }, null);
  s = applyProgress(s, { projects: [proj({ recv: [flow({ rateBps: 2000 })], toGo: 5 })] });
  expect(s.projects.p1.waitingForPublisher).toBe(12);
  expect(s.projects.p1.toGo).toBe(5);
  expect(s.rates['p1|recv|devA']).toEqual([1000, 2000]);
});
it('the all-zero quiet event removes the project', () => {
  let s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({})] })], names: [] }, null);
  s = applyProgress(s, { projects: [proj({ recv: [flow({ moving: false, rateBps: 0 })] })] });
  expect(s.projects.p1).toBeUndefined();
});
it('rate history keeps the last 40 samples', () => {
  let s = EMPTY_EXCHANGE;
  for (let i = 0; i < 50; i++) s = applyProgress(s, { projects: [proj({ recv: [flow({ rateBps: i + 1 })] })] });
  expect(s.rates['p1|recv|devA']).toHaveLength(40);
  expect(s.rates['p1|recv|devA'][39]).toBe(50);
});
it('unknown devices are reported until a snapshot names them; the label falls back to the short id', () => {
  let s = applyProgress(EMPTY_EXCHANGE, { projects: [proj({ send: [flow({ device: 'abcdefghijkl', direction: 'send' })] })] });
  expect(unknownDevices(s)).toEqual([nameKey('p1', 'abcdefghijkl')]);
  expect(peerLabel(s, 'p1', 'abcdefghijkl')).toEqual({ member: null, device: 'abcdefgh' });
  s = applySnapshot(s, { projects: [], names: [{ projectId: 'p1', device: 'abcdefghijkl', memberName: 'Kostya', deviceName: 'kostya-obs' }] }, 'p9');
  expect(unknownDevices(s)).toEqual([]);
  expect(peerLabel(s, 'p1', 'abcdefghijkl')).toEqual({ member: 'Kostya', device: 'kostya-obs' });
});
it('a scoped snapshot replaces only its project', () => {
  let s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({})] }), proj({ projectId: 'p2', recv: [flow({ projectId: 'p2' })] })], names: [] }, null);
  s = applySnapshot(s, { projects: [], names: [] }, 'p2');
  expect(Object.keys(s.projects)).toEqual(['p1']);
});
it('totals sum rates by direction', () => {
  const s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({ rateBps: 10 })], send: [flow({ direction: 'send', rateBps: 5 })] })], names: [] }, null);
  expect(exchangeTotals(s)).toEqual({ recvBps: 10, sendBps: 5, active: true });
});
```

  `CollabExchangeContext.test.tsx` (mock `../api` like `ProjectDetail.test.tsx`, capture listeners by event name):
  (a) mounts → `get_collab_exchange` called with `{ projectId: null }`;
  (b) a `collab-exchange-progress` event with an unnamed device triggers exactly **one** extra
  `get_collab_exchange { projectId: null }` even when three such events arrive before it resolves;
  (c) a `collab-live-status` event `{ state: 'off' }` empties `state.projects` (read it through a probe component that
  renders `Object.keys(state.projects).join(',')`).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** both files and mount `<CollabExchangeProvider>` inside `<TransfersProvider>` in
  `Layout.tsx` (it wraps everything `TransfersProvider` wraps).
- [ ] **Step 4: Run** → PASS; `npx tsc --noEmit` clean.
- [ ] **Step 5: Commit** `feat(collab): app-root exchange state from the snapshot and progress events`.

---

### Task 9: Frame drawer + `ExcludeDialog`

**Files:**
- Create: `src/components/collab/project/FrameDrawer.tsx`, `src/components/collab/project/ExcludeDialog.tsx`
- Test: `src/components/collab/project/FrameDrawer.test.tsx`, `src/components/collab/project/ExcludeDialog.test.tsx`

**Interfaces:**
- Consumes: `FrameVM` (Task 6), `FrameHolderView`, `RuleVerdict` (Task 4), `formatTimestamp`, `formatBytes`; the
  commands of Task 3.
- Produces:
  - `export default function FrameDrawer({ projectId, frame, coordinator, onClose, onChanged }: { projectId: string; frame: FrameVM; coordinator: boolean; onClose: () => void; onChanged: () => void }): JSX.Element`
  - `export default function ExcludeDialog({ projectId, frames, onClose, onDone }: { projectId: string; frames: FrameVM[]; onClose: () => void; onDone: (excluded: number) => void }): JSX.Element`,
    which Tasks 10 and 11 reuse for the table action.

`ExcludeDialog`: a modal (the page's existing modal markup) titled `Exclude N frames from the project`. It has a
required reason `<textarea>` (trimmed, 1..=500 characters, a live `N / 500` counter; Exclude disabled outside the
range) and the note `Excluded frames stop counting toward the project and are no longer exchanged. You can restore
them from the frame's panel.` Exclude invokes `exclude_collab_frame { projectId, frameUuid, reason }` for each frame
in turn. On the first failure it stops, logs `console.error('[exclude] failed:', err)` and shows `Excluded K of N —
<message>` inline, then calls `onDone(K)`. All succeeding calls `onDone(N)`, then `onClose()`.

Drawer content (a fixed right panel `w-[26rem] max-w-[90vw] border-l border-border bg-surface-elevated`, over the
page, a close button with `aria-label="Close"`; Escape closes via a `keydown` listener on `document`):
1. **Identity:** file name, filter dot + filter (`(unmapped)` suffix when `!filterMapped`), camera (`Unknown camera`
   for `''`), night, exposure, size, publisher (library/moderation), object (own) with a `Link` "Open object" to
   `/objects/${setId}` when `setId`, and for own frames the **local path** (`own.path`, monospace, `break-all`, with
   a copy button that uses `navigator.clipboard.writeText` inside try/catch + `console.error`).
2. **Metrics:** FWHM″, Ecc, Stars, SNR (`—` when null).
3. **Gate** (own frames):
   - first the precondition failures: the `failures` whose `kind !== 'threshold'`, each `✕ {text}` in `text-error`;
   - then a rules table from `own.rules`, with columns `Rule · Value · Needs · ✓/✕`: `pass === true` → `✓` in
     `text-success`, `false` → `✕` in `text-error`, `null` → `—` muted with the title `not evaluated — see above`;
   - no rules → `No quality rules set.`
   - Own published frames also show version `v{contentVersion}`, `Published {formatTimestamp(publishedAt)}` and
     `lastError` in `text-error` when present.
4. **Exclusion** (announced frames): when `excluded`, a warning box `Excluded — {acceptedReason}`. When
   `coordinator`, a `Restore` button there (invokes `restore_collab_frame`, then `onChanged()`; a failure goes to
   `console.error` and inline `text-error`). When `coordinator && !excluded && pubState === 'published'`, an
   `Exclude…` button opens `ExcludeDialog` for this one frame, whose `onDone` calls `onChanged()`.
5. **Who holds it** (when `frameUuid`): invoke `get_collab_frame_holders { projectId, frameUuid }` on open and on
   `frameUuid` change (a cancelled flag guards the late answer).
   - Loading → `Loading holders…`.
   - Error → `console.error('[drawer] holders failed:', err)` + `Could not load holders.` in `text-error`.
   - Empty → `Nobody else holds it yet.`
   - Rows → online dot (`bg-success` / `bg-border`), `memberName ?? 'Unknown member'`, `deviceName ?? deviceShort`,
     a `publisher` chip when `isPublisher`, `v{contentVersion}` muted.
6. **Provenance** (library frames with `lib.receivedAt`): `Received {formatTimestamp(receivedAt)} from {X}`, where X
   is `receivedFromMember`, else `this device's files` when `receivedFromDevice === 'local'`, else the device's first
   8 chars.

- [ ] **Step 1: Write the failing tests:**
  - (a) An own held frame lists its precondition failure (`no analysis`) and a rules table where FWHM reads `3.42″ ·
    ≤ 3.00″ · ✕` and stars read `500 · ≥ 200 · ✓`.
  - (b) A rule with `pass: null` shows `—`.
  - (c) An own frame shows its path.
  - (d) Holders load and show `Kostya`, `kostya-obs` and a `publisher` chip; an unknown member reads `Unknown member`
    with the short id.
  - (e) A failed holders call shows `Could not load holders.`
  - (f) Escape calls `onClose`.
  - (g) A received library frame matches `getByText(/Received .* from Olga/)`.
  - (h) A coordinator on an excluded frame sees `Restore`, which invokes `restore_collab_frame` and then calls
    `onChanged`; a non-coordinator sees the reason but no button.
  - (i) `ExcludeDialog`: Exclude is disabled for a blank reason and for 501 characters. With two frames it invokes
    `exclude_collab_frame` twice with the trimmed reason. A failure on the second reads `Excluded 1 of 2` and calls
    `onDone(1)`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): frame drawer with rule-by-rule gate, holders, provenance, exclusion`.

---

### Task 10: My frames tab (Ready · Published · Held back) + `ReasonGroupAction`

**Files:**
- Create: `src/components/collab/project/MyFramesTab.tsx`, `src/components/collab/project/ReasonGroupAction.tsx`
- Test: `src/components/collab/project/MyFramesTab.test.tsx`, `src/components/collab/project/ReasonGroupAction.test.tsx`
- Delete in Task 16 (not here): `GateBlockers.tsx` + test (still imported by the old page until then)

**Interfaces:**
- Consumes: `ProjectFrameTable`, `TableAction`, `fromOwn`, `FrameVM`, `OwnFrameRow`, `LinkedSetView`,
  `AutoPublishSwitch`, `LinkObjectDialog`, `FilterMappingDialog`, `ExcludeDialog` (Task 9).
- Produces:

```ts
export type Segment = 'ready' | 'published' | 'held';
export interface MyFramesTabProps {
  projectId: string;
  rows: OwnFrameRow[] | null;          // null = loading
  error: boolean;
  links: LinkedSetView[];
  autoPublish: boolean;
  segment: Segment;
  onSegment: (s: Segment) => void;
  onReload: () => void;                // re-read list_project_own_frames
  onDetailReload: () => void;          // re-read get_collab_project_detail (links, card)
  onRequestPublish: (frameIds: number[]) => void;   // the shell confirms, then publishes
  publishBusy: boolean;
  onRequestRepublish: (frameIds: number[] | null) => void;  // null = "all"; the shell's guard dialog confirms
  republishBusy: boolean;
  canRepublish: boolean;
  coordinator: boolean;
  republishError: string | null;
  refusal: ReactNode;                  // the shell's publishing-device refusal box, or null
  onOpen: (vm: FrameVM) => void;
}
export default function MyFramesTab(p: MyFramesTabProps): JSX.Element;

export interface ReasonGroupActionProps {
  kind: string;                        // the Reason group's key (a BLOCKER_ORDER kind or '')
  rows: FrameVM[];                     // the group's frames
  solveBusy: boolean;
  analyzeBusy: ReadonlySet<number>;
  onSolve: (frameIds: number[]) => void;
  onAnalyze: (setId: number) => void;
  onOpenCalibration: (setId: number) => void;
  onMapFilters: () => void;
}
export default function ReasonGroupAction(p: ReasonGroupActionProps): JSX.Element | null;
```

Layout (mockup "My frames"): top row `Linked objects` + chips (`name · N lights · on target`) + `+ Link an object` +
`AutoPublishSwitch`. Then the segment switch (three buttons with counts: `Ready to publish N`, `Published N`, `Held
back N`; `aria-pressed` on the active one). On the right, `Recalibrate and republish all` (shown when
`canRepublish`), then `republishError` and `refusal`. Below that, one `ProjectFrameTable` for the active segment:
- **Ready:** `rows.filter(segment === 'ready')`. One primary action `{ id: 'publish', verb: 'Publish', eligible: ()
  => true, primary: true, busy: publishBusy, run: (t) => onRequestPublish(t.map((v) => v.frameId!)) }`. Empty text:
  `links.length === 0 ? 'Link an object to start.' : 'Nothing ready to publish — new frames appear here once they
  pass the gate.'`.
- **Held back:** `groupAction = (node) => node.def.id === 'reason' ? <ReasonGroupAction kind={node.key}
  rows={node.rows} … /> : null`. Actions `Solve` (eligible: failures include `solve`; run →
  `onSolveIds(ids)`) and `Analyze` (eligible: failures include `analyze`; run → `analyze_frame_set` for each distinct
  `setId`). Empty text: `Nothing held back.`
- **Published:**
  - `Republish`: `{ id: 'republish', verb: 'Republish', eligible: (v) => !v.excluded, busy: republishBusy, run:
    (t) => onRequestRepublish(t.map((v) => v.frameId!)) }`.
  - For a coordinator, also `Exclude`: `{ id: 'exclude', verb: 'Exclude', eligible: (v) => !v.excluded && v.pubState
    === 'published' && v.frameUuid !== null, run: (t) => setExcluding(t) }`, which opens `ExcludeDialog` and then
    calls `onReload()`.
  - `Recalibrate and republish all` (above the segments) calls `onRequestRepublish(null)`.
  - Empty text: `Nothing published yet.`

Solve/Analyze orchestration moves here from the page, unchanged in behaviour:
`plate_solve_batch { frameIds }` with the `solveStartedHereRef` guard;
`analyze_frame_set { frameSetId }` with a per-set busy set;
listeners `analysis-complete` (clears that set's busy, calls `onReload`) and `plate-solve-complete` (only when this
tab started it: clears busy, calls `onReload`);
failure notifications with the same titles and `dedupeKey`s as today (`solve-failed-…`, `analyze-failed-…`).
`FilterMappingDialog.onSaved` → close + `onReload()` (it returns a `GateReport`, which the tab ignores).
`LinkObjectDialog.onChanged` → `onDetailReload(); onReload()`.

`ReasonGroupAction` by kind: `analyze` → `Analyze` (one set: a button; several: a menu of `setName ?? Set #id`,
disabled while that set is busy); `solve` → `Solve N` (N = the group's frames) calling `onSolve(ids)`;
`linkCalibration` | `buildMasters` | `attest` → `Open calibration` (set picker as above) and `Attest as calibrated…`
(same target: the set's calibration tab); `mapFilter` → `Map filters`; `threshold` → the muted text `quality — the
frames themselves`; `uuid` → muted `re-scan the folder`; `outsideTarget` → muted `outside the target radius`; `''` →
`null`. Reuse the menu code from today's `GateBlockers` (Escape/outside-click close).

- [ ] **Step 1: Write the failing tests** (`MyFramesTab.test.tsx`, mock `../../../api`; wrap in `MemoryRouter`,
  `NotificationProvider`, `SessionStateProvider`):
  1. the segment buttons read `Ready to publish 2`, `Published 1`, `Held back 1` for a fixture of 4 rows;
  2. with no selection, `Publish all 2` calls `onRequestPublish([ids of the two ready rows])`;
  3. an empty Ready segment with no links shows `Link an object to start.` and no Publish button;
  4. the Held back Reason group for `solve` renders `Solve 1`; clicking it invokes `plate_solve_batch` with `{
     frameIds: [id] }`; a later `plate-solve-complete` calls `onReload` once; a `plate-solve-complete` without a solve
     started here calls nothing;
  5. `Analyze` on a group with two sets opens a menu naming both sets by `setName`; picking one invokes
     `analyze_frame_set { frameSetId }`; `analysis-complete` for it calls `onReload`;
  6. a failed `plate_solve_batch` raises a warning notification titled `Could not start the solve`;
  7. in Published, selecting two frames and clicking `Republish 2` calls `onRequestRepublish([id1, id2])`, and
     `Recalibrate and republish all` calls `onRequestRepublish(null)`;
  8. `Exclude` shows only when `coordinator`; with one excluded frame among three selected, it reads
     `Exclude 2 of 3` and opens the dialog listing 2 frames.
  `ReasonGroupAction.test.tsx`: the `threshold` kind renders the quality text and no button; `mapFilter` calls
  `onMapFilters`; `attest` offers `Attest as calibrated…` which calls `onOpenCalibration(setId)`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): My frames tab — Ready, Published and Held back tables`.

---

### Task 11: Library tab

**Files:**
- Create: `src/components/collab/project/LibraryTab.tsx`
- Test: `src/components/collab/project/LibraryTab.test.tsx`

**Interfaces:**
- Consumes: `ProjectFrameTable`, `fromLibrary`, `useCollabExchange` (in-flight map), `CollabAttention`,
  `ProjectExportDialog`.
- Produces: `export default function LibraryTab({ projectId, projectTitle, frames, error, reload, coordinator, onOpen }: { projectId: string; projectTitle: string; frames: ProjectFrameView[] | null; error: boolean; reload: () => void; coordinator: boolean; onOpen: (vm: FrameVM) => void }): JSX.Element` and `export function libraryToCome(frames: ProjectFrameView[] | null, inFlight: ReadonlyMap<string, unknown>): number` (the tab badge: frames that map to device `downloading`, `queued` or `missing`).

Behaviour: everything `ReceiveTab` does today moves here unchanged: the Collaboration-folder banner, `Export for
WBPP`, `CollabAttention`, and the reload on `collab-attention-changed` / `collab-frames-landed` for this project.
Then the table over `frames.filter((f) => !f.own && f.state !== 'pending')` mapped with `fromLibrary(f, inFlight)`,
where `inFlight` is built from `state.projects[projectId]?.recv.flatMap((fl) => fl.inFlight)` keyed by `frameUuid`
(`{ done, size }`). Actions: `{ id: 'keep', verb: 'Keep again', eligible: (v) => v.device === 'notKept', run:
(t) => keep_collab_frames_again({ projectId, frameUuids }) then reload }`, and for a coordinator `{ id: 'exclude',
verb: 'Exclude', eligible: (v) => !v.excluded && v.pubState === 'published', run: (t) => open ExcludeDialog(t) }`,
followed by `reload()` when the dialog is done. A failure → `console.error` + inline
`text-error`. Empty text: `No frames from other members yet — published contributions appear here.` Loading →
`Loading…`. `error` → `Could not load the library — see console.`

- [ ] **Step 1: Write the failing tests** (port `ReceiveTab.test.tsx`'s banner/attention/listener cases, then add):
  a frame in the exchange's in-flight list renders `downloading` with its percent; a `wanted` frame with
  `waitingForPublisher` reads `missing` + `publisher offline`; `Keep again` is disabled with no `not_kept` frame and,
  with one, invokes `keep_collab_frames_again { projectId, frameUuids: ['u2'] }`; `libraryToCome` counts downloading
  + queued + missing and not `notKept`/`have`; `Exclude` is absent for a non-coordinator and, for a coordinator,
  opens the dialog with the eligible frames. Wrap renders in a test `CollabExchangeProvider` whose mocked
  `get_collab_exchange` returns the in-flight fixture.
- [ ] **Step 2–4:** Run → FAIL, implement, run → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): Library tab — other members' frames and their state here`.

---

### Task 12: Moderation tab

**Files:**
- Create: `src/components/collab/project/ModerationTab.tsx`
- Test: `src/components/collab/project/ModerationTab.test.tsx`

**Interfaces:**
- Produces: `export default function ModerationTab({ projectId, library, onDecided, onOpen }: { projectId: string; library: ProjectFrameView[] | null; onDecided: () => void; onOpen: (vm: FrameVM) => void }): JSX.Element`.

Behaviour: load `list_collab_moderation { projectId }` (errors inline, as today); rows =
`fromModeration(m, byUuid(library))`. `toolbarExtra` = `Checkbox` (the one checkbox component) "Trust these
publishers" (default on). Actions: `Approve` (primary, eligible all) → for each target sequentially
`approve_collab_frame { projectId, frameUuid, trust }`; `Reject` (eligible all) → opens the existing reject dialog
(reason required, ≤ 500 chars; move `RejectDialog` from `ModerationQueue.tsx` into this file), then
`reject_collab_frame { projectId, frameUuid, reason }` for each target. Stop at the first failure: show `Approved N
of M — <error>` inline (`text-error`), then reload and `onDecided()`. Buttons are busy while the loop runs. Empty
text: `Nothing waiting for review.`

- [ ] **Step 1: Write the failing tests:** approve-all sends one `approve_collab_frame` per pending frame with
  `trust: true`; unchecking trust sends `trust: false`; a failure on the second of three stops and reads `Approved 1
  of 3`; reject requires a reason and sends it for every selected frame; the manifest-mirror columns show `night`
  when the library has the frame.
- [ ] **Step 2–4:** Run → FAIL, implement, run → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): Moderation tab as a table with batch approve and reject`.

---

### Task 13: Members tab

**Files:**
- Create: `src/components/collab/project/MembersTab.tsx`, `src/components/collab/project/memberColors.ts`
- Test: `src/components/collab/project/MembersTab.test.tsx`, `src/components/collab/project/memberColors.test.ts`

**Interfaces:**
- Produces: `export default function MembersTab({ projectId, onMembers }: { projectId: string; onMembers?: (m: MemberSummary[]) => void }): JSX.Element`; `memberColors.ts`: `export const MEMBER_TONES = ['bg-accent', 'bg-success', 'bg-purple', 'bg-warning', 'bg-orange', 'bg-error', 'bg-info', 'bg-accent-muted']; export function memberTone(accountId: string, all: { accountId: string; displayName: string }[]): string` (members sorted by `displayName` then `accountId`; tone = index modulo the list length; stable for the same member set).

Behaviour: `get_collab_member_summary { projectId }` on mount (error → console + `Could not load members — see
console.`). Columns: Member (tone dot, name, `coordinator` chip), Role (`dataRole` → `Processor` for
`send_receive`, `Contributor` for `send`, else the raw value), Last seen (`online` → `online now` in `text-success`;
else `lastSeenAt` → `formatTimestamp(…)` + muted `formatRelative(…)`; `null` → `never`), Published (frames),
Integration (`formatDuration(Σ secondsByFilter)`), one column per filter present in any member's `secondsByFilter`
(ordered by `filterOrder`, hours via `formatDuration`), Holds (`holdsFrames` · `formatBytes(holdsBytes)` · `(share ×
100).toFixed(0)%`). Every column sorts; a header click flips direction. Last seen sorts `online` first, then by
`lastSeenAt` desc, then never. Default sort: Published desc. Clicking a row expands it: devices (dot + name or
`device.slice(0,8)`), and `qualityByCamera` as a small table `Camera · Filter · Frames · x̃ FWHM · x̃ Ecc` (`''` camera
→ `Unknown camera`). `onMembers` is called with the loaded list (the Overview reuses it through the shell).

- [ ] **Step 1: Write the failing tests:** online member reads `online now`; an offline member with `lastSeenAt`
  shows its timestamp and a relative time; `null` reads `never`; sorting by Last seen puts online first and never
  last; the expanded row lists `Unknown camera` for `camera: ''`; `memberTone` is stable across calls and differs
  for two members.
- [ ] **Step 2–4:** Run → FAIL, implement, run → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): Members tab — people, last seen, integration and holdings`.

---

### Task 14: Exchange tab

**Files:**
- Create: `src/components/collab/project/ExchangeTab.tsx`, `src/components/collab/exchange/PeerFlowRow.tsx`
  (shared with Transfers in Task 17)
- Test: `src/components/collab/project/ExchangeTab.test.tsx`

**Interfaces:**
- Consumes: `useCollabExchange`, `peerLabel`, `formatRate`, `formatDuration`, `formatBytes`,
  `ReceiveSessionView`.
- Produces: `export default function ExchangeTab({ projectId, canReceive }: { projectId: string; canReceive: boolean }): JSX.Element`; `export function PeerFlowRow({ flow, label, rates }: { flow: FlowView; label: { member: string | null; device: string }; rates: number[] }): JSX.Element` (expandable, `aria-expanded`).

Behaviour (mockup `liveHtml` / `peerRow`):
- On mount `refreshProject(projectId)` (so `toGo` / `waitingForPublisher` show even when nothing moves), and
  `list_collab_receive_sessions { projectId, limit: 50 }`, re-read on `collab-frames-landed` for this project.
- **Receiving** card (only when `canReceive`): header `Receiving · {formatRate(Σ recv rate)} from {n} members · {toGo}
  frames to go · waiting for publisher: {waitingForPublisher}` (omit the last clause when `null` or 0); one
  `PeerFlowRow` per recv flow; no flows → `Nothing arriving right now.` When `!canReceive`: the card reads `Your role
  does not receive project data` and `Contributors only send. Ask the coordinator for the Processor role to receive
  the project.`
- **Sending** card: header `Sending · {rate} to {n} members`; rows; none → `Nothing being served right now.`
- `PeerFlowRow`: avatar initial (tone from `memberTone` when the member is known, `bg-surface-hover` otherwise); `↓
  from {member ?? device}` / `↑ to …`; sub-line `{device} · {inFlight.length} in flight · {completed} {landed|served}
  this session · {formatBytes(bytesSession)}`; rate `formatRate(rateBps)`; an inline SVG sparkline of `rates` (120×26,
  `polyline` stroke `currentColor` in `text-accent` for recv and `text-success` for send; no raw colours); `ETA
  {formatDuration(etaSecs)}` when not null. Expanded: one line per in-flight frame (name, a progress bar, `formatBytes(done)
  / formatBytes(size)`).
- **Receive history** (when `canReceive`): a table Started · Frames · Size · Sources (`memberName ?? deviceName ??
  device.slice(0,8)` joined with `, `) · Rate (`bytes / (finishedAt − startedAt)` via `formatRate`, `—` when the span
  is 0) · Failed (`text-error` when > 0). Empty → `No receive sessions yet.`

- [ ] **Step 1: Write the failing tests:** a recv flow from a named device renders `↓ from Kostya` and `31.0 MB/s`;
  an unnamed device renders its 8-char id; a contributor (`canReceive=false`) sees the role sentence and still the
  Sending card; expanding a row lists its in-flight file; the sessions table lists `Kostya, Olga` for two sources;
  `waitingForPublisher: 12` appears in the header after `refreshProject` resolves.
- [ ] **Step 2–4:** Run → FAIL, implement, run → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): Exchange tab — live peers both directions and receive history`.

---

### Task 15: Overview tab

**Files:**
- Create: `src/components/collab/project/OverviewTab.tsx`
- Test: `src/components/collab/project/OverviewTab.test.tsx`

**Interfaces:**
- Produces: `export default function OverviewTab(p: { projectId: string; goals: Record<string, number> | null; members: MemberSummary[] | null; own: OwnFrameRow[] | null; libraryToCome: number; pending: number; canModerate: boolean; thresholds: ThresholdRuleView[]; thresholdsVersion: number | null; onOpenSegment: (s: Segment) => void; onOpenTab: (t: string) => void }): JSX.Element`.

Content (mockup Overview):
1. **Integration per filter.** Filters = union of `goals` keys and every member's `secondsByFilter` keys, ordered by
   `filterOrder`. Per filter, one row: filter dot + name; a horizontal bar whose width scale is
   `max(goal, total)` across all filters; stacked segments per member (`memberTone`, `title` = `member · 12h 30m`);
   when a goal exists, a goal marker (`border-l-2 border-content` at `goal / scale`) and on the right `N to go`
   (`formatDuration(goal − total)`) or `goal met` (`text-success`); without a goal just `formatDuration(total)`. A goal
   for a filter with no frames still draws its empty bar. `members === null` → `Loading…`; no filters at all → `No
   integration yet.`
2. **My frames**: three clickable numbers `Ready N`, `Published N`, `Held back N` → `onOpenSegment`.
3. **Needs attention** (only non-zero items, each a button): `N held back — mostly {REASON_LABEL of the most common
   first kind}` → held; `N published frames not on disk or changed` → published; `N of your frames exist in one copy
   only` (own published with `copies === 1`) → published; `N library frames still to come` → library;
   `N frames wait for your review` (when `canModerate && pending > 0`) → moderation. None → `Nothing needs attention.`
4. **Exchange now**: `↓ {rate} from {members}` / `↑ {rate} to {members}` from `useCollabExchange` for this project,
   names via `peerLabel`; nothing moving → `Quiet.`; a `Open Exchange →` link → `onOpenTab('exchange')`.
5. **Quality thresholds**: moved verbatim from today's Overview section (text and the portal note unchanged).

- [ ] **Step 1: Write the failing tests:** with goals `{ Ha: 36000 }` and two members contributing 7200 s and 3600 s
  of Ha, the Ha row reads `7h to go` and renders two segments; a goal for `SII` with no frames renders an `SII` row;
  a filter without a goal shows only its total; Needs attention lists the held-back item naming `No coordinates or
  pixel scale` when that is the commonest first kind; clicking `Ready 2` calls `onOpenSegment('ready')`; empty
  attention reads `Nothing needs attention.`
- [ ] **Step 2–4:** Run → FAIL, implement, run → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(collab): Overview tab — integration against goals, my numbers, attention`.

---

### Task 16: The page shell, removals, deep links, test migration

**Files:**
- Rewrite: `src/pages/ProjectDetail.tsx`
- Create: `src/components/collab/project/usePublishing.ts`, `src/components/collab/project/RepublishGuardDialog.tsx`
  (+ `RepublishGuardDialog.test.tsx`)
- Modify: `src/hooks/useCollabNotifications.ts`, `src/components/collab/CollabAttention.tsx` (`?tab=receive` →
  `?tab=library`), `src/components/collab/FrameSetProjectBlock.tsx` (`?tab=contribute` → `?tab=mine`)
- Delete: `src/components/collab/ReceiveTab.tsx`, `ReceiveTab.test.tsx`, `ModerationQueue.tsx`, `GateBlockers.tsx`,
  `GateBlockers.test.tsx`
- Rewrite tests: `src/pages/ProjectDetail.test.tsx`

**Interfaces:**
- Consumes: every tab from Tasks 9–15; Task 1's `frameIds` on both publish and republish.
- Produces: `usePublishing(projectId, { reloadDetail, reloadOwn })` → `{ publish(frameIds: number[]): Promise<void>; republish(frameIds: number[] | null): Promise<void>; switchHere(): Promise<void>; publishBusy; publishError; republishBusy; republishError; switchBusy; refusedBy; updateRequired; clearPublishError(); clearRepublishError() }`. It holds today's `doPublish`/`doRepublish`/`doSwitch` bodies verbatim (same notifications, dedupe keys, busy/outdated/publishing-device handling). The one change: `publish_collab_frames` and `republish_collab_frames` are invoked with `{ projectId, frameIds }`
  (`frameIds: null` for "republish all"). `RepublishGuardDialog({ count, sourceBytes, all, busy, error, onConfirm,
  onCancel })` is exported with `REPUBLISH_TYPED_CONFIRM_ABOVE = 100`.

Shell responsibilities:
- Loads `get_collab_project_detail` (missing → today's "not in your local list" text),
  `list_project_own_frames { projectId }` (own rows), `list_collab_frames { projectId }` (library/moderation mirror,
  no contributor state needed any more), and `get_collab_member_summary` through `MembersTab`'s `onMembers`. Also
  load it once itself for the Overview when the Members tab has not mounted: the shell calls the command on mount and
  passes the result to both.
- Header: unchanged from today (title, target, coordinator chip, `CollabLiveStatus`, Manage on portal, publishing
  device line with the switch, `UpdateRequired`, `AutoReplicateBar` when `canReceive`).
- Tabs, in order: `overview` Overview · `mine` My frames (badge = ready count) · `library` Library (only
  `canReceive`; badge = `libraryToCome`) · `members` Members · `exchange` Exchange · `moderation` Moderation (only
  `canModerate`; badge `card.pendingFrames`). Tab state `useSessionState('projectDetail.tab', 'overview')`; an
  unavailable tab falls back to `overview`. Segment state `useSessionState('projectDetail.segment', 'ready')`.
- Deep links: `?tab=` accepts the six ids plus the aliases `receive` → `library` and `contribute` → `mine`, then is
  removed from the URL (`replace`).
- Drawer: `const [drawer, setDrawer] = useState<FrameVM | null>(null)`; `<FrameDrawer projectId frame={drawer}
  coordinator={c.coordinator} onClose={() => setDrawer(null)} onChanged={() => { reloadOwn(); reloadLibrary(); }} />`
  when set. `coordinator={c.coordinator}` also goes to `MyFramesTab` and `LibraryTab`.
- Publish confirm dialog: today's dialog, with the count = the requested ids' length and the estimate `ids.length ×
  APPROX_FRAME_BYTES`. The dialog keeps the requested ids in state; Publish calls `publishing.publish(ids)`, which
  then reloads own frames + detail.
- **Republish guard (owner ruling: nobody re-announces 100 TB with one click).** `onRequestRepublish(ids)` opens
  `RepublishGuardDialog` with:
  - `all = ids === null`;
  - `count` = `ids.length`, or for "all" the number of own rows in segment `published` that are not excluded;
  - `sourceBytes` = Σ `byteSize` of those rows.

  The dialog reads:
  - title `Recalibrate and republish all` for "all", else `Republish N frames`;
  - `N frames · {formatBytes(sourceBytes)} of source frames will be recalibrated. Every frame whose bytes change is
    posted as a new version, and every processor holding it downloads it again.`;
  - the existing warning line.

  When `all || count > REPUBLISH_TYPED_CONFIRM_ABOVE`, a text input labelled `Type {count} to confirm` appears, and
  Republish stays disabled until its trimmed value equals `String(count)`. Below the threshold, a plain confirm is
  enough. Confirm calls `publishing.republish(ids)`; busy and error show inside the dialog, as the old dialog did.
  `count === 0` → Republish is disabled with `Nothing to republish.`

- [ ] **Step 1: Migrate the tests.** Rewrite `ProjectDetail.test.tsx` around the new structure, keeping every
  behaviour the old file pinned:
  - the default mock switch answers `list_project_own_frames`, `get_collab_member_summary`, `get_collab_exchange` and
    `list_collab_receive_sessions`, and the render wraps a `CollabExchangeProvider`;
  - the publish tests (inline error, busy refusal, no double toast, publishing-device refusal and switch,
    held-back-for-device, "unrelated reasons show no refusal", "names the device from the field", switching clears)
    now open **My frames**, click `Publish all N`, confirm, and assert `publish_collab_frames` was called with
    `{ projectId: 'proj-1', frameIds: [...] }`;
  - the analyze/solve/listener/notification tests move to My frames' Held back segment;
  - the republish tests (busy, refusal, publishing-device) go through the guard: "all" needs the typed count
    (Republish disabled until `5` is typed for 5 published frames), then `republish_collab_frames` is called with
    `{ projectId: 'proj-1', frameIds: null }`; a 2-frame selection confirms without typing and sends `frameIds: [a,
    b]`;
  - `RepublishGuardDialog.test.tsx`: a 101-frame selection requires typing `101`, and `100` does not; a wrong number
    keeps the button disabled; `count 0` disables it with `Nothing to republish.`;
  - new: `?tab=receive` opens Library, `?tab=contribute` opens My frames; a contributor
    (`dataRole: 'send'`, not coordinator) sees no Library tab; Moderation shows only for `coordinator &&
    requireApproval`; the My frames badge shows the ready count; clicking a row opens the drawer and Escape closes
    it.
  Delete the tests of removed components (`ReceiveTab.test.tsx`, `GateBlockers.test.tsx`). Their cases were ported
  in Tasks 10–11.
- [ ] **Step 2: Run** `npx vitest run src/pages/ProjectDetail.test.tsx` → FAIL (old page).
- [ ] **Step 3: Implement** the shell and `usePublishing`, delete the removed files, update the three link sites.
  `grep -rn "GateBlockers\|ReceiveTab\|ModerationQueue\|PublicationHistory\|GateTable" src` must return nothing.
- [ ] **Step 4: Run** `npx vitest run src/pages src/components/collab src/hooks` → PASS; `npx tsc --noEmit` clean.
- [ ] **Step 5: Commit** `feat(collab): six-tab project page shell; retire the four-tab lists`.

---

### Task 17: Transfers — collab groups, sessions in history, panel, sidebar indicator

**Files:**
- Create: `src/components/transfers/CollabTrafficGroups.tsx`
- Modify: `src/pages/Transfers.tsx`, `src/components/transfers/types.ts`, `src/components/transfers/TransferRow.tsx`,
  `src/components/transfers/TransfersPanel.tsx`, `src/components/transfers/TransferIndicator.tsx`
- Test: `src/components/transfers/CollabTrafficGroups.test.tsx`, `src/pages/Transfers.test.tsx` (extend or create),
  `src/components/transfers/TransferIndicator.test.tsx` (extend or create)

**Interfaces:**
- Consumes: `useCollabExchange`, `exchangeTotals`, `peerLabel`, `PeerFlowRow` (Task 14), `ReceiveSessionView`.
- Produces: `export function CollabTrafficGroups({ projectTitles }: { projectTitles: Record<string, string> }): JSX.Element | null` (one expandable group per project with flows; header `{title} · ↓ {rate} · ↑ {rate} · {n} peers`; body = `PeerFlowRow`s; link `Open in project →` to `/projects/${id}?tab=exchange`); `UnifiedRow` gains `| { kind: 'session'; selKey: string; session: ReceiveSessionView }`; `export function mergeHistory<T extends { at: string }>(a: T[], b: T[]): T[]` in `src/components/transfers/historyGrouping.ts` (newest first, stable).

Behaviour:
- **Transfers page:** render `CollabTrafficGroups` between the chips and the list when it has flows (visible under
  the `all`, `sending` and `receiving` filters). Load `list_collab_receive_sessions { projectId: null, limit: 200 }`
  on mount, on `collab-frames-landed`, and on the page's existing `sync-finished` refetch. Map each session to a
  `session` row and merge it with the history rows by time (`finishedAt` for sessions, the group's
  `finishedAt ?? startedAt` for history), newest first; live rows stay on top. A session belongs to buckets `all`,
  `completed` and also `failed` when `failed > 0`. `TransferRow` renders a session as one line: `{projectTitle} chip
  · {frames} frames · from {sources joined} · {formatBytes(bytes)} · {formatRate(bytes / seconds)}`, with no actions,
  and selecting it shows no detail pane (return `null` for sessions in the detail selector).
- **TransfersPanel** Active tab: under the personal rows, one line per collab project with flows (`title · ↓ rate ·
  ↑ rate`) linking to the project's Exchange tab, closing the panel on click. Titles come from the panel's existing
  `projectNames` map.
- **TransferIndicator:** `const { recvBps, sendBps, active } = exchangeTotals(useCollabExchange().state)`. The icon
  goes `text-accent` when personal `up > 0` **or** collab `active`. The title gains `\nCollaboration — ↓ {rate} · ↑
  {rate}` when active. The expanded form shows a third item `{formatRate(recvBps + sendBps)}` when active. The `visible` gate stays unchanged: it already means "signed in with a role", which collab needs too.

- [ ] **Step 1: Write the failing tests:** `mergeHistory` orders by time and keeps equal-time order; a session row
  renders `from Kostya, Olga` and `48 frames`; a failed session counts under Failed; `CollabTrafficGroups` renders
  nothing with no flows and one group per project otherwise, with `Open in project →` linking to `?tab=exchange`; the
  indicator's icon is `text-accent` with only collab traffic and `up = 0` (render it inside a test `TransfersContext`
  value + a `CollabExchangeProvider` whose snapshot has one moving flow).
- [ ] **Step 2–4:** Run → FAIL, implement, run → PASS, `tsc` clean.
- [ ] **Step 5: Commit** `feat(transfers): collab traffic groups, receive sessions in history, indicator counts collab`.

---

### Task 18: Docs, open items, spec amendments, final gates

**Files:**
- Modify: `docs/superpowers/specs/2026-09-29-collab-project-observability-design.md` (add §14 "Amendments (wave 2,
  2026-09-30)")
- Modify: `docs/superpowers/open-items.md`
- Modify: `docs/transfers/README.md` (Collab section: the project page tabs, Transfers groups/sessions, indicator)
- Modify: `CLAUDE.md`. A rule, a command surface and a path changed:
  - `publish_collab_frames` / `republish_collab_frames` take `frameIds`;
  - two new commands `exclude_collab_frame` / `restore_collab_frame` move the Tauri command count from 286 to 288.
    Recount with the strict grep CLAUDE.md names and write the new number, date and two names in its parenthetical;
  - the collab frontend moved to `src/components/collab/project/`.

  Add one clause to the Transfers / collab summary line.

- [ ] **Step 1: Spec §14 amendments.** One numbered line each:
  1. Publish and republish take an optional `frameIds`. The primary action always sends the ids it shows. Every
     republish goes through a guard dialog, which asks for the typed frame count for "all" or for more than 100
     frames.
  2. Exclude/Restore (coordinator) are built on the hub's existing `PATCH …/frames/{uuid}`. Ask to exclude,
     Download and Stop keeping (held) are not built (hub mechanism / new core model).
  3. The project page no longer calls `evaluate_collab_gate`. Held back's Reason actions work from the group's rows.
  4. `FrameGateRow` / `OwnFrameRow` carry `rules[]` (`RuleVerdict`) from `evaluate_frame`, and `OwnFrameRow` carries
     `path` and `accepted`. The drawer is rule-by-rule, as §4.1 says.
  5. `ProjectFrameView` gained `receivedAt` / `receivedFromDevice` / `receivedFromMember`.
  6. "Only one copy" means `holdersTotal + (this device holds it)` = 1, because `holdersTotal` counts other devices
     only.
- [ ] **Step 2: open-items.md**, following that file's own format:
  - the owner smoke: spec §10's three-instance scenario, plus
    - select three Ready frames and publish exactly those;
    - republish two selected frames;
    - "Recalibrate and republish all" asks for the typed count;
    - the coordinator excludes a frame with a reason and it shows `excluded` on the publisher's Published tab, then
      restores it;
    - the drawer shows the rules table and the path;
    - Held back Solve/Analyze from the group header;
    - Library `Keep again`;
    - Transfers shows a project group and a session with both sources;
    - the sidebar indicator lights on collab-only traffic;
  - one backlog line for Ask to exclude (hub) and per-frame Download / Stop keeping (core want/unwant override on top
    of the policy + replica deletion with the last-copy warning).
- [ ] **Step 3: docs/transfers/README.md** + CLAUDE.md edits as scoped above.
- [ ] **Step 4: Final gates**, all of them, reporting output honestly:
  `npm test` (full vitest) → all green; `npx tsc --noEmit` → clean; `cargo check --workspace` → clean;
  `cargo test -p athenaeum-core` (all targets) → green apart from the known live-exchange load flakes, which must
  then pass alone with `cargo test -p athenaeum-core --lib collab_live::live_tests -- --test-threads=2`;
  `grep -rn "zp\b\|ZP" src/components/collab` → nothing.
- [ ] **Step 5: Commit** `docs(collab): wave 2 amendments, transfers reference, open items`.

---

## Self-review notes

- **Spec coverage:**
  - §4 page and tabs: Tasks 10–16.
  - §4.1 table: Tasks 5–7.
  - §4.1 drawer (rule-by-rule, path, holders): Tasks 4 and 9.
  - §4.2 configs: Task 6.
  - §4.2 actions, per the owner's rulings:
    - Publish and Republish on the selection: Tasks 1, 10 and 16;
    - Exclude: Tasks 3, 9, 10 and 11;
    - Keep again: Task 11;
    - Approve / Reject: Task 12.
  - Held back specifics: Task 10.
  - Library specifics: Tasks 6 and 11.
  - §5.5 goals: Task 15.
  - §6.4 surface consumption: Task 8.
  - §7.2 drawer provenance: Tasks 2 and 9.
  - §7.3 Transfers and indicator: Task 17.
  - §7.4 no notifications added: nothing to build.
  - §8 removals and split: Task 16.
  - §10 frontend test list:
    - grouping and aggregates, facet counts, sort, `(N of M)`, windowing: Task 5 (and Task 7);
    - Transfers merge order and indicator: Task 17;
    - role-dependent tabs: Task 16.
  - Items deliberately not built (Ask to exclude, Download, Stop keeping) are listed under Scope rulings and land in
    open items (Task 18).
- **Type names used across tasks:**
  - `FrameVM` (with `excluded`), `TableId`, `TableAction`, `Segment`, `ExchangeState`, `peerLabel`,
    `exchangeTotals`, `memberTone`, `PeerFlowRow`, `libraryToCome`, `ExcludeDialog`, `RepublishGuardDialog`,
    `REPUBLISH_TYPED_CONFIRM_ABOVE`, `usePublishing`, `RuleVerdict` are defined in the task that produces them and
    consumed with the same names.
  - `publish_collab_frames` / `republish_collab_frames { projectId, frameIds }` match Task 1's Tauri arg `frame_ids`
    (Tauri camelCases args) and the web `PublishArgs`.
  - `exclude_collab_frame { projectId, frameUuid, reason }` / `restore_collab_frame { projectId, frameUuid }` match
    Task 3.
