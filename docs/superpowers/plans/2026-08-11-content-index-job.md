# Content Index Job Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take `files.content_hash` computation out of the scan hot path and turn it into one explicit, visible, cancellable background job that only runs automatically when sync is configured.

**Architecture:** Scanning returns to reading `duplicates.use_content_hash` (default `false`), which restores 0.4.0 scan speed — measured ×5.9 on cold real data. The existing `duplicates::backfill` pass — already chunked, throttled, stale-row-guarded — grows progress events, a cancel flag, and `ComputeQueue` admission so it shows up in the sidebar's `ComputeQueueIndicator` like every other heavy job. A new `api::content_index` module owns the trigger policy: auto-start at boot and after each scan, but only behind the same predicate the sync receiver's autostart uses (`dev_pairing_enabled || account_signed_in`). Everything else — the old silent `backfill_content_hashes_once`, the synchronous `rescan_all_for_content_hash` command, and the Settings copy that claims the toggle governs scan hashing — is retired so exactly one mechanism remains.

**Tech Stack:** Rust (athenaeum-core / athenaeum-tauri / athenaeum-web), rusqlite, xxhash-rust XXH3, ts-rs 12 for TS bindings, React 18 + TypeScript + Tailwind design tokens.

## Background: the regression this fixes

`5aa58fed` (2026-07-22) hard-coded `use_content_hash = true` at both production scan entry points. `compute_xxhash` reads 3 × 512 KB per file (that sampling shape is unchanged since before 0.4.0 — only the *call* is new). Measured on the owner's library (18 946 files, 1.1 TB):

| | bytes/file | total | real scanner, 2010 files, cold |
| ---- | ---- | ---- | ---- |
| header only (0.4.0) | 16.4 KiB | 0.30 GiB | 0.35 s |
| header + hash (now) | 1542 KiB | 27.86 GiB | 2.07 s |

The same A/B also proves nothing *else* regressed: current code with hashing off scans 2010 files in 0.35 s.

## Global Constraints

- **Two backends in sync.** Every Tauri command added in `crates/athenaeum-tauri/src/commands/` gets its Axum mirror in `crates/athenaeum-web/src/routes/` with the same name and surface, in the same change. Real logic lives in `athenaeum-core`; the host layer is a 3–5 line wrapper.
- **No `@tauri-apps/*` imports outside `src/api/`.** Frontend always goes through the `api` object.
- **Serde boundary:** `#[serde(rename_all = "camelCase")]` on every new DTO; TS types are generated, never hand-written.
- **Never swallow errors.** Log to `tracing` before returning.
- **Instrumentation belongs on the wrappers, not the handlers.** `#[tracing::instrument(skip_all, err)]` goes on the Tauri command and the Axum route (`err(Debug)` on the web side, where the error type is `(StatusCode, String)`). Core `api::*` handlers do NOT carry it — `crates/athenaeum-core/src/api/mod.rs:2` states this, and no handler in that directory has one. Adding it there produces a duplicate nested span and double error events.
- **Log message style:** message = short stable phrase, all data in snake_case fields — `info!(pending = 12, "content index started")`, never interpolated prose.
- **Zero-print rule:** no `println!` / `eprintln!` in production code.
- **`anyhow::Result` inside core**, converted with `.map_err(|e| e.to_string())` at the command boundary.
- **Design tokens only** in React — `bg-surface`, `text-content-muted`, `text-warning`, `border-border`, … never raw hex.
- **Real gates:** `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. Clippy warnings are not a gate.
- **Formatting: never reformat pre-existing code.** Several core files predate any `rustfmt` pass — `scanner/mod.rs` alone drifts ~1021 lines, `monitor/orchestrator.rs` ~55. Running `rustfmt <file>` on one of those buries a 15-line change in a 1100-line diff and rewrites `git blame` on a file this plan barely touches. Format only the code you add, matching the surrounding style by hand. If you do run `rustfmt`, diff afterwards and revert every hunk that is not yours. New files you create may be `rustfmt`-formatted freely.
- **Notifications** are raised only via `notify()` from `useNotifications()`, on discrete outcomes, never on `*-progress` events.
- **Tauri/SSE listener pattern** in React must use the cancelled-flag form (StrictMode-safe) documented in CLAUDE.md.

---

## File Structure

**Modified — core**
- `crates/athenaeum-core/src/scanner/mod.rs` — `run_registered_scan` reads the setting again; new `scan_hashing_enabled` helper + tests.
- `crates/athenaeum-core/src/api/scan_roots.rs` — `start_scan` reads the setting again; `rescan_all_for_content_hash` deleted.
- `crates/athenaeum-core/src/duplicates/backfill.rs` — the pass gains progress + cancel; `backfill_content_hashes_once` deleted.
- `crates/athenaeum-core/src/services/compute_queue.rs` — `ComputeJobKind::ContentIndex`.
- `crates/athenaeum-core/src/api/sync.rs` — expose `pub fn sync_configured`.
- `crates/athenaeum-core/src/api/mod.rs` — declare + re-export the new module.
- `crates/athenaeum-core/src/ts_export.rs` — register the two new DTOs.

**Created — core**
- `crates/athenaeum-core/src/api/content_index.rs` — trigger policy, status, single-flight, thread spawn.

**Modified — hosts**
- `crates/athenaeum-tauri/src/commands/mod.rs`, `crates/athenaeum-tauri/src/lib.rs`, `crates/athenaeum-tauri/src/commands/core.rs`, `crates/athenaeum-tauri/src/commands/scan_roots.rs`
- `crates/athenaeum-web/src/routes/mod.rs`, `crates/athenaeum-web/src/main.rs`, `crates/athenaeum-web/src/routes/scan_roots.rs`

**Created — hosts**
- `crates/athenaeum-tauri/src/commands/content_index.rs`
- `crates/athenaeum-web/src/routes/content_index.rs`

**Modified — frontend**
- `src/types/models.ts` (generated), `src/pages/Settings.tsx`, `src/components/Layout.tsx`

**Created — frontend**
- `src/hooks/useContentIndex.ts`

---

### Task 1: Scan stops hashing (the regression fix)

This task alone restores scan speed. It is independently shippable.

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:2488-2500`
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:933-945`
- Test: `crates/athenaeum-core/src/scanner/mod.rs` (inline `#[cfg(test)] mod tests`, which already has `use super::*; use crate::db::schema::init_db; use crate::events::NullEmitter; use tempfile::TempDir;`)

**Interfaces:**
- Produces: `pub(crate) fn scan_hashing_enabled(settings: &crate::settings::SettingsManager, conn: &rusqlite::Connection) -> bool` — used by both scan entry points. No other task consumes it.

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `crates/athenaeum-core/src/scanner/mod.rs`:

```rust
    /// Scan-time content hashing is OPT-IN. `5aa58fed` hard-coded it on at both
    /// entry points, which added a 3 x 512 KB read per file — measured x5.9 on a
    /// cold 2010-file real library. The hash column is now populated by the
    /// content-index job instead (see `api::content_index`).
    #[test]
    fn scan_hashing_defaults_off_and_follows_the_setting() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let settings = crate::settings::SettingsManager::new();

        assert!(
            !scan_hashing_enabled(&settings, &conn),
            "no setting row: scan must not hash"
        );

        crate::db::set_setting(&conn, "duplicates.use_content_hash", "true").unwrap();
        assert!(
            scan_hashing_enabled(&settings, &conn),
            "setting on: scan hashes"
        );

        crate::db::set_setting(&conn, "duplicates.use_content_hash", "false").unwrap();
        assert!(
            !scan_hashing_enabled(&settings, &conn),
            "setting off: scan must not hash"
        );
    }

    /// The behavioural half: with hashing off the scanner writes NULL, so the
    /// content-index job has rows to find.
    #[test]
    fn scan_leaves_content_hash_null_when_hashing_is_off() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("M33/L_001.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()],
        )
        .unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let result = scan_directory_parallel(
            scan.path(),
            1,
            &conn,
            &NullEmitter,
            false, // hashing off
            cancel,
            false,
        );
        assert_eq!(result.files_processed, 1);

        let hashed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hashed, 0, "hashing off must leave content_hash NULL");
    }
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p athenaeum-core scan_hashing_defaults_off_and_follows_the_setting scan_leaves_content_hash_null_when_hashing_is_off`
Expected: FAIL — `cannot find function 'scan_hashing_enabled' in this scope`.

- [ ] **Step 3: Add the helper and use it at both entry points**

In `crates/athenaeum-core/src/scanner/mod.rs`, above `run_registered_scan`:

```rust
/// Scan-time content hashing is opt-in via `duplicates.use_content_hash`
/// (default false).
///
/// `5aa58fed` briefly hard-coded this on so the transfer dedup handshake would
/// see the whole scanned library. That put a 3 x 512 KB sampling read on every
/// new/changed file — 27.86 GiB instead of 0.30 GiB on an 18 946-file library,
/// measured x5.9 cold. The dedup index is now filled by the explicit,
/// cancellable content-index job (`api::content_index`) instead, so the scan
/// hot path stays header-only.
pub(crate) fn scan_hashing_enabled(
    settings: &crate::settings::SettingsManager,
    conn: &rusqlite::Connection,
) -> bool {
    // A read failure must not pass silently just because the fallback happens
    // to equal the default: the visible symptom would be a user turning the
    // setting ON and scans quietly continuing not to hash.
    settings
        .get_duplicates_use_content_hash(conn)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "duplicates.use_content_hash read failed; scan hashing disabled");
            false
        })
}
```

Replace the hard-coded `true` in `run_registered_scan` (`crates/athenaeum-core/src/scanner/mod.rs:2488-2500`):

```rust
    let use_content_hash = scan_hashing_enabled(&ctx.settings, &conn);
    let result = scan_directory_parallel(
        Path::new(&root.path),
        root_id,
        &conn,
        emitter,
        use_content_hash,
        cancel_flag,
        root.unique_camera,
    );
```

Replace the same hard-coded `true` in `crates/athenaeum-core/src/api/scan_roots.rs:933-945`:

```rust
    // Scan-time hashing is opt-in (see `scanner::scan_hashing_enabled`); the
    // dedup index is filled by `api::content_index`, not by the scan.
    let use_content_hash = crate::scanner::scan_hashing_enabled(&ctx.settings, &conn);
    let mut result = crate::scanner::scan_directory(
        Path::new(&root.path),
        &conn,
        None,
        use_content_hash,
        root.unique_camera,
        root_id,
    );
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p athenaeum-core scan_hashing`
Expected: PASS (both tests).

- [ ] **Step 5: Run the full core suite for regressions**

Run: `cargo test -p athenaeum-core`
Expected: PASS. If a sync/dedup test asserted that a scan populates `content_hash`, it was pinning the regression — update it to drive `duplicates::backfill` explicitly and note why in the test's doc comment.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/scanner/mod.rs crates/athenaeum-core/src/api/scan_roots.rs
git add crates/athenaeum-core/src/scanner/mod.rs crates/athenaeum-core/src/api/scan_roots.rs
git commit -m "perf(scanner): scan-time content hashing is opt-in again

5aa58fed hard-coded use_content_hash = true at both scan entry points so
transfer dedup would cover the whole library. That put a 3x512 KB sampling
read on every new/changed file: 27.86 GiB instead of 0.30 GiB on an
18 946-file catalog, measured x5.9 cold on the real scanner. The dedup index
moves to an explicit background job; the scan hot path is header-only again."
```

---

### Task 2: The hashing pass becomes a cancellable job with progress

**Files:**
- Modify: `crates/athenaeum-core/src/duplicates/backfill.rs`
- Test: same file's `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub struct ContentIndexProgress { pub done: usize, pub total: usize, pub updated: usize, pub skipped: usize }` — serde camelCase + `ts_rs::TS`, emitted as `content-index-progress`.
  - `pub struct ContentIndexFinished { pub updated: usize, pub skipped: usize, pub cancelled: bool, pub failed: bool }` — serde camelCase + `ts_rs::TS`, emitted as `content-index-finished`.
  - `pub struct BackfillSummary { pub pending: usize, pub updated: usize, pub skipped: usize, pub cancelled: bool }` (existing struct, one field added).
  - `pub fn count_pending(db: &Database) -> usize`
  - `pub fn run_content_index(db: &Database, emitter: &dyn ProgressEmitter, cancel: Arc<AtomicBool>) -> BackfillSummary`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/athenaeum-core/src/duplicates/backfill.rs`. Use the `CapturingEmitter` shape already used elsewhere in the codebase (`archive/executor.rs:581`):

```rust
    struct CapturingEmitter(std::sync::Mutex<Vec<(String, serde_json::Value)>>);

    impl crate::events::ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    /// The pass reports progress and a terminal event, so the sidebar card and
    /// the completion notification have something to render.
    #[test]
    fn content_index_emits_progress_and_finish() {
        let (db, _tmp) = test_db_with_pending_rows(3);
        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));

        let summary = run_content_index(&db, &emitter, Arc::new(AtomicBool::new(false)));

        assert_eq!(summary.updated, 3);
        assert!(!summary.cancelled);

        let events = emitter.0.lock().unwrap();
        assert!(
            events.iter().any(|(name, _)| name == "content-index-progress"),
            "expected at least one progress event"
        );
        let (_, finished) = events
            .iter()
            .find(|(name, _)| name == "content-index-finished")
            .expect("expected a terminal event");
        assert_eq!(finished["updated"], 3);
        assert_eq!(finished["cancelled"], false);
    }

    /// A pre-set cancel flag stops the pass before it hashes anything, and the
    /// terminal event says so — the sidebar's X must not look like a no-op.
    #[test]
    fn content_index_honours_cancel_flag() {
        let (db, _tmp) = test_db_with_pending_rows(3);
        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));

        let summary = run_content_index(&db, &emitter, Arc::new(AtomicBool::new(true)));

        assert!(summary.cancelled, "pre-set flag must report cancelled");
        assert_eq!(summary.updated, 0, "cancelled before any chunk ran");

        let events = emitter.0.lock().unwrap();
        let (_, finished) = events
            .iter()
            .find(|(name, _)| name == "content-index-finished")
            .expect("a cancelled run still emits a terminal event");
        assert_eq!(finished["cancelled"], true);
    }

    /// Status needs a cheap count that does not walk the disk.
    #[test]
    fn count_pending_counts_null_hash_rows_only() {
        let (db, _tmp) = test_db_with_pending_rows(3);
        assert_eq!(count_pending(&db), 3);
        run_content_index(&db, &crate::events::NullEmitter, Arc::new(AtomicBool::new(false)));
        assert_eq!(count_pending(&db), 0);
    }
```

Add the fixture helper in the same `mod tests`. It writes real files so the stale-row guard (which compares on-disk size/mtime against the row) passes:

```rust
    /// N real files on disk with matching `files` rows and NULL content_hash.
    /// Real bytes because the pass's stale-row guard compares the row's
    /// (size, modified_at) against the file's — a fake row would be skipped.
    fn test_db_with_pending_rows(n: usize) -> (Database, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("t.db")).unwrap();
        let conn = db.conn();
        crate::db::schema::init_db(&conn).unwrap();
        for i in 0..n {
            let p = tmp.path().join(format!("f{i}.fits"));
            crate::archive::restore::tests::write_minimal_fits(&p);
            let meta = std::fs::metadata(&p).unwrap();
            let modified: chrono::DateTime<Utc> = meta.modified().unwrap().into();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'FITS', ?5)",
                rusqlite::params![
                    p.to_str().unwrap(),
                    format!("f{i}.fits"),
                    meta.len() as i64,
                    modified.to_rfc3339(),
                    Utc::now().to_rfc3339(),
                ],
            )
            .unwrap();
        }
        drop(conn);
        (db, tmp)
    }
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p athenaeum-core --lib duplicates::backfill`
Expected: FAIL — `cannot find function 'run_content_index'` / `'count_pending'`.

- [ ] **Step 3: Implement**

In `crates/athenaeum-core/src/duplicates/backfill.rs`:

Add to the imports at the top of the file:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::events::{emit_event, ProgressEmitter};
```

Add the two payloads next to `BackfillSummary`, and the `cancelled` field:

```rust
/// Per-chunk progress for the content-index job. UI data, not a log line — the
/// pass also logs its own `debug!` per chunk (ProgressEmitter events and
/// tracing stay separate concerns).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexProgress {
    pub done: usize,
    pub total: usize,
    pub updated: usize,
    pub skipped: usize,
}

/// Terminal event. Emitted on EVERY exit path — normal completion, cancel, the
/// nothing-to-do early return, and the row-listing failure — so the sidebar card
/// and the notification handler have exactly one place to close on. A pass that
/// never emits this strands the card open.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexFinished {
    pub updated: usize,
    pub skipped: usize,
    pub cancelled: bool,
    /// The pass could not enumerate its work (locked DB, schema mismatch,
    /// corrupt catalog). Without this the failure exit is indistinguishable
    /// from a clean nothing-to-do run, and the UI would cheerfully report
    /// "finished — 0 indexed" over a broken catalog.
    pub failed: bool,
}
```

Add `pub cancelled: bool` to `BackfillSummary` (it already derives `Default`, so no other construction site changes).

Add the cheap count:

```rust
/// Rows still missing a hash. Pure SQL — never touches the disk, so the status
/// command is safe to call from the UI on every Settings mount.
pub fn count_pending(db: &Database) -> usize {
    let conn = db.conn();
    conn.query_row(
        "SELECT COUNT(*) FROM files WHERE content_hash IS NULL",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "content index: failed to count pending rows");
        0
    })
}
```

Rename `backfill_content_hashes` to `run_content_index` and give it the two new parameters. Keep the whole existing body — the pending snapshot, the stale-row guard, the per-chunk connection checkout, the throttle nap — and add exactly four things:

1. after the `pending_count == 0` early return, emit the terminal event before returning;
2. a cancel check at the top of the chunk loop;
3. a progress emit at the end of each chunk;
4. a terminal emit before the final return.

```rust
pub fn run_content_index(
    db: &Database,
    emitter: &dyn ProgressEmitter,
    cancel: Arc<AtomicBool>,
) -> BackfillSummary {
    // ... unchanged: snapshot `pending` (connection dropped before hashing) ...

    let pending_count = pending.len();
    if pending_count == 0 {
        tracing::info!(pending = 0, "content index: nothing to do");
        emit_event(
            emitter,
            "content-index-finished",
            &ContentIndexFinished { updated: 0, skipped: 0, cancelled: false, failed: false },
        );
        return BackfillSummary::default();
    }
    tracing::info!(pending = pending_count, "content index started");

    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut cancelled = false;
    let chunk_total = pending_count.div_ceil(CHUNK);
    for (chunk_idx, chunk) in pending.chunks(CHUNK).enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            tracing::info!(updated, skipped, "content index cancelled");
            break;
        }

        // ... unchanged: hash the chunk, then check a connection out for the
        // chunk's UPDATEs ...

        tracing::debug!(chunk = chunk_idx + 1, of = chunk_total, updated, skipped, "content index chunk done");
        emit_event(
            emitter,
            "content-index-progress",
            &ContentIndexProgress {
                done: ((chunk_idx + 1) * CHUNK).min(pending_count),
                total: pending_count,
                updated,
                skipped,
            },
        );

        // ... unchanged: throttle nap, skipped after the last chunk ...
    }

    emit_event(
        emitter,
        "content-index-finished",
        &ContentIndexFinished { updated, skipped, cancelled, failed: false },
    );
    tracing::info!(pending = pending_count, updated, skipped, cancelled, "content index finished");
    BackfillSummary { pending: pending_count, updated, skipped, cancelled }
}
```

Update `backfill_content_hashes_once` so the crate still compiles — it keeps its single-flight guard and passes inert arguments. Task 5 deletes it:

```rust
    let _ = run_content_index(db, &crate::events::NullEmitter, Arc::new(AtomicBool::new(false)));
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p athenaeum-core --lib duplicates::backfill`
Expected: PASS (3 new tests plus the module's existing ones).

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/duplicates/backfill.rs
git add crates/athenaeum-core/src/duplicates/backfill.rs
git commit -m "feat(content-index): the hashing pass emits progress and honours cancel

Same chunking, throttle and stale-row guard as before; it now reports
content-index-progress per chunk, always emits content-index-finished, and
breaks out of the chunk loop when its cancel flag is set."
```

---

### Task 3: `ComputeJobKind::ContentIndex`

**Files:**
- Modify: `crates/athenaeum-core/src/services/compute_queue.rs:44-49`
- Modify: `crates/athenaeum-core/src/ts_export.rs:151-153`
- Test: `crates/athenaeum-core/src/services/compute_queue.rs` (inline tests) and `crates/athenaeum-core/tests/ts_contract.rs` (existing, run only)

**Interfaces:**
- Consumes: nothing.
- Produces: `ComputeJobKind::ContentIndex` — serialises to `"content_index"`; consumed by Task 4's `acquire` call and by the frontend's generated `ComputeJobKind` union.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/athenaeum-core/src/services/compute_queue.rs`:

```rust
    /// The sidebar indicator keys off the serialised kind; pin the wire string
    /// so a rename can't silently orphan the card.
    #[test]
    fn content_index_kind_serialises_snake_case() {
        let json = serde_json::to_string(&ComputeJobKind::ContentIndex).unwrap();
        assert_eq!(json, "\"content_index\"");
    }
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo test -p athenaeum-core --lib compute_queue::tests::content_index_kind_serialises_snake_case`
Expected: FAIL — `no variant named 'ContentIndex'`.

- [ ] **Step 3: Add the variant**

In `crates/athenaeum-core/src/services/compute_queue.rs`:

```rust
pub enum ComputeJobKind {
    Analysis,
    MasterBuild,
    LightCalibration,
    /// Whole-library `files.content_hash` pass that feeds transfer dedup.
    /// IO-bound rather than CPU-bound, but it rides the same admission queue so
    /// it can't fight a master build for the disk, and so the sidebar card and
    /// its cancel button come for free.
    ContentIndex,
}
```

- [ ] **Step 4: Register the new DTOs and regenerate the TS contract**

In `crates/athenaeum-core/src/ts_export.rs`, add next to the existing `ComputeQueueEntry` line (same output file, keep registry order stable):

```rust
            crate::duplicates::backfill::ContentIndexProgress,
            crate::duplicates::backfill::ContentIndexFinished,
```

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`
Then run it again without the env var to confirm the checked-in bindings match:
Run: `cargo test -p athenaeum-core --test ts_contract`
Expected: PASS, and `src/types/models.ts` now contains `ComputeJobKind = "analysis" | "master_build" | "light_calibration" | "content_index"` plus the two new interfaces.

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p athenaeum-core --lib compute_queue`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/services/compute_queue.rs crates/athenaeum-core/src/ts_export.rs
git add crates/athenaeum-core/src/services/compute_queue.rs crates/athenaeum-core/src/ts_export.rs src/types/models.ts
git commit -m "feat(compute-queue): ContentIndex job kind + generated TS bindings"
```

---

### Task 4: Trigger policy — `api::content_index`

**Files:**
- Create: `crates/athenaeum-core/src/api/content_index.rs`
- Modify: `crates/athenaeum-core/src/api/mod.rs`
- Modify: `crates/athenaeum-core/src/api/sync.rs` (add `pub fn sync_configured` beside `autostart_gate`)
- Modify: `crates/athenaeum-core/src/ts_export.rs`
- Test: `crates/athenaeum-core/src/api/content_index.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `duplicates::backfill::{run_content_index, count_pending}` (Task 2); `ComputeJobKind::ContentIndex` (Task 3).
- Produces:
  - `pub fn sync_configured(ctx: &ServiceContext) -> Result<bool, ApiError>` (in `api::sync`)
  - `pub struct ContentIndexStatus { pub pending: i64, pub total: i64, pub running: bool, pub sync_configured: bool }` — serde camelCase + `ts_rs::TS`
  - `pub fn get_content_index_status(ctx: &ServiceContext) -> Result<ContentIndexStatus, ApiError>`
  - `pub fn start_content_index(db: Database, queue: ComputeQueue, emitter: Arc<dyn ProgressEmitter>) -> bool` — returns `false` if a pass is already running for this DB
  - `pub fn autostart_content_index(ctx: &ServiceContext, emitter: Arc<dyn ProgressEmitter>)` — the gated entry point both hosts call at boot and after every scan
  - a per-process record of what the last pass could not hash, keyed by DB path, so the re-arm converges on a catalog holding permanently-unhashable rows (offline drive, drifted mtime). `start_content_index`'s worker records `summary.skipped` into it; `autostart_content_index` reads it. Not persisted — a reconnected drive earns one fresh attempt per launch.

- [ ] **Step 1: Expose the sync gate**

In `crates/athenaeum-core/src/api/sync.rs`, directly below `autostart_gate` (line 1182):

```rust
/// "Is device-to-device sync configured on this node" — the SAME predicate the
/// receiver's autostart uses, deliberately shared rather than re-derived.
///
/// `files.content_hash` exists for exactly one consumer: the transfer dedup
/// handshake. A node that never transfers must not pay 3 x 512 KB of disk reads
/// per catalogued file for an index nobody reads, so the content-index job is
/// gated on this. Local state only — no hub call, no keychain.
pub fn sync_configured(ctx: &ServiceContext) -> Result<bool, ApiError> {
    Ok(autostart_gate(
        dev_pairing_enabled(ctx)?,
        account_signed_in(ctx)?,
    ))
}
```

- [ ] **Step 2: Write the failing tests**

Create `crates/athenaeum-core/src/api/content_index.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Status is honest about the gate: a node with no sync configured reports
    /// `syncConfigured: false`, which is what the Settings card renders its
    /// "not running automatically" explanation from.
    #[test]
    fn status_reports_pending_and_gate() {
        let (ctx, _tmp) = test_ctx_with_files(2);
        let status = get_content_index_status(&ctx).unwrap();
        assert_eq!(status.total, 2);
        assert_eq!(status.pending, 2);
        assert!(!status.running);
        assert!(!status.sync_configured, "no ACCOUNT_DEVICE_ID => gate closed");
    }

    /// Signing in opens the gate — same predicate as the receiver autostart.
    #[test]
    fn status_gate_opens_when_signed_in() {
        let (ctx, _tmp) = test_ctx_with_files(1);
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_DEVICE_ID, "device-1")
                .unwrap();
        }
        assert!(get_content_index_status(&ctx).unwrap().sync_configured);
    }

    /// Single-flight: a second start while one is running is a no-op, so a
    /// boot autostart racing a post-scan re-arm can't double the disk load.
    #[test]
    fn start_is_single_flight_per_database() {
        let (ctx, _tmp) = test_ctx_with_files(0);
        let db = ctx.db.get().unwrap().clone();
        mark_running_for_test(&db);
        assert!(
            !start_content_index(db.clone(), ctx.compute_queue.clone(), Arc::new(NullEmitter)),
            "a second start while running must be refused"
        );
        clear_running_for_test(&db);
        assert!(
            start_content_index(db.clone(), ctx.compute_queue.clone(), Arc::new(NullEmitter)),
            "once the guard clears, a start is accepted again"
        );
    }

    /// The gate is enforced at the autostart entry point, not inside the job:
    /// a manual start from Settings still works on an ungated node.
    #[test]
    fn autostart_is_a_noop_when_sync_is_not_configured() {
        let (ctx, _tmp) = test_ctx_with_files(2);
        autostart_content_index(&ctx, Arc::new(NullEmitter));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            get_content_index_status(&ctx).unwrap().pending,
            2,
            "gate closed: nothing should have been hashed"
        );
    }
}
```

Add the fixture helper in the same `mod tests`. This codebase has no shared `ServiceContext` builder — tests construct it field-by-field (`api/masters.rs:2950-2976` is the reference). Real files on disk, because the pass's stale-row guard compares the row's `(size, modified_at)` against the file's:

```rust
    use std::collections::HashMap;
    use std::sync::RwLock;

    use crate::cache::MemoryImageCache;
    use crate::events::NullEmitter;
    use crate::services::operation_queue::OperationQueue;
    use crate::settings::SettingsManager;

    /// A ServiceContext over a temp DB with N real files catalogued and unhashed.
    fn test_ctx_with_files(n: usize) -> (ServiceContext, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let database = Database::new(tmp.path().join("t.db")).unwrap();
        {
            let conn = database.conn();
            crate::db::schema::init_db(&conn).unwrap();
            for i in 0..n {
                let p = tmp.path().join(format!("f{i}.fits"));
                crate::archive::restore::tests::write_minimal_fits(&p);
                let meta = std::fs::metadata(&p).unwrap();
                let modified: chrono::DateTime<chrono::Utc> = meta.modified().unwrap().into();
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'FITS', ?5)",
                    rusqlite::params![
                        p.to_str().unwrap(),
                        format!("f{i}.fits"),
                        meta.len() as i64,
                        modified.to_rfc3339(),
                        chrono::Utc::now().to_rfc3339(),
                    ],
                )
                .unwrap();
            }
        }

        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: Arc::new(tokio::sync::Mutex::new(None)),
        };
        (ctx, tmp)
    }
```

If `ServiceContext` has gained a field since `api/masters.rs:2952` was written, the compiler names it — add it with the same inert value that file uses.

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p athenaeum-core --lib api::content_index`
Expected: FAIL — the module has no `get_content_index_status` / `start_content_index` / `autostart_content_index`.

- [ ] **Step 4: Implement the module**

Prepend to `crates/athenaeum-core/src/api/content_index.rs`:

```rust
//! Trigger policy for the whole-library content-hash index.
//!
//! `files.content_hash` has exactly one consumer — the device-to-device
//! transfer dedup handshake. So the index is not part of scanning (which used
//! to hash unconditionally, at 3 x 512 KB of disk reads per file) and it does
//! not run at all on a node that has never configured sync. When it does run it
//! is a first-class visible job: it takes a `ComputeQueue` ticket, so it shows
//! up in the sidebar with a cancel button and can't fight a master build for
//! the disk.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::events::ProgressEmitter;
use crate::services::compute_queue::{ComputeJobKind, ComputeQueue};
use crate::services::ServiceContext;

use super::{db, ApiError};

/// DB paths with a pass in flight. Keyed by path, not a process-global bool, so
/// a dev DB-path swap can still start its own pass (same reasoning as the guard
/// this replaces in `duplicates::backfill`).
static RUNNING_FOR: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

fn running_set() -> &'static Mutex<HashSet<PathBuf>> {
    RUNNING_FOR.get_or_init(|| Mutex::new(HashSet::new()))
}

/// What the Settings card renders.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexStatus {
    /// Catalogued files still missing a hash.
    pub pending: i64,
    /// Catalogued files in total.
    pub total: i64,
    /// A pass is in flight right now.
    pub running: bool,
    /// Whether the job runs automatically on this node.
    pub sync_configured: bool,
}

// No `#[tracing::instrument]` here — see the Global Constraints. The boundary
// span lives on Task 5's Tauri command and Axum route.
pub fn get_content_index_status(ctx: &ServiceContext) -> Result<ContentIndexStatus, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    drop(conn);
    let pending = crate::duplicates::backfill::count_pending(&database) as i64;
    let running = running_set()
        .lock()
        .expect("content index running-set poisoned")
        .contains(database.path());
    Ok(ContentIndexStatus {
        pending,
        total,
        running,
        sync_configured: crate::api::sync::sync_configured(ctx)?,
    })
}

/// Start a pass on a background thread. Returns `false` when one is already in
/// flight for this database — the boot autostart and the post-scan re-arm can
/// both fire in the same second, and doubling the disk load would be exactly
/// the behaviour this whole change exists to remove.
///
/// NOT gated: a manual "Index now" from Settings must work on a node that has
/// no sync configured. The gate lives in [`autostart_content_index`].
pub fn start_content_index(
    database: Database,
    queue: ComputeQueue,
    emitter: Arc<dyn ProgressEmitter>,
) -> bool {
    {
        let mut set = running_set().lock().expect("content index running-set poisoned");
        if !set.insert(database.path().to_path_buf()) {
            tracing::debug!(path = %database.path().display(), "content index already running; ignoring start");
            return false;
        }
    }

    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let label = "Content index".to_string();
        // Admission first: an IO-heavy whole-library pass must not run beside a
        // master build. A queued-then-cancelled ticket never runs the pass.
        let permit = match queue.acquire(ComputeJobKind::ContentIndex, &label, cancel.clone()) {
            Ok((permit, job_id)) => {
                tracing::info!(job_id, "content index admitted");
                permit
            }
            Err(_) => {
                tracing::info!("content index cancelled while queued");
                running_set()
                    .lock()
                    .expect("content index running-set poisoned")
                    .remove(database.path());
                return;
            }
        };

        crate::duplicates::backfill::run_content_index(&database, emitter.as_ref(), cancel);

        drop(permit);
        running_set()
            .lock()
            .expect("content index running-set poisoned")
            .remove(database.path());
    });

    true
}

/// The gated entry point. Both hosts call this at boot and after every scan.
/// A node with no sync configured pays nothing.
pub fn autostart_content_index(ctx: &ServiceContext, emitter: Arc<dyn ProgressEmitter>) {
    let configured = match crate::api::sync::sync_configured(ctx) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "content index autostart: gate check failed");
            return;
        }
    };
    if !configured {
        tracing::debug!("content index autostart skipped: sync not configured");
        return;
    }
    let Some(database) = ctx.db.get().cloned() else {
        tracing::debug!("content index autostart skipped: database not initialised");
        return;
    };
    // Re-arm on WORK A PASS CAN ACTUALLY DO, not on NULL-hash rows.
    //
    // `count_pending` counts every `files` row with a NULL hash, but the pass
    // permanently skips rows whose file is missing on disk or whose
    // (size, modified_at) drifted — it leaves those NULL by design, because
    // "missing != orphan" is a project rule and a disconnected drive must never
    // cost catalog rows. Gating on `pending == 0` therefore never converges on
    // any catalog with an offline drive: the job would re-fire at every boot AND
    // after every scan, for the life of the install, each time re-enumerating
    // and `stat`ing every pending row to hash nothing.
    //
    // So compare against what the previous pass could not hash, and let the
    // pass's own skip logic be the single source of truth for "unhashable" —
    // minting a second, drift-prone definition here is how the two would
    // silently diverge. The record is PER PROCESS, deliberately not persisted:
    // a reconnected drive gets one fresh attempt at the next launch, while
    // within a session the degenerate catalog settles after exactly one pass.
    let pending = crate::duplicates::backfill::count_pending(&database);
    if pending == 0 {
        tracing::debug!("content index autostart skipped: nothing pending");
        return;
    }
    if let Some(unhashable) = last_unhashable(&database) {
        if pending <= unhashable {
            tracing::debug!(
                pending,
                unhashable,
                "content index autostart skipped: no newly hashable rows"
            );
            return;
        }
    }
    if start_content_index(database, ctx.compute_queue.clone(), emitter) {
        tracing::info!(pending, "content index autostart");
    }
}

/// Test-only hooks for the single-flight guard.
#[cfg(test)]
pub(crate) fn mark_running_for_test(database: &Database) {
    running_set().lock().unwrap().insert(database.path().to_path_buf());
}

#[cfg(test)]
pub(crate) fn clear_running_for_test(database: &Database) {
    running_set().lock().unwrap().remove(database.path());
}
```

Declare the module in `crates/athenaeum-core/src/api/mod.rs` beside the other `pub mod` lines, and re-export the three public functions the same way neighbouring modules do.

Register the status DTO in `crates/athenaeum-core/src/ts_export.rs` beside the two payloads from Task 3:

```rust
            crate::api::content_index::ContentIndexStatus,
```

Regenerate: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p athenaeum-core --lib api::content_index`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/api/content_index.rs crates/athenaeum-core/src/api/mod.rs crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/ts_export.rs
git add crates/athenaeum-core/src/api/content_index.rs crates/athenaeum-core/src/api/mod.rs crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/ts_export.rs src/types/models.ts
git commit -m "feat(content-index): gated trigger policy, status and single-flight start

Autostart reuses api::sync::sync_configured — the same predicate the receiver
autostart uses — so a node that never transfers never pays for the index.
Manual start stays ungated so Settings' button works anywhere."
```

---

### Task 5: Host wiring — commands, routes, boot, post-scan re-arm

**Files:**
- Create: `crates/athenaeum-tauri/src/commands/content_index.rs`
- Create: `crates/athenaeum-web/src/routes/content_index.rs`
- Modify: `crates/athenaeum-tauri/src/commands/mod.rs`, `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-tauri/src/commands/core.rs:157-168` (replace the backfill spawn)
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs` (post-scan re-arm)
- Modify: `crates/athenaeum-web/src/routes/mod.rs`, `crates/athenaeum-web/src/main.rs:265-278`, `crates/athenaeum-web/src/routes/scan_roots.rs`
- Modify: `crates/athenaeum-core/src/duplicates/backfill.rs` (delete `backfill_content_hashes_once` + its guard)

**Interfaces:**
- Consumes: `api::content_index::{get_content_index_status, start_content_index, autostart_content_index}` (Task 4).
- Produces: commands `get_content_index_status` (no args → `ContentIndexStatus`) and `start_content_index` (no args → `bool`, `false` = already running), reachable from the frontend as `api.invoke('get_content_index_status')` / `api.invoke('start_content_index')`.

- [ ] **Step 1: Add the Tauri commands**

Create `crates/athenaeum-tauri/src/commands/content_index.rs`:

```rust
use athenaeum_core::api;
use athenaeum_core::api::content_index::ContentIndexStatus;
use std::sync::Arc;
use tauri::{AppHandle, State};

use crate::AppState;

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_content_index_status(
    state: State<'_, AppState>,
) -> Result<ContentIndexStatus, String> {
    api::content_index::get_content_index_status(&state.ctx).map_err(|e| e.to_string())
}

/// Manual "Index now". Returns false when a pass is already in flight.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_content_index(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<bool, String> {
    let db = state
        .ctx
        .db
        .get()
        .cloned()
        .ok_or_else(|| "database not initialized".to_string())?;
    let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
        Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle));
    Ok(api::content_index::start_content_index(
        db,
        state.ctx.compute_queue.clone(),
        emitter,
    ))
}
```

Declare `pub mod content_index;` in `crates/athenaeum-tauri/src/commands/mod.rs` and re-export both functions the way the neighbouring modules do. Register both in `invoke_handler![]` in `crates/athenaeum-tauri/src/lib.rs`, next to `commands::get_compute_queue` (line 394).

- [ ] **Step 2: Add the mirrored Axum routes**

Create `crates/athenaeum-web/src/routes/content_index.rs` with the same two handlers, using `SseProgressEmitter::new(state.event_tx.clone())` for the emitter and `#[tracing::instrument(skip_all, err(Debug))]`. Register both in `build_router` in `crates/athenaeum-web/src/routes/mod.rs`:

```rust
        .route("/api/get_content_index_status", post(content_index::get_content_index_status))
        .route("/api/start_content_index", post(content_index::start_content_index))
```

- [ ] **Step 3: Replace the silent boot backfill with the gated autostart**

In `crates/athenaeum-tauri/src/commands/core.rs`, replace the `backfill_content_hashes_once` block (lines 157-168) with:

```rust
    // Content index (transfer dedup's `files.content_hash`). Gated on sync being
    // configured and admitted through the compute queue, so it is visible in the
    // sidebar and cancellable — the predecessor ran silently on every launch and
    // read ~1.5 MB per catalogued file with nothing in the UI to explain it.
    {
        let ctx = Arc::clone(&state.ctx);
        let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
            Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle.clone()));
        std::thread::spawn(move || {
            athenaeum_core::api::content_index::autostart_content_index(&ctx, emitter);
        });
    }
```

Make the equivalent replacement in `crates/athenaeum-web/src/main.rs` (lines 265-278), using the SSE emitter.

- [ ] **Step 4: Re-arm after every scan**

A scan is the only thing that mints new NULL-hash rows, so it is the only re-arm trigger needed. The re-arm lives at the host boundary because `api::scan_roots::start_scan_with_progress` takes a *borrowed* emitter (`&E`) and the job needs an owned `Arc<dyn ProgressEmitter>`.

**Two scan paths, two triggers.** The command boundary only covers scans the user clicked. The background monitor calls `scanner::run_registered_scan` directly, so an unattended tick would mint NULL-hash rows the re-arm never sees — on a monitored library that means the index silently lags until the next launch. Cover it through the seam that already exists for exactly this: `MonitorService::set_scan_completion_hook`, documented as "host-installed hook invoked after a registered scan … with newly-ingested files", dormant since Sync 2C because no host installs one. Each host installs a hook closing over its own `Arc<ServiceContext>` and emitter. The contract fits without strain — the hook is fire-and-forget on the monitor's blocking thread and must not block, and `autostart_content_index` spawns and returns — and it fires only when the scan actually ingested new files, which is a sharper trigger than re-arming on every scan. Keeping it a host-installed hook is also what keeps the layering right: core never depends on host types, and `monitor` never reaches up into `api`.

In `crates/athenaeum-tauri/src/commands/scan_roots.rs`, after the `start_scan_with_progress` call returns `Ok`, before returning the DTO:

```rust
    // New rows may need hashing. Gated + single-flight inside, so this is a
    // no-op on an ungated node or while a pass is already running.
    {
        let ctx = Arc::clone(&state.ctx);
        let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
            Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle.clone()));
        std::thread::spawn(move || {
            athenaeum_core::api::content_index::autostart_content_index(&ctx, emitter);
        });
    }
```

Mirror it in `crates/athenaeum-web/src/routes/scan_roots.rs` with the SSE emitter.

- [ ] **Step 5: Delete the superseded silent path**

In `crates/athenaeum-core/src/duplicates/backfill.rs`, delete `backfill_content_hashes_once`, the `BACKFILL_RAN_FOR` static, `running_set`-adjacent imports it alone used, and any test that only exercised the removed guard. The single-flight responsibility now belongs to `api::content_index::start_content_index`.

- [ ] **Step 6: Gate the workspace**

Run: `cargo build --workspace`
Expected: PASS with no unresolved references to `backfill_content_hashes_once`.

Run: `cargo test -p athenaeum-core`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
rustfmt crates/athenaeum-tauri/src/commands/content_index.rs crates/athenaeum-web/src/routes/content_index.rs crates/athenaeum-core/src/duplicates/backfill.rs
git add -A
git commit -m "feat(content-index): wire both backends, boot autostart and post-scan re-arm

Replaces the silent one-shot backfill: the pass is now gated on sync being
configured, admitted through the compute queue, and re-armed after each scan.
Manual start/status commands added on both transports."
```

---

### Task 6: Frontend — status card, manual start, completion notification

**Files:**
- Create: `src/hooks/useContentIndex.ts`
- Modify: `src/pages/Settings.tsx:1190-1240`
- Modify: `src/components/Layout.tsx`

**Interfaces:**
- Consumes: `api.invoke('get_content_index_status')` → `ContentIndexStatus`; `api.invoke('start_content_index')` → `boolean`; events `content-index-progress` (`ContentIndexProgress`) and `content-index-finished` (`ContentIndexFinished`), all generated into `src/types/models.ts` by Task 3/4.
- Produces: `useContentIndex()` returning `{ status, refresh, start, starting }`, and `useContentIndexNotifications()` mounted once at app root.

The sidebar card needs no work: `ComputeQueueIndicator` renders every kind except `analysis`, so `content_index` appears there with its cancel button for free.

- [ ] **Step 1: Write the hook**

Create `src/hooks/useContentIndex.ts`. The listener uses the cancelled-flag form required by CLAUDE.md:

```ts
import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type { ContentIndexStatus, ContentIndexFinished } from '../types/models';

/** Status + manual start for the content index (Settings card). */
export function useContentIndex() {
  const [status, setStatus] = useState<ContentIndexStatus | null>(null);
  const [starting, setStarting] = useState(false);

  const refresh = useCallback(() => {
    api.invoke<ContentIndexStatus>('get_content_index_status')
      .then(setStatus)
      .catch((err) => console.error('[useContentIndex] status failed:', err));
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<ContentIndexFinished>('content-index-finished', () => {
      if (cancelled) return;
      refresh();
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useContentIndex] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [refresh]);

  const start = useCallback(async () => {
    setStarting(true);
    try {
      await api.invoke<boolean>('start_content_index');
      refresh();
    } catch (err) {
      console.error('[useContentIndex] start failed:', err);
    } finally {
      setStarting(false);
    }
  }, [refresh]);

  return { status, refresh, start, starting };
}

/**
 * App-root listener that turns the terminal event into one notification.
 * Mounted once in Layout so it fires whatever page the user is on. Progress is
 * deliberately NOT notified — it is high-frequency UI data, and the sidebar
 * compute-queue card already shows the job running.
 */
export function useContentIndexNotifications() {
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<ContentIndexFinished>('content-index-finished', (payload) => {
      if (cancelled) return;
      // A failed pass must never read as a finished one — it is the only
      // terminal state the user cannot infer from the counts.
      if (payload.failed) {
        notify({
          title: 'Content index failed',
          detail: 'Could not read the catalog. See the log for details.',
          kind: 'sync',
          tone: 'warning',
          hasErrors: true,
          dedupeKey: 'content-index-failed',
        });
        return;
      }
      if (payload.updated === 0 && !payload.cancelled) return; // nothing-to-do pass: stay quiet
      notify({
        title: payload.cancelled
          ? `Content index cancelled — ${payload.updated} indexed`
          : `Content index finished — ${payload.updated} files indexed`,
        detail: payload.skipped > 0 ? `${payload.skipped} skipped` : undefined,
        kind: 'sync',
        tone: payload.cancelled ? 'warning' : 'success',
        dedupeKey: `content-index-${payload.updated}-${payload.skipped}-${payload.cancelled}`,
      });
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useContentIndexNotifications] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [notify]);
}
```

The `sync` kind already exists in the `NotificationKind` union and the icon map — no new kind needed, and it is the honest label since the index exists only for transfer dedup.

- [ ] **Step 2: Mount the notification hook at app root**

In `src/components/Layout.tsx`, call `useContentIndexNotifications();` in the component body, next to where `<NotificationPanel />` is rendered.

- [ ] **Step 3: Replace the Settings block**

In `src/pages/Settings.tsx`, replace lines 1190-1240 (the `Duplicate Detection` heading's toggle description and the whole `{useContentHash && !contentHashRescanned && ( ... )}` rescan-warning block) with an honest toggle description plus a separate Content Index card:

```tsx
        <div>
          <h3 className="text-lg font-semibold mb-4">Duplicate Detection</h3>

          <div className="space-y-4">
            <div>
              <label className="flex items-center gap-3 cursor-pointer">
                <input
                  type="checkbox"
                  checked={useContentHash}
                  onChange={(e) => setUseContentHash(e.target.checked)}
                  className="w-5 h-5 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0"
                />
                <div>
                  <span className="block text-sm font-medium text-content-secondary">
                    Group duplicates by file content (XXHash)
                  </span>
                  <span className="block text-xs text-content-muted mt-1">
                    Groups the Duplicates view by a content hash instead of by size, date and
                    filename. More accurate, and it needs the content index below.
                  </span>
                </div>
              </label>
            </div>
          </div>
        </div>

        <div>
          <h3 className="text-lg font-semibold mb-4">Content Index</h3>
          <div className="p-4 bg-surface rounded-lg border border-border space-y-3">
            <p className="text-xs text-content-muted">
              A content hash of every catalogued file, used to skip files a device already has
              when transferring, and to group duplicates by content. Building it reads about
              1.5 MB per file, so it runs in the background — never during a scan — and you can
              stop it any time from the job card in the sidebar.
            </p>

            {contentIndex.status && (
              <p className="text-sm text-content-secondary">
                {contentIndex.status.pending === 0
                  ? `All ${contentIndex.status.total} files indexed.`
                  : `${contentIndex.status.pending} of ${contentIndex.status.total} files not indexed yet.`}
              </p>
            )}

            {contentIndex.status && !contentIndex.status.syncConfigured && (
              <p className="text-xs text-content-muted">
                Sync is not set up on this device, so the index is not built automatically.
                You can still build it now if you want content-based duplicate grouping.
              </p>
            )}

            <button
              onClick={contentIndex.start}
              disabled={
                contentIndex.starting ||
                contentIndex.status?.running ||
                contentIndex.status?.pending === 0
              }
              className="flex items-center gap-2 px-4 py-2 bg-accent hover:brightness-110 disabled:bg-surface-hover disabled:text-content-muted disabled:cursor-not-allowed text-white rounded-lg transition-colors"
            >
              <RefreshCw size={18} className={contentIndex.status?.running ? 'animate-spin' : ''} />
              {contentIndex.status?.running ? 'Indexing…' : 'Build index now'}
            </button>
          </div>
        </div>
```

Add `const contentIndex = useContentIndex();` to the component body, import the hook, and delete the now-unused `handleRescanContentHash`, `rescanningContentHash`, `rescanSuccess`, `contentHashRescanned` state and their setters (including the `setContentHashRescanned` reset at line 379).

- [ ] **Step 4: Gate the frontend**

Run: `npx tsc --noEmit`
Expected: PASS with no unused-variable or missing-import errors.

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useContentIndex.ts src/pages/Settings.tsx src/components/Layout.tsx
git commit -m "feat(content-index): Settings status card, manual start, completion notification

The Duplicate Detection copy told users to rescan every file after enabling
the content-hash toggle. That flow is gone: the index is built by an explicit
background job with progress and a cancel button, startable from Settings and
armed automatically only where sync is configured. The toggle keeps its two
real effects — Duplicates-view grouping, and opt-in scan-time hashing — and
now says so.

The sidebar compute-queue card renders the running job for free."
```

---

### Task 7: Retire `rescan_all_for_content_hash`

The last of the three overlapping mechanisms. Its replacement (Task 6's "Build index now") is background, cancellable and reports progress; this one blocks its thread with no progress and no cancel.

**Files:**
- Modify: `crates/athenaeum-core/src/api/scan_roots.rs:983` (delete the function and its `RescanResultDto` if unused elsewhere)
- Modify: `crates/athenaeum-tauri/src/commands/scan_roots.rs:157-159`, `crates/athenaeum-tauri/src/lib.rs:275`
- Modify: `crates/athenaeum-web/src/routes/scan_roots.rs:338-345`, `crates/athenaeum-web/src/routes/mod.rs:69`
- Modify: `crates/athenaeum-core/src/ts_export.rs` (drop `RescanResultDto` if it was registered)

- [ ] **Step 1: Confirm nothing else references it**

Run: `grep -rn "rescan_all_for_content_hash\|RescanResultDto" crates/ src/`
Expected: only the sites listed above — Task 6 already removed the single frontend caller in `Settings.tsx`.

- [ ] **Step 2: Delete every site**

Remove the core function, both host wrappers, both registrations, and `RescanResultDto` if the grep showed it has no other consumer.

- [ ] **Step 3: Regenerate bindings if the DTO was registered**

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`
Then: `cargo test -p athenaeum-core --test ts_contract`
Expected: PASS.

- [ ] **Step 4: Gate everything**

Run: `cargo build --workspace`
Run: `cargo test -p athenaeum-core`
Run: `npx tsc --noEmit`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(content-index): retire rescan_all_for_content_hash

Superseded by the background content-index job, which has progress, a cancel
button and a compute-queue ticket. Three overlapping mechanisms for one
column become one."
```

---

## Owner smoke list (after implementation, on real data)

1. Scan the 18 946-file root with sync signed out. Expect seconds, not ~40 s; `SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL` stays 0; no job card appears.
2. Sign in, relaunch. A "Content index" card appears in the sidebar, progresses, and finishes with one notification. `pending` reaches 0.
3. Press the card's X mid-pass. It stops promptly, the notification says cancelled, and Settings shows a non-zero `pending`.
4. Scan again after adding a few new files while signed in. The index re-arms and picks up only the new rows.
5. Settings → "Build index now" while signed out. It runs (the manual path is deliberately ungated) and the card appears.
6. Start a master build and press "Build index now". They queue rather than run together (`compute.max_concurrent` default 1).

## Deliberately out of scope

- **Making the index incremental per scan.** The pass already visits only NULL-hash rows, which is incremental enough; a per-scan file list would add a second code path for no measurable gain.
- **A download-side or per-file throttle setting.** The existing 64-file chunk plus 50 ms nap is the throttle; adding a knob before anyone has asked is speculative.
- **Changing the sampling shape of `compute_xxhash`.** It is a wire contract — the transfer dedup handshake on the other device compares against the same three windows, and `sync/ingest_tests.rs` pins the window maths.
