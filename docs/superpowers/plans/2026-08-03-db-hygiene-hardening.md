# DB Hygiene & Transaction Discipline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every Critical and non-deferred Important finding of the 2026-08-03 DB layer audit (`docs/superpowers/research/2026-08-03-db-layer-audit.md`): one SQL injection, two silent scanner data-loss paths, the Black Hole/Void atomicity hole, the raw-BEGIN leak sites, the web plate-solve panic swallow, and the query-correctness defects.

**Architecture:** Five phases. A — stop the bleeding (injection, scanner swallows). B — Black Hole / Void atomicity + the frames-side cleanup trigger. C — transaction-discipline sweep (SavepointGuard / `unchecked_transaction` everywhere the audit found raw BEGIN or bare autocommit sequences). D — panic & async hygiene at the plate-solve boundary + defensive timestamp parsing. E — query correctness (upsert guard, deterministic set metadata) + a minors batch. Fixes reuse patterns that already exist in the codebase (`SavepointGuard`, `unchecked_transaction`, catch_unwind worker shells) — no new machinery.

**Tech Stack:** Rust (athenaeum-core + Tauri/Axum wrappers), rusqlite 0.40 (bundled SQLite ≥ 3.50), r2d2.

## Global Constraints

- Every changed Tauri command keeps its Axum mirror in sync in the same task (`crates/athenaeum-web/src/routes/<domain>.rs`).
- Never swallow errors: `tracing::error!`/`warn!` before returning. Message = short stable phrase, data in snake_case fields (`info!(file_id, "…")`), per the logging spec's field dictionary.
- `anyhow::Result` inside core; `.map_err(|e| e.to_string())` at the Tauri boundary, `db_err`/`(StatusCode, String)` at the Axum boundary.
- No third-party tool names in code or comments.
- Gates per task: the test command listed in the task. Full gates before merge: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. clippy is NOT a gate; format touched files with `rustfmt <files>` (never `cargo fmt -p`).
- Commit as the user (`eg013ra1n <vilen.sharifov@gmail.com>`), one commit per task, on the `0.5.1` branch.
- Tests use in-memory SQLite via `crate::db::schema::init_db` fixtures (existing pattern in each touched module). If a fixture INSERT trips a NOT NULL you didn't expect, add the minimal missing column to the INSERT — do not weaken the schema.
- `SavepointGuard` lives at `crates/athenaeum-core/src/db/operations.rs:148-182` (`pub(crate)`), reachable crate-wide as `crate::db::SavepointGuard` via the glob re-export in `db/mod.rs`. If name resolution fails from a sibling module, add `pub(crate) use operations::SavepointGuard;` to `crates/athenaeum-core/src/db/mod.rs` — do not change the struct's visibility.
- Line numbers in this plan were captured on 2026-08-03 at commit `12b0b772`; re-anchor with the quoted code if they have drifted.

---

## Phase A — Stop the bleeding

### Task 1: Parameterize `get_black_hole_files` (audit C1)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs:199-218`
- Test: same file, `#[cfg(test)]` module

**Interfaces:**
- Produces: `get_black_hole_files(conn, filter_by_source)` — signature unchanged, `filter` now bound as `?1`.

- [ ] **Step 1: Write the failing test** (append to the file's test module; create one if absent, using the same `init_db` fixture style as sibling db tests):

```rust
#[test]
fn black_hole_filter_is_bound_not_spliced() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format)
         VALUES ('/t/a.fits','a.fits',1,'2026-01-01T00:00:00Z','FITS')", []).unwrap();
    let fid = conn.last_insert_rowid();
    add_to_black_hole(&conn, fid, "duplicates", "/t/a.fits").unwrap();

    // A single quote in the filter must be data, not syntax: no SQL error,
    // zero rows (no source is literally named this).
    let evil = "x' UNION SELECT 1,1,'p','p','w','2026-01-01T00:00:00Z',1 --".to_string();
    let rows = get_black_hole_files(&conn, Some(evil)).unwrap();
    assert!(rows.is_empty(), "injection text must match nothing: {rows:?}");

    // And a legitimate filter still works.
    let rows = get_black_hole_files(&conn, Some("duplicates".into())).unwrap();
    assert_eq!(rows.len(), 1);
}
```

- [ ] **Step 2: Run it — expect FAIL.** `cargo test -p athenaeum-core black_hole_filter_is_bound` — the UNION text currently returns a fabricated row (or errors), not an empty set.

- [ ] **Step 3: Replace the `format!` query with a bound parameter.** Replace lines 203-218 with:

```rust
    let query = if filter_by_source.is_some() {
        "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
         FROM black_hole bh
         JOIN files f ON bh.file_id = f.id
         WHERE bh.from_where = ?1
         ORDER BY bh.moved_at DESC"
    } else {
        "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
         FROM black_hole bh
         JOIN files f ON bh.file_id = f.id
         ORDER BY bh.moved_at DESC"
    };
    let params: Vec<rusqlite::types::Value> = match filter_by_source {
        Some(s) => vec![s.into()],
        None => vec![],
    };

    let mut stmt = conn.prepare(query)?;

    let entries = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
```

(keep the existing row-mapping closure and collection code unchanged; only the query construction and the `query_map` argument change).

- [ ] **Step 4: Run the test — expect PASS.** `cargo test -p athenaeum-core black_hole_filter_is_bound`

- [ ] **Step 5: Commit** — `fix(db): bind black-hole source filter as a parameter, closing SQL injection`

### Task 2: Scanner new-file inserts are atomic per file; header failure is surfaced (audit C2 + C3 prevention)

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:2168-2222` (new-file branch of the parallel write loop)

**Interfaces:**
- Consumes: `crate::db::SavepointGuard` (nests inside the loop's outer `BEGIN TRANSACTION` at `scanner/mod.rs:1923`).
- Produces: a `files` row can no longer commit without its `frames` row; `insert_fits_header` failure rolls the whole file back and lands in `result.errors`.

- [ ] **Step 1: Rewrite the new-file branch.** Replace the block at lines 2168-2222 (`// New file path — existing INSERT behavior.` through the closing brace of the `match insert_file` arm) with:

```rust
        // New file path — atomic per file: files + frames + fits_header
        // either all commit or none do (SAVEPOINT nests inside the batch
        // transaction). Previously an insert_frame/insert_fits_header
        // failure after a successful insert_file committed a frameless or
        // headerless files row that every later scan classified as
        // "unchanged" and never repaired.
        let sp = match crate::db::SavepointGuard::new(conn, "scan_new_file") {
            Ok(sp) => sp,
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: failed to open savepoint: {}", file_result.file.path, e));
                continue;
            }
        };
        let file_id = match insert_file(conn, &file_result.file) {
            Ok(id) => id,
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert file: {}", file_result.file.path, e));
                continue; // sp drops → rollback
            }
        };
        // Update frame file_id in place (avoids clone of 32-field struct)
        file_result.frame.file_id = file_id;

        // Apply unique camera suffix to INSTRUME if enabled
        if unique_camera {
            if let Some(ref instrume) = file_result.frame.instrume {
                file_result.frame.instrume = Some(format!("{} N{}", instrume, root_id));
            }
        }

        let frame_id = match insert_frame(conn, &file_result.frame) {
            Ok(id) => id,
            Err(e) => {
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert frame: {}", file_result.file.path, e));
                continue; // sp drops → the files row rolls back too
            }
        };

        // Insert header if available — a failure here rolls the whole file
        // back so the next scan retries it, instead of committing a frame
        // whose header blob (metadata revert, light-cal copy-through) is
        // silently missing.
        if let Some(ref header) = file_result.header {
            if let Err(e) = insert_fits_header(conn, file_id, header) {
                tracing::error!(file_id, path = %file_result.file.path, error = %e,
                    "failed to insert fits header; rolling file back");
                errors.lock().unwrap().push(format!(
                    "{}: Failed to insert header: {}", file_result.file.path, e));
                continue; // sp drops → rollback
            }
        }

        if let Err(e) = sp.commit() {
            errors.lock().unwrap().push(format!(
                "{}: failed to commit file savepoint: {}", file_result.file.path, e));
            continue;
        }

        result.files_processed += 1;
        // A genuinely new file+frame row — eligible for auto-mode sync.
        new_file_ids.push(file_id);

        // Track by image type
        if let Some(ref imagetyp) = file_result.imagetyp {
            match imagetyp {
                ImageType::Light => lights_count += 1,
                ImageType::Flat => flat_frame_ids.push(frame_id),
                ImageType::Dark => dark_frame_ids.push(frame_id),
                ImageType::Bias => bias_frame_ids.push(frame_id),
                ImageType::DarkFlat => darkflat_frame_ids.push(frame_id),
                ImageType::MasterDark => master_dark_ids.push(frame_id),
                ImageType::MasterFlat => master_flat_ids.push(frame_id),
                ImageType::MasterBias => master_bias_ids.push(frame_id),
                ImageType::MasterDarkFlat => master_darkflat_ids.push(frame_id),
                _ => {}
            }
        }
```

Note the deliberate ordering change vs. the old code: counters and id-lists update only **after** `sp.commit()` succeeds.

- [ ] **Step 2: Grep for regressions in the same file.** Run `grep -n "let _ = insert_fits_header" crates/athenaeum-core/src/scanner/mod.rs` — expect zero hits. Also confirm the sequential path (`process_file`, ~line 628-646) still logs its header failure (untouched by this task).

- [ ] **Step 3: Run the scanner test module.** `cargo test -p athenaeum-core scanner` — expect PASS (existing tests; the behavior change is failure-path-only).

- [ ] **Step 4: Commit** — `fix(scanner): per-file savepoint in the insert loop; header failures roll back and surface`

### Task 3: A frameless `files` row re-parses instead of being skipped forever (audit C3 cure)

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:1719-1746` (parallel-scan existing-files map), `:1754-1783` (classification), and the sequential path's per-file check (`scan_directory`, the `SELECT id, size, modified_at FROM files WHERE path = ?1` at ~line 243 with its `unchanged` computation just below)
- Test: `crates/athenaeum-core/src/scanner/mod.rs` test module

**Interfaces:**
- Consumes: the `frame_count == 0` self-heal already inside `reparse_and_update_in_place` (`scanner/mod.rs:1153-1174`) — this task only makes it reachable.
- Produces: classification treats "files row exists but no frames row" as *modified* on both scan paths.

- [ ] **Step 1: Write the failing test** (append to the scanner test module; `write_minimal_fits` — reuse or copy the helper from `archive/restore.rs` tests):

```rust
#[test]
fn scan_heals_a_frameless_files_row() {
    use crate::events::NullEmitter;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("L_001.fits");
    write_minimal_fits(&f);

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
        [dir.path().to_str().unwrap()]).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let r1 = scan_directory_parallel(dir.path(), 1, &conn, &NullEmitter,
        false, cancel.clone(), false);
    assert!(r1.errors.is_empty(), "first scan clean: {:?}", r1.errors);
    let file_id: i64 = conn.query_row(
        "SELECT id FROM files WHERE filename = 'L_001.fits'", [], |r| r.get(0)).unwrap();

    // Simulate the historical C3 orphan: files row present, frames row gone.
    conn.execute("DELETE FROM frames WHERE file_id = ?1", [file_id]).unwrap();

    // Size and mtime are unchanged — the old classification skipped this
    // file forever. It must now be re-parsed and the frames row recreated,
    // with files.id preserved.
    let r2 = scan_directory_parallel(dir.path(), 1, &conn, &NullEmitter,
        false, cancel, false);
    assert!(r2.errors.is_empty(), "healing scan clean: {:?}", r2.errors);
    let frames: i64 = conn.query_row(
        "SELECT COUNT(*) FROM frames WHERE file_id = ?1", [file_id], |r| r.get(0)).unwrap();
    assert_eq!(frames, 1, "frameless files row must be re-parsed");
}
```

- [ ] **Step 2: Run it — expect FAIL** (`cargo test -p athenaeum-core scan_heals_a_frameless`): the second scan classifies the file as unchanged and `frames == 0`.

- [ ] **Step 3: Extend the parallel-scan map with a has-frame flag.** At lines 1719-1746, change the map type and query:

```rust
    let existing_files: std::collections::HashMap<String, (i64, i64, String, bool)> = {
        let mut map = std::collections::HashMap::new();
        match conn.prepare(
            "SELECT f.path, f.id, f.size, f.modified_at,
                    EXISTS(SELECT 1 FROM frames fr WHERE fr.file_id = f.id)
             FROM files f") {
            Ok(mut stmt) => {
                match stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, bool>(4)?,
                    ))
                }) {
                    Ok(rows) => {
                        for entry in rows.flatten() {
                            map.insert(entry.0, (entry.1, entry.2, entry.3, entry.4));
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to query existing files");
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to prepare existing files query");
            }
        }
        map
    };
```

- [ ] **Step 4: Require the frame in the unchanged-check.** In the classification closure (lines 1754-1783), change the match arm binding and condition:

```rust
                Some((db_id, db_size, db_modified, has_frame)) => {
                    match std::fs::metadata(p) {
                        Ok(meta) => {
                            let on_disk_size = meta.len() as i64;
                            let on_disk_modified = meta
                                .modified()
                                .ok()
                                .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                            // A files row with no frames row is never
                            // "unchanged" — route it through re-parse so the
                            // frame_count == 0 self-heal in
                            // reparse_and_update_in_place can recreate the
                            // frame (audit C3).
                            let unchanged = on_disk_size == *db_size
                                && on_disk_modified.as_deref() == Some(db_modified.as_str())
                                && *has_frame;
```

(rest of the closure unchanged — the `else` branch already pushes `(path_str, *db_id)` into `modified_paths`).

- [ ] **Step 5: Same change on the sequential path.** In `scan_directory` (per-file check at ~line 243), extend the query and the condition:

```rust
                "SELECT id, size, modified_at,
                        EXISTS(SELECT 1 FROM frames WHERE file_id = files.id)
                 FROM files WHERE path = ?1",
```

carry the fourth column as `has_frame: bool` through the surrounding
`existing` tuple, and add `&& has_frame` to that path's `unchanged`
computation (same rationale comment as Step 4).

- [ ] **Step 6: Run the test — expect PASS**, then the whole scanner module: `cargo test -p athenaeum-core scanner`.

- [ ] **Step 7: Commit** — `fix(scanner): frameless files rows classify as modified and self-heal via re-parse`

---

## Phase B — Black Hole / Void atomicity

### Task 4: `add_to_black_hole` / `send_to_void` are transactional; Void deletes catalog before disk (audit C5 + I3)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs:71-96` (`add_to_black_hole`), `:251-284` (`send_to_void`)
- Test: same file's test module

**Interfaces:**
- Consumes: `crate::db::SavepointGuard`; `unregister_master_if_any` (unchanged — it finally gets the caller-transaction its doc contract promises).
- Produces: both functions nest cleanly inside an outer transaction (savepoint semantics) — Task 5 relies on this for the bulk path.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn black_hole_and_void_nest_inside_an_outer_transaction() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format)
         VALUES ('/t/v.fits','v.fits',1,'2026-01-01T00:00:00Z','FITS')", []).unwrap();
    let fid = conn.last_insert_rowid();

    // Raw BEGIN inside the functions would error with "cannot start a
    // transaction within a transaction"; savepoints must nest.
    let tx = conn.unchecked_transaction().unwrap();
    add_to_black_hole(&conn, fid, "duplicates", "/t/v.fits").unwrap();
    send_to_void(&conn, fid).unwrap();
    drop(tx); // rollback

    // The outer rollback must take the inner writes with it.
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(files, 1, "outer rollback must restore the files row");
    let bh: i64 = conn.query_row("SELECT COUNT(*) FROM black_hole", [], |r| r.get(0)).unwrap();
    assert_eq!(bh, 0);
}
```

- [ ] **Step 2: Run it — expect PASS-or-FAIL check.** `cargo test -p athenaeum-core black_hole_and_void_nest` — currently the functions open no transaction at all, so the calls succeed but the assertion holds trivially only if autocommit writes joined the outer tx (they do — statements on a connection with an open tx join it). The test's real value is pinning the savepoint refactor doesn't break nesting; expect it to PASS before AND after. Keep it.

- [ ] **Step 3: Wrap `add_to_black_hole`.** Replace the body (lines 71-96) with:

```rust
pub fn add_to_black_hole(
    conn: &Connection,
    file_id: i64,
    from_where: &str,
    original_path: &str,
) -> Result<i64> {
    // One atomic unit: the master-unregister sequence (6 statements, doc
    // contract: "runs in the CALLER's transaction") plus the black-hole
    // insert. Without this, a failure mid-unregister left the calibration
    // lineage permanently half-rewired (audit C5).
    let sp = crate::db::SavepointGuard::new(conn, "add_to_black_hole")?;

    unregister_master_if_any(conn, file_id)?;

    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO black_hole (file_id, from_where, moved_at, original_path)
         VALUES (?1, ?2, ?3, ?4)",
        params![file_id, from_where, now, original_path],
    )?;

    // `last_insert_rowid()` is stale when the insert is ignored (already present),
    // so resolve the row id explicitly — correct for both fresh and repeat calls.
    let id: i64 = conn.query_row(
        "SELECT id FROM black_hole WHERE file_id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;

    sp.commit()?;
    Ok(id)
}
```

- [ ] **Step 4: Rewrite `send_to_void` — catalog first, disk second.** Replace the body (lines 256-284) with:

```rust
pub fn send_to_void(conn: &Connection, file_id: i64) -> Result<()> {
    // Get file path before deletion
    let path: String = conn.query_row(
        "SELECT path FROM files WHERE id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;

    // Catalog first, disk second (same stance as api::masters::delete_master):
    // the benign crash leftover is an orphan file on disk — which a later
    // scan simply re-ingests — never a catalog row pointing at a file that
    // is gone forever. All catalog writes are one atomic unit so the
    // master-unregister sequence can't half-commit (audit C5/I3).
    let sp = crate::db::SavepointGuard::new(conn, "send_to_void")?;
    unregister_master_if_any(conn, file_id)?;
    conn.execute("DELETE FROM black_hole WHERE file_id = ?1", params![file_id])?;
    // files delete cascades to frames, frame_tags, etc.
    conn.execute("DELETE FROM files WHERE id = ?1", params![file_id])?;
    sp.commit()?;

    if std::path::Path::new(&path).exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::error!(file_id, path = %path, error = %e,
                "send_to_void: catalog rows removed but disk delete failed; file remains on disk");
            return Err(e.into());
        }
    }

    Ok(())
}
```

- [ ] **Step 5: Update the two doc comments** above the functions (lines 62-70 and 251-255) to describe the new ordering (catalog-first) and atomicity; drop the now-false "before the disk file goes" sentence.

- [ ] **Step 6: Run the module tests.** `cargo test -p athenaeum-core operations_blackhole` (plus `black_hole_and_void_nest`, `black_hole_filter_is_bound`). Expected: PASS. Also run `cargo test -p athenaeum-core master_unregister` — the un-supersede tests must still pass through the new savepoint.

- [ ] **Step 7: Commit** — `fix(db): black-hole/void run atomically; void deletes catalog before disk`

### Task 5: Per-file savepoints in `bulk_move_to_black_hole` (audit C5-bulk, C4 leak class)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs:108-196`
- Test: same file's test module

**Interfaces:**
- Consumes: Task 4's savepoint-based `add_to_black_hole` shape (this function inlines the same insert; keep them consistent).
- Produces: a file listed in `BulkMoveResult::failed` has had NO writes committed for it.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn bulk_move_failed_file_leaves_no_partial_writes() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format)
         VALUES ('/t/b.fits','b.fits',1,'2026-01-01T00:00:00Z','FITS')", []).unwrap();
    let good = conn.last_insert_rowid();

    // file_id 9999 has no files row → its path lookup fails → it must land
    // in `failed` while the good file still moves, and the batch commits.
    let r = bulk_move_to_black_hole(&conn, &[good, 9999], "duplicates", None).unwrap();
    assert_eq!(r.moved, 1);
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.failed[0].0, 9999);
    let bh: i64 = conn.query_row("SELECT COUNT(*) FROM black_hole", [], |r| r.get(0)).unwrap();
    assert_eq!(bh, 1, "only the good file is in the black hole");
}
```

- [ ] **Step 2: Run it.** `cargo test -p athenaeum-core bulk_move_failed_file` — expect PASS already (this failure path predates the fix); it pins the batch-continues contract across the refactor.

- [ ] **Step 3: Replace raw BEGIN/COMMIT with an outer guard + per-file inner guards.** In `bulk_move_to_black_hole`, replace line 117 (`conn.execute("BEGIN TRANSACTION", [])?;`) with:

```rust
    let outer = crate::db::SavepointGuard::new(conn, "bulk_black_hole")?;
```

replace line 193 (`conn.execute("COMMIT", [])?;`) with:

```rust
    outer.commit()?;
```

and wrap the per-file work (the `unregister_master_if_any` call through the insert `match`, lines 140-165) as:

```rust
        // Per-file savepoint: a file that fails mid-sequence (e.g. its
        // master-unregister dies on statement 4 of 6) must contribute ZERO
        // committed writes — previously its partial writes rode the batch
        // COMMIT while the file was reported as failed (audit C5).
        let sp = match crate::db::SavepointGuard::new(conn, "bulk_black_hole_file") {
            Ok(sp) => sp,
            Err(e) => {
                failed.push((*file_id, format!("savepoint open failed: {}", e)));
                continue;
            }
        };

        // A master file gives up its registration before it is black-holed;
        // a failure there fails THIS file only, never the batch.
        if let Err(e) = unregister_master_if_any(conn, *file_id) {
            failed.push((*file_id, format!("master unregister failed: {}", e)));
            continue; // sp drops → this file's partial writes roll back
        }

        let insert = conn.execute(
            "INSERT OR IGNORE INTO black_hole (file_id, from_where, moved_at, original_path)
             VALUES (?1, ?2, ?3, ?4)",
            params![file_id, from_where, now, path],
        );

        match insert {
            // changed == 0 means the file was already in the black hole — a
            // silent idempotent no-op (not a move, not a failure).
            Ok(changed) => match sp.commit() {
                Ok(()) => {
                    if changed > 0 {
                        moved += 1;
                    }
                }
                Err(e) => {
                    failed.push((*file_id, format!("savepoint commit failed: {}", e)));
                    continue;
                }
            },
            Err(e) => {
                tracing::error!(file_id, path = %path, error = %e, "bulk_move_to_black_hole: failed to move file");
                failed.push((*file_id, e.to_string()));
                continue; // sp drops → rollback
            }
        }
```

(the path lookup above and the progress-emit block below stay unchanged;
`continue` after the emit is unaffected because the emit block is after the
match).

- [ ] **Step 4: Verify the doc comment** (lines 98-107) still matches: update "A connection-level error (transaction begin/commit fails) returns `Err` and leaves the DB unchanged" if wording needs the savepoint vocabulary.

- [ ] **Step 5: Run the tests.** `cargo test -p athenaeum-core operations_blackhole` — PASS.

- [ ] **Step 6: Commit** — `fix(db): per-file savepoints in bulk black-hole; failed files commit nothing`

### Task 6: frames-side cleanup trigger for `calibration_set_to_frames` (audit I9)

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` — insert immediately after the `calibration_set_subcal_cleanup` trigger block (lines 876-885)
- Test: `crates/athenaeum-core/src/db/schema.rs` test module

**Interfaces:**
- Produces: deleting a `frames` row by ANY means (direct DELETE or `files`→`frames` FK CASCADE — cascades DO fire triggers, verified empirically 2026-08-03) removes its `source_type='frame'` consumer links. The manual DELETEs in `delete_scan_root` / `bulk_update_frame_metadata` become redundant-but-harmless; leave them.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn deleting_a_frame_cleans_its_consumer_links() {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format)
         VALUES ('/t/l.fits','l.fits',1,'2026-01-01T00:00:00Z','FITS')", []).unwrap();
    let file_id = conn.last_insert_rowid();
    conn.execute("INSERT INTO frames (file_id) VALUES (?1)", [file_id]).unwrap();
    let frame_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark','2026-01-01')", []).unwrap();
    let set_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type)
         VALUES (?1, 'frame', ?2, 'dark')",
        rusqlite::params![frame_id, set_id]).unwrap();

    // Kill the frame via the files→frames CASCADE (the send_to_void shape).
    conn.execute("DELETE FROM files WHERE id = ?1", [file_id]).unwrap();

    let left: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calibration_set_to_frames", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0, "consumer link must die with its frame");
}
```

- [ ] **Step 2: Run it — expect FAIL** (`cargo test -p athenaeum-core deleting_a_frame_cleans`): the link row survives.

- [ ] **Step 3: Add the trigger + one-time sweep** right after the `calibration_set_subcal_cleanup` block (after line 885):

```rust
    // Mirror of calibration_set_subcal_cleanup for the OTHER side of the
    // polymorphic (source_id, source_type) reference: when a frames row dies
    // — direct DELETE or files→frames FK CASCADE; cascade deletes DO fire
    // AFTER DELETE triggers — its consumer links must not linger. Before
    // this trigger, send_to_void, the relinking orphan purge, and
    // delete_missing_files each leaked one calibration_set_to_frames row per
    // deleted linked frame, forever (2026-08-03 audit I9). DROP+CREATE is
    // the codebase's trigger-evolution mechanism (see the UUID triggers).
    conn.execute("DROP TRIGGER IF EXISTS frame_subcal_cleanup", [])?;
    conn.execute(
        "CREATE TRIGGER frame_subcal_cleanup AFTER DELETE ON frames
         FOR EACH ROW
         BEGIN
             DELETE FROM calibration_set_to_frames
             WHERE source_type = 'frame' AND source_id = OLD.id;
         END",
        [],
    )?;
    // Idempotent sweep of orphans leaked before the trigger existed.
    conn.execute(
        "DELETE FROM calibration_set_to_frames
         WHERE source_type = 'frame'
           AND source_id NOT IN (SELECT id FROM frames)",
        [],
    )?;
```

- [ ] **Step 4: Run the test — expect PASS**, then the full schema + calibration modules: `cargo test -p athenaeum-core schema && cargo test -p athenaeum-core calibration` (the relink/supersede tests must be unaffected — the trigger only fires on frame deletion).

- [ ] **Step 5: Commit** — `fix(schema): frame-side cleanup trigger for polymorphic consumer links + orphan sweep`

---

## Phase C — Transaction-discipline sweep

### Task 7: `rebuild_folder_similarity_cache` — compute outside the lock, SavepointGuard inside (audit C4)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs:400-437`
- Test: same file's test module

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn similarity_rebuild_nests_inside_an_outer_transaction() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    // Raw BEGIN inside the function would error here; savepoints nest.
    let tx = conn.unchecked_transaction().unwrap();
    rebuild_folder_similarity_cache(&conn, 50.0).unwrap();
    tx.commit().unwrap();
}
```

- [ ] **Step 2: Run it — expect FAIL** (`cargo test -p athenaeum-core similarity_rebuild_nests`): "cannot start a transaction within a transaction".

- [ ] **Step 3: Rewrite the function:**

```rust
pub fn rebuild_folder_similarity_cache(
    conn: &Connection,
    similarity_threshold: f64,
) -> Result<usize> {
    // Compute OUTSIDE the transaction: the pairwise folder comparison is
    // in-memory CPU work and previously ran while holding SQLite's sole
    // write lock, starving every other writer for its whole runtime
    // (audit C4). Only the DELETE + INSERTs need the lock.
    let similarities = find_duplicate_folders(conn, similarity_threshold)?;

    let now = chrono::Utc::now().to_rfc3339();
    let sp = crate::db::SavepointGuard::new(conn, "folder_similarity_rebuild")?;

    conn.execute("DELETE FROM folder_similarity", [])?;

    let mut count = 0;
    for sim in similarities {
        conn.execute(
            "INSERT INTO folder_similarity
             (folder_a, folder_b, shared_files, shared_size, unique_a, unique_b, similarity_percent, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                sim.folder_a,
                sim.folder_b,
                sim.shared_files,
                sim.shared_size,
                sim.unique_a,
                sim.unique_b,
                sim.similarity_percent,
                now
            ],
        )?;
        count += 1;
    }

    sp.commit()?;
    Ok(count)
}
```

- [ ] **Step 4: Run the tests — expect PASS.** `cargo test -p athenaeum-core operations_blackhole`

- [ ] **Step 5: Commit** — `fix(db): similarity-cache rebuild computes outside the write lock and rolls back on error`

### Task 8: `bulk_update_frame_metadata` + `bulk_update_calibration_metadata` are atomic (audit I1)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (`bulk_update_frame_metadata`, write section lines ~1553-1627)
- Modify: `crates/athenaeum-core/src/api/calibration.rs` (`bulk_update_calibration_metadata`, per-set loop from line ~987)
- Test: `crates/athenaeum-core/src/db/operations.rs` test module

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn bulk_update_frame_metadata_nests_inside_an_outer_transaction() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format)
         VALUES ('/t/m.fits','m.fits',1,'2026-01-01T00:00:00Z','FITS')", []).unwrap();
    let file_id = conn.last_insert_rowid();
    conn.execute("INSERT INTO frames (file_id) VALUES (?1)", [file_id]).unwrap();
    let frame_id = conn.last_insert_rowid();

    let tx = conn.unchecked_transaction().unwrap();
    let edits = FrameMetadataEdits { object: Some(Some("M31".into())), ..Default::default() };
    bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();
    drop(tx); // rollback — the edit must vanish with it

    let obj: Option<String> = conn.query_row(
        "SELECT object FROM frames WHERE id = ?1", [frame_id], |r| r.get(0)).unwrap();
    assert_eq!(obj, None, "outer rollback must undo the savepointed edit");
}
```

(If `FrameMetadataEdits` lacks `Default`, construct it with every field `None`
explicitly — check the struct definition above `bulk_update_frame_metadata`
and mirror an existing test's construction.)

- [ ] **Step 2: Run it — expect FAIL or compile-fix, then FAIL on the assertion is NOT expected** — autocommit statements inside an open tx already join it, so the assertion may pass today. The test's job is pinning nesting safety across the refactor. If it passes pre-change, keep it and continue.

- [ ] **Step 3: Wrap the write section of `bulk_update_frame_metadata`.** Immediately before the `let count = conn.execute(&sql, …)` (line ~1559) insert:

```rust
    // One atomic unit: the frames UPDATE (override stamp included), the
    // three cascade DELETEs, and the empty-set prune. A failure partway
    // previously left edited frames with a partial subset of their stale
    // links — silently violating the "ANY edit unlinks" invariant above
    // (audit I1).
    let sp = SavepointGuard::new(conn, "bulk_update_frame_metadata")?;
```

and immediately after the `prune_orphaned_calibration_sets` call's closing
`.map_err(…)?;` (line ~1626, still inside the block), after the block's
closing brace insert:

```rust
    sp.commit()?;
```

`recompute_override_flag_for_frames` stays OUTSIDE the savepoint (its failure
is non-fatal by design — the existing `warn!` comment already says so).

- [ ] **Step 4: Wrap the per-set loop in `bulk_update_calibration_metadata`.** In `api/calibration.rs`, before `for set_id in &set_ids {` insert:

```rust
    // All-or-nothing across the whole selection: matches the existing
    // failure semantics (any per-set error fails the command) — previously
    // earlier sets stayed committed and the failing set stayed half-written
    // (audit I1).
    let sp = crate::db::SavepointGuard::new(&conn, "bulk_update_calibration_metadata")
        .map_err(|e| ApiError::Internal(e.to_string()))?;
```

and after the loop's closing brace (before the function's summary/return):

```rust
    sp.commit()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
```

- [ ] **Step 5: Run the tests.** `cargo test -p athenaeum-core operations && cargo test -p athenaeum-core calibration` — PASS.

- [ ] **Step 6: Commit** — `fix(db): bulk metadata edits are atomic with their relation cascades`

### Task 9: `sync_missing_files` moves to core with a savepoint (audit I2)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (new function, place near `reconcile_unique_camera_instrume`)
- Modify: `crates/athenaeum-tauri/src/commands/missing_files.rs:28-104`
- Modify: `crates/athenaeum-web/src/routes/missing_files.rs` (the `sync_missing_files` handler, ~lines 168-212)
- Test: `crates/athenaeum-core/src/db/operations.rs` test module

**Interfaces:**
- Produces: `pub fn sync_missing_files(conn: &Connection, root_id: i64, file_ids: &[i64]) -> rusqlite::Result<()>` in `athenaeum_core::db` — both command layers become 3-line wrappers.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn sync_missing_files_reconciles_and_preserves_ignored() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (7, '/t')", []).unwrap();
    for name in ["a", "b", "c"] {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 1, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![format!("/t/{name}.fits"), format!("{name}.fits")]).unwrap();
    }
    let ids: Vec<i64> = (1..=3).collect();

    // First sync: all three missing.
    sync_missing_files(&conn, 7, &ids).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM missing_files", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 3);

    // User ignores file 2; file 3 reappears on disk (drops out of the list).
    conn.execute("UPDATE missing_files SET status = 'ignored' WHERE file_id = 2", []).unwrap();
    sync_missing_files(&conn, 7, &[1]).unwrap();

    let still: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT file_id, status FROM missing_files ORDER BY file_id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    };
    // 1 stays missing, 2 stays ignored, 3 is gone.
    assert_eq!(still, vec![(1, "missing".to_string()), (2, "ignored".to_string())]);
}
```

- [ ] **Step 2: Run it — expect FAIL** with "function not found" (`cargo test -p athenaeum-core sync_missing_files_reconciles`).

- [ ] **Step 3: Add the core function** to `db/operations.rs`:

```rust
/// Reconcile the `missing_files` table with a scan's still-missing list.
///
/// Extracted from the (formerly duplicated) Tauri/Axum command bodies so the
/// multi-statement reconcile is atomic: any failure rolls the whole pass
/// back via the savepoint instead of leaking an open raw transaction onto
/// the pooled connection (2026-08-03 audit I2). Semantics unchanged:
/// files absent from `file_ids` lose their 'missing' rows ('ignored' rows
/// are kept), present files get last_checked_at bumped or a fresh row.
pub fn sync_missing_files(conn: &Connection, root_id: i64, file_ids: &[i64]) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let sp = SavepointGuard::new(conn, "sync_missing_files")?;

    if file_ids.is_empty() {
        conn.execute(
            "DELETE FROM missing_files WHERE scan_root_id = ?1 AND status = 'missing'",
            [root_id],
        )?;
    } else {
        let placeholders: String =
            file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let delete_sql = format!(
            "DELETE FROM missing_files
             WHERE scan_root_id = ?1 AND status = 'missing' AND file_id NOT IN ({placeholders})"
        );
        let mut params: Vec<rusqlite::types::Value> = vec![root_id.into()];
        for id in file_ids {
            params.push((*id).into());
        }
        conn.execute(&delete_sql, rusqlite::params_from_iter(params))?;

        for file_id in file_ids {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM missing_files WHERE file_id = ?1",
                    [file_id],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if exists {
                conn.execute(
                    "UPDATE missing_files SET last_checked_at = ?1 WHERE file_id = ?2",
                    rusqlite::params![&now, file_id],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at, status)
                     VALUES (?1, ?2, ?3, ?4, 'missing')",
                    rusqlite::params![file_id, root_id, &now, &now],
                )?;
            }
        }
    }

    sp.commit()?;
    Ok(())
}
```

- [ ] **Step 4: Run the test — expect PASS.**

- [ ] **Step 5: Shrink both command bodies to wrappers.** Tauri (`commands/missing_files.rs:32-104` body becomes):

```rust
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::db::sync_missing_files(&conn, root_id, &file_ids).map_err(|e| e.to_string())
```

Axum mirror (`routes/missing_files.rs`, same handler): identical call with the
route's existing arg struct and `.map_err(db_err)?` / `Ok(Json(()))` shape —
delete the duplicated raw SQL from both files.

- [ ] **Step 6: Build both backends.** `cargo build -p athenaeum-tauri -p athenaeum-web` — PASS.

- [ ] **Step 7: Commit** — `fix(missing-files): sync moves to core behind a savepoint; both backends wrap it`

### Task 10: Plate-solve + analysis persist phases use Drop-safe transactions, unified across backends (audit I12, M11)

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/plate_solve.rs:438-485` (persist phase)
- Modify: `crates/athenaeum-web/src/routes/plate_solve.rs:345-399` (persist phase)
- Modify: `crates/athenaeum-core/src/api/analysis.rs:355-370`

- [ ] **Step 1: Tauri persist phase.** Replace `conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;` (line 441) with:

```rust
        // Drop-safe: an early return or panic rolls back instead of leaking
        // an open transaction onto the pooled connection. A failed BEGIN
        // falls back to per-row autocommit — losing atomicity, never the
        // batch's computed results (audit I12: the old `?` discarded every
        // solve result on a BEGIN failure).
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => Some(tx),
            Err(e) => {
                tracing::error!(error = %e, "plate solve persist: BEGIN failed; persisting per-row");
                None
            }
        };
```

and replace `conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;` (line 484) with:

```rust
        if let Some(tx) = tx {
            tx.commit().map_err(|e| {
                tracing::error!(error = %e, "plate solve persist: COMMIT failed; batch results rolled back");
                e.to_string()
            })?;
        }
```

(the loop body between them is unchanged — its statements run on `conn` and
join the transaction when one is open).

- [ ] **Step 2: Web persist phase.** Same replacement shape in
`routes/plate_solve.rs` (the `if let Err(e) = conn.execute_batch("BEGIN")` /
`COMMIT` pair at ~349 and ~397): `unchecked_transaction()` with the same
fallback log, and on commit failure `tracing::error!` (the web closure has no
`?` to a response — log is the correct terminal there; the Drop rollback
prevents the old open-transaction leak). Both backends now behave
identically: atomic when possible, per-row when BEGIN fails, never a leak.

- [ ] **Step 3: Analysis persist.** In `api/analysis.rs`, replace lines 355-369 with:

```rust
        if !analyses.is_empty() {
            // Drop-rollback replaces the manual ROLLBACK-in-map_err dance and
            // also covers the previously-unguarded COMMIT failure (audit M11).
            let tx = conn.unchecked_transaction()?;
            for a in &analyses {
                let analysis_id = db_analysis::upsert_frame_analysis(&conn, a)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                if let Some(stars) = stars_by_frame.get(&a.frame_id) {
                    db_analysis::upsert_star_metrics(&conn, analysis_id, stars)
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                }
            }
            tx.commit()?;
        }
```

- [ ] **Step 4: Build + targeted tests.** `cargo build -p athenaeum-tauri -p athenaeum-web && cargo test -p athenaeum-core analysis` — PASS.

- [ ] **Step 5: Commit** — `fix(plate-solve,analysis): Drop-safe persist transactions, identical behavior on both backends`

---

## Phase D — Panic & async hygiene

### Task 11: Web plate-solve workers catch panics and always clean up (audit C6-web)

**Files:**
- Modify: `crates/athenaeum-web/src/routes/plate_solve.rs:222-417` (`plate_solve_batch` worker closure), `:526-597` (`autofind_objects_from_coordinates` worker closure)

**Interfaces:**
- Produces: on ANY panic inside either worker, an `error!` is logged, the `active_plate_solves` handle (key 0 / key 1) is removed, and the terminal SSE event (`plate-solve-complete` / `autofind-objects-complete`) still fires — the frontend progress UI can never hang on a panic. Mirrors the Tauri twin's panic safety.

- [ ] **Step 1: Wrap the `plate_solve_batch` closure body.** Restructure the `tokio::task::spawn_blocking(move || { … })` at line 222 to:

```rust
    tokio::task::spawn_blocking(move || {
        // Fire-and-forget by design (progress rides SSE) — which is exactly
        // why a panic must be caught here: a dropped JoinHandle swallows it,
        // leaving no log, no completion event, and a stuck cancel handle
        // (audit C6). The Tauri twin awaits its join; this shell is the
        // web-side equivalent.
        let cleanup_ctx = ctx.clone();
        let cleanup_tx = event_tx.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // ── entire existing body, unchanged (lines 223-417) ──
        }));
        if let Err(panic) = outcome {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            tracing::error!(panic = %msg, "plate solve batch worker panicked");
            cleanup_ctx.active_plate_solves.lock().unwrap().remove(&0);
            let _ = cleanup_tx.send(SseEvent {
                event_name: "plate-solve-complete".into(),
                data: serde_json::to_value(PlateSolveCompleteEvent {
                    solved: 0,
                    failed: total,
                    total,
                    total_time_ms: 0,
                })
                .unwrap_or_default(),
            });
        }
    });
```

Notes: `total` is `Copy` and already computed before the spawn; `ctx` and
`event_tx` are already moved into the outer closure — the inner closure
borrows them, the cleanup arm uses the pre-made clones. If the borrow checker
objects to a specific capture, clone that value before the `catch_unwind` the
same way.

- [ ] **Step 2: Same shell for the autofind worker** (closure at line 526): cleanup arm removes key `1` and emits:

```rust
            let _ = cleanup_tx.send(SseEvent {
                event_name: "autofind-objects-complete".into(),
                data: serde_json::to_value(AutofindCompleteEvent {
                    total: 0,
                    labeled: 0,
                    no_match: 0,
                    already_labeled: 0,
                    missing_coords: 0,
                    errors: 0,
                    cancelled: false,
                    total_time_ms: 0,
                })
                .unwrap_or_default(),
            });
```

with `tracing::error!(panic = %msg, "autofind batch worker panicked");`. The
in-body `ctx.db.get().expect("DB not initialized")` at line 527 is now caught
by this shell — leave it (a missing DB at this point is a startup bug worth a
loud panic log).

- [ ] **Step 3: Build.** `cargo build -p athenaeum-web` — PASS.

- [ ] **Step 4: Commit** — `fix(web): plate-solve/autofind workers catch panics; completion events and cancel handles always release`

### Task 12: Tauri autofind runs on `spawn_blocking` (audit C6-tauri)

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/plate_solve.rs:636-654` (the inline compute block of `autofind_objects_from_coordinates`)

- [ ] **Step 1: Move the batch off the async runtime.** Replace lines 636-654 with:

```rust
    let start = std::time::Instant::now();
    // The whole batch is blocking work (per-frame DB reads + coordinate
    // math). Running it inline starved a tokio worker thread for the
    // batch's full duration while holding a pooled connection (audit C6);
    // the Axum mirror already used spawn_blocking. A JoinError (panic) is
    // surfaced, and the cancel handle is removed on every path.
    let db_worker = db.clone();
    let join = tokio::task::spawn_blocking(move || {
        let conn = db_worker.conn();
        object_fill::autofind_objects_from_coordinates(
            &conn,
            &dso,
            &frame_ids,
            tolerance_deg,
            cancel_flag,
            &progress,
        )
    })
    .await;

    {
        let mut handles = state.ctx.active_plate_solves.lock().unwrap();
        handles.remove(&1);
    }

    let summary = match join {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(join_err) => return Err(format!("autofind batch panicked: {join_err}")),
    };
```

(`db` is the `Database` handle obtained at line 597 — `Database` is `Clone`;
`dso`, `frame_ids`, `tolerance_deg`, `cancel_flag`, `progress` are already
owned values that move into the closure. The existing handle-removal block at
lines 649-652 is replaced by the one above; the `summary_result.map_err` line
654 is replaced by the `match`. The completion emit below stays unchanged.)

- [ ] **Step 2: Build.** `cargo build -p athenaeum-tauri` — PASS.

- [ ] **Step 3: Commit** — `fix(tauri): autofind batch runs on spawn_blocking; panic surfaces, handle always released`

### Task 13: Defensive timestamp parsing in file listings (audit I6)

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (add helper near the top; replace 14 unwrap sites), `crates/athenaeum-core/src/db/equipment.rs:395-405`
- Test: `crates/athenaeum-core/src/db/operations.rs` test module

**Interfaces:**
- Produces: `pub(crate) fn parse_stored_ts(field: &'static str, raw: &str) -> DateTime<Utc>` in `db::operations`, reachable as `crate::db::parse_stored_ts`.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn malformed_stored_timestamp_does_not_panic_reads() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at)
         VALUES ('/t/x.fits','x.fits',1,'not-a-timestamp','FITS','also-bad')", []).unwrap();
    // Previously: thread panic on DateTime::parse_from_rfc3339(...).unwrap().
    let f = get_file_by_path(&conn, "/t/x.fits").unwrap();
    assert!(f.is_some());
}
```

(match `get_file_by_path`'s actual return shape — adjust the final two lines
to its signature if it returns `Result<File>`.)

- [ ] **Step 2: Run it — expect PANIC/FAIL.** `cargo test -p athenaeum-core malformed_stored_timestamp`

- [ ] **Step 3: Add the helper** to `db/operations.rs` (near `SavepointGuard`):

```rust
/// Parse a stored RFC3339 timestamp defensively: a malformed value logs a
/// warning and falls back to the UNIX epoch instead of panicking every read
/// of that row (2026-08-03 audit I6 — `frames.date_obs` was already parsed
/// defensively; `files.modified_at`/`created_at` were not).
pub(crate) fn parse_stored_ts(field: &'static str, raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|e| {
            tracing::warn!(field, raw, error = %e, "malformed stored timestamp; substituting epoch");
            DateTime::<Utc>::UNIX_EPOCH
        })
}
```

- [ ] **Step 4: Replace every unwrap site.** `grep -n "parse_from_rfc3339" crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/db/equipment.rs` — at each hit that ends in `.unwrap().with_timezone(&Utc)` (audit list: operations.rs 759, 767, 812, 820, 932, 940, 1052, 1060, 1147-1149, 1155-1157, 2680-2682, 2688-2690, 2836-2838, 2844-2846; equipment.rs 395-397, 403-405), replace the pattern

```rust
                modified_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&Utc),
```

with

```rust
                modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
```

(field name `"files.created_at"` for created_at columns; column index per
site; in `equipment.rs` call it as `crate::db::parse_stored_ts`). Defensive
sites that already use `.ok()`/`unwrap_or` (e.g. `date_obs`) are NOT touched.
After the pass, `grep -n "parse_from_rfc3339" … | grep unwrap` over both
files must return only hits inside `#[cfg(test)]`.

- [ ] **Step 5: Run — expect PASS.** `cargo test -p athenaeum-core operations && cargo test -p athenaeum-core equipment`

- [ ] **Step 6: Commit** — `fix(db): malformed stored timestamps degrade to epoch+warn instead of panicking reads`

---

## Phase E — Query correctness + guards

### Task 14: `insert_calibration_link` cannot clobber a manual override, atomically (audit I8)

**Files:**
- Modify: `crates/athenaeum-core/src/db/calibration_links.rs:53-63`
- Test: same file's test module

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn auto_upsert_cannot_clobber_manual_override_even_without_precheck() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute("INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark','2026-01-01')", []).unwrap();
    let set_a = conn.last_insert_rowid();
    conn.execute("INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark','2026-01-02')", []).unwrap();
    let set_b = conn.last_insert_rowid();

    // Manual link exists.
    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type, is_manual_override)
         VALUES (42, 'frame', ?1, 'dark', 1)", [set_a]).unwrap();

    // Simulate the audit-I8 race: the auto-find upsert fires AFTER the
    // manual write, without its SELECT pre-check seeing it. Run the raw
    // upsert exactly as insert_calibration_link builds it for an auto link.
    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type, matched_at, match_score, date_warning, temp_warning, is_manual_override)
         VALUES (42, 'frame', ?1, 'dark', '2026-08-03T00:00:00Z', 0.9, 0, 0, 0)
         ON CONFLICT(source_id, source_type, calibration_type) DO UPDATE SET
         calibration_set_id = excluded.calibration_set_id,
         match_score = excluded.match_score,
         date_warning = excluded.date_warning,
         temp_warning = excluded.temp_warning,
         matched_at = excluded.matched_at,
         is_manual_override = excluded.is_manual_override
         WHERE excluded.is_manual_override = 1
            OR calibration_set_to_frames.is_manual_override = 0",
        [set_b]).unwrap();

    let (linked, manual): (i64, i64) = conn.query_row(
        "SELECT calibration_set_id, is_manual_override FROM calibration_set_to_frames
         WHERE source_id = 42 AND source_type = 'frame' AND calibration_type = 'dark'",
        [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((linked, manual), (set_a, 1), "manual pick must survive the auto upsert");
}
```

- [ ] **Step 2: Run it — expect PASS** (it exercises the NEW SQL directly; it fails only if the guard syntax is wrong). Then update the production statement.

- [ ] **Step 3: Add the same `WHERE` guard to the production upsert** in `insert_calibration_link` (after line 63, `is_manual_override = excluded.is_manual_override`):

```rust
         is_manual_override = excluded.is_manual_override
         WHERE excluded.is_manual_override = 1
            OR calibration_set_to_frames.is_manual_override = 0",
```

and extend the function's doc comment: the SELECT pre-check (lines 34-51)
remains as the fast path + debug log; the `DO UPDATE … WHERE` is the atomic
guarantee against the check-then-act race (audit I8).

- [ ] **Step 4: Run the module tests.** `cargo test -p athenaeum-core calibration_links` — PASS (existing manual-assignment tests must be unaffected: a manual link has `excluded.is_manual_override = 1`, which the guard always lets through).

- [ ] **Step 5: Commit** — `fix(calibration): manual-override guard is enforced inside the upsert, not just the pre-check`

### Task 15: Equipment library queries return the first member's metadata deterministically (audit I7)

**Files:**
- Modify: `crates/athenaeum-core/src/db/equipment.rs:71-78` (raw library), `:~173-180` (master dark library), `:~278-285` (master flat library)
- Test: same file's test module

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn library_metadata_comes_from_the_lowest_frame_id() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO calibration_set (imagetyp, date, instrume, date_start, date_end)
         VALUES ('Dark','2026-01-01','CamX','2026-01-01T00:00:00Z','2026-01-01T01:00:00Z')", []).unwrap();
    let set_id = conn.last_insert_rowid();
    for (name, naxis1) in [("d1", 6000_i64), ("d2", 9999_i64)] {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 1, '2026-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![format!("/t/{name}.fits"), format!("{name}.fits")]).unwrap();
        let fid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, naxis1, instrume) VALUES (?1, ?2, 'CamX')",
            rusqlite::params![fid, naxis1]).unwrap();
        let frid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frid]).unwrap();
    }
    let sets = get_camera_dark_library(&conn, "CamX").unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].naxis1, Some(6000), "must be the FIRST member (lowest frame id), not an arbitrary one");
}
```

- [ ] **Step 2: Run it** (`cargo test -p athenaeum-core library_metadata_comes_from`) — under the current bare-column GROUP BY it may pass by luck; treat a pass as unpinned behavior and proceed (the point is pinning it).

- [ ] **Step 3: Replace the join + GROUP BY in all three queries.** In each of `get_camera_dark_library`, `get_camera_master_dark_library`, `get_camera_master_flat_library`, replace

```sql
        FROM calibration_set cs
        LEFT JOIN calibration_set_frames csf ON csf.set_id = cs.id
        LEFT JOIN frames f ON f.id = csf.frame_id
        LEFT JOIN files fi ON fi.id = f.file_id
```

with

```sql
        FROM calibration_set cs
        LEFT JOIN (
            SELECT set_id, MIN(frame_id) AS frame_id
            FROM calibration_set_frames
            GROUP BY set_id
        ) first_member ON first_member.set_id = cs.id
        LEFT JOIN frames f ON f.id = first_member.frame_id
        LEFT JOIN files fi ON fi.id = f.file_id
```

and delete the `GROUP BY cs.id` line from each (the joined subquery is 1:1
per set, so grouping is no longer needed; SELECT lists and ORDER BY stay).
Update each query's "first frame in each set" comment to say "first member =
lowest frame id, deterministic (audit I7 — the old bare-column GROUP BY
returned an arbitrary member)".

- [ ] **Step 4: Run — expect PASS.** `cargo test -p athenaeum-core equipment`

- [ ] **Step 5: Commit** — `fix(equipment): library set metadata reads the lowest-id member deterministically`

### Task 16: Minors batch — explicit FK pragma, migration pragma restore, savepoints for the small writers (audit M1, M2, M4, M5, M7 + hardening)

**Files:**
- Modify: `crates/athenaeum-core/src/db/mod.rs:40-47` (setup_connection)
- Modify: `crates/athenaeum-core/src/db/schema.rs:1573-1641` and `:1663-1754` (both migration error paths)
- Modify: `crates/athenaeum-core/src/db/operations.rs` (`deduplicate_session_members_in_set` ~2424-2574, `insert_excluded_frames` ~2991-3001)
- Modify: `crates/athenaeum-core/src/calibration/scan_integration.rs:884-962` (`create_master_sets_from_frames`)
- Modify: `crates/athenaeum-core/src/archive/planner.rs:467-533` (`commit_plan`)

- [ ] **Step 1: Explicit FK pragma.** In `setup_connection`, add as the FIRST pragma line:

```rust
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 268435456;",
        )?;
```

with a comment above the batch: `// foreign_keys is already the bundled
build's compile-time default; stating it makes enforcement survive a switch
to a system SQLite or a build-flag change.`

- [ ] **Step 2: Migration pragma restore (M7).** In BOTH rebuild blocks
(schema.rs — the batch result handling around lines 1614 and 1726), change
the sequence to roll back the leaked transaction BEFORE re-enabling:

```rust
        // A failed statement inside the batch leaves its explicit BEGIN
        // open; `PRAGMA foreign_keys` is a silent no-op while a transaction
        // is pending, so roll back FIRST or the re-enable below would do
        // nothing (audit M7).
        if batch.is_err() && !conn.is_autocommit() {
            if let Err(e) = conn.execute("ROLLBACK", []) {
                tracing::error!(error = %e, "migration batch rollback failed");
            }
        }
        let re_enable = conn.pragma_update(None, "foreign_keys", true);
        batch?;
        re_enable?;
```

(where `batch` is the existing `let batch = conn.execute_batch(…)` /
equivalent binding at each site — adjust the local variable name to what's
there; do not reorder anything else).

- [ ] **Step 3: `deduplicate_session_members_in_set` savepoint (M1).** Wrap the
three phases: `let sp = SavepointGuard::new(conn, "dedup_session_members")?;`
before Phase 1's first statement, `sp.commit()?;` after Phase 3's last, no
other changes.

- [ ] **Step 4: `insert_excluded_frames` nesting guard (M2).** Replace the
unconditional `let tx = conn.unchecked_transaction()?;` … `tx.commit()?;`
with the sibling pattern (`insert_session_members`, operations.rs:2328-2351):

```rust
    let tx = if conn.is_autocommit() {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    // …existing body unchanged…
    if let Some(tx) = tx {
        tx.commit()?;
    }
```

- [ ] **Step 5: `create_master_sets_from_frames` per-frame savepoint (M4).**
Wrap the two INSERTs (calibration_set at line 926 + calibration_set_frames at
line 952) per loop iteration:

```rust
        let sp = crate::db::SavepointGuard::new(conn, "create_master_set")?;
        conn.execute(/* INSERT INTO calibration_set … unchanged */)?;
        let set_id = conn.last_insert_rowid();
        conn.execute(/* INSERT INTO calibration_set_frames … unchanged */)?;
        sp.commit()?;
        sets_created += 1;
```

(nests fine both under `register_master`'s transaction and in the scanner's
post-commit autocommit context.)

- [ ] **Step 6: `commit_plan` savepoint (M5).** Wrap the operation-row insert +
per-file INSERT loop: `let sp = crate::db::SavepointGuard::new(conn,
"archive_commit_plan")?;` at the top of the write section, `sp.commit()?;`
before returning the operation id.

- [ ] **Step 7: Run the touched modules.** `cargo test -p athenaeum-core operations && cargo test -p athenaeum-core schema && cargo test -p athenaeum-core scan_integration && cargo test -p athenaeum-core planner` — PASS.

- [ ] **Step 8: Commit** — `fix(db): explicit FK pragma, migration pragma restore, savepoints for the remaining small writers`

---

## Final gates (before merge)

- [ ] `cargo build --workspace` — clean.
- [ ] `cargo test -p athenaeum-core` — all green.
- [ ] `cargo test -p athenaeum-tauri && cargo test -p athenaeum-web` (if test targets exist) — green.
- [ ] `npx tsc --noEmit` — clean (no TS changes expected; gate anyway).
- [ ] `rustfmt` every touched file (listed per task), not `cargo fmt -p`.
- [ ] Grep sweeps: `grep -rn "let _ = insert_fits_header" crates/` → 0 hits; `grep -rn "execute(\"BEGIN" crates/athenaeum-core/src crates/athenaeum-tauri/src crates/athenaeum-web/src` → only the scanner batch transaction (`scanner/mod.rs:1923`, deliberately kept: its cancel/commit-failure rollback discipline is correct and pre-audited) and `schema.rs` init-time batches remain.
- [ ] Whole-branch review (superpowers:requesting-code-review) with special attention to the plan-verbatim seams: SavepointGuard visibility from sibling modules, the scanner counter reordering in Task 2, and the web catch_unwind capture set in Task 11.

## Owner smoke list (post-merge, real data)

1. Black Hole: void a master file → raw set un-superseded, no zombie set rows, file gone; void a file on a read-only volume → error surfaced, catalog rows gone, file remains (documented benign leftover).
2. Scan a root, hand-delete one `frames` row via sqlite3, rescan → frame reappears with the same `files.id`.
3. Web build: `POST /api/get_black_hole_files` with `{"filter": "x' UNION SELECT …"}` → empty list, no 500.
4. Web build: start a plate-solve batch, kill the DSO catalog file mid-run (or otherwise force a panic) → error in log, completion event fires, UI does not hang.
5. Bulk metadata edit across ~50 frames with calibration links → all links dropped together; Equipment page set metadata stable across refreshes on a mixed-geometry set.
