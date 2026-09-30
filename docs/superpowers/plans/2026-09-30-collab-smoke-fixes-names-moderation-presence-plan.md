# Collab smoke fixes — device names, moderation by capability, live presence — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three defects the owner found in the first smoke of the wave-2 project page:
1. Device names read as the member's name ("Vilen Vilen Vilen").
2. The Moderation tab, and Exclude/Restore, are gated on the wrong rule.
3. A member coming online does not show until the page is reopened.

**Architecture:** Four changes:
- **Hub:** one query fix (the holder snapshot sends the device's name).
- **Core:** one card field (`canModerate`) and one throttled event (`collab-peers-changed`), both hosts via the existing event emitter.
- **Frontend:** gating on `canModerate`, a Moderation tab with an Excluded-frames table, and listeners that re-read on `collab-peers-changed`.

**Tech Stack:** Rust (hub: axum + sqlx/Postgres; app core), React/TS + vitest.

**Spec:** `docs/superpowers/specs/2026-09-29-collab-project-observability-design.md` (§4 tabs, §5.3 holders, §5.4 members, §14 wave-2 amendments). This plan adds §15 amendments (Task 6).

## Diagnosis (owner smoke 2026-09-30, verified in code and data)

- **Device names.** `athenaeum-hub/src/routes/holders.rs` `holders_snapshot` selects
  `pm.display_name` (the member's name) as each device's `displayName`. Every one of a member's devices is
  therefore stored in the app's `collab_holder_devices.display_name` as the member's name. The owner's dev DB
  holds three rows, all `Vilen`.
  - The app reads that field as the device name everywhere: `FrameHolderView.deviceName`,
    `MemberDeviceView.name`, the exchange `names[].deviceName`, and session sources. Its tests use device
    names (`anna-obs`, `bo-mac`).
  - `devices.name` exists (nullable).
  - Fix at the hub: `COALESCE(d.name, '')`. An empty name already falls back to the short device id in the
    app, so the app needs no change for this.
- **Moderation gating.** The hub authorizes approve, reject and exclude/restore with the `data.moderate`
  capability (`frames.rs` `lock_and_require_cap(..., "data.moderate")`, `patch_frame`).
  - The app gates the Moderation tab on `coordinator && requireApproval`, and Exclude/Restore on
    `coordinator`.
  - In the smoke project the local member holds `data.moderate` without being coordinator, and approval is
    off, so nobody sees the tab.
  - The cached `collab_projects.gov_caps_json` already carries the caps; `ProjectCard` does not expose them.
- **Presence lag.** The hub publishes presence changes coalesced per second. The core applies them at once
  (`feed.rs` `LiveEvent::Presence` → `FeedEffect::ProvidersChanged(project)`; holder changes raise the same
  effect).
  - No event reaches the frontend, so the project page reads "online" only when it loads:
    - the member summary on mount;
    - the frame lists on landed/attention events;
    - drawer holders on open.

## Owner rulings (2026-09-30)

- Moderation is shown to everyone who can moderate: `coordinator` **or** `data.moderate`. The tab has two
  parts:
  1. **Waiting for review.** The existing pending queue when the project requires approval. Otherwise the
     single line `This project publishes without review.`
  2. **Excluded frames.** Every frame with `accepted === false`, in a table with its reason and a
     **Restore** action.
- Exclude (Library, Published, drawer) and Restore (drawer, Moderation) follow the same `canModerate` rule.
- A member coming online shows without reopening anything.

## Global Constraints

- Two backends in sync. No new command here: `ProjectCard` is an existing struct, and the event goes
  through the existing emitter. The web host forwards every emitted event over SSE unchanged; verify that in
  Task 2.
- Serde camelCase. TS types regenerate via `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- Never swallow errors; log first. Design tokens only. StrictMode-safe listeners. Hold listener callbacks in
  latest-value refs, so listeners subscribe once.
- Event name `collab-peers-changed`, payload `{ projectId: string }`, at most one per project per second
  (reuse the `LANDED_BURST` pattern and constant).
- Build discipline: one cargo command at a time. `rustfmt <file>`, never on crate roots. Commit trailers:
  - `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  - `Claude-Session: https://claude.ai/code/session_015RRpMcroNShaEgp8Q33fR3`
- Hub repo: `~/Documents/Projects/athenaeum-hub`, branch `holder-device-names` from `main`. Hub gate:
  `cargo test` (needs the test Postgres the hub's tests already use; see its README/CI). Deploy to the test
  hub is a separate owner-approved step (Task 7), never automatic. No prod deploy.
- App branch: `collab-smoke-fixes` from `main`.

## Review Focus

1. **A device with no name** (`devices.name` NULL). The snapshot sends `""`, and the app shows the 8-char
   short id, never the member's name and never an empty label. Pinned in Task 1 (hub test) and already
   covered in the app.
2. **A moderator who is not coordinator.** The Moderation tab, Exclude and Restore are all available. A
   plain member sees none of them. Pinned in Tasks 3 and 4.
3. **Approval off.** The Moderation tab still shows, reads `This project publishes without review.`, and
   lists excluded frames. Pinned in Task 4.
4. **A burst of presence changes** (a member's three devices come online within 200 ms). The frontend gets
   one `collab-peers-changed`, not three, and reloads once. Pinned in Tasks 2 and 5.
5. **An event for another project** re-reads nothing. Pinned in Task 5.

---

### Task 1 (hub): the holder snapshot names devices by their own name

**Files:** Modify `athenaeum-hub/src/routes/holders.rs` (the `SnapshotDeviceRow` query, ~line 274). Test:
`athenaeum-hub/tests/holders.rs`.

- [ ] **Step 1 — failing test** (append to `tests/holders.rs`, using the file's own helpers:
  `app_with_capture`, `register_device(app, mailer, email, n, device_name)`, `create_project_via`,
  `join_and_approve`, `send`, `get`, `as_json`):

```rust
#[sqlx::test]
async fn the_snapshot_names_each_device_by_its_own_name(pool: PgPool) {
    let (app, mailer) = app_with_capture(pool);
    let (coord, _) = register_device(&app, &mailer, "coord@example.com", 1, "Desktop").await;
    let (anna, anna_dev) = register_device(&app, &mailer, "anna@example.com", 2, "anna-obs").await;
    let project = create_project_via(&app, &coord, "P", false).await;
    let id = project["id"].as_str().unwrap();
    join_and_approve(&app, &coord, &anna, id, "Anna", "send_receive").await;
    let (status, body) = send(&app, get(&format!("/api/v1/projects/{id}/holders/snapshot"), Some(&coord))).await;
    assert_eq!(status, StatusCode::OK);
    let snap = as_json(&body);
    let names: Vec<&str> = snap["devices"].as_array().unwrap().iter()
        .map(|d| d["displayName"].as_str().unwrap()).collect();
    assert!(names.contains(&"anna-obs"), "{names:?}");
    assert!(names.contains(&"Desktop"), "{names:?}");
    assert!(!names.contains(&"Anna"), "a member name leaked as a device name: {names:?}");
}
```

  Adapt it to what `register_device` actually returns; the second element may not be the device. For the
  NULL case, add a second test: set one device's `name` to NULL with a direct `sqlx::query("UPDATE devices
  SET name = NULL WHERE …")` on the pool clone, and assert its `displayName == ""`.
- [ ] **Step 2 — run, see it fail:** `cargo test --test holders the_snapshot_names` (member name returned).
- [ ] **Step 3 — fix:** in the query, replace `pm.display_name` with `COALESCE(d.name, '') AS display_name`.
  Update the doc comment on `SnapshotDevice.display_name`:
  `/// The device's own name ("" when it has none) — never the member's; the member comes from the membership snapshot.`
- [ ] **Step 4 — run** the two new tests and the rest of `tests/holders.rs` and `tests/holders_read.rs` →
  all pass. Then the hub's full `cargo test`.
- [ ] **Step 5 — commit** on `holder-device-names`:
  `fix(holders): the snapshot names a device by its own name, not its member's`.

### Task 2 (core): `canModerate` on the card and a throttled `collab-peers-changed` event

**Files:**
- `crates/athenaeum-core/src/api/collab.rs`: `ProjectCard` (~136) and `card_from_row` (~1859).
- `crates/athenaeum-core/src/api/collab_live/runtime.rs`: the `FeedEffect::ProvidersChanged` arm (~1695),
  the timers, and `deadline()`.
- `src/types/models.ts`: regenerated.

**Interfaces (Produces):**
- `ProjectCard.can_moderate: bool` → TS `canModerate: boolean`. True when `is_coordinator`, or when the
  parsed `gov_caps_json` contains `"data.moderate"`. A malformed JSON is `warn!`-logged and read as `[]`.
- Event `collab-peers-changed`, payload `CollabPeersChanged { project_id: String }` → TS
  `{ projectId: string }`. Register it in `ts_export.rs`.
  - Emitted from the runtime when a `ProvidersChanged(p)` effect is applied: at most once per project per
    `LANDED_BURST` (1 s).
  - A change inside the window is emitted when the window ends. Add it to `deadline()` exactly like the
    landed bursts, and flush it in `on_timers`.
  - Add a constant `COLLAB_PEERS_CHANGED_EVENT` beside `COLLAB_FRAMES_LANDED_EVENT`.

- [ ] **Step 1 — failing tests:**
  - (a) `card_from_row` unit tests: `is_coordinator=false`, caps `["data.moderate"]` → true; caps `[]` →
    false; coordinator with `[]` → true; `"not json"` → false (with the warn).
  - (b) A runtime-level test in the style of `live_tests.rs` (an emitter that records events, e.g. the one
    `collab-frames-landed` tests use). Three `ProvidersChanged` for one project within 200 ms produce exactly
    one `collab-peers-changed` with that `projectId`. A second change 1.2 s later produces a second one.
  - If a runtime-level test is too heavy, test a small pure throttle helper instead (`PeerBurst` with
    `note(p, now)` / `due(now) -> Vec<String>`), and wire it with one live test asserting at least one event
    after a presence change. Name which option you chose in the report.
- [ ] **Step 2 — run** → fail.
- [ ] **Step 3 — implement.**
  - Check `crates/athenaeum-web` for how emitter events reach SSE clients. It is expected to forward every
    event generically; if it whitelists names, add this one there, since both hosts must deliver it.
- [ ] **Step 4 — regenerate TS; run.** Run these one at a time:
  - `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`
  - the focused tests
  - `cargo check --workspace`
  - `npx tsc --noEmit`
  - Fix any TS literal of `ProjectCard` in tests by adding `canModerate: false`, or `true` where the fixture
    is a coordinator.
- [ ] **Step 5 — commit:** `feat(collab): canModerate on the project card; collab-peers-changed when presence or holders change`.

### Task 3 (frontend): gate moderation on `canModerate`

**Files:**
- `src/pages/ProjectDetail.tsx`
- `src/components/collab/project/{MyFramesTab,LibraryTab,FrameDrawer}.tsx` and their tests.

- Replace every moderation gate that reads `c.coordinator` or `coordinator` with `c.canModerate` /
  `canModerate`:
  - the Exclude actions in Published and Library;
  - Exclude/Restore in the drawer.
- Rename the props `coordinator` → `canModerate` on `MyFramesTab`, `LibraryTab` and `FrameDrawer`, so the
  name says what it gates. Update all call sites and tests.
- The Moderation tab becomes visible when `c.canModerate`, with no `requireApproval` condition. Its badge
  stays `card.pendingFrames` (shown only when > 0).
- Tests:
  - a non-coordinator with `canModerate: true` sees the Moderation tab, the Exclude action in Library, and
    Restore in the drawer;
  - a member with `canModerate: false` sees none;
  - `coordinator: true, canModerate: true, requireApproval: false` still sees the tab.
- Gate: `npx vitest run src/pages src/components/collab`, then `npx tsc --noEmit`.
- Commit: `fix(collab): moderation follows the data.moderate capability, not the coordinator flag`.

### Task 4 (frontend): the Moderation tab — waiting for review + excluded frames

**Files:**
- Modify: `src/components/collab/project/ModerationTab.tsx`, `src/components/collab/project/frames.tsx` (a
  new `excluded` table config and an `exclusion` column), `src/pages/ProjectDetail.tsx` (pass
  `requireApproval`).
- Tests: `ModerationTab.test.tsx`, `frames.test.tsx`.

**Interfaces:**
- `TableId` gains `'excluded'`.
- `TABLES.excluded`:

  | Field | Value |
  | ---- | ---- |
  | columns | `['name', 'publisher', 'night', 'filter', 'camera', 'exp', 'exclusion', 'fwhm', 'ecc', 'size']` |
  | defaultColumns | `['name', 'publisher', 'night', 'filter', 'exp', 'exclusion']` |
  | groupings | `['publisher', 'night', 'filter', 'camera', 'none']` |
  | defaultGrouping | `['publisher', 'none']` |
  | stateFacet | `null` |
  | publisherFacet | `true` |

- `COLUMNS.exclusion`: label `Why excluded`, width 240. Its value and cell are `acceptedReason` (`—` when
  null, muted).
- `ModerationTab` props become `{ projectId, requireApproval, library, onDecided, onOpen }`.

**Layout:**
- Section **Waiting for review**:
  - approval on → today's table, unchanged;
  - approval off → the muted line `This project publishes without review.`, and no pending load call.
- Section **Excluded frames**: a `ProjectFrameTable` keyed `${projectId}.excluded` over
  `library.filter(f => !f.accepted).map(f => fromLibrary(f, new Map()))`. `list_collab_frames` includes the
  caller's own rows, so own excluded frames appear too.
  - One action: `Restore` (eligible: all). It invokes `restore_collab_frame { projectId, frameUuid }` per
    frame, sequentially.
  - On the first failure it stops with `Restored K of N — <error>` inline. Then it calls `onDecided()`,
    which makes the shell reload the frames.
  - Empty text: `No frames are excluded.`

**Tests:**
- approval off → the review line shows and `list_collab_moderation` is not invoked;
- two excluded frames and one normal frame → 2 rows, each showing its reason;
- `Restore 2` invokes `restore_collab_frame` twice and then `onDecided`;
- a failure on the second → `Restored 1 of 2 — …`;
- no excluded frames → the empty text.

Gate: vitest on the touched files, then `npx tsc --noEmit`. Commit:
`feat(collab): Moderation tab lists excluded frames with Restore; says when review is off`.

### Task 5 (frontend): presence changes refresh the page

**Files:**
- `src/pages/ProjectDetail.tsx`
- `src/components/collab/project/MembersTab.tsx` (new prop `refreshToken: number`)
- `src/components/collab/project/FrameDrawer.tsx`
- Tests for each.

**Behaviour:**
- **Shell:**
  - Listen for `collab-peers-changed` (StrictMode-safe, subscribe once, loaders in refs). For this project,
    schedule a single trailing reload 1 s after the last event:
    - `loadLibrary()` (holder online counts);
    - the member summary the shell holds;
    - bump `membersRefresh` (passed to `MembersTab` as `refreshToken`);
    - `loadOwn()`.
  - `loadOwn()` is the expensive gate read, so give it its own trailing debounce of 5 s. Put both debounce
    windows in named constants.
  - Clear the timers on unmount.
  - Events for other projects are ignored.
- **MembersTab:** re-invokes `get_collab_member_summary` whenever `refreshToken` changes. This must not
  reset its sort or expansion state, and it keeps the existing cancel guard.
- **FrameDrawer:** listens for `collab-peers-changed` for its project and re-reads
  `get_collab_frame_holders` for its `frameUuid`. The re-read keeps the current rows until the answer
  arrives, so it does not flash "Loading holders…".

**Tests (fake timers):**
- Three events within 300 ms → one `list_collab_frames` and one `get_collab_member_summary` after 1 s, and
  one `list_project_own_frames` after 5 s.
- An event for another project → nothing.
- The drawer re-reads holders on an event for its project, and the shown name changes to the new answer.
- `MembersTab` keeps its sort after a refresh.

Gate: `npx vitest run src/pages src/components/collab`, `npx tsc --noEmit`, then the full `npx vitest run`.
Commit: `feat(collab): the project page re-reads presence and holders when peers change`.

### Task 6: docs

- **Spec §15 "Amendments (smoke fixes, 2026-09-30)"**:
  1. The holder snapshot's `displayName` is the device's own name, set by the hub.
  2. Moderation, Exclude and Restore follow `canModerate` (coordinator or `data.moderate`).
  3. The Moderation tab also lists excluded frames with Restore, and says when review is off.
  4. `collab-peers-changed { projectId }` is sent at most once per project per second on presence or holder
     changes. The page re-reads after 1 s, and own frames after 5 s.
- **`docs/transfers/README.md`**: the wave-2 section gains a line each on the event and the Moderation tab.
- **`docs/superpowers/open-items.md`**: the wave 1 + 2 smoke list gains these checks:
  - device names in the drawer and in Members;
  - a moderator who is not coordinator sees Moderation;
  - a member coming online shows within about 2 s.
- **CLAUDE.md**: in the collab clause, add `canModerate` gating and the new event name. The command count is
  unchanged.
- Commit: `docs(collab): smoke-fix amendments`.

### Task 7 (owner-gated): deploy the hub fix to the test hub

- Push the hub branch and merge it to hub `main` (owner's word).
- Deploy to the **test** hub:
  `cd ~/Documents/astronet && ansible-playbook deploy_athenaeum_hub.yml -e hub_target=athenaeum_hub_test -e hub_artifact_ref=<ref> -e @~/.config/athenaeum-hub/smtp.yml`.
  1Password prompts on the owner's machine.
- Afterwards, check the health endpoint and that a snapshot names a device by its own name.
- Existing app caches pick up the new names on their next holder-snapshot read (a new session, or a
  resync). Say so in the report.
- No prod deploy.
