# Perseus 0.5.1 — Local Library, Scheduler, Preview, Disk & Retention Transparency

**Date:** 2026-07-26 · **Status:** approved design, pre-plan · **Target:** 0.5.1 (branch `0.5.1`)

Perseus grows from a send-pipe into a *local-library-oriented* agent: the operator
can always see what the agent watches, send anything (again) anywhere, delete
anything at any moment with honest consequences, schedule sends by wall-clock,
preview frames for a pre-blink, and understand exactly when retention will (or
will never) delete their files.

## Decisions (owner Q&A, 2026-07-26)

1. **Scheduler model = fire-at-time**, not transfer windows. One or more local
   times per day; at each point the whole pending set flushes as one batch; the
   upload may run as long as it needs afterwards.
2. **Library browser scope = browse + send + delete.** No move/rename/mkdir.
3. **Preview = full rustafits with turbojpeg (JPEG).** The arm64 risk was
   re-checked and retracted: `build_arm64.sh` builds *natively* inside an arm64
   Debian container (OrbStack `--platform linux/arm64`), so libjpeg-turbo builds
   like on any native host — the container script just gains `cmake nasm`.
   Windows/macOS CI runners already build rustafits for the desktop.

## §1 Library tab — one-panel browser

New "Library" tab in the existing vanilla-JS web page (no npm, same
`include_str!` embedding). Scope is strictly the **configured capture roots**
(`Config::capture_dirs_resolved()`); Perseus internals (data dir, payload/staging
dirs) never appear in listings, and deletion refuses paths under them even if an
operator nests `data_dir` inside a capture root by misconfiguration.

- **Top level = the roots list.** Each root renders as a top-level node carrying
  its volume's free-space chip (§5). All APIs address files as
  `(root_index, rel_path)` — absolute paths never travel on the wire.
- `GET /api/library?root=<idx>&path=<rel>` — lazy, single-directory listing:
  `{ dirs: [{name}], files: [{name, size, mtime, status, retention?}] }`. No
  recursive scans on request; a Pi with thousands of subs lists one directory at
  a time.
- **Status is derived, no new database.** Join of existing stores:
  - batcher pending set → `queued` (waiting for auto/scheduled/manual flush);
  - `perseus_batch_files` (indexed by a new `source_path` index) × live outbound
    rows → `sending` / `delivered` / `declined` per batch, with "in N batches"
    detail;
  - `perseus_seen` → `sent` (recorded, all containing batches terminal);
  - none of the above → `unsent`.
  - `retention` — the per-file fate line from §8, when the policy produces one.
- **Selection** (files and whole directories) → three actions: **Send to…**
  (§1a), **Delete** (§2), **Preview** (§4).

### §1a Send from the browser

`POST /api/library/send { targets, items: [(root, rel)…] }`:

- Builds the batch **now** via the existing `build_batch_package` /
  `enqueue_package_to_all` path; `perseus_batch` row gets mode `browser`.
- Selected files that sit in the batcher pending set are **removed from the
  pending set first** — otherwise the next scheduled/auto flush would send them
  a second time. (Receiver dedup would absorb it, but wasted upload is a bug.)
- Already-sent files are explicitly allowed — "combine one more transfer even
  for files already sent" is this exact button. The receiver's dedup handshake
  decides what actually travels; a fully-duplicate selection confirms as
  "already on peer" (existing behavior, an honest success).
- Vanished-between-listing-and-send files follow the batcher's existing rule:
  dropped with `warn!`, the rest of the batch proceeds (eligible-subset).
- **As shipped**, the response is `{ enqueued, skipped, package_ref }`. `skipped`
  counts exactly the files dropped above, so the page reports "Sent 4 files
  (1 no longer on disk)" rather than leaving the operator to work out why fewer
  files shipped than were picked.

## §2 Deletion — allowed always, honest always

Owner requirement: the user may delete **anything at any moment**. The design
makes consequences explicit instead of forbidding:

| Case | Behavior |
| ---- | ---- |
| File is `queued` (pending, never sent) | Removed from the batcher pending set **first**, then disk-deleted. The scheduler/auto flush never sees it. |
| File is in an **in-flight** batch | The transfer is unaffected — uploads read the package's payload *copy*, not the original (`build_batch_package` copies at flush). UI states: "N selected files are part of transfer #X — it will complete from its packaged copy." |
| File was in a **confirmed** batch | Deleted freely. Consequence: that batch's *Send again* / *Send to device* (§6) degrade by the eligible-subset rule — "97 of 100 (3 deleted locally)"; at 0 remaining the affordance disables with the reason. |
| File **reappears** later (re-copied from camera media) | On delete Perseus stamps `seen.mark_deleted` (the retention deleter's existing mechanism), so an identical `(size, mtime)` re-creation is treated as NEW and gets sent. Without this the seen-store would silently skip it — the worst quiet failure in this matrix. **As shipped the stamp alone is not enough within one agent run**: the watcher's in-process emitted-paths set short-circuits *before* the seen store, so every in-app deletion (Library §2 *and* retention) also broadcasts a batched `WatcherForget::forget` for the paths it unlinked (T9b) — a re-creation re-enqueues on the next sweep. A file deleted **outside** Perseus (SSH, Finder, the capture software) gets neither the stamp nor the forget: its seen row stays live, so a re-creation is re-sent only if its `(size, mtime)` differ — a byte-identical, mtime-preserving re-copy (`cp -p`, `rsync -t`, some camera-media copies) matches the live row and is treated as already sent, restart or no restart. That is deliberate: auto-forgetting on a stat flap would re-send half a night's data off a network share that blinked. The in-app delete is the supported way to make a file re-sendable. |
| Delete races a flush / scheduled fire | Already safe: the batcher drops vanished files with `warn!`; a flush is never fatal. |
| Delete during a preview render | Render returns an error; UI shows "file gone" and refreshes the listing. |
| Directory delete | Recursive, per-file rules above; a per-file failure (e.g. Windows sharing violation while the flush copies it into a payload) is reported per file and the rest proceed; the directory is removed only once emptied. |
| Overlap with obligation-gated **Delete source files** | That flow already treats an already-missing file as already-deleted (idempotent) and keeps its audit. |
| Audit | Every manual deletion writes the same retention-style audit row with actor `manual-web` (path, size, timestamp) — visible in the retention log beside `retention_deleted` rows. The row lands **before** the unlink (retention's own audit-before-delete contract), so a failed unlink can leave an audit row for a file that is still on disk. That asymmetry is the intended one — a deletion is never invisible — and it is the same shape retention already has. |

API: `POST /api/library/delete { items, confirm }` — two-step on one route.
`confirm: false` deletes nothing and returns the per-item preview (counts, the
in-flight/pending/confirmed-batch warnings above); the UI renders that in the
confirm dialog; `confirm: true` performs and returns per-item outcomes
(`deleted` / `error(reason)` / `refused(internal-path)`).

## §3 Scheduler — calendar auto-send

Config (`[send]`): `mode = "auto" | "manual" | "scheduled"` (existing enum grows
one variant), `schedule_times = ["06:00", "14:30"]` (local device time,
`HH:MM`), `schedule_catchup = true` (default).

- **Batcher third arm.** `Mode::Scheduled` arms `sleep_until(next_fire)`; firing
  performs the same drain-and-flush as the manual button, batch mode
  `scheduled`. Empty pending → no-op; empty batches never exist. Mode and times
  are live via the existing `watch` channel (no restart).
- **Catch-up.** `last_scheduled_fire` is persisted in a new single-row
  `perseus_meta` key-value table in `perseus.db` (additive DDL, same pattern as
  the other Perseus-only tables). At
  startup, if at least one schedule point falls in `(last_fire, now)`, fire
  **once** (never N times for N missed points). Governed by `schedule_catchup`.
- **Clock/DST.** `next_fire` is recomputed from the wall clock at every arm; a
  backward clock jump merely re-arms (worst case one extra no-op flush). A
  nonexistent local time on a DST-spring-forward day fires at the next valid
  minute.
- **Collision matrix** (most cells fall out of drain-at-flush + the seen store):
  - Manual send in flight when the scheduler fires → the scheduled batch
    contains **only files accumulated after** the manual drain (owner's stated
    requirement — satisfied by construction).
  - Scheduler batch uploading when the operator presses Send now → allowed;
    takes the current pending set; overlapping outbound batches are supported.
  - Browser-send (§1a) removes its files from pending → no double-send.
  - Two schedule points close together → the second is a no-op if nothing new.
  - Agent asleep across points → one catch-up fire at startup.
  - Mode flipped to `scheduled` with a non-empty pending set → next point takes
    it; nothing is lost or double-armed.
- **UI.** Mode radio (Immediately / On schedule / Manually) + an HH:MM list
  editor; status header shows "next scheduled send: …". The web page's existing
  "Send N pending now" button remains in every mode. **As shipped these live in
  the Transfers tab's "To Sync" strip, not Settings** — they sit beside the
  pending count and the Send-now button they govern, which is where the operator
  is already looking when they decide when the night should leave.

## §4 Preview — pre-blink

- Perseus takes a **direct dependency on `rustafits`** (not core's `render`
  feature — that would drag the desktop render-processor module along). Cargo
  feature `preview`, **in default features and kept in the headless variant**
  (the web page *is* the UI on a Pi); the arm64 container gets
  `apt-get install cmake nasm`. Binary cost ≈ +3–5 MB.
- `GET /api/library/preview?root=<idx>&path=<rel>&w=<px>`:
  - width clamped to ≤1600 px; JPEG out;
  - a **semaphore of 1** serializes renders (a Pi must never run two stretches
    at once);
  - LRU cache of 8 rendered previews keyed `(root, rel, size, mtime, w)`; the
    key is also the ETag, so a repeated blink pass rides 304s;
  - non-FITS/XISF or unreadable file → clean 4xx with a reason the UI renders.
- **UI:** preview pane over the listing with ←/→ (and keyboard arrows) walking
  the current directory's files — that walk *is* the pre-blink.

## §5 Free disk space

New `diskspace.rs`: unix `libc::statvfs` (libc already a dependency), Windows
`GetDiskFreeSpaceExW` via a minimal `windows-sys` feature set. Resolved per
**unique volume** across `capture_dirs_resolved()` + `data_dir`; UNC roots work
(the Windows API accepts UNC paths; statvfs on an SMB mount reports what the
server exports). Exposed on `GET /api/status` as
`volumes: [{root, free_bytes, total_bytes}]`; UI shows a chip per root in the
Library tab and the status header, red under a fixed 10 GB threshold.
Display-only in v1 — no alerts, no config.

**Shipped gap, deliberate.** Because the dedup keeps only the *first* requested
path as a volume's label, two **sibling** roots on one disk (`…/cam1`, `…/cam2`)
collapse to a single entry labelled `…/cam1`, and nothing on the wire says the
second belongs to it. The UI matches a root to a volume by longest containing
prefix, so the sibling gets **no chip at all** rather than a guessed one — for an
unmounted NAS root a guess would print the local disk's free space under the
NAS's name, the exact wrong-disk reading `diskspace.rs` refuses to commit by
never resolving a path to an ancestor. Closing the gap needs the status DTO to
say which volume each root sits on; noted as a follow-up, not done in v1.

## §6 Send a previous batch to another node

Transfers history rows gain **Send to device…** (picker of this node's send
targets):

- Mints a **new** transfer — fresh payload dir basename ⇒ fresh wire
  `batch_uuid` ⇒ a brand-new inbound row on the chosen peer. The original row
  and its history are untouched.
- Payload is rebuilt from `perseus_batch_files` source linkage via the existing
  confirmed-rebuild machinery (`resend.rs` / `rebuild_package_payloads`),
  extended to accept a target peer different from the row's own.
- Missing sources → eligible-subset: "sends 97 of 100 (3 deleted locally)" on
  the confirm; 0 → disabled with reason.
- The receiving side's dedup handshake trims whatever that node already has.
- **Picker amendment (shipped).** The draft above said "the account's
  receive-capable devices, same source as the targets editor". It is not: the
  picker (shared by §1a and §6 through one `loadSendTargets` read) offers
  `GET /api/targets`'s **`runtime`** list — the targets the supervisor handed the
  *running engines* — which is a subset of the configured list, itself a subset
  of the account's devices. Offering an account device this node has no engine
  for would be a guaranteed `400 unknown send target`; a target saved under
  Settings but not yet applied is named as pending instead of listed.

## §7 Multiple roots, Windows paths, SMB

- **Multiple capture roots are already native** (`capture_dirs`, one watcher per
  root, `(capture_dir, file)` flowing through the whole pipeline). The Library
  tab, §1a send, §2 delete, §4 preview and §5 free space are all specified in
  `(root_index, rel_path)` terms, so N roots is the base case, not an
  extension.
- **Wire path contract.** Rel paths travel in forward-slash form. The server
  splits on `/`, rejects any segment that is empty, `.`, `..`, or contains
  `\\`, `:` or NUL, then joins with the native separator. Roots come from
  config and may be `C:\…` or UNC `\\server\share\…`.
- **Containment guard.** Root and resolved candidate are both canonicalized and
  prefix-compared canonical-vs-canonical (on Windows both come back as
  `\\?\C:\…` / `\\?\UNC\…`, so the comparison is consistent). Symlinks that
  escape the root fail the guard. Every library API passes this guard; there is
  exactly one implementation.
- **SMB: fix the watch-establish hole.** The watcher's design already carries
  SMB: `notify` events only *seed* candidates, and every poll tick runs a full
  `scan_eligible` sweep — discovery works even when the notify backend is
  silent (as it typically is over SMB/NFS). But `watcher.watch(&dir)?` at
  startup is a hard error: on network filesystems establishing the watch can
  fail, which today would kill that root's watcher *including* its poll
  fallback. Change: a failed `watch()` degrades to `warn!` + **poll-only mode**
  for that root. This single change is what makes SMB a supported
  configuration.
- **Offline share.** Startup canonicalize already falls back gracefully; an
  unreachable root's poll ticks emit a rate-limited `warn!` and recover when
  the mount returns. The Library tab shows an honest listing error for an
  offline root, not an empty directory.
- **Poll cost.** A recursive sweep every `poll_interval_secs` (default 2) can
  hammer a NAS. The knob exists; the Settings page documents "raise this for
  network shares". Per-root intervals are out of scope for v1.
- **Known, inherited consideration.** Some SMB servers report coarse mtimes;
  the stability window already keys on `(size, mtime)` and this design does not
  change that behavior — recorded here so it isn't rediscovered as a new bug.

## §8 Retention transparency

Facts (verified against `sync/retention.rs` + `config.rs`, restated for the UI):
candidates come from **one chokepoint** — confirmed packages only; `keep_days`
ages from the row's **`confirmed_at`** (not capture time) **per confirmed row**
— see the correction below; `keep_everything` is the default and short-circuits
before any probe; `dry_run` defaults on and live deletion additionally requires
`i_have_verified_the_soak = true`; passes run every `interval_secs` (default
hourly); every candidate is audited before deletion and the file is verified to
exactly match what was transferred.

> **Correction (shipped behavior).** An earlier draft of this section said a file
> becomes deletable "N days after **every target** confirmed receiving it". That
> is wrong, and it is wrong in the dangerous direction — it promises a delay the
> code does not honour. The evaluator draws its candidates from
> `store.confirmed()`, i.e. **every confirmed row, aged individually**. A package
> fanned out to three devices is therefore a candidate as soon as its
> **earliest** confirmation ages out; it never waits for the slowest target. The
> card copy and the per-file fate line below are the shipped, corrected wording.

- **Settings → Retention card**, generated from the *effective* config:
  - `keep_everything`: "Perseus never deletes capture files. Manual deletion in
    Library, and a batch's *Delete source files*, are the only paths."
  - `on_confirm`: "A file is deleted on the first pass after a target confirms
    receiving it — there is no grace period."
  - `keep_days`: "A file becomes deletable **N days after it was confirmed
    received** — the clock starts at the confirmation, not at capture. When a
    batch went to several devices, **the earliest confirmation starts it**."
  - `disk_pct`: "While a capture volume sits at or over **N %**, confirmed files
    are deleted oldest-confirmed-first until usage drops back under the cap.
    Below the cap nothing is deleted."
  - **Cadence, not a countdown.** The card states "Checked every hour" (from
    `interval_secs`) plus the **last recorded pass**, not a "next pass: <time>"
    as drafted: the retention loop is a plain `sleep(interval)` select with no
    exposed deadline, so a next-pass clock would be a number the agent never
    promised. A pass whose record carries `errors` — a failed/panicked tick, or a
    pass that ran and left the volume over the cap — renders as "did not complete
    cleanly", never as a quiet 0-candidate pass.
  - Mode banner: yellow **DRY-RUN — nothing is deleted, candidates are only
    logged**, or red **live deletion armed** (two-key opt-in visible). Link to
    the existing retention log (`/api/retention/log`).
- **Per-file fate in the Library listing** (the `retention` field of §1):
  `keep_days` + confirmed → "deletable after <confirmed_at + N>" ("would be
  deletable after …" in dry-run); `disk_pct` → "deleted under disk pressure,
  oldest first"; unsent/unconfirmed → "kept until sent and confirmed";
  `keep_everything` → no fate line at all.
  - The anchor is the **earliest usable `confirmed_at` across the targets of the
    package the file is still LIVE-linked to** (`SeenStore::package_for_path`) —
    same rule as the card, and read through the one linkage that can actually
    delete the file, not through its whole batch-participation history. The map
    separates *confirmed at all* (the key) from *when* (the value), because
    `on_confirm`/`disk_pct` never read a timestamp: a confirmed-but-undated
    package still gets their line, while `keep_days` degrades to **no line at
    all** rather than naming a wrong date or falsely claiming the file is kept.
    `keep_everything` skips the whole confirm scan *and* the per-file linkage
    lookup; so does an empty confirm map (nothing on the node has confirmed yet).
- The card explicitly separates **manual deletion** (§2, always available,
  audit actor `manual-web`) from **retention** (`retention_deleted`) so the log
  reads unambiguously.

## §9 API & config surface (summary)

New routes: `GET /api/library`, `GET /api/library/preview`,
`POST /api/library/send`, `POST /api/library/delete`,
`POST /api/transfers/send-to`; `GET /api/status` gains `volumes` and
`nextScheduledSend`. Config: `[send] mode` gains `scheduled`,
`schedule_times`, `schedule_catchup`; no new tables — one new index
(`perseus_batch_files(source_path)`) and one meta row (`last_scheduled_fire`).
Cargo: `rustafits` behind default feature `preview`; `windows-sys` (tiny
feature set) on Windows targets.

## §10 Testing

- **Unit:** listing + status join (each status source); containment guard
  (traversal, `..`, backslash smuggling, symlink escape, UNC roots); §2 matrix
  — one test per row, including the seen-`mark_deleted`-then-reappear case;
  next-fire math table (multiple times, midnight wrap, DST spring-forward,
  backward clock); catch-up fires exactly once; eligible-subset rebuild with
  deleted sources; free-space volume dedup.
- **Route tests** (existing tower-util pattern): every new endpoint, including
  refused internal paths and the in-flight-deletion warning payload.
- **E2E (existing two-node harness):** scheduler fires while a manual batch is
  in flight → second batch carries only post-drain files; browser re-send of
  fully-duplicate selection → confirms as "already on peer"; send-to-device of
  a confirmed batch lands as a new transfer on the second node.
- **Watcher:** `watch()` failure degrades to poll-only and still discovers
  files (fault-injected watcher backend).

## Non-goals (v1)

Move/rename/mkdir in the browser; cron expressions or transfer *windows*;
thumbnail grids (single preview pane only); free-space alerts/thresholds config;
per-root poll intervals; any change to stability-detection semantics.
