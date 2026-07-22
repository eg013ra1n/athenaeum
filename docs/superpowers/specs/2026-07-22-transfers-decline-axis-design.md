# Transfers — Decline Finality Axis (Batch Model v2.2)

**Date**: 2026-07-22 · **Branch**: `0.5.0` · **Status**: approved (owner smoke №7 root-cause cycle)
**Predecessors**: `2026-07-20-transfers-status-v2-design.md`, `2026-07-21-transfers-batch-model-design.md`

## §1 Problem

Owner smoke №7 (2026-07-22): sender cancels a transfer mid-flight, then presses **Resend**.
Result: the transfer is permanently bricked with cross-blame — sender shows *"Cancelled by
receiver"*, receiver shows *"Cancelled by the sending device"* — and every further Resend
repeats the loop.

Causal chain (all verified in code):

1. Sender cancel → `Revoke{Cancelled}` → receiver `handle_revoke` maps it to
   `InboundState::Cancelled` + detail `"by sender"` (`receiver.rs`, Cancelled arm).
2. `InboundState::Cancelled` is by contract the **receiver-decline** terminal
   (`models.rs`), and `upsert_inbound_attempt` keys `cancelled_final` on **state alone**
   (`store.rs`).
3. Sender Resend re-announces the same `batch_uuid` → receiver hits cancelled-is-final →
   `cancel_epilogue` → **all-cancelled ack**.
4. Sender's all-cancelled handler (`engine.rs`) stamps `Cancelled` + *"cancelled by
   receiver"*. Both sides now blame the other; goto 3 forever.

The revoke arm's doc comment rests on an explicitly false assumption — *"no re-announce
follows a sender's terminal revoke"* — which Resend contradicts by design (batch model:
resend RESETS the same row). The `Superseded` arm already dodges this exact trap
deliberately; the `Cancelled` arm didn't.

### §1.1 Root cause (the model flaw, not the symptom)

**Transfer-level finality ("the receiver refused this transfer") is encoded in the
attempt-level lifecycle column `state`** — a column that is legitimately overwritten by
sender revokes, restart reconciliation (`failed "interrupted by restart"`), and epilogue
stamps. Every writer of `state` therefore silently participates in the finality decision.
Three concrete defects fall out:

- **(a) Conflation** — sender-revoke writes the same `cancelled` state the decline uses →
  the smoke bug.
- **(b) Decline is not crash-durable** — `cancel_incoming_package` cannot stamp
  `Cancelled` while a fetch is live (it would race the fetch loop's state writes — a
  race that exists *only because* finality shares the state column), so a decline during
  `Fetching` lives in an in-memory flag until the epilogue. Receiver crash before the
  epilogue → restart reconcile stamps `failed` → the decline is lost and a resend
  re-fetches.
- **(c) Receipt-set orphaning** — a declined row's `package_id` is never rotated, but a
  post-decline epilogue writes its `Cancelled` receipts under the **new** attempt's wire
  id. The §B4 seeding reads `row.package_id` → never finds them → every subsequent resend
  re-fetches the manifest and re-writes a full set of `sync_history` rows (42 dupes per
  resend in the smoke log).

Cosmetic: revoke/cancel journals report `landed=42` because the settle path reuses
`state=done` with `outcome=cancelled` and the `landed` counter checks state only.

## §2 Design: split the finality axis

One new nullable column carries the receiver's transfer-level decision; the `state`
column reverts to what it already was everywhere else — the **current attempt's**
lifecycle.

### D1 — `sync_inbound.declined_at TEXT NULL`

ISO-8601 stamp. Non-NULL ⇔ *the receiving user declined this transfer; it is final*.
Independent of `state`, survives any later `state` overwrite (revoke, restart reconcile,
epilogue). Added via the existing guarded-ALTER idiom in `ensure_inbound_columns`
(`store.rs`). NOT part of the upgrade-wipe detector (that stays keyed on `batch_uuid`).

**Backfill** (runs only in the ALTER branch, i.e. exactly once per DB):

```sql
UPDATE sync_inbound SET declined_at = COALESCE(finished_at, created_at)
WHERE state = 'cancelled'
  AND (last_error IS NULL OR last_error <> 'by sender')
```

Rationale: on a Wave-B DB the only `cancelled` rows with `last_error = 'by sender'` are
sender revokes (must NOT become declines — this is the smoke artifact); every other
`cancelled` row is a genuine receiver decline.

### D2 — Write sites (exactly one primary, two repairs)

| Site | When | Writes |
| ---- | ---- | ---- |
| **Primary**: `cancel_incoming_package` (`api/sync.rs`) | user declines; row exists and is non-terminal (`Announced` or `Fetching`; `Ingesting` stays refused) | `declined_at = now` **immediately**, before any flag/stamp — safe during a live fetch because it is not the `state` column, closing §1.1(b). Existing behavior otherwise unchanged (flag; `Announced` also stamps state now; `Fetching` leaves state to the epilogue). |
| Repair 1: `cancel_epilogue` (`receiver.rs`) | epilogue runs (only ever on decline evidence: `is_cancelled` flag or declined-final divert) | `declined_at = now` iff NULL |
| Repair 2: replay-guard all-cancelled branch (`receiver.rs`) | full all-cancelled receipt replay (only ever decline-originated) | `declined_at = now` iff NULL, alongside the existing crash-repair `Cancelled` stamp |

`handle_revoke` **never** touches `declined_at` — that is the whole point.

The decline-before-announce case (no row yet) keeps its in-memory-only flag: the window
is tiny, the user can decline again, and pre-creating rows for unannounced transfers is
not worth the surface. Accepted risk, documented on `InboundControl`.

### D3 — Finality check keys on the axis

`upsert_inbound_attempt` (`store.rs`): SELECT gains `declined_at`; the final-branch
condition becomes `declined_at IS NOT NULL` (was `state == 'cancelled'`). The returned
flag renames `cancelled_final` → `declined_final` through `receiver.rs`. Behavior on
final: unchanged (row untouched; seed + re-ack all-cancelled; no fetch).

Consequences:

- A sender-revoked row (`state=cancelled`, `declined_at` NULL) **resets** on re-announce
  like any attempt terminal → Resend works. Fixes the smoke.
- A declined row whose state was later overwritten (e.g. restart reconcile → `failed`)
  is **still final** → decline survives crashes. Fixes §1.1(b).

### D4 — `InboundState::Cancelled` demoted to attempt terminal

`handle_revoke`'s Cancelled arm keeps writing `state=cancelled` + `"by sender"` (honest
attempt terminal; UI strings unchanged). Doc updates required:

- `models.rs` `InboundState::Cancelled`: "attempt cancelled (receiver decline **or**
  sender revoke); transfer-level decline finality lives in `declined_at`".
- `handle_revoke` serial-loop note: delete the false "no re-announce follows a sender's
  terminal revoke" claim; a resend now legitimately resets the row. The
  `request_cancel(old_wire_id)` call stays harmless (new attempt = new wire id; the
  in-memory set is keyed by wire id).

### D5 — Receipt anchor invariant (fixes §1.1(c))

**Invariant**: `sync_inbound.package_id` of a declined row always names the wire id that
holds (or is about to hold) its authoritative full `Cancelled` receipt set.

- `cancel_epilogue` entry: if the row's `package_id` ≠ the current announce's wire id,
  rotate it (UPDATE by row id) **first** — every downstream write in the epilogue
  (`get_inbound` name fallback, receipts, `set_inbound_state`) is then consistently
  keyed and the silent no-op state-write in the final path disappears.
- §B4 seeding: after successfully re-keying the full prior set under the new wire id,
  rotate `package_id` to the new wire id, so seeding always reads the newest set.
- Epilogue failure after rotation (manifest fetch error) is convergent: next resend finds
  no receipts under `row.package_id` and re-runs the epilogue — same as today.

Result: resend-of-declined answers from the receipt log every time after the first
epilogue — no repeat manifest fetch, no duplicate `sync_history` rows.

### D6 — Honest `landed` counter

Shared predicate for the revoke / cancel-epilogue / supersede journals and details:
`landed` = file rows with `state == done` **and** `outcome ∉ {cancelled, superseded}`
(settle-written outcomes are not landings; ingest verdicts are).

### D7 — Explicit non-changes

- **Wire**: zero changes. No new `Msg`, indices untouched, golden pins untouched,
  Perseus compatibility unaffected.
- **Sender/engine**: zero changes. After D3, an all-cancelled ack only ever originates
  from a genuine decline, so the existing *"cancelled by receiver"* attribution becomes
  truthful as-is. Stale acks are already dropped by current-wire-id correlation.
- **Frontend**: zero changes. `InboundSummary` is unchanged (receiver-decline rows keep
  rendering as plain Cancelled; sender-revoked rows keep `"by sender"` → *"Cancelled by
  the sending device"*). `InboundRow.declined_at` is a Rust-side field only.
- **Out of scope** (stays with the queued transport cycle): lost-Revoke zombie row on
  the receiver (fire-and-forget `spawn_revoke` + the M1 restart-skip watch) — a resend
  still works there (row is non-terminal → resets), heals at receiver restart reconcile;
  first-contact announce stall (the Windows-first-launch symptom in the same smoke log).

## §3 Case matrix (what this closes)

| Case | Before | After |
| ---- | ---- | ---- |
| Sender cancel → Resend (smoke №7) | bricked, cross-blame | resets, delivers |
| Receiver decline → Resend | final, all-cancelled re-ack | unchanged (final via axis) |
| Decline during `Fetching` → receiver crash → resend | decline LOST, re-fetch | final (`declined_at` stamped at command time, survives reconcile) |
| Decline epilogue crash between receipts and stamp → resend | decline lost on fresh wire id | final via axis; receipts converge via D5 |
| Resend of declined ×N | manifest re-fetch + N×42 history dupes (revoke path / crash path) | replay from log, history written once |
| Revoke ↔ resend-announce race (either order) | converges (wire-id keying) | unchanged |
| Both sides cancel simultaneously | each shows own cancel | unchanged |
| Late all-cancelled ack after resend | dropped (current-wire-id correlation) | unchanged |
| Legacy v1/v2 peers (`batch_uuid` := wire id, row per attempt) | finality never spans attempts | unchanged |

## §4 Test plan

Store units (`store.rs` tests):

- `declined_at_migration_backfills_receiver_declines` — legacy-shape table: `cancelled`+
  NULL error → backfilled; `cancelled`+`"by sender"` → NOT backfilled; `failed` → NOT;
  re-run is a no-op.
- `upsert_inbound_attempt_finality_keys_on_declined_at` — revoked-cancelled row
  (declined_at NULL) resets with generation bump; declined row untouched-final; declined
  row with state later forced to `failed` still final.

E2E (loopback harness, `engine_tests.rs` / `receiver.rs` tests):

- `sender_cancel_then_resend_delivers` — the smoke №7 regression: cancel (revoke lands,
  receiver row `cancelled`/`"by sender"`) → resend → receiver resets (generation+1),
  fetches, ingests; sender confirms; no all-cancelled ack anywhere.
- `decline_during_fetch_survives_restart_and_refuses_resend` — decline stamps
  `declined_at` while row is `Fetching`; simulate restart (startup reconcile → `failed`);
  resend → all-cancelled ack from epilogue/replay, **no payload fetch**; sender terminal
  *"cancelled by receiver"*.
- `resend_of_declined_transfer_writes_history_once` — decline; resend ×2; receiver
  `sync_history` rows for the batch == frame_count exactly once; second resend answered
  from the receipt log (journal shows replay, not a second epilogue).

Existing 18 transfer e2e must stay green unmodified except where they pin the old
`cancelled_final` name.

## §5 Touched files

- `crates/athenaeum-core/src/sync/store.rs` — D1 column+backfill, D3 finality, D5
  rotation helper, `INBOUND_COLS`/decode.
- `crates/athenaeum-core/src/sync/models.rs` — `InboundRow.declined_at`, D4 docs.
- `crates/athenaeum-core/src/sync/receiver.rs` — D2 repairs, D3 rename, D5 rotation
  call-sites, D4 doc fix, D6 landed.
- `crates/athenaeum-core/src/api/sync.rs` — D2 primary write.
- Tests per §4.

Gates: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`
(expected no-op — no TS surface change).

## §6 Review-fix addendum (8-angle review, same cycle)

Findings confirmed by the post-implementation review and folded into the design:

- **§D2 hardening — the `cancels` set must stay decline-originated.**
  `handle_revoke` no longer calls `request_cancel` (its old "defensive" insert
  was harmless only under cancelled-is-final; under the reset semantics a
  straggler re-announce of the revoked wire id — announce/revoke have no
  cross-stream ordering guarantee — would divert into the epilogue and mint a
  `declined_at` the user never chose, for Cancelled/Superseded/Failed alike).
  The sibling `revoke_aborts` entry is now CONSUMED at `handle_revoke` entry
  (`clear_revoke_abort`) — a lingering entry would break the straggler's
  legitimate re-fetch with no terminal written. Pinned by
  `revoke_then_same_wire_straggler_announce_delivers`.
- **§D2 primary write is an atomic guarded UPDATE** (`WHERE state NOT IN
  ('ingesting','done')`), single lock scope: no TOCTOU minting declined-final on
  a transfer that raced to delivery, and a decline that loses the race to a
  sender revoke / fetch failure (attempt-terminal `cancelled`/`failed`) is still
  recorded.
- **§D5 rotation is gated on a COMPLETE seed** — anchoring a partial receipt set
  would orphan the full one and re-open the duplicate-history epilogue.
- **§D1 backfill is one canonical statement inside a savepoint** with the ALTERs
  (crash between column-add and backfill can no longer strand the migration),
  the revoke detail is the shared `REVOKED_BY_SENDER_DETAIL` const (Rust single
  source; TS mirror in `presentation.ts` noted), and the reduced test fixtures
  were given the real column set instead of column-tolerant production branches.

Deferred (noted, out of this cycle): surface `declined` on `InboundSummary` so a
declined row reconciled to `failed "interrupted by restart"` doesn't render as a
plain failure (needs TS + UI); batch-uuid-keyed decline command (today a decline
against a stale wire id no-ops with Ok); `delete_transfer_history` leaves
`sync_receipts`, so a deleted declined transfer can resurrect as a fresh row on
a retrying announce (pre-existing).
