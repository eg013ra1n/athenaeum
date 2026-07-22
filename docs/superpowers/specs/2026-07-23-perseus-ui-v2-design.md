# Perseus UI v2 — Transfers-style web interface + sender-kind in app Transfers

Date: 2026-07-23. Status: approved by owner (brainstorm session).

## 1. Context and goals

Perseus's local web page grew section-by-section (Account, Status, To Sync,
Capture Directories, Send Targets, Sent, Batched History, History, Retention —
one long scroll, per-file dumps everywhere) and no longer matches the transfers
batch model v2.1 the backend now speaks. The tv2 design (2026-07-20) explicitly
deferred "Perseus UI"; this spec is that catch-up plus the owner's UX asks:

- Make the page look and behave like Athenaeum's `/transfers` (unified
  master-detail list, filter chips, batch names, bottom detail pane).
- Move configuration into a separate **Settings** tab (account, device name,
  capture dirs, send targets, retention policy).
- Batch history gains two independent delete actions: **delete history** and
  **delete the successfully transferred source files** from the capture dirs.
- A batch fanned out to several targets is **one row**, grouped across targets,
  with per-file × per-target delivery visible — so it's obvious when every file
  of a batch is delivered everywhere and the sources are safe to delete.
- Never show long file lists on the main screen (the current page's core
  failure). Files live only in the detail pane.
- Separately, Athenaeum's Transfers page shows **what kind of sender** a
  received transfer came from: full Athenaeum peer or Perseus agent.

## 2. Direction decision (closed)

**Local web UI now; remote management from Athenaeum stays a future idea.**
There has never been a written plan for managing Perseus from inside Athenaeum —
BRD B1a / component C-10 / the personal-sync design all fix the product path as
a minimal local web page (Syncthing-style). Building remote management would
mean a control protocol over iroh (new ALPN or new frozen-index `Msg` variants),
an authorization model for commanding agents, offline handling, and app-side UI
— a full cycle for one-owner benefit today.

**Future (recorded so it isn't lost):** the cheapest remote-management path is
an **HTTP tunnel over the existing iroh connection** — a "Manage" button on the
device row in Athenaeum opens this same Perseus web UI proxied through iroh.
Everything this spec builds carries over 100% to that path. Not in scope here.

## 3. Page structure

Header: Perseus wordmark + connection dot + two tabs: **Transfers** (default)
and **Settings**. Vanilla JS stays; the single `index.html` splits into
`index.html` + `app.js` + `style.css` under `crates/perseus/src/web/`, embedded
into the binary exactly as today (`include_str!` per file). No npm, no build
step — `cargo build -p perseus` remains self-contained. Data flow stays
polling-based (as today); no SSE.

### 3.1 Transfers tab

Top strip — **To Sync** (compact, one line when idle):

- pending counter + **Send N** button (disabled at 0),
- **Auto / Manual** toggle with the quiet-window seconds input (auto only),
- the pending tree is collapsed by default and expands on clicking the counter
  (the tree itself is the existing `/api/pending` rendering, unchanged data).

Below — filter chips, same vocabulary as the app minus receiving (Perseus is
send-only): **All / Sending / Waiting / Completed / Cancelled / Failed**, each
with a count.

Then the **unified list: one row per batch**, grouped by `batch_uuid` across
every target it was fanned out to (today's page shows one row per batch×target —
that duplication is the "junk drawer" effect). Row contents:

- batch `display_name` (T1; uuid fallback for pre-T1 rows), date, file count +
  total bytes,
- one **target chip per destination**: device name + state (sending with %,
  waiting, delivered, confirmed, failed, cancelled, declined) — device names,
  never node-id hex (hex only in the detail pane),
- "attempt N" marker when `generation > 1`,
- row actions (contextual): **Delete files** and **Delete history** (§4), plus
  the aggregate-level send-now/cancel where they apply.

**Detail pane** (bottom, on row select — same pattern as the app's
`TransferDetail`), three sub-tabs:

- **Files** — the batch's files with per-target delivery: rel_path, size, and
  either a compact "3/3 confirmed" or the per-target breakdown when mixed
  (e.g. `confirmed (dedup) on Home · missing on NAS`). This is the "is
  everything delivered everywhere" answer.
- **Targets** — one row per destination: device name, state, progress, error,
  attempt counter, and per-target actions: kick (send now), cancel, retry,
  and resend-as-new for a declined target (T5 divert).
- **Log** — the `sync_events` journal for the batch across its targets
  (already written by the core store; capped 200/batch).

The old **Sent** and **History** sections die, replaced by this list + detail
pane. Their endpoints `/api/sent` and `/api/history` are retired with them
(verify nothing else consumes them during implementation).

### 3.2 Settings tab

Pure relayout of existing sections onto one tab — zero new endpoints:

- **Account** — OTP sign-in/out (existing `/api/account*`).
- **Device name** (existing `/api/device-name`).
- **Capture directories** (existing `/api/capture-dirs`).
- **Send targets** (existing `/api/targets`, `/api/targets/options`).
- **Retention** — policy editor + recent retention passes log (existing
  `/api/retention/policy`, `/api/retention/log`).

## 4. Deletion semantics

Two independent actions per batch row. Both are server-verified — the UI's
enabled/disabled state is advisory, the axum handler re-checks everything.

### 4.1 Delete files (source cleanup)

Deletes the batch's **source files from the capture directories**. The batch
row stays in history, marked **files deleted** (new nullable column
`perseus_batch.files_deleted_at`, additive `ALTER`/`CREATE` — perseus.db stays
backward-compatible).

**Gate — the obligation model (owner decision):** a file is deletable when
every *open obligation* on it is fulfilled; the batch's button enables when
that holds for every file.

- The unit of judgment is **file × target**, not batch status.
- **Confirmed** fulfills the obligation. A file skipped by the dedup handshake
  ("already on peer") **counts as confirmed** — the receiver verified presence
  against its live catalog at attempt time, and blackholed files count as
  presence (standing owner decision).
- **Receiver decline** and **sender cancel** *close* the obligation without
  delivery — both are explicit human decisions, and they do NOT block
  deletion. The UI stays honest next to the button: "delivered to Home,
  declined on B7PC".
- **Technical failure** (and any non-terminal state) keeps the obligation
  open and blocks deletion — until a retry confirms it, or the operator
  explicitly cancels that target (thereby closing it themselves).
- **Divert interplay (T5):** a declined target diverted via resend-as-new
  relinks the files to the new batch; the gate evaluates a file across ALL
  its batch participations, so either row's button reaches the same verdict.

Execution: resolve source paths through `perseus_batch_files` linkage (seen
store reverse-mapping as the pre-T2 fallback), delete file-by-file, report
partial failures honestly (per-path errors in the response; failed paths stay
listed, `files_deleted_at` set only when the pass finishes clean). Never
delete a path that no longer resolves to the recorded linkage. `perseus_seen`
rows are kept — history and dedup identity survive the deletion.

### 4.2 Delete history

Same meaning as the app's trash action: removes the whole batch **group** —
all its per-target outbound rows, per-file rows, the `perseus_batch` row and
its `perseus_batch_files` linkage. Refused while any target row is non-terminal
(mirror of the app's `Invalid`-refusal). Files on disk are untouched.

## 5. Backend API changes (axum, `web.rs`)

All bearer-gated like every existing `/api/*` route.

- **`GET /api/transfers`** — the new unified read model: batches grouped by
  `batch_uuid`; per batch: display_name, created/updated, generation, files
  (rel_path, size, per-target file state), targets (device name, state,
  progress, error, attempt), aggregate state, `files_deleted_at`, and the
  computed `deletable_files` verdict (with per-target blocking reasons so the
  UI can explain a dead button). Sourced from `sync_outbound` +
  `sync_outbound_files` + `perseus_batch` + `perseus_batch_files`.
- **`GET /api/transfers/events?batch_uuid=…`** — the Log sub-tab's data
  (`sync_events` for one batch across its targets), fetched on demand when
  the detail pane opens so the list payload stays lean.
- **`POST /api/delete-files { batch_uuid }`** — §4.1. Server-side gate
  re-verification, per-path outcome list in the response.
- **`POST /api/delete`** — extended from single-row to the batch group (§4.2).
- Retired: `/api/sent`, `/api/history`.
- Unchanged: everything else (`/api/status`, `/api/pending`, `/api/send-mode`,
  `/api/send-now`, `/api/retry`, `/api/resend-as-new`, `/api/kick`,
  `/api/cancel`, account/settings routes).

## 6. Visual language

**Nord palette** (standing owner rule for new UI), expressed as CSS custom
properties named like the app's design tokens (`--surface`, `--surface-hover`,
`--border`, `--content`, `--content-muted`, `--accent`, `--error`, `--warning`,
`--success`) so the stylesheet reads like the app's Tailwind vocabulary:

- backgrounds/surfaces: polar night (nord0–nord3),
- text: snow storm (nord4–nord6),
- accent/interactive: frost (nord8 primary, nord10 secondary),
- states: aurora — nord11 error/declined, nord13 warning/waiting,
  nord14 success/confirmed.

Dark theme only (matches the current page; the observatory use-case). Layout
mirrors the app's Transfers: full-height list, chips row, bottom detail pane
with sub-tabs.

## 7. Athenaeum: sender kind on received transfers

The receiver already validates an announcing peer against the account device
list — at that moment it knows the device's `DeviceCapability`
(`athenaeum` | `perseus`). Persist it: new nullable column
`sync_inbound.peer_capability` (additive), stamped on announce handling.
Stamping at receive time survives later device revocation; UI-time lookup
would not.

- `InboundSummary` gains `peerKind: 'athenaeum' | 'perseus' | null`
  (snake_case in Rust, camelCase over serde, TS model updated).
- Legacy rows (NULL): resolve live from the current device list when the
  device still exists; otherwise no badge.
- UI: a small badge/icon on received rows (Perseus vs Athenaeum) + a line in
  the Details sub-tab. Design tokens only.
- Both backends in the same change: Tauri command surface and the Axum route
  mirror, per the standing two-backend rule.

## 8. Testing

- **Perseus e2e (`tests/e2e_loopback.rs`):**
  - fan-out to two targets → `GET /api/transfers` returns ONE group with two
    target entries;
  - delete-files refused while one target is unconfirmed (open obligation);
  - delete-files succeeds after both confirm — sources gone,
    `files_deleted_at` set, history row intact;
  - declined target + confirmed target → obligation model allows delete-files
    (and the verdict payload names the declined target);
  - dedup-skipped file counts as confirmed for the gate;
  - delete (history) removes the whole group, refuses while active.
- **Core:** unit test for `peer_capability` stamping on announce; derive of
  `peerKind` fallback for NULL rows.
- **Gates:** `cargo build --workspace`, `cargo build -p perseus
  --no-default-features`, `cargo test -p perseus`, core sync tests,
  `npx tsc --noEmit`.

## 9. Out of scope

- The iroh HTTP tunnel / "Manage from Athenaeum" (recorded in §2 as future).
- Any wire/protocol change — `sharing/` untouched, `Msg` indices frozen.
- Retention policy semantics (only relocated into Settings).
- Light theme for the Perseus page.
