# Transfers Batch Model (v2.1) — Implementation Plan (wave B)

**Spec:** `docs/superpowers/specs/2026-07-21-transfers-batch-model-design.md` · **Branch:** `0.5.0` · **Ledger:** `.superpowers/sdd/progress-transfers-v2.md` (wave B section, briefs/reports `tvb-*`)

Process: SDD — controller-written brief per task, fresh implementer per task (rust-engineer / frontend-dev, opus), independent controller re-gate after every task, task review per diff, whole-wave review at the end. Gates on every task: `cargo build --workspace --all-targets`, `cargo test -p athenaeum-core --lib`, `--test sync_e2e`, `npx tsc --noEmit`, `npm run build` (frontend tasks).

## At a glance

| Task | Scope | Depends on |
| ---- | ---- | ---- |
| B0 | Protocol verification (no code) | — |
| B1 | Wire: `Announce3` + `Revoke` | B0 |
| B2 | DB: reset-on-upgrade + `batch_uuid`/`project_id` | — |
| B3 | Sender engine: resend-as-reset, Revoke, stop-serving | B1, B2 |
| B4 | Receiver: upsert by `(peer, batch_uuid)`, Revoke handling | B1, B2 |
| B5 | Commands/status re-keyed to transfers | B3, B4 |
| B6 | Frontend simplification | B5 |
| B7 | GC: orphan sweep + Transfer storage UI | B2 (sweep), B5 (UI) |
| B8 | e2e suite | B3–B7 |
| B9 | Finale: docs, whole-wave review | all |

---

## B0. Protocol verification (owner rule for iroh-touching plans)

**Approach.** No code. Against the VENDORED sources of the pinned versions (iroh 1.0.2 / iroh-blobs 0.103.0, cargo registry checkout) + context7:
1. Confirm appending two `Msg` variants keeps v1/v2 byte-compat (postcard discriminant rules — re-verify against our `wire_golden_tests` discipline).
2. Verify what terminates an in-flight provider stream when the sender "stops serving": does dropping the serve registration / releasing the tag close open transfers, or must the QUIC connection be closed explicitly? (D2 assumes cancel stops the outbound flow promptly — find the actual mechanism and name it for B3.)
3. Verify re-serve semantics after payload re-import (B3's resend resets the same row and re-serves the same dir): does `import_package_collection` re-tag deterministically; is a previously-released hash servable again immediately?
4. Verify receiver-side fetch abort (existing `InboundControl`) cleans partial blobs or leaves them for the in-flight tag release (B4/B7 need to know who owns that cleanup).

**Acceptance.** Ledger note "B0 findings" with file/line citations from the vendored sources for each of the four questions, plus GO/adjust verdict for B3/B4 assumptions. If any assumption fails → STOP, replan the affected task before any dispatch.

## B1. Wire: `Announce3` + `Revoke`

**Files.** `crates/athenaeum-core/src/sharing/types.rs`, `sharing/iroh/proto.rs`, `sharing/wire_golden_tests.rs`, `sharing/iroh/mod.rs` + `node.rs` (routing), `sharing/loopback.rs`, `sharing/types.rs::TransportEvent`.

**Approach.**
1. `types.rs`: `PackageAnnounceV3 { package_id, root_hash, byte_size, frame_count, batch_name, batch_uuid: String, files: Vec<AnnounceFileEntry> }`; `RevokeReason { Cancelled, Superseded, Failed }` (postcard enum, snake_case serde for logs).
2. `proto.rs`: append `Announce3(PackageAnnounceV3)` then `Revoke { package_id: PackageId, reason: RevokeReason }` at the enum tail; extend the frozen-index comment.
3. Sender plumbing: transport announce API gains `batch_uuid`; app sender emits ONLY v3. `Revoke` send helper on the transport trait (best-effort, one-shot).
4. Receive routing: v1/v2/v3 all → `AnnounceReceived` (v1/v2 → `batch_uuid = wire package id` fallback, existing extras rules unchanged); `Revoke` → new `TransportEvent::RevokeReceived { from, package_id, reason }`.
5. Loopback mock mirrors both.

**Tests.** Postcard roundtrips (v3 + revoke, non-ASCII name); golden pins added (existing pins byte-identical — the frozen test proves it); loopback delivers v3 extras incl. batch_uuid and routes Revoke; v1 and v2 announces still decode to the fallback shape.

**Acceptance.** `cargo test -p athenaeum-core --lib sharing` green; no change to any existing pinned bytes.

## B2. DB: reset-on-upgrade + new columns

**Files.** `crates/athenaeum-core/src/sync/store.rs`, `sync/models.rs`, `db/schema.rs` (third materialization site).

**Approach.**
1. Upgrade detector: `column_exists(sync_inbound, "batch_uuid")`. When absent → ONE transaction: `DELETE FROM sync_outbound, sync_inbound, sync_outbound_files, sync_inbound_files, sync_events, sync_receipts, sync_sources, sync_history` (order irrelevant, no FKs), `info!` with per-table counts — then the ALTERs.
2. Columns: `sync_inbound.batch_uuid TEXT` + `CREATE UNIQUE INDEX idx_sync_inbound_batch ON sync_inbound(peer, batch_uuid)`; `sync_inbound.project_id TEXT NULL`; `sync_outbound.project_id TEXT NULL`. Keep the legacy `UNIQUE(peer, package_id)` constraint in place (it still holds — package_id is per-attempt-unique).
3. `InboundRow`/`OutboundRow` gain the fields; read/write paths threaded (T2 discipline: owned column lists, `to_*` mappers).
4. Store CRUD for B3/B4: `reset_outbound_for_resend(id, new_wire_id)` (state queued, attempts+1, wire id, clear last_error/next_retry, files→pending in one tx); `upsert_inbound_attempt(peer, batch_uuid, wire_id, …)` (insert or reset existing row to announced with the new wire id); exact signatures recorded in the report for B3/B4 briefs.
5. `init_db` in `db/schema.rs` calls the same guards (never inline copies — the sync_migrate pattern).

**Tests.** Fresh DB: no wipe, all columns present, double-init no-op. Legacy DB (pre-column shape seeded with rows in every transfer table + catalog rows) → wiped exactly once, catalog untouched, second init no-op. `reset_outbound_for_resend` and `upsert_inbound_attempt` unit-tested (state/wire-id/files effects).

**Acceptance.** `--lib sync` + `--lib db` green; `legacy_sync_tables_converge_to_fresh`-style convergence pin updated.

## B3. Sender engine: resend-as-reset, Revoke, stop-serving

**Files.** `crates/athenaeum-core/src/sync/engine.rs`, `api/sync.rs` (`retry_sync_package`), `sync/store.rs` (only via B2 CRUD).

**Approach.**
1. `retry_sync_package` → `resend_transfer`: same row via `reset_outbound_for_resend` (payload presence check stays; display_name/manifest already on the row/dir — the 730a4f1b re-read logic drops away), then kick the owning engine (ensure/resurrect as today). NO new row, NO new id returned (returns the same id; frontend already keys by row id).
2. Engine announce path: mint-per-attempt wire id comes from the reset (row's `wire_package_id` current); `batch_uuid` = a stable per-row UUID — decision: reuse the package-dir basename as `batch_uuid` (it IS the batch identity today, stable, unique) — no new column on outbound needed; record this equivalence in code docs.
3. Revoke: in `cancel_package` and in the all-duplicate-confirm path when an announce is outstanding un-acked (`pending` entry has a sent announce, no receipts yet) → `transport.revoke(peer, wire_id, reason)` best-effort + journal `revoke_sent`. Stop-serving per B0's verified mechanism.
4. Files reset on resend covered by B2 CRUD; journal `resend` (attempt no in detail).
5. Receipts/ack isolation: nothing to do — per-attempt wire ids isolate replay by construction; pin with a test.

**Tests.** Resend cycle: enqueue → cancel → resend → confirm, ONE row throughout, attempts=2, per-file rows reset then settle; receipts of the cancelled attempt do NOT replay onto the resend (seed old receipts, assert fresh negotiation); revoke emitted on user cancel AND on all-duplicate-after-announce race (loopback); stop-serving verified per B0 mechanism.

**Acceptance.** engine tests + sync_e2e green; `retry_sync_package` row-minting gone (grep).

## B4. Receiver: one row per (peer, batch_uuid) + Revoke handling

**Files.** `crates/athenaeum-core/src/sync/receiver.rs`, `sync/ingest.rs` (landing unchanged — batch_slug logic keeps working since display_name still travels), `sync/store.rs` (B2 CRUD).

**Approach.**
1. `handle_announce` v3: `upsert_inbound_attempt` keyed `(peer, batch_uuid)` — existing row (any terminal or failed state EXCEPT the cancelled-final guard: decision — a receiver-cancelled batch STAYS cancelled until the receiver deletes the record; a new attempt's announce on it is ack-replayed cancelled as today, preserving "Cancelled-is-final") resets to announced with the new wire id, name/manifest refreshed, file rows replaced. v1/v2: batch_uuid := wire id (each attempt its own row — exactly today's behavior).
2. `RevokeReceived`: row lookup by current wire id; non-terminal → abort fetch (InboundControl), state per reason (`cancelled` + detail `by sender` / `superseded`), settle file rows, staging cleanup, release in-flight tags, journal `revoked`; terminal/unknown → debug no-op.
3. Ack correlation keeps using the wire id (unchanged).
4. `landing_dir` persistence continues to work per row (now per batch — even better: attempts land into the same dir naturally).

**Tests.** Two attempts (cancel then resend) → ONE inbound row through announced→fetching→done, history carries both attempts' verdicts; revoke mid-fetch → aborted, honest state, staging gone, tags released; receiver-cancelled batch + new announce → still cancelled + replayed ack (existing guard pinned); v2 announce → per-attempt rows as today.

**Acceptance.** receiver/ingest tests + sync_e2e green.

## B5. Commands/status re-keyed to transfers

**Files.** `crates/athenaeum-core/src/sync/status.rs`, `api/sync.rs`, both command mirrors, `ts_export.rs`, `src/types/models.ts`.

**Approach.**
1. Summaries gain `attempts: u32` (+ inbound too); everything else already row-keyed = transfer-keyed now.
2. `delete_transfer_history`: sent side loses the basename matching — the row IS the batch (delete by key stays for received-legacy groups); ADD payload-dir removal (sent) and in-flight tag release (received) inside the same guarded flow (dead-peer branch unchanged).
3. `resend` returns/keeps the same id (frontend contract simplifies); `resendable` semantics unchanged.
4. ts_export + models.ts regen.

**Tests.** Unit per changed command; delete reclaims payload dir (tempdir) and releases tags (loopback store assert).

**Acceptance.** both mirrors identical; `tsc` green.

## B6. Frontend simplification

**Files.** `src/pages/Transfers.tsx`, `src/hooks/useTransferQueue.ts`, `src/components/transfers/{TransferRow,TransferDetail,FileTree}.tsx`, `historyGrouping.ts`.

**Approach.**
1. DELETE: attempt-collapse, supersession keys, delete-key resolution via history, hide-trash-when-live special case, resend-id juggling. Rows render as delivered.
2. `· attempt N` from `attempts` (N>1). Resend keeps the row in place (state flips to queued live).
3. Copy: `duplicate` chip → `already on peer`; all-duplicate transfer → detail subline "Peer already had every file — nothing was re-transferred".
4. History groups: received-legacy grouping stays for pre-wipe... (moot after the reset — history is empty; keep the grouping code path only if it still serves post-B2 rows, else delete it too — implementer verifies and reports).

**Tests.** `tsc` + `vite build`; walkthrough of both owner screenshots (one row per batch, both directions); empty states.

**Acceptance.** Grep-clean: no supersession/collapse code remains; both screenshots' scenarios produce one row.

## B7. GC: orphan sweep + Transfer storage UI

**Files.** `crates/athenaeum-core/src/api/sync.rs` (sweep + storage stats + cleanup command), both mirrors, `src/pages/Settings` sync section (small UI block).

**Approach.**
1. Startup sweep (spawned post-`ensure_started`, like resurrection): payload dirs under `<sync>/packages/` with no `sync_outbound` row referencing them → remove; `in-flight/` tags with no non-terminal inbound row → release. Namespace discipline: touch ONLY `batch/…`/`in-flight/…` tags, never `project/…`. Cross-check against the DB in the same pass; `info!` totals.
2. `get_transfer_storage` → `{ packagesBytes, blobsBytes }` (walk + store size); `cleanup_finished_transfers` → sweep terminal payloads + orphan tags + GC pass; both mirrored.
3. Settings block: sizes + "Clean up finished" button + result toast. Tokens, existing Settings patterns.

**Tests.** Sweep spares dirs/tags referenced by non-terminal rows (seeded); cleanup flips `resendable` on affected terminal rows (via payload absence); storage numbers sane on a seeded tempdir.

**Acceptance.** First launch post-wipe reclaims the debris (manual verify note in report); gates green.

## B8. e2e

**Files.** `crates/athenaeum-core/tests/sync_e2e.rs`.

**Scenarios.** (1) sender cancel mid-fetch → receiver stops via Revoke within the harness deadline, honest states both ends; (2) *(re-specified after B4: cancel→resend→confirm is unreachable under cancelled-is-final)* attempt-1 FAILS (fetch fault / ack loss) → resend on a rotated wire id → confirm into the SAME batch-keyed inbound row (id constant), only missing files travel (dedup asserted); (3) all-duplicate-after-announce race → revoke closes the receiver row; (4) restart mid-resend → same row resumes; (5) delete → payload dir gone, tags released, storage stats drop.

**Acceptance.** suite green and deterministic (3 consecutive runs), runtime bounded.

## B9. Finale

Spec finalization (v2.1 marked implemented), CLAUDE.md Transfers section update, release-note reminders (transfer-history reset + update-all-devices), ledger closure, whole-wave review (most capable model) + single fix wave, memory update.

## Live smoke script (owner, post-wave)

U1–U8 in order, both directions, two machines: send object → cancel on sender (receiver stops in seconds) → resend (one row, only missing files) → restart sender mid-transfer (resumes) → wifi flap (auto-resume) → delete records (space visibly reclaimed in Settings) → fresh-receiver full fetch (the "6th participant" shape).
