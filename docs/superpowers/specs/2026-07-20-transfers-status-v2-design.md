# Transfers Status Model v2 — Design

**Date:** 2026-07-20 · **Branch:** `0.5.0` · **Scope:** device-to-device Transfers only (collab migrates later)

## Problem

The 2026-07-20 smoke showed the Transfers UX blocks even smoke testing:

1. Sender sees per-file status; receiver sees nothing until fetch succeeds (announce carries only count + bytes + root hash).
2. No directory hierarchy anywhere; object sends should follow the export structure.
3. Receiver-side decline leaves no history trace (silent cancel).
4. Resumed transfers lose per-file status; per-file status is ephemeral (live events only).
5. Active views show the peer node-id hex instead of the device name.
6. Batches are unnamed (raw UUID dir basename in the UI).
7. Retry errors stick ("transferring · retrying · Peer didn't respond — will keep retrying") even after recovery — the frontend gates the reason line on the monotonic `attempts` counter.

Root causes confirmed in code: flat `rel_path` at `api/sync.rs` (`unique_rel_path`), no batch-name field, announce without a manifest, per-file bytes only via `sync-file-progress` events, cancel epilogue writing receipts but no receiver history, `showReason = lastError && (terminal || attempts > 0)` in `ActiveTransferRow.tsx`.

## Reference model (torrent-client review)

qBittorrent / Transmission / Deluge / µTorrent + Syncthing / Resilio converge on:

- **State ⊥ error**: a small set of mutually-exclusive lifecycle states, plus a separate optional error attribute (Transmission's `error`/`errorString` beside a live status). An error is a badge on a live state, never a replacement, and auto-clears on the next successful activity.
- **Stalled is benign**: "wants to transfer, nothing moving right now" is a neutral, self-healing state — never red, never terminal.
- **Per-file detail is a drill-in Files tab with a directory tree**; the main list is one row per batch. Receiver gets the same tree as the sender.
- **Connection noise goes to a log** (qBittorrent Execution Log / Transmission message log), never the main list.
- **The unit of naming is the batch root**; provisional label until metadata arrives; local rename allowed.
- **Completed is a filter over the same list**, not another screen.
- **Device ID is keyed on, the human label is displayed** (Syncthing); the raw ID lives only in details.
- ETA degrades gracefully (∞/blank), progress is "N of M files · X of Y bytes", failure is per-item, not per-batch.

## Owner decisions

Full redesign; own devices only; auto-accept stays and decline/cancel becomes a first-class outcome recorded in BOTH histories; WBPP structure for object sends + source-relative structure for browser sends; batch name = auto + editable field in the send dialog, name travels in the manifest; manifest embedded in the announce; master-detail torrent-style UI.

## Protocol-layer verification (pre-plan rule for iroh-based work)

- `Msg` evolution is append-only (postcard indices frozen, `proto.rs:92-99`; the collab variants were added the same way).
- `MAX_CONTROL_BYTES = 16 MiB` (`iroh/mod.rs:112`) — a manifest of thousands of files fits with headroom; `Msg::Offer` already carries per-file lists of the same magnitude.
- No new iroh/iroh-blobs semantics needed: the design stays in our proto layer + existing `ServeFileProgress` / `FetchEvent::File` machinery.
- Compatibility: v1 `Announce` decode is kept (Perseus beta.3 is send-only and keeps working). A 0.5.0 sender to a beta.3 app receiver requires the receiver to update — release-notes item "update all devices".

## Design

### D1. Batch — a named entity

- `display_name` on `sync_outbound` / `sync_inbound`; history rows get `batch_name`.
- Auto-name: object send → frame-set name; browser folder → common root dir name; loose files → `N files — YYYY-MM-DD HH:MM`. Editable, pre-filled field in `SendToNodeDialog`.
- The name travels in `AnnounceV2` — the receiver shows the same name. UUIDs appear only in the Details tab.

### D2. Directory tree

- Object send → `rel_path` follows the WBPP hierarchy (reuse `export/file_organizer.rs::organize_files_wbpp` as a pure path computation, no copying): `<frame_set>/camera_<instrume>/[BIAS_/DARKS_/FLAT_]/lights/<file>`.
- Browser send → paths relative to the common root of the selection (as on disk).
- Receiver lands files at `<incoming_root>/<sender_slug>/<batch_display_name sanitized>/<rel_path>`; batch-name collision → `_2`, `_3` suffix.

### D3. Wire: `AnnounceV2`

- New variant `Msg::AnnounceV2(PackageAnnounceV2)` appended at the END of the enum: `{ package_id, root_hash, byte_size, frame_count, batch_name: String, files: Vec<AnnounceFileEntry { rel_path, byte_size, frame_uuid }> }`.
- Receiver handles v1 and v2; v1 → no name/list (current fallback behavior). The app sender emits only v2.
- Golden tests (`sharing/wire_golden_tests.rs`) gain v2 pins; existing pinned bytes are untouched.

### D4. Per-file state — persisted, both sides

- New tables:
  - `sync_outbound_files(outbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at)`
  - `sync_inbound_files(inbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at)`
- Outbound rows are created at package build; inbound rows AT ANNOUNCE TIME (from the v2 manifest).
- State transitions are persisted; byte progress stays event-driven but is checkpointed on transitions (+ `ServeComplete` / `FetchEvent::File` done). After restart/resume the per-file picture restores from the DB.
- `list_transfer_files` always returns `rel_path` + persisted state (including announced/stalled batches).

### D5. State ⊥ error

- Batch states: sender `queued → preparing → transferring → uploaded → confirmed | cancelled | failed`; receiver `announced → fetching → ingesting → done | cancelled | failed`. `failed` is ONLY local-fatal (package dir gone, etc.); the network never fails a batch (delivery-forever preserved).
- `uploaded` is persisted (use the reserved `Delivered` state; shown as "uploaded — awaiting confirmation"; survives restart; the receiver ack remains the only delivery truth).
- Presentation state **`waiting`** (= stalled): derived from `next_retry_at != null` — neutral styling, "waiting for peer — retry in mm:ss" countdown, NOT an error. Self-clears: any successful step (announce ok, serve start, ack) clears `last_error` + `next_retry_at`.
- Backend additionally clears `last_error` on the first successful serve tick (today: only announce/confirm).
- Frontend: single `displayState(state, nextRetryAt, liveActivity)` mapper; the `attempts > 0` gate is removed; the reason text is visible only while a retry is genuinely pending.

### D6. Decline/cancel — a first-class outcome

- The receiver's cancel epilogue writes receiver-side history rows (`cancelled`, with batch name and the count of files that made it) — today it writes only receipts.
- The sender's "cancelled by receiver" already exists; both sides see who/when/how many files arrived.

### D7. Transfer log (three detail tiers)

- Tier 1 — list row: current state + optional error badge only.
- Tier 2 — detail pane: Files / Log / Details.
- Tier 3 — new capped table `sync_events(batch_key, direction, ts, kind, detail)`: announce sent/received, dial failed `class:…`, retry scheduled (rung N), serve start, uploaded, ack received, fetch start/done, ingest done, cancel, reject. Cap ~200 rows per batch, pruned with the batch. Surfaced in the detail pane's **Log** tab. A connection error is a timestamped log line, not a permanent status.

### D8. UI: master-detail (Nord tokens)

- List: one row per batch — direction, **batch name**, **device name** (join `get_sync_device_names`; hex only in Details), state chip + error badge, progress "N of M files · X/Y GB", speed, ETA (∞/blank when unestimable).
- Filter chips with live counts: All / Sending / Receiving / Waiting / Completed / Failed. **Completed is a filter over the same list** (history merges in) — nothing disappears.
- Detail pane (row selection): **Files** — directory tree with per-file bars/chips (collapsible folders); **Log** — `sync_events` journal; **Details** — uuid, paths, timings, peer id.
- Slide-over `TransfersPanel` — compact mini-rows of the same model; "Open full screen".
- `SendToNodeDialog` gains a "Transfer name" field (pre-filled with the auto-name).
- Notifications unchanged (outcome-only, deduped by operation id).

### D9. Out of scope

- The first-contact-stall transport fix (separate queued cycle; the new model at least shows it honestly as `waiting`).
- Collab-exchange migration to the new model; Perseus UI.
- Any change to delivery-forever / dedup-handshake semantics.

## Testing

Unit: store migrations/pruning, WBPP + source-relative rel_path computation, announce v1/v2 handling, `waiting` derivation, error-clear on recovery. Golden wire pins. E2e (loopback): manifest visible pre-fetch; restart/resume restores per-file state; receiver cancel appears in both histories; ack-timeout → waiting → recovery leaves no sticky error; structured rel_paths land correctly. Live two-instance smoke per the verification section of the plan.
