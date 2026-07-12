# App→App Send UI (Phase 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the operator select frames (frame table) or files/folder (dual-pane browser), pick one or more destination nodes, and send — via the existing explicit-target primitive.

**Architecture:** A reusable `SendToNodeDialog` (loads the account's `athenaeum` devices, multi-selects, enqueues to each via `enqueue_sync_selection`) fed by two entry points (the dual-pane browser via a new `resolve_frame_ids_for_paths` command, and `LightsAnalysisView`'s revived send button). A revived `useSyncSend.sendSelection` loops the primitive; the Transfers panel already surfaces outbound status. Plus a `build_sender_status` dedup fix for multi-peer correctness.

**Tech Stack:** React/TS frontend (Tauri IPC via `src/api`), `athenaeum-core` (SQLite catalog) + Tauri + Axum for the two thin commands.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-07-12-app-send-ui-design.md`.
- **The send primitive is DONE — do not change it:** `enqueue_sync_selection({ frameIds, destinationDeviceId }) -> EnqueueSelectionResult` (Tauri `commands/sync.rs`, Axum `routes/sync.rs`, core `api/sync.rs`), and `resolve_dest_node` (rejects Perseus + self). One destination per call — the dialog loops for multiple.
- **Two backends in sync:** the new `resolve_frame_ids_for_paths` gets a Tauri command AND an Axum route in the same task; real logic in `athenaeum-core`.
- **A2 account isolation (grep-guarded):** the dialog/hook MUST NOT import `useAccount`/`AccountSection` (the guard `grep -riE "useAccount|AccountSection" src/pages src/components --include=*.tsx | grep -vi settings` must stay empty). Read destinations via `api.invoke('list_account_devices')` + `api.invoke('account_status')` directly (the `useSyncSend`/`SyncSection` precedent).
- **Valid destinations** = `AccountDevice[]` with `capability === 'athenaeum'` and `id !== accountStatus.deviceId` (self). The backend re-validates (rejects Perseus/self) in `resolve_dest_node`.
- **Design tokens** (`bg-surface`, `text-content-muted`, `bg-accent`, `text-error`, …); `lucide-react` icons; notifications via `notify()` from `useNotifications()` (kind `'sync'`) — never ad-hoc toasts.
- **Serde boundary** snake_case ↔ camelCase; `#[serde(rename_all = "camelCase")]`; new model types go through `ts_export.rs` if returned as a struct (here the command returns `Vec<i64>` — no new model type).
- **Author every commit as `eg013ra1n <vilen.sharifov@gmail.com>`; no Claude footer.** GitLab only. Branch `0.4.0`.
- **Gates:** backend tasks — `cargo test -p athenaeum-core` + `cargo build --workspace`; frontend tasks — `npx tsc --noEmit` (no JS test runner — verify with `package.json`; if absent, gate is tsc + a manual-render checklist).

---

### Task 1: Backend — `resolve_frame_ids_for_paths`

Map a set of selected paths (files or folders) to catalog frame ids, so the dual-pane browser can send by path.

**Files:**
- Create/Modify: `crates/athenaeum-core/src/api/files.rs` (or the closest existing `api` module for catalog queries) — the handler
- Modify: `crates/athenaeum-core/src/db/operations.rs` (a `frame_ids_under_paths` query if not reusing an existing one)
- Modify: `crates/athenaeum-tauri/src/commands/files.rs` (Tauri command) + `crates/athenaeum-tauri/src/lib.rs` (register)
- Modify: `crates/athenaeum-web/src/routes/files.rs` (Axum route) + `crates/athenaeum-web/src/routes/mod.rs` (register)
- Test: `crates/athenaeum-core/src/db/operations.rs` (or `api/files.rs`) inline

**Interfaces:**
- Produces:
  ```rust
  // db/operations.rs — for each path: exact file match OR files.path under "<path>/". DISTINCT frame ids.
  pub fn frame_ids_under_paths(conn: &rusqlite::Connection, paths: &[String]) -> rusqlite::Result<Vec<i64>>;
  // api/files.rs (or wherever catalog api handlers live)
  pub fn resolve_frame_ids_for_paths(ctx: &ServiceContext, paths: Vec<String>) -> Result<Vec<i64>, ApiError>;
  // Tauri: async fn resolve_frame_ids_for_paths(state, paths: Vec<String>) -> Result<Vec<i64>, String>
  // Axum: body { paths: Vec<String> } -> Json<Vec<i64>>
  ```

- [ ] **Step 1: Write the failing test**

```rust
// operations.rs tests — build a temp catalog with files+frames at known paths.
#[test]
fn frame_ids_under_paths_matches_file_and_folder() {
    let conn = test_conn_with_frames(&[
        ("/data/astro/M31/2026-07-12/L_0001.fits", 10),   // (path, frame_id)
        ("/data/astro/M31/2026-07-12/L_0002.fits", 11),
        ("/data/astro/M31/flats/F_0001.fits", 12),
        ("/data/other/x.fits", 13),
    ]);
    // exact file → its frame
    assert_eq!(frame_ids_under_paths(&conn, &["/data/astro/M31/2026-07-12/L_0001.fits".into()]).unwrap(), vec![10]);
    // folder → all frames under it (recursive), sorted/deduped
    let mut got = frame_ids_under_paths(&conn, &["/data/astro/M31".into()]).unwrap();
    got.sort();
    assert_eq!(got, vec![10, 11, 12]);
    // a file + an overlapping folder → deduped
    let mut both = frame_ids_under_paths(&conn, &["/data/astro/M31/2026-07-12/L_0001.fits".into(), "/data/astro/M31".into()]).unwrap();
    both.sort();
    assert_eq!(both, vec![10, 11, 12]);
    // non-cataloged path → empty
    assert!(frame_ids_under_paths(&conn, &["/nope".into()]).unwrap().is_empty());
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib frame_ids_under_paths_matches_file_and_folder` → FAIL (fn absent).

- [ ] **Step 3: Implement** —
  - `frame_ids_under_paths`: for each path, run `SELECT DISTINCT fr.id FROM files f JOIN frames fr ON fr.file_id = f.id WHERE f.path = ?1 OR f.path LIKE ?2` with `?2 = format!("{}/%", path.trim_end_matches('/'))`; collect into an ordered de-dup set (a `BTreeSet<i64>` or a `Vec` + `dedup`). Escape `%`/`_` in the LIKE prefix is unnecessary for real paths but use `ESCAPE` if the codebase already does; otherwise a plain LIKE is fine (paths don't contain `%`). If the app stores macOS `/Volumes` vs `/private/Volumes` variants (the file-op layer normalizes — check `get_file_by_path`), match the stored form; do not silently miss (a comment noting the assumption is fine).
  - `resolve_frame_ids_for_paths` (api): `db(ctx)?`, `conn`, delegate; `.map_err(|e| e.to_string())` at the boundary.
  - Tauri command `#[tracing::instrument(skip_all, err)]` + Axum route (body `{ paths }`), both registered.

- [ ] **Step 4: Run** — the test + `cargo test -p athenaeum-core` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core crates/athenaeum-tauri crates/athenaeum-web
git commit -m "feat(sync): resolve_frame_ids_for_paths — map selected files/folders to catalog frame ids"
```

---

### Task 2: Backend — fix `build_sender_status` N-count

The per-peer engines each read the shared `sync_outbound` with no peer filter, so with N started peers the `active` list + `queued`/`transferring` counts are N-duplicated. Dedup by row id.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs` (`build_sender_status`, ~L420-472)
- Test: `crates/athenaeum-core/src/api/sync.rs` inline

**Interfaces:**
- Consumes: `SyncSenderRuntime` (`HashMap<NodeId, StartedSender>`), each engine's `status_snapshot() -> Vec<OutboundSummary>`.
- Produces: `build_sender_status` returns an `active` list deduped by `OutboundSummary.id`, with `queued`/`transferring` counted from the deduped set.

- [ ] **Step 1: Write the failing test**

```rust
// api/sync.rs tests — two started peers over the SAME catalog store, each snapshot returns the same non-terminal rows.
#[tokio::test]
async fn sender_status_dedupes_active_rows_across_peers() {
    let sender = SyncSenderRuntime::new();
    // insert two StartedSender entries whose engines share a store holding 2 non-terminal rows (ids 1,2)
    // (reuse the loopback StartedSender test helper; both engines read the same CatalogSyncStore)
    let status = build_sender_status(&sender).await.unwrap();
    assert_eq!(status.active.len(), 2, "2 distinct rows, not 4 (deduped across the 2 peers)");
    assert_eq!(status.queued + status.transferring, 2);
}
```
(If the existing test harness can't easily share one store between two `StartedSender`s, factor the dedup into a pure `fn dedup_active(rows: Vec<OutboundSummary>) -> Vec<OutboundSummary>` and unit-test THAT directly with duplicate ids — plus keep `build_sender_status` calling it.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib sender_status_dedupes_active_rows_across_peers` → FAIL (rows N-duplicated).

- [ ] **Step 3: Implement** — after collecting `active_rows` across `guard.values()`, dedup by `id` (e.g. a `HashSet<i64>` seen-set preserving first occurrence, or collect into a `BTreeMap<i64, OutboundSummary>` then values). Compute `queued`/`transferring` from the deduped list. `confirmed_total`/`failed_total` stay as the single-DB-read counts (already correct). Keep the ordering stable (by id).

- [ ] **Step 4: Run** — the test + `cargo test -p athenaeum-core` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/sync.rs
git commit -m "fix(sync): dedupe sender-status active rows across per-peer engines (multi-destination)"
```

---

### Task 3: Frontend — revive `useSyncSend` + `SendToNodeDialog`

The reusable dialog + the hook that loops the primitive over the chosen destinations.

**Files:**
- Modify: `src/hooks/useSyncSend.ts` (replace dormant `sendToPrimary` with `sendSelection`; `canSend` from signed-in + destinations)
- Create: `src/components/transfers/SendToNodeDialog.tsx`
- Test: `npx tsc --noEmit` + manual (no JS runner — confirm via `package.json`)

**Interfaces:**
- Consumes: `api.invoke<EnqueueSelectionResult>('enqueue_sync_selection', { frameIds, destinationDeviceId })`; `api.invoke<AccountDevice[]>('list_account_devices')`; `api.invoke<AccountStatus>('account_status')`; `notify()` from `useNotifications()`; types `AccountDevice`/`AccountStatus`/`EnqueueSelectionResult`/`IneligibleFrame` from `src/types/models.ts`.
- Produces:
  ```ts
  // useSyncSend.ts
  interface UseSyncSend {
    sending: boolean;
    // enqueue frameIds to EACH deviceId; returns the per-destination results
    sendSelection: (frameIds: number[], deviceIds: string[]) => Promise<{ deviceId: string; result?: EnqueueSelectionResult; error?: string }[]>;
  }
  // SendToNodeDialog.tsx
  function SendToNodeDialog(props: { frameIds: number[]; open: boolean; onClose: () => void }): JSX.Element;
  ```

- [ ] **Step 1: Confirm current tsc is clean** — `npx tsc --noEmit` (baseline 0 errors).

- [ ] **Step 2: Implement `useSyncSend.sendSelection`** — remove the `canSend===false` dormancy and the `sendToPrimary` `{ frameIds }`-only call. New `sendSelection(frameIds, deviceIds)`: set `sending`, then for each `deviceId` `await api.invoke<EnqueueSelectionResult>('enqueue_sync_selection', { frameIds, destinationDeviceId: deviceId })` in a `try/catch` collecting `{ deviceId, result }` or `{ deviceId, error: errMsg(e) }`; clear `sending`; return the array. Do NOT notify here (the dialog aggregates + notifies).

- [ ] **Step 3: Implement `SendToNodeDialog`** — on `open`, load destinations: `Promise.all([api.invoke<AccountDevice[]>('list_account_devices'), api.invoke<AccountStatus>('account_status')])`; candidates = `devices.filter(d => d.capability === 'athenaeum' && d.id !== status.deviceId)`. States: loading, signed-out/empty (`status.signedIn === false` or zero candidates → explanatory message, no send), list. Render a checkbox list (name + `shortId(id)` + last-seen); a "Send to N nodes" button enabled when ≥1 checked and `frameIds.length > 0`. On send: `const results = await sendSelection(frameIds, checkedIds)`; aggregate — `queued = Σ result.enqueuedCount`, `total = frameIds.length`, per-destination failures from `results.filter(r => r.error)`, grouped `ineligible` reasons (reuse the existing `summarizeIneligible` idea from the old `useSyncSend`); one `notify({ kind:'sync', tone: allOk ? 'success' : 'warning', title, detail })` (e.g. "Queued 30 frames to 2 nodes" / "Queued 27 of 30 — 3 not on disk; nas-01 offline"); `onClose()`. Design tokens + `lucide-react` (`Send`, `Check`); a StrictMode-safe load (cancelled-flag on the effect). Do NOT import `useAccount`.

- [ ] **Step 4: Run** — `npx tsc --noEmit` clean. Manual-render note in the report (dialog loads destinations, filters to athenaeum-minus-self, multi-select enables the button, send notifies + closes).

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useSyncSend.ts src/components/transfers/SendToNodeDialog.tsx
git commit -m "feat(ui): SendToNodeDialog + useSyncSend.sendSelection — multi-destination explicit send"
```

---

### Task 4: Frontend — entry point in `LightsAnalysisView`

Revive the dormant send button into a "Send to…" that opens the dialog with the selected frames.

**Files:**
- Modify: `src/components/LightsAnalysisView.tsx` (the `useSyncSend`/`handleSendToPrimary` block ~L520-525 + the button ~L829-845)
- Test: `npx tsc --noEmit` + manual

**Interfaces:**
- Consumes: `SendToNodeDialog` (Task 3); the existing `selectedFrameIds: Set<number>` (L74).

- [ ] **Step 1: Confirm tsc clean** — `npx tsc --noEmit`.

- [ ] **Step 2: Implement** — remove the old `useSyncSend()`/`canSend`/`handleSendToPrimary` "primary" wiring. Add local `const [sendOpen, setSendOpen] = useState(false)`. Replace the "Send to primary" button with a "Send to…" button (`lucide` `Send`, design tokens) enabled when `selectedFrameIds.size > 0`, onClick `setSendOpen(true)`. Render `<SendToNodeDialog frameIds={[...selectedFrameIds]} open={sendOpen} onClose={() => setSendOpen(false)} />`. Keep the `(N of M)` selection-count idiom the view already uses if present.

- [ ] **Step 3: Run** — `npx tsc --noEmit` clean; manual: button disabled with no selection, opens the dialog with the selected frames.

- [ ] **Step 4: Commit**

```bash
git add src/components/LightsAnalysisView.tsx
git commit -m "feat(ui): Send to… from the frame table (LightsAnalysisView) via SendToNodeDialog"
```

---

### Task 5: Frontend — entry point in the dual-pane browser

Send selected files/folders: resolve paths → frame ids, then open the dialog.

**Files:**
- Modify: `src/components/dualpane/DualPaneFileBrowser.tsx` (a "Send to…" action on the active-pane selection)
- Test: `npx tsc --noEmit` + manual

**Interfaces:**
- Consumes: `api.invoke<number[]>('resolve_frame_ids_for_paths', { paths })` (Task 1); `SendToNodeDialog` (Task 3); the active pane's `selection: Set<string>` (paths).

- [ ] **Step 1: Confirm tsc clean** — `npx tsc --noEmit`.

- [ ] **Step 2: Implement** — add a "Send to…" toolbar/context action on the active pane, enabled when the selection is non-empty. onClick: `const frameIds = await api.invoke<number[]>('resolve_frame_ids_for_paths', { paths: [...selection] });` — if `frameIds.length === 0` → an inline notice ("No cataloged frames in the selection"), do not open; else `setSendFrameIds(frameIds); setSendOpen(true)`. Render `<SendToNodeDialog frameIds={sendFrameIds} open={sendOpen} onClose={...} />`. Match the browser's existing action/toolbar idiom (Move/Delete/Rename); `lucide` `Send`; design tokens; StrictMode-safe async (cancelled-flag).

- [ ] **Step 3: Run** — `npx tsc --noEmit` clean; manual: select files/a folder → "Send to…" resolves + opens the dialog; a non-cataloged selection shows the inline notice.

- [ ] **Step 4: Commit**

```bash
git add src/components/dualpane/DualPaneFileBrowser.tsx
git commit -m "feat(ui): Send to… from the dual-pane browser (files/folder → resolve → SendToNodeDialog)"
```

---

## Self-Review

**Spec coverage:** §2 reusable dialog + 2 entry points → Tasks 3 (dialog) + 4 (frame table) + 5 (browser). §2 multi-destination → Task 3 (`sendSelection` loops; checkbox multi-select). §4 dialog (candidate filter, per-dest enqueue, aggregate/notify) → Task 3. §5 entry points → Tasks 4, 5. §6.1 `resolve_frame_ids_for_paths` → Task 1. §6.2 `build_sender_status` fix → Task 2. §7 Transfers reuse (no new view) + `{new,duplicate}` — Transfers already surfaces outbound; `{new,duplicate}` rides the existing `useSyncStatus` `sync-finished` notification (no task needed; note in the Task-3 report that the send-finished notification already carries them via `useSyncStatus`, and only add to it if the current notification omits the counts). §8 edge cases: signed-out/empty (Task 3 state), per-dest failure (Task 3 aggregate), ineligible (Task 3 from `EnqueueSelectionResult.ineligible`), empty (Tasks 3/4/5 disable), A2 isolation (Task 3 direct `api.invoke`).

**Placeholder scan:** the resolver query is spelled out; the dialog logic names the exact commands + filter; the entry points reference existing selection state (`selectedFrameIds`, `selection`). The frontend "test" is honestly tsc + manual (no JS runner) rather than a fabricated harness. No TODOs.

**Type consistency:** `resolve_frame_ids_for_paths(paths) -> number[]` (Task 1) is called only in Task 5. `sendSelection(frameIds, deviceIds) -> {deviceId,result?,error?}[]` (Task 3) is called only by `SendToNodeDialog` (Task 3). `SendToNodeDialog({ frameIds, open, onClose })` (Task 3) is consumed by Tasks 4 + 5 with the same prop shape. `enqueue_sync_selection({ frameIds, destinationDeviceId })` + `EnqueueSelectionResult` are the unchanged primitive.

**Ordering:** 1 (resolver) → 2 (status fix) → 3 (hook + dialog) → 4 (frame-table entry, uses 3) → 5 (browser entry, uses 1 + 3). Backend ends green on `athenaeum-core` + workspace; frontend ends green on `tsc`.

---

## Execution Handoff

Execute with **superpowers:subagent-driven-development** (owner's standing choice): rust-engineer + opus for Tasks 1–2, frontend-dev + opus for Tasks 3–5, opus reviewer after each, broad review at the end. Ledger: `.superpowers/sdd/progress-p3.md`. No deploy (rides the held joint-hub ship; adds no hub dependency).
