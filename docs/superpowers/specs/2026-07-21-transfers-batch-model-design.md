# Transfers Batch Model (v2.1) — Design

**Date:** 2026-07-21 · **Branch:** `0.5.0` · **Supersedes the point-fix waves S1–S3; extends** `2026-07-20-transfers-status-v2-design.md`

## Context

The v2 status model shipped and the owner's live-smoke marathon (2026-07-20/21) surfaced a run of bugs that were individually fixed — stale retry statuses, restart resume, dead Resend buttons, duplicate rows, undeletable records, uncontrolled temp data. The owner then called the pattern: the fixes were piling up in one subsystem because something more fundamental is wrong. The audit below confirms it.

**The fundamental mismatch: the database models ATTEMPTS, the UX models TRANSFERS.**

Every recent frontend patch (terminal-row collapse, supersession keys, delete-key resolution via history basenames, hide-trash-when-live) is the UI reconstructing a missing entity from side channels. Each new user story needs a new crutch. This spec removes the mismatch at the source.

## User stories exercised in the smokes

| # | Story | Expectation |
| ---- | ---- | ---- |
| U1 | Send an object/files to my other device | ONE transfer in the list, per-file progress, completion |
| U2 | Cancel on the receiver | transfer stops, fact recorded in BOTH histories |
| U3 | Cancel on the sender | transfer stops EVERYWHERE |
| U4 | Resend after cancel/failure | the SAME transfer continues; one row; only missing files travel; no leftovers |
| U5 | Restart the app at any point | everything visible, resumable, cancellable, deletable |
| U6 | Network flap | prompt auto-resume |
| U7 | Delete a record | space reclaimed, no zombies |
| U8 | Honest statuses at all times | row/files match reality |

## Root causes (audit of the current schema + command surface)

- **F1 — no batch entity.** `sync_outbound` = one row per attempt (enqueue AND retry mint rows); batch identity is a PATH CONVENTION (package-dir basename). `sync_inbound` = one row per received attempt keyed `(peer, wire_package_id)`; attempts of one batch are unrelated in the receiver's DB.
- **F2 — batch identity never crosses the wire.** `Announce2` carries a per-attempt package id only. Hence the receiver's triple rows for one batch, the post-ingest `announced` orphan, and the impossibility of reliable receive-side supersession.
- **F3 — asymmetric lifecycle.** Receiver cancel travels in the ack; sender cancel is LOCAL (no revoke message, live provider streams not torn down) — the receiver keeps downloading. The "sender confirmed all-duplicate after announcing" race leaves a receiver row stuck `announced` forever.
- **F4 — temp data has no owner.** Payload dirs (`<sync>/packages/<uuid>/`, full file copies) are batch-scoped but cleaned only on confirm; cancelled/failed payloads live forever; record deletion leaves them; failed receives hold `in-flight/` blob tags forever (3.9 GB observed on one machine).
- **F5 — commands address attempts where the user thinks in transfers.** Resend duplicates rows; delete needs basename gymnastics for sent and wire-ids for received; cancel can't reach invisible attempts.
- **F6 — history is dual-keyed** (dir basename for sent, wire id for received): sent attempts merge by accident, received attempts never merge.

## Design

### D1. Row = transfer; attempt = counter

- **`sync_outbound`: one row per transfer.** Resend does NOT insert a row: the SAME row resets to `queued`, `attempts += 1`, a **fresh per-attempt wire id** is minted into `wire_package_id` (now "current attempt's wire id"). The "wire id stable across restarts" invariant holds WITHIN an attempt (restart re-drive reuses it); only a new attempt rotates it. Ack-replay safety is automatic: receipts stay keyed by per-attempt wire ids, so a cancelled attempt's receipts can never replay onto the next attempt.
- **`sync_inbound`: one row per `(peer, batch_uuid)`.** New `batch_uuid` column + unique index. A new attempt's announce UPSERTS the existing row (state machine runs a fresh cycle: `announced → fetching → …`); `package_id` becomes "current attempt's wire id" for ack correlation. The receiver's three rows for one batch become one long-lived row.
- **Per-file rows and the journal are per TRANSFER.** At attempt start the transfer's `sync_*_files` rows reset to `pending`/`announced` (previous attempts' verdicts already live in history); `sync_events` keeps appending to the same feed — attempts are naturally visible in the Log tab.
- **History keys on `batch_uuid` in BOTH directions** (existing `package_id` column, new value discipline). Legacy rows read as before.

### D2. Wire: `Announce3` + `Revoke`

Both appended at the END of `Msg` (frozen-index discipline, golden pins for v1/v2 untouched):

- `Announce3`: everything in v2 + `batch_uuid: String` (+ room for the collab forward: announces of project packages will reuse the same field later). v1/v2 announces remain decodable; legacy fallback: `batch_uuid := wire package id` (behavior identical to today).
- `Revoke { package_id, reason }` — sent best-effort by the sender on ANY terminal transition with an outstanding un-acked announce: user cancel, all-duplicate confirm that raced its own announce, local failure. The sender also stops serving immediately. The receiver, on Revoke for a non-terminal row: aborts the in-flight fetch (existing `InboundControl` abort), marks the row terminal with the honest reason (`cancelled (by sender)` / `superseded`), settles file rows, removes staging, releases in-flight tags, journals.
- Compatibility: a beta.3 peer ignores unknown variants (stream decode error) → today's behavior; release notes already require "update all devices" for 0.5.0.

### D3. Command surface (re-keyed to transfers)

| Command | Before | After |
| ---- | ---- | ---- |
| `enqueue_sync_selection` | new row per send | unchanged (new transfer) |
| `retry_sync_package` | NEW row + new dir reuse | SAME row reset (attempts++, fresh wire id, files→pending, journal `resend`) |
| `cancel_sync_package` | local terminal | terminal + `Revoke` + stop serving |
| `delete_transfer_history` | basename gymnastics (sent) / wire id (received) | row id = transfer; also deletes payload dir (sent) / releases in-flight tags (received); dead-peer branch unchanged |
| `send_now_sync_package` | kick attempt | unchanged |
| `get_sync_status` / `list_terminal_transfers` | attempt rows | transfer rows (+ `attempts` field) |
| `list_transfer_files` / `list_transfer_events` | per attempt | per transfer (same ids) |

### D4. Temp-data lifecycle (GC)

- **Delete = reclaim**: deleting a transfer record removes its payload dir (sent) and releases its `in-flight/` tags (received). Guards already ensure only terminal transfers delete.
- **Startup sweep of orphans**: payload dirs with no DB row; `in-flight/` tags with no non-terminal inbound row. The sweep cross-checks the DB in the same pass and never touches anything a non-terminal row references.
- **Settings → Sync: "Transfer storage"**: `packages X GB · blobs Y GB` + a "Clean up finished" action (terminal payloads + orphan tags + a GC pass). After cleanup, Resend of old batches honestly disappears (`resendable=false` already gates the button).
- **Blob-tag namespaces are contract**: transfer machinery owns `batch/…` and `in-flight/…`; the future collab swarm owns `project/<id>/<hash>` (long-lived seeding tags). Sweeps clean ONLY their own namespace.

### D5. Presentation

- The UI consumes transfer rows as-is: the frontend collapse/supersession/delete-key-resolution code is DELETED (the model does it). "attempt N" comes from the `attempts` column.
- `duplicate` file verdicts render as **"already on peer"**; an all-duplicate transfer shows "Peer already had every file — nothing was re-transferred" in the detail pane.
- Filters (incl. Cancelled), trash, Resend, countdown — unchanged mechanics, simpler inputs.

### D6. Collab swarm forward-compatibility (design constraints, NOT scope)

The torrent-like collaboration (dynamic project file set; any member fetches missing files from any holder; members re-seed to offload the uploader) layers on top:

```
L3 Registry: project file set (dynamic, hash-keyed), membership, authority — hub-signed (exists in Stage II)
L2 Planner:  my haves vs registry → missing; disjoint partition across holders; failover; have-map updates
L1 Delivery: THIS SPEC — each planned fetch from one holder = one immutable transfer
```

Constraints this spec bakes in for L2/L3 (cheap now, painful later):

- **`project_id TEXT NULL`** on `sync_outbound`/`sync_inbound` (D7 migration): distinguishes collab transfers (fixes today's resurrection/status blindness) and gives the planner its key.
- **Tag namespaces** (D4): re-seeding = long-lived `project/<id>/<hash>` tags on received blobs; transfer sweeps must not collide.
- **Transfers stay immutable**: a dynamic project set is a STREAM of immutable delta-transfers, never a mutable manifest.
- **File-level granularity, not chunk-level**: different files from different holders in parallel; one file travels whole from one source. Right granularity for hundreds of ~50 MB files; chunk swarming is an explicit non-goal.
- Forward notes for the collab cycle (not here): hub have-map; disjoint-partition planner; import-by-reference seeding (serving catalog files without payload copies); registry keyed by content hash, not path.

## Migration & compatibility

**No data migration — clean reset (owner decision 2026-07-21).** The accumulated transfer records are smoke-test debris; preserving them buys nothing. On first init with the new schema (detected by the absence of the `batch_uuid` column, which doubles as the migrated-flag), `init_db` wipes ALL transfer bookkeeping in one transaction: `sync_outbound`, `sync_inbound`, `sync_outbound_files`, `sync_inbound_files`, `sync_events`, `sync_receipts`, `sync_sources`, and `sync_history` — then adds the new columns/indexes. Idempotent (the column exists on every later run). Catalog data (`files`/`frames`/…) and files on disk are untouched.

- The B7 startup orphan sweep runs after the wipe and reclaims everything the deleted rows referenced: payload dirs under `<sync>/packages/`, `in-flight/` blob tags → disk space returns on first launch.
- Release note: "0.5.0 clears the transfer history/records of earlier builds (files themselves are not affected); update all devices."
- Wire: 0.5.0↔0.5.0 full; 0.5.0→beta.3 degrades to today's behavior; Perseus v1 sends unaffected.

## What is deleted (net simplification)

Frontend: terminal-row collapse, receive-side supersession heuristics, delete-key resolution via history, hide-trash-when-live special case. Backend: row-minting resend, basename-based delete matching. Everything else from the v2 cycle (per-file tables, journal, displayState, resurrection, resendable, dead-peer delete) is reused as-is.

## Task plan (wave B)

- **B0. Protocol verification** (owner rule): pin `Announce3`/`Revoke` shapes against the frozen-index discipline; verify against vendored iroh-blobs 0.103 sources that (a) dropping the serve registration terminates provider streams the way D2 assumes, (b) re-serve after a payload re-import behaves as B3 assumes. Output: ledger note with source citations. No code.
- **B1. Wire**: `types.rs` (+`PackageAnnounceV3`, `RevokeReason`), `proto.rs` (2 appended variants), golden pins, routing in both transports + loopback, `TransportEvent` extras. Tests: roundtrips, pins, v1/v2 decode untouched. Acceptance: sharing tests green, old pins byte-identical.
- **B2. DB reset + new columns**: one-shot wipe of all transfer tables gated on the missing `batch_uuid` column (see Migration), then `batch_uuid` + unique `(peer, batch_uuid)` index + `project_id` on inbound, `project_id` on outbound. Guarded-ALTER (T2 pattern). Tests: fresh init no-wipe; legacy DB (rows seeded, no column) → wiped once, catalog tables untouched, second init no-op.
- **B3. Sender engine**: resend-as-reset (same row, attempts++, fresh wire id, files reset, journal `resend`); Revoke on cancel AND on terminal-with-outstanding-announce; stop-serving on cancel; `retry_sync_package` rewritten to reset (row minting removed). Tests: resend cycle on one row; receipts isolation between attempts; revoke on the all-duplicate race.
- **B4. Receiver**: upsert by `(peer, batch_uuid)` (v3); `Revoke` handling (abort, honest terminal, staging/tags); v1/v2 fallback (`batch_uuid := wire id`). Tests: two attempts → ONE row through a full cycle; revoke mid-fetch; legacy path untouched.
- **B5. Commands/status**: surface re-keyed per D3; `attempts` in summaries; delete reclaims payload/tags; both mirrors + ts_export. Tests: unit per command.
- **B6. Frontend simplification**: delete the compensation code; `attempt N` from the column; "already on peer" copy; filters/trash/Resend on the simpler inputs. tsc/build + walkthrough of both owner screenshots.
- **B7. GC**: startup orphan sweep (namespace-scoped); Settings "Transfer storage" + Clean up finished. Tests: sweep spares everything a non-terminal row references; cleanup flips `resendable`.
- **B8. e2e**: sender cancel stops the receiver (revoke); resend cycle = one row on both ends; the announced-race closes via revoke; restart mid-resend; sweep after delete.
- **B9. Finale**: spec/CLAUDE.md updates, whole-branch review, release-note reminder.

Order B0→B9, linear dependencies, per-task briefs/reviews per the established SDD process.

## Verification (end-to-end)

Gates per task (`cargo build --workspace --all-targets`, `cargo test -p athenaeum-core --lib`, `--test sync_e2e`, `npx tsc --noEmit`, `npm run build`, golden pins). Live two-instance smoke script: U1–U8 walked in order, both directions, including: cancel on sender mid-fetch (receiver stops within seconds), resend of a cancelled batch (one row both ends, only missing files travel), 6th-device-style fresh receiver (full fetch), delete + storage reclaim visible in Settings.
