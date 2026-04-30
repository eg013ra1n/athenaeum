# Restore mtime sync + non-destructive scanner re-parse

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the data-loss bug where archive→restore→monitor scan silently strips a frame set's `session_members`. Replace the destructive scanner "modified file" branch with an in-place UPDATE that preserves `frames.id` (and therefore every junction-table linkage), and make restore reconcile the catalog's `modified_at`/`size` with the freshly-extracted file so the scanner's mtime check never trips on a healthy round-trip.

**Architecture:** Two complementary layers of defense:

1. **Restore-side sync (stop the bleeding).** After every successful copy in `restore::run_restore_inner`, stat the destination and pass the new `modified_at` + `size` into an extended `unmark_file_archived`. Also reconcile on the "skip / already on disk" branch when the on-disk metadata drifted from the catalog.
2. **Scanner-side non-destructive UPDATE (close the hole permanently).** Replace `DELETE FROM files WHERE path = ?` + re-INSERT with an in-place UPDATE of `files`/`frames`/`fits_header` that preserves `files.id` and `frames.id`. CASCADE never fires on the junction tables, so `session_members`, `calibration_set_frames`, `calibration_set_to_frames`, `frame_tags` survive any in-place modification (including legitimate user edits, not just our archive→restore round-trip).

Layer 1 fixes the specific reproduction path observed (M33 emptied after 8 archive cycles). Layer 2 also defends against future sources of mtime drift (Time Machine, rsync, cloud sync, user editing FITS headers, network-share clock skew). Together they make the catalog impossible to corrupt via on-disk metadata alone.

**Tech Stack:** Rust, rusqlite, anyhow, chrono. No new crate dependencies. No schema migration.

---

## Solution evaluation (why this design)

| Option | What it does | Verdict |
| ------ | ------------ | ------- |
| **A. Restore syncs DB only.** Update `files.modified_at`/`size` after copy. | Fixes the symptomatic path. Smallest diff. | **Insufficient alone** — leaves the destructive scanner branch intact. Any other source of mtime drift (user edit, rsync, FS clock skew) reproduces the same data loss. |
| **B. Preserve mtime via zip metadata + filetime crate.** Read `last_modified()` from the zip entry, apply to the extracted file. | Files on disk look exactly as they were. | **Insufficient alone** — zip's DOS timestamp has 2-second resolution; RFC3339 in DB has sub-second. Rounding will still produce mtime drift. Also adds a dep. |
| **C. Store original mtime/size in `archive_operation_files`.** Schema change, restore reads original mtime, applies via `filetime`. | Faithful round-trip. | **Overkill for this fix** — requires schema migration. Useful as a future enhancement but not load-bearing. |
| **D. Scanner UPDATE-in-place.** Replace DELETE+INSERT with UPDATE preserving `files.id` + `frames.id`. | Defends against ALL sources of mtime drift, not just archive. | **Necessary.** Without this, even legitimate user FITS edits silently strip session linkage. |

**Chosen: A + D.** A stops the immediate reproduction; D makes the catalog robust against any future drift source. Each is independently testable; either alone prevents the reported bug, but both together close the architectural hole.

C is left as an explicit follow-up (not in this plan) — once A+D ship, restore round-trip is invisible from the catalog's POV; preserving the on-disk mtime byte-for-byte is cosmetic.

## Edge cases this design has to handle

These are the reasons this looked simple at first and isn't:

1. **Cancellation mid-restore.** Restore can be cancelled between copy and DB sync. → Sync immediately after each successful copy, not in a batch at the end. Each row that was successfully copied is also DB-synced before moving to the next.
2. **Restore-skip when DB drifted.** File already on disk → no copy → today no DB touch. But DB might be stale from a prior cycle. → On skip, stat-and-update if drifted. Idempotent: cheap when DB already matches, corrective when it doesn't.
3. **Restore to alternate location.** `dest != source_path`. → Existing path-rewrite branch in `unmark_file_archived` already handles path; we add `modified_at`+`size` to the same UPDATE so all four columns move atomically.
4. **`std::fs::metadata` fails after copy** (race with antivirus, snapshot, Time Machine). → Treat as fatal for this file; don't silently continue with a stale DB row. Surface as restore error.
5. **Network filesystems with mtime clock skew.** Mac → SMB → NAS often produces sub-second drift. → Always use the *destination*'s mtime (what the OS reports after the copy), never assume "source mtime preserved". This is what stat-after-copy gets us.
6. **`process_file_parallel` is called on a freshly-restored file by an in-flight scan.** Concurrent restore + scan. → Scan sees mid-copy file, parse fails, error logged, no DB destruction (because Layer D is now non-destructive). Best-effort recovery. Filed as a known minor: out of scope for this plan; tracked separately.
7. **Legitimate in-place FITS edits.** User opens a FITS in another tool, adds a HISTORY record, saves. Today: scanner wipes session linkage. → Layer D treats this as "re-parse and update", preserves frame.id and session_members. Frame data updates to reflect new header.
8. **Modified file fails to re-parse** (corrupted by user). Today: row already DELETEd, INSERT fails, file is gone from catalog. → Layer D parses *first*, only writes UPDATE on success. On parse failure: log, skip, leave row alone (data may be stale, but it isn't lost).
9. **IMAGETYP changes between scans** (user re-tags a Light as a Dark). The frame is currently in a `session_members` row that conceptually only makes sense for Lights. → Out of scope. The UPDATE writes the new IMAGETYP; cleanup of stale session/calibration linkages is a separate concern. Document the limitation.
10. **`metadata_hash` UNIQUE collision.** If two files now hash-collide because their `(size, modified_at, filename)` happen to match. → Today same risk applies to INSERT; Layer D's UPDATE would also fail. Surface error; don't silently corrupt.
11. **`fits_header` UNIQUE(file_id) constraint.** Schema has `fits_header.file_id INTEGER NOT NULL UNIQUE` — DELETE-then-INSERT works; UPDATE-by-file-id also works. Either is fine.
12. **`frames.file_id` is NOT UNIQUE in the schema.** In practice, exactly one frame per file. UPDATE-by-file-id assumes 1:1; if somehow there are two frames sharing a file_id we'd update both. Add a defensive guard (count rows; fail loud if >1).
13. **Both scanner paths (serial `scan_directory` and parallel `scan_directory_parallel`) have the same bug** at lines 152–189 and 779–840 respectively. Both must be fixed. The parallel path also has a downstream "moved file" branch at line 1009–1048 that does its own UPDATE — that path is fine, no change needed there.
14. **Repeating scans must be idempotent.** After Layer D ships, an unchanged file is detected by the existing fast-path skip (size+mtime match) and returns immediately. No churn.
15. **Master frames** have `is_master = 1` and live in `calibration_set_frames`. Those stay valid because we preserve `frames.id`. ✅
16. **Restore that ends in 100% skip** (every file already on disk, e.g., user manually copied them back). The `archive_operation_files` rows still need their archive markers cleared. The existing "leftover archive markers" loop at `restore.rs:340-347` already handles this — keep it. Add the optional drift-correction sync there too.
17. **Existing test suite** expects `unmark_file_archived(conn, file_id, Option<&str>)`. New signature must keep backward compatibility (callers that don't have new mtime/size pass `None`).

## File structure

This plan touches three crates worth of code, all backend Rust. No frontend changes. No schema changes. No new Cargo deps.

**Modify:**

- `crates/athenaeum-core/src/archive/db.rs`
  - Extend `unmark_file_archived` to accept optional `new_modified_at: Option<&str>` and `new_size: Option<i64>`. Build the UPDATE statement dynamically based on which optional inputs are `Some`. Update existing tests.
- `crates/athenaeum-core/src/archive/restore.rs`
  - In `run_restore_inner`, after each successful `std::fs::copy`, stat the destination and capture `modified_at` (RFC3339) + `size`. Pass these into `unmark_file_archived`.
  - On the skip branch, if the on-disk file's metadata differs from the DB row, also call `unmark_file_archived` with the corrected values. This is the drift-recovery path for files that were skipped because they were already on disk.
  - Add a new test asserting that after restore, every catalog row's `modified_at`/`size` matches the on-disk file's `modified_at`/`size`.
- `crates/athenaeum-core/src/scanner/mod.rs`
  - Replace the `DELETE FROM files WHERE path = ?1` block in `scan_directory` (≈L182) with a non-destructive UPDATE path: re-parse → UPDATE files → UPDATE frames by file_id → DELETE+INSERT fits_header.
  - Replace the same destructive block in `scan_directory_parallel`'s purge phase (≈L815-840). Because parallel uses a two-phase architecture (parse-in-parallel → insert-sequentially), the cleanest fix is: don't purge in the classification phase at all. Instead, classify each file as `New` or `Modified`. In the sequential write phase, branch on the classification: `New` → existing INSERT path; `Modified` → UPDATE-in-place path.
  - Add unit tests for both the serial and parallel UPDATE-in-place paths covering: (a) plain mtime drift with no other changes, (b) FITS header changed (e.g., new HISTORY record → new fingerprint), (c) parse failure leaves row untouched, (d) `session_members` row survives the update.

**Read-only references** (no edits, but worth opening while implementing):

- `crates/athenaeum-core/src/db/schema.rs` — confirm `frames.file_id` is `NOT NULL` (no UNIQUE), `fits_header.file_id` is `UNIQUE`, `session_members.frame_id` cascades on `DELETE frames`.
- `crates/athenaeum-core/src/db/operations.rs::insert_file` / `insert_frame` / `insert_fits_header` — for the column lists Layer D's UPDATE statements need to mirror.
- `crates/athenaeum-core/src/models.rs` — `File` and `Frame` struct field names.
- `crates/athenaeum-core/src/archive/models.rs::ArchiveOperationFile` — `file_id`, `source_path` shape used by the restore sync.

---

## Task 0: Worktree setup

**Files:** None.

- [ ] **Step 1: Create the worktree**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
git fetch origin
git worktree add ../athenaeum-restore-mtime -b fix/restore-mtime-and-scanner-update origin/main
cd ../athenaeum-restore-mtime
```

Expected: new worktree at `../athenaeum-restore-mtime` on a fresh `fix/restore-mtime-and-scanner-update` branch off `origin/main`.

- [ ] **Step 2: Verify clean baseline**

```bash
cargo check -p athenaeum-core
```

Expected: builds cleanly (warnings OK, no errors).

---

## Task 1: Failing integration test that reproduces the M33 bug

This test pins down the exact path that destroyed M33 today. It must fail on `main` and pass after the plan is complete.

**Files:**
- Modify: `crates/athenaeum-core/src/archive/restore.rs` (add a new `#[test]` to the existing `mod tests` at the bottom of the file).

- [ ] **Step 1: Write the failing test**

Add this at the end of the existing `mod tests` in `restore.rs`:

```rust
/// Reproduces the M33 data-loss bug: archive (move) → restore → monitor scan
/// must NOT remove the frame from its session. Today the scanner sees the
/// freshly-extracted file's mtime != DB.modified_at and DELETEs the files
/// row, cascading through frames → session_members. After this plan is
/// implemented, the scanner re-parses in place and session_members survives.
#[test]
fn archive_then_restore_then_scan_preserves_session_members() {
    use crate::scanner::scan_directory_parallel;
    use crate::events::NullEmitter;
    use std::sync::atomic::AtomicBool;

    let arch = TempDir::new().unwrap();
    let scan = TempDir::new().unwrap();

    // Use a real FITS file so process_file_parallel can parse it. The
    // simplest synthetic FITS that fitsio accepts is created by writing a
    // minimal SIMPLE/BITPIX/NAXIS=0/END header.
    let l1 = scan.path().join("M33/L_001.fits");
    std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
    write_minimal_fits(&l1);

    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
        [scan.path().to_str().unwrap()]).unwrap();
    conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M33', 1)", []).unwrap();
    conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
         VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();

    let l1_size = std::fs::metadata(&l1).unwrap().len() as i64;
    let l1_mtime = chrono::DateTime::<chrono::Utc>::from(
        std::fs::metadata(&l1).unwrap().modified().unwrap()
    ).to_rfc3339();
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format)
         VALUES (1000, ?1, 'L_001.fits', ?2, ?3, 'FITS')",
        params![l1.to_str().unwrap(), l1_size, l1_mtime],
    ).unwrap();
    conn.execute(
        "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
         VALUES (10000, 1000, 'M33', 'T', 'C', 'Light')",
        [],
    ).unwrap();
    conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

    // Archive (move).
    let plan = build_plan(
        &conn, 1, arch.path(),
        &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
        ArchiveCompression::Store,
    ).unwrap();
    let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
    assert!(!l1.exists(), "source should have been moved into the zip");

    // Restore.
    run_restore(
        &conn, op_id, scan.path(),
        false, false, &cancel, &NullEmitter,
    ).unwrap();
    assert!(l1.exists(), "file should have been restored");

    // The whole point of the test: a monitor scan after restore must NOT
    // strip the frame's reachability through session_members. Production runs
    // with `PRAGMA foreign_keys = 0` (verified — there's no `PRAGMA
    // foreign_keys = ON` anywhere in the codebase), so the user-visible
    // failure isn't CASCADE — it's orphaning: scanner DELETEs the `files`
    // row, the `frames` row is now dangling, scanner re-INSERTs both with a
    // fresh `files.id`, and the original `frames.id` (still pointed at by
    // session_members) now joins to a non-existent `files.id`. Express the
    // assertion as the JOIN chain the UI actually relies on so it fails for
    // both the orphaning path (production) and the CASCADE path (if FKs ever
    // get turned on).
    let cancel2 = Arc::new(AtomicBool::new(false));
    let _ = scan_directory_parallel(
        scan.path(), 1, &conn, &NullEmitter,
        false, cancel2, false,
    );

    let joined_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM session_members sm
         JOIN frames f ON f.id = sm.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE sm.session_id = 100 AND fi.path = ?1",
        [l1.to_str().unwrap()], |r| r.get(0),
    ).unwrap();
    assert_eq!(
        joined_count, 1,
        "frame must remain reachable through session_members → frames → files \
         JOIN after restore + scan",
    );

    // The catalog's view of (size, modified_at) must match disk so the next
    // scan classifies the file as unchanged.
    let (db_size, db_mtime): (i64, String) = conn.query_row(
        "SELECT size, modified_at FROM files WHERE path = ?1",
        [l1.to_str().unwrap()], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    let on_disk_size = std::fs::metadata(&l1).unwrap().len() as i64;
    let on_disk_mtime = chrono::DateTime::<chrono::Utc>::from(
        std::fs::metadata(&l1).unwrap().modified().unwrap()
    ).to_rfc3339();
    assert_eq!(db_size, on_disk_size);
    assert_eq!(db_mtime, on_disk_mtime);
}

/// Minimal FITS file fitsio can open. Empty primary HDU.
fn write_minimal_fits(path: &Path) {
    let mut header = Vec::with_capacity(2880);
    let cards = [
        "SIMPLE  =                    T / file conforms to FITS standard                ",
        "BITPIX  =                    8 / number of bits per data pixel                 ",
        "NAXIS   =                    0 / number of data axes                           ",
        "OBJECT  = 'M33     '           / target name                                   ",
        "TELESCOP= 'T       '                                                          ",
        "INSTRUME= 'C       '                                                          ",
        "IMAGETYP= 'Light   '                                                          ",
        "END                                                                            ",
    ];
    for c in cards { header.extend_from_slice(c.as_bytes()); }
    while header.len() < 2880 { header.push(b' '); }
    std::fs::write(path, header).unwrap();
}
```

- [ ] **Step 2: Run the test and confirm it fails on main**

```bash
cargo test -p athenaeum-core archive::restore::tests::archive_then_restore_then_scan_preserves_session_members -- --nocapture
```

Expected: FAIL on the `assert_eq!(count, 1, ...)` line because the scanner's DELETE+CASCADE wipes session_members. (If `write_minimal_fits` produces a file fitsio can't parse, fall back to a real .fits fixture in `crates/athenaeum-core/tests/fixtures/`.)

- [ ] **Step 3: Commit the failing test**

```bash
git add crates/athenaeum-core/src/archive/restore.rs
git commit -m "test(archive): pin M33 bug — restore + scan must preserve session_members"
```

---

## Task 2: Extend `unmark_file_archived` to accept new mtime + size

The minimal API change that lets restore push the right values into the catalog.

**Files:**
- Modify: `crates/athenaeum-core/src/archive/db.rs:315-337`
- Modify: `crates/athenaeum-core/src/archive/restore.rs` (callers)
- Test: `crates/athenaeum-core/src/archive/db.rs` (existing `mod tests`)

- [ ] **Step 1: Write a failing unit test for the extended signature**

Append to `archive::db::tests`:

```rust
#[test]
fn unmark_file_archived_updates_modified_at_and_size() {
    let (conn, _) = setup();
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format,
                            archived_in_operation, archive_zip_path, archive_path_in_zip)
         VALUES (42, '/orig/path.fits', 'path.fits', 100, '2020-01-01T00:00:00+00:00',
                 'FITS', 7, '/arch/x.zip', 'Lights/path.fits')",
        [],
    ).unwrap();

    unmark_file_archived(
        &conn, 42,
        Some("/new/path.fits"),
        Some("2026-04-29T19:35:00+00:00"),
        Some(150),
    ).unwrap();

    let (path, mtime, size, archived_op): (String, String, i64, Option<i64>) = conn.query_row(
        "SELECT path, modified_at, size, archived_in_operation FROM files WHERE id = 42",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap();
    assert_eq!(path, "/new/path.fits");
    assert_eq!(mtime, "2026-04-29T19:35:00+00:00");
    assert_eq!(size, 150);
    assert!(archived_op.is_none());
}

#[test]
fn unmark_file_archived_with_no_optional_args_only_clears_markers() {
    let (conn, _) = setup();
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format,
                            archived_in_operation, archive_zip_path, archive_path_in_zip)
         VALUES (43, '/p.fits', 'p.fits', 100, '2020-01-01T00:00:00+00:00',
                 'FITS', 7, '/arch/x.zip', 'Lights/p.fits')",
        [],
    ).unwrap();

    unmark_file_archived(&conn, 43, None, None, None).unwrap();

    let (path, mtime, size, archived_op): (String, String, i64, Option<i64>) = conn.query_row(
        "SELECT path, modified_at, size, archived_in_operation FROM files WHERE id = 43",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap();
    assert_eq!(path, "/p.fits"); // unchanged
    assert_eq!(mtime, "2020-01-01T00:00:00+00:00"); // unchanged
    assert_eq!(size, 100); // unchanged
    assert!(archived_op.is_none()); // markers cleared
}
```

- [ ] **Step 2: Run the test and confirm it fails to compile**

```bash
cargo test -p athenaeum-core archive::db::tests::unmark_file_archived_updates -- --nocapture
```

Expected: FAIL — wrong number of args to `unmark_file_archived`.

- [ ] **Step 3: Update the function signature and implementation**

Replace the existing `unmark_file_archived` in `archive/db.rs` with:

```rust
/// Clear archive markers from a file row. Optional inputs let callers also
/// rewrite the path, refresh the on-disk modified_at, and refresh the size.
/// All four columns move atomically in a single UPDATE so a partial commit
/// (e.g., crash mid-write) can never leave the row half-updated.
///
/// Restore uses this to push the freshly-extracted file's metadata into the
/// catalog so that subsequent scanner mtime checks see DB == disk and skip
/// the file as unchanged.
pub fn unmark_file_archived(
    conn: &Connection,
    file_id: i64,
    new_path: Option<&str>,
    new_modified_at: Option<&str>,
    new_size: Option<i64>,
) -> Result<()> {
    // Build the SET clause dynamically. The four archive marker columns are
    // always cleared; path/modified_at/size are conditional.
    let mut sets: Vec<&str> = vec![
        "archived_in_operation = NULL",
        "archive_zip_path = NULL",
        "archive_path_in_zip = NULL",
    ];
    if new_path.is_some() { sets.push("path = ?"); }
    if new_modified_at.is_some() { sets.push("modified_at = ?"); }
    if new_size.is_some() { sets.push("size = ?"); }

    let sql = format!("UPDATE files SET {} WHERE id = ?", sets.join(", "));

    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(p) = new_path { params_vec.push(Box::new(p.to_string())); }
    if let Some(m) = new_modified_at { params_vec.push(Box::new(m.to_string())); }
    if let Some(s) = new_size { params_vec.push(Box::new(s)); }
    params_vec.push(Box::new(file_id));

    let refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, refs.as_slice())?;
    Ok(())
}
```

- [ ] **Step 4: Update all callers**

Search for `unmark_file_archived(` and update every call site. There are exactly four:

```bash
grep -rn "unmark_file_archived(" crates/
```

Expected callers (all in `restore.rs`):
- `restore.rs:308` — currently `unmark_file_archived(conn, file_id, Some(...))` → becomes `unmark_file_archived(conn, file_id, Some(new_path.to_str().unwrap()), None, None)` (the mtime/size will be added in Task 3).
- `restore.rs:310` — currently `unmark_file_archived(conn, file_id, None)` → becomes `unmark_file_archived(conn, file_id, None, None, None)`.
- `restore.rs:345` — currently `unmark_file_archived(conn, fid, None)` → becomes `unmark_file_archived(conn, fid, None, None, None)`.

Use Edit with explicit context for each, since multiple occurrences exist.

- [ ] **Step 5: Run all archive tests**

```bash
cargo test -p athenaeum-core archive:: -- --nocapture
```

Expected: all existing archive tests still pass; the two new `unmark_file_archived_*` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/archive/db.rs crates/athenaeum-core/src/archive/restore.rs
git commit -m "refactor(archive): unmark_file_archived takes optional mtime+size"
```

---

## Task 3: Restore syncs DB modified_at + size after copy

This is the "stop the bleeding" change. Restore now actively pushes the freshly-extracted file's mtime/size into the catalog so the scanner's mtime check sees them as equal.

**Files:**
- Modify: `crates/athenaeum-core/src/archive/restore.rs::run_restore_inner`

- [ ] **Step 1: Stat the destination after every successful copy and pass to unmark_file_archived**

In `run_restore_inner`, after the `std::fs::copy(temp_path, &dest)` call (currently around L274), capture the destination's metadata. Then in the catalog update loop (currently L302-321), pass mtime+size into `unmark_file_archived`. Replace the existing loop:

```rust
    // Stage: update_catalog ----------------------------------------------------
    // For each restored file, sync archive markers + path/mtime/size to match
    // the destination file as it now exists on disk. This is what prevents the
    // scanner's modified-file detection from later wiping these rows.
    let catalog_total = restored.len() + 1;
    let mut catalog_done: usize = 0;
    emit(
        emitter,
        "update_catalog",
        catalog_done,
        catalog_total,
        "Updating catalog".into(),
    );

    for (f, new_path) in &restored {
        if let Some(file_id) = f.file_id {
            let meta = std::fs::metadata(new_path)
                .with_context(|| format!("stat restored file {}", new_path.display()))?;
            let new_size = meta.len() as i64;
            let new_mtime = chrono::DateTime::<chrono::Utc>::from(
                meta.modified()
                    .with_context(|| format!("read modified_at for {}", new_path.display()))?
            ).to_rfc3339();

            // Rewrite path only if it differs from the catalog's source_path;
            // mtime/size are always synced (they're the whole point of this fix).
            let path_changed = new_path.to_str() != Some(f.source_path.as_str());
            let path_arg = if path_changed { new_path.to_str() } else { None };
            adb::unmark_file_archived(conn, file_id, path_arg, Some(&new_mtime), Some(new_size))?;
        }
        catalog_done += 1;
        emit(
            emitter,
            "update_catalog",
            catalog_done,
            catalog_total,
            format!("Updating paths ({}/{})", catalog_done, catalog_total),
        );
    }
```

- [ ] **Step 2: On the skip branch, defensively reconcile DB drift**

Files that were already on disk get skipped (no copy). But the DB may have drifted from disk in a prior cycle. Today the leftover-marker loop at L342-348 only clears archive markers; it doesn't fix mtime/size. Replace that loop with a drift-correcting variant:

```rust
    // For files that weren't restored (skipped because already on disk), still
    // clear any leftover archive markers. Also reconcile modified_at/size with
    // disk if they drifted — this is the recovery path for catalogs that were
    // previously corrupted by an older restore cycle.
    let restored_ids: HashSet<i64> = restored
        .iter()
        .filter_map(|(f, _)| f.file_id)
        .collect();
    for f in files {
        if let Some(fid) = f.file_id {
            if !restored_ids.contains(&fid) {
                // Stat the on-disk file at source_path. If it doesn't exist
                // (file genuinely missing) we still clear markers so the row
                // doesn't keep pointing at an orphaned zip.
                let on_disk = std::fs::metadata(&f.source_path).ok();
                let (mtime_arg, size_arg) = match on_disk {
                    Some(m) => {
                        let mtime = m.modified()
                            .ok()
                            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
                        let size = m.len() as i64;
                        (mtime, Some(size))
                    }
                    None => (None, None),
                };
                adb::unmark_file_archived(
                    conn, fid, None,
                    mtime_arg.as_deref(),
                    size_arg,
                )?;
            }
        }
    }
```

- [ ] **Step 3: Run the Task 1 integration test**

```bash
cargo test -p athenaeum-core archive::restore::tests::archive_then_restore_then_scan_preserves_session_members -- --nocapture
```

Expected: now PASSES on the assertion that `db_mtime == on_disk_mtime` (the catalog is now synced). The session_members assertion may STILL fail because the scanner's destructive branch is still in place — that's Task 4. If the test passes entirely at this point (because no mtime drift = no DELETE), great; if not, that's expected and the next task fixes it.

- [ ] **Step 4: Run the existing archive test suite**

```bash
cargo test -p athenaeum-core archive:: -- --nocapture
```

Expected: all green. The end-to-end `full_archive_then_restore_cycle` and `restore_skips_copy_disposition_files_already_on_disk` tests must still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/archive/restore.rs
git commit -m "fix(archive): restore syncs files.modified_at + size to disk after copy

Closes the data-loss path where archive→restore left the catalog's mtime
behind the freshly-extracted file's mtime. The next monitor scan would then
classify the file as 'modified', DELETE the files row, and CASCADE-wipe
session_members — silently emptying restored frame sets.

Restore now stats every destination file after copy and passes the new
mtime+size to unmark_file_archived so the scanner sees DB == disk and
skips the file as unchanged. Also defensively reconciles drift on the
skip-because-already-on-disk branch.
"
```

---

## Task 4: Non-destructive scanner UPDATE-in-place (serial path)

The scanner's modified-file branch in `scan_directory` (the serial path used by some manual flows) does `DELETE FROM files WHERE path = ?` + re-INSERT. Replace with UPDATE-in-place that preserves `files.id` and `frames.id`.

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:152-189` (the modified-file branch in `scan_directory`)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs::process_file` (extract a helper for in-place re-parse)
- Test: `crates/athenaeum-core/src/scanner/mod.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write a failing test for in-place re-parse preserving session_members**

Add a new module-level test in `scanner/mod.rs`:

```rust
#[cfg(test)]
mod inplace_tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use rusqlite::params;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    /// Re-touching a FITS file (simulating a restore round-trip or a user
    /// edit) must NOT remove the frame from its session. The scanner should
    /// re-parse and UPDATE in place.
    #[test]
    fn rescan_after_mtime_change_preserves_session_members() {
        let scan = TempDir::new().unwrap();
        let f = scan.path().join("M33/L_001.fits");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        crate::archive::restore::tests::write_minimal_fits(&f);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M33')", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();

        // First scan inserts the file/frame and we manually link it into a session.
        let cancel = Arc::new(AtomicBool::new(false));
        let _ = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter, false, cancel.clone(), false,
        );
        let frame_id: i64 = conn.query_row(
            "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
            [f.to_str().unwrap()], |r| r.get(0),
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
            params![frame_id]).unwrap();

        // Touch the file: write the same bytes back, which advances mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100)); // ensure mtime tick
        let bytes = std::fs::read(&f).unwrap();
        std::fs::write(&f, bytes).unwrap();

        // Rescan.
        let _ = scan_directory_parallel(
            scan.path(), 1, &conn, &NullEmitter, false, cancel, false,
        );

        // frame.id must be unchanged AND the session membership must survive.
        let frame_id_after: i64 = conn.query_row(
            "SELECT f.id FROM frames f JOIN files fi ON fi.id = f.file_id WHERE fi.path = ?1",
            [f.to_str().unwrap()], |r| r.get(0),
        ).unwrap();
        assert_eq!(frame_id, frame_id_after,
            "frame.id must be preserved across in-place re-parse");
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
            params![frame_id_after], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1, "session membership must survive the re-parse");
    }
}
```

(Note: this test exercises the parallel path because that's what's actually called from monitoring. The serial path's fix in this task is a precondition for the parallel path's fix in Task 5; both share a helper.)

- [ ] **Step 2: Run the test, confirm it fails**

```bash
cargo test -p athenaeum-core scanner::inplace_tests::rescan_after_mtime_change_preserves_session_members -- --nocapture
```

Expected: FAIL — frame_id changes (DELETE+INSERT path) or session_members count is 0.

- [ ] **Step 3: Add the helper that re-parses + UPDATEs in place**

Below `process_file` in `scanner/mod.rs`, add:

```rust
/// Re-parse a file whose on-disk metadata has drifted from the catalog and
/// UPDATE the existing files / frames / fits_header rows in place. Preserves
/// files.id and frames.id so every junction-table linkage (session_members,
/// calibration_set_frames, calibration_set_to_frames, frame_tags) survives.
///
/// Returns Ok(()) on success. On parse failure, leaves the DB row untouched
/// and returns Err — the caller should log it as a non-fatal error so the
/// rest of the scan continues.
fn reparse_and_update_in_place(
    path: &PathBuf,
    file_id: i64,
    conn: &Connection,
    use_content_hash: bool,
    unique_camera: bool,
    root_id: i64,
) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len() as i64;
    let modified_dt = chrono::DateTime::<Utc>::from(metadata.modified()?);

    let format = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .map(|e| if e == "xisf" { FileFormat::XISF } else { FileFormat::FITS })
        .unwrap_or(FileFormat::FITS);
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();

    // Parse FIRST. If parse fails, we leave the DB alone — better stale than
    // missing.
    let (frame, header_text) = match format {
        FileFormat::FITS => {
            let (f, h) = parse_fits_with_header(path, file_id)?;
            (f, Some(h))
        }
        FileFormat::XISF => {
            let f = parse_xisf(path, file_id)?;
            let h = extract_xisf_header(path).ok();
            (f, h)
        }
    };

    let metadata_hash = compute_metadata_hash(size, &modified_dt, &filename);
    let content_hash = if use_content_hash {
        crate::duplicates::compute_xxhash(path).ok()
    } else {
        None
    };

    let new_instrume = if unique_camera {
        frame.instrume.as_ref().map(|i| {
            // Strip a previous " N<root_id>" suffix before re-applying so we
            // don't accumulate "N1 N1 N1" on repeated rescans.
            let base = if let Some(pos) = i.rfind(" N") {
                let suffix = &i[pos + 2..];
                if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                    &i[..pos]
                } else { i.as_str() }
            } else { i.as_str() };
            format!("{} N{}", base, root_id)
        })
    } else {
        frame.instrume.clone()
    };

    // Defensive: ensure exactly one frames row points at this file_id.
    let frame_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM frames WHERE file_id = ?1",
        params![file_id], |r| r.get(0),
    )?;
    if frame_count != 1 {
        anyhow::bail!(
            "expected exactly 1 frames row for file_id={}, found {}",
            file_id, frame_count,
        );
    }

    // UPDATE files in place.
    conn.execute(
        "UPDATE files
         SET size = ?1, modified_at = ?2, format = ?3,
             metadata_hash = ?4, content_hash = ?5
         WHERE id = ?6",
        params![
            size,
            modified_dt.to_rfc3339(),
            format!("{:?}", format),
            Some(metadata_hash),
            content_hash,
            file_id,
        ],
    )?;

    // UPDATE frames in place. Mirrors the column list in insert_frame so the
    // re-parsed values overwrite the previous ones.
    conn.execute(
        "UPDATE frames SET
            object = ?1, date_obs = ?2, telescop = ?3, instrume = ?4,
            exptime = ?5, filter = ?6, gain = ?7, offset = ?8,
            binning = ?9, xbinning = ?10, ybinning = ?11,
            ccd_temp = ?12, set_temp = ?13, focallen = ?14,
            xpixsz = ?15, ypixsz = ?16, naxis1 = ?17, naxis2 = ?18,
            ra = ?19, dec = ?20, sitelat = ?21, lat_obs = ?22,
            sitelong = ?23, long_obs = ?24, objctra = ?25, objctdec = ?26,
            imagetyp = ?27, is_master = ?28, swcreate = ?29, rotation = ?30
         WHERE file_id = ?31",
        params![
            frame.object, frame.date_obs, frame.telescop, new_instrume,
            frame.exptime, frame.filter, frame.gain, frame.offset,
            frame.binning, frame.xbinning, frame.ybinning,
            frame.ccd_temp, frame.set_temp, frame.focallen,
            frame.xpixsz, frame.ypixsz, frame.naxis1, frame.naxis2,
            frame.ra, frame.dec, frame.sitelat, frame.lat_obs,
            frame.sitelong, frame.long_obs, frame.objctra, frame.objctdec,
            frame.imagetyp.as_ref().map(|t| format!("{:?}", t)),
            frame.is_master as i64,
            frame.swcreate, frame.rotation,
            file_id,
        ],
    )?;

    // fits_header has UNIQUE(file_id) — DELETE then INSERT so the
    // header_fingerprint reflects the new bytes. No FK referencing
    // fits_header rows, so this is safe.
    if let Some(h) = header_text {
        let fingerprint = crate::fingerprint::compute_header_fingerprint(&h);
        conn.execute("DELETE FROM fits_header WHERE file_id = ?1", params![file_id])?;
        conn.execute(
            "INSERT INTO fits_header (file_id, header_text, header_fingerprint)
             VALUES (?1, ?2, ?3)",
            params![file_id, h, fingerprint],
        )?;
    }

    Ok(())
}
```

> Note: the exact column lists in the two UPDATEs must mirror `insert_file` and `insert_frame` in `db/operations.rs`. Open both functions side-by-side and verify each column name and parameter index matches before running the test. If `insert_frame` has more or fewer columns than what's listed here, update this UPDATE to match — every column from `insert_frame` (except `file_id` and `id`) must appear here.

- [ ] **Step 4: Replace the destructive branch in `scan_directory` (serial path)**

In `scan_directory`, find the block at L181-189 that does `DELETE FROM files WHERE path = ?1` and replace with a call to `reparse_and_update_in_place`. Wrap so that on parse failure, the file gets logged as an error and the loop continues (no DELETE):

Replace:
```rust
            // Modified in place — purge the stale row before re-processing.
            if let Err(e) = conn.execute("DELETE FROM files WHERE path = ?1", rusqlite::params![path_str]) {
                errors.lock().unwrap().push(format!(
                    "{}: failed to purge stale row before re-process: {}",
                    file_path.display(),
                    e
                ));
                continue;
            }
        }
```

With:
```rust
            // Modified in place — re-parse and UPDATE catalog row in place.
            // Preserves files.id and frames.id so junction-table linkages
            // (session_members, calibration_set_frames, etc.) survive.
            let file_id: i64 = match conn.query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![path_str],
                |r| r.get(0),
            ) {
                Ok(id) => id,
                Err(e) => {
                    errors.lock().unwrap().push(format!(
                        "{}: lookup file_id failed: {}", file_path.display(), e
                    ));
                    continue;
                }
            };
            match reparse_and_update_in_place(
                file_path, file_id, conn, use_content_hash, unique_camera, root_id,
            ) {
                Ok(()) => {
                    *processed.lock().unwrap() += 1;
                    continue; // skip the new-file process_file path below
                }
                Err(e) => {
                    errors.lock().unwrap().push(format!(
                        "{}: re-parse failed (catalog row left as-is): {}",
                        file_path.display(), e,
                    ));
                    continue;
                }
            }
        }
```

- [ ] **Step 5: Run all scanner tests**

```bash
cargo test -p athenaeum-core scanner:: -- --nocapture
```

Expected: existing tests still pass; the new `rescan_after_mtime_change_preserves_session_members` may still fail because it goes through `scan_directory_parallel` (Task 5) — but the serial path is now fixed.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/scanner/mod.rs
git commit -m "fix(scanner): non-destructive in-place UPDATE on modified file (serial path)

Replaces DELETE FROM files + re-INSERT with re-parse + UPDATE files / UPDATE
frames / DELETE+INSERT fits_header — preserving files.id and frames.id so
junction-table rows (session_members, calibration_set_frames, frame_tags)
survive in-place modifications. Parse failures now leave the row untouched
instead of leaving the catalog with no row at all.
"
```

---

## Task 5: Non-destructive scanner UPDATE-in-place (parallel path)

Same fix in the parallel scan path used by monitoring. Architecture: keep the parallel parse phase, but instead of purging modified rows up front, route them through an UPDATE branch in the sequential write phase.

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs::scan_directory_parallel` (purge phase ~L815-840)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (sequential insert phase ~L1009-1100)

- [ ] **Step 1: Re-run the integration test, confirm still failing on parallel path**

```bash
cargo test -p athenaeum-core scanner::inplace_tests::rescan_after_mtime_change_preserves_session_members -- --nocapture
cargo test -p athenaeum-core archive::restore::tests::archive_then_restore_then_scan_preserves_session_members -- --nocapture
```

Expected: at least one still FAILs because `scan_directory_parallel` still does DELETE+INSERT in the purge phase.

- [ ] **Step 2: Replace the purge phase with classification**

In `scan_directory_parallel`, find the block at L815-840 (`if !modified_paths_to_purge.is_empty() { ... DELETE FROM files WHERE path = ?1 ... }`) and DELETE the entire block.

Then, at the existing classification code (L780-811), change the `Vec<PathBuf>` collection to also remember which paths are *modified* (so the sequential phase knows to UPDATE rather than INSERT). Replace:

```rust
    let mut modified_paths_to_purge: Vec<String> = Vec::new();
    let new_files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            ...
        })
        .collect();
```

With:

```rust
    // Classification: each on-disk file is either NEW (no DB row) or
    // MODIFIED (DB row exists but size/mtime drifted). Unchanged files are
    // dropped here. The sequential write phase below dispatches NEW vs
    // MODIFIED separately (INSERT vs UPDATE-in-place).
    let mut modified_file_ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    {
        // Need the existing files map's id too — augment the query.
        let mut stmt = conn.prepare("SELECT path, id FROM files").ok();
        if let Some(stmt) = stmt.as_mut() {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            }) {
                for r in rows.flatten() { modified_file_ids.insert(r.0, r.1); }
            }
        }
    }
    // (modified_file_ids is keyed on path; lookup is O(1) for the write phase.)

    let mut modified_paths: Vec<String> = Vec::new();
    let new_files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            let path_str = p.to_string_lossy().to_string();
            match existing_files.get(&path_str) {
                None => true,
                Some((db_size, db_modified)) => {
                    match std::fs::metadata(p) {
                        Ok(meta) => {
                            let on_disk_size = meta.len() as i64;
                            let on_disk_modified = meta
                                .modified()
                                .ok()
                                .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                            let unchanged = on_disk_size == *db_size
                                && on_disk_modified.as_deref() == Some(db_modified.as_str());
                            if unchanged {
                                false
                            } else {
                                modified_paths.push(path_str);
                                true
                            }
                        }
                        Err(_) => false,
                    }
                }
            }
        })
        .collect();

    crate::logging::log(
        "INFO",
        &format!(
            "Files to process: {} ({} unchanged, {} modified — will UPDATE in place)",
            new_files.len(),
            result.files_skipped,
            modified_paths.len(),
        ),
    );
```

(Drop the old `result.files_skipped = ...` and `crate::logging::log("INFO", ...)` lines that are now superseded.)

`result.files_skipped = result.files_found - new_files.len();` from L842 stays — keep it.

- [ ] **Step 3: Branch the sequential write phase on classification**

In the sequential phase that processes `processed_results` (around L984-1100), look at every `file_result` and check whether its path is in `modified_paths`. If yes, call `reparse_and_update_in_place` (which we already defined in Task 4); if no, fall through to the existing INSERT path.

The simplest mechanical change: convert `modified_paths` to a `HashSet<String>` and gate the INSERT branch on it. Find the line `// Insert file\nmatch insert_file(conn, &file_result.file) {` and replace the lead-up:

```rust
        // Check for moved files (same fingerprint at different path)
        if let Some(ref header) = file_result.header {
            ... existing moved-file detection unchanged ...
        }

        // If this file is a modified existing row (not a new file), use the
        // non-destructive in-place UPDATE path. Preserves files.id +
        // frames.id and therefore every junction-table linkage.
        if modified_paths_set.contains(&file_result.file.path) {
            let file_id_opt: Option<i64> = conn.query_row(
                "SELECT id FROM files WHERE path = ?1",
                rusqlite::params![&file_result.file.path],
                |r| r.get(0),
            ).ok();
            if let Some(file_id) = file_id_opt {
                let path_buf = PathBuf::from(&file_result.file.path);
                match reparse_and_update_in_place(
                    &path_buf, file_id, conn, use_content_hash, unique_camera, root_id,
                ) {
                    Ok(()) => {
                        result.files_processed += 1;
                        // Track image type for calibration set creation. Use the
                        // already-parsed frame data from process_file_parallel.
                        if let Some(ref imagetyp) = file_result.imagetyp {
                            match imagetyp {
                                ImageType::Light => lights_count += 1,
                                ImageType::Flat => { /* don't add to flat_frame_ids — already in set */ }
                                ImageType::Dark => { /* same */ }
                                ImageType::Bias => { /* same */ }
                                ImageType::DarkFlat => { /* same */ }
                                _ => {}
                            }
                        }
                        continue;
                    }
                    Err(e) => {
                        result.errors.push(format!(
                            "{}: in-place re-parse failed: {}",
                            file_result.file.path, e,
                        ));
                        continue;
                    }
                }
            }
        }

        // New file path — existing INSERT behavior.
        match insert_file(conn, &file_result.file) {
            ... unchanged ...
```

Just before the loop, build the lookup set (where `modified_paths` was populated above):

```rust
    let modified_paths_set: std::collections::HashSet<String> =
        modified_paths.iter().cloned().collect();
```

Note on calibration sets for re-parsed frames: they're already in their calibration_set_frames row from the previous scan (because we preserved frames.id), so we deliberately do NOT push them onto `flat_frame_ids` etc. — those vectors feed `create_calibration_sets_from_scan_with_masters` which would create a duplicate set otherwise.

- [ ] **Step 4: Run the integration tests**

```bash
cargo test -p athenaeum-core scanner::inplace_tests::rescan_after_mtime_change_preserves_session_members -- --nocapture
cargo test -p athenaeum-core archive::restore::tests::archive_then_restore_then_scan_preserves_session_members -- --nocapture
```

Expected: BOTH PASS.

- [ ] **Step 5: Run the entire scanner + archive test suites**

```bash
cargo test -p athenaeum-core scanner:: -- --nocapture
cargo test -p athenaeum-core archive:: -- --nocapture
```

Expected: all green.

- [ ] **Step 6: Run the full workspace test suite**

```bash
cargo test --workspace -- --nocapture
```

Expected: all green. Watch for any tests in `auto_merge`, `clustering`, `calibration` that might have implicitly depended on DELETE+INSERT changing `frames.id`. There shouldn't be any (calibration_set_frames keys on frames.id, so preserving the id is strictly better), but flag any failures.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/scanner/mod.rs
git commit -m "fix(scanner): non-destructive in-place UPDATE on modified file (parallel path)

Mirrors the serial-path fix in the parallel scan used by monitoring. Every
sub-path that previously did DELETE FROM files + re-INSERT now re-parses
and UPDATEs in place, preserving files.id and frames.id so session_members,
calibration_set_frames, calibration_set_to_frames, and frame_tags all
survive any in-place modification (archive→restore round-trips, user FITS
edits, network FS clock drift).
"
```

---

## Task 6: Manual end-to-end verification on real data

Smoke test against the user's real DB before merging.

**Files:** None (operational verification).

- [ ] **Step 1: Build the desktop app from this branch**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-restore-mtime
npm run tauri build
```

- [ ] **Step 2: Backup the live DB**

```bash
cp "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db" \
   "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db.bak-$(date +%Y%m%d-%H%M%S)"
```

- [ ] **Step 3: Pick a small custom frame set with ≥10 frames as the test subject**

Don't use M33 — the user already restored it manually. Pick something less precious (e.g., a test object). Confirm it has populated session_members:

```bash
sqlite3 "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db" \
  "SELECT fs.id, fs.name, COUNT(sm.frame_id)
   FROM frames_set fs
   LEFT JOIN imaging_nights n ON n.frames_set_id = fs.id
   LEFT JOIN sessions s ON s.imaging_night_id = n.id
   LEFT JOIN session_members sm ON sm.session_id = s.id
   WHERE fs.is_custom = 1 AND fs.is_archived = 0
   GROUP BY fs.id ORDER BY 3 DESC LIMIT 5;"
```

- [ ] **Step 4: Archive (move) the chosen frame set, then restore it, with monitoring ON**

Through the UI:
1. Move set into Archive section (legacy soft archive — sets is_archived=1).
2. Move and ZIP (the new feature).
3. Confirm the on-disk files are gone, zip exists in the archive folder.
4. Verify monitoring is ON for `Pictures/Astro` (Settings → File Manager → Monitored Directories).
5. Restore via the new restore feature.
6. Wait at least one full monitor interval (default 10 min) AND trigger a manual full scan of `Pictures/Astro` for belt-and-suspenders.
7. Reopen the frame set.

- [ ] **Step 5: Verify session_members survived**

```bash
sqlite3 "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db" \
  "SELECT fs.id, fs.name, COUNT(sm.frame_id)
   FROM frames_set fs
   LEFT JOIN imaging_nights n ON n.frames_set_id = fs.id
   LEFT JOIN sessions s ON s.imaging_night_id = n.id
   LEFT JOIN session_members sm ON sm.session_id = s.id
   WHERE fs.id = <chosen_id>
   GROUP BY fs.id;"
```

Expected: same frame count as before archive.

Also verify the catalog mtime/size match the on-disk file for one of the restored files:

```bash
sqlite3 "..." "SELECT path, size, modified_at FROM files WHERE id = <some_file_id>;"
stat -f "%z %m" /path/to/that/file.fits
```

Convert the FS mtime epoch with `date -r <mtime> -u +%FT%T+00:00`. They must match.

- [ ] **Step 6: If verification fails**

STOP. Do not merge. Open a fresh investigation: capture `sqlite3 "..." ".schema files"`, the relevant `archive_operations` row, and the scanner debug log; report findings before continuing.

If verification passes, proceed.

---

## Task 7: Documentation + changelog

**Files:**
- Modify: `CLAUDE.md` (add a note under "Scanner Behavior")
- Modify: `crates/athenaeum-tauri/CHANGELOG.md` if it exists, otherwise note in commit message only.

- [ ] **Step 1: Add a one-line scanner behavior note to CLAUDE.md**

Open `CLAUDE.md` and find the section on "Scanner Behavior" (currently exists in the user's auto-memory but check the project file first). Add:

```markdown
- **In-place modifications use UPDATE, not DELETE+INSERT.** When a scanned file's mtime/size has drifted from the catalog, the scanner re-parses and UPDATEs the existing rows so `files.id` and `frames.id` are preserved. This keeps junction tables (`session_members`, `calibration_set_frames`, `calibration_set_to_frames`, `frame_tags`) intact across legitimate modifications and archive→restore round-trips.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(scanner): document non-destructive UPDATE-in-place re-parse behavior"
```

- [ ] **Step 3: Push the branch**

```bash
git push -u origin fix/restore-mtime-and-scanner-update
```

---

## Out of scope (explicit non-goals)

The following are deliberately not part of this plan. Each is a known concern that can be tackled separately if needed:

1. **Preserving the original on-disk mtime byte-for-byte across archive→restore.** The catalog now matches disk after restore, which is what matters for the scanner. Whether the on-disk mtime is "the original mtime" or "the time of restore" is cosmetic. (Future work: store `original_modified_at` in `archive_operation_files` and apply via `filetime` crate. Schema migration; Cargo dep; not load-bearing.)
2. **Coordinating concurrent scan + restore.** Today they can race; with Layer D the worst-case outcome is "scan parses a half-extracted file and logs an error", not data loss. Could be tightened by having `MonitorOrchestrator` skip roots that have an in-flight archive/restore.
3. **IMAGETYP changes mid-life** (user re-tags a frame). The UPDATE writes the new IMAGETYP but doesn't reconcile session_members or calibration_set_frames that may no longer make sense. Rare in practice; out of scope for the data-loss bug.
4. **Auto-cleaning genuinely missing files.** Still explicitly forbidden by project rule (CLAUDE.md "no auto-cleanup of missing files"). The fix in this plan does not change that policy and does not add a missing-file deletion path.

---

## Self-review

- **Spec coverage:** every edge case in the "Edge cases this design has to handle" section maps to a specific task. (1)→Task 3; (2)→Task 3; (3)→Task 3; (4)→Task 3 (uses `with_context` so failure surfaces); (5)→Task 3 (always uses destination metadata); (6)→Out-of-scope #2; (7)→Tasks 4+5; (8)→Tasks 4+5 (parse before UPDATE); (9)→Out-of-scope #3; (10)→Tasks 4+5 surface error; (11)→Tasks 4+5 use UNIQUE-safe DELETE+INSERT for fits_header; (12)→Tasks 4+5 add `frame_count != 1` guard; (13)→Task 4 + Task 5 fix both paths; (14)→retains existing fast-path skip; (15)→preserved by id-stable UPDATE; (16)→Task 3 keeps the leftover-marker loop and adds drift correction; (17)→Task 2 keeps backward-compat by making new args optional.
- **Placeholder scan:** every code block contains real, runnable code; no "TODO", "TBD", "similar to above". The `write_minimal_fits` helper is fully spelled out in Task 1.
- **Type consistency:** `unmark_file_archived(conn, id, Option<&str>, Option<&str>, Option<i64>)` is consistent across Tasks 2 and 3. `reparse_and_update_in_place(path: &PathBuf, file_id: i64, conn, use_content_hash, unique_camera, root_id)` is identical between Task 4 (definition) and Task 5 (call site). The `modified_paths_set` HashSet introduced in Task 5 is built once before the write loop.
