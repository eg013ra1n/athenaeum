# Transfer preparation, single-copy footprint, and the Transfers settings tab — design

Date: 2026-08-30
Status: approved for planning

## 1. Goal

Three things the owner hit on the LDN 1272 send (562 files, 29 GB, this Mac →
the observatory pod) and asked to fix together:

1. **Send blocks the dialog.** `Send to node…` hashes and copies the whole
   package before the command returns — 162 s for that transfer, with a
   spinner and no figures. The send should land in the Transfers list at once,
   in a visible **preparing** state with a byte/file progress line, and the
   dialog should close immediately.
2. **Every transfer costs two to three copies of its data.** The sender holds
   the `packages/<uuid>` snapshot AND a full copy in the iroh blob store; the
   receiver holds the blob-store download, an exported copy in `staging/`, and
   the landed file. On APFS the extra copies are clones (≈ free); on ext4 /
   NTFS / Docker overlay they are real. Both ends should hold **one** copy of a
   package's data at rest, plus the small BLAKE3 outboards iroh needs.
3. **The operator has no say over where that transient data lives.** Both ends
   should get a folder setting, on a dedicated **Settings → Transfers** tab that
   also gathers the existing transfer knobs (upload limit, concurrent receives,
   storage / clean-up).

What the user sees afterwards:

- Click *Send* → dialog closes within a second → a row `LDN 1272 · Pod ·
  preparing · 84 of 300 · 4.2 GB / 15.6 GB · 180 MB/s` in Transfers, with
  *Cancel*. It proceeds through `queued` (indexing), `announced`,
  `transferring` exactly as today.
- Storage on the sender ≈ one copy of the package (APFS: ≈ 0) + ~0.4 % for
  outboards. Storage on the receiver ≈ one copy of what landed, plus the
  in-flight download until it lands.
- Settings → Transfers: *Outgoing staging folder*, *Incoming working folder*,
  upload speed limit, max concurrent receives, storage + clean-up.

### Non-goals

- Resuming an interrupted preparation across a restart (v1 fails the row
  honestly; the user sends again — §3.6).
- Sharing one prepared package across several destinations (N destinations =
  N preparations, as today — only now visible — §3.7).
- Changing Perseus's import mode or its own `packages_dir` config (§4.3).
- Touching the collab swarm path (`fetch_collection_multi`, `collab_seed`
  hardlink dirs) — it already has its own zero-copy seeding (D3 §3.4) and its
  export semantics stay as they are (§5.5).
- Live re-binding of the iroh node when the working folder changes; it applies
  at the next start (§6.4).
- Migrating existing store/staging content to a new working folder (§6.5).
- Any wire or manifest change. `Announce3`/`Announce4`, the dedup handshake and
  the ack/receipt protocol are untouched.

## 2. What exists and is reused

Facts verified in code on 2026-08-30 (iroh-blobs 0.103.0, iroh 1.0.3):

- **Send path** — `api::sync::enqueue_frame_set_send` /
  `build_and_enqueue_selection` → `build_selection_package` (per file:
  `package::xxh3_full_file` = one full read, then `package::write_package` =
  `fs::copy` per file + `manifest.ndjson`) → `enqueue_built` →
  `SyncEngineHandle::enqueue_package` → engine `Command::Process` inserts the
  `Queued` row + per-file rows in one transaction (`store.enqueue`) and drives
  it (`start_package`). Everything before `enqueue_built` runs inside the
  command; the Tauri command and the Axum route both block for its duration.
- **Serve import** — `IrohTransport::serve` →
  `blobs::import_package_collection` (`ImportMode::Copy`, doc: "the safe
  default for a dir the app may rebuild or mutate (a Perseus resend rewrites
  its payloads in place)"); the want-subset path `import_subset_collection`
  calls `add_path` with the default mode (also Copy). The D3 seeding path
  already uses `import_package_collection_with_mode(…, TryReference)`.
- **iroh `ImportMode::TryReference`** (`store/fs/import.rs:469`): a file above
  the inline threshold is opened only to compute the outboard; the entry's
  `data_location` becomes `External(vec![path])`. No same-filesystem
  requirement (it is a path reference, not a hardlink). `finish_import_impl`
  builds the location from THIS import only, but `update_await` goes through
  `meta.rs::handle_update` → `EntryState::union`, which **unions** the external
  path list, sorts it and dedups it (`store/fs/entry_state.rs:44`); every reader
  takes `paths.first()`. So a re-import of an already-known hash from a new path
  does **not** re-point the entry: re-importing the SAME path is idempotent, but
  a stale path that sorts first keeps winning the read while a live tag pins the
  entry against GC. `Owned` beats `External` from both sides of that union
  (`entry_state.rs:58`/`:63`), so a `Copy` re-import repairs such an entry
  permanently — which is what §4.2's probe does. `has`/`status` answer from the
  metadata row and do not notice; only a read (one byte via `export_ranges`)
  does.
- **iroh `ExportMode::TryReference`** (`store/fs.rs:1284`): if the entry is
  store-owned, `rename` to the target (EXDEV → `reflink_or_copy` fallback) and
  the entry becomes `External([target])`; "setting the new entry state will
  also take care of deleting the owned data file". If the entry is already
  external, it **copies** from `external[0]` and appends the target. Blob
  deletion (`Blobs::delete*`) is `pub(crate)` in 0.103 — a dead external path
  is reclaimed only by GC after its tags are gone (`GC_INTERVAL`, ≤ 15 min).
- **Receive path** — `fetch_collection_to_dir` downloads into the store, then
  `store.blobs().export(hash, target)` (= `ExportMode::Copy`) into
  `<working>/staging/<wire_id>/…`; `ingest::land_payload` then `fs::copy`s the
  staged file to `<landing>/<rel>.tmp` + `rename`. Staging is cleaned in the
  package's cancel/ack epilogues.
- **Paths** — `api::sync::sync_paths` = `(<db dir>/sync, db_path)`. Under it:
  `device_key` + `device_key.lock` (identity — `SharedIrohNode::bind` loads
  them from the same dir it opens `blobs/` in), `packages/` (sender), `blobs/`
  (one `FsStore` for every role), `staging/` + `incoming/` (receiver),
  `collab_*`. `get_transfer_storage` walks `packages/`, `blobs/`, `staging/`.
- **Status** — active outbound rows come from each engine's
  `store.non_terminal()` (a DB read), summarized by `outbound_summary` (which
  reads `byte_size`/`file_count` off the on-disk manifest via
  `package_totals`) + `outbound_file_counts` (one grouped query, now carrying
  `duplicate`/`duplicate_bytes`). Display state: `Queued → queued`,
  `Announced → preparing`, `Transferring → transferring`, ….
- **Events** — `sync-progress` (`stage`, optional `bytes_done`/`bytes_total`),
  `sync-file-progress` (per file), `sync-finished`; the frontend's
  `useTransferQueue` already turns any Sent-direction `sync-progress` with
  bytes into the row's live bar + EMA speed.
- **Settings page** — tabs `general | calibration | analysis | plate_solving`
  via `?tab=`; `SyncSection` (upload limit, concurrent receives, storage +
  clean-up, pairing/ticket) and `AccountSection` sit in General. Folder
  picking: desktop `api/desktop.ts::pickDirectory`, web `FolderBrowserModal`.
- **Scan-root overlap check** — `api::scan_roots::add_scan_root` rejects a
  path equal to, inside, or containing an existing root (canonicalized,
  `starts_with`). Reused as a helper for the new folder validation.

## 3. Sender: preparation as a visible, cancellable phase

### 3.1 State

`OutboundState::Preparing` (DB string `"preparing"`), non-terminal, ordered
before `Queued`. `outbound_display_state` maps it to `"preparing"`; to free
that label, `Announced` now maps to `"announced"` (the chip already exists in
`presentation.ts`; `stageProgress` gets an `announced` arm at the same 0.08
rung, and the status tests are updated).

### 3.2 Command contract (both backends)

`enqueue_sync_selection` / `enqueue_frame_set_send` keep their signatures and
`EnqueueSelectionResult`, but now return as soon as the row exists:

1. Resolve entries as today (`selection_entries` / `frame_set_entries`,
   `check_mode_ready` gate). Empty → no-op result, no engine, as today.
2. `ensure_sender_engine` for the destination (unchanged order — the engine
   must exist for the row to appear in Active and for the peer address).
3. Pre-flight per entry: `stat` only (exists, size). Missing → `ineligible`
   (reason as today). No hashing, no copying.
4. Mint `pkg_dir = <outgoing_staging>/<uuid>` and insert the row in ONE
   transaction: `sync_outbound(state='preparing', package_ref=pkg_dir,
   display_name, layout)` + `sync_outbound_files` rows (`pending`, sizes from
   `stat`) via a new store method `insert_outbound_preparing(…)` — the same
   `insert_outbound_with_files` body with a state parameter. Journal
   `enqueued frames=N bytes=B`.
5. Hand `(id, pkg_dir, entries, batch metadata)` to the preparation worker
   (§3.3) and return `EnqueueSelectionResult { enqueued_count = eligible,
   … }`.

Errors before step 4 (peer unresolvable, engine start failure, DB) are still
synchronous command errors, so the dialog's per-destination error handling is
unchanged.

### 3.3 Preparation worker

`api::sync_prepare` (new sibling module — `api/` is flat). One
`PrepareRuntime` on `SyncSenderRuntime`:
`Semaphore(1)` (one package copies at a time — two sends must not fight over
the source disk) + `Mutex<HashMap<i64, CancellationToken>>`.

Per package, a `spawn_blocking` task:

1. Create `pkg_dir`. For each entry, in manifest order:
   - **reflink first**: `reflink_copy::reflink(src, dest)` — `reflink-copy`
     0.1.30 is already in `Cargo.lock` through iroh-blobs and becomes a direct
     dependency of `athenaeum-core`. Success → hash the destination
     (`xxh3_full_file`; the clone shares extents, one read).
   - **else one-pass copy + hash**: stream `src` in 1 MiB chunks, updating
     xxh3 and writing `dest`; verify the written size equals the `stat` size
     (as `write_package` does today).
   - Bank the xxh3 as `files.strong_hash` under `disk_matches_row`, exactly as
     `build_selection_package` does now (one transaction at the end; a bank
     failure never fails the send).
   - Check the cancellation token between files and every 64 MiB inside a
     file.
2. Write `manifest.ndjson` (`package::write_manifest` — the manifest half of
   `write_package_with_root_hash`, split out so the writer no longer copies).
3. Flip the row `preparing → queued`, settle nothing (per-file rows stay
   `pending`), journal `prepared files=N bytes=B duration_ms=…`, and send the
   engine `Command::Drive(id)` — "a `queued` row exists on disk and in the DB;
   read it and drive it like a crash-resume". Same handler path as
   `Command::Resend`, without its "row was reset by the API layer" wording.

Progress: `sync-progress { direction: Sent, stage: "preparing", bytes_done,
bytes_total }` throttled to ≥ 300 ms (`bytes_total` = sum of `stat` sizes), and
`sync-file-progress` per file (throttled mid-file, always on completion).
Preparation writes **no per-file state**: the `pending → sending → uploaded`
rungs mean "bytes to the peer" and stay reserved for the serve phase. The
byte bar carries the preparation progress; the file counter is not shown for
a preparing row (§7.1).

### 3.4 Cancel

`cancel_sync_package(id)` first looks the row up: `preparing` → cancel the
token, wait for the task to exit, remove `pkg_dir`, mark the row `cancelled`
(`last_error = None`), settle per-file rows `done/cancelled`
(`settle_unsettled_files` semantics, now callable from the API layer), journal
`cancelled by_user`, emit `sync-finished { outcome: "cancelled" }`. Any other
state → the engine's `Command::Cancel`, as today.

### 3.5 Failure

A source that vanishes or fails to read mid-way, a full destination disk, a
manifest write error: the worker stops at the first error, removes `pkg_dir`,
marks the row `failed` with `last_error = "preparation failed: <file>: <io
error>"`, settles per-file rows (`failed` + `error` for the culprit, the rest
`done/cancelled`), journals `prepare_failed`, emits `sync-finished { outcome:
"failed" }`. The row is terminal and **not resendable** (no payload) — the
detail pane's reason line says why and the user sends again from the source
page. Never swallowed: `error!` with `package_id`, `path`, `error`.

### 3.6 Restart heal

Before `resurrect_pending_senders` runs, `heal_interrupted_preparations`:
every `preparing` row → `failed` ("preparation interrupted by a restart — send
again"), its `package_ref` dir removed if present, per-file rows settled,
`warn!` per row. The existing orphan sweep is untouched (a `preparing` dir has
a row, so the sweep never saw it as an orphan; the heal is the one owner).

### 3.7 Several destinations

The dialog's fan-out is unchanged: one command per destination, one row and
one preparation each, serialized by the semaphore, each visible as its own row
with its own progress. Sharing one prepared package is a non-goal (the
multi-target payload coordinator exists for Perseus fan-out; wiring the app's
sends into it is a separate cycle).

### 3.8 Summary fields for a preparing row

`package_totals` reads the manifest, which does not exist yet. Add
`total_bytes` (`SUM(byte_size)`) to `TransferFileCounts` (same grouped query),
and let `outbound_summary` fall back to `(file_counts.total,
file_counts.total_bytes)` when the manifest is unreadable. A legacy row with no
per-file rows keeps today's `(0, 0)`.

## 4. Sender: reference import instead of a second copy

### 4.1 Mode

`SharedIrohNode::bind` takes `NodeOptions { serve_import_mode: ImportMode }`.
The app (`api::sync::ensure_iroh_node`) passes `TryReference`; Perseus passes
`Copy` (§4.3). `IrohTransport::serve` threads the mode into
`import_package_collection_with_mode` AND into `import_subset_collection`,
which grows a `mode` parameter and passes it to `add_path_with_opts` (today it
calls `add_path`, i.e. Copy — a silent second copy on every want-subset send).

Outcome: for an app send the store holds the collection, the hash-seq and the
per-blob outboards (BLAKE3 tree, 64 B per 16 KiB ≈ 0.4 %); the payload bytes
are read from `packages/<uuid>/…`. Hashes are mode-independent, so the
announced `root_hash` is identical either way (existing invariant, pinned by a
test — §10).

### 4.2 Invariant and lifecycle

The invariant TryReference demands — the file never changes after import — is
already the app's: `packages/<uuid>` is written once by preparation and
touched again only by `cleanup_package_payloads` (delete) after confirm.

Order on confirm is **protect → cleanup → release**, all three on one detached
task (`engine.rs::spawn_protect_cleanup_release`; the event dispatch itself stays
synchronous, so a package still cannot be confirmed twice by interleaving):

1. `SharingTransport::protect_shared_before_cleanup` — a new hook with a no-op
   default (loopback, Perseus, the legacy transport need nothing). The iroh
   implementation copies into the store every child of this package that ANOTHER
   live hash-seq tag also references, making it `Owned` (which wins the union
   from both sides) while the payload files are still on disk. Without it, two
   packages carrying the same frame share one entry whose first external path
   belongs to whichever finishes first — and the survivor's tag then keeps that
   dead entry alive past GC, so nothing would heal it. Source is our own file,
   falling back to the sharing package's if ours is unexpectedly gone.
2. Cleanup — **skipped** if step 1 failed: the payload stays on disk (Settings →
   Sync's "clean up finished transfers" reclaims it later) rather than being
   deleted out from under another transfer. `error!` either way; the row is
   already confirmed.
3. `release` — the tag delete, as before.

The re-serve short-circuit probes for a related reason: it skips the import, and
with it the import's own repair.

Between cleanup and the next GC pass the store may hold entries whose external
path is gone; nothing reads them (a manifest-only dir fails
`package_has_payload`, so it can never be re-served). A later import of the
same hash from a live path does NOT re-point such an entry — iroh unions the
external paths and reads the first one (§2) — so the import instead **probes
each referenced child for one byte and, on a failed read, re-imports that one
file with `Copy`** (`blobs::ensure_child_readable`): `Owned` wins the union, so
the entry is repaired permanently and the cost is one copy of the affected
file, not of the package. `warn!` on repair, `error!` if it still fails. Cancel
keeps the payload (as today) — resend re-serves the same dir, same bytes, and
the union dedups the identical path.

Declined-divert (`resend_declined_as_new_transfer`) renames the payload dir to
a new uuid: the old row is terminal (tag released on decline), the new row
imports from the new path — the stale sibling path is exactly the case the
probe repairs.

### 4.3 Perseus

Perseus keeps `Copy`: its resend rebuilds payloads in place
(`resend.rs`, "transfer resent in place"), which is exactly the mutation
TryReference forbids. Its own `packages_dir` config, retention and fan-out are
untouched. The `served` reuse map and `role_release` behave identically under
both modes.

### 4.4 Import progress

`import_package_collection_with_mode` iterates each `add_path_with_opts`
progress stream (`AddProgressItem::Size` / `CopyProgress` /
`OutboardProgress` / `Done`) instead of only awaiting the temp tag, and calls
a new `SyncSink::route_import_progress(pkg, bytes_done, bytes_total)`
(throttled ≥ 300 ms) → engine `sync-progress { stage: "indexing", bytes }`
while the row is still `Queued`. The frontend renders a `queued` row whose
last live stage is `indexing` with the `preparing` chip, the subline
"indexing", and the byte bar (§7.1).

## 5. Receiver: one copy

### 5.1 Export moves instead of copies

In `fetch_collection_to_dir` the export loop uses
`export_with_opts(ExportOptions { hash, target, mode: ExportMode::TryReference })`.
For a store-owned blob iroh renames its data file into
`<working>/staging/<wire_id>/<rel>` (EXDEV → copy, then the owned file is
deleted anyway); the store now references the staged file. Inline (small)
blobs are written as before.

### 5.2 Landing links instead of copying

`ingest::land_payload`: `std::fs::hard_link(staged, tmp)` → `rename(tmp,
dest)`; on `EXDEV`, `PermissionDenied`, or `Unsupported` (SMB/NFS/exFAT) fall
back to today's `fs::copy` + `rename`. The staged file is **left in place**
until the package's existing epilogue cleanup (post-ack / cancel / revoke),
so the store's external reference stays valid for as long as the collection's
tag lives; after cleanup the tag is already released and GC reclaims the
entry.

Net footprint on the receiver: one copy of every landed file (staging and
landing are the same inode until cleanup; a copy fallback holds two until
then), plus the in-flight download in the store until it is exported.

### 5.3 The same-hash edge

Two in-flight inbound packages that carry a byte-identical file (same hash)
the receiver's catalog does not yet hold — possible only across concurrent
senders or batches, since the dedup handshake excludes what the catalog has:

- Package A exports first (move); B's export finds the entry external and
  **copies** from `external[0]` = A's staged path. While A is un-released the
  path exists (§5.2), so B lands correctly.
- If A was released AND cleaned before B's export, and GC has not yet swept
  the entry (≤ 15 min), B's export fails with `NotFound` on the external
  source. The fetch classifies that error as **transfer-class** (row parks
  `Waiting`, the sender's retry ladder re-announces) rather than
  `LocalFault`, with a `warn!(hash, path, "export source vanished; waiting for
  GC")`.

  The heal needs one more step, because a `Waiting` park never calls `release`:
  **the fetch drops its own collection tag on this error path**
  (`on_export_source_vanished`). Without that, the receiver's tag would pin the
  dead entry against GC indefinitely and every retry would re-run the same
  failing export — a downloader skips a blob whose entry reads `Complete`, dead
  file or not. The in-flight tag is already retired by then (it is deleted right
  after the permanent tag is set), so that one delete leaves the entry untagged;
  GC purges it within one window (≤ 15 min) and the sender's retry ladder then
  re-fetches the blob for real. Self-heals within one GC window; documented as a
  known limitation (§12 D5).

  Known rough edge, upstream: while the entry is dead-but-not-yet-swept, a
  re-fetch that touches it trips an iroh-blobs 0.103 panic in a store entity
  actor (`bitfield()` on `BaoFileStorage::Poisoned`, reached through the fetch's
  own per-file `observe`) and the fetch returns `unexpected end of stream`
  instead of reaching the export. That is still an unmarked, transfer-class
  error, so the row parks `Waiting` exactly as above and the heal is unchanged —
  it costs one noisy retry, not correctness.

### 5.4 Progress

Export and landing already tick per file (`sync-file-progress` from the fetch
sink, `ingesting` stage). No new events; a move is instantaneous, so the
export phase simply stops being a visible pause on non-CoW disks.

### 5.5 Collab swarm path

`fetch_collection_multi` keeps `ExportMode::Copy`: a downloaded project package
is re-seeded from `collab_seed/<pkg>` hardlink dirs (D3 §3.4) and the D3 tests
pin the store-copy semantics. Unchanged.

## 6. Paths and settings

### 6.1 Two folders

| Setting key | UI label | Default | Holds |
| ---- | ---- | ---- | ---- |
| `sync.outgoing_staging_dir` | Outgoing staging folder | `<app-data>/sync/packages` | `packages/<uuid>/…` prepared sends |
| `sync.incoming_working_dir` | Incoming working folder | `<app-data>/sync` | `blobs/` (the one store, all roles), `staging/`, `incoming/` (fallback landing), `collab_*` |

Empty / unset = default. Stored through `SettingsManager` (DB `settings`).

The defaults are exactly today's locations (`<db dir>/sync`), so an install
that never touches the tab changes nothing. `<app-data>` per platform (Tauri
`app_data_dir()` + the build flavor's identifier — `com.vsharifov.athenaeum`,
`.dev` suffix for debug builds):

| Platform | Incoming working folder (default) | Outgoing staging folder (default) |
| ---- | ---- | ---- |
| macOS | `~/Library/Application Support/com.vsharifov.athenaeum/sync` | `…/sync/packages` |
| Linux | `$XDG_DATA_HOME/com.vsharifov.athenaeum/sync` (`~/.local/share/…`) | `…/sync/packages` |
| Windows | `%APPDATA%\com.vsharifov.athenaeum\sync` (Roaming) | `…\sync\packages` |
| Docker / web | `/data/sync` — the parent of `ATHENAEUM_DB_PATH` | `/data/sync/packages` |

Windows note: `%APPDATA%` is the *Roaming* profile; on a domain machine the
working folder is the setting to move transient gigabytes to `%LOCALAPPDATA%`
or another drive. The default is deliberately not changed (no migration, §6.5).
Perseus keeps its own TOML `data_dir` and is not affected.

### 6.2 Path resolution

`sync_paths` becomes `SyncDirs { identity_dir, packages_dir, working_dir,
db_path }`:

- `identity_dir` = `<db dir>/sync` — `device_key` + `device_key.lock` live
  here and **never move**; `SharedIrohNode::bind(identity_dir, working_dir,
  …)` loads the key from the first and opens `blobs/` under the second.
- `packages_dir` = setting or `<identity_dir>/packages`.
- `working_dir` = setting or `identity_dir`.

Every current `sync_dir.join("packages" | "blobs" | "staging" | "incoming" |
"collab_*")` call site switches to the matching `SyncDirs` field
(`sender_packages_dir`, `ensure_iroh_node`, receiver `staging_root`,
`incoming_resolver` fallback, `cleanup_orphan_blob_stores`, the orphan sweeps,
`get_transfer_storage`, `cleanup_finished_transfers`, collab exchange). Old
outbound rows keep their absolute `package_ref`, so a changed outgoing folder
never strands an in-flight or resendable package.

### 6.3 Validation (`api::sync::validate_transfer_dir`, both backends)

In order, first failure wins, message is user-facing:

1. Absolute path (`normalize_path`); `PathPolicy::check` (web: allowed roots;
   desktop: no-op).
2. Not equal to, inside, or containing any scan root — including the
   `sync_incoming` and `calibration_library` roots — via the overlap helper
   extracted from `add_scan_root` ("the scanner would ingest the copies as
   duplicates"). A designated incoming root's volume is *recommended* for the
   working folder, but the folder itself must sit outside the root.
3. Create if missing; write-probe (create + delete `.athenaeum-write-test`).
4. The outgoing folder may be inside the working folder (the default is), the
   working folder must not be inside the outgoing folder.

### 6.4 Apply semantics

- **Outgoing staging folder**: effective immediately for the next
  preparation (the worker reads `SyncDirs` at enqueue).
- **Incoming working folder**: effective at the next transport start. The
  node holds the `working_dir` it bound; `get_transfer_paths` reports
  `restartRequired = configured != bound`, and the UI shows a "Restart
  Athenaeum to apply" badge. No live re-bind in v1 (the store is opened once
  for every role; a live swap would have to drain sender engines, receiver
  lanes and collab seeding — a separate cycle if ever needed).

On the restart that applies a new working folder: outbound in-flight packages
re-import from `package_ref` on their first serve (the `served` map is empty
after a restart — existing behavior); an inbound package that was mid-fetch
restarts from zero on the sender's next announce (its partial data is in the
old store); persisted inbound `landing_dir`s are untouched.

### 6.5 Leftovers, storage report

No migration. `get_transfer_storage` grows:

- `packages_dir`, `working_dir` (effective paths, for display),
- `leftover_bytes`: the size of `blobs/` + `staging/` + `packages/` under
  `identity_dir` when they are NOT the effective dirs (i.e. after the user
  moved away from the defaults), plus the same three under a previous custom
  working folder recorded in `sync.incoming_working_dir_previous` when the
  setting last changed.
- `cleanup_transfer_leftovers` (both backends): deletes exactly those
  leftover trees, refuses while the transport is bound to any of them, logs
  `info!(freed_bytes)`, clears `_previous` when its trees are gone.

`packages_bytes` / `blobs_bytes` / `staging_bytes` keep their meaning against
the effective dirs. After §4, `blobs_bytes` on a sender is outboards plus any
in-flight downloads.

### 6.6 Commands (Tauri + Axum mirrors, `#[tracing::instrument(skip_all, err)]`)

- `get_transfer_paths() -> TransferPaths { outgoing: PathSetting, working:
  PathSetting }` with `PathSetting { configured: Option<String>, effective:
  String, default: String, restart_required: bool }`.
- `set_transfer_paths(outgoing: Option<String>, working: Option<String>)` —
  `None` = reset to default; runs §6.3; persists; returns the new
  `TransferPaths`.
- `cleanup_transfer_leftovers() -> u64`.

Registered in `invoke_handler![]` and `build_router`; types in the
`ts_export.rs` registry.

## 7. UI

### 7.1 Transfers list

- New display state `preparing` (chip `preparing`, muted; existing label). The
  progress line reads `300 files · 4.2 GB / 15.6 GB · 180 MB/s · ETA 1m` — no
  `N of M` (no file has moved yet; the counter form returns with `queued`),
  bytes from the `preparing` ticks against `total_bytes` (§3.8). The speed/ETA
  gate (`isTransferring`) widens to the preparing family.
- `queued` + last live stage `indexing` → chip `preparing`, subline
  "indexing", bar from the `indexing` bytes. `useTransferQueue` records the
  last `stage` per outbound id alongside the live bytes.
- `announced` keeps its existing chip; the subline says "waiting for the peer
  to start pulling".
- Cancel is offered on `preparing` rows (same button as other active rows).
- Sidebar `TransfersPanel` mini-rows get the same `preparing` chip and the
  byte fraction (the panel already shows `N of M`; a preparing row shows the
  bytes instead, since no file has moved).
- Terminal `failed` rows from §3.5/§3.6 show the reason line (already
  rendered from `lastError`) and no Resend.

### 7.2 Send dialog

Unchanged layout. After the fan-out resolves (sub-second now) it notifies as
today; the title reads `Queued 300 files to Pod — preparing` when every
destination accepted, and the toast links to `/transfers`.

### 7.3 Settings → Transfers tab

New tab `transfers` (`?tab=transfers`), component
`components/settings/TransfersSection.tsx`:

1. **Folders** — two cards (Outgoing staging folder / Incoming working
   folder): effective path (monospace), "Default" hint, *Choose…* (desktop
   `pickDirectory`, web `FolderBrowserModal`), *Use default*. Validation
   errors from §6.3 inline under the card. The working-folder card shows the
   "Restart Athenaeum to apply" badge while `restartRequired`. One-line
   guidance under each: outgoing — "Prepared sends are staged here until the
   receiver confirms them"; working — "Downloads are verified here before
   landing in your Incoming folder. Same disk as Incoming = no extra copy."
2. **Bandwidth** — upload speed limit (moved from `SyncSection`, unchanged).
3. **Receiving** — max concurrent receives (moved, unchanged).
4. **Storage** — the moved storage card, now per effective folder, plus a
   "Leftovers in previous folders: N GB — Clean up" row when
   `leftover_bytes > 0`.

General keeps Account, pairing/ticket, device list, sync status. No existing
deep-link targets the moved cards (the only `?tab=` link in the app points at
`plate_solving`), so nothing else changes.

## 8. Data and compatibility

- DB: `sync_outbound.state` gains the value `preparing`. No schema change
  (the column is free text; `OutboundState::from_db` learns it). A downgraded
  build would fail `from_db("preparing")` only for a row created while
  preparing — a restart of the new build heals such rows to `failed` (§3.6).
- `sync_events` kinds added: `prepared`, `prepare_failed`. Cap and pruning
  unchanged.
- `TransferFileCounts.total_bytes` (additive; TS regenerated).
- New settings keys (§6.1) + `sync.incoming_working_dir_previous`.
- Wire, manifest, receipts: untouched. Old receivers/senders interoperate.
- Perseus: no behavior change (Copy, own dirs). Docker: defaults resolve under
  `/data/sync` as today.
- Uninstall scripts (`crates/athenaeum-tauri/scripts/uninstall-*.sh`) gain a
  comment that custom transfer folders are not removed.

## 9. Error handling

- Every failure path in §3 logs at `error!`/`warn!` with `package_id`, `path`,
  `error` before it changes state; never a silent `failed`.
- Export `NotFound`-on-external-source (§5.3) is transfer-class, never
  `LocalFault`; every other export/land error stays `LocalFault` (row
  `Failed`, "we cannot accept this").
- `set_transfer_paths` never persists a value that failed validation; the
  previous value stays effective.
- `cleanup_transfer_leftovers` refuses (Conflict) while the bound working dir
  is one of the candidates — it can only delete stores the node is not using.

## 10. Testing

Rust (`cargo test -p athenaeum-core`):

- **Store**: `preparing` round-trips `as_str`/`from_db`; `non_terminal()`
  includes it; `insert_outbound_preparing` writes row + files in one tx;
  `total_bytes` in `grouped_file_counts` (extend both existing tests).
- **Prepare worker** (loopback engine, tmp dirs): completes → row `queued`,
  manifest present, xxh3 per file equals `xxh3_full_file` of the source, the
  engine drives it to `confirmed`; cancel mid-copy → `cancelled`, dir gone,
  `sync-finished` emitted once; source deleted mid-copy → `failed` with the
  culprit in `last_error`, dir gone; `heal_interrupted_preparations` on a
  seeded `preparing` row → `failed`, dir gone; two enqueues run one after the
  other (semaphore pinned by timestamps).
- **Import**: `import_package_collection_with_mode(TryReference)` and `Copy`
  yield the same root hash; `import_subset_collection` honors the mode (the
  store's `blobs/data` dir gains no file for a > inline-threshold payload);
  re-import of a known hash from a new path serves from the new path after the
  old one is deleted (real `FsStore` in tmp).
- **Export/land**: `fetch_collection_to_dir` with TryReference leaves no owned
  data file in the store after export; `land_payload` links (same inode when
  the platform supports it) and falls back to copy when `hard_link` fails
  (inject via a non-linkable target, e.g. a cross-tmpfs path on Linux CI, or a
  unit seam); export `NotFound` classifies as non-`LocalFault`.
- **Paths**: `validate_transfer_dir` rejects inside/containing a scan root,
  the `sync_incoming` root, a non-writable dir (chmod on unix), working inside
  outgoing; accepts the defaults; `SyncDirs` resolution with and without
  settings; `get_transfer_storage.leftover_bytes` after a change.
- **Status**: `outbound_display_state` for `Preparing`/`Announced`; summary
  fallback to `total_bytes` for a manifest-less row.
- `ts_contract` regenerated; `npx tsc --noEmit` clean.

Hand smoke (into `docs/superpowers/open-items.md`): a 20 GB send from this Mac
(dialog closes < 1 s; preparing bar; cancel mid-way; APFS storage stays ≈
manifest-only after confirm) and a receive on the pod (ext4: `du` of
`blobs/` drops to KB after export, landing files share inodes with staging
until confirm, storage card matches `du`); change both folders and restart.

## 11. Key files (expected)

- `crates/athenaeum-core/src/sync/models.rs` — `OutboundState::Preparing`.
- `crates/athenaeum-core/src/sync/store.rs` — `insert_outbound_preparing`,
  `set_outbound_state`, `total_bytes` in `grouped_file_counts`.
- `crates/athenaeum-core/src/sync/status.rs` — display mapping, `total_bytes`.
- `crates/athenaeum-core/src/sync/engine.rs` — `Command::Drive`, `indexing`
  progress emit, `settle_unsettled_files` callable from the API layer.
- `crates/athenaeum-core/src/api/sync_prepare.rs` (new) — `PrepareRuntime`,
  worker, cancel, heal; `api/sync.rs` — enqueue split, `SyncDirs`,
  `validate_transfer_dir`, `get/set_transfer_paths`,
  `cleanup_transfer_leftovers`, storage report.
- `crates/athenaeum-core/src/package/writer.rs` — `write_manifest` split out
  of `write_package_with_root_hash`; reflink-or-stream copy+hash helper.
- `crates/athenaeum-core/src/sharing/iroh/node.rs` — `NodeOptions`, bind with
  `(identity_dir, working_dir)`, serve mode threading;
  `sharing/iroh/blobs.rs` — mode on `import_subset_collection`, import
  progress, `TryReference` export; `sharing/mod.rs` — `route_import_progress`.
- `crates/athenaeum-core/src/sync/ingest.rs` — `land_payload` link-first.
- `crates/athenaeum-core/src/sync/receiver.rs` / `sharing/iroh/blobs.rs` —
  export error classification (§5.3).
- `crates/perseus/src/run.rs` — pass `NodeOptions { Copy }`.
- `crates/athenaeum-tauri/src/commands/sync.rs`,
  `crates/athenaeum-web/src/routes/sync.rs` — the three new commands.
- `src/hooks/useTransferQueue.ts`, `src/components/transfers/{TransferRow,
  TransfersPanel, presentation}.tsx|ts` — `preparing`/`indexing`/`announced`.
- `src/pages/Settings.tsx`, `src/components/settings/TransfersSection.tsx`
  (new), `SyncSection.tsx` (knobs moved out).
- `docs/superpowers/open-items.md`, `RELEASE_NOTES.md` lines owed; artfrom-space
  docs: the `packages/` rationale + ext4 footprint notes already owed are
  rewritten against the new single-copy behavior.

## 12. Decisions and known limitations

- **D1 — preparation is a row state, not a dialog state.** The row exists
  before a byte is copied so a reload, the web build, and a second window all
  see it; the HTTP route no longer holds a request open for minutes.
- **D2 — one preparation at a time** (semaphore 1). Parallel preparations
  would only thrash the source disk; the queue is visible as several
  `preparing` rows whose bars advance in turn.
- **D3 — TryReference is app-only; Perseus stays Copy** (§4.3).
- **D4 — the working folder applies at restart.** Live re-bind is a separate
  cycle if it is ever asked for.
- **D5 — receiver same-hash window** (§5.3): a second in-flight package with
  a byte-identical file can park `Waiting` for up to one GC window after the
  first was confirmed and cleaned. Self-healing; logged.
- **D6 — no migration of old store/staging content**; leftovers are reported
  and cleanable.
- **D7 — interrupted preparation fails, never resumes** (§3.6). Resume would
  need the selection persisted; not worth it for a phase that is disk-bound
  and restartable from the source page.
- **D8 — landing keeps the staged inode alive until cleanup** (§5.2) so the
  store's reference stays valid while the tag lives. Cost: a copy-fallback
  receiver (cross-volume, SMB) holds two copies until confirm, as today.
