# D2 — The receive side learns to wait

**Status:** design approved 2026-07-25 · **Audit:** `docs/superpowers/research/2026-07-25-delivery-model-audit.md` (F5, and the receive-side half of F1/F2) · **Predecessor:** D1 (`2026-07-25-peer-reachability-delivery-ladder-design.md`) · **Successor:** D3 (multi-source project distribution)

## 1. Problem

Three defects from the owner's two-machine smoke, all on the receiving side, all one theme: **the receiver has no way to say "I am waiting"**, and no honest way to report what it has.

**(a) A transfer whose SENDER went away reads `failed`.** The fetch-failure path stamps `Failed` for any error (`receiver.rs:1382` → `stamp_inbound_failed`), and the startup reconcile stamps `Failed "interrupted by restart"` (`receiver.rs:744`). Neither distinguishes "the peer vanished" from "we cannot accept this" — the exact distinction D1 taught the SEND side, never applied to its mirror. Observed: `sync receiver announce handling failed error=fetch package …: Unable to download …` twice, each leaving a red row for a transfer the sender was still obliged to redeliver.

**(b) The per-file counter reads `0 of 38` while bytes are visibly flowing.** The counter is shared by both directions (`store.rs::grouped_file_counts`) and counts `state = 'done' OR state = 'uploaded'`. `uploaded` is the SENDER's rung for "this file's bytes are out, verdict pending" — the receiver has no equivalent: a fully-arrived file sits in `fetching` until the whole collection is fetched AND ingested. So the sender's counter climbs during a transfer and the receiver's cannot. The per-file bars in the UI (fed by live `sync-file-progress`) already show the truth, which is why the row contradicts itself.

**(c) "Clean up" cannot move the number it sits next to.** `cleanup_finished_transfers` releases blob tags, but iroh-blobs 0.103 exposes no on-demand GC (`gc_run_once` lives in a private module, `store/mod.rs:11`), so the bytes come back on the store's own ~15-minute pass. Meanwhile `get_transfer_storage` reports the raw `blobs/` directory size. Pressing the button therefore looks like a no-op; observed four times in the smoke log with `payload_dirs=0 payload_bytes=0 tags_released=0`.

## 2. Decisions

| Decision | Choice |
| ---- | ---- |
| An interrupted fetch | A NEW non-terminal state `InboundState::Waiting` — stored, not derived |
| What stays `Failed` | Only what WE cannot accept (see §4) |
| The file counter | A new rung `InboundFileState::Fetched`, twin of the sender's `Uploaded`, counted by the SAME predicate |
| Blob bytes after cleanup | Honest reporting. The GC interval is NOT touched |
| Display vocabulary | Reuse D1's `waiting_peer` — one chip for both sides |

## 3. Architecture

### 3.1 `InboundState::Waiting`

A fetch that stops because the other side went away is not a failure: under delivery-forever the sender is *obliged* to re-announce, so the honest state is "this attempt ended; the sender owes us another". It is written in the two places that stamp `Failed` today — the fetch-failure path and the startup reconcile — and carries the reason in `last_error` exactly as before.

**Stored, not derived, and that asymmetry is deliberate.** On the sending side `waiting` is derived (`outbound_display_state`) because the sender owns a clock: `next_retry_at` says when it will act. The receiver owns no clock and takes no initiative — it cannot derive anything. The only alternative would be deriving from `state = failed` plus the text of `last_error`, which is precisely the lie in the database this design removes.

A welcome consequence: the row stops depending on an event to stay visible. `Waiting` is non-terminal, so `inbound_active` returns it and the 10 s status poll keeps it on screen by itself — which is what the `sync-finished`-on-failed-fetch emission (added in the D1 review pass) was compensating for. That emission moves to the genuine failures in §4.

### 3.2 `InboundFileState::Fetched`

The rung the receiver was missing: **bytes complete, verdict pending** — the exact meaning of the sender's `Uploaded`. The receiver already detects the moment (`receiver.rs:1286-1303` writes on the `bytes_done >= bytes_total` transition); it simply writes `Fetching` there today. The shared counter gains one term:

```sql
state = 'done' OR state = 'uploaded' OR state = 'fetched'
```

so both directions count for the same reason, through the same statement. Ingest still moves each file to `Done` with its outcome, and `settle_unsettled_inbound_files` (which settles everything `state <> 'done'`) needs no change — a `fetched` row settles like a `fetching` one.

### 3.3 Honest cleanup reporting

The GC interval stays at 900 s: partial downloads are protected by `in-flight/` tags, but a shorter interval buys nothing here — even at one minute the button would still be reporting a number it does not control.

What changes is the promise. The result separates **freed now** (payload directories, which really are gone) from **released references** (blob tags whose bytes the store reclaims on its own pass), and the UI says so in words rather than showing a storage figure that refuses to move. We deliberately do NOT estimate "bytes that will come back": computing it would mean walking the store's tag graph ourselves, and a wrong estimate is worse than an honest "the store reclaims these within about fifteen minutes".

### 3.4 Display

`inbound_summary` echoes the raw state today. It gains a mapping, and `Waiting` renders as D1's `waiting_peer` — the "waiting for peer" chip, the "device unreachable — resumes when it is back" subline, and the Waiting filter bucket. No new vocabulary: the two sides meet in one chip even though the mechanics beneath differ (§3.1).

## 4. What `Failed` means now

`Failed` is reserved for **what we cannot accept**:

- every frame rejected on integrity (a partial rejection stays `Done`, as today);
- an unsafe `package_id` (no row is written at all — unchanged);
- a local fault: the landing write fails, the catalog write fails, staging cannot be created.

Everything else that stops an attempt — a dead connection, a vanished sender, a restart mid-fetch — is `Waiting`. These paths keep emitting the terminal `sync-finished`; `Waiting` does not (it is not terminal).

## 5. Boundaries this moves

A new non-terminal state changes membership tests, so each is named here and must be verified in implementation:

| Site | Consequence |
| ---- | ---- |
| `InboundState::is_terminal()` | `false` for `Waiting` |
| `inbound_active` / `terminal_inbound` | included in active, absent from the terminal window — automatic, both key off the same three strings |
| `InboundState::from_db` | must parse `"waiting"` — it errors on unknown states (see §6) |
| `delete_transfer_history` (received) | refuses a batch with a non-terminal row: deleting a waiting transfer now requires cancelling it first. Deliberate, and symmetric with the sent side |
| `cancel_incoming_package` | the `declined_at` guard (`state NOT IN ('ingesting','done')`) already admits `Waiting`, and the row is non-terminal so the command proceeds — but it must **terminalize the row immediately**, not wait for an announce that may never come |
| `upsert_inbound_attempt` | a re-announce must revive a `Waiting` row into a fresh attempt exactly as it does a `Failed` one; `declined_at` finality is untouched |
| reconcile receipt-repair | unchanged and still runs FIRST: a row with a complete receipt set becomes `Done`/`Cancelled`; only the fallback changes from `Failed` to `Waiting` |

## 6. Compatibility

`InboundState::from_db` rejects unknown states with an error, so a row stored as `waiting` is **unreadable by an older build**. This is a forward-only change: a downgrade after receiving a transfer would surface as a row-parse error, not silent corruption. Acceptable on the same grounds as the batch-model upgrade wipe — beta devices move forward together — but it must be stated in the release notes rather than discovered.

Historical rows already stamped `failed` are **not migrated**. They record what the app believed at the time; rewriting them would be inventing history.

## 7. Out of scope

The GC interval (§3.3) and any on-demand GC: `gc_run_once` is private in iroh-blobs 0.103, so reaching it needs an upstream change, which is its own decision. A receive-side retry initiative: the receiver stays passive by design — delivery-forever puts redelivery on the sender, and D1 gave that side the signal to act on. Multi-source download stays D3.

## 8. Testing

- A fetch that fails leaves the row **non-terminal**, visible in `inbound_active`, with the reason preserved — and emits no terminal event.
- A rejected-on-integrity batch is still `Failed` and still emits its terminal event.
- The startup reconcile stamps `Waiting`, not `Failed`, for an interrupted row — and still repairs a complete-receipt row to `Done`/`Cancelled` first.
- A re-announce revives a `Waiting` row into a new attempt (same durable row, `generation` bumped).
- Cancelling a `Waiting` row terminalizes it there and then.
- `delete_transfer_history` refuses a batch whose row is `Waiting`.
- The file counter climbs during a fetch, through the same predicate that makes the sender's climb; a `fetched` row settles at terminal like a `fetching` one.
- The cleanup result separates freed-now from released-references, and the UI states the delayed half.

## 9. Risks

- **The membership sweep is the risk, not the state.** Every place that asks "is this row done with?" must be found; §5 is the checklist, and the tests above pin the ones that matter.
- **A `Waiting` row is visible indefinitely** if the sender never returns and the user never cancels. That is honest — the transfer genuinely is outstanding — but it means the Active list can accumulate rows that need a human decision. The Waiting filter chip is where they belong.
- **`fetched` widens the counter's "done" term**, so a batch that stalls after downloading shows a full file count with an unfinished transfer. Correct (the files ARE fetched) but worth watching in the UI copy: the count is files, the state is the transfer.
