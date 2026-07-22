# Perseus UI v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild Perseus's local web page as a Transfers-style two-tab UI (unified batch list grouped across targets + Settings), add obligation-gated source-file deletion and batch-history deletion, and show the sender kind (Athenaeum/Perseus) on received transfers in the app.

**Architecture:** Perseus keeps its vanilla-JS embedded page (split into `index.html` + `app.js` + `style.css`, Nord palette) served by the existing axum router; new read/write endpoints compose the existing `StandaloneSyncStore` / `SeenStore` / `BatchStore`. Source deletion reuses the retention deleter (extended from one-source-per-package to all sources). The app side stamps the announcing peer's `DeviceCapability` onto `sync_inbound` at announce time and surfaces it as `peerKind` on `InboundSummary` plus a device-capabilities command.

**Tech Stack:** Rust (axum, rusqlite, tokio), vanilla JS/CSS (no build step), ts-rs for the TS contract, React/TS only for the small app-side badge.

**Spec:** `docs/superpowers/specs/2026-07-23-perseus-ui-v2-design.md`. Read it before starting any task.

## Global Constraints

- Branch: `0.5.0`. Commit as `eg013ra1n` — NEVER add a Claude co-author footer.
- Nothing in `crates/athenaeum-core/src/sharing/` changes — `Msg` postcard indices are FROZEN. This cycle needs no wire change at all.
- `cargo build -p perseus` must keep working with `--no-default-features` and WITHOUT npm — the web page stays `include_str!`-embedded static files.
- Logging: `tracing` only, message = short stable phrase, data in snake_case fields. Zero `println!` in production code. Every new axum handler logs its error before returning it.
- Serde boundary: `#[serde(rename_all = "camelCase")]` on every DTO both sides.
- App-side command changes ship Tauri command + axum route mirror in the same task.
- TS contract: after changing any ts-rs-exported type, regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and commit the `src/types/*.ts` diff.
- Perseus DB stays backward-compatible: additive `CREATE TABLE IF NOT EXISTS` / guarded `ALTER TABLE ADD COLUMN` only.
- Subagents: `rust-engineer` for Rust tasks (1–5, 8–10), `frontend-dev` for UI tasks (6, 7, 11).
- Gates every task: `cargo build --workspace`, the task's tests, plus `cargo build -p perseus --no-default-features` for perseus/core tasks and `npx tsc --noEmit` for frontend/TS-contract tasks.

## Key existing facts (read before coding)

- `crates/perseus/src/web.rs` (~4270 lines): `WebState` (store/seen/batches/config/engines/cleanup/peer_device/batcher…), `build_router(state, token)` with bearer `auth_layer` on `/api/*` (`/` exempt), handler tests use `test_state().await` + `tower::ServiceExt::oneshot`. `STATUS_SCAN_LIMIT: u32 = 5000`.
- One batch = one `perseus_batch` row keyed `package_ref` (payload dir path; its basename is the wire `batch_uuid`). A fan-out writes ONE `sync_outbound` row per target sharing that `package_ref`. `aggregate_outcome(rows: &[&OutboundRow]) -> String` already exists in web.rs.
- `OutboundState`: `Queued|Announced|Transferring|Delivered|Confirmed|Failed|Cancelled`; terminal = Confirmed/Failed/Cancelled. `Confirmed` guarantees every frame receipt is non-rejected. Receiver decline lands as `Cancelled` + `last_error` starting with `CANCELLED_BY_RECEIVER_DETAIL` (`"cancelled by receiver"`, exported from `athenaeum_core::sync`).
- Per-file rows: `sync_outbound_files` (`OutboundFileRow { outbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at }`, state `pending|sending|uploaded|done`, outcome = receipt text `ingested|duplicate|cancelled|rejected:…`). Read via `SyncStore::list_outbound_files(outbound_id)` (trait import needed in web.rs: `athenaeum_core::sync::SyncStore`).
- `sync_events` journal: free fns in `sync/store.rs` — `list_sync_events(conn, Direction, batch_key)` / `delete_sync_events(…)`; sender-side `batch_key` = outbound row id as string. Also free fns `delete_outbound_row(conn, id)`, `delete_outbound_files(conn, id)`.
- `SeenStore` (`crates/perseus/src/seen.rs`): `perseus_seen` rows per FILE with `package_ref`, `deleted_at`; `source_for_package` returns ONE row (`query_row`) — the pre-batcher shape. `mark_deleted(path)`, `relink_package(old, new)`.
- `BatchStore` (`crates/perseus/src/batch_store.rs`): `perseus_batch (package_ref PK, mode, created_at, file_count)` + `perseus_batch_files (package_ref, rel_path, source_path)`; methods `record`, `record_files`, `files_for`, `clone_files`, `list`.
- Deleter (`crates/perseus/src/run.rs`): `retention_delete_source(store: &dyn SyncStore, seen, pkg_ref, outcome_tag, peer_device) -> Result<DeleteOutcome>` — audit-before-delete (`build_retention_history_rows` + `append_history` BEFORE `remove_file`), TOCTOU stat guard (`source_stat_unchanged`), honest `Removed`/`SkippedNoop`. `delete_confirmed_packages(store, seen, ids, peer_device)` is the current `POST /api/delete` body (single-row-confirmed gate — replaced in Task 5, deleted in Task 8).
- App receive side: `handle_announce` (`sync/receiver.rs:897`) validates the peer via `PeerAuthorizer`, keys the row with `upsert_inbound_attempt(conn, peer_hex, batch_uuid, wire_package_id, frame_count, byte_size) -> (i64, bool)`. Device-name cache idiom: `ingest::cached_device_name(conn, peer_hex)` reads settings key `SYNC_DEVICE_NAMES` (JSON hex→name), written by `api::sync::refresh_authorized_peers` via `account_device_names(&devices)`. The single inbound row mapper is `to_inbound(raw: InboundRaw)` (`sync/store.rs:1261`).
- `InboundSummary` lives in `sync/status.rs:146`, built ONLY by `api/sync.rs::inbound_summary(row, device_names, file_counts)`; ts-rs-registered in `ts_export.rs:155`.
- `get_sync_device_names`: core `api/sync.rs:1797`, tauri wrapper `commands/sync.rs:244`, axum `routes/sync.rs:337` + `routes/mod.rs:267`.
- Frontend: `src/pages/Transfers.tsx` (row model `UnifiedRow` in `src/components/transfers/types.ts`), rows render in `TransferRow.tsx`, detail tabs in `TransferDetail.tsx`/`TransferDetails.tsx`; history hook `useTransferHistory.ts` already fetches `get_sync_device_names`.

---

### Task 1: Multi-source seen linkage + whole-batch deleter

The deleter still assumes one source file per package (beta.1 shape). A batcher package carries MANY files — today a confirmed multi-file batch loses only ONE file per retention tick / manual delete call. Fix the primitive first; everything in Task 5 builds on it.

**Files:**
- Modify: `crates/perseus/src/seen.rs` (add `sources_for_package`)
- Modify: `crates/perseus/src/run.rs` (loop the deleter; add `SourceDeleteDetail` + `delete_package_sources`)
- Test: same files' `#[cfg(test)]` modules

**Interfaces:**
- Consumes: existing `SourceLink { path, size, mtime_ms }`, `retention_delete_source` internals (`source_stat_unchanged`, `build_retention_history_rows`).
- Produces: `SeenStore::sources_for_package(&self, package_ref: &str) -> Result<Vec<SourceLink>>`; `pub struct SourceDeleteDetail { pub removed: Vec<String>, pub skipped: Vec<(String, String)>, pub failed: Vec<(String, String)> }`; `pub fn delete_package_sources(store: &dyn SyncStore, seen: &SeenStore, pkg_ref: &Path, outcome: &str, peer_device: &str) -> Result<SourceDeleteDetail>`. Task 5 calls `delete_package_sources` with tag `"deleted_manual"`; retention keeps calling `retention_delete_source` (now a thin wrapper).

- [ ] **Step 1: Failing test — plural linkage**

In `seen.rs` tests, next to `source_for_package_resolves_the_enqueued_file`:

```rust
#[test]
fn sources_for_package_returns_every_live_file() {
    let (_tmp, store) = store();
    store.mark_enqueued(&PathBuf::from("/cap/a.fits"), 10, 1, "/pkg/uuid-1").unwrap();
    store.mark_enqueued(&PathBuf::from("/cap/b.fits"), 20, 2, "/pkg/uuid-1").unwrap();
    store.mark_enqueued(&PathBuf::from("/cap/c.fits"), 30, 3, "/pkg/uuid-2").unwrap();
    store.mark_deleted(&PathBuf::from("/cap/b.fits")).unwrap();
    let live = store.sources_for_package("/pkg/uuid-1").unwrap();
    assert_eq!(live.len(), 1, "deleted linkage must be excluded");
    assert_eq!(live[0].path, PathBuf::from("/cap/a.fits"));
    // Undelete-free check on the multi-row case:
    store.mark_enqueued(&PathBuf::from("/cap/d.fits"), 40, 4, "/pkg/uuid-1").unwrap();
    let live = store.sources_for_package("/pkg/uuid-1").unwrap();
    assert_eq!(live.len(), 2, "every live row of the package is returned");
}
```

- [ ] **Step 2: Run it — FAIL** (`cargo test -p perseus sources_for_package_returns_every_live_file` → "no method named `sources_for_package`")

- [ ] **Step 3: Implement `sources_for_package`** (below `source_for_package`; keep the singular fn — `resend.rs` still uses it):

```rust
/// Every *live* (`deleted_at IS NULL`) source linkage of `package_ref`, ordered
/// by path. The batcher records one row per packaged file, so a batch package
/// resolves to MANY links — [`source_for_package`] (singular, `query_row`) sees
/// only the first and exists for the legacy one-file-per-package callers.
pub fn sources_for_package(&self, package_ref: &str) -> Result<Vec<SourceLink>> {
    let conn = self.conn.lock().expect("seen store mutex poisoned");
    let mut stmt = conn
        .prepare(
            "SELECT path, size, mtime FROM perseus_seen
             WHERE package_ref = ?1 AND deleted_at IS NULL ORDER BY path",
        )
        .context("prepare sources_for_package")?;
    let rows = stmt
        .query_map(params![package_ref], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })
        .context("query sources_for_package")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("collect sources_for_package")?;
    Ok(rows
        .into_iter()
        .map(|(path, size, mtime_ms)| SourceLink {
            path: PathBuf::from(path),
            size: size.max(0) as u64,
            mtime_ms,
        })
        .collect())
}
```

- [ ] **Step 4: Run it — PASS**

- [ ] **Step 5: Failing test — deleter removes every file of a batch**

In `run.rs` tests (find the existing retention deleter tests for the fixture idiom — a temp dir with real files, a `StandaloneSyncStore`, a `SeenStore`):

```rust
#[test]
fn delete_package_sources_removes_every_confirmed_file_and_audits_each() {
    let tmp = tempfile::tempdir().unwrap();
    let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();
    let seen = SeenStore::open(tmp.path().join("perseus.db")).unwrap();
    let a = tmp.path().join("a.fits");
    let b = tmp.path().join("b.fits");
    std::fs::write(&a, b"aaaa").unwrap();
    std::fs::write(&b, b"bbbbbb").unwrap();
    let stat = |p: &std::path::Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.len(), crate::seen::mtime_millis(m.modified().ok()))
    };
    let (asz, amt) = stat(&a);
    let (bsz, bmt) = stat(&b);
    seen.mark_enqueued(&a, asz, amt, "/pkg/uuid-1").unwrap();
    seen.mark_enqueued(&b, bsz, bmt, "/pkg/uuid-1").unwrap();

    let detail = delete_package_sources(&store, &seen, std::path::Path::new("/pkg/uuid-1"), "deleted_manual", "peerhex").unwrap();
    assert_eq!(detail.removed.len(), 2, "both files removed: {detail:?}");
    assert!(detail.failed.is_empty() && detail.skipped.is_empty());
    assert!(!a.exists() && !b.exists());
    // Audit rows persisted for BOTH files:
    let hist = store.search_history(HistoryQuery::default()).unwrap();
    assert_eq!(hist.iter().filter(|h| h.outcome == "deleted_manual").count(), 2);
    // Linkage stamped dead — a second pass is an honest no-op:
    let again = delete_package_sources(&store, &seen, std::path::Path::new("/pkg/uuid-1"), "deleted_manual", "peerhex").unwrap();
    assert!(again.removed.is_empty());
}

#[test]
fn delete_package_sources_toctou_skips_changed_file_but_removes_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();
    let seen = SeenStore::open(tmp.path().join("perseus.db")).unwrap();
    let a = tmp.path().join("a.fits");
    let b = tmp.path().join("b.fits");
    std::fs::write(&a, b"aaaa").unwrap();
    std::fs::write(&b, b"bbbbbb").unwrap();
    let stat = |p: &std::path::Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.len(), crate::seen::mtime_millis(m.modified().ok()))
    };
    let (asz, amt) = stat(&a);
    let (bsz, bmt) = stat(&b);
    seen.mark_enqueued(&a, asz, amt, "/pkg/uuid-1").unwrap();
    seen.mark_enqueued(&b, bsz, bmt, "/pkg/uuid-1").unwrap();
    // TOCTOU: `b` was rewritten (new, unconfirmed content) since enqueue.
    std::fs::write(&b, b"different-and-longer").unwrap();

    let detail = delete_package_sources(&store, &seen, std::path::Path::new("/pkg/uuid-1"), "deleted_manual", "peerhex").unwrap();
    assert_eq!(detail.removed, vec![a.display().to_string()]);
    assert_eq!(detail.skipped.len(), 1, "changed file skipped, not failed: {detail:?}");
    assert!(detail.skipped[0].0.contains("b.fits"));
    assert!(!a.exists(), "unchanged batch-mate still deleted");
    assert!(b.exists(), "changed file preserved");
}
```

(`HistoryQuery::default()` in the first test — check its real construction in `store.rs::search_history_rows` tests and mirror it.)

- [ ] **Step 6: Run — FAIL** (`delete_package_sources` not found)

- [ ] **Step 7: Implement.** Extract the current single-source body of `retention_delete_source` (everything from the `!source.exists()` guard through `seen.mark_deleted`) into a private `fn delete_one_source(store: &dyn SyncStore, seen: &SeenStore, pkg_ref: &Path, link: &crate::seen::SourceLink, outcome: &str, peer_device: &str) -> Result<DeleteOutcome>` (unchanged logic, takes the link instead of looking it up). Then:

```rust
/// Per-file outcome of one whole-package source deletion — the web layer's
/// honest per-path report (spec §4.1). `skipped`/`failed` carry `(path, reason)`.
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDeleteDetail {
    pub removed: Vec<String>,
    pub skipped: Vec<(String, String)>,
    pub failed: Vec<(String, String)>,
}

/// Delete EVERY live source file of `pkg_ref` through the shared safety
/// contract (audit-before-delete, TOCTOU guard, honest no-ops), capturing a
/// per-file outcome instead of stopping at the first error — one bad file must
/// not strand its batch-mates' deletion.
pub fn delete_package_sources(
    store: &dyn SyncStore,
    seen: &SeenStore,
    pkg_ref: &Path,
    outcome: &str,
    peer_device: &str,
) -> Result<SourceDeleteDetail> {
    let mut detail = SourceDeleteDetail::default();
    let links = seen.sources_for_package(&pkg_ref.to_string_lossy())?;
    for link in &links {
        let path = link.path.display().to_string();
        match delete_one_source(store, seen, pkg_ref, link, outcome, peer_device) {
            Ok(DeleteOutcome::Removed) => detail.removed.push(path),
            Ok(DeleteOutcome::SkippedNoop) => detail
                .skipped
                .push((path, "skipped: source missing or changed since confirmation".into())),
            Err(error) => {
                tracing::error!(path = %link.path.display(), %error, "package source delete failed");
                detail.failed.push((path, format!("{error:#}")));
            }
        }
    }
    Ok(detail)
}
```

Rewrite `retention_delete_source` as the wrapper (public signature unchanged so `run_retention_once`'s closure compiles untouched):

```rust
fn retention_delete_source(
    store: &dyn SyncStore,
    seen: &SeenStore,
    pkg_ref: &Path,
    outcome: &str,
    peer_device: &str,
) -> Result<DeleteOutcome> {
    let detail = delete_package_sources(store, seen, pkg_ref, outcome, peer_device)?;
    if !detail.removed.is_empty() {
        return Ok(DeleteOutcome::Removed);
    }
    if let Some((path, err)) = detail.failed.first() {
        anyhow::bail!("delete {path}: {err}");
    }
    Ok(DeleteOutcome::SkippedNoop)
}
```

Keep `delete_one_source`'s TOCTOU/missing branches returning `SkippedNoop` exactly as today (the split changes structure, not behavior). If the existing per-source body distinguishes "missing" vs "changed" reasons in its logs, thread that reason string out (change `delete_one_source` to return `Result<(DeleteOutcome, &'static str)>` or an enum) so `skipped` reasons stay honest — implementer's choice, but the reason text must distinguish the two cases.

- [ ] **Step 8: Run all of it** — `cargo test -p perseus` PASS (existing retention tests must stay green: the single-file case now goes through the loop with one link).

- [ ] **Step 9: Commit** — `feat(perseus): whole-batch source deletion — seen linkage goes plural, deleter loops every live source`

---

### Task 2: StandaloneSyncStore group helpers (core)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/store.rs` (inside `impl StandaloneSyncStore`, next to `get_outbound`/`all_outbound`)
- Test: `store.rs` `#[cfg(test)]` module

**Interfaces:**
- Consumes: free fns `list_sync_events`, `delete_sync_events`, `delete_outbound_files`, `delete_outbound_row`, `Direction::Sent`; the impl's existing `self.conn.lock()` idiom (mirror `get_outbound`'s exact lock expression).
- Produces: `pub fn list_sync_events_for(&self, outbound_id: i64) -> Result<Vec<SyncEventRow>>`; `pub fn delete_outbound_group(&self, ids: &[i64]) -> Result<()>` (one transaction; rows + per-file rows + journals). Tasks 4/5 call these from perseus web.rs.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn delete_outbound_group_removes_rows_files_and_journals_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let store = StandaloneSyncStore::open(tmp.path().join("s.db")).unwrap();
    // Two rows of one fan-out batch (same package_ref, two peers) — seed via the
    // store's own trait methods (enqueue + replace_outbound_files + append_sync_event),
    // mirroring how existing store tests build rows.
    // ... seed id1, id2 with 2 files each and 1 event each ...
    store.delete_outbound_group(&[id1, id2]).unwrap();
    assert!(store.get_outbound(id1).unwrap().is_none());
    assert!(store.get_outbound(id2).unwrap().is_none());
    assert!(store.list_outbound_files(id1).unwrap().is_empty());
    assert!(store.list_sync_events_for(id1).unwrap().is_empty());
}

#[test]
fn list_sync_events_for_reads_the_sent_journal_of_one_row() {
    // seed one row + append_sync_event(Direction::Sent, "<id>", …) twice;
    // assert two rows back, in insertion order, kinds preserved.
}
```

Seed idiom: this file's existing tests around `reset_outbound_for_resend` / `replace_outbound_files` show the exact enqueue call — copy it.

- [ ] **Step 2: Run — FAIL** (methods missing)

- [ ] **Step 3: Implement**

```rust
/// The sender-side `sync_events` journal of ONE outbound row (the Perseus web
/// Log tab's read). `batch_key` is the row id as text — the engine's own
/// journaling convention.
pub fn list_sync_events_for(&self, outbound_id: i64) -> Result<Vec<SyncEventRow>> {
    let conn = self.conn.lock().expect("sync store mutex poisoned");
    list_sync_events(&conn, Direction::Sent, &outbound_id.to_string())
}

/// History-delete a whole batch group's sender bookkeeping — each row's
/// per-file rows and journal, then the row — in ONE transaction. Never touches
/// disk payloads or Perseus's `perseus_seen` linkage (dedup identity survives).
pub fn delete_outbound_group(&self, ids: &[i64]) -> Result<()> {
    let conn = self.conn.lock().expect("sync store mutex poisoned");
    let tx = conn.unchecked_transaction().context("begin delete_outbound_group")?;
    for &id in ids {
        delete_outbound_files(&tx, id)?;
        delete_sync_events(&tx, Direction::Sent, &id.to_string())?;
        delete_outbound_row(&tx, id)?;
    }
    tx.commit().context("commit delete_outbound_group")
}
```

(If the impl's real lock expression differs, mirror it. `&tx` derefs to `&Connection`.)

- [ ] **Step 4: Run — PASS** (`cargo test -p athenaeum-core --lib sync::`)
- [ ] **Step 5: Commit** — `feat(sync): StandaloneSyncStore group helpers — per-row journal read + atomic outbound-group history delete`

---

### Task 3: BatchStore v2 — `files_deleted_at` + delete + divert-participation lookup

**Files:**
- Modify: `crates/perseus/src/batch_store.rs`
- Test: its `#[cfg(test)]` module

**Interfaces:**
- Consumes: existing DDL + `record`/`list`.
- Produces: `BatchRow.files_deleted_at: Option<String>` (and `list()` returns it); `pub fn mark_files_deleted(&self, package_ref: &str, at: &str) -> Result<()>`; `pub fn delete(&self, package_ref: &str) -> Result<()>` (batch row + its `perseus_batch_files`, one tx); `pub fn packages_for_sources(&self, sources: &[String]) -> Result<Vec<String>>` (DISTINCT `package_ref`s whose `perseus_batch_files.source_path` matches any of `sources`). Tasks 4/5 consume all four.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn files_deleted_at_roundtrips_and_defaults_null() {
    let (_tmp, store) = store();
    store.record("/pkg/u1", "auto", "2026-07-23T10:00:00Z", 3).unwrap();
    assert_eq!(store.list().unwrap()[0].files_deleted_at, None);
    store.mark_files_deleted("/pkg/u1", "2026-07-23T11:00:00Z").unwrap();
    assert_eq!(store.list().unwrap()[0].files_deleted_at.as_deref(), Some("2026-07-23T11:00:00Z"));
}

#[test]
fn delete_removes_batch_row_and_linkage() {
    let (_tmp, store) = store();
    store.record("/pkg/u1", "auto", "2026-07-23T10:00:00Z", 1).unwrap();
    store.record_files("/pkg/u1", &[("a.fits".into(), PathBuf::from("/cap/a.fits"))]).unwrap();
    store.delete("/pkg/u1").unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(store.files_for("/pkg/u1").unwrap().is_empty());
}

#[test]
fn packages_for_sources_finds_every_participation() {
    let (_tmp, store) = store();
    store.record_files("/pkg/u1", &[("a.fits".into(), PathBuf::from("/cap/a.fits"))]).unwrap();
    store.record_files("/pkg/u2", &[("a.fits".into(), PathBuf::from("/cap/a.fits")),
                                     ("b.fits".into(), PathBuf::from("/cap/b.fits"))]).unwrap();
    store.record_files("/pkg/u3", &[("c.fits".into(), PathBuf::from("/cap/c.fits"))]).unwrap();
    let mut refs = store.packages_for_sources(&["/cap/a.fits".to_string()]).unwrap();
    refs.sort();
    assert_eq!(refs, vec!["/pkg/u1".to_string(), "/pkg/u2".to_string()]);
}
```

(Mirror the module's existing `store()` fixture helper name.)

- [ ] **Step 2: Run — FAIL**

- [ ] **Step 3: Implement.** Guarded column add in `open` (after both `conn.execute(DDL…)` calls):

```rust
// files_deleted_at (UI v2 §4.1): guarded ALTER — CREATE IF NOT EXISTS never
// adds a column to an existing table. Additive; pre-upgrade rows read NULL.
let has_col: bool = conn
    .prepare("PRAGMA table_info(perseus_batch)")
    .context("prepare table_info(perseus_batch)")?
    .query_map([], |r| r.get::<_, String>(1))
    .context("query table_info(perseus_batch)")?
    .filter_map(|c| c.ok())
    .any(|c| c == "files_deleted_at");
if !has_col {
    conn.execute("ALTER TABLE perseus_batch ADD COLUMN files_deleted_at TEXT", [])
        .context("add perseus_batch.files_deleted_at")?;
}
```

Also add `files_deleted_at TEXT` to the `DDL` literal itself (fresh DBs get it directly; the guard covers upgrades). `BatchRow` gains `pub files_deleted_at: Option<String>`; extend `list()`'s SELECT + row mapping. New methods:

```rust
pub fn mark_files_deleted(&self, package_ref: &str, at: &str) -> Result<()> {
    let conn = self.conn.lock().expect("batch store mutex poisoned");
    conn.execute(
        "UPDATE perseus_batch SET files_deleted_at = ?2 WHERE package_ref = ?1",
        params![package_ref, at],
    )
    .context("mark perseus_batch files_deleted_at")?;
    Ok(())
}

pub fn delete(&self, package_ref: &str) -> Result<()> {
    let conn = self.conn.lock().expect("batch store mutex poisoned");
    let tx = conn.unchecked_transaction().context("begin batch delete")?;
    tx.execute("DELETE FROM perseus_batch_files WHERE package_ref = ?1", params![package_ref])
        .context("delete perseus_batch_files")?;
    tx.execute("DELETE FROM perseus_batch WHERE package_ref = ?1", params![package_ref])
        .context("delete perseus_batch")?;
    tx.commit().context("commit batch delete")
}

/// DISTINCT package_refs that reference ANY of `sources` — a file's full set of
/// batch participations (original + divert copies), the obligation verdict's
/// cross-batch input. Chunked IN-list (999 SQLite param cap).
pub fn packages_for_sources(&self, sources: &[String]) -> Result<Vec<String>> {
    let conn = self.conn.lock().expect("batch store mutex poisoned");
    let mut out: Vec<String> = Vec::new();
    for chunk in sources.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT package_ref FROM perseus_batch_files WHERE source_path IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql).context("prepare packages_for_sources")?;
        let refs = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |r| r.get::<_, String>(0))
            .context("query packages_for_sources")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect packages_for_sources")?;
        out.extend(refs);
    }
    out.sort();
    out.dedup();
    Ok(out)
}
```

- [ ] **Step 4: Run — PASS** (`cargo test -p perseus batch_store`)
- [ ] **Step 5: Commit** — `feat(perseus): batch store v2 — files_deleted_at column, group delete, divert-participation lookup`

---

### Task 4: `GET /api/transfers` + `GET /api/transfers/events` + obligation verdict

**Files:**
- Modify: `crates/perseus/src/web.rs` (new DTOs + handlers + routes; keep `/api/batches` alive until Task 8)
- Test: `web.rs` `#[cfg(test)]` (handler tests via `test_state()` + `oneshot`)

**Interfaces:**
- Consumes: Task 2 (`list_sync_events_for`), Task 3 (`BatchRow.files_deleted_at`, `packages_for_sources`), `SyncStore::list_outbound_files` (add `use athenaeum_core::sync::SyncStore;`), existing `aggregate_outcome`, `CANCELLED_BY_RECEIVER_DETAIL`, `node_id_hex`, `STATUS_SCAN_LIMIT`.
- Produces (JSON, camelCase): `TransferDto { packageRef, batchUuid, displayName, mode, createdAt, fileCount, totalBytes, filesDeletedAt, outcome, generation, targets: [TransferTargetDto], files: [TransferFileDto], deletable: DeletableDto }`; `TransferTargetDto { rowId, peerHex, name, state, generation, lastError, nextRetryAt, bytesDone, byteSize, createdAt, confirmedAt }`; `TransferFileDto { relPath, byteSize, targets: [{peerHex, state, outcome, error, bytesDone}] }`; `DeletableDto { allowed, deliveredTargets, closed: [String], blockers: [String] }`; `EventDto { ts, kind, detail, target }`. Also the pure `fn obligation_verdict(participations: &[(String, Vec<&OutboundRow>)], names: &HashMap<String, String>) -> DeletableDto` — Task 5 re-runs it server-side at delete time.

**Semantics (spec §3.1/§4.1, encode exactly):**
- Group = one `perseus_batch` row joined to every `sync_outbound` row with the same `package_ref` (reuse `api_batches`'s `by_ref` HashMap idiom). `batchUuid` = `Path::new(package_ref).file_name()` string (fallback: whole ref). `displayName` = first non-empty `display_name` across the group's rows (fallback `None` → UI shows batchUuid). `generation` = max across rows. `totalBytes` = sum of one row's file `byte_size`s (rows share the manifest; use the row with the most file rows). `outcome` = existing `aggregate_outcome`.
- Files matrix: union of `rel_path`s across every row's `list_outbound_files`; per file per target: that row's matching file row (state/outcome/error/bytesDone), absent → target omitted for that file (a partial rebuild shrank that attempt's manifest).
- **Obligation verdict** (row-granular — `Confirmed` already guarantees every receipt non-rejected; `cancelled` receipts inside a confirmed batch are the receiver's per-file human decision):
  - Participations = for THIS batch's source files (`state.batches.files_for(package_ref)` paths; empty linkage → fall back to just this `package_ref`), `packages_for_sources(paths)` ∪ self; each ref resolves to its outbound rows from the same `by_ref` map.
  - Per row: `Confirmed` → delivered (fulfills); `Cancelled` → closed (label `"declined"` when `last_error` starts with `CANCELLED_BY_RECEIVER_DETAIL`, else `"cancelled"`); `Failed` → blocker `"<name>: failed — <last_error>"`; any non-terminal → blocker `"<name>: in flight"`.
  - A participation ref with NO visible rows (aged out of `all_outbound(STATUS_SCAN_LIMIT)`) → blocker `"transfer rows unavailable (aged out)"` — fail closed.
  - `allowed` = no blockers AND ≥1 row total. `deliveredTargets` = count of Confirmed rows. `filesDeletedAt` already set → `allowed = false`, blocker `"files already deleted"`.
- `GET /api/transfers/events?ref=<package_ref>`: for each row of the group, `store.list_sync_events_for(row.id)`, tag each event with the target's device name, merge, sort by `ts` ascending.

- [ ] **Step 1: Failing tests** — write these handler/unit tests first (seed stores through their public APIs exactly like Task 2's seeding; `test_state()` exposes `state.store`/`state.seen`/`state.batches`):

```rust
#[tokio::test]
async fn transfers_groups_one_batch_across_two_targets() { /* seed 1 batch,
    2 outbound rows (peers P1 P2), 2 files each; GET /api/transfers; assert:
    1 element, targets.len()==2, files.len()==2, files[0].targets.len()==2,
    batchUuid == basename, outcome == "sending" */ }

#[test]
fn verdict_confirmed_plus_declined_allows_with_closed_label() { /* rows:
    Confirmed + Cancelled(last_error="cancelled by receiver — …");
    allowed, deliveredTargets==1, closed==["<name>: declined"] */ }

#[test]
fn verdict_failed_or_inflight_blocks() { /* Failed row → blocker with reason;
    separately Announced row → "in flight" blocker; allowed==false */ }

#[test]
fn verdict_divert_participation_blocks_until_new_batch_confirms() { /* batch A
    rows all Cancelled; batch B (sharing a source via packages_for_sources
    seeding) row Announced → A's verdict blocked; flip B to Confirmed → allowed */ }

#[tokio::test]
async fn transfer_events_merges_rows_sorted_and_named() { /* 2 rows, journal
    entries interleaved; assert ts-ascending order and target names attached */ }
```

- [ ] **Step 2: Run — FAIL**
- [ ] **Step 3: Implement DTOs + `obligation_verdict` + `api_transfers` + `api_transfer_events`; register `.route("/api/transfers", get(api_transfers))` and `.route("/api/transfers/events", get(api_transfer_events))` inside the bearer-gated `api` router.** Every handler error: `tracing::error!(error = %msg, "…")` then `(StatusCode::INTERNAL_SERVER_ERROR, msg)` — the file's uniform idiom.
- [ ] **Step 4: Run — PASS**; also `cargo build -p perseus --no-default-features`
- [ ] **Step 5: Commit** — `feat(perseus): grouped /api/transfers read model + per-batch event log + obligation verdict`

---

### Task 5: `POST /api/delete-files` + `POST /api/delete` (history groups)

**Files:**
- Modify: `crates/perseus/src/web.rs` (replace `api_delete`'s body + add `api_delete_files`; the OLD `DeleteRequest {ids}` JS contract dies with the old UI in this same cycle — no external consumers, the page is the only client)
- Modify: `crates/perseus/tests/e2e_loopback.rs` (one full-cycle e2e)
- Test: `web.rs` handler tests + the e2e

**Interfaces:**
- Consumes: Task 1 (`delete_package_sources`, `SourceDeleteDetail`), Task 2 (`delete_outbound_group`), Task 3 (`mark_files_deleted`, `delete`, `packages_for_sources`), Task 4 (`obligation_verdict` — re-run server-side at delete time, never trust the UI's cached verdict).
- Produces:
  - `POST /api/delete-files` body `{"packageRef": "..."}` → `200 {removed: [..], skipped: [[path,reason]..], failed: [[path,error]..], filesDeletedAt: "..."|null}`; `409` with `{blockers: [..]}` when the verdict refuses; `404` unknown ref.
  - `POST /api/delete` body `{"packageRefs": ["..."]}` → `{deleted: [ref..], rejected: [{ref, reason}..]}`.

**Rules:** delete-files re-computes the verdict → refuse `409` on `allowed == false`. On allow: `delete_package_sources(store, seen, ref, "deleted_manual", peer_device)`; set `files_deleted_at` (RFC3339 UTC now, format like the file's existing timestamp writes) ONLY when `failed.is_empty()`; respond the full detail either way. History delete per ref: any non-terminal row → rejected `"a transfer of this batch is still active"`; else `store.delete_outbound_group(&row_ids)` + `batches.delete(ref)`; `perseus_seen` rows are KEPT (dedup identity + retention audit history survive).

- [ ] **Step 1: Failing handler tests**

```rust
#[tokio::test]
async fn delete_files_refuses_while_a_target_is_open() { /* Confirmed + Announced
    rows → POST → 409, body lists the in-flight blocker, files untouched,
    files_deleted_at still NULL */ }

#[tokio::test]
async fn delete_files_removes_sources_and_stamps_batch() { /* two real temp files,
    seen linkage, both rows Confirmed → POST → 200, files gone from disk,
    batch.files_deleted_at set, GET /api/transfers now blocks re-delete */ }

#[tokio::test]
async fn delete_files_confirmed_plus_declined_is_allowed_and_reports_delivery() {
    /* Confirmed + Cancelled(decline detail) → 200; response deliveredTargets
       surfaced via the pre-verdict (include verdict fields in the 200 body) */ }

#[tokio::test]
async fn delete_files_all_duplicate_confirmed_batch_is_deletable() {
    /* spec §8: a Confirmed row whose per-file outcomes are all "duplicate"
       (dedup handshake — nothing traveled) allows deletion. Seed one Confirmed
       row, files state Done + outcome "duplicate" → POST → 200, files removed.
       Pins dedup-counts-as-confirmed against a future per-file verdict refactor. */ }

#[tokio::test]
async fn history_delete_removes_group_keeps_seen() { /* terminal group → POST
    /api/delete → rows+files+events+batch gone; seen.sources_for_package still
    resolves */ }

#[tokio::test]
async fn history_delete_refuses_active_group() { /* one Announced row → rejected
    entry, nothing deleted */ }
```

- [ ] **Step 2: Run — FAIL**
- [ ] **Step 3: Implement both handlers.** `api_delete_files` flow: look up batch (`404` if `batches.list()` has no such ref) → build the same participation set as Task 4 → `obligation_verdict` → `409` or proceed as above. Log every refusal at `info!` (`package_ref`, `blockers` count) and every deletion at `info!(package_ref, removed = n, "manual source delete")`.
- [ ] **Step 4: e2e (`tests/e2e_loopback.rs`)** — one test `delete_files_and_history_after_real_confirm`: reuse the `two_fixtures_are_enqueued_once_and_confirmed` harness up through confirmation, then open `StandaloneSyncStore`/`SeenStore`/`BatchStore` over the same data dir, `build_router` a detached `WebState` (engine not needed for the delete paths), `oneshot` POST `/api/delete-files` → assert the capture fixtures are gone from disk + batch stamped; then POST `/api/delete` → assert `GET /api/transfers` returns empty. This pins spec §8's happy path against a REAL confirmed transfer.
- [ ] **Step 5: Run — PASS** (`cargo test -p perseus`)
- [ ] **Step 6: Commit** — `feat(perseus): obligation-gated source deletion + batch history delete endpoints`

---

### Task 6: Static split + Nord shell + Settings tab

**Files:**
- Create: `crates/perseus/src/web/style.css`, `crates/perseus/src/web/app.js`
- Modify: `crates/perseus/src/web/index.html` (full rewrite: shell only), `crates/perseus/src/web.rs` (serve the two new assets)
- Test: `web.rs` — assets served with correct content-type, un-gated

**Interfaces:**
- Consumes: every existing settings endpoint unchanged (`/api/account*`, `/api/device-name`, `/api/capture-dirs`, `/api/targets*`, `/api/retention/*`, `/api/status`).
- Produces: the two-tab shell + a fully working Settings tab; `window.PerseusApp` structure in `app.js` that Task 7 extends with the Transfers tab renderer. Routes `/app.js`, `/style.css` (bearer-EXEMPT like `/` — static, data-free; document that in the same comment style as the `/` exemption).

**Requirements:**
- `index.html` becomes a slim shell: `<link rel="stylesheet" href="/style.css">`, header (wordmark + conn dot + tab buttons `Transfers` / `Settings`), two empty `<main id="tab-transfers">` / `<main id="tab-settings" hidden>`, `<script src="/app.js"></script>`. No inline CSS/JS.
- `style.css` opens with the Nord token block (spec §6) and styles ONLY via the variables:

```css
:root {
  --surface: #2e3440;         /* nord0  — page background        */
  --surface-raised: #3b4252;  /* nord1  — cards, list rows       */
  --surface-hover: #434c5e;   /* nord2  — hover / selected       */
  --border: #4c566a;          /* nord3                            */
  --content: #eceff4;         /* nord6  — primary text            */
  --content-muted: #d8dee9;   /* nord4  — secondary text          */
  --accent: #88c0d0;          /* nord8  — interactive / links     */
  --accent-strong: #5e81ac;   /* nord10 — primary buttons         */
  --error: #bf616a;           /* nord11 — failed / declined       */
  --warning: #ebcb8b;         /* nord13 — waiting / attention     */
  --success: #a3be8c;         /* nord14 — confirmed / delivered   */
}
```

- `app.js`: port the EXISTING vanilla JS verbatim where behavior is unchanged — token handling, account OTP flow, device-name editor, capture-dirs editor, targets editor, retention policy editor + passes log, status poll. Reorganize into named sections (`// ── account ──` etc.), all Settings sections render into `#tab-settings`. Tab switching = toggle `hidden` + an `active` class; remember the active tab in `localStorage("perseus.tab")`.
- The Transfers tab shows the To Sync strip (port the existing pending-tree + Auto/Manual + Send N logic, collapsed-by-default tree toggled by clicking the counter) and a `<div id="transferList">` placeholder that Task 7 fills. The old Sent / Batched History / History sections are NOT ported (Task 7 replaces them; their endpoints die in Task 8).
- web.rs:

```rust
async fn app_js() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "application/javascript; charset=utf-8")], include_str!("web/app.js"))
}
async fn style_css() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("web/style.css"))
}
```

registered beside `/` (outside the bearer layer).

- [ ] **Step 1: Failing test** — `assets_served_ungated_with_content_types`: `oneshot` GET `/app.js` + `/style.css` on a token-protected router (build_router with `Some("tok")`, NO auth header) → both `200`, content-type asserted.
- [ ] **Step 2: Run — FAIL** → **Step 3: implement** → **Step 4: PASS**
- [ ] **Step 5: Manual smoke** — `cargo run -p perseus -- web` (or the harness the README documents; check `crates/perseus/src/main.rs` subcommands): page loads, tabs switch, every Settings section works against a live agent, zero long file lists anywhere.
- [ ] **Step 6: Commit** — `feat(perseus): web UI v2 shell — split static assets, Nord tokens, Settings tab`

---

### Task 7: Transfers tab — unified list, chips, detail pane, actions

**Files:**
- Modify: `crates/perseus/src/web/app.js`, `crates/perseus/src/web/style.css` (list/detail styles), `crates/perseus/src/web/index.html` (only if the shell needs another container)

**Interfaces:**
- Consumes: `GET /api/transfers` (Task 4 DTO — field names are the contract), `GET /api/transfers/events?ref=`, `POST /api/delete-files` / `/api/delete` (Task 5 shapes incl. the `409 {blockers}`), existing `POST /api/retry|kick|cancel|resend-as-new` (bodies `{ids:[rowId]}` / `{id: rowId}` — read their handlers for exact shapes).
- Produces: the finished Transfers tab.

**Requirements (spec §3.1 — each is acceptance):**
1. Filter chips `All / Sending / Waiting / Completed / Cancelled / Failed` with live counts; membership from the batch `outcome` + per-target states (`waiting` = any target has `nextRetryAt` set and is non-terminal).
2. One row per batch: `displayName` (fallback `batchUuid` short), `createdAt` (existing page's date formatting), `fileCount` + human bytes, target chips (device name + state, colored `--success`/`--error`/`--warning`/`--content-muted`; live targets show `bytesDone/byteSize` %), `attempt N` marker when `generation > 1`, `files deleted` marker when `filesDeletedAt`.
3. Row click toggles the bottom detail pane, sub-tabs **Files / Targets / Log**:
   - Files: table `relPath · byteSize · delivery` where delivery is `"N/N confirmed"` compact when uniform, else per-target breakdown chips (`confirmed (dedup)` when outcome `duplicate`, `missing on <name>` when the file has no row for a target).
   - Targets: per-target rows with state, progress, `lastError`, per-target actions: **Send now** (`/api/kick`), **Cancel** (`/api/cancel`), **Retry** (`/api/retry`), **Resend as new** (`/api/resend-as-new`, only when the target row is declined — `state == "cancelled"` && lastError starts with `"cancelled by receiver"`).
   - Log: events list (`ts · target · kind · detail`), fetched on pane open only.
4. Row-level buttons: **Delete files** enabled iff `deletable.allowed` — disabled state carries `title` = blockers joined; click opens a confirm that lists `removed-to-be` count, `deliveredTargets`, `closed` labels, and — when `deliveredTargets === 0` — a `--error`-styled warning "No target confirmed this batch — these files exist nowhere else." **Delete history** on any all-terminal batch; confirm dialog; on success remove the row locally + refetch.
5. Poll `/api/transfers` on the same cadence the old page polled its lists (reuse the existing poll loop; only while the Transfers tab is active). Render errors into the existing flash/banner idiom, never `alert()`.
6. No file list ever renders outside the detail pane.

- [ ] **Step 1: Build it** (frontend-dev; vanilla, no framework)
- [ ] **Step 2: Manual smoke against a live agent** — two-target fan-out if available, else single: chips filter, detail tabs, disabled-Delete tooltip, both delete flows end-to-end, resend-as-new on a declined row
- [ ] **Step 3: `cargo test -p perseus`** (handler tests must stay green — contract untouched)
- [ ] **Step 4: Commit** — `feat(perseus): web UI v2 Transfers tab — grouped batches, detail pane, delete actions`

---

### Task 8: Retire `/api/sent` + `/api/history` + `delete_confirmed_packages`

**Files:**
- Modify: `crates/perseus/src/web.rs` (drop routes + `api_sent`/`api_history` handlers + their DTOs; drop `/api/batches` + `api_batches` + `BatchDto`/`BatchTargetDto` too — `/api/transfers` replaced it; keep `aggregate_outcome`, Task 4 uses it)
- Modify: `crates/perseus/src/run.rs` (delete `delete_confirmed_packages` + `DeleteReport`/`DeleteRejection` — Task 5's handler stopped calling them)

**Interfaces:** Consumes nothing new; produces a smaller surface. Grep first: `rg "api_sent|api_history|api_batches|delete_confirmed_packages|BatchDto" crates/perseus/` — every remaining reference must be a test to update or dead code to remove. `SeenStore::source_for_package` (singular) STAYS — `resend.rs` uses it.

- [ ] **Step 1: Remove routes/handlers/DTOs/tests of the retired endpoints; migrate any still-valuable assertions onto `/api/transfers` tests**
- [ ] **Step 2: Full gates** — `cargo build --workspace` (must be warning-free in perseus/core), `cargo test -p perseus`, `cargo build -p perseus --no-default-features`
- [ ] **Step 3: Commit** — `refactor(perseus): retire pre-v2 web surface (/api/sent, /api/history, /api/batches, single-row delete)`

---

### Task 9: App receiver stamps `peer_capability` (core)

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (new key), `crates/athenaeum-core/src/api/sync.rs` (`refresh_authorized_peers` + helper), `crates/athenaeum-core/src/sync/store.rs` (DDL + guarded column + `InboundRow.peer_capability` + setter), `crates/athenaeum-core/src/sync/ingest.rs` (`cached_device_capability`), `crates/athenaeum-core/src/sync/receiver.rs` (stamp in `handle_announce`)
- Test: store tests + a receiver test in the existing harness (`receiver.rs` `#[cfg(test)]`, fixture around line 2374)

**Interfaces:**
- Consumes: `AccountDevice.capability: DeviceCapability { Athenaeum, Perseus }`, `pubkey_b64_to_hex`, the `SYNC_DEVICE_NAMES` write/read idiom.
- Produces: settings key `keys::SYNC_PEER_CAPABILITIES = "sync.peer_capabilities"` (JSON hex→`"athenaeum"|"perseus"`); `InboundRow.peer_capability: Option<String>` (`#[serde(default)]`, doc: stamped at announce from the device-list cache; survives later device revocation); `store::set_inbound_peer_capability(conn, id, cap: &str)`; `ingest::cached_device_capability(conn, peer_hex) -> Option<String>`. Task 10 reads the row field + the cache key.

- [ ] **Step 1: Failing store test** — `peer_capability_roundtrips_and_defaults_null`: upsert an inbound row (existing seeder), assert `None`; `set_inbound_peer_capability` → re-read → `Some("perseus")`.
- [ ] **Step 2: Implement storage**: `peer_capability TEXT` appended to `DDL_INBOUND`; `ensure_inbound_columns` adds it via the existing guarded-ALTER list; extend `InboundRaw` tuple + the SELECT column list + `to_inbound` + `InboundRow`. Setter:

```rust
pub fn set_inbound_peer_capability(conn: &Connection, id: i64, cap: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET peer_capability = ?2 WHERE id = ?1",
        params![id, cap],
    )
    .context("set sync_inbound.peer_capability")?;
    Ok(())
}
```

- [ ] **Step 3: Cache plumbing.** In `api/sync.rs` beside `account_device_names`:

```rust
fn account_device_capabilities(
    devices: &[crate::account::AccountDevice],
) -> HashMap<String, String> {
    devices
        .iter()
        .filter_map(|d| {
            pubkey_b64_to_hex(&d.pubkey).map(|hex| {
                let cap = match d.capability {
                    crate::account::DeviceCapability::Perseus => "perseus",
                    crate::account::DeviceCapability::Athenaeum => "athenaeum",
                };
                (hex, cap.to_string())
            })
        })
        .collect()
}
```

`refresh_authorized_peers` writes it next to the names cache (same best-effort match/warn shape, key `SYNC_PEER_CAPABILITIES`). In `ingest.rs`, `cached_device_capability` = copy of `cached_device_name` reading the new key (warn strings say "capability cache").

- [ ] **Step 4: Stamp.** In `handle_announce`, immediately after the `upsert_inbound_attempt` call yields `(inbound_id, …)` on the non-declined path (best-effort — a cache miss or write failure must never affect the transfer):

```rust
{
    let conn = store.lock_conn();
    if let Some(cap) = super::ingest::cached_device_capability(&conn, &peer_device) {
        if let Err(error) = super::store::set_inbound_peer_capability(&conn, inbound_id, &cap) {
            tracing::warn!(%error, inbound_id, "failed to stamp peer capability");
        }
    }
}
```

(Adjust paths/visibility to the module's actual idiom — `cached_device_capability` may need `pub(crate)`.)

- [ ] **Step 5: Receiver test** — seed the capability cache setting in the harness DB, run an announce through the fixture, assert the row's `peer_capability == Some("perseus")`; second test: no cache → `None`, transfer still lands.
- [ ] **Step 6: Refresh test** — beside the existing `account_peer_hexes` test: `refresh` writes the JSON map (or unit-test `account_device_capabilities` directly if the refresh path needs a live hub — mirror how the names cache is tested).
- [ ] **Step 7: Run** — `cargo test -p athenaeum-core --lib sync::` PASS
- [ ] **Step 8: Commit** — `feat(sync): stamp announcing peer's device capability onto sync_inbound (survives revocation)`

---

### Task 10: Surface `peerKind` + `get_sync_device_capabilities` (core + both backends + TS)

**Files:**
- Modify: `crates/athenaeum-core/src/sync/status.rs` (`InboundSummary.peer_kind`), `crates/athenaeum-core/src/api/sync.rs` (mapper + its callers + new command fn), `crates/athenaeum-tauri/src/commands/sync.rs` + `crates/athenaeum-tauri/src/lib.rs` (wrapper + registration), `crates/athenaeum-web/src/routes/sync.rs` + `routes/mod.rs` (mirror), `src/types/models.ts` (regenerated)
- Test: mapper unit test in `api/sync.rs`

**Interfaces:**
- Consumes: Task 9 (`InboundRow.peer_capability`, `SYNC_PEER_CAPABILITIES`).
- Produces: `InboundSummary.peer_kind: Option<String>` (`"athenaeum" | "perseus"`); core `pub async fn get_sync_device_capabilities(ctx) -> Result<HashMap<String, String>, ApiError>` (hex→kind, hub-list-derived, empty-map degradation exactly like `get_sync_device_names`); command name `get_sync_device_capabilities` on both backends. Task 11 consumes both.

- [ ] **Step 1: Failing mapper test** — stamped row → `peer_kind = Some("perseus")`; NULL-stamped row + cache map containing the hex → cache value; neither → `None`.
- [ ] **Step 2: Implement.** `inbound_summary` gains a `device_kinds: &HashMap<String, String>` param; `peer_kind: row.peer_capability.clone().or_else(|| device_kinds.get(&row.peer).cloned())`. Callers (`active_inbound_summaries`, `list_terminal_transfers` — find every call site) load the cache once per call via a small `fn cached_device_kind_map(conn: &Connection) -> HashMap<String, String>` (same parse-warn shape as `cached_device_name`). New command = copy of `get_sync_device_names` with the capability match from Task 9's helper. Tauri wrapper beside `commands/sync.rs:244` with `#[tracing::instrument(skip_all, err)]`; axum route beside `routes/sync.rs:337`, registered in `routes/mod.rs` (`.route("/api/get_sync_device_capabilities", post(sync::get_sync_device_capabilities))`).
- [ ] **Step 3: Regenerate TS** — `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; verify `src/types/models.ts` gains `peerKind: string | null` on `InboundSummary`.
- [ ] **Step 4: Gates** — `cargo build --workspace`, core lib tests, `npx tsc --noEmit`
- [ ] **Step 5: Commit** — `feat(sync): peerKind on InboundSummary + get_sync_device_capabilities (both backends)`

---

### Task 11: App badge — sender kind on received transfers

**Files:**
- Modify: `src/hooks/useTransferHistory.ts` (fetch capabilities map beside names), `src/components/transfers/types.ts` (thread `deviceKind`), `src/pages/Transfers.tsx` (pass-through on history rows), `src/components/transfers/TransferRow.tsx` (badge), `src/components/transfers/TransferDetails.tsx` (Details line), `src/hooks/useTransferQueue.ts` (thread `peerKind` from `InboundSummary` if the row mapper strips fields — verify)

**Interfaces:**
- Consumes: `InboundSummary.peerKind`, `api.invoke<Record<string,string>>('get_sync_device_capabilities')`.
- Produces: received rows show a small badge — lucide `Telescope` icon + text `Perseus` (accent-muted chip) when kind is `perseus`; no badge for `athenaeum` (the common case stays clean) — plus a `Sender` line in the Details sub-tab (`<device name> · Perseus agent` / `· Athenaeum`). Design tokens only; unknown kind → nothing.

- [ ] **Step 1: Implement** (frontend-dev). Resolution order per received row: live `peerKind` → capabilities map by peer hex → null. History fetch mirrors the existing `get_sync_device_names` cancelled-flag pattern in `useTransferHistory.ts:79`.
- [ ] **Step 2: `npx tsc --noEmit`** PASS
- [ ] **Step 3: Manual smoke** — desktop app + a Perseus send (or seeded row): badge renders on the received row + Details line; Athenaeum-to-Athenaeum rows unchanged.
- [ ] **Step 4: Commit** — `feat(transfers): show sender kind badge on received transfers`

---

## Final verification (controller, after Task 11)

- [ ] `cargo build --workspace` — clean, no new warnings
- [ ] `cargo build -p perseus --no-default-features` — clean
- [ ] `cargo test -p perseus` + `cargo test -p athenaeum-core --lib sync::` — green
- [ ] `npx tsc --noEmit` — green
- [ ] Live smoke (owner or controller with a real agent): two-instance transfer Perseus→app; Perseus page — batch grouping, both deletes, declined divert; app — Perseus badge on the received row
- [ ] Update `CLAUDE.md`'s Transfers section with one line: Perseus web UI v2 (two tabs, `/api/transfers` model, obligation-gated source delete) + `sync_inbound.peer_capability`
