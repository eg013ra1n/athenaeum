# Sync Delivery Semantics + Transfer Queue — Design

**Date:** 2026-07-15
**Status:** Approved (brainstorm 2026-07-15)
**Scope:** one cycle, both halves — (A) torrent-style delivery semantics for outbound sync packages, (B) a torrent-client-style transfer queue screen with live per-file receive progress, in both the desktop app and Perseus.

## Context

Today an outbound package retries on a fixed 30s ack-timeout and dies after 5
attempts (`MAX_ATTEMPTS = 5`, `engine.rs:75`; loop in `handle_timeouts`,
`engine.rs:1261`). A peer that is offline for three minutes means a permanently
`Failed` package, and the desktop has no resend at all (Perseus has
`POST /api/retry`; the app has nothing). Two adjacent gaps compound this:

- The receiver loads its authorized-device list only at startup, so a newly
  added device knocks forever and the gate refuses forever until the receiving
  app restarts (observed in the 2026-07-14 owner smoke).
- Transfer visibility is minimal: the desktop `TransfersPanel` shows
  package-short + state + attempts for outbound only; incoming packages are
  invisible until fully ingested (no persisted inbound record at all); byte
  progress does not exist anywhere — the receiver awaits the whole iroh-blobs
  collection in one future (`sharing/iroh/blobs.rs:245`) and the progress
  stream is never consumed. `TransportEvent::FetchProgress` fires exactly once,
  after completion.

Decision (owner): delivery works like a torrent — a queued package is
delivered *eventually, always*, unless the user cancels it or the payload is
locally unrecoverable; and transfers get a real queue screen (batches in both
directions, per-file live progress on receive, sizes, speed, retry controls),
in the app and in Perseus.

## Goals

1. No terminal failure from network conditions. Pending packages survive
   offline peers indefinitely with capped exponential backoff and
   event-triggered instant retries.
2. Receiver authorization refreshes itself — "add a new machine, the old one
   lets it in without restart" works unattended.
3. Desktop resend for terminal packages; "send now" for stalled ones.
4. A dedicated Transfers screen: outgoing + incoming batches, status, file
   lists, live per-file byte progress on the receiving side, speed, retry
   controls. Perseus web page gets the same semantics surface (countdown,
   send-now, stalled badge, batch byte totals).

## Non-goals

- No wire-protocol changes. Everything here is sender-local scheduling,
  receiver-local observation, and hub-API reuse.
- No per-file upload progress on the sending side in this cycle. Provider-side
  events are investigated during planning (§7); adopted only if iroh-blobs
  0.103 exposes them without protocol or architectural cost.
- No receiver-side cancel of an in-flight download (v1: cancel is a sender
  action).
- Perseus receive UI — Perseus does not ingest; nothing to show.

---

## Part A — delivery semantics

### 1. Package states

`OutboundState` (`sync/models.rs:24`) keeps its variants; what changes is who
may write `Failed` and when a package is allowed to rest in a pending state.

- **True finals:**
  - `Confirmed` — ack received. An ack whose receipts reject frames
    (integrity reject) is still a completed exchange: the package confirms
    and the per-frame `rejected` outcomes live in `sync_history`, exactly as
    today. A deliberate "no" from the receiver is terminal by design — it is
    not a network failure and is not retried.
  - **Cancelled** — user action. New `OutboundState::Cancelled` variant (DB
    text `cancelled`); `cancel_package` (`engine.rs:1365`) writes it instead
    of `Failed`. History outcome stays `cancelled`. Legacy rows that recorded
    cancellation as `failed` are indistinguishable from old attempt-failures —
    acceptable; both are retryable history (§4).
  - `Failed` — **local unrecoverable only**: package payload missing/corrupt
    on disk, i.e. re-announcing can never succeed. The attempt-counter path
    to `Failed` is deleted. Existing `Failed` rows in the store are left
    untouched (terminal, retryable via §4).
- **Pending forever:** `Queued`/`Announced`/`Transferring` no longer expire.
  A pending package with `attempts > 0` is presented as **stalled** — a
  *derived* UI state (pending ∧ attempts > 0), not a stored one. The UI shows
  the attempt count, `last_error` (already persisted, Task 9) and the
  next-retry countdown (§2).

### 2. Retry schedule (torrent-style)

Replace fixed 30s × 5 with unlimited attempts on capped exponential backoff:

```
30s → 1m → 5m → 15m → 30m → 30m → … (cap 30m)
```

- Schedule is a small pure function of `attempts` (unit-testable table, §12).
  `SyncConfig` keeps `ack_timeout` as the base rung; `max_attempts` is
  removed (Perseus and tests updated — deliberate contract change, §5).
- **`next_retry_at` column** on `sync_outbound` via the guarded-ALTER pattern
  already used for `last_error` (`store.rs:37` `ensure_outbound_columns`).
  The worker's in-memory deadline heap stays authoritative; the column exists
  so (a) the UI can render a countdown from a plain status poll and (b) a
  restart re-arms honestly instead of pretending the wait never happened
  (re-arm at `min(next_retry_at, now + base)` on startup re-announce).
- **Wake events → instant out-of-band attempt + backoff reset to base** for
  every pending package targeting the affected peer (or all, where the event
  is global):
  - home relay reconnected (the T8+ relay watcher already observes this),
  - peer address/relay updated by the hourly refresh (T8),
  - app restart (existing pending re-announce path — now also resets backoff),
  - manual **Send now** (§4).
- Engine API: `kick(id)` (force attempt now, reset backoff) and
  `kick_peer(node_id)` / `kick_all()` for the wake paths. Attempts counter
  keeps counting monotonically (it is diagnostic, not a fuse).

### 3. Authorized-peers refresh (companion fix — "forever" is useless without it)

The receiver gate consults a device list fetched from the hub at startup only.
Two additions, both hub-API-reuse (no new endpoints):

- **Periodic refresh**: `refresh_authorized_peers` rides the existing hourly
  relay-map refresh cycle (same timer, same failure tolerance — a failed
  refresh keeps the previous list and logs `warn!`).
- **Refusal-triggered refresh**: when the gate refuses an *unknown* peer, fire
  one out-of-band refresh, **debounced to at most one per 5 minutes** so an
  unauthenticated stranger cannot make the node hammer the hub. If the refresh
  admits the peer, the peer's own retry loop (§2) delivers on its next attempt
  — no callback needed.
- Acceptance scenario: add a new machine to the account → existing machine
  admits it without restart, package delivers unattended.

### 4. Resend + send-now (desktop parity with Perseus)

- **`retry_sync_package(id)`** — Tauri command + Axum route (both backends,
  same change), modeled on Perseus `POST /api/retry` (`perseus/src/web.rs`,
  `api_retry`): verify the payload still exists on disk, re-enqueue the same
  content as a **fresh outbound row** (new id), original row untouched.
  Eligible: terminal rows (`Failed`, `Cancelled`, legacy failed).
- **`send_now_sync_package(id)`** — thin wrapper over engine `kick(id)`.
  Eligible: pending/stalled rows.
- **`cancel_sync_package(id)`** — expose the existing engine `cancel`
  (`engine.rs:452`) as a command + route (it currently has no frontend
  surface at all).
- UI: buttons on the Transfers screen rows (§10); Perseus keeps its existing
  Retry and gains Send now (§11).

### 5. Compatibility and boundaries

- **Wire protocol unchanged.** Announce/fetch/ack/receipts as-is; all changes
  are sender scheduling, receiver-local progress observation, store columns.
- Old `Failed` history/store rows remain as-is (readable, retryable).
- Perseus inherits the semantics automatically (shared engine); its web
  `Retry` keeps working; its `/api/status` `inFlight` payload gains the new
  summary fields (§9) — additive JSON, old page code ignores them.
- Tests asserting "Failed after 5 attempts" are rewritten to the new contract
  (no terminal fail from timeouts; backoff schedule; wake resets). This is a
  deliberate semantics change, not test-fitting.

### 6. Tests (Part A)

- Unit: backoff table (attempts → delay, cap); timeout path never writes
  `Failed`; wake event resets backoff and fires an immediate attempt;
  `next_retry_at` persisted/re-armed across engine restart.
- Unit: refusal-triggered gate refresh fires once per debounce window;
  periodic refresh replaces the list; failed refresh keeps the old list.
- Loopback e2e (acceptance): peer offline → package waits (stalled, attempts
  climbing, no terminal state) → peer comes online → delivered + confirmed
  with **zero user action**.

---

## Part B — transfer queue

### 7. Byte progress in the transport layer

The only engine-deep change of this cycle. `fetch_collection_to_dir`
(`sharing/iroh/blobs.rs:236`) switches from a single awaited download future
to consuming the iroh-blobs downloader progress stream:

- Map blob → file name via the collection order / package manifest (each
  package file is its own blob — the granularity already exists in the
  protocol).
- Emit `TransportEvent::FetchFileProgress { package_id, file, bytes_done,
  bytes_total }`, throttled to ≥300ms per file (progress is UI event data,
  never logs — existing rule).
- `TransportEvent::FetchProgress` (batch-level, `sharing/types.rs:77`) becomes
  periodic during the fetch instead of a single completion tick; final
  completion tick retained.
- Loopback transport (`sharing/loopback.rs`) emits synthetic per-file progress
  so e2e tests can assert the contract without a network.
- **Plan-time validation flag:** the exact progress API shape of iroh-blobs
  `=0.103.0` (downloader progress stream vs `remote().fetch()` events) is
  confirmed as the first planning task; the design assumes only "a stream of
  (blob, offset) observations exists", which BLAKE3-verified streaming
  guarantees in some form. Same task checks whether provider-side (upload)
  events are free; if yes, batch-level upload bytes ride the same event; if
  no, upload progress is out of this cycle (non-goal).
- Sender side of the fetch (`serve`) is untouched — no wire change.

### 8. Persistent inbound rows

New `sync_inbound` table (DDL next to `DDL_OUTBOUND`, guarded-ALTER pattern
for future columns): `id`, `peer`, `package_id` (announce id), `state`
(`Announced → Fetching → Ingesting → Done | Failed`), `frame_count`,
`byte_size`, `bytes_done`, `last_error`, `created_at`, `finished_at`.
`UNIQUE(peer, package_id)` — a re-announce after restart updates the same
row. Per-frame outcomes (ingested/duplicate/rejected) stay in
receipts/history as today; a batch whose frames were all rejected still ends
`Done` — the exchange completed, the rejection detail is per-frame. `Failed`
is for fetch/ingest errors only.

- Batch-level fields are persisted (`bytes_done` updated on the throttled
  batch tick, not per file). Per-file bytes are transient: an in-memory
  snapshot in the receiver runtime, streamed to the UI via events (§9).
- After a receiver restart mid-fetch: the sender re-announces (existing
  behavior, §2 makes it prompt), the download resumes — iroh-blobs
  BLAKE3-verification keeps completed ranges, so `bytes_done` recovers
  naturally.
- Receiver loop writes the row at announce-accept, updates state at fetch
  start / ingest start / terminal, mirroring where it emits its stage events
  today (`receiver.rs:425,471,494,545`).

### 9. Status API and events

- `OutboundSummary` (`sync/status.rs:16`) gains: `byteSize`, `nextRetryAt`,
  `lastError`, `fileCount` (names come from the detail command, not the
  summary — keep the 5s poll payload small).
- `SyncReceiverStatus` (`status.rs:52`) replaces `active: bool` with
  `active: InboundSummary[]` (`id`, `packageShort`, `peerShort`, `state`,
  `frameCount`, `byteSize`, `bytesDone`, `createdAt`); keeps
  `receivedTotal`.
- New command/route **`list_transfer_files(direction, id)`** → per-file rows
  for one expanded batch: outgoing — names + sizes from the package dir
  (Perseus already derives this for `SentDto.files`) plus per-frame outcome
  from receipts once confirmed; incoming — names + sizes from the manifest +
  live `bytes_done` from the in-memory snapshot.
- New event **`sync-file-progress`** (payload = `FetchFileProgress` + row id),
  emitted only while a fetch is active; the Transfers page subscribes while
  mounted (cancelled-flag listener pattern, as everywhere). `sync-progress` /
  `sync-finished` keep their shape (additive `bytes` fields only).
- Transfer speed is computed client-side from byte deltas between events —
  the engine never calculates rates.

### 10. Desktop Transfers screen

- New route **`/transfers`** (React Router) + sidebar entry. The existing
  slide-over `TransfersPanel` stays as the quick glance; it gets an "open
  full screen" link and its Active tab reuses the new summaries.
- Screen layout (torrent-client style): one table, direction column (↑/↓),
  columns: peer/device, batch (package short + file count), status chip
  (with **stalled** badge = pending ∧ attempts > 0), progress bar
  (`bytesDone/byteSize` for incoming; state-staged for outgoing), size,
  speed, attempts + next-retry countdown, actions.
- Actions per row: **Send now** (stalled/pending outbound), **Cancel**
  (pending outbound), **Resend** (terminal outbound). Incoming rows have no
  actions in v1.
- Row expands to the file list (`list_transfer_files`): incoming files show
  live per-file progress bars driven by `sync-file-progress`; outgoing files
  show name + size, and per-frame outcome chips (ingested/duplicate/rejected)
  once the batch confirms.
- **History** becomes the third tab of the screen (existing `HistoryTab`
  content, grouped by batch like Perseus's batched history).
- Design tokens throughout; `formatTimestamp` for times; notifications stay
  on discrete outcomes only (no notification changes in this cycle beyond
  wording that no longer says "failed after N attempts").

### 11. Perseus web parity

Same engine → same semantics for free. `index.html` changes only:

- Sent table: next-retry countdown (from `nextRetryAt`), **Send now** button
  for stalled rows (`POST /api/kick` with `{id}`, thin wrapper over engine
  `kick(id)`, mirroring the desktop `send_now_sync_package`), stalled badge,
  batch byte total.
- Existing Retry button now also covers `cancelled` rows.
- In-flight table gains the byte total; per-file receive progress is N/A
  (Perseus does not ingest). Upload progress only if §7's provider-events
  check lands in-cycle.

### 12. Tests (Part B)

- Unit: progress throttling (no more than one event per file per window);
  blob→file mapping; inbound row lifecycle (state transitions, UNIQUE
  re-announce update, `bytes_done` monotonicity).
- Loopback e2e: receive a batch → `sync-file-progress` events are monotonic
  per file and end at `bytes_total`; `sync_inbound` row walks
  `Announced → Fetching → Ingesting → Done`.
- The §6 acceptance e2e also asserts the receiver has a visible
  `sync_inbound` row already in `Fetching` while the transfer is live —
  incoming visibility is part of the acceptance bar.

---

## Cross-references

- `2026-07-06-personal-sync-design.md` — Stage I engine/store this modifies.
- `2026-07-12-perseus-send-workflow-design.md` — Perseus queue UI this extends.
- `2026-07-12-app-send-ui-design.md` — desktop send path feeding this queue.
- `2026-07-15-iroh-transport-hardening-design.md` — the transport state this
  builds on (relay watcher, hourly refresh = §2 wake sources, §3 timer).
- `2026-07-12-sync-dedup-handshake-design.md` — `new/duplicate` counts shown
  in file-outcome chips (§10).
