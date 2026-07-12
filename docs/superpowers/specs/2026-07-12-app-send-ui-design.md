# App→App Send UI (Phase 3) — Design — 2026-07-12

**Status:** design (brainstormed with owner 2026-07-12). **Repos touched:** `athenaeum` (React frontend + two thin `athenaeum-core`/tauri/web command additions). **Builds on:** the explicit-target send primitive (Plan 2C `enqueue_sync_selection({frameIds, destinationDeviceId})` + `resolve_dest_node`), the mirror landing (Plan 2B), and the dedup handshake (Plan 3, whose `{new,duplicate}` the app already receives on `sync-finished`).

This is **Phase 3** — the last piece of the sync feature: the desktop app's operator-facing "send frames to another node" UI. Phase 1 deliberately left the app with no send UI (`useSyncSend.canSend` hard-`false`).

---

## 1. Goal

Let the operator, from the app, **select frames (from a frame table) or files/a folder (from the dual-pane browser), pick one or more destination nodes, and send** — reusing the done explicit-target primitive + Plan-3 dedup, with outbound status surfaced in the existing Transfers panel. The whole send path (transport, dedup, mirror-landing, per-peer engines) already exists; this phase is the selection → destination-picker → enqueue UI on top, plus two small backend helpers.

Non-goals (v1): recursive folder send beyond a `files.path` prefix query; a dedicated outbound-queue view (reuse Transfers); scheduling/auto-send (that's Perseus); wiring the "Send to…" action into every frame table (v1 does the browser + `LightsAnalysisView`; the dialog is reusable for the rest later).

---

## 2. Resolved decisions (owner, 2026-07-12)

1. **One reusable `SendToNodeDialog`, two entry points:** the dual-pane file browser (select files/folder) and the frame tables (starting with `LightsAnalysisView`, the existing dormant host). Other frame tables adopt the dialog trivially later.
2. **Multiple destinations per send:** the dialog multi-selects destination nodes and enqueues to each (checkboxes → "Send to N nodes"). The per-peer sender runtime + Plan-3 dedup already support this; each destination is a separate `enqueue_sync_selection` → its own package (the app engines are un-sinked, so no shared-package coordinator is involved).

---

## 3. Architecture

Everything routes through the existing primitive; the new pieces are a reusable dialog, a revived hook, two entry points, and two thin backend helpers.

```
Frame table (selectedFrameIds: Set<number>) ──┐
                                              ├──► SendToNodeDialog ──► useSyncSend.sendSelection(frameIds, deviceIds[])
Dual-pane browser (selected paths) ──►         │        │                    │  for each deviceId:
  resolve_frame_ids_for_paths(paths) ─► frameIds┘        │                    │    api.invoke('enqueue_sync_selection', { frameIds, destinationDeviceId })
                                                          │                    │  → EnqueueSelectionResult per dest → aggregate → notify()
                                          list_account_devices + account_status│
                                          → athenaeum devices, minus self      ▼
                                                                    per-peer engine (Plan 2C) → Plan-3 negotiate → mirror-land on receiver
                                                                                 │
                                        Transfers panel (Active tab) ◄──── get_sync_status / sync-progress / sync-finished ({new,duplicate})
```

**Component boundaries:**
- **`SendToNodeDialog`** (new, `src/components/transfers/` or `src/components/send/`) — self-contained: loads destinations, multi-selects, enqueues per destination, reports the aggregate result. Given a `frameIds: number[]`. Reusable by any caller.
- **`useSyncSend`** (revive `src/hooks/useSyncSend.ts`) — `sendSelection(frameIds, deviceIds)` loops the primitive; `canSend` reflects "signed in + ≥1 athenaeum destination". Drops the "primary" framing.
- **Destination source** — a small internal loader in the dialog/hook calling `list_account_devices` + `account_status` directly via `api.invoke` (NOT `useAccount` — the A2 isolation guard forbids importing `useAccount`/`AccountSection` outside Settings; the `useSyncSend`/`SyncSection` precedent reads the offline commands directly).
- **`resolve_frame_ids_for_paths`** (new backend command) — the browser's path selection → frame ids.
- **`build_sender_status` fix** — dedup the N-counted active rows (multi-peer correctness).

---

## 4. The `SendToNodeDialog`

- **Input:** `frameIds: number[]` (already resolved by the caller) + open/close control.
- **Destinations:** on open, `api.invoke<AccountDevice[]>('list_account_devices')` + `api.invoke<AccountStatus>('account_status')`; the candidate list = devices with `capability === 'athenaeum'` and `id !== status.deviceId` (self). Signed-out or zero-candidate → an explanatory empty state (no send). The backend independently re-validates and rejects Perseus/self in `resolve_dest_node`.
- **Selection:** a checkbox list of candidate nodes (name + short id + last-seen); "Send to N nodes" button enabled when ≥1 node checked and `frameIds` non-empty.
- **Send:** `useSyncSend.sendSelection(frameIds, checkedIds)` → for each destination, one `enqueue_sync_selection({ frameIds, destinationDeviceId })`; collect each `EnqueueSelectionResult`.
- **Result + notify:** aggregate across destinations — total queued, and the `ineligible` reasons (grouped, capped, e.g. "3 not on disk"). One `notify({ kind: 'sync', tone })`: success ("Queued N frames to M nodes"), partial ("Queued N of T — …"), or all-ineligible (warning). A per-destination failure (e.g. `resolve_dest_node` rejects, offline) is reported without aborting the others. Close on completion; the outbound rows then appear in the Transfers panel.

---

## 5. Entry points

**Frame tables — `LightsAnalysisView.tsx`.** Replace the dormant "Send to primary" button (currently gated by `canSend===false`) with a "Send to…" button (enabled when `selectedFrameIds` non-empty) that opens `SendToNodeDialog` with `[...selectedFrameIds]`. Remove the "primary" framing. (Pattern documented so Blink/duplicates/etc. adopt it later — out of v1 scope.)

**Dual-pane browser — `DualPaneFileBrowser.tsx`.** A "Send to…" action on the active pane's selection (`Set<string>` of paths). On invoke: `api.invoke<number[]>('resolve_frame_ids_for_paths', { paths: [...selection] })` → open `SendToNodeDialog` with the returned frame ids. Empty resolution (no cataloged frames under the selection) → an inline message, no dialog. Works for both selected files and a selected folder (the resolver recurses via `files.path` prefix).

---

## 6. Backend additions (thin; core logic reused)

1. **`resolve_frame_ids_for_paths(paths: Vec<String>) -> Vec<i64>`** — `athenaeum-core/src/api/` handler + Tauri command + Axum route. For each path: an exact `files.path` match → its frame id; a directory → every frame whose `files.path` is under `path` + separator (a `LIKE 'path/%'` prefix query, using the same `path`-vs-`/private` normalization the file-op layer already applies). De-duplicated, order-stable. Reuses/extends the existing `get_frame_ids_for_file_ids` (`db/operations.rs`) or a direct prefix query. Frames whose file has no catalog frame row are skipped (nothing to send).
2. **`build_sender_status` fix** (`athenaeum-core/src/api/sync.rs`) — the per-peer engines each open a `CatalogSyncStore` over the shared catalog DB and `non_terminal()` has no peer filter, so with N started engines the `active` list + `queued`/`transferring` counts are N-duplicated. Fix: **dedup `active_rows` by `row.id`** before building the rollup (and count `queued`/`transferring` from the deduped set). Terminal totals (from a single DB read) are already correct. Add a regression test with two started peers.

`enqueue_sync_selection` (the command) and `resolve_dest_node` are unchanged — already explicit-target and Perseus-rejecting.

---

## 7. Outbound status & dedup outcome

- **Transfers panel** (`TransfersPanel.tsx` Active tab) already renders per-destination outbound rows (`OutboundSummary { packageShort, peerShort, state, attempts }`) from `get_sync_status → sender.active`, updated by the shared `useSyncStatus` poller + `sync-progress`/`sync-finished`. A Phase-3 send appears automatically once enqueued (the engine creates the durable `sync_outbound` row; `sender.started` flips true so the panel becomes `visible`). **No new queue view.**
- **`{new,duplicate}`** — the app's `useSyncStatus` already listens to `sync-finished`, whose payload carries `newCount`/`duplicateCount` (Plan 3). Surface them in the send-finished notification ("Delivered: N new, M already there") and, where the history row exposes it, in the Transfers history. This is the app-side payoff of Plan 3 — trivial here because the app receives the event (Perseus did not).

---

## 8. Error handling & edge cases

- **Signed out / no athenaeum destinations:** the dialog shows an explanatory empty state; the entry-point buttons stay enabled but the dialog can't send (or the button tooltip explains). No crash.
- **Per-destination failure** (offline peer, `resolve_dest_node` reject, a revoked device): reported in the aggregate result; the other destinations still enqueue. The engine retries transport-level failures.
- **Ineligible frames** (missing on disk, not resolvable): surfaced from `EnqueueSelectionResult.ineligible` — never silently dropped.
- **Empty selection / empty resolution:** the send button is disabled / an inline message; no empty enqueue.
- **Multi-peer status:** the `build_sender_status` dedup keeps the Transfers Active tab honest with ≥2 destinations.
- **A2 isolation:** the dialog/hook must NOT import `useAccount`/`AccountSection` (grep-guarded); it reads `list_account_devices`/`account_status` via `api.invoke` directly.

---

## 9. Testing strategy

- **Backend:** `resolve_frame_ids_for_paths` — a file path → its frame id; a folder path → all frames under it (prefix), recursively; a non-cataloged path → empty; de-dup + order. `build_sender_status` — two started peers with overlapping non-terminal rows → `active` deduped by id, `queued`/`transferring` counted once (regression test).
- **Frontend:** if a runner exists (check `package.json`), test the dialog's candidate filtering (athenaeum-only, minus self) and the per-destination aggregate; else `npx tsc --noEmit` + a manual-render checklist. The two-backend command mirror (Tauri + Axum) is verified by build.
- Gates: `cargo test -p athenaeum-core` + `cargo build --workspace` + `npx tsc --noEmit`.

---

## 10. Out of scope / follow-ups

- Wiring "Send to…" into every frame table (v1 = browser + `LightsAnalysisView`; the dialog is reusable).
- A dedicated send-queue view (reuse Transfers).
- Cross-account send (Stage II collaboration, separate).
- Persisting/aggregating `{new,duplicate}` into the Transfers *history* rows if the current history shape doesn't already carry them (the live notification path covers the primary need).
