# D2 — The receive side learns to wait

**Status:** design approved 2026-07-25 · rewritten 2026-07-25 after a claim-by-claim verification pass against the code (35 defects and 14 omissions in the first draft) · **Audit:** `docs/superpowers/research/2026-07-25-delivery-model-audit.md` (F5, and the receive-side half of F1/F2) · **Predecessor:** D1 (`2026-07-25-peer-reachability-delivery-ladder-design.md`) · **Successor:** D3 (multi-source project distribution)

## 1. Problem

Three defects from the owner's two-machine smoke, all on the receiving side, all one theme: **the receiver has no way to say "I am waiting"**, and no honest way to report what it holds.

**(a) A transfer whose SENDER went away reads `failed`.** Any `Err` out of `transport.fetch` is stamped terminal (`receiver.rs:1383`), and the startup reconcile stamps `Failed "interrupted by restart"` over every non-terminal row (`receiver.rs:743`). Neither distinguishes "the peer vanished" from "we cannot accept this" — the exact distinction D1 taught the SEND side, never applied to its mirror. Observed twice in the smoke log, each time leaving a red row for a transfer the sender was still obliged to redeliver.

**(b) The per-file counter reads `0 of 38` while bytes are visibly flowing.** The counter is shared by both directions (`store.rs::grouped_file_counts`, the predicate at `store.rs:1978`) and counts `state = 'done' OR state = 'uploaded'`. `uploaded` is the SENDER's rung for "this file's bytes are out, verdict pending"; the receiver has no equivalent, so a fully-arrived file sits in `fetching` until the whole collection is fetched AND ingested. The per-file bars, fed by live `sync-file-progress`, already show the truth — which is why the row contradicts itself.

**(c) "Clean up" cannot move the number it sits next to — and on the receive side it never could.** The first draft read this as a reporting-honesty defect. It is not, or not only:

- `remove_terminal_payload_dirs` walks `outbound_ref_states` — `SELECT id, package_ref, state FROM sync_outbound` (`store.rs:2148`, called from `api/sync.rs:3802`). **Sender rows only.** On a receive-only device `payload_dirs` and `payload_bytes` are structurally zero, always. The observed `payload_dirs=0 payload_bytes=0` was not a lag; there was nothing in scope.
- `tags_released=0` likewise means nothing was orphaned, not that released bytes lagged. The `in-flight/` tag is set only after phase 1 of the download succeeds (`blobs.rs:377-382`); the smoke's fetch died before that, so no tag ever existed to release.
- The receiver's real on-disk cost lives in two places the command cannot reach: the staging tree `<sync>/staging/<wire_id>` (`receiver.rs:1215`) and the permanent `recv/pkg/<id>` tag pinning the fetched collection in `blobs/`. Three failure paths return without touching either — fetch (`receiver.rs:1379-1403`), ingest (`receiver.rs:1470-1477`), ack (`receiver.rs:1491-1498`) — so a receiver whose ingest fails keeps a full second copy of the batch on disk indefinitely.
- `TransferCleanup` **already** separates freed-now from released-references, with the ~15-minute GC caveat in the type doc (`api/sync.rs:3992-4011`) and a test asserting all three fields (`api/sync.rs:5103-5105`).

So the honest-wording change the first draft proposed was already in the code. What is missing is the receive-side pass itself.

## 2. Decisions

| Decision | Choice |
| ---- | ---- |
| A fetch interrupted by an absent peer | A NEW non-terminal state `InboundState::Waiting` — stored, not derived |
| Which errors become `Waiting` | Decided at the failure site, by a typed classification, not by sniffing error text (§3.2) |
| What stays `Failed` | Only what WE cannot accept — the full inventory in §3.2 |
| Per-file rows on a `Waiting` stamp | Untouched. They are the resume checkpoint |
| The file counter | A new rung `InboundFileState::Fetched`, twin of the sender's `Uploaded`, counted by the SAME predicate |
| Blob bytes after cleanup | A receive-side pass (staging sweep + terminal-row tag release). The GC interval is NOT touched |
| Display vocabulary | Reuse D1's `waiting_peer` — one chip for both sides |

## 3. Architecture

### 3.1 `InboundState::Waiting`

A fetch that stops because the other side went away is not a failure: under delivery-forever the sender is *obliged* to re-announce, so the honest state is "this attempt ended; the sender owes us another". The reason is preserved in `last_error` exactly as before; what changes is that the row is no longer terminal, so it reads as outstanding rather than lost.

**What the user sees:** the row stays in the active list with D1's "waiting for peer" chip and its subline, keeps the file count it has reached, and revives by itself when the sender returns — D1's presence beacon makes the sender re-announce, and `upsert_inbound_attempt` turns the same durable row into a fresh attempt. `Failed` keeps its old meaning: this one needs a human.

**Stored, not derived.** The sender derives two different things: the neutral `waiting` chip from a clock (`next_retry_at`, `status.rs:145-151`) and `waiting_peer` from an error-class prefix (`PEER_ABSENT_PREFIXES`, `status.rs:116-123`, checked first at `status.rs:142`). The receiver has neither: it owns no clock, takes no initiative, and — decisively — its row is stamped **terminal** at the moment of failure. Deriving a non-terminal display from a terminal row would leave every membership test in §4 still treating the transfer as finished: the row would read "waiting" while `inbound_active` excluded it, the boot reconcile ignored it, and `delete_transfer_history` swept it. The state has to move, not the label.

A welcome consequence: the row stops depending on an event to stay visible. `Waiting` is non-terminal, so `inbound_active` returns it and the 10 s status poll keeps it on screen by itself — which is what the `sync-finished`-on-failed-fetch emission (added in the D1 review pass, `receiver.rs:1384-1401`) was compensating for. That emission is removed here, and §3.2 says where the terminal event must be **added** instead.

### 3.2 The `Failed` inventory, and a verdict for each site

The first draft named two write sites; there are six, and three of them share one helper (`stamp_inbound_failed`, `receiver.rs:781`). A literal sweep of "everything not on the keep-list becomes Waiting" would have miscoloured the revoke path. The complete inventory, with the verdict this design assigns:

| Site | Fires when | Verdict |
| ---- | ---- | ---- |
| `receiver.rs:1383` (fetch) | any `Err` from `transport.fetch` | **Classified** — peer-absent → `Waiting`; local fault → `Failed` |
| `receiver.rs:743` (boot reconcile) | non-terminal row found at startup | **`Waiting`**, and rows already `Waiting` are skipped (§4) |
| `receiver.rs:1475` (ingest) | manifest unreadable (`ingest.rs:145`), `load_receipts` (`ingest.rs:181`), `spawn_blocking` join panic (`receiver.rs:1465`) | **`Failed`** — the bytes are ours and we cannot process them |
| `receiver.rs:1496` (ack) | `transport.ack` fails after a successful ingest | **`Failed`** — every frame is landed and catalogued; only the verdict is undelivered, and the ack-replay guard re-acks on redelivery. The code comment at that site already argues the terminal |
| `receiver.rs:1529` (all-rejected) | `outcome.ok_count() == 0` | **`Failed`** — unchanged. A partial rejection stays `Done` (`receiver.rs:1517-1523`) |
| `receiver.rs:2022/2067` (revoke) | sender sends `RevokeReason::Failed` | **`Failed`** — unchanged. The sender declared a local fatal; there is no redelivery obligation to wait for |

The unsafe-`package_id` rejection (`receiver.rs:936-955`) writes no row at all — it emits a `failed`-flavoured `sync-finished` and returns. It is part of the event sweep below, not the state sweep.

**The fetch classification is produced at the failure site, not re-derived from text.** D1's `peer_looks_absent` classifies the *sender's* connect errors; the receiver's failures originate inside iroh-blobs' download path and would not match those prefixes. Worse, the fetch helper conflates two unrelated causes: the connection dying, and local I/O — `create_dir_all(dest_dir)` at `blobs.rs:501-503` and the payload writes at `blobs.rs:507-516` are inside the same `Result`. String sniffing cannot separate them; the function that failed can. So the fetch path returns a typed failure (peer-absent vs local-fault) and the receiver maps it. **When the classification is unclear, the answer is `Waiting`**: a local fault mislabelled `Waiting` produces a transfer that retries and stays visible, while a vanished peer mislabelled `Failed` is the lie this design exists to remove.

This also settles the first draft's contradictory §4, which listed "staging cannot be created" as a distinct local fault while sweeping all fetch errors into `Waiting`. Staging is only a path join in the receiver (`receiver.rs:1215`); the directory is created inside the fetch, so it is the same `Result` and the same classification.

Two bullets from the first draft's `Failed` list are dropped as separate causes: "the landing write fails" (`ingest.rs:355`) and "the catalog write fails" (`ingest.rs:383-394`) are caught **per frame** inside `process_frame` and become `Rejected` receipts. They reach `Failed` only through the all-rejected rule above.

**The terminal event follows the terminal state.** Today only two of the six sites emit anything: the unsafe-id rejection (`receiver.rs:943`) and the normal ingest terminal (`receiver.rs:1559`, which carries `finished_outcome == "failed"` for the all-rejected case). The ingest-error path (`receiver.rs:1470-1477`) and the ack-error path (`receiver.rs:1491-1498`) emit nothing. Removing the D1-era emission from the fetch path without adding it to those two would leave the vanishing-row bug alive on both. So: **remove** from `receiver.rs:1384-1401`, **add** at `receiver.rs:1475` and `receiver.rs:1496`.

### 3.3 Per-file rows on a `Waiting` stamp

`stamp_inbound_failed` does two things: it stamps the row, and it settles every non-`done` file row to `InboundFileState::Failed` (`receiver.rs:788-796`). The boot reconcile does the same with `"interrupted by restart"` (`receiver.rs:754-766`).

On a `Waiting` stamp **neither runs**. Marking already-fetched files `failed` would destroy the resume checkpoint and reset the very counter §3.4 exists to fix — the row would report zero received files for a transfer that is holding most of them on disk. The file rows stay exactly as the attempt left them; `upsert_inbound_attempt` resets them when the next attempt actually starts.

### 3.4 `InboundFileState::Fetched`

The rung the receiver was missing: **bytes complete, verdict pending** — the exact meaning of the sender's `Uploaded`. The shared counter gains one term:

```sql
state = 'done' OR state = 'uploaded' OR state = 'fetched'
```

so both directions count for the same reason, through the same statement (`store.rs:1978`).

**This is not a one-word relabel of the existing write site.** The receiver's progress sink is a two-write scheme keyed on a `file_seen` map (`receiver.rs:1283-1303`): the `None` arm fires on the first tick for a file and writes `Fetching` unconditionally — even when that first tick already carries `bytes_done >= bytes_total` — after which the completion arm can never run. Resumed files, dedup'd files, files small enough to complete within one tick, and zero-byte files would therefore never reach `fetched`. The sink must adopt the sender's shape: compute the target state from the tick (`bytes_done >= bytes_total → Fetched, else Fetching`) and write whenever the tracked value changes.

Ingest still moves each file to `Done` with its outcome, and `settle_unsettled_inbound_files` (which settles everything `state <> 'done'`) needs no change — a `fetched` row settles like a `fetching` one.

### 3.5 Receive-side cleanup

The button gains the pass it never had. Two sweeps, both keyed on rows that are genuinely finished:

- **Staging trees.** Remove `<sync>/staging/<wire_id>` when its `sync_inbound` row is terminal or absent. This is what the three leaking failure paths (§1c) leave behind, and it is real freed-now bytes.
- **Permanent package tags.** Release the `recv/pkg/<id>` tag of a terminal inbound row, the same way the sender's side releases its own. These are released references: the bytes return on the store's own pass.

`TransferStorage` gains `staging_bytes` so the figure the button sits next to includes what the button can actually move. The existing three-field `TransferCleanup` result and its GC caveat stay as they are — they were already right.

The GC interval stays at 900 s. Partial downloads are protected by `in-flight/` tags, and a shorter interval buys nothing: even at one minute the button would still be reporting a number it does not control. We deliberately do NOT estimate "bytes that will come back": computing it means walking the store's tag graph ourselves, and a wrong estimate is worse than an honest "the store reclaims these within about fifteen minutes".

**A `Waiting` row does not protect its `in-flight/` tag.** `release_orphan_in_flight_tags` keeps a tag alive whenever its wire id appears in `inbound_active` (`api/sync.rs:3879-3891`). Left alone, a `Waiting` row would pin its partial download forever and make Clean-up free strictly less than it does today — the direct opposite of this section's purpose. The partial bytes are not worth an unbounded pin: the dedup handshake renegotiates the file list on the next attempt anyway, so at worst the peer re-sends what the GC reclaimed. The active set that protects tags is therefore the non-terminal rows **minus** `Waiting`.

### 3.6 Display

`inbound_summary` echoes the raw state today (`api/sync.rs:1592`). It gains a mapping, and `Waiting` renders as D1's `waiting_peer` — the "waiting for peer" chip, the "device unreachable — resumes when it is back" subline, and the Waiting filter bucket.

**Answering the question that prompted this section:** the Waiting bucket on the Transfers page already exists and is already direction-agnostic — the predicate at `src/pages/Transfers.tsx:135` sits inside the `kind === 'live'` branch with no outbound check. It is unreachable for received rows today only because `displayState` can never be anything but a raw `InboundState` string. The backend mapping alone opens it; no filter work is needed. One side effect to accept: because `waiting` is the first arm of the else-if chain, a waiting received row **drops out of the "Receiving" bucket** — exactly as a waiting sent row drops out of "Sending".

`InboundSummary` also ships the raw enum alongside the display string (`status.rs:184`), and that enum is a ts_rs-generated closed union in `src/types/models.ts:595`. Adding `Waiting` therefore adds wire vocabulary the frontend must be regenerated to know (§5).

## 4. Boundaries this moves

A new non-terminal state changes membership tests, and a new file rung changes rendering. The first draft's table named seven sites and missed the ones that actually break. This is the checklist; every row must be verified in implementation.

**Row state — Rust**

| Site | Consequence |
| ---- | ---- |
| `InboundState::is_terminal()` (`models.rs:168-173`) | `false` for `Waiting` |
| `inbound_active` (`store.rs:1397`), `terminal_inbound` (`store.rs:1420`), `landing_dir_claimed_by_active` (`store.rs:1820`) | Three **hand-duplicated** SQL literal lists of `('done','failed','cancelled')` — not derived from `is_terminal()`. Each admits a new non-terminal state automatically, so the pairing is correct by duplication, not by construction. A `Waiting` row therefore **holds its landing-dir claim**, which is intended: the sender is obliged to redeliver into the same tree, and releasing the claim would give the next attempt a `_2` suffix |
| `InboundState::from_db` (`models.rs:151-161`) | must parse `"waiting"` — it errors on unknown states (§5) |
| `upsert_inbound_attempt` | a re-announce must revive a `Waiting` row into a fresh attempt exactly as it does a `Failed` one; `declined_at` finality is untouched |
| boot reconcile (`receiver.rs:687-743`) | receipt-repair still runs FIRST and still short-circuits on a complete receipt set (`receiver.rs:706-741`); the fallback changes `Failed` → `Waiting` **and must skip rows already `Waiting`**. Without the skip, a `Waiting` row — non-terminal, hence returned by `inbound_active` on every launch — has its preserved reason overwritten with `"interrupted by restart"` at the first restart |
| `handle_revoke` (`receiver.rs:1969-1978`) | returns early on a terminal row. A `Waiting` row is not terminal, so a revoke that was previously a debug no-op now runs the full bookkeeping — including `insert_history_row` per known file (`receiver.rs:2074-2092`), a plain `INSERT`. Guard against duplicate history rows |
| `cancel_incoming_package` (`api/sync.rs:3323-3357`) | the `declined_at` guard (`state NOT IN ('ingesting','done')`) already admits `Waiting`, and the non-terminal check at `:3334` lets it proceed — but the `stamp_now` match at `:3348` has no `Waiting` arm, so today's code would write `declined_at`, request a cancel, and leave the row `waiting` forever: there is no in-flight fetch whose epilogue would terminalize it. Add `Some(InboundState::Waiting) => true` |
| `delete_transfer_history` (received) | refuses a batch with a non-terminal row: deleting a waiting transfer requires cancelling it first. Deliberate, and symmetric with the sent side |
| `release_orphan_in_flight_tags` (`api/sync.rs:3879-3891`) | the tag-protection set must exclude `Waiting` (§3.5) |

**File state — Rust**

| Site | Consequence |
| ---- | ---- |
| `InboundFileState::from_db` (`models.rs:503-511`) | must parse `"fetched"` — equally strict (§5) |
| `grouped_file_counts` (`store.rs:1978`) | one new term, shared by both directions |
| `inbound_file_counts` doc (`store.rs:1962-1965`) | asserts the invariant this design breaks — *"`state = 'uploaded'` never occurs inbound … so inbound `done` reduces to `state = 'done'`"*. Rewrite it |
| `settle_unsettled_inbound_files` | no change — `state <> 'done'` already covers `fetched` |

**Frontend**

| Site | Consequence |
| ---- | ---- |
| `TransferRow.tsx:250-252` `canCancel` | gates the inbound arm on the RAW state (`'announced' \|\| 'fetching'`). A `Waiting` row would show **no Cancel button** while `delete_transfer_history` refuses to delete it — a row the user cannot get rid of. Switch the inbound arm to `!row.terminal`, mirroring outbound. This is the one omission that makes the design unusable if missed |
| `cancelInbound` refresh (`useTransferQueue.ts:725`, terminal refetch at `:285-304`) | `cancel_incoming_package` emits no event, and terminal rows are refetched only on mount and on `sync-finished` — so a just-declined row disappears instead of settling in place. The cancel path must refresh terminal rows too |
| `TransfersPanel.tsx` incoming branch | does not call `displayStateSubline`; the outbound branch does. Add it, or the waiting subline never renders in the panel |
| `Transfers.tsx:135` filter | structurally ready (§3.6); no change, but note the Receiving-bucket side effect |
| `presentation.ts:220-233` `fileStateChipClass` | no `'fetched'` case → a fetched file falls to the muted default. Add it alongside `'done'`/`'uploaded'` (accent) |
| `src/types/models.ts:595` | `InboundState` is generated from the ts_export registry (`ts_export.rs:151`). Regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` in the same change or the contract gate fails. `InboundFileState` is **not** in the registry and needs no regeneration |
| `crates/perseus/src/web/app.js` | renders received transfers; verify it does not switch on inbound state strings in a way an unknown `waiting` breaks |

## 5. Compatibility

Both `InboundState::from_db` (`models.rs:151-161`) and `InboundFileState::from_db` (`models.rs:503-511`) reject unknown values with an error, so `waiting` and `fetched` are **forward-only**.

**The blast radius is wider than a bad row.** `inbound_active` collects through `Result<Vec<_>>` (`store.rs:1405-1410`), so the first parse error aborts the whole query. On an older build a single `waiting` row fails the entire `get_sync_status` call — **both directions**, so the Transfers screen loses every live row including sent ones — and separately the entire `list_terminal_transfers` and `delete_transfer_history` calls. The file-state hazard is quieter and worse: a downgraded build cannot parse the per-file rows, so the post-ingest settle is skipped rather than merely unrendered.

The batch-model upgrade wipe does not cover this: it fires on a first init without `sync_inbound.batch_uuid`, a column that now exists. A downgrade is only reachable by installing an older build over a newer database — not routine for beta devices, but it must be stated in the release notes rather than discovered.

Historical rows already stamped `failed` are **not migrated**. They record what the app believed at the time; rewriting them would be inventing history.

## 6. Out of scope

The GC interval (§3.5) and any on-demand GC: `gc_run_once` is private in iroh-blobs 0.103 (`store/mod.rs:11`), so reaching it needs an upstream change, which is its own decision. A receive-side retry initiative: the receiver stays passive by design — delivery-forever puts redelivery on the sender, and D1 gave that side the signal to act on. Multi-source download stays D3.

## 7. Testing

**Existing tests go red first.** Eight already-green tests assert `Failed` on paths that become `Waiting` (`receiver.rs:2577` among them), and they break in three distinct ways: the wrong enum, a hard poll that panics on timeout, and secondary assertions on `finished_at` and `inbound_active` membership that flip as a side effect of non-terminality. Enumerating and converting them is the first implementation task, not incidental cleanup.

New coverage, on top of that:

- A fetch that fails with a peer-absent classification leaves the row **non-terminal**, visible in `inbound_active`, reason preserved, **file rows untouched**, and emits no terminal event.
- A fetch that fails with a local-fault classification is `Failed` and emits its terminal event.
- The ingest-error and ack-error paths are `Failed` **and now emit** `sync-finished`.
- A rejected-on-integrity batch is still `Failed`; a partial rejection is still `Done`.
- The boot reconcile stamps `Waiting` for an interrupted row, still repairs a complete-receipt row to `Done`/`Cancelled` first, and **leaves an existing `Waiting` row's reason intact on a second pass**.
- A re-announce revives a `Waiting` row into a new attempt (same durable row, `generation` bumped).
- Cancelling a `Waiting` row terminalizes it there and then.
- `delete_transfer_history` refuses a batch whose row is `Waiting`.
- A revoke arriving for a `Waiting` row terminalizes it without duplicating history rows.
- The file counter climbs during a fetch: an **inbound** mixed-state case over `DDL_INBOUND_FILES` through `inbound_file_counts` (announced / fetching / fetched / done / failed / rejected-outcome), mirroring the outbound-only pin at `store.rs:3622`. The existing test would pass without ever exercising the receive side.
- A file that completes within its first progress tick — resumed, dedup'd, or zero-byte — still reaches `fetched` (§3.4).
- Cleanup removes a terminal row's staging tree and releases its permanent tag; a `Waiting` row's `in-flight/` tag is not protected.

## 8. Risks

- **The membership sweep is the risk, not the state.** §4 is the checklist; the frontend `canCancel` row is the one that turns a design defect into an unusable row, and the reconcile-skip row is the one that silently erases the reason the design exists to record.
- **A `Waiting` row is visible indefinitely** if the sender never returns and the user never cancels. That is honest — the transfer genuinely is outstanding — but the Active list can accumulate rows needing a human decision. The Waiting filter chip is where they belong.
- **`fetched` widens the counter's "done" term**, so a batch that stalls after downloading shows a full file count against an unfinished transfer. Correct — the files ARE fetched — but the UI copy must keep the two readable as different things: the count is files, the state is the transfer.
- **Dropping the `in-flight/` tag protection for `Waiting` rows costs a re-download** of partial bytes the GC reclaims before the sender returns. Accepted deliberately (§3.5); the dedup handshake bounds the cost to whole files, and the alternative is an unbounded pin the user cannot clear.
