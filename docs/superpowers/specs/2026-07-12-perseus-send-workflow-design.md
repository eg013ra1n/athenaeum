# Perseus Send-Workflow (Phase 2) — Design — 2026-07-12

**Status:** design (brainstormed with owner 2026-07-12). **Repos touched:** `athenaeum` (**`perseus` crate + its web page only**; `athenaeum-core` reused unchanged). **Builds on:** the mesh model Phase 1 — capability + explicit targets (Plan 2C), the multi-target fan-out + cleanup coordinator (2C Task 7), and the dedup handshake (Plan 3, which supplies the `{new,duplicate}` batch outcome).

This is **Phase 2** (owner requirement #4 from the 2026-07-11 brainstorm). Phase 3 (app→app send UI) is a separate cycle.

---

## 1. Goal

Replace Perseus's current **per-file, immediate** auto-send with an operator-facing **send workflow**: a pending queue with per-file status, a hierarchy (tree) view of what will sync, a **manual/auto** toggle, and a **batched** history. The transport, dedup, targets, and mirror-landing are all Phase-1 mechanics reused as-is — this phase is the queue + batcher + UX layer on top.

Non-goals (v1): any `athenaeum-core` change; per-target *queues* (delivery retry stays the engine's job); app→app send UI (Phase 3); selecting a subset of the pending set to send (manual sends the whole pending set).

---

## 2. Resolved decisions (owner, 2026-07-12)

1. **Pending queue is DERIVED, not a persisted queue table.** `pending` = stable files on disk that are not yet recorded as sent in the existing `perseus_seen` store. Crash-safe by reconstruction: a restart rescans the capture dirs and the pending set reappears. No new per-file queue table.
2. **File status is aggregate + per-target drill-down.** Primary per-file status (`pending → sending → sent`/`duplicate`) is aggregate (from `perseus_seen` + the engines' outbound state). Expanding a file/batch shows **per-target** delivery (`studio-mac ✓ confirmed`, `nas-01 ↻ sending`) read from each per-target engine — cheap, the engines already track it.
3. **Manual = send the whole pending set, one button.** No subset selection in v1. The tree shows what will go; one "Send N pending" action builds a single batch of the entire pending set.
4. **Auto = quiet-period debounce only.** Flush the accumulated pending set after **N seconds with no new stable file** (config `send.auto_quiet_secs`, default e.g. 60). No periodic-interval cap, no per-file sends. A continuous imaging session accumulates and flushes at each lull (target change, dawn, clouds).
5. **A batch is ONE multi-frame package.** The batcher builds a single package containing every pending file (not one package per file), then fans it to all targets via the existing `enqueue_package_to_all`. This composes cleanly with Plan 3 dedup (offer the whole batch → want-subset per target) and the 2C-Task-7 cleanup coordinator (one package dir per batch).
6. **Mode is live-apply** (config `send.mode = auto|manual`, edited on the web page, applied via a watch channel like retention — no engine restart).

---

## 3. Architecture

A new **batcher** component in the `perseus` crate sits between the watcher and the engines, replacing the current per-file `spawn_enqueue_consumer` send path.

```
watcher (per capture_dir, stabilizes files)
   │  (capture_dir, path)  — a newly stable file
   ▼
pending accumulator  ──────────────────────────────►  "To sync" web view (tree)
   │                          (derived: on-disk stable files ∉ perseus_seen-as-sent)
   ▼   flush trigger:
   │     • auto: send.auto_quiet_secs elapsed with no new file
   │     • manual: operator clicks "Send N pending"
   ▼
batch builder  →  build ONE package from the whole pending set
   │               (multi-frame, mirror rel_path per file; reuse the core package writer)
   ▼
enqueue_package_to_all(engines, pkg_dir)   ← existing 2C-Task-7 fan-out (+ coord.register)
   │               (per engine: Plan-3 negotiate → serve want-subset → confirm)
   ▼
record_seen(each file)   +   write a perseus_batch row (package_ref, mode, created_at, file_count)
```

**Component boundaries:**
- **`perseus::batcher`** (new) — owns the pending accumulator + flush logic (auto timer / manual signal). Consumes the watcher's `(capture_dir, path)` stream; produces batch-build requests. Testable with a fake clock + a fake enqueue sink.
- **`perseus::batch_build`** — turns a pending file set into one package dir by **generalizing Perseus's own single-file `build_package_for_file` into a multi-file `build_batch_package`**: the same `athenaeum-core` `package` writer + `ManifestRecord` shape and the same `compute_rel_path` (Plan 2C, capture-dir-relative) per file — just N records/payloads in one package instead of one. (It is NOT the app's catalog-driven `build_selection_package`; Perseus works from raw on-disk capture files, not cataloged frame ids.)
- **`perseus::seen`** (existing) — the durable "already sent this exact file" record; the pending set is its complement over the on-disk stable files. Gets a read helper for "give me the sent set for these paths".
- **`perseus::web`** (existing) — new endpoints + the "To sync" tree section + batched history.

---

## 4. Pending set (derivation + status)

**Derivation.** The pending set is computed on demand (for the web view and at flush time), not stored:
`pending = { stable file f in any capture_dir : perseus_seen.should_enqueue(f.path, f.size, f.mtime) }`
— i.e. files new / changed / never-recorded per the existing stat-aware `should_enqueue`. Non-eligible files (wrong extension) are excluded by the watcher's existing filter. A file deleted before flush is simply absent from the on-disk scan → not in the batch (the batch builder also re-checks existence per file and drops a vanished one).

**Per-file status (composite, no new per-file table):**
- `pending` — in the derived set, no in-flight package.
- `sending` — the file's batch package has an active engine outbound row (`Queued`/`Announced`/`Transferring`) on any target.
- `sent` — the batch confirmed; `{new}` for that file (transferred) or `duplicate` (Plan 3 dedup skipped it — the receiver already had it). Both are success.
- `failed` — the batch reached a terminal `Failed` on a target (surfaced with the per-target reason).

The web view assembles this from three sources it already has access to: the on-disk scan vs `perseus_seen` (pending), the engines' `status_snapshot()` per target (sending / per-target delivery), and the confirmed history + `{new,duplicate}` (sent).

**Per-target drill-down.** Expanding a file or batch lists each configured target with its delivery state from that target's engine (`confirmed`/`transferring`/`failed`) and, for confirmed, whether the file was `new` or `duplicate` there.

---

## 5. Batcher & modes

- **`send.mode = auto | manual`** (config, default `auto` to match current behavior's intent). Live-editable on the web page via a watch channel (like retention) — no restart.
- **Auto:** a debounce timer resets on every new stable file; when `send.auto_quiet_secs` elapse with the pending set non-empty and no new file, the batcher flushes the whole pending set as one batch. If a flush is already in flight, new files accumulate for the next batch.
- **Manual:** the batcher never flushes on its own; the web `POST /api/send-now` flushes the current pending set as one batch. If pending is empty, it's a no-op (clear response).
- **Flush = build one package from the pending set → `enqueue_package_to_all` → per-file `record_seen` → write the `perseus_batch` row.** A file that fails to add to the package (vanished/unreadable) is dropped from the batch with a `warn!`, not fatal.
- **Empty-batch guard:** never build/enqueue an empty package.

---

## 6. Data model

**Reuse `perseus_seen`** unchanged as the sent-anchor (add only a read helper `sent_set(paths) -> set` for the pending derivation).

**New lightweight `perseus_batch` table** (Perseus-only, in `perseus.db`, WAL) — the send-batch record that groups history and distinguishes auto vs manual:
```sql
CREATE TABLE IF NOT EXISTS perseus_batch (
    package_ref TEXT PRIMARY KEY,   -- the fan-out package dir / id (the batch key)
    mode        TEXT NOT NULL,      -- 'auto' | 'manual'
    created_at  TEXT NOT NULL,
    file_count  INTEGER NOT NULL
);
```
Everything else — per-target confirmed/failed status, `{new,duplicate}` counts — is read from the engines' outbound/history at render time (a batch = one package, so its `{new,duplicate}` is the package's Plan-3 finished outcome). No duplication of engine state.

---

## 7. Web UI (extends the existing Perseus page)

**New "To sync" section:**
- A **tree** of the pending set grouped by `rel_path` components (object / date / type), counts per node.
- A **mode toggle** (Auto / Manual) → `PUT /api/send-mode`.
- In manual mode, a **"Send N pending"** button → `POST /api/send-now`.
- Per-file status badges (pending / sending / sent / duplicate / failed); expand a file → per-target rows.
- Auto mode shows the quiet-timer state ("flushing in ~Ns" / "waiting for new files").

**Batched history section** (evolves the current sent/history):
- One entry per batch (`perseus_batch` ⋈ engine package history): timestamp, mode, file_count, `{new N / duplicate M}`, aggregate confirmed/failed, target list.
- **Auto** batches are **grouped under a collapsible day header** (calendar day of `created_at`); **manual** batches shown individually.
- Expanding a batch → its files + per-target delivery.

**Endpoints (all behind the existing bearer auth, mirroring the capture-dirs/targets/retention editors):**
- `GET /api/pending` — the derived pending tree + per-file status.
- `GET /api/send-mode` / `PUT /api/send-mode` — read/set `send.mode` (+ `auto_quiet_secs`).
- `POST /api/send-now` — flush the pending set (manual); returns the batch summary or an empty-noop.
- `GET /api/batches` — batched history (grouped as above).

---

## 8. Error handling & edge cases

- **Restart mid-batch:** the fan-out engines' crash-resume (2C) re-drives non-terminal rows; `perseus_seen` was written only after enqueue, so an un-enqueued pending file reappears in the derived pending set — never silently lost (the `seen` design invariant).
- **A target offline at flush:** `enqueue_package_to_all` is best-effort per target (2C Task 7); that target's engine retries on reconnect; the per-target status shows it pending/failed. The file is still `record_seen` (Perseus did its job — enqueued to all); delivery is the engine's concern.
- **File deleted before flush:** dropped from the batch (existence re-check) and absent from future pending scans.
- **Empty pending / empty batch:** no-op, never an empty package.
- **Dedup all-duplicate batch:** Plan 3 terminalizes it as confirmed `{new:0, duplicate:n}`; the history shows "0 new, n already there".

---

## 9. Testing strategy

TDD; gates `cargo test -p perseus` + `cargo build --workspace`. Key tests (all `perseus`-side, fake clock + fake enqueue sink):
- **Pending derivation:** files ∉ seen appear; a `record_seen`'d file drops out; a deleted file is absent; a changed (size/mtime) file reappears.
- **Auto debounce:** no flush before `auto_quiet_secs`; a new file resets the timer; flush fires once on quiet with the whole pending set; a second batch accumulates during an in-flight flush.
- **Manual:** `send-now` flushes the whole pending set; empty pending → no-op.
- **Batch = one package:** N pending files → one package with N frames → one `enqueue_package_to_all` → N `record_seen` + one `perseus_batch` row.
- **Removed-before-send:** a file deleted between accumulate and build is dropped, batch still sends the rest.
- **History grouping:** auto batches group by day; manual shown individually; `{new,duplicate}` surfaced from the package finished outcome.
- **config_edit:** `apply_send_mode_edit` round-trips `mode`/`auto_quiet_secs` (toml_edit, atomic, like the existing editors).

---

## 10. Out of scope / follow-ups

- Subset selection in manual mode (send-the-whole-set only in v1).
- Per-target *queues* / per-target retry UI beyond the drill-down status (the engine owns retry).
- app→app send UI (Phase 3).
- Any `athenaeum-core` change — this phase is pure `perseus` + its web page.
